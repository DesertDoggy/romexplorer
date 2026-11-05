//! Repairs wrong IGDB links and the title merges they caused. Before the shared-file guard
//! (`cache::SHARED_CONTENT_MAX_TITLES`), one file several discs share -- a demo disc's
//! audio track -- spread one IGDB id to unrelated titles as a `hash` link, and the title
//! dedupe then merged them ("Hoge Quest VII International" and "Stories of Sample" into
//! "Otherfight", all linked to one unrelated music disc).
//!
//! The audit compares names against the IGDB game's own names (its name, alternative names
//! and localized titles, fetched fresh -- the copies on titles were mixed up by the merges):
//! 1. every merge logged `igdb:<id>` whose merged title's name is unrelated gets decoupled;
//! 2. every `hash` / `name` / `fuzzy` IGDB link whose title's name is unrelated is removed,
//!    with the metadata that link brought (source `igdb`: localized / alternative names,
//!    genres, companies, keywords, ...). Exact and manual links are never touched.
//!
//! Before both, links a bonus demo disc gave its title ([`separate_demo_links`]): a demo of the
//! sequel filed under the game it came with linked that game's title to the sequel, and the
//! two games were merged. The loose name rule calls "Hoge Quest" related to "Hoge Quest 2", so
//! the steps above never caught it.

use std::collections::{HashMap, HashSet};

use sqlx::mysql::MySqlPool;
use sqlx::Row;

use super::error::DatabaseError;
use super::jobs::Progress;
use super::matching::{names_close, names_related};
use super::providers::ProviderClients;

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct AuditSummary {
    pub merges_checked: u64,
    pub merges_decoupled: u64,
    pub decouple_failed: u64,
    /// Earlier audit decouples merged back because the (since widened) name rule calls them
    /// related.
    pub remerged: u64,
    /// Automatic merges of titles on different platforms, split again.
    pub platform_splits: u64,
    /// Automatic merges of a pirate / unlicensed-only title into a retail one, split again.
    pub unlicensed_splits: u64,
    /// Titles whose own files name another IGDB game than their link: the merges the wrong
    /// link brought undone, and the title linked to its own game.
    pub relinked: Vec<u64>,
    pub links_checked: u64,
    pub links_removed: u64,
    /// IGDB games whose alternative / localized names were fetched to judge a name.
    pub games_fetched: u64,
    /// Titles that held a demo of another game and were linked (and merged) through it.
    pub demo_titles_fixed: u64,
}

/// An IGDB game's names: the cached name, then (on demand) alternative / localized names.
#[derive(Default)]
struct IgdbNameBook {
    primary: HashMap<u32, String>,
    extra: HashMap<u32, Vec<String>>,
}

impl IgdbNameBook {
    async fn load_primary(&mut self, pool: &MySqlPool, ids: &[u32]) -> Result<(), DatabaseError> {
        for chunk in ids.chunks(1000) {
            let list = chunk.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
            for r in sqlx::query(sqlx::AssertSqlSafe(format!("SELECT provider_id, title FROM igdb_games WHERE provider_id IN ({list}) AND title IS NOT NULL"))).fetch_all(pool).await? {
                self.primary.insert(r.try_get("provider_id")?, r.try_get("title")?);
            }
        }
        Ok(())
    }

    /// Alternative / localized names of `id` from the cached raw game's id lists, fetched
    /// from IGDB (one request per list). Without an IGDB client: none.
    async fn load_extra(&mut self, pool: &MySqlPool, clients: &ProviderClients, id: u32, summary: &mut AuditSummary) -> Result<(), DatabaseError> {
        if self.extra.contains_key(&id) {
            return Ok(());
        }
        let mut names = Vec::new();
        let path: Option<String> = sqlx::query_scalar("SELECT raw_response_path FROM igdb_games WHERE provider_id = ?").bind(id).fetch_optional(pool).await?.flatten();
        if let (Some(path), Some(client)) = (path, clients.igdb.as_ref()) {
            if let Ok(text) = tokio::fs::read_to_string(&path).await {
                let value: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
                // A bulk sync's page holds many games: this one, not the first.
                let game = match value { serde_json::Value::Array(games) => games.into_iter().find(|g| g.get("id").and_then(|v| v.as_i64()) == Some(id as i64)).unwrap_or_default(), other => other };
                let ids = |k: &str| -> Vec<i32> { game.get(k).and_then(|v| v.as_array()).into_iter().flatten().filter_map(|x| x.as_i64()).map(|x| x as i32).collect() };
                for (endpoint, key) in [("alternative_names", "alternative_names"), ("game_localizations", "game_localizations")] {
                    let list = ids(key);
                    for chunk in list.chunks(100) {
                        match client.get_raw_by_ids(endpoint, chunk.to_vec(), "name").await {
                            Ok(rows) => names.extend(rows.iter().filter_map(|r| r.get("name").and_then(|n| n.as_str()).map(str::to_string))),
                            Err(e) => log::warn!("merge audit: IGDB {endpoint} for game {id}: {e}"),
                        }
                    }
                }
                summary.games_fetched += 1;
            }
        }
        self.extra.insert(id, names);
        Ok(())
    }

    /// Whether `name` is related to IGDB game `id` (unknown games count as related). `strict`
    /// for a link made from a search result rather than an exact name (`names_close`).
    async fn related(&mut self, pool: &MySqlPool, clients: &ProviderClients, id: u32, name: &str, strict: bool, summary: &mut AuditSummary) -> Result<bool, DatabaseError> {
        let Some(primary) = self.primary.get(&id).cloned() else { return Ok(true) };
        let check = |a: &str, b: &str| if strict { names_close(a, b) } else { names_related(a, b) };
        if check(name, &primary) {
            return Ok(true);
        }
        self.load_extra(pool, clients, id, summary).await?;
        Ok(self.extra.get(&id).is_some_and(|names| names.iter().any(|n| check(name, n))))
    }
}

/// Names a title answers to: its own and its releases' (a merged title's canonical name
/// may be English while a release is Japanese).
async fn title_names(pool: &MySqlPool, title_id: u64) -> Result<Vec<String>, DatabaseError> {
    let mut names: Vec<String> = sqlx::query_scalar("SELECT name FROM title WHERE id = ?").bind(title_id).fetch_all(pool).await?;
    names.extend(sqlx::query_scalar::<_, String>("SELECT r.name FROM title_releases tr JOIN `release` r ON r.id = tr.release_id WHERE tr.title_id = ? LIMIT 20").bind(title_id).fetch_all(pool).await?);
    Ok(names)
}

/// The IGDB games Hasheous' dump index gives a title's own files: those of the releases it
/// had itself (not the ones its merges brought) that are in no more titles than a file can
/// be said to identify (`cache::SHARED_CONTENT_MAX_TITLES` -- a stock audio track on dozens of
/// discs names one game for all of them). Empty: nothing to judge by.
async fn own_hasheous_igdb(pool: &MySqlPool, title_id: u64) -> Result<HashSet<String>, DatabaseError> {
    let brought: HashSet<u64> = sqlx::query_scalar::<_, u64>(
        "SELECT j.release_id FROM title_merge_log l JOIN title_merge_archive a ON a.log_id = l.id, \
           JSON_TABLE(a.rows_json, '$.title_releases[*]' COLUMNS (release_id BIGINT UNSIGNED PATH '$.release_id')) j WHERE l.survivor_id = ? AND l.undone_at IS NULL AND l.reason <> 'split'",
    )
    .bind(title_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect();
    let own: Vec<u64> = sqlx::query_scalar::<_, u64>("SELECT tr.release_id FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE AND r.material IS NULL WHERE tr.title_id = ?")
        .bind(title_id)
        .fetch_all(pool)
        .await?
        .into_iter()
        .filter(|r| !brought.contains(r))
        .collect();
    if own.is_empty() {
        return Ok(HashSet::new());
    }
    let list = own.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let contents: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT rc.content_id FROM release_contents rc JOIN title_releases tr ON tr.release_id = rc.release_id \
         WHERE rc.content_id IN (SELECT content_id FROM release_contents WHERE release_id IN ({list})) GROUP BY rc.content_id HAVING COUNT(DISTINCT tr.title_id) <= {}",
        super::providers::cache::SHARED_CONTENT_MAX_TITLES
    )))
    .fetch_all(pool)
    .await?;
    if contents.is_empty() {
        return Ok(HashSet::new());
    }
    let clist = contents.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let mapped: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT g.mapped FROM hasheous_dump_games g WHERE g.id IN ( \
           SELECT CAST(SUBSTRING_INDEX(SUBSTRING_INDEX(hl.raw_response_path, 'dump_game_', -1), '.', 1) AS UNSIGNED) FROM hasheous_lookups hl \
           WHERE hl.content_id IN ({clist}) AND hl.hit = TRUE AND hl.deleted = FALSE AND hl.raw_response_path LIKE '%dump_game_%')"
    )))
    .fetch_all(pool)
    .await?;
    let mut out = HashSet::new();
    for m in mapped {
        let pairs: Vec<(String, String)> = serde_json::from_str(&m).unwrap_or_default();
        out.extend(pairs.into_iter().filter(|(p, _)| p == "igdb").map(|(_, id)| id));
    }
    Ok(out)
}

/// Removes a title's IGDB link and everything it brought (rows with source `igdb`).
async fn drop_igdb_link(pool: &MySqlPool, title_id: u64, igdb_id: &str) -> Result<(), DatabaseError> {
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE title_provider_links SET deleted = TRUE WHERE title_id = ? AND provider = 'igdb' AND provider_id = ? AND confidence IN ('hash', 'name', 'fuzzy')")
        .bind(title_id)
        .bind(igdb_id)
        .execute(&mut *tx)
        .await?;
    for table in super::providers::metadata::IGDB_TITLE_TABLES {
        sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {table} WHERE title_id = ? AND source = 'igdb'"))).bind(title_id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// The game a demo release's name says it is a demo of: "Fuga Saga" out of "Hoge Quest (Japan)
/// (Disc 2) (Fuga Saga Taikenban)". None for a plain "(Demo)" -- a demo of the title's own game.
pub fn demo_of(release_name: &str) -> Option<String> {
    lazy_static::lazy_static! {
        static ref RE: regex::Regex = regex::Regex::new(r"(?i)\(([^()]+?)\s+(demo|taikenban|trial|otameshiban)\)").unwrap();
    }
    RE.captures_iter(release_name).filter_map(|c| c.get(1)).map(|m| m.as_str().trim().to_string()).find(|n| !n.is_empty())
}

/// Puts right what a bonus demo disc did before demos took their links themselves
/// (`providers::cache::APART_SQL`). A title holding a demo of another game, linked to the IGDB
/// game the demo names: the IGDB merges into it are decoupled, and each title that comes out of
/// it keeps the link only when one of its own (non-demo) releases is that game. Titles that do
/// are free to merge again; the rest are linked by their own discs the next time.
async fn separate_demo_links(pool: &MySqlPool, clients: &ProviderClients, book: &mut IgdbNameBook, s: &mut AuditSummary) -> Result<(), DatabaseError> {
    let rows: Vec<(u64, String, String)> = sqlx::query_as(
        "SELECT DISTINCT t.id, l.provider_id, r.name FROM title t \
         JOIN title_provider_links l ON l.title_id = t.id AND l.provider = 'igdb' AND l.deleted = FALSE AND l.confidence IN ('hash', 'name', 'fuzzy') \
         JOIN title_releases tr ON tr.title_id = t.id JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE AND r.is_demo = TRUE \
         WHERE t.deleted = FALSE AND EXISTS (SELECT 1 FROM title_releases tg JOIN `release` rg ON rg.id = tg.release_id AND rg.deleted = FALSE AND rg.is_demo = FALSE AND rg.material IS NULL WHERE tg.title_id = t.id)",
    )
    .fetch_all(pool)
    .await?;
    let ids: Vec<u32> = rows.iter().filter_map(|(_, p, _)| p.parse().ok()).filter(|id| !book.primary.contains_key(id)).collect::<HashSet<u32>>().into_iter().collect();
    book.load_primary(pool, &ids).await?;
    let mut done: HashSet<u64> = HashSet::new();
    for (title, igdb, demo_release) in rows {
        let (Ok(id), Some(game)) = (igdb.parse::<u32>(), demo_of(&demo_release)) else { continue };
        if done.contains(&title) || !book.related(pool, clients, id, &game, true, s).await? {
            continue;
        }
        done.insert(title);
        // Every IGDB merge into it, newest first, each side back on its own.
        let merges: Vec<u64> = sqlx::query_scalar("SELECT id FROM title_merge_log WHERE survivor_id = ? AND undone_at IS NULL AND reason = ? ORDER BY id DESC").bind(title).bind(format!("igdb:{igdb}")).fetch_all(pool).await?;
        let mut parts = vec![title];
        for log_id in merges {
            match super::title_merge::decouple(pool, log_id).await {
                Ok(restored) => {
                    sqlx::query("UPDATE title_merge_log SET reason = CONCAT(reason, ' (demo)') WHERE id = ?").bind(log_id).execute(pool).await?;
                    s.merges_decoupled += 1;
                    parts.push(restored);
                }
                Err(e) => {
                    log::warn!("merge audit: decoupling merge {log_id} (a demo of another game): {e}");
                    s.decouple_failed += 1;
                }
            }
        }
        // Each part keeps the link only when one of its own games is that game.
        let mut kept: Vec<u64> = Vec::new();
        for part in parts {
            let own: Vec<String> = sqlx::query_scalar("SELECT r.name FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE AND r.is_demo = FALSE AND r.material IS NULL WHERE tr.title_id = ?").bind(part).fetch_all(pool).await?;
            let mut is_it = false;
            for name in &own {
                if book.related(pool, clients, id, name, true, s).await? {
                    is_it = true;
                    break;
                }
            }
            if is_it {
                kept.push(part);
            } else {
                drop_igdb_link(pool, part, &igdb).await?;
                s.links_removed += 1;
            }
        }
        // The decouples kept apart titles that are one game after all: let them merge again.
        if kept.len() > 1 {
            let list = kept.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
            sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM title_merge_blocks WHERE title_a IN ({list}) AND title_b IN ({list})"))).execute(pool).await?;
        }
        s.demo_titles_fixed += 1;
        log::info!("merge audit: title {title} was linked to IGDB {igdb} through a demo of {game:?}; kept on {kept:?}");
    }
    Ok(())
}

/// Once per database, after migration 0056 marked the demos of other games: queues an audit,
/// so what such demos already did is put right without anyone asking. Only where it can be
/// needed -- some title holds such a demo and is linked to IGDB -- and only on a machine that
/// builds its own catalogue (a client's comes from its main server, which repairs its own).
/// Once after updating to 0059: a merge audit, which splits automatic merges of titles on
/// different platforms (step 0b) and undoes those a wrong link brought in (step 0c) -- and
/// keeps them apart from then on.
/// Once: the audit that splits pirate / unlicensed-only titles back out of retail ones
/// (step 0b'), on a machine that has automatic merges to look at and builds its own catalogue.
pub async fn queue_unlicensed_repair_once(pool: &MySqlPool) -> Result<(), DatabaseError> {
    const NAME: &str = "unlicensed_merges_0075";
    if sqlx::query_scalar::<_, String>("SELECT name FROM data_migrations WHERE name = ?").bind(NAME).fetch_optional(pool).await?.is_some() {
        return Ok(());
    }
    let client = super::settings::instance_mode(pool).await? == super::settings::InstanceMode::Client;
    let any: Option<u64> = sqlx::query_scalar("SELECT id FROM title_merge_log WHERE undone_at IS NULL AND ((grade <> 'manual' AND (reason LIKE 'igdb:%' OR reason LIKE 'ra:%')) OR reason LIKE 'suggestion:%') LIMIT 1").fetch_optional(pool).await?;
    if any.is_some() && !client {
        let id = super::jobs::enqueue(pool, super::jobs::JobType::TitleMergeAudit, serde_json::json!({})).await?;
        log::info!("queued merge audit job {id}: pirate and unlicensed games merged into retail titles are split again");
    }
    sqlx::query("INSERT IGNORE INTO data_migrations (name) VALUES (?)").bind(NAME).execute(pool).await?;
    Ok(())
}

pub async fn queue_platform_repair_once(pool: &MySqlPool) -> Result<(), DatabaseError> {
    const NAME: &str = "platform_merges_0059";
    let done: Option<String> = sqlx::query_scalar("SELECT name FROM data_migrations WHERE name = ?").bind(NAME).fetch_optional(pool).await?;
    if done.is_some() {
        return Ok(());
    }
    let client = super::settings::instance_mode(pool).await? == super::settings::InstanceMode::Client;
    let any: Option<u64> = sqlx::query_scalar("SELECT id FROM title_merge_log WHERE undone_at IS NULL AND grade <> 'manual' AND (reason LIKE 'igdb:%' OR reason LIKE 'ra:%') LIMIT 1").fetch_optional(pool).await?;
    if any.is_some() && !client {
        let id = super::jobs::enqueue(pool, super::jobs::JobType::TitleMergeAudit, serde_json::json!({})).await?;
        log::info!("queued merge audit job {id}: titles merged across platforms or through a wrong link are split again");
    }
    sqlx::query("INSERT IGNORE INTO data_migrations (name) VALUES (?)").bind(NAME).execute(pool).await?;
    Ok(())
}

/// Titles whose own automatic IGDB link was dropped and never made again, with a release of
/// the game itself to look up by (`APART_SQL`). "Hash match all" used to reset a title's links
/// for its DLC or update files, which re-link only their own release.
pub async fn titles_that_lost_igdb(pool: &MySqlPool) -> Result<Vec<u64>, DatabaseError> {
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT t.id FROM title t JOIN title_provider_links l ON l.title_id = t.id AND l.provider = 'igdb' AND l.deleted = TRUE AND l.confidence IN ('hash', 'name', 'fuzzy') \
         WHERE t.deleted = FALSE AND EXISTS (SELECT 1 FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE WHERE tr.title_id = t.id AND NOT {}) ORDER BY t.id",
        super::providers::cache::APART_SQL
    )))
    .fetch_all(pool)
    .await?)
}

/// Once after 0067: an IGDB lookup by name for every title that lost its own link that way
/// (`titles_that_lost_igdb`), so its releases get IGDB's metadata and art again. In jobs of
/// 500 titles, so a restart costs one job's worth; only on a machine that builds its own
/// catalogue.
pub async fn queue_lost_igdb_repair_once(pool: &MySqlPool) -> Result<(), DatabaseError> {
    const NAME: &str = "lost_igdb_links_0067";
    let done: Option<String> = sqlx::query_scalar("SELECT name FROM data_migrations WHERE name = ?").bind(NAME).fetch_optional(pool).await?;
    if done.is_some() {
        return Ok(());
    }
    if super::settings::instance_mode(pool).await? != super::settings::InstanceMode::Client {
        let titles = titles_that_lost_igdb(pool).await?;
        for chunk in titles.chunks(500) {
            super::jobs::enqueue(pool, super::jobs::JobType::ProviderQuery, serde_json::json!({ "provider": "igdb", "title_ids": chunk })).await?;
        }
        if !titles.is_empty() {
            log::info!("queued IGDB name lookups for {} title(s) whose own IGDB link was lost", titles.len());
        }
    }
    sqlx::query("INSERT IGNORE INTO data_migrations (name) VALUES (?)").bind(NAME).execute(pool).await?;
    Ok(())
}

pub async fn queue_demo_repair_once(pool: &MySqlPool) -> Result<(), DatabaseError> {
    const NAME: &str = "demo_links_0056";
    let done: Option<String> = sqlx::query_scalar("SELECT name FROM data_migrations WHERE name = ?").bind(NAME).fetch_optional(pool).await?;
    if done.is_some() {
        return Ok(());
    }
    let client = super::settings::instance_mode(pool).await? == super::settings::InstanceMode::Client;
    let needed: Option<u64> = sqlx::query_scalar(
        "SELECT tr.title_id FROM `release` r JOIN title_releases tr ON tr.release_id = r.id JOIN title_provider_links l ON l.title_id = tr.title_id AND l.provider = 'igdb' AND l.deleted = FALSE \
         WHERE r.is_demo = TRUE AND r.deleted = FALSE AND r.name REGEXP '\\([^()]* (Demo|Taikenban|Trial|Otameshiban)\\)' LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;
    if needed.is_some() && !client {
        let id = super::jobs::enqueue(pool, super::jobs::JobType::TitleMergeAudit, serde_json::json!({})).await?;
        log::info!("queued merge audit job {id}: titles linked through a demo of another game are put right");
    }
    sqlx::query("INSERT IGNORE INTO data_migrations (name) VALUES (?)").bind(NAME).execute(pool).await?;
    Ok(())
}

pub async fn audit(pool: &MySqlPool, clients: &ProviderClients, progress: &Progress) -> Result<AuditSummary, DatabaseError> {
    let mut s = AuditSummary::default();
    let mut book = IgdbNameBook::default();

    progress.force(0, None, "merge audit: titles linked through a demo of another game").await;
    separate_demo_links(pool, clients, &mut book, &mut s).await?;

    // 0. An earlier audit's decouples that the current name rule calls related: merged back
    //    (a merge by hand lifts the block the decouple set).
    let undone = sqlx::query("SELECT id, survivor_id, merged_name, reason, restored_title_id FROM title_merge_log WHERE undone_at IS NOT NULL AND reason LIKE 'igdb:%(audit)' AND restored_title_id IS NOT NULL")
        .fetch_all(pool)
        .await?;
    let undone_ids: Vec<u32> = undone.iter().filter_map(|r| r.try_get::<String, _>("reason").ok()).filter_map(|r| r.trim_start_matches("igdb:").trim_end_matches(" (audit)").parse().ok()).collect::<HashSet<u32>>().into_iter().collect();
    book.load_primary(pool, &undone_ids).await?;
    for r in &undone {
        let name: String = r.try_get("merged_name")?;
        let Ok(igdb) = r.try_get::<String, _>("reason")?.trim_start_matches("igdb:").trim_end_matches(" (audit)").parse::<u32>() else { continue };
        if !book.related(pool, clients, igdb, &name, false, &mut s).await? {
            continue;
        }
        let (Some(a), Some(b)) = (super::title_merge::live_title(pool, r.try_get("survivor_id")?).await?, super::title_merge::live_title(pool, r.try_get("restored_title_id")?).await?) else { continue };
        // Never across platforms: the same game on two of them is two titles, one row on the
        // cross-platform list -- however alike the names.
        if a != b && super::title_merge::share_platform(pool, a, b).await? {
            super::title_merge::merge_titles(pool, &[a, b], &format!("igdb:{igdb}"), "name").await?;
            sqlx::query("UPDATE title_merge_log SET reason = CONCAT(reason, ' (re-merged)') WHERE id = ?").bind(r.try_get::<u64, _>("id")?).execute(pool).await?;
            s.remerged += 1;
        }
    }

    // 0b. Automatic merges of titles with no platform in common: made by passes that did not
    //     ask (or through a link that was wrong), split again for good -- marked so step 0
    //     never joins them back.
    let automatic: Vec<u64> = sqlx::query_scalar("SELECT id FROM title_merge_log WHERE undone_at IS NULL AND grade <> 'manual' AND (reason LIKE 'igdb:%' OR reason LIKE 'ra:%') ORDER BY id DESC")
        .fetch_all(pool)
        .await?;
    for log_id in automatic {
        let Some((merged, survivor)) = super::title_merge::merge_side_platforms(pool, log_id).await? else { continue };
        if !merged.is_disjoint(&survivor) {
            continue;
        }
        match super::title_merge::decouple(pool, log_id).await {
            Ok(_) => {
                sqlx::query("UPDATE title_merge_log SET reason = CONCAT(reason, ' (platforms)') WHERE id = ?").bind(log_id).execute(pool).await?;
                s.platform_splits += 1;
            }
            Err(e) => {
                log::warn!("merge audit: splitting merge {log_id} across platforms: {e}");
                s.decouple_failed += 1;
            }
        }
    }

    // 0b'. Automatic merges that joined a title of nothing but pirate or unlicensed releases
    //      to one with a retail release (`title_merge::unlicensed_only`): a shared IGDB or
    //      RetroAchievements game between the two was a wrong link. Split again for good.
    // Accepted suggestions too ("accept all" took many at once); a merge the user made by
    // picking the titles stays.
    let automatic: Vec<u64> = sqlx::query_scalar("SELECT id FROM title_merge_log WHERE undone_at IS NULL AND ((grade <> 'manual' AND (reason LIKE 'igdb:%' OR reason LIKE 'ra:%')) OR reason LIKE 'suggestion:%') ORDER BY id DESC")
        .fetch_all(pool)
        .await?;
    for log_id in automatic {
        if !super::title_merge::merge_joined_unlicensed(pool, log_id).await? {
            continue;
        }
        match super::title_merge::decouple(pool, log_id).await {
            Ok(_) => {
                sqlx::query("UPDATE title_merge_log SET reason = CONCAT(reason, ' (unlicensed)') WHERE id = ?").bind(log_id).execute(pool).await?;
                s.unlicensed_splits += 1;
            }
            Err(e) => {
                log::warn!("merge audit: splitting unlicensed merge {log_id}: {e}");
                s.decouple_failed += 1;
            }
        }
    }

    // 0c. A title whose own files Hasheous names as another game than its IGDB link: the link
    //     came in through something else (a file it shares with other games, a demo), and
    //     pulled that game's titles in. Those merges are undone, the link replaced by the
    //     title's own game.
    let survivors: Vec<u64> = sqlx::query_scalar("SELECT DISTINCT survivor_id FROM title_merge_log WHERE undone_at IS NULL AND grade <> 'manual' AND (reason LIKE 'igdb:%' OR reason LIKE 'ra:%')")
        .fetch_all(pool)
        .await?;
    for survivor in survivors {
        let Some(title) = super::title_merge::live_title(pool, survivor).await? else { continue };
        let link: Option<String> = sqlx::query_scalar("SELECT provider_id FROM title_provider_links WHERE title_id = ? AND provider = 'igdb' AND deleted = FALSE AND confidence IN ('hash', 'name', 'fuzzy')")
            .bind(title)
            .fetch_optional(pool)
            .await?;
        let Some(link) = link else { continue };
        let own = own_hasheous_igdb(pool, title).await?;
        if own.is_empty() || own.contains(&link) {
            continue;
        }
        let logs: Vec<u64> = sqlx::query_scalar("SELECT id FROM title_merge_log WHERE survivor_id = ? AND undone_at IS NULL AND grade <> 'manual' ORDER BY id DESC").bind(title).fetch_all(pool).await?;
        for log_id in logs {
            match super::title_merge::decouple(pool, log_id).await {
                Ok(_) => {
                    sqlx::query("UPDATE title_merge_log SET reason = CONCAT(reason, ' (wrong link)') WHERE id = ?").bind(log_id).execute(pool).await?;
                    s.merges_decoupled += 1;
                }
                Err(e) => {
                    log::warn!("merge audit: undoing merge {log_id} of title {title} (wrong link): {e}");
                    s.decouple_failed += 1;
                }
            }
        }
        drop_igdb_link(pool, title, &link).await?;
        s.links_removed += 1;
        if own.len() == 1 {
            if let Some(game) = own.iter().next() {
                super::providers::cache::link_title(pool, title, super::providers::Provider::Igdb, game, super::providers::cache::Confidence::Hash).await?;
            }
        }
        s.relinked.push(title);
        log::info!("merge audit: title {title} was linked to IGDB {link}, its own files name {own:?}: merges undone, relinked");
    }

    // 1. Merges made because of an IGDB id.
    let merges = sqlx::query("SELECT id, merged_name, reason FROM title_merge_log WHERE undone_at IS NULL AND reason LIKE 'igdb:%' AND reason NOT LIKE '%(audit)%' AND reason NOT LIKE '%(platforms)%' ORDER BY id").fetch_all(pool).await?;
    let merge_ids: Vec<u32> = merges.iter().filter_map(|r| r.try_get::<String, _>("reason").ok()).filter_map(|r| r.trim_start_matches("igdb:").parse().ok()).collect::<HashSet<u32>>().into_iter().collect();
    book.load_primary(pool, &merge_ids).await?;
    let n = merges.len() as u64;
    for (i, m) in merges.iter().enumerate() {
        progress.report(i as u64, Some(n), &format!("merge audit: merge {} of {n} ({} decoupled)", i + 1, s.merges_decoupled)).await;
        let log_id: u64 = m.try_get("id")?;
        let name: String = m.try_get("merged_name")?;
        let Ok(igdb) = m.try_get::<String, _>("reason")?.trim_start_matches("igdb:").parse::<u32>() else { continue };
        s.merges_checked += 1;
        if book.related(pool, clients, igdb, &name, false, &mut s).await? {
            continue;
        }
        match super::title_merge::decouple(pool, log_id).await {
            Ok(_) => {
                sqlx::query("UPDATE title_merge_log SET reason = CONCAT(reason, ' (audit)') WHERE id = ? AND reason NOT LIKE '%(audit)'").bind(log_id).execute(pool).await?;
                s.merges_decoupled += 1;
            }
            Err(e) => {
                log::warn!("merge audit: decoupling merge {log_id} ({name}): {e}");
                s.decouple_failed += 1;
            }
        }
    }

    // 2. Loose IGDB links whose title doesn't answer to the game's names.
    // A fuzzy link (a search result, no exact name) has to name the same game; the others
    // only must not plainly name another one.
    let links: Vec<(u64, String, String)> = sqlx::query_as(
        "SELECT tpl.title_id, tpl.provider_id, CAST(tpl.confidence AS CHAR) FROM title_provider_links tpl JOIN title t ON t.id = tpl.title_id AND t.deleted = FALSE \
         WHERE tpl.provider = 'igdb' AND tpl.deleted = FALSE AND tpl.confidence IN ('hash', 'name', 'fuzzy')",
    )
    .fetch_all(pool)
    .await?;
    let link_ids: Vec<u32> = links.iter().filter_map(|(_, p, _)| p.parse().ok()).filter(|id| !book.primary.contains_key(id)).collect::<HashSet<u32>>().into_iter().collect();
    book.load_primary(pool, &link_ids).await?;
    let n = links.len() as u64;
    for (i, (title_id, igdb, confidence)) in links.iter().enumerate() {
        if i % 200 == 0 {
            progress.report(i as u64, Some(n), &format!("merge audit: link {} of {n} ({} removed)", i + 1, s.links_removed)).await;
        }
        let Ok(id) = igdb.parse::<u32>() else { continue };
        s.links_checked += 1;
        let mut related = false;
        for name in title_names(pool, *title_id).await? {
            if book.related(pool, clients, id, &name, confidence == "fuzzy", &mut s).await? {
                related = true;
                break;
            }
        }
        if !related {
            drop_igdb_link(pool, *title_id, igdb).await?;
            s.links_removed += 1;
            log::info!("merge audit: removed IGDB {igdb} ({}) from title {title_id}", book.primary.get(&id).cloned().unwrap_or_default());
        }
    }
    progress.force(n, Some(n), &format!("merge audit: {} of {} merges decoupled, {} of {} links removed", s.merges_decoupled, s.merges_checked, s.links_removed, s.links_checked)).await;
    Ok(s)
}
