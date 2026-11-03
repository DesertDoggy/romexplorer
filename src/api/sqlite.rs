//! The mobile cache: the same rows as a backup or a peer export, in a SQLite file.
//!
//! A phone cannot host MariaDB (`docs/plan/gui.md`), so the mobile build is a REST client --
//! but a REST client with no local copy has to ask the server for every screen. This is that
//! local copy: one file the phone downloads once and then reads directly, and which it can
//! also import back into a full database.
//!
//! **JSON is the interchange in both directions**, which is what keeps this symmetric with
//! everything else. Going out, rows arrive from MariaDB's `JSON_OBJECT` projection
//! ([`super::sync::export_page`]) and are inserted here. Coming back in, SQLite's own
//! `json_object()` produces the identical shape and [`super::rows::Writer`] puts it into
//! MariaDB. Neither direction has a per-table serialiser to keep in step with migrations.
//!
//! **Types are carried as affinities, not as values.** Every value is bound as text, and the
//! column is *declared* INTEGER / REAL / TEXT according to what MariaDB says it is; SQLite
//! then converts on the way in. Without that, an `app_platform_id` stored as the text "5"
//! would not match `WHERE app_platform_id = 5` on the phone -- the whole point of shipping a
//! queryable file rather than a dump.

use std::path::Path;

use sqlx::Row;

use super::{App, Error, Result};

// Shared with the GUI through `romexplorer_types`.
pub use romexplorer_types::sqlite::{CacheFile, ImportSummary, SqliteSummary};

/// Where a cache records which installation produced it. Underscore-prefixed so it cannot
/// collide with a table name from the schema.
const META_TABLE: &str = "_romexplorer_meta";

/// MariaDB's declared type for a column -> the SQLite affinity that makes it behave the same
/// in a `WHERE`. Anything unrecognised is TEXT, which is SQLite's own default and never
/// silently reinterprets a value.
fn affinity(data_type: &str) -> &'static str {
    match data_type {
        "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "bigint" | "year" | "bit" => "INTEGER",
        "float" | "double" | "decimal" | "newdecimal" => "REAL",
        _ => "TEXT",
    }
}

/// The target table's shape: writable columns with their affinities, and the primary key.
async fn shape(app: &App, table: &str) -> Result<(Vec<(String, &'static str)>, Vec<String>)> {
    let rows = sqlx::query(
        "SELECT COLUMN_NAME, DATA_TYPE FROM information_schema.COLUMNS \
         WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? AND (EXTRA IS NULL OR EXTRA NOT LIKE '%GENERATED%') \
         AND COLUMN_NAME <> 'sync_rev' ORDER BY ORDINAL_POSITION",
    )
    .bind(table)
    .fetch_all(&app.pool)
    .await?;
    let columns: Vec<(String, &'static str)> = rows
        .iter()
        .filter_map(|r| {
            let name: String = r.try_get("COLUMN_NAME").ok()?;
            let ty: String = r.try_get("DATA_TYPE").ok()?;
            Some((name, affinity(&ty)))
        })
        .collect();
    if columns.is_empty() {
        return Err(Error::not_found(format!("no such table {table}")));
    }
    let key_rows = sqlx::query(
        "SELECT COLUMN_NAME FROM information_schema.STATISTICS \
         WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? AND INDEX_NAME = 'PRIMARY' ORDER BY SEQ_IN_INDEX",
    )
    .bind(table)
    .fetch_all(&app.pool)
    .await?;
    let key: Vec<String> = key_rows.iter().filter_map(|r| r.try_get::<String, _>("COLUMN_NAME").ok()).collect();
    Ok((columns, key))
}

/// Writes `tables` into a fresh SQLite file at `path`, replacing any file already there.
///
/// `tables` is interpolated into SQL: **the caller must have validated it** against a fixed
/// list, as everywhere else in this family.
pub async fn export(app: &App, tables: &[&str], path: &Path, progress: &crate::database::jobs::Progress) -> Result<SqliteSummary> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::internal(format!("creating {}: {e}", parent.display())))?;
    }
    // Written beside it as `.part` and renamed when whole: the cache already there stays
    // usable while this runs, and an export that fails or is cancelled never leaves a
    // half-written file under the real name to be shipped. The guard removes the part
    // however this ends.
    let part = path.with_file_name(format!("{}.part", path.file_name().unwrap_or_default().to_string_lossy()));
    let _ = std::fs::remove_file(&part);
    struct PartGuard(Option<std::path::PathBuf>);
    impl Drop for PartGuard {
        fn drop(&mut self) {
            if let Some(path) = self.0.take() {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    let mut guard = PartGuard(Some(part.clone()));

    let url = format!("sqlite://{}?mode=rwc", part.display());
    let out = sqlx::SqlitePool::connect(&url).await.map_err(|e| Error::internal(format!("opening {}: {e}", part.display())))?;
    // A cache is rebuilt from scratch if it is ever damaged, so durability per statement buys
    // nothing and costs a great deal on a file with millions of rows.
    sqlx::query("PRAGMA journal_mode = OFF").execute(&out).await.ok();
    sqlx::query("PRAGMA synchronous = OFF").execute(&out).await.ok();

    // Provenance, written first so it is there even if the export is interrupted. An
    // importer needs to know whose data this is before it decides what to do with it.
    let instance = crate::database::settings::instance_id(&app.pool).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE TABLE \"{META_TABLE}\" (instance_id TEXT, exported_at TEXT, schema_version INTEGER)"))).execute(&out).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO \"{META_TABLE}\" (instance_id, exported_at, schema_version) VALUES (?, ?, ?)")))
        .bind(&instance)
        .bind(chrono::Local::now().naive_local().to_string())
        .bind(crate::database::schema_version())
        .execute(&out)
        .await?;

    // Exact, not the optimizer's estimate: see `rows::exact_rows`.
    let estimate = super::rows::exact_rows(app, tables).await;
    let total_rows: u64 = estimate.iter().sum();
    let mut rows_done = 0u64;

    let mut written = Vec::new();
    for (i, table) in tables.iter().enumerate() {
        progress.force(rows_done, Some(total_rows.max(1)), &format!("table {} of {} ({table})", i + 1, tables.len())).await;
        let (columns, key) = shape(app, table).await?;
        // A JSON column is stored as its JSON *text* here, so `false` stays `false` and a
        // string keeps its quotes -- and `import` reads it back with SQLite's `json()` so
        // both databases produce the identical nested shape for the writer.
        let json_cols = super::rows::json_columns(app, table).await?;
        let defs = columns.iter().map(|(c, a)| format!("\"{c}\" {a}")).collect::<Vec<_>>().join(", ");
        let pk = if key.is_empty() { String::new() } else { format!(", PRIMARY KEY ({})", key.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(", ")) };
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE TABLE \"{table}\" ({defs}{pk})"))).execute(&out).await?;

        let names = columns.iter().map(|(c, _)| format!("\"{c}\"")).collect::<Vec<_>>().join(", ");
        let placeholders = format!("({})", vec!["?"; columns.len()].join(", "));
        // SQLite's own variable limit is 32,766 in modern builds; stay well inside it.
        let batch = (20_000 / columns.len().max(1)).max(1);

        let mut rows = 0u64;
        let mut after: Option<Vec<String>> = None;
        loop {
            let page = super::sync::export_page(app, table, after.as_deref(), 2_000).await?;
            for chunk in page.rows.chunks(batch) {
                let values = vec![placeholders.as_str(); chunk.len()].join(", ");
                let mut query = sqlx::query(sqlx::AssertSqlSafe(format!("INSERT OR REPLACE INTO \"{table}\" ({names}) VALUES {values}")));
                for row in chunk {
                    let object = row.as_object().ok_or_else(|| Error::invalid(format!("{table}: a row was not a JSON object")))?;
                    for (column, _) in &columns {
                        let value = object.get(column.as_str());
                        query = query.bind(if json_cols.contains(column.as_str()) {
                            super::rows::json_column_text(value)
                        } else {
                            super::rows::json_text(value)
                        });
                    }
                }
                query.execute(&out).await?;
                rows += chunk.len() as u64;
                rows_done += chunk.len() as u64;
            }
            progress
                .report(
                    rows_done,
                    Some(total_rows.max(rows_done).max(1)),
                    &format!("table {} of {} ({table}) {rows}/~{} -- {rows_done}/~{total_rows} overall", i + 1, tables.len(), estimate[i]),
                )
                .await;
            match page.next {
                Some(cursor) => after = Some(cursor),
                None => break,
            }
        }
        written.push((table.to_string(), rows));
    }
    out.close().await;
    std::fs::rename(&part, path).map_err(|e| Error::internal(format!("renaming {} to {}: {e}", part.display(), path.display())))?;
    guard.0 = None;

    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    Ok(SqliteSummary { file: path.file_name().unwrap_or_default().to_string_lossy().into_owned(), bytes, tables: written })
}

/// The tables a SQLite cache actually contains, in the order they were created.
pub async fn tables_in(path: &Path) -> Result<Vec<String>> {
    if !path.is_file() {
        return Err(Error::not_found(format!("no such file {}", path.display())));
    }
    let url = format!("sqlite://{}?mode=ro", path.display());
    let db = sqlx::SqlitePool::connect(&url).await.map_err(|e| Error::invalid(format!("not a readable SQLite file: {e}")))?;
    let rows = sqlx::query("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY rootpage").fetch_all(&db).await?;
    let names = rows.iter().filter_map(|r| r.try_get::<String, _>("name").ok()).collect();
    db.close().await;
    Ok(names)
}

/// What the file says about where it came from.
async fn meta_instance(db: &sqlx::SqlitePool) -> Option<String> {
    let exists: Option<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT name FROM sqlite_master WHERE type='table' AND name='{META_TABLE}'")))
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
    exists?;
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT instance_id FROM \"{META_TABLE}\" LIMIT 1"))).fetch_optional(db).await.ok().flatten()
}

/// Reads a SQLite cache and upserts it into this machine's database.
///
/// `allowed` is the whitelist -- a file is data from elsewhere, so the table names in it
/// decide nothing; anything not on the list is reported as skipped rather than written.
///
/// **Ownership from another installation is declined unless `adopt_ownership` is set.** The
/// catalogue is shared by construction -- `content`, `release`, `title` mean the same thing
/// everywhere, so importing them is always right. The `local_*` tables are not: they say
/// *this machine holds these files*, and writing another machine's copy of them makes this
/// database claim files it does not have. That is correct for a phone, which exists to be a
/// view of the collection, and wrong for a desktop, which should mirror a peer into
/// `remote_*` via [`super::sync::pull`] instead. Since both use the same file format, the
/// file's own `instance_id` is what tells them apart, and the safe reading is the default.
pub async fn import(app: &App, path: &Path, allowed: &[&str], adopt_ownership: bool, progress: &crate::database::jobs::Progress) -> Result<ImportSummary> {
    let present = tables_in(path).await?;
    let url = format!("sqlite://{}?mode=ro", path.display());
    let db = sqlx::SqlitePool::connect(&url).await.map_err(|e| Error::invalid(format!("not a readable SQLite file: {e}")))?;

    let mut summary = ImportSummary { from_instance: meta_instance(&db).await, ..Default::default() };
    let ours = crate::database::settings::Identity::capture(&app.pool).await?;
    summary.from_self = summary.from_instance.as_deref() == Some(ours.instance_id.as_str());
    // A file with no metadata predates this or came from elsewhere; either way it is not
    // demonstrably ours, so it gets the cautious treatment.
    let take_ownership = adopt_ownership || summary.from_self;

    let mut wanted: Vec<&String> = Vec::new();
    for table in &present {
        if table == META_TABLE {
            continue;
        }
        if !allowed.contains(&table.as_str()) {
            summary.skipped.push(table.clone());
        } else if !take_ownership && super::backup::DATABASE_TABLES.contains(&table.as_str()) {
            summary.declined_ownership.push(table.clone());
        } else {
            wanted.push(table);
        }
    }

    // One connection for the whole import: foreign-key checks are session state (see
    // `rows::Writer::write`), and a cache's tables arrive in whatever order the file holds
    // them, which need not be dependency order.
    let mut conn = app.pool.acquire().await?;
    // Closed rather than returned to the pool if this is cut short (a cancel, a dropped
    // request): FOREIGN_KEY_CHECKS is per connection and must never be left off on one.
    conn.close_on_drop();
    sqlx::query("SET FOREIGN_KEY_CHECKS = 0").execute(&mut *conn).await?;

    // Everything between here and restoring the setting is wrapped, so that an error cannot
    // hand this connection back to the pool with foreign-key checks still off -- which would
    // silently disable them for whatever unrelated query picked it up next.
    // Counted before writing anything. SQLite keeps no row count, but `COUNT(*)` on a local
    // file is a cheap index scan, and it is what turns the bar from "table 7 of 77" -- which
    // stalls for most of the import inside `hashes` -- into a real fraction.
    let mut sizes: Vec<u64> = Vec::with_capacity(wanted.len());
    for table in &wanted {
        let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM \"{table}\""))).fetch_one(&db).await.unwrap_or(0);
        sizes.push(n as u64);
    }
    let total_rows: u64 = sizes.iter().sum();
    let mut rows_done = 0u64;

    let result = async {
        for (i, table) in wanted.iter().enumerate() {
            progress.force(rows_done, Some(total_rows.max(1)), &format!("table {} of {} ({table})", i + 1, wanted.len())).await;
            let writer = super::rows::Writer::new(app, table, Vec::new()).await?;
            let columns = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT name FROM pragma_table_info('{table}')"))).fetch_all(&db).await?;
            let names: Vec<String> = columns.iter().filter_map(|r| r.try_get::<String, _>("name").ok()).collect();
            if names.is_empty() {
                continue;
            }
            // SQLite's json_object() mirrors MariaDB's JSON_OBJECT, so the writer sees exactly
            // the shape it sees from a backup or a peer. A JSON column is wrapped in `json()`
            // so SQLite embeds it as nested JSON instead of as a string containing JSON --
            // which is exactly what MariaDB's JSON_OBJECT does with the same column, and is
            // what makes one writer correct for both sources.
            let json_cols = super::rows::json_columns(app, table).await?;
            let projection = names
                .iter()
                .map(|c| if json_cols.contains(c.as_str()) { format!("'{c}', json(\"{c}\")") } else { format!("'{c}', \"{c}\"") })
                .collect::<Vec<_>>()
                .join(", ");

            // Paged by rowid rather than read whole: `hashes` alone is ~10M rows on a real
            // collection, and pulling a table into one Vec before writing any of it would
            // scale memory with the table rather than with the batch. rowid is the right
            // cursor here -- every table this exports has one, and it is already the index
            // a sequential scan follows.
            let batch_size = writer.batch_size().max(500);
            let mut after: i64 = 0;
            let mut count = 0u64;
            loop {
                let sql = format!("SELECT rowid AS _rid, json_object({projection}) AS row_json FROM \"{table}\" WHERE rowid > ? ORDER BY rowid LIMIT {batch_size}");
                let page = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(after).fetch_all(&db).await?;
                if page.is_empty() {
                    break;
                }
                let mut batch: Vec<serde_json::Value> = Vec::with_capacity(page.len());
                for row in &page {
                    after = row.try_get("_rid")?;
                    let text: String = row.try_get("row_json")?;
                    batch.push(serde_json::from_str(&text).map_err(|e| Error::invalid(format!("{table}: {e}")))?);
                }
                let n = writer.write(&mut conn, &batch).await?.rows;
                count += n;
                rows_done += n;
                progress
                    .report(
                        rows_done,
                        Some(total_rows.max(1)),
                        &format!("table {} of {} ({table}) {count}/{} -- {rows_done}/{total_rows} overall", i + 1, wanted.len(), sizes[i]),
                    )
                    .await;
            }
            summary.tables.push((table.to_string(), count));
        }
        Ok::<(), Error>(())
    }
    .await;

    db.close().await;
    sqlx::query("SET FOREIGN_KEY_CHECKS = 1").execute(&mut *conn).await?;
    // `settings` was just written wholesale; put this installation's own identity back
    // (`settings::Identity` says why all three rows, not just the id).
    ours.restore(&mut conn).await?;
    result?;
    Ok(summary)
}

/// Where the mobile cache is written, and under what name: beside the backups (the same
/// directory `backup::directory` resolves, so the `backup_dir` setting moves both), because it
/// is the same kind of artefact -- a file you move to another machine.
pub async fn cache_path(app: &App, kind: &str) -> Result<std::path::PathBuf> {
    Ok(super::backup::directory(app).await?.join(format!("{CACHE_PREFIX}{kind}-{}.sqlite", chrono::Local::now().format("%Y%m%d-%H%M%S"))))
}

const CACHE_PREFIX: &str = "romexplorer-cache-";

/// Which kind a file on disk is, or `None` if it is not a cache of ours. Backups and caches
/// share a directory, and `backup::list` ignores these the same way this ignores those.
fn cache_kind_of(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_suffix(".sqlite")?.strip_prefix(CACHE_PREFIX)?;
    // `<kind>-<yyyymmdd>-<hhmmss>`: the kind is everything before the timestamp.
    let kind = stem.rsplitn(3, '-').nth(2)?;
    tables_for(kind).ok().map(|_| kind.to_string())
}

/// Every cache in the backup directory, newest first.
pub async fn list(app: &App) -> Result<Vec<CacheFile>> {
    let dir = super::backup::directory(app).await?;
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else { return Ok(out) };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(kind) = cache_kind_of(&path) else { continue };
        let meta = entry.metadata().map_err(|e| Error::internal(e.to_string()))?;
        let created = meta.modified().ok().map(|t| chrono::DateTime::<chrono::Local>::from(t).naive_local()).unwrap_or_default();
        out.push(CacheFile { name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(), kind, bytes: meta.len(), created });
    }
    out.sort_by(|a, b| b.created.cmp(&a.created));
    Ok(out)
}

/// The path of one cache, checked to be a cache in the backup directory -- `name` comes from
/// a request, so it must not be able to name a file elsewhere.
pub async fn file(app: &App, name: &str) -> Result<std::path::PathBuf> {
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(Error::invalid("bad cache name"));
    }
    let path = super::backup::directory(app).await?.join(name);
    if cache_kind_of(&path).is_none() || !path.is_file() {
        return Err(Error::not_found(format!("no cache {name}")));
    }
    Ok(path)
}

/// Deletes one cache. Same name checking as [`file`].
pub async fn remove(app: &App, name: &str) -> Result<()> {
    let path = file(app, name).await?;
    std::fs::remove_file(&path).map_err(|e| Error::internal(format!("delete {}: {e}", path.display())))
}

/// `database` | `catalog` | `all` -> the tables that kind covers.
pub fn tables_for(kind: &str) -> Result<Vec<&'static str>> {
    match kind {
        "database" => Ok(super::backup::DATABASE_TABLES.to_vec()),
        "catalog" => Ok(super::backup::CATALOG_TABLES.to_vec()),
        // The default for a phone: it browses the catalogue *and* wants to see what is held,
        // so a cache of only one half would not render a single screen properly.
        "all" => Ok(super::backup::CATALOG_TABLES.iter().copied().chain(super::backup::DATABASE_TABLES.iter().copied()).collect()),
        other => Err(Error::invalid(format!("kind must be database, catalog or all (not {other:?})"))),
    }
}

/// [`export`] for a named kind, writing into the backup directory.
pub async fn export_kind(app: &App, kind: &str, progress: &crate::database::jobs::Progress) -> Result<SqliteSummary> {
    let tables = tables_for(kind)?;
    export(app, &tables, &cache_path(app, kind).await?, progress).await
}

/// [`import`] for a file named by a request. The name is checked the same way a backup's is:
/// it must sit in the backup directory and be one of ours.
pub async fn import_file(app: &App, name: &str, adopt_ownership: bool, progress: &crate::database::jobs::Progress) -> Result<ImportSummary> {
    let path = file(app, name).await?;
    let allowed: Vec<&str> = tables_for("all")?;
    import(app, &path, &allowed, adopt_ownership, progress).await
}

#[cfg(test)]
mod tests {
    use super::{affinity, cache_kind_of};
    use crate::api::backup::{CATALOG_TABLES, DATABASE_TABLES};
    use std::path::Path;

    /// The kind is read back out of the file name, so the name format and the parser have to
    /// agree -- and a backup zip in the same directory must not be taken for a cache.
    #[test]
    fn cache_names_round_trip_their_kind() {
        assert_eq!(cache_kind_of(Path::new("/x/romexplorer-cache-all-20260928-162136.sqlite")).as_deref(), Some("all"));
        assert_eq!(cache_kind_of(Path::new("romexplorer-cache-database-20260928-162136.sqlite")).as_deref(), Some("database"));
        assert_eq!(cache_kind_of(Path::new("romexplorer-cache-catalog-20260928-162136.sqlite")).as_deref(), Some("catalog"));
        assert_eq!(cache_kind_of(Path::new("romexplorer-catalog-20260928-152845.zip")), None, "a backup, not a cache");
        assert_eq!(cache_kind_of(Path::new("romexplorer-cache-banana-20260928-162136.sqlite")), None, "unknown kind");
        assert_eq!(cache_kind_of(Path::new("romexplorer-cache-all.sqlite")), None, "no timestamp");
        assert_eq!(cache_kind_of(Path::new("notes.sqlite")), None);
    }

    #[test]
    fn affinities_keep_numbers_comparable_on_the_phone() {
        // The case that matters: an id stored with TEXT affinity would not match `= 5`.
        assert_eq!(affinity("bigint"), "INTEGER");
        assert_eq!(affinity("smallint"), "INTEGER");
        assert_eq!(affinity("tinyint"), "INTEGER");
        assert_eq!(affinity("double"), "REAL");
        assert_eq!(affinity("varchar"), "TEXT");
        assert_eq!(affinity("datetime"), "TEXT");
        assert_eq!(affinity("enum"), "TEXT");
        // Unknown types fall back to TEXT rather than guessing.
        assert_eq!(affinity("geometry"), "TEXT");
    }

    /// The guard declines exactly the ownership half and nothing else.
    ///
    /// It tests membership of `DATABASE_TABLES` alone, which is only correct because the two
    /// lists are disjoint (asserted in `api::backup`). If that ever stopped holding, a
    /// catalogue table could be declined -- which would not fail loudly, it would quietly
    /// hand a phone a cache that renders nothing.
    #[test]
    fn the_ownership_guard_declines_ownership_and_never_the_catalogue() {
        // Claiming these is claiming to hold files this machine does not have.
        for t in ["local_files", "local_rom", "local_game", "local_release_games", "local_directories"] {
            assert!(DATABASE_TABLES.contains(&t), "{t} must be guarded");
        }
        // These must always come through, or the cache is useless.
        for t in ["content", "release", "title", "hashes", "release_contents"] {
            assert!(CATALOG_TABLES.contains(&t), "{t} should be catalogue");
            assert!(!DATABASE_TABLES.contains(&t), "{t} must never be declined");
        }
    }
}
