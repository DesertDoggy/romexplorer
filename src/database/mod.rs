//! MariaDB-backed catalog, collection and provider cache. Design: docs/plan/database.md.
//!
//! Layers, low to high:
//! - [`server`]: the embedded `mariadbd` from `app_root_ex/lib/<os>/mariadb/` (bootstrap, start,
//!   `SHUTDOWN`), [`config`]/[`connection`]: paths, pool, migrations, platform seeding.
//! - [`dat_download`] -> [`dat_parse`] -> [`import`]: reference DATs into `content`/`hashes`/
//!   `release`/`title`. [`lookup`] is the read side; [`matching`] (ported from playmatch)
//!   backfills fields on the hash-matched path and scores candidates on the no-hash path
//!   ([`matching::resolve`]).
//! - [`providers`]: the vendored `playmatch_deredis` clients plus this project's
//!   RetroAchievements client, MariaDB-cached (`providers::cache`), raw responses on disk
//!   (`providers::raw_store`), bulk sync (`providers::bulk`), media (`providers::media`).
//!   Credentials come from `app_root_ex/assets/api.env` only ([`env_creds`]).
//! - [`jobs`] + [`settings`]: the worker queue and the JSON settings table; the worker
//!   itself is the `romexplorer-worker` binary (`src/bin/romexplorer_worker.rs`).
//!
//! Async (`tokio`/`sqlx`): a GUI and a browser-facing web backend are planned to share this
//! layer. The server may be the embedded one on `127.0.0.1` or a MariaDB on a fileserver --
//! just a different [`DatabaseConfig::host`].

pub mod config;
pub mod connection;
pub mod content;
pub mod counterparts;
pub mod dat_convert;
pub mod dat_download;
pub mod dat_parse;
pub mod dictionary;
pub mod directories;
pub mod emulators;
pub mod env_creds;
pub mod env_file;
pub mod error;
pub mod icon_import;
pub mod import;
pub mod jobs;
pub mod lookup;
pub mod media_cache;
pub mod media_store;
pub mod matching;
pub mod merge_audit;
pub mod patch_names;
pub mod platform_map;
pub mod schedules;
pub mod providers;
pub mod release_bases;
pub mod release_buckets;
pub mod release_editions;
pub mod release_merge;
pub mod release_order;
pub mod ra_patches;
pub mod serial_match;
pub mod server;
pub mod settings;
pub mod title_family;
pub mod title_merge;
pub mod title_review;
pub mod vault;
pub mod storage;
pub mod sync_rev;
pub mod virtual_collection;
pub mod warm;

pub use config::{AppPaths, DatabaseConfig};
pub use connection::{connect, ensure_database, initial_setup, initialize, initialize_with_root, migrate, probe, schema_version, seed_platforms, DatabaseState, InitialSetup};
pub use env_creds::ApiCredentials;
pub use error::DatabaseError;
pub use vault::Vault;
pub use server::{MariaDbServer, ServerConfig, UserPrivileges};

/// A pooled connection, named so callers outside this crate (the GUI holds one to keep the
/// worker lock) don't have to depend on `sqlx` just to write the type down.
pub type MySqlPoolConnection = sqlx::pool::PoolConnection<sqlx::MySql>;

/// Whether a database is already listening and answering on `config`'s address, in the few
/// seconds it's worth waiting to find out. The question every start-up asks before deciding
/// to launch the bundled server: `romexplorer_server` and the desktop GUI both do, and both
/// must answer it the same way.
pub async fn database_answers(config: &config::DatabaseConfig) -> bool {
    use sqlx::Connection;
    let options = config.connect_options(false);
    match tokio::time::timeout(std::time::Duration::from_secs(3), sqlx::mysql::MySqlConnection::connect_with(&options)).await {
        Ok(Ok(mut conn)) => {
            let ok = sqlx::query("SELECT 1").execute(&mut conn).await.is_ok();
            let _ = sqlx::Connection::close(conn).await;
            ok
        }
        _ => false,
    }
}

/// What a starting app should do about its local database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalDatabase {
    /// This installation's own database is already up (its other app started it): use it.
    Adopt,
    /// Nothing of ours is running: start the bundled server on this port.
    Start { port: u16 },
}

/// Decides between the two, by *whose* database answers rather than whether one does.
///
/// "Something answers on 127.0.0.1:3307" used to be the whole test, which made every
/// installation on a machine share whichever database was started first: a second copy of
/// the app unpacked into a fresh folder, with no `data/` of its own, opened the first one's
/// catalogue instead of starting new. The port is not the identity -- the data directory is.
/// So the server on the configured port is adopted only if it serves `datadir`; anything
/// else there is somebody else's, and ours starts on the next free port (the caller saves
/// that port, so this installation's desktop app and server find each other next time).
///
/// With `ROMEXPLORER_DB_PORT` set the caller was told which database to use, and the old
/// rule applies: adopt whatever answers there.
pub async fn plan_local_database(config: &config::DatabaseConfig, datadir: &std::path::Path) -> LocalDatabase {
    use sqlx::Connection;
    let told = std::env::var_os("ROMEXPLORER_DB_PORT").is_some();
    let options = config.connect_options(false);
    let connected = tokio::time::timeout(std::time::Duration::from_secs(3), sqlx::mysql::MySqlConnection::connect_with(&options)).await;
    if let Ok(Ok(mut conn)) = connected {
        let theirs: Option<String> = sqlx::query_scalar("SELECT @@datadir").fetch_one(&mut conn).await.ok();
        let _ = sqlx::Connection::close(conn).await;
        if told || theirs.as_deref().is_some_and(|d| same_directory(d, &datadir.display().to_string())) {
            return LocalDatabase::Adopt;
        }
    } else if told || !port_in_use(config.port).await {
        return LocalDatabase::Start { port: config.port };
    }
    // Occupied by a database that is not this installation's (or by something that would
    // not let us in to ask): leave it alone and take the next port nobody listens on.
    for port in config.port.saturating_add(1)..=config.port.saturating_add(50) {
        if !port_in_use(port).await {
            return LocalDatabase::Start { port };
        }
    }
    LocalDatabase::Start { port: config.port }
}

async fn port_in_use(port: u16) -> bool {
    matches!(tokio::time::timeout(std::time::Duration::from_millis(500), tokio::net::TcpStream::connect(("127.0.0.1", port))).await, Ok(Ok(_)))
}

/// Whether two spellings name one directory: separators, a trailing one, and (on Windows,
/// where `@@datadir` comes back in whatever case it was started with) case.
fn same_directory(a: &str, b: &str) -> bool {
    let key = |s: &str| {
        let k = s.trim().replace('\\', "/").trim_end_matches('/').to_string();
        if cfg!(windows) {
            k.to_lowercase()
        } else {
            k
        }
    };
    key(a) == key(b)
}

/// The id of the row named `name` in a small lookup table (`sources`, `genre`,
/// `perspective`, ...), inserting it only when it's missing. Deliberately select-first
/// rather than `INSERT IGNORE` + select: InnoDB consumes an AUTO_INCREMENT value even for
/// an ignored duplicate, so the insert-every-time pattern exhausts a TINYINT/SMALLINT id
/// after enough calls -- and then new names silently fail to insert (this broke every
/// Redump import once `sources` hit 255). `table` is always a constant from this crate.
pub(crate) async fn lookup_or_insert_name(pool: &sqlx::mysql::MySqlPool, table: &'static str, name: &str) -> Result<u64, DatabaseError> {
    use sqlx::Row;
    let select = format!("SELECT CAST(id AS UNSIGNED) AS id FROM `{table}` WHERE name = ?");
    if let Some(row) = sqlx::query(sqlx::AssertSqlSafe(select.as_str())).bind(name).fetch_optional(pool).await? {
        return Ok(row.try_get("id")?);
    }
    // INSERT IGNORE only covers a concurrent insert of the same name.
    sqlx::query(sqlx::AssertSqlSafe(format!("INSERT IGNORE INTO `{table}` (name) VALUES (?)"))).bind(name).execute(pool).await?;
    let row = sqlx::query(sqlx::AssertSqlSafe(select.as_str()))
        .bind(name)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| DatabaseError::Config(format!("could not add {name:?} to `{table}` (is its AUTO_INCREMENT exhausted?)")))?;
    Ok(row.try_get("id")?)
}