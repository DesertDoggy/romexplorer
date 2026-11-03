//! TheGamesDB lists each regional release as a game of its own -- the Japanese box and the
//! European box are two entries with the same name -- and says which in `region_id` /
//! `country_id`. Linking by name alone gave every same-named release whichever entry was
//! processed last; here a release takes the entry of its own region.
//!
//! The ids mean nothing without TheGamesDB's own lists (`/v1/Regions`, `/v1/Countries`), one
//! call each, cached for 30 days. Its region field is the less reliable one -- a game sold in
//! Japan can say "PAL" -- so the country decides, and the region only when there is none.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use sqlx::mysql::MySqlPool;
use sqlx::Row;

use crate::common::models::{Platform, Region};

use super::super::error::DatabaseError;
use super::bulk::ExactNameIndex;
use super::cache::{self, Confidence};
use super::{Provider, ProviderClients};

/// TheGamesDB region and country ids, as our region codes (`US`, `JP`, `EU`, ...).
#[derive(Debug, Clone, Default)]
pub struct TgdbRegions {
    regions: HashMap<i64, &'static str>,
    countries: HashMap<i64, &'static str>,
}

impl TgdbRegions {
    /// The cached lists, fetched again when older than 30 days. Without a key and without a
    /// cache, empty: every entry's region is then unknown, which links as before.
    pub async fn load(clients: &ProviderClients, cache_root: &Path) -> Result<Self, DatabaseError> {
        let regions = cached_list(clients, &cache_root.join("thegamesdb").join("regions_list.json"), Kind::Regions).await?;
        let countries = cached_list(clients, &cache_root.join("thegamesdb").join("countries_list.json"), Kind::Countries).await?;
        let loaded = Self::from_lists(&regions, &countries);
        *REMEMBERED.write().unwrap_or_else(|p| p.into_inner()) = Some(loaded.clone());
        Ok(loaded)
    }

    /// The lists last loaded in this process, for a link made where they are not at hand;
    /// empty before any load (every region then unknown).
    pub fn remembered() -> Self {
        REMEMBERED.read().unwrap_or_else(|p| p.into_inner()).clone().unwrap_or_default()
    }

    pub fn from_lists(regions: &[(i64, String)], countries: &[(i64, String)]) -> Self {
        let map = |list: &[(i64, String)]| list.iter().filter_map(|(id, name)| region_code(name).map(|c| (*id, c))).collect();
        Self { regions: map(regions), countries: map(countries) }
    }

    /// The region an entry is for: its country when it has one we know, else its region.
    pub fn code_of(&self, region_id: Option<i64>, country_id: Option<i64>) -> Option<&'static str> {
        let known = |id: Option<i64>, map: &HashMap<i64, &'static str>| id.filter(|i| *i > 0).and_then(|i| map.get(&i).copied());
        known(country_id, &self.countries).or_else(|| known(region_id, &self.regions))
    }
}

static REMEMBERED: std::sync::RwLock<Option<TgdbRegions>> = std::sync::RwLock::new(None);

#[derive(Clone, Copy)]
enum Kind {
    Regions,
    Countries,
}

async fn cached_list(clients: &ProviderClients, path: &Path, kind: Kind) -> Result<Vec<(i64, String)>, DatabaseError> {
    let fresh = std::fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() < 30 * 86_400);
    let read = || -> Option<Vec<(i64, String)>> { serde_json::from_slice(&std::fs::read(path).ok()?).ok() };
    if fresh {
        if let Some(list) = read() {
            return Ok(list);
        }
    }
    let Some(client) = clients.thegamesdb.as_ref() else { return Ok(read().unwrap_or_default()) };
    let fetched = match kind {
        Kind::Regions => client.regions().await,
        Kind::Countries => client.countries().await,
    };
    match fetched {
        Ok(list) => {
            let list: Vec<(i64, String)> = list.into_iter().filter_map(|n| n.name.map(|name| (n.id, name))).collect();
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, serde_json::to_vec_pretty(&list)?)?;
            Ok(list)
        }
        // An old list beats none; none links as if regions were unknown.
        Err(e) => {
            log::warn!("thegamesdb: could not fetch its {} list: {e}", if matches!(kind, Kind::Regions) { "region" } else { "country" });
            Ok(read().unwrap_or_default())
        }
    }
}

/// A TheGamesDB region or country name as our region code. Its regions are video standards
/// ("NTSC-J", "PAL") as much as places.
pub fn region_code(name: &str) -> Option<&'static str> {
    let key = name.trim().to_ascii_lowercase();
    let code = match key.as_str() {
        "ntsc-j" => Some(Region::Japan),
        "ntsc-k" => Some(Region::Korea),
        "ntsc-c" => Some(Region::China),
        "ntsc" | "ntsc-u" | "north america" => Some(Region::Usa),
        "region free" | "worldwide" | "global" => Some(Region::World),
        _ => Region::from_name(&key),
    };
    code.filter(|r| *r != Region::Other).map(Region::code)
}

/// The countries `EU` stands for.
const EUROPE: [&str; 9] = ["UK", "FR", "DE", "IT", "ES", "NL", "SE", "PL", "RU"];

/// A release's `region` column (`US,EU`, or older rows' `USA` / `Japan`) as codes.
pub fn release_codes(region: Option<&str>) -> Vec<&'static str> {
    region
        .unwrap_or_default()
        .split(',')
        .filter_map(|token| {
            let token = token.trim();
            if token.is_empty() {
                return None;
            }
            match Region::from_code(token) {
                Region::Other => Region::from_name(token).filter(|r| *r != Region::Other).map(Region::code),
                r => Some(r.code()),
            }
        })
        .collect()
}

/// How well an entry for `entry` fits a release sold in `release` -- higher is better, then
/// the lower `region_priority` position of the code that matched.
///
/// 4: the same code. 3: one is a country inside the other (UK in EU). 2: either is WORLD.
/// 1: the entry's region is unknown. 0: they differ -- still a candidate, so a release whose
/// only same-name entry is another region's keeps a link rather than none.
pub fn score(release: &[&str], entry: Option<&str>, priority: &[String]) -> (u8, usize) {
    let rank = |code: &str| priority.iter().position(|p| p == code).unwrap_or(usize::MAX);
    let Some(entry) = entry else { return (1, usize::MAX) };
    let inside = |a: &str, b: &str| b == "EU" && EUROPE.contains(&a);
    let mut best = (0u8, usize::MAX);
    for code in release {
        let s = if *code == entry {
            4
        } else if inside(entry, code) || inside(code, entry) {
            3
        } else if *code == "WORLD" || entry == "WORLD" {
            2
        } else {
            0
        };
        let candidate = (s, if s > 0 { rank(code) } else { usize::MAX });
        if candidate.0 > best.0 || (candidate.0 == best.0 && candidate.1 < best.1) {
            best = candidate;
        }
    }
    if release.is_empty() && entry == "WORLD" {
        best = (2, usize::MAX);
    }
    best
}

/// Orders scores best first: higher score, then lower priority position.
fn better(a: (u8, usize), b: (u8, usize)) -> bool {
    a.0 > b.0 || (a.0 == b.0 && a.1 < b.1)
}

/// One TheGamesDB entry to link: its id, names (title and alternates), and region fields.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: String,
    pub names: Vec<String>,
    pub region_id: Option<i64>,
    pub country_id: Option<i64>,
}

/// Links TheGamesDB entries to one platform's releases by exact name, each release to the
/// same-named entry that fits its region best. A release already linked by name to an entry
/// that fits at least as well keeps it -- so a sync that pages through only part of a
/// platform cannot swap a good link for a worse one. Stronger links (hash, exact, manual) are
/// never touched. Returns how many entries linked something.
pub async fn link_platform(pool: &MySqlPool, regions: &TgdbRegions, priority: &[String], platform: Platform, games: &[Candidate]) -> Result<u64, DatabaseError> {
    if games.is_empty() || platform == Platform::Unknown {
        return Ok(0);
    }
    let index = ExactNameIndex::load(pool, platform).await?;
    let release_regions: HashMap<u64, Option<String>> = sqlx::query("SELECT id, region FROM `release` WHERE deleted = FALSE AND app_platform_id = ?")
        .bind(platform.igdb_id())
        .fetch_all(pool)
        .await?
        .iter()
        .map(|r| Ok((r.try_get::<u64, _>("id")?, r.try_get::<Option<String>, _>("region")?)))
        .collect::<Result<_, DatabaseError>>()?;
    // Each release's candidates.
    let mut by_release: HashMap<u64, Vec<&Candidate>> = HashMap::new();
    for game in games {
        if let Some(targets) = index.find(&game.names) {
            for (release_id, _) in targets {
                by_release.entry(*release_id).or_default().push(game);
            }
        }
    }
    let mut linked: HashSet<&str> = HashSet::new();
    let mut release_ids: Vec<u64> = by_release.keys().copied().collect();
    release_ids.sort_unstable();
    for release_id in release_ids {
        let codes = release_codes(release_regions.get(&release_id).and_then(|r| r.as_deref()));
        let mut candidates = by_release[&release_id].clone();
        // Deterministic among equals: the lowest id.
        candidates.sort_by_key(|c| c.id.parse::<i64>().unwrap_or(i64::MAX));
        let mut best: Option<(&Candidate, (u8, usize))> = None;
        for c in candidates {
            let s = score(&codes, regions.code_of(c.region_id, c.country_id), priority);
            if best.as_ref().is_none_or(|(_, b)| better(s, *b)) {
                best = Some((c, s));
            }
        }
        let Some((chosen, chosen_score)) = best else { continue };
        if !replaces_current(pool, regions, priority, release_id, &codes, &chosen.id, chosen_score).await? {
            continue;
        }
        cache::link_release(pool, release_id, Provider::TheGamesDb, &chosen.id, Confidence::Name).await?;
        linked.insert(chosen.id.as_str());
    }
    Ok(linked.len() as u64)
}

/// Whether a name-level link of `release_id` to TheGamesDB `new_id` should be written: there
/// is no current link, or the current one is name-level or weaker and fits the release's
/// region worse. A stronger link is left to `link_release`'s own rule (it keeps it).
async fn replaces_current(pool: &MySqlPool, regions: &TgdbRegions, priority: &[String], release_id: u64, codes: &[&str], new_id: &str, new_score: (u8, usize)) -> Result<bool, DatabaseError> {
    let current = sqlx::query("SELECT l.provider_id, l.confidence, g.region_id, g.country_id FROM release_provider_links l LEFT JOIN thegamesdb_games g ON g.provider_id = CAST(l.provider_id AS UNSIGNED) WHERE l.release_id = ? AND l.provider = 'thegamesdb' AND l.deleted = FALSE")
        .bind(release_id)
        .fetch_optional(pool)
        .await?;
    let Some(row) = current else { return Ok(true) };
    let id: String = row.try_get("provider_id")?;
    if id == new_id {
        return Ok(true);
    }
    let confidence: String = row.try_get("confidence")?;
    if !matches!(confidence.as_str(), "name" | "fuzzy") {
        return Ok(true);
    }
    let region_id: Option<u32> = row.try_get("region_id")?;
    let country_id: Option<u32> = row.try_get("country_id")?;
    let current_score = score(codes, regions.code_of(region_id.map(i64::from), country_id.map(i64::from)), priority);
    Ok(better(new_score, current_score))
}

/// Links just `release_ids` to TheGamesDB from the games already cached here (the free dump,
/// `thegamesdb_games`), by exact name on their platform and each to its own region's entry --
/// no API call. What "download art" for a set runs first: a per-platform sync to link a few
/// games costs one call of the small allowance per 20 games of the whole platform.
///
/// Matches the cached title only (the dump's alternate names aren't kept per game here).
/// Returns how many releases it linked, and whether anything is cached to link from at all.
pub async fn link_releases(pool: &MySqlPool, regions: &TgdbRegions, priority: &[String], release_ids: &[u64]) -> Result<(u64, bool), DatabaseError> {
    use super::super::matching::util::norm_key;
    let cached = sqlx::query("SELECT 1 FROM thegamesdb_games WHERE deleted = FALSE AND app_platform_id IS NOT NULL LIMIT 1").fetch_optional(pool).await?.is_some();
    if release_ids.is_empty() || !cached {
        return Ok((0, cached));
    }
    // The releases, by platform.
    let mut by_platform: HashMap<u16, Vec<(u64, String, Option<String>)>> = HashMap::new();
    for chunk in release_ids.chunks(1000) {
        let list = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT id, app_platform_id, norm_name, region FROM `release` WHERE deleted = FALSE AND id IN ({list})"))).fetch_all(pool).await?;
        for r in rows {
            by_platform.entry(r.try_get("app_platform_id")?).or_default().push((r.try_get("id")?, r.try_get::<Option<String>, _>("norm_name")?.unwrap_or_default(), r.try_get("region")?));
        }
    }
    let mut linked = 0u64;
    for (platform, releases) in by_platform {
        // That platform's cached entries by normalized name.
        let mut by_name: HashMap<String, Vec<(i64, Option<u32>, Option<u32>)>> = HashMap::new();
        let rows = sqlx::query("SELECT provider_id, title, region_id, country_id FROM thegamesdb_games WHERE deleted = FALSE AND app_platform_id = ?").bind(platform).fetch_all(pool).await?;
        for r in rows {
            let Some(title) = r.try_get::<Option<String>, _>("title")? else { continue };
            let key = norm_key(&title);
            if !key.is_empty() {
                by_name.entry(key).or_default().push((r.try_get::<u64, _>("provider_id")? as i64, r.try_get("region_id")?, r.try_get("country_id")?));
            }
        }
        for (release_id, norm_name, region) in releases {
            let Some(found) = by_name.get(&norm_name) else { continue };
            let codes = release_codes(region.as_deref());
            let mut best: Option<(i64, (u8, usize))> = None;
            for (id, region_id, country_id) in found {
                let s = score(&codes, regions.code_of(region_id.map(i64::from), country_id.map(i64::from)), priority);
                if best.is_none_or(|(b_id, b)| better(s, b) || (s == b && *id < b_id)) {
                    best = Some((*id, s));
                }
            }
            let Some((id, s)) = best else { continue };
            let id = id.to_string();
            if replaces_current(pool, regions, priority, release_id, &codes, &id, s).await? {
                cache::link_release(pool, release_id, Provider::TheGamesDb, &id, Confidence::Name).await?;
                linked += 1;
            }
        }
    }
    Ok((linked, cached))
}

/// For a link made from one search result (a title's name query): whether `release_id` should
/// take TheGamesDB `new_id`, given the entry's region and the release's current link.
pub async fn should_link(pool: &MySqlPool, regions: &TgdbRegions, priority: &[String], release_id: u64, new_id: &str) -> Result<bool, DatabaseError> {
    let region: Option<String> = sqlx::query_scalar("SELECT region FROM `release` WHERE id = ?").bind(release_id).fetch_optional(pool).await?.flatten();
    let codes = release_codes(region.as_deref());
    let entry: Option<(Option<u32>, Option<u32>)> = sqlx::query_as("SELECT region_id, country_id FROM thegamesdb_games WHERE provider_id = CAST(? AS UNSIGNED)").bind(new_id).fetch_optional(pool).await?;
    let (region_id, country_id) = entry.unwrap_or((None, None));
    let new_score = score(&codes, regions.code_of(region_id.map(i64::from), country_id.map(i64::from)), priority);
    replaces_current(pool, regions, priority, release_id, &codes, new_id, new_score).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regions() -> TgdbRegions {
        TgdbRegions::from_lists(&[(1, "NTSC-U".into()), (4, "NTSC-J".into()), (6, "PAL".into()), (9, "Region Free".into())], &[(28, "Japan".into()), (50, "USA".into()), (13, "United Kingdom".into()), (14, "Germany".into())])
    }

    fn priority() -> Vec<String> {
        ["US", "WORLD", "EU", "JP"].iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_country_decides_before_the_region() {
        let r = regions();
        // The user's example: a Japanese game whose region says PAL.
        assert_eq!(r.code_of(Some(6), Some(28)), Some("JP"));
        assert_eq!(r.code_of(Some(6), Some(0)), Some("EU"));
        assert_eq!(r.code_of(Some(0), Some(0)), None);
        assert_eq!(r.code_of(Some(4), None), Some("JP"));
        assert_eq!(r.code_of(Some(9), None), Some("WORLD"));
        assert_eq!(r.code_of(Some(77), Some(99)), None, "ids not in the lists are unknown");
    }

    #[test]
    fn release_regions_read_codes_and_old_names() {
        assert_eq!(release_codes(Some("US,EU")), vec!["US", "EU"]);
        assert_eq!(release_codes(Some("Japan")), vec!["JP"]);
        assert_eq!(release_codes(Some("USA")), vec!["US"]);
        assert!(release_codes(None).is_empty());
    }

    #[test]
    fn each_release_prefers_the_entry_of_its_own_region() {
        let p = priority();
        // Two same-name entries, Japan and USA: each release takes its own.
        assert!(better(score(&["JP"], Some("JP"), &p), score(&["JP"], Some("US"), &p)));
        assert!(better(score(&["US"], Some("US"), &p), score(&["US"], Some("JP"), &p)));
        // A UK entry fits a European release; a German one does too, below an EU entry.
        assert_eq!(score(&["EU"], Some("UK"), &p).0, 3);
        assert!(better(score(&["EU"], Some("EU"), &p), score(&["EU"], Some("DE"), &p)));
        // A US,EU release with a US and an EU entry: region_priority breaks the tie.
        assert!(better(score(&["US", "EU"], Some("US"), &p), score(&["US", "EU"], Some("EU"), &p)));
        // A WORLD release whose only entry is the US one still fits it (and beats none).
        assert_eq!(score(&["WORLD"], Some("US"), &p).0, 2);
        // An entry of unknown region is below any that fits, above one that differs.
        assert!(better(score(&["JP"], None, &p), score(&["JP"], Some("US"), &p)));
        assert!(better(score(&["JP"], Some("JP"), &p), score(&["JP"], None, &p)));
    }

    #[test]
    fn video_standards_name_regions() {
        assert_eq!(region_code("NTSC-J"), Some("JP"));
        assert_eq!(region_code("PAL"), Some("EU"));
        assert_eq!(region_code("NTSC-U"), Some("US"));
        assert_eq!(region_code("Japan"), Some("JP"));
        assert_eq!(region_code("Atlantis"), None);
    }
}
