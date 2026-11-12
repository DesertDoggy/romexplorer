//! Music inside ROMs, played on title pages like a CD player (`rom::rom_audio`).
//!
//! - **Indexing** (`rom_audio_index` job, [`run_index`]): each disc image or dumped folder is
//!   opened and every track found is recorded -- where it is and what it is, never the
//!   audio itself.
//! - **Playing** ([`serve`]): a browser cannot play a console's codecs, so a track is
//!   decoded on its first play (two loops and a fade, as a game plays it) and kept in the
//!   audio RAM cache (`api::audio_ram`, `audio_ram_cache_mb`). What no longer fits there
//!   moves to the converted-audio cache on disk DSD and ALAC use, capped by
//!   `audio_cache_max_mb`; a track evicted from both is decoded again next time.
//! - **Playlists** ([`title_tracks`], [`stand_in`]): which tracks a title page plays, by the
//!   `audio_rom_*` settings. Whether a track is BGM, a jingle or a sound effect is worked out
//!   here, from its loop and length, so a changed threshold needs no re-index.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use sqlx::Row;

use romexplorer_types::jobs::{FileScanMode, RomAudioIndexRequest};

use crate::database::jobs::Progress;
use crate::database::settings as db_settings;
use crate::rom::rom_audio::{self, FoundTrack, SoundKind, Thresholds, TrackLocation};
use crate::rom::{audio_decode, gme, rip_match, rip_sets, rom_songs};
use crate::rom::{vgmstream, xaac};

use super::audio::AudioRow;
use super::audio_convert::{Served, Target};
use super::{App, Error, Result};

// ---------------------------------------------------------------------------
// settings
// ---------------------------------------------------------------------------

/// `audio_title_sources`: where a title page's music comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleSources {
    Tagged,
    Rom,
    Both,
}

impl TitleSources {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "tagged" => Some(Self::Tagged),
            "rom" => Some(Self::Rom),
            "both" => Some(Self::Both),
            _ => None,
        }
    }
    pub fn tagged(self) -> bool {
        self != Self::Rom
    }
    pub fn rom(self) -> bool {
        self != Self::Tagged
    }
    pub fn key(self) -> &'static str {
        match self {
            Self::Tagged => "tagged",
            Self::Rom => "rom",
            Self::Both => "both",
        }
    }
}

/// `audio_rom_kinds`: which sounds out of a ROM a page plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kinds {
    Bgm,
    BgmJingle,
    All,
}

impl Kinds {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "bgm" => Some(Self::Bgm),
            "bgm_jingle" => Some(Self::BgmJingle),
            "all" => Some(Self::All),
            _ => None,
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Self::Bgm => "bgm",
            Self::BgmJingle => "bgm_jingle",
            Self::All => "all",
        }
    }
    pub fn plays(self, kind: SoundKind) -> bool {
        match self {
            Self::Bgm => kind == SoundKind::Bgm,
            Self::BgmJingle => kind != SoundKind::Se,
            Self::All => true,
        }
    }
}

/// `audio_rom_not_owned`: ROM music for a title with none of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotOwned {
    Related,
    Random,
    None,
}

impl NotOwned {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "related" => Some(Self::Related),
            "random" => Some(Self::Random),
            "none" => Some(Self::None),
            _ => None,
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Self::Related => "related",
            Self::Random => "random",
            Self::None => "none",
        }
    }
    /// The playlist's `fallback` when this chose the tracks.
    pub fn name(self) -> &'static str {
        match self {
            Self::Related => "rom_related",
            Self::Random => "rom_random",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Settings {
    pub sources: TitleSources,
    pub kinds: Kinds,
    pub not_owned: NotOwned,
    pub thresholds: Thresholds,
    pub keep_playing: bool,
    /// `audio_rom_videos`: video sound plays.
    pub videos: bool,
    /// `videos` came with the request (a client's answer for its page): it goes over the
    /// titles' own choices here.
    pub videos_forced: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { sources: TitleSources::Both, kinds: Kinds::Bgm, not_owned: NotOwned::Related, thresholds: Thresholds::default(), keep_playing: true, videos: false, videos_forced: false }
    }
}

async fn seconds(app: &App, key: &str, default_ms: u64) -> Result<u64> {
    let value = db_settings::get::<serde_json::Value>(&app.pool, key).await?;
    Ok(value.and_then(|v| v.as_f64()).filter(|s| s.is_finite() && *s >= 0.0).map_or(default_ms, |s| (s * 1000.0).round() as u64))
}

async fn word(app: &App, key: &str) -> Result<Option<String>> {
    Ok(db_settings::get::<String>(&app.pool, key).await?)
}

impl Settings {
    pub async fn load(app: &App) -> Result<Self> {
        let d = Self::default();
        let t = d.thresholds;
        Ok(Self {
            sources: word(app, db_settings::KEY_AUDIO_TITLE_SOURCES).await?.as_deref().and_then(TitleSources::parse).unwrap_or(d.sources),
            kinds: word(app, db_settings::KEY_AUDIO_ROM_KINDS).await?.as_deref().and_then(Kinds::parse).unwrap_or(d.kinds),
            not_owned: word(app, db_settings::KEY_AUDIO_ROM_NOT_OWNED).await?.as_deref().and_then(NotOwned::parse).unwrap_or(d.not_owned),
            thresholds: Thresholds {
                loop_min_ms: seconds(app, db_settings::KEY_AUDIO_ROM_LOOP_MIN_S, t.loop_min_ms).await?,
                bgm_min_ms: seconds(app, db_settings::KEY_AUDIO_ROM_BGM_MIN_S, t.bgm_min_ms).await?,
                jingle_min_ms: seconds(app, db_settings::KEY_AUDIO_ROM_JINGLE_MIN_S, t.jingle_min_ms).await?,
            },
            keep_playing: db_settings::get::<bool>(&app.pool, db_settings::KEY_AUDIO_KEEP_PLAYING).await?.unwrap_or(d.keep_playing),
            videos: db_settings::get::<bool>(&app.pool, db_settings::KEY_AUDIO_ROM_VIDEOS).await?.unwrap_or(d.videos),
            videos_forced: false,
        })
    }

    /// These settings as a playlist request's query, with `not_owned` given apart: how a
    /// client asks the main server for its own choice of tracks rather than the server's.
    pub fn query(&self, not_owned: NotOwned) -> String {
        let t = self.thresholds;
        let s = |ms: u64| ms as f64 / 1000.0;
        format!(
            "sources={}&kinds={}&not_owned={}&videos={}&loop_min_s={}&bgm_min_s={}&jingle_min_s={}",
            self.sources.key(),
            self.kinds.key(),
            not_owned.key(),
            self.videos,
            s(t.loop_min_ms),
            s(t.bgm_min_ms),
            s(t.jingle_min_ms)
        )
    }

    /// These settings with what a playlist request spells out in their place.
    pub fn overridden(mut self, q: &super::audio::PlaylistQuery) -> Result<Self> {
        fn bad(what: &str) -> Error {
            Error::invalid(format!("{what} is not one of its values"))
        }
        if let Some(v) = q.sources.as_deref() {
            self.sources = TitleSources::parse(v).ok_or_else(|| bad("sources"))?;
        }
        if let Some(v) = q.kinds.as_deref() {
            self.kinds = Kinds::parse(v).ok_or_else(|| bad("kinds"))?;
        }
        if let Some(v) = q.not_owned.as_deref() {
            self.not_owned = NotOwned::parse(v).ok_or_else(|| bad("not_owned"))?;
        }
        if let Some(v) = q.videos {
            self.videos = v;
            self.videos_forced = true;
        }
        let ms = |v: Option<f64>, what: &str, current: u64| -> Result<u64> {
            match v {
                None => Ok(current),
                Some(s) if s.is_finite() && s >= 0.0 => Ok((s * 1000.0).round() as u64),
                Some(_) => Err(Error::invalid(format!("{what} must be zero or more seconds"))),
            }
        };
        let t = self.thresholds;
        self.thresholds = Thresholds {
            loop_min_ms: ms(q.loop_min_s, "loop_min_s", t.loop_min_ms)?,
            bgm_min_ms: ms(q.bgm_min_s, "bgm_min_s", t.bgm_min_ms)?,
            jingle_min_ms: ms(q.jingle_min_s, "jingle_min_s", t.jingle_min_ms)?,
        };
        Ok(self)
    }
}

// ---------------------------------------------------------------------------
// the index job
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, serde::Deserialize)]
#[serde(default)]
pub struct IndexSummary {
    pub mode: String,
    pub roms_seen: u64,
    pub roms_read: u64,
    pub skipped_existing: u64,
    /// Not a disc image or a folder: cartridges, archives.
    pub skipped_not_disc: u64,
    /// Left out before the run: nothing here reads their platform or media yet (a floppy or
    /// cartridge with no sound player and no music files beside it). Not marked as indexed,
    /// so a later run takes them once something does.
    pub skipped_unsupported: u64,
    pub tracks: u64,
    /// Of `tracks`, those from rip sets (in game folders or the rip library).
    pub rip_tracks: u64,
    /// Sets in the rip library folders that can be matched to ROMs.
    pub library_sets: u64,
    /// ROMs a library set was matched to.
    pub library_matches: u64,
    /// Of `tracks`, songs read from cartridge ROMs' own sound engines.
    pub rom_songs: u64,
    /// ROMs whose files are byte for byte those of a ROM already indexed: its tracks were
    /// copied instead of reading the ROM again.
    pub same_content: u64,
    pub errors: Vec<String>,
    /// A title page's run (`title_id`): every ROM it looked at, music found or not, so the
    /// page does not queue the same ROMs again when they turn out to have none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rom_ids: Vec<u64>,
}

pub fn one_liner(s: &IndexSummary) -> String {
    let mut parts = vec![format!("{} ROM(s) read, {} track(s) found", s.roms_read, s.tracks)];
    if s.rip_tracks > 0 {
        parts.push(format!("{} from original audio format sets", s.rip_tracks));
    }
    if s.rom_songs > 0 {
        parts.push(format!("{} song(s) read from cartridge sound engines", s.rom_songs));
    }
    if s.library_sets > 0 {
        parts.push(format!("{} ROM(s) matched to {} original audio format set(s)", s.library_matches, s.library_sets));
    }
    if s.same_content > 0 {
        parts.push(format!("{} copied from an indexed ROM with the same content", s.same_content));
    }
    if s.skipped_existing > 0 {
        parts.push(format!("{} already indexed", s.skipped_existing));
    }
    if s.skipped_not_disc > 0 {
        parts.push(format!("{} not a disc", s.skipped_not_disc));
    }
    if s.skipped_unsupported > 0 {
        parts.push(format!("{} on a platform or media not supported yet", s.skipped_unsupported));
    }
    if !s.errors.is_empty() {
        parts.push(format!("{} could not be read", s.errors.len()));
    }
    format!("done: {}", parts.join(", "))
}

pub async fn validate(_app: &App, _request: &RomAudioIndexRequest) -> Result<()> {
    if !vgmstream::available() && !rip_sets::available() && !crate::rom::agbplay::available() {
        return Err(Error::unavailable("none of the game audio decoder (rvgmstream), the chip music players (rommusic) is built for this machine"));
    }
    Ok(())
}

/// Where to open a ROM from: its main file when that is a disc image, the folder it is a
/// dump of when it is made of many files, or nothing (a cartridge, an archive).
fn rom_path(main_path: &str, files: &[String]) -> Option<PathBuf> {
    let main = PathBuf::from(main_path);
    let ext = main.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    // A sheet names its own tracks; an archive's members are not files on disk.
    // A Wii U image counts its key file among its files, and is still one image.
    if matches!(ext.as_str(), "cue" | "gdi" | "ccd" | "mds" | "mdf" | "wua" | "wud" | "wux") || main.is_dir() {
        return Some(main);
    }
    if matches!(ext.as_str(), "zip" | "7z" | "rar") {
        return None;
    }
    if files.len() < 2 {
        return rom_audio::may_hold_audio(&main).then_some(main);
    }
    // Many files: a dumped folder (its main file is an EBOOT.BIN or a PARAM.SFO, not a disc).
    let mut common: PathBuf = Path::new(&files[0]).parent()?.to_path_buf();
    for file in &files[1..] {
        while !Path::new(file).starts_with(&common) {
            common = common.parent()?.to_path_buf();
        }
    }
    (common.components().count() > 1 && common.is_dir()).then_some(common)
}

/// The folder a ROM's rip sets are looked for in: the folder its file sits in, or the
/// folder it is a dump of.
fn game_folder(main_path: &str) -> Option<PathBuf> {
    let main = Path::new(main_path);
    if main.is_dir() {
        return Some(main.to_path_buf());
    }
    main.parent().filter(|p| p.components().count() > 1 && p.is_dir()).map(Path::to_path_buf)
}

/// Whether a ROM may be a disc image or a dumped folder ([`rom_path`]'s cases), from its
/// main path and how many files it has, before its file list is read.
fn disc_like(main_path: &str, files: i64) -> bool {
    let main = Path::new(main_path);
    let ext = main.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if matches!(ext.as_str(), "zip" | "7z" | "rar") {
        return false;
    }
    matches!(ext.as_str(), "cue" | "gdi") || rom_audio::DISC_EXTENSIONS.contains(&ext.as_str()) || files >= 2 || main.is_dir()
}

/// Whether `folder` holds music files of its own (a rip set beside the game), a level or
/// two down.
fn folder_has_music(folder: &Path) -> bool {
    let mut stack = vec![(folder.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            match entry.file_type() {
                Ok(k) if k.is_dir() && depth < 2 => stack.push((entry.path(), depth + 1)),
                Ok(k) if k.is_file() && rip_sets::is_rip_member(&name) => return true,
                _ => {}
            }
        }
    }
    false
}

/// Whether anything here can read music from this ROM: a disc or folder, a cartridge
/// platform with a sound player or rip-library sets, or music files in its own game folder.
/// Partly is enough (a disc whose CD audio plays though its sequences don't yet); a ROM
/// with nothing at all is left out of an index run instead of read for nothing.
async fn readable(app: &App, library: &rip_match::Library, rom_id: u64, main_path: &str, platform: u16, files: i64) -> Result<bool> {
    if rom_songs::covers(platform) || library.covers(platform) || (vgmstream::available() && disc_like(main_path, files)) {
        return Ok(true);
    }
    if !rip_sets::available() {
        return Ok(false);
    }
    let Some(folder) = game_folder(main_path) else { return Ok(false) };
    if !owns_rip_sets(app, rom_id, &folder).await? {
        return Ok(false);
    }
    Ok(tokio::task::spawn_blocking(move || folder_has_music(&folder)).await.unwrap_or(false))
}

/// Whether `rom_id` is the ROM whose tracks a rip set in `folder` is listed under: the
/// folder holds one game (its ROMs all belong to at most one title, and there are few of
/// them), and of that game's ROMs there this is the first. A folder of a whole platform's
/// ROMs has no rip sets of its own.
async fn owns_rip_sets(app: &App, rom_id: u64, folder: &Path) -> Result<bool> {
    const MAX_ROMS: usize = 32;
    let mut prefix = folder.to_string_lossy().into_owned();
    if !prefix.ends_with(std::path::MAIN_SEPARATOR) {
        prefix.push(std::path::MAIN_SEPARATOR);
    }
    let pattern = prefix.replace('|', "||").replace('%', "|%").replace('_', "|_") + "%";
    let ids: Vec<u64> = sqlx::query_scalar("SELECT id FROM local_rom WHERE deleted = FALSE AND (main_path LIKE ? ESCAPE '|' OR main_path = ?) ORDER BY id LIMIT 33")
        .bind(&pattern)
        .bind(folder.to_string_lossy().as_ref())
        .fetch_all(&app.pool)
        .await?;
    if ids.len() > MAX_ROMS || ids.first().is_some_and(|&first| first != rom_id) {
        return Ok(false);
    }
    if ids.len() <= 1 {
        return Ok(true);
    }
    let list = ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let titles: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT COUNT(DISTINCT tr.title_id) FROM local_game_roms gr JOIN local_release_games lrg ON lrg.game_id = gr.game_id \
         JOIN title_releases tr ON tr.release_id = lrg.release_id WHERE gr.rom_id IN ({list})"
    )))
    .fetch_one(&app.pool)
    .await?;
    Ok(titles <= 1)
}

/// The rip library from `audio_rip_folders`, fingerprinted; empty when there is none or the
/// player is not built. Fingerprints are cached beside the media.
async fn rip_library(app: &App, progress: &Progress) -> Result<rip_match::Library> {
    progress.report(0, None, "reading the original audio format sets").await;
    library_quiet(app).await
}

async fn library_quiet(app: &App) -> Result<rip_match::Library> {
    let folders: Vec<PathBuf> = db_settings::get::<Vec<String>>(&app.pool, db_settings::KEY_AUDIO_RIP_FOLDERS).await?.unwrap_or_default().into_iter().map(PathBuf::from).filter(|p| p.is_dir()).collect();
    if folders.is_empty() || !rip_sets::available() {
        return Ok(rip_match::Library::build(&[], None, |_, _| {}));
    }
    let cache = app.paths.media_dir().join("_rip_library.json");
    tokio::task::spawn_blocking(move || rip_match::Library::build(&folders, Some(&cache), |_, _| {})).await.map_err(|e| Error::internal(format!("ROM audio task: {e}")))
}

/// PS1 and PS2 rips play on a kernel built from a PS2 BIOS, and none ships with romexplorer:
/// this hands the player the first 4 MB PS2 BIOS in the collection that works, loose or in
/// a zip/7z, whether a DAT lists it or a scan identified it. Without one those rips are
/// skipped. Looked for again at most every few minutes while none is found.
pub async fn provide_ps2_bios(app: &App) -> Result<()> {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    use crate::rom::psf;
    static LAST_TRY: Mutex<Option<Instant>> = Mutex::new(None);
    if !psf::psx_installed() || psf::has_ps2_bios() {
        return Ok(());
    }
    {
        let mut last = LAST_TRY.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed() < Duration::from_secs(300)) {
            return Ok(());
        }
        *last = Some(Instant::now());
    }
    let platforms = super::catalog::platform_filter_ids(crate::common::models::Platform::PlayStation2 as u16).iter().map(u16::to_string).collect::<Vec<_>>().join(",");
    let size = psf::PS2_BIOS_BYTES;
    let dat_bios = format!(
        "SELECT rc.content_id FROM release_contents rc JOIN `release` r ON r.id = rc.release_id \
         WHERE r.material = 'bios' AND r.deleted = FALSE AND r.app_platform_id IN ({platforms})"
    );
    let sql = format!(
        "SELECT lf.path, NULL AS member FROM local_files lf \
         WHERE lf.deleted = FALSE AND lf.size = {size} \
           AND ((lf.system_type = 'bios' AND lf.system_platform IN ({platforms})) OR lf.content_id IN ({dat_bios})) \
         UNION ALL \
         SELECT lf.path, cc.member_path AS member FROM content_contents cc \
         JOIN content c ON c.id = cc.child_content_id AND c.size = {size} \
         JOIN local_files lf ON lf.content_id = cc.parent_content_id AND lf.deleted = FALSE \
         WHERE cc.member_path IS NOT NULL AND cc.member_path <> '' AND cc.child_content_id IN ({dat_bios}) \
         LIMIT 50"
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(&app.pool).await?;
    let candidates: Vec<(String, Option<String>)> = rows.iter().map(|r| Ok((r.try_get("path")?, r.try_get("member")?))).collect::<Result<_>>()?;
    tokio::task::spawn_blocking(move || {
        for (path, member) in candidates {
            let image = match member {
                None => std::fs::read(&path).ok(),
                Some(member) => read_member(Path::new(&path), &member),
            };
            if let Some(image) = image {
                if psf::set_ps2_bios(&image).unwrap_or(false) {
                    log::info!("PS1/PS2 music: using the PS2 BIOS {path}");
                    return;
                }
            }
        }
    })
    .await
    .map_err(|e| Error::internal(format!("ROM audio task: {e}")))?;
    Ok(())
}

/// One member of a zip or 7z, by the path the scan recorded for it.
fn read_member(archive: &Path, member: &str) -> Option<Vec<u8>> {
    use crate::rom::sevenzip_wrapper::{ArchiveFormat, SevenZipArchive};
    let format = match archive.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "zip" => ArchiveFormat::Zip,
        "7z" => ArchiveFormat::SevenZ,
        _ => return None,
    };
    let norm = |p: &str| p.replace('\\', "/").trim_start_matches('/').to_string();
    let want = norm(member);
    let opened = SevenZipArchive::open(archive, format, |_, _| true).ok()?;
    (0..opened.entry_count()).find(|&i| opened.entry(i).is_ok_and(|e| !e.is_dir && norm(&e.path) == want)).and_then(|i| opened.read_entry(i).ok())
}

/// The ROMs index runs are reading, across the runs going on at once.
static READING: std::sync::LazyLock<std::sync::Mutex<HashSet<u64>>> = std::sync::LazyLock::new(Default::default);

/// One run's claim on a ROM while it reads it; dropped, the claim ends.
struct Reading(u64);

impl Reading {
    /// The claim, or `None` when another run holds it.
    fn start(rom_id: u64) -> Option<Self> {
        let fresh = READING.lock().unwrap_or_else(|p| p.into_inner()).insert(rom_id);
        fresh.then(|| Self(rom_id))
    }
}

impl Drop for Reading {
    fn drop(&mut self) {
        READING.lock().unwrap_or_else(|p| p.into_inner()).remove(&self.0);
    }
}

/// How long one ROM may take before the index gives up on it. The longest honest reads (a
/// cartridge's sound-engine search) stop by themselves well inside this.
const ROM_TIME_LIMIT: std::time::Duration = std::time::Duration::from_secs(20 * 60);

pub async fn run_index(app: &App, request: &RomAudioIndexRequest, progress: &Progress) -> Result<IndexSummary> {
    provide_ps2_bios(app).await?;
    let quick = request.mode == FileScanMode::NewOnly;
    let mut summary = IndexSummary { mode: if quick { "new_only" } else { "full" }.to_string(), ..Default::default() };
    let thresholds = Settings::load(app).await?.thresholds;
    let scope = match request.title_id {
        Some(id) => format!(
            " AND r.id IN (SELECT gr.rom_id FROM title_releases tr JOIN local_release_games lrg ON lrg.release_id = tr.release_id \
              JOIN local_game_roms gr ON gr.game_id = lrg.game_id WHERE tr.title_id = {id})"
        ),
        None => String::new(),
    };
    let skip = if quick { " AND NOT EXISTS (SELECT 1 FROM rom_audio_index i WHERE i.rom_id = r.id)" } else { "" };
    // Smallest first: cartridges and small discs are playable within minutes, and big disc
    // images, which take longest to read, come last. A ROM with no file sizes recorded goes
    // last too, not first.
    let all = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT r.id, r.main_path, r.app_platform_id, s.files, s.bytes FROM local_rom r \
         LEFT JOIN (SELECT rf.rom_id, COUNT(*) AS files, CAST(SUM(f.size) AS UNSIGNED) AS bytes FROM local_rom_files rf JOIN local_files f ON f.id = rf.file_id AND f.deleted = FALSE GROUP BY rf.rom_id) s ON s.rom_id = r.id \
         WHERE r.deleted = FALSE{scope}{skip} ORDER BY s.bytes IS NULL, s.bytes, r.main_path"
    )))
    .fetch_all(&app.pool)
    .await?;
    let library = Arc::new(rip_library(app, progress).await?);
    summary.library_sets = library.len() as u64;
    let mut roms = Vec::with_capacity(all.len());
    let checking = all.len() as u64;
    for (i, row) in all.into_iter().enumerate() {
        if i % 500 == 0 {
            progress.report(i as u64, Some(checking), &format!("checking which ROMs have music this can read: {i} of {checking}")).await;
        }
        let files: i64 = row.try_get::<Option<i64>, _>("files")?.unwrap_or(0);
        if readable(app, &library, row.try_get("id")?, &row.try_get::<String, _>("main_path")?, row.try_get("app_platform_id")?, files).await? {
            roms.push(row);
        } else {
            summary.skipped_unsupported += 1;
        }
    }
    if quick {
        summary.skipped_existing = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM local_rom r WHERE r.deleted = FALSE{scope} AND EXISTS (SELECT 1 FROM rom_audio_index i WHERE i.rom_id = r.id)")))
            .fetch_one(&app.pool)
            .await?
            .max(0) as u64;
    }
    let total = roms.len() as u64;
    summary.roms_seen = total;
    if request.title_id.is_some() {
        summary.rom_ids = roms.iter().filter_map(|r| r.try_get("id").ok()).collect();
    }
    let mut chosen = choose_library_sets(app, progress, &library, &roms).await?;
    // A library set's tracks, read once per run however many ROMs (regions) it matches.
    let mut set_tracks: HashMap<PathBuf, Vec<FoundTrack>> = HashMap::new();
    // ROMs this run indexed: what a full run may copy a same-content ROM's tracks from.
    let mut read_this_run: HashSet<u64> = HashSet::new();
    for (i, row) in roms.iter().enumerate() {
        if progress.cancelled().await {
            break;
        }
        let rom_id: u64 = row.try_get("id")?;
        let main_path: String = row.try_get("main_path")?;
        // A ROM another run is reading right now (a title page's, beside this one) is that
        // run's: the same disc is not read twice at once.
        let Some(_reading) = Reading::start(rom_id) else {
            summary.skipped_existing += 1;
            continue;
        };
        // A title page opened while this run goes on indexes its own ROMs first, beside it:
        // a quick run does not read them a second time when it gets there.
        if quick && sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM rom_audio_index WHERE rom_id = ?").bind(rom_id).fetch_one(&app.pool).await? > 0 {
            summary.skipped_existing += 1;
            continue;
        }
        let files: Vec<String> = sqlx::query_scalar("SELECT f.path FROM local_rom_files rf JOIN local_files f ON f.id = rf.file_id AND f.deleted = FALSE WHERE rf.rom_id = ? ORDER BY f.path LIMIT 100000")
            .bind(rom_id)
            .fetch_all(&app.pool)
            .await?;
        let folder = match game_folder(&main_path) {
            Some(folder) if rip_sets::available() && owns_rip_sets(app, rom_id, &folder).await? => Some(folder),
            _ => None,
        };
        let platform: u16 = row.try_get("app_platform_id")?;
        // The same files as a ROM already indexed (another copy, a duplicate folder): its
        // tracks are this one's too. Not when this ROM's folder has music files of its own,
        // which the other's index would not have. A full run reuses only ROMs it read itself.
        if folder.is_none() {
            if let Some(source) = same_content_indexed(app, rom_id, platform, (!quick).then_some(&read_this_run)).await? {
                let label = Path::new(&main_path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| main_path.clone());
                progress.report(i as u64, Some(total), &format!("ROM {} of {total}: {label} (same content as one already indexed)", i + 1)).await;
                if let Some(copied) = copy_index(app, source, rom_id, !quick).await? {
                    summary.same_content += 1;
                    summary.tracks += copied;
                    read_this_run.insert(rom_id);
                    continue;
                }
            }
        }
        let disc = rom_path(&main_path, &files).filter(|_| vgmstream::available());
        let in_library = library.covers(platform);
        let songs = rom_songs::covers(platform);
        if disc.is_none() && folder.is_none() && !in_library && !songs {
            summary.skipped_not_disc += 1;
            continue;
        }
        let label = Path::new(&main_path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| main_path.clone());
        progress.report(i as u64, Some(total), &format!("ROM {} of {total}: {label}", i + 1)).await;
        let open = disc.clone();
        let matched = chosen.remove(&rom_id).unwrap_or(Ok(Vec::new()));
        let rom_file = PathBuf::from(&main_path);
        // A NES ROM matched to a set with an NSF: its music map, built once, so the songs play
        // straight from the ROM with no boot.
        let map_sets: Vec<PathBuf> = match (&matched, rom_songs::builds_maps(platform)) {
            (Ok(sets), true) => sets.clone(),
            _ => Vec::new(),
        };
        // What inside the ROM is being read right now, for the job's progress line.
        let step = Arc::new(std::sync::Mutex::new(String::new()));
        let at = step.clone();
        let mut task = tokio::task::spawn_blocking(move || {
            let note = |what: &str| *at.lock().unwrap_or_else(|p| p.into_inner()) = what.to_string();
            note("music map");
            let map_error = (!map_sets.is_empty() && rom_file.is_file()).then(|| rom_songs::map_from_sets(&rom_file, &map_sets)).and_then(|r| r.err());
            note("music files in the game's folder");
            let rips = folder.map(|f| rip_sets::index(&f, &thresholds, |p| note(p))).unwrap_or_default();
            note("disc");
            let disc = open.map(|p| rom_audio::index_rom(&p, &thresholds, |p| note(p)));
            note("the game's sound engine");
            let songs = if songs && rom_file.is_file() { rom_songs::index(&rom_file, platform, &thresholds) } else { Ok(Vec::new()) };
            (rips, disc, songs, map_error)
        });
        let started = std::time::Instant::now();
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
        let found = loop {
            tokio::select! {
                done = &mut task => match done {
                    Ok(found) => break Some(found),
                    // A reader that panics on one ROM (a bad map, a broken file) costs that ROM
                    // only: it is noted as an error and the run goes on with the next.
                    Err(e) if e.is_panic() => {
                        let what = step.lock().unwrap_or_else(|p| p.into_inner()).clone();
                        let error = format!("reader failed while reading {what}: {}", panic_text(e.into_panic()));
                        log::warn!("rom audio index: {main_path}: {error}");
                        summary.errors.push(format!("{main_path}: {error}"));
                        record_error(app, rom_id, &error).await?;
                        break None;
                    }
                    Err(e) => return Err(Error::internal(format!("ROM audio task: {e}"))),
                },
                _ = tick.tick() => {
                    let what = step.lock().unwrap_or_else(|p| p.into_inner()).clone();
                    let secs = started.elapsed().as_secs();
                    // A ROM that never finishes must not hold the whole run: it is noted as
                    // an error and the run goes on. Its reader is left to end on its own.
                    if started.elapsed() > ROM_TIME_LIMIT || progress.cancelled().await {
                        if started.elapsed() > ROM_TIME_LIMIT {
                            let error = format!("gave up after {} min, still reading {what}", ROM_TIME_LIMIT.as_secs() / 60);
                            log::warn!("rom audio index: {main_path}: {error}");
                            summary.errors.push(format!("{main_path}: {error}"));
                            record_error(app, rom_id, &error).await?;
                        }
                        break None;
                    }
                    if secs >= 4 {
                        progress.report(i as u64, Some(total), &format!("ROM {} of {total}: {main_path}: {what} ({secs} s)", i + 1)).await;
                    }
                }
            }
        };
        let Some(found) = found else { continue };
        let (mut rips, disc_found, songs_found, map_error) = found;
        if let Some(e) = map_error {
            summary.errors.push(format!("{main_path}: music map: {e}"));
        }
        let mut song_count = 0;
        match songs_found {
            Ok(songs) => {
                song_count = songs.len();
                summary.rom_songs += song_count as u64;
                rips.extend(songs);
            }
            Err(e) => summary.errors.push(format!("{main_path}: ROM music: {e}")),
        }
        match matched {
            Ok(sets) => {
                if !sets.is_empty() {
                    summary.library_matches += 1;
                }
                for set in sets {
                    if !set_tracks.contains_key(&set) {
                        let (path, t) = (set.clone(), thresholds);
                        let tracks = match tokio::task::spawn_blocking(move || rip_sets::index_set(&path, &t, |_| {})).await {
                            Ok(tracks) => tracks,
                            // A set that panics its reader is left out; the rest still count.
                            Err(e) if e.is_panic() => {
                                summary.errors.push(format!("{}: reader failed: {}", set.display(), panic_text(e.into_panic())));
                                Vec::new()
                            }
                            Err(e) => return Err(Error::internal(format!("ROM audio task: {e}"))),
                        };
                        set_tracks.insert(set.clone(), tracks);
                    }
                    rips.extend(set_tracks[&set].iter().cloned());
                }
            }
            Err(e) => summary.errors.push(format!("{main_path}: original audio format sets: {e}")),
        }
        let (disc_tracks, error) = match disc_found {
            Some(Ok(tracks)) => (tracks, None),
            Some(Err(e)) => {
                summary.errors.push(format!("{}: {e}", disc.as_deref().unwrap_or(Path::new(&main_path)).display()));
                (Vec::new(), Some(e))
            }
            None => (Vec::new(), None),
        };
        if let (Some(e), true) = (&error, rips.is_empty()) {
            record_error(app, rom_id, e).await?;
            continue;
        }
        if disc.is_none() && rips.is_empty() {
            summary.skipped_not_disc += 1;
            continue;
        }
        summary.rip_tracks += (rips.len() - song_count) as u64;
        let tracks: Vec<FoundTrack> = disc_tracks.into_iter().chain(rips).collect();
        summary.tracks += tracks.len() as u64;
        summary.roms_read += 1;
        store(app, rom_id, &tracks, error.as_deref()).await?;
        if error.is_none() {
            read_this_run.insert(rom_id);
        }
    }
    // A run stopped before its end did not look at all of its ROMs: a title page asks for
    // them again.
    if progress.cancelled().await {
        summary.rom_ids.clear();
    }
    progress.force(total, Some(total.max(1)), &one_liner(&summary)).await;
    Ok(summary)
}

/// The library sets each ROM of the run takes, by ROM id. Every covered ROM is matched first,
/// so a weak match can be checked against the set's strong one: a set that matches its own
/// game strongly (in this run, or already attached to a ROM outside it) is not also given to
/// ROMs that only share its sound driver.
async fn choose_library_sets(app: &App, progress: &Progress, library: &Arc<rip_match::Library>, roms: &[sqlx::mysql::MySqlRow]) -> Result<HashMap<u64, std::result::Result<Vec<PathBuf>, String>>> {
    if library.is_empty() {
        return Ok(HashMap::new());
    }
    let mut found: Vec<(u64, std::result::Result<Vec<(PathBuf, f32)>, String>)> = Vec::new();
    let covered: Vec<(u64, PathBuf, u16)> = roms
        .iter()
        .filter_map(|r| Some((r.try_get::<u64, _>("id").ok()?, PathBuf::from(r.try_get::<String, _>("main_path").ok()?), r.try_get::<u16, _>("app_platform_id").ok()?)))
        .filter(|(_, path, platform)| library.covers(*platform) && !rip_match::skip_rom(path))
        .collect();
    let total = covered.len() as u64;
    for (i, (rom_id, path, platform)) in covered.into_iter().enumerate() {
        if progress.cancelled().await {
            break;
        }
        let label = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        progress.report(i as u64, Some(total), &format!("matching ROM {} of {total} to the original audio format sets: {label}", i + 1)).await;
        let lib = library.clone();
        let matched = match tokio::task::spawn_blocking(move || if path.is_file() { lib.match_rom(&path, platform) } else { Ok(Vec::new()) }).await {
            Ok(m) => m,
            // A ROM whose matching panics is that ROM's error, not the run's.
            Err(e) if e.is_panic() => Err(format!("matching failed: {}", panic_text(e.into_panic()))),
            Err(e) => return Err(Error::internal(format!("ROM audio task: {e}"))),
        };
        found.push((rom_id, matched));
    }
    // Who holds each set strongly: the same game in another region, revision or beta is not
    // another owner, so owners are kept by title.
    let mut paths: HashMap<u64, String> = roms.iter().filter_map(|r| Some((r.try_get::<u64, _>("id").ok()?, r.try_get::<String, _>("main_path").ok()?))).collect();
    let mut strong: Vec<(PathBuf, u64)> = found
        .iter()
        .filter_map(|(rom_id, m)| Some((*rom_id, m.as_ref().ok()?)))
        .flat_map(|(rom_id, sets)| sets.iter().filter(|(_, share)| *share >= rip_match::SURE_SHARE).map(move |(set, _)| (set.clone(), rom_id)))
        .collect();
    // Sets already attached to ROMs this run does not re-read.
    let attached: Vec<(u64, String, String)> = sqlx::query_as(
        "SELECT DISTINCT t.rom_id, t.inner_path, r.main_path FROM rom_audio_tracks t JOIN local_rom r ON r.id = t.rom_id \
         WHERE t.location = 'file' AND t.data_track = ? AND t.inner_path IS NOT NULL",
    )
    .bind(rip_sets::RIP_VIEW)
    .fetch_all(&app.pool)
    .await?;
    let read_now: HashSet<u64> = found.iter().map(|(id, _)| *id).collect();
    for (rom_id, inner, main_path) in attached {
        if !read_now.contains(&rom_id) {
            if let Some(set) = library.set_of(&inner) {
                strong.push((set.to_path_buf(), rom_id));
                paths.entry(rom_id).or_insert(main_path);
            }
        }
    }
    // A multi-cart holds other games whole; it is never the reason a game loses its own set.
    strong.retain(|(_, rom_id)| !paths.get(rom_id).is_some_and(|p| rip_match::multi_cart(Path::new(p))));
    let ids: Vec<u64> = found.iter().map(|(id, _)| *id).chain(strong.iter().map(|(_, id)| *id)).collect::<HashSet<_>>().into_iter().collect();
    let titles = title_ids(app, &ids).await?;
    let mut owners: HashMap<PathBuf, HashSet<u64>> = HashMap::new();
    for (set, rom_id) in strong {
        if let Some(title) = titles.get(&rom_id) {
            owners.entry(set).or_default().insert(*title);
        }
    }
    // Only titles decide who a set belongs to: names differ across regions ("... no Densetsu",
    // a Japanese and a Western title), so a ROM not in a title yet keeps its weak matches.
    Ok(found
        .into_iter()
        .map(|(rom_id, m)| {
            let mine = titles.get(&rom_id).copied();
            let other = |set: &Path| mine.is_some_and(|me| owners.get(set).is_some_and(|o| o.iter().any(|t| *t != me)));
            let sibling = |set: &Path| mine.is_some_and(|me| owners.get(set).is_some_and(|o| o.contains(&me)));
            (rom_id, m.map(|sets| rip_match::choose(&sets, other, sibling)))
        })
        .collect())
}

/// The title each ROM belongs to, for those in one. Regions, revisions and betas of one game
/// share it whatever their names.
async fn title_ids(app: &App, ids: &[u64]) -> Result<HashMap<u64, u64>> {
    let mut titles = HashMap::new();
    for chunk in ids.chunks(1000) {
        let list = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        let rows: Vec<(u64, u64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT gr.rom_id, MIN(tr.title_id) FROM local_game_roms gr JOIN local_release_games lrg ON lrg.game_id = gr.game_id \
             JOIN title_releases tr ON tr.release_id = lrg.release_id WHERE gr.rom_id IN ({list}) GROUP BY gr.rom_id"
        )))
        .fetch_all(&app.pool)
        .await?;
        titles.extend(rows);
    }
    Ok(titles)
}

/// Notes why a ROM could not be read, and keeps the tracks of its last good index: the
/// failure may pass (a drive not mounted, a file in use), and the kinds set by hand live on
/// those rows. `indexed_at` is left as it was, so cached decodes keep their names.
async fn record_error(app: &App, rom_id: u64, error: &str) -> Result<()> {
    sqlx::query("INSERT INTO rom_audio_index (rom_id, indexed_at, tracks, error) VALUES (?, NOW(), 0, ?) ON DUPLICATE KEY UPDATE error = VALUES(error)")
        .bind(rom_id)
        .bind(error.chars().take(1024).collect::<String>())
        .execute(&app.pool)
        .await?;
    Ok(())
}

/// A ROM's content: the content ids of its files, in order. Two ROMs with the same list
/// hold the same bytes, wherever their files are.
async fn content_of(app: &App, rom_id: u64) -> Result<Vec<u64>> {
    Ok(sqlx::query_scalar("SELECT f.content_id FROM local_rom_files rf JOIN local_files f ON f.id = rf.file_id AND f.deleted = FALSE WHERE rf.rom_id = ? ORDER BY f.content_id")
        .bind(rom_id)
        .fetch_all(&app.pool)
        .await?)
}

/// Another ROM of the same platform with exactly `rom_id`'s content that was indexed without
/// an error -- with `among`, only one of those. Candidates are the ROMs sharing its first
/// file's content, so this is a lookup by index, not a pass over every ROM.
async fn same_content_indexed(app: &App, rom_id: u64, platform: u16, among: Option<&HashSet<u64>>) -> Result<Option<u64>> {
    let mine = content_of(app, rom_id).await?;
    let Some(&first) = mine.first() else { return Ok(None) };
    let candidates: Vec<u64> = sqlx::query_scalar(
        "SELECT DISTINCT rf.rom_id FROM local_files f JOIN local_rom_files rf ON rf.file_id = f.id JOIN local_rom r ON r.id = rf.rom_id AND r.deleted = FALSE AND r.app_platform_id = ? \
         JOIN rom_audio_index i ON i.rom_id = rf.rom_id AND i.error IS NULL WHERE f.content_id = ? AND f.deleted = FALSE AND rf.rom_id <> ? LIMIT 50",
    )
    .bind(platform)
    .bind(first)
    .bind(rom_id)
    .fetch_all(&app.pool)
    .await?;
    for other in candidates {
        if among.is_some_and(|set| !set.contains(&other)) {
            continue;
        }
        if content_of(app, other).await? == mine {
            return Ok(Some(other));
        }
    }
    Ok(None)
}

/// Gives `rom_id` the tracks of `source` (the same content), kinds set by hand included.
/// With `keep_own`, a ROM whose own tracks carry kinds set by hand is left to be read
/// instead (`None`), so those are not lost. Returns how many tracks it now has.
async fn copy_index(app: &App, source: u64, rom_id: u64, keep_own: bool) -> Result<Option<u64>> {
    let mut tx = app.pool.begin().await?;
    if keep_own {
        let own: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM rom_audio_tracks WHERE rom_id = ? AND kind_manual IS NOT NULL").bind(rom_id).fetch_one(&mut *tx).await?;
        if own > 0 {
            return Ok(None);
        }
    }
    sqlx::query("DELETE FROM rom_audio_tracks WHERE rom_id = ?").bind(rom_id).execute(&mut *tx).await?;
    let copied = sqlx::query(
        "INSERT INTO rom_audio_tracks (rom_id, location, cd_track, data_track, inner_path, subsong, name, kind_manual, sample_rate, channels, duration_ms, looping, loop_start, loop_end, codec, format, bytes, video) \
         SELECT ?, location, cd_track, data_track, inner_path, subsong, name, kind_manual, sample_rate, channels, duration_ms, looping, loop_start, loop_end, codec, format, bytes, video FROM rom_audio_tracks WHERE rom_id = ? ORDER BY id",
    )
    .bind(rom_id)
    .bind(source)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    sqlx::query("INSERT INTO rom_audio_index (rom_id, indexed_at, tracks, error) VALUES (?, NOW(), ?, NULL) ON DUPLICATE KEY UPDATE indexed_at = NOW(), tracks = VALUES(tracks), error = NULL")
        .bind(rom_id)
        .bind(copied as u32)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Some(copied))
}

/// A title page's ROMs still being indexed: what the page shows in place of music until
/// they are done.
#[derive(Debug, Clone, Default, Serialize, serde::Deserialize)]
pub struct Indexing {
    pub job_id: u64,
    /// The page's ROMs with no index yet.
    pub roms: u64,
    /// `pending` (waiting for its slot) or `in_progress`.
    pub status: String,
    pub done: Option<u64>,
    pub total: Option<u64>,
    pub message: Option<String>,
}

/// The page's own ROMs first: when `titles` have ROMs here that were never indexed, a quick
/// index of just those titles goes into the ROM audio lane's fast slot, so it runs beside a
/// long index of the whole library instead of waiting behind it. Returns the job while it
/// is queued or running; `None` once there is nothing left to index for the page (all done,
/// or the last run for the title looked at them and found nothing to keep).
pub async fn indexing_for(app: &App, titles: &[u64]) -> Result<Option<Indexing>> {
    if titles.is_empty() || validate(app, &RomAudioIndexRequest { mode: FileScanMode::NewOnly, title_id: None }).await.is_err() {
        return Ok(None);
    }
    let mut queued = None;
    for &title in titles.iter().take(4) {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT r.id, r.main_path, r.app_platform_id, (SELECT COUNT(*) FROM local_rom_files rf WHERE rf.rom_id = r.id) AS files \
             FROM local_rom r WHERE r.deleted = FALSE AND r.id IN ({}) AND NOT EXISTS (SELECT 1 FROM rom_audio_index i WHERE i.rom_id = r.id)",
            roms_of(&[title])
        )))
        .fetch_all(&app.pool)
        .await?;
        // Only ROMs something here can read: a page of a platform with no support yet
        // queues nothing and shows no "indexing" that would never end.
        let mut unindexed: Vec<u64> = Vec::new();
        if !rows.is_empty() {
            let library = page_library(app).await?;
            for row in rows {
                let id: u64 = row.try_get("id")?;
                if readable(app, &library, id, &row.try_get::<String, _>("main_path")?, row.try_get("app_platform_id")?, row.try_get("files")?).await? {
                    unindexed.push(id);
                }
            }
        }
        if unindexed.is_empty() {
            continue;
        }
        let jobs = sqlx::query(
            "SELECT id, status, progress_done, progress_total, progress_message, result FROM jobs \
             WHERE job_type = 'rom_audio_index' AND CAST(JSON_VALUE(payload, '$.title_id') AS UNSIGNED) = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(title)
        .fetch_optional(&app.pool)
        .await?;
        if let Some(job) = &jobs {
            let status: String = job.try_get("status")?;
            if status == "pending" || status == "in_progress" {
                // The page open now goes before pages opened earlier and left.
                if status == "pending" {
                    crate::database::jobs::run_next(&app.pool, &[job.try_get("id")?]).await?;
                }
                let indexing = Indexing {
                    job_id: job.try_get("id")?,
                    roms: unindexed.len() as u64,
                    status,
                    done: job.try_get::<Option<u64>, _>("progress_done")?,
                    total: job.try_get::<Option<u64>, _>("progress_total")?,
                    message: job.try_get("progress_message")?,
                };
                queued.get_or_insert(indexing);
                continue;
            }
            // The last run for this title looked at these ROMs and kept nothing (not music
            // it can read), or failed: not queued again on every visit. The Audio tab's
            // index buttons still read them.
            let looked: HashSet<u64> = job
                .try_get::<Option<serde_json::Value>, _>("result")?
                .and_then(|r| serde_json::from_value::<IndexSummary>(r).ok())
                .map(|r| r.rom_ids.into_iter().collect())
                .unwrap_or_default();
            if status == "failed" || unindexed.iter().all(|id| looked.contains(id)) {
                continue;
            }
        }
        let request = super::jobs::JobRequest::RomAudioIndex(RomAudioIndexRequest { mode: FileScanMode::NewOnly, title_id: Some(title) });
        let id = super::jobs::enqueue_with_priority(app, &request, super::jobs::PRIORITY_INTERACTIVE).await?;
        crate::database::jobs::run_next(&app.pool, &[id]).await?;
        log::info!("rom audio: title {title}'s page opened with {} ROM(s) not indexed; job {id} indexes them first", unindexed.len());
        queued.get_or_insert(Indexing { job_id: id, roms: unindexed.len() as u64, status: "pending".to_string(), ..Default::default() });
    }
    Ok(queued)
}

/// A track's place in its ROM, as the key a user's own kind is kept by across re-indexes.
type PlaceKey = (String, Option<u16>, u16, String, u32);

fn place(location: &TrackLocation) -> (&'static str, Option<u16>, u16, Option<&str>, u32) {
    match location {
        TrackLocation::Cdda { track } => ("cdda", Some(*track as u16), 0, None, 0),
        TrackLocation::File { view, path, subsong } => ("file", None, *view as u16, Some(path.as_str()), *subsong),
        TrackLocation::Rip { path, track } => ("file", None, rip_sets::RIP_VIEW as u16, Some(path.as_str()), *track),
        TrackLocation::RomSong { engine, song } => ("file", None, rom_songs::ROM_SONG_VIEW as u16, Some(engine.as_str()), *song),
    }
}

/// Replaces `rom_id`'s tracks with `tracks`, keeping the kinds a user set on the ones that
/// are still there.
async fn store(app: &App, rom_id: u64, tracks: &[FoundTrack], error: Option<&str>) -> Result<()> {
    let mut tx = app.pool.begin().await?;
    let manual: HashMap<PlaceKey, String> = sqlx::query("SELECT location, cd_track, data_track, inner_path, subsong, kind_manual FROM rom_audio_tracks WHERE rom_id = ? AND kind_manual IS NOT NULL")
        .bind(rom_id)
        .fetch_all(&mut *tx)
        .await?
        .iter()
        .filter_map(|r| {
            let key = (r.try_get("location").ok()?, r.try_get("cd_track").ok()?, r.try_get("data_track").ok()?, r.try_get::<Option<String>, _>("inner_path").ok()?.unwrap_or_default(), r.try_get("subsong").ok()?);
            Some((key, r.try_get("kind_manual").ok()?))
        })
        .collect();
    sqlx::query("DELETE FROM rom_audio_tracks WHERE rom_id = ?").bind(rom_id).execute(&mut *tx).await?;
    for chunk in tracks.chunks(500) {
        let mut q = sqlx::QueryBuilder::<sqlx::MySql>::new(
            "INSERT INTO rom_audio_tracks (rom_id, location, cd_track, data_track, inner_path, subsong, name, kind_manual, sample_rate, channels, duration_ms, looping, loop_start, loop_end, codec, format, bytes, video) ",
        );
        q.push_values(chunk, |mut b, t| {
            let (location, cd_track, data_track, inner, subsong) = place(&t.location);
            let key: PlaceKey = (location.to_string(), cd_track, data_track, inner.unwrap_or_default().to_string(), subsong);
            b.push_bind(rom_id)
                .push_bind(location)
                .push_bind(cd_track)
                .push_bind(data_track)
                .push_bind(inner.map(|p| p.chars().take(1024).collect::<String>()))
                .push_bind(subsong)
                .push_bind(t.name.chars().take(512).collect::<String>())
                .push_bind(manual.get(&key).cloned())
                .push_bind(t.sample_rate)
                .push_bind(t.channels)
                .push_bind(t.duration_ms.min(u64::from(u32::MAX)) as u32)
                .push_bind(t.looping)
                .push_bind(t.loop_start)
                .push_bind(t.loop_end)
                .push_bind(t.codec.chars().take(128).collect::<String>())
                .push_bind(t.format.chars().take(128).collect::<String>())
                .push_bind(t.bytes)
                .push_bind(t.video);
        });
        q.build().execute(&mut *tx).await?;
    }
    sqlx::query("INSERT INTO rom_audio_index (rom_id, indexed_at, tracks, error) VALUES (?, NOW(), ?, ?) ON DUPLICATE KEY UPDATE indexed_at = NOW(), tracks = VALUES(tracks), error = VALUES(error)")
        .bind(rom_id)
        .bind(tracks.len() as u32)
        .bind(error.map(|e| e.chars().take(1024).collect::<String>()))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// playlists
// ---------------------------------------------------------------------------

const TRACK_SELECT: &str = "t.id, t.rom_id, t.location, t.cd_track, t.data_track, t.inner_path, t.subsong, t.name, t.kind_manual, t.sample_rate, t.channels, t.duration_ms, t.looping, t.codec, t.bytes, r.main_path";

/// The ROMs of `titles`, as SQL.
/// The rip library as a title page sees it, built again at most every ten minutes: a page
/// asks on every visit, and only to know which of its ROMs any set could belong to.
async fn page_library(app: &App) -> Result<Arc<rip_match::Library>> {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    static CACHED: Mutex<Option<(Instant, Arc<rip_match::Library>)>> = Mutex::new(None);
    if let Some((at, lib)) = CACHED.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if at.elapsed() < Duration::from_secs(600) {
            return Ok(lib.clone());
        }
    }
    let lib = Arc::new(library_quiet(app).await?);
    *CACHED.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), lib.clone()));
    Ok(lib)
}

fn roms_of(titles: &[u64]) -> String {
    let ids = titles.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    format!(
        "SELECT gr.rom_id FROM title_releases tr JOIN local_release_games lrg ON lrg.release_id = tr.release_id \
         JOIN local_game_roms gr ON gr.game_id = lrg.game_id WHERE tr.title_id IN ({ids})"
    )
}

/// The kind a track plays as: the user's, or the one its loop and length say.
pub fn kind_of(manual: Option<&str>, shape: &rom_audio::Shape, inner_path: &str, name: &str, t: &Thresholds) -> SoundKind {
    manual.and_then(SoundKind::from_name).unwrap_or_else(|| rom_audio::classify(shape, &format!("{inner_path}/{name}"), t))
}

/// SQL that leaves out what can only be a sound effect, when sound effects are not wanted:
/// nothing shorter than the shortest BGM, jingle or loop is music.
fn length_filter(s: &Settings) -> String {
    if s.kinds == Kinds::All {
        return String::new();
    }
    let t = s.thresholds;
    let min = t.loop_min_ms.min(t.bgm_min_ms).min(t.jingle_min_ms);
    format!(" AND (t.duration_ms >= {min} OR t.kind_manual IS NOT NULL)")
}

fn row_to_track(row: &sqlx::mysql::MySqlRow, s: &Settings) -> Result<Option<AudioRow>> {
    let inner: Option<String> = row.try_get("inner_path")?;
    let name: String = row.try_get("name")?;
    let duration: u32 = row.try_get("duration_ms")?;
    let looping: bool = row.try_get("looping")?;
    let manual: Option<String> = row.try_get("kind_manual")?;
    let channels: u8 = row.try_get("channels")?;
    let cd_track: Option<u16> = row.try_get("cd_track")?;
    let data_track = row.try_get::<u16, _>("data_track")?;
    let rip = data_track == rip_sets::RIP_VIEW as u16 || data_track == rom_songs::ROM_SONG_VIEW as u16;
    let shape = rom_audio::Shape { looping, duration_ms: u64::from(duration), channels: u16::from(channels), cd_track: cd_track.unwrap_or(0), rip };
    // Hidden from the player by hand: it plays under no setting.
    if manual.as_deref() == Some(HIDDEN_KIND) {
        return Ok(None);
    }
    let kind = kind_of(manual.as_deref(), &shape, inner.as_deref().unwrap_or(""), &name, &s.thresholds);
    if !s.kinds.plays(kind) {
        return Ok(None);
    }
    build_row(row, kind)
}

/// A track row as the player gets it, played as `kind`.
fn build_row(row: &sqlx::mysql::MySqlRow, kind: SoundKind) -> Result<Option<AudioRow>> {
    let inner: Option<String> = row.try_get("inner_path")?;
    let name: String = row.try_get("name")?;
    let duration: u32 = row.try_get("duration_ms")?;
    let cd_track: Option<u16> = row.try_get("cd_track")?;
    let data_track = row.try_get::<u16, _>("data_track")?;
    let main_path: String = row.try_get("main_path")?;
    let rom_name = Path::new(&main_path).file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| main_path.clone());
    Ok(Some(AudioRow {
        remote: false,
        rom: true,
        rom_kind: Some(kind.name().to_string()),
        id: row.try_get("id")?,
        path: main_path,
        // A ROM song's stored path names its engine, not a file.
        file_name: inner.filter(|_| data_track != rom_songs::ROM_SONG_VIEW as u16).unwrap_or_else(|| name.clone()),
        size: row.try_get("bytes")?,
        format: row.try_get("codec")?,
        playable: true,
        support: "converted".to_string(),
        duration_ms: Some(duration),
        loudness_lufs: None,
        sound_end_ms: None,
        title: Some(name),
        album: Some(rom_name),
        artist: None,
        album_artist: None,
        disc_number: None,
        track_number: cd_track,
        year: None,
        deleted: false,
        tags: None,
        auto_tags: None,
    }))
}

/// The manual kind the player's "don't play this track" button gives a ROM track. Nothing
/// else sets a manual kind to it, so it is read as hidden: the track plays under no
/// `audio_rom_kinds` setting. A re-index keeps manual kinds, so it stays hidden.
pub const HIDDEN_KIND: &str = "se";

/// Hides a ROM track from title pages' players, or brings it back. A rip-library set is one
/// set for every ROM it matched (a title's regions), so all its rows for that track change.
pub async fn set_hidden(app: &App, id: u64, hidden: bool) -> Result<()> {
    let row = sqlx::query("SELECT data_track, inner_path, subsong FROM rom_audio_tracks WHERE id = ?")
        .bind(id)
        .fetch_optional(&app.pool)
        .await?
        .ok_or_else(|| Error::not_found(format!("no ROM audio track {id}")))?;
    let data_track: u16 = row.try_get("data_track")?;
    let inner: Option<String> = row.try_get("inner_path")?;
    let subsong: u32 = row.try_get("subsong")?;
    let value = hidden.then_some(HIDDEN_KIND);
    let library = data_track == rip_sets::RIP_VIEW as u16 && inner.as_deref().is_some_and(|p| Path::new(p.split('|').next().unwrap_or(p)).is_absolute());
    if library {
        sqlx::query("UPDATE rom_audio_tracks SET kind_manual = ? WHERE data_track = ? AND inner_path = ? AND subsong = ?")
            .bind(value)
            .bind(data_track)
            .bind(inner)
            .bind(subsong)
            .execute(&app.pool)
            .await?;
    } else {
        sqlx::query("UPDATE rom_audio_tracks SET kind_manual = ? WHERE id = ?").bind(value).bind(id).execute(&app.pool).await?;
    }
    Ok(())
}

/// The hidden ROM tracks of `titles`, for the player's "hidden tracks" list.
pub async fn hidden_tracks(app: &App, titles: &[u64]) -> Result<Vec<AudioRow>> {
    if titles.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT {TRACK_SELECT} FROM rom_audio_tracks t JOIN local_rom r ON r.id = t.rom_id AND r.deleted = FALSE WHERE t.rom_id IN ({}) AND t.kind_manual = '{HIDDEN_KIND}' ORDER BY r.main_path, t.location, t.cd_track, t.data_track, t.inner_path, t.subsong LIMIT {MAX_TRACKS}",
        roms_of(titles)
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(&app.pool).await?;
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for row in &rows {
        let key = (row.try_get::<Option<String>, _>("inner_path")?.unwrap_or_default(), row.try_get::<u32, _>("subsong")?, row.try_get::<u16, _>("data_track")?);
        if key.2 == rip_sets::RIP_VIEW as u16 && !seen.insert(key) {
            continue;
        }
        if let Some(track) = build_row(row, SoundKind::Se)? {
            out.push(track);
        }
    }
    Ok(out)
}

/// How many ROM tracks a page is handed at most.
const MAX_TRACKS: usize = 2000;
/// How many a random stand-in picks.
const RANDOM_TRACKS: usize = 50;

async fn tracks_where(app: &App, condition: &str, order: &str, s: &Settings, take: usize) -> Result<Vec<AudioRow>> {
    let filter = length_filter(s);
    let sql = format!("SELECT {TRACK_SELECT} FROM rom_audio_tracks t JOIN local_rom r ON r.id = t.rom_id AND r.deleted = FALSE WHERE {condition}{filter} ORDER BY {order} LIMIT 20000");
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(&app.pool).await?;
    let mut out = Vec::new();
    // A rip library set matched to several ROMs of a title (its regions) is listed once.
    let mut rips_seen = std::collections::HashSet::new();
    for row in &rows {
        if row.try_get::<u16, _>("data_track")? == rip_sets::RIP_VIEW as u16 {
            let key = (row.try_get::<Option<String>, _>("inner_path")?.unwrap_or_default(), row.try_get::<u32, _>("subsong")?);
            if Path::new(key.0.split('|').next().unwrap_or("")).is_absolute() && !rips_seen.insert(key) {
                continue;
            }
        }
        if let Some(track) = row_to_track(row, s)? {
            out.push(track);
            if out.len() >= take {
                break;
            }
        }
    }
    Ok(out)
}

/// The tracks in the ROMs of `titles` that `s` says a page plays, in disc order. Video
/// sound comes in by each title's own choice, else `audio_rom_videos`.
pub async fn title_tracks(app: &App, titles: &[u64], s: &Settings) -> Result<Vec<AudioRow>> {
    if titles.is_empty() {
        return Ok(Vec::new());
    }
    let mut groups = Vec::new();
    for &title in titles {
        let videos = videos_for(app, title, s).await?;
        groups.push(format!("(t.rom_id IN ({}){})", roms_of(&[title]), if videos { "" } else { " AND t.video = FALSE" }));
    }
    tracks_where(app, &format!("({})", groups.join(" OR ")), "r.main_path, t.location, t.cd_track, t.data_track, t.inner_path, t.subsong", s, MAX_TRACKS).await
}

/// A title's own choice on video sound, when it has one.
pub async fn title_choice(app: &App, title: u64) -> Result<Option<bool>> {
    Ok(sqlx::query_scalar("SELECT videos FROM rom_audio_title WHERE title_id = ?").bind(title).fetch_optional(&app.pool).await?)
}

/// Whether `title`'s video sound plays under `s`.
pub async fn videos_for(app: &App, title: u64, s: &Settings) -> Result<bool> {
    if s.videos_forced {
        return Ok(s.videos);
    }
    Ok(title_choice(app, title).await?.unwrap_or(s.videos))
}

/// Sets a title's own choice on video sound: `on`, `off`, or `default` (the setting decides).
pub async fn set_title_choice(app: &App, title: u64, choice: &str) -> Result<()> {
    match choice {
        "default" => {
            sqlx::query("DELETE FROM rom_audio_title WHERE title_id = ?").bind(title).execute(&app.pool).await?;
        }
        "on" | "off" => {
            sqlx::query("INSERT INTO rom_audio_title (title_id, videos) VALUES (?, ?) ON DUPLICATE KEY UPDATE videos = VALUES(videos)")
                .bind(title)
                .bind(choice == "on")
                .execute(&app.pool)
                .await
                .map_err(|e| if e.as_database_error().is_some_and(|d| d.is_foreign_key_violation()) { Error::not_found(format!("no title {title}")) } else { e.into() })?;
        }
        _ => return Err(Error::invalid("videos must be default, on or off")),
    }
    Ok(())
}

/// What a title page shows about video sound: its own choice, and whether its ROMs have any.
pub async fn title_videos(app: &App, title: u64) -> Result<super::audio::TitleVideos> {
    let choice = match title_choice(app, title).await? {
        None => "default",
        Some(true) => "on",
        Some(false) => "off",
    };
    let has_video: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT EXISTS (SELECT 1 FROM rom_audio_tracks t WHERE t.video = TRUE AND t.rom_id IN ({}))", roms_of(&[title])))).fetch_one(&app.pool).await?;
    Ok(super::audio::TitleVideos { title_id: title, choice: choice.to_string(), has_video })
}

/// The other versions of `titles`: the rest of their Definitive family (the original of a
/// remake, its ports), by `title.family_root_id`. A title with no root heads its own family.
async fn related_titles(app: &App, titles: &[u64]) -> Result<Vec<u64>> {
    let ids = titles.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT DISTINCT m.id FROM title t JOIN title m ON COALESCE(m.family_root_id, m.id) = COALESCE(t.family_root_id, t.id) \
         WHERE t.id IN ({ids}) AND m.id NOT IN ({ids})"
    )))
    .fetch_all(&app.pool)
    .await?)
}

/// ROM music for a title with none of its own, by `audio_rom_not_owned`; the mode that
/// chose them goes with them.
pub async fn stand_in(app: &App, titles: &[u64], s: &Settings) -> Result<(Vec<AudioRow>, NotOwned)> {
    match s.not_owned {
        NotOwned::None => Ok((Vec::new(), NotOwned::None)),
        NotOwned::Related => {
            if titles.is_empty() {
                return Ok((Vec::new(), NotOwned::Related));
            }
            let related = related_titles(app, titles).await?;
            Ok((title_tracks(app, &related, s).await?, NotOwned::Related))
        }
        NotOwned::Random => Ok((tracks_where(app, if s.videos { "1 = 1" } else { "t.video = FALSE" }, "RAND()", s, RANDOM_TRACKS).await?, NotOwned::Random)),
    }
}

// ---------------------------------------------------------------------------
// playing
// ---------------------------------------------------------------------------

/// Where a cached decode of track `id` goes: `rom<id>-<what>-<index time>.<ext>`. A ROM
/// indexed again gets new names; the old ones are left for the size cap.
fn cache_name(id: u64, target: Target, stamp: i64, level: bool, limit: bool) -> String {
    // `-ts`: the silent end is trimmed ([`TailTrim`]); earlier cache files without it are
    // left for the size cap. `-lm`: through the peak limiter ([`Limiter`]).
    format!(
        "rom{id}-{}-{stamp}{}{}-ts.{}",
        target.tag(),
        if level { "-lv" } else { "" },
        if limit { "-lm" } else { "" },
        target.ext()
    )
}

/// What a track is decoded to: AAC when the encoder is built and takes the track's shape,
/// FLAC otherwise or when `audio_dsd_cache_format` asks for lossless.
fn target_for(lossless: bool, sample_rate: u32, channels: u16) -> Target {
    if lossless || !xaac::available() || near_aac_rate(sample_rate).is_none() || channels > 2 {
        Target::Flac
    } else {
        Target::Aac { kbps: super::audio_convert::CACHE_AAC_KBPS }
    }
}

/// The file to play for ROM track `id`: decoded from the ROM on first play, then cached.
/// `level` is the player's "normalise": the track is measured as it is decoded and brought
/// to the player's loudness target, up as well as down ([`level_gain`]), and cached apart.
pub async fn serve(app: &App, id: u64, level: bool) -> Result<Served> {
    provide_ps2_bios(app).await?;
    let row = sqlx::query(
        "SELECT t.location, t.cd_track, t.data_track, t.inner_path, t.subsong, t.sample_rate, t.channels, t.looping, t.loop_start, t.loop_end, r.main_path, i.indexed_at \
         FROM rom_audio_tracks t JOIN local_rom r ON r.id = t.rom_id AND r.deleted = FALSE LEFT JOIN rom_audio_index i ON i.rom_id = t.rom_id WHERE t.id = ?",
    )
    .bind(id)
    .fetch_optional(&app.pool)
    .await?
    .ok_or_else(|| Error::not_found(format!("no ROM audio track {id}")))?;
    let main_path: String = row.try_get("main_path")?;
    let rom_id_files: Vec<String> = sqlx::query_scalar("SELECT f.path FROM rom_audio_tracks t JOIN local_rom_files rf ON rf.rom_id = t.rom_id JOIN local_files f ON f.id = rf.file_id AND f.deleted = FALSE WHERE t.id = ? ORDER BY f.path LIMIT 100000")
        .bind(id)
        .fetch_all(&app.pool)
        .await?;
    let data_track: u16 = row.try_get("data_track")?;
    let location = match row.try_get::<String, _>("location")?.as_str() {
        "cdda" => TrackLocation::Cdda { track: u32::from(row.try_get::<Option<u16>, _>("cd_track")?.unwrap_or(1)) },
        _ if u32::from(data_track) == rip_sets::RIP_VIEW => TrackLocation::Rip { path: row.try_get::<Option<String>, _>("inner_path")?.unwrap_or_default(), track: row.try_get("subsong")? },
        _ if u32::from(data_track) == rom_songs::ROM_SONG_VIEW => TrackLocation::RomSong { engine: row.try_get::<Option<String>, _>("inner_path")?.unwrap_or_default(), song: row.try_get("subsong")? },
        _ => TrackLocation::File { view: u32::from(data_track), path: row.try_get::<Option<String>, _>("inner_path")?.unwrap_or_default(), subsong: row.try_get("subsong")? },
    };
    // A rip-set track is read from the game's folder, a disc's from the ROM.
    let path = match &location {
        // A library set's path is absolute; a game folder's is relative to that folder.
        TrackLocation::Rip { path, .. } if Path::new(path.split('|').next().unwrap_or(path)).is_absolute() => Some(PathBuf::new()),
        TrackLocation::Rip { .. } => game_folder(&main_path),
        TrackLocation::RomSong { .. } => Some(PathBuf::from(&main_path)),
        _ => rom_path(&main_path, &rom_id_files),
    };
    let path = path.ok_or_else(|| Error::not_found(format!("ROM audio track {id}: the ROM is not on this machine")))?;
    if !path.as_os_str().is_empty() && !path.exists() {
        return Err(Error::not_found(format!("ROM audio track {id}: {} is missing", path.display())));
    }
    let lossless = db_settings::get::<String>(&app.pool, db_settings::KEY_AUDIO_DSD_CACHE_FORMAT).await?.as_deref() == Some("flac");
    let rate: u32 = row.try_get("sample_rate")?;
    let target = target_for(lossless, rate, row.try_get::<u8, _>("channels").map(u16::from)?);
    let loop_min_ms = Settings::load(app).await?.thresholds.loop_min_ms;
    let config = play_config(row.try_get("looping")?, row.try_get("loop_start")?, row.try_get("loop_end")?, rate, loop_min_ms);
    let stamp = row.try_get::<Option<chrono::NaiveDateTime>, _>("indexed_at")?.map(|t| t.and_utc().timestamp()).unwrap_or(0);

    let dir = super::audio_convert::cache_dir(app).await;
    // Music a sound chip makes (from the ROM's own code, or a library set's) can burst out
    // loud where code is read wrong; the limiter holds such bursts down.
    let chip = matches!(location, TrackLocation::RomSong { .. } | TrackLocation::Rip { .. });
    let limit = chip && db_settings::get::<bool>(&app.pool, db_settings::KEY_AUDIO_ROM_LIMITER).await?.unwrap_or(true);
    let name = cache_name(id, target, stamp, level, limit);
    let cached = dir.join(&name);
    let served = |body: Option<super::audio_ram::Bytes>| Served { mime: crate::database::media_store::mime_for(&cached), ext: target.ext().to_string(), path: cached.clone(), converted: true, note: None, body };
    if let Some(bytes) = super::audio_ram::get(&name) {
        return Ok(served(Some(bytes)));
    }
    let _turn = super::audio_convert::CONVERTING.lock().await;
    // Made while this request waited its turn.
    if let Some(bytes) = super::audio_ram::get(&name) {
        return Ok(served(Some(bytes)));
    }
    let disk_cap = db_settings::get::<u64>(&app.pool, db_settings::KEY_AUDIO_CACHE_MAX_MB).await?.unwrap_or(super::audio_convert::DEFAULT_CACHE_MB).saturating_mul(1 << 20);
    if cached.is_file() {
        let _ = std::fs::File::options().write(true).open(&cached).and_then(|f| f.set_modified(std::time::SystemTime::now()));
        return Ok(served(None));
    }
    let ram_cap = db_settings::get::<u64>(&app.pool, db_settings::KEY_AUDIO_RAM_CACHE_MB).await?.unwrap_or(super::audio_ram::DEFAULT_MB).saturating_mul(1 << 20);
    let (out, keep_dir) = (cached.clone(), dir.clone());
    let made = tokio::task::spawn_blocking(move || -> std::result::Result<Option<super::audio_ram::Bytes>, String> {
        decode_to(&path, &location, &out, target, config, level, limit)?;
        if ram_cap == 0 {
            super::audio_convert::evict(&keep_dir, disk_cap, &out);
            return Ok(None);
        }
        // Into RAM; the file it was decoded to is not kept, unless the track is too big for
        // the RAM cache and comes straight back.
        let bytes = super::audio_ram::Bytes::from(std::fs::read(&out).map_err(|e| format!("read {}: {e}", out.display()))?);
        let _ = std::fs::remove_file(&out);
        let pushed_out = super::audio_ram::put(&name, bytes.clone(), out.clone(), ram_cap);
        super::audio_ram::spill(&pushed_out);
        if !pushed_out.is_empty() {
            super::audio_convert::evict(&keep_dir, disk_cap, &out);
        }
        Ok(Some(bytes))
    })
    .await
    .map_err(|e| Error::internal(format!("ROM audio task: {e}")))?;
    match made {
        Err(reason) => Err(Error::unavailable(format!("ROM audio track {id} could not be decoded: {reason}"))),
        Ok(None) => Ok(served(None)),
        Ok(Some(bytes)) => Ok(served(Some(bytes))),
    }
}

/// Decodes one track out of the ROM at `rom` into `out`, written beside it and renamed so
/// the cache only ever holds whole files.
/// How a track is decoded for the player: its loop played twice and faded out, unless the
/// loop is shorter than `loop_min_ms` (a voice blip or a click flagged as looping would
/// otherwise become ten seconds of fade).
pub fn play_config(looping: bool, loop_start: u64, loop_end: u64, sample_rate: u32, loop_min_ms: u64) -> vgmstream::PlayConfig {
    let loop_ms = loop_end.saturating_sub(loop_start) * 1000 / u64::from(sample_rate.max(1));
    if looping && loop_ms >= loop_min_ms {
        vgmstream::PlayConfig::LISTEN
    } else {
        vgmstream::PlayConfig::AS_STORED
    }
}

/// An AAC rate within half a percent of `rate`: some banks (Xbox 360's XMA) store 47,998 or
/// 48,003 Hz for what was made at 48,000, a pitch difference nobody hears.
fn near_aac_rate(rate: u32) -> Option<u32> {
    xaac::SAMPLE_RATES.iter().copied().find(|&r| (i64::from(rate) - i64::from(r)).unsigned_abs() * 200 <= u64::from(r))
}

pub fn decode_to(rom: &Path, location: &TrackLocation, out: &Path, target: Target, config: vgmstream::PlayConfig, level: bool, limit: bool) -> std::result::Result<(), String> {
    let dir = out.parent().ok_or("no cache directory")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let partial = out.with_extension(format!("{}.part", target.ext()));
    let _ = std::fs::remove_file(&partial);
    let made = super::audio_convert::encode_with(&partial, target, |write| {
        let mut trim = TailTrim::default();
        let mut limiter = limit.then(Limiter::default);
        let mut sink = |pcm: &[i16], rate: u32, channels: u16| match limiter.as_mut() {
            Some(l) => trim.push(&l.apply(pcm, rate, channels), rate, channels, &mut *write),
            None => trim.push(pcm, rate, channels, &mut *write),
        };
        let made = (|| {
        let mut stream = rom_audio::open_track(rom, location, config)?;
        let (rate, channels) = (stream.sample_rate(), stream.channels());
        let rate = if matches!(target, Target::Aac { .. }) { near_aac_rate(rate).unwrap_or(rate) } else { rate };
        if !level {
            while let Some(pcm) = stream.next_chunk()? {
                sink(&pcm, rate, channels)?;
            }
            return Ok((rate, channels));
        }
        // Levelling needs the whole track's loudness before its first sample is written, so
        // it is held decoded until then -- up to LEVEL_MAX_SECONDS; past that, the rest is
        // written as it comes and the track keeps its own level.
        let limit = LEVEL_MAX_SECONDS * u64::from(stream.sample_rate()) * u64::from(channels);
        let mut held: Vec<Vec<i16>> = Vec::new();
        let mut count = 0u64;
        let mut whole = true;
        while let Some(pcm) = stream.next_chunk()? {
            count += pcm.len() as u64;
            held.push(pcm);
            if count > limit {
                whole = false;
                break;
            }
        }
        let gain = if whole { level_gain(&held, stream.sample_rate(), channels) } else { 1.0 };
        for pcm in held {
            sink(&scaled(pcm, gain), rate, channels)?;
        }
        while let Some(pcm) = stream.next_chunk()? {
            sink(&pcm, rate, channels)?;
        }
        Ok((rate, channels))
        })();
        trim.finish(&mut *write)?;
        made
    });
    match made {
        Ok(()) => std::fs::rename(&partial, out).map_err(|e| format!("move into the cache: {e}")),
        Err(e) => {
            let _ = std::fs::remove_file(&partial);
            Err(e)
        }
    }
}

/// Near-silence at the end of a track is left out: it is no music, and in the player it
/// is a gap before the next track (and before any crossfade can start). A sample this
/// small or smaller (-60 dBFS) counts as silent.
const TAIL_SILENCE: i16 = 32;
/// How much of a silent end is kept, so the last note's tail is not cut at the sample.
const TAIL_KEEP_MS: u64 = 150;

/// Holds back samples while they are silent, and writes them only when sound follows: what
/// is still held at the end is the silent end, and all but [`TAIL_KEEP_MS`] of it is dropped.
/// A silent start is dropped the same way, all but its last [`TAIL_KEEP_MS`].
#[derive(Default)]
struct TailTrim {
    held: Vec<i16>,
    shape: Option<(u32, u16)>,
    /// Sound has been heard: silence is held from here on, not dropped.
    started: bool,
}

type Write<'a> = dyn FnMut(&[i16], u32, u16) -> std::result::Result<(), String> + 'a;

impl TailTrim {
    fn push(&mut self, pcm: &[i16], rate: u32, channels: u16, write: &mut Write<'_>) -> std::result::Result<(), String> {
        self.shape = Some((rate, channels));
        let frame = usize::from(channels.max(1));
        // Up to the last frame with sound: written (after anything held); the rest is held.
        let loud = pcm.chunks(frame).rposition(|f| f.iter().any(|&v| v.unsigned_abs() > TAIL_SILENCE as u16));
        if !self.started {
            let first = pcm.chunks(frame).position(|f| f.iter().any(|&v| v.unsigned_abs() > TAIL_SILENCE as u16));
            let keep = (TAIL_KEEP_MS * u64::from(rate) / 1000) as usize * frame;
            match first {
                None => {
                    // Still silent: keep only the last stretch, which leads into the sound.
                    self.held.extend_from_slice(pcm);
                    if self.held.len() > keep {
                        self.held.drain(..self.held.len() - keep);
                    }
                    return Ok(());
                }
                Some(at) => {
                    self.started = true;
                    let lead = (at * frame).saturating_sub(keep);
                    if lead > 0 {
                        self.held.clear();
                    } else {
                        let have = self.held.len();
                        self.held.drain(..have.saturating_sub(keep - at * frame));
                    }
                    return self.push(&pcm[lead..], rate, channels, write);
                }
            }
        }
        match loud {
            Some(last) => {
                let cut = (last + 1) * frame;
                if !self.held.is_empty() {
                    write(&std::mem::take(&mut self.held), rate, channels)?;
                }
                write(&pcm[..cut], rate, channels)?;
                self.held.extend_from_slice(&pcm[cut..]);
            }
            None => self.held.extend_from_slice(pcm),
        }
        Ok(())
    }

    fn finish(&mut self, write: &mut Write<'_>) -> std::result::Result<(), String> {
        let Some((rate, channels)) = self.shape else { return Ok(()) };
        let keep = (TAIL_KEEP_MS * u64::from(rate) / 1000) as usize * usize::from(channels.max(1));
        let keep = keep.min(self.held.len());
        if keep > 0 {
            write(&self.held[..keep], rate, channels)?;
        }
        self.held.clear();
        Ok(())
    }
}

/// The longest track that is levelled (a movie's sound can run far longer than any song).
const LEVEL_MAX_SECONDS: u64 = 20 * 60;

/// How much track `pcm` is scaled by to sit at the player's loudness target
/// ([`audio_decode::LOUDNESS_TARGET_LUFS`]): down for a loud one, up for a quiet one, but
/// never so far up that its loudest sample passes -1 dBFS -- so nothing clips. A track too
/// short or too quiet to measure is left as it is.
pub fn level_gain(pcm: &[Vec<i16>], rate: u32, channels: u16) -> f32 {
    let Ok(mut meter) = ebur128::EbuR128::new(u32::from(channels), rate, ebur128::Mode::I | ebur128::Mode::SAMPLE_PEAK) else { return 1.0 };
    for chunk in pcm {
        if meter.add_frames_i16(chunk).is_err() {
            return 1.0;
        }
    }
    let Ok(lufs) = meter.loudness_global() else { return 1.0 };
    if !lufs.is_finite() {
        return 1.0;
    }
    let peak = (0..u32::from(channels)).filter_map(|c| meter.sample_peak(c).ok()).fold(0.0f64, f64::max);
    let wanted = 10f64.powf((f64::from(audio_decode::LOUDNESS_TARGET_LUFS) - lufs) / 20.0);
    let ceiling = if peak > 0.0 { 10f64.powf(-1.0 / 20.0) / peak } else { wanted };
    wanted.min(ceiling) as f32
}

/// Peaks a limited track stays under: -1 dBFS.
const LIMIT_CEILING: f32 = 0.891 * 32767.0;
/// How fast the limiter lets the level back up after a burst.
const LIMIT_RELEASE_MS: f32 = 250.0;

/// A peak limiter: holds every sample under [`LIMIT_CEILING`], turning a burst down at once and
/// letting the level back up over [`LIMIT_RELEASE_MS`]. Music that stays under the ceiling
/// passes unchanged. One gain for all channels, so the stereo picture holds.
#[derive(Default)]
struct Limiter {
    /// The gain now (1 when nothing is held down; 0 before the first sample).
    gain: f32,
    started: bool,
}

impl Limiter {
    fn apply(&mut self, pcm: &[i16], rate: u32, channels: u16) -> Vec<i16> {
        if !self.started {
            self.started = true;
            self.gain = 1.0;
        }
        let channels = usize::from(channels.max(1));
        let release = 1.0 - (-1000.0 / (LIMIT_RELEASE_MS * rate.max(1) as f32)).exp();
        let mut out = Vec::with_capacity(pcm.len());
        for frame in pcm.chunks(channels) {
            let peak = frame.iter().map(|&s| f32::from(s).abs()).fold(0.0, f32::max);
            let need = if peak > LIMIT_CEILING { LIMIT_CEILING / peak } else { 1.0 };
            self.gain = if need < self.gain { need } else { self.gain + (1.0 - self.gain) * release };
            let g = self.gain.min(need);
            out.extend(frame.iter().map(|&s| (f32::from(s) * g).round().clamp(-32768.0, 32767.0) as i16));
        }
        out
    }
}

fn scaled(mut pcm: Vec<i16>, gain: f32) -> Vec<i16> {
    if (gain - 1.0).abs() > 1e-3 {
        for s in &mut pcm {
            *s = (f32::from(*s) * gain).round().clamp(-32768.0, 32767.0) as i16;
        }
    }
    pcm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limiter_holds_a_burst_down_and_leaves_quiet_music_alone() {
        let mut l = Limiter::default();
        let quiet: Vec<i16> = (0..2000).map(|i| ((i % 50) as i16 - 25) * 400).collect();
        assert_eq!(l.apply(&quiet, 48_000, 2), quiet);
        let burst = vec![32767i16; 2000];
        let held = l.apply(&burst, 48_000, 2);
        assert!(held.iter().all(|&s| f32::from(s) <= LIMIT_CEILING + 1.0));
        // A second after the burst the level is back.
        let after = l.apply(&vec![10_000i16; 96_000], 48_000, 2);
        assert!(*after.last().unwrap() > 9_900);
    }

    fn tone(level: f32, peak_click: bool) -> Vec<Vec<i16>> {
        let mut pcm: Vec<i16> = (0..48_000 * 5).flat_map(|i| {
            let v = ((i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 48_000.0).sin() * level * 32767.0) as i16;
            [v, v]
        }).collect();
        if peak_click {
            pcm[1000] = 32000;
        }
        vec![pcm]
    }

    #[test]
    fn a_silent_end_is_left_out_but_a_quiet_middle_is_kept() {
        let mut out: Vec<i16> = Vec::new();
        let mut write = |pcm: &[i16], _: u32, _: u16| -> std::result::Result<(), String> { out.extend_from_slice(pcm); Ok(()) };
        let mut trim = TailTrim::default();
        // 1 kHz mono: 1 s of silence, sound, 0.5 s of silence, sound, then 2 s of silence.
        let sound = vec![5_000i16; 100];
        trim.push(&[0; 700], 1_000, 1, &mut write).unwrap();
        trim.push(&[0; 300], 1_000, 1, &mut write).unwrap();
        trim.push(&sound, 1_000, 1, &mut write).unwrap();
        trim.push(&[0; 500], 1_000, 1, &mut write).unwrap();
        trim.push(&sound, 1_000, 1, &mut write).unwrap();
        trim.push(&[10; 2_000], 1_000, 1, &mut write).unwrap();
        trim.finish(&mut write).unwrap();
        assert_eq!(out.len(), 150 + 100 + 500 + 100 + 150);
    }

    #[test]
    fn levelling_turns_loud_down_and_quiet_up_until_the_peak() {
        assert!(level_gain(&tone(0.9, false), 48_000, 2) < 1.0);
        // A quiet tone goes up, but stops short of clipping.
        let up = level_gain(&tone(0.05, false), 48_000, 2);
        assert!(up > 2.0 && up * 0.05 < 0.9, "{up}");
        // One loud sample holds the whole track down.
        assert!(level_gain(&tone(0.05, true), 48_000, 2) < 1.0);
        assert_eq!(level_gain(&[vec![0i16; 9600]], 48_000, 2), 1.0);
    }

    #[test]
    fn the_length_floor_is_the_lowest_of_the_three_thresholds() {
        let mut s = Settings::default();
        s.thresholds = Thresholds { loop_min_ms: 3000, bgm_min_ms: 1000, jingle_min_ms: 8000 };
        assert!(length_filter(&s).contains(">= 1000 "), "{}", length_filter(&s));
        s.kinds = Kinds::All;
        assert_eq!(length_filter(&s), "");
    }

    #[test]
    fn a_clients_settings_travel_to_the_main_server_and_back() {
        let mut mine = Settings::default();
        mine.sources = TitleSources::Tagged;
        mine.kinds = Kinds::BgmJingle;
        mine.thresholds = Thresholds { loop_min_ms: 2500, bgm_min_ms: 1000, jingle_min_ms: 4000 };
        mine.videos = true;
        let uri: axum::http::Uri = format!("/p?title_id=1&{}", mine.query(NotOwned::Random)).parse().unwrap();
        let axum::extract::Query(query) = axum::extract::Query::<super::super::audio::PlaylistQuery>::try_from_uri(&uri).unwrap();
        let theirs = Settings::default().overridden(&query).unwrap();
        assert_eq!((theirs.sources, theirs.kinds, theirs.not_owned), (TitleSources::Tagged, Kinds::BgmJingle, NotOwned::Random));
        assert_eq!((theirs.thresholds.loop_min_ms, theirs.thresholds.bgm_min_ms, theirs.thresholds.jingle_min_ms), (2500, 1000, 4000));
        assert!(theirs.videos && theirs.videos_forced, "the client's answer on video sound goes over the server's titles' own");
        let bad = super::super::audio::PlaylistQuery { kinds: Some("loud".into()), ..Default::default() };
        assert!(Settings::default().overridden(&bad).is_err());
    }

    #[test]
    fn a_kind_set_by_hand_wins_over_the_lengths() {
        let t = Thresholds::default();
        assert_eq!(kind_of(None, &rom_audio::Shape { looping: true, duration_ms: 60_000, channels: 2, cd_track: 0, rip: false }, "", "x", &t), SoundKind::Bgm);
        assert_eq!(kind_of(Some("se"), &rom_audio::Shape { looping: true, duration_ms: 60_000, channels: 2, cd_track: 0, rip: false }, "", "x", &t), SoundKind::Se);
        assert_eq!(kind_of(None, &rom_audio::Shape { looping: false, duration_ms: 1_000, channels: 2, cd_track: 0, rip: false }, "", "x", &t), SoundKind::Se);
    }

    #[test]
    fn the_kinds_setting_widens_in_steps() {
        assert!(Kinds::Bgm.plays(SoundKind::Bgm) && !Kinds::Bgm.plays(SoundKind::Jingle));
        assert!(Kinds::BgmJingle.plays(SoundKind::Jingle) && !Kinds::BgmJingle.plays(SoundKind::Se));
        assert!(Kinds::All.plays(SoundKind::Se));
    }

    #[test]
    fn a_track_the_encoder_cannot_take_is_kept_lossless() {
        assert_eq!(target_for(true, 44_100, 2), Target::Flac);
        assert_eq!(target_for(false, 37_800, 2), Target::Flac, "XA's rate is not an AAC rate");
        assert_eq!(target_for(false, 44_100, 6), Target::Flac);
        assert_eq!(near_aac_rate(47_998), Some(48_000));
        assert_eq!(near_aac_rate(48_003), Some(48_000));
        assert_eq!(near_aac_rate(47_758), None, "half a percent off is where pitch starts to tell");
        assert_eq!(near_aac_rate(51_200), None, "too far from any AAC rate to relabel");
    }

    #[test]
    fn a_short_loop_is_not_played_twice_and_faded() {
        assert_eq!(play_config(true, 0, 480, 48_000, 3000), vgmstream::PlayConfig::AS_STORED, "a 10 ms loop");
        assert_eq!(play_config(true, 0, 48_000 * 30, 48_000, 3000), vgmstream::PlayConfig::LISTEN);
        assert_eq!(play_config(false, 0, 48_000 * 30, 48_000, 3000), vgmstream::PlayConfig::AS_STORED);
    }

    #[test]
    fn a_rom_of_many_files_opens_as_their_folder() {
        let dir = std::env::temp_dir().join(format!("rx-rom-path-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("GAME/USRDIR")).unwrap();
        let files = [dir.join("GAME/PARAM.SFO"), dir.join("GAME/USRDIR/EBOOT.BIN")].map(|p| p.display().to_string());
        assert_eq!(rom_path(&files[1], &files), Some(dir.join("GAME")));
        assert_eq!(rom_path("/x/game.chd", &[]), Some(PathBuf::from("/x/game.chd")));
        assert_eq!(rom_path("/x/game.sfc", &["/x/game.sfc".to_string()]), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The message a panic carried, for the job's error list.
fn panic_text(payload: Box<dyn std::any::Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(s) => *s,
        Err(p) => p.downcast_ref::<&str>().map(|s| s.to_string()).unwrap_or_else(|| "unknown panic".into()),
    }
}
