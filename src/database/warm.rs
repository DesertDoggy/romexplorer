//! Reading the catalogue into MariaDB's buffer pool after the database starts, so the first
//! screens come from memory instead of each paying for its own disk reads.
//!
//! Two things fill the pool at start, in this order:
//!
//! 1. MariaDB's own reload of the page list it saved at the last shutdown
//!    (`--innodb-buffer-pool-load-at-startup`, all of it since `--innodb-buffer-pool-dump-pct=100`,
//!    see `server.rs`). That restores exactly what was hot, but only after a clean shutdown,
//!    and there is nothing to restore on the first start or after an import rewrote tables.
//! 2. This pass, which waits for (1) and then walks every index of every table once
//!    (`SELECT COUNT(*) ... FORCE INDEX`): the primary key brings in the rows, the secondary
//!    indexes themselves. Pages (1) already loaded are buffer hits and cost next to nothing.
//!
//! Smallest tables first, and only as much as fits: when the catalogue is bigger than the
//! pool, reading all of it would evict the start of the walk with its end. TEXT/BLOB columns
//! stored off-page are not read -- they are fetched when a row is actually shown.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use sqlx::{MySqlPool, Row};

/// What the warm-up is doing, for the server screen.
#[derive(Debug, Clone, Default, Serialize)]
pub struct WarmStatus {
    /// `idle` (never ran in this process), `waiting` (for MariaDB's own reload), `running`,
    /// `done`, `skipped`, `failed`.
    pub state: &'static str,
    pub tables_done: usize,
    pub tables_total: usize,
    /// Table + index bytes walked so far, and planned (InnoDB's own size estimates).
    pub bytes_done: u64,
    pub bytes_planned: u64,
    /// Tables left out because the pool could not hold them as well.
    pub tables_skipped: usize,
    pub seconds: f64,
    pub message: Option<String>,
}

static STATUS: Mutex<WarmStatus> = Mutex::new(WarmStatus { state: "idle", tables_done: 0, tables_total: 0, bytes_done: 0, bytes_planned: 0, tables_skipped: 0, seconds: 0.0, message: None });

pub fn status() -> WarmStatus {
    STATUS.lock().map(|s| s.clone()).unwrap_or_default()
}

fn update(f: impl FnOnce(&mut WarmStatus)) {
    if let Ok(mut s) = STATUS.lock() {
        f(&mut s);
    }
}

/// How long to wait for MariaDB's own reload before warming anyway.
const LOAD_WAIT: Duration = Duration::from_secs(600);
/// The share of the pool the walk may fill; the rest stays for whatever the app reads next.
const POOL_SHARE: f64 = 0.9;

/// MariaDB's `Innodb_buffer_pool_load_status`, e.g. "Loaded 5/120 pages" or "Buffer pool(s)
/// load completed at ...".
pub async fn load_status(pool: &MySqlPool) -> Option<String> {
    sqlx::query("SHOW GLOBAL STATUS LIKE 'Innodb_buffer_pool_load_status'").fetch_optional(pool).await.ok().flatten().and_then(|r| r.try_get::<String, _>(1).ok())
}

fn still_loading(status: &str) -> bool {
    status.starts_with("Loaded ") || status.starts_with("Loading ")
}

/// Runs the warm-up to completion. Meant for `tokio::spawn`: it logs and records its outcome
/// rather than returning an error, since nothing waits on it.
pub async fn warm(pool: MySqlPool) {
    let started = Instant::now();
    update(|s| *s = WarmStatus { state: "waiting", ..Default::default() });
    match warm_inner(&pool, started).await {
        Ok(()) => {
            let s = status();
            eprintln!("database cache warmed: {} table(s), {} MB in {:.1}s{}", s.tables_done, s.bytes_done >> 20, s.seconds, if s.tables_skipped > 0 { format!(" ({} left out: the cache is smaller than the catalogue)", s.tables_skipped) } else { String::new() });
        }
        Err(e) => {
            eprintln!("database cache warm-up stopped: {e}");
            update(|s| {
                s.state = "failed";
                s.message = Some(e.to_string());
                s.seconds = started.elapsed().as_secs_f64();
            });
        }
    }
}

async fn warm_inner(pool: &MySqlPool, started: Instant) -> Result<(), sqlx::Error> {
    // Let MariaDB's own reload finish first: it restores what was hot, and both at once
    // would just compete for the same disk.
    let wait_until = Instant::now() + LOAD_WAIT;
    while Instant::now() < wait_until {
        match load_status(pool).await {
            Some(s) if still_loading(&s) => tokio::time::sleep(Duration::from_secs(2)).await,
            _ => break,
        }
    }

    let pool_bytes: u64 = sqlx::query_scalar::<_, u64>("SELECT @@innodb_buffer_pool_size").fetch_one(pool).await?;
    let budget = (pool_bytes as f64 * POOL_SHARE) as u64;
    let tables: Vec<(String, u64)> = sqlx::query(
        "SELECT TABLE_NAME, CAST(COALESCE(DATA_LENGTH, 0) + COALESCE(INDEX_LENGTH, 0) AS UNSIGNED) AS bytes \
         FROM information_schema.TABLES WHERE TABLE_SCHEMA = DATABASE() AND ENGINE = 'InnoDB' AND TABLE_TYPE = 'BASE TABLE' ORDER BY bytes, TABLE_NAME",
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| (r.get::<String, _>(0), r.get::<u64, _>(1)))
    .collect();
    let (planned, skipped) = plan(&tables, budget);
    let bytes_planned = planned.iter().map(|(_, b)| b).sum();
    update(|s| {
        s.state = "running";
        s.tables_total = planned.len();
        s.tables_skipped = skipped;
        s.bytes_planned = bytes_planned;
    });

    // One connection for the whole walk: it is a background job and must not crowd out the
    // pool the app's own requests use.
    let mut conn = pool.acquire().await?;
    for (table, bytes) in planned {
        let indexes: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT INDEX_NAME FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? AND INDEX_TYPE NOT IN ('FULLTEXT', 'SPATIAL')",
        )
        .bind(&table)
        .fetch_all(&mut *conn)
        .await?;
        for index in indexes {
            let sql = format!("SELECT COUNT(*) FROM `{}` FORCE INDEX (`{}`)", table.replace('`', "``"), index.replace('`', "``"));
            // One table that can't be read this way (dropped mid-walk, say) is not a reason
            // to stop warming the rest.
            let _ = sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_one(&mut *conn).await;
        }
        update(|s| {
            s.tables_done += 1;
            s.bytes_done += bytes;
            s.seconds = started.elapsed().as_secs_f64();
        });
    }
    update(|s| {
        s.state = "done";
        s.seconds = started.elapsed().as_secs_f64();
    });
    Ok(())
}

/// The tables to walk, smallest first, while they fit in `budget`; and how many were left out.
/// Smallest first because those are the lookup tables every screen joins against.
fn plan(tables: &[(String, u64)], budget: u64) -> (Vec<(String, u64)>, usize) {
    let mut used = 0u64;
    let mut planned = Vec::new();
    let mut skipped = 0;
    let mut sorted: Vec<_> = tables.to_vec();
    sorted.sort_by_key(|(name, bytes)| (*bytes, name.clone()));
    for (name, bytes) in sorted {
        if used + bytes <= budget {
            used += bytes;
            planned.push((name, bytes));
        } else {
            skipped += 1;
        }
    }
    (planned, skipped)
}

/// Starts [`warm`] in the background when this installation wants it and the database is
/// on this machine. A remote one is someone else's server to manage: its cache is warm
/// already if it has been up, and walking it over the network would be a lot of traffic to
/// make a point.
pub fn start(pool: &MySqlPool, stored: &super::config::StoredConfig, local_db: bool) {
    if !local_db {
        skip("the database is on another machine");
    } else if !stored.warm_cache() {
        skip("switched off in Config › Server");
    } else {
        tokio::spawn(warm(pool.clone()));
    }
}

/// Marks the warm-up as not run, with why (a remote database, or switched off).
pub fn skip(reason: &str) {
    update(|s| {
        *s = WarmStatus { state: "skipped", message: Some(reason.to_string()), ..Default::default() };
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_takes_small_tables_first_while_they_fit() {
        let tables = vec![("big".to_string(), 900), ("small".to_string(), 10), ("mid".to_string(), 200), ("mid2".to_string(), 200)];
        let (planned, skipped) = plan(&tables, 500);
        assert_eq!(planned.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["small", "mid", "mid2"]);
        assert_eq!(skipped, 1);
        let (planned, skipped) = plan(&tables, 10_000);
        assert_eq!((planned.len(), skipped), (4, 0));
    }

    /// Against a real `mariadbd`: the start-up options are accepted, the pool is the size
    /// asked for, the warm-up walks every table, and the whole cache comes back after a
    /// restart. Needs a MariaDB laid out like the bundled one:
    /// `ROMEXPLORER_TEST_MARIADB_BASEDIR=... cargo test --lib warm -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn warms_a_real_database_and_reloads_it_after_a_restart() {
        use crate::database::config::BufferPool;
        use crate::database::server::{MariaDbServer, ServerConfig};
        use sqlx::Connection;
        let Ok(base) = std::env::var("ROMEXPLORER_TEST_MARIADB_BASEDIR") else { return };
        let root = std::env::temp_dir().join(format!("romexplorer-warm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let port = 33_000 + (std::process::id() % 1000) as u16;
        let config = ServerConfig::new(&base, root.join("data"), port).with_buffer_pool(BufferPool::Fixed(256 << 20));
        let mut server = MariaDbServer::new(config.clone());
        server.start().await.unwrap();

        let opts = sqlx::mysql::MySqlConnectOptions::new().host("127.0.0.1").port(port).username("root");
        let mut admin = sqlx::mysql::MySqlConnection::connect_with(&opts).await.unwrap();
        for sql in ["CREATE DATABASE w", "CREATE TABLE w.small (id INT PRIMARY KEY, name VARCHAR(40), KEY by_name (name))", "CREATE TABLE w.big (id INT PRIMARY KEY AUTO_INCREMENT, payload CHAR(200), KEY by_payload (payload(20)))"] {
            sqlx::query(sql).execute(&mut admin).await.unwrap();
        }
        sqlx::query("USE w").execute(&mut admin).await.unwrap();
        sqlx::query("INSERT INTO w.small VALUES (1, 'a'), (2, 'b')").execute(&mut admin).await.unwrap();
        sqlx::query("INSERT INTO w.big (payload) SELECT REPEAT(CHAR(65 + seq % 26), 200) FROM seq_1_to_50000").execute(&mut admin).await.unwrap();
        sqlx::query("ANALYZE TABLE w.small, w.big").execute(&mut admin).await.unwrap();

        let pool = sqlx::mysql::MySqlPoolOptions::new().max_connections(2).connect_with(opts.clone().database("w")).await.unwrap();
        let size: u64 = sqlx::query_scalar("SELECT @@innodb_buffer_pool_size").fetch_one(&pool).await.unwrap();
        assert_eq!(size, 256 << 20, "the pool is the size asked for");
        let pct: u64 = sqlx::query_scalar("SELECT @@innodb_buffer_pool_dump_pct").fetch_one(&pool).await.unwrap();
        assert_eq!(pct, 100, "the whole cache is kept across a restart");
        // It writes as `WriteTuning` says, from the start and when changed while running.
        let written = |pool: sqlx::MySqlPool| async move {
            let v = |name: &'static str| {
                let pool = pool.clone();
                async move { sqlx::query_scalar::<_, u64>(sqlx::AssertSqlSafe(format!("SELECT CAST(@@{name} AS UNSIGNED)"))).fetch_one(&pool).await.unwrap() }
            };
            (v("innodb_flush_log_at_trx_commit").await, v("innodb_io_capacity").await, v("innodb_io_capacity_max").await, v("innodb_flush_neighbors").await, v("innodb_log_file_size").await)
        };
        let started = config.write_tuning();
        let live = written(pool.clone()).await;
        assert_eq!((live.0, live.1, live.2, live.3), (2, u64::from(started.io_capacity), u64::from(started.io_capacity_max), u64::from(started.flush_neighbors)), "fast writes, paced for {}", started.disk);
        assert_eq!(live.4, 1 << 30, "a 1 GB redo log");
        let nvme = crate::database::server::WriteTuning::new(false, crate::database::config::DiskKind::Nvme, &root);
        nvme.apply(&pool).await.unwrap();
        let live = written(pool.clone()).await;
        assert_eq!((live.0, live.1, live.2, live.3), (1, 10_000, 20_000, 0), "changed while running, upwards");
        crate::database::server::WriteTuning::new(true, crate::database::config::DiskKind::Hdd, &root).apply(&pool).await.unwrap();
        let live = written(pool.clone()).await;
        assert_eq!((live.0, live.1, live.2, live.3), (2, 200, 2_000, 1), "and back down");

        warm(pool.clone()).await;
        let s = status();
        assert_eq!((s.state, s.tables_done, s.tables_skipped), ("done", 2, 0), "{s:?}");
        assert!(s.bytes_done > 5 << 20, "the big table was walked: {s:?}");
        pool.close().await;
        drop(admin);

        server.shutdown(Duration::from_secs(30)).await.unwrap();
        let mut server = MariaDbServer::new(config);
        server.start().await.unwrap();
        let pool = sqlx::mysql::MySqlPoolOptions::new().max_connections(1).connect_with(opts.database("w")).await.unwrap();
        let mut last = String::new();
        for _ in 0..60 {
            last = load_status(&pool).await.unwrap_or_default();
            if !still_loading(&last) && !last.is_empty() && !last.contains("not started") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        assert!(last.contains("completed"), "reloaded after restart: {last}");
        pool.close().await;
        server.shutdown(Duration::from_secs(30)).await.unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_reload_in_progress_is_waited_for() {
        assert!(still_loading("Loaded 5/120 pages"));
        assert!(still_loading("Loading buffer pool(s) from /data/ib_buffer_pool"));
        assert!(!still_loading("Buffer pool(s) load completed at 261005 10:00:00"));
        assert!(!still_loading("Buffer pool(s) load aborted on request"));
    }
}
