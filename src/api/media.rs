//! Media for clients: serving a file (full size through the cache tiers, or a grid
//! thumbnail) and the user's own edits of a title's / release's media slots -- set from a
//! URL, upload a local file, or remove. A user pick is stored as `source = 'manual'` and a
//! provider sync never adds to or replaces a slot that holds one.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::database::media_cache::{self, ThumbFormat};
use crate::database::media_store::{self, MediaTarget, NewMedia, RootRole, Roots};
use crate::database::settings;

use super::{App, Error, Result};

/// Largest file accepted from a URL or an upload.
pub const MAX_MEDIA_BYTES: usize = 512 * 1024 * 1024;

async fn roots(app: &App) -> Result<Roots> {
    Ok(Roots::load(&app.pool, &app.paths.media_dir()).await?)
}

/// Whether to ask "use the media files already in this folder?" (`media_reuse_existing`):
/// never answered, no media on record yet, and the media folder(s) already hold files --
/// an earlier install's art under a new database. Both front ends ask once at first run and
/// answer by saving the setting.
#[derive(Debug, Serialize)]
pub struct ReusePrompt {
    pub ask: bool,
}

pub async fn reuse_prompt(app: &App) -> Result<ReusePrompt> {
    if settings::get::<bool>(&app.pool, settings::KEY_MEDIA_REUSE_EXISTING).await?.is_some() {
        return Ok(ReusePrompt { ask: false });
    }
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media").fetch_one(&app.pool).await?;
    if rows > 0 {
        return Ok(ReusePrompt { ask: false });
    }
    let roots = roots(app).await?;
    let ask = tokio::task::spawn_blocking(move || media_store::roots_hold_files(&roots)).await.map_err(|e| Error::internal(e.to_string()))?;
    Ok(ReusePrompt { ask })
}

/// The file to stream for media `id`: full size (through the cache tiers), or with
/// `thumb` a grid thumbnail in `format` (default: the `media_thumb_format` setting). A
/// source that can't be thumbnailed (video, SVG, ...) is served full size.
pub async fn file(app: &App, id: u64, thumb: bool, format: Option<&str>) -> Result<(PathBuf, &'static str)> {
    let row = sqlx::query("SELECT media_type, path, root_role FROM media WHERE id = ? AND deleted = FALSE").bind(id).fetch_optional(&app.pool).await?.ok_or_else(|| Error::not_found(format!("no media {id}")))?;
    let roots = roots(app).await?;
    let media_type: String = row.try_get("media_type")?;
    let original = roots.resolve(row.try_get::<Option<String>, _>("root_role")?.as_deref(), &row.try_get::<String, _>("path")?);
    if !original.is_file() {
        // The original is only the deepest tier: a cache copy still serves (full size, even
        // for a thumbnail request -- a thumbnail cannot be made without the source).
        if let Some(path) = media_cache::cached_full(&app.pool, &roots, id).await? {
            // And it goes back into the media dir while it is at hand: a cache tier is
            // evicted, so art held only there is art about to be lost. Best effort -- the
            // view is served either way.
            match tokio::fs::read(&path).await {
                Ok(bytes) => {
                    if let Err(e) = media_store::write_file(&original, &bytes).await {
                        log::warn!("media {id}: could not put {} back from its cache copy: {e}", original.display());
                    }
                }
                Err(e) => log::warn!("media {id}: cache copy {} unreadable: {e}", path.display()),
            }
            let mime = media_store::mime_for(&path);
            return Ok((path, mime));
        }
        return Err(Error::not_found(format!("media {id} file is missing on disk")));
    }
    let caps = settings::media_cache_caps(&app.pool).await?;
    if thumb && RootRole::for_media_type(&media_type) == RootRole::Image && !matches!(original.extension().and_then(|e| e.to_str()), Some("svg")) {
        let (px, quality, default_format) = settings::media_thumb(&app.pool).await?;
        let format = match format {
            Some(f) => ThumbFormat::from_name(f).ok_or_else(|| Error::invalid("format must be avif|jpeg|webp|jxl"))?,
            None => ThumbFormat::from_name(&default_format).unwrap_or(ThumbFormat::Avif),
        };
        if let Some(path) = media_cache::serve_thumb(&app.pool, &roots, caps, id, &original, px, format, quality).await? {
            let mime = media_store::mime_for(&path);
            return Ok((path, mime));
        }
    }
    let path = media_cache::serve_full(&app.pool, &roots, caps, id, &original).await?;
    let mime = media_store::mime_for(&path);
    Ok((path, mime))
}

/// A URL this server may fetch on a client's behalf: http(s) to a named host, never an
/// address literal or `localhost`, so a signed-in account cannot use the server to reach into
/// the server's own network. `allow_loopback` is for tests, which stand a provider in on
/// 127.0.0.1.
pub fn provider_url_allowed(url: &str, allow_loopback: bool) -> Result<()> {
    let parsed = reqwest::Url::parse(url).map_err(|_| Error::invalid("url must be an absolute http(s) URL"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(Error::invalid("url must be http or https"));
    }
    let host = parsed.host_str().ok_or_else(|| Error::invalid("url has no host"))?;
    let literal = host.trim_start_matches('[').trim_end_matches(']').parse::<std::net::IpAddr>().ok();
    let loopback = literal.is_some_and(|ip| ip.is_loopback()) || host.eq_ignore_ascii_case("localhost");
    if loopback && allow_loopback {
        return Ok(());
    }
    if literal.is_some() || loopback {
        return Err(Error::invalid("url must name a provider's host, not an address"));
    }
    Ok(())
}

/// The other half of [`provider_url_allowed`]: a named host must not resolve into this
/// machine or its network either (a public name pointed at 10.0.0.1). Every address it
/// resolves to is checked, not only the first.
async fn host_resolves_outside(url: &str, allow_loopback: bool) -> Result<()> {
    let parsed = reqwest::Url::parse(url).map_err(|_| Error::invalid("url must be an absolute http(s) URL"))?;
    let host = parsed.host_str().ok_or_else(|| Error::invalid("url has no host"))?.to_string();
    let port = parsed.port_or_known_default().unwrap_or(443);
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host.as_str(), port)).await.map_err(|e| Error::unavailable(format!("{host}: {e}")))?.collect();
    if addrs.is_empty() {
        return Err(Error::unavailable(format!("{host} did not resolve")));
    }
    let all_loopback = addrs.iter().all(|a| a.ip().is_loopback());
    if allow_loopback && all_loopback {
        return Ok(());
    }
    if addrs.iter().any(|a| crate::database::providers::is_internal_ip(a.ip())) {
        return Err(Error::invalid(format!("{host} resolves to an address on a local network")));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefetchResult {
    pub media_id: u64,
    pub bytes: u64,
    /// `cache`: the original was rebuilt from a cached copy; `provider`: it was downloaded
    /// again from the row's own URL; `present`: it was there all along.
    pub from: String,
}

/// Makes sure the file behind row `id` exists on disk: from a cached copy if any tier has
/// one, else downloaded again from the URL it was fetched from. The server as a cache, seen
/// from a client: a miss at the deepest tier is filled from the provider, not reported.
pub async fn refetch(app: &App, id: u64) -> Result<RefetchResult> {
    let row = sqlx::query("SELECT path, root_role, source_url FROM media WHERE id = ? AND deleted = FALSE").bind(id).fetch_optional(&app.pool).await?.ok_or_else(|| Error::not_found(format!("no media {id}")))?;
    let roots = roots(app).await?;
    let original = roots.resolve(row.try_get::<Option<String>, _>("root_role")?.as_deref(), &row.try_get::<String, _>("path")?);
    if original.is_file() {
        let bytes = std::fs::metadata(&original).map(|m| m.len()).unwrap_or(0);
        return Ok(RefetchResult { media_id: id, bytes, from: "present".to_string() });
    }
    if let Some(cached) = media_cache::cached_full(&app.pool, &roots, id).await? {
        let bytes = tokio::fs::read(&cached).await.map_err(|e| Error::internal(format!("read {}: {e}", cached.display())))?;
        media_store::write_file(&original, &bytes).await?;
        return Ok(RefetchResult { media_id: id, bytes: bytes.len() as u64, from: "cache".to_string() });
    }
    let url: String = row.try_get::<Option<String>, _>("source_url")?.filter(|u| !u.is_empty()).ok_or_else(|| Error::invalid(format!("media {id} has no source URL to fetch again from (a manual upload)")))?;
    // Stored by this server itself, so wherever it points is where this server chose to go.
    provider_url_allowed(&url, true)?;
    let response = app.http.get(&url).send().await.and_then(|r| r.error_for_status()).map_err(|e| Error::unavailable(format!("fetching {url} again: {e}")))?;
    let bytes = response.bytes().await.map_err(|e| Error::unavailable(format!("fetching {url} again: {e}")))?;
    media_store::write_file(&original, &bytes).await?;
    sqlx::query("UPDATE media SET content_sha1 = ?, bytes = ? WHERE id = ?").bind(media_store::sha1_bytes(&bytes)).bind(bytes.len() as u64).bind(id).execute(&app.pool).await?;
    // Whatever the tiers held was made from a file that is gone; the bytes may differ.
    media_cache::forget(&app.pool, &roots, id).await?;
    Ok(RefetchResult { media_id: id, bytes: bytes.len() as u64, from: "provider".to_string() })
}

/// What a client asks this server to fetch into its cache on the client's behalf.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FetchRequest {
    pub provider: String,
    #[serde(default)]
    pub provider_id: Option<String>,
    pub url: String,
    pub media_type: String,
    #[serde(default)]
    pub sub_category: Option<String>,
    #[serde(default)]
    pub title_id: Option<u64>,
    #[serde(default)]
    pub release_id: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchResult {
    /// The row to `GET /media/{id}` for the bytes.
    pub media_id: u64,
    pub bytes: u64,
    /// Whether the server had to go to the provider for it.
    pub fetched: bool,
}

/// Fetches a provider's art into this server for `req`'s title or release -- unless a row for
/// that URL already exists, in which case it is linked and, if its file has gone, restored
/// ([`refetch`]). What a client calls instead of a provider, so that the server holds every
/// piece of art the household has and the provider is asked once for it.
pub async fn fetch_for(app: &App, req: &FetchRequest, allow_loopback: bool) -> Result<FetchResult> {
    let provider = crate::database::providers::Provider::from_name(&req.provider).ok_or_else(|| Error::invalid(format!("unknown provider {:?}", req.provider)))?;
    let sub = req.sub_category.as_deref().filter(|s| !s.is_empty());
    validate_slot(&req.media_type, sub)?;
    let target = SlotTarget { title_id: req.title_id, release_id: req.release_id }.target()?;
    require_target(app, target).await?;
    provider_url_allowed(&req.url, allow_loopback)?;
    host_resolves_outside(&req.url, allow_loopback).await?;
    let targets = with_group(app, target).await?;
    let (record, _) = crate::database::providers::media::store_url(&app.http, &app.pool, &app.paths.media_dir(), provider.name(), req.provider_id.as_deref(), &req.url, &req.media_type, sub, &targets).await?;
    if record.path.is_file() {
        return Ok(FetchResult { media_id: record.media_id, bytes: record.bytes, fetched: record.fresh });
    }
    let restored = refetch(app, record.media_id).await?;
    Ok(FetchResult { media_id: record.media_id, bytes: restored.bytes, fetched: restored.from == "provider" })
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SlotTarget {
    pub title_id: Option<u64>,
    pub release_id: Option<u64>,
}

impl SlotTarget {
    fn target(&self) -> Result<MediaTarget> {
        match (self.title_id, self.release_id) {
            (Some(t), None) => Ok(MediaTarget::Title(t)),
            (None, Some(r)) => Ok(MediaTarget::Release(r)),
            _ => Err(Error::invalid("give exactly one of title_id / release_id")),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManualUrl {
    #[serde(flatten)]
    pub target: SlotTarget,
    pub media_type: String,
    pub sub_category: Option<String>,
    pub url: String,
    /// What to do when the slot's file name is already taken; unset, the request is refused
    /// with a conflict naming the file so the user can choose.
    #[serde(default)]
    pub on_conflict: Option<OnConflict>,
}

/// The user's answer to "that file already exists".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnConflict {
    Overwrite,
    Rename,
}

impl OnConflict {
    pub fn parse(s: &str) -> Result<Option<Self>> {
        match s {
            "" => Ok(None),
            "overwrite" => Ok(Some(Self::Overwrite)),
            "rename" => Ok(Some(Self::Rename)),
            other => Err(Error::invalid(format!("on_conflict must be overwrite or rename, not {other:?}"))),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ManualResult {
    pub media_id: u64,
    /// Releases the file is now linked to (the target's revisions share it).
    pub linked_releases: Vec<u64>,
    /// Previous items of the slot unlinked from the target.
    pub replaced: u32,
}

fn validate_slot(media_type: &str, sub_category: Option<&str>) -> Result<()> {
    if !super::settings::MEDIA_TYPES.contains(&media_type) {
        return Err(Error::invalid(format!("media_type must be one of {}", super::settings::MEDIA_TYPES.join("|"))));
    }
    if let Some(sub) = sub_category {
        if !media_store::SUB_CATEGORIES.contains(&sub) {
            return Err(Error::invalid(format!("sub_category must be one of {}", media_store::SUB_CATEGORIES.join("|"))));
        }
    }
    Ok(())
}

async fn require_target(app: &App, target: MediaTarget) -> Result<()> {
    let (table, id) = match target {
        MediaTarget::Title(id) => ("title", id),
        MediaTarget::Release(id) => ("`release`", id),
        _ => return Err(Error::invalid("only titles and releases have editable media slots")),
    };
    let sql = format!("SELECT 1 FROM {table} WHERE id = ? AND deleted = FALSE");
    sqlx::query(sqlx::AssertSqlSafe(sql)).bind(id).fetch_optional(&app.pool).await?.ok_or_else(|| Error::not_found(format!("no {} {id}", table.trim_matches('`'))))?;
    Ok(())
}

/// The target plus, for a release, every release of its group (its revisions).
async fn with_group(app: &App, target: MediaTarget) -> Result<Vec<MediaTarget>> {
    Ok(match target {
        MediaTarget::Release(id) => match media_store::release_group(&app.pool, id).await? {
            Some(g) => g.members.into_iter().map(MediaTarget::Release).collect(),
            None => vec![target],
        },
        other => vec![other],
    })
}

#[allow(clippy::too_many_arguments)]
async fn store_manual(app: &App, target: MediaTarget, media_type: &str, sub_category: Option<&str>, bytes: &[u8], ext: &str, source_url: Option<&str>, original_name: Option<&str>, on_conflict: Option<OnConflict>) -> Result<ManualResult> {
    let pool = &app.pool;
    let roots = roots(app).await?;
    let (placement, group) = media_store::placement_for(pool, &[target], media_type, sub_category, original_name.or(source_url), "manual", None).await?;
    if !media_store::ext_allowed(placement.role, ext) {
        return Err(Error::invalid(format!("a .{ext} file can't be stored as {media_type}")));
    }
    // Asked before anything is unlinked: a taken name is the user's call, never silently
    // overwritten or renamed. Identical bytes are only linked again, so they never conflict.
    let on_existing = match on_conflict {
        Some(OnConflict::Overwrite) => media_store::OnExisting::Overwrite,
        Some(OnConflict::Rename) => media_store::OnExisting::Rename,
        None => {
            let (root_role, root) = roots.write_root(placement.role);
            let plain = media_store::plain_rel(&placement, ext);
            if !media_store::has_bytes(pool, bytes, media_type).await? && media_store::path_taken(pool, root, root_role, &plain).await? {
                return Err(Error::conflict(format!("{plain} already exists")));
            }
            media_store::OnExisting::Rename
        }
    };
    // Replace: whatever the slot held on the target (and its revisions) is unlinked;
    // screenshots / videos / fanart / audio are lists, so they're added to instead.
    let mut replaced = 0;
    if !media_store::TITLE_LEVEL_TYPES.contains(&media_type) && placement.role != RootRole::Audio {
        for t in with_group(app, target).await? {
            for (id, _) in media_store::slot_media(pool, t, media_type, sub_category, None).await? {
                if media_store::unlink(pool, id, t).await? {
                    replaced += 1;
                }
                media_store::retire_if_unlinked(pool, id).await?;
            }
        }
    }
    let meta = NewMedia { media_type, sub_category, source: "manual", provider_id: None, source_url, ext, own_row: true, on_existing };
    let stored = media_store::store_bytes(pool, &roots, &placement, &meta, bytes).await?;
    media_store::link_placed(pool, stored.media_id, &[target], group.as_ref(), media_type, sub_category).await?;
    let linked_releases = sqlx::query("SELECT release_id FROM release_media WHERE media_id = ? ORDER BY release_id").bind(stored.media_id).fetch_all(pool).await?.iter().filter_map(|r| r.try_get("release_id").ok()).collect();
    Ok(ManualResult { media_id: stored.media_id, linked_releases, replaced })
}

/// Sets a slot from a URL: the server downloads it into the media layout.
pub async fn set_from_url(app: &App, req: &ManualUrl) -> Result<ManualResult> {
    let target = req.target.target()?;
    let sub = req.sub_category.as_deref().filter(|s| !s.is_empty());
    validate_slot(&req.media_type, sub)?;
    require_target(app, target).await?;
    let url = req.url.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(Error::invalid("url must be http(s)"));
    }
    let response = app.http.get(url).send().await.and_then(|r| r.error_for_status()).map_err(|e| Error::unavailable(format!("download failed: {e}")))?;
    if response.content_length().is_some_and(|n| n as usize > MAX_MEDIA_BYTES) {
        return Err(Error::invalid(format!("file is larger than {} MiB", MAX_MEDIA_BYTES >> 20)));
    }
    let content_type = response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_string);
    let bytes = response.bytes().await.map_err(|e| Error::unavailable(format!("download failed: {e}")))?;
    if bytes.len() > MAX_MEDIA_BYTES {
        return Err(Error::invalid(format!("file is larger than {} MiB", MAX_MEDIA_BYTES >> 20)));
    }
    if content_type.as_deref().is_some_and(|ct| ct.starts_with("text/html")) {
        return Err(Error::invalid("that URL is a web page, not an image/video/audio file (use the file's own URL)"));
    }
    let ext = media_store::extension_for(url, content_type.as_deref());
    store_manual(app, target, &req.media_type, sub, &bytes, ext, Some(url), None, req.on_conflict).await
}

/// Sets a slot from an uploaded file (copied into the media layout; the upload itself is
/// whatever the client sent -- the user's original stays where it was).
pub async fn upload(app: &App, target: &SlotTarget, media_type: &str, sub_category: Option<&str>, file_name: &str, bytes: &[u8], on_conflict: Option<OnConflict>) -> Result<ManualResult> {
    let target = target.target()?;
    let sub = sub_category.filter(|s| !s.is_empty());
    validate_slot(media_type, sub)?;
    require_target(app, target).await?;
    if bytes.is_empty() {
        return Err(Error::invalid("empty file"));
    }
    let ext = upload_ext(media_type, file_name, bytes)?;
    store_manual(app, target, media_type, sub, bytes, ext, None, Some(file_name), on_conflict).await
}

/// The stored extension of an upload: its own extension, which must suit the slot's role,
/// and for raster images the bytes must actually be that kind of image.
fn upload_ext(media_type: &str, file_name: &str, bytes: &[u8]) -> Result<&'static str> {
    let named = file_name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    let ext = media_store::extension_for(&format!(".{named}"), None);
    let role = RootRole::for_media_type(media_type);
    if named.is_empty() || !(ext == named || (ext == "jpg" && named == "jpeg")) || !media_store::ext_allowed(role, ext) {
        return Err(Error::invalid(format!("a .{named} file can't be stored as {media_type}")));
    }
    if role == RootRole::Image && ext != "svg" {
        let sniffed = image::guess_format(bytes).ok().map(|f| f.extensions_str().first().copied().unwrap_or(""));
        let same = match sniffed {
            Some("jpg") | Some("jpeg") => ext == "jpg",
            Some(s) => s == ext,
            None => false,
        };
        if !same {
            return Err(Error::invalid(format!("the file's content isn't a .{ext} image")));
        }
    }
    Ok(ext)
}

#[derive(Debug, Clone, Serialize)]
pub struct RemoveResult {
    pub unlinked: u32,
    /// The row is no longer linked anywhere and was retired (its file stays on disk).
    pub retired: bool,
}

/// Unlinks media `id` from a title, or from a release, its revisions and its group's
/// demo/proto/beta dependents. The row is
/// retired once nothing links it; no file is deleted.
pub async fn remove(app: &App, id: u64, target: &SlotTarget) -> Result<RemoveResult> {
    let target = target.target()?;
    let mut targets = with_group(app, target).await?;
    // Demo/proto/beta releases that got the retail group's art lose it along with the group.
    if let MediaTarget::Release(id) = target {
        if let Some(g) = media_store::release_group(&app.pool, id).await? {
            targets.extend(g.dependents.into_iter().map(MediaTarget::Release));
        }
    }
    let mut unlinked = 0;
    for t in targets {
        if media_store::unlink(&app.pool, id, t).await? {
            unlinked += 1;
        }
    }
    if unlinked == 0 {
        return Err(Error::not_found(format!("media {id} is not linked there")));
    }
    let retired = media_store::retire_if_unlinked(&app.pool, id).await?;
    Ok(RemoveResult { unlinked, retired })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_server_fetches_from_named_hosts_only() {
        assert!(provider_url_allowed("https://images.igdb.com/igdb/image/upload/t_cover_big/co1.jpg", false).is_ok());
        assert!(provider_url_allowed("http://media.retroachievements.org/Images/1.png", false).is_ok());
        assert!(provider_url_allowed("ftp://images.igdb.com/x", false).is_err());
        assert!(provider_url_allowed("/relative.png", false).is_err());
        // Not a way into the server's own network.
        assert!(provider_url_allowed("http://192.168.1.1/admin", false).is_err());
        assert!(provider_url_allowed("http://10.0.0.5:8080/x.png", true).is_err(), "loopback is the only literal a test may allow");
        assert!(provider_url_allowed("http://[::1]/x.png", false).is_err());
        assert!(provider_url_allowed("http://127.0.0.1:9/x.png", false).is_err());
        assert!(provider_url_allowed("http://localhost/x.png", false).is_err());
        // Tests stand a provider in on loopback.
        assert!(provider_url_allowed("http://127.0.0.1:9/x.png", true).is_ok());
        assert!(provider_url_allowed("http://localhost:9/x.png", true).is_ok());
        use crate::database::providers::{is_internal_ip, names_internal_host};
        for ip in ["10.1.2.3", "172.16.0.1", "192.168.0.1", "127.0.0.1", "169.254.169.254", "100.64.0.1", "0.0.0.0", "::1", "fd00::1", "fe80::1", "::ffff:10.0.0.1"] {
            assert!(is_internal_ip(ip.parse().unwrap()), "{ip}");
        }
        assert!(!is_internal_ip("8.8.8.8".parse().unwrap()));
        assert!(names_internal_host(&reqwest::Url::parse("http://[::ffff:127.0.0.1]/").unwrap()));
        assert!(!names_internal_host(&reqwest::Url::parse("https://images.igdb.com/x").unwrap()));
    }

    #[test]
    fn slot_target_needs_exactly_one() {
        assert!(SlotTarget { title_id: Some(1), release_id: None }.target().is_ok());
        assert!(SlotTarget { title_id: None, release_id: Some(2) }.target().is_ok());
        assert!(SlotTarget { title_id: Some(1), release_id: Some(2) }.target().is_err());
        assert!(SlotTarget::default().target().is_err());
    }

    #[test]
    fn uploads_need_a_matching_extension() {
        let png = {
            let mut out = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(4, 4).write_to(&mut out, image::ImageFormat::Png).unwrap();
            out.into_inner()
        };
        assert_eq!(upload_ext("box", "Back Cover.PNG", &png).unwrap(), "png");
        assert!(upload_ext("box", "evil.exe", &png).is_err(), "unknown extension");
        assert!(upload_ext("box", "noext", &png).is_err());
        assert!(upload_ext("box", "fake.jpg", &png).is_err(), "content isn't a jpeg");
        assert!(upload_ext("video", "clip.png", &png).is_err(), "wrong role");
        assert_eq!(upload_ext("bgm", "Green Hill.flac", b"fLaC....").unwrap(), "flac");
    }

    #[test]
    fn slots_are_validated() {
        assert!(validate_slot("box", Some("front")).is_ok());
        assert!(validate_slot("screenshot", None).is_ok());
        assert!(validate_slot("poster", None).is_err());
        assert!(validate_slot("box", Some("inside")).is_err());
    }
}

// ---------------------------------------------------------------------------
// what the media settings can offer, and the strip a title / release page shows
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ProviderMedia {
    pub name: &'static str,
    /// The media types this provider's sync can download.
    pub types: Vec<&'static str>,
    /// What "download art" can ask it for, as `type` or `type:sub` (`box:front`).
    pub slots: Vec<String>,
    /// Its art is fetched only when asked for (TheGamesDB's allowance is small).
    pub manual_only: bool,
}

/// What the media settings screens draw from, so both UIs offer exactly what the code does.
#[derive(Debug, Clone, Serialize)]
pub struct MediaOptions {
    pub types: Vec<&'static str>,
    /// One per game: a provider order decides whose is used.
    pub single_types: Vec<&'static str>,
    pub providers: Vec<ProviderMedia>,
    /// The types a sync downloads when `media_download` does not say; the rest are opt-in.
    pub default_on: Vec<&'static str>,
    /// Export artwork: each slot's provider order when `media_export_priority` does not say,
    /// and the media types each slot is filled from.
    pub export_priority: serde_json::Value,
    pub export_slots: serde_json::Value,
}

/// `(box, front)` -> `box:front`, as "download art" asks for it.
fn slot_names(slots: &[(&str, Option<&str>)]) -> Vec<String> {
    slots.iter().map(|(t, sub)| sub.map_or_else(|| t.to_string(), |s| format!("{t}:{s}"))).collect()
}

/// Part of the vocabulary (`GET /api/v1/vocabulary`, key `media`).
pub fn options() -> MediaOptions {
    MediaOptions {
        types: super::settings::MEDIA_TYPES.to_vec(),
        single_types: settings::SINGLE_MEDIA_TYPES.to_vec(),
        // The serial sources first: where they have a cover it is the release's own box, so a
        // new order puts them on top; they have nothing for any other platform.
        providers: settings::SERIAL_ART_SOURCES
            .iter()
            .map(|(name, types)| ProviderMedia { name, types: types.to_vec(), slots: slot_names(&crate::database::providers::xlenore::SLOTS), manual_only: false })
            .chain(settings::MEDIA_CAPABILITIES.iter().map(|(provider, types)| ProviderMedia {
                name: provider.name(),
                types: types.to_vec(),
                slots: slot_names(crate::database::providers::media::provider_media(*provider)),
                manual_only: provider.media_manual_only(),
            }))
            .collect(),
        default_on: settings::MEDIA_DEFAULT_ON.to_vec(),
        export_priority: super::artwork::default_priority_json(),
        export_slots: super::artwork::slot_types_json(),
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct StripQuery {
    pub title_id: Option<u64>,
    pub release_id: Option<u64>,
}

/// One thing in the strip.
#[derive(Debug, Clone, Serialize)]
pub struct StripItem {
    /// The media id: this machine's, or the main server's when `remote`.
    pub id: u64,
    pub media_type: String,
    pub sub_category: Option<String>,
    pub source: Option<String>,
    /// image | video | audio, from the file's extension.
    pub kind: &'static str,
    /// Not kept here: streamed from the main server.
    pub remote: bool,
    /// The route that serves it.
    pub file: String,
}

fn kind_of(path: &str) -> &'static str {
    let mime = media_store::mime_for(std::path::Path::new(path));
    if mime.starts_with("video/") {
        "video"
    } else if mime.starts_with("audio/") {
        "audio"
    } else {
        "image"
    }
}

/// What a title or release page shows in its media strip: every item of a type `media_show`
/// allows, in media-type order; within a type a user's own pick first (unless `media_priority`
/// places it), then art in a release's own region folder before `common/` art, each by
/// `media_priority` -- the order the page's cover is picked in (`catalog::media_order`). A
/// user's pick counts as region art wherever it is filed.
///
/// A title shows its own media and that of its releases, each file once -- provider art is
/// attached to releases, so the title's own list is usually empty. On a client, what the
/// main server holds and this machine does not is included as `remote` and streamed.
pub async fn strip(app: &App, q: &StripQuery) -> Result<Vec<StripItem>> {
    let (local_scope, remote_scope, id) = match (q.title_id, q.release_id) {
        (Some(id), _) => (
            "(m.id IN (SELECT media_id FROM title_media WHERE title_id = ?) OR m.id IN (SELECT rm.media_id FROM release_media rm JOIN title_releases tr ON tr.release_id = rm.release_id JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE WHERE tr.title_id = ?))",
            "(m.id IN (SELECT media_id FROM remote_title_media WHERE source_id = m.source_id AND title_id = ?) OR m.id IN (SELECT rrm.media_id FROM remote_release_media rrm JOIN title_releases tr ON tr.release_id = rrm.release_id WHERE rrm.source_id = m.source_id AND tr.title_id = ?))",
            id,
        ),
        (None, Some(id)) => (
            "(m.id IN (SELECT media_id FROM release_media WHERE release_id = ?) OR 0 = ?)",
            "(m.id IN (SELECT media_id FROM remote_release_media WHERE source_id = m.source_id AND release_id = ?) OR 0 = ?)",
            id,
        ),
        (None, None) => return Err(Error::invalid("the media strip needs a title_id or a release_id")),
    };
    let rules = settings::MediaRules::load(&app.pool).await?;

    struct Found {
        item: StripItem,
        sha1: Option<String>,
        url: Option<String>,
        /// Not under `common/`, or a user's pick.
        regional: bool,
    }
    let read = |rows: Vec<sqlx::mysql::MySqlRow>, remote: bool| -> Result<Vec<Found>> {
        rows.iter()
            .map(|r| {
                let id: u64 = r.try_get("id")?;
                let path: String = r.try_get("path")?;
                let source: Option<String> = r.try_get("source")?;
                let regional = settings::is_region_art(source.as_deref(), &path);
                Ok(Found {
                    item: StripItem {
                        id,
                        media_type: r.try_get("media_type")?,
                        sub_category: r.try_get("sub_category")?,
                        source,
                        kind: kind_of(&path),
                        remote,
                        file: if remote { format!("/api/v1/media/remote/{id}") } else { format!("/api/v1/media/{id}") },
                    },
                    sha1: r.try_get("content_sha1")?,
                    url: r.try_get("source_url")?,
                    regional,
                })
            })
            .collect()
    };

    let sql = format!("SELECT m.id, m.media_type, m.sub_category, m.source, m.path, m.content_sha1, m.source_url FROM media m WHERE m.deleted = FALSE AND {local_scope}");
    let mut found = read(sqlx::query(sqlx::AssertSqlSafe(sql)).bind(id).bind(id).fetch_all(&app.pool).await?, false)?;

    if let Some(source_id) = super::media_sync::main_source(app).await? {
        let sql = format!("SELECT m.id, m.media_type, m.sub_category, m.source, m.path, m.content_sha1, m.source_url FROM remote_media m WHERE m.source_id = ? AND m.deleted = FALSE AND {remote_scope}");
        let remote = read(sqlx::query(sqlx::AssertSqlSafe(sql)).bind(source_id).bind(id).bind(id).fetch_all(&app.pool).await?, true)?;
        // The server's copy of something already here is the same thing: by bytes, by URL
        // (a converted copy has different bytes), or -- for a one-per-game type -- by the
        // slot holding that source's art.
        let held: Vec<Found> = remote
            .into_iter()
            .filter(|r| {
                !found.iter().any(|l| {
                    l.item.media_type == r.item.media_type
                        && ((r.sha1.is_some() && l.sha1 == r.sha1)
                            || (r.url.as_deref().is_some_and(|u| !u.is_empty()) && l.url == r.url)
                            || (settings::SINGLE_MEDIA_TYPES.contains(&r.item.media_type.as_str()) && l.item.sub_category == r.item.sub_category && l.item.source == r.item.source))
                })
            })
            .collect();
        found.extend(held);
    }

    let type_order = |t: &str| super::settings::MEDIA_TYPES.iter().position(|m| *m == t).unwrap_or(usize::MAX);
    found.retain(|f| rules.shows(&f.item.media_type));
    found.sort_by(|a, b| {
        let key = |f: &Found| {
            let i = &f.item;
            let source = i.source.as_deref().unwrap_or(settings::MANUAL_SOURCE);
            // A box front and a cover are one slot (every provider's front picture, under the
            // name it uses): listed together where the box front goes, by the merged order.
            let front = i.media_type == "cover" || (i.media_type == "box" && i.sub_category.as_deref().is_none_or(|s| s == "front"));
            let (group, sub, slot_type, rank) = if front { (type_order("box"), Some("front".to_string()), "box", rules.front_rank(source)) } else { (type_order(&i.media_type), i.sub_category.clone(), i.media_type.as_str(), rules.rank(source, &i.media_type)) };
            // A user's pick first unless `media_priority` places it.
            let placed_pick = source != settings::MANUAL_SOURCE || rules.places_manual(slot_type);
            (group, sub, placed_pick, !f.regional, rank, i.remote, i.id)
        };
        key(a).cmp(&key(b))
    });
    Ok(found.into_iter().map(|f| f.item).collect())
}
