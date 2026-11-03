//! Accounts and signing in, for network access to a main server or a share.
//!
//! The desktop app's own in-process backend never signs in; these types only matter to a
//! client talking to another machine, and to the admin managing who may.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Admin,
    #[default]
    User,
}

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::User => "user",
        }
    }
}

/// An account as the admin screen shows it. Never carries a password or its hash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct User {
    pub id: u32,
    pub username: String,
    pub role: Role,
    /// May register new files it copied onto the server's share (never change existing ones).
    pub can_upload: bool,
    pub enabled: bool,
    /// False for an account restored from a backup (backups never carry hashes) until a new
    /// password is set.
    pub has_password: bool,
    /// Signed-in devices that have not been revoked.
    pub devices: u32,
    pub created_at: Option<chrono::NaiveDateTime>,
    pub last_login_at: Option<chrono::NaiveDateTime>,
}

/// Creating or editing an account. `password: None` on an edit keeps the current one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UserSpec {
    pub username: String,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub role: Role,
    #[serde(default)]
    pub can_upload: bool,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
    /// What the device calls itself, shown in the device list ("Alex's phone").
    #[serde(default)]
    pub device: String,
    /// A browser asks for an `HttpOnly` session cookie instead of the token, so page scripts
    /// never hold it. The response's `token` is then empty.
    #[serde(default)]
    pub cookie: bool,
    /// This sign-in is for the server's own console (the page at `/`, or the desktop app's
    /// Server console tab), which is for admin accounts: the server refuses it to anyone
    /// else. A client signing in to sync leaves it false.
    #[serde(default)]
    pub console: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoginResponse {
    pub token: String,
    pub me: Me,
}

/// Who a request is running as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    /// The desktop app's own in-process backend: this machine's admin, never signed in.
    Local,
    /// The setup token in `data/server_token`, accepted until an admin account has a password.
    Setup,
    /// A signed-in account.
    User,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Me {
    pub kind: SessionKind,
    /// Set for `SessionKind::User`.
    pub user: Option<User>,
    pub admin: bool,
    pub can_upload: bool,
    /// Whether any admin account can sign in yet -- until then the setup token is how to get in.
    pub accounts_ready: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PasswordChange {
    pub current: String,
    pub new: String,
}

/// One signed-in device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub id: u64,
    pub device: String,
    pub created_at: Option<chrono::NaiveDateTime>,
    pub last_used_at: Option<chrono::NaiveDateTime>,
    /// The device this request came from.
    pub current: bool,
}

/// The shortest password or passphrase accepted.
pub const MIN_PASSWORD_LEN: usize = 8;
