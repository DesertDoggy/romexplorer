//! A client's own copies of its main server's audio (`audio_sync` job).
//!
//! By default a client streams the server's tracks (`audio::playlist` lists them as
//! `remote`). With `bgm` ticked under `media_client_keep` it keeps copies instead, fetched
//! in the format `audio_client_format` names: the server converts a track that is above
//! `audio_client_bitrate_kbps` (or that a browser cannot play at all) and sends the rest as
//! they are -- see `audio_convert::serve`. The conversion happens there, not here, so the
//! small file is what crosses the network and the encoder only has to exist on the server.
//!
//! Only tracks that carry a keyword tag are copied: those are the ones a page can play.
//! Unsupported formats (WMA, APE) are never copied.
//!
//! It is a sync, not only a download: a copy already here takes the server's current tags,
//! title, album and loudness on every run, and a copy whose track the server no longer
//! offers -- deleted there, or left with no tag -- is removed, file and row. Only files this
//! job put under `_from_server` are ever deleted; nothing of the user's own is touched. A copy remembers which server track it
//! came from (`remote_source_id`, `remote_id`), so a second run fetches nothing and the
//! playlist lists the track once, as local. The scan leaves these rows alone: their
//! metadata is the server's, and a converted file has no tags of its own to re-read.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tokio::io::AsyncWriteExt;

use crate::database::import::sha1_hex;
use crate::database::jobs::Progress;
use crate::database::media_store::sanitize_component;
use crate::database::settings as db_settings;

use super::audio::{AudioPage, AudioRow};
use super::audio_convert::Support;
use super::{App, Error, Result};

#[derive(Debug, Clone, Default, Serialize)]
pub struct AudioSyncSummary {
    pub source: String,
    /// The format asked for: none | aac | xhe_aac.
    pub format: String,
    pub bitrate_kbps: u32,
    /// Tracks the server lists.
    pub listed: u64,
    /// Of those, the ones with a keyword tag and a format that can be played.
    pub wanted: u64,
    pub already_held: u64,
    /// Of those, copies whose tags or details changed on the server and were brought in line.
    pub updated: u64,
    /// Copies removed because the server no longer offers the track.
    pub removed: u64,
    pub fetched: u64,
    /// Of the fetched, how many the server converted.
    pub converted: u64,
    pub failed: u64,
    pub bytes: u64,
    pub errors: Vec<String>,
}

pub fn one_liner(s: &AudioSyncSummary) -> String {
    let mut parts = vec![format!("{} tagged track(s) on {}", s.wanted, s.source), format!("{} fetched ({} converted)", s.fetched, s.converted)];
    if s.already_held > 0 {
        parts.push(format!("{} already here ({} updated)", s.already_held, s.updated));
    }
    if s.removed > 0 {
        parts.push(format!("{} removed (gone from the server)", s.removed));
    }
    if s.failed > 0 {
        parts.push(format!("{} failed", s.failed));
    }
    format!("done: {} -- {}", parts.join(", "), crate::database::jobs::human_bytes(s.bytes))
}

/// Refused up front, with the reason, rather than as a failed job later.
pub async fn validate(app: &App) -> Result<()> {
    super::media_sync::main_source(app).await?.ok_or_else(|| Error::invalid("audio_sync: there is no main server to fetch audio from -- add one under Config > Share"))?;
    if !db_settings::MediaRules::load(&app.pool).await?.keeps("bgm") {
        return Err(Error::invalid("audio_sync: this machine streams the server's audio. Tick bgm under Settings > media_client_keep to keep copies."));
    }
    Ok(())
}

/// Where copies go: the first configured audio folder, else `<media>/audio`, in a folder of
/// their own so they are never mistaken for the user's files.
async fn destination(app: &App) -> Result<PathBuf> {
    let roots = crate::database::storage::list_media_dirs(&app.pool).await?;
    let base = roots.into_iter().find(|d| d.role == "audio").map(|d| PathBuf::from(d.path)).unwrap_or_else(|| app.paths.media_dir().join("audio"));
    Ok(base.join("_from_server"))
}

/// `<album>/<track name>.<ext>`, made of safe components. The server's id is in the name so
/// two tracks that would land on the same name do not overwrite each other.
fn relative_path(track: &AudioRow, ext: &str) -> PathBuf {
    let album = track.album.as_deref().map(str::trim).filter(|a| !a.is_empty()).unwrap_or("Unknown album");
    let stem = Path::new(&track.file_name).file_stem().and_then(|s| s.to_str()).unwrap_or("track");
    PathBuf::from(sanitize_component(album)).join(format!("{} [{}].{ext}", sanitize_component(stem), track.id))
}

pub async fn run(app: &App, progress: &Progress) -> Result<AudioSyncSummary> {
    validate(app).await?;
    let source_id = super::media_sync::main_source(app).await?.ok_or_else(|| Error::invalid("no main server"))?;
    let source = super::sync::load_source(app, source_id).await?;
    let format = db_settings::get::<String>(&app.pool, db_settings::KEY_AUDIO_CLIENT_FORMAT).await?.unwrap_or_else(|| db_settings::default_audio_client_format().to_string());
    let bitrate = db_settings::get::<u32>(&app.pool, db_settings::KEY_AUDIO_CLIENT_BITRATE).await?.unwrap_or(128);
    let dest = destination(app).await?;
    let mut summary = AudioSyncSummary { source: source.name.clone(), format: format.clone(), bitrate_kbps: bitrate, ..Default::default() };

    // The whole list first: it is small (rows, not files), and it gives the progress bar a total.
    progress.force(0, None, &format!("{}: reading its audio library", source.name)).await;
    let mut wanted: Vec<AudioRow> = Vec::new();
    let mut offset = 0u32;
    loop {
        let page: AudioPage = super::media_sync::from_main(app, &format!("/api/v1/audio?limit=200&offset={offset}&sort=id"), None)
            .await?
            .json()
            .await
            .map_err(|e| Error::unavailable(format!("{}: its audio list was not understood ({e}); is the server up to date?", source.name)))?;
        let got = page.items.len() as u32;
        summary.listed += u64::from(got);
        wanted.extend(page.items.into_iter().filter(|t| !t.deleted && t.tags.as_deref().is_some_and(|tags| !tags.is_empty()) && Support::from_name(&t.support) != Support::Unsupported));
        offset += got;
        if got == 0 || u64::from(offset) >= page.total {
            break;
        }
    }
    summary.wanted = wanted.len() as u64;
    let total = wanted.len() as u64;

    for (i, track) in wanted.iter().enumerate() {
        progress.report(i as u64, Some(total), &format!("{}: track {} of {total}, {} fetched -- {}", source.name, i + 1, summary.fetched, track.file_name)).await;
        let held: Option<(u64, String)> = sqlx::query_as("SELECT id, path FROM audio_files WHERE remote_source_id = ? AND remote_id = ? AND deleted = FALSE").bind(source_id).bind(track.id).fetch_optional(&app.pool).await?;
        if let Some((local_id, _)) = held.filter(|(_, path)| Path::new(path).is_file()) {
            summary.already_held += 1;
            summary.updated += u64::from(refresh(app, local_id, track).await?);
            continue;
        }
        match fetch(app, source_id, track, &format, bitrate, &dest).await {
            Ok((bytes, converted)) => {
                summary.fetched += 1;
                summary.converted += u64::from(converted);
                summary.bytes += bytes;
            }
            Err(e) => {
                summary.failed += 1;
                if summary.errors.len() < 20 {
                    summary.errors.push(format!("{}: {}", track.file_name, e.message));
                }
                log::warn!("audio_sync: {}: {}", track.file_name, e.message);
            }
        }
    }
    // What the server no longer offers: deleted there, untagged, or turned unsupported. The
    // list above is the whole of what it offers, so anything else from this source goes.
    let offered: Vec<u64> = wanted.iter().map(|t| t.id).collect();
    let copies: Vec<(u64, u64, String)> = sqlx::query_as("SELECT id, remote_id, path FROM audio_files WHERE remote_source_id = ? AND remote_id IS NOT NULL AND deleted = FALSE").bind(source_id).fetch_all(&app.pool).await?;
    for (local_id, remote_id, path) in copies {
        if offered.contains(&remote_id) {
            continue;
        }
        remove_copy(app, local_id, Path::new(&path), &dest).await?;
        summary.removed += 1;
    }
    progress.force(total, Some(total.max(1)), &one_liner(&summary)).await;
    Ok(summary)
}

/// Brings a held copy's tags and details in line with the server's. Returns whether anything
/// changed. The file itself is not fetched again: the audio is the same track.
async fn refresh(app: &App, local_id: u64, track: &AudioRow) -> Result<bool> {
    let title = track.title.clone().or_else(|| Path::new(&track.file_name).file_stem().map(|s| s.to_string_lossy().into_owned()));
    let details = sqlx::query(
        "UPDATE audio_files SET title = ?, album = ?, artist = ?, album_artist = ?, disc_number = ?, track_number = ?, year = ?, duration_ms = COALESCE(?, duration_ms), loudness_lufs = COALESCE(?, loudness_lufs)          WHERE id = ? AND NOT (title <=> ? AND album <=> ? AND artist <=> ? AND album_artist <=> ? AND disc_number <=> ? AND track_number <=> ? AND year <=> ?            AND duration_ms <=> COALESCE(?, duration_ms) AND loudness_lufs <=> COALESCE(?, loudness_lufs))",
    )
    .bind(&title)
    .bind(&track.album)
    .bind(&track.artist)
    .bind(&track.album_artist)
    .bind(track.disc_number)
    .bind(track.track_number)
    .bind(track.year)
    .bind(track.duration_ms)
    .bind(track.loudness_lufs)
    .bind(local_id)
    // The same values again, for the "only if something differs" half: the connection reports
    // rows matched, not rows changed, so an unconditional UPDATE would always count.
    .bind(&title)
    .bind(&track.album)
    .bind(&track.artist)
    .bind(&track.album_artist)
    .bind(track.disc_number)
    .bind(track.track_number)
    .bind(track.year)
    .bind(track.duration_ms)
    .bind(track.loudness_lufs)
    .execute(&app.pool)
    .await?
    .rows_affected()
        > 0;
    Ok(set_tags(app, local_id, track).await? || details)
}

/// Makes a copy's tags exactly the server's. Returns whether they differed.
async fn set_tags(app: &App, local_id: u64, track: &AudioRow) -> Result<bool> {
    let mut theirs: Vec<&str> = track.tags.as_deref().unwrap_or("").split(',').map(str::trim).filter(|t| !t.is_empty()).collect();
    theirs.sort_unstable();
    let ours: Vec<String> = sqlx::query_scalar("SELECT name FROM audio_file_tags WHERE audio_file_id = ? ORDER BY name").bind(local_id).fetch_all(&app.pool).await?;
    let mut sorted: Vec<&str> = ours.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    if sorted == theirs {
        return Ok(false);
    }
    sqlx::query("DELETE FROM audio_file_tags WHERE audio_file_id = ?").bind(local_id).execute(&app.pool).await?;
    for tag in theirs {
        sqlx::query("INSERT IGNORE INTO audio_file_tags (audio_file_id, name) VALUES (?, ?)").bind(local_id).bind(tag).execute(&app.pool).await?;
    }
    Ok(true)
}

/// Drops a copy the server no longer offers. The file is deleted only when it is under the
/// folder this job writes to -- a row pointing anywhere else is retired and its file left.
async fn remove_copy(app: &App, local_id: u64, path: &Path, dest: &Path) -> Result<()> {
    if path.starts_with(dest) {
        match tokio::fs::remove_file(path).await {
            Ok(()) => {
                // The album folder too, once its last track is gone. Fails, harmlessly, while
                // anything is still in it.
                if let Some(folder) = path.parent().filter(|f| *f != dest) {
                    let _ = tokio::fs::remove_dir(folder).await;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log::warn!("audio_sync: could not delete {}: {e}", path.display()),
        }
    }
    sqlx::query("DELETE FROM audio_file_tags WHERE audio_file_id = ?").bind(local_id).execute(&app.pool).await?;
    // Hard-deleted: the row is this job's own bookkeeping, and a soft-deleted one would keep
    // holding the (source, track) pair a later re-tag wants to use again.
    sqlx::query("DELETE FROM audio_files WHERE id = ?").bind(local_id).execute(&app.pool).await?;
    Ok(())
}

/// Downloads one track and records it. Returns its size and whether the server converted it.
async fn fetch(app: &App, source_id: u16, track: &AudioRow, format: &str, bitrate: u32, dest: &Path) -> Result<(u64, bool)> {
    let query = if format == "none" { String::new() } else { format!("?format={format}&bitrate={bitrate}") };
    let mut response = super::media_sync::from_main(app, &format!("/api/v1/audio/{}/file{query}", track.id), None).await?;
    let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
    // A server too old to say what it sent sent the original.
    let ext = header("x-audio-ext").filter(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_alphanumeric())).unwrap_or_else(|| track.format.clone());
    let converted = header("x-audio-source").is_some_and(|s| s == "converted");

    let path = dest.join(relative_path(track, &ext));
    let folder = path.parent().ok_or_else(|| Error::internal("no destination folder"))?;
    tokio::fs::create_dir_all(folder).await.map_err(|e| Error::internal(format!("create {}: {e}", folder.display())))?;
    // Written beside its final name and renamed: a copy that exists is a whole one.
    let partial = path.with_extension(format!("{ext}.part"));
    let written = async {
        let mut file = tokio::fs::File::create(&partial).await.map_err(|e| Error::internal(format!("create {}: {e}", partial.display())))?;
        let mut bytes = 0u64;
        while let Some(chunk) = response.chunk().await.map_err(|e| Error::unavailable(format!("the server stopped sending: {e}")))? {
            file.write_all(&chunk).await.map_err(|e| Error::internal(format!("write {}: {e}", partial.display())))?;
            bytes += chunk.len() as u64;
        }
        file.flush().await.map_err(|e| Error::internal(format!("write {}: {e}", partial.display())))?;
        Ok::<u64, Error>(bytes)
    }
    .await;
    let bytes = match written {
        Ok(bytes) if bytes > 0 => bytes,
        Ok(_) => {
            let _ = tokio::fs::remove_file(&partial).await;
            return Err(Error::unavailable("the server sent an empty file"));
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&partial).await;
            return Err(e);
        }
    };
    tokio::fs::rename(&partial, &path).await.map_err(|e| Error::internal(format!("move into place {}: {e}", path.display())))?;

    // An unconverted .m4a may hold ALAC, which this machine then converts when it plays it.
    let probe = path.clone();
    let codec_playable = tokio::task::spawn_blocking(move || super::audio::read_tags(&probe).codec_playable).await.unwrap_or(true);
    let support = Support::of(&ext, codec_playable);
    let text = path.display().to_string();
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| text.clone());
    sqlx::query(
        "INSERT INTO audio_files (path, path_hash, file_name, size, last_modified, format, playable, support, duration_ms, loudness_lufs, sound_end_ms, title, album, artist, album_artist, disc_number, track_number, year, remote_source_id, remote_id, scanned_at, deleted) \
         VALUES (?, ?, ?, ?, NOW(), ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NOW(), FALSE) \
         ON DUPLICATE KEY UPDATE path = VALUES(path), file_name = VALUES(file_name), size = VALUES(size), last_modified = NOW(), format = VALUES(format), playable = VALUES(playable), support = VALUES(support), \
           duration_ms = VALUES(duration_ms), loudness_lufs = VALUES(loudness_lufs), sound_end_ms = VALUES(sound_end_ms), title = VALUES(title), album = VALUES(album), artist = VALUES(artist), album_artist = VALUES(album_artist), \
           disc_number = VALUES(disc_number), track_number = VALUES(track_number), year = VALUES(year), remote_source_id = VALUES(remote_source_id), remote_id = VALUES(remote_id), scanned_at = NOW(), deleted = FALSE",
    )
    .bind(&text)
    .bind(sha1_hex(&text))
    .bind(name.chars().take(512).collect::<String>())
    .bind(bytes)
    .bind(&ext)
    .bind(support != Support::Unsupported)
    .bind(support.name())
    .bind(track.duration_ms)
    .bind(track.loudness_lufs)
    .bind(track.sound_end_ms)
    // A file with no title tag is still called something on the list.
    .bind(track.title.clone().or_else(|| Path::new(&track.file_name).file_stem().map(|s| s.to_string_lossy().into_owned())))
    .bind(&track.album)
    .bind(&track.artist)
    .bind(&track.album_artist)
    .bind(track.disc_number)
    .bind(track.track_number)
    .bind(track.year)
    .bind(source_id)
    .bind(track.id)
    .execute(&app.pool)
    .await?;
    let local_id: u64 = sqlx::query_scalar("SELECT id FROM audio_files WHERE remote_source_id = ? AND remote_id = ?").bind(source_id).bind(track.id).fetch_one(&app.pool).await?;
    set_tags(app, local_id, track).await?;
    Ok((bytes, converted))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: u64, file_name: &str, album: Option<&str>) -> AudioRow {
        AudioRow {
            remote: false,
            rom: false,
            rom_kind: None,
            id,
            path: String::new(),
            file_name: file_name.to_string(),
            size: 0,
            format: "flac".to_string(),
            playable: true,
            support: "direct".to_string(),
            duration_ms: None,
            loudness_lufs: None,
            sound_end_ms: None,
            title: None,
            album: album.map(str::to_string),
            artist: None,
            album_artist: None,
            disc_number: None,
            track_number: None,
            year: None,
            deleted: false,
            tags: None,
            auto_tags: None,
        }
    }

    #[test]
    fn a_copy_is_filed_under_its_album_with_the_servers_id_in_its_name() {
        assert_eq!(relative_path(&track(7, "01 Prelude.flac", Some("FF3 OST")), "m4a"), PathBuf::from("FF3 OST").join("01 Prelude [7].m4a"));
        assert_eq!(relative_path(&track(8, "loose.wav", None), "wav"), PathBuf::from("Unknown album").join("loose [8].wav"));
        assert_eq!(relative_path(&track(9, "x.flac", Some("   ")), "flac"), PathBuf::from("Unknown album").join("x [9].flac"));
        // Nothing in a name can climb out of the destination.
        let odd = relative_path(&track(10, "a.flac", Some("../../etc")), "flac");
        assert!(!odd.components().any(|c| matches!(c, std::path::Component::ParentDir)), "{odd:?}");
        // Same album, same name, two server tracks: two files.
        assert_ne!(relative_path(&track(1, "a.flac", Some("X")), "m4a"), relative_path(&track(2, "a.flac", Some("X")), "m4a"));
    }
}
