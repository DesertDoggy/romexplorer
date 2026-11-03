//! Suggestions IGDB's grouping cannot give, for Edit › Merge review. IGDB is written from an
//! English-speaking side: a Japanese "International" re-release has a page of its own while the
//! Japanese original is filed with the foreign versions, a Director's Cut sits wherever IGDB
//! put it, a game sold under a very different Japanese name has no link to its foreign title,
//! a collection is a bundle on one side and a game on the other, and a Japanese release IGDB
//! has no page for ends up inside another game's title. None of that is fixed automatically:
//! each finder here writes a suggestion with the choices it sees (`ReviewOption`), and the
//! user picks one in the queue.
//!
//! The finders run at the end of the merge pass (`title_merge::dedupe_by_igdb_in`), each over
//! the titles the pass looked at. Their rows sit in `title_merge_suggestions` beside the
//! IGDB-game ones, told apart by `kind`; a decided one (accepted or dismissed) is never offered
//! again, and a pending one that no longer applies is dropped by the next full pass.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sqlx::mysql::MySqlPool;
use sqlx::Row;

use super::error::DatabaseError;
use super::jobs::Progress;
use super::matching::util::norm_key;
use super::matching::names_close;
use super::providers::cache::APART_SQL;
use super::title_merge::{base_title, game_platforms, merge_platform, PassScope};

pub const KIND_INTERNATIONAL: &str = "international";
pub const KIND_DIRECTORS_CUT: &str = "directors_cut";
pub const KIND_TRANSLATED_NAME: &str = "translated_name";
pub const KIND_BUNDLE: &str = "bundle_vs_game";
pub const KIND_SPLIT_NAMES: &str = "split_names";
/// Every kind written here (the IGDB-game suggestions are kind `igdb`).
pub const KINDS: [&str; 5] = [KIND_INTERNATIONAL, KIND_DIRECTORS_CUT, KIND_SPLIT_NAMES, KIND_TRANSLATED_NAME, KIND_BUNDLE];

/// How each kind ranks in the queue, higher first (`merge_review_order` = regional).
pub fn priority(kind: &str) -> i16 {
    match kind {
        KIND_INTERNATIONAL | KIND_DIRECTORS_CUT => 30,
        KIND_SPLIT_NAMES => 25,
        KIND_TRANSLATED_NAME => 20,
        KIND_BUNDLE => 15,
        _ => 0,
    }
}

/// One choice a review offers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewOption {
    /// What it does, as a key the UIs word: `with_foreign` (the variant with the foreign
    /// versions, the Japanese original apart), `alone` (the variant apart, the original with
    /// the foreign versions), `merge`, `split` (the odd releases into a title of their own).
    pub label: String,
    /// The finder thinks this is right.
    #[serde(default)]
    pub recommended: bool,
    /// The titles are like this already.
    #[serde(default)]
    pub current: bool,
    /// Whole titles to merge into one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merge: Vec<u64>,
    /// Releases as the titles they end up in, one list per title.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<Vec<u64>>,
}

/// What a suggestion's `detail` holds.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviewDetail {
    pub options: Vec<ReviewOption>,
    /// The variant's marker as named ("International", "Director's Cut").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    /// The names that matched, for the card ("Hoge Quest" ↔ "ほげクエスト").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matched: Vec<String>,
}

/// A release as the finders see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rel {
    pub id: u64,
    pub title: u64,
    pub name: String,
    pub region: String,
    pub platform: u16,
}

impl Rel {
    fn japanese(&self) -> bool {
        self.region.split(',').any(|r| r.trim().eq_ignore_ascii_case("JP"))
    }
}

/// A regional re-release IGDB tends to file apart from, or with, the wrong versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Variant {
    International,
    DirectorsCut,
}

impl Variant {
    pub fn kind(self) -> &'static str {
        match self {
            Self::International => KIND_INTERNATIONAL,
            Self::DirectorsCut => KIND_DIRECTORS_CUT,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::International => "International",
            Self::DirectorsCut => "Director's Cut",
        }
    }
}

/// Games a name match cannot pair across regions (`regional_titles.json`): a version sold in
/// Japan under one colour or name and abroad under another, a game renumbered abroad, and a
/// Japanese release with no counterpart abroad at all. Keys are `norm_key`s, platforms merged.
#[derive(Debug, Default)]
pub struct RegionalNames {
    /// (platform, Japanese name) -> the names abroad; empty: never released abroad.
    world_of: HashMap<(u16, String), Vec<String>>,
    /// (platform, a name abroad) -> the Japanese name.
    jp_of: HashMap<(u16, String), String>,
}

#[derive(Deserialize)]
struct RegionalFile {
    entries: Vec<RegionalEntry>,
}

#[derive(Deserialize)]
struct RegionalEntry {
    platform: u16,
    jp: String,
    #[serde(default)]
    world: Vec<String>,
}

impl RegionalNames {
    pub fn parse(json: &str) -> Self {
        let file: RegionalFile = serde_json::from_str(json).unwrap_or(RegionalFile { entries: Vec::new() });
        let mut out = Self::default();
        for e in file.entries {
            let platform = merge_platform(e.platform);
            let jp = norm_key(&e.jp);
            let world: Vec<String> = e.world.iter().map(|w| norm_key(w)).collect();
            for w in &world {
                out.jp_of.insert((platform, w.clone()), jp.clone());
            }
            out.world_of.insert((platform, jp), world);
        }
        out
    }

    /// The table this build ships.
    pub fn builtin() -> &'static Self {
        lazy_static::lazy_static! {
            static ref BUILTIN: RegionalNames = RegionalNames::parse(include_str!("regional_titles.json"));
        }
        &BUILTIN
    }

    /// Which game a name is on a platform, as the table knows it (its Japanese name), when it
    /// does: a Japanese release by its Japanese name, any other by a name abroad.
    pub fn game(&self, platform: u16, key: &str, japanese: bool) -> Option<&str> {
        let p = merge_platform(platform);
        if japanese {
            self.world_of.get_key_value(&(p, key.to_string())).map(|((_, k), _)| k.as_str())
        } else {
            self.jp_of.get(&(p, key.to_string())).map(String::as_str)
        }
    }

    /// A Japanese release never sold abroad.
    pub fn alone(&self, platform: u16, key: &str) -> bool {
        self.world_of.get(&(merge_platform(platform), key.to_string())).is_some_and(Vec::is_empty)
    }

    /// The (platform, Japanese name, name abroad) of every game the table pairs.
    fn pairs(&self) -> Vec<(u16, &str, &str)> {
        self.world_of.iter().flat_map(|((p, jp), world)| world.iter().map(move |w| (*p, jp.as_str(), w.as_str()))).collect()
    }
}

/// The variant a release name says it is, and the name without it as a comparison key:
/// "Hoge Quest VII International (Japan)" -> (International, "hoge quest 7").
pub fn variant_of(release_name: &str) -> Option<(Variant, String)> {
    lazy_static::lazy_static! {
        static ref INTERNATIONAL: regex::Regex = regex::Regex::new(r"(?i)[\s:\-]*\binternational(?:\s+(?:version|edition))?\b").unwrap();
        static ref DIRECTORS_CUT: regex::Regex = regex::Regex::new(r"(?i)[\s:\-]*\bdirector['’]?s\s+(?:cut|edition)\b").unwrap();
    }
    let (variant, re) = if DIRECTORS_CUT.is_match(release_name) {
        (Variant::DirectorsCut, &*DIRECTORS_CUT)
    } else if INTERNATIONAL.is_match(release_name) {
        (Variant::International, &*INTERNATIONAL)
    } else {
        return None;
    };
    // A name that starts with it is a game called so ("International Hoge Soccer").
    if re.find(release_name).is_some_and(|m| release_name[..m.start()].trim().is_empty()) {
        return None;
    }
    let key = norm_key(&base_title(&re.replace_all(release_name, "")));
    (!key.is_empty()).then_some((variant, key))
}

/// Whether `groups` already are titles: each group's releases under one title, a different
/// one per group.
fn in_shape(groups: &[Vec<u64>], title_of: &HashMap<u64, u64>) -> bool {
    let mut seen = HashSet::new();
    groups.iter().filter(|g| !g.is_empty()).all(|g| {
        let titles: HashSet<u64> = g.iter().filter_map(|r| title_of.get(r).copied()).collect();
        titles.len() == 1 && seen.insert(*titles.iter().next().unwrap())
    })
}

/// The choices for a variant (`variant`) and the other releases of its game on the platform
/// (`others`: same name without the marker). Needs a Japanese original and a foreign release
/// to choose between; None when there is nothing to decide -- an International already with
/// the foreign versions, the Japanese original apart.
pub fn plan_variant(kind: Variant, variant: &[Rel], others: &[Rel]) -> Option<ReviewDetail> {
    let jp: Vec<u64> = others.iter().filter(|r| r.japanese()).map(|r| r.id).collect();
    let foreign: Vec<u64> = others.iter().filter(|r| !r.japanese()).map(|r| r.id).collect();
    if jp.is_empty() || foreign.is_empty() || variant.is_empty() {
        return None;
    }
    let title_of: HashMap<u64, u64> = variant.iter().chain(others).map(|r| (r.id, r.title)).collect();
    let ids = |rs: &[Rel]| rs.iter().map(|r| r.id).collect::<Vec<u64>>();
    let with_foreign = vec![foreign.iter().copied().chain(ids(variant)).collect::<Vec<u64>>(), jp.clone()];
    let alone = vec![ids(variant), jp.iter().copied().chain(foreign.iter().copied()).collect()];
    let (with_now, alone_now) = (in_shape(&with_foreign, &title_of), in_shape(&alone, &title_of));
    // An International goes with the foreign versions (it is their version, sold in Japan).
    if kind == Variant::International && with_now {
        return None;
    }
    let recommended = kind == Variant::International;
    Some(ReviewDetail {
        options: vec![
            ReviewOption { label: "with_foreign".into(), recommended, current: with_now, groups: with_foreign, ..Default::default() },
            ReviewOption { label: "alone".into(), recommended: false, current: alone_now, groups: alone, ..Default::default() },
        ],
        variant: Some(kind.label().to_string()),
        matched: Vec::new(),
    })
}

/// The releases of one title, one platform and one region that name a different game than
/// the title does: "Piyo Monsters Ao" inside the title of "Piyo Monsters Midori" (IGDB has no
/// page for it). `names` are the title's own names (its name, alternative and localized ones).
/// None when every name is one of the title's, or none is (nothing to tell apart by).
///
/// `table` (`RegionalNames`) says what names cannot: a Japanese colour version that is a
/// foreign one of another colour belongs with it, and a Japanese release never sold abroad is
/// split off however the title's names read. Each game split off gets a title of its own.
pub fn plan_split(names: &[String], releases: &[Rel], table: &RegionalNames) -> Option<ReviewDetail> {
    // A release's game: the table's when it lists the name (`true`), else its own name.
    let game = |r: &Rel| -> (String, bool) {
        let key = norm_key(&r.name);
        match table.game(r.platform, &key, r.japanese()) {
            Some(g) => (g.to_string(), true),
            None => (key, false),
        }
    };
    let known = |platform: u16, id: &str, listed: bool| {
        names.iter().any(|n| {
            let k = norm_key(n);
            if listed {
                k == id || table.game(platform, &k, true) == Some(id) || table.game(platform, &k, false) == Some(id)
            } else {
                names_close(&k, id)
            }
        })
    };
    let alone = |r: &Rel| r.japanese() && table.alone(r.platform, &norm_key(&r.name));
    let mut buckets: BTreeMap<(u16, String), Vec<&Rel>> = BTreeMap::new();
    // An International or Director's Cut is the variants' finder's to ask about.
    for r in releases.iter().filter(|r| variant_of(&r.name).is_none() && !alone(r)) {
        buckets.entry((merge_platform(r.platform), r.region.to_ascii_uppercase())).or_default().push(r);
    }
    let mut odd: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    // A Japanese release never sold abroad is no other release's game.
    if releases.iter().any(|r| !alone(r)) {
        for r in releases.iter().filter(|r| alone(r)) {
            odd.entry(norm_key(&r.name)).or_default().push(r.id);
        }
    }
    for ((platform, _), rels) in &buckets {
        // The bucket's games, joined when close ("Hoge" and "Hoge - Special Edition"); a
        // name the table lists joins only its own game ("Aka" and "Ao" are close, not one).
        let mut clusters: Vec<(String, bool, Vec<u64>)> = Vec::new();
        for r in rels {
            let (id, listed) = game(r);
            match clusters.iter_mut().find(|(k, l, _)| *k == id || (!listed && !*l && (names_close(k, &id) || names_close(&id, k)))) {
                Some((_, _, ids)) => ids.push(r.id),
                None => clusters.push((id, listed, vec![r.id])),
            }
        }
        if clusters.len() < 2 {
            continue;
        }
        let (mine, theirs): (Vec<_>, Vec<_>) = clusters.into_iter().partition(|(k, l, _)| known(*platform, k, *l));
        if !mine.is_empty() {
            for (k, _, ids) in theirs {
                odd.entry(k).or_default().extend(ids);
            }
        }
    }
    if odd.is_empty() {
        return None;
    }
    let taken: BTreeSet<u64> = odd.values().flatten().copied().collect();
    let rest: Vec<u64> = releases.iter().map(|r| r.id).filter(|id| !taken.contains(id)).collect();
    let groups: Vec<Vec<u64>> = std::iter::once(rest).chain(odd.into_values()).collect();
    Some(ReviewDetail { options: vec![ReviewOption { label: "split".into(), recommended: true, groups, ..Default::default() }], variant: None, matched: Vec::new() })
}

/// A bundle's name as its game would be called: "Hoge Saga Collection" -> "Hoge Saga".
fn without_bundle_words(name: &str) -> String {
    lazy_static::lazy_static! {
        static ref WORDS: regex::Regex = regex::Regex::new(r"(?i)\b(collection|compilation|complete|trilogy|anthology)\b").unwrap();
    }
    WORDS.replace_all(&base_title(name), " ").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether a release name is the bundle itself: the bundle's own name, or one that says it is
/// a collection.
fn is_bundle_release(bundle_name: &str, release_name: &str) -> bool {
    let key = norm_key(release_name);
    key == norm_key(bundle_name) || norm_key(&without_bundle_words(release_name)) != key
}

/// The single game one of a bundle's releases is: that game's title and its releases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Counterpart {
    pub title: u64,
    pub name: String,
    pub releases: Vec<u64>,
}

/// The choice for a bundle title (IGDB filed separate games under a collection): each release
/// that is one single game goes to that game's title (`counterpart`), or a title of its own
/// when the game has none; the collection's own releases stay. Recommended, never applied by
/// itself. None when every release is the collection.
pub fn plan_bundle(bundle_name: &str, releases: &[Rel], counterpart: impl Fn(&Rel) -> Option<Counterpart>) -> Option<ReviewDetail> {
    let mut stay: Vec<u64> = Vec::new();
    let mut to_titles: BTreeMap<u64, (String, Vec<u64>, Vec<u64>)> = BTreeMap::new();
    let mut own: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    let mut matched = Vec::new();
    for r in releases {
        if is_bundle_release(bundle_name, &r.name) {
            stay.push(r.id);
            continue;
        }
        match counterpart(r) {
            Some(c) if c.title != r.title => {
                matched.push(format!("{} → {}", r.name, c.name));
                to_titles.entry(c.title).or_insert_with(|| (c.name.clone(), c.releases.clone(), Vec::new())).2.push(r.id);
            }
            _ => own.entry(norm_key(&r.name)).or_default().push(r.id),
        }
    }
    // A game with no title anywhere: a title of its own -- unless it is all the bundle holds.
    if to_titles.is_empty() && (own.is_empty() || (own.len() == 1 && stay.is_empty())) {
        return None;
    }
    let mut groups: Vec<Vec<u64>> = Vec::new();
    if !stay.is_empty() {
        groups.push(stay);
    }
    for (_, (_, theirs, moved)) in to_titles {
        groups.push(theirs.into_iter().chain(moved).collect());
    }
    groups.extend(own.into_values());
    Some(ReviewDetail { options: vec![ReviewOption { label: "split_bundle".into(), recommended: true, groups, ..Default::default() }], variant: None, matched })
}

/// The platforms merged as `merge_platform` has them, back to the ids releases carry.
fn raw_platforms(merged: u16) -> Vec<u16> {
    use crate::common::models::Platform;
    let mut out = vec![merged];
    for p in [Platform::Famicom, Platform::SuperFamicom, Platform::NeoGeoAes] {
        if merge_platform(p.igdb_id()) == merged && p.igdb_id() != merged {
            out.push(p.igdb_id());
        }
    }
    out
}

fn list<T: ToString>(ids: &[T]) -> String {
    ids.iter().map(T::to_string).collect::<Vec<_>>().join(",")
}

/// Writes one suggestion. A decided one (accepted, dismissed) stays decided whatever changed:
/// the user looked at it. A pending one takes the newest titles and choices.
async fn upsert(pool: &MySqlPool, key: &str, kind: &str, titles: &[u64], detail: &ReviewDetail) -> Result<(), DatabaseError> {
    let mut titles = titles.to_vec();
    titles.sort_unstable();
    titles.dedup();
    let detail = serde_json::to_string(detail).map_err(|e| DatabaseError::Config(e.to_string()))?;
    sqlx::query(
        "INSERT INTO title_merge_suggestions (igdb_id, title_ids, reason, kind, priority, detail) VALUES (?, ?, ?, ?, ?, ?) ON DUPLICATE KEY UPDATE \
         updated_at = IF(status = 'pending' AND (title_ids <> VALUES(title_ids) OR detail <> VALUES(detail)), NOW(), updated_at), \
         title_ids = IF(status = 'pending', VALUES(title_ids), title_ids), detail = IF(status = 'pending', VALUES(detail), detail), \
         kind = VALUES(kind), priority = VALUES(priority), reason = VALUES(reason)",
    )
    .bind(key)
    .bind(list(&titles).chars().take(1024).collect::<String>())
    .bind(kind)
    .bind(kind)
    .bind(priority(kind))
    .bind(detail)
    .execute(pool)
    .await?;
    Ok(())
}

fn rel_from(r: &sqlx::mysql::MySqlRow) -> Result<Rel, sqlx::Error> {
    Ok(Rel { id: r.try_get("id")?, title: r.try_get("title_id")?, name: r.try_get("name")?, region: r.try_get("region")?, platform: r.try_get("app_platform_id")? })
}

/// A game release of a live title (`r`, `tr`, `t` joined).
fn game_release_sql(extra: &str) -> String {
    format!(
        "SELECT r.id, r.name, COALESCE(r.region, '') AS region, r.app_platform_id, tr.title_id FROM `release` r JOIN title_releases tr ON tr.release_id = r.id \
         JOIN title t ON t.id = tr.title_id AND t.deleted = FALSE WHERE r.deleted = FALSE AND NOT {APART_SQL} {extra}"
    )
}

/// International / Director's Cut releases with a Japanese original and a foreign release of
/// the same game on the platform. Returns the keys written.
async fn variants(pool: &MySqlPool, scope: &PassScope) -> Result<Vec<String>, DatabaseError> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(game_release_sql("AND (r.name LIKE '%International%' OR r.name LIKE '%Director%')"))).fetch_all(pool).await?;
    let mut sets: BTreeMap<(u16, Variant, String), Vec<Rel>> = BTreeMap::new();
    for row in &rows {
        let rel = rel_from(row)?;
        let Some((variant, key)) = variant_of(&rel.name) else { continue };
        if rel.japanese() {
            sets.entry((merge_platform(rel.platform), variant, key)).or_default().push(rel);
        }
    }
    let mut written = Vec::new();
    for ((platform, variant, key), rels) in sets {
        let first = key.split_whitespace().next().unwrap_or("").replace(['%', '_', '\\'], "");
        if first.is_empty() {
            continue;
        }
        let sql = game_release_sql(&format!("AND r.app_platform_id IN ({}) AND r.norm_name LIKE ?", list(&raw_platforms(platform))));
        let others: Vec<Rel> = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(format!("{first}%"))
            .fetch_all(pool)
            .await?
            .iter()
            .map(rel_from)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|r| variant_of(&r.name).is_none() && names_close(&key, &norm_key(&r.name)))
            .collect();
        let titles: Vec<u64> = rels.iter().chain(&others).map(|r| r.title).collect::<BTreeSet<_>>().into_iter().collect();
        if !scope.touches(&titles) {
            continue;
        }
        let Some(detail) = plan_variant(variant, &rels, &others) else { continue };
        let id = format!("{}:{}", if variant == Variant::International { "intl" } else { "dc" }, rels.iter().map(|r| r.id).min().unwrap_or(0));
        upsert(pool, &id, variant.kind(), &titles, &detail).await?;
        written.push(id);
    }
    Ok(written)
}

/// The title pairs the user kept apart (`title_merge_blocks`), as (lower, higher).
async fn blocked_pairs(pool: &MySqlPool) -> Result<HashSet<(u64, u64)>, DatabaseError> {
    Ok(sqlx::query_as::<_, (u64, u64)>("SELECT title_a, title_b FROM title_merge_blocks").fetch_all(pool).await?.into_iter().collect())
}

/// Writes a merge suggestion per pair that shares a platform and was not kept apart.
async fn write_pairs(pool: &MySqlPool, kind: &str, prefix: &str, pairs: BTreeMap<(u64, u64), Vec<String>>, scope: &PassScope) -> Result<Vec<String>, DatabaseError> {
    let blocked = blocked_pairs(pool).await?;
    let ids: Vec<u64> = pairs.keys().flat_map(|(a, b)| [*a, *b]).collect::<BTreeSet<_>>().into_iter().collect();
    let platforms = game_platforms(pool, &ids).await?;
    let mut written = Vec::new();
    for ((a, b), matched) in pairs {
        if blocked.contains(&(a, b)) || !scope.touches(&[a, b]) {
            continue;
        }
        let shared = matches!((platforms.get(&a), platforms.get(&b)), (Some(x), Some(y)) if !x.is_disjoint(y));
        if !shared {
            continue;
        }
        let detail = ReviewDetail { options: vec![ReviewOption { label: "merge".into(), recommended: kind == KIND_TRANSLATED_NAME, merge: vec![a, b], ..Default::default() }], variant: None, matched };
        let id = format!("{prefix}:{a}-{b}");
        upsert(pool, &id, kind, &[a, b], &detail).await?;
        written.push(id);
    }
    Ok(written)
}

/// A Japanese-only title whose name, or one the name dictionary gives for it, is a name of an
/// IGDB-linked title of another game on the same platform -- its alternative or localized name
/// ("Hoge Emblem if" for "Hoge Emblem Fates"). Returns the keys written.
async fn translated_names(pool: &MySqlPool, scope: &PassScope) -> Result<Vec<String>, DatabaseError> {
    let mut igdb: HashMap<u64, String> = HashMap::new();
    let mut index: HashMap<String, Vec<u64>> = HashMap::new();
    let add = |index: &mut HashMap<String, Vec<u64>>, key: String, title: u64| {
        if key.chars().count() >= 3 {
            let at = index.entry(key).or_default();
            if !at.contains(&title) {
                at.push(title);
            }
        }
    };
    for r in sqlx::query("SELECT t.id, t.name, tpl.provider_id FROM title t JOIN title_provider_links tpl ON tpl.title_id = t.id AND tpl.provider = 'igdb' AND tpl.deleted = FALSE WHERE t.deleted = FALSE").fetch_all(pool).await? {
        let id: u64 = r.try_get("id")?;
        igdb.insert(id, r.try_get("provider_id")?);
        add(&mut index, norm_key(&r.try_get::<String, _>("name")?), id);
    }
    for table in ["title_alt_names", "title_locales"] {
        let sql = format!("SELECT x.title_id, x.name FROM {table} x JOIN title t ON t.id = x.title_id AND t.deleted = FALSE JOIN title_provider_links tpl ON tpl.title_id = x.title_id AND tpl.provider = 'igdb' AND tpl.deleted = FALSE");
        for r in sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(pool).await? {
            add(&mut index, norm_key(&r.try_get::<String, _>("name")?), r.try_get("title_id")?);
        }
    }
    let candidates = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT t.id, t.name FROM title t JOIN title_releases tr ON tr.title_id = t.id JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE \
         WHERE t.deleted = FALSE AND NOT {APART_SQL} GROUP BY t.id, t.name HAVING SUM(NOT FIND_IN_SET('JP', REPLACE(COALESCE(r.region, ''), ' ', ''))) = 0"
    )))
    .fetch_all(pool)
    .await?;
    let dict = super::dictionary::dict(pool).await?;
    let table = RegionalNames::builtin();
    let mut pairs: BTreeMap<(u64, u64), Vec<String>> = BTreeMap::new();
    for c in candidates {
        let id: u64 = c.try_get("id")?;
        let name: String = c.try_get("name")?;
        let own = norm_key(&name);
        // A Japanese release never sold abroad has no foreign title to join.
        if table.world_of.iter().any(|((_, k), w)| w.is_empty() && *k == own) {
            continue;
        }
        for alt in dict.equivalents(&name) {
            let Some(targets) = index.get(&norm_key(&alt)) else { continue };
            for &t in targets {
                // Another title, not of the same IGDB game (the pass merges those itself). Two
                // linked titles matching by their own names are two games of one name.
                if t == id || igdb.get(&id).is_some_and(|g| igdb.get(&t) == Some(g)) {
                    continue;
                }
                if norm_key(&alt) == own && igdb.contains_key(&id) {
                    continue;
                }
                let matched = pairs.entry((id.min(t), id.max(t))).or_default();
                if !matched.contains(&alt) {
                    matched.push(alt.clone());
                }
            }
        }
    }
    // The table's games: the Japanese release under one title, the foreign one under another.
    for (platform, jp, world) in table.pairs() {
        let jp_titles = titles_named(pool, platform, jp, true).await?;
        let world_titles = titles_named(pool, platform, world, false).await?;
        for &a in &jp_titles {
            for &b in world_titles.iter().filter(|b| **b != a && !jp_titles.contains(b)) {
                let matched = pairs.entry((a.min(b), a.max(b))).or_default();
                let note = format!("{jp} = {world}");
                if !matched.contains(&note) {
                    matched.push(note);
                }
            }
        }
    }
    write_pairs(pool, KIND_TRANSLATED_NAME, "tname", pairs, scope).await
}

/// The titles holding a game release named `key` on a platform: Japanese releases, or the rest.
async fn titles_named(pool: &MySqlPool, platform: u16, key: &str, japanese: bool) -> Result<Vec<u64>, DatabaseError> {
    let sql = format!(
        "SELECT DISTINCT tr.title_id FROM `release` r JOIN title_releases tr ON tr.release_id = r.id JOIN title t ON t.id = tr.title_id AND t.deleted = FALSE \
         WHERE r.deleted = FALSE AND NOT {APART_SQL} AND r.app_platform_id IN ({}) AND r.norm_hash = ? AND (FIND_IN_SET('JP', REPLACE(COALESCE(r.region, ''), ' ', '')) > 0) = ?",
        list(&raw_platforms(platform))
    );
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(sql)).bind(super::import::sha1_hex(key)).bind(japanese).fetch_all(pool).await?)
}

/// The single-game title a bundle's release is, on its platform: a non-bundle title named as
/// the release (the same name first, then a close one), with its own releases.
async fn counterpart_of(pool: &MySqlPool, bundle: u64, r: &Rel) -> Result<Option<Counterpart>, DatabaseError> {
    let key = norm_key(&r.name);
    let first = key.split_whitespace().next().unwrap_or("").replace(['%', '_', '\\'], "");
    if first.is_empty() {
        return Ok(None);
    }
    let sql = format!(
        "SELECT DISTINCT t.id, t.name FROM title t JOIN title_releases tr ON tr.title_id = t.id JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE \
         WHERE t.deleted = FALSE AND t.id <> ? AND t.category <> 'bundle' AND r.app_platform_id IN ({}) AND t.norm_name LIKE ? LIMIT 200",
        list(&raw_platforms(merge_platform(r.platform)))
    );
    let candidates: Vec<(u64, String)> = sqlx::query_as(sqlx::AssertSqlSafe(sql)).bind(bundle).bind(format!("{first}%")).fetch_all(pool).await?;
    let single = |n: &str| norm_key(&without_bundle_words(n)) == norm_key(n);
    let mut best = candidates.iter().filter(|(_, n)| single(n)).find(|(_, n)| norm_key(n) == key).or_else(|| candidates.iter().filter(|(_, n)| single(n)).find(|(_, n)| names_close(&key, &norm_key(n)))).cloned();
    // Else by a title's alternative or localized name: a Japanese release named as the game
    // was sold there, its title under the English name.
    if best.is_none() {
        let sql = format!(
            "SELECT DISTINCT t.id, t.name, x.name AS alt FROM title t JOIN (SELECT title_id, name FROM title_alt_names UNION ALL SELECT title_id, name FROM title_locales) x ON x.title_id = t.id \
             JOIN title_releases tr ON tr.title_id = t.id JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE \
             WHERE t.deleted = FALSE AND t.id <> ? AND t.category <> 'bundle' AND r.app_platform_id IN ({}) AND x.name LIKE ? LIMIT 200",
            list(&raw_platforms(merge_platform(r.platform)))
        );
        let by_alt: Vec<(u64, String, String)> = sqlx::query_as(sqlx::AssertSqlSafe(sql)).bind(bundle).bind(format!("{first}%")).fetch_all(pool).await?;
        best = by_alt.iter().filter(|(_, n, _)| single(n)).find(|(_, _, a)| norm_key(a) == key).or_else(|| by_alt.iter().filter(|(_, n, _)| single(n)).find(|(_, _, a)| names_close(&key, &norm_key(a)))).map(|(t, n, _)| (*t, n.clone()));
    }
    let Some((title, name)) = best else { return Ok(None) };
    let releases: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT r.id FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE WHERE tr.title_id = ? AND NOT {APART_SQL}"
    )))
    .bind(title)
    .fetch_all(pool)
    .await?;
    Ok(Some(Counterpart { title, name, releases }))
}

/// A bundle (IGDB's category, or a "Collection" / "Compilation" name) holding releases that
/// are single games: the suggestion to give each its game's title. Returns the keys written.
async fn bundles(pool: &MySqlPool, scope: &PassScope) -> Result<Vec<String>, DatabaseError> {
    let rows: Vec<(u64, String)> = sqlx::query_as("SELECT t.id, t.name FROM title t WHERE t.deleted = FALSE AND (t.category = 'bundle' OR t.name REGEXP '(^|[^a-z])(collection|compilation)([^a-z]|$)')").fetch_all(pool).await?;
    let mut written = Vec::new();
    for (bundle, name) in rows {
        let releases: Vec<Rel> = sqlx::query(sqlx::AssertSqlSafe(game_release_sql("AND tr.title_id = ?"))).bind(bundle).fetch_all(pool).await?.iter().map(rel_from).collect::<Result<_, _>>()?;
        if releases.iter().all(|r| is_bundle_release(&name, &r.name)) {
            continue;
        }
        let mut found: HashMap<u64, Option<Counterpart>> = HashMap::new();
        for r in releases.iter().filter(|r| !is_bundle_release(&name, &r.name)) {
            found.insert(r.id, counterpart_of(pool, bundle, r).await?);
        }
        let Some(detail) = plan_bundle(&name, &releases, |r| found.get(&r.id).cloned().flatten()) else { continue };
        let titles: Vec<u64> = std::iter::once(bundle).chain(found.values().flatten().map(|c| c.title)).collect();
        if !scope.touches(&titles) {
            continue;
        }
        let id = format!("bundle:{bundle}");
        upsert(pool, &id, KIND_BUNDLE, &titles, &detail).await?;
        written.push(id);
    }
    Ok(written)
}

/// Titles holding, for one platform and region, releases of differently named games, one of
/// them not a name of the title's. Returns the keys written.
async fn split_names(pool: &MySqlPool, scope: &PassScope) -> Result<Vec<String>, DatabaseError> {
    let titles: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT DISTINCT x.title_id FROM (SELECT tr.title_id FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE \
           JOIN title t ON t.id = tr.title_id AND t.deleted = FALSE WHERE NOT {APART_SQL} AND COALESCE(r.region, '') <> '' \
           GROUP BY tr.title_id, r.app_platform_id, r.region HAVING COUNT(DISTINCT r.norm_name) > 1) x"
    )))
    .fetch_all(pool)
    .await?;
    // And every title holding a Japanese release the table says was never sold abroad, beside
    // anything else.
    let alone: Vec<String> = RegionalNames::builtin().world_of.iter().filter(|(_, w)| w.is_empty()).map(|((_, k), _)| super::import::sha1_hex(k)).collect();
    let mut titles: BTreeSet<u64> = titles.into_iter().collect();
    if !alone.is_empty() {
        let sql = format!(
            "SELECT DISTINCT tr.title_id FROM `release` r JOIN title_releases tr ON tr.release_id = r.id JOIN title t ON t.id = tr.title_id AND t.deleted = FALSE \
             WHERE r.deleted = FALSE AND r.norm_hash IN ({}) AND EXISTS (SELECT 1 FROM title_releases t2 WHERE t2.title_id = tr.title_id AND t2.release_id <> r.id)",
            alone.iter().map(|h| format!("'{h}'")).collect::<Vec<_>>().join(",")
        );
        titles.extend(sqlx::query_scalar::<_, u64>(sqlx::AssertSqlSafe(sql)).fetch_all(pool).await?);
    }
    let titles: Vec<u64> = titles.into_iter().filter(|t| scope.touches(&[*t])).collect();
    let mut written = Vec::new();
    for chunk in titles.chunks(500) {
        let ids = list(chunk);
        let mut names: HashMap<u64, Vec<String>> = HashMap::new();
        let sql = format!(
            "SELECT id AS title_id, name FROM title WHERE id IN ({ids}) UNION ALL SELECT title_id, name FROM title_alt_names WHERE title_id IN ({ids}) \
             UNION ALL SELECT title_id, name FROM title_locales WHERE title_id IN ({ids})"
        );
        for r in sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(pool).await? {
            names.entry(r.try_get("title_id")?).or_default().push(r.try_get("name")?);
        }
        let mut releases: HashMap<u64, Vec<Rel>> = HashMap::new();
        for r in sqlx::query(sqlx::AssertSqlSafe(game_release_sql(&format!("AND tr.title_id IN ({ids})")))).fetch_all(pool).await? {
            let rel = rel_from(&r)?;
            releases.entry(rel.title).or_default().push(rel);
        }
        for title in chunk {
            let Some(rels) = releases.get(title) else { continue };
            let Some(detail) = plan_split(names.get(title).map(Vec::as_slice).unwrap_or(&[]), rels, RegionalNames::builtin()) else { continue };
            let odd_first = detail.options[0].groups.get(1).and_then(|g| g.first()).copied().unwrap_or(0);
            let id = format!("split:{title}:{odd_first}");
            upsert(pool, &id, KIND_SPLIT_NAMES, &[*title], &detail).await?;
            written.push(id);
        }
    }
    Ok(written)
}

/// Every finder, over what `scope` covers. A full pass also drops pending suggestions of these
/// kinds that no longer apply. Returns how many suggestions were written.
pub async fn suggest(pool: &MySqlPool, scope: &PassScope, progress: &Progress) -> Result<u64, DatabaseError> {
    let mut live: Vec<String> = Vec::new();
    progress.force(0, None, "review: International and Director's Cut releases").await;
    live.extend(variants(pool, scope).await?);
    progress.force(0, None, "review: releases of another game inside a title").await;
    live.extend(split_names(pool, scope).await?);
    progress.force(0, None, "review: names that translate each other").await;
    live.extend(translated_names(pool, scope).await?);
    progress.force(0, None, "review: bundles beside their game").await;
    live.extend(bundles(pool, scope).await?);
    if scope.dirty.is_none() {
        let keep: HashSet<&str> = live.iter().map(String::as_str).collect();
        let pending: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT igdb_id FROM title_merge_suggestions WHERE status = 'pending' AND kind IN ('{}')", KINDS.join("','")))).fetch_all(pool).await?;
        for id in pending.iter().filter(|id| !keep.contains(id.as_str())) {
            sqlx::query("DELETE FROM title_merge_suggestions WHERE igdb_id = ? AND status = 'pending'").bind(id).execute(pool).await?;
        }
    }
    Ok(live.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(id: u64, title: u64, name: &str, region: &str) -> Rel {
        Rel { id, title, name: name.into(), region: region.into(), platform: 7 }
    }

    #[test]
    fn variants_are_read_from_the_name() {
        assert_eq!(variant_of("Hoge Quest VII International (Japan) (Disc 1)"), Some((Variant::International, "hoge quest 7".into())));
        assert_eq!(variant_of("Fuga Saga - Director's Cut (Japan)"), Some((Variant::DirectorsCut, "fuga saga".into())));
        assert_eq!(variant_of("Fuga Saga (Japan) (Director's Edition)").map(|v| v.0), Some(Variant::DirectorsCut));
        assert_eq!(variant_of("Hoge Quest VII (Japan)"), None);
    }

    /// The Japanese original and the foreign versions in one title, the International alone:
    /// it goes with the foreign versions, recommended; already so, nothing to suggest.
    #[test]
    fn an_international_goes_with_the_foreign_versions() {
        let intl = [rel(1, 10, "Hoge Quest VII International (Japan)", "JP")];
        let others = [rel(2, 20, "Hoge Quest VII (Japan)", "JP"), rel(3, 20, "Hoge Quest VII (USA)", "US"), rel(4, 20, "Hoge Quest VII (Europe)", "EU")];
        let plan = plan_variant(Variant::International, &intl, &others).expect("a suggestion");
        let with = &plan.options[0];
        assert!(with.recommended && !with.current);
        assert_eq!(with.groups, vec![vec![3, 4, 1], vec![2]]);
        assert!(plan.options[1].groups == vec![vec![1], vec![2, 3, 4]] && plan.options[1].current, "today's shape is the other choice");
        let done = [rel(2, 30, "Hoge Quest VII (Japan)", "JP"), rel(3, 10, "Hoge Quest VII (USA)", "US"), rel(4, 10, "Hoge Quest VII (Europe)", "EU")];
        assert!(plan_variant(Variant::International, &intl, &done).is_none());
        assert!(plan_variant(Variant::International, &intl, &others[1..]).is_none(), "no Japanese original: nothing to decide");
    }

    /// A Director's Cut is offered both ways, neither recommended, whatever the shape.
    #[test]
    fn a_directors_cut_is_always_asked() {
        let dc = [rel(1, 10, "Fuga Saga - Director's Cut (Japan)", "JP")];
        let others = [rel(2, 20, "Fuga Saga (Japan)", "JP"), rel(3, 20, "Fuga Saga (USA)", "US")];
        let plan = plan_variant(Variant::DirectorsCut, &dc, &others).expect("asked");
        assert!(plan.options.iter().all(|o| !o.recommended));
        assert_eq!(plan.options.iter().filter(|o| o.current).count(), 1);
        assert_eq!(plan.variant.as_deref(), Some("Director's Cut"));
    }

    /// "Piyo Monsters Ao" inside "Piyo Monsters Midori"'s title: split off. When the title
    /// knows both names, or neither, there is nothing to say.
    #[test]
    fn a_release_of_another_name_is_split_off() {
        let rels = [rel(1, 9, "Piyo Monsters Midori (Japan)", "JP"), rel(2, 9, "Piyo Monsters Midori (Japan) (Rev 1)", "JP"), rel(3, 9, "Piyo Monsters Ao (Japan)", "JP"), rel(4, 9, "Piyo Monsters Blue (USA)", "US")];
        let names = vec!["Piyo Monsters Blue".to_string(), "Piyo Monsters Midori".to_string()];
        let plan = plan_split(&names, &rels, &RegionalNames::default()).expect("split");
        assert_eq!(plan.options[0].groups, vec![vec![1, 2, 4], vec![3]]);
        let all = vec!["Piyo Monsters Blue".to_string(), "Piyo Monsters Midori".to_string(), "Piyo Monsters Ao".to_string()];
        assert!(plan_split(&all, &rels, &RegionalNames::default()).is_none());
        assert!(plan_split(&["Something Else".to_string()], &rels, &RegionalNames::default()).is_none());
        // A special edition of the same game is the same game.
        let editions = [rel(1, 9, "Hoge Quest (Japan)", "JP"), rel(2, 9, "Hoge Quest - Special Edition (Japan)", "JP")];
        assert!(plan_split(&["Hoge Quest".to_string()], &editions, &RegionalNames::default()).is_none());
        // A Director's Cut beside its original is the variant finder's question, not this one's.
        let dc = [rel(1, 9, "Fuga Saga (Japan)", "JP"), rel(2, 9, "Fuga Saga - Director's Cut (Japan)", "JP")];
        assert!(plan_split(&["Fuga Saga".to_string()], &dc, &RegionalNames::default()).is_none());
    }

    #[test]
    /// The colour versions: the Japanese green is the foreign blue, the Japanese blue was
    /// never sold abroad, and "Aka" and "Ao" are close names but two games.
    #[test]
    fn the_regional_table_keeps_colour_versions_apart() {
        let table = RegionalNames::parse(
            r#"{ "entries": [
                { "platform": 9, "jp": "Piyo Monsters - Aka", "world": ["Piyo Monsters - Red Version"] },
                { "platform": 9, "jp": "Piyo Monsters - Midori", "world": ["Piyo Monsters - Blue Version"] },
                { "platform": 9, "jp": "Piyo Monsters - Ao", "world": [] }
            ] }"#,
        );
        let gb = |id: u64, name: &str, region: &str| Rel { id, title: 5, name: name.into(), region: region.into(), platform: 9 };
        // The blue title holds the foreign blue, the Japanese green and the Japanese blue.
        let rels = [gb(1, "Piyo Monsters - Blue Version (USA, Europe)", "US,EU"), gb(2, "Piyo Monsters - Midori (Japan)", "JP"), gb(3, "Piyo Monsters - Ao (Japan)", "JP")];
        let names = vec!["Piyo Monsters Blue Version".to_string(), "Piyo Monsters Ao".to_string()];
        let plan = plan_split(&names, &rels, &table).expect("the Japanese blue is split off");
        assert_eq!(plan.options[0].groups, vec![vec![1, 2], vec![3]], "green stays with the foreign blue, the title naming Ao or not");
        // The red title with the Japanese red and the Japanese blue: Ao off, Aka kept.
        let rels = [gb(4, "Piyo Monsters - Red Version (USA, Europe)", "US,EU"), gb(5, "Piyo Monsters - Aka (Japan)", "JP"), gb(6, "Piyo Monsters - Ao (Japan)", "JP")];
        let plan = plan_split(&["Piyo Monsters Red Version".to_string()], &rels, &table).expect("split");
        assert_eq!(plan.options[0].groups, vec![vec![4, 5], vec![6]]);
        // A title of the Japanese blue alone: nothing to split.
        assert!(plan_split(&["Piyo Monsters Ao".to_string()], &rels[2..], &table).is_none());
        assert_eq!(table.game(9, &norm_key("Piyo Monsters - Blue Version"), false), Some("piyo monsters midori"));
        assert!(table.alone(9, "piyo monsters ao") && !table.alone(9, "piyo monsters aka"));
    }

    #[test]
    fn the_built_in_regional_table_reads() {
        let table = RegionalNames::builtin();
        assert!(table.world_of.len() > 50, "{} entries", table.world_of.len());
        assert!(table.world_of.values().any(Vec::is_empty) && table.jp_of.len() > 50);
    }

    #[test]
    fn a_bundle_is_split_into_its_single_games_in_one_choice() {
        // "Hoge the Lad Collection" holds the collection itself and the Japanese releases of
        // the first two games; the first has a title of its own, the second has none.
        let rels = [
            rel(1, 50, "Hoge the Lad Collection (USA)", "US"),
            rel(2, 50, "Hoge the Lad (Japan)", "JP"),
            rel(3, 50, "Hoge the Lad II (Japan)", "JP"),
        ];
        let single = Counterpart { title: 60, name: "Hoge the Lad".into(), releases: vec![7, 8] };
        let plan = plan_bundle("Hoge the Lad Collection", &rels, |r| (r.id == 2).then(|| single.clone())).expect("a suggestion");
        let o = &plan.options[0];
        assert!(o.recommended && o.label == "split_bundle");
        // One click: the collection stays, the first game joins its title, the second gets one.
        assert_eq!(o.groups, vec![vec![1], vec![7, 8, 2], vec![3]]);
        assert_eq!(plan.matched, vec!["Hoge the Lad (Japan) → Hoge the Lad".to_string()]);
        // Only the collection itself: nothing to split.
        assert!(plan_bundle("Hoge the Lad Collection", &rels[..1], |_| None).is_none());
        // One game filed as a bundle, with no other title: nothing to split either.
        assert!(plan_bundle("Fuga Saga", &[rel(4, 51, "Fuga Saga Gaiden (Japan)", "JP")], |_| None).is_none());
    }
}
