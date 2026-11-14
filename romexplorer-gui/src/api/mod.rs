//! The one shape the GUI's screens know: a route string in, `serde_json::Value` out. They
//! read the same fields the web console does.
//!
//! *Where* that goes is [`backend`]'s business. [`rest`] speaks HTTP to a
//! romexplorer_server -- the web build to the server that served it (same origin), mobile
//! to a server the user points it at, desktop to another machine's. The desktop build
//! installs `local` instead and answers from this process against this machine's own
//! database, which is what `docs/plan/api.md`'s client model asks for: REST is how you
//! reach a database that isn't in your process, and your own never is.

#[cfg(feature = "local")]
pub mod assets;
pub mod backend;
#[cfg(feature = "mobile")]
pub mod companion;
#[cfg(feature = "local")]
pub mod local;
#[cfg(feature = "mobile")]
pub mod offline;
pub mod rest;

// Part of this module's surface whichever build this is, but only the desktop one installs
// a backend of its own, so the web and mobile builds legitimately never name these.
#[allow(unused_imports)]
pub use backend::{set_backend, Backend, BoxFut, Method};

use backend::backend;
use dioxus::prelude::*;
use romexplorer_types::auth::{LoginRequest, LoginResponse, Me};
use serde_json::Value;
use std::sync::OnceLock;

/// Where the server is and how this device signs in to it.
#[derive(Debug, Clone, Default)]
pub struct Endpoint {
    /// `https://192.168.1.10:7337` (mobile), `""` (web: same origin).
    pub base: String,
    /// This device's token from signing in, or the setup token. Empty on the web once signed
    /// in: there the browser holds the session in a cookie page scripts can't read.
    pub token: String,
    /// SHA-256 of the server's TLS certificate, trusted on first connect (native only: the
    /// browser does its own TLS).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub fingerprint: String,
}

/// Set when the server says this device isn't signed in (a 401 on any screen): the app shows
/// the sign-in screen with this message instead of the page.
pub static NEEDS_SIGN_IN: GlobalSignal<Option<String>> = Signal::global(|| None);

/// Who this app is running as. `None` until loaded, or against a server without accounts.
pub static ME: GlobalSignal<Option<Me>> = Signal::global(|| None);

/// Whether the current session may administer the server. Unknown counts as yes, so nothing
/// is hidden from an admin while the answer loads; the server enforces it either way.
pub fn is_admin() -> bool {
    ME.read().as_ref().is_none_or(|m| m.admin)
}

/// A LAN `http://` address becomes `https://`: a romexplorer server only answers plain HTTP
/// from itself and redirects everyone else. Loopback stays as it is.
pub fn normalize_base(base: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    match base.strip_prefix("http://") {
        Some(rest) => {
            let host = rest.split('/').next().unwrap_or("");
            let host = host.rsplit_once(':').filter(|(h, p)| !h.is_empty() && p.chars().all(|c| c.is_ascii_digit())).map(|(h, _)| h).unwrap_or(host);
            if matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1") {
                base.to_string()
            } else {
                format!("https://{rest}")
            }
        }
        None => base.to_string(),
    }
}

/// What this device calls itself in the server's device list.
fn device_name() -> String {
    if cfg!(target_arch = "wasm32") {
        "web browser".to_string()
    } else if cfg!(feature = "mobile") {
        format!("romcompanion ({})", std::env::consts::OS)
    } else {
        format!("romexplorer app ({})", std::env::consts::OS)
    }
}

/// Signs in to the current endpoint's server and keeps what it hands back: a token on native,
/// a session cookie on the web (where the token never reaches the page).
pub async fn sign_in(username: &str, password: &str) -> ApiResult<Me> {
    let web = cfg!(target_arch = "wasm32");
    let body = LoginRequest { username: username.trim().to_string(), password: password.to_string(), device: device_name(), cookie: web, console: false };
    let mut ep = endpoint();
    // Whatever was held before is not what signs this request in.
    ep.token.clear();
    set_endpoint(ep.clone());
    let reply: LoginResponse = post_as("/api/v1/auth/login", &body).await?;
    ep.token = reply.token;
    save_endpoint(&ep);
    set_endpoint(ep);
    crate::cache::clear();
    *ME.write() = Some(reply.me.clone());
    *NEEDS_SIGN_IN.write() = None;
    Ok(reply.me)
}

/// Ends this device's session and forgets its token.
pub async fn sign_out() {
    let _ = post("/api/v1/auth/logout", Value::Null).await;
    let mut ep = endpoint();
    ep.token.clear();
    save_endpoint(&ep);
    set_endpoint(ep);
    crate::cache::clear();
    *ME.write() = None;
    *NEEDS_SIGN_IN.write() = Some(crate::state::tr("Signed out."));
}

static ENDPOINT: OnceLock<std::sync::RwLock<Endpoint>> = OnceLock::new();

fn endpoint_lock() -> &'static std::sync::RwLock<Endpoint> {
    ENDPOINT.get_or_init(|| std::sync::RwLock::new(Endpoint::default()))
}

pub fn set_endpoint(e: Endpoint) {
    *endpoint_lock().write().unwrap_or_else(|p| p.into_inner()) = e;
}

pub fn endpoint() -> Endpoint {
    endpoint_lock().read().unwrap_or_else(|p| p.into_inner()).clone()
}

/// Connects to `base` (empty on the web: the server that served the page) with `token` and
/// the certificate `fingerprint` to trust, and remembers them.
pub fn connect(base: &str, token: &str, fingerprint: &str) {
    let e = Endpoint { base: normalize_base(base), token: token.trim().to_string(), fingerprint: fingerprint.to_string() };
    save_endpoint(&e);
    set_endpoint(e);
}

/// An absolute URL for `path` (which starts with `/`).
pub fn url(path: &str) -> String {
    let base = endpoint().base;
    #[cfg(target_arch = "wasm32")]
    let base = if base.is_empty() { web_sys::window().and_then(|w| w.location().origin().ok()).unwrap_or_default() } else { base };
    format!("{base}{path}")
}

/// A URL an `<img>` can load for a server path.
///
/// Desktop serves it from this process through the webview's asset handler
/// ([`assets`]) -- a path on the page's own origin, so `loading="lazy"` behaves exactly as
/// it does over HTTP, and no bearer token ends up in the DOM. Web and mobile go over HTTP
/// with the token in the query, because an `<img>` cannot send an `Authorization` header.
pub fn media_url(path: &str) -> String {
    // The phone serves its synced covers itself, and fetches the rest from the server.
    #[cfg(any(feature = "local", feature = "mobile"))]
    {
        format!("/rxasset{path}")
    }
    #[cfg(not(any(feature = "local", feature = "mobile")))]
    {
        // A web session is a cookie the browser sends with the image request by itself.
        let token = endpoint().token;
        if token.is_empty() {
            return url(path);
        }
        let sep = if path.contains('?') { '&' } else { '?' };
        format!("{}{sep}token={}", url(path), encode(&token))
    }
}

pub fn media(id: u64) -> String {
    media_url(&format!("/api/v1/media/{id}"))
}

pub fn thumb(id: u64) -> String {
    with_thumb(media(id))
}

/// `?size=thumb`, or `&size=thumb` after a token. Only the token case has a query already:
/// on desktop and in a cookie session the URL ends at the id, and `&size=thumb` glued onto
/// that became part of the id -- "media id must be a number", and every grid cover blank.
fn with_thumb(url: String) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}size=thumb")
}

/// Percent-encoding for a query value.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `?a=1&b=x` from pairs, leaving out empty values.
pub fn qs(pairs: &[(&str, String)]) -> String {
    let parts: Vec<String> = pairs.iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| format!("{k}={}", encode(v))).collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.join("&"))
    }
}

/// A failed call: the server's message (its JSON `error.message`), or the transport error.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

pub type ApiResult<T = Value> = Result<T, ApiError>;

/// The body as the screens have always seen it: JSON, or the raw text when it is not JSON.
pub(crate) fn parse_value(text: String) -> Value {
    if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    }
}

/// Straight into `T`, no `Value` tree in between. The types are the server's own
/// (`romexplorer_types`), so a shape mismatch is a bug on one side or the other, reported
/// as such rather than silently read as missing fields.
fn typed<T: serde::de::DeserializeOwned>(response: ApiResult<String>) -> ApiResult<T> {
    let text = response?;
    serde_json::from_str(&text).map_err(|e| ApiError { status: 200, message: format!("unexpected response shape: {e}") })
}

/// A GET. On the web and mobile a failure to reach the server falls back to the offline
/// cache; see [`rest`], which is where that lives now.
pub async fn get(path: &str) -> ApiResult {
    backend().call(Method::Get, path, None).await.map(parse_value)
}

/// A GET deserialised into one of the shared types.
pub async fn get_as<T: serde::de::DeserializeOwned>(path: &str) -> ApiResult<T> {
    typed(backend().call(Method::Get, path, None).await)
}

// --- where the server is, kept between runs ---

/// The endpoint the user connected to last (web: this browser's storage; native: a small
/// JSON file in the user's config dir). Desktop finds its local server instead (`boot`).
#[cfg_attr(feature = "local", allow(dead_code))]
pub fn saved_endpoint() -> Option<Endpoint> {
    #[cfg(target_arch = "wasm32")]
    {
        let storage = web_sys::window()?.local_storage().ok()??;
        let token = storage.get_item("romexplorer_token").ok()?.unwrap_or_default();
        let base = storage.get_item("romexplorer_server").ok().flatten().unwrap_or_default();
        Some(Endpoint { base, token, fingerprint: String::new() })
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(config_file()?).ok()?).ok()?;
        Some(Endpoint { base: s(&v, "server"), token: s(&v, "token"), fingerprint: s(&v, "fingerprint") })
    }
}

pub fn save_endpoint(e: &Endpoint) {
    // Only the setup token ever lands in web storage: a signed-in browser holds a cookie
    // instead, and an empty value here removes any token an older build left behind.
    #[cfg(target_arch = "wasm32")]
    if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        if e.token.is_empty() {
            let _ = storage.remove_item("romexplorer_token");
        } else {
            let _ = storage.set_item("romexplorer_token", &e.token);
        }
        let _ = storage.set_item("romexplorer_server", &e.base);
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(f) = config_file() {
        if let Some(dir) = f.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let text = serde_json::json!({ "server": e.base, "token": e.token, "fingerprint": e.fingerprint }).to_string();
        let _ = write_private(&f, text.as_bytes());
    }
}

/// Owner-only (0o600 / an owner-only DACL): the file holds this device's token. Also
/// tightens a file an older build wrote with default permissions.
#[cfg(not(target_arch = "wasm32"))]
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    romexplorer_types::private_file::write(path, bytes)
}

#[cfg(not(target_arch = "wasm32"))]
fn config_file() -> Option<std::path::PathBuf> {
    // A phone has no HOME or APPDATA to speak of: its own files directory.
    #[cfg(feature = "mobile")]
    {
        Some(companion::data_dir().join("gui.json"))
    }
    #[cfg(not(feature = "mobile"))]
    {
        use std::path::PathBuf;
        let dir = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .or_else(|| std::env::var_os("TMPDIR").map(PathBuf::from))?;
        Some(dir.join("romexplorer").join("gui.json"))
    }
}

/// Set by a queue button just before it presses the job button beside it (`ui::QueueButton`):
/// the next job that is enqueued takes it, and is asked to wait its turn.
static QUEUE_NEXT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Arms (or, when the press came to nothing -- a cancelled confirm, a refused form --
/// disarms) the "wait its turn" request for the next job enqueued.
pub fn queue_next(on: bool) {
    QUEUE_NEXT.store(on, std::sync::atomic::Ordering::SeqCst);
}

/// Whether the next job was to wait its turn, disarming it: for a press that enqueues
/// several jobs, which re-arms it before each ([`queue_next`]).
pub fn take_queue_next() -> bool {
    QUEUE_NEXT.swap(false, std::sync::atomic::Ordering::SeqCst)
}

pub async fn post(path: &str, body: Value) -> ApiResult {
    // A job started from a queue button starts when nothing else is running or waiting,
    // instead of alongside the other lanes (`POST /api/v1/jobs?in_turn=true`).
    let in_turn = path == "/api/v1/jobs" && QUEUE_NEXT.swap(false, std::sync::atomic::Ordering::SeqCst);
    let path = if in_turn { "/api/v1/jobs?in_turn=true" } else { path };
    backend().call(Method::Post, path, Some(&body)).await.map(parse_value)
}

/// A POST whose body is one of the shared types, answered with another.
pub async fn post_as<B: serde::Serialize, T: serde::de::DeserializeOwned>(path: &str, body: &B) -> ApiResult<T> {
    let body = serde_json::to_value(body).map_err(|e| ApiError { status: 400, message: format!("bad request body: {e}") })?;
    typed(backend().call(Method::Post, path, Some(&body)).await)
}

pub async fn put(path: &str, body: Value) -> ApiResult {
    backend().call(Method::Put, path, Some(&body)).await.map(parse_value)
}

pub async fn put_as<B: serde::Serialize, T: serde::de::DeserializeOwned>(path: &str, body: &B) -> ApiResult<T> {
    let body = serde_json::to_value(body).map_err(|e| ApiError { status: 400, message: format!("bad request body: {e}") })?;
    typed(backend().call(Method::Put, path, Some(&body)).await)
}

pub async fn patch(path: &str, body: Value) -> ApiResult {
    backend().call(Method::Patch, path, Some(&body)).await.map(parse_value)
}

pub async fn delete(path: &str) -> ApiResult {
    backend().call(Method::Delete, path, None).await.map(parse_value)
}

/// A multipart POST (media upload): text fields plus one file.
pub async fn upload(path: &str, fields: Vec<(String, String)>, file_field: &str, file_name: String, bytes: Vec<u8>) -> ApiResult {
    backend().upload(path, fields, file_field, file_name, bytes).await
}

/// Adds a backup zip to the backup directory (`POST /api/v1/backup/upload`). In-process it is
/// copied by path; over the wire, its bytes are uploaded.
pub async fn add_backup(file: dioxus::html::FileData) -> ApiResult {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(fut) = backend().add_backup_from_path(file.path(), file.name()) {
        return fut.await;
    }
    let bytes = file.read_bytes().await.map_err(|e| ApiError { status: 400, message: e.to_string() })?.to_vec();
    backend().upload("/api/v1/backup/upload", Vec::new(), "file", file.name(), bytes).await
}

/// Stores a DAT file (for `source`) or a DAT bundle in the DAT folder
/// (`POST /api/v1/dats/upload`), by path in-process and by upload over the wire.
pub async fn add_dat(file: dioxus::html::FileData, source: Option<String>) -> ApiResult {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(fut) = backend().add_dat_from_path(file.path(), file.name(), source.clone()) {
        return fut.await;
    }
    let bytes = file.read_bytes().await.map_err(|e| ApiError { status: 400, message: e.to_string() })?.to_vec();
    let fields = source.map(|s| vec![("source".to_string(), s)]).unwrap_or_default();
    backend().upload("/api/v1/dats/upload", fields, "file", file.name(), bytes).await
}

// --- JSON accessors: the screens read fields the way the console's JS does ---

pub fn s(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

pub fn so(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).filter(|x| !x.is_empty()).map(str::to_string)
}

pub fn u(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(|x| x.as_u64().or_else(|| x.as_i64().map(|n| n.max(0) as u64)).or_else(|| x.as_str().and_then(|s| s.parse().ok()))).unwrap_or(0)
}

pub fn uo(v: &Value, k: &str) -> Option<u64> {
    v.get(k).and_then(|x| x.as_u64().or_else(|| x.as_str().and_then(|s| s.parse().ok())))
}

pub fn b(v: &Value, k: &str) -> bool {
    match v.get(k) {
        Some(Value::Bool(x)) => *x,
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        _ => false,
    }
}

pub fn arr<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v.get(k).and_then(|x| x.as_array()).map(Vec::as_slice).unwrap_or(&[])
}

/// A page's rows (`items`, else `rows` / `jobs`, else the value itself when it's a list).
pub fn items(v: &Value) -> Vec<Value> {
    for k in ["items", "rows", "jobs"] {
        if let Some(a) = v.get(k).and_then(|x| x.as_array()) {
            return a.clone();
        }
    }
    v.as_array().cloned().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::with_thumb;

    #[test]
    fn a_thumbnail_url_keeps_the_id_out_of_the_query() {
        assert_eq!(with_thumb("/rxasset/api/v1/media/5".into()), "/rxasset/api/v1/media/5?size=thumb");
        assert_eq!(with_thumb("http://h/api/v1/media/5?token=abc".into()), "http://h/api/v1/media/5?token=abc&size=thumb");
    }
}
