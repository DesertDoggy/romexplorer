//! romcompanion's own screens, inside Big Picture's frame: the Sync sheet (sync now, RA
//! progress now, how the last one went), the phone's settings (what to sync, and when), the
//! sync status line, and the Held and Machine filters' choices.
//!
//! Everything here is the phone build's (`mobile`); the other builds get the catalogue's own
//! Held and Machine choices and nothing else, so Big Picture can name these without a cfg at
//! every use.

use dioxus::prelude::*;

use crate::state;

/// The Held filter's choices (`held=`, as `api::catalog::held_having` reads it): held or not
/// on the machine the Machine filter picks. Off the phone, the catalogue's own list, with the
/// backlog not yet on the main server on a client.
pub fn held_views() -> Vec<(String, String)> {
    #[cfg(feature = "mobile")]
    {
        [("", "anywhere"), ("held", "held"), ("not_held", "not held (catalogue only)")]
            .into_iter()
            .map(|(v, l)| (v.to_string(), state::tr(l)))
            .collect()
    }
    #[cfg(not(feature = "mobile"))]
    {
        super::common::held_options()
    }
}

/// The Machine filter's choices (`machine=`), apart from Held. On the phone: any machine,
/// this phone, and each machine the copy was synced with, by name. Elsewhere the catalogue's
/// own list: on a server or a standalone machine only "this machine".
pub fn machine_views() -> Vec<(String, String)> {
    #[cfg(feature = "mobile")]
    {
        use crate::api::companion;
        let mut out = vec![
            (String::new(), state::tr("any machine")),
            // Nothing is held on the phone yet; the choice is here for when it can be.
            ("phone".to_string(), state::tr("this phone")),
        ];
        let synced: Vec<String> = companion::copy_meta("options")
            .and_then(|o| serde_json::from_str::<companion::SyncOptions>(&o).ok())
            .map(|o| o.machines)
            .unwrap_or_default();
        let machines: Vec<companion::Machine> = companion::copy_meta("machines")
            .and_then(|m| serde_json::from_str(&m).ok())
            .unwrap_or_default();
        for m in machines.into_iter().filter(|m| synced.contains(&m.id)) {
            out.push((m.id.clone(), machine_label(&m)));
        }
        out
    }
    #[cfg(not(feature = "mobile"))]
    {
        super::common::machine_options()
    }
}

#[cfg(feature = "mobile")]
fn machine_label(m: &crate::api::companion::Machine) -> String {
    let role = if m.is_main { state::tr("main server") } else { state::tr("client") };
    format!("{} ({role})", m.name)
}

/// What to sync from each machine: Local, Local + catalogue, or Don't sync, one row per
/// machine. `modes` starts from the saved choice or the defaults; `on_change` gets the
/// whole set after each tap.
#[component]
pub fn MachineModes(on_change: EventHandler<std::collections::BTreeMap<String, String>>) -> Element {
    #[cfg(feature = "mobile")]
    {
        use crate::api::companion::{self, MODE_CATALOG, MODE_LOCAL, MODE_NONE};
        let machines = use_resource(|| async { companion::machines().await });
        let mut modes = use_signal(|| companion::current_modes());
        let list = match &*machines.read() {
            None => return rsx! { div { class: "muted", {state::tr("loading…")} } },
            Some(Err(e)) => return rsx! { div { class: "muted", {state::trf("Connect to the server to change this ({error}).", &[("error", &e.message)])} } },
            Some(Ok(list)) => list.clone(),
        };
        // Saved choices, with the defaults for any machine added since.
        let current: std::collections::BTreeMap<String, String> = list
            .iter()
            .map(|m| {
                let saved = modes().and_then(|s| s.get(&m.id).cloned());
                (m.id.clone(), saved.unwrap_or_else(|| companion::default_mode(m).to_string()))
            })
            .collect();
        rsx! {
            for m in list {
                {
                    let mode = current.get(&m.id).cloned().unwrap_or_default();
                    let games = state::trf("{n} games", &[("n", &m.games.to_string())]);
                    rsx! {
                        div { key: "{m.id}", class: "bp-machine",
                            div { b { {machine_label(&m)} } span { class: "muted", " · {games}" } }
                            div { class: "bp-actions",
                                for (value, label) in [(MODE_LOCAL, "Local titles"), (MODE_CATALOG, "Local + catalogue titles"), (MODE_NONE, "Don't sync")] {
                                    {
                                        let mut next = current.clone();
                                        let id = m.id.clone();
                                        rsx! {
                                            button { key: "{value}", class: if mode == value { "bp-choice active" } else { "bp-choice" },
                                                onclick: move |_| {
                                                    next.insert(id.clone(), value.to_string());
                                                    modes.set(Some(next.clone()));
                                                    on_change.call(next.clone());
                                                },
                                                {state::tr(label)}
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            p { class: "muted", {state::tr("Catalogue titles are the ones not owned yet, on the platforms that machine holds games for.")} }
        }
    }
    #[cfg(not(feature = "mobile"))]
    {
        let _ = on_change;
        rsx! {}
    }
}

/// Asked on the first sync with a server: what to sync from each of its machines. Saved
/// for that server; changed later under ⚙ Settings.
#[component]
pub fn MachineSheet() -> Element {
    #[cfg(feature = "mobile")]
    {
        use crate::api::companion;
        let mut picked = use_signal(|| None::<std::collections::BTreeMap<String, String>>);
        rsx! {
            super::bigpicture::SheetHead { title: state::tr("What to sync from each machine") }
            div { class: "bp-sheet",
                MachineModes { on_change: move |m| picked.set(Some(m)) }
                div { class: "bp-sheet-actions",
                    button { class: "bp-choice primary",
                        onclick: move |_| {
                            spawn(async move {
                                // Untouched: the defaults as shown.
                                let modes = match picked() {
                                    Some(m) => m,
                                    None => match companion::machines().await {
                                        Ok(list) => list.iter().map(|m| (m.id.clone(), companion::default_mode(m).to_string())).collect(),
                                        Err(e) => { crate::ui::toast(e.message, true); return; }
                                    },
                                };
                                companion::save_modes(modes);
                                crate::ui::close_modal();
                                start_sync();
                            });
                        },
                        {state::tr("Save and sync")}
                    }
                }
            }
        }
    }
    #[cfg(not(feature = "mobile"))]
    {
        rsx! {}
    }
}

/// Starts a full sync in the background, with its progress in the status line.
#[cfg(feature = "mobile")]
pub fn start_sync() {
    use crate::api::companion::{self, LAST_ERROR, SYNCING};
    if companion::has_server() && companion::current_modes().is_none() {
        crate::ui::open_modal(|| rsx! { MachineSheet {} });
        return;
    }
    *SYNCING.write() = Some(companion::Progress::default());
    *LAST_ERROR.write() = None;
    spawn(async move {
        let result = companion::full_sync(|p| *SYNCING.write() = Some(p)).await;
        *SYNCING.write() = None;
        match result {
            Ok(sum) => crate::ui::toast(state::trf("Synced: {size}", &[("size", &crate::html::fmt_size(Some(sum.bytes)))]), false),
            Err(e) => *LAST_ERROR.write() = Some(e.message),
        }
        // The tabs, filters and lists follow the new copy.
        let _ = crate::state::load().await;
    });
}

/// A sync running now: what it is doing and a bar. Nothing when none is.
#[component]
pub fn SyncLine() -> Element {
    #[cfg(feature = "mobile")]
    {
        use crate::api::companion::{LAST_ERROR, SYNCING};
        if let Some(p) = SYNCING() {
            let pct = p
                .total
                .filter(|t| *t > 0)
                .map(|t| (p.done as f64 / t as f64 * 100.0).min(100.0));
            return rsx! {
                div { class: "bp-syncline",
                    span { "⟳ {p.message}" }
                    if let Some(pct) = pct { div { class: "bp-bar", div { style: "width:{pct:.0}%" } } }
                }
            };
        }
        // Signed out, or the server stopped knowing this phone: it keeps working from its copy,
        // and says so here rather than stopping at a sign-in screen.
        if crate::api::NEEDS_SIGN_IN().is_some() && crate::api::companion::has_server() {
            return rsx! { div { class: "bp-syncline bad", onclick: move |_| crate::ui::open_modal(|| rsx! { ServerSheet {} }), {state::tr("Not signed in to the server. Tap to sign in.")} } };
        }
        if let Some(e) = LAST_ERROR() {
            return rsx! { div { class: "bp-syncline bad", onclick: move |_| *LAST_ERROR.write() = None, {state::trf("Sync failed: {error}", &[("error", &e)])} } };
        }
    }
    rsx! {}
}

#[cfg(feature = "mobile")]
fn ago(stamp: Option<&str>) -> String {
    match crate::api::companion::age(stamp) {
        None => state::tr("never"),
        Some(s) if s < 120 => state::tr("just now"),
        Some(s) if s < 7200 => state::trf("{n} minutes ago", &[("n", &(s / 60).to_string())]),
        Some(s) if s < 172_800 => state::trf("{n} hours ago", &[("n", &(s / 3600).to_string())]),
        Some(s) => state::trf("{n} days ago", &[("n", &(s / 86_400).to_string())]),
    }
}

/// The ⟳ sheet: sync now, RA progress now, and how the last sync went.
#[component]
pub fn SyncSheet() -> Element {
    #[cfg(feature = "mobile")]
    {
        use crate::api::companion::{self, SYNCING};
        let s = companion::settings();
        let busy = SYNCING().is_some();
        let mut ra_busy = use_signal(|| false);
        let server = crate::api::endpoint().base;
        let summary = s.last_summary.clone();
        rsx! {
            super::bigpicture::SheetHead { title: state::tr("Sync") }
            div { class: "bp-sheet",
                if server.is_empty() {
                    div { class: "card",
                        p { {state::tr("No server set up yet. The app works on its own until then; add the server or client to sync from when you want to.")} }
                        button { class: "bp-choice primary", onclick: move |_| { crate::ui::close_modal(); crate::ui::open_modal(|| rsx! { ServerSheet {} }); }, {state::tr("Set up the server")} }
                    }
                } else {
                    p { class: "muted", {state::trf("Synced from {server}", &[("server", &server)])} }
                }
                div { class: "kv",
                    div { class: "muted", {state::tr("last sync")} } div { {ago(s.last_sync.as_deref())} }
                    div { class: "muted", {state::tr("RA progress")} } div { {ago(s.last_ra_sync.as_deref())} }
                    if let Some(sum) = summary {
                        div { class: "muted", {state::tr("size")} } div { {crate::html::fmt_size(Some(sum.bytes))} }
                        div { class: "muted", {state::tr("pages")} } div { "{sum.pages}" }
                        div { class: "muted", {state::tr("covers")} } div { "{sum.covers}" }
                    }
                }
                SyncLine {}
                button { class: "bp-choice primary", disabled: busy,
                    onclick: move |_| {
                        // The first sync with a server asks what to take from each machine.
                        if companion::current_modes().is_none() { crate::ui::close_modal(); }
                        start_sync();
                    },
                    "⟳ " {state::tr("Sync now")}
                }
                button { class: "bp-choice", disabled: busy || ra_busy(),
                    onclick: move |_| {
                        ra_busy.set(true);
                        spawn(async move {
                            match companion::ra_sync(true).await {
                                Ok(n) => crate::ui::toast(state::trf("RA progress synced ({n} games)", &[("n", &n.to_string())]), false),
                                Err(e) => crate::ui::toast(e.message, true),
                            }
                            ra_busy.set(false);
                        });
                    },
                    if ra_busy() { {state::tr("syncing RA progress…")} } else { {state::tr("Sync RA progress only")} }
                }
                p { class: "muted", {state::tr("What is synced, and when, is under ⚙ Settings.")} }
            }
        }
    }
    #[cfg(not(feature = "mobile"))]
    {
        rsx! {}
    }
}

/// The phone's settings, inside Big Picture's settings sheet: what goes into the synced copy,
/// and what syncs by itself.
#[component]
pub fn PhoneSettings() -> Element {
    #[cfg(feature = "mobile")]
    {
        use crate::api::companion::{self, Settings};
        let mut cfg = use_signal(companion::settings);
        let mut ra_warn = use_signal(|| false);
        let mut estimate = use_signal(|| None::<Result<companion::SyncEstimate, String>>);
        let mut save = move |mut s: Settings| {
            // Kept elsewhere (the machine choices, the app's own settings): never from this
            // screen's older copy.
            let now = companion::settings();
            s.server_modes = now.server_modes;
            s.app_settings = now.app_settings;
            companion::save_settings(&s);
            cfg.set(s);
            estimate.set(None);
            // The tab buttons read these: show the change at once, not on the next tab.
            *state::LOADED.write() += 1;
        };
        let c = cfg();
        let o = c.options.clone();
        let tab_rows: [(&str, &str, bool); 8] = [
            ("crossplatform", "Cross-platform", o.tabs.crossplatform),
            ("definitive", "Definitive", o.tabs.definitive),
            ("titles", "Titles", o.tabs.titles),
            ("releases", "Releases", o.tabs.releases),
            ("ra", "RetroAchievements", o.tabs.ra),
            ("games", "Games", o.tabs.games),
            ("files", "Files", o.tabs.files),
            ("contents", "Contents", o.tabs.contents),
        ];
        let server = crate::api::endpoint().base;
        let signed_in = crate::api::ME().and_then(|m| m.user).map(|u| u.username);
        rsx! {
            h4 { {state::tr("Server")} }
            if server.is_empty() {
                p { class: "muted", {state::tr("None yet. Everything below works without one; add the server or client to sync from when you want to.")} }
                button { class: "bp-choice primary", onclick: move |_| crate::ui::open_modal(|| rsx! { ServerSheet {} }), {state::tr("Set up the server")} }
            } else {
                p { class: "muted",
                    "{server}"
                    if let Some(user) = signed_in { " · " {state::trf("signed in as {user}", &[("user", &user)])} }
                }
                div { class: "bp-actions",
                    button { class: "bp-choice", onclick: move |_| crate::ui::open_modal(|| rsx! { ServerSheet {} }), {state::tr("Sign in / change server")} }
                    button { class: "bp-choice", onclick: move |_| { spawn(async move { crate::api::sign_out().await; }); }, {state::tr("Sign out")} }
                }
            }
            h4 { {state::tr("What to sync from each machine")} }
            MachineModes { on_change: move |m| { companion::save_modes(m); estimate.set(None); } }
            h4 { {state::tr("Tabs")} }
            p { class: "muted", {state::tr("Games, Files and Contents are off on a phone by default; turn them on for a handheld with emulators.")} }
            div { class: "bp-choices",
                for (key, label, on) in tab_rows {
                    button { key: "{key}", class: if on { "bp-choice active" } else { "bp-choice" },
                        onclick: move |_| {
                            let mut s = cfg();
                            let t = &mut s.options.tabs;
                            let slot = match key { "crossplatform" => &mut t.crossplatform, "definitive" => &mut t.definitive, "titles" => &mut t.titles, "releases" => &mut t.releases, "ra" => &mut t.ra, "games" => &mut t.games, "files" => &mut t.files, _ => &mut t.contents };
                            *slot = !*slot;
                            save(s);
                        },
                        if on { "✓ " } {state::t(&format!("sub.collection.{key}"), label)}
                    }
                }
            }
            h4 { {state::tr("Pages and covers")} }
            button { class: if o.pages { "bp-choice active" } else { "bp-choice" }, onclick: move |_| { let mut s = cfg(); s.options.pages = !s.options.pages; save(s); },
                if o.pages { "✓ " } {state::tr("Full pages of held titles and releases")}
            }
            button { class: if o.covers { "bp-choice active" } else { "bp-choice" }, onclick: move |_| { let mut s = cfg(); s.options.covers = !s.options.covers; save(s); },
                if o.covers { "✓ " } {state::tr("Covers (converted on the server, not on the phone)")}
            }
            if o.covers {
                div { class: "bp-actions",
                    for (f, label) in [("avif", "AVIF (smallest)"), ("jpeg", "JPEG (older phones)")] {
                        button { key: "{f}", class: if o.cover_format == f { "bp-choice active" } else { "bp-choice" }, onclick: move |_| { let mut s = cfg(); s.options.cover_format = f.into(); save(s); }, {state::tr(label)} }
                    }
                    for px in [256u32, 512, 768] {
                        button { key: "{px}", class: if o.cover_px == px { "bp-choice active" } else { "bp-choice" }, onclick: move |_| { let mut s = cfg(); s.options.cover_px = px; save(s); }, "{px} px" }
                    }
                }
            }
            button { class: if o.audio { "bp-choice active" } else { "bp-choice" }, onclick: move |_| { let mut s = cfg(); s.options.audio = !s.options.audio; save(s); },
                if o.audio { "✓ " } {state::tr("Audio (off on a phone by default; not synced yet)")}
            }
            div { class: "bp-actions",
                button { class: "bp-choice",
                    onclick: move |_| { spawn(async move {
                        let modes = companion::current_modes().unwrap_or_default();
                        let o = companion::sync_options(&cfg(), &modes);
                        estimate.set(Some(companion::estimate(&o).await.map_err(|e| e.message)));
                    }); },
                    {state::tr("Estimate the size")}
                }
                match estimate() {
                    Some(Ok(e)) => rsx! { span { {state::trf("about {size}: {titles} titles, {releases} releases, {pages} pages, {covers} covers", &[("size", &crate::html::fmt_size(Some(e.bytes))), ("titles", &e.titles.to_string()), ("releases", &e.releases.to_string()), ("pages", &e.pages.to_string()), ("covers", &e.covers.to_string())])} } },
                    Some(Err(e)) => rsx! { span { class: "muted", "{e}" } },
                    None => rsx! {},
                }
            }
            h4 { {state::tr("Syncing by itself")} }
            button { class: if c.auto_sync { "bp-choice active" } else { "bp-choice" }, onclick: move |_| { let mut s = cfg(); s.auto_sync = !s.auto_sync; save(s); },
                if c.auto_sync { "✓ " } {state::tr("Sync on start when the server can be reached (at most once a day)")}
            }
            button { class: if c.ra_auto { "bp-choice active" } else { "bp-choice" },
                onclick: move |_| { if cfg().ra_auto { let mut s = cfg(); s.ra_auto = false; save(s); } else { ra_warn.set(true); } },
                if c.ra_auto { "✓ " } {state::tr("Sync RetroAchievements progress automatically")}
            }
            if ra_warn() {
                div { class: "card",
                    p { {state::tr("This syncs RA progress every so often while the app is open, wherever the server can be reached, which may use mobile data.")} }
                    div { class: "bp-actions",
                        button { class: "bp-choice primary", onclick: move |_| { let mut s = cfg(); s.ra_auto = true; save(s); ra_warn.set(false); }, {state::tr("Turn on")} }
                        button { class: "bp-choice", onclick: move |_| ra_warn.set(false), {state::tr("cancel")} }
                    }
                }
            }
            if c.ra_auto {
                div { class: "bp-actions",
                    for m in [15u32, 30, 60] {
                        button { key: "{m}", class: if c.ra_every_min == m { "bp-choice active" } else { "bp-choice" }, onclick: move |_| { let mut s = cfg(); s.ra_every_min = m; save(s); }, {state::trf("every {n} min", &[("n", &m.to_string())])} }
                    }
                }
            }
            p { class: "muted", {state::tr("Metadata and art downloads run on the server or client. Start one from a title's or release's menu (long press or Y); the phone never starts one by itself.")} }
        }
    }
    #[cfg(not(feature = "mobile"))]
    {
        rsx! {}
    }
}

/// The server (or client) to sync from, and signing in to it: the app's own sign-in form,
/// opened from the phone's Settings when the user wants one, never at start.
#[component]
pub fn ServerSheet() -> Element {
    #[cfg(feature = "mobile")]
    {
        let error = crate::api::NEEDS_SIGN_IN().unwrap_or_default();
        rsx! {
            super::bigpicture::SheetHead { title: state::tr("Server") }
            div { class: "bp-sheet",
                crate::screens::SignIn {
                    error,
                    embedded: true,
                    on_done: move |_| {
                        *crate::api::NEEDS_SIGN_IN.write() = None;
                        crate::ui::close_modal();
                        // The machines, tabs and settings the server knows, now that it answers.
                        spawn(async move { let _ = state::load().await; });
                    },
                }
            }
        }
    }
    #[cfg(not(feature = "mobile"))]
    {
        rsx! {}
    }
}
