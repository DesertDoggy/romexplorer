//! Config › Share: the servers and peers this machine syncs from (the console's
//! `showSources` / `sourceDialog`).

use std::collections::HashMap;

use dioxus::core::spawn_forever;
use dioxus::prelude::*;
use romexplorer_types::copy::{Mount, MountCheck, MountSpec};
use romexplorer_types::jobs::{JobEnqueued, JobRow};
use romexplorer_types::sources::{MediaOnReplace, ModeView, Reachability, RemoteSource, SourceSpec};
use serde_json::{json, Value};

use super::backup::JobWatch;
use super::common::bump;
use super::mode::{consent_dialog, Proceed};
use crate::api;
use crate::html::{self, badge, esc};
use crate::state;
use crate::ui::{self, Html, Explain};
use crate::Route;

#[component]
pub fn ConfigSources() -> Element {
    let reload = use_signal(|| 0u64);
    let sources = use_resource(move || async move {
        let _ = reload();
        api::get_as::<Vec<RemoteSource>>("/api/v1/sources").await
    });
    let mode = use_resource(|| async { api::get_as::<ModeView>("/api/v1/mode").await });
    let checks = use_signal(HashMap::<u16, String>::new);
    let watch = use_signal(|| None::<u64>);
    let list: Vec<RemoteSource> = match &*sources.read() {
        None => return rsx! { div { class: "muted", {state::tr("loading…")} } },
        Some(Err(e)) => return rsx! { div { class: "card bad", "{e.message}" } },
        Some(Ok(l)) => l.clone(),
    };
    let client = mode.read().as_ref().and_then(|r| r.as_ref().ok()).is_some_and(|m| m.mode == "client");
    let refresh = move |_: JobRow| bump(reload);
    rsx! {
        div { class: "card",
            h2 { {state::tr("Share: remote sources")} }
            Explain {
                {state::tr("The servers and peers this machine syncs from. One may be the ")} b { {state::tr("main")} } {state::tr(" server — where this machine's catalogue comes from, and what \"not on server\" is measured against; the rest are ")}
                b { {state::tr("peer")} } {state::tr(" shares, whose collections show up as \"on a peer\". Sign in to each with an account on that machine; only the token it hands back is kept, sealed, and it never comes back out.")}
            }
            // In view, not behind a "!": it is the one thing to know before adding a main server.
            if !client {
                p { class: "muted",
                    Html { html: badge(&state::tr("standalone"), "warn") }
                    {state::tr(" This machine builds its own catalogue. Designating a main server here replaces that catalogue with the server's — you will be asked first. Choosing ")}
                    Link { to: Route::ConfigMode {}, {state::tr("client mode")} } {state::tr(" first is the intended order.")}
                }
            }
            div { class: "toolbar",
                button { class: "btn primary", onclick: move |_| source_dialog(None, EventHandler::new(move |_| bump(reload))), {state::tr("add source")} }
            }
            if let Some(id) = watch() { JobWatch { key: "{id}", job_id: id, on_done: refresh } }
            table { class: "grid",
                thead { tr { th { {state::tr("name")} } th { {state::tr("role")} } th { {state::tr("address")} } th { {state::tr("token")} } th { {state::tr("enabled")} } th { {state::tr("last sync")} } th { {state::tr("mirrored")} } th {} } }
                tbody {
                    if list.is_empty() { tr { td { colspan: 8, class: "muted", {state::tr("none — add the main server, or a peer whose share you want to see")} } } }
                    for src in list {
                        SourceRow { key: "{src.id}", source: src.clone(), checks, watch, reload }
                    }
                }
            }
        }
    }
}

#[component]
fn SourceRow(source: RemoteSource, checks: Signal<HashMap<u16, String>>, watch: Signal<Option<u64>>, reload: Signal<u64>) -> Element {
    let id = source.id;
    let mirrored = if source.tables.is_empty() {
        format!("<span class=\"muted\">{}</span>", state::tr("nothing yet"))
    } else {
        let rows: u64 = source.tables.iter().map(|t| t.row_count).sum();
        let body: String = source.tables.iter().map(|t| format!("<tr><td class=\"mono\">{}</td><td class=\"num\">{}</td><td>{}</td></tr>", esc(&t.table_name), t.row_count, t.synced_at.map(|d| html::fmt_date(&d.to_string())).unwrap_or_default())).collect();
        format!("<details><summary>{}</summary><table class=\"grid\">{body}</table></details>", state::trf("{n} table(s), {rows} rows", &[("n", &source.tables.len().to_string()), ("rows", &rows.to_string())]))
    };
    let last = source.last_sync_at.map(|d| html::fmt_date(&d.to_string())).unwrap_or_default();
    let check_html = checks.read().get(&id).cloned().unwrap_or_default();
    let (edit_src, name_forget, name_del) = (source.clone(), source.name.clone(), source.name.clone());
    let is_main = source.role == "main";
    rsx! {
        tr {
            td { "{source.name}" }
            td { Html { html: badge(&source.role, if is_main { "ok" } else { "dim" }) } }
            td { class: "mono",
                "{source.base_url}"
                if let Some(fp) = source.tls_fingerprint.clone() {
                    div { class: "muted", title: state::tr("the certificate this machine trusts for it (SHA-256)"), {state::trf("cert {fp}…", &[("fp", fp.get(..16).unwrap_or(&fp))])} }
                }
            }
            td { Html { html: if source.has_token { badge(&state::tr("set"), "ok") } else { badge(&state::tr("none"), "bad") } } }
            td { Html { html: if source.enabled { badge(&state::tr("yes"), "ok") } else { badge(&state::tr("no"), "dim") } } }
            td { if last.is_empty() { span { class: "muted", {state::tr("never")} } } else { "{last}" } }
            td { Html { html: mirrored } }
            td { style: "white-space:nowrap",
                span { class: "link",
                    onclick: move |_| { spawn(async move {
                        checks.write().insert(id, state::tr("checking…"));
                        let html = match api::get_as::<Reachability>(&format!("/api/v1/sources/{id}/check")).await {
                            Ok(r) => {
                                // A main that only offers ownership is a share port, not a server: loud,
                                // because it would otherwise just never supply a catalogue.
                                let wrong_port = is_main && r.reachable && r.authorized && !r.offers_catalog;
                                format!("{} {} {} <span class=\"muted\">{}</span>{}",
                                    badge(&if r.reachable { state::tr("reachable") } else { state::tr("unreachable") }, if r.reachable { "ok" } else { "bad" }),
                                    badge(&if r.authorized { state::tr("token ok") } else { state::tr("token rejected") }, if r.authorized { "ok" } else { "bad" }),
                                    badge(&if r.offers_catalog { state::tr("offers catalogue") } else { state::tr("ownership only") }, if r.offers_catalog { "ok" } else if wrong_port { "bad" } else { "dim" }),
                                    esc(&r.detail),
                                    if wrong_port { format!("<div class=\"bad\">{}</div>", state::tr("Set as the main server, but this address is a share: it cannot supply a catalogue. Use the server's main port, not its share port.")) } else { String::new() })
                            }
                            Err(e) => format!("<span class=\"bad\">{}</span>", esc(&e.message)),
                        };
                        checks.write().insert(id, html);
                    }); },
                    {state::tr("check")}
                }
                " "
                span { class: "link",
                    onclick: move |_| { spawn(async move {
                        match api::post_as::<Value, JobEnqueued>(&format!("/api/v1/sources/{id}/sync"), &json!({})).await {
                            Ok(r) => watch.set(Some(r.job_id)),
                            Err(e) => ui::toast(e.message, true),
                        }
                    }); },
                    {state::tr("sync")}
                }
                " "
                span { class: "link", title: state::tr("download every table again, not only what changed -- for a mirror that has gone wrong"),
                    onclick: move |_| { spawn(async move {
                        match api::post_as::<Value, JobEnqueued>(&format!("/api/v1/sources/{id}/sync?full=true"), &json!({})).await {
                            Ok(r) => watch.set(Some(r.job_id)),
                            Err(e) => ui::toast(e.message, true),
                        }
                    }); },
                    {state::tr("full sync")}
                }
                " "
                span { class: "link", title: state::tr("fetch the art this source has and this machine lacks: from it first, from the provider only for a file it no longer has"),
                    onclick: move |_| { spawn(async move {
                        match api::post_as::<Value, JobEnqueued>(&format!("/api/v1/sources/{id}/media"), &json!({})).await {
                            Ok(r) => watch.set(Some(r.job_id)),
                            Err(e) => ui::toast(e.message, true),
                        }
                    }); },
                    {state::tr("art")}
                }
                " "
                span { class: "link", onclick: move |_| source_dialog(Some(edit_src.clone()), EventHandler::new(move |_| bump(reload))), {state::tr("edit")} }
                if source.tls_fingerprint.is_some() {
                    " "
                    span { class: "link", title: state::tr("after the other machine's certificate was replaced on purpose"),
                        onclick: move |_| { spawn(async move {
                            if !ui::confirm(&state::tr("Forget the certificate trusted for this source? The next connection trusts whatever it presents -- only do this if its certificate was replaced on purpose.")).await { return; }
                            match api::post(&format!("/api/v1/sources/{id}/trust"), json!({})).await {
                                Ok(_) => { ui::toast(state::tr("the next connection will trust its new certificate"), false); bump(reload); }
                                Err(e) => ui::toast(e.message, true),
                            }
                        }); },
                        {state::tr("trust new certificate")}
                    }
                }
                " "
                span { class: "link",
                    onclick: move |_| { let name = name_forget.clone(); spawn(async move {
                        if !ui::confirm(&state::trf("Forget everything mirrored from {name}? The source stays; its mirrored rows are removed and come back on the next sync.", &[("name", &name)])).await { return; }
                        match api::post(&format!("/api/v1/sources/{id}/forget"), json!({})).await {
                            Ok(r) => { ui::toast(state::trf("removed {n} mirrored row(s)", &[("n", &api::u(&r, "rows_removed").to_string())]), false); bump(reload); }
                            Err(e) => ui::toast(e.message, true),
                        }
                    }); },
                    {state::tr("forget")}
                }
                " "
                span { class: "link",
                    onclick: move |_| { let name = name_del.clone(); spawn(async move {
                        if !ui::confirm(&state::trf("Delete the source {name}? Its mirrored rows stay until you forget them; adding the same address again revives it.", &[("name", &name)])).await { return; }
                        match api::delete(&format!("/api/v1/sources/{id}")).await {
                            Ok(_) => { ui::toast(state::trc("done", "deleted"), false); bump(reload); }
                            Err(e) => ui::toast(e.message, true),
                        }
                    }); },
                    {state::tr("delete")}
                }
                if !check_html.is_empty() { div { class: "muted", Html { html: check_html } } }
                SourceMounts { source_id: id, source_name: source.name.clone() }
            }
        }
    }
}

/// Where this source's paths appear on this machine.
///
/// A copy from another machine is plain OS file copy of the resolved path, so a source with
/// no mount can be browsed but not copied from. The prefix is a path on the *other* machine
/// (`D:\Roms`, `/srv/roms`); the local path is where that share is mounted here.
#[component]
fn SourceMounts(source_id: u16, source_name: String) -> Element {
    let mut data = use_resource(move || async move { api::get_as::<Vec<Mount>>(&format!("/api/v1/sources/{source_id}/mounts")).await });
    let mut checks = use_signal(HashMap::<u16, String>::new);
    let list = match &*data.read() {
        Some(Ok(l)) => l.clone(),
        _ => Vec::new(),
    };
    if let Some(Err(e)) = &*data.read() {
        return rsx! { div { class: "bad", "{e.message}" } };
    }
    // `Resource` is `Copy`, so a fresh handle per call keeps this an `Fn`.
    let reload = move || {
        let mut d = data;
        d.restart();
    };
    rsx! {
        details { style: "margin-top:4px",
            summary { class: "muted", {state::trf("mounts ({n})", &[("n", &list.len().to_string())])} }
            table { class: "grid",
                thead { tr { th { {state::tr("its path")} } th { {state::tr("here")} } th { {state::tr("checked")} } th {} } }
                tbody {
                    if list.is_empty() { tr { td { colspan: "4", class: "muted", {state::tr("none — add one to copy files from this source")} } } }
                    for m in list {
                        {
                            let mid = m.id;
                            let detail = checks.read().get(&mid).cloned().unwrap_or_default();
                            rsx! {
                                tr {
                                    td { class: "mono", "{m.remote_prefix}" }
                                    td { class: "mono", "{m.local_path}" }
                                    td { class: "muted",
                                        if let Some(at) = m.last_verified_at { "{html::fmt_date(&at.to_string())}" } else { {state::tr("never")} }
                                        if !detail.is_empty() { div { Html { html: detail } } }
                                    }
                                    td { style: "white-space:nowrap",
                                        span { class: "link",
                                            onclick: move |_| { spawn(async move {
                                                checks.write().insert(mid, state::tr("checking…"));
                                                let html = match api::post_as::<Value, MountCheck>(&format!("/api/v1/mounts/{mid}/check"), &json!({})).await {
                                                    Ok(c) => format!("{} <span class=\"muted\">{}</span>", badge(&if c.ok { state::tr("ok") } else { state::tr("wrong") }, if c.ok { "ok" } else { "bad" }), esc(&c.detail)),
                                                    Err(e) => format!("<span class=\"bad\">{}</span>", esc(&e.message)),
                                                };
                                                checks.write().insert(mid, html);
                                            }); },
                                            {state::tr("check")}
                                        }
                                        " "
                                        span { class: "link",
                                            onclick: move |_| { spawn(async move {
                                                if !ui::confirm(&state::tr("Remove this mount? Only the mapping is removed — nothing on disk is touched.")).await { return; }
                                                match api::delete(&format!("/api/v1/mounts/{mid}")).await {
                                                    Ok(_) => { ui::toast(state::tr("removed"), false); data.restart(); }
                                                    Err(e) => ui::toast(e.message, true),
                                                }
                                            }); },
                                            {state::tr("remove")}
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            button { class: "btn small", r#type: "button", onclick: move |_| mount_dialog(source_id, source_name.clone(), std::rc::Rc::new(reload)), {state::tr("add a mount…")} }
        }
    }
}

fn mount_dialog(source_id: u16, source_name: String, done: std::rc::Rc<dyn Fn()>) {
    ui::open_modal(move || rsx! { MountDialog { source_id, source_name: source_name.clone(), done: super::common::DoneFn(Some(done.clone())) } });
}

#[component]
fn MountDialog(source_id: u16, source_name: String, done: super::common::DoneFn) -> Element {
    let roots = use_resource(move || async move { api::get(&format!("/api/v1/sources/{source_id}/roots")).await.map(|v| api::items(&v)) });
    let mut remote_prefix = use_signal(String::new);
    let local_path = use_signal(String::new);
    let mut error = use_signal(String::new);
    let root_list = roots.read().clone().and_then(|r| r.ok()).unwrap_or_default();
    let roots_error = roots.read().as_ref().and_then(|r| r.as_ref().err()).map(|e| e.message.clone());
    rsx! {
        h3 { {state::trf("Mount {name}", &[("name", &source_name)])} }
        Explain {
            {state::tr("Copying from another machine is a plain file copy, so its collection has to be reachable here — mounted over SMB, NFS, or whatever you use. Tell romexplorer where its paths turn up on this machine and it can work the rest out.")}
        }
        div { class: "kv", style: "grid-template-columns:max-content 1fr; gap:6px 10px",
            div { {state::tr("its collection root")} }
            div {
                select { onchange: move |e| remote_prefix.set(e.value()),
                    option { value: "", {state::tr("— pick one, or type a parent below —")} }
                    for r in root_list.iter() { option { value: "{api::s(r, \"path\")}", "{api::s(r, \"path\")}" } }
                }
                if let Some(e) = roots_error { div { class: "bad", "{e}" } }
            }
            div { {state::tr("its path")} }
            div {
                input { r#type: "text", style: "width:100%", placeholder: "D:\\Roms · /srv/roms · \\\\nas\\share\\Roms", value: "{remote_prefix}", oninput: move |e| remote_prefix.set(e.value()) }
                div { class: "muted", {state::tr("Exactly as that machine writes it. A parent of several roots works too.")} }
            }
            div { {state::tr("here")} }
            div { super::config::PathInput { value: local_path, style: "width:100%", placeholder: state::tr("where that share is mounted on this machine") } }
        }
        if !error().is_empty() { div { class: "card bad", "{error}" } }
        div { class: "fsfoot",
            span { class: "grow" }
            button { r#type: "button", class: "btn", onclick: move |_| ui::close_modal(), {state::tr("cancel")} }
            button { r#type: "button", class: "btn primary", disabled: remote_prefix().trim().is_empty() || local_path().trim().is_empty(),
                onclick: move |_| {
                    let spec = MountSpec { remote_prefix: remote_prefix().trim().to_string(), local_path: local_path().trim().to_string(), remote_sep: None, remote_directory_id: None, enabled: true };
                    let done = done.0.clone();
                    spawn_forever(async move {
                        match api::post_as::<MountSpec, Mount>(&format!("/api/v1/sources/{source_id}/mounts"), &spec).await {
                            Ok(_) => { ui::toast(state::tr("mount added"), false); ui::close_modal(); if let Some(d) = done { d(); } }
                            Err(e) => error.set(e.message),
                        }
                    });
                },
                {state::tr("add")}
            }
        }
    }
}

/// Add (`None`) or edit a source.
pub fn source_dialog(existing: Option<RemoteSource>, on_done: EventHandler<()>) {
    ui::open_modal(move || rsx! { SourceDialog { existing: existing.clone(), on_done } });
}

/// Servers and shares that answered a search of the local network (asked by the server this
/// app talks to, so the web app gets the same list). Picking one fills in the form: a server
/// becomes the main server, a share a peer; signing in is still the fields below.
#[component]
fn NetworkSources(on_pick: EventHandler<romexplorer_types::discovery::Found>) -> Element {
    let mut found = use_resource(|| async { api::get_as::<Vec<romexplorer_types::discovery::Found>>("/api/v1/sources/discover").await });
    let list = found.read().as_ref().map(|r| r.as_ref().map(|l| l.clone()).unwrap_or_default());
    rsx! {
        div { class: "muted", style: "margin-bottom:6px", {state::tr("On this network")} }
        div { class: "toolbar", style: "flex-wrap:wrap;margin-bottom:10px",
            match list {
                None => rsx! { span { class: "muted", {state::tr("searching…")} } },
                Some(l) if l.is_empty() => rsx! { span { class: "muted", {state::tr("Nothing answered. Type the address below.")} } },
                Some(l) => rsx! {
                    for f in l {
                        button { key: "{f.base()}", class: "btn", r#type: "button", onclick: { let f = f.clone(); move |_| on_pick.call(f.clone()) },
                            b { "{f.announce.name}" } " "
                            span { class: "muted", "{f.ip}:{f.announce.port} · " {if f.announce.kind == "server" { state::tr("main server") } else { state::tr("peer") }} }
                        }
                    }
                },
            }
            button { class: "btn", r#type: "button", onclick: move |_| found.restart(), {state::tr("search again")} }
        }
    }
}

#[component]
fn SourceDialog(existing: Option<RemoteSource>, on_done: EventHandler<()>) -> Element {
    let (id, has_token) = existing.as_ref().map(|s| (Some(s.id), s.has_token)).unwrap_or((None, false));
    let mut name = use_signal(|| existing.as_ref().map(|s| s.name.clone()).unwrap_or_default());
    let (address0, port0) = api::split_base(&existing.as_ref().map(|s| s.base_url.clone()).unwrap_or_default());
    let mut address = use_signal(|| address0);
    let mut port = use_signal(|| if port0.is_empty() && id.is_none() { api::DEFAULT_PORT.to_string() } else { port0 });
    let mut token = use_signal(String::new);
    let mut username = use_signal(String::new);
    let mut password = use_signal(String::new);
    let mut role = use_signal(|| existing.as_ref().map(|s| s.role.clone()).unwrap_or_else(|| "peer".into()));
    let mut enabled = use_signal(|| existing.as_ref().map(|s| s.enabled).unwrap_or(true));
    let placeholder = if id.is_some() { if has_token { state::tr("stored — leave blank to keep it") } else { state::tr("none stored") } } else { state::tr("the other machine's bearer token") };
    let submit = move |_| {
        let spec = SourceSpec {
            name: name().trim().to_string(),
            base_url: api::join_base(&address(), &port()),
            token: Some(token()).filter(|t| !t.is_empty()),
            username: Some(username().trim().to_string()).filter(|u| !u.is_empty()),
            password: Some(password()).filter(|_| !username().trim().is_empty()),
            role: role(),
            enabled: enabled(),
            replace_catalog: false,
            media: MediaOnReplace::default(),
        };
        // Detached (`spawn_forever`): the 409 branch closes this dialog and carries on.
        spawn_forever(async move {
            let send = move |spec: SourceSpec| async move {
                match id {
                    Some(id) => api::put_as::<SourceSpec, RemoteSource>(&format!("/api/v1/sources/{id}"), &spec).await,
                    None => api::post_as::<SourceSpec, RemoteSource>("/api/v1/sources", &spec).await,
                }
            };
            match send(spec.clone()).await {
                Ok(_) => {
                    ui::toast(state::tr("saved"), false);
                    ui::close_modal();
                    on_done.call(());
                }
                // Designating a main server on a machine that built its own catalogue: the
                // server refuses without consent and says what would be lost. Ask, then resend.
                Err(e) if e.status == 409 => {
                    ui::close_modal();
                    // The dialog that owns this scope is closed by now, so the resend must not be
                    // tied to it (see `mode::apply_mode`).
                    let again = Proceed::new(move |media: MediaOnReplace| {
                        let mut spec = spec.clone();
                        spec.replace_catalog = true;
                        spec.media = media;
                        spawn_forever(async move {
                            match send(spec).await {
                                Ok(_) => { ui::toast(state::tr("saved — the catalogue was cleared; run a sync now"), false); on_done.call(()); }
                                Err(e) => ui::toast(e.message, true),
                            }
                        });
                    });
                    consent_dialog(state::tr("Replace this machine's catalogue?"), e.message, again);
                }
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    rsx! {
        h3 { if id.is_some() { {state::tr("Edit source")} } else { {state::tr("Add source")} } }
        if id.is_none() {
            NetworkSources { on_pick: move |f: romexplorer_types::discovery::Found| {
                name.set(f.announce.name.clone());
                let (a, p) = api::split_base(&f.base());
                address.set(a);
                port.set(p);
                role.set(if f.announce.kind == "server" { "main".into() } else { "peer".into() });
            } }
        }
        form { class: "kv", style: "grid-template-columns:110px 1fr", onsubmit: move |e| { e.prevent_default(); submit(()); },
            div { {state::tr("name")} } div { input { value: "{name}", required: true, style: "width:100%", oninput: move |e| name.set(e.value()) } }
            div { {state::tr("address")} } div {
                input { value: "{address}", required: true, style: "width:100%", placeholder: "https://192.168.1.10", oninput: move |e| address.set(e.value()) }
                div { class: "muted", {state::tr("the other machine's IP address or name, no /api/v1")} }
            }
            div { {state::tr("port")} } div {
                input { value: "{port}", inputmode: "numeric", style: "width:8em", placeholder: api::DEFAULT_PORT, oninput: move |e| port.set(e.value()) }
                div { class: "muted", {state::tr("a main server's port (7337 unless changed), or the port a peer shares on")} }
            }
            div { {state::tr("sign in as")} } div {
                input { value: "{username}", style: "width:48%", autocomplete: "off", placeholder: state::tr("username on that machine"), oninput: move |e| username.set(e.value()) }
                " "
                input { r#type: "password", value: "{password}", style: "width:48%", autocomplete: "off", placeholder: state::tr("password"), oninput: move |e| password.set(e.value()) }
                div { class: "muted", {state::tr("This machine signs in once and keeps only the token it is given for itself — the password is not stored. Its certificate is trusted on that first connection.")} }
            }
            div { {state::tr("or a token")} } div { input { r#type: "password", value: "{token}", style: "width:100%", autocomplete: "off", placeholder: "{placeholder}", oninput: move |e| token.set(e.value()) } }
            div { {state::tr("role")} } div {
                label { input { r#type: "radio", name: "role", checked: role() != "main", onchange: move |_| role.set("peer".into()) } {state::tr(" peer — a share; its collection shows as \"on a peer\"")} }
                br {}
                label { input { r#type: "radio", name: "role", checked: role() == "main", onchange: move |_| role.set("main".into()) } {state::tr(" main server — this machine's catalogue comes from it")} }
            }
            div { {state::tr("enabled")} } div { label { input { r#type: "checkbox", checked: enabled(), onchange: move |e| enabled.set(e.checked()) } {state::tr(" enabled")} } }
            div {} div {
                button { class: "btn primary", {state::tr("save")} }
                " "
                button { class: "btn", r#type: "button", onclick: move |_| ui::close_modal(), {state::tr("cancel")} }
            }
        }
    }
}
