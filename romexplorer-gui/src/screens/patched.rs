//! What a patched release is made from, on the pages: the "made from" row of a release page
//! with its confirm / change / unlink, the same three for a guessed language, the lists of
//! what is made from a release or a title, a title's RetroAchievements patch files, and the
//! "patch links" dialog of the Titles toolbar (the console's `patchLinks`).

use dioxus::prelude::*;
use serde_json::{json, Value};

use super::common;
use crate::api::{self, s, u};
use crate::html::{self, badge};
use crate::state;
use crate::ui::{self, Html};
use crate::Route;

/// A release that is a game changed by someone: listed apart from the game's own.
pub fn is_patched(r: &Value) -> bool {
    api::so(r, "material").is_none() && matches!(s(r, "game_kind").as_str(), "hack" | "mod" | "translation")
}

/// Whether a release page has a "made from" row: it has a link, or is the kind that could.
pub fn has_base_row(r: &Value) -> bool {
    r.get("base").is_some_and(|b| !b.is_null()) || is_patched(r) || matches!(s(r, "material").as_str(), "patch" | "trainer" | "save" | "mappack")
}

fn status_label(status: &str) -> String {
    state::tr(match status {
        "exact" => "exact",
        "igdb" => "by IGDB",
        "hash" => "by hash",
        "suggested" => "suggested",
        "manual" => "confirmed",
        "rejected" => "unlinked",
        other => other,
    })
}

/// Rows for releases that are only referred to: what is made from this release, or from
/// this title but filed under a title of its own.
#[component]
pub fn PatchedRefs(items: Vec<Value>) -> Element {
    rsx! {
        table { class: "grid",
            for p in items {
                {
                    let id = u(&p, "id");
                    let held = u(&p, "local_games") > 0;
                    let kind = common::kind_badge(&json!({ "game_kind": s(&p, "game_kind"), "base_status": s(&p, "status") }));
                    let material = s(&p, "material");
                    rsx! {
                        tr { key: "{id}", class: if held { "local" } else { "" }, style: "cursor:pointer", onclick: move |_| { navigator().push(Route::Release { id }); },
                            td { "{s(&p, \"name\")} " Html { html: kind } if !material.is_empty() { Html { html: badge(&material, "dim") } } }
                            td { Html { html: api::so(&p, "platform").map(|x| html::cbadge("platform", &x, None, None)).unwrap_or_default() } }
                            td { Html { html: common::release_lang_badges(&p) } }
                            td { if held { Html { html: badge(&state::tr("held"), "ok") } } }
                        }
                    }
                }
            }
        }
    }
}

/// Titles per page of the picker.
const PICK_PAGE: u64 = 25;

/// Pick a title by name: for "made from" set by hand, or releases moved to another title.
/// Best matches first (the exact name, then names starting with it), a page at a time. Given
/// the releases being moved, titles on their platforms come first, or alone with the toggle.
#[component]
pub(crate) fn PickTitle(heading: String, on_pick: EventHandler<u64>, #[props(default)] release_ids: Vec<u64>) -> Element {
    let mut text = use_signal(String::new);
    let mut query = use_signal(String::new);
    let mut gen = use_signal(|| 0u64);
    let mut offset = use_signal(|| 0u64);
    let near = release_ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let has_near = !near.is_empty();
    let mut near_only = use_signal(move || has_near);
    let found = use_resource(move || {
        let (q, at, only, near) = (query(), offset(), near_only(), near.clone());
        async move {
            if q.trim().is_empty() {
                return None;
            }
            let mut params = vec![("search", q.trim().to_string()), ("limit", PICK_PAGE.to_string()), ("offset", at.to_string()), ("view", "collection".into()), ("kinds", "commercial,homebrew,arcade,wiiware".into())];
            if !near.is_empty() {
                params.push(("near_releases", near));
                params.push(("near_only", only.to_string()));
            }
            api::get(&format!("/api/v1/catalog/titles{}", api::qs(&params))).await.ok().map(|p| (api::items(&p), u(&p, "total")))
        }
    });
    let (items, total) = found.read().clone().flatten().unwrap_or_default();
    let at = offset();
    let shown = format!("{}–{} / {total}", if total == 0 { 0 } else { at + 1 }, (at + PICK_PAGE).min(total));
    rsx! {
        h2 { "{heading}" }
        input { r#type: "text", style: "width:100%", placeholder: state::tr("search titles…"), value: "{text}",
            oninput: move |e| {
                text.set(e.value());
                let g = { let mut w = gen.write(); *w += 1; *w };
                let value = e.value();
                spawn(async move {
                    ui::sleep(300).await;
                    if gen() == g {
                        offset.set(0);
                        query.set(value);
                    }
                });
            },
        }
        if has_near {
            label { class: "muted", input { r#type: "checkbox", checked: near_only(), onchange: move |e| { offset.set(0); near_only.set(e.checked()); } } " " {state::tr("only titles on these releases' platforms")} }
        }
        if !query().trim().is_empty() && found.read().is_some() && items.is_empty() {
            div { class: "muted", {state::tr("no titles")} }
        }
        table { class: "grid",
            for t in items {
                {
                    let id = u(&t, "id");
                    rsx! {
                        tr { key: "{id}", style: "cursor:pointer", onclick: move |_| { ui::close_modal(); on_pick.call(id); },
                            td { "{api::so(&t, \"display_name\").unwrap_or_else(|| s(&t, \"name\"))}" }
                            td { Html { html: html::platform_badges(&s(&t, "platforms")) } }
                        }
                    }
                }
            }
        }
        div { class: "pager",
            if total > PICK_PAGE {
                button { class: "btn", disabled: at == 0, onclick: move |_| offset.set(at.saturating_sub(PICK_PAGE)), "‹" }
                span { class: "muted", " {shown} " }
                button { class: "btn", disabled: at + PICK_PAGE >= total, onclick: move |_| offset.set(at + PICK_PAGE), "›" }
                " "
            }
            button { class: "btn", onclick: move |_| ui::close_modal(), {state::tr("close")} }
        }
    }
}

/// The "made from" row of a release page: the link, how sure it is, and the user's say.
#[component]
pub fn BaseRow(id: u64, release: Value, on_change: EventHandler<()>) -> Element {
    let base = release.get("base").cloned().filter(|b| !b.is_null());
    let say = move |body: Value| {
        spawn(async move {
            match api::post(&format!("/api/v1/catalog/releases/{id}/base"), body).await {
                Ok(_) => on_change.call(()),
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    let status = base.as_ref().map(|b| s(b, "status")).unwrap_or_default();
    let (base_release, base_title) = (base.as_ref().and_then(|b| api::uo(b, "release_id")), base.as_ref().and_then(|b| api::uo(b, "title_id")));
    let name = base.as_ref().map(|b| api::so(b, "release_name").or_else(|| api::so(b, "title_name")).unwrap_or_else(|| "—".into())).unwrap_or_default();
    rsx! {
        if base.is_some() {
            span { class: "link", onclick: move |_| {
                    if let Some(id) = base_release { navigator().push(Route::Release { id }); } else if let Some(id) = base_title { navigator().push(Route::TitleDetail { id }); }
                }, "{name}" }
            " " Html { html: badge(&status_label(&status), if status == "suggested" { "warn" } else { "dim" }) } " "
        } else {
            span { class: "muted", {state::tr("not linked")} } " "
        }
        if base.is_some() && status != "manual" {
            button { class: "btn small", onclick: move |_| say(json!({ "action": "confirm" })), {state::tr("confirm")} } " "
        }
        button { class: "btn small",
            onclick: move |_| ui::open_modal(move || rsx! { PickTitle { heading: state::tr("The game this is made from"), on_pick: move |title: u64| say(json!({ "action": "set", "title_id": title })) } }),
            {state::tr("change…")}
        }
        if base.is_some() {
            " "
            button { class: "btn small",
                onclick: move |_| { spawn(async move { if ui::confirm(&state::tr("Unlink this release from that game? It will no longer be listed there.")).await { say(json!({ "action": "unlink" })); } }); },
                {state::tr("unlink")}
            }
        }
    }
}

/// The language of a patched release or a patch: confirm a guess, change it, or clear it.
#[component]
pub fn LangTools(id: u64, release: Value, on_change: EventHandler<()>) -> Element {
    if !(is_patched(&release) || s(&release, "material") == "patch" || api::so(&release, "languages_state").is_some()) {
        return rsx! {};
    }
    let say = move |body: Value| {
        spawn(async move {
            match api::post(&format!("/api/v1/catalog/releases/{id}/language"), body).await {
                Ok(_) => on_change.call(()),
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    let languages = s(&release, "languages");
    let guessed = s(&release, "languages_state") == "guessed";
    let current = languages.clone();
    rsx! {
        " "
        if guessed {
            Html { html: badge(&state::tr("guessed"), "warn") } " "
            button { class: "btn small", onclick: move |_| say(json!({ "action": "confirm" })), {state::tr("confirm")} } " "
        }
        button { class: "btn small",
            onclick: move |_| {
                let current = current.clone();
                spawn(async move {
                    if let Some(codes) = ui::prompt(&state::tr("Language codes, comma separated (en, pt, zh):"), &current).await.filter(|c| !c.trim().is_empty()) {
                        say(json!({ "action": "set", "languages": codes.trim() }));
                    }
                });
            },
            {state::tr("change…")}
        }
        if !languages.is_empty() {
            " " button { class: "btn small", onclick: move |_| say(json!({ "action": "clear" })), {state::tr("unlink")} }
        }
    }
}

/// A title's RetroAchievements patch files, with the download its game page has.
#[component]
pub fn RaPatchesCard(title_id: u64) -> Element {
    let mut page = use_resource(move || async move { api::get(&format!("/api/v1/catalog/patches{}", api::qs(&[("from", "ra".to_string()), ("title_id", title_id.to_string()), ("limit", "200".into())]))).await.ok() });
    let (items, total) = match &*page.read() {
        Some(Some(p)) => (api::items(p), u(p, "total")),
        _ => (Vec::new(), 0),
    };
    if items.is_empty() {
        return rsx! {};
    }
    let heading = state::trf("RetroAchievements patches ({n})", &[("n", &total.to_string())]);
    rsx! {
        div { class: "card",
            h2 { "{heading}" }
            table { class: "grid",
                for x in items {
                    {
                        let (game, md5, held, job) = (u(&x, "ra_game_id"), s(&x, "ra_md5"), api::b(&x, "held"), api::so(&x, "job"));
                        rsx! {
                            tr { key: "{md5}", style: "cursor:pointer", onclick: move |_| { navigator().push(Route::RaGame { id: game }); },
                                td { "{s(&x, \"name\")}" }
                                td { Html { html: common::release_lang_badges(&x) } }
                                td {
                                    if let Some(job) = job {
                                        Html { html: badge(&job, "warn") }
                                    } else if held {
                                        Html { html: badge(&state::tr("downloaded"), "ok") }
                                    } else {
                                        button { class: "btn small",
                                            onclick: move |e| {
                                                e.stop_propagation();
                                                let md5 = md5.clone();
                                                spawn(async move {
                                                    match api::post("/api/v1/ra/patches/download", json!({ "md5": md5 })).await {
                                                        Ok(r) => {
                                                            ui::toast(if api::b(&r, "already_queued") { state::tr("already downloading") } else { state::trf("patch download queued #{id}", &[("id", &u(&r, "job_id").to_string())]) }, false);
                                                            page.restart();
                                                        }
                                                        Err(e) => ui::toast(e.message, true),
                                                    }
                                                });
                                            },
                                            {state::tr("download patch")}
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

/// Titles toolbar: how many patched releases sit under a game by name alone, and the
/// dialog to confirm or unlink them.
#[component]
pub fn PatchLinksButton() -> Element {
    let count = use_resource(|| async { api::get("/api/v1/catalog/base-links?limit=1").await.map(|p| u(&p, "total")).ok() });
    let label = match *count.read() {
        Some(Some(n)) => state::trf("patch links ({n})", &[("n", &n.to_string())]),
        _ => state::tr("patch links"),
    };
    rsx! {
        button { class: "btn small", title: state::tr("hacks, translations and trainers placed under a game by name: confirm or unlink them"), onclick: move |_| ui::open_modal(|| rsx! { PatchLinks {} }), "{label}" }
    }
}

#[component]
fn PatchLinks() -> Element {
    let mut status = use_signal(|| "suggested".to_string());
    let mut offset = use_signal(|| 0u64);
    let mut search = use_signal(String::new);
    let mut page = use_resource(move || {
        let (st, off, q) = (status(), offset(), search());
        async move { api::get(&format!("/api/v1/catalog/base-links{}", api::qs(&[("status", st), ("limit", "50".into()), ("offset", off.to_string()), ("search", q)]))).await }
    });
    let (items, total) = match &*page.read() {
        Some(Ok(p)) => (api::items(p), u(p, "total")),
        _ => (Vec::new(), 0),
    };
    let mut picked = use_signal(Vec::<u64>::new);
    let page_ids: Vec<u64> = items.iter().map(|l| u(l, "id")).collect();
    let bulk = move |action: &'static str, all: bool, n: u64| {
        let ids = picked();
        spawn(async move {
            if all {
                let q = if action == "accept" { state::trf("Confirm all {n} suggested links?", &[("n", &n.to_string())]) } else { state::trf("Unlink all {n} suggested links? Each release leaves the game it was placed under.", &[("n", &n.to_string())]) };
                if !ui::confirm(&q).await {
                    return;
                }
            }
            match api::post("/api/v1/catalog/base-links/bulk", json!({ "action": action, "ids": if all { Vec::new() } else { ids }, "all": all })).await {
                Ok(r) => {
                    let done = u(&r, "changed").to_string();
                    ui::toast(if action == "accept" { state::trf("confirmed {n} link(s)", &[("n", &done)]) } else { state::trf("unlinked {n} release(s)", &[("n", &done)]) }, false);
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
    let open = st != "rejected" && st != "manual";
    let range = state::trf("{from}–{to} of {total}", &[("from", &(if total > 0 { off + 1 } else { 0 }).to_string()), ("to", &(off + 50).min(total).to_string()), ("total", &total.to_string())]);
    rsx! {
        h2 { {state::tr("Patch links")} }
        p { class: "muted", {state::tr("Hacks, translations, trainers and patches that were put under a game because a folder or their own name said so. Confirm keeps one there; unlink takes it out of that game and it is not suggested again.")} }
        div { class: "toolbar",
            for x in ["suggested", "manual", "rejected", "hash", "exact", "igdb"] {
                button { class: if x == st { "btn small on" } else { "btn small" }, onclick: move |_| { status.set(x.to_string()); offset.set(0); picked.set(Vec::new()); }, {status_label(x)} }
            }
            " "
            input { r#type: "text", placeholder: state::tr("search…"), value: "{search}", onchange: move |e| { search.set(e.value()); offset.set(0); picked.set(Vec::new()); } }
            span { class: "muted", " {total}" }
        }
        if open && total > 0 {
            div { class: "toolbar",
                label {
                    input { r#type: "checkbox", checked: !page_ids.is_empty() && page_ids.iter().all(|id| picked().contains(id)),
                        onchange: { let page_ids = page_ids.clone(); move |e: Event<FormData>| picked.set(if e.checked() { page_ids.clone() } else { Vec::new() }) } }
                    " " {state::tr("select this page")}
                }
                button { class: "btn small primary", disabled: picked().is_empty(), onclick: move |_| bulk("accept", false, 0), {state::tr("confirm selected")} }
                " "
                button { class: "btn small", disabled: picked().is_empty(), onclick: move |_| bulk("reject", false, 0), {state::tr("unlink selected")} }
                span { class: "spacer" }
                if st == "suggested" && search().is_empty() {
                    button { class: "btn small", onclick: move |_| bulk("accept", true, total), {state::trf("confirm all {n}", &[("n", &total.to_string())])} }
                    " "
                    button { class: "btn small", onclick: move |_| bulk("reject", true, total), {state::trf("unlink all {n}", &[("n", &total.to_string())])} }
                }
            }
        }
        table { class: "grid",
            if items.is_empty() { tr { td { class: "muted", {state::tr("none")} } } }
            for l in items {
                {
                    let id = u(&l, "id");
                    let base = l.get("base").cloned().unwrap_or(Value::Null);
                    let base_title = api::uo(&base, "title_id");
                    let base_name = api::so(&base, "title_name").unwrap_or_else(|| base_title.map(|t| format!("#{t}")).unwrap_or_default());
                    let ticked = picked().contains(&id);
                    let material = s(&l, "material");
                    rsx! {
                        tr { key: "{id}",
                            if open {
                                td { input { r#type: "checkbox", checked: ticked, onchange: move |e: Event<FormData>| { let mut now = picked(); now.retain(|x| *x != id); if e.checked() { now.push(id); } picked.set(now); } } }
                            }
                            td {
                                span { class: "link", onclick: move |_| { ui::close_modal(); navigator().push(Route::Release { id }); }, "{s(&l, \"name\")}" }
                                " " Html { html: common::kind_badge(&json!({ "game_kind": s(&l, "game_kind") })) }
                                if !material.is_empty() { Html { html: badge(&material, "dim") } }
                                br {}
                                Html { html: api::so(&l, "platform").map(|x| html::cbadge("platform", &x, None, None)).unwrap_or_default() }
                            }
                            td {
                                if let Some(title) = base_title {
                                    span { class: "link", onclick: move |_| { ui::close_modal(); navigator().push(Route::TitleDetail { id: title }); }, "{base_name}" }
                                }
                                if let Some(name) = api::so(&base, "release_name") { div { class: "muted", "{name}" } }
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
