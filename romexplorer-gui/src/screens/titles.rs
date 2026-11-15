//! Collection › Titles and the title page (the console's `showTitles` / `showTitle`).

use std::rc::Rc;

use dioxus::prelude::*;
use serde_json::{json, Value};

use super::common::{self, bump, release_card, release_columns, release_context};
use super::widgets::{self, BaseGameFilter, MediaSlots, RaCard, TitleMergeTools};
use crate::api::{self, s, u};
use crate::components::{Column, Filter, Grid, GridCtl, GridSpec, SelAction, Spec};
use crate::html::{self, badge, esc};
use crate::state;
use crate::ui::{self, Html, MenuItem};
use crate::Route;

fn title_name_html(r: &Value) -> String {
    let d = api::so(r, "display_name");
    let n = s(r, "name");
    let kind = common::bucket_badge(r);
    let kind = if kind.is_empty() { kind } else { format!(" {kind}") };
    match d {
        Some(d) if d != n => format!("{}{kind}<div class=\"localname\">{}</div>", esc(&d), esc(&n)),
        _ => format!("{}{kind}", esc(&n)),
    }
}

#[component]
fn BaseGameTool() -> Element {
    let ctl = use_context::<GridCtl>();
    rsx! { BaseGameFilter { value: ctl.filter("basegame"), on_pick: move |v| ctl.set_filter("basegame", v) } }
}

fn titles_spec(q: &str, reload: Signal<u64>) -> GridSpec {
    let columns = vec![
        common::status_column(),
        Column::render("name", &state::tr("title"), move |r| {
            let key = super::versions::group_key(r);
            rsx! {
                Html { html: format!("{}{}", title_name_html(r), super::versions::group_badge(r)) }
                if let Some(key) = key {
                    button { class: "btn small", title: state::tr("show another version of this set"),
                        onclick: move |e| { e.stop_propagation(); let key = key.clone(); spawn(async move { if super::versions::reroll(&key).await { bump(reload); } }); },
                        "🎲"
                    }
                }
            }
        })
        .sort("name"),
        Column::html("category", &state::tr("category"), common::title_kind_badge).sort("category"),
        Column::html("platforms", &state::tr("platforms"), |r| html::platform_badges(&s(r, "platforms"))).sort("platforms"),
        Column::html("providers", &state::tr("providers"), |r| html::provider_icons(&s(r, "providers"))).sort("providers"),
        Column::text("release_count", &state::tr("releases")).sort("release_count").num(),
        Column::html("local_size_all", &state::tr("size on disk"), |r| common::size_cell(api::uo(r, "local_size_all"), api::uo(r, "local_size_top"), u(r, "release_count") > 1)).sort("local_size_all").num(),
        Column::html("catalog_size_all", &state::tr("catalogue size (raw)"), |r| format!("<span title=\"{}\">{}</span>", esc(&state::tr(common::RAW_NOTE)), common::size_cell(api::uo(r, "catalog_size_all"), api::uo(r, "catalog_size_top"), u(r, "release_count") > 1))).sort("catalog_size_all").num(),
    ]
    .into_iter()
    .chain(common::display_columns())
    .chain([Column::text("id", &state::tr("id")).sort("id").num()])
    .collect::<Vec<_>>();
    let mut filters = vec![
        common::platform_filter(),
        common::view_filter(),
        common::held_filter(),
        common::machine_filter(),
        common::kinds_filter(),
        common::material_filter(),
        Filter::Select {
            key: "provider",
            label: state::tr("provider"),
            options: Rc::new(|| {
                let mut o = vec![(String::new(), state::tr("any provider link"))];
                o.extend(crate::state::vocab().pointer("/jobs/providers").and_then(|p| p.as_array()).into_iter().flatten().filter_map(|p| p.as_str()).map(|p| (p.to_string(), p.to_string())));
                o
            }),
        },
    ];
    filters.push(Filter::Check { key: "hide_minor", label: state::tr("hide minor versions"), off: String::new() });
    filters.push(Filter::Check { key: "one_per_group", label: state::tr("one of each version group"), off: String::new() });
    filters.extend(common::facet_filters());
    let mut spec = GridSpec::new("titles", columns, |q| async move {
        let mut p = q.base();
        let on = |k: &str| if q.f(k) == "true" { "true".to_string() } else { String::new() };
        p.extend([("platforms", q.f("platform")), ("kinds", common::kinds(&q)), ("material", q.f("material")), ("igdb", q.f("igdb")), ("basegame", q.f("basegame")), ("view", q.f("view")), ("provider", q.f("provider")), ("hide_minor", on("hide_minor")), ("one_per_group", on("one_per_group"))]);
        p.extend(common::facet_params(&q));
        api::get(&format!("/api/v1/catalog/titles{}", api::qs(&p))).await
    })
    .filters(filters)
    .link(|r| Some(Route::TitleDetail { id: u(r, "id") }))
    .local(common::held_class)
    .cards("titles", |r, _| crate::components::Card {
        title: api::so(r, "display_name").unwrap_or_else(|| s(r, "name")),
        media: api::uo(r, "cover_media_id"),
        held: common::held_class(r),
        flags: format!("{}{}{}", common::bucket_badge(r), common::status_badges(r, false), super::versions::group_badge(r)),
        sub: format!("{}{}", html::platform_badges(&s(r, "platforms")), html::provider_icons(&s(r, "providers"))),
        ..Default::default()
    })
    // A row is one kind of a title's releases (its `row_id`): a set or a copy takes only that
    // kind. What works on whole titles -- keyword tags, IGDB, merging -- takes the rows' titles.
    .context(move |r, sel| async move {
        let rows = sel.unwrap_or_else(|| vec![u(&r, "row_id")]);
        let after: Option<Rc<dyn Fn()>> = Some(Rc::new(move || bump(reload)));
        let name = api::so(&r, "display_name").unwrap_or_else(|| s(&r, "name"));
        let mut items = vec![MenuItem::head(if rows.len() > 1 { state::trf("{n} selected titles", &[("n", &rows.len().to_string())]) } else { name.clone() })];
        items.extend(common::set_menu_items(json!({ "title_row_ids": rows }), after).await);
        let (a, b, c) = (rows.clone(), rows.clone(), rows.clone());
        items.push(MenuItem::new(state::tr("Add keyword tag…"), move || {
            let rows = a.clone();
            spawn(async move {
                if common::add_keyword_tag(common::title_ids_of_rows(rows).await).await {
                    bump(reload)
                }
            });
        }));
        items.push(MenuItem::new(state::tr("Remove keyword tag…"), move || {
            let rows = b.clone();
            spawn(async move {
                if common::remove_keyword_tag(common::title_ids_of_rows(rows).await).await {
                    bump(reload)
                }
            });
        }));
        items.push(MenuItem::new(state::tr("Re-apply IGDB metadata"), move || {
            let rows = c.clone();
            spawn(async move {
                reapply_igdb(common::title_ids_of_rows(rows).await).await;
            });
        }));
        items.push(super::art::context_item("titles", u(&r, "row_id"), &name, Some(rows.clone())));
        items.push(super::art::export_context_item("titles", u(&r, "row_id"), &name, Some(rows.clone())));
        items.push(super::copy::context_item("titles", u(&r, "row_id"), &name, Some(rows)));
        if let Some(key) = super::versions::group_key(&r) {
            items.push(MenuItem::new(state::tr("Show another version of this set 🎲"), move || {
                let key = key.clone();
                spawn(async move {
                    if super::versions::reroll(&key).await {
                        bump(reload)
                    }
                });
            }));
        }
        items
    })
    .select(
        |r| u(r, "row_id"),
        vec![
            SelAction::new(&state::tr("add to set"), |rows| async move {
                common::pick_set(json!({ "title_row_ids": rows }), None);
                false
            }),
            SelAction::new(&state::tr("add keyword tag"), |rows| async move { common::add_keyword_tag(common::title_ids_of_rows(rows).await).await }),
            SelAction::new(&state::tr("remove keyword tag"), |rows| async move { common::remove_keyword_tag(common::title_ids_of_rows(rows).await).await }),
            SelAction::new(&state::tr("re-apply IGDB metadata"), |rows| async move { reapply_igdb(common::title_ids_of_rows(rows).await).await }),
            super::copy::copy_action("titles"),
            super::art::art_action("titles"),
            super::art::export_art_action("titles"),
            SelAction::new(&state::tr("merge selected into one title"), |rows: Vec<u64>| async move {
                let ids = common::title_ids_of_rows(rows).await;
                if ids.len() < 2 {
                    ui::toast(state::tr("select at least two titles"), true);
                    return false;
                }
                if !ui::confirm(&state::trf("Merge {n} titles into one? Their releases, metadata and links move to the lowest id, which is renamed by the canonical name priority (Config › Settings).", &[("n", &ids.len().to_string())])).await {
                    return false;
                }
                match api::post("/api/v1/catalog/titles/merge", json!({ "title_ids": ids })).await {
                    Ok(r) => {
                        ui::toast(state::trf("merged into title #{id}", &[("id", &u(&r, "title_id").to_string())]), false);
                        true
                    }
                    Err(e) => {
                        ui::toast(e.message, true);
                        false
                    }
                }
            }),
        ],
    )
    .toolbar(|| rsx! { BaseGameTool {} TitleMergeTools {} super::patched::PatchLinksButton {} });
    spec.initial_filters = common::parse_q(q).into_iter().filter(|(k, _)| k == "basegame" || k == "keywords").collect();
    spec
}

#[component]
pub fn Titles(q: String) -> Element {
    let reload = use_signal(|| 0u64);
    let facets = use_resource(|| common::load_facets(false));
    if facets.read().is_none() {
        return rsx! { div { class: "muted", {state::tr("loading…")} } };
    }
    // A new query string (a link to ?keywords=… / ?basegame=…) starts the list over.
    rsx! {
        for k in [q.clone()] {
            div { key: "{k}", Grid { spec: Spec(Rc::new(titles_spec(&k, reload))), reload: Some(reload) } }
        }
    }
}

// --- the title page ---

fn link_confidence_badge(conf: &str) -> String {
    badge(conf, if ["exact", "hash", "manual"].contains(&conf) { "ok" } else { "warn" })
}

#[component]
pub fn TitleDetail(id: u64) -> Element {
    rsx! { for k in [id] { TitlePage { key: "{k}", id: k } } }
}

#[component]
fn TitlePage(id: u64) -> Element {
    let mut gen = use_signal(|| 0u64);
    // The patched ROMs of the game: declared here, before the page can return early.
    // A page at a time from the server, with a search: one game can have thousands.
    let mut patched_total = use_signal(|| 0u64);
    let patched_grid = use_hook(|| {
        Rc::new(
            crate::components::GridSpec::new("title_patched", common::release_columns().into_iter().filter(|c| c.key != "title_name").collect(), move |q| async move {
                let mut p = q.base();
                p.extend([("title_id", id.to_string()), ("kinds", "hack,mod,translation".to_string()), ("material", "game".to_string()), ("view", "collection".to_string())]);
                let page = api::get(&format!("/api/v1/catalog/releases{}", api::qs(&p))).await;
                if let (Ok(page), true) = (&page, q.search.is_empty()) {
                    patched_total.set(u(page, "total"));
                }
                page
            })
            .link(|r| Some(Route::Release { id: u(r, "id") }))
            .local(common::held_class)
            .context(|r, _| async move { common::release_context(u(&r, "id"), s(&r, "name"), None, None, None).await })
            .cards("releases", common::release_card),
        )
    });
    let data = use_resource(move || {
        let _ = gen();
        async move { api::get(&format!("/api/v1/catalog/titles/{id}")).await }
    });
    let mut vgen = use_signal(|| 0u64);
    let vinfo = use_resource(move || {
        let _ = gen();
        let _ = vgen();
        async move { api::get(&format!("/api/v1/catalog/titles/{id}/versions")).await.ok() }
    });
    use_effect(move || {
        spawn(async move { widgets::page_refresh("titles", id, false).await });
    });
    let t = match &*data.read() {
        None => return rsx! { div { class: "muted", {state::tr("loading…")} } },
        Some(Err(e)) => return rsx! { div { class: "card bad", "{e.message}" } },
        Some(Ok(t)) => t.clone(),
    };
    let mut reload = move || *gen.write() += 1;
    let name = s(&t, "name");
    let shown = api::so(&t, "display_name").unwrap_or_else(|| name.clone());
    let links = api::arr(&t, "provider_links").to_vec();
    let igdb = links.iter().find(|l| s(l, "provider") == "igdb").cloned();
    // The game's own releases; what came with it (DLC, updates, a soundtrack disc) is listed
    // below them, a section per kind.
    let every_release = api::arr(&t, "releases").to_vec();
    let releases: Vec<Value> = every_release.iter().filter(|r| api::so(r, "material").is_none() && !super::patched::is_patched(r)).cloned().collect();
    // Hacks and translations of the game: releases too, but not the game's own. With them,
    // the ones IGDB lists as games, under titles of their own.
    let patched: Vec<Value> = every_release.iter().filter(|r| super::patched::is_patched(r)).cloned().collect();
    let linked = api::arr(&t, "linked").to_vec();
    let patched_h = state::trf("Patched ROMs ({n})", &[("n", &(patched_total().max(patched.len() as u64) + linked.len() as u64).to_string())]);
    let patched_spec = (!patched.is_empty()).then_some(patched_grid.clone());
    let sections = common::material_sections(&every_release);
    let manual = api::b(&t, "manual_order");
    let platform_id = releases.first().and_then(|r| api::uo(r, "platform_id"));
    let keywords = api::arr(&t, "keywords").to_vec();
    let locales = api::arr(&t, "locales").to_vec();
    let locales_locked = api::b(&t, "locales_locked");
    // The names the editor starts from: one per language and region, the user's first.
    let locale_rows: Vec<(String, String, String)> = {
        let mut seen = std::collections::HashSet::new();
        locales.iter().filter(|l| seen.insert((s(l, "language"), s(l, "region")))).map(|l| (s(l, "language"), s(l, "region"), s(l, "name"))).collect()
    };
    let alts = api::arr(&t, "alt_names").to_vec();
    let genres = widgets::tags_html(api::arr(&t, "genres"), |g| badge(&format!("{} · {}", s(g, "name"), s(g, "source")), ""));
    let franchises = widgets::tags_html(api::arr(&t, "franchises"), |g| badge(&format!("{} · {}", s(g, "name"), s(g, "source")), ""));
    let companies = widgets::tags_html(api::arr(&t, "companies"), |c| badge(&format!("{} ({}) · {}", s(c, "name"), s(c, "role"), s(c, "source")), ""));
    let basegame = api::uo(&t, "basegame_title_id");
    let derived = api::arr(&t, "derived").to_vec();
    let siblings = api::arr(&t, "siblings").to_vec();
    let bundle_games = api::arr(&t, "bundle_games").to_vec();
    let in_bundles = api::arr(&t, "in_bundles").to_vec();
    let basegame_locked = api::b(&t, "basegame_locked");
    let family_root = api::uo(&t, "family_root_id").unwrap_or(id);
    let category = s(&t, "category");
    // The editor starts from the current base game, or a collection's games.
    let edit_bases: Vec<(u64, String)> = if category == "bundle" {
        bundle_games.iter().map(|g| (u(g, "id"), s(g, "name"))).collect()
    } else {
        basegame.map(|b| vec![(b, api::so(&t, "basegame_name").unwrap_or_else(|| b.to_string()))]).unwrap_or_default()
    };
    let this_name = shown.clone();
    let media = api::arr(&t, "media").to_vec();

    // The releases, in their order; drag or ▲▼ to order them yourself.
    let move_release = Rc::new(move |row: Value, after: Option<Value>| {
        spawn(async move {
            match api::post(&format!("/api/v1/catalog/titles/{id}/release-order/move"), json!({ "release_id": u(&row, "id"), "after_release_id": after.as_ref().map(|a| u(a, "id")) })).await {
                Ok(_) => *gen.write() += 1,
                Err(e) => ui::toast(e.message, true),
            }
        });
    });
    let rel_rows = Rc::new(releases.clone());
    let order_col = {
        let rows = rel_rows.clone();
        let mv = move_release.clone();
        Column::render("order", "", move |r| {
            let i = rows.iter().position(|x| u(x, "id") == u(r, "id")).unwrap_or(0);
            let (rows_up, rows_dn, mv_up, mv_dn) = (rows.clone(), rows.clone(), mv.clone(), mv.clone());
            rsx! {
                span { class: "draghandle", title: state::tr("drag to reorder"), "⋮⋮" }
                button { class: "btn small", title: state::tr("move up"), onclick: move |e| { e.stop_propagation(); if i > 0 { mv_up(rows_up[i].clone(), if i >= 2 { Some(rows_up[i - 2].clone()) } else { None }); } }, "▲" }
                button { class: "btn small", title: state::tr("move down"), onclick: move |e| { e.stop_propagation(); if i + 1 < rows_dn.len() { mv_dn(rows_dn[i].clone(), Some(rows_dn[i + 1].clone())); } }, "▼" }
            }
        })
    };
    let mut cols = vec![order_col];
    cols.extend(release_columns());
    // Minor releases (a fuller release of this title covers them), for the grid's filter.
    let minor: Vec<u64> = vinfo.read().as_ref().and_then(|v| v.as_ref()).map(|v| api::arr(v, "minor_releases").iter().filter_map(Value::as_u64).collect()).unwrap_or_default();
    let all_releases = releases.clone();
    let minor_key = minor.iter().map(u64::to_string).collect::<Vec<_>>().join(".");
    let mut rel_spec = GridSpec::new("title_releases", cols, move |q| {
        let items: Vec<Value> = if q.f("hide_minor") == "true" { all_releases.iter().filter(|r| !minor.contains(&u(r, "id"))).cloned().collect() } else { all_releases.clone() };
        let n = items.len();
        std::future::ready(Ok(json!({ "items": items, "total": n })))
    })
    .filters(vec![Filter::Check { key: "hide_minor", label: state::tr("hide minor versions"), off: String::new() }])
        .link(|r| Some(Route::Release { id: u(r, "id") }))
        .local(common::held_class)
        .context(|r, _| async move { release_context(u(&r, "id"), s(&r, "name"), None, None, None).await })
        .select(
            |r| u(r, "id"),
            vec![SelAction::new(&state::tr("add to set"), |ids| async move {
                common::pick_set(json!({ "release_ids": ids }), None);
                false
            }), super::display::display_action()]
            .into_iter()
            .chain(common::release_title_actions())
            .collect(),
        )
        .cards("releases", release_card);
    rel_spec.search = false;
    rel_spec.page_size = 500;
    let mv = move_release.clone();
    rel_spec.reorder = Some(Rc::new(move |row, after| mv(row, after)));
    // Remounted whenever the order itself changes: keyed by the move counter alone, the list
    // was rebuilt as soon as a move was sent -- from the data still on screen -- and not again
    // when the title came back in its new order.
    let rel_key = format!("{id}-{}-{minor_key}", releases.iter().map(|r| u(r, "id").to_string()).collect::<Vec<_>>().join("."));

    let crumbs_name = shown.clone();
    let (match_name, match_links) = (name.clone(), links.clone());
    let releases_h = state::trf("Releases ({n})", &[("n", &releases.len().to_string())]);
    rsx! {
        super::PagePlayer { query: format!("title_id={id}") }
        div { class: "crumbs", a { onclick: move |_| { navigator().push(Route::Titles { q: String::new() }); }, {state::tr("Titles")} } " › {crumbs_name}" }
        div { class: "two",
            div { class: "card",
                if let Some(c) = api::uo(&t, "cover_media_id") { img { class: "hero", src: api::media(c), alt: "" } }
                h2 {
                    "{shown}"
                    if shown != name { " " span { class: "localname", style: "font-size:15px", "{name}" } }
                    " "
                    Html { html: common::title_kind_badge(&t) }
                    " "
                    Html { html: common::status_badges(&t, false) }
                    " "
                    button { class: "btn small", onclick: move |_| widgets::match_dialog("titles", id, match_name.clone(), platform_id, match_links.clone(), Some(id), EventHandler::new(move |_| *gen.write() += 1)), {state::tr("manual match…")} }
                    " "
                    button { class: "btn small", title: state::tr("queue metadata/art fetches for this title at high priority"), onclick: move |_| { spawn(async move { widgets::page_refresh("titles", id, true).await }); }, {state::tr("refresh metadata")} }
                    " "
                    button { class: "btn small", title: state::tr("this game, its base game and its ports / remakes / DLC, in the Titles list"),
                        onclick: move |e| { let r = Route::Titles { q: format!("basegame={id}") }; if e.modifiers().ctrl() || e.modifiers().meta() || e.modifiers().shift() { crate::components::open_new_tab(&r); } else { navigator().push(r); } },
                        {state::tr("base game & ports / remakes")}
                    }
                    " "
                    button { class: "btn small", title: state::tr("this game's versions in definitive order (Collection › Definitive)"), onclick: move |_| { navigator().push(Route::Family { id: family_root }); }, {state::tr("definitive version")} }
                    " "
                    button { class: "btn small", title: state::tr("titles combined into this one (Edit › Combined titles), to split them apart again"), onclick: move |_| { navigator().push(Route::EditMerges { q: format!("title={id}") }); }, {state::tr("combined titles")} }
                }
                div { class: "kv",
                    div { {state::tr("collection id")} } div { "{id} " span { class: "muted", {state::tr("(this app’s title id)")} } }
                    div { {state::tr("IGDB id")} }
                    div {
                        if let Some(l) = igdb.clone() {
                            if s(&l, "url").is_empty() { "{s(&l, \"provider_id\")}" } else { a { class: "link", href: "{s(&l, \"url\")}", target: "_blank", rel: "noopener", "{s(&l, \"provider_id\")}" } }
                            " "
                            Html { html: link_confidence_badge(&s(&l, "confidence")) }
                        } else {
                            span { class: "muted", {state::tr("not linked")} }
                        }
                    }
                    div { {state::tr("normalized")} } div { class: "mono", "{s(&t, \"norm_name\")}" }
                    div { {state::tr("base game")} }
                    div {
                        if let Some(b) = basegame {
                            span { class: "link", onclick: move |_| { navigator().push(Route::TitleDetail { id: b }); }, "{api::so(&t, \"basegame_name\").unwrap_or_else(|| b.to_string())}" }
                            " " Html { html: html::platform_badges(&s(&t, "basegame_platforms")) }
                            " " Html { html: html::cbadge("category", &category, None, None) }
                        } else { "— " }
                        " "
                        if basegame_locked { Html { html: badge(&state::tr("set by you"), "warn") } " " }
                        button { class: "btn small", title: state::tr("what this title is (port, remaster, remake, edition, …) and of which game"),
                            onclick: move |_| {
                                let (cat, bases) = (category.clone(), edit_bases.clone());
                                ui::open_modal(move || rsx! { super::definitive::EditBasegame { title_id: Some(id), category: cat.clone(), bases: bases.clone(), fixed_base: None, on_saved: move |_| *gen.write() += 1 } })
                            },
                            {state::tr("edit base game…")}
                        }
                        " "
                        button { class: "btn small", title: state::tr("make another title a port, remake, remaster or edition of this one"),
                            onclick: move |_| {
                                let me = (id, this_name.clone());
                                ui::open_modal(move || rsx! { super::definitive::EditBasegame { title_id: None, category: String::new(), bases: Vec::new(), fixed_base: Some(me.clone()), on_saved: move |_| *gen.write() += 1 } })
                            },
                            {state::tr("add port / remake…")}
                        }
                        if basegame_locked {
                            " "
                            button { class: "btn small", title: state::tr("back to the base game and category IGDB gives"), onclick: move |_| super::definitive::reset_basegame(id, move || *gen.write() += 1), {state::tr("reset to IGDB")} }
                        }
                    }
                    if !siblings.is_empty() {
                        div { {state::tr("other versions")} }
                        div { FamilyRefs { refs: siblings.clone() } }
                    }
                    if !bundle_games.is_empty() {
                        div { {state::tr("games in this collection")} }
                        div { FamilyRefs { refs: bundle_games.clone() } }
                    }
                    if !in_bundles.is_empty() {
                        div { {state::tr("in collections")} }
                        div { FamilyRefs { refs: in_bundles.clone() } }
                    }
                    // The other direction: what was made from this game.
                    if !derived.is_empty() {
                        div { {state::tr("ports, remakes and add-ons")} }
                        div {
                            for d in derived.clone() {
                                {
                                    let did = u(&d, "id");
                                    rsx! {
                                        div { key: "{did}",
                                            span { class: "link", onclick: move |_| { navigator().push(Route::TitleDetail { id: did }); }, "{s(&d, \"name\")}" }
                                            " " Html { html: html::cbadge("category", &s(&d, "category"), None, None) }
                                            " " Html { html: html::platform_badges(&s(&d, "platforms")) }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    div { {state::tr("genres")} } Html { html: genres }
                    div { {state::tr("franchises")} } Html { html: franchises }
                    div { {state::tr("companies")} } Html { html: companies }
                    div { {state::tr("keywords")} }
                    div {
                        if keywords.is_empty() { span { class: "muted", "—" } }
                        for k in keywords {
                            {
                                let kn = s(&k, "name");
                                if s(&k, "source") == "user" {
                                    rsx! {
                                        span { class: "badge ok", "{kn} "
                                            span { class: "link", title: state::tr("remove this tag"),
                                                onclick: move |_| {
                                                    let kn = kn.clone();
                                                    spawn(async move {
                                                        match api::delete(&format!("/api/v1/catalog/titles/{id}/keywords/{}", api::encode(&kn))).await {
                                                            Ok(_) => { ui::toast(state::trf("removed tag {name}", &[("name", &kn)]), false); common::forget_facets(); *gen.write() += 1; }
                                                            Err(e) => ui::toast(e.message, true),
                                                        }
                                                    });
                                                },
                                                "✕"
                                            }
                                        }
                                        " "
                                    }
                                } else {
                                    rsx! { Html { html: badge(&format!("{kn} · {}", s(&k, "source")), "") } " " }
                                }
                            }
                        }
                        button { class: "btn small", onclick: move |_| { spawn(async move { if common::add_keyword_tag(vec![id]).await { *gen.write() += 1; } }); }, {state::tr("add keyword tag…")} }
                        " "
                        button { class: "btn small", title: state::tr("adds the title: its best release, one you have first, whenever the set is opened or copied"), onclick: move |_| common::pick_set(json!({ "title_ids": [id] }), None), {state::tr("add to set…")} }
                    }
                    div { {state::tr("localized titles")} }
                    div {
                        if locales.is_empty() { "—" }
                        for l in locales {
                            {
                                let lang = s(&l, "language");
                                let region = s(&l, "region");
                                let label = format!("{}{}", html::lang_label(&lang), if region.is_empty() { String::new() } else { format!("/{region}") });
                                rsx! { div { Html { html: html::cbadge("language", &lang, Some(&label), None) } " {s(&l, \"name\")} " span { class: "muted", "· {s(&l, \"source\")}" } } }
                            }
                        }
                        div {
                            if locales_locked { Html { html: badge(&state::tr("set by you"), "warn") } " " }
                            button { class: "btn small",
                                onclick: move |_| {
                                    let rows = locale_rows.clone();
                                    ui::open_modal(move || rsx! { EditLocales { title_id: id, rows: rows.clone(), on_saved: move |_| *gen.write() += 1 } })
                                },
                                {state::tr("edit…")}
                            }
                            if locales_locked {
                                " "
                                button { class: "btn small", title: state::tr("drop your names and take IGDB’s again"),
                                    onclick: move |_| {
                                        spawn(async move {
                                            if !ui::confirm(&state::tr("Drop your localized names and take IGDB’s again?")).await { return; }
                                            match api::delete(&format!("/api/v1/catalog/titles/{id}/locales")).await {
                                                Ok(r) => {
                                                    match api::uo(&r, "job_id") { Some(j) => ui::toast(state::trf("IGDB names queued #{id}", &[("id", &j.to_string())]), false), None => ui::toast(state::tr("unlocked"), false) }
                                                    *gen.write() += 1;
                                                }
                                                Err(e) => ui::toast(e.message, true),
                                            }
                                        });
                                    },
                                    {state::tr("unlock")}
                                }
                            }
                        }
                    }
                    div { {state::tr("alternative titles")} }
                    div {
                        if alts.is_empty() { "—" }
                        for a in alts {
                            {
                                let c = s(&a, "comment");
                                let an = s(&a, "name");
                                rsx! { div { "{an} " span { class: "muted", if !c.is_empty() { "· {c} " } "· {s(&a, \"source\")} " }
                                    span { class: "link", title: state::tr("remove this alternative title"),
                                        onclick: move |_| {
                                            let an = an.clone();
                                            spawn(async move {
                                                match api::delete(&format!("/api/v1/catalog/titles/{id}/alt-names/{}", api::encode(&an))).await {
                                                    Ok(_) => { ui::toast(state::trf("removed alternative title {name}", &[("name", &an)]), false); *gen.write() += 1; }
                                                    Err(e) => ui::toast(e.message, true),
                                                }
                                            });
                                        },
                                        "✕"
                                    }
                                } }
                            }
                        }
                    }
                }
            }
            div { class: "card",
                h2 { {state::tr("Web pages")} }
                if links.is_empty() {
                    span { class: "muted", {state::tr("not matched to any provider yet")} }
                } else {
                    table { class: "grid",
                        for p in links.clone() {
                            tr {
                                td { Html { html: html::provider_icon(&s(&p, "provider")) } " {s(&p, \"provider\")}" }
                                td { {widgets::link_url_cell(&p)} }
                                td {
                                    Html { html: badge(&s(&p, "confidence"), "dim") }
                                    if s(&p, "via") == "release" { " " Html { html: badge(&state::trf("via release #{id}", &[("id", &u(&p, "release_id").to_string())]), "dim") } }
                                }
                            }
                        }
                    }
                }
                h2 { style: "margin-top:12px", {state::tr("Media")} " "
                    // One provider's art for just this title (how TheGamesDB, with its small
                    // allowance, is meant to be used), or this title's art into an emulator.
                    button { class: "btn small", onclick: move |_| super::art::art_dialog(super::copy::title_selection(vec![id]), state::tr("this title")), {state::tr("download art…")} }
                    " "
                    button { class: "btn small", onclick: move |_| super::art::export_dialog(super::copy::title_selection(vec![id]), state::tr("this title")), {state::tr("export art…")} }
                }
                MediaSlots { kind: "titles", id, list: media, on_change: move |_| reload() }
            }
        }
        super::display::DisplayCard { values: api::arr(&t, "display").to_vec(), release_ids: releases.iter().map(|r| u(r, "id")).collect(), note: if releases.len() > 1 { state::trf("its top release; a change applies to all {n} releases", &[("n", &releases.len().to_string())]) } else { String::new() }, media: format!("title_id={id}"), on_change: move |_| *gen.write() += 1 }
        if let Some(Some(v)) = vinfo.read().as_ref() {
            div { class: "card", super::versions::VersionsCard { id, info: v.clone(), on_change: move |_| *vgen.write() += 1 } }
        }
        widgets::MediaStrip { query: format!("title_id={id}"), stamp: gen() }
        div { class: "card",
            h2 {
                "{releases_h} "
                if manual {
                    Html { html: badge(&state::tr("manual order"), "warn") }
                    " "
                    button { class: "btn small", title: state::tr("back to the default order"),
                        onclick: move |_| { spawn(async move { match api::delete(&format!("/api/v1/catalog/titles/{id}/release-order")).await { Ok(_) => { ui::toast(state::tr("back to the default order"), false); *gen.write() += 1; } Err(e) => ui::toast(e.message, true) } }); },
                        {state::tr("reset order")}
                    }
                    " "
                }
                span { class: "muted", style: "font-weight:normal;font-size:12px",
                    if manual { {state::tr("your order (drag ⋮⋮ or ▲▼), then the default for the rest")} } else { {state::tr("ranked: commercial before patches / subsets, then region and language priority, then edition and version (Config › Settings); drag ⋮⋮ or ▲▼ to order them yourself")} }
                    {state::tr(" — right-click a release to add it to a set")}
                }
            }
            for k in [rel_key] {
                div { key: "{k}", Grid { spec: Spec(Rc::new(rel_spec.clone())) } }
            }
        }
        RaCard { links }
        super::patched::RaPatchesCard { title_id: id }
        if patched_spec.is_some() || !linked.is_empty() {
            div { class: "card",
                h2 { "{patched_h}" }
                if let Some(spec) = patched_spec { Grid { spec: Spec(spec) } }
                if !linked.is_empty() {
                    p { class: "muted", {state::tr("Under a title of their own:")} }
                    super::patched::PatchedRefs { items: linked }
                }
            }
        }
        for (heading, spec) in sections {
            div { class: "card", key: "{heading}",
                h2 { "{heading}" }
                Grid { spec: Spec(spec) }
            }
        }
    }
}

/// Re-applies the cached IGDB game of each of these titles (localized names included), also
/// when they have names already.
async fn reapply_igdb(ids: Vec<u64>) -> bool {
    match api::post("/api/v1/jobs", json!({ "job_type": "igdb_reapply", "title_ids": ids })).await {
        Ok(r) => {
            ui::toast(state::trf("queued #{id}", &[("id", &u(&r, "id").to_string())]), false);
            true
        }
        Err(e) => {
            ui::toast(e.message, true);
            false
        }
    }
}

/// A title's localized names, edited by hand: saved as the user's own, which IGDB re-apply
/// (one title or a batch) leaves alone until unlocked.
#[component]
fn EditLocales(title_id: u64, rows: Vec<(String, String, String)>, on_saved: EventHandler<()>) -> Element {
    let mut list = use_signal(move || rows.into_iter().enumerate().map(|(i, r)| (i, r)).collect::<Vec<_>>());
    let mut next = use_signal(|| 10_000usize);
    let save = move |_| {
        let locales: Vec<Value> = list.read().iter().map(|(_, (l, r, n))| (l.trim().to_string(), r.trim().to_string(), n.trim().to_string())).filter(|(l, _, n)| !l.is_empty() || !n.is_empty()).map(|(l, r, n)| json!({ "language": l, "region": r, "name": n })).collect();
        spawn(async move {
            match api::post(&format!("/api/v1/catalog/titles/{title_id}/locales"), json!({ "locales": locales })).await {
                Ok(_) => { ui::close_modal(); on_saved.call(()); }
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    rsx! {
        h2 { {state::tr("Localized titles")} }
        p { class: "muted", {state::tr("Saved as your own names. IGDB metadata re-apply, for this title or all, leaves them alone until you unlock them.")} }
        table { class: "grid",
            tr { th { {state::tr("language")} } th { {state::tr("region")} } th { {state::tr("name")} } th {} }
            for (key, (lang, region, name)) in list.read().clone() {
                tr { key: "{key}",
                    td { input { r#type: "text", size: "4", placeholder: "ja", value: "{lang}", oninput: move |e| { if let Some(r) = list.write().iter_mut().find(|(k, _)| *k == key) { r.1 .0 = e.value(); } } } }
                    td { input { r#type: "text", size: "4", placeholder: "JP", value: "{region}", oninput: move |e| { if let Some(r) = list.write().iter_mut().find(|(k, _)| *k == key) { r.1 .1 = e.value(); } } } }
                    td { input { r#type: "text", style: "width:100%", value: "{name}", oninput: move |e| { if let Some(r) = list.write().iter_mut().find(|(k, _)| *k == key) { r.1 .2 = e.value(); } } } }
                    td { button { class: "btn small", title: state::tr("remove"), onclick: move |_| list.write().retain(|(k, _)| *k != key), "✕" } }
                }
            }
        }
        div { class: "toolbar", button { class: "btn small", onclick: move |_| { let k = next(); next.set(k + 1); list.write().push((k, Default::default())); }, {state::tr("add a name")} } }
        div { class: "toolbar",
            button { class: "btn", onclick: save, {state::tr("save")} }
            " "
            button { class: "btn", onclick: move |_| ui::close_modal(), {state::tr("cancel")} }
        }
    }
}

/// Titles of one family on a title page, each a link with its category and platforms.
#[component]
fn FamilyRefs(refs: Vec<Value>) -> Element {
    rsx! {
        for d in refs {
            {
                let did = u(&d, "id");
                rsx! {
                    div { key: "{did}",
                        span { class: "link", onclick: move |_| { navigator().push(Route::TitleDetail { id: did }); }, "{s(&d, \"name\")}" }
                        " " Html { html: html::cbadge("category", &s(&d, "category"), None, None) }
                        " " Html { html: html::platform_badges(&s(&d, "platforms")) }
                    }
                }
            }
        }
    }
}
