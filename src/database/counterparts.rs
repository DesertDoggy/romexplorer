//! Release -> USA / English counterpart table, a development export to share with other
//! developers (`romexplorer_dbtool export-counterparts`, `GET /api/v1/dev/counterparts/...`).
//!
//! For every release that isn't itself a USA (or World, or English-only) release, the best
//! release of the same title on the same platform that is: a USA release when the release
//! carries a region, an English release when it only carries languages. Titles are the
//! catalog's (after title merging), so "Shin Legend of Hogehoge V (Japan)" pairs with
//! "Legend of Hogehoge V (USA)". With no counterpart, a Japan-only release names itself (its
//! romanized DAT name is its canonical name); anything else gets an empty counterpart --
//! the table doubles as a list for others to fill in.
//!
//! Two tables, three fields each (platform, release, counterpart): `full` with the DAT
//! names as they are, `stripped` with region / language / revision tags removed and
//! duplicates collapsed. Written as RFC 4180 CSV.

use std::collections::{BTreeMap, HashMap};

use sqlx::mysql::MySqlPool;
use sqlx::Row;

use super::error::DatabaseError;
use super::matching::name_parse::strip_region_lang_revision;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterpartRow {
    pub platform: String,
    pub release: String,
    pub counterpart: Option<String>,
}

/// What the pairing needs to know about one release.
#[derive(Debug, Clone)]
pub struct Rel {
    pub id: u64,
    pub name: String,
    pub title_id: Option<u64>,
    pub regions: Vec<String>,
    pub languages: Vec<String>,
    pub revision: Option<String>,
    pub disc_count: u16,
    pub prerelease: bool,
}

impl Rel {
    fn has_region(&self, code: &str) -> bool {
        self.regions.iter().any(|r| r == code)
    }

    /// Already a USA / World / English-only release: a target, not a row.
    pub fn is_target(&self) -> bool {
        self.has_region("US") || self.has_region("WORLD") || (self.regions.is_empty() && !self.languages.is_empty() && self.languages.iter().all(|l| l == "en"))
    }

    fn japan_only(&self) -> bool {
        (self.regions.len() == 1 && self.regions[0] == "JP") || (self.regions.is_empty() && self.languages.len() == 1 && self.languages[0] == "ja")
    }
}

fn split_list(v: Option<String>, upper: bool) -> Vec<String> {
    v.unwrap_or_default().split(',').map(|s| if upper { s.trim().to_ascii_uppercase() } else { s.trim().to_ascii_lowercase() }).filter(|s| !s.is_empty()).collect()
}

/// A revision's sort key: none < any, then longer / larger ("Rev 2" > "Rev 1", "v1.10" > "v1.9").
fn revision_key(rev: &Option<String>) -> (bool, usize, String) {
    match rev {
        None => (false, 0, String::new()),
        Some(r) => {
            let digits: String = r.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
            (true, digits.len(), digits)
        }
    }
}

/// The counterpart of `r` among `candidates` (same title and platform), or `None`.
pub fn pick_counterpart<'a>(r: &Rel, candidates: &'a [Rel]) -> Option<&'a Rel> {
    let by_region = !r.regions.is_empty() || r.languages.is_empty();
    candidates
        .iter()
        .filter(|c| c.id != r.id)
        .filter(|c| if by_region { c.has_region("US") || c.has_region("WORLD") } else { c.languages.iter().any(|l| l == "en") || c.has_region("US") })
        .min_by(|a, b| {
            let rank = |c: &Rel| {
                let region_rank = if c.regions == ["US"] {
                    0
                } else if c.has_region("US") {
                    1
                } else if c.has_region("WORLD") {
                    2
                } else {
                    3
                };
                (c.prerelease != r.prerelease, region_rank, c.disc_count != r.disc_count)
            };
            rank(a)
                .cmp(&rank(b))
                .then_with(|| revision_key(&b.revision).cmp(&revision_key(&a.revision)))
                .then_with(|| a.name.len().cmp(&b.name.len()))
                .then_with(|| a.name.cmp(&b.name))
        })
}

/// Rows for one platform's releases (any order in; sorted by release name out).
pub fn rows_for_platform(platform: &str, releases: &[Rel]) -> Vec<CounterpartRow> {
    let mut by_title: HashMap<u64, Vec<Rel>> = HashMap::new();
    for r in releases {
        if let Some(t) = r.title_id {
            by_title.entry(t).or_default().push(r.clone());
        }
    }
    let empty: Vec<Rel> = Vec::new();
    let mut out: Vec<CounterpartRow> = releases
        .iter()
        .filter(|r| !r.is_target())
        .map(|r| {
            let siblings = r.title_id.and_then(|t| by_title.get(&t)).unwrap_or(&empty);
            let counterpart = match pick_counterpart(r, siblings) {
                Some(c) => Some(c.name.clone()),
                None if r.japan_only() => Some(r.name.clone()),
                None => None,
            };
            CounterpartRow { platform: platform.to_string(), release: r.name.clone(), counterpart }
        })
        .collect();
    out.sort_by(|a, b| a.release.cmp(&b.release));
    out
}

/// The `stripped` table: both names without region / language / revision tags, one row
/// per (platform, stripped release); when its full rows disagree, the most common non-empty
/// counterpart wins.
pub fn stripped(rows: &[CounterpartRow]) -> Vec<CounterpartRow> {
    let mut votes: BTreeMap<(String, String), BTreeMap<String, usize>> = BTreeMap::new();
    for r in rows {
        let key = (r.platform.clone(), strip_region_lang_revision(&r.release));
        let counter = r.counterpart.as_deref().map(strip_region_lang_revision).unwrap_or_default();
        *votes.entry(key).or_default().entry(counter).or_default() += 1;
    }
    let mut conflicts = 0usize;
    let out = votes
        .into_iter()
        .map(|((platform, release), counts)| {
            let non_empty: Vec<(&String, &usize)> = counts.iter().filter(|(c, _)| !c.is_empty()).collect();
            if non_empty.len() > 1 {
                conflicts += 1;
            }
            let best = non_empty.iter().max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0))).map(|(c, _)| (*c).clone());
            CounterpartRow { platform, release, counterpart: best }
        })
        .collect();
    if conflicts > 0 {
        log::info!("counterparts: {conflicts} stripped name(s) had differing counterparts (most common kept)");
    }
    out
}

/// Both tables for the whole catalog, platforms by display name.
pub async fn build(pool: &MySqlPool) -> Result<Vec<CounterpartRow>, DatabaseError> {
    let platforms = sqlx::query("SELECT DISTINCT r.app_platform_id, p.name FROM `release` r JOIN platforms p ON p.id = r.app_platform_id WHERE r.deleted = FALSE ORDER BY p.name").fetch_all(pool).await?;
    let mut out = Vec::new();
    for p in platforms {
        let platform_id: u16 = p.try_get("app_platform_id")?;
        let platform: String = p.try_get("name")?;
        let rows = sqlx::query(
            "SELECT r.id, r.name, r.region, r.languages, r.revision, r.disc_count, CAST((r.is_demo OR r.is_beta OR r.is_prototype) AS SIGNED) AS pre, \
             (SELECT MIN(tr.title_id) FROM title_releases tr WHERE tr.release_id = r.id) AS title_id \
             FROM `release` r WHERE r.app_platform_id = ? AND r.deleted = FALSE",
        )
        .bind(platform_id)
        .fetch_all(pool)
        .await?;
        let releases: Vec<Rel> = rows
            .iter()
            .map(|r| {
                Ok(Rel {
                    id: r.try_get("id")?,
                    name: r.try_get("name")?,
                    title_id: r.try_get("title_id")?,
                    regions: split_list(r.try_get("region")?, true),
                    languages: split_list(r.try_get("languages")?, false),
                    revision: r.try_get("revision")?,
                    disc_count: r.try_get("disc_count")?,
                    prerelease: r.try_get::<i64, _>("pre")? != 0,
                })
            })
            .collect::<Result<_, sqlx::Error>>()?;
        out.extend(rows_for_platform(&platform, &releases));
    }
    Ok(out)
}

fn csv_field(v: &str) -> String {
    if v.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v.to_string()
    }
}

/// RFC 4180 CSV (UTF-8, CRLF line ends, header `platform,release,counterpart`).
pub fn to_csv(rows: &[CounterpartRow]) -> String {
    let mut s = String::from("platform,release,counterpart\r\n");
    for r in rows {
        s.push_str(&format!("{},{},{}\r\n", csv_field(&r.platform), csv_field(&r.release), csv_field(r.counterpart.as_deref().unwrap_or(""))));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(id: u64, name: &str, title: u64, regions: &str, langs: &str, rev: Option<&str>, pre: bool) -> Rel {
        Rel {
            id,
            name: name.to_string(),
            title_id: Some(title),
            regions: split_list(Some(regions.to_string()), true),
            languages: split_list(Some(langs.to_string()), false),
            revision: rev.map(str::to_string),
            disc_count: 1,
            prerelease: pre,
        }
    }

    #[test]
    fn pairs_by_region_then_language() {
        let set = vec![
            rel(1, "Shin Legend of Hogehoge V (Japan)", 7, "JP", "ja", None, false),
            rel(2, "Legend of Hogehoge V (Europe) (En,Fr,De,Es,It)", 7, "EU", "en,fr,de,es,it", None, false),
            rel(3, "Legend of Hogehoge V (USA)", 7, "US", "en", None, false),
            rel(4, "Legend of Hogehoge V (USA, Europe)", 7, "US,EU", "", None, false),
            rel(5, "Legend of Hogehoge V (USA) (Rev 1)", 7, "US", "", Some("Rev 1"), false),
            rel(6, "Legend of Hogehoge V (Japan) (Demo)", 7, "JP", "", None, true),
            rel(7, "Legend of Hogehoge V (USA) (Demo)", 7, "US", "", None, true),
            rel(8, "Hanguk Only (Korea)", 8, "KR", "", None, false),
            rel(9, "Japan Only (Japan)", 9, "JP", "", None, false),
            rel(10, "Lang Only (Ja)", 10, "", "ja", None, false),
            rel(11, "Lang Only (En)", 10, "", "en", None, false),
        ];
        let rows = rows_for_platform("Sony PlayStation 2", &set);
        let get = |n: &str| rows.iter().find(|r| r.release == n).map(|r| r.counterpart.clone());
        assert_eq!(get("Shin Legend of Hogehoge V (Japan)"), Some(Some("Legend of Hogehoge V (USA) (Rev 1)".into())), "exact US, latest revision");
        assert_eq!(get("Legend of Hogehoge V (Europe) (En,Fr,De,Es,It)"), Some(Some("Legend of Hogehoge V (USA) (Rev 1)".into())));
        assert_eq!(get("Legend of Hogehoge V (Japan) (Demo)"), Some(Some("Legend of Hogehoge V (USA) (Demo)".into())), "a demo pairs with a demo");
        assert_eq!(get("Legend of Hogehoge V (USA)"), None, "USA releases are targets, not rows");
        assert_eq!(get("Hanguk Only (Korea)"), Some(None), "no counterpart: empty, left for others to fill");
        assert_eq!(get("Japan Only (Japan)"), Some(Some("Japan Only (Japan)".into())), "Japan-only: its own romanized name");
        assert_eq!(get("Lang Only (Ja)"), Some(Some("Lang Only (En)".into())), "language-only pairs by English");
        let s = stripped(&rows);
        let sget = |n: &str| s.iter().find(|r| r.release == n).map(|r| r.counterpart.clone());
        assert_eq!(sget("Shin Legend of Hogehoge V"), Some(Some("Legend of Hogehoge V".into())));
        assert_eq!(sget("Hanguk Only"), Some(None));
    }

    #[test]
    fn csv_quotes_when_needed() {
        let rows = vec![CounterpartRow { platform: "Sony PlayStation 2".into(), release: "Legend of Hogehoge V (Europe) (En,Fr,De)".into(), counterpart: Some("Say \"Hi\"".into()) }, CounterpartRow { platform: "P".into(), release: "A".into(), counterpart: None }];
        assert_eq!(to_csv(&rows), "platform,release,counterpart\r\nSony PlayStation 2,\"Legend of Hogehoge V (Europe) (En,Fr,De)\",\"Say \"\"Hi\"\"\"\r\nP,A,\r\n");
    }
}
