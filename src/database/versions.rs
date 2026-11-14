//! Titles › "hide minor versions" and "one of each version group".
//!
//! **Minor versions.** A title is a minor version of another when the other is a fuller
//! edition of the same game: an International or Director's Cut, a "Final Mix", a third run
//! with extra content. Where it comes from, strongest first:
//! - the user (`title_version_relations`: `minor`, and `not_minor` to undo anything below);
//! - IGDB: a title of category `edition` (IGDB's "version of") is the fuller one, its base game
//!   the minor one -- except when the edition's name says it is a reduced one ("Easy Type",
//!   "Lite", a demo: then the other way round), and except the editions listed in
//!   `assets/version_groups.json` as `not_superset` (third versions that add some things and
//!   lose others);
//! - names, within one family: "<name> International" and "<name>" are the edition and the
//!   regular game even when IGDB does not link them.
//!
//! A minor title is hidden only where the fuller one covers it: every platform and region it
//! has a game release on must have one of the fuller title too. A Japan-only International
//! hides the Japanese regular game, never the American one.
//!
//! Inside one title the same goes for releases: on one platform and region, the International
//! / Director's Cut / ... release hides the regular one (and a regular release hides an Easy
//! Type or demo of itself).
//!
//! **Version groups.** Two or more equal versions of one game, sold side by side (which
//! monsters you get, which route): `assets/version_groups.json` lists them by IGDB id, and the
//! user adds or removes titles (`group` / `ungroup`). One version of each group is shown:
//! the one held here, else the user's pick (`version_group_picks.title_id`), else a stable
//! random one (`version_group_picks.seed`, which the 🎲 button moves on). They never touch the
//! Definitive tab's families, which come from IGDB's own links.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sqlx::mysql::MySqlPool;
use sqlx::Row;

use super::error::DatabaseError;

/// What the two filters hide, and the group of each grouped title.
#[derive(Debug, Default)]
pub struct VersionView {
    /// Titles a fuller edition covers (hide minor versions).
    pub minor_titles: HashSet<u64>,
    /// Releases a fuller release of the same title covers.
    pub minor_releases: HashSet<u64>,
    /// Titles of the versions of a group that are not shown (one of each version group).
    pub group_hidden: HashSet<u64>,
    /// Each grouped title's group.
    pub group_of: HashMap<u64, String>,
    pub groups: BTreeMap<String, Group>,
    /// (minor, major, source) pairs, every one found, hidden or not.
    pub pairs: Vec<(u64, u64, &'static str)>,
}

/// One version group.
#[derive(Debug, Clone, Serialize)]
pub struct Group {
    pub key: String,
    /// Its versions: an IGDB game (`igdb:<id>`) or a title of its own (`title:<id>`), with
    /// their titles.
    pub variants: BTreeMap<String, Vec<u64>>,
    /// The version shown.
    pub shown: String,
    pub owned: Vec<String>,
    pub pinned: Option<u64>,
    pub seed: u32,
}

/// One version of a listed set: an IGDB game, a dump (`sha1:<hex>`) for a version IGDB has
/// no entry of its own for, or several of those that are the same version (an IGDB entry and
/// the DAT's dump, when the two are separate titles here).
#[derive(Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
#[serde(untagged)]
enum Member {
    Igdb(u64),
    Key(String),
    Many(Vec<Member>),
}

impl Member {
    /// The single ids it stands for.
    fn parts(&self) -> Vec<&Member> {
        match self {
            Member::Many(v) => v.iter().flat_map(Member::parts).collect(),
            other => vec![other],
        }
    }

    /// The version's key: its first part's.
    fn key(&self) -> String {
        match self.parts().first() {
            Some(Member::Igdb(i)) => format!("igdb:{i}"),
            Some(Member::Key(k)) => k.to_ascii_lowercase(),
            _ => String::new(),
        }
    }
}

#[derive(Deserialize, Default)]
struct GroupsFile {
    #[serde(default)]
    groups: Vec<Vec<Member>>,
    /// IGDB editions of a regular game that are not a fuller version of it.
    #[serde(default)]
    not_superset: Vec<u64>,
    /// (fuller, minor) IGDB games: a combined edition that holds the minor one whole.
    #[serde(default)]
    supersedes: Vec<[u64; 2]>,
}

fn groups_file() -> &'static GroupsFile {
    static FILE: std::sync::OnceLock<GroupsFile> = std::sync::OnceLock::new();
    FILE.get_or_init(|| serde_json::from_str(include_str!("../../app_root_ex/assets/version_groups.json")).expect("version_groups.json parses"))
}

/// What a name says about its version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditionKind {
    /// A fuller edition: International, Director's Cut, ...
    Fuller,
    /// A reduced one: Easy Type, Lite, a demo.
    Reduced,
}

lazy_static::lazy_static! {
    /// Fuller editions, anywhere after the start of the name.
    static ref FULLER_ANY: regex::Regex = regex::Regex::new(
        r"(?i)[\s:\-~〜]*(\binternational(?:\s+(?:version|edition))?\b|\bdirector['’]?s\s+(?:cut|edition)\b|\bfinal\s+mix\b|\bmaniax(?:\s+chronicle)?(?:\s+edition)?\b|\b(?:special|complete|definitive|enhanced|ultimate|game\s+of\s+the\s+year|goty|royal|golden)\s+edition\b|\bfinal\s+cut\b|インターナショナル|ディレクターズ\s*カット|ファイナル\s*ミックス|マニアクス(?:\s*クロニクル)?|二周目はじめました|2周目はじめました)"
    ).unwrap();
    /// Fuller editions named by one word at the very end ("<name> Royal").
    static ref FULLER_END: regex::Regex = regex::Regex::new(r"(?i)[\s:\-~〜]+(royal|golden|fes|vengeance|integral|substance|subsistence|redux)\s*$").unwrap();
    /// Reduced versions.
    static ref REDUCED: regex::Regex = regex::Regex::new(r"(?i)[\s:\-~〜]*(\beasy\s+type\b|\blite\b|\bkids\b|\bdemo\b|\btrial\b|\btaikenban\b|体験版|イージータイプ)").unwrap();
}

/// A name's edition and the name without it, normalized ([`norm`]). `None` for a name that
/// says neither, or that starts with the word ("International Hoge Soccer" is a game).
pub fn edition_of(name: &str) -> Option<(EditionKind, String)> {
    for (kind, re) in [(EditionKind::Reduced, &*REDUCED), (EditionKind::Fuller, &*FULLER_ANY), (EditionKind::Fuller, &*FULLER_END)] {
        if let Some(m) = re.find(name) {
            if name[..m.start()].trim().is_empty() {
                continue;
            }
            let key = norm(&re.replace_all(name, ""));
            if !key.is_empty() {
                return Some((kind, key));
            }
        }
    }
    None
}

/// Lower case, letters and digits only: how two names are compared.
pub fn norm(name: &str) -> String {
    name.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// The stable random pick: FNV-1a over the group key and the seed.
fn pick_index(key: &str, seed: u32, n: usize) -> usize {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.bytes().chain(seed.to_le_bytes()) {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    (h % n.max(1) as u64) as usize
}

type Cached = Option<(Instant, Arc<VersionView>)>;

fn cache() -> &'static Mutex<Cached> {
    static CACHE: std::sync::OnceLock<Mutex<Cached>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Forgets the cached view: after the user's corrections, a pick, or a reroll.
pub fn invalidate() {
    *cache().lock().unwrap_or_else(|p| p.into_inner()) = None;
}

/// The view, worked out at most once a minute (the lists ask on every page).
pub async fn view(pool: &MySqlPool) -> Result<Arc<VersionView>, DatabaseError> {
    if let Some((at, v)) = cache().lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
        if at.elapsed() < Duration::from_secs(60) {
            return Ok(v.clone());
        }
    }
    let v = Arc::new(build(pool).await?);
    *cache().lock().unwrap_or_else(|p| p.into_inner()) = Some((Instant::now(), v.clone()));
    Ok(v)
}

fn list(ids: impl IntoIterator<Item = u64>) -> String {
    let v: Vec<String> = ids.into_iter().map(|i| i.to_string()).collect();
    if v.is_empty() {
        "0".into()
    } else {
        v.join(",")
    }
}

/// A title's or release's (platform, region) places: a region list split, `""` for none.
fn places(platform: u16, region: Option<&str>) -> Vec<(u16, String)> {
    let regions: Vec<String> = region.unwrap_or("").split(',').map(|r| r.trim().to_ascii_uppercase()).filter(|r| !r.is_empty()).collect();
    if regions.is_empty() {
        vec![(platform, String::new())]
    } else {
        regions.into_iter().map(|r| (platform, r)).collect()
    }
}

/// Whether `minor`'s places are all among `major`'s: same platform, and the same region, or
/// a worldwide one, or (for a place with no region) any.
fn covered(minor: &HashSet<(u16, String)>, major: &HashSet<(u16, String)>) -> bool {
    !minor.is_empty()
        && minor.iter().all(|(p, r)| major.contains(&(*p, r.clone())) || major.contains(&(*p, "WORLD".to_string())) || (r.is_empty() && major.iter().any(|(mp, _)| mp == p)))
}

async fn build(pool: &MySqlPool) -> Result<VersionView, DatabaseError> {
    let mut v = VersionView::default();
    let file = groups_file();
    let not_superset: HashSet<String> = file.not_superset.iter().map(u64::to_string).collect();

    // The user's corrections.
    let mut user_minor: Vec<(u64, u64)> = Vec::new();
    let mut not_minor: HashSet<(u64, u64)> = HashSet::new();
    let mut user_group: Vec<(u64, u64)> = Vec::new();
    let mut ungroup: HashSet<u64> = HashSet::new();
    for r in sqlx::query("SELECT title_id, other_title_id, CAST(kind AS CHAR) AS kind FROM title_version_relations").fetch_all(pool).await? {
        let (a, b, kind): (u64, Option<u64>, String) = (r.try_get("title_id")?, r.try_get("other_title_id")?, r.try_get("kind")?);
        match (kind.as_str(), b) {
            ("minor", Some(b)) => user_minor.push((a, b)),
            ("not_minor", Some(b)) => {
                not_minor.insert((a.min(b), a.max(b)));
            }
            ("group", Some(b)) => user_group.push((a, b)),
            ("ungroup", _) => {
                ungroup.insert(a);
            }
            _ => {}
        }
    }

    // --- minor versions: (minor, major) by pair, the strongest source first -------------
    let mut by_pair: HashMap<(u64, u64), (u64, u64, &'static str)> = HashMap::new();
    let mut add = |minor: u64, major: u64, source: &'static str| {
        if minor == major {
            return;
        }
        let pair = (minor.min(major), minor.max(major));
        if not_minor.contains(&pair) && source != "user" {
            return;
        }
        by_pair.entry(pair).or_insert((minor, major, source));
    };
    for (a, b) in &user_minor {
        add(*a, *b, "user");
    }
    // IGDB editions.
    for r in sqlx::query(
        "SELECT e.id, e.name, e.basegame_title_id AS base, (SELECT MIN(l.provider_id) FROM title_provider_links l WHERE l.title_id = e.id AND l.provider = 'igdb' AND l.deleted = FALSE) AS igdb \
         FROM title e JOIN title b ON b.id = e.basegame_title_id AND b.deleted = FALSE WHERE e.category = 'edition' AND e.deleted = FALSE",
    )
    .fetch_all(pool)
    .await?
    {
        let (id, name, base, igdb): (u64, String, u64, Option<String>) = (r.try_get("id")?, r.try_get("name")?, r.try_get("base")?, r.try_get("igdb")?);
        if igdb.as_ref().is_some_and(|g| not_superset.contains(g)) {
            continue;
        }
        if matches!(edition_of(&name), Some((EditionKind::Reduced, _))) {
            add(id, base, "igdb");
        } else {
            add(base, id, "igdb");
        }
    }
    // The listed combined editions.
    if !file.supersedes.is_empty() {
        let ids: HashSet<u64> = file.supersedes.iter().flatten().copied().collect();
        let mut titles: HashMap<u64, Vec<u64>> = HashMap::new();
        for r in sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT l.title_id, CAST(l.provider_id AS UNSIGNED) AS g FROM title_provider_links l JOIN title t ON t.id = l.title_id AND t.deleted = FALSE WHERE l.provider = 'igdb' AND l.deleted = FALSE AND l.provider_id IN ({})",
            ids.iter().map(|i| format!("'{i}'")).collect::<Vec<_>>().join(",")
        )))
        .fetch_all(pool)
        .await?
        {
            titles.entry(r.try_get("g")?).or_default().push(r.try_get("title_id")?);
        }
        for [major, minor] in &file.supersedes {
            for a in titles.get(minor).into_iter().flatten() {
                for b in titles.get(major).into_iter().flatten() {
                    add(*a, *b, "data");
                }
            }
        }
    }
    // Names, within a family.
    let named: Vec<(u64, String, u64)> = sqlx::query(
        "SELECT t.id, t.name, COALESCE(t.family_root_id, t.id) AS fam FROM title t WHERE t.deleted = FALSE AND COALESCE(t.family_root_id, t.id) IN \
         (SELECT COALESCE(x.family_root_id, x.id) FROM title x WHERE x.deleted = FALSE AND x.family_root_id IS NOT NULL)",
    )
    .fetch_all(pool)
    .await?
    .iter()
    .map(|r| Ok((r.try_get("id")?, r.try_get("name")?, r.try_get("fam")?)))
    .collect::<Result<_, sqlx::Error>>()?;
    let mut by_family_key: HashMap<(u64, String), Vec<u64>> = HashMap::new();
    for (id, name, fam) in &named {
        by_family_key.entry((*fam, norm(name))).or_default().push(*id);
    }
    for (id, name, fam) in &named {
        if let Some((kind, key)) = edition_of(name) {
            for other in by_family_key.get(&(*fam, key)).into_iter().flatten().filter(|o| *o != id) {
                match kind {
                    EditionKind::Fuller => add(*other, *id, "name"),
                    EditionKind::Reduced => add(*id, *other, "name"),
                }
            }
        }
    }
    v.pairs = by_pair.into_values().collect();
    v.pairs.sort_unstable();

    // --- minor releases, inside a title ---------------------------------------------------
    let rows = sqlx::query(
        "SELECT r.id, r.name, r.app_platform_id, r.region, tr.title_id FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE AND r.material IS NULL \
         WHERE tr.title_id IN (SELECT tr2.title_id FROM title_releases tr2 JOIN `release` r2 ON r2.id = tr2.release_id AND r2.deleted = FALSE \
           WHERE r2.name REGEXP '(?i)international|director.?s (cut|edition)|final mix|maniax|easy type|インターナショナル|ディレクターズ|ファイナル ?ミックス|マニアクス|周目はじめました|イージータイプ')",
    )
    .fetch_all(pool)
    .await?;
    struct Rel {
        id: u64,
        title: u64,
        platform: u16,
        key: String,
        edition: Option<EditionKind>,
        places: HashSet<(u16, String)>,
    }
    let mut rels: Vec<Rel> = Vec::new();
    for r in &rows {
        let name: String = r.try_get("name")?;
        let platform: u16 = r.try_get("app_platform_id")?;
        let region: Option<String> = r.try_get("region")?;
        let base = super::title_merge::base_title(&name);
        let (edition, key) = match edition_of(&base) {
            Some((k, key)) => (Some(k), key),
            None => (None, norm(&base)),
        };
        rels.push(Rel { id: r.try_get("id")?, title: r.try_get("title_id")?, platform, key, edition, places: places(platform, region.as_deref()).into_iter().collect() });
    }
    for a in &rels {
        let Some(kind) = a.edition else { continue };
        for b in rels.iter().filter(|b| b.title == a.title && b.platform == a.platform && b.id != a.id && b.edition.is_none() && b.key == a.key) {
            match kind {
                EditionKind::Fuller if covered(&b.places, &a.places) => {
                    v.minor_releases.insert(b.id);
                }
                EditionKind::Reduced if covered(&a.places, &b.places) => {
                    v.minor_releases.insert(a.id);
                }
                _ => {}
            }
        }
    }

    // --- version groups ---------------------------------------------------------------------
    let grouped = sets(pool, &user_group, &ungroup).await?;
    let all_titles: Vec<u64> = grouped.values().flat_map(|vs| vs.values().flatten().copied()).collect();
    let owned: HashSet<u64> = sqlx::query_scalar::<_, u64>(sqlx::AssertSqlSafe(format!(
        "SELECT DISTINCT tr.title_id FROM title_releases tr JOIN local_release_games lrg ON lrg.release_id = tr.release_id JOIN local_game lg ON lg.id = lrg.game_id AND lg.deleted = FALSE WHERE tr.title_id IN ({})",
        list(all_titles.iter().copied())
    )))
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect();
    let keys: Vec<String> = grouped.keys().cloned().collect();
    let mut picks: HashMap<String, (Option<u64>, u32)> = HashMap::new();
    if !keys.is_empty() {
        let marks = vec!["?"; keys.len()].join(",");
        let mut q = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT group_key, title_id, seed FROM version_group_picks WHERE group_key IN ({marks})")));
        for k in &keys {
            q = q.bind(k);
        }
        for r in q.fetch_all(pool).await? {
            picks.insert(r.try_get("group_key")?, (r.try_get("title_id")?, r.try_get("seed")?));
        }
    }
    for (key, variants) in grouped {
        let (pinned, seed) = picks.get(&key).cloned().unwrap_or((None, 0));
        let owned_variants: Vec<String> = variants.iter().filter(|(_, ts)| ts.iter().any(|t| owned.contains(t))).map(|(k, _)| k.clone()).collect();
        let candidates: Vec<String> = if owned_variants.is_empty() { variants.keys().cloned().collect() } else { owned_variants.clone() };
        let pinned_variant = pinned.and_then(|p| variants.iter().find(|(_, ts)| ts.contains(&p)).map(|(k, _)| k.clone())).filter(|k| candidates.contains(k));
        let shown = pinned_variant.unwrap_or_else(|| candidates[pick_index(&key, seed, candidates.len())].clone());
        for (variant, ts) in &variants {
            for t in ts {
                v.group_of.insert(*t, key.clone());
                if *variant != shown {
                    v.group_hidden.insert(*t);
                }
            }
        }
        v.groups.insert(key.clone(), Group { key, variants, shown, owned: owned_variants, pinned, seed });
    }
    // Versions of one set never hide each other: a third version is a member, not an edition.
    v.pairs.retain(|(minor, major, source)| matches!(*source, "user" | "data") || v.group_of.get(minor).is_none_or(|g| v.group_of.get(major) != Some(g)));
    // Where each title involved has game releases.
    let involved: HashSet<u64> = v.pairs.iter().flat_map(|(a, b, _)| [*a, *b]).collect();
    let mut at: HashMap<u64, HashSet<(u16, String)>> = HashMap::new();
    if !involved.is_empty() {
        for r in sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT tr.title_id, r.app_platform_id, r.region FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE AND r.material IS NULL WHERE tr.title_id IN ({})",
            list(involved.iter().copied())
        )))
        .fetch_all(pool)
        .await?
        {
            let (t, p, region): (u64, u16, Option<String>) = (r.try_get("title_id")?, r.try_get("app_platform_id")?, r.try_get("region")?);
            at.entry(t).or_default().extend(places(p, region.as_deref()));
        }
    }
    let mut majors_of: HashMap<u64, Vec<u64>> = HashMap::new();
    for (minor, major, _) in &v.pairs {
        majors_of.entry(*minor).or_default().push(*major);
    }
    let empty = HashSet::new();
    for (minor, majors) in &majors_of {
        let all: HashSet<(u16, String)> = majors.iter().flat_map(|m| at.get(m).unwrap_or(&empty).iter().cloned()).collect();
        if covered(at.get(minor).unwrap_or(&empty), &all) {
            v.minor_titles.insert(*minor);
        }
    }

    Ok(v)
}

/// The version sets as they are here: by group key, each version (`igdb:<id>`,
/// `sha1:<hex>` or `title:<id>`) with its titles. From the listed sets and the user's
/// `group` / `ungroup` corrections; sets of fewer than two versions present are left out.
///
/// A dump-keyed version (`sha1:`) is the title holding that dump only while the title is that
/// version alone (every release named like the dump's): before the user splits it off, it is
/// still inside another version's title, which keeps its own.
async fn sets(pool: &MySqlPool, user_group: &[(u64, u64)], ungroup: &HashSet<u64>) -> Result<BTreeMap<String, BTreeMap<String, Vec<u64>>>, DatabaseError> {
    let file = groups_file();
    // Titles by IGDB id, for the listed sets and the user's grouped titles.
    let listed: HashSet<String> = file.groups.iter().flatten().flat_map(Member::parts).filter_map(|m| if let Member::Igdb(i) = m { Some(i.to_string()) } else { None }).collect();
    let user_titles: HashSet<u64> = user_group.iter().flat_map(|(a, b)| [*a, *b]).collect();
    let mut igdb_of: HashMap<u64, String> = HashMap::new();
    let mut titles_of: HashMap<String, Vec<u64>> = HashMap::new();
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT l.title_id, l.provider_id FROM title_provider_links l JOIN title t ON t.id = l.title_id AND t.deleted = FALSE WHERE l.provider = 'igdb' AND l.deleted = FALSE AND (l.provider_id IN ({}) OR l.title_id IN ({}))",
        if listed.is_empty() { "''".to_string() } else { listed.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(",") },
        list(user_titles.iter().copied())
    )))
    .fetch_all(pool)
    .await?;
    for r in &rows {
        let (t, g): (u64, String) = (r.try_get("title_id")?, r.try_get("provider_id")?);
        let e = igdb_of.entry(t).or_insert_with(|| g.clone());
        if g < *e {
            *e = g.clone();
        }
        titles_of.entry(format!("igdb:{g}")).or_default().push(t);
    }
    // Dump-keyed versions: the title holding the dump, when it is that version alone.
    let hashes: Vec<String> = file
        .groups
        .iter()
        .flatten()
        .flat_map(Member::parts)
        .filter_map(|m| if let Member::Key(k) = m { k.strip_prefix("sha1:").map(|h| h.to_ascii_lowercase()) } else { None })
        .filter(|h| h.len() == 40 && h.chars().all(|c| c.is_ascii_hexdigit()))
        .collect();
    let mut keyed: HashMap<u64, String> = HashMap::new();
    // `hashes.value_hex` is stored lowercase and the list is lowercased above, so the
    // comparison is on the bare column and uses `idx_hashes_algo_value`.
    if !hashes.is_empty() {
        let found = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT h.value_hex AS sha1, r.name, tr.title_id FROM hashes h JOIN release_contents rc ON rc.content_id = h.content_id JOIN `release` r ON r.id = rc.release_id AND r.deleted = FALSE \
             JOIN title_releases tr ON tr.release_id = r.id JOIN title t ON t.id = tr.title_id AND t.deleted = FALSE WHERE h.algorithm = 'sha1' AND h.value_hex IN ({})",
            hashes.iter().map(|h| format!("'{h}'")).collect::<Vec<_>>().join(",")
        )))
        .fetch_all(pool)
        .await?;
        for r in &found {
            let (sha1, name, title): (String, String, u64) = (r.try_get("sha1")?, r.try_get("name")?, r.try_get("title_id")?);
            let key = norm(&super::title_merge::base_title(&name));
            let names: Vec<String> = sqlx::query_scalar("SELECT r.name FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE WHERE tr.title_id = ?").bind(title).fetch_all(pool).await?;
            if names.iter().all(|n| norm(&super::title_merge::base_title(n)) == key) {
                keyed.insert(title, format!("sha1:{sha1}"));
            }
        }
    }
    let variant_of = |t: u64| keyed.get(&t).cloned().or_else(|| igdb_of.get(&t).map(|g| format!("igdb:{g}"))).unwrap_or_else(|| format!("title:{t}"));
    // Union-find over versions.
    fn find(parent: &mut HashMap<String, String>, x: &str) -> String {
        let p = parent.entry(x.to_string()).or_insert_with(|| x.to_string()).clone();
        if p == x {
            return p;
        }
        let root = find(parent, &p);
        parent.insert(x.to_string(), root.clone());
        root
    }
    fn union(parent: &mut HashMap<String, String>, a: &str, b: &str) {
        let (ra, rb) = (find(parent, a), find(parent, b));
        if ra != rb {
            let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
            parent.insert(hi, lo);
        }
    }
    let mut parent: HashMap<String, String> = HashMap::new();
    let mut members: HashMap<String, HashSet<u64>> = HashMap::new();
    let put = |members: &mut HashMap<String, HashSet<u64>>, t: u64| {
        if !ungroup.contains(&t) {
            members.entry(variant_of(t)).or_default().insert(t);
        }
    };
    for g in &file.groups {
        let mut present: Vec<String> = Vec::new();
        for m in g {
            let key = m.key();
            let mut ts: Vec<u64> = Vec::new();
            for part in m.parts() {
                match part {
                    Member::Igdb(i) => ts.extend(titles_of.get(&format!("igdb:{i}")).into_iter().flatten().copied()),
                    Member::Key(k) => {
                        let k = k.to_ascii_lowercase();
                        ts.extend(keyed.iter().filter(|(_, v)| **v == k).map(|(t, _)| *t));
                    }
                    Member::Many(_) => {}
                }
            }
            for t in ts.into_iter().filter(|t| !ungroup.contains(t)) {
                // A title that is this version alone by its dump is never also another's.
                if keyed.get(&t).is_some_and(|k| !m.parts().iter().any(|p| matches!(p, Member::Key(x) if x.to_ascii_lowercase() == *k))) {
                    continue;
                }
                members.entry(key.clone()).or_default().insert(t);
            }
            present.push(key);
        }
        for w in present.windows(2) {
            union(&mut parent, &w[0], &w[1]);
        }
    }
    for (a, b) in user_group {
        if ungroup.contains(a) || ungroup.contains(b) {
            continue;
        }
        put(&mut members, *a);
        put(&mut members, *b);
        union(&mut parent, &variant_of(*a), &variant_of(*b));
    }
    let mut grouped: BTreeMap<String, BTreeMap<String, Vec<u64>>> = BTreeMap::new();
    for (variant, ts) in &members {
        if ts.is_empty() {
            continue;
        }
        let root = find(&mut parent, variant);
        let mut ts: Vec<u64> = ts.iter().copied().collect();
        ts.sort_unstable();
        grouped.entry(root).or_default().insert(variant.clone(), ts);
    }
    grouped.retain(|_, variants| variants.len() >= 2);
    Ok(grouped)
}

/// Each title of a version set, with its set and version: the Definitive tab's families never
/// join two versions of one set (`title_family::recompute_roots`).
pub async fn set_versions(pool: &MySqlPool) -> Result<HashMap<u64, (String, String)>, DatabaseError> {
    let (user_group, ungroup) = user_groups(pool).await?;
    let mut out = HashMap::new();
    for (key, variants) in sets(pool, &user_group, &ungroup).await? {
        for (variant, ts) in variants {
            for t in ts {
                out.insert(t, (key.clone(), variant.clone()));
            }
        }
    }
    Ok(out)
}

async fn user_groups(pool: &MySqlPool) -> Result<(Vec<(u64, u64)>, HashSet<u64>), DatabaseError> {
    let mut group = Vec::new();
    let mut ungroup = HashSet::new();
    for r in sqlx::query("SELECT title_id, other_title_id, CAST(kind AS CHAR) AS kind FROM title_version_relations WHERE kind IN ('group','ungroup')").fetch_all(pool).await? {
        let (a, b, kind): (u64, Option<u64>, String) = (r.try_get("title_id")?, r.try_get("other_title_id")?, r.try_get("kind")?);
        match (kind.as_str(), b) {
            ("group", Some(b)) => group.push((a, b)),
            ("ungroup", _) => {
                ungroup.insert(a);
            }
            _ => {}
        }
    }
    Ok((group, ungroup))
}

/// Sets which version of `group_key` is shown (`None`: back to held / random).
pub async fn set_pick(pool: &MySqlPool, group_key: &str, title_id: Option<u64>) -> Result<(), DatabaseError> {
    sqlx::query("INSERT INTO version_group_picks (group_key, title_id) VALUES (?, ?) ON DUPLICATE KEY UPDATE title_id = VALUES(title_id)").bind(group_key).bind(title_id).execute(pool).await?;
    invalidate();
    Ok(())
}

/// The 🎲 button: a new random pick (and no pinned one).
pub async fn reroll(pool: &MySqlPool, group_key: &str) -> Result<(), DatabaseError> {
    sqlx::query("INSERT INTO version_group_picks (group_key, title_id, seed) VALUES (?, NULL, 1) ON DUPLICATE KEY UPDATE title_id = NULL, seed = seed + 1").bind(group_key).execute(pool).await?;
    invalidate();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_say_which_edition() {
        assert_eq!(edition_of("Hoge Quest VII International"), Some((EditionKind::Fuller, "hogequestvii".into())));
        assert_eq!(edition_of("Hoge Quest: Director's Cut"), Some((EditionKind::Fuller, "hogequest".into())));
        assert_eq!(edition_of("Hoge Quest IV Easy Type"), Some((EditionKind::Reduced, "hogequestiv".into())));
        assert_eq!(edition_of("Piyo Tensei V Vengeance"), Some((EditionKind::Fuller, "piyotenseiv".into())));
        assert_eq!(edition_of("ほげクエスト 二周目はじめました"), Some((EditionKind::Fuller, "ほげクエスト".into())));
        assert_eq!(edition_of("International Hoge Soccer"), None, "a name that starts with it is a game called so");
        assert_eq!(edition_of("Royal Hoge"), None);
        assert_eq!(edition_of("Hoge Quest"), None);
    }

    #[test]
    fn a_minor_version_is_hidden_only_where_it_is_covered() {
        let s = |v: &[(u16, &str)]| v.iter().map(|(p, r)| (*p, r.to_string())).collect::<HashSet<_>>();
        assert!(covered(&s(&[(1, "JP")]), &s(&[(1, "JP"), (2, "US")])));
        assert!(!covered(&s(&[(1, "JP"), (1, "US")]), &s(&[(1, "JP")])), "the American regular game stays");
        assert!(!covered(&s(&[(1, "JP")]), &s(&[(2, "JP")])), "another platform does not cover it");
        assert!(covered(&s(&[(1, "US")]), &s(&[(1, "WORLD")])));
        assert!(covered(&s(&[(1, "")]), &s(&[(1, "EU")])));
        assert!(!covered(&s(&[]), &s(&[(1, "JP")])), "a title with no game release is never hidden");
    }

    #[test]
    fn the_random_pick_is_stable_and_moves_with_the_seed() {
        let a = pick_index("igdb:1", 0, 3);
        assert_eq!(a, pick_index("igdb:1", 0, 3));
        assert!((0..20).any(|s| pick_index("igdb:1", s, 3) != a), "rerolling changes it at some point");
        assert!(pick_index("igdb:1", 7, 2) < 2);
    }

    #[test]
    fn the_groups_file_parses() {
        let f = groups_file();
        assert!(f.groups.iter().all(|g| g.len() >= 2), "a group has two versions or more");
        let ids: Vec<Member> = f.groups.iter().flatten().flat_map(Member::parts).cloned().collect();
        let unique: HashSet<Member> = ids.iter().cloned().collect();
        assert_eq!(ids.len(), unique.len(), "a game is in one group at most");
    }
}
