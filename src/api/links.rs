//! Manual matching: search a provider by name, link/unlink a provider entry to a title or
//! release by hand (`confidence = manual`), and let the worker pull that entry's metadata
//! and art afterwards. The search runs in the calling process through the provider's
//! rate-limited client -- one interactive lookup, not a sync.

use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::common::models::Platform;
use crate::database::jobs as db_jobs;
use crate::database::providers::cache::{self, Confidence};
use crate::database::providers::raw_store::RawStore;
use crate::database::providers::{Provider, Tier};
use crate::database::settings;

use super::jobs::{JobRequest, ProviderQueryRequest};
use super::{App, Error, Result};

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub provider: String,
    pub provider_id: String,
    pub title: Option<String>,
    pub url: Option<String>,
    /// The platform(s) the provider lists this entry on, when known (local search).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LinkSpec {
    pub provider: String,
    pub provider_id: String,
}

fn provider_of(name: &str) -> Result<Provider> {
    Provider::from_name(name).ok_or_else(|| Error::invalid(format!("unknown provider {name:?}")))
}

/// Providers a name search exists for (the vendored clients' search endpoints).
pub fn searchable(provider: Provider) -> bool {
    matches!(provider, Provider::Igdb | Provider::TheGamesDb | Provider::SteamGridDb | Provider::MobyGames | Provider::EmuReady | Provider::ScreenScraper)
}

pub async fn search(app: &App, provider: &str, name: &str, platform: Option<u16>) -> Result<Vec<SearchHit>> {
    let provider = provider_of(provider)?;
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::invalid("name is required"));
    }
    if !searchable(provider) {
        return Err(Error::invalid(format!("{} has no name search; link it by id", provider.name())));
    }
    if !app.clients().is_available(provider) {
        return Err(Error::unavailable(format!("provider {} has no usable credentials", provider.name())));
    }
    let platform = platform.map(|id| Platform::from_igdb_id(id).ok_or_else(|| Error::invalid(format!("unknown platform id {id}")))).transpose()?;
    let store = RawStore::new(app.paths.provider_cache_dir(), settings::raw_retention(&app.pool).await?);
    let entries = cache::search_by_name(&app.clients(), &app.pool, &store, provider, name, platform).await?;
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        let url = super::catalog::provider_page_url(&app.pool, provider.name(), &e.provider_id, e.title.as_deref().unwrap_or(name)).await;
        out.push(SearchHit { provider: provider.name().to_string(), provider_id: e.provider_id, title: e.title, url, platform: None });
    }
    Ok(out)
}

/// Providers with a title-bearing cache table a local search can read (Hasheous and
/// PlayMatch cache per-content lookups, not a catalog).
pub fn locally_searchable(provider: Provider) -> bool {
    !matches!(provider, Provider::Hasheous | Provider::PlayMatch)
}

/// Searches the provider's locally cached catalog (what bulk syncs and earlier lookups
/// stored) by title -- no network, works for every provider with a cache table, IGDB
/// included. Exact title first, then prefix, then substring; at most 50 hits. Each hit
/// carries the provider's platform where the cache knows it (TheGamesDB: its own platform
/// name; RetroAchievements: the synced console's app platform; IGDB: from the cached game),
/// and `platform` (an app platform id) narrows TheGamesDB and RetroAchievements hits to it.
pub async fn search_local(app: &App, provider: &str, name: &str, platform: Option<u16>) -> Result<Vec<SearchHit>> {
    let provider = provider_of(provider)?;
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::invalid("name is required"));
    }
    if !locally_searchable(provider) {
        return Err(Error::invalid(format!("{} keeps no local catalog to search", provider.name())));
    }
    // (platform column, extra join, platform filter)
    let (platform_col, join, filter) = match provider {
        Provider::TheGamesDb => ("g.platform_name", "", "g.app_platform_id = ?"),
        Provider::RetroAchievements => (
            "(SELECT GROUP_CONCAT(DISTINCT p.name SEPARATOR ', ') FROM provider_sync_state s JOIN platforms p ON p.id = s.app_platform_id \
              WHERE s.provider = 'retroachievements' AND s.app_platform_id <> 0 AND s.remote_id = CAST(g.console_id AS CHAR))",
            "",
            "EXISTS (SELECT 1 FROM provider_sync_state s WHERE s.provider = 'retroachievements' AND s.app_platform_id = ? AND s.remote_id = CAST(g.console_id AS CHAR))",
        ),
        _ => ("NULL", "", ""),
    };
    let filter = if platform.is_some() && !filter.is_empty() { format!("AND {filter}") } else { String::new() };
    let sql = format!(
        "SELECT CAST(g.provider_id AS CHAR) AS provider_id, g.title, {platform_col} AS platform, g.raw_response_path FROM {} g {join} \
         WHERE g.deleted = FALSE AND g.title LIKE ? {filter} \
         ORDER BY (g.title = ?) DESC, (g.title LIKE ?) DESC, CHAR_LENGTH(g.title), g.provider_id LIMIT 50",
        provider.cache_table()
    );
    let prefix = format!("{}%", super::like_pattern(name).trim_start_matches('%'));
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(super::like_pattern(name));
    if !filter.is_empty() {
        query = query.bind(platform);
    }
    let rows = query.bind(name).bind(prefix).fetch_all(&app.pool).await?;
    let igdb_names = if provider == Provider::Igdb { igdb_platform_names(&app.pool).await? } else { std::collections::HashMap::new() };
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let provider_id: String = r.try_get("provider_id")?;
        let title: Option<String> = r.try_get("title")?;
        let mut platform: Option<String> = r.try_get("platform")?;
        if provider == Provider::Igdb {
            let raw: Option<String> = r.try_get("raw_response_path")?;
            platform = raw.and_then(|p| igdb_cached_platforms(&p, &provider_id, &igdb_names));
        }
        let url = super::catalog::provider_page_url(&app.pool, provider.name(), &provider_id, title.as_deref().unwrap_or(name)).await;
        out.push(SearchHit { provider: provider.name().to_string(), provider_id, title, url, platform });
    }
    Ok(out)
}

/// IGDB platform id -> app platform name (`platforms.igdb_id`).
async fn igdb_platform_names(pool: &sqlx::MySqlPool) -> Result<std::collections::HashMap<i64, String>> {
    let rows = sqlx::query("SELECT igdb_id, name FROM platforms WHERE igdb_id IS NOT NULL").fetch_all(pool).await?;
    Ok(rows.iter().filter_map(|r| Some((r.try_get::<u16, _>("igdb_id").ok()? as i64, r.try_get::<String, _>("name").ok()?))).collect())
}

/// The platforms an IGDB game lists, read from its cached raw response (one game or a
/// page of games); unknown IGDB platforms show as `IGDB platform <id>`.
fn igdb_cached_platforms(path: &str, provider_id: &str, names: &std::collections::HashMap<i64, String>) -> Option<String> {
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let wanted: i64 = provider_id.parse().ok()?;
    let entries: Vec<&serde_json::Value> = match &json {
        serde_json::Value::Array(a) => a.iter().collect(),
        other => vec![other],
    };
    let game = entries.into_iter().find(|e| e.get("id").and_then(|v| v.as_i64()) == Some(wanted))?;
    let list: Vec<String> = game.get("platforms")?.as_array()?.iter().filter_map(|p| p.as_i64().or_else(|| p.get("id").and_then(|v| v.as_i64()))).map(|id| names.get(&id).cloned().unwrap_or_else(|| format!("IGDB platform {id}"))).collect();
    (!list.is_empty()).then(|| list.join(", "))
}

/// The id part of a provider page URL, without network access. `None` for a URL no known
/// provider page pattern matches; IGDB game pages carry a slug instead (see `resolve_url`).
fn parse_page_url(url: &str) -> Option<(Provider, UrlId)> {
    let url = url.trim();
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let path = path.split('#').next().unwrap_or("");
    let query = query.split('#').next().unwrap_or("");
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let param = |key: &str| query.split('&').find_map(|kv| kv.split_once('=').filter(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.to_string()));
    let numeric = |s: &str| (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.to_string());
    let after = |key: &str| segs.iter().position(|s| s.eq_ignore_ascii_case(key)).and_then(|i| segs.get(i + 1)).copied();
    match host {
        "igdb.com" => after("games").map(|slug| (Provider::Igdb, UrlId::Slug(slug.to_string()))),
        "thegamesdb.net" => param("id").and_then(|v| numeric(&v)).map(|id| (Provider::TheGamesDb, UrlId::Id(id))),
        "retroachievements.org" => after("game").and_then(numeric).map(|id| (Provider::RetroAchievements, UrlId::Id(id))),
        "steamgriddb.com" => after("game").and_then(numeric).map(|id| (Provider::SteamGridDb, UrlId::Id(id))),
        "mobygames.com" => after("game").and_then(numeric).map(|id| (Provider::MobyGames, UrlId::Id(id))),
        "screenscraper.fr" => param("gameid").and_then(|v| numeric(&v)).map(|id| (Provider::ScreenScraper, UrlId::Id(id))),
        "emuready.com" => after("games").filter(|s| s.len() <= 64).map(|id| (Provider::EmuReady, UrlId::Id(id.to_string()))),
        "hasheous.org" => param("id").and_then(|v| numeric(&v)).map(|id| (Provider::Hasheous, UrlId::Id(id))),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum UrlId {
    Id(String),
    Slug(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedUrl {
    pub provider: String,
    pub provider_id: String,
    pub title: Option<String>,
    pub url: Option<String>,
}

/// A pasted provider page URL -> (provider, id), ready for `link_title` / `link_release`.
/// An IGDB `/games/<slug>` page is resolved through the IGDB API (one request).
pub async fn resolve_url(app: &App, url: &str) -> Result<ResolvedUrl> {
    let (provider, id) = parse_page_url(url).ok_or_else(|| Error::invalid("not a recognized provider game page URL (IGDB, TheGamesDB, RetroAchievements, SteamGridDB, MobyGames, ScreenScraper, EmuReady, Hasheous)"))?;
    let provider_id = match id {
        UrlId::Id(id) => id,
        UrlId::Slug(slug) => {
            let clients = app.clients();
            let client = clients.igdb.as_ref().ok_or_else(|| Error::unavailable("IGDB has no usable credentials; paste the numeric id instead"))?;
            let game = client.get_game_by_slug(&slug).await.map_err(|e| Error::unavailable(format!("IGDB: {e}")))?;
            game.ok_or_else(|| Error::not_found(format!("IGDB has no game with slug {slug:?}")))?.id.to_string()
        }
    };
    let title: Option<String> = if locally_searchable(provider) {
        let sql = format!("SELECT title FROM {} WHERE CAST(provider_id AS CHAR) = ? AND deleted = FALSE LIMIT 1", provider.cache_table());
        sqlx::query(sqlx::AssertSqlSafe(sql)).bind(&provider_id).fetch_optional(&app.pool).await?.and_then(|r| r.try_get("title").ok())
    } else {
        None
    };
    let page = super::catalog::provider_page_url(&app.pool, provider.name(), &provider_id, title.as_deref().unwrap_or("")).await;
    Ok(ResolvedUrl { provider: provider.name().to_string(), provider_id, title, url: page })
}

/// Providers whose by-id fetch yields metadata and/or art worth a job.
fn fetchable(provider: Provider) -> bool {
    matches!(provider, Provider::Igdb | Provider::RetroAchievements | Provider::SteamGridDb | Provider::MobyGames | Provider::ScreenScraper)
}

/// Whether the title (or one of its releases) is linked to `provider`.
///
/// For a title-tier provider (IGDB, SteamGridDB, MobyGames) a DLC, update, hack or bonus demo
/// under the title does not count: its link is its own (`cache::APART_SQL`). Counting it left
/// a title that had lost its own link never looked up again.
pub async fn title_has_provider(pool: &sqlx::MySqlPool, title_id: u64, provider: Provider) -> Result<bool> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT 1 FROM title_provider_links WHERE title_id = ? AND provider = ? AND deleted = FALSE \
         UNION SELECT 1 FROM release_provider_links rpl JOIN title_releases tr ON tr.release_id = rpl.release_id JOIN `release` r ON r.id = rpl.release_id \
         WHERE tr.title_id = ? AND rpl.provider = ? AND rpl.deleted = FALSE AND (? OR NOT {}) LIMIT 1",
        cache::APART_SQL
    )))
    .bind(title_id)
    .bind(provider.name())
    .bind(title_id)
    .bind(provider.name())
    .bind(provider.tier() != Tier::Title)
    .fetch_optional(pool)
    .await?;
    Ok(row.is_some())
}

/// A pending/running job with exactly this type and payload already exists.
async fn already_queued(pool: &sqlx::MySqlPool, request: &JobRequest) -> Result<bool> {
    let row = sqlx::query("SELECT 1 FROM jobs WHERE job_type = ? AND status IN ('pending', 'in_progress') AND payload = ? LIMIT 1").bind(request.job_type().name()).bind(request.payload()).fetch_optional(pool).await?;
    Ok(row.is_some())
}

/// After a manual link: fetch that entry (metadata + art) through the provider's lane,
/// at interactive priority (the user is looking at the page).
async fn enqueue_fetch(app: &App, provider: Provider, provider_id: &str) -> Result<()> {
    if !fetchable(provider) || !app.clients().is_available(provider) {
        return Ok(());
    }
    let request = JobRequest::ProviderQuery(ProviderQueryRequest { provider: provider.name().to_string(), name: None, content_id: None, provider_id: None, provider_ids: Some(vec![provider_id.to_string()]), title_ids: None, platform: None });
    db_jobs::enqueue_with_priority(&app.pool, request.job_type(), request.payload(), super::jobs::PRIORITY_INTERACTIVE).await?;
    Ok(())
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RefreshSummary {
    pub title_id: u64,
    /// Jobs enqueued (interactive priority): by-id fetches for linked providers, by-name
    /// lookups for searchable providers with no link yet.
    pub jobs: Vec<u64>,
    /// Providers whose refresh was already queued or running.
    pub already_queued: Vec<String>,
}

/// The "fast lane" for a page the user just opened: queue, at interactive priority, a by-id
/// fetch for every provider the title is linked to (metadata + art, cached ids are re-read
/// cheaply) and a by-name lookup for every searchable provider it has no link to. Nothing
/// is queued twice while a same job is pending.
pub async fn refresh_title(app: &App, title_id: u64) -> Result<RefreshSummary> {
    let pool = &app.pool;
    sqlx::query("SELECT 1 FROM title WHERE id = ? AND deleted = FALSE").bind(title_id).fetch_optional(pool).await?.ok_or_else(|| Error::not_found(format!("no title {title_id}")))?;
    let mut summary = RefreshSummary { title_id, ..Default::default() };
    // `apart`: a DLC, update, hack or bonus demo's own link. Its entry is still fetched, but it
    // does not make the title linked to a title-tier provider (see `title_has_provider`).
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT provider, provider_id, 0 AS apart FROM title_provider_links WHERE title_id = ? AND deleted = FALSE \
         UNION SELECT rpl.provider, rpl.provider_id, {} AS apart FROM release_provider_links rpl JOIN title_releases tr ON tr.release_id = rpl.release_id \
         JOIN `release` r ON r.id = rpl.release_id WHERE tr.title_id = ? AND rpl.deleted = FALSE",
        cache::APART_SQL
    )))
    .bind(title_id)
    .bind(title_id)
    .fetch_all(pool)
    .await?;
    let mut linked: Vec<Provider> = Vec::new();
    let mut fetched: Vec<(Provider, String)> = Vec::new();
    for r in rows {
        let name: String = r.try_get("provider")?;
        let id: String = r.try_get("provider_id")?;
        let apart: i64 = r.try_get("apart")?;
        let Some(provider) = Provider::from_name(&name) else { continue };
        if apart == 0 || provider.tier() != Tier::Title {
            linked.push(provider);
        }
        // The same entry through the title and through its releases: fetched once.
        if fetched.contains(&(provider, id.clone())) {
            continue;
        }
        fetched.push((provider, id.clone()));
        if !fetchable(provider) || !app.clients().is_available(provider) {
            continue;
        }
        let request = JobRequest::ProviderQuery(ProviderQueryRequest { provider: provider.name().to_string(), name: None, content_id: None, provider_id: None, provider_ids: Some(vec![id]), title_ids: None, platform: None });
        if already_queued(pool, &request).await? {
            summary.already_queued.push(provider.name().to_string());
            continue;
        }
        summary.jobs.push(db_jobs::enqueue_with_priority(pool, request.job_type(), request.payload(), super::jobs::PRIORITY_INTERACTIVE).await?);
    }
    for provider in Provider::ALL.into_iter().filter(|p| searchable(*p) && app.clients().is_available(*p) && !linked.contains(p)) {
        let request = JobRequest::ProviderQuery(ProviderQueryRequest { provider: provider.name().to_string(), name: None, content_id: None, provider_id: None, provider_ids: None, title_ids: Some(vec![title_id]), platform: None });
        if already_queued(pool, &request).await? {
            summary.already_queued.push(provider.name().to_string());
            continue;
        }
        summary.jobs.push(db_jobs::enqueue_with_priority(pool, request.job_type(), request.payload(), super::jobs::PRIORITY_INTERACTIVE).await?);
    }
    Ok(summary)
}

/// A release page refresh is its title's refresh.
pub async fn refresh_release(app: &App, release_id: u64) -> Result<RefreshSummary> {
    let title_id: u64 = sqlx::query("SELECT title_id FROM title_releases WHERE release_id = ? LIMIT 1").bind(release_id).fetch_optional(&app.pool).await?.ok_or_else(|| Error::not_found(format!("release {release_id} has no title")))?.try_get("title_id")?;
    refresh_title(app, title_id).await
}

fn clean_id(spec: &LinkSpec) -> Result<String> {
    let id = spec.provider_id.trim();
    if id.is_empty() || id.len() > 64 {
        return Err(Error::invalid("provider_id must be 1-64 characters"));
    }
    Ok(id.to_string())
}

/// Links a provider entry to a title. A release-tier provider (RetroAchievements,
/// TheGamesDB) is linked to every release of the title instead -- only when they are all on
/// one platform: an entry of those is one console's game, so a title on several is linked
/// from the release page.
pub async fn link_title(app: &App, title_id: u64, spec: &LinkSpec) -> Result<super::catalog::TitleDetail> {
    let provider = provider_of(&spec.provider)?;
    let provider_id = clean_id(spec)?;
    let pool = &app.pool;
    sqlx::query("SELECT 1 FROM title WHERE id = ? AND deleted = FALSE").bind(title_id).fetch_optional(pool).await?.ok_or_else(|| Error::not_found(format!("no title {title_id}")))?;
    match provider.tier() {
        Tier::Title | Tier::Content => cache::link_title(pool, title_id, provider, &provider_id, Confidence::Manual).await?,
        Tier::Release => {
            let rows = sqlx::query("SELECT tr.release_id, r.app_platform_id FROM title_releases tr JOIN `release` r ON r.id = tr.release_id WHERE tr.title_id = ? AND r.deleted = FALSE").bind(title_id).fetch_all(pool).await?;
            if rows.is_empty() {
                return Err(Error::invalid(format!("{} links to releases and this title has none", provider.name())));
            }
            let platforms: std::collections::HashSet<u16> = rows.iter().filter_map(|r| r.try_get::<u16, _>("app_platform_id").ok()).collect();
            if platforms.len() > 1 {
                return Err(Error::invalid(format!("{} links to one platform's release, and this title has releases on {} platforms: link it from the release's own page", provider.name(), platforms.len())));
            }
            for r in rows {
                cache::link_release(pool, r.try_get("release_id")?, provider, &provider_id, Confidence::Manual).await?;
            }
        }
    }
    enqueue_fetch(app, provider, &provider_id).await?;
    detach_stale_for(app, &[title_id]).await?;
    // A manual IGDB link can show this title is another's regional name: merge now.
    let mut title_id = title_id;
    if provider == Provider::Igdb {
        crate::database::title_merge::dedupe_by_igdb(pool, Some(&provider_id), &crate::database::jobs::Progress::none()).await?;
        let survivor: Option<u64> = sqlx::query_scalar("SELECT survivor_id FROM title_merge_log WHERE merged_title_id = ? AND reason <> 'split' ORDER BY id DESC LIMIT 1").bind(title_id).fetch_optional(pool).await?;
        title_id = survivor.unwrap_or(title_id);
    }
    super::catalog::title(app, title_id).await
}

/// Links a provider entry to a release; a title-tier provider is linked to the release's
/// title (that's where its data attaches).
pub async fn link_release(app: &App, release_id: u64, spec: &LinkSpec) -> Result<super::catalog::ReleaseDetail> {
    let provider = provider_of(&spec.provider)?;
    let provider_id = clean_id(spec)?;
    let pool = &app.pool;
    sqlx::query("SELECT 1 FROM `release` WHERE id = ? AND deleted = FALSE").bind(release_id).fetch_optional(pool).await?.ok_or_else(|| Error::not_found(format!("no release {release_id}")))?;
    match provider.tier() {
        Tier::Release | Tier::Content => cache::link_release(pool, release_id, provider, &provider_id, Confidence::Manual).await?,
        Tier::Title => {
            let title_id: u64 = sqlx::query("SELECT title_id FROM title_releases WHERE release_id = ? LIMIT 1").bind(release_id).fetch_optional(pool).await?.ok_or_else(|| Error::invalid("release has no title to attach a title-tier provider to"))?.try_get("title_id")?;
            cache::link_title(pool, title_id, provider, &provider_id, Confidence::Manual).await?;
        }
    }
    enqueue_fetch(app, provider, &provider_id).await?;
    detach_stale_for(app, &titles_of_release(app, release_id).await?).await?;
    super::catalog::release(app, release_id).await
}

/// The titles a release sits under.
async fn titles_of_release(app: &App, release_id: u64) -> Result<Vec<u64>> {
    Ok(sqlx::query_scalar("SELECT title_id FROM title_releases WHERE release_id = ?").bind(release_id).fetch_all(&app.pool).await?)
}

/// Takes off the art of the game a title or release was linked to before, and queues the
/// download of the right game's (the files already here are reused).
async fn detach_stale_for(app: &App, titles: &[u64]) -> Result<()> {
    if crate::database::providers::media::detach_stale(&app.pool, Some(titles)).await? > 0 {
        let request = JobRequest::MediaBackfill(romexplorer_types::jobs::MediaBackfillRequest { scope: "owned".to_string(), platform: None, ..Default::default() });
        super::jobs::enqueue_unless_queued(app, &request).await?;
    }
    Ok(())
}

pub async fn unlink_title(app: &App, title_id: u64, provider: &str) -> Result<super::catalog::TitleDetail> {
    let provider = provider_of(provider)?;
    sqlx::query("UPDATE title_provider_links SET deleted = TRUE WHERE title_id = ? AND provider = ?").bind(title_id).bind(provider.name()).execute(&app.pool).await?;
    detach_stale_for(app, &[title_id]).await?;
    super::catalog::title(app, title_id).await
}

pub async fn unlink_release(app: &App, release_id: u64, provider: &str) -> Result<super::catalog::ReleaseDetail> {
    let provider = provider_of(provider)?;
    sqlx::query("UPDATE release_provider_links SET deleted = TRUE WHERE release_id = ? AND provider = ?").bind(release_id).bind(provider.name()).execute(&app.pool).await?;
    detach_stale_for(app, &titles_of_release(app, release_id).await?).await?;
    super::catalog::release(app, release_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_urls_parse_to_ids() {
        let id = |u: &str| parse_page_url(u).map(|(p, i)| (p.name(), i));
        assert_eq!(id("https://www.igdb.com/games/hogehoge-adventure"), Some(("igdb", UrlId::Slug("hogehoge-adventure".into()))));
        assert_eq!(id("https://thegamesdb.net/game.php?id=1234"), Some(("thegamesdb", UrlId::Id("1234".into()))));
        assert_eq!(id("retroachievements.org/game/228#comments"), Some(("retroachievements", UrlId::Id("228".into()))));
        assert_eq!(id("https://www.steamgriddb.com/game/5250436"), Some(("steamgriddb", UrlId::Id("5250436".into()))));
        assert_eq!(id("https://www.mobygames.com/game/1044/hogehoge-adventure/"), Some(("mobygames", UrlId::Id("1044".into()))));
        assert_eq!(id("https://www.screenscraper.fr/gameinfos.php?plateforme=23&gameid=3159"), Some(("screenscraper", UrlId::Id("3159".into()))));
        assert_eq!(id("https://www.emuready.com/games/abc-123"), Some(("emuready", UrlId::Id("abc-123".into()))));
        assert_eq!(id("https://example.com/game/1"), None);
        assert_eq!(id("https://retroachievements.org/game/notanid"), None);
    }
}
