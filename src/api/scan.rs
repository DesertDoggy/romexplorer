//! `FileScan` / `HashMatch` execution -- the first real write path from `rom::scan` into the
//! `local_*` tables, deliberately minimal (docs/plan/api.md, "Order of work" step 2):
//!
//! - whole-file hashes (crc32/md5/sha1/sha256), and for an archive (CHD, RVZ / WIA / GCZ,
//!   zip / 7z) every member too -- stream-decoded and hashed, nothing written to disk --
//!   recorded as `content` rows linked in `content_contents`, so a CHD whose own hash no
//!   mame-redump DAT lists still matches Redump / TOSEC by its tracks;
//! - platform from the directory's forced/effective setting, the candidate's own hint, or an
//!   unambiguous extension -- never a deep detect cycle;
//! - one `local_rom` per candidate (a sheet with its tracks, a directory bundle with its
//!   files), one `local_game` per rom; `.m3u` groups aren't merged into one game yet;
//! - `HashMatch` links a game to the release whose `release_contents` cover most of its
//!   files (`to_dat`), then identifies what is left (`to_hasheous`, `to_playmatch`) in
//!   passes over the whole set, database before network: the Hasheous answers already
//!   recorded and the hash index of its downloaded dumps; then Hasheous itself for the
//!   files those cannot answer, a few requests in flight at once; then PlayMatch, a hundred
//!   files to a request, for what is still unidentified. Finally one batched
//!   `provider_query` per cross-referenced provider -- see `run_hash_match`.
//!
//! Hashing runs on the blocking pool one file at a time; rows are written as each input
//! finishes, so a crash mid-scan leaves a consistent, resumable state (`new_only` picks up
//! where it stopped).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use serde::Serialize;
use sqlx::mysql::MySqlPool;
use sqlx::Row;

use crate::common::models::Platform;
use crate::database::directories::{self as db_dirs, Directory};
use crate::database::import::sha1_hex;
use crate::database::jobs::FileScanMode;
use crate::database::providers::raw_store::RawStore;
use crate::database::providers::{cache, Provider};
use crate::database::settings;
use crate::rom::content::InputKind;
use crate::rom::detect::systemfile::{DirType, OtherFileKind, SystemFileType};
use crate::rom::detect::{self, HeuristicFlags};
use crate::rom::hash::{self, HashFlags, HashOptions, HashReport};
use crate::rom::multiple::{Job, Multiple};
use crate::rom::operation::{Detect, Single, SingleArgs, SingleError, SingleOutcome, StageSwitches};
use crate::rom::scan::{self as rom_scan, PlatformContext, PlatformLayout, RomCandidate, ScanContext, WalkResult};

use super::jobs::{FileScanRequest, HashMatchRequest, PlatformSelect};
use super::{App, Error, Result};

#[derive(Debug, Default, Clone, Serialize)]
pub struct ScanSummary {
    pub mode: String,
    pub directory: String,
    pub candidates: u64,
    pub skipped_existing: u64,
    pub files_hashed: u64,
    /// Archive members (CHD tracks, zip / 7z / RVZ contents) hashed from the stream.
    pub members_hashed: u64,
    pub content_new: u64,
    pub content_reused: u64,
    pub roms_new: u64,
    pub games_new: u64,
    pub playlists_seen: u64,
    /// Playlists (.m3u) whose discs were put into one game.
    pub playlists_grouped: u64,
    pub marked_missing: u64,
    /// Files the settings asked for an `ra_hash` on, where no platform was forced on the
    /// directory and none was detected -- so no RetroAchievements console id could be
    /// resolved. Reported rather than guessed: the console id selects the algorithm, and a
    /// stand-in would return the plain file MD5 for most consoles (already stored under
    /// `md5`) and the wrong value for NES, SNES and disc systems.
    pub ra_hash_skipped_unknown_platform: u64,
    /// RetroAchievements hashes computed for roms whose console hashes something other
    /// than the whole file (NES, SNES, N64, disc systems, ...).
    pub ra_hashes: u64,
    /// Roms whose RetroAchievements hash could not be worked out (a format RAHasher does
    /// not read, a damaged image).
    pub ra_hash_failed: u64,
    /// The RAHasher library is not installed, so no RetroAchievements hash was computed.
    pub ra_hasher_missing: bool,
    /// The `chd_reader` checked mode: CHDs whose built-in reading was compared with chdman,
    /// and ones read while chdman was busy with a conversion (not compared).
    pub chd_compared: u64,
    pub chd_not_compared: u64,
    /// Where the built-in CHD reader and chdman disagreed, one line per CHD: worth reporting.
    pub chd_reader_differences: Vec<String>,
    /// Candidates whose platform no signature check confirmed: recorded as Unknown until a
    /// DAT hash match identifies them (the hint -- a folder's platform, an extension -- is
    /// never recorded on its own).
    pub platform_unconfirmed: u64,
    /// Files recorded before whose bytes have changed since (same path, another hash): the
    /// file now points at the new content, its integrity stamp is cleared, its rom takes the
    /// new platform, and its game's release links are checked again.
    pub contents_replaced: u64,
    /// Release links dropped because no file of the game backs them any more; the next hash
    /// match links the new bytes.
    pub release_links_dropped: u64,
    pub errors: Vec<String>,
    /// Files that were read but are not whole, and not worth failing the scan over: a cue /
    /// gdi sheet none of whose tracks are beside it (they were converted to CHD and kept
    /// elsewhere). The sheet is still recorded, on its own.
    pub warnings: Vec<String>,
    /// Roms with a replaced file, for the release-link check at the end of the scan.
    #[serde(skip)]
    replaced_roms: Vec<u64>,
    /// A rescan of one file (`FileScanRequest::file_id`): what it read and what it linked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_rescan: Option<FileRescan>,
}

/// What "Rescan this file" did beyond the scan's counts.
#[derive(Debug, Default, Clone, Serialize)]
pub struct FileRescan {
    pub file: String,
    /// What was read again: the rom the file belongs to (its sheet, its folder, its archive),
    /// or the file alone when it belongs to none.
    pub read: Vec<String>,
    /// The games those roms are in.
    pub games: u64,
    /// Games this run linked: "game #id → release (title)".
    pub linked: Vec<String>,
    /// Games that were linked already, the same way.
    pub already_linked: Vec<String>,
    /// Games no DAT line or serial matched.
    pub unmatched: u64,
    /// The DAT releases listing these files' bytes or their members': with a game still
    /// unlinked, which release it should have been.
    pub listed_by: Vec<String>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct MatchSummary {
    /// DAT resolve (before matching): roms folded into one because they hold different files
    /// of one DAT release, and games folded into one because their roms are the discs of one
    /// multi-disc release set.
    pub roms_merged: u64,
    pub games_merged: u64,
    /// Releases linked to a RetroAchievements game by a rom's own RA hash.
    pub ra_rom_links: u64,
    pub games: u64,
    pub matched: u64,
    pub already_linked: u64,
    pub unmatched: u64,
    /// What the games no DAT release matched are, most first: "platform · .extension" of
    /// each game's first rom, and how many games. Says at a glance whether they are files no
    /// DAT lists (digital updates, homebrew) or dumps that should have matched.
    pub unmatched_by_kind: Vec<(String, u64)>,
    /// Games linked by the serial in their header (or manifest, or name) because no DAT
    /// hashes their format (`serial_match` setting), and games whose serial fits several
    /// releases nothing told apart.
    pub serial_matched: u64,
    pub serial_ambiguous: u64,
    pub hasheous_skipped: bool,
    pub hasheous_lookups: u64,
    pub hasheous_cached: u64,
    pub hasheous_hits: u64,
    /// Games linked to a release built from a Hasheous hit (no DAT covered the file).
    pub hasheous_matched: u64,
    /// DAT-linked games sent to Hasheous anyway because they had no provider ids yet.
    pub hasheous_for_linked: u64,
    /// Title/provider pairs handed to by-name follow-up jobs (no hash source gave an id).
    pub name_followup_titles: u64,
    pub hasheous_errors: u64,
    pub playmatch_skipped: bool,
    pub playmatch_lookups: u64,
    pub playmatch_cached: u64,
    pub playmatch_hits: u64,
    pub playmatch_errors: u64,
    /// Contents already identified locally (exact hash + a known IGDB id): no online
    /// PlayMatch request.
    pub playmatch_local_skips: u64,
    /// Links filled from local exact-hash sources (Hasheous mapping, RA hashes, SS index).
    pub local_links_filled: u64,
    /// One `provider_query` job per provider with cross-refs, carrying every id at once.
    pub followups_enqueued: u64,
    pub followup_ids: u64,
}

struct HashedFile {
    size: u64,
    modified: Option<chrono::NaiveDateTime>,
    report: HashReport,
}

/// The algorithms this scan computes, from `settings::scan_hashes`.
///
/// `console_id` stays `None` here on purpose. `hash::hash` rejects the whole call when
/// `RAHASH` is set without one ([`crate::rom::hash`]), so requesting an RA hash for a file
/// whose platform is unknown would fail the scan outright rather than skip one value. Until
/// a platform can be resolved to a RetroAchievements console id, `ra_hash` is left blank and
/// counted, not guessed: a stand-in id would return that console's algorithm, which for most
/// consoles is the plain file MD5 (already stored) and for NES/SNES/disc systems is simply
/// the wrong answer.
fn hash_options(flags: HashFlags) -> HashOptions {
    HashOptions { flags, buf_size: None, console_id: None }
}

async fn hash_file(path: PathBuf, flags: HashFlags, step: &Step<'_>) -> Result<HashedFile> {
    let joined = step
        .run("hashing", flags, move |on_progress| -> std::io::Result<HashedFile> {
            let meta = std::fs::metadata(&path)?;
            let modified = meta.modified().ok().map(|t| chrono::DateTime::<chrono::Utc>::from(t).naive_utc());
            let report = hash::hash(&path, &hash_options(flags), on_progress)?;
            Ok(HashedFile { size: meta.len(), modified, report })
        })
        .await
        .map_err(|e| Error::internal(format!("hash task: {e}")))?;
    joined.map_err(|e| Error::internal(format!("hash: {e}")))
}

/// The algorithms a run computes, as a `+`-joined list.
///
/// Deliberately a *set*, not a sequence. Every byte-stream algorithm gets its own worker
/// thread fed from one read of the file ([`hash::HashDigest`]), and RAHASH runs beside them
/// on a thread of its own -- so there is no moment at which the scan is "doing CRC32" and
/// then later "doing SHA1". Reporting them as steps would be an invented narrative; what
/// actually advances is one shared percentage over the bytes.
fn algorithms(flags: HashFlags) -> String {
    let flags = if flags.is_empty() { HashFlags::default() } else { flags };
    let mut names = Vec::new();
    for (bit, name) in [
        (HashFlags::CRC32, "crc32"),
        (HashFlags::XXH3_128, "xxh3"),
        (HashFlags::MD5, "md5"),
        (HashFlags::SHA1, "sha1"),
        (HashFlags::SHA256, "sha256"),
        (HashFlags::RAHASH, "rahash"),
    ] {
        if flags.contains(bit) {
            names.push(name);
        }
    }
    names.join("+")
}

/// One file's slot in a scan, and what the job says while that file is being worked on.
///
/// The hashing itself happens on the blocking pool, so it cannot write progress; both
/// [`hash::hash`] and [`FileData::hash`](crate::common::models::FileData::hash) already
/// publish a 0..=100 fraction through a callback, and both were being handed `|_| {}`. The
/// effect was that a scan went silent for the whole of a large file -- a 40 GB ISO sat on
/// "file 3 of 40" for minutes with no sign it was doing anything. `Step` polls what the
/// blocking thread publishes and turns it into a live message.
struct Step<'a> {
    progress: &'a crate::database::jobs::Progress,
    /// Bytes finished before this file, and the scan's total, so the job's own bar keeps
    /// advancing smoothly *through* a file rather than jumping at each file boundary.
    done_bytes: u64,
    total_bytes: u64,
    file_bytes: u64,
    /// "file 3 of 40: Game.iso"
    label: String,
}

impl Step<'_> {
    const TICK: std::time::Duration = std::time::Duration::from_millis(400);

    /// Runs `job` on the blocking pool, reporting `<label> -- <stage> (<algorithms>) NN%`
    /// while it goes.
    async fn run<T, F>(&self, stage: &str, flags: HashFlags, job: F) -> std::result::Result<T, tokio::task::JoinError>
    where
        T: Send + 'static,
        F: FnOnce(&mut dyn FnMut(f32)) -> T + Send + 'static,
    {
        let bits = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0f32.to_bits()));
        let published = std::sync::Arc::clone(&bits);
        let mut handle = tokio::task::spawn_blocking(move || {
            let mut on_progress = move |pct: f32| published.store(pct.to_bits(), std::sync::atomic::Ordering::Relaxed);
            job(&mut on_progress)
        });
        let prefix = format!("{} -- {stage} ({})", self.label, algorithms(flags));
        loop {
            tokio::select! {
                joined = &mut handle => return joined,
                _ = tokio::time::sleep(Self::TICK) => {
                    let pct = f32::from_bits(bits.load(std::sync::atomic::Ordering::Relaxed)).clamp(0.0, 100.0);
                    // Count the fraction of *this* file into the scan-wide byte total, so
                    // the bar moves continuously instead of once per file.
                    let within = (self.file_bytes as f64 * (pct as f64 / 100.0)) as u64;
                    // The fraction measures how much of the file has been *read*, not how
                    // much has been digested: the reader feeds chunks to one worker thread
                    // per algorithm, so with several slow algorithms (sha256 especially) on
                    // a file the OS already has cached, reading finishes long before the
                    // workers drain their queues. Sitting on "100%" for the minute that
                    // takes reads as a hang, so say what is actually happening instead.
                    let note = if pct >= 100.0 { "finishing".to_string() } else { format!("{pct:.0}%") };
                    self.progress.report(self.done_bytes + within, Some(self.total_bytes), &format!("{prefix} {note}")).await;
                }
            }
        }
    }
}

/// What a candidate's platform is *expected* to be: the walk's hint for it (its folder's
/// platform, configured or by name), else the scanned directory's own, else an extension
/// only one platform uses. Only a hint -- [`detect_for`] has the file's signature confirm it.
fn hint_for(candidate: &RomCandidate, forced: Option<Platform>) -> Option<Platform> {
    if let Some(p) = candidate.platform_hint.or(forced) {
        return Some(p);
    }
    let ext = candidate.path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match detect::infer_platforms_from_extension_heuristics(&ext).as_slice() {
        [only] => Some(*only),
        _ => None,
    }
}

/// How a candidate's platform is decided: its signature has to confirm the hint (or another
/// platform its extension allows), else it is Unknown until a DAT hash match says what it is.
/// A folder bundle's hint already came from a positive signature check of the folder.
fn detect_for(candidate: &RomCandidate, hint: Option<Platform>) -> Detect {
    match candidate.kind {
        rom_scan::CandidateKind::DirectoryBundle => Detect::Trust(hint.unwrap_or(Platform::Unknown)),
        _ => Detect::Verify(hint),
    }
}

/// A scan's arguments for one candidate's [`Single`]: hash only; the platform decided by
/// `detect` (a signature-confirmed hint, see [`detect_for`]); the container's own bytes hashed
/// beside the decode of its members; disc members named and sheets rebuilt in the
/// installation's `sheet_style`. `whole_file`: the path alone, a sheet not expanded.
fn scan_args(detect: Detect, flags: HashFlags, style: crate::common::models::CueSheetStyle, whole_file: bool) -> SingleArgs {
    let flags = if flags.is_empty() { HashFlags::default() } else { flags.difference(HashFlags::RAHASH) };
    SingleArgs {
        stages: StageSwitches { hash: true, ..StageSwitches::NONE },
        hash_flags: flags,
        detect,
        container_hash: Some(flags),
        whole_file,
        ..SingleArgs::default()
    }
    .with_sheet_style(style)
}

/// One on-disk file as a scan records it: its own hashes, and its members' when it is a
/// container that was decoded.
struct ScannedFile {
    path: PathBuf,
    hashed: HashedFile,
    members: Vec<crate::rom::hash::FileHashRecord>,
}

/// The on-disk files of one [`Single`] run, in the order it streamed them.
///
/// Content files that *are* files on disk (a plain file, a sheet and its tracks, a
/// bundle's parts) are their own records. Otherwise the input is a container: its own
/// bytes are `input_container`, and the content files are its members.
fn scanned_files(outcome: &SingleOutcome) -> std::result::Result<Vec<ScannedFile>, String> {
    if let crate::rom::operation::StageResult::Err(e) = &outcome.stages.hash {
        return Err(e.clone());
    }
    // A container that could not be opened skips the hash after it: its error is the one
    // to read, not "not hashed".
    if !outcome.stages.hash.is_ok() {
        if let crate::rom::operation::StageResult::Err(e) = &outcome.stages.extract {
            return Err(e.clone());
        }
    }
    let records = outcome.input_hashes.as_ref().map(|r| r.content_hashes.as_slice()).unwrap_or_default();
    let stat = |path: &std::path::Path| -> std::result::Result<(u64, Option<chrono::NaiveDateTime>), String> {
        let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok((meta.len(), meta.modified().ok().map(|t| chrono::DateTime::<chrono::Utc>::from(t).naive_utc())))
    };
    if outcome.content_files.iter().all(|f| f.source_path.is_some()) {
        return outcome
            .content_files
            .iter()
            .enumerate()
            .map(|(i, file)| {
                let path = file.source_path.clone().expect("checked above");
                let record = records.get(i).ok_or_else(|| format!("{}: not hashed", path.display()))?;
                let (size, modified) = stat(&path)?;
                Ok(ScannedFile { path, hashed: HashedFile { size, modified, report: crate::rom::operation::report_from_record(record) }, members: Vec::new() })
            })
            .collect();
    }
    let path = outcome.input.path.clone();
    let report = match &outcome.input_container {
        Some(Ok(report)) => report.clone(),
        Some(Err(e)) => return Err(format!("{}: {e}", path.display())),
        None => return Err(format!("{}: container not hashed", path.display())),
    };
    let (size, modified) = stat(&path)?;
    Ok(vec![ScannedFile { path, hashed: HashedFile { size, modified, report }, members: records.to_vec() }])
}

/// A cue / gdi sheet none of whose track files are beside it: the tracks were kept somewhere
/// else (as a CHD, say), so the sheet is a warning, not a broken set. A sheet with only some
/// of its tracks missing is an error.
fn lone_sheet(path: &Path) -> bool {
    let Ok(sheet) = crate::rom::detect::sheet::Sheet::parse(path) else { return false };
    let tracks = sheet.payload_paths(path.parent().unwrap_or_else(|| Path::new(".")));
    !tracks.is_empty() && tracks.iter().all(|t| !t.is_file())
}

fn warnings_note(summary: &ScanSummary) -> String {
    if summary.warnings.is_empty() {
        String::new()
    } else {
        format!(", {} warning(s)", summary.warnings.len())
    }
}

/// How many errors and warnings a scan's job result lists. A scan the open-files limit broke
/// reported 54,540 errors; listing them all made a 10 MB result every jobs list carries. The
/// counts stay whole, and the server log has the full summary.
const RESULT_LIST_MAX: usize = 2000;

/// The summary as the job's result (`JobRow::result`), its error and warning lists cut to
/// [`RESULT_LIST_MAX`] with how many more there were.
fn job_result(summary: &ScanSummary) -> serde_json::Value {
    let mut value = serde_json::to_value(summary).unwrap_or(serde_json::Value::Null);
    if let Some(map) = value.as_object_mut() {
        for key in ["errors", "warnings"] {
            if let Some(serde_json::Value::Array(list)) = map.get_mut(key) {
                if list.len() > RESULT_LIST_MAX {
                    let more = list.len() - RESULT_LIST_MAX;
                    list.truncate(RESULT_LIST_MAX);
                    map.insert(format!("{key}_not_listed"), serde_json::json!(more));
                }
            }
        }
    }
    value
}

/// Runs one candidate's [`Single`] on the blocking pool -- for the odd file a scan handles
/// outside its [`Multiple`] run.
async fn scan_one(path: PathBuf, args: SingleArgs) -> Result<SingleOutcome> {
    tokio::task::spawn_blocking(move || Single::new(path, args).run(&mut |_| true))
        .await
        .map_err(|e| Error::internal(format!("scan task: {e}")))?
        .map_err(|e| Error::internal(e.to_string()))
}

/// `local_files.system_type` for what `rom::detect::systemfile` identified a file as.
fn system_type_name(kind: SystemFileType) -> &'static str {
    match kind {
        SystemFileType::Bios | SystemFileType::Unknown => "bios",
        SystemFileType::Firmware => "firmware",
        SystemFileType::Nand => "nand",
        SystemFileType::Sdcard => "sdcard",
        SystemFileType::Other(OtherFileKind::Font) => "font",
        SystemFileType::Other(OtherFileKind::Kanji) => "kanji",
        SystemFileType::Other(OtherFileKind::Dsp) => "dsp",
        SystemFileType::Other(OtherFileKind::Unspecified) => "other",
    }
}

/// The first-level folder of `path` below the scanned directory `root`: what a scan finishes
/// before starting the next (`Multiple::run_in_turn`). Empty for what lies directly in
/// `root`, which sorts first.
fn first_folder(root: &Path, path: &Path) -> String {
    let Ok(rel) = path.strip_prefix(root) else { return String::new() };
    let mut parts = rel.components();
    match (parts.next(), parts.next()) {
        (Some(first), Some(_)) => first.as_os_str().to_string_lossy().into_owned(),
        _ => String::new(),
    }
}

/// Cancels a [`Multiple`] when dropped: a scan's task aborted, or returning early, must not
/// leave its blocking reads going with nobody to collect them.
struct CancelOnDrop(Multiple);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Writes a running scan's bar and the files it is reading right now into the job, every
/// 400 ms, from a task of its own, until dropped.
///
/// A task of its own because the scan loop cannot do it: the loop records each result in
/// the database before it takes the next, and while it does it polls nothing. When results
/// come faster than that (a folder of small disk images), the loop never waited at all, so a
/// timer in it never fired, and the files under the bar stayed whatever a rare quiet moment
/// last saw -- "detecting, 0 B" for files long since finished.
struct RunningReport {
    status: std::sync::Arc<std::sync::Mutex<(String, String)>>,
    stop: std::sync::Arc<tokio::sync::Notify>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl RunningReport {
    const TICK: std::time::Duration = std::time::Duration::from_millis(400);

    fn start(multiple: &Multiple, progress: &crate::database::jobs::Progress, status: String) -> Self {
        let status = std::sync::Arc::new(std::sync::Mutex::new((status, String::new())));
        let stop = std::sync::Arc::new(tokio::sync::Notify::new());
        let (multiple, progress, shared, stopped) = (multiple.clone(), progress.duplicate(), std::sync::Arc::clone(&status), std::sync::Arc::clone(&stop));
        let task = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Self::TICK);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                // Stops only between writes, so `finish` never leaves one in flight.
                tokio::select! {
                    _ = tick.tick() => {}
                    _ = stopped.notified() => return,
                }
                // A cancel stops the files being read and the ones not started yet; the
                // results still come in, as cancelled, and the scan stops at the next one.
                if !multiple.is_cancelled() && progress.cancelled().await {
                    multiple.cancel();
                }
                let (done, total, running) = multiple.progress();
                // The files being read right now, each with its own progress: the Scan tab
                // lists them under the job's bar.
                let detail = serde_json::json!({ "files": multiple.running_files(), "running": running, "limits": multiple.limits() });
                // Where a folder-by-folder run is (`Multiple::run_in_turn`).
                let place = multiple.current_group().map(|(folder, tier)| format!(" -- {}, {} files", if folder.is_empty() { "top folder".to_string() } else { folder }, tier.name())).unwrap_or_default();
                // The file name last: the row holds 255 characters, and a long name should
                // be what gets cut, not the counts.
                let (count, file) = shared.lock().unwrap_or_else(|p| p.into_inner()).clone();
                progress.report_detail(done, Some(total), &format!("{count} ({} of {}, {running} at once){place}{file}", human_bytes(done), human_bytes(total)), &detail).await;
            }
        });
        Self { status, stop, task: Some(task) }
    }

    /// Stops the writes and waits out the one under way, if any, so the caller's final
    /// message is the last word and not overwritten by a late tick.
    async fn finish(mut self) {
        self.stop.notify_one();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }

    /// What the scan is at, written with the next tick: "120 of 4000 done" and the file it
    /// is on (", last: <file>").
    fn set(&self, count: String, file: String) {
        *self.status.lock().unwrap_or_else(|p| p.into_inner()) = (count, file);
    }
}

impl Drop for RunningReport {
    /// An early return (an error, a cancel): nothing final follows, so no need to wait.
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

/// Records an archive's members as content rows linked to the archive's own -- replacing
/// whatever it was linked to before, so a member that changed (a rebuilt sheet under another
/// `sheet_style`) does not leave its old row hanging off the archive too.
async fn record_members(pool: &MySqlPool, platform: Platform, archive_content: u64, members: &[crate::rom::hash::FileHashRecord], summary: &mut ScanSummary) -> Result<()> {
    let mut linked = Vec::with_capacity(members.len());
    for m in members {
        let report = HashReport { crc32: m.crc32.clone(), md5: m.md5.clone(), sha1: m.sha1.clone(), sha256: m.sha256.clone(), ..Default::default() };
        if report.has_no_fallback() {
            continue;
        }
        let name = m.path.rsplit(['/', '\\']).next().unwrap_or(&m.path).to_string();
        let child = upsert_content(pool, platform, &name, m.size_bytes.unwrap_or(0), &report, summary).await?;
        crate::database::content::link_member(pool, archive_content, child, Some(&m.path)).await?;
        linked.push(child);
        summary.members_hashed += 1;
    }
    if !members.is_empty() {
        crate::database::content::unlink_members_except(pool, archive_content, &linked).await?;
    }
    Ok(())
}

/// A `.wua` written by a copy (or chdlite) carries the hashes of the disc it was made from in
/// its manifest: no DAT hashes a `.wua` itself. Once every file in it re-hashes to what the
/// manifest says, the disc is recorded as the archive's one member, so the archive matches the
/// release that disc matched -- as a CHD matches through its tracks. A `.wua` made elsewhere
/// has no manifest and is left as it was.
async fn record_wua_source(pool: &MySqlPool, platform: Platform, wua_content: u64, path: &Path, summary: &mut ScanSummary) -> Result<()> {
    use crate::rom::cemu_wrapper::{self, SourceRecord, Verified};
    if !cemu_wrapper::available() {
        return Ok(());
    }
    let wua = path.to_path_buf();
    // A .zar (Xbox 360) carries the same manifest as a .wua, for its files at the root.
    let zar = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("zar"));
    let verified = tokio::task::spawn_blocking(move || if zar { crate::rom::xbox::verify_zar(&wua, |_, _| true) } else { cemu_wrapper::verify(&wua, |_, _| true) })
        .await
        .map_err(|e| Error::internal(e.to_string()))?
        .map_err(|e| Error::internal(e.to_string()))?;
    let manifest = match verified {
        Verified::Intact(manifest) => manifest,
        Verified::NoManifest => return Ok(()),
        Verified::Mismatch { path: member, reason, .. } => return Err(Error::internal(format!("does not match its manifest: {member}: {reason}"))),
    };
    let Ok(source) = serde_json::from_value::<SourceRecord>(manifest.source) else { return Ok(()) };
    let report = HashReport { crc32: source.crc32.clone(), md5: source.md5.clone(), sha1: source.sha1.clone(), ..Default::default() };
    if report.has_no_fallback() || source.name.is_empty() {
        return Ok(());
    }
    let child = upsert_content(pool, platform, &source.name, source.size.unwrap_or(0), &report, summary).await?;
    crate::database::content::link_member(pool, wua_content, child, Some(&source.name)).await?;
    crate::database::content::unlink_members_except(pool, wua_content, &[child]).await?;
    summary.members_hashed += 1;
    Ok(())
}

/// No DAT hashes a whole `.nsp`: No-Intro's digital Switch DATs hash each NCA in it. So the
/// NCAs are recorded as the package's members, and the package matches the release they
/// belong to, as a CHD matches through its tracks. When every NCA's name is a DAT entry the
/// package is not read at all; otherwise each NCA is hashed. An NSP whose content row
/// already has its members (the same bytes scanned before) is not read again.
async fn record_nsp_ncas(pool: &MySqlPool, platform: Platform, nsp_content: u64, path: &Path, summary: &mut ScanSummary) -> Result<()> {
    use crate::rom::hash::HashFlags;
    let known: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM content_contents WHERE parent_content_id = ?").bind(nsp_content).fetch_one(pool).await?;
    if known > 0 {
        return Ok(());
    }
    // An NCA's name is the first half of its SHA-256, and a DAT lists NCAs by that name and
    // hash: when every NCA is a DAT entry already, the package is linked without reading it.
    let listed = path.to_path_buf();
    let entries = tokio::task::spawn_blocking(move || crate::rom::platforms::switch::pfs0_entries(&listed)).await.map_err(|e| Error::internal(e.to_string()))?.unwrap_or_default();
    let ncas: Vec<_> = entries.into_iter().filter(|e| e.name.to_ascii_lowercase().ends_with(".nca")).collect();
    if !ncas.is_empty() {
        let mut known = Vec::with_capacity(ncas.len());
        for nca in &ncas {
            match dat_nca(pool, nca).await? {
                Some(id) => known.push((id, &nca.name)),
                None => break,
            }
        }
        if known.len() == ncas.len() {
            for (child, name) in known {
                crate::database::content::link_member(pool, nsp_content, child, Some(name)).await?;
                summary.content_reused += 1;
            }
            return Ok(());
        }
    }
    let nsp = path.to_path_buf();
    let flags = HashFlags::CRC32 | HashFlags::MD5 | HashFlags::SHA1 | HashFlags::SHA256;
    let ncas = tokio::task::spawn_blocking(move || crate::rom::platforms::switch::hash_ncas(&nsp, flags))
        .await
        .map_err(|e| Error::internal(e.to_string()))?
        .map_err(|e| Error::internal(e.to_string()))?;
    let mut linked = Vec::with_capacity(ncas.len());
    for (entry, report) in ncas {
        let child = upsert_content(pool, platform, &entry.name, entry.size, &report, summary).await?;
        crate::database::content::link_member(pool, nsp_content, child, Some(&entry.name)).await?;
        linked.push(child);
        summary.members_hashed += 1;
    }
    Ok(())
}

/// The DAT entry an NCA is, by its name alone: a release content of the same size whose
/// SHA-256 starts with the name. A DAT without SHA-256 finds nothing here, and the NSP is hashed.
async fn dat_nca(pool: &MySqlPool, nca: &crate::rom::platforms::switch::Pfs0Entry) -> Result<Option<u64>> {
    let Some(id) = nca.name.get(..32).filter(|id| id.bytes().all(|b| b.is_ascii_hexdigit())).map(str::to_ascii_lowercase) else { return Ok(None) };
    Ok(sqlx::query_scalar(
        "SELECT h.content_id FROM hashes h JOIN content c ON c.id = h.content_id AND c.deleted = FALSE AND c.size = ? \
         WHERE h.algorithm = 'sha256' AND h.value_hex LIKE CONCAT(?, '%') AND EXISTS (SELECT 1 FROM release_contents rc WHERE rc.content_id = h.content_id) LIMIT 1",
    )
    .bind(nca.size)
    .bind(&id)
    .fetch_optional(pool)
    .await?)
}

/// The content row for these bytes (`database::content`), counted in the summary.
async fn upsert_content(pool: &MySqlPool, platform: Platform, file_name: &str, size: u64, report: &HashReport, summary: &mut ScanSummary) -> Result<u64> {
    let (id, reused) = crate::database::content::upsert_content(pool, platform, file_name, size, report).await?;
    if reused {
        summary.content_reused += 1;
    } else {
        summary.content_new += 1;
    }
    Ok(id)
}

/// Which of `local_files`' two check stamps a write has earned.
///
/// The distinction is `common::models::FileData`'s own, and the columns exist to persist it:
/// `full` is what `FileData::hash` sets -- every configured algorithm, archive members and
/// all -- and `integrity` is what `FileData::integrity` sets, one hash compared against a
/// stored one. A scan earns the first; a copy that reused a known `content_id` earns
/// neither, and earns the second only if it read its output back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FileChecks {
    pub full: bool,
    pub integrity: bool,
}

impl FileChecks {
    /// A scan: it just hashed the file with everything configured.
    pub const FULL: Self = Self { full: true, integrity: false };
}

/// Whether `path` is recorded (live or soft-deleted) with other bytes than `content_id`.
async fn content_replaced(pool: &MySqlPool, path: &str, content_id: u64) -> Result<bool> {
    let old: Option<Option<u64>> = sqlx::query_scalar("SELECT content_id FROM local_files WHERE path_hash = ?").bind(sha1_hex(path)).fetch_optional(pool).await?;
    Ok(matches!(old, Some(Some(old)) if old != content_id))
}

/// For roms whose bytes changed: drops their games' release links that no live file of the
/// game backs any more -- neither the file's content nor one of its members is a content of
/// that release. Run once a scan has recorded every file, so a disc not re-read yet does not
/// count as gone. Returns the links dropped.
async fn drop_unbacked_release_links(pool: &MySqlPool, roms: &[u64]) -> Result<u64> {
    if roms.is_empty() {
        return Ok(0);
    }
    let list = roms.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let done = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DELETE lrg FROM local_release_games lrg WHERE lrg.game_id IN (SELECT game_id FROM local_game_roms WHERE rom_id IN ({list})) \
         AND NOT EXISTS (SELECT 1 FROM local_game_roms lgr JOIN local_rom_files lrf ON lrf.rom_id = lgr.rom_id JOIN local_files f ON f.id = lrf.file_id AND f.deleted = FALSE \
             JOIN release_contents rc ON rc.release_id = lrg.release_id AND (rc.content_id = f.content_id OR rc.content_id IN (SELECT cc.child_content_id FROM content_contents cc WHERE cc.parent_content_id = f.content_id)) \
             WHERE lgr.game_id = lrg.game_id)"
    )))
    .execute(pool)
    .await?;
    Ok(done.rows_affected())
}

pub(crate) async fn upsert_local_file(pool: &MySqlPool, path: &str, content_id: u64, directory_id: u64, archive_format: Option<&str>, size: u64, modified: Option<chrono::NaiveDateTime>, checks: FileChecks) -> Result<u64> {
    let result = sqlx::query(
        "INSERT INTO local_files (path, path_hash, content_id, directory_id, archive_format, size, deleted, last_modified, last_full_check, last_integrity_check) \
         VALUES (?, ?, ?, ?, ?, ?, FALSE, ?, IF(?, NOW(), NULL), IF(?, NOW(), NULL)) \
         ON DUPLICATE KEY UPDATE \
             last_integrity_check = IF(content_id <=> VALUES(content_id), COALESCE(VALUES(last_integrity_check), last_integrity_check), VALUES(last_integrity_check)), \
             content_id = VALUES(content_id), directory_id = VALUES(directory_id), archive_format = VALUES(archive_format), size = VALUES(size), deleted = FALSE, last_modified = VALUES(last_modified), \
             last_full_check = COALESCE(VALUES(last_full_check), last_full_check), id = LAST_INSERT_ID(id)",
    )
    .bind(path)
    .bind(sha1_hex(path))
    .bind(content_id)
    .bind(directory_id)
    .bind(archive_format)
    .bind(size)
    .bind(modified)
    .bind(checks.full)
    .bind(checks.integrity)
    .execute(pool)
    .await?;
    Ok(result.last_insert_id())
}

/// Moves the live files under `dir` that are recorded with a directory above it (a scan of
/// that folder got there first) to `dir`, the most specific configured directory they lie in.
/// Returns how many moved.
async fn adopt_files_below(pool: &MySqlPool, dir: &Directory) -> Result<u64> {
    let all = db_dirs::list(pool, true).await?;
    let above: Vec<u64> = all.iter().filter(|d| d.id != dir.id && !db_dirs::same_path(&d.path, &dir.path) && db_dirs::path_contains(&d.path, &dir.path)).map(|d| d.id).collect();
    if above.is_empty() {
        return Ok(0);
    }
    let list = above.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let rows: Vec<(u64, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT id, path FROM local_files WHERE deleted = FALSE AND directory_id IN ({list})"))).fetch_all(pool).await?;
    let ours: Vec<u64> = rows.into_iter().filter(|(_, path)| db_dirs::path_contains(&dir.path, path)).map(|(id, _)| id).collect();
    for part in ours.chunks(1000) {
        let ids = part.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE local_files SET directory_id = ? WHERE id IN ({ids})"))).bind(dir.id).execute(pool).await?;
    }
    Ok(ours.len() as u64)
}

async fn local_file_is_live(pool: &MySqlPool, path: &str) -> Result<bool> {
    let row = sqlx::query("SELECT deleted FROM local_files WHERE path_hash = ?").bind(sha1_hex(path)).fetch_optional(pool).await?;
    Ok(matches!(row, Some(r) if !r.try_get::<bool, _>("deleted").unwrap_or(true)))
}

/// One `local_rom` per input path; its file set is replaced with what was just hashed.
///
/// Shared with `api::copy`, which registers what it wrote through these same three writers
/// rather than a second implementation of them.
pub(crate) async fn upsert_rom(pool: &MySqlPool, main_path: &str, platform: Platform, file_ids: &[u64]) -> Result<(u64, bool)> {
    let existing = sqlx::query("SELECT id FROM local_rom WHERE main_path = ? AND deleted = FALSE ORDER BY id LIMIT 1").bind(main_path).fetch_optional(pool).await?;
    let (rom_id, is_new) = match existing {
        Some(row) => {
            let id: u64 = row.try_get("id")?;
            if platform != Platform::Unknown {
                sqlx::query("UPDATE local_rom SET app_platform_id = ? WHERE id = ?").bind(platform.igdb_id()).bind(id).execute(pool).await?;
            }
            (id, false)
        }
        None => {
            let result = sqlx::query("INSERT INTO local_rom (app_platform_id, main_path) VALUES (?, ?)").bind(platform.igdb_id()).bind(main_path).execute(pool).await?;
            (result.last_insert_id(), true)
        }
    };
    sqlx::query("DELETE FROM local_rom_files WHERE rom_id = ?").bind(rom_id).execute(pool).await?;
    for file_id in file_ids {
        sqlx::query("INSERT IGNORE INTO local_rom_files (rom_id, file_id) VALUES (?, ?)").bind(rom_id).bind(file_id).execute(pool).await?;
    }
    supersede_covered_roms(pool, rom_id, file_ids).await?;
    Ok((rom_id, is_new))
}

/// Retires the other roms whose every file is one of `rom_id`'s: what an earlier scan made of
/// pieces this rom now holds whole -- a DSi CDN folder's `00000001`, `tmd.1` and `titleKey.bin`,
/// each once a game of its own, now one folder game; a disc's tracks seen without their sheet.
/// Their games go with them when nothing is left in them.
async fn supersede_covered_roms(pool: &MySqlPool, rom_id: u64, file_ids: &[u64]) -> Result<()> {
    if file_ids.is_empty() {
        return Ok(());
    }
    let files = file_ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let covered: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT DISTINCT lrf.rom_id FROM local_rom_files lrf JOIN local_rom lr ON lr.id = lrf.rom_id AND lr.deleted = FALSE \
         WHERE lrf.file_id IN ({files}) AND lrf.rom_id <> ? \
           AND NOT EXISTS (SELECT 1 FROM local_rom_files other WHERE other.rom_id = lrf.rom_id AND other.file_id NOT IN ({files}))"
    )))
    .bind(rom_id)
    .fetch_all(pool)
    .await?;
    if covered.is_empty() {
        return Ok(());
    }
    let list = covered.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let mut tx = pool.begin().await?;
    let games: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT DISTINCT game_id FROM local_game_roms WHERE rom_id IN ({list})"))).fetch_all(&mut *tx).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM local_rom_files WHERE rom_id IN ({list})"))).execute(&mut *tx).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE local_rom SET deleted = TRUE WHERE id IN ({list})"))).execute(&mut *tx).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM local_game_roms WHERE rom_id IN ({list})"))).execute(&mut *tx).await?;
    retire_empty_games(&mut tx, &games).await?;
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn ensure_game(pool: &MySqlPool, rom_id: u64, name: &str) -> Result<(u64, bool)> {
    let existing = sqlx::query("SELECT lg.id FROM local_game_roms lgr JOIN local_game lg ON lg.id = lgr.game_id WHERE lgr.rom_id = ? AND lg.deleted = FALSE ORDER BY lg.id LIMIT 1").bind(rom_id).fetch_optional(pool).await?;
    if let Some(row) = existing {
        return Ok((row.try_get("id")?, false));
    }
    let result = sqlx::query("INSERT INTO local_game (name) VALUES (?)").bind(name).execute(pool).await?;
    let game_id = result.last_insert_id();
    sqlx::query("INSERT INTO local_game_roms (game_id, rom_id) VALUES (?, ?)").bind(game_id).bind(rom_id).execute(pool).await?;
    Ok((game_id, true))
}

async fn load_directory(app: &App, id: u64) -> Result<(Directory, PathBuf)> {
    let dir = match db_dirs::get(&app.pool, id).await? {
        Some(d) if !d.deleted => d,
        Some(_) => return Err(Error::invalid(format!("directory {id} is deleted"))),
        None => return Err(Error::not_found(format!("no directory {id}"))),
    };
    let root = PathBuf::from(&dir.path);
    if !root.is_dir() {
        return Err(Error::invalid(format!("{} is not a directory on this machine", dir.path)));
    }
    Ok((dir, root))
}

/// The directory rows a match over `dir` covers, as an SQL `IN` list: `dir` itself and every
/// configured row beneath it. `local_files.directory_id` names the row a file was *scanned*
/// under, so matching a collection root has to reach the files its children scanned --
/// with `= dir.id` alone it found no games at all. Soft-deleted rows count: their files keep
/// pointing at them.
async fn directory_scope(pool: &MySqlPool, dir: &Directory) -> Result<String> {
    let all = db_dirs::list(pool, true).await?;
    Ok(all.iter().filter(|d| d.id == dir.id || db_dirs::path_contains(&dir.path, &d.path)).map(|d| d.id.to_string()).collect::<Vec<_>>().join(","))
}

/// `as_roms`: false in a directory of type `patch` -- its files are hashed and listed
/// (so a patch copied in by hand is recognized), but not made into ROMs / games.
#[allow(clippy::too_many_arguments)]
/// Writes one candidate's scanned files: a content row per file (its members linked under
/// it), a `local_files` row per file, a sheet's tracks linked under the sheet, then the rom
/// and its game.
/// Records one candidate; the rom's id when it was recorded as a rom.
async fn record_candidate(app: &App, dir: &Directory, candidate: &RomCandidate, platform: Platform, as_roms: bool, outcome: &SingleOutcome, summary: &mut ScanSummary) -> Result<Option<u64>> {
    let set = &outcome.input;
    let archive_tag = match set.kind {
        InputKind::Archive(format) => Some(format.to_string()),
        InputKind::PlainFile if candidate.path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("iso")) => Some(crate::common::models::ArchiveFormat::RawIso.to_string()),
        _ => None,
    };
    let files = scanned_files(outcome).map_err(Error::internal)?;
    let mut file_ids = Vec::new();
    // A cue / gdi sheet's contents are its tracks: recorded like an archive's members.
    let is_sheet = matches!(set.kind, InputKind::Sheet(_));
    let mut sheet_content: Option<u64> = None;
    let mut replaced = false;
    for file in files {
        summary.files_hashed += 1;
        let file_name = file.path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        let content_id = upsert_content(&app.pool, platform, &file_name, file.hashed.size, &file.hashed.report, summary).await?;
        if let Err(e) = record_members(&app.pool, platform, content_id, &file.members, summary).await {
            summary.errors.push(format!("{}: members: {e}", file.path.display()));
        }
        if file.path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("wua") || e.eq_ignore_ascii_case("zar")) {
            if let Err(e) = record_wua_source(&app.pool, platform, content_id, &file.path, summary).await {
                summary.errors.push(format!("{}: {e}", file.path.display()));
            }
        }
        if file.path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("nsp")) {
            if let Err(e) = record_nsp_ncas(&app.pool, platform, content_id, &file.path, summary).await {
                summary.errors.push(format!("{}: {e}", file.path.display()));
            }
        }
        if is_sheet {
            if file.path == candidate.path {
                sheet_content = Some(content_id);
            } else if let Some(sheet) = sheet_content {
                crate::database::content::link_member(&app.pool, sheet, content_id, Some(&file_name)).await?;
            }
        }
        let tag = if file.path == candidate.path { archive_tag.as_deref() } else { None };
        let path = file.path.display().to_string();
        if content_replaced(&app.pool, &path, content_id).await? {
            summary.contents_replaced += 1;
            replaced = true;
        }
        let id = upsert_local_file(&app.pool, &path, content_id, dir.id, tag, file.hashed.size, file.hashed.modified, FileChecks::FULL).await?;
        file_ids.push(id);
    }
    if file_ids.is_empty() || !as_roms {
        return Ok(None);
    }
    let (rom_id, rom_new) = upsert_rom(&app.pool, &candidate.path.display().to_string(), platform, &file_ids).await?;
    if rom_new {
        summary.roms_new += 1;
    }
    if replaced && !rom_new {
        // The platform was the old bytes'; an unconfirmed new one is Unknown, not the old.
        sqlx::query("UPDATE local_rom SET app_platform_id = ? WHERE id = ?").bind(platform.igdb_id()).bind(rom_id).execute(&app.pool).await?;
        summary.replaced_roms.push(rom_id);
    }
    let (_, game_new) = ensure_game(&app.pool, rom_id, &set.stem).await?;
    if game_new {
        summary.games_new += 1;
    }
    Ok(Some(rom_id))
}

/// The RetroAchievements hash of `path`: a CHD, RVZ, CSO, PBP or an archived cartridge read in
/// memory (`rom::ra_disc`), anything else read by RAHasher where it lies (a cue finds its
/// tracks). Nothing is extracted to disk.
fn ra_hash_of(path: &Path, console: u32) -> std::result::Result<hash::RahashOutcome, hash::RahashError> {
    if crate::rom::ra_disc::handles(path) {
        // N64, SNES, NES, PC Engine, Lynx, DS, 7800, DSi, FDS: the cartridge (or disk) systems
        // whose RA hash is not the file's MD5.
        let cartridge = matches!(console, 2 | 3 | 7 | 8 | 13 | 18 | 51 | 78 | 81);
        return crate::rom::ra_disc::hash_of(path, console, cartridge).map(hash::RahashOutcome::Done);
    }
    hash::rahash_file(path, console, 0)
}

type RaHashResult = std::result::Result<hash::RahashOutcome, hash::RahashError>;

/// The RetroAchievements hash of a file just read, on the scan worker's own thread, so it runs
/// as many at once as the hashing does (and finds the file still in the cache). `None` when
/// the platform needs none, or RAHasher was already found missing this run.
fn ra_hash_in_worker(path: &Path, platform: Platform, missing: &std::sync::atomic::AtomicBool) -> Option<RaHashResult> {
    use std::sync::atomic::Ordering;
    let console = hash::ra_console_id(platform)?;
    if platform == Platform::Unknown || hash::ra_hash_is_file_md5(console) || missing.load(Ordering::Relaxed) {
        return None;
    }
    let outcome = ra_hash_of(path, console);
    if matches!(outcome, Err(hash::RahashError::Unavailable(_))) {
        missing.store(true, Ordering::Relaxed);
    }
    Some(outcome)
}

/// The RetroAchievements hash of a rom, for a console whose hash is not the file's own MD5:
/// RAHasher reads the rom where it lies (a cue finds its tracks), and the hash is kept as the
/// rom's (`local_rom_rahash`), replacing the one an older scan left. A hash match then links
/// it through `retroachievements_hashes`.
///
/// `ready` is the hash the scan's worker already worked out (`ra_hash_in_worker`); without it
/// the file is read here, one at a time.
async fn record_ra_hash(pool: &MySqlPool, rom_id: u64, path: &Path, platform: Platform, ready: Option<RaHashResult>, summary: &mut ScanSummary) -> Result<()> {
    let Some(console) = hash::ra_console_id(platform) else { return Ok(()) };
    if hash::ra_hash_is_file_md5(console) || summary.ra_hasher_missing {
        return Ok(());
    }
    let outcome = match ready {
        Some(outcome) => outcome,
        None => {
            let at = path.to_path_buf();
            tokio::task::spawn_blocking(move || ra_hash_of(&at, console)).await.map_err(|e| Error::internal(e.to_string()))?
        }
    };
    let value = match outcome {
        Ok(hash::RahashOutcome::Done(value)) if !value.is_empty() => value.to_ascii_lowercase(),
        Ok(_) => return Ok(()),
        Err(hash::RahashError::Unavailable(message)) => {
            log::info!("scan: no RetroAchievements hashes this run: {message}");
            summary.ra_hasher_missing = true;
            return Ok(());
        }
        Err(hash::RahashError::Failed(message)) => {
            log::debug!("scan: RA hash of {}: {message}", path.display());
            summary.ra_hash_failed += 1;
            return Ok(());
        }
    };
    // Rom-level hashes have no content: one row per value, shared by every rom that has it.
    let existing: Option<u64> = sqlx::query_scalar("SELECT id FROM hashes WHERE content_id IS NULL AND algorithm = 'ra_hash' AND value_hex = ? ORDER BY id LIMIT 1").bind(&value).fetch_optional(pool).await?;
    let hash_id = match existing {
        Some(id) => id,
        None => sqlx::query("INSERT INTO hashes (content_id, algorithm, value_hex) VALUES (NULL, 'ra_hash', ?)").bind(&value).execute(pool).await?.last_insert_id(),
    };
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM local_rom_rahash WHERE rom_id = ? AND hash_id <> ?").bind(rom_id).bind(hash_id).execute(&mut *tx).await?;
    sqlx::query("INSERT IGNORE INTO local_rom_rahash (rom_id, hash_id) VALUES (?, ?)").bind(rom_id).bind(hash_id).execute(&mut *tx).await?;
    tx.commit().await?;
    summary.ra_hashes += 1;
    Ok(())
}

/// Progress: "walking" while the directory is listed, then bytes hashed of the candidates'
/// total size, with the file count and current file in the message.
pub async fn run_file_scan(app: &App, req: &FileScanRequest, progress: &crate::database::jobs::Progress) -> Result<ScanSummary> {
    run_file_scan_since(app, req, None, progress).await
}

/// Whether a full scan that began at `since` already read the file at `path`: its row is
/// live, was fully checked after `since`, and the file is the size and age the row says.
/// Only a plain file can be judged this way (a folder candidate is always read again).
async fn read_since(pool: &MySqlPool, path: &std::path::Path, since: chrono::NaiveDateTime) -> Result<bool> {
    let Ok(meta) = std::fs::metadata(path) else { return Ok(false) };
    if !meta.is_file() {
        return Ok(false);
    }
    let row = sqlx::query("SELECT size, last_modified FROM local_files WHERE path_hash = ? AND deleted = FALSE AND last_full_check > ?").bind(sha1_hex(&path.display().to_string())).bind(since).fetch_optional(pool).await?;
    let Some(row) = row else { return Ok(false) };
    let size: Option<u64> = row.try_get("size")?;
    let recorded: Option<chrono::NaiveDateTime> = row.try_get("last_modified")?;
    let modified = meta.modified().ok().map(|t| chrono::DateTime::<chrono::Utc>::from(t).naive_utc());
    // The column keeps whole seconds; the file system keeps more.
    let same_age = match (recorded, modified) {
        (Some(a), Some(b)) => (a - b).num_seconds().abs() <= 1,
        _ => true,
    };
    Ok(size == Some(meta.len()) && same_age)
}

/// [`run_file_scan`] as a job runs it. `since` is when the job was queued: a full scan
/// that is started again -- the server restarted under it, or an earlier try broke off --
/// goes on from where it was instead of from the first byte. What it already read (a live
/// row fully checked after `since`, for a file unchanged on disk) is not read again, and
/// the files-gone-missing line is `since` too, so what the earlier run recorded is not
/// taken for missing. On a library of ten terabytes a restart used to cost the whole scan.
pub async fn run_file_scan_since(app: &App, req: &FileScanRequest, since: Option<chrono::NaiveDateTime>, progress: &crate::database::jobs::Progress) -> Result<ScanSummary> {
    let (dir, root) = load_directory(app, req.directory_id).await?;
    let effective = db_dirs::resolve(&app.pool, &dir.path).await?;
    let forced = match req.platform {
        // Validated in `jobs::validate`, so an unknown id never gets this far.
        PlatformSelect::Id(id) => Platform::from_igdb_id(id),
        PlatformSelect::All => effective.app_platform_id.and_then(Platform::from_igdb_id),
    };
    // Read once per scan, not per file: which algorithms to compute, and whether an RA hash
    // was asked for. `ra_wanted` is tracked separately because it needs a console id the
    // scan cannot always resolve -- see `hash_options`.
    let (flags, ra_wanted) = settings::scan_hashes(&app.pool).await?;
    settings::apply_chd_reader(&app.pool).await?;
    settings::apply_scan_pkg_titles(&app.pool).await?;
    let _ = crate::rom::chd_info::take_check_report();
    if let Some(file_id) = req.file_id {
        return rescan_file(app, req, file_id, flags, ra_wanted, progress).await;
    }
    let mut summary = ScanSummary { mode: req.mode.name().to_string(), directory: dir.path.clone(), ..Default::default() };

    // Files under this directory that a scan of a folder above it recorded are this
    // directory's from now on: a quick scan skips them as known, a rehash reads only its own
    // directory's rows, and the hash match after the scan looks only at this directory's --
    // so left with the folder above, they were never read or matched from here.
    let adopted = adopt_files_below(&app.pool, &dir).await?;
    if adopted > 0 {
        log::info!("scan of {}: took over {adopted} file(s) a scan of a folder above it recorded", dir.path);
    }
    if req.mode == FileScanMode::Rehash {
        return rehash(app, &dir, forced, flags, summary, progress).await;
    }
    // A full scan marks what it did not see as missing at the end, not everything up front:
    // marking first left a whole library "missing" for as long as the scan ran, and for good
    // when it never finished (cancelled, server restarted). What this scan records gets
    // `last_full_check = NOW()`, so the database's own clock is the line between the two.
    let full_scan_started: Option<chrono::NaiveDateTime> = if req.mode == FileScanMode::Full {
        // The cast is not decoration: `NOW()` is a TIMESTAMP on newer MariaDB and a DATETIME
        // on older, and sqlx decodes a `NaiveDateTime` from a DATETIME only -- a bare
        // `SELECT NOW()` failed every full scan on the servers where it is the former.
        let now: chrono::NaiveDateTime = sqlx::query_scalar("SELECT CAST(NOW() AS DATETIME)").fetch_one(&app.pool).await?;
        // The job's own start when it has one: the same clock, the same column.
        Some(since.map_or(now, |s| s.min(now)))
    } else {
        None
    };

    let layout = match forced {
        Some(p) => PlatformLayout::Forced(p),
        None => PlatformLayout::SeparatedBySubdir(configured_subfolder_platforms(app, &dir).await?),
    };
    let as_roms = effective.dir_type.as_deref() != Some("patch");
    let dir_type = match effective.dir_type.as_deref() {
        Some("bios") => DirType::Bios,
        Some("rom") => DirType::Rom,
        _ => DirType::Mixed,
    };
    let (ext_heuristics, path_heuristics) = settings::scan_heuristics(&app.pool).await?;
    let files_seen = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let ctx = ScanContext { dir_type, platform: PlatformContext::None, heuristics: HeuristicFlags::new(ext_heuristics, path_heuristics), shapes: Vec::new(), files_seen: Some(std::sync::Arc::clone(&files_seen)) };
    progress.force(0, None, &format!("walking {}", dir.path)).await;
    // The walk runs to completion on a blocking thread and only then says anything, so the
    // count of files it has listed is reported from here while it runs: before this, a scan
    // of a large directory showed "walking <dir>" and nothing else until hashing began.
    let mut walking = tokio::task::spawn_blocking(move || rom_scan::scan(&root, &layout, &ctx, false));
    let walk: WalkResult = loop {
        tokio::select! {
            joined = &mut walking => break joined.map_err(|e| Error::internal(format!("scan task: {e}")))?,
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                let seen = files_seen.load(std::sync::atomic::Ordering::Relaxed);
                progress.force(0, None, &format!("walking {}: {seen} file(s) found so far", dir.path)).await;
            }
        }
    };
    // The system files the walk identified (a BIOS, firmware, a NAND dump, a DSP ROM) are
    // read like everything else -- recorded, so a hash match can link them and Collection ›
    // BIOS / firmware can list them -- and what they were identified as is noted on their row
    // after it is written. They used to be set aside and never recorded at all.
    let mut walk = walk;
    let mut system_files: Vec<(String, &'static str, Option<u16>)> = Vec::with_capacity(walk.system_files.len());
    for (path, found) in std::mem::take(&mut walk.system_files) {
        let platform = found.platform.filter(|p| *p != Platform::Unknown);
        system_files.push((path.display().to_string(), system_type_name(found.file_type), platform.map(|p| p.igdb_id())));
        walk.rom_candidates.push(RomCandidate { path, kind: rom_scan::CandidateKind::Unknown, platform_hint: platform });
    }
    progress.force(0, None, &format!("walked {}: {} file(s) found, {} to look at", dir.path, files_seen.load(std::sync::atomic::Ordering::Relaxed), walk.rom_candidates.len())).await;
    summary.candidates = walk.rom_candidates.len() as u64;
    summary.playlists_seen = walk.playlists.len() as u64;

    let sizes: Vec<u64> = walk.rom_candidates.iter().map(|c| candidate_bytes(&c.path)).collect();
    let total_bytes: u64 = sizes.iter().sum();
    let n = walk.rom_candidates.len();

    // A new-files-only scan skips what is already recorded: decided before anything is
    // hashed, so the prefetch never reads a file the loop will not record.
    let mut skip = vec![false; n];
    if req.mode == FileScanMode::NewOnly {
        for (i, candidate) in walk.rom_candidates.iter().enumerate() {
            skip[i] = local_file_is_live(&app.pool, &candidate.path.display().to_string()).await?;
        }
    }
    // A full scan started again goes on from where it was (`run_file_scan_since`).
    if let (FileScanMode::Full, Some(since)) = (req.mode, since) {
        let mut resumed = 0u64;
        for (i, candidate) in walk.rom_candidates.iter().enumerate() {
            if read_since(&app.pool, &candidate.path, since).await? {
                skip[i] = true;
                resumed += 1;
            }
        }
        if resumed > 0 {
            progress.force(0, None, &format!("going on from an earlier run of this scan: {resumed} of {n} file(s) already read")).await;
        }
    }
    summary.skipped_existing = skip.iter().filter(|s| **s).count() as u64;
    let style = super::sheets::style_setting(app).await?;
    let limits = settings::parallel_files(&app.pool, settings::KEY_SCAN_PARALLEL).await?.resolve_for(std::path::PathBuf::from(&dir.path), flags, crate::rom::operation::JobStages::default()).await?;
    progress.force(0, Some(total_bytes), &format!("hashing up to {} small / {} medium / {} large file(s) at once", limits.small, limits.medium, limits.large)).await;
    let multiple = Multiple::new(limits);
    let _stop_on_drop = CancelOnDrop(multiple.clone());
    let mut jobs = Vec::new();
    for (i, candidate) in walk.rom_candidates.iter().enumerate() {
        if skip[i] {
            continue;
        }
        let detect = detect_for(candidate, hint_for(candidate, forced));
        let name = candidate.path.display().to_string();
        jobs.push((first_folder(Path::new(&dir.path), &candidate.path), sizes[i], name, (candidate.path.clone(), scan_args(detect, flags, style, false), sizes[i], (i, detect))));
    }
    // Every candidate is one `Single`, read once whatever it is: a plain file, an archive and
    // its members, a sheet and its tracks. Results are written as each finishes; the writes
    // themselves stay one at a time, so two files of one game, or two copies of the same
    // bytes, never race to create the same rows. One first-level folder at a time, and in it
    // one size at a time, smallest first (`Multiple::run_in_turn`).
    // The RetroAchievements hash, where it isn't the MD5 (N64, SNES, discs, ...), is worked
    // out by the same worker right after the file is read: in the recording loop below it
    // read every such file again, one at a time, while the parallel hashing sat idle.
    let ra_missing = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_ra = ra_wanted && as_roms;
    let mut running = multiple.run_tracked_in_turn(jobs, move |(input, args, size, tag): (PathBuf, crate::rom::operation::SingleArgs, u64, (usize, Detect)), tracker: &mut crate::rom::multiple::Tracker<'_>| {
        let (tag, result) = crate::rom::multiple::run_single((input.clone(), args, size, tag), tracker);
        let ra = match &result {
            Ok(outcome) if worker_ra => ra_hash_in_worker(&input, outcome.metadata.platform, &ra_missing),
            _ => None,
        };
        (tag, result, ra)
    });
    let mut finished = 0usize;
    let wanted = n - summary.skipped_existing as usize;
    let report = RunningReport::start(&multiple, progress, format!("hashing {wanted} candidate(s)"));
    while let Some(((), ((i, detect), result, ra_ready))) = running.next().await {
        if multiple.is_cancelled() {
            return Err(Error::internal("cancelled"));
        }
        finished += 1;
        let candidate = &walk.rom_candidates[i];
        let main = candidate.path.display().to_string();
        let name = candidate.path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| main.clone());
        // Said before the database writes, so a file slow to record shows as that file.
        report.set(format!("{} of {wanted} done", finished - 1), format!(", recording: {name}"));
        let outcome = match result {
            Ok(outcome) => Ok(outcome),
            // A sheet naming a track that is not there: the sheet is still a file worth
            // recording, on its own, and the missing track an error worth reading.
            Err(SingleError::Input(message)) if candidate.path.is_file() => {
                if lone_sheet(&candidate.path) {
                    summary.warnings.push(format!("{main}: none of the sheet's track files are beside it"));
                } else {
                    summary.errors.push(format!("{main}: {message}"));
                }
                scan_one(candidate.path.clone(), scan_args(detect, flags, style, true)).await
            }
            Err(e) => Err(Error::internal(e.to_string())),
        };
        let recorded = match outcome {
            Ok(outcome) => {
                let platform = outcome.metadata.platform;
                // `ra_hash` was asked for but this file's platform wasn't confirmed, so no
                // RetroAchievements console id exists to hash against. Count it and leave the
                // value blank -- `hash_options` explains why guessing is worse than omitting.
                if ra_wanted && platform == Platform::Unknown {
                    summary.ra_hash_skipped_unknown_platform += 1;
                }
                if platform == Platform::Unknown {
                    summary.platform_unconfirmed += 1;
                }
                match record_candidate(app, &dir, candidate, platform, as_roms, &outcome, &mut summary).await {
                    Ok(Some(rom_id)) if ra_wanted && platform != Platform::Unknown => record_ra_hash(&app.pool, rom_id, &candidate.path, platform, ra_ready, &mut summary).await,
                    Ok(_) => Ok(()),
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        };
        if let Err(e) = recorded {
            summary.errors.push(format!("{main}: {e}"));
        }
        report.set(format!("{finished} of {wanted} done"), format!(", last: {name}"));
    }
    // Stopped before the final message, so a late tick cannot write over it.
    report.finish().await;
    for playlist in &walk.playlists {
        let step = Step { progress, done_bytes: total_bytes, total_bytes, file_bytes: 0, label: "playlists".to_string() };
        if let Err(e) = apply_playlist(&app.pool, &dir, playlist, forced.unwrap_or(Platform::Unknown), flags, &mut summary, &step).await {
            summary.errors.push(format!("{}: playlist: {e}", playlist.m3u_path.display()));
        }
    }
    for (path, kind, platform) in &system_files {
        sqlx::query("UPDATE local_files SET system_type = ?, system_platform = ? WHERE path_hash = ? AND deleted = FALSE").bind(kind).bind(platform).bind(sha1_hex(path)).execute(&app.pool).await?;
    }
    if let Some(started) = full_scan_started {
        summary.marked_missing = mark_unseen_missing(&app.pool, dir.id, started).await?;
    }
    summary.release_links_dropped = drop_unbacked_release_links(&app.pool, &std::mem::take(&mut summary.replaced_roms)).await?;
    (summary.chd_compared, summary.chd_not_compared, summary.chd_reader_differences) = crate::rom::chd_info::take_check_report();
    let chd_note = if summary.chd_reader_differences.is_empty() { String::new() } else { format!(", {} CHD(s) read differently by the built-in reader and chdman (listed in the result; please report)", summary.chd_reader_differences.len()) };
    let message = format!("done: {n} candidate(s), {} hashed, {} skipped, {} error(s){}{chd_note}", summary.files_hashed, summary.skipped_existing, summary.errors.len(), warnings_note(&summary));
    progress.finish(total_bytes, Some(total_bytes), &message, &job_result(&summary)).await;
    Ok(summary)
}

/// "Rescan this file": reads the rom `file_id` belongs to again (a track reads its sheet, a
/// folder's file its folder), records it as a scan would, then matches its game(s) to the DATs
/// on the spot -- by hash, then by serial -- without walking the directory or matching the
/// rest of it. The result says what was read, what each game is linked to, and which DAT
/// releases list the bytes, so a game left unlinked shows what it should have matched.
async fn rescan_file(app: &App, req: &FileScanRequest, file_id: u64, flags: HashFlags, ra_wanted: bool, progress: &crate::database::jobs::Progress) -> Result<ScanSummary> {
    let pool = &app.pool;
    let (path, deleted): (String, bool) = sqlx::query_as("SELECT path, deleted FROM local_files WHERE id = ?").bind(file_id).fetch_optional(pool).await?.ok_or_else(|| Error::not_found(format!("no file {file_id}")))?;
    let name = path.rsplit(['/', '\\']).next().unwrap_or(&path).to_string();
    progress.force(0, None, &format!("{name}: finding the rom it belongs to")).await;
    let effective = db_dirs::resolve(pool, &path).await?;
    // Recorded under the nearest configured directory, as a scan of that directory would.
    let dir = match effective.directory.clone() {
        Some(d) => d,
        None => load_directory(app, req.directory_id).await?.0,
    };
    let forced = match req.platform {
        PlatformSelect::Id(id) => Platform::from_igdb_id(id),
        PlatformSelect::All => effective.app_platform_id.and_then(Platform::from_igdb_id),
    };
    let as_roms = effective.dir_type.as_deref() != Some("patch");
    let mut summary = ScanSummary { mode: "file".to_string(), directory: dir.path.clone(), ..Default::default() };
    let mut rescan = FileRescan { file: path.clone(), ..Default::default() };
    if !as_roms {
        summary.warnings.push(format!("{} is a patch directory: its files are recorded but not made into games", dir.path));
    }

    // What to read: the rom the file is part of, by its main file -- a track alone would be
    // recorded as a rom of its own. A file that is in no rom is read by itself.
    let mut mains: Vec<String> = sqlx::query_scalar("SELECT DISTINCT lr.main_path FROM local_rom_files lrf JOIN local_rom lr ON lr.id = lrf.rom_id AND lr.deleted = FALSE WHERE lrf.file_id = ? ORDER BY lr.main_path")
        .bind(file_id)
        .fetch_all(pool)
        .await?;
    if mains.is_empty() {
        mains.push(path.clone());
    }
    let mut candidates = Vec::new();
    for main in &mains {
        match std::fs::metadata(main) {
            Ok(_) => candidates.push(RomCandidate::of(PathBuf::from(main))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if *main == path && !deleted {
                    sqlx::query("UPDATE local_files SET deleted = TRUE WHERE id = ?").bind(file_id).execute(pool).await?;
                    summary.marked_missing += 1;
                }
                summary.errors.push(format!("{main}: not on disk"));
            }
            Err(e) => summary.errors.push(format!("{main}: {e}")),
        }
    }
    summary.candidates = candidates.len() as u64;
    rescan.read = candidates.iter().map(|c| c.path.display().to_string()).collect();

    // Read, as a scan reads: in the scan's worker pool, so the bar shows the bytes and the
    // files being read.
    let sizes: Vec<u64> = candidates.iter().map(|c| candidate_bytes(&c.path)).collect();
    let total_bytes: u64 = sizes.iter().sum();
    let style = super::sheets::style_setting(app).await?;
    let limits = settings::parallel_files(pool, settings::KEY_SCAN_PARALLEL).await?.resolve_for(PathBuf::from(&dir.path), flags, crate::rom::operation::JobStages::default()).await?;
    let multiple = Multiple::new(limits);
    let _stop_on_drop = CancelOnDrop(multiple.clone());
    let jobs = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let detect = detect_for(c, hint_for(c, forced));
            (String::new(), Job { input: c.path.clone(), args: scan_args(detect, flags, style, false), size: sizes[i], tag: (i, detect) })
        })
        .collect();
    let wanted = candidates.len();
    let mut running = multiple.run_in_turn(jobs);
    let report = RunningReport::start(&multiple, progress, format!("{name}: reading {wanted} rom(s)"));
    let mut roms: Vec<u64> = Vec::new();
    let mut finished = 0usize;
    while let Some(((i, detect), result)) = running.next().await {
        if multiple.is_cancelled() {
            return Err(Error::internal("cancelled"));
        }
        let candidate = &candidates[i];
        let main = candidate.path.display().to_string();
        report.set(format!("{name}: {finished} of {wanted} read"), format!(", recording: {main}"));
        let outcome = match result {
            Ok(outcome) => Ok(outcome),
            Err(SingleError::Input(message)) if candidate.path.is_file() => {
                if lone_sheet(&candidate.path) {
                    summary.warnings.push(format!("{main}: none of the sheet's track files are beside it"));
                } else {
                    summary.errors.push(format!("{main}: {message}"));
                }
                scan_one(candidate.path.clone(), scan_args(detect, flags, style, true)).await
            }
            Err(e) => Err(Error::internal(e.to_string())),
        };
        let recorded = match outcome {
            Ok(outcome) => {
                let platform = outcome.metadata.platform;
                if platform == Platform::Unknown {
                    summary.platform_unconfirmed += 1;
                    if ra_wanted {
                        summary.ra_hash_skipped_unknown_platform += 1;
                    }
                }
                match record_candidate(app, &dir, candidate, platform, as_roms, &outcome, &mut summary).await {
                    Ok(Some(rom_id)) => {
                        roms.push(rom_id);
                        if ra_wanted && platform != Platform::Unknown {
                            record_ra_hash(pool, rom_id, &candidate.path, platform, None, &mut summary).await
                        } else {
                            Ok(())
                        }
                    }
                    Ok(None) => Ok(()),
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        };
        if let Err(e) = recorded {
            summary.errors.push(format!("{main}: {e}"));
        }
        finished += 1;
        report.set(format!("{name}: {finished} of {wanted} read"), format!(", last: {main}"));
    }
    report.finish().await;
    summary.release_links_dropped = drop_unbacked_release_links(pool, &std::mem::take(&mut summary.replaced_roms)).await?;
    (summary.chd_compared, summary.chd_not_compared, summary.chd_reader_differences) = crate::rom::chd_info::take_check_report();

    // Its games, matched now: by hash (the DAT line covering most of their files), then by
    // serial for what that leaves.
    let to_dat = req.hash_match.map_or(true, |h| h.to_dat);
    let games: Vec<u64> = if roms.is_empty() {
        Vec::new()
    } else {
        let list = roms.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT DISTINCT lgr.game_id FROM local_game_roms lgr JOIN local_game lg ON lg.id = lgr.game_id AND lg.deleted = FALSE WHERE lgr.rom_id IN ({list}) ORDER BY lgr.game_id"))).fetch_all(pool).await?
    };
    rescan.games = games.len() as u64;
    let source_order = settings::release_source_priority(pool).await?;
    let best_sql = best_release_sql(&source_order);
    let mut unlinked: Vec<u64> = Vec::new();
    for (i, &game_id) in games.iter().enumerate() {
        progress.report(i as u64, Some(games.len() as u64), &format!("{name}: matching game {} of {} to the DATs, {} linked", i + 1, games.len(), rescan.linked.len())).await;
        let existing: Vec<u64> = sqlx::query_scalar("SELECT release_id FROM local_release_games WHERE game_id = ? ORDER BY release_id").bind(game_id).fetch_all(pool).await?;
        if !existing.is_empty() {
            for release_id in existing {
                rescan.already_linked.push(describe_link(pool, game_id, release_id).await?);
            }
            continue;
        }
        if !to_dat {
            unlinked.push(game_id);
            continue;
        }
        match best_release(pool, &best_sql, &source_order, game_id).await? {
            Some((release_id, platform_id)) => {
                link_game(pool, game_id, release_id, platform_id).await?;
                rescan.linked.push(describe_link(pool, game_id, release_id).await?);
            }
            None => unlinked.push(game_id),
        }
    }
    if to_dat && !unlinked.is_empty() {
        let cfg = settings::serial_match(pool).await?;
        if !cfg.formats.is_empty() {
            progress.force(0, None, &format!("{name}: serial match for {} game(s) no hash matched", unlinked.len())).await;
            let (linked, _) = crate::database::serial_match::match_games(pool, &unlinked, &cfg, &source_order).await?;
            for &game_id in &linked {
                let releases: Vec<u64> = sqlx::query_scalar("SELECT release_id FROM local_release_games WHERE game_id = ? ORDER BY release_id").bind(game_id).fetch_all(pool).await?;
                for release_id in releases {
                    rescan.linked.push(describe_link(pool, game_id, release_id).await?);
                }
            }
            unlinked.retain(|g| !linked.contains(g));
        }
    }
    crate::database::import::confirm_rom_platforms(pool).await?;
    rescan.unmatched = unlinked.len() as u64;

    // Which DAT releases list these bytes: the file's own, and the other files of its roms.
    let mut files: Vec<u64> = vec![file_id];
    if !roms.is_empty() {
        let list = roms.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        files.extend(sqlx::query_scalar::<_, u64>(sqlx::AssertSqlSafe(format!("SELECT file_id FROM local_rom_files WHERE rom_id IN ({list})"))).fetch_all(pool).await?);
    }
    files.sort_unstable();
    files.dedup();
    let list = files.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let listed: Vec<(u64, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT DISTINCT r.id, r.name FROM local_files f JOIN release_contents rc ON rc.content_id = f.content_id JOIN `release` r ON r.id = rc.release_id AND r.deleted = FALSE WHERE f.id IN ({list}) \
         UNION SELECT DISTINCT r.id, r.name FROM local_files f JOIN content_contents cc ON cc.parent_content_id = f.content_id JOIN release_contents rc ON rc.content_id = cc.child_content_id \
           JOIN `release` r ON r.id = rc.release_id AND r.deleted = FALSE WHERE f.id IN ({list}) ORDER BY 1 LIMIT 50"
    )))
    .fetch_all(pool)
    .await?;
    rescan.listed_by = listed.into_iter().map(|(id, n)| format!("{n} #{id}")).collect();

    let outcome = if !rescan.linked.is_empty() {
        format!("linked {} game(s)", rescan.linked.len())
    } else if !rescan.already_linked.is_empty() && rescan.unmatched == 0 {
        "already linked".to_string()
    } else if rescan.unmatched > 0 {
        format!("{} game(s) not matched by any DAT", rescan.unmatched)
    } else {
        "not in a game".to_string()
    };
    let message = format!("done: {name}: {} rom(s) read, {} file(s) hashed, {outcome}, {} error(s){}", wanted, summary.files_hashed, summary.errors.len(), warnings_note(&summary));
    summary.file_rescan = Some(rescan);
    progress.finish(total_bytes, Some(total_bytes), &message, &job_result(&summary)).await;
    Ok(summary)
}

/// "game #12 → Release (Region) #34 · title Name #56", for a rescan's result.
async fn describe_link(pool: &MySqlPool, game_id: u64, release_id: u64) -> Result<String> {
    let row: Option<(String, Option<u64>, Option<String>)> = sqlx::query_as(
        "SELECT r.name, t.id, t.name FROM `release` r LEFT JOIN title_releases tr ON tr.release_id = r.id LEFT JOIN title t ON t.id = tr.title_id AND t.deleted = FALSE WHERE r.id = ? ORDER BY t.id IS NULL, t.id LIMIT 1",
    )
    .bind(release_id)
    .fetch_optional(pool)
    .await?;
    Ok(match row {
        Some((release, Some(title_id), Some(title))) => format!("game #{game_id} → {release} #{release_id} · title {title} #{title_id}"),
        Some((release, _, _)) => format!("game #{game_id} → {release} #{release_id} · no title"),
        None => format!("game #{game_id} → release #{release_id}, which is not in the catalogue"),
    })
}

/// After a full scan of `directory_id`: its live files the scan did not record (no full
/// check since `started`) that are gone from disk are marked missing. One that is still there
/// but was not recorded (its read failed, and that is in the scan's errors) stays as it was:
/// missing means not on disk, as the integrity check uses it.
///
/// `<=`, not `<`: the stamps are whole seconds, so a file checked by the previous scan in the
/// very second this one started would otherwise never count as unseen. What this scan itself
/// recorded in that second is caught too, and let go again by being on disk.
async fn mark_unseen_missing(pool: &MySqlPool, directory_id: u64, started: chrono::NaiveDateTime) -> Result<u64> {
    let unseen: Vec<(u64, String)> = sqlx::query_as("SELECT id, path FROM local_files WHERE directory_id = ? AND deleted = FALSE AND (last_full_check IS NULL OR last_full_check <= ?)")
        .bind(directory_id)
        .bind(started)
        .fetch_all(pool)
        .await?;
    // Gone only when the system says so: an unreadable path (a permission, a share that
    // dropped for a moment) is not a deleted file.
    let gone: Vec<u64> = unseen.into_iter().filter(|(_, path)| std::fs::metadata(path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)).map(|(id, _)| id).collect();
    for part in gone.chunks(1000) {
        let list = part.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE local_files SET deleted = TRUE WHERE id IN ({list})"))).execute(pool).await?;
    }
    Ok(gone.len() as u64)
}

/// The platforms configured on `dir`'s immediate subfolders, by folder name: the hint each
/// subfolder's files get in a scan of `dir` (a folder's name is the fallback hint).
async fn configured_subfolder_platforms(app: &App, dir: &Directory) -> Result<HashMap<String, Platform>> {
    let rows = db_dirs::list(&app.pool, false).await?;
    Ok(rows
        .iter()
        .filter(|d| std::path::Path::new(&d.path).parent().is_some_and(|parent| db_dirs::same_path(&parent.to_string_lossy(), &dir.path)))
        .filter_map(|d| {
            let platform = Platform::from_igdb_id(d.app_platform_id?)?;
            let name = std::path::Path::new(&d.path).file_name()?.to_string_lossy().into_owned();
            Some((name, platform))
        })
        .collect())
}

fn human_bytes(n: u64) -> String {
    crate::database::jobs::human_bytes(n)
}

/// A candidate's on-disk size for progress: a file's length, a bundle directory's total.
fn candidate_bytes(path: &std::path::Path) -> u64 {
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() => m.len(),
        Ok(m) if m.is_dir() => std::fs::read_dir(path).map(|rd| rd.flatten().map(|e| candidate_bytes(&e.path())).sum()).unwrap_or(0),
        _ => 0,
    }
}

async fn rehash(app: &App, dir: &Directory, forced: Option<Platform>, flags: HashFlags, mut summary: ScanSummary, progress: &crate::database::jobs::Progress) -> Result<ScanSummary> {
    let rows = sqlx::query("SELECT id, path, archive_format, size FROM local_files WHERE directory_id = ? AND deleted = FALSE ORDER BY id").bind(dir.id).fetch_all(&app.pool).await?;
    summary.candidates = rows.len() as u64;
    let n = rows.len();
    let total_bytes: u64 = rows.iter().map(|r| r.try_get::<Option<u64>, _>("size").ok().flatten().unwrap_or(0)).sum();
    let style = super::sheets::style_setting(app).await?;
    let limits = settings::parallel_files(&app.pool, settings::KEY_SCAN_PARALLEL).await?.resolve_for(std::path::PathBuf::from(&dir.path), flags, crate::rom::operation::JobStages::default()).await?;
    let multiple = Multiple::new(limits);
    let _stop_on_drop = CancelOnDrop(multiple.clone());
    let mut jobs = Vec::new();
    for row in rows {
        let id: u64 = row.try_get("id")?;
        let path: String = row.try_get("path")?;
        let archive_format: Option<String> = row.try_get("archive_format")?;
        let p = PathBuf::from(&path);
        let size = match std::fs::metadata(&p) {
            Ok(m) if m.is_file() => m.len(),
            // Unreadable is not gone: a permission or a share hiccup is reported, and the
            // file keeps its row.
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                summary.errors.push(format!("{path}: {e}"));
                continue;
            }
            _ => {
                sqlx::query("UPDATE local_files SET deleted = TRUE WHERE id = ?").bind(id).execute(&app.pool).await?;
                summary.marked_missing += 1;
                continue;
            }
        };
        // Each recorded file on its own: a sheet's tracks are rows of their own here.
        jobs.push((first_folder(Path::new(&dir.path), &p), Job { input: p, args: scan_args(Detect::Verify(forced), flags, style, true), size, tag: (path, archive_format) }));
    }
    // Missing and unreadable files are not read again, so they are not in the count either;
    // the final summary still lists them.
    let wanted = jobs.len();
    let mut running = multiple.run_in_turn(jobs);
    let report = RunningReport::start(&multiple, progress, format!("rehashing {wanted} file(s)"));
    let mut finished = 0usize;
    while let Some(((path, archive_format), result)) = running.next().await {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(&path).to_string();
        // Said before the database writes, so a file slow to record shows as that file.
        report.set(format!("{finished} of {wanted} rehashed"), format!(", recording: {name}"));
        if multiple.is_cancelled() {
            return Err(Error::internal("cancelled"));
        }
        let recorded = async {
            let outcome = result.map_err(|e| Error::internal(e.to_string()))?;
            let platform = outcome.metadata.platform;
            for file in scanned_files(&outcome).map_err(Error::internal)? {
                summary.files_hashed += 1;
                let file_name = file.path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
                let content_id = upsert_content(&app.pool, platform, &file_name, file.hashed.size, &file.hashed.report, &mut summary).await?;
                if let Err(e) = record_members(&app.pool, platform, content_id, &file.members, &mut summary).await {
                    summary.errors.push(format!("{path}: members: {e}"));
                }
                let replaced = content_replaced(&app.pool, &path, content_id).await?;
                let file_id = upsert_local_file(&app.pool, &path, content_id, dir.id, archive_format.as_deref(), file.hashed.size, file.hashed.modified, FileChecks::FULL).await?;
                if replaced {
                    summary.contents_replaced += 1;
                    let roms: Vec<u64> = sqlx::query_scalar("SELECT rom_id FROM local_rom_files WHERE file_id = ?").bind(file_id).fetch_all(&app.pool).await?;
                    // The rom's platform follows its main file's bytes.
                    sqlx::query("UPDATE local_rom SET app_platform_id = ? WHERE main_path = ? AND deleted = FALSE").bind(platform.igdb_id()).bind(&path).execute(&app.pool).await?;
                    summary.replaced_roms.extend(roms);
                }
            }
            Ok::<(), Error>(())
        }
        .await;
        if let Err(e) = recorded {
            summary.errors.push(format!("{path}: {e}"));
        }
        finished += 1;
        report.set(format!("{finished} of {wanted} rehashed"), format!(", last: {name}"));
    }
    report.finish().await;
    link_sheet_tracks(&app.pool, dir.id).await?;
    summary.release_links_dropped = drop_unbacked_release_links(&app.pool, &std::mem::take(&mut summary.replaced_roms)).await?;
    let message = format!("done: {n} file(s), {} rehashed, {} missing, {} error(s){}", summary.files_hashed, summary.marked_missing, summary.errors.len(), warnings_note(&summary));
    progress.finish(total_bytes, Some(total_bytes), &message, &job_result(&summary)).await;
    Ok(summary)
}

/// A cue / gdi / ccd sheet's contents are its tracks: links each sheet rom's sheet content
/// to its other files' (named as the files are), like an archive's members.
async fn link_sheet_tracks(pool: &MySqlPool, directory_id: u64) -> Result<()> {
    let sheets = sqlx::query(
        "SELECT DISTINCT lr.id, lr.main_path FROM local_rom lr JOIN local_rom_files lrf ON lrf.rom_id = lr.id JOIN local_files f ON f.id = lrf.file_id \
         WHERE f.directory_id = ? AND lr.deleted = FALSE AND (lr.main_path LIKE '%.cue' OR lr.main_path LIKE '%.gdi' OR lr.main_path LIKE '%.ccd')",
    )
    .bind(directory_id)
    .fetch_all(pool)
    .await?;
    for sheet in sheets {
        let rom_id: u64 = sheet.try_get("id")?;
        let main_path: String = sheet.try_get("main_path")?;
        let files = sqlx::query("SELECT f.path, f.content_id FROM local_rom_files lrf JOIN local_files f ON f.id = lrf.file_id AND f.deleted = FALSE WHERE lrf.rom_id = ?").bind(rom_id).fetch_all(pool).await?;
        let Some(sheet_content) = files.iter().find(|f| f.try_get::<String, _>("path").is_ok_and(|p| p == main_path)).and_then(|f| f.try_get::<u64, _>("content_id").ok()) else { continue };
        for f in &files {
            let path: String = f.try_get("path")?;
            if path == main_path {
                continue;
            }
            let name = std::path::Path::new(&path).file_name().and_then(|n| n.to_str()).unwrap_or(&path).to_string();
            crate::database::content::link_member(pool, sheet_content, f.try_get("content_id")?, Some(&name)).await?;
        }
    }
    Ok(())
}

/// What the DATs say about one live rom under the directory: the release its files (or
/// their archive members) cover most, the lines covered, and the disc those lines belong to.
#[derive(Debug, Clone)]
struct RomMatch {
    release: u64,
    lines: std::collections::BTreeSet<u64>,
    disc: Option<u8>,
    /// A whole disc / cartridge on its own: a container (CHD, RVZ, ...) or a sheet with its
    /// tracks (cue / gdi / ccd) -- never folded into another rom.
    whole: bool,
    /// Its main file, where `game_groups` looks when copies of a disc set share a directory.
    main_path: String,
}

/// Hasheous-built releases are left out: one of those lists every regional dump of a game as
/// its contents, which are separate ROMs and not the tracks or discs of one. Grouping by them
/// folded seven Hoge Quest dumps (Japan, USA, beta, Virtual Console...) into a single rom.
async fn rom_release_matches(pool: &MySqlPool, scope: &str, progress: &crate::database::jobs::Progress) -> Result<HashMap<u64, RomMatch>> {
    // A thousand roms at a time rather than the whole directory in one statement: on a large
    // archive that one ran for many minutes with nothing to show, and it is what the bar now
    // counts.
    let rom_ids: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT DISTINCT lrf.rom_id FROM local_rom_files lrf JOIN local_rom lr ON lr.id = lrf.rom_id AND lr.deleted = FALSE \
         JOIN local_files f ON f.id = lrf.file_id AND f.deleted = FALSE WHERE f.directory_id IN ({scope}) ORDER BY lrf.rom_id"
    )))
    .fetch_all(pool)
    .await?;
    let n_roms = rom_ids.len() as u64;
    // rom -> release -> (lines, disc numbers seen in their DAT file names)
    type Lines = (std::collections::BTreeSet<u64>, Vec<u8>);
    let mut per_rom: HashMap<u64, (String, HashMap<u64, Lines>)> = HashMap::new();
    for (i, chunk) in rom_ids.chunks(1000).enumerate() {
        progress.report((i * 1000) as u64, Some(n_roms.max(1)), &format!("DAT resolve: finding which DAT lines the files are, {} of {n_roms} rom(s)", i * 1000)).await;
        let roms = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        // The same line reached twice (a file and its member) counts once: `lines` is a set.
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT x.rom_id, x.main_path, x.release_id, x.content_id, x.file_name FROM ( \
               SELECT lrf.rom_id, lr.main_path, rc.release_id, rc.content_id, rc.file_name FROM local_rom_files lrf JOIN local_rom lr ON lr.id = lrf.rom_id AND lr.deleted = FALSE \
                 JOIN local_files f ON f.id = lrf.file_id AND f.deleted = FALSE JOIN release_contents rc ON rc.content_id = f.content_id WHERE lrf.rom_id IN ({roms}) AND f.directory_id IN ({scope}) \
               UNION ALL \
               SELECT lrf.rom_id, lr.main_path, rc.release_id, rc.content_id, rc.file_name FROM local_rom_files lrf JOIN local_rom lr ON lr.id = lrf.rom_id AND lr.deleted = FALSE \
                 JOIN local_files f ON f.id = lrf.file_id AND f.deleted = FALSE JOIN content_contents cc ON cc.parent_content_id = f.content_id \
                 JOIN release_contents rc ON rc.content_id = cc.child_content_id WHERE lrf.rom_id IN ({roms}) AND f.directory_id IN ({scope}) \
             ) x JOIN `release` r ON r.id = x.release_id AND r.deleted = FALSE \
             AND r.source_id NOT IN (SELECT hs.id FROM sources hs WHERE hs.name = 'hasheous')"
        )))
        .fetch_all(pool)
        .await?;
        for r in rows {
            let file_name: String = r.try_get("file_name")?;
            let entry = per_rom.entry(r.try_get("rom_id")?).or_insert_with(|| (r.try_get("main_path").unwrap_or_default(), HashMap::new()));
            let lines = entry.1.entry(r.try_get("release_id")?).or_default();
            if lines.0.insert(r.try_get("content_id")?) {
                if let Some(d) = disc_set_key(&file_name).1 {
                    lines.1.push(d);
                }
            }
        }
    }
    const WHOLE: [&str; 11] = ["chd", "rvz", "wia", "gcz", "cso", "zso", "pbp", "iso", "cue", "gdi", "ccd"];
    Ok(per_rom
        .into_iter()
        .filter_map(|(rom, (main_path, releases))| {
            let ext = std::path::Path::new(&main_path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            let (release, (lines, discs)) = releases.into_iter().max_by(|a, b| a.1 .0.len().cmp(&b.1 .0.len()).then(b.0.cmp(&a.0)))?;
            // The disc most of its lines name.
            let disc = discs.iter().copied().max_by_key(|d| discs.iter().filter(|x| *x == d).count());
            Some((rom, RomMatch { release, lines, disc, whole: WHOLE.contains(&ext.as_str()), main_path }))
        })
        .collect())
}

/// The disc-set key of a release name: the name without its "(Disc N)" group, and N.
pub fn disc_set_key(release_name: &str) -> (String, Option<u8>) {
    lazy_static::lazy_static! {
        static ref RE_DISC: regex::Regex = regex::Regex::new(r"(?i)\s*\((?:Disc|Disk|CD)\s*(\d+)[^)]*\)").unwrap();
    }
    let disc = RE_DISC.captures(release_name).and_then(|c| c.get(1)).and_then(|m| m.as_str().parse().ok());
    // A DAT file name ("... (Disc 3).bin"): without its extension -- only a short
    // alphanumeric one, so "Mr. Ex!" stays whole.
    let stem = match release_name.rsplit_once('.') {
        Some((stem, ext)) if (1..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()) => stem,
        _ => release_name,
    };
    (RE_DISC.replace_all(stem, "").split_whitespace().collect::<Vec<_>>().join(" "), disc)
}

/// The folder `up` levels above a file (1: the one it sits in), on either separator.
fn ancestor(path: &str, up: usize) -> Option<&str> {
    let mut end = path.len();
    for _ in 0..up {
        end = path[..end].rfind(['/', '\\'])?;
    }
    Some(&path[..end])
}

/// Roms (by index) that belong in one game: connected when they share a release, or when
/// their releases are the discs of one set. A group where no (release, disc) appears twice is
/// one game. One that holds copies of a disc -- the same game in two folders of one scanned
/// directory -- is split by where the files sit (the 4th field, the rom's main path): the
/// discs in one folder under one name ("Hoge (USA) (Disc 1/2).chd"), then one folder up (a
/// folder per disc), then any in one folder. Each part is a game when it holds no disc twice;
/// what is left stays as it was.
pub fn game_groups(roms: &[(u64, Option<u8>, Option<String>, Option<String>)]) -> Vec<Vec<usize>> {
    let n = roms.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut Vec<usize>, x: usize) -> usize {
        let mut r = x;
        while p[r] != r {
            r = p[r];
        }
        p[x] = r;
        r
    }
    for i in 0..n {
        for j in (i + 1)..n {
            let same_release = roms[i].0 == roms[j].0;
            let same_set = roms[i].1.is_some() && roms[j].1.is_some() && roms[i].2.is_some() && roms[i].2 == roms[j].2;
            if same_release || same_set {
                let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                parent[a] = b;
            }
        }
    }
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(i);
    }
    let valid = |g: &[usize]| {
        let pairs: std::collections::HashSet<(u64, Option<u8>)> = g.iter().map(|&i| (roms[i].0, roms[i].1)).collect();
        g.len() > 1 && pairs.len() == g.len()
    };
    let mut groups: Vec<Vec<usize>> = groups.into_values().filter(|g| g.len() > 1).collect();
    groups.sort();
    let mut out = Vec::new();
    for group in groups {
        if valid(&group) {
            out.push(group);
            continue;
        }
        let mut left = group;
        // (levels up, by name, by format too): two formats of a set side by side are two games.
        for (up, by_name, by_format) in [(1usize, true, true), (1, true, false), (2, true, true), (2, true, false), (1, false, false)] {
            let mut parts: std::collections::BTreeMap<(&str, String, String), Vec<usize>> = std::collections::BTreeMap::new();
            for &i in &left {
                let Some(path) = roms[i].3.as_deref() else { continue };
                let Some(folder) = ancestor(path, up) else { continue };
                let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
                let name = if by_name { disc_set_key(file).0 } else { String::new() };
                let format = if by_format { file.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default() } else { String::new() };
                parts.entry((folder, name, format)).or_default().push(i);
            }
            let mut taken: std::collections::HashSet<usize> = std::collections::HashSet::new();
            for part in parts.into_values().filter(|p| valid(p)) {
                taken.extend(part.iter().copied());
                out.push(part);
            }
            left.retain(|i| !taken.contains(i));
            if left.len() < 2 {
                break;
            }
        }
    }
    out
}

/// One file a hash match is looking up: the game it belongs to, and the provider ids the
/// lookups have named for it so far (provider, id, links already written).
struct LookupFile {
    game_id: u64,
    content_id: u64,
    platform: Platform,
    refs: Vec<(Provider, String, u32)>,
}

/// What a Hasheous hit does for a file, wherever the answer came from (the cache, the dump
/// index, or a request just made). A hit no DAT covers becomes its own (source `hasheous`)
/// release and title, so the file shows up in the collection and the cross references have
/// something to attach to; a game with no release yet is linked to it.
///
/// Whether the game is linked is read here, at the moment of applying, because replies to
/// concurrent requests arrive in no particular order and two files of one game must not
/// both link it.
async fn apply_hasheous_hit(pool: &MySqlPool, summary: &mut MatchSummary, file: &mut LookupFile, record: cache::HasheousRecord) -> Result<()> {
    if let Some((release_id, _title_id)) = cache::ensure_hasheous_release(pool, file.content_id, &record, file.platform).await? {
        let linked = sqlx::query("SELECT 1 FROM local_release_games WHERE game_id = ? LIMIT 1").bind(file.game_id).fetch_optional(pool).await?.is_some();
        if !linked {
            sqlx::query("INSERT IGNORE INTO local_release_games (release_id, game_id) VALUES (?, ?)").bind(release_id).bind(file.game_id).execute(pool).await?;
            let platform_id: u16 = sqlx::query("SELECT app_platform_id FROM `release` WHERE id = ?").bind(release_id).fetch_one(pool).await?.try_get("app_platform_id")?;
            sqlx::query("UPDATE local_rom SET app_platform_id = ? WHERE app_platform_id = 0 AND id IN (SELECT rom_id FROM local_game_roms WHERE game_id = ?)").bind(platform_id).bind(file.game_id).execute(pool).await?;
            summary.hasheous_matched += 1;
        }
    }
    file.refs.extend(record.cross_refs);
    Ok(())
}

/// [`MatchSummary::unmatched_by_kind`] for `games`: each game counted once, by its first rom's
/// platform and extension; the fifteen biggest groups.
async fn unmatched_kinds(pool: &MySqlPool, games: &[u64], progress: &crate::database::jobs::Progress) -> Result<Vec<(String, u64)>> {
    let mut counts: HashMap<String, u64> = HashMap::new();
    let n = games.len() as u64;
    for (i, chunk) in games.chunks(1000).enumerate() {
        progress.report((i * 1000) as u64, Some(n.max(1)), &format!("sorting the {n} unmatched game(s) by kind: {}", i * 1000)).await;
        let ids = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        let rows: Vec<(u64, String, Option<String>)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT lgr.game_id, lr.main_path, p.name FROM local_game_roms lgr JOIN local_rom lr ON lr.id = lgr.rom_id AND lr.deleted = FALSE \
             LEFT JOIN platforms p ON p.id = lr.app_platform_id WHERE lgr.game_id IN ({ids}) ORDER BY lgr.game_id, lr.id"
        )))
        .fetch_all(pool)
        .await?;
        let mut last: Option<u64> = None;
        for (game, path, platform) in rows {
            if last == Some(game) {
                continue;
            }
            last = Some(game);
            let ext = std::path::Path::new(&path).extension().and_then(|e| e.to_str()).map(|e| format!(".{}", e.to_ascii_lowercase())).unwrap_or_else(|| "(no extension)".to_string());
            *counts.entry(format!("{} · {ext}", platform.unwrap_or_else(|| "unknown platform".to_string()))).or_default() += 1;
        }
    }
    let mut kinds: Vec<(String, u64)> = counts.into_iter().collect();
    kinds.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    kinds.truncate(15);
    Ok(kinds)
}

/// Hash match **all**: clears what this run is about to redo for the directory's games, so a
/// wrong link does not survive just because something is already there. The provider links
/// go first, found through the DAT links they hang off: `hash` ones when a hash service runs,
/// `name`/`fuzzy` ones when PlayMatch runs (its follow-up is the name search that re-makes
/// them); then, with DAT on, the DAT links themselves. Manual and exact links always stay.
/// Provider links are soft-deleted, so a re-made one revives its row.
pub async fn unlink_for_rematch(pool: &MySqlPool, scope: &str, req: &HashMatchRequest) -> Result<()> {
    let games = format!("SELECT DISTINCT lgr.game_id FROM local_game_roms lgr JOIN local_rom_files lrf ON lrf.rom_id = lgr.rom_id JOIN local_files f ON f.id = lrf.file_id WHERE f.directory_id IN ({scope})");
    let mut confidences: Vec<&str> = Vec::new();
    if req.to_hasheous || req.to_playmatch {
        confidences.push("'hash'");
    }
    if req.to_playmatch {
        confidences.extend(["'name'", "'fuzzy'"]);
    }
    if !confidences.is_empty() {
        let list = confidences.join(", ");
        // A title's links only through the game's own releases. A DLC, update, hack or demo
        // file (`APART_SQL`) re-links its own release, never the title, so resetting the
        // title for it lost the title's link for good.
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE title_provider_links tpl JOIN title_releases tr ON tr.title_id = tpl.title_id JOIN `release` r ON r.id = tr.release_id JOIN local_release_games lrg ON lrg.release_id = tr.release_id \
             SET tpl.deleted = TRUE WHERE tpl.deleted = FALSE AND tpl.confidence IN ({list}) AND lrg.game_id IN ({games}) AND NOT {}",
            crate::database::providers::cache::APART_SQL
        )))
        .execute(pool)
        .await?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE release_provider_links rpl JOIN local_release_games lrg ON lrg.release_id = rpl.release_id \
             SET rpl.deleted = TRUE WHERE rpl.deleted = FALSE AND rpl.confidence IN ({list}) AND lrg.game_id IN ({games})"
        )))
        .execute(pool)
        .await?;
    }
    if req.to_dat {
        sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM local_release_games WHERE game_id IN ({games})"))).execute(pool).await?;
    }
    Ok(())
}

/// The contents a hash service can know a file by. A loose file is its own; an archive (a
/// CHD, a zip, a 7z) is known by what is inside it -- Hasheous and PlayMatch index Redump's
/// bins and No-Intro's roms, never our CHD or zip -- so it is asked about by its largest
/// member (the data track, the rom; a sheet or a readme is smaller), and by its own hash
/// only when a DAT lists the archive as is. Asking by the archive's hash alone missed every
/// CHD, and its ids then came from the by-name search instead.
async fn lookup_contents(pool: &MySqlPool, content_id: u64) -> Result<Vec<u64>> {
    let member: Option<u64> = sqlx::query_scalar("SELECT cc.child_content_id FROM content_contents cc JOIN content c ON c.id = cc.child_content_id WHERE cc.parent_content_id = ? ORDER BY c.size DESC, c.id LIMIT 1")
        .bind(content_id)
        .fetch_optional(pool)
        .await?;
    let Some(member) = member else { return Ok(vec![content_id]) };
    let listed = sqlx::query("SELECT 1 FROM release_contents WHERE content_id = ? LIMIT 1").bind(content_id).fetch_optional(pool).await?.is_some();
    Ok(if listed { vec![content_id, member] } else { vec![member] })
}

/// Uses the DATs to put files into roms and roms into games, before matching:
/// 1. loose roms (not a container or sheet: those are whole discs) that cover *different*
///    lines of the same disc of one release -- tracks a scan saw without their sheet --
///    become one rom; copies of the same dump (the same lines) stay apart;
/// 2. roms of one release (a DAT listing a multi-disc game as one release) and roms of the
///    per-disc releases of one "(Disc N)" set become one game, linked to every release;
///    each rom gets its disc number. Two copies of a disc keep their own games.
///
/// Returns (roms merged, games merged).
async fn resolve_by_dat(pool: &MySqlPool, scope: &str, progress: &crate::database::jobs::Progress) -> Result<(u64, u64)> {
    progress.force(0, None, "DAT resolve: finding which DAT lines the files are").await;
    let mut matches = rom_release_matches(pool, scope, progress).await?;
    let mut loose: HashMap<(u64, Option<u8>), Vec<u64>> = HashMap::new();
    for (rom, m) in &matches {
        if !m.whole {
            loose.entry((m.release, m.disc)).or_default().push(*rom);
        }
    }
    let mut roms_merged = 0u64;
    let n_loose = loose.len() as u64;
    for (i, roms) in loose.values_mut().enumerate() {
        progress.report(i as u64, Some(n_loose.max(1)), &format!("DAT resolve: joining loose tracks into discs, {} of {n_loose} disc(s), {roms_merged} joined", i + 1)).await;
        if roms.len() < 2 {
            continue;
        }
        roms.sort_unstable();
        let sets: Vec<&std::collections::BTreeSet<u64>> = roms.iter().map(|r| &matches[r].lines).collect();
        if !sets.iter().enumerate().all(|(i, a)| sets.iter().skip(i + 1).all(|b| a.is_disjoint(b))) {
            continue; // copies of the same dump
        }
        let keep = roms[0];
        let others: Vec<u64> = roms[1..].to_vec();
        let list = others.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        let mut tx = pool.begin().await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("INSERT IGNORE INTO local_rom_files (rom_id, file_id) SELECT {keep}, file_id FROM local_rom_files WHERE rom_id IN ({list})"))).execute(&mut *tx).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM local_rom_files WHERE rom_id IN ({list})"))).execute(&mut *tx).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE local_rom SET deleted = TRUE WHERE id IN ({list})"))).execute(&mut *tx).await?;
        let orphan_games: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT DISTINCT game_id FROM local_game_roms WHERE rom_id IN ({list})"))).fetch_all(&mut *tx).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM local_game_roms WHERE rom_id IN ({list})"))).execute(&mut *tx).await?;
        retire_empty_games(&mut tx, &orphan_games).await?;
        tx.commit().await?;
        roms_merged += others.len() as u64;
        for o in &others {
            matches.remove(o);
        }
    }

    // Disc numbers, and roms → games.
    let release_ids: Vec<u64> = matches.values().map(|m| m.release).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    if release_ids.is_empty() {
        return Ok((roms_merged, 0));
    }
    let list = release_ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let names: HashMap<u64, (String, u16)> = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT id, name, app_platform_id FROM `release` WHERE id IN ({list})")))
        .fetch_all(pool)
        .await?
        .iter()
        .map(|r| Ok((r.try_get("id")?, (r.try_get("name")?, r.try_get("app_platform_id")?))))
        .collect::<std::result::Result<_, sqlx::Error>>()?;
    let mut roms: Vec<(u64, u64, Option<u8>, Option<String>, String)> = Vec::new();
    let mut by_disc: HashMap<u8, Vec<u64>> = HashMap::new();
    for (rom, m) in &matches {
        let (set_key, name_disc) = names.get(&m.release).map(|(n, p)| {
            let (k, d) = disc_set_key(n);
            (Some(format!("{p}|{k}")), d)
        }).unwrap_or((None, None));
        let disc = m.disc.or(name_disc);
        if let Some(d) = disc {
            by_disc.entry(d).or_default().push(*rom);
        }
        roms.push((*rom, m.release, disc, set_key.filter(|_| name_disc.is_some()), m.main_path.clone()));
    }
    // One statement per disc number and thousand roms, not one per rom: a large directory has
    // tens of thousands of them, and each was a round trip with the bar standing still.
    let n_numbered: u64 = by_disc.values().map(|v| v.len() as u64).sum();
    let mut numbered = 0u64;
    for (disc, ids) in &by_disc {
        for chunk in ids.chunks(1000) {
            progress.report(numbered, Some(n_numbered.max(1)), &format!("DAT resolve: disc numbers, {numbered} of {n_numbered} rom(s)")).await;
            let list = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
            sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE local_rom SET disc_number = ? WHERE id IN ({list}) AND (disc_number IS NULL OR disc_number <> ?)"))).bind(disc).bind(disc).execute(pool).await?;
            numbered += chunk.len() as u64;
        }
    }
    roms.sort_by_key(|r| r.0);
    let keyed: Vec<(u64, Option<u8>, Option<String>, Option<String>)> = roms.iter().map(|r| (r.1, r.2, r.3.clone(), Some(r.4.clone()).filter(|p| !p.is_empty()))).collect();
    let mut games_merged = 0u64;
    let groups = game_groups(&keyed);
    let n_groups = groups.len() as u64;
    for (i, group) in groups.into_iter().enumerate() {
        progress.report(i as u64, Some(n_groups.max(1)), &format!("DAT resolve: grouping discs into games, {} of {n_groups}, {games_merged} merged", i + 1)).await;
        let members: Vec<&(u64, u64, Option<u8>, Option<String>, String)> = group.iter().map(|&i| &roms[i]).collect();
        let rom_list = members.iter().map(|m| m.0.to_string()).collect::<Vec<_>>().join(",");
        let games: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT DISTINCT lgr.game_id FROM local_game_roms lgr JOIN local_game lg ON lg.id = lgr.game_id AND lg.deleted = FALSE WHERE lgr.rom_id IN ({rom_list}) ORDER BY lgr.game_id"))).fetch_all(pool).await?;
        let Some(&keep) = games.first() else { continue };
        let mut tx = pool.begin().await?;
        for m in &members {
            sqlx::query("INSERT IGNORE INTO local_game_roms (game_id, rom_id) VALUES (?, ?)").bind(keep).bind(m.0).execute(&mut *tx).await?;
            sqlx::query("INSERT IGNORE INTO local_release_games (release_id, game_id) VALUES (?, ?)").bind(m.1).bind(keep).execute(&mut *tx).await?;
        }
        let others: Vec<u64> = games[1..].to_vec();
        if !others.is_empty() {
            let list = others.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
            sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM local_game_roms WHERE game_id IN ({list}) AND rom_id IN ({rom_list})"))).execute(&mut *tx).await?;
            retire_empty_games(&mut tx, &others).await?;
            games_merged += others.len() as u64;
        }
        tx.commit().await?;
    }
    Ok((roms_merged, games_merged))
}

/// An `.m3u` playlist makes its discs one game: the playlist is hashed and listed as a
/// file, the roms whose main file it lists (in order: disc 1, 2, ...) go into one game --
/// the lowest-id game among theirs -- and that game points at the playlist
/// (`local_game.playlist_file_id`). When no listed entry resolves (discs renamed since the
/// playlist was written), sibling roms in its folder whose name without "(Disc N)" is the
/// playlist's name are taken instead.
async fn apply_playlist(pool: &MySqlPool, dir: &Directory, playlist: &rom_scan::PlaylistGroup, platform: Platform, flags: HashFlags, summary: &mut ScanSummary, step: &Step<'_>) -> Result<()> {
    let path = &playlist.m3u_path;
    let hashed = hash_file(path.clone(), flags, step).await?;
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
    let content_id = upsert_content(pool, platform, &file_name, hashed.size, &hashed.report, summary).await?;
    let file_id = upsert_local_file(pool, &path.display().to_string(), content_id, dir.id, None, hashed.size, hashed.modified, FileChecks::FULL).await?;
    // Its discs, in playlist order.
    let mut roms: Vec<(u64, u16)> = Vec::new();
    for (i, member) in playlist.member_paths.iter().enumerate() {
        let rom: Option<u64> = sqlx::query_scalar("SELECT id FROM local_rom WHERE main_path = ? AND deleted = FALSE ORDER BY id LIMIT 1").bind(member.display().to_string()).fetch_optional(pool).await?;
        if let Some(rom) = rom {
            roms.push((rom, i as u16 + 1));
        }
    }
    if roms.is_empty() {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        let parent = path.parent().map(|p| p.display().to_string()).unwrap_or_default();
        let siblings = sqlx::query("SELECT id, main_path FROM local_rom WHERE main_path LIKE ? AND deleted = FALSE ORDER BY main_path").bind(format!("{}%", parent.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))).fetch_all(pool).await?;
        for r in siblings {
            let main: String = r.try_get("main_path")?;
            let p = std::path::Path::new(&main);
            if p.parent().map(|x| x.display().to_string()) != Some(parent.clone()) {
                continue;
            }
            let rom_stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let (key, disc) = disc_set_key(rom_stem);
            if key.eq_ignore_ascii_case(&stem) {
                roms.push((r.try_get("id")?, u16::from(disc.unwrap_or(0))));
            }
        }
    }
    if roms.is_empty() {
        return Ok(());
    }
    let rom_list = roms.iter().map(|r| r.0.to_string()).collect::<Vec<_>>().join(",");
    let games: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT DISTINCT lgr.game_id FROM local_game_roms lgr JOIN local_game lg ON lg.id = lgr.game_id AND lg.deleted = FALSE WHERE lgr.rom_id IN ({rom_list}) ORDER BY lgr.game_id"))).fetch_all(pool).await?;
    let mut tx = pool.begin().await?;
    let keep = match games.first() {
        Some(&g) => g,
        None => {
            let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("playlist").to_string();
            sqlx::query("INSERT INTO local_game (name) VALUES (?)").bind(name).execute(&mut *tx).await?.last_insert_id()
        }
    };
    for (rom, disc) in &roms {
        sqlx::query("INSERT IGNORE INTO local_game_roms (game_id, rom_id) VALUES (?, ?)").bind(keep).bind(rom).execute(&mut *tx).await?;
        if *disc > 0 {
            sqlx::query("UPDATE local_rom SET disc_number = ? WHERE id = ?").bind(disc).bind(rom).execute(&mut *tx).await?;
        }
    }
    let others: Vec<u64> = games.iter().copied().filter(|g| *g != keep).collect();
    if !others.is_empty() {
        let list = others.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        // Their release links carry over to the playlist's game.
        sqlx::query(sqlx::AssertSqlSafe(format!("INSERT IGNORE INTO local_release_games (release_id, game_id) SELECT release_id, {keep} FROM local_release_games WHERE game_id IN ({list})"))).execute(&mut *tx).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM local_game_roms WHERE game_id IN ({list}) AND rom_id IN ({rom_list})"))).execute(&mut *tx).await?;
        retire_empty_games(&mut tx, &others).await?;
    }
    sqlx::query("UPDATE local_game SET playlist_file_id = ? WHERE id = ?").bind(file_id).bind(keep).execute(&mut *tx).await?;
    tx.commit().await?;
    summary.playlists_grouped += 1;
    Ok(())
}

/// Soft-deletes games left without roms (and drops their release links).
async fn retire_empty_games(tx: &mut sqlx::Transaction<'_, sqlx::MySql>, games: &[u64]) -> Result<()> {
    if games.is_empty() {
        return Ok(());
    }
    let list = games.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let empty = format!("id IN ({list}) AND NOT EXISTS (SELECT 1 FROM local_game_roms x WHERE x.game_id = local_game.id)");
    sqlx::query(sqlx::AssertSqlSafe(format!("DELETE lrg FROM local_release_games lrg JOIN local_game ON local_game.id = lrg.game_id WHERE local_game.{empty}"))).execute(&mut **tx).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE local_game SET deleted = TRUE WHERE {empty}"))).execute(&mut **tx).await?;
    Ok(())
}

#[cfg(test)]
mod progress_tests {
    use super::algorithms;
    use crate::rom::hash::HashFlags;

    #[test]
    fn algorithm_labels_list_what_is_actually_being_computed() {
        assert_eq!(algorithms(HashFlags::CRC32 | HashFlags::SHA1), "crc32+sha1");
        // Order is the fixed display order, not the order the bits were written in.
        assert_eq!(algorithms(HashFlags::SHA1 | HashFlags::CRC32), "crc32+sha1");
        assert_eq!(algorithms(HashFlags::RAHASH), "rahash");
    }

    #[test]
    fn an_empty_flag_set_reports_the_defaults_that_will_actually_run() {
        // `HashDigest::new` substitutes the defaults for an empty set, so a label saying
        // "nothing" would contradict what the scan then does.
        assert_eq!(algorithms(HashFlags::empty()), algorithms(HashFlags::default()));
        assert!(!algorithms(HashFlags::empty()).is_empty());
    }
}

#[cfg(test)]
mod dat_resolve_tests {
    use super::{ancestor, disc_set_key, game_groups};

    /// A zipped NES cartridge gets the RA hash of the rom inside, header skipped.
    #[test]
    fn ra_hash_reads_a_zipped_cartridge() {
        use crate::rom::sevenzip_wrapper::{create_archive, ArchiveFormat, CompressionMethod, CreateArchiveArgs, CreateEntry};
        use md5::{Digest, Md5};
        let dir = std::env::temp_dir().join(format!("romexplorer-ra-zip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let body: Vec<u8> = (0..16 * 1024).map(|i| (i % 241) as u8).collect();
        let mut rom = b"NES\x1a\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00".to_vec();
        rom.extend_from_slice(&body);
        std::fs::write(dir.join("Game.nes"), &rom).unwrap();
        let zip = dir.join("Game.zip");
        let args = CreateArchiveArgs { format: ArchiveFormat::Zip, method: CompressionMethod::Deflate, level: 5, entries: vec![CreateEntry { input_path: dir.join("Game.nes"), archive_name: "Game.nes".into() }] };
        if create_archive(&zip, &args, |_, _| true).is_err() {
            eprintln!("no 7z build -- skipping");
            return;
        }
        match super::ra_hash_of(&zip, 7) {
            Err(crate::rom::hash::RahashError::Unavailable(_)) => eprintln!("no RAHasher -- skipping"),
            other => {
                let expected: String = Md5::digest(&body).iter().map(|b| format!("{b:02x}")).collect();
                assert_eq!(other, Ok(crate::rom::hash::RahashOutcome::Done(expected)));
            }
        }
        // A disc system in an archive is not unpacked.
        assert!(super::ra_hash_of(&zip, 12).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn disc_sets() {
        assert_eq!(disc_set_key("Hoge Quest VII (USA) (Disc 2)"), ("Hoge Quest VII (USA)".to_string(), Some(2)));
        assert_eq!(disc_set_key("Game (Japan) (Disc 1) (Rev 1)"), ("Game (Japan) (Rev 1)".to_string(), Some(1)));
        assert_eq!(disc_set_key("Hoge Quest VII (USA) (Disc 3).bin").1, Some(3), "DAT file names too");
        assert_eq!(disc_set_key("Game (USA)"), ("Game (USA)".to_string(), None));
        assert_eq!(disc_set_key("Mr. Ex! (USA)").0, "Mr. Ex! (USA)");
        assert_eq!(disc_set_key("Dr. Sample (Japan, USA) (Rev 1)").0, "Dr. Sample (Japan, USA) (Rev 1)");
    }

    #[test]
    fn games_from_releases_and_disc_sets() {
        let k = |s: &str| Some(s.to_string());
        // One release listing three discs -> one game.
        assert_eq!(game_groups(&[(7, Some(1), None, None), (7, Some(2), None, None), (7, Some(3), None, None)]).len(), 1);
        // Per-disc releases of one set -> one game.
        assert_eq!(game_groups(&[(1, Some(1), k("p|Game (USA)"), None), (2, Some(2), k("p|Game (USA)"), None)]).len(), 1);
        // Two copies of one single-disc release -> kept apart.
        assert!(game_groups(&[(5, None, None, None), (5, None, None, None)]).is_empty());
        // Two copies of disc 1 plus a disc 2, nowhere known -> ambiguous, kept apart.
        assert!(game_groups(&[(1, Some(1), k("p|G"), None), (1, Some(1), k("p|G"), None), (2, Some(2), k("p|G"), None)]).is_empty());
        // Different games -> separate.
        assert!(game_groups(&[(1, None, None, None), (2, None, None, None)]).is_empty());
    }

    /// The same two-disc release in two folders of one scanned directory: one game per
    /// folder, not four games of one disc.
    #[test]
    fn copies_of_a_disc_set_in_two_folders_are_a_game_each() {
        let at = |p: &str| Some(p.to_string());
        let roms = [
            (9, Some(1), None, at("/roms/PS2 - RPG/Hoge (Japan) (Disc 1).chd")),
            (9, Some(2), None, at("/roms/PS2 - RPG/Hoge (Japan) (Disc 2).chd")),
            (9, Some(1), None, at("/roms/PS2 -Japan-/Hoge (Japan) (Disc 1).chd")),
            (9, Some(2), None, at("/roms/PS2 -Japan-/Hoge (Japan) (Disc 2).chd")),
        ];
        let mut groups = game_groups(&roms);
        groups.sort();
        assert_eq!(groups, vec![vec![0, 1], vec![2, 3]]);
        // A folder per disc: joined one level up.
        let roms = [
            (9, Some(1), None, at("/roms/a/Hoge (Disc 1)/Hoge (Disc 1).cue")),
            (9, Some(2), None, at("/roms/a/Hoge (Disc 2)/Hoge (Disc 2).cue")),
            (9, Some(1), None, at("/roms/b/Hoge (Disc 1)/Hoge (Disc 1).cue")),
            (9, Some(2), None, at("/roms/b/Hoge (Disc 2)/Hoge (Disc 2).cue")),
        ];
        let mut groups = game_groups(&roms);
        groups.sort();
        assert_eq!(groups, vec![vec![0, 1], vec![2, 3]]);
        // Two revisions side by side: paired by name.
        let roms = [
            (9, Some(1), None, at("/roms/Hoge (Japan) (Disc 1).chd")),
            (9, Some(1), None, at("/roms/Hoge (Japan) (Disc 1) (Rev 1).chd")),
            (9, Some(2), None, at("/roms/Hoge (Japan) (Disc 2).chd")),
            (9, Some(2), None, at("/roms/Hoge (Japan) (Disc 2) (Rev 1).chd")),
        ];
        let mut groups = game_groups(&roms);
        groups.sort();
        assert_eq!(groups, vec![vec![0, 2], vec![1, 3]]);
        // Two formats of the set side by side: a game each.
        let roms = [
            (9, Some(1), None, at("/roms/Hoge (Disc 1).chd")),
            (9, Some(1), None, at("/roms/Hoge (Disc 1).iso")),
            (9, Some(2), None, at("/roms/Hoge (Disc 2).chd")),
            (9, Some(2), None, at("/roms/Hoge (Disc 2).iso")),
        ];
        let mut groups = game_groups(&roms);
        groups.sort();
        assert_eq!(groups, vec![vec![0, 2], vec![1, 3]]);
        // Two names for one release side by side, discs as releases of their own (a set),
        // spaced or not, in either format: a game per name.
        let set = || Some("12|Game".to_string());
        for (a, b) in [("chd", "iso"), ("chd", "chd")] {
            let roms = [
                (11, Some(1), set(), at("/roms/PS2(Disc 1).chd")),
                (12, Some(2), set(), at("/roms/PS2(Disc 2).chd")),
                (11, Some(1), set(), at(&format!("/roms/PS22 (Disc 1).{a}"))),
                (12, Some(2), set(), at(&format!("/roms/PS22 (Disc 2).{b}"))),
            ];
            let mut groups = game_groups(&roms);
            groups.sort();
            assert_eq!(groups, vec![vec![0, 1], vec![2, 3]], "{a}/{b}");
        }
        // A lone extra copy of disc 1 stays a game of its own.
        let roms = [(9, Some(1), None, at("/roms/x/Hoge (Disc 1).chd")), (9, Some(2), None, at("/roms/x/Hoge (Disc 2).chd")), (9, Some(1), None, at("/roms/y/Hoge (Disc 1).chd"))];
        assert_eq!(game_groups(&roms), vec![vec![0, 1]]);
        assert_eq!(ancestor("C:\\roms\\x\\a.chd", 2), Some("C:\\roms"));
    }
}

/// The query [`best_release`] runs: the release whose DAT lines cover the most of one game's
/// files. A tie on hits (a TOSEC release and a No-Intro one listing the same files under
/// different names) goes to the source ranked first in Settings > metadata source priority >
/// release name, not to whichever DAT happened to be imported first.
fn best_release_sql(source_order: &[String]) -> String {
    let rank_sql = format!(
        "(SELECT COALESCE(MIN(NULLIF(FIELD(s.name, {}), 0)), 255) FROM release_sources rs JOIN sources s ON s.id = rs.source_id WHERE rs.release_id = hits.release_id)",
        vec!["?"; source_order.len().max(1)].join(", ")
    );
    format!(
        "SELECT release_id, MAX(platform_id) AS platform_id, COUNT(DISTINCT content_id) AS hits FROM ( \
           SELECT rc.release_id, r.app_platform_id AS platform_id, rc.content_id FROM local_game_roms lgr JOIN local_rom_files lrf ON lrf.rom_id = lgr.rom_id \
             JOIN local_files f ON f.id = lrf.file_id JOIN release_contents rc ON rc.content_id = f.content_id JOIN `release` r ON r.id = rc.release_id AND r.deleted = FALSE WHERE lgr.game_id = ? \
           UNION ALL \
           SELECT rc.release_id, r.app_platform_id, rc.content_id FROM local_game_roms lgr JOIN local_rom_files lrf ON lrf.rom_id = lgr.rom_id \
             JOIN local_files f ON f.id = lrf.file_id JOIN content_contents cc ON cc.parent_content_id = f.content_id \
             JOIN release_contents rc ON rc.content_id = cc.child_content_id JOIN `release` r ON r.id = rc.release_id AND r.deleted = FALSE WHERE lgr.game_id = ? \
         ) hits GROUP BY release_id ORDER BY hits DESC, {rank_sql}, release_id LIMIT 1"
    )
}

/// The release a game's files match best by hash, and its platform. A file matches a DAT line
/// by its own hash (a zip / CHD a DAT lists as is) or by an archive member's (a CHD's tracks
/// against Redump's bins). `sql` is [`best_release_sql`] for `source_order`.
async fn best_release(pool: &MySqlPool, sql: &str, source_order: &[String], game_id: u64) -> Result<Option<(u64, u16)>> {
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(game_id).bind(game_id);
    if source_order.is_empty() {
        query = query.bind("");
    }
    for source in source_order {
        query = query.bind(source.as_str());
    }
    let Some(row) = query.fetch_optional(pool).await? else { return Ok(None) };
    Ok(Some((row.try_get("release_id")?, row.try_get("platform_id")?)))
}

/// Links a game to a release a hash matched, and gives its roms with no platform yet the
/// release's.
async fn link_game(pool: &MySqlPool, game_id: u64, release_id: u64, platform_id: u16) -> Result<()> {
    sqlx::query("INSERT IGNORE INTO local_release_games (release_id, game_id) VALUES (?, ?)").bind(release_id).bind(game_id).execute(pool).await?;
    sqlx::query("UPDATE local_rom SET app_platform_id = ? WHERE app_platform_id = 0 AND id IN (SELECT rom_id FROM local_game_roms WHERE game_id = ?)").bind(platform_id).bind(game_id).execute(pool).await?;
    Ok(())
}

/// Hash match, in order: (1) `to_dat` links each game under the directory to the release
/// whose `release_contents` cover the most of its files -- possible because a scanned file
/// that hashes like a DAT line reuses the DAT's own `content` row (`upsert_content`);
/// (2) `to_hasheous` looks every still-unlinked game's content up on Hasheous (single-hash
/// API, so one request per content, serial in the `provider:hasheous` lane), skipping
/// content already looked up within `cache_ttl_days`, and `to_playmatch` asks PlayMatch;
/// (3) with either of the two on, the cross-reference ids the services reported become
/// **one** `provider_query` per provider carrying the whole id list (that provider's lane
/// fetches them in list requests), and every searchable provider gets a by-name job for the
/// matched titles still without a link to it.
pub async fn run_hash_match(app: &App, req: &HashMatchRequest, progress: &crate::database::jobs::Progress) -> Result<MatchSummary> {
    use crate::database::jobs as db_jobs;
    use crate::database::providers::raw_store::RawStore;
    use crate::database::providers::{cache, Provider};
    use crate::database::settings;

    let (dir, _) = load_directory(app, req.directory_id).await?;
    let pool = &app.pool;
    let scope = directory_scope(pool, &dir).await?;
    if req.all {
        progress.force(0, None, "Hash match all: removing the links this run will make again").await;
        unlink_for_rematch(pool, &scope, req).await?;
    }
    let (roms_merged, games_merged) = if req.to_dat {
        resolve_by_dat(pool, &scope, progress).await?
    } else {
        (0, 0)
    };
    // Roms whose RetroAchievements hash the scan worked out, to the RA game with that hash.
    let ra_rom_links = cache::link_ra_rom_hashes(pool, Some(&scope), None).await?;
    // `linked` comes back with the row rather than from a `SELECT 1` per game: it is what
    // decides whether a game is work at all, so knowing it up front is what lets the progress
    // count the games that actually need matching instead of counting every game and racing
    // through the ones it skips. It also removes one round trip per game.
    let games = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT DISTINCT lg.id, EXISTS (SELECT 1 FROM local_release_games lrg WHERE lrg.game_id = lg.id) AS linked \
         FROM local_game lg JOIN local_game_roms lgr ON lgr.game_id = lg.id JOIN local_rom_files lrf ON lrf.rom_id = lgr.rom_id \
         JOIN local_files f ON f.id = lrf.file_id WHERE f.directory_id IN ({scope}) AND lg.deleted = FALSE"
    )))
    .fetch_all(pool)
    .await?;
    let mut summary = MatchSummary { games: games.len() as u64, roms_merged, games_merged, ra_rom_links, ..Default::default() };
    let all_games: Vec<u64> = games.iter().filter_map(|r| r.try_get::<u64, _>("id").ok()).collect();
    let mut unlinked: Vec<u64> = Vec::new();
    let n_games = games.len() as u64;
    let mut todo: Vec<u64> = Vec::new();
    for row in &games {
        if row.try_get::<i64, _>("linked").unwrap_or(0) != 0 {
            summary.already_linked += 1;
        } else {
            todo.push(row.try_get("id")?);
        }
    }
    let n_todo = todo.len() as u64;
    let source_order = settings::release_source_priority(pool).await?;
    let best_sql = best_release_sql(&source_order);
    let linked_note = if summary.already_linked > 0 { format!(", {} already linked", summary.already_linked) } else { String::new() };
    progress.force(0, Some(n_todo.max(1)), &format!("DAT match: {n_todo} of {n_games} game(s) need a match{linked_note}")).await;
    for (i, game_id) in todo.into_iter().enumerate() {
        progress
            .report(i as u64, Some(n_todo.max(1)), &format!("DAT match: {} of {n_todo} to match, {} matched{linked_note}", i + 1, summary.matched))
            .await;
        if !req.to_dat {
            unlinked.push(game_id);
            continue;
        }
        match best_release(pool, &best_sql, &source_order, game_id).await? {
            Some((release_id, platform_id)) => {
                link_game(pool, game_id, release_id, platform_id).await?;
                summary.matched += 1;
            }
            None => unlinked.push(game_id),
        }
    }
    // Lenient matching: what no hash matched, by serial, for the formats the setting allows.
    if req.to_dat && !unlinked.is_empty() {
        let cfg = settings::serial_match(pool).await?;
        if !cfg.formats.is_empty() {
            progress.force(0, None, &format!("serial match: {} unmatched game(s)", unlinked.len())).await;
            let (linked, outcome) = crate::database::serial_match::match_games(pool, &unlinked, &cfg, &source_order).await?;
            unlinked.retain(|g| !linked.contains(g));
            summary.matched += outcome.matched;
            summary.serial_matched = outcome.matched;
            summary.serial_ambiguous = outcome.ambiguous;
        }
    }
    // Games linked before this loop (a multi-disc game joined from its discs by the DAT
    // resolve) were skipped above as linked, and kept the platform the scan guessed: none.
    crate::database::import::confirm_rom_platforms(pool).await?;
    summary.unmatched = unlinked.len() as u64;
    summary.unmatched_by_kind = unmatched_kinds(pool, &unlinked, progress).await?;

    // The hash-lookup services also serve games a DAT already identified: their cross
    // references are how those get their IGDB/RA/... ids. Take every game in the directory
    // whose release (or title) has no provider link yet -- and, with Hasheous on, every game
    // holding a file Hasheous has not been asked about within the cache TTL. A link from
    // somewhere else (a TheGamesDB name match, a bulk sync) is not a reason to skip the
    // lookup: on the live catalogue it left 2013 of 7533 games never asked. Keyed on the
    // lookup rather than on the links so a re-run asks nothing twice.
    let use_hash_services = req.to_hasheous || req.to_playmatch;
    let mut targets: Vec<u64> = unlinked.clone();
    let mut targeted: std::collections::HashSet<u64> = targets.iter().copied().collect();
    if use_hash_services {
        progress.force(0, None, &format!("finding the games to look up ({} matched by DAT, {} not)", summary.matched, summary.unmatched)).await;
        let ttl = settings::cache_ttl_days(pool).await?;
        // Two hundred and fifty games at a time, each batch a handful of plain lookups by
        // key, decided here: as one statement with a correlated subquery per file (and per
        // archive member) a thousand games took minutes, and the count stood still.
        let n_all = all_games.len() as u64;
        const STEP: usize = 250;
        for (i, chunk) in all_games.chunks(STEP).enumerate() {
            progress.report((i * STEP) as u64, Some(n_all.max(1)), &format!("finding the games to look up: {} of {n_all} game(s), {} so far", i * STEP, targets.len())).await;
            let ids = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
            // The batch's DAT-linked games, their releases, and their files here.
            let rows: Vec<(u64, u64, u64, bool)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "SELECT DISTINCT lg.id, lrg.release_id, f.content_id, f.deleted FROM local_game lg JOIN local_release_games lrg ON lrg.game_id = lg.id \
                 JOIN local_game_roms lgr ON lgr.game_id = lg.id JOIN local_rom_files lrf ON lrf.rom_id = lgr.rom_id JOIN local_files f ON f.id = lrf.file_id \
                 WHERE lg.id IN ({ids}) AND f.directory_id IN ({scope}) AND lg.deleted = FALSE"
            )))
            .fetch_all(pool)
            .await?;
            if rows.is_empty() {
                continue;
            }
            let in_list = |v: &mut dyn Iterator<Item = u64>| {
                let mut v: Vec<u64> = v.collect();
                v.sort_unstable();
                v.dedup();
                v.iter().map(u64::to_string).collect::<Vec<_>>().join(",")
            };
            let releases = in_list(&mut rows.iter().map(|r| r.1));
            // Releases with any provider link, on themselves or on their title.
            let linked: std::collections::HashSet<u64> = sqlx::query_scalar::<_, u64>(sqlx::AssertSqlSafe(format!(
                "SELECT release_id FROM release_provider_links WHERE deleted = FALSE AND release_id IN ({releases}) \
                 UNION SELECT tr.release_id FROM title_releases tr JOIN title_provider_links tpl ON tpl.title_id = tr.title_id AND tpl.deleted = FALSE WHERE tr.release_id IN ({releases})"
            )))
            .fetch_all(pool)
            .await?
            .into_iter()
            .collect();
            // Releases whose title has an IGDB id better than a name guess: PlayMatch is asked
            // only while that is missing.
            let igdb_known: std::collections::HashSet<u64> = if req.to_playmatch {
                sqlx::query_scalar::<_, u64>(sqlx::AssertSqlSafe(format!(
                    "SELECT DISTINCT tr.release_id FROM title_releases tr JOIN title_provider_links tpl ON tpl.title_id = tr.title_id \
                     AND tpl.provider = 'igdb' AND tpl.deleted = FALSE AND tpl.confidence IN ('exact', 'hash', 'manual') WHERE tr.release_id IN ({releases})"
                )))
                .fetch_all(pool)
                .await?
                .into_iter()
                .collect()
            } else {
                Default::default()
            };
            // Each live file's contents as the services know them: itself and its members.
            let files = in_list(&mut rows.iter().filter(|r| !r.3).map(|r| r.2));
            let mut members: HashMap<u64, Vec<u64>> = HashMap::new();
            if !files.is_empty() {
                for (parent, child) in sqlx::query_as::<_, (u64, u64)>(sqlx::AssertSqlSafe(format!("SELECT parent_content_id, child_content_id FROM content_contents WHERE parent_content_id IN ({files})"))).fetch_all(pool).await? {
                    members.entry(parent).or_default().push(child);
                }
            }
            let asked = |table: &'static str| {
                let all = in_list(&mut rows.iter().filter(|r| !r.3).flat_map(|r| std::iter::once(r.2).chain(members.get(&r.2).cloned().unwrap_or_default())));
                async move {
                    if all.is_empty() {
                        return Ok::<std::collections::HashSet<u64>, sqlx::Error>(Default::default());
                    }
                    Ok(sqlx::query_scalar::<_, u64>(sqlx::AssertSqlSafe(format!("SELECT content_id FROM {table} WHERE deleted = FALSE AND fetched_at > NOW() - INTERVAL {ttl} DAY AND content_id IN ({all})")))
                        .fetch_all(pool)
                        .await?
                        .into_iter()
                        .collect())
                }
            };
            let hasheous_asked = if req.to_hasheous { asked("hasheous_lookups").await? } else { Default::default() };
            let playmatch_asked = if req.to_playmatch { asked("playmatch_lookups").await? } else { Default::default() };
            let never_asked = |set: &std::collections::HashSet<u64>, content: u64| !set.contains(&content) && !members.get(&content).is_some_and(|m| m.iter().any(|c| set.contains(c)));
            for (game, release, content, deleted) in &rows {
                if targeted.contains(game) {
                    continue;
                }
                let wanted = req.all
                    || !linked.contains(release)
                    || (req.to_hasheous && !deleted && never_asked(&hasheous_asked, *content))
                    || (req.to_playmatch && !deleted && !igdb_known.contains(release) && never_asked(&playmatch_asked, *content));
                if wanted {
                    targeted.insert(*game);
                    targets.push(*game);
                    summary.hasheous_for_linked += 1;
                }
            }
        }
    }

    if use_hash_services && !targets.is_empty() {
        let hasheous_on = req.to_hasheous && app.clients().is_available(Provider::Hasheous);
        let playmatch_on = req.to_playmatch && app.clients().is_available(Provider::PlayMatch);
        if req.to_hasheous && !hasheous_on {
            summary.hasheous_skipped = true;
            log::warn!("hash_match: hasheous client unavailable, skipping");
        }
        if req.to_playmatch && !playmatch_on {
            summary.playmatch_skipped = true;
            log::warn!("hash_match: playmatch client unavailable, skipping");
        }
        let store = RawStore::new(app.paths.provider_cache_dir(), settings::raw_retention(pool).await?);
        let ttl_days = settings::cache_ttl_days(pool).await?;
        // Hasheous' published dumps, once a bulk sync has downloaded them, are indexed by
        // hash in the database. Answer what they can answer before anything goes to the
        // network: the loop below then finds those files cached and asks Hasheous only for
        // what the dumps do not have.
        if hasheous_on {
            use crate::database::providers::dumps;
            if let Err(e) = dumps::ensure_hasheous_index(pool, &store, progress).await {
                log::warn!("hash_match: indexing the downloaded Hasheous dumps failed: {e}");
            }
            progress.force(0, None, "Hasheous: answering from the downloaded dumps").await;
            match dumps::match_hasheous_index(pool, &store, &scope, progress).await {
                Ok(n) => log::info!("hash_match: {n} file(s) answered from the downloaded Hasheous dumps"),
                Err(e) => log::warn!("hash_match: matching against the downloaded Hasheous dumps failed: {e}"),
            }
        }
        let mut cross_refs: HashMap<Provider, Vec<String>> = HashMap::new();

        // The lookups run as passes over the whole set rather than file by file. One loop
        // that asked the cache, then Hasheous, then PlayMatch for each file in turn waited
        // out every request before it could reach the next file -- including files further
        // on that needed no request at all. On the test collection that was about 0.67 s a
        // file, 75 minutes for 6,826 games. So: everything the database can answer first;
        // then what is left goes to Hasheous several at a time; then what is still
        // unidentified goes to PlayMatch a hundred to a request.
        let mut files: Vec<LookupFile> = Vec::new();
        {
            let mut seen_content: std::collections::HashSet<u64> = std::collections::HashSet::new();
            let n_targets = targets.len() as u64;
            for (i, game_id) in targets.iter().enumerate() {
                progress.report(i as u64, Some(n_targets.max(1)), &format!("collecting the files to look up: game {} of {n_targets}, {} file(s)", i + 1, files.len())).await;
                let contents = sqlx::query("SELECT DISTINCT f.content_id, lr.app_platform_id FROM local_game_roms lgr JOIN local_rom lr ON lr.id = lgr.rom_id JOIN local_rom_files lrf ON lrf.rom_id = lgr.rom_id JOIN local_files f ON f.id = lrf.file_id WHERE lgr.game_id = ? AND f.deleted = FALSE").bind(game_id).fetch_all(pool).await?;
                for c in contents {
                    let platform = c.try_get::<u16, _>("app_platform_id").ok().and_then(Platform::from_igdb_id).unwrap_or(Platform::Unknown);
                    for content_id in lookup_contents(pool, c.try_get("content_id")?).await? {
                        if seen_content.insert(content_id) {
                            files.push(LookupFile { game_id: *game_id, content_id, platform, refs: Vec::new() });
                        }
                    }
                }
            }
        }
        let n_files = files.len() as u64;

        // --- Pass 1: Hasheous answers already in the database. No network.
        let mut pending: Vec<(usize, playmatch_deredis::providers::hasheous::model::HashLookupRequest)> = Vec::new();
        if hasheous_on {
            for i in 0..files.len() {
                progress.report(i as u64, Some(n_files), &format!("lookups: file {} of {n_files} ({} answered from cache, {} to ask Hasheous)", i + 1, summary.hasheous_cached, pending.len())).await;
                let content_id = files[i].content_id;
                let fresh = sqlx::query("SELECT hit FROM hasheous_lookups WHERE content_id = ? AND deleted = FALSE AND fetched_at > NOW() - INTERVAL ? DAY AND (hit OR NOT ?)").bind(content_id).bind(ttl_days).bind(req.all).fetch_optional(pool).await?;
                match fresh {
                    Some(row) => {
                        summary.hasheous_cached += 1;
                        if row.try_get::<bool, _>("hit").unwrap_or(false) {
                            summary.hasheous_hits += 1;
                            if let Some(record) = cache::hasheous_record_from_cache(pool, content_id).await?.filter(|r| r.hit) {
                                apply_hasheous_hit(pool, &mut summary, &mut files[i], record).await?;
                            }
                        }
                    }
                    None => {
                        let request = cache::hasheous_request_for_content(pool, content_id).await?;
                        if request.md5.is_some() || request.sha1.is_some() || request.sha256.is_some() || request.crc.is_some() {
                            pending.push((i, request));
                        }
                    }
                }
            }
        }

        // --- Pass 2: what the database could not answer, asked of Hasheous with a few
        // requests in flight at once. The limit is the client's own (4 a second, see
        // `providers::hasheous`): its limiter admits that many, and a loop that awaited each
        // reply before sending the next reached barely a third of it. Replies are applied
        // here, one at a time, as they arrive.
        if !pending.is_empty() {
            const IN_FLIGHT: usize = 4;
            let asked = pending.len() as u64;
            progress.force(0, Some(asked), &format!("Hasheous: {} answered from cache, asking for {asked}", summary.hasheous_cached)).await;
            let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(IN_FLIGHT));
            let mut requests: tokio::task::JoinSet<(usize, std::result::Result<cache::HasheousRecord, String>)> = tokio::task::JoinSet::new();
            for (index, request) in pending {
                let (clients, pool, store, semaphore, content_id) = (app.clients(), pool.clone(), store.clone(), semaphore.clone(), files[index].content_id);
                requests.spawn(async move {
                    let _permit = semaphore.acquire_owned().await;
                    (index, cache::hasheous_lookup(&clients, &pool, &store, content_id, &request).await.map_err(|e| e.to_string()))
                });
            }
            while let Some(joined) = requests.join_next().await {
                let (index, result) = joined.map_err(|e| Error::internal(format!("hasheous lookup task: {e}")))?;
                summary.hasheous_lookups += 1;
                match result {
                    Ok(record) if record.hit => {
                        summary.hasheous_hits += 1;
                        apply_hasheous_hit(pool, &mut summary, &mut files[index], record).await?;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        summary.hasheous_errors += 1;
                        log::warn!("hash_match: hasheous lookup for content {} failed: {e}", files[index].content_id);
                    }
                }
                progress.report(summary.hasheous_lookups, Some(asked), &format!("Hasheous: {} of {asked} asked, {} hits ({} answered from cache)", summary.hasheous_lookups, summary.hasheous_hits, summary.hasheous_cached)).await;
            }
        }

        // --- Local identity, then PlayMatch for what is still unidentified. Ids our own
        // tables know by exact hash fill in without a request; PlayMatch is a second opinion
        // on the ids (it often has IGDB where Hasheous reports none) and is asked only while
        // IGDB is still unknown.
        let mut to_playmatch: Vec<(usize, crate::database::providers::playmatch::IdentifyRequest)> = Vec::new();
        for i in 0..files.len() {
            progress.report(i as u64, Some(n_files.max(1)), &format!("ids our own tables know: file {} of {n_files}, {} filled", i + 1, summary.local_links_filled)).await;
            let content_id = files[i].content_id;
            let local = cache::identify_local(pool, content_id).await?;
            summary.local_links_filled += local.filled;
            let known_locally = local.exact && local.igdb_known;
            if !playmatch_on {
                continue;
            }
            if known_locally {
                summary.playmatch_local_skips += 1;
                continue;
            }
            let fresh = sqlx::query("SELECT hit FROM playmatch_lookups WHERE content_id = ? AND deleted = FALSE AND fetched_at > NOW() - INTERVAL ? DAY AND (hit OR NOT ?)").bind(content_id).bind(ttl_days).bind(req.all).fetch_optional(pool).await?;
            match fresh {
                Some(row) => {
                    summary.playmatch_cached += 1;
                    if row.try_get::<bool, _>("hit").unwrap_or(false) {
                        summary.playmatch_hits += 1;
                        if let Some(record) = cache::playmatch_record_from_cache(pool, content_id).await?.filter(|r| r.hit) {
                            files[i].refs.extend(record.cross_refs);
                        }
                    }
                }
                None => {
                    let request = cache::playmatch_request_for_content(pool, content_id).await?;
                    if request.has_hash() {
                        to_playmatch.push((i, request));
                    }
                }
            }
        }

        // --- Pass 3: PlayMatch, a hundred files to a request where it has the file's name
        // and size (its bulk call needs both), one at a time for the few it does not.
        if !to_playmatch.is_empty() {
            let clients = app.clients();
            let (bulk, single): (Vec<_>, Vec<_>) = to_playmatch.into_iter().partition(|(_, r)| r.file_name.is_some() && r.file_size.is_some());
            let asked = (bulk.len() + single.len()) as u64;
            if let Some(client) = clients.playmatch.as_ref() {
                let bulk_max = crate::database::providers::playmatch::BULK_MAX;
                for chunk in bulk.chunks(bulk_max) {
                    progress.report(summary.playmatch_lookups, Some(asked), &format!("PlayMatch: {} of {asked} asked, {} hits", summary.playmatch_lookups, summary.playmatch_hits)).await;
                    let items: Vec<(String, crate::database::providers::playmatch::IdentifyRequest)> = chunk.iter().map(|(i, r)| (i.to_string(), r.clone())).collect();
                    summary.playmatch_lookups += chunk.len() as u64;
                    let results = match client.identify_bulk(&items).await {
                        Ok(r) => r,
                        Err(e) => {
                            summary.playmatch_errors += chunk.len() as u64;
                            log::warn!("hash_match: playmatch bulk identify of {} file(s) failed: {e}", chunk.len());
                            continue;
                        }
                    };
                    for (key, result) in results {
                        let Ok(index) = key.parse::<usize>() else { continue };
                        if index >= files.len() {
                            continue;
                        }
                        // A rejected item is recorded as a miss, so it waits out the cache
                        // period instead of being asked again on every run.
                        let (response, raw) = result.unwrap_or_else(|| (crate::database::providers::playmatch::IdentifyResponse { game_match_type: Some("NoMatch".into()), external_metadata: Vec::new() }, serde_json::json!({ "gameMatchType": "NoMatch" })));
                        let record = cache::record_playmatch(pool, &store, files[index].content_id, &response, &raw, |_| true).await?;
                        if record.hit {
                            summary.playmatch_hits += 1;
                            files[index].refs.extend(record.cross_refs);
                        }
                    }
                }
            }
            for (index, request) in single {
                progress.report(summary.playmatch_lookups, Some(asked), &format!("PlayMatch: {} of {asked} asked, {} hits", summary.playmatch_lookups, summary.playmatch_hits)).await;
                summary.playmatch_lookups += 1;
                match cache::playmatch_lookup(&clients, pool, &store, files[index].content_id, &request).await {
                    Ok(record) if record.hit => {
                        summary.playmatch_hits += 1;
                        files[index].refs.extend(record.cross_refs);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        summary.playmatch_errors += 1;
                        log::warn!("hash_match: playmatch lookup for content {} failed: {e}", files[index].content_id);
                    }
                }
            }
        }

        // Cross references that found no release at lookup time (the release was built
        // afterwards, or the answer came from cache) are linked now.
        let n_files = files.len() as u64;
        for (i, file) in files.into_iter().enumerate() {
            progress.report(i as u64, Some(n_files.max(1)), &format!("linking what the lookups found: file {} of {n_files}", i + 1)).await;
            for (provider, id, links) in &file.refs {
                if *links == 0 {
                    cache::link_for_content(pool, file.content_id, *provider, id, cache::Confidence::Hash).await?;
                }
            }
            for (provider, id, _) in file.refs {
                let list = cross_refs.entry(provider).or_default();
                if !list.contains(&id) {
                    list.push(id);
                }
            }
        }

        // Follow-ups, whichever hash service matched (Hasheous alone too, so its matches also
        // get IGDB data and art): one by-id job per provider with the ids every service
        // reported...
        for (provider, ids) in cross_refs {
            if ids.is_empty() || matches!(provider, Provider::Hasheous | Provider::PlayMatch) {
                continue;
            }
            if !app.clients().is_available(provider) {
                log::info!("hash_match: {} cross-refs ({} ids) skipped, client unavailable", provider.name(), ids.len());
                continue;
            }
            summary.followup_ids += ids.len() as u64;
            let request = super::jobs::JobRequest::ProviderQuery(super::jobs::ProviderQueryRequest { provider: provider.name().to_string(), name: None, content_id: None, provider_id: None, provider_ids: Some(ids), title_ids: None, platform: None });
            db_jobs::enqueue(pool, request.job_type(), request.payload()).await?;
            summary.followups_enqueued += 1;
        }
        // ...and a by-name job per searchable provider for the titles still without a
        // link to it (no hash service had that id) -- the worker looks each up by the
        // title's own name + platform.
        let mut title_ids: Vec<u64> = Vec::new();
        let mut seen_titles: std::collections::HashSet<u64> = std::collections::HashSet::new();
        let n_targets = targets.len() as u64;
        for (i, game_id) in targets.iter().enumerate() {
            progress.report(i as u64, Some(n_targets.max(1)), &format!("follow-ups: titles of game {} of {n_targets}", i + 1)).await;
            let rows = sqlx::query("SELECT DISTINCT tr.title_id FROM local_release_games lrg JOIN title_releases tr ON tr.release_id = lrg.release_id WHERE lrg.game_id = ?").bind(game_id).fetch_all(pool).await?;
            for r in rows {
                let id: u64 = r.try_get("title_id")?;
                if seen_titles.insert(id) {
                    title_ids.push(id);
                }
            }
        }
        for provider in Provider::ALL.into_iter().filter(|p| super::links::searchable(*p) && app.clients().is_available(*p)) {
            let mut missing: Vec<u64> = Vec::new();
            let n_titles = title_ids.len() as u64;
            for (i, title_id) in title_ids.iter().enumerate() {
                progress.report(i as u64, Some(n_titles.max(1)), &format!("follow-ups: titles without {}, {} of {n_titles}", provider.name(), i + 1)).await;
                if !super::links::title_has_provider(pool, *title_id, provider).await? && !crate::database::providers::cache::unlinked_by_user(pool, *title_id, provider).await? {
                    missing.push(*title_id);
                }
            }
            if missing.is_empty() {
                continue;
            }
            summary.name_followup_titles += missing.len() as u64;
            let request = super::jobs::JobRequest::ProviderQuery(super::jobs::ProviderQueryRequest { provider: provider.name().to_string(), name: None, content_id: None, provider_id: None, provider_ids: None, title_ids: Some(missing), platform: None });
            db_jobs::enqueue(pool, request.job_type(), request.payload()).await?;
            summary.followups_enqueued += 1;
        }
    }
    // A match gives a rom its platform (above); its files' contents, and what is inside
    // them, were hashed before anyone knew and still say "unknown" -- a PS2 CHD and its inner
    // image both. In bulk at the end so games linked by an earlier run are covered too.
    progress.force(0, None, "giving the matched files their platform").await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE content c JOIN local_files f ON f.content_id = c.id AND f.directory_id IN ({scope}) JOIN local_rom_files lrf ON lrf.file_id = f.id          JOIN local_rom lr ON lr.id = lrf.rom_id AND lr.deleted = FALSE AND lr.app_platform_id <> 0 SET c.app_platform_id = lr.app_platform_id WHERE c.app_platform_id = 0"
    )))
    .execute(pool)
    .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE content c JOIN content_contents cc ON cc.child_content_id = c.id JOIN content pc ON pc.id = cc.parent_content_id AND pc.app_platform_id <> 0          JOIN local_files f ON f.content_id = pc.id AND f.directory_id IN ({scope}) SET c.app_platform_id = pc.app_platform_id WHERE c.app_platform_id = 0"
    )))
    .execute(pool)
    .await?;
    // The biggest groups of what no DAT matched, so the line says what they are.
    let kinds = summary.unmatched_by_kind.iter().take(3).map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(", ");
    let kinds = if kinds.is_empty() { String::new() } else { format!(" (mostly {kinds})") };
    // With the whole summary as the job's result, the breakdown included.
    progress.finish(1, Some(1), &format!("done: {} game(s), {} unmatched after DAT{kinds}, {} Hasheous hits, {} follow-up job(s)", summary.games, summary.unmatched, summary.hasheous_hits, summary.followups_enqueued), &summary).await;
    Ok(summary)
}

// ---------------------------------------------------------------------------
// background PlayMatch gap filler
// ---------------------------------------------------------------------------

/// Providers PlayMatch cross-references that we can use: those with a by-id fetch or links
/// worth having, and only when their client is usable (credentials present or not needed).
fn gap_fill_providers(app: &App) -> Vec<Provider> {
    [Provider::Igdb, Provider::TheGamesDb, Provider::RetroAchievements, Provider::SteamGridDb, Provider::ScreenScraper, Provider::MobyGames].into_iter().filter(|p| app.clients().is_available(*p)).collect()
}

/// Providers already linked (any confidence) to the releases/titles a content belongs to.
async fn linked_providers(pool: &MySqlPool, content_id: u64) -> Result<std::collections::HashSet<String>> {
    let rows = sqlx::query(
        "SELECT tpl.provider FROM release_contents rc JOIN title_releases tr ON tr.release_id = rc.release_id JOIN title_provider_links tpl ON tpl.title_id = tr.title_id AND tpl.deleted = FALSE WHERE rc.content_id = ? \
         UNION SELECT rpl.provider FROM release_contents rc JOIN release_provider_links rpl ON rpl.release_id = rc.release_id AND rpl.deleted = FALSE WHERE rc.content_id = ?",
    )
    .bind(content_id)
    .bind(content_id)
    .fetch_all(pool)
    .await?;
    rows.iter().map(|r| Ok(r.try_get::<String, _>("provider")?)).collect()
}

/// Contents to gap-fill: in the local collection, matched to a catalog release by exact
/// hash, with a hash and a file name/size (PlayMatch needs both), not asked within the TTL,
/// and still missing an id for at least one usable provider. `limit` 0 = all.
pub async fn playmatch_fill_candidates(app: &App, limit: u64) -> Result<Vec<u64>> {
    let pool = &app.pool;
    let providers = gap_fill_providers(app);
    if providers.is_empty() {
        return Ok(Vec::new());
    }
    let ttl_days = settings::cache_ttl_days(pool).await?;
    let mut out = Vec::new();
    let mut after = 0u64;
    loop {
        let rows = sqlx::query(
            // A file's own content, and an archive's largest member (see `lookup_contents`).
            "SELECT DISTINCT x.content_id FROM ( \
               SELECT f.content_id FROM local_files f WHERE f.deleted = FALSE \
               UNION SELECT (SELECT cc.child_content_id FROM content_contents cc JOIN content m ON m.id = cc.child_content_id WHERE cc.parent_content_id = f.content_id ORDER BY m.size DESC, m.id LIMIT 1) \
                 FROM local_files f WHERE f.deleted = FALSE \
             ) x JOIN release_contents rc ON rc.content_id = x.content_id JOIN content c ON c.id = x.content_id \
             WHERE x.content_id > ? AND c.file_name IS NOT NULL AND c.size IS NOT NULL \
             AND NOT EXISTS (SELECT 1 FROM playmatch_lookups pl WHERE pl.content_id = x.content_id AND pl.deleted = FALSE AND pl.fetched_at > NOW() - INTERVAL ? DAY) \
             ORDER BY x.content_id LIMIT 500",
        )
        .bind(after)
        .bind(ttl_days)
        .fetch_all(pool)
        .await?;
        if rows.is_empty() {
            return Ok(out);
        }
        for row in &rows {
            let content_id: u64 = row.try_get("content_id")?;
            after = content_id;
            let linked = linked_providers(pool, content_id).await?;
            if providers.iter().any(|p| !linked.contains(p.name())) {
                out.push(content_id);
                if limit > 0 && out.len() as u64 >= limit {
                    return Ok(out);
                }
            }
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GapFillSummary {
    pub candidates: u64,
    pub requests: u64,
    pub hits: u64,
    pub links: u64,
    pub followups: u64,
}

/// One gap-fill run: bulk-identify the candidates with PlayMatch (100 per request), link
/// what it names for usable providers (`Confidence::Hash`), then queue by-id metadata jobs
/// for the new ids. `limit` = the batch size setting (0 = every candidate).
pub async fn playmatch_fill(app: &App, limit: u64, progress: &crate::database::jobs::Progress) -> Result<GapFillSummary> {
    let pool = &app.pool;
    let clients = app.clients();
    let client = clients.playmatch.as_ref().ok_or_else(|| Error::unavailable("playmatch client unavailable"))?;
    let providers = gap_fill_providers(app);
    let candidates = playmatch_fill_candidates(app, limit).await?;
    let mut summary = GapFillSummary { candidates: candidates.len() as u64, ..Default::default() };
    let store = RawStore::new(app.paths.provider_cache_dir(), settings::raw_retention(pool).await?);
    let mut items: Vec<(String, crate::database::providers::playmatch::IdentifyRequest)> = Vec::new();
    for content_id in &candidates {
        let request = cache::playmatch_request_for_content(pool, *content_id).await?;
        if request.has_hash() && request.file_name.is_some() && request.file_size.is_some() {
            items.push((content_id.to_string(), request));
        }
    }
    let requests = items.len().div_ceil(crate::database::providers::playmatch::BULK_MAX) as u64;
    let mut new_ids: HashMap<Provider, std::collections::BTreeSet<String>> = HashMap::new();
    for (i, chunk) in items.chunks(crate::database::providers::playmatch::BULK_MAX).enumerate() {
        progress.report(i as u64, Some(requests.max(1)), &format!("PlayMatch gap fill: request {} of {requests} ({} contents), {} hits, {} links", i + 1, chunk.len(), summary.hits, summary.links)).await;
        let results = client.identify_bulk(chunk).await?;
        summary.requests += 1;
        for (key, result) in results {
            let Ok(content_id) = key.parse::<u64>() else { continue };
            let (response, raw) = match result {
                Some(r) => r,
                // Rejected/errored items are recorded as a miss so they wait out the TTL.
                None => (crate::database::providers::playmatch::IdentifyResponse { game_match_type: Some("NoMatch".into()), external_metadata: Vec::new() }, serde_json::json!({ "gameMatchType": "NoMatch" })),
            };
            let record = cache::record_playmatch(pool, &store, content_id, &response, &raw, |p| providers.contains(&p)).await?;
            if record.hit {
                summary.hits += 1;
            }
            for (provider, id, links) in record.cross_refs {
                summary.links += links as u64;
                if links > 0 {
                    new_ids.entry(provider).or_default().insert(id);
                }
            }
        }
    }
    // Metadata for the newly linked ids, per provider with a by-id fetch.
    for (provider, ids) in new_ids {
        if !matches!(provider, Provider::Igdb | Provider::RetroAchievements | Provider::SteamGridDb | Provider::MobyGames | Provider::ScreenScraper) {
            continue;
        }
        let ids: Vec<String> = ids.into_iter().collect();
        for chunk in ids.chunks(500) {
            crate::database::jobs::enqueue(pool, crate::database::jobs::JobType::ProviderQuery, serde_json::json!({ "provider": provider.name(), "provider_ids": chunk })).await?;
            summary.followups += 1;
        }
    }
    progress.force(requests.max(1), Some(requests.max(1)), &format!("PlayMatch gap fill: {} contents, {} requests, {} hits, {} links, {} metadata job(s)", summary.candidates, summary.requests, summary.hits, summary.links, summary.followups)).await;
    Ok(summary)
}