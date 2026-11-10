//! "Copy to…": the dialog, and the menu entries that open it.
//!
//! The dialog is one modal over four things the user actually decides -- where, which mode,
//! how the files are named on disk, and what they are converted to. Everything else it shows
//! is the server's own plan (`POST /api/v1/copy/preview`), so the numbers on screen are the
//! numbers the job will produce rather than a second estimate.

use dioxus::prelude::*;
use romexplorer_types::copy::{ConflictChoice, CopyDefaults, CopyFilesRequest, CopyMode, CopyPlan, CopySelection, FolderScheme, OutputFormat, OutputSettings, SourcePreference, codec_levels, CHD_CD_CODECS, CHD_DVD_CODECS, CHD_MAX_CODECS, RVZ_BLOCK_SIZES, RVZ_CODECS, SEVEN_ZIP_CODECS, WIA_CODECS, ZIP_CODECS};
use romexplorer_types::jobs::JobEnqueued;

use super::backup::JobWatch;
use crate::api;
use crate::components::SelAction;
use crate::html::badge;
use crate::state;
use crate::ui::{self, Html, MenuItem};

fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < UNITS.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// Opens the dialog for whatever the user picked.
pub fn copy_dialog(selection: CopySelection, what: String) {
    ui::open_modal(move || rsx! { CopyDialog { selection: selection.clone(), what: what.clone() } });
}

/// A "Copy to…" entry for a context menu. `what` is the phrase the dialog's heading uses.
pub fn copy_menu_item(label: &str, selection: CopySelection, what: String) -> MenuItem {
    MenuItem::new(label, move || copy_dialog(selection.clone(), what.clone()))
}

/// The selection-bar button, for a grid whose rows are `local_*` ids of one kind.
pub fn copy_action(kind: &'static str) -> SelAction {
    SelAction::new(&state::tr("copy to…"), move |ids: Vec<u64>| {
        let n = ids.len();
        let refs: Vec<romexplorer_types::copy::OwnedRef> = ids.into_iter().map(romexplorer_types::copy::OwnedRef::local).collect();
        let selection = match kind {
            "releases" => CopySelection { release_ids: refs.iter().map(|r| r.id).collect(), ..Default::default() },
            "titles" => CopySelection { title_row_ids: refs.iter().map(|r| r.id).collect(), ..Default::default() },
            "sets" => CopySelection { set_ids: refs.iter().map(|r| r.id).collect(), ..Default::default() },
            "roms" => CopySelection { rom_ids: refs, ..Default::default() },
            "files" => CopySelection { file_ids: refs, ..Default::default() },
            _ => CopySelection { game_ids: refs, ..Default::default() },
        };
        async move {
            copy_dialog(selection, state::trf("{n} selected {kind}", &[("n", &n.to_string()), ("kind", &state::kind_word(kind, false))]));
            // The selection stays: a copy doesn't change the list it was started from.
            false
        }
    })
}

#[component]
fn CopyDialog(selection: CopySelection, what: String) -> Element {
    let defaults = use_resource(|| async { api::get_as::<CopyDefaults>("/api/v1/copy/defaults").await });
    let mut destination = use_signal(String::new);
    let mut mode = use_signal(|| CopyMode::ToCollection);
    let mut scheme = use_signal(|| None::<FolderScheme>);
    let mut all_releases = use_signal(|| false);
    let mut prefer_main = use_signal(|| false);
    let mut output = use_signal(|| None::<OutputSettings>);
    let mut verify = use_signal(|| None::<bool>);
    let mut show_output = use_signal(|| false);
    let mut plan = use_signal(|| None::<CopyPlan>);
    let mut planning = use_signal(|| false);
    let mut starting = use_signal(|| false);
    // A preview is of one destination: another one takes it away until it is asked for again.
    use_effect(move || {
        let _ = destination();
        if plan.peek().is_some() {
            plan.set(None);
        }
    });
    let mut error = use_signal(String::new);
    let mut job = use_signal(|| None::<u64>);
    // The started job has ended (done, failed or cancelled): only "close" is left to offer.
    let mut finished = use_signal(|| false);
    // What to do about each destination that already exists and holds something else. Keyed
    // by destination path, which is what the preview names them by.
    let mut conflict_choices = use_signal(std::collections::BTreeMap::<String, ConflictChoice>::new);
    // Emulators with an artwork folder: art exported for what the copy writes, once it is done.
    let emulators = use_resource(|| async { api::get("/api/v1/emulators").await.map(|v| api::items(&v)) });
    let art_targets: Vec<(u64, String, bool)> = emulators
        .read()
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .map(|list| {
            list.iter()
                .filter(|e| e.get("artwork").is_some_and(|a| !crate::api::s(a, "dir").trim().is_empty()))
                .map(|e| (crate::api::u(e, "id"), crate::api::s(e, "name"), e.get("artwork").is_some_and(|a| crate::api::b(a, "after_copy"))))
                .collect()
        })
        .unwrap_or_default();
    let mut export_for = use_signal(|| None::<Vec<u64>>);
    // Ticked to start with: the emulators set to export after a copy.
    use_effect(move || {
        if export_for.peek().is_none() {
            if let Some(Ok(list)) = &*emulators.read() {
                let on: Vec<u64> = list.iter().filter(|e| e.get("artwork").is_some_and(|a| crate::api::b(a, "after_copy") && !crate::api::s(a, "dir").trim().is_empty())).map(|e| crate::api::u(e, "id")).collect();
                export_for.set(Some(on));
            }
        }
    });

    let d = match &*defaults.read() {
        None => return rsx! { h2 { {state::tr("Copy to…")} } div { class: "muted", {state::tr("loading…")} } },
        Some(Err(e)) => return rsx! { h2 { {state::tr("Copy to…")} } div { class: "card bad", "{e.message}" } },
        Some(Ok(d)) => d.clone(),
    };
    use_effect({
        let d = d.clone();
        move || {
            if scheme().is_none() {
                scheme.set(Some(d.scheme));
            }
            if output().is_none() {
                output.set(Some(d.output.clone()));
            }
            if d.decide_on_copy {
                show_output.set(true);
            }
            if verify().is_none() {
                verify.set(Some(d.verify_default));
            }
        }
    });

    // Every mode this machine can run; never an empty list, since `to_collection` and
    // `to_external` always apply.
    let modes: Vec<CopyMode> = if d.modes.is_empty() { vec![CopyMode::ToCollection, CopyMode::ToExternal] } else { d.modes.clone() };
    use_effect({
        let modes = modes.clone();
        move || {
            if !modes.contains(&mode()) {
                mode.set(modes[0]);
            }
        }
    });

    let request = {
        let selection = selection.clone();
        move || CopyFilesRequest {
            selection: selection.clone(),
            mode: mode(),
            destination: destination().trim().to_string(),
            folder_scheme: scheme(),
            output: output(),
            source_preference: if prefer_main() { SourcePreference::MainFirst } else { SourcePreference::LocalFirst },
            on_conflict: Default::default(),
            titles_all_releases: all_releases(),
            verify: verify(),
            conflict_choices: conflict_choices().into_iter().collect(),
            upload_source_id: None,
            export_artwork: export_for().unwrap_or_default(),
        }
    };

    let preview = {
        let request = request.clone();
        move |_| {
            let body = request();
            if body.destination.is_empty() {
                error.set(state::tr("pick a destination folder first"));
                return;
            }
            planning.set(true);
            error.set(String::new());
            spawn(async move {
                match api::post_as::<CopyFilesRequest, CopyPlan>("/api/v1/copy/preview", &body).await {
                    Ok(p) => {
                        plan.set(Some(p));
                    }
                    Err(e) => {
                        plan.set(None);
                        error.set(e.message);
                    }
                }
                planning.set(false);
            });
        }
    };

    let start = {
        let request = request.clone();
        move |_| {
            // One copy per press: a second click while the first is being queued does nothing.
            if starting() {
                return;
            }
            starting.set(true);
            let body = request();
            let request = request.clone();
            spawn(async move {
                // Files already in the way are asked about first, in a popup, whether or not
                // a preview was looked at: skip (the default), rename or overwrite, for each
                // file or for all of them.
                if !body.mode.never_overwrites() {
                    match api::post_as::<CopyFilesRequest, CopyPlan>("/api/v1/copy/preview", &body).await {
                        Ok(p) if !p.conflicts.is_empty() => {
                            let conflicts = p.conflicts.clone();
                            plan.set(Some(p));
                            starting.set(false);
                            let request = request.clone();
                            ui::open_over_modal(move || {
                                let request = request.clone();
                                rsx! {
                                    ConflictPopup {
                                        conflicts: conflicts.clone(),
                                        choices: conflict_choices,
                                        on_go: move |_| {
                                            ui::close_top_modal();
                                            let body = request();
                                            starting.set(true);
                                            spawn(async move {
                                                match api::post_as::<CopyFilesRequest, JobEnqueued>("/api/v1/copy", &body).await {
                                                    Ok(r) => job.set(Some(r.job_id)),
                                                    Err(e) => error.set(e.message),
                                                }
                                                starting.set(false);
                                            });
                                        },
                                    }
                                }
                            });
                            return;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            error.set(e.message);
                            starting.set(false);
                            return;
                        }
                    }
                }
                // `spawn_forever`: the request has to outlive the dialog it was started from.
                match api::post_as::<CopyFilesRequest, JobEnqueued>("/api/v1/copy", &body).await {
                    Ok(r) => job.set(Some(r.job_id)),
                    Err(e) => error.set(e.message),
                }
                starting.set(false);
            });
        }
    };

    let p = plan();
    let unavailable = d.unavailable_formats.clone();
    let recent = d.recent_destinations.clone();
    let heading = state::trf("Copy {what}", &[("what", &what)]);
    let hash_note = state::trf("Off, a copy still knows exactly what it copied: the file's identity comes from the row it came from, not from re-hashing it. On, each file is compared against whichever hash the catalogue already holds for those bytes, and {hash} is recorded off the same read once it matches.", &[("hash", &d.populate_hash)]);

    rsx! {
        h2 { "{heading}" }
        div { class: "kv", style: "grid-template-columns:max-content 1fr; gap:6px 10px",
            div { {state::tr("to")} }
            div {
                super::config::PathInput { value: destination, style: "width:100%", placeholder: state::tr("the folder to copy into") }
                if !recent.is_empty() {
                    div { style: "margin-top:4px",
                        span { class: "muted", {state::tr("recent: ")} }
                        for r in recent {
                            button { class: "btn small", r#type: "button", onclick: { let r2 = r.clone(); move |_| destination.set(r2.clone()) }, "{r}" }
                            " "
                        }
                    }
                }
            }
            div { {state::tr("mode")} }
            div {
                for m in modes.clone() {
                    div { style: "margin-bottom:4px",
                        label {
                            input {
                                r#type: "radio",
                                name: "copymode",
                                checked: mode() == m,
                                onchange: move |_| { mode.set(m); plan.set(None); conflict_choices.write().clear(); },
                            }
                            " "
                            {state::tr(m.label())}
                        }
                        div { class: "muted", style: "margin-left:22px", {state::tr(m.hint())} }
                    }
                }
            }
            div { {state::tr("folders")} }
            div {
                select { onchange: move |e| { scheme.set(FolderScheme::from_name(&e.value())); plan.set(None); },
                    for s in FolderScheme::ALL {
                        option { value: "{s.name()}", selected: scheme() == Some(s), {state::tr(s.label())} }
                    }
                }
                " "
                span { class: "muted", {state::tr("change the per-platform names under Config › Copy")} }
            }
            if !selection.title_ids.is_empty() || !selection.title_row_ids.is_empty() || !selection.title_rows.is_empty() {
                div { {state::tr("titles")} }
                div {
                    label {
                        input { r#type: "checkbox", checked: all_releases(), onchange: move |e| { all_releases.set(e.checked()); plan.set(None); } }
                        " "
                        {state::tr("copy every release of a selected title, not just its top-ranked one")}
                    }
                }
            }
            div { {state::tr("check")} }
            div {
                label {
                    input {
                        r#type: "checkbox",
                        checked: verify() == Some(true) || mode().mandatory_verify(),
                        disabled: mode().mandatory_verify(),
                        onchange: move |e| { verify.set(Some(e.checked())); plan.set(None); },
                    }
                    " "
                    {state::tr("read each file back and compare it — doubles the reads, which over a mounted share is the whole cost")}
                }
                if mode().mandatory_verify() {
                    div { class: "muted", {state::tr("Always on for this mode: it rearranges the collection the rest of the app trusts, so a bad write there is a lost ROM.")} }
                }
                if mode() == CopyMode::Upload {
                    div { class: "muted", {state::tr("The server hashes what arrived itself once the copy finishes — a hash taken here would only say the bytes left correctly.")} }
                }
                div { class: "muted",
                    "{hash_note}"
                }
            }
            if !art_targets.is_empty() {
                div { {state::tr("artwork")} }
                div {
                    span { {state::tr("export art afterwards for: ")} }
                    for (id, name, _) in art_targets.clone() {
                        label { style: "margin-right:10px",
                            input { r#type: "checkbox", checked: export_for().unwrap_or_default().contains(&id),
                                onchange: move |e| { let mut list = export_for().unwrap_or_default(); list.retain(|x| *x != id); if e.checked() { list.push(id); } export_for.set(Some(list)); } }
                            " {name}"
                        }
                    }
                    div { class: "muted", {state::tr("Named after the files this copy writes, into each emulator's artwork folder (Config › Emulators).")} }
                }
            }
            div { {state::tr("prefer")} }
            div {
                label {
                    input { r#type: "checkbox", checked: prefer_main(), onchange: move |e| { prefer_main.set(e.checked()); plan.set(None); } }
                    " "
                    {state::tr("read from the main server rather than this machine where both have it")}
                }
                div { class: "muted", {state::tr("A complete copy always beats an incomplete one, whichever machine holds it.")} }
            }
        }

        div { style: "margin:8px 0",
            button { class: "btn small", r#type: "button", onclick: move |_| show_output.set(!show_output()), if show_output() { {state::tr("hide conversion settings")} } else { {state::tr("change conversion for this copy…")} } }
            if !show_output() {
                span { class: "muted", " " {state::tr("using the settings from Config › Copy")} }
            }
        }
        if show_output() {
            CopyOutputForm { output, unavailable: unavailable.clone(), on_change: move |_| plan.set(None) }
        }

        if !error().is_empty() { div { class: "card bad", "{error}" } }

        if let Some(id) = job() {
            div { class: "card", JobWatch { key: "{id}", job_id: id, on_done: move |_| finished.set(true) } }
        } else if let Some(p) = p {
            CopyPlanView { plan: p, choices: conflict_choices }
        }

        if job().is_some() && !finished() {
            div { class: "muted", style: "margin-top:6px", {state::tr("The copy keeps running in the background after this window closes; follow it under Jobs.")} }
        }
        div { class: "fsfoot",
            span { class: "grow" }
            if let Some(id) = job() {
                RunningJobButtons { job_id: id, finished: finished() }
            } else {
                button { r#type: "button", class: "btn", onclick: move |_| ui::close_modal(), {state::tr("cancel")} }
            }
            if job().is_none() {
                button { r#type: "button", class: "btn", disabled: planning(), onclick: preview, if planning() { {state::tr("checking…")} } else { {state::tr("preview")} } }
                button { r#type: "button", class: "btn primary", disabled: destination().trim().is_empty() || starting(), onclick: start, {state::tr("copy")} }
            }
        }
    }
}

/// A dialog's buttons once its job is started: leave it running, or cancel it, then close;
/// once it has ended, just close. Without the two, closing left a job running with nothing
/// saying so.
#[component]
pub fn RunningJobButtons(job_id: u64, finished: bool) -> Element {
    let mut aborting = use_signal(|| false);
    if finished {
        return rsx! { button { r#type: "button", class: "btn", onclick: move |_| ui::close_modal(), {state::tr("close")} } };
    }
    rsx! {
        button { r#type: "button", class: "btn", onclick: move |_| ui::close_modal(), {state::tr("continue in background")} }
        button { r#type: "button", class: "btn danger", disabled: aborting(),
            onclick: move |_| {
                aborting.set(true);
                spawn(async move {
                    match api::post(&format!("/api/v1/jobs/{job_id}/cancel"), serde_json::json!({})).await {
                        Ok(_) => ui::close_modal(),
                        Err(e) => {
                            ui::toast(e.message, true);
                            aborting.set(false);
                        }
                    }
                });
            },
            {state::tr("abort and close")}
        }
    }
}

/// What happens to each file already at its destination: skip (the default -- unanswered is
/// skip), rename (written beside it as ` (2)`) or overwrite, per file or for all.
#[component]
fn ConflictChoices(conflicts: Vec<String>, choices: Signal<std::collections::BTreeMap<String, ConflictChoice>>, #[props(default = "conflict".to_string())] group: String) -> Element {
    let all = |c: ConflictChoice| {
        let keys = conflicts.clone();
        move |_| {
            let mut ch = choices;
            let mut w = ch.write();
            for k in &keys {
                w.insert(k.clone(), c);
            }
        }
    };
    let options = [(ConflictChoice::Skip, "skip"), (ConflictChoice::Rename, "rename"), (ConflictChoice::Overwrite, "overwrite")];
    rsx! {
        div { class: "toolbar",
            button { class: "btn small", r#type: "button", onclick: all(ConflictChoice::Skip), {state::tr("skip all")} }
            button { class: "btn small", r#type: "button", onclick: all(ConflictChoice::Rename), {state::tr("rename all")} }
            button { class: "btn small", r#type: "button", onclick: all(ConflictChoice::Overwrite), {state::tr("overwrite all")} }
        }
        table { class: "grid",
            thead { tr { th { {state::tr("destination")} } th { "" } } }
            tbody {
                for key in conflicts.iter() {
                    tr {
                        td { class: "mono", title: "{key}", "{key}" }
                        td {
                            for (c, label) in options {
                                label {
                                    input {
                                        r#type: "radio",
                                        name: "{group}-{key}",
                                        checked: choices().get(key).copied().unwrap_or(ConflictChoice::Skip) == c,
                                        onchange: { let k = key.clone(); move |_| { let mut ch = choices; ch.write().insert(k.clone(), c); } },
                                    }
                                    " "
                                    {state::tr(label)}
                                }
                                " "
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Pressing Copy with files already in the way: say what happens to them, then copy.
#[component]
fn ConflictPopup(conflicts: Vec<String>, choices: Signal<std::collections::BTreeMap<String, ConflictChoice>>, on_go: EventHandler<()>) -> Element {
    let n = conflicts.len().to_string();
    rsx! {
        h2 { {state::tr("Files already there")} }
        p { class: "muted", {state::trf("{n} destination(s) already hold a different file. Choose what happens to each, or to all of them. Skip leaves the file there as it is; rename writes the new one beside it with (2) added; overwrite replaces it.", &[("n", &n)])} }
        ConflictChoices { conflicts: conflicts.clone(), choices, group: "pop-conflict".to_string() }
        div { class: "fsfoot",
            span { class: "grow" }
            button { r#type: "button", class: "btn", onclick: move |_| ui::close_top_modal(), {state::tr("cancel")} }
            button { r#type: "button", class: "btn primary",
                onclick: {
                    let keys = conflicts.clone();
                    move |_| {
                        // What was left as it showed is the skip it showed.
                        let mut ch = choices;
                        let mut w = ch.write();
                        for k in &keys {
                            w.entry(k.clone()).or_insert(ConflictChoice::Skip);
                        }
                        drop(w);
                        on_go.call(());
                    }
                },
                {state::tr("copy")}
            }
        }
    }
}

/// The preview: what the job will actually do, from the server's own planning pass.
#[component]
fn CopyPlanView(plan: CopyPlan, choices: Signal<std::collections::BTreeMap<String, ConflictChoice>>) -> Element {
    let p = plan;
    // A mode that never overwrites has nothing to ask: the answer is fixed, and the list is
    // shown so the user knows which files will be reported rather than copied.
    let decidable = !p.mode.never_overwrites();
    let tight = p.free_bytes.is_some_and(|free| free < p.required_bytes);
    let n_conflicts = p.conflicts.len().to_string();
    let free_text = p.free_bytes.map(|free| state::trf("{free} free; needs {needed} including the safety margin", &[("free", &bytes(free)), ("needed", &bytes(p.required_bytes))]));
    let no_mount_text = state::trf("no mount for: {list} — those files can't be reached from this machine. Set one up under Config › Share.", &[("list", &p.no_mount.join(", "))]);
    let unavailable_text = state::trf("this machine can't produce {list} — those files will be copied as they are", &[("list", &p.archiving_unavailable.join(", "))]);
    let renamed_text = state::trf("{n} name collision(s) renamed", &[("n", &p.renamed.len().to_string())]);
    let conflicts_text = if decidable {
        state::trf("{n} destination(s) already hold a different file. Choose what happens to each — anything left unanswered follows the default, which is to skip.", &[("n", &n_conflicts)])
    } else {
        state::trf("{n} destination(s) already hold a different file. This mode never overwrites: those files are left alone and reported as failures, and the rest of the copy carries on.", &[("n", &n_conflicts)])
    };
    let in_the_way_text = state::trf("the {n} file(s) in the way", &[("n", &n_conflicts)]);
    let sample_text = state::trf("first {n} of {total} file(s)", &[("n", &p.sample.len().to_string()), ("total", &p.files.to_string())]);
    rsx! {
        div { class: "card",
            div { class: "toolbar",
                Html { html: badge(&state::trf("{n} item(s)", &[("n", &p.items.to_string())]), "dim") }
                Html { html: badge(&state::trf("{n} file(s) to copy, {size}", &[("n", &p.files_to_copy.to_string()), ("size", &bytes(p.bytes_to_copy))]), "ok") }
                if p.conversions > 0 { Html { html: badge(&state::trf("{n} converted", &[("n", &p.conversions.to_string())]), "warn") } }
                if p.already_held > 0 { Html { html: badge(&state::trf("{n} already held", &[("n", &p.already_held.to_string())]), "dim") } }
                if p.mode == CopyMode::ToCollection { Html { html: badge(&state::tr("recorded as held here"), "dim") } }
                if p.already_at_destination > 0 { Html { html: badge(&state::trf("{n} already there", &[("n", &p.already_at_destination.to_string())]), "dim") } }
                if p.deduped > 0 { Html { html: badge(&state::trf("{n} duplicate(s)", &[("n", &p.deduped.to_string())]), "dim") } }
            }
            if let Some(free_text) = free_text {
                div { class: if tight { "bad" } else { "muted" },
                    "{free_text}"
                }
            }
            for w in p.warnings.iter() { div { class: "warn", "{w}" } }
            if !p.no_mount.is_empty() {
                div { class: "bad", "{no_mount_text}" }
            }
            if !p.archiving_unavailable.is_empty() {
                div { class: "warn", "{unavailable_text}" }
            }
            if !p.renamed.is_empty() {
                details {
                    summary { "{renamed_text}" }
                    for (from, to) in p.renamed.iter() { div { class: "mono", "{from} → {to}" } }
                }
            }
            if !p.conflicts.is_empty() {
                div { class: if decidable { "warn" } else { "bad" },
                    "{conflicts_text}"
                }
                if decidable {
                    details { open: true,
                        summary { "{in_the_way_text}" }
                        ConflictChoices { conflicts: p.conflicts.clone(), choices }
                    }
                } else {
                    details {
                        summary { "{in_the_way_text}" }
                        table { class: "grid",
                            thead { tr { th { {state::tr("destination")} } th { "" } } }
                            tbody {
                                for key in p.conflicts.iter() {
                                    tr {
                                        td { class: "mono", title: "{key}", "{key}" }
                                        td { span { class: "muted", {state::tr("left alone")} } }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if !p.sample.is_empty() {
                details {
                    summary { "{sample_text}" }
                    table { class: "grid",
                        thead { tr { th { {state::tr("from")} } th { {state::tr("to")} } th { {state::tr("size")} } th { "" } } }
                        tbody {
                            for f in p.sample.iter() {
                                tr {
                                    td { class: "mono", title: "{f.source}", "{f.source}" }
                                    td { class: "mono", title: "{f.destination}", "{f.destination}" }
                                    td { class: "num", "{bytes(f.size)}" }
                                    td { class: "muted",
                                        if let Some(s) = &f.skipped { "{s}" }
                                        else if f.format != OutputFormat::Same { "→ {f.format.name()}" }
                                        if let Some(src) = &f.from_source { " " {state::trf("(from {source})", &[("source", src)])} }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The archive settings, either as this copy's override or (from Config › Copy) as the
/// stored defaults. `on_change` lets the caller drop a stale preview.
#[component]
pub fn CopyOutputForm(output: Signal<Option<OutputSettings>>, unavailable: Vec<OutputFormat>, on_change: EventHandler<()>) -> Element {
    let Some(cur) = output() else { return rsx! {} };
    // `Fn`, not `FnMut`: every `oninput` below borrows it, and `Signal` is `Copy`, so a
    // fresh handle per call costs nothing.
    let edit = move |f: &dyn Fn(&mut OutputSettings)| {
        let mut sig = output;
        let mut next = sig().unwrap_or_default();
        f(&mut next);
        sig.set(Some(next));
        on_change.call(());
    };
    let zip_off = unavailable.contains(&OutputFormat::Zip);
    let chd_off = unavailable.contains(&OutputFormat::Chd);
    let rvz_off = unavailable.contains(&OutputFormat::Rvz);
    let gcz_off = unavailable.contains(&OutputFormat::Gcz);
    let wia_off = unavailable.contains(&OutputFormat::Wia);
    let wua_off = unavailable.contains(&OutputFormat::Wua);
    let zar_off = unavailable.contains(&OutputFormat::Zar);
    let xiso_off = unavailable.contains(&OutputFormat::Xiso);
    let cso_off = unavailable.contains(&OutputFormat::Cso);
    let no_native_text = state::trf("this machine has no native library for: {list} — those stay plain copies", &[("list", &unavailable.iter().map(|f| f.name()).collect::<Vec<_>>().join(", "))]);
    rsx! {
        div { class: "card",
            p { class: "muted", {state::tr("These apply to this copy only — change the defaults under Config › Copy. Which platforms are converted at all is set there too; this is how each format is written.")} }
            table { class: "grid",
                thead { tr { th { {state::tr("format")} } th { {state::tr("codec")} } th { {state::tr("level")} } th { {state::tr("re-archive")} } } }
                tbody {
                    tr { class: if zip_off { "muted" } else { "" },
                        td { "zip" }
                        td { select { onchange: move |e| { let v = e.value(); edit(&|s: &mut OutputSettings| { s.zip.codec = v.clone(); s.zip.level = level_in_range(&v, s.zip.level); }); },
                                for c in with_current(&ZIP_CODECS, &cur.zip.codec) { option { value: "{c}", selected: cur.zip.codec == c, "{c}" } }
                            } }
                        td { input { r#type: "number", min: "{codec_levels(&cur.zip.codec).0}", max: "{codec_levels(&cur.zip.codec).1}", disabled: codec_levels(&cur.zip.codec).1 == 0, style: "width:64px", value: "{cur.zip.level}",
                                oninput: move |e| { let v = e.value().parse().unwrap_or(-1); edit(&|s: &mut OutputSettings| s.zip.level = level_in_range(&s.zip.codec.clone(), v)); } } }
                        td { label { input { r#type: "checkbox", checked: cur.zip.re_archive, onchange: move |e| { let v = e.checked(); edit(&|s: &mut OutputSettings| s.zip.re_archive = v); } } " " {state::tr("re-encode a file that is already a zip")} } }
                    }
                    tr { class: if zip_off { "muted" } else { "" },
                        td { "7z" }
                        td { select { onchange: move |e| { let v = e.value(); edit(&|s: &mut OutputSettings| { s.seven_zip.codec = v.clone(); s.seven_zip.level = level_in_range(&v, s.seven_zip.level); }); },
                                for c in with_current(&SEVEN_ZIP_CODECS, &cur.seven_zip.codec) { option { value: "{c}", selected: cur.seven_zip.codec == c, "{c}" } }
                            } }
                        td { input { r#type: "number", min: "{codec_levels(&cur.seven_zip.codec).0}", max: "{codec_levels(&cur.seven_zip.codec).1}", disabled: codec_levels(&cur.seven_zip.codec).1 == 0, style: "width:64px", value: "{cur.seven_zip.level}",
                                oninput: move |e| { let v = e.value().parse().unwrap_or(-1); edit(&|s: &mut OutputSettings| s.seven_zip.level = level_in_range(&s.seven_zip.codec.clone(), v)); } } }
                        td { label { input { r#type: "checkbox", checked: cur.seven_zip.re_archive, onchange: move |e| { let v = e.checked(); edit(&|s: &mut OutputSettings| s.seven_zip.re_archive = v); } } " " {state::tr("re-encode a file that is already a 7z")} } }
                    }
                    tr { class: if rvz_off { "muted" } else { "" },
                        td { "rvz" }
                        td { select { onchange: move |e| { let v = e.value(); edit(&|s: &mut OutputSettings| { s.rvz.codec = v.clone(); s.rvz.level = level_in_range(&v, s.rvz.level); }); },
                                for c in with_current(&RVZ_CODECS, &cur.rvz.codec) { option { value: "{c}", selected: cur.rvz.codec == c, "{c}" } }
                            } }
                        td {
                            input { r#type: "number", min: "{codec_levels(&cur.rvz.codec).0}", max: "{codec_levels(&cur.rvz.codec).1}", disabled: codec_levels(&cur.rvz.codec).1 == 0, style: "width:64px", value: "{cur.rvz.level}",
                                oninput: move |e| { let v = e.value().parse().unwrap_or(-1); edit(&|s: &mut OutputSettings| s.rvz.level = level_in_range(&s.rvz.codec.clone(), v)); } }
                            " " {state::tr("block")} " "
                            select { onchange: move |e| { let v = e.value(); edit(&|s: &mut OutputSettings| s.rvz.block_size = v.clone()); },
                                for b in with_current(&RVZ_BLOCK_SIZES, &cur.rvz.block_size) { option { value: "{b}", selected: cur.rvz.block_size == b, "{b}" } }
                            }
                        }
                        td { label { input { r#type: "checkbox", checked: cur.rvz.re_archive, onchange: move |e| { let v = e.checked(); edit(&|s: &mut OutputSettings| s.rvz.re_archive = v); } } " " {state::tr("re-encode a file that is already an rvz")} } }
                    }
                    tr { class: if gcz_off { "muted" } else { "" },
                        td { "gcz" crate::ui::Explain { {state::tr("GCZ is always deflate: only the block size is yours to pick.")} } }
                        td { class: "muted", "deflate" }
                        td {
                            {state::tr("block")} " "
                            select { onchange: move |e| { let v = e.value(); edit(&|s: &mut OutputSettings| s.gcz.block_size = v.clone()); },
                                for b in with_current(&RVZ_BLOCK_SIZES, &cur.gcz.block_size) { option { value: "{b}", selected: cur.gcz.block_size == b, "{b}" } }
                            }
                        }
                        td { label { input { r#type: "checkbox", checked: cur.gcz.re_archive, onchange: move |e| { let v = e.checked(); edit(&|s: &mut OutputSettings| s.gcz.re_archive = v); } } " " {state::tr("re-encode a file that is already a gcz")} } }
                    }
                    tr { class: if wia_off { "muted" } else { "" },
                        td { "wia" crate::ui::Explain { {state::tr("WIA has no zstd (that is RVZ). Its block size is fixed at 2 MiB, the only one Dolphin offers for WIA.")} } }
                        td { select { onchange: move |e| { let v = e.value(); edit(&|s: &mut OutputSettings| { s.wia.codec = v.clone(); s.wia.level = level_in_range(&v, s.wia.level); }); },
                                for c in with_current(&WIA_CODECS, &cur.wia.codec) { option { value: "{c}", selected: cur.wia.codec == c, "{c}" } }
                            } }
                        td {
                            input { r#type: "number", min: "{codec_levels(&cur.wia.codec).0}", max: "{codec_levels(&cur.wia.codec).1}", disabled: codec_levels(&cur.wia.codec).1 == 0, style: "width:64px", value: "{cur.wia.level}",
                                oninput: move |e| { let v = e.value().parse().unwrap_or(-1); edit(&|s: &mut OutputSettings| s.wia.level = level_in_range(&s.wia.codec.clone(), v)); } }
                            " " {state::tr("block")} " 2m"
                        }
                        td { label { input { r#type: "checkbox", checked: cur.wia.re_archive, onchange: move |e| { let v = e.checked(); edit(&|s: &mut OutputSettings| s.wia.re_archive = v); } } " " {state::tr("re-encode a file that is already a wia")} } }
                    }
                    tr { class: if wua_off { "muted" } else { "" },
                        td { "wua" crate::ui::Explain { {state::tr("A Wii U disc's files, decrypted, in Cemu's own archive: zstd at a level Cemu fixes. It needs the disc's key: the .key file beside the .wux or .wud, or a keys.txt in its folder. It can't be turned back into a disc image.")} } }
                        td { class: "muted", "zstd" }
                        td { class: "muted", {state::tr("fixed")} }
                        td { class: "muted", {state::tr("Wii U .wux / .wud + key only")} }
                    }
                    tr { class: if zar_off { "muted" } else { "" },
                        td { "zar" crate::ui::Explain { {state::tr("An Xbox 360 disc's game files in Xenia's own archive: zstd at a level fixed for it. The video partition and the disc's layout are not kept, so it can't be turned back into the disc image.")} } }
                        td { class: "muted", "zstd" }
                        td { class: "muted", {state::tr("fixed")} }
                        td { class: "muted", {state::tr("Xbox 360 .iso only")} }
                    }
                    tr { class: if xiso_off { "muted" } else { "" },
                        td { "xiso" crate::ui::Explain { {state::tr("An Xbox disc's game partition alone, rewritten by extract-xiso, as xemu reads it. The video partition is dropped, so it can't be turned back into the Redump image. Not compressed.")} } }
                        td { class: "muted", "—" }
                        td { class: "muted", "—" }
                        td { class: "muted", {state::tr("Xbox .iso only")} }
                    }
                    tr { class: if cso_off { "muted" } else { "" },
                        td { "cso" crate::ui::Explain { {state::tr("A PS2 or PSP ISO in compressed 2 KiB blocks, read by PCSX2 and PPSSPP. Lossless: the ISO comes back byte for byte. Deflate level 9, which the format fixes.")} } }
                        td { class: "muted", "deflate" }
                        td { class: "muted", "9" }
                        td { class: "muted", {state::tr("PS2 / PSP .iso only")} }
                    }
                    tr {
                        td { "cci · 3ds · zcci · cxi · zcxi" crate::ui::Explain { {state::tr("A 3DS title, decrypted with your own keys: aes_keys.txt (or boot9.bin), and seeddb.bin for a title that needs it, beside the file, beside romexplorer or in its assets folder. A game becomes a .cci or .3ds, or a .cxi: its main part alone, without the manual and download-play parts. zcci and zcxi are the same compressed for Azahar (zstd level 3). An update, DLC or demo stays a .cia (.zcia), homebrew a .3dsx (.z3dsx).")} } }
                        td { class: "muted", "zstd (z…)" }
                        td { class: "muted", "3" }
                        td { class: "muted", {state::tr("3DS files + your keys only")} }
                    }
                    tr { class: if chd_off { "muted" } else { "" },
                        td { "chd" crate::ui::Explain { p { {state::tr("pc: zstd. A CD gets cdzs (zstd) with cdfl for its audio tracks; a DVD gets zstd. Fast to read, and every current PC emulator reads it.")} } p { {state::tr("mobile: the same, except PlayStation 2, which gets zlib (cdzl + cdfl for a CD, zlib for a DVD): some mobile emulators have trouble with zstd CHDs, and PS2 is where that shows.")} } p { {state::tr("Both presets choose the hunk size: for a CD, 2448 bytes (one sector) for Neo Geo CD, PC Engine CD and Dreamcast, which read in small scattered pieces, and 19584 (chdman's own) elsewhere; for a DVD, 4096, or 2048 for PSP.")} } p { {state::tr("chdman default: no codec list or hunk size, chdman decides. custom: your own codecs (up to 4) and hunk size, separately for CDs and DVDs.")} } } }
                        td { colspan: "2",
                            select { onchange: move |e| {
                                    let v = match e.value().as_str() {
                                        "pc" => romexplorer_types::copy::ChdPreset::Pc,
                                        "mobile" => romexplorer_types::copy::ChdPreset::Mobile,
                                        "custom" => romexplorer_types::copy::ChdPreset::Custom,
                                        _ => romexplorer_types::copy::ChdPreset::ChdmanDefault,
                                    };
                                    edit(&|s: &mut OutputSettings| s.chd.preset = v);
                                },
                                option { value: "chdman_default", selected: cur.chd.preset == romexplorer_types::copy::ChdPreset::ChdmanDefault, {state::tr("chdman default")} }
                                option { value: "pc", selected: cur.chd.preset == romexplorer_types::copy::ChdPreset::Pc, {state::tr("pc (zstd)")} }
                                option { value: "mobile", selected: cur.chd.preset == romexplorer_types::copy::ChdPreset::Mobile, {state::tr("mobile (zstd; PS2 zlib)")} }
                                option { value: "custom", selected: cur.chd.preset == romexplorer_types::copy::ChdPreset::Custom, {state::tr("custom")} }
                            }
                            if cur.chd.preset == romexplorer_types::copy::ChdPreset::Custom {
                                ChdCustomLine { output, on_change, dvd: false }
                                ChdCustomLine { output, on_change, dvd: true }
                            }
                        }
                        td { label { input { r#type: "checkbox", checked: cur.chd.re_archive, onchange: move |e| { let v = e.checked(); edit(&|s: &mut OutputSettings| s.chd.re_archive = v); } } " " {state::tr("re-encode a file that is already a chd")} } }
                    }
                }
            }
            if !unavailable.is_empty() {
                div { class: "warn", "{no_native_text}" }
            }
        }
    }
}

/// One line of the CHD custom preset: a CD's values, or a DVD's (and any other non-CD
/// image's). Four codec pulldowns, best first, any of them blank (skipped), then the hunk
/// size typed in, blank letting chdman choose.
#[component]
fn ChdCustomLine(output: Signal<Option<OutputSettings>>, on_change: EventHandler<()>, dvd: bool) -> Element {
    let Some(cur) = output() else { return rsx! {} };
    let (codecs, hunk) = if dvd { (cur.chd.dvd_codecs.clone(), cur.chd.dvd_hunk_size) } else { (cur.chd.codecs.clone(), cur.chd.hunk_size) };
    let offered: &[&str] = if dvd { &CHD_DVD_CODECS } else { &CHD_CD_CODECS };
    let edit = move |f: &dyn Fn(&mut romexplorer_types::copy::ChdSettings)| {
        let mut sig = output;
        let mut next = sig().unwrap_or_default();
        f(&mut next.chd);
        sig.set(Some(next));
        on_change.call(());
    };
    let (label, hint) = if dvd {
        (state::tr("DVD / other"), state::tr("bytes per hunk; blank lets chdman choose. A multiple of 2048 for a DVD (e.g. 4096), of 512 for a hard disk image."))
    } else {
        (state::tr("CD"), state::tr("bytes per hunk; blank lets chdman choose. A multiple of 2448, the CD frame (e.g. 19584 = 8 frames)."))
    };
    rsx! {
        div { style: "margin-top:4px",
            span { style: "display:inline-block;min-width:90px", "{label}" }
            {state::tr("codecs")} " "
            for slot in 0..CHD_MAX_CODECS {
                {
                    let here = codecs.get(slot).cloned().unwrap_or_default();
                    rsx! {
                        select { key: "{slot}",
                            onchange: move |e| {
                                let v = e.value();
                                edit(&|c: &mut romexplorer_types::copy::ChdSettings| {
                                    let list = if dvd { &mut c.dvd_codecs } else { &mut c.codecs };
                                    let mut slots = list.clone();
                                    slots.resize(CHD_MAX_CODECS, String::new());
                                    slots[slot] = v.clone();
                                    *list = slots.into_iter().filter(|x| !x.is_empty()).collect();
                                });
                            },
                            option { value: "", selected: here.is_empty(), "" }
                            for c in with_current(offered, &here).into_iter().filter(|c| !c.is_empty()) { option { value: "{c}", selected: here == c, "{c}" } }
                        }
                        " "
                    }
                }
            }
            " " {state::tr("hunk")} " "
            input { r#type: "number", min: "0", step: "1", style: "width:90px", placeholder: state::tr("auto"), title: "{hint}",
                value: "{hunk.map(|n| n.to_string()).unwrap_or_default()}",
                oninput: move |e| {
                    let v = e.value().trim().parse::<u32>().ok().filter(|n| *n > 0);
                    edit(&|c: &mut romexplorer_types::copy::ChdSettings| if dvd { c.dvd_hunk_size = v } else { c.hunk_size = v });
                }
            }
        }
    }
}

/// `level` if `codec` takes it, else that codec's default (`codec_levels`).
fn level_in_range(codec: &str, level: i8) -> i8 {
    let (lo, hi, def) = codec_levels(codec);
    if (lo..=hi).contains(&level) { level } else { def }
}

/// A pulldown's options, plus the stored value when it isn't one of them (set by hand in the
/// JSON, or by an older version), so opening the form never silently changes it.
fn with_current(options: &[&str], current: &str) -> Vec<String> {
    let mut out: Vec<String> = options.iter().map(|o| o.to_string()).collect();
    if !current.is_empty() && !options.contains(&current) {
        out.insert(0, current.to_string());
    }
    out
}

/// Selection helpers for the six levels, so a menu site is one line.
pub fn set_selection(id: u64) -> CopySelection {
    CopySelection { set_ids: vec![id], ..Default::default() }
}
pub fn title_selection(ids: Vec<u64>) -> CopySelection {
    CopySelection { title_ids: ids, ..Default::default() }
}
pub fn release_selection(ids: Vec<u64>) -> CopySelection {
    CopySelection { release_ids: ids, ..Default::default() }
}
pub fn game_selection(ids: Vec<u64>) -> CopySelection {
    CopySelection { game_ids: ids.into_iter().map(romexplorer_types::copy::OwnedRef::local).collect(), ..Default::default() }
}
pub fn rom_selection(ids: Vec<u64>) -> CopySelection {
    CopySelection { rom_ids: ids.into_iter().map(romexplorer_types::copy::OwnedRef::local).collect(), ..Default::default() }
}
pub fn file_selection(ids: Vec<u64>) -> CopySelection {
    CopySelection { file_ids: ids.into_iter().map(romexplorer_types::copy::OwnedRef::local).collect(), ..Default::default() }
}

/// "Copy to…" for a context menu on one row, or on the whole selection when several rows
/// are selected and the clicked one is among them (the grid hands that over as `ids`).
pub fn context_item(kind: &'static str, id: u64, name: &str, ids: Option<Vec<u64>>) -> MenuItem {
    let many = ids.as_ref().filter(|v| v.len() > 1).cloned();
    let (label, selection, what) = match &many {
        Some(v) => (state::trf("Copy {n} selected {kind} to…", &[("n", &v.len().to_string()), ("kind", &state::kind_word(kind, false))]), selection_for(kind, v.clone()), state::trf("{n} selected {kind}", &[("n", &v.len().to_string()), ("kind", &state::kind_word(kind, false))])),
        None => (state::tr("Copy to…"), selection_for(kind, vec![id]), if name.is_empty() { state::trf("this {kind}", &[("kind", &state::kind_word(kind, true))]) } else { name.to_string() }),
    };
    copy_menu_item(&label, selection, what)
}

fn selection_for(kind: &'static str, ids: Vec<u64>) -> CopySelection {
    match kind {
        // The Titles list's rows: each a title's releases of one kind.
        "titles" => CopySelection { title_row_ids: ids, ..Default::default() },
        "releases" => release_selection(ids),
        "sets" => CopySelection { set_ids: ids, ..Default::default() },
        "roms" => rom_selection(ids),
        "files" => file_selection(ids),
        _ => game_selection(ids),
    }
}
