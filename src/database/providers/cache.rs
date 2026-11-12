//! Check-then-fetch on top of the provider clients: every result lands in that provider's
//! cache table (raw response on disk via [`RawStore`]) and, where a catalog row can be
//! found for it, in `title_provider_links`/`release_provider_links`.

use std::path::PathBuf;

use playmatch_deredis::providers::hasheous::model::HashLookupRequest;
use playmatch_deredis::providers::hasheous::HashLookupOutcome;
use sqlx::mysql::MySqlPool;
use sqlx::Row;

use crate::common::models::Platform;

use super::super::error::DatabaseError;
use super::super::lookup;
use super::super::matching;
use super::raw_store::RawStore;
use super::{Provider, ProviderClients, Tier};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// The provider's own hash list matched our file.
    Exact,
    /// A hash-lookup service (Hasheous, PlayMatch) identified the file and reported this
    /// provider id for the game.
    Hash,
    /// A name search whose normalized name equals ours.
    Name,
    /// A name search's best guess.
    Fuzzy,
    Manual,
}

impl Confidence {
    fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Hash => "hash",
            Self::Name => "name",
            Self::Fuzzy => "fuzzy",
            Self::Manual => "manual",
        }
    }
}

/// One cached provider entity.
#[derive(Debug, Clone)]
pub struct CachedEntry {
    pub provider: Provider,
    pub provider_id: String,
    pub title: Option<String>,
    pub raw_path: Option<PathBuf>,
}

fn provider_id_is_text(provider: Provider) -> bool {
    matches!(provider, Provider::EmuReady)
}

/// Upserts one provider game into its cache table, storing `raw` on disk first.
pub async fn upsert_game(pool: &MySqlPool, store: &RawStore, provider: Provider, provider_id: &str, title: Option<&str>, raw: &serde_json::Value) -> Result<CachedEntry, DatabaseError> {
    if matches!(provider, Provider::Hasheous | Provider::PlayMatch) {
        return Err(DatabaseError::Provider(format!("{} is content-keyed; use its lookup function", provider.name())));
    }
    let raw_path = store.save_json(provider, provider_id, raw)?;
    let raw_path_str = raw_path.as_ref().map(|p| p.display().to_string());
    let table = provider.cache_table();
    let sql = format!(
        "INSERT INTO `{table}` (provider_id, raw_response_path, raw_format, title, fetched_at, deleted) VALUES (?, ?, 'json', ?, NOW(), FALSE) \
         ON DUPLICATE KEY UPDATE raw_response_path = VALUES(raw_response_path), raw_format = 'json', title = VALUES(title), fetched_at = NOW(), deleted = FALSE"
    );
    let query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
    let query = if provider_id_is_text(provider) {
        query.bind(provider_id.to_string())
    } else {
        let id: u64 = provider_id.parse().map_err(|_| DatabaseError::Provider(format!("{}: non-numeric provider id {provider_id:?}", provider.name())))?;
        query.bind(id)
    };
    query.bind(raw_path_str).bind(title).execute(pool).await?;
    // What the game says about its family, read now rather than out of the file later.
    if provider == Provider::Igdb {
        super::super::title_family::index_cached(pool, provider_id, raw).await?;
    }
    Ok(CachedEntry { provider, provider_id: provider_id.to_string(), title: title.map(str::to_string), raw_path })
}

/// RetroAchievements has a few extra indexed columns.
pub async fn upsert_ra_game(pool: &MySqlPool, store: &RawStore, game_id: u32, console_id: Option<u32>, title: Option<&str>, num_achievements: Option<u32>, genre_raw: Option<&str>, raw: &serde_json::Value) -> Result<CachedEntry, DatabaseError> {
    let raw_path = store.save_json(Provider::RetroAchievements, &game_id.to_string(), raw)?;
    let raw_path_str = raw_path.as_ref().map(|p| p.display().to_string());
    sqlx::query(
        "INSERT INTO retroachievements_games (provider_id, console_id, raw_response_path, raw_format, title, num_achievements, genre_raw, genre_major, genre_checked_at, fetched_at, deleted) VALUES (?, ?, ?, 'json', ?, ?, ?, ?, NOW(), NOW(), FALSE) \
         ON DUPLICATE KEY UPDATE console_id = COALESCE(VALUES(console_id), console_id), raw_response_path = VALUES(raw_response_path), title = COALESCE(VALUES(title), title), \
         num_achievements = COALESCE(VALUES(num_achievements), num_achievements), genre_raw = COALESCE(VALUES(genre_raw), genre_raw), genre_major = COALESCE(VALUES(genre_major), genre_major), \
         genre_checked_at = NOW(), fetched_at = NOW(), deleted = FALSE",
    )
    .bind(game_id)
    .bind(console_id)
    .bind(raw_path_str)
    .bind(title)
    .bind(num_achievements)
    .bind(genre_raw)
    .bind(genre_raw.and_then(super::ra_account::genre_majors))
    .execute(pool)
    .await?;
    Ok(CachedEntry { provider: Provider::RetroAchievements, provider_id: game_id.to_string(), title: title.map(str::to_string), raw_path })
}

/// What our own tables already know about a content, from exact-hash sources only.
#[derive(Debug, Clone, Default)]
pub struct LocalIdentity {
    /// The content belongs to a catalog release (matched to a DAT by exact hash).
    pub exact: bool,
    /// Its title has an IGDB link at exact/hash/manual confidence.
    pub igdb_known: bool,
    /// Links written from local sources this call.
    pub filled: u64,
}

/// Local-first identification, the step before asking PlayMatch online. Only **exact-hash**
/// sources ever write a link here -- never a name or fuzzy match:
/// - the Hasheous mapping recorded for this content (dump import or live lookup) → `Hash`
/// - `retroachievements_hashes` by the content's MD5 → `Exact`
/// - `screenscraper_rom_index` → `Exact`
/// Existing links keep their strength (`LINK_UPSERT_KEEP_STRONGER`; a manual link stays).
/// `igdb_known` is then read from the title's links at exact/hash/manual confidence.
pub async fn identify_local(pool: &MySqlPool, content_id: u64) -> Result<LocalIdentity, DatabaseError> {
    let titles: Vec<Option<u64>> = sqlx::query("SELECT tr.title_id FROM release_contents rc LEFT JOIN title_releases tr ON tr.release_id = rc.release_id WHERE rc.content_id = ?")
        .bind(content_id)
        .fetch_all(pool)
        .await?
        .iter()
        .map(|r| r.try_get("title_id"))
        .collect::<Result<_, _>>()?;
    if titles.is_empty() {
        return Ok(LocalIdentity::default());
    }
    let mut out = LocalIdentity { exact: true, ..Default::default() };
    if let Some(record) = hasheous_record_from_cache(pool, content_id).await?.filter(|r| r.hit) {
        for (provider, id, _) in &record.cross_refs {
            out.filled += link_for_content(pool, content_id, *provider, id, Confidence::Hash).await? as u64;
        }
    }
    let ra = sqlx::query("SELECT DISTINCT rh.ra_game_id FROM retroachievements_hashes rh JOIN hashes h ON h.algorithm IN ('md5', 'ra_hash') AND h.value_hex = rh.md5 WHERE h.content_id = ?").bind(content_id).fetch_all(pool).await?;
    for row in ra {
        let id: u32 = row.try_get("ra_game_id")?;
        out.filled += link_for_content(pool, content_id, Provider::RetroAchievements, &id.to_string(), Confidence::Exact).await? as u64;
    }
    if let Some(row) = sqlx::query("SELECT jeu_id FROM screenscraper_rom_index WHERE content_id = ?").bind(content_id).fetch_optional(pool).await? {
        let id: u64 = row.try_get("jeu_id")?;
        out.filled += link_for_content(pool, content_id, Provider::ScreenScraper, &id.to_string(), Confidence::Exact).await? as u64;
    }
    let title_ids: Vec<u64> = titles.into_iter().flatten().collect();
    if !title_ids.is_empty() {
        let sql = format!("SELECT 1 FROM title_provider_links WHERE provider = 'igdb' AND deleted = FALSE AND confidence IN ('exact', 'hash', 'manual') AND title_id IN ({}) LIMIT 1", vec!["?"; title_ids.len()].join(","));
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
        for t in &title_ids {
            query = query.bind(t);
        }
        out.igdb_known = query.fetch_optional(pool).await?.is_some();
    }
    Ok(out)
}

/// A cached entity read back from its raw response file into `T`, when its cache row is
/// younger than `ttl_days` -- lets a by-id fetch skip the request and still apply metadata.
/// `None` when not cached, stale, the raw file was discarded (retention setting), or it
/// doesn't parse as `T` (e.g. an RA entry cached from a console list, not a GetGame reply).
pub async fn load_cached<T: serde::de::DeserializeOwned>(pool: &MySqlPool, provider: Provider, provider_id: &str, ttl_days: u32) -> Result<Option<T>, DatabaseError> {
    if !is_cached(pool, provider, provider_id, ttl_days).await? {
        return Ok(None);
    }
    let sql = format!("SELECT raw_response_path FROM `{}` WHERE provider_id = ? AND deleted = FALSE", provider.cache_table());
    let query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
    let query = if provider_id_is_text(provider) { query.bind(provider_id.to_string()) } else { query.bind(provider_id.parse::<u64>().unwrap_or(0)) };
    let Some(path) = query.fetch_optional(pool).await?.and_then(|r| r.try_get::<Option<String>, _>("raw_response_path").ok().flatten()) else { return Ok(None) };
    let Ok(text) = std::fs::read_to_string(&path) else { return Ok(None) };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else { return Ok(None) };
    Ok(entry_for(value, provider_id).and_then(|v| serde_json::from_value(v).ok()))
}

/// The entry for `provider_id` in a cached response: the response itself, or -- a file an
/// older build wrote for a whole page of games -- the one game in it with that id, under
/// whichever key its provider names ids by. Read as one game, such a file gave nothing.
pub fn entry_for(value: serde_json::Value, provider_id: &str) -> Option<serde_json::Value> {
    match value {
        serde_json::Value::Array(entries) => entries.into_iter().find(|e| {
            ["id", "ID", "game_id", "GameID"].iter().any(|k| match e.get(*k) {
                Some(serde_json::Value::Number(n)) => n.to_string() == provider_id,
                Some(serde_json::Value::String(s)) => s == provider_id,
                _ => false,
            })
        }),
        other => Some(other),
    }
}

/// Is there a fresh (< `ttl_days`) cache row for this provider id?
pub async fn is_cached(pool: &MySqlPool, provider: Provider, provider_id: &str, ttl_days: u32) -> Result<bool, DatabaseError> {
    let table = provider.cache_table();
    let key_column = if matches!(provider, Provider::Hasheous | Provider::PlayMatch) { "content_id" } else { "provider_id" };
    let sql = format!("SELECT 1 FROM `{table}` WHERE {key_column} = ? AND deleted = FALSE AND fetched_at > NOW() - INTERVAL ? DAY LIMIT 1");
    let query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
    let query = if provider_id_is_text(provider) { query.bind(provider_id.to_string()) } else { query.bind(provider_id.parse::<u64>().unwrap_or(0)) };
    Ok(query.bind(ttl_days).fetch_optional(pool).await?.is_some())
}

/// The upsert tail both link tables share: an existing link is only replaced by one at
/// least as strong (manual > exact > hash > name > fuzzy) -- so a later name/fuzzy pass
/// (a bulk sync) can't overwrite the exact id a hash mapping (Hasheous, RA hashes) gave --
/// or when the old link was removed by a pass of ours.
///
/// A link the user removed (`deleted` with confidence `manual`, see `api::links::unlink_title`)
/// stays removed: only another manual link brings it back. Before, the next metadata update
/// searched the title by name and linked the same game again. Since `manual` is the strongest,
/// "at least as strong" as a removed manual link means manual.
///
/// Assignment order matters: MySQL evaluates them left to right, so `confidence`/`deleted`,
/// which the condition reads, come last. `deleted` reads the new `confidence`: it stays TRUE
/// only for a user-removed link that a weaker one did not replace.
const LINK_UPSERT_KEEP_STRONGER: &str = " ON DUPLICATE KEY UPDATE \
     provider_id = IF((deleted AND confidence <> 'manual') OR FIELD(VALUES(confidence),'fuzzy','name','hash','exact','manual') >= FIELD(confidence,'fuzzy','name','hash','exact','manual'), VALUES(provider_id), provider_id), \
     linked_at = IF((deleted AND confidence <> 'manual') OR FIELD(VALUES(confidence),'fuzzy','name','hash','exact','manual') >= FIELD(confidence,'fuzzy','name','hash','exact','manual'), NOW(), linked_at), \
     confidence = IF((deleted AND confidence <> 'manual') OR FIELD(VALUES(confidence),'fuzzy','name','hash','exact','manual') >= FIELD(confidence,'fuzzy','name','hash','exact','manual'), VALUES(confidence), confidence), \
     deleted = (deleted AND confidence = 'manual' AND VALUES(confidence) <> 'manual')";

/// [`LINK_UPSERT_KEEP_STRONGER`] for `title_provider_links`, which also remembers a game a
/// merge audit took off the title as wrong (`rejected_id`, `merge_audit::drop_igdb_link`):
/// only a manual link makes that one again, so the next hash match does not bring back the
/// link (and the merges it made) the audit removed. Another game links as before; a manual
/// link clears the mark.
fn title_link_upsert() -> String {
    let rejected = "(VALUES(confidence) <> 'manual' AND COALESCE(rejected_id = VALUES(provider_id), FALSE))";
    let replace = format!("(NOT {rejected} AND ((deleted AND confidence <> 'manual') OR FIELD(VALUES(confidence),'fuzzy','name','hash','exact','manual') >= FIELD(confidence,'fuzzy','name','hash','exact','manual')))");
    format!(
        " ON DUPLICATE KEY UPDATE provider_id = IF({replace}, VALUES(provider_id), provider_id), linked_at = IF({replace}, NOW(), linked_at), \
         confidence = IF({replace}, VALUES(confidence), confidence), \
         deleted = (deleted AND ((confidence = 'manual' AND VALUES(confidence) <> 'manual') OR {rejected})), \
         rejected_id = IF(VALUES(confidence) = 'manual', NULL, rejected_id)"
    )
}

/// Whether a merge audit found `provider_id` wrong for this title ([`title_link_upsert`]).
pub async fn rejected_for_title(pool: &MySqlPool, title_id: u64, provider: Provider, provider_id: &str) -> Result<bool, DatabaseError> {
    Ok(sqlx::query("SELECT 1 FROM title_provider_links WHERE title_id = ? AND provider = ? AND rejected_id = ?")
        .bind(title_id)
        .bind(provider.name())
        .bind(provider_id)
        .fetch_optional(pool)
        .await?
        .is_some())
}

/// Whether the user removed `provider`'s link from this title (`api::links::unlink_title`):
/// nothing but a manual link may make it again, so a by-name lookup is not even tried.
pub async fn unlinked_by_user(pool: &MySqlPool, title_id: u64, provider: Provider) -> Result<bool, DatabaseError> {
    Ok(sqlx::query("SELECT 1 FROM title_provider_links WHERE title_id = ? AND provider = ? AND deleted = TRUE AND confidence = 'manual'")
        .bind(title_id)
        .bind(provider.name())
        .fetch_optional(pool)
        .await?
        .is_some())
}

/// Links a provider entry to a title. Returns false when the link was refused: the user
/// removed this provider's link from the title, or a merge audit found this entry wrong for
/// it, and `confidence` is not manual. A caller that goes on to write the entry's metadata
/// onto the title stops there.
pub async fn link_title(pool: &MySqlPool, title_id: u64, provider: Provider, provider_id: &str, confidence: Confidence) -> Result<bool, DatabaseError> {
    let sql = format!("INSERT INTO title_provider_links (title_id, provider, provider_id, confidence, linked_at, deleted) VALUES (?, ?, ?, ?, NOW(), FALSE){}", title_link_upsert());
    sqlx::query(sqlx::AssertSqlSafe(sql))
    .bind(title_id)
    .bind(provider.name())
    .bind(provider_id)
    .bind(confidence.as_str())
    .execute(pool)
    .await?;
    if confidence == Confidence::Manual {
        return Ok(true);
    }
    Ok(!unlinked_by_user(pool, title_id, provider).await? && !rejected_for_title(pool, title_id, provider, provider_id).await?)
}

pub async fn link_release(pool: &MySqlPool, release_id: u64, provider: Provider, provider_id: &str, confidence: Confidence) -> Result<(), DatabaseError> {
    let sql = format!("INSERT INTO release_provider_links (release_id, provider, provider_id, confidence, linked_at, deleted) VALUES (?, ?, ?, ?, NOW(), FALSE){LINK_UPSERT_KEEP_STRONGER}");
    sqlx::query(sqlx::AssertSqlSafe(sql))
    .bind(release_id)
    .bind(provider.name())
    .bind(provider_id)
    .bind(confidence.as_str())
    .execute(pool)
    .await?;
    Ok(())
}

/// A release that sits under a title without being that title's game (alias `r`): a hack,
/// mod or translation of it, or something made for it or shipped with it. What a hash says
/// about such a file is about the file -- a hack has its own entry on IGDB, as a "mod" by
/// someone else -- and linked to the title it made the game itself a mod by a stranger, with
/// every hack's page listed as the game's. Such a release takes the link itself.
///
/// So is a demo under a title that also holds a game as it was sold: a bonus disc with a demo
/// of the sequel, filed under the game it came with. Its hash names the sequel, and linked the
/// title to it, after which the merge pass joined the two games. A title holding only a demo
/// still takes the link, which is how it finds its game. Needs `tr` (`title_releases`) too.
pub const APART_SQL: &str = "(COALESCE(r.game_kind IN ('hack', 'mod', 'translation'), FALSE) OR r.material IS NOT NULL \
     OR (r.is_demo AND EXISTS (SELECT 1 FROM title_releases trd JOIN `release` rd ON rd.id = trd.release_id AND rd.deleted = FALSE AND rd.is_demo = FALSE AND rd.material IS NULL WHERE trd.title_id = tr.title_id)))";

/// Links `provider_id` at the provider's own tier for every release/title that `content_id`
/// belongs to. Returns how many links were written.
pub async fn link_for_content(pool: &MySqlPool, content_id: u64, provider: Provider, provider_id: &str, confidence: Confidence) -> Result<u32, DatabaseError> {
    let found: Vec<(u64, Option<u64>, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT rc.release_id, IF({APART_SQL}, NULL, tr.title_id), {APART_SQL} FROM release_contents rc JOIN `release` r ON r.id = rc.release_id LEFT JOIN title_releases tr ON tr.release_id = rc.release_id WHERE rc.content_id = ?"
    )))
    .bind(content_id)
    .fetch_all(pool)
    .await?;
    let apart: std::collections::HashSet<u64> = found.iter().filter(|(_, _, apart)| *apart != 0).map(|(release, _, _)| *release).collect();
    let rows: Vec<(u64, Option<u64>)> = found.into_iter().map(|(release, title, _)| (release, title)).collect();
    // The shared-file guard `link_contents_bulk` applies, for the one-content path (a live
    // Hasheous / PlayMatch answer, or one read back from their cache during a scan).
    if confidence == Confidence::Hash && is_shared(&rows) {
        return Ok(0);
    }
    let mut links = 0;
    for (release_id, title_id) in rows {
        match provider.tier() {
            Tier::Release | Tier::Content => {
                link_release(pool, release_id, provider, provider_id, confidence).await?;
                links += 1;
            }
            Tier::Title => {
                if let Some(title_id) = title_id {
                    if link_title(pool, title_id, provider, provider_id, confidence).await? {
                        links += 1;
                    }
                } else if apart.contains(&release_id) {
                    link_release(pool, release_id, provider, provider_id, confidence).await?;
                    links += 1;
                }
            }
        }
    }
    Ok(links)
}

/// RetroAchievements links from roms' own RA hashes (`local_rom_rahash`, worked out by a
/// scan for consoles RA does not hash as the whole file): every release the rom's game
/// is linked to takes the RA game whose hash it is. `scope`, an SQL list of directory ids,
/// limits it to roms with a file there; `console` to one RA console. Returns links written.
pub async fn link_ra_rom_hashes(pool: &MySqlPool, scope: Option<&str>, console: Option<u32>) -> Result<u64, DatabaseError> {
    let mut sql = String::from(
        "SELECT DISTINCT lrg.release_id, rh.ra_game_id FROM local_rom_rahash lrr \
         JOIN hashes h ON h.id = lrr.hash_id AND h.algorithm = 'ra_hash' \
         JOIN retroachievements_hashes rh ON rh.md5 = h.value_hex \
         JOIN local_rom lr ON lr.id = lrr.rom_id AND lr.deleted = FALSE \
         JOIN local_game_roms lgr ON lgr.rom_id = lrr.rom_id \
         JOIN local_release_games lrg ON lrg.game_id = lgr.game_id \
         JOIN `release` r ON r.id = lrg.release_id AND r.deleted = FALSE WHERE 1 = 1",
    );
    if let Some(scope) = scope {
        sql.push_str(&format!(" AND lrr.rom_id IN (SELECT lrf.rom_id FROM local_rom_files lrf JOIN local_files f ON f.id = lrf.file_id WHERE f.directory_id IN ({scope}))"));
    }
    if let Some(console) = console {
        sql.push_str(&format!(" AND rh.console_id = {console}"));
    }
    let rows: Vec<(u64, u32)> = sqlx::query_as(sqlx::AssertSqlSafe(sql)).fetch_all(pool).await?;
    let mut links = 0;
    for (release_id, ra_game_id) in rows {
        link_release(pool, release_id, Provider::RetroAchievements, &ra_game_id.to_string(), Confidence::Exact).await?;
        links += 1;
    }
    Ok(links)
}

/// `link_for_content` for many (content, provider, provider id) at once -- a dump import
/// matches tens of thousands of contents, and most map onto the same few releases. One
/// query per 10,000 contents finds their releases/titles; the links are deduplicated in
/// memory and written 1,000 rows per statement, with the same tier rules and the same
/// keep-stronger upsert as `link_title`/`link_release`. Returns how many distinct links
/// were written and which (provider, id) pairs linked at least one release/title.
/// A file in more titles than this is shared, not identifying (see `link_contents_bulk`).
pub const SHARED_CONTENT_MAX_TITLES: usize = 2;

/// A content's (release, title) rows put it in more than `SHARED_CONTENT_MAX_TITLES` titles.
fn is_shared(rows: &[(u64, Option<u64>)]) -> bool {
    rows.iter().filter_map(|(_, t)| *t).collect::<std::collections::HashSet<u64>>().len() > SHARED_CONTENT_MAX_TITLES
}

pub async fn link_contents_bulk(pool: &MySqlPool, items: &[(u64, Provider, String)], confidence: Confidence) -> Result<(u64, std::collections::HashSet<(Provider, String)>), DatabaseError> {
    use std::collections::{HashMap, HashSet};
    let mut contents: Vec<u64> = items.iter().map(|(c, _, _)| *c).collect();
    contents.sort_unstable();
    contents.dedup();
    let mut targets: HashMap<u64, Vec<(u64, Option<u64>)>> = HashMap::new();
    // Releases that take a title-tier link themselves (`APART_SQL`).
    let mut apart: HashSet<u64> = HashSet::new();
    for chunk in contents.chunks(10_000) {
        let sql = format!(
            "SELECT rc.content_id, rc.release_id, IF({APART_SQL}, NULL, tr.title_id) AS title_id, {APART_SQL} AS apart FROM release_contents rc JOIN `release` r ON r.id = rc.release_id LEFT JOIN title_releases tr ON tr.release_id = rc.release_id WHERE rc.content_id IN ({})",
            vec!["?"; chunk.len()].join(",")
        );
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
        for id in chunk {
            query = query.bind(id);
        }
        for row in query.fetch_all(pool).await? {
            let release_id: u64 = row.try_get("release_id")?;
            if row.try_get::<i64, _>("apart")? != 0 {
                apart.insert(release_id);
            }
            targets.entry(row.try_get("content_id")?).or_default().push((release_id, row.try_get("title_id")?));
        }
    }
    // A file many different games share (an eShop system file, a compilation's common
    // track, a boot executable) says nothing about which game it is: a hash service mapping
    // it to one game would link every title that contains it (hundreds of unrelated games
    // under one IGDB id). Such files give no links.
    let shared: HashSet<u64> = targets
        .iter()
        .filter(|(_, rows)| is_shared(rows))
        .map(|(c, _)| *c)
        .collect();
    let mut release_links: HashSet<(u64, Provider, &str)> = HashSet::new();
    let mut title_links: HashSet<(u64, Provider, &str)> = HashSet::new();
    let mut linked: HashSet<(Provider, String)> = HashSet::new();
    for (content_id, provider, provider_id) in items {
        if shared.contains(content_id) {
            continue;
        }
        for (release_id, title_id) in targets.get(content_id).into_iter().flatten() {
            match provider.tier() {
                Tier::Release | Tier::Content => {
                    release_links.insert((*release_id, *provider, provider_id.as_str()));
                }
                Tier::Title => match title_id {
                    Some(t) => {
                        title_links.insert((*t, *provider, provider_id.as_str()));
                    }
                    None if apart.contains(release_id) => {
                        release_links.insert((*release_id, *provider, provider_id.as_str()));
                    }
                    None => continue,
                },
            }
            linked.insert((*provider, provider_id.clone()));
        }
    }
    for (table, key, links) in [("release_provider_links", "release_id", &release_links), ("title_provider_links", "title_id", &title_links)] {
        let rows: Vec<&(u64, Provider, &str)> = links.iter().collect();
        for chunk in rows.chunks(1000) {
            let upsert = if table == "title_provider_links" { title_link_upsert() } else { LINK_UPSERT_KEEP_STRONGER.to_string() };
            let sql = format!("INSERT INTO {table} ({key}, provider, provider_id, confidence, linked_at, deleted) VALUES {}{upsert}", vec!["(?, ?, ?, ?, NOW(), FALSE)"; chunk.len()].join(", "));
            let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
            for (id, provider, provider_id) in chunk {
                query = query.bind(*id).bind(provider.name()).bind(*provider_id).bind(confidence.as_str());
            }
            query.execute(pool).await?;
        }
    }
    Ok(((release_links.len() + title_links.len()) as u64, linked))
}

/// Links `provider_id` to the title whose normalized name matches `name` (within
/// `platform` when given). Fuzzy by construction. Returns the title id linked, if any.
pub async fn link_by_name(pool: &MySqlPool, provider: Provider, provider_id: &str, name: &str, platform: Option<Platform>) -> Result<Option<u64>, DatabaseError> {
    let candidates = lookup::search_releases_by_name(pool, name, platform, 5).await?;
    let key = matching::util::loose_key(name);
    let exact = candidates.iter().find(|c| matching::util::join_word_hyphens(&c.norm_name) == key);
    let confidence = if exact.is_some() { Confidence::Name } else { Confidence::Fuzzy };
    // Without an exact name, only a candidate that names the same game (`names_close`).
    let Some(best) = exact.or_else(|| candidates.iter().find(|c| matching::names_close(name, &c.name))) else { return Ok(None) };
    match provider.tier() {
        Tier::Title | Tier::Content => {
            if let Some(title_id) = best.title_id {
                if link_title(pool, title_id, provider, provider_id, confidence).await? {
                    return Ok(Some(title_id));
                }
            }
            Ok(None)
        }
        Tier::Release => {
            // TheGamesDB has one entry per regional release: a name match is for the release
            // of that region, not whichever was found first.
            if provider == Provider::TheGamesDb {
                let regions = super::thegamesdb_region::TgdbRegions::remembered();
                let priority = super::super::settings::region_priority(pool).await?;
                if !super::thegamesdb_region::should_link(pool, &regions, &priority, best.release_id, provider_id).await? {
                    return Ok(None);
                }
            }
            link_release(pool, best.release_id, provider, provider_id, confidence).await?;
            Ok(best.title_id)
        }
    }
}

// ---------------------------------------------------------------------------
// Hasheous (content-keyed)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct HasheousRecord {
    pub hit: bool,
    pub name: Option<String>,
    pub raw_path: Option<PathBuf>,
    /// (provider, provider id) cross-references Hasheous reported, and how many links
    /// each produced.
    pub cross_refs: Vec<(Provider, String, u32)>,
    /// What the signature said about the game: (name, year, publisher, system name).
    pub game: Option<HasheousGameInfo>,
    pub platform_name: Option<String>,
    pub publisher: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct HasheousGameInfo {
    pub name: Option<String>,
    pub year: Option<String>,
    pub publisher: Option<String>,
    pub system: Option<String>,
}

/// Looks `request` up on Hasheous for `content_id`, caches the outcome, and turns the
/// `metadata[]` cross-references into provider links.
/// A cached Hasheous hit rebuilt from its raw response file (the shape `hasheous_lookup`
/// writes), with the cross references it reported -- no links are written.
pub async fn hasheous_record_from_cache(pool: &MySqlPool, content_id: u64) -> Result<Option<HasheousRecord>, DatabaseError> {
    let row = sqlx::query("SELECT raw_response_path, hit FROM hasheous_lookups WHERE content_id = ? AND deleted = FALSE").bind(content_id).fetch_optional(pool).await?;
    let Some(row) = row else { return Ok(None) };
    let mut record = HasheousRecord { hit: row.try_get("hit")?, ..Default::default() };
    let Some(path) = row.try_get::<Option<String>, _>("raw_response_path")? else { return Ok(Some(record)) };
    let Ok(text) = std::fs::read_to_string(&path) else { return Ok(Some(record)) };
    let Ok(raw) = serde_json::from_str::<serde_json::Value>(&text) else { return Ok(Some(record)) };
    record.raw_path = Some(PathBuf::from(path));
    let s = |v: &serde_json::Value| v.as_str().map(str::to_string);
    record.name = s(&raw["name"]);
    record.platform_name = s(&raw["platform"]);
    record.publisher = s(&raw["publisher"]);
    if let Some(g) = raw.pointer("/signature/game").filter(|g| g.is_object()) {
        record.game = Some(HasheousGameInfo { name: s(&g["name"]), year: s(&g["year"]), publisher: s(&g["publisher"]), system: s(&g["system"]) });
    }
    for entry in raw["metadata"].as_array().into_iter().flatten() {
        let (Some(source), Some(id)) = (entry["source"].as_str(), entry["id"].as_str()) else { continue };
        let Some(provider) = Provider::from_hasheous_source(source) else { continue };
        if !id.is_empty() && id != "0" {
            record.cross_refs.push((provider, id.to_string(), 0));
        }
    }
    Ok(Some(record))
}

/// Makes a Hasheous hit browsable: when `content_id` belongs to no catalog release (no DAT
/// covers it), a release + title are created from the hit under the `hasheous` source, the
/// content attached, and the publisher recorded. Returns the (release, title) the content
/// now belongs to -- existing or new -- or `None` when the hit has no usable name.
pub async fn ensure_hasheous_release(pool: &MySqlPool, content_id: u64, record: &HasheousRecord, fallback_platform: Platform) -> Result<Option<(u64, u64)>, DatabaseError> {
    if let Some(row) = sqlx::query("SELECT rc.release_id, tr.title_id FROM release_contents rc JOIN title_releases tr ON tr.release_id = rc.release_id WHERE rc.content_id = ? LIMIT 1").bind(content_id).fetch_optional(pool).await? {
        return Ok(Some((row.try_get("release_id")?, row.try_get("title_id")?)));
    }
    let game = record.game.as_ref();
    let Some(name) = game.and_then(|g| g.name.as_deref()).or(record.name.as_deref()).map(str::trim).filter(|n| !n.is_empty()) else { return Ok(None) };
    let mut platform = game.and_then(|g| g.system.as_deref()).or(record.platform_name.as_deref()).map(super::super::platform_map::platform_from_dat_name).unwrap_or(Platform::Unknown);
    if platform == Platform::Unknown {
        platform = fallback_platform;
    }
    if platform == Platform::Unknown {
        let row = sqlx::query("SELECT app_platform_id FROM content WHERE id = ?").bind(content_id).fetch_optional(pool).await?;
        platform = row.and_then(|r| r.try_get::<u16, _>("app_platform_id").ok()).and_then(Platform::from_igdb_id).unwrap_or(Platform::Unknown);
    }
    let platform_id = platform.igdb_id();
    let source_id = super::super::import::ensure_source(pool, Provider::Hasheous.name()).await?;
    let name_hash = super::super::import::sha1_hex(name);
    // Same key the DAT import uses for titles (lowercased normalized name), so a hit
    // for a game a DAT already named joins that title instead of duplicating it.
    let norm_name = matching::util::normalize_title(name).to_lowercase();
    let norm_hash = super::super::import::sha1_hex(&norm_name);
    let year: Option<u16> = game.and_then(|g| g.year.as_deref()).and_then(|y| y.trim().get(..4)).and_then(|y| y.parse().ok());

    // One of these per name and platform at a time: two hits for the same game (two dumps of
    // it) each found no title and made one. The lock is a session's, so it is taken and
    // released on the connection the transaction runs on.
    let lock = format!("romexplorer_title:{platform_id}:{norm_hash}");
    let mut conn = pool.acquire().await?;
    let got: Option<i64> = sqlx::query_scalar("SELECT GET_LOCK(?, 60)").bind(&lock).fetch_one(&mut *conn).await?;
    if got != Some(1) {
        return Err(DatabaseError::Config(format!("{name}: another hash match is adding the same game")));
    }
    let made = hasheous_release_locked(&mut conn, content_id, name, platform_id, source_id, &name_hash, &norm_name, &norm_hash, year).await;
    let _ = sqlx::query("SELECT RELEASE_LOCK(?)").bind(&lock).execute(&mut *conn).await;
    let (release_id, title_id) = made?;

    if let Some(publisher) = game.and_then(|g| g.publisher.as_deref()).or(record.publisher.as_deref()) {
        super::metadata::apply_hasheous_publisher(pool, title_id, publisher).await?;
    }
    Ok(Some((release_id, title_id)))
}

/// [`ensure_hasheous_release`]'s writes, under its lock: the release, and its title.
#[allow(clippy::too_many_arguments)]
async fn hasheous_release_locked(conn: &mut sqlx::pool::PoolConnection<sqlx::MySql>, content_id: u64, name: &str, platform_id: u16, source_id: u8, name_hash: &str, norm_name: &str, norm_hash: &str, year: Option<u16>) -> Result<(u64, u64), DatabaseError> {
    use sqlx::Connection;
    let mut tx = conn.begin().await?;
    sqlx::query(
        "INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, year, source_id, deleted) VALUES (?, ?, ?, ?, ?, ?, ?, FALSE) \
         ON DUPLICATE KEY UPDATE year = COALESCE(VALUES(year), year), deleted = FALSE",
    )
    .bind(platform_id)
    .bind(name)
    .bind(name_hash)
    .bind(norm_name)
    .bind(norm_hash)
    .bind(year)
    .bind(source_id)
    .execute(&mut *tx)
    .await?;
    let release_id: u64 = sqlx::query("SELECT id FROM `release` WHERE app_platform_id = ? AND name_hash = ?").bind(platform_id).bind(name_hash).fetch_one(&mut *tx).await?.try_get("id")?;
    sqlx::query("INSERT IGNORE INTO release_sources (release_id, source_id) VALUES (?, ?)").bind(release_id).bind(source_id).execute(&mut *tx).await?;
    let file_name: String = sqlx::query("SELECT file_name FROM content WHERE id = ?").bind(content_id).fetch_optional(&mut *tx).await?.and_then(|r| r.try_get("file_name").ok()).unwrap_or_default();
    sqlx::query("INSERT IGNORE INTO release_contents (release_id, content_id, file_name, source_id) VALUES (?, ?, ?, ?)").bind(release_id).bind(content_id).bind(&file_name).bind(source_id).execute(&mut *tx).await?;
    // A release that has a title already (the same name, from a DAT or an earlier hit) keeps it.
    let titled: Option<u64> = sqlx::query_scalar("SELECT tr.title_id FROM title_releases tr JOIN title t ON t.id = tr.title_id AND t.deleted = FALSE WHERE tr.release_id = ? ORDER BY tr.title_id LIMIT 1").bind(release_id).fetch_optional(&mut *tx).await?;
    // Same title rule as the DAT import: one title per normalized name within a platform --
    // its own, or one a merged title had (`title_aliases`), so a hit for a merged game's other
    // name joins the merged title instead of bringing the old one back.
    let existing: Option<u64> = match titled {
        Some(t) => Some(t),
        None => sqlx::query_scalar(
            "SELECT id FROM (SELECT t.id FROM title t JOIN title_releases tr ON tr.title_id = t.id JOIN `release` r ON r.id = tr.release_id WHERE r.app_platform_id = ? AND t.norm_hash = ? AND t.deleted = FALSE \
             UNION SELECT a.title_id AS id FROM title_aliases a JOIN title t ON t.id = a.title_id AND t.deleted = FALSE JOIN title_releases tr ON tr.title_id = a.title_id JOIN `release` r ON r.id = tr.release_id \
             WHERE r.app_platform_id = ? AND a.norm_hash = ?) x ORDER BY id LIMIT 1",
        )
        .bind(platform_id)
        .bind(norm_hash)
        .bind(platform_id)
        .bind(norm_hash)
        .fetch_optional(&mut *tx)
        .await?,
    };
    let title_id: u64 = match existing {
        Some(id) => id,
        None => sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, ?, ?)").bind(name).bind(norm_name).bind(norm_hash).execute(&mut *tx).await?.last_insert_id(),
    };
    sqlx::query("INSERT IGNORE INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(title_id).bind(release_id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok((release_id, title_id))
}

pub async fn hasheous_lookup(clients: &ProviderClients, pool: &MySqlPool, store: &RawStore, content_id: u64, request: &HashLookupRequest) -> Result<HasheousRecord, DatabaseError> {
    let client = clients.hasheous.as_ref().ok_or_else(|| DatabaseError::Provider("hasheous client unavailable".to_string()))?;
    let outcome = client.lookup_by_hash(request).await?;
    let mut record = HasheousRecord::default();
    let raw: serde_json::Value = match &outcome {
        HashLookupOutcome::Hit(response) => {
            record.hit = true;
            record.name = response.name.clone();
            record.platform_name = response.platform.as_ref().and_then(|p| p.name.clone());
            record.publisher = response.publisher.as_ref().and_then(|p| p.name.clone());
            record.game = response.signature.as_ref().and_then(|s| s.game.as_ref()).map(|g| HasheousGameInfo { name: g.name.clone(), year: g.year.clone(), publisher: g.publisher.clone(), system: g.system.clone() });
            // The vendored model only keeps the fields playmatch needs; serialize what we
            // have (the full wire response isn't exposed by the client).
            serde_json::json!({
                "id": response.id,
                "name": response.name,
                "platform": response.platform.as_ref().map(|p| p.name.clone()),
                "publisher": response.publisher.as_ref().map(|p| p.name.clone()),
                "signature": response.signature.as_ref().map(|s| serde_json::json!({
                    "game": s.game.as_ref().map(|g| serde_json::json!({"id": g.id, "name": g.name, "year": g.year, "publisher": g.publisher, "system": g.system})),
                    "rom": s.rom.as_ref().map(|r| serde_json::json!({"crc": r.crc, "name": r.name, "size": r.size})),
                })),
                "metadata": response.metadata.as_ref().map(|m| m.iter().map(|e| serde_json::json!({"id": e.id, "immutableId": e.immutable_id, "source": e.source, "link": e.link})).collect::<Vec<_>>()),
            })
        }
        HashLookupOutcome::NotFound => serde_json::json!({"hit": false}),
    };
    record.raw_path = store.save_json(Provider::Hasheous, &content_id.to_string(), &raw)?;
    let raw_path_str = record.raw_path.as_ref().map(|p| p.display().to_string());
    sqlx::query(
        "INSERT INTO hasheous_lookups (content_id, raw_response_path, raw_format, hit, fetched_at, deleted) VALUES (?, ?, 'json', ?, NOW(), FALSE) \
         ON DUPLICATE KEY UPDATE raw_response_path = VALUES(raw_response_path), hit = VALUES(hit), fetched_at = NOW(), deleted = FALSE",
    )
    .bind(content_id)
    .bind(raw_path_str)
    .bind(record.hit)
    .execute(pool)
    .await?;

    if let HashLookupOutcome::Hit(response) = &outcome {
        for entry in response.metadata.iter().flatten() {
            let (Some(source), Some(id)) = (entry.source.as_deref(), entry.id.as_deref()) else { continue };
            let Some(provider) = Provider::from_hasheous_source(source) else { continue };
            if id.is_empty() || id == "0" {
                continue;
            }
            let links = link_for_content(pool, content_id, provider, id, Confidence::Hash).await?;
            record.cross_refs.push((provider, id.to_string(), links));
        }
    }
    Ok(record)
}

/// The `HashLookupRequest` for a content row, from whatever hashes it has.
pub async fn hasheous_request_for_content(pool: &MySqlPool, content_id: u64) -> Result<HashLookupRequest, DatabaseError> {
    let rows = sqlx::query("SELECT algorithm, value_hex FROM hashes WHERE content_id = ?").bind(content_id).fetch_all(pool).await?;
    let mut request = HashLookupRequest::default();
    for row in rows {
        let algorithm: String = row.try_get("algorithm")?;
        let value: String = row.try_get("value_hex")?;
        match algorithm.as_str() {
            "md5" => request.md5 = Some(value),
            "sha1" => request.sha1 = Some(value),
            "sha256" => request.sha256 = Some(value),
            "crc32" => request.crc = Some(value),
            _ => {}
        }
    }
    Ok(request)
}

/// PlayMatch identifies one file: hashes plus (when the content row has them) file name
/// and size, which is what its FileNameAndSize fallback needs.
pub async fn playmatch_request_for_content(pool: &MySqlPool, content_id: u64) -> Result<super::playmatch::IdentifyRequest, DatabaseError> {
    let hashes = hasheous_request_for_content(pool, content_id).await?;
    let row = sqlx::query("SELECT file_name, size FROM content WHERE id = ?").bind(content_id).fetch_optional(pool).await?;
    let (file_name, file_size) = match row {
        Some(r) => (r.try_get::<Option<String>, _>("file_name")?, r.try_get::<Option<u64>, _>("size")?),
        None => (None, None),
    };
    Ok(super::playmatch::IdentifyRequest { file_name, file_size, md5: hashes.md5, sha1: hashes.sha1, sha256: hashes.sha256, crc: hashes.crc })
}

#[derive(Debug, Clone, Default)]
pub struct PlayMatchRecord {
    pub hit: bool,
    pub match_type: Option<String>,
    /// (provider, provider id, links written).
    pub cross_refs: Vec<(Provider, String, u32)>,
}

/// One PlayMatch identify call for a content row: raw response stored, `playmatch_lookups`
/// upserted, every known provider id linked (`Confidence::Hash`) to the releases/titles
/// the content belongs to.
pub async fn playmatch_lookup(clients: &ProviderClients, pool: &MySqlPool, store: &RawStore, content_id: u64, request: &super::playmatch::IdentifyRequest) -> Result<PlayMatchRecord, DatabaseError> {
    let client = clients.playmatch.as_ref().ok_or_else(|| DatabaseError::Provider("playmatch client unavailable".to_string()))?;
    let (response, raw) = client.identify(request).await?;
    record_playmatch(pool, store, content_id, &response, &raw, |_| true).await
}

/// Records one PlayMatch answer for a content (raw file + `playmatch_lookups` row) and, on
/// a hit, links the cross references it names -- only for providers `allowed` accepts (the
/// background gap filler passes "has usable credentials"). Links are `Confidence::Hash`.
pub async fn record_playmatch(pool: &MySqlPool, store: &RawStore, content_id: u64, response: &super::playmatch::IdentifyResponse, raw: &serde_json::Value, allowed: impl Fn(Provider) -> bool) -> Result<PlayMatchRecord, DatabaseError> {
    let mut record = PlayMatchRecord { hit: response.is_hit(), match_type: response.game_match_type.clone(), cross_refs: Vec::new() };
    let raw_path = store.save_json(Provider::PlayMatch, &content_id.to_string(), raw)?;
    sqlx::query(
        "INSERT INTO playmatch_lookups (content_id, raw_response_path, raw_format, hit, match_type, fetched_at, deleted) VALUES (?, ?, 'json', ?, ?, NOW(), FALSE) \
         ON DUPLICATE KEY UPDATE raw_response_path = VALUES(raw_response_path), hit = VALUES(hit), match_type = VALUES(match_type), fetched_at = NOW(), deleted = FALSE",
    )
    .bind(content_id)
    .bind(raw_path.map(|p| p.display().to_string()))
    .bind(record.hit)
    .bind(&record.match_type)
    .execute(pool)
    .await?;
    if record.hit {
        for (provider, id) in response.cross_refs().into_iter().filter(|(p, _)| allowed(*p)) {
            let links = link_for_content(pool, content_id, provider, &id, Confidence::Hash).await?;
            record.cross_refs.push((provider, id, links));
        }
    }
    Ok(record)
}

/// A cached PlayMatch hit's cross references, from its raw file (no links written).
pub async fn playmatch_record_from_cache(pool: &MySqlPool, content_id: u64) -> Result<Option<PlayMatchRecord>, DatabaseError> {
    let row = sqlx::query("SELECT raw_response_path, hit, match_type FROM playmatch_lookups WHERE content_id = ? AND deleted = FALSE").bind(content_id).fetch_optional(pool).await?;
    let Some(row) = row else { return Ok(None) };
    let mut record = PlayMatchRecord { hit: row.try_get("hit")?, match_type: row.try_get("match_type")?, cross_refs: Vec::new() };
    let Some(path) = row.try_get::<Option<String>, _>("raw_response_path")? else { return Ok(Some(record)) };
    let Ok(text) = std::fs::read_to_string(&path) else { return Ok(Some(record)) };
    if let Ok(response) = serde_json::from_str::<super::playmatch::IdentifyResponse>(&text) {
        record.cross_refs = response.cross_refs().into_iter().map(|(p, id)| (p, id, 0)).collect();
    }
    Ok(Some(record))
}

// ---------------------------------------------------------------------------
// Title/release-keyed providers: search + cache
// ---------------------------------------------------------------------------

/// One name search across the providers that have one (platform-scoped where the API
/// allows: IGDB by platform id, EmuReady/ScreenScraper by their system id; the rest ignore
/// it). Providers without a name search (Hasheous, RetroAchievements) get an error.
pub async fn search_by_name(clients: &ProviderClients, pool: &MySqlPool, store: &RawStore, provider: Provider, name: &str, platform: Option<Platform>) -> Result<Vec<CachedEntry>, DatabaseError> {
    match provider {
        Provider::Igdb => igdb_search(clients, pool, store, name, platform).await,
        Provider::TheGamesDb => thegamesdb_search(clients, pool, store, name).await,
        Provider::SteamGridDb => steamgriddb_search(clients, pool, store, name).await,
        Provider::MobyGames => mobygames_search(clients, pool, store, name, None).await,
        Provider::EmuReady => match platform {
            Some(p) => match emuready_system_id(clients, p).await? {
                Some(system_id) => emuready_search(clients, pool, store, &system_id, name).await,
                None => Ok(Vec::new()),
            },
            None => Err(DatabaseError::Provider("emuready search needs a platform".to_string())),
        },
        Provider::ScreenScraper => match platform {
            Some(p) => match screenscraper_system_id(clients, p).await? {
                Some(system_id) => screenscraper_search(clients, pool, store, system_id, name).await,
                None => Ok(Vec::new()),
            },
            None => Err(DatabaseError::Provider("screenscraper search needs a platform".to_string())),
        },
        Provider::RetroAchievements => retroachievements_local_search(clients, pool, name, platform).await,
        Provider::Hasheous | Provider::PlayMatch => Err(DatabaseError::Provider(format!("{} has no name search", provider.name()))),
    }
}

/// RetroAchievements has no search endpoint, so a name search runs over the game lists a
/// `provider_bulk_sync` already cached (`retroachievements_games`), on the platform's
/// console when one is given. Shortest titles first, so an exact name tends to lead.
pub async fn retroachievements_local_search(clients: &ProviderClients, pool: &MySqlPool, name: &str, platform: Option<Platform>) -> Result<Vec<CachedEntry>, DatabaseError> {
    let cached: i64 = sqlx::query("SELECT COUNT(*) AS n FROM retroachievements_games WHERE deleted = FALSE").fetch_one(pool).await?.try_get("n")?;
    if cached == 0 {
        return Err(DatabaseError::Provider("no RetroAchievements game lists cached yet -- run a platform or full sync first (RA has no search API)".to_string()));
    }
    let console = match platform {
        Some(p) => match super::bulk::ra_console_id_for(clients, p).await? {
            Some((id, _)) => Some(id),
            None => return Ok(Vec::new()),
        },
        None => None,
    };
    let pattern = format!("%{}%", name.trim().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
    let rows = match console {
        Some(id) => sqlx::query("SELECT provider_id, title, raw_response_path FROM retroachievements_games WHERE deleted = FALSE AND title LIKE ? AND console_id = ? ORDER BY CHAR_LENGTH(title), title LIMIT 50").bind(&pattern).bind(id).fetch_all(pool).await?,
        None => sqlx::query("SELECT provider_id, title, raw_response_path FROM retroachievements_games WHERE deleted = FALSE AND title LIKE ? ORDER BY CHAR_LENGTH(title), title LIMIT 50").bind(&pattern).fetch_all(pool).await?,
    };
    rows.iter()
        .map(|r| {
            let id: u32 = r.try_get("provider_id")?;
            Ok(CachedEntry { provider: Provider::RetroAchievements, provider_id: id.to_string(), title: r.try_get("title")?, raw_path: r.try_get::<Option<String>, _>("raw_response_path")?.map(PathBuf::from) })
        })
        .collect()
}

/// The search hit to link for `name`: the one whose normalized name equals the query's,
/// else the first result when the provider's own ranking is all there is.
/// IGDB platform ids that name the same hardware for matching purposes (a game filed
/// under one is a valid match for a title on the other).
pub fn sibling_igdb_platforms(id: u16) -> Vec<u16> {
    // IGDB already unifies the JP/US names that share hardware (Mega Drive/Genesis 29,
    // PC Engine/TurboGrafx-16 86, PC Engine CD/TurboGrafx-CD 150, Mega CD/Sega CD 78,
    // Mark III/Master System 64), so those need no entry. What it splits, or what this
    // project keys under its own app-only ids (>= 2048, never a valid IGDB platform), is
    // listed here with IGDB's ids: the first element is what a scoped search should use.
    match id {
        18 | 99 => vec![18, 99],                       // NES / Famicom
        30000 => vec![51, 99, 18],                     // Famicom Disk System (IGDB 51), Famicom, NES
        19 | 58 => vec![19, 58],                       // SNES / Super Famicom
        30008 => vec![58, 19],                         // Sufami Turbo carts are Super Famicom games on IGDB
        33 | 22 => vec![33, 22],                       // Game Boy / Game Boy Color
        86 | 128 => vec![86, 128],                     // PC Engine / SuperGrafx
        79 | 80 => vec![79, 80],                       // Neo Geo MVS / AES
        30004 => vec![57],                             // WonderSwan
        30005 => vec![123, 57],                        // WonderSwan Color (+ mono titles)
        30006 => vec![339],                            // Sega Pico
        30007 => vec![416, 4],                         // 64DD, Nintendo 64
        30001 => vec![441, 7],                         // PocketStation, PlayStation
        30002 | 30003 => vec![53, 27],                 // MSX turbo R / MSX2+ -> MSX2, MSX
        other => vec![other],
    }
}

/// The IGDB platforms a per-platform sync of app platform `id` pages through: its first
/// sibling (what a scoped search uses) and, where IGDB files one machine's regional releases
/// apart, the other region's id -- a Japan-only SNES game is on the Super Famicom (58), a
/// Japan-only NES game on the Famicom (99). Other siblings (Game Boy Color for Game Boy, ...)
/// are different hardware with their own games and stay out. Empty: no IGDB platform.
pub fn igdb_sync_platforms(id: u16) -> Vec<u16> {
    let first = sibling_igdb_platforms(id)[0];
    match first {
        0 => Vec::new(),
        f if f >= 2048 => Vec::new(),
        19 => vec![19, 58],
        58 => vec![58, 19],
        18 => vec![18, 99],
        99 => vec![99, 18],
        f => vec![f],
    }
}

/// Keeps the search hits whose provider entry is for `platform` (or a sibling). Only
/// IGDB carries a platform list in its cached game; other providers pass through
/// unchanged. No platform -> unchanged.
pub async fn platform_compatible(pool: &MySqlPool, provider: Provider, entries: Vec<CachedEntry>, platform: Option<Platform>) -> Result<Vec<CachedEntry>, DatabaseError> {
    let (Provider::Igdb, Some(platform)) = (provider, platform) else { return Ok(entries) };
    let wanted = sibling_igdb_platforms(platform.igdb_id());
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        let Some(id) = e.provider_id.parse::<i32>().ok() else { continue };
        let Some(game) = load_cached_igdb_game(pool, id).await? else { continue };
        let platforms = game.platforms.unwrap_or_default();
        if platforms.is_empty() || platforms.iter().any(|p| wanted.contains(&(*p as u16))) {
            out.push(e);
        }
    }
    Ok(out)
}

/// Only a hit whose normalized name equals the query's (for unscoped fallbacks, where
/// "first result" could be a different platform's game).
/// Names compare by `loose_key`, so a DAT's "Fu-Ga" is the provider's "FuGa".
pub fn exact_hit<'a>(entries: &'a [CachedEntry], name: &str) -> Option<&'a CachedEntry> {
    let key = matching::util::loose_key(name);
    entries.iter().find(|e| e.title.as_deref().map(matching::util::loose_key).as_deref() == Some(key.as_str()))
}

pub fn best_hit<'a>(entries: &'a [CachedEntry], name: &str) -> Option<&'a CachedEntry> {
    exact_hit(entries, name).or(entries.first())
}

pub async fn igdb_search(clients: &ProviderClients, pool: &MySqlPool, store: &RawStore, name: &str, platform: Option<Platform>) -> Result<Vec<CachedEntry>, DatabaseError> {
    let client = clients.igdb.as_ref().ok_or_else(|| DatabaseError::Provider("igdb client unavailable".to_string()))?;
    // Scoped to every IGDB platform the app's counts as the same machine: IGDB files a
    // Japan-only Super Nintendo game under the Super Famicom (58), not the SNES (19), so a
    // search of 19 alone missed such games and found only others sharing a word. App-only ids
    // search under their IGDB siblings (Famicom Disk System -> 51, ...).
    let scoped: Vec<u16> = platform.filter(|p| p.igdb_id() != 0).map(|p| sibling_igdb_platforms(p.igdb_id()).into_iter().filter(|id| *id < 2048).collect()).unwrap_or_default();
    // A name with a hyphen inside a word ("Hoge Fu-Ga") is searched as written and joined
    // ("Hoge FuGa"): IGDB's search splits the word at the hyphen and does not find the game
    // under its own spelling.
    let joined = matching::util::join_word_hyphens(name);
    let spellings: Vec<&str> = if joined != name { vec![name, joined.as_str()] } else { vec![name] };
    let mut games: Vec<playmatch_deredis::providers::igdb::model::Game> = Vec::new();
    for spelling in spellings {
        let found = if scoped.is_empty() {
            client.search_game_by_name(spelling).await?
        } else {
            let mut all = Vec::new();
            for id in &scoped {
                all.extend(client.search_game_by_name_and_platform(spelling, *id as i32).await?);
            }
            all
        };
        for game in found {
            if !games.iter().any(|g| g.id == game.id) {
                games.push(game);
            }
        }
    }
    let mut out = Vec::with_capacity(games.len());
    for game in &games {
        let raw = serde_json::to_value(game)?;
        out.push(upsert_game(pool, store, Provider::Igdb, &game.id.to_string(), Some(&game.name), &raw).await?);
    }
    Ok(out)
}

pub async fn igdb_games_by_ids(clients: &ProviderClients, pool: &MySqlPool, store: &RawStore, ids: &[i32]) -> Result<Vec<CachedEntry>, DatabaseError> {
    Ok(igdb_games_by_ids_full(clients, pool, store, ids).await?.into_iter().map(|(e, _)| e).collect())
}

/// `igdb_games_by_ids` keeping the parsed games (for metadata application).
pub async fn igdb_games_by_ids_full(clients: &ProviderClients, pool: &MySqlPool, store: &RawStore, ids: &[i32]) -> Result<Vec<(CachedEntry, playmatch_deredis::providers::igdb::model::Game)>, DatabaseError> {
    let client = clients.igdb.as_ref().ok_or_else(|| DatabaseError::Provider("igdb client unavailable".to_string()))?;
    let games = client.get_games_by_id(ids.to_vec()).await?;
    let mut out = Vec::with_capacity(games.len());
    for game in games {
        let raw = serde_json::to_value(&game)?;
        let entry = upsert_game(pool, store, Provider::Igdb, &game.id.to_string(), Some(&game.name), &raw).await?;
        out.push((entry, game));
    }
    Ok(out)
}

pub async fn thegamesdb_search(clients: &ProviderClients, pool: &MySqlPool, store: &RawStore, name: &str) -> Result<Vec<CachedEntry>, DatabaseError> {
    let client = clients.thegamesdb.as_ref().ok_or_else(|| DatabaseError::Provider("thegamesdb client unavailable".to_string()))?;
    let result = client.search_by_name(name).await?;
    let mut out = Vec::with_capacity(result.games.len());
    for game in &result.games {
        let raw = serde_json::to_value(game)?;
        out.push(upsert_game(pool, store, Provider::TheGamesDb, &game.id.to_string(), game.game_title.as_deref(), &raw).await?);
    }
    super::bulk::set_thegamesdb_regions(pool, &result.games.iter().collect::<Vec<_>>()).await?;
    Ok(out)
}

pub async fn steamgriddb_search(clients: &ProviderClients, pool: &MySqlPool, store: &RawStore, name: &str) -> Result<Vec<CachedEntry>, DatabaseError> {
    let client = clients.steamgriddb.as_ref().ok_or_else(|| DatabaseError::Provider("steamgriddb client unavailable".to_string()))?;
    let games = client.search_games(name).await?;
    let mut out = Vec::with_capacity(games.len());
    for game in &games {
        let raw = serde_json::to_value(game)?;
        out.push(upsert_game(pool, store, Provider::SteamGridDb, &game.id.to_string(), Some(&game.name), &raw).await?);
    }
    Ok(out)
}

pub async fn mobygames_search(clients: &ProviderClients, pool: &MySqlPool, store: &RawStore, name: &str, moby_platform_id: Option<i64>) -> Result<Vec<CachedEntry>, DatabaseError> {
    let client = clients.mobygames.as_ref().ok_or_else(|| DatabaseError::Provider("mobygames client unavailable".to_string()))?;
    let games = client.search_games(moby_platform_id, name).await.map_err(|err| {
        // MobyGames answers a bad/placeholder key with an error object instead of the
        // `{"games": [...]}` envelope; the vendored client reports that as a parse failure.
        let text = format!("{err:#}");
        if text.contains("missing field `games`") {
            DatabaseError::Provider("mobygames: response had no `games` field -- the API rejected the request (is mob_api_key in api.env a real key?)".to_string())
        } else {
            DatabaseError::Provider(text)
        }
    })?;
    let mut out = Vec::with_capacity(games.len());
    for game in &games {
        let raw = serde_json::to_value(game)?;
        out.push(upsert_game(pool, store, Provider::MobyGames, &game.game_id.to_string(), Some(&game.title), &raw).await?);
    }
    Ok(out)
}

/// EmuReady's system id whose name matches `platform`'s display name (normalized).
pub async fn emuready_system_id(clients: &ProviderClients, platform: Platform) -> Result<Option<String>, DatabaseError> {
    let client = clients.emuready.as_ref().ok_or_else(|| DatabaseError::Provider("emuready client unavailable".to_string()))?;
    let systems = client.list_systems().await?;
    // By EmuReady's own key where it is known (`api::providers::EMUREADY_SYSTEMS`): names
    // are matched only for a system it has added since.
    if let Some((_, key)) = crate::api::providers::EMUREADY_SYSTEMS.iter().find(|(p, _)| *p == platform) {
        if let Some(system) = systems.iter().find(|s| s.key.as_deref() == Some(*key)) {
            return Ok(Some(system.id.clone()));
        }
    }
    let wanted = super::super::platform_map::norm_key(platform.display_name());
    let alt = super::super::platform_map::norm_key(&platform.to_string());
    Ok(systems.iter().find(|s| {
        let k = super::super::platform_map::norm_key(&s.name);
        k == wanted || k == alt || super::super::platform_map::platform_from_dat_name(&s.name) == platform
    }).map(|s| s.id.clone()))
}

pub async fn emuready_search(clients: &ProviderClients, pool: &MySqlPool, store: &RawStore, system_id: &str, name: &str) -> Result<Vec<CachedEntry>, DatabaseError> {
    let client = clients.emuready.as_ref().ok_or_else(|| DatabaseError::Provider("emuready client unavailable".to_string()))?;
    let games = client.search_games(system_id, name).await?;
    let mut out = Vec::with_capacity(games.len());
    for game in &games {
        let raw = serde_json::to_value(game)?;
        out.push(upsert_game(pool, store, Provider::EmuReady, &game.id, Some(&game.title), &raw).await?);
    }
    Ok(out)
}

/// ScreenScraper's system id for `platform`, by matching its localized system names.
pub async fn screenscraper_system_id(clients: &ProviderClients, platform: Platform) -> Result<Option<i32>, DatabaseError> {
    let client = clients.screenscraper.as_ref().ok_or_else(|| DatabaseError::Provider("screenscraper client unavailable".to_string()))?;
    let systems = client.list_systems().await?;
    let wanted = super::super::platform_map::norm_key(platform.display_name());
    Ok(systems.iter().find(|s| s.iter_names().any(|n| super::super::platform_map::norm_key(n) == wanted || super::super::platform_map::platform_from_dat_name(n) == platform)).map(|s| s.id))
}

pub async fn screenscraper_by_hashes(clients: &ProviderClients, pool: &MySqlPool, store: &RawStore, content_id: Option<u64>, system_id: i32, rom_name: &str, rom_size: Option<i64>, md5: Option<&str>, sha1: Option<&str>, crc: Option<&str>) -> Result<Option<CachedEntry>, DatabaseError> {
    let client = clients.screenscraper.as_ref().ok_or_else(|| DatabaseError::Provider("screenscraper client unavailable".to_string()))?;
    let Some(game) = client.get_game_by_hashes(system_id, rom_name, rom_size, md5, sha1, crc).await? else { return Ok(None) };
    let Some(jeu_id) = game.id else { return Ok(None) };
    let title = game.iter_candidate_names().next().map(str::to_string);
    let raw = serde_json::to_value(&game)?;
    let entry = upsert_game(pool, store, Provider::ScreenScraper, &jeu_id.to_string(), title.as_deref(), &raw).await?;
    if let Some(content_id) = content_id {
        sqlx::query("INSERT INTO screenscraper_rom_index (content_id, jeu_id, fetched_at) VALUES (?, ?, NOW()) ON DUPLICATE KEY UPDATE jeu_id = VALUES(jeu_id), fetched_at = NOW()").bind(content_id).bind(jeu_id as u64).execute(pool).await?;
        link_for_content(pool, content_id, Provider::ScreenScraper, &jeu_id.to_string(), Confidence::Exact).await?;
    }
    Ok(Some(entry))
}

pub async fn screenscraper_search(clients: &ProviderClients, pool: &MySqlPool, store: &RawStore, system_id: i32, name: &str) -> Result<Vec<CachedEntry>, DatabaseError> {
    let client = clients.screenscraper.as_ref().ok_or_else(|| DatabaseError::Provider("screenscraper client unavailable".to_string()))?;
    let games = client.search_games(system_id, name).await?;
    let mut out = Vec::new();
    for game in &games {
        let Some(jeu_id) = game.id else { continue };
        let title = game.iter_candidate_names().next().map(str::to_string);
        let raw = serde_json::to_value(game)?;
        out.push(upsert_game(pool, store, Provider::ScreenScraper, &jeu_id.to_string(), title.as_deref(), &raw).await?);
    }
    Ok(out)
}

/// One RA game by id, cached with its raw response; genre normalized into `genre`/
/// `title_genres` when the game is linked to a title.
pub async fn retroachievements_game(clients: &ProviderClients, pool: &MySqlPool, store: &RawStore, game_id: u32) -> Result<(CachedEntry, super::retroachievements::RaGame), DatabaseError> {
    let client = clients.retroachievements.as_ref().ok_or_else(|| DatabaseError::Provider("retroachievements client unavailable".to_string()))?;
    let (game, raw) = client.get_game(game_id).await?;
    let entry = upsert_ra_game(pool, store, game.id, Some(game.console_id), Some(&game.title), game.num_achievements, game.genre.as_deref(), &raw).await?;
    Ok((entry, game))
}

/// Records normalized genres for a title -- see `metadata::set_title_genres` (source-tagged).
pub async fn set_title_genres(pool: &MySqlPool, title_id: u64, source: &str, genres: &[&str]) -> Result<(), DatabaseError> {
    super::metadata::set_title_genres(pool, title_id, source, genres).await
}

/// A cached IGDB game, deserialized back from its raw response file (for applying metadata
/// without another request). The file is one game, or -- from a bulk sync -- a page of them,
/// and then this one is picked out: read as a single game, such a file used to give nothing.
pub async fn load_cached_igdb_game(pool: &MySqlPool, provider_id: i32) -> Result<Option<playmatch_deredis::providers::igdb::model::Game>, DatabaseError> {
    let row = sqlx::query("SELECT raw_response_path FROM igdb_games WHERE provider_id = ? AND deleted = FALSE").bind(provider_id).fetch_optional(pool).await?;
    let Some(path) = row.and_then(|r| r.try_get::<Option<String>, _>("raw_response_path").ok().flatten()) else { return Ok(None) };
    let Ok(text) = std::fs::read_to_string(&path) else { return Ok(None) };
    Ok(match serde_json::from_str::<serde_json::Value>(&text).ok() {
        Some(serde_json::Value::Array(games)) => games.into_iter().find(|g| g.get("id").and_then(|v| v.as_i64()) == Some(provider_id as i64)).and_then(|g| serde_json::from_value(g).ok()),
        Some(game) => serde_json::from_value(game).ok(),
        None => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_is_found_in_a_page_of_games() {
        let page = serde_json::json!([{"id": 1, "name": "a"}, {"id": 2, "name": "b"}]);
        assert_eq!(entry_for(page.clone(), "2").and_then(|e| e.get("name").cloned()), Some(serde_json::json!("b")));
        assert_eq!(entry_for(page, "3"), None, "not in the page: nothing, not the first game");
        assert_eq!(entry_for(serde_json::json!([{"ID": "7"}]), "7"), Some(serde_json::json!({"ID": "7"})), "keys and string ids vary by provider");
        assert_eq!(entry_for(serde_json::json!({"id": 9}), "9"), Some(serde_json::json!({"id": 9})), "one game: itself");
    }

    #[test]
    fn a_file_in_three_titles_is_shared() {
        assert!(!is_shared(&[(1, Some(10)), (2, Some(10)), (3, Some(11))]), "two titles: still identifying");
        assert!(!is_shared(&[(1, None), (2, None), (3, None)]), "releases without titles count none");
        assert!(is_shared(&[(1, Some(10)), (2, Some(11)), (3, Some(12))]));
    }

    #[test]
    fn sibling_platforms_bridge_igdb_splits_and_app_only_ids() {
        assert_eq!(sibling_igdb_platforms(18), vec![18, 99]);
        assert_eq!(sibling_igdb_platforms(99), vec![18, 99]);
        assert_eq!(sibling_igdb_platforms(30000)[0], 51, "FDS searches IGDB's own platform id first");
        assert!(sibling_igdb_platforms(30000).contains(&99));
        assert_eq!(sibling_igdb_platforms(29), vec![29], "Mega Drive/Genesis is one IGDB platform already");
        assert_eq!(sibling_igdb_platforms(86), vec![86, 128]);
        assert_eq!(sibling_igdb_platforms(78), vec![78], "Mega CD/Sega CD is one IGDB platform already");
        assert!(sibling_igdb_platforms(30004)[0] < 2048, "app-only ids map to a real IGDB id");
    }
}
