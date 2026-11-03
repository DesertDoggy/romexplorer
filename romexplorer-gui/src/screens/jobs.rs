//! Jobs: start scans / matches / syncs, and the job list with live progress (the console's
//! `showJobs`).

use std::rc::Rc;

use dioxus::prelude::*;
use serde_json::{json, Value};

use super::common::bump;
use crate::api::{self, s, u};
use crate::components::{Column, Filter, Grid, GridSpec, Spec};
use crate::html::{self, badge, esc};
use crate::state;
use crate::ui;

/// A job's payload, shortened: long arrays show their first few items and a count; the whole
/// JSON is in the tooltip.
pub fn payload_cell(j: &Value) -> String {
    fn shorten(v: &Value) -> Value {
        match v {
            Value::Array(a) if a.len() > 4 => {
                let mut out: Vec<Value> = a.iter().take(3).map(shorten).collect();
                out.push(Value::String(format!("…+{}", a.len() - 3)));
                Value::Array(out)
            }
            Value::Array(a) => Value::Array(a.iter().map(shorten).collect()),
            Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), shorten(v))).collect()),
            x => x.clone(),
        }
    }
    let p = j.get("payload").cloned().unwrap_or(Value::Null);
    let full = p.to_string();
    let short = shorten(&p).to_string();
    let text: String = if short.chars().count() > 90 { format!("{}…", short.chars().take(88).collect::<String>()) } else { short };
    let tip: String = if full.chars().count() > 2000 { format!("{}…", full.chars().take(2000).collect::<String>()) } else { full };
    format!("<span title=\"{}\" style=\"white-space:nowrap\">{}</span>", esc(&tip), esc(&text))
}

/// A job's live progress: a bar when the total is known, else the count, plus the message.
pub fn progress_cell(j: &Value) -> String {
    progress_bar(j, false)
}

/// The same with the whole message, wrapping onto more lines when it is long: for a running
/// job shown on its own (the Scan tab's Running now), where there is room for all of it.
pub fn progress_full(j: &Value) -> String {
    progress_bar(j, true)
}

/// A step's label in the UI language, by its key; the English the worker sent otherwise.
fn step_label(step: &Value) -> String {
    match s(step, "key").as_str() {
        "bundle" => state::tr("Bundle DLC and updates with their games"),
        "patched" => state::tr("One release per patched work"),
        "bases" => state::tr("Link patched releases to their originals"),
        "merge" => state::tr("Merge titles that share a game"),
        "versions" => state::tr("Find each game's official versions"),
        "basegames" => state::tr("Link ports, remakes and add-ons to their base game"),
        "families" => state::tr("Collections, families and definitive versions"),
        _ => s(step, "label"),
    }
}

/// m:ss, as the console's `fmtSecs`.
fn fmt_secs(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// How far the running step is, in percent, from the bar's numbers: each step has `span`
/// of them from `base` (`jobs::Steps` on the server).
fn running_step_pct(j: &Value, d: &Value) -> Option<u64> {
    let (done, total, base, span) = (api::uo(j, "progress_done")?, u(j, "progress_total"), u(d, "base"), u(d, "span"));
    let steps = d.get("steps").and_then(|x| x.as_array()).map_or(0, |a| a.len() as u64);
    if span == 0 || total != steps * span {
        return None;
    }
    Some((100 * done.saturating_sub(base) / span).min(100))
}

/// The steps of a job made of them (the title dedupe pass), one row each: done with how it
/// came out and how long it took, running with its own bar, or still to come. `full` is the
/// list; otherwise one mark per step for a grid cell, the list in its tooltip.
pub fn steps_html(j: &Value, full: bool) -> String {
    let Some(d) = j.get("progress_detail").filter(|d| d.get("steps").is_some_and(|x| x.is_array())) else { return String::new() };
    let steps = d["steps"].as_array().cloned().unwrap_or_default();
    let mark = |st: &str| match st {
        "done" => "✓",
        "running" => "●",
        _ => "○",
    };
    if !full {
        let tip: Vec<String> = steps
            .iter()
            .enumerate()
            .map(|(i, st)| {
                let note = s(st, "note");
                format!("{} {}. {}{}", mark(&s(st, "state")), i + 1, step_label(st), if note.is_empty() { String::new() } else { format!(" — {note}") })
            })
            .collect();
        let marks: String = steps.iter().map(|st| format!("<span class=\"{}\">{}</span>", esc(&s(st, "state")), mark(&s(st, "state")))).collect();
        return format!("<span class=\"stepmarks\" title=\"{}\">{marks}</span>", esc(&tip.join("\n")));
    }
    let rows: String = steps
        .iter()
        .enumerate()
        .map(|(i, st)| {
            let state_name = s(st, "state");
            let mut tail = String::new();
            if state_name == "running" {
                if let Some(pct) = running_step_pct(j, d) {
                    tail.push_str(&format!("<div class=\"pbar\"><div style=\"width:{pct}%\"></div></div><span class=\"num\">{pct}%</span>"));
                }
            }
            let note = s(st, "note");
            let secs = api::uo(st, "secs").map(fmt_secs);
            let said: Vec<String> = [Some(note).filter(|n| !n.is_empty()), secs].into_iter().flatten().collect();
            if !said.is_empty() {
                tail.push_str(&format!("<span class=\"muted note\">{}</span>", esc(&said.join(" · "))));
            }
            format!("<div class=\"step {}\"><span class=\"mark\">{}</span><span class=\"slabel\">{}. {}</span>{tail}</div>", esc(&state_name), mark(&state_name), i + 1, esc(&step_label(st)))
        })
        .collect();
    let scope = s(d, "scope");
    let head = if scope.is_empty() { String::new() } else { format!("<div class=\"muted\">{}</div>", esc(&scope)) };
    format!("<div class=\"runsteps\">{head}{rows}</div>")
}

fn progress_bar(j: &Value, full: bool) -> String {
    let done = api::uo(j, "progress_done");
    let msg = s(j, "progress_message");
    if done.is_none() && msg.is_empty() {
        return String::new();
    }
    let total = u(j, "progress_total");
    let bar = if total > 0 {
        // Rounded down: 100% only once it is all done, never while the last bit is still going.
        let pct = (100.0 * done.unwrap_or(0) as f64 / total as f64).floor().min(100.0) as u64;
        format!("<div class=\"pbar\"><div style=\"width:{pct}%\"></div></div><span class=\"num\">{pct}%</span>")
    } else if let Some(d) = done {
        format!("<span class=\"num\">{d}</span>")
    } else {
        String::new()
    };
    let shown: String = if full { msg.clone() } else { msg.chars().take(90).collect() };
    let marks = if full { String::new() } else { steps_html(j, false) };
    format!("<div class=\"prog{}\" title=\"{}\">{bar}{marks}<span class=\"muted\">{}</span></div>", if full { " full" } else { "" }, esc(&msg), esc(&shown))
}

/// `POST /api/v1/jobs/clear` with `body`; says what happened. `true` when it went through.
async fn clear_jobs(body: Value) -> bool {
    match api::post("/api/v1/jobs/clear", body).await {
        Ok(r) => {
            let (n, kept) = (u(&r, "removed").to_string(), u(&r, "kept"));
            ui::toast(if kept > 0 { state::trf("{n} job(s) cleared; {kept} still pending or running were kept", &[("n", &n), ("kept", &kept.to_string())]) } else { state::trf("{n} job(s) cleared", &[("n", &n)]) }, false);
            true
        }
        Err(e) => {
            ui::toast(e.message, true);
            false
        }
    }
}

pub async fn enqueue(body: Value) -> bool {
    match api::post("/api/v1/jobs", body).await {
        Ok(r) => {
            ui::toast(state::trf("enqueued {job} #{id}", &[("job", &s(&r, "job_type")), ("id", &u(&r, "id").to_string())]), false);
            true
        }
        Err(e) => {
            ui::toast(e.message, true);
            false
        }
    }
}

fn vocab_list(path: &str) -> Vec<String> {
    state::vocab().pointer(path).and_then(|p| p.as_array()).into_iter().flatten().filter_map(|x| x.as_str().map(str::to_string)).collect()
}

/// "Run next" (the console's `runNextJobs`): pending jobs to the front of their lanes,
/// keeping their order among themselves. `true` when it went through.
pub async fn run_next_jobs(ids: Vec<u64>) -> bool {
    match api::post("/api/v1/jobs/run-next", json!({ "ids": ids })).await {
        Ok(r) => {
            let moved: Vec<&Value> = r.get("positions").and_then(Value::as_array).into_iter().flatten().filter(|p| p.get("position").is_some_and(|v| !v.is_null())).collect();
            if let [one] = moved.as_slice() {
                ui::toast(state::trf("#{id} runs next in its lane (position {n})", &[("id", &u(one, "id").to_string()), ("n", &u(one, "position").to_string())]), false);
            } else {
                ui::toast(state::trf("{n} job(s) moved to the front of their lanes", &[("n", &moved.len().to_string())]), false);
            }
            true
        }
        Err(e) => {
            ui::toast(e.message, true);
            false
        }
    }
}

/// A jobs row's buttons (the console's `jobActions`): retry a failed or cancelled job, run a
/// waiting one next, cancel a waiting or running one, and for a finished one with a result
/// (a scan's errors and warnings, say) open it.
pub fn job_actions(j: &Value, reload: Signal<u64>) -> Element {
    let id = u(j, "id");
    let st = s(j, "status");
    let finished = matches!(st.as_str(), "done" | "failed" | "cancelled");
    let has_result = finished && j.get("result").is_some_and(|r| !r.is_null());
    let main = match st.as_str() {
        "failed" | "cancelled" => rsx! {
            button { class: "btn small",
                onclick: move |_| { spawn(async move { match api::post(&format!("/api/v1/jobs/{id}/retry"), json!({})).await { Ok(_) => { ui::toast(state::trf("retrying #{id}", &[("id", &id.to_string())]), false); bump(reload); } Err(e) => ui::toast(e.message, true) } }); },
                {state::tr("retry")}
            }
        },
        "pending" | "in_progress" => {
            let running = st == "in_progress";
            rsx! {
                if !running {
                    button { class: "btn small", title: state::tr("Starts after the job running in its lane; jobs in other lanes are not affected."),
                        onclick: move |_| { spawn(async move { if run_next_jobs(vec![id]).await { bump(reload); } }); },
                        {state::tr("run next")}
                    }
                    " "
                }
                button { class: "btn small danger",
                    onclick: move |_| {
                        spawn(async move {
                            if running && !ui::confirm(&state::trf("Cancel running job #{id}? It stops within a few seconds; work already saved stays, the rest is redone by a later run.", &[("id", &id.to_string())])).await { return; }
                            match api::post(&format!("/api/v1/jobs/{id}/cancel"), json!({})).await {
                                Ok(r) => { ui::toast(if s(&r, "outcome") == "cancelled" { state::trf("cancelled #{id}", &[("id", &id.to_string())]) } else { state::trf("cancelling #{id}…", &[("id", &id.to_string())]) }, false); bump(reload); }
                                Err(e) => ui::toast(e.message, true),
                            }
                        });
                    },
                    {state::tr("cancel")}
                }
            }
        }
        _ => rsx! {},
    };
    rsx! {
        {main}
        if has_result {
            " "
            button { class: "btn small", onclick: move |_| show_job_result(id), {state::tr("result")} }
        }
    }
}

/// A finished job's result in a dialog (the console's `showJobResult`), read fresh.
fn show_job_result(id: u64) {
    spawn(async move {
        let j = match api::get(&format!("/api/v1/jobs/{id}")).await {
            Ok(j) => j,
            Err(e) => return ui::toast(e.message, true),
        };
        let title = format!("#{id} {}", s(&j, "job_type"));
        let message = s(&j, "progress_message");
        let error = s(&j, "last_error");
        let body = j.get("result").map(super::backup::result_html).unwrap_or_default();
        ui::open_modal(move || {
            rsx! {
                h2 { "{title}" }
                if !message.is_empty() { p { class: "muted", "{message}" } }
                if !error.is_empty() { div { class: "bad", "{error}" } }
                div { style: "max-height:65vh; overflow:auto", ui::HtmlDiv { html: body.clone() } }
                div { class: "fsfoot", span { class: "grow" } button { r#type: "button", class: "btn", onclick: move |_| ui::close_modal(), {state::tr("close")} } }
            }
        });
    });
}

#[component]
pub fn Jobs() -> Element {
    let reload = use_signal(|| 0u64);
    let mut source = use_signal(String::new);
    let mut media_scope = use_signal(|| "owned".to_string());
    let mut media_platform = use_signal(String::new);
    // One provider's art, of the types ticked; none: every provider whose art comes on its own.
    let mut media_provider = use_signal(String::new);
    let mut media_types = use_signal(|| super::art::DEFAULT_TICKED.iter().map(|s| s.to_string()).collect::<Vec<String>>());
    let mut disp_sfo = use_signal(|| true);
    let mut disp_pcsx2 = use_signal(|| true);
    let mut disp_ipbin = use_signal(|| true);
    let mut disp_mame = use_signal(|| false);
    let mut ra_scope = use_signal(|| "played".to_string());
    // A client installation may not build the catalogue: the server refuses those jobs, so
    // say so on the button rather than after the click.
    let mode = use_resource(|| async { api::get_as::<romexplorer_types::sources::ModeView>("/api/v1/mode").await });
    let client = mode.read().as_ref().and_then(|r| r.as_ref().ok()).is_some_and(|m| m.mode == "client");
    let providers = use_resource(|| async { api::get("/api/v1/providers").await.map(|v| api::items(&v)) });
    let client_title = if client { state::tr("refused in client mode: the catalogue comes from the main server") } else { String::new() };
    // The list refreshes every 2 s while the page is open.
    use_future(move || async move {
        loop {
            ui::sleep(2000).await;
            bump(reload);
        }
    });
    let spec = use_hook(move || {
        let columns = vec![
            Column::text("no", &state::tr("No.")).num(),
            Column::text("job_type", &state::tr("type")),
            Column::text("id", &state::tr("id")).num(),
            Column::html("lane", &state::tr("lane"), |j| {
                let lane = s(j, "lane");
                match lane.strip_prefix("provider:") {
                    Some(p) => format!("{}{}", html::provider_icon(p), esc(p)),
                    None => badge(&lane, "dim"),
                }
            }),
            Column::html("status", &state::tr("status"), |j| {
                let st = s(j, "status");
                let cls = match st.as_str() {
                    "pending" | "in_progress" => "warn",
                    "done" => "ok",
                    "failed" => "bad",
                    _ => "dim",
                };
                let status = badge(&format!("{st}{}", if api::b(j, "cancel_requested") { state::tr(" (cancelling)") } else { String::new() }), cls);
                // Added with a queue button: it waits for everything else, not just its lane.
                if st == "pending" && api::b(j, "in_turn") {
                    format!("{status} <span title=\"{}\">{}</span>", esc(&state::tr("queued: starts when nothing else is running or waiting")), badge(&state::tr("in turn"), "dim"))
                } else {
                    status
                }
            }),
            Column::render("x", &state::tr("action"), move |j| job_actions(j, reload)),
            Column::html("progress", &state::tr("progress"), progress_cell),
            Column::html("payload", &state::tr("payload"), payload_cell),
            Column::text("attempts", &state::tr("attempts")).num(),
            Column::html("last_error", &state::tr("error"), |j| {
                let e = s(j, "last_error");
                format!("<span title=\"{}\">{}</span>", esc(&e), esc(&e.chars().take(120).collect::<String>()))
            }),
            Column::html("created_at", &state::tr("created"), |j| html::fmt_date(&s(j, "created_at"))),
            Column::html("updated_at", &state::tr("updated"), |j| html::fmt_date(&s(j, "updated_at"))),
        ];
        let mut spec = GridSpec::new("jobs", columns, |q| async move {
            let p = vec![("job_type", q.f("job_type")), ("status", q.f("status")), ("limit", q.limit.to_string()), ("offset", q.offset.to_string())];
            // No.: the row's place in the list as shown, counted across pages.
            let mut page = api::get(&format!("/api/v1/jobs{}", api::qs(&p))).await?;
            if let Some(jobs) = page.get_mut("jobs").and_then(Value::as_array_mut) {
                for (i, j) in jobs.iter_mut().enumerate() {
                    j["no"] = json!(q.offset + i as u64 + 1);
                }
            }
            Ok(page)
        })
        .filters(vec![
            Filter::Select {
                key: "job_type",
                label: state::tr("type"),
                options: Rc::new(|| {
                    let mut o = vec![(String::new(), state::tr("all types"))];
                    o.extend(vocab_list("/jobs/job_types").into_iter().map(|t| (t.clone(), t)));
                    o
                }),
            },
            Filter::Select {
                key: "status",
                label: state::tr("status"),
                options: Rc::new(|| {
                    let mut o = vec![(String::new(), state::tr("all statuses"))];
                    o.extend(vocab_list("/jobs/statuses").into_iter().map(|t| (t.clone(), t)));
                    o
                }),
            },
        ]);
        spec.search = false;
        // By status: finished jobs only. A selection also removes a job that never started
        // and asks a running one to stop (it stays, cancelled, until cleared).
        let spec = spec.select(
            |j| u(j, "id"),
            vec![
                crate::components::SelAction::new(&state::tr("run next"), |ids| async move { run_next_jobs(ids).await }),
                crate::components::SelAction::new(&state::tr("clear selected from the list"), |ids| async move { clear_jobs(json!({ "ids": ids })).await }),
            ],
        );
        Spec(Rc::new(spec))
    });
    let dat_plats = super::widgets::use_dat_platforms();
    rsx! {
        div { class: "card",
            p { class: "muted", {state::tr("Scans and hash matches have their own tab, with the files being read right now:")} " "
                Link { class: "link", to: crate::Route::Scan {}, {state::tr("Scan")} }
            }
        }
        // One box per kind of job, so each is found by its name.
        div { class: "card",
            h2 { {state::tr("DAT sync")} " " {ui::key_explain("explain.jobs.other")} }
            form { class: "inline", onsubmit: move |e| e.prevent_default(),
                label { {state::tr("DAT source")}
                    select { onchange: move |e| source.set(e.value()),
                        option { value: "", {state::tr("all sources")} }
                        for x in vocab_list("/jobs/dat_sources") { option { "{x}" } }
                    }
                }
                button { class: "btn", r#type: "button", disabled: client, title: "{client_title}", onclick: move |_| { let src = source(); let Some(body) = super::widgets::with_dat_platforms(dat_plats, json!({ "job_type": "dat_sync", "source": if src.is_empty() { Value::Null } else { json!(src) }, "import": true })) else { return }; spawn(async move { if enqueue(body).await { bump(reload) } }); }, {state::tr("DAT sync (download + import)")} }
                crate::ui::QueueButton {}
            }
            super::widgets::DatPlatformPicker { pick: dat_plats }
        }
        div { class: "card",
            h2 { {state::tr("Media download")} }
            form { class: "inline", onsubmit: move |e| e.prevent_default(),
                label { {state::tr("media for")}
                    select { onchange: move |e| media_scope.set(e.value()),
                        option { value: "owned", {state::tr("owned games")} }
                        option { value: "all", {state::tr("the whole catalogue")} }
                    }
                }
                label { {state::tr("platform")}
                    select { onchange: move |e| media_platform.set(e.value()),
                        option { value: "", {state::tr("all platforms")} }
                        for p in state::platforms() { option { value: "{u(&p, \"id\")}", "{s(&p, \"name\")}" } }
                    }
                }
                label { {state::tr("from")}
                    select { onchange: move |e| media_provider.set(e.value()),
                        option { value: "", {state::tr("every provider (the media settings)")} }
                        for p in super::art::art_providers() { option { value: "{p.name}", "{p.name}" } }
                    }
                }
                if let Some(p) = super::art::art_providers().into_iter().find(|p| p.name == media_provider()) {
                    for slot in p.slots.clone() {
                        label {
                            input { r#type: "checkbox", checked: media_types().contains(&slot),
                                onchange: { let slot = slot.clone(); move |e: Event<FormData>| { let mut t = media_types.write(); t.retain(|x| x != &slot); if e.checked() { t.push(slot.clone()); } } } }
                            " " {super::art::slot_label(&slot)}
                        }
                    }
                }
                button { class: "btn", r#type: "button",
                    onclick: move |_| {
                        let plat = media_platform();
                        let provider = media_provider();
                        let slots: Vec<String> = super::art::art_providers().into_iter().find(|p| p.name == provider).map(|p| p.slots).unwrap_or_default();
                        let types: Vec<String> = media_types().into_iter().filter(|t| slots.contains(t)).collect();
                        let body = if provider.is_empty() {
                            json!({ "job_type": "media_backfill", "scope": media_scope(), "platform": plat.parse::<u64>().ok() })
                        } else {
                            json!({ "job_type": "media_backfill", "scope": media_scope(), "platform": plat.parse::<u64>().ok(), "providers": [provider], "types": types })
                        };
                        spawn(async move { if enqueue(body).await { bump(reload) } });
                    },
                    {state::tr("download missing media")}
                }
                crate::ui::QueueButton {}
                {ui::key_explain("explain.jobs.media_backfill")}
            }
            form { class: "inline", style: "margin-top:8px", onsubmit: move |e| e.prevent_default(),
                button { class: "btn", r#type: "button", title: state::tr("move media the app stored in its own folder into the Platform/Title/Region layout and into the configured image / video / audio dirs (only app-written files; user files are never touched)"), onclick: move |_| { spawn(async move { if enqueue(json!({ "job_type": "media_relayout" })).await { bump(reload) } }); }, {state::tr("media relayout")} }
                crate::ui::QueueButton {}
            }
        }
        div { class: "card",
            h2 { {state::tr("RetroAchievements data")} }
            form { class: "inline", onsubmit: move |e| e.prevent_default(),
                label { {state::tr("RetroAchievements games")}
                    select { onchange: move |e| ra_scope.set(e.value()),
                        option { value: "played", {state::tr("played by the account")} }
                        option { value: "owned", {state::tr("owned games")} }
                    }
                }
                button { class: "btn", r#type: "button",
                    onclick: move |_| {
                        let body = json!({ "job_type": "ra_badges", "scope": ra_scope() });
                        spawn(async move { if enqueue(body).await { bump(reload) } });
                    },
                    {state::tr("RA badges download/update")}
                }
                crate::ui::QueueButton {}
                {ui::key_explain("explain.jobs.ra_badges")}
            }
        }
        if let Some(Ok(list)) = &*providers.read() { super::data::ProviderRequest { list: list.clone() } }
        IntegrityCard { reload }
        super::sheets::SheetCard { reload }
        div { class: "card",
            h2 { {state::tr("Display data")} }
            form { class: "inline", onsubmit: move |e| e.prevent_default(),
                span { {state::tr("display data from")} }
                label { input { r#type: "checkbox", checked: disp_sfo(), onchange: move |e| disp_sfo.set(e.checked()) } {state::tr(" PS3 games' PARAM.SFO")} }
                label { input { r#type: "checkbox", checked: disp_ipbin(), onchange: move |e| disp_ipbin.set(e.checked()) } {state::tr(" Dreamcast discs' IP.BIN (VGA)")} }
                label { input { r#type: "checkbox", checked: disp_pcsx2(), onchange: move |e| disp_pcsx2.set(e.checked()) } {state::tr(" PCSX2's widescreen patches (download)")} }
                label { input { r#type: "checkbox", checked: disp_mame(), onchange: move |e| disp_mame.set(e.checked()) } {state::tr(" MAME's machine list (large download)")} }
                button { class: "btn", r#type: "button",
                    onclick: move |_| {
                        let sources: Vec<&str> = [("sfo", disp_sfo()), ("ipbin", disp_ipbin()), ("pcsx2", disp_pcsx2()), ("mame", disp_mame())].into_iter().filter(|(_, on)| *on).map(|(s, _)| s).collect();
                        if sources.is_empty() {
                            ui::toast(state::tr("pick at least one source"), true);
                            return;
                        }
                        let body = json!({ "job_type": "display_data", "sources": sources });
                        spawn(async move { if enqueue(body).await { bump(reload) } });
                    },
                    {state::tr("collect display data")}
                }
                crate::ui::QueueButton {}
            }
        }
        div { class: "card",
            h2 { {state::tr("Jobs")} " " {ui::key_explain("explain.jobs.list")} }
            div { class: "toolbar",
                span { class: "muted", {state::tr("clear from the list:")} }
                for (status, label) in [("done", state::tr("done")), ("failed", state::tr("failed")), ("cancelled", state::tr("cancelled")), ("", state::tr("all finished"))] {
                    button { class: "btn small", r#type: "button",
                        onclick: move |_| {
                            spawn(async move {
                                let question = if status.is_empty() { state::tr("Clear every finished job (done, failed, cancelled) from the list?") } else { state::trf("Clear every {status} job from the list?", &[("status", &state::tr(status))]) };
                                if !ui::confirm(&question).await { return; }
                                let body = if status.is_empty() { json!({}) } else { json!({ "statuses": [status] }) };
                                if clear_jobs(body).await { bump(reload); }
                            });
                        },
                        "{label}"
                    }
                }
            }
            Grid { spec, reload: Some(reload) }
        }
    }
}

/// Re-reads held files and checks one quick hash against the catalogue.
///
/// Deliberately not a rehash scan: that re-derives every file's full identity, which is the
/// right thing when you do not know what a file is. Here the `content` row already says, and
/// the only open question is whether the bytes still match -- one algorithm, and a limit, so
/// it can run often and briefly rather than seldom and for hours.
#[component]
fn IntegrityCard(reload: Signal<u64>) -> Element {
    let dirs = use_resource(|| async { api::get("/api/v1/directories").await.map(|v| api::items(&v)).unwrap_or_default() });
    let mut directory = use_signal(String::new);
    let mut limit = use_signal(|| "500".to_string());
    let mut days = use_signal(String::new);
    let list = dirs.read().clone().unwrap_or_default();
    let info = use_resource(|| async { api::get("/api/v1/integrity").await.ok() });
    let info = info.read().clone().flatten().unwrap_or_default();
    let populate = s(&info, "populate");
    let order = api::arr(&info, "compare_order").iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(" → ");
    rsx! {
        div { class: "card",
            h2 { {state::tr("Integrity check")} }
            ui::Explain { p { {state::tr("Reads each file this machine holds once and compares it with the hash the catalogue records for those bytes: the one set for the integrity check (")}
                b { "{populate}" }
                {state::tr(") when the catalogue has it, otherwise the cheapest one it has on this machine (")}
                b { "{order}" }
                {state::tr("). After such a fallback it also records ")}
                b { "{populate}" }
                {state::tr(" off the same read, once the file has matched, so later checks use it. Least recently checked first, so short runs repeated over time sweep the whole collection; a file that fails isn't stamped, so it stays at the front of the queue. Choose the hash under Config › Settings › Scanning & files (integrity_populate_hash).")} } }
            form { class: "inline", onsubmit: move |e| e.prevent_default(),
                label { {state::tr("directory")}
                    select { onchange: move |e| directory.set(e.value()),
                        option { value: "", {state::tr("every directory")} }
                        for d in list.iter() { option { value: "{u(d, \"id\")}", "{s(d, \"path\")}" } }
                    }
                }
                label { {state::tr("at most")} input { r#type: "number", min: "0", style: "width:90px", value: "{limit}", oninput: move |e| limit.set(e.value()) } }
                label { {state::tr("not checked for (days)")} input { r#type: "number", min: "0", style: "width:90px", placeholder: state::tr("any"), value: "{days}", oninput: move |e| days.set(e.value()) } }
                button { class: "btn", r#type: "button",
                    onclick: move |_| {
                        let body = json!({
                            "job_type": "integrity_check",
                            "directory_id": directory().parse::<u64>().ok(),
                            "limit": limit().parse::<u64>().ok().filter(|n| *n > 0),
                            "older_than_days": days().parse::<u64>().ok().filter(|n| *n > 0),
                        });
                        spawn(async move { if enqueue(body).await { bump(reload) } });
                    },
                    {state::tr("check now")}
                }
                crate::ui::QueueButton {}
            }
        }
    }
}
