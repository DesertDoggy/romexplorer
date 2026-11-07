//! One release per platform + name, whichever DATs list it.
//!
//! Releases used to be keyed by DAT source as well (`uq_release_identity (source_id,
//! app_platform_id, name_hash)`), so one disc listed by redump (cue/bin), mame-redump (CHD)
//! and the RetroAchievements hash list was three releases -- three rows in every list,
//! three sets of links, local games split between them. [`merge_duplicate_releases`] folds
//! each platform + name group into its lowest id once, re-points everything that refers to
//! the others, and switches the unique key to `(app_platform_id, name_hash)`. Which sources
//! list a release is kept in `release_sources`; each file keeps its source in
//! `release_contents.source_id`, so a release's dumps (cue/bin set, CHD, ...) stay apart.
//!
//! Runs from `connection::migrate` after the SQL migrations. Idempotent and resumable: it
//! only acts while the old three-column key exists, the merge commits as one transaction,
//! and the key change comes last.

use sqlx::mysql::MySqlPool;
use sqlx::{Executor, Row};

use super::error::DatabaseError;

#[derive(Debug, Clone, Default)]
pub struct MergeSummary {
    pub merged_rows: u64,
    pub groups: u64,
}

/// Tables with a `release_id`, and their other columns (copied as they are onto the
/// surviving release; a row the survivor already has is kept as the survivor's).
const JUNCTIONS: [(&str, &str); 14] = [
    ("release_sources", "source_id"),
    ("release_contents", "content_id, file_name, source_id, variant"),
    ("title_releases", "title_id, sort_weight, sort_set_at"),
    ("local_release_games", "game_id"),
    ("release_media", "media_id"),
    ("release_provider_serials", "provider, serial"),
    ("release_ratings", "board, rating"),
    ("collection_set_releases", "set_id, position, added_at"),
    ("release_content_sources", "content_id, source_id, dat_key"),
    ("release_contents_superseded", "content_id, file_name, source_id, variant, dat_name, dat_version, superseded_at"),
    ("remote_release_games", "source_id, game_id"),
    ("remote_release_media", "source_id, media_id"),
    ("release_display", "field, value, source, noted_at"),
    ("local_game_order", "game_id, position"),
];

/// A connection that holds the `release_merge` work table to itself. Every fold fills that
/// one table and then reads it back, so two at once (an import's lane and a fold job) would
/// fold each other's pairs. The lock is MySQL's, so it covers the worker and the server
/// alike, and the connection is closed when dropped, which releases it however the fold
/// ends.
async fn merge_conn(pool: &MySqlPool) -> Result<sqlx::pool::PoolConnection<sqlx::MySql>, DatabaseError> {
    let mut conn = pool.acquire().await?;
    conn.close_on_drop();
    let got: Option<i64> = sqlx::query_scalar("SELECT GET_LOCK('romexplorer.release_merge', 3600)").fetch_one(&mut *conn).await?;
    if got != Some(1) {
        return Err(DatabaseError::Config("another release merge has held the work table for an hour".into()));
    }
    Ok(conn)
}

async fn old_key_present(pool: &MySqlPool) -> Result<bool, DatabaseError> {
    Ok(sqlx::query("SELECT 1 FROM information_schema.statistics WHERE table_schema = DATABASE() AND table_name = 'release' AND index_name = 'uq_release_identity' AND column_name = 'source_id' LIMIT 1")
        .fetch_optional(pool)
        .await?
        .is_some())
}

/// Folds every `old_id` of the connection's `release_merge` table into its `new_id`: the
/// survivor takes what it lacks, the rows of every table that refers to a release move to
/// it (its own kept where both have one), and the old releases are deleted. One transaction.
async fn fold_mapped(conn: &mut sqlx::MySqlConnection) -> Result<(), DatabaseError> {
    let mut tx = sqlx::Connection::begin(&mut *conn).await?;
    // The survivor takes what it lacks from the others; it stays live if any was live, and
    // is a beta/prototype/demo/... if any source says so.
    sqlx::query(
        "UPDATE `release` s JOIN (SELECT m.new_id, MAX(o.region) AS region, MAX(o.languages) AS languages, MAX(o.revision) AS revision, MAX(o.year) AS year, \
           MAX(o.released_on) AS released_on, MAX(o.serial) AS serial, MAX(o.category) AS category, MAX(o.disc_count) AS disc_count, MAX(o.min_players) AS min_players, \
           MAX(o.max_players) AS max_players, MAX(o.is_beta) AS is_beta, MAX(o.is_prototype) AS is_prototype, MAX(o.is_demo) AS is_demo, MAX(o.is_unlicensed) AS is_unlicensed, MAX(o.is_pirate) AS is_pirate, \
           MAX(o.community_patched) AS community_patched, MAX(o.patch_type) AS patch_type, MAX(o.is_special_edition) AS is_special_edition, MAX(o.is_budget) AS is_budget, MAX(o.is_aftermarket) AS is_aftermarket, MAX(o.is_rerelease) AS is_rerelease, MAX(o.revision_sort) AS revision_sort, MIN(o.deleted) AS deleted \
           FROM release_merge m JOIN `release` o ON o.id = m.old_id GROUP BY m.new_id) a ON a.new_id = s.id \
         SET s.region = COALESCE(s.region, a.region), s.languages = COALESCE(s.languages, a.languages), s.revision = COALESCE(s.revision, a.revision), \
           s.year = COALESCE(s.year, a.year), s.released_on = COALESCE(s.released_on, a.released_on), s.serial = COALESCE(s.serial, a.serial), \
           s.category = COALESCE(s.category, a.category), s.disc_count = GREATEST(s.disc_count, a.disc_count), s.min_players = COALESCE(s.min_players, a.min_players), \
           s.max_players = COALESCE(s.max_players, a.max_players), s.is_beta = (s.is_beta OR a.is_beta), s.is_prototype = (s.is_prototype OR a.is_prototype), \
           s.is_demo = (s.is_demo OR a.is_demo), s.is_unlicensed = (s.is_unlicensed OR a.is_unlicensed), s.is_pirate = (s.is_pirate OR a.is_pirate), s.community_patched = (s.community_patched OR a.community_patched), \
           s.patch_type = COALESCE(s.patch_type, a.patch_type), s.is_special_edition = (s.is_special_edition OR a.is_special_edition), s.is_budget = (s.is_budget OR a.is_budget), \n           s.is_aftermarket = (s.is_aftermarket OR a.is_aftermarket), s.is_rerelease = (s.is_rerelease OR a.is_rerelease), s.revision_sort = GREATEST(s.revision_sort, a.revision_sort), s.deleted = (s.deleted AND a.deleted)",
    )
    .execute(&mut *tx)
    .await?;
    for (table, columns) in JUNCTIONS {
        let sql = format!("INSERT IGNORE INTO {table} (release_id, {columns}) SELECT m.new_id, {columns} FROM {table} t JOIN release_merge m ON m.old_id = t.release_id");
        sqlx::query(sqlx::AssertSqlSafe(sql)).execute(&mut *tx).await?;
        let sql = format!("DELETE t FROM {table} t JOIN release_merge m ON m.old_id = t.release_id");
        sqlx::query(sqlx::AssertSqlSafe(sql)).execute(&mut *tx).await?;
    }
    // A set's release entries follow too (a set holding both keeps one).
    sqlx::query("UPDATE IGNORE collection_set_entries e JOIN release_merge m ON m.old_id = e.release_id SET e.release_id = m.new_id").execute(&mut *tx).await?;
    sqlx::query("DELETE e FROM collection_set_entries e JOIN release_merge m ON m.old_id = e.release_id").execute(&mut *tx).await?;
    // Provider links: the stronger one wins (manual > exact > hash > name > fuzzy), a live
    // link beats a removed one -- except one the user removed, which only a manual link
    // replaces (`cache::LINK_UPSERT_KEEP_STRONGER`).
    sqlx::query(
        "INSERT INTO release_provider_links (release_id, provider, provider_id, confidence, linked_at, deleted) \
         SELECT m.new_id, t.provider, t.provider_id, t.confidence, t.linked_at, t.deleted FROM release_provider_links t JOIN release_merge m ON m.old_id = t.release_id \
         ON DUPLICATE KEY UPDATE \
           provider_id = IF(NOT VALUES(deleted) AND ((release_provider_links.deleted AND (release_provider_links.confidence <> 'manual' OR VALUES(confidence) = 'manual')) OR FIELD(VALUES(confidence),'fuzzy','name','hash','exact','manual') > FIELD(release_provider_links.confidence,'fuzzy','name','hash','exact','manual')), VALUES(provider_id), release_provider_links.provider_id), \
           linked_at = IF(NOT VALUES(deleted) AND ((release_provider_links.deleted AND (release_provider_links.confidence <> 'manual' OR VALUES(confidence) = 'manual')) OR FIELD(VALUES(confidence),'fuzzy','name','hash','exact','manual') > FIELD(release_provider_links.confidence,'fuzzy','name','hash','exact','manual')), VALUES(linked_at), release_provider_links.linked_at), \
           confidence = IF(NOT VALUES(deleted) AND ((release_provider_links.deleted AND (release_provider_links.confidence <> 'manual' OR VALUES(confidence) = 'manual')) OR FIELD(VALUES(confidence),'fuzzy','name','hash','exact','manual') > FIELD(release_provider_links.confidence,'fuzzy','name','hash','exact','manual')), VALUES(confidence), release_provider_links.confidence), \
           deleted = (release_provider_links.deleted AND (VALUES(deleted) OR (release_provider_links.confidence = 'manual' AND VALUES(confidence) <> 'manual')))",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE t FROM release_provider_links t JOIN release_merge m ON m.old_id = t.release_id").execute(&mut *tx).await?;
    // What refers to a release without being a list of its own rows.
    sqlx::query("UPDATE IGNORE ra_patches p JOIN release_merge m ON m.old_id = p.release_id SET p.release_id = m.new_id").execute(&mut *tx).await?;
    sqlx::query("INSERT IGNORE INTO release_bases (release_id, base_title_id, base_release_id, status) SELECT m.new_id, t.base_title_id, t.base_release_id, t.status FROM release_bases t JOIN release_merge m ON m.old_id = t.release_id")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE t FROM release_bases t JOIN release_merge m ON m.old_id = t.release_id").execute(&mut *tx).await?;
    sqlx::query("UPDATE release_bases rb JOIN release_merge m ON m.old_id = rb.base_release_id SET rb.base_release_id = m.new_id").execute(&mut *tx).await?;
    sqlx::query("DELETE r FROM `release` r JOIN release_merge m ON m.old_id = r.id").execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// [`fold_mapped`] for works that were each under a title: the survivor stays under its own
/// alone (two names of one hack may have been filed under two games), and a title that held
/// nothing else is gone.
async fn fold_works(conn: &mut sqlx::MySqlConnection) -> Result<(), DatabaseError> {
    let left: Vec<u64> = sqlx::query_scalar("SELECT DISTINCT t.title_id FROM title_releases t JOIN release_merge m ON m.old_id = t.release_id").fetch_all(&mut *conn).await?;
    sqlx::query("DELETE t FROM title_releases t JOIN release_merge m ON m.old_id = t.release_id WHERE EXISTS (SELECT 1 FROM title_releases s WHERE s.release_id = m.new_id)").execute(&mut *conn).await?;
    fold_mapped(conn).await?;
    for chunk in left.chunks(1000) {
        let ids = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE title t SET t.deleted = TRUE WHERE t.id IN ({ids}) AND NOT EXISTS (SELECT 1 FROM title_releases tr WHERE tr.title_id = t.id)"))).execute(&mut *conn).await?;
    }
    Ok(())
}

/// Folds each `(old, new)` release into its `new` one ([`fold_works`]: the survivor keeps its
/// own title, and a title left with nothing is gone).
pub async fn fold_pairs(pool: &MySqlPool, pairs: &[(u64, u64)]) -> Result<(), DatabaseError> {
    let mut conn = merge_conn(pool).await?;
    fold_pairs_on(&mut conn, pairs).await
}

async fn fold_pairs_on(conn: &mut sqlx::MySqlConnection, pairs: &[(u64, u64)]) -> Result<(), DatabaseError> {
    if pairs.is_empty() {
        return Ok(());
    }
    conn.execute("DELETE FROM release_merge").await?;
    for chunk in pairs.chunks(500) {
        let values = chunk.iter().map(|(o, n)| format!("({o}, {n})")).collect::<Vec<_>>().join(", ");
        sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO release_merge (old_id, new_id) VALUES {values}"))).execute(&mut *conn).await?;
    }
    fold_works(conn).await?;
    conn.execute("DELETE FROM release_merge").await?;
    Ok(())
}

/// Releases imported under the original model of a handheld that are its colour model's
/// (`import::COLOR_SYSTEMS`: every file a `.wsc` / `.ngc`) go to the colour model. One that
/// the colour model already lists under the same name folds into that release, which keeps
/// its own title; the others just move. Decided by the files the DAT lists, never by the
/// names of files on disk: scanned copies follow the release they match. Once, on a machine
/// with its own catalogue: a client takes the corrected catalogue from its server.
/// Returns (moved, folded).
pub async fn move_color_releases_once(pool: &MySqlPool) -> Result<(u64, u64), DatabaseError> {
    const NAME: &str = "color_handhelds_0062";
    let done: Option<String> = sqlx::query_scalar("SELECT name FROM data_migrations WHERE name = ?").bind(NAME).fetch_optional(pool).await?;
    if done.is_some() {
        return Ok((0, 0));
    }
    let mut moved = 0u64;
    let mut folded = 0u64;
    if super::settings::instance_mode(pool).await? != super::settings::InstanceMode::Client {
        let mut conn = merge_conn(pool).await?;
        for (mono, ext, color) in super::import::COLOR_SYSTEMS {
            let (from, to) = (mono.igdb_id(), color.igdb_id());
            let pattern = format!("%.{ext}");
            // Every file the colour model's kind.
            let found: Vec<(u64, Option<u64>)> = sqlx::query_as(
                "SELECT r.id, (SELECT t.id FROM `release` t WHERE t.app_platform_id = ? AND t.name_hash = r.name_hash) FROM `release` r \
                 WHERE r.app_platform_id = ? AND EXISTS (SELECT 1 FROM release_contents rc WHERE rc.release_id = r.id) \
                 AND NOT EXISTS (SELECT 1 FROM release_contents rc WHERE rc.release_id = r.id AND rc.file_name NOT LIKE ?)",
            )
            .bind(to)
            .bind(from)
            .bind(&pattern)
            .fetch_all(&mut *conn)
            .await?;
            let pairs: Vec<(u64, u64)> = found.iter().filter_map(|(id, twin)| twin.map(|t| (*id, t))).collect();
            fold_pairs_on(&mut conn, &pairs).await?;
            folded += pairs.len() as u64;
            let alone: Vec<u64> = found.iter().filter(|(_, twin)| twin.is_none()).map(|(id, _)| *id).collect();
            for chunk in alone.chunks(1000) {
                let ids = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
                moved += sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE `release` SET app_platform_id = ? WHERE id IN ({ids})"))).bind(to).execute(&mut *conn).await?.rows_affected();
            }
        }
        if moved + folded > 0 {
            log::info!("colour handhelds: {moved} release(s) moved to the colour model, {folded} folded into the one it already listed");
        }
    }
    sqlx::query("INSERT IGNORE INTO data_migrations (name) VALUES (?)").bind(NAME).execute(pool).await?;
    Ok((moved, folded))
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct FoldSummary {
    /// Releases that were another version of a work already listed, by name.
    pub by_name: u64,
    /// Releases whose every ROM another release of a patched work already lists.
    pub by_content: u64,
}

/// One release per patched work, whichever DATs list it and however they name it.
///
/// An import already joins a work by name (`patch_names`: "... [T-En by X v1.0]" and
/// "... v1.1" are one release). This is for what was imported before it did, and for what
/// only the bytes can tell -- the same ROM under two names:
///
/// 1. *by name*: patched releases of one platform whose names are one work once the version
///    is taken out fold into the one named for the work (renamed to it when none is), each
///    one's files marked with the version its name gave.
/// 2. *by content*: a patched release whose every ROM is listed by another patched release
///    of the same platform is that release under another name, and folds into it. Sharing
///    some ROMs is not enough: two mods of one game share most of the game.
///
/// Commercial releases are never folded: their revisions are releases of their own.
pub async fn fold_patched_works(pool: &MySqlPool, progress: &super::jobs::Progress) -> Result<FoldSummary, DatabaseError> {
    use std::collections::BTreeMap;
    let mut summary = FoldSummary::default();
    let patched = "deleted = FALSE AND material IS NULL AND game_kind IN ('hack', 'mod', 'translation')";
    let mut conn = merge_conn(pool).await?;
    // An ordinary work table (migration 0050), emptied before and after: a TEMPORARY one
    // needs a privilege the app's database user may not have.
    conn.execute("DELETE FROM release_merge").await?;

    // 1. by name
    progress.force(0, Some(2), "looking for versions of one patched work listed as releases of their own").await;
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT id, app_platform_id, name FROM `release` WHERE {patched} ORDER BY id"))).fetch_all(&mut *conn).await?;
    // (platform, work) -> (id, name, variant)
    let mut works: BTreeMap<(u16, String), Vec<(u64, String, Option<String>, String)>> = BTreeMap::new();
    for r in &rows {
        let name: String = r.try_get("name")?;
        let info = super::patch_names::analyze(&name);
        works.entry((r.try_get("app_platform_id")?, info.work.to_lowercase())).or_default().push((r.try_get("id")?, name, info.variant, info.work));
    }
    for members in works.values().filter(|m| m.len() > 1) {
        let work = &members[0].3;
        let keep = members.iter().find(|(_, name, _, _)| name == work).unwrap_or(&members[0]).0;
        for (id, name, variant, _) in members {
            // Which build each file is: what the name said, or the name itself.
            let label: String = variant.clone().unwrap_or_else(|| name.clone()).chars().take(64).collect();
            if *id != keep || name != work {
                sqlx::query("UPDATE release_contents SET variant = ? WHERE release_id = ? AND variant IS NULL").bind(&label).bind(id).execute(&mut *conn).await?;
            }
            if *id != keep {
                sqlx::query("INSERT IGNORE INTO release_merge (old_id, new_id) VALUES (?, ?)").bind(id).bind(keep).execute(&mut *conn).await?;
                summary.by_name += 1;
            }
        }
        // The survivor is the work, not one version of it.
        sqlx::query("UPDATE IGNORE `release` SET name = ?, name_hash = ? WHERE id = ? AND name <> ?").bind(work).bind(super::import::sha1_hex(work)).bind(keep).bind(work).execute(&mut *conn).await?;
    }
    if summary.by_name > 0 {
        fold_works(&mut conn).await?;
    }

    // 2. by content: until nothing folds (a release folded into one that folds in turn).
    progress.force(1, Some(2), "looking for patched releases that are another's ROMs under another name").await;
    for _ in 0..5 {
        conn.execute("DELETE FROM release_merge").await?;
        let mapped = sqlx::query(
            "INSERT INTO release_merge (old_id, new_id) SELECT pairs.small, MIN(pairs.big) FROM ( \
               SELECT a.release_id AS small, b.release_id AS big FROM release_contents a \
               JOIN `release` ra ON ra.id = a.release_id AND ra.deleted = FALSE AND ra.material IS NULL AND ra.game_kind IN ('hack', 'mod', 'translation') \
               JOIN release_contents b ON b.content_id = a.content_id AND b.release_id <> a.release_id \
               JOIN `release` rb ON rb.id = b.release_id AND rb.deleted = FALSE AND rb.material IS NULL AND rb.game_kind IN ('hack', 'mod', 'translation') AND rb.app_platform_id = ra.app_platform_id \
               GROUP BY a.release_id, b.release_id \
               HAVING COUNT(DISTINCT a.content_id) = (SELECT COUNT(DISTINCT sa.content_id) FROM release_contents sa WHERE sa.release_id = a.release_id) \
                  AND (COUNT(DISTINCT a.content_id) < (SELECT COUNT(DISTINCT sb.content_id) FROM release_contents sb WHERE sb.release_id = b.release_id) OR b.release_id < a.release_id) \
             ) pairs GROUP BY pairs.small",
        )
        .execute(&mut *conn)
        .await?
        .rows_affected();
        if mapped == 0 {
            break;
        }
        // Not both ways round, and not into a release that is itself folding this time.
        conn.execute("DELETE m FROM release_merge m JOIN (SELECT old_id FROM release_merge) gone ON gone.old_id = m.new_id").await?;
        let folding: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM release_merge").fetch_one(&mut *conn).await?;
        if folding == 0 {
            break;
        }
        fold_works(&mut conn).await?;
        summary.by_content += folding as u64;
    }
    conn.execute("DELETE FROM release_merge").await?;
    progress.force(2, Some(2), &format!("patched works: {} release(s) folded by name, {} by their ROMs", summary.by_name, summary.by_content)).await;
    Ok(summary)
}

/// Merges same platform + name releases and switches the unique key. `None` when already
/// done.
pub async fn merge_duplicate_releases(pool: &MySqlPool) -> Result<Option<MergeSummary>, DatabaseError> {
    if !old_key_present(pool).await? {
        return Ok(None);
    }
    let started = std::time::Instant::now();
    let mut conn = merge_conn(pool).await?;
    // An ordinary work table (migration 0050), emptied before and after: a TEMPORARY one
    // needs a privilege the app's database user may not have.
    conn.execute("DELETE FROM release_merge").await?;
    let mapped = sqlx::query(
        "INSERT INTO release_merge (old_id, new_id) SELECT r.id, k.keep FROM `release` r \
         JOIN (SELECT app_platform_id, name_hash, MIN(id) AS keep FROM `release` GROUP BY app_platform_id, name_hash HAVING COUNT(*) > 1) k \
           ON k.app_platform_id = r.app_platform_id AND k.name_hash = r.name_hash WHERE r.id <> k.keep",
    )
    .execute(&mut *conn)
    .await?
    .rows_affected();
    let groups: i64 = sqlx::query("SELECT COUNT(DISTINCT new_id) AS n FROM release_merge").fetch_one(&mut *conn).await?.try_get("n")?;
    if mapped > 0 {
        log::info!("release merge: folding {mapped} duplicate release row(s) into {groups} release(s) (one-time; a few minutes on a large catalog)");
    }

    fold_mapped(&mut conn).await?;
    conn.execute("DELETE FROM release_merge").await?;
    // The old key also served the source_id foreign key; it needs its own index first.
    conn.execute("ALTER TABLE `release` ADD INDEX IF NOT EXISTS idx_release_source (source_id)").await?;
    conn.execute("ALTER TABLE `release` DROP INDEX uq_release_identity, ADD UNIQUE KEY uq_release_identity (app_platform_id, name_hash)").await?;
    if mapped > 0 {
        log::info!("release merge: done in {:.0?}", started.elapsed());
    }
    Ok(Some(MergeSummary { merged_rows: mapped, groups: groups as u64 }))
}
