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
                record_error(app, rom_id, &e).await?;
            }
        }
    }
    progress.force(total, Some(total.max(1)), &one_liner(&summary)).await;
    Ok(summary)
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
    let location: String = row.try_get("location")?;
    let shape = rom_audio::Shape { looping, duration_ms: u64::from(duration), channels: u16::from(channels), cd_audio: location == "cdda" };
    let kind = kind_of(manual.as_deref(), &shape, inner.as_deref().unwrap_or(""), &name, &s.thresholds);
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
fn cache_name(id: u64, target: Target, stamp: i64) -> String {
    format!("rom{id}-{}-{stamp}.{}", target.tag(), target.ext())
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
pub async fn serve(app: &App, id: u64) -> Result<Served> {
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
    let path = rom_path(&main_path, &rom_id_files).ok_or_else(|| Error::not_found(format!("ROM audio track {id}: the ROM is not on this machine")))?;
    if !path.exists() {
        return Err(Error::not_found(format!("ROM audio track {id}: {} is missing", path.display())));
    }
    let location = match row.try_get::<String, _>("location")?.as_str() {
        "cdda" => TrackLocation::Cdda { track: u32::from(row.try_get::<Option<u16>, _>("cd_track")?.unwrap_or(1)) },
        _ => TrackLocation::File { view: u32::from(row.try_get::<u16, _>("data_track")?), path: row.try_get::<Option<String>, _>("inner_path")?.unwrap_or_default(), subsong: row.try_get("subsong")? },
    };
    let lossless = db_settings::get::<String>(&app.pool, db_settings::KEY_AUDIO_DSD_CACHE_FORMAT).await?.as_deref() == Some("flac");
    let rate: u32 = row.try_get("sample_rate")?;
    let target = target_for(lossless, rate, row.try_get::<u8, _>("channels").map(u16::from)?);
    let loop_min_ms = Settings::load(app).await?.thresholds.loop_min_ms;
    let config = play_config(row.try_get("looping")?, row.try_get("loop_start")?, row.try_get("loop_end")?, rate, loop_min_ms);
    let stamp = row.try_get::<Option<chrono::NaiveDateTime>, _>("indexed_at")?.map(|t| t.and_utc().timestamp()).unwrap_or(0);

    let dir = super::audio_convert::cache_dir(app);
    let cached = dir.join(cache_name(id, target, stamp));
    let _turn = super::audio_convert::CONVERTING.lock().await;
    if !cached.is_file() {
        let out = cached.clone();
        let made = tokio::task::spawn_blocking(move || decode_to(&path, &location, &out, target, config)).await.map_err(|e| Error::internal(format!("ROM audio task: {e}")))?;
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

pub fn decode_to(rom: &Path, location: &TrackLocation, out: &Path, target: Target, config: vgmstream::PlayConfig) -> std::result::Result<(), String> {
    let dir = out.parent().ok_or("no cache directory")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let partial = out.with_extension(format!("{}.part", target.ext()));
    let _ = std::fs::remove_file(&partial);
    let made = super::audio_convert::encode_with(&partial, target, |sink| {
        let mut stream = rom_audio::open_track(rom, location, config)?;
        let (rate, channels) = (stream.sample_rate(), stream.channels());
        let rate = if matches!(target, Target::Aac { .. }) { near_aac_rate(rate).unwrap_or(rate) } else { rate };
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
        assert_eq!(kind_of(None, &rom_audio::Shape { looping: true, duration_ms: 60_000, channels: 2, cd_audio: false }, "", "x", &t), SoundKind::Bgm);
        assert_eq!(kind_of(Some("se"), &rom_audio::Shape { looping: true, duration_ms: 60_000, channels: 2, cd_audio: false }, "", "x", &t), SoundKind::Se);
        assert_eq!(kind_of(None, &rom_audio::Shape { looping: false, duration_ms: 1_000, channels: 2, cd_audio: false }, "", "x", &t), SoundKind::Se);
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
