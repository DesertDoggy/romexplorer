//! "Download art…": one provider's art, of the types ticked, for what the user picked (titles,
//! releases, sets). The same `media_backfill` job the Jobs screen runs, narrowed down -- and the
//! only way art from a provider with a small allowance (TheGamesDB) is ever fetched.

use dioxus::prelude::*;
use romexplorer_types::copy::CopySelection;
use serde_json::json;

use super::backup::JobWatch;
use super::copy::RunningJobButtons;
use crate::api::{self, s};
use crate::state;
use crate::ui::{self, MenuItem};

/// Ticked by default: what an emulator shows (box, title screen, in-game shots, logo).
pub(crate) const DEFAULT_TICKED: [&str; 4] = ["box:front", "title_screen", "screenshot", "logo"];

/// The last provider and types used, for the next dialog this session.
static LAST: GlobalSignal<Option<(String, Vec<String>)>> = Signal::global(|| None);

/// A provider that has art to fetch, and what it can be asked for (the vocabulary's
/// `media.providers`).
#[derive(Clone, PartialEq)]
pub(crate) struct ArtProvider {
    pub name: String,
    pub slots: Vec<String>,
    pub manual_only: bool,
}

pub(crate) fn art_providers() -> Vec<ArtProvider> {
    api::arr(&state::vocab()["media"], "providers")
        .iter()
        .map(|p| ArtProvider {
            name: s(p, "name"),
            slots: api::arr(p, "slots").iter().filter_map(|t| t.as_str().map(str::to_string)).collect(),
            manual_only: p.get("manual_only").and_then(|v| v.as_bool()).unwrap_or(false),
        })
        .filter(|p| !p.slots.is_empty())
        .collect()
}

/// `box:front` -> "box front", for a checkbox.
pub(crate) fn slot_label(slot: &str) -> String {
    match slot {
        "box:front" => state::tr("box front"),
        "box:back" => state::tr("box back"),
        "title_screen" => state::tr("title screen"),
        "screenshot" => state::tr("screenshots"),
        "box_3d" => state::tr("3D box"),
        other => state::tr(&other.replace([':', '_'], " ")),
    }
}

pub fn art_dialog(selection: CopySelection, what: String) {
    ui::open_modal(move || rsx! { ArtDialog { selection: selection.clone(), what: what.clone() } });
}

/// "Download art…" for a context menu on one row, or on the whole selection when several rows
/// are selected and the clicked one is among them.
pub fn context_item(kind: &'static str, id: u64, name: &str, ids: Option<Vec<u64>>) -> MenuItem {
    let many = ids.as_ref().filter(|v| v.len() > 1).cloned();
    let (label, ids, what) = match many {
        Some(v) => (state::trf("Download art for {n} selected {kind}…", &[("n", &v.len().to_string()), ("kind", &state::kind_word(kind, false))]), v.clone(), state::trf("{n} selected {kind}", &[("n", &v.len().to_string()), ("kind", &state::kind_word(kind, false))])),
        None => (state::tr("Download art…"), vec![id], if name.is_empty() { state::trf("this {kind}", &[("kind", &state::kind_word(kind, true))]) } else { name.to_string() }),
    };
    let selection = match kind {
        "titles" => CopySelection { title_row_ids: ids, ..Default::default() },
        "sets" => CopySelection { set_ids: ids, ..Default::default() },
        _ => CopySelection { release_ids: ids, ..Default::default() },
    };
    MenuItem::new(label, move || art_dialog(selection.clone(), what.clone()))
}

fn selection_of(kind: &str, ids: Vec<u64>) -> CopySelection {
    match kind {
        "titles" => CopySelection { title_row_ids: ids, ..Default::default() },
        "sets" => CopySelection { set_ids: ids, ..Default::default() },
        _ => CopySelection { release_ids: ids, ..Default::default() },
    }
}

pub fn export_dialog(selection: CopySelection, what: String) {
    ui::open_modal(move || rsx! { ExportDialog { selection: selection.clone(), what: what.clone() } });
}

/// "Export artwork…" for a context menu: this machine's art, into an emulator's folders.
pub fn export_context_item(kind: &'static str, id: u64, name: &str, ids: Option<Vec<u64>>) -> MenuItem {
    let many = ids.as_ref().filter(|v| v.len() > 1).cloned();
    let (label, ids, what) = match many {
        Some(v) => (state::trf("Export artwork for {n} selected {kind}…", &[("n", &v.len().to_string()), ("kind", &state::kind_word(kind, false))]), v.clone(), state::trf("{n} selected {kind}", &[("n", &v.len().to_string()), ("kind", &state::kind_word(kind, false))])),
        None => (state::tr("Export artwork…"), vec![id], if name.is_empty() { state::trf("this {kind}", &[("kind", &state::kind_word(kind, true))]) } else { name.to_string() }),
    };
    let selection = selection_of(kind, ids);
    MenuItem::new(label, move || export_dialog(selection.clone(), what.clone()))
}

/// The formats a layout's reader takes, best first, when nothing says otherwise.
fn layout_formats(layout: &str) -> Vec<String> {
    if layout == "retroarch" { vec!["png".to_string()] } else { vec!["jpg".to_string(), "png".to_string(), "webp".to_string()] }
}

/// "Export artwork…": into any folder, in RetroArch's thumbnails layout or as covers named by
/// serial. An emulator with an artwork folder set only fills the form in as a starting point.
#[component]
fn ExportDialog(selection: CopySelection, what: String) -> Element {
    let emulators = use_resource(|| async { api::get("/api/v1/emulators").await.map(|v| api::items(&v)) });
    let dir = use_signal(String::new);
    let mut layout = use_signal(|| "retroarch".to_string());
    let mut formats = use_signal(|| layout_formats("retroarch"));
    let mut width = use_signal(|| "512".to_string());
    let mut from = use_signal(|| 0u64);
    let mut job = use_signal(|| None::<u64>);
    let mut finished = use_signal(|| false);
    let mut error = use_signal(String::new);
    // Emulators with an artwork folder: (id, name, artwork).
    let presets: Vec<(u64, String, serde_json::Value)> = emulators
        .read()
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .map(|list| list.iter().filter_map(|e| e.get("artwork").filter(|a| !s(a, "dir").trim().is_empty()).map(|a| (api::u(e, "id"), s(e, "name"), a.clone()))).collect())
        .unwrap_or_default();
    let mut apply = {
        let mut dir = dir;
        move |a: &serde_json::Value| {
            dir.set(s(a, "dir"));
            let l = s(a, "layout");
            let f: Vec<String> = api::arr(a, "formats").iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
            formats.set(if f.is_empty() { layout_formats(&l) } else { f });
            width.set(a.get("max_width").and_then(|v| v.as_u64()).map(|w| w.to_string()).unwrap_or_default());
            layout.set(l);
        }
    };
    // Starts from the first emulator's folder, when one has an artwork setting.
    use_effect(move || {
        if from.peek().eq(&0) && dir.peek().is_empty() {
            if let Some(Ok(list)) = &*emulators.read() {
                if let Some((id, a)) = list.iter().find_map(|e| e.get("artwork").filter(|a| !s(a, "dir").trim().is_empty()).map(|a| (api::u(e, "id"), a.clone()))) {
                    from.set(id);
                    apply(&a);
                }
            }
        }
    });
    let heading = state::trf("Export artwork for {what}", &[("what", &what)]);
    let start = {
        let selection = selection.clone();
        move |_| {
            let target = json!({ "dir": dir().trim(), "layout": layout(), "formats": formats(), "max_width": width().trim().parse::<u32>().ok().filter(|w| *w > 0) });
            let body = json!({ "job_type": "artwork_export", "target": target, "emulator_id": if from() == 0 { serde_json::Value::Null } else { json!(from()) }, "selection": selection });
            spawn(async move {
                match api::post("/api/v1/jobs", body).await {
                    Ok(r) => job.set(r.get("id").and_then(|v| v.as_u64())),
                    Err(e) => error.set(e.message),
                }
            });
        }
    };
    let busy = job().is_some();
    rsx! {
        h2 { "{heading}" }
        div { class: "kv", style: "grid-template-columns:max-content 1fr; gap:6px 10px",
            if !presets.is_empty() {
                div { {state::tr("settings from")} }
                div {
                    select { disabled: busy,
                        onchange: {
                            let presets = presets.clone();
                            move |e: Event<FormData>| {
                                let id: u64 = e.value().parse().unwrap_or(0);
                                from.set(id);
                                if let Some((_, _, a)) = presets.iter().find(|(i, _, _)| *i == id) { apply(a); }
                            }
                        },
                        option { value: "0", selected: from() == 0, {state::tr("none (this folder only)")} }
                        for (id, name, _) in presets.clone() { option { value: "{id}", selected: from() == id, "{name}" } }
                    }
                }
            }
            div { {state::tr("into")} }
            div { super::config::PathInput { value: dir, style: "width:100%", placeholder: state::tr("the folder to export into") } }
            div { {state::tr("layout")} }
            div {
                select { disabled: busy,
                    onchange: move |e: Event<FormData>| { let v = e.value(); formats.set(layout_formats(&v)); width.set(if v == "retroarch" { "512".to_string() } else { String::new() }); layout.set(v); },
                    option { value: "retroarch", selected: layout() == "retroarch", {state::tr("RetroArch thumbnails (Named_Boxarts, Named_Titles, Named_Snaps, Named_Logos)")} }
                    option { value: "cover_folder", selected: layout() == "cover_folder", {state::tr("covers folder, by serial (PCSX2, DuckStation)")} }
                    option { value: "cover_folder_name", selected: layout() == "cover_folder_name", {state::tr("covers folder, by file name (PCSX2, DuckStation)")} }
                }
            }
            div { {state::tr("formats")} }
            div {
                for f in ["png", "jpg", "webp"] {
                    label { style: "margin-right:10px",
                        input { r#type: "checkbox", disabled: busy, checked: formats().iter().any(|x| x == f),
                            onchange: move |e| { let mut list = formats.write(); list.retain(|x| x != f); if e.checked() { list.push(f.to_string()); } } }
                        " {f}"
                    }
                }
                div { class: "muted", {state::tr("A picture already in a ticked format is copied as it is; anything else is converted to the first ticked one this build can write (PNG, JPG).")} }
            }
            div { {state::tr("max width")} }
            div {
                input { size: "6", disabled: busy, value: "{width}", placeholder: state::tr("none"), oninput: move |e| width.set(e.value()) }
                span { class: "muted", {state::tr(" px; wider pictures are scaled down, never up")} }
            }
            div {}
            div { class: "muted", {state::tr("Named after this machine's file of each game. Files already there that romexplorer did not write are left alone.")} }
        }
        if !error().is_empty() { div { class: "card bad", "{error}" } }
        if let Some(id) = job() {
            div { class: "card", JobWatch { key: "{id}", job_id: id, on_done: move |_| finished.set(true) } }
        }
        div { class: "fsfoot",
            span { class: "grow" }
            if let Some(id) = job() {
                RunningJobButtons { job_id: id, finished: finished() }
            } else {
                button { r#type: "button", class: "btn", onclick: move |_| ui::close_modal(), {state::tr("cancel")} }
                button { r#type: "button", class: "btn primary", disabled: dir().trim().is_empty() || formats().is_empty(), onclick: start, {state::tr("export")} }
            }
        }
    }
}

/// The selection-bar button, for a grid of titles, releases or sets.
pub fn art_action(kind: &'static str) -> crate::components::SelAction {
    crate::components::SelAction::new(&state::tr("download art…"), move |ids: Vec<u64>| {
        let n = ids.len();
        let selection = match kind {
            "titles" => CopySelection { title_row_ids: ids, ..Default::default() },
            "sets" => CopySelection { set_ids: ids, ..Default::default() },
            _ => CopySelection { release_ids: ids, ..Default::default() },
        };
        async move {
            art_dialog(selection, state::trf("{n} selected {kind}", &[("n", &n.to_string()), ("kind", &state::kind_word(kind, false))]));
            false
        }
    })
}

/// The selection-bar button for "Export artwork…", next to [`art_action`].
pub fn export_art_action(kind: &'static str) -> crate::components::SelAction {
    crate::components::SelAction::new(&state::tr("export art…"), move |ids: Vec<u64>| {
        let n = ids.len();
        let selection = selection_of(kind, ids);
        async move {
            export_dialog(selection, state::trf("{n} selected {kind}", &[("n", &n.to_string()), ("kind", &state::kind_word(kind, false))]));
            false
        }
    })
}

#[component]
fn ArtDialog(selection: CopySelection, what: String) -> Element {
    let providers = art_providers();
    let first = LAST().map(|(p, _)| p).filter(|p| providers.iter().any(|x| &x.name == p)).or_else(|| providers.first().map(|p| p.name.clone())).unwrap_or_default();
    let mut provider = use_signal(|| first);
    let mut ticked = use_signal(|| LAST().map(|(_, t)| t).unwrap_or_else(|| DEFAULT_TICKED.iter().map(|s| s.to_string()).collect()));
    let mut job = use_signal(|| None::<u64>);
    let mut finished = use_signal(|| false);
    let mut error = use_signal(String::new);

    let current = providers.iter().find(|p| p.name == provider()).cloned();
    // What a manual-only provider's allowance has left, as it last said.
    let quotas = use_resource(|| async { api::get("/api/v1/providers").await.map(|v| api::items(&v)) });
    let allowance: Option<String> = current.as_ref().filter(|p| p.manual_only).and_then(|p| {
        let list = quotas.read().as_ref().and_then(|r| r.as_ref().ok()).cloned().unwrap_or_default();
        let row = list.into_iter().find(|r| s(r, "name") == p.name)?;
        let left = row.get("quota_remaining").and_then(|v| v.as_i64())?;
        Some(state::trf("{n} API calls left this month (as of {at}); one call covers about 20 games.", &[("n", &left.to_string()), ("at", &crate::html::fmt_date(&s(&row, "quota_checked_at")))]))
    });
    let slots = current.as_ref().map(|p| p.slots.clone()).unwrap_or_default();
    // Only what this provider has: a type ticked for another stays remembered, but isn't sent.
    let chosen: Vec<String> = slots.iter().filter(|s| ticked().contains(*s)).cloned().collect();
    let heading = state::trf("Download art for {what}", &[("what", &what)]);

    let start = {
        let selection = selection.clone();
        let chosen = chosen.clone();
        move |_| {
            let body = json!({ "job_type": "media_backfill", "selection": selection, "providers": [provider()], "types": chosen });
            *LAST.write() = Some((provider(), ticked()));
            spawn(async move {
                match api::post("/api/v1/jobs", body).await {
                    Ok(r) => job.set(r.get("id").and_then(|v| v.as_u64())),
                    Err(e) => error.set(e.message),
                }
            });
        }
    };

    rsx! {
        h2 { "{heading}" }
        div { class: "kv", style: "grid-template-columns:max-content 1fr; gap:6px 10px",
            div { {state::tr("from")} }
            div {
                select { disabled: job().is_some(), onchange: move |e| provider.set(e.value()),
                    for p in providers.clone() {
                        option { value: "{p.name}", selected: p.name == provider(), "{p.name}" }
                    }
                }
                if current.as_ref().is_some_and(|p| p.manual_only) {
                    div { class: "muted", {state::tr("This provider's allowance is small: its art is only downloaded from here, for what you pick.")} }
                    div { class: "muted", {state::tr("What you pick is first linked from TheGamesDB's free game list (its full sync, run once; no allowance used); only the pictures use calls.")} }
                    if let Some(line) = allowance.clone() { div { class: "muted", "{line}" } }
                }
            }
            div { {state::tr("art")} }
            div {
                for slot in slots.clone() {
                    label { style: "margin-right:12px",
                        input {
                            r#type: "checkbox",
                            disabled: job().is_some(),
                            checked: ticked().contains(&slot),
                            onchange: {
                                let slot = slot.clone();
                                move |e: Event<FormData>| {
                                    let mut t = ticked.write();
                                    t.retain(|x| x != &slot);
                                    if e.checked() {
                                        t.push(slot.clone());
                                    }
                                }
                            },
                        }
                        " "
                        {slot_label(&slot)}
                    }
                }
                div { class: "muted", {state::tr("Art already downloaded from this provider, and your own picks, are left as they are.")} }
            }
        }
        if !error().is_empty() { div { class: "card bad", "{error}" } }
        if let Some(id) = job() {
            div { class: "card", JobWatch { key: "{id}", job_id: id, on_done: move |_| finished.set(true) } }
            if !finished() {
                div { class: "muted", style: "margin-top:6px", {state::tr("The download keeps running in the background after this window closes; follow it under Jobs.")} }
            }
        }
        div { class: "fsfoot",
            span { class: "grow" }
            if let Some(id) = job() {
                RunningJobButtons { job_id: id, finished: finished() }
            } else {
                button { r#type: "button", class: "btn", onclick: move |_| ui::close_modal(), {state::tr("cancel")} }
                button { r#type: "button", class: "btn primary", disabled: chosen.is_empty() || current.is_none(), onclick: start, {state::tr("download")} }
            }
        }
    }
}
