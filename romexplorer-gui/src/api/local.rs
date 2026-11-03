//! This machine's own database, in this process. Desktop only.
//!
//! `docs/plan/api.md`: *"REST is how you reach a database that isn't in your process -- the
//! GUI never uses it for its own local database, only for the server's."* The payoff isn't
//! the loopback hop it removes (a few hundred microseconds against multi-second catalog
//! queries); it's that a client machine owns its catalog outright, so browsing stays local
//! SQL whether or not the main server is reachable.
//!
//! The requests still go through the server's own `axum` router, called directly with
//! `tower`'s `oneshot` -- no socket, no listener, no second process. That keeps every route,
//! extractor and error shape identical to the REST path for free, and means the ~120 screen
//! call sites don't change at all. Screens that later move to typed `api::*` calls simply
//! stop coming through here.

use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, Request};
use serde_json::Value;
use tower::ServiceExt;

use romexplorer::api::App;
use romexplorer::server::{Lifecycle, ServerState, Shared};

use super::backend::{Backend, BoxFut, Method};
use super::rest::decode_text;
use super::{ApiError, ApiResult};

/// The one `ServerState` this process has, for callers that need the `App` directly rather
/// than through a route -- chiefly the asset handler, which must hand the webview raw bytes
/// and so cannot go through the router (whose reply is decoded as UTF-8 text).
static STATE: OnceLock<Shared> = OnceLock::new();

/// `None` until [`LocalBackend::new`] has run, i.e. on the web and mobile builds.
pub fn state() -> Option<&'static Shared> {
    STATE.get()
}

pub struct LocalBackend {
    state: Shared,
    router: axum::Router,
}

impl LocalBackend {
    /// Wraps an already-open [`App`] (pool, paths, provider clients) in the same router
    /// `romexplorer_server` serves, minus the listener.
    ///
    /// Its requests never touch a socket, so there is nothing to sign in to: every one runs as
    /// this machine's own admin (`in_process`). Accounts only matter to a machine that other
    /// machines reach over the network.
    pub fn new(app: App, worker_running: bool) -> Self {
        let state = Arc::new(ServerState {
            app,
            setup_token: None,
            in_process: true,
            allow_public_ips: false,
            fetch_loopback: false,
            tls_fingerprint: None,
            lifecycle: Lifecycle::new(worker_running),
        });
        let router = romexplorer::server::router(state.clone());
        let _ = STATE.set(state.clone());
        Self { state, router }
    }

    /// For screens (and the asset handler) that call `romexplorer::api::*` directly rather
    /// than going through a route.
    pub fn app(&self) -> &App {
        &self.state.app
    }

    async fn oneshot(&self, req: Request<Body>) -> ApiResult<String> {
        let res = self
            .router
            .clone()
            .oneshot(req)
            .await
            .map_err(|e| ApiError { status: 500, message: format!("in-process request failed: {e}") })?;
        let status = res.status().as_u16();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .map_err(|e| ApiError { status: 500, message: format!("reading the reply failed: {e}") })?;
        decode_text(status, String::from_utf8_lossy(&bytes).into_owned())
    }

    fn request(&self, method: Method, path: &str) -> axum::http::request::Builder {
        Request::builder().method(method.as_str()).uri(path)
    }
}

impl Backend for LocalBackend {
    fn in_process(&self) -> bool {
        true
    }

    fn call<'a>(&'a self, m: Method, path: &'a str, body: Option<&'a Value>) -> BoxFut<'a, ApiResult<String>> {
        let body = body.cloned();
        Box::pin(async move {
            let builder = self.request(m, path);
            let built = match &body {
                Some(b) => builder
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(b).unwrap_or_default())),
                None => builder.body(Body::empty()),
            };
            let mut req = built.map_err(|e| ApiError { status: 400, message: format!("bad request: {e}") })?;
            // `guard` extracts ConnectInfo, which only a real listener would have populated.
            // Loopback, so the private-network check passes on its own terms rather than by
            // turning it off.
            req.extensions_mut().insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))));
            self.oneshot(req).await
        })
    }

    fn add_backup_from_path<'a>(&'a self, local_path: std::path::PathBuf, file_name: String) -> Option<BoxFut<'a, ApiResult>> {
        Some(Box::pin(async move {
            // Copied into the backup directory's own upload slot, then checked and named
            // exactly as a REST upload is (`api::backup::accept_upload`).
            let part = romexplorer::api::backup::upload_part_path(self.app()).await.map_err(from_api_error)?;
            let (from, to) = (local_path.clone(), part.clone());
            let copied = tokio::task::spawn_blocking(move || std::fs::copy(&from, &to)).await.map_err(|e| ApiError { status: 500, message: e.to_string() })?;
            if let Err(e) = copied {
                let _ = std::fs::remove_file(&part);
                return Err(ApiError { status: 400, message: format!("{}: {e}", local_path.display()) });
            }
            romexplorer::api::backup::accept_upload(self.app(), &part, &file_name)
                .await
                .map_err(from_api_error)
                .and_then(|r| serde_json::to_value(r).map_err(|e| ApiError { status: 500, message: e.to_string() }))
        }))
    }

    fn add_dat_from_path<'a>(&'a self, local_path: std::path::PathBuf, file_name: String, source: Option<String>) -> Option<BoxFut<'a, ApiResult>> {
        Some(Box::pin(async move {
            // Copied into the DAT folder's scratch space, then stored exactly as a REST upload
            // is (`api::dats::accept_upload`).
            let part = romexplorer::api::dats::upload_part_path(self.app()).map_err(from_api_error)?;
            let (from, to) = (local_path.clone(), part.clone());
            let copied = tokio::task::spawn_blocking(move || std::fs::copy(&from, &to)).await.map_err(|e| ApiError { status: 500, message: e.to_string() })?;
            if let Err(e) = copied {
                let _ = std::fs::remove_file(&part);
                return Err(ApiError { status: 400, message: format!("{}: {e}", local_path.display()) });
            }
            romexplorer::api::dats::accept_upload(self.app(), &part, &file_name, source.as_deref())
                .await
                .map_err(from_api_error)
                .and_then(|r| serde_json::to_value(r).map_err(|e| ApiError { status: 500, message: e.to_string() }))
        }))
    }

    fn upload<'a>(&'a self, _path: &'a str, fields: Vec<(String, String)>, _file_field: &'a str, file_name: String, bytes: Vec<u8>) -> BoxFut<'a, ApiResult> {
        Box::pin(async move {
            // The one request whose body isn't JSON. Rather than build a multipart body just
            // for the router to take it apart again, call the operation it would have
            // reached: this is the GUI's only upload, and its fields are already name/value.
            let field = |k: &str| fields.iter().find(|(n, _)| n == k).map(|(_, v)| v.trim().to_string()).unwrap_or_default();
            let id = |k: &str| -> Option<u64> { field(k).parse().ok() };
            let target = romexplorer::api::media::SlotTarget { title_id: id("title_id"), release_id: id("release_id"), ..Default::default() };
            let sub = field("sub_category");
            let on_conflict = match romexplorer::api::media::OnConflict::parse(&field("on_conflict")) {
                Ok(c) => c,
                Err(e) => return Err(from_api_error(e)),
            };
            romexplorer::api::media::upload(self.app(), &target, &field("media_type"), (!sub.is_empty()).then_some(sub.as_str()), &file_name, &bytes, on_conflict)
                .await
                .map_err(from_api_error)
                .and_then(|r| serde_json::to_value(r).map_err(|e| ApiError { status: 500, message: e.to_string() }))
        })
    }
}

/// An `api::Error` in the shape the screens already read, mapped through the same status
/// codes the REST layer would have produced for it.
fn from_api_error(e: romexplorer::api::Error) -> ApiError {
    use romexplorer::api::ErrorKind;
    let status = match e.kind {
        ErrorKind::Invalid => 400,
        ErrorKind::NotFound => 404,
        ErrorKind::Conflict => 409,
        _ => 500,
    };
    ApiError { status, message: e.message }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The desktop app never signs in: its in-process requests carry no token and are this
    /// machine's admin. `server/restart` is admin-only and touches no database, so it shows
    /// exactly what `guard` decided.
    #[tokio::test]
    async fn the_desktop_app_is_admin_without_signing_in() {
        let root = std::env::temp_dir().join(format!("rx_gui_local_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let config = romexplorer::database::DatabaseConfig::default().with_port(1);
        let pool = sqlx::mysql::MySqlPoolOptions::new().connect_lazy_with(config.connect_options(true));
        let app = App::with_pool(pool, romexplorer::database::AppPaths::from_root(&root), config).unwrap();
        let backend = LocalBackend::new(app, false);

        assert!(backend.call(Method::Get, "/api/v1/auth/check", None).await.is_ok());
        let restart = backend.call(Method::Post, "/api/v1/server/restart", Some(&serde_json::json!({}))).await;
        assert!(restart.is_ok(), "an admin route, with no token: {restart:?}");
        let _ = std::fs::remove_dir_all(root);
    }
}
