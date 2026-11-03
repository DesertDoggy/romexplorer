//! Errors shared across the `database` module.

use std::fmt;

#[derive(Debug)]
pub enum DatabaseError {
    /// A `sqlx` connection/query/migration failure.
    Sql(sqlx::Error),
    /// A source dat file (or, for retrobios, its JSON database) didn't parse as expected --
    /// see `dat_parse`'s doc comment for the shape each source is expected to have.
    Parse(String),
    /// A `dat_download` / provider fetch failed (network, unexpected page shape, ...).
    Fetch(String),
    /// Filesystem trouble (download dir, raw-response cache, media files, server datadir).
    Io(std::io::Error),
    /// The embedded MariaDB server couldn't be bootstrapped/started/stopped.
    Server(String),
    /// A provider client couldn't be built or its call failed (credentials missing, API error).
    Provider(String),
    /// Bad/missing configuration (app root not found, unreadable api.env, ...).
    Config(String),
    /// A job that ran to its end with some of its items failed (a scan with unreadable
    /// files, a DAT sync with one DAT that would not parse). The work that could be done is
    /// done and recorded, and doing all of it again would fail the same items the same way,
    /// so the worker marks the job failed without another try (`jobs::fail_final`).
    Partial(String),
}

impl fmt::Display for DatabaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sql(err) => write!(f, "database error: {err}"),
            Self::Parse(message) => write!(f, "dat parse error: {message}"),
            Self::Fetch(message) => write!(f, "fetch error: {message}"),
            Self::Io(err) => write!(f, "io error: {err}"),
            Self::Server(message) => write!(f, "mariadb server error: {message}"),
            Self::Provider(message) => write!(f, "provider error: {message}"),
            Self::Config(message) => write!(f, "config error: {message}"),
            Self::Partial(message) => write!(f, "{message}"),
        }
    }
}

impl DatabaseError {
    /// MariaDB gave up on the statement because another connection held the rows: a
    /// deadlock (1213, the transaction was rolled back) or a lock wait timeout (1205). Both
    /// happen when two jobs write the same rows at once -- two DAT syncs importing the
    /// same platform -- and both succeed when the transaction is simply run again.
    pub fn is_lock_conflict(&self) -> bool {
        let Self::Sql(err) = self else { return false };
        err.as_database_error().and_then(|d| d.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>()).is_some_and(|m| matches!(m.number(), 1205 | 1213))
    }
}

impl std::error::Error for DatabaseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sql(err) => Some(err),
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<sqlx::Error> for DatabaseError {
    fn from(err: sqlx::Error) -> Self {
        Self::Sql(err)
    }
}

impl From<sqlx::migrate::MigrateError> for DatabaseError {
    fn from(err: sqlx::migrate::MigrateError) -> Self {
        Self::Sql(sqlx::Error::Migrate(Box::new(err)))
    }
}

impl From<std::io::Error> for DatabaseError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<reqwest::Error> for DatabaseError {
    fn from(err: reqwest::Error) -> Self {
        Self::Fetch(err.to_string())
    }
}

impl From<anyhow::Error> for DatabaseError {
    fn from(err: anyhow::Error) -> Self {
        Self::Provider(format!("{err:#}"))
    }
}

impl From<serde_json::Error> for DatabaseError {
    fn from(err: serde_json::Error) -> Self {
        Self::Parse(err.to_string())
    }
}

impl From<zip::result::ZipError> for DatabaseError {
    fn from(err: zip::result::ZipError) -> Self {
        Self::Parse(format!("zip: {err}"))
    }
}
