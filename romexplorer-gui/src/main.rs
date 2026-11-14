//! romexplorer's GUI: the web console's screens as Dioxus components, same layout and
//! stylesheet, talking to a romexplorer server over its REST API. Desktop starts (or finds)
//! the server on this machine; the web build is served by the server at `/app/`; mobile
//! connects to a server the user names. See docs/plan/gui.md.

// Features are additive, so `--features "desktop,web"` (or a `dx` invocation that leaves
// the defaults on for a web build) would otherwise switch `local` on for a target the
// backend crate cannot build for, and fail deep inside sqlx with nothing pointing here.
#[cfg(all(feature = "local", target_arch = "wasm32"))]
compile_error!("`local` (the in-process backend) cannot build for wasm32 -- use `--no-default-features --features web`");
#[cfg(all(feature = "local", any(target_os = "android", target_os = "ios")))]
compile_error!("`local` cannot build for mobile -- use `--no-default-features --features mobile`");

mod api;
mod cache;
#[cfg(feature = "local")]
mod boot;
mod components;
mod html;
mod screens;
mod state;
mod ui;

use dioxus::prelude::*;

use screens::*;
use screens::bigpicture::{BigPicture, BigPictureShell, BpCross, BpRelease, BpTitle};

// Compiled into the binary rather than shipped beside it.
//
// `asset!()` hands the file to the Dioxus asset pipeline, which `dx build` runs and a plain
// `cargo build` does not -- so a cargo-built desktop binary looked for `/assets/app.css`,
// found nothing, and rendered every screen unstyled: white background, serif text, and the
// nav tabs showing as native buttons because `nav button {}` never applied. Nothing in the
// app can detect that; it just looks wrong.
//
// `include_str!` makes the stylesheet part of the executable, so every way of building it
// produces the same UI. It is also what the web console does -- `app.css` is a verbatim copy
// of `console.html`'s own inline `<style>` -- so the two now load their styling the same way
// and cannot drift at run time.
//
// Trade-off: no CSS hot-reload under `dx serve`. Worth it for a stylesheet that is a copy of
// another file and changes rarely.
const APP_CSS: &str = include_str!("../assets/app.css");
const MOBILE_CSS: &str = include_str!("../assets/mobile.css");

/// Touch screens: holding a finger on something opens its right-click menu (iOS webviews
/// never send `contextmenu`; Android's own long-press one is used when it comes first).
const LONG_PRESS_JS: &str = r#"
if (!window.__rxLongPress) {
  window.__rxLongPress = true;
  let timer = null, start = null, fired = false;
  document.addEventListener('touchstart', e => {
    if (e.touches.length !== 1) return;
    const t = e.touches[0];
    start = { x: t.clientX, y: t.clientY, target: e.target }; fired = false;
    clearTimeout(timer);
    timer = setTimeout(() => { fired = true; start.target.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: start.x, clientY: start.y })); }, 550);
  }, { passive: true });
  document.addEventListener('touchmove', e => { const t = e.touches[0]; if (start && Math.hypot(t.clientX - start.x, t.clientY - start.y) > 10) clearTimeout(timer); }, { passive: true });
  document.addEventListener('touchend', e => { clearTimeout(timer); if (fired) { e.preventDefault(); fired = false; } });
  document.addEventListener('contextmenu', e => { if (e.isTrusted) clearTimeout(timer); }, true);
}
"#;

/// Every page the console has, by the same paths (the console's `#collection/titles/12`
/// is `/collection/titles/12`). `q` carries a page's query string (`?basegame=12`).
#[derive(Routable, Clone, PartialEq, Debug)]
#[rustfmt::skip]
pub enum Route {
    #[layout(Shell)]
        #[route("/")]
        Status {},
        #[route("/status")]
        StatusPage {},
        #[route("/collection/titles?:..q")]
        Titles { q: String },
        #[route("/collection/titles/:id")]
        TitleDetail { id: u64 },
        #[route("/collection/releases?:..q")]
        Releases { q: String },
        #[route("/collection/releases/:id")]
        Release { id: u64 },
        #[route("/collection/crossplatform")]
        Crossplatform {},
        #[route("/collection/definitive")]
        Definitive {},
        #[route("/collection/definitive/:id")]
        Family { id: u64 },
        #[route("/collection/games")]
        Games {},
        #[route("/collection/roms")]
        Roms {},
        #[route("/collection/roms/:id")]
        Rom { id: u64 },
        #[route("/collection/files")]
        Files {},
        #[route("/collection/files/:id")]
        File { id: u64 },
        #[route("/collection/contents")]
        Contents {},
        #[route("/collection/contents/:id")]
        Content { id: u64 },
        #[route("/collection/audio")]
        Audio {},
        #[route("/collection/multimedia")]
        Multimedia {},
        #[route("/collection/documents")]
        Documents {},
        #[route("/collection/source")]
        SourceCode {},
        #[route("/collection/etc")]
        Etc {},
        #[route("/collection/patches")]
        Patches {},
        #[route("/collection/bios")]
        Bios {},
        #[route("/collection/sets")]
        Sets {},
        #[route("/collection/sets/:id")]
        Set { id: u64 },
        #[route("/ra/game/:id")]
        RaGame { id: u64 },
        #[route("/ra/:list")]
        RaList { list: String },
        #[route("/edit/keywords")]
        EditKeywords {},
        #[route("/edit/dictionary")]
        EditDictionary {},
        #[route("/edit/merges?:..q")]
        EditMerges { q: String },
        #[route("/edit/orders")]
        EditOrders {},
        #[route("/edit/merge-review")]
        EditMergeReview {},
        #[route("/edit/aspect-review")]
        EditAspectReview {},
        #[route("/scan")]
        Scan {},
        #[route("/jobs")]
        Jobs {},
        #[route("/about")]
        About {},
        #[route("/server")]
        ServerConsole {},
        #[route("/config/directories")]
        ConfigDirectories {},
        #[route("/config/emulators")]
        ConfigEmulators {},
        #[route("/config/copy")]
        ConfigCopy {},
        #[route("/config/storage")]
        ConfigStorage {},
        #[route("/config/settings")]
        ConfigSettings {},
        #[route("/config/settings/json")]
        ConfigSettingsJson {},
        #[route("/config/schedules")]
        ConfigSchedules {},
        #[route("/config/server")]
        ConfigServer {},
        #[route("/config/backup")]
        ConfigBackup {},
        #[route("/config/share")]
        ConfigSources {},
        #[route("/config/mode")]
        ConfigMode {},
        #[route("/config/users")]
        ConfigUsers {},
        #[route("/config/credentials")]
        ConfigCredentials {},
        #[route("/config/ai")]
        ConfigAi {},
        #[route("/config/advanced")]
        ConfigAdvanced {},
        #[route("/data/providers")]
        DataProviders {},
        #[route("/data/dats")]
        DataDats {},
        #[route("/data/db")]
        DataDb {},
        #[route("/data/db/:table")]
        DataDbTable { table: String },
    #[end_layout]
    // Big Picture mode: full screen, its own frame (screens/bigpicture.rs).
    #[layout(BigPictureShell)]
        #[route("/bigpicture/list/:tab")]
        BigPicture { tab: String },
        #[route("/bigpicture/cross/:igdb")]
        BpCross { igdb: String },
        #[route("/bigpicture/title/:id")]
        BpTitle { id: u64 },
        #[route("/bigpicture/release/:id")]
        BpRelease { id: u64 },
    #[end_layout]
    #[route("/:..rest")]
    NotFound { rest: Vec<String> },
}

fn main() {
    #[cfg(feature = "local")]
    {
        // The in-process server and worker share this process's open-files limit, which
        // Linux and macOS start far too low for several jobs at once.
        romexplorer::common::fd_limit::raise();
        // The runtime is built here, not by dioxus, because start-up is async and has to
        // finish *before* the first render: the pool must exist before any `use_resource`
        // fires, or the Connect screen flashes on every launch. `enter()` makes this the
        // current handle, so `launch` reuses it instead of building a second one -- it
        // requires a multi-thread runtime, which is also what the worker needs.
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("couldn't start the async runtime");
        let guard = rt.enter();

        let boot = match rt.block_on(boot::prepare()) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("romexplorer: {e}");
                std::process::exit(1);
            }
        };

        // Quitting has to stop the worker and the bundled database -- nothing else owns them.
        // Not after `launch`: dioxus-desktop runs tao's event loop, whose `run` is `-> !` and
        // ends the process itself when the last window closes, so code after `launch` never
        // runs. (It used to sit there, and every quit orphaned mariadbd -- found on Windows,
        // where the orphan kept the datadir locked for the next start.) `LoopDestroyed` is the
        // loop's last event before that exit. The shutdown runs on its own thread because this
        // one is inside the runtime's context, where `block_on` is not allowed.
        let handle = rt.handle().clone();
        let mut boot = Some(boot);
        let cfg = dioxus::desktop::Config::new()
            .with_menu(None)
            // The window's own file-drop handler swallows HTML drag and drop on Windows (and
            // with it every drag-to-reorder); nothing here takes files dropped from the OS.
            .with_disable_drag_drop_handler(true)
            // WebView2's data in the app root's `data/`, not %LOCALAPPDATA%, so the app stays
            // portable.
            .with_data_directory(romexplorer::database::config::AppPaths::detect().webview_data_dir())
            .with_window(dioxus::desktop::WindowBuilder::new().with_title("romexplorer").with_inner_size(dioxus::desktop::LogicalSize::new(1400.0, 900.0)))
            .with_custom_event_handler(move |event, _| {
                if let dioxus::desktop::tao::event::Event::LoopDestroyed = event {
                    if let Some(boot) = boot.take() {
                        let handle = handle.clone();
                        let _ = std::thread::spawn(move || handle.block_on(boot.shutdown())).join();
                    }
                }
            });
        dioxus::LaunchBuilder::desktop().with_cfg(cfg).launch(App);
        // Unreachable (see above); kept alive until then by being in scope.
        drop(guard);
        drop(rt);
    }
    #[cfg(not(feature = "local"))]
    {
        #[cfg(target_arch = "wasm32")]
        {
            api::set_endpoint(api::saved_endpoint().unwrap_or_default());
        }
        // Mobile: the server the user connected to last, if any (set up under Settings).
        #[cfg(not(target_arch = "wasm32"))]
        api::set_endpoint(api::saved_endpoint().unwrap_or_default());
        // The phone (romcompanion) reads its synced copy first and the server for the rest.
        #[cfg(feature = "mobile")]
        {
            if let Err(e) = api::companion::open_saved() {
                eprintln!("romcompanion: the synced copy can't be read ({e}); sync again");
            }
            api::set_backend(Box::new(api::companion::CompanionBackend));
        }
        dioxus::launch(App);
    }
}

#[component]
fn App() -> Element {
    // Vocabulary and settings, once (and again when the token changes).
    let loaded = use_resource(move || async move { state::load().await });
    // Serves `/rxasset/...` -- media, icons and RA art -- from this process. Registered here
    // and not in `Shell`, because `use_asset_handler` deregisters when its component
    // unmounts and `App` never does.
    #[cfg(feature = "local")]
    api::assets::register();
    #[cfg(feature = "mobile")]
    api::companion::register_assets();
    // The phone's own background work: the daily sync and RA progress, only as turned on.
    #[cfg(feature = "mobile")]
    use_future(|| async { api::companion::background().await });
    use_effect(|| {
        let _ = document::eval(LONG_PRESS_JS);
    });
    rsx! {
        document::Title { "romexplorer" }
        document::Style { {APP_CSS} }
        document::Style { {MOBILE_CSS} }
        // The phone starts as a client of its own, signed in or not: sign-in waits under its
        // Settings, so it never stops the app here.
        match (if cfg!(feature = "mobile") { None } else { api::NEEDS_SIGN_IN.read().clone() }, &*loaded.read()) {
            // A 401 anywhere (signed out, or the device was revoked) and a failed first load
            // both land here.
            (Some(message), _) => rsx! { SignIn { error: message, on_done: move |_| { *api::NEEDS_SIGN_IN.write() = None; let mut l = loaded; l.restart(); } } },
            (None, Some(Err(e))) if !cfg!(feature = "mobile") && (e.status == 401 || e.status == 0) => rsx! { SignIn { error: e.message.clone(), on_done: move |_| { *api::NEEDS_SIGN_IN.write() = None; let mut l = loaded; l.restart(); } } },
            // Pages read the vocabulary / settings while they build (filters, editors): only
            // once they're in.
            (None, None) => rsx! { header { h1 { "romexplorer" } } main { id: "view", div { class: "muted", {state::tr("loading…")} } } },
            _ => rsx! { Router::<Route> {} },
        }
    }
}

/// Which top tab a page belongs to, and its subtab.
fn tab_of(route: &Route) -> (&'static str, &'static str) {
    use Route::*;
    match route {
        Status {} | StatusPage {} | NotFound { .. } => ("status", ""),
        Titles { .. } | TitleDetail { .. } => ("collection", "titles"),
        Releases { .. } | Release { .. } => ("collection", "releases"),
        Crossplatform {} => ("collection", "crossplatform"),
        Definitive {} | Family { .. } => ("collection", "definitive"),
        Games {} => ("collection", "games"),
        Roms {} | Rom { .. } => ("collection", "roms"),
        Files {} | File { .. } => ("collection", "files"),
        Contents {} | Content { .. } => ("collection", "contents"),
        Sets {} | Set { .. } => ("collection", "sets"),
        Audio {} => ("collection", "audio"),
        Multimedia {} => ("collection", "multimedia"),
        Documents {} => ("collection", "documents"),
        SourceCode {} => ("collection", "source"),
        Etc {} => ("collection", "etc"),
        Patches {} => ("collection", "patches"),
        Bios {} => ("collection", "bios"),
        RaList { .. } | RaGame { .. } => ("ra", ""),
        EditKeywords {} => ("edit", "keywords"),
        EditDictionary {} => ("edit", "dictionary"),
        EditMerges { .. } => ("edit", "merges"),
        EditOrders {} => ("edit", "orders"),
        EditMergeReview {} => ("edit", "merge_review"),
        EditAspectReview {} => ("edit", "aspect_review"),
        Scan {} => ("scan", ""),
        Jobs {} => ("jobs", ""),
        About {} => ("about", ""),
        ServerConsole {} => ("server", ""),
        ConfigDirectories {} => ("config", "directories"),
        ConfigEmulators {} => ("config", "emulators"),
        ConfigCopy {} => ("config", "copy"),
        ConfigStorage {} => ("config", "storage"),
        ConfigSettings {} => ("config", "settings"),
        ConfigSettingsJson {} => ("config", "settings_json"),
        ConfigSchedules {} => ("schedules", ""),
        ConfigServer {} => ("config", "server"),
        ConfigBackup {} => ("config", "backup"),
        ConfigSources {} => ("config", "share"),
        ConfigMode {} => ("config", "mode"),
        ConfigUsers {} => ("config", "users"),
        ConfigCredentials {} => ("config", "credentials"),
        ConfigAi {} => ("config", "ai"),
        ConfigAdvanced {} => ("config", "advanced"),
        DataProviders {} => ("data", "providers"),
        DataDats {} => ("data", "dats"),
        DataDb {} | DataDbTable { .. } => ("data", "db"),
        BigPicture { .. } | BpCross { .. } | BpTitle { .. } | BpRelease { .. } => ("bigpicture", ""),
    }
}

fn tab_route(tab: &str) -> Route {
    match tab {
        // The sub-tab `collection_default_tab` names; Titles when it names none.
        "collection" => {
            let wanted = state::collection_default_tab();
            subtabs("collection").into_iter().find(|(key, _, _)| *key == wanted).map_or(Route::Titles { q: String::new() }, |(_, _, route)| route)
        }
        "ra" => Route::RaList { list: "titles".into() },
        "edit" => Route::EditKeywords {},
        "scan" => Route::Scan {},
        "jobs" => Route::Jobs {},
        "schedules" => Route::ConfigSchedules {},
        "about" => Route::About {},
        "server" => Route::ServerConsole {},
        "config" => Route::ConfigDirectories {},
        "data" => Route::DataProviders {},
        _ => Route::Status {},
    }
}

/// Every Collection sub-tab, shown or not (`collection_hidden_tabs` picks).
fn collection_subtabs() -> Vec<(&'static str, &'static str, Route)> {
    vec![
        ("crossplatform", "Cross-platform", Route::Crossplatform {}),
        ("definitive", "Definitive", Route::Definitive {}),
        ("titles", "Titles", Route::Titles { q: String::new() }),
        ("releases", "Releases", Route::Releases { q: String::new() }),
        ("games", "Games", Route::Games {}),
        ("roms", "Roms", Route::Roms {}),
        ("files", "Files", Route::Files {}),
        ("contents", "Contents", Route::Contents {}),
        ("sets", "Sets", Route::Sets {}),
        ("audio", "Audio", Route::Audio {}),
        ("multimedia", "Multimedia", Route::Multimedia {}),
        ("documents", "Documents", Route::Documents {}),
        ("source", "Source code", Route::SourceCode {}),
        ("etc", "Etc", Route::Etc {}),
        ("patches", "Patches", Route::Patches {}),
        ("bios", "BIOS / firmware", Route::Bios {}),
    ]
}

fn subtabs(tab: &str) -> Vec<(&'static str, &'static str, Route)> {
    match tab {
        "collection" => {
            let hidden = state::collection_hidden_tabs();
            collection_subtabs().into_iter().filter(|(key, _, _)| !hidden.iter().any(|h| h == key)).collect()
        }
        "ra" => ["titles", "mastery", "complete", "beat", "in_progress", "to_play"]
            .iter()
            .zip(["Titles", "Mastery", "Complete", "Beat", "In progress", "To be played"])
            .map(|(k, l)| (*k, l, Route::RaList { list: k.to_string() }))
            .collect(),
        "edit" => vec![("keywords", "Keyword tags", Route::EditKeywords {}), ("dictionary", "Name dictionary", Route::EditDictionary {}), ("merges", "Combined titles", Route::EditMerges { q: String::new() }), ("orders", "Manual order", Route::EditOrders {}), ("merge_review", "Merge review", Route::EditMergeReview {}), ("aspect_review", "Aspect review", Route::EditAspectReview {})],
        "config" => vec![
            ("directories", "Directories", Route::ConfigDirectories {}),
            ("emulators", "Emulators", Route::ConfigEmulators {}),
            ("copy", "Copy", Route::ConfigCopy {}),
            ("storage", "Storage", Route::ConfigStorage {}),
            ("settings", "Settings", Route::ConfigSettings {}),
            ("settings_json", "Settings (JSON)", Route::ConfigSettingsJson {}),
            ("server", "Server", Route::ConfigServer {}),
            ("backup", "Backup", Route::ConfigBackup {}),
            ("share", "Share", Route::ConfigSources {}),
            ("mode", "Mode", Route::ConfigMode {}),
            ("users", "Users", Route::ConfigUsers {}),
            ("credentials", "Provider credentials", Route::ConfigCredentials {}),
            ("ai", "AI", Route::ConfigAi {}),
            ("advanced", "Advanced", Route::ConfigAdvanced {}),
        ],
        "data" => vec![("providers", "Providers", Route::DataProviders {}), ("dats", "DATs", Route::DataDats {}), ("db", "DB", Route::DataDb {})],
        _ => Vec::new(),
    }
}

/// The header's "!": every top tab and what it is for (`tabinfo.<tab>` in the language file).
#[component]
fn HeaderExplain() -> Element {
    let lines: Vec<(String, String)> = TABS.iter().map(|(key, label)| (state::t(&format!("tab.{key}"), label), state::t(&format!("tabinfo.{key}"), ""))).filter(|(_, text)| !text.is_empty()).collect();
    if lines.is_empty() {
        return rsx! {};
    }
    rsx! {
        ui::Explain {
            p { {state::t("tabinfo.header", "")} }
            for (label, text) in lines { p { b { "{label}" } ": {text}" } }
        }
    }
}

/// The "!" at the end of a tab's subtab row: what each subtab is for
/// (`tabinfo.<tab>.<sub>` in the language file).
#[component]
fn SubtabExplain(tab: String) -> Element {
    let lines: Vec<(String, String)> = subtabs(&tab)
        .into_iter()
        .map(|(key, label, _)| (state::t(&format!("sub.{tab}.{key}"), label), state::t(&format!("tabinfo.{tab}.{key}"), "")))
        .filter(|(_, text)| !text.is_empty())
        .collect();
    if lines.is_empty() {
        return rsx! {};
    }
    rsx! {
        ui::Explain {
            p { {state::t("tabinfo.subtabs", "")} }
            for (label, text) in lines { p { b { "{label}" } ": {text}" } }
        }
    }
}

/// " (12)" after a subtab whose list reported its size (the RA lists).
fn sub_count(tab: &str, key: &str) -> String {
    if tab != "ra" {
        return String::new();
    }
    state::RA_COUNTS.read().get(key).map(|n| format!(" ({n})")).unwrap_or_default()
}

thread_local! {
    static BIG_PICTURE_CHECKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

const TABS: [(&str, &str); 11] = [("status", "Status"), ("collection", "Collection"), ("ra", "RetroAchievements"), ("edit", "Edit"), ("scan", "Scan"), ("jobs", "Jobs"), ("schedules", "Scheduled jobs"), ("config", "Config"), ("data", "Data"), ("server", "Server console"), ("about", "About")];

/// The console's frame: header (database, job counts), tabs, subtabs, the page, and the
/// overlays (menu, modal, popover, toasts).
#[component]
fn Shell() -> Element {
    let route = use_route::<Route>();
    let (tab, sub) = tab_of(&route);
    let ra_list = if let Route::RaList { list } = &route { list.clone() } else { String::new() };
    let sub = if tab == "ra" { ra_list.as_str() } else { sub };
    let mut counts = use_signal(String::new);
    // The first-run mode choice, once per run, for an installation that was never asked.
    use_future(|| async { crate::screens::maybe_first_run().await });
    // A device set to open in Big Picture does so once per run; leaving it stays left.
    // The phone's whole UI is that view, so it always opens there.
    use_future(|| async {
        if !BIG_PICTURE_CHECKED.with(|c| c.replace(true)) && (cfg!(feature = "mobile") || screens::bigpicture::starts_in_big_picture().await) {
            navigator().replace(Route::BigPicture { tab: screens::bigpicture::first_tab() });
        }
    });
    // Job counts in the header, every few seconds.
    use_future(move || async move {
        loop {
            if let Ok(v) = api::get("/api/v1/jobs/counts").await {
                let text = api::items(&v).iter().map(|c| format!("{} {}", api::s(c, "status"), api::u(c, "count"))).collect::<Vec<_>>().join(" · ");
                counts.set(state::trf("jobs: {counts}", &[("counts", &text)]));
            }
            ui::sleep(5000).await;
        }
    });
    // Which database this is, as the console's header says it: once, from the status route.
    let mut db_line = use_signal(String::new);
    use_future(move || async move {
        if let Ok(v) = api::get("/api/v1/status").await {
            let d = &v["database"];
            db_line.set(format!("{}@{}:{}/{} · MariaDB {}", api::s(d, "user"), api::s(d, "host"), api::u(d, "port"), api::s(d, "database"), api::s(d, "server_version")));
        }
    });
    let db = state::vocab();
    let _ = db;
    // The header and tabs again once the settings are reloaded (the UI language).
    let _ = state::LOADED();
    let me = api::ME.read().clone();
    let signed_in = me.as_ref().and_then(|m| m.user.as_ref()).map(|u| format!("{} ({})", u.username, u.role.name()));
    let on_setup = me.as_ref().is_some_and(|m| m.kind == romexplorer_types::auth::SessionKind::Setup);
    // The catalogue editor changes the server's data; the server refuses it to anyone else, so
    // don't offer it.
    let tabs: Vec<(&str, &str)> = TABS.iter().copied().filter(|(k, _)| *k != "edit" || api::is_admin()).collect();
    rsx! {
        div { onclick: move |_| ui::close_menu(),
            header {
                // The webview has no chrome of its own, so the history buttons a browser would
                // give the console live here.
                span { class: "histnav",
                    button { class: "btn small", title: state::t("common.back", "back"), disabled: !navigator().can_go_back(), onclick: move |_| navigator().go_back(), "◀" }
                    button { class: "btn small", title: state::t("common.forward", "forward"), disabled: !navigator().can_go_forward(), onclick: move |_| navigator().go_forward(), "▶" }
                }
                h1 { "romexplorer" }
                HeaderExplain {}
                // A full-screen launcher view for handhelds and TVs (app only; CLAUDE.md).
                button { class: "btn small", title: state::tr("full screen, for a controller or touch"), onclick: move |_| { navigator().push(Route::BigPicture { tab: "titles".into() }); }, {state::tr("Big Picture")} }
                span { class: "meta", "{db_line}" }
                span { class: "spacer" }
                span { class: "meta", "{counts}" }
                if let Some(who) = signed_in {
                    span { class: "meta", " · {who} " }
                    button { class: "btn small", onclick: move |_| { spawn(async move { api::sign_out().await; }); }, {state::t("common.sign_out", "sign out")} }
                }
                if on_setup {
                    span { class: "meta", " · " }
                    Link { to: Route::ConfigUsers {}, class: "bad", {state::tr("setup token — create the admin account")} }
                }
                // The display language, top right like a web page's (the console's #hdr-lang).
                screens::language::LanguageSwitch {}
            }
            nav { id: "nav",
                for (key, label) in tabs {
                    {
                        let shown = state::t(&format!("tab.{key}"), label);
                        rsx! {
                            Link { key: "{key}", to: tab_route(key), class: if key == tab { "active" } else { "" }, button { class: if key == tab { "active" } else { "" }, "{shown}" } }
                        }
                    }
                }
            }
            if !subtabs(tab).is_empty() {
                nav { class: "sub", id: "subnav",
                    for (key, label, to) in subtabs(tab) {
                        {
                            let shown = format!("{}{}", state::t(&format!("sub.{tab}.{key}"), label), sub_count(tab, key));
                            rsx! { Link { key: "{key}", to: to, button { class: if key == sub { "active" } else { "" }, "{shown}" } } }
                        }
                    }
                    SubtabExplain { key: "{tab}-subinfo", tab: tab.to_string() }
                }
            }
            // Keyed on `REDRAW`, so an import that changed the settings rebuilds the page.
            for redraw in [state::REDRAW()] {
                main { key: "{redraw}", id: "view", Outlet::<Route> {} }
            }
            ui::MenuHost {}
            ui::ModalHost {}
            ui::PopoverHost {}
            ui::ToastHost {}
        }
    }
}

#[component]
fn NotFound(rest: Vec<String>) -> Element {
    rsx! { div { class: "card", h2 { {state::tr("Not found")} } p { class: "muted", "/{rest.join(\"/\")}" } Link { to: Route::Status {}, {state::tr("status")} } } }
}
