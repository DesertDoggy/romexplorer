//! Families: a game, its official versions (ports, remakes, remasters, editions) and the
//! collections that hold it -- what Collection > Definitive lists, one row per family.
//!
//! A family is the titles joined by `title.basegame_title_id`, through every level (a
//! remaster of a remake of hoge is in hoge's family), plus the titles of one IGDB game on
//! several platforms. Its head is the top of that chain; every title of a family of more than
//! one stores the head in `title.family_root_id` ([`recompute_roots`]). A collection
//! (`category = 'bundle'`) is not joined by a base game: `title_bundle_games` lists what it
//! holds, and it is a member of each of those games' families.
//!
//! Which version leads a family -- its *definitive* version -- is worked out here
//! ([`rank`]) and kept in `title_families` ([`refresh`]), so the list can sort and page by it:
//! the user's own order first, then the newest version, and within one version the platform
//! priority (with the Definitive tab's own PC entry, [`PC_DEFINITIVE`]). An older PC version
//! moves up next to the newest version unless it is [`KEY_PC_MAX_AGE`] years older.
//!
//! The family pass ([`run`], a step of the title merge job) also adds the official versions
//! IGDB knows that the catalogue has no release of: a PS5 or Switch 2 version no DAT lists, a
//! PC original. Each becomes a release with no files (source `igdb`) under a title of its
//! own, just like a release of a DAT that nobody owns.

use std::collections::{BTreeMap, HashMap, HashSet};

use playmatch_deredis::providers::igdb::model::Game as IgdbGame;
use sqlx::mysql::MySqlPool;
use sqlx::Row;

use super::error::DatabaseError;
use super::jobs::Progress;
use super::providers::cache::{self, Confidence};
use super::providers::raw_store::RawStore;
use super::providers::{Provider, ProviderClients};
use crate::common::models::Platform;

/// The platform-priority entry that stands for PC on the Definitive tab alone: placed where
/// the user wants PC versions among the consoles, while Collection > Cross-platform keeps
/// ranking PC by its own entry. Never a real platform id.
pub const PC_DEFINITIVE: u16 = 65001;

/// `definitive_pc_max_age`: an older version's PC release moves up next to the newest
/// version unless it is this many years older or more (default 10; 0 = never moves up).
pub const KEY_PC_MAX_AGE: &str = "definitive_pc_max_age";
pub const DEFAULT_PC_MAX_AGE: u16 = 10;

/// `definitive_two_cover_layout`: how the Definitive tab's two-cover grid puts the two
/// covers together: top_bottom (default) | diagonal | left_right.
pub const KEY_TWO_COVER_LAYOUT: &str = "definitive_two_cover_layout";
pub const TWO_COVER_LAYOUTS: [&str; 3] = ["top_bottom", "diagonal", "left_right"];

/// `definitive_fetch_versions`: where a family's other versions (older and newer, the base
/// game too) come from. `off` (default): what is here already -- the catalogue's titles, and
/// unowned versions only on platforms whose IGDB metadata is synced. `per_title`: for the
/// games held on disk, the whole family is fetched from IGDB by id, on any platform.
/// `whole_platform` (deprecated): `per_title`, and every platform such a version is on gets
/// its IGDB metadata synced too.
pub const KEY_FETCH_VERSIONS: &str = "definitive_fetch_versions";
pub const FETCH_VERSIONS: [&str; 3] = ["off", "per_title", "whole_platform"];

/// `definitive_fetch_versions` as stored, else `off`.
pub async fn fetch_versions(pool: &MySqlPool) -> Result<&'static str, DatabaseError> {
    let v = super::settings::get::<String>(pool, KEY_FETCH_VERSIONS).await?;
    Ok(FETCH_VERSIONS.iter().find(|m| Some(**m) == v.as_deref()).copied().unwrap_or("off"))
}

/// The categories a family member can have: the game and its official versions, and the
/// collections that hold it. DLC, expansions (standalone ones too) and mods stay out.
pub const MEMBER_CATEGORIES: &str = "'base_game','port','remake','remaster','edition','bundle'";

/// Every category, as the schema has them.
pub const CATEGORIES: [&str; 10] = ["base_game", "port", "remake", "remaster", "edition", "dlc", "expansion", "standalone", "bundle", "mod"];

/// The source a provider-only release is recorded under.
pub const PROVIDER_ONLY_SOURCE: &str = "igdb";

/// Whether release `alias` is a provider-only version (no DAT lists it, no file can be one).
pub fn provider_only_sql(alias: &str) -> String {
    // Null-safe: with no such source yet, nothing is provider-only (not NULL, which a NOT
    // would turn into "no row at all").
    format!("{alias}.source_id <=> (SELECT so.id FROM sources so WHERE so.name = '{PROVIDER_ONLY_SOURCE}')")
}

// ---------------------------------------------------------------------------------------
// The IGDB index: what a cached game says about its family, read once.
// ---------------------------------------------------------------------------------------

/// One game's row of the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameIndex {
    pub id: u32,
    pub game_type: &'static str,
    pub first_year: Option<u16>,
    /// `first_release_date` as a day.
    pub first_release: Option<chrono::NaiveDate>,
    pub platforms: Vec<u16>,
    /// (related game, relation) -- see `igdb_game_relations`.
    pub relations: Vec<(u32, &'static str)>,
}

pub fn index_of(game: &IgdbGame) -> GameIndex {
    let mut relations: Vec<(u32, &'static str)> = Vec::new();
    if let Some(p) = game.parent_game.or(game.version_parent).filter(|p| *p > 0) {
        relations.push((p as u32, "parent"));
    }
    for (list, rel) in [(&game.ports, "port"), (&game.remakes, "remake"), (&game.remasters, "remaster"), (&game.bundles, "bundle")] {
        for id in list.iter().flatten().filter(|id| **id > 0) {
            relations.push((*id as u32, rel));
        }
    }
    relations.sort_unstable();
    relations.dedup();
    let first_day = game.first_release_date.and_then(|t| chrono::DateTime::from_timestamp(t, 0)).map(|d| d.date_naive()).filter(|d| (1950..=2200).contains(&chrono::Datelike::year(d)));
    let first_year = first_day.map(|d| chrono::Datelike::year(&d) as u16);
    let mut platforms: Vec<u16> = game.platforms.iter().flatten().filter(|p| **p > 0 && **p < 2048).map(|p| *p as u16).collect();
    platforms.sort_unstable();
    platforms.dedup();
    GameIndex { id: game.id as u32, game_type: super::providers::metadata::igdb_category(game), first_year, first_release: first_day, platforms, relations }
}

/// Writes the index rows of `games` (their `igdb_games` row must exist).
pub async fn write_index(pool: &MySqlPool, games: &[GameIndex]) -> Result<(), DatabaseError> {
    for chunk in games.chunks(500) {
        if chunk.is_empty() {
            continue;
        }
        let mut tx = pool.begin().await?;
        let ids = chunk.iter().map(|g| g.id.to_string()).collect::<Vec<_>>().join(",");
        sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM igdb_game_relations WHERE game_id IN ({ids})"))).execute(&mut *tx).await?;
        let rels: Vec<String> = chunk.iter().flat_map(|g| g.relations.iter().map(move |(r, rel)| format!("({},{r},'{rel}')", g.id))).collect();
        for part in rels.chunks(1000) {
            sqlx::query(sqlx::AssertSqlSafe(format!("INSERT IGNORE INTO igdb_game_relations (game_id, related_id, relation) VALUES {}", part.join(",")))).execute(&mut *tx).await?;
        }
        for g in chunk {
            let platforms = g.platforms.iter().map(u16::to_string).collect::<Vec<_>>().join(",");
            sqlx::query("UPDATE igdb_games SET game_type = ?, first_year = ?, first_release = ?, platforms = ?, relations_read = TRUE, release_read = TRUE WHERE provider_id = ?")
                .bind(g.game_type)
                .bind(g.first_year)
                .bind(g.first_release)
                .bind(platforms.get(..platforms.len().min(512)))
                .bind(g.id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
    }
    Ok(())
}

/// After an IGDB game was cached (`cache::upsert_game`): its index, or a mark that it still
/// has to be read when the raw response is not one game.
pub async fn index_cached(pool: &MySqlPool, provider_id: &str, raw: &serde_json::Value) -> Result<(), DatabaseError> {
    match serde_json::from_value::<IgdbGame>(raw.clone()) {
        Ok(game) => write_index(pool, &[index_of(&game)]).await,
        Err(_) => {
            sqlx::query("UPDATE igdb_games SET relations_read = FALSE WHERE provider_id = ?").bind(provider_id).execute(pool).await?;
            Ok(())
        }
    }
}

/// Reads the first-release day of every cached IGDB game indexed before it was kept
/// (`release_read`), a raw file at a time. Only that column: the rest of the index stands.
pub async fn backfill_release_dates(pool: &MySqlPool, progress: &Progress) -> Result<u64, DatabaseError> {
    let rows = sqlx::query("SELECT provider_id, raw_response_path FROM igdb_games WHERE release_read = FALSE AND relations_read = TRUE AND deleted = FALSE AND raw_response_path IS NOT NULL ORDER BY raw_response_path")
        .fetch_all(pool)
        .await?;
    let total = rows.len() as u64;
    let mut by_file: BTreeMap<String, HashSet<u32>> = BTreeMap::new();
    for r in &rows {
        by_file.entry(r.try_get("raw_response_path")?).or_default().insert(r.try_get::<u32, _>("provider_id")?);
    }
    let mut done = 0u64;
    let mut found: Vec<(u32, Option<chrono::NaiveDate>)> = Vec::new();
    let flush = |found: Vec<(u32, Option<chrono::NaiveDate>)>| async move {
        let mut tx = pool.begin().await?;
        for (id, day) in found {
            sqlx::query("UPDATE igdb_games SET first_release = ?, release_read = TRUE WHERE provider_id = ?").bind(day).bind(id).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok::<(), DatabaseError>(())
    };
    for (path, wanted) in by_file {
        progress.report(done, Some(total), &format!("families: reading release dates {done} of {total}")).await;
        let games: Vec<IgdbGame> = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .map(|v| match v {
                serde_json::Value::Array(a) => a.into_iter().filter_map(|g| serde_json::from_value(g).ok()).collect(),
                other => serde_json::from_value(other).ok().into_iter().collect(),
            })
            .unwrap_or_default();
        let mut seen: HashSet<u32> = HashSet::new();
        for g in games.iter().filter(|g| wanted.contains(&(g.id as u32))) {
            seen.insert(g.id as u32);
            found.push((g.id as u32, index_of(g).first_release));
        }
        for id in wanted.difference(&seen) {
            found.push((*id, None));
        }
        done += wanted.len() as u64;
        if found.len() >= 1000 {
            flush(std::mem::take(&mut found)).await?;
        }
    }
    flush(found).await?;
    Ok(total)
}

/// Reads the index out of every cached IGDB game not read yet, a raw file at a time.
pub async fn backfill_index(pool: &MySqlPool, progress: &Progress) -> Result<u64, DatabaseError> {
    let rows = sqlx::query("SELECT provider_id, raw_response_path FROM igdb_games WHERE relations_read = FALSE AND deleted = FALSE AND raw_response_path IS NOT NULL ORDER BY raw_response_path")
        .fetch_all(pool)
        .await?;
    let total = rows.len() as u64;
    let mut by_file: BTreeMap<String, HashSet<u32>> = BTreeMap::new();
    for r in &rows {
        by_file.entry(r.try_get("raw_response_path")?).or_default().insert(r.try_get::<u32, _>("provider_id")?);
    }
    let mut done = 0u64;
    let mut batch: Vec<GameIndex> = Vec::new();
    for (path, wanted) in by_file {
        progress.report(done, Some(total), &format!("families: reading IGDB game {done} of {total}")).await;
        let games: Vec<IgdbGame> = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .map(|v| match v {
                serde_json::Value::Array(a) => a.into_iter().filter_map(|g| serde_json::from_value(g).ok()).collect(),
                other => serde_json::from_value(other).ok().into_iter().collect(),
            })
            .unwrap_or_default();
        let mut found: HashSet<u32> = HashSet::new();
        for g in games.iter().filter(|g| wanted.contains(&(g.id as u32))) {
            found.insert(g.id as u32);
            batch.push(index_of(g));
        }
        // A file that does not hold the game says nothing more next time: marked read empty.
        for id in wanted.difference(&found) {
            batch.push(GameIndex { id: *id, game_type: "base_game", first_year: None, first_release: None, platforms: Vec::new(), relations: Vec::new() });
        }
        done += wanted.len() as u64;
        if batch.len() >= 500 {
            write_index(pool, &batch).await?;
            batch.clear();
        }
    }
    write_index(pool, &batch).await?;
    Ok(total)
}

// ---------------------------------------------------------------------------------------
// Families
// ---------------------------------------------------------------------------------------

/// The head of each family, from the base-game links and the titles that share an IGDB
/// game: `edges` join two titles, `info` is (title, has a base game, is a base game,
/// earliest year) for every title in an edge. A family's head is a title with no base game,
/// a base game before a version, the earliest, then the lowest id.
pub fn heads(edges: &[(u64, u64)], info: &HashMap<u64, (bool, bool, Option<u16>)>) -> HashMap<u64, u64> {
    let mut parent: HashMap<u64, u64> = HashMap::new();
    fn find(parent: &mut HashMap<u64, u64>, x: u64) -> u64 {
        let mut root = x;
        while let Some(&p) = parent.get(&root) {
            if p == root {
                break;
            }
            root = p;
        }
        let mut cur = x;
        while let Some(&p) = parent.get(&cur) {
            if p == root {
                break;
            }
            parent.insert(cur, root);
            cur = p;
        }
        root
    }
    for (a, b) in edges {
        parent.entry(*a).or_insert(*a);
        parent.entry(*b).or_insert(*b);
        let (ra, rb) = (find(&mut parent, *a), find(&mut parent, *b));
        if ra != rb {
            parent.insert(ra.max(rb), ra.min(rb));
        }
    }
    let ids: Vec<u64> = parent.keys().copied().collect();
    let mut groups: HashMap<u64, Vec<u64>> = HashMap::new();
    for id in ids {
        let r = find(&mut parent, id);
        groups.entry(r).or_default().push(id);
    }
    let mut out = HashMap::new();
    for members in groups.values() {
        if members.len() < 2 {
            continue;
        }
        let key = |id: &u64| {
            let (has_base, is_base, year) = info.get(id).copied().unwrap_or((true, false, None));
            (has_base, !is_base, year.unwrap_or(u16::MAX), *id)
        };
        let head = *members.iter().min_by_key(|id| key(id)).expect("non-empty");
        for m in members {
            out.insert(*m, head);
        }
    }
    out
}

/// Sets `title.family_root_id` from the links as they are now. Returns the titles changed.
pub async fn recompute_roots(pool: &MySqlPool) -> Result<u64, DatabaseError> {
    let mut edges: Vec<(u64, u64)> = Vec::new();
    for r in sqlx::query("SELECT t.id, t.basegame_title_id FROM title t JOIN title b ON b.id = t.basegame_title_id AND b.deleted = FALSE WHERE t.deleted = FALSE AND t.basegame_title_id IS NOT NULL AND t.basegame_title_id <> t.id")
        .fetch_all(pool)
        .await?
    {
        edges.push((r.try_get("id")?, r.try_get("basegame_title_id")?));
    }
    // Titles of one IGDB game (one game on several platforms), but never through a collection
    // or an add-on linked to its game's id.
    let shared = sqlx::query(
        "SELECT l.provider_id, l.title_id FROM title_provider_links l JOIN title t ON t.id = l.title_id AND t.deleted = FALSE AND t.category NOT IN ('bundle','dlc','mod') \
         WHERE l.provider = 'igdb' AND l.deleted = FALSE AND l.provider_id IN \
         (SELECT l2.provider_id FROM title_provider_links l2 JOIN title t2 ON t2.id = l2.title_id AND t2.deleted = FALSE WHERE l2.provider = 'igdb' AND l2.deleted = FALSE GROUP BY l2.provider_id HAVING COUNT(DISTINCT l2.title_id) > 1) \
         ORDER BY l.provider_id, l.title_id",
    )
    .fetch_all(pool)
    .await?;
    let mut first: HashMap<String, u64> = HashMap::new();
    for r in &shared {
        let (game, title): (String, u64) = (r.try_get("provider_id")?, r.try_get("title_id")?);
        match first.get(&game) {
            Some(f) => edges.push((*f, title)),
            None => {
                first.insert(game, title);
            }
        }
    }
    // Two versions of one version set (one game sold side by side, a third version included)
    // never share a family, whatever IGDB links: each keeps its own line of remakes.
    let set_of = super::versions::set_versions(pool).await?;
    edges.retain(|(a, b)| match (set_of.get(a), set_of.get(b)) {
        (Some((sa, va)), Some((sb, vb))) => sa != sb || va == vb,
        _ => true,
    });
    let ids: HashSet<u64> = edges.iter().flat_map(|(a, b)| [*a, *b]).collect();
    let mut info: HashMap<u64, (bool, bool, Option<u16>)> = HashMap::new();
    let list: Vec<u64> = ids.into_iter().collect();
    for chunk in list.chunks(1000) {
        let sql = format!(
            "SELECT t.id, t.basegame_title_id IS NOT NULL AS has_base, t.category = 'base_game' AS is_base, \
               (SELECT CAST(MIN(COALESCE(YEAR(r.released_on), r.year)) AS SIGNED) FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE AND r.material IS NULL WHERE tr.title_id = t.id) AS year \
             FROM title t WHERE t.id IN ({})",
            chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",")
        );
        for r in sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(pool).await? {
            let year: Option<i64> = r.try_get("year")?;
            info.insert(r.try_get("id")?, (r.try_get::<i64, _>("has_base")? != 0, r.try_get::<i64, _>("is_base")? != 0, year.and_then(|y| u16::try_from(y).ok())));
        }
    }
    let heads = heads(&edges, &info);
    let mut current: HashMap<u64, u64> = HashMap::new();
    for r in sqlx::query("SELECT id, family_root_id FROM title WHERE family_root_id IS NOT NULL").fetch_all(pool).await? {
        current.insert(r.try_get("id")?, r.try_get("family_root_id")?);
    }
    let mut changed = 0u64;
    let mut tx = pool.begin().await?;
    for (id, head) in &heads {
        if current.get(id) != Some(head) {
            sqlx::query("UPDATE title SET family_root_id = ? WHERE id = ?").bind(head).bind(id).execute(&mut *tx).await?;
            changed += 1;
        }
    }
    for id in current.keys().filter(|id| !heads.contains_key(id)) {
        sqlx::query("UPDATE title SET family_root_id = NULL WHERE id = ?").bind(id).execute(&mut *tx).await?;
        changed += 1;
    }
    tx.commit().await?;
    Ok(changed)
}

/// The games each collection title holds, from IGDB: a title linked to a game that lists the
/// collection's game among its bundles. The user's own rows, and collections whose base games
/// the user set, are left alone. Returns the rows now from IGDB.
pub async fn link_bundles(pool: &MySqlPool) -> Result<u64, DatabaseError> {
    let rows = sqlx::query(
        "SELECT DISTINCT bl.title_id AS bundle, gl.title_id AS game FROM igdb_game_relations r \
         JOIN title_provider_links bl ON bl.provider = 'igdb' AND bl.deleted = FALSE AND bl.provider_id = CAST(r.related_id AS CHAR) \
         JOIN title b ON b.id = bl.title_id AND b.deleted = FALSE AND b.basegame_locked = FALSE \
         JOIN title_provider_links gl ON gl.provider = 'igdb' AND gl.deleted = FALSE AND gl.provider_id = CAST(r.game_id AS CHAR) \
         JOIN title g ON g.id = gl.title_id AND g.deleted = FALSE AND g.category <> 'bundle' \
         WHERE r.relation = 'bundle' AND bl.title_id <> gl.title_id",
    )
    .fetch_all(pool)
    .await?;
    let mut want: HashSet<(u64, u64)> = HashSet::new();
    for r in &rows {
        want.insert((r.try_get("bundle")?, r.try_get("game")?));
    }
    let mut have: HashSet<(u64, u64)> = HashSet::new();
    for r in sqlx::query("SELECT bundle_title_id, game_title_id FROM title_bundle_games WHERE source = 'igdb'").fetch_all(pool).await? {
        have.insert((r.try_get("bundle_title_id")?, r.try_get("game_title_id")?));
    }
    let mut tx = pool.begin().await?;
    for (b, g) in have.difference(&want) {
        sqlx::query("DELETE FROM title_bundle_games WHERE bundle_title_id = ? AND game_title_id = ? AND source = 'igdb'").bind(b).bind(g).execute(&mut *tx).await?;
    }
    for (b, g) in want.difference(&have) {
        sqlx::query("INSERT IGNORE INTO title_bundle_games (bundle_title_id, game_title_id, source) VALUES (?, ?, 'igdb')").bind(b).bind(g).execute(&mut *tx).await?;
        // A collection is a collection, whatever IGDB filed it under.
        sqlx::query("UPDATE title SET category = 'bundle' WHERE id = ? AND basegame_locked = FALSE").bind(b).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(want.len() as u64)
}

/// Takes each IGDB-linked title's category from its game again (an edition was filed as a
/// base game before editions had a category of their own). Titles the user set are left
/// alone. Returns titles changed.
pub async fn refresh_categories(pool: &MySqlPool) -> Result<u64, DatabaseError> {
    let done = sqlx::query(
        "UPDATE title t JOIN title_provider_links l ON l.title_id = t.id AND l.provider = 'igdb' AND l.deleted = FALSE AND l.provider_id REGEXP '^[0-9]+$' \
         JOIN igdb_games g ON g.provider_id = CAST(l.provider_id AS UNSIGNED) AND g.relations_read = TRUE AND g.game_type IS NOT NULL AND g.raw_response_path IS NOT NULL \
         SET t.category = g.game_type \
         WHERE t.deleted = FALSE AND t.basegame_locked = FALSE AND t.category <> g.game_type",
    )
    .execute(pool)
    .await?;
    Ok(done.rows_affected())
}

// ---------------------------------------------------------------------------------------
// Provider-only versions
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct VersionSummary {
    /// IGDB games read for the families (cached already, or fetched now).
    pub games: u64,
    pub fetched: u64,
    /// Provider-only releases (and titles) added.
    pub added: u64,
    /// DAT releases of the same name linked to their game instead.
    pub linked: u64,
    /// Platforms (app ids) whose IGDB metadata `whole_platform` wants synced.
    pub platforms_to_sync: Vec<u16>,
}

/// The official versions of a game IGDB lists, through `relations`: its parent, its ports,
/// remakes and remasters, the collections holding it, and its parent's versions too (its
/// siblings). Found collections are not followed further, nor are add-ons.
async fn family_games(pool: &MySqlPool, start: &HashSet<u32>, clients: Option<&ProviderClients>, store: &RawStore, summary: &mut VersionSummary) -> Result<HashMap<u32, (String, &'static str, Option<u16>, Vec<u16>)>, DatabaseError> {
    let mut known: HashMap<u32, (String, &'static str, Option<u16>, Vec<u16>)> = HashMap::new();
    let mut seen: HashSet<u32> = start.clone();
    let mut frontier: Vec<u32> = start.iter().copied().collect();
    for _round in 0..4 {
        if frontier.is_empty() {
            break;
        }
        // The frontier's own entries, fetched first when not cached yet.
        let mut missing: Vec<u32> = Vec::new();
        for chunk in frontier.chunks(1000) {
            let list = chunk.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
            let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT provider_id, title, game_type, first_year, platforms FROM igdb_games WHERE provider_id IN ({list}) AND deleted = FALSE AND relations_read = TRUE")))
                .fetch_all(pool)
                .await?;
            let mut found: HashSet<u32> = HashSet::new();
            for r in rows {
                let id: u32 = r.try_get("provider_id")?;
                found.insert(id);
                let ty: Option<String> = r.try_get("game_type")?;
                let ty = CATEGORIES.iter().find(|c| Some(**c) == ty.as_deref()).copied().unwrap_or("base_game");
                let platforms = r.try_get::<Option<String>, _>("platforms")?.unwrap_or_default().split(',').filter_map(|p| p.parse().ok()).collect();
                known.insert(id, (r.try_get::<Option<String>, _>("title")?.unwrap_or_default(), ty, r.try_get::<Option<u16>, _>("first_year")?, platforms));
            }
            missing.extend(chunk.iter().filter(|id| !found.contains(id)));
        }
        if let Some(igdb) = clients.and_then(|c| c.igdb.as_ref()) {
            for chunk in missing.chunks(400) {
                let games = igdb.get_games_by_id(chunk.iter().map(|i| *i as i32).collect()).await.map_err(|e| DatabaseError::Provider(format!("igdb: {e}")))?;
                let mut index = Vec::new();
                for g in &games {
                    let raw = serde_json::to_value(g)?;
                    cache::upsert_game(pool, store, Provider::Igdb, &g.id.to_string(), Some(&g.name), &raw).await?;
                    let ix = index_of(g);
                    known.insert(ix.id, (g.name.clone(), ix.game_type, ix.first_year, ix.platforms.clone()));
                    index.push(ix);
                    summary.fetched += 1;
                }
                write_index(pool, &index).await?;
            }
        }
        // The next ring: what the frontier's games name, and the versions of their parents.
        let followed: Vec<u32> = frontier.iter().copied().filter(|id| known.get(id).is_none_or(|k| !matches!(k.1, "bundle" | "dlc" | "expansion" | "standalone" | "mod"))).collect();
        let mut next: Vec<u32> = Vec::new();
        for chunk in followed.chunks(1000) {
            let list = chunk.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
            let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
                "SELECT related_id AS id FROM igdb_game_relations WHERE game_id IN ({list}) \
                 UNION SELECT game_id FROM igdb_game_relations WHERE related_id IN ({list}) AND relation = 'parent' \
                 UNION SELECT r2.related_id FROM igdb_game_relations r JOIN igdb_game_relations r2 ON r2.game_id = r.related_id AND r2.relation IN ('port','remake','remaster') WHERE r.game_id IN ({list}) AND r.relation = 'parent'"
            )))
            .fetch_all(pool)
            .await?;
            for r in rows {
                let id: u32 = r.try_get("id")?;
                if seen.insert(id) {
                    next.push(id);
                }
            }
        }
        frontier = next;
    }
    summary.games = known.len() as u64;
    Ok(known)
}

/// Adds a provider-only release (and title) for every platform an official version of a
/// catalogue game is on and the catalogue has no title of that game on.
pub async fn add_provider_versions(pool: &MySqlPool, clients: Option<&ProviderClients>, store: &RawStore, media: Option<(&reqwest::Client, &std::path::Path)>, progress: &Progress) -> Result<VersionSummary, DatabaseError> {
    let mut summary = VersionSummary::default();
    progress.force(0, None, "families: finding the official versions of the catalogue's games").await;
    let linked: Vec<u32> = sqlx::query_scalar(
        "SELECT DISTINCT CAST(l.provider_id AS UNSIGNED) FROM title_provider_links l JOIN title t ON t.id = l.title_id AND t.deleted = FALSE \
         WHERE l.provider = 'igdb' AND l.deleted = FALSE AND l.provider_id REGEXP '^[0-9]+$' AND t.category IN ('base_game','port','remake','remaster','edition','bundle')",
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|v: u64| v as u32)
    .collect();
    let start: HashSet<u32> = linked.into_iter().collect();
    let mode = fetch_versions(pool).await?;
    // The platforms whose IGDB metadata is here: their versions need no fetch.
    let synced: HashSet<u16> = super::providers::sync_state::all(pool)
        .await?
        .into_iter()
        .filter(|s| s.provider == Provider::Igdb.name() && s.entries > 0)
        .filter_map(|s| s.app_platform_id)
        .flat_map(cache::sibling_igdb_platforms)
        .collect();
    // The families of the games held on disk, fetched by id when the setting asks for it.
    let fetched_family: HashSet<u32> = if mode == "off" {
        HashSet::new()
    } else {
        let held: HashSet<u32> = sqlx::query_scalar(
            "SELECT DISTINCT CAST(l.provider_id AS UNSIGNED) FROM title_provider_links l JOIN title_releases tr ON tr.title_id = l.title_id \
             JOIN local_release_games lrg ON lrg.release_id = tr.release_id JOIN local_game lg ON lg.id = lrg.game_id AND lg.deleted = FALSE \
             WHERE l.provider = 'igdb' AND l.deleted = FALSE AND l.provider_id REGEXP '^[0-9]+$'",
        )
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|v: u64| v as u32)
        .collect();
        progress.force(0, None, &format!("families: fetching the versions of {} held game(s)", held.len())).await;
        family_games(pool, &held, clients, store, &mut summary).await?.into_keys().collect()
    };
    // Every family, from what is cached now.
    let games = family_games(pool, &start, None, store, &mut summary).await?;
    let mut to_sync: HashSet<u16> = HashSet::new();
    // Which platforms each game already has a title on (any of IGDB's names for the machine).
    let mut on: HashMap<u32, HashSet<u16>> = HashMap::new();
    for r in sqlx::query(
        "SELECT DISTINCT CAST(l.provider_id AS UNSIGNED) AS game, r.app_platform_id FROM title_provider_links l JOIN title t ON t.id = l.title_id AND t.deleted = FALSE \
         JOIN title_releases tr ON tr.title_id = t.id JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE AND r.material IS NULL \
         WHERE l.provider = 'igdb' AND l.deleted = FALSE AND l.provider_id REGEXP '^[0-9]+$'",
    )
    .fetch_all(pool)
    .await?
    {
        let game: u64 = r.try_get("game")?;
        let p: u16 = r.try_get("app_platform_id")?;
        on.entry(game as u32).or_default().extend(cache::sibling_igdb_platforms(p));
    }
    let source_id = super::import::ensure_source(pool, PROVIDER_ONLY_SOURCE).await?;
    let mut added_titles: Vec<(u64, u32)> = Vec::new();
    let total = games.len() as u64;
    for (i, (game, (name, ty, year, platforms))) in games.iter().enumerate() {
        if i % 200 == 0 {
            progress.report(i as u64, Some(total), &format!("families: official versions, game {i} of {total} ({} added)", summary.added)).await;
        }
        if name.trim().is_empty() || matches!(*ty, "dlc" | "expansion" | "standalone" | "mod") {
            continue;
        }
        let have = on.get(game).cloned().unwrap_or_default();
        for p in platforms {
            let Some(platform) = Platform::from_igdb_id(*p).filter(|p| *p != Platform::Unknown) else { continue };
            if have.contains(p) || cache::sibling_igdb_platforms(platform.igdb_id()).iter().any(|s| have.contains(s)) {
                continue;
            }
            let on_synced = synced.contains(p) || synced.contains(&platform.igdb_id());
            if !on_synced {
                if !fetched_family.contains(game) {
                    continue;
                }
                if mode == "whole_platform" {
                    to_sync.insert(platform.igdb_id());
                }
            }
            match add_version(pool, source_id, platform, *game, name, *year).await? {
                Added::New(title) => {
                    summary.added += 1;
                    added_titles.push((title, *game));
                }
                Added::Linked => summary.linked += 1,
                Added::Nothing => {}
            }
            on.entry(*game).or_default().insert(*p);
        }
    }
    summary.platforms_to_sync = to_sync.into_iter().collect();
    summary.platforms_to_sync.sort_unstable();
    // Their metadata and covers, like a platform sync gives the titles it links.
    if let (Some(c), false) = (clients, added_titles.is_empty()) {
        if c.igdb.is_some() {
            let ids: Vec<i32> = added_titles.iter().map(|(_, g)| *g as i32).collect::<HashSet<_>>().into_iter().collect();
            let mut by_id: HashMap<i32, IgdbGame> = HashMap::new();
            for id in &ids {
                if let Some(g) = cache::load_cached_igdb_game(pool, *id).await? {
                    by_id.insert(*id, g);
                }
            }
            let page: Vec<IgdbGame> = by_id.values().cloned().collect();
            let mut names = super::providers::metadata::IgdbNames::new();
            if let Err(e) = names.resolve(c, &page).await {
                log::warn!("families: IGDB names for the added versions: {e}");
            }
            for (title, game) in &added_titles {
                if let Some(g) = by_id.get(&(*game as i32)) {
                    super::providers::metadata::apply_igdb_game(pool, &names, *title, g).await?;
                }
            }
            if let Some((http, media_dir)) = media {
                let covers = super::providers::media::igdb_cover_urls(c, &page).await.unwrap_or_default();
                for (title, game) in &added_titles {
                    let Some(url) = by_id.get(&(*game as i32)).and_then(|g| g.cover).and_then(|c| covers.get(&c)) else { continue };
                    let targets = super::providers::media::release_targets(pool, *title, Provider::Igdb, &game.to_string()).await?;
                    if let Err(e) = super::providers::media::attach_media(http, pool, media_dir, Provider::Igdb, Some(&game.to_string()), url, "box", Some("front"), &targets).await {
                        log::warn!("families: cover for title {title}: {e}");
                    }
                }
            }
        }
    } else {
        // No IGDB client: the category and parent from the index alone.
        for (title, game) in &added_titles {
            if let Some((_, ty, _, _)) = games.get(game) {
                sqlx::query("UPDATE title SET category = ? WHERE id = ?").bind(*ty).bind(title).execute(pool).await?;
            }
            let parent: Option<u32> = sqlx::query_scalar("SELECT related_id FROM igdb_game_relations WHERE game_id = ? AND relation = 'parent' LIMIT 1").bind(game).fetch_optional(pool).await?;
            sqlx::query("UPDATE title SET basegame_igdb_id = ? WHERE id = ?").bind(parent.unwrap_or(0)).bind(title).execute(pool).await?;
        }
    }
    Ok(summary)
}

enum Added {
    New(u64),
    Linked,
    Nothing,
}

async fn add_version(pool: &MySqlPool, source_id: u8, platform: Platform, game: u32, name: &str, year: Option<u16>) -> Result<Added, DatabaseError> {
    let platform_id = platform.igdb_id();
    let name = name.trim();
    let name_hash = super::import::sha1_hex(name);
    let norm_name = super::matching::util::normalize_title(name).to_lowercase();
    let norm_hash = super::import::sha1_hex(&norm_name);
    // A DAT release of that very name on that platform is the version: link its title.
    if let Some(r) = sqlx::query("SELECT r.id, r.source_id, tr.title_id FROM `release` r LEFT JOIN title_releases tr ON tr.release_id = r.id WHERE r.app_platform_id = ? AND r.name_hash = ? LIMIT 1")
        .bind(platform_id)
        .bind(&name_hash)
        .fetch_optional(pool)
        .await?
    {
        let title: Option<u64> = r.try_get("title_id")?;
        if r.try_get::<u8, _>("source_id")? != source_id {
            if let Some(t) = title {
                cache::link_title(pool, t, Provider::Igdb, &game.to_string(), Confidence::Name).await?;
                return Ok(Added::Linked);
            }
        }
        return Ok(Added::Nothing);
    }
    let mut tx = pool.begin().await?;
    let release_id = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, year, source_id, deleted) VALUES (?, ?, ?, ?, ?, ?, ?, FALSE)")
        .bind(platform_id)
        .bind(name)
        .bind(&name_hash)
        .bind(&norm_name)
        .bind(&norm_hash)
        .bind(year)
        .bind(source_id)
        .execute(&mut *tx)
        .await?
        .last_insert_id();
    sqlx::query("INSERT IGNORE INTO release_sources (release_id, source_id) VALUES (?, ?)").bind(release_id).bind(source_id).execute(&mut *tx).await?;
    let title_id = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, ?, ?)").bind(name).bind(&norm_name).bind(&norm_hash).execute(&mut *tx).await?.last_insert_id();
    sqlx::query("INSERT IGNORE INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(title_id).bind(release_id).execute(&mut *tx).await?;
    tx.commit().await?;
    cache::link_title(pool, title_id, Provider::Igdb, &game.to_string(), Confidence::Exact).await?;
    Ok(Added::New(title_id))
}

// ---------------------------------------------------------------------------------------
// The definitive version
// ---------------------------------------------------------------------------------------

/// One member of a family, as the ranking sees it.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Member {
    pub title_id: u64,
    pub name: String,
    pub category: String,
    /// The version: its IGDB game, else the title itself.
    pub version: String,
    /// The year the version came out (its IGDB first release, else its earliest release).
    pub year: Option<u16>,
    /// The title's platforms (app ids), best by the priority first once ranked.
    pub platforms: Vec<u16>,
    /// The user's weight on the family page.
    pub weight: Option<f64>,
    /// A collection: the family's title it holds (shown as "collection (game)").
    pub holds: Option<String>,
    pub is_head: bool,
}

/// Where a platform ranks on the Definitive tab: its place in `priority`, with PC at the
/// [`PC_DEFINITIVE`] entry when the list has one. Unlisted platforms after, in id order.
pub fn platform_rank(priority: &[u16], platform: u16) -> (usize, u16) {
    let pc = Platform::PcWindows as u16;
    let at = |id: u16| priority.iter().position(|p| *p == id);
    let pos = if platform == pc { at(PC_DEFINITIVE).or_else(|| at(pc)) } else { at(platform) };
    (pos.unwrap_or(usize::MAX), platform)
}

/// The family in definitive order (`members` in any order). The user's weights first;
/// then the newest version, a version's year being its earliest; within one version by
/// platform. A PC release of an older version is ranked with the newest version unless it
/// is `pc_max_age` or more years older (0: never).
pub fn rank(mut members: Vec<Member>, priority: &[u16], pc_max_age: u16) -> Vec<Member> {
    for m in &mut members {
        m.platforms.sort_by_key(|p| platform_rank(priority, *p));
    }
    let mut version_year: HashMap<String, u16> = HashMap::new();
    for m in &members {
        if let Some(y) = m.year {
            let e = version_year.entry(m.version.clone()).or_insert(y);
            *e = (*e).min(y);
        }
    }
    let newest = version_year.values().copied().max();
    let pc = Platform::PcWindows as u16;
    let key = |m: &Member| {
        let year = version_year.get(&m.version).copied();
        let lifted = m.platforms.first() == Some(&pc) && pc_max_age > 0 && matches!((newest, year), (Some(n), Some(y)) if n.saturating_sub(y) < pc_max_age);
        let effective = if lifted { newest } else { year };
        let platform = m.platforms.first().map(|p| platform_rank(priority, *p)).unwrap_or((usize::MAX, u16::MAX));
        // Newest first (a version with no year after the dated ones); within a year, the
        // platform priority, so a lifted PC version sits where "PC (Definitive tab)" is.
        (std::cmp::Reverse(effective.unwrap_or(0)), platform, m.version.clone(), m.title_id)
    };
    let (mut weighted, mut rest): (Vec<Member>, Vec<Member>) = members.into_iter().partition(|m| m.weight.is_some());
    weighted.sort_by(|a, b| a.weight.partial_cmp(&b.weight).unwrap_or(std::cmp::Ordering::Equal).then(a.title_id.cmp(&b.title_id)));
    rest.sort_by_key(|m| key(m));
    weighted.extend(rest);
    weighted
}

/// `definitive_pc_max_age` as stored, else the default.
pub async fn pc_max_age(pool: &MySqlPool) -> Result<u16, DatabaseError> {
    Ok(super::settings::get::<u16>(pool, KEY_PC_MAX_AGE).await?.unwrap_or(DEFAULT_PC_MAX_AGE).min(100))
}

/// The members of each of `heads`' families: its titles of a member category with a game
/// release of the retail kind, and the collections holding one of them.
pub async fn members_of(pool: &MySqlPool, heads: &[u64]) -> Result<HashMap<u64, Vec<Member>>, DatabaseError> {
    let mut out: HashMap<u64, Vec<Member>> = HashMap::new();
    if heads.is_empty() {
        return Ok(out);
    }
    let unl = super::release_buckets::unlicensed_joins_retail(pool).await?;
    let retail = format!("{} = 'retail'", super::release_buckets::bucket_sql("rx", unl));
    let member_cols = format!(
        "t.id, t.name, t.category, t.family_root_id, \
         (SELECT MIN(l.provider_id) FROM title_provider_links l WHERE l.title_id = t.id AND l.provider = 'igdb' AND l.deleted = FALSE) AS igdb, \
         (SELECT CAST(GROUP_CONCAT(DISTINCT rx.app_platform_id) AS CHAR) FROM title_releases trx JOIN `release` rx ON rx.id = trx.release_id AND rx.deleted = FALSE AND rx.material IS NULL WHERE trx.title_id = t.id AND {retail}) AS platforms, \
         (SELECT CAST(MIN(COALESCE(YEAR(rx.released_on), rx.year)) AS SIGNED) FROM title_releases trx JOIN `release` rx ON rx.id = trx.release_id AND rx.deleted = FALSE AND rx.material IS NULL WHERE trx.title_id = t.id AND {retail}) AS year"
    );
    for chunk in heads.chunks(500) {
        let list = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT {member_cols}, COALESCE(t.family_root_id, t.id) AS head, NULL AS holds FROM title t \
               WHERE t.deleted = FALSE AND (t.family_root_id IN ({list}) OR (t.family_root_id IS NULL AND t.id IN ({list}))) AND t.category IN ({MEMBER_CATEGORIES}) \
             UNION ALL \
             SELECT {member_cols}, COALESCE(g.family_root_id, g.id) AS head, g.name AS holds FROM title_bundle_games bg JOIN title g ON g.id = bg.game_title_id AND g.deleted = FALSE \
               JOIN title t ON t.id = bg.bundle_title_id AND t.deleted = FALSE \
               WHERE COALESCE(g.family_root_id, g.id) IN ({list})"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(pool).await?;
        let ids: Vec<String> = rows.iter().filter_map(|r| r.try_get::<Option<String>, _>("igdb").ok().flatten()).filter(|i| i.chars().all(|c| c.is_ascii_digit())).collect();
        let mut first_year: HashMap<String, u16> = HashMap::new();
        if !ids.is_empty() {
            for r in sqlx::query(sqlx::AssertSqlSafe(format!("SELECT provider_id, first_year FROM igdb_games WHERE provider_id IN ({}) AND first_year IS NOT NULL", ids.join(",")))).fetch_all(pool).await? {
                first_year.insert(r.try_get::<u32, _>("provider_id")?.to_string(), r.try_get("first_year")?);
            }
        }
        let weights: HashMap<(u64, u64), f64> = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT root_title_id, title_id, sort_weight FROM title_family_order WHERE root_title_id IN ({list})")))
            .fetch_all(pool)
            .await?
            .iter()
            .filter_map(|r| Some(((r.try_get("root_title_id").ok()?, r.try_get("title_id").ok()?), r.try_get("sort_weight").ok()?)))
            .collect();
        for r in rows {
            let platforms: Vec<u16> = r.try_get::<Option<String>, _>("platforms")?.unwrap_or_default().split(',').filter_map(|p| p.parse().ok()).collect();
            if platforms.is_empty() {
                continue;
            }
            let id: u64 = r.try_get("id")?;
            let head: u64 = r.try_get::<i64, _>("head").map(|h| h as u64).or_else(|_| r.try_get::<u64, _>("head"))?;
            let igdb: Option<String> = r.try_get("igdb")?;
            let year: Option<i64> = r.try_get("year")?;
            let year = igdb.as_ref().and_then(|g| first_year.get(g)).copied().or(year.and_then(|y| u16::try_from(y).ok()));
            let list = out.entry(head).or_default();
            if list.iter().any(|m| m.title_id == id) {
                continue;
            }
            list.push(Member {
                title_id: id,
                name: r.try_get("name")?,
                category: r.try_get("category")?,
                version: igdb.map(|g| format!("igdb:{g}")).unwrap_or_else(|| format!("title:{id}")),
                year,
                platforms,
                weight: weights.get(&(head, id)).copied(),
                holds: r.try_get("holds")?,
                is_head: id == head,
            });
        }
    }
    Ok(out)
}

/// Works out the definitive version of `heads`' families (None: every family) and stores
/// it in `title_families`. Returns the families stored.
pub async fn refresh(pool: &MySqlPool, heads: Option<&[u64]>) -> Result<u64, DatabaseError> {
    let full = heads.is_none();
    let heads: Vec<u64> = match heads {
        Some(h) => h.to_vec(),
        None => sqlx::query_scalar("SELECT DISTINCT family_root_id FROM title WHERE family_root_id IS NOT NULL AND deleted = FALSE UNION SELECT DISTINCT COALESCE(g.family_root_id, g.id) FROM title_bundle_games bg JOIN title g ON g.id = bg.game_title_id")
            .fetch_all(pool)
            .await?,
    };
    let priority = super::settings::platform_priority(pool).await?;
    let max_age = pc_max_age(pool).await?;
    if full {
        sqlx::query("DELETE FROM title_families").execute(pool).await?;
    }
    let mut stored = 0u64;
    for chunk in heads.chunks(500) {
        let members = members_of(pool, chunk).await?;
        let mut tx = pool.begin().await?;
        let list = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM title_families WHERE root_title_id IN ({list})"))).execute(&mut *tx).await?;
        for head in chunk {
            let Some(m) = members.get(head) else { continue };
            if m.is_empty() {
                continue;
            }
            let ranked = rank(m.clone(), &priority, max_age);
            sqlx::query("INSERT INTO title_families (root_title_id, definitive_title_id, members) VALUES (?, ?, ?)").bind(head).bind(ranked[0].title_id).bind(ranked.len().min(u16::MAX as usize) as u16).execute(&mut *tx).await?;
            stored += 1;
        }
        tx.commit().await?;
    }
    Ok(stored)
}

/// The head of `title_id`'s family.
pub async fn head_of(pool: &MySqlPool, title_id: u64) -> Result<u64, DatabaseError> {
    let head: Option<Option<u64>> = sqlx::query_scalar("SELECT family_root_id FROM title WHERE id = ?").bind(title_id).fetch_optional(pool).await?;
    Ok(head.flatten().unwrap_or(title_id))
}

/// The heads of the families `titles` are in, and of those of the games they hold.
pub async fn heads_of(pool: &MySqlPool, titles: &[u64]) -> Result<Vec<u64>, DatabaseError> {
    if titles.is_empty() {
        return Ok(Vec::new());
    }
    let list = titles.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let mut heads: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT COALESCE(family_root_id, id) FROM title WHERE id IN ({list}) UNION SELECT COALESCE(g.family_root_id, g.id) FROM title_bundle_games bg JOIN title g ON g.id = bg.game_title_id WHERE bg.bundle_title_id IN ({list})"
    )))
    .fetch_all(pool)
    .await?;
    heads.sort_unstable();
    heads.dedup();
    Ok(heads)
}

// ---------------------------------------------------------------------------------------
// The user's edits
// ---------------------------------------------------------------------------------------

/// Sets `title_id`'s base game(s) and category by hand. A collection (`bundle`) takes the
/// games it holds; anything else one base game, or none (it is a base game itself). Locked
/// from IGDB from then on.
pub async fn set_basegame(pool: &MySqlPool, title_id: u64, category: &str, bases: &[u64]) -> Result<(), DatabaseError> {
    if !CATEGORIES.contains(&category) {
        return Err(DatabaseError::Config(format!("unknown category {category}")));
    }
    if bases.contains(&title_id) {
        return Err(DatabaseError::Config("a title can't be its own base game".to_string()));
    }
    let bundle = category == "bundle";
    if !bundle && bases.len() > 1 {
        return Err(DatabaseError::Config("only a collection holds more than one game".to_string()));
    }
    let before = heads_of(pool, &[title_id]).await?;
    if let (false, Some(base)) = (bundle, bases.first()) {
        // Never a loop: the base game must not be under this title already.
        let mut cur = Some(*base);
        let mut steps = 0;
        while let Some(c) = cur {
            if c == title_id {
                return Err(DatabaseError::Config("that title is already a version of this one".to_string()));
            }
            steps += 1;
            if steps > 64 {
                break;
            }
            cur = sqlx::query_scalar::<_, Option<u64>>("SELECT basegame_title_id FROM title WHERE id = ? AND deleted = FALSE").bind(c).fetch_optional(pool).await?.flatten();
        }
    }
    let mut tx = pool.begin().await?;
    let basegame = if bundle { None } else { bases.first().copied() };
    sqlx::query("UPDATE title SET category = ?, basegame_title_id = ?, basegame_locked = TRUE WHERE id = ?").bind(category).bind(basegame).bind(title_id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM title_bundle_games WHERE bundle_title_id = ?").bind(title_id).execute(&mut *tx).await?;
    if bundle {
        for b in bases {
            sqlx::query("INSERT IGNORE INTO title_bundle_games (bundle_title_id, game_title_id, source) VALUES (?, ?, 'manual')").bind(title_id).bind(b).execute(&mut *tx).await?;
        }
    }
    tx.commit().await?;
    after_edit(pool, &[title_id], &before).await
}

/// Gives `title_id`'s base game and category back to IGDB: unlocked, and taken from its
/// IGDB game at once.
pub async fn reset_basegame(pool: &MySqlPool, title_id: u64) -> Result<(), DatabaseError> {
    let before = heads_of(pool, &[title_id]).await?;
    let game: Option<String> = sqlx::query_scalar("SELECT MIN(provider_id) FROM title_provider_links WHERE title_id = ? AND provider = 'igdb' AND deleted = FALSE").bind(title_id).fetch_one(pool).await?;
    let game: Option<u32> = game.and_then(|g| g.parse().ok());
    let (category, parent): (String, Option<u32>) = match game {
        Some(g) => (
            sqlx::query_scalar::<_, Option<String>>("SELECT game_type FROM igdb_games WHERE provider_id = ?").bind(g).fetch_optional(pool).await?.flatten().unwrap_or_else(|| "base_game".to_string()),
            sqlx::query_scalar("SELECT related_id FROM igdb_game_relations WHERE game_id = ? AND relation = 'parent' LIMIT 1").bind(g).fetch_optional(pool).await?,
        ),
        None => ("base_game".to_string(), None),
    };
    // The parent's title: one on the same platform, else the lowest id (the pass refines it).
    let basegame: Option<u64> = match parent {
        Some(p) => sqlx::query_scalar(
            "SELECT l.title_id FROM title_provider_links l JOIN title b ON b.id = l.title_id AND b.deleted = FALSE WHERE l.provider = 'igdb' AND l.deleted = FALSE AND l.provider_id = ? AND l.title_id <> ? \
             ORDER BY EXISTS (SELECT 1 FROM title_releases ta JOIN `release` ra ON ra.id = ta.release_id JOIN `release` rb ON rb.app_platform_id = ra.app_platform_id JOIN title_releases tb ON tb.release_id = rb.id AND tb.title_id = b.id WHERE ta.title_id = ?) DESC, l.title_id LIMIT 1",
        )
        .bind(p.to_string())
        .bind(title_id)
        .bind(title_id)
        .fetch_optional(pool)
        .await?,
        None => None,
    };
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE title SET category = ?, basegame_title_id = ?, basegame_igdb_id = ?, basegame_locked = FALSE WHERE id = ?").bind(&category).bind(basegame).bind(parent.unwrap_or(0)).bind(title_id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM title_bundle_games WHERE bundle_title_id = ? AND source = 'manual'").bind(title_id).execute(&mut *tx).await?;
    tx.commit().await?;
    link_bundles(pool).await?;
    after_edit(pool, &[title_id], &before).await
}

/// After an edit: the families rebuilt and the definitive versions of the ones touched
/// (before and after) worked out again.
pub async fn after_edit(pool: &MySqlPool, titles: &[u64], before: &[u64]) -> Result<(), DatabaseError> {
    recompute_roots(pool).await?;
    let mut heads = heads_of(pool, titles).await?;
    heads.extend_from_slice(before);
    heads.sort_unstable();
    heads.dedup();
    refresh(pool, Some(&heads)).await?;
    Ok(())
}

/// The family's members as they are shown, with the user's weights, for a move.
pub async fn move_member(pool: &MySqlPool, head: u64, title_id: u64, after: Option<u64>) -> Result<(), DatabaseError> {
    let members = members_of(pool, &[head]).await?.remove(&head).unwrap_or_default();
    let priority = super::settings::platform_priority(pool).await?;
    let ranked = rank(members, &priority, pc_max_age(pool).await?);
    let current: Vec<(u64, Option<f64>)> = ranked.iter().map(|m| (m.title_id, m.weight)).collect();
    let weights = super::release_order::plan_move(&current, title_id, after).map_err(|e| DatabaseError::Config(e.replace("release", "title")))?;
    let mut tx = pool.begin().await?;
    for (id, w) in &weights {
        sqlx::query("INSERT INTO title_family_order (root_title_id, title_id, sort_weight) VALUES (?, ?, ?) ON DUPLICATE KEY UPDATE sort_weight = VALUES(sort_weight)").bind(head).bind(id).bind(w).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    refresh(pool, Some(&[head])).await?;
    Ok(())
}

/// Back to the default order for a family.
pub async fn reset_order(pool: &MySqlPool, head: u64) -> Result<(), DatabaseError> {
    sqlx::query("DELETE FROM title_family_order WHERE root_title_id = ?").bind(head).execute(pool).await?;
    refresh(pool, Some(&[head])).await?;
    Ok(())
}

// ---------------------------------------------------------------------------------------
// The pass
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct FamilySummary {
    pub games_read: u64,
    pub categories: u64,
    pub versions: VersionSummary,
    pub bundles: u64,
    pub roots_changed: u64,
    pub families: u64,
}

/// The first half of the family pass, before the base-game links: the index, the
/// categories, and the provider-only versions (they need their base game linked too).
pub async fn run_versions(pool: &MySqlPool, clients: Option<&ProviderClients>, store: &RawStore, media: Option<(&reqwest::Client, &std::path::Path)>, progress: &Progress) -> Result<FamilySummary, DatabaseError> {
    // Fetching versions goes to IGDB; the index and the categories are the catalogue's own.
    let slice = |base: u64, span: u64| progress.set_inner_frame(Some(super::jobs::Frame { base, span, whole: 100 }));
    slice(0, 30);
    let mut s = FamilySummary { games_read: backfill_index(pool, progress).await?, ..Default::default() };
    backfill_release_dates(pool, progress).await?;
    slice(30, 5);
    progress.force(0, None, &format!("families: sorting games into ports, remakes and add-ons ({} IGDB game(s) read)", s.games_read)).await;
    s.categories = refresh_categories(pool).await?;
    slice(35, 65);
    s.versions = add_provider_versions(pool, clients, store, media, progress).await?;
    progress.set_inner_frame(None);
    Ok(s)
}

/// The second half, after the base-game links: collections, families and their
/// definitive versions.
pub async fn run_families(pool: &MySqlPool, s: &mut FamilySummary, progress: &Progress) -> Result<(), DatabaseError> {
    progress.force(0, Some(3), "families: linking collections to the games they hold").await;
    s.bundles = link_bundles(pool).await?;
    progress.force(1, Some(3), &format!("families: finding each family's root game ({} collection link(s))", s.bundles)).await;
    s.roots_changed = recompute_roots(pool).await?;
    progress.force(2, Some(3), &format!("families: grouping families and picking their definitive versions ({} root(s) changed)", s.roots_changed)).await;
    s.families = refresh(pool, None).await?;
    progress.force(3, Some(3), &format!("families: {} family(ies)", s.families)).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(id: u64, version: &str, year: Option<u16>, platform: u16) -> Member {
        Member { title_id: id, name: format!("hoge {id}"), category: "port".into(), version: version.into(), year, platforms: vec![platform], ..Default::default() }
    }

    const PS5: u16 = Platform::PlayStation5 as u16;
    const SWITCH: u16 = Platform::NintendoSwitch as u16;
    const SWITCH2: u16 = Platform::NintendoSwitch2 as u16;
    const PC: u16 = Platform::PcWindows as u16;
    const SNES: u16 = Platform::Snes as u16;
    const GBA: u16 = Platform::GameBoyAdvance as u16;

    fn ids(v: &[Member]) -> Vec<u64> {
        v.iter().map(|m| m.title_id).collect()
    }

    #[test]
    fn newest_version_first_then_platform() {
        let prio = vec![PS5, PC_DEFINITIVE, SWITCH2, SWITCH, PC];
        let ranked = rank(vec![m(1, "base", Some(1994), SNES), m(2, "gba", Some(2006), GBA), m(3, "remaster", Some(2023), SWITCH), m(4, "remaster", Some(2023), PS5), m(5, "remaster", Some(2023), PC)], &prio, 10);
        assert_eq!(ids(&ranked), vec![4, 5, 3, 2, 1]);
    }

    #[test]
    fn a_version_released_late_on_one_platform_keeps_its_year() {
        // hoge remaster: Switch and PC in 2025, Switch 2 in 2026 -- one version, by platform.
        let prio = vec![SWITCH2, PC_DEFINITIVE, SWITCH];
        let ranked = rank(vec![m(1, "r", Some(2025), SWITCH), m(2, "r", Some(2026), SWITCH2), m(3, "r", Some(2025), PC), m(4, "base", Some(1995), SNES)], &prio, 10);
        assert_eq!(ids(&ranked), vec![2, 3, 1, 4]);
    }

    #[test]
    fn an_older_pc_version_moves_up_unless_ten_years_older() {
        let prio = vec![PS5, PC_DEFINITIVE];
        let near = rank(vec![m(1, "pc port", Some(2018), PC), m(2, "remake", Some(2024), PS5), m(3, "x", Some(2020), SWITCH)], &prio, 10);
        assert_eq!(ids(&near), vec![2, 1, 3], "six years older: next to the newest");
        let far = rank(vec![m(1, "pc port", Some(2008), PC), m(2, "remake", Some(2024), PS5), m(3, "x", Some(2020), SWITCH)], &prio, 10);
        assert_eq!(ids(&far), vec![2, 3, 1], "sixteen years older: in its own place");
        let off = rank(vec![m(1, "pc port", Some(2018), PC), m(2, "remake", Some(2024), PS5), m(3, "x", Some(2020), SWITCH)], &prio, 0);
        assert_eq!(ids(&off), vec![2, 3, 1], "0 turns the lift off");
    }

    #[test]
    fn pc_uses_its_definitive_entry_when_listed() {
        assert!(platform_rank(&[PS5, PC_DEFINITIVE, SWITCH, PC], PC) < platform_rank(&[PS5, PC_DEFINITIVE, SWITCH, PC], SWITCH));
        assert!(platform_rank(&[PS5, SWITCH, PC], PC) > platform_rank(&[PS5, SWITCH, PC], SWITCH), "without it, the normal PC entry");
    }

    #[test]
    fn the_users_order_comes_first() {
        let mut gba = m(2, "gba", Some(2006), GBA);
        gba.weight = Some(1.0);
        let ranked = rank(vec![m(1, "remaster", Some(2023), PS5), gba, m(3, "base", Some(1994), SNES)], &[PS5], 10);
        assert_eq!(ids(&ranked), vec![2, 1, 3], "hoge (GBA) has more content than hoge remaster: dragged to the top");
    }

    #[test]
    fn families_join_through_every_level_and_pick_the_base_game() {
        // 3 remasters 2, which remakes 1; 4 is 1 on another platform (same IGDB game).
        let edges = vec![(3, 2), (2, 1), (1, 4)];
        let info: HashMap<u64, (bool, bool, Option<u16>)> = [(1, (false, true, Some(1994))), (2, (true, false, Some(2010))), (3, (true, false, Some(2020))), (4, (false, true, Some(1990)))].into_iter().collect();
        let h = heads(&edges, &info);
        assert!([1, 2, 3, 4].iter().all(|t| h[t] == 4), "one family, headed by the earliest base game: {h:?}");
    }
}
