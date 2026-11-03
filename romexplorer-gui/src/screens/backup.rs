//! Config › Backup: the two backup kinds and the SQLite mobile cache (the console's
//! `showBackup`), plus [`JobWatch`], the one-job poller the sync screens share (`watchJob`).
//!
//! Typed throughout: the rows are `romexplorer_types::{backup, sqlite, jobs}` structs read
//! with `api::get_as`, not `Value`s walked with `s()`/`u()`.

use dioxus::prelude::*;
use romexplorer_types::backup::BackupFile;
use romexplorer_types::jobs::{JobEnqueued, JobRow};
use romexplorer_types::sqlite::CacheFile;
use serde_json::{json, Value};

use super::common::bump;
use crate::api;
use crate::html::{self, badge, esc};
use crate::state;
use crate::ui::{self, Html, Explain};

/// A typed job row's progress: the same bar the jobs list draws.
pub fn progress_html(j: &JobRow) -> String {
    let msg = j.progress_message.clone().unwrap_or_default();
    if j.progress_done.is_none() && msg.is_empty() {
        return String::new();
    }
    let bar = match (j.progress_done, j.progress_total) {
        (Some(d), Some(t)) if t > 0 => {
            let pct = (100.0 * d as f64 / t as f64).floor().min(100.0) as u64;
            format!("<div class=\"pbar\"><div style=\"width:{pct}%\"></div></div><span class=\"num\">{pct}%</span>")
        }
        (Some(d), _) => format!("<span class=\"num\">{d}</span>"),
        _ => String::new(),
    };
    format!("<div class=\"prog\" title=\"{}\">{bar}<span class=\"muted\">{}</span></div>", esc(&msg), esc(&msg.chars().take(90).collect::<String>()))
}

/// A job's `result` (the worker's summary struct as JSON) rendered generically: `[name,
/// count]` lists as a collapsed per-table breakdown with a total, other lists joined, scalars
/// as key/value. Empty things are skipped so "nothing was skipped" takes no row.
pub fn result_html(r: &Value) -> String {
    let Some(map) = r.as_object() else { return String::new() };
    let mut rows = String::new();
    for (k, v) in map {
        let row = match v {
            Value::Null | Value::Bool(false) => continue,
            Value::String(s) if s.is_empty() => continue,
            Value::Array(a) if a.is_empty() => continue,
            Value::Array(a) if a.iter().all(|x| x.as_array().is_some_and(|p| p.len() == 2)) => {
                let total: u64 = a.iter().filter_map(|p| p[1].as_u64()).sum();
                let body: String = a.iter().map(|p| format!("<tr><td class=\"mono\">{}</td><td class=\"num\">{}</td></tr>", esc(p[0].as_str().unwrap_or_default()), p[1])).collect();
                format!("<details><summary>{}</summary><table class=\"grid\">{body}</table></details>", state::trf("{n} table(s), {rows} rows", &[("n", &a.len().to_string()), ("rows", &total.to_string())]))
            }
            // A long list (a scan's errors and warnings) one per line, folded.
            Value::Array(a) if a.len() > 3 => format!(
                "<details><summary>{}</summary><div class=\"mono\" style=\"white-space:pre-wrap; max-height:40vh; overflow:auto\">{}</div></details>",
                state::trf("{n} item(s)", &[("n", &a.len().to_string())]),
                a.iter().map(|x| esc(&x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string()))).collect::<Vec<_>>().join("\n")
            ),
            Value::Array(a) => esc(&a.iter().map(|x| x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string())).collect::<Vec<_>>().join(", ")),
            // One level of nesting as its own list (a backup's `file`), not raw JSON.
            Value::Object(o) => o.iter().map(|(k2, v2)| format!("<div><span class=\"muted\">{}</span> {}</div>", esc(k2), esc(&v2.as_str().map(str::to_string).unwrap_or_else(|| v2.to_string())))).collect(),
            Value::String(s) => esc(s),
            other => esc(&other.to_string()),
        };
        rows.push_str(&format!("<div>{}</div><div>{row}</div>", esc(k)));
    }
    format!("<div class=\"kv\" style=\"margin-top:6px\">{rows}</div>")
}

/// Polls one job every second until it finishes, showing its progress and then its
/// `result`. Give it `key: "{job_id}"` so a new job remounts it rather than reusing the
/// old poll.
#[component]
pub fn JobWatch(job_id: u64, #[props(default)] on_done: Option<EventHandler<JobRow>>) -> Element {
    let mut job = use_signal(|| None::<JobRow>);
    let mut error = use_signal(String::new);
    use_future(move || async move {
        loop {
            match api::get_as::<JobRow>(&format!("/api/v1/jobs/{job_id}")).await {
                Ok(j) => {
                    let finished = matches!(j.status.as_str(), "done" | "failed" | "cancelled");
                    if !error.peek().is_empty() {
                        error.set(String::new());
                    }
                    job.set(Some(j.clone()));
                    if finished {
                        if let Some(cb) = on_done {
                            cb.call(j);
                        }
                        return;
                    }
                }
                // A job that is gone stays gone; anything else (the server restarting, a
                // dropped connection) is shown and tried again a little later.
                Err(e) if e.status == 404 => {
                    error.set(e.message);
                    return;
                }
                Err(e) => {
                    error.set(e.message);
                    ui::sleep(4000).await;
                }
            }
            ui::sleep(1000).await;
        }
    });
    if !error().is_empty() && job().is_none() {
        return rsx! { div { class: "bad", "{error}" } };
    }
    let Some(j) = job() else { return rsx! { div { class: "muted", {state::trf("job #{id} queued…", &[("id", &job_id.to_string())])} } } };
    let cls = match j.status.as_str() {
        "pending" | "in_progress" => "warn",
        "done" => "ok",
        "failed" => "bad",
        _ => "dim",
    };
    let finished = matches!(j.status.as_str(), "done" | "failed" | "cancelled");
    let err = j.last_error.clone().unwrap_or_default();
    let result = if finished { j.result.as_ref().map(result_html).unwrap_or_default() } else { String::new() };
    rsx! {
        div { class: "prog",
            Html { html: badge(&format!("#{job_id} {} · {}", j.job_type.name(), j.status), cls) }
            " "
            Html { html: progress_html(&j) }
        }
        if !err.is_empty() { div { class: "bad", "{err}" } }
        if !error().is_empty() { div { class: "bad", "{error}" } }
        if !result.is_empty() { Html { html: result } }
    }
}

/// Starts a job through one of the enqueueing routes and hands the id to a `JobWatch` slot.
pub fn start_job(path: String, mut watch: Signal<Option<u64>>) {
    spawn(async move {
        match api::post_as::<Value, JobEnqueued>(&path, &json!({})).await {
            Ok(r) => watch.set(Some(r.job_id)),
            Err(e) => ui::toast(e.message, true),
        }
    });
}

/// "download" for a file the server holds. Web and mobile fetch it over HTTP with the token;
/// the desktop build has no HTTP path to the backup directory (its asset handler serves media
/// only), and the file is on this machine anyway.
#[component]
fn Download(kind: String, name: String) -> Element {
    #[cfg(feature = "local")]
    {
        let _ = (kind, name);
        rsx! { span { class: "muted", title: state::tr("the file is in this machine's backup directory"), {state::tr("on disk")} } }
    }
    #[cfg(not(feature = "local"))]
    {
        let href = api::media_url(&format!("/api/v1/{kind}/{}", api::encode(&name)));
        rsx! { a { class: "link", href: "{href}", download: "{name}", {state::tr("download")} } }
    }
}

#[component]
pub fn ConfigBackup() -> Element {
    let reload = use_signal(|| 0u64);
    let backups = use_resource(move || async move {
        let _ = reload();
        api::get_as::<Vec<BackupFile>>("/api/v1/backup").await
    });
    let caches = use_resource(move || async move {
        let _ = reload();
        api::get_as::<Vec<CacheFile>>("/api/v1/cache").await
    });
    let bk_watch = use_signal(|| None::<u64>);
    let cache_watch = use_signal(|| None::<u64>);
    let mut kind = use_signal(|| "all".to_string());
    let backup_list: Vec<BackupFile> = backups.read().clone().and_then(|r| r.ok()).unwrap_or_default();
    let cache_list: Vec<CacheFile> = caches.read().clone().and_then(|r| r.ok()).unwrap_or_default();
    // A list that would not load says why, rather than reading as an empty one.
    let backup_err = backups.read().as_ref().and_then(|r| r.as_ref().err()).map(|e| e.message.clone());
    let cache_err = caches.read().as_ref().and_then(|r| r.as_ref().err()).map(|e| e.message.clone());
    let refresh = move |_: JobRow| bump(reload);
    let del = move |path: String| {
        spawn(async move {
            match api::delete(&path).await {
                Ok(_) => {
                    ui::toast(state::trc("done", "deleted"), false);
                    bump(reload);
                }
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    let mut upload_file = use_signal(|| None::<dioxus::html::FileData>);
    let mut uploading = use_signal(|| false);
    rsx! {
        div { class: "two",
            div { class: "card",
                h2 { {state::tr("Backups")} }
                Explain {
                    {state::tr("Two kinds, two files. ")} b { {state::tr("database")} } {state::tr(": what this machine holds and how it is configured — small, worth backing up often. ")}
                    b { {state::tr("catalogue")} } {state::tr(": what the DAT imports and providers built — large, re-derivable, worth backing up rarely. Media files are in neither. A restore upserts rows and never deletes; this installation keeps its own identity and mode.")}
                }
                div { class: "toolbar",
                    button { class: "btn primary", onclick: move |_| start_job("/api/v1/backup?kind=database".into(), bk_watch), {state::tr("back up database")} }
                    button { class: "btn", onclick: move |_| start_job("/api/v1/backup?kind=catalog".into(), bk_watch), {state::tr("back up catalogue")} }
                    button { class: "btn", title: state::tr("DAT releases and titles with their IGDB / RetroAchievements ids only — a starting catalogue for another install"),
                        onclick: move |_| start_job("/api/v1/backup?kind=dat-seed".into(), bk_watch), {state::tr("make DAT seed")} }
                    span { class: "grow" }
                    input { r#type: "file", accept: ".zip,application/zip", onchange: move |e| upload_file.set(e.files().into_iter().next()) }
                    button { class: "btn", disabled: uploading(),
                        onclick: move |_| {
                            let Some(f) = upload_file() else { ui::toast(state::tr("choose a file"), true); return; };
                            uploading.set(true);
                            spawn(async move {
                                match api::add_backup(f).await {
                                    Ok(r) => { ui::toast(state::trf("added {name}", &[("name", &api::s(&r, "name"))]), false); bump(reload); }
                                    Err(e) => ui::toast(e.message, true),
                                }
                                uploading.set(false);
                            });
                        },
                        if uploading() { {state::tr("uploading…")} } else { {state::tr("upload backup")} }
                    }
                    Explain { {state::tr("Adds a backup zip from another machine, or one kept elsewhere, to this backup folder. It is checked first (a romexplorer database or catalogue backup this version can read) and then listed like the others; restoring it is a separate step.")} }
                }
                if let Some(id) = bk_watch() { JobWatch { key: "{id}", job_id: id, on_done: refresh } }
                table { class: "grid",
                    thead { tr { th { {state::tr("file")} } th { {state::tr("kind")} } th { class: "num", {state::tr("size")} } th { {state::tr("created")} } th {} } }
                    tbody {
                        if let Some(e) = backup_err.clone() { tr { td { colspan: 5, class: "bad", "{e}" } } } else if backup_list.is_empty() { tr { td { colspan: 5, class: "muted", {state::tr("none yet")} } } }
                        for f in backup_list {
                            tr { key: "{f.name}",
                                td { class: "mono", "{f.name}" }
                                td { Html { html: badge(&f.kind, if f.kind == "database" { "ok" } else { "dim" }) } }
                                td { class: "num", "{html::fmt_size(Some(f.bytes))}" }
                                td { "{html::fmt_date(&f.created.to_string())}" }
                                td { style: "white-space:nowrap",
                                    Download { kind: "backup".to_string(), name: f.name.clone() }
                                    " "
                                    span { class: "link",
                                        onclick: { let name = f.name.clone(); move |_| { let name = name.clone(); spawn(async move {
                                            if !ui::confirm(&state::trf("Restore {name}?\n\nRows are upserted into this database; nothing is deleted.", &[("name", &name)])).await { return; }
                                            start_job(format!("/api/v1/backup/{}/restore", api::encode(&name)), bk_watch);
                                        }); } },
                                        {state::tr("restore")}
                                    }
                                    " "
                                    span { class: "link",
                                        onclick: { let name = f.name.clone(); move |_| { let name = name.clone(); spawn(async move {
                                            if ui::confirm(&state::trf("Delete {name}?", &[("name", &name)])).await { del(format!("/api/v1/backup/{}", api::encode(&name))); }
                                        }); } },
                                        {state::tr("delete")}
                                    }
                                }
                            }
                        }
                    }
                }
            }
            div { class: "card",
                h2 { {state::tr("Mobile cache (SQLite)")} }
                Explain {
                    {state::tr("One SQLite file a phone reads offline; ")} b { {state::tr("all")} } {state::tr(" is what a phone needs (both halves). Importing brings a cache back into this database — its ownership tables only when the file is this machine's own, or with \"adopt\".")}
                }
                form { class: "inline", onsubmit: move |e| e.prevent_default(),
                    label { {state::tr("kind")}
                        select { onchange: move |e| kind.set(e.value()),
                            option { value: "all", {state::tr("all")} }
                            option { value: "database", {state::tr("database")} }
                            option { value: "catalog", {state::tr("catalog")} }
                        }
                    }
                    button { class: "btn primary", r#type: "button", onclick: move |_| start_job(format!("/api/v1/cache?kind={}", kind()), cache_watch), {state::tr("export")} }
                }
                if let Some(id) = cache_watch() { JobWatch { key: "{id}", job_id: id, on_done: refresh } }
                table { class: "grid",
                    thead { tr { th { {state::tr("file")} } th { {state::tr("kind")} } th { class: "num", {state::tr("size")} } th { {state::tr("created")} } th {} } }
                    tbody {
                        if let Some(e) = cache_err.clone() { tr { td { colspan: 5, class: "bad", "{e}" } } } else if cache_list.is_empty() { tr { td { colspan: 5, class: "muted", {state::tr("none yet")} } } }
                        for f in cache_list {
                            CacheRow { key: "{f.name}", file: f.clone(), watch: cache_watch, on_delete: del }
                        }
                    }
                }
            }
        }
        ConfigFileCard {}
    }
}

/// The sections of a configuration file (`api::config_file::SECTIONS`) and what each is
/// called on its own screen.
const CONFIG_SECTIONS: [(&str, &str); 6] =
    [("settings", "Settings"), ("directories", "Directories"), ("storage", "Storage"), ("emulators", "Emulators"), ("output", "Copy folders and formats"), ("schedules", "Scheduled jobs")];

fn config_section_label(id: &str) -> String {
    state::tr(CONFIG_SECTIONS.iter().find(|(s, _)| *s == id).map_or(id, |(_, label)| label))
}

fn config_section_count(config: &Value, section: &str) -> usize {
    let len = |v: &Value| v.as_array().map(Vec::len).or_else(|| v.as_object().map(|o| o.len())).unwrap_or(0);
    let v = &config[section];
    match section {
        "storage" => len(&v["media_dirs"]) + len(&v["temp_dirs"]),
        "output" => len(&v["folders"]) + len(&v["formats"]),
        _ => len(v),
    }
}

/// The last import's summary. Outside the card, because an import that changed the settings
/// rebuilds the page (`state::REDRAW`) and the summary is shown in the new card.
static IMPORTED: GlobalSignal<Option<Value>> = Signal::global(|| None);

/// The configuration as one JSON file (the console's `configFileCard`): export saves it;
/// import reads the chosen file here, asks which of its sections to bring in, and shows
/// what was applied, what was already so, and what was skipped and why.
#[component]
fn ConfigFileCard() -> Element {
    let mut chosen = use_signal(|| None::<dioxus::html::FileData>);
    // The file once read (its name and its JSON), then which of its sections are ticked.
    let mut read = use_signal(|| None::<(String, Value)>);
    let mut ticked = use_signal(std::collections::BTreeSet::<String>::new);
    let mut result = IMPORTED.signal();
    let mut busy = use_signal(|| false);
    let export = move |_| {
        spawn(async move {
            match api::backend::backend().call(api::Method::Get, "/api/v1/config/export", None).await {
                Ok(text) => {
                    let day = serde_json::from_str::<Value>(&text).ok().and_then(|v| v["exported_at"].as_str().and_then(|d| d.get(..10)).map(str::to_string)).unwrap_or_else(|| "export".to_string());
                    super::data::save_text(&text, &format!("romexplorer-config-{day}.json"), "application/json").await;
                }
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    let open = move |_| {
        let Some(file) = chosen() else { ui::toast(state::tr("choose a file"), true); return; };
        spawn(async move {
            let config = match file.read_bytes().await {
                Ok(bytes) => serde_json::from_slice::<Value>(&bytes).ok(),
                Err(e) => { ui::toast(e.to_string(), true); return; }
            };
            let Some(config) = config else { ui::toast(state::tr("That file is not JSON."), true); return; };
            let held: std::collections::BTreeSet<String> = CONFIG_SECTIONS.iter().filter(|(id, _)| config_section_count(&config, id) > 0).map(|(id, _)| id.to_string()).collect();
            if config["format"] != "romexplorer-config" || held.is_empty() {
                ui::toast(state::tr("That is not a romexplorer configuration file, or it is empty."), true);
                return;
            }
            ticked.set(held);
            result.set(None);
            read.set(Some((file.name(), config)));
        });
    };
    let import = move |_| {
        let Some((_, config)) = read() else { return };
        let sections: Vec<String> = ticked().into_iter().collect();
        if sections.is_empty() { ui::toast(state::tr("tick at least one section"), true); return; }
        busy.set(true);
        spawn(async move {
            let outcome = api::post("/api/v1/config/import", json!({ "config": config, "sections": sections })).await;
            busy.set(false);
            match outcome {
                Ok(r) => {
                    // Settings take effect as a Save would: the app's copy of them (the UI
                    // language among them) is read again, and the page built again with it.
                    let settings_changed = r["sections"].as_array().into_iter().flatten().any(|s| api::s(s, "section") == "settings" && api::u(s, "applied") > 0);
                    result.set(Some(r));
                    read.set(None);
                    if settings_changed {
                        state::reload_settings().await;
                        *state::REDRAW.write() += 1;
                    }
                    ui::toast(state::tr("configuration imported"), false);
                }
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    let sections: Vec<Value> = result().and_then(|r| r["sections"].as_array().cloned()).unwrap_or_default();
    let skipped: Vec<(String, String, String)> = sections.iter().flat_map(|s| s["skipped"].as_array().cloned().unwrap_or_default().into_iter().map(|k| (api::s(s, "section"), api::s(&k, "item"), api::s(&k, "reason")))).collect();
    rsx! {
        div { class: "card",
            h2 { {state::tr("Configuration file")} }
            Explain { {state::tr("Settings, scheduled jobs, emulators, copy folder names and formats, collection directories and storage folders as one JSON file — for a new install or another machine. Provider credentials and accounts are not in it. Importing adds and changes; it never removes.")} }
            div { class: "toolbar",
                button { class: "btn primary", onclick: export, {state::tr("export configuration")} }
                span { class: "grow" }
                input { r#type: "file", accept: ".json,application/json", onchange: move |e| chosen.set(e.files().into_iter().next()) }
                button { class: "btn", onclick: open, {state::tr("import configuration…")} }
            }
            if let Some((name, config)) = read() {
                p { {state::trf("{name}: exported {date} by romexplorer {version}. Bring in:", &[("name", &name), ("date", config["exported_at"].as_str().unwrap_or("?")), ("version", config["app_version"].as_str().unwrap_or("?"))])} }
                div { class: "toolbar",
                    for (id, _) in CONFIG_SECTIONS.iter().filter(|(id, _)| config_section_count(&config, id) > 0) {
                        label { key: "{id}",
                            input { r#type: "checkbox", checked: ticked().contains(*id),
                                onchange: move |e| { let mut t = ticked(); if e.checked() { t.insert(id.to_string()); } else { t.remove(*id); } ticked.set(t); } }
                            " {config_section_label(id)} "
                            span { class: "muted", "({config_section_count(&config, id)})" }
                        }
                    }
                    span { class: "grow" }
                    button { class: "btn primary", disabled: busy(), onclick: import, {state::tr("import")} }
                }
            }
            if !sections.is_empty() {
                table { class: "grid",
                    thead { tr { th { {state::tr("section")} } th { class: "num", {state::tr("added or changed")} } th { class: "num", {state::tr("already so")} } th { class: "num", {state::tr("skipped")} } } }
                    tbody {
                        for s in sections.iter() {
                            tr { td { "{config_section_label(&api::s(s, \"section\"))}" } td { class: "num", "{api::u(s, \"applied\")}" } td { class: "num", "{api::u(s, \"unchanged\")}" } td { class: "num", "{s[\"skipped\"].as_array().map_or(0, Vec::len)}" } }
                        }
                    }
                }
            }
            if !skipped.is_empty() {
                table { class: "grid",
                    thead { tr { th { {state::tr("section")} } th { {state::tr("skipped")} } th { {state::tr("why")} } } }
                    tbody {
                        for (section, item, reason) in skipped {
                            tr { td { "{config_section_label(&section)}" } td { class: "mono", "{item}" } td { class: "bad", "{reason}" } }
                        }
                    }
                }
            }
        }
    }
}

/// One cache row: its own component because "adopt ownership" is per-row state.
#[component]
fn CacheRow(file: CacheFile, watch: Signal<Option<u64>>, on_delete: EventHandler<String>) -> Element {
    let mut adopt = use_signal(|| false);
    let name = file.name.clone();
    let (n_import, n_delete) = (name.clone(), name.clone());
    rsx! {
        tr {
            td { class: "mono", "{file.name}" }
            td { Html { html: badge(&file.kind, "dim") } }
            td { class: "num", "{html::fmt_size(Some(file.bytes))}" }
            td { "{html::fmt_date(&file.created.to_string())}" }
            td { style: "white-space:nowrap",
                Download { kind: "cache".to_string(), name: file.name.clone() }
                " "
                span { class: "link",
                    onclick: move |_| { let name = n_import.clone(); let adopt = adopt(); spawn(async move {
                        let q = if adopt { state::trf("Import {name} and adopt its ownership tables?", &[("name", &name)]) } else { state::trf("Import {name}?", &[("name", &name)]) };
                        if !ui::confirm(&q).await { return; }
                        start_job(format!("/api/v1/cache/{}/import?adopt_ownership={adopt}", api::encode(&name)), watch);
                    }); },
                    {state::tr("import")}
                }
                " "
                label { class: "muted", title: state::tr("take the local_* ownership tables even if the file came from another installation"),
                    input { r#type: "checkbox", checked: adopt(), onchange: move |e| adopt.set(e.checked()) }
                    {state::tr(" adopt ownership")}
                }
                " "
                span { class: "link",
                    onclick: move |_| { let name = n_delete.clone(); spawn(async move {
                        if ui::confirm(&state::trf("Delete {name}?", &[("name", &name)])).await { on_delete.call(format!("/api/v1/cache/{}", api::encode(&name))); }
                    }); },
                    {state::tr("delete")}
                }
            }
        }
    }
}
