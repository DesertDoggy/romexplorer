//! What a patched release was made from (`release_bases`, migration 0048).
//!
//! A hack, a translation, a trainer, a save, a map pack or a patch file is of some game. The
//! import already puts it under a title by name; this records *how sure* that is, the very
//! release where one can be told, and what the user has said about it:
//!
//! | status | set by | means |
//! |---|---|---|
//! | `hash` | [`link_by_content`] | one of its files is, byte for byte, a file of that release |
//! | `exact` | [`resolve`] | the name without its patch tags is a release of the same platform |
//! | `igdb` | [`resolve`] | its own title is IGDB's child of the base title |
//! | `suggested` | [`resolve`] | under a game by name alone; shown there as a suggestion |
//! | `manual` | the user | confirmed or set by hand; nothing automatic touches it again |
//! | `rejected` | the user | unlinked; kept so the same pairing is not suggested again |
//!
//! [`resolve`] is a step of the title merge job, after add-ons are bundled, so it sees each
//! game with what the import and the bundling put under it.

use sqlx::{MySqlPool, Row};

use super::error::DatabaseError;
use super::import::sha1_hex;
use super::jobs::Progress;
use super::matching;

pub const STATUSES: [&str; 6] = ["hash", "exact", "igdb", "suggested", "manual", "rejected"];

/// A release that is made from a game: a patched ROM, or something made for one.
fn made_from(alias: &str) -> String {
    format!("({alias}.deleted = FALSE AND ({alias}.game_kind IN ('hack', 'mod', 'translation') OR {alias}.material IN ('patch', 'trainer', 'save', 'mappack')))")
}

/// A release a patch can be of: a game as it was sold, or homebrew.
fn original(alias: &str) -> String {
    format!("({alias}.deleted = FALSE AND {alias}.material IS NULL AND ({alias}.game_kind IS NULL OR {alias}.game_kind NOT IN ('hack', 'mod', 'translation')) AND {alias}.community_patched = FALSE AND {alias}.patch_type IS NULL)")
}

/// A release a DAT of games as they were sold lists, and nothing says is patched: what a
/// file shared with a patched release is the original *of*. Narrower than [`original`] on
/// purpose -- two homebrew programs, or a mod and the homebrew it is for, share fonts and
/// loaders without one being made from the other.
fn sold(alias: &str) -> String {
    format!(
        "({alias}.deleted = FALSE AND {alias}.material IS NULL AND {alias}.game_kind IS NULL AND {alias}.community_patched = FALSE AND {alias}.patch_type IS NULL \
          AND EXISTS (SELECT 1 FROM release_sources rss JOIN sources ss ON ss.id = rss.source_id WHERE rss.release_id = {alias}.id AND ss.name IN ('no-intro', 'redump', 'mame-redump', 'tosec')))"
    )
}

/// The original a collector kept beside the patch: a trainer's folder holding the game's
/// own disc image and the `.ppf`, a translation's holding the untranslated ROM. Its hash is
/// the one thing in such a DAT that says *which ROM* the patch is of -- everything else is
/// a name -- so the release whose file it is becomes the base (`hash`), and the file stops
/// being one of the patched release's: it is not a version of the patch, and counted as one
/// it made the patch "held" by anyone who owns the game, and a candidate for the game's own
/// file in the hash match.
///
/// An entry that is *nothing but* such files (the same ROM under another name) keeps them:
/// there would be nothing left of it. On the user's collections: about 450 entries of
/// 86,000 hold an original, 166 of them nothing else.
///
/// Returns (releases linked or re-linked, files set aside). Runs from [`resolve`], and after
/// a manual source's import, since importing a DAT again lists the file again.
pub async fn link_by_content(pool: &MySqlPool) -> Result<(u64, u64), DatabaseError> {
    use std::collections::BTreeMap;
    let (made, sold) = (made_from("p"), sold("b"));
    let sources: Vec<u8> = sqlx::query_scalar("SELECT id FROM sources WHERE name IN ('no-intro', 'redump', 'mame-redump', 'tosec')").fetch_all(pool).await?;
    if sources.is_empty() {
        return Ok((0, 0));
    }
    // Read from the originals' side, in this order: each file of a DAT of originals is
    // looked up once among the patched releases' files, and almost none is there. From the
    // other side every file of a patched release meets every other release that lists it,
    // and mods and map packs share thousands of files with each other -- 18 s against 0.1 s
    // on 100,000 patched and 7,600 original files, and worse the more mods there are.
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT STRAIGHT_JOIN p.id AS pid, bc.content_id, MIN(b.id) AS bid FROM release_contents bc \
         JOIN release_contents pc ON pc.content_id = bc.content_id AND pc.release_id <> bc.release_id \
         JOIN `release` p ON p.id = pc.release_id AND {made} \
         JOIN `release` b ON b.id = bc.release_id AND b.app_platform_id = p.app_platform_id AND {sold} \
         WHERE bc.source_id IN ({}) GROUP BY p.id, bc.content_id",
        sources.iter().map(u8::to_string).collect::<Vec<_>>().join(",")
    )))
    .fetch_all(pool)
    .await?;
    // patched release -> (the files that are an original's, the lowest such original)
    let mut shared: BTreeMap<u64, (Vec<u64>, u64)> = BTreeMap::new();
    for r in &rows {
        let entry = shared.entry(r.try_get("pid")?).or_insert((Vec::new(), u64::MAX));
        entry.0.push(r.try_get("content_id")?);
        entry.1 = entry.1.min(r.try_get("bid")?);
    }
    let (mut linked, mut set_aside) = (0, 0);
    for (release_id, (contents, base)) in &shared {
        let mut tx = pool.begin().await?;
        // Asked first: the connection reports a row an upsert merely found as affected, so
        // the statement cannot say whether anything changed, and the user's word stands.
        let have: Option<(String, Option<u64>)> = sqlx::query_as("SELECT status, base_release_id FROM release_bases WHERE release_id = ?").bind(release_id).fetch_optional(&mut *tx).await?;
        let settled = have.as_ref().is_some_and(|(status, linked_to)| matches!(status.as_str(), "manual" | "rejected") || (status == "hash" && *linked_to == Some(*base)));
        if !settled {
            sqlx::query(
                "INSERT INTO release_bases (release_id, base_release_id, base_title_id, status) VALUES (?, ?, (SELECT tb.title_id FROM title_releases tb WHERE tb.release_id = ? LIMIT 1), 'hash') \
                 ON DUPLICATE KEY UPDATE base_release_id = VALUES(base_release_id), base_title_id = VALUES(base_title_id), status = 'hash'",
            )
            .bind(release_id)
            .bind(base)
            .bind(base)
            .execute(&mut *tx)
            .await?;
            linked += 1;
        }
        let own: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM release_contents WHERE release_id = ?").bind(release_id).fetch_one(&mut *tx).await?;
        if own as usize > contents.len() {
            for chunk in contents.chunks(500) {
                let ids = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
                sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM release_content_sources WHERE release_id = ? AND content_id IN ({ids})"))).bind(release_id).execute(&mut *tx).await?;
                set_aside += sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM release_contents WHERE release_id = ? AND content_id IN ({ids})"))).bind(release_id).execute(&mut *tx).await?.rows_affected();
            }
        }
        tx.commit().await?;
    }
    Ok((linked, set_aside))
}

/// Undoes what a hack's hash did to its game's title before `providers::cache::APART_SQL`:
/// a title that holds a game as it was sold, yet is linked by hash to an IGDB entry that
/// IGDB calls a mod, got that link from one of the hacks under it. The link and what it
/// brought (IGDB's names, companies, genres) are removed, with the other title-level links
/// the same hash answer gave, and the title is a base game again; the next metadata pass
/// links it by its own releases. Exact and manual links are the user's or a DAT's, and stay.
/// Returns how many titles were put right.
pub async fn unlink_titles_from_hacks(pool: &MySqlPool) -> Result<u64, DatabaseError> {
    let sold = sold("b");
    let titles: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT t.id FROM title t JOIN title_provider_links l ON l.title_id = t.id AND l.provider = 'igdb' AND l.deleted = FALSE AND l.confidence IN ('hash', 'name', 'fuzzy') \
         WHERE t.deleted = FALSE AND t.category = 'mod' \
           AND EXISTS (SELECT 1 FROM title_releases tb JOIN `release` b ON b.id = tb.release_id WHERE tb.title_id = t.id AND {sold}) \
           AND EXISTS (SELECT 1 FROM title_releases tp JOIN `release` p ON p.id = tp.release_id WHERE tp.title_id = t.id AND p.deleted = FALSE AND p.game_kind IN ('hack', 'mod', 'translation'))"
    )))
    .fetch_all(pool)
    .await?;
    for title in &titles {
        let mut tx = pool.begin().await?;
        sqlx::query("UPDATE title_provider_links SET deleted = TRUE WHERE title_id = ? AND confidence IN ('hash', 'name', 'fuzzy')").bind(title).execute(&mut *tx).await?;
        for table in ["title_locales", "title_alt_names", "title_genres", "title_perspectives", "title_franchises", "title_companies", "title_keywords"] {
            sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {table} WHERE title_id = ? AND source = 'igdb'"))).bind(title).execute(&mut *tx).await?;
        }
        sqlx::query("UPDATE title SET category = 'base_game', basegame_title_id = NULL WHERE id = ? AND basegame_locked = FALSE").bind(title).execute(&mut *tx).await?;
        tx.commit().await?;
    }
    Ok(titles.len() as u64)
}

/// Gives a release the kind its own row already states and `game_kind` does not: the
/// category a DAT filed it under (RetroAchievements' "Hack", a manual source's
/// "Translation"), or the patch flags a name's tags set. The import stamps the kind as it
/// writes a release, so a row that came another way -- a catalogue restored from a backup
/// made before kinds existed, a sync from a server that has none -- has every sign of a hack
/// except the one the filters, the badges and the title pages read. A release a DAT of
/// originals lists is left alone: there the missing kind is the point (see `import_chunk`).
/// Returns how many were given one.
pub async fn restore_kinds(pool: &MySqlPool) -> Result<u64, DatabaseError> {
    let listed = "EXISTS (SELECT 1 FROM release_sources rss JOIN sources ss ON ss.id = rss.source_id WHERE rss.release_id = r.id AND ss.name IN ('no-intro', 'redump', 'mame-redump', 'tosec'))";
    let by_category = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE `release` r SET r.game_kind = CASE r.category WHEN 'Hack' THEN 'hack' WHEN 'Translation' THEN 'translation' WHEN 'Mod' THEN 'mod' ELSE 'homebrew' END, \
           r.patch_type = CASE r.category WHEN 'Translation' THEN 'translation' WHEN 'Homebrew' THEN r.patch_type ELSE COALESCE(r.patch_type, 'gameplay') END, \
           r.community_patched = (r.community_patched OR r.category <> 'Homebrew') \
         WHERE r.game_kind IS NULL AND r.material IS NULL AND r.deleted = FALSE AND r.category IN ('Hack', 'Translation', 'Mod', 'Homebrew') AND NOT {listed}"
    )))
    .execute(pool)
    .await?
    .rows_affected();
    let by_flags = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE `release` r SET r.game_kind = IF(r.patch_type = 'translation', 'translation', 'hack') \
         WHERE r.game_kind IS NULL AND r.material IS NULL AND r.deleted = FALSE AND (r.community_patched = TRUE OR r.patch_type IS NOT NULL) AND NOT {listed}"
    )))
    .execute(pool)
    .await?
    .rows_affected();
    Ok(by_category + by_flags)
}

/// The import's BIOS rule (`platform_map::bios_from_release`) over rows that came another
/// way -- a catalogue restored from a backup, or pulled from a server, made before system
/// files had a kind of their own: by category, "[BIOS]" name or the retrobios source. Then
/// the titles they leave with nothing else go (`title_merge::drop_standalone_titles`).
/// Returns how many releases were marked.
pub async fn mark_bios(pool: &MySqlPool) -> Result<u64, DatabaseError> {
    let marked = sqlx::query(
        "UPDATE `release` r SET r.material = 'bios' \
         WHERE r.material IS NULL AND r.deleted = FALSE \
           AND (r.category IN ('BIOS', 'Firmware') OR r.name LIKE '[BIOS]%' \
                OR EXISTS (SELECT 1 FROM release_sources rs JOIN sources s ON s.id = rs.source_id WHERE rs.release_id = r.id AND s.name = 'retrobios'))",
    )
    .execute(pool)
    .await?
    .rows_affected();
    if marked > 0 {
        super::title_merge::drop_standalone_titles(pool).await?;
    }
    Ok(marked)
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ResolveSummary {
    /// Releases whose row said hack or translation in every way but `game_kind`.
    pub kinds_restored: u64,
    /// Releases that were a BIOS or firmware in every way but `material` (`mark_bios`).
    pub bios_marked: u64,
    /// Titles of a game that a hack's hash had linked to the hack's own IGDB entry.
    pub titles_unlinked_from_hacks: u64,
    /// Linked to the release whose own file they ship: the original beside the patch.
    pub by_hash: u64,
    /// Files of an original no longer listed as a patched release's own.
    pub originals_set_aside: u64,
    /// Linked to the very release their name is made from.
    pub exact: u64,
    /// Linked to the game IGDB says theirs is made from.
    pub igdb: u64,
    /// Under a game by name alone: suggestions to look through.
    pub suggested: u64,
    /// Moved under the title of the release they are made from.
    pub moved: u64,
}

/// Fills `release_bases` for every release made from a game that has no row the user set.
/// Idempotent: a second run links only what has been imported since.
pub async fn resolve(pool: &MySqlPool, progress: &Progress) -> Result<ResolveSummary, DatabaseError> {
    let mut summary = ResolveSummary::default();
    let (made, base) = (made_from("p"), original("b"));
    // Nothing here overrules the user, and a name does not overrule a hash.
    let keep = "release_bases.status IN ('manual', 'rejected', 'hash')";

    summary.kinds_restored = restore_kinds(pool).await?;
    summary.bios_marked = mark_bios(pool).await?;
    summary.titles_unlinked_from_hacks = unlink_titles_from_hacks(pool).await?;

    // 0. hash: the original's own file, kept beside the patch.
    progress.force(0, Some(3), "linking patched releases to the original they were kept with").await;
    (summary.by_hash, summary.originals_set_aside) = link_by_content(pool).await?;

    // 1. exact: "Game (Japan)" for "Game (Japan) [T-En by X]" -- `base_hint` is that name
    //    when the DAT or the name gives it, and a release is keyed by platform and name.
    progress.force(0, Some(3), "linking patched releases to the releases they are made from").await;
    summary.exact = sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO release_bases (release_id, base_release_id, base_title_id, status) \
         SELECT p.id, b.id, (SELECT tb.title_id FROM title_releases tb WHERE tb.release_id = b.id LIMIT 1), 'exact' \
         FROM `release` p JOIN `release` b ON b.app_platform_id = p.app_platform_id AND b.name_hash = SHA1(p.base_hint) AND b.id <> p.id AND {base} \
         LEFT JOIN release_bases have ON have.release_id = p.id \
         WHERE {made} AND p.base_hint IS NOT NULL AND (have.release_id IS NULL OR (have.status NOT IN ('manual', 'rejected', 'hash') AND NOT (have.status = 'exact' AND have.base_release_id <=> b.id))) \
         ON DUPLICATE KEY UPDATE base_release_id = IF({keep}, release_bases.base_release_id, VALUES(base_release_id)), \
           base_title_id = IF({keep}, release_bases.base_title_id, VALUES(base_title_id)), status = IF({keep}, release_bases.status, 'exact')"
    )))
    .execute(pool)
    .await?
    .rows_affected();
    // Under the same title as what it is made from. A title left with nothing is gone.
    let mut tx = pool.begin().await?;
    let left: Vec<u64> = sqlx::query_scalar(
        "SELECT DISTINCT tr.title_id FROM title_releases tr JOIN release_bases rb ON rb.release_id = tr.release_id WHERE rb.status IN ('exact', 'hash') AND rb.base_title_id IS NOT NULL AND tr.title_id <> rb.base_title_id",
    )
    .fetch_all(&mut *tx)
    .await?;
    summary.moved = sqlx::query(
        "UPDATE IGNORE title_releases tr JOIN release_bases rb ON rb.release_id = tr.release_id SET tr.title_id = rb.base_title_id WHERE rb.status IN ('exact', 'hash') AND rb.base_title_id IS NOT NULL AND tr.title_id <> rb.base_title_id",
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    for chunk in left.chunks(1000) {
        let ids = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE title t SET t.deleted = TRUE WHERE t.id IN ({ids}) AND NOT EXISTS (SELECT 1 FROM title_releases tr WHERE tr.title_id = t.id)"))).execute(&mut *tx).await?;
    }
    tx.commit().await?;

    // The titles that have a game a patch can be of, once: asking it of each patched release
    // in turn reads a title's every release each time, and one game can have thirty
    // thousand hacks under it.
    //
    // Each such list is to be worked out once and joined to: left to itself the optimizer
    // works it out again for every patched release ("lateral derived"), which is the same
    // reading of thirty thousand releases thirty thousand times. Measured on 30,000 hacks of
    // one game: over ten minutes, against 0.3 s with the switch off for the statement.
    const ONCE: &str = "SET STATEMENT optimizer_switch = 'split_materialized=off' FOR ";
    let game = original("g");
    let with_game = format!("(SELECT DISTINCT tg.title_id FROM title_releases tg JOIN `release` g ON g.id = tg.release_id WHERE {game})");

    // 2. igdb: the patched game has an IGDB entry of its own, whose parent is the original.
    //    It keeps its title -- that entry's page -- and is listed on the original's too.
    progress.force(1, Some(3), "linking patched games to the game IGDB says they are made from").await;
    summary.igdb = sqlx::query(sqlx::AssertSqlSafe(format!(
        "{ONCE}INSERT IGNORE INTO release_bases (release_id, base_title_id, status) \
         SELECT p.id, t.basegame_title_id, 'igdb' FROM `release` p JOIN title_releases tp ON tp.release_id = p.id \
         JOIN title t ON t.id = tp.title_id AND t.deleted = FALSE JOIN title bt ON bt.id = t.basegame_title_id AND bt.deleted = FALSE \
         LEFT JOIN {with_game} own ON own.title_id = t.id LEFT JOIN release_bases have ON have.release_id = p.id \
         WHERE {made} AND have.release_id IS NULL AND own.title_id IS NULL"
    )))
    .execute(pool)
    .await?
    .rows_affected();

    // 3. suggested: the import put it under a game because a folder or its own name said
    //    so. Shown there, marked, until the user says yes or no.
    progress.force(2, Some(3), "noting the patched releases placed under a game by name").await;
    summary.suggested = sqlx::query(sqlx::AssertSqlSafe(format!(
        "{ONCE}INSERT IGNORE INTO release_bases (release_id, base_title_id, status) \
         SELECT p.id, tp.title_id, 'suggested' FROM `release` p JOIN title_releases tp ON tp.release_id = p.id JOIN title t ON t.id = tp.title_id AND t.deleted = FALSE \
         JOIN {with_game} own ON own.title_id = tp.title_id LEFT JOIN release_bases have ON have.release_id = p.id \
         WHERE {made} AND have.release_id IS NULL"
    )))
    .execute(pool)
    .await?
    .rows_affected();
    // The release of that game it is most likely of, on its own platform: its own region's,
    // else the one most patches are made from (USA, then World, then Europe). One release
    // per title, platform and region is worked out once and joined to.
    let waiting = "rb.status IN ('suggested', 'igdb') AND rb.base_release_id IS NULL";
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "{ONCE}UPDATE release_bases rb JOIN `release` p ON p.id = rb.release_id \
         JOIN (SELECT tg.title_id, g.app_platform_id, g.region, MIN(g.id) AS id FROM title_releases tg JOIN `release` g ON g.id = tg.release_id WHERE {game} GROUP BY tg.title_id, g.app_platform_id, g.region) o \
           ON o.title_id = rb.base_title_id AND o.app_platform_id = p.app_platform_id AND o.region = p.region \
         SET rb.base_release_id = o.id WHERE {waiting}"
    )))
    .execute(pool)
    .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "{ONCE}UPDATE release_bases rb JOIN `release` p ON p.id = rb.release_id \
         JOIN (SELECT tg.title_id, g.app_platform_id, \
                 CAST(SUBSTRING_INDEX(MIN(CONCAT(CASE WHEN FIND_IN_SET('US', g.region) THEN 0 WHEN FIND_IN_SET('WORLD', g.region) THEN 1 WHEN FIND_IN_SET('EU', g.region) THEN 2 ELSE 3 END, ':', LPAD(g.id, 20, '0'))), ':', -1) AS UNSIGNED) AS id \
               FROM title_releases tg JOIN `release` g ON g.id = tg.release_id WHERE {game} GROUP BY tg.title_id, g.app_platform_id) o \
           ON o.title_id = rb.base_title_id AND o.app_platform_id = p.app_platform_id \
         SET rb.base_release_id = o.id WHERE {waiting}"
    )))
    .execute(pool)
    .await?;
    progress.force(3, Some(3), &format!("patched releases linked: {} by hash, {} exact, {} by IGDB, {} suggested", summary.by_hash, summary.exact, summary.igdb, summary.suggested)).await;
    Ok(summary)
}

/// Confirms links as they stand: `manual`, so nothing automatic changes them. Returns how
/// many were confirmed. `ids` of `None` is every suggestion.
pub async fn confirm(pool: &MySqlPool, ids: Option<&[u64]>) -> Result<u64, DatabaseError> {
    let scope = match ids {
        Some(ids) if ids.is_empty() => return Ok(0),
        Some(ids) => format!("release_id IN ({}) AND status IN ('suggested', 'igdb', 'exact', 'hash')", ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",")),
        None => "status = 'suggested'".to_string(),
    };
    Ok(sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE release_bases SET status = 'manual' WHERE {scope}"))).execute(pool).await?.rows_affected())
}

/// Sets what `release_id` is made from by hand: a title, a release, or both (a release
/// alone names its title). The release moves under that title.
pub async fn set_base(pool: &MySqlPool, release_id: u64, base_title: Option<u64>, base_release: Option<u64>) -> Result<(), DatabaseError> {
    if base_release == Some(release_id) {
        return Err(DatabaseError::Config("a release cannot be made from itself".into()));
    }
    let title_of_release: Option<u64> = match base_release {
        Some(id) => {
            let live: Option<u64> = sqlx::query_scalar("SELECT id FROM `release` WHERE id = ? AND deleted = FALSE").bind(id).fetch_optional(pool).await?;
            if live.is_none() {
                return Err(DatabaseError::Config(format!("no release {id}")));
            }
            sqlx::query_scalar("SELECT title_id FROM title_releases WHERE release_id = ? LIMIT 1").bind(id).fetch_optional(pool).await?
        }
        None => None,
    };
    let Some(title) = base_title.or(title_of_release) else { return Err(DatabaseError::Config("give the title or the release it is made from".into())) };
    if sqlx::query_scalar::<_, u64>("SELECT id FROM title WHERE id = ? AND deleted = FALSE").bind(title).fetch_optional(pool).await?.is_none() {
        return Err(DatabaseError::Config(format!("no title {title}")));
    }
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO release_bases (release_id, base_title_id, base_release_id, status) VALUES (?, ?, ?, 'manual') \
         ON DUPLICATE KEY UPDATE base_title_id = VALUES(base_title_id), base_release_id = VALUES(base_release_id), status = 'manual'",
    )
    .bind(release_id)
    .bind(title)
    .bind(base_release)
    .execute(&mut *tx)
    .await?;
    let old: Vec<u64> = sqlx::query_scalar("SELECT title_id FROM title_releases WHERE release_id = ? AND title_id <> ?").bind(release_id).bind(title).fetch_all(&mut *tx).await?;
    sqlx::query("DELETE FROM title_releases WHERE release_id = ? AND title_id <> ?").bind(release_id).bind(title).execute(&mut *tx).await?;
    sqlx::query("INSERT IGNORE INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(title).bind(release_id).execute(&mut *tx).await?;
    for id in old {
        sqlx::query("UPDATE title t SET t.deleted = TRUE WHERE t.id = ? AND NOT EXISTS (SELECT 1 FROM title_releases tr WHERE tr.title_id = t.id)").bind(id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Unlinks: the release is no longer listed under that game, and the pairing is remembered
/// (`rejected`) so it is not suggested again. A release that sat in the game's own title
/// gets a title of its own, kept apart from the game's by a merge block. Returns how many
/// were unlinked. `ids` of `None` is every suggestion.
pub async fn reject(pool: &MySqlPool, ids: Option<&[u64]>) -> Result<u64, DatabaseError> {
    let scope = match ids {
        Some(ids) if ids.is_empty() => return Ok(0),
        Some(ids) => format!("rb.release_id IN ({}) AND rb.status <> 'rejected'", ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",")),
        None => "rb.status = 'suggested'".to_string(),
    };
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT rb.release_id, r.name, (SELECT tr.title_id FROM title_releases tr WHERE tr.release_id = rb.release_id LIMIT 1) AS title_id, rb.base_title_id FROM release_bases rb JOIN `release` r ON r.id = rb.release_id WHERE {scope}"
    )))
    .fetch_all(pool)
    .await?;
    let mut done = 0;
    for r in &rows {
        let (release_id, name): (u64, String) = (r.try_get("release_id")?, r.try_get("name")?);
        let (title, base): (Option<u64>, Option<u64>) = (r.try_get("title_id")?, r.try_get("base_title_id")?);
        let mut tx = pool.begin().await?;
        // In the game's own title: out of it, into one named for itself.
        if let (Some(title), Some(base)) = (title, base) {
            if title == base {
                let norm_name = matching::normalize_title(&name).to_lowercase();
                let new_id = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, ?, ?)")
                    .bind(name.chars().take(512).collect::<String>())
                    .bind(norm_name.chars().take(512).collect::<String>())
                    .bind(sha1_hex(&norm_name))
                    .execute(&mut *tx)
                    .await?
                    .last_insert_id();
                sqlx::query("UPDATE title_releases SET title_id = ? WHERE release_id = ? AND title_id = ?").bind(new_id).bind(release_id).bind(title).execute(&mut *tx).await?;
                sqlx::query("INSERT IGNORE INTO title_merge_blocks (title_a, title_b) VALUES (?, ?)").bind(title.min(new_id)).bind(title.max(new_id)).execute(&mut *tx).await?;
            }
        }
        sqlx::query("UPDATE release_bases SET status = 'rejected' WHERE release_id = ?").bind(release_id).execute(&mut *tx).await?;
        tx.commit().await?;
        done += 1;
    }
    Ok(done)
}

/// What the user says the language of a patched release is. `confirm` keeps it as it
/// stands, `set` replaces it, `clear` removes it; all three are `manual` afterwards, which
/// an import leaves alone -- so a cleared guess is not guessed again.
pub async fn set_language(pool: &MySqlPool, release_id: u64, action: &str, languages: Option<&str>) -> Result<(), DatabaseError> {
    let done = match action {
        "confirm" => sqlx::query("UPDATE `release` SET languages_state = 'manual' WHERE id = ? AND deleted = FALSE").bind(release_id).execute(pool).await?,
        "clear" => sqlx::query("UPDATE `release` SET languages = NULL, languages_state = 'manual' WHERE id = ? AND deleted = FALSE").bind(release_id).execute(pool).await?,
        "set" => {
            let codes: Vec<String> = languages.unwrap_or_default().split(',').map(|c| c.trim().to_ascii_lowercase()).filter(|c| !c.is_empty()).collect();
            if codes.is_empty() || codes.iter().any(|c| c.len() > 8 || !c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-')) {
                return Err(DatabaseError::Config("languages must be language codes, comma separated (en, pt-br)".into()));
            }
            sqlx::query("UPDATE `release` SET languages = ?, languages_state = 'manual' WHERE id = ? AND deleted = FALSE").bind(codes.join(",")).bind(release_id).execute(pool).await?
        }
        other => return Err(DatabaseError::Config(format!("action {other:?} must be confirm, set or clear"))),
    };
    if done.rows_affected() == 0 && sqlx::query_scalar::<_, u64>("SELECT id FROM `release` WHERE id = ? AND deleted = FALSE").bind(release_id).fetch_optional(pool).await?.is_none() {
        return Err(DatabaseError::Config(format!("no release {release_id}")));
    }
    Ok(())
}
