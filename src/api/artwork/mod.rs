//! "Export artwork" (`artwork_export`): this machine's art, written into an emulator's own
//! folders in the layout and formats it reads, so its game list shows covers.
//!
//! - **RetroArch** (`retroarch`): `<dir>/<system>/Named_Boxarts|Named_Titles|Named_Snaps|
//!   Named_Logos/<rom>.png` -- the libretro-thumbnails layout. `<system>` is the No-Intro /
//!   Redump system name (what a scanned playlist is called), `<rom>` the game file's name
//!   without extension, which RetroArch tries first (1.17+), with `&*/:\`<>?\|"` as `_`.
//! - **PCSX2, DuckStation** (`cover_folder`): `<dir>/<serial>.<ext>`, the game's serial (what
//!   both look up first after the file name, and what survives a rename), else the file name.
//!   `cover_folder_name` names it by the game file's name instead, which both also look up:
//!   it matches the names this app copies under, and a cover is easy to tell apart by eye.
//!
//! Which picture fills each slot comes from `media_export_priority` (by slot), not the display
//! order: an emulator's box wants a real box scan, so TheGamesDB / MobyGames come before
//! SteamGridDB's posters. A release's own art comes before its title's.
//!
//! Nothing it did not write is ever overwritten (`media_exports` records what it wrote): a
//! cover the user or RetroArch's own thumbnail updater put there stays. A re-run writes only
//! what changed. Converted pictures are kept in the export cache, so a second run, or a second
//! emulator, only copies.

pub mod cache;
pub mod convert;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use sqlx::Row;

use romexplorer_types::jobs::{ArtworkExportRequest, ExportFile};

use crate::common::models::Platform;
use crate::database::emulators::{Emulator, EmulatorArtwork, ARTWORK_LAYOUTS};
use crate::database::jobs::Progress;
use crate::database::media_store::{self, Roots};
use crate::database::settings as db_settings;

use self::convert::{Codec, Step};
use super::{App, Error, Result};

pub const KEY_EXPORT_PRIORITY: &str = "media_export_priority";
pub const KEY_EXPORT_PREFER_REGION: &str = "media_export_prefer_region";
pub const KEY_EXPORT_CACHE_MB: &str = "media_export_cache_max_mb";

/// What fills a slot, best first, when `media_export_priority` does not say.
pub const DEFAULT_PRIORITY: [(&str, &[&str]); 4] = [
    ("box", &["manual", "xlenore", "thegamesdb", "screenscraper", "mobygames", "retroachievements", "igdb", "steamgriddb"]),
    ("title", &["manual", "thegamesdb", "screenscraper", "retroachievements"]),
    ("snap", &["manual", "thegamesdb", "screenscraper", "retroachievements", "igdb"]),
    ("logo", &["manual", "steamgriddb", "thegamesdb", "screenscraper"]),
];

pub const SLOTS: [&str; 4] = ["box", "title", "snap", "logo"];

/// A slot's media types, best first: a box front, else a cover.
fn slot_types(slot: &str) -> &'static [(&'static str, Option<&'static str>)] {
    match slot {
        "box" => &[("box", Some("front")), ("cover", None)],
        "title" => &[("title_screen", None)],
        "snap" => &[("screenshot", None)],
        _ => &[("logo", None)],
    }
}

/// RetroArch's folder for a slot.
fn retroarch_folder(slot: &str) -> &'static str {
    match slot {
        "box" => "Named_Boxarts",
        "title" => "Named_Titles",
        "snap" => "Named_Snaps",
        _ => "Named_Logos",
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ArtworkExportSummary {
    pub emulator: String,
    pub games: u64,
    /// Files written (new or changed).
    pub written: u64,
    /// Of those, converted (format or size) rather than copied.
    pub converted: u64,
    /// Already there as this export left them.
    pub unchanged: u64,
    /// A file this export did not write was already at the path: left alone.
    pub kept: u64,
    /// Slots no art was found for.
    pub no_art: u64,
    pub errors: Vec<String>,
}

pub fn one_liner(s: &ArtworkExportSummary) -> String {
    let mut line = format!("{}: {} game(s), {} written ({} converted), {} unchanged", s.emulator, s.games, s.written, s.converted, s.unchanged);
    if s.kept > 0 {
        line.push_str(&format!(", {} kept (not written by romexplorer)", s.kept));
    }
    if s.no_art > 0 {
        line.push_str(&format!(", {} without art", s.no_art));
    }
    if !s.errors.is_empty() {
        line.push_str(&format!(", {} error(s)", s.errors.len()));
    }
    line
}

/// Where the art goes: the request's own folder, else the emulator's artwork setting. With the
/// emulator, when there is one, for the record and the summary.
async fn target(app: &App, request: &ArtworkExportRequest) -> Result<(Option<Emulator>, EmulatorArtwork)> {
    if let Some(t) = &request.target {
        if t.dir.trim().is_empty() {
            return Err(Error::invalid("pick the folder to export into"));
        }
        let emulator = match request.emulator_id {
            Some(id) => crate::database::emulators::get(&app.pool, id).await?,
            None => None,
        };
        return Ok((emulator, EmulatorArtwork { dir: t.dir.trim().to_string(), layout: t.layout.clone(), formats: t.formats.clone(), max_width: t.max_width, after_copy: false }));
    }
    let Some(emulator_id) = request.emulator_id else { return Err(Error::invalid("pick the folder to export into")) };
    let emulator = crate::database::emulators::get(&app.pool, emulator_id).await?.ok_or_else(|| Error::not_found(format!("no emulator {emulator_id}")))?;
    let artwork = emulator.artwork.clone().filter(|a| !a.dir.trim().is_empty()).ok_or_else(|| Error::invalid(format!("{} has no artwork folder set (Config › Emulators)", emulator.name)))?;
    Ok((Some(emulator), artwork))
}

pub async fn validate(app: &App, request: &ArtworkExportRequest) -> Result<()> {
    let (_, artwork) = target(app, request).await?;
    validate_artwork(&artwork)?;
    if request.files.is_empty() && request.selection.as_ref().is_none_or(|s| s.is_empty()) {
        return Err(Error::invalid("nothing to export art for: no files and no selection"));
    }
    Ok(())
}

pub fn validate_artwork(artwork: &EmulatorArtwork) -> Result<()> {
    if !ARTWORK_LAYOUTS.contains(&artwork.layout.as_str()) {
        return Err(Error::invalid(format!("artwork layout must be one of {}", ARTWORK_LAYOUTS.join(", "))));
    }
    let codecs: Vec<Codec> = artwork.formats.iter().map(|f| Codec::from_name(f).ok_or_else(|| Error::invalid(format!("unknown picture format {f:?}")))).collect::<Result<_>>()?;
    if !codecs.iter().any(|c| c.can_write()) && !codecs.is_empty() {
        // Copies of pictures already in those formats still work; say what can't be done.
        log::info!("artwork formats {:?}: none can be written here, only copied", artwork.formats);
    }
    if codecs.is_empty() {
        return Err(Error::invalid("pick at least one picture format the emulator reads"));
    }
    Ok(())
}

/// Queues an export per emulator for the files a copy just wrote.
pub async fn queue_after_copy(app: &App, emulator_ids: &[u64], files: &[ExportFile]) -> Vec<u64> {
    let mut queued = Vec::new();
    if files.is_empty() {
        return queued;
    }
    for id in emulator_ids {
        let request = super::jobs::JobRequest::ArtworkExport(ArtworkExportRequest { emulator_id: Some(*id), target: None, files: files.to_vec(), selection: None });
        match super::jobs::enqueue(app, &request).await {
            Ok(job) => queued.push(job),
            Err(e) => log::warn!("copy: could not queue the artwork export for emulator {id}: {}", e.message),
        }
    }
    queued
}

/// Each slot's provider order.
async fn priorities(app: &App) -> Result<HashMap<String, Vec<String>>> {
    let stored: Option<HashMap<String, Vec<String>>> = db_settings::get(&app.pool, KEY_EXPORT_PRIORITY).await?;
    let stored = stored.unwrap_or_default();
    Ok(DEFAULT_PRIORITY.iter().map(|(slot, order)| (slot.to_string(), stored.get(*slot).cloned().unwrap_or_else(|| order.iter().map(|s| s.to_string()).collect()))).collect())
}

/// One media row a slot could use.
#[derive(Debug, Clone)]
struct Candidate {
    id: u64,
    media_type: String,
    sub_category: Option<String>,
    source: Option<String>,
    path: String,
    root_role: Option<String>,
    sha1: Option<String>,
    source_url: Option<String>,
    /// On the release itself, not only its title.
    own: bool,
    /// The title it came through, when it is title art.
    title_id: Option<u64>,
    /// Only on the main server: a type this client streams instead of keeping. `id` is then
    /// the server's media id.
    remote: bool,
}

impl Candidate {
    /// Its media row here, if it has one.
    fn local_id(&self) -> Option<u64> {
        (!self.remote).then_some(self.id)
    }
}

/// The art a release could export: its own and its title's, here -- and on a client the main
/// server's too (`main`), so a type this client only streams (screenshots, say) is exported
/// as well. Where both have a picture, the copy here ranks first and is fetched from the
/// server all the same (see `source_bytes`).
async fn candidates(app: &App, main: Option<u16>, release_id: u64) -> Result<Vec<Candidate>> {
    let mut out = local_candidates(app, release_id).await?;
    if let Some(source_id) = main {
        out.extend(server_candidates(app, source_id, release_id).await?);
    }
    Ok(out)
}

async fn server_candidates(app: &App, source_id: u16, release_id: u64) -> Result<Vec<Candidate>> {
    let rows = sqlx::query(
        "SELECT rm.id, rm.media_type, rm.sub_category, COALESCE(rm.source, 'manual') AS source, rm.path, rm.root_role, rm.content_sha1, rm.source_url, 1 AS own, CAST(NULL AS UNSIGNED) AS title_id \
           FROM remote_release_media rrm JOIN remote_media rm ON rm.source_id = rrm.source_id AND rm.id = rrm.media_id \
          WHERE rrm.source_id = ? AND rrm.release_id = ? AND rm.deleted = FALSE \
         UNION ALL \
         SELECT rm.id, rm.media_type, rm.sub_category, COALESCE(rm.source, 'manual') AS source, rm.path, rm.root_role, rm.content_sha1, rm.source_url, 0 AS own, CAST(rtm.title_id AS UNSIGNED) AS title_id \
           FROM title_releases tr JOIN remote_title_media rtm ON rtm.title_id = tr.title_id AND rtm.source_id = ? JOIN remote_media rm ON rm.source_id = rtm.source_id AND rm.id = rtm.media_id \
          WHERE tr.release_id = ? AND rm.deleted = FALSE",
    )
    .bind(source_id)
    .bind(release_id)
    .bind(source_id)
    .bind(release_id)
    .fetch_all(&app.pool)
    .await?;
    rows_to_candidates(&rows, true)
}

async fn local_candidates(app: &App, release_id: u64) -> Result<Vec<Candidate>> {
    let rows = sqlx::query(
        "SELECT m.id, m.media_type, m.sub_category, m.source, m.path, m.root_role, m.content_sha1, m.source_url, 1 AS own, CAST(NULL AS UNSIGNED) AS title_id FROM release_media rm JOIN media m ON m.id = rm.media_id WHERE rm.release_id = ? AND m.deleted = FALSE \
         UNION ALL \
         SELECT m.id, m.media_type, m.sub_category, m.source, m.path, m.root_role, m.content_sha1, m.source_url, 0 AS own, CAST(tm.title_id AS UNSIGNED) AS title_id FROM title_releases tr JOIN title_media tm ON tm.title_id = tr.title_id JOIN media m ON m.id = tm.media_id WHERE tr.release_id = ? AND m.deleted = FALSE",
    )
    .bind(release_id)
    .bind(release_id)
    .fetch_all(&app.pool)
    .await?;
    rows_to_candidates(&rows, false)
}

fn rows_to_candidates(rows: &[sqlx::mysql::MySqlRow], remote: bool) -> Result<Vec<Candidate>> {
    rows.iter()
        .map(|r| {
            Ok(Candidate {
                id: r.try_get("id")?,
                media_type: r.try_get("media_type")?,
                sub_category: r.try_get("sub_category")?,
                source: r.try_get("source")?,
                path: r.try_get("path")?,
                root_role: r.try_get("root_role")?,
                sha1: r.try_get("content_sha1")?,
                source_url: r.try_get("source_url")?,
                own: r.try_get::<i32, _>("own")? == 1,
                title_id: r.try_get::<Option<u64>, _>("title_id")?,
                remote,
            })
        })
        .collect::<std::result::Result<_, sqlx::Error>>()
        .map_err(Into::into)
}

/// The best candidate for a slot (a box front and a cover are one slot: every provider's
/// front picture, under the name it uses), then -- with
/// "prefer my region" -- art filed under the release's region before art for every region,
/// then the slot's provider order, then (within one provider) its regional picture before its
/// main one -- IGDB's Japanese box before its main cover -- then the release's own art before
/// its title's, then a copy held here before the server's own row of it.
fn pick<'a>(candidates: &'a [Candidate], slot: &str, order: &[String], prefer_region: bool) -> Option<&'a Candidate> {
    let types = slot_types(slot);
    let rank = |c: &Candidate| {
        let source = c.source.as_deref().unwrap_or("");
        types.iter().find(|(t, sub)| *t == c.media_type && (sub.is_none() || c.sub_category.as_deref().is_none_or(|s| Some(s) == *sub)))?;
        // 0 for region art (SteamGridDB's art never is, wherever it is filed), 1 for common.
        let regional = if db_settings::is_region_art(c.source.as_deref(), &c.path) { 0 } else { 1 };
        let provider = order.iter().position(|p| p == source).unwrap_or(usize::MAX);
        Some((if prefer_region { regional } else { 0 }, provider, regional, if c.own { 0 } else { 1 }, c.remote, c.id))
    };
    candidates.iter().filter_map(|c| rank(c).map(|r| (r, c))).min_by_key(|(r, _)| *r).map(|(_, c)| c)
}

/// A file name RetroArch can match: `&*/:\`<>?\|"` become `_`, as its thumbnail lookup does.
pub fn retroarch_name(stem: &str) -> String {
    stem.chars().map(|c| if "&*/:`<>?\\|\"".contains(c) { '_' } else { c }).collect()
}

/// The first serial of a release (`SLUS-20062, SLUS-20062GH` -> `SLUS-20062`).
pub fn first_serial(serial: Option<&str>) -> Option<String> {
    serial?.split([',', ';']).map(str::trim).find(|s| !s.is_empty()).map(str::to_string)
}

/// Where a slot's picture goes.
pub fn target_path(artwork: &EmulatorArtwork, system: &str, slot: &str, stem: &str, serial: Option<&str>, ext: &str) -> PathBuf {
    let dir = Path::new(artwork.dir.trim());
    if artwork.layout == "retroarch" {
        dir.join(system).join(retroarch_folder(slot)).join(format!("{}.{ext}", retroarch_name(stem)))
    } else {
        let by_serial = if artwork.layout == "cover_folder_name" { None } else { first_serial(serial) };
        let name = by_serial.unwrap_or_else(|| stem.to_string());
        dir.join(format!("{}.{ext}", super::copy::sanitize_segment(&name)))
    }
}

/// What the files to export for come to: (release, platform, stem).
async fn files_for(app: &App, request: &ArtworkExportRequest, warnings: &mut Vec<String>) -> Result<Vec<ExportFile>> {
    if !request.files.is_empty() {
        return Ok(request.files.clone());
    }
    let Some(selection) = &request.selection else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for release_id in super::selection::release_ids(app, selection, true, warnings).await? {
        let Some(row) = sqlx::query("SELECT name, app_platform_id FROM `release` WHERE id = ? AND deleted = FALSE").bind(release_id).fetch_optional(&app.pool).await? else { continue };
        let name: String = row.try_get("name")?;
        let platform: u16 = row.try_get("app_platform_id")?;
        // Named after this machine's file of it -- a multi-disc game after its playlist -- or,
        // with none here, the catalogue's (DAT) name, which is what a renamed copy is called.
        let local = sqlx::query(
            "SELECT (SELECT lf.path FROM local_files lf WHERE lf.id = lg.playlist_file_id) AS playlist, \
                    (SELECT lr.main_path FROM local_game_roms lgr JOIN local_rom lr ON lr.id = lgr.rom_id WHERE lgr.game_id = lg.id AND lr.deleted = FALSE ORDER BY lr.disc_number, lr.id LIMIT 1) AS main \
             FROM local_release_games lrg JOIN local_game lg ON lg.id = lrg.game_id AND lg.deleted = FALSE WHERE lrg.release_id = ? ORDER BY lg.id LIMIT 1",
        )
        .bind(release_id)
        .fetch_optional(&app.pool)
        .await?;
        let file: Option<String> = match &local {
            Some(r) => r.try_get::<Option<String>, _>("playlist")?.or(r.try_get::<Option<String>, _>("main")?),
            None => None,
        };
        let stem = file.as_deref().and_then(|p| Path::new(p).file_stem()).map(|s| s.to_string_lossy().to_string()).unwrap_or(name);
        out.push(ExportFile { release_id, platform, stem });
    }
    Ok(out)
}

/// The main server's row for the same picture: by its provider link, or -- art picked by hand
/// has none -- by the same slot on the same title or release.
async fn server_row(app: &App, source_id: u16, release_id: u64, c: &Candidate) -> Result<Option<(u64, Option<String>)>> {
    let url = c.source_url.as_deref().filter(|u| !u.is_empty());
    Ok(sqlx::query_as(
        "SELECT rm.id, rm.content_sha1 FROM remote_media rm \
         WHERE rm.source_id = ? AND rm.deleted = FALSE AND rm.media_type = ? AND (rm.sub_category <=> ?) \
           AND ((? IS NOT NULL AND rm.source_url = ?) \
             OR (COALESCE(rm.source, 'manual') = ? AND ( \
                  EXISTS (SELECT 1 FROM remote_title_media rtm WHERE rtm.source_id = rm.source_id AND rtm.media_id = rm.id AND rtm.title_id = ?) \
               OR EXISTS (SELECT 1 FROM remote_release_media rrm WHERE rrm.source_id = rm.source_id AND rrm.media_id = rm.id AND rrm.release_id = ?)))) \
         ORDER BY (rm.source_url <=> ?) DESC, rm.id LIMIT 1",
    )
    .bind(source_id)
    .bind(&c.media_type)
    .bind(&c.sub_category)
    .bind(url)
    .bind(url)
    .bind(c.source.as_deref().unwrap_or("manual"))
    .bind(c.title_id)
    .bind(release_id)
    .bind(url)
    .fetch_optional(&app.pool)
    .await?)
}

/// Where a candidate's original comes from: the main server's row (`remote_id`) or this
/// machine's file, and its hash when that is known before reading it.
struct SourceRef {
    remote_id: Option<u64>,
    sha: Option<String>,
}

/// A server keeps the original file (AVIF is only what it shows), so that is used. A client
/// keeps an AVIF copy for display, so it takes the main server's original instead; its own
/// copy is used only for art the server doesn't have, or while the server can't be reached.
async fn source_ref(app: &App, main: Option<u16>, release_id: u64, c: &Candidate) -> Result<SourceRef> {
    let local = SourceRef { remote_id: None, sha: c.sha1.clone() };
    let Some(source_id) = main else { return Ok(local) };
    let row = if c.remote { Some((c.id, c.sha1.clone())) } else { server_row(app, source_id, release_id, c).await? };
    Ok(match row {
        Some((rid, sha)) => SourceRef { remote_id: Some(rid), sha },
        None => local,
    })
}

/// The original picture to export from, and its hash. A client fetches the server's original
/// over the network each time it is needed and keeps no copy of it: the export cache holds
/// only conversions, and a file already exported unchanged is skipped before any fetch.
async fn source_bytes(app: &App, roots: &Roots, src: &SourceRef, c: &Candidate) -> Result<Option<(Vec<u8>, String)>> {
    let path = roots.resolve(c.root_role.as_deref(), &c.path);
    let local = || {
        std::fs::read(&path).ok().map(|bytes| {
            let sha = c.sha1.clone().unwrap_or_else(|| media_store::sha1_bytes(&bytes));
            (bytes, sha)
        })
    };
    let Some(rid) = src.remote_id else { return Ok(local()) };
    let fetched = async {
        let response = super::media_sync::from_main(app, &format!("/api/v1/media/{rid}"), None).await?;
        response.bytes().await.map(|b| b.to_vec()).map_err(|e| Error::unavailable(e.to_string()))
    };
    match fetched.await {
        Ok(bytes) => {
            let sha = src.sha.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| media_store::sha1_bytes(&bytes));
            Ok(Some((bytes, sha)))
        }
        Err(e) => match local().filter(|_| !c.remote) {
            Some(own) if Codec::sniff(&own.0).is_some_and(|k| k.can_read()) => {
                log::warn!("artwork export: media {}: using this machine's copy, the main server's original couldn't be fetched: {}", c.id, e.message);
                Ok(Some(own))
            }
            _ => Err(Error::unavailable(format!("the main server's original couldn't be fetched: {}", e.message))),
        },
    }
}

pub async fn run(app: &App, request: &ArtworkExportRequest, progress: &Progress) -> Result<ArtworkExportSummary> {
    let (emulator, artwork) = target(app, request).await?;
    validate_artwork(&artwork)?;
    let mut summary = ArtworkExportSummary { emulator: emulator.as_ref().map_or_else(|| artwork.dir.clone(), |e| e.name.clone()), ..Default::default() };
    let emulator_id = emulator.as_ref().map(|e| e.id);
    progress.force(0, None, "finding the games").await;
    let mut warnings = Vec::new();
    let files = files_for(app, request, &mut warnings).await?;
    summary.errors.extend(warnings);
    summary.games = files.len() as u64;
    let accepted: Vec<Codec> = artwork.formats.iter().filter_map(|f| Codec::from_name(f)).collect();
    let slots: &[&str] = if artwork.layout == "retroarch" { &SLOTS } else { &["box"] };
    let order = priorities(app).await?;
    let prefer_region = db_settings::get::<bool>(&app.pool, KEY_EXPORT_PREFER_REGION).await?.unwrap_or(true);
    let cache_mb = db_settings::get::<u64>(&app.pool, KEY_EXPORT_CACHE_MB).await?.unwrap_or(cache::DEFAULT_MAX_MB);
    let roots = Roots::load(&app.pool, &app.paths.media_dir()).await?;
    let cache_root = cache::dir(&app.paths.media_dir());
    cache::drop_originals(&cache_root);
    // A client takes the originals from its main server: its own copies are for display.
    let main = if db_settings::instance_mode(&app.pool).await? == db_settings::InstanceMode::Client { super::media_sync::main_source(app).await? } else { None };
    let combine = super::copy::combine_map(app).await?;
    let total = files.len() as u64;
    for (i, file) in files.iter().enumerate() {
        progress.report(i as u64, Some(total), &format!("{} of {total}: {}, {} written", i + 1, file.stem, summary.written)).await;
        let platform = Platform::from_igdb_id(file.platform).unwrap_or(Platform::Unknown);
        let system = super::copy::sanitize_segment(&super::copy::apply_combine(platform, &combine).dat_folder_name());
        let serial: Option<String> = sqlx::query_scalar("SELECT serial FROM `release` WHERE id = ?").bind(file.release_id).fetch_optional(&app.pool).await?.flatten();
        let found = candidates(app, main, file.release_id).await?;
        for slot in slots {
            let empty = Vec::new();
            let slot_order = order.get(*slot).unwrap_or(&empty);
            let Some(c) = pick(&found, slot, slot_order, prefer_region) else {
                summary.no_art += 1;
                continue;
            };
            if let Err(e) = export_one(app, &roots, &cache_root, cache_mb, main, emulator_id, &artwork, &accepted, &system, slot, file, serial.as_deref(), c, &mut summary).await {
                if summary.errors.len() < 50 {
                    summary.errors.push(format!("{} {slot}: {}", file.stem, e.message));
                }
            }
        }
    }
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
async fn export_one(
    app: &App,
    roots: &Roots,
    cache_root: &Path,
    cache_mb: u64,
    main: Option<u16>,
    emulator_id: Option<u64>,
    artwork: &EmulatorArtwork,
    accepted: &[Codec],
    system: &str,
    slot: &str,
    file: &ExportFile,
    serial: Option<&str>,
    c: &Candidate,
    summary: &mut ArtworkExportSummary,
) -> Result<()> {
    let src = source_ref(app, main, file.release_id, c).await?;
    // Already exported from these bytes: nothing to fetch. The file's extension depends on
    // the picture, so each format this target takes is tried.
    if let Some(sha) = src.sha.as_deref().filter(|s| !s.is_empty()) {
        for codec in accepted {
            let path_text = target_path(artwork, system, slot, &file.stem, serial, codec.ext()).display().to_string();
            let record: Option<(Option<u64>, Option<String>)> = sqlx::query_as("SELECT media_id, source_sha1 FROM media_exports WHERE path_sha1 = ?").bind(media_store::sha1_bytes(path_text.as_bytes())).fetch_optional(&app.pool).await?;
            if let Some((media_id, source_sha1)) = record {
                if media_id == c.local_id() && source_sha1.as_deref() == Some(sha) && Path::new(&path_text).is_file() {
                    summary.unchanged += 1;
                    return Ok(());
                }
            }
        }
    }
    let Some((bytes, sha)) = source_bytes(app, roots, &src, c).await? else {
        summary.no_art += 1;
        return Ok(());
    };
    let source = Codec::sniff(&bytes).ok_or_else(|| Error::invalid("not a picture this build knows"))?;
    let width = convert::width_of(&bytes).unwrap_or(0);
    let step = convert::plan(source, width, accepted, artwork.max_width).ok_or_else(|| Error::invalid(format!("{} can't be turned into {} here", source.name(), artwork.formats.join("/"))))?;
    let ext = match step {
        Step::Copy(k) => k.ext(),
        Step::Convert { to, .. } => to.ext(),
    };
    let to_path = target_path(artwork, system, slot, &file.stem, serial, ext);
    let path_text = to_path.display().to_string();
    let key = media_store::sha1_bytes(path_text.as_bytes());
    let record: Option<(Option<u64>, Option<String>)> = sqlx::query_as("SELECT media_id, source_sha1 FROM media_exports WHERE path_sha1 = ?").bind(&key).fetch_optional(&app.pool).await?;
    match &record {
        None if to_path.exists() => {
            summary.kept += 1;
            return Ok(());
        }
        Some((media_id, source_sha1)) if *media_id == c.local_id() && source_sha1.as_deref() == Some(sha.as_str()) && to_path.is_file() => {
            summary.unchanged += 1;
            return Ok(());
        }
        _ => {}
    }
    let out: Vec<u8> = match step {
        Step::Copy(_) => bytes,
        Step::Convert { to, width } => {
            let cached = cache::path_for(cache_root, &sha, to, width);
            match cache::hit(&cached).and_then(|p| std::fs::read(p).ok()) {
                Some(hit) => hit,
                None => {
                    let made = tokio::task::spawn_blocking(move || convert::convert(&bytes, to, width)).await.map_err(|e| Error::internal(e.to_string()))?.map_err(Error::invalid)?;
                    if let Err(e) = cache::put(cache_root, &cached, &made, cache_mb) {
                        log::warn!("artwork export: could not cache {}: {e}", cached.display());
                    }
                    made
                }
            }
        }
    };
    if matches!(step, Step::Convert { .. }) {
        summary.converted += 1;
    }
    if let Some(parent) = to_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::internal(format!("can't create {}: {e}", parent.display())))?;
    }
    let part = to_path.with_extension(format!("{ext}.part"));
    std::fs::write(&part, &out).map_err(|e| Error::internal(format!("can't write {}: {e}", part.display())))?;
    std::fs::rename(&part, &to_path).map_err(|e| Error::internal(format!("can't move {} into place: {e}", to_path.display())))?;
    sqlx::query("INSERT INTO media_exports (emulator_id, path_sha1, path, media_id, source_sha1) VALUES (?, ?, ?, ?, ?) ON DUPLICATE KEY UPDATE emulator_id = VALUES(emulator_id), path = VALUES(path), media_id = VALUES(media_id), source_sha1 = VALUES(source_sha1), written_at = NOW()")
        .bind(emulator_id)
        .bind(&key)
        .bind(&path_text)
        .bind(c.local_id())
        .bind(&sha)
        .execute(&app.pool)
        .await?;
    summary.written += 1;
    Ok(())
}

/// For the settings screens: the default provider order per slot.
pub fn default_priority_json() -> Value {
    Value::Object(DEFAULT_PRIORITY.iter().map(|(slot, order)| (slot.to_string(), Value::from(order.to_vec()))).collect())
}

/// For the settings screens: the media types each slot is filled from.
pub fn slot_types_json() -> Value {
    Value::Object(SLOTS.iter().map(|slot| (slot.to_string(), Value::from(slot_types(slot).iter().map(|(t, _)| *t).collect::<Vec<_>>()))).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: u64, media_type: &str, sub: Option<&str>, source: &str, path: &str, own: bool) -> Candidate {
        Candidate { id, media_type: media_type.into(), sub_category: sub.map(Into::into), source: Some(source.into()), path: path.into(), root_role: None, sha1: None, source_url: None, own, title_id: None, remote: false }
    }

    fn order(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_box_scan_beats_a_poster_and_the_order_decides_box_or_cover() {
        let c = vec![
            cand(1, "cover", None, "steamgriddb", "steamgriddb/1/common/cover.png", false),
            cand(2, "box", Some("front"), "igdb", "igdb/2/common/box_front.jpg", false),
            cand(3, "box", Some("front"), "thegamesdb", "thegamesdb/3/JP/box_front.jpg", true),
            cand(4, "box", Some("back"), "thegamesdb", "thegamesdb/3/JP/box_back.jpg", true),
        ];
        let o = order(DEFAULT_PRIORITY[0].1);
        assert_eq!(pick(&c, "box", &o, false).map(|c| c.id), Some(3), "TheGamesDB's box first by default");
        assert_eq!(pick(&c[..2], "box", &o, false).map(|c| c.id), Some(2), "IGDB before SteamGridDB in the order");
        assert_eq!(pick(&c[..2], "box", &order(&["steamgriddb", "igdb"]), false).map(|c| c.id), Some(1), "a box front and a cover are one slot: the order decides");
        assert_eq!(pick(&c[..1], "box", &o, false).map(|c| c.id), Some(1), "a cover when there is no box");
        assert_eq!(pick(&c, "logo", &o, false).map(|c| c.id), None);
    }

    #[test]
    fn prefer_region_puts_the_regional_file_first() {
        let c = vec![cand(1, "box", Some("front"), "mobygames", "mobygames/1/common/box_front.jpg", false), cand(2, "box", Some("front"), "igdb", "igdb/2/JP/box_front.jpg", true)];
        let o = order(&["manual", "mobygames", "igdb"]);
        assert_eq!(pick(&c, "box", &o, false).map(|c| c.id), Some(1), "the order wins");
        assert_eq!(pick(&c, "box", &o, true).map(|c| c.id), Some(2), "the region wins when asked");
        let igdb = vec![cand(1, "box", Some("front"), "igdb", "igdb/7/common/box_front.jpg", true), cand(2, "box", Some("front"), "igdb", "igdb/7/JP/box_front.jpg", true)];
        assert_eq!(pick(&igdb, "box", &o, false).map(|c| c.id), Some(2), "one provider's regional box before its main one");
    }

    #[test]
    fn paths_follow_each_layout() {
        let ra = EmulatorArtwork { dir: "/ra/thumbnails".into(), ..EmulatorArtwork::retroarch() };
        assert_eq!(target_path(&ra, "Nintendo - Super Nintendo Entertainment System", "box", "Hoge: Quest & Fuga?", None, "png"), Path::new("/ra/thumbnails/Nintendo - Super Nintendo Entertainment System/Named_Boxarts/Hoge_ Quest _ Fuga_.png"));
        assert_eq!(target_path(&ra, "Sony - PlayStation", "snap", "Hoge Quest (Disc 1)", None, "png").file_name().unwrap(), "Hoge Quest (Disc 1).png");
        let covers = EmulatorArtwork { dir: "/pcsx2/covers".into(), ..EmulatorArtwork::cover_folder() };
        assert_eq!(target_path(&covers, "x", "box", "Hoge Quest (USA)", Some("SLUS-20062, SLUS-20062GH"), "jpg"), Path::new("/pcsx2/covers/SLUS-20062.jpg"));
        assert_eq!(target_path(&covers, "x", "box", "Hoge Quest (USA)", None, "jpg"), Path::new("/pcsx2/covers/Hoge Quest (USA).jpg"), "no serial: the file name");
        let by_name = EmulatorArtwork { layout: "cover_folder_name".into(), ..covers };
        assert_eq!(target_path(&by_name, "x", "box", "Hoge Quest (USA)", Some("SLUS-20062"), "jpg"), Path::new("/pcsx2/covers/Hoge Quest (USA).jpg"), "by name: the file name even with a serial");
    }

    #[test]
    fn first_serials() {
        assert_eq!(first_serial(Some(" SLPS-00001, SLPS-00002")), Some("SLPS-00001".to_string()));
        assert_eq!(first_serial(Some("")), None);
        assert_eq!(first_serial(None), None);
    }
}
