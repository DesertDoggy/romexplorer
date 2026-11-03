//! Accounts, signing in, and who a request is running as.
//!
//! Only network access signs in. The desktop app's in-process backend is [`Principal::Local`]
//! and never presents anything; a standalone or client install needs no account at all.
//!
//! Passwords are kept only as argon2id hashes. A sign-in mints a random token for that one
//! device and keeps only its SHA-256, so neither table signs anyone in if it is copied, and a
//! device is revoked without touching the others.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use sqlx::Row;

pub use romexplorer_types::auth::{Device, LoginRequest, Me, PasswordChange, Role, SessionKind, User, UserSpec, MIN_PASSWORD_LEN};

use super::{App, Error, Result};

/// Who a request is running as. Inserted by the server's `guard`.
#[derive(Debug, Clone)]
pub enum Principal {
    /// The desktop app's own in-process backend: this machine's admin.
    Local,
    /// The setup token, accepted until an admin account can sign in.
    Setup,
    /// A signed-in account, on one device.
    User { user: User, token_id: u64 },
}

impl Principal {
    pub fn is_admin(&self) -> bool {
        match self {
            Self::Local | Self::Setup => true,
            Self::User { user, .. } => user.role == Role::Admin,
        }
    }

    pub fn can_upload(&self) -> bool {
        match self {
            Self::Local | Self::Setup => true,
            Self::User { user, .. } => user.role == Role::Admin || user.can_upload,
        }
    }

    pub fn kind(&self) -> SessionKind {
        match self {
            Self::Local => SessionKind::Local,
            Self::Setup => SessionKind::Setup,
            Self::User { .. } => SessionKind::User,
        }
    }
}

// ---------------------------------------------------------------------------------------
// hashing
// ---------------------------------------------------------------------------------------

fn hash_blocking(password: &str) -> Result<String> {
    use argon2::password_hash::{PasswordHasher, SaltString};
    let mut salt = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut salt);
    let salt = SaltString::encode_b64(&salt).map_err(|e| Error::internal(format!("salt: {e}")))?;
    argon2::Argon2::default().hash_password(password.as_bytes(), &salt).map(|h| h.to_string()).map_err(|e| Error::internal(format!("hashing: {e}")))
}

fn verify_blocking(password: &str, phc: &str) -> bool {
    use argon2::password_hash::{PasswordHash, PasswordVerifier};
    PasswordHash::new(phc).is_ok_and(|parsed| argon2::Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
}

/// argon2id with the crate's defaults, off the async threads: each hash is tens of
/// milliseconds of deliberate work.
pub async fn hash_password(password: &str) -> Result<String> {
    let password = password.to_string();
    tokio::task::spawn_blocking(move || hash_blocking(&password)).await.map_err(|e| Error::internal(e.to_string()))?
}

pub async fn verify_password(password: &str, phc: &str) -> bool {
    let (password, phc) = (password.to_string(), phc.to_string());
    tokio::task::spawn_blocking(move || verify_blocking(&password, &phc)).await.unwrap_or(false)
}

/// A real hash of nothing anyone knows, verified against when the username does not exist, so
/// a wrong name takes as long to refuse as a wrong password and the timing gives nothing away.
static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| hash_blocking("romexplorer-no-such-account").unwrap_or_default());

pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut bytes);
    crate::database::vault::to_hex(&bytes)
}

/// What is stored for a token: its SHA-256, hex.
pub fn digest(token: &str) -> String {
    use sha2::{Digest, Sha256};
    crate::database::vault::to_hex(&Sha256::digest(token.as_bytes()))
}

pub fn validate_password(password: &str) -> Result<()> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(Error::invalid(format!("a password needs at least {MIN_PASSWORD_LEN} characters")));
    }
    Ok(())
}

pub fn validate_username(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(Error::invalid("a username is 1 to 64 characters"));
    }
    if !name.chars().all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '@')) {
        return Err(Error::invalid("a username is letters, digits and . _ - @ only"));
    }
    Ok(name.to_string())
}

// ---------------------------------------------------------------------------------------
// the sign-in limiter
// ---------------------------------------------------------------------------------------

const MAX_FAILURES: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(30);

/// Failures per username and per source address. In memory on purpose: it only has to slow
/// guessing down, and argon2 already makes each guess expensive.
static FAILURES: LazyLock<Mutex<HashMap<String, (u32, Instant)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn limiter_keys(username: &str, ip: Option<IpAddr>) -> Vec<String> {
    let mut keys = vec![format!("user:{}", username.trim().to_lowercase())];
    if let Some(ip) = ip {
        keys.push(format!("ip:{ip}"));
    }
    keys
}

fn locked_out(keys: &[String]) -> bool {
    let mut map = FAILURES.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    map.retain(|_, (_, at)| now.duration_since(*at) < LOCKOUT);
    keys.iter().any(|k| map.get(k).is_some_and(|(n, _)| *n >= MAX_FAILURES))
}

fn record_failure(keys: &[String]) {
    let mut map = FAILURES.lock().unwrap_or_else(|e| e.into_inner());
    for key in keys {
        let entry = map.entry(key.clone()).or_insert((0, Instant::now()));
        entry.0 += 1;
        entry.1 = Instant::now();
    }
}

fn clear_failures(keys: &[String]) {
    let mut map = FAILURES.lock().unwrap_or_else(|e| e.into_inner());
    for key in keys {
        map.remove(key);
    }
}

// ---------------------------------------------------------------------------------------
// reading accounts
// ---------------------------------------------------------------------------------------

const USER_COLUMNS: &str = "u.id, u.username, u.role, u.can_upload, u.enabled, u.password_hash IS NOT NULL AS has_password, u.created_at, u.last_login_at, \
     (SELECT COUNT(*) FROM user_tokens t WHERE t.user_id = u.id AND t.revoked = FALSE) AS devices";

fn row_to_user(row: &sqlx::mysql::MySqlRow) -> Result<User> {
    let role: String = row.try_get("role")?;
    Ok(User {
        id: row.try_get("id")?,
        username: row.try_get("username")?,
        role: if role == "admin" { Role::Admin } else { Role::User },
        can_upload: row.try_get("can_upload")?,
        enabled: row.try_get("enabled")?,
        has_password: row.try_get::<i64, _>("has_password")? != 0,
        devices: row.try_get::<i64, _>("devices")? as u32,
        created_at: row.try_get("created_at")?,
        last_login_at: row.try_get("last_login_at")?,
    })
}

pub async fn list_users(app: &App) -> Result<Vec<User>> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT {USER_COLUMNS} FROM users u WHERE u.deleted = FALSE ORDER BY u.role, u.username_key"))).fetch_all(&app.pool).await?;
    rows.iter().map(row_to_user).collect()
}

pub async fn get_user(app: &App, id: u32) -> Result<User> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT {USER_COLUMNS} FROM users u WHERE u.id = ? AND u.deleted = FALSE")))
        .bind(id)
        .fetch_optional(&app.pool)
        .await?
        .ok_or_else(|| Error::not_found(format!("no user {id}")))?;
    row_to_user(&row)
}

/// Whether an admin account can sign in. Until one can, the setup token is how to get in.
pub async fn admin_ready(app: &App) -> Result<bool> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE role = 'admin' AND enabled = TRUE AND deleted = FALSE AND password_hash IS NOT NULL").fetch_one(&app.pool).await?;
    Ok(n > 0)
}

/// The account behind a device token, if it is live.
pub async fn resolve(app: &App, token: &str) -> Result<Option<Principal>> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT t.id AS token_id, {USER_COLUMNS} FROM user_tokens t JOIN users u ON u.id = t.user_id \
         WHERE t.token_sha256 = ? AND t.revoked = FALSE AND u.enabled = TRUE AND u.deleted = FALSE AND u.password_hash IS NOT NULL"
    )))
    .bind(digest(token))
    .fetch_optional(&app.pool)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let token_id: u64 = row.try_get("token_id")?;
    // At most once a minute, so a busy screen isn't a write per request.
    sqlx::query("UPDATE user_tokens SET last_used_at = NOW() WHERE id = ? AND (last_used_at IS NULL OR last_used_at < NOW() - INTERVAL 1 MINUTE)").bind(token_id).execute(&app.pool).await?;
    Ok(Some(Principal::User { user: row_to_user(&row)?, token_id }))
}

pub async fn me(app: &App, principal: &Principal) -> Result<Me> {
    let user = match principal {
        Principal::User { user, .. } => Some(get_user(app, user.id).await?),
        _ => None,
    };
    Ok(Me { kind: principal.kind(), user, admin: principal.is_admin(), can_upload: principal.can_upload(), accounts_ready: admin_ready(app).await? })
}

// ---------------------------------------------------------------------------------------
// signing in and out
// ---------------------------------------------------------------------------------------

/// Checks the password and mints a token for this device.
///
/// One answer for "no such user" and "wrong password", taking the same time for both.
pub async fn login(app: &App, request: &LoginRequest, ip: Option<IpAddr>) -> Result<(String, User)> {
    let keys = limiter_keys(&request.username, ip);
    if locked_out(&keys) {
        return Err(Error::unauthorized(format!("too many failed sign-ins; wait {} seconds and try again", LOCKOUT.as_secs())));
    }
    let row = sqlx::query("SELECT id, password_hash FROM users WHERE username_key = ? AND enabled = TRUE AND deleted = FALSE")
        .bind(request.username.trim().to_lowercase())
        .fetch_optional(&app.pool)
        .await?;
    let (id, hash): (Option<u32>, Option<String>) = match &row {
        Some(r) => (Some(r.try_get("id")?), r.try_get("password_hash")?),
        None => (None, None),
    };
    let ok = verify_password(&request.password, hash.as_deref().unwrap_or(DUMMY_HASH.as_str())).await && hash.is_some();
    let (true, Some(id)) = (ok, id) else {
        record_failure(&keys);
        return Err(Error::unauthorized("wrong username or password"));
    };
    clear_failures(&keys);

    // The console is the server's own control panel. Refused here, after the password has
    // been checked (so this says nothing about which accounts exist) and before a token is
    // made (so a refused sign-in leaves no session behind). What a regular account may do
    // over the API is unchanged -- a client still signs in with it to sync.
    if request.console && get_user(app, id).await?.role != Role::Admin {
        return Err(Error::forbidden("the server console is for admin accounts"));
    }

    let token = new_token();
    let device: String = request.device.trim().chars().take(128).collect();
    sqlx::query("INSERT INTO user_tokens (user_id, token_sha256, device) VALUES (?, ?, ?)").bind(id).bind(digest(&token)).bind(device).execute(&app.pool).await?;
    sqlx::query("UPDATE users SET last_login_at = NOW() WHERE id = ?").bind(id).execute(&app.pool).await?;
    Ok((token, get_user(app, id).await?))
}

/// Ends this device's session.
pub async fn logout(app: &App, principal: &Principal) -> Result<()> {
    if let Principal::User { token_id, .. } = principal {
        sqlx::query("UPDATE user_tokens SET revoked = TRUE WHERE id = ?").bind(token_id).execute(&app.pool).await?;
    }
    Ok(())
}

async fn revoke_all(app: &App, user_id: u32, except: Option<u64>) -> Result<u64> {
    Ok(sqlx::query("UPDATE user_tokens SET revoked = TRUE WHERE user_id = ? AND revoked = FALSE AND id <> ?")
        .bind(user_id)
        .bind(except.unwrap_or(0))
        .execute(&app.pool)
        .await?
        .rows_affected())
}

/// Changes one's own password, signing out every other device.
pub async fn change_password(app: &App, principal: &Principal, change: &PasswordChange) -> Result<()> {
    let Principal::User { user, token_id } = principal else {
        return Err(Error::invalid("only a signed-in account has a password to change"));
    };
    validate_password(&change.new)?;
    let hash: Option<String> = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ?").bind(user.id).fetch_one(&app.pool).await?;
    let current_ok = match hash.as_deref() {
        Some(h) => verify_password(&change.current, h).await,
        None => false,
    };
    if !current_ok {
        return Err(Error::unauthorized("the current password is wrong"));
    }
    let new_hash = hash_password(&change.new).await?;
    sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?").bind(new_hash).bind(user.id).execute(&app.pool).await?;
    revoke_all(app, user.id, Some(*token_id)).await?;
    Ok(())
}

pub async fn devices(app: &App, user_id: u32, current: Option<u64>) -> Result<Vec<Device>> {
    let rows = sqlx::query("SELECT id, device, created_at, last_used_at FROM user_tokens WHERE user_id = ? AND revoked = FALSE ORDER BY COALESCE(last_used_at, created_at) DESC")
        .bind(user_id)
        .fetch_all(&app.pool)
        .await?;
    rows.iter()
        .map(|r| {
            let id: u64 = r.try_get("id")?;
            Ok(Device { id, device: r.try_get("device")?, created_at: r.try_get("created_at")?, last_used_at: r.try_get("last_used_at")?, current: Some(id) == current })
        })
        .collect()
}

pub async fn revoke_device(app: &App, user_id: u32, device_id: u64) -> Result<()> {
    let n = sqlx::query("UPDATE user_tokens SET revoked = TRUE WHERE id = ? AND user_id = ? AND revoked = FALSE").bind(device_id).bind(user_id).execute(&app.pool).await?.rows_affected();
    if n == 0 {
        return Err(Error::not_found(format!("no signed-in device {device_id}")));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// managing accounts (admin)
// ---------------------------------------------------------------------------------------

/// Admins who could still sign in if `excluding` were changed or removed.
async fn other_usable_admins(app: &App, excluding: u32) -> Result<i64> {
    Ok(sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE role = 'admin' AND enabled = TRUE AND deleted = FALSE AND password_hash IS NOT NULL AND id <> ?")
        .bind(excluding)
        .fetch_one(&app.pool)
        .await?)
}

/// Creates an account (or brings back a deleted one with the same name).
pub async fn create_user(app: &App, spec: &UserSpec) -> Result<User> {
    let name = validate_username(&spec.username)?;
    let password = spec.password.as_deref().ok_or_else(|| Error::invalid("a new account needs a password"))?;
    validate_password(password)?;
    let hash = hash_password(password).await?;
    let key = name.to_lowercase();
    let existing: Option<(u32, bool)> = sqlx::query("SELECT id, deleted FROM users WHERE username_key = ?")
        .bind(&key)
        .fetch_optional(&app.pool)
        .await?
        .map(|r| -> Result<(u32, bool)> { Ok((r.try_get("id")?, r.try_get("deleted")?)) })
        .transpose()?;
    let id = match existing {
        Some((_, false)) => return Err(Error::conflict(format!("there is already an account called {name}"))),
        Some((id, true)) => {
            revoke_all(app, id, None).await?;
            sqlx::query("UPDATE users SET username = ?, password_hash = ?, role = ?, can_upload = ?, enabled = ?, deleted = FALSE, last_login_at = NULL WHERE id = ?")
                .bind(&name)
                .bind(&hash)
                .bind(spec.role.name())
                .bind(spec.can_upload)
                .bind(spec.enabled)
                .bind(id)
                .execute(&app.pool)
                .await?;
            id
        }
        None => sqlx::query("INSERT INTO users (username, username_key, password_hash, role, can_upload, enabled) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(&name)
            .bind(&key)
            .bind(&hash)
            .bind(spec.role.name())
            .bind(spec.can_upload)
            .bind(spec.enabled)
            .execute(&app.pool)
            .await?
            .last_insert_id() as u32,
    };
    after_account_change(app).await?;
    get_user(app, id).await
}

/// Edits an account. A new password, a demotion or disabling signs its devices out.
///
/// Refuses to leave the server with no admin able to sign in: that would lock everyone out
/// until someone ran `reset-admin` on the server itself.
pub async fn update_user(app: &App, id: u32, spec: &UserSpec) -> Result<User> {
    let current = get_user(app, id).await?;
    let name = validate_username(&spec.username)?;
    let loses_admin = current.role == Role::Admin && (spec.role != Role::Admin || !spec.enabled);
    if loses_admin && other_usable_admins(app, id).await? == 0 {
        return Err(Error::conflict("that would leave no admin able to sign in -- make another account an admin first"));
    }
    let clash: Option<u32> = sqlx::query_scalar("SELECT id FROM users WHERE username_key = ? AND id <> ? AND deleted = FALSE").bind(name.to_lowercase()).bind(id).fetch_optional(&app.pool).await?;
    if clash.is_some() {
        return Err(Error::conflict(format!("there is already an account called {name}")));
    }
    let new_hash = match spec.password.as_deref().filter(|p| !p.is_empty()) {
        Some(p) => {
            validate_password(p)?;
            Some(hash_password(p).await?)
        }
        None => None,
    };
    sqlx::query("UPDATE users SET username = ?, username_key = ?, role = ?, can_upload = ?, enabled = ?, password_hash = COALESCE(?, password_hash) WHERE id = ?")
        .bind(&name)
        .bind(name.to_lowercase())
        .bind(spec.role.name())
        .bind(spec.can_upload)
        .bind(spec.enabled)
        .bind(&new_hash)
        .bind(id)
        .execute(&app.pool)
        .await?;
    if new_hash.is_some() || !spec.enabled || (current.role == Role::Admin && spec.role != Role::Admin) {
        revoke_all(app, id, None).await?;
    }
    after_account_change(app).await?;
    get_user(app, id).await
}

pub async fn delete_user(app: &App, id: u32) -> Result<()> {
    let current = get_user(app, id).await?;
    if current.role == Role::Admin && other_usable_admins(app, id).await? == 0 {
        return Err(Error::conflict("that is the last admin able to sign in -- make another account an admin first"));
    }
    revoke_all(app, id, None).await?;
    sqlx::query("UPDATE users SET deleted = TRUE, enabled = FALSE WHERE id = ?").bind(id).execute(&app.pool).await?;
    Ok(())
}

/// Once an admin can sign in, the setup token has done its job: delete the file, so it can't
/// be found and used later. (`guard` also stops accepting it, file or no file.)
async fn after_account_change(app: &App) -> Result<()> {
    if admin_ready(app).await? {
        super::setup::retire_setup_token(&app.paths);
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// the seed file and the admin reset
// ---------------------------------------------------------------------------------------

/// First accounts, as an installer writes them into `data/auth.env`:
///
/// ```text
/// admin_username = alice
/// admin_password = ...
/// user.bob       = ...      # a regular account
/// upload.bob     = true     # ...that may register files it uploads
/// ```
#[derive(Debug, Default, PartialEq)]
pub struct SeedFile {
    pub admin: (String, String),
    /// `(name, password, can_upload)`.
    pub users: Vec<(String, String, bool)>,
}

impl SeedFile {
    /// Checks everything before anything is created, so a typo in the fourth account doesn't
    /// leave three created and a file that will never be applied again.
    pub fn parse(text: &str) -> Result<Self> {
        let values = crate::database::env_file::parse(text);
        let get = |k: &str| values.get(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let (Some(name), Some(password)) = (get("admin_username"), get("admin_password")) else {
            return Err(Error::invalid("auth.env needs admin_username and admin_password"));
        };
        let name = validate_username(&name)?;
        validate_password(&password)?;
        let mut users = Vec::new();
        let mut keys: Vec<&String> = values.keys().collect();
        keys.sort();
        for key in keys {
            if let Some(user) = key.strip_prefix("user.") {
                let user = validate_username(user)?;
                let password = values[key].trim().to_string();
                validate_password(&password).map_err(|e| Error::invalid(format!("{user}: {}", e.message)))?;
                let upload = values.get(&format!("upload.{user}")).is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "yes" | "1"));
                users.push((user, password, upload));
            } else if !matches!(key.as_str(), "admin_username" | "admin_password") && !key.starts_with("upload.") {
                return Err(Error::invalid(format!("auth.env: unknown key {key:?}")));
            }
        }
        if users.iter().any(|(u, _, _)| u.eq_ignore_ascii_case(&name)) {
            return Err(Error::invalid(format!("auth.env: {name} is both the admin and a regular account")));
        }
        Ok(Self { admin: (name, password), users })
    }
}

/// What happened to `data/auth.env` on this start.
#[derive(Debug, PartialEq, Eq)]
pub enum SeedOutcome {
    NoFile,
    /// Created the admin and this many regular accounts.
    Applied { users: usize },
    /// An admin could already sign in, so the file was not applied -- dropping a file must
    /// never be a way to reset the admin.
    Ignored,
}

/// Applies `data/auth.env` once, then deletes it.
///
/// Applied only while no admin can sign in. Deleted either way, and only after the accounts
/// are committed; if the delete fails it is said loudly, and it still can't be applied again
/// because by then an admin exists.
pub async fn seed_from_file(app: &App) -> Result<SeedOutcome> {
    let path = app.paths.auth_env_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(SeedOutcome::NoFile),
        Err(e) => return Err(Error::internal(format!("cannot read {}: {e}", path.display()))),
    };
    let outcome = if admin_ready(app).await? {
        log::warn!("{} ignored: an admin account can already sign in (use `romexplorer_server reset-admin` to reset one)", path.display());
        SeedOutcome::Ignored
    } else {
        let seed = SeedFile::parse(&text)?;
        reset_admin(app, &seed.admin.0, &seed.admin.1).await?;
        let mut created = 0;
        for (name, password, can_upload) in &seed.users {
            let spec = UserSpec { username: name.clone(), password: Some(password.clone()), role: Role::User, can_upload: *can_upload, enabled: true };
            match create_user(app, &spec).await {
                Ok(_) => created += 1,
                Err(e) if e.kind == super::ErrorKind::Conflict => log::warn!("auth.env: {name} already exists; left as it is"),
                Err(e) => return Err(e),
            }
        }
        SeedOutcome::Applied { users: created }
    };
    if let Err(e) = std::fs::remove_file(&path) {
        log::warn!("could not delete {} after reading it: {e} -- delete it by hand, it holds passwords", path.display());
    }
    Ok(outcome)
}

/// Makes `name` an admin who can sign in with `password`: creating the account, or promoting
/// and re-enabling an existing one. Every device it was signed in on is signed out. Used by the
/// seed file and by `romexplorer_server reset-admin`.
pub async fn reset_admin(app: &App, name: &str, password: &str) -> Result<User> {
    let name = validate_username(name)?;
    validate_password(password)?;
    let existing: Option<u32> = sqlx::query_scalar("SELECT id FROM users WHERE username_key = ? AND deleted = FALSE").bind(name.to_lowercase()).fetch_optional(&app.pool).await?;
    let user = match existing {
        Some(id) => {
            let hash = hash_password(password).await?;
            sqlx::query("UPDATE users SET password_hash = ?, role = 'admin', enabled = TRUE WHERE id = ?").bind(hash).bind(id).execute(&app.pool).await?;
            revoke_all(app, id, None).await?;
            get_user(app, id).await?
        }
        None => create_user(app, &UserSpec { username: name, password: Some(password.to_string()), role: Role::Admin, can_upload: true, enabled: true }).await?,
    };
    after_account_change(app).await?;
    Ok(user)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seed_file_is_read_whole_or_not_at_all() {
        let seed = SeedFile::parse("admin_username = Alice\nadmin_password = adminpass1\nuser.bob = bobpass12\nupload.bob = true\nuser.carl = carlpass1\n").unwrap();
        assert_eq!(seed.admin, ("Alice".to_string(), "adminpass1".to_string()));
        assert_eq!(seed.users, vec![("bob".into(), "bobpass12".into(), true), ("carl".into(), "carlpass1".into(), false)]);

        assert!(SeedFile::parse("admin_password = adminpass1\n").is_err(), "no admin name");
        assert!(SeedFile::parse("admin_username=a\nadmin_password=short\n").is_err(), "short admin password");
        assert!(SeedFile::parse("admin_username=a\nadmin_password=adminpass1\nuser.bob=short\n").is_err(), "one bad account refuses the file");
        assert!(SeedFile::parse("admin_username=a\nadmin_password=adminpass1\nusr.bob=bobpass12\n").is_err(), "a typo is not ignored");
        assert!(SeedFile::parse("admin_username=a\nadmin_password=adminpass1\nuser.A=bobpass12\n").is_err(), "the admin twice");
    }

    #[test]
    fn a_hash_verifies_its_own_password_only() {
        let h = hash_blocking("correct horse").unwrap();
        assert!(h.starts_with("$argon2id$"), "{h}");
        assert!(verify_blocking("correct horse", &h));
        assert!(!verify_blocking("correct horsE", &h));
        assert!(!verify_blocking("correct horse", "not a hash"));
        assert_ne!(hash_blocking("correct horse").unwrap(), h, "a fresh salt each time");
    }

    #[test]
    fn a_token_is_random_and_only_its_digest_is_kept() {
        let (a, b) = (new_token(), new_token());
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert_eq!(digest(&a).len(), 64);
        assert_eq!(digest(&a), digest(&a));
        assert_ne!(digest(&a), a);
        // Known SHA-256 vector.
        assert_eq!(digest("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn names_and_passwords_are_checked() {
        assert_eq!(validate_username("  alice ").unwrap(), "alice");
        assert!(validate_username("").is_err());
        assert!(validate_username("has space").is_err());
        assert!(validate_username("bob.o'neil").is_err());
        assert!(validate_username("user@home").is_ok());
        assert!(validate_password("short").is_err());
        assert!(validate_password("long enough").is_ok());
    }

    #[test]
    fn repeated_failures_lock_a_name_out_and_success_clears_it() {
        let keys = limiter_keys("limiter-test-user", None);
        clear_failures(&keys);
        for _ in 0..MAX_FAILURES - 1 {
            record_failure(&keys);
        }
        assert!(!locked_out(&keys));
        record_failure(&keys);
        assert!(locked_out(&keys));
        clear_failures(&keys);
        assert!(!locked_out(&keys));
    }

    #[test]
    fn what_each_principal_may_do() {
        assert!(Principal::Local.is_admin() && Principal::Setup.is_admin());
        let user = |role, can_upload| Principal::User {
            user: User { id: 1, username: "u".into(), role, can_upload, enabled: true, has_password: true, devices: 0, created_at: None, last_login_at: None },
            token_id: 1,
        };
        assert!(!user(Role::User, false).is_admin());
        assert!(!user(Role::User, false).can_upload());
        assert!(user(Role::User, true).can_upload());
        assert!(user(Role::Admin, false).can_upload());
    }
}
