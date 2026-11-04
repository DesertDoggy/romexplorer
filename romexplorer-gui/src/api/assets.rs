//! Serving media, icons and RetroAchievements art to the webview, from this process.
//!
//! Desktop has no HTTP server any more, so `<img src="/api/v1/media/5">` resolves against
//! `dioxus://index.html/` and finds nothing. This registers a handler for `/rxasset/...`
//! and answers those requests directly out of the database and the media layout.
//!
//! It is a **path prefix on the page's own origin**, not a custom scheme
//! (`dioxus-desktop`'s `protocol.rs` matches `uri().path().split('/').nth(1)` against
//! registered handler names). That is what keeps `loading="lazy"` working: as far as the
//! webview is concerned these are ordinary same-origin images, so it still decides when to
//! fetch them. It also takes the bearer token out of `<img>` URLs, which until now sat in
//! the DOM and in webview history.
//!
//! Deliberately **not** routed through the in-process router: `LocalBackend`'s oneshot path
//! decodes replies with `from_utf8_lossy`, which would corrupt every byte of a JPEG.

use dioxus::desktop::use_asset_handler;
use dioxus::desktop::wry::http::Response;
use romexplorer::api::{self, App, ErrorKind};
use romexplorer::database::media_store;
use std::path::PathBuf;

/// Registers the handler. Must be called from a component that never unmounts (`App`):
/// `use_asset_handler` deregisters on unmount, and it is per-webview.
pub fn register() {
    register_console();
    use_asset_handler("rxasset", move |request, responder| {
        let path = request.uri().path().trim_start_matches("/rxasset").to_string();
        let query = request.uri().query().unwrap_or_default().to_string();
        let range = request.headers().get("range").and_then(|v| v.to_str().ok()).map(str::to_string);
        // `RequestAsyncResponder` is `Send` precisely so the work can happen off the event
        // loop, and dioxus runs a multi-thread runtime, so this never blocks the UI.
        tokio::spawn(async move {
            let located = match main_server_path(&path) {
                Some(theirs) => relay(&theirs, range.as_deref()).await,
                None => match serve(&path, &query).await {
                    Ok((file, mime)) => send(&file, mime, range.as_deref()).await,
                    Err(e) => Err(e),
                },
            };
            let response = match located {
                Ok(Sent { status, mime, bytes, content_range }) => {
                    let mut builder = Response::builder()
                        .status(status)
                        .header("Content-Type", mime)
                        .header("Accept-Ranges", "bytes")
                        // The same hour the HTTP path used, so a re-render of a grid doesn't
                        // re-read every thumbnail from disk.
                        .header("Cache-Control", "private, max-age=3600");
                    if let Some(content_range) = content_range {
                        builder = builder.header("Content-Range", content_range);
                    }
                    builder.body(std::borrow::Cow::Owned(bytes))
                }
                Err((status, message)) => Response::builder().status(status).body(std::borrow::Cow::Owned(message.into_bytes())),
            };
            if let Ok(response) = response {
                responder.respond(response);
            }
        });
    });
}

/// The Server console tab's session on the main server, while there is one. In memory only:
/// closing the app ends it.
static CONSOLE: std::sync::RwLock<Option<std::sync::Arc<romexplorer::api::sources::ConsoleSession>>> = std::sync::RwLock::new(None);

pub fn console_session() -> Option<std::sync::Arc<romexplorer::api::sources::ConsoleSession>> {
    CONSOLE.read().unwrap_or_else(|p| p.into_inner()).clone()
}

pub fn set_console_session(session: Option<romexplorer::api::sources::ConsoleSession>) {
    *CONSOLE.write().unwrap_or_else(|p| p.into_inner()) = session.map(std::sync::Arc::new);
}

/// Hands the main server's console to the webview as pages of this app's own origin.
///
/// The console page asks for everything by root path (`/api/v1/...`, `/icons/...`), so those
/// two prefixes are forwarded as they are, and `/rxserver/` is the page itself. Nothing else
/// in the desktop app uses the root `/api` or `/icons` paths -- its own images are under
/// `/rxasset/` -- so the two cannot be confused. Without a session every one of them is a
/// 401, which is also what the page sees once the tab signs out.
fn register_console() {
    for (name, strip) in [("rxserver", true), ("api", false), ("icons", false), ("player.js", false)] {
        use_asset_handler(name, move |request, responder| {
            let method = request.method().as_str().to_string();
            let uri = request.uri().clone();
            let content_type = request.headers().get("content-type").and_then(|v| v.to_str().ok()).map(str::to_string);
            let body = request.body().clone();
            tokio::spawn(async move {
                let path = if strip { "/".to_string() } else { uri.path_and_query().map(|p| p.as_str().to_string()).unwrap_or_else(|| uri.path().to_string()) };
                let (status, mime, bytes) = match console_session() {
                    None => (401, "application/json".to_string(), br#"{"error":"unauthorized","message":"not signed in to the server console"}"#.to_vec()),
                    Some(session) => match session.forward(&method, &path, content_type.as_deref(), body).await {
                        Ok(reply) => reply,
                        Err(e) => (502, "application/json".to_string(), serde_json::json!({ "error": "unavailable", "message": e.message }).to_string().into_bytes()),
                    },
                };
                if let Ok(response) = Response::builder().status(status).header("Content-Type", mime).header("Cache-Control", "no-store").body(std::borrow::Cow::Owned(bytes)) {
                    responder.respond(response);
                }
            });
        });
    }
}

/// `?a=1&b=2` -> the value for `key`. Small enough not to want a dependency.
fn param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|pair| pair.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| v))
}

fn not_found(what: impl std::fmt::Display) -> (u16, String) {
    (404, what.to_string())
}

/// Maps an `api::Error` onto the status the REST layer would have produced, so a screen
/// cannot tell which backend answered it.
fn from_api(e: api::Error) -> (u16, String) {
    let status = match e.kind {
        ErrorKind::Invalid => 400,
        ErrorKind::NotFound => 404,
        ErrorKind::Conflict => 409,
        _ => 500,
    };
    (status, e.message)
}

/// A located file, as `serve` hands it on. Kept as its own step (it used to read the bytes)
/// so the routes below stay one line each.
async fn read(path: PathBuf, mime: &'static str) -> Result<(PathBuf, &'static str), (u16, String)> {
    Ok((path, mime))
}

/// What goes back to the webview for one request.
struct Sent {
    status: u16,
    mime: String,
    bytes: Vec<u8>,
    content_range: Option<String>,
}

/// The most one ranged reply carries. A media element asks for `bytes=0-` and then seeks; it
/// is content with part of what it asked for and comes back for more, so a long FLAC or a
/// video is never held in memory whole.
const RANGE_CHUNK: u64 = 4 << 20;

/// `bytes=a-b`, `bytes=a-` or `bytes=-n` against a file of `len` bytes: the inclusive span
/// to send, capped at [`RANGE_CHUNK`]. `None` for anything else (several ranges, a start
/// past the end): the caller answers 416.
fn byte_span(range: &str, len: u64) -> Option<(u64, u64)> {
    let spec = range.trim().strip_prefix("bytes=")?;
    if spec.contains(',') || len == 0 {
        return None;
    }
    let (from, to) = spec.split_once('-')?;
    let (start, end) = match (from.trim(), to.trim()) {
        ("", n) => {
            let n: u64 = n.parse().ok()?;
            (len.saturating_sub(n.max(1)), len - 1)
        }
        (a, "") => (a.parse().ok()?, len - 1),
        (a, b) => (a.parse().ok()?, b.parse::<u64>().ok()?.min(len - 1)),
    };
    (start <= end && start < len).then(|| (start, end.min(start + RANGE_CHUNK - 1)))
}

/// Reads what the request asked for: the whole file, or -- with a `Range` header, which every
/// `<audio>` and `<video>` sends -- just that part, as a 206. Without this the handler read
/// each file whole and ignored `Range`, so nothing could seek and a long track was loaded
/// into memory entire.
async fn send(path: &std::path::Path, mime: &'static str, range: Option<&str>) -> Result<Sent, (u16, String)> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let failed = |e: std::io::Error| (500u16, format!("reading {}: {e}", path.display()));
    let Some(range) = range else {
        let bytes = tokio::fs::read(path).await.map_err(failed)?;
        return Ok(Sent { status: 200, mime: mime.to_string(), bytes, content_range: None });
    };
    let mut file = tokio::fs::File::open(path).await.map_err(failed)?;
    let len = file.metadata().await.map_err(failed)?.len();
    let Some((start, end)) = byte_span(range, len) else {
        return Err((416, format!("range {range} is outside a file of {len} bytes")));
    };
    file.seek(std::io::SeekFrom::Start(start)).await.map_err(failed)?;
    let mut bytes = vec![0u8; (end - start + 1) as usize];
    file.read_exact(&mut bytes).await.map_err(failed)?;
    Ok(Sent { status: 206, mime: mime.to_string(), bytes, content_range: Some(format!("bytes {start}-{end}/{len}")) })
}

/// The main server's route behind one of this app's "not kept here" paths: media and audio a
/// client streams instead of storing (`media_client_keep`).
fn main_server_path(path: &str) -> Option<String> {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match segments.as_slice() {
        ["api", "v1", "media", "remote", id] if id.parse::<u64>().is_ok() => Some(format!("/api/v1/media/{id}")),
        ["api", "v1", "audio", "remote", id, "file"] if id.parse::<u64>().is_ok() => Some(format!("/api/v1/audio/{id}/file")),
        ["api", "v1", "audio", "remote", "rom", id, "file"] if id.parse::<u64>().is_ok() => Some(format!("/api/v1/audio/rom/{id}/file")),
        _ => None,
    }
}

/// What a media element's `Range` becomes when it is passed to the main server: the same
/// start, at most [`RANGE_CHUNK`] long, so a video is fetched a piece at a time here too.
/// A suffix range (`bytes=-n`) is passed as it is.
fn capped_range(range: &str) -> String {
    let Some((from, to)) = range.trim().strip_prefix("bytes=").and_then(|spec| spec.split_once('-')) else { return range.to_string() };
    let Ok(start) = from.trim().parse::<u64>() else { return range.to_string() };
    let last = start + RANGE_CHUNK - 1;
    let end = to.trim().parse::<u64>().map_or(last, |end| end.min(last));
    format!("bytes={start}-{end}")
}

/// One piece of a file on the main server, for the webview.
async fn relay(path: &str, range: Option<&str>) -> Result<Sent, (u16, String)> {
    let Some(state) = super::local::state() else {
        return Err((500, "no in-process backend".into()));
    };
    let range = range.map(capped_range);
    let response = api::media_sync::from_main(&state.app, path, range.as_deref()).await.map_err(from_api)?;
    let status = response.status().as_u16();
    let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
    let mime = header("content-type").unwrap_or_else(|| "application/octet-stream".to_string());
    let content_range = header("content-range");
    let bytes = response.bytes().await.map_err(|e| (502u16, format!("the main server stopped sending {path}: {e}")))?.to_vec();
    Ok(Sent { status, mime, bytes, content_range })
}

/// The routes the screens actually request. Anything not here 404s rather than guessing.
async fn serve(path: &str, query: &str) -> Result<(PathBuf, &'static str), (u16, String)> {
    let Some(state) = super::local::state() else {
        return Err((500, "no in-process backend".into()));
    };
    let app: &App = &state.app;
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    match segments.as_slice() {
        // Platform glyphs. Plain file names only -- the same rule the HTTP route enforces,
        // because this one string comes from a setting and a slug, not from a route match.
        ["icons", file] => {
            if file.contains('\\') || file.starts_with('.') {
                return Err((400, "bad icon name".into()));
            }
            let candidate = app.paths.icons_dir().join(file);
            if !candidate.is_file() {
                return Err(not_found(format!("no icon {file}")));
            }
            let mime = media_store::mime_for(&candidate);
            read(candidate, mime).await
        }
        ["api", "v1", "media", id] => {
            let id: u64 = id.parse().map_err(|_| (400u16, "media id must be a number".to_string()))?;
            let thumb = param(query, "size") == Some("thumb");
            let format = param(query, "format").filter(|f| !f.is_empty());
            let (file, mime) = api::media::file(app, id, thumb, format).await.map_err(from_api)?;
            read(file, mime).await
        }
        ["api", "v1", "audio", id, "file"] => {
            let id: u64 = id.parse().map_err(|_| (400u16, "audio id must be a number".to_string()))?;
            // The original, or for DSD / ALAC / AIFF the cached conversion (made on first use).
            let served = api::audio::file(app, id, &Default::default()).await.map_err(from_api)?;
            read(served.path, served.mime).await
        }
        ["api", "v1", "audio", "rom", id, "file"] => {
            let id: u64 = id.parse().map_err(|_| (400u16, "audio id must be a number".to_string()))?;
            // Decoded out of the ROM on its first play, then read from the audio cache.
            let served = api::rom_audio::serve(app, id).await.map_err(from_api)?;
            read(served.path, served.mime).await
        }
        ["api", "v1", "ra", "avatar"] => {
            let file = api::ra::avatar(app).await.map_err(from_api)?;
            let mime = media_store::mime_for(&file);
            read(file, mime).await
        }
        ["api", "v1", "ra", "games", id, "image", kind] => {
            let id: u32 = id.parse().map_err(|_| (400u16, "game id must be a number".to_string()))?;
            let file = api::ra::image(app, id, kind).await.map_err(from_api)?;
            let mime = media_store::mime_for(&file);
            read(file, mime).await
        }
        // A game with no achievements has no mosaic; the HTTP route falls back to the icon.
        ["api", "v1", "ra", "games", id, "badges"] => {
            let id: u32 = id.parse().map_err(|_| (400u16, "game id must be a number".to_string()))?;
            let file = match api::ra::badge_mosaic(app, id).await.map_err(from_api)? {
                Some(file) => file,
                None => api::ra::image(app, id, "icon").await.map_err(from_api)?,
            };
            let mime = media_store::mime_for(&file);
            read(file, mime).await
        }
        ["api", "v1", "ra", "badges", name] => {
            let locked = param(query, "locked") == Some("true");
            let file = api::ra::badge(app, name, locked).await.map_err(from_api)?;
            let mime = media_store::mime_for(&file);
            read(file, mime).await
        }
        _ => Err(not_found(format!("no handler for {path}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::{byte_span, capped_range, main_server_path, param, RANGE_CHUNK};

    #[test]
    fn streamed_paths_map_to_the_main_servers_routes() {
        assert_eq!(main_server_path("/api/v1/media/remote/7").as_deref(), Some("/api/v1/media/7"));
        assert_eq!(main_server_path("/api/v1/audio/remote/3/file").as_deref(), Some("/api/v1/audio/3/file"));
        assert_eq!(main_server_path("/api/v1/media/7"), None);
        assert_eq!(main_server_path("/api/v1/media/remote/x"), None);
    }

    #[test]
    fn a_passed_on_range_is_capped_but_keeps_its_start() {
        assert_eq!(capped_range("bytes=0-"), format!("bytes=0-{}", RANGE_CHUNK - 1));
        assert_eq!(capped_range("bytes=100-199"), "bytes=100-199");
        assert_eq!(capped_range(&format!("bytes=10-{}", 100 * RANGE_CHUNK)), format!("bytes=10-{}", 10 + RANGE_CHUNK - 1));
        assert_eq!(capped_range("bytes=-500"), "bytes=-500");
    }

    #[test]
    fn ranges_as_a_media_element_sends_them() {
        assert_eq!(byte_span("bytes=0-", 1000), Some((0, 999)));
        assert_eq!(byte_span("bytes=200-299", 1000), Some((200, 299)));
        assert_eq!(byte_span("bytes=900-5000", 1000), Some((900, 999)), "an end past the file is clipped");
        assert_eq!(byte_span("bytes=-100", 1000), Some((900, 999)), "the last hundred bytes");
        assert_eq!(byte_span("bytes=1000-", 1000), None, "a start at the end is unsatisfiable");
        assert_eq!(byte_span("bytes=0-10,20-30", 1000), None, "one range at a time");
        assert_eq!(byte_span("items=0-10", 1000), None);
        // An open-ended request on a big file gets one chunk; the element asks again.
        assert_eq!(byte_span("bytes=0-", 100 << 20), Some((0, RANGE_CHUNK - 1)));
    }

    #[test]
    fn reads_the_query_shapes_the_builders_produce() {
        // `thumb` adds `size`, after a `token=` when the URL carries one.
        assert_eq!(param("size=thumb", "size"), Some("thumb"));
        assert_eq!(param("token=&size=thumb", "size"), Some("thumb"));
        assert_eq!(param("token=abc&size=thumb&format=jpeg", "format"), Some("jpeg"));
        assert_eq!(param("locked=true", "locked"), Some("true"));
        assert_eq!(param("token=", "size"), None);
        assert_eq!(param("", "size"), None);
    }
}
