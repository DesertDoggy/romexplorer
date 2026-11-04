//! Merging titles that are one game under regional names ("Legend of Hogehoge V" / "Shin Legend of Hogehoge V",
//! "Dark Hoge 2" / "Hoge Chronicle"). The DAT import keys titles by normalized base name
//! within a platform, so regional renames become separate titles; they are merged
//!
//! - automatically ([`dedupe_by_igdb`]) once they share an IGDB game id and a platform. The
//!   link may come from a hash, from the user, or from the title's name being one of that
//!   game's names (`MERGE_CONFIDENCES`) -- so a catalogue with no files on disk merges too --
//!   but never from a search's fuzzy best guess. Cross-platform ports keep their own titles
//!   unless the user merges them;
//! - by the user ([`merge_titles`], the collection's "merge selected").
//!
//! The surviving title (lowest id) takes everything the others had -- releases, genres,
//! franchises, companies, keywords, perspectives, localized names, media, provider links
//! (the stronger link wins) -- and is renamed to its canonical name: the base name of its
//! best commercial release by the `canonical_name_priority` setting (default USA > EUR > WORLD
//! > ASIA > en > JPN, then the rest; [`pick_canonical`]); the display order is separate.
//! Every merged title's normalized name stays as an alias (`title_aliases`), so a DAT
//! re-import finds the merged title instead of splitting it again. Every merge is logged
//! (`title_merge_log`: reason, grade -- [`GRADES`] -- and the merged title's name), so the
//! merges of one grade can be listed and undone together ([`decouple_grade`]). Titles sharing an IGDB game that
//! the automatic pass doesn't merge -- a fuzzy link, or different platforms -- go to
//! the backlog (`title_merge_suggestions`) for the user to accept or dismiss.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use sqlx::mysql::MySqlPool;
use sqlx::Row;

use super::error::DatabaseError;
use super::import::sha1_hex;
use super::jobs::{Frame, Progress};
use super::matching;

/// IGDB link confidences the automatic pass merges on, as a SQL list. `name` is in: it is a
/// title whose normalized name equals one of the game's own names (main, regional or
/// alternative) on a compatible platform, which is all a catalogue built from DATs and a
/// provider sync ever has -- requiring a hash meant nothing merged until files were scanned.
/// `fuzzy` (a search's first result) stays a suggestion. `merge_audit` checks these merges.
const MERGE_CONFIDENCES: &str = "'exact', 'hash', 'name', 'manual'";

/// How sure a merge was (`title_merge_log.grade`): the user's own decision, a hash, or a
/// name match -- the weakest link among the merged titles decides.
pub const GRADES: [&str; 3] = ["manual", "hash", "name"];

/// Title-keyed tables and their other columns.
const JUNCTIONS: [(&str, &str); 9] = [
    // With the user's manual order (weights), which travels with the release.
    ("title_releases", "release_id, sort_weight, sort_set_at"),
    ("title_alt_names", "name, name_hash, source, comment"),
    ("title_locales", "language, region, source, name"),
    ("title_genres", "genre_id, source"),
    ("title_perspectives", "perspective_id, source"),
    ("title_franchises", "franchise_id, source"),
    ("title_companies", "company_id, role, source"),
    ("title_media", "media_id"),
    ("title_keywords", "keyword_id, source"),
];

/// Every title-keyed table the archive keeps: the junctions, plus provider links.
fn archived_tables() -> Vec<(&'static str, Vec<&'static str>)> {
    JUNCTIONS
        .iter()
        .map(|(t, c)| (*t, c.split(", ").collect()))
        .chain(std::iter::once(("title_provider_links", vec!["provider", "provider_id", "confidence", "linked_at", "deleted"])))
        .collect()
}

/// SQL building the archive's JSON for the title `id_expr`: {"<table>": [{col: value}], ...}.
fn archive_json_sql(id_expr: &str) -> String {
    let parts: Vec<String> = archived_tables()
        .iter()
        .map(|(table, cols)| {
            let obj = cols.iter().map(|c| format!("'{c}', x.{c}")).collect::<Vec<_>>().join(", ");
            format!("'{table}', COALESCE((SELECT JSON_ARRAYAGG(JSON_OBJECT({obj})) FROM {table} x WHERE x.title_id = {id_expr}), JSON_ARRAY())")
        })
        .collect();
    format!("JSON_OBJECT({})", parts.join(", "))
}

fn in_list(ids: &[u64]) -> String {
    ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",")
}

/// The title name a release name stands for (the DAT import's rule: its base name).
pub fn base_title(release_name: &str) -> String {
    let parsed = matching::parse_name(release_name);
    if parsed.base.trim().is_empty() {
        release_name.trim().to_string()
    } else {
        parsed.base.trim().to_string()
    }
}

/// A title's release as `pick_canonical` sees it.
pub struct CanonCandidate {
    pub name: String,
    pub region: Option<String>,
    pub languages: Option<String>,
    /// Its bucket's place in `release_buckets::BUCKETS` (1 = retail): a retail release names
    /// the title before anything else does, whatever its region -- then an unlicensed or
    /// aftermarket one, a patched one, a demo or pre-release build, a subset.
    pub rank: u8,
    /// Special / limited edition: commercial, but only names the title when no regular
    /// edition of the same priority does.
    pub special: bool,
}

/// The first `order` entry the release matches: a region code (upper case) in its region
/// list or a language code (lower case) in its languages; unmatched rank after the list.
fn priority_rank(c: &CanonCandidate, order: &[String]) -> usize {
    let codes = |v: &Option<String>| -> Vec<String> { v.as_deref().unwrap_or("").split(',').map(|x| x.trim().to_ascii_lowercase()).filter(|x| !x.is_empty()).collect() };
    let (regions, langs) = (codes(&c.region), codes(&c.languages));
    order
        .iter()
        .position(|o| {
            if o.chars().any(|ch| ch.is_ascii_lowercase()) { langs.contains(&o.to_ascii_lowercase()) } else { regions.contains(&super::settings::canonical_region_code(o).to_ascii_lowercase()) }
        })
        .unwrap_or(order.len())
}

/// Picks the canonical release name: by bucket (retail first), then by the
/// `canonical_name_priority` setting (default USA > Europe > World > Asia > English > Japan,
/// then the rest), then the shorter name. Display order is separate (`title_display_mode`).
pub fn pick_canonical<'a>(releases: &'a [CanonCandidate], order: &[String]) -> Option<&'a str> {
    releases.iter().min_by_key(|c| (c.rank, priority_rank(c, order), c.special, c.name.len(), c.name.clone())).map(|c| c.name.as_str())
}

/// The canonical name of `title_id` from its releases (None: no releases).
pub async fn canonical_name(pool: &MySqlPool, title_id: u64) -> Result<Option<String>, DatabaseError> {
    let order = super::settings::canonical_name_priority(pool).await?;
    let rank = super::release_buckets::rank_sql("r", super::release_buckets::unlicensed_joins_retail(pool).await?);
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT r.name, r.region, r.languages, CAST({rank} AS SIGNED) AS bucket_rank, CAST((r.material IS NOT NULL) AS SIGNED) AS bundled, CAST(r.is_special_edition AS SIGNED) AS special \
         FROM title_releases tr JOIN `release` r ON r.id = tr.release_id WHERE tr.title_id = ? AND r.deleted = FALSE"
    )))
    .bind(title_id)
    .fetch_all(pool)
    .await?;
    let releases: Vec<CanonCandidate> = rows
        .iter()
        .map(|r| Ok(CanonCandidate { name: r.try_get("name")?, region: r.try_get("region")?, languages: r.try_get("languages")?, rank: r.try_get::<i64, _>("bucket_rank")?.clamp(0, 255) as u8, special: r.try_get::<i64, _>("special")? != 0 }))
        .collect::<Result<_, sqlx::Error>>()?;
    // A title is named after its game: the DLC and soundtracks bundled with it name it only
    // when there is nothing else.
    let bundled: Vec<bool> = rows.iter().map(|r| r.try_get::<i64, _>("bundled").unwrap_or(0) != 0).collect();
    let (own, came_with): (Vec<_>, Vec<_>) = releases.into_iter().zip(bundled).partition(|(_, bundled)| !*bundled);
    let candidates: Vec<CanonCandidate> = if own.is_empty() { came_with } else { own }.into_iter().map(|(c, _)| c).collect();
    Ok(pick_canonical(&candidates, &order).map(base_title))
}

/// Takes disc media, documents and source code (`platform_map::STANDALONE`) out from under
/// the titles an import gave them, and removes the titles that held nothing else. Returns
/// how many titles went.
///
/// Migration 0046 did this once for what was marked then. It is needed again whenever an
/// import marks releases that already had a title: a DAT sync re-importing the non-game DATs
/// of a catalogue from before the kinds existed, or a DAT whose classification changed.
/// Not for links made by hand -- those are made after, and nothing here runs between a
/// person linking a soundtrack to its game and the next import of that soundtrack's DAT,
/// which leaves a release that already has a title alone. So this is called only where an
/// import has just (re)classified releases: `worker::dat_sync`.
pub async fn drop_standalone_titles(pool: &MySqlPool) -> Result<u64, DatabaseError> {
    let kinds = super::platform_map::STANDALONE.iter().map(|m| format!("'{m}'")).collect::<Vec<_>>().join(", ");
    let mut tx = pool.begin().await?;
    let gone = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE title t SET t.deleted = TRUE WHERE t.deleted = FALSE \
         AND EXISTS (SELECT 1 FROM title_releases tr JOIN `release` r ON r.id = tr.release_id WHERE tr.title_id = t.id AND r.material IN ({kinds})) \
         AND NOT EXISTS (SELECT 1 FROM title_releases tr JOIN `release` r ON r.id = tr.release_id WHERE tr.title_id = t.id AND (r.material IS NULL OR r.material NOT IN ({kinds})))"
    )))
    .execute(&mut *tx)
    .await?
    .rows_affected();
    // Only from the titles just removed: one under a live title was put there by hand.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DELETE tr FROM title_releases tr JOIN `release` r ON r.id = tr.release_id JOIN title t ON t.id = tr.title_id WHERE t.deleted = TRUE AND r.material IN ({kinds})"
    )))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(gone)
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct BundleSummary {
    /// Titles with nothing but DLC / update releases.
    pub addon_titles: u64,
    /// Of those, merged into their game's title.
    pub bundled: u64,
    /// Games that gained them.
    pub games: u64,
    /// Left alone: more than one game answers to the name.
    pub ambiguous: u64,
}

/// The games an add-on named `name` could belong to, most specific first: No-Intro names
/// DLC "Game - Pack", so every shorter " - " prefix of the name is a candidate
/// ("Taiko no Tatsujin - Doko Don! Mystery Adventure - Pack 1" is tried as the Mystery
/// Adventure's before Taiko no Tatsujin's).
pub fn addon_base_names(name: &str) -> Vec<String> {
    let parts: Vec<&str> = name.split(" - ").collect();
    (1..parts.len()).rev().map(|n| parts[..n].join(" - ")).collect()
}

fn platform_set(list: Option<String>) -> BTreeSet<u16> {
    list.unwrap_or_default().split(',').filter_map(|p| p.trim().parse().ok()).collect()
}

/// Where a title merge pass started: the database's clock and the highest title and release
/// ids (migration 0053). What changed after it is what the next pass looks at.
#[derive(Debug, Clone, PartialEq)]
pub struct PassMark {
    pub started_at: chrono::NaiveDateTime,
    pub max_title_id: u64,
    pub max_release_id: u64,
}

const PASS: &str = "title_merge";

/// Now, as a pass starting now would record it.
pub async fn pass_start(pool: &MySqlPool) -> Result<PassMark, DatabaseError> {
    let row = sqlx::query("SELECT CAST(NOW() AS DATETIME) AS at, CAST(COALESCE((SELECT MAX(id) FROM title), 0) AS UNSIGNED) AS t, CAST(COALESCE((SELECT MAX(id) FROM `release`), 0) AS UNSIGNED) AS r").fetch_one(pool).await?;
    Ok(PassMark { started_at: row.try_get("at")?, max_title_id: row.try_get("t")?, max_release_id: row.try_get("r")? })
}

/// The mark as stored, whatever it describes.
pub async fn stored_pass(pool: &MySqlPool) -> Result<Option<PassMark>, DatabaseError> {
    let Some(row) = sqlx::query("SELECT started_at, max_title_id, max_release_id FROM pass_marks WHERE name = ?").bind(PASS).fetch_optional(pool).await? else { return Ok(None) };
    Ok(Some(PassMark { started_at: row.try_get("started_at")?, max_title_id: row.try_get("max_title_id")?, max_release_id: row.try_get("max_release_id")? }))
}

/// The last completed pass's mark, if it still describes this catalogue. A catalogue whose
/// highest ids went down since was replaced (restored, pulled from a server): its next pass
/// is a full one.
pub fn usable_pass(stored: Option<PassMark>, now: &PassMark) -> Option<PassMark> {
    stored.filter(|mark| mark.max_title_id <= now.max_title_id && mark.max_release_id <= now.max_release_id)
}

/// Records a completed pass's mark.
pub async fn save_pass(pool: &MySqlPool, mark: &PassMark) -> Result<(), DatabaseError> {
    sqlx::query("INSERT INTO pass_marks (name, started_at, max_title_id, max_release_id) VALUES (?, ?, ?, ?) ON DUPLICATE KEY UPDATE started_at = VALUES(started_at), max_title_id = VALUES(max_title_id), max_release_id = VALUES(max_release_id)")
        .bind(PASS)
        .bind(mark.started_at)
        .bind(mark.max_title_id)
        .bind(mark.max_release_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Forgets the mark: the next pass goes over everything.
pub async fn forget_pass(pool: &MySqlPool) -> Result<(), DatabaseError> {
    sqlx::query("DELETE FROM pass_marks WHERE name = ?").bind(PASS).execute(pool).await?;
    Ok(())
}

/// What a pass looks at: everything, or the titles touched since the last pass.
#[derive(Debug, Clone, Default)]
pub struct PassScope {
    /// `None`: every title (a full pass).
    pub dirty: Option<HashSet<u64>>,
    /// When the last pass started (for merges logged since).
    pub since: Option<chrono::NaiveDateTime>,
}

impl PassScope {
    pub fn full() -> Self {
        Self::default()
    }

    pub(crate) fn touches(&self, titles: &[u64]) -> bool {
        self.dirty.as_ref().is_none_or(|d| titles.iter().any(|t| d.contains(t)))
    }

    pub(crate) fn has(&self, title: u64) -> bool {
        self.dirty.as_ref().is_none_or(|d| d.contains(&title))
    }
}

/// The titles touched since `last`, into `title_merge_dirty` and back as a [`PassScope`]:
/// new titles, titles of new releases, titles whose own or whose releases' provider links
/// were made or changed, and titles a merge survived into. Emptied again by
/// [`clear_dirty`] at the end of the pass.
pub async fn collect_dirty(pool: &MySqlPool, last: &PassMark) -> Result<PassScope, DatabaseError> {
    sqlx::query("DELETE FROM title_merge_dirty").execute(pool).await?;
    sqlx::query("INSERT IGNORE INTO title_merge_dirty (title_id) SELECT id FROM title WHERE id > ? AND deleted = FALSE").bind(last.max_title_id).execute(pool).await?;
    sqlx::query("INSERT IGNORE INTO title_merge_dirty (title_id) SELECT tr.title_id FROM title_releases tr WHERE tr.release_id > ?").bind(last.max_release_id).execute(pool).await?;
    sqlx::query("INSERT IGNORE INTO title_merge_dirty (title_id) SELECT title_id FROM title_provider_links WHERE linked_at >= ?").bind(last.started_at).execute(pool).await?;
    sqlx::query("INSERT IGNORE INTO title_merge_dirty (title_id) SELECT tr.title_id FROM release_provider_links rpl JOIN title_releases tr ON tr.release_id = rpl.release_id WHERE rpl.linked_at >= ?").bind(last.started_at).execute(pool).await?;
    sqlx::query("INSERT IGNORE INTO title_merge_dirty (title_id) SELECT survivor_id FROM title_merge_log WHERE merged_at >= ?").bind(last.started_at).execute(pool).await?;
    let ids: Vec<u64> = sqlx::query_scalar("SELECT title_id FROM title_merge_dirty").fetch_all(pool).await?;
    Ok(PassScope { dirty: Some(ids.into_iter().collect()), since: Some(last.started_at) })
}

pub async fn clear_dirty(pool: &MySqlPool) -> Result<(), DatabaseError> {
    sqlx::query("DELETE FROM title_merge_dirty").execute(pool).await?;
    Ok(())
}

/// Merges every title that is only a game's extras (`platform_map::GAME_EXTRAS`: DLC, updates,
/// themes and avatars) into the title of the game it is for, found
/// by name ([`addon_base_names`]) among the titles that have a game release on one of the
/// add-on's platforms (any platform when the add-on's DAT names none). Updates mostly need
/// nothing: "Game (v1.01) (Update)" already shares the game's title.
///
/// These are ordinary merges (`reason` "addon", grade `name`): listed under combined titles,
/// undone there, and a pair the user split is not merged again. What the name does not
/// settle stays a title of its own, to merge by hand.
///
/// Scoped ([`PassScope`]): an add-on title touched since the last pass is matched against
/// every game; one that was not, only against games touched since (a game that arrived
/// after its DLC) -- anything else was already looked at and found nothing new.
pub async fn bundle_addons(pool: &MySqlPool, progress: &Progress) -> Result<BundleSummary, DatabaseError> {
    bundle_addons_in(pool, progress, &PassScope::full()).await
}

pub async fn bundle_addons_in(pool: &MySqlPool, progress: &Progress, scope: &PassScope) -> Result<BundleSummary, DatabaseError> {
    let mut summary = BundleSummary::default();
    progress.force(0, None, "looking for DLC and updates without their game").await;
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT t.id, t.name, CAST(GROUP_CONCAT(DISTINCT r.app_platform_id) AS CHAR) AS platforms FROM title t JOIN title_releases tr ON tr.title_id = t.id JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE \
         WHERE t.deleted = FALSE GROUP BY t.id, t.name HAVING SUM(r.material IS NULL OR r.material NOT IN ({})) = 0",
        super::platform_map::game_extras_sql()
    )))
    .fetch_all(pool)
    .await?;
    let mut addons: Vec<(u64, BTreeSet<u16>, Vec<String>)> = Vec::with_capacity(rows.len());
    for r in &rows {
        let hashes = addon_base_names(&r.try_get::<String, _>("name")?).iter().map(|n| sha1_hex(&matching::normalize_title(n).to_lowercase())).collect();
        addons.push((r.try_get("id")?, platform_set(r.try_get("platforms")?), hashes));
    }
    summary.addon_titles = addons.len() as u64;

    // Every candidate name at once: hash -> the titles answering to it (by name or alias)
    // that have a game release, with the platforms of those.
    let wanted: Vec<&String> = addons.iter().flat_map(|(_, _, h)| h).collect::<BTreeSet<_>>().into_iter().collect();
    let mut games: HashMap<String, Vec<(u64, BTreeSet<u16>)>> = HashMap::new();
    for chunk in wanted.chunks(1000) {
        let marks = vec!["?"; chunk.len()].join(",");
        let sql = format!(
            "SELECT x.norm_hash, t.id, CAST(GROUP_CONCAT(DISTINCT r.app_platform_id) AS CHAR) AS platforms \
             FROM (SELECT norm_hash, id AS title_id FROM title WHERE deleted = FALSE AND norm_hash IN ({marks}) UNION SELECT norm_hash, title_id FROM title_aliases WHERE norm_hash IN ({marks})) x \
             JOIN title t ON t.id = x.title_id AND t.deleted = FALSE JOIN title_releases tr ON tr.title_id = t.id JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE AND r.material IS NULL \
             GROUP BY x.norm_hash, t.id"
        );
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
        for _ in 0..2 {
            for hash in chunk {
                query = query.bind(*hash);
            }
        }
        for r in query.fetch_all(pool).await? {
            games.entry(r.try_get("norm_hash")?).or_default().push((r.try_get("id")?, platform_set(r.try_get("platforms")?)));
        }
    }

    let mut by_game: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    for (id, platforms, hashes) in &addons {
        let unplaced = platforms.iter().all(|p| *p == 0);
        for hash in hashes {
            let fits: Vec<u64> = games.get(hash).map(|c| c.iter().filter(|(game, on)| game != id && (unplaced || !on.is_disjoint(platforms))).map(|(game, _)| *game).collect()).unwrap_or_default();
            // An add-on nothing touched was looked at before: again only when a game it
            // fits was touched (one that arrived after it), and judged on all it fits.
            if !scope.has(*id) && !fits.iter().any(|g| scope.has(*g)) {
                continue;
            }
            match fits.as_slice() {
                [] => continue,
                [game] => by_game.entry(*game).or_default().push(*id),
                _ => summary.ambiguous += 1,
            }
            break;
        }
    }

    let blocks = merge_blocks(pool).await?;
    let total = by_game.len() as u64;
    for (n, (game, addons)) in by_game.into_iter().enumerate() {
        let mut group = vec![game];
        group.extend(addons);
        let group = without_blocked(&group, &blocks);
        if group.len() > 1 {
            merge_titles(pool, &group, "addon", "name").await?;
            summary.games += 1;
            summary.bundled += group.len() as u64 - 1;
        }
        if n % 50 == 0 {
            progress.force(n as u64, Some(total), &format!("bundling DLC and updates with their games: {} of {total} game(s)", n + 1)).await;
        }
    }
    Ok(summary)
}

/// Renames `title_id` to its canonical name; its old normalized name stays an alias.
pub async fn rename_canonical(pool: &MySqlPool, title_id: u64) -> Result<(), DatabaseError> {
    let Some(name) = canonical_name(pool, title_id).await? else { return Ok(()) };
    let norm_name = matching::normalize_title(&name).to_lowercase();
    let norm_hash = sha1_hex(&norm_name);
    sqlx::query("INSERT IGNORE INTO title_aliases (norm_hash, title_id) SELECT norm_hash, id FROM title WHERE id = ?").bind(title_id).execute(pool).await?;
    sqlx::query("INSERT IGNORE INTO title_aliases (norm_hash, title_id) VALUES (?, ?)").bind(&norm_hash).bind(title_id).execute(pool).await?;
    sqlx::query("UPDATE title SET name = ?, norm_name = ?, norm_hash = ? WHERE id = ?").bind(name.chars().take(512).collect::<String>()).bind(norm_name.chars().take(512).collect::<String>()).bind(&norm_hash).bind(title_id).execute(pool).await?;
    Ok(())
}

/// Merges `ids` into the lowest of them and renames it canonically; `reason` (`manual`,
/// `igdb:<id>`, `suggestion:igdb:<id>`) and `grade` ([`GRADES`]) go into the merge log.
/// Returns the survivor.
pub async fn merge_titles(pool: &MySqlPool, ids: &[u64], reason: &str, grade: &str) -> Result<u64, DatabaseError> {
    let ids: BTreeSet<u64> = ids.iter().copied().collect();
    let Some(&keep) = ids.first() else { return Err(DatabaseError::Config("no titles to merge".to_string())) };
    let others: Vec<u64> = ids.iter().copied().filter(|id| *id != keep).collect();
    if others.is_empty() {
        return Ok(keep);
    }
    let found: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM title WHERE id IN ({}) AND deleted = FALSE", in_list(&ids.iter().copied().collect::<Vec<_>>())))).fetch_one(pool).await?;
    if found as usize != ids.len() {
        return Err(DatabaseError::Config("some of those titles don't exist".to_string()));
    }
    let list = in_list(&others);
    let mut tx = pool.begin().await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO title_merge_log (survivor_id, merged_title_id, merged_name, merged_norm_hash, reason, grade) SELECT {keep}, id, name, norm_hash, ?, ? FROM title WHERE id IN ({list})")))
        .bind(reason.chars().take(64).collect::<String>())
        .bind(grade)
        .execute(&mut *tx)
        .await?;
    // Each merged title as it is (row + its rows everywhere), so decoupling restores it.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO title_merge_archive (log_id, title_id, name, norm_name, norm_hash, category, basegame_title_id, rows_json) \
         SELECT l.id, t.id, t.name, t.norm_name, t.norm_hash, t.category, t.basegame_title_id, {} FROM title t \
         JOIN title_merge_log l ON l.merged_title_id = t.id AND l.survivor_id = {keep} AND l.undone_at IS NULL LEFT JOIN title_merge_archive a ON a.log_id = l.id \
         WHERE t.id IN ({list}) AND a.log_id IS NULL",
        archive_json_sql("t.id")
    )))
    .execute(&mut *tx)
    .await?;
    // Earlier merges into a merged title now belong to the survivor; blocks follow the
    // titles, and merging a blocked pair on purpose lifts the block.
    sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE title_merge_log SET survivor_id = {keep} WHERE survivor_id IN ({list}) AND undone_at IS NULL"))).execute(&mut *tx).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT IGNORE INTO title_merge_blocks (title_a, title_b) SELECT LEAST(x, y), GREATEST(x, y) FROM \
         (SELECT IF(title_a IN ({list}), {keep}, title_a) AS x, IF(title_b IN ({list}), {keep}, title_b) AS y FROM title_merge_blocks WHERE title_a IN ({list}) OR title_b IN ({list})) m WHERE x <> y"
    )))
    .execute(&mut *tx)
    .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DELETE FROM title_merge_blocks WHERE title_a IN ({list}) OR title_b IN ({list}) OR (title_a = {keep} AND title_b IN ({keep},{list})) OR (title_b = {keep} AND title_a IN ({keep},{list}))"
    )))
    .execute(&mut *tx)
    .await?;
    // Aliases: every merged title's normalized name (and its own aliases) -> the survivor.
    sqlx::query(sqlx::AssertSqlSafe(format!("INSERT IGNORE INTO title_aliases (norm_hash, title_id) SELECT norm_hash, {keep} FROM title WHERE id IN ({keep},{list})"))).execute(&mut *tx).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("INSERT IGNORE INTO title_aliases (norm_hash, title_id) SELECT norm_hash, {keep} FROM title_aliases WHERE title_id IN ({list})"))).execute(&mut *tx).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM title_aliases WHERE title_id IN ({list})"))).execute(&mut *tx).await?;
    // Both manually ordered: the merged titles' releases go after the survivor's, keeping
    // their own order.
    let survivor_max: Option<f64> = sqlx::query_scalar("SELECT MAX(sort_weight) FROM title_releases WHERE title_id = ?").bind(keep).fetch_one(&mut *tx).await?;
    if let Some(max) = survivor_max {
        let merged_min: Option<f64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT MIN(sort_weight) FROM title_releases WHERE title_id IN ({list})"))).fetch_one(&mut *tx).await?;
        if let Some(min) = merged_min {
            sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE title_releases SET sort_weight = sort_weight + ? WHERE title_id IN ({list}) AND sort_weight IS NOT NULL")))
                .bind(max + super::release_order::STEP - min)
                .execute(&mut *tx)
                .await?;
        }
    }
    for (table, columns) in JUNCTIONS {
        sqlx::query(sqlx::AssertSqlSafe(format!("INSERT IGNORE INTO {table} (title_id, {columns}) SELECT {keep}, {columns} FROM {table} WHERE title_id IN ({list})"))).execute(&mut *tx).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {table} WHERE title_id IN ({list})"))).execute(&mut *tx).await?;
    }
    // Names the user set on a merged title stay theirs on the survivor.
    let locked: Option<bool> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT MAX(locales_locked) FROM title WHERE id IN ({list})"))).fetch_one(&mut *tx).await?;
    if locked == Some(true) {
        sqlx::query("UPDATE title SET locales_locked = TRUE WHERE id = ?").bind(keep).execute(&mut *tx).await?;
    }
    let t = "title_provider_links";
    let cond = format!("NOT VALUES(deleted) AND ({t}.deleted OR FIELD(VALUES(confidence),'fuzzy','name','hash','exact','manual') > FIELD({t}.confidence,'fuzzy','name','hash','exact','manual'))");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO {t} (title_id, provider, provider_id, confidence, linked_at, deleted) SELECT {keep}, provider, provider_id, confidence, linked_at, deleted FROM {t} src WHERE src.title_id IN ({list}) \
         ON DUPLICATE KEY UPDATE provider_id = IF({cond}, VALUES(provider_id), {t}.provider_id), linked_at = IF({cond}, VALUES(linked_at), {t}.linked_at), \
         confidence = IF({cond}, VALUES(confidence), {t}.confidence), deleted = ({t}.deleted AND VALUES(deleted))"
    )))
    .execute(&mut *tx)
    .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {t} WHERE title_id IN ({list})"))).execute(&mut *tx).await?;
    // A set's title entries follow to the survivor (a set holding both keeps one).
    sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE IGNORE collection_set_entries SET title_id = {keep} WHERE title_id IN ({list})"))).execute(&mut *tx).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM collection_set_entries WHERE title_id IN ({list})"))).execute(&mut *tx).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE IGNORE collection_set_entries SET family_title_id = {keep} WHERE family_title_id IN ({list})"))).execute(&mut *tx).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM collection_set_entries WHERE family_title_id IN ({list})"))).execute(&mut *tx).await?;
    // DLC / ports pointing at a merged title point at the survivor; the survivor never
    // points at itself.
    sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE title SET basegame_title_id = {keep} WHERE basegame_title_id IN ({list})"))).execute(&mut *tx).await?;
    sqlx::query("UPDATE title SET basegame_title_id = NULL WHERE id = ? AND basegame_title_id = ?").bind(keep).bind(keep).execute(&mut *tx).await?;
    // The games a collection holds, and the user's order of a family, follow too.
    for (table, cols) in [("title_bundle_games", ["bundle_title_id", "game_title_id"]), ("title_family_order", ["root_title_id", "title_id"])] {
        for col in cols {
            sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE IGNORE {table} SET {col} = {keep} WHERE {col} IN ({list})"))).execute(&mut *tx).await?;
            sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {table} WHERE {col} IN ({list})"))).execute(&mut *tx).await?;
        }
    }
    sqlx::query("DELETE FROM title_bundle_games WHERE bundle_title_id = game_title_id").execute(&mut *tx).await?;
    // Soft delete: the row keeps its id reserved (and its name / category) for decoupling.
    sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE title SET deleted = TRUE, basegame_title_id = NULL WHERE id IN ({list})"))).execute(&mut *tx).await?;
    tx.commit().await?;
    rename_canonical(pool, keep).await?;
    Ok(keep)
}

/// Gives releases a title of their own, together, named after them (`rename_canonical`): how releases that
/// ended up with another game (a special edition with one of its stories, say) are put right.
/// The new title and each title they came from are kept from merging again automatically
/// (`title_merge_blocks`), and those titles stop claiming the releases' names for new imports
/// (`title_aliases`). Returns the new title -- or the releases' own when they are all of it.
pub async fn split_releases(pool: &MySqlPool, release_ids: &[u64]) -> Result<u64, DatabaseError> {
    let mut ids: Vec<u64> = Vec::new();
    for id in release_ids {
        if !ids.contains(id) {
            ids.push(*id);
        }
    }
    let Some(&first) = ids.first() else { return Err(DatabaseError::Config("no releases to split".to_string())) };
    let list = in_list(&ids);
    let rows: Vec<(u64, String, Option<u64>)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT r.id, r.name, (SELECT tr.title_id FROM title_releases tr WHERE tr.release_id = r.id LIMIT 1) FROM `release` r WHERE r.id IN ({list}) AND r.deleted = FALSE"
    )))
    .fetch_all(pool)
    .await?;
    if rows.len() != ids.len() {
        return Err(DatabaseError::Config("some of those releases don't exist".to_string()));
    }
    let mut olds: Vec<u64> = rows.iter().filter_map(|r| r.2).collect();
    olds.sort_unstable();
    olds.dedup();
    if let [only] = olds[..] {
        if rows.iter().all(|r| r.2.is_some()) {
            let others: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM title_releases WHERE title_id = ? AND release_id NOT IN ({list})"))).bind(only).fetch_one(pool).await?;
            if others == 0 {
                return Ok(only);
            }
        }
    }
    let name = rows.iter().find(|r| r.0 == first).map(|r| r.1.clone()).unwrap_or_default();
    let title_name = base_title(&name);
    let norm_name = matching::normalize_title(&title_name).to_lowercase();
    let norm_hash = sha1_hex(&norm_name);
    let hashes: Vec<String> = rows.iter().map(|r| sha1_hex(&matching::normalize_title(&base_title(&r.1)).to_lowercase())).collect();
    let mut tx = pool.begin().await?;
    let new_id = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, ?, ?)")
        .bind(title_name.chars().take(512).collect::<String>())
        .bind(norm_name.chars().take(512).collect::<String>())
        .bind(&norm_hash)
        .execute(&mut *tx)
        .await?
        .last_insert_id();
    sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM title_releases WHERE release_id IN ({list})"))).execute(&mut *tx).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO title_releases (title_id, release_id) SELECT {new_id}, id FROM `release` WHERE id IN ({list})"))).execute(&mut *tx).await?;
    for old in &olds {
        sqlx::query("INSERT IGNORE INTO title_merge_blocks (title_a, title_b) VALUES (?, ?)").bind((*old).min(new_id)).bind((*old).max(new_id)).execute(&mut *tx).await?;
        for hash in &hashes {
            sqlx::query("DELETE FROM title_aliases WHERE norm_hash = ? AND title_id = ?").bind(hash).bind(old).execute(&mut *tx).await?;
        }
        let moved: Vec<u64> = rows.iter().filter(|r| r.2 == Some(*old)).map(|r| r.0).collect();
        log_split(&mut tx, *old, new_id, &moved, None).await?;
    }
    tx.commit().await?;
    rename_canonical(pool, new_id).await?;
    for old in &olds {
        rename_canonical(pool, *old).await?;
    }
    // The log names the new title as it ended up.
    sqlx::query("UPDATE title_merge_log l JOIN title t ON t.id = l.merged_title_id SET l.merged_name = t.name, l.merged_norm_hash = t.norm_hash WHERE l.merged_title_id = ? AND l.reason = ?")
        .bind(new_id)
        .bind(SPLIT)
        .execute(pool)
        .await?;
    Ok(new_id)
}

/// `title_merge_log.reason` of a split: `survivor_id` is the title the releases came out of,
/// `merged_title_id` the title they went to, the archive lists them. Shown in Edit > Combined
/// titles beside the merges, and undone there ([`decouple`] puts them back).
pub const SPLIT: &str = "split";

/// Logs that `releases` left `from` for the new title `to`. `at` backdates it (rebuilt logs).
async fn log_split(tx: &mut sqlx::Transaction<'_, sqlx::MySql>, from: u64, to: u64, releases: &[u64], at: Option<chrono::NaiveDateTime>) -> Result<(), DatabaseError> {
    if releases.is_empty() {
        return Ok(());
    }
    let log_id = sqlx::query(
        "INSERT INTO title_merge_log (survivor_id, merged_title_id, merged_name, merged_norm_hash, reason, grade, merged_at) \
         SELECT ?, id, name, norm_hash, ?, 'manual', COALESCE(?, NOW()) FROM title WHERE id = ?",
    )
    .bind(from)
    .bind(SPLIT)
    .bind(at)
    .bind(to)
    .execute(&mut **tx)
    .await?
    .last_insert_id();
    let json = serde_json::json!({ "title_releases": releases.iter().map(|id| serde_json::json!({ "release_id": id })).collect::<Vec<_>>() }).to_string();
    sqlx::query(
        "INSERT INTO title_merge_archive (log_id, title_id, name, norm_name, norm_hash, category, basegame_title_id, rows_json) \
         SELECT ?, id, name, norm_name, norm_hash, category, basegame_title_id, ? FROM title WHERE id = ?",
    )
    .bind(log_id)
    .bind(json)
    .bind(to)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Undoes a split ([`SPLIT`]): its releases go back into the title they came out of (or what
/// that was merged into since), a title they leave empty is dropped, and the two may merge
/// again. Returns the title they are in now.
async fn undo_split(pool: &MySqlPool, log_id: u64, survivor: u64) -> Result<u64, DatabaseError> {
    let Some(target) = live_title(pool, survivor).await? else {
        return Err(DatabaseError::Config("the title it was split out of no longer exists".to_string()));
    };
    let json: Option<String> = sqlx::query_scalar("SELECT rows_json FROM title_merge_archive WHERE log_id = ?").bind(log_id).fetch_optional(pool).await?;
    let rows: serde_json::Value = json.as_deref().and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default();
    let ids: Vec<u64> = rows.get("title_releases").and_then(|v| v.as_array()).into_iter().flatten().filter_map(|r| r.get("release_id").and_then(|x| x.as_u64())).collect();
    if ids.is_empty() {
        return Err(DatabaseError::Config(format!("split {log_id} lists no releases")));
    }
    let away: Vec<(u64, u64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT tr.release_id, tr.title_id FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE WHERE tr.release_id IN ({}) AND tr.title_id <> ?",
        in_list(&ids)
    )))
    .bind(target)
    .fetch_all(pool)
    .await?;
    if away.is_empty() {
        return Err(DatabaseError::Config("those releases are back in that title already".to_string()));
    }
    let moving: Vec<u64> = away.iter().map(|(r, _)| *r).collect::<BTreeSet<_>>().into_iter().collect();
    let left: Vec<u64> = away.iter().map(|(_, t)| *t).collect::<BTreeSet<_>>().into_iter().collect();
    let mut tx = pool.begin().await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE IGNORE title_releases SET title_id = ? WHERE release_id IN ({}) AND title_id IN ({})", in_list(&moving), in_list(&left)))).bind(target).execute(&mut *tx).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM title_releases WHERE release_id IN ({}) AND title_id IN ({})", in_list(&moving), in_list(&left)))).execute(&mut *tx).await?;
    for t in &left {
        sqlx::query("DELETE FROM title_merge_blocks WHERE title_a = ? AND title_b = ?").bind((*t).min(target)).bind((*t).max(target)).execute(&mut *tx).await?;
    }
    sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE title t SET t.deleted = TRUE WHERE t.id IN ({}) AND NOT EXISTS (SELECT 1 FROM title_releases tr WHERE tr.title_id = t.id)", in_list(&left)))).execute(&mut *tx).await?;
    sqlx::query("UPDATE title_merge_log SET undone_at = NOW(), restored_title_id = ? WHERE id = ?").bind(target).bind(log_id).execute(&mut *tx).await?;
    tx.commit().await?;
    rename_canonical(pool, target).await?;
    for t in &left {
        if sqlx::query_scalar::<_, u64>("SELECT id FROM title WHERE id = ? AND deleted = FALSE").bind(t).fetch_optional(pool).await?.is_some() {
            rename_canonical(pool, *t).await?;
        }
    }
    log::info!("title split {log_id} undone: {} release(s) back in title {target}", moving.len());
    Ok(target)
}

/// Rebuilds the log of splits made from Edit › Merge review before splits were logged: each
/// accepted "another game inside a title" suggestion names the releases it split off, and the
/// titles they are in now (other than the one they came out of) are what it made. Once; not
/// on a client, whose log comes from its server.
pub async fn log_past_splits_once(pool: &MySqlPool) -> Result<u64, DatabaseError> {
    const NAME: &str = "split_log_backfill";
    if sqlx::query_scalar::<_, String>("SELECT name FROM data_migrations WHERE name = ?").bind(NAME).fetch_optional(pool).await?.is_some() {
        return Ok(0);
    }
    let mut logged = 0u64;
    if super::settings::instance_mode(pool).await? != super::settings::InstanceMode::Client {
        let taken = sqlx::query("SELECT title_ids, CAST(detail AS CHAR) AS detail, updated_at FROM title_merge_suggestions WHERE kind = ? AND status = 'accepted' AND detail IS NOT NULL ORDER BY updated_at")
            .bind(super::title_review::KIND_SPLIT_NAMES)
            .fetch_all(pool)
            .await?;
        for row in taken {
            let Some(from) = row.try_get::<String, _>("title_ids")?.split(',').find_map(|x| x.trim().parse::<u64>().ok()) else { continue };
            let Some(detail) = row.try_get::<Option<String>, _>("detail")?.and_then(|d| serde_json::from_str::<super::title_review::ReviewDetail>(&d).ok()) else { continue };
            let at: Option<chrono::NaiveDateTime> = row.try_get("updated_at")?;
            let home = live_title(pool, from).await?;
            for group in detail.options.first().map(|o| &o.groups[..]).unwrap_or(&[]).iter().skip(1).filter(|g| !g.is_empty()) {
                let found: Vec<(u64, u64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                    "SELECT tr.title_id, tr.release_id FROM title_releases tr JOIN title t ON t.id = tr.title_id AND t.deleted = FALSE WHERE tr.release_id IN ({})",
                    in_list(group)
                )))
                .fetch_all(pool)
                .await?;
                let mut by_title: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
                for (t, r) in found.into_iter().filter(|(t, _)| Some(*t) != home) {
                    by_title.entry(t).or_default().push(r);
                }
                for (to, releases) in by_title {
                    let have: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title_merge_log WHERE reason = ? AND merged_title_id = ? AND survivor_id = ?").bind(SPLIT).bind(to).bind(from).fetch_one(pool).await?;
                    if have > 0 {
                        continue;
                    }
                    let mut tx = pool.begin().await?;
                    log_split(&mut tx, from, to, &releases, at).await?;
                    tx.commit().await?;
                    logged += 1;
                }
            }
        }
        if logged > 0 {
            log::info!("{logged} earlier split(s) added to the merge log");
        }
    }
    sqlx::query("INSERT IGNORE INTO data_migrations (name) VALUES (?)").bind(NAME).execute(pool).await?;
    Ok(logged)
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DedupeSummary {
    /// IGDB games more than one title was linked to.
    pub shared_ids: u64,
    /// Merges done (each folds 2+ titles into one).
    pub merges: u64,
    /// Titles folded away.
    pub titles_merged: u64,
    /// Groups left for the user (name-link or cross-platform), pending in the backlog.
    pub suggestions: u64,
    /// Merged titles renamed to their (changed) canonical name.
    pub renamed: u64,
}

/// SQL for `title_review::unlicensed` on `release.name`: No-Intro's "(Unl)", "(Pirate)",
/// "(Aftermarket)", TOSEC's "(PD)" and "[p]" (MariaDB's REGEXP, case-insensitive by collation).
pub const UNLICENSED_SQL: &str = r"'\\((Unl|Pirate|Aftermarket|PD)\\)|\\[p[0-9]*\\]'";

/// Those of `titles` whose every game release is pirate, unlicensed or public domain. Such a
/// title never merges with one holding a retail release, automatically or as a suggestion: no
/// publisher sold it, so it is no retail game's other name -- a shared IGDB or
/// RetroAchievements game between the two is a wrong link (a pirate of the same name is under
/// the retail title from the import already).
pub async fn unlicensed_only(pool: &MySqlPool, titles: &[u64]) -> Result<HashSet<u64>, DatabaseError> {
    let mut out = HashSet::new();
    for chunk in titles.chunks(2000) {
        let sql = format!(
            "SELECT tr.title_id FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE AND r.material IS NULL \
             WHERE tr.title_id IN ({}) GROUP BY tr.title_id HAVING MIN(r.name REGEXP {UNLICENSED_SQL}) = 1",
            in_list(chunk)
        );
        out.extend(sqlx::query_scalar::<_, u64>(sqlx::AssertSqlSafe(sql)).fetch_all(pool).await?);
    }
    Ok(out)
}

/// `group` as the merges it may make: its retail titles together, its unlicensed-only ones
/// ([`unlicensed_only`]) together, never one with the other.
pub fn by_licence(group: &[u64], unlicensed: &HashSet<u64>) -> Vec<Vec<u64>> {
    let (unl, retail): (Vec<u64>, Vec<u64>) = group.iter().partition(|t| unlicensed.contains(t));
    [retail, unl].into_iter().filter(|g| !g.is_empty()).collect()
}

/// Groups of titles to merge: titles linked to the same game, connected through a shared
/// platform (a title on PS2 and PS4 and another only on PS4 all join).
pub fn merge_groups(titles: &[u64], platforms: &HashMap<u64, BTreeSet<u16>>) -> Vec<Vec<u64>> {
    let mut parent: BTreeMap<u64, u64> = titles.iter().map(|t| (*t, *t)).collect();
    fn find(parent: &mut BTreeMap<u64, u64>, x: u64) -> u64 {
        let p = parent[&x];
        if p == x {
            return x;
        }
        let root = find(parent, p);
        parent.insert(x, root);
        root
    }
    for (i, a) in titles.iter().enumerate() {
        for b in &titles[i + 1..] {
            let shared = match (platforms.get(a), platforms.get(b)) {
                (Some(pa), Some(pb)) => pa.intersection(pb).next().is_some(),
                _ => false,
            };
            if shared {
                let (ra, rb) = (find(&mut parent, *a), find(&mut parent, *b));
                if ra != rb {
                    parent.insert(ra.max(rb), ra.min(rb));
                }
            }
        }
    }
    let mut groups: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    for t in titles {
        let root = find(&mut parent, *t);
        groups.entry(root).or_default().push(*t);
    }
    groups.into_values().filter(|g| g.len() > 1).collect()
}

/// Renames every merged title whose canonical name changed (the naming rule changes, or a
/// release was added). Returns how many were renamed.
async fn recanonicalize_merged(pool: &MySqlPool, progress: &Progress, scope: &PassScope) -> Result<u64, DatabaseError> {
    let ids: Vec<u64> = match scope.since {
        // Survivors of merges since the last pass (this pass's included) and touched titles.
        Some(since) => sqlx::query_scalar("SELECT DISTINCT l.survivor_id FROM title_merge_log l JOIN title t ON t.id = l.survivor_id AND t.deleted = FALSE WHERE l.merged_at >= ? OR EXISTS (SELECT 1 FROM title_merge_dirty d WHERE d.title_id = l.survivor_id)").bind(since).fetch_all(pool).await?,
        None => sqlx::query_scalar("SELECT DISTINCT l.survivor_id FROM title_merge_log l JOIN title t ON t.id = l.survivor_id AND t.deleted = FALSE").fetch_all(pool).await?,
    };
    let mut renamed = 0;
    let total = ids.len() as u64;
    for (i, id) in ids.into_iter().enumerate() {
        progress.report(i as u64, Some(total), &format!("naming merged titles: {} of {total}", i + 1)).await;
        let current: Option<String> = sqlx::query_scalar("SELECT name FROM title WHERE id = ?").bind(id).fetch_optional(pool).await?;
        if let Some(name) = canonical_name(pool, id).await? {
            if current.as_deref() != Some(name.as_str()) {
                rename_canonical(pool, id).await?;
                renamed += 1;
            }
        }
    }
    Ok(renamed)
}

/// Title pairs the user decoupled (`title_merge_blocks`), as (lower, higher).
async fn merge_blocks(pool: &MySqlPool) -> Result<HashSet<(u64, u64)>, DatabaseError> {
    let rows: Vec<(u64, u64)> = sqlx::query_as("SELECT title_a, title_b FROM title_merge_blocks").fetch_all(pool).await?;
    Ok(rows.into_iter().collect())
}

/// `group` minus every title blocked against one kept before it (lowest ids first).
pub fn without_blocked(group: &[u64], blocks: &HashSet<(u64, u64)>) -> Vec<u64> {
    let mut kept: Vec<u64> = Vec::new();
    for &id in group {
        if kept.iter().all(|&k| !blocks.contains(&(k.min(id), k.max(id)))) {
            kept.push(id);
        }
    }
    kept
}

/// Follows merges forward from `title_id` to the live title that has its releases now.
pub(crate) async fn live_title(pool: &MySqlPool, title_id: u64) -> Result<Option<u64>, DatabaseError> {
    let mut id = title_id;
    for _ in 0..32 {
        let live: Option<u64> = sqlx::query_scalar("SELECT id FROM title WHERE id = ? AND deleted = FALSE").bind(id).fetch_optional(pool).await?;
        if live.is_some() {
            return Ok(Some(id));
        }
        let next: Option<u64> = sqlx::query_scalar("SELECT survivor_id FROM title_merge_log WHERE merged_title_id = ? AND undone_at IS NULL AND reason <> 'split' ORDER BY id DESC LIMIT 1").bind(id).fetch_optional(pool).await?;
        match next {
            Some(n) if n != id => id = n,
            _ => return Ok(None),
        }
    }
    Ok(None)
}

/// One merge, for Edit > Combined titles.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MergeLogRow {
    pub id: u64,
    /// The title it was merged into; `current_title_id` is where its releases are now (the
    /// survivor, or whatever that was merged into later).
    pub survivor_id: u64,
    pub current_title_id: Option<u64>,
    pub current_title_name: Option<String>,
    pub merged_title_id: u64,
    pub merged_name: String,
    pub reason: String,
    /// manual | hash | name ([`GRADES`]).
    pub grade: String,
    pub merged_at: chrono::NaiveDateTime,
    /// Releases it had (None: merged before titles were archived; decoupling then matches
    /// release names).
    pub release_count: Option<u64>,
    pub undone_at: Option<chrono::NaiveDateTime>,
    pub restored_title_id: Option<u64>,
}

/// The merge log, newest first. `search` matches the merged or the surviving title's name;
/// `survivor` limits it to merges into one title, `grade` to one of [`GRADES`].
pub async fn merge_log(pool: &MySqlPool, search: Option<&str>, survivor: Option<u64>, grade: Option<&str>, include_undone: bool, limit: u32, offset: u32) -> Result<(Vec<MergeLogRow>, i64), DatabaseError> {
    let mut cond = String::from(" WHERE 1=1");
    if !include_undone {
        cond.push_str(" AND l.undone_at IS NULL");
    }
    // One of three literals, checked here, so it goes into the SQL as it is.
    if let Some(g) = grade.filter(|g| GRADES.contains(g)) {
        cond.push_str(&format!(" AND l.grade = '{g}'"));
    }
    if survivor.is_some() {
        // A title's own splits too: what was split out of it, and what it was split out of.
        cond.push_str(" AND (l.survivor_id = ? OR (l.reason = 'split' AND l.merged_title_id = ?))");
    }
    let like = search.map(str::trim).filter(|x| !x.is_empty()).map(|x| format!("%{}%", x.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")));
    if like.is_some() {
        cond.push_str(" AND (l.merged_name LIKE ? OR l.survivor_id IN (SELECT id FROM title WHERE name LIKE ?))");
    }
    let binds: Vec<String> = survivor.into_iter().flat_map(|id| [id.to_string(), id.to_string()]).chain(like.iter().flat_map(|l| [l.clone(), l.clone()])).collect();
    let mut count = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM title_merge_log l{cond}")));
    for b in &binds {
        count = count.bind(b.clone());
    }
    let total = count.fetch_one(pool).await?;
    let mut query = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT l.id, l.survivor_id, l.merged_title_id, l.merged_name, l.reason, l.grade, l.merged_at, CAST(JSON_LENGTH(a.rows_json, '$.title_releases') AS UNSIGNED) AS release_count, l.undone_at, l.restored_title_id \
         FROM title_merge_log l LEFT JOIN title_merge_archive a ON a.log_id = l.id{cond} ORDER BY l.id DESC LIMIT {limit} OFFSET {offset}"
    )));
    for b in &binds {
        query = query.bind(b.clone());
    }
    let rows = query.fetch_all(pool).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let survivor_id: u64 = r.try_get("survivor_id")?;
        let current = live_title(pool, survivor_id).await?;
        let current_title_name: Option<String> = match current {
            Some(id) => sqlx::query_scalar("SELECT name FROM title WHERE id = ?").bind(id).fetch_optional(pool).await?,
            None => None,
        };
        out.push(MergeLogRow {
            id: r.try_get("id")?,
            survivor_id,
            current_title_id: current,
            current_title_name,
            merged_title_id: r.try_get("merged_title_id")?,
            merged_name: r.try_get("merged_name")?,
            reason: r.try_get("reason")?,
            grade: r.try_get("grade")?,
            merged_at: r.try_get("merged_at")?,
            release_count: r.try_get("release_count")?,
            undone_at: r.try_get("undone_at")?,
            restored_title_id: r.try_get("restored_title_id")?,
        });
    }
    Ok((out, total))
}

/// Splits a merged title back out. Archived merges come back exactly: the soft-deleted title
/// row revives under its old id, name and category, with its own releases, localized / alternative names, genres,
/// companies, media, keywords and provider links (the survivor keeps its copies of the
/// metadata; only the releases move). Older merges get a new title under the old name with
/// the releases whose name gives that name. Either way the name stops being an alias of the
/// survivor and the pair is blocked from automatic re-merging. Returns the title id. A logged
/// split ([`SPLIT`]) is undone instead: its releases go back ([`undo_split`]).
pub async fn decouple(pool: &MySqlPool, log_id: u64) -> Result<u64, DatabaseError> {
    let row = sqlx::query("SELECT survivor_id, merged_title_id, merged_name, merged_norm_hash, reason, undone_at FROM title_merge_log WHERE id = ?")
        .bind(log_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| DatabaseError::Config(format!("no merge {log_id}")))?;
    if row.try_get::<Option<chrono::NaiveDateTime>, _>("undone_at")?.is_some() {
        return Err(DatabaseError::Config("that merge was already decoupled".to_string()));
    }
    let survivor: u64 = row.try_get("survivor_id")?;
    if row.try_get::<String, _>("reason")? == SPLIT {
        return undo_split(pool, log_id, survivor).await;
    }
    let merged_title_id: u64 = row.try_get("merged_title_id")?;
    let mut name: String = row.try_get("merged_name")?;
    let norm_hash: String = row.try_get("merged_norm_hash")?;
    let Some(current) = live_title(pool, survivor).await? else {
        return Err(DatabaseError::Config("the title it was merged into no longer exists".to_string()));
    };
    let owned: Vec<(u64, String)> = sqlx::query_as("SELECT r.id, r.name FROM title_releases tr JOIN `release` r ON r.id = tr.release_id WHERE tr.title_id = ?").bind(current).fetch_all(pool).await?;
    let archive = sqlx::query("SELECT name, norm_name, category, basegame_title_id, rows_json FROM title_merge_archive WHERE log_id = ?").bind(log_id).fetch_optional(pool).await?;
    let rows: serde_json::Map<String, serde_json::Value> = match &archive {
        Some(a) => serde_json::from_str(&a.try_get::<String, _>("rows_json")?).map_err(|e| DatabaseError::Config(format!("merge {log_id}: bad archive: {e}")))?,
        None => serde_json::Map::new(),
    };
    let take: Vec<u64> = if archive.is_some() {
        let ids: HashSet<u64> = rows.get("title_releases").and_then(|v| v.as_array()).into_iter().flatten().filter_map(|r| r.get("release_id").and_then(|x| x.as_u64())).collect();
        owned.iter().filter(|(id, _)| ids.contains(id)).map(|(id, _)| *id).collect()
    } else {
        owned.iter().filter(|(_, n)| sha1_hex(&matching::normalize_title(&base_title(n)).to_lowercase()) == norm_hash).map(|(id, _)| *id).collect()
    };
    if take.is_empty() {
        return Err(DatabaseError::Config(format!("none of {name:?}'s releases are on title {current} any more")));
    }
    if take.len() == owned.len() {
        return Err(DatabaseError::Config(format!("{name:?} has all of title {current}'s releases; nothing would be left")));
    }
    let (norm_name, category, basegame): (String, String, Option<u64>) = match &archive {
        Some(a) => {
            name = a.try_get("name")?;
            (a.try_get("norm_name")?, a.try_get("category")?, a.try_get("basegame_title_id")?)
        }
        None => (matching::normalize_title(&name).to_lowercase(), sqlx::query_scalar("SELECT CAST(category AS CHAR) FROM title WHERE id = ?").bind(current).fetch_one(pool).await?, None),
    };
    // The base game may be gone (merged) since; follow it.
    let basegame = match basegame {
        Some(b) => live_title(pool, b).await?.filter(|b| *b != current),
        None => None,
    };
    // Merges soft-delete, so the old row is normally still there: it comes back to life under
    // its id. Merges from before that (hard-deleted) get a new title.
    let soft_deleted = sqlx::query_scalar::<_, u64>("SELECT id FROM title WHERE id = ? AND deleted = TRUE").bind(merged_title_id).fetch_optional(pool).await?.is_some();
    let mut tx = pool.begin().await?;
    let new_id = if soft_deleted {
        sqlx::query("UPDATE title SET name = ?, norm_name = ?, norm_hash = ?, category = ?, basegame_title_id = ?, deleted = FALSE WHERE id = ?")
            .bind(&name)
            .bind(norm_name.chars().take(512).collect::<String>())
            .bind(&norm_hash)
            .bind(&category)
            .bind(basegame)
            .bind(merged_title_id)
            .execute(&mut *tx)
            .await?;
        merged_title_id
    } else {
        sqlx::query("INSERT INTO title (name, norm_name, norm_hash, category, basegame_title_id) VALUES (?, ?, ?, ?, ?)")
            .bind(&name)
            .bind(norm_name.chars().take(512).collect::<String>())
            .bind(&norm_hash)
            .bind(&category)
            .bind(basegame)
            .execute(&mut *tx)
            .await?
            .last_insert_id()
    };
    sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE title_releases SET title_id = {new_id} WHERE title_id = {current} AND release_id IN ({})", in_list(&take)))).execute(&mut *tx).await?;
    // Its own metadata, as archived (releases are done above: only the ones still there).
    for (table, cols) in archived_tables() {
        if table == "title_releases" {
            continue;
        }
        for r in rows.get(table).and_then(|v| v.as_array()).into_iter().flatten() {
            let sql = format!("INSERT IGNORE INTO {table} (title_id, {}) VALUES (?{})", cols.join(", "), ", ?".repeat(cols.len()));
            let mut q = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(new_id);
            for c in &cols {
                q = match r.get(*c) {
                    Some(serde_json::Value::Number(n)) if n.is_u64() => q.bind(n.as_u64()),
                    Some(serde_json::Value::Number(n)) if n.is_i64() => q.bind(n.as_i64()),
                    Some(serde_json::Value::Number(n)) => q.bind(n.as_f64()),
                    Some(serde_json::Value::Bool(b)) => q.bind(*b),
                    Some(serde_json::Value::String(s)) => q.bind(s.clone()),
                    _ => q.bind(None::<String>),
                };
            }
            q.execute(&mut *tx).await?;
        }
    }
    sqlx::query("DELETE FROM title_aliases WHERE norm_hash = ? AND title_id = ?").bind(&norm_hash).bind(current).execute(&mut *tx).await?;
    sqlx::query("INSERT IGNORE INTO title_merge_blocks (title_a, title_b) VALUES (?, ?)").bind(current.min(new_id)).bind(current.max(new_id)).execute(&mut *tx).await?;
    sqlx::query("UPDATE title_merge_log SET undone_at = NOW(), restored_title_id = ? WHERE id = ?").bind(new_id).bind(log_id).execute(&mut *tx).await?;
    // Titles merged into the one being split out go with it.
    sqlx::query("UPDATE title_merge_log SET survivor_id = ? WHERE survivor_id = ? AND undone_at IS NULL").bind(new_id).bind(merged_title_id).execute(&mut *tx).await?;
    tx.commit().await?;
    rename_canonical(pool, current).await?;
    rename_canonical(pool, new_id).await?;
    log::info!("title merge {log_id} decoupled: {name:?} is title {new_id} again ({} release(s) from title {current}{})", take.len(), if archive.is_some() { ", metadata restored" } else { ", no archive" });
    Ok(new_id)
}

/// What [`decouple_grade`] did.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DecoupleSummary {
    pub decoupled: u64,
    /// Could not be split out (its releases are gone, or it has all of them); left merged.
    pub failed: u64,
    /// Merges of the grade still standing after this call, the failed ones included.
    pub remaining: u64,
}

/// Decouples the merges of one grade, newest first, at most `limit` a call -- the way back
/// from a batch of name-grade merges that turned out wrong. Each is a [`decouple`], so the
/// pairs are blocked from merging automatically again.
pub async fn decouple_grade(pool: &MySqlPool, grade: &str, limit: u32) -> Result<DecoupleSummary, DatabaseError> {
    if !GRADES.contains(&grade) {
        return Err(DatabaseError::Config(format!("grade {grade:?} must be one of {GRADES:?}")));
    }
    // Splits are not merges: "decouple all" leaves them.
    let ids: Vec<u64> = sqlx::query_scalar("SELECT id FROM title_merge_log WHERE grade = ? AND reason <> ? AND undone_at IS NULL ORDER BY id DESC LIMIT ?").bind(grade).bind(SPLIT).bind(limit).fetch_all(pool).await?;
    let mut s = DecoupleSummary::default();
    for id in ids {
        match decouple(pool, id).await {
            Ok(_) => s.decoupled += 1,
            Err(e) => {
                log::info!("title merge {id}: not decoupled: {e}");
                s.failed += 1;
            }
        }
    }
    s.remaining = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM title_merge_log WHERE grade = ? AND reason <> ? AND undone_at IS NULL").bind(grade).bind(SPLIT).fetch_one(pool).await? as u64;
    Ok(s)
}

/// The grade of merging `group` under provider game `key`: `name` when any of the links is a
/// name match, `manual` when the user made every one, `hash` otherwise. `reason` is the
/// group's source, `igdb` (title links) or `ra` (release links).
async fn group_grade(pool: &MySqlPool, reason: &str, key: &str, group: &[u64]) -> Result<&'static str, DatabaseError> {
    let from = if reason == "ra" {
        format!("release_provider_links l JOIN title_releases tr ON tr.release_id = l.release_id WHERE l.provider = 'retroachievements' AND l.confidence IN ('exact', 'manual') AND tr.title_id IN ({})", in_list(group))
    } else {
        format!("title_provider_links l WHERE l.provider = 'igdb' AND l.confidence IN ({MERGE_CONFIDENCES}) AND l.title_id IN ({})", in_list(group))
    };
    let sql = format!("SELECT CAST(COALESCE(MAX(l.confidence = 'name'), 0) AS SIGNED) AS any_name, CAST(COALESCE(MIN(l.confidence = 'manual'), 0) AS SIGNED) AS all_manual FROM {from} AND l.deleted = FALSE AND l.provider_id = ?");
    let row = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(key).fetch_one(pool).await?;
    Ok(if row.try_get::<i64, _>("any_name")? != 0 {
        "name"
    } else if row.try_get::<i64, _>("all_manual")? != 0 {
        "manual"
    } else {
        "hash"
    })
}

/// (key, title) pairs grouped by key: groups of 2+ live titles. Pairs rather than
/// GROUP_CONCAT, which MariaDB cuts at 1024 characters.
async fn shared_groups(pool: &MySqlPool, sql: &str, only: Option<&str>) -> Result<BTreeMap<String, Vec<u64>>, DatabaseError> {
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.to_string()));
    if let Some(id) = only {
        query = query.bind(id.to_string());
    }
    let mut groups: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
    for row in query.fetch_all(pool).await? {
        groups.entry(row.try_get("k")?).or_default().insert(row.try_get("title_id")?);
    }
    Ok(groups.into_iter().filter(|(_, t)| t.len() > 1).map(|(k, t)| (k, t.into_iter().collect())).collect())
}

/// Merges each group's titles that share a platform (`merge_groups`); `reason` prefixes the
/// log, `what` names the groups in the progress message.
async fn merge_shared(pool: &MySqlPool, groups: BTreeMap<String, Vec<u64>>, reason: &str, what: &str, s: &mut DedupeSummary, progress: &Progress) -> Result<(), DatabaseError> {
    let blocks = merge_blocks(pool).await?;
    let total = groups.len() as u64;
    for (i, (key, titles)) in groups.into_iter().enumerate() {
        progress.report(i as u64, Some(total), &format!("{what}: {} of {total} ({} merged)", i + 1, s.titles_merged)).await;
        // Titles merged earlier in this pass are gone.
        let live: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT id FROM title WHERE id IN ({}) AND deleted = FALSE", in_list(&titles)))).fetch_all(pool).await?;
        if live.len() < 2 {
            continue;
        }
        let sql = format!("SELECT tr.title_id, r.app_platform_id FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE WHERE tr.title_id IN ({}) GROUP BY tr.title_id, r.app_platform_id", in_list(&live));
        let mut platforms: HashMap<u64, BTreeSet<u16>> = HashMap::new();
        for r in sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(pool).await? {
            platforms.entry(r.try_get("title_id")?).or_default().insert(merge_platform(r.try_get("app_platform_id")?));
        }
        let unlicensed = unlicensed_only(pool, &live).await?;
        for group in merge_groups(&live, &platforms).into_iter().flat_map(|g| by_licence(&without_blocked(&g, &blocks), &unlicensed)) {
            if group.len() < 2 {
                continue;
            }
            let grade = group_grade(pool, reason, &key, &group).await?;
            merge_titles(pool, &group, &format!("{reason}:{key}"), grade).await?;
            s.merges += 1;
            s.titles_merged += group.len() as u64 - 1;
        }
    }
    Ok(())
}

/// Whether any title is linked to IGDB at all -- without that, [`dedupe_by_igdb`] has no ids
/// to group by and the merge pass after a hash match would be an empty run.
pub async fn has_igdb_links(pool: &MySqlPool) -> Result<bool, DatabaseError> {
    let exists: i64 = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM title_provider_links WHERE provider = 'igdb' AND deleted = FALSE)").fetch_one(pool).await?;
    Ok(exists != 0)
}

/// The automatic pass. Titles on a common platform are one game when
/// - they share an IGDB id at a confidence in [`MERGE_CONFIDENCES`], or
/// - RetroAchievements' hash list puts releases of each under the same RA game (an exact
///   link: RA hashes that exact disc / ROM for that game).
///
/// Neither needs a file on disk: the links are between catalogue rows. `only` limits it to
/// one IGDB game (after a manual link). The backlog is refreshed after. `progress` gets one
/// scale for the whole pass, each step a slice of it.
pub async fn dedupe_by_igdb(pool: &MySqlPool, only: Option<&str>, progress: &Progress) -> Result<DedupeSummary, DatabaseError> {
    dedupe_by_igdb_in(pool, only, progress, &PassScope::full()).await
}

/// [`dedupe_by_igdb`] over what `scope` holds: groups with a touched title, the merged
/// titles touched, the suggestions those groups make.
pub async fn dedupe_by_igdb_in(pool: &MySqlPool, only: Option<&str>, progress: &Progress, scope: &PassScope) -> Result<DedupeSummary, DatabaseError> {
    const WHOLE: u64 = 100;
    let step = |base: u64, span: u64| progress.set_inner_frame(Some(Frame { base, span, whole: WHOLE }));
    let mut s = DedupeSummary::default();
    step(0, 45);
    progress.force(0, None, "finding titles that share an IGDB game").await;
    let mut sql = format!(
        "SELECT DISTINCT tpl.provider_id AS k, tpl.title_id FROM title_provider_links tpl JOIN title t ON t.id = tpl.title_id AND t.deleted = FALSE \
         WHERE tpl.provider = 'igdb' AND tpl.deleted = FALSE AND tpl.confidence IN ({MERGE_CONFIDENCES})"
    );
    if only.is_some() {
        sql.push_str(" AND tpl.provider_id = ?");
    }
    let igdb: BTreeMap<String, Vec<u64>> = shared_groups(pool, &sql, only).await?.into_iter().filter(|(_, titles)| scope.touches(titles)).collect();
    s.shared_ids = igdb.len() as u64;
    merge_shared(pool, igdb, "igdb", "IGDB games shared by several titles", &mut s, progress).await?;
    if only.is_none() {
        step(45, 20);
        progress.force(0, None, "finding titles that share a RetroAchievements game").await;
        let ra = shared_groups(
            pool,
            "SELECT DISTINCT rpl.provider_id AS k, tr.title_id FROM release_provider_links rpl JOIN `release` r ON r.id = rpl.release_id AND r.deleted = FALSE \
             JOIN title_releases tr ON tr.release_id = r.id JOIN title t ON t.id = tr.title_id AND t.deleted = FALSE \
             WHERE rpl.provider = 'retroachievements' AND rpl.deleted = FALSE AND rpl.confidence IN ('exact', 'manual')",
            None,
        )
        .await?
        .into_iter()
        .filter(|(_, titles)| scope.touches(titles))
        .collect();
        merge_shared(pool, ra, "ra", "RetroAchievements games shared by several titles", &mut s, progress).await?;
        step(65, 20);
        s.renamed = recanonicalize_merged(pool, progress, scope).await?;
    }
    step(85, 10);
    s.suggestions = refresh_suggestions(pool, only, progress, scope).await?;
    if only.is_none() {
        step(95, 5);
        progress.force(0, None, "suggestions: Japanese names without a link").await;
        s.suggestions += suggest_by_name(pool, scope).await?;
        // The review queue's kinds; the first pass after they came in looks at everything.
        const REVIEW_ONCE: &str = "title_review_0068";
        let first = sqlx::query_scalar::<_, String>("SELECT name FROM data_migrations WHERE name = ?").bind(REVIEW_ONCE).fetch_optional(pool).await?.is_none();
        let review_scope = if first { PassScope::full() } else { scope.clone() };
        s.suggestions += super::title_review::suggest(pool, &review_scope, progress).await?;
        if first {
            sqlx::query("INSERT IGNORE INTO data_migrations (name) VALUES (?)").bind(REVIEW_ONCE).execute(pool).await?;
        }
    }
    progress.set_inner_frame(None);
    if s.merges > 0 {
        log::info!("title dedupe: {} merge(s), {} title(s) folded", s.merges, s.titles_merged);
    }
    Ok(s)
}

/// The IGDB-linked title `jp_name` is probably a Japanese name of: it ends with that title's
/// normalized name after one or two leading words ("shin legend of hogehoge ii" -> "legend of hogehoge ii").
pub fn suffix_match<'a>(jp_name: &str, known: &'a HashMap<String, u64>) -> Option<(&'a String, u64)> {
    let words: Vec<&str> = jp_name.split_whitespace().collect();
    (1..=2.min(words.len().saturating_sub(1))).find_map(|k| {
        let suffix = words[k..].join(" ");
        (suffix.len() >= 4).then(|| known.get_key_value(&suffix).map(|(n, id)| (n, *id))).flatten()
    })
}

/// Suggestions (not merges) for titles no hash links to anything: a Japan-only title without
/// an IGDB link whose name ends with an IGDB-linked title's name on the same platform
/// ("Shin Legend of Hogehoge II" / "Legend of Hogehoge II"). Keyed `name:<title id>`, reason `name_suffix`.
async fn suggest_by_name(pool: &MySqlPool, scope: &PassScope) -> Result<u64, DatabaseError> {
    let mut known: HashMap<u16, HashMap<String, u64>> = HashMap::new();
    for r in sqlx::query(
        "SELECT DISTINCT r.app_platform_id, t.id, t.norm_name FROM title t JOIN title_provider_links tpl ON tpl.title_id = t.id AND tpl.provider = 'igdb' AND tpl.deleted = FALSE \
         AND tpl.confidence IN ('exact', 'hash', 'manual') JOIN title_releases tr ON tr.title_id = t.id JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE WHERE t.deleted = FALSE",
    )
    .fetch_all(pool)
    .await?
    {
        known.entry(r.try_get("app_platform_id")?).or_default().insert(r.try_get("norm_name")?, r.try_get("id")?);
    }
    let candidates = sqlx::query(
        "SELECT t.id, t.norm_name, MIN(r.app_platform_id) AS platform, CAST(SUM(COALESCE(r.region, '') <> 'JP') AS SIGNED) AS non_jp, COUNT(DISTINCT r.app_platform_id) AS platforms \
         FROM title t JOIN title_releases tr ON tr.title_id = t.id JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE \
         WHERE t.deleted = FALSE AND NOT EXISTS (SELECT 1 FROM title_provider_links x WHERE x.title_id = t.id AND x.provider = 'igdb' AND x.deleted = FALSE) \
         GROUP BY t.id, t.norm_name HAVING non_jp = 0 AND platforms = 1",
    )
    .fetch_all(pool)
    .await?;
    let ids: Vec<u64> = candidates.iter().filter_map(|c| c.try_get::<u64, _>("id").ok()).chain(known.values().flat_map(|k| k.values().copied())).collect::<BTreeSet<_>>().into_iter().collect();
    let unlicensed = unlicensed_only(pool, &ids).await?;
    let mut groups: BTreeMap<u64, BTreeSet<u64>> = BTreeMap::new();
    for c in candidates {
        // Only titles touched since the last pass (the others were looked at then).
        if !scope.has(c.try_get("id")?) {
            continue;
        }
        let platform: u16 = c.try_get("platform")?;
        let name: String = c.try_get("norm_name")?;
        if let Some((_, target)) = known.get(&platform).and_then(|k| suffix_match(&name, k)) {
            let id: u64 = c.try_get("id")?;
            if unlicensed.contains(&id) != unlicensed.contains(&target) {
                continue;
            }
            groups.entry(target).or_default().insert(id);
        }
    }
    for (target, jp) in &groups {
        let ids: Vec<u64> = std::iter::once(*target).chain(jp.iter().copied()).collect::<BTreeSet<u64>>().into_iter().collect();
        sqlx::query(
            "INSERT INTO title_merge_suggestions (igdb_id, title_ids, reason) VALUES (?, ?, 'name_suffix') ON DUPLICATE KEY UPDATE \
             status = IF(title_ids = VALUES(title_ids), status, 'pending'), updated_at = IF(title_ids = VALUES(title_ids), updated_at, NOW()), title_ids = VALUES(title_ids)",
        )
        .bind(format!("name:{target}"))
        .bind(in_list(&ids))
        .execute(pool)
        .await?;
    }
    Ok(groups.len() as u64)
}

/// The platforms each of `titles` has a game release on (its DLC and other bundled material
/// do not count: a 3DS add-on's DAT names no platform at all).
/// The platform a merge compares: one console sold under two regional names is one. Some DATs
/// file Japanese games under the Super Famicom and the rest under the SNES, and the same game
/// stayed two titles because they shared no platform.
pub(crate) fn merge_platform(id: u16) -> u16 {
    use crate::common::models::Platform;
    match Platform::from_igdb_id(id) {
        Some(Platform::Famicom) => Platform::Nes.igdb_id(),
        Some(Platform::SuperFamicom) => Platform::Snes.igdb_id(),
        Some(Platform::NeoGeoAes) => Platform::NeoGeoMvs.igdb_id(),
        _ => id,
    }
}

/// Whether two titles have a game platform in common (Famicom and NES counting as one, as
/// `merge_platform` has it): what every automatic merge needs. The same game on two platforms
/// is two titles, one row on the cross-platform list.
pub async fn share_platform(pool: &MySqlPool, a: u64, b: u64) -> Result<bool, DatabaseError> {
    let platforms = game_platforms(pool, &[a, b]).await?;
    Ok(matches!((platforms.get(&a), platforms.get(&b)), (Some(x), Some(y)) if !x.is_disjoint(y)))
}

/// The game platforms on the two sides of a merge still standing: the merged title's releases
/// as the merge archived them, and the survivor's others. None when either side has no game
/// release left to judge by.
pub async fn merge_side_platforms(pool: &MySqlPool, log_id: u64) -> Result<Option<(BTreeSet<u16>, BTreeSet<u16>)>, DatabaseError> {
    let survivor: Option<u64> = sqlx::query_scalar("SELECT survivor_id FROM title_merge_log WHERE id = ? AND undone_at IS NULL").bind(log_id).fetch_optional(pool).await?;
    let Some(survivor) = survivor else { return Ok(None) };
    let merged: Vec<(u64, u16)> = sqlx::query_as(
        "SELECT r.id, r.app_platform_id FROM title_merge_archive a, JSON_TABLE(a.rows_json, '$.title_releases[*]' COLUMNS (release_id BIGINT UNSIGNED PATH '$.release_id')) j \
         JOIN `release` r ON r.id = j.release_id AND r.deleted = FALSE AND r.material IS NULL WHERE a.log_id = ?",
    )
    .bind(log_id)
    .fetch_all(pool)
    .await?;
    let theirs: BTreeSet<u64> = merged.iter().map(|(id, _)| *id).collect();
    let ours: Vec<(u64, u16)> = sqlx::query_as("SELECT r.id, r.app_platform_id FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE AND r.material IS NULL WHERE tr.title_id = ?")
        .bind(survivor)
        .fetch_all(pool)
        .await?;
    let merged_side: BTreeSet<u16> = merged.iter().map(|(_, p)| merge_platform(*p)).collect();
    let survivor_side: BTreeSet<u16> = ours.iter().filter(|(id, _)| !theirs.contains(id)).map(|(_, p)| merge_platform(*p)).collect();
    Ok((!merged_side.is_empty() && !survivor_side.is_empty()).then_some((merged_side, survivor_side)))
}

/// Whether merge `log_id` (still standing) joined a title of nothing but pirate or unlicensed
/// game releases, as the merge archived them, to a survivor whose other game releases include
/// a retail one -- what [`unlicensed_only`] keeps apart now.
pub async fn merge_joined_unlicensed(pool: &MySqlPool, log_id: u64) -> Result<bool, DatabaseError> {
    let survivor: Option<u64> = sqlx::query_scalar("SELECT survivor_id FROM title_merge_log WHERE id = ? AND undone_at IS NULL").bind(log_id).fetch_optional(pool).await?;
    let Some(survivor) = survivor else { return Ok(false) };
    let Some(survivor) = live_title(pool, survivor).await? else { return Ok(false) };
    let merged: Vec<(u64, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT r.id, CAST(r.name REGEXP {UNLICENSED_SQL} AS SIGNED) FROM title_merge_archive a, JSON_TABLE(a.rows_json, '$.title_releases[*]' COLUMNS (release_id BIGINT UNSIGNED PATH '$.release_id')) j \
         JOIN `release` r ON r.id = j.release_id AND r.deleted = FALSE AND r.material IS NULL WHERE a.log_id = ?"
    )))
    .bind(log_id)
    .fetch_all(pool)
    .await?;
    if merged.is_empty() || merged.iter().any(|(_, unl)| *unl == 0) {
        return Ok(false);
    }
    let theirs: Vec<u64> = merged.iter().map(|(id, _)| *id).collect();
    let retail: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT COUNT(*) FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE AND r.material IS NULL \
         WHERE tr.title_id = ? AND r.id NOT IN ({}) AND NOT (r.name REGEXP {UNLICENSED_SQL})",
        in_list(&theirs)
    )))
    .bind(survivor)
    .fetch_one(pool)
    .await?;
    Ok(retail > 0)
}

pub(crate) async fn game_platforms(pool: &MySqlPool, titles: &[u64]) -> Result<HashMap<u64, BTreeSet<u16>>, DatabaseError> {
    let mut platforms: HashMap<u64, BTreeSet<u16>> = HashMap::new();
    for chunk in titles.chunks(2000) {
        let sql = format!(
            "SELECT tr.title_id, r.app_platform_id FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE AND r.material IS NULL \
             WHERE tr.title_id IN ({}) GROUP BY tr.title_id, r.app_platform_id",
            in_list(chunk)
        );
        for r in sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(pool).await? {
            platforms.entry(r.try_get("title_id")?).or_default().insert(merge_platform(r.try_get("app_platform_id")?));
        }
    }
    Ok(platforms)
}

/// Rebuilds the backlog from what's still shared after the automatic merges: every IGDB game
/// linked to 2+ titles (any confidence) *that have a platform in common*. The same game on
/// PS4 and on Xbox One is two titles on purpose -- emulators and RetroAchievements treat them
/// as different sets, and the cross-platform list is where they are one row -- so that is not
/// something to suggest, however many times the pass runs. A suggestion the user dismissed
/// stays dismissed unless its set of titles changes; one whose titles no longer share the
/// game, or a platform, is dropped.
async fn refresh_suggestions(pool: &MySqlPool, only: Option<&str>, progress: &Progress, scope: &PassScope) -> Result<u64, DatabaseError> {
    progress.force(0, None, "suggestions: titles still sharing an IGDB game").await;
    let mut sql = format!(
        "SELECT tpl.provider_id, GROUP_CONCAT(DISTINCT tpl.title_id ORDER BY tpl.title_id) AS titles, \
         CAST(MIN(tpl.confidence IN ({MERGE_CONFIDENCES})) AS SIGNED) AS all_strong \
         FROM title_provider_links tpl JOIN title t ON t.id = tpl.title_id AND t.deleted = FALSE WHERE tpl.provider = 'igdb' AND tpl.deleted = FALSE"
    );
    if only.is_some() {
        sql.push_str(" AND tpl.provider_id = ?");
    }
    sql.push_str(" GROUP BY tpl.provider_id HAVING COUNT(DISTINCT tpl.title_id) > 1");
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    if let Some(id) = only {
        query = query.bind(id.to_string());
    }
    let rows = query.fetch_all(pool).await?;
    let mut shared: Vec<(String, Vec<u64>, bool)> = Vec::with_capacity(rows.len());
    for row in &rows {
        let titles: Vec<u64> = row.try_get::<String, _>("titles")?.split(',').filter_map(|x| x.parse().ok()).collect();
        shared.push((row.try_get("provider_id")?, titles, row.try_get::<i64, _>("all_strong")? != 0));
    }
    // A group nothing touched keeps whatever suggestion it has: it is not looked at again,
    // and not dropped as stale either.
    let mut live: Vec<String> = Vec::with_capacity(shared.len());
    let (shared, untouched): (Vec<_>, Vec<_>) = shared.into_iter().partition(|(_, titles, _)| scope.touches(titles));
    live.extend(untouched.into_iter().map(|(id, _, _)| id));
    let every: Vec<u64> = shared.iter().flat_map(|(_, titles, _)| titles.iter().copied()).collect::<BTreeSet<u64>>().into_iter().collect();
    let platforms = game_platforms(pool, &every).await?;
    let unlicensed = unlicensed_only(pool, &every).await?;
    let total = shared.len() as u64;
    for (i, (igdb_id, titles, all_strong)) in shared.into_iter().enumerate() {
        progress.report(i as u64, Some(total), &format!("suggestions: {} of {total}", i + 1)).await;
        // Only the titles a merge would actually join: those sharing a platform with another,
        // retail with retail (or else unlicensed with unlicensed), never across.
        let groups: Vec<Vec<u64>> = merge_groups(&titles, &platforms).into_iter().flat_map(|g| by_licence(&g, &unlicensed)).filter(|g| g.len() > 1).collect();
        let retail: Vec<u64> = groups.iter().filter(|g| !unlicensed.contains(&g[0])).flatten().copied().collect();
        let mut mergeable: Vec<u64> = if retail.is_empty() { groups.into_iter().flatten().collect() } else { retail };
        if mergeable.is_empty() {
            continue;
        }
        mergeable.sort_unstable();
        let titles = in_list(&mergeable);
        // name_link: some link is only a search's fuzzy guess; review: every link is one the
        // pass merges on, but the titles were decoupled.
        let reason = if all_strong { "review" } else { "name_link" };
        // status/updated_at are computed before title_ids is overwritten (left to right).
        sqlx::query(
            "INSERT INTO title_merge_suggestions (igdb_id, title_ids, reason) VALUES (?, ?, ?) ON DUPLICATE KEY UPDATE \
             status = IF(title_ids = VALUES(title_ids), status, 'pending'), updated_at = IF(title_ids = VALUES(title_ids), updated_at, NOW()), \
             reason = VALUES(reason), title_ids = VALUES(title_ids)",
        )
        .bind(&igdb_id)
        .bind(titles.chars().take(1024).collect::<String>())
        .bind(reason)
        .execute(pool)
        .await?;
        live.push(igdb_id);
    }
    // Drop pending suggestions that no longer apply (merged elsewhere, links removed).
    match only {
        Some(id) if live.is_empty() => {
            sqlx::query("DELETE FROM title_merge_suggestions WHERE igdb_id = ? AND status = 'pending'").bind(id).execute(pool).await?;
        }
        Some(_) => {}
        None => {
            // Only this generator's own (the review kinds clean up after themselves).
            let stale = sqlx::query("SELECT igdb_id FROM title_merge_suggestions WHERE status = 'pending' AND kind = 'igdb'").fetch_all(pool).await?;
            let live_set: std::collections::HashSet<&str> = live.iter().map(String::as_str).collect();
            for r in stale {
                let id: String = r.try_get("igdb_id")?;
                if !id.starts_with("name:") && !live_set.contains(id.as_str()) {
                    sqlx::query("DELETE FROM title_merge_suggestions WHERE igdb_id = ? AND status = 'pending'").bind(&id).execute(pool).await?;
                }
            }
        }
    }
    Ok(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM title_merge_suggestions WHERE status = 'pending'").fetch_one(pool).await? as u64)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Suggestion {
    pub igdb_id: String,
    pub reason: String,
    pub status: String,
    pub titles: Vec<SuggestedTitle>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SuggestedTitle {
    pub id: u64,
    pub name: String,
    pub platforms: Option<String>,
    pub confidence: Option<String>,
}

/// The backlog, newest first.
pub async fn suggestions(pool: &MySqlPool, status: &str, limit: u32, offset: u32) -> Result<(Vec<Suggestion>, i64), DatabaseError> {
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title_merge_suggestions WHERE status = ?").bind(status).fetch_one(pool).await?;
    let rows = sqlx::query("SELECT igdb_id, title_ids, reason, status FROM title_merge_suggestions WHERE status = ? ORDER BY updated_at DESC, igdb_id LIMIT ? OFFSET ?").bind(status).bind(limit).bind(offset).fetch_all(pool).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let igdb_id: String = r.try_get("igdb_id")?;
        let ids: Vec<u64> = r.try_get::<String, _>("title_ids")?.split(',').filter_map(|x| x.parse().ok()).collect();
        let sql = format!(
            "SELECT t.id, t.name, (SELECT GROUP_CONCAT(DISTINCT p.name ORDER BY p.name SEPARATOR ', ') FROM title_releases tr JOIN `release` r ON r.id = tr.release_id JOIN platforms p ON p.id = r.app_platform_id WHERE tr.title_id = t.id) AS platforms, \
             (SELECT tpl.confidence FROM title_provider_links tpl WHERE tpl.title_id = t.id AND tpl.provider = 'igdb' AND tpl.deleted = FALSE) AS confidence FROM title t WHERE t.id IN ({}) ORDER BY t.id",
            in_list(&ids)
        );
        let titles = sqlx::query(sqlx::AssertSqlSafe(sql))
            .fetch_all(pool)
            .await?
            .iter()
            .map(|t| Ok(SuggestedTitle { id: t.try_get("id")?, name: t.try_get("name")?, platforms: t.try_get("platforms")?, confidence: t.try_get("confidence")? }))
            .collect::<Result<Vec<_>, sqlx::Error>>()?;
        out.push(Suggestion { igdb_id, reason: r.try_get("reason")?, status: r.try_get("status")?, titles });
    }
    Ok((out, total))
}

/// Accept (merge the suggestion's titles) or dismiss one backlog entry.
pub async fn resolve_suggestion(pool: &MySqlPool, igdb_id: &str, accept: bool) -> Result<Option<u64>, DatabaseError> {
    let row = sqlx::query("SELECT title_ids FROM title_merge_suggestions WHERE igdb_id = ?").bind(igdb_id).fetch_optional(pool).await?.ok_or_else(|| DatabaseError::Config(format!("no suggestion for IGDB game {igdb_id}")))?;
    let survivor = if accept {
        let ids: Vec<u64> = row.try_get::<String, _>("title_ids")?.split(',').filter_map(|x| x.parse().ok()).collect();
        let live: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT id FROM title WHERE id IN ({}) AND deleted = FALSE ORDER BY id", in_list(&ids)))).fetch_all(pool).await?;
        // Platform by platform, as the suggestion was made: one game's PS4 titles and its Xbox
        // One titles are two merges, not one. A name suggestion (`name:<title>`) is one group.
        let groups = if igdb_id.starts_with("name:") { vec![live] } else { merge_groups(&live, &game_platforms(pool, &live).await?) };
        let mut survivor = None;
        for group in groups {
            let kept = merge_titles(pool, &group, &format!("suggestion:igdb:{igdb_id}"), "manual").await?;
            survivor.get_or_insert(kept);
        }
        Some(survivor.ok_or_else(|| DatabaseError::Config(format!("the titles of IGDB game {igdb_id} no longer share a platform: nothing to merge")))?)
    } else {
        None
    };
    sqlx::query("UPDATE title_merge_suggestions SET status = ?, updated_at = NOW() WHERE igdb_id = ?").bind(if accept { "accepted" } else { "dismissed" }).bind(igdb_id).execute(pool).await?;
    Ok(survivor)
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct BulkResolved {
    /// Suggestions merged or dismissed.
    pub resolved: u64,
    /// The ones that could not be merged, with why (their titles are gone, or no longer
    /// share a platform). They stay as they were.
    pub failed: Vec<String>,
}

/// [`resolve_suggestion`] for many at once: the given suggestions, or with `ids` `None` every
/// one of `status` (the backlog as a whole). One that cannot be merged does not stop the
/// rest. Dismissing is one statement however many there are; merging is a merge each.
pub async fn resolve_suggestions(pool: &MySqlPool, ids: Option<&[String]>, status: &str, accept: bool) -> Result<BulkResolved, DatabaseError> {
    let ids: Vec<String> = match ids {
        Some(ids) => ids.to_vec(),
        None => sqlx::query_scalar("SELECT igdb_id FROM title_merge_suggestions WHERE status = ? ORDER BY updated_at DESC, igdb_id").bind(status).fetch_all(pool).await?,
    };
    let mut out = BulkResolved::default();
    if !accept {
        for chunk in ids.chunks(1000) {
            let sql = format!("UPDATE title_merge_suggestions SET status = 'dismissed', updated_at = NOW() WHERE status = ? AND igdb_id IN ({})", vec!["?"; chunk.len()].join(","));
            let mut query = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(status);
            for id in chunk {
                query = query.bind(id);
            }
            out.resolved += query.execute(pool).await?.rows_affected();
        }
        return Ok(out);
    }
    for id in &ids {
        match resolve_suggestion(pool, id, true).await {
            Ok(_) => out.resolved += 1,
            Err(e) => out.failed.push(format!("{id}: {e}")),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_follows_priority_then_retail() {
        let order: Vec<String> = crate::database::settings::DEFAULT_CANONICAL_NAME_PRIORITY.iter().map(|s| s.to_string()).collect();
        let r = |n: &str, reg: &str, lang: &str, secondary: bool| CanonCandidate { name: n.to_string(), region: (!reg.is_empty()).then(|| reg.to_string()), languages: (!lang.is_empty()).then(|| lang.to_string()), rank: if secondary { 12 } else { 1 }, special: false };
        let set = vec![r("Shin Legend of Hogehoge V (Japan)", "JP", "", false), r("Legend of Hogehoge V (Europe) (En,Fr,De,Es,It)", "EU", "en,fr,de,es,it", false), r("Legend of Hogehoge V (USA)", "US", "", false), r("Legend of Hogehoge V (USA) (Demo)", "US", "", true)];
        assert_eq!(pick_canonical(&set, &order), Some("Legend of Hogehoge V (USA)"));
        let jp_eu = vec![r("Dark Hoge 2 (Japan)", "JP", "", false), r("Hoge Chronicle (Europe)", "EU", "", false)];
        assert_eq!(pick_canonical(&jp_eu, &order), Some("Hoge Chronicle (Europe)"));
        let multi = vec![r("Game (Japan)", "JP", "", false), r("Game (USA, Europe)", "US,EU", "", false)];
        assert_eq!(pick_canonical(&multi, &order), Some("Game (USA, Europe)"));
        let lang_only = vec![r("Shin Legend of Hogehoge II (Japan)", "JP", "ja", false), r("Shin Legend of Hogehoge II (Asia) (En)", "ASIA", "en", false)];
        assert_eq!(pick_canonical(&lang_only, &order), Some("Shin Legend of Hogehoge II (Asia) (En)"), "Asia (and English) before Japan");
        let jp_only_en = vec![r("Game (Japan)", "JP", "ja", false), r("Game (Korea) (En)", "KR", "en", false), r("Game (Korea)", "KR", "ko", false)];
        assert_eq!(pick_canonical(&jp_only_en, &order), Some("Game (Korea) (En)"), "English before Japan");
        assert_eq!(pick_canonical(&jp_only_en[..1], &order), Some("Game (Japan)"));
        let custom: Vec<String> = vec!["JPN".into()];
        assert_eq!(pick_canonical(&set, &custom), Some("Shin Legend of Hogehoge V (Japan)"), "the setting decides");
        let patched = vec![r("Shin Legend of Hogehoge II (Japan) (Translated En)", "US", "en", true), r("Shin Legend of Hogehoge II (Japan)", "JP", "ja", false)];
        assert_eq!(pick_canonical(&patched, &order), Some("Shin Legend of Hogehoge II (Japan)"), "commercial before patches");
        let mut editions = vec![r("Game (Japan) (Shokai Genteiban)", "JP", "", false), r("Game - Long Subtitle (Japan) (v1.02)", "JP", "", false)];
        editions[0].special = true;
        assert_eq!(pick_canonical(&editions, &order), Some("Game - Long Subtitle (Japan) (v1.02)"), "a regular edition names the title before a special one");
        let mut aftermarket = vec![r("Game (USA) (Aftermarket) (Unl)", "US", "", false), r("Game (Japan)", "JP", "", false)];
        aftermarket[0].rank = 3;
        assert_eq!(pick_canonical(&aftermarket, &order), Some("Game (Japan)"), "retail anywhere before aftermarket");
    }

    #[test]
    fn blocked_titles_stay_apart() {
        let blocks: HashSet<(u64, u64)> = [(1, 3)].into_iter().collect();
        assert_eq!(without_blocked(&[1, 2, 3], &blocks), vec![1, 2]);
        assert_eq!(without_blocked(&[2, 3], &blocks), vec![2, 3]);
    }

    #[test]
    fn japanese_names_match_by_suffix() {
        let known: HashMap<String, u64> = [("legend of hogehoge ii".to_string(), 7), ("legend of hogehoge".to_string(), 8)].into_iter().collect();
        assert_eq!(suffix_match("shin legend of hogehoge ii", &known).map(|(_, id)| id), Some(7));
        assert_eq!(suffix_match("genso legend of hogehoge", &known).map(|(_, id)| id), Some(8));
        assert_eq!(suffix_match("legend of hogehoge ii", &known), None, "the whole name isn't a suffix match");
        assert_eq!(suffix_match("a b c legend of hogehoge", &known), None, "at most two leading words");
    }

    #[test]
    fn unlicensed_titles_merge_only_with_their_own() {
        let unl: HashSet<u64> = [3, 4].into_iter().collect();
        assert_eq!(by_licence(&[1, 2, 3, 4], &unl), vec![vec![1, 2], vec![3, 4]]);
        assert_eq!(by_licence(&[1, 3], &unl), vec![vec![1], vec![3]]);
        assert_eq!(by_licence(&[1, 2], &unl), vec![vec![1, 2]]);
    }

    #[test]
    fn groups_need_a_shared_platform() {
        let p = |ids: &[u16]| ids.iter().copied().collect::<BTreeSet<u16>>();
        let platforms: HashMap<u64, BTreeSet<u16>> = [(1, p(&[8])), (2, p(&[8])), (3, p(&[48])), (4, p(&[48, 8]))].into_iter().collect();
        assert_eq!(merge_groups(&[1, 2, 3], &platforms), vec![vec![1, 2]], "PS2 US + JP merge, the PS4 port stays");
        assert_eq!(merge_groups(&[1, 3, 4], &platforms), vec![vec![1, 3, 4]], "joined through a title on both");
        assert!(merge_groups(&[1, 3], &platforms).is_empty());
    }
}
