//! Connection settings and the app's on-disk layout.
//!
//! [`DatabaseConfig`] covers both deployment shapes without any per-shape code: a client on
//! the same machine as the (embedded, see `server.rs`) server (`host = "127.0.0.1"`) and a
//! client talking to a MariaDB instance on a fileserver -- both are just a different `host`
//! to the same [`super::connection::connect`].
//!
//! [`AppPaths`] is everything that hangs off the app root (the directory the executables
//! ship in; `app_root_ex/` is this repository's stand-in for it): the bundled MariaDB under
//! `lib/<os>/<arch>/mariadb/`, its data dir, the DAT
//! download tree, the provider raw-response cache, downloaded media, and `assets/api.env`
//! (the one and only place provider credentials live).

use std::env;
use std::path::{Path, PathBuf};

/// `ROMEXPLORER_DB_*` env vars, read once by [`Self::from_env`].
#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub database: String,
    /// `sqlx::mysql::MySqlPoolOptions::max_connections`. Modest by default: the GUI/API side
    /// is a short-read workload; the worker's imports batch their writes.
    pub max_connections: u32,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            // Not 3306: the embedded server must never collide with a system-wide MySQL/MariaDB.
            port: 3307,
            user: "root".to_string(),
            password: String::new(),
            database: "romexplorer".to_string(),
            max_connections: 10,
        }
    }
}

impl DatabaseConfig {
    /// Reads `ROMEXPLORER_DB_{HOST,PORT,USER,PASSWORD,DATABASE,MAX_CONNECTIONS}`, falling
    /// back to [`Self::default`]'s value for any that aren't set (or don't parse).
    /// Default, then `data/database.env`, then the environment -- **in that order**, so an
    /// explicitly exported `ROMEXPLORER_DB_PORT` still wins over a stored value. That is the
    /// direction that keeps a one-off run (a test against another port, a second instance)
    /// from needing the file edited and put back.
    pub fn load(paths: &AppPaths) -> Self {
        StoredConfig::load(paths).database
    }

    pub fn from_env() -> Self {
        let default = Self::default();
        Self {
            host: env::var("ROMEXPLORER_DB_HOST").unwrap_or(default.host),
            port: env::var("ROMEXPLORER_DB_PORT").ok().and_then(|v| v.parse().ok()).unwrap_or(default.port),
            user: env::var("ROMEXPLORER_DB_USER").unwrap_or(default.user),
            password: env::var("ROMEXPLORER_DB_PASSWORD").unwrap_or(default.password),
            database: env::var("ROMEXPLORER_DB_DATABASE").unwrap_or(default.database),
            max_connections: env::var("ROMEXPLORER_DB_MAX_CONNECTIONS").ok().and_then(|v| v.parse().ok()).unwrap_or(default.max_connections),
        }
    }

    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    pub fn with_database(mut self, database: impl Into<String>) -> Self {
        self.database = database.into();
        self
    }

    pub fn with_user(mut self, user: impl Into<String>) -> Self {
        self.user = user.into();
        self
    }

    pub fn with_password(mut self, password: impl Into<String>) -> Self {
        self.password = password.into();
        self
    }

    pub fn with_host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    /// The `mysql://user:password@host:port/database` URL `sqlx` connects with.
    pub fn to_url(&self) -> String {
        format!("mysql://{}:{}@{}:{}/{}", urlencode(&self.user), urlencode(&self.password), self.host, self.port, self.database)
    }

    /// `sqlx` connect options for this config (with or without the database selected --
    /// `None` is what `CREATE DATABASE` needs).
    pub fn connect_options(&self, with_database: bool) -> sqlx::mysql::MySqlConnectOptions {
        let mut options = sqlx::mysql::MySqlConnectOptions::new().host(&self.host).port(self.port).username(&self.user);
        if !self.password.is_empty() {
            options = options.password(&self.password);
        }
        if with_database {
            options = options.database(&self.database);
        }
        options
    }
}

/// Percent-encodes the few characters that would otherwise break the `user:password@host`
/// section of the URL (`:`, `@`, `/`, `%`, whitespace), plus any non-ASCII character (each
/// UTF-8 byte of it, individually). Not a general URL encoder.
pub(crate) fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii() && !matches!(ch, ':' | '@' | '/' | '%' | ' ' | '?' | '#' | '&' | '=' | '+') {
            out.push(ch);
            continue;
        }
        let mut buf = [0u8; 4];
        for byte in ch.encode_utf8(&mut buf).as_bytes() {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The `app_root_ex/` layout. See this module's doc comment.
#[derive(Debug, Clone)]
pub struct AppPaths {
    /// `app_root_ex/` itself.
    pub root: PathBuf,
}

impl AppPaths {
    /// `ROMEXPLORER_APP_ROOT` if set; otherwise wherever [`Self::root_for_exe`] finds one
    /// for the running executable; otherwise (dev builds, tests) `<crate>/app_root_ex/`.
    pub fn detect() -> Self {
        if let Ok(root) = env::var("ROMEXPLORER_APP_ROOT") {
            return Self { root: PathBuf::from(root) };
        }
        if let Some(root) = env::current_exe().ok().and_then(|exe| Self::root_for_exe(&exe)) {
            return Self { root };
        }
        Self { root: crate::rom::native_lib::source_root().join("app_root_ex") }
    }

    /// The app root for an executable at `exe`, recognised by its `lib/`.
    ///
    /// A shipped package *is* the app root: the executables sit at its top level beside
    /// `lib/` and `assets/`, so the executable's own directory is the first place to look.
    /// `app_root_ex/` is only this repository's imitation of that directory -- the name
    /// exists because a checkout's top level is full of other things -- and is what the walk
    /// upwards finds for a binary run from `target/`.
    fn root_for_exe(exe: &Path) -> Option<PathBuf> {
        let dir = exe.parent()?;
        // Inside a macOS `.app` the executable is in `Contents/MacOS/` and everything else
        // has to be in `Contents/Resources/`, so the root is beside the executable's
        // directory rather than being it -- and `Info.plist`'s `LSEnvironment` can't say so
        // instead, because modern macOS ignores that key.
        #[cfg(target_os = "macos")]
        if let Some(resources) = dir.parent().map(|contents| contents.join("Resources")) {
            if resources.join("lib").is_dir() {
                return Some(resources);
            }
        }
        if dir.join("lib").is_dir() {
            return Some(dir.to_path_buf());
        }
        dir.ancestors().map(|d| d.join("app_root_ex")).find(|candidate| candidate.join("lib").is_dir())
    }

    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The bundled-native-library directory this build runs from: `lib/<os>/<arch>/`, e.g.
    /// `lib/linux/x64/`, `lib/macos/arm64/` (matching the `deps/<name>/release/<platform>/<arch>/`
    /// layout the submodule build scripts produce). The arch level exists so one app root
    /// can carry several architectures of the same OS.
    pub fn os_lib_dir(&self) -> PathBuf {
        self.root.join("lib").join(Self::os_dir_name()).join(Self::arch_dir_name())
    }

    /// Delegates to `rom::native_lib::os_dir_name` -- the canonical definition now lives
    /// there (consolidated so it can't drift from `platform_arch_dirs`'s submodule-facing
    /// `mac` naming the way it briefly had before).
    pub fn os_dir_name() -> &'static str {
        crate::rom::native_lib::os_dir_name()
    }

    /// Delegates to `rom::native_lib::arch_dir_name` -- `x64` / `arm64`, the same spelling
    /// the submodule build scripts use.
    pub fn arch_dir_name() -> &'static str {
        crate::rom::native_lib::arch_dir_name()
    }

    /// `lib/<os>/<arch>/mariadb/` -- `bin/`, `shared/`, `share/` (bootstrap SQL, errmsg.sys, charsets).
    pub fn mariadb_dir(&self) -> PathBuf {
        self.os_lib_dir().join("mariadb")
    }

    /// The embedded server's data directory (InnoDB files). Git-ignored, machine-local.
    /// `ROMEXPLORER_DATA_DIR` overrides the parent `data/` dir if set (matching
    /// `ROMEXPLORER_APP_ROOT`'s style) -- the permanent app-data home is independent of
    /// `app_root_ex` on machines that want it elsewhere (e.g. a dedicated data volume).
    pub fn mariadb_data_dir(&self) -> PathBuf {
        Self::data_root(&self.root).join("mariadb")
    }

    /// The desktop GUI's webview data (WebView2's cache and storage on Windows), kept in
    /// the app's own `data/` so nothing lands in `%LOCALAPPDATA%`.
    pub fn webview_data_dir(&self) -> PathBuf {
        Self::data_root(&self.root).join("webview")
    }

    fn data_root(root: &Path) -> PathBuf {
        env::var("ROMEXPLORER_DATA_DIR").map(PathBuf::from).unwrap_or_else(|_| root.join("data"))
    }

    /// `data/database.env` -- the connection settings chosen at first run, so the app can
    /// be started again without any environment variables. Owner-only: it holds passwords.
    /// See [`StoredConfig`].
    pub fn database_env_path(&self) -> PathBuf {
        Self::data_root(&self.root).join("database.env")
    }

    /// `data/backups/` -- where `api::backup` writes, unless `backup_dir` says otherwise.
    pub fn backups_dir(&self) -> PathBuf {
        Self::data_root(&self.root).join("backups")
    }

    /// `data/server_token` -- the REST server's shared bearer token (docs/plan/api.md).
    /// Generated on first server start, owner-only permissions, git-ignored with the rest
    /// of `data/`, never logged.
    pub fn server_token_path(&self) -> PathBuf {
        Self::data_root(&self.root).join("server_token")
    }

    /// `data/master.key` -- the key that seals provider credentials and peer tokens at rest
    /// (`database::vault`). `ROMEXPLORER_MASTER_KEY_FILE` puts it elsewhere, e.g. on a volume
    /// that is not backed up alongside the data.
    pub fn master_key_path(&self) -> PathBuf {
        env::var("ROMEXPLORER_MASTER_KEY_FILE").map(PathBuf::from).unwrap_or_else(|_| Self::data_root(&self.root).join("master.key"))
    }

    /// `data/auth.env` -- first accounts written by an installer, consumed once on start and
    /// then deleted (`api::auth::seed`).
    pub fn auth_env_path(&self) -> PathBuf {
        Self::data_root(&self.root).join("auth.env")
    }

    /// `data/tls/` -- the listener's certificate and key (`server::tls`).
    pub fn tls_dir(&self) -> PathBuf {
        Self::data_root(&self.root).join("tls")
    }

    /// `assets/download/dats/<source>/...` -- downloaded reference DATs.
    /// Under [`Self::assets_root`]: this is written at run time and can reach gigabytes.
    pub fn dats_dir(&self) -> PathBuf {
        Self::assets_root(&self.root).join("download").join("dats")
    }

    /// `assets/cache/provider_responses/<provider>/<key>.<ext>` -- raw provider responses'
    /// permanent home. `ROMEXPLORER_CACHE_DIR` overrides the parent `cache/` dir if set.
    pub fn provider_cache_dir(&self) -> PathBuf {
        Self::cache_root(&self.root).join("provider_responses")
    }

    fn cache_root(root: &Path) -> PathBuf {
        env::var("ROMEXPLORER_CACHE_DIR").map(PathBuf::from).unwrap_or_else(|_| Self::assets_root(root).join("cache"))
    }

    /// The writable half of `assets/`, overridden by `ROMEXPLORER_ASSETS_DIR`.
    ///
    /// `data/` and `assets/cache/` have had an override for a while; DATs, media, icons and
    /// `api.env` had not, and they are written at run time just the same. That is fine while
    /// the app root is the source tree, and wrong in every shape we ship: an AppImage's
    /// squashfs is read-only, a `/opt` install is root-owned and the service runs as someone
    /// else, and a container image's layer is not where gigabytes of cover art should land.
    /// One explicit variable, rather than probing whether the directory happens to be
    /// writable -- that answer is racy, and a path that moves depending on a `chmod` is worse
    /// than one that moves depending on an environment variable.
    ///
    /// `gui_web/` deliberately stays on `self.root`: it ships inside the package and is only
    /// ever read.
    fn assets_root(root: &Path) -> PathBuf {
        env::var("ROMEXPLORER_ASSETS_DIR").map(PathBuf::from).unwrap_or_else(|_| root.join("assets"))
    }

    /// `assets/media/<provider>/...` -- downloaded cover art etc. Under
    /// [`Self::assets_root`]: written at run time, and the largest thing here by far.
    pub fn media_dir(&self) -> PathBuf {
        Self::assets_root(&self.root).join("media")
    }

    /// `assets/icons/<platform-slug>.png|svg|...` -- optional user-provided system icons the
    /// console shows before platform names (e.g. a ScreenScraper-style pictogram pack).
    pub fn icons_dir(&self) -> PathBuf {
        Self::assets_root(&self.root).join("icons")
    }

    /// `assets/i18n/<lang>.json` -- the UI's text per language (`api::i18n`). Shipped and
    /// only read, like `gui_web/`, so it stays on `self.root`.
    pub fn i18n_dir(&self) -> PathBuf {
        self.root.join("assets").join("i18n")
    }

    /// `assets/gui_web/` -- the GUI's web build (`dx build --platform web`), served at
    /// `/app/` by `romexplorer_server` when present.
    pub fn gui_web_dir(&self) -> PathBuf {
        self.root.join("assets").join("gui_web")
    }

    /// `assets/api.env` -- provider credentials, read by `env_creds`. The app writes it
    /// only through `romexplorer_server encrypt-credentials`, which seals the values in
    /// place; credentials set from the UI go to the database instead. Under
    /// [`Self::assets_root`] because of that one writer.
    pub fn api_env(&self) -> PathBuf {
        Self::assets_root(&self.root).join("api.env")
    }
}

/// Everything first-run setup decides and every later run needs back: the connection
/// itself, the root password the embedded server must present to poll readiness and shut
/// down cleanly, where its data lives, and whether this machine shares its collection.
///
/// How big MariaDB's InnoDB buffer pool -- its in-memory copy of the tables and indexes --
/// is made: a share of this machine's RAM, or a fixed size.
///
/// Stored in `database.env` as `25%` or `16G` / `512M` (key `db_buffer_pool`), because
/// `mariadbd` takes it as a start-up option: it is needed before the database answers.
/// InnoDB commits the pool as it fills, so a generous size costs nothing until the
/// catalogue actually grows into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferPool {
    /// Percent of total RAM, 5-90.
    Percent(u8),
    /// Bytes.
    Fixed(u64),
}

/// What the embedded database's files are on, for how hard it may write (`db_disk` in
/// `database.env`; see `server::WriteTuning`). MariaDB's own defaults are a spinning disk's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiskKind {
    /// Whatever the data folder turns out to be on (`rom::hash::StorageKind::for_path`).
    #[default]
    Auto,
    Ssd,
    /// Never detected, only chosen: nothing tells an NVMe drive from a SATA SSD here.
    Nvme,
    Hdd,
}

impl DiskKind {
    pub fn parse(text: &str) -> Result<Self, String> {
        match text.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "ssd" => Ok(Self::Ssd),
            "nvme" => Ok(Self::Nvme),
            "hdd" => Ok(Self::Hdd),
            other => Err(format!("the database disk must be auto, ssd, nvme or hdd, not {other:?}")),
        }
    }

    pub fn to_text(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Ssd => "ssd",
            Self::Nvme => "nvme",
            Self::Hdd => "hdd",
        }
    }
}

impl Default for BufferPool {
    /// A quarter of RAM. This used to stop at 8 GB, which on a large machine left most of a
    /// full catalogue (3.9 GB with every DAT and the Hasheous dump, and growing) to be read
    /// back from disk once the cache churned; a share of RAM with no ceiling scales with the
    /// machine instead.
    fn default() -> Self {
        Self::Percent(25)
    }
}

impl BufferPool {
    pub const MIN_PERCENT: u8 = 5;
    pub const MAX_PERCENT: u8 = 90;
    /// MariaDB's own floor is lower, but below this the catalogue is all disk reads anyway.
    pub const MIN_BYTES: u64 = 128 << 20;

    /// `25%`, `16G`, `512M`, `1T`, or plain bytes.
    pub fn parse(text: &str) -> Result<Self, String> {
        let t = text.trim().to_ascii_uppercase();
        if let Some(p) = t.strip_suffix('%') {
            let p: u8 = p.trim().parse().map_err(|_| format!("not a percentage: {text:?}"))?;
            if !(Self::MIN_PERCENT..=Self::MAX_PERCENT).contains(&p) {
                return Err(format!("the database cache must be {}-{}% of memory, not {p}%", Self::MIN_PERCENT, Self::MAX_PERCENT));
            }
            return Ok(Self::Percent(p));
        }
        let t = t.strip_suffix('B').unwrap_or(&t);
        let (digits, shift) = match t.chars().last() {
            Some('K') => (&t[..t.len() - 1], 10),
            Some('M') => (&t[..t.len() - 1], 20),
            Some('G') => (&t[..t.len() - 1], 30),
            Some('T') => (&t[..t.len() - 1], 40),
            _ => (t, 0),
        };
        let n: u64 = digits.trim().parse().map_err(|_| format!("not a size: {text:?} (try 25% or 16G)"))?;
        let bytes = n.checked_shl(shift).filter(|b| b >> shift == n).ok_or_else(|| format!("too large: {text:?}"))?;
        if bytes < Self::MIN_BYTES {
            return Err(format!("the database cache must be at least {} MB", Self::MIN_BYTES >> 20));
        }
        Ok(Self::Fixed(bytes))
    }

    /// The stored spelling, which [`parse`](Self::parse) reads back.
    pub fn to_text(self) -> String {
        match self {
            Self::Percent(p) => format!("{p}%"),
            Self::Fixed(b) if b % (1 << 30) == 0 => format!("{}G", b >> 30),
            Self::Fixed(b) if b % (1 << 20) == 0 => format!("{}M", b >> 20),
            Self::Fixed(b) => b.to_string(),
        }
    }

    /// Bytes on a machine with `total_ram` bytes. A fixed size larger than 90% of RAM is cut
    /// to that -- a pool the OS has to swap out is slower than no pool.
    pub fn bytes_for(self, total_ram: u64) -> u64 {
        // Whole MB: MariaDB rounds the pool to its chunk size anyway, and it reads better.
        let share = |p: u8| ((u128::from(total_ram) * u128::from(p) / 100) as u64) >> 20 << 20;
        let ceiling = share(Self::MAX_PERCENT).max(Self::MIN_BYTES);
        match self {
            Self::Percent(p) => share(p).clamp(Self::MIN_BYTES, ceiling),
            Self::Fixed(b) => b.clamp(Self::MIN_BYTES, ceiling),
        }
    }

    /// [`bytes_for`](Self::bytes_for) this machine.
    pub fn bytes(self) -> u64 {
        self.bytes_for(total_ram())
    }
}

/// This machine's RAM in bytes.
pub fn total_ram() -> u64 {
    let mut system = sysinfo::System::new();
    system.refresh_memory();
    system.total_memory()
}

/// Stored as `data/database.env` (`KEY=VALUE`, owner-only) rather than in the `settings`
/// table, because it is what you need *in order to* open the database. Read by both the
/// desktop GUI and `romexplorer_server`, so `--init-from-config` sets the machine up once
/// instead of once per process.
#[derive(Debug, Clone, Default)]
pub struct StoredConfig {
    pub database: DatabaseConfig,
    /// `root@127.0.0.1`'s password once setup has set one. Needed by `ServerConfig`, not for
    /// day-to-day queries -- those use the admin or app account in `database`.
    pub root_password: Option<String>,
    /// Overrides `AppPaths::mariadb_data_dir()` when the user pointed setup at an existing
    /// database somewhere else.
    pub datadir: Option<PathBuf>,
    /// The port the REST listener uses when this machine shares its collection.
    pub server_port: Option<u16>,
    /// Serve the share API at all. Off by default: a client is not a server.
    pub share_enabled: bool,
    /// Advertise that share over mDNS. Only meaningful with `share_enabled` -- advertising
    /// a port nothing answers on would just point peers at nothing.
    pub share_advertise: bool,
    /// The embedded database's cache size (`db_buffer_pool`; `ROMEXPLORER_DB_BUFFER_POOL`
    /// overrides it).
    pub buffer_pool: BufferPool,
    /// Read the catalogue into that cache in the background after the database starts
    /// (`db_warm_cache`, on unless stored as off). `None` is the default, on.
    pub warm_cache: Option<bool>,
    /// Flush the embedded database's log about once a second instead of at every commit
    /// (`db_fast_writes`, on unless stored as off). `None` is the default, on.
    pub fast_writes: Option<bool>,
    /// What the embedded database's files are on (`db_disk`).
    pub disk: DiskKind,
}

impl StoredConfig {
    /// Whether first-run setup has happened on this machine.
    pub fn exists(paths: &AppPaths) -> bool {
        paths.database_env_path().is_file()
    }

    /// Defaults, overlaid with the stored file, overlaid with the environment.
    pub fn load(paths: &AppPaths) -> Self {
        Self::from_values(&super::env_file::load(&paths.database_env_path()).unwrap_or_default())
    }

    /// The mapping half of [`load`](Self::load), split out so it can be tested without
    /// touching the filesystem or the process environment -- the latter matters because
    /// these tests share a process with ones that set `ROMEXPLORER_*` and never unset it.
    fn from_values(values: &super::env_file::Values) -> Self {
        let text = |key: &str| values.get(key).map(String::as_str).filter(|v| !v.is_empty()).map(str::to_string);
        let number = |key: &str| text(key).and_then(|v| v.parse::<u16>().ok());
        let flag = |key: &str| matches!(text(key).as_deref(), Some("1" | "true" | "yes" | "on"));

        let d = DatabaseConfig::default();
        let stored = DatabaseConfig {
            host: text("db_host").unwrap_or(d.host),
            port: number("db_port").unwrap_or(d.port),
            user: text("db_user").unwrap_or(d.user),
            password: text("db_password").unwrap_or(d.password),
            database: text("db_database").unwrap_or(d.database),
            max_connections: text("db_max_connections").and_then(|v| v.parse().ok()).unwrap_or(d.max_connections),
        };

        // The environment has the last word.
        let database = DatabaseConfig {
            host: env::var("ROMEXPLORER_DB_HOST").unwrap_or(stored.host),
            port: env::var("ROMEXPLORER_DB_PORT").ok().and_then(|v| v.parse().ok()).unwrap_or(stored.port),
            user: env::var("ROMEXPLORER_DB_USER").unwrap_or(stored.user),
            password: env::var("ROMEXPLORER_DB_PASSWORD").unwrap_or(stored.password),
            database: env::var("ROMEXPLORER_DB_DATABASE").unwrap_or(stored.database),
            max_connections: env::var("ROMEXPLORER_DB_MAX_CONNECTIONS").ok().and_then(|v| v.parse().ok()).unwrap_or(stored.max_connections),
        };

        Self {
            database,
            root_password: text("db_root_password"),
            datadir: text("db_datadir").map(PathBuf::from),
            server_port: number("server_port"),
            share_enabled: flag("share_enabled"),
            share_advertise: flag("share_enabled") && flag("share_advertise"),
            // A bad stored value falls back to the default rather than stopping the start:
            // the screen that writes it validates, so only a hand edit gets here.
            buffer_pool: env::var("ROMEXPLORER_DB_BUFFER_POOL").ok().or_else(|| text("db_buffer_pool")).and_then(|v| BufferPool::parse(&v).ok()).unwrap_or_default(),
            warm_cache: text("db_warm_cache").map(|v| !matches!(v.as_str(), "0" | "false" | "no" | "off")),
            fast_writes: text("db_fast_writes").map(|v| !matches!(v.as_str(), "0" | "false" | "no" | "off")),
            disk: text("db_disk").and_then(|v| DiskKind::parse(&v).ok()).unwrap_or_default(),
        }
    }

    /// Whether the log is flushed once a second rather than at every commit (default on).
    pub fn fast_writes(&self) -> bool {
        self.fast_writes.unwrap_or(true)
    }

    /// Whether to warm the cache after start (default on).
    pub fn warm_cache(&self) -> bool {
        self.warm_cache.unwrap_or(true)
    }

    /// Where the embedded server's files live: the stored override, else the default.
    pub fn datadir(&self, paths: &AppPaths) -> PathBuf {
        self.datadir.clone().unwrap_or_else(|| paths.mariadb_data_dir())
    }

    pub fn save(&self, paths: &AppPaths) -> std::io::Result<()> {
        let mut pairs: Vec<(&str, String)> = vec![
            ("db_host", self.database.host.clone()),
            ("db_port", self.database.port.to_string()),
            ("db_user", self.database.user.clone()),
            ("db_password", self.database.password.clone()),
            ("db_database", self.database.database.clone()),
            ("share_enabled", self.share_enabled.to_string()),
            ("share_advertise", self.share_advertise.to_string()),
        ];
        // Kept, or saving any other setting here would quietly reset a raised limit.
        if self.database.max_connections != DatabaseConfig::default().max_connections {
            pairs.push(("db_max_connections", self.database.max_connections.to_string()));
        }
        if self.buffer_pool != BufferPool::default() {
            pairs.push(("db_buffer_pool", self.buffer_pool.to_text()));
        }
        if let Some(w) = self.warm_cache {
            pairs.push(("db_warm_cache", w.to_string()));
        }
        if let Some(f) = self.fast_writes {
            pairs.push(("db_fast_writes", f.to_string()));
        }
        if self.disk != DiskKind::Auto {
            pairs.push(("db_disk", self.disk.to_text().to_string()));
        }
        if let Some(p) = &self.root_password {
            pairs.push(("db_root_password", p.clone()));
        }
        if let Some(d) = &self.datadir {
            pairs.push(("db_datadir", d.display().to_string()));
        }
        if let Some(p) = self.server_port {
            pairs.push(("server_port", p.to_string()));
        }
        super::env_file::write(
            &paths.database_env_path(),
            "romexplorer connection settings, written by first-run setup.\nOwner-only: this file holds passwords. Delete it to run setup again.\nROMEXPLORER_DB_* environment variables override anything here.",
            &pairs,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_pool_parses_percent_and_sizes_and_round_trips() {
        assert_eq!(BufferPool::parse("25%"), Ok(BufferPool::Percent(25)));
        assert_eq!(BufferPool::parse(" 40 % "), Ok(BufferPool::Percent(40)));
        assert_eq!(BufferPool::parse("16G"), Ok(BufferPool::Fixed(16 << 30)));
        assert_eq!(BufferPool::parse("512mb"), Ok(BufferPool::Fixed(512 << 20)));
        assert_eq!(BufferPool::parse("1T"), Ok(BufferPool::Fixed(1 << 40)));
        assert!(BufferPool::parse("95%").is_err());
        assert!(BufferPool::parse("4%").is_err());
        assert!(BufferPool::parse("64M").is_err(), "under the floor");
        assert!(BufferPool::parse("lots").is_err());
        for p in [BufferPool::Percent(30), BufferPool::Fixed(48 << 30), BufferPool::Fixed(768 << 20)] {
            assert_eq!(BufferPool::parse(&p.to_text()), Ok(p));
        }
    }

    #[test]
    fn write_mode_and_disk_are_stored() {
        let values: super::super::env_file::Values = [("db_fast_writes".to_string(), "false".to_string()), ("db_disk".to_string(), "NVMe".to_string())].into_iter().collect();
        let c = StoredConfig::from_values(&values);
        assert_eq!((c.fast_writes(), c.disk), (false, DiskKind::Nvme));
        let unset = StoredConfig::from_values(&Default::default());
        assert_eq!((unset.fast_writes(), unset.disk), (true, DiskKind::Auto), "fast, and the disk found out, unless said");
        let odd: super::super::env_file::Values = [("db_disk".to_string(), "floppy".to_string())].into_iter().collect();
        assert_eq!(StoredConfig::from_values(&odd).disk, DiskKind::Auto, "a hand-edited value it cannot read does not stop the start");
        for d in [DiskKind::Auto, DiskKind::Ssd, DiskKind::Nvme, DiskKind::Hdd] {
            assert_eq!(DiskKind::parse(d.to_text()), Ok(d));
        }
        assert!(DiskKind::parse("tape").is_err());
    }

    #[test]
    fn buffer_pool_scales_with_ram_and_never_exceeds_most_of_it() {
        let ram = 256u64 << 30;
        assert_eq!(BufferPool::Percent(25).bytes_for(ram), 64 << 30, "no 8 GB ceiling any more");
        assert_eq!(BufferPool::Fixed(32 << 30).bytes_for(ram), 32 << 30);
        assert_eq!(BufferPool::Fixed(300 << 30).bytes_for(ram), (ram / 10 * 9) >> 20 << 20);
        assert_eq!(BufferPool::Percent(5).bytes_for(1 << 30), BufferPool::MIN_BYTES, "floor on a small machine");
    }

    #[test]
    fn buffer_pool_and_warm_up_are_stored() {
        let values: super::super::env_file::Values = [("db_buffer_pool".to_string(), "48G".to_string()), ("db_warm_cache".to_string(), "false".to_string())].into_iter().collect();
        let c = StoredConfig::from_values(&values);
        assert_eq!(c.buffer_pool, BufferPool::Fixed(48 << 30));
        assert!(!c.warm_cache());
        let c = StoredConfig::from_values(&Default::default());
        assert_eq!(c.buffer_pool, BufferPool::Percent(25));
        assert!(c.warm_cache(), "on by default");
    }

    #[test]
    fn url_encodes_special_characters_in_credentials() {
        let config = DatabaseConfig { user: "ro@ot".to_string(), password: "p:ss/word".to_string(), host: "db.local".to_string(), port: 3307, database: "romexplorer".to_string(), max_connections: 1 };
        assert_eq!(config.to_url(), "mysql://ro%40ot:p%3Ass%2Fword@db.local:3307/romexplorer");
    }

    #[test]
    fn default_targets_localhost_off_3306() {
        let config = DatabaseConfig::default();
        assert_eq!(config.host, "127.0.0.1");
        assert_ne!(config.port, 3306);
    }

    #[test]
    fn app_paths_layout() {
        let paths = AppPaths::from_root("/x/app_root_ex");
        assert_eq!(paths.mariadb_dir(), PathBuf::from("/x/app_root_ex/lib").join(AppPaths::os_dir_name()).join(AppPaths::arch_dir_name()).join("mariadb"));
        assert!(matches!(AppPaths::os_dir_name(), "linux" | "windows" | "macos" | "ios" | "android"));
        assert!(!AppPaths::arch_dir_name().is_empty());
        assert_eq!(paths.dats_dir(), PathBuf::from("/x/app_root_ex/assets/download/dats"));
        assert_eq!(paths.api_env(), PathBuf::from("/x/app_root_ex/assets/api.env"));
        assert_eq!(paths.mariadb_data_dir(), PathBuf::from("/x/app_root_ex/data/mariadb"));
        assert_eq!(paths.provider_cache_dir(), PathBuf::from("/x/app_root_ex/assets/cache/provider_responses"));
    }

    #[test]
    fn a_package_is_its_own_app_root_and_a_checkout_uses_app_root_ex() {
        let tmp = env::temp_dir().join(format!("rx-rootforexe-{}", std::process::id()));
        // Shipped: the executable at the top level, beside lib/.
        let pkg = tmp.join("pkg");
        std::fs::create_dir_all(pkg.join("lib")).unwrap();
        assert_eq!(AppPaths::root_for_exe(&pkg.join("romexplorer_server")), Some(pkg.clone()));
        // The source tree: the executable under target/, app_root_ex/ standing in for the root.
        let tree = tmp.join("tree");
        std::fs::create_dir_all(tree.join("app_root_ex").join("lib")).unwrap();
        std::fs::create_dir_all(tree.join("target").join("release")).unwrap();
        assert_eq!(AppPaths::root_for_exe(&tree.join("target").join("release").join("romexplorer_server")), Some(tree.join("app_root_ex")));
        // Neither: the caller falls back to the source root.
        let bare = tmp.join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        assert_ne!(AppPaths::root_for_exe(&bare.join("romexplorer_server")), Some(bare.clone()));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn credential_builders_override_defaults() {
        let config = DatabaseConfig::default().with_host("db.local").with_user("app").with_password("secret").with_database("romexplorer");
        assert_eq!(config.host, "db.local");
        assert_eq!(config.user, "app");
        assert_eq!(config.password, "secret");
    }

    // Env-var tests: unique var names not touched by any other test in this crate, so
    // they're safe under cargo test's parallel-within-binary execution.
    #[test]
    fn stored_config_reads_what_setup_writes() {
        let values = crate::database::env_file::parse(
            "db_host=\"10.0.0.5\"\ndb_port=\"3311\"\ndb_user=\"ex_admin\"\ndb_password=\"s3cret\"\ndb_database=\"romexplorer\"\n\
             db_root_password=\"r00t\"\ndb_datadir=\"/srv/rx/mariadb\"\nserver_port=\"7337\"\nshare_enabled=\"true\"\nshare_advertise=\"true\"\n",
        );
        let c = StoredConfig::from_values(&values);
        assert_eq!(c.database.host, "10.0.0.5");
        assert_eq!(c.database.port, 3311);
        assert_eq!(c.database.user, "ex_admin");
        assert_eq!(c.root_password.as_deref(), Some("r00t"));
        assert_eq!(c.datadir, Some(PathBuf::from("/srv/rx/mariadb")));
        assert_eq!(c.server_port, Some(7337));
        assert!(c.share_enabled && c.share_advertise);
    }

    #[test]
    fn an_absent_file_leaves_every_default_in_place() {
        let c = StoredConfig::from_values(&Default::default());
        let d = DatabaseConfig::default();
        assert_eq!((c.database.host, c.database.port), (d.host, d.port));
        assert!(c.root_password.is_none() && c.datadir.is_none() && c.server_port.is_none());
        assert!(!c.share_enabled && !c.share_advertise, "sharing must be off unless asked for");
    }

    #[test]
    fn advertising_requires_sharing() {
        // Advertising a port nothing answers on would just point peers at nothing, so the
        // combination is refused at the point it is read rather than represented.
        let values = crate::database::env_file::parse("share_enabled=\"false\"\nshare_advertise=\"true\"\n");
        let c = StoredConfig::from_values(&values);
        assert!(!c.share_enabled);
        assert!(!c.share_advertise, "advertise must not survive without share");
    }

    #[test]
    fn data_and_cache_dirs_honor_env_overrides() {
        let paths = AppPaths::from_root("/x/app_root_ex");
        env::set_var("ROMEXPLORER_DATA_DIR", "/override/data");
        env::set_var("ROMEXPLORER_CACHE_DIR", "/override/cache");
        assert_eq!(paths.mariadb_data_dir(), PathBuf::from("/override/data/mariadb"));
        assert_eq!(paths.provider_cache_dir(), PathBuf::from("/override/cache/provider_responses"));
        env::remove_var("ROMEXPLORER_DATA_DIR");
        env::remove_var("ROMEXPLORER_CACHE_DIR");
        assert_eq!(paths.mariadb_data_dir(), PathBuf::from("/x/app_root_ex/data/mariadb"));
    }

    #[test]
    fn the_writable_half_of_assets_honors_its_override() {
        // Everything here is written while the app runs, so a read-only app root (an
        // AppImage's squashfs, a root-owned /opt install, a container layer) has to be able
        // to send all four somewhere else.
        let paths = AppPaths::from_root("/x/app_root_ex");
        env::set_var("ROMEXPLORER_ASSETS_DIR", "/override/assets");
        assert_eq!(paths.dats_dir(), PathBuf::from("/override/assets/download/dats"));
        assert_eq!(paths.media_dir(), PathBuf::from("/override/assets/media"));
        assert_eq!(paths.icons_dir(), PathBuf::from("/override/assets/icons"));
        assert_eq!(paths.api_env(), PathBuf::from("/override/assets/api.env"));
        // The cache sits under assets/ too, so it follows unless it has its own override.
        assert_eq!(paths.provider_cache_dir(), PathBuf::from("/override/assets/cache/provider_responses"));
        env::set_var("ROMEXPLORER_CACHE_DIR", "/override/cache");
        assert_eq!(paths.provider_cache_dir(), PathBuf::from("/override/cache/provider_responses"), "the more specific override still wins");
        env::remove_var("ROMEXPLORER_CACHE_DIR");
        env::remove_var("ROMEXPLORER_ASSETS_DIR");
        assert_eq!(paths.media_dir(), PathBuf::from("/x/app_root_ex/assets/media"));
        // Shipped, read-only, and therefore deliberately not moved.
        assert_eq!(paths.gui_web_dir(), PathBuf::from("/x/app_root_ex/assets/gui_web"));
    }
}
