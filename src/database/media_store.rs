//! Where media files live and how rows point at them (docs/plan/database.md, "Media storage").
//!
//! - **Roots:** a new file goes to the first configured `media_dirs` entry of its role
//!   (`image` / `video` / `audio`), else the app's own `assets/media` (role `app`). The row
//!   stores `root_role` plus a `path` relative to that root with `/` separators, so a
//!   library moved to another drive or OS only needs its media dirs re-pointed. Rows from
//!   before the layout keep `root_role = NULL` and an absolute path (`media_relayout`
//!   moves them).
//! - **Layout:** by what the art is of, not by where the game happens to be filed.
//!   Provider art goes under the provider's own id for the game,
//!   `<source>/<provider id>/<Region>/<type>[_<sub>].<ext>`, with `common` for the region when
//!   it is not one release group's (an IGDB cover) and
//!   `<source>/<provider id>/common/{screenshots,videos,fanart}/<original name>.<ext>` for the
//!   list types. One IGDB game is one folder whichever platforms it is on here: filed under
//!   `<Platform>/<Title>`, as it used to be, a cover shared by the PS4 and the Xbox One title
//!   sat in whichever folder fetched it first, and moved when a title was renamed or merged.
//!   The user's own uploads go by what they were uploaded to: `release/<id>/`, `title/<id>/`,
//!   `franchise/<id>/`, `keyword/<id>/` (`crossplatform/<IGDB id>/` is reserved for uploads
//!   to a game across its platforms). Provider art that came without an id is kept apart
//!   under `<source>/_<level>/<id>/`.
//!   A release group is every release of one title on one platform in one region with the
//!   same base name -- i.e. its revisions -- and they all link the one file. Demo /
//!   prototype / beta releases have their own `<Region>/_prerelease/` folder for art of their
//!   own and otherwise are linked to the retail group's art.
//!   Audio keeps its original file name in the audio root.
//! - **Names:** a provider download takes the slot's fixed name, replacing a file only an
//!   unlinked row of the app's still points at; an upload whose name is taken is refused
//!   unless the user chose to overwrite it or have it renamed ([`OnExisting`]).
//! - **Dedup:** identical bytes (`content_sha1`) are stored once and linked again.
//!
//! Only files the app itself writes live here; the user's own files are never moved or
//! deleted.

use std::path::{Path, PathBuf};

use sha1::{Digest, Sha1};
use sqlx::mysql::MySqlPool;
use sqlx::Row;

use super::error::DatabaseError;

/// Media types stored per title rather than per release group.
pub const TITLE_LEVEL_TYPES: [&str; 3] = ["screenshot", "video", "fanart"];
/// `media.sub_category`'s ENUM values (migrations/0001_schema.sql).
pub const SUB_CATEGORIES: [&str; 7] = ["front", "back", "top", "bottom", "left", "right", "spine"];

/// Largest media file this server takes in: a video or a music track.
pub const MAX_MEDIA_BYTES: usize = 512 * 1024 * 1024;
/// Largest image: far above any cover, screenshot or fan art a provider serves.
pub const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;

/// Reads `response`'s body, giving up as soon as it passes `limit` bytes: a declared
/// `Content-Length` over it is refused before any of the body is read, and a body that never
/// declared one is cut off at the limit rather than held whole.
pub async fn read_capped(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, DatabaseError> {
    let what = response_url_text(&response);
    let too_big = || DatabaseError::Fetch(format!("{what} is larger than {} MiB", limit >> 20));
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(too_big());
    }
    let mut body = Vec::with_capacity(response.content_length().map_or(0, |n| n as usize));
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > limit {
            return Err(too_big());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn response_url_text(response: &reqwest::Response) -> String {
    let url = response.url();
    format!("{}{}", url.host_str().unwrap_or_default(), url.path())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootRole {
    App,
    Image,
    Video,
    Audio,
}

impl RootRole {
    pub fn name(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Image => "image",
            Self::Video => "video",
            Self::Audio => "audio",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "app" => Some(Self::App),
            "image" => Some(Self::Image),
            "video" => Some(Self::Video),
            "audio" => Some(Self::Audio),
            _ => None,
        }
    }

    /// The configured-dir role a media type is written under.
    /// The most bytes one file of this role may hold ([`MAX_MEDIA_BYTES`], [`MAX_IMAGE_BYTES`]).
    pub fn max_bytes(self) -> usize {
        match self {
            Self::Image => MAX_IMAGE_BYTES,
            Self::App | Self::Video | Self::Audio => MAX_MEDIA_BYTES,
        }
    }

    pub fn for_media_type(media_type: &str) -> Self {
        match media_type {
            "video" => Self::Video,
            "bgm" => Self::Audio,
            _ => Self::Image,
        }
    }
}

/// The media roots and cache tiers as configured right now.
#[derive(Debug, Clone)]
pub struct Roots {
    pub app: PathBuf,
    pub image: Vec<PathBuf>,
    pub video: Vec<PathBuf>,
    pub audio: Vec<PathBuf>,
    pub tier1: Option<PathBuf>,
    pub tier2: Option<PathBuf>,
    /// Thumbnail dir used when no cache tier is configured (`media_cache` tier 0).
    pub tier0: PathBuf,
}

impl Roots {
    pub async fn load(pool: &MySqlPool, app_media_dir: &Path) -> Result<Self, DatabaseError> {
        let dirs = super::storage::list_media_dirs(pool).await?;
        let of = |role: &str| dirs.iter().filter(|d| d.role == role).map(|d| PathBuf::from(&d.path)).collect::<Vec<_>>();
        Ok(Self {
            app: app_media_dir.to_path_buf(),
            image: of("image"),
            video: of("video"),
            audio: of("audio"),
            tier1: of("tier1_cache").into_iter().next(),
            tier2: of("tier2_cache").into_iter().next(),
            tier0: app_media_dir.join("_cache"),
        })
    }

    fn configured(&self, role: RootRole) -> &[PathBuf] {
        match role {
            RootRole::App => &[],
            RootRole::Image => &self.image,
            RootRole::Video => &self.video,
            RootRole::Audio => &self.audio,
        }
    }

    /// Where a new file of `role` is written: the first configured dir of the role, else
    /// the app's media dir.
    pub fn write_root(&self, role: RootRole) -> (RootRole, &Path) {
        match self.configured(role).first() {
            Some(dir) => (role, dir.as_path()),
            None => (RootRole::App, self.app.as_path()),
        }
    }

    /// The absolute path of a row: a legacy row (`root_role` NULL) is already absolute; a
    /// relative one is looked up under every configured root of its role (first existing
    /// file wins, else the first root's candidate).
    pub fn resolve(&self, root_role: Option<&str>, path: &str) -> PathBuf {
        let Some(role) = root_role.and_then(RootRole::from_name) else { return PathBuf::from(path) };
        let candidates: Vec<PathBuf> = match role {
            RootRole::App => vec![self.app.clone()],
            other => self.configured(other).iter().cloned().chain(std::iter::once(self.app.clone())).collect(),
        };
        let joined: Vec<PathBuf> = candidates.iter().map(|root| join_rel(root, path)).collect();
        joined.iter().find(|p| p.is_file()).cloned().unwrap_or_else(|| joined[0].clone())
    }
}

/// `root` + a `/`-separated relative path, with the platform's own separators.
pub fn join_rel(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').filter(|s| !s.is_empty()).fold(root.to_path_buf(), |p, seg| p.join(seg))
}

/// One path component that is valid on every target OS: no `<>:"/\|?*` or control
/// characters, no trailing dots/spaces (Windows strips them), not a reserved device name
/// (CON, NUL, COM1, ...), at most 100 characters, never empty.
pub fn sanitize_component(name: &str) -> String {
    let replaced: String = name.chars().map(|c| if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') { '_' } else { c }).collect();
    let mut s: String = replaced.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(100).collect();
    while s.ends_with('.') || s.ends_with(' ') {
        s.pop();
    }
    if s.is_empty() {
        return "_".to_string();
    }
    let stem = s.split('.').next().unwrap_or("").trim().to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        s.insert(0, '_');
    }
    s
}

pub fn sha1_bytes(bytes: &[u8]) -> String {
    Sha1::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// The releases one set of per-release art is shared by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseGroup {
    /// The group's own part of a path: `<Region>[/_prerelease]` ([`region_folder`]).
    pub folder: String,
    pub title_id: Option<u64>,
    /// Same title, platform, region and base name (revisions), same retail/pre-release kind.
    pub members: Vec<u64>,
    /// For a retail group: the same region's demo / prototype / beta releases, linked to
    /// this group's art for any slot they have nothing of their own for.
    pub dependents: Vec<u64>,
}

/// `<Region>[/_prerelease]`; a release with no region is `common`.
pub fn region_folder(region: &str, prerelease: bool) -> String {
    let mut folder = if region.trim().is_empty() { COMMON.to_string() } else { sanitize_component(region) };
    if prerelease {
        folder.push_str("/_prerelease");
    }
    folder
}

/// The region folder of art that is not one release group's.
const COMMON: &str = "common";

/// The folder art goes in (see the module doc): `owner` is the level and id it is linked to
/// first (`("release", 12)`), `region` a release group's [`region_folder`], `list` the
/// sub-folder of a list type.
pub fn layout_folder(source: &str, provider_id: Option<&str>, owner: (&str, u64), region: Option<&str>, list: Option<&str>) -> String {
    let region = region.unwrap_or(COMMON);
    let mut folder = match provider_id.map(str::trim).filter(|id| !id.is_empty()) {
        _ if source == "manual" => format!("{}/{}", owner.0, owner.1),
        Some(id) => format!("{}/{}/{region}", source_folder(source), sanitize_component(id)),
        None => format!("{}/_{}/{}/{region}", source_folder(source), owner.0, owner.1),
    };
    if let Some(list) = list {
        folder.push('/');
        folder.push_str(list);
    }
    folder
}

/// SQL for "release `alias` is a demo / prototype / beta / trial": the DAT flags, or a tag in
/// the name the DAT import didn't flag (`(C86 Prototype)`, `(Otameshiban)`, `(Sample)`, ...).
fn prerelease_sql(alias: &str) -> String {
    format!(
        "({a}.is_demo OR {a}.is_beta OR {a}.is_prototype OR {a}.name REGEXP '\\\\([^)]*\\\\b(demo|beta|proto|prototype|sample|trial|preview|kiosk|otameshiban|taikenban)\\\\b[^)]*\\\\)')",
        a = alias
    )
}

pub async fn release_group(pool: &MySqlPool, release_id: u64) -> Result<Option<ReleaseGroup>, DatabaseError> {
    let pre_sql = prerelease_sql("r");
    let head = format!(
        "SELECT r.app_platform_id, COALESCE(r.region, '') AS region, r.norm_hash, r.norm_name, CAST({pre_sql} AS SIGNED) AS pre, \
                tr.title_id \
         FROM `release` r LEFT JOIN title_releases tr ON tr.release_id = r.id \
         WHERE r.id = ? LIMIT 1"
    );
    let Some(row) = sqlx::query(sqlx::AssertSqlSafe(head))
    .bind(release_id)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };
    let platform_id: u16 = row.try_get("app_platform_id")?;
    let region: String = row.try_get("region")?;
    let norm_hash: String = row.try_get("norm_hash")?;
    let pre: bool = row.try_get::<i64, _>("pre")? != 0;
    let title_id: Option<u64> = row.try_get("title_id")?;
    let scope = if title_id.is_some() { "JOIN title_releases tr ON tr.release_id = r.id AND tr.title_id = ?" } else { "" };
    let ids = |same_pre: bool, same_name: bool| {
        let sql = format!(
            "SELECT r.id FROM `release` r {scope} WHERE r.deleted = FALSE AND r.app_platform_id = ? AND COALESCE(r.region, '') = ? \
             AND {pre_sql} = ? {} ORDER BY r.id",
            if same_name { "AND r.norm_hash = ?" } else { "" }
        );
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
        if let Some(t) = title_id {
            q = q.bind(t);
        }
        q = q.bind(platform_id).bind(region.clone()).bind(if same_pre { pre } else { !pre });
        if same_name {
            q = q.bind(norm_hash.clone());
        }
        async move { q.fetch_all(pool).await.map(|rows| rows.iter().filter_map(|r| r.try_get::<u64, _>("id").ok()).collect::<Vec<u64>>()) }
    };
    let mut members = ids(true, true).await?;
    if !members.contains(&release_id) {
        members.push(release_id);
    }
    let dependents = if pre || title_id.is_none() { Vec::new() } else { ids(false, false).await? };
    Ok(Some(ReleaseGroup { folder: region_folder(&region, pre), title_id, members, dependents }))
}

/// Where a file goes: a folder under a role's root, and the file name stem to start from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub role: RootRole,
    pub folder: String,
    /// `box_front`, `cover`, ... or, for audio, the original file name without extension.
    pub stem: String,
    /// Audio's collision style is `name (2)`, art's is `box_front_2`.
    pub keep_name: bool,
}

pub fn slot_stem(media_type: &str, sub_category: Option<&str>) -> String {
    match sub_category {
        Some(sub) => format!("{media_type}_{sub}"),
        None => media_type.to_string(),
    }
}

/// The `n`th candidate name for a stem (n = 1 is the plain name).
pub fn candidate_name(stem: &str, ext: &str, n: u32, keep_name: bool) -> String {
    match (n, keep_name) {
        (1, _) => format!("{stem}.{ext}"),
        (n, true) => format!("{stem} ({n}).{ext}"),
        (n, false) => format!("{stem}_{n}.{ext}"),
    }
}

/// The slot's own name for `placement` (`n = 1`), relative to its root.
pub fn plain_rel(placement: &Placement, ext: &str) -> String {
    format!("{}/{}", placement.folder, candidate_name(&placement.stem, ext, 1, placement.keep_name)).trim_start_matches('/').to_string()
}

/// Whether `rel` under `root` is taken: a file on disk or a row (retired ones included).
pub async fn path_taken(pool: &MySqlPool, root: &Path, root_role: RootRole, rel: &str) -> Result<bool, DatabaseError> {
    if join_rel(root, rel).exists() {
        return Ok(true);
    }
    Ok(sqlx::query("SELECT 1 FROM media WHERE root_role = ? AND path = ? LIMIT 1").bind(root_role.name()).bind(rel).fetch_optional(pool).await?.is_some())
}

/// A row already holding these bytes as `media_type` (stored again, it would only be linked).
pub async fn has_bytes(pool: &MySqlPool, bytes: &[u8], media_type: &str) -> Result<bool, DatabaseError> {
    Ok(sqlx::query("SELECT 1 FROM media WHERE content_sha1 = ? AND media_type = ? AND deleted = FALSE LIMIT 1").bind(sha1_bytes(bytes)).bind(media_type).fetch_optional(pool).await?.is_some())
}

/// The folder a source's files go in: the provider's name, or `user` for uploads.
pub fn source_folder(source: &str) -> String {
    if source == "manual" {
        "user".to_string()
    } else {
        sanitize_component(source)
    }
}

/// The sub-folder of a list type under its source folder.
fn list_folder(media_type: &str) -> &str {
    match media_type {
        "screenshot" => "screenshots",
        "video" => "videos",
        other => other,
    }
}

/// A file name's stem (no folders, no extension), safe as a path component.
fn original_stem(name: &str) -> Option<String> {
    let name = name.split(['?', '#']).next().unwrap_or(name);
    let file = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    (!stem.trim().is_empty()).then(|| sanitize_component(stem))
}

/// A free relative path for `placement` under `root`: not on disk and not in `media`.
pub async fn free_path(pool: &MySqlPool, root: &Path, root_role: RootRole, placement: &Placement, ext: &str) -> Result<String, DatabaseError> {
    for n in 1..10_000u32 {
        let rel = format!("{}/{}", placement.folder, candidate_name(&placement.stem, ext, n, placement.keep_name));
        let rel = rel.trim_start_matches('/').to_string();
        if join_rel(root, &rel).exists() {
            continue;
        }
        let taken = sqlx::query("SELECT 1 FROM media WHERE root_role = ? AND path = ? LIMIT 1").bind(root_role.name()).bind(&rel).fetch_optional(pool).await?.is_some();
        if !taken {
            return Ok(rel);
        }
    }
    Err(DatabaseError::Config(format!("no free file name in {}", placement.folder)))
}

/// Writes `bytes` atomically (`.part` then rename).
/// Moves a file, creating the destination's folders; across file systems it is copied and
/// the original removed.
pub async fn move_file(from: &Path, to: &Path) -> Result<(), DatabaseError> {
    if let Some(parent) = to.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    if tokio::fs::rename(from, to).await.is_ok() {
        return Ok(());
    }
    let tmp = to.with_extension(format!("{}.part", to.extension().and_then(|e| e.to_str()).unwrap_or("")));
    if let Err(e) = tokio::fs::copy(from, &tmp).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e.into());
    }
    if let Err(e) = tokio::fs::rename(&tmp, to).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e.into());
    }
    let _ = tokio::fs::remove_file(from).await;
    Ok(())
}

pub async fn write_file(path: &Path, bytes: &[u8]) -> Result<(), DatabaseError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp = path.with_extension(format!("{}.part", path.extension().and_then(|e| e.to_str()).unwrap_or("")));
    tokio::fs::write(&tmp, bytes).await?;
    if let Err(e) = tokio::fs::rename(&tmp, path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e.into());
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Stored {
    pub media_id: u64,
    pub path: PathBuf,
    pub bytes: u64,
    /// false: identical bytes (or the same source URL) were already stored and are reused.
    pub fresh: bool,
}

/// What a new file does when its name in the layout is already taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnExisting {
    /// The next free name (`box_front_2.jpg`, `name (2).png`).
    #[default]
    Rename,
    /// Replace the file; rows still pointing at it get the new bytes. Only on the user's say.
    Overwrite,
    /// Provider downloads: replace the file when only retired rows point at it (art the app
    /// wrote and nothing shows any more), else rename. A file no row knows is the user's own
    /// and is never replaced.
    ReplaceRetired,
}

/// What to record about a new file.
#[derive(Debug, Clone)]
pub struct NewMedia<'a> {
    pub media_type: &'a str,
    pub sub_category: Option<&'a str>,
    pub source: &'a str,
    /// The provider's own id for whatever this art depicts (an IGDB game id, a SteamGridDB
    /// game id...). With `source` it is a machine-independent handle, which is what lets
    /// `sync::relink_media` rebuild `title_media`/`release_media` after a catalogue
    /// replacement renumbers every title and release. `None` for manual uploads.
    pub provider_id: Option<&'a str>,
    pub source_url: Option<&'a str>,
    pub ext: &'a str,
    /// Identical bytes already stored: still give this a row of its own (sharing the
    /// file) instead of reusing the existing row -- a manual pick must be its own
    /// `source = 'manual'` row even when a provider supplied the same image.
    pub own_row: bool,
    pub on_existing: OnExisting,
}

/// Stores `bytes` under `placement` (or reuses a row with the same bytes and type) and
/// returns the row. Nothing is linked here.
pub async fn store_bytes(pool: &MySqlPool, roots: &Roots, placement: &Placement, meta: &NewMedia<'_>, bytes: &[u8]) -> Result<Stored, DatabaseError> {
    let sha1 = sha1_bytes(bytes);
    if let Some(row) = sqlx::query("SELECT id, path, root_role FROM media WHERE content_sha1 = ? AND media_type = ? AND deleted = FALSE ORDER BY id LIMIT 1").bind(&sha1).bind(meta.media_type).fetch_optional(pool).await? {
        let media_id: u64 = row.try_get("id")?;
        let root_role: Option<String> = row.try_get("root_role")?;
        let rel: String = row.try_get("path")?;
        let path = roots.resolve(root_role.as_deref(), &rel);
        // On record but not on disk (a database restored without its media, a folder
        // emptied by hand): these are the very bytes, so the file goes back where the row
        // says rather than the row going on pointing at nothing.
        if !path.is_file() {
            write_file(&path, bytes).await?;
        }
        if !meta.own_row {
            return Ok(Stored { media_id, path, bytes: bytes.len() as u64, fresh: false });
        }
        let result = sqlx::query("INSERT INTO media (media_type, sub_category, path, root_role, content_sha1, bytes, source, provider_id, source_url) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(meta.media_type)
            .bind(meta.sub_category)
            .bind(&rel)
            .bind(root_role)
            .bind(&sha1)
            .bind(bytes.len() as u64)
            .bind(meta.source)
            .bind(meta.provider_id)
            .bind(meta.source_url)
            .execute(pool)
            .await?;
        return Ok(Stored { media_id: result.last_insert_id(), path, bytes: bytes.len() as u64, fresh: false });
    }
    let (root_role, root) = roots.write_root(placement.role);
    let plain = plain_rel(placement, meta.ext);
    let overwrite = match meta.on_existing {
        OnExisting::Rename => false,
        OnExisting::Overwrite => true,
        OnExisting::ReplaceRetired => {
            let rows: Vec<bool> = sqlx::query_scalar("SELECT deleted FROM media WHERE root_role = ? AND path = ?").bind(root_role.name()).bind(&plain).fetch_all(pool).await?;
            !rows.is_empty() && rows.iter().all(|deleted| *deleted)
        }
    };
    let rel = if overwrite { plain } else { free_path(pool, root, root_role, placement, meta.ext).await? };
    let path = join_rel(root, &rel);
    write_file(&path, bytes).await?;
    if overwrite {
        let live: Vec<u64> = sqlx::query_scalar("SELECT id FROM media WHERE root_role = ? AND path = ? AND deleted = FALSE").bind(root_role.name()).bind(&rel).fetch_all(pool).await?;
        for id in live {
            sqlx::query("UPDATE media SET content_sha1 = ?, bytes = ? WHERE id = ?").bind(&sha1).bind(bytes.len() as u64).bind(id).execute(pool).await?;
            super::media_cache::forget(pool, roots, id).await?;
        }
    }
    let result = sqlx::query("INSERT INTO media (media_type, sub_category, path, root_role, content_sha1, bytes, source, provider_id, source_url) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)")
        .bind(meta.media_type)
        .bind(meta.sub_category)
        .bind(&rel)
        .bind(root_role.name())
        .bind(&sha1)
        .bind(bytes.len() as u64)
        .bind(meta.source)
        .bind(meta.provider_id)
        .bind(meta.source_url)
        .execute(pool)
        .await?;
    Ok(Stored { media_id: result.last_insert_id(), path, bytes: bytes.len() as u64, fresh: true })
}

/// File extension for downloaded or uploaded media, from the URL / file name, else the
/// Content-Type, else `jpg`.
pub fn extension_for(name: &str, content_type: Option<&str>) -> &'static str {
    if let Some(ext) = known_extension(name) {
        return ext;
    }
    match content_type.unwrap_or("") {
        ct if ct.contains("png") => "png",
        ct if ct.contains("webp") => "webp",
        ct if ct.contains("gif") => "gif",
        ct if ct.contains("avif") => "avif",
        ct if ct.contains("svg") => "svg",
        ct if ct.contains("mp4") => "mp4",
        ct if ct.contains("webm") => "webm",
        ct if ct.contains("mpeg") => "mp3",
        ct if ct.contains("ogg") => "ogg",
        ct if ct.contains("flac") => "flac",
        _ => "jpg",
    }
}

/// The extension a URL or file name spells, when it is one media is stored as; `None` when
/// it names none (only the response's Content-Type could say).
pub fn known_extension(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    let lower = lower.split(['?', '#']).next().unwrap_or("");
    const KNOWN: [(&str, &str); 14] = [
        (".png", "png"),
        (".jpeg", "jpg"),
        (".jpg", "jpg"),
        (".webp", "webp"),
        (".gif", "gif"),
        (".avif", "avif"),
        (".svg", "svg"),
        (".mp4", "mp4"),
        (".webm", "webm"),
        (".mkv", "mkv"),
        (".mp3", "mp3"),
        (".ogg", "ogg"),
        (".flac", "flac"),
        (".wav", "wav"),
    ];
    if let Some((_, ext)) = KNOWN.iter().find(|(suffix, _)| lower.ends_with(suffix)) {
        return Some(ext);
    }
    KNOWN.iter().find(|(suffix, _)| lower.contains(suffix)).map(|(_, ext)| *ext)
}

/// Every extension a role's files are stored with ([`ext_allowed`]), most common first.
pub fn role_extensions(role: RootRole) -> &'static [&'static str] {
    match role {
        RootRole::Video => &["mp4", "webm", "mkv"],
        RootRole::Audio => &["mp3", "ogg", "flac", "wav"],
        _ => &["jpg", "png", "webp", "gif", "avif", "svg"],
    }
}

/// Whether `bytes` start the way a file of `ext` does: enough to tell a real picture or
/// video from an empty file or an HTML error page saved under a picture's name.
pub fn looks_like(ext: &str, bytes: &[u8]) -> bool {
    let at = |offset: usize, magic: &[u8]| bytes.len() >= offset + magic.len() && &bytes[offset..offset + magic.len()] == magic;
    match ext {
        "png" => at(0, b"\x89PNG\r\n\x1a\n"),
        "jpg" => at(0, &[0xFF, 0xD8, 0xFF]),
        "gif" => at(0, b"GIF8"),
        "webp" => at(0, b"RIFF") && at(8, b"WEBP"),
        "avif" | "mp4" => at(4, b"ftyp"),
        "webm" | "mkv" => at(0, &[0x1A, 0x45, 0xDF, 0xA3]),
        "svg" => String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]).contains("<svg"),
        "mp3" => at(0, b"ID3") || (bytes.len() > 1 && bytes[0] == 0xFF && bytes[1] & 0xE0 == 0xE0),
        "ogg" => at(0, b"OggS"),
        "flac" => at(0, b"fLaC"),
        "wav" => at(0, b"RIFF") && at(8, b"WAVE"),
        _ => !bytes.is_empty(),
    }
}

/// Media reuse (`media_reuse_existing`): the file a download would land on is already
/// there, from an earlier install whose database is gone (a fresh database pointed at the
/// old media folder). If no live row claims it and it looks like what the slot holds, it is
/// recorded as it is and returned instead of being downloaded again -- and instead of the
/// download being written beside it as `box_front_2.jpg`.
///
/// Per provider and per slot: the name is the slot's own ([`plain_rel`]), so only what that
/// provider would have written there is taken, and a slot with no file still downloads.
/// List types (screenshots, videos, fan art) are named after the URL's own file name, so the
/// same check finds them. `exts` are the extensions to try: the URL's, or every one of the
/// role's when the URL names none.
///
/// Not covered: the user's own uploads (`user/release/<id>/...`: the folder is a row id,
/// which a new database numbers differently), and files from before the media layout, which
/// rows held by absolute path in a different tree.
pub async fn adopt_existing(pool: &MySqlPool, roots: &Roots, placement: &Placement, meta: &NewMedia<'_>, exts: &[&str]) -> Result<Option<Stored>, DatabaseError> {
    let (root_role, root) = roots.write_root(placement.role);
    for ext in exts {
        let rel = plain_rel(placement, ext);
        let path = join_rel(root, &rel);
        if !path.is_file() {
            continue;
        }
        let claimed = sqlx::query("SELECT 1 FROM media WHERE root_role = ? AND path = ? AND deleted = FALSE LIMIT 1").bind(root_role.name()).bind(&rel).fetch_optional(pool).await?.is_some();
        if claimed {
            continue;
        }
        let bytes = tokio::fs::read(&path).await?;
        if !looks_like(ext, &bytes) || bytes.is_empty() {
            continue;
        }
        let sha1 = sha1_bytes(&bytes);
        // The same picture already on record elsewhere: that row is it, as `store_bytes`
        // would have found after downloading.
        if let Some(row) = sqlx::query("SELECT id, path, root_role FROM media WHERE content_sha1 = ? AND media_type = ? AND deleted = FALSE ORDER BY id LIMIT 1").bind(&sha1).bind(meta.media_type).fetch_optional(pool).await? {
            let media_id: u64 = row.try_get("id")?;
            let found = roots.resolve(row.try_get::<Option<String>, _>("root_role")?.as_deref(), &row.try_get::<String, _>("path")?);
            return Ok(Some(Stored { media_id, path: found, bytes: bytes.len() as u64, fresh: false }));
        }
        let result = sqlx::query("INSERT INTO media (media_type, sub_category, path, root_role, content_sha1, bytes, source, provider_id, source_url) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(meta.media_type)
            .bind(meta.sub_category)
            .bind(&rel)
            .bind(root_role.name())
            .bind(&sha1)
            .bind(bytes.len() as u64)
            .bind(meta.source)
            .bind(meta.provider_id)
            .bind(meta.source_url)
            .execute(pool)
            .await?;
        log::debug!("media reuse: took {} as it is", path.display());
        return Ok(Some(Stored { media_id: result.last_insert_id(), path, bytes: bytes.len() as u64, fresh: false }));
    }
    Ok(None)
}

/// Whether any media root (configured, or the app's own media dir) holds at least one file,
/// leaving out the thumbnail cache: the first-run question about reusing them is only worth
/// asking when there is something to reuse.
pub fn roots_hold_files(roots: &Roots) -> bool {
    fn any_file(dir: &Path, depth: u32) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else { return false };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_file() {
                return true;
            }
            if kind.is_dir() && depth < 8 && entry.file_name() != "_cache" && any_file(&entry.path(), depth + 1) {
                return true;
            }
        }
        false
    }
    roots.image.iter().chain(&roots.video).chain(&roots.audio).chain(std::iter::once(&roots.app)).any(|root| any_file(root, 0))
}


pub fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("jxl") => "image/jxl",
        Some("svg") => "image/svg+xml",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mkv") => "video/x-matroska",
        Some("mp3") => "audio/mpeg",
        Some("ogg") | Some("oga") => "audio/ogg",
        Some("opus") => "audio/opus",
        Some("flac") => "audio/flac",
        Some("wav") => "audio/wav",
        Some("m4a") | Some("m4b") | Some("alac") => "audio/mp4",
        Some("aac") => "audio/aac",
        Some("aiff") | Some("aif") => "audio/aiff",
        Some("wma") => "audio/x-ms-wma",
        Some("ape") => "audio/x-ape",
        Some("wv") => "audio/x-wavpack",
        Some("dsf") | Some("dff") => "audio/x-dsd",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// Which extensions a role accepts for uploads.
pub fn ext_allowed(role: RootRole, ext: &str) -> bool {
    match role {
        RootRole::Video => matches!(ext, "mp4" | "webm" | "mkv"),
        RootRole::Audio => matches!(ext, "mp3" | "ogg" | "flac" | "wav"),
        _ => matches!(ext, "png" | "jpg" | "webp" | "gif" | "avif" | "svg"),
    }
}

// ---------------------------------------------------------------------------
// linking
// ---------------------------------------------------------------------------

/// What a media row is linked to (docs/plan/database.md "Media attachment rule").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaTarget {
    Title(u64),
    Release(u64),
    Franchise(u64),
    Keyword(u64),
}

impl MediaTarget {
    pub fn junction(self) -> (&'static str, &'static str, u64) {
        match self {
            Self::Title(id) => ("title_media", "title_id", id),
            Self::Release(id) => ("release_media", "release_id", id),
            Self::Franchise(id) => ("franchise_media", "franchise_id", id),
            Self::Keyword(id) => ("keyword_media", "keyword_id", id),
        }
    }
}

pub async fn link(pool: &MySqlPool, media_id: u64, target: MediaTarget) -> Result<(), DatabaseError> {
    let (junction, key, id) = target.junction();
    let sql = format!("INSERT IGNORE INTO {junction} ({key}, media_id) VALUES (?, ?)");
    sqlx::query(sqlx::AssertSqlSafe(sql)).bind(id).bind(media_id).execute(pool).await?;
    Ok(())
}

pub async fn unlink(pool: &MySqlPool, media_id: u64, target: MediaTarget) -> Result<bool, DatabaseError> {
    let (junction, key, id) = target.junction();
    let sql = format!("DELETE FROM {junction} WHERE {key} = ? AND media_id = ?");
    Ok(sqlx::query(sqlx::AssertSqlSafe(sql)).bind(id).bind(media_id).execute(pool).await?.rows_affected() > 0)
}

/// Media ids linked to `target` in one slot (`sub_category` NULL matches NULL).
pub async fn slot_media(pool: &MySqlPool, target: MediaTarget, media_type: &str, sub_category: Option<&str>, only_source: Option<&str>) -> Result<Vec<(u64, Option<String>)>, DatabaseError> {
    let (junction, key, id) = target.junction();
    let sql = format!(
        "SELECT m.id, m.source FROM {junction} j JOIN media m ON m.id = j.media_id WHERE j.{key} = ? AND m.media_type = ? AND m.sub_category <=> ? AND m.deleted = FALSE {} ORDER BY m.id",
        if only_source.is_some() { "AND m.source = ?" } else { "" }
    );
    let mut q = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(id).bind(media_type).bind(sub_category);
    if let Some(s) = only_source {
        q = q.bind(s);
    }
    q.fetch_all(pool).await?.iter().map(|r| Ok((r.try_get("id")?, r.try_get("source")?))).collect()
}

/// Whether `target` holds a user-picked (`source = 'manual'`) item in the slot -- a
/// provider sync never adds or replaces anything there.
pub async fn has_manual(pool: &MySqlPool, target: MediaTarget, media_type: &str, sub_category: Option<&str>) -> Result<bool, DatabaseError> {
    Ok(!slot_media(pool, target, media_type, sub_category, Some("manual")).await?.is_empty())
}

/// Soft-deletes a row nothing links to any more. The file stays on disk (the relayout /
/// a later cleanup decides; nothing here deletes files).
pub async fn retire_if_unlinked(pool: &MySqlPool, media_id: u64) -> Result<bool, DatabaseError> {
    let linked = sqlx::query(
        "SELECT 1 FROM title_media WHERE media_id = ? UNION ALL SELECT 1 FROM release_media WHERE media_id = ? \
         UNION ALL SELECT 1 FROM franchise_media WHERE media_id = ? UNION ALL SELECT 1 FROM keyword_media WHERE media_id = ? \
         UNION ALL SELECT 1 FROM local_game WHERE cover_media_id = ? LIMIT 1",
    )
    .bind(media_id)
    .bind(media_id)
    .bind(media_id)
    .bind(media_id)
    .bind(media_id)
    .fetch_optional(pool)
    .await?
    .is_some();
    if linked {
        return Ok(false);
    }
    sqlx::query("UPDATE media SET deleted = TRUE WHERE id = ?").bind(media_id).execute(pool).await?;
    Ok(true)
}

/// Where art for `targets` goes ([`layout_folder`]), and which extra releases share it (a
/// single release's whole group, plus its dependents for slots they have nothing in).
/// `provider_id` is the provider's own id for what the art depicts; `None` for uploads.
pub async fn placement_for(pool: &MySqlPool, targets: &[MediaTarget], media_type: &str, sub_category: Option<&str>, original_name: Option<&str>, source: &str, provider_id: Option<&str>) -> Result<(Placement, Option<ReleaseGroup>), DatabaseError> {
    let role = RootRole::for_media_type(media_type);
    let stem = slot_stem(media_type, sub_category);
    if role == RootRole::Audio {
        let name = original_name.map(|n| n.rsplit(['/', '\\']).next().unwrap_or(n)).map(|n| n.rsplit_once('.').map_or(n, |(s, _)| s)).filter(|n| !n.trim().is_empty()).unwrap_or("track");
        return Ok((Placement { role, folder: String::new(), stem: sanitize_component(name), keep_name: true }, None));
    }
    let title_level = TITLE_LEVEL_TYPES.contains(&media_type);
    let release_ids: Vec<u64> = targets.iter().filter_map(|t| if let MediaTarget::Release(id) = t { Some(*id) } else { None }).collect();
    // One group's art has that group's region; anything wider is `common`.
    let mut group = None;
    if !title_level && !release_ids.is_empty() {
        if let Some(first) = release_group(pool, release_ids[0]).await? {
            if release_ids.iter().all(|id| first.members.contains(id)) {
                group = Some(first);
            }
        }
    }
    let owner = match targets.first() {
        Some(MediaTarget::Title(id)) => ("title", *id),
        Some(MediaTarget::Release(id)) => ("release", *id),
        Some(MediaTarget::Franchise(id)) => ("franchise", *id),
        Some(MediaTarget::Keyword(id)) => ("keyword", *id),
        None => ("unsorted", 0),
    };
    let folder = layout_folder(source, provider_id, owner, group.as_ref().map(|g| g.folder.as_str()), title_level.then(|| list_folder(media_type)));
    if title_level {
        let stem = original_name.and_then(original_stem).unwrap_or(stem);
        return Ok((Placement { role, folder, stem, keep_name: true }, None));
    }
    Ok((Placement { role, folder, stem, keep_name: false }, group))
}

/// Links `media_id` to `targets`; for a group placement also to every group member, and
/// to the group's dependents that have nothing in the slot yet.
pub async fn link_placed(pool: &MySqlPool, media_id: u64, targets: &[MediaTarget], group: Option<&ReleaseGroup>, media_type: &str, sub_category: Option<&str>) -> Result<u32, DatabaseError> {
    let mut linked = 0;
    for t in targets {
        link(pool, media_id, *t).await?;
        linked += 1;
    }
    if let Some(g) = group {
        for id in &g.members {
            if !targets.contains(&MediaTarget::Release(*id)) {
                link(pool, media_id, MediaTarget::Release(*id)).await?;
            }
        }
        for id in &g.dependents {
            if slot_media(pool, MediaTarget::Release(*id), media_type, sub_category, None).await?.is_empty() {
                link(pool, media_id, MediaTarget::Release(*id)).await?;
            }
        }
    }
    Ok(linked)
}

// ---------------------------------------------------------------------------
// relayout (job media_relayout)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct RelayoutSummary {
    pub candidates: u64,
    pub moved: u64,
    /// Identical bytes already stored elsewhere: links re-pointed, this row retired.
    pub merged: u64,
    /// Not under the app's media dir (not written by the app): left alone.
    pub skipped_not_app: u64,
    pub missing: u64,
    pub failed: u64,
    /// Files moved to where the layout puts them now: the configured image / video / audio
    /// dir instead of the app's media dir, and their `<source>` folder.
    pub rehomed: u64,
}

const JUNCTIONS: [(&str, &str); 4] = [("title_media", "title_id"), ("release_media", "release_id"), ("franchise_media", "franchise_id"), ("keyword_media", "keyword_id")];

async fn linked_targets(pool: &MySqlPool, media_id: u64) -> Result<Vec<MediaTarget>, DatabaseError> {
    let mut out = Vec::new();
    for (junction, key) in [("release_media", "release_id"), ("title_media", "title_id"), ("franchise_media", "franchise_id"), ("keyword_media", "keyword_id")] {
        let sql = format!("SELECT {key} AS id FROM {junction} WHERE media_id = ? ORDER BY {key}");
        for r in sqlx::query(sqlx::AssertSqlSafe(sql)).bind(media_id).fetch_all(pool).await? {
            let id: u64 = r.try_get("id")?;
            out.push(match junction {
                "release_media" => MediaTarget::Release(id),
                "title_media" => MediaTarget::Title(id),
                "franchise_media" => MediaTarget::Franchise(id),
                _ => MediaTarget::Keyword(id),
            });
        }
    }
    Ok(out)
}

/// Re-points every link of `from` to `to` and retires `from`.
async fn merge_into(pool: &MySqlPool, from: u64, to: u64) -> Result<(), DatabaseError> {
    let mut tx = pool.begin().await?;
    for (junction, key) in JUNCTIONS {
        let sql = format!("INSERT IGNORE INTO {junction} ({key}, media_id) SELECT {key}, ? FROM {junction} WHERE media_id = ?");
        sqlx::query(sqlx::AssertSqlSafe(sql)).bind(to).bind(from).execute(&mut *tx).await?;
        let sql = format!("DELETE FROM {junction} WHERE media_id = ?");
        sqlx::query(sqlx::AssertSqlSafe(sql)).bind(from).execute(&mut *tx).await?;
    }
    sqlx::query("UPDATE local_game SET cover_media_id = ? WHERE cover_media_id = ?").bind(to).bind(from).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM media_cache WHERE media_id = ?").bind(from).execute(&mut *tx).await?;
    sqlx::query("UPDATE media SET deleted = TRUE WHERE id = ?").bind(from).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Whether a row from before the layout, not yet moved, still points at `path`.
async fn still_used(pool: &MySqlPool, path: &str) -> Result<bool, DatabaseError> {
    Ok(sqlx::query("SELECT 1 FROM media WHERE root_role IS NULL AND deleted = FALSE AND path = ? LIMIT 1").bind(path).fetch_optional(pool).await?.is_some())
}

/// Moves media rows from before the layout (absolute path, `root_role` NULL) into it, then
/// every laid-out file that is not where the layout puts it now ([`rehome`]). Only files under the app's own media dir are moved -- anything else was
/// not written by the app and is left exactly where it is.
pub async fn relayout(pool: &MySqlPool, roots: &Roots, progress: &super::jobs::Progress) -> Result<RelayoutSummary, DatabaseError> {
    let rows = sqlx::query("SELECT id, media_type, sub_category, path, source_url, source, provider_id FROM media WHERE root_role IS NULL AND deleted = FALSE ORDER BY id").fetch_all(pool).await?;
    let mut s = RelayoutSummary { candidates: rows.len() as u64, ..Default::default() };
    let total = Some(s.candidates);
    let mut moved_paths = std::collections::HashSet::new();
    for (i, r) in rows.iter().enumerate() {
        progress.report(i as u64, total, &format!("moved {}, merged {}, skipped {}", s.moved, s.merged, s.skipped_not_app)).await;
        let media_id: u64 = r.try_get("id")?;
        let media_type: String = r.try_get("media_type")?;
        let sub: Option<String> = r.try_get("sub_category")?;
        let old_text: String = r.try_get("path")?;
        let old = PathBuf::from(&old_text);
        let source_url: Option<String> = r.try_get("source_url")?;
        let source: String = r.try_get::<Option<String>, _>("source")?.unwrap_or_else(|| "manual".to_string());
        let provider_id: Option<String> = r.try_get("provider_id")?;
        if !old.starts_with(&roots.app) {
            s.skipped_not_app += 1;
            continue;
        }
        if moved_paths.contains(&old_text) {
            // Moved along with an earlier row that shared its file.
            continue;
        }
        if !old.is_file() {
            s.missing += 1;
            continue;
        }
        let step = async {
            let bytes = tokio::fs::read(&old).await?;
            let sha1 = sha1_bytes(&bytes);
            let dup = sqlx::query("SELECT id FROM media WHERE content_sha1 = ? AND media_type = ? AND deleted = FALSE AND id <> ? ORDER BY id LIMIT 1").bind(&sha1).bind(&media_type).bind(media_id).fetch_optional(pool).await?;
            if let Some(d) = dup {
                merge_into(pool, media_id, d.try_get("id")?).await?;
                // Another row from before the layout may still point at the same file.
                if !still_used(pool, &old_text).await? {
                    let _ = tokio::fs::remove_file(&old).await;
                }
                return Ok::<bool, DatabaseError>(false);
            }
            let targets = linked_targets(pool, media_id).await?;
            let name_hint = source_url.clone().unwrap_or_else(|| old.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
            let (placement, _) = placement_for(pool, &targets, &media_type, sub.as_deref(), Some(&name_hint), &source, provider_id.as_deref()).await?;
            let ext = old.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_else(|| "jpg".to_string());
            let (root_role, root) = roots.write_root(placement.role);
            let rel = free_path(pool, root, root_role, &placement, &ext).await?;
            write_file(&join_rel(root, &rel), &bytes).await?;
            // Every row sharing the old file moves with it, or the next one finds it gone.
            sqlx::query("UPDATE media SET path = ?, root_role = ?, content_sha1 = ?, bytes = ? WHERE root_role IS NULL AND deleted = FALSE AND (id = ? OR path = ?)")
                .bind(&rel)
                .bind(root_role.name())
                .bind(&sha1)
                .bind(bytes.len() as u64)
                .bind(media_id)
                .bind(&old_text)
                .execute(pool)
                .await?;
            let _ = tokio::fs::remove_file(&old).await;
            Ok(true)
        };
        match step.await {
            Ok(true) => {
                s.moved += 1;
                moved_paths.insert(old_text.clone());
            }
            Ok(false) => s.merged += 1,
            Err(e) => {
                log::warn!("media_relayout: media {media_id} ({}): {e}", old.display());
                s.failed += 1;
            }
        }
    }
    rehome(pool, roots, progress, &mut s).await?;
    progress.force(s.candidates, Some(s.candidates), &format!("done: moved {}, merged {}, rehomed {}, skipped {}, missing {}, failed {}", s.moved, s.merged, s.rehomed, s.skipped_not_app, s.missing, s.failed)).await;
    Ok(s)
}

/// Moves every file the app laid out (`root_role` set) that is not where the layout puts it
/// now: to the first configured dir of its role (a file written to the app's media dir while
/// none was configured), and into its folder of the layout -- which is how files laid out as
/// `<Platform>/<Title>/...` reach `<source>/<provider id>/...`. Rows sharing a file move
/// together.
async fn rehome(pool: &MySqlPool, roots: &Roots, progress: &super::jobs::Progress, s: &mut RelayoutSummary) -> Result<(), DatabaseError> {
    let rows = sqlx::query("SELECT id, media_type, sub_category, path, root_role, source_url, source, provider_id FROM media WHERE root_role IS NOT NULL AND deleted = FALSE ORDER BY id").fetch_all(pool).await?;
    let mut seen = std::collections::HashSet::new();
    let done_before = s.candidates;
    s.candidates += rows.len() as u64;
    let total = Some(s.candidates);
    for (i, r) in rows.iter().enumerate() {
        progress.report(done_before + i as u64, total, &format!("rehomed {}, missing {}, failed {}", s.rehomed, s.missing, s.failed)).await;
        let media_id: u64 = r.try_get("id")?;
        let rel: String = r.try_get("path")?;
        let Some(role_now) = RootRole::from_name(&r.try_get::<String, _>("root_role")?) else { continue };
        if !seen.insert((role_now.name(), rel.clone())) {
            continue;
        }
        let step = async {
            let media_type: String = r.try_get("media_type")?;
            let sub: Option<String> = r.try_get("sub_category")?;
            let source: String = r.try_get::<Option<String>, _>("source")?.unwrap_or_else(|| "manual".to_string());
            let name_hint = r.try_get::<Option<String>, _>("source_url")?.filter(|u| !u.is_empty()).unwrap_or_else(|| rel.clone());
            let targets = linked_targets(pool, media_id).await?;
            if targets.is_empty() {
                return Ok(None);
            }
            let provider_id: Option<String> = r.try_get("provider_id")?;
            let (placement, _) = placement_for(pool, &targets, &media_type, sub.as_deref(), Some(&name_hint), &source, provider_id.as_deref()).await?;
            let (root_role, root) = roots.write_root(placement.role);
            let folder_now = rel.rsplit_once('/').map_or("", |(f, _)| f);
            if root_role == role_now && folder_now == placement.folder {
                return Ok::<Option<bool>, DatabaseError>(None);
            }
            let old = roots.resolve(Some(role_now.name()), &rel);
            if !old.is_file() {
                return Ok(Some(false));
            }
            if folder_now == placement.folder && join_rel(root, &rel) == old {
                // The configured dir is the app's media dir itself: only the role changes.
                sqlx::query("UPDATE media SET root_role = ? WHERE root_role = ? AND path = ?").bind(root_role.name()).bind(role_now.name()).bind(&rel).execute(pool).await?;
                return Ok(None);
            }
            let ext = old.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_else(|| "jpg".to_string());
            let plain = plain_rel(&placement, &ext);
            let new_rel = if path_taken(pool, root, root_role, &plain).await? { free_path(pool, root, root_role, &placement, &ext).await? } else { plain };
            move_file(&old, &join_rel(root, &new_rel)).await?;
            sqlx::query("UPDATE media SET root_role = ?, path = ? WHERE root_role = ? AND path = ?").bind(root_role.name()).bind(&new_rel).bind(role_now.name()).bind(&rel).execute(pool).await?;
            Ok(Some(true))
        };
        match step.await {
            Ok(None) => {}
            Ok(Some(true)) => s.rehomed += 1,
            Ok(Some(false)) => s.missing += 1,
            Err(e) => {
                log::warn!("media_relayout: media {media_id} ({rel}): {e}");
                s.failed += 1;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn components_are_portable() {
        assert_eq!(sanitize_component("Hogehoge Adventure 2: Battle"), "Hogehoge Adventure 2_ Battle");
        assert_eq!(sanitize_component("What?*<>|\"/\\"), "What________");
        assert_eq!(sanitize_component("Trailing dots... "), "Trailing dots");
        assert_eq!(sanitize_component("CON"), "_CON");
        assert_eq!(sanitize_component("com1.txt"), "_com1.txt");
        assert_eq!(sanitize_component("COMMAND"), "COMMAND");
        assert_eq!(sanitize_component(""), "_");
        assert_eq!(sanitize_component(".."), "_");
        assert_eq!(sanitize_component("a\tb\nc"), "a_b_c");
        assert_eq!(sanitize_component(&"x".repeat(300)).len(), 100);
        assert_eq!(sanitize_component("サンプルクエスト"), "サンプルクエスト");
    }

    #[test]
    fn folders_follow_the_layout() {
        assert_eq!(region_folder("US", false), "US");
        assert_eq!(region_folder("US,EU", true), "US,EU/_prerelease");
        assert_eq!(region_folder("", false), "common");
        // A provider's art: by its id for the game, whichever title or platform asked.
        assert_eq!(layout_folder("igdb", Some("1234"), ("title", 7), None, None), "igdb/1234/common");
        assert_eq!(layout_folder("igdb", Some("1234"), ("release", 9), None, None), "igdb/1234/common", "the same folder from the game's other platform");
        assert_eq!(layout_folder("screenscraper", Some("55"), ("release", 9), Some("US"), None), "screenscraper/55/US");
        assert_eq!(layout_folder("igdb", Some("1234"), ("title", 7), None, Some("screenshots")), "igdb/1234/common/screenshots");
        // The user's own: by what it was uploaded to.
        assert_eq!(layout_folder("manual", None, ("release", 9), Some("US"), None), "release/9");
        assert_eq!(layout_folder("manual", None, ("title", 7), None, Some("fanart")), "title/7/fanart");
        assert_eq!(layout_folder("manual", Some("ignored"), ("franchise", 3), None, None), "franchise/3");
        // A provider's art that came without an id cannot share a folder with anything.
        assert_eq!(layout_folder("igdb", None, ("title", 7), None, None), "igdb/_title/7/common");
        assert_eq!(layout_folder("igdb", Some(" "), ("release", 9), Some("JP"), None), "igdb/_release/9/JP");
    }

    #[test]
    fn sources_get_their_own_folder() {
        assert_eq!(source_folder("igdb"), "igdb");
        assert_eq!(source_folder("manual"), "user");
        assert_eq!(list_folder("screenshot"), "screenshots");
        assert_eq!(list_folder("fanart"), "fanart");
        assert_eq!(original_stem("https://images.igdb.com/igdb/image/upload/t_screenshot_huge/sc6abc.jpg?x=1").as_deref(), Some("sc6abc"));
        assert_eq!(original_stem("C:\\shots\\my shot.png").as_deref(), Some("my shot"));
        assert_eq!(original_stem("https://x/"), None);
        let p = Placement { role: RootRole::Image, folder: "igdb/1234/US".into(), stem: "box_front".into(), keep_name: false };
        assert_eq!(plain_rel(&p, "jpg"), "igdb/1234/US/box_front.jpg");
    }

    #[test]
    fn names_and_collisions() {
        assert_eq!(slot_stem("box", Some("front")), "box_front");
        assert_eq!(candidate_name("box_front", "jpg", 1, false), "box_front.jpg");
        assert_eq!(candidate_name("box_front", "jpg", 2, false), "box_front_2.jpg");
        assert_eq!(candidate_name("Green Hill Zone", "mp3", 2, true), "Green Hill Zone (2).mp3");
    }

    #[test]
    fn relative_paths_round_trip() {
        let root = Path::new("media_root");
        let p = join_rel(root, "Dreamcast/Hogehoge Adventure/USA/box_front.jpg");
        assert_eq!(p, root.join("Dreamcast").join("Hogehoge Adventure").join("USA").join("box_front.jpg"));
        let roots = Roots { app: PathBuf::from("app"), image: vec![PathBuf::from("img1"), PathBuf::from("img2")], video: vec![], audio: vec![], tier1: None, tier2: None, tier0: PathBuf::from("app/_cache") };
        assert_eq!(roots.write_root(RootRole::Image), (RootRole::Image, Path::new("img1")));
        assert_eq!(roots.write_root(RootRole::Video), (RootRole::App, Path::new("app")));
        assert_eq!(roots.resolve(Some("image"), "a/b.jpg"), PathBuf::from("img1").join("a").join("b.jpg"));
        assert_eq!(roots.resolve(None, "/abs/x.jpg"), PathBuf::from("/abs/x.jpg"));
    }

    #[test]
    fn extensions() {
        assert_eq!(extension_for("https://x/y/cover.PNG?size=big", None), "png");
        assert_eq!(extension_for("https://x/y/t_cover_big/abc.jpg", None), "jpg");
        assert_eq!(extension_for("https://x/y/image", Some("image/webp")), "webp");
        assert_eq!(extension_for("track.flac", None), "flac");
        assert!(ext_allowed(RootRole::Image, "png") && !ext_allowed(RootRole::Image, "mp4") && ext_allowed(RootRole::Audio, "ogg"));
    }
}

#[cfg(test)]
mod reuse_tests {
    use super::*;

    #[test]
    fn known_extension_only_answers_for_a_name_that_spells_one() {
        assert_eq!(known_extension("https://example.invalid/a/b/box.PNG?x=1"), Some("png"));
        assert_eq!(known_extension("https://example.invalid/t_cover_big/abc.jpeg"), Some("jpg"));
        assert_eq!(known_extension("https://example.invalid/image/12345"), None);
        // extension_for still falls back to the Content-Type, then jpg.
        assert_eq!(extension_for("https://example.invalid/image/12345", Some("image/webp")), "webp");
        assert_eq!(extension_for("https://example.invalid/image/12345", None), "jpg");
    }

    #[test]
    fn looks_like_tells_pictures_from_leftovers() {
        assert!(looks_like("png", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"));
        assert!(looks_like("jpg", &[0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10]));
        assert!(looks_like("webp", b"RIFF\0\0\0\0WEBPVP8 "));
        assert!(looks_like("mp4", b"\0\0\0\x18ftypmp42"));
        assert!(!looks_like("jpg", b"<!DOCTYPE html><html>"), "an error page saved as a picture");
        assert!(!looks_like("png", b""), "an empty file");
        assert!(!looks_like("png", &[0xFF, 0xD8, 0xFF]), "a jpeg under a png name");
    }

    #[test]
    fn every_role_extension_is_allowed_for_its_role() {
        for role in [RootRole::Image, RootRole::Video, RootRole::Audio] {
            for ext in role_extensions(role) {
                assert!(ext_allowed(role, ext), "{ext} for {}", role.name());
            }
        }
    }
}
