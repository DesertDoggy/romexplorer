//! The phone build's data: a synced copy of the server's (or a client's) catalogue in
//! SQLite, read in place of asking the server for every screen.
//!
//! [`CompanionBackend`] sits in front of the REST backend: a GET the synced copy can answer
//! is answered from it ([`super::offline`]), anything else goes to the server, and when the
//! server can't be reached the copy answers whatever it can, ignoring the filters it can't
//! apply. Writes always go to the server.
//!
//! [`full_sync`] asks the machine the phone is connected to for a fresh copy
//! (`POST /api/v1/mobile/sync`, a job there), follows the job, downloads the file, adds the
//! fixed answers every screen needs (vocabulary, settings, translations), and swaps it in.
//! [`ra_sync`] refreshes RetroAchievements progress alone. Nothing here runs a job on its
//! own except those two, and they only when the user turned them on.
//!
//! Android and iOS both: no platform API is used, only the app's own files directory.

use std::path::PathBuf;
use std::sync::Mutex;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::backend::{Backend, BoxFut, Method};
use super::offline;
use super::rest::RestBackend;
use super::{ApiError, ApiResult};
pub use romexplorer_types::mobile::{Machine, SyncEstimate, SyncOptions, SyncSummary};

/// Where the app keeps its files: the synced copy, its settings.
pub fn data_dir() -> PathBuf {
    #[cfg(target_os = "android")]
    {
        // The package's own files directory. Android writes the package name as the
        // process's command line, so no Java call is needed to find it.
        let package = std::fs::read("/proc/self/cmdline")
            .ok()
            .and_then(|raw| {
                raw.split(|b| *b == 0).next().map(|n| {
                    String::from_utf8_lossy(n)
                        .split(':')
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_string()
                })
            })
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "com.desertdoggy.romcompanion".to_string());
        PathBuf::from("/data/data").join(package).join("files")
    }
    #[cfg(target_os = "ios")]
    {
        // The app's sandbox: Library/Application Support is kept and backed up, unlike Caches.
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
            .join("Library")
            .join("Application Support")
            .join("romcompanion")
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share"))
            })
            .unwrap_or_else(std::env::temp_dir);
        base.join("romcompanion")
    }
}

fn db_path() -> PathBuf {
    data_dir().join("synced.sqlite")
}

fn settings_path() -> PathBuf {
    data_dir().join("companion.json")
}

// --- settings ---

/// The phone's own settings: what to sync, and when. Saved in its files directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub options: SyncOptions,
    /// Sync on start when the server answers and the copy is older than a day. A server on
    /// the home network only answers at home, so this is "sync when back on the same network".
    pub auto_sync: bool,
    /// RA progress on its own, every `ra_every_min` while the app is open. Off by default:
    /// it may run over mobile data (the user is told when turning it on).
    pub ra_auto: bool,
    pub ra_every_min: u32,
    pub last_sync: Option<String>,
    pub last_ra_sync: Option<String>,
    pub last_summary: Option<SyncSummary>,
    /// The phone's own app settings (`PUT /api/v1/settings/{key}`: its language and the
    /// like). The phone is a client of its own, so they never change the server's; the
    /// synced copy's settings are only their defaults.
    pub app_settings: serde_json::Map<String, Value>,
    /// What is synced from each machine, per server the phone syncs from (by its address):
    /// machine id -> [`MODE_LOCAL`], [`MODE_CATALOG`] or [`MODE_NONE`]. A server not in here
    /// has not been asked yet: its first sync asks, starting from [`default_mode`].
    pub server_modes: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
}

/// The titles and releases that machine holds.
pub const MODE_LOCAL: &str = "local";
/// Those, plus the catalogue's titles not owned yet on the platforms it holds games for.
pub const MODE_CATALOG: &str = "catalog";
/// Nothing from that machine.
pub const MODE_NONE: &str = "none";

/// Where a machine starts before the user chooses: the main server brings the catalogue
/// along, each client only what it holds.
pub fn default_mode(m: &Machine) -> &'static str {
    if m.is_main {
        MODE_CATALOG
    } else {
        MODE_LOCAL
    }
}

/// The choices saved for the server the phone is connected to, if it was asked already.
pub fn current_modes() -> Option<std::collections::BTreeMap<String, String>> {
    settings().server_modes.get(&super::endpoint().base).cloned()
}

pub fn save_modes(modes: std::collections::BTreeMap<String, String>) {
    let mut s = settings();
    s.server_modes.insert(super::endpoint().base, modes);
    save_settings(&s);
}

/// The options a sync sends: the phone's own, with the machines from the saved choices.
pub fn sync_options(s: &Settings, modes: &std::collections::BTreeMap<String, String>) -> SyncOptions {
    let mut o = s.options.clone();
    o.machines = modes.iter().filter(|(_, m)| *m != MODE_NONE).map(|(id, _)| id.clone()).collect();
    o.catalog_machines = modes.iter().filter(|(_, m)| *m == MODE_CATALOG).map(|(id, _)| id.clone()).collect();
    // The catalogue comes by machine now, not from a hand-kept platform list.
    o.scope = "held".into();
    o.catalog_platforms.clear();
    o
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            options: SyncOptions::default(),
            auto_sync: true,
            ra_auto: false,
            ra_every_min: 30,
            last_sync: None,
            last_ra_sync: None,
            last_summary: None,
            app_settings: Default::default(),
            server_modes: Default::default(),
        }
    }
}

pub fn settings() -> Settings {
    std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_settings(s: &Settings) {
    let _ = std::fs::create_dir_all(data_dir());
    if let Ok(text) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(settings_path(), text);
    }
}

fn now() -> String {
    // Seconds since the epoch, as text: no date crate in the phone build for one timestamp.
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

/// Seconds since a [`now`] stamp, or `None` if never.
pub fn age(stamp: Option<&str>) -> Option<u64> {
    let then: u64 = stamp?.parse().ok()?;
    let now: u64 = now().parse().ok()?;
    Some(now.saturating_sub(then))
}

// --- the synced copy ---

static DB: Mutex<Option<Connection>> = Mutex::new(None);

fn with_db<R>(f: impl FnOnce(&mut Connection) -> R) -> Option<R> {
    let mut guard = DB.lock().unwrap_or_else(|p| p.into_inner());
    guard.as_mut().map(f)
}

/// Opens the synced copy if there is one. Called at start-up and after each sync.
pub fn open_saved() -> Result<bool, String> {
    let path = db_path();
    let mut guard = DB.lock().unwrap_or_else(|p| p.into_inner());
    *guard = None;
    if !path.is_file() {
        return Ok(false);
    }
    *guard = Some(offline::open(&path)?);
    Ok(true)
}

pub fn has_copy() -> bool {
    DB.lock().unwrap_or_else(|p| p.into_inner()).is_some()
}

/// The copy's own record of what it holds: options, machines, when it was made.
pub fn copy_meta(key: &str) -> Option<String> {
    with_db(|c| offline::meta(c, key)).flatten()
}

/// A cover or RA image from the copy.
pub fn image(key: &str) -> Option<(String, Vec<u8>)> {
    with_db(|c| offline::image(c, key)).flatten()
}

/// The image key the copy keeps a server path under (`/api/v1/media/5` -> `media/5`).
pub fn image_key(path: &str) -> Option<String> {
    let path = path.split('?').next().unwrap_or(path);
    if let Some(id) = path
        .strip_prefix("/api/v1/media/")
        .filter(|id| id.chars().all(|c| c.is_ascii_digit()))
    {
        return Some(format!("media/{id}"));
    }
    let rest = path.strip_prefix("/api/v1/ra/games/")?;
    let (gid, kind) = rest.split_once("/image/")?;
    Some(format!("ra/{gid}/{kind}"))
}

/// Whether a server has been set up. The phone starts without one, as a client of its
/// own: browsing the copy and its own settings need none, and the server is added later
/// under Settings.
pub fn has_server() -> bool {
    !super::endpoint().base.is_empty()
}

fn no_server() -> ApiError {
    ApiError {
        status: 0,
        message: crate::state::tr("No server set up yet (⚙ Settings › Server)."),
    }
}

/// The languages the app ships, built in, so the phone speaks the user's language before it
/// has ever reached a server. The same files the server builds in (`api::i18n`).
const BUILT_IN_I18N: [(&str, &str); 9] = [
    ("en", include_str!("../../../app_root_ex/assets/i18n/en.json")),
    ("ja", include_str!("../../../app_root_ex/assets/i18n/ja.json")),
    ("de", include_str!("../../../app_root_ex/assets/i18n/de.json")),
    ("es", include_str!("../../../app_root_ex/assets/i18n/es.json")),
    ("fr", include_str!("../../../app_root_ex/assets/i18n/fr.json")),
    ("it", include_str!("../../../app_root_ex/assets/i18n/it.json")),
    ("ko", include_str!("../../../app_root_ex/assets/i18n/ko.json")),
    ("pl", include_str!("../../../app_root_ex/assets/i18n/pl.json")),
    ("pt", include_str!("../../../app_root_ex/assets/i18n/pt.json")),
];

const SETTINGS_PATH: &str = "/api/v1/settings";

/// The settings list with the phone's own values laid over the copy's (or none, before the
/// first sync).
fn settings_list(base: Option<&str>, own: &serde_json::Map<String, Value>) -> Value {
    let base: Value = base
        .and_then(|t| serde_json::from_str(t).ok())
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let mut items = super::items(&base);
    for (key, value) in own {
        match items
            .iter_mut()
            .find(|i| i.get("key").and_then(Value::as_str) == Some(key.as_str()))
        {
            Some(Value::Object(item)) => {
                item.insert("value".into(), value.clone());
            }
            _ => items.push(serde_json::json!({ "key": key, "value": value })),
        }
    }
    match base {
        Value::Object(mut o) if o.contains_key("items") => {
            o.insert("items".into(), Value::Array(items));
            Value::Object(o)
        }
        _ => Value::Array(items),
    }
}

/// What the phone answers itself, server or not: its own settings, and the built-in
/// languages when the copy has none.
fn answer_locally(method: Method, path: &str, body: Option<&Value>) -> Option<ApiResult<String>> {
    let bare = path.split('?').next().unwrap_or(path);
    let key = bare
        .strip_prefix("/api/v1/settings/")
        .filter(|k| !k.is_empty() && !k.contains('/'));
    match (method, bare, key) {
        (Method::Put, _, Some(key)) => {
            let value = body.cloned().unwrap_or(Value::Null);
            let mut s = settings();
            s.app_settings.insert(key.to_string(), value.clone());
            save_settings(&s);
            Some(Ok(serde_json::json!({ "key": key, "value": value }).to_string()))
        }
        (Method::Get, SETTINGS_PATH, _) => {
            let base = with_db(|c| offline::answer(c, SETTINGS_PATH, false))
                .flatten()
                .and_then(Result::ok);
            Some(Ok(settings_list(base.as_deref(), &settings().app_settings).to_string()))
        }
        (Method::Get, _, Some(key)) => {
            let own = settings().app_settings.get(key).cloned()?;
            Some(Ok(serde_json::json!({ "key": key, "value": own }).to_string()))
        }
        (Method::Get, _, None) => {
            let lang = bare.strip_prefix("/api/v1/i18n/")?;
            let copy = with_db(|c| offline::answer(c, bare, false))
                .flatten()
                .and_then(Result::ok);
            let text = copy.or_else(|| {
                BUILT_IN_I18N
                    .iter()
                    .find(|(l, _)| *l == lang)
                    .map(|(_, t)| t.to_string())
            })?;
            Some(Ok(text))
        }
        _ => None,
    }
}

/// The vocabulary before the first sync with no server: just the languages, so the language
/// picker and the settings work.
fn empty_vocabulary() -> String {
    let langs: Vec<&str> = BUILT_IN_I18N.iter().map(|(l, _)| *l).collect();
    serde_json::json!({ "ui_languages": langs }).to_string()
}

pub struct CompanionBackend;

impl Backend for CompanionBackend {
    fn call<'a>(
        &'a self,
        method: Method,
        path: &'a str,
        body: Option<&'a Value>,
    ) -> BoxFut<'a, ApiResult<String>> {
        Box::pin(async move {
            if let Some(answer) = answer_locally(method, path, body) {
                return answer;
            }
            if method != Method::Get {
                if !has_server() {
                    return Err(no_server());
                }
                return RestBackend.call(method, path, body).await;
            }
            match with_db(|c| offline::answer(c, path, true)).flatten() {
                Some(Ok(text)) => return Ok(text),
                Some(Err(e)) => eprintln!("romcompanion: synced copy could not answer {path}: {e}"),
                None => {}
            }
            let reply = if has_server() {
                RestBackend.call(method, path, body).await
            } else {
                Err(no_server())
            };
            match reply {
                // The server can't be reached: the copy's best answer, filters it can't apply
                // left out, rather than nothing.
                Err(e) if e.status == 0 => {
                    match with_db(|c| offline::answer(c, path, false)).flatten() {
                        Some(Ok(text)) => Ok(text),
                        _ if path == "/api/v1/vocabulary" => Ok(empty_vocabulary()),
                        _ => Err(ApiError {
                            status: 0,
                            message: format!(
                                "{} {}",
                                e.message,
                                crate::state::tr("(not in the synced copy)")
                            ),
                        }),
                    }
                }
                other => other,
            }
        })
    }

    fn upload<'a>(
        &'a self,
        path: &'a str,
        fields: Vec<(String, String)>,
        file_field: &'a str,
        file_name: String,
        bytes: Vec<u8>,
    ) -> BoxFut<'a, ApiResult> {
        RestBackend.upload(path, fields, file_field, file_name, bytes)
    }
}

// --- syncing ---

/// Where a sync is, for the screen: what it is doing, and how far.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Progress {
    pub message: String,
    pub done: u64,
    pub total: Option<u64>,
}

/// Every language the app ships: the copy keeps all of them, so switching language works
/// without the server.
const LANGUAGES: [&str; 9] = ["en", "ja", "fr", "es", "it", "pt", "de", "ko", "pl"];

/// The fixed answers every screen reads before it draws, captured into the copy.
fn fixed_paths() -> Vec<String> {
    let mut out: Vec<String> = [
        "/api/v1/vocabulary",
        "/api/v1/settings",
        "/api/v1/auth/me",
        "/api/v1/ra/status",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    out.extend(LANGUAGES.iter().map(|l| format!("/api/v1/i18n/{l}")));
    out
}

async fn sleep(ms: u64) {
    futures_timer::Delay::new(std::time::Duration::from_millis(ms)).await;
}

fn err(message: impl Into<String>) -> ApiError {
    ApiError {
        status: 500,
        message: message.into(),
    }
}

/// The machines the phone can count holdings on, from the machine it is connected to.
pub async fn machines() -> ApiResult<Vec<Machine>> {
    super::get_as("/api/v1/mobile/machines").await
}

pub async fn estimate(o: &SyncOptions) -> ApiResult<SyncEstimate> {
    super::post_as("/api/v1/mobile/estimate", o).await
}

/// Builds, downloads and swaps in a fresh copy. `report` is called as it goes.
pub async fn full_sync(mut report: impl FnMut(Progress)) -> ApiResult<SyncSummary> {
    if !has_server() {
        return Err(no_server());
    }
    let mut s = settings();
    let Some(modes) = current_modes() else {
        return Err(err(crate::state::tr(
            "Choose what to sync from each machine first (⟳ Sync now).",
        )));
    };
    let options = sync_options(&s, &modes);
    if options.machines.is_empty() {
        return Err(err(crate::state::tr("Every machine is set to Don't sync.")));
    }
    report(Progress {
        message: crate::state::tr("asking the server for a copy"),
        ..Default::default()
    });
    let queued: Value = super::post_as("/api/v1/mobile/sync", &options).await?;
    let job = queued
        .get("job_id")
        .and_then(Value::as_u64)
        .ok_or_else(|| err("the server did not start a sync job"))?;

    // The job, until it ends: its own detailed progress shown as it comes.
    let summary: SyncSummary = loop {
        let row = super::get(&format!("/api/v1/jobs/{job}")).await?;
        let status = row
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let message = row
            .get("progress_message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let done = row
            .get("progress_done")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let total = row.get("progress_total").and_then(Value::as_u64);
        match status {
            "done" => {
                let result = row.get("result").cloned().unwrap_or(Value::Null);
                break serde_json::from_value(result)
                    .map_err(|e| err(format!("the sync job's result: {e}")))?;
            }
            "failed" | "cancelled" => {
                let why = row
                    .get("last_error")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or(message);
                return Err(err(format!(
                    "{} {why}",
                    crate::state::tr("the server's sync job stopped:")
                )));
            }
            "pending" => report(Progress {
                message: crate::state::tr("waiting for the server to start the sync"),
                done: 0,
                total: None,
            }),
            _ => report(Progress {
                message: format!("{} {message}", crate::state::tr("server:")),
                done,
                total,
            }),
        }
        sleep(1000).await;
    };

    // The file, beside the current copy until it is whole.
    let dir = data_dir();
    std::fs::create_dir_all(&dir).map_err(|e| err(format!("{}: {e}", dir.display())))?;
    let part = dir.join("synced.sqlite.part");
    let _ = std::fs::remove_file(&part);
    let label = crate::state::tr("downloading");
    super::rest::download(
        &format!("/api/v1/cache/{}", super::encode(&summary.file)),
        &part,
        |done, total| {
            report(Progress {
                message: format!("{label} {}", crate::html::fmt_size(Some(done))),
                done,
                total,
            });
        },
    )
    .await?;

    // The answers every screen reads first, as this server gives them now.
    report(Progress {
        message: crate::state::tr("saving settings and translations"),
        ..Default::default()
    });
    {
        let conn = offline::open(&part).map_err(err)?;
        for path in fixed_paths() {
            if let Ok(text) = RestBackend.call(Method::Get, &path, None).await {
                offline::put_fixed(&conn, &path, &text).map_err(err)?;
            }
        }
    }

    // Swap: close the current copy, move the new one in, open it.
    {
        let mut guard = DB.lock().unwrap_or_else(|p| p.into_inner());
        *guard = None;
        std::fs::rename(&part, db_path())
            .map_err(|e| err(format!("{}: {e}", db_path().display())))?;
    }
    open_saved().map_err(err)?;
    crate::cache::clear();
    // The server keeps no copy of a phone's file once it has it.
    let _ = super::delete(&format!("/api/v1/cache/{}", super::encode(&summary.file))).await;

    s.last_sync = Some(now());
    s.last_ra_sync = Some(now());
    s.last_summary = Some(summary.clone());
    save_settings(&s);
    Ok(summary)
}

/// RetroAchievements progress alone: asks the server to refresh the account first, then
/// takes its rows into the copy (`GET /api/v1/mobile/ra`).
pub async fn ra_sync(refresh_account: bool) -> ApiResult<usize> {
    if !has_server() {
        return Err(no_server());
    }
    if !has_copy() {
        return Err(err(crate::state::tr("sync once first")));
    }
    if refresh_account {
        // Best effort: an account without RetroAchievements set up still gets the local rows.
        if let Ok(v) = super::post("/api/v1/ra/refresh", Value::Null).await {
            if let Some(job) = v.get("job_id").and_then(Value::as_u64) {
                for _ in 0..120 {
                    let row = super::get(&format!("/api/v1/jobs/{job}")).await?;
                    if matches!(
                        row.get("status").and_then(Value::as_str),
                        Some("done" | "failed" | "cancelled")
                    ) {
                        break;
                    }
                    sleep(1000).await;
                }
            }
        }
    }
    let v = super::get("/api/v1/mobile/ra").await?;
    let rows = v
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let progress = v
        .get("progress")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let n = with_db(|c| -> Result<usize, String> {
        let n = offline::replace_ra(c, &rows)?;
        offline::replace_table_rows(c, "ra_user_progress", &progress)?;
        Ok(n)
    })
    .ok_or_else(|| err(crate::state::tr("sync once first")))?
    .map_err(err)?;
    let mut s = settings();
    s.last_ra_sync = Some(now());
    save_settings(&s);
    crate::cache::clear();
    Ok(n)
}

/// What runs by itself, and only what the user turned on: a full sync on start when the
/// copy is a day old (or missing) and the server answers, and RA progress on its timer.
pub async fn background() {
    let _ = open_saved();
    let s = settings();
    if s.auto_sync
        && has_server()
        && current_modes().is_some()
        && age(s.last_sync.as_deref()).is_none_or(|a| a > 24 * 3600)
        && super::get("/api/v1/ping").await.is_ok()
    {
        *SYNCING.write() = Some(Progress::default());
        let result = full_sync(|p| *SYNCING.write() = Some(p)).await;
        *SYNCING.write() = None;
        *LAST_ERROR.write() = result.err().map(|e| e.message);
    }
    loop {
        let s = settings();
        if s.ra_auto
            && has_server()
            && has_copy()
            && age(s.last_ra_sync.as_deref())
                .is_none_or(|a| a >= u64::from(s.ra_every_min.max(5)) * 60)
        {
            let _ = ra_sync(true).await;
        }
        sleep(60_000).await;
    }
}

use dioxus::prelude::{GlobalSignal, Signal};

/// A sync running now, for every screen's status line.
pub static SYNCING: GlobalSignal<Option<Progress>> = Signal::global(|| None);
/// How the last automatic sync ended, when it failed.
pub static LAST_ERROR: GlobalSignal<Option<String>> = Signal::global(|| None);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_phones_own_settings_lie_over_the_copys() {
        let mut own = serde_json::Map::new();
        own.insert("ui_language".into(), Value::from("ja"));
        own.insert("phone_only".into(), Value::from(true));
        let copy = r#"{"items":[{"key":"ui_language","value":"en","help":"x"},{"key":"other","value":1}]}"#;
        let v = settings_list(Some(copy), &own);
        let items = super::super::items(&v);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["value"], "ja", "overridden in place, other fields kept");
        assert_eq!(items[0]["help"], "x");
        assert_eq!(items[1]["value"], 1);
        assert_eq!(items[2]["key"], "phone_only");
        // Before the first sync: the phone's own alone.
        let v = settings_list(None, &own);
        assert_eq!(super::super::items(&v).len(), 2);
    }

    #[test]
    fn every_language_is_built_in() {
        for (lang, text) in BUILT_IN_I18N {
            assert!(LANGUAGES.contains(&lang));
            assert!(serde_json::from_str::<Value>(text).is_ok_and(|v| v.is_object()), "{lang}");
        }
        assert_eq!(BUILT_IN_I18N.len(), LANGUAGES.len());
    }

    #[test]
    fn images_are_found_by_their_server_paths() {
        assert_eq!(image_key("/api/v1/media/42").as_deref(), Some("media/42"));
        assert_eq!(
            image_key("/api/v1/media/42?size=thumb&token=x").as_deref(),
            Some("media/42")
        );
        assert_eq!(
            image_key("/api/v1/ra/games/7/image/boxart").as_deref(),
            Some("ra/7/boxart")
        );
        assert_eq!(image_key("/api/v1/media/strip"), None);
    }

    #[test]
    fn the_defaults_match_what_a_phone_wants() {
        let s = Settings::default();
        assert!(s.auto_sync);
        assert!(
            !s.ra_auto,
            "RA auto sync may use mobile data: off until asked"
        );
        assert!(!s.options.audio);
        assert!(s.options.covers);
        assert!(
            s.options.tabs.crossplatform
                && s.options.tabs.definitive
                && s.options.tabs.titles
                && s.options.tabs.releases
                && s.options.tabs.ra
        );
        assert!(!s.options.tabs.games && !s.options.tabs.files && !s.options.tabs.contents);
    }
}

/// Serves `/rxasset/...` images to the webview: the synced copy's covers, else the server's
/// (`super::media_url` points every `<img>` here on the phone). Same-origin paths, so lazy
/// loading works and no token sits in the page.
pub fn register_assets() {
    use dioxus::mobile::use_asset_handler;
    use dioxus::mobile::wry::http::Response;
    use_asset_handler("rxasset", move |request, responder| {
        let full = request
            .uri()
            .path()
            .trim_start_matches("/rxasset")
            .to_string();
        let query = request
            .uri()
            .query()
            .map(|q| format!("?{q}"))
            .unwrap_or_default();
        dioxus::prelude::spawn(async move {
            let found = match image_key(&full).and_then(|k| image(&k)) {
                Some(hit) => Ok(hit),
                None => super::rest::get_bytes(&format!("{full}{query}"))
                    .await
                    .map_err(|e| e.message),
            };
            let response = match found {
                Ok((mime, bytes)) => Response::builder()
                    .status(200)
                    .header("Content-Type", mime)
                    .header("Cache-Control", "private, max-age=3600")
                    .body(bytes),
                Err(message) => Response::builder().status(404).body(message.into_bytes()),
            };
            if let Ok(r) = response {
                responder.respond(r);
            }
        });
    });
}
