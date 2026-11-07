//! Edit: the user's keyword tags, combined titles (decouple), manual release order (the
//! console's `showKeywordTags` / `showCombinedTitles` / `showManualOrders`).
//! TODO(release): admin users only once accounts exist.

use std::rc::Rc;

use dioxus::prelude::*;
use serde_json::{json, Value};

use super::common::{self, bump, CellLink};
use crate::api::{self, s, u};
use crate::components::{Column, Filter, Grid, GridSpec, SelAction, Spec};
use crate::html::{self, badge};
use crate::state;
use crate::ui::{self, Html};
use crate::Route;

// --- keyword tags ---

#[component]
pub fn EditKeywords() -> Element {
    let reload = use_signal(|| 0u64);
    let spec = use_hook(move || {
        let columns = vec![
            Column::text("name", &state::tr("keyword")).sort("name"),
            Column::render("titles", &state::tr("titles"), |r| {
                let n = u(r, "titles");
                if n > 0 {
                    let q = format!("keywords={}", api::encode(&s(r, "name")));
                    rsx! { CellLink { to: Route::Titles { q }, text: n.to_string() } }
                } else {
                    rsx! { Html { html: badge(&state::tr("0 — unused"), "warn") } }
                }
            })
            .sort("titles")
            .num(),
            Column::html("user_titles", &state::tr("your tags"), |r| match u(r, "user_titles") {
                0 => String::new(),
                n => n.to_string(),
            })
            .num(),
            Column::html("auto_titles", &state::tr("automatic"), |r| match u(r, "auto_titles") {
                0 => String::new(),
                n => n.to_string(),
            })
            .num(),
            Column::html("files", &state::tr("audio files"), |r| match (u(r, "files"), u(r, "auto_files")) {
                (0, _) => String::new(),
                (n, 0) => n.to_string(),
                (n, a) => format!("{n} <span class=\"muted\">({})</span>", html::esc(&state::trf("{n} automatic", &[("n", &a.to_string())]))),
            })
            .num(),
            Column::html("sources", &state::trc("sources", "from"), |r| s(r, "sources").split(',').filter(|x| !x.is_empty()).map(|x| badge(x, "")).collect::<Vec<_>>().join(" ")),
            Column::render("act", "", move |r| {
                let (id, name, titles) = (u(r, "id"), s(r, "name"), u(r, "titles"));
                let n2 = name.clone();
                rsx! {
                    button { class: "btn small",
                        onclick: move |e| {
                            e.stop_propagation();
                            let name = name.clone();
                            spawn(async move {
                                let Some(new) = ui::prompt(&state::tr("Rename keyword (an existing name joins the two):"), &name).await.filter(|n| !n.trim().is_empty()) else { return };
                                match api::patch(&format!("/api/v1/catalog/keywords/{id}"), json!({ "name": new.trim() })).await {
                                    Ok(_) => { ui::toast(state::tr("renamed"), false); common::forget_facets(); bump(reload); }
                                    Err(e) => ui::toast(e.message, true),
                                }
                            });
                        },
                        {state::tr("rename")}
                    }
                    " "
                    button { class: "btn small danger",
                        onclick: move |e| {
                            e.stop_propagation();
                            let name = n2.clone();
                            spawn(async move {
                                let q = if titles > 0 { state::trf("Delete \"{name}\"? It is removed from {n} title(s).", &[("name", &name), ("n", &titles.to_string())]) } else { state::trf("Delete \"{name}\"?", &[("name", &name)]) };
                                if !ui::confirm(&q).await { return; }
                                match api::delete(&format!("/api/v1/catalog/keywords/{id}")).await {
                                    Ok(r) => { ui::toast(state::trf("deleted ({n} title tag(s) removed)", &[("n", &u(&r, "removed_tags").to_string())]), false); common::forget_facets(); bump(reload); }
                                    Err(e) => ui::toast(e.message, true),
                                }
                            });
                        },
                        {state::tr("delete")}
                    }
                }
            }),
        ];
        let mut spec = GridSpec::new("keywords", columns, |q| async move {
            let scope = q.f("scope");
            let mut p = vec![("scope", if scope.is_empty() { "user".into() } else { scope }), ("source", q.f("source")), ("unused", q.f("unused"))];
            p.extend(q.base());
            api::get(&format!("/api/v1/catalog/keywords{}", api::qs(&p))).await
        })
        .filters(vec![
            Filter::Select { key: "scope", label: state::tr("which keywords"), options: Rc::new(|| vec![("user".into(), state::tr("your tags + unused")), ("all".into(), state::tr("all keywords (incl. providers)"))]) },
            Filter::Select {
                key: "source",
                label: state::tr("tag source"),
                options: Rc::new(|| vec![(String::new(), state::tr("tags from anyone")), ("user".into(), state::tr("your tags")), ("auto".into(), state::tr("automatic tags"))]),
            },
            Filter::Check { key: "unused", label: state::tr("unused only (0 titles)"), off: String::new() },
        ])
        .toolbar(move || {
            rsx! {
                button { class: "btn small danger", title: state::tr("delete every keyword no title has"),
                    onclick: move |_| {
                        spawn(async move {
                            if !ui::confirm(&state::tr("Delete every keyword that no title has?")).await { return; }
                            match api::post("/api/v1/catalog/keywords/delete-unused", json!({})).await {
                                Ok(r) => { ui::toast(state::trf("deleted {n} unused keyword(s)", &[("n", &u(&r, "deleted").to_string())]), false); bump(reload); }
                                Err(e) => ui::toast(e.message, true),
                            }
                        });
                    },
                    {state::tr("delete all unused")}
                }
                button { class: "btn small danger", title: state::tr("remove the auto-tagger's tags from titles and audio files (every one; select keywords to remove only theirs); your own tags stay"),
                    onclick: move |_| {
                        spawn(async move {
                            if !ui::confirm(&state::tr("Remove every automatic tag?")).await { return; }
                            if remove_auto(json!({})).await { bump(reload); }
                        });
                    },
                    {state::tr("remove automatic tags")}
                }
            }
        })
        .select(|r| u(r, "id"), vec![SelAction::new(&state::tr("remove automatic tags"), |ids: Vec<u64>| async move { remove_auto(json!({ "ids": ids })).await })]);
        spec.default_sort = "name".into();
        Spec(Rc::new(spec))
    });
    rsx! {
        div { class: "card", h2 { {state::tr("Keyword tags")} }
            ui::Explain { p { {state::tr("Your keyword tags (add them from a title page, or right-click / select titles in the Titles list) and keywords no title uses any more. The count is how many titles have the keyword; click it for those titles. Renaming onto an existing keyword joins the two. “All keywords” includes the providers' (IGDB …) keywords too: renaming or deleting those lasts only until the provider's next fetch.")} } }
        }
        div { class: "card", Grid { spec, reload: Some(reload) } }
    }
}

/// Removes the auto-tagger's tags (titles and audio files) of the keywords in `body`.
async fn remove_auto(body: serde_json::Value) -> bool {
    match api::post("/api/v1/catalog/keywords/remove-auto", body).await {
        Ok(r) => {
            ui::toast(state::trf("removed {t} automatic title tag(s) and {f} automatic audio tag(s)", &[("t", &u(&r, "title_tags").to_string()), ("f", &u(&r, "file_tags").to_string())]), false);
            common::forget_facets();
            true
        }
        Err(e) => {
            ui::toast(e.message, true);
            false
        }
    }
}

// --- name dictionary ---

const DICT_SOURCES: [(&str, &str); 4] = [("user", "yours"), ("catalog", "catalogue"), ("series", "series"), ("wikidata", "Wikidata")];

fn dict_source(source: &str) -> String {
    DICT_SOURCES.iter().find(|(k, _)| *k == source).map(|(_, l)| if *l == "Wikidata" { l.to_string() } else { state::tr(l) }).unwrap_or_else(|| source.to_string())
}

async fn dict_build(wikidata: bool) {
    match api::post("/api/v1/jobs", json!({ "job_type": "dictionary_build", "wikidata": wikidata })).await {
        Ok(r) => ui::toast(state::trf("{what} queued #{id}", &[("what", &state::tr("dictionary rebuild")), ("id", &u(&r, "id").to_string())]), false),
        Err(e) => ui::toast(e.message, true),
    }
}

/// The name dictionary: one game or series under its names in different scripts, which
/// searches also try and the audio auto-tagger matches by.
#[component]
pub fn EditDictionary() -> Element {
    let reload = use_signal(|| 0u64);
    let info = use_resource(move || async move {
        let _ = reload();
        api::get("/api/v1/dictionary?limit=1").await.ok()
    });
    let spec = use_hook(move || {
        let columns = vec![
            Column::html("names", &state::tr("names"), |r| api::arr(r, "names").iter().map(|n| html::esc(n.as_str().unwrap_or(""))).collect::<Vec<_>>().join(" <span class=\"muted\">=</span> ")),
            Column::html("source", &state::trc("sources", "from"), |r| badge(&dict_source(&s(r, "source")), if s(r, "source") == "user" { "ok" } else { "dim" })),
        ];
        let spec = GridSpec::new("dictionary", columns, |q| async move {
            let mut p = vec![("source", q.f("source"))];
            p.extend(q.base());
            api::get(&format!("/api/v1/dictionary{}", api::qs(&p))).await
        })
        .filters(vec![Filter::Select {
            key: "source",
            label: state::trc("sources", "from"),
            options: Rc::new(|| {
                let mut o = vec![(String::new(), state::tr("every source"))];
                o.extend(DICT_SOURCES.iter().map(|(k, _)| (k.to_string(), dict_source(k))));
                o
            }),
        }])
        .toolbar(move || {
            rsx! {
                button { class: "btn small primary",
                    onclick: move |_| {
                        spawn(async move {
                            let Some(text) = ui::prompt(&state::tr("Names for the same game or series, separated by \"=\" (Hoge Quest = ホゲクエスト):"), "").await else { return };
                            let names: Vec<String> = text.split('=').map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).collect();
                            match api::post("/api/v1/dictionary", json!({ "names": names })).await {
                                Ok(_) => { ui::toast(state::tr("added"), false); bump(reload); }
                                Err(e) => ui::toast(e.message, true),
                            }
                        });
                    },
                    {state::tr("add names…")}
                }
                button { class: "btn small", title: state::tr("rebuild the automatic names from the catalogue (and the kept Wikidata download, if any)"), onclick: move |_| { spawn(dict_build(false)); }, {state::tr("rebuild")} }
                crate::ui::QueueButton {}
                button { class: "btn small", title: state::tr("download Wikidata's video games and series with Japanese names (about 15 seconds), then rebuild"), onclick: move |_| { spawn(dict_build(true)); }, {state::tr("rebuild + download Wikidata")} }
                crate::ui::QueueButton {}
                button { class: "btn small",
                    onclick: move |_| {
                        spawn(async move {
                            let Some(q) = ui::prompt(&state::tr("A search to try:"), "").await.filter(|q| !q.trim().is_empty()) else { return };
                            match api::get(&format!("/api/v1/dictionary/variants{}", api::qs(&[("q", q.trim().to_string())]))).await {
                                Ok(v) => {
                                    let names: Vec<String> = v.as_array().cloned().unwrap_or_default().iter().filter_map(|x| x.as_str().map(str::to_string)).collect();
                                    ui::toast(if names.is_empty() { state::tr("the dictionary knows nothing in it") } else { state::trf("also searched for: {names}", &[("names", &names.join(" · "))]) }, false);
                                }
                                Err(e) => ui::toast(e.message, true),
                            }
                        });
                    },
                    {state::tr("try a search…")}
                }
                button { class: "btn small", title: state::tr("forget the automatic names you removed: the next rebuild brings them back"),
                    onclick: move |_| {
                        spawn(async move {
                            match api::post("/api/v1/dictionary/blocks/clear", json!({})).await {
                                Ok(r) => ui::toast(state::trf("{n} removed name(s) come back with the next rebuild", &[("n", &u(&r, "cleared").to_string())]), false),
                                Err(e) => ui::toast(e.message, true),
                            }
                        });
                    },
                    {state::tr("bring back removed")}
                }
            }
        })
        .select(
            |r| u(r, "id"),
            vec![SelAction::new(&state::tr("remove"), |ids: Vec<u64>| async move {
                if !ui::confirm(&state::trf("Remove {n} group(s)? Automatic ones stay out of later rebuilds.", &[("n", &ids.len().to_string())])).await {
                    return false;
                }
                match api::post("/api/v1/dictionary/remove", json!({ "ids": ids })).await {
                    Ok(r) => {
                        ui::toast(state::trf("removed {n} name(s)", &[("n", &u(&r, "removed").to_string())]), false);
                        true
                    }
                    Err(e) => {
                        ui::toast(e.message, true);
                        false
                    }
                }
            })],
        );
        Spec(Rc::new(spec))
    });
    let summary = match info.read().clone().flatten() {
        Some(p) => {
            let sources = api::arr(&p, "sources").iter().map(|x| format!("{} {}", dict_source(&s(x, "source")), u(x, "groups"))).collect::<Vec<_>>();
            let kept = if api::b(&p, "wikidata_kept") { state::tr("Wikidata download kept") } else { state::tr("Wikidata not downloaded") };
            format!("{} · {kept}", if sources.is_empty() { state::tr("empty — rebuild to fill it") } else { sources.join(" · ") })
        }
        None => String::new(),
    };
    rsx! {
        div { class: "card", h2 { {state::tr("Name dictionary")} }
            ui::Explain { p { {state::tr("The same game or series under its names in different languages (Hoge Quest = ホゲクエスト). A search in one also finds the other — titles, releases, games, cross-platform and audio — and the audio auto-tagger matches albums by them. Built from the catalogue (IGDB localized names, alternative names, and series taken from numbered titles) and from Wikidata when downloaded; names you add stay through every rebuild, and automatic ones you remove stay removed. Turn it off with search_translations under Config › Settings.")} } }
            div { class: "muted", "{summary}" }
        }
        div { class: "card", Grid { spec, reload: Some(reload) } }
    }
}

// --- combined titles ---

#[component]
pub fn EditMerges(q: String) -> Element {
    rsx! { for k in [q] { MergesPage { key: "{k}", q: k } } }
}

#[component]
fn MergesPage(q: String) -> Element {
    let only = common::parse_q(&q).get("title").cloned().unwrap_or_default();
    let reload = use_signal(|| 0u64);
    let only2 = only.clone();
    let spec = use_hook(move || {
        let only = only2.clone();
        let columns = vec![
            Column::render("merged", &state::tr("merged title"), |r| rsx! { "{s(r, \"merged_name\")} " span { class: "muted", "#{u(r, \"merged_title_id\")}" } }),
            Column::render("into", &state::tr("into"), |r| match api::uo(r, "current_title_id") {
                Some(cur) => {
                    let text = api::so(r, "current_title_name").unwrap_or_else(|| format!("#{cur}"));
                    let via = u(r, "survivor_id");
                    let via_text = state::trf("(via #{id})", &[("id", &via.to_string())]);
                    let split = s(r, "reason") == "split";
                    rsx! {
                        if split { span { class: "muted", {state::tr("split out of")} } " " }
                        CellLink { to: Route::TitleDetail { id: cur }, text }
                        if cur != via { " " span { class: "muted", "{via_text}" } }
                    }
                }
                None => rsx! {
                    if s(r, "reason") == "split" { span { class: "muted", {state::tr("split out of")} } " " }
                    span { class: "muted", {state::tr("gone")} }
                },
            }),
            Column::html("reason", &state::tr("why"), |r| format!("<span class=\"mono\">{}</span>", html::esc(&s(r, "reason")))),
            Column::html("grade", &state::tr("grade"), |r| { let g = s(r, "grade"); html::badge(&state::tr(&g), if g == "name" { "warn" } else { "dim" }) }),
            Column::html("releases", &state::tr("releases"), |r| match api::uo(r, "release_count") {
                Some(n) => n.to_string(),
                None => format!("<span class=\"muted\" title=\"{}\">?</span>", html::esc(&state::tr("merged before titles were archived"))),
            })
            .num(),
            Column::html("when", &state::tr("merged"), |r| html::fmt_date(&s(r, "merged_at"))),
            Column::render("act", "", move |r| {
                let split = s(r, "reason") == "split";
                if let Some(undone) = api::so(r, "undone_at") {
                    let rt = u(r, "restored_title_id");
                    let decoupled = if split { format!("{} ", state::trf("rejoined {date} →", &[("date", &html::fmt_date(&undone))])) } else { state::trf("decoupled {date} → ", &[("date", &html::fmt_date(&undone))]) };
                    return rsx! { span { class: "muted", "{decoupled}" } CellLink { to: Route::TitleDetail { id: rt }, text: format!("#{rt}") } };
                }
                let (lid, merged, cur) = (u(r, "id"), s(r, "merged_name"), s(r, "current_title_name"));
                rsx! {
                    button { class: "btn small",
                        onclick: move |e| {
                            e.stop_propagation();
                            let (merged, cur) = (merged.clone(), cur.clone());
                            spawn(async move {
                                let ask = if split { state::trf("Put \"{merged}\" back into \"{current}\"?", &[("merged", &merged), ("current", &cur)]) } else { state::trf("Split \"{merged}\" back out of \"{current}\"?", &[("merged", &merged), ("current", &cur)]) };
                                if !ui::confirm(&ask).await { return; }
                                match api::post(&format!("/api/v1/catalog/titles/merges/{lid}/decouple"), json!({})).await {
                                    Ok(res) => { ui::toast(state::trf(if split { "back in title #{id}" } else { "decoupled: title #{id}" }, &[("id", &u(&res, "title_id").to_string())]), false); bump(reload); }
                                    Err(e) => ui::toast(e.message, true),
                                }
                            });
                        },
                        {state::tr(if split { "undo split" } else { "decouple" })}
                    }
                }
            }),
        ];
        let spec = GridSpec::new("merges", columns, move |q| {
            let only = only.clone();
            async move {
                let p = vec![("search", q.search.clone()), ("title_id", only), ("grade", q.f("grade")), ("include_undone", q.f("include_undone")), ("limit", q.limit.to_string()), ("offset", q.offset.to_string())];
                api::get(&format!("/api/v1/catalog/titles/merges{}", api::qs(&p))).await
            }
        })
        .filters(vec![
            Filter::Select { key: "grade", label: state::tr("grade"), options: Rc::new(|| std::iter::once((String::new(), state::tr("any grade"))).chain(["name", "hash", "manual"].into_iter().map(|g| (g.to_string(), state::tr(g)))).collect()) },
            Filter::Check { key: "include_undone", label: state::tr("include decoupled"), off: String::new() },
        ]);
        Spec(Rc::new(spec))
    });
    let into_title = state::trf("into title #{id}", &[("id", &only)]);
    rsx! {
        div { class: "card",
            h2 {
                {state::tr("Combined titles")}
                if !only.is_empty() {
                    " " span { class: "muted", "{into_title}" } " "
                    a { class: "link", onclick: move |_| { navigator().push(Route::EditMerges { q: String::new() }); }, {state::tr("show all")} }
                }
                " "
                button { class: "btn small", title: state::tr("decouple merges whose titles don't share the IGDB game's names, and remove such IGDB links (a job; see Jobs)"),
                    onclick: move |_| { spawn(async move { match api::post("/api/v1/jobs", json!({ "job_type": "title_merge_audit" })).await { Ok(r) => ui::toast(state::trf("merge audit queued #{id}", &[("id", &u(&r, "id").to_string())]), false), Err(e) => ui::toast(e.message, true) } }); },
                    {state::tr("audit merges")}
                }
                crate::ui::QueueButton {}
                // Every merge of one grade, 500 a request until none is left.
                for grade in ["name", "hash"] {
                    " "
                    button { class: "btn small", title: state::tr("decouple every merge of this grade"),
                        onclick: move |_| { spawn(async move {
                            if !ui::confirm(&state::trf("Decouple every {grade} merge?", &[("grade", &state::tr(grade))])).await { return; }
                            let (mut done, mut left) = (0u64, 0u64);
                            loop {
                                match api::post("/api/v1/catalog/titles/merges/decouple", json!({ "grade": grade })).await {
                                    Ok(r) => { done += u(&r, "decoupled"); left = u(&r, "remaining"); if u(&r, "decoupled") == 0 || left == 0 { break; } }
                                    Err(e) => { ui::toast(e.message, true); break; }
                                }
                            }
                            ui::toast(state::trf("{n} decoupled, {left} left merged", &[("n", &done.to_string()), ("left", &left.to_string())]), false);
                            bump(reload);
                        }); },
                        {state::trf("decouple all: {grade}", &[("grade", &state::tr(grade))])}
                    }
                }
            }
            ui::Explain { p { {state::tr("Every merge of one title into another — automatic (shared IGDB game or RetroAchievements hash: ")} span { class: "mono", "igdb:…" } ", " span { class: "mono", "ra:…" } {state::tr("), from the suggestions backlog, or yours (")} span { class: "mono", "manual" } "). "
                b { {state::tr("Decouple")} } {state::tr(" splits a title back out under its old id and name, with its own releases and metadata; the automatic pass won't join the two again (merging them yourself lifts that). Merges from before titles were archived come back as a new title with the releases whose name matches, without their metadata.")} " " {state::tr("Splits are listed here too (why: split): undo split puts the releases back into the title they came out of.")} } }
        }
        div { class: "card", Grid { spec, reload: Some(reload) } }
    }
}

// --- manual order ---

#[component]
pub fn EditOrders() -> Element {
    let reload = use_signal(|| 0u64);
    let spec = use_hook(move || {
        let columns = vec![
            Column::render("title", &state::tr("title"), |r| rsx! { CellLink { to: Route::TitleDetail { id: u(r, "title_id") }, text: s(r, "title_name") } }),
            Column::render("top", &state::tr("on top"), |r| match api::uo(r, "top_release_id") {
                Some(id) => rsx! { CellLink { to: Route::Release { id }, text: s(r, "top_release_name") } " " Html { html: html::region_badges(&s(r, "region")) } },
                None => rsx! {},
            }),
            Column::text("placed", &state::tr("placed")).num(),
            Column::html("set_at", &state::trc("column", "changed"), |r| html::fmt_date(&s(r, "set_at"))),
        ];
        let spec = GridSpec::new("release_orders", columns, |q| async move {
            let p = vec![("search", q.search.clone()), ("limit", q.limit.to_string()), ("offset", q.offset.to_string())];
            api::get(&format!("/api/v1/catalog/titles/release-orders{}", api::qs(&p))).await
        })
        .select(
            |r| u(r, "title_id"),
            vec![SelAction::new(&state::tr("reset to default order"), |ids| async move {
                match api::post("/api/v1/catalog/titles/release-orders/clear", json!({ "title_ids": ids })).await {
                    Ok(r) => {
                        ui::toast(state::trf("reset {n} title(s)", &[("n", &u(&r, "cleared").to_string())]), false);
                        true
                    }
                    Err(e) => {
                        ui::toast(e.message, true);
                        false
                    }
                }
            })],
        )
        .toolbar(move || {
            rsx! {
                button { class: "btn small danger",
                    onclick: move |_| {
                        spawn(async move {
                            if !ui::confirm(&state::tr("Put every title back in the default release order?")).await { return; }
                            match api::post("/api/v1/catalog/titles/release-orders/clear", json!({ "all": true })).await {
                                Ok(r) => { ui::toast(state::trf("reset {n} title(s)", &[("n", &u(&r, "cleared").to_string())]), false); bump(reload); }
                                Err(e) => ui::toast(e.message, true),
                            }
                        });
                    },
                    {state::tr("reset all")}
                }
            }
        });
        Spec(Rc::new(spec))
    });
    rsx! {
        div { class: "card", h2 { {state::tr("Manual order")} }
            ui::Explain { p { {state::tr("Titles whose releases you put in your own order (drag ⋮⋮ or ▲▼ in a title's release list). Your order beats every other rule — region, language, demo / patch, edition and version — and decides the title's name and art in the lists. Select titles and reset them to the default order, or reset all.")} } }
        }
        div { class: "card", Grid { spec, reload: Some(reload) } }
    }
}

// --- review queues (merge review, aspect review) ---

/// The Titles list's filters, with a set: what both review queues narrow by.
fn review_filters(sets: Signal<Vec<(String, String)>>, extra: Vec<Filter>) -> Vec<Filter> {
    let mut filters = vec![
        Filter::Select {
            key: "set_id",
            label: state::tr("set"),
            options: Rc::new(move || {
                let mut o = vec![(String::new(), state::tr("any set"))];
                o.extend(sets.read().iter().cloned());
                o
            }),
        },
        common::platform_filter(),
        common::view_filter(),
        common::kinds_filter(),
        common::material_filter(),
    ];
    filters.extend(extra);
    filters.extend(common::facet_filters());
    filters
}

/// The filters' query pairs (no paging). The release kinds only when picked: the list's
/// default kinds would narrow every review to the Titles list's rows for nothing.
fn review_params(q: &crate::components::Query) -> Vec<(&'static str, String)> {
    let mut p = vec![("search", q.search.clone()), ("platforms", q.f("platform")), ("kinds", q.f("kinds")), ("material", q.f("material")), ("view", q.f("view")), ("set_id", q.f("set_id"))];
    p.extend(common::facet_params(q));
    p
}

/// The sets, for the set filter.
fn use_sets() -> Signal<Vec<(String, String)>> {
    let mut sets = use_signal(Vec::new);
    use_future(move || async move {
        if let Ok(v) = api::get("/api/v1/sets").await {
            let list = v.as_array().cloned().unwrap_or_else(|| api::items(&v));
            sets.set(list.iter().map(|x| (u(x, "id").to_string(), s(x, "name"))).collect());
        }
    });
    sets
}

/// A page of a queue, each row given its place in the queue (for "review this one").
async fn queue_page(path: &str, q: &crate::components::Query, extra: Vec<(&'static str, String)>, mut params: Signal<Vec<(&'static str, String)>>) -> api::ApiResult {
    let mut p = review_params(q);
    p.extend(extra);
    params.set(p.clone());
    p.extend([("limit", q.limit.to_string()), ("offset", q.offset.to_string())]);
    let mut page = api::get(&format!("{path}{}", api::qs(&p))).await?;
    if let Some(items) = page.get_mut("items").and_then(|i| i.as_array_mut()) {
        for (i, item) in items.iter_mut().enumerate() {
            item["position"] = json!(q.offset + i as u64);
        }
    }
    Ok(page)
}

fn review_kind_label(kind: &str) -> String {
    state::tr(match kind {
        "international" => "International",
        "directors_cut" => "Director's Cut",
        "translated_name" => "translated name",
        "bundle_vs_game" => "bundle to split",
        "split_names" => "another game inside",
        "igdb" => "same IGDB game",
        other => other,
    })
}

/// " taken" / " said no" for a decided suggestion, nothing for a pending one.
fn review_status_badge(status: &str) -> String {
    match status {
        "accepted" => format!(" {}", badge(&state::tr("taken"), "ok")),
        "dismissed" => format!(" {}", badge(&state::tr("said no"), "dim")),
        _ => String::new(),
    }
}

fn review_option_label(o: &serde_json::Value, variant: &str) -> String {
    match s(o, "label").as_str() {
        "with_foreign" => state::trf("{variant} with the foreign versions, the Japanese original apart", &[("variant", variant)]),
        "alone" => state::trf("{variant} apart, the Japanese original with the foreign versions", &[("variant", variant)]),
        "merge" => state::tr("merge into one title"),
        "split" => state::tr("give these releases a title of their own"),
        "split_bundle" => state::tr("split into the single games, each joining its game's title"),
        other => other.to_string(),
    }
}

#[component]
pub fn EditMergeReview() -> Element {
    let reload = use_signal(|| 0u64);
    let params = use_signal(Vec::<(&'static str, String)>::new);
    let at = use_signal(|| 0u64);
    let sets = use_sets();
    let spec = use_hook(move || {
        let columns = vec![
            Column::html("kind", &state::tr("kind"), |r| format!("{}{}", badge(&review_kind_label(&s(r, "kind")), "dim"), review_status_badge(&s(r, "status")))),
            Column::render("titles", &state::tr("titles"), move |r| {
                let names = api::arr(r, "titles").iter().map(|t| s(t, "name")).collect::<Vec<_>>().join(" · ");
                let position = u(r, "position");
                let mut at = at;
                rsx! { span { class: "link", title: state::tr("review this one"), onclick: move |_| at.set(position), "{names}" } }
            }),
            Column::html("platforms", &state::tr("platforms"), |r| {
                let mut seen: Vec<String> = Vec::new();
                for t in api::arr(r, "titles") {
                    for p in s(t, "platforms").split(", ").filter(|p| !p.is_empty()) {
                        if !seen.iter().any(|x| x == p) {
                            seen.push(p.to_string());
                        }
                    }
                }
                html::platform_badges(&seen.join(", "))
            }),
        ];
        let kind_filter = Filter::Select {
            key: "kind",
            label: state::tr("kind"),
            options: Rc::new(|| {
                let mut o = vec![(String::new(), state::tr("every kind"))];
                o.extend(["international", "directors_cut", "split_names", "translated_name", "bundle_vs_game", "igdb"].iter().map(|k| (k.to_string(), review_kind_label(k))));
                o
            }),
        };
        let state_filter = Filter::Select {
            key: "state",
            label: state::tr("state"),
            options: Rc::new(|| vec![(String::new(), state::tr("to do")), ("done".to_string(), state::tr("done")), ("all".to_string(), state::tr("all"))]),
        };
        let spec = GridSpec::new("merge_review", columns, move |q| async move {
            let (kind, st) = (q.f("kind"), q.f("state"));
            queue_page("/api/v1/review/merges", &q, vec![("kind", kind), ("state", st)], params).await
        })
        .filters(review_filters(sets, vec![kind_filter, state_filter]));
        Spec(Rc::new(spec))
    });
    rsx! {
        div { class: "card", h2 { {state::tr("Merge review")} }
            ui::Explain { p { {state::tr("Suggestions IGDB's grouping cannot give, one at a time: an International or Director's Cut release next to its Japanese original and the foreign versions, a Japanese release filed under another game, names that translate each other, a bundle holding single games, and titles sharing an IGDB game. Take a choice, say no (not offered again), or skip. The filters narrow the queue like the Titles list, a set included. Settings › merge_review_order puts the regional ones or the IGDB ones first.")} } }
        }
        MergeCard { params, at, reload }
        div { class: "card", Grid { spec, reload: Some(reload) } }
    }
}

#[component]
fn MergeCard(params: Signal<Vec<(&'static str, String)>>, at: Signal<u64>, reload: Signal<u64>) -> Element {
    let mut at = at;
    let item = use_resource(move || {
        let (mut p, offset, _) = (params(), at(), reload());
        async move {
            p.extend([("limit", "1".to_string()), ("offset", offset.to_string())]);
            api::get(&format!("/api/v1/review/merges{}", api::qs(&p))).await.ok()
        }
    });
    let page = item.read().clone().flatten();
    let Some(page) = page else { return rsx! { div { class: "card muted", {state::tr("loading…")} } } };
    let total = u(&page, "total");
    let Some(it) = api::arr(&page, "items").first().cloned() else {
        return rsx! {
            div { class: "card",
                if total > 0 && at() > 0 {
                    {state::tr("End of the queue.")}
                    " "
                    button { class: "btn small", onclick: move |_| at.set(0), {state::tr("back to the start")} }
                } else {
                    {state::tr("Nothing to review here.")}
                }
            }
        };
    };
    let key = s(&it, "key");
    let variant = api::so(&it, "variant").map(|v| state::tr(&v)).unwrap_or_default();
    let releases = it.get("releases").cloned().unwrap_or_default();
    let release_label = move |id: u64| releases.get(id.to_string()).map(|r| format!("{} [{}]", s(r, "name"), api::so(r, "region").unwrap_or_else(|| "—".into()))).unwrap_or_else(|| format!("#{id}"));
    let decide = move |key: String, body: Option<serde_json::Value>| {
        spawn(async move {
            let action = if body.is_some() { "apply" } else { "dismiss" };
            match api::post(&format!("/api/v1/review/merges/{}/{action}", api::encode(&key)), body.unwrap_or(json!({}))).await {
                Ok(_) => bump(reload),
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    let position = at() + 1;
    let (k_no, k_look) = (key.clone(), key.clone());
    let matched = api::arr(&it, "matched").iter().filter_map(|m| m.as_str()).collect::<Vec<_>>().join(" ↔ ");
    let titles = api::arr(&it, "titles").to_vec();
    let options = api::arr(&it, "options").to_vec();
    let status = s(&it, "status");
    let taken = status == "accepted";
    let arrange = it.clone();
    let k_arrange = key.clone();
    rsx! {
        div { class: "card", tabindex: "0",
            onkeydown: move |e: KeyboardEvent| match e.key() {
                Key::ArrowRight => at.set(at() + 1),
                Key::ArrowLeft => at.set(at().saturating_sub(1)),
                _ => {}
            },
            h2 {
                Html { html: badge(&review_kind_label(&s(&it, "kind")), "warn") }
                Html { html: review_status_badge(&status) }
                " "
                span { class: "muted", style: "font-weight:normal", "{position} / {total}" }
            }
            if !matched.is_empty() {
                div { class: "muted", {state::tr("matched by:")} " {matched}" }
            }
            div { style: "display:flex;flex-wrap:wrap;gap:12px;margin:8px 0",
                for t in titles {
                    ReviewTitleCard { key: "{u(&t, \"id\")}", title: t.clone() }
                }
            }
            if api::b(&it, "can_look_up") {
                LookupPanel { key: "{k_look}", lookup: k_look.clone() }
            }
            div { style: "display:flex;flex-direction:column;gap:6px;margin-top:8px",
                for (i, o) in options.into_iter().enumerate() {
                    {
                        let label = review_option_label(&o, &variant);
                        let groups: Vec<String> = api::arr(&o, "groups").iter().map(|g| g.as_array().into_iter().flatten().filter_map(|x| x.as_u64()).map(&release_label).collect::<Vec<_>>().join(", ")).collect();
                        let key = key.clone();
                        let rec = api::b(&o, "recommended");
                        let current = api::b(&o, "current");
                        rsx! {
                            div { key: "{i}",
                                button { class: if rec { "btn primary" } else { "btn" }, disabled: taken, onclick: move |_| decide(key.clone(), Some(json!({ "option": i }))),
                                    "{label}"
                                    if rec { " " {state::tr("(recommended)")} }
                                    if current { " " {state::tr("(as now)")} }
                                }
                                for (n, g) in groups.into_iter().enumerate() {
                                    div { key: "{n}", class: "muted", style: "font-size:12px;margin-left:12px", {state::trf("title {n}:", &[("n", &(n + 1).to_string())])} " {g}" }
                                }
                            }
                        }
                    }
                }
            }
            if !taken {
                ArrangePanel { key: "{k_arrange}", item: arrange, reload }
            }
            div { class: "toolbar", style: "margin-top:8px",
                if status != "dismissed" {
                    button { class: "btn danger", title: state::tr("not offered again"), onclick: move |_| decide(k_no.clone(), None), {state::tr("Leave as it is")} }
                }
                " "
                button { class: "btn", onclick: move |_| at.set(at() + 1), {state::tr("Skip")} " ›" }
                " "
                button { class: "btn", disabled: at() == 0, onclick: move |_| at.set(at().saturating_sub(1)), "‹ " {state::tr("Back")} }
            }
        }
    }
}

/// A release's name and region on a merge review card, by id.
type Names = std::collections::HashMap<u64, (String, String)>;
/// The boxes of [`ArrangePanel`]: a title (none: a new one) and the releases in it.
type Boxes = Vec<(Option<u64>, Vec<u64>)>;

/// Where [`ArrangePanel`] starts (the console's `arrangeStart`): the recommended choice's
/// grouping, each group in the title holding most of it, when it has one -- else the titles
/// as they are.
fn arrange_start(it: &Value, from_suggestion: bool) -> (Boxes, Names) {
    let mut names: Names = std::collections::HashMap::new();
    let mut holder: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
    let titles = api::arr(it, "titles");
    for t in titles {
        for r in api::arr(t, "releases") {
            names.insert(u(r, "id"), (s(r, "name"), s(r, "region")));
            holder.insert(u(r, "id"), u(t, "id"));
        }
    }
    if let Some(map) = it.get("releases").and_then(Value::as_object) {
        for r in map.values() {
            names.insert(u(r, "id"), (s(r, "name"), s(r, "region")));
            if let Some(t) = api::uo(r, "title_id") {
                holder.insert(u(r, "id"), t);
            }
        }
    }
    let live: Vec<u64> = titles.iter().filter(|t| !api::b(t, "deleted")).map(|t| u(t, "id")).collect();
    let rec = api::arr(it, "options").iter().find(|o| api::b(o, "recommended") && !api::arr(o, "groups").is_empty()).cloned();
    let mut boxes: Boxes = Vec::new();
    match rec.filter(|_| from_suggestion) {
        Some(o) => {
            let mut used = std::collections::HashSet::new();
            for g in api::arr(&o, "groups") {
                let ids: Vec<u64> = g.as_array().into_iter().flatten().filter_map(Value::as_u64).collect();
                let mut count: Vec<(u64, usize)> = Vec::new();
                for id in &ids {
                    if let Some(t) = holder.get(id) {
                        match count.iter_mut().find(|(x, _)| x == t) {
                            Some((_, n)) => *n += 1,
                            None => count.push((*t, 1)),
                        }
                    }
                }
                count.sort_by(|a, b| b.1.cmp(&a.1));
                let pick = count.into_iter().map(|(t, _)| t).find(|t| !used.contains(t) && live.contains(t));
                if let Some(t) = pick {
                    used.insert(t);
                }
                boxes.push((pick, ids));
            }
            boxes.extend(live.iter().filter(|t| !used.contains(t)).map(|t| (Some(*t), Vec::new())));
        }
        None => {
            boxes = live.iter().map(|t| (Some(*t), Vec::new())).collect();
            let mut ids: Vec<u64> = names.keys().copied().collect();
            ids.sort_unstable();
            for id in ids {
                if let Some(b) = holder.get(&id).and_then(|t| boxes.iter_mut().find(|(x, _)| *x == Some(*t))) {
                    b.1.push(id);
                }
            }
        }
    }
    (boxes, names)
}

/// The releases of a merge review card as boxes, one per title, to drag between (the
/// console's `arrangePanel`): into another title, or onto "+" for a new title named after the
/// first one dropped there. A tap picks a release and a tap on a box puts it there.
#[component]
fn ArrangePanel(item: Value, reload: Signal<u64>) -> Element {
    let start = use_hook(|| arrange_start(&item, true));
    let names = use_signal(|| start.1.clone());
    let mut boxes = use_signal(|| start.0.clone());
    let mut picked = use_signal(|| None::<u64>);
    let mut dragging = use_signal(|| None::<u64>);
    let key = s(&item, "key");
    let title_name = |id: u64| api::arr(&item, "titles").iter().find(|t| u(t, "id") == id).map(|t| s(t, "name")).unwrap_or_else(|| format!("#{id}"));
    // `to`: a box's index, or None for "+".
    let mut put = move |rid: u64, to: Option<usize>| {
        let mut b = boxes.write();
        for (_, rels) in b.iter_mut() {
            rels.retain(|x| *x != rid);
        }
        match to {
            Some(i) if i < b.len() => b[i].1.push(rid),
            _ => b.push((None, vec![rid])),
        }
        b.retain(|(t, rels)| t.is_some() || !rels.is_empty());
        picked.set(None);
    };
    let shown: Vec<(usize, Option<u64>, Vec<u64>)> = boxes.read().iter().enumerate().map(|(i, (t, r))| (i, *t, r.clone())).collect();
    let (it_now, it_rec) = (item.clone(), item.clone());
    rsx! {
        div { class: "card", style: "margin:8px 0",
            h3 { {state::tr("Arrange yourself")} }
            p { class: "muted", style: "font-size:12px", {state::tr("Drag releases to the title they belong to, or onto + for a new title named after the first one. Or tap a release, then a box.")} }
            div { style: "display:flex;flex-wrap:wrap;gap:8px",
                for (i, t, rels) in shown {
                    div { key: "{i}", class: "card", style: "flex:1 1 240px;margin:0;min-height:60px",
                        ondragover: move |e| e.prevent_default(),
                        ondrop: move |e| { e.prevent_default(); if let Some(rid) = dragging() { put(rid, Some(i)); dragging.set(None); } },
                        onclick: move |_| { if let Some(rid) = picked() { put(rid, Some(i)); } },
                        div {
                            b {
                                match t {
                                    Some(id) => rsx! { CellLink { to: Route::TitleDetail { id }, text: title_name(id) } },
                                    None => {
                                        let first = rels.first().and_then(|r| names.read().get(r).map(|n| n.0.clone())).unwrap_or_default();
                                        rsx! { {state::tr("new title")} ": {first}" }
                                    }
                                }
                            }
                            if t.is_some() && rels.is_empty() { " " span { class: "muted", {state::tr("(left empty: removed)")} } }
                        }
                        div { style: "display:flex;flex-direction:column;gap:2px;margin-top:4px;font-size:12px",
                            for rid in rels {
                                {
                                    let (name, region) = names.read().get(&rid).cloned().unwrap_or_else(|| (format!("#{rid}"), String::new()));
                                    let outline = if picked() == Some(rid) { "outline:2px solid var(--accent, #4a8);" } else { "" };
                                    rsx! {
                                        span { key: "{rid}", draggable: true, style: "cursor:grab;padding:2px 4px;border-radius:4px;{outline}",
                                            ondragstart: move |_| dragging.set(Some(rid)),
                                            onclick: move |e| { e.stop_propagation(); picked.set(if picked() == Some(rid) { None } else { Some(rid) }); },
                                            "⋮⋮ {name} "
                                            Html { html: html::region_badges(&region) }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                div { class: "card", title: state::tr("a new title"),
                    style: "flex:0 0 120px;margin:0;min-height:60px;display:flex;align-items:center;justify-content:center;font-size:28px;border-style:dashed",
                    ondragover: move |e| e.prevent_default(),
                    ondrop: move |e| { e.prevent_default(); if let Some(rid) = dragging() { put(rid, None); dragging.set(None); } },
                    onclick: move |_| { if let Some(rid) = picked() { put(rid, None); } },
                    "+"
                }
            }
            div { class: "toolbar", style: "margin-top:8px",
                button { class: "btn primary",
                    onclick: move |_| {
                        let key = key.clone();
                        let layout: Vec<Value> = boxes.read().iter().map(|(t, r)| json!({ "title_id": t, "releases": r })).collect();
                        spawn(async move {
                            match api::post(&format!("/api/v1/review/merges/{}/apply", api::encode(&key)), json!({ "layout": layout })).await {
                                Ok(_) => bump(reload),
                                Err(e) => ui::toast(e.message, true),
                            }
                        });
                    },
                    {state::tr("apply this grouping")}
                }
                " "
                button { class: "btn", onclick: move |_| { let (b, _) = arrange_start(&it_now, false); boxes.set(b); picked.set(None); }, {state::tr("start from the titles as they are")} }
                " "
                button { class: "btn", onclick: move |_| { let (b, _) = arrange_start(&it_rec, true); boxes.set(b); picked.set(None); }, {state::tr("start from the suggestion")} }
            }
        }
    }
}

/// One title of a merge review card: cover, name, platforms, IGDB game and releases.
#[component]
fn ReviewTitleCard(title: serde_json::Value) -> Element {
    let t = title;
    let id = u(&t, "id");
    let shown = api::arr(&t, "releases").to_vec();
    let more = u(&t, "release_total").saturating_sub(shown.len() as u64);
    rsx! {
        div { class: "card", style: "flex:1 1 280px;margin:0",
            if let Some(m) = api::uo(&t, "cover_media_id") {
                img { src: "{api::thumb(m)}", style: "max-width:96px;max-height:96px;float:right", alt: "" }
            }
            div {
                CellLink { to: Route::TitleDetail { id }, text: s(&t, "name") }
                " "
                Html { html: common::title_kind_badge(&t) }
                if api::b(&t, "deleted") { " " Html { html: badge(&state::tr("gone"), "bad") } }
            }
            div {
                Html { html: html::platform_badges(&s(&t, "platforms")) }
                if let Some(g) = api::so(&t, "igdb") { span { class: "muted", " IGDB {g}" } }
            }
            ul { style: "margin:4px 0;padding-left:18px;font-size:12px",
                for r in shown {
                    li { key: "{u(&r, \"id\")}", "{s(&r, \"name\")} " Html { html: html::region_badges(&s(&r, "region")) } }
                }
                if more > 0 { li { class: "muted", {state::trf("and {n} more", &[("n", &more.to_string())])} } }
            }
        }
    }
}

/// What the web says about an International / Director's Cut (the console's `lookupPanel`):
/// the in-app AI's answer, saved per suggestion and language, with Ask again / pin, "Ask in
/// <language>" when only another language's answer is saved, and the Ask menu of browser
/// links. Asked when the card opens only if nothing about it is saved yet.
#[component]
fn LookupPanel(lookup: String) -> Element {
    // What the next request asks for ("", "again", "ask"), for this suggestion only: a card
    // opened next must never inherit a paid "ask again".
    let mut req = use_signal(|| (String::new(), ""));
    let mut answer = use_resource(use_reactive!(|lookup| async move {
        let (for_key, mode) = req();
        let mode = if for_key == lookup { mode } else { "" };
        api::post(&format!("/api/v1/review/merges/{}/lookup", api::encode(&lookup)), json!({ "lang": state::ui_language(), "again": mode == "again", "ask": mode == "ask" })).await
    }));
    let a = answer.read().clone();
    let key = lookup.clone();
    rsx! {
        div { class: "card", style: "margin:8px 0",
            h3 { {state::tr("What differs")} }
            match a {
                None => rsx! { div { class: "muted", {state::tr("asking…")} } },
                Some(Err(e)) => rsx! { div { class: "bad", "{e.message}" } },
                Some(Ok(v)) => {
                    let saved = v.get("answer").filter(|x| !x.is_null()).cloned();
                    let other = v.get("other_language").filter(|x| !x.is_null()).cloned();
                    let shown = saved.clone().or_else(|| other.clone());
                    let pinned = saved.as_ref().is_some_and(|x| api::b(x, "pinned"));
                    let saved_id = saved.as_ref().map(|x| u(x, "id"));
                    let lang = s(&v, "lang");
                    let links = api::arr(&v, "ask_links").to_vec();
                    let (k_again, k_ask) = (key.clone(), key.clone());
                    rsx! {
                        if saved.is_none() {
                            if let Some(o) = &other {
                                div { class: "muted", {state::trf("Saved in {language}.", &[("language", &state::language_native(&s(o, "lang")))])} }
                            }
                        }
                        if let Some(x) = &shown {
                            div { style: "white-space:pre-wrap;font-size:13px", "{s(x, \"answer\")}" }
                            if !api::arr(x, "sources").is_empty() {
                                div { class: "muted", style: "font-size:12px;margin-top:4px",
                                    {state::tr("sources:")}
                                    " "
                                    for src in api::arr(x, "sources").to_vec() {
                                        a { class: "link", href: "{s(&src, \"url\")}", target: "_blank", rel: "noopener", {if s(&src, "title").is_empty() { s(&src, "url") } else { s(&src, "title") }} }
                                        " "
                                    }
                                }
                            }
                            div { class: "muted", style: "font-size:12px;margin-top:4px",
                                {state::trf("saved on {date} · {model}", &[("date", &html::fmt_date(&s(x, "created_at"))), ("model", &s(x, "model"))])}
                                if let Some(c) = x.get("cost_usd").and_then(Value::as_f64) {
                                    " · " {state::trf("approx. US${cost}", &[("cost", &format!("{c:.4}"))])}
                                }
                                if api::b(x, "pinned") { " " Html { html: badge(&state::tr("pinned"), "ok") } }
                            }
                        }
                        if let Some(err) = api::so(&v, "error") { div { class: "muted", "{err}" } }
                        div { class: "toolbar", style: "margin-top:6px",
                            if saved.is_some() && !pinned {
                                button { class: "btn small", title: state::tr("asks the AI again and replaces this answer"), onclick: move |_| req.set((k_again.clone(), "again")), {state::tr("Ask again")} }
                            }
                            if let Some(id) = saved_id {
                                button { class: "btn small", title: state::tr("a pinned answer is kept: never asked again, expired or cleared with the cache"),
                                    onclick: move |_| {
                                        spawn(async move {
                                            match api::put(&format!("/api/v1/ai/answers/{id}/pin"), json!({ "pinned": !pinned })).await {
                                                Ok(_) => answer.restart(),
                                                Err(e) => ui::toast(e.message, true),
                                            }
                                        });
                                    },
                                    {if pinned { state::tr("unpin") } else { state::tr("pin") }}
                                }
                            }
                            if saved.is_none() && other.is_some() {
                                button { class: "btn small primary", onclick: move |_| req.set((k_ask.clone(), "ask")), {state::trf("Ask in {language}", &[("language", &state::language_native(&lang))])} }
                            }
                        }
                        div { class: "toolbar", style: "margin-top:6px",
                            span { class: "muted", title: state::tr("opens the question in the browser, signed in as you"), {state::tr("Ask:")} }
                            for (i, l) in links.into_iter().enumerate() {
                                " "
                                a { class: if i == 0 { "btn small primary" } else { "btn small" }, href: "{s(&l, \"url\")}", target: "_blank", rel: "noopener", "{s(&l, \"label\")}" }
                            }
                            " "
                            a { class: "link", href: "{s(&v, \"google_url\")}", target: "_blank", rel: "noopener", {state::tr("search")} }
                        }
                    }
                }
            }
        }
    }
}

#[component]
pub fn EditAspectReview() -> Element {
    let reload = use_signal(|| 0u64);
    let params = use_signal(Vec::<(&'static str, String)>::new);
    let at = use_signal(|| 0u64);
    let sets = use_sets();
    let spec = use_hook(move || {
        let columns = vec![
            Column::render("title", &state::tr("title"), move |r| {
                let position = u(r, "position");
                let name = s(r, "title_name");
                let mut at = at;
                rsx! { span { class: "link", title: state::tr("review this one"), onclick: move |_| at.set(position), "{name}" } }
            }),
            Column::html("platform", &state::tr("platform"), |r| html::platform_badges(&s(r, "platform"))),
            Column::html("state", &state::tr("state"), |r| {
                let mut out = String::new();
                if api::b(r, "untouched") {
                    out.push_str(&badge(&state::tr("no release chosen"), "warn"));
                }
                if !api::b(r, "has_screenshot") {
                    out.push_str(&badge(&state::tr("no screenshot"), "bad"));
                }
                out
            }),
            Column::text("releases", &state::tr("releases")).num(),
        ];
        let shots = Filter::Check { key: "with_screenshots", label: state::tr("only with screenshots"), off: String::new() };
        let spec = GridSpec::new("aspect_review", columns, move |q| async move {
            let extra = vec![("with_screenshots", if q.f("with_screenshots") == "true" { "true".into() } else { String::new() })];
            let mut page = queue_page("/api/v1/review/aspects", &q, extra, params).await?;
            if let Some(items) = page.get_mut("items").and_then(|i| i.as_array_mut()) {
                for item in items {
                    let n = api::arr(item, "apply_to").len();
                    item["releases"] = json!(n);
                }
            }
            Ok(page)
        })
        .filters(review_filters(sets, vec![shots]));
        Spec(Rc::new(spec))
    });
    rsx! {
        div { class: "card", h2 { {state::tr("Aspect review")} }
            ui::Explain { p { {state::tr("Games whose aspect is a per-game question (NES and SNES: 4:3 or 8:7; PC Engine, Mega Drive, Saturn, Neo Geo: square pixels or 4:3), one title at a time: titles none of whose releases has your answer first, then the rest. A pick applies to the title's releases on that platform you have not answered. Narrow it like the Titles list, a set included.")} } }
        }
        AspectCard { params, at, reload }
        div { class: "card", Grid { spec, reload: Some(reload) } }
    }
}

#[component]
fn AspectCard(params: Signal<Vec<(&'static str, String)>>, at: Signal<u64>, reload: Signal<u64>) -> Element {
    let mut at = at;
    let mut last = use_signal(Vec::<u64>::new);
    let item = use_resource(move || {
        let (mut p, offset, _) = (params(), at(), reload());
        async move {
            p.extend([("limit", "1".to_string()), ("offset", offset.to_string())]);
            api::get(&format!("/api/v1/review/aspects{}", api::qs(&p))).await.ok()
        }
    });
    let page = item.read().clone().flatten();
    let Some(page) = page else { return rsx! { div { class: "card muted", {state::tr("loading…")} } } };
    let total = u(&page, "total");
    let undo = move |_| {
        let ids = last();
        spawn(async move {
            match api::post("/api/v1/catalog/releases/display", json!({ "release_ids": ids, "field": "aspect", "value": null })).await {
                Ok(_) => {
                    last.set(Vec::new());
                    bump(reload);
                }
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    let Some(it) = api::arr(&page, "items").first().cloned() else {
        return rsx! {
            div { class: "card",
                if total > 0 && at() > 0 {
                    {state::tr("End of the queue.")}
                    " "
                    button { class: "btn small", onclick: move |_| at.set(0), {state::tr("back to the start")} }
                } else {
                    {state::tr("Nothing to review here.")}
                }
                if !last().is_empty() { " " button { class: "btn small", onclick: undo, {state::tr("undo the last pick")} } }
            }
        };
    };
    let title_id = u(&it, "title_id");
    let apply_to: Vec<u64> = api::arr(&it, "apply_to").iter().filter_map(|x| x.as_u64()).collect();
    let picked = apply_to.clone();
    let position = at() + 1;
    let n = apply_to.len();
    let card_key = format!("{title_id}-{}", u(&it, "release_id"));
    rsx! {
        div { class: "card",
            h2 {
                CellLink { to: Route::TitleDetail { id: title_id }, text: s(&it, "title_name") }
                " "
                Html { html: html::platform_badges(&s(&it, "platform")) }
                " "
                span { class: "muted", style: "font-weight:normal", "{position} / {total}" }
            }
            div { class: "muted", style: "font-size:12px", {s(&it, "release_name")} " · " {state::trf("the pick applies to {n} release(s)", &[("n", &n.to_string())])} }
            super::display::AspectCompare {
                key: "{card_key}",
                value: api::so(&it, "value").unwrap_or_default(),
                kind: s(&it, "compare"),
                media: format!("title_id={title_id}"),
                release_ids: apply_to,
                on_change: move |_| {
                    last.set(picked.clone());
                    bump(reload);
                },
            }
            div { class: "toolbar", style: "margin-top:8px",
                button { class: "btn", onclick: move |_| at.set(at() + 1), {state::tr("Skip")} " ›" }
                " "
                button { class: "btn", disabled: at() == 0, onclick: move |_| at.set(at().saturating_sub(1)), "‹ " {state::tr("Back")} }
                if !last().is_empty() { " " button { class: "btn", onclick: undo, {state::tr("undo the last pick")} } }
            }
        }
    }
}
