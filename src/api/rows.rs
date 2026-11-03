//! Writing exported rows back into a table.
//!
//! The counterpart to [`super::sync`]'s `JSON_OBJECT` projection, and deliberately as
//! generic as it is: one writer serves the backup restore, the peer mirror import and
//! anything later that replays exported rows, because all three are the same operation --
//! take rows that a database projected to JSON and put them into a table.
//!
//! Like the export, the column list comes from `information_schema` at run time rather than
//! from hand-written per-table code. A column added by a later migration is carried without
//! anyone remembering to update a serialiser, and -- the case that actually bites -- a
//! backup written by an *older* build simply lacks the newer column, which here means the
//! column takes its default instead of the insert failing.

use serde::{Deserialize, Serialize};
use sqlx::Row;

use super::{App, Error, Result};

/// Roughly how many placeholders to put in one statement. MariaDB's limit is 65,535; this
/// leaves room and keeps a single statement's memory modest on wide tables.
const PLACEHOLDER_BUDGET: usize = 15_000;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WriteSummary {
    /// Rows sent. Not the same as rows created: an upsert that matches an existing row
    /// updates it, and MariaDB reports that as two affected rows, so counting what we sent
    /// is the only number that means anything definite here.
    pub rows: u64,
}

/// The columns of `table` that hold JSON (MariaDB implements the JSON type as LONGTEXT plus
/// a `json_valid()` CHECK, so this is where that shows up).
///
/// They need separate treatment on the way in: a JSON column must receive *JSON text*, so a
/// string value has to keep its quotes. Unwrapping it -- which is right for every other
/// column type -- produces text that fails the CHECK, and the failure only appears for
/// settings whose value happens to be a bare string.
pub(crate) async fn json_columns(app: &App, table: &str) -> Result<std::collections::HashSet<String>> {
    let rows = sqlx::query(
        "SELECT CONSTRAINT_NAME, CHECK_CLAUSE FROM information_schema.CHECK_CONSTRAINTS \
         WHERE CONSTRAINT_SCHEMA = DATABASE() AND TABLE_NAME = ? AND CHECK_CLAUSE LIKE '%json_valid%'",
    )
    .bind(table)
    .fetch_all(&app.pool)
    .await?;
    let mut out = std::collections::HashSet::new();
    for row in &rows {
        let clause: String = row.try_get("CHECK_CLAUSE").unwrap_or_default();
        let name: String = row.try_get("CONSTRAINT_NAME").unwrap_or_default();
        // MariaDB names the implicit constraint after the column, but confirm against the
        // clause rather than trusting the name -- a hand-written constraint may be named
        // anything.
        if !name.is_empty() && clause.contains(&format!("json_valid(`{name}`)")) {
            out.insert(name);
        }
    }
    Ok(out)
}

/// A JSON column's value as the JSON text it must be given back.
///
/// `Null` still binds as SQL NULL; everything else keeps its JSON encoding, so `false` stays
/// `false` and a string keeps its quotes.
pub(crate) fn json_column_text(value: Option<&serde_json::Value>) -> Option<String> {
    match value {
        None | Some(serde_json::Value::Null) => None,
        Some(other) => Some(other.to_string()),
    }
}

/// Roughly how many rows each of `tables` holds, for sizing a progress bar.
///
/// From `information_schema.TABLES`, which for InnoDB is an **estimate** from the optimizer's
/// statistics -- it can be out by a good margin. That is the right trade here: an exact
/// `COUNT(*)` on every table means a full index scan each, which on a ten-million-row `hashes`
/// costs real time *before* the work starts, to produce a number only a progress bar reads.
/// Callers clamp the total upward as they pass it, so an underestimate cannot make the bar go
/// backwards past 100%.
pub(crate) async fn estimated_rows(app: &App, tables: &[&str]) -> Vec<u64> {
    let mut out = vec![0u64; tables.len()];
    let rows = sqlx::query("SELECT TABLE_NAME, TABLE_ROWS FROM information_schema.TABLES WHERE TABLE_SCHEMA = DATABASE()")
        .fetch_all(&app.pool)
        .await
        .unwrap_or_default();
    for row in &rows {
        let Ok(name) = row.try_get::<String, _>("TABLE_NAME") else { continue };
        let n: u64 = row.try_get::<Option<u64>, _>("TABLE_ROWS").ok().flatten().unwrap_or(0);
        if let Some(i) = tables.iter().position(|t| *t == name) {
            out[i] = n;
        }
    }
    out
}

/// Exactly how many rows each of `tables` holds: a `COUNT(*)` apiece.
///
/// For work long enough that the estimate's error shows. On the live catalogue the optimizer
/// was under by 5-15% on the big tables (`release_contents`: 3.57M estimated, 4.10M actual),
/// which on a twelve-minute catalogue backup is a bar that reaches 100% and then keeps
/// counting for a minute or two. Counting the six biggest tables exactly took 1.7 s.
///
/// `tables` are interpolated, so they must come from this crate's constant lists. A table
/// that cannot be counted falls back to its estimate rather than failing the job.
pub(crate) async fn exact_rows(app: &App, tables: &[&str]) -> Vec<u64> {
    let mut out = estimated_rows(app, tables).await;
    for (i, table) in tables.iter().enumerate() {
        if let Ok(n) = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM `{table}`"))).fetch_one(&app.pool).await {
            out[i] = n.max(0) as u64;
        }
    }
    out
}

/// One value as the text a database will coerce into the column's own type.
///
/// Everything is bound as a string (or NULL) rather than being matched to the column's
/// declared type: MariaDB coerces `'123'` into an integer column, `'2026-01-01 00:00:00'` into
/// a DATETIME and `'1'` into a boolean, and SQLite does the same through column affinity. So
/// one code path covers every column type in the schema, in both databases. The alternative --
/// deciding a Rust type per column from `information_schema` -- is a large match that would
/// need extending with every new column type for no behavioural gain.
pub(crate) fn json_text(value: Option<&serde_json::Value>) -> Option<String> {
    match value {
        None | Some(serde_json::Value::Null) => None,
        // JSON `true` would bind as the string "true", which is not what a TINYINT(1) wants;
        // a database's own boolean literal is 1/0.
        Some(serde_json::Value::Bool(b)) => Some(if *b { "1".into() } else { "0".into() }),
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        // Numbers, and any object/array a JSON-typed column holds, round-trip as their
        // compact JSON text -- which is exactly what such a column stored to begin with.
        Some(other) => Some(other.to_string()),
    }
}

/// Writes JSON rows into one table, upserting on its primary key.
#[derive(Debug, Clone)]
pub struct Writer {
    table: String,
    /// Real, writable columns of the *target* table.
    columns: Vec<String>,
    /// Columns stamped on every row regardless of what the source sent -- `source_id` for
    /// the mirror. Applied after the source's own values, so a source cannot overwrite them.
    extra: Vec<(String, String)>,
    /// Of `columns`, the ones that must receive JSON text rather than unwrapped text.
    json: std::collections::HashSet<String>,
}

impl Writer {
    /// `table` is interpolated into SQL: **the caller must have validated it** against a
    /// fixed list, exactly as [`super::sync::export_page`] requires.
    pub async fn new(app: &App, table: &str, extra: Vec<(String, String)>) -> Result<Self> {
        // Generated columns are excluded: they have no value of their own to write, and
        // naming one in an INSERT is an error rather than a no-op. `remote_sources.main_role`
        // is one, and it is in the database backup, so this is load-bearing and not
        // hypothetical.
        let rows = sqlx::query(
            "SELECT COLUMN_NAME FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? AND (EXTRA IS NULL OR EXTRA NOT LIKE '%GENERATED%') \
             ORDER BY ORDINAL_POSITION",
        )
        .bind(table)
        .fetch_all(&app.pool)
        .await?;
        // The change number is this database's own, set by its triggers; one carried over from
        // another machine would be meaningless here (`database::sync_rev`).
        let columns: Vec<String> = rows.iter().filter_map(|r| r.try_get::<String, _>("COLUMN_NAME").ok()).filter(|c| c != crate::database::sync_rev::COLUMN).collect();
        if columns.is_empty() {
            return Err(Error::not_found(format!("no such table {table}")));
        }
        let json = json_columns(app, table).await?;
        Ok(Self { table: table.to_string(), columns, extra, json })
    }

    /// How many rows to send at once, given how wide this table is.
    pub fn batch_size(&self) -> usize {
        (PLACEHOLDER_BUDGET / self.columns.len().max(1)).max(1)
    }

    /// Upserts `rows` in as few statements as the placeholder budget allows.
    ///
    /// Takes one connection rather than the pool, and that is deliberate: a restore runs with
    /// `FOREIGN_KEY_CHECKS` off, which is *session* state. Handed a pool, successive batches
    /// would land on different connections and the setting would silently apply to some of
    /// them and not others -- the kind of failure that only shows up as a foreign-key error
    /// partway through a large restore.
    pub async fn write(&self, conn: &mut sqlx::MySqlConnection, rows: &[serde_json::Value]) -> Result<WriteSummary> {
        if rows.is_empty() {
            return Ok(WriteSummary::default());
        }
        let extra_names: Vec<&str> = self.extra.iter().map(|(k, _)| k.as_str()).collect();
        // The source's own copy of a stamped column is ignored rather than written.
        let source_columns: Vec<&String> = self.columns.iter().filter(|c| !extra_names.contains(&c.as_str())).collect();
        let all: Vec<String> = source_columns.iter().map(|c| format!("`{c}`")).chain(self.extra.iter().map(|(k, _)| format!("`{k}`"))).collect();
        let width = all.len();
        // A credential arrives as NULL from any export (`super::secrets`) and must not erase
        // the one this database already holds.
        let assignments = source_columns
            .iter()
            .map(|c| c.as_str())
            .chain(self.extra.iter().map(|(k, _)| k.as_str()))
            .map(|c| super::secrets::assignment(&self.table, c))
            .collect::<Vec<_>>()
            .join(", ");
        let placeholders = format!("({})", vec!["?"; width].join(", "));

        let mut written = 0u64;
        for chunk in rows.chunks(self.batch_size()) {
            let values = vec![placeholders.as_str(); chunk.len()].join(", ");
            let sql = format!("INSERT INTO `{}` ({}) VALUES {values} ON DUPLICATE KEY UPDATE {assignments}", self.table, all.join(", "));
            let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
            for row in chunk {
                let object = row.as_object().ok_or_else(|| Error::invalid(format!("{}: a row was not a JSON object", self.table)))?;
                for column in &source_columns {
                    let value = object.get(column.as_str());
                    query = query.bind(if self.json.contains(column.as_str()) { json_column_text(value) } else { json_text(value) });
                }
                for (_, value) in &self.extra {
                    query = query.bind(value.clone());
                }
            }
            query.execute(&mut *conn).await?;
            written += chunk.len() as u64;
        }
        Ok(WriteSummary { rows: written })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_values_become_what_mariadb_can_coerce() {
        let v = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
        assert_eq!(json_text(Some(&v("null"))), None);
        assert_eq!(json_text(None), None);
        // Not "true"/"false": a TINYINT(1) takes 1/0.
        assert_eq!(json_text(Some(&v("true"))).as_deref(), Some("1"));
        assert_eq!(json_text(Some(&v("false"))).as_deref(), Some("0"));
        assert_eq!(json_text(Some(&v("123"))).as_deref(), Some("123"));
        assert_eq!(json_text(Some(&v("1.5"))).as_deref(), Some("1.5"));
        // A string keeps its own text -- not re-quoted, which `to_string()` would do.
        assert_eq!(json_text(Some(&v(r#""hello""#))).as_deref(), Some("hello"));
        assert_eq!(json_text(Some(&v(r#""2026-01-02 03:04:05""#))).as_deref(), Some("2026-01-02 03:04:05"));
        // A JSON-typed column's contents survive as their own text.
        assert_eq!(json_text(Some(&v(r#"{"a":1}"#))).as_deref(), Some(r#"{"a":1}"#));
    }

    #[test]
    fn a_json_column_keeps_its_json_encoding() {
        let v = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
        // The bug this exists for: unwrapping a JSON string gives text that is not JSON, and
        // `json_valid()` rejects it -- but only for settings whose value is a bare string, so
        // it survives every test that happens to use numbers or objects.
        assert_eq!(json_column_text(Some(&v(r#""keep""#))).as_deref(), Some(r#""keep""#));
        assert_eq!(json_text(Some(&v(r#""keep""#))).as_deref(), Some("keep"));
        // And booleans stay booleans rather than becoming 1/0, which `get::<bool>` needs.
        assert_eq!(json_column_text(Some(&v("false"))).as_deref(), Some("false"));
        assert_eq!(json_column_text(Some(&v(r#"{"a":1}"#))).as_deref(), Some(r#"{"a":1}"#));
        assert_eq!(json_column_text(Some(&v("null"))), None);
        assert_eq!(json_column_text(None), None);
    }
}
