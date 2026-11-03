//! Generic, read-only database viewer: every table in the schema, its columns, and paged
//! rows with server-side sort and substring filter. Table and column names are only ever
//! taken from `information_schema`, never from the request, so a request can't name
//! anything the schema doesn't have.

use serde::{Deserialize, Serialize};
use sqlx::Row;

use super::{clamp_page, like_pattern, App, Error, Result};

#[derive(Debug, Clone, Serialize)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TableCount {
    pub table: String,
    pub rows: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TableInfo {
    pub name: String,
    pub rows: u64,
    pub columns: Vec<ColumnInfo>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RowsQuery {
    pub sort: Option<String>,
    #[serde(default)]
    pub desc: bool,
    /// Substring matched against every column (or just `column` when given).
    pub filter: Option<String>,
    pub column: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RowsPage {
    pub table: String,
    pub columns: Vec<String>,
    /// Every cell cast to text server-side (`CAST(col AS CHAR)`), `null` when NULL.
    pub rows: Vec<Vec<Option<String>>>,
    pub total: u64,
    pub limit: u32,
    pub offset: u32,
}

pub async fn table_names(app: &App) -> Result<Vec<String>> {
    let rows = sqlx::query("SELECT TABLE_NAME AS t FROM information_schema.TABLES WHERE TABLE_SCHEMA = DATABASE() AND TABLE_TYPE = 'BASE TABLE' ORDER BY TABLE_NAME").fetch_all(&app.pool).await?;
    rows.iter().map(|r| Ok(r.try_get::<String, _>("t")?)).collect()
}

pub async fn columns(app: &App, table: &str) -> Result<Vec<ColumnInfo>> {
    let rows = sqlx::query("SELECT COLUMN_NAME AS c, DATA_TYPE AS d, IS_NULLABLE AS n FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? AND COLUMN_NAME <> 'sync_rev' ORDER BY ORDINAL_POSITION")
        .bind(table)
        .fetch_all(&app.pool)
        .await?;
    if rows.is_empty() {
        return Err(Error::not_found(format!("no table {table:?}")));
    }
    rows.iter()
        .map(|r| {
            let nullable: String = r.try_get("n")?;
            Ok(ColumnInfo { name: r.try_get("c")?, data_type: r.try_get("d")?, nullable: nullable.eq_ignore_ascii_case("YES") })
        })
        .collect()
}

async fn count_rows(app: &App, table: &str, where_sql: &str, binds: &[String]) -> Result<u64> {
    let mut q = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) AS n FROM `{table}`{where_sql}")));
    for b in binds {
        q = q.bind(b);
    }
    let n: i64 = q.fetch_one(&app.pool).await?.try_get("n")?;
    Ok(n as u64)
}

/// How long a set of whole-table counts is reused. Long enough that opening the console,
/// reloading it and moving between Status and Data > DB count once; short enough that a
/// number on screen is never far behind an import that is running.
const COUNTS_TTL: std::time::Duration = std::time::Duration::from_secs(30);
/// Counts in flight at once. Each one holds a pool connection for as long as it runs, and
/// the rest of the app must keep getting connections while a dashboard loads.
const COUNTS_AT_ONCE: usize = 4;

type CountsCache = std::sync::Mutex<Option<(String, std::time::Instant, Vec<TableCount>)>>;
static COUNTS: CountsCache = std::sync::Mutex::new(None);

/// Row counts for every table.
///
/// These are exact `COUNT(*)`s, and InnoDB answers one by walking an index: on a catalogue
/// of a few million rows the big tables take seconds each. Run one after another on every
/// load of the Status page -- the console's landing page -- that was the whole of its
/// "several seconds to ten or more" start-up (a single count was logged at 4.6 s on a
/// server that was also importing). So they run [`COUNTS_AT_ONCE`] at a time, which makes a
/// load cost the slowest few tables instead of the sum of all of them, and the result is
/// kept for [`COUNTS_TTL`], which makes every load after the first cost nothing.
///
/// Exact rather than `information_schema.TABLES.TABLE_ROWS`: that is free, but it is an
/// estimate that is routinely tens of percent out for InnoDB, on a page whose job is to say
/// how many rows there are.
pub async fn table_counts(app: &App) -> Result<Vec<TableCount>> {
    // One process can serve more than one database (the tests do), so the cache is one
    // database's, not the process's.
    let key = format!("{}:{}/{}", app.config.host, app.config.port, app.config.database);
    if let Some((cached_key, at, counts)) = COUNTS.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
        if *cached_key == key && at.elapsed() < COUNTS_TTL {
            return Ok(counts.clone());
        }
    }
    let names = table_names(app).await?;
    let mut out = Vec::with_capacity(names.len());
    for batch in names.chunks(COUNTS_AT_ONCE) {
        let mut running = tokio::task::JoinSet::new();
        for (slot, table) in batch.iter().enumerate() {
            let pool = app.pool.clone();
            let table = table.clone();
            running.spawn(async move {
                let rows: i64 = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) AS n FROM `{table}`"))).fetch_one(&pool).await?.try_get("n")?;
                Ok::<_, sqlx::Error>((slot, TableCount { table, rows: rows as u64 }))
            });
        }
        let mut counted: Vec<(usize, TableCount)> = Vec::with_capacity(batch.len());
        while let Some(joined) = running.join_next().await {
            counted.push(joined.map_err(|err| Error::internal(format!("counting rows: {err}")))??);
        }
        // Back into table-name order: the batch finished in whatever order it liked.
        counted.sort_by_key(|(slot, _)| *slot);
        out.extend(counted.into_iter().map(|(_, count)| count));
    }
    *COUNTS.lock().unwrap_or_else(|p| p.into_inner()) = Some((key, std::time::Instant::now(), out.clone()));
    Ok(out)
}

pub async fn tables(app: &App) -> Result<Vec<TableInfo>> {
    let mut out = Vec::new();
    for count in table_counts(app).await? {
        let columns = columns(app, &count.table).await?;
        out.push(TableInfo { name: count.table, rows: count.rows, columns });
    }
    Ok(out)
}

pub async fn rows(app: &App, table: &str, q: &RowsQuery) -> Result<RowsPage> {
    if !table_names(app).await?.iter().any(|t| t == table) {
        return Err(Error::not_found(format!("no table {table:?}")));
    }
    let columns = columns(app, table).await?;
    let names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
    let (limit, offset) = clamp_page(q.limit, q.offset);

    let mut where_sql = String::new();
    let mut binds: Vec<String> = Vec::new();
    if let Some(filter) = q.filter.as_deref().filter(|f| !f.trim().is_empty()) {
        let pattern = like_pattern(filter.trim());
        let targets: Vec<&String> = match q.column.as_deref() {
            Some(col) => {
                let Some(found) = names.iter().find(|n| *n == col) else { return Err(Error::invalid(format!("no column {col:?} in {table}"))) };
                // A substring filter on a credential answers "does it start with a?", so it
                // would give the value away one character at a time.
                if super::secrets::is_secret(table, found) {
                    return Err(Error::invalid(format!("{table}.{found} holds a credential and can't be filtered on")));
                }
                vec![found]
            }
            None => names.iter().filter(|n| !super::secrets::is_secret(table, n)).collect(),
        };
        let clauses: Vec<String> = targets.iter().map(|c| format!("CAST(`{c}` AS CHAR) LIKE ?")).collect();
        where_sql = format!(" WHERE {}", clauses.join(" OR "));
        binds.extend(vec![pattern; targets.len()]);
    }

    let order_sql = match q.sort.as_deref() {
        Some(sort) => {
            if !names.iter().any(|n| n == sort) {
                return Err(Error::invalid(format!("no column {sort:?} in {table}")));
            }
            // Sorting on a credential leaks how it orders against the others.
            if super::secrets::is_secret(table, sort) {
                return Err(Error::invalid(format!("{table}.{sort} holds a credential and can't be sorted on")));
            }
            format!(" ORDER BY `{sort}` {}", if q.desc { "DESC" } else { "ASC" })
        }
        None => String::new(),
    };

    let total = count_rows(app, table, &where_sql, &binds).await?;
    let select: Vec<String> = names.iter().map(|c| super::secrets::viewer_select(table, c)).collect();
    let sql = format!("SELECT {} FROM `{table}`{where_sql}{order_sql} LIMIT ? OFFSET ?", select.join(", "));
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    for b in &binds {
        query = query.bind(b);
    }
    let fetched = query.bind(limit).bind(offset).fetch_all(&app.pool).await?;
    let mut rows = Vec::with_capacity(fetched.len());
    for row in &fetched {
        let mut cells = Vec::with_capacity(names.len());
        for i in 0..names.len() {
            cells.push(row.try_get::<Option<String>, _>(i)?);
        }
        rows.push(cells);
    }
    Ok(RowsPage { table: table.to_string(), columns: names, rows, total, limit, offset })
}

/// What a `db_optimize` run would permanently delete, counted with the same predicates
/// `worker::db_optimize` deletes with -- one `COUNT(*)` per `DELETE`, in the same order.
///
/// Kept next to the viewer rather than in `jobs` because it exists for one purpose: letting
/// the UI tell the user what they are about to lose. `missing_found` is the number that
/// matters most and the one a bare row count hides -- see its field comment.
#[derive(Debug, Clone, Default, Serialize)]
pub struct OptimizePreview {
    /// Finished jobs older than 30 days.
    pub jobs: u64,
    pub files: u64,
    pub roms: u64,
    pub games: u64,
    pub hashes: u64,
    pub contents: u64,
    /// Soft-deleted files whose bytes a live file still holds somewhere on this machine --
    /// exactly the rows behind the "missing, but found elsewhere" hint (`catalog`'s
    /// `missing_found` / `found_elsewhere`). Purging destroys that trail, so a file the user
    /// moved or renamed stops being recognised as the same file.
    pub missing_found: u64,
    /// Every row above except `missing_found`, which is a subset of `files`.
    pub total: u64,
}

async fn count(app: &App, sql: &str) -> Result<u64> {
    let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string())).fetch_one(&app.pool).await?;
    Ok(n as u64)
}

pub async fn optimize_preview(app: &App) -> Result<OptimizePreview> {
    let mut p = OptimizePreview {
        jobs: count(app, "SELECT COUNT(*) FROM jobs WHERE status IN ('done','failed') AND updated_at < NOW() - INTERVAL 30 DAY").await?,
        files: count(app, "SELECT COUNT(*) FROM local_files WHERE deleted = TRUE").await?,
        roms: count(app, "SELECT COUNT(*) FROM local_rom WHERE deleted = TRUE AND NOT EXISTS (SELECT 1 FROM local_rom_files rf JOIN local_files f ON f.id = rf.file_id WHERE rf.rom_id = local_rom.id AND f.deleted = FALSE)").await?,
        games: count(app, "SELECT COUNT(*) FROM local_game WHERE deleted = TRUE AND NOT EXISTS (SELECT 1 FROM local_game_roms gr JOIN local_rom r ON r.id = gr.rom_id WHERE gr.game_id = local_game.id AND r.deleted = FALSE)").await?,
        hashes: count(app, "SELECT COUNT(*) FROM hashes h JOIN content c ON c.id = h.content_id WHERE c.deleted = TRUE AND NOT EXISTS (SELECT 1 FROM local_files f WHERE f.content_id = c.id AND f.deleted = FALSE) AND NOT EXISTS (SELECT 1 FROM release_contents rc WHERE rc.content_id = c.id)").await?,
        contents: count(app, "SELECT COUNT(*) FROM content WHERE deleted = TRUE AND NOT EXISTS (SELECT 1 FROM local_files f WHERE f.content_id = content.id AND f.deleted = FALSE) AND NOT EXISTS (SELECT 1 FROM release_contents rc WHERE rc.content_id = content.id)").await?,
        missing_found: count(app, "SELECT COUNT(*) FROM local_files d WHERE d.deleted = TRUE AND EXISTS (SELECT 1 FROM local_files l WHERE l.content_id = d.content_id AND l.deleted = FALSE AND l.id <> d.id)").await?,
        total: 0,
    };
    p.total = p.jobs + p.files + p.roms + p.games + p.hashes + p.contents;
    Ok(p)
}
