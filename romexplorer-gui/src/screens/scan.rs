//! Scan: scans and hash matches only, with the files a running scan is reading right now,
//! and the title merge pass a scan queues with the step it is on (the console's `showScan`).

use std::rc::Rc;

use dioxus::prelude::*;
use serde_json::{json, Value};

use super::common::bump;
use super::jobs::{enqueue, progress_cell, progress_full, steps_html};
use crate::api::{self, s, u};
use crate::components::{Column, Filter, Grid, GridSpec, Spec};
use crate::html::{self, badge, esc};
use crate::state;
use crate::ui;

const SCAN_TYPES: &str = "file_scan,hash_match";
/// Running now also shows the title merge pass (a scan queues one), with its steps.
const RUNNING_TYPES: &str = "file_scan,hash_match,title_dedupe";

/// The files a running scan is reading right now, one row each: name, what it is doing, how far.
pub fn running_files_html(j: &Value) -> String {
    let files = j.pointer("/progress_detail/files").and_then(|f| f.as_array()).cloned().unwrap_or_default();
    if files.is_empty() {
        return String::new();
    }
    let rows: String = files
        .iter()
        .map(|f| {
            let name = s(f, "name");
            let short = name.rsplit(['/', '\\']).next().unwrap_or(&name).to_string();
            let (size, done) = (u(f, "size"), u(f, "done"));
            let pct = if size > 0 { ((100 * done) / size).min(100) } else { 0 };
            let stage = match s(f, "stage").as_str() {
                "detect" => state::tr("detecting"),
                "extract" => state::tr("extracting"),
                "hash" => state::tr("hashing"),
                "convert" => state::tr("converting"),
                "archive" => state::tr("archiving"),
                _ => state::tr("reading"),
            };
            format!(
                "<div class=\"prog\" title=\"{}\"><span class=\"fname\">{}</span>{}<div class=\"pbar\"><div style=\"width:{pct}%\"></div></div><span class=\"num\">{pct}%</span><span class=\"muted\">{} / {}</span></div>",
                esc(&name),
                esc(&short),
                badge(&stage, "dim"),
                html::fmt_size(Some(done)),
                html::fmt_size(Some(size))
            )
        })
        .collect();
    format!("<div class=\"runfiles\">{rows}</div>")
}

/// Browser storage key for the DAT / Hasheous / PlayMatch boxes; the console's `pref('scan_match')`.
const SCAN_MATCH_PREF: &str = "rx_scan_match";

#[component]
pub fn Scan() -> Element {
    let reload = use_signal(|| 0u64);
    let mut running = use_signal(Vec::<Value>::new);
    let dirs = use_resource(|| async { api::get("/api/v1/directories").await.map(|v| api::items(&v)).unwrap_or_default() });
    let mut dir = use_signal(String::new);
    let mut platform = use_signal(String::new);
    let mut to_dat = use_signal(|| true);
    let mut to_hasheous = use_signal(|| false);
    let mut to_playmatch = use_signal(|| false);
    // The DAT / Hasheous / PlayMatch boxes are remembered on this device (same key as the
    // web console), so a refresh does not put them back to DAT only.
    use_future(move || async move {
        if let Some(saved) = crate::screens::bigpicture::pref(SCAN_MATCH_PREF).await {
            let on: Vec<&str> = saved.split(',').collect();
            to_dat.set(on.contains(&"dat"));
            to_hasheous.set(on.contains(&"hasheous"));
            to_playmatch.set(on.contains(&"playmatch"));
        }
    });
    let save_match = move || {
        let on: Vec<&str> = [("dat", to_dat()), ("hasheous", to_hasheous()), ("playmatch", to_playmatch())].into_iter().filter(|(_, v)| *v).map(|(k, _)| k).collect();
        crate::screens::bigpicture::set_pref(SCAN_MATCH_PREF, &on.join(","));
    };
    // The running scans every second, the list every 3.
    use_future(move || async move {
        let mut tick = 0u64;
        loop {
            let q = api::qs(&[("job_type", RUNNING_TYPES.to_string()), ("status", "in_progress".to_string()), ("limit", "20".to_string())]);
            if let Ok(page) = api::get(&format!("/api/v1/jobs{q}")).await {
                running.set(api::arr(&page, "jobs").to_vec());
            }
            tick += 1;
            if tick % 3 == 0 {
                bump(reload);
            }
            ui::sleep(1000).await;
        }
    });
    let dir_list = dirs.read().clone().unwrap_or_default();
    let dir_path = {
        let list = dir_list.clone();
        move |id: u64| list.iter().find(|d| u(d, "id") == id).map(|d| s(d, "path")).unwrap_or_default()
    };
    let spec = use_hook({
        move || {
            let columns = vec![
                Column::text("id", &state::tr("id")).num(),
                Column::text("job_type", &state::tr("type")),
                // Looked up when a row is drawn, not when the grid is set up: the directories
                // are still loading then.
                Column::html("directory", &state::tr("directory"), move |j| {
                    let id = j.pointer("/payload/directory_id").and_then(|v| v.as_u64()).unwrap_or(0);
                    let path = dirs.read().as_ref().and_then(|list| list.iter().find(|d| u(d, "id") == id).map(|d| s(d, "path"))).unwrap_or_default();
                    esc(&path)
                }),
                Column::html("status", &state::tr("status"), |j| {
                    let st = s(j, "status");
                    let cls = match st.as_str() {
                        "pending" | "in_progress" => "warn",
                        "done" => "ok",
                        "failed" => "bad",
                        _ => "dim",
                    };
                    badge(&st, cls)
                }),
                Column::render("x", &state::tr("action"), move |j| super::jobs::job_actions(j, reload)),
                Column::html("progress", &state::tr("progress"), progress_cell),
                Column::html("last_error", &state::tr("error"), |j| {
                    let e = s(j, "last_error");
                    format!("<span title=\"{}\">{}</span>", esc(&e), esc(&e.chars().take(120).collect::<String>()))
                }),
                Column::html("created_at", &state::tr("created"), |j| html::fmt_date(&s(j, "created_at"))),
                Column::html("updated_at", &state::tr("updated"), |j| html::fmt_date(&s(j, "updated_at"))),
            ];
            let mut spec = GridSpec::new("scan_jobs", columns, |q| async move {
                let p = vec![("job_type", SCAN_TYPES.to_string()), ("status", q.f("status")), ("limit", q.limit.to_string()), ("offset", q.offset.to_string())];
                api::get(&format!("/api/v1/jobs{}", api::qs(&p))).await
            })
            .filters(vec![Filter::Select {
                key: "status",
                label: state::tr("status"),
                options: Rc::new(|| {
                    let mut o = vec![(String::new(), state::tr("all statuses"))];
                    o.extend(state::vocab().pointer("/jobs/statuses").and_then(|p| p.as_array()).into_iter().flatten().filter_map(|x| x.as_str().map(|t| (t.to_string(), t.to_string()))));
                    o
                }),
            }]);
            spec.search = false;
            Spec(Rc::new(spec))
        }
    });
    let chosen_dir = if dir().is_empty() { dir_list.first().map(|d| u(d, "id").to_string()).unwrap_or_default() } else { dir() };
    let cd = chosen_dir.clone();
    let scan = move |mode: &'static str| {
        let d = cd.clone();
        if d.is_empty() {
            ui::toast(state::tr("pick a directory"), true);
            return;
        }
        let plat = platform();
        // The follow-up hash match uses the same boxes as the hash match buttons.
        let with = json!({ "to_dat": to_dat(), "to_hasheous": to_hasheous(), "to_playmatch": to_playmatch() });
        let body = json!({ "job_type": "file_scan", "directory_id": d.parse::<u64>().unwrap_or(0), "mode": mode, "platform": if plat.is_empty() { json!("all") } else { json!(plat.parse::<u64>().unwrap_or(0)) }, "hash_match": with });
        spawn(async move {
            if enqueue(body).await {
                bump(reload)
            }
        });
    };
    let cd2 = chosen_dir.clone();
    let hash_match = move |all: bool| {
        if cd2.is_empty() {
            ui::toast(state::tr("pick a directory"), true);
            return;
        }
        let body = json!({ "job_type": "hash_match", "directory_id": cd2.parse::<u64>().unwrap_or(0), "to_dat": to_dat(), "to_hasheous": to_hasheous(), "to_playmatch": to_playmatch(), "all": all });
        spawn(async move {
            if enqueue(body).await {
                bump(reload)
            }
        });
    };
    let running_now = running.read().clone();
    rsx! {
        div { class: "card",
            h2 { {state::tr("Scan / match a directory")} " " {ui::key_explain("explain.scan.form")} }
            if dir_list.is_empty() { p { class: "muted", {state::tr("No directories configured yet — add one under Config › Directories first.")} } }
            form { class: "inline", onsubmit: move |e| e.prevent_default(),
                label { {state::tr("directory")}
                    select { style: "min-width:320px", onchange: move |e| dir.set(e.value()),
                        for d in dir_list.clone() { option { value: "{u(&d, \"id\")}", selected: u(&d, "id").to_string() == chosen_dir, "{s(&d, \"path\")}" } }
                    }
                }
                label { {state::tr("platform")}
                    select { onchange: move |e| platform.set(e.value()),
                        option { value: "", {state::tr("all (conform to directory setting)")} }
                        for p in state::platforms() { option { value: "{u(&p, \"id\")}", "{s(&p, \"name\")}" } }
                    }
                }
                span {
                    button { class: "btn primary", r#type: "button", onclick: { let scan = scan.clone(); move |_| scan("full") }, {state::tr("Full scan")} }
                    crate::ui::QueueButton {}
                    " "
                    button { class: "btn", r#type: "button", onclick: { let scan = scan.clone(); move |_| scan("new_only") }, {state::tr("Quick scan (new only)")} }
                    crate::ui::QueueButton {}
                    " "
                    button { class: "btn", r#type: "button", onclick: { let scan = scan.clone(); move |_| scan("rehash") }, {state::tr("Rehash")} }
                    crate::ui::QueueButton {}
                    " "
                    button { class: "btn", r#type: "button", onclick: { let hash_match = hash_match.clone(); move |_| hash_match(false) }, {state::tr("Hash match (new)")} }
                    crate::ui::QueueButton {}
                    " "
                    button { class: "btn", r#type: "button", onclick: { let hash_match = hash_match.clone(); move |_| hash_match(true) }, {state::tr("Hash match (all)")} }
                    crate::ui::QueueButton {}
                }
                label { class: "muted", style: "flex-direction:row;align-items:center;gap:6px", input { r#type: "checkbox", checked: to_dat(), onchange: move |e| { to_dat.set(e.checked()); save_match() } } " DAT" }
                label { class: "muted", style: "flex-direction:row;align-items:center;gap:6px", input { r#type: "checkbox", checked: to_hasheous(), onchange: move |e| { to_hasheous.set(e.checked()); save_match() } } " Hasheous" }
                label { class: "muted", style: "flex-direction:row;align-items:center;gap:6px", input { r#type: "checkbox", checked: to_playmatch(), onchange: move |e| { to_playmatch.set(e.checked()); save_match() } } " PlayMatch" }
            }
        }
        div { class: "card",
            h2 { {state::tr("Running now")} " " {ui::key_explain("explain.scan.running")} }
            if running_now.is_empty() {
                div { class: "muted", {state::tr("nothing running")} }
            }
            for j in running_now.iter() {
                div { style: "margin-bottom:8px",
                    div { class: "toolbar",
                        ui::Html { html: badge(&format!("#{} {}", u(j, "id"), s(j, "job_type")), "warn") }
                        span { class: "mono muted", "{dir_path(j.pointer(\"/payload/directory_id\").and_then(|v| v.as_u64()).unwrap_or(0))}" }
                        span { class: "grow" }
                        button { class: "btn small danger",
                            onclick: {
                                let id = u(j, "id");
                                move |_| {
                                    spawn(async move {
                                        if !ui::confirm(&state::trf("Cancel running job #{id}? It stops within a few seconds; work already saved stays, the rest is redone by a later run.", &[("id", &id.to_string())])).await { return; }
                                        match api::post(&format!("/api/v1/jobs/{id}/cancel"), json!({})).await {
                                            Ok(r) => { ui::toast(if s(&r, "outcome") == "cancelled" { state::trf("cancelled #{id}", &[("id", &id.to_string())]) } else { state::trf("cancelling #{id}…", &[("id", &id.to_string())]) }, false); bump(reload); }
                                            Err(e) => ui::toast(e.message, true),
                                        }
                                    });
                                }
                            },
                            {state::tr("cancel")}
                        }
                    }
                    ui::HtmlDiv { html: format!("{}{}{}", progress_full(j), steps_html(j, true), running_files_html(j)) }
                }
            }
        }
        div { class: "card", h2 { {state::tr("Scans and hash matches")} } Grid { spec, reload: Some(reload) } }
    }
}
