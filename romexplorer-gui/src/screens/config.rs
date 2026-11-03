//! Config: directories (with the folder picker), emulators, storage, schedules, server (the
//! console's `showDirectories` … `showServer`). Settings are in `settings.rs`.

use std::collections::HashMap;

use dioxus::prelude::*;
use serde_json::{json, Value};

use crate::api::{self, s, u};
use crate::html::{self, badge};
use crate::state;
use crate::ui::{self, Html, Explain};

const DISK_NOTE: &str = "Only romexplorer's configuration is removed — folders and files on disk are never touched.";

// --- path fields: the OS's folder dialog when the server runs on this machine, else an
// in-page browser over /api/v1/fs/dirs ---

async fn picker_native() -> bool {
    thread_local! { static NATIVE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) }; }
    if let Some(n) = NATIVE.with(|c| c.get()) {
        return n;
    }
    let n = api::get("/api/v1/fs/picker").await.map(|v| api::b(&v, "native")).unwrap_or(false);
    NATIVE.with(|c| c.set(Some(n)));
    n
}

/// A path input with "browse…" (and "in page" when a native dialog exists). `start`: where
/// the picker opens when the field is empty. `file`: pick a file instead of a folder (native
/// only).
#[component]
pub fn PathInput(value: Signal<String>, #[props(default)] placeholder: String, #[props(default)] start: String, #[props(default)] file: bool, #[props(default = "min-width:360px".to_string())] style: String) -> Element {
    let native = use_resource(picker_native);
    let is_native = native.read().unwrap_or(false);
    let mut value = value;
    let begin = move || {
        let v = value.peek().trim().to_string();
        if v.is_empty() {
            start.clone()
        } else {
            v
        }
    };
    let begin2 = begin.clone();
    let begin3 = begin.clone();
    let pick_native = move |_| {
        let st = begin();
        spawn(async move {
            if !file {
                ui::toast(state::tr("choose a folder in the dialog that opened on this computer"), false);
            }
            let mut body = json!({ "start": if st.is_empty() { Value::Null } else { json!(st) } });
            if file {
                body["file"] = json!(true);
            }
            match api::post("/api/v1/fs/pick", body).await {
                Ok(r) => {
                    if let Some(p) = api::so(&r, "path") {
                        value.set(p);
                    }
                }
                Err(e) => {
                    if file {
                        ui::toast(e.message, true);
                    } else {
                        ui::toast(state::trf("{error} — using the in-page browser instead", &[("error", &e.message)]), true);
                        folder_browser(value, st);
                    }
                }
            }
        });
    };
    rsx! {
        span { class: "browse",
            input { r#type: "text", style: "{style}", placeholder: "{placeholder}", value: "{value}", oninput: move |e| value.set(e.value()) }
            if is_native {
                button { r#type: "button", class: "btn small", title: if file { state::tr("choose the file on this computer") } else { state::tr("open the folder dialog on this computer") }, onclick: pick_native, {state::tr("browse…")} }
                if !file {
                    button { r#type: "button", class: "btn small", title: state::tr("browse the server's folders in this page instead"), onclick: move |_| folder_browser(value, begin2()), {state::tr("in page")} }
                }
            } else if !file {
                button { r#type: "button", class: "btn small", title: state::tr("browse the server's folders"), onclick: move |_| folder_browser(value, begin3()), {state::tr("browse…")} }
            }
        }
    }
}

/// Above the dialog the field is in, when it is in one: `target` belongs to that dialog.
pub fn folder_browser(target: Signal<String>, start: String) {
    ui::open_over_modal(move || rsx! { FolderBrowser { target, start: start.clone() } });
}

#[component]
fn FolderBrowser(target: Signal<String>, start: String) -> Element {
    let mut path = use_signal(|| start.clone());
    let mut typed = use_signal(|| start.clone());
    let mut fallback = use_signal(|| true);
    let listing = use_resource(move || async move {
        let p = path();
        match api::get(&format!("/api/v1/fs/dirs{}", api::qs(&[("path", p.clone())]))).await {
            Ok(l) => Ok(l),
            Err(e) => {
                // A missing / mistyped start opens the roots instead.
                if *fallback.peek() && !p.is_empty() {
                    fallback.set(false);
                    path.set(String::new());
                }
                Err(e.message)
            }
        }
    });
    // Said once per failed listing, not on every redraw (each keystroke in the path box is one).
    use_effect(move || {
        if let Some(Err(e)) = &*listing.read() {
            ui::toast(e.clone(), true);
        }
    });
    let l = match &*listing.read() {
        Some(Ok(l)) => Some(l.clone()),
        _ => None,
    };
    let cur = l.as_ref().and_then(|l| api::so(l, "path"));
    let parent = l.as_ref().and_then(|l| api::so(l, "parent")).unwrap_or_default();
    let entries = l.as_ref().map(|l| api::arr(l, "entries").to_vec()).unwrap_or_default();
    let roots = l.as_ref().map(|l| api::arr(l, "roots").to_vec()).unwrap_or_default();
    use_effect(move || {
        if let Some(Ok(l)) = &*listing.read() {
            typed.set(api::so(l, "path").unwrap_or_default());
        }
    });
    let info = if cur.is_some() {
        if entries.len() == 1 { state::tr("1 subfolder") } else { state::trf("{n} subfolders", &[("n", &entries.len().to_string())]) }
    } else {
        state::tr("pick a drive or location")
    };
    let mut target = target;
    let cur2 = cur.clone();
    rsx! {
        h2 { {state::tr("Choose a folder on the server")} }
        div { class: "fsroots",
            for r in roots { button { r#type: "button", class: "btn small", onclick: { let p = s(&r, "path"); move |_| path.set(p.clone()) }, "{s(&r, \"name\")}" } }
        }
        form { class: "fspath", onsubmit: move |e| { e.prevent_default(); path.set(typed()); },
            button { r#type: "button", class: "btn", title: state::tr("up one level"), disabled: cur.is_none(), onclick: move |_| path.set(parent.clone()), "↑" }
            input { r#type: "text", placeholder: state::tr("type or paste a path"), value: "{typed}", oninput: move |e| typed.set(e.value()) }
            button { class: "btn", {state::tr("go")} }
        }
        div { class: "fslist",
            if entries.is_empty() { div { class: "empty", {state::tr("no subfolders")} } }
            for e in entries {
                div { title: "{s(&e, \"path\")}", onclick: { let p = s(&e, "path"); move |_| path.set(p.clone()) },
                    svg { view_box: "0 0 16 16", path { d: "M1 3a1 1 0 0 1 1-1h4l2 2h6a1 1 0 0 1 1 1v8a1 1 0 0 1-1 1H2a1 1 0 0 1-1-1z" } }
                    span { "{s(&e, \"name\")}" }
                }
            }
        }
        div { class: "fsfoot",
            span { class: "muted grow", "{info}" }
            button { r#type: "button", class: "btn", onclick: move |_| ui::close_top_modal(), {state::tr("cancel")} }
            button { r#type: "button", class: "btn primary", disabled: cur2.is_none(), onclick: move |_| { if let Some(c) = cur2.clone() { let _ = target.try_write().map(|mut t| *t = c); } ui::close_top_modal(); }, {state::tr("use this folder")} }
        }
    }
}

// --- directories ---

#[component]
pub fn ConfigDirectories() -> Element {
    let mut dirs = use_resource(|| async { api::get("/api/v1/directories").await.map(|v| api::items(&v)) });
    let mut edit_id = use_signal(|| None::<u64>);
    let path = use_signal(String::new);
    let mut dir_type = use_signal(String::new);
    let mut platform = use_signal(String::new);
    let mut is_root = use_signal(|| false);
    let mut archival = use_signal(|| false);
    let list = match &*dirs.read() {
        None => return rsx! { div { class: "muted", {state::tr("loading…")} } },
        Some(Err(e)) => return rsx! { div { class: "card bad", "{e.message}" } },
        Some(Ok(l)) => l.clone(),
    };
    let by_id: HashMap<u64, Value> = list.iter().map(|d| (u(d, "id"), d.clone())).collect();
    // The server lists them in tree order by priority (siblings ▲▼), so its order is kept.
    let roots: Vec<Value> = list.iter().filter(|d| api::b(d, "is_collection_root")).cloned().collect();
    let depth = |d: &Value| {
        let mut n = 0;
        let mut p = api::uo(d, "parent_directory_id");
        while let Some(pid) = p {
            if n >= 64 || !by_id.contains_key(&pid) {
                break;
            }
            n += 1;
            p = api::uo(&by_id[&pid], "parent_directory_id");
        }
        n
    };
    // Tree order: each root, then everything configured below it.
    let mut ordered: Vec<Value> = Vec::new();
    for r in &roots {
        ordered.push(r.clone());
        let below: Vec<u64> = api::arr(r, "descendants").iter().map(|x| u(x, "id")).collect();
        ordered.extend(list.iter().filter(|d| below.contains(&u(d, "id"))).cloned());
    }
    let seen: Vec<u64> = ordered.iter().map(|d| u(d, "id")).collect();
    ordered.extend(list.iter().filter(|d| !seen.contains(&u(d, "id"))).cloned());
    let no_root = roots.is_empty();
    let start = if !is_root() { roots.first().map(|r| s(r, "path")).unwrap_or_default() } else { String::new() };
    let types: Vec<String> = state::vocab().get("dir_types").and_then(|t| t.as_array()).into_iter().flatten().filter_map(|t| t.as_str().map(str::to_string)).collect();
    let mut path_w = path;
    let clear = move || {
        edit_id.set(None);
        path_w.set(String::new());
        dir_type.set(String::new());
        platform.set(String::new());
        is_root.set(false);
        archival.set(false);
    };
    let mut clear2 = clear.clone();
    let rows: Vec<(Value, usize)> = ordered.iter().map(|d| (d.clone(), depth(d))).collect();
    rsx! {
        div { class: "card",
            h2 { {state::tr("Add / edit directory")} }
            p { class: "muted",
                if no_root { {state::tr("Add a ")} b { {state::tr("collection root")} } {state::tr(" first — every other directory is configured inside one.")} } else { {state::tr("Collection roots hold your sets (any number, e.g. redump and no-intro apart, but never one inside another). Other directories go inside a root, to override its type/platform/archival; their parent follows from the path.")} }
            }
            form { class: "inline",
                onsubmit: move |e| {
                    e.prevent_default();
                    let spec = json!({
                        "path": path(),
                        "dir_type": if dir_type().is_empty() { Value::Null } else { json!(dir_type()) },
                        "app_platform_id": platform().parse::<u64>().ok(),
                        "is_collection_root": is_root() || no_root,
                        "is_archival": archival(),
                    });
                    let id = edit_id();
                    let mut clear = clear.clone();
                    spawn(async move {
                        let r = match id { Some(id) => api::put(&format!("/api/v1/directories/{id}"), spec).await, None => api::post("/api/v1/directories", spec).await };
                        match r { Ok(_) => { ui::toast(state::tr("saved"), false); clear(); dirs.restart(); } Err(e) => ui::toast(e.message, true) }
                    });
                },
                label { {state::tr("path")} PathInput { value: path, placeholder: if no_root { state::tr("collection root folder") } else { state::tr("a folder inside a collection root") }, start } }
                label { {state::tr("type")}
                    select { onchange: move |e| dir_type.set(e.value()),
                        option { value: "", selected: dir_type().is_empty(), {state::tr("inherit")} }
                        for t in types { option { selected: t == dir_type(), "{t}" } }
                    }
                }
                label { {state::tr("platform")}
                    select { onchange: move |e| platform.set(e.value()),
                        option { value: "", {state::tr("inherit")} }
                        for p in state::platforms() { option { value: "{u(&p, \"id\")}", selected: u(&p, "id").to_string() == platform(), "{s(&p, \"name\")}" } }
                    }
                }
                label { span { {state::tr("collection root")} }
                    input { r#type: "checkbox", checked: is_root() || no_root, disabled: no_root, title: if no_root { state::tr("the first directory has to be a collection root") } else { String::new() }, onchange: move |e| is_root.set(e.checked()) }
                }
                label { span { {state::tr("archival")} } input { r#type: "checkbox", checked: archival(), onchange: move |e| archival.set(e.checked()) } }
                button { class: "btn primary", {state::tr("save")} }
                button { class: "btn", r#type: "button", onclick: move |_| clear2(), {state::tr("clear")} }
            }
        }
        div { class: "card",
            h2 { {state::trf("Configured directories ({n})", &[("n", &list.len().to_string())])} }
            table { class: "grid",
                thead { tr { th { {state::tr("path")} } th { {state::tr("type")} } th { {state::tr("platform")} } th { {state::tr("archival")} } th { {state::tr("parent")} } th { {state::tr("files")} } th { {state::tr("on disk")} } th {} } }
                tbody {
                    if rows.is_empty() { tr { td { colspan: "8", class: "muted", {state::tr("none yet — add a collection root above, then scan it from Jobs")} } } }
                    for (d, dep) in rows {
                        {
                            let id = u(&d, "id");
                            let parent = api::uo(&d, "parent_directory_id").and_then(|p| by_id.get(&p)).map(|p| s(p, "path")).unwrap_or_default();
                            let eff_type = if api::so(&d, "dir_type").is_none() { api::so(&d, "effective_dir_type") } else { None };
                            let eff_plat = if api::uo(&d, "app_platform_id").is_none() { api::so(&d, "effective_platform_name") } else { None };
                            let d_edit = d.clone();
                            let d_del = d.clone();
                            rsx! {
                                tr {
                                    td { class: "mono", style: "padding-left:{8 + dep * 18}px",
                                        if api::b(&d, "is_collection_root") { Html { html: badge(&state::tr("root"), "ok") } " " } else { "└ " }
                                        "{s(&d, \"path\")}"
                                    }
                                    td { "{s(&d, \"dir_type\")} " if let Some(t) = eff_type { Html { html: badge(&format!("→ {t}"), "dim") } } }
                                    td { Html { html: html::cbadge("platform", &s(&d, "platform_name"), None, None) } " " if let Some(p) = eff_plat { Html { html: badge(&format!("→ {p}"), "dim") } } }
                                    td { if api::b(&d, "is_archival") { Html { html: badge(&state::tr("archival"), "warn") } } }
                                    td { class: "mono muted", "{parent}" }
                                    td { class: "num", "{s(&d, \"file_count\")}" }
                                    td { Html { html: if api::b(&d, "exists_on_disk") { badge(&state::tr("yes"), "ok") } else { badge(&state::tr("missing"), "bad") } } }
                                    td {
                                        for (arrow, dir, tip) in [("▲", "up", "higher priority among its siblings"), ("▼", "down", "lower priority among its siblings")] {
                                            button { class: "btn small", title: state::tr(tip),
                                                onclick: move |_| {
                                                    spawn(async move {
                                                        match api::post(&format!("/api/v1/directories/{id}/move"), json!({ "direction": dir })).await {
                                                            Ok(_) => dirs.restart(),
                                                            Err(e) => ui::toast(e.message, true),
                                                        }
                                                    });
                                                },
                                                "{arrow}"
                                            }
                                        }
                                        " "
                                        button { class: "btn small",
                                            onclick: move |_| {
                                                let d = &d_edit;
                                                edit_id.set(Some(id));
                                                path_w.set(s(d, "path"));
                                                dir_type.set(s(d, "dir_type"));
                                                platform.set(api::uo(d, "app_platform_id").map(|p| p.to_string()).unwrap_or_default());
                                                is_root.set(api::b(d, "is_collection_root"));
                                                archival.set(api::b(d, "is_archival"));
                                                let _ = document::eval("window.scrollTo(0, 0)");
                                            },
                                            {state::tr("edit")}
                                        }
                                        " "
                                        button { class: "btn small danger", onclick: move |_| remove_directory(d_del.clone(), move || { let mut d = dirs; d.restart(); }), {state::tr("remove")} }
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

/// Removing a directory's configuration; with configured subdirectories below it: a warning
/// listing them, then "are you sure?", then a recursive removal.
fn remove_directory(d: Value, done: impl Fn() + Clone + 'static) {
    let n = api::arr(&d, "descendants").len();
    if n == 0 {
        spawn(async move {
            if !ui::confirm(&state::trf("Remove the configuration for {path}?\n\n{note}", &[("path", &s(&d, "path")), ("note", &state::tr(DISK_NOTE))])).await {
                return;
            }
            match api::delete(&format!("/api/v1/directories/{}", u(&d, "id"))).await {
                Ok(r) => {
                    let k = api::arr(&r, "removed").len();
                    ui::toast(removed_msg(k), false);
                    done();
                }
                Err(e) => {
                    // A subdirectory was configured since this list loaded: go through the
                    // recursive warning instead.
                    if e.message.starts_with("conflict") {
                        if let Ok(fresh) = api::get(&format!("/api/v1/directories/{}", u(&d, "id"))).await {
                            if !api::arr(&fresh, "descendants").is_empty() {
                                remove_directory(fresh, done);
                                return;
                            }
                        }
                    }
                    ui::toast(e.message, true);
                }
            }
        });
        return;
    }
    let done = std::rc::Rc::new(done);
    ui::open_modal(move || {
        let d = d.clone();
        let done = done.clone();
        let list: Vec<String> = api::arr(&d, "descendants").iter().map(|x| s(x, "path")).collect();
        let id = u(&d, "id");
        let what = if api::b(&d, "is_collection_root") { state::tr("collection root") } else { state::tr("directory") };
        let title = state::trf("Remove {what} and its subdirectories?", &[("what", &what)]);
        let below = if n == 1 { state::tr(" subdirectory below it:") } else { state::tr(" subdirectories below it:") };
        rsx! {
            h2 { "{title}" }
            p { {state::tr("Removing ")} span { class: "mono", "{s(&d, \"path\")}" } {state::tr(" also removes the configuration of ")} b { "{n}" } "{below}" }
            ul { class: "mono", style: "max-height:40vh; overflow:auto; margin:6px 0 10px", for p in list { li { "{p}" } } }
            p { class: "muted", {state::tr(DISK_NOTE)} }
            div { class: "fsfoot",
                span { class: "grow" }
                button { r#type: "button", class: "btn", onclick: move |_| ui::close_modal(), {state::tr("cancel")} }
                button { r#type: "button", class: "btn danger",
                    onclick: move |_| {
                        let done = done.clone();
                        spawn(async move {
                            if !ui::confirm(&state::trf("Are you sure? This removes {n} directory configurations.\n\n{note}", &[("n", &(n + 1).to_string()), ("note", &state::tr(DISK_NOTE))])).await { return; }
                            match api::delete(&format!("/api/v1/directories/{id}?recursive=true")).await {
                                Ok(r) => { let k = api::arr(&r, "removed").len(); ui::toast(removed_msg(k), false); ui::close_modal(); done(); }
                                Err(e) => ui::toast(e.message, true),
                            }
                        });
                    },
                    {state::tr("delete")}
                }
            }
        }
    });
}

/// "removed N directory configuration(s)".
fn removed_msg(k: usize) -> String {
    if k == 1 {
        state::tr("removed 1 directory configuration")
    } else {
        state::trf("removed {n} directory configurations", &[("n", &k.to_string())])
    }
}

// --- emulators ---

fn platform_name(id: u64) -> String {
    state::platform_name(id)
}

#[component]
pub fn ConfigEmulators() -> Element {
    let mut data = use_resource(|| async {
        let list = api::get("/api/v1/emulators").await.map(|v| api::items(&v))?;
        let presets = api::get("/api/v1/emulators/presets").await.map(|v| api::items(&v))?;
        Ok::<_, api::ApiError>((list, presets))
    });
    let (list, presets) = match &*data.read() {
        None => return rsx! { div { class: "muted", {state::tr("loading…")} } },
        Some(Err(e)) => return rsx! { div { class: "card bad", "{e.message}" } },
        Some(Ok(v)) => v.clone(),
    };
    let reload = move || data.restart();
    let list2 = list.clone();
    let presets2 = presets.clone();
    rsx! {
        div { class: "card",
            h2 { {state::tr("Emulators")} }
            div { class: "warnbox", {state::tr("Not tested yet: launching emulators is new and has not been tried on real setups. Expect rough edges, and report what does not work.")} }
            Explain { {state::tr("Right-click a rom (Roms, a rom page, a release's local copies) or a game with an .m3u playlist and pick ")} b { {state::tr("Open with …")} } {state::tr(": the emulators set up for its platform. The emulator starts on the machine running this app. Presets are the emulators RetroAchievements supports; RetroArch runs the core set per platform.")} }
            div { class: "toolbar",
                select {
                    onchange: move |e| {
                        let Some(x) = presets2.iter().find(|p| s(p, "key") == e.value()) else { return };
                        let platforms: Vec<Value> = api::arr(x, "platforms").iter().filter_map(|p| p.as_array()).map(|p| {
                            let id = p.first().and_then(|v| v.as_u64()).unwrap_or(0);
                            let taken = list2.iter().any(|o| api::arr(o, "platforms").iter().any(|q| u(q, "app_platform_id") == id && api::b(q, "is_default")));
                            json!({ "app_platform_id": id, "core": p.get(1).cloned().unwrap_or(Value::Null), "is_default": !taken })
                        }).collect();
                        emulator_dialog(json!({ "id": 0, "name": s(x, "name"), "preset": s(x, "key"), "kind": s(x, "kind"), "path": "", "args": s(x, "args"), "cores_dir": null, "working_dir": null, "supports_archives": api::b(x, "supports_archives"), "supports_playlists": api::b(x, "supports_playlists"), "enabled": true, "platforms": platforms, "artwork": x.get("artwork").cloned().unwrap_or(Value::Null) }), reload);
                    },
                    option { value: "", {state::tr("add from preset…")} }
                    for x in presets.iter() { option { value: "{s(x, \"key\")}", "{s(x, \"name\")}" } }
                }
                button { class: "btn small", onclick: move |_| emulator_dialog(json!({ "id": 0, "name": "", "preset": null, "kind": "standalone", "path": "", "args": "{rom}", "cores_dir": null, "working_dir": null, "supports_archives": false, "supports_playlists": true, "enabled": true, "platforms": [] }), reload), {state::tr("add custom")} }
            }
        }
        div { class: "card",
            table { class: "grid",
                thead { tr { th { {state::tr("emulator")} } th { {state::tr("path")} } th { {state::tr("platforms")} } th { {state::tr("state")} } th {} } }
                tbody {
                    if list.is_empty() { tr { td { colspan: "5", class: "muted", {state::tr("none yet — add one from a preset")} } } }
                    for e in list {
                        {
                            let id = u(&e, "id");
                            let e2 = e.clone();
                            rsx! {
                                tr {
                                    td { b { "{s(&e, \"name\")}" } " " span { class: "muted", "{s(&e, \"kind\")}" } }
                                    td { class: "mono", style: "max-width:360px;overflow:hidden;text-overflow:ellipsis", title: "{s(&e, \"path\")}", "{s(&e, \"path\")}" }
                                    td {
                                        for p in api::arr(&e, "platforms").to_vec() {
                                            {
                                                let def = api::b(&p, "is_default");
                                                let tip = format!("{}{}", if def { state::tr("default for this platform") } else { String::new() }, api::so(&p, "core").map(|c| state::trf(" core {core}", &[("core", &c)])).unwrap_or_default());
                                                rsx! { span { class: if def { "badge ok" } else { "badge" }, title: "{tip}", "{platform_name(u(&p, \"app_platform_id\"))}" if def { " ★" } } " " }
                                            }
                                        }
                                    }
                                    td { Html { html: if api::b(&e, "enabled") { badge(&state::tr("on"), "ok") } else { badge(&state::tr("off"), "dim") } } }
                                    td {
                                        button { class: "btn small", onclick: move |_| emulator_dialog(e2.clone(), reload), {state::tr("edit")} }
                                        " "
                                        button { class: "btn small danger",
                                            onclick: move |_| {
                                                spawn(async move {
                                                    if !ui::confirm(&state::tr("Delete this emulator? (Only the setting; the program itself is untouched.)")).await { return; }
                                                    match api::delete(&format!("/api/v1/emulators/{id}")).await { Ok(_) => data.restart(), Err(e) => ui::toast(e.message, true) }
                                                });
                                            },
                                            {state::tr("delete")}
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
}

fn emulator_dialog(e: Value, done: impl FnMut() + Clone + 'static) {
    let done = std::rc::Rc::new(std::cell::RefCell::new(done));
    ui::open_modal(move || {
        let d = done.clone();
        rsx! { EmulatorDialog { emu: e.clone(), on_done: move |_| (d.borrow_mut())() } }
    });
}

#[component]
fn EmulatorDialog(emu: Value, on_done: EventHandler<()>) -> Element {
    let e = emu;
    let mut name = use_signal(|| s(&e, "name"));
    let mut kind = use_signal(|| s(&e, "kind"));
    let path = use_signal(|| s(&e, "path"));
    let mut args = use_signal(|| s(&e, "args"));
    let mut cores_dir = use_signal(|| s(&e, "cores_dir"));
    let mut working_dir = use_signal(|| s(&e, "working_dir"));
    let mut archives = use_signal(|| api::b(&e, "supports_archives"));
    // Rows saved before the setting existed carry none: they opened playlists.
    let mut playlists = use_signal(|| e.get("supports_playlists").and_then(Value::as_bool).unwrap_or(true));
    let mut enabled = use_signal(|| api::b(&e, "enabled"));
    let mut rows = use_signal(|| api::arr(&e, "platforms").to_vec());
    // Where its own art goes ("export artwork"); an empty layout = no export.
    let art = e.get("artwork").cloned().unwrap_or(Value::Null);
    let art_dir = use_signal(|| s(&art, "dir"));
    let mut art_layout = use_signal(|| s(&art, "layout"));
    let mut art_formats = use_signal(|| api::arr(&art, "formats").iter().filter_map(|f| f.as_str().map(str::to_string)).collect::<Vec<String>>());
    let mut art_width = use_signal(|| art.get("max_width").and_then(|v| v.as_u64()).map(|w| w.to_string()).unwrap_or_default());
    let mut art_after = use_signal(|| api::b(&art, "after_copy"));
    let ra = kind() == "retroarch";
    let id = u(&e, "id");
    let preset = e.get("preset").cloned().unwrap_or(Value::Null);
    rsx! {
        h3 { if id > 0 { {state::tr("Edit emulator")} } else { {state::tr("Add emulator")} } }
        form { class: "kv", style: "grid-template-columns:140px 1fr",
            onsubmit: move |ev| {
                ev.prevent_default();
                let a = args().trim().to_string();
                let body = json!({
                    "id": id, "name": name().trim(), "preset": preset, "kind": kind(), "path": path().trim(),
                    "args": if a.is_empty() { "{rom}".to_string() } else { a },
                    "cores_dir": Some(cores_dir().trim().to_string()).filter(|x| !x.is_empty()),
                    "working_dir": Some(working_dir().trim().to_string()).filter(|x| !x.is_empty()),
                    "supports_archives": archives(), "supports_playlists": playlists(), "enabled": enabled(), "platforms": rows(),
                    "artwork": if art_layout().is_empty() { Value::Null } else { json!({
                        "dir": art_dir().trim(), "layout": art_layout(), "formats": art_formats(),
                        "max_width": art_width().trim().parse::<u32>().ok().filter(|w| *w > 0), "after_copy": art_after(),
                    }) },
                });
                spawn(async move {
                    match api::post("/api/v1/emulators", body).await {
                        Ok(_) => { ui::toast(state::tr("saved"), false); ui::close_modal(); on_done.call(()); }
                        Err(e) => ui::toast(e.message, true),
                    }
                });
            },
            div { {state::tr("name")} } div { input { required: true, value: "{name}", oninput: move |e| name.set(e.value()) } }
            div { {state::tr("kind")} }
            div {
                select { onchange: move |e| kind.set(e.value()),
                    option { value: "standalone", selected: kind() == "standalone", {state::tr("standalone")} }
                    option { value: "retroarch", selected: kind() == "retroarch", "RetroArch" }
                    option { value: "command", selected: kind() == "command", {state::tr("command line (e.g. flatpak run …)")} }
                }
            }
            div { {state::tr("path")} }
            div { PathInput { value: path, file: true, style: "width:100%", placeholder: "C:\\Emulators\\duckstation.exe · /Applications/RetroArch.app · ~/Apps/Dolphin.AppImage" } }
            div { {state::tr("arguments")} }
            div { input { style: "width:100%", value: "{args}", oninput: move |e| args.set(e.value()) } " " span { class: "muted", {state::tr("{rom} = the file; {core} = the RetroArch core")} } }
            if ra {
                div { {state::tr("cores folder")} }
                div { input { style: "width:100%", placeholder: state::tr("default: cores next to RetroArch / the OS default"), value: "{cores_dir}", oninput: move |e| cores_dir.set(e.value()) } }
            }
            div { {state::tr("working folder")} }
            div { input { style: "width:100%", placeholder: state::tr("default: the emulator's folder"), value: "{working_dir}", oninput: move |e| working_dir.set(e.value()) } }
            div { {state::tr("options")} }
            div {
                label { input { r#type: "checkbox", checked: archives(), onchange: move |e| archives.set(e.checked()) } {state::tr(" opens zip / 7z itself")} }
                " "
                label { title: state::tr("off: a multi-disc game starts from its first disc instead of its .m3u"), input { r#type: "checkbox", checked: playlists(), onchange: move |e| playlists.set(e.checked()) } {state::tr(" opens .m3u playlists")} }
                " "
                label { input { r#type: "checkbox", checked: enabled(), onchange: move |e| enabled.set(e.checked()) } {state::tr(" enabled")} }
            }
            div { {state::tr("artwork")} }
            div {
                select {
                    onchange: move |e| {
                        let v = e.value();
                        // A layout picked for the first time starts from what that emulator reads.
                        if art_formats().is_empty() || art_layout().is_empty() {
                            art_formats.set(if v == "retroarch" { vec!["png".to_string()] } else { vec!["jpg".to_string(), "png".to_string(), "webp".to_string()] });
                            if v == "retroarch" && art_width().is_empty() { art_width.set("512".to_string()); }
                        }
                        art_layout.set(v);
                    },
                    option { value: "", selected: art_layout().is_empty(), {state::tr("no export")} }
                    option { value: "retroarch", selected: art_layout() == "retroarch", {state::tr("RetroArch thumbnails (Named_Boxarts, Named_Titles, Named_Snaps, Named_Logos)")} }
                    option { value: "cover_folder", selected: art_layout() == "cover_folder", {state::tr("covers folder, by serial (PCSX2, DuckStation)")} }
                    option { value: "cover_folder_name", selected: art_layout() == "cover_folder_name", {state::tr("covers folder, by file name (PCSX2, DuckStation)")} }
                }
                if !art_layout().is_empty() {
                    div { style: "margin-top:4px",
                        PathInput { value: art_dir, style: "width:100%", placeholder: if art_layout() == "retroarch" { state::tr("RetroArch's thumbnails folder") } else { state::tr("the emulator's covers folder") } }
                    }
                    div { style: "margin-top:4px",
                        span { {state::tr("formats it reads: ")} }
                        for f in ["png", "jpg", "webp"] {
                            label { style: "margin-right:10px",
                                input { r#type: "checkbox", checked: art_formats().iter().any(|x| x == f),
                                    onchange: move |e| { let mut list = art_formats.write(); list.retain(|x| x != f); if e.checked() { list.push(f.to_string()); } } }
                                " {f}"
                            }
                        }
                        if art_layout() == "retroarch" {
                            div { class: "muted", {state::tr("JPG and WebP only work with RetroArch's Settings › Playlists › \"Allow non-PNG thumbnails\" on (1.20+); without it, leave PNG alone.")} }
                        }
                        div { class: "muted", {state::tr("A picture already in a ticked format is copied as it is; anything else is converted to the first ticked one this build can write (PNG, JPG).")} }
                    }
                    div { style: "margin-top:4px",
                        span { {state::tr("max width ")} }
                        input { size: "6", value: "{art_width}", placeholder: state::tr("none"), oninput: move |e| art_width.set(e.value()) }
                        span { class: "muted", {state::tr(" px; wider pictures are scaled down, never up")} }
                    }
                    div { style: "margin-top:4px",
                        label { input { r#type: "checkbox", checked: art_after(), onchange: move |e| art_after.set(e.checked()) } {state::tr(" tick \"export artwork\" in the copy dialog for this emulator")} }
                    }
                }
            }
            div { {state::tr("platforms")} }
            div {
                table { class: "grid",
                    tr { th { {state::tr("platform")} } if ra { th { {state::tr("core")} } } th { {state::tr("default")} } th {} }
                    for (i, p) in rows().into_iter().enumerate() {
                        tr { key: "{u(&p, \"app_platform_id\")}",
                            td { "{platform_name(u(&p, \"app_platform_id\"))}" }
                            if ra { td { input { size: "16", value: "{s(&p, \"core\")}", oninput: move |e| { rows.write()[i]["core"] = json!(e.value()); } } } }
                            td { input { r#type: "checkbox", checked: api::b(&p, "is_default"), onchange: move |e| { rows.write()[i]["is_default"] = json!(e.checked()); } } }
                            td { span { class: "link", onclick: move |_| { rows.write().remove(i); }, "✕" } }
                        }
                    }
                }
                select {
                    onchange: move |e| {
                        if let Ok(pid) = e.value().parse::<u64>() {
                            if !rows.read().iter().any(|r| u(r, "app_platform_id") == pid) {
                                rows.write().push(json!({ "app_platform_id": pid, "core": null, "is_default": false }));
                            }
                        }
                    },
                    option { value: "", selected: true, {state::tr("add platform…")} }
                    for p in state::platforms().into_iter().filter(|p| u(p, "id") > 0) { option { value: "{u(&p, \"id\")}", "{s(&p, \"name\")}" } }
                }
            }
            div {}
            div { button { class: "btn primary", {state::tr("save")} } " " button { class: "btn", r#type: "button", onclick: move |_| ui::close_modal(), {state::tr("cancel")} } }
        }
    }
}

// --- storage ---

#[component]
pub fn ConfigStorage() -> Element {
    let mut data = use_resource(|| async { api::get("/api/v1/storage").await });
    let mut role = use_signal(String::new);
    let media_path = use_signal(String::new);
    let mut tier = use_signal(|| "1".to_string());
    let temp_path = use_signal(String::new);
    let mut gib = use_signal(|| "4".to_string());
    let mut pick_out = use_signal(String::new);
    let st = match &*data.read() {
        None => return rsx! { div { class: "muted", {state::tr("loading…")} } },
        Some(Err(e)) => return rsx! { div { class: "card bad", "{e.message}" } },
        Some(Ok(v)) => v.clone(),
    };
    let roles: Vec<String> = api::arr(&st, "media_roles").iter().filter_map(|r| r.as_str().map(str::to_string)).collect();
    // `image` unless chosen otherwise: it is the dir people come here to set, and the first
    // role in the list is a cache tier, which holds copies and never the art itself.
    let chosen_role = if role().is_empty() { roles.iter().find(|r| *r == "image").or(roles.first()).cloned().unwrap_or_default() } else { role() };
    let shown_role = chosen_role.clone();
    let downloads = api::arr(&st, "download_dirs").to_vec();
    let download_ids: Vec<u64> = downloads.iter().filter_map(|d| api::uo(d, "media_dir_id")).collect();
    let media = api::arr(&st, "media_dirs").to_vec();
    let temps = api::arr(&st, "temp_dirs").to_vec();
    let (mut mp, mut tp) = (media_path, temp_path);
    rsx! {
        div { class: "two",
            div { class: "card",
                h2 { {state::tr("Media dirs")} }
                Explain { {state::tr("tier1/tier2 cache: one each (setting replaces). image/video/audio: any number. Default media dir: ")} span { class: "mono", "{s(&st, \"default_media_dir\")}" } }
                p { class: "muted", {state::tr("Art a sync downloads is written to the first image dir (videos to the first video dir, audio to the first audio dir), else the default media dir. The cache tiers only hold copies made when art is shown. Art downloaded before a dir was added stays where it is until Jobs › media relayout moves it.")} }
                table { class: "grid",
                    thead { tr { th { {state::tr("new downloads")} } th { {state::tr("go to")} } } }
                    tbody {
                        for d in downloads.iter() {
                            tr { key: "{s(d, \"role\")}",
                                td { "{s(d, \"role\")}" }
                                td { class: "mono", "{s(d, \"path\")} " if api::uo(d, "media_dir_id").is_none() { Html { html: badge(&state::tr("default"), "dim") } } }
                            }
                        }
                    }
                }
                form { class: "inline",
                    onsubmit: move |e| {
                        e.prevent_default();
                        let body = json!({ "role": chosen_role.clone(), "path": mp() });
                        spawn(async move { match api::post("/api/v1/storage/media", body).await { Ok(_) => { ui::toast(state::tr("added"), false); mp.set(String::new()); data.restart(); } Err(e) => ui::toast(e.message, true) } });
                    },
                    label { {state::tr("role")} select { onchange: move |e| role.set(e.value()), for r in roles { option { selected: r == shown_role, "{r}" } } } }
                    label { {state::tr("path")} PathInput { value: media_path, style: "min-width:320px" } }
                    button { class: "btn primary", {state::tr("add")} }
                }
                table { class: "grid",
                    thead { tr { th { {state::tr("role")} } th { {state::tr("path")} } th { {state::tr("on disk")} } th {} } }
                    tbody {
                        if media.is_empty() { tr { td { colspan: "4", class: "muted", {state::tr("none")} } } }
                        for d in media {
                            {
                                let id = u(&d, "id");
                                rsx! {
                                    tr {
                                        td { "{s(&d, \"role\")}" }
                                        td { class: "mono", "{s(&d, \"path\")} " if download_ids.contains(&id) { Html { html: badge(&state::tr("new downloads"), "ok") } } }
                                        td {
                                            if !api::b(&d, "exists_on_disk") { Html { html: badge(&state::tr("missing"), "bad") } }
                                            else if d.get("writable").and_then(|w| w.as_bool()) == Some(false) {
                                                span { title: state::tr("The server cannot write here, so every download into it fails. A server run as a service has its own user: give that user the folder (chown)."), Html { html: badge(&state::tr("not writable"), "bad") } }
                                            }
                                            else { Html { html: badge(&state::tr("yes"), "ok") } }
                                        }
                                        td { button { class: "btn small danger", onclick: move |_| { spawn(async move { match api::delete(&format!("/api/v1/storage/media/{id}")).await { Ok(_) => data.restart(), Err(e) => ui::toast(e.message, true) } }); }, {state::tr("remove")} } }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            div { class: "card",
                h2 { {state::tr("Temp dirs")} }
                Explain { {state::tr("tier 1 (fast, e.g. ramdisk/SSD) is tried first, tier 2 as fallback; an operation only gets a dir with its needed bytes plus the ")} span { class: "mono", "temp_dir_margin" } {state::tr(" setting free. Provider cache: ")} span { class: "mono", "{s(&st, \"default_cache_dir\")}" } }
                form { class: "inline",
                    onsubmit: move |e| {
                        e.prevent_default();
                        let body = json!({ "tier": tier().parse::<u64>().unwrap_or(1), "path": tp() });
                        spawn(async move { match api::post("/api/v1/storage/temp", body).await { Ok(_) => { ui::toast(state::tr("set"), false); tp.set(String::new()); data.restart(); } Err(e) => ui::toast(e.message, true) } });
                    },
                    label { {state::tr("tier")} select { onchange: move |e| tier.set(e.value()), option { "1" } option { "2" } } }
                    label { {state::tr("path")} PathInput { value: temp_path, style: "min-width:320px" } }
                    button { class: "btn primary", {state::trc("action", "set")} }
                }
                table { class: "grid",
                    thead { tr { th { {state::tr("tier")} } th { {state::tr("path")} } th { {state::tr("on disk")} } th {} } }
                    tbody {
                        if temps.is_empty() { tr { td { colspan: "4", class: "muted", {state::tr("none")} } } }
                        for d in temps {
                            {
                                let id = u(&d, "id");
                                rsx! {
                                    tr {
                                        td { "{s(&d, \"tier\")}" }
                                        td { class: "mono", "{s(&d, \"path\")}" }
                                        td { Html { html: if api::b(&d, "exists_on_disk") { badge(&state::tr("yes"), "ok") } else { badge(&state::tr("missing"), "bad") } } }
                                        td { button { class: "btn small danger", onclick: move |_| { spawn(async move { match api::delete(&format!("/api/v1/storage/temp/{id}")).await { Ok(_) => data.restart(), Err(e) => ui::toast(e.message, true) } }); }, {state::tr("remove")} } }
                                    }
                                }
                            }
                        }
                    }
                }
                form { class: "inline", style: "margin-top:10px",
                    onsubmit: move |e| {
                        e.prevent_default();
                        let g: f64 = gib().parse().unwrap_or(0.0);
                        let bytes = (g * 1073741824.0).round() as u64;
                        spawn(async move {
                            match api::get(&format!("/api/v1/storage/temp/pick?bytes={bytes}")).await {
                                Ok(r) => {
                                    let text = match r.get("choice").filter(|c| !c.is_null()) {
                                        Some(c) => {
                                            let d = c.get("dir").cloned().unwrap_or_default();
                                            state::trf("tier {tier}: {path} (free {free}, reserved {reserved}, margin {margin})", &[("tier", &s(&d, "tier")), ("path", &s(&d, "path")), ("free", &html::fmt_size(api::uo(c, "available_bytes"))), ("reserved", &html::fmt_size(api::uo(c, "reserved_bytes"))), ("margin", &html::fmt_size(api::uo(c, "margin_bytes")))])
                                        }
                                        None => {
                                            let m = r.get("margin").cloned().unwrap_or_default();
                                            state::trf("no temp dir has {gib} GiB + margin ({percent}% / min {min}) free", &[("gib", &g.to_string()), ("percent", &s(&m, "percent")), ("min", &html::fmt_size(api::uo(&m, "min_bytes")))])
                                        }
                                    };
                                    pick_out.set(text);
                                }
                                Err(e) => pick_out.set(e.message),
                            }
                        });
                    },
                    label { {state::tr("which temp dir would an operation needing")} input { r#type: "number", min: "0", step: "0.5", value: "{gib}", oninput: move |e| gib.set(e.value()) } {state::tr(" GiB get?")} }
                    button { class: "btn", {state::tr("check")} }
                }
                div { class: "muted", "{pick_out}" }
            }
        }
    }
}

// --- schedules ---

const UNITS: [&str; 5] = ["minutes", "hours", "days", "weeks", "months"];

#[component]
pub fn ConfigSchedules() -> Element {
    let mut data = use_resource(|| async {
        let list = api::get("/api/v1/schedules").await.map(|v| api::items(&v))?;
        let ps = api::get("/api/v1/providers").await.map(|v| api::items(&v))?;
        // The directories a quick scan can be pointed at; a server that refuses the list
        // still gets the rest of the screen.
        let dirs = api::get("/api/v1/directories").await.map(|v| api::items(&v)).unwrap_or_default();
        let sets = api::get("/api/v1/sets").await.map(|v| api::items(&v)).unwrap_or_default();
        Ok::<_, api::ApiError>((list, ps, dirs, sets))
    });
    let mut ai_set = use_signal(String::new);
    let mut ai_max = use_signal(String::new);
    let mut edit_id = use_signal(|| None::<u64>);
    let mut name = use_signal(String::new);
    let mut directory = use_signal(String::new);
    let mut when_idle = use_signal(|| true);
    let mut mscope = use_signal(|| "owned".to_string());
    let mut job = use_signal(|| "provider_bulk_sync".to_string());
    let mut provider = use_signal(String::new);
    let mut scope = use_signal(String::new);
    let mut platform = use_signal(String::new);
    let mut source = use_signal(String::new);
    let mut mode = use_signal(|| "diff".to_string());
    let mut trigger = use_signal(|| "fixed".to_string());
    let mut fixed_n = use_signal(|| "7".to_string());
    let mut fixed_u = use_signal(|| "days".to_string());
    let mut check_n = use_signal(|| "6".to_string());
    let mut check_u = use_signal(|| "days".to_string());
    let mut enabled = use_signal(|| true);
    let (list, ps, dirs, sets) = match &*data.read() {
        None => return rsx! { div { class: "muted", {state::tr("loading…")} } },
        Some(Err(e)) => return rsx! { div { class: "card bad", "{e.message}" } },
        Some(Ok(v)) => v.clone(),
    };
    let bulk: Vec<Value> = ps.iter().filter(|p| api::b(p, "available") && { let c = p.get("capabilities").cloned().unwrap_or_default(); api::b(&c, "platform") || api::b(&c, "full") }).cloned().collect();
    let prov_name = if bulk.iter().any(|p| s(p, "name") == provider()) { provider() } else { bulk.first().map(|p| s(p, "name")).unwrap_or_default() };
    // The platforms the chosen provider has, when it only has some.
    let only: Option<Vec<u64>> = bulk.iter().find(|p| s(p, "name") == prov_name).and_then(|p| p.get("bulk_platforms")).and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_u64()).collect());
    let caps = bulk.iter().find(|p| s(p, "name") == prov_name).and_then(|p| p.get("capabilities").cloned()).unwrap_or_default();
    let mut scopes: Vec<(&str, &str)> = Vec::new();
    if api::b(&caps, "platform") {
        scopes.push(("platform", "one platform"));
        scopes.push(("all", "every catalog platform"));
    }
    if api::b(&caps, "full") {
        scopes.push(("full", "full database"));
    }
    let sc = if scopes.iter().any(|(v, _)| *v == scope()) { scope() } else { scopes.first().map(|(v, _)| v.to_string()).unwrap_or_default() };
    let jt = job();
    let tr = trigger();
    let describe = |t: &Value| match s(t, "job_type").as_str() {
        "provider_bulk_sync" => format!(
            "{} {}",
            s(t, "provider"),
            if api::b(t, "full") {
                state::tr("full")
            } else {
                match t.get("platform") {
                    Some(Value::Number(n)) => platform_name(n.as_u64().unwrap_or(0)),
                    _ => state::tr("all platforms"),
                }
            }
        ),
        "dat_sync" => state::trf("DAT sync {source}", &[("source", &api::so(t, "source").unwrap_or_else(|| state::tr("all sources")))]),
        "file_scan" => {
            let id = u(t, "directory_id");
            let path = dirs.iter().find(|d| u(d, "id") == id).map(|d| s(d, "path")).unwrap_or_else(|| format!("#{id}"));
            state::trf("quick scan {dir}", &[("dir", &path)])
        }
        "ra_badges" => state::trf("RA badges: {scope}", &[("scope", &if s(t, "scope") == "owned" { state::tr("owned games") } else { state::tr("played by the account") })]),
        "media_backfill" => {
            let mut scope = if s(t, "scope") == "all" { state::tr("the whole catalogue") } else { state::tr("owned games") };
            if let Some(p) = api::uo(t, "platform") {
                scope = format!("{scope} · {}", platform_name(p));
            }
            state::trf("missing media: {scope}", &[("scope", &scope)])
        }
        "ai_batch_rate" => {
            let id = u(t, "set_id");
            let set = sets.iter().find(|x| u(x, "id") == id).map(|x| s(x, "name")).unwrap_or_else(|| format!("#{id}"));
            state::trf("batch rate with AI: {set}", &[("set", &set)])
        }
        x => x.to_string(),
    };
    let when = |n: Option<u64>, unit: String| n.map(|n| state::trf("every {n} {unit}", &[("n", &n.to_string()), ("unit", &state::tr(&unit))])).unwrap_or_default();
    let dat_sources: Vec<String> = state::vocab().pointer("/jobs/dat_sources").and_then(|p| p.as_array()).into_iter().flatten().filter_map(|x| x.as_str().map(str::to_string)).collect();
    let (sc2, prov2) = (sc.clone(), prov_name.clone());
    let reset = move || {
        edit_id.set(None);
        name.set(String::new());
        job.set("provider_bulk_sync".into());
        scope.set(String::new());
        platform.set(String::new());
        source.set(String::new());
        mode.set("diff".into());
        trigger.set("fixed".into());
        fixed_n.set("7".into());
        fixed_u.set("days".into());
        check_n.set("6".into());
        check_u.set("days".into());
        enabled.set(true);
        directory.set(String::new());
        when_idle.set(true);
        mscope.set("owned".into());
        ai_set.set(String::new());
        ai_max.set(String::new());
    };
    let mut reset2 = reset.clone();
    rsx! {
        div { class: "card",
            h2 { {state::tr("Add / edit schedule")} }
            Explain { b { {state::tr("Fixed")} } {state::tr(" runs the job every interval. ")} b { {state::tr("Check")} } {state::tr(" asks the provider every interval whether its data changed (one small request: IGDB update count, TheGamesDB dump ETag, Hasheous checksums, EmuReady game count) and runs a diff sync only then; RetroAchievements has no such check, so use fixed for it. ")} b { {state::tr("Both")} } {state::tr(" does the two independently.")} }
            form { class: "inline",
                onsubmit: move |e| {
                    e.prevent_default();
                    let jt = job();
                    let mut target = json!({ "job_type": jt });
                    if jt == "provider_bulk_sync" {
                        target["provider"] = json!(prov2);
                        match sc2.as_str() {
                            "full" => target["full"] = json!(true),
                            "all" => target["platform"] = json!("all"),
                            _ => { let Ok(p) = platform().parse::<u64>() else { ui::toast(state::tr("choose a platform"), true); return; }; target["platform"] = json!(p); }
                        }
                    } else if jt == "dat_sync" {
                        target["source"] = if source().is_empty() { Value::Null } else { json!(source()) };
                        target["import"] = json!(true);
                    } else if jt == "file_scan" {
                        let Ok(dir) = directory().parse::<u64>() else { ui::toast(state::tr("pick a directory"), true); return; };
                        target["directory_id"] = json!(dir);
                        target["mode"] = json!("new_only");
                    } else if jt == "ra_badges" {
                        target["scope"] = json!(if mscope() == "owned" { "owned" } else { "played" });
                    } else if jt == "ai_batch_rate" {
                        let Ok(set) = ai_set().parse::<u64>() else { ui::toast(state::tr("choose a set"), true); return; };
                        target["set_id"] = json!(set);
                        target["lang"] = json!(state::ui_language());
                        if let Ok(max) = ai_max().trim().parse::<u32>() { if max > 0 { target["max"] = json!(max); } }
                    } else if jt == "media_backfill" {
                        target["scope"] = json!(mscope());
                        target["platform"] = platform().parse::<u64>().map(|p| json!(p)).unwrap_or(Value::Null);
                    }
                    let t = trigger();
                    let spec = json!({
                        "name": name(), "target": target, "mode": mode(), "trigger": t, "enabled": enabled(), "when_idle": when_idle(),
                        "fixed_every_n": if t != "check" { json!(fixed_n().parse::<u64>().unwrap_or(1)) } else { Value::Null },
                        "fixed_unit": if t != "check" { json!(fixed_u()) } else { Value::Null },
                        "check_every_n": if t != "fixed" { json!(check_n().parse::<u64>().unwrap_or(1)) } else { Value::Null },
                        "check_unit": if t != "fixed" { json!(check_u()) } else { Value::Null },
                    });
                    let id = edit_id();
                    let mut reset = reset.clone();
                    spawn(async move {
                        let r = match id { Some(id) => api::put(&format!("/api/v1/schedules/{id}"), spec).await, None => api::post("/api/v1/schedules", spec).await };
                        match r { Ok(_) => { ui::toast(state::tr("saved"), false); reset(); data.restart(); } Err(e) => ui::toast(e.message, true) }
                    });
                },
                label { {state::tr("name")} input { r#type: "text", required: true, style: "min-width:180px", value: "{name}", oninput: move |e| name.set(e.value()) } }
                label { {state::tr("job")}
                    select { onchange: move |e| job.set(e.value()),
                        for (v, l) in [("provider_bulk_sync", "provider bulk sync"), ("dat_sync", "DAT sync"), ("db_optimize", "DB optimize"), ("integrity_check", "integrity check"), ("file_scan", "file quick scan"), ("media_backfill", "download missing media"), ("ra_badges", "RA badges download/update"), ("ai_batch_rate", "batch rate with AI")] { option { value: "{v}", selected: v == jt, {state::tr(l)} } }
                    }
                }
                if jt == "provider_bulk_sync" {
                    label { {state::tr("provider")} select { onchange: move |e| provider.set(e.value()), for p in bulk.iter() { option { selected: s(p, "name") == prov_name, "{s(p, \"name\")}" } } } }
                    label { {state::tr("scope")} select { onchange: move |e| scope.set(e.value()), for (v, l) in scopes.clone() { option { value: "{v}", selected: v == sc, {state::tr(l)} } } } }
                    if sc == "platform" {
                        label { {state::tr("platform")}
                            select { onchange: move |e| platform.set(e.value()),
                                option { value: "", {state::tr("— choose —")} }
                                for p in state::platforms().into_iter().filter(|p| only.as_ref().is_none_or(|o| o.contains(&u(p, "id")))) { option { value: "{u(&p, \"id\")}", selected: u(&p, "id").to_string() == platform(), "{s(&p, \"name\")}" } }
                            }
                        }
                    }
                }
                if jt == "file_scan" {
                    label { {state::tr("directory")}
                        select { onchange: move |e| directory.set(e.value()),
                            option { value: "", {state::tr("— choose —")} }
                            for d in dirs.iter() { option { value: "{u(d, \"id\")}", selected: u(d, "id").to_string() == directory(), "{s(d, \"path\")}" } }
                        }
                    }
                }
                if jt == "ra_badges" {
                    label { {state::tr("RetroAchievements games")}
                        select { onchange: move |e| mscope.set(e.value()),
                            option { value: "played", selected: mscope() != "owned", {state::tr("played by the account")} }
                            option { value: "owned", selected: mscope() == "owned", {state::tr("owned games")} }
                        }
                    }
                }
                if jt == "ai_batch_rate" {
                    label { {state::tr("set")}
                        select { onchange: move |e| ai_set.set(e.value()),
                            option { value: "", {state::tr("— choose —")} }
                            for x in sets.iter() { option { value: "{u(x, \"id\")}", selected: u(x, "id").to_string() == ai_set(), "{s(x, \"name\")}" } }
                        }
                    }
                    label { {state::tr("at most")} input { r#type: "number", min: "1", style: "width:72px", placeholder: state::tr("all"), value: "{ai_max}", oninput: move |e| ai_max.set(e.value()) } }
                }
                if jt == "media_backfill" {
                    label { {state::tr("media for")}
                        select { onchange: move |e| mscope.set(e.value()),
                            option { value: "owned", selected: mscope() == "owned", {state::tr("owned games")} }
                            option { value: "all", selected: mscope() == "all", {state::tr("the whole catalogue")} }
                        }
                    }
                    label { {state::tr("platform")}
                        select { onchange: move |e| platform.set(e.value()),
                            option { value: "", {state::tr("all platforms")} }
                            for p in state::platforms() { option { value: "{u(&p, \"id\")}", selected: u(&p, "id").to_string() == platform(), "{s(&p, \"name\")}" } }
                        }
                    }
                }
                if jt == "dat_sync" {
                    label { {state::tr("source")} select { onchange: move |e| source.set(e.value()), option { value: "", {state::tr("all sources")} } for x in dat_sources { option { selected: x == source(), "{x}" } } } }
                }
                label { {state::tr("mode")} select { onchange: move |e| mode.set(e.value()), option { value: "diff", selected: mode() == "diff", {state::tr("diff")} } option { value: "redownload", selected: mode() == "redownload", {state::tr("redownload")} } } }
                label { {state::tr("trigger")} select { onchange: move |e| trigger.set(e.value()), for v in ["fixed", "check", "both"] { option { value: "{v}", selected: v == tr, {state::tr(v)} } } } }
                if tr != "check" {
                    label { {state::tr("fixed every")}
                        span { style: "display:flex;gap:4px",
                            input { r#type: "number", min: "1", style: "width:64px", value: "{fixed_n}", oninput: move |e| fixed_n.set(e.value()) }
                            select { onchange: move |e| fixed_u.set(e.value()), for x in UNITS { option { value: "{x}", selected: x == fixed_u(), {state::tr(x)} } } }
                        }
                    }
                }
                if tr != "fixed" {
                    label { {state::tr("check every")}
                        span { style: "display:flex;gap:4px",
                            input { r#type: "number", min: "1", style: "width:64px", value: "{check_n}", oninput: move |e| check_n.set(e.value()) }
                            select { onchange: move |e| check_u.set(e.value()), for x in UNITS { option { value: "{x}", selected: x == check_u(), {state::tr(x)} } } }
                        }
                    }
                }
                label { span { {state::tr("enabled")} } input { r#type: "checkbox", checked: enabled(), onchange: move |e| enabled.set(e.checked()) } }
                label { title: state::tr("A due run waits until no other job is queued or running."), span { {state::tr("run only when idle")} } input { r#type: "checkbox", checked: when_idle(), onchange: move |e| when_idle.set(e.checked()) } }
                button { class: "btn primary", {state::tr("save")} }
                button { class: "btn", r#type: "button", onclick: move |_| reset2(), {state::tr("clear")} }
            }
            if jt == "integrity_check" {
                Explain { {state::tr("Reads held files back and compares one quick hash. Worth scheduling: it is read-only, and running it often and briefly sweeps the collection oldest-first. Set the size of a run under Jobs.")} }
            }
            if jt == "ai_batch_rate" {
                p { class: "notice", {state::tr("Each run asks only about games of the set without a saved answer, one paid question each, billed by the provider to your own API key. Use a dedicated project (or workspace) at the provider with a budget or spending limit (Config › AI has the links).")} }
            }
            if jt == "db_optimize" {
                p { class: "bad", {state::tr("Scheduling this is almost never right: it repeatedly destroys the records that let moved and renamed files be recognised, and the \"missing, but found elsewhere\" hint with them. Prefer running it by hand when the database is actually too large.")} }
            }
        }
        div { class: "card",
            h2 { {state::trf("Schedules ({n})", &[("n", &list.len().to_string())])} }
            table { class: "grid",
                thead { tr { th { {state::tr("name")} } th { {state::tr("job")} } th { {state::tr("mode")} } th { {state::tr("trigger")} } th { {state::tr("next fixed")} } th { {state::tr("next check")} } th { {state::tr("last run")} } th { {state::tr("last check")} } th {} } }
                tbody {
                    if list.is_empty() { tr { td { colspan: "9", class: "muted", {state::tr("none yet")} } } }
                    for x in list.clone() {
                        {
                            let id = u(&x, "id");
                            let t = x.get("target").cloned().unwrap_or_default();
                            let whens = [when(api::uo(&x, "fixed_every_n"), s(&x, "fixed_unit")), when(api::uo(&x, "check_every_n"), s(&x, "check_unit"))].into_iter().filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" / ");
                            let x2 = x.clone();
                            rsx! {
                                tr {
                                    td { "{s(&x, \"name\")} " if !api::b(&x, "enabled") { Html { html: badge(&state::tr("disabled"), "dim") } } if api::b(&x, "when_idle") { " " Html { html: badge(&state::tr("when idle"), "dim") } } }
                                    td { "{describe(&t)}" }
                                    td { "{s(&x, \"mode\")}" }
                                    td { "{s(&x, \"trigger\")} " span { class: "muted", "{whens}" } }
                                    td { "{html::fmt_date(&s(&x, \"next_fixed_at\"))}" }
                                    td { "{html::fmt_date(&s(&x, \"next_check_at\"))}" }
                                    td { "{html::fmt_date(&s(&x, \"last_run_at\"))} " if let Some(j) = api::uo(&x, "last_job_id") { span { class: "muted", "#{j}" } } }
                                    td { class: "muted", style: "white-space:normal;max-width:260px", "{html::fmt_date(&s(&x, \"last_check_at\"))} {s(&x, \"last_check_result\")}" }
                                    td {
                                        button { class: "btn small", onclick: move |_| { spawn(async move { match api::post(&format!("/api/v1/schedules/{id}/run"), json!({})).await { Ok(r) => ui::toast(if api::b(&r, "already_queued") { state::tr("already queued") } else { state::trf("enqueued job #{id}", &[("id", &u(&r, "job_id").to_string())]) }, false), Err(e) => ui::toast(e.message, true) } }); }, {state::tr("run now")} }
                                        " "
                                        button { class: "btn small",
                                            onclick: move |_| {
                                                let x = &x2;
                                                let t = x.get("target").cloned().unwrap_or_default();
                                                edit_id.set(Some(id));
                                                name.set(s(x, "name"));
                                                job.set(s(&t, "job_type"));
                                                mode.set(s(x, "mode"));
                                                trigger.set(s(x, "trigger"));
                                                enabled.set(api::b(x, "enabled"));
                                                when_idle.set(x.get("when_idle").and_then(|v| v.as_bool()).unwrap_or(true));
                                                if let Some(dir) = api::uo(&t, "directory_id") { directory.set(dir.to_string()); }
                                                if let Some(p) = api::so(&t, "provider") { provider.set(p); }
                                                scope.set(if api::b(&t, "full") { "full".into() } else if matches!(t.get("platform"), Some(Value::Number(_))) { "platform".into() } else { "all".into() });
                                                if let Some(Value::Number(n)) = t.get("platform") { platform.set(n.to_string()); }
                                                source.set(s(&t, "source"));
                                                if s(&t, "job_type") == "ra_badges" { mscope.set(if s(&t, "scope") == "owned" { "owned".into() } else { "played".into() }); }
                                                if s(&t, "job_type") == "ai_batch_rate" { ai_set.set(api::uo(&t, "set_id").map(|v| v.to_string()).unwrap_or_default()); ai_max.set(api::uo(&t, "max").map(|v| v.to_string()).unwrap_or_default()); }
                                                if s(&t, "job_type") == "media_backfill" { mscope.set(if s(&t, "scope") == "all" { "all".into() } else { "owned".into() }); if t.get("platform").map_or(true, Value::is_null) { platform.set(String::new()); } }
                                                if let Some(n) = api::uo(x, "fixed_every_n") { fixed_n.set(n.to_string()); fixed_u.set(s(x, "fixed_unit")); }
                                                if let Some(n) = api::uo(x, "check_every_n") { check_n.set(n.to_string()); check_u.set(s(x, "check_unit")); }
                                                let _ = document::eval("window.scrollTo(0, 0)");
                                            },
                                            {state::tr("edit")}
                                        }
                                        " "
                                        button { class: "btn small danger",
                                            onclick: move |_| {
                                                spawn(async move {
                                                    if !ui::confirm(&state::tr("Delete this schedule? (jobs it already queued stay)")).await { return; }
                                                    match api::delete(&format!("/api/v1/schedules/{id}")).await { Ok(_) => data.restart(), Err(e) => ui::toast(e.message, true) }
                                                });
                                            },
                                            {state::tr("delete")}
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
}

// --- server ---

#[component]
pub fn ConfigServer() -> Element {
    let mut data = use_resource(|| async { api::get("/api/v1/server").await });
    let mut state_text = use_signal(String::new);
    let st = match &*data.read() {
        None => return rsx! { div { class: "muted", {state::tr("loading…")} } },
        Some(Err(e)) => return rsx! { div { class: "card bad", "{e.message}" } },
        Some(Ok(v)) => v.clone(),
    };
    let pid = u(&st, "pid");
    let started = s(&st, "started_at");
    rsx! {
        div { class: "card",
            h2 { {state::tr("Server")} }
            div { class: "kv",
                div { {state::tr("process")} } div { {state::trf("pid {pid}, version {version}", &[("pid", &pid.to_string()), ("version", &s(&st, "version"))])} }
                div { {state::tr("running since")} } div { "{html::fmt_date(&s(&st, \"started_at\"))}" }
                div { {state::tr("background worker")} } div { Html { html: if api::b(&st, "worker_running") { badge(&state::tr("running in this process"), "ok") } else { badge(&state::tr("not in this process (--no-worker)"), "warn") } } }
                // What a new device is asked to compare on its first connection.
                div { {state::tr("TLS certificate")} } div {
                    if s(&st, "tls_fingerprint").is_empty() {
                        span { class: "muted", {state::tr("none (this app's own database, or a server started with --no-tls)")} }
                    } else {
                        span { class: "mono", style: "word-break:break-all", {s(&st, "tls_fingerprint").as_bytes().chunks(2).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join(":")} }
                        div { class: "muted", {state::tr("SHA-256. A device connecting for the first time shows this: it should match.")} }
                    }
                }
            }
            Explain { {state::tr("The server and its worker keep running with no app or browser open; queued jobs carry on. Only these buttons (or Ctrl+C where it was started) stop it. Jobs still running are put back in the queue and start over on the next start.")} }
            div { class: "toolbar",
                button { class: "btn",
                    onclick: move |_| {
                        let started = started.clone();
                        spawn(async move {
                            if !ui::confirm(&state::tr("Restart the server? Running jobs are re-queued and start over.")).await { return; }
                            if let Err(e) = api::post("/api/v1/server/restart", json!({})).await { ui::toast(e.message, true); return; }
                            state_text.set(state::tr("restarting…"));
                            // Wait for the old process to go and a new one to answer: another pid,
                            // or the same pid started again later (a container's server is pid 1
                            // every time).
                            for i in 0..120 {
                                ui::sleep(1000).await;
                                if let Ok(now) = api::get("/api/v1/server").await {
                                    if u(&now, "pid") != pid || s(&now, "started_at") != started { ui::toast(state::tr("server restarted"), false); state_text.set(String::new()); data.restart(); return; }
                                }
                                state_text.set(state::trf("restarting… ({s}s)", &[("s", &(i + 1).to_string())]));
                            }
                            state_text.set(state::tr("the server hasn't come back after 2 minutes — check the console it was started from"));
                        });
                    },
                    {state::tr("restart server")}
                }
                button { class: "btn danger",
                    onclick: move |_| {
                        spawn(async move {
                            if !ui::confirm(&state::tr("Shut the server down? The app stops working until it is started again; running jobs are re-queued.")).await { return; }
                            if let Err(e) = api::post("/api/v1/server/shutdown", json!({})).await { ui::toast(e.message, true); return; }
                            state_text.set(state::tr("shutting down — start romexplorer_server again (the desktop app starts it when it opens)."));
                        });
                    },
                    {state::tr("shut down server")}
                }
            }
            div { class: "muted", "{state_text}" }
        }
        PortsCard { db_port: u(&st, "db_port"), server_port: u(&st, "server_port") }
        DatabaseCacheCard {}
    }
}

/// How much memory MariaDB keeps the catalogue in (a share of RAM or a fixed size), and
/// whether it is read in at start. Saved to `data/database.env`; the size applies from the
/// next start.
#[component]
fn DatabaseCacheCard() -> Element {
    let mut data = use_resource(|| async { api::get("/api/v1/server/database_cache").await });
    let c = match &*data.read() {
        None => return rsx! { div { class: "card", h2 { {state::tr("Database cache")} } div { class: "muted", {state::tr("loading…")} } } },
        Some(Err(e)) => return rsx! { div { class: "card", h2 { {state::tr("Database cache")} } div { class: "bad", "{e.message}" } } },
        Some(Ok(v)) => v.clone(),
    };
    rsx! { DatabaseCacheForm { c, on_saved: move |_| data.restart() } }
}

#[component]
fn DatabaseCacheForm(c: Value, on_saved: EventHandler<()>) -> Element {
    const GIB: f64 = (1u64 << 30) as f64;
    let ram = u(&c, "total_ram_bytes") as f64;
    let mut fixed_mode = use_signal(|| s(&c, "mode") == "fixed");
    let mut percent = use_signal(|| c["percent"].as_u64().unwrap_or(25).to_string());
    let mut fixed = use_signal(|| {
        let b = c["fixed_bytes"].as_u64().unwrap_or(u(&c, "next_start_bytes")) as f64;
        format!("{}", ((b / GIB) * 1000.0).round() / 1000.0)
    });
    let mut warm = use_signal(|| api::b(&c, "warm_cache"));
    let mut fast = use_signal(|| api::b(&c, "fast_writes"));
    let mut disk = use_signal(|| s(&c, "disk"));
    let local = api::b(&c, "local");
    let detected = if s(&c, "disk_detected") == "ssd" { "SSD".to_string() } else { state::tr("hard disk") };
    let writing_now = match c["live_flush_at_commit"].as_u64() {
        None => "—".to_string(),
        Some(flush) => {
            let mut t = state::tr(if flush == 1 { "flush at every commit" } else { "flush about once a second" });
            if let Some(n) = c["live_io_capacity"].as_u64() {
                t += &format!(" · {}", state::trf("{n} background writes per second", &[("n", &n.to_string())]));
            }
            t
        }
    };
    // (stored spelling, bytes) for what the form says now.
    let chosen = move || -> (String, f64) {
        if fixed_mode() {
            let g: f64 = fixed().parse().unwrap_or(0.0);
            (format!("{}M", (g * 1024.0).round() as u64), g * GIB)
        } else {
            let p: f64 = percent().parse().unwrap_or(0.0);
            (format!("{}%", p as u64), ram * p / 100.0)
        }
    };
    let (_, next) = chosen();
    let calc = state::trf("next start: {size} of {ram}", &[("size", &html::fmt_size(Some(next.min(ram * 0.9) as u64))), ("ram", &html::fmt_size(Some(ram as u64)))])
        + if next > ram * 0.9 { " (capped at 90% of memory)" } else { "" };
    let w = &c["warm"];
    let warm_text = match s(w, "state").as_str() {
        "idle" => state::tr("not run in this process"),
        "waiting" => state::tr("waiting for MariaDB's own reload to finish"),
        "running" => state::trf("reading… {done} of {total} tables ({bytes})", &[("done", &u(w, "tables_done").to_string()), ("total", &u(w, "tables_total").to_string()), ("bytes", &html::fmt_size(Some(u(w, "bytes_done"))))]),
        "done" => {
            let mut t = state::trf("{n} tables, {bytes} in {s}s", &[("n", &u(w, "tables_done").to_string()), ("bytes", &html::fmt_size(Some(u(w, "bytes_done")))), ("s", &format!("{:.1}", w["seconds"].as_f64().unwrap_or(0.0)))]);
            if u(w, "tables_skipped") > 0 {
                t += &format!(" — {}", state::trf("{n} left out: the cache is smaller than the catalogue", &[("n", &u(w, "tables_skipped").to_string())]));
            }
            t
        }
        other => format!("{other}{}", w["message"].as_str().map(|m| format!(" — {m}")).unwrap_or_default()),
    };
    let live = c["live_bytes"].as_u64();
    let catalogue = c["catalogue_bytes"].as_u64();
    let too_big = matches!((catalogue, live), (Some(cat), Some(l)) if cat as f64 > l as f64 * 0.9);
    rsx! {
        div { class: "card",
            h2 { {state::tr("Database cache")} }
            Explain { {state::tr("MariaDB keeps tables and indexes in memory so screens are not read from disk each time. Memory is only used as the catalogue fills it, so a generous size costs nothing until it is needed. Saved for this installation and used from the next start.")} }
            if !api::b(&c, "local") {
                div { class: "warn", {state::tr("This database runs on another machine; these settings only apply to a database this installation starts itself.")} }
            }
            label { style: "display:block;margin:4px 0",
                input { r#type: "radio", name: "dbcache-mode", checked: !fixed_mode(), onchange: move |_| fixed_mode.set(false) }
                " " {state::tr("share of memory")} " "
                input { r#type: "number", min: "{u(&c, \"min_percent\")}", max: "{u(&c, \"max_percent\")}", style: "width:70px", value: "{percent}", oninput: move |e| { percent.set(e.value()); fixed_mode.set(false); } }
                " %"
            }
            label { style: "display:block;margin:4px 0",
                input { r#type: "radio", name: "dbcache-mode", checked: fixed_mode(), onchange: move |_| fixed_mode.set(true) }
                " " {state::tr("fixed size")} " "
                input { r#type: "number", min: "0.125", step: "0.125", style: "width:90px", value: "{fixed}", oninput: move |e| { fixed.set(e.value()); fixed_mode.set(true); } }
                " GiB"
            }
            div { class: "muted", "{calc}" }
            label { style: "display:block;margin:8px 0",
                input { r#type: "checkbox", checked: warm(), onchange: move |e| warm.set(e.checked()) }
                " " {state::tr("read the catalogue into the cache when the database starts (in the background)")}
            }
            h3 { style: "margin-top:12px", {state::tr("Writing")} }
            label { style: "display:block;margin:4px 0",
                input { r#type: "checkbox", checked: fast(), disabled: !local, onchange: move |e| fast.set(e.checked()) }
                " " {state::tr("fast writes: flush to disk about once a second instead of at every commit")}
            }
            div { class: "muted", {state::tr("Much faster scans and imports. A crash of the app or the database loses nothing; a power cut or an operating system crash can lose up to the last second of changes. Off: every change waits for the disk.")} }
            label { style: "display:block;margin:8px 0",
                {state::tr("database disk")} " "
                select { disabled: !local, onchange: move |e| disk.set(e.value()),
                    option { value: "auto", selected: disk() == "auto", {state::trf("auto (detected: {kind})", &[("kind", &detected)])} }
                    option { value: "ssd", selected: disk() == "ssd", "SSD" }
                    option { value: "nvme", selected: disk() == "nvme", "NVMe SSD" }
                    option { value: "hdd", selected: disk() == "hdd", {state::tr("hard disk")} }
                }
            }
            div { class: "muted", {state::tr("How hard the database writes in the background. A hard disk setting on an SSD holds heavy writes back; an SSD setting on a hard disk makes everything else wait.")} }
            div { class: "toolbar",
                button { class: "btn primary",
                    onclick: move |_| {
                        let mut body = json!({ "buffer_pool": chosen().0, "warm_cache": warm() });
                        if local {
                            body["fast_writes"] = json!(fast());
                            body["disk"] = json!(disk());
                        }
                        spawn(async move {
                            match api::put("/api/v1/server/database_cache", body).await {
                                Ok(_) => { ui::toast(state::tr("saved — used from the next start"), false); on_saved.call(()); }
                                Err(e) => ui::toast(e.message, true),
                            }
                        });
                    },
                    {state::tr("save")}
                }
            }
            p { class: "muted",
                {state::tr("The writing settings apply as soon as they are saved.")} " "
                if api::b(&c, "in_process") {
                    {state::tr("Both settings take effect only when the database restarts: quit and reopen the app.")}
                } else {
                    {state::tr("Both settings take effect only when the database restarts: use \"restart server\" above, which restarts the database it started along with itself.")}
                }
            }
            if api::b(&c, "restart_needed") {
                div { class: "warn",
                    {state::trf("Restart needed: the cache is {now} now and will be {next} after a restart.", &[("now", &html::fmt_size(live)), ("next", &html::fmt_size(Some(u(&c, "next_start_bytes"))))])}
                }
            }
            div { class: "kv",
                div { {state::tr("memory")} } div { "{html::fmt_size(Some(ram as u64))}" }
                div { {state::tr("cache now")} }
                div {
                    {live.map(|l| html::fmt_size(Some(l))).unwrap_or_else(|| "—".into())}
                    if let Some(d) = c["cached_bytes"].as_u64() { {format!(" · {}", state::trf("{n} holding data", &[("n", &html::fmt_size(Some(d)))]))} }
                }
                div { {state::tr("catalogue on disk")} }
                div {
                    {catalogue.map(|b| html::fmt_size(Some(b))).unwrap_or_else(|| "—".into())}
                    if too_big { " " Html { html: badge(&state::tr("bigger than the cache"), "warn") } }
                }
                div { {state::tr("reload at start")} } div { {c["load_status"].as_str().unwrap_or("—").to_string()} }
                div { {state::tr("warm-up")} } div { "{warm_text}" }
            }
        }
    }
}

/// The database and server ports, saved to `data/database.env` and used from the next start.
#[component]
fn PortsCard(db_port: u64, server_port: u64) -> Element {
    let mut db = use_signal(|| db_port.to_string());
    let mut server = use_signal(|| server_port.to_string());
    rsx! {
        div { class: "card",
            h2 { {state::tr("Ports")} }
            Explain { {state::tr("Saved for this installation and used from the next start. Each installation on a machine needs its own database port; one that finds its port taken by another installation moves to the next free one by itself and records it here.")} }
            form { class: "inline",
                onsubmit: move |e| {
                    e.prevent_default();
                    let body = json!({ "db_port": db().parse::<u16>().ok(), "server_port": server().parse::<u16>().ok() });
                    spawn(async move {
                        match api::put("/api/v1/server/ports", body).await {
                            Ok(_) => ui::toast(state::tr("saved — used from the next start"), false),
                            Err(e) => ui::toast(e.message, true),
                        }
                    });
                },
                label { {state::tr("database port")} input { r#type: "number", min: "1024", max: "65535", style: "width:110px", value: "{db}", oninput: move |e| db.set(e.value()) } }
                label { {state::tr("server port")} input { r#type: "number", min: "1024", max: "65535", style: "width:110px", value: "{server}", oninput: move |e| server.set(e.value()) } }
                button { class: "btn primary", {state::tr("save")} }
            }
        }
    }
}
