//! Rebuilding the index file a dump is short of: the button, its preview, and the job card.
//!
//! One dialog serves both screens. It always previews before it offers to do anything, because
//! two of the three things it can do are worth seeing first: which convention a rebuilt cue
//! will use, and -- when it is offered at all -- exactly which of the user's ROM files would be
//! renamed and to what.

use dioxus::prelude::*;
use romexplorer_types::sheets::{FixResult, SheetCandidate, SheetCreateRequest, SheetPlan, SheetTarget, SheetVerdictView};
use serde_json::json;

use super::backup::JobWatch;
use crate::html::badge;
use crate::state;
use crate::ui::{self, Html};
use super::common::bump;
use crate::api;

/// The button that goes on a rom or game screen.
///
/// `game_id` or `rom_id`: a rom resolves up to its game server-side, since a missing cue is a
/// property of the dump rather than of one rom.
#[component]
pub fn SheetButton(game_id: Option<u64>, rom_id: Option<u64>, target: SheetTarget) -> Element {
    let mut plan = use_signal(|| None::<SheetPlan>);
    let request = move || SheetCreateRequest {
        game_ids: game_id.into_iter().collect(),
        rom_ids: rom_id.into_iter().collect(),
        target,
        ..Default::default()
    };
    // The button's own state comes from the same preview the dialog shows, so "greyed out" and
    // "here is why" can never disagree.
    let probe = use_resource(move || async move { api::post_as::<SheetCreateRequest, SheetPlan>("/api/v1/sheets/preview", &request()).await.ok() });
    let probed = probe.read().clone().flatten();
    let Some(candidate) = probed.as_ref().and_then(|p| p.candidates.first()).cloned() else {
        return rsx! {};
    };
    let label = match target {
        SheetTarget::M3u => state::tr("create playlist"),
        SheetTarget::Sheet => state::tr("create sheet"),
    };
    let blocked = candidate.blocked.clone();
    // Precomputed: a formatted attribute takes an identifier or a simple expression, not a
    // closure.
    let hint = blocked.clone().unwrap_or_else(|| state::trf("build the {ext} from the files that are here", &[("ext", &candidate.extension)]));
    rsx! {
        button {
            class: "btn small",
            r#type: "button",
            disabled: blocked.is_some(),
            // Disabled rather than hidden, with the reason on hover: "why can't I do this"
            // is the question, and an absent button does not answer it.
            title: "{hint}",
            onclick: move |_| {
                let body = request();
                spawn(async move {
                    if let Ok(p) = api::post_as::<SheetCreateRequest, SheetPlan>("/api/v1/sheets/preview", &body).await {
                        plan.set(Some(p.clone()));
                        ui::open_modal(move || rsx! { SheetDialog { plan: p.clone(), request: body.clone() } });
                    }
                });
            },
            "✚ {label}"
        }
        if let Some(reason) = blocked.as_ref() {
            " " span { class: "muted", style: "font-size:11px", "{reason}" }
        }
    }
}

/// The preview, the rename table, and the go button.
#[component]
fn SheetDialog(plan: SheetPlan, request: SheetCreateRequest) -> Element {
    let mut standardize = use_signal(|| false);
    let mut pattern = use_signal(String::new);
    let mut job = use_signal(|| None::<u64>);
    let mut error = use_signal(String::new);
    // Re-previewed whenever the naming choice changes, so the table always shows what would
    // actually happen rather than what would have happened before the pattern was typed.
    let mut live = use_signal(|| plan.clone());

    // A free function rather than a closure: two `move` closures below both need it, and a
    // closure capturing the request would be moved into the first of them.
    fn reprice(mut live: Signal<SheetPlan>, base: SheetCreateRequest, standardize: bool, pattern: String) {
        let body = SheetCreateRequest { standardize, pattern: Some(pattern).filter(|p| !p.trim().is_empty()), ..base };
        spawn(async move {
            if let Ok(p) = api::post_as::<SheetCreateRequest, SheetPlan>("/api/v1/sheets/preview", &body).await {
                live.set(p);
            }
        });
    }

    let current = live();
    let one = current.candidates.first().cloned();
    let renames: Vec<_> = one.as_ref().map(|c| c.rename.clone()).unwrap_or_default();
    let will_rename: Vec<_> = renames.iter().filter(|r| r.problem.is_none() && r.from != r.to).cloned().collect();
    let refused: Vec<_> = renames.iter().filter(|r| r.problem.is_some()).cloned().collect();
    let base2 = request.clone();
    let refused_text = state::trf("{n} file(s) will be left alone:", &[("n", &refused.len().to_string())]);

    rsx! {
        h2 { {state::tr("Create the missing index file")} }
        if let Some(c) = one.clone() {
            div { class: "card",
                div { class: "kv", style: "grid-template-columns:max-content 1fr; gap:6px 10px",
                    div { {state::tr("dump")} } div { "{c.game_name}" }
                    div { {state::tr("writes")} } div { class: "mono", "{c.destination}" }
                    div { {state::tr("from")} }
                    div {
                        for m in c.members.iter() { div { class: "mono", "{m}" } }
                    }
                    div { {state::tr("checkable")} }
                    div {
                        if c.checkable {
                            Html { html: badge(&state::tr("the DAT records this sheet"), "ok") }
                            div { class: "muted", {state::tr("Every convention will be tried and the one that reproduces the DAT's own sheet kept — so this rebuild can be confirmed, not just attempted.")} }
                        } else {
                            Html { html: badge(&state::tr("nothing to check against"), "warn") }
                            div { class: "muted", {state::tr("No DAT records this sheet, so the most common convention is used and the result is marked as unconfirmed rather than claimed to be right.")} }
                        }
                    }
                    div { {state::tr("style")} } div { "{current.style}" }
                }
            }
            if let Some(reason) = c.blocked.clone() {
                div { class: "card bad", "{reason}" }
            }
        }

        div { class: "card",
            h3 { {state::tr("File names")} }
            if renames.is_empty() {
                p { class: "muted", {state::tr("These files already agree with each other, so nothing needs renaming.")} }
            }
            label {
                input {
                    r#type: "checkbox",
                    checked: standardize(),
                    onchange: {
                        let base = request.clone();
                        move |e: Event<FormData>| { standardize.set(e.checked()); reprice(live, base.clone(), e.checked(), pattern()); }
                    },
                }
                " "
                {state::tr("rename these files to match the naming convention")}
            }
            div { class: "muted",
                {state::tr("This moves your actual ROM files. Nothing else in romexplorer does that, so it is off unless you turn it on, and the table below is exactly what would happen.")}
            }
            if standardize() {
                div { style: "margin-top:6px",
                    label { {state::tr("pattern")} " "
                        input {
                            style: "width:340px",
                            placeholder: state::tr("leave empty for the DAT's own names"),
                            value: "{pattern}",
                            oninput: {
                                let base = request.clone();
                                move |e: Event<FormData>| { pattern.set(e.value()); reprice(live, base.clone(), standardize(), e.value()); }
                            },
                        }
                    }
                    div { class: "muted", {state::tr("{name}, {track}, {disc}, {ext}. Empty uses the names the DAT gives, which is what lets the renamed dump be matched afterwards.")} }
                }
            }
            if !will_rename.is_empty() {
                table { class: "grid",
                    thead { tr { th { {state::tr("from")} } th { {state::tr("to")} } } }
                    tbody {
                        for r in will_rename.iter() {
                            tr { td { class: "mono", "{r.from}" } td { class: "mono", "{r.to}" } }
                        }
                    }
                }
            }
            if !refused.is_empty() {
                div { class: "bad", style: "margin-top:6px", "{refused_text}" }
                for r in refused.iter() {
                    div { class: "muted mono", "{r.from}: {r.problem.clone().unwrap_or_default()}" }
                }
            }
        }

        if !error().is_empty() { div { class: "card bad", "{error}" } }
        if let Some(id) = job() {
            div { class: "card", JobWatch { key: "{id}", job_id: id } }
        }

        div { class: "fsfoot",
            span { class: "grow" }
            button { r#type: "button", class: "btn", onclick: move |_| ui::close_modal(), if job().is_some() { {state::tr("close")} } else { {state::tr("cancel")} } }
            if job().is_none() {
                button {
                    r#type: "button",
                    class: "btn primary",
                    disabled: one.as_ref().is_none_or(|c| !c.eligible()),
                    onclick: move |_| {
                        let body = SheetCreateRequest {
                            standardize: standardize(),
                            pattern: Some(pattern()).filter(|p| !p.trim().is_empty()),
                            // The confirmation the server insists on: this dialog has shown
                            // the old-to-new table, so it can say so.
                            rename_confirmed: true,
                            ..base2.clone()
                        };
                        spawn(async move {
                            match api::post_as::<SheetCreateRequest, romexplorer_types::jobs::JobEnqueued>("/api/v1/sheets", &body).await {
                                Ok(j) => job.set(Some(j.job_id)),
                                Err(e) => error.set(e.message),
                            }
                        });
                    },
                    if standardize() { {state::tr("rename and create")} } else { {state::tr("create")} }
                }
            }
        }
    }
}

/// Jobs › the three sheet jobs: build every one that is missing, and check the ones that exist.
#[component]
pub fn SheetCard(reload: Signal<u64>) -> Element {
    let dirs = use_resource(|| async { api::get("/api/v1/directories").await.map(|v| api::items(&v)).unwrap_or_default() });
    let mut directory = use_signal(String::new);
    let mut limit = use_signal(|| "500".to_string());
    let mut fix = use_signal(|| false);
    let list = dirs.read().clone().unwrap_or_default();
    rsx! {
        div { class: "card",
            h2 { {state::tr("Cue sheet and playlist check")} }
            ui::Explain { p { {state::tr("A cue/bin set is the one dump shape whose index is a separate file, so losing it loses the dump: the tracks are still there, but nothing says what they are or what order they go in.")}
                " "
                b { {state::tr("Create")} }
                " "
                {state::tr("rebuilds that index for every dump whose tracks are all present and whose sheet is not — track lengths are arithmetic, types are a two-byte read, and the gaps, which cannot be recovered from the bytes at all, are resolved by trying each convention and keeping whichever reproduces what the DAT records. A dump missing anything other than its sheet is skipped, as is one that already has it.")} } p { b { {state::tr("Check")} }
                " "
                {state::tr("re-reads the sheets already here. A sheet that parses, names files that are present and divides into whole sectors is sound; one that also matches the DAT byte for byte is verified. The two are kept apart on purpose — a sheet can play perfectly and not be Redump's, and that is never called broken. Least recently checked first, so short runs repeated over time sweep everything.")} } }
            form { class: "inline", onsubmit: move |e| e.prevent_default(),
                label { {state::tr("directory")}
                    select { onchange: move |e| directory.set(e.value()),
                        option { value: "", {state::tr("every directory")} }
                        for d in list.iter() { option { value: "{api::u(d, \"id\")}", "{api::s(d, \"path\")}" } }
                    }
                }
                label { {state::tr("at most")} input { r#type: "number", min: "1", style: "width:90px", value: "{limit}", oninput: move |e| limit.set(e.value()) } }
                label { class: "muted", title: state::tr("off: only check. On: a sheet or playlist that works but doesn't match the DAT is rewritten to match it, when its files are named the way the DAT names them"),
                    input { r#type: "checkbox", checked: fix(), onchange: move |e| fix.set(e.checked()) }
                    " {state::tr(\"also fix\")}"
                }
                button { class: "btn", r#type: "button",
                    onclick: move |_| {
                        let body = json!({ "job_type": "sheet_verify", "directory_id": directory().parse::<u64>().ok(), "limit": limit().parse::<u64>().ok(), "fix": fix() });
                        spawn(async move { if super::jobs::enqueue(body).await { bump(reload) } });
                    },
                    {state::tr("check sheets")}
                }
                crate::ui::QueueButton {}
                button { class: "btn", r#type: "button",
                    onclick: move |_| {
                        let body = json!({ "job_type": "m3u_verify", "directory_id": directory().parse::<u64>().ok(), "limit": limit().parse::<u64>().ok(), "fix": fix() });
                        spawn(async move { if super::jobs::enqueue(body).await { bump(reload) } });
                    },
                    {state::tr("check playlists")}
                }
                crate::ui::QueueButton {}
            }
            div { class: "toolbar", style: "margin-top:8px",
                button { class: "btn", r#type: "button",
                    onclick: move |_| {
                        spawn(async move {
                            // Never renames from here: a job over a whole collection is the
                            // last place to move somebody's files without showing them first.
                            let body = json!({ "job_type": "sheet_create", "target": "sheet", "all": true });
                            if super::jobs::enqueue(body).await { bump(reload) }
                        });
                    },
                    {state::tr("create every missing sheet")}
                }
                crate::ui::QueueButton {}
                button { class: "btn", r#type: "button",
                    onclick: move |_| {
                        spawn(async move {
                            let body = json!({ "job_type": "sheet_create", "target": "m3u", "all": true });
                            if super::jobs::enqueue(body).await { bump(reload) }
                        });
                    },
                    {state::tr("create every missing playlist")}
                }
                crate::ui::QueueButton {}
                span { class: "muted", {state::tr("Neither renames anything — that is only ever offered on one dump at a time, with the table in front of you.")} }
            }
        }
    }
}

/// Whether a "fix hash" button is worth showing for a verdict.
///
/// Only where it could possibly help: structurally sound, and with a DAT hash to aim at. A
/// verified sheet has nothing to fix, an unverifiable one has nothing to aim at, and a broken
/// one has a different problem that a rewrite would only paper over.
pub fn offerable(view: &SheetVerdictView) -> bool {
    view.verdict == "structure_ok" && view.hash_known
}

/// Runs a hash repair and shows what it concluded.
///
/// Always a dialog, never a silent success or a toast, because the interesting outcomes are
/// the refusals -- and each of them has to explain both why it stopped and that the disc is
/// fine regardless. "Doesn't match the database" reads as a fault; someone who believes their
/// disc is faulty will go and re-dump it.
pub async fn fix_dialog(file_id: u64, verdict: Signal<Option<SheetVerdictView>>) {
    let result = api::post_as::<(), FixResult>(&format!("/api/v1/sheets/files/{file_id}/fix"), &()).await;
    match result {
        Ok(r) => {
            // A successful fix makes the line's own badge stale; refresh it in place rather
            // than leaving the old verdict under a dialog that says it was repaired.
            if r.outcome.changed() {
                let mut sig = verdict;
                if let Ok(v) = api::post_as::<(), SheetVerdictView>(&format!("/api/v1/sheets/files/{file_id}/verify"), &()).await {
                    sig.set(Some(v));
                }
            }
            ui::open_modal(move || {
                let r = r.clone();
                rsx! {
                    h2 { {state::tr("Fix the hash")} }
                    div { class: "card",
                        Html { html: badge(&state::tr(r.outcome.label()), r.outcome.tone()) }
                        p { "{r.message}" }
                        if !r.problems.is_empty() {
                            ul {
                                for p in r.problems.iter() { li { class: "mono", "{p}" } }
                            }
                        }
                    }
                    div { class: "fsfoot",
                        span { class: "grow" }
                        button { r#type: "button", class: "btn", onclick: move |_| ui::close_modal(), {state::tr("close")} }
                    }
                }
            });
        }
        Err(e) => ui::open_modal(move || {
            let message = e.message.clone();
            rsx! {
                h2 { {state::tr("Fix the hash")} }
                div { class: "card bad", "{message}" }
                div { class: "fsfoot", span { class: "grow" } button { r#type: "button", class: "btn", onclick: move |_| ui::close_modal(), {state::tr("close")} } }
            }
        }),
    }
}
