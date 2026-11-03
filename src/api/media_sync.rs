//! Art from the main server, which is the household's art cache.
//!
//! A client's catalogue is its main server's, and so is its art. The server has already paid
//! a provider's quota for every cover it holds and serves them to any signed-in account
//! (`GET /api/v1/media/{id}`, `server::access`), through its own cache tiers before its disk.
//! So while the server can be reached, a client never talks to a provider: what the server
//! has it fetches from the server, and what the server lacks it asks the server to fetch
//! (`POST /media/{id}/refetch` for a row the server advertised and lost, `POST /media/fetch`
//! for a URL it never had) and then takes from the server. Only while the server is offline
//! does a client go to a provider's URL itself, and only if `media_provider_fallback` says so.
//!
//! What the client knows about the server's art is the `remote_media` mirror that
//! `super::sync` pulls alongside the catalogue. Two entry points: [`run`] is the `media_sync`
//! job over that mirror; [`via_main`] is what a plain `media_download` job calls before it
//! would fetch a provider URL, so that path is server-first too.
//!
//! What is written here is indistinguishable from a download this machine made itself: the
//! same `source` / `provider_id` / `source_url`, in the same layout, so `sync::relink_media`
//! and the "already has this provider's cover" checks treat it exactly like one.

use sqlx::Row;

use super::sync::Source;
use super::{App, Error, Result};
use crate::database::jobs::Progress;
use crate::database::media_store::{self, MediaTarget, NewMedia, Roots};
use crate::database::providers::Provider;

pub use romexplorer_types::sync::MediaSyncSummary;

/// The main server's id, if one is configured and enabled.
pub async fn main_source(app: &App) -> Result<Option<u16>> {
    Ok(sqlx::query_scalar("SELECT id FROM remote_sources WHERE role = 'main' AND enabled = TRUE AND deleted = FALSE ORDER BY id LIMIT 1").fetch_optional(&app.pool).await?)
}

/// One server media row, as mirrored.
struct RemoteRow {
    id: u64,
    media_type: String,
    sub_category: Option<String>,
    source: Option<String>,
    provider_id: Option<String>,
    source_url: Option<String>,
    content_sha1: Option<String>,
    /// Where the server filed it, relative to its root (`igdb/12/JP/box_front.jpg`).
    path: String,
}

/// A body fetched from somewhere, with what the server said it was.
pub struct Fetched {
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
}

/// Whether the server answers at all. `ping` is open and cheap; any HTTP answer means the
/// server is up, and only a connection that cannot be made means it is not.
async fn online(source: &Source) -> bool {
    source.peer.http.get(format!("{}/api/v1/ping", source.base_url)).send().await.is_ok()
}

fn status_error(source: &Source, what: &str, status: reqwest::StatusCode, body: &str) -> Error {
    let hint = match status.as_u16() {
        401 | 403 => " -- check the token",
        _ => "",
    };
    let detail = if body.is_empty() { String::new() } else { format!(": {}", body.chars().take(300).collect::<String>()) };
    Error::unavailable(format!("{}: {what}: the server answered {status}{hint}{detail}", source.name))
}

/// The file behind `remote_id` on the server, as the server serves it (so from whichever of
/// its cache tiers has it). `None` when the server says it has no such file (404).
async fn fetch_by_id(app: &App, source: &Source, remote_id: u64) -> Result<Option<Fetched>> {
    let url = format!("{}/api/v1/media/{remote_id}", source.base_url);
    let response = super::sync::request(app, source, &url).send().await.map_err(|e| Error::unavailable(format!("{}: {e}", source.name)))?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::GONE {
        return Ok(None);
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(status_error(source, &format!("media {remote_id}"), status, &body));
    }
    let content_type = response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_string);
    let bytes = response.bytes().await.map_err(|e| Error::unavailable(format!("{}: media {remote_id}: {e}", source.name)))?.to_vec();
    Ok(Some(Fetched { bytes, content_type }))
}

fn post(source: &Source, url: &str) -> reqwest::RequestBuilder {
    let builder = source.peer.http.post(url);
    match &source.token {
        Some(token) if !token.is_empty() => builder.bearer_auth(token),
        _ => builder,
    }
}

/// Asks the server to put the file behind `remote_id` back (from its own cache tiers or the
/// provider), then takes it. `None` only if the server could not: it said the row is gone.
async fn refetch_then_fetch(app: &App, source: &Source, remote_id: u64) -> Result<Option<Fetched>> {
    let url = format!("{}/api/v1/media/{remote_id}/refetch", source.base_url);
    let response = post(source, &url).send().await.map_err(|e| Error::unavailable(format!("{}: {e}", source.name)))?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(status_error(source, &format!("media {remote_id} refetch"), status, &body));
    }
    fetch_by_id(app, source, remote_id).await
}

/// Asks the server to fetch a provider's art it never had, into its own cache, for the
/// given title or release, then takes it from the server.
async fn fetch_via_server(app: &App, source: &Source, req: &super::media::FetchRequest) -> Result<Fetched> {
    let url = format!("{}/api/v1/media/fetch", source.base_url);
    let response = post(source, &url).json(req).send().await.map_err(|e| Error::unavailable(format!("{}: {e}", source.name)))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(status_error(source, &format!("fetch {}", req.url), status, &body));
    }
    let result: super::media::FetchResult = response.json().await.map_err(|e| Error::unavailable(format!("{}: fetch: {e}", source.name)))?;
    fetch_by_id(app, source, result.media_id).await?.ok_or_else(|| Error::unavailable(format!("{}: fetched {} as media {} but does not serve it", source.name, req.url, result.media_id)))
}

/// What asking the main server for a provider URL came to.
pub enum ServerCopy {
    /// The server supplied it (having fetched it first if it had to).
    Bytes(Fetched),
    /// This machine has no main server: a standalone fetches from providers itself.
    NoMainServer,
    /// The server cannot be reached; whether to go to the provider is the caller's call.
    Offline(String),
    /// Art the server's cache route does not handle (franchise and keyword slots).
    NotServed,
}

/// The main server's copy of the provider file at `url` for `target` -- had already, fetched
/// again after losing it, or fetched for the first time at this request -- so that a
/// `media_download` on a client goes through the server like everything else. An error means
/// the server is up but could not supply it, which is not a reason to go to the provider.
pub async fn via_main(app: &App, url: &str, provider: Provider, provider_id: Option<&str>, media_type: &str, sub_category: Option<&str>, target: MediaTarget) -> Result<ServerCopy> {
    let Some(source_id) = main_source(app).await? else { return Ok(ServerCopy::NoMainServer) };
    let source = super::sync::load_source(app, source_id).await?;
    if !online(&source).await {
        return Ok(ServerCopy::Offline(source.name.clone()));
    }
    let listed: Option<u64> = sqlx::query_scalar("SELECT id FROM remote_media WHERE source_id = ? AND source_url = ? AND deleted = FALSE ORDER BY id LIMIT 1")
        .bind(source_id)
        .bind(url)
        .fetch_optional(&app.pool)
        .await?;
    if let Some(remote_id) = listed {
        if let Some(fetched) = fetch_by_id(app, &source, remote_id).await? {
            return Ok(ServerCopy::Bytes(fetched));
        }
        if let Some(fetched) = refetch_then_fetch(app, &source, remote_id).await? {
            return Ok(ServerCopy::Bytes(fetched));
        }
        // The mirror is stale: the row is gone on the server. Fall through and have it
        // fetched afresh.
    }
    let (title_id, release_id) = match target {
        MediaTarget::Title(id) => (Some(id), None),
        MediaTarget::Release(id) => (None, Some(id)),
        _ => return Ok(ServerCopy::NotServed),
    };
    let req = super::media::FetchRequest {
        provider: provider.name().to_string(),
        provider_id: provider_id.map(str::to_string),
        url: url.to_string(),
        media_type: media_type.to_string(),
        sub_category: sub_category.map(str::to_string),
        title_id,
        release_id,
    };
    Ok(ServerCopy::Bytes(fetch_via_server(app, &source, &req).await?))
}

/// A file on the main server, streamed through this machine: what a client shows for media it
/// does not keep a copy of. `path` is the server's own route (`/api/v1/media/7`,
/// `/api/v1/audio/3/file`); `range` is passed on so a player can seek. The reply is the
/// server's, unread -- status, headers and body are the caller's to hand on.
///
/// Asked for uncompressed: a `Content-Range` describes the bytes as stored, and `reqwest`
/// would otherwise accept zstd and undo it, leaving the header describing something else.
pub async fn from_main(app: &App, path: &str, range: Option<&str>) -> Result<reqwest::Response> {
    let source_id = main_source(app).await?.ok_or_else(|| Error::not_found("there is no main server to stream from"))?;
    let source = super::sync::load_source(app, source_id).await?;
    let mut request = super::sync::request(app, &source, &format!("{}{path}", source.base_url)).header(reqwest::header::ACCEPT_ENCODING, "identity");
    if let Some(range) = range {
        request = request.header(reqwest::header::RANGE, range);
    }
    let response = request.send().await.map_err(|e| Error::unavailable(format!("{}: {e}", source.name)))?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::GONE {
        return Err(Error::not_found(format!("{} no longer has {path}", source.name)));
    }
    if !status.is_success() && status != reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        let body = response.text().await.unwrap_or_default();
        return Err(status_error(&source, path, status, &body));
    }
    Ok(response)
}

/// Whether `target` already holds art from `art_source` (a provider name, or `manual`) in
/// this slot here.
async fn holds(pool: &sqlx::MySqlPool, target: MediaTarget, art_source: &str, media_type: &str, sub_category: Option<&str>) -> Result<bool> {
    let (junction, key, id) = target.junction();
    let sql = format!(
        "SELECT 1 FROM {junction} j JOIN media m ON m.id = j.media_id \
         WHERE j.{key} = ? AND m.source = ? AND m.media_type = ? AND (m.sub_category <=> ?) AND m.deleted = FALSE LIMIT 1"
    );
    Ok(sqlx::query(sqlx::AssertSqlSafe(sql)).bind(id).bind(art_source).bind(media_type).bind(sub_category).fetch_optional(pool).await?.is_some())
}

/// Whether `target` already holds the file from `url` here.
async fn holds_url(pool: &sqlx::MySqlPool, target: MediaTarget, url: &str) -> Result<bool> {
    let (junction, key, id) = target.junction();
    let sql = format!("SELECT 1 FROM {junction} j JOIN media m ON m.id = j.media_id WHERE j.{key} = ? AND m.source_url = ? AND m.deleted = FALSE LIMIT 1");
    Ok(sqlx::query(sqlx::AssertSqlSafe(sql)).bind(id).bind(url).fetch_optional(pool).await?.is_some())
}

/// Whether `target` holds art from `art_source` in this slot that the server no longer lists
/// for it: the file the server has since replaced, so the server's new one is a replacement,
/// not a second picture. Art the server still lists for the target -- IGDB's main cover next
/// to its Japanese one -- does not count, so both are kept here as they are there.
async fn holds_unlisted(pool: &sqlx::MySqlPool, source_id: u16, target: MediaTarget, art_source: &str, media_type: &str, sub_category: Option<&str>) -> Result<bool> {
    if !matches!(target, MediaTarget::Title(_) | MediaTarget::Release(_)) {
        return holds(pool, target, art_source, media_type, sub_category).await;
    }
    let (junction, key, id) = target.junction();
    let sql = format!(
        "SELECT 1 FROM {junction} j JOIN media m ON m.id = j.media_id \
         WHERE j.{key} = ? AND m.source = ? AND m.media_type = ? AND (m.sub_category <=> ?) AND m.deleted = FALSE \
         AND NOT EXISTS (SELECT 1 FROM remote_{junction} rj JOIN remote_media rm ON rm.source_id = rj.source_id AND rm.id = rj.media_id \
                         WHERE rj.source_id = ? AND rj.{key} = ? AND rm.deleted = FALSE AND rm.source_url = m.source_url) \
         LIMIT 1"
    );
    Ok(sqlx::query(sqlx::AssertSqlSafe(sql)).bind(id).bind(art_source).bind(media_type).bind(sub_category).bind(source_id).bind(id).fetch_optional(pool).await?.is_some())
}

/// The folder the server filed provider art in, when it is one of this layout's folders for
/// the same source and id (`igdb/12/JP` for `igdb/12/common`'s art): the server split the
/// targets by release group when it downloaded, which a client cannot redo from the targets
/// alone, and the region folder is what ranks a regional box above the common one.
fn server_folder(server_path: &str, source: &str, provider_id: Option<&str>) -> Option<String> {
    if source == "manual" {
        return None;
    }
    let id = provider_id.map(str::trim).filter(|id| !id.is_empty())?;
    let common = media_store::layout_folder(source, Some(id), ("title", 0), None, None);
    let prefix = common.strip_suffix("/common")?;
    let path = server_path.replace('\\', "/");
    let (folder, _) = path.rsplit_once('/')?;
    let rest = folder.strip_prefix(prefix)?.strip_prefix('/')?;
    if rest.split('/').any(|c| c.is_empty() || c == "." || c == "..") {
        return None;
    }
    Some(folder.to_string())
}

/// The targets of one server row that exist here and still want it.
async fn pending_targets(app: &App, source_id: u16, row: &RemoteRow, art_source: &str) -> Result<Vec<MediaTarget>> {
    let mut targets = Vec::new();
    let titles: Vec<u64> = sqlx::query_scalar("SELECT rtm.title_id FROM remote_title_media rtm JOIN title t ON t.id = rtm.title_id WHERE rtm.source_id = ? AND rtm.media_id = ? AND t.deleted = FALSE")
        .bind(source_id)
        .bind(row.id)
        .fetch_all(&app.pool)
        .await?;
    targets.extend(titles.into_iter().map(MediaTarget::Title));
    let releases: Vec<u64> = sqlx::query_scalar("SELECT rrm.release_id FROM remote_release_media rrm JOIN `release` r ON r.id = rrm.release_id WHERE rrm.source_id = ? AND rrm.media_id = ? AND r.deleted = FALSE")
        .bind(source_id)
        .bind(row.id)
        .fetch_all(&app.pool)
        .await?;
    targets.extend(releases.into_iter().map(MediaTarget::Release));

    let mut pending = Vec::with_capacity(targets.len());
    for target in targets {
        // A pick made on this machine outranks the server's -- the same rule a provider sync
        // follows -- and a slot already holding this source's art is done.
        // A type a game has several of (screenshots, fan art) is done per file, by its URL:
        // "this source already has one here" would stop at the first.
        let list_url = row.source_url.as_deref().filter(|u| !u.is_empty() && !crate::database::settings::SINGLE_MEDIA_TYPES.contains(&row.media_type.as_str()));
        // A single slot is done when this file is here already, or when this source's art in
        // it is one the server has since replaced. Not merely "this source has one": a
        // release holds IGDB's main cover and its regional one, as it does on the server.
        let url = row.source_url.as_deref().filter(|u| !u.is_empty());
        let held = match (list_url, url) {
            (Some(url), _) => holds_url(&app.pool, target, url).await?,
            (None, Some(url)) => {
                media_store::has_manual(&app.pool, target, &row.media_type, row.sub_category.as_deref()).await?
                    || holds_url(&app.pool, target, url).await?
                    || holds_unlisted(&app.pool, source_id, target, art_source, &row.media_type, row.sub_category.as_deref()).await?
            }
            (None, None) => media_store::has_manual(&app.pool, target, &row.media_type, row.sub_category.as_deref()).await? || holds(&app.pool, target, art_source, &row.media_type, row.sub_category.as_deref()).await?,
        };
        if held {
            continue;
        }
        pending.push(target);
    }
    Ok(pending)
}

/// A media row this machine already has for the same bytes or the same URL, so the server
/// need not be asked for what an earlier download brought.
async fn existing_local(pool: &sqlx::MySqlPool, row: &RemoteRow) -> Result<Option<u64>> {
    if let Some(sha1) = row.content_sha1.as_deref().filter(|s| !s.is_empty()) {
        let hit: Option<u64> = sqlx::query_scalar("SELECT id FROM media WHERE content_sha1 = ? AND media_type = ? AND deleted = FALSE ORDER BY id LIMIT 1").bind(sha1).bind(&row.media_type).fetch_optional(pool).await?;
        if hit.is_some() {
            return Ok(hit);
        }
    }
    if let Some(url) = row.source_url.as_deref().filter(|u| !u.is_empty()) {
        let hit: Option<u64> = sqlx::query_scalar("SELECT id FROM media WHERE source_url = ? AND media_type = ? AND deleted = FALSE ORDER BY id LIMIT 1").bind(url).bind(&row.media_type).fetch_optional(pool).await?;
        if hit.is_some() {
            return Ok(hit);
        }
    }
    Ok(None)
}

/// Where a row's bytes came from.
enum Origin {
    Server,
    ServerRefetched,
    Provider,
}

/// The bytes for one mirrored row: from the server while it is up (asking it to put a lost
/// file back first), from the provider only while it is down and the fallback is allowed.
/// `None` means nobody could supply it.
async fn obtain(app: &App, source: &Source, is_online: bool, fallback: bool, row: &RemoteRow) -> Result<Option<(Fetched, Origin)>> {
    if is_online {
        if let Some(fetched) = fetch_by_id(app, source, row.id).await? {
            return Ok(Some((fetched, Origin::Server)));
        }
        return match refetch_then_fetch(app, source, row.id).await {
            Ok(Some(fetched)) => Ok(Some((fetched, Origin::ServerRefetched))),
            Ok(None) => Ok(None),
            // The server is up but could not get this one (its provider refused, say): its
            // problem to report, this row's loss, and no reason to stop the rest.
            Err(e) => {
                log::warn!("media_sync: {}: media {}: {e}", source.name, row.id);
                Ok(None)
            }
        };
    }
    let Some(url) = row.source_url.as_deref().filter(|u| !u.is_empty() && fallback) else { return Ok(None) };
    match app.http.get(url).send().await.and_then(|r| r.error_for_status()) {
        Ok(response) => {
            let content_type = response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_string);
            match response.bytes().await {
                Ok(bytes) => Ok(Some((Fetched { bytes: bytes.to_vec(), content_type }, Origin::Provider))),
                Err(e) => {
                    log::warn!("media_sync: {url}: {e}");
                    Ok(None)
                }
            }
        }
        Err(e) => {
            log::warn!("media_sync: {url}: {e}");
            Ok(None)
        }
    }
}

/// Fetches every file `source_id` (the main server when `None`) holds that this machine's
/// titles and releases lack: from the server while it is up, from providers only while it is
/// down and `media_provider_fallback` allows it.
pub async fn run(app: &App, source_id: Option<u16>, progress: &Progress) -> Result<MediaSyncSummary> {
    let source_id = match source_id {
        Some(id) => id,
        None => main_source(app).await?.ok_or_else(|| Error::invalid("there is no main server to fetch art from -- add one under Config > Share"))?,
    };
    let source = super::sync::load_source(app, source_id).await?;
    let fallback = crate::database::settings::media_provider_fallback(&app.pool).await?;
    let is_online = online(&source).await;
    if !is_online && !fallback {
        return Err(Error::unavailable(format!("{} cannot be reached, and media_provider_fallback is off, so there is nowhere to fetch art from right now", source.name)));
    }
    let roots = Roots::load(&app.pool, &app.paths.media_dir()).await?;
    let mut summary = MediaSyncSummary { source: source.name.clone(), ..Default::default() };
    // A client chooses, per media type, between its own copy and streaming from the server.
    // A standalone taking art from a peer has nobody to stream from, so it keeps everything.
    let rules = crate::database::settings::MediaRules::load(&app.pool).await?;
    let is_client = crate::database::settings::instance_mode(&app.pool).await? == crate::database::settings::InstanceMode::Client;

    let rows: Vec<RemoteRow> = sqlx::query("SELECT id, media_type, sub_category, source, provider_id, source_url, content_sha1, path FROM remote_media WHERE source_id = ? AND deleted = FALSE ORDER BY id")
        .bind(source_id)
        .fetch_all(&app.pool)
        .await?
        .iter()
        .map(|r| {
            Ok(RemoteRow {
                id: r.try_get("id")?,
                media_type: r.try_get("media_type")?,
                sub_category: r.try_get("sub_category")?,
                source: r.try_get("source")?,
                provider_id: r.try_get("provider_id")?,
                source_url: r.try_get("source_url")?,
                content_sha1: r.try_get("content_sha1")?,
                path: r.try_get("path")?,
            })
        })
        .collect::<Result<_>>()?;
    let total = rows.len() as u64;
    progress.force(0, Some(total.max(1)), &format!("{}: {total} art row(s) on the server{}", source.name, if is_online { "" } else { " (offline -- fetching from providers)" })).await;

    for (i, row) in rows.iter().enumerate() {
        summary.considered += 1;
        if is_client && !rules.keeps(&row.media_type) {
            summary.streamed_only += 1;
            continue;
        }
        // A row with no source is one this schema never writes; carrying it as "unknown"
        // would let a later provider sync add a second copy next to it, so it is recorded
        // under the neutral name the server would have shown for it.
        let source_name = row.source.clone().unwrap_or_else(|| "manual".to_string());
        let targets = pending_targets(app, source_id, row, &source_name).await?;
        if targets.is_empty() {
            summary.already_held += 1;
            continue;
        }
        let (mut placement, group) = media_store::placement_for(&app.pool, &targets, &row.media_type, row.sub_category.as_deref(), row.source_url.as_deref(), &source_name, row.provider_id.as_deref()).await?;
        if let Some(folder) = server_folder(&row.path, &source_name, row.provider_id.as_deref()) {
            placement.folder = folder;
        }

        if let Some(media_id) = existing_local(&app.pool, row).await? {
            media_store::link_placed(&app.pool, media_id, &targets, group.as_ref(), &row.media_type, row.sub_category.as_deref()).await?;
            summary.linked_existing += 1;
            continue;
        }

        let Some((Fetched { bytes, content_type }, origin)) = obtain(app, &source, is_online, fallback, row).await? else {
            summary.failed += 1;
            progress.report(i as u64 + 1, Some(total), &format!("{}: {} fetched, {} not available", source.name, summary.from_server + summary.from_provider, summary.failed)).await;
            continue;
        };

        let name = row.source_url.as_deref().filter(|u| !u.is_empty()).unwrap_or(&row.media_type);
        let ext = media_store::extension_for(name, content_type.as_deref());
        // A client's copy of an image is AVIF when that is smaller. Video is never converted
        // (there is no encoder here), which is why it is streamed unless asked for.
        // What the grid thumbnail of a converted image is made from: see `store_thumb`.
        let mut thumb_source = None;
        let (bytes, ext) = if is_client {
            let original = bytes;
            let (original, smaller) = tokio::task::spawn_blocking(move || {
                let smaller = crate::database::media_cache::optimised_copy(&original);
                (original, smaller)
            })
            .await
            .map_err(|e| Error::internal(format!("media_sync: converting media {}: {e}", row.id)))?;
            match smaller {
                Some(avif) => {
                    summary.converted += 1;
                    thumb_source = Some(original);
                    (avif, "avif")
                }
                None => (original, ext),
            }
        } else {
            (bytes, ext)
        };
        let meta = NewMedia {
            media_type: &row.media_type,
            sub_category: row.sub_category.as_deref(),
            source: &source_name,
            provider_id: row.provider_id.as_deref(),
            source_url: row.source_url.as_deref().filter(|u| !u.is_empty()),
            ext,
            // A user's pick on the server is a pick here too: its own row, so no provider
            // sync replaces it.
            own_row: source_name == "manual",
            on_existing: media_store::OnExisting::ReplaceRetired,
        };
        let stored = media_store::store_bytes(&app.pool, &roots, &placement, &meta, &bytes).await?;
        media_store::link_placed(&app.pool, stored.media_id, &targets, group.as_ref(), &row.media_type, row.sub_category.as_deref()).await?;
        if let Some(original) = thumb_source {
            let caps = crate::database::settings::media_cache_caps(&app.pool).await?;
            let (px, quality, format) = crate::database::settings::media_thumb(&app.pool).await?;
            let format = crate::database::media_cache::ThumbFormat::from_name(&format).unwrap_or(crate::database::media_cache::ThumbFormat::Avif);
            crate::database::media_cache::store_thumb(&app.pool, &roots, caps, stored.media_id, original, px, format, quality).await?;
        }
        summary.bytes += bytes.len() as u64;
        match origin {
            Origin::Server => summary.from_server += 1,
            Origin::ServerRefetched => {
                summary.from_server += 1;
                summary.server_refetched += 1;
            }
            Origin::Provider => summary.from_provider += 1,
        }
        progress
            .report(
                i as u64 + 1,
                Some(total),
                &format!("{}: {} from the server, {} from providers, {} not available -- {}", source.name, summary.from_server, summary.from_provider, summary.failed, crate::database::jobs::human_bytes(summary.bytes)),
            )
            .await;
    }
    progress
        .force(
            total,
            Some(total.max(1)),
            &format!(
                "{}: {} from the server ({} it had to fetch again, {} stored as AVIF), {} from providers, {} already here, {} left to stream, {} not available",
                source.name,
                summary.from_server,
                summary.server_refetched,
                summary.converted,
                summary.from_provider,
                summary.already_held + summary.linked_existing,
                summary.streamed_only,
                summary.failed
            ),
        )
        .await;
    Ok(summary)
}
