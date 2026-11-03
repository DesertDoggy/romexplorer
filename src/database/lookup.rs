//! The read side: hash -> content -> release -> title, and name -> release candidates.

use sqlx::mysql::MySqlPool;
use sqlx::Row;

use crate::common::models::{HashAlgorithms, Platform};

use super::error::DatabaseError;
use super::matching;

/// One `content` row that matched a looked-up hash, with the release(s)/title it belongs to.
#[derive(Debug, Clone)]
pub struct HashMatch {
    pub content_id: u64,
    pub file_name: Option<String>,
    pub size: Option<u64>,
    pub platform: Platform,
    pub source: Option<String>,
    pub release_id: Option<u64>,
    pub release_name: Option<String>,
    pub region: Option<String>,
    pub languages: Option<String>,
    pub year: Option<u16>,
    pub revision: Option<String>,
    pub serial: Option<String>,
    pub category: Option<String>,
    pub title_id: Option<u64>,
    pub title_name: Option<String>,
}

/// A release found by name, with what scoring needs.
#[derive(Debug, Clone)]
pub struct ReleaseCandidate {
    pub release_id: u64,
    pub name: String,
    pub norm_name: String,
    pub platform: Platform,
    pub region: Option<String>,
    pub year: Option<u16>,
    pub source: Option<String>,
    pub title_id: Option<u64>,
    pub title_name: Option<String>,
}

pub fn algorithm_column(algorithm: HashAlgorithms) -> &'static str {
    match algorithm {
        HashAlgorithms::Crc32 => "crc32",
        HashAlgorithms::Md5 => "md5",
        HashAlgorithms::Sha1 => "sha1",
        HashAlgorithms::Sha256 => "sha256",
        HashAlgorithms::Rahash => "ra_hash",
        HashAlgorithms::Xxhash3_128 => "xxh3_128",
    }
}

const MATCH_SELECT: &str = "SELECT c.id AS content_id, c.file_name, c.size, c.app_platform_id, s.name AS source, \
    r.id AS release_id, r.name AS release_name, r.region, r.languages, r.year, r.revision, r.serial, r.category, \
    t.id AS title_id, t.name AS title_name \
    FROM hashes h \
    JOIN content c ON c.id = h.content_id AND c.deleted = FALSE \
    LEFT JOIN sources s ON s.id = c.source_id \
    LEFT JOIN release_contents rc ON rc.content_id = c.id \
    LEFT JOIN `release` r ON r.id = rc.release_id AND r.deleted = FALSE \
    LEFT JOIN title_releases tr ON tr.release_id = r.id \
    LEFT JOIN title t ON t.id = tr.title_id AND t.deleted = FALSE \
    WHERE h.algorithm = ? AND h.value_hex = ?";

fn row_to_match(row: &sqlx::mysql::MySqlRow) -> Result<HashMatch, DatabaseError> {
    let platform_id: u16 = row.try_get("app_platform_id")?;
    Ok(HashMatch {
        content_id: row.try_get("content_id")?,
        file_name: row.try_get("file_name")?,
        size: row.try_get("size")?,
        platform: Platform::from_igdb_id(platform_id).unwrap_or(Platform::Unknown),
        source: row.try_get("source")?,
        release_id: row.try_get("release_id")?,
        release_name: row.try_get("release_name")?,
        region: row.try_get("region")?,
        languages: row.try_get("languages")?,
        year: row.try_get("year")?,
        revision: row.try_get("revision")?,
        serial: row.try_get("serial")?,
        category: row.try_get("category")?,
        title_id: row.try_get("title_id")?,
        title_name: row.try_get("title_name")?,
    })
}

/// Every content/release/title row whose content has `value` recorded for `algorithm`
/// (case-insensitive hex compare -- values are stored lowercased by `import`).
pub async fn lookup_by_hash(pool: &MySqlPool, algorithm: HashAlgorithms, value: &str) -> Result<Vec<HashMatch>, DatabaseError> {
    let rows = sqlx::query(MATCH_SELECT).bind(algorithm_column(algorithm)).bind(value.trim().to_ascii_lowercase()).fetch_all(pool).await?;
    rows.iter().map(row_to_match).collect()
}

/// Tries `hashes` strongest-first (sha256, sha1, md5, ra_hash, crc32) and returns the first
/// algorithm that hits, with its matches.
pub async fn lookup_by_hashes(pool: &MySqlPool, hashes: &[(HashAlgorithms, String)]) -> Result<Option<(HashAlgorithms, Vec<HashMatch>)>, DatabaseError> {
    let order = [HashAlgorithms::Sha256, HashAlgorithms::Sha1, HashAlgorithms::Md5, HashAlgorithms::Rahash, HashAlgorithms::Crc32];
    for algorithm in order {
        for (a, v) in hashes {
            if *a == algorithm && !v.is_empty() {
                let matches = lookup_by_hash(pool, algorithm, v).await?;
                if !matches.is_empty() {
                    return Ok(Some((algorithm, matches)));
                }
            }
        }
    }
    Ok(None)
}

const CANDIDATE_SELECT: &str = "SELECT r.id AS release_id, r.name, r.norm_name, r.app_platform_id, r.region, r.year, s.name AS source, t.id AS title_id, t.name AS title_name \
    FROM `release` r \
    LEFT JOIN sources s ON s.id = r.source_id \
    LEFT JOIN title_releases tr ON tr.release_id = r.id \
    LEFT JOIN title t ON t.id = tr.title_id \
    WHERE r.deleted = FALSE";

fn row_to_candidate(row: &sqlx::mysql::MySqlRow) -> Result<ReleaseCandidate, DatabaseError> {
    let platform_id: u16 = row.try_get("app_platform_id")?;
    Ok(ReleaseCandidate {
        release_id: row.try_get("release_id")?,
        name: row.try_get("name")?,
        norm_name: row.try_get("norm_name")?,
        platform: Platform::from_igdb_id(platform_id).unwrap_or(Platform::Unknown),
        region: row.try_get("region")?,
        year: row.try_get("year")?,
        source: row.try_get("source")?,
        title_id: row.try_get("title_id")?,
        title_name: row.try_get("title_name")?,
    })
}

/// Releases whose normalized base title equals `name`'s (exact key match), optionally
/// restricted to `platform`; falls back to a prefix match on the normalized name when the
/// exact key finds nothing. Deduplicated per release.
pub async fn search_releases_by_name(pool: &MySqlPool, name: &str, platform: Option<Platform>, limit: u32) -> Result<Vec<ReleaseCandidate>, DatabaseError> {
    let key = matching::util::norm_key(name);
    if key.is_empty() {
        return Ok(Vec::new());
    }
    let norm_hash = super::import::sha1_hex(&key);
    let mut sql = format!("{CANDIDATE_SELECT} AND r.norm_hash = ?");
    if platform.is_some() {
        sql.push_str(" AND r.app_platform_id = ?");
    }
    sql.push_str(" ORDER BY r.id LIMIT ?");
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str())).bind(&norm_hash);
    if let Some(p) = platform {
        query = query.bind(p.igdb_id());
    }
    let rows = query.bind(limit).fetch_all(pool).await?;
    let mut out: Vec<ReleaseCandidate> = rows.iter().map(row_to_candidate).collect::<Result<_, _>>()?;
    if out.is_empty() {
        let mut sql = format!("{CANDIDATE_SELECT} AND r.norm_name LIKE ?");
        if platform.is_some() {
            sql.push_str(" AND r.app_platform_id = ?");
        }
        sql.push_str(" ORDER BY LENGTH(r.norm_name), r.id LIMIT ?");
        let like = format!("{}%", key.replace('%', "\\%").replace('_', "\\_"));
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str())).bind(like);
        if let Some(p) = platform {
            query = query.bind(p.igdb_id());
        }
        let rows = query.bind(limit).fetch_all(pool).await?;
        out = rows.iter().map(row_to_candidate).collect::<Result<_, _>>()?;
    }
    out.dedup_by_key(|c| c.release_id);
    Ok(out)
}

/// Content/hashes for one release (what a `release_contents` row points at), for reports.
pub async fn release_contents(pool: &MySqlPool, release_id: u64) -> Result<Vec<(u64, String, Vec<(String, String)>)>, DatabaseError> {
    let rows = sqlx::query("SELECT rc.content_id, rc.file_name, h.algorithm, h.value_hex FROM release_contents rc LEFT JOIN hashes h ON h.content_id = rc.content_id WHERE rc.release_id = ? ORDER BY rc.content_id, h.algorithm")
        .bind(release_id)
        .fetch_all(pool)
        .await?;
    let mut out: Vec<(u64, String, Vec<(String, String)>)> = Vec::new();
    for row in rows {
        let content_id: u64 = row.try_get("content_id")?;
        let file_name: String = row.try_get("file_name")?;
        let algorithm: Option<String> = row.try_get("algorithm")?;
        let value: Option<String> = row.try_get("value_hex")?;
        match out.last_mut() {
            Some(last) if last.0 == content_id => {
                if let (Some(a), Some(v)) = (algorithm, value) {
                    last.2.push((a, v));
                }
            }
            _ => {
                let mut hashes = Vec::new();
                if let (Some(a), Some(v)) = (algorithm, value) {
                    hashes.push((a, v));
                }
                out.push((content_id, file_name, hashes));
            }
        }
    }
    Ok(out)
}

/// Row counts for the catalog tables, for reports/diagnostics.
pub async fn table_counts(pool: &MySqlPool) -> Result<Vec<(&'static str, u64)>, DatabaseError> {
    let mut out = Vec::new();
    for table in ["sources", "platforms", "content", "hashes", "release", "release_contents", "title", "title_releases", "dat_imports", "hasheous_lookups", "igdb_games", "screenscraper_games", "steamgriddb_games", "emuready_games", "mobygames_games", "thegamesdb_games", "retroachievements_games", "retroachievements_hashes", "title_provider_links", "release_provider_links", "media", "jobs"] {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) AS n FROM `{table}`"))).fetch_one(pool).await?;
        let n: i64 = row.try_get("n")?;
        out.push((table, n as u64));
    }
    Ok(out)
}
