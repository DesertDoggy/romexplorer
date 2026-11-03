//! Putting a backup back.
//!
//! Shares everything but the carrier with the peer mirror import and the SQLite cache: a
//! backup is rows a database projected to JSON, and [`super::rows::Writer`] puts them back.
//! What is specific to a restore is the checking that happens before any of that.
//!
//! **A restore adds and updates; it never deletes.** Rows are upserted by primary key, so a
//! row deleted after the backup was written stays deleted-from-the-backup's-point-of-view
//! but present here. Onto an empty database -- the case a restore actually exists for --
//! that is an exact restore. Onto a populated one it is a merge, which is the conservative
//! reading and the only one that cannot destroy data the backup does not know about. A
//! genuine "make this database identical to the backup" would have to truncate first, and
//! that is not something to do implicitly.
//!
//! **Foreign-key checks are off for the duration.** A backup's tables are written in the
//! order the backup lists them, which is dependency order, but `local_release_games` points
//! at `release` rows that live in the *other* backup -- so a database restore onto a machine
//! whose catalogue has not been restored yet would fail row by row. Turning the checks off
//! lets both halves be restored in either order, and the result is consistent once both are
//! in. A database backup restored alone can be re-linked by `hash_match`; see
//! [`super::backup`] on why that is the trade.

use std::io::BufRead;


use super::{App, Error, Result};

// Shared with the GUI through `romexplorer_types`.
pub use romexplorer_types::restore::RestoreSummary;

/// Every table any backup may legitimately contain.
fn allowed(table: &str) -> bool {
    super::backup::DATABASE_TABLES.contains(&table) || super::backup::CATALOG_TABLES.contains(&table)
}

/// How many rows the reader thread hands over at a time.
///
/// Sized to match what [`super::rows::Writer`] will actually put in one statement (its
/// placeholder budget divided by the table's width, so ~3,700 rows for a narrow table). At
/// 1,000 the reader was the limit rather than the writer, and the restore issued three to
/// four times as many round trips as it needed -- measurably 3x slower than the SQLite
/// import doing identical work.
const BATCH: usize = 4_000;

/// Restores the backup named `name` from the backup directory.
pub async fn restore(app: &App, name: &str, progress: &crate::database::jobs::Progress) -> Result<RestoreSummary> {
    let path = super::backup::file(app, name).await?;

    // The manifest first, on its own, so a backup this build cannot read is refused before
    // anything has been written.
    let manifest = {
        let file = std::fs::File::open(&path).map_err(|e| Error::internal(format!("opening {}: {e}", path.display())))?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| Error::invalid(format!("{name}: not a readable zip: {e}")))?;
        let entry = zip.by_name("manifest.json").map_err(|_| Error::invalid(format!("{name}: no manifest.json -- not a romexplorer backup")))?;
        serde_json::from_reader::<_, serde_json::Value>(entry).map_err(|e| Error::invalid(format!("{name}: manifest.json: {e}")))?
    };

    let written_with = manifest.get("schema_version").and_then(|v| v.as_i64()).unwrap_or(0);
    let ours = crate::database::schema_version();
    if written_with > ours {
        // Forward compatibility is not something this can fake: a newer backup may carry
        // columns and tables that do not exist here, and writing what happens to fit would
        // restore a subset while reporting success.
        return Err(Error::invalid(format!(
            "{name} was written by a newer version (schema {written_with}, this build has {ours}); upgrade before restoring it"
        )));
    }
    let kind = manifest.get("kind").and_then(|v| v.as_str()).unwrap_or("database").to_string();

    // The backup's own table order is dependency order; keep it rather than imposing ours,
    // so a backup from another version restores in the order it was written.
    //
    // The row count comes along too: the manifest records it per table, so the total size of
    // the restore is known before the first row is read, and progress can be a real fraction
    // rather than "table 7 of 51" -- which stands still for minutes inside a large one.
    let listed: Vec<(String, u64)> = manifest
        .get("tables")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|t| Some((t.get("table")?.as_str()?.to_string(), t.get("rows").and_then(|v| v.as_u64()).unwrap_or(0))))
                .collect()
        })
        .unwrap_or_default();
    let mut summary = RestoreSummary { file: name.to_string(), kind, schema_version: written_with, ..Default::default() };
    let (keep, drop): (Vec<(String, u64)>, Vec<(String, u64)>) = listed.into_iter().partition(|(t, _)| allowed(t));
    summary.skipped = drop.into_iter().map(|(t, _)| t).collect();
    let total_rows: u64 = keep.iter().map(|(_, n)| n).sum();
    let wanted: Vec<String> = keep.into_iter().map(|(t, _)| t).collect();
    if wanted.is_empty() {
        return Err(Error::invalid(format!("{name}: nothing in it is a table this build restores")));
    }

    // Decompressing a zip member is blocking, CPU-bound work, and a catalogue backup's
    // `hashes` member is hundreds of megabytes. Reading it on a blocking thread and handing
    // batches over a bounded channel keeps the decompression off the async runtime and the
    // memory flat -- reading the whole member into a Vec first would not.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<std::result::Result<(String, Vec<serde_json::Value>), String>>(2);
    let reader_path = path.clone();
    let reader_tables = wanted.clone();
    let reader = tokio::task::spawn_blocking(move || {
        let send = |msg| tx.blocking_send(msg).is_ok();
        let file = match std::fs::File::open(&reader_path) {
            Ok(f) => f,
            Err(e) => {
                send(Err(format!("opening {}: {e}", reader_path.display())));
                return;
            }
        };
        let mut zip = match zip::ZipArchive::new(file) {
            Ok(z) => z,
            Err(e) => {
                send(Err(format!("reading the zip: {e}")));
                return;
            }
        };
        for table in reader_tables {
            let entry = match zip.by_name(&format!("{table}.ndjson")) {
                Ok(e) => e,
                // A table listed in the manifest with no member is empty, not broken.
                Err(_) => continue,
            };
            let mut batch = Vec::with_capacity(BATCH);
            for line in std::io::BufReader::new(entry).lines() {
                let line = match line {
                    Ok(l) => l,
                    Err(e) => {
                        send(Err(format!("{table}: {e}")));
                        return;
                    }
                };
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str(&line) {
                    Ok(value) => batch.push(value),
                    Err(e) => {
                        send(Err(format!("{table}: {e}")));
                        return;
                    }
                }
                if batch.len() >= BATCH && !send(Ok((table.clone(), std::mem::take(&mut batch)))) {
                    return;
                }
            }
            if !batch.is_empty() && !send(Ok((table.clone(), batch))) {
                return;
            }
        }
    });

    // Captured before anything is written: a backup's `settings` carries the identity of the
    // machine that made it (see `settings::Identity` for why each of the three matters).
    let ours = crate::database::settings::Identity::capture(&app.pool).await?;
    // A restore cut short (cancelled, or the worker stopping) still puts it back.
    let mut keep_identity = IdentityGuard { pool: app.pool.clone(), ours: Some(ours.clone()) };
    let mut conn = app.pool.acquire().await?;
    // Closed rather than returned to the pool if this is cut short (a cancel, a dropped
    // request): FOREIGN_KEY_CHECKS is per connection and must never be left off on one.
    conn.close_on_drop();
    sqlx::query("SET FOREIGN_KEY_CHECKS = 0").execute(&mut *conn).await?;

    let total = wanted.len() as u64;
    let mut writer: Option<(String, super::rows::Writer)> = None;
    let mut done = 0u64;
    let mut rows_done = 0u64;
    let result = async {
        while let Some(message) = rx.recv().await {
            let (table, rows) = message.map_err(Error::invalid)?;
            // One writer per table, rebuilt only when the table changes.
            if writer.as_ref().is_none_or(|(name, _)| name != &table) {
                writer = Some((table.clone(), super::rows::Writer::new(app, &table, Vec::new()).await?));
                summary.tables.push((table.clone(), 0));
                done = summary.tables.len() as u64;
            }
            let (_, w) = writer.as_ref().expect("just set");
            let n = w.write(&mut conn, &rows).await?.rows;
            rows_done += n;
            if let Some(entry) = summary.tables.last_mut() {
                entry.1 += n;
                progress
                    .report(
                        rows_done,
                        Some(total_rows.max(1)),
                        &format!("table {done} of {total} ({}) {} row(s) -- {rows_done}/{total_rows} overall", entry.0, entry.1),
                    )
                    .await;
            }
        }
        Ok::<(), Error>(())
    }
    .await;

    // Restored or not, this connection must not go back to the pool with checks disabled.
    sqlx::query("SET FOREIGN_KEY_CHECKS = 1").execute(&mut *conn).await?;
    ours.restore(&mut conn).await?;
    keep_identity.ours = None;
    result?;
    reader.await.map_err(|e| Error::internal(format!("restore reader: {e}")))?;
    Ok(summary)
}

/// Puts this machine's identity back if a restore ends before it does so itself: the
/// backup's `instance_id` and `catalog_from` must never stay behind.
struct IdentityGuard {
    pool: sqlx::MySqlPool,
    ours: Option<crate::database::settings::Identity>,
}

impl Drop for IdentityGuard {
    fn drop(&mut self) {
        let Some(ours) = self.ours.take() else { return };
        let pool = self.pool.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                match pool.acquire().await {
                    Ok(mut conn) => {
                        if let Err(e) = ours.restore(&mut conn).await {
                            log::error!("restore: could not put this machine's identity back after the restore stopped: {e}");
                        }
                    }
                    Err(e) => log::error!("restore: could not put this machine's identity back after the restore stopped: {e}"),
                }
            });
        }
    }
}
