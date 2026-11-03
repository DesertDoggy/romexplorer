//! The audio library: scanning the audio folders, listing and tagging what was found, and
//! the playlist a title plays.
//!
//! - **Scan** (`audio_scan` job): every configured audio folder (Config > Storage, role
//!   `audio`), walked to any depth -- music is kept as `artist/album/track` -- with each
//!   file's tags read by `lofty` (pure Rust, so the same on every target). *Full* re-reads
//!   everything and marks files that have gone as deleted; *quick* (`new_only`) only reads
//!   files it has no live row for.
//! - **Tags** are keyword names (`audio_file_tags`, see migration 0034 for why names and not
//!   ids). Tagging a file also makes sure the keyword exists, so it is offered on titles.
//! - **Playlist**: every playable file sharing at least one keyword name with the title.
//!   One match is enough -- a soundtrack tagged with the series' keyword plays for every
//!   game of the series.
//!
//! Every file is listed. What the browser plays itself is `direct`; DSD, ALAC and AIFF are
//! `converted` -- served from a conversion made on first use (`super::audio_convert`) -- and
//! what nothing here decodes (WMA, APE...) is `unsupported`: `playable = false`, left out of
//! every playlist and never copied to a client.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::database::import::sha1_hex;
use crate::database::jobs::Progress;

use super::{clamp_page, like_pattern, App, Error, Result};

pub use romexplorer_types::jobs::{AudioScanRequest, FileScanMode};

/// Extensions the scan takes for audio. Everything here is listed;
/// [`super::audio_convert::Support`] says which of them the player can use, and how.
pub const AUDIO_EXTENSIONS: [&str; 22] = ["mp3", "m4a", "m4b", "aac", "flac", "wav", "ogg", "oga", "opus", "aiff", "aif", "ape", "wv", "wma", "mpc", "tta", "dsf", "dff", "mka", "spx", "alac", "mp4"];

#[derive(Debug, Clone, Default, Serialize)]
pub struct AudioScanSummary {
    pub mode: String,
    pub folders: Vec<String>,
    pub files_seen: u64,
    pub files_read: u64,
    pub skipped_existing: u64,
    pub marked_missing: u64,
    pub unplayable: u64,
    /// Files whose loudness was measured in this run (for the player's "normalise").
    pub loudness_measured: u64,
    pub errors: Vec<String>,
}

/// What a file's tags say. Every field is optional: plenty of files have none.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioTags {
    pub title: Option<String>,
    pub album: Option<String>,
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub disc_number: Option<u16>,
    pub track_number: Option<u16>,
    pub year: Option<u16>,
    pub duration_ms: Option<u32>,
    /// False when the container is one the browser plays but the codec inside is not
    /// (ALAC in an `.m4a`).
    pub codec_playable: bool,
}

/// "3", "3/12", " 03 " -> 3.
fn leading_number(text: &str) -> Option<u16> {
    let digits: String = text.trim().chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok().filter(|n| *n > 0)
}

/// The year out of "1994", "1994-07-15", "1994-07-15T00:00:00Z".
fn leading_year(text: &str) -> Option<u16> {
    let digits: String = text.trim().chars().take_while(|c| c.is_ascii_digit()).collect();
    (digits.len() == 4).then(|| digits.parse().ok()).flatten().filter(|y| (1000..=9999).contains(y))
}

/// Reads one file's tags and properties. A file `lofty` cannot parse (DSD, WMA, a damaged
/// file) is not an error for the scan: it is listed with no tags.
pub fn read_tags(path: &Path) -> AudioTags {
    use lofty::prelude::*;
    let mut out = AudioTags { codec_playable: true, ..Default::default() };
    let Ok(tagged) = lofty::read_from_path(path) else { return out };
    let millis = tagged.properties().duration().as_millis();
    out.duration_ms = (millis > 0).then(|| millis.min(u32::MAX as u128) as u32);
    if tagged.file_type() == lofty::file::FileType::Mp4 {
        // Same container, different codec: only AAC (and the rare MP3/FLAC-in-MP4) plays.
        if let Ok(mut file) = std::fs::File::open(path) {
            if let Ok(mp4) = lofty::mp4::Mp4File::read_from(&mut file, lofty::config::ParseOptions::new()) {
                out.codec_playable = !matches!(mp4.properties().codec(), Some(lofty::mp4::Mp4Codec::ALAC));
            }
        }
    }
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else { return out };
    let text = |key: ItemKey| tag.get_string(key).map(|s| s.trim().chars().take(512).collect::<String>()).filter(|s| !s.is_empty());
    out.title = text(ItemKey::TrackTitle);
    out.album = text(ItemKey::AlbumTitle);
    out.artist = text(ItemKey::TrackArtist);
    out.album_artist = text(ItemKey::AlbumArtist);
    out.disc_number = tag.get_string(ItemKey::DiscNumber).and_then(leading_number);
    out.track_number = tag.get_string(ItemKey::TrackNumber).and_then(leading_number);
    out.year = tag.get_string(ItemKey::RecordingDate).and_then(leading_year).or_else(|| tag.get_string(ItemKey::Year).and_then(leading_year));
    out
}

fn extension_of(path: &Path) -> Option<String> {
    path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase)
}

/// Every audio file under `root`, at any depth. Unreadable directories are skipped and
/// reported rather than ending the walk; symlinked directories are not followed, so a loop
/// in the tree cannot hang it.
fn walk(root: &Path, files: &mut Vec<PathBuf>, errors: &mut Vec<String>) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                errors.push(format!("{}: {e}", dir.display()));
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() && extension_of(&path).is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.as_str())) {
                files.push(path);
            }
        }
    }
}

/// The configured audio folders.
async fn audio_roots(app: &App) -> Result<Vec<PathBuf>> {
    Ok(crate::database::storage::list_media_dirs(&app.pool).await?.into_iter().filter(|d| d.role == "audio").map(|d| PathBuf::from(d.path)).collect())
}

pub async fn validate(app: &App, _request: &AudioScanRequest) -> Result<()> {
    if audio_roots(app).await?.is_empty() {
        return Err(Error::invalid("no audio folder is configured -- add one under Config > Storage (role: audio) first"));
    }
    Ok(())
}

pub async fn run_scan(app: &App, request: &AudioScanRequest, progress: &Progress) -> Result<AudioScanSummary> {
    let roots = audio_roots(app).await?;
    // `rehash` has no meaning of its own here: re-reading every file is what `full` does.
    let quick = request.mode == FileScanMode::NewOnly;
    let mut summary = AudioScanSummary { mode: if quick { "new_only" } else { "full" }.to_string(), folders: roots.iter().map(|r| r.display().to_string()).collect(), ..Default::default() };

    progress.force(0, None, "walking the audio folders").await;
    let (files, walk_errors) = {
        let roots = roots.clone();
        tokio::task::spawn_blocking(move || {
            let (mut files, mut errors) = (Vec::new(), Vec::new());
            for root in &roots {
                if root.is_dir() {
                    walk(root, &mut files, &mut errors);
                } else {
                    errors.push(format!("{} is not a folder on this machine", root.display()));
                }
            }
            files.sort();
            (files, errors)
        })
        .await
        .map_err(|e| Error::internal(format!("audio walk task: {e}")))?
    };
    summary.errors = walk_errors;
    summary.files_seen = files.len() as u64;

    // A full scan marks everything gone first and un-marks what the walk finds, the way the
    // ROM scan does: what is left marked is what is no longer there.
    if !quick {
        // Not the copies fetched from a main server (`audio_sync`): those may live outside
        // the scanned folders, and that job is what keeps them.
        sqlx::query("UPDATE audio_files SET deleted = TRUE WHERE deleted = FALSE AND remote_id IS NULL").execute(&app.pool).await?;
    }
    let total = files.len() as u64;
    for (i, path) in files.iter().enumerate() {
        let text = path.display().to_string();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| text.clone());
        progress.report(i as u64, Some(total), &format!("file {} of {total}: {name}", i + 1)).await;
        let hash = sha1_hex(&text);
        if quick && sqlx::query("SELECT 1 FROM audio_files WHERE path_hash = ? AND deleted = FALSE").bind(&hash).fetch_optional(&app.pool).await?.is_some() {
            summary.skipped_existing += 1;
            continue;
        }
        // A copy fetched from the main server carries the server's metadata, and a converted
        // one has no tags of its own: re-reading it would blank its title and album.
        if sqlx::query("SELECT 1 FROM audio_files WHERE path_hash = ? AND remote_id IS NOT NULL").bind(&hash).fetch_optional(&app.pool).await?.is_some() {
            summary.skipped_existing += 1;
            continue;
        }
        let read_path = path.clone();
        let (tags, size, modified) = match tokio::task::spawn_blocking(move || {
            let meta = std::fs::metadata(&read_path)?;
            let modified = meta.modified().ok().map(|t| chrono::DateTime::<chrono::Local>::from(t).naive_local());
            Ok::<_, std::io::Error>((read_tags(&read_path), meta.len(), modified))
        })
        .await
        {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                summary.errors.push(format!("{text}: {e}"));
                continue;
            }
            Err(e) => {
                summary.errors.push(format!("{text}: {e}"));
                continue;
            }
        };
        let format = extension_of(path).unwrap_or_default();
        let support = super::audio_convert::Support::of(&format, tags.codec_playable);
        let playable = support != super::audio_convert::Support::Unsupported;
        if !playable {
            summary.unplayable += 1;
        }
        sqlx::query(
            "INSERT INTO audio_files (path, path_hash, file_name, size, last_modified, format, playable, support, duration_ms, title, album, artist, album_artist, disc_number, track_number, year, scanned_at, deleted) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NOW(), FALSE) \
             ON DUPLICATE KEY UPDATE loudness_lufs = IF(size <=> VALUES(size) AND last_modified <=> VALUES(last_modified), loudness_lufs, NULL), path = VALUES(path), file_name = VALUES(file_name), size = VALUES(size), last_modified = VALUES(last_modified), format = VALUES(format), playable = VALUES(playable), support = VALUES(support), \
               duration_ms = VALUES(duration_ms), title = VALUES(title), album = VALUES(album), artist = VALUES(artist), album_artist = VALUES(album_artist), disc_number = VALUES(disc_number), \
               track_number = VALUES(track_number), year = VALUES(year), scanned_at = NOW(), deleted = FALSE",
        )
        .bind(&text)
        .bind(&hash)
        .bind(name.chars().take(512).collect::<String>())
        .bind(size)
        .bind(modified)
        .bind(&format)
        .bind(playable)
        .bind(support.name())
        .bind(tags.duration_ms)
        .bind(&tags.title)
        .bind(&tags.album)
        .bind(&tags.artist)
        .bind(&tags.album_artist)
        .bind(tags.disc_number)
        .bind(tags.track_number)
        .bind(tags.year)
        .execute(&app.pool)
        .await?;
        summary.files_read += 1;
    }
    if !quick {
        summary.marked_missing = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audio_files WHERE deleted = TRUE").fetch_one(&app.pool).await?.max(0) as u64;
    }
    summary.loudness_measured = measure_loudness(app, progress).await?;
    progress.force(total, Some(total.max(1)), &one_liner(&summary)).await;
    Ok(summary)
}

/// Measures every file that has no loudness yet: new files, files that changed, and a
/// library scanned before this existed. Each is decoded once, start to end, several at a
/// time -- the slow part of a first scan, and nothing at all on the next one.
///
/// Only what symphonia decodes. DSD is left unmeasured (it would have to be converted
/// first) and so plays un-normalised, as does anything unsupported. A client's fetched
/// copies carry the server's measurement and are not re-measured.
async fn measure_loudness(app: &App, progress: &Progress) -> Result<u64> {
    let pending: Vec<(u64, String)> =
        sqlx::query_as("SELECT id, path FROM audio_files WHERE deleted = FALSE AND loudness_lufs IS NULL AND remote_id IS NULL AND support <> 'unsupported' AND format NOT IN ('dsf', 'dff') ORDER BY id").fetch_all(&app.pool).await?;
    if pending.is_empty() {
        return Ok(0);
    }
    let total = pending.len() as u64;
    // Half the cores: a scan runs beside whatever else the machine is doing.
    let workers = (std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2) / 2).max(1);
    let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(workers));
    let mut tasks = tokio::task::JoinSet::new();
    for (id, path) in pending {
        let permit = gate.clone().acquire_owned().await.map_err(|e| Error::internal(format!("loudness gate: {e}")))?;
        tasks.spawn_blocking(move || {
            let _permit = permit;
            (id, crate::rom::audio_decode::loudness_lufs(Path::new(&path)))
        });
    }
    let (mut done, mut measured) = (0u64, 0u64);
    while let Some(result) = tasks.join_next().await {
        done += 1;
        if let Ok((id, Some(lufs))) = result {
            sqlx::query("UPDATE audio_files SET loudness_lufs = ? WHERE id = ?").bind(lufs).bind(id).execute(&app.pool).await?;
            measured += 1;
        }
        progress.report(done, Some(total), &format!("measuring loudness: file {done} of {total}")).await;
    }
    Ok(measured)
}

pub fn one_liner(s: &AudioScanSummary) -> String {
    let mut parts = vec![format!("{} file(s) found, {} read", s.files_seen, s.files_read)];
    if s.skipped_existing > 0 {
        parts.push(format!("{} already known", s.skipped_existing));
    }
    if s.unplayable > 0 {
        parts.push(format!("{} in a format nothing here can decode", s.unplayable));
    }
    if s.loudness_measured > 0 {
        parts.push(format!("loudness measured for {}", s.loudness_measured));
    }
    if s.marked_missing > 0 {
        parts.push(format!("{} no longer there", s.marked_missing));
    }
    if !s.errors.is_empty() {
        parts.push(format!("{} error(s)", s.errors.len()));
    }
    format!("done: {}", parts.join(", "))
}

// ---------------------------------------------------------------------------
// the list
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AudioQuery {
    /// Matches the file name, title, album or artist.
    pub search: Option<String>,
    pub title: Option<String>,
    pub album: Option<String>,
    pub artist: Option<String>,
    pub year: Option<u16>,
    /// Only files carrying this keyword tag; `-` = only files with no tag at all.
    pub tag: Option<String>,
    /// manual | auto: only tags put there by the user / by the auto-tagger (with `tag`: that
    /// tag, put there that way).
    pub tag_source: Option<String>,
    pub include_deleted: Option<bool>,
    pub sort: Option<String>,
    #[serde(default)]
    pub desc: bool,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioRow {
    /// On the main server, not here: `id` is the server's, and the file is streamed from it.
    #[serde(default)]
    pub remote: bool,
    pub id: u64,
    pub path: String,
    pub file_name: String,
    pub size: u64,
    pub format: String,
    pub playable: bool,
    /// direct | converted | unsupported (`audio_convert::Support`). Defaulted for a main
    /// server too old to send it.
    #[serde(default = "default_support")]
    pub support: String,
    pub duration_ms: Option<u32>,
    /// Integrated loudness in LUFS, when it has been measured.
    #[serde(default)]
    pub loudness_lufs: Option<f32>,
    pub title: Option<String>,
    pub album: Option<String>,
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub disc_number: Option<u16>,
    pub track_number: Option<u16>,
    pub year: Option<u16>,
    pub deleted: bool,
    /// Keyword tags, comma separated.
    pub tags: Option<String>,
    /// Of those, the ones the auto-tagger put there.
    #[serde(default)]
    pub auto_tags: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioPage {
    pub items: Vec<AudioRow>,
    pub total: u64,
    pub limit: u32,
    pub offset: u32,
}

fn default_support() -> String {
    "direct".to_string()
}

const AUDIO_SELECT: &str = "a.id, a.path, a.file_name, a.size, a.format, a.playable, a.support, a.duration_ms, a.loudness_lufs, a.title, a.album, a.artist, a.album_artist, a.disc_number, a.track_number, a.year, a.deleted, \
    (SELECT GROUP_CONCAT(t.name ORDER BY t.name SEPARATOR ',') FROM audio_file_tags t WHERE t.audio_file_id = a.id) AS tags, \
    (SELECT GROUP_CONCAT(t.name ORDER BY t.name SEPARATOR ',') FROM audio_file_tags t WHERE t.audio_file_id = a.id AND t.source = 'auto') AS auto_tags";

fn row_to_audio(row: &sqlx::mysql::MySqlRow) -> Result<AudioRow> {
    Ok(AudioRow {
        remote: false,
        id: row.try_get("id")?,
        path: row.try_get("path")?,
        file_name: row.try_get("file_name")?,
        size: row.try_get("size")?,
        format: row.try_get("format")?,
        playable: row.try_get("playable")?,
        support: row.try_get("support")?,
        duration_ms: row.try_get("duration_ms")?,
        loudness_lufs: row.try_get("loudness_lufs")?,
        title: row.try_get("title")?,
        album: row.try_get("album")?,
        artist: row.try_get("artist")?,
        album_artist: row.try_get("album_artist")?,
        disc_number: row.try_get("disc_number")?,
        track_number: row.try_get("track_number")?,
        year: row.try_get("year")?,
        deleted: row.try_get("deleted")?,
        tags: row.try_get("tags")?,
        auto_tags: row.try_get("auto_tags")?,
    })
}

pub async fn list(app: &App, q: &AudioQuery) -> Result<AudioPage> {
    let mut conditions = String::from("WHERE 1 = 1");
    let mut binds: Vec<String> = Vec::new();
    if !q.include_deleted.unwrap_or(false) {
        conditions.push_str(" AND a.deleted = FALSE");
    }
    if let Some(s) = q.search.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        // Also the query's other-language versions (name dictionary): ホゲクエスト finds an
        // album tagged "Hoge Quest Original Soundtrack".
        let (cond, pats) = crate::database::dictionary::search_condition(&app.pool, s, &["a.file_name LIKE ?", "a.title LIKE ?", "a.album LIKE ?", "a.artist LIKE ?", "a.album_artist LIKE ?"], like_pattern(s)).await?;
        conditions.push_str(&format!(" AND {cond}"));
        binds.extend(pats);
    }
    if let Some(v) = q.title.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        conditions.push_str(" AND a.title LIKE ?");
        binds.push(like_pattern(v));
    }
    // Album and artist come from the filter lists, so they match whole values.
    for (column, value) in [("a.album", &q.album), ("a.artist", &q.artist)] {
        if let Some(v) = value.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            conditions.push_str(&format!(" AND {column} = ?"));
            binds.push(v.to_string());
        }
    }
    if let Some(year) = q.year {
        conditions.push_str(&format!(" AND a.year = {year}"));
    }
    let source = match q.tag_source.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s @ ("manual" | "auto")) => Some(s),
        Some(other) => return Err(Error::invalid(format!("tag_source {other:?} must be manual or auto"))),
        None => None,
    };
    let by_source = if source.is_some() { " AND t0.source = ?" } else { "" };
    match q.tag.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some("-") => conditions.push_str(" AND NOT EXISTS (SELECT 1 FROM audio_file_tags t0 WHERE t0.audio_file_id = a.id)"),
        Some(tag) => {
            conditions.push_str(&format!(" AND EXISTS (SELECT 1 FROM audio_file_tags t0 WHERE t0.audio_file_id = a.id AND t0.name = ?{by_source})"));
            binds.push(tag.to_string());
            binds.extend(source.map(str::to_string));
        }
        None => {
            if let Some(s) = source {
                conditions.push_str(" AND EXISTS (SELECT 1 FROM audio_file_tags t0 WHERE t0.audio_file_id = a.id AND t0.source = ?)");
                binds.push(s.to_string());
            }
        }
    }
    let direction = if q.desc { "DESC" } else { "ASC" };
    // An allowlist: the column goes into the statement as text.
    let order = match q.sort.as_deref().unwrap_or("") {
        "file_name" => format!("a.file_name {direction}"),
        "title" => format!("a.title {direction}, a.file_name"),
        "artist" => format!("a.artist {direction}, a.album, a.disc_number, a.track_number"),
        "year" => format!("a.year {direction}, a.album, a.disc_number, a.track_number"),
        "duration" => format!("a.duration_ms {direction}"),
        "size" => format!("a.size {direction}"),
        "format" => format!("a.format {direction}, a.file_name"),
        "id" => format!("a.id {direction}"),
        "" | "album" => format!("a.album {direction}, a.disc_number, a.track_number, a.file_name"),
        other => return Err(Error::invalid(format!("unknown sort {other:?}"))),
    };
    let (limit, offset) = clamp_page(q.limit, q.offset);
    let count_sql = format!("SELECT COUNT(*) FROM audio_files a {conditions}");
    let mut count = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(count_sql));
    for b in &binds {
        count = count.bind(b);
    }
    let total = count.fetch_one(&app.pool).await?.max(0) as u64;
    let sql = format!("SELECT {AUDIO_SELECT} FROM audio_files a {conditions} ORDER BY {order} LIMIT ? OFFSET ?");
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    for b in &binds {
        query = query.bind(b);
    }
    let rows = query.bind(limit).bind(offset).fetch_all(&app.pool).await?;
    Ok(AudioPage { items: rows.iter().map(row_to_audio).collect::<Result<_>>()?, total, limit, offset })
}

/// What the list's filters offer: the tags in use, and the artists, albums and years found.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AudioFacets {
    pub tags: Vec<String>,
    pub artists: Vec<String>,
    pub albums: Vec<String>,
    pub years: Vec<u16>,
}

pub async fn facets(app: &App) -> Result<AudioFacets> {
    let pool = &app.pool;
    Ok(AudioFacets {
        tags: tags(app).await?.into_iter().map(|(name, _)| name).collect(),
        artists: sqlx::query_scalar("SELECT DISTINCT artist FROM audio_files WHERE deleted = FALSE AND artist IS NOT NULL ORDER BY artist LIMIT 2000").fetch_all(pool).await?,
        albums: sqlx::query_scalar("SELECT DISTINCT album FROM audio_files WHERE deleted = FALSE AND album IS NOT NULL ORDER BY album LIMIT 5000").fetch_all(pool).await?,
        years: sqlx::query_scalar("SELECT DISTINCT year FROM audio_files WHERE deleted = FALSE AND year IS NOT NULL ORDER BY year DESC").fetch_all(pool).await?,
    })
}

/// The file to stream for one audio row, and its MIME type.
pub async fn file(app: &App, id: u64, query: &super::audio_convert::FileQuery) -> Result<super::audio_convert::Served> {
    super::audio_convert::serve(app, id, query).await
}

// ---------------------------------------------------------------------------
// tags
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct AudioTagRequest {
    pub ids: Vec<u64>,
    pub name: String,
}

/// The one tag that means something by itself: a file carrying it is background music for
/// any title that has none of its own, when `audio_fallback` says so. It goes on audio files
/// and comes off them like any tag, but it is not a keyword -- no title can carry it, and
/// there is no `keyword` row behind it to rename or delete.
pub const GENERIC_BGM_TAG: &str = "generic_bgm";

pub fn is_reserved_tag(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case(GENERIC_BGM_TAG)
}

fn tag_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(Error::invalid("a keyword tag is 1 to 64 characters"));
    }
    Ok(if is_reserved_tag(name) { GENERIC_BGM_TAG.to_string() } else { name.to_string() })
}

/// Tags the files, and makes sure the keyword exists so titles can be given the same one.
/// Returns how many files gained the tag.
pub async fn add_tag(app: &App, request: &AudioTagRequest) -> Result<u64> {
    let name = tag_name(&request.name)?;
    if request.ids.is_empty() {
        return Err(Error::invalid("no audio files selected"));
    }
    let name = if is_reserved_tag(&name) {
        name
    } else {
        sqlx::query("INSERT IGNORE INTO keyword (name) VALUES (?)").bind(&name).execute(&app.pool).await?;
        // The keyword's own spelling wins, so "Hoge Quest" and "hoge quest" stay one tag.
        let stored: Option<String> = sqlx::query_scalar("SELECT name FROM keyword WHERE name = ? LIMIT 1").bind(&name).fetch_optional(&app.pool).await?;
        stored.unwrap_or(name)
    };
    let mut added = 0u64;
    for chunk in request.ids.chunks(1000) {
        // Put there by hand: also a tag the auto-tagger had put there becomes the user's own.
        let sql = format!(
            "INSERT INTO audio_file_tags (audio_file_id, name, source) SELECT a.id, ?, 'manual' FROM audio_files a WHERE a.id IN ({}) ON DUPLICATE KEY UPDATE source = 'manual'",
            chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",")
        );
        added += sqlx::query(sqlx::AssertSqlSafe(sql)).bind(&name).execute(&app.pool).await?.rows_affected();
    }
    Ok(added)
}

pub async fn remove_tag(app: &App, request: &AudioTagRequest) -> Result<u64> {
    let name = tag_name(&request.name)?;
    let mut removed = 0u64;
    for chunk in request.ids.chunks(1000) {
        let sql = format!("DELETE FROM audio_file_tags WHERE name = ? AND audio_file_id IN ({})", chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(","));
        removed += sqlx::query(sqlx::AssertSqlSafe(sql)).bind(&name).execute(&app.pool).await?.rows_affected();
    }
    Ok(removed)
}

/// What `GET /audio/tags` is asked: nothing (every tag in use), or what a tag picker has typed
/// so far.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TagQuery {
    /// Part of a name, matched anywhere in it.
    pub q: Option<String>,
    /// `audio`: only tags already on audio files (for removing one). `tags`: only the keyword
    /// tags on titles (the user's and the auto-tagger's, not providers' keywords) -- with
    /// `title_ids`, only those on these titles. Otherwise the catalogue's keywords are offered
    /// too, since a file tagged with a title's keyword plays for it.
    pub scope: Option<String>,
    /// Comma-separated title ids, for `scope=tags`.
    pub title_ids: Option<String>,
}

/// A tag a picker can offer.
#[derive(Debug, Clone, Serialize)]
pub struct TagSuggestion {
    pub name: String,
    /// Audio files carrying it.
    pub files: u64,
    /// Titles carrying it as a keyword.
    pub titles: u64,
}

/// Tags whose name contains `q`, for a picker that searches as its user types: the ones
/// already on audio files first (most used first), then catalogue keywords, each name once.
/// At most 30 -- it is a list to pick from, and typing one more letter narrows it.
pub async fn suggest_tags(app: &App, query: &TagQuery) -> Result<Vec<TagSuggestion>> {
    const LIMIT: usize = 30;
    let pattern = like_pattern(query.q.as_deref().unwrap_or("").trim());
    if query.scope.as_deref() == Some("tags") {
        let ids: Vec<u64> = query.title_ids.as_deref().unwrap_or("").split(',').filter_map(|v| v.trim().parse().ok()).collect();
        let only = if ids.is_empty() { String::new() } else { format!(" AND tk.title_id IN ({})", ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",")) };
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT k.name, COUNT(DISTINCT tk.title_id) AS titles, (SELECT COUNT(*) FROM audio_file_tags af WHERE af.name = k.name) AS files \
             FROM keyword k JOIN title_keywords tk ON tk.keyword_id = k.id WHERE tk.source IN ('user', 'auto') AND k.name LIKE ?{only} GROUP BY k.id, k.name ORDER BY titles DESC, k.name LIMIT 30"
        )))
        .bind(&pattern)
        .fetch_all(&app.pool)
        .await?;
        return rows.iter().map(|r| Ok(TagSuggestion { name: r.try_get("name")?, files: r.try_get::<i64, _>("files")?.max(0) as u64, titles: r.try_get::<i64, _>("titles")?.max(0) as u64 })).collect();
    }
    let rows = sqlx::query(
        "SELECT t.name, COUNT(*) AS files, (SELECT COUNT(DISTINCT tk.title_id) FROM keyword k JOIN title_keywords tk ON tk.keyword_id = k.id WHERE k.name = t.name) AS titles \
         FROM audio_file_tags t JOIN audio_files a ON a.id = t.audio_file_id AND a.deleted = FALSE WHERE t.name LIKE ? GROUP BY t.name ORDER BY files DESC, t.name LIMIT 30",
    )
    .bind(&pattern)
    .fetch_all(&app.pool)
    .await?;
    let mut out: Vec<TagSuggestion> =
        rows.iter().map(|r| Ok(TagSuggestion { name: r.try_get("name")?, files: r.try_get::<i64, _>("files")?.max(0) as u64, titles: r.try_get::<i64, _>("titles")?.max(0) as u64 })).collect::<Result<_>>()?;
    // The reserved tag is always there to pick, files or none: it exists without them.
    let typed = query.q.as_deref().unwrap_or("").trim().to_lowercase();
    if query.scope.as_deref() != Some("audio") && GENERIC_BGM_TAG.contains(&typed) && !out.iter().any(|s| is_reserved_tag(&s.name)) {
        out.insert(0, TagSuggestion { name: GENERIC_BGM_TAG.to_string(), files: 0, titles: 0 });
        out.truncate(LIMIT);
    }
    if query.scope.as_deref() != Some("audio") && out.len() < LIMIT {
        // Keywords some title carries: one nothing uses would tag a file for no page.
        let rows = sqlx::query("SELECT k.name, COUNT(DISTINCT tk.title_id) AS titles FROM keyword k JOIN title_keywords tk ON tk.keyword_id = k.id WHERE k.name LIKE ? GROUP BY k.id, k.name ORDER BY (k.name = ?) DESC, titles DESC, k.name LIMIT 30")
            .bind(&pattern)
            .bind(query.q.as_deref().unwrap_or("").trim())
            .fetch_all(&app.pool)
            .await?;
        for r in &rows {
            let name: String = r.try_get("name")?;
            if out.len() < LIMIT && !out.iter().any(|s| s.name.eq_ignore_ascii_case(&name)) {
                out.push(TagSuggestion { name, files: 0, titles: r.try_get::<i64, _>("titles")?.max(0) as u64 });
            }
        }
    }
    Ok(out)
}

/// Every tag in use on the library, with how many files carry it.
pub async fn tags(app: &App) -> Result<Vec<(String, u64)>> {
    let rows = sqlx::query("SELECT t.name, COUNT(*) AS n FROM audio_file_tags t JOIN audio_files a ON a.id = t.audio_file_id AND a.deleted = FALSE GROUP BY t.name ORDER BY t.name").fetch_all(&app.pool).await?;
    rows.iter().map(|r| Ok((r.try_get("name")?, r.try_get::<i64, _>("n")?.max(0) as u64))).collect()
}

// ---------------------------------------------------------------------------
// the playlist a title plays
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PlaylistQuery {
    pub title_id: Option<u64>,
    pub release_id: Option<u64>,
    pub ra_game_id: Option<u64>,
    /// What to play when the title has no audio, instead of this machine's `audio_fallback`:
    /// how a client asks the main server for its own setting's answer rather than the
    /// server's.
    pub fallback: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Playlist {
    /// The keyword names that matched something.
    pub tags: Vec<String>,
    pub tracks: Vec<AudioRow>,
    /// Set when the title has no audio of its own and these tracks stand in for it: the
    /// `audio_fallback` mode that chose them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
}

/// `audio_fallback`: what a title with no audio of its own plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    /// Nothing: the page is silent.
    None,
    /// Tracks picked at random from the audio that is some title's.
    Random,
    /// The files tagged [`GENERIC_BGM_TAG`].
    GenericBgm,
}

impl Fallback {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "none" => Some(Self::None),
            "random" => Some(Self::Random),
            "generic_bgm" => Some(Self::GenericBgm),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Random => "random",
            Self::GenericBgm => "generic_bgm",
        }
    }
}

/// How many tracks a random fallback hands a page: enough that a long stay does not loop.
const RANDOM_FALLBACK_TRACKS: u32 = 50;

/// The titles a page is about: the title itself, a release's title, or the titles of the
/// releases linked to an RA game.
async fn titles_of(app: &App, q: &PlaylistQuery) -> Result<Vec<u64>> {
    if let Some(id) = q.title_id {
        return Ok(vec![id]);
    }
    if let Some(id) = q.release_id {
        return Ok(sqlx::query_scalar("SELECT title_id FROM title_releases WHERE release_id = ?").bind(id).fetch_all(&app.pool).await?);
    }
    if let Some(id) = q.ra_game_id {
        return Ok(sqlx::query_scalar(
            "SELECT DISTINCT tr.title_id FROM release_provider_links rpl JOIN title_releases tr ON tr.release_id = rpl.release_id \
             WHERE rpl.provider = 'retroachievements' AND rpl.provider_id = ? AND rpl.deleted = FALSE",
        )
        .bind(id.to_string())
        .fetch_all(&app.pool)
        .await?);
    }
    Err(Error::invalid("playlist: one of title_id, release_id, ra_game_id is required"))
}

/// Every playable file that shares at least one keyword name with the page's title(s), in
/// album / disc / track order (the player shuffles; this is the order the list is shown in).
pub async fn playlist(app: &App, q: &PlaylistQuery) -> Result<Playlist> {
    // `bgm` unticked under "show in UI" is "no player": an empty list is how a page knows.
    if !crate::database::settings::MediaRules::load(&app.pool).await?.shows("bgm") {
        return Ok(Playlist::default());
    }
    let client = crate::database::settings::instance_mode(&app.pool).await? == crate::database::settings::InstanceMode::Client;
    let mut list = local_playlist(app, q).await?;
    // A client plays the main server's library too, streamed: its catalogue is the server's,
    // so the same ids name the same title there. A server that cannot be reached, or one too
    // old to have an audio library, just contributes nothing.
    if client {
        add_from_main(app, q, Fallback::None, &mut list).await?;
    }
    if !list.tracks.is_empty() {
        return Ok(list);
    }
    let fallback = match q.fallback.as_deref() {
        Some(name) => Fallback::parse(name).ok_or_else(|| Error::invalid("fallback must be none, random or generic_bgm"))?,
        None => crate::database::settings::get::<String>(&app.pool, crate::database::settings::KEY_AUDIO_FALLBACK).await?.as_deref().and_then(Fallback::parse).unwrap_or(Fallback::None),
    };
    if fallback == Fallback::None {
        return Ok(list);
    }
    list.tracks = fallback_tracks(app, fallback).await?;
    if client {
        add_from_main(app, q, fallback, &mut list).await?;
    }
    if !list.tracks.is_empty() {
        list.fallback = Some(fallback.name().to_string());
    }
    Ok(list)
}

/// Adds the main server's playlist for the same page to `list`. `fallback` is passed along
/// spelled out, so the server answers by this machine's setting and not its own.
async fn add_from_main(app: &App, q: &PlaylistQuery, fallback: Fallback, list: &mut Playlist) -> Result<()> {
    let query = match (q.title_id, q.release_id, q.ra_game_id) {
        (Some(id), _, _) => format!("title_id={id}"),
        (_, Some(id), _) => format!("release_id={id}"),
        (_, _, Some(id)) => format!("ra_game_id={id}"),
        _ => return Ok(()),
    };
    match super::media_sync::from_main(app, &format!("/api/v1/audio/playlist?{query}&fallback={}", fallback.name()), None).await {
        Ok(response) => match response.json::<Playlist>().await {
            Ok(theirs) => {
                // A server track this machine keeps a copy of is already in the list, as
                // that copy: it is not offered a second time as a stream.
                let held: Vec<u64> = sqlx::query_scalar("SELECT a.remote_id FROM audio_files a JOIN remote_sources s ON s.id = a.remote_source_id AND s.role = 'main' AND s.deleted = FALSE WHERE a.deleted = FALSE AND a.remote_id IS NOT NULL")
                    .fetch_all(&app.pool)
                    .await?;
                list.tracks.extend(theirs.tracks.into_iter().filter(|t| !held.contains(&t.id)).map(|t| AudioRow { remote: true, ..t }));
                for tag in theirs.tags {
                    if !list.tags.contains(&tag) {
                        list.tags.push(tag);
                    }
                }
            }
            Err(e) => log::debug!("audio playlist: the main server's reply was not a playlist: {e}"),
        },
        Err(e) => log::debug!("audio playlist: nothing from the main server: {}", e.message),
    }
    Ok(())
}

/// What this machine's library plays for a title with no audio of its own.
async fn fallback_tracks(app: &App, fallback: Fallback) -> Result<Vec<AudioRow>> {
    let sql = match fallback {
        Fallback::None => return Ok(Vec::new()),
        // "Some title's": tagged with a keyword a title carries, which is all a title's
        // audio ever is.
        Fallback::Random => format!(
            "SELECT {AUDIO_SELECT} FROM audio_files a WHERE a.deleted = FALSE AND a.playable = TRUE \
             AND EXISTS (SELECT 1 FROM audio_file_tags m JOIN keyword k ON k.name = m.name JOIN title_keywords tk ON tk.keyword_id = k.id WHERE m.audio_file_id = a.id AND m.name <> '{GENERIC_BGM_TAG}') \
             ORDER BY RAND() LIMIT {RANDOM_FALLBACK_TRACKS}"
        ),
        Fallback::GenericBgm => format!(
            "SELECT {AUDIO_SELECT} FROM audio_files a WHERE a.deleted = FALSE AND a.playable = TRUE \
             AND EXISTS (SELECT 1 FROM audio_file_tags m WHERE m.audio_file_id = a.id AND m.name = '{GENERIC_BGM_TAG}') \
             ORDER BY a.album, a.disc_number, a.track_number, a.file_name LIMIT 2000"
        ),
    };
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(&app.pool).await?;
    rows.iter().map(row_to_audio).collect()
}

async fn local_playlist(app: &App, q: &PlaylistQuery) -> Result<Playlist> {
    let titles = titles_of(app, q).await?;
    if titles.is_empty() {
        return Ok(Playlist::default());
    }
    let ids = titles.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let keyword_names = format!("SELECT k.name FROM title_keywords tk JOIN keyword k ON k.id = tk.keyword_id WHERE tk.title_id IN ({ids}) AND k.name <> '{GENERIC_BGM_TAG}'");
    let sql = format!(
        "SELECT {AUDIO_SELECT} FROM audio_files a WHERE a.deleted = FALSE AND a.playable = TRUE \
         AND EXISTS (SELECT 1 FROM audio_file_tags m WHERE m.audio_file_id = a.id AND m.name IN ({keyword_names})) \
         ORDER BY a.album, a.disc_number, a.track_number, a.file_name LIMIT 2000"
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(&app.pool).await?;
    let tracks = rows.iter().map(row_to_audio).collect::<Result<Vec<_>>>()?;
    let tags_sql = format!(
        "SELECT DISTINCT m.name FROM audio_file_tags m JOIN audio_files a ON a.id = m.audio_file_id AND a.deleted = FALSE AND a.playable = TRUE WHERE m.name IN ({keyword_names}) ORDER BY m.name"
    );
    let tags: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(tags_sql)).fetch_all(&app.pool).await?;
    Ok(Playlist { tags, tracks, fallback: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_come_out_of_the_shapes_tags_use() {
        assert_eq!(leading_number("3"), Some(3));
        assert_eq!(leading_number(" 03/12 "), Some(3));
        assert_eq!(leading_number("0"), None, "a zero is 'not set', not a track");
        assert_eq!(leading_number("A1"), None);
        assert_eq!(leading_year("1994"), Some(1994));
        assert_eq!(leading_year("1994-07-15T00:00:00Z"), Some(1994));
        assert_eq!(leading_year("94"), None, "two digits is not a year anyone can place");
        assert_eq!(leading_year(""), None);
    }

    /// A hand-written 16-bit mono WAV: enough for the reader to open, with no tag block.
    fn wav_bytes(seconds: u32) -> Vec<u8> {
        let rate = 8000u32;
        let data_len = rate * 2 * seconds;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        out.resize(out.len() + data_len as usize, 0);
        out
    }

    /// The walk reaches files at any depth and only takes audio; a file with no tags is
    /// still read (its duration comes from the format, not from a tag).
    #[test]
    fn the_walk_is_recursive_and_an_untagged_file_still_reads() {
        let root = std::env::temp_dir().join(format!("romexplorer_audio_walk_{}", std::process::id()));
        let deep = root.join("Artist").join("Album").join("Disc 2");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(root.join("top.wav"), wav_bytes(1)).unwrap();
        std::fs::write(deep.join("03 - deep.wav"), wav_bytes(2)).unwrap();
        std::fs::write(deep.join("cover.jpg"), b"not audio").unwrap();
        std::fs::write(deep.join("unknown.dsf"), b"DSD ").unwrap();

        let (mut files, mut errors) = (Vec::new(), Vec::new());
        walk(&root, &mut files, &mut errors);
        files.sort();
        let names: Vec<String> = files.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names.len(), 3, "{names:?}");
        assert!(names.contains(&"03 - deep.wav".to_string()) && names.contains(&"unknown.dsf".to_string()) && names.contains(&"top.wav".to_string()));
        assert!(errors.is_empty(), "{errors:?}");

        let tags = read_tags(&deep.join("03 - deep.wav"));
        assert_eq!(tags.title, None);
        assert!(tags.codec_playable);
        assert_eq!(tags.duration_ms.map(|d| d / 1000), Some(2));
        // A format the reader does not know is listed with nothing, not an error.
        assert_eq!(read_tags(&deep.join("unknown.dsf")), AudioTags { codec_playable: true, ..Default::default() });
        let _ = std::fs::remove_dir_all(&root);
    }
}
