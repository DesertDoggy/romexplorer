//! App-wide data loaded once from the server: the vocabulary (platforms, regions,
//! languages, icons, job types, ...) and the UI settings the display helpers read (colors,
//! icons, language display, grid media type, view defaults, localized names, release
//! kinds). Kept in plain statics so the `html` helpers can read them anywhere; `LOADED`
//! is the reactive signal screens subscribe to so they render again once it's in.

use std::sync::RwLock;

use dioxus::prelude::*;
use serde_json::Value;

use crate::api;

#[derive(Debug, Clone, Default)]
pub struct AppData {
    pub vocab: Value,
    pub settings: Vec<Value>,
    /// The UI's text for the `ui_language` setting (`/api/v1/i18n/<lang>`).
    pub i18n: Value,
}

static DATA: RwLock<Option<AppData>> = RwLock::new(None);

/// The RA lists' sizes (their subtab labels), from the last page loaded.
pub static RA_COUNTS: GlobalSignal<std::collections::HashMap<String, u64>> = Signal::global(std::collections::HashMap::new);

/// Bumped when `DATA` changes.
pub static LOADED: GlobalSignal<u64> = Signal::global(|| 0);

/// Bumped to build the page in view again from scratch. A screen reads the settings while
/// it builds, so after a configuration import changed them (the UI language, colors, view
/// defaults) the page has to be rebuilt for them to show without a Save.
pub static REDRAW: GlobalSignal<u64> = Signal::global(|| 0);

/// A look at the loaded data in place. Every label goes through here (`tr`, `setting`,
/// `platform_name`), so it must not copy: a copy of the whole store per lookup -- the
/// vocabulary and every UI string -- was thousands of copies per grid redraw.
pub fn with_data<R>(f: impl FnOnce(&AppData) -> R) -> R {
    static EMPTY: std::sync::OnceLock<AppData> = std::sync::OnceLock::new();
    let guard = DATA.read().unwrap_or_else(|p| p.into_inner());
    f(guard.as_ref().unwrap_or_else(|| EMPTY.get_or_init(AppData::default)))
}

fn with_vocab<R>(f: impl FnOnce(&Value) -> R) -> R {
    with_data(|d| f(&d.vocab))
}

pub fn data() -> AppData {
    with_data(AppData::clone)
}

/// A copy of the vocabulary, for a screen that builds its options from it once per render.
pub fn vocab() -> Value {
    with_vocab(Value::clone)
}

/// The UI's text for `key` in the chosen language, else `fallback` (the English in the code).
pub fn t(key: &str, fallback: &str) -> String {
    with_data(|d| d.i18n.get(key).and_then(|v| v.as_str()).filter(|v| !v.is_empty()).map(str::to_string)).unwrap_or_else(|| fallback.to_string())
}

/// User-facing text keyed by its own English: the file maps the English to the chosen
/// language, and an English string with no entry is shown as it is. `trf` fills `{name}`
/// placeholders after translating, so word order can differ per language.
pub fn tr(text: &str) -> String {
    t(text, text)
}

/// `tr` for a word whose English has more than one sense ("set": the object, the badge
/// state, the button): the file may carry `context|text`, tried before the bare text, and
/// the English shown is still just `text`.
pub fn trc(context: &str, text: &str) -> String {
    let keyed = format!("{context}|{text}");
    let v = t(&keyed, "");
    if v.is_empty() { tr(text) } else { v }
}

/// A selection kind ("titles", "sets", ...) in the UI's language, for a `{kind}` fill:
/// the plural, or the singular for "this {kind}". The singular "set" is `trc`'d, since the
/// bare word is the "has a value" badge.
pub fn kind_word(kind: &str, singular: bool) -> String {
    match (kind, singular) {
        ("sets", true) => trc("sets", "set"),
        (k, true) => tr(k.trim_end_matches('s')),
        (k, false) => tr(k),
    }
}

pub fn trf(text: &str, fills: &[(&str, &str)]) -> String {
    let mut out = tr(text);
    for (name, value) in fills {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

/// A platform's name as shown: the canonical English name rows carry, or its Japanese name
/// when the `platform_name_language` setting asks for it.
pub fn platform_label(name: &str) -> String {
    if setting("platform_name_language").as_str() != Some("ja") {
        return name.to_string();
    }
    with_vocab(|v| v.get("platforms").and_then(|p| find(p, "name", name)).map(|p| api::s(p, "name_ja"))).filter(|n| !n.is_empty()).unwrap_or_else(|| name.to_string())
}

pub fn setting(key: &str) -> Value {
    with_data(|d| d.settings.iter().find(|s| s.get("key").and_then(|k| k.as_str()) == Some(key)).and_then(|s| s.get("value").cloned())).unwrap_or(Value::Null)
}

/// Loads (or reloads) vocabulary and settings.
pub async fn load() -> api::ApiResult<()> {
    let vocab = api::get("/api/v1/vocabulary").await?;
    let settings = api::get("/api/v1/settings").await.map(|v| api::items(&v)).unwrap_or_default();
    // Who this app is, for the header and for what it offers. A server from before accounts
    // has no such route: leave it unknown, which shows everything.
    *api::ME.write() = api::get_as::<romexplorer_types::auth::Me>("/api/v1/auth/me").await.ok();
    let i18n = load_i18n(&settings).await;
    *DATA.write().unwrap_or_else(|p| p.into_inner()) = Some(AppData { vocab, settings, i18n });
    *LOADED.write() += 1;
    Ok(())
}

pub async fn reload_settings() {
    if let Ok(v) = api::get("/api/v1/settings").await {
        let settings = api::items(&v);
        let i18n = load_i18n(&settings).await;
        if let Some(d) = DATA.write().unwrap_or_else(|p| p.into_inner()).as_mut() {
            d.settings = settings;
            d.i18n = i18n;
        }
        *LOADED.write() += 1;
    }
}

/// The UI language code (`ui_language`: en, ja, fr, ...), for a request whose answer should
/// come back in it (Merge review's lookup).
pub fn ui_language() -> String {
    with_data(|d| d.settings.iter().find(|s| s.get("key").and_then(|k| k.as_str()) == Some("ui_language")).and_then(|s| s.get("value")).and_then(|v| v.as_str()).map(str::to_string)).unwrap_or_else(|| "en".to_string())
}

async fn load_i18n(settings: &[Value]) -> Value {
    let lang = settings.iter().find(|s| s.get("key").and_then(|k| k.as_str()) == Some("ui_language")).and_then(|s| s.get("value")).and_then(|v| v.as_str()).unwrap_or("en").to_string();
    // English too: keyed text (tab explanations) is only in the language files.
    api::get(&format!("/api/v1/i18n/{}", api::encode(&lang))).await.unwrap_or_else(|_| Value::Object(Default::default()))
}

fn find<'a>(list: &'a Value, key: &str, val: &str) -> Option<&'a Value> {
    list.as_array()?.iter().find(|x| x.get(key).and_then(|k| k.as_str()) == Some(val))
}

pub fn platforms() -> Vec<Value> {
    vocab().get("platforms").and_then(|p| p.as_array()).cloned().unwrap_or_default()
}

pub fn platform_name(id: u64) -> String {
    let name = with_vocab(|v| v.get("platforms").and_then(|p| p.as_array()).and_then(|ps| ps.iter().find(|p| api::u(p, "id") == id)).map(|p| api::s(p, "name")));
    name.map(|n| platform_label(&n)).unwrap_or_else(|| format!("#{id}"))
}

pub fn platform_family(name: &str) -> String {
    with_vocab(|v| v.get("platforms").and_then(|p| find(p, "name", name)).map(|p| api::s(p, "family"))).unwrap_or_default()
}

pub fn platform_disc_based(name: &str) -> bool {
    with_vocab(|v| v.get("platforms").and_then(|p| find(p, "name", name)).is_some_and(|p| api::b(p, "is_disc_based")))
}

pub fn has_icon(file: &str) -> bool {
    with_vocab(|v| v.get("icons").and_then(|i| i.as_array()).is_some_and(|i| i.iter().any(|x| x.as_str() == Some(file))))
}

/// One setting's value looked at in place (`setting` copies it out).
fn with_setting<R>(key: &str, f: impl FnOnce(Option<&Value>) -> R) -> R {
    with_data(|d| f(d.settings.iter().find(|s| s.get("key").and_then(|k| k.as_str()) == Some(key)).and_then(|s| s.get("value"))))
}

pub fn ui_color(kind: &str, value: &str) -> Option<String> {
    with_setting("ui_colors", |s| s?.get(kind).and_then(|k| k.get(value)).and_then(|c| c.as_str()).map(str::to_string))
}

pub fn ui_icon(kind: &str, value: &str) -> Option<String> {
    with_setting("ui_icons", |s| s?.get(kind).and_then(|k| k.get(value)).and_then(|c| c.as_str()).map(str::to_string))
}

pub fn region_name(code: &str) -> String {
    with_vocab(|v| v.get("regions").and_then(|r| find(r, "code", code)).map(|r| api::s(r, "name"))).unwrap_or_else(|| code.to_string())
}

pub fn language_english(code: &str) -> String {
    with_vocab(|v| v.get("languages").and_then(|r| find(r, "code", code)).map(|r| api::s(r, "english"))).unwrap_or_else(|| code.to_string())
}

/// A language's own name (日本語), whatever `language_display` says. The UI's Korean is
/// `ko` (ISO 639-1); the vocabulary, following the DATs, calls it `kr`.
pub fn language_native(code: &str) -> String {
    let lookup = if code == "ko" { "kr" } else { code };
    with_vocab(|v| v.get("languages").and_then(|r| find(r, "code", lookup)).map(|r| api::s(r, "native"))).filter(|n| !n.is_empty()).unwrap_or_else(|| code.to_string())
}

/// A language as `language_display` says: code | english | native.
pub fn language_label(code: &str) -> String {
    let field = match with_setting("language_display", |s| s.and_then(Value::as_str).map(str::to_string)).as_deref() {
        Some("english") => "english",
        Some("native") => "native",
        _ => return code.to_string(),
    };
    with_vocab(|v| v.get("languages").and_then(|r| find(r, "code", code)).map(|l| api::s(l, field))).unwrap_or_else(|| code.to_string())
}

pub fn localized_names() -> bool {
    !matches!(setting("title_display_mode").as_str(), None | Some("english"))
}

/// The Collection sub-tab the Collection tab opens on (`collection_default_tab`).
pub fn collection_default_tab() -> String {
    setting("collection_default_tab").as_str().unwrap_or("titles").to_string()
}

/// The Collection sub-tabs turned off (`collection_hidden_tabs`); Titles and Releases never are.
pub fn collection_hidden_tabs() -> Vec<String> {
    setting("collection_hidden_tabs").as_array().into_iter().flatten().filter_map(|v| v.as_str()).filter(|t| !matches!(*t, "titles" | "releases")).map(str::to_string).collect()
}

pub fn release_kinds_default() -> String {
    setting("release_kinds_default").as_str().unwrap_or("all").to_string()
}

/// A list's default view (list / grid / ...), `view_defaults` or the fallback.
pub fn view_default(key: &str, fallback: &str) -> String {
    setting("view_defaults").get(key).and_then(|v| v.as_str()).unwrap_or(fallback).to_string()
}

/// A release as the lists show it (the console's `releaseDisplayName`).
pub fn release_display_name(r: &Value) -> String {
    if localized_names() {
        api::so(r, "display_name").or_else(|| api::so(r, "local_title")).unwrap_or_else(|| api::s(r, "name"))
    } else {
        api::s(r, "name")
    }
}

/// The gray line under a release name: the canonical DAT name under a localized main name,
/// or the localized name under the DAT name when localized titles are off.
pub fn release_sub_name(r: &Value) -> Option<String> {
    let main = release_display_name(r);
    let name = api::s(r, "name");
    let other = if main == name { api::so(r, "local_title").or_else(|| api::so(r, "display_name")).unwrap_or_default() } else { name };
    (!other.is_empty() && other != main).then_some(other)
}
