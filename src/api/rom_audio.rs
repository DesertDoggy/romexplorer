//! Music inside ROMs, played on title pages like a CD player (`rom::rom_audio`).
//!
//! - **Indexing** (`rom_audio_index` job, [`run_index`]): each disc image or dumped folder is
//!   opened and every track found is recorded -- where it is and what it is, never the
//!   audio itself.
//! - **Playing** ([`serve`]): a browser cannot play a console's codecs, so a track is
//!   decoded on its first play (two loops and a fade, as a game plays it) into the same
//!   converted-audio cache DSD and ALAC use, and served from there. The cache's size cap
//!   (`audio_cache_max_mb`) applies; a track that was evicted is decoded again next time.
//! - **Playlists** ([`title_tracks`], [`stand_in`]): which tracks a title page plays, by the
//!   `audio_rom_*` settings. Whether a track is BGM, a jingle or a sound effect is worked out
//!   here, from its loop and length, so a changed threshold needs no re-index.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sqlx::Row;

use romexplorer_types::jobs::{FileScanMode, RomAudioIndexRequest};

use crate::database::jobs::Progress;
use crate::database::settings as db_settings;
use crate::rom::rom_audio::{self, FoundTrack, SoundKind, Thresholds, TrackLocation};
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
}

impl Default for Settings {
    fn default() -> Self {
        Self { sources: TitleSources::Both, kinds: Kinds::Bgm, not_owned: NotOwned::Related, thresholds: Thresholds::default(), keep_playing: true }
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
        })
    }
}

// ---------------------------------------------------------------------------
// the index job
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize)]
pub struct IndexSummary {
    pub mode: String,
    pub roms_seen: u64,
    pub roms_read: u64,
    pub skipped_existing: u64,
    /// Not a disc image or a folder: cartridges, archives.
    pub skipped_not_disc: u64,
    pub tracks: u64,
    pub errors: Vec<String>,
}

pub fn one_liner(s: &IndexSummary) -> String {
    let mut parts = vec![format!("{} ROM(s) read, {} track(s) found", s.roms_read, s.tracks)];
    if s.skipped_existing > 0 {
        parts.push(format!("{} already indexed", s.skipped_existing));
    }
    if s.skipped_not_disc > 0 {
        parts.push(format!("{} not a disc", s.skipped_not_disc));
    }
    if !s.errors.is_empty() {
        parts.push(format!("{} could not be read", s.errors.len()));
    }
    format!("done: {}", parts.join(", "))
}

pub async fn validate(_app: &App, _request: &RomAudioIndexRequest) -> Result<()> {
    if !vgmstream::available() {
        return Err(Error::unavailable("the game audio decoder (rvgmstream) is not built for this machine"));
    }
    Ok(())
}

/// Where to open a ROM from: its main file when that is a disc image, the folder it is a
/// dump of when it is made of many files, or nothing (a cartridge, an archive).
fn rom_path(main_path: &str, files: &[String]) -> Option<PathBuf> {
    let main = PathBuf::from(main_path);
    let ext = main.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    // A sheet names its own tracks; an archive's members are not files on disk.
    if matches!(ext.as_str(), "cue" | "gdi") || main.is_dir() {
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

pub async fn run_index(app: &App, request: &RomAudioIndexRequest, progress: &Progress) -> Result<IndexSummary> {
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
    let roms = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT r.id, r.main_path FROM local_rom r WHERE r.deleted = FALSE{scope}{skip} ORDER BY r.main_path")))
        .fetch_all(&app.pool)
        .await?;
    if quick {
        summary.skipped_existing = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM local_rom r WHERE r.deleted = FALSE{scope} AND EXISTS (SELECT 1 FROM rom_audio_index i WHERE i.rom_id = r.id)")))
            .fetch_one(&app.pool)
            .await?
            .max(0) as u64;
    }
    let total = roms.len() as u64;
    summary.roms_seen = total;
    for (i, row) in roms.iter().enumerate() {
        if progress.cancelled().await {
            break;
        }
        let rom_id: u64 = row.try_get("id")?;
        let main_path: String = row.try_get("main_path")?;
        let files: Vec<String> = sqlx::query_scalar("SELECT f.path FROM local_rom_files rf JOIN local_files f ON f.id = rf.file_id AND f.deleted = FALSE WHERE rf.rom_id = ? ORDER BY f.path LIMIT 100000")
            .bind(rom_id)
            .fetch_all(&app.pool)
            .await?;
        let Some(path) = rom_path(&main_path, &files) else {
            summary.skipped_not_disc += 1;
            continue;
        };
        let label = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| main_path.clone());
        progress.report(i as u64, Some(total), &format!("ROM {} of {total}: {label}", i + 1)).await;
        let open = path.clone();
        let found = tokio::task::spawn_blocking(move || rom_audio::index_rom(&open, &thresholds, |_| {})).await.map_err(|e| Error::internal(format!("ROM audio task: {e}")))?;
        match found {
            Ok(tracks) => {
                summary.tracks += tracks.len() as u64;
                summary.roms_read += 1;
                store(app, rom_id, &tracks, None).await?;
            }
            Err(e) => {
                summary.errors.push(format!("{}: {e}", path.display()));
                store(app, rom_id, &[], Some(&e)).await?;
            }
        }
    }
    progress.force(total, Some(total.max(1)), &one_liner(&summary)).await;
    Ok(summary)
}

/// A track's place in its ROM, as the key a user's own kind is kept by across re-indexes.
type PlaceKey = (String, Option<u16>, u16, String, u32);

fn place(location: &TrackLocation) -> (&'static str, Option<u16>, u16, Option<&str>, u32) {
    match location {
        TrackLocation::Cdda { track } => ("cdda", Some(*track as u16), 0, None, 0),
        TrackLocation::File { view, path, subsong } => ("file", None, *view as u16, Some(path.as_str()), *subsong),
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
            "INSERT INTO rom_audio_tracks (rom_id, location, cd_track, data_track, inner_path, subsong, name, kind_manual, sample_rate, channels, duration_ms, looping, loop_start, loop_end, codec, format, bytes) ",
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
                .push_bind(t.bytes);
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

const TRACK_SELECT: &str = "t.id, t.rom_id, t.location, t.cd_track, t.inner_path, t.subsong, t.name, t.kind_manual, t.sample_rate, t.channels, t.duration_ms, t.looping, t.codec, t.bytes, r.main_path";

/// The ROMs of `titles`, as SQL.
fn roms_of(titles: &[u64]) -> String {
    let ids = titles.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    format!(
        "SELECT gr.rom_id FROM title_releases tr JOIN local_release_games lrg ON lrg.release_id = tr.release_id \
         JOIN local_game_roms gr ON gr.game_id = lrg.game_id WHERE tr.title_id IN ({ids})"
    )
}

/// The kind a track plays as: the user's, or the one its loop and length say.
pub fn kind_of(manual: Option<&str>, looping: bool, duration_ms: u64, inner_path: &str, name: &str, t: &Thresholds) -> SoundKind {
    manual.and_then(SoundKind::from_name).unwrap_or_else(|| rom_audio::classify(looping, duration_ms, &format!("{inner_path}/{name}"), t))
}

/// SQL that leaves out what can only be a sound effect, when sound effects are not wanted:
/// nothing shorter than the shortest jingle or loop is music.
fn length_filter(s: &Settings) -> String {
    if s.kinds == Kinds::All {
        return String::new();
    }
    let min = s.thresholds.loop_min_ms.min(s.thresholds.jingle_min_ms);
    format!(" AND (t.duration_ms >= {min} OR t.kind_manual IS NOT NULL)")
}

fn row_to_track(row: &sqlx::mysql::MySqlRow, s: &Settings) -> Result<Option<AudioRow>> {
    let inner: Option<String> = row.try_get("inner_path")?;
    let name: String = row.try_get("name")?;
    let duration: u32 = row.try_get("duration_ms")?;
    let looping: bool = row.try_get("looping")?;
    let manual: Option<String> = row.try_get("kind_manual")?;
    let kind = kind_of(manual.as_deref(), looping, u64::from(duration), inner.as_deref().unwrap_or(""), &name, &s.thresholds);
    if !s.kinds.plays(kind) {
        return Ok(None);
    }
    let main_path: String = row.try_get("main_path")?;
    let rom_name = Path::new(&main_path).file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| main_path.clone());
    let cd_track: Option<u16> = row.try_get("cd_track")?;
    Ok(Some(AudioRow {
        remote: false,
        rom: true,
        rom_kind: Some(kind.name().to_string()),
        id: row.try_get("id")?,
        path: main_path,
        file_name: inner.unwrap_or_else(|| name.clone()),
        size: row.try_get("bytes")?,
        format: row.try_get("codec")?,
        playable: true,
        support: "converted".to_string(),
        duration_ms: Some(duration),
        loudness_lufs: None,
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

/// How many ROM tracks a page is handed at most.
const MAX_TRACKS: usize = 2000;
/// How many a random stand-in picks.
const RANDOM_TRACKS: usize = 50;

async fn tracks_where(app: &App, condition: &str, order: &str, s: &Settings, take: usize) -> Result<Vec<AudioRow>> {
    let filter = length_filter(s);
    let sql = format!("SELECT {TRACK_SELECT} FROM rom_audio_tracks t JOIN local_rom r ON r.id = t.rom_id AND r.deleted = FALSE WHERE {condition}{filter} ORDER BY {order} LIMIT 20000");
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(&app.pool).await?;
    let mut out = Vec::new();
    for row in &rows {
        if let Some(track) = row_to_track(row, s)? {
            out.push(track);
            if out.len() >= take {
                break;
            }
        }
    }
    Ok(out)
}

/// The tracks in the ROMs of `titles` that `s` says a page plays, in disc order.
pub async fn title_tracks(app: &App, titles: &[u64], s: &Settings) -> Result<Vec<AudioRow>> {
    if titles.is_empty() {
        return Ok(Vec::new());
    }
    tracks_where(app, &format!("t.rom_id IN ({})", roms_of(titles)), "r.main_path, t.location, t.cd_track, t.data_track, t.inner_path, t.subsong", s, MAX_TRACKS).await
}

/// The other versions of `titles`: their Definitive family (the original of a remake, its
/// ports).
async fn related_titles(app: &App, titles: &[u64]) -> Result<Vec<u64>> {
    let ids = titles.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT DISTINCT o2.title_id FROM title_family_order o1 JOIN title_family_order o2 ON o2.root_title_id = o1.root_title_id \
         WHERE o1.title_id IN ({ids}) AND o2.title_id NOT IN ({ids}) \
         UNION SELECT f.root_title_id FROM title_family_order f WHERE f.title_id IN ({ids}) AND f.root_title_id NOT IN ({ids})"
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
        NotOwned::Random => Ok((tracks_where(app, "1 = 1", "RAND()", s, RANDOM_TRACKS).await?, NotOwned::Random)),
    }
}

// ---------------------------------------------------------------------------
// playing
// ---------------------------------------------------------------------------

/// Where a cached decode of track `id` goes: `rom<id>-<what>-<index time>.<ext>`. A ROM
/// indexed again gets new names; the old ones are left for the size cap.
fn cache_name(id: u64, target: Target, stamp: i64) -> String {
    format!("rom{id}-{}-{stamp}.{}", target.tag(), target.ext())
}

/// What a track is decoded to: AAC when the encoder is built and takes the track's shape,
/// FLAC otherwise or when `audio_dsd_cache_format` asks for lossless.
fn target_for(lossless: bool, sample_rate: u32, channels: u16) -> Target {
    if lossless || !xaac::available() || !xaac::SAMPLE_RATES.contains(&sample_rate) || channels > 2 {
        Target::Flac
    } else {
        Target::Aac { kbps: super::audio_convert::CACHE_AAC_KBPS }
    }
}

/// The file to play for ROM track `id`: decoded from the ROM on first play, then cached.
pub async fn serve(app: &App, id: u64) -> Result<Served> {
    let row = sqlx::query(
        "SELECT t.location, t.cd_track, t.data_track, t.inner_path, t.subsong, t.sample_rate, t.channels, r.main_path, i.indexed_at \
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
    let path = rom_path(&main_path, &rom_id_files).ok_or_else(|| Error::not_found(format!("ROM audio track {id}: the ROM is not on this machine")))?;
    if !path.exists() {
        return Err(Error::not_found(format!("ROM audio track {id}: {} is missing", path.display())));
    }
    let location = match row.try_get::<String, _>("location")?.as_str() {
        "cdda" => TrackLocation::Cdda { track: u32::from(row.try_get::<Option<u16>, _>("cd_track")?.unwrap_or(1)) },
        _ => TrackLocation::File { view: u32::from(row.try_get::<u16, _>("data_track")?), path: row.try_get::<Option<String>, _>("inner_path")?.unwrap_or_default(), subsong: row.try_get("subsong")? },
    };
    let lossless = db_settings::get::<String>(&app.pool, db_settings::KEY_AUDIO_DSD_CACHE_FORMAT).await?.as_deref() == Some("flac");
    let target = target_for(lossless, row.try_get("sample_rate")?, row.try_get::<u8, _>("channels").map(u16::from)?);
    let stamp = row.try_get::<Option<chrono::NaiveDateTime>, _>("indexed_at")?.map(|t| t.and_utc().timestamp()).unwrap_or(0);

    let dir = super::audio_convert::cache_dir(app);
    let cached = dir.join(cache_name(id, target, stamp));
    let _turn = super::audio_convert::CONVERTING.lock().await;
    if !cached.is_file() {
        let out = cached.clone();
        let made = tokio::task::spawn_blocking(move || decode_to(&path, &location, &out, target)).await.map_err(|e| Error::internal(format!("ROM audio task: {e}")))?;
        if let Err(reason) = made {
            return Err(Error::unavailable(format!("ROM audio track {id} could not be decoded: {reason}")));
        }
        let cap = db_settings::get::<u64>(&app.pool, db_settings::KEY_AUDIO_CACHE_MAX_MB).await?.unwrap_or(super::audio_convert::DEFAULT_CACHE_MB);
        let (dir, keep) = (dir.clone(), cached.clone());
        let _ = tokio::task::spawn_blocking(move || super::audio_convert::evict(&dir, cap.saturating_mul(1 << 20), &keep)).await;
    } else {
        let _ = std::fs::File::options().write(true).open(&cached).and_then(|f| f.set_modified(std::time::SystemTime::now()));
    }
    Ok(Served { mime: crate::database::media_store::mime_for(&cached), ext: target.ext().to_string(), path: cached, converted: true, note: None })
}

/// Decodes one track out of the ROM at `rom` into `out`, written beside it and renamed so
/// the cache only ever holds whole files.
pub fn decode_to(rom: &Path, location: &TrackLocation, out: &Path, target: Target) -> std::result::Result<(), String> {
    let dir = out.parent().ok_or("no cache directory")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let partial = out.with_extension(format!("{}.part", target.ext()));
    let _ = std::fs::remove_file(&partial);
    let made = super::audio_convert::encode_with(&partial, target, |sink| {
        let mut stream = rom_audio::open_track(rom, location, vgmstream::PlayConfig::LISTEN)?;
        let (rate, channels) = (stream.sample_rate(), stream.channels());
        while let Some(pcm) = stream.next_chunk()? {
            sink(&pcm, rate, channels)?;
        }
        Ok((rate, channels))
    });
    match made {
        Ok(()) => std::fs::rename(&partial, out).map_err(|e| format!("move into the cache: {e}")),
        Err(e) => {
            let _ = std::fs::remove_file(&partial);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kind_set_by_hand_wins_over_the_lengths() {
        let t = Thresholds::default();
        assert_eq!(kind_of(None, true, 60_000, "", "x", &t), SoundKind::Bgm);
        assert_eq!(kind_of(Some("se"), true, 60_000, "", "x", &t), SoundKind::Se);
        assert_eq!(kind_of(None, false, 1_000, "", "x", &t), SoundKind::Se);
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
