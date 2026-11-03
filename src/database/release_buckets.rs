//! What kind of release each release is, as the Titles list groups them: a title is listed
//! once per bucket it has releases in -- its retail releases together, its demos together, each
//! hack on a row of its own -- and inside a title the buckets rank in [`BUCKETS`] order, full
//! versions before demos and pre-release builds.
//!
//! One SQL expression ([`bucket_sql`]) is the rule, used by the ranking, the `kinds` filter,
//! the list's grouping and what a set entry or a copy resolves to, so all of them agree.

use sqlx::mysql::MySqlPool;

use super::error::DatabaseError;

/// Every bucket, in rank order (a title's top release is from the first it has).
pub const BUCKETS: [&str; 15] = ["retail", "unlicensed", "aftermarket", "homebrew", "arcade", "wiiware", "translation", "hack", "mod", "patched", "pirate", "demo", "beta", "proto", "subset"];

/// The buckets whose releases are each a row of their own: every hack is a different game,
/// under its own name, and so is every pirate copy and RetroAchievements subset.
pub const PER_RELEASE: [&str; 5] = ["hack", "mod", "patched", "pirate", "subset"];

/// The `kinds` filter's older word for everything sold as it was: retail, and what used to
/// be listed with it.
pub const COMMERCIAL: [&str; 10] = ["retail", "unlicensed", "aftermarket", "homebrew", "arcade", "wiiware", "pirate", "demo", "beta", "proto"];

/// `release_unlicensed`: whether an unlicensed release with no other tag is its own bucket
/// (`separate`, the default) or listed with the title's retail releases (`retail`).
pub const KEY_UNLICENSED: &str = "release_unlicensed";

/// Whether unlicensed releases are listed with retail ([`KEY_UNLICENSED`] = `retail`).
pub async fn unlicensed_joins_retail(pool: &MySqlPool) -> Result<bool, DatabaseError> {
    let v: Option<serde_json::Value> = super::settings::get(pool, KEY_UNLICENSED).await?;
    Ok(v.as_ref().and_then(|v| v.as_str()) == Some("retail"))
}

/// The bucket of release `alias`, one of [`BUCKETS`]. The first that matches wins, so a
/// release with two tags is the less complete of the two: "(Demo) (Unl)" is a demo, a hack
/// of a beta a hack. What came with a game (DLC, an update, a soundtrack) stays with it.
pub fn bucket_sql(alias: &str, unlicensed_is_retail: bool) -> String {
    let a = alias;
    let unl = if unlicensed_is_retail { "" } else { " WHEN {a}.is_unlicensed THEN 'unlicensed'" };
    format!(
        "(CASE WHEN {a}.name REGEXP '[[(]Subset' THEN 'subset' \
           WHEN {a}.material IS NOT NULL THEN 'retail' \
           WHEN {a}.game_kind = 'mod' THEN 'mod' \
           WHEN {a}.game_kind = 'translation' OR {a}.patch_type = 'translation' THEN 'translation' \
           WHEN {a}.game_kind = 'hack' OR {a}.community_patched OR {a}.patch_type IS NOT NULL THEN 'hack' \
           WHEN {a}.game_kind IN ('homebrew', 'arcade', 'wiiware') THEN {a}.game_kind \
           WHEN NOT EXISTS (SELECT 1 FROM release_sources rsk JOIN sources sk ON sk.id = rsk.source_id WHERE rsk.release_id = {a}.id AND sk.name <> 'retroachievements') THEN 'patched' \
           WHEN {a}.is_pirate THEN 'pirate' \
           WHEN {a}.is_prototype THEN 'proto' \
           WHEN {a}.is_beta THEN 'beta' \
           WHEN {a}.is_demo THEN 'demo' \
           WHEN {a}.is_aftermarket THEN 'aftermarket'{unl} \
           ELSE 'retail' END)",
        unl = unl.replace("{a}", a)
    )
}

/// The bucket's place in [`BUCKETS`] (1 = retail): what ranks a title's releases first.
pub fn rank_sql(alias: &str, unlicensed_is_retail: bool) -> String {
    format!("FIELD({}, {})", bucket_sql(alias, unlicensed_is_retail), list(&BUCKETS))
}

/// The Titles list's row within a title: the bucket, or for the [`PER_RELEASE`] buckets the
/// bucket and the release (`hack:1234`).
pub fn row_key_sql(alias: &str, unlicensed_is_retail: bool) -> String {
    let b = bucket_sql(alias, unlicensed_is_retail);
    format!("IF({b} IN ({}), CONCAT({b}, ':', {alias}.id), {b})", list(&PER_RELEASE))
}

/// A row key ([`row_key_sql`]) as bucket and release: `("hack", Some(1234))`, `("demo", None)`.
/// None for anything that is not one.
pub fn parse_row_key(key: &str) -> Option<(&'static str, Option<u64>)> {
    let (bucket, release) = match key.split_once(':') {
        Some((b, r)) => (b, Some(r.parse::<u64>().ok()?)),
        None => (key, None),
    };
    let bucket = BUCKETS.iter().find(|b| **b == bucket)?;
    (PER_RELEASE.contains(bucket) == release.is_some()).then_some((bucket, release))
}

/// The SQL condition "release `alias` is in row `key` of its title" (`key` already parsed
/// by [`parse_row_key`], so it can go into the statement as text).
pub fn in_row_sql(alias: &str, key: &str, unlicensed_is_retail: bool) -> Option<String> {
    let (bucket, release) = parse_row_key(key)?;
    Some(match release {
        Some(id) => format!("({alias}.id = {id} AND {} = '{bucket}')", bucket_sql(alias, unlicensed_is_retail)),
        None => format!("{} = '{bucket}'", bucket_sql(alias, unlicensed_is_retail)),
    })
}

/// Where an own-release row's numbers start ([`row_id`]): above every title row's, below
/// 2^53 so a browser's numbers hold it exactly.
const OWN_ROWS: u64 = 1 << 52;

/// A list row as one number, for the UIs' selections, which are numbers: a title's row is
/// `title * 64 + bucket`, an own-release row (a hack, a pirate copy) `2^52 + release * 64 +
/// bucket`. Transient -- what is stored is the row key. None for a malformed key.
pub fn row_id(title_id: u64, row_key: &str) -> Option<u64> {
    let (bucket, release) = parse_row_key(row_key)?;
    let index = BUCKETS.iter().position(|b| *b == bucket)? as u64;
    Some(match release {
        Some(r) => OWN_ROWS + r * 64 + index,
        None => title_id * 64 + index,
    })
}

/// [`row_id`] back to (title, row key): an own-release row's title is the release's.
pub async fn decode_row_ids(pool: &MySqlPool, ids: &[u64]) -> Result<Vec<romexplorer_types::copy::TitleRowRef>, DatabaseError> {
    let mut out = Vec::with_capacity(ids.len());
    for &id in ids {
        let bucket = BUCKETS.get((id % 64) as usize).copied();
        let Some(bucket) = bucket else { continue };
        if id >= OWN_ROWS {
            let release = (id - OWN_ROWS) / 64;
            let title: Option<u64> = sqlx::query_scalar("SELECT MIN(title_id) FROM title_releases WHERE release_id = ?").bind(release).fetch_one(pool).await?;
            if let (Some(title_id), true) = (title, PER_RELEASE.contains(&bucket)) {
                out.push(romexplorer_types::copy::TitleRowRef { title_id, row_key: format!("{bucket}:{release}") });
            }
        } else if !PER_RELEASE.contains(&bucket) {
            out.push(romexplorer_types::copy::TitleRowRef { title_id: id / 64, row_key: bucket.to_string() });
        }
    }
    Ok(out)
}

fn list(items: &[&str]) -> String {
    items.iter().map(|b| format!("'{b}'")).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_keys_parse_only_as_written() {
        assert_eq!(parse_row_key("retail"), Some(("retail", None)));
        assert_eq!(parse_row_key("demo"), Some(("demo", None)));
        assert_eq!(parse_row_key("hack:12"), Some(("hack", Some(12))));
        assert_eq!(parse_row_key("hack"), None, "a hack row names its release");
        assert_eq!(parse_row_key("demo:12"), None, "a demo row is the title's");
        assert_eq!(parse_row_key("hack:x"), None);
        assert_eq!(parse_row_key("retail'; DROP TABLE x"), None);
    }

    #[test]
    fn row_ids_are_unique_and_fit_a_browser_number() {
        assert_eq!(row_id(7, "retail"), Some(7 * 64));
        assert_eq!(row_id(7, "demo"), Some(7 * 64 + 11));
        let hack = row_id(7, "hack:5000000").unwrap();
        assert!(hack > OWN_ROWS && hack < (1u64 << 53));
        assert_eq!((hack - OWN_ROWS) / 64, 5_000_000);
        assert_eq!(row_id(7, "hack"), None);
    }

    #[test]
    fn retail_ranks_first_and_unlicensed_follows_the_setting() {
        assert_eq!(BUCKETS[0], "retail");
        let pos = |b: &str| BUCKETS.iter().position(|x| *x == b).unwrap();
        assert!(pos("aftermarket") < pos("demo") && pos("translation") < pos("demo") && pos("demo") < pos("beta") && pos("beta") < pos("proto"));
        assert!(bucket_sql("r", false).contains("'unlicensed'"));
        assert!(!bucket_sql("r", true).contains("'unlicensed'"));
        assert!(COMMERCIAL.iter().all(|b| BUCKETS.contains(b)) && PER_RELEASE.iter().all(|b| BUCKETS.contains(b)));
    }
}
