//! romcompanion's own screens, inside Big Picture's frame: the Sync sheet (sync now, RA
//! progress now, how the last one went), the phone's settings (what to sync, and when), the
//! sync status line, and the Held filter's choices by machine.
//!
//! Everything here is the phone build's (`mobile`); the other builds get the plain Held
//! choices and nothing else, so Big Picture can name these without a cfg at every use.

use dioxus::prelude::*;

use crate::state;

/// The Held filter's choices: the catalogue's own views, or on the phone, each machine its
/// copy was synced with.
pub fn held_views() -> Vec<(String, String)> {
    #[cfg(feature = "mobile")]
    {
        use crate::api::companion;
        let mut out = vec![
            (String::new(), state::tr("anywhere")),
            ("held".to_string(), state::tr("held on a synced machine")),
        ];
        let chosen = companion::settings().options.machines;
        let machines: Vec<companion::Machine> = companion::copy_meta("machines")
            .and_then(|m| serde_json::from_str(&m).ok())
            .unwrap_or_default();
        for m in machines.into_iter().filter(|m| chosen.contains(&m.id)) {
            let (view, name) = if m.role == "this" {
                (
                    "local".to_string(),
                    state::tr("the machine this phone syncs from"),
                )
            } else {
                (format!("on:{}", m.id), m.name.clone())
            };
            out.push((view, state::trf("on {name}", &[("name", &name)])));
        }
        out
    }
    #[cfg(not(feature = "mobile"))]
    {
        [
            ("", "anywhere"),
            ("local", "held here"),
            ("server", "on the main server only"),
            ("missing_on_server", "not on the main server"),
        ]
        .into_iter()
        .map(|(v, l)| (v.to_string(), state::tr(l)))
        .collect()
    }
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
        use crate::api::companion::{self, LAST_ERROR, SYNCING};
        let s = companion::settings();
        let busy = SYNCING().is_some();
        let mut ra_busy = use_signal(|| false);
        let server = crate::api::endpoint().base;
        let summary = s.last_summary.clone();
        rsx! {
            h3 { {state::tr("Sync")} }
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
                div { class: "bp-sheet-actions", button { class: "bp-choice", onclick: move |_| crate::ui::close_modal(), {state::tr("close")} } }
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
        let machines = use_resource(|| async { companion::machines().await });
        let mut save = move |s: Settings| {
            companion::save_settings(&s);
            cfg.set(s);
            estimate.set(None);
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
        let platforms: Vec<(u16, String)> = state::platforms()
            .iter()
            .filter_map(|p| {
                Some((
                    p.get("id")?.as_u64()? as u16,
                    state::platform_label(p.get("name")?.as_str()?),
                ))
            })
            .filter(|(id, _)| *id > 0)
            .collect();
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
            h4 { {state::tr("Machines")} }
            p { class: "muted", {state::tr("Whose games count as held. Only their titles and releases are synced, unless the catalogue is added below.")} }
            match &*machines.read() {
                None => rsx! { div { class: "muted", {state::tr("loading…")} } },
                Some(Err(e)) => rsx! { div { class: "muted", {state::trf("Connect to the server to change this ({error}).", &[("error", &e.message)])} } },
                Some(Ok(list)) => rsx! {
                    for m in list.clone() {
                        {
                            let on = o.machines.contains(&m.id);
                            let id = m.id.clone();
                            let name = if m.role == "this" { state::tr("the machine this phone syncs from") } else { format!("{} ({})", m.name, state::tr(if m.role == "main" { "main server" } else { "shared client" })) };
                            rsx! {
                                button { key: "{m.id}", class: if on { "bp-choice active" } else { "bp-choice" },
                                    onclick: move |_| {
                                        let mut s = cfg();
                                        if on { s.options.machines.retain(|x| *x != id); } else { s.options.machines.push(id.clone()); }
                                        save(s);
                                    },
                                    if on { "✓ " } "{name} · " {state::trf("{n} games", &[("n", &m.games.to_string())])}
                                }
                            }
                        }
                    }
                },
            }
            h4 { {state::tr("Catalogue")} }
            button { class: if o.scope == "held" { "bp-choice active" } else { "bp-choice" }, onclick: move |_| { let mut s = cfg(); s.options.scope = "held".into(); save(s); }, {state::tr("Only what the machines hold")} }
            button { class: if o.scope == "catalog" { "bp-choice active" } else { "bp-choice" }, onclick: move |_| { let mut s = cfg(); s.options.scope = "catalog".into(); save(s); }, {state::tr("Also titles not owned yet, on these platforms:")} }
            if o.scope == "catalog" {
                div { class: "bp-choices",
                    for (id, name) in platforms {
                        {
                            let on = o.catalog_platforms.contains(&id);
                            rsx! {
                                button { key: "{id}", class: if on { "bp-choice active" } else { "bp-choice" },
                                    onclick: move |_| { let mut s = cfg(); if on { s.options.catalog_platforms.retain(|x| *x != id); } else { s.options.catalog_platforms.push(id); } save(s); },
                                    if on { "✓ " } "{name}"
                                }
                            }
                        }
                    }
                }
            }
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
                    onclick: move |_| { spawn(async move { let o = cfg().options; estimate.set(Some(companion::estimate(&o).await.map_err(|e| e.message))); }); },
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
                div { class: "bp-sheet-actions", button { class: "bp-choice", onclick: move |_| crate::ui::close_modal(), {state::tr("close")} } }
            }
        }
    }
    #[cfg(not(feature = "mobile"))]
    {
        rsx! {}
    }
}
