//! Data: providers (manual requests, coverage), DAT sources and imports, the database tables
//! (the console's `showProviders` / `showDats` / `showDb` / `showDbTable`).

use std::rc::Rc;

use dioxus::prelude::*;
use serde_json::{json, Value};

use super::backup::JobWatch;
use super::common::bump;
use super::jobs::enqueue;
use crate::api::{self, s, u};
use crate::components::{Column, Filter, Grid, GridSpec, Spec};
use crate::html::{self, badge, esc};
use crate::state;
use crate::ui::{self, Explain, Html};
use crate::Route;

fn num(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn pct(a: u64, b: u64) -> Option<f64> {
    (b > 0).then(|| ((1000.0 * a as f64 / b as f64).round() / 10.0).min(100.0))
}

/// How much of a provider is held here.
fn coverage_cell(c: &Value) -> Element {
    let cached = u(c, "cached");
    let fetched = api::uo(c, "fetched");
    let got = fetched.unwrap_or(cached);
    let remote = u(c, "remote_total");
    let mut head = state::trf("<b>{n}</b> cached", &[("n", &num(cached))]);
    if let Some(f) = fetched.filter(|f| *f != cached) {
        head.push_str(&state::trf(" · {n} fetched", &[("n", &num(f))]));
    }
    if let Some(p) = pct(got, remote) {
        head.push_str(&state::trf(" · <b>{pct}%</b> of {n}", &[("pct", &p.to_string()), ("n", &num(remote))]));
    }
    if api::b(c, "full") {
        head.push_str(&format!(" {}", badge(&state::tr("full sync"), "ok")));
    }
    let platforms = api::arr(c, "platforms").to_vec();
    if platforms.is_empty() {
        return rsx! { Html { html: head } };
    }
    let full = platforms.iter().filter(|p| api::b(p, "complete")).count();
    let summary = if platforms.len() == 1 {
        state::trf("{n} platform ({full} full)", &[("n", &platforms.len().to_string()), ("full", &full.to_string())])
    } else {
        state::trf("{n} platforms ({full} full)", &[("n", &platforms.len().to_string()), ("full", &full.to_string())])
    };
    rsx! {
        Html { html: head }
        details {
            summary { "{summary}" }
            table { class: "grid",
                thead { tr { th { {state::tr("platform")} } th { {state::tr("entries")} } th { {state::tr("list")} } th { {state::tr("synced")} } } }
                tbody {
                    for p in platforms.iter() {
                        {
                            let entries = u(p, "entries");
                            let rt = u(p, "remote_total");
                            let list = if api::b(p, "complete") {
                                badge(&state::tr("full list"), "ok")
                            } else if let Some(x) = pct(entries, rt) {
                                format!("{} <span class=\"muted\">{}</span>", badge(&state::trf("partial {pct}%", &[("pct", &x.to_string())]), "warn"), state::trf("of {n}", &[("n", &num(rt))]))
                            } else {
                                badge(&state::tr("partial"), "warn")
                            };
                            rsx! {
                                tr {
                                    td { "{s(p, \"platform_name\")}" }
                                    td { class: "num", "{num(entries)}" }
                                    td { Html { html: list } }
                                    td { class: "muted", "{html::fmt_date(&s(p, \"synced_at\"))}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
pub fn DataProviders() -> Element {
    let mut ps = use_resource(|| async { api::get("/api/v1/providers").await.map(|v| api::items(&v)) });
    let list = match &*ps.read() {
        None => return rsx! { div { class: "muted", {state::tr("loading…")} } },
        Some(Err(e)) => return rsx! { div { class: "card bad", "{e.message}" } },
        Some(Ok(l)) => l.clone(),
    };
    let caps = |p: &Value| p.get("capabilities").cloned().unwrap_or_default();
    let scope_badges = |c: &Value| {
        let v = [
            if api::b(c, "keyword") { badge(&if api::b(c, "keyword_needs_platform") { state::tr("keyword (per platform)") } else { state::tr("keyword") }, "ok") } else { String::new() },
            if api::b(c, "platform") { badge(&state::tr("platform"), "ok") } else { String::new() },
            if api::b(c, "full") { badge(&state::tr("full"), "ok") } else { String::new() },
        ]
        .join("");
        if v.is_empty() { "<span class=\"muted\">—</span>".to_string() } else { v }
    };
    rsx! {
        ProviderRequest { list: list.clone() }
        div { class: "card",
            h2 { {state::tr("Providers")} }
            table { class: "grid",
                thead { tr { th { {state::tr("provider")} } th { {state::tr("available")} } th { {state::tr("tier")} } th { {state::tr("scopes")} } th { {state::tr("coverage")} } th { {state::tr("quota")} } th { {state::tr("limits / notes")} } } }
                tbody {
                    for p in list.iter() {
                        {
                            let c = caps(p);
                            let avail = api::b(p, "available");
                            let reason = s(p, "reason");
                            rsx! {
                                tr {
                                    td { Html { html: html::provider_icon(&s(p, "name")) } " {s(p, \"name\")}" }
                                    td { if avail { Html { html: badge(&state::tr("yes"), "ok") } } else { span { title: "{reason}", Html { html: badge(&state::tr("no"), "bad") } } } }
                                    td { "{s(p, \"tier\")}" }
                                    td { Html { html: scope_badges(&c) } }
                                    td { style: "white-space:normal;min-width:220px", {coverage_cell(p.get("coverage").unwrap_or(&Value::Null))} }
                                    td { "{s(p, \"quota_remaining\")} " if let Some(r) = api::so(p, "quota_resets_at") { {state::trf("(resets {date})", &[("date", &html::fmt_date(&r))])} } }
                                    td { class: "muted", style: "white-space:normal;max-width:520px", if avail { "{s(&c, \"notes\")}" } else { "{reason}" } }
                                }
                            }
                        }
                    }
                }
            }
            ui::Explain { p { {state::tr("Credentials are kept in assets/api.env on the server, sealed with its master key; they are never shown here.")} } }
        }
        if api::is_admin() { Credentials { on_change: move |_| ps.restart() } }
    }
}

/// Config › Provider credentials: the same card as on Data › Providers.
#[component]
pub fn ConfigCredentials() -> Element {
    if !api::is_admin() {
        return rsx! { div { class: "card muted", {state::tr("Only an administrator can change provider credentials.")} } };
    }
    rsx! { Credentials { on_change: move |_| {} } }
}

/// The provider "Manual request" form: on Data › Providers, and again on Jobs. `list` is
/// `/api/v1/providers`.
#[component]
pub fn ProviderRequest(list: Vec<Value>) -> Element {
    let mut provider = use_signal(String::new);
    let mut mode = use_signal(String::new);
    let mut name = use_signal(String::new);
    let mut platform = use_signal(String::new);
    let mut max_pages = use_signal(String::new);
    let mut redownload = use_signal(|| false);
    // "bulk: checked platforms": each ticked platform is queued as its own one-platform job.
    let mut checked = use_signal(std::collections::BTreeSet::<u64>::new);
    // Ticked to start with: the platforms the DAT sync setting stands for (Settings ›
    // dat_sync_platforms), so a bulk sync covers what you have.
    use_future(move || async move {
        if let Ok(v) = api::get("/api/v1/dat_platforms").await {
            if let Some(ids) = v.get("platforms").and_then(Value::as_array) {
                checked.set(ids.iter().filter_map(Value::as_u64).collect());
            }
        }
    });
    let caps = |p: &Value| p.get("capabilities").cloned().unwrap_or_default();
    let usable: Vec<Value> = list.iter().filter(|p| api::b(p, "available") && { let c = caps(p); api::b(&c, "keyword") || api::b(&c, "platform") || api::b(&c, "full") }).cloned().collect();
    let cur = usable.iter().find(|p| s(p, "name") == provider()).or(usable.first()).cloned();
    let c = cur.as_ref().map(caps).unwrap_or_default();
    let pname = cur.as_ref().map(|p| s(p, "name")).unwrap_or_default();
    // The platforms this provider has, when it only has some: the others are not offered.
    let only: Option<Vec<u64>> = cur.as_ref().and_then(|p| p.get("bulk_platforms")).and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_u64()).collect());
    let offered = { let only = only.clone(); move |id: u64| only.as_ref().is_none_or(|o| o.contains(&id)) };
    let offered2 = offered.clone();
    let only_note = only.as_ref().map(|o| state::trf("{provider} has {n} of these platforms; the others are not offered.", &[("provider", &pname), ("n", &o.len().to_string())]));
    let mut modes: Vec<(&str, String)> = Vec::new();
    if api::b(&c, "keyword") {
        modes.push(("keyword", state::tr("keyword search")));
    }
    if api::b(&c, "platform") {
        modes.push(("platform", state::tr("bulk: checked platforms")));
        modes.push(("all", state::tr("bulk: every catalog platform")));
    }
    if api::b(&c, "full") {
        modes.push(("full", state::tr("bulk: full database")));
    }
    let m = if modes.iter().any(|(v, _)| *v == mode()) { mode() } else { modes.first().map(|(v, _)| v.to_string()).unwrap_or_default() };
    let needs_platform = api::b(&c, "keyword_needs_platform");
    let show_pages = (m == "platform" || m == "all") && ["igdb", "thegamesdb", "emuready"].contains(&pname.as_str());
    let notes = s(&c, "notes");
    let (m2, pname2, notes2) = (m.clone(), pname.clone(), notes.clone());
    rsx! {
        div { class: "card",
            h2 { {state::tr("Metadata request")} }
            ui::Explain { p { {state::tr("Runs as a job on the server's worker (progress under Jobs). ")} b { {state::tr("Keyword")} } {state::tr(" looks a name up and links the best match. ")} b { {state::tr("Platform")} } {state::tr(" pulls one platform's whole catalog from the provider (or every platform your catalog has). ")} b { {state::tr("Full")} } {state::tr(" pulls the provider's whole database — offered only where its API allows that. Bulk results link to your catalog by exact name (or by hash, for Hasheous); keyword results may link a close match.")} } }
            if usable.is_empty() {
                span { class: "muted", {state::tr("No provider available — check api.env.")} }
            } else {
                if let Some(note) = only_note { p { class: "muted", "{note}" } }
                form { class: "inline",
                    onsubmit: move |e| {
                        e.prevent_default();
                        let plat = platform().parse::<u64>().ok();
                        let (mode, provider, notes) = (m2.clone(), pname2.clone(), notes2.clone());
                        // A platform ticked for another provider is not sent to one that lacks it.
                        let (kw, pages, redl, ids) = (name(), max_pages(), redownload(), checked().into_iter().filter(|id| offered2(*id)).collect::<std::collections::BTreeSet<u64>>());
                        spawn(async move {
                            if mode == "keyword" {
                                if kw.is_empty() { ui::toast(state::tr("type a keyword"), true); return; }
                                if needs_platform && plat.is_none() { ui::toast(state::trf("{provider} searches per platform: choose one", &[("provider", &provider)]), true); return; }
                                let mut body = json!({ "job_type": "provider_query", "provider": provider, "name": kw });
                                if let Some(p) = plat { body["platform"] = json!(p); }
                                enqueue(body).await;
                                return;
                            }
                            let mut body = json!({ "job_type": "provider_bulk_sync", "provider": provider, "mode": if redl { "redownload" } else { "diff" } });
                            if let Ok(n) = pages.parse::<u64>() { body["max_pages"] = json!(n); }
                            match mode.as_str() {
                                "full" => { if !ui::confirm(&state::trf("Pull {provider}'s whole database?\n\n{notes}", &[("provider", &provider), ("notes", &notes)])).await { return; } body["full"] = json!(true); }
                                "all" => { if !ui::confirm(&state::trf("Sync {provider} for every platform your catalog has releases for? This can take a long time and use the provider's quota.", &[("provider", &provider)])).await { return; } body["platform"] = json!("all"); }
                                _ => {
                                    if ids.is_empty() { ui::toast(state::tr("check at least one platform"), true); return; }
                                    // From "+ queue", every platform's job waits its turn, not only the first.
                                    let in_turn = api::take_queue_next();
                                    for id in ids {
                                        let mut one = body.clone();
                                        one["platform"] = json!(id);
                                        api::queue_next(in_turn);
                                        enqueue(one).await;
                                    }
                                    return;
                                }
                            }
                            enqueue(body).await;
                        });
                    },
                    label { {state::tr("provider")}
                        select { onchange: move |e| provider.set(e.value()),
                            for p in usable.iter() { option { value: "{s(p, \"name\")}", selected: s(p, "name") == pname, "{s(p, \"name\")}" } }
                        }
                    }
                    label { {state::tr("request")}
                        select { onchange: move |e| mode.set(e.value()),
                            for (v, l) in modes.clone() { option { value: "{v}", selected: v == m, "{l}" } }
                        }
                    }
                    if m == "keyword" { label { {state::tr("keyword")} input { r#type: "text", placeholder: state::tr("game name"), style: "min-width:200px", value: "{name}", oninput: move |e| name.set(e.value()) } } }
                    if m == "keyword" {
                        label { {state::tr("platform")}
                            select { onchange: move |e| platform.set(e.value()),
                                option { value: "", if m == "keyword" && !needs_platform { {state::tr("any platform")} } else { {state::tr("— choose a platform —")} } }
                                for p in state::platforms().into_iter().filter(|p| offered(u(p, "id"))) { option { value: "{u(&p, \"id\")}", selected: u(&p, "id").to_string() == platform(), "{s(&p, \"name\")}" } }
                            }
                        }
                    }
                    if show_pages { label { {state::tr("max pages")} input { r#type: "number", min: "1", style: "width:80px", placeholder: state::tr("all"), value: "{max_pages}", oninput: move |e| max_pages.set(e.value()) } } }
                    if m != "keyword" {
                        label { title: state::tr("By default a bulk sync fetches only what changed since the last complete sync (a dump that did not change is not downloaded at all). Tick to fetch everything again."),
                            span { {state::tr("redownload everything")} }
                            input { r#type: "checkbox", checked: redownload(), onchange: move |e| redownload.set(e.checked()) }
                        }
                    }
                    button { class: "btn primary", {state::tr("send")} }
                    crate::ui::QueueButton {}
                    if m == "platform" {
                        super::widgets::PlatformPicker { checked, offered: only.clone() }
                    }
                }
                p { class: "muted", style: "margin-top:8px", "{notes}" }
            }
        }
    }
}

/// Data › Providers: the credentials each provider needs. Values are write-only: the form
/// shows which keys are set and takes new ones; saving an empty field clears that key.
#[component]
/// `scope`: `providers` (the default: Data › Providers, Config › Provider credentials) or
/// `ai` (Config › AI).
pub fn Credentials(on_change: EventHandler<()>, scope: Option<String>) -> Element {
    let scope = scope.unwrap_or_else(|| "providers".to_string());
    let mut fields = use_resource(use_reactive!(|scope| async move { api::get(&format!("/api/v1/providers/credentials?scope={}", api::encode(&scope))).await.map(|v| api::items(&v)) }));
    let mut edits = use_signal(std::collections::BTreeMap::<String, String>::new);
    let mut busy = use_signal(|| false);
    let list = match &*fields.read() {
        None => return rsx! { div { class: "card", h2 { {state::tr("Credentials")} } div { class: "muted", {state::tr("loading…")} } } },
        Some(Err(e)) => return rsx! { div { class: "card bad", "{e.message}" } },
        Some(Ok(l)) => l.clone(),
    };
    let mut providers: Vec<String> = Vec::new();
    for f in &list {
        let p = s(f, "provider");
        if !providers.contains(&p) {
            providers.push(p);
        }
    }
    rsx! {
        div { class: "card",
            h2 { {state::tr("Credentials")} }
            ui::Explain { p { {state::tr("Type a value to set it, or leave a field blank to keep what is there. To clear a key, press its × and save. Saved values take effect on the next request or job, no restart needed.")} } }
            form {
                onsubmit: move |e| {
                    e.prevent_default();
                    let values = edits();
                    if values.is_empty() { ui::toast(state::tr("nothing changed"), false); return; }
                    busy.set(true);
                    spawn(async move {
                        match api::put("/api/v1/providers/credentials", json!(values)).await {
                            Ok(_) => { ui::toast(state::tr("credentials saved"), false); edits.set(Default::default()); fields.restart(); on_change.call(()); }
                            Err(e) => ui::toast(e.message, true),
                        }
                        busy.set(false);
                    });
                },
                for p in providers {
                    div { style: "margin:8px 0",
                        h3 { Html { html: html::provider_icon(&p) } " {p}" }
                        table { class: "grid", tbody {
                            for f in list.iter().filter(|f| s(f, "provider") == p) {
                                {
                                    let key = s(f, "key");
                                    let set = api::b(f, "set");
                                    let secret = api::b(f, "secret");
                                    let required = api::b(f, "required");
                                    let pending = edits().get(&key).cloned();
                                    let clearing = pending.as_deref() == Some("");
                                    let (k1, k2) = (key.clone(), key.clone());
                                    rsx! {
                                        tr {
                                            td { "{s(f, \"label\")}" if !required { span { class: "muted", {state::tr(" (optional)")} } } }
                                            td { Html { html: if clearing { badge(&state::tr("will be cleared"), "warn") } else if pending.is_some() { badge(&state::tr("changed"), "warn") } else if set { badge(&state::tr("set"), "ok") } else { badge(&state::tr("not set"), "dim") } } }
                                            td {
                                                input { r#type: if secret { "password" } else { "text" }, autocomplete: "off", style: "min-width:320px", placeholder: if set { state::tr("(unchanged)") } else { String::new() },
                                                    value: "{pending.clone().unwrap_or_default()}",
                                                    oninput: move |e| { let mut m = edits(); if e.value().is_empty() { m.remove(&k1); } else { m.insert(k1.clone(), e.value()); } edits.set(m); } }
                                            }
                                            td { if set { button { class: "btn small danger", r#type: "button", title: state::tr("clear this key on save"), onclick: move |_| { let mut m = edits(); m.insert(k2.clone(), String::new()); edits.set(m); }, "×" } } }
                                        }
                                    }
                                }
                            }
                        } }
                    }
                }
                div { class: "toolbar actions", button { class: "btn primary", disabled: busy(), {state::tr("save")} } }
            }
        }
    }
}

/// A downloaded DAT file, or a bundle of them (`name` absent; `source` absent for every
/// source). Over the wire it is a link the browser saves; in-process there is no browser
/// download to hand it to, so it asks for a folder (the OS dialog) and copies it there
/// (`POST /api/v1/dats/export`).
#[component]
fn DatDownload(source: Option<String>, name: Option<String>, label: String, #[props(default)] class: String) -> Element {
    #[cfg(feature = "local")]
    {
        let class = if class.is_empty() { "link".to_string() } else { class };
        // The folder lives here, not in the dialog: the in-page folder browser is a modal of
        // its own and replaces the dialog while it is open, so picking a folder there reopens
        // the dialog with it.
        let dir = use_signal(String::new);
        let mut saving = use_signal(|| false);
        let show = {
            let (source, name) = (source.clone(), name.clone());
            move || {
                let (source, name) = (source.clone(), name.clone());
                ui::open_modal(move || rsx! { DatSave { source: source.clone(), name: name.clone(), dir, saving } });
            }
        };
        let reopen = show.clone();
        use_effect(move || {
            let _ = dir();
            if *saving.peek() && !ui::modal_is_open() {
                reopen();
            }
        });
        rsx! {
            span { class: "{class}", title: state::tr("save a copy into a folder you choose"),
                onclick: move |_| {
                    saving.set(true);
                    show();
                },
                "{label}"
            }
        }
    }
    #[cfg(not(feature = "local"))]
    {
        let path = match (&source, &name) {
            (Some(src), Some(n)) => format!("/api/v1/dats/files/{}/{}", api::encode(src), n.split('/').map(api::encode).collect::<Vec<_>>().join("/")),
            (Some(src), None) => format!("/api/v1/dats/bundle?source={}", api::encode(src)),
            _ => "/api/v1/dats/bundle".to_string(),
        };
        let href = api::media_url(&path);
        let class = if class.is_empty() { "link".to_string() } else { class };
        rsx! { a { class: "{class}", href: "{href}", download: "", "{label}" } }
    }
}

/// Where the desktop app saves a DAT file or bundle: a folder picked with the OS dialog or
/// the in-page browser (`config::PathInput`, so it works without a desktop portal too), then
/// `POST /api/v1/dats/export`. `dir` and `saving` belong to the [`DatDownload`] that opened it.
#[cfg(feature = "local")]
#[component]
fn DatSave(source: Option<String>, name: Option<String>, dir: Signal<String>, saving: Signal<bool>) -> Element {
    let mut saving = saving;
    let mut busy = use_signal(|| false);
    let what = match (&source, &name) {
        (_, Some(n)) => n.clone(),
        (Some(src), None) => state::trf("every {source} DAT file, as one zip", &[("source", src)]),
        _ => state::tr("every downloaded DAT file, as one zip"),
    };
    rsx! {
        h2 { {state::tr("Save DAT files")} }
        p { class: "mono", "{what}" }
        div { class: "toolbar", super::config::PathInput { value: dir, placeholder: state::tr("folder to save into") } }
        div { class: "toolbar actions",
            button { r#type: "button", class: "btn", onclick: move |_| { saving.set(false); ui::close_modal(); }, {state::tr("cancel")} }
            button { r#type: "button", class: "btn primary", disabled: busy() || dir().trim().is_empty(),
                onclick: move |_| {
                    let body = json!({ "dir": dir().trim(), "source": source.clone(), "name": name.clone() });
                    busy.set(true);
                    spawn(async move {
                        match api::post("/api/v1/dats/export", body).await {
                            Ok(r) => {
                                ui::toast(state::trf("saved {path}", &[("path", &s(&r, "path"))]), false);
                                saving.set(false);
                                ui::close_modal();
                            }
                            Err(e) => ui::toast(e.message, true),
                        }
                        busy.set(false);
                    });
                },
                {state::tr("save")}
            }
        }
    }
}

#[component]
pub fn DataDats() -> Element {
    let reload = use_signal(|| 0u64);
    let data = use_resource(move || async move {
        let _ = reload();
        let sources = api::get("/api/v1/dats/sources").await.map(|v| api::items(&v))?;
        let imports = api::get("/api/v1/dats/imports?limit=200").await.map(|v| api::items(&v))?;
        let files = api::get("/api/v1/dats/files").await.map(|v| api::items(&v))?;
        Ok::<_, api::ApiError>((sources, imports, files))
    });
    let watch = use_signal(|| None::<u64>);
    let dat_plats = super::widgets::use_dat_platforms();
    let mut up_source = use_signal(String::new);
    let mut up_file = use_signal(|| None::<dioxus::html::FileData>);
    let mut uploading = use_signal(|| false);
    let (sources, imports, files) = match &*data.read() {
        None => return rsx! { div { class: "muted", {state::tr("loading…")} } },
        Some(Err(e)) => return rsx! { div { class: "card bad", "{e.message}" } },
        Some(Ok(v)) => v.clone(),
    };
    let post = move |body: Value| {
        let mut watch = watch;
        spawn(async move {
            match api::post("/api/v1/jobs", body).await {
                Ok(r) => watch.set(Some(u(&r, "id"))),
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    let run = move |body: Value| {
        if let Some(body) = super::widgets::with_dat_platforms(dat_plats, body) {
            post(body);
        }
    };
    let total: u64 = files.iter().map(|f| u(f, "bytes")).sum();
    let source_names: Vec<String> = sources.iter().map(|x| s(x, "name")).collect();
    rsx! {
        div { class: "card",
            h2 { {state::tr("Platforms")} }
            super::widgets::DatPlatformPicker { pick: dat_plats }
        }
        div { class: "card",
            h2 { {state::tr("DAT sources")} " " ui::KeyExplain { keys: vec!["explain.data.dat_sources".to_string()] } }
            ui::Explain { p { {state::tr("\"declared\" is what the DAT headers count (games / roms or files); \"in database\" is what the import produced under that source: titles, releases (one per game entry, discs rolled up) and content rows (one per distinct file).")} } }
            table { class: "grid",
                thead {
                    tr { th { rowspan: "2", {state::tr("source")} } th { rowspan: "2", {state::tr("imports")} } th { colspan: "2", {state::tr("declared")} } th { colspan: "3", {state::tr("in database")} } th { rowspan: "2", {state::tr("last import")} } th { rowspan: "2", {state::tr("downloaded")} } th { rowspan: "2" } }
                    tr { th { {state::tr("games")} } th { {state::tr("files")} } th { {state::tr("titles")} } th { {state::tr("releases")} } th { {state::tr("content")} } }
                }
                tbody {
                    for x in sources {
                        {
                            let name = s(&x, "name");
                            let mine: Vec<&Value> = files.iter().filter(|f| s(f, "source") == name).collect();
                            let held = mine.len();
                            let held_bytes: u64 = mine.iter().map(|f| u(f, "bytes")).sum();
                            let (n2, n3) = (name.clone(), name.clone());
                            rsx! {
                                tr {
                                    td { Html { html: html::cbadge("source", &name, None, None) } }
                                    td { class: "num", "{s(&x, \"imports\")}" }
                                    td { class: "num", "{s(&x, \"games\")}" }
                                    td { class: "num", "{s(&x, \"files\")}" }
                                    td { class: "num", "{s(&x, \"titles_in_db\")}" }
                                    td { class: "num", "{s(&x, \"releases_in_db\")}" }
                                    td { class: "num", "{s(&x, \"content_in_db\")}" }
                                    td { "{html::fmt_date(&s(&x, \"last_imported_at\"))}" }
                                    td { class: "num", if held > 0 { "{held} · {html::fmt_size(Some(held_bytes))}" } else { span { class: "muted", "—" } } }
                                    td { style: "white-space:nowrap",
                                        button { class: "btn small", title: state::tr("download the current DATs, then import what changed"), onclick: move |_| run(json!({ "job_type": "dat_sync", "source": n2.clone(), "import": true })), {state::tr("sync")} }
                                        crate::ui::QueueButton {}
                                        " "
                                        button { class: "btn small", disabled: held == 0, title: state::tr("import the downloaded files without the network"), onclick: move |_| super::widgets::ask_dat_import(json!({ "job_type": "dat_sync", "source": n3.clone(), "offline": true }), post), {state::tr("import downloaded")} }
                                        crate::ui::QueueButton {}
                                        " "
                                        if held > 0 { DatDownload { source: Some(name.clone()), name: None, label: state::tr("download") } }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        div { class: "card",
            h2 { {state::tr("Downloaded DAT files")} }
            Explain {
                p { {state::tr("Every DAT a sync downloads stays here, so a clean install does not need the network again: download them all at once (one zip, a folder per source) or one by one, keep that somewhere, and on the new install upload it and press \"import all downloaded\".")} }
                p { {state::tr("Upload takes either that zip, unpacked into each source's folder, or a single DAT file (.dat, .xml, .json, or a .zip pack) for the source chosen beside it. A file of the same name is replaced. Nothing is imported until you ask.")} }
                p { {state::tr("An offline import reads only the newest copy of each DAT and skips one already imported. Older copies are marked \"superseded\": kept as history, never imported.")} }
            }
            div { class: "toolbar",
                if !files.is_empty() {
                    DatDownload { source: None, name: None, class: "btn".to_string(), label: state::trf("download all ({n} files, {size})", &[("n", &files.len().to_string()), ("size", &html::fmt_size(Some(total)))]) }
                }
                button { class: "btn", disabled: files.is_empty(), onclick: move |_| super::widgets::ask_dat_import(json!({ "job_type": "dat_sync", "offline": true }), post), {state::tr("import all downloaded (offline)")} }
                crate::ui::QueueButton {}
                span { class: "grow" }
                select { value: "{up_source}", onchange: move |e| up_source.set(e.value()),
                    option { value: "", {state::tr("source: from the bundle")} }
                    for n in source_names { option { value: "{n}", "{n}" } }
                }
                input { r#type: "file", accept: ".zip,.dat,.xml,.json", onchange: move |e| up_file.set(e.files().into_iter().next()) }
                button { class: "btn", disabled: uploading(),
                    onclick: move |_| {
                        let Some(f) = up_file() else { ui::toast(state::tr("choose a file"), true); return; };
                        let src = up_source().trim().to_string();
                        uploading.set(true);
                        spawn(async move {
                            match api::add_dat(f, (!src.is_empty()).then_some(src)).await {
                                Ok(r) => {
                                    let n = r.get("files").and_then(|v| v.as_array()).map_or(0, Vec::len);
                                    ui::toast(state::trf("stored {n} DAT file(s)", &[("n", &n.to_string())]), false);
                                    bump(reload);
                                }
                                Err(e) => ui::toast(e.message, true),
                            }
                            uploading.set(false);
                        });
                    },
                    if uploading() { {state::tr("uploading…")} } else { {state::tr("upload")} }
                }
            }
            if let Some(id) = watch() { JobWatch { key: "{id}", job_id: id, on_done: move |_| bump(reload) } }
            table { class: "grid",
                thead { tr { th { {state::tr("source")} } th { {state::tr("file")} } th { class: "num", {state::tr("size")} } th { {state::tr("modified")} } th {} } }
                tbody {
                    if files.is_empty() { tr { td { colspan: "5", class: "muted", {state::tr("none yet — a sync downloads them, or upload them here")} } } }
                    for f in files.clone() {
                        tr { key: "{s(&f, \"source\")}/{s(&f, \"name\")}",
                            td { Html { html: html::cbadge("source", &s(&f, "source"), None, None) } }
                            td { class: "mono", "{s(&f, \"name\")} "
                                if !api::b(&f, "latest") { Html { html: badge(&state::tr("superseded"), "dim") } }
                            }
                            td { class: "num", "{html::fmt_size(Some(u(&f, \"bytes\")))}" }
                            td { "{html::fmt_date(&s(&f, \"modified\"))}" }
                            td { DatDownload { source: Some(s(&f, "source")), name: Some(s(&f, "name")), label: state::tr("download") } }
                        }
                    }
                }
            }
        }
        div { class: "card",
            h2 { {state::tr("Recent imports")} " " {ui::key_explain("explain.data.imports")} }
            table { class: "grid",
                thead { tr { th { {state::tr("when")} } th { {state::tr("source")} } th { {state::tr("platform")} } th { {state::tr("file")} } th { {state::tr("dat")} } th { {state::tr("version")} } th { {state::tr("games")} } th { {state::tr("files")} } } }
                tbody {
                    if imports.is_empty() { tr { td { colspan: "8", class: "muted", {state::tr("none")} } } }
                    for i in imports {
                        tr {
                            td { "{html::fmt_date(&s(&i, \"imported_at\"))}" }
                            td { Html { html: html::cbadge("source", &s(&i, "source"), None, None) } }
                            td { Html { html: html::cbadge("platform", &s(&i, "platform"), None, None) } }
                            td { class: "mono", "{s(&i, \"file_name\")}" }
                            td { "{s(&i, \"dat_name\")}" }
                            td { "{s(&i, \"dat_version\")}" }
                            td { class: "num", "{s(&i, \"game_count\")}" }
                            td { class: "num", "{s(&i, \"file_count\")}" }
                        }
                    }
                }
            }
        }
    }
}

/// Downloads a text file the server generates (a CSV) as `name`.
///
/// Fetched through the backend like any other call -- in-process on desktop, where there is
/// no HTTP server for the webview to `fetch` from, and over the signed-in session elsewhere --
/// so no token is handed to page script. The webview only turns the text into a download.
async fn download(path: &str, name: &str) {
    match api::backend::backend().call(api::Method::Get, path, None).await {
        Ok(text) => save_text(&text, name, "text/csv").await,
        Err(e) => ui::toast(e.message, true),
    }
}

/// Hands `text` to the webview as a download called `name`.
pub(super) async fn save_text(text: &str, name: &str, mime: &str) {
    let js = format!(
        r#"try {{ const u = URL.createObjectURL(new Blob([{text}], {{ type: {mime} }})); const a = document.createElement('a'); a.href = u; a.download = {name}; document.body.appendChild(a); a.click(); a.remove(); setTimeout(() => URL.revokeObjectURL(u), 1000); return ''; }} catch (e) {{ return String(e); }}"#,
        text = serde_json::to_string(text).unwrap_or_default(),
        mime = serde_json::to_string(&format!("{mime};charset=utf-8")).unwrap_or_default(),
        name = serde_json::to_string(name).unwrap_or_default()
    );
    if let Ok(v) = document::eval(&js).await {
        if let Some(err) = v.as_str().filter(|e| !e.is_empty()) {
            ui::toast(err.to_string(), true);
        }
    }
}

// --- db optimize: the informed confirmation ---

/// Opens the purge dialog. `after` reloads whatever list the caller is showing once the job
/// is enqueued.
///
/// Not `ui::confirm`: that is the webview's own one-line `confirm()` (`ui.rs`), which cannot
/// show the counts that make this decision an informed one. The shape here is `mode.rs`'s
/// `ConsentDialog` -- an explaining modal, a checkbox gating the danger button, then
/// `ui::confirm` as the last click -- this codebase's established idiom for destructive and
/// irreversible.
pub fn db_optimize_dialog(after: Option<Rc<dyn Fn()>>) {
    ui::open_modal(move || rsx! { DbOptimizeDialog { after: super::common::DoneFn(after.clone()) } });
}

#[component]
fn DbOptimizeDialog(after: super::common::DoneFn) -> Element {
    let preview = use_resource(|| async { api::get("/api/v1/db/optimize/preview").await });
    let mut understood = use_signal(|| false);
    let p = match &*preview.read() {
        None => None,
        Some(Err(e)) => return rsx! {
            h2 { {state::tr("Purge soft-deleted rows?")} }
            div { class: "card bad", "{e.message}" }
            div { class: "fsfoot", span { class: "grow" } button { r#type: "button", class: "btn", onclick: move |_| ui::close_modal(), {state::tr("close")} } }
        },
        Some(Ok(v)) => Some(v.clone()),
    };
    let found = p.as_ref().map(|v| u(v, "missing_found")).unwrap_or(0);
    let total = p.as_ref().map(|v| u(v, "total")).unwrap_or(0);
    let rows: Vec<(String, u64)> = match &p {
        Some(v) => vec![
            (state::tr("files"), u(v, "files")),
            (state::tr("roms"), u(v, "roms")),
            (state::tr("games"), u(v, "games")),
            (state::tr("hashes"), u(v, "hashes")),
            (state::tr("contents"), u(v, "contents")),
            (state::tr("finished jobs older than 30 days"), u(v, "jobs")),
        ],
        None => Vec::new(),
    };
    rsx! {
        h2 { {state::tr("Purge soft-deleted rows?")} }
        p { {state::tr("Purging soft-deleted rows is ")} b { {state::tr("not")} } {state::tr(" recommended, and nothing in romexplorer needs it.")} }
        p {
            {state::tr("A row marked deleted is not junk — it is the record of a file this machine once held. It is what lets romexplorer recognise a file you moved, renamed, re-archived or restored from a backup as the ")}
            i { {state::tr("same")} }
            {state::tr(" file instead of re-hashing it as a stranger and losing its release, its RetroAchievements hash and its place in a game.")}
        }
        p {
            {state::tr("It is also the whole basis of the \"missing, but found elsewhere\" hint: ")}
            b { "{num(found)}" }
            {state::tr(" of your missing files are currently traceable to bytes you still have, and that hint disappears the moment those rows go.")}
        }
        p { class: "muted", {state::tr("Purging frees database rows, not disk space — your ROMs are never touched. It is worth doing only when the database itself has grown beyond the space you have for it.")} }
        if p.is_none() {
            div { class: "muted", {state::tr("counting…")} }
        } else {
            div { class: "kv", style: "grid-template-columns:max-content 1fr; margin:8px 0",
                for (label, n) in rows {
                    div { "{label}" }
                    div { class: "num", "{num(n)}" }
                }
            }
            p { {state::tr("This run would permanently delete ")} b { "{num(total)}" } {state::tr(" rows. This cannot be undone. Back up first (Config › Backup) if you are unsure.")} }
            label { style: "display:block; margin:8px 0",
                input { r#type: "checkbox", checked: understood(), onchange: move |e| understood.set(e.checked()) }
                {state::tr(" I understand these rows cannot be recovered")}
            }
        }
        div { class: "fsfoot",
            span { class: "grow" }
            button { r#type: "button", class: "btn", onclick: move |_| ui::close_modal(), {state::tr("cancel")} }
            button { r#type: "button", class: "btn danger", disabled: p.is_none() || !understood(),
                onclick: move |_| {
                    let after = after.0.clone();
                    spawn(async move {
                        if !ui::confirm(&state::trf("Permanently delete {n} soft-deleted rows? This cannot be undone.", &[("n", &num(total))])).await { return; }
                        ui::close_modal();
                        if enqueue(json!({ "job_type": "db_optimize" })).await {
                            if let Some(a) = after { a(); }
                        }
                    });
                },
                {state::tr("purge anyway")}
            }
            crate::ui::QueueButton {}
        }
    }
}

#[component]
pub fn DataDb() -> Element {
    let tables = use_resource(|| async { api::get("/api/v1/db/tables").await.map(|v| api::items(&v)) });
    let mut busy = use_signal(|| None::<&'static str>);
    let list = match &*tables.read() {
        None => return rsx! { div { class: "muted", {state::tr("loading…")} } },
        Some(Err(e)) => return rsx! { div { class: "card bad", "{e.message}" } },
        Some(Ok(l)) => l.clone(),
    };
    let mut cp = move |kind: &'static str| {
        busy.set(Some(kind));
        spawn(async move {
            download(&format!("/api/v1/dev/counterparts/{kind}.csv"), &format!("counterparts_{kind}.csv")).await;
            busy.set(None);
        });
    };
    rsx! {
        div { class: "card",
            h2 { {state::tr("Maintenance")} " " {ui::key_explain("explain.data.maintenance")} }
            div { class: "toolbar",
                button { class: "btn", disabled: busy().is_some(), title: state::tr("release -> USA/English counterpart per platform, DAT names (CSV; Japan-only releases name themselves, others without a counterpart are left empty)"), onclick: move |_| cp("full"), if busy() == Some("full") { {state::tr("building…")} } else { {state::tr("counterparts CSV (full names)")} } }
                button { class: "btn", disabled: busy().is_some(), title: state::tr("the same with region/language/revision tags removed, duplicates collapsed"), onclick: move |_| cp("stripped"), if busy() == Some("stripped") { {state::tr("building…")} } else { {state::tr("counterparts CSV (stripped)")} } }
                button { class: "btn", onclick: move |_| { spawn(async move { if ui::confirm(&state::tr("Download every DAT source and import what changed? (network + a few minutes)")).await { enqueue(json!({ "job_type": "dat_sync", "import": true })).await; } }); }, {state::tr("DAT sync — all sources (download + import)")} }
                crate::ui::QueueButton {}
                span { class: "muted", {state::tr("both run as jobs on the worker")} }
            }
        }
        div { class: "card",
            h2 { {state::tr("Tables")} " " {ui::key_explain("explain.data.tables")} }
            table { class: "grid",
                thead { tr { th { {state::tr("table")} } th { {state::tr("rows")} } th { {state::tr("columns")} } } }
                tbody {
                    for t in list {
                        {
                            let name = s(&t, "name");
                            let cols = api::arr(&t, "columns").iter().map(|c| s(c, "name")).collect::<Vec<_>>().join(", ");
                            let n2 = name.clone();
                            rsx! {
                                tr { class: "click", onclick: move |e| super::common::go(Route::DataDbTable { table: n2.clone() }, &e),
                                    td { "{name}" }
                                    td { class: "num", "{s(&t, \"rows\")}" }
                                    td { class: "muted", "{cols}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
pub fn DataDbTable(table: String) -> Element {
    rsx! { for k in [table] { DbTablePage { key: "{k}", table: k } } }
}

#[component]
fn DbTablePage(table: String) -> Element {
    let t = table.clone();
    let first = use_resource(move || {
        let t = t.clone();
        async move { api::get(&format!("/api/v1/db/tables/{}/rows?limit=1", api::encode(&t))).await }
    });
    let t2 = table.clone();
    let spec = use_memo(move || {
        let first = first.read().clone()?.ok()?;
        let columns: Vec<String> = api::arr(&first, "columns").iter().filter_map(|c| c.as_str().map(str::to_string)).collect();
        let grid_cols: Vec<Column> = columns
            .iter()
            .enumerate()
            .map(|(i, c)| {
                // Column keys are static: the few names of one table, leaked once per view.
                let key: &'static str = Box::leak(c.clone().into_boxed_str());
                let mut col = Column::html(key, c, move |r| match r.get(i) {
                    None | Some(Value::Null) => "NULL".into(),
                    Some(Value::String(x)) => esc(x),
                    Some(v) => esc(&v.to_string()),
                })
                .sort(key);
                col.cls = "mono";
                col
            })
            .collect();
        let t = t2.clone();
        let cols2 = Rc::new(columns);
        let spec = GridSpec::new(&format!("db_{t2}"), grid_cols, move |q| {
            let t = t.clone();
            async move {
                let p = vec![("sort", q.sort.clone()), ("desc", if q.desc { "true".into() } else { String::new() }), ("filter", q.search.clone()), ("column", q.f("column")), ("limit", q.limit.to_string()), ("offset", q.offset.to_string())];
                let v = api::get(&format!("/api/v1/db/tables/{}/rows{}", api::encode(&t), api::qs(&p))).await?;
                Ok(json!({ "items": v.get("rows").cloned().unwrap_or_default(), "total": v.get("total").cloned().unwrap_or_default() }))
            }
        })
        .filters(vec![Filter::Select {
            key: "column",
            label: state::tr("column"),
            options: Rc::new(move || {
                let mut o = vec![(String::new(), state::tr("filter: any column"))];
                o.extend(cols2.iter().map(|c| (c.clone(), c.clone())));
                o
            }),
        }]);
        Some(Spec(Rc::new(spec)))
    });
    rsx! {
        div { class: "crumbs", a { onclick: move |_| { navigator().push(Route::DataDb {}); }, {state::tr("DB")} } " › {table}" }
        if let Some(spec) = spec() { Grid { spec } } else { div { class: "muted", {state::tr("loading…")} } }
    }
}
