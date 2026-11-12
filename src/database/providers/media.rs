//! Media download for provider art: fetched once, stored through `media_store` (layout
//! `<root>/<Platform>/<Title>/<Region>/<type>_<sub>.<ext>`, dedup by bytes), linked to
//! its targets. Provider-specific URL helpers for the art each client exposes.

use std::path::{Path, PathBuf};

use playmatch_deredis::providers::steamgriddb::model::AssetFilters;
use sqlx::mysql::MySqlPool;
use sqlx::Row;

use super::super::error::DatabaseError;
use super::super::media_store::{self, NewMedia, Roots};
use super::retroachievements::{RaClient, RaGame};
use super::{Provider, ProviderClients};

/// Attachment rule (docs/plan/database.md): images and videos go to releases -- a
/// provider's title-level art is downloaded once and linked to every release of the title
/// (`release_targets`), region-specific art from release-tier providers then sits next to
/// it per release group; audio goes to a title (its soundtrack), a franchise or a keyword
/// (arrangement tracks spanning games).
pub use super::super::media_store::MediaTarget;

/// Every release of `title_id` -- the targets the title's art from `provider` game
/// `provider_id` is linked to -- except a release of the game itself that has a game of its
/// own at that provider (a title combined from several IGDB games): that one shows its own.
pub async fn release_targets(pool: &MySqlPool, title_id: u64, provider: Provider, provider_id: &str) -> Result<Vec<MediaTarget>, DatabaseError> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT tr.release_id FROM title_releases tr JOIN `release` r ON r.id = tr.release_id WHERE tr.title_id = ? AND r.deleted = FALSE \
         AND NOT EXISTS (SELECT 1 FROM release_provider_links ro WHERE ro.release_id = r.id AND ro.provider = ? AND ro.deleted = FALSE AND ro.provider_id <> ? AND NOT {})",
        super::cache::APART_SQL
    )))
    .bind(title_id)
    .bind(provider.name())
    .bind(provider_id)
    .fetch_all(pool)
    .await?;
    rows.iter().map(|r| Ok(MediaTarget::Release(r.try_get::<u64, _>("release_id")?))).collect()
}

/// Whether `target` already has media of `media_type` from `provider` in the slot
/// `sub_category` names (box front and box back are separate slots), so a cover isn't
/// re-fetched on every provider query.
pub async fn has_media(pool: &MySqlPool, target: MediaTarget, provider: Provider, media_type: &str, sub_category: Option<&str>) -> Result<bool, DatabaseError> {
    Ok(count_media(pool, target, provider.name(), media_type, sub_category).await? > 0)
}

/// How many items `target` has of `media_type` / `sub_category` from `source`.
async fn count_media(pool: &MySqlPool, target: MediaTarget, source: &str, media_type: &str, sub_category: Option<&str>) -> Result<u64, DatabaseError> {
    let (junction, key, id) = target.junction();
    let sql = format!("SELECT COUNT(*) FROM {junction} j JOIN media m ON m.id = j.media_id WHERE j.{key} = ? AND m.source = ? AND m.media_type = ? AND m.sub_category <=> ? AND m.deleted = FALSE");
    let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql)).bind(id).bind(source).bind(media_type).bind(sub_category).fetch_one(pool).await?;
    Ok(n as u64)
}

/// Whether `target` still wants `media_type` / `sub_category` from `provider`: the gate
/// [`attach_media`] applies, without the download rule (the caller checks that once).
///
/// A one-per-game type is wanted when the slot has nothing from this provider and nothing
/// from anyone ranked above it in `media_priority` -- your own uploads included, which rank
/// first unless the order puts them lower. A list type (screenshots, fan art) is wanted while
/// the target has fewer than `list_n` from this provider, so raising `media_list_cap` tops it
/// up; `list_n` is the cap, or less where the provider is known to have fewer.
pub async fn wants(pool: &MySqlPool, rules: &super::super::settings::MediaRules, provider: Provider, media_type: &str, sub_category: Option<&str>, target: MediaTarget, list_n: usize) -> Result<bool, DatabaseError> {
    wants_as(pool, rules, &Ask::Auto, provider.name(), media_type, sub_category, target, list_n).await
}

/// Who wants the art: the settings, or someone who asked for it by hand.
///
/// [`Ask::Asked`] is "download art" run on a selection with chosen types. It fetches those
/// types whatever `media_download` says, and whether or not a provider ranked higher already
/// gave one -- they asked this provider for it. What it still respects: art this provider
/// already gave the target, and the user's own pick.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Ask {
    #[default]
    Auto,
    /// Slots as `type` or `type:sub` (`box:front`, `screenshot`); a bare `box` is every box.
    Asked(Vec<String>),
}

impl Ask {
    pub fn is_asked(&self) -> bool {
        matches!(self, Ask::Asked(_))
    }

    /// Whether this type is to be downloaded at all.
    pub(super) fn allows(&self, rules: &super::super::settings::MediaRules, source: &str, media_type: &str, sub_category: Option<&str>) -> bool {
        match self {
            Ask::Auto => rules.downloads(source, media_type),
            Ask::Asked(slots) => slots.iter().any(|s| slot_matches(s, media_type, sub_category)),
        }
    }
}

/// `box:front` against (`box`, `front`); a bare `box` takes any sub-category.
pub fn slot_matches(slot: &str, media_type: &str, sub_category: Option<&str>) -> bool {
    match slot.split_once(':') {
        Some((t, s)) => t == media_type && Some(s) == sub_category,
        None => slot == media_type,
    }
}

#[allow(clippy::too_many_arguments)]
/// [`wants`] for any art source by name: a provider, or a source no provider stands for
/// (`xlenore`'s covers by serial).
pub(super) async fn wants_as(pool: &MySqlPool, rules: &super::super::settings::MediaRules, ask: &Ask, source: &str, media_type: &str, sub_category: Option<&str>, target: MediaTarget, list_n: usize) -> Result<bool, DatabaseError> {
    if !super::super::settings::SINGLE_MEDIA_TYPES.contains(&media_type) {
        return Ok(count_media(pool, target, source, media_type, sub_category).await? < list_n as u64);
    }
    if count_media(pool, target, source, media_type, sub_category).await? > 0 {
        return Ok(false);
    }
    if ask.is_asked() {
        return Ok(!media_store::has_manual(pool, target, media_type, sub_category).await?);
    }
    for higher in rules.outranking(source, media_type) {
        let taken = if higher == super::super::settings::MANUAL_SOURCE {
            media_store::has_manual(pool, target, media_type, sub_category).await?
        } else {
            count_media(pool, target, higher, media_type, sub_category).await? > 0
        };
        if taken {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether any of `targets` wants `media_type` from `provider` and the settings download it --
/// the check made before asking a provider for URLs, so each release is judged on its own
/// rather than by the title's first.
pub async fn any_wanting(pool: &MySqlPool, rules: &super::super::settings::MediaRules, provider: Provider, media_type: &str, sub_category: Option<&str>, targets: &[MediaTarget], list_n: usize) -> Result<bool, DatabaseError> {
    any_wanting_as(pool, rules, &Ask::Auto, provider, media_type, sub_category, targets, list_n).await
}

#[allow(clippy::too_many_arguments)]
async fn any_wanting_as(pool: &MySqlPool, rules: &super::super::settings::MediaRules, ask: &Ask, provider: Provider, media_type: &str, sub_category: Option<&str>, targets: &[MediaTarget], list_n: usize) -> Result<bool, DatabaseError> {
    if list_n == 0 || !ask.allows(rules, provider.name(), media_type, sub_category) {
        return Ok(false);
    }
    for target in targets {
        if wants_as(pool, rules, ask, provider.name(), media_type, sub_category, *target, list_n).await? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Downloads `url` once and links it to every `target` that neither has this provider's
/// art of the type yet nor holds a user-picked (manual) item in the slot -- manual always
/// wins. Returns how many targets were newly linked; a failed download is logged, never
/// fatal.
///
/// This is the one place provider art is stored, so it is where the media settings apply:
/// nothing is fetched for a type that is turned off for the provider (`media_download`), nor
/// for a one-per-game type on a target that already has it from a provider ranked higher
/// (`media_priority`).
pub async fn attach_media(
    http: &reqwest::Client,
    pool: &MySqlPool,
    media_dir: &Path,
    provider: Provider,
    // The provider's own id for what this art depicts. Stored on the media row so the link
    // can be rebuilt after a catalogue replacement renumbers titles (`media.provider_id`).
    provider_id: Option<&str>,
    url: &str,
    media_type: &str,
    sub_category: Option<&str>,
    targets: &[MediaTarget],
) -> Result<u32, DatabaseError> {
    attach_media_as(http, pool, media_dir, &Ask::Auto, provider, provider_id, url, media_type, sub_category, targets).await
}

#[allow(clippy::too_many_arguments)]
async fn attach_media_as(
    http: &reqwest::Client,
    pool: &MySqlPool,
    media_dir: &Path,
    ask: &Ask,
    provider: Provider,
    provider_id: Option<&str>,
    url: &str,
    media_type: &str,
    sub_category: Option<&str>,
    targets: &[MediaTarget],
) -> Result<u32, DatabaseError> {
    match try_attach_media_as(http, pool, media_dir, ask, provider, provider_id, url, media_type, sub_category, targets).await {
        Ok(linked) => Ok(linked),
        Err(e) => {
            log::warn!("media: {} {media_type} for {} target(s) failed: {e}", provider.name(), targets.len());
            Ok(0)
        }
    }
}

/// [`attach_media`] that hands a failed download back instead of logging it, for a caller
/// that counts them: a job that says "0 covers" has to be able to say why -- a media dir
/// that cannot be written looks, from the jobs list, exactly like art nobody wanted.
/// Detaches art a provider gave for a game the title or release is no longer linked to: a
/// link that was wrong and has since been changed, removed by the merge audit, or unlinked by
/// hand left the old game's cover and screenshots in place, because nothing looked back.
///
/// Art carries the provider and the provider's id for what it shows (`media.source`,
/// `media.provider_id`, migration 0023), so this needs no guessing: a piece is stale when its
/// release (or title) has a link for that provider -- current or removed -- and none of the
/// current ones, on the release or on its title, is the id the art came from. Art with no
/// provider id (uploads, art from before 0023) and art from a provider with no link at all
/// (found by name) are left alone. The files stay; "download missing media" fetches the right
/// game's art, from the files already here when they are the same.
///
/// `titles`: only these titles and their releases (None: everything). Returns how many
/// attachments were removed.
pub async fn detach_stale(pool: &MySqlPool, titles: Option<&[u64]>) -> Result<u64, DatabaseError> {
    let (release_scope, title_scope) = match titles {
        Some([]) => return Ok(0),
        Some(ids) => {
            let list = ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
            (format!(" AND rm.release_id IN (SELECT release_id FROM title_releases WHERE title_id IN ({list}))"), format!(" AND tm.title_id IN ({list})"))
        }
        None => (String::new(), String::new()),
    };
    let release_rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DELETE rm FROM release_media rm JOIN media m ON m.id = rm.media_id AND m.source IS NOT NULL AND m.provider_id IS NOT NULL \
         WHERE (EXISTS (SELECT 1 FROM release_provider_links rl WHERE rl.release_id = rm.release_id AND rl.provider = m.source) \
             OR EXISTS (SELECT 1 FROM title_releases tr JOIN title_provider_links tl ON tl.title_id = tr.title_id AND tl.provider = m.source WHERE tr.release_id = rm.release_id)) \
           AND NOT EXISTS (SELECT 1 FROM release_provider_links rl WHERE rl.release_id = rm.release_id AND rl.provider = m.source AND rl.deleted = FALSE AND rl.provider_id = m.provider_id) \
           AND NOT EXISTS (SELECT 1 FROM title_releases tr JOIN title_provider_links tl ON tl.title_id = tr.title_id AND tl.provider = m.source AND tl.deleted = FALSE AND tl.provider_id = m.provider_id WHERE tr.release_id = rm.release_id \
               AND NOT EXISTS (SELECT 1 FROM release_provider_links ro JOIN `release` r ON r.id = ro.release_id WHERE ro.release_id = rm.release_id AND ro.provider = m.source AND ro.deleted = FALSE AND NOT {apart})){release_scope}",
        apart = super::cache::APART_SQL
    )))
    .execute(pool)
    .await?
    .rows_affected();
    let title_rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DELETE tm FROM title_media tm JOIN media m ON m.id = tm.media_id AND m.source IS NOT NULL AND m.provider_id IS NOT NULL \
         WHERE EXISTS (SELECT 1 FROM title_provider_links tl WHERE tl.title_id = tm.title_id AND tl.provider = m.source) \
           AND NOT EXISTS (SELECT 1 FROM title_provider_links tl WHERE tl.title_id = tm.title_id AND tl.provider = m.source AND tl.deleted = FALSE AND tl.provider_id = m.provider_id) \
           AND NOT EXISTS (SELECT 1 FROM title_releases tr JOIN release_provider_links rl ON rl.release_id = tr.release_id AND rl.provider = m.source AND rl.deleted = FALSE AND rl.provider_id = m.provider_id WHERE tr.title_id = tm.title_id){title_scope}"
    )))
    .execute(pool)
    .await?
    .rows_affected();
    Ok(release_rows + title_rows)
}

pub async fn try_attach_media(
    http: &reqwest::Client,
    pool: &MySqlPool,
    media_dir: &Path,
    provider: Provider,
    provider_id: Option<&str>,
    url: &str,
    media_type: &str,
    sub_category: Option<&str>,
    targets: &[MediaTarget],
) -> Result<u32, DatabaseError> {
    try_attach_media_as(http, pool, media_dir, &Ask::Auto, provider, provider_id, url, media_type, sub_category, targets).await
}

#[allow(clippy::too_many_arguments)]
async fn try_attach_media_as(
    http: &reqwest::Client,
    pool: &MySqlPool,
    media_dir: &Path,
    ask: &Ask,
    provider: Provider,
    provider_id: Option<&str>,
    url: &str,
    media_type: &str,
    sub_category: Option<&str>,
    targets: &[MediaTarget],
) -> Result<u32, DatabaseError> {
    let rules = super::super::settings::MediaRules::load(pool).await?;
    if !ask.allows(&rules, provider.name(), media_type, sub_category) {
        return Ok(0);
    }
    let mut pending = Vec::with_capacity(targets.len());
    for target in targets {
        if wants_as(pool, &rules, ask, provider.name(), media_type, sub_category, *target, 1).await? {
            pending.push(*target);
        }
    }
    if pending.is_empty() {
        return Ok(0);
    }
    Ok(store_url(http, pool, media_dir, provider.name(), provider_id, url, media_type, sub_category, &pending).await?.1)
}

/// Whether `target` already has the file at `url`.
pub(super) async fn has_url(pool: &MySqlPool, target: MediaTarget, url: &str) -> Result<bool, DatabaseError> {
    let (junction, key, id) = target.junction();
    let sql = format!("SELECT 1 FROM {junction} j JOIN media m ON m.id = j.media_id WHERE j.{key} = ? AND m.source_url = ? AND m.deleted = FALSE LIMIT 1");
    Ok(sqlx::query(sqlx::AssertSqlSafe(sql)).bind(id).bind(url).fetch_optional(pool).await?.is_some())
}

/// [`attach_media`] for a type a game has several of (screenshots, fan art): the first
/// `media_list_cap` of `urls`, each linked to the targets that do not have that file yet.
/// Nothing when `media_download` has the type off for the provider. Returns links made.
pub async fn attach_media_list(
    http: &reqwest::Client,
    pool: &MySqlPool,
    media_dir: &Path,
    provider: Provider,
    provider_id: Option<&str>,
    urls: &[String],
    media_type: &str,
    targets: &[MediaTarget],
) -> Result<u32, DatabaseError> {
    attach_media_list_as(http, pool, media_dir, &Ask::Auto, provider, provider_id, urls, media_type, targets).await
}

#[allow(clippy::too_many_arguments)]
async fn attach_media_list_as(
    http: &reqwest::Client,
    pool: &MySqlPool,
    media_dir: &Path,
    ask: &Ask,
    provider: Provider,
    provider_id: Option<&str>,
    urls: &[String],
    media_type: &str,
    targets: &[MediaTarget],
) -> Result<u32, DatabaseError> {
    if targets.is_empty() || urls.is_empty() || !ask.allows(&super::super::settings::MediaRules::load(pool).await?, provider.name(), media_type, None) {
        return Ok(0);
    }
    let cap = super::super::settings::media_list_cap(pool).await?;
    let mut linked = 0;
    for url in urls.iter().take(cap) {
        let mut pending = Vec::with_capacity(targets.len());
        for target in targets {
            // Up to the cap per target: a provider whose list changed must not push a
            // release past it with its new URLs.
            if !has_url(pool, *target, url).await? && count_media(pool, *target, provider.name(), media_type, None).await? < cap as u64 {
                pending.push(*target);
            }
        }
        if pending.is_empty() {
            continue;
        }
        match store_url(http, pool, media_dir, provider.name(), provider_id, url, media_type, None, &pending).await {
            Ok((_, n)) => linked += n,
            Err(e) => log::warn!("media: {} {media_type} for {} target(s) failed: {e}", provider.name(), pending.len()),
        }
    }
    Ok(linked)
}

#[derive(Debug, Clone)]
pub struct MediaRecord {
    pub media_id: u64,
    pub path: PathBuf,
    pub bytes: u64,
    pub fresh: bool,
}

/// Fetches `url` (unless a row for it exists), stores it in the layout for `targets` and
/// links it. Returns the record and how many targets were linked.
pub async fn store_url(
    http: &reqwest::Client,
    pool: &MySqlPool,
    media_dir: &Path,
    source: &str,
    provider_id: Option<&str>,
    url: &str,
    media_type: &str,
    sub_category: Option<&str>,
    targets: &[MediaTarget],
) -> Result<(MediaRecord, u32), DatabaseError> {
    store_url_with(http, pool, media_dir, source, provider_id, url, media_type, sub_category, targets, None).await
}

/// [`store_url`], given the body already when the caller got it from somewhere better than
/// the provider -- a client's main server (`api::media_sync::from_main_by_url`). The row is
/// recorded against `url` either way, so what was fetched from the server is later found by
/// the same URL a provider sync would look for.
pub async fn store_url_with(
    http: &reqwest::Client,
    pool: &MySqlPool,
    media_dir: &Path,
    source: &str,
    // `provider_id`: the provider's own id for what this art depicts, so the link can be
    // rebuilt after a catalogue replacement renumbers titles (see `media.provider_id`).
    // `None` when the caller has none.
    provider_id: Option<&str>,
    url: &str,
    media_type: &str,
    sub_category: Option<&str>,
    targets: &[MediaTarget],
    prefetched: Option<(Vec<u8>, Option<String>)>,
) -> Result<(MediaRecord, u32), DatabaseError> {
    store_url_placed(http, pool, media_dir, source, provider_id, url, media_type, sub_category, targets, prefetched, true).await
}

/// [`store_url_with`] that links `targets` alone, not the rest of their release group:
/// for art that is one release's own -- a cover found by that release's serial, where a
/// budget reissue or a limited edition in the same group has a cover of its own.
#[allow(clippy::too_many_arguments)]
pub async fn store_url_own(
    http: &reqwest::Client,
    pool: &MySqlPool,
    media_dir: &Path,
    source: &str,
    provider_id: Option<&str>,
    url: &str,
    media_type: &str,
    sub_category: Option<&str>,
    targets: &[MediaTarget],
    prefetched: Option<(Vec<u8>, Option<String>)>,
) -> Result<(MediaRecord, u32), DatabaseError> {
    store_url_placed(http, pool, media_dir, source, provider_id, url, media_type, sub_category, targets, prefetched, false).await
}

#[allow(clippy::too_many_arguments)]
async fn store_url_placed(
    http: &reqwest::Client,
    pool: &MySqlPool,
    media_dir: &Path,
    source: &str,
    provider_id: Option<&str>,
    url: &str,
    media_type: &str,
    sub_category: Option<&str>,
    targets: &[MediaTarget],
    prefetched: Option<(Vec<u8>, Option<String>)>,
    grouped: bool,
) -> Result<(MediaRecord, u32), DatabaseError> {
    let roots = Roots::load(pool, media_dir).await?;
    let (placement, group) = media_store::placement_for(pool, targets, media_type, sub_category, Some(url), source, provider_id).await?;
    let group = if grouped { group } else { None };
    if let Some(row) = sqlx::query("SELECT id, path, root_role, bytes FROM media WHERE source_url = ? AND media_type = ? AND deleted = FALSE ORDER BY id LIMIT 1").bind(url).bind(media_type).fetch_optional(pool).await? {
        let media_id: u64 = row.try_get("id")?;
        let path = roots.resolve(row.try_get::<Option<String>, _>("root_role")?.as_deref(), &row.try_get::<String, _>("path")?);
        let linked = media_store::link_placed(pool, media_id, targets, group.as_ref(), media_type, sub_category).await?;
        // Known, but its file is gone (a database restored without its media, a media dir
        // emptied or replaced): a sync that reaches it puts it back -- from a cache tier
        // that still has it, else from the provider -- instead of counting it as held.
        if !path.is_file() {
            let restored = match super::super::media_cache::cached_full(pool, &roots, media_id).await? {
                Some(cached) => tokio::fs::read(&cached).await?,
                None => {
                    let fetched = match prefetched {
                        Some((bytes, _)) => bytes,
                        None => media_store::read_capped(http.get(url).send().await?.error_for_status()?, media_store::RootRole::for_media_type(media_type).max_bytes()).await?,
                    };
                    sqlx::query("UPDATE media SET content_sha1 = ?, bytes = ? WHERE id = ?").bind(media_store::sha1_bytes(&fetched)).bind(fetched.len() as u64).bind(media_id).execute(pool).await?;
                    fetched
                }
            };
            media_store::write_file(&path, &restored).await?;
            return Ok((MediaRecord { media_id, path, bytes: restored.len() as u64, fresh: true }, linked));
        }
        let bytes = row.try_get::<Option<u64>, _>("bytes")?.unwrap_or_else(|| std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0));
        return Ok((MediaRecord { media_id, path, bytes, fresh: false }, linked));
    }
    // Already on disk at the slot's own name (an earlier install's media folder under a new
    // database): take that file rather than download it again (`media_store::adopt_existing`).
    if super::super::settings::media_reuse_existing(pool).await? {
        let exts: Vec<&str> = match media_store::known_extension(url) {
            Some(ext) => vec![ext],
            None => media_store::role_extensions(placement.role).to_vec(),
        };
        let meta = NewMedia { media_type, sub_category, source, provider_id, source_url: Some(url), ext: exts[0], own_row: false, on_existing: media_store::OnExisting::ReplaceRetired };
        if let Some(adopted) = media_store::adopt_existing(pool, &roots, &placement, &meta, &exts).await? {
            let linked = media_store::link_placed(pool, adopted.media_id, targets, group.as_ref(), media_type, sub_category).await?;
            return Ok((MediaRecord { media_id: adopted.media_id, path: adopted.path, bytes: adopted.bytes, fresh: false }, linked));
        }
    }
    let (bytes, content_type) = match prefetched {
        Some((bytes, content_type)) => (bytes, content_type),
        None => {
            let response = http.get(url).send().await?.error_for_status()?;
            let content_type = response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_string);
            (media_store::read_capped(response, media_store::RootRole::for_media_type(media_type).max_bytes()).await?, content_type)
        }
    };
    let ext = media_store::extension_for(url, content_type.as_deref());
    let meta = NewMedia { media_type, sub_category, source, provider_id, source_url: Some(url), ext, own_row: false, on_existing: media_store::OnExisting::ReplaceRetired };
    let stored = media_store::store_bytes(pool, &roots, &placement, &meta, &bytes).await?;
    let linked = media_store::link_placed(pool, stored.media_id, targets, group.as_ref(), media_type, sub_category).await?;
    Ok((MediaRecord { media_id: stored.media_id, path: stored.path, bytes: stored.bytes, fresh: stored.fresh }, linked))
}

/// Downloads `url` (unless a media row for it already exists) and links it to `target`.
pub async fn download_media(
    http: &reqwest::Client,
    pool: &MySqlPool,
    media_dir: &Path,
    provider: Provider,
    provider_id: Option<&str>,
    url: &str,
    media_type: &str,
    sub_category: Option<&str>,
    target: MediaTarget,
) -> Result<MediaRecord, DatabaseError> {
    Ok(store_url(http, pool, media_dir, provider.name(), provider_id, url, media_type, sub_category, &[target]).await?.0)
}

/// IGDB cover URL (big size) for a game's `cover` id.
pub async fn igdb_cover_url(clients: &ProviderClients, cover_id: i32) -> Result<Option<String>, DatabaseError> {
    let client = clients.igdb.as_ref().ok_or_else(|| DatabaseError::Provider("igdb client unavailable".to_string()))?;
    let Some(cover) = client.get_cover_by_id(cover_id).await? else { return Ok(None) };
    let url = cover.url.replace("t_thumb", "t_cover_big");
    Ok(Some(if url.starts_with("//") { format!("https:{url}") } else { url }))
}

/// IGDB cover URLs (big size) for many games' `cover` ids in list requests of 100:
/// cover id -> URL.
pub async fn igdb_cover_urls(clients: &ProviderClients, games: &[playmatch_deredis::providers::igdb::model::Game]) -> Result<std::collections::HashMap<i32, String>, DatabaseError> {
    let client = clients.igdb.as_ref().ok_or_else(|| DatabaseError::Provider("igdb client unavailable".to_string()))?;
    let mut ids: Vec<i32> = games.iter().filter_map(|g| g.cover).collect();
    ids.sort_unstable();
    ids.dedup();
    let mut out = std::collections::HashMap::with_capacity(ids.len());
    for chunk in ids.chunks(100) {
        for cover in client.get_covers_by_id(chunk.to_vec()).await? {
            let url = cover.url.replace("t_thumb", "t_cover_big");
            out.insert(cover.id, if url.starts_with("//") { format!("https:{url}") } else { url });
        }
    }
    Ok(out)
}

/// An IGDB image URL at `size` (`t_screenshot_huge`, `t_1080p`, ...): the API hands out the
/// thumbnail's, scheme-less.
fn igdb_image_url(url: &str, size: &str) -> String {
    let url = url.replace("t_thumb", size);
    if url.starts_with("//") {
        format!("https:{url}")
    } else {
        url
    }
}

/// IGDB screenshots and artworks (stored as `fanart`) for the titles a sync just linked,
/// where `media_download` asks for them. Image ids become URLs in list requests of 100, and
/// only for titles that have none of the type from IGDB yet, so a repeated sync asks nothing.
pub async fn attach_igdb_extras(
    http: &reqwest::Client,
    pool: &MySqlPool,
    media_dir: &Path,
    clients: &ProviderClients,
    linked: &[(u64, &playmatch_deredis::providers::igdb::model::Game)],
) -> Result<u32, DatabaseError> {
    let mut targeted = Vec::with_capacity(linked.len());
    for (title_id, game) in linked {
        targeted.push((release_targets(pool, *title_id, Provider::Igdb, &game.id.to_string()).await?, *game));
    }
    attach_igdb_extras_to(http, pool, media_dir, clients, &Ask::Auto, &targeted).await
}

/// [`attach_igdb_extras`] for games whose targets are already known.
pub async fn attach_igdb_extras_to(
    http: &reqwest::Client,
    pool: &MySqlPool,
    media_dir: &Path,
    clients: &ProviderClients,
    ask: &Ask,
    linked: &[(Vec<MediaTarget>, &playmatch_deredis::providers::igdb::model::Game)],
) -> Result<u32, DatabaseError> {
    let rules = super::super::settings::MediaRules::load(pool).await?;
    let cap = super::super::settings::media_list_cap(pool).await?;
    let mut total = 0;
    for (media_type, size) in [("screenshot", "t_screenshot_huge"), ("fanart", "t_1080p")] {
        if !ask.allows(&rules, Provider::Igdb.name(), media_type, None) {
            continue;
        }
        let mut wanted = Vec::new();
        for (targets, game) in linked {
            let ids: Vec<i32> = if media_type == "screenshot" { &game.screenshots } else { &game.artworks }.as_deref().unwrap_or_default().iter().take(cap).copied().collect();
            if ids.is_empty() || !any_wanting_as(pool, &rules, ask, Provider::Igdb, media_type, None, targets, ids.len()).await? {
                continue;
            }
            wanted.push((targets, *game, ids));
        }
        if wanted.is_empty() {
            continue;
        }
        let client = clients.igdb.as_ref().ok_or_else(|| DatabaseError::Provider("igdb client unavailable".to_string()))?;
        let mut all: Vec<i32> = wanted.iter().flat_map(|(_, _, ids)| ids.iter().copied()).collect();
        all.sort_unstable();
        all.dedup();
        let mut urls = std::collections::HashMap::with_capacity(all.len());
        for chunk in all.chunks(100) {
            if media_type == "screenshot" {
                urls.extend(client.get_screenshots_by_id(chunk.to_vec()).await?.into_iter().map(|s| (s.id, igdb_image_url(&s.url, size))));
            } else {
                urls.extend(client.get_artworks_by_id(chunk.to_vec()).await?.into_iter().map(|a| (a.id, igdb_image_url(&a.url, size))));
            }
        }
        for (targets, game, ids) in &wanted {
            let list: Vec<String> = ids.iter().filter_map(|id| urls.get(id).cloned()).collect();
            total += attach_media_list_as(http, pool, media_dir, ask, Provider::Igdb, Some(&game.id.to_string()), &list, media_type, targets).await?;
        }
    }
    Ok(total)
}

/// SteamGridDB heroes (stored as `fanart`), a logo and an icon for a game, where
/// `media_download` asks for them and the releases have none from SteamGridDB yet. One
/// request per type that is wanted.
pub async fn attach_steamgriddb_extras(http: &reqwest::Client, pool: &MySqlPool, media_dir: &Path, clients: &ProviderClients, ask: &Ask, game_id: i64, targets: &[MediaTarget]) -> Result<u32, DatabaseError> {
    if targets.is_empty() {
        return Ok(0);
    }
    let rules = super::super::settings::MediaRules::load(pool).await?;
    let provider = Provider::SteamGridDb;
    let id = game_id.to_string();
    let mut total = 0;
    for media_type in ["fanart", "logo", "icon"] {
        let cap = super::super::settings::media_list_cap(pool).await?;
        if !any_wanting_as(pool, &rules, ask, provider, media_type, None, targets, cap).await? {
            continue;
        }
        let client = clients.steamgriddb.as_ref().ok_or_else(|| DatabaseError::Provider("steamgriddb client unavailable".to_string()))?;
        let limit = if media_type == "fanart" { cap as u32 } else { 1 };
        let filters = AssetFilters { limit: Some(limit), ..Default::default() };
        let assets = match media_type {
            "fanart" => client.get_heroes_by_game(game_id, &filters).await?,
            "logo" => client.get_logos_by_game(game_id, &filters).await?,
            _ => client.get_icons_by_game(game_id, &filters).await?,
        };
        if media_type == "fanart" {
            let urls: Vec<String> = assets.iter().map(|a| a.url.clone()).collect();
            total += attach_media_list_as(http, pool, media_dir, ask, provider, Some(&id), &urls, media_type, targets).await?;
        } else if let Some(asset) = assets.first() {
            total += attach_media_as(http, pool, media_dir, ask, provider, Some(&id), &asset.url, media_type, None, targets).await?;
        }
    }
    Ok(total)
}

/// An RA image path (`/Images/...`) as a URL; `None` for none and for RA's placeholder.
pub fn retroachievements_image_url(path: Option<&str>) -> Option<String> {
    path.filter(|p| !p.is_empty() && !p.ends_with("000002.png")).map(RaClient::media_url)
}

/// First SteamGridDB grid for a game.
pub async fn steamgriddb_grid_url(clients: &ProviderClients, game_id: i64) -> Result<Option<String>, DatabaseError> {
    let client = clients.steamgriddb.as_ref().ok_or_else(|| DatabaseError::Provider("steamgriddb client unavailable".to_string()))?;
    let filters = AssetFilters { limit: Some(1), ..Default::default() };
    let grids = client.get_grids_by_game(game_id, &filters).await?;
    Ok(grids.first().map(|g| g.url.clone()))
}

/// RA box art URL for a game.
pub fn retroachievements_boxart_url(game: &RaGame) -> Option<String> {
    game.image_box_art.as_deref().filter(|p| !p.is_empty() && !p.ends_with("000002.png")).map(RaClient::media_url)
}

/// MobyGames sample cover for a game id (from the cached search result's `sample_cover`).
pub async fn mobygames_cover_url(clients: &ProviderClients, game_id: i64) -> Result<Option<String>, DatabaseError> {
    let client = clients.mobygames.as_ref().ok_or_else(|| DatabaseError::Provider("mobygames client unavailable".to_string()))?;
    let Some(game) = client.get_game_by_id(game_id).await? else { return Ok(None) };
    Ok(game.sample_cover.map(|c| c.image))
}

/// The media types each provider's art comes as, with the sub-category a one-per-game type
/// is stored under. Providers absent here have no art the app downloads.
pub fn provider_media(provider: Provider) -> &'static [(&'static str, Option<&'static str>)] {
    match provider {
        Provider::Igdb => &[("box", Some("front")), ("screenshot", None), ("fanart", None)],
        Provider::SteamGridDb => &[("cover", None), ("fanart", None), ("logo", None), ("icon", None)],
        Provider::MobyGames => &[("box", Some("front"))],
        Provider::RetroAchievements => &[("box", Some("front")), ("title_screen", None), ("screenshot", None)],
        Provider::TheGamesDb => &[("box", Some("front")), ("box", Some("back")), ("title_screen", None), ("screenshot", None), ("logo", None), ("fanart", None)],
        _ => &[],
    }
}

/// What [`attach_for`] came to for one provider entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attached {
    /// Every target already has what the settings ask for from this provider.
    Already,
    /// Links made (0 when the provider simply has none of the art wanted).
    Linked(u32),
    /// Something is wanted, but the provider has no usable credentials and nothing cached.
    Unavailable,
    /// The provider has no art the app downloads.
    Unsupported,
}

/// Cached provider responses are read regardless of age here: art URLs do not go stale the
/// way metadata does, and re-asking the provider for each one would defeat the point.
const ANY_AGE_DAYS: u32 = 36_500;

/// Every media type `media_download` enables for `provider`, for the targets of one of its
/// entries (`provider_id`), that those targets don't have yet. The one place the per-provider
/// art lookup lives: a page open, a provider query and the "download missing media" job all
/// come here. Nothing is asked of the provider unless some target wants something, and a
/// cached response is used before a request.
#[allow(clippy::too_many_arguments)]
pub async fn attach_for(
    http: &reqwest::Client,
    pool: &MySqlPool,
    media_dir: &Path,
    clients: &ProviderClients,
    store: &super::raw_store::RawStore,
    provider: Provider,
    provider_id: &str,
    targets: &[MediaTarget],
    ask: &Ask,
) -> Result<Attached, DatabaseError> {
    let types = provider_media(provider);
    // A provider whose art costs a scarce allowance (TheGamesDB) is only ever asked by hand.
    if types.is_empty() || (provider.media_manual_only() && !ask.is_asked()) {
        return Ok(Attached::Unsupported);
    }
    let rules = super::super::settings::MediaRules::load(pool).await?;
    let cap = super::super::settings::media_list_cap(pool).await?;
    let mut needed: Vec<&str> = Vec::new();
    for (media_type, sub) in types {
        // RetroAchievements has one in-game shot per game, however high the cap.
        let list_n = if provider == Provider::RetroAchievements { 1 } else { cap };
        if any_wanting_as(pool, &rules, ask, provider, media_type, *sub, targets, list_n).await? {
            needed.push(media_type);
        }
    }
    // IGDB's localized covers go beside the main one, so a release that has the main cover
    // still gets its region's: looked at before "everything is here".
    let mut localized: Option<u32> = None;
    if provider == Provider::Igdb {
        if let Ok(id) = provider_id.parse::<i32>() {
            if let Some(game) = super::cache::load_cached_igdb_game(pool, id).await? {
                localized = Some(super::igdb_localized::attach(http, pool, media_dir, clients, store.root(), ask, &[(targets.to_vec(), &game)]).await?);
            }
        }
    }
    if needed.is_empty() {
        return Ok(match localized {
            Some(n) if n > 0 => Attached::Linked(n),
            _ => Attached::Already,
        });
    }
    let wanted = |t: &str| needed.contains(&t);
    let mut linked = localized.unwrap_or(0);
    match provider {
        Provider::Igdb => {
            let Ok(id) = provider_id.parse::<i32>() else { return Ok(Attached::Linked(0)) };
            if !clients.is_available(provider) {
                return Ok(Attached::Unavailable);
            }
            let game = match super::cache::load_cached_igdb_game(pool, id).await? {
                Some(game) => game,
                None => match super::cache::igdb_games_by_ids_full(clients, pool, store, &[id]).await?.into_iter().next() {
                    Some((_, game)) => game,
                    None => return Ok(Attached::Linked(linked)),
                },
            };
            if localized.is_none() {
                linked += super::igdb_localized::attach(http, pool, media_dir, clients, store.root(), ask, &[(targets.to_vec(), &game)]).await?;
            }
            if wanted("box") {
                if let Some(url) = match game.cover {
                    Some(cover) => igdb_cover_url(clients, cover).await?,
                    None => None,
                } {
                    linked += attach_media_as(http, pool, media_dir, ask, provider, Some(provider_id), &url, "box", Some("front"), targets).await?;
                }
            }
            if wanted("screenshot") || wanted("fanart") {
                linked += attach_igdb_extras_to(http, pool, media_dir, clients, ask, &[(targets.to_vec(), &game)]).await?;
            }
        }
        Provider::SteamGridDb => {
            let Ok(id) = provider_id.parse::<i64>() else { return Ok(Attached::Linked(0)) };
            if !clients.is_available(provider) {
                return Ok(Attached::Unavailable);
            }
            if wanted("cover") {
                if let Some(url) = steamgriddb_grid_url(clients, id).await? {
                    linked += attach_media_as(http, pool, media_dir, ask, provider, Some(provider_id), &url, "cover", None, targets).await?;
                }
            }
            linked += attach_steamgriddb_extras(http, pool, media_dir, clients, ask, id, targets).await?;
        }
        Provider::MobyGames => {
            let Ok(id) = provider_id.parse::<i64>() else { return Ok(Attached::Linked(0)) };
            let cached = super::cache::load_cached(pool, provider, provider_id, ANY_AGE_DAYS).await?;
            let game = match cached {
                Some(game) => Some(game),
                None => {
                    let Some(client) = clients.mobygames.as_ref() else { return Ok(Attached::Unavailable) };
                    let game = client.get_game_by_id(id).await?;
                    if let Some(game) = &game {
                        super::cache::upsert_game(pool, store, provider, provider_id, Some(&game.title), &serde_json::to_value(game)?).await?;
                    }
                    game
                }
            };
            if let Some(url) = game.and_then(|g| g.sample_cover).map(|c| c.image) {
                linked += attach_media_as(http, pool, media_dir, ask, provider, Some(provider_id), &url, "box", Some("front"), targets).await?;
            }
        }
        Provider::RetroAchievements => {
            let Ok(id) = provider_id.parse::<u32>() else { return Ok(Attached::Linked(0)) };
            let game: RaGame = match super::cache::load_cached(pool, provider, provider_id, ANY_AGE_DAYS).await? {
                Some(game) => game,
                None => {
                    if !clients.is_available(provider) {
                        return Ok(Attached::Unavailable);
                    }
                    super::cache::retroachievements_game(clients, pool, store, id).await?.1
                }
            };
            if let Some(url) = retroachievements_boxart_url(&game) {
                linked += attach_media_as(http, pool, media_dir, ask, provider, Some(provider_id), &url, "box", Some("front"), targets).await?;
            }
            if let Some(url) = retroachievements_image_url(game.image_title.as_deref()) {
                linked += attach_media_as(http, pool, media_dir, ask, provider, Some(provider_id), &url, "title_screen", None, targets).await?;
            }
            if let Some(url) = retroachievements_image_url(game.image_ingame.as_deref()) {
                linked += attach_media_list_as(http, pool, media_dir, ask, provider, Some(provider_id), &[url], "screenshot", targets).await?;
            }
        }
        Provider::TheGamesDb => {
            let Ok(id) = provider_id.parse::<i64>() else { return Ok(Attached::Linked(0)) };
            let kinds = super::thegamesdb_media::tgdb_types(&needed);
            let Some(found) = super::thegamesdb_media::images_for(clients, pool, store.root(), id, &kinds).await? else { return Ok(Attached::Unavailable) };
            // One release group per call: an entry linked to releases of two groups (USA and
            // USA,Europe) would otherwise file its box under `common/`, not the region's.
            let groups = split_by_group(pool, targets).await?;
            let mut lists: std::collections::BTreeMap<&str, Vec<String>> = std::collections::BTreeMap::new();
            let mut singles_done: Vec<(&str, Option<&str>)> = Vec::new();
            for image in &found.images {
                let Some((media_type, sub)) = super::thegamesdb_media::slot_of(image) else { continue };
                if !wanted(media_type) {
                    continue;
                }
                if super::super::settings::SINGLE_MEDIA_TYPES.contains(&media_type) {
                    // The first of each slot (the lowest image id); TheGamesDB lists the
                    // original upload first.
                    if singles_done.contains(&(media_type, sub)) {
                        continue;
                    }
                    singles_done.push((media_type, sub));
                    for group in &groups {
                        linked += attach_media_as(http, pool, media_dir, ask, provider, Some(provider_id), &found.url(image), media_type, sub, group).await?;
                    }
                } else {
                    lists.entry(media_type).or_default().push(found.url(image));
                }
            }
            for (media_type, urls) in lists {
                linked += attach_media_list_as(http, pool, media_dir, ask, provider, Some(provider_id), &urls, media_type, targets).await?;
            }
        }
        _ => return Ok(Attached::Unsupported),
    }
    Ok(Attached::Linked(linked))
}

/// `targets` (releases) in their release groups, each group's in one list: art from a
/// release-level entry is filed under the group's region only when all of a call's targets
/// are in that one group (`media_store::placement_for`). Anything not a release, or with no
/// group, goes on its own.
pub(super) async fn split_by_group(pool: &MySqlPool, targets: &[MediaTarget]) -> Result<Vec<Vec<MediaTarget>>, DatabaseError> {
    let mut groups: Vec<(Vec<u64>, Vec<MediaTarget>)> = Vec::new();
    for target in targets {
        let MediaTarget::Release(id) = target else {
            groups.push((Vec::new(), vec![*target]));
            continue;
        };
        if let Some((_, list)) = groups.iter_mut().find(|(members, _)| members.contains(id)) {
            list.push(*target);
            continue;
        }
        let members = media_store::release_group(pool, *id).await?.map(|g| g.members).unwrap_or_default();
        groups.push((members, vec![*target]));
    }
    Ok(groups.into_iter().map(|(_, list)| list).collect())
}
