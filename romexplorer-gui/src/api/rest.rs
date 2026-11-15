//! The HTTP backend: romexplorer_server over the wire. What every build used to do
//! unconditionally, moved behind [`Backend`] unchanged.

use serde_json::Value;
#[cfg(target_arch = "wasm32")]
use std::sync::OnceLock;

use super::backend::{Backend, BoxFut, Method};
use super::{endpoint, url, ApiError, ApiResult};

#[cfg(target_arch = "wasm32")]
fn client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new).clone()
}

/// Native: a client that trusts exactly the certificate pinned for this endpoint (the
/// server's is self-signed; see `romexplorer_types::pinning`). Rebuilt only when the pin
/// changes.
#[cfg(not(target_arch = "wasm32"))]
fn client() -> reqwest::Client {
    static CACHE: std::sync::RwLock<Option<(String, reqwest::Client)>> = std::sync::RwLock::new(None);
    let fingerprint = endpoint().fingerprint;
    if let Some((fp, c)) = CACHE.read().unwrap_or_else(|p| p.into_inner()).as_ref() {
        if *fp == fingerprint {
            return c.clone();
        }
    }
    let client = if fingerprint.is_empty() {
        reqwest::Client::new()
    } else {
        use romexplorer_types::pinning::{client_config, Pin};
        reqwest::Client::builder().use_preconfigured_tls(client_config(Pin::Expect(fingerprint.clone()))).build().unwrap_or_default()
    };
    *CACHE.write().unwrap_or_else(|p| p.into_inner()) = Some((fingerprint, client.clone()));
    client
}

/// The certificate a server presents, for the user to confirm before it is trusted -- the
/// first connection to a server, like SSH's first connection to a host. `None` over plain
/// HTTP (this machine's own server).
#[cfg(not(target_arch = "wasm32"))]
pub async fn probe_fingerprint(base: &str) -> ApiResult<Option<String>> {
    use romexplorer_types::pinning::{client_config, Pin};
    if !base.starts_with("https://") {
        return Ok(None);
    }
    let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
    let probe = reqwest::Client::builder().use_preconfigured_tls(client_config(Pin::Capture(slot.clone()))).build().map_err(transport_error)?;
    probe.get(format!("{base}/api/v1/ping")).send().await.map_err(transport_error)?;
    let captured = slot.lock().unwrap_or_else(|p| p.into_inner()).clone();
    Ok(captured)
}

fn method(m: Method) -> reqwest::Method {
    match m {
        Method::Get => reqwest::Method::GET,
        Method::Post => reqwest::Method::POST,
        Method::Put => reqwest::Method::PUT,
        Method::Patch => reqwest::Method::PATCH,
        Method::Delete => reqwest::Method::DELETE,
    }
}

/// `status == 0` is the sentinel for "never reached the server", which is what the offline
/// cache keys off -- an error the server *answered* with is a real answer, not a miss.
fn transport_error(e: impl std::fmt::Display) -> ApiError {
    ApiError { status: 0, message: format!("can't reach the server: {e}") }
}

/// Shared with the in-process backend so both shape failures identically -- a screen
/// showing `error.message` must not care which one answered it. A success is handed back as
/// the raw text; `super::parse_value` or a typed `from_str` does the rest.
pub(super) fn decode_text(status: u16, text: String) -> ApiResult<String> {
    if status >= 400 {
        let value: Value = if text.is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::Null) };
        let message = value
            .pointer("/error/message")
            .and_then(|m| m.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| if text.is_empty() { format!("HTTP {status}") } else { text.chars().take(300).collect() });
        return Err(ApiError { status, message: if status == 401 { format!("unauthorized: {message}") } else { message } });
    }
    Ok(text)
}

pub(super) fn decode(status: u16, text: String) -> ApiResult {
    decode_text(status, text).map(super::parse_value)
}

pub struct RestBackend;

impl Backend for RestBackend {
    fn call<'a>(&'a self, m: Method, path: &'a str, body: Option<&'a Value>) -> BoxFut<'a, ApiResult<String>> {
        Box::pin(async move {
            let ep = endpoint();
            let mut req = client().request(method(m), url(path));
            if !ep.token.is_empty() {
                req = req.bearer_auth(&ep.token);
            }
            if let Some(b) = body {
                req = req.json(b);
            }
            let res = req.send().await.map_err(transport_error)?;
            let status = res.status().as_u16();
            let text = res.text().await.unwrap_or_default();
            let out = decode_text(status, text);
            // Signed out, revoked, or never signed in: back to the sign-in screen, from any
            // page. (A wrong password on the sign-in screen itself is just an error there.)
            if let Err(e) = &out {
                if e.status == 401 && path != "/api/v1/auth/login" {
                    *super::NEEDS_SIGN_IN.write() = Some(e.message.clone());
                }
            }

            // The offline cache lives here rather than in `api::get`, for two reasons: an
            // in-process backend can't miss the way this one can (a database error is a 5xx,
            // never the status-0 sentinel), and a screen that moves to a typed call would
            // otherwise silently stop being cached. Mobile is the build that needs it.
            // Keyed by server as well as path, and cleared on signing in or out, so a cached
            // page never answers for another server or another account.
            let key = format!("{}{path}", ep.base);
            match (m, out) {
                (Method::Get, Ok(text)) => {
                    crate::cache::put(&key, &text);
                    Ok(text)
                }
                (Method::Get, Err(e)) if e.status == 0 => crate::cache::get(&key).ok_or(e),
                (_, out) => out,
            }
        })
    }

    fn upload<'a>(&'a self, path: &'a str, fields: Vec<(String, String)>, file_field: &'a str, file_name: String, bytes: Vec<u8>) -> BoxFut<'a, ApiResult> {
        Box::pin(async move {
            let ep = endpoint();
            let mut form = reqwest::multipart::Form::new();
            for (k, v) in fields {
                form = form.text(k, v);
            }
            form = form.part(file_field.to_string(), reqwest::multipart::Part::bytes(bytes).file_name(file_name));
            let mut req = client().post(url(path)).multipart(form);
            if !ep.token.is_empty() {
                req = req.bearer_auth(&ep.token);
            }
            let res = req.send().await.map_err(transport_error)?;
            let status = res.status().as_u16();
            let text = res.text().await.unwrap_or_default();
            decode(status, text)
        })
    }
}

/// A file from the server into `dest`, chunk by chunk (a phone's synced copy is tens of MB:
/// never held in memory whole). `on_bytes(done, total)` after each chunk.
#[cfg(feature = "mobile")]
pub async fn download(
    path: &str,
    dest: &std::path::Path,
    mut on_bytes: impl FnMut(u64, Option<u64>),
) -> ApiResult<u64> {
    use std::io::Write;
    let ep = endpoint();
    let mut req = client().get(url(path));
    if !ep.token.is_empty() {
        req = req.bearer_auth(&ep.token);
    }
    let mut res = req.send().await.map_err(transport_error)?;
    let status = res.status().as_u16();
    if status >= 400 {
        let text = res.text().await.unwrap_or_default();
        return decode_text(status, text).map(|_| 0);
    }
    let total = res.content_length();
    let mut file = std::fs::File::create(dest).map_err(|e| ApiError {
        status: 500,
        message: format!("{}: {e}", dest.display()),
    })?;
    let mut done = 0u64;
    while let Some(chunk) = res.chunk().await.map_err(transport_error)? {
        file.write_all(&chunk).map_err(|e| ApiError {
            status: 500,
            message: format!("{}: {e}", dest.display()),
        })?;
        done += chunk.len() as u64;
        on_bytes(done, total);
    }
    file.sync_all().ok();
    Ok(done)
}

/// A server path's bytes and content type: an image the synced copy doesn't hold.
#[cfg(feature = "mobile")]
pub async fn get_bytes(path: &str) -> ApiResult<(String, Vec<u8>)> {
    let ep = endpoint();
    let mut req = client().get(url(path));
    if !ep.token.is_empty() {
        req = req.bearer_auth(&ep.token);
    }
    let res = req.send().await.map_err(transport_error)?;
    let status = res.status().as_u16();
    let mime = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let bytes = res.bytes().await.map_err(transport_error)?;
    if status >= 400 {
        return Err(ApiError {
            status,
            message: String::from_utf8_lossy(&bytes).into_owned(),
        });
    }
    Ok((mime, bytes.to_vec()))
}
