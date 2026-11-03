//! Detail-page pieces shared by several screens, ported from the console: media slots, the
//! manual-match dialog, the RetroAchievements card, file columns and the archive hover, the
//! page-open metadata refresh, the base-game filter and the title merge tools.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use dioxus::prelude::*;
use serde_json::{json, Value};

use super::common::CellLink;
use crate::api::{self, s, u};
use crate::components::Column;
use crate::html::{self, badge, esc};
use crate::state;
use crate::ui::{self, Html};
use crate::Route;

// --- page refresh: opening a title / release page queues its fetches at high priority ---

thread_local! {
    static REFRESHED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    static FILE_DETAIL: RefCell<HashMap<u64, Value>> = RefCell::new(HashMap::new());
}

pub async fn page_refresh(kind: &str, id: u64, loud: bool) {
    let key = format!("{kind}:{id}");
    if !loud && REFRESHED.with(|r| r.borrow().contains(&key)) {
        return;
    }
    REFRESHED.with(|r| r.borrow_mut().insert(key));
    match api::post(&format!("/api/v1/catalog/{kind}/{id}/refresh"), json!({})).await {
        Ok(r) => {
            let jobs = api::arr(&r, "jobs").len();
            if loud || jobs > 0 {
                let already: Vec<String> = api::arr(&r, "already_queued").iter().filter_map(|x| x.as_str().map(str::to_string)).collect();
                ui::toast(
                    if jobs > 0 {
                        state::trf("refresh queued: {n} job(s) at high priority — reload in a bit", &[("n", &jobs.to_string())])
                    } else {
                        format!("{}{}", state::tr("nothing to refresh"), if already.is_empty() { String::new() } else { format!(" {}", state::trf("(already queued: {list})", &[("list", &already.join(", "))])) })
                    },
                    false,
                );
            }
        }
        Err(e) => {
            if loud {
                ui::toast(e.message, true)
            }
        }
    }
}

// --- files ---

pub async fn file_detail(id: u64) -> Option<Value> {
    if let Some(v) = FILE_DETAIL.with(|c| c.borrow().get(&id).cloned()) {
        return Some(v);
    }
    let v = api::get(&format!("/api/v1/catalog/files/{id}")).await.ok()?;
    FILE_DETAIL.with(|c| c.borrow_mut().insert(id, v.clone()));
    Some(v)
}

/// An archive / container file's members, for the row hover.
pub async fn archive_hover(row: Value) -> Option<String> {
    let fmt = s(&row, "archive_format");
    let members = u(&row, "member_count");
    if fmt.is_empty() && members == 0 {
        return None;
    }
    if fmt == "raw_iso" && members == 0 {
        return None;
    }
    let d = file_detail(u(&row, "id")).await?;
    let list = api::arr(&d, "members");
    let label = if fmt.is_empty() { state::tr("container") } else { fmt.clone() };
    if list.is_empty() {
        return Some(format!("<b>{}</b> — {}", esc(&label), state::tr("no member content recorded yet (a rehash records them)")));
    }
    let contents = state::tr("contents");
    Some(format!(
        "<b>{}</b> · {}<table class=\"grid\">{}</table>",
        esc(if fmt.is_empty() { &contents } else { &fmt }),
        state::trf("{n} member(s)", &[("n", &list.len().to_string())]),
        list.iter()
            .map(|m| format!(
                "<tr><td class=\"mono\">{}</td><td class=\"num\">{}</td><td>{}</td></tr>",
                super::common::member_name(m),
                html::fmt_size(api::uo(m, "size")),
                html::hashes_html(m.get("hashes"))
            ))
            .collect::<String>()
    ))
}

pub fn file_columns() -> Vec<Column> {
    vec![
        Column::render("path", "path", |r| rsx! { CellLink { to: Route::File { id: u(r, "id") }, text: s(r, "path") } }).sort("path"),
        Column::html("size", "size", |r| html::fmt_size(api::uo(r, "size"))).sort("size").num(),
        Column::html("archive_format", "format", |r| {
            let f = s(r, "archive_format");
            if f.is_empty() {
                let file = state::tr("file");
                html::cbadge("archive_format", "unknown", Some(&file), None)
            } else {
                let n = u(r, "member_count");
                html::cbadge("archive_format", &f, Some(&format!("{f}{}", if n > 0 { format!(" ×{n}") } else { String::new() })), None)
            }
        })
        .sort("archive_format"),
        Column::html("platform", "platform", |r| html::cbadge("platform", &s(r, "platform"), None, None)).sort("platform"),
        Column::html("hashes", "hashes", |r| html::hashes_html(r.get("hashes"))),
        Column::render("deleted", "state", |r| {
            if api::b(r, "deleted") {
                let found = api::uo(r, "found_elsewhere");
                rsx! {
                    if let Some(f) = found {
                        span { class: "link", onclick: move |e| { e.stop_propagation(); navigator().push(Route::File { id: f }); },
                            Html { html: html::hint_icon(&state::trf("missing file found: the same bytes are held by file #{id} (click to open)", &[("id", &f.to_string())])) }
                        }
                    }
                    Html { html: badge(&state::tr("missing"), "bad") }
                }
            } else {
                rsx! {
                    Html { html: badge(&state::tr("present"), "ok") }
                    // Only a sidecar renders anything; every other row is unchanged.
                    SheetCell { row: r.clone() }
                }
            }
        }),
    ]
}

/// The verdict badge and the two buttons a `.cue` / `.gdi` / `.m3u` line carries.
///
/// A component rather than a plain function, and that is load-bearing: `Column::render`'s
/// closure is invoked inline inside `Grid`'s own `rsx!`, so a `use_signal` written directly in
/// it would allocate in *Grid's* hook scope, once per row -- and the count would change with
/// filtering and paging while the order changed with sorting, so each row's state would drift
/// onto a different row. A component gets a scope of its own, keyed by its position, which is
/// what makes per-row state safe here at all.
#[component]
fn SheetCell(row: Value) -> Element {
    let path = s(&row, "path");
    let is_sidecar = std::path::Path::new(&path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "cue" | "gdi" | "m3u"));
    let id = u(&row, "id");
    let stored = row
        .get("sheet_check")
        .filter(|v| !v.is_null())
        .and_then(|v| serde_json::from_value::<romexplorer_types::sheets::SheetVerdictView>(v.clone()).ok());
    // Unconditional, before any early return: hooks may not be skipped.
    let mut live = use_signal(|| None::<romexplorer_types::sheets::SheetVerdictView>);
    let mut busy = use_signal(|| false);
    if !is_sidecar {
        return rsx! {};
    }

    let current = live().or(stored);
    let fixable = current.as_ref().is_some_and(crate::screens::sheets::offerable);
    let title = current.as_ref().map(|v| v.problems.join("\n")).unwrap_or_default();
    rsx! {
        if let Some(v) = current.clone() {
            " "
            span { title: "{title}", Html { html: badge(&state::tr(&v.label()), v.tone()) } }
        } else {
            " " Html { html: badge(&state::tr("not checked"), "dim") }
        }
        " "
        button {
            class: "btn small",
            r#type: "button",
            disabled: busy(),
            title: state::tr("read it back now and say whether it still works"),
            onclick: move |e: Event<MouseData>| {
                // The row itself is a link; checking a file is not opening it.
                e.stop_propagation();
                busy.set(true);
                spawn(async move {
                    match api::post_as::<(), romexplorer_types::sheets::SheetVerdictView>(&format!("/api/v1/sheets/files/{id}/verify"), &()).await {
                        Ok(v) => live.set(Some(v)),
                        Err(e) => ui::toast(e.message, true),
                    }
                    busy.set(false);
                });
            },
            {state::tr("check")}
        }
        // Only where it could help, so the button is absent rather than present and always
        // refusing.
        if fixable {
            " "
            button {
                class: "btn small",
                r#type: "button",
                disabled: busy(),
                title: state::tr("try every legal way of writing this sheet and keep the one that matches the DAT"),
                onclick: move |e: Event<MouseData>| {
                    e.stop_propagation();
                    spawn(async move { crate::screens::sheets::fix_dialog(id, live).await });
                },
                {state::tr("fix hash")}
            }
        }
    }
}

/// A static list of rows as a bare grid (no toolbar / pager).
pub fn static_fetch(rows: Vec<Value>) -> impl Fn(crate::components::Query) -> std::future::Ready<api::ApiResult> + 'static {
    move |_| {
        let n = rows.len();
        std::future::ready(Ok(json!({ "items": rows.clone(), "total": n })))
    }
}

// --- the RetroAchievements card on title / release pages ---

#[component]
pub fn RaCard(links: Vec<Value>) -> Element {
    let mut ids: Vec<String> = Vec::new();
    for l in &links {
        let id = s(l, "provider_id");
        // RA-verified only: RetroAchievements' own hash list, or a link the user made. A name
        // guess may be a set this release does not run with.
        let verified = matches!(s(l, "confidence").as_str(), "exact" | "manual");
        if s(l, "provider") == "retroachievements" && verified && !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) && !ids.contains(&id) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "card",
            h2 { "RetroAchievements" }
            for i in ids {
                div { style: "margin-bottom:10px",
                    span { class: "link", onclick: { let i = i.clone(); move |_| { navigator().push(Route::RaGame { id: i.parse().unwrap_or(0) }); } }, {state::trf("RA game {id}: achievements →", &[("id", &i)])} }
                    br {}
                    img { class: "mosaic", loading: "lazy", src: api::media_url(&format!("/api/v1/ra/games/{i}/badges")), alt: "" }
                }
            }
        }
    }
}

// --- provider links table (Web pages card) ---

pub fn link_url_cell(p: &Value) -> Element {
    let url = s(p, "url");
    let id = s(p, "provider_id");
    if url.is_empty() {
        rsx! { span { class: "muted", "#{id}" } }
    } else {
        rsx! { a { class: "link", href: "{url}", target: "_blank", rel: "noopener", "{url}" } }
    }
}

// --- media slots ---

const SUBS: [&str; 8] = ["", "front", "back", "top", "bottom", "left", "right", "spine"];

/// `kind`: "titles" | "releases".
#[component]
pub fn MediaSlots(kind: String, id: u64, list: Vec<Value>, on_change: EventHandler<()>) -> Element {
    let key = if kind == "titles" { "title_id" } else { "release_id" };
    let mut media_type = use_signal(|| "box".to_string());
    let mut sub = use_signal(|| "front".to_string());
    let mut url = use_signal(String::new);
    let mut file = use_signal(|| None::<dioxus::html::FileData>);
    let types: Vec<String> = api::arr(&state::vocab(), "media_types").iter().filter_map(|t| t.as_str().map(str::to_string)).collect();
    let done = move |r: Value| {
        let linked = api::arr(&r, "linked_releases").len();
        ui::toast(
            format!(
                "{}{}{}",
                state::trf("media #{id} set", &[("id", &u(&r, "media_id").to_string())]),
                api::uo(&r, "replaced").map(|x| format!(" {}", state::trf("(replaced {id})", &[("id", &x.to_string())]))).unwrap_or_default(),
                if linked > 1 { format!(" — {}", state::trf("shared by {n} releases", &[("n", &linked.to_string())])) } else { String::new() }
            ),
            false,
        );
        on_change.call(());
    };
    let kind2 = kind.clone();
    rsx! {
        if list.is_empty() {
            span { class: "muted", {state::tr("none")} }
        } else {
            table { class: "grid",
                for m in list.clone() {
                    {
                        let mid = u(&m, "id");
                        let mt = s(&m, "media_type");
                        let is_img = !matches!(mt.as_str(), "video" | "bgm");
                        let sc = s(&m, "sub_category");
                        let path = s(&m, "path");
                        let fname = path.rsplit('/').next().unwrap_or("").to_string();
                        let kind = kind2.clone();
                        rsx! {
                            tr { key: "{mid}",
                                td {
                                    // A video or a track plays where it is listed, not in a new window.
                                    if is_img {
                                        img { src: api::thumb(mid), alt: "", style: "max-height:48px;max-width:64px" }
                                    } else if mt == "video" {
                                        video { controls: true, preload: "metadata", src: api::media(mid), style: "max-height:120px;max-width:213px;display:block" }
                                    } else {
                                        audio { controls: true, preload: "none", src: api::media(mid), style: "width:213px" }
                                    }
                                }
                                td { "{mt}" if !sc.is_empty() { "/{sc}" } }
                                td { if s(&m, "source") == "manual" { Html { html: badge("manual", "ok") } } else { "{s(&m, \"source\")}" } }
                                td { a { class: "link", href: api::media(mid), target: "_blank", "{fname}" } }
                                // Shown here for want of its own: nothing of this page's to unlink.
                                if let Some(via) = api::so(&m, "via") {
                                    td { Html { html: badge(&state::tr(if via == "title" { "from the title" } else { "from the game" }), "dim") } }
                                } else {
                                td { button { class: "btn small danger",
                                    onclick: move |_| {
                                        let kind = kind.clone();
                                        spawn(async move {
                                            let scope = if kind == "releases" { format!(" {}", state::tr("(and from this release's revisions)")) } else { String::new() };
                                            let q = state::trf("Unlink this media item here{scope}? The file itself is kept.", &[("scope", &scope)]);
                                            if !ui::confirm(&q).await { return; }
                                            match api::delete(&format!("/api/v1/media/{mid}{}", api::qs(&[(key, id.to_string())]))).await {
                                                Ok(_) => { ui::toast(state::tr("removed"), false); on_change.call(()); }
                                                Err(e) => ui::toast(e.message, true),
                                            }
                                        });
                                    },
                                    {state::tr("remove")}
                                } }
                                }
                            }
                        }
                    }
                }
            }
        }
        form { class: "inline", style: "margin-top:8px", onsubmit: move |e| e.prevent_default(),
            span { class: "muted", {state::tr("set slot:")} }
            select { onchange: move |e| media_type.set(e.value()),
                for t in types { option { selected: t == media_type(), "{t}" } }
            }
            select { onchange: move |e| sub.set(e.value()),
                for x in SUBS { option { value: "{x}", selected: x == sub(), if x.is_empty() { {state::tr("(none)")} } else { "{x}" } } }
            }
            input { r#type: "text", placeholder: state::tr("image/video/audio file URL"), style: "min-width:260px", value: "{url}", oninput: move |e| url.set(e.value()) }
            button { class: "btn small",
                onclick: move |_| {
                    let u2 = url().trim().to_string();
                    if u2.is_empty() { ui::toast(state::tr("paste a file URL"), true); return; }
                    let sc = sub();
                    let mut body = json!({ key: id, "media_type": media_type(), "sub_category": if sc.is_empty() { Value::Null } else { json!(sc) }, "url": u2 });
                    spawn(async move {
                        loop {
                            match api::post("/api/v1/media/manual", body.clone()).await {
                                Ok(r) => { url.set(String::new()); done(r); }
                                Err(e) if e.status == 409 && body.get("on_conflict").is_none() => match ui::media_on_conflict(&e.message).await {
                                    Some(choice) => { body["on_conflict"] = json!(choice); continue; }
                                    None => {}
                                },
                                Err(e) => ui::toast(e.message, true),
                            }
                            break;
                        }
                    });
                },
                {state::tr("set from URL")}
            }
            input { r#type: "file", onchange: move |e| file.set(e.files().into_iter().next()) }
            button { class: "btn small",
                onclick: move |_| {
                    let Some(f) = file() else { ui::toast(state::tr("choose a file"), true); return; };
                    let mut fields = vec![(key.to_string(), id.to_string()), ("media_type".into(), media_type()), ("sub_category".into(), sub())];
                    spawn(async move {
                        let bytes = match f.read_bytes().await { Ok(b) => b.to_vec(), Err(e) => { ui::toast(e.to_string(), true); return; } };
                        loop {
                            match api::upload("/api/v1/media/upload", fields.clone(), "file", f.name(), bytes.clone()).await {
                                Ok(r) => done(r),
                                Err(e) if e.status == 409 && !fields.iter().any(|(k, _)| k == "on_conflict") => match ui::media_on_conflict(&e.message).await {
                                    Some(choice) => { fields.push(("on_conflict".into(), choice.into())); continue; }
                                    None => {}
                                },
                                Err(e) => ui::toast(e.message, true),
                            }
                            break;
                        }
                    });
                },
                {state::tr("upload")}
            }
        }
    }
}

// --- manual match ---

/// `kind`: "titles" | "releases". `title_id`: for unlinking a release's via-title link.
pub fn match_dialog(kind: &'static str, id: u64, name: String, platform_id: Option<u64>, links: Vec<Value>, title_id: Option<u64>, on_done: EventHandler<()>) {
    ui::open_modal(move || rsx! { MatchDialog { kind, id, name: name.clone(), platform_id, links: links.clone(), title_id, on_done } });
}

#[component]
fn MatchDialog(kind: &'static str, id: u64, name: String, platform_id: Option<u64>, links: Vec<Value>, title_id: Option<u64>, on_done: EventHandler<()>) -> Element {
    let providers = use_resource(|| async { api::get("/api/v1/providers").await.map(|v| api::items(&v)).unwrap_or_default() });
    let mut current = use_signal(|| links.clone());
    let mut where_ = use_signal(|| "local".to_string());
    let mut provider = use_signal(String::new);
    let mut q = use_signal(|| name.clone());
    let mut platform = use_signal(|| platform_id.map(|p| p.to_string()).unwrap_or_default());
    let mut results = use_signal(|| None::<Result<Vec<Value>, String>>);
    let mut page_url = use_signal(String::new);
    let mut resolved = use_signal(|| None::<Value>);
    let mut id_provider = use_signal(String::new);
    let mut id_value = use_signal(String::new);
    let base = format!("/api/v1/catalog/{kind}/{id}/links");

    let plist = providers.read().clone().unwrap_or_default();
    let choices: Vec<String> = if where_() == "local" {
        plist.iter().map(|p| s(p, "name")).filter(|n| n != "hasheous" && n != "playmatch").collect()
    } else {
        plist.iter().filter(|p| api::b(p, "query_by_name") && api::b(p, "available")).map(|p| s(p, "name")).collect()
    };
    let chosen = if choices.contains(&provider()) { provider() } else { choices.first().cloned().unwrap_or_default() };
    let all_names: Vec<String> = plist.iter().map(|p| s(p, "name")).collect();
    let online_names: Vec<String> = plist.iter().filter(|p| api::b(p, "query_by_name") && api::b(p, "available")).map(|p| s(p, "name")).collect();
    let id_chosen = if all_names.contains(&id_provider()) { id_provider() } else { all_names.first().cloned().unwrap_or_default() };

    let link = {
        let base = base.clone();
        move |prov: String, pid: String| {
            let base = base.clone();
            spawn(async move {
                match api::post(&base, json!({ "provider": prov, "provider_id": pid })).await {
                    Ok(d) => {
                        ui::toast(state::trf("linked {provider} #{id} — metadata/art fetch queued", &[("provider", &prov), ("id", &pid)]), false);
                        current.set(api::arr(&d, "provider_links").to_vec());
                        on_done.call(());
                    }
                    Err(e) => ui::toast(e.message, true),
                }
            });
        }
    };
    let link_hit = link.clone();
    let link_url = link.clone();
    let link_id = link.clone();
    let chosen2 = chosen.clone();
    let heading = state::trf("Manual match — {name}", &[("name", &name)]);

    rsx! {
        h2 { "{heading}" }
        div { class: "muted", {state::tr("Search a provider by name and link the right entry, paste its page URL, or type its id.")} " " b { {state::tr("local")} } " " {state::tr("searches what's already cached (bulk syncs, earlier lookups; no network, every provider incl. IGDB; the platform filter applies to TheGamesDB and RetroAchievements);")} " " b { {state::tr("online")} } " " {state::tr("asks the provider. Title-tier providers (IGDB, ScreenScraper, …) attach to the title; RetroAchievements / TheGamesDB attach to releases. A manual link is never replaced automatically; linking queues a metadata + art fetch.")} }
        form { class: "inline", style: "margin:10px 0",
            onsubmit: move |e| {
                e.prevent_default();
                let path = format!("/api/v1/providers/{}/search{}", chosen2, api::qs(&[("name", q()), ("platform", platform()), ("local", if where_() == "local" { "true".into() } else { String::new() })]));
                results.set(Some(Ok(Vec::new())));
                spawn(async move {
                    match api::get(&path).await {
                        Ok(v) => results.set(Some(Ok(api::items(&v)))),
                        Err(e) => { results.set(None); ui::toast(e.message, true); }
                    }
                });
            },
            select { onchange: move |e| where_.set(e.value()),
                option { value: "local", selected: where_() == "local", {state::tr("local")} }
                option { value: "online", selected: where_() == "online", {state::tr("online")} }
            }
            select { onchange: move |e| provider.set(e.value()),
                for p in choices.clone() { option { value: "{p}", selected: p == chosen, "{p}" } }
            }
            input { r#type: "text", value: "{q}", style: "min-width:260px", oninput: move |e| q.set(e.value()) }
            select { onchange: move |e| platform.set(e.value()),
                option { value: "", {state::tr("any platform")} }
                for p in state::platforms() { option { value: "{u(&p, \"id\")}", selected: u(&p, "id").to_string() == platform(), "{s(&p, \"name\")}" } }
            }
            button { class: "btn small primary", {state::tr("search")} }
        }
        div {
            match results() {
                None => rsx! {},
                Some(Err(e)) => rsx! { span { class: "bad", "{e}" } },
                Some(Ok(hits)) if hits.is_empty() => rsx! { span { class: "muted", {state::tr("no results")} } },
                Some(Ok(hits)) => rsx! {
                    table { class: "grid",
                        for h in hits {
                            {
                                let link = link_hit.clone();
                                let (prov, pid) = (s(&h, "provider"), s(&h, "provider_id"));
                                let url = s(&h, "url");
                                rsx! {
                                    tr {
                                        td { Html { html: html::provider_icon(&prov) } " {pid}" }
                                        td { "{s(&h, \"title\")}" }
                                        td { class: "muted", "{s(&h, \"platform\")}" }
                                        td { if !url.is_empty() { a { class: "link", href: "{url}", target: "_blank", rel: "noopener", {state::tr("page")} } } }
                                        td { button { class: "btn small primary", onclick: move |_| link(prov.clone(), pid.clone()), {state::tr("link")} } }
                                    }
                                }
                            }
                        }
                    }
                },
            }
        }
        form { class: "inline", style: "margin:10px 0",
            onsubmit: move |e| {
                e.prevent_default();
                let u2 = page_url().trim().to_string();
                if u2.is_empty() { ui::toast(state::tr("paste a URL"), true); return; }
                spawn(async move {
                    match api::post("/api/v1/providers/resolve-url", json!({ "url": u2 })).await {
                        Ok(r) => resolved.set(Some(r)),
                        Err(e) => { resolved.set(None); ui::toast(e.message, true); }
                    }
                });
            },
            span { class: "muted", {state::tr("or page URL:")} }
            input { r#type: "text", placeholder: "https://www.igdb.com/games/… · thegamesdb.net/game.php?id=… · retroachievements.org/game/…", style: "min-width:420px", value: "{page_url}", oninput: move |e| page_url.set(e.value()) }
            button { class: "btn small", {state::tr("resolve")} }
        }
        if let Some(r) = resolved() {
            {
                let (prov, pid) = (s(&r, "provider"), s(&r, "provider_id"));
                let url = s(&r, "url");
                let title = api::so(&r, "title").unwrap_or_else(|| state::tr("(not cached yet)"));
                let link = link_url.clone();
                rsx! {
                    table { class: "grid", tr {
                        td { Html { html: html::provider_icon(&prov) } " {prov}" }
                        td { "{pid}" }
                        td { "{title}" }
                        td { if !url.is_empty() { a { class: "link", href: "{url}", target: "_blank", rel: "noopener", {state::tr("page")} } } }
                        td { button { class: "btn small primary", onclick: move |_| link(prov.clone(), pid.clone()), {state::tr("link")} } }
                    } }
                }
            }
        }
        form { class: "inline", style: "margin:10px 0",
            onsubmit: move |e| {
                e.prevent_default();
                let v = id_value().trim().to_string();
                if v.is_empty() { ui::toast(state::tr("type an id"), true); return; }
                link_id(id_chosen.clone(), v);
            },
            span { class: "muted", {state::tr("or by id:")} }
            select { onchange: move |e| id_provider.set(e.value()),
                for p in all_names.clone() { option { value: "{p}", "{p}" } }
            }
            input { r#type: "text", placeholder: state::tr("provider id"), style: "width:140px", value: "{id_value}", oninput: move |e| id_value.set(e.value()) }
            button { class: "btn small", {state::tr("link id")} }
        }
        h3 { {state::tr("Current links")} }
        if current().is_empty() {
            span { class: "muted", {state::tr("none")} }
        } else {
            table { class: "grid",
                for p in current() {
                    {
                        let prov = s(&p, "provider");
                        let via = s(&p, "via");
                        let rel = u(&p, "release_id");
                        let url = s(&p, "url");
                        let pid = s(&p, "provider_id");
                        let base = base.clone();
                        let unlink_url = if via == "release" && kind == "titles" {
                            format!("/api/v1/catalog/releases/{rel}/links/{prov}")
                        } else if via == "title" && kind == "releases" {
                            format!("/api/v1/catalog/titles/{}/links/{prov}", title_id.unwrap_or(0))
                        } else {
                            format!("{base}/{prov}")
                        };
                        let prov2 = prov.clone();
                        let prov3 = prov.clone();
                        let online = online_names.contains(&prov);
                        rsx! {
                            tr {
                                td { Html { html: html::provider_icon(&prov) } " {prov}" }
                                td { if url.is_empty() { "{pid}" } else { a { class: "link", href: "{url}", target: "_blank", rel: "noopener", "{pid}" } } }
                                td {
                                    Html { html: badge(&s(&p, "confidence"), "dim") }
                                    if via == "title" && kind == "releases" { Html { html: badge(&state::tr("via title"), "dim") } }
                                    if via == "release" && kind == "titles" { Html { html: badge(&state::trf("via release #{id}", &[("id", &rel.to_string())]), "dim") } }
                                }
                                td { button { class: "btn small", title: state::tr("search this provider for the right entry; linking it replaces this link, whatever matched it (a hash match too)"),
                                    // A link made by hand outranks every automatic one, a hash
                                    // match included: the next one linked replaces this.
                                    onclick: move |_| {
                                        where_.set(if online { "online" } else { "local" }.to_string());
                                        provider.set(prov3.clone());
                                        ui::toast(state::trf("search {provider} and link the right entry: it replaces the current link", &[("provider", &prov3)]), false);
                                    },
                                    {state::tr("change…")}
                                } " " button { class: "btn small",
                                    onclick: move |_| {
                                        let (unlink_url, prov) = (unlink_url.clone(), prov2.clone());
                                        spawn(async move {
                                            match api::delete(&unlink_url).await {
                                                Ok(d) => { ui::toast(state::trf("unlinked {provider}", &[("provider", &prov)]), false); current.set(api::arr(&d, "provider_links").to_vec()); on_done.call(()); }
                                                Err(e) => ui::toast(e.message, true),
                                            }
                                        });
                                    },
                                    {state::tr("unlink")}
                                } }
                            }
                        }
                    }
                }
            }
        }
        div { style: "margin-top:10px;text-align:right", button { class: "btn small", onclick: move |_| ui::close_modal(), {state::tr("close")} } }
    }
}

// --- base game filter (Titles toolbar) ---

/// Type to search titles; picking one sets the `basegame` filter through `on_pick`.
#[component]
pub fn BaseGameFilter(value: String, on_pick: EventHandler<String>) -> Element {
    let mut text = use_signal(String::new);
    let mut hits = use_signal(|| None::<Vec<Value>>);
    let mut gen = use_signal(|| 0u64);
    let v2 = value.clone();
    let label = use_resource(move || {
        let v = v2.clone();
        async move {
            if v.is_empty() {
                return String::new();
            }
            api::get(&format!("/api/v1/catalog/titles/{v}")).await.map(|t| api::so(&t, "display_name").unwrap_or_else(|| s(&t, "name"))).unwrap_or_else(|_| format!("#{v}"))
        }
    });
    rsx! {
        span { class: "combo", title: state::tr("base game: a title and its ports / remakes"),
            span { class: "chips",
                if !value.is_empty() {
                    span { class: "badge", "{label.read().clone().unwrap_or_default()} " b { title: state::tr("remove"), onclick: move |_| on_pick.call(String::new()), "×" } }
                }
            }
            input { r#type: "text", placeholder: state::tr("base game…"), value: "{text}",
                oninput: move |e| {
                    text.set(e.value());
                    let q = e.value().trim().to_string();
                    let g = { let mut w = gen.write(); *w += 1; *w };
                    if q.chars().count() < 2 { hits.set(None); return; }
                    spawn(async move {
                        ui::sleep(300).await;
                        if gen() != g { return; }
                        match api::get(&format!("/api/v1/catalog/titles{}", api::qs(&[("search", q), ("limit", "15".into()), ("view", "collection".into())]))).await {
                            Ok(p) => hits.set(Some(api::items(&p))),
                            Err(e) => ui::toast(e.message, true),
                        }
                    });
                },
                onblur: move |_| { spawn(async move { ui::sleep(150).await; hits.set(None); }); },
            }
            if let Some(list) = hits() {
                div { class: "drop",
                    if list.is_empty() { div { class: "muted", {state::tr("no titles")} } }
                    for t in list {
                        {
                            let id = u(&t, "id");
                            let name = api::so(&t, "display_name").unwrap_or_else(|| s(&t, "name"));
                            rsx! {
                                div { class: "opt", onmousedown: move |e| { e.prevent_default(); text.set(String::new()); hits.set(None); on_pick.call(id.to_string()); },
                                    "{name} " span { class: "muted", "{s(&t, \"platforms\")} · {s(&t, \"category\")}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

// --- title merge tools (Titles toolbar) ---

#[component]
pub fn TitleMergeTools() -> Element {
    let count = use_resource(|| async { api::get("/api/v1/catalog/titles/merge-suggestions?limit=1").await.map(|p| u(&p, "total")).ok() });
    let label = match *count.read() {
        Some(Some(n)) => state::trf("merge suggestions ({n})", &[("n", &n.to_string())]),
        _ => state::tr("merge suggestions"),
    };
    rsx! {
        button { class: "btn small", title: state::tr("titles that share an IGDB game but weren't merged automatically (name-search link, or different platforms)"), onclick: move |_| open_merge_suggestions(), "{label}" }
        button { class: "btn small", title: state::tr("merge titles sharing an IGDB id (exact/hash/manual links) on a common platform"),
            onclick: move |_| { spawn(async move { match api::post("/api/v1/catalog/titles/dedupe", json!({})).await { Ok(r) => ui::toast(if api::b(&r, "already_queued") { state::tr("already queued") } else { state::trf("merge pass queued #{id}", &[("id", &u(&r, "job_id").to_string())]) }, false), Err(e) => ui::toast(e.message, true) } }); },
            {state::tr("run merge pass")}
        }
        button { class: "btn small", title: state::tr("the merge pass normally looks only at titles, releases and links changed since the last pass; this one goes over every title"),
            onclick: move |_| { spawn(async move { match api::post("/api/v1/catalog/titles/dedupe?full=true", json!({})).await { Ok(r) => ui::toast(if api::b(&r, "already_queued") { state::tr("already queued") } else { state::trf("merge pass queued #{id}", &[("id", &u(&r, "job_id").to_string())]) }, false), Err(e) => ui::toast(e.message, true) } }); },
            {state::tr("full merge pass")}
        }
        button { class: "btn small", title: state::tr("re-apply cached IGDB games to titles that have no localized names yet; for others, tick them and use re-apply IGDB metadata on the selection"),
            onclick: move |_| { spawn(async move { match api::post("/api/v1/jobs", json!({ "job_type": "igdb_reapply" })).await { Ok(r) => ui::toast(state::trf("queued #{id}", &[("id", &u(&r, "id").to_string())]), false), Err(e) => ui::toast(e.message, true) } }); },
            {state::tr("re-apply IGDB metadata")}
        }
        button { class: "btn small", title: state::tr("re-apply the cached IGDB game of every IGDB-linked title, also those that have localized names already (names, genres, companies, base game); only name lookups go to IGDB"),
            onclick: move |_| {
                spawn(async move {
                    if !ui::confirm(&state::tr("Re-apply IGDB metadata to every IGDB-linked title? On a large catalogue this takes a while.")).await {
                        return;
                    }
                    match api::post("/api/v1/jobs", json!({ "job_type": "igdb_reapply", "all": true })).await {
                        Ok(r) => ui::toast(state::trf("queued #{id}", &[("id", &u(&r, "id").to_string())]), false),
                        Err(e) => ui::toast(e.message, true),
                    }
                });
            },
            {state::tr("re-apply IGDB metadata (all)")}
        }
        crate::ui::QueueButton {}
    }
}

pub fn open_merge_suggestions() {
    ui::open_modal(|| rsx! { MergeSuggestions {} });
}

#[component]
fn MergeSuggestions() -> Element {
    let mut status = use_signal(|| "pending".to_string());
    let mut offset = use_signal(|| 0u64);
    let mut page = use_resource(move || {
        let (st, off) = (status(), offset());
        async move { api::get(&format!("/api/v1/catalog/titles/merge-suggestions{}", api::qs(&[("status", st), ("limit", "50".into()), ("offset", off.to_string())]))).await }
    });
    let (items, total) = match &*page.read() {
        Some(Ok(p)) => (api::items(p), u(p, "total")),
        _ => (Vec::new(), 0),
    };
    // Ticked suggestions (igdb ids), for merging or dismissing several at once.
    let mut picked = use_signal(Vec::<String>::new);
    let page_ids: Vec<String> = items.iter().map(|g| s(g, "igdb_id")).collect();
    let bulk = move |action: &'static str, all: bool, n: u64| {
        let (st, ids) = (status(), picked());
        spawn(async move {
            if all {
                let q = if action == "accept" { state::trf("Merge all {n} suggestions? Each can be undone under Edit › Combined titles.", &[("n", &n.to_string())]) } else { state::trf("Dismiss all {n} suggestions?", &[("n", &n.to_string())]) };
                if !ui::confirm(&q).await {
                    return;
                }
            }
            match api::post("/api/v1/catalog/titles/merge-suggestions/bulk", json!({ "action": action, "status": st, "ids": if all { Vec::new() } else { ids }, "all": all })).await {
                Ok(r) => {
                    let failed = api::arr(&r, "failed").len();
                    let done = u(&r, "resolved").to_string();
                    let mut text = if action == "accept" { state::trf("merged {n} suggestion(s)", &[("n", &done)]) } else { state::trf("dismissed {n} suggestion(s)", &[("n", &done)]) };
                    if failed > 0 {
                        text.push_str(&format!(" — {}", state::trf("{n} could not be merged", &[("n", &failed.to_string())])));
                    }
                    ui::toast(text, failed > 0);
                    picked.set(Vec::new());
                    offset.set(0);
                    page.restart();
                }
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    let st = status();
    let off = offset();
    let range = state::trf("{from}–{to} of {total}", &[("from", &(if total > 0 { off + 1 } else { 0 }).to_string()), ("to", &(off + 50).min(total).to_string()), ("total", &total.to_string())]);
    let count_text = state::trf("{total} {status}", &[("total", &total.to_string()), ("status", &state::tr(&st))]);
    let act = move |id: String, action: &'static str| {
        spawn(async move {
            match api::post(&format!("/api/v1/catalog/titles/merge-suggestions/{}/{action}", api::encode(&id)), json!({})).await {
                Ok(r) => {
                    ui::toast(if action == "accept" { state::trf("merged into title #{id}", &[("id", &u(&r, "title_id").to_string())]) } else { state::tr("dismissed") }, false);
                    page.restart();
                }
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    rsx! {
        h2 { {state::tr("Merge suggestions")} }
        p { class: "muted", {state::tr("Titles linked to the same IGDB game that weren't merged automatically: the link is only a name-search guess, or the titles are on different platforms (ports). Accept merges them into one title (renamed by region priority); dismiss keeps them apart.")} }
        div { class: "toolbar",
            for x in ["pending", "accepted", "dismissed"] {
                button { class: if x == st { "btn small on" } else { "btn small" }, onclick: move |_| { status.set(x.to_string()); offset.set(0); picked.set(Vec::new()); }, {state::tr(x)} }
            }
            span { class: "muted", " {count_text}" }
        }
        if st != "accepted" && total > 0 {
            div { class: "toolbar",
                label {
                    input { r#type: "checkbox", checked: !page_ids.is_empty() && page_ids.iter().all(|id| picked().contains(id)),
                        onchange: { let page_ids = page_ids.clone(); move |e: Event<FormData>| picked.set(if e.checked() { page_ids.clone() } else { Vec::new() }) } }
                    " " {state::tr("select this page")}
                }
                button { class: "btn small primary", disabled: picked().is_empty(), onclick: move |_| bulk("accept", false, 0), {state::tr("merge selected")} }
                if st == "pending" { " " button { class: "btn small", disabled: picked().is_empty(), onclick: move |_| bulk("dismiss", false, 0), {state::tr("dismiss selected")} } }
                span { class: "spacer" }
                button { class: "btn small", onclick: move |_| bulk("accept", true, total), {state::trf("merge all {n}", &[("n", &total.to_string())])} }
                if st == "pending" { " " button { class: "btn small", onclick: move |_| bulk("dismiss", true, total), {state::trf("dismiss all {n}", &[("n", &total.to_string())])} } }
            }
        }
        table { class: "grid",
            if items.is_empty() { tr { td { class: "muted", {state::tr("none")} } } }
            for g in items {
                {
                    let igdb = s(&g, "igdb_id");
                    let reason = s(&g, "reason");
                    let titles = api::arr(&g, "titles").to_vec();
                    let first = titles.first().map(|t| s(t, "name")).unwrap_or_default();
                    let (i1, i2, i3) = (igdb.clone(), igdb.clone(), igdb.clone());
                    let ticked = picked().contains(&igdb);
                    rsx! {
                        tr {
                            if st != "accepted" {
                                td { input { r#type: "checkbox", checked: ticked, onchange: move |e: Event<FormData>| { let mut now = picked(); now.retain(|id| *id != i3); if e.checked() { now.push(i3.clone()); } picked.set(now); } } }
                            }
                            td {
                                if igdb.starts_with("name:") { {state::tr("name match")} } else { a { class: "link", href: "https://www.igdb.com/search?q={api::encode(&first)}", target: "_blank", rel: "noopener", "IGDB {igdb}" } }
                                br {}
                                Html { html: badge(&reason.replacen('_', " ", 1), if reason == "name_link" { "warn" } else { "dim" }) }
                            }
                            td {
                                for t in titles {
                                    {
                                        let tid = u(&t, "id");
                                        let conf = s(&t, "confidence");
                                        rsx! {
                                            div {
                                                span { class: "link", onclick: move |_| { ui::close_modal(); navigator().push(Route::TitleDetail { id: tid }); }, "{s(&t, \"name\")}" }
                                                span { class: "muted", " #{tid} · {s(&t, \"platforms\")} " }
                                                if !conf.is_empty() { Html { html: badge(&conf, if ["exact", "hash", "manual"].contains(&conf.as_str()) { "ok" } else { "warn" }) } }
                                            }
                                        }
                                    }
                                }
                            }
                            td {
                                if st == "pending" {
                                    button { class: "btn small primary", onclick: move |_| act(i1.clone(), "accept"), {state::tr("merge")} }
                                    " "
                                    button { class: "btn small", onclick: move |_| act(i2.clone(), "dismiss"), {state::tr("dismiss")} }
                                } else if st == "dismissed" {
                                    button { class: "btn small", onclick: move |_| act(i1.clone(), "accept"), {state::tr("merge anyway")} }
                                }
                            }
                        }
                    }
                }
            }
        }
        div { class: "pager",
            button { class: "btn", disabled: off == 0, onclick: move |_| offset.set(off.saturating_sub(50)), {state::tr("‹ prev")} }
            span { "{range}" }
            button { class: "btn", disabled: off + 50 >= total, onclick: move |_| offset.set(off + 50), {state::tr("next ›")} }
            " "
            button { class: "btn", onclick: move |_| ui::close_modal(), {state::tr("close")} }
        }
    }
}

/// `tags(list, f)`: badges or a dash.
pub fn tags_html(list: &[Value], f: impl Fn(&Value) -> String) -> String {
    if list.is_empty() {
        "<span class=\"muted\">—</span>".into()
    } else {
        list.iter().map(f).collect::<Vec<_>>().join(" ")
    }
}

/// The media strip of a title or release page: one horizontal, scrollable row of everything
/// the `media_show` setting allows. `query` is `title_id=...` or `release_id=...`; `stamp`
/// changes when the page's media did. Draws nothing when there is nothing to show.
///
/// Images are thumbnails that open full size; videos and audio carry their own controls and
/// load nothing until played. An item marked `remote` is not stored on this machine and is
/// streamed from the main server.
#[component]
pub fn MediaStrip(query: String, stamp: u64) -> Element {
    let mut zoom = use_signal(|| None::<String>);
    let items = use_resource(use_reactive!(|query, stamp| async move {
        let _ = stamp;
        api::get(&format!("/api/v1/media/strip?{query}")).await.ok().and_then(|v| v.as_array().cloned()).unwrap_or_default()
    }));
    let list = items.read().clone().unwrap_or_default();
    if list.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "card",
            div { class: "media-strip",
                for m in list {
                    {
                        let full = api::media_url(&s(&m, "file"));
                        let remote = api::b(&m, "remote");
                        // A thumbnail is made from a file held here; a streamed one is shown as it is.
                        let small = if remote { full.clone() } else { api::thumb(u(&m, "id")) };
                        let label = format!("{}{}", s(&m, "media_type"), api::so(&m, "sub_category").map(|c| format!(" / {c}")).unwrap_or_default());
                        let caption = format!("{label}{}", api::so(&m, "source").map(|c| format!(" · {c}")).unwrap_or_default());
                        rsx! {
                            figure { key: "{remote}-{u(&m, \"id\")}",
                                match s(&m, "kind").as_str() {
                                    "video" => rsx! { video { controls: true, preload: "metadata", src: "{full}" } },
                                    "audio" => rsx! { audio { controls: true, preload: "none", src: "{full}" } },
                                    _ => rsx! { img { src: "{small}", alt: "{label}", loading: "lazy", onclick: move |_| zoom.set(Some(full.clone())) } },
                                }
                                figcaption {
                                    "{caption}"
                                    if remote { " " Html { html: badge(&state::tr("streamed"), "dim") } }
                                }
                            }
                        }
                    }
                }
            }
        }
        if let Some(src) = zoom() {
            div { class: "media-zoom", onclick: move |_| zoom.set(None), img { src: "{src}", alt: "" } }
        }
    }
}

/// The DATs a DAT sync can leave out that are not a platform's games
/// (`dat_download::DAT_EXTRAS`): the same list as the web console's.
pub const DAT_EXTRAS: [(&str, &str); 7] = [
    ("audio", "audio CDs and music"),
    ("video", "video discs"),
    ("image", "photo CDs and images"),
    ("document", "manuals, magazines and scans (TOSEC-PIX)"),
    ("source", "source code"),
    ("theme", "avatars, themes and wallpapers"),
    ("other", "other DATs, platform unknown (Palm OS, ...)"),
];

/// What a DAT sync form (Jobs, Data › DATs) has ticked: platforms, and DATs without one.
#[derive(Clone, Copy, PartialEq)]
pub struct DatPick {
    pub platforms: Signal<std::collections::BTreeSet<u64>>,
    pub extras: Signal<std::collections::BTreeSet<String>>,
}

/// A DAT sync form's ticks, started from the `dat_sync_platforms` setting -- every
/// platform (or every extra) when it says all.
pub fn use_dat_platforms() -> DatPick {
    let mut platforms = use_signal(std::collections::BTreeSet::<u64>::new);
    let mut extras = use_signal(std::collections::BTreeSet::<String>::new);
    use_future(move || async move {
        let v = api::get("/api/v1/dat_platforms").await.ok();
        let ids = v.as_ref().and_then(|v| v.get("platforms")).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).collect());
        platforms.set(ids.unwrap_or_else(|| state::platforms().iter().map(|p| u(p, "id")).filter(|id| *id != 0).collect()));
        let keys = v.as_ref().and_then(|v| v.get("extras")).and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect());
        extras.set(keys.unwrap_or_else(|| DAT_EXTRAS.iter().map(|(k, _)| k.to_string()).collect()));
    });
    DatPick { platforms, extras }
}

/// A `dat_sync` job body with the ticked platforms and extras, or `None` with a toast when
/// nothing is ticked.
pub fn with_dat_platforms(pick: DatPick, mut body: Value) -> Option<Value> {
    let (platforms, extras) = ((pick.platforms)(), (pick.extras)());
    if platforms.is_empty() && extras.is_empty() {
        ui::toast(state::tr("check at least one platform"), true);
        return None;
    }
    body["platforms"] = json!(platforms.iter().collect::<Vec<_>>());
    body["extras"] = json!(extras.iter().collect::<Vec<_>>());
    Some(body)
}

/// The platform list of a DAT sync form, under its explanation, and the extras.
#[component]
pub fn DatPlatformPicker(pick: DatPick) -> Element {
    rsx! {
        p { class: "muted", {state::tr("Platforms to download and import (from Settings › dat_sync_platforms; untick to leave some out of this run).")} }
        PlatformPicker { checked: pick.platforms, offered: None }
        DatExtrasPicker { extras: pick.extras }
    }
}

/// "import downloaded" (Data › DATs): every DAT in the downloaded files, or only the
/// platforms (and kinds of non-platform DAT) ticked in a second step, preset from the
/// setting. Shown in the popup; `on_go` gets the job body.
pub fn ask_dat_import(body: Value, on_go: impl Fn(Value) + Clone + 'static) {
    ui::open_modal(move || {
        let on_go = on_go.clone();
        rsx! { DatImportAsk { body: body.clone(), on_go: move |b: Value| on_go(b) } }
    });
}

#[component]
fn DatImportAsk(body: Value, on_go: EventHandler<Value>) -> Element {
    let mut by_platform = use_signal(|| false);
    let pick = use_dat_platforms();
    let whole = {
        let body = body.clone();
        move |_| {
            let mut b = body.clone();
            b["platforms"] = json!(state::platforms().iter().map(|p| u(p, "id")).filter(|id| *id != 0).collect::<Vec<_>>());
            b["extras"] = json!(DAT_EXTRAS.iter().map(|(k, _)| *k).collect::<Vec<_>>());
            ui::close_modal();
            on_go.call(b);
        }
    };
    let go = {
        let body = body.clone();
        move |_| {
            if let Some(b) = with_dat_platforms(pick, body.clone()) {
                ui::close_modal();
                on_go.call(b);
            }
        }
    };
    rsx! {
        h2 { {state::tr("Import downloaded DATs")} }
        if by_platform() {
            DatPlatformPicker { pick }
            div { class: "toolbar", style: "margin-top:10px",
                button { class: "btn primary", r#type: "button", onclick: go, {state::tr("import")} }
                " "
                button { class: "btn", r#type: "button", onclick: move |_| ui::close_modal(), {state::tr("cancel")} }
            }
        } else {
            p { class: "muted", {state::tr("Import every DAT in the downloaded files, or only the platforms you pick.")} }
            div { class: "toolbar",
                button { class: "btn primary", r#type: "button", onclick: whole, {state::tr("whole zip")} }
                " "
                button { class: "btn", r#type: "button", onclick: move |_| by_platform.set(true), {state::tr("by platform…")} }
                " "
                button { class: "btn", r#type: "button", onclick: move |_| ui::close_modal(), {state::tr("cancel")} }
            }
        }
    }
}

/// The checkboxes for the DATs that are not a platform's games.
#[component]
pub fn DatExtrasPicker(extras: Signal<std::collections::BTreeSet<String>>, on_change: Option<EventHandler<()>>) -> Element {
    let mut extras = extras;
    rsx! {
        div { class: "muted", style: "margin-top:8px", {state::tr("Not a platform")} }
        div { class: "optgrid",
            for (k, l) in DAT_EXTRAS {
                label { key: "{k}",
                    input { r#type: "checkbox", checked: extras().contains(k), onchange: move |e| { extras.with_mut(|x| { if e.checked() { x.insert(k.to_string()); } else { x.remove(k); } }); if let Some(h) = on_change { h.call(()); } } }
                    " " {state::tr(l)}
                }
            }
        }
    }
}

/// Makers and kinds of machine the platform checkbox lists toggle by (`family` and `group`
/// of the vocabulary's platforms); the same lists as the web console's.
pub const PLATFORM_MAKERS: [(&str, &str); 9] = [("nintendo", "Nintendo"), ("sony", "Sony"), ("microsoft", "Microsoft"), ("sega", "Sega"), ("snk", "SNK"), ("atari", "Atari"), ("bandai", "Bandai"), ("nec", "NEC"), ("msx", "MSX")];
pub const PLATFORM_GROUPS: [(&str, &str); 5] = [("console", "consoles"), ("handheld", "handhelds"), ("arcade", "arcade"), ("retro_pc", "retro PCs"), ("modern_pc", "modern PCs")];

/// The platform checkbox list of the provider request form and the `dat_sync_platforms`
/// setting: a filter, check all / clear all, and toggles by maker and by kind of machine
/// (pressed again, a toggle unchecks what it checked). Each platform's name is above its
/// checkbox. `offered`: the platforms that may be ticked at all (`None` = every one).
#[component]
pub fn PlatformPicker(checked: Signal<std::collections::BTreeSet<u64>>, offered: Option<Vec<u64>>, on_change: Option<EventHandler<()>>) -> Element {
    let mut checked = checked;
    let mut pfilter = use_signal(String::new);
    let is_offered = { let offered = offered.clone(); move |id: u64| offered.as_ref().is_none_or(|o| o.contains(&id)) };
    let all: Vec<Value> = state::platforms().into_iter().filter(|p| u(p, "id") != 0 && is_offered(u(p, "id"))).collect();
    let q = pfilter().trim().to_lowercase();
    let shown: Vec<(u64, String)> = all.iter().map(|p| (u(p, "id"), s(p, "name"))).filter(|(_, n)| q.is_empty() || n.to_lowercase().contains(&q) || state::platform_label(n).to_lowercase().contains(&q)).collect();
    let shown_ids: Vec<u64> = shown.iter().map(|(id, _)| *id).collect();
    let n = checked().len();
    let changed = move || {
        if let Some(h) = on_change {
            h.call(());
        }
    };
    // Every offered platform whose `field` is `value`: all checked -> uncheck them, else check them.
    let toggle = {
        let all = all.clone();
        move |field: &'static str, value: &'static str| {
            // A maker's arcade boards (Neo Geo AES/MVS) are left to the arcade button.
            let ids: Vec<u64> = all.iter().filter(|p| s(p, field) == value && !(field == "family" && s(p, "group") == "arcade")).map(|p| u(p, "id")).collect();
            checked.with_mut(|c| {
                if ids.iter().all(|id| c.contains(id)) {
                    for id in &ids {
                        c.remove(id);
                    }
                } else {
                    c.extend(ids.iter().copied());
                }
            });
            changed();
        }
    };
    let toggle2 = toggle.clone();
    rsx! {
        div { style: "flex-basis:100%",
            div { class: "toolbar", style: "margin:6px 0",
                input { r#type: "search", placeholder: state::tr("filter platforms…"), style: "min-width:200px", value: "{pfilter}", oninput: move |e| pfilter.set(e.value()), onkeydown: move |e| if e.key() == Key::Enter { e.prevent_default() } }
                " "
                button { class: "btn small", r#type: "button", onclick: move |_| { checked.with_mut(|c| c.extend(shown_ids.iter().copied())); changed(); }, {state::tr("check all")} }
                " "
                button { class: "btn small", r#type: "button", onclick: move |_| { checked.with_mut(|c| c.clear()); changed(); }, {state::tr("clear all")} }
                " "
                if n > 0 { span { class: "muted", {state::trf("{n} checked", &[("n", &n.to_string())])} } }
            }
            div { class: "toolbar", style: "margin:0 0 6px", title: state::tr("Checks every platform of that maker or kind; pressed again, unchecks them."),
                for (f, l) in PLATFORM_MAKERS {
                    button { key: "{f}", class: "btn small", r#type: "button", onclick: { let mut t = toggle.clone(); move |_| t("family", f) }, "{l}" }
                    " "
                }
                " | "
                for (g, l) in PLATFORM_GROUPS {
                    button { key: "{g}", class: "btn small", r#type: "button", onclick: { let mut t = toggle2.clone(); move |_| t("group", g) }, {state::tr(l)} }
                    " "
                }
            }
            div { class: "optgrid", style: "max-height:260px;overflow:auto",
                for (id, pn) in shown {
                    label { key: "{id}", class: "platcheck",
                        span { {state::platform_label(&pn)} }
                        input { r#type: "checkbox", checked: checked().contains(&id), onchange: move |e| { checked.with_mut(|c| { if e.checked() { c.insert(id); } else { c.remove(&id); } }); changed(); } }
                    }
                }
            }
        }
    }
}
