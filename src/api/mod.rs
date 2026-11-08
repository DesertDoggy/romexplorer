//! The intermediate layer between `database`/`rom` and everything a user touches -- the
//! REST server (`romexplorer_server`), the desktop GUI, the CLI tools and the worker
//! (docs/plan/api.md). One plain async Rust function per user-level operation, serde types
//! in and out, no HTTP or GUI types anywhere in here. `database` modules keep owning their
//! own SQL; this layer composes them, validates input and shapes the result.

pub mod about;
pub mod audio;
pub mod audio_autotag;
pub mod audio_convert;
pub mod audio_ram;
pub mod artwork;
pub mod audio_sync;
pub mod auth;
pub mod backup;
pub mod bases;
pub mod bios;
pub mod batch;
pub mod catalog;
pub mod config_file;
pub mod copy;
pub mod dats;
pub mod dictionary;
pub mod display;
pub mod display_data;
pub mod db;
pub mod definitive;
pub mod dev;
pub mod directories;
pub mod fs;
pub mod i18n;
pub mod integrity;
pub mod jobs;
pub mod launch;
pub mod links;
pub mod media;
pub mod media_backfill;
pub mod media_sync;
pub mod naming;
pub mod parallel;
pub mod patches;
pub mod providers;
pub mod ra;
pub mod release_combine;
pub mod restore;
pub mod rows;
pub mod sqlite;
pub mod scan;
pub mod secrets;
pub mod schedules;
pub mod selection;
pub mod sets;
pub mod settings;
pub mod setup;
pub mod sheets;
pub mod sources;
pub mod status;
pub mod sync;
pub mod review;
pub mod rom_audio;
pub mod ai;
pub mod set_file;
pub mod titles;
pub mod versions;
pub mod storage;

use std::fmt;

use serde::Serialize;
use sqlx::mysql::MySqlPool;

use crate::database::providers::ProviderClients;
use crate::database::{self, ApiCredentials, AppPaths, DatabaseConfig, DatabaseError, Vault};

/// Shared per-process context every `api` function takes. Built once by each binary from
/// the same `AppPaths::detect()` + `DatabaseConfig::from_env()` the existing binaries use.
pub struct App {
    pub pool: MySqlPool,
    pub paths: AppPaths,
    pub config: DatabaseConfig,
    /// The provider clients, rebuilt from `api.env` whenever it changes -- see [`Self::clients`].
    pub providers: std::sync::Arc<ProviderState>,
    pub http: reqwest::Client,
    /// Seals provider keys and peer tokens at rest (`database::vault`).
    pub vault: Vault,
}

impl App {
    /// Connects (creating the database and applying migrations if needed) and builds the
    /// provider clients from `api.env`.
    pub async fn open(paths: AppPaths, config: DatabaseConfig) -> Result<Self> {
        // The root password setup stored, for a migration the configured account may not run.
        let root_password = database::config::StoredConfig::load(&paths).root_password;
        let pool = database::initialize_with_root(&config, root_password.as_deref()).await?;
        let app = Self::with_pool(pool, paths, config)?;
        let sealed = sources::seal_plaintext_tokens(&app).await?;
        if sealed > 0 {
            log::info!("sealed {sealed} stored peer token(s) with this machine's master key");
        }
        // Who reads CHD headers in this process, until a scan or a settings change re-reads it.
        if let Err(e) = database::settings::apply_chd_reader(&app.pool).await {
            log::warn!("chd_reader setting not read: {e}");
        }
        if let Err(e) = database::settings::apply_scan_pkg_titles(&app.pool).await {
            log::warn!("scan_pkg_titles setting not read: {e}");
        }
        Ok(app)
    }

    pub fn with_pool(pool: MySqlPool, paths: AppPaths, config: DatabaseConfig) -> Result<Self> {
        let vault = Vault::load_or_create(&paths.master_key_path())?;
        let http = database::providers::build_http_client()?;
        let providers = std::sync::Arc::new(ProviderState::load(paths.api_env(), vault.clone(), http.clone())?);
        Ok(Self { pool, paths, config, providers, http, vault })
    }

    /// The provider clients as `api.env` stands now. Cheap: one `stat` per call, and a
    /// rebuild only when the file changed since the last one.
    pub fn clients(&self) -> std::sync::Arc<ProviderClients> {
        self.providers.current()
    }
}

/// `api.env`'s provider clients, kept current with the file.
///
/// Rebuilt when the file's size or modification time changes, rather than told to by whoever
/// wrote it: the server, its in-process worker, a separate `romexplorer_worker` and the
/// desktop each hold their own `App`, and a file that changed is the one signal all of them
/// see without being wired together. Saving credentials in the UI therefore takes effect
/// everywhere on the next request or job, with no restart.
pub struct ProviderState {
    path: std::path::PathBuf,
    vault: Vault,
    http: reqwest::Client,
    current: std::sync::RwLock<(Option<(std::time::SystemTime, u64)>, std::sync::Arc<ProviderClients>)>,
}

impl ProviderState {
    fn load(path: std::path::PathBuf, vault: Vault, http: reqwest::Client) -> std::result::Result<Self, DatabaseError> {
        let stamp = Self::stamp(&path);
        let clients = Self::build(&path, &vault, &http)?;
        Ok(Self { path, vault, http, current: std::sync::RwLock::new((stamp, std::sync::Arc::new(clients))) })
    }

    fn stamp(path: &std::path::Path) -> Option<(std::time::SystemTime, u64)> {
        let meta = std::fs::metadata(path).ok()?;
        Some((meta.modified().ok()?, meta.len()))
    }

    fn build(path: &std::path::Path, vault: &Vault, http: &reqwest::Client) -> std::result::Result<ProviderClients, DatabaseError> {
        let mut creds = ApiCredentials::load(path)?;
        for key in creds.decrypt(vault) {
            log::warn!("api.env: {key} is sealed but won't open with this machine's master key; that provider is unavailable until it is re-entered");
        }
        Ok(ProviderClients::from_credentials(&creds, http.clone()))
    }

    fn current(&self) -> std::sync::Arc<ProviderClients> {
        let stamp = Self::stamp(&self.path);
        {
            let held = self.current.read().unwrap_or_else(|e| e.into_inner());
            if held.0 == stamp {
                return held.1.clone();
            }
        }
        let mut held = self.current.write().unwrap_or_else(|e| e.into_inner());
        if held.0 != stamp {
            // The stamp moves on even when the rebuild fails, so an unreadable file is
            // reported once rather than on every call; the previous clients stay in use.
            held.0 = stamp;
            match Self::build(&self.path, &self.vault, &self.http) {
                Ok(clients) => {
                    held.1 = std::sync::Arc::new(clients);
                    log::info!("api.env changed; provider clients rebuilt");
                }
                Err(e) => log::warn!("api.env changed but could not be read ({e}); keeping the previous provider clients"),
            }
        }
        held.1.clone()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    NotFound,
    Invalid,
    Conflict,
    Unavailable,
    Internal,
    /// Not signed in, or the credentials were wrong.
    Unauthorized,
    /// Signed in, but this account may not do that.
    Forbidden,
}

#[derive(Debug, Clone, Serialize)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
}

impl Error {
    pub fn not_found(message: impl Into<String>) -> Self {
        Self { kind: ErrorKind::NotFound, message: message.into() }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self { kind: ErrorKind::Invalid, message: message.into() }
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self { kind: ErrorKind::Conflict, message: message.into() }
    }
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self { kind: ErrorKind::Unavailable, message: message.into() }
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self { kind: ErrorKind::Internal, message: message.into() }
    }
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self { kind: ErrorKind::Unauthorized, message: message.into() }
    }
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self { kind: ErrorKind::Forbidden, message: message.into() }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for Error {}

impl From<DatabaseError> for Error {
    fn from(err: DatabaseError) -> Self {
        match err {
            DatabaseError::Sql(sqlx::Error::RowNotFound) => Self::not_found("not found"),
            DatabaseError::Config(m) => Self::invalid(m),
            DatabaseError::Provider(m) | DatabaseError::Server(m) => Self::unavailable(m),
            other => Self::internal(other.to_string()),
        }
    }
}

impl From<sqlx::Error> for Error {
    fn from(err: sqlx::Error) -> Self {
        Self::from(DatabaseError::from(err))
    }
}

/// For callers that live in `DatabaseError` terms (the worker): the message survives, the
/// kind collapses to the closest existing variant.
impl From<Error> for DatabaseError {
    fn from(err: Error) -> Self {
        match err.kind {
            ErrorKind::Unavailable => DatabaseError::Provider(err.message),
            ErrorKind::Internal => DatabaseError::Server(err.message),
            _ => DatabaseError::Config(err.message),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Paging defaults shared by every list operation.
pub(crate) fn clamp_page(limit: Option<u32>, offset: Option<u32>) -> (u32, u32) {
    (limit.unwrap_or(50).clamp(1, 500), offset.unwrap_or(0))
}

/// `%value%` for a search box: `%` and `\` escaped, and each run of separators -- space,
/// hyphen, colon, full stop, underscore, slash, tilde -- matching any one character, so
/// "Hoge Quest" finds "Hoge-Quest" and the other way round (DATs write some names hyphenated
/// that people type with a space).
pub(crate) fn like_pattern(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('%', "\\%");
    let mut out = String::with_capacity(escaped.len() + 2);
    out.push('%');
    let mut in_run = false;
    for c in escaped.chars() {
        if matches!(c, ' ' | '-' | ':' | '.' | '_' | '/' | '~') {
            if !in_run {
                out.push('_');
            }
            in_run = true;
        } else {
            out.push(c);
            in_run = false;
        }
    }
    out.push('%');
    out
}

#[cfg(test)]
mod like_tests {
    #[test]
    fn a_search_pattern_takes_any_separator_between_words() {
        assert_eq!(super::like_pattern("Hoge Quest 2"), "%Hoge_Quest_2%");
        assert_eq!(super::like_pattern("Hoge-Quest  - 2"), "%Hoge_Quest_2%");
        assert_eq!(super::like_pattern("100% Hoge\\"), "%100\\%_Hoge\\\\%");
    }
}

/// The LIKE metacharacters escaped, with no wildcards added -- for a caller building its
/// own pattern (`LIKE 'prefix%'`).
pub(crate) fn like_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}
