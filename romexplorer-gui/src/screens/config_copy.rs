//! Config › Copy: how a copy lays out its destination, and what it converts files to.
//!
//! Three cards on one tab, because they are three halves of one decision and splitting them
//! would mean reading a platform's row in two places: the folder scheme and the combine
//! groups, the per-platform table (folder name + output format), and the archive settings
//! those formats are written with.

use dioxus::prelude::*;
use romexplorer_types::copy::{CopyDefaults, FolderScheme, HeaderPreference, NamingUpdate, NamingView, OutputFormat, WiiuPreference, QUICK_HASHES};

use super::copy::CopyOutputForm;
use crate::api;
use crate::html::badge;
use crate::state;
use crate::ui::{self, Html, Explain};

#[component]
pub fn ConfigCopy() -> Element {
    let mut data = use_resource(|| async {
        let naming = api::get_as::<NamingView>("/api/v1/naming/folders").await?;
        let defaults = api::get_as::<CopyDefaults>("/api/v1/copy/defaults").await?;
        Ok::<_, api::ApiError>((naming, defaults))
    });
    let (naming, defaults) = match &*data.read() {
        None => return rsx! { div { class: "muted", {state::tr("loading…")} } },
        Some(Err(e)) => return rsx! { div { class: "card bad", "{e.message}" } },
        Some(Ok(v)) => v.clone(),
    };

    // Pending edits, applied on save. Keyed by platform id so a row edited twice is one entry.
    let mut folder_edits = use_signal(std::collections::BTreeMap::<u16, String>::new);
    let mut format_edits = use_signal(std::collections::BTreeMap::<u16, OutputFormat>::new);
    let mut search = use_signal(String::new);
    let output = use_signal(|| Some(defaults.output.clone()));
    let mut decide = use_signal(|| defaults.decide_on_copy);
    let mut verify_default = use_signal(|| defaults.verify_default);
    let mut populate_hash = use_signal(|| defaults.populate_hash.clone());
    let mut header_pref = use_signal(|| defaults.header_preference);
    let mut wiiu_pref = use_signal(|| defaults.wiiu_preference);
    let lines = |order: &Vec<Vec<String>>| order.iter().map(|f| f.join(", ")).collect::<Vec<_>>().join("\n");
    let mut formats = use_signal(|| lines(&defaults.format_order));
    let mut prefer_decrypted = use_signal(|| defaults.prefer_decrypted);
    let mut lossy_warning = use_signal(|| defaults.lossy_warning);
    // Bumped to rebuild the output selects, so one whose lossy choice was declined shows
    // what it had again.
    let mut select_rev = use_signal(|| 0u32);
    let save_formats = move |order: Vec<Vec<String>>| {
        spawn(async move {
            match api::put_as::<_, CopyDefaults>("/api/v1/copy/defaults", &serde_json::json!({ "format_order": order })).await {
                Ok(d) => formats.set(d.format_order.iter().map(|f| f.join(", ")).collect::<Vec<_>>().join("\n")),
                Err(e) => ui::toast(e.message, true),
            }
        });
    };

    let scheme = naming.scheme;
    let save_naming = move |update: NamingUpdate| {
        spawn(async move {
            match api::put_as::<NamingUpdate, NamingView>("/api/v1/naming/folders", &update).await {
                Ok(_) => {
                    ui::toast(state::tr("saved"), false);
                    folder_edits.write().clear();
                    format_edits.write().clear();
                    data.restart();
                }
                Err(e) => ui::toast(e.message, true),
            }
        });
    };

    let filter = search().trim().to_lowercase();
    let rows: Vec<_> = naming
        .platforms
        .iter()
        .filter(|p| filter.is_empty() || p.name.to_lowercase().contains(&filter) || p.effective.to_lowercase().contains(&filter))
        .cloned()
        .collect();
    let unavailable = defaults.unavailable_formats.clone();
    let pending = folder_edits().len() + format_edits().len();

    rsx! {
        div { class: "card",
            h2 { {state::tr("Destination folders")} }
            Explain {
                {state::tr("A copy puts each platform's files in its own subdirectory of the folder you pick. Which name that is comes from the scheme below, unless you set one yourself in the table.")}
            }
            div { class: "toolbar",
                label { {state::tr("scheme")}
                    select { onchange: move |e| { if let Some(s) = FolderScheme::from_name(&e.value()) { save_naming(NamingUpdate { scheme: Some(s), ..Default::default() }); } },
                        for s in FolderScheme::ALL { option { value: "{s.name()}", selected: scheme == s, {state::tr(s.label())} } }
                    }
                }
                button { class: "btn small danger", r#type: "button",
                    onclick: move |_| {
                        spawn(async move {
                            if !ui::confirm(&state::tr("Clear every custom folder name? Output formats are left alone.")).await { return; }
                            match api::post("/api/v1/naming/folders/reset", serde_json::json!({})).await {
                                Ok(_) => { ui::toast(state::tr("custom names cleared"), false); data.restart(); }
                                Err(e) => ui::toast(e.message, true),
                            }
                        });
                    },
                    {state::tr("clear custom names")}
                }
            }
            if !naming.shared_folders.is_empty() {
                details {
                    summary { class: "warn", {state::trf("{n} folder(s) shared by more than one platform", &[("n", &naming.shared_folders.len().to_string())])} }
                    for (folder, names) in naming.shared_folders.iter() {
                        div { span { class: "mono", "{folder}" } " — {names.join(\", \")}" }
                    }
                    p { class: "muted", {state::tr("Legal, and sometimes what you want. Their games land in one directory.")} }
                }
            }
        }

        div { class: "card",
            h2 { {state::tr("Combine")} }
            Explain { {state::tr("romexplorer tells these apart; most folder layouts don't. Switched on, the first platform's games go in the second's folder.")} }
            div { class: "optgrid", style: "grid-template-columns:repeat(auto-fill, minmax(340px, 1fr))",
                for g in naming.combine.iter() {
                    {
                        let key = g.key.clone();
                        let on = g.on;
                        rsx! {
                            label {
                                input { r#type: "checkbox", checked: on, onchange: move |e| { let v = e.checked(); save_naming(NamingUpdate { combine: vec![(key.clone(), v)], ..Default::default() }); } }
                                " {g.label}"
                            }
                        }
                    }
                }
            }
        }

        div { class: "card",
            h2 { {state::tr("Platforms")} }
            div { class: "toolbar",
                input { r#type: "text", placeholder: state::tr("filter platforms…"), value: "{search}", oninput: move |e| search.set(e.value()) }
                span { class: "grow" }
                if pending > 0 { span { class: "muted", {state::trf("{n} unsaved change(s)", &[("n", &pending.to_string())])} } }
                button { class: "btn primary", r#type: "button", disabled: pending == 0,
                    onclick: move |_| {
                        let update = NamingUpdate {
                            scheme: None,
                            combine: Vec::new(),
                            folders: folder_edits().into_iter().collect(),
                            formats: format_edits().into_iter().collect(),
                        };
                        save_naming(update);
                    },
                    {state::tr("save")}
                }
            }
            // A card per platform, as many columns as fit: name, custom folder (the folder in
            // use as its placeholder) and output. Every scheme's name is in the tooltip.
            div { class: "pgrid",
                if rows.is_empty() { div { class: "muted", {state::tr("nothing matches")} } }
                for p in rows {
                    {
                        let id = p.app_platform_id;
                        let current = folder_edits().get(&id).cloned().unwrap_or_else(|| p.custom.clone().unwrap_or_default());
                        let format = format_edits().get(&id).copied().unwrap_or(p.output_format);
                        let mark = |s: FolderScheme| if scheme == s { "▸ " } else { "   " };
                        let names = format!(
                            "{}{}: {}\n{}{}: {}\n{}{}: {}\n{}{}: {}",
                            mark(FolderScheme::Dat), state::tr("pack name"), p.dat,
                            mark(FolderScheme::Display), state::tr("display"), p.display,
                            mark(FolderScheme::Slug), state::tr("slug"), p.slug.clone().unwrap_or_else(|| "—".into()),
                            mark(FolderScheme::Es), state::tr("short"), p.es,
                        );
                        let hint = format!("{}\n{names}", state::tr("folder name; empty uses the scheme"));
                        rsx! {
                            div { class: "pcard", key: "{id}",
                                div { class: "pname", title: "{names}",
                                    "{p.name}"
                                    if p.custom.is_some() { " " Html { html: badge(&state::tr("custom"), "ok") } }
                                    if let Some(into) = &p.combined_into { " " Html { html: badge(&format!("→ {into}"), "dim") } }
                                }
                                div { class: "prow",
                                    input { r#type: "text", placeholder: "{p.effective}", title: "{hint}", value: "{current}", oninput: move |e| { folder_edits.write().insert(id, e.value()); } }
                                    select { key: "{id}-{select_rev}", title: state::tr("output"), onchange: move |e| {
                                            let Some(f) = OutputFormat::from_name(&e.value()) else { return };
                                            if !f.is_lossy() || !lossy_warning() {
                                                format_edits.write().insert(id, f);
                                                return;
                                            }
                                            // Back to what it was until the answer is yes.
                                            *select_rev.write() += 1;
                                            ui::ask_lossy(f.name(), move |yes, dont_ask| {
                                                if !yes {
                                                    return;
                                                }
                                                let (mut format_edits, mut lossy_warning) = (format_edits, lossy_warning);
                                                format_edits.write().insert(id, f);
                                                if dont_ask {
                                                    lossy_warning.set(false);
                                                    spawn(async move {
                                                        if let Err(e) = api::put("/api/v1/copy/defaults", serde_json::json!({ "lossy_warning": false })).await { ui::toast(e.message, true); }
                                                    });
                                                }
                                            });
                                        },
                                        for f in p.formats.iter() {
                                            option { value: "{f.name()}", selected: format == *f, disabled: unavailable.contains(f) && *f != OutputFormat::Same,
                                                "{f.name()}" if unavailable.contains(f) && *f != OutputFormat::Same { {state::tr(" (no library)")} }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Explain {
                {state::tr("\"same\" copies the bytes as they are. \"raw\" unpacks an archive into loose files. A format a platform can't take isn't offered — a GameCube disc has no CHD, a cartridge has no RVZ.")}
            }
        }

        div { class: "card",
            h2 { {state::tr("Archive settings")} }
            Explain { {state::tr("How each format is written when a copy converts to it. The defaults are this project's measured ones (see CLAUDE.md): zstd at level 5 throughout, 128 KiB RVZ blocks.")} }
            label { style: "display:block; margin-bottom:6px",
                input { r#type: "checkbox", checked: verify_default(),
                    onchange: move |e| {
                        let v = e.checked();
                        verify_default.set(v);
                        spawn(async move {
                            let body = serde_json::json!({ "verify_default": v });
                            if let Err(e) = api::put("/api/v1/copy/defaults", body).await { ui::toast(e.message, true); }
                        });
                    }
                }
                {state::tr(" check copies by default — read each file back and compare one hash")}
            }
            label { style: "display:block; margin-bottom:6px",
                input { r#type: "checkbox", checked: lossy_warning(),
                    onchange: move |e| {
                        let v = e.checked();
                        lossy_warning.set(v);
                        spawn(async move {
                            if let Err(e) = api::put("/api/v1/copy/defaults", serde_json::json!({ "lossy_warning": v })).await { ui::toast(e.message, true); }
                        });
                    }
                }
                {state::tr(" ask before a lossy output (wua, zar, xiso) — one that can't be turned back into the disc image")}
            }
            label { style: "display:block; margin-bottom:6px",
                {state::tr("a checked copy compares with ")}
                select { onchange: move |e| {
                        let v = e.value();
                        populate_hash.set(v.clone());
                        spawn(async move {
                            let body = serde_json::json!({ "populate_hash": v });
                            if let Err(e) = api::put("/api/v1/copy/defaults", body).await { ui::toast(e.message, true); }
                        });
                    },
                    for h in QUICK_HASHES { option { value: "{h}", selected: populate_hash() == h, "{h}" } }
                }
                Explain { {state::tr("A checked copy compares what it wrote against this hash whenever the catalogue has it for those bytes. Where it does not, it compares against the cheapest hash the catalogue does hold and records this one once the copy matches, so later checks use it. xxh3_128 (the default) is far stronger than crc32 at nearly its speed. The integrity check has its own setting (integrity_populate_hash).")} }
            }
            label { style: "display:block; margin-bottom:6px",
                input { r#type: "checkbox", checked: decide(),
                    onchange: move |e| {
                        let v = e.checked();
                        decide.set(v);
                        spawn(async move {
                            let body = serde_json::json!({ "decide_on_copy": v });
                            if let Err(e) = api::put("/api/v1/copy/defaults", body).await { ui::toast(e.message, true); }
                        });
                    }
                }
                {state::tr(" decide on copy — open every copy's dialog with these settings expanded and editable")}
            }
            CopyOutputForm { output, unavailable: unavailable.clone(), on_change: move |_| {} }
            div { class: "toolbar",
                span { class: "grow" }
                button { class: "btn primary", r#type: "button",
                    onclick: move |_| {
                        let Some(o) = output() else { return };
                        spawn(async move {
                            let body = serde_json::json!({ "output": o });
                            match api::put("/api/v1/copy/defaults", body).await {
                                Ok(_) => ui::toast(state::tr("saved"), false),
                                Err(e) => ui::toast(e.message, true),
                            }
                        });
                    },
                    {state::tr("save archive settings")}
                }
            }
        }

        div { class: "card",
            h2 { {state::tr("Which file a copy takes")} }
            Explain { {state::tr("A copy takes one copy of each release and one file of each game. These decide which, when there is more than one.")} }
            label { style: "display:block; margin-bottom:6px",
                {state::tr("headered or headerless: ")}
                select { onchange: move |e| {
                        let Some(v) = HeaderPreference::from_name(&e.value()) else { return };
                        header_pref.set(v);
                        spawn(async move {
                            let body = serde_json::json!({ "header_preference": v });
                            if let Err(e) = api::put("/api/v1/copy/defaults", body).await { ui::toast(e.message, true); }
                        });
                    },
                    for (p, label) in [(HeaderPreference::Priority, "folder priority"), (HeaderPreference::Headered, "headered"), (HeaderPreference::Headerless, "headerless")] {
                        option { value: "{p.name()}", selected: header_pref() == p, {state::tr(label)} }
                    }
                }
                Explain { {state::tr("When one copy of a release holds a headered and a headerless dump of one cartridge (the DAT names the headerless one .unh), which one a copy takes. Folder priority takes the one in the folder first under Config › Directories. The set's size counts the same file.")} }
            }
            label { style: "display:block; margin-bottom:6px",
                {state::tr("Wii U, wua or wux: ")}
                select { onchange: move |e| {
                        let Some(v) = WiiuPreference::from_name(&e.value()) else { return };
                        wiiu_pref.set(v);
                        spawn(async move {
                            let body = serde_json::json!({ "wiiu_preference": v });
                            if let Err(e) = api::put("/api/v1/copy/defaults", body).await { ui::toast(e.message, true); }
                        });
                    },
                    for (p, label) in [(WiiuPreference::Wua, "wua"), (WiiuPreference::Wux, "wux / wud"), (WiiuPreference::Priority, "folder priority")] {
                        option { value: "{p.name()}", selected: wiiu_pref() == p, {state::tr(label)} }
                    }
                }
                Explain { {state::tr("When one copy of a release holds a Wii U game both as Cemu's .wua and as a disc image (.wux or .wud), which one a copy takes. With the .wua, the disc's .key stays behind too. Folder priority takes the one in the folder first under Config › Directories. The set's size counts the same file.")} }
            }
            div { style: "margin-bottom:6px",
                div { {state::tr("dump formats, first preferred:")} }
                textarea { rows: "5", style: "width:100%; font-family:monospace", value: "{formats}",
                    oninput: move |e| formats.set(e.value()),
                    onchange: move |e| {
                        let order: Vec<Vec<String>> = e.value().lines().map(|l| l.split([',', ' ', '>']).map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()).collect();
                        save_formats(order);
                    }
                }
                button { class: "btn small", r#type: "button", onclick: move |_| save_formats(romexplorer_types::copy::default_format_order()), {state::tr("back to the defaults")} }
                Explain { {state::tr("When one copy holds the same game in several formats (the same name, different extensions), a copy takes the first format of its line. One family per line, first preferred. A disc's cue and bin are never two formats, and .nes / .unh follow the headered or headerless setting. The set's size counts the same file.")} }
            }
            label { style: "display:block; margin-bottom:6px",
                input { r#type: "checkbox", checked: prefer_decrypted(),
                    onchange: move |e| {
                        let v = e.checked();
                        prefer_decrypted.set(v);
                        spawn(async move {
                            if let Err(e) = api::put("/api/v1/copy/defaults", serde_json::json!({ "prefer_decrypted": v })).await { ui::toast(e.message, true); }
                        });
                    }
                }
                {state::tr(" prefer decrypted dumps over encrypted ones")}
                Explain { {state::tr("Between copies of one release, a decrypted dump (3DS, DS, Wii U, …) comes before one whose folder or file name says Encrypted. A copy you put first by hand still comes first.")} }
            }
        }
    }
}
