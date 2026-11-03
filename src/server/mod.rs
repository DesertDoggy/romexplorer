//! The REST server behind `romexplorer_server` (docs/plan/api.md): axum routes 1:1 over
//! `api`, JSON in/out with `api`'s own serde types, and the built-in dev console page at `/`.
//! No logic lives in handlers -- parse, call one `api` fn, map the error kind to a status.
//!
//! Every request passes `guard`: a private-range source-IP check, then who is calling
//! ([`Principal`]: the in-process desktop backend, the setup token, or a signed-in account's
//! device token), then whether that caller may use this route ([`access`]).

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::{ConnectInfo, DefaultBodyLimit, Multipart, Path, Query, Request, State};
use axum::http::{header, Extensions, HeaderMap, StatusCode, Version};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::auth::Principal;
use crate::api::{self, App, Error, ErrorKind};

pub mod access;
pub mod tls;

pub const CONSOLE_HTML: &str = include_str!("console.html");
pub const DEFAULT_PORT: u16 = 7337;

pub struct ServerState {
    pub app: App,
    /// The setup token (`api::setup::setup_token`), honoured only while no admin account can
    /// sign in. `None` once one can. Never logged.
    pub setup_token: Option<String>,
    /// Set only by the desktop app's in-process backend, whose requests never touch a socket:
    /// every one of them is this machine's own admin, with nothing to present.
    pub in_process: bool,
    /// Skip the private-range source-IP check (tests behind unusual networking only).
    pub allow_public_ips: bool,
    /// Let a client's `media/fetch` reach a provider on 127.0.0.1. Tests only, which stand a
    /// provider in there; never tied to who may connect.
    pub fetch_loopback: bool,
    /// Deliberate shutdown/restart requests from the console (`POST /api/v1/server/...`).
    pub lifecycle: Lifecycle,
    /// SHA-256 of the TLS certificate the listeners serve, for clients to compare against what
    /// they were shown on first connect. `None` without TLS (and in-process).
    pub tls_fingerprint: Option<String>,
}

/// What a deliberate stop asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleRequest {
    Shutdown,
    Restart,
}

/// The one way (besides Ctrl+C) to stop a running server: the HTTP layer only *records* a
/// request here; the binary that owns the process waits on [`Lifecycle::requested`], then
/// stops the listener, the worker and the embedded database in order (and re-execs for a
/// restart). Nothing else -- no idle timeout, no dropped connection -- ends it.
#[derive(Clone)]
pub struct Lifecycle {
    tx: tokio::sync::watch::Sender<Option<LifecycleRequest>>,
    /// Whether this process runs the background worker (shown in the console).
    pub worker_running: bool,
    pub started_at: chrono::NaiveDateTime,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self::new(false)
    }
}

impl Lifecycle {
    pub fn new(worker_running: bool) -> Self {
        Self { tx: tokio::sync::watch::channel(None).0, worker_running, started_at: chrono::Local::now().naive_local() }
    }

    pub fn request(&self, what: LifecycleRequest) {
        self.tx.send_replace(Some(what));
    }

    /// Resolves once a shutdown/restart has been requested.
    pub async fn requested(&self) -> LifecycleRequest {
        let mut rx = self.tx.subscribe();
        loop {
            if let Some(what) = *rx.borrow_and_update() {
                return what;
            }
            if rx.changed().await.is_err() {
                return LifecycleRequest::Shutdown;
            }
        }
    }
}

pub type Shared = Arc<ServerState>;

#[derive(Debug, Clone)]
pub struct ServeOptions {
    pub bind: IpAddr,
    pub port: u16,
    /// Serve TLS (`tls`). `None` is plain HTTP for every client -- only for a server behind a
    /// proxy that terminates TLS itself, or tests.
    pub tls: Option<tls::Tls>,
}

/// RFC 1918 + loopback + link-local for v4; loopback, ULA (`fc00::/7`), link-local
/// (`fe80::/10`) and v4-mapped private addresses for v6.
pub fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            if v6.is_loopback() {
                return true;
            }
            if let Some(v4) = v6.to_ipv4_mapped() {
                return v4.is_private() || v4.is_loopback() || v4.is_link_local();
            }
            let seg = v6.segments();
            (seg[0] & 0xfe00) == 0xfc00 || (seg[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Removes a file when dropped, if it is still there.
struct RemoveOnDrop(std::path::PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ---------------------------------------------------------------------------
// errors
// ---------------------------------------------------------------------------

pub struct ApiError(pub Error);

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        Self(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self.0.kind {
            ErrorKind::NotFound => StatusCode::NOT_FOUND,
            ErrorKind::Invalid => StatusCode::BAD_REQUEST,
            ErrorKind::Conflict => StatusCode::CONFLICT,
            ErrorKind::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            ErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            ErrorKind::Unauthorized => StatusCode::UNAUTHORIZED,
            ErrorKind::Forbidden => StatusCode::FORBIDDEN,
        };
        (status, Json(json!({ "error": self.0 }))).into_response()
    }
}

fn plain_error(status: StatusCode, kind: &str, message: &str) -> Response {
    (status, Json(json!({ "error": { "kind": kind, "message": message } }))).into_response()
}

type ApiResult<T> = std::result::Result<Json<T>, ApiError>;

// ---------------------------------------------------------------------------
// auth
// ---------------------------------------------------------------------------

fn mime_for(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript",
        Some("css") => "text/css",
        Some("wasm") => "application/wasm",
        Some("json") => "application/json",
        Some("woff2") => "font/woff2",
        Some("avif") => "image/avif",
        _ => "application/octet-stream",
    }
}

/// Files in `AppPaths::icons_dir`, by plain file name (no subdirectories).
fn icon_files(paths: &crate::database::AppPaths) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(paths.icons_dir()).map(|rd| rd.flatten().filter(|e| e.path().is_file()).filter_map(|e| e.file_name().to_str().map(str::to_string)).collect()).unwrap_or_default();
    out.sort();
    out
}

/// `/icons/<file>`: decorative, user-provided system icons -- token-exempt (the private-IP
/// guard still applies), served only by plain file name from the icons dir.
async fn icon_file(State(s): State<Shared>, Path(file): Path<String>) -> Response {
    if file.is_empty() || file.contains(['/', '\\']) || file.starts_with('.') {
        return plain_error(StatusCode::NOT_FOUND, "not_found", "no such icon");
    }
    let path = s.app.paths.icons_dir().join(&file);
    match tokio::fs::read(&path).await {
        Ok(bytes) if path.is_file() => ([(header::CONTENT_TYPE, mime_for(&path)), (header::CACHE_CONTROL, "private, max-age=86400")], bytes).into_response(),
        _ => plain_error(StatusCode::NOT_FOUND, "not_found", "no such icon"),
    }
}

/// The GUI's web build (`AppPaths::gui_web_dir`): a file under it, else its `index.html`
/// (the app's own routes, e.g. `/app/collection/titles`).
///
/// Caching, in two tiers, because the bundle is a 4 MB wasm that was being re-sent (and
/// re-compressed) on every page load:
///
/// - Files dx content-hashed (`name-dxh<hex>.ext` -- every file under `assets/` today) change
///   name when their bytes change, so they are `immutable` for a year. Decided by the file
///   name, not the directory, so an unhashed file dropped into `assets/` later is not wrongly
///   frozen.
/// - `index.html` is the one cache-busting root (the hashed names are baked into it), so it
///   stays `no-cache` but carries a strong `ETag`; a matching `If-None-Match` answers `304`
///   with no body. The file is 534 bytes and already read, so hashing it costs nothing.
/// Whether a `/app/...` path (already percent-decoded, so `%2F` is a `/` here) stays under the
/// web build: plain names only. A leading `/` or `\`, a drive (`C:`), `..` or a hidden file
/// would let `root.join` reach anywhere on the disk, and this route needs no sign-in.
fn gui_web_path_is_safe(rel: &str) -> bool {
    let parts: Vec<&str> = rel.split(['/', '\\']).collect();
    !rel.contains(':') && !rel.contains('\0') && parts.iter().all(|c| !c.is_empty() && !c.starts_with('.'))
}

async fn gui_web(State(s): State<Shared>, headers: HeaderMap, path: Option<Path<String>>) -> Response {
    let root = s.app.paths.gui_web_dir();
    let rel = path.map(|Path(p)| p).unwrap_or_default();
    let safe = gui_web_path_is_safe(&rel);
    let file = root.join(&rel);
    let target = if safe && !rel.is_empty() && file.is_file() { file } else { root.join("index.html") };
    let bytes = match tokio::fs::read(&target).await {
        Ok(bytes) => bytes,
        Err(_) => return plain_error(StatusCode::NOT_FOUND, "not_found", "the GUI web build isn't installed (assets/gui_web)"),
    };
    let fingerprinted = target.file_name().and_then(|n| n.to_str()).is_some_and(is_fingerprinted);
    if fingerprinted {
        return ([(header::CONTENT_TYPE, mime_for(&target)), (header::CACHE_CONTROL, "public, max-age=31536000, immutable")], bytes).into_response();
    }
    let etag = format!("\"{:032x}\"", xxhash_rust::xxh3::xxh3_128(&bytes));
    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()).is_some_and(|v| etag_matches(v, &etag)) {
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, etag), (header::CACHE_CONTROL, "no-cache".to_string())]).into_response();
    }
    ([(header::CONTENT_TYPE, mime_for(&target).to_string()), (header::CACHE_CONTROL, "no-cache".to_string()), (header::ETAG, etag)], bytes).into_response()
}

/// A dx content-hashed asset name: `<stem>-dxh<hex>.<ext>`. The hex run has to be there and
/// be hex, so a stem that merely contains "-dxh" does not qualify.
fn is_fingerprinted(name: &str) -> bool {
    let Some(rest) = name.split("-dxh").nth(1) else { return false };
    let hex: &str = rest.split('.').next().unwrap_or_default();
    !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit())
}

/// `If-None-Match` is a comma-separated list of tags, or `*`; weak-comparison prefixes
/// (`W/`) are tolerated since the tag is only ever ours.
fn etag_matches(header_value: &str, etag: &str) -> bool {
    header_value.split(',').map(str::trim).any(|t| t == "*" || t == etag || t.strip_prefix("W/") == Some(etag))
}

/// `?token=...` -- for `<img src>`/media URLs, which can't carry a header.
fn query_token(query: Option<&str>) -> Option<&str> {
    query?.split('&').find_map(|pair| pair.strip_prefix("token=")).map(str::trim)
}

const SESSION_COOKIE: &str = "rx_session";

fn cookie_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| pair.trim().strip_prefix(SESSION_COOKIE)?.strip_prefix('='))
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

/// The token a request presents: the `Authorization` header, else the browser's session
/// cookie, else `?token=` where a URL is the only place it can go.
fn presented_token(req: &Request) -> Option<&str> {
    // An empty `Bearer ` (a page that holds no token because its session is a cookie) is no
    // token at all, not a wrong one.
    let header_token = req.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).map(str::trim).filter(|t| !t.is_empty());
    header_token.or_else(|| cookie_token(req.headers())).or_else(|| {
        if access::query_token_allowed(req.method(), req.uri().path()) {
            query_token(req.uri().query())
        } else {
            None
        }
    })
}

/// Who is calling, or `None` for nobody we know. Takes the token already extracted, because a
/// `&Request` held across the lookup would make `guard` not `Send`.
async fn identify(state: &ServerState, token: Option<String>) -> api::Result<Option<Principal>> {
    if state.in_process {
        return Ok(Some(Principal::Local));
    }
    let Some(token) = token else { return Ok(None) };
    let token = token.as_str();
    if let Some(setup) = &state.setup_token {
        if constant_time_eq(token, setup) {
            // Checked on every use rather than once: the moment an admin can sign in, the
            // setup token stops working, without a restart.
            return Ok(if api::auth::admin_ready(&state.app).await? { None } else { Some(Principal::Setup) });
        }
    }
    api::auth::resolve(&state.app, token).await
}

/// A RetroAchievements read with `refresh` makes the server call RA; a regular account reads
/// what is cached.
fn asks_for_refresh(req: &Request) -> bool {
    req.uri().path().starts_with("/api/v1/ra/") && req.uri().query().is_some_and(|q| q.split('&').any(|p| matches!(p, "refresh=true" | "refresh=1")))
}

async fn guard(State(state): State<Shared>, ConnectInfo(addr): ConnectInfo<SocketAddr>, mut req: Request, next: Next) -> Response {
    if !state.allow_public_ips && !is_private_ip(addr.ip()) {
        return plain_error(StatusCode::FORBIDDEN, "forbidden", "source address is not on a private network");
    }
    let need = access::required(req.method(), req.uri().path());
    if need == access::Access::Open {
        return next.run(req).await;
    }
    let token = presented_token(&req).map(str::to_owned);
    let principal = match identify(&state, token).await {
        Ok(Some(p)) => p,
        Ok(None) => return plain_error(StatusCode::UNAUTHORIZED, "unauthorized", "sign in first (missing, wrong or revoked token)"),
        Err(e) => return ApiError(e).into_response(),
    };
    if !access::allows(&principal, need) || (asks_for_refresh(&req) && !principal.is_admin()) {
        return plain_error(StatusCode::FORBIDDEN, "forbidden", "that needs an admin account");
    }
    req.extensions_mut().insert(principal);
    next.run(req).await
}

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for Principal {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut axum::http::request::Parts, _state: &S) -> std::result::Result<Self, Self::Rejection> {
        parts.extensions.get::<Principal>().cloned().ok_or_else(|| plain_error(StatusCode::UNAUTHORIZED, "unauthorized", "sign in first"))
    }
}

// ---------------------------------------------------------------------------
// accounts
// ---------------------------------------------------------------------------

fn session_cookie(token: &str) -> String {
    format!("{SESSION_COOKIE}={token}; Path=/; HttpOnly; Secure; SameSite=Strict")
}

async fn auth_login(State(s): State<Shared>, ConnectInfo(addr): ConnectInfo<SocketAddr>, Json(req): Json<api::auth::LoginRequest>) -> std::result::Result<Response, ApiError> {
    let (token, user) = api::auth::login(&s.app, &req, Some(addr.ip())).await?;
    let principal = Principal::User { user, token_id: 0 };
    let me = api::auth::me(&s.app, &principal).await?;
    if req.cookie {
        // The page never sees the token: the browser holds it where scripts can't read it.
        let body = romexplorer_types::auth::LoginResponse { token: String::new(), me };
        return Ok(([(header::SET_COOKIE, session_cookie(&token))], Json(body)).into_response());
    }
    Ok(Json(romexplorer_types::auth::LoginResponse { token, me }).into_response())
}

async fn auth_logout(State(s): State<Shared>, principal: Principal) -> std::result::Result<Response, ApiError> {
    api::auth::logout(&s.app, &principal).await?;
    let clear = format!("{SESSION_COOKIE}=; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=0");
    Ok(([(header::SET_COOKIE, clear)], Json(json!({ "ok": true }))).into_response())
}

async fn auth_me(State(s): State<Shared>, principal: Principal) -> ApiResult<api::auth::Me> {
    Ok(Json(api::auth::me(&s.app, &principal).await?))
}

async fn auth_password(State(s): State<Shared>, principal: Principal, Json(change): Json<api::auth::PasswordChange>) -> ApiResult<Value> {
    api::auth::change_password(&s.app, &principal, &change).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn auth_devices(State(s): State<Shared>, principal: Principal) -> ApiResult<Vec<api::auth::Device>> {
    match &principal {
        Principal::User { user, token_id } => Ok(Json(api::auth::devices(&s.app, user.id, Some(*token_id)).await?)),
        _ => Ok(Json(Vec::new())),
    }
}

async fn auth_device_revoke(State(s): State<Shared>, principal: Principal, Path(id): Path<u64>) -> ApiResult<Value> {
    let Principal::User { user, .. } = &principal else { return Err(Error::invalid("only a signed-in account has devices").into()) };
    api::auth::revoke_device(&s.app, user.id, id).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn users_list(State(s): State<Shared>) -> ApiResult<Vec<api::auth::User>> {
    Ok(Json(api::auth::list_users(&s.app).await?))
}

async fn users_create(State(s): State<Shared>, Json(spec): Json<api::auth::UserSpec>) -> std::result::Result<(StatusCode, Json<api::auth::User>), ApiError> {
    Ok((StatusCode::CREATED, Json(api::auth::create_user(&s.app, &spec).await?)))
}

async fn users_update(State(s): State<Shared>, Path(id): Path<u32>, Json(spec): Json<api::auth::UserSpec>) -> ApiResult<api::auth::User> {
    Ok(Json(api::auth::update_user(&s.app, id, &spec).await?))
}

async fn users_delete(State(s): State<Shared>, Path(id): Path<u32>) -> ApiResult<Value> {
    api::auth::delete_user(&s.app, id).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn user_devices(State(s): State<Shared>, Path(id): Path<u32>) -> ApiResult<Vec<api::auth::Device>> {
    Ok(Json(api::auth::devices(&s.app, id, None).await?))
}

async fn user_device_revoke(State(s): State<Shared>, Path((id, device_id)): Path<(u32, u64)>) -> ApiResult<Value> {
    api::auth::revoke_device(&s.app, id, device_id).await?;
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// handlers
// ---------------------------------------------------------------------------

/// `no-cache`: the page is compiled into the binary, so after a server update a browser
/// must not keep running the previous console's JS against the new API.
/// The audio player script (`romexplorer_types::PLAYER_JS`), shared with the desktop app.
async fn player_js() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], romexplorer_types::PLAYER_JS)
}

async fn console() -> impl IntoResponse {
    ([(header::CACHE_CONTROL, "no-cache")], Html(CONSOLE_HTML))
}

async fn ping() -> Json<Value> {
    Json(json!({ "ok": true, "service": "romexplorer_server" }))
}

async fn auth_check() -> Json<Value> {
    Json(json!({ "ok": true }))
}

#[derive(Deserialize)]
struct AboutQuery {
    lang: Option<String>,
}

/// `?lang=` picks the notes' language; without it, the server's own `ui_language` -- so a
/// client built before the parameter existed (a stale web bundle, an older desktop) still
/// gets the notes in the language the UI is set to. The database being unreachable is no
/// reason to fail the About page: it falls back to English.
async fn about(State(s): State<Shared>, Query(q): Query<AboutQuery>) -> Json<Value> {
    let lang = match q.lang {
        Some(l) => Some(l),
        None => tokio::time::timeout(std::time::Duration::from_secs(2), crate::database::settings::get::<String>(&s.app.pool, crate::database::settings::KEY_UI_LANGUAGE))
            .await
            .ok()
            .and_then(Result::ok)
            .flatten(),
    };
    Json(api::about::about(lang.as_deref()))
}

async fn server_info(State(s): State<Shared>) -> Json<Value> {
    // The ports as stored (`data/database.env`): what the next start will use, which is what
    // a settings screen should show.
    let stored = crate::database::config::StoredConfig::load(&s.app.paths);
    Json(json!({ "pid": std::process::id(), "started_at": s.lifecycle.started_at, "worker_running": s.lifecycle.worker_running, "version": env!("CARGO_PKG_VERSION"), "tls_fingerprint": s.tls_fingerprint,
        "db_port": stored.database.port, "server_port": stored.server_port.unwrap_or(DEFAULT_PORT) }))
}

#[derive(serde::Deserialize)]
struct PortsBody {
    db_port: Option<u16>,
    server_port: Option<u16>,
}

/// Saves the database and server ports to `data/database.env`. Neither can change under a
/// running process -- the database is already listening on one and this listener on the
/// other -- so both take effect at the next start, and the answer says so.
async fn server_ports_put(State(s): State<Shared>, Json(b): Json<PortsBody>) -> ApiResult<Value> {
    let mut stored = crate::database::config::StoredConfig::load(&s.app.paths);
    let db_port = b.db_port.unwrap_or(stored.database.port);
    let server_port = b.server_port.unwrap_or(stored.server_port.unwrap_or(DEFAULT_PORT));
    for (name, port) in [("database", db_port), ("server", server_port)] {
        if port < 1024 {
            return Err(api::Error::invalid(format!("the {name} port must be 1024 or above")).into());
        }
    }
    if db_port == server_port {
        return Err(api::Error::invalid("the database and the server need different ports").into());
    }
    stored.database.port = db_port;
    stored.server_port = Some(server_port);
    stored.save(&s.app.paths).map_err(|e| api::Error::internal(format!("saving {}: {e}", s.app.paths.database_env_path().display())))?;
    Ok(Json(json!({ "db_port": db_port, "server_port": server_port, "applies": "next start" })))
}

/// The database cache: what `database.env` asks for (used from the next start), what the
/// running MariaDB has, how much of it holds data, how big the catalogue is, and how the
/// start-up reload and warm-up went.
async fn database_cache(State(s): State<Shared>) -> ApiResult<Value> {
    use crate::database::config::{total_ram, BufferPool, StoredConfig};
    let stored = StoredConfig::load(&s.app.paths);
    let pool = &s.app.pool;
    let scalar = |sql: &'static str| async move { sqlx::query_scalar::<_, u64>(sql).fetch_one(pool).await.ok() };
    let live_pool = scalar("SELECT @@innodb_buffer_pool_size").await;
    // MariaDB rounds the pool up to whole chunks, so only a difference of a chunk or more
    // means the saved size isn't the running one. 10.8+ reports 0 for "sized automatically",
    // which must not make every size count as different.
    let chunk = scalar("SELECT @@innodb_buffer_pool_chunk_size").await.unwrap_or(128 << 20).max(1 << 20);
    let page_size = scalar("SELECT @@innodb_page_size").await;
    let pages_data: Option<u64> = sqlx::query("SHOW GLOBAL STATUS LIKE 'Innodb_buffer_pool_pages_data'")
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .and_then(|r| sqlx::Row::try_get::<String, _>(&r, 1).ok())
        .and_then(|v| v.parse().ok());
    let catalogue = scalar("SELECT CAST(COALESCE(SUM(DATA_LENGTH + INDEX_LENGTH), 0) AS UNSIGNED) FROM information_schema.TABLES WHERE TABLE_SCHEMA = DATABASE()").await;
    let ram = total_ram();
    let local = matches!(s.app.config.host.as_str(), "127.0.0.1" | "localhost" | "::1");
    let next_start = stored.buffer_pool.bytes_for(ram);
    let restart_needed = local && live_pool.is_some_and(|live| live.abs_diff(next_start) >= chunk);
    let (mode, percent, fixed) = match stored.buffer_pool {
        BufferPool::Percent(p) => ("percent", Some(p), None),
        BufferPool::Fixed(b) => ("fixed", None, Some(b)),
    };
    // How it writes: what is stored, what that comes to on this disk, and what the running
    // server has (the same, unless it was started by someone else).
    let tuning = crate::database::server::WriteTuning::new(stored.fast_writes(), stored.disk, &stored.datadir(&s.app.paths));
    let live_flush = scalar("SELECT CAST(@@innodb_flush_log_at_trx_commit AS UNSIGNED)").await;
    let live_io = scalar("SELECT CAST(@@innodb_io_capacity AS UNSIGNED)").await;
    Ok(Json(json!({
        "fast_writes": stored.fast_writes(), "disk": stored.disk.to_text(), "disk_detected": tuning.disk, "write_tuning": tuning,
        "live_flush_at_commit": live_flush, "live_io_capacity": live_io,
        "setting": stored.buffer_pool.to_text(), "mode": mode, "percent": percent, "fixed_bytes": fixed,
        "next_start_bytes": next_start, "total_ram_bytes": ram, "restart_needed": restart_needed,
        // How to restart the database: the desktop app starts its own (so quit and reopen
        // it); a server restarts the database it started along with itself.
        "in_process": s.in_process,
        "min_percent": BufferPool::MIN_PERCENT, "max_percent": BufferPool::MAX_PERCENT, "min_bytes": BufferPool::MIN_BYTES,
        "live_bytes": live_pool, "cached_bytes": pages_data.zip(page_size).map(|(n, p)| n * p), "catalogue_bytes": catalogue,
        "load_status": crate::database::warm::load_status(pool).await,
        "warm_cache": stored.warm_cache(), "warm": crate::database::warm::status(),
        // An adopted or remote database was started by someone else, with their options.
        "local": local,
    })))
}

#[derive(serde::Deserialize)]
struct DatabaseCacheBody {
    /// `25%` or `64G`.
    buffer_pool: Option<String>,
    warm_cache: Option<bool>,
    fast_writes: Option<bool>,
    /// `auto` | `ssd` | `nvme` | `hdd`.
    disk: Option<String>,
}

/// Saves the cache size, the warm-up switch, the write mode and the disk kind to
/// `data/database.env`. The size is a `mariadbd` start-up option, so like the ports it
/// applies from the next start; how it writes can be changed on a running server, and is.
async fn database_cache_put(State(s): State<Shared>, Json(b): Json<DatabaseCacheBody>) -> ApiResult<Value> {
    use crate::database::config::{BufferPool, StoredConfig};
    let mut stored = StoredConfig::load(&s.app.paths);
    if let Some(text) = &b.buffer_pool {
        stored.buffer_pool = BufferPool::parse(text).map_err(api::Error::invalid)?;
    }
    if let Some(w) = b.warm_cache {
        stored.warm_cache = Some(w);
    }
    let writes_changed = b.fast_writes.is_some() || b.disk.is_some();
    if let Some(f) = b.fast_writes {
        stored.fast_writes = Some(f);
    }
    if let Some(text) = &b.disk {
        stored.disk = crate::database::config::DiskKind::parse(text).map_err(api::Error::invalid)?;
    }
    stored.save(&s.app.paths).map_err(|e| api::Error::internal(format!("saving {}: {e}", s.app.paths.database_env_path().display())))?;
    // On a database this installation started, the write settings take effect now. One it
    // only connects to keeps its owner's; one whose account may not SET GLOBAL gets them at
    // the next start, which is said rather than failed.
    let local = matches!(s.app.config.host.as_str(), "127.0.0.1" | "localhost" | "::1");
    let mut writes_applied = false;
    if writes_changed && local {
        let tuning = crate::database::server::WriteTuning::new(stored.fast_writes(), stored.disk, &stored.datadir(&s.app.paths));
        match tuning.apply(&s.app.pool).await {
            Ok(()) => writes_applied = true,
            Err(e) => log::warn!("database write settings saved, but not applied to the running server: {e}"),
        }
    }
    Ok(Json(json!({
        "setting": stored.buffer_pool.to_text(), "next_start_bytes": stored.buffer_pool.bytes(), "warm_cache": stored.warm_cache(), "applies": "next start",
        "fast_writes": stored.fast_writes(), "disk": stored.disk.to_text(), "writes_applied": writes_applied,
    })))
}

/// Records the request and answers right away; the process stops (or restarts) just after,
/// once the response is out -- see `Lifecycle`.
async fn server_shutdown(State(s): State<Shared>) -> (StatusCode, Json<Value>) {
    s.lifecycle.request(LifecycleRequest::Shutdown);
    (StatusCode::ACCEPTED, Json(json!({ "ok": true, "action": "shutdown" })))
}

async fn server_restart(State(s): State<Shared>) -> (StatusCode, Json<Value>) {
    s.lifecycle.request(LifecycleRequest::Restart);
    (StatusCode::ACCEPTED, Json(json!({ "ok": true, "action": "restart" })))
}

async fn status(State(s): State<Shared>) -> ApiResult<api::status::Status> {
    Ok(Json(api::status::status(&s.app).await?))
}

async fn i18n_languages(State(s): State<Shared>) -> Json<Vec<String>> {
    Json(api::i18n::languages(&s.app))
}

async fn i18n_strings(State(s): State<Shared>, Path(lang): Path<String>) -> ApiResult<Value> {
    Ok(Json(api::i18n::strings(&s.app, &lang)?))
}

async fn vocabulary(State(s): State<Shared>) -> ApiResult<Value> {
    use crate::common::models::{Language, Region};
    let jobs = api::jobs::vocabulary();
    let platforms = api::catalog::platforms(&s.app).await?;
    let languages: Vec<Value> = Language::ALL.iter().map(|l| json!({ "code": l.code(), "english": l.english_name(), "native": l.native_name() })).collect();
    let regions: Vec<Value> = Region::ALL.iter().map(|r| json!({ "code": r.code(), "name": r.display_name() })).collect();
    Ok(Json(json!({
        "jobs": jobs,
        "dir_types": api::directories::DIR_TYPES,
        "media_roles": api::storage::MEDIA_ROLES,
        "media_types": api::settings::MEDIA_TYPES,
        "media": api::media::options(),
        "ui_color_kinds": api::settings::UI_COLOR_KINDS,
        "views": api::catalog::VIEWS,
        "platforms": platforms,
        // Platforms a list's platform filter shows and matches as one system.
        "platform_priority_default": crate::database::settings::default_platform_priority(),
        "platform_groups": api::catalog::PLATFORM_FILTER_GROUPS.iter().map(|group| group.iter().map(|p| *p as u16).collect::<Vec<_>>()).collect::<Vec<_>>(),
        "languages": languages,
        // The languages the UI's own text exists in, for the language menus.
        "ui_languages": api::i18n::languages(&s.app),
        "regions": regions,
        // The display filters' choices: every resolution some system draws, the presets.
        "display": {
            "resolutions": api::display::all_resolutions(),
            "aspects": crate::common::display::ASPECTS,
            "refresh": crate::common::display::REFRESH,
        },
        "icons": icon_files(&s.app.paths),
        "icons_dir": s.app.paths.icons_dir().display().to_string(),
    })))
}

#[derive(Debug, Deserialize)]
struct SetsExportQuery {
    /// Comma-separated set ids; every set when absent.
    ids: Option<String>,
}

async fn sets_export(State(s): State<Shared>, Query(q): Query<SetsExportQuery>) -> ApiResult<api::set_file::SetFile> {
    let ids: Vec<u64> = q.ids.as_deref().unwrap_or("").split(',').filter_map(|x| x.trim().parse().ok()).collect();
    Ok(Json(api::set_file::export(&s.app, &ids).await?))
}

async fn sets_import(State(s): State<Shared>, Json(req): Json<api::set_file::ImportRequest>) -> ApiResult<Vec<api::set_file::SetImported>> {
    Ok(Json(api::set_file::import(&s.app, &req).await?))
}

async fn sets_list(State(s): State<Shared>) -> ApiResult<Vec<api::sets::CollectionSet>> {
    Ok(Json(api::sets::list(&s.app).await?))
}

async fn sets_create(State(s): State<Shared>, Json(spec): Json<api::sets::SetSpec>) -> std::result::Result<(StatusCode, Json<api::sets::CollectionSet>), ApiError> {
    Ok((StatusCode::CREATED, Json(api::sets::create(&s.app, &spec).await?)))
}

async fn sets_get(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<api::sets::CollectionSet> {
    Ok(Json(api::sets::get(&s.app, id).await?))
}

async fn sets_update(State(s): State<Shared>, Path(id): Path<u64>, Json(spec): Json<api::sets::SetSpec>) -> ApiResult<api::sets::CollectionSet> {
    Ok(Json(api::sets::update(&s.app, id, &spec).await?))
}

async fn sets_delete(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<Value> {
    api::sets::delete(&s.app, id).await?;
    Ok(Json(json!({ "id": id, "deleted": true })))
}

async fn sets_releases(State(s): State<Shared>, Path(id): Path<u64>, Query(q): Query<api::catalog::ListQuery>) -> ApiResult<api::catalog::Page<api::catalog::ReleaseRow>> {
    Ok(Json(api::sets::releases(&s.app, id, &q).await?))
}

#[derive(Deserialize)]
struct SetReleaseBody {
    /// One release, several; titles (each an entry: its best release, held first, whenever
    /// the set is read); games from Cross-platform (IGDB ids, any platform).
    release_id: Option<u64>,
    #[serde(default)]
    release_ids: Vec<u64>,
    #[serde(default)]
    title_ids: Vec<u64>,
    /// Rows of the Titles list (a title's retail releases, its demos, one hack): each stays
    /// that row's.
    #[serde(default)]
    title_rows: Vec<romexplorer_types::copy::TitleRowRef>,
    /// The same by the Titles list's row numbers (`TitleRow::row_id`).
    #[serde(default)]
    title_row_ids: Vec<u64>,
    #[serde(default)]
    igdb_ids: Vec<String>,
    /// Families from Collection > Definitive, by any of their titles: each adds one release,
    /// as `copy_definitive` says.
    #[serde(default)]
    family_ids: Vec<u64>,
    /// RetroAchievements games: each adds one release, the best you hold of it, else its top one.
    #[serde(default)]
    ra_game_ids: Vec<u64>,
}

async fn sets_add_release(State(s): State<Shared>, Path(id): Path<u64>, Json(b): Json<SetReleaseBody>) -> ApiResult<Value> {
    let mut ids = b.release_ids.clone();
    ids.extend(b.release_id);
    ids.extend(api::sets::ra_game_releases(&s.app, &b.ra_game_ids).await?);
    let mut title_rows = b.title_rows.clone();
    title_rows.extend(crate::database::release_buckets::decode_row_ids(&s.app.pool, &b.title_row_ids).await.map_err(api::Error::from)?);
    if ids.is_empty() && b.title_ids.is_empty() && title_rows.is_empty() && b.igdb_ids.is_empty() && b.family_ids.is_empty() {
        let why = if b.ra_game_ids.is_empty() { "give release_id, release_ids, title_ids, title_rows, igdb_ids, family_ids or ra_game_ids" } else { "none of these RetroAchievements games is linked to a release in the catalogue" };
        return Err(api::Error::invalid(why).into());
    }
    use api::sets::EntryKind;
    let mut added = 0;
    if !ids.is_empty() {
        added += api::sets::add_releases(&s.app, id, &ids).await?;
    }
    if !b.title_ids.is_empty() {
        added += api::sets::add_entries(&s.app, id, EntryKind::Title, &b.title_ids.iter().map(u64::to_string).collect::<Vec<_>>()).await?;
    }
    if !title_rows.is_empty() {
        added += api::sets::add_title_rows(&s.app, id, &title_rows).await?;
    }
    if !b.igdb_ids.is_empty() {
        added += api::sets::add_entries(&s.app, id, EntryKind::Game, &b.igdb_ids).await?;
    }
    if !b.family_ids.is_empty() {
        added += api::sets::add_entries(&s.app, id, EntryKind::Family, &b.family_ids.iter().map(u64::to_string).collect::<Vec<_>>()).await?;
    }
    let set = api::sets::get(&s.app, id).await?;
    Ok(Json(json!({ "added": added, "set": set })))
}

#[derive(Deserialize)]
struct SetRemoveBody {
    release_ids: Vec<u64>,
}

async fn sets_remove_releases(State(s): State<Shared>, Path(id): Path<u64>, Json(b): Json<SetRemoveBody>) -> ApiResult<Value> {
    let removed = api::sets::remove_releases(&s.app, id, &b.release_ids).await?;
    let set = api::sets::get(&s.app, id).await?;
    Ok(Json(json!({ "removed": removed, "set": set })))
}

async fn sets_remove_release(State(s): State<Shared>, Path((id, release_id)): Path<(u64, u64)>) -> ApiResult<api::sets::CollectionSet> {
    Ok(Json(api::sets::remove_release(&s.app, id, release_id).await?))
}

#[derive(Deserialize)]
struct ReorderBody {
    #[serde(default)]
    release_ids: Vec<u64>,
    /// Two releases whose places swap, leaving every other entry where it is: what ▲/▼ on a
    /// paged or filtered list sends, since it only sees part of the set.
    #[serde(default)]
    swap: Option<[u64; 2]>,
}

async fn sets_reorder(State(s): State<Shared>, Path(id): Path<u64>, Json(b): Json<ReorderBody>) -> ApiResult<api::sets::CollectionSet> {
    if let Some([a, b]) = b.swap {
        return Ok(Json(api::sets::swap(&s.app, id, a, b).await?));
    }
    Ok(Json(api::sets::reorder(&s.app, id, &b.release_ids).await?))
}

#[derive(Deserialize)]
struct MediaQuery {
    /// `thumb`: a grid thumbnail (generated and cached on first request).
    size: Option<String>,
    /// Thumbnail format override: avif|jpeg|webp|jxl (webp/jxl fall back to jpeg for now).
    format: Option<String>,
}

// --- the audio library ---

async fn audio_list(State(s): State<Shared>, Query(q): Query<api::audio::AudioQuery>) -> ApiResult<api::audio::AudioPage> {
    Ok(Json(api::audio::list(&s.app, &q).await?))
}

async fn audio_facets(State(s): State<Shared>) -> ApiResult<api::audio::AudioFacets> {
    Ok(Json(api::audio::facets(&s.app).await?))
}

/// Without `q`: every tag in use. With `q` (a tag picker typing): up to 30 suggestions, tags
/// on audio files first and then catalogue keywords -- `scope=audio` leaves the keywords out.
async fn audio_tags(State(s): State<Shared>, Query(q): Query<api::audio::TagQuery>) -> ApiResult<Value> {
    if q.q.is_some() || q.scope.is_some() {
        return Ok(Json(json!(api::audio::suggest_tags(&s.app, &q).await?)));
    }
    let tags = api::audio::tags(&s.app).await?;
    Ok(Json(json!(tags.into_iter().map(|(name, files)| json!({ "name": name, "files": files })).collect::<Vec<_>>())))
}

async fn audio_tag_add(State(s): State<Shared>, Json(b): Json<api::audio::AudioTagRequest>) -> ApiResult<Value> {
    Ok(Json(json!({ "changed": api::audio::add_tag(&s.app, &b).await? })))
}

async fn audio_tag_remove(State(s): State<Shared>, Json(b): Json<api::audio::AudioTagRequest>) -> ApiResult<Value> {
    Ok(Json(json!({ "changed": api::audio::remove_tag(&s.app, &b).await? })))
}

async fn audio_playlist(State(s): State<Shared>, Query(q): Query<api::audio::PlaylistQuery>) -> ApiResult<api::audio::Playlist> {
    Ok(Json(api::audio::playlist(&s.app, &q).await?))
}

/// One track, streamed with `Range` support: a player seeks, and a long FLAC is not
/// something to send whole to find its last minute.
///
/// `?format=aac|xhe_aac&bitrate=N` is a client asking for the copy it keeps; a plain request
/// is a player, which gets the original or -- for DSD, ALAC, AIFF -- its cached conversion.
/// `X-Audio-Ext` says what the body is (the extension to save it under) and `X-Audio-Source`
/// whether it was converted, and if not, why.
async fn audio_file(State(s): State<Shared>, Path(id): Path<u64>, Query(q): Query<api::audio_convert::FileQuery>, headers: HeaderMap) -> std::result::Result<Response, ApiError> {
    let served = api::audio::file(&s.app, id, &q).await?;
    let mut response = stream_file(&served.path, served.mime, Some(&headers)).await?;
    let source = match (&served.note, served.converted) {
        (_, true) => "converted".to_string(),
        (Some(note), false) => format!("original; {note}"),
        (None, false) => "original".to_string(),
    };
    for (name, value) in [("x-audio-ext", served.ext.as_str()), ("x-audio-source", source.as_str())] {
        if let Ok(value) = axum::http::HeaderValue::from_str(value) {
            response.headers_mut().insert(name, value);
        }
    }
    Ok(response)
}

async fn media_strip(State(s): State<Shared>, Query(q): Query<api::media::StripQuery>) -> ApiResult<Vec<api::media::StripItem>> {
    Ok(Json(api::media::strip(&s.app, &q).await?))
}

/// Media this client does not keep, streamed from its main server.
async fn media_remote(State(s): State<Shared>, Path(id): Path<u64>, headers: HeaderMap) -> std::result::Result<Response, ApiError> {
    relay_from_main(&s.app, &format!("/api/v1/media/{id}"), &headers).await
}

/// A track of the main server's audio library, streamed.
async fn audio_remote(State(s): State<Shared>, Path(id): Path<u64>, headers: HeaderMap) -> std::result::Result<Response, ApiError> {
    relay_from_main(&s.app, &format!("/api/v1/audio/{id}/file"), &headers).await
}

/// Hands on the main server's reply as it arrives: its status (`200` / `206` / `416`), the
/// headers a media element reads, and the body unbuffered -- a video is never held here.
async fn relay_from_main(app: &api::App, path: &str, headers: &HeaderMap) -> std::result::Result<Response, ApiError> {
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    let upstream = api::media_sync::from_main(app, path, range).await?;
    let mut response = Response::builder().status(upstream.status().as_u16()).header(header::CACHE_CONTROL, "private, max-age=3600").header(header::ACCEPT_RANGES, "bytes");
    for name in [reqwest::header::CONTENT_TYPE, reqwest::header::CONTENT_LENGTH, reqwest::header::CONTENT_RANGE] {
        if let Some(value) = upstream.headers().get(&name).and_then(|v| v.to_str().ok()) {
            response = response.header(name.as_str(), value);
        }
    }
    response.body(axum::body::Body::from_stream(upstream.bytes_stream())).map_err(|e| Error::internal(format!("relay {path}: {e}")).into())
}

async fn media_file(State(s): State<Shared>, Path(id): Path<u64>, Query(q): Query<MediaQuery>, headers: HeaderMap) -> std::result::Result<Response, ApiError> {
    let thumb = q.size.as_deref() == Some("thumb");
    // `mime` belongs to the file actually served -- a thumbnail can be a different format in
    // a different cache tier than the original -- so it comes from here, not from the row.
    let (path, mime) = api::media::file(&s.app, id, thumb, q.format.as_deref()).await?;
    stream_file(&path, mime, Some(&headers)).await
}

/// A file as a streamed response, with single-range `Range` support when `headers` is given.
///
/// Streamed rather than `tokio::fs::read` into a `Vec`: media can be a 512 MB video, and the
/// route used to hold all of it in memory per request. `Range` is what lets `<video>` seek
/// and resume -- one `bytes=a-b` / `a-` / `-n` range, which is all a media element ever
/// sends; a multipart range or a malformed header is answered with the whole file (`200`),
/// which is what the RFC allows and what every server does. An unsatisfiable range is `416`
/// with `Content-Range: bytes */len`. tower-http's compression layer never touches a response
/// carrying `Content-Range`, so a `206` is never compressed regardless of content type.
async fn stream_file(path: &std::path::Path, mime: &str, headers: Option<&HeaderMap>) -> std::result::Result<Response, ApiError> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let mut file = tokio::fs::File::open(path).await.map_err(|e| Error::internal(format!("open {}: {e}", path.display())))?;
    let len = file.metadata().await.map_err(|e| Error::internal(format!("stat {}: {e}", path.display())))?.len();
    let cache = "private, max-age=3600";

    let range = headers.and_then(|h| h.get(header::RANGE)).and_then(|v| v.to_str().ok()).and_then(|v| byte_range(v, len));
    match range {
        Some(Err(())) => Ok((
            StatusCode::RANGE_NOT_SATISFIABLE,
            [(header::CONTENT_RANGE, format!("bytes */{len}")), (header::ACCEPT_RANGES, "bytes".to_string())],
        )
            .into_response()),
        Some(Ok((start, end))) => {
            file.seek(std::io::SeekFrom::Start(start)).await.map_err(|e| Error::internal(format!("seek {}: {e}", path.display())))?;
            let count = end - start + 1;
            let body = axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(file.take(count)));
            Ok((
                StatusCode::PARTIAL_CONTENT,
                [
                    (header::CONTENT_TYPE, mime.to_string()),
                    (header::CACHE_CONTROL, cache.to_string()),
                    (header::ACCEPT_RANGES, "bytes".to_string()),
                    (header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}")),
                    (header::CONTENT_LENGTH, count.to_string()),
                ],
                body,
            )
                .into_response())
        }
        None => {
            let body = axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(file));
            Ok((
                [
                    (header::CONTENT_TYPE, mime.to_string()),
                    (header::CACHE_CONTROL, cache.to_string()),
                    (header::ACCEPT_RANGES, "bytes".to_string()),
                    (header::CONTENT_LENGTH, len.to_string()),
                ],
                body,
            )
                .into_response())
        }
    }
}

/// One `Range: bytes=...` header against a file of `len` bytes: `Ok((first, last))` inclusive,
/// `Err(())` when the range is syntactically fine but nothing in it lies inside the file
/// (`416`), and `None` when the header is not a single byte range we handle (serve it all).
fn byte_range(header_value: &str, len: u64) -> Option<std::result::Result<(u64, u64), ()>> {
    let spec = header_value.strip_prefix("bytes=")?.trim();
    if spec.contains(',') {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (a, b) = (a.trim(), b.trim());
    if len == 0 {
        return Some(Err(()));
    }
    let range = match (a.is_empty(), b.is_empty()) {
        // `-n`: the last n bytes.
        (true, false) => {
            let n: u64 = b.parse().ok()?;
            if n == 0 {
                return Some(Err(()));
            }
            (len.saturating_sub(n), len - 1)
        }
        // `a-`: from a to the end.
        (false, true) => {
            let start: u64 = a.parse().ok()?;
            if start >= len {
                return Some(Err(()));
            }
            (start, len - 1)
        }
        // `a-b`, clamped to the file.
        (false, false) => {
            let start: u64 = a.parse().ok()?;
            let end: u64 = b.parse().ok()?;
            if start > end || start >= len {
                return Some(Err(()));
            }
            (start, end.min(len - 1))
        }
        (true, true) => return None,
    };
    Some(Ok(range))
}

#[derive(Debug, Deserialize)]
struct BackupQuery {
    /// `database` | `catalog`; absent lists both.
    kind: Option<String>,
}

fn backup_kind(name: &str) -> std::result::Result<api::backup::Kind, Error> {
    api::backup::Kind::parse(name).ok_or_else(|| Error::invalid("kind must be database, catalog or dat-seed"))
}

async fn backup_list(State(s): State<Shared>, Query(q): Query<BackupQuery>) -> ApiResult<Vec<api::backup::BackupFile>> {
    let kind = q.kind.as_deref().map(backup_kind).transpose()?;
    Ok(Json(api::backup::list(&s.app, kind).await?))
}

/// Enqueues the job rather than writing the file here: a catalogue backup takes minutes, so
/// it belongs in the worker with the same progress and history as every other long job.
async fn backup_create(State(s): State<Shared>, Query(q): Query<BackupQuery>) -> ApiResult<api::jobs::JobEnqueued> {
    let request = match backup_kind(q.kind.as_deref().unwrap_or("database"))? {
        api::backup::Kind::Database => api::jobs::JobRequest::BackupDatabase,
        api::backup::Kind::Catalog => api::jobs::JobRequest::BackupCatalog,
        api::backup::Kind::DatSeed => api::jobs::JobRequest::BackupDatSeed,
    };
    let id = api::jobs::enqueue(&s.app, &request).await?;
    Ok(Json(api::jobs::JobEnqueued { job_id: id }))
}

/// Streamed, not buffered: a catalogue backup can be several GB.
async fn backup_download(State(s): State<Shared>, Path(name): Path<String>) -> std::result::Result<Response, ApiError> {
    let path = api::backup::file(&s.app, &name).await?;
    let file = tokio::fs::File::open(&path).await.map_err(|e| Error::internal(format!("open {}: {e}", path.display())))?;
    let body = axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(file));
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
        ],
        body,
    )
        .into_response())
}

async fn backup_delete(State(s): State<Shared>, Path(name): Path<String>) -> ApiResult<serde_json::Value> {
    api::backup::remove(&s.app, &name).await?;
    Ok(Json(serde_json::json!({ "deleted": name })))
}

// -- sync: what another machine may read from this one ------------------------------------
//
// `with_catalog` is what separates a main server from a share. The full router is a main
// server and offers both halves; `share_router` passes false, so a client that shares its
// collection offers only what it holds. The decision is made here, on the serving side,
// rather than trusted to whatever the client asks for.

async fn sync_manifest(State(s): State<Shared>) -> ApiResult<api::sync::Manifest> {
    Ok(Json(api::sync::manifest(&s.app, true).await?))
}

async fn share_manifest(State(s): State<Shared>) -> ApiResult<api::sync::Manifest> {
    Ok(Json(api::sync::manifest(&s.app, false).await?))
}

#[derive(Debug, Deserialize)]
struct ExportQuery {
    /// The previous page's `next`, as a JSON array -- one value per primary-key column, in
    /// order.
    ///
    /// One opaque parameter rather than a repeated `after=`: `Query` deserialises with
    /// `serde_urlencoded`, which cannot build a `Vec` from repeated keys, so a repeated form
    /// fails with 400 on the *second* page only -- the first page has no cursor at all, so it
    /// works, and the break looks like a paging bug rather than a parsing one. A JSON array
    /// also carries values containing `,`, `&` or `=` without further escaping, and file
    /// paths are primary keys here.
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
    /// Only the rows changed after this change number, paged by change number rather than by
    /// key (`api::sync::export_since`). `after` is then that one number.
    #[serde(default)]
    since: Option<u64>,
}

impl ExportQuery {
    /// `after` as a change number, for a `since` export: `[n]` as a page's `next` gives it.
    fn rev_cursor(&self) -> std::result::Result<Option<u64>, Error> {
        match self.cursor()? {
            None => Ok(None),
            Some(values) => match values.as_slice() {
                [one] => one.parse().map(Some).map_err(|_| Error::invalid("after must be a change number with since")),
                _ => Err(Error::invalid("after must be a single change number with since")),
            },
        }
    }

    fn cursor(&self) -> std::result::Result<Option<Vec<String>>, Error> {
        match self.after.as_deref().filter(|s| !s.is_empty()) {
            None => Ok(None),
            Some(text) => serde_json::from_str::<Vec<String>>(text)
                .map(Some)
                .map_err(|e| Error::invalid(format!("after must be a JSON array of primary-key values: {e}"))),
        }
    }
}

async fn sync_export(State(s): State<Shared>, Path(table): Path<String>, Query(q): Query<ExportQuery>) -> ApiResult<api::sync::ExportPage> {
    if let Some(since) = q.since {
        return Ok(Json(api::sync::export_since(&s.app, &table, since, q.rev_cursor()?, q.limit.unwrap_or(2_000), true).await?));
    }
    let after = q.cursor()?;
    Ok(Json(api::sync::export(&s.app, &table, after.as_deref(), q.limit.unwrap_or(2_000), true).await?))
}

#[derive(Debug, Deserialize)]
struct DeletedQuery {
    since: u64,
    #[serde(default)]
    after: Option<u64>,
    #[serde(default)]
    limit: Option<u32>,
}

/// The rows of a table deleted after a change number, so a client can delete them too.
async fn sync_deleted(State(s): State<Shared>, Path(table): Path<String>, Query(q): Query<DeletedQuery>) -> ApiResult<api::sync::DeletedPage> {
    Ok(Json(api::sync::deleted_since(&s.app, &table, q.since, q.after, q.limit.unwrap_or(10_000), true).await?))
}

async fn share_deleted(State(s): State<Shared>, Path(table): Path<String>, Query(q): Query<DeletedQuery>) -> ApiResult<api::sync::DeletedPage> {
    Ok(Json(api::sync::deleted_since(&s.app, &table, q.since, q.after, q.limit.unwrap_or(10_000), false).await?))
}

async fn share_export(State(s): State<Shared>, Path(table): Path<String>, Query(q): Query<ExportQuery>) -> ApiResult<api::sync::ExportPage> {
    if let Some(since) = q.since {
        return Ok(Json(api::sync::export_since(&s.app, &table, since, q.rev_cursor()?, q.limit.unwrap_or(2_000), false).await?));
    }
    let after = q.cursor()?;
    Ok(Json(api::sync::export(&s.app, &table, after.as_deref(), q.limit.unwrap_or(2_000), false).await?))
}

// -- Config › AI ----------------------------------------------------------------------------

async fn ai_overview(State(s): State<Shared>) -> ApiResult<api::ai::Overview> {
    Ok(Json(api::ai::overview(&s.app).await?))
}

async fn ai_clear(State(s): State<Shared>, Json(req): Json<api::ai::ClearRequest>) -> ApiResult<Value> {
    Ok(Json(json!({ "removed": api::ai::clear(&s.app, &req).await? })))
}

async fn ai_export(State(s): State<Shared>) -> ApiResult<Vec<api::ai::Answer>> {
    Ok(Json(api::ai::export(&s.app).await?))
}

async fn ai_import(State(s): State<Shared>, Json(answers): Json<Vec<api::ai::Answer>>) -> ApiResult<Value> {
    Ok(Json(json!({ "imported": api::ai::import(&s.app, &answers).await? })))
}

async fn ai_test(State(s): State<Shared>, Path(provider): Path<String>) -> ApiResult<Value> {
    Ok(Json(api::ai::test(&s.app, &provider).await?))
}

#[derive(Deserialize)]
struct PinBody {
    pinned: bool,
}

async fn ai_pin(State(s): State<Shared>, Path(id): Path<u64>, Json(b): Json<PinBody>) -> ApiResult<Value> {
    api::ai::set_pinned(&s.app, id, b.pinned).await?;
    Ok(Json(json!({ "id": id, "pinned": b.pinned })))
}

async fn media_reuse_prompt(State(s): State<Shared>) -> ApiResult<api::media::ReusePrompt> {
    Ok(Json(api::media::reuse_prompt(&s.app).await?))
}

// -- sources: the servers and peers this machine syncs FROM --------------------------------

/// The options and the warning travel with the value so the first-run prompt, the console
/// and the GUI show the same words instead of three paraphrases that drift apart.
async fn mode_get(State(s): State<Shared>) -> ApiResult<api::sources::ModeView> {
    Ok(Json(api::sources::mode_view(&s.app).await?))
}

/// The mode is the desktop app's to choose: it decides whether *that machine* builds a
/// catalogue or takes one from a main server. A server is the thing others take theirs from,
/// so it is standalone by definition (`romexplorer_server` refuses to start otherwise) and
/// nothing reaching it over the network may say different.
async fn mode_set(State(s): State<Shared>, Json(b): Json<api::sources::ModeBody>) -> ApiResult<Value> {
    if !s.in_process {
        return Err(api::Error::forbidden("the instance mode is chosen in the desktop app on that machine; a server is always standalone").into());
    }
    Ok(Json(api::sources::set_mode(&s.app, &b.mode, b.replace_catalog, b.media).await?))
}

// --- copying ROMs out of the collection ---

async fn copy_defaults(State(s): State<Shared>) -> ApiResult<api::copy::CopyDefaults> {
    Ok(Json(api::copy::defaults(&s.app).await?))
}

async fn copy_defaults_put(State(s): State<Shared>, Json(update): Json<api::copy::DefaultsUpdate>) -> ApiResult<api::copy::CopyDefaults> {
    Ok(Json(api::copy::set_defaults(&s.app, &update).await?))
}

/// What a copy would do, with no side effects -- the same planning pass the job runs.
async fn copy_preview(State(s): State<Shared>, Json(req): Json<api::copy::CopyFilesRequest>) -> ApiResult<api::copy::CopyPlan> {
    Ok(Json(api::copy::plan::plan(&s.app, &req).await?.view))
}

async fn copy_start(State(s): State<Shared>, Json(req): Json<api::copy::CopyFilesRequest>) -> std::result::Result<(StatusCode, Json<api::jobs::JobEnqueued>), ApiError> {
    let job_id = api::jobs::enqueue(&s.app, &api::jobs::JobRequest::CopyFiles(req)).await?;
    Ok((StatusCode::ACCEPTED, Json(api::jobs::JobEnqueued { job_id })))
}

/// What an integrity run would fill in, and which hashes it would compare against here --
/// without reading a byte.
async fn integrity_preview(State(s): State<Shared>, Query(q): Query<api::integrity::IntegrityRequest>) -> ApiResult<serde_json::Value> {
    let populate: String = api::integrity::populate_algorithm(&s.app, &q).await?;
    let order: Vec<&str> = crate::rom::hash::fallback_order().iter().map(|a| api::copy::algorithm_name(*a)).collect();
    Ok(Json(serde_json::json!({ "populate": populate, "compare_order": order })))
}

async fn integrity_start(State(s): State<Shared>, Json(req): Json<api::integrity::IntegrityRequest>) -> std::result::Result<(StatusCode, Json<api::jobs::JobEnqueued>), ApiError> {
    let job_id = api::jobs::enqueue(&s.app, &api::jobs::JobRequest::IntegrityCheck(req)).await?;
    Ok((StatusCode::ACCEPTED, Json(api::jobs::JobEnqueued { job_id })))
}

/// What a sheet run would do, with nothing done -- including the rename table, which is the
/// thing the caller has to show before a rename may be confirmed.
async fn sheets_preview(State(s): State<Shared>, Json(req): Json<romexplorer_types::sheets::SheetCreateRequest>) -> ApiResult<romexplorer_types::sheets::SheetPlan> {
    Ok(Json(api::sheets::run::preview(&s.app, &req).await?))
}

async fn sheets_create(State(s): State<Shared>, Json(req): Json<romexplorer_types::sheets::SheetCreateRequest>) -> std::result::Result<(StatusCode, Json<api::jobs::JobEnqueued>), ApiError> {
    let job_id = api::jobs::enqueue(&s.app, &api::jobs::JobRequest::SheetCreate(req)).await?;
    Ok((StatusCode::ACCEPTED, Json(api::jobs::JobEnqueued { job_id })))
}

async fn sheets_verify(State(s): State<Shared>, Json(req): Json<romexplorer_types::sheets::SheetVerifyRequest>) -> std::result::Result<(StatusCode, Json<api::jobs::JobEnqueued>), ApiError> {
    let job_id = api::jobs::enqueue(&s.app, &api::jobs::JobRequest::SheetVerify(req)).await?;
    Ok((StatusCode::ACCEPTED, Json(api::jobs::JobEnqueued { job_id })))
}

async fn m3u_verify(State(s): State<Shared>, Json(req): Json<romexplorer_types::sheets::SheetVerifyRequest>) -> std::result::Result<(StatusCode, Json<api::jobs::JobEnqueued>), ApiError> {
    let job_id = api::jobs::enqueue(&s.app, &api::jobs::JobRequest::M3uVerify(req)).await?;
    Ok((StatusCode::ACCEPTED, Json(api::jobs::JobEnqueued { job_id })))
}

/// Re-reads one sidecar now, rather than waiting for a sweep.
async fn sheet_file_verify(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<romexplorer_types::sheets::SheetVerdictView> {
    Ok(Json(api::sheets::fix::verify_one(&s.app, id).await?))
}

/// Tries to make one sidecar match what the catalogue records for it.
async fn sheet_file_fix(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<romexplorer_types::sheets::FixResult> {
    Ok(Json(api::sheets::fix::fix_one(&s.app, id).await?))
}

async fn naming_folders(State(s): State<Shared>) -> ApiResult<api::naming::NamingView> {
    Ok(Json(api::naming::view(&s.app).await?))
}

async fn naming_folders_put(State(s): State<Shared>, Json(update): Json<api::naming::NamingUpdate>) -> ApiResult<api::naming::NamingView> {
    Ok(Json(api::naming::update(&s.app, &update).await?))
}

async fn naming_folders_reset(State(s): State<Shared>) -> ApiResult<api::naming::NamingView> {
    api::naming::reset_folders(&s.app).await?;
    Ok(Json(api::naming::view(&s.app).await?))
}

async fn source_roots(State(s): State<Shared>, Path(id): Path<u16>) -> ApiResult<Vec<serde_json::Value>> {
    let roots = api::copy::mount::roots(&s.app, id).await?;
    Ok(Json(roots.into_iter().map(|(id, path)| serde_json::json!({ "id": id, "path": path })).collect()))
}

async fn source_mounts(State(s): State<Shared>, Path(id): Path<u16>) -> ApiResult<Vec<api::copy::Mount>> {
    Ok(Json(api::copy::mount::list(&s.app, id).await?))
}

async fn source_mount_create(State(s): State<Shared>, Path(id): Path<u16>, Json(spec): Json<api::copy::MountSpec>) -> std::result::Result<(StatusCode, Json<api::copy::Mount>), ApiError> {
    Ok((StatusCode::CREATED, Json(api::copy::mount::create(&s.app, id, &spec).await?)))
}

async fn mount_delete(State(s): State<Shared>, Path(id): Path<u16>) -> ApiResult<serde_json::Value> {
    api::copy::mount::remove(&s.app, id).await?;
    Ok(Json(serde_json::json!({ "deleted": true })))
}

async fn mount_check(State(s): State<Shared>, Path(id): Path<u16>) -> ApiResult<api::copy::MountCheck> {
    let mount = api::copy::mount::get(&s.app, id).await?;
    let resolved = api::copy::mount::Resolved {
        id: mount.id,
        source_id: mount.source_id,
        remote_prefix: mount.remote_prefix.clone(),
        remote_sep: mount.remote_sep.chars().next().unwrap_or('/'),
        local_path: mount.local_path.clone(),
    };
    let check = api::copy::mount::verify(&s.app, &resolved).await?;
    if check.ok {
        let _ = api::copy::mount::mark_verified(&s.app, id).await;
    }
    Ok(Json(check))
}

async fn sources_list(State(s): State<Shared>) -> ApiResult<Vec<api::sources::RemoteSource>> {
    Ok(Json(api::sources::list(&s.app).await?))
}

async fn sources_create(State(s): State<Shared>, Json(spec): Json<api::sources::SourceSpec>) -> std::result::Result<(StatusCode, Json<api::sources::RemoteSource>), ApiError> {
    Ok((StatusCode::CREATED, Json(api::sources::create(&s.app, &spec).await?)))
}

async fn sources_get(State(s): State<Shared>, Path(id): Path<u16>) -> ApiResult<api::sources::RemoteSource> {
    Ok(Json(api::sources::get(&s.app, id).await?))
}

async fn sources_update(State(s): State<Shared>, Path(id): Path<u16>, Json(spec): Json<api::sources::SourceSpec>) -> ApiResult<api::sources::RemoteSource> {
    Ok(Json(api::sources::update(&s.app, id, &spec).await?))
}

async fn sources_delete(State(s): State<Shared>, Path(id): Path<u16>) -> ApiResult<Value> {
    Ok(Json(json!({ "deleted": api::sources::delete(&s.app, id).await? })))
}

/// Forgets the certificate trusted for a source, after it was replaced on purpose.
async fn sources_trust(State(s): State<Shared>, Path(id): Path<u16>) -> ApiResult<api::sources::RemoteSource> {
    Ok(Json(api::sources::trust_new_certificate(&s.app, id).await?))
}

async fn sources_forget(State(s): State<Shared>, Path(id): Path<u16>) -> ApiResult<Value> {
    Ok(Json(json!({ "rows_removed": api::sources::forget(&s.app, id).await? })))
}

async fn sources_check(State(s): State<Shared>, Path(id): Path<u16>) -> ApiResult<api::sources::Reachability> {
    Ok(Json(api::sources::check(&s.app, id).await?))
}

/// Enqueues the pull. Syncing is a job like any other long operation.
#[derive(Debug, Deserialize)]
struct SyncQuery {
    /// Download every table whole rather than only what changed (the "full sync" button).
    #[serde(default)]
    full: bool,
}

async fn sources_sync(State(s): State<Shared>, Path(id): Path<u16>, Query(q): Query<SyncQuery>) -> ApiResult<api::jobs::JobEnqueued> {
    let request = api::jobs::JobRequest::CatalogSync(api::jobs::CatalogSyncRequest { source_id: id, full: q.full });
    Ok(Json(api::jobs::JobEnqueued { job_id: api::jobs::enqueue(&s.app, &request).await? }))
}

/// Makes sure the file behind a media row is on disk again (`api::media::refetch`): a
/// client's way of turning a 404 into a re-fetch rather than a provider call of its own.
async fn media_refetch(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<api::media::RefetchResult> {
    Ok(Json(api::media::refetch(&s.app, id).await?))
}

/// Fetches a provider's art into this server for a client (`api::media::fetch_for`). Only
/// a test lets it reach 127.0.0.1 (`fetch_loopback`).
async fn media_fetch(State(s): State<Shared>, Json(req): Json<api::media::FetchRequest>) -> ApiResult<api::media::FetchResult> {
    Ok(Json(api::media::fetch_for(&s.app, &req, s.fetch_loopback).await?))
}

/// Enqueues a fetch of the art this source holds and this machine lacks (`api::media_sync`).
async fn sources_media(State(s): State<Shared>, Path(id): Path<u16>) -> ApiResult<api::jobs::JobEnqueued> {
    let request = api::jobs::JobRequest::MediaSync(api::jobs::MediaSyncRequest { source_id: Some(id) });
    Ok(Json(api::jobs::JobEnqueued { job_id: api::jobs::enqueue(&s.app, &request).await? }))
}

// -- restore and the SQLite cache ----------------------------------------------------------

async fn backup_restore(State(s): State<Shared>, Path(name): Path<String>) -> ApiResult<api::jobs::JobEnqueued> {
    let request = api::jobs::JobRequest::RestoreBackup(api::jobs::NamedFileRequest { name, adopt_ownership: false });
    Ok(Json(api::jobs::JobEnqueued { job_id: api::jobs::enqueue(&s.app, &request).await? }))
}

#[derive(Debug, Deserialize)]
struct ImportQuery {
    /// Take the `local_*` tables too, even from another installation (see
    /// `api::sqlite::import`). A phone sets this; a desktop almost never should.
    #[serde(default)]
    adopt_ownership: bool,
}

async fn cache_list(State(s): State<Shared>) -> ApiResult<Vec<api::sqlite::CacheFile>> {
    Ok(Json(api::sqlite::list(&s.app).await?))
}

async fn cache_create(State(s): State<Shared>, Query(q): Query<BackupQuery>) -> ApiResult<api::jobs::JobEnqueued> {
    let kind = q.kind.unwrap_or_else(|| "all".into());
    // Checked here so a typo is a 400 with the accepted values, not a job that fails later.
    api::sqlite::tables_for(&kind)?;
    let request = api::jobs::JobRequest::SqliteExport(api::jobs::SqliteExportRequest { kind });
    Ok(Json(api::jobs::JobEnqueued { job_id: api::jobs::enqueue(&s.app, &request).await? }))
}

/// Streamed like a backup: a cache of the whole catalogue is tens of MB.
async fn cache_download(State(s): State<Shared>, Path(name): Path<String>) -> std::result::Result<Response, ApiError> {
    let path = api::sqlite::file(&s.app, &name).await?;
    let file = tokio::fs::File::open(&path).await.map_err(|e| Error::internal(format!("open {}: {e}", path.display())))?;
    let body = axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(file));
    Ok((
        [
            (header::CONTENT_TYPE, "application/vnd.sqlite3".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
        ],
        body,
    )
        .into_response())
}

async fn cache_delete(State(s): State<Shared>, Path(name): Path<String>) -> ApiResult<Value> {
    api::sqlite::remove(&s.app, &name).await?;
    Ok(Json(json!({ "deleted": name })))
}

async fn cache_import(State(s): State<Shared>, Path(name): Path<String>, Query(q): Query<ImportQuery>) -> ApiResult<api::jobs::JobEnqueued> {
    let request = api::jobs::JobRequest::SqliteImport(api::jobs::NamedFileRequest { name, adopt_ownership: q.adopt_ownership });
    Ok(Json(api::jobs::JobEnqueued { job_id: api::jobs::enqueue(&s.app, &request).await? }))
}

async fn media_manual(State(s): State<Shared>, Json(b): Json<api::media::ManualUrl>) -> ApiResult<api::media::ManualResult> {
    Ok(Json(api::media::set_from_url(&s.app, &b).await?))
}

/// multipart fields: `title_id` or `release_id`, `media_type`, `sub_category`?,
/// `on_conflict`? (`overwrite` | `rename`), `file`.
async fn media_upload(State(s): State<Shared>, mut form: Multipart) -> ApiResult<api::media::ManualResult> {
    let bad = |e: axum::extract::multipart::MultipartError| Error::invalid(format!("upload: {e}"));
    let mut target = api::media::SlotTarget::default();
    let (mut media_type, mut sub, mut file, mut on_conflict) = (None::<String>, None::<String>, None::<(String, Vec<u8>)>, None);
    while let Some(field) = form.next_field().await.map_err(bad)? {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "file" => {
                let file_name = field.file_name().unwrap_or("upload").to_string();
                file = Some((file_name, field.bytes().await.map_err(bad)?.to_vec()));
            }
            other => {
                let text = field.text().await.map_err(bad)?;
                let text = text.trim().to_string();
                let id = || text.parse::<u64>().map_err(|_| Error::invalid(format!("{other} must be a number")));
                match other {
                    "title_id" if !text.is_empty() => target.title_id = Some(id()?),
                    "release_id" if !text.is_empty() => target.release_id = Some(id()?),
                    "media_type" => media_type = Some(text),
                    "sub_category" => sub = Some(text).filter(|t| !t.is_empty()),
                    "on_conflict" => on_conflict = api::media::OnConflict::parse(&text)?,
                    _ => {}
                }
            }
        }
    }
    let media_type = media_type.ok_or_else(|| Error::invalid("media_type is required"))?;
    let (file_name, bytes) = file.ok_or_else(|| Error::invalid("file is required"))?;
    Ok(Json(api::media::upload(&s.app, &target, &media_type, sub.as_deref(), &file_name, &bytes, on_conflict).await?))
}

/// A backup zip from another machine (or kept elsewhere) into this machine's backup
/// directory, listed like any other; restoring it is a separate step. Streamed to disk a
/// chunk at a time -- a catalogue backup runs to gigabytes -- then checked and named by
/// `api::backup::accept_upload`.
async fn backup_upload(State(s): State<Shared>, mut form: Multipart) -> ApiResult<api::backup::BackupFile> {
    use tokio::io::AsyncWriteExt;
    let bad = |e: axum::extract::multipart::MultipartError| Error::invalid(format!("upload: {e}"));
    while let Some(mut field) = form.next_field().await.map_err(bad)? {
        if field.name() != Some("file") {
            continue;
        }
        let original = field.file_name().unwrap_or("backup.zip").to_string();
        let part = api::backup::upload_part_path(&s.app).await?;
        // Removed however this ends, a client that hangs up mid-upload included (the
        // handler is dropped then); accepting it renames it away first.
        let _leftover = RemoveOnDrop(part.clone());
        let received = async {
            let mut out = tokio::fs::File::create(&part).await.map_err(|e| Error::internal(format!("{}: {e}", part.display())))?;
            while let Some(chunk) = field.chunk().await.map_err(bad)? {
                out.write_all(&chunk).await.map_err(|e| Error::internal(format!("{}: {e}", part.display())))?;
            }
            out.flush().await.map_err(|e| Error::internal(e.to_string()))
        }
        .await;
        if let Err(e) = received {
            let _ = std::fs::remove_file(&part);
            return Err(e.into());
        }
        return Ok(Json(api::backup::accept_upload(&s.app, &part, &original).await?));
    }
    Err(Error::invalid("file is required").into())
}

async fn media_remove(State(s): State<Shared>, Path(id): Path<u64>, Query(q): Query<api::media::SlotTarget>) -> ApiResult<api::media::RemoveResult> {
    Ok(Json(api::media::remove(&s.app, id, &q).await?))
}

/// A cached image file as a response (RetroAchievements media). Streamed like `media_file`,
/// without `Range` -- these are images, nothing seeks in them.
async fn image_file(path: std::path::PathBuf) -> std::result::Result<Response, ApiError> {
    let mime = crate::database::media_store::mime_for(&path);
    stream_file(&path, mime, None).await
}

/// `full.csv` | `stripped.csv`, downloaded as a file.
async fn dev_counterparts(State(s): State<Shared>, Path(file): Path<String>) -> std::result::Result<Response, ApiError> {
    let kind = file.strip_suffix(".csv").unwrap_or(&file);
    let csv = api::dev::counterparts_csv(&s.app, kind).await?;
    let disposition = format!("attachment; filename=\"counterparts_{kind}.csv\"");
    Ok(([(header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()), (header::CONTENT_DISPOSITION, disposition)], csv).into_response())
}

async fn titles_merge(State(s): State<Shared>, Json(b): Json<api::titles::MergeRequest>) -> ApiResult<api::titles::MergeResult> {
    Ok(Json(api::titles::merge(&s.app, &b).await?))
}

async fn titles_suggestions(State(s): State<Shared>, Query(q): Query<api::titles::SuggestionsQuery>) -> ApiResult<api::titles::SuggestionsPage> {
    Ok(Json(api::titles::suggestions(&s.app, &q).await?))
}

async fn titles_suggestion_resolve(State(s): State<Shared>, Path((igdb_id, action)): Path<(String, String)>) -> ApiResult<Value> {
    let accept = match action.as_str() {
        "accept" => true,
        "dismiss" => false,
        _ => return Err(Error::invalid("action must be accept or dismiss").into()),
    };
    let title_id = api::titles::resolve_suggestion(&s.app, &igdb_id, accept).await?;
    Ok(Json(json!({ "title_id": title_id })))
}

async fn review_merges(State(s): State<Shared>, Query(q): Query<api::catalog::ListQuery>, Query(p): Query<api::review::ReviewParams>) -> ApiResult<api::review::ReviewPage<api::review::MergeItem>> {
    Ok(Json(api::review::merge_queue(&s.app, &q, &p).await?))
}

/// `apply` (body `{option}`), `dismiss`, or `lookup` (what the web says about a variant).
async fn review_merge_action(State(s): State<Shared>, Path((key, action)): Path<(String, String)>, body: Option<Json<api::review::ApplyRequest>>) -> ApiResult<Value> {
    match action.as_str() {
        "apply" => {
            let req = body.map(|Json(b)| b).unwrap_or_default();
            Ok(Json(serde_json::to_value(api::review::apply(&s.app, &key, &req).await?).unwrap_or_default()))
        }
        "dismiss" => {
            api::review::dismiss(&s.app, &key).await?;
            Ok(Json(json!({ "dismissed": key })))
        }
        "lookup" => {
            let req = body.map(|Json(b)| b).unwrap_or_default();
            Ok(Json(serde_json::to_value(api::review::lookup(&s.app, &key, &req).await?).unwrap_or_default()))
        }
        _ => Err(Error::invalid("action must be apply, dismiss or lookup").into()),
    }
}

async fn review_aspects(State(s): State<Shared>, Query(q): Query<api::catalog::ListQuery>, Query(p): Query<api::review::ReviewParams>) -> ApiResult<api::review::ReviewPage<api::review::AspectItem>> {
    Ok(Json(api::review::aspect_queue(&s.app, &q, &p).await?))
}

async fn titles_suggestions_bulk(State(s): State<Shared>, Json(b): Json<api::titles::BulkSuggestions>) -> ApiResult<crate::database::title_merge::BulkResolved> {
    Ok(Json(api::titles::resolve_suggestions(&s.app, &b).await?))
}

async fn titles_add_keyword(State(s): State<Shared>, Json(b): Json<api::titles::KeywordRequest>) -> ApiResult<Value> {
    let n = api::titles::add_keyword(&s.app, &b).await?;
    Ok(Json(json!({ "titles": n })))
}

async fn titles_remove_keyword_bulk(State(s): State<Shared>, Json(b): Json<api::titles::KeywordRemoveRequest>) -> ApiResult<Value> {
    let n = api::titles::remove_keyword_bulk(&s.app, &b).await?;
    Ok(Json(json!({ "titles": n })))
}

async fn keywords_remove_auto(State(s): State<Shared>, Json(b): Json<api::titles::AutoTagRemove>) -> ApiResult<api::titles::AutoTagRemoved> {
    Ok(Json(api::titles::remove_auto_tags(&s.app, &b).await?))
}

async fn dictionary_list(State(s): State<Shared>, Query(q): Query<api::dictionary::DictQuery>) -> ApiResult<api::dictionary::DictPage> {
    Ok(Json(api::dictionary::list(&s.app, &q).await?))
}

async fn dictionary_add(State(s): State<Shared>, Json(b): Json<api::dictionary::DictAdd>) -> ApiResult<Value> {
    Ok(Json(json!({ "group_key": api::dictionary::add(&s.app, &b).await? })))
}

async fn dictionary_remove(State(s): State<Shared>, Json(b): Json<api::dictionary::DictRemove>) -> ApiResult<Value> {
    Ok(Json(json!({ "removed": api::dictionary::remove(&s.app, &b).await? })))
}

async fn dictionary_clear_blocks(State(s): State<Shared>) -> ApiResult<Value> {
    Ok(Json(json!({ "cleared": api::dictionary::clear_blocks(&s.app).await? })))
}

#[derive(Debug, Deserialize)]
struct VariantsQuery {
    q: String,
}

async fn dictionary_variants(State(s): State<Shared>, Query(q): Query<VariantsQuery>) -> ApiResult<Value> {
    Ok(Json(json!(api::dictionary::variants(&s.app, &q.q).await?)))
}

async fn keywords_list(State(s): State<Shared>, Query(q): Query<api::titles::KeywordListQuery>) -> ApiResult<api::titles::KeywordPage> {
    Ok(Json(api::titles::keywords(&s.app, &q).await?))
}

async fn keyword_rename(State(s): State<Shared>, Path(id): Path<u64>, Json(b): Json<api::titles::KeywordRename>) -> ApiResult<Value> {
    let id = api::titles::rename_keyword(&s.app, id, &b).await?;
    Ok(Json(json!({ "id": id })))
}

async fn keyword_delete(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<Value> {
    let n = api::titles::delete_keyword(&s.app, id).await?;
    Ok(Json(json!({ "removed_tags": n })))
}

async fn keywords_delete_unused(State(s): State<Shared>) -> ApiResult<Value> {
    let n = api::titles::delete_unused_keywords(&s.app).await?;
    Ok(Json(json!({ "deleted": n })))
}

async fn title_release_move(State(s): State<Shared>, Path(id): Path<u64>, Json(b): Json<api::titles::ReleaseMoveRequest>) -> ApiResult<Value> {
    let order = api::titles::move_release(&s.app, id, &b).await?;
    Ok(Json(json!({ "order": order })))
}

async fn title_release_order_reset(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<Value> {
    api::titles::reset_release_order(&s.app, id).await?;
    Ok(Json(json!({ "reset": true })))
}

async fn titles_release_orders(State(s): State<Shared>, Query(q): Query<api::titles::ReleaseOrderQuery>) -> ApiResult<api::titles::ReleaseOrderPage> {
    Ok(Json(api::titles::release_orders(&s.app, &q).await?))
}

async fn titles_release_orders_clear(State(s): State<Shared>, Json(b): Json<api::titles::ReleaseOrderClear>) -> ApiResult<Value> {
    let n = api::titles::clear_release_orders(&s.app, &b).await?;
    Ok(Json(json!({ "cleared": n })))
}

async fn titles_merge_log(State(s): State<Shared>, Query(q): Query<api::titles::MergeLogQuery>) -> ApiResult<api::titles::MergeLogPage> {
    Ok(Json(api::titles::merge_log(&s.app, &q).await?))
}

async fn titles_decouple_grade(State(s): State<Shared>, Json(req): Json<api::titles::DecoupleGradeRequest>) -> ApiResult<crate::database::title_merge::DecoupleSummary> {
    Ok(Json(api::titles::decouple_grade(&s.app, &req).await?))
}

async fn titles_decouple(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<Value> {
    let title_id = api::titles::decouple(&s.app, id).await?;
    Ok(Json(json!({ "title_id": title_id })))
}

async fn titles_remove_keyword(State(s): State<Shared>, Path((id, name)): Path<(u64, String)>) -> ApiResult<Value> {
    api::titles::remove_keyword(&s.app, id, &name).await?;
    Ok(Json(json!({ "removed": true })))
}

#[derive(Debug, Default, Deserialize)]
struct DedupeQuery {
    #[serde(default)]
    full: bool,
}

async fn titles_dedupe(State(s): State<Shared>, Query(q): Query<DedupeQuery>) -> ApiResult<Value> {
    let id = api::titles::dedupe(&s.app, q.full).await?;
    Ok(Json(json!({ "job_id": id, "already_queued": id.is_none() })))
}

async fn ra_status(State(s): State<Shared>) -> ApiResult<api::ra::Status> {
    Ok(Json(api::ra::status(&s.app).await?))
}

async fn ra_refresh(State(s): State<Shared>) -> ApiResult<Value> {
    let id = api::ra::refresh(&s.app).await?;
    Ok(Json(json!({ "job_id": id, "already_queued": id.is_none() })))
}

#[derive(Deserialize)]
struct GenreFetchQuery {
    #[serde(default)]
    all: bool,
}

async fn ra_fetch_genres(State(s): State<Shared>, Query(q): Query<GenreFetchQuery>) -> ApiResult<Value> {
    let id = api::ra::fetch_genres(&s.app, q.all).await?;
    Ok(Json(json!({ "job_id": id, "already_queued": id.is_none() })))
}

async fn ra_games(State(s): State<Shared>, Query(q): Query<api::ra::GamesQuery>) -> ApiResult<api::ra::GamesPage> {
    Ok(Json(api::ra::games(&s.app, &q).await?))
}

#[derive(Deserialize)]
struct RaGameQuery {
    #[serde(default)]
    refresh: bool,
}

#[derive(serde::Deserialize)]
struct RaPatchDownloadBody {
    md5: String,
}

async fn ra_patch_download(State(s): State<Shared>, Json(b): Json<RaPatchDownloadBody>) -> ApiResult<Value> {
    let job = api::ra::download_patch(&s.app, &b.md5).await?;
    Ok(Json(json!({ "job_id": job, "already_queued": job.is_none() })))
}

async fn ra_game(State(s): State<Shared>, Path(id): Path<u32>, Query(q): Query<RaGameQuery>) -> ApiResult<api::ra::GameDetail> {
    Ok(Json(api::ra::game(&s.app, id, q.refresh).await?))
}

async fn ra_game_image(State(s): State<Shared>, Path((id, kind)): Path<(u32, String)>) -> std::result::Result<Response, ApiError> {
    image_file(api::ra::image(&s.app, id, &kind).await?).await
}

/// The badge mosaic; a game without achievements falls back to its icon.
async fn ra_game_badges(State(s): State<Shared>, Path(id): Path<u32>) -> std::result::Result<Response, ApiError> {
    match api::ra::badge_mosaic(&s.app, id).await? {
        Some(path) => image_file(path).await,
        None => image_file(api::ra::image(&s.app, id, "icon").await?).await,
    }
}

#[derive(Deserialize)]
struct BadgeQuery {
    #[serde(default)]
    locked: bool,
}

async fn ra_badge(State(s): State<Shared>, Path(name): Path<String>, Query(q): Query<BadgeQuery>) -> std::result::Result<Response, ApiError> {
    image_file(api::ra::badge(&s.app, &name, q.locked).await?).await
}

async fn ra_avatar(State(s): State<Shared>) -> std::result::Result<Response, ApiError> {
    image_file(api::ra::avatar(&s.app).await?).await
}

#[derive(Deserialize)]
struct PickQuery {
    bytes: u64,
}

async fn storage_pick_temp(State(s): State<Shared>, Query(q): Query<PickQuery>) -> ApiResult<Value> {
    let choice = api::storage::pick_temp_dir(&s.app, q.bytes).await?;
    Ok(Json(json!({ "required_bytes": q.bytes, "choice": choice, "margin": api::storage::margin(&s.app).await? })))
}

async fn db_tables(State(s): State<Shared>) -> ApiResult<Vec<api::db::TableInfo>> {
    Ok(Json(api::db::tables(&s.app).await?))
}

async fn db_rows(State(s): State<Shared>, Path(table): Path<String>, Query(q): Query<api::db::RowsQuery>) -> ApiResult<api::db::RowsPage> {
    Ok(Json(api::db::rows(&s.app, &table, &q).await?))
}

async fn db_optimize_preview(State(s): State<Shared>) -> ApiResult<api::db::OptimizePreview> {
    Ok(Json(api::db::optimize_preview(&s.app).await?))
}

async fn settings_list(State(s): State<Shared>) -> ApiResult<Vec<api::settings::SettingEntry>> {
    Ok(Json(api::settings::list(&s.app).await?))
}

/// What `scan_parallel_files` / `copy_parallel_files` would pick on auto here, and the size
/// tiers, for the settings form. Probed on the first collection directory's disk (the data
/// folder's when there is none): auto depends on the disk a job reads. `detected` is what that
/// disk was taken for, and `drives` has auto's numbers for each kind the form's drive choice
/// can name, so the placeholders follow the choice.
async fn parallel_auto(State(s): State<Shared>) -> ApiResult<Value> {
    use crate::api::parallel::{auto_limits_on, StorageKind, MAX_PER_TIER, MEDIUM_MAX_BYTES, SMALL_MAX_BYTES};
    let dirs = crate::database::directories::list(&s.app.pool, false).await.unwrap_or_default();
    let probe = dirs.iter().find(|d| !d.deleted).map(|d| std::path::PathBuf::from(&d.path)).unwrap_or_else(|| s.app.paths.root.clone());
    let (flags, _) = crate::database::settings::scan_hashes(&s.app.pool).await.map_err(api::Error::from)?;
    let shown = probe.display().to_string();
    let (detected, drives) = tokio::task::spawn_blocking(move || {
        let copy_stages = crate::rom::operation::JobStages { archive: true, ..Default::default() };
        let plain = crate::rom::hash::HashFlags::default();
        let numbers = |kind| json!({
            "scan": auto_limits_on(kind, flags, Default::default()),
            "copy": auto_limits_on(kind, plain, Default::default()),
            "copy_converting": auto_limits_on(kind, plain, copy_stages),
        });
        let drives: serde_json::Map<String, Value> = [StorageKind::Ssd, StorageKind::Hdd, StorageKind::Network].into_iter().map(|k| (k.as_str().to_string(), numbers(k))).collect();
        (StorageKind::for_path(&probe), drives)
    })
    .await
    .map_err(|e| api::Error::internal(e.to_string()))?;
    let here = &drives[detected.as_str()];
    Ok(Json(json!({
        "scan": here["scan"], "copy": here["copy"], "copy_converting": here["copy_converting"], "probed": shown,
        "detected": detected.as_str(), "drives": drives,
        "small_max_bytes": SMALL_MAX_BYTES, "medium_max_bytes": MEDIUM_MAX_BYTES, "max": MAX_PER_TIER,
        "cores": crate::rom::native_lib::physical_core_count(),
    })))
}

/// The configuration as one JSON file (`api::config_file`), named for the day it was made.
async fn config_export(State(s): State<Shared>) -> std::result::Result<Response, ApiError> {
    let file = api::config_file::export(&s.app).await?;
    let body = serde_json::to_string_pretty(&file).map_err(|e| api::Error::internal(e.to_string()))?;
    let disposition = format!("attachment; filename=\"romexplorer-config-{}.json\"", file.exported_at.get(..10).unwrap_or("export"));
    Ok(([(header::CONTENT_TYPE, "application/json; charset=utf-8".to_string()), (header::CONTENT_DISPOSITION, disposition)], body).into_response())
}

async fn config_import(State(s): State<Shared>, Json(request): Json<api::config_file::ImportRequest>) -> ApiResult<api::config_file::ImportSummary> {
    Ok(Json(api::config_file::import(&s.app, &request).await?))
}

async fn dat_platforms(State(s): State<Shared>) -> ApiResult<Value> {
    Ok(Json(api::settings::dat_platforms(&s.app).await?))
}

async fn settings_get(State(s): State<Shared>, Path(key): Path<String>) -> ApiResult<Value> {
    Ok(Json(json!({ "key": key, "value": api::settings::get(&s.app, &key).await? })))
}

async fn settings_put(State(s): State<Shared>, Path(key): Path<String>, Json(value): Json<Value>) -> ApiResult<Value> {
    api::settings::set(&s.app, &key, value.clone()).await?;
    Ok(Json(json!({ "key": key, "value": value })))
}

async fn settings_delete(State(s): State<Shared>, Path(key): Path<String>) -> ApiResult<Value> {
    Ok(Json(json!({ "key": key, "deleted": api::settings::delete(&s.app, &key).await? })))
}

async fn directories_list(State(s): State<Shared>) -> ApiResult<Vec<api::directories::DirectoryView>> {
    Ok(Json(api::directories::list(&s.app).await?))
}

async fn directories_create(State(s): State<Shared>, Json(spec): Json<api::directories::DirectorySpec>) -> ApiResult<api::directories::DirectoryView> {
    Ok(Json(api::directories::create(&s.app, &spec).await?))
}

async fn directories_get(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<api::directories::DirectoryView> {
    Ok(Json(api::directories::get(&s.app, id).await?))
}

async fn directories_move(State(s): State<Shared>, Path(id): Path<u64>, Json(b): Json<api::directories::MoveRequest>) -> ApiResult<Vec<api::directories::DirectoryView>> {
    Ok(Json(api::directories::move_directory(&s.app, id, &b).await?))
}

async fn release_games_move(State(s): State<Shared>, Path(id): Path<u64>, Json(b): Json<api::catalog::LocalGameMove>) -> ApiResult<Value> {
    api::catalog::move_local_game(&s.app, id, &b).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn release_games_order_reset(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<Value> {
    Ok(Json(json!({ "cleared": api::catalog::reset_local_game_order(&s.app, id).await? })))
}

async fn directories_update(State(s): State<Shared>, Path(id): Path<u64>, Json(spec): Json<api::directories::DirectorySpec>) -> ApiResult<api::directories::DirectoryView> {
    Ok(Json(api::directories::update(&s.app, id, &spec).await?))
}

#[derive(Deserialize)]
struct DirectoryDeleteQuery {
    #[serde(default)]
    recursive: bool,
}

/// Configuration only -- never touches the directory on disk.
async fn directories_delete(State(s): State<Shared>, Path(id): Path<u64>, Query(q): Query<DirectoryDeleteQuery>) -> ApiResult<Value> {
    let removed = api::directories::delete(&s.app, id, q.recursive).await?;
    Ok(Json(json!({ "id": id, "deleted": true, "removed": removed })))
}

#[derive(Deserialize)]
struct ResolveQuery {
    path: String,
}

async fn directories_resolve(State(s): State<Shared>, Query(q): Query<ResolveQuery>) -> ApiResult<api::directories::Effective> {
    Ok(Json(api::directories::resolve(&s.app, &q.path).await?))
}

async fn storage_view(State(s): State<Shared>) -> ApiResult<api::storage::StorageView> {
    Ok(Json(api::storage::view(&s.app).await?))
}

#[derive(Deserialize)]
struct MediaDirBody {
    role: String,
    path: String,
}

async fn storage_add_media(State(s): State<Shared>, Json(b): Json<MediaDirBody>) -> ApiResult<api::storage::StorageView> {
    Ok(Json(api::storage::add_media_dir(&s.app, &b.role, &b.path).await?))
}

async fn storage_remove_media(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<api::storage::StorageView> {
    Ok(Json(api::storage::remove_media_dir(&s.app, id).await?))
}

#[derive(Deserialize)]
struct TempDirBody {
    tier: u8,
    path: String,
}

async fn storage_set_temp(State(s): State<Shared>, Json(b): Json<TempDirBody>) -> ApiResult<api::storage::StorageView> {
    Ok(Json(api::storage::set_temp_dir(&s.app, b.tier, &b.path).await?))
}

#[derive(Deserialize)]
struct FsDirsQuery {
    path: Option<String>,
}

async fn fs_dirs(Query(q): Query<FsDirsQuery>) -> ApiResult<api::fs::DirListing> {
    Ok(Json(api::fs::list_dir(q.path.as_deref()).await?))
}

/// The OS dialog opens on the *server's* screen, so it's only offered to a browser on that
/// same machine (a loopback client); anyone else gets the in-page browser.
async fn fs_picker(ConnectInfo(addr): ConnectInfo<SocketAddr>) -> Json<Value> {
    Json(json!({ "native": api::fs::native_picker_available() && addr.ip().is_loopback() }))
}

#[derive(Deserialize)]
struct FsPickBody {
    start: Option<String>,
    /// A file instead of a folder.
    #[serde(default)]
    file: bool,
}

async fn fs_pick(ConnectInfo(addr): ConnectInfo<SocketAddr>, Json(b): Json<FsPickBody>) -> Response {
    if !addr.ip().is_loopback() {
        return plain_error(StatusCode::FORBIDDEN, "forbidden", "the native folder dialog is only available to a browser on the server's own machine");
    }
    let picked = if b.file { api::fs::pick_file_native(b.start.as_deref()).await } else { api::fs::pick_dir_native(b.start.as_deref()).await };
    match picked {
        Ok(path) => Json(json!({ "path": path })).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

async fn emulators_list(State(s): State<Shared>) -> ApiResult<Vec<crate::database::emulators::Emulator>> {
    Ok(Json(api::launch::list(&s.app).await?))
}

async fn emulators_presets() -> Json<Vec<crate::database::emulators::Preset>> {
    Json(api::launch::presets())
}

async fn emulators_save(State(s): State<Shared>, Json(b): Json<crate::database::emulators::Emulator>) -> ApiResult<crate::database::emulators::Emulator> {
    Ok(Json(api::launch::save(&s.app, &b).await?))
}

async fn emulators_delete(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<Value> {
    api::launch::delete(&s.app, id).await?;
    Ok(Json(json!({ "deleted": true })))
}

async fn launch_options(State(s): State<Shared>, Query(q): Query<api::launch::Target>) -> ApiResult<api::launch::Options> {
    Ok(Json(api::launch::options(&s.app, &q).await?))
}

/// Starts an emulator on this machine: for a client on this machine, or any client when
/// `allow_remote_launch` is on.
async fn launch_start(State(s): State<Shared>, ConnectInfo(addr): ConnectInfo<SocketAddr>, Json(b): Json<api::launch::Target>) -> Response {
    match api::launch::remote_launch_allowed(&s.app).await {
        Ok(allowed) if !allowed && !addr.ip().is_loopback() => {
            return plain_error(StatusCode::FORBIDDEN, "forbidden", "launching is only allowed from this machine (Config > Settings: allow_remote_launch)");
        }
        Err(e) => return ApiError(e).into_response(),
        _ => {}
    }
    match api::launch::launch(&s.app, &b).await {
        Ok(done) => Json(done).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

async fn storage_remove_temp(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<api::storage::StorageView> {
    Ok(Json(api::storage::remove_temp_dir(&s.app, id).await?))
}

async fn jobs_list(State(s): State<Shared>, Query(q): Query<api::jobs::ListQuery>) -> ApiResult<api::jobs::JobsPage> {
    Ok(Json(api::jobs::list(&s.app, &q).await?))
}

/// Any job for an admin. For an account with upload, only a new-files scan: registering what
/// it copied onto the share, which never changes a file already known.
#[derive(Debug, Default, Deserialize)]
struct EnqueueQuery {
    /// Wait its turn instead of starting alongside the other lanes (`api::jobs::enqueue_in_turn`).
    #[serde(default)]
    in_turn: bool,
}

async fn jobs_enqueue(State(s): State<Shared>, principal: Principal, Query(q): Query<EnqueueQuery>, Json(req): Json<api::jobs::JobRequest>) -> std::result::Result<(StatusCode, Json<Value>), ApiError> {
    let upload_scan = matches!(&req, api::jobs::JobRequest::FileScan(f) if f.mode == romexplorer_types::jobs::FileScanMode::NewOnly);
    if !principal.is_admin() && !(upload_scan && principal.can_upload()) {
        return Err(Error::forbidden("this account may only queue a new-files scan, to register what it uploaded").into());
    }
    let id = if q.in_turn { api::jobs::enqueue_in_turn(&s.app, &req).await? } else { api::jobs::enqueue(&s.app, &req).await? };
    Ok((StatusCode::CREATED, Json(json!({ "id": id, "job_type": req.job_type().name(), "in_turn": q.in_turn }))))
}

async fn jobs_clear(State(s): State<Shared>, Json(request): Json<api::jobs::ClearRequest>) -> ApiResult<api::jobs::ClearResult> {
    Ok(Json(api::jobs::clear(&s.app, &request).await?))
}

async fn jobs_counts(State(s): State<Shared>) -> ApiResult<Vec<api::jobs::StatusCount>> {
    Ok(Json(api::jobs::counts(&s.app).await?))
}

async fn jobs_get(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<crate::database::jobs::JobRow> {
    Ok(Json(api::jobs::get(&s.app, id).await?))
}

async fn jobs_run_next(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<api::jobs::RunNextPosition> {
    Ok(Json(api::jobs::run_next_one(&s.app, id).await?))
}

async fn jobs_run_next_many(State(s): State<Shared>, Json(request): Json<api::jobs::RunNextRequest>) -> ApiResult<api::jobs::RunNextResult> {
    Ok(Json(api::jobs::run_next(&s.app, &request.ids).await?))
}

async fn jobs_retry(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<crate::database::jobs::JobRow> {
    Ok(Json(api::jobs::retry(&s.app, id).await?))
}

async fn schedules_list(State(s): State<Shared>) -> ApiResult<Vec<crate::database::schedules::Schedule>> {
    Ok(Json(api::schedules::list(&s.app).await?))
}

async fn schedules_create(State(s): State<Shared>, Json(spec): Json<crate::database::schedules::ScheduleSpec>) -> std::result::Result<(StatusCode, Json<crate::database::schedules::Schedule>), ApiError> {
    Ok((StatusCode::CREATED, Json(api::schedules::create(&s.app, &spec).await?)))
}

async fn schedules_get(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<crate::database::schedules::Schedule> {
    Ok(Json(api::schedules::get(&s.app, id).await?))
}

async fn schedules_update(State(s): State<Shared>, Path(id): Path<u64>, Json(spec): Json<crate::database::schedules::ScheduleSpec>) -> ApiResult<crate::database::schedules::Schedule> {
    Ok(Json(api::schedules::update(&s.app, id, &spec).await?))
}

async fn schedules_delete(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<Value> {
    api::schedules::delete(&s.app, id).await?;
    Ok(Json(json!({ "id": id, "deleted": true })))
}

async fn schedules_run(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<Value> {
    let job = api::schedules::run_now(&s.app, id).await?;
    Ok(Json(json!({ "id": id, "job_id": job, "already_queued": job.is_none() })))
}

/// 202: a pending job is cancelled at once; a running one stops within a worker tick.
async fn jobs_cancel(State(s): State<Shared>, Path(id): Path<u64>) -> std::result::Result<(StatusCode, Json<Value>), ApiError> {
    let (job, outcome) = api::jobs::cancel(&s.app, id).await?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "job": job, "outcome": outcome }))))
}

async fn catalog_platforms(State(s): State<Shared>) -> ApiResult<Vec<api::catalog::PlatformInfo>> {
    Ok(Json(api::catalog::platforms(&s.app).await?))
}

async fn catalog_facets(State(s): State<Shared>) -> ApiResult<api::catalog::Facets> {
    Ok(Json(api::catalog::facets(&s.app).await?))
}

async fn catalog_titles(State(s): State<Shared>, Query(q): Query<api::catalog::ListQuery>) -> ApiResult<api::catalog::Page<api::catalog::TitleRow>> {
    Ok(Json(api::catalog::titles(&s.app, &q).await?))
}

#[derive(Deserialize)]
struct TitleRowIdsBody {
    #[serde(default)]
    row_ids: Vec<u64>,
}

/// What the Titles list's row numbers (`TitleRow::row_id`) are: each one's title and row key,
/// for the actions on a selection that work on whole titles (keyword tags, merging).
async fn catalog_title_rows(State(s): State<Shared>, Json(b): Json<TitleRowIdsBody>) -> ApiResult<Vec<romexplorer_types::copy::TitleRowRef>> {
    if b.row_ids.len() > 5000 {
        return Err(api::Error::invalid("at most 5000 at once").into());
    }
    Ok(Json(crate::database::release_buckets::decode_row_ids(&s.app.pool, &b.row_ids).await.map_err(api::Error::from)?))
}

async fn catalog_crossplatform(State(s): State<Shared>, Query(q): Query<api::catalog::ListQuery>) -> ApiResult<api::catalog::Page<api::catalog::CrossRow>> {
    Ok(Json(api::catalog::crossplatform(&s.app, &q).await?))
}

async fn catalog_crossplatform_platforms(State(s): State<Shared>) -> ApiResult<Vec<u16>> {
    Ok(Json(api::catalog::crossplatform_platforms(&s.app).await?))
}

async fn catalog_definitive(State(s): State<Shared>, Query(q): Query<api::catalog::ListQuery>) -> ApiResult<api::catalog::Page<api::definitive::DefinitiveRow>> {
    Ok(Json(api::definitive::list(&s.app, &q).await?))
}

async fn catalog_definitive_family(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<api::definitive::FamilyDetail> {
    Ok(Json(api::definitive::detail(&s.app, id).await?))
}

async fn definitive_order_move(State(s): State<Shared>, Path(id): Path<u64>, Json(b): Json<api::definitive::MoveRequest>) -> ApiResult<api::definitive::FamilyDetail> {
    Ok(Json(api::definitive::move_member(&s.app, id, &b).await?))
}

async fn definitive_ask(State(s): State<Shared>, Path(id): Path<u64>, body: Option<Json<api::definitive::AskRequest>>) -> ApiResult<api::definitive::AskAnswer> {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    Ok(Json(api::definitive::ask(&s.app, id, &req).await?))
}

async fn title_versions(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<api::versions::VersionInfo> {
    Ok(Json(api::versions::info(&s.app, id).await?))
}

async fn title_versions_add(State(s): State<Shared>, Path(id): Path<u64>, Json(req): Json<api::versions::RelationRequest>) -> ApiResult<api::versions::VersionInfo> {
    Ok(Json(api::versions::add_relation(&s.app, id, &req).await?))
}

async fn title_versions_delete(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<serde_json::Value> {
    api::versions::delete_relation(&s.app, id).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn version_group_pick(State(s): State<Shared>, Path(key): Path<String>, Json(req): Json<api::versions::PickRequest>) -> ApiResult<serde_json::Value> {
    api::versions::pick(&s.app, &key, &req).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn version_group_reroll(State(s): State<Shared>, Path(key): Path<String>) -> ApiResult<serde_json::Value> {
    api::versions::reroll(&s.app, &key).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn definitive_batch_estimate(State(s): State<Shared>, Json(req): Json<romexplorer_types::jobs::AiBatchRateRequest>) -> ApiResult<api::definitive::BatchEstimate> {
    Ok(Json(api::definitive::batch_estimate(&s.app, &req).await?))
}

async fn definitive_paste(State(s): State<Shared>, Path(id): Path<u64>, Json(req): Json<api::definitive::PasteRequest>) -> ApiResult<api::definitive::AskAnswer> {
    Ok(Json(api::definitive::paste(&s.app, id, &req).await?))
}

async fn definitive_order_reset(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<api::definitive::FamilyDetail> {
    Ok(Json(api::definitive::reset_order(&s.app, id).await?))
}

async fn title_basegame_set(State(s): State<Shared>, Path(id): Path<u64>, Json(b): Json<api::definitive::BasegameRequest>) -> ApiResult<api::catalog::TitleDetail> {
    api::definitive::set_basegame(&s.app, id, &b).await?;
    Ok(Json(api::catalog::title(&s.app, id).await?))
}

async fn title_basegame_reset(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<api::catalog::TitleDetail> {
    api::definitive::reset_basegame(&s.app, id).await?;
    Ok(Json(api::catalog::title(&s.app, id).await?))
}

async fn catalog_title(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<api::catalog::TitleDetail> {
    Ok(Json(api::catalog::title(&s.app, id).await?))
}

async fn catalog_releases(State(s): State<Shared>, Query(q): Query<api::catalog::ListQuery>) -> ApiResult<api::catalog::Page<api::catalog::ReleaseRow>> {
    Ok(Json(api::catalog::releases(&s.app, &q).await?))
}

async fn catalog_bios(State(s): State<Shared>, Query(q): Query<api::bios::BiosQuery>) -> ApiResult<api::catalog::Page<api::bios::BiosRow>> {
    Ok(Json(api::bios::list(&s.app, &q).await?))
}

async fn catalog_patches(State(s): State<Shared>, Query(q): Query<api::patches::PatchQuery>) -> ApiResult<api::catalog::Page<api::patches::PatchRow>> {
    Ok(Json(api::patches::list(&s.app, &q).await?))
}

async fn catalog_release(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<api::catalog::ReleaseDetail> {
    Ok(Json(api::catalog::release(&s.app, id).await?))
}

async fn catalog_roms(State(s): State<Shared>, Query(q): Query<api::catalog::ListQuery>) -> ApiResult<api::catalog::Page<api::catalog::RomRow>> {
    Ok(Json(api::catalog::roms(&s.app, &q).await?))
}

async fn catalog_rom(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<api::catalog::RomPage> {
    Ok(Json(api::catalog::rom(&s.app, id).await?))
}

async fn catalog_contents(State(s): State<Shared>, Query(q): Query<api::catalog::ListQuery>) -> ApiResult<api::catalog::Page<api::catalog::ContentListRow>> {
    Ok(Json(api::catalog::contents(&s.app, &q).await?))
}

async fn catalog_content(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<api::catalog::ContentDetail> {
    Ok(Json(api::catalog::content(&s.app, id).await?))
}

async fn catalog_games(State(s): State<Shared>, Query(q): Query<api::catalog::ListQuery>) -> ApiResult<api::catalog::Page<api::catalog::GameRow>> {
    Ok(Json(api::catalog::games(&s.app, &q).await?))
}

async fn catalog_files(State(s): State<Shared>, Query(q): Query<api::catalog::ListQuery>) -> ApiResult<api::catalog::Page<api::catalog::FileRow>> {
    Ok(Json(api::catalog::files(&s.app, &q).await?))
}

async fn catalog_file(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<api::catalog::FileDetail> {
    Ok(Json(api::catalog::file(&s.app, id).await?))
}

#[derive(Deserialize)]
struct LookupQuery {
    algorithm: Option<String>,
    value: String,
}

async fn catalog_lookup(State(s): State<Shared>, Query(q): Query<LookupQuery>) -> ApiResult<Vec<api::catalog::HashMatchRow>> {
    Ok(Json(api::catalog::lookup_hash(&s.app, q.algorithm.as_deref(), &q.value).await?))
}

async fn providers_list(State(s): State<Shared>) -> ApiResult<Vec<api::providers::ProviderInfo>> {
    Ok(Json(api::providers::list(&s.app).await?))
}

#[derive(Deserialize)]
struct SearchQuery {
    name: String,
    platform: Option<u16>,
    /// `true`: search the locally cached catalog instead of the provider's online search.
    #[serde(default)]
    local: bool,
}

async fn providers_search(State(s): State<Shared>, Path(provider): Path<String>, Query(q): Query<SearchQuery>) -> ApiResult<Vec<api::links::SearchHit>> {
    if q.local {
        return Ok(Json(api::links::search_local(&s.app, &provider, &q.name, q.platform).await?));
    }
    Ok(Json(api::links::search(&s.app, &provider, &q.name, q.platform).await?))
}

#[derive(Deserialize)]
struct ResolveUrlBody {
    url: String,
}

async fn providers_credentials(State(s): State<Shared>, Query(q): Query<std::collections::HashMap<String, String>>) -> ApiResult<Vec<api::providers::CredentialView>> {
    Ok(Json(api::providers::credentials(&s.app, q.get("scope").map(String::as_str))?))
}

async fn providers_credentials_put(State(s): State<Shared>, Json(values): Json<std::collections::BTreeMap<String, String>>) -> ApiResult<Vec<api::providers::CredentialView>> {
    api::providers::set_credentials(&s.app, &values)?;
    Ok(Json(api::providers::credentials(&s.app, None)?))
}

async fn providers_resolve_url(State(s): State<Shared>, Json(b): Json<ResolveUrlBody>) -> ApiResult<api::links::ResolvedUrl> {
    Ok(Json(api::links::resolve_url(&s.app, &b.url).await?))
}

async fn title_refresh(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<api::links::RefreshSummary> {
    Ok(Json(api::links::refresh_title(&s.app, id).await?))
}

async fn release_refresh(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<api::links::RefreshSummary> {
    Ok(Json(api::links::refresh_release(&s.app, id).await?))
}

async fn title_link(State(s): State<Shared>, Path(id): Path<u64>, Json(spec): Json<api::links::LinkSpec>) -> ApiResult<api::catalog::TitleDetail> {
    Ok(Json(api::links::link_title(&s.app, id, &spec).await?))
}

async fn title_unlink(State(s): State<Shared>, Path((id, provider)): Path<(u64, String)>) -> ApiResult<api::catalog::TitleDetail> {
    Ok(Json(api::links::unlink_title(&s.app, id, &provider).await?))
}

async fn release_link(State(s): State<Shared>, Path(id): Path<u64>, Json(spec): Json<api::links::LinkSpec>) -> ApiResult<api::catalog::ReleaseDetail> {
    Ok(Json(api::links::link_release(&s.app, id, &spec).await?))
}

async fn release_title_set(State(s): State<Shared>, Path(id): Path<u64>, Json(b): Json<api::titles::ReleaseTitle>) -> ApiResult<api::catalog::ReleaseDetail> {
    api::titles::link_release(&s.app, id, &b).await?;
    Ok(Json(api::catalog::release(&s.app, id).await?))
}

async fn base_links(State(s): State<Shared>, Query(q): Query<api::bases::LinkQuery>) -> ApiResult<api::catalog::Page<api::bases::LinkRow>> {
    Ok(Json(api::bases::list(&s.app, &q).await?))
}

async fn base_links_bulk(State(s): State<Shared>, Json(b): Json<api::bases::BulkLinks>) -> ApiResult<api::bases::BulkDone> {
    Ok(Json(api::bases::bulk(&s.app, &b).await?))
}

async fn release_base_set(State(s): State<Shared>, Path(id): Path<u64>, Json(b): Json<api::bases::BaseRequest>) -> ApiResult<api::catalog::ReleaseDetail> {
    api::bases::set(&s.app, id, &b).await?;
    Ok(Json(api::catalog::release(&s.app, id).await?))
}

async fn release_language_set(State(s): State<Shared>, Path(id): Path<u64>, Json(b): Json<api::bases::LanguageRequest>) -> ApiResult<api::catalog::ReleaseDetail> {
    api::bases::language(&s.app, id, &b).await?;
    Ok(Json(api::catalog::release(&s.app, id).await?))
}

async fn title_locales_set(State(s): State<Shared>, Path(id): Path<u64>, Json(b): Json<api::titles::LocalesEdit>) -> ApiResult<api::catalog::TitleDetail> {
    api::titles::set_locales(&s.app, id, &b).await?;
    Ok(Json(api::catalog::title(&s.app, id).await?))
}

async fn title_locales_unlock(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<Value> {
    Ok(Json(json!({ "job_id": api::titles::unlock_locales(&s.app, id).await? })))
}

async fn releases_display_set(State(s): State<Shared>, Json(b): Json<api::display::DisplaySet>) -> ApiResult<Value> {
    Ok(Json(json!({ "changed": api::display::set(&s.app, &b).await? })))
}

async fn releases_split(State(s): State<Shared>, Json(b): Json<api::titles::ReleasesSplit>) -> ApiResult<Value> {
    Ok(Json(json!({ "title_id": api::titles::split_releases(&s.app, &b).await? })))
}

async fn releases_move(State(s): State<Shared>, Json(b): Json<api::titles::ReleasesMove>) -> ApiResult<Value> {
    Ok(Json(json!({ "moved": api::titles::move_releases(&s.app, &b).await?, "title_id": b.title_id })))
}

async fn release_split(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<Value> {
    Ok(Json(json!({ "title_id": api::titles::split_release(&s.app, id).await? })))
}

async fn release_title_clear(State(s): State<Shared>, Path(id): Path<u64>) -> ApiResult<api::catalog::ReleaseDetail> {
    api::titles::unlink_release(&s.app, id).await?;
    Ok(Json(api::catalog::release(&s.app, id).await?))
}

async fn release_unlink(State(s): State<Shared>, Path((id, provider)): Path<(u64, String)>) -> ApiResult<api::catalog::ReleaseDetail> {
    Ok(Json(api::links::unlink_release(&s.app, id, &provider).await?))
}

async fn dats_sources(State(s): State<Shared>) -> ApiResult<Vec<api::dats::SourceInfo>> {
    Ok(Json(api::dats::sources(&s.app).await?))
}

#[derive(Deserialize)]
struct LimitQuery {
    limit: Option<u32>,
}

async fn dats_imports(State(s): State<Shared>, Query(q): Query<LimitQuery>) -> ApiResult<Vec<api::dats::ImportRow>> {
    Ok(Json(api::dats::imports(&s.app, q.limit.unwrap_or(100)).await?))
}

async fn dats_files(State(s): State<Shared>) -> ApiResult<Vec<api::dats::DatFile>> {
    Ok(Json(api::dats::files(&s.app).await?))
}

/// Streams a file from disk as a download; `delete_after` for a scratch file nobody else
/// needs once it is open (unlinking an open file leaves the open handle readable on Unix;
/// where it fails, `api::dats` clears the leftover a day later).
async fn stream_download(path: &std::path::Path, name: &str, content_type: &str, delete_after: bool) -> std::result::Result<Response, ApiError> {
    let file = tokio::fs::File::open(path).await.map_err(|e| Error::internal(format!("open {}: {e}", path.display())))?;
    let len = file.metadata().await.map(|m| m.len()).ok();
    if delete_after {
        let _ = std::fs::remove_file(path);
    }
    let body = axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(file));
    let ascii: String = name.chars().map(|c| if c.is_ascii() && c != '"' && !c.is_ascii_control() { c } else { '_' }).collect();
    let mut headers = vec![
        (header::CONTENT_TYPE, content_type.to_string()),
        (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{}", urlencoding_component(name))),
    ];
    if let Some(len) = len {
        headers.push((header::CONTENT_LENGTH, len.to_string()));
    }
    let mut response = body.into_response();
    for (k, v) in headers {
        if let Ok(v) = header::HeaderValue::from_str(&v) {
            response.headers_mut().insert(k, v);
        }
    }
    Ok(response)
}

/// RFC 5987 percent-encoding for `filename*`.
fn urlencoding_component(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// One downloaded DAT file, as is. `application/zip` for a pack (already compressed: the
/// predicate skips it); a loose `.dat`/`.xml`/`.json` is text and compresses well, so it goes
/// as such.
async fn dats_file(State(s): State<Shared>, Path((source, name)): Path<(String, String)>) -> std::result::Result<Response, ApiError> {
    let path = api::dats::file(&s.app, &source, &name).await?;
    let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(name);
    let content_type = match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("zip") => "application/zip",
        Some("json") => "application/json",
        _ => "application/xml",
    };
    stream_download(&path, &file_name, content_type, false).await
}

#[derive(Deserialize)]
struct BundleQuery {
    source: Option<String>,
}

/// Every downloaded DAT file (or one source's) in one zip, for keeping or for another
/// install's `dats_upload`.
async fn dats_bundle(State(s): State<Shared>, Query(q): Query<BundleQuery>) -> std::result::Result<Response, ApiError> {
    let source = q.source.as_deref().filter(|s| !s.is_empty());
    let path = api::dats::bundle_temp(&s.app, source).await?;
    stream_download(&path, &api::dats::bundle_name(source), "application/zip", true).await
}

async fn dats_export(State(s): State<Shared>, Json(req): Json<api::dats::ExportRequest>) -> ApiResult<api::dats::Exported> {
    Ok(Json(api::dats::export(&s.app, &req).await?))
}

/// A DAT file for one source (field `source`), or a bundle from `dats_bundle`, into the DAT
/// folder. Streamed to disk like a backup upload: a bundle of every source is hundreds of MB.
async fn dats_upload(State(s): State<Shared>, mut form: Multipart) -> ApiResult<api::dats::Uploaded> {
    use tokio::io::AsyncWriteExt;
    let bad = |e: axum::extract::multipart::MultipartError| Error::invalid(format!("upload: {e}"));
    let mut source: Option<String> = None;
    while let Some(mut field) = form.next_field().await.map_err(bad)? {
        match field.name() {
            Some("source") => source = Some(field.text().await.map_err(bad)?),
            Some("file") => {
                let original = field.file_name().unwrap_or("upload.dat").to_string();
                let part = api::dats::upload_part_path(&s.app)?;
                // Removed however this ends, a client that hangs up mid-upload included (the
                // handler is dropped then); accepting it renames it away first.
                let _leftover = RemoveOnDrop(part.clone());
                let received = async {
                    let mut out = tokio::fs::File::create(&part).await.map_err(|e| Error::internal(format!("{}: {e}", part.display())))?;
                    while let Some(chunk) = field.chunk().await.map_err(bad)? {
                        out.write_all(&chunk).await.map_err(|e| Error::internal(format!("{}: {e}", part.display())))?;
                    }
                    out.flush().await.map_err(|e| Error::internal(e.to_string()))
                }
                .await;
                if let Err(e) = received {
                    let _ = std::fs::remove_file(&part);
                    return Err(e.into());
                }
                // `source` must come before `file` in the form, which is what every client
                // here sends (and what a browser sends for fields appended in that order).
                return Ok(Json(api::dats::accept_upload(&s.app, &part, &original, source.as_deref()).await?));
            }
            _ => {}
        }
    }
    Err(Error::invalid("file is required").into())
}

// ---------------------------------------------------------------------------
// router / serve
// ---------------------------------------------------------------------------

/// What a *client* serves when the user turns sharing on -- four read-only routes and
/// nothing else.
///
/// Deliberately **not** [`router`]. That is the full control surface: jobs, scan, launch,
/// directories, storage, settings, media upload, `POST /server/shutdown`. A client shares so
/// that peers can mirror its ownership tables, and it never takes work from them, so a peer
/// holding the token has no business enqueueing jobs on this machine, starting an emulator
/// through `/api/v1/launch`, or shutting the app down. Exposing all of that to gain two read
/// endpoints is the wrong trade, and "we'll just document that peers shouldn't call those" is
/// not a boundary.
///
/// The catalogue is not offered either ([`share_manifest`] passes `with_catalog = false`): a
/// share says what it holds, and the shared catalogue is the main server's to hand out.
///
/// Same `ServerState` and the same `guard` -- one bearer token and one IP allowlist, not a
/// second authentication path.
/// Compresses responses for callers that ask for it.
///
/// The sync export is JSON by the hundred megabytes -- a full catalogue pull measured 130 MB
/// on the wire -- and it compresses about 7:1, so this is the single biggest saving available
/// on a LAN sync. Per *response* rather than per stream, which costs nothing: measured on this
/// repo's own export, compressing each ~300 KB page separately came out 0.8% **smaller** than
/// one stream over the whole table, because each page gets entropy tables fitted to its own
/// rows. Paging, the primary-key cursor, per-page retry and the progress bar are all untouched.
///
/// `DefaultPredicate` skips tiny bodies, gRPC, `image/*` and SSE -- but *only* `image/*` among
/// the already-compressed types, and this server also serves zip, video, audio and PDF. So it
/// is composed with [`skip_already_compressed`] below.
fn compression() -> tower_http::compression::CompressionLayer<CompressionPredicate> {
    use tower_http::compression::predicate::{DefaultPredicate, Predicate};

    tower_http::compression::CompressionLayer::new()
        .zstd(true)
        .gzip(true)
        .compress_when(DefaultPredicate::new().and(skip_already_compressed as ContentTypeRule))
}

/// A bare `fn` rather than a closure so the layer's type can be named: `Predicate` is
/// implemented for any `Fn(StatusCode, Version, &HeaderMap, &Extensions) -> bool + Clone`.
type ContentTypeRule = fn(StatusCode, Version, &HeaderMap, &Extensions) -> bool;

type CompressionPredicate =
    tower_http::compression::predicate::And<tower_http::compression::predicate::DefaultPredicate, ContentTypeRule>;

fn skip_already_compressed(_: StatusCode, _: Version, headers: &HeaderMap, _: &Extensions) -> bool {
    !headers.get(header::CONTENT_TYPE).and_then(|value| value.to_str().ok()).is_some_and(is_already_compressed)
}

/// Whether a body of this content type is already compressed, making a second pass over it
/// pure cost -- the output is still correct, just paid for twice.
///
/// What this catches, in the order it matters:
///
/// - `application/zip` from `backup_download`. The worst case by far: backups run to
///   gigabytes, their members are already zstd-3 (`api::backup`), and being *streamed* they
///   defeat even the size check -- `SizeAbove` reads the body's exact size hint, falls back to
///   `Content-Length`, and votes *compress* when it has neither, which a `ReaderStream` never
///   does.
/// - video and the compressed audio formats from `api::media`. Not hypothetical: `MEDIA_TYPES`
///   includes `video` and `bgm`, and a media file can be 512 MB.
/// - `application/pdf` (Flate inside) and `application/octet-stream` (unknown, so assume the
///   worst) from the same route's `mime_for`.
/// - `font/woff`, `font/woff2`: WOFF2 is Brotli inside. Nothing under `assets/gui_web` is a
///   font today, but that is a build-output directory.
///
/// Deliberately *not* here: `audio/wav` is PCM and compresses genuinely, and the 4 MB
/// uncompressed `.wasm` plus the JS, CSS and 73 SVG icons this server serves are all real
/// wins. That is why this is a list and not "skip anything binary". `image/*` is left to
/// `DefaultPredicate`, which already has it.
fn is_already_compressed(content_type: &str) -> bool {
    // Drop any `; charset=...` parameter and normalise, rather than matching the raw header
    // the way tower-http's own `NotForContentType` does.
    let essence = content_type.split(';').next().unwrap_or_default().trim().to_ascii_lowercase();

    essence.starts_with("video/")
        || essence.starts_with("font/woff")
        || matches!(
            essence.as_str(),
            "application/zip"
                | "application/pdf"
                | "application/octet-stream"
                | "audio/mpeg"
                | "audio/ogg"
                | "audio/flac"
                // The audio library's other formats. All already compressed (lossy, or
                // lossless with their own entropy coding); `audio/wav`, `audio/aiff` and
                // `audio/x-dsd` are raw samples and stay compressible.
                | "audio/opus"
                | "audio/mp4"
                | "audio/aac"
                | "audio/x-ms-wma"
                | "audio/x-ape"
                | "audio/x-wavpack"
        )
}

pub fn share_router(state: Shared) -> Router {
    Router::new()
        // Token-exempt in `guard`, which is what lets a client tell "wrong token" from
        // "nothing is listening there" (`api::sources::check`).
        .route("/api/v1/ping", get(ping))
        .route("/api/v1/auth/check", get(auth_check))
        .route("/api/v1/auth/login", post(auth_login))
        .route("/api/v1/auth/logout", post(auth_logout))
        .route("/api/v1/auth/me", get(auth_me))
        .route("/api/v1/sync/manifest", get(share_manifest))
        .route("/api/v1/sync/export/{table}", get(share_export))
        .route("/api/v1/sync/deleted/{table}", get(share_deleted))
        .layer(compression())
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

pub fn router(state: Shared) -> Router {
    Router::new()
        .route("/", get(console))
        .route("/index.html", get(console))
        .route("/player.js", get(player_js))
        .route("/api/v1/ping", get(ping))
        .route("/icons/{file}", get(icon_file))
        .route("/app", get(gui_web))
        .route("/app/", get(gui_web))
        .route("/app/{*path}", get(gui_web))
        .route("/api/v1/auth/check", get(auth_check))
        .route("/api/v1/auth/login", post(auth_login))
        .route("/api/v1/auth/logout", post(auth_logout))
        .route("/api/v1/auth/me", get(auth_me))
        .route("/api/v1/auth/password", axum::routing::put(auth_password))
        .route("/api/v1/auth/devices", get(auth_devices))
        .route("/api/v1/auth/devices/{id}", delete(auth_device_revoke))
        .route("/api/v1/users", get(users_list).post(users_create))
        .route("/api/v1/users/{id}", axum::routing::put(users_update).delete(users_delete))
        .route("/api/v1/users/{id}/devices", get(user_devices))
        .route("/api/v1/users/{id}/devices/{device_id}", delete(user_device_revoke))
        .route("/api/v1/audio", get(audio_list))
        .route("/api/v1/audio/tags", get(audio_tags).post(audio_tag_add))
        .route("/api/v1/audio/tags/remove", post(audio_tag_remove))
        .route("/api/v1/audio/playlist", get(audio_playlist))
        .route("/api/v1/audio/facets", get(audio_facets))
        .route("/api/v1/audio/{id}/file", get(audio_file))
        .route("/api/v1/audio/remote/{id}/file", get(audio_remote))
        .route("/api/v1/media/strip", get(media_strip))
        .route("/api/v1/media/reuse_prompt", get(media_reuse_prompt))
        .route("/api/v1/ai", get(ai_overview))
        .route("/api/v1/ai/clear", post(ai_clear))
        .route("/api/v1/ai/export", get(ai_export))
        .route("/api/v1/ai/import", post(ai_import))
        .route("/api/v1/ai/test/{provider}", post(ai_test))
        .route("/api/v1/ai/answers/{id}/pin", axum::routing::put(ai_pin))
        .route("/api/v1/media/remote/{id}", get(media_remote))
        .route("/api/v1/server", get(server_info))
        .route("/api/v1/server/ports", axum::routing::put(server_ports_put))
        .route("/api/v1/server/database_cache", get(database_cache).put(database_cache_put))
        .route("/api/v1/about", get(about))
        .route("/api/v1/server/shutdown", post(server_shutdown))
        .route("/api/v1/server/restart", post(server_restart))
        .route("/api/v1/status", get(status))
        .route("/api/v1/vocabulary", get(vocabulary))
        .route("/api/v1/i18n", get(i18n_languages))
        .route("/api/v1/i18n/{lang}", get(i18n_strings))
        .route("/api/v1/db/tables", get(db_tables))
        .route("/api/v1/db/tables/{table}/rows", get(db_rows))
        .route("/api/v1/db/optimize/preview", get(db_optimize_preview))
        .route("/api/v1/settings", get(settings_list))
        .route("/api/v1/config/export", get(config_export))
        .route("/api/v1/config/import", post(config_import))
        .route("/api/v1/parallel/auto", get(parallel_auto))
        .route("/api/v1/dat_platforms", get(dat_platforms))
        .route("/api/v1/settings/{key}", get(settings_get).put(settings_put).delete(settings_delete))
        .route("/api/v1/directories", get(directories_list).post(directories_create))
        .route("/api/v1/directories/resolve", get(directories_resolve))
        .route("/api/v1/directories/{id}", get(directories_get).put(directories_update).delete(directories_delete))
        .route("/api/v1/directories/{id}/move", post(directories_move))
        .route("/api/v1/catalog/releases/{id}/games/move", post(release_games_move))
        .route("/api/v1/catalog/releases/{id}/games/order", delete(release_games_order_reset))
        .route("/api/v1/storage", get(storage_view))
        .route("/api/v1/storage/media", post(storage_add_media))
        .route("/api/v1/storage/media/{id}", delete(storage_remove_media))
        .route("/api/v1/storage/temp", post(storage_set_temp))
        .route("/api/v1/storage/temp/pick", get(storage_pick_temp))
        .route("/api/v1/storage/temp/{id}", delete(storage_remove_temp))
        .route("/api/v1/fs/dirs", get(fs_dirs))
        .route("/api/v1/fs/picker", get(fs_picker))
        .route("/api/v1/fs/pick", post(fs_pick))
        .route("/api/v1/emulators", get(emulators_list).post(emulators_save))
        .route("/api/v1/emulators/presets", get(emulators_presets))
        .route("/api/v1/emulators/{id}", delete(emulators_delete))
        .route("/api/v1/launch/options", get(launch_options))
        .route("/api/v1/launch", post(launch_start))
        .route("/api/v1/sets", get(sets_list).post(sets_create))
        .route("/api/v1/sets/export", get(sets_export))
        .route("/api/v1/sets/import", post(sets_import))
        .route("/api/v1/sets/{id}", get(sets_get).put(sets_update).delete(sets_delete))
        .route("/api/v1/sets/{id}/releases", get(sets_releases).post(sets_add_release))
        .route("/api/v1/sets/{id}/releases/{release_id}", delete(sets_remove_release))
        .route("/api/v1/sets/{id}/remove", post(sets_remove_releases))
        .route("/api/v1/sets/{id}/reorder", post(sets_reorder))
        .route("/api/v1/ra/status", get(ra_status))
        .route("/api/v1/ra/refresh", post(ra_refresh))
        .route("/api/v1/ra/genres/fetch", post(ra_fetch_genres))
        .route("/api/v1/ra/avatar", get(ra_avatar))
        .route("/api/v1/ra/games", get(ra_games))
        .route("/api/v1/ra/games/{id}", get(ra_game))
        .route("/api/v1/ra/patches/download", post(ra_patch_download))
        .route("/api/v1/ra/games/{id}/badges", get(ra_game_badges))
        .route("/api/v1/ra/games/{id}/image/{kind}", get(ra_game_image))
        .route("/api/v1/ra/badges/{name}", get(ra_badge))
        .route("/api/v1/media/manual", post(media_manual))
        .route("/api/v1/media/upload", post(media_upload).layer(DefaultBodyLimit::max(api::media::MAX_MEDIA_BYTES + 64 * 1024)))
        .route("/api/v1/media/{id}", get(media_file).delete(media_remove))
        .route("/api/v1/media/{id}/refetch", post(media_refetch))
        .route("/api/v1/media/fetch", post(media_fetch))
        .route("/api/v1/backup", get(backup_list).post(backup_create))
        .route("/api/v1/backup/upload", post(backup_upload).layer(DefaultBodyLimit::disable()))
        .route("/api/v1/backup/{name}", get(backup_download).delete(backup_delete))
        .route("/api/v1/backup/{name}/restore", post(backup_restore))
        .route("/api/v1/cache", get(cache_list).post(cache_create))
        .route("/api/v1/cache/{name}", get(cache_download).delete(cache_delete))
        .route("/api/v1/cache/{name}/import", post(cache_import))
        .route("/api/v1/sync/manifest", get(sync_manifest))
        .route("/api/v1/sync/export/{table}", get(sync_export))
        .route("/api/v1/sync/deleted/{table}", get(sync_deleted))
        .route("/api/v1/mode", get(mode_get).put(mode_set))
        .route("/api/v1/sources", get(sources_list).post(sources_create))
        .route("/api/v1/sources/{id}/roots", get(source_roots))
        .route("/api/v1/sources/{id}/mounts", get(source_mounts).post(source_mount_create))
        .route("/api/v1/mounts/{id}", delete(mount_delete))
        .route("/api/v1/mounts/{id}/check", post(mount_check))
        .route("/api/v1/copy", post(copy_start))
        .route("/api/v1/copy/preview", post(copy_preview))
        .route("/api/v1/copy/defaults", get(copy_defaults).put(copy_defaults_put))
        .route("/api/v1/integrity", get(integrity_preview).post(integrity_start))
        .route("/api/v1/sheets/preview", post(sheets_preview))
        .route("/api/v1/sheets", post(sheets_create))
        .route("/api/v1/sheets/verify", post(sheets_verify))
        .route("/api/v1/sheets/m3u/verify", post(m3u_verify))
        .route("/api/v1/sheets/files/{id}/verify", post(sheet_file_verify))
        .route("/api/v1/sheets/files/{id}/fix", post(sheet_file_fix))
        .route("/api/v1/naming/folders", get(naming_folders).put(naming_folders_put))
        .route("/api/v1/naming/folders/reset", post(naming_folders_reset))
        .route("/api/v1/sources/{id}", get(sources_get).put(sources_update).delete(sources_delete))
        .route("/api/v1/sources/{id}/check", get(sources_check))
        .route("/api/v1/sources/{id}/sync", post(sources_sync))
        .route("/api/v1/sources/{id}/media", post(sources_media))
        .route("/api/v1/sources/{id}/forget", post(sources_forget))
        .route("/api/v1/sources/{id}/trust", post(sources_trust))
        .route("/api/v1/jobs", get(jobs_list).post(jobs_enqueue))
        .route("/api/v1/jobs/clear", post(jobs_clear))
        .route("/api/v1/jobs/counts", get(jobs_counts))
        .route("/api/v1/jobs/{id}", get(jobs_get))
        .route("/api/v1/jobs/{id}/retry", post(jobs_retry))
        .route("/api/v1/jobs/{id}/run-next", post(jobs_run_next))
        .route("/api/v1/jobs/run-next", post(jobs_run_next_many))
        .route("/api/v1/jobs/{id}/cancel", post(jobs_cancel))
        .route("/api/v1/schedules", get(schedules_list).post(schedules_create))
        .route("/api/v1/schedules/{id}", get(schedules_get).put(schedules_update).delete(schedules_delete))
        .route("/api/v1/schedules/{id}/run", post(schedules_run))
        .route("/api/v1/catalog/platforms", get(catalog_platforms))
        .route("/api/v1/catalog/facets", get(catalog_facets))
        .route("/api/v1/catalog/titles", get(catalog_titles))
        .route("/api/v1/catalog/title_rows", post(catalog_title_rows))
        .route("/api/v1/catalog/crossplatform", get(catalog_crossplatform))
        .route("/api/v1/catalog/crossplatform/platforms", get(catalog_crossplatform_platforms))
        .route("/api/v1/catalog/definitive", get(catalog_definitive))
        .route("/api/v1/catalog/definitive/{id}", get(catalog_definitive_family))
        .route("/api/v1/catalog/definitive/{id}/order/move", post(definitive_order_move))
        .route("/api/v1/catalog/definitive/{id}/order", delete(definitive_order_reset))
        .route("/api/v1/catalog/definitive/{id}/ask", post(definitive_ask))
        .route("/api/v1/catalog/definitive/{id}/paste", post(definitive_paste))
        .route("/api/v1/catalog/definitive/batch-estimate", post(definitive_batch_estimate))
        .route("/api/v1/catalog/titles/{id}/basegame", post(title_basegame_set).delete(title_basegame_reset))
        .route("/api/v1/catalog/titles/{id}", get(catalog_title))
        .route("/api/v1/catalog/titles/{id}/versions", get(title_versions).post(title_versions_add))
        .route("/api/v1/catalog/titles/versions/relations/{id}", delete(title_versions_delete))
        .route("/api/v1/catalog/version-groups/{key}/pick", axum::routing::put(version_group_pick))
        .route("/api/v1/catalog/version-groups/{key}/reroll", post(version_group_reroll))
        .route("/api/v1/catalog/releases", get(catalog_releases))
        .route("/api/v1/catalog/patches", get(catalog_patches))
        .route("/api/v1/catalog/bios", get(catalog_bios))
        .route("/api/v1/catalog/releases/{id}", get(catalog_release))
        .route("/api/v1/catalog/games", get(catalog_games))
        .route("/api/v1/catalog/roms", get(catalog_roms))
        .route("/api/v1/catalog/roms/{id}", get(catalog_rom))
        .route("/api/v1/catalog/contents", get(catalog_contents))
        .route("/api/v1/catalog/contents/{id}", get(catalog_content))
        .route("/api/v1/catalog/files", get(catalog_files))
        .route("/api/v1/catalog/files/{id}", get(catalog_file))
        .route("/api/v1/catalog/lookup", get(catalog_lookup))
        .route("/api/v1/providers", get(providers_list))
        .route("/api/v1/providers/credentials", get(providers_credentials).put(providers_credentials_put))
        .route("/api/v1/providers/resolve-url", post(providers_resolve_url))
        .route("/api/v1/providers/{provider}/search", get(providers_search))
        .route("/api/v1/catalog/titles/{id}/refresh", post(title_refresh))
        .route("/api/v1/catalog/releases/{id}/refresh", post(release_refresh))
        .route("/api/v1/dev/counterparts/{file}", get(dev_counterparts))
        .route("/api/v1/catalog/titles/merge", post(titles_merge))
        .route("/api/v1/catalog/titles/dedupe", post(titles_dedupe))
        .route("/api/v1/catalog/titles/keywords", post(titles_add_keyword))
        .route("/api/v1/catalog/titles/keywords/remove", post(titles_remove_keyword_bulk))
        .route("/api/v1/catalog/keywords", get(keywords_list))
        .route("/api/v1/catalog/keywords/remove-auto", post(keywords_remove_auto))
        .route("/api/v1/dictionary", get(dictionary_list).post(dictionary_add))
        .route("/api/v1/dictionary/remove", post(dictionary_remove))
        .route("/api/v1/dictionary/blocks/clear", post(dictionary_clear_blocks))
        .route("/api/v1/dictionary/variants", get(dictionary_variants))
        .route("/api/v1/catalog/keywords/delete-unused", post(keywords_delete_unused))
        .route("/api/v1/catalog/keywords/{id}", axum::routing::patch(keyword_rename).delete(keyword_delete))
        .route("/api/v1/catalog/titles/merges", get(titles_merge_log))
        .route("/api/v1/catalog/titles/release-orders", get(titles_release_orders))
        .route("/api/v1/catalog/titles/release-orders/clear", post(titles_release_orders_clear))
        .route("/api/v1/catalog/titles/{id}/release-order/move", post(title_release_move))
        .route("/api/v1/catalog/titles/{id}/release-order", delete(title_release_order_reset))
        .route("/api/v1/catalog/titles/merges/decouple", post(titles_decouple_grade))
        .route("/api/v1/catalog/titles/merges/{id}/decouple", post(titles_decouple))
        .route("/api/v1/catalog/titles/{id}/keywords/{name}", delete(titles_remove_keyword))
        .route("/api/v1/catalog/titles/merge-suggestions", get(titles_suggestions))
        .route("/api/v1/catalog/titles/merge-suggestions/bulk", post(titles_suggestions_bulk))
        .route("/api/v1/catalog/titles/merge-suggestions/{igdb_id}/{action}", post(titles_suggestion_resolve))
        .route("/api/v1/review/merges", get(review_merges))
        .route("/api/v1/review/merges/{key}/{action}", post(review_merge_action))
        .route("/api/v1/review/aspects", get(review_aspects))
        .route("/api/v1/catalog/titles/{id}/links", post(title_link))
        .route("/api/v1/catalog/titles/{id}/locales", post(title_locales_set).delete(title_locales_unlock))
        .route("/api/v1/catalog/titles/{id}/links/{provider}", delete(title_unlink))
        .route("/api/v1/catalog/releases/{id}/title", post(release_title_set).delete(release_title_clear))
        .route("/api/v1/catalog/releases/{id}/split", post(release_split))
        .route("/api/v1/catalog/releases/split", post(releases_split))
        .route("/api/v1/catalog/releases/display", post(releases_display_set))
        .route("/api/v1/catalog/releases/move-title", post(releases_move))
        .route("/api/v1/catalog/releases/{id}/base", post(release_base_set))
        .route("/api/v1/catalog/releases/{id}/language", post(release_language_set))
        .route("/api/v1/catalog/base-links", get(base_links))
        .route("/api/v1/catalog/base-links/bulk", post(base_links_bulk))
        .route("/api/v1/catalog/releases/{id}/links", post(release_link))
        .route("/api/v1/catalog/releases/{id}/links/{provider}", delete(release_unlink))
        .route("/api/v1/dats/sources", get(dats_sources))
        .route("/api/v1/dats/imports", get(dats_imports))
        .route("/api/v1/dats/files", get(dats_files))
        .route("/api/v1/dats/files/{source}/{*name}", get(dats_file))
        .route("/api/v1/dats/bundle", get(dats_bundle))
        .route("/api/v1/dats/export", post(dats_export))
        .route("/api/v1/dats/upload", post(dats_upload).layer(DefaultBodyLimit::disable()))
        .layer(compression())
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

/// Binds and serves until `shutdown` resolves. Returns the bound address once listening
/// through `on_listen` (useful with port 0 in tests).
/// Binds [`share_router`] on `port` and serves it until `shutdown`.
///
/// Separate from [`serve`] and on its own port on purpose: which router a machine binds is
/// what makes it a server or a client that shares. A server binds the full [`router`] because
/// that is its job; a client binds only this, and binds nothing at all when sharing is off.
pub async fn serve_share(state: Shared, options: &ServeOptions, on_listen: impl FnOnce(SocketAddr), shutdown: impl Future<Output = ()> + Send + 'static) -> std::io::Result<()> {
    refuse_in_process(&state)?;
    serve_router(share_router(state), options, on_listen, shutdown).await
}

pub async fn serve(state: Shared, options: &ServeOptions, on_listen: impl FnOnce(SocketAddr), shutdown: impl Future<Output = ()> + Send + 'static) -> std::io::Result<()> {
    refuse_in_process(&state)?;
    serve_router(router(state), options, on_listen, shutdown).await
}

/// The desktop app's state makes every request its own admin, with no sign-in. Put on a
/// socket, that would hand the whole machine to anyone on the LAN -- so a desktop that shares
/// its collection must build a network state of its own (`in_process: false`) for the listener.
fn refuse_in_process(state: &ServerState) -> std::io::Result<()> {
    if state.in_process {
        return Err(std::io::Error::other("the in-process state can't be served on a network listener: it grants admin to every request"));
    }
    Ok(())
}

/// How long requests still in flight get once a stop is asked for. axum's graceful shutdown
/// has no limit of its own: it waits for every connection to finish, so one long download or
/// a client that never lets go held the whole stop -- the worker and the database behind it
/// -- for as long as it liked. Short, because Windows gives a closed console about five
/// seconds in all, and the database still has to be shut down inside them.
const DRAIN: std::time::Duration = std::time::Duration::from_secs(2);

async fn serve_router(app: Router, options: &ServeOptions, on_listen: impl FnOnce(SocketAddr), shutdown: impl Future<Output = ()> + Send + 'static) -> std::io::Result<()> {
    use axum::serve::ListenerExt;
    let listener = tokio::net::TcpListener::bind(SocketAddr::new(options.bind, options.port)).await?;
    on_listen(listener.local_addr()?);
    let make = app.into_make_service_with_connect_info::<SocketAddr>();
    let (stopping_tx, stopping_rx) = tokio::sync::oneshot::channel::<()>();
    let shutdown = async move {
        shutdown.await;
        let _ = stopping_tx.send(());
    };
    let tls = options.tls.clone();
    let serving = async move {
        match tls {
            // `tap_io` is what lets `ConnectInfo<SocketAddr>` (the private-IP check) work on a
            // listener that isn't a bare `TcpListener`.
            Some(tls) => axum::serve(tls::TlsListener::new(listener, tls)?.tap_io(|_| {}), make).with_graceful_shutdown(shutdown).await,
            None => axum::serve(listener, make).with_graceful_shutdown(shutdown).await,
        }
    };
    let drained = async move {
        match stopping_rx.await {
            Ok(()) => tokio::time::sleep(DRAIN).await,
            // Never asked to stop: the listener ended on its own, and says so itself.
            Err(_) => std::future::pending().await,
        }
    };
    tokio::select! {
        result = serving => result,
        _ = drained => {
            log::info!("stopping: connections still open after {DRAIN:?} were dropped");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An `App` whose pool never connects until something queries it -- enough for requests
    /// that `guard` settles before any handler reaches the database.
    fn lazy_app(name: &str) -> (App, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("rx_server_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let config = crate::database::DatabaseConfig::default().with_port(1);
        let pool = sqlx::mysql::MySqlPoolOptions::new().connect_lazy_with(config.connect_options(true));
        (App::with_pool(pool, crate::database::AppPaths::from_root(&root), config).unwrap(), root)
    }

    fn state(app: App, in_process: bool) -> Shared {
        Arc::new(ServerState { app, setup_token: None, in_process, allow_public_ips: false, fetch_loopback: false, lifecycle: Lifecycle::new(false), tls_fingerprint: None })
    }

    /// The desktop app's state grants admin to every request; it must never reach a socket.
    #[tokio::test]
    async fn the_in_process_state_is_never_served_on_a_network_listener() {
        let (app, root) = lazy_app("refuse");
        let options = ServeOptions { bind: "127.0.0.1".parse().unwrap(), port: 0, tls: None };
        let shared = state(app, true);
        let err = serve(shared.clone(), &options, |_| panic!("must not listen"), async {}).await.unwrap_err();
        assert!(err.to_string().contains("in-process"), "{err}");
        assert!(serve_share(shared, &options, |_| panic!("must not listen"), async {}).await.is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    /// On a network listener nothing is admitted without a token -- not a read, not an admin
    /// route -- while the open routes stay open.
    #[tokio::test]
    async fn a_network_listener_admits_nobody_without_a_token() {
        let (app, root) = lazy_app("network");
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let shared = state(app, false);
        let task = tokio::spawn(async move {
            let options = ServeOptions { bind: "127.0.0.1".parse().unwrap(), port: 0, tls: None };
            serve(shared, &options, |a| { let _ = addr_tx.send(a); }, async { let _ = stop_rx.await; }).await.unwrap();
        });
        let base = format!("http://{}", addr_rx.await.unwrap());
        let http = reqwest::Client::new();
        assert_eq!(http.get(format!("{base}/api/v1/ping")).send().await.unwrap().status(), 200);
        assert_eq!(http.get(format!("{base}/api/v1/auth/check")).send().await.unwrap().status(), 401);
        assert_eq!(http.post(format!("{base}/api/v1/server/restart")).send().await.unwrap().status(), 401);
        // An empty bearer is no token at all, not a way around the check.
        assert_eq!(http.get(format!("{base}/api/v1/auth/check")).header("authorization", "Bearer ").send().await.unwrap().status(), 401);
        let _ = stop_tx.send(());
        task.await.unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    /// The About notes follow `?lang=` through the whole router -- the way the desktop app
    /// and the console ask -- and anything else gets the English.
    #[tokio::test]
    async fn the_about_notes_follow_the_requested_language() {
        use tower::ServiceExt;
        let (app, root) = lazy_app("about");
        let router = router(state(app, true));
        let notes = |uri: &'static str| {
            let router = router.clone();
            async move {
                let mut req = axum::http::Request::get(uri).body(axum::body::Body::empty()).unwrap();
                // As the desktop's LocalBackend does: `guard` reads the peer address.
                req.extensions_mut().insert(axum::extract::ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 0))));
                let res = router.oneshot(req).await.unwrap();
                let status = res.status();
                if status != 200 { panic!("{uri}: {status} {}", String::from_utf8_lossy(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())); }
                let v: Value = serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap()).unwrap();
                (v["notes_language"].as_str().unwrap().to_string(), v["notes_markdown"].as_str().unwrap().to_string())
            }
        };
        let (lang, md) = notes("/api/v1/about?lang=ja").await;
        assert_eq!(lang, "ja");
        assert!(md.contains("## 変更点"), "{md}");
        // No ?lang= and no database to read ui_language from: English, and promptly.
        assert_eq!(notes("/api/v1/about").await.0, "en");
        assert_eq!(notes("/api/v1/about?lang=en").await.0, "en");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn private_ranges_are_recognized() {
        for ok in ["127.0.0.1", "10.1.2.3", "172.16.0.9", "172.31.255.255", "192.168.1.50", "169.254.1.1", "::1", "fc00::1", "fd12::34", "fe80::1", "::ffff:192.168.0.2"] {
            assert!(is_private_ip(ok.parse().unwrap()), "{ok} should be private");
        }
        for bad in ["8.8.8.8", "172.32.0.1", "100.64.0.1", "2001:db8::1", "::ffff:8.8.8.8"] {
            assert!(!is_private_ip(bad.parse().unwrap()), "{bad} should be public");
        }
    }

    #[test]
    fn query_token_is_extracted() {
        assert_eq!(query_token(Some("a=1&token=abc&b=2")), Some("abc"));
        assert_eq!(query_token(Some("a=1")), None);
        assert_eq!(query_token(None), None);
    }

    #[test]
    fn already_compressed_bodies_are_left_alone() {
        // Everything here is a content type this server actually emits: `application/zip` from
        // backup_download, the rest from media_store::mime_for or server::mime_for.
        for skipped in [
            "application/zip",
            "video/mp4",
            "video/webm",
            "video/x-matroska",
            "audio/mpeg",
            "audio/ogg",
            "audio/flac",
            "application/pdf",
            "application/octet-stream",
            "font/woff",
            "font/woff2",
        ] {
            assert!(is_already_compressed(skipped), "{skipped} should not be compressed again");
        }

        // The wins. Compressing these is the reason the layer exists at all -- a blanket
        // "skip binary" rule would lose the 4 MB wasm bundle.
        for compressed in [
            "application/json",
            "text/html",
            "text/css",
            "text/javascript",
            "application/wasm",
            "image/svg+xml",
            "text/csv",
            "audio/wav",
        ] {
            assert!(!is_already_compressed(compressed), "{compressed} should still be compressed");
        }

        // Parameters and casing must not defeat the match.
        assert!(is_already_compressed("application/zip; charset=binary"));
        assert!(is_already_compressed("VIDEO/MP4"));
        assert!(!is_already_compressed(""));

        // And the same through the predicate the layer actually installs.
        let rule = |content_type: Option<&str>| {
            let mut headers = HeaderMap::new();
            if let Some(value) = content_type {
                headers.insert(header::CONTENT_TYPE, value.parse().unwrap());
            }
            skip_already_compressed(StatusCode::OK, Version::HTTP_11, &headers, &Extensions::new())
        };
        assert!(!rule(Some("application/zip")));
        assert!(rule(Some("application/json")));
        // No content type at all: tower-http treats that as compressible, and so do we --
        // every binary handler in this server sets one.
        assert!(rule(None));
    }

    #[test]
    fn byte_ranges_follow_the_rfc_for_the_one_shape_media_elements_send() {
        // Ordinary, open-ended, suffix, clamped past the end.
        assert_eq!(byte_range("bytes=0-99", 1000), Some(Ok((0, 99))));
        assert_eq!(byte_range("bytes=100-", 1000), Some(Ok((100, 999))));
        assert_eq!(byte_range("bytes=-10", 1000), Some(Ok((990, 999))));
        assert_eq!(byte_range("bytes=-5000", 1000), Some(Ok((0, 999))), "a suffix longer than the file is the whole file");
        assert_eq!(byte_range("bytes=900-5000", 1000), Some(Ok((900, 999))), "end past EOF is clamped, not refused");
        assert_eq!(byte_range("bytes=999-999", 1000), Some(Ok((999, 999))));
        // Unsatisfiable: 416.
        assert_eq!(byte_range("bytes=1000-", 1000), Some(Err(())));
        assert_eq!(byte_range("bytes=50-10", 1000), Some(Err(())));
        assert_eq!(byte_range("bytes=-0", 1000), Some(Err(())));
        assert_eq!(byte_range("bytes=0-", 0), Some(Err(())), "nothing lies inside an empty file");
        // Not ours to handle: serve the whole thing with 200.
        assert_eq!(byte_range("bytes=0-99,200-299", 1000), None, "multipart ranges are served whole");
        assert_eq!(byte_range("items=0-9", 1000), None);
        assert_eq!(byte_range("bytes=-", 1000), None);
        assert_eq!(byte_range("bytes=abc-def", 1000), None);
    }

    #[test]
    fn the_web_build_route_serves_only_files_under_it() {
        assert!(gui_web_path_is_safe("assets/app-dxhba3cd85aeea4d398.css"));
        assert!(gui_web_path_is_safe("collection/titles"));
        // `/app/%2Fetc%2Fpasswd` and `/app//etc/passwd` arrive as a rooted path.
        for bad in ["/etc/passwd", "\\Windows\\win.ini", "C:/x", "../data/database.env", "assets/../../x", ".env", "a//b", "a\0b"] {
            assert!(!gui_web_path_is_safe(bad), "{bad:?}");
        }
    }

    #[test]
    fn only_dx_fingerprinted_names_are_immutable() {
        assert!(is_fingerprinted("romexplorer-gui_bg-dxh56494210d84d58c4.wasm"));
        assert!(is_fingerprinted("app-dxhba3cd85aeea4d398.css"));
        assert!(!is_fingerprinted("index.html"), "the one cache-busting root");
        assert!(!is_fingerprinted("favicon.png"));
        assert!(!is_fingerprinted("notes-dxh.txt"), "the marker alone is not a hash");
        assert!(!is_fingerprinted("thing-dxhzzzz.js"), "not hex");
        assert!(etag_matches("\"abc\"", "\"abc\""));
        assert!(etag_matches("W/\"abc\"", "\"abc\""));
        assert!(etag_matches("\"x\", \"abc\"", "\"abc\""));
        assert!(etag_matches("*", "\"abc\""));
        assert!(!etag_matches("\"abd\"", "\"abc\""));
    }

    #[test]
    fn token_compare_and_exemptions() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "abcd"));
        use access::{required, Access};
        use axum::http::Method;
        assert_eq!(required(&Method::GET, "/"), Access::Open);
        assert_eq!(required(&Method::GET, "/api/v1/ping"), Access::Open);
        assert_eq!(required(&Method::GET, "/icons/psx.png"), Access::Open);
        assert_ne!(required(&Method::GET, "/api/v1/media/1"), Access::Open);
        assert_ne!(required(&Method::GET, "/api/v1/status"), Access::Open);
        assert_ne!(required(&Method::POST, "/"), Access::Open);
    }

    #[test]
    fn the_session_cookie_is_found_among_others() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, "theme=dark; rx_session=abc123 ; other=1".parse().unwrap());
        assert_eq!(cookie_token(&headers), Some("abc123"));
        headers.insert(header::COOKIE, "rx_sessionx=nope".parse().unwrap());
        assert_eq!(cookie_token(&headers), None);
        assert!(session_cookie("t").contains("HttpOnly") && session_cookie("t").contains("Secure") && session_cookie("t").contains("SameSite=Strict"));
    }
}
