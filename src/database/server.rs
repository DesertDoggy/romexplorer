//! The embedded MariaDB server: bootstrapping its data directory on first run and starting/
//! stopping `mariadbd` from `app_root_ex/lib/<os>/mariadb/`.
//!
//! Done in Rust rather than via the bundled `mariadb-install-db` script because that script
//! is POSIX shell (no use on Windows) and needs `my_print_defaults`, which the trimmed
//! install doesn't ship. What it actually does on the bootstrap path is small: feed
//! `share/*.sql` (system tables, help tables, sys schema) into `mariadbd --bootstrap` --
//! reproduced by [`MariaDbServer::bootstrap`] with the exact same option set. Verified
//! against the real binary: bootstrap takes ~0.3s, the server is accepting TCP connections
//! ~0.5s after spawn, and `SHUTDOWN` (SQL, works on every platform, no `mariadb-admin`
//! needed) stops it in ~0.3s.
//!
//! Only the root@127.0.0.1 account exists after bootstrap (no password, `auth_root_socket`
//! unset), which is what a per-user local instance bound to loopback wants; a password can
//! be set later through ordinary SQL and carried in [`super::DatabaseConfig`].

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use sqlx::Connection;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};

use super::error::DatabaseError;

/// The seven bootstrap SQL files, in the order `mariadb-install-db` concatenates them.
const BOOTSTRAP_SQL: &[&str] = &[
    "mariadb_system_tables.sql",
    "mariadb_performance_tables.sql",
    "mariadb_system_tables_data.sql",
    "fill_help_tables.sql",
    "maria_add_gis_sp_bootstrap.sql",
    "mariadb_sys_schema.sql",
];

/// How hard the embedded server writes: what `db_fast_writes` and `db_disk` come to.
///
/// MariaDB's defaults are for a spinning disk and the strictest durability, and both cost
/// this app speed it has no use for:
///
/// - `innodb_flush_log_at_trx_commit = 1` makes every commit wait for a disk flush. A scan
///   or an import commits in thousands of small steps and paid one flush each. `2` hands
///   the log to the OS at each commit and flushes about once a second: a crash of the app
///   or of `mariadbd` still loses nothing; a power cut or an OS crash can lose up to the
///   last second of commits, with the database consistent either way -- and what this app
///   writes in bulk can be run again.
/// - `innodb_io_capacity` 200 (max 2000) caps background writing at a hard disk's pace, so
///   on an SSD changed pages pile up and then stall the writers.
/// - `innodb_flush_neighbors` writes a page's neighbours with it to save a hard disk's head
///   a trip, which on an SSD is only more writes.
///
/// All four are dynamic, so a change in Config > Server applies at once ([`Self::apply`]) as
/// well as from the next start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct WriteTuning {
    pub flush_at_commit: u8,
    pub io_capacity: u32,
    pub io_capacity_max: u32,
    pub flush_neighbors: u8,
    /// The disk these are for: what was chosen, or what `auto` found (`ssd` / `hdd`).
    pub disk: &'static str,
}

impl WriteTuning {
    /// For a database whose files are in `datadir`. `auto` is settled by where that is: an
    /// SSD as one, anything else -- a hard disk, a network share, a folder nothing can be
    /// told about -- as a hard disk, the setting that cannot hurt.
    pub fn new(fast_writes: bool, disk: super::config::DiskKind, datadir: &Path) -> Self {
        use super::config::DiskKind;
        let disk = match disk {
            DiskKind::Auto if crate::rom::hash::StorageKind::for_path(datadir) == crate::rom::hash::StorageKind::Ssd => DiskKind::Ssd,
            DiskKind::Auto => DiskKind::Hdd,
            chosen => chosen,
        };
        let (io_capacity, io_capacity_max, flush_neighbors) = match disk {
            DiskKind::Nvme => (10_000, 20_000, 0),
            DiskKind::Ssd => (2_000, 4_000, 0),
            DiskKind::Hdd | DiskKind::Auto => (200, 2_000, 1),
        };
        Self { flush_at_commit: if fast_writes { 2 } else { 1 }, io_capacity, io_capacity_max, flush_neighbors, disk: disk.to_text() }
    }

    /// As `mariadbd` start-up options.
    fn args(&self) -> [String; 4] {
        [
            format!("--innodb-flush-log-at-trx-commit={}", self.flush_at_commit),
            // The ceiling first: the server refuses a capacity above its current maximum.
            format!("--innodb-io-capacity-max={}", self.io_capacity_max),
            format!("--innodb-io-capacity={}", self.io_capacity),
            format!("--innodb-flush-neighbors={}", self.flush_neighbors),
        ]
    }

    /// Sets them on a running server. Needs an account that may `SET GLOBAL`.
    pub async fn apply(&self, pool: &sqlx::MySqlPool) -> Result<(), DatabaseError> {
        // Raising: the ceiling first. Lowering: the capacity first. Doing the capacity on
        // both sides of the ceiling covers either without asking which this is.
        let low = self.io_capacity.min(100);
        for sql in [
            format!("SET GLOBAL innodb_flush_log_at_trx_commit = {}", self.flush_at_commit),
            format!("SET GLOBAL innodb_io_capacity = {low}"),
            format!("SET GLOBAL innodb_io_capacity_max = {}", self.io_capacity_max),
            format!("SET GLOBAL innodb_io_capacity = {}", self.io_capacity),
            format!("SET GLOBAL innodb_flush_neighbors = {}", self.flush_neighbors),
        ] {
            sqlx::query(sqlx::AssertSqlSafe(sql)).execute(pool).await?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// `app_root_ex/lib/<os>/mariadb/` -- must contain `bin/`, `shared/`, `share/`.
    pub basedir: PathBuf,
    pub datadir: PathBuf,
    pub port: u16,
    pub bind_address: String,
    /// `innodb_buffer_pool_size` -- the "size it generously" knob from the plan. Bytes.
    pub buffer_pool_bytes: u64,
    /// Flush the log once a second instead of at every commit ([`WriteTuning`]). On unless
    /// the user turned it off.
    pub fast_writes: bool,
    /// What `datadir` is on, for [`WriteTuning`].
    pub disk: super::config::DiskKind,
    /// Where `mariadbd` writes its error log (inside `datadir`'s parent by default).
    pub log_error: Option<PathBuf>,
    /// `root@127.0.0.1`'s password, once one has been set on this datadir.
    ///
    /// `None` means the account is still passwordless, which is its state from
    /// [`bootstrap`](MariaDbServer::bootstrap) until [`set_root_password`]
    /// (MariaDbServer::set_root_password) runs. After that, every later run of this server
    /// has to present it -- readiness polling and graceful shutdown both connect as root --
    /// so whoever reconstructs a `ServerConfig` for an existing datadir must supply it.
    pub root_password: Option<String>,
}

/// An absolute form of `path`, without requiring it to exist yet (`canonicalize` would).
fn absolute(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        return path;
    }
    std::env::current_dir().map(|cwd| cwd.join(&path)).unwrap_or(path)
}

/// The default pool: [`BufferPool::default`](super::config::BufferPool), a quarter of this
/// machine's memory. The `database.env` setting replaces it via [`ServerConfig::with_buffer_pool`].
///
/// History: a fixed 256 MB was a sixteenth of a full catalogue (3.9 GB with every DAT and the
/// Hasheous dump imported), so the cache never held it: one page of the title list read
/// 41,000 pages (640 MB) back from disk, every time. With the catalogue in memory the same
/// query went from 4.8 s to 2.3 s, and the release list from 7.0 s to 4.9 s. InnoDB commits
/// the pool as it fills, so a small catalogue does not cost the full size.
fn default_buffer_pool_bytes() -> u64 {
    super::config::BufferPool::default().bytes()
}

impl ServerConfig {
    pub fn new(basedir: impl Into<PathBuf>, datadir: impl Into<PathBuf>, port: u16) -> Self {
        Self {
            basedir: absolute(basedir.into()),
            // mariadbd resolves a relative `--datadir` against its *basedir*, not against the
            // working directory -- so a relative one becomes `<bundled mariadb>/tests/...` and
            // fails with "Can't change dir", naming a path nobody passed. Making it absolute
            // here is the only place that can't be forgotten: the app root legitimately comes
            // in relative (`ROMEXPLORER_APP_ROOT=tests/output/...`).
            datadir: absolute(datadir.into()),
            port,
            bind_address: "127.0.0.1".to_string(),
            buffer_pool_bytes: default_buffer_pool_bytes(),
            fast_writes: true,
            disk: super::config::DiskKind::Auto,
            log_error: None,
            root_password: None,
        }
    }

    /// The cache size the user chose (`db_buffer_pool` in `database.env`).
    pub fn with_buffer_pool(mut self, pool: super::config::BufferPool) -> Self {
        self.buffer_pool_bytes = pool.bytes();
        self
    }

    /// The write mode the user chose (`db_fast_writes` in `database.env`).
    pub fn with_fast_writes(mut self, fast: bool) -> Self {
        self.fast_writes = fast;
        self
    }

    /// The disk the user chose (`db_disk` in `database.env`).
    pub fn with_disk(mut self, disk: super::config::DiskKind) -> Self {
        self.disk = disk;
        self
    }

    pub fn write_tuning(&self) -> WriteTuning {
        WriteTuning::new(self.fast_writes, self.disk, &self.datadir)
    }

    /// The root password for a datadir that already has one.
    pub fn with_root_password(mut self, password: impl Into<String>) -> Self {
        let password = password.into();
        self.root_password = (!password.is_empty()).then_some(password);
        self
    }

    fn mariadbd(&self) -> PathBuf {
        let name = if cfg!(windows) { "mariadbd.exe" } else { "mariadbd" };
        self.basedir.join("bin").join(name)
    }

    /// A `mariadbd` command that can find its DLLs. Linux resolves `shared/` through
    /// `mariadbd`'s own `$ORIGIN/../shared` rpath; Windows has no rpath and only searches the
    /// exe's directory and `PATH`, so `shared/` (`server.dll`, OpenSSL, the MSVC runtime) is
    /// prepended to the child's `PATH` -- without it the process dies with
    /// STATUS_DLL_NOT_FOUND before reading any input.
    fn mariadbd_command(&self) -> Command {
        let mut command = Command::new(self.mariadbd());
        // Off our console. Sharing it, `mariadbd` got every Ctrl+C and window close itself and
        // terminated on the spot -- before our `SHUTDOWN`, with requests still being served
        // -- so InnoDB crash-recovered on the next start. It stops when we tell it to.
        #[cfg(windows)]
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        if cfg!(windows) {
            let mut paths = vec![self.plugin_dir()];
            paths.extend(std::env::var_os("PATH").iter().flat_map(std::env::split_paths));
            if let Ok(joined) = std::env::join_paths(paths) {
                command.env("PATH", joined);
            }
        }
        command
    }

    fn share_dir(&self) -> PathBuf {
        self.basedir.join("share")
    }

    fn plugin_dir(&self) -> PathBuf {
        self.basedir.join("shared")
    }

    fn tmpdir(&self) -> PathBuf {
        self.datadir.parent().map(|p| p.join("tmp")).unwrap_or_else(|| self.datadir.join("tmp"))
    }

    fn log_error_path(&self) -> PathBuf {
        self.log_error.clone().unwrap_or_else(|| self.datadir.parent().map(|p| p.join("mariadb.err")).unwrap_or_else(|| self.datadir.join("mariadb.err")))
    }

    /// Unix socket path: kept under the OS temp dir with a short name because the kernel
    /// limits socket paths to ~107 bytes and `datadir` may be deep. Unused on Windows.
    fn socket_path(&self) -> PathBuf {
        std::env::temp_dir().join(format!("romexplorer-mariadb-{}.sock", self.port))
    }

    /// Options common to bootstrap and normal runs.
    fn base_args(&self) -> Vec<String> {
        vec![
            "--no-defaults".to_string(),
            format!("--basedir={}", self.basedir.display()),
            format!("--datadir={}", self.datadir.display()),
            format!("--lc-messages-dir={}", self.share_dir().display()),
            format!("--plugin-dir={}", self.plugin_dir().display()),
            format!("--tmpdir={}", self.tmpdir().display()),
            "--character-set-server=utf8mb4".to_string(),
        ]
    }
}

/// Privilege level granted by [`MariaDbServer::create_user`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserPrivileges {
    /// `ALL PRIVILEGES ON *.* WITH GRANT OPTION` -- global, not scoped to one database.
    /// For the account migrations and maintenance run as. Deliberately global rather than
    /// database-scoped: `CREATE USER` (needed so this account can itself create further
    /// accounts, e.g. the app account) is a global-only privilege in MariaDB -- there is
    /// no such thing as a database-scoped grant of it.
    Admin,
    /// `SELECT, INSERT, UPDATE, DELETE` on just the target database -- least privilege
    /// for the app's own day-to-day account; migrations always run as `Admin`, never this
    /// account, and this account can never create further accounts of its own.
    App,
}

/// Escapes a value for use inside a single-quoted SQL string literal.
/// MariaDB's "access denied for user" -- SQLSTATE 28000, error 1045.
fn is_access_denied(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("28000"))
}

fn escape_literal(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Escapes a value for use inside a backtick-quoted SQL identifier.
fn escape_identifier(value: &str) -> String {
    value.replace('`', "``")
}

pub struct MariaDbServer {
    config: ServerConfig,
    child: Option<Child>,
}

impl MariaDbServer {
    pub fn new(config: ServerConfig) -> Self {
        Self { config, child: None }
    }

    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    /// True once the data directory holds a bootstrapped `mysql` system database.
    pub fn is_bootstrapped(&self) -> bool {
        self.config.datadir.join("mysql").is_dir()
    }

    /// Creates `datadir` and runs `mariadbd --bootstrap` over the bundled `share/*.sql`,
    /// exactly as `mariadb-install-db` would. A no-op when already bootstrapped.
    pub async fn bootstrap(&self) -> Result<(), DatabaseError> {
        if self.is_bootstrapped() {
            return Ok(());
        }
        let cfg = &self.config;
        let mariadbd = cfg.mariadbd();
        if !mariadbd.is_file() {
            return Err(DatabaseError::Server(format!("mariadbd not found at {}", mariadbd.display())));
        }
        std::fs::create_dir_all(&cfg.datadir)?;
        std::fs::create_dir_all(cfg.tmpdir())?;

        let mut sql = String::from("create database if not exists mysql;\nuse mysql;\nSET @auth_root_socket=NULL;\n");
        for name in BOOTSTRAP_SQL {
            let path = cfg.share_dir().join(name);
            let text = std::fs::read_to_string(&path).map_err(|err| DatabaseError::Server(format!("bootstrap SQL {} unreadable: {err}", path.display())))?;
            sql.push_str(&text);
            sql.push('\n');
        }

        let mut args = cfg.base_args();
        args.extend(
            ["--bootstrap", "--log-warnings=0", "--enforce-storage-engine=", "--max_allowed_packet=8M", "--net_buffer_length=16K", "--skip-log-error"].iter().map(|s| s.to_string()),
        );
        let mut child = cfg.mariadbd_command().args(&args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().map_err(|err| DatabaseError::Server(format!("spawn bootstrap: {err}")))?;
        {
            // A write failure here is almost always EPIPE, and EPIPE means mariadbd has
            // already exited -- which means the *reason* it exited is sitting in stderr,
            // waiting to be read. Propagating the write error instead would replace a
            // precise diagnosis ("unknown variable", "can't create file") with "Broken
            // pipe", which says nothing about what to fix. So the write error is kept only
            // as a fallback, and the child is reaped either way.
            let mut stdin = child.stdin.take().expect("piped stdin");
            if stdin.write_all(sql.as_bytes()).await.is_ok() {
                let _ = stdin.shutdown().await;
            }
        }
        let output = child.wait_with_output().await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let tail: String = stderr.lines().rev().take(20).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
            return Err(DatabaseError::Server(format!("bootstrap failed ({}):\n{tail}", output.status)));
        }
        if !self.is_bootstrapped() {
            return Err(DatabaseError::Server("bootstrap finished but no mysql/ system database was created".to_string()));
        }
        Ok(())
    }

    /// Spawns `mariadbd` bound to `bind_address:port` and waits until it accepts a query.
    /// Bootstraps first if needed.
    pub async fn start(&mut self) -> Result<(), DatabaseError> {
        if self.child.is_some() {
            return Ok(());
        }
        self.bootstrap().await?;
        let cfg = self.config.clone();
        std::fs::create_dir_all(cfg.tmpdir())?;
        let mut args = cfg.base_args();
        args.push(format!("--bind-address={}", cfg.bind_address));
        args.push(format!("--port={}", cfg.port));
        args.push(format!("--log-error={}", cfg.log_error_path().display()));
        args.push(format!("--innodb-buffer-pool-size={}", cfg.buffer_pool_bytes));
        // Across a restart, keep the whole cache, not MariaDB's default 25% of it: the page
        // list is written at shutdown and read back in the background at start, so the first
        // screens after a restart come from memory instead of each paying for its own reads.
        // (The list is page ids, a few MB however large the pool is.)
        args.push("--innodb-buffer-pool-dump-at-shutdown=ON".to_string());
        args.push("--innodb-buffer-pool-load-at-startup=ON".to_string());
        args.push("--innodb-buffer-pool-dump-pct=100".to_string());
        // How hard it writes ([`WriteTuning`]): by default the log is flushed once a second,
        // not per commit, and background writing is paced for the disk the data is on.
        args.extend(cfg.write_tuning().args());
        // A redo log of MariaDB's default 96 MB fills within seconds of a DAT import, and
        // every time it does the writers wait for a checkpoint. 1 GB is a gigabyte of disk
        // and a somewhat longer crash recovery for bulk writes that run level.
        args.push("--innodb-log-file-size=1G".to_string());
        // Multi-row inserts rely on consecutive auto-increment ids (see import.rs); mode 1
        // guarantees that for a single INSERT ... VALUES (...),(...) statement.
        args.push("--innodb-autoinc-lock-mode=1".to_string());
        args.push("--max-allowed-packet=64M".to_string());
        args.push("--skip-name-resolve".to_string());
        if cfg!(unix) {
            args.push(format!("--socket={}", cfg.socket_path().display()));
        }
        let child = cfg.mariadbd_command().args(&args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true).spawn().map_err(|err| DatabaseError::Server(format!("spawn mariadbd: {err}")))?;
        self.child = Some(child);
        match self.wait_ready(Duration::from_secs(60)).await {
            Ok(()) => Ok(()),
            Err(err) => {
                let log_tail = std::fs::read_to_string(cfg.log_error_path()).map(|t| t.lines().rev().take(15).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")).unwrap_or_default();
                let _ = self.kill().await;
                Err(DatabaseError::Server(format!("{err}\nerror log tail:\n{log_tail}")))
            }
        }
    }

    /// Polls with a real `SELECT 1` (not just a TCP connect: the port opens before the
    /// server is fully up) until it succeeds or `timeout` elapses.
    pub async fn wait_ready(&mut self, timeout: Duration) -> Result<(), DatabaseError> {
        let started = Instant::now();
        let options = self.root_options();
        loop {
            if let Some(child) = &mut self.child {
                if let Ok(Some(status)) = child.try_wait() {
                    return Err(DatabaseError::Server(format!("mariadbd exited early with {status}")));
                }
            }
            match sqlx::mysql::MySqlConnection::connect_with(&options).await {
                Ok(mut conn) => {
                    if sqlx::query("SELECT 1").execute(&mut conn).await.is_ok() {
                        let _ = conn.close().await;
                        return Ok(());
                    }
                }
                // "Access denied" is still an answer, and only a running server gives it.
                // Readiness is the question here, not whether we hold the right password --
                // treating this as not-ready is what used to make a datadir with a root
                // password spin for the whole timeout and then report a server that was
                // in fact up and serving.
                Err(e) if is_access_denied(&e) => return Ok(()),
                Err(_) => {}
            }
            if started.elapsed() > timeout {
                return Err(DatabaseError::Server(format!("mariadbd did not become ready on port {} within {timeout:?}", self.config.port)));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Connect options for `root@127.0.0.1`, with the password when this datadir has one.
    fn root_options(&self) -> sqlx::mysql::MySqlConnectOptions {
        let options = Self::passwordless_root_options(&self.config);
        match &self.config.root_password {
            Some(p) => options.password(p),
            None => options,
        }
    }

    /// Root as it exists immediately after `bootstrap`: no password at all. Only
    /// [`set_root_password`](Self::set_root_password) should use this -- everything else
    /// wants [`root_options`](Self::root_options), which presents the password once set.
    fn passwordless_root_options(config: &ServerConfig) -> sqlx::mysql::MySqlConnectOptions {
        sqlx::mysql::MySqlConnectOptions::new().host(&config.bind_address).port(config.port).username("root")
    }

    /// Sets the passwordless `root@127.0.0.1` account's password (the only account that
    /// exists right after [`bootstrap`](Self::bootstrap)). Connects as today's passwordless
    /// root, so this only works before a password has already been set. Never logs or
    /// returns the password.
    pub async fn set_root_password(&mut self, password: &str) -> Result<(), DatabaseError> {
        let mut conn = sqlx::mysql::MySqlConnection::connect_with(&Self::passwordless_root_options(&self.config)).await?;
        let alter = format!("ALTER USER 'root'@'127.0.0.1' IDENTIFIED BY '{}'", escape_literal(password));
        sqlx::query(sqlx::AssertSqlSafe(alter)).execute(&mut conn).await?;
        sqlx::query("FLUSH PRIVILEGES").execute(&mut conn).await?;
        conn.close().await?;
        // Remember it, or this very process could no longer shut the server down cleanly.
        self.config.root_password = Some(password.to_string());
        Ok(())
    }

    /// Creates `user@127.0.0.1` (no-op if it already exists) and grants it `privileges` on
    /// `database`, connecting with `admin`'s own credentials (whichever account currently
    /// holds DDL rights -- root itself right after bootstrap, or an already-created admin
    /// account on any later call). Never logs or returns the password.
    ///
    /// Neither the username nor the password can be bound as an ordinary `?` placeholder --
    /// MariaDB's binary-protocol prepared statements reject one anywhere in `CREATE USER`/
    /// `ALTER USER`'s `user_name@host_name IDENTIFIED BY auth_string` grammar (confirmed:
    /// `ALTER USER ... IDENTIFIED BY ?` fails with a syntax error). Both are instead quoted
    /// as string literals and escaped, the same way `ensure_database` already handles
    /// `CREATE DATABASE`'s own unbindable identifier.
    pub async fn create_user(&self, admin: &super::config::DatabaseConfig, user: &str, password: &str, database: &str, privileges: UserPrivileges) -> Result<(), DatabaseError> {
        let mut conn = sqlx::mysql::MySqlConnection::connect_with(&admin.connect_options(false)).await?;
        let create = format!("CREATE USER IF NOT EXISTS '{}'@'127.0.0.1' IDENTIFIED BY '{}'", escape_literal(user), escape_literal(password));
        sqlx::query(sqlx::AssertSqlSafe(create)).execute(&mut conn).await?;
        // `IF NOT EXISTS` leaves an existing account's password as it was: set it, or the
        // account keeps one the config no longer has and every login fails.
        let password_now = format!("ALTER USER '{}'@'127.0.0.1' IDENTIFIED BY '{}'", escape_literal(user), escape_literal(password));
        sqlx::query(sqlx::AssertSqlSafe(password_now)).execute(&mut conn).await?;
        let grant = match privileges {
            UserPrivileges::Admin => format!("GRANT ALL PRIVILEGES ON *.* TO '{}'@'127.0.0.1' WITH GRANT OPTION", escape_literal(user)),
            UserPrivileges::App => format!("GRANT SELECT, INSERT, UPDATE, DELETE ON `{}`.* TO '{}'@'127.0.0.1'", escape_identifier(database), escape_literal(user)),
        };
        sqlx::query(sqlx::AssertSqlSafe(grant)).execute(&mut conn).await?;
        if privileges == UserPrivileges::App {
            // Read-only server state: lets the catalog sync see open transactions
            // (`sync_rev::stable`). It shows other sessions' queries, nothing else.
            sqlx::query(sqlx::AssertSqlSafe(format!("GRANT PROCESS ON *.* TO '{}'@'127.0.0.1'", escape_literal(user)))).execute(&mut conn).await?;
        }
        sqlx::query("FLUSH PRIVILEGES").execute(&mut conn).await?;
        conn.close().await?;
        Ok(())
    }

    /// Graceful stop via the SQL `SHUTDOWN` statement, falling back to killing the process
    /// if the server hasn't exited within `timeout`.
    pub async fn shutdown(&mut self, timeout: Duration) -> Result<(), DatabaseError> {
        let Some(mut child) = self.child.take() else { return Ok(()) };
        if let Ok(mut conn) = sqlx::mysql::MySqlConnection::connect_with(&self.root_options()).await {
            let _ = sqlx::query("SHUTDOWN").execute(&mut conn).await;
        }
        let started = Instant::now();
        loop {
            if let Ok(Some(_)) = child.try_wait() {
                let _ = std::fs::remove_file(self.config.socket_path());
                return Ok(());
            }
            if started.elapsed() > timeout {
                let _ = child.kill().await;
                let _ = child.wait().await;
                let _ = std::fs::remove_file(self.config.socket_path());
                return Err(DatabaseError::Server("mariadbd ignored SHUTDOWN; killed".to_string()));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn kill(&mut self) -> Result<(), DatabaseError> {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        Ok(())
    }

    /// Starts it again after it stopped on its own. The dead process is let go of first:
    /// [`Self::start`] does nothing while it still holds one.
    pub async fn restart_after_exit(&mut self) -> Result<(), DatabaseError> {
        if let Some(mut child) = self.child.take() {
            let _ = child.wait().await;
        }
        self.start().await
    }

    /// The last `lines` lines of the server's error log: why it stopped, when it did.
    pub fn error_log_tail(&self, lines: usize) -> String {
        std::fs::read_to_string(self.config.log_error_path()).map(|t| t.lines().rev().take(lines).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")).unwrap_or_default()
    }

    /// Where the error log is.
    pub fn error_log_path(&self) -> PathBuf {
        self.config.log_error_path()
    }

    pub fn is_running(&mut self) -> bool {
        match &mut self.child {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }
}

impl Drop for MariaDbServer {
    fn drop(&mut self) {
        // `kill_on_drop` on the Child handles the process; the socket file is cleaned up on
        // a graceful `shutdown()`, which callers are expected to prefer.
        let _ = std::fs::remove_file(self.config.socket_path());
    }
}

/// Picks a free TCP port on `127.0.0.1` (for tests and multi-instance dev runs).
pub fn free_port() -> std::io::Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

/// Sanity check that `basedir` looks like the bundled MariaDB layout this module expects.
pub fn check_layout(basedir: &Path) -> Result<(), DatabaseError> {
    let name = if cfg!(windows) { "mariadbd.exe" } else { "mariadbd" };
    let missing: Vec<String> = [basedir.join("bin").join(name), basedir.join("share").join("english").join("errmsg.sys"), basedir.join("share").join(BOOTSTRAP_SQL[0])]
        .iter()
        .filter(|p| !p.is_file())
        .map(|p| p.display().to_string())
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(DatabaseError::Server(format!("bundled MariaDB layout incomplete under {}; missing: {}", basedir.display(), missing.join(", "))))
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn a_relative_datadir_is_made_absolute() {
        // mariadbd resolves a relative --datadir against its basedir, which silently points
        // it inside the bundled MariaDB tree. This is what stops that.
        let cfg = ServerConfig::new("lib/mariadb", "tests/output/x/data/mariadb", 3399);
        assert!(cfg.datadir.is_absolute(), "datadir stayed relative: {}", cfg.datadir.display());
        assert!(cfg.basedir.is_absolute(), "basedir stayed relative: {}", cfg.basedir.display());
        assert!(cfg.datadir.ends_with("tests/output/x/data/mariadb"));
    }

    #[test]
    fn an_absolute_datadir_is_left_alone() {
        // A drive letter is part of "absolute" on Windows: `/var/lib/...` there is rooted but
        // relative to the current drive, and gets that drive prepended -- correctly.
        let (base, data) = if cfg!(windows) {
            (r"C:\opt\mariadb", r"C:\var\lib\romexplorer")
        } else {
            ("/opt/mariadb", "/var/lib/romexplorer")
        };
        let cfg = ServerConfig::new(base, data, 3399);
        assert_eq!(cfg.datadir, PathBuf::from(data));
        assert_eq!(cfg.basedir, PathBuf::from(base));
    }
}

/// Grants PROCESS to an existing `user@127.0.0.1`, over a connection with grant rights. For an
/// app account created before the catalog sync needed it ([`MariaDbServer::create_user`]).
pub async fn grant_process(pool: &sqlx::MySqlPool, user: &str) -> Result<(), DatabaseError> {
    sqlx::query(sqlx::AssertSqlSafe(format!("GRANT PROCESS ON *.* TO '{}'@'127.0.0.1'", escape_literal(user)))).execute(pool).await?;
    Ok(())
}
