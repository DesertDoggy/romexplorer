//! Edit › Name dictionary: listing, adding and removing the names a search tries in the other
//! script (`database::dictionary`). A rebuild is the `dictionary_build` job.

use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::database::dictionary;

use super::{clamp_page, like_pattern, App, Error, Result};

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DictQuery {
    /// Part of any name in a group (as written, or folded: ほげ finds ホゲ).
    pub search: Option<String>,
    /// user | catalog | series | wikidata
    pub source: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DictGroup {
    /// The group's first row: a number to select it by.
    pub id: u64,
    pub group_key: String,
    pub source: String,
    pub names: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DictPage {
    pub items: Vec<DictGroup>,
    pub total: i64,
    /// Groups per source, over the whole dictionary.
    pub sources: Vec<SourceCount>,
    /// Whether a Wikidata download is kept (a rebuild without downloading uses it).
    pub wikidata_kept: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceCount {
    pub source: String,
    pub groups: i64,
}

const SOURCES: [&str; 4] = ["user", "catalog", "series", "wikidata"];

pub async fn list(app: &App, q: &DictQuery) -> Result<DictPage> {
    let pool = &app.pool;
    let mut cond = String::from(" WHERE 1 = 1");
    let mut binds: Vec<String> = Vec::new();
    if let Some(source) = q.source.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        if !SOURCES.contains(&source) {
            return Err(Error::invalid(format!("source {source:?} must be one of {}", SOURCES.join(", "))));
        }
        cond.push_str(" AND d.source = ?");
        binds.push(source.to_string());
    }
    if let Some(s) = q.search.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        cond.push_str(" AND d.group_key IN (SELECT m.group_key FROM name_dictionary m WHERE m.term LIKE ? OR m.norm LIKE ?)");
        binds.push(like_pattern(s));
        binds.push(like_pattern(&dictionary::norm(s)));
    }
    let mut count = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!("SELECT COUNT(DISTINCT d.group_key) FROM name_dictionary d{cond}")));
    for b in &binds {
        count = count.bind(b);
    }
    let total = count.fetch_one(pool).await?;
    let (limit, offset) = clamp_page(q.limit, q.offset);
    // The user's own first, then in the order they were made.
    let mut page = sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(format!(
        "SELECT d.group_key FROM name_dictionary d{cond} GROUP BY d.group_key ORDER BY MAX(d.source = 'user') DESC, MIN(d.id) LIMIT {limit} OFFSET {offset}"
    )));
    for b in &binds {
        page = page.bind(b);
    }
    let keys = page.fetch_all(pool).await?;
    let mut items: Vec<DictGroup> = Vec::with_capacity(keys.len());
    if !keys.is_empty() {
        let marks = vec!["?"; keys.len()].join(", ");
        let mut q = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT id, group_key, term, source FROM name_dictionary WHERE group_key IN ({marks}) ORDER BY id")));
        for k in &keys {
            q = q.bind(k);
        }
        let rows = q.fetch_all(pool).await?;
        for k in &keys {
            let mine: Vec<&sqlx::mysql::MySqlRow> = rows.iter().filter(|r| r.try_get::<String, _>("group_key").ok().as_deref() == Some(k.as_str())).collect();
            let source = mine.first().map(|r| r.try_get::<String, _>("source")).transpose()?.unwrap_or_default();
            let id = mine.first().map(|r| r.try_get::<u64, _>("id")).transpose()?.unwrap_or_default();
            let names = mine.iter().map(|r| r.try_get::<String, _>("term")).collect::<std::result::Result<_, _>>()?;
            items.push(DictGroup { id, group_key: k.clone(), source, names });
        }
    }
    let sources = sqlx::query("SELECT source, COUNT(DISTINCT group_key) AS n FROM name_dictionary GROUP BY source ORDER BY source")
        .fetch_all(pool)
        .await?
        .iter()
        .map(|r| Ok(SourceCount { source: r.try_get("source")?, groups: r.try_get("n")? }))
        .collect::<std::result::Result<_, sqlx::Error>>()?;
    Ok(DictPage { items, total, sources, wikidata_kept: dictionary::wikidata_cache(&app.paths.provider_cache_dir()).is_file() })
}

#[derive(Debug, Clone, Deserialize)]
pub struct DictAdd {
    /// Two or more names for the same game or series.
    pub names: Vec<String>,
}

/// Adds a group of the user's own. Returns its key.
pub async fn add(app: &App, req: &DictAdd) -> Result<String> {
    let names: Vec<String> = req.names.iter().map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).collect();
    if names.len() < 2 {
        return Err(Error::invalid("give at least two names for the same game or series"));
    }
    if names.iter().any(|n| n.chars().count() > 255 || dictionary::norm(n).chars().count() < 2) {
        return Err(Error::invalid("each name is 2 to 255 characters (letters or digits)"));
    }
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let key = format!("user:{nanos:x}");
    for n in &names {
        sqlx::query("INSERT IGNORE INTO name_dictionary (group_key, term, norm, cjk, source) VALUES (?, ?, ?, ?, 'user')")
            .bind(&key)
            .bind(n)
            .bind(dictionary::norm(n))
            .bind(dictionary::has_cjk(n))
            .execute(&app.pool)
            .await?;
    }
    dictionary::forget();
    Ok(key)
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DictRemove {
    #[serde(default)]
    pub group_keys: Vec<String>,
    /// Groups by [`DictGroup::id`] (any row of the group does).
    #[serde(default)]
    pub ids: Vec<u64>,
}

/// Removes groups. An automatic group's names are remembered as removed, so a rebuild does
/// not bring them back. Returns how many names went.
pub async fn remove(app: &App, req: &DictRemove) -> Result<u64> {
    let mut keys = req.group_keys.clone();
    if !req.ids.is_empty() {
        let list = req.ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        keys.extend(sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(format!("SELECT DISTINCT group_key FROM name_dictionary WHERE id IN ({list})"))).fetch_all(&app.pool).await?);
    }
    if keys.is_empty() {
        return Err(Error::invalid("nothing selected"));
    }
    let mut removed = 0;
    for key in &keys {
        sqlx::query("INSERT IGNORE INTO name_dictionary_blocks (group_key, norm) SELECT group_key, norm FROM name_dictionary WHERE group_key = ? AND source <> 'user'").bind(key).execute(&app.pool).await?;
        removed += sqlx::query("DELETE FROM name_dictionary WHERE group_key = ?").bind(key).execute(&app.pool).await?.rows_affected();
    }
    dictionary::forget();
    Ok(removed)
}

/// Forgets every removal: the next rebuild brings those names back.
pub async fn clear_blocks(app: &App) -> Result<u64> {
    Ok(sqlx::query("DELETE FROM name_dictionary_blocks").execute(&app.pool).await?.rows_affected())
}

/// What a search for `q` also tries (to see the dictionary at work).
pub async fn variants(app: &App, q: &str) -> Result<Vec<String>> {
    let q = q.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    Ok(dictionary::dict(&app.pool).await?.variants(q, 6))
}
