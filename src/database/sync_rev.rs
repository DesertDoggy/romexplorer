//! Row-level change numbers for the peer sync (`api::sync`).
//!
//! Before this, a client compared one checksum per table and downloaded the *whole* table
//! again whenever any row in it had changed. On a main server something touches almost every
//! large table between two syncs -- a DAT sync, a metadata fetch, a scan -- so an update sync
//! cost as much as the first one.
//!
//! Now every synced table carries an invisible `sync_rev` column. Triggers give a row the next
//! number from one shared sequence (`sync_rev_seq`) when it is inserted or when one of its
//! counted values really changes, and a hard delete leaves a tombstone in `sync_tombstones`.
//! A client remembers, per table, the highest number it has received and asks only for rows
//! (and tombstones) above it. The server keeps nothing per client: it never writes during a
//! sync, and any number of clients work the same way.
//!
//! The columns and triggers are made here, from the live schema, rather than in a migration:
//! a trigger names every column it compares, and one written by hand would silently stop
//! counting a column added by a later migration. [`ensure`] runs at every start and only
//! touches what differs.

use std::collections::{BTreeMap, HashMap};

use sqlx::{MySqlPool, Row};

use super::error::DatabaseError;

/// The column every tracked table carries. INVISIBLE, so `SELECT *` and column-less
/// `INSERT ... VALUES` elsewhere in the app behave exactly as before.
pub const COLUMN: &str = "sync_rev";

/// Every trigger made here starts with this, and nothing else may: [`ensure`] drops any
/// trigger with this prefix it did not generate.
const PREFIX: &str = "sync_rev_";

/// How long a tombstone is kept. A client that has not synced for longer than this gets the
/// affected tables in full instead of as a diff.
pub const TOMBSTONE_DAYS: u32 = 30;

/// Columns whose change alone does not make a row worth sending again. The new value still
/// travels the next time the row changes for another reason.
///
/// These are the "when was this last looked at" stamps: an integrity check or a full scan
/// rewrites them on every file in the collection, and counting them would make every client
/// download all of `local_files` again after each one -- the very cost this module exists to
/// remove. A *failed* check (`last_integrity_failure`) is a real change and is counted.
pub const UNCOUNTED: &[(&str, &str)] = &[("local_files", "last_integrity_check"), ("local_files", "last_full_check")];

fn uncounted(table: &str, column: &str) -> bool {
    UNCOUNTED.iter().any(|(t, c)| *t == table && *c == column)
}

/// The trigger statements for one table, keyed by trigger name.
type Triggers = BTreeMap<String, (String, String)>;

/// Adds the column and (re)creates the triggers on every table in `tables`, then prunes old
/// tombstones. Idempotent and cheap when nothing changed: the generated trigger bodies are
/// compared with the stored ones and only differences are written.
///
/// The first run on an existing database adds a column and an index to each table, which on
/// a large `hashes` takes a while once. Existing rows keep number 0: a client that already
/// holds them needs nothing, and one that does not gets the table in full anyway.
pub async fn ensure(pool: &MySqlPool, tables: &[&str]) -> Result<(), DatabaseError> {
    let present: Vec<String> = sqlx::query_scalar("SELECT TABLE_NAME FROM information_schema.TABLES WHERE TABLE_SCHEMA = DATABASE() AND TABLE_TYPE = 'BASE TABLE'").fetch_all(pool).await?;
    let tables: Vec<&str> = tables.iter().copied().filter(|t| present.iter().any(|p| p == t)).collect();

    let with_column: Vec<String> = sqlx::query_scalar("SELECT TABLE_NAME FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = DATABASE() AND COLUMN_NAME = ?")
        .bind(COLUMN)
        .fetch_all(pool)
        .await?;
    for table in &tables {
        if with_column.iter().any(|t| t == table) {
            continue;
        }
        log::info!("sync: adding change numbers to {table}");
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "ALTER TABLE `{table}` ADD COLUMN `{COLUMN}` BIGINT UNSIGNED NOT NULL DEFAULT 0 INVISIBLE, ADD KEY `idx_{COLUMN}` (`{COLUMN}`)"
        )))
        .execute(pool)
        .await?;
    }

    let wanted = triggers(pool, &tables).await?;
    let existing: HashMap<String, (String, String)> = sqlx::query("SELECT TRIGGER_NAME, EVENT_OBJECT_TABLE, ACTION_STATEMENT FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = DATABASE() AND TRIGGER_NAME LIKE 'sync\\_rev\\_%'")
        .fetch_all(pool)
        .await?
        .iter()
        .filter_map(|r| Some((r.try_get::<String, _>(0).ok()?, (r.try_get::<String, _>(1).ok()?, r.try_get::<String, _>(2).ok()?))))
        .collect();
    for (name, (table, _)) in &existing {
        if !wanted.contains_key(name) {
            log::info!("sync: dropping trigger {name} on {table}");
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP TRIGGER IF EXISTS `{name}`"))).execute(pool).await?;
        }
    }
    let mut written = 0usize;
    for (name, (create, body)) in &wanted {
        if existing.get(name).is_some_and(|(_, stored)| stored.trim() == body.trim()) {
            continue;
        }
        sqlx::raw_sql(sqlx::AssertSqlSafe(create.clone())).execute(pool).await?;
        written += 1;
    }
    if written > 0 {
        log::info!("sync: wrote {written} change-tracking trigger(s)");
    }
    prune(pool).await?;
    Ok(())
}

/// One table's columns as the triggers need them.
struct Columns {
    /// Compared by the update trigger, each with whether it holds text.
    counted: Vec<(String, bool)>,
    /// The primary key, in order.
    key: Vec<String>,
}

async fn columns(pool: &MySqlPool, tables: &[&str]) -> Result<HashMap<String, Columns>, DatabaseError> {
    let mut out: HashMap<String, Columns> = tables.iter().map(|t| (t.to_string(), Columns { counted: Vec::new(), key: Vec::new() })).collect();
    let rows = sqlx::query("SELECT TABLE_NAME, COLUMN_NAME, EXTRA, DATA_TYPE FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = DATABASE() ORDER BY TABLE_NAME, ORDINAL_POSITION").fetch_all(pool).await?;
    for row in &rows {
        let table: String = row.try_get(0)?;
        let Some(entry) = out.get_mut(&table) else { continue };
        let column: String = row.try_get(1)?;
        let extra: String = row.try_get::<Option<String>, _>(2)?.unwrap_or_default().to_ascii_lowercase();
        // Generated columns follow the others; an `ON UPDATE CURRENT_TIMESTAMP` stamp moves
        // only when another column did, or when something merely touched the row.
        if column == COLUMN || uncounted(&table, &column) || extra.contains("generated") || extra.contains("on update") {
            continue;
        }
        let data_type: String = row.try_get::<Option<String>, _>(3)?.unwrap_or_default().to_ascii_lowercase();
        let text = matches!(data_type.as_str(), "char" | "varchar" | "tinytext" | "text" | "mediumtext" | "longtext" | "enum" | "set" | "json");
        entry.counted.push((column, text));
    }
    let rows = sqlx::query("SELECT TABLE_NAME, COLUMN_NAME FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = DATABASE() AND INDEX_NAME = 'PRIMARY' ORDER BY TABLE_NAME, SEQ_IN_INDEX").fetch_all(pool).await?;
    for row in &rows {
        let table: String = row.try_get(0)?;
        if let Some(entry) = out.get_mut(&table) {
            entry.key.push(row.try_get(1)?);
        }
    }
    Ok(out)
}

/// NULL-safe equality for the update trigger. Text is compared byte for byte: the column's own
/// collation is case-insensitive, and `Game` renamed to `GAME` is a change a client must see.
fn same_value(column: &str, text: bool) -> String {
    if text {
        format!("CAST(NEW.`{column}` AS BINARY) <=> CAST(OLD.`{column}` AS BINARY)")
    } else {
        format!("NEW.`{column}` <=> OLD.`{column}`")
    }
}

/// The primary key of `OLD` as the JSON array a tombstone stores: text values, key order.
fn key_json(alias: &str, key: &[String]) -> String {
    format!("JSON_ARRAY({})", key.iter().map(|c| format!("CAST({alias}.`{c}` AS CHAR)")).collect::<Vec<_>>().join(", "))
}

async fn triggers(pool: &MySqlPool, tables: &[&str]) -> Result<Triggers, DatabaseError> {
    let cols = columns(pool, tables).await?;

    // `ON DELETE CASCADE` children: InnoDB deletes them without firing their triggers, so the
    // parent's trigger records their tombstones before the cascade takes them. One level is
    // enough for this schema (`release_bases` under `release` is the only synced one).
    let mut cascades: HashMap<String, Vec<(String, Vec<(String, String)>)>> = HashMap::new();
    let rows = sqlx::query(
        "SELECT rc.TABLE_NAME AS child, rc.REFERENCED_TABLE_NAME AS parent, k.COLUMN_NAME AS col, k.REFERENCED_COLUMN_NAME AS ref \
         FROM information_schema.REFERENTIAL_CONSTRAINTS rc \
         JOIN information_schema.KEY_COLUMN_USAGE k ON k.CONSTRAINT_SCHEMA = rc.CONSTRAINT_SCHEMA AND k.CONSTRAINT_NAME = rc.CONSTRAINT_NAME AND k.TABLE_NAME = rc.TABLE_NAME \
         WHERE rc.CONSTRAINT_SCHEMA = DATABASE() AND rc.DELETE_RULE = 'CASCADE' ORDER BY rc.TABLE_NAME, rc.CONSTRAINT_NAME, k.ORDINAL_POSITION",
    )
    .fetch_all(pool)
    .await?;
    for row in &rows {
        let (child, parent, col, refcol): (String, String, String, String) = (row.try_get(0)?, row.try_get(1)?, row.try_get(2)?, row.try_get(3)?);
        if !tables.contains(&child.as_str()) || !tables.contains(&parent.as_str()) {
            continue;
        }
        let list = cascades.entry(parent).or_default();
        match list.iter_mut().find(|(c, _)| *c == child) {
            Some((_, pairs)) => pairs.push((col, refcol)),
            None => list.push((child, vec![(col, refcol)])),
        }
    }

    let mut out = Triggers::new();
    for table in tables {
        let Some(c) = cols.get(*table) else { continue };
        if c.key.is_empty() {
            log::warn!("sync: {table} has no primary key; its changes are not tracked");
            continue;
        }
        let next = "NEXT VALUE FOR sync_rev_seq";
        let mut add = |kind: &str, timing: &str, body: String| {
            let name = format!("{PREFIX}{kind}_{table}");
            let create = format!("CREATE OR REPLACE TRIGGER `{name}` {timing} ON `{table}` FOR EACH ROW {body}");
            out.insert(name, (create, body));
        };

        add("i", "BEFORE INSERT", format!("SET NEW.`{COLUMN}` = {next}"));

        let same = if c.counted.is_empty() { "TRUE".to_string() } else { c.counted.iter().map(|(col, text)| same_value(col, *text)).collect::<Vec<_>>().join(" AND ") };
        // A key changed in place (`title_releases.title_id` when titles merge) is, to a
        // mirror, the old row deleted and a new one added: without the tombstone a client
        // would keep the old row beside the new one.
        let same_key = c.key.iter().map(|col| format!("NEW.`{col}` <=> OLD.`{col}`")).collect::<Vec<_>>().join(" AND ");
        add(
            "u",
            "BEFORE UPDATE",
            format!(
                "BEGIN IF NOT ({same_key}) THEN INSERT INTO sync_tombstones (rev, table_name, pk) VALUES ({next}, '{table}', {}); END IF; \
                 IF {same} THEN SET NEW.`{COLUMN}` = OLD.`{COLUMN}`; ELSE SET NEW.`{COLUMN}` = {next}; END IF; END",
                key_json("OLD", &c.key)
            ),
        );

        add("d", "AFTER DELETE", format!("INSERT INTO sync_tombstones (rev, table_name, pk) VALUES ({next}, '{table}', {})", key_json("OLD", &c.key)));

        if let Some(children) = cascades.get(*table) {
            let mut inserts = Vec::new();
            for (child, pairs) in children {
                let Some(cc) = cols.get(child) else { continue };
                if cc.key.is_empty() {
                    continue;
                }
                let on = pairs.iter().map(|(col, refcol)| format!("ch.`{col}` = OLD.`{refcol}`")).collect::<Vec<_>>().join(" AND ");
                inserts.push(format!("INSERT INTO sync_tombstones (rev, table_name, pk) SELECT {next}, '{child}', {} FROM `{child}` ch WHERE {on};", key_json("ch", &cc.key)));
            }
            if !inserts.is_empty() {
                // With foreign-key checks off nothing cascades, so nothing is recorded either.
                add("c", "BEFORE DELETE", format!("IF @@foreign_key_checks THEN {} END IF", inserts.join(" ")));
            }
        }
    }
    Ok(out)
}

/// This database's epoch and the highest tombstone number already pruned.
pub async fn state(pool: &MySqlPool) -> Result<(String, u64), DatabaseError> {
    let row = sqlx::query("SELECT epoch, pruned_through FROM sync_state WHERE id = 1").fetch_optional(pool).await?;
    match row {
        Some(row) => Ok((row.try_get(0)?, row.try_get(1)?)),
        None => {
            new_epoch(pool).await?;
            Box::pin(state(pool)).await
        }
    }
}

/// Starts a new epoch: every client's remembered position stops meaning anything, and its
/// next pull of each table is a full one. For anything that changes rows without the
/// triggers seeing it -- `TRUNCATE` above all.
pub async fn new_epoch(pool: &MySqlPool) -> Result<(), DatabaseError> {
    sqlx::query("INSERT INTO sync_state (id, epoch) VALUES (1, REPLACE(UUID(), '-', '')) ON DUPLICATE KEY UPDATE epoch = VALUES(epoch)").execute(pool).await?;
    Ok(())
}

/// Deletes tombstones older than [`TOMBSTONE_DAYS`] and records how far that reached.
pub async fn prune(pool: &MySqlPool) -> Result<u64, DatabaseError> {
    let reach: Option<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT MAX(rev) FROM sync_tombstones WHERE deleted_at < NOW() - INTERVAL {TOMBSTONE_DAYS} DAY")))
        .fetch_one(pool)
        .await?;
    let Some(reach) = reach else { return Ok(0) };
    // The mark first: a crash between the two leaves tombstones a client no longer trusts,
    // never a client trusting a gap.
    sqlx::query("UPDATE sync_state SET pruned_through = GREATEST(pruned_through, ?) WHERE id = 1").bind(reach).execute(pool).await?;
    let gone = sqlx::query("DELETE FROM sync_tombstones WHERE rev <= ?").bind(reach).execute(pool).await?.rows_affected();
    Ok(gone)
}

/// The last settled number found, per epoch: one process can serve more than one database,
/// and the epoch is what tells them apart.
fn last_stable() -> &'static std::sync::Mutex<HashMap<String, u64>> {
    static LAST: std::sync::OnceLock<std::sync::Mutex<HashMap<String, u64>>> = std::sync::OnceLock::new();
    LAST.get_or_init(Default::default)
}

/// A change number every row at or below which is committed, or `None` if none is known.
///
/// A number is handed out when a row is written, but the transaction holding it may commit
/// later. A client that recorded "I have everything up to N" while a lower number was still
/// uncommitted would never see that row. So this takes the sequence's current value and then
/// looks for writing transactions: with none open, everything below that value is settled.
/// With one open it falls back to the last value that was found settled, and a client keeps
/// its old position for that pull, which costs a few rows sent twice and nothing else.
///
/// `INNODB_TRX` is a snapshot the server refreshes at most every 100 ms, hence the pause
/// between the two reads: whatever held a number below the value read first is visible by
/// then.
/// `epoch` is this database's ([`state`]).
pub async fn stable(pool: &MySqlPool, epoch: &str) -> Option<u64> {
    let found = async {
        let mut conn = pool.acquire().await?;
        let current: i64 = sqlx::query_scalar("SELECT NEXT VALUE FOR sync_rev_seq").fetch_one(&mut *conn).await?;
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let writing: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.INNODB_TRX WHERE trx_mysql_thread_id <> CONNECTION_ID() \
             AND (trx_rows_modified > 0 OR (trx_query IS NOT NULL AND trx_autocommit_non_locking = 0))",
        )
        .fetch_one(&mut *conn)
        .await?;
        Ok::<_, sqlx::Error>((current, writing))
    }
    .await;
    let mut last = last_stable().lock().unwrap_or_else(|p| p.into_inner());
    match found {
        Ok((current, 0)) if current > 0 => {
            let settled = current as u64 - 1;
            let entry = last.entry(epoch.to_string()).or_insert(settled);
            *entry = (*entry).max(settled);
            Some(*entry)
        }
        Ok(_) => last.get(epoch).copied(),
        Err(e) => {
            log::warn!("sync: could not tell which changes are settled: {e}");
            last.get(epoch).copied()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tombstone_key_is_text_in_key_order() {
        assert_eq!(key_json("OLD", &["source_id".into(), "id".into()]), "JSON_ARRAY(CAST(OLD.`source_id` AS CHAR), CAST(OLD.`id` AS CHAR))");
    }

    #[test]
    fn only_check_stamps_are_uncounted() {
        assert!(uncounted("local_files", "last_integrity_check"));
        assert!(!uncounted("local_files", "last_integrity_failure"), "a failed check is news");
        assert!(!uncounted("hashes", "last_integrity_check"));
    }
}
