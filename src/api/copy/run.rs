//! Moving the bytes.
//!
//! Plain filesystem I/O on the worker's own machine -- nothing here goes through REST, which
//! is the reason the mount has to exist in the first place. Three properties the code is
//! shaped around:
//!
//! **Resumable.** Every write lands on `<dst>.recopy-part` and is renamed into place, so a
//! crash or a cancel never leaves a truncated file that the next run would mistake for a
//! finished one. Before copying, an existing destination that matches by size *and* by the
//! cheapest hash the catalogue already has is counted as done. Re-running an identical job is
//! therefore a no-op, and that is the whole resume story: a copy that died halfway is resumed
//! by enqueueing it again.
//!
//! **Cancellable.** `worker.rs` cancels by aborting the tokio task, which cannot interrupt a
//! `spawn_blocking` copy loop -- and one CHD over SMB is minutes of uninterruptible work. So
//! an async sibling polls `jobs.cancel_requested` and flips an `AtomicBool` the blocking loop
//! checks between items and every chunk.
//!
//! **Honest about what it did.** Every skip, rename, duplicate and failure is a field on
//! `CopySummary`, because a copy that quietly did three-quarters of the job is worse than one
//! that failed.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use romexplorer_types::copy::{ConflictPolicy, CopyFilesRequest, CopySummary, OutputFormat, OutputSettings};

use crate::database::jobs::Progress;

use super::super::{App, Error, Result};
use super::content_check;
use super::mount;
use super::plan::{self, PlannedCopy};
use super::upload;

/// Below this, `std::fs::copy` wins: it reaches `copy_file_range` / `CopyFileEx` and beats a
/// userspace loop. Above it the loop earns its keep in progress and cancel granularity.
const CHUNKED_ABOVE: u64 = 64 * 1024 * 1024;
const BUF: usize = 4 * 1024 * 1024;

/// Suffix for a copy in flight. Swept at job start, so a previous crash leaves no litter.
const PART_SUFFIX: &str = ".recopy-part";

/// A cancel flag the blocking copy can read without touching the database.
#[derive(Clone)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub fn set(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Polls `jobs.cancel_requested` once a second until the flag is set or `stop` is.
    /// Spawned alongside the blocking work; the blocking thread must never `block_on`.
    pub fn watch(&self, app: &App, job_id: u64, stop: Cancel) -> tokio::task::JoinHandle<()> {
        let flag = self.clone();
        let pool = app.pool.clone();
        tokio::spawn(async move {
            while !stop.is_set() {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                if stop.is_set() {
                    return;
                }
                if let Ok(ids) = crate::database::jobs::cancel_requested(&pool, &[job_id]).await {
                    if !ids.is_empty() {
                        flag.set();
                        return;
                    }
                }
            }
        })
    }
}

impl Default for Cancel {
    fn default() -> Self {
        Self::new()
    }
}

/// Copies `src` to `dst`, reporting bytes as it goes. `on_chunk` returning false aborts and
/// leaves nothing behind.
pub fn copy_with_progress(src: &Path, dst: &Path, on_chunk: &mut dyn FnMut(u64) -> bool) -> std::io::Result<u64> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let part = part_path(dst);
    let size = std::fs::metadata(src)?.len();
    if size <= CHUNKED_ABOVE {
        let n = std::fs::copy(src, &part)?;
        if !on_chunk(n) {
            let _ = std::fs::remove_file(&part);
            return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled"));
        }
        std::fs::rename(&part, dst)?;
        return Ok(n);
    }
    let mut reader = std::fs::File::open(src)?;
    let mut writer = std::fs::File::create(&part)?;
    let mut buf = vec![0u8; BUF];
    let mut total = 0u64;
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                drop(writer);
                let _ = std::fs::remove_file(&part);
                return Err(e);
            }
        };
        if let Err(e) = writer.write_all(&buf[..n]) {
            drop(writer);
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
        total += n as u64;
        if !on_chunk(n as u64) {
            drop(writer);
            let _ = std::fs::remove_file(&part);
            return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled"));
        }
    }
    writer.flush()?;
    drop(writer);
    std::fs::rename(&part, dst)?;
    Ok(total)
}

/// One member of a zip or 7z written out as a loose file at `dst`, through the bundled 7-Zip:
/// into the part file first, renamed when whole, as [`copy_with_progress`] does.
pub fn unpack_member(archive: &Path, format: crate::rom::sevenzip_wrapper::ArchiveFormat, member: &str, size: u64, dst: &Path, on_chunk: &mut dyn FnMut(u64) -> bool) -> std::io::Result<u64> {
    use crate::rom::sevenzip_wrapper::{SevenZipArchive, SevenZipError};
    let fail = |e: SevenZipError| match e {
        SevenZipError::Cancelled => std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled"),
        other => std::io::Error::other(format!("{}: {other}", archive.display())),
    };
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let opened = SevenZipArchive::open(archive, format, |_, _| true).map_err(fail)?;
    // Members are compared the way the scan recorded them: forward slashes.
    let wanted = member.replace('\\', "/");
    let mut index = None;
    for i in 0..opened.entry_count() {
        let entry = opened.entry(i).map_err(fail)?;
        if !entry.is_dir && entry.path.replace('\\', "/") == wanted {
            index = Some(i);
            break;
        }
    }
    let Some(index) = index else {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("{member} is no longer in {}", archive.display())));
    };
    let part = part_path(dst);
    let mut reported = 0u64;
    let result = opened.extract_to_file(index, &part, |_, percent| {
        if percent < 0.0 {
            return on_chunk(0);
        }
        let now = ((size as f64) * (percent.min(100.0) as f64) / 100.0) as u64;
        let step = now.saturating_sub(reported);
        reported = reported.max(now);
        on_chunk(step)
    });
    if let Err(e) = result {
        let _ = std::fs::remove_file(&part);
        return Err(fail(e));
    }
    let written = std::fs::metadata(&part)?.len();
    if !on_chunk(written.saturating_sub(reported)) {
        let _ = std::fs::remove_file(&part);
        return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled"));
    }
    std::fs::rename(&part, dst)?;
    Ok(written)
}

fn part_path(dst: &Path) -> PathBuf {
    let mut name = dst.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "copy".to_string());
    name.push_str(PART_SUFFIX);
    dst.parent().map(|p| p.join(&name)).unwrap_or_else(|| PathBuf::from(name))
}

/// Removes `*.recopy-part` files and `.recopy-*` conversion scratch folders under `root`, so
/// a previous crash or cancel doesn't leave litter and a later run doesn't trip over a stale
/// one. Best-effort and depth-limited: the destination can be a whole card.
///
/// Only ones untouched for a while: another copy job into an overlapping folder (`/mnt/sd`
/// and `/mnt/sd/roms` are different lanes) may be writing one right now.
fn sweep_parts(root: &Path, depth: u32) {
    const STALE: std::time::Duration = std::time::Duration::from_secs(15 * 60);
    if depth > 6 {
        return;
    }
    let stale = |path: &Path| std::fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age >= STALE);
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if name.starts_with(".recopy-") {
                // A scratch folder is as fresh as the newest file in it.
                let newest = std::fs::read_dir(&path).map(|rd| rd.flatten().all(|e| stale(&e.path()))).unwrap_or(true);
                if newest && stale(&path) {
                    let _ = std::fs::remove_dir_all(&path);
                }
            } else {
                sweep_parts(&path, depth + 1);
            }
        } else if name.ends_with(PART_SUFFIX) && stale(&path) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Is `dst` already this content?
///
/// Size first, then the cheapest algorithm the catalogue already holds for it -- preferring
/// `xxh3_128` and `crc32`, because over SMB this reads the whole file and the point is to
/// make a re-run cheap, not thorough. No hash row at all falls back to size alone, and the
/// summary says so.
async fn already_there(app: &App, dst: &Path, content_id: u64, size: u64) -> (bool, bool) {
    let Ok(meta) = std::fs::metadata(dst) else { return (false, false) };
    if meta.len() != size {
        return (false, false);
    }
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT algorithm, value_hex FROM hashes WHERE content_id = ?").bind(content_id).fetch_all(&app.pool).await.unwrap_or_default();
    use crate::rom::hash::HashFlags;
    let pick = ["xxh3_128", "crc32", "md5", "sha1", "sha256"].into_iter().find_map(|algo| rows.iter().find(|(a, _)| a == algo).map(|(a, v)| (a.clone(), v.clone())));
    let Some((algo, expected)) = pick else { return (true, true) };
    let flag = match algo.as_str() {
        "xxh3_128" => HashFlags::XXH3_128,
        "crc32" => HashFlags::CRC32,
        "md5" => HashFlags::MD5,
        "sha1" => HashFlags::SHA1,
        _ => HashFlags::SHA256,
    };
    let path = dst.to_path_buf();
    let got = tokio::task::spawn_blocking(move || crate::rom::hash::hash(&path, &crate::rom::hash::HashOptions { flags: flag, ..Default::default() }, |_| {}))
        .await
        .ok()
        .and_then(|r| r.ok());
    let matches = got
        .and_then(|r| match algo.as_str() {
            "xxh3_128" => r.xxh3_128,
            "crc32" => r.crc32,
            "md5" => r.md5,
            "sha1" => r.sha1,
            _ => r.sha256,
        })
        .is_some_and(|v| v.eq_ignore_ascii_case(&expected));
    (matches, false)
}

/// Rewrites an `.m3u`'s lines to the names this job actually produced.
///
/// Necessary because a bin/cue -> chd conversion changes every extension, and an unrewritten
/// playlist is silently broken. Only a line whose stem matches something produced is touched;
/// anything else is left exactly as it was and reported, so a playlist the app did not create
/// cannot be corrupted by a bad guess.
pub fn rewrite_m3u(text: &str, produced: &[(String, String)]) -> (String, Vec<String>) {
    let mut out = String::with_capacity(text.len());
    let mut unmatched = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let name = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
        let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
        // The file the line names first: a disc's cue and bin share a stem, and matching on
        // the stem alone could turn the cue's line into the bin's -- whichever was copied
        // first. The stem only when nothing has that name (a cue folded into a CHD).
        // Among same-stem files, a disc's sheet or image before its tracks: an unpacked zip
        // names its .cue and its .bin by the stem the playlist gave the zip.
        let exact = produced.iter().find(|(from, _)| from.eq_ignore_ascii_case(name));
        let same_stem: Vec<&(String, String)> = produced.iter().filter(|(from, _)| from.rsplit_once('.').map(|(s, _)| s).unwrap_or(from).eq_ignore_ascii_case(stem)).collect();
        let disc = |to: &str| matches!(to.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).as_deref(), Some("cue" | "gdi" | "ccd" | "chd" | "iso" | "rvz" | "cso" | "pbp"));
        let by_stem = same_stem.iter().find(|(_, to)| disc(to)).or(same_stem.first()).copied();
        match exact.or(by_stem) {
            Some((_, to)) => out.push_str(to),
            None => {
                unmatched.push(trimmed.to_string());
                out.push_str(line);
            }
        }
        out.push('\n');
    }
    (out, unmatched)
}

/// Whether an error means the whole job should stop rather than log and carry on.
fn is_fatal(e: &std::io::Error) -> bool {
    matches!(e.kind(), std::io::ErrorKind::StorageFull | std::io::ErrorKind::QuotaExceeded)
}

/// Runs a copy. `job_id` enables cancellation; `None` (a CLI call) simply cannot be cancelled.
pub async fn run(app: &App, req: &CopyFilesRequest, progress: &Progress, job_id: Option<u64>) -> Result<CopySummary> {
    super::validate_mode(app, req).await?;
    crate::database::settings::apply_chd_reader(&app.pool).await?;
    crate::database::settings::apply_scan_pkg_titles(&app.pool).await?;
    let root = super::check_destination(&req.destination)?;
    super::check_writable(&root)?;

    // Every mount the job will read through, checked before anything is read. A mount aimed
    // at the wrong share passes `is_dir` and fails the probe, which is the failure worth
    // catching here rather than after a thousand wrong files.
    let mounts = mount::load(app, None).await?;
    for m in &mounts {
        let check = mount::verify(app, m).await?;
        if !check.ok {
            return Err(Error::unavailable(format!("mount {} -> {}: {}", m.remote_prefix, m.local_path, check.detail)));
        }
        let _ = mount::mark_verified(app, m.id).await;
    }

    let settings = match &req.output {
        Some(o) => {
            super::validate_output(o)?;
            o.clone()
        }
        None => super::output_defaults(app).await?,
    };

    progress.force(0, None, "planning").await;
    let planned = plan::plan(app, req).await?;

    if let Some(free) = planned.view.free_bytes {
        if free < planned.view.required_bytes {
            return Err(Error::invalid(format!(
                "{} needs {} free (including the safety margin) and has {}",
                root.display(),
                crate::database::jobs::human_bytes(planned.view.required_bytes),
                crate::database::jobs::human_bytes(free)
            )));
        }
    }

    sweep_parts(&root, 0);

    let mut summary = CopySummary {
        destination: root.display().to_string(),
        mode: req.mode,
        items: planned.view.items,
        deduped: planned.view.deduped,
        already_held: planned.view.already_held,
        duplicated_content: planned.duplicated_content.clone(),
        renamed: planned.view.renamed.clone(),
        no_mount: planned.view.no_mount.clone(),
        archiving_unavailable: planned.view.archiving_unavailable.clone(),
        ..Default::default()
    };

    // In-server is the one mode that does not take "don't check" for an answer. It moves a
    // file within a collection the app is responsible for, often deleting nothing but leaving
    // the catalogue pointing at the new copy -- a silent bad write there is a lost ROM, not an
    // inconvenient one.
    let verify = req.mode.mandatory_verify() || req.verify.unwrap_or(super::verify_default(app).await?);
    let populate_name = super::populate_hash_name(app).await?;
    let populate = super::algorithm_from_name(&populate_name).unwrap_or(crate::common::models::HashAlgorithms::Xxhash3_128);
    // What a re-containered output is described with. The scan's own set, not the one quick
    // populate hash: the new archive is bytes nobody has seen, and giving it the full set now
    // is what lets the *next* integrity check compare the whole file instead of decoding it
    // again, and what lets a DAT match it at all.
    let container_flags = crate::rom::hash::HashFlags::default() | crate::rom::hash::HashFlags::algorithm_to_flag(populate);

    let cancel = Cancel::new();
    let stop = Cancel::new();
    let watcher = job_id.map(|id| cancel.watch(app, id, stop.clone()));

    let total_bytes: u64 = planned.view.bytes;
    // Shared, because several files are copied at once and each adds what it moves.
    let done = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let skip_bytes = |n: u64| {
        done.fetch_add(n, Ordering::Relaxed);
    };
    let total_files = planned.files.len();
    // Per item: what each source file became, so the item's `.m3u` can be rewritten.
    let mut produced: std::collections::HashMap<usize, Vec<(String, String)>> = std::collections::HashMap::new();
    let mut failed_items: std::collections::HashSet<usize> = std::collections::HashSet::new();
    // What landed, and the content it is -- `None` means "the source's own", which is the
    // usual case and the reason registration needs no hashing at all.
    let mut copied: Vec<(&PlannedCopy, Option<u64>)> = Vec::new();
    // Which of them were actually read back and matched, so registration can stamp
    // `last_integrity_check` on exactly those and no others.
    let mut verified_files: std::collections::HashSet<(usize, u64)> = std::collections::HashSet::new();
    // And which were read end to end with the scan's own hash set, which is a different and
    // stronger claim -- `FileData::hash` versus `FileData::integrity`, the same split the
    // `local_files` stamps draw.
    let mut full_hashed: std::collections::HashSet<(usize, u64)> = std::collections::HashSet::new();
    // Destinations holding a file whose payload was converted, so a `hash_match` can say what
    // it now is. Empty until payload conversion is switched on.
    let mut rehashed_dirs: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();

    // A plain loop rather than a helper closure: the body needs `&mut summary`, `await`, and
    // an early exit, and every way of wrapping that costs more than it saves.
    let mut fatal: Option<Error> = None;
    // Two passes. First, in order and one at a time, what each file needs: skipped, already
    // there, or a conflict resolved -- quick checks, and the ones whose order matters (a
    // `fail` policy stops at the first conflict). Then the files that need copying run several
    // at once (`api::parallel`), and what each came to is folded into the summary as it ends.
    let mut work: Vec<&PlannedCopy> = Vec::new();
    for file in planned.files.iter() {
        if cancel.is_set() {
            summary.cancelled = true;
            break;
        }
        if let Some(reason) = &file.skip {
            summary.bytes_skipped += file.weight;
            skip_bytes(file.weight);
            if reason == "already at the destination" {
                summary.files_skipped_same += 1;
            } else if reason == super::plan::BLOCKED {
                summary.blocked_existing.push(file.destination.display().to_string());
                failed_items.insert(file.item);
            } else if reason == super::plan::KEPT {
                summary.kept_existing.push(file.destination.display().to_string());
            }
            continue;
        }

        // A destination that already holds these exact bytes is the resume path. Only for a
        // plain copy: a converted output has different bytes from its source by definition,
        // so hashing it against the source's own hash would read the whole file to learn
        // nothing.
        let (same, size_only) = if file.format == OutputFormat::Same || file.source.member.is_some() {
            already_there(app, &file.destination, file.source.content_id, file.source.size).await
        } else {
            // A converted output's bytes are not the source's, so there is nothing of its own
            // recorded to compare against on a first run. An existing non-empty file is taken
            // as done -- which is what makes re-running a conversion cheap -- and counted as
            // the guess it is. `on_conflict: overwrite` forces it.
            let exists = std::fs::metadata(&file.destination).map(|m| m.len() > 0).unwrap_or(false);
            let forced = req.on_conflict == ConflictPolicy::Overwrite || req.conflict_choices.iter().any(|(k, c)| *c == romexplorer_types::copy::ConflictChoice::Overwrite && Path::new(k) == file.destination);
            (exists && !forced, exists)
        };
        if same {
            summary.files_skipped_same += 1;
            summary.bytes_skipped += file.weight;
            skip_bytes(file.weight);
            if size_only {
                summary.skipped_size_only += 1;
            }
            record_produced(&mut produced, file);
            copied.push((file, None));
            continue;
        }
        if file.destination.exists() {
            // The planner already refused this, but between the preview and here is a window
            // somebody else can write in, and "never overwrites" has to mean never.
            if req.mode.never_overwrites() {
                summary.blocked_existing.push(file.destination.display().to_string());
                failed_items.insert(file.item);
                summary.bytes_skipped += file.weight;
                skip_bytes(file.weight);
                continue;
            }
            // The same rule the planner applied, re-applied against the file as it is now:
            // for a mode that asks, only an explicit yes overwrites.
            if req.mode.asks_about_conflicts()
                && !req.conflict_choices.iter().any(|(k, c)| *c == romexplorer_types::copy::ConflictChoice::Overwrite && Path::new(k) == file.destination)
            {
                summary.bytes_skipped += file.weight;
                skip_bytes(file.weight);
                continue;
            }
            let answer = req.conflict_choices.iter().find(|(k, _)| Path::new(k) == file.destination).map(|(_, c)| *c);
            if answer == Some(romexplorer_types::copy::ConflictChoice::Overwrite) {
                let _ = std::fs::remove_file(&file.destination);
                work.push(file);
                continue;
            }
            if answer == Some(romexplorer_types::copy::ConflictChoice::Skip) {
                summary.kept_existing.push(file.destination.display().to_string());
                summary.bytes_skipped += file.weight;
                skip_bytes(file.weight);
                continue;
            }
            match req.on_conflict {
                ConflictPolicy::Skip => {
                    summary.kept_existing.push(file.destination.display().to_string());
                    summary.bytes_skipped += file.weight;
                    skip_bytes(file.weight);
                    continue;
                }
                ConflictPolicy::Fail => {
                    fatal = Some(Error::conflict(format!("{} already exists and differs", file.destination.display())));
                    break;
                }
                ConflictPolicy::Overwrite => {
                    let _ = std::fs::remove_file(&file.destination);
                }
                // `Rename` picked a free name at plan time, against this job's writes and the
                // disk. A file there now is one of the same size that turned out different,
                // or one written since the preview: it stays, and the summary says so.
                ConflictPolicy::Rename if req.conflict_choices.iter().any(|(k, c)| *c == romexplorer_types::copy::ConflictChoice::Overwrite && Path::new(k) == file.destination) => {
                    let _ = std::fs::remove_file(&file.destination);
                }
                ConflictPolicy::Rename => {
                    summary.blocked_existing.push(file.destination.display().to_string());
                    failed_items.insert(file.item);
                    summary.bytes_skipped += file.weight;
                    skip_bytes(file.weight);
                    continue;
                }
            }
        }

        work.push(file);
    }

    if fatal.is_none() && !work.is_empty() {
        use crate::api::parallel::{Gate, Tier};
        use futures_util::stream::{FuturesUnordered, StreamExt};
        // Auto reads the slower of the two disks; a conversion counts as the heavy stage it is.
        let stages = crate::rom::operation::JobStages { archive: work.iter().any(|f| f.format != OutputFormat::Same && f.format != OutputFormat::Raw), ..Default::default() };
        let flags = crate::rom::hash::HashFlags::default();
        let source = work[0].source.path.parent().map(Path::to_path_buf).unwrap_or_else(|| root.clone());
        let (src_root, dst_root) = (source.clone(), root.clone());
        let setting = crate::database::settings::parallel_files(&app.pool, crate::database::settings::KEY_COPY_PARALLEL).await?;
        let auto = tokio::task::spawn_blocking(move || setting.auto_limits(&src_root, flags, stages).min(setting.auto_limits(&dst_root, flags, stages))).await.map_err(|e| Error::internal(e.to_string()))?;
        let limits = setting.resolve(auto);
        let gate = Gate::new(limits);
        let n_work = work.len();
        // One size at a time, smallest first -- as a scan does: the three tiers' numbers are
        // how many of that size run at once, not three queues side by side (1 / 1 / 1 had a
        // small, a medium and a large file copying together). A tier starts once every file of
        // the ones before it is done.
        let tier_index = |weight: u64| match Tier::of(weight) {
            Tier::Small => 0usize,
            Tier::Medium => 1,
            Tier::Large => 2,
        };
        let mut left = [0usize; 3];
        for f in &work {
            left[tier_index(f.weight)] += 1;
        }
        let first_turn = (0..3).find(|&i| left[i] > 0).unwrap_or(0);
        let (turn_tx, turn_rx) = tokio::sync::watch::channel(first_turn);
        progress.force(done.load(Ordering::Relaxed), Some(total_bytes), &format!("copying {n_work} file(s), up to {} small / {} medium / {} large at once", limits.small, limits.medium, limits.large)).await;
        let mut running: FuturesUnordered<_> = work
            .iter()
            .map(|&file| {
                let (gate, settings, cancel, done) = (&gate, &settings, &cancel, &done);
                let mut turn = turn_rx.clone();
                let my_turn = tier_index(file.weight);
                async move {
                    let _ = turn.wait_for(|t| *t >= my_turn).await;
                    let _slot = gate.enter(Tier::of(file.weight)).await;
                    if cancel.is_set() {
                        return (file, Ok(CopyOutcome::Cancelled), None);
                    }
                    let outcome = copy_one(app, file, settings, populate, container_flags, verify, cancel, progress, done, total_bytes).await;
                    let checked = match &outcome {
                        Ok(CopyOutcome::Copied { converted, payload_converted, content_id, read_back, .. }) => Some(check_copy(app, file, verify, *converted, *payload_converted, *content_id, read_back.as_deref(), populate, container_flags).await),
                        _ => None,
                    };
                    (file, outcome, checked)
                }
            })
            .collect();
        let mut finished = 0usize;
        while let Some((file, outcome, checked)) = running.next().await {
            finished += 1;
            // This size done: the next one with files starts.
            let t = tier_index(file.weight);
            left[t] = left[t].saturating_sub(1);
            if left[t] == 0 && *turn_tx.borrow() == t {
                let next = (t + 1..3).find(|&i| left[i] > 0).unwrap_or(3);
                let _ = turn_tx.send(next);
            }
            let name = file.destination.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let now = done.load(Ordering::Relaxed);
            progress
                .report(now, Some(total_bytes), &format!("{finished} of {n_work} files done ({} at once), last: {name} ({} of {})", gate.running(), crate::database::jobs::human_bytes(now), crate::database::jobs::human_bytes(total_bytes)))
                .await;
            match outcome {
                Ok(CopyOutcome::Copied { bytes, converted, .. }) => {
                    let c = checked.unwrap_or_default();
                    if c.verified {
                        summary.verified += 1;
                        verified_files.insert((file.item, file.source.file_id));
                    }
                    if c.recorded {
                        summary.hashes_recorded += 1;
                    }
                    if c.content_checked {
                        summary.content_checked += 1;
                    }
                    if c.full_hashed {
                        full_hashed.insert((file.item, file.source.file_id));
                    }
                    if c.archive_hashed {
                        summary.archives_hashed += 1;
                    }
                    if c.unverifiable {
                        summary.unverifiable += 1;
                    }
                    if let Some(dir) = c.rehashed_dir {
                        rehashed_dirs.insert(dir);
                    }
                    summary.content_mismatched.extend(c.content_mismatched);
                    summary.errors.extend(c.errors);
                    if let Some(bad) = c.verify_failed {
                        summary.verify_failed.push(bad);
                    }
                    if !c.good {
                        failed_items.insert(file.item);
                    }
                    summary.files_copied += 1;
                    if converted {
                        summary.files_converted += 1;
                    }
                    summary.bytes_copied += bytes;
                    record_produced(&mut produced, file);
                    // A copy whose bytes are wrong is never recorded as a held file.
                    if c.good {
                        copied.push((file, c.identity));
                    }
                }
                Ok(CopyOutcome::Cancelled) => summary.cancelled = true,
                Err(e) => {
                    failed_items.insert(file.item);
                    // A source path that vanished is one deleted file or a lost mount;
                    // re-verifying tells them apart, and a lost mount is not survivable.
                    if fatal.is_none() {
                        if let Some(source_id) = file.source.owner {
                            if !file.source.path.exists() {
                                for m in mounts.iter().filter(|m| m.source_id == source_id) {
                                    if !mount::verify(app, m).await.map(|c| c.ok).unwrap_or(false) {
                                        fatal = Some(Error::unavailable(format!(
                                            "the mount for {} disappeared; {} of {} files copied -- re-run this job to resume",
                                            m.local_path, summary.files_copied, total_files
                                        )));
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    // A full destination writes hundreds of identical errors and a field of
                    // partial files if it is allowed to continue.
                    if fatal.is_none() && e.kind == super::super::ErrorKind::Unavailable && e.message.contains("full") {
                        fatal = Some(e);
                    } else if fatal.is_none() {
                        // Its bytes are already counted as done: each path of `copy_one` counts
                        // what it did not move itself, failed or not.
                        summary.errors.push(format!("{}: {}", file.source.path.display(), e.message));
                    }
                    // Stop the rest: the ones running see the flag between chunks, the ones
                    // waiting for a slot never start.
                    if fatal.is_some() {
                        cancel.set();
                    }
                }
            }
        }
    }

    stop.set();
    if let Some(w) = watcher {
        w.abort();
    }
    if let Some(e) = fatal {
        // What did land is recorded all the same: otherwise a later run finds it there,
        // takes it as done, and it is never registered at all.
        if req.mode.registers() {
            if let Err(r) = register(app, &planned, &copied, &failed_items, &verified_files, &full_hashed).await {
                log::warn!("copy: registering the files copied before the failure: {}", r.message);
            }
        }
        return Err(e);
    }

    // Playlists last: their lines can only be rewritten once the discs have names.
    for (index, file) in planned.files.iter().enumerate() {
        let _ = index;
        if !file.is_playlist || file.skip.is_some() {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&file.source.path) {
            // Every disc of the playlist's game, not only the first one it rides with: a
            // two-disc set converted to CHD had its second line left naming the old cue.
            let game = planned.items.get(file.item).and_then(|i| i.game);
            let names: Vec<(String, String)> = match game {
                Some(game) => planned.items.iter().enumerate().filter(|(_, i)| i.game == Some(game)).flat_map(|(n, _)| produced.get(&n).cloned().unwrap_or_default()).collect(),
                None => produced.get(&file.item).cloned().unwrap_or_default(),
            };
            let names: Vec<(String, String)> = names.into_iter().map(|(from, to)| (from, playlist_line(&file.destination, std::path::Path::new(&to)))).collect();
            let (rewritten, unmatched) = rewrite_m3u(&text, &names);
            if let Err(e) = std::fs::write(&file.destination, rewritten) {
                summary.errors.push(format!("{}: {e}", file.destination.display()));
            }
            summary.m3u_unmatched.extend(unmatched);
        }
    }

    for index in &failed_items {
        if let Some(item) = planned.items.get(*index) {
            summary.incomplete_items.push(item.game_name.clone());
        }
    }

    if req.mode.registers() {
        match register(app, &planned, &copied, &failed_items, &verified_files, &full_hashed).await {
            Ok((n, ra)) => {
                summary.registered = n;
                summary.ra_hashes_carried = ra;
            }
            Err(e) => summary.errors.push(format!("registering the copied files: {}", e.message)),
        }
    }

    // An upload's rows are the server's to write, so the client asks for them instead of
    // inventing them -- and that request is also the mandatory rehash, taken by the machine
    // that now holds the bytes rather than the one that sent them.
    if req.mode == romexplorer_types::copy::CopyMode::Upload && !summary.cancelled {
        let dirs: std::collections::BTreeSet<PathBuf> = copied.iter().filter_map(|(f, _)| f.destination.parent().map(Path::to_path_buf)).collect();
        match super::plan::upload_target(app, req).await {
            Ok(Some(source_id)) => match upload::request_scan(app, source_id, &dirs).await {
                Ok(requests) => {
                    summary.rehash_jobs.extend(requests.job_ids);
                    summary.errors.extend(requests.errors);
                }
                Err(e) => summary.errors.push(format!("asking the server to scan what was uploaded: {}", e.message)),
            },
            Ok(None) => summary.errors.push("the upload landed, but there's no server to tell about it".to_string()),
            Err(e) => summary.errors.push(e.message),
        }
    }

    // Payload-converted files are new things; a hash match is what says which things. One job
    // per directory, not per file, since that is the unit `hash_match` takes.
    for dir in &rehashed_dirs {
        let resolved = crate::database::directories::resolve(&app.pool, &dir.display().to_string()).await.ok().and_then(|e| e.directory).map(|d| d.id);
        let Some(directory_id) = resolved else { continue };
        let request = romexplorer_types::jobs::JobRequest::HashMatch(romexplorer_types::jobs::HashMatchRequest { directory_id, to_dat: true, to_hasheous: false, to_playmatch: false, all: false });
        match crate::api::jobs::enqueue_unless_queued(app, &request).await {
            Ok(Some(id)) => summary.rehash_jobs.push(id),
            Ok(None) => {}
            Err(e) => summary.errors.push(format!("queueing a hash match for {}: {}", dir.display(), e.message)),
        }
    }
    if !req.export_artwork.is_empty() {
        let files = export_files(&planned, &copied);
        summary.artwork_jobs = crate::api::artwork::queue_after_copy(app, &req.export_artwork, &files).await;
    }
    super::remember_destination(app, &root.display().to_string()).await;
    progress.force(done.load(Ordering::Relaxed), Some(total_bytes), &one_liner(&summary)).await;
    Ok(summary)
}

/// What the copy wrote, as the files an emulator will list: one per release, named after its
/// playlist when it has one (a multi-disc game), else its first file.
fn export_files(planned: &plan::Planned, copied: &[(&PlannedCopy, Option<u64>)]) -> Vec<romexplorer_types::jobs::ExportFile> {
    let mut out: Vec<romexplorer_types::jobs::ExportFile> = Vec::new();
    let mut seen: Vec<(usize, bool)> = Vec::new();
    for (file, _) in copied {
        let Some(release_id) = planned.items.get(file.item).and_then(|i| i.release_id) else { continue };
        let Some(stem) = file.destination.file_stem().map(|s| s.to_string_lossy().to_string()) else { continue };
        match seen.iter().position(|(item, _)| *item == file.item) {
            // A playlist names the game over its first disc.
            Some(at) if file.is_playlist && !seen[at].1 => {
                out[at].stem = stem;
                seen[at].1 = true;
            }
            Some(_) => {}
            None => {
                seen.push((file.item, file.is_playlist));
                out.push(romexplorer_types::jobs::ExportFile { release_id, platform: file.platform.igdb_id(), stem });
            }
        }
    }
    out
}

/// Which of the three checks a copied file gets.
///
/// The distinction is what the copy did to the bytes, and it is not cosmetic: asking the
/// wrong question here returns a confident wrong answer rather than an error.
///
/// - **Whole**: the bytes are the source's. Compare the file against a hash the catalogue
///   already has for that content. Cheapest, and the only one that needs no decode.
/// - **Content**: the container changed but the payload did not -- a cue+bin written as one
///   CHD, a 7z rewritten as a zip. The file will not equal anything recorded, so compare what
///   is *inside* it against the members the source content had, and give the new container
///   its own identity off the same pass. This is what an in-server copy of an archive gets.
/// - **Rehash**: the payload itself changed (header add/strip, byte order, decryption). There
///   is nothing it should equal, so nothing is verified; it is hashed to be identified, and
///   matched against the catalogue afterwards. Unreachable today -- `convert_one` runs with
///   `StageSwitches { convert: false }` -- and deliberately written anyway, because that
///   switch flipping would otherwise silently record files as things they are not.
fn check_kind(verify: bool, file: &PlannedCopy, converted: bool, payload_converted: bool) -> CheckKind {
    if file.is_playlist {
        return CheckKind::None;
    }
    // A disc made into a .wua or .zar was read back against its manifest, an XISO file by
    // file and a CSO against the ISO's hash, by the converters themselves.
    if converted && matches!(file.format, OutputFormat::Wua | OutputFormat::Zar | OutputFormat::Xiso | OutputFormat::Cso) {
        return CheckKind::None;
    }
    // A rehash is not a check the user can decline: without it the file has no identity at
    // all, which is worse than an unverified one.
    if payload_converted {
        return CheckKind::Rehash;
    }
    if !verify {
        return CheckKind::None;
    }
    if converted {
        CheckKind::Content
    } else {
        CheckKind::Whole
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum CheckKind {
    None,
    Whole,
    Content,
    Rehash,
}

fn record_produced(produced: &mut std::collections::HashMap<usize, Vec<(String, String)>>, file: &PlannedCopy) {
    if file.is_playlist {
        return;
    }
    // An unpacked member by its own name: the archive's name now stands for several files.
    let from = match &file.source.member {
        Some(member) => member.rsplit(['/', '\\']).next().unwrap_or(member).to_string(),
        None => file.source.path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
    };
    // The whole destination: a disc that kept its game's folder is named from the playlist's
    // place by its path (`playlist_line`), not by its file name alone.
    let to = file.destination.to_string_lossy().to_string();
    produced.entry(file.item).or_default().push((from, to));
}

/// How a playlist at `playlist` names `disc`: its path below the playlist's folder with `/`
/// (a disc in its game's own folder), or its file name when it is elsewhere.
pub fn playlist_line(playlist: &std::path::Path, disc: &std::path::Path) -> String {
    let base = playlist.parent().unwrap_or(std::path::Path::new(""));
    match disc.strip_prefix(base) {
        Ok(rel) if rel.components().count() > 0 => rel.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect::<Vec<_>>().join("/"),
        _ => disc.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
    }
}

enum CopyOutcome {
    /// `content_id` is `Some` only for a converted output, whose bytes are new and had to be
    /// hashed to be identified at all. A plain copy is the source's own content.
    /// `converted` means the container changed; `payload_converted` means the bytes inside
    /// did too. They are separate because only the first leaves the payload -- and so the
    /// RetroAchievements hash, and the member hashes -- intact.
    /// `read_back`: a converted output as `Single` read it back once written -- its own
    /// bytes, and with verify its members -- for the check to compare instead of decoding
    /// the file again.
    Copied { bytes: u64, converted: bool, payload_converted: bool, content_id: Option<u64>, read_back: Option<Box<crate::rom::operation::CheckedOutput>> },
    Cancelled,
}

/// Copies (or converts) one file. `done` is the job's shared byte count: this file adds what
/// it moves as it goes, and the rest of its weight when it ends, whichever way it ends.
#[allow(clippy::too_many_arguments)]
async fn copy_one(
    app: &App,
    file: &PlannedCopy,
    settings: &OutputSettings,
    populate: crate::common::models::HashAlgorithms,
    container_flags: crate::rom::hash::HashFlags,
    verify: bool,
    cancel: &Cancel,
    progress: &Progress,
    done: &Arc<std::sync::atomic::AtomicU64>,
    total_bytes: u64,
) -> Result<CopyOutcome> {
    if file.format == OutputFormat::Wua && file.source_format != OutputFormat::Wua {
        return convert_wua(app, file, populate, container_flags, verify, cancel, progress, done, total_bytes).await;
    }
    if matches!(file.format, OutputFormat::Zar | OutputFormat::Xiso | OutputFormat::Cso) && file.source_format != file.format {
        if let Some(outcome) = convert_disc(app, file, populate, container_flags, verify, cancel, progress, done, total_bytes).await? {
            return Ok(outcome);
        }
    } else if !matches!(file.format, OutputFormat::Same | OutputFormat::Raw | OutputFormat::Wua | OutputFormat::Zar | OutputFormat::Xiso | OutputFormat::Cso) {
        match convert_one(app, file, settings, populate, container_flags, verify, cancel).await {
            Ok(Some(outcome)) => {
                done.fetch_add(file.weight, Ordering::Relaxed);
                return Ok(outcome);
            }
            Ok(None) => {}
            Err(e) => {
                done.fetch_add(file.weight, Ordering::Relaxed);
                return Err(e);
            }
        }
    }
    let src = file.source.path.clone();
    let dst = file.destination.clone();
    let cancel = cancel.clone();
    let weight = file.weight;
    let progress_handle = ProgressRelay::new(progress, Arc::clone(done), total_bytes);
    let relay = progress_handle.sender();
    let member = file.source.member.clone();
    let archive = if file.source_format == OutputFormat::SevenZip || file.source.archive_format.as_deref() == Some("7z") {
        crate::rom::sevenzip_wrapper::ArchiveFormat::SevenZ
    } else {
        crate::rom::sevenzip_wrapper::ArchiveFormat::Zip
    };
    let member_size = file.source.size;
    let copied = tokio::task::spawn_blocking(move || {
        let mut on_chunk = |n| {
            let _ = relay.send(n);
            !cancel.is_set()
        };
        match &member {
            Some(member) => unpack_member(&src, archive, member, member_size, &dst, &mut on_chunk),
            None => copy_with_progress(&src, &dst, &mut on_chunk),
        }
    })
    .await
    .map_err(|e| Error::internal(e.to_string()))?;
    let moved = progress_handle.finish().await;
    // Done or not, the file's bytes are accounted for once: what the relay moved, and the rest.
    done.fetch_add(weight.saturating_sub(moved), Ordering::Relaxed);
    match copied {
        Ok(bytes) => Ok(CopyOutcome::Copied { bytes, converted: false, payload_converted: false, content_id: None, read_back: None }),
        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Ok(CopyOutcome::Cancelled),
        Err(e) if is_fatal(&e) => Err(Error::unavailable(format!("the destination is full: {e}"))),
        Err(e) => Err(Error::internal(e.to_string())),
    }
}

/// Converts through `rom::operation::Single` into a per-file scratch directory, then moves
/// the outputs up.
///
/// `Ok(None)` means "this can't be converted here, copy it instead" -- the native library is
/// missing or the operation produced nothing. A conversion failure is a per-file error, never
/// the job's.
/// Whether this copy path converts payloads as well as containers.
///
/// One constant rather than a scattering of `false`s, because three places have to agree and
/// two of them fail silently if they do not: `carry_ra_hashes` carries a RetroAchievements
/// hash across a conversion *because* the payload is invariant, and `check_kind` picks the
/// member comparison for the same reason. Flipping this flips all three together.
const PAYLOAD_CONVERSION: bool = false;

async fn convert_one(
    app: &App,
    file: &PlannedCopy,
    settings: &OutputSettings,
    populate: crate::common::models::HashAlgorithms,
    container_flags: crate::rom::hash::HashFlags,
    verify: bool,
    cancel: &Cancel,
) -> Result<Option<CopyOutcome>> {
    use crate::rom::models::MetadataArgs;
    use crate::rom::operation::{OutputCheck, Single, SingleArgs, StageSwitches};

    let Some(archive) = super::archive_args(settings, file.platform, file.format, file.source_format)? else {
        return Ok(None);
    };
    let Some(parent) = file.destination.parent().map(Path::to_path_buf) else { return Ok(None) };
    std::fs::create_dir_all(&parent).map_err(|e| Error::internal(format!("can't create {}: {e}", parent.display())))?;
    // One scratch folder per conversion, not per process: files are converted side by side
    // (`copy_parallel_files`), and two discs of one game share a parent -- with one name, each
    // conversion's opening `remove_dir_all` deleted the other's half-written output.
    static SCRATCH_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SCRATCH_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let scratch = parent.join(format!(".recopy-{}-{seq}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).map_err(|e| Error::internal(format!("can't create {}: {e}", scratch.display())))?;

    let temp = super::super::storage::reserve_temp_dir(app, file.weight.saturating_mul(2)).await.ok().flatten();
    let temp_dir = temp.as_ref().map(|(choice, _)| PathBuf::from(&choice.dir.path));
    // The members are hashed off the stream being written, with the one quick algorithm. The
    // output's own identity is different bytes -- a CHD is not its tracks -- known only once it
    // is complete, so `Single` reads it back with the scan's hash set (and, verifying, decodes
    // its members on a second thread at the same time): one pass, not the two sequential ones
    // a decode followed by a raw hash used to cost.
    let hash_flags = crate::rom::hash::HashFlags::algorithm_to_flag(populate);
    let style = super::super::sheets::style_setting(app).await?;
    let args = SingleArgs {
        metadata: MetadataArgs { forced_platform: Some(file.platform), ..MetadataArgs::default() },
        // `convert: false` is load-bearing beyond this function: `carry_ra_hashes` carries a
        // rom's RetroAchievements hash across a conversion *because* only the container
        // changes here. Turning payload conversion on means recomputing it there.
        stages: StageSwitches { extract: true, hash: true, convert: PAYLOAD_CONVERSION, archive: true },
        hash_flags,
        archive,
        output_dir: Some(scratch.clone()),
        temp_dir,
        // Library defaults: CODING_RULES.md says nothing here hardcodes a thread cap.
        max_workers: None,
        output_check: Some(OutputCheck { flags: container_flags, members: verify && !PAYLOAD_CONVERSION }),
        ..SingleArgs::default()
    }
    .with_sheet_style(style);
    let input = file.source.path.clone();
    let flag = cancel.clone();
    let scratch_for_run = scratch.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        let _ = &scratch_for_run;
        Single::new(&input, args).run(&mut |_| !flag.is_set())
    })
    .await
    .map_err(|e| Error::internal(e.to_string()))?;

    let (produced, outcome_hashes, checked) = match outcome {
        Ok(o) => {
            let hashes: Vec<_> = o.outputs.iter().map(|f| f.get_hash_report().cloned()).collect();
            (o.outputs, hashes, o.checked_outputs)
        }
        Err(crate::rom::operation::SingleError::Cancelled) => {
            let _ = std::fs::remove_dir_all(&scratch);
            return Ok(Some(CopyOutcome::Cancelled));
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&scratch);
            return Err(Error::internal(format!("converting {}: {e}", file.source.path.display())));
        }
    };
    let mut bytes = 0u64;
    let mut moved = false;
    let mut content_id = None;
    let mut read_back = None;
    for (index, out) in produced.iter().enumerate() {
        let from = out.path().to_path_buf();
        let to = if produced.len() == 1 { file.destination.clone() } else { parent.join(from.file_name().map(|s| s.to_string_lossy().to_string()).map(|n| super::sanitize_segment(&n)).unwrap_or_default()) };
        if let Err(e) = std::fs::rename(&from, &to) {
            let _ = std::fs::remove_dir_all(&scratch);
            return Err(Error::internal(format!("moving {} into place: {e}", from.display())));
        }
        let written = std::fs::metadata(&to).map(|m| m.len()).unwrap_or(0);
        bytes += written;
        // The first output is the one the destination names; its content is what a
        // registered `local_files` row points at.
        if content_id.is_none() {
            read_back = checked.iter().find(|c| c.path == from).map(|c| Box::new(crate::rom::operation::CheckedOutput { path: to.clone(), ..c.clone() }));
            if let Some(report) = outcome_hashes.get(index).and_then(|r| r.as_ref()).filter(|r| !r.has_no_fallback()) {
                let name = to.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                match crate::database::content::upsert_content(&app.pool, file.platform, &name, written, report).await {
                    Ok((id, _)) => content_id = Some(id),
                    Err(e) => log::warn!("copy: couldn't record the converted content for {}: {e}", to.display()),
                }
            }
        }
        moved = true;
    }
    let _ = std::fs::remove_dir_all(&scratch);
    if !moved {
        // Not "copy it instead": the destination already carries the converted format's
        // name, so a plain copy would put the source's bytes under it -- a raw disc image
        // named .wia. A conversion that writes nothing is this file's failure.
        return Err(Error::internal(format!("converting {} to {} produced no output", file.source.path.display(), file.format.name())));
    }
    Ok(Some(CopyOutcome::Copied { bytes, converted: true, payload_converted: PAYLOAD_CONVERSION, content_id, read_back }))
}

/// A Wii U disc image (`.wud`/`.wux`) into Cemu's `.wua`, through `rom::cemu_wrapper`.
///
/// Not `Single`'s work: the archiver knows nothing of Wii U partitions and keys, and the
/// output is a set of decrypted files rather than the disc. So this step does what
/// `convert_one` gets from `Single` itself -- writes beside the destination and moves it into
/// place, hashes the result into its own content row -- and checks it the `.wua` way: every
/// file re-read against the manifest it was written with. The manifest also records the disc
/// as the catalogue knows it, its hashes and whether they matched a DAT, which is what lets a
/// later scan match the `.wua` to the same release.
#[allow(clippy::too_many_arguments)]
async fn convert_wua(
    app: &App,
    file: &PlannedCopy,
    populate: crate::common::models::HashAlgorithms,
    container_flags: crate::rom::hash::HashFlags,
    verify: bool,
    cancel: &Cancel,
    progress: &Progress,
    done: &Arc<std::sync::atomic::AtomicU64>,
    total_bytes: u64,
) -> Result<CopyOutcome> {
    use crate::rom::cemu_wrapper::{self, Source, SourceRecord, Verified};

    let src = file.source.path.clone();
    let dst = file.destination.clone();
    let key = cemu_wrapper::key_file_for(&src).and_then(|p| cemu_wrapper::read_key_file(&p));
    let keys_txt = src.parent().map(|d| d.join("keys.txt")).filter(|p| p.is_file());
    if key.is_none() && keys_txt.is_none() {
        return Err(Error::invalid(format!("{}: no disc key -- the .key file beside it (or a keys.txt in its folder) is needed to make a .wua", src.display())));
    }

    // What the catalogue knows of the disc, for the manifest.
    let hashes: Vec<(String, String)> = sqlx::query_as("SELECT algorithm, value_hex FROM hashes WHERE content_id = ?").bind(file.source.content_id).fetch_all(&app.pool).await?;
    let of = |algorithm: &str| hashes.iter().find(|(a, _)| a == algorithm).map(|(_, v)| v.to_ascii_lowercase());
    let matched: Option<u64> = sqlx::query_scalar("SELECT release_id FROM release_contents WHERE content_id = ? LIMIT 1").bind(file.source.content_id).fetch_optional(&app.pool).await?;
    let record = SourceRecord {
        name: src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        size: Some(file.source.size),
        crc32: of("crc32"),
        md5: of("md5"),
        sha1: of("sha1"),
        verified: matched.is_some(),
    };
    let record = serde_json::to_value(&record).map_err(|e| Error::internal(e.to_string()))?;

    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::internal(format!("can't create {}: {e}", parent.display())))?;
    }
    let part = part_path(&dst);
    let weight = file.weight;
    let relay_handle = ProgressRelay::new(progress, Arc::clone(done), total_bytes);
    let relay = relay_handle.sender();
    let flag = cancel.clone();
    let (src_run, dst_run, part_run) = (src.clone(), dst.clone(), part.clone());
    let flags = container_flags | crate::rom::hash::HashFlags::algorithm_to_flag(populate);
    let result = tokio::task::spawn_blocking(move || -> std::result::Result<(u64, crate::rom::hash::HashReport), String> {
        // Progress in the disc's own bytes: the job's bar is sized by them.
        let mut sent = 0u64;
        cemu_wrapper::create(&[Source { path: src_run.clone(), key }], keys_txt.as_deref(), Some(&record), &part_run, |d, t| {
            let now = if t > 0 { (weight as u128 * d as u128 / t as u128) as u64 } else { 0 };
            if now > sent {
                let _ = relay.send(now - sent);
                sent = now;
            }
            !flag.is_set()
        })
        .map_err(|e| if e.status == cemu_wrapper::Status::Cancelled { "cancelled".to_string() } else { format!("converting {}: {e}", src_run.display()) })?;
        if verify {
            match cemu_wrapper::verify(&part_run, |_, _| !flag.is_set()).map_err(|e| e.to_string())? {
                Verified::Intact(_) => {}
                Verified::Mismatch { path, reason, .. } => return Err(format!("{}: read back wrong: {path}: {reason}", dst_run.display())),
                Verified::NoManifest => return Err(format!("{}: read back without its manifest", dst_run.display())),
            }
        }
        let report = crate::rom::hash::hash(&part_run, &crate::rom::hash::HashOptions { flags, ..Default::default() }, |_| {}).map_err(|e| format!("hashing {}: {e}", part_run.display()))?;
        std::fs::rename(&part_run, &dst_run).map_err(|e| format!("moving {} into place: {e}", part_run.display()))?;
        let bytes = std::fs::metadata(&dst_run).map(|m| m.len()).unwrap_or(0);
        Ok((bytes, report))
    })
    .await
    .map_err(|e| Error::internal(e.to_string()))?;
    let moved = relay_handle.finish().await;
    done.fetch_add(weight.saturating_sub(moved), Ordering::Relaxed);
    let (bytes, report) = match result {
        Ok(r) => r,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            if e == "cancelled" {
                return Ok(CopyOutcome::Cancelled);
            }
            return Err(Error::internal(e));
        }
    };
    let name = dst.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let content_id = match crate::database::content::upsert_content(&app.pool, file.platform, &name, bytes, &report).await {
        Ok((id, _)) => {
            // The disc it holds, as a scan of the .wua would link it.
            let disc = src.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            if let Err(e) = crate::database::content::link_member(&app.pool, id, file.source.content_id, Some(&disc)).await {
                log::warn!("copy: couldn't link {} to its disc: {e}", dst.display());
            }
            Some(id)
        }
        Err(e) => {
            log::warn!("copy: couldn't record the converted content for {}: {e}", dst.display());
            None
        }
    };
    Ok(CopyOutcome::Copied { bytes, converted: true, payload_converted: PAYLOAD_CONVERSION, content_id, read_back: None })
}

/// An Xbox 360 disc into Xenia's `.zar`, an Xbox disc into an XISO, or a PS2/PSP ISO into a
/// CSO: the conversions this project does itself rather than through `Single`.
///
/// Same shape as [`convert_wua`]: written beside the destination, checked, hashed into its
/// own content row, moved into place. The check is each format's own: a `.zar` against the
/// manifest it was written with, an XISO file by file against the disc it came from, and a
/// CSO decoded back to the ISO and compared with the hash the catalogue has for it.
///
/// `Ok(None)` is "copy it as it is": an Xbox image that is already an XISO.
#[allow(clippy::too_many_arguments)]
async fn convert_disc(
    app: &App,
    file: &PlannedCopy,
    populate: crate::common::models::HashAlgorithms,
    container_flags: crate::rom::hash::HashFlags,
    verify: bool,
    cancel: &Cancel,
    progress: &Progress,
    done: &Arc<std::sync::atomic::AtomicU64>,
    total_bytes: u64,
) -> Result<Option<CopyOutcome>> {
    use crate::rom::cemu_wrapper::{SourceRecord, Verified};
    use crate::rom::hash::{HashDigest, HashFlags, HashOptions};
    use crate::rom::source::ReadAt;
    use crate::rom::{cso, xbox};

    let src = file.source.path.clone();
    let dst = file.destination.clone();
    let format = file.format;
    if format == OutputFormat::Xiso {
        let base = std::fs::File::open(&src).and_then(|mut f| xbox::open_volume(&mut f)).ok().flatten().map(|v| v.base);
        match base {
            Some(0) => return Ok(None),
            Some(_) => {}
            None => return Err(Error::invalid(format!("{}: not an Xbox disc image", src.display()))),
        }
    }

    // What the catalogue knows of the disc: the .zar's manifest records it, a CSO is checked
    // against it.
    let hashes: Vec<(String, String)> = sqlx::query_as("SELECT algorithm, value_hex FROM hashes WHERE content_id = ?").bind(file.source.content_id).fetch_all(&app.pool).await?;
    let of = |algorithm: &str| hashes.iter().find(|(a, _)| a == algorithm).map(|(_, v)| v.to_ascii_lowercase());
    let known = (of("crc32"), of("md5"), of("sha1"));
    let matched: Option<u64> = sqlx::query_scalar("SELECT release_id FROM release_contents WHERE content_id = ? LIMIT 1").bind(file.source.content_id).fetch_optional(&app.pool).await?;
    let source_name = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();

    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::internal(format!("can't create {}: {e}", parent.display())))?;
    }
    let part = part_path(&dst);
    let weight = file.weight;
    let relay_handle = ProgressRelay::new(progress, Arc::clone(done), total_bytes);
    let relay = relay_handle.sender();
    let flag = cancel.clone();
    let (src_run, dst_run, part_run) = (src.clone(), dst.clone(), part.clone());
    let flags = container_flags | HashFlags::algorithm_to_flag(populate);
    let size = file.source.size;
    let verified = matched.is_some();
    let result = tokio::task::spawn_blocking(move || -> std::result::Result<(u64, crate::rom::hash::HashReport), String> {
        // Progress in the disc's own bytes: the job's bar is sized by them.
        let mut sent = 0u64;
        let mut report = |d: u64, t: u64| {
            let now = if t > 0 { (weight as u128 * d.min(t) as u128 / t as u128) as u64 } else { 0 };
            if now > sent {
                let _ = relay.send(now - sent);
                sent = now;
            }
            !flag.is_set()
        };
        match format {
            OutputFormat::Zar => {
                let record = |image: &crate::rom::hash::HashReport, image_size: u64| {
                    // The catalogue's hashes where it has them, the pass's own otherwise.
                    let record = SourceRecord {
                        name: source_name.clone(),
                        size: Some(image_size),
                        crc32: known.0.clone().or_else(|| image.crc32.clone()),
                        md5: known.1.clone().or_else(|| image.md5.clone()),
                        sha1: known.2.clone().or_else(|| image.sha1.clone()),
                        verified,
                    };
                    serde_json::to_value(&record).unwrap_or_default()
                };
                xbox::create_zar(&src_run, &part_run, record, &mut report).map_err(|e| if e == "cancelled" { e } else { format!("converting {}: {e}", src_run.display()) })?;
                if verify {
                    match xbox::verify_zar(&part_run, |_, _| !flag.is_set()).map_err(|e| e.to_string())? {
                        Verified::Intact(_) => {}
                        Verified::Mismatch { path, reason, .. } => return Err(format!("{}: read back wrong: {path}: {reason}", dst_run.display())),
                        Verified::NoManifest => return Err(format!("{}: read back without its manifest", dst_run.display())),
                    }
                }
            }
            OutputFormat::Xiso => {
                xbox::rewrite_xiso(&src_run, &part_run, &mut report).map_err(|e| if e == "cancelled" { e } else { format!("converting {}: {e}", src_run.display()) })?;
                if verify {
                    if let Some((path, reason)) = xbox::compare_files(&src_run, &part_run, || !flag.is_set())? {
                        return Err(format!("{}: read back wrong: {path}: {reason}", dst_run.display()));
                    }
                }
            }
            OutputFormat::Cso => {
                cso::write(&src_run, &part_run, 9, &mut report)?;
                if verify {
                    // Decoded back and hashed with whichever algorithm the catalogue has.
                    let (want_flag, want) = match &known {
                        (_, _, Some(sha1)) => (HashFlags::SHA1, sha1.clone()),
                        (_, Some(md5), _) => (HashFlags::MD5, md5.clone()),
                        (Some(crc), _, _) => (HashFlags::CRC32, crc.clone()),
                        _ => (HashFlags::empty(), String::new()),
                    };
                    let mut reader = cso::CsoReader::open(&part_run).map_err(|e| format!("{}: {e}", part_run.display()))?;
                    if reader.size() != size {
                        return Err(format!("{}: read back {} bytes, the ISO has {size}", dst_run.display(), reader.size()));
                    }
                    if !want_flag.is_empty() {
                        let digest = HashDigest::new(want_flag);
                        let mut buf = vec![0u8; 4 << 20];
                        let mut at = 0u64;
                        while at < size {
                            if flag.is_set() {
                                return Err("cancelled".into());
                            }
                            let n = ((size - at) as usize).min(buf.len());
                            reader.read_at(at, &mut buf[..n]).map_err(|e| format!("{}: {e}", part_run.display()))?;
                            digest.update(&buf[..n]).map_err(|e| e.to_string())?;
                            at += n as u64;
                        }
                        let got = digest.finish();
                        let got = [got.sha1, got.md5, got.crc32].into_iter().flatten().next().unwrap_or_default();
                        if !got.eq_ignore_ascii_case(&want) {
                            return Err(format!("{}: read back wrong: the ISO inside does not match the original", dst_run.display()));
                        }
                    }
                }
            }
            other => return Err(format!("{} is not converted here", other.name())),
        }
        let hashed = crate::rom::hash::hash(&part_run, &HashOptions { flags, ..Default::default() }, |_| {}).map_err(|e| format!("hashing {}: {e}", part_run.display()))?;
        std::fs::rename(&part_run, &dst_run).map_err(|e| format!("moving {} into place: {e}", part_run.display()))?;
        let bytes = std::fs::metadata(&dst_run).map(|m| m.len()).unwrap_or(0);
        Ok((bytes, hashed))
    })
    .await
    .map_err(|e| Error::internal(e.to_string()))?;
    let moved = relay_handle.finish().await;
    done.fetch_add(weight.saturating_sub(moved), Ordering::Relaxed);
    let (bytes, report) = match result {
        Ok(r) => r,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            if e == "cancelled" {
                return Ok(Some(CopyOutcome::Cancelled));
            }
            return Err(Error::internal(e));
        }
    };
    let name = dst.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let content_id = match crate::database::content::upsert_content(&app.pool, file.platform, &name, bytes, &report).await {
        Ok((id, _)) => {
            // A .zar holds the disc's files and a CSO the disc itself, as a scan of either
            // links them. An XISO holds neither: it is a different image of the same game.
            if format != OutputFormat::Xiso {
                let disc = src.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                if let Err(e) = crate::database::content::link_member(&app.pool, id, file.source.content_id, Some(&disc)).await {
                    log::warn!("copy: couldn't link {} to its disc: {e}", dst.display());
                }
            }
            Some(id)
        }
        Err(e) => {
            log::warn!("copy: couldn't record the converted content for {}: {e}", dst.display());
            None
        }
    };
    Ok(Some(CopyOutcome::Copied { bytes, converted: true, payload_converted: PAYLOAD_CONVERSION, content_id, read_back: None }))
}

/// Relays byte counts from the blocking copy back into the async `Progress` writer, which is
/// the only side that may touch the database.
/// Carries one file's progress from its blocking copy thread into the job's shared byte
/// count: each message is bytes moved since the last, added to `done` and reported.
struct ProgressRelay {
    tx: tokio::sync::mpsc::UnboundedSender<u64>,
    handle: tokio::task::JoinHandle<u64>,
}

impl ProgressRelay {
    fn new(progress: &Progress, done: Arc<std::sync::atomic::AtomicU64>, total: u64) -> Self {
        // A tokio channel, not `std::sync::mpsc`: the receiver awaits instead of parking an
        // executor thread, and `send` on the unbounded sender is synchronous and non-blocking,
        // which is what the copy thread needs.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<u64>();
        // `Progress` is not `Clone`, and the relay only needs to write; a second `Progress`
        // on the same job row is the simplest thing that works.
        let relay_progress = progress.duplicate();
        let handle = tokio::spawn(async move {
            let mut moved = 0u64;
            while let Some(n) = rx.recv().await {
                moved += n;
                let now = done.fetch_add(n, Ordering::Relaxed) + n;
                relay_progress.report(now, Some(total), "copying").await;
            }
            moved
        });
        Self { tx, handle }
    }

    fn sender(&self) -> tokio::sync::mpsc::UnboundedSender<u64> {
        self.tx.clone()
    }

    /// Bytes this file moved.
    async fn finish(self) -> u64 {
        drop(self.tx);
        self.handle.await.unwrap_or(0)
    }
}

/// What checking one copied file found, for the job to fold into its summary.
#[derive(Default)]
struct Checked {
    /// Bytes as they should be: registered as held.
    good: bool,
    /// The content the registered row points at (`None` = the source's own).
    identity: Option<u64>,
    verified: bool,
    recorded: bool,
    content_checked: bool,
    full_hashed: bool,
    archive_hashed: bool,
    unverifiable: bool,
    verify_failed: Option<String>,
    content_mismatched: Vec<String>,
    errors: Vec<String>,
    rehashed_dir: Option<PathBuf>,
}

/// Checks one copied file the way `check_kind` says this copy calls for. What the copy did to
/// the bytes decides what checking them can mean -- three different questions, and asking the
/// wrong one gives a confident wrong answer rather than an error.
#[allow(clippy::too_many_arguments)]
async fn check_copy(
    app: &App,
    file: &PlannedCopy,
    verify: bool,
    converted: bool,
    payload_converted: bool,
    content_id: Option<u64>,
    read_back: Option<&crate::rom::operation::CheckedOutput>,
    populate: crate::common::models::HashAlgorithms,
    container_flags: crate::rom::hash::HashFlags,
) -> Checked {
    let mut c = Checked { good: true, identity: content_id, ..Default::default() };
    let where_ = file.destination.display().to_string();
    match check_kind(verify, file, converted, payload_converted) {
        CheckKind::None => {}
        // A copy already knows what it copied, so this confirms the bytes landed rather than
        // working out what they are -- one algorithm, once.
        CheckKind::Whole => match verify_one(app, file, content_id, populate).await {
            Ok(super::CheckOutcome::Matched { recorded, .. }) => {
                c.verified = true;
                c.recorded = recorded;
            }
            Ok(super::CheckOutcome::Mismatched { .. }) => {
                c.good = false;
                c.verify_failed = Some(where_);
            }
            // Nothing recorded to compare against, so nothing is concluded and nothing is
            // written. The file is still registered: it is as good as the row it came from,
            // which is all a copy ever claims.
            Ok(super::CheckOutcome::Unverifiable) => c.unverifiable = true,
            Err(e) => c.errors.push(format!("{where_}: {}", e.message)),
        },
        CheckKind::Content => match content_check::check(app, &file.destination, file.source.content_id, file.platform, container_flags, read_back).await {
            Ok(content_check::ContentOutcome::Matched { content_id: container, hashed, .. }) => {
                c.content_checked = true;
                c.verified = true;
                // The container read its whole self to answer, with the scan's hash set, so
                // it is fully hashed in exactly the sense `FileData::hash` means.
                c.full_hashed = true;
                // Hashed whole either here or already in the conversion's read-back, which
                // recorded the row this check then found.
                c.archive_hashed = hashed || read_back.is_some_and(|r| r.container.is_ok());
                // The new container describes different bytes from the source, so the
                // registered row points at its own content, not the source's. `convert_one`'s
                // quick-hash row is superseded by this fuller one.
                c.identity = Some(container);
            }
            Ok(content_check::ContentOutcome::Mismatched { missing }) => {
                c.good = false;
                c.content_mismatched = missing.into_iter().map(|m| format!("{where_}: {m}")).collect();
                c.verify_failed = Some(where_);
            }
            Ok(content_check::ContentOutcome::NoMembers) => c.unverifiable = true,
            Err(e) => c.errors.push(format!("{where_}: {}", e.message)),
        },
        CheckKind::Rehash => match content_check::rehash(app, &file.destination, file.platform, container_flags).await {
            Ok(id) => {
                c.identity = Some(id);
                c.full_hashed = true;
                c.archive_hashed = true;
                c.rehashed_dir = file.destination.parent().map(Path::to_path_buf);
            }
            Err(e) => {
                c.good = false;
                c.errors.push(format!("{where_}: {}", e.message));
            }
        },
    }
    c
}

/// Reads one copied file back and checks it against what the catalogue records.
///
/// Not "what is this file?" -- a copy already knows -- but "did these bytes land?". So the
/// comparison is against a hash the catalogue *already had* for that content -- the
/// configured `copy_populate_hash` where it has that one, else the cheapest to compute here,
/// with the configured one filled in off the same read only once that comparison has passed. A hash taken from a copy nobody has confirmed must
/// never become the catalogue's idea of what the content is: `hashes` hangs off `content`,
/// which the original file shares, so writing one there on the strength of a bad copy would
/// go on to declare the original corrupt.
async fn verify_one(app: &App, file: &PlannedCopy, converted: Option<u64>, populate: crate::common::models::HashAlgorithms) -> Result<super::CheckOutcome> {
    let content_id = converted.unwrap_or(file.source.content_id);
    Ok(super::check_file(app, &file.destination, content_id, populate).await?.0)
}

/// Records what the copy wrote in this machine's own ownership tables.
///
/// **This does not hash anything**, and that is the point. A copy carries each file's
/// `content_id` from the row it came from, and `content` is shared catalogue data -- the same
/// id means the same bytes on every machine that synced the same catalogue. Re-deriving that
/// identity by re-reading and re-hashing every byte, which is what handing the job to a
/// `file_scan` did, is work whose answer was already in hand. Checking that the bytes
/// actually landed is a different and much cheaper question, and it is `verify`'s.
///
/// The rows themselves go through `api::scan`'s own writers, so there is one implementation
/// of what a `local_files` / `local_rom` / `local_game` row looks like, not two.
///
/// A converted output is the exception: its bytes are new, so `convert_one` hashed them with
/// the one quick algorithm and made a `content` row, and that id is used here.
async fn register(
    app: &App,
    planned: &plan::Planned,
    copied: &[(&PlannedCopy, Option<u64>)],
    incomplete: &std::collections::HashSet<usize>,
    verified: &std::collections::HashSet<(usize, u64)>,
    full_hashed: &std::collections::HashSet<(usize, u64)>,
) -> Result<(u64, u64)> {
    use std::collections::HashMap;

    if copied.is_empty() {
        return Ok((0, 0));
    }
    // Which configured directory governs each destination; a file outside every one of them
    // cannot be registered (`jobs::validate` refuses that up front, but a copy spanning two
    // platform folders can still straddle a boundary).
    let mut dir_of: HashMap<std::path::PathBuf, Option<u64>> = HashMap::new();
    let mut per_item: HashMap<usize, Vec<u64>> = HashMap::new();
    let mut registered = 0u64;
    let mut ra_carried = 0u64;
    // How many of each item's files actually landed. A file skipped at plan time -- already
    // held elsewhere on this machine, or with no mount to read it through -- never reaches
    // `copied`, so comparing these two counts is what tells a whole rom from a partial one.
    // A folded-in track is excluded from both sides: its bytes are inside the carrier's
    // output, so a cue+bin that became one CHD is whole with one file, not two-thirds missing.
    let mut landed: HashMap<usize, usize> = HashMap::new();
    for (file, _) in copied {
        if !file.is_playlist && !file.folded_into_carrier {
            *landed.entry(file.item).or_default() += 1;
        }
    }
    let wanted: HashMap<usize, usize> = planned.files.iter().filter(|f| !f.is_playlist && !f.folded_into_carrier).fold(HashMap::new(), |mut m, f| {
        *m.entry(f.item).or_default() += 1;
        m
    });

    for (file, converted) in copied {
        let Some(parent) = file.destination.parent().map(Path::to_path_buf) else { continue };
        let directory_id = match dir_of.get(&parent) {
            Some(id) => *id,
            None => {
                let resolved = crate::database::directories::resolve(&app.pool, &parent.display().to_string()).await?.directory.map(|d| d.id);
                dir_of.insert(parent.clone(), resolved);
                resolved
            }
        };
        let Some(directory_id) = directory_id else { continue };
        // The source's own content, which is the whole reason this needs no hashing: the id
        // identifies the bytes, and every `hashes` row already hanging off it -- crc32, md5,
        // sha1, whatever the other machine recorded -- applies to this copy unchanged,
        // because it is the same content row.
        // A conversion with no identity of its own (an output found already there and taken
        // as done, or one whose content row could not be written) is not the source's
        // content: left for the next scan to record rather than recorded as the source.
        if converted.is_none() && file.format != OutputFormat::Same && file.format != file.source_format && file.source.member.is_none() {
            continue;
        }
        let content_id = converted.unwrap_or(file.source.content_id);
        let size = std::fs::metadata(&file.destination).map(|m| m.len()).unwrap_or(file.source.size);
        let modified = std::fs::metadata(&file.destination).ok().and_then(|m| m.modified().ok()).and_then(|t| chrono::DateTime::<chrono::Utc>::from(t).naive_utc().into());
        // An unpacked member is a loose file now.
        let archive_format = if file.source.member.is_some() { None } else { file.format.archive_tag().or(file.source.archive_format.as_deref()) };
        // Each stamp is earned separately, the same way `FileData::hash` and
        // `FileData::integrity` earn theirs. A plain copy gets neither: nothing was hashed,
        // the identity came with the content row. A verified copy read its output back, so it
        // gets `integrity`. A content-checked or rehashed one read its output back *with the
        // full hash set*, so it gets both -- and that `full` stamp is what makes the next
        // integrity check a cheap whole-file comparison rather than another decode.
        let key = (file.item, file.source.file_id);
        let checks = crate::api::scan::FileChecks { full: full_hashed.contains(&key), integrity: verified.contains(&key) };
        let file_id = crate::api::scan::upsert_local_file(&app.pool, &file.destination.display().to_string(), content_id, directory_id, archive_format, size, modified, checks).await?;
        per_item.entry(file.item).or_default().push(file_id);
        registered += 1;
    }

    for (index, file_ids) in per_item {
        let Some(item) = planned.items.get(index) else { continue };
        // The rom's own main file at its new home: the destination of whichever planned entry
        // was the item's `main_path`, else the first one that landed.
        let main = planned
            .files
            .iter()
            .find(|f| f.item == index && !f.is_playlist && f.source.raw_path == item.main_path)
            .or_else(|| planned.files.iter().find(|f| f.item == index && !f.is_playlist))
            .map(|f| f.destination.display().to_string());
        let Some(main) = main else { continue };
        let (rom_id, _) = crate::api::scan::upsert_rom(&app.pool, &main, item.platform, &file_ids).await?;
        let (game_id, _) = crate::api::scan::ensure_game(&app.pool, rom_id, &item.game_name).await?;
        // The release the copy came from, carried straight over: a copied game shows as held
        // immediately, with no hash match to wait for. That link is the one thing a
        // `file_scan` could not have reconstructed without re-matching it.
        if let Some(release_id) = item.release_id {
            sqlx::query("INSERT IGNORE INTO local_release_games (release_id, game_id) VALUES (?, ?)").bind(release_id).bind(game_id).execute(&app.pool).await?;
        }
        let whole = !incomplete.contains(&index) && landed.get(&index).copied().unwrap_or(0) == wanted.get(&index).copied().unwrap_or(usize::MAX);
        ra_carried += carry_ra_hashes(app, item, rom_id, !whole).await?;
    }
    Ok((registered, ra_carried))
}

/// Gives the new rom the RetroAchievements hashes the source rom had.
///
/// The scalar hashes need no copying at all -- they hang off `content`, and the copy reuses
/// the source's `content_id`, so crc32/md5/sha1/xxh3 all apply to the new file the moment its
/// row exists. `ra_hash` is the exception the schema calls out: it is computed per *playable
/// unit* rather than per payload (for some platforms it spans several files), so it hangs off
/// the rom through this junction and has to be carried across. Without it a copied game shows
/// no RetroAchievements support until something rehashes it, which is exactly the expensive
/// thing this whole path exists to avoid.
///
/// **Re-containering does not change it**, which is why a converted rom carries it too. The
/// RetroAchievements hash is computed from the *decoded* content, not from the container --
/// a scan reads a CHD's or RVZ's content in memory for it (`rom::ra_disc`), exactly as
/// RAHasher reads a `.cue`'s `.bin` tracks -- and this copy only ever changes the container: `convert_one`
/// runs with `StageSwitches { convert: false, .. }`, so the payload bytes are invariant by
/// construction. A cue+bin that became one CHD is the same playable unit with the same hash.
/// If payload conversion is ever switched on here (header add/strip, N64 byte order), that
/// stops holding and this must recompute instead of carrying.
///
/// One refusal remains: an **incomplete** rom -- one whose files did not all land, whether
/// because a copy failed, verification failed, or one was skipped as already held elsewhere
/// -- is not the playable unit the hash describes, so it cannot claim it. A track folded into
/// a disc container does not count as missing; its bytes are in the output.
async fn carry_ra_hashes(app: &App, item: &plan::CopyItem, rom_id: u64, incomplete: bool) -> Result<u64> {
    let Some(source) = item.rom else { return Ok(0) };
    if incomplete {
        return Ok(0);
    }
    let hash_ids: Vec<u64> = match source.source_id {
        None => sqlx::query_scalar("SELECT hash_id FROM local_rom_rahash WHERE rom_id = ?").bind(source.id).fetch_all(&app.pool).await?,
        Some(sid) => sqlx::query_scalar("SELECT hash_id FROM remote_rom_rahash WHERE source_id = ? AND rom_id = ?").bind(sid).bind(source.id).fetch_all(&app.pool).await?,
    };
    let mut carried = 0;
    for hash_id in hash_ids {
        // `hashes` is shared and unprefixed, so a remote rom's ra_hash row is the very same
        // row a local one would point at -- the junction is all that differs.
        let done = sqlx::query("INSERT IGNORE INTO local_rom_rahash (rom_id, hash_id) VALUES (?, ?)").bind(rom_id).bind(hash_id).execute(&app.pool).await?;
        carried += done.rows_affected();
    }
    Ok(carried)
}

/// The jobs list's one-line version of a summary.
pub fn one_liner(s: &CopySummary) -> String {
    let mut parts = vec![format!("{} file(s), {}", s.files_copied, crate::database::jobs::human_bytes(s.bytes_copied))];
    if s.files_converted > 0 {
        parts.push(format!("{} converted", s.files_converted));
    }
    if s.files_skipped_same > 0 {
        parts.push(format!("{} already there", s.files_skipped_same));
    }
    if s.verified > 0 {
        parts.push(format!("{} verified", s.verified));
    }
    if s.content_checked > 0 {
        parts.push(format!("{} checked inside", s.content_checked));
    }
    if !s.verify_failed.is_empty() {
        parts.push(format!("{} FAILED verification", s.verify_failed.len()));
    }
    if !s.blocked_existing.is_empty() {
        parts.push(format!("{} refused (already there)", s.blocked_existing.len()));
    }
    if !s.kept_existing.is_empty() {
        parts.push(format!("{} skipped (a different file is there)", s.kept_existing.len()));
    }
    if !s.rehash_jobs.is_empty() {
        parts.push(format!("{} scan job(s) queued", s.rehash_jobs.len()));
    }
    if s.registered > 0 {
        parts.push(format!("{} registered", s.registered));
    }
    if s.ra_hashes_carried > 0 {
        parts.push(format!("{} RA hash(es) carried", s.ra_hashes_carried));
    }
    if s.already_held > 0 {
        parts.push(format!("{} already held", s.already_held));
    }
    if s.deduped > 0 {
        parts.push(format!("{} duplicate(s)", s.deduped));
    }
    if !s.errors.is_empty() {
        parts.push(format!("{} error(s)", s.errors.len()));
    }
    if s.cancelled {
        parts.push("cancelled".into());
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planned(is_playlist: bool) -> PlannedCopy {
        PlannedCopy {
            source: plan::SourceFile { owner: None, file_id: 1, path: "/a/x.zip".into(), raw_path: "/a/x.zip".into(), content_id: 7, size: 10, archive_format: Some("zip".into()), member: None },
            weight: 10,
            destination: "/b/x.7z".into(),
            platform: crate::common::models::Platform::Snes,
            format: OutputFormat::SevenZip,
            source_format: OutputFormat::Zip,
            item: 0,
            is_playlist,
            folded_into_carrier: false,
            skip: None,
        }
    }

    /// The three checks answer three different questions, and the dispatch is what keeps a
    /// confident wrong answer off the table.
    #[test]
    fn what_the_copy_did_to_the_bytes_picks_the_check() {
        let f = planned(false);
        assert_eq!(check_kind(true, &f, false, false), CheckKind::Whole, "same bytes: compare the file");
        assert_eq!(check_kind(true, &f, true, false), CheckKind::Content, "new container, same payload: compare what's inside");
        assert_eq!(check_kind(true, &f, true, true), CheckKind::Rehash, "new payload: nothing to compare, so identify it instead");
    }

    /// Declining the check declines a comparison. It cannot decline an identity: a
    /// payload-converted file that is not hashed is not anything.
    #[test]
    fn verification_off_still_hashes_a_converted_payload() {
        let f = planned(false);
        assert_eq!(check_kind(false, &f, false, false), CheckKind::None);
        assert_eq!(check_kind(false, &f, true, false), CheckKind::None);
        assert_eq!(check_kind(false, &f, true, true), CheckKind::Rehash);
    }

    /// A rewritten `.m3u` is this app's own text, not the content it names.
    #[test]
    fn a_playlist_is_never_checked() {
        let f = planned(true);
        assert_eq!(check_kind(true, &f, false, false), CheckKind::None);
        assert_eq!(check_kind(true, &f, true, true), CheckKind::None);
    }

    /// Three places have to agree about payload conversion, and two fail silently if they do
    /// not. This is the tripwire for the day the constant flips.
    #[test]
    fn payload_conversion_is_off_and_the_ra_hash_carry_depends_on_it() {
        assert!(!PAYLOAD_CONVERSION, "flipping this means carry_ra_hashes must recompute, not carry -- see its doc comment");
    }

    #[test]
    fn a_playlist_names_a_disc_in_its_own_folder_by_its_path() {
        use std::path::Path;
        assert_eq!(playlist_line(Path::new("/out/pc98/Hoge.m3u"), Path::new("/out/pc98/Hoge (Disk 1)/Hoge (Disk 1).dsk")), "Hoge (Disk 1)/Hoge (Disk 1).dsk");
        assert_eq!(playlist_line(Path::new("/out/psx/Hoge.m3u"), Path::new("/out/psx/Hoge (Disc 2).chd")), "Hoge (Disc 2).chd");
        assert_eq!(playlist_line(Path::new("/out/psx/Hoge.m3u"), Path::new("/elsewhere/Hoge (Disc 2).chd")), "Hoge (Disc 2).chd");
    }

    #[test]
    fn m3u_lines_follow_the_names_we_produced() {
        let produced = [("Disc 1.cue".to_string(), "Disc 1.chd".to_string()), ("Disc 2.cue".to_string(), "Disc 2.chd".to_string())];
        let (out, unmatched) = rewrite_m3u("Disc 1.cue\nDisc 2.cue\n", &produced);
        assert_eq!(out, "Disc 1.chd\nDisc 2.chd\n");
        assert!(unmatched.is_empty());
    }

    #[test]
    fn m3u_line_for_an_unpacked_zip_names_its_sheet() {
        let produced = [("Disc 1.bin".to_string(), "/out/Disc 1.bin".to_string()), ("Disc 1.cue".to_string(), "/out/Disc 1.cue".to_string())];
        let (out, unmatched) = rewrite_m3u("Disc 1.zip\n", &produced);
        assert_eq!(out, "/out/Disc 1.cue\n");
        assert!(unmatched.is_empty());
    }

    #[test]
    fn m3u_comments_and_blank_lines_survive() {
        let produced = [("a.cue".to_string(), "a.chd".to_string())];
        let (out, _) = rewrite_m3u("# a playlist\n\na.cue\n", &produced);
        assert_eq!(out, "# a playlist\n\na.chd\n");
    }

    /// A playlist this app didn't create must not be mangled by a bad guess.
    #[test]
    fn an_unmatched_line_is_left_alone_and_reported() {
        let produced = [("a.cue".to_string(), "a.chd".to_string())];
        let (out, unmatched) = rewrite_m3u("a.cue\nsomething else.bin\n", &produced);
        assert_eq!(out, "a.chd\nsomething else.bin\n");
        assert_eq!(unmatched, vec!["something else.bin".to_string()]);
    }

    #[test]
    fn a_line_keeps_the_file_it_names_when_a_bin_shares_its_stem() {
        // Copied in whichever order: the bin finished first.
        let produced = [("Disc 1.bin".to_string(), "Disc 1.bin".to_string()), ("Disc 1.cue".to_string(), "Disc 1.cue".to_string())];
        assert_eq!(rewrite_m3u("Disc 1.cue
", &produced).0, "Disc 1.cue
");
        // Converted: the cue became a CHD and the bin went into it.
        let produced = [("Disc 1.cue".to_string(), "Disc 1.chd".to_string())];
        assert_eq!(rewrite_m3u("Disc 1.cue
", &produced).0, "Disc 1.chd
");
    }

    #[test]
    fn m3u_matching_ignores_directories_and_case() {
        let produced = [("Disc 1.CUE".to_string(), "Disc 1.chd".to_string())];
        let (out, unmatched) = rewrite_m3u("./subdir/disc 1.cue\n", &produced);
        assert_eq!(out, "Disc 1.chd\n");
        assert!(unmatched.is_empty());
    }

    #[test]
    fn the_part_file_sits_beside_its_target() {
        assert_eq!(part_path(Path::new("/a/b/game.chd")), PathBuf::from("/a/b/game.chd.recopy-part"));
    }

    #[test]
    fn a_small_copy_is_byte_for_byte_and_leaves_no_part_file() {
        let dir = std::env::temp_dir().join(format!("recopy-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("in.bin");
        let dst = dir.join("out.bin");
        std::fs::write(&src, b"hello rom").unwrap();
        let mut seen = 0u64;
        let n = copy_with_progress(&src, &dst, &mut |b| {
            seen += b;
            true
        })
        .unwrap();
        assert_eq!(n, 9);
        assert_eq!(seen, 9);
        assert_eq!(std::fs::read(&dst).unwrap(), b"hello rom");
        assert!(!part_path(&dst).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cancelled_copy_leaves_neither_a_target_nor_a_part_file() {
        let dir = std::env::temp_dir().join(format!("recopy-cancel-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("in.bin");
        let dst = dir.join("out.bin");
        std::fs::write(&src, b"hello rom").unwrap();
        let err = copy_with_progress(&src, &dst, &mut |_| false).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
        assert!(!dst.exists());
        assert!(!part_path(&dst).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_part_file_is_swept() {
        let dir = std::env::temp_dir().join(format!("recopy-sweep-{}", std::process::id()));
        let sub = dir.join("nested");
        let _ = std::fs::create_dir_all(&sub);
        let age = |path: &Path| {
            let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
            std::fs::File::options().write(true).open(path).unwrap().set_modified(old).unwrap();
        };
        let stale = sub.join("x.chd.recopy-part");
        std::fs::write(&stale, b"half").unwrap();
        age(&stale);
        // Another job's, being written right now.
        let fresh = sub.join("y.chd.recopy-part");
        std::fs::write(&fresh, b"half").unwrap();
        let keep = sub.join("x.chd");
        std::fs::write(&keep, b"whole").unwrap();
        // A cancelled conversion's scratch folder.
        let scratch = sub.join(".recopy-1-1");
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::write(scratch.join("Game.chd"), b"half").unwrap();
        age(&scratch.join("Game.chd"));
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::open(&scratch).unwrap().set_modified(when).unwrap();
        sweep_parts(&dir, 0);
        assert!(!stale.exists());
        assert!(fresh.exists());
        assert!(keep.exists());
        assert!(!scratch.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
