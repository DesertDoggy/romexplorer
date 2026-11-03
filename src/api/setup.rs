//! First-run setup from a config file (never a network route -- docs/plan/api.md), and the
//! REST server's shared token. Nothing here logs, prints or echoes a password or the token.

use std::fmt;
use std::path::Path;

use crate::database::server::MariaDbServer;
use crate::database::{self, AppPaths, DatabaseConfig, InitialSetup};

use super::{Error, Result};

/// `KEY=VALUE` lines, same syntax as `api.env`. Keys: `database` (default `romexplorer`),
/// `root_password`, `admin_user`, `admin_password` (required), `app_user`/`app_password`
/// (optional, both or neither), `db_port`, `server_port` (optional).
#[derive(Clone)]
pub struct SetupFile {
    pub database: String,
    pub root_password: String,
    pub admin_user: String,
    pub admin_password: String,
    pub app_user: Option<(String, String)>,
    pub db_port: Option<u16>,
    pub server_port: Option<u16>,
}

impl fmt::Debug for SetupFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SetupFile").field("database", &self.database).field("admin_user", &self.admin_user).field("app_user", &self.app_user.as_ref().map(|(u, _)| u)).field("db_port", &self.db_port).field("server_port", &self.server_port).finish_non_exhaustive()
    }
}

impl SetupFile {
    pub fn parse(text: &str) -> Result<Self> {
        let values = crate::database::env_file::parse(text);
        let required = |key: &str| values.get(key).filter(|v| !v.is_empty()).cloned().ok_or_else(|| Error::invalid(format!("setup file: {key} is required")));
        let port = |key: &str| -> Result<Option<u16>> {
            match values.get(key).filter(|v| !v.is_empty()) {
                Some(v) => v.parse::<u16>().map(Some).map_err(|_| Error::invalid(format!("setup file: {key} must be a port number"))),
                None => Ok(None),
            }
        };
        let app_user = match (values.get("app_user").filter(|v| !v.is_empty()), values.get("app_password").filter(|v| !v.is_empty())) {
            (Some(u), Some(p)) => Some((u.clone(), p.clone())),
            (None, None) => None,
            _ => return Err(Error::invalid("setup file: app_user and app_password go together")),
        };
        Ok(Self {
            database: values.get("database").filter(|v| !v.is_empty()).cloned().unwrap_or_else(|| "romexplorer".to_string()),
            root_password: required("root_password")?,
            admin_user: required("admin_user")?,
            admin_password: required("admin_password")?,
            app_user,
            db_port: port("db_port")?,
            server_port: port("server_port")?,
        })
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|err| Error::invalid(format!("cannot read setup file {}: {err}", path.display())))?;
        Self::parse(&text)
    }
}

/// Runs the one-time bring-up (`connection::initial_setup`) against `server`. Returns the
/// config for day-to-day use (the app account if one was created, else the admin).
pub async fn run(server: &mut MariaDbServer, file: &SetupFile) -> Result<DatabaseConfig> {
    let setup = InitialSetup {
        database: &file.database,
        root_password: &file.root_password,
        admin_user: &file.admin_user,
        admin_password: &file.admin_password,
        app_user: file.app_user.as_ref().map(|(u, p)| (u.as_str(), p.as_str())),
    };
    Ok(database::initial_setup(server, setup).await?)
}

/// The REST server's shared bearer token: read from `AppPaths::server_token_path`, or
/// generated (32 random bytes, hex) and written there with owner-only permissions on
/// first use.
/// The setup token for this start, or `None` once an admin account can sign in (and the file
/// is then removed). Only generated while there is no such admin: after that, a fresh file
/// would be a way in that nobody meant to leave open.
pub async fn setup_token(app: &super::App) -> Result<Option<String>> {
    if super::auth::admin_ready(app).await? {
        retire_setup_token(&app.paths);
        return Ok(None);
    }
    ensure_server_token(&app.paths).map(Some)
}

/// Deletes `data/server_token`. Best effort: `guard` stops honouring the token as soon as an
/// admin can sign in, whether or not the file is gone, so a failure here is only untidy.
pub fn retire_setup_token(paths: &AppPaths) {
    let path = paths.server_token_path();
    match std::fs::remove_file(&path) {
        Ok(()) => log::info!("an admin account can now sign in; removed the setup token {}", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => log::warn!("an admin account can now sign in, but the setup token {} could not be removed: {e}", path.display()),
    }
}

pub fn ensure_server_token(paths: &AppPaths) -> Result<String> {
    let path = paths.server_token_path();
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let token = existing.trim().to_string();
        if token.len() >= 32 {
            return Ok(token);
        }
    }
    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut bytes);
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| Error::internal(format!("cannot create {}: {err}", parent.display())))?;
    }
    romexplorer_types::private_file::write(&path, format!("{token}\n").as_bytes()).map_err(|err| Error::internal(format!("cannot write {}: {err}", path.display())))?;
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_setup_file_and_requires_the_essentials() {
        let f = SetupFile::parse("database=rx\nroot_password = 'r'\nadmin_user=adm\nadmin_password=\"a\"\napp_user=app\napp_password=p\ndb_port=3311\nserver_port=7337\n").unwrap();
        assert_eq!(f.database, "rx");
        assert_eq!(f.app_user.as_ref().map(|(u, _)| u.as_str()), Some("app"));
        assert_eq!(f.db_port, Some(3311));
        assert_eq!(f.server_port, Some(7337));
        assert!(SetupFile::parse("admin_user=adm\nadmin_password=a\n").is_err());
        assert!(SetupFile::parse("root_password=r\nadmin_user=adm\nadmin_password=a\napp_user=only\n").is_err());
        let d = SetupFile::parse("root_password=r\nadmin_user=adm\nadmin_password=a\n").unwrap();
        assert_eq!(d.database, "romexplorer");
        assert!(d.app_user.is_none());
    }

    #[test]
    fn debug_never_shows_passwords() {
        let f = SetupFile::parse("root_password=hunter2\nadmin_user=adm\nadmin_password=hunter3\n").unwrap();
        let shown = format!("{f:?}");
        assert!(!shown.contains("hunter2") && !shown.contains("hunter3"));
    }

    #[test]
    fn token_is_generated_once_then_reused() {
        let dir = std::env::temp_dir().join(format!("romexplorer-token-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // No env override here: config.rs's own tests read ROMEXPLORER_DATA_DIR in parallel.
        let paths = AppPaths::from_root(&dir);
        let a = ensure_server_token(&paths).unwrap();
        let b = ensure_server_token(&paths).unwrap();
        assert!(paths.server_token_path().starts_with(&dir));
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
