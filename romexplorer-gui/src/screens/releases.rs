//! Collection › Releases and the release page (the console's `showReleases` / `showRelease`).

use std::rc::Rc;

use dioxus::prelude::*;
use serde_json::{json, Value};

use super::common::{self, release_card, release_columns, release_context, OpenWithButton};
use super::widgets::{self, MediaSlots, RaCard};
use crate::api::{self, s, u};
use crate::components::{Filter, Grid, GridSpec, SelAction, Spec};
use crate::html::{self, badge};
use crate::state;
use crate::ui::{self, Html};
use crate::Route;

fn releases_spec() -> GridSpec {
    let mut filters = vec![
        common::platform_filter(),
        common::view_filter(),
        common::kinds_filter(),
        common::material_filter(),
        Filter::Select {
            key: "source",
            label: state::tr("source"),
            options: Rc::new(|| {
                let mut o = vec![(String::new(), state::tr("all sources"))];
                o.extend(state::vocab().pointer("/jobs/dat_sources").and_then(|p| p.as_array()).into_iter().flatten().filter_map(|p| p.as_str()).map(|p| (p.to_string(), p.to_string())));
                o
            }),
        },
    ];
    filters.extend(common::facet_filters());
    GridSpec::new("releases", release_columns(), |q| async move {
        let mut p = q.base();
        p.extend([("platforms", q.f("platform")), ("kinds", common::kinds(&q)), ("material", q.f("material")), ("view", q.f("view")), ("source", q.f("source"))]);
        p.extend(common::facet_params(&q));
        api::get(&format!("/api/v1/catalog/releases{}", api::qs(&p))).await
    })
    .filters(filters)
    .link(|r| Some(Route::Release { id: u(r, "id") }))
    .local(common::held_class)
    .context(|r, sel| async move { release_context(u(&r, "id"), s(&r, "name"), sel, None, None).await })
    .select(
        |r| u(r, "id"),
        vec![
            SelAction::new(&state::tr("add to set"), |ids| async move {
                common::pick_set(json!({ "release_ids": ids }), None);
                false
            }),
            super::copy::copy_action("releases"),
            super::art::art_action("releases"),
            super::art::export_art_action("releases"),
            super::display::display_action(),
        ]
        .into_iter()
        .chain(common::release_title_actions())
        .collect(),
    )
    .cards("releases", release_card)
}

/// The kinds a tab of several lists, with the label of each.
fn material_group(kind: &str) -> &'static [(&'static str, &'static str)] {
    match kind {
        "multimedia" => &[("audio", "audio"), ("video", "video"), ("image", "images")],
        "etc" => &[("mappack", "map packs"), ("tool", "tools"), ("trainer", "trainers"), ("save", "saves")],
        _ => &[],
    }
}

/// Collection › Multimedia / Documents / Source code / Etc: what the DATs list that is not a
/// game. Releases like any other -- matched by hash, held or not -- listed here by their own
/// names and nowhere under Titles or Releases. Multimedia and Etc are several kinds each,
/// with a filter between them.
fn material_spec(kind: &'static str) -> GridSpec {
    let group = material_group(kind);
    let discs = !group.is_empty();
    // What is made for a game says which; a disc or a document has no title.
    let mut columns: Vec<crate::components::Column> = release_columns().into_iter().filter(|c| kind == "etc" || c.key != "title_name").collect();
    let mut filters = Vec::new();
    if discs {
        let label = move |m: &str| match group.iter().find(|(k, _)| *k == m) {
            Some((_, l)) => state::tr(l),
            None if m == "dlc" => state::tr("DLC installers"),
            None => m.to_string(),
        };
        columns.insert(1.min(columns.len()), crate::components::Column::html("material", &state::tr("kind"), move |r| badge(&label(&s(r, "material")), "dim")));
        filters.push(Filter::Select {
            key: "material",
            label: state::tr("kind"),
            options: Rc::new(move || {
                let mut o = vec![(String::new(), state::tr(if kind == "etc" { "all kinds" } else { "all discs" }))];
                o.extend(group.iter().map(|(k, l)| (k.to_string(), state::tr(l))));
                o
            }),
        });
    }
    filters.push(common::view_filter());
    let key = match kind {
        "multimedia" => "material_multimedia",
        "document" => "material_document",
        "etc" => "material_etc",
        _ => "material_source",
    };
    GridSpec::new(key, columns, move |q| async move {
        let mut p = q.base();
        let picked = q.f("material");
        p.extend([("material", if discs && !picked.is_empty() { picked } else { kind.to_string() }), ("view", q.f("view"))]);
        api::get(&format!("/api/v1/catalog/releases{}", api::qs(&p))).await
    })
    .filters(filters)
    .link(|r| Some(Route::Release { id: u(r, "id") }))
    .local(common::held_class)
    .context(|r, sel| async move { release_context(u(&r, "id"), s(&r, "name"), sel, None, None).await })
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
    .cards("releases", release_card)
}

#[component]
fn MaterialList(kind: &'static str) -> Element {
    let spec = use_hook(|| Spec(Rc::new(material_spec(kind))));
    rsx! { Grid { spec } }
}

#[component]
pub fn Multimedia() -> Element {
    rsx! { MaterialList { kind: "multimedia" } }
}

#[component]
pub fn Documents() -> Element {
    rsx! { MaterialList { kind: "document" } }
}

#[component]
pub fn SourceCode() -> Element {
    rsx! { MaterialList { kind: "source" } }
}

#[component]
pub fn Etc() -> Element {
    rsx! { MaterialList { kind: "etc" } }
}

/// Collection › Patches: every patch file the catalogue knows -- RetroAchievements'
/// compatibility patches (with the download its game page has), patch files a DAT lists,
/// and what is in a patch directory that is neither.
#[component]
pub fn Patches() -> Element {
    let mut reload = use_signal(|| 0u64);
    let spec = use_hook(|| {
        let from_label = |from: &str| match from {
            "ra" => "RetroAchievements".to_string(),
            "dat" => "DAT".to_string(),
            _ => state::tr("my files"),
        };
        let columns = vec![
            crate::components::Column::text("name", &state::tr("patch")),
            crate::components::Column::html("from", &state::tr("from"), move |r| badge(&from_label(&s(r, "from")), if s(r, "from") == "ra" { "warn" } else { "dim" })),
            crate::components::Column::html("platform", &state::tr("platform"), |r| api::so(r, "platform").map(|p| html::cbadge("platform", &p, None, None)).unwrap_or_default()),
            crate::components::Column::html("languages", &state::tr("languages"), common::release_lang_badges),
            crate::components::Column::render("target", &state::tr("patches"), |r| match (api::uo(r, "title_id"), api::so(r, "target")) {
                (Some(id), Some(target)) => rsx! { common::CellLink { to: Route::TitleDetail { id }, text: target } },
                (None, Some(target)) => rsx! { "{target}" },
                _ => rsx! {},
            }),
            crate::components::Column::html("size", &state::tr("size"), |r| html::fmt_size(api::uo(r, "size"))).num(),
            crate::components::Column::render("held", &state::tr("state"), move |r| {
                let (from, held, job, md5) = (s(r, "from"), api::b(r, "held"), api::so(r, "job"), s(r, "ra_md5"));
                if from != "ra" {
                    return if held { rsx! { Html { html: common::status_badges(&serde_json::json!({ "local_games": 1 }), true) } } } else { rsx! {} };
                }
                if let Some(job) = job {
                    return rsx! { Html { html: badge(&job, "warn") } };
                }
                if held {
                    return rsx! { Html { html: badge(&state::tr("downloaded"), "ok") } };
                }
                rsx! {
                    button { class: "btn small",
                        onclick: move |e| {
                            e.stop_propagation();
                            let md5 = md5.clone();
                            spawn(async move {
                                match api::post("/api/v1/ra/patches/download", json!({ "md5": md5 })).await {
                                    Ok(r) => {
                                        ui::toast(if api::b(&r, "already_queued") { state::tr("already downloading") } else { state::trf("patch download queued #{id}", &[("id", &u(&r, "job_id").to_string())]) }, false);
                                        *reload.write() += 1;
                                    }
                                    Err(e) => ui::toast(e.message, true),
                                }
                            });
                        },
                        {state::tr("download patch")}
                    }
                }
            }),
        ];
        let spec = GridSpec::new("patches", columns, |q| async move {
            let mut p = q.base();
            p.push(("from", q.f("from")));
            api::get(&format!("/api/v1/catalog/patches{}", api::qs(&p))).await
        })
        .filters(vec![Filter::Select {
            key: "from",
            label: state::tr("from"),
            options: Rc::new(|| vec![(String::new(), state::tr("all patches")), ("ra".into(), "RetroAchievements".into()), ("dat".into(), "DAT".into()), ("own".into(), state::tr("my files"))]),
        }])
        .link(|r| match s(r, "from").as_str() {
            "ra" => api::uo(r, "ra_game_id").map(|id| Route::RaGame { id }),
            "dat" => api::uo(r, "release_id").map(|id| Route::Release { id }),
            _ => api::uo(r, "file_id").map(|id| Route::File { id }),
        });
        Spec(Rc::new(spec))
    });
    rsx! { Grid { spec, reload } }
}

/// What a BIOS / firmware row is (`api::bios::KINDS`), as the console's `BIOS_KINDS` says it.
const BIOS_KINDS: [(&str, &str); 8] = [("bios", "BIOS"), ("firmware", "firmware"), ("nand", "NAND"), ("sdcard", "SD card"), ("dsp", "DSP"), ("font", "font"), ("kanji", "kanji"), ("other", "other system file")];

fn bios_kind_label(kind: &str) -> String {
    state::tr(BIOS_KINDS.iter().find(|(k, _)| *k == kind).map_or(kind, |(_, label)| label))
}

/// Collection › BIOS / firmware (the console's `showBios`): a machine's own system files,
/// from DATs and from scans (`api::bios`). None of them is a title.
#[component]
pub fn Bios() -> Element {
    let spec = use_hook(|| {
        let from_label = |from: &str| if from == "dat" { "DAT".to_string() } else { state::tr("my files") };
        let columns = vec![
            crate::components::Column::text("name", &state::tr("name")),
            crate::components::Column::html("kind", &state::tr("kind"), |r| {
                let kind = s(r, "kind");
                badge(&bios_kind_label(&kind), if kind == "nand" || kind == "sdcard" { "warn" } else { "dim" })
            }),
            crate::components::Column::html("platform", &state::tr("platform"), |r| {
                api::so(r, "platform").map(|p| html::cbadge("platform", &p, None, None)).unwrap_or_else(|| format!("<span class=\"muted\">{}</span>", html::esc(&state::tr("unknown"))))
            }),
            crate::components::Column::html("from", &state::tr("from"), move |r| {
                let source = api::so(r, "source").map(|s| format!(" <span class=\"muted\">{}</span>", html::esc(&s))).unwrap_or_default();
                format!("{}{source}", badge(&from_label(&s(r, "from")), "dim"))
            }),
            crate::components::Column::html("size", &state::tr("size"), |r| html::fmt_size(api::uo(r, "size"))).num(),
            crate::components::Column::html("status", "", |r| if api::b(r, "held") { format!("<span title=\"{}\">{}</span>", html::esc(&s(r, "path")), common::status_badges(&serde_json::json!({ "local_games": 1 }), true)) } else { String::new() }),
        ];
        let spec = GridSpec::new("bios", columns, |q| async move {
            let mut p = q.base();
            p.push(("from", q.f("from")));
            p.push(("kind", q.f("kind")));
            p.push(("platforms", q.f("platform")));
            api::get(&format!("/api/v1/catalog/bios{}", api::qs(&p))).await
        })
        .filters(vec![
            common::platform_filter(),
            Filter::Select {
                key: "kind",
                label: state::tr("kind"),
                options: Rc::new(|| std::iter::once((String::new(), state::tr("every kind"))).chain(BIOS_KINDS.iter().map(|(k, label)| (k.to_string(), state::tr(label)))).collect()),
            },
            Filter::Select {
                key: "from",
                label: state::tr("from"),
                options: Rc::new(|| vec![(String::new(), state::tr("DATs and my files")), ("dat".into(), "DAT".into()), ("own".into(), state::tr("my files"))]),
            },
        ])
        .link(|r| match s(r, "from").as_str() {
            "dat" => api::uo(r, "release_id").map(|id| Route::Release { id }),
            _ => api::uo(r, "file_id").map(|id| Route::File { id }),
        })
        .local(|r| if api::b(r, "held") { "local" } else { "" });
        Spec(Rc::new(spec))
    });
    rsx! { Grid { spec } }
}

#[component]
pub fn Releases(q: String) -> Element {
    let _ = &q;
    let facets = use_resource(|| common::load_facets(false));
    let spec = use_hook(|| Spec(Rc::new(releases_spec())));
    if facets.read().is_none() {
        return rsx! { div { class: "muted", {state::tr("loading…")} } };
    }
    rsx! { Grid { spec } }
}

#[component]
pub fn Release(id: u64) -> Element {
    rsx! { for k in [id] { ReleasePage { key: "{k}", id: k } } }
}

#[component]
fn ReleasePage(id: u64) -> Element {
    let mut gen = use_signal(|| 0u64);
    let data = use_resource(move || {
        let _ = gen();
        async move {
            let v = api::get(&format!("/api/v1/catalog/releases/{id}")).await;
            // Opening the page queues its title's metadata / art fetches (once per session).
            if v.as_ref().is_ok_and(|r| api::uo(r, "title_id").is_some()) {
                spawn(async move { widgets::page_refresh("releases", id, false).await });
            }
            v
        }
    });
    let r = match &*data.read() {
        None => return rsx! { div { class: "muted", {state::tr("loading…")} } },
        Some(Err(e)) => return rsx! { div { class: "card bad", "{e.message}" } },
        Some(Ok(r)) => r.clone(),
    };
    let title_id = api::uo(&r, "title_id");
    let name = s(&r, "name");
    let shown = state::release_display_name(&r);
    let sub = state::release_sub_name(&r);
    let mut flags: Vec<String> = Vec::new();
    for (k, l) in [("is_beta", "beta"), ("is_prototype", "prototype"), ("is_demo", "demo"), ("is_unlicensed", "unlicensed")] {
        if api::b(&r, k) {
            flags.push(state::tr(l));
        }
    }
    if api::b(&r, "community_patched") {
        flags.push(format!("{}{}", state::tr("patched"), api::so(&r, "patch_type").map(|p| format!(":{p}")).unwrap_or_default()));
    }
    for (k, l) in [("is_budget", "budget reissue"), ("is_special_edition", "special edition"), ("is_aftermarket", "aftermarket"), ("is_rerelease", "re-release (Virtual Console, ...)")] {
        if api::b(&r, k) {
            flags.push(state::tr(l));
        }
    }
    let links = api::arr(&r, "provider_links").to_vec();
    let contents = api::arr(&r, "contents").to_vec();
    let sources: std::collections::HashSet<String> = contents.iter().map(|c| s(c, "dump_source")).collect();
    // A patched work lists every version of itself: which build each file is, and which is held.
    let versions = contents.iter().any(|c| api::so(c, "variant").is_some());
    let games = api::arr(&r, "games").to_vec();
    let sets = api::arr(&r, "sets").to_vec();
    let media = api::arr(&r, "media").to_vec();
    let serials = api::arr(&r, "provider_serials").iter().map(|x| badge(&format!("{}: {}", s(x, "source"), s(x, "name")), "")).collect::<String>();
    let ratings = api::arr(&r, "ratings").iter().map(|x| badge(&format!("{} {}", s(x, "source"), s(x, "name")), "")).collect::<String>();
    let platform = api::so(&r, "platform").unwrap_or_else(|| u(&r, "platform_id").to_string());
    let dash = |v: String| if v.is_empty() { "—".to_string() } else { v };
    let (match_name, match_links) = (name.clone(), links.clone());
    let platform_id = api::uo(&r, "platform_id");
    let set_name = name.clone();
    let title_crumb = api::so(&r, "title_name").unwrap_or_else(|| state::tr("title"));
    let contents_h = state::trf("Catalog contents ({n})", &[("n", &contents.len().to_string())]);
    let superseded = api::arr(&r, "superseded").to_vec();
    let superseded_h = state::trf("Superseded files ({n})", &[("n", &superseded.len().to_string())]);
    // What is made from this release: its translations and hacks, trainers and patches.
    let patched_refs = api::arr(&r, "patched").to_vec();
    let patched_h = state::trf("Patched ROMs and extras of this release ({n})", &[("n", &patched_refs.len().to_string())]);
    let copies_h = state::trf("Local copies ({n})", &[("n", &games.len().to_string())]);
    let ra_games = api::arr(&r, "ra_games").to_vec();
    // Complete copies first, then by directory priority (Config › Directories), unless the
    // user put this release's copies in an order of their own.
    let manual_games = games.iter().any(|g| api::uo(g, "manual_position").is_some());
    let game_ids: Vec<u64> = games.iter().map(|g| u(g, "id")).collect();

    rsx! {
        super::PagePlayer { query: format!("release_id={id}") }
        div { class: "crumbs",
            a { onclick: move |_| { navigator().push(Route::Releases { q: String::new() }); }, {state::tr("Releases")} }
            " › "
            if let Some(t) = title_id { a { onclick: move |_| { navigator().push(Route::TitleDetail { id: t }); }, "{title_crumb}" } " › " }
            "{name}"
        }
        div { class: "two",
            div { class: "card",
                if let Some(c) = api::uo(&r, "cover_media_id") { img { class: "hero", src: api::media(c), alt: "" } }
                h2 {
                    "{shown}"
                    if let Some(o) = sub.clone() { " " span { class: "localname", style: "font-size:15px", "{o}" } }
                    " "
                    Html { html: common::status_badges(&r, false) }
                    " "
                    Html { html: common::release_chips(&r) }
                    " "
                    button { class: "btn small",
                        onclick: move |e| {
                            e.stop_propagation();
                            let p = e.client_coordinates();
                            let n = set_name.clone();
                            spawn(async move {
                                let items = release_context(id, n, None, None, Some(Rc::new(move || common::bump(gen)))).await;
                                ui::open_menu(p.x, p.y, items);
                            });
                        },
                        {state::tr("add to set…")}
                    }
                    " "
                    button { class: "btn small", onclick: move |_| widgets::match_dialog("releases", id, match_name.clone(), platform_id, match_links.clone(), title_id, EventHandler::new(move |_| *gen.write() += 1)), {state::tr("manual match…")} }
                    " "
                    if title_id.is_some() {
                        button { class: "btn small", title: state::tr("give this release a title of its own, apart from the others it is listed with"),
                            onclick: move |_| {
                                spawn(async move {
                                    if !ui::confirm(&state::tr("Give this release a title of its own? The two titles will not be combined again automatically.")).await { return; }
                                    match api::post(&format!("/api/v1/catalog/releases/{id}/split"), json!({})).await {
                                        Ok(r) => { navigator().push(Route::TitleDetail { id: u(&r, "title_id") }); }
                                        Err(e) => ui::toast(e.message, true),
                                    }
                                });
                            },
                            {state::tr("split into its own title")}
                        }
                        " "
                        button { class: "btn small",
                            onclick: move |_| ui::open_modal(move || rsx! { super::patched::PickTitle { heading: state::tr("Move this release to"), release_ids: vec![id], on_pick: move |title: u64| {
                                spawn(async move {
                                    match api::post(&format!("/api/v1/catalog/releases/{id}/title"), json!({ "title_id": title })).await {
                                        Ok(_) => { navigator().push(Route::TitleDetail { id: title }); }
                                        Err(e) => ui::toast(e.message, true),
                                    }
                                });
                            } } }),
                            {state::tr("move to another title…")}
                        }
                        " "
                    }
                    button { class: "btn small", title: state::tr("queue metadata/art fetches for this release's title at high priority"), onclick: move |_| { spawn(async move { widgets::page_refresh("releases", id, true).await }); }, {state::tr("refresh metadata")} }
                }
                div { class: "kv",
                    div { {state::tr("collection id")} } div { "{id} " span { class: "muted", {state::tr("(this app’s release id; DATs give their entries no ids — see sources and serial)")} } }
                    div { {state::tr("localized title")} } div { if let Some(l) = api::so(&r, "local_title") { "{l}" } else { span { class: "muted", "—" } } }
                    if !ra_games.is_empty() {
                        div { "RetroAchievements" }
                        div {
                            for (i, g) in ra_games.into_iter().enumerate() {
                                if i > 0 { br {} }
                                Html { html: "<span class=\"lb ra\">RA</span> " }
                                span { class: "link", onclick: move |_| { navigator().push(Route::RaGame { id: u(&g, "id") }); },
                                    {api::so(&g, "title").unwrap_or_else(|| state::trf("RA game {id}", &[("id", &u(&g, "id").to_string())]))}
                                }
                                if let Some(n) = api::uo(&g, "achievements") { " " span { class: "muted", {state::trf("{n} achievements", &[("n", &n.to_string())])} } }
                            }
                        }
                    }
                    div { {state::tr("platform")} } div { Html { html: html::cbadge("platform", &platform, None, None) } }
                    div { {state::tr("region")} } div { Html { html: dash(html::region_badges(&s(&r, "region"))) } }
                    div { {state::tr("languages")} }
                    div {
                        Html { html: dash(common::release_lang_badges(&r)) }
                        super::patched::LangTools { id, release: r.clone(), on_change: move |_| *gen.write() += 1 }
                    }
                    if super::patched::has_base_row(&r) {
                        div { {state::tr("made from")} }
                        div { super::patched::BaseRow { id, release: r.clone(), on_change: move |_| *gen.write() += 1 } }
                    }
                    div { {state::tr("year / date")} } div { "{dash(s(&r, \"year\"))} " if let Some(d) = api::so(&r, "released_on") { "· {d}" } }
                    div { {state::tr("revision")} } div { "{dash(s(&r, \"revision\"))}" }
                    div { {state::tr("serial")} } div { "{dash(s(&r, \"serial\"))} " Html { html: serials } }
                    div { {state::tr("category")} } div { "{dash(s(&r, \"category\"))}" }
                    div { {state::tr("discs")} } div { "{s(&r, \"disc_count\")}" }
                    div { {state::tr("players")} } div { "{dash(s(&r, \"min_players\"))}–{dash(s(&r, \"max_players\"))}" }
                    div { {state::tr("sources")} } div { Html { html: dash(html::source_badges(&s(&r, "source"))) } }
                    div { {state::tr("flags")} } div { if flags.is_empty() { "—" } for f in flags { Html { html: badge(&f, "warn") } } }
                    div { {state::tr("ratings")} } div { if ratings.is_empty() { "—" } else { Html { html: ratings } } }
                    div { {state::tr("sets")} }
                    div {
                        if sets.is_empty() { "—" }
                        for (i, x) in sets.into_iter().enumerate() {
                            if i > 0 { ", " }
                            span { class: "link", onclick: move |_| { navigator().push(Route::Set { id: u(&x, "id") }); }, "{s(&x, \"name\")}" }
                            " " span { class: "muted", "#{s(&x, \"position\")}" }
                        }
                    }
                }
            }
            div { class: "card",
                h2 { {state::tr("Web pages")} }
                if links.is_empty() {
                    span { class: "muted", {state::tr("none")} }
                } else {
                    table { class: "grid",
                        for p in links.clone() {
                            tr {
                                td { Html { html: html::provider_icon(&s(&p, "provider")) } " {s(&p, \"provider\")}" }
                                td { {widgets::link_url_cell(&p)} }
                                td { Html { html: badge(&s(&p, "confidence"), "dim") } if s(&p, "via") == "title" { " " Html { html: badge(&state::tr("via title"), "dim") } } }
                            }
                        }
                    }
                }
                h2 { style: "margin-top:12px", {state::tr("Media")} " "
                    // One provider's art for just this release (how TheGamesDB, with its small
                    // allowance, is meant to be used), or this release's art into an emulator.
                    button { class: "btn small", onclick: move |_| super::art::art_dialog(super::copy::release_selection(vec![id]), state::tr("this release")), {state::tr("download art…")} }
                    " "
                    button { class: "btn small", onclick: move |_| super::art::export_dialog(super::copy::release_selection(vec![id]), state::tr("this release")), {state::tr("export art…")} }
                }
                MediaSlots { kind: "releases", id, list: media, on_change: move |_| *gen.write() += 1 }
            }
        }
        super::display::DisplayCard { values: api::arr(&r, "display").to_vec(), release_ids: vec![id], note: String::new(), media: format!("release_id={id}"), on_change: move |_| *gen.write() += 1 }
        widgets::MediaStrip { query: format!("release_id={id}"), stamp: gen() }
        div { class: "card",
            h2 {
                "{copies_h} "
                if manual_games {
                    Html { html: badge(&state::tr("manual order"), "warn") }
                    " "
                    button { class: "btn small", title: state::tr("back to the default order"),
                        onclick: move |_| { spawn(async move { match api::delete(&format!("/api/v1/catalog/releases/{id}/games/order")).await { Ok(_) => *gen.write() += 1, Err(e) => ui::toast(e.message, true) } }); },
                        {state::tr("reset order")}
                    }
                }
            }
            if games.is_empty() { span { class: "muted", {state::tr("this release is not held locally")} } }
            for (i, g) in games.into_iter().enumerate() {
                LocalGame { key: "{u(&g, \"id\")}", game: g, release: r.clone(), up: (i > 0).then(|| if i >= 2 { game_ids[i - 2] } else { 0 }), down: game_ids.get(i + 1).copied(), on_change: move |_| *gen.write() += 1 }
            }
        }
        RaCard { links }
        div { class: "card",
            h2 { "{contents_h}" }
            if sources.len() > 1 { p { class: "muted", {state::tr("Listed by several DATs: each source’s file list is one dump of this release (e.g. redump cue/bin, mame-redump CHD); a local copy matching any of them is this release.")} } }
            if versions { p { class: "muted", {state::tr("Versions of one patched work: holding any one of them is holding this release.")} } }
            table { class: "grid",
                thead { tr { th { {state::tr("dump")} } if versions { th { {state::tr("version")} } } th { {state::tr("file")} } th { {state::tr("size")} } th { {state::tr("platform")} } th { {state::tr("type")} } th { {state::tr("hashes")} } } }
                tbody {
                    if contents.is_empty() { tr { td { colspan: "7", class: "muted", {state::tr("none")} } } }
                    for c in contents.clone() {
                        tr {
                            td { Html { html: html::source_badges(&api::so(&c, "dump_sources").or_else(|| api::so(&c, "dump_source")).unwrap_or_default()) } }
                            if versions {
                                td {
                                    "{s(&c, \"variant\")} "
                                    if api::b(&c, "held") { Html { html: badge(&state::tr("held"), "ok") } }
                                }
                            }
                            td {
                                "{s(&c, \"file_name\")} "
                                if api::b(&c, "partial") { Html { html: badge(&state::tr("partial"), "dim") } }
                                if u(&c, "member_count") > 0 { Html { html: badge(&state::trf("×{n} members", &[("n", &u(&c, "member_count").to_string())]), "warn") } }
                            }
                            td { class: "num", "{html::fmt_size(api::uo(&c, \"size\"))}" }
                            td { Html { html: html::cbadge("platform", &s(&c, "platform"), None, None) } }
                            td { "{s(&c, \"media_type\")}" }
                            td { Html { html: html::hashes_html(c.get("hashes")) } }
                        }
                    }
                }
            }
        }
        // Files a newer DAT changed or dropped: kept, shown, not counted.
        if !superseded.is_empty() {
            div { class: "card",
                h2 { "{superseded_h}" }
                p { class: "muted", {state::tr("A newer DAT changed or no longer lists these files. They no longer count toward this release; a DAT that lists them again puts them back.")} }
                table { class: "grid",
                    thead { tr { th { {state::tr("dump")} } th { {state::tr("file")} } th { {state::tr("size")} } th { {state::tr("hashes")} } th { {state::tr("superseded by")} } } }
                    tbody {
                        for c in superseded {
                            tr {
                                td { Html { html: html::source_badges(&s(&c, "dump_source")) } }
                                td {
                                    "{s(&c, \"file_name\")} "
                                    if api::b(&c, "held") { Html { html: badge(&state::tr("held"), "ok") } }
                                }
                                td { class: "num", "{html::fmt_size(api::uo(&c, \"size\"))}" }
                                td { Html { html: html::hashes_html(c.get("hashes")) } }
                                td { "{s(&c, \"superseded_by\")} " span { class: "muted", "{html::fmt_date(&s(&c, \"superseded_at\"))}" } }
                            }
                        }
                    }
                }
            }
        }
        if !patched_refs.is_empty() {
            div { class: "card",
                h2 { "{patched_h}" }
                super::patched::PatchedRefs { items: patched_refs }
            }
        }
        // The title's DLC and updates on this release's platform, its own region's first.
        for (heading, spec) in common::material_sections(api::arr(&r, "addons")) {
            div { class: "card", key: "{heading}",
                h2 { "{heading}" }
                Grid { spec: Spec(spec) }
            }
        }
    }
}

/// Puts a local copy after another one of its release (`after` 0: first), the release's own
/// order from then on.
fn move_game(release_id: u64, game_id: u64, after: u64, on_change: EventHandler<()>) {
    spawn(async move {
        let body = serde_json::json!({ "game_id": game_id, "after_game_id": if after == 0 { None } else { Some(after) } });
        match api::post(&format!("/api/v1/catalog/releases/{release_id}/games/move"), body).await {
            Ok(_) => on_change.call(()),
            Err(e) => ui::toast(e.message, true),
        }
    });
}

/// "serial match", for a copy linked by a serial rather than by its files' hashes
/// (`database::serial_match`); the title says which serial, and from where. Empty otherwise.
fn serial_match_badge(game: &Value) -> String {
    let Some(origin) = api::so(game, "serial_match") else { return String::new() };
    let serial = api::so(game, "serial_matched").unwrap_or_default();
    let title = match origin.as_str() {
        "manifest" => "Matched by the serial {serial} in the manifest this app wrote, not by its files' hashes.",
        "file_name" => "Matched by the serial {serial} in its file name, not by its files' hashes.",
        _ => "Matched by the serial {serial} in its header, not by its files' hashes.",
    };
    format!(r#"<span class="badge warn" title="{}">{}</span>"#, html::esc(&state::trf(title, &[("serial", &serial)])), html::esc(&state::tr("serial match")))
}

/// One local copy: `up` / `down` are the moves its ▲▼ make (`up`: the copy to go after, 0 for
/// first; `down`: the copy to go after), `None` at either end.
#[component]
fn LocalGame(game: Value, release: Value, up: Option<u64>, down: Option<u64>, on_change: EventHandler<()>) -> Element {
    let release_id = u(&release, "id");
    let serial = serial_match_badge(&game);
    // A serial match has no DAT hash to be complete against: its own badge says so instead.
    let complete = match game.get("complete").and_then(|c| c.as_bool()).filter(|_| serial.is_empty()) {
        Some(true) => badge(&state::tr("complete"), "ok"),
        Some(false) if u(&game, "contents_required") > 0 => badge(&state::trf("incomplete {p}/{r}", &[("p", &u(&game, "contents_present").to_string()), ("r", &u(&game, "contents_required").to_string())]), "warn"),
        Some(false) => badge(&state::tr("incomplete"), "warn"),
        None => String::new(),
    };
    let missing = u(&game, "missing_files");
    let dir = api::so(&game, "directory_path");
    let pills = common::status_badges(&serde_json::json!({ "local_games": 1, "ra": api::b(&release, "ra") }), false);
    let gid = u(&game, "id");
    let gname = s(&game, "name");
    let rname = s(&release, "name");
    let shown = { let d = state::release_display_name(&release); if d.is_empty() { rname.clone() } else { d } };
    let playlist = game.get("playlist").filter(|p| !p.is_null()).cloned();
    let roms = api::arr(&game, "roms").to_vec();
    let game_no = state::trf("game #{id}", &[("id", &gid.to_string())]);
    let scanned_as = state::trf("scanned as {name}", &[("name", &gname)]);
    rsx! {
        div { class: "card",
            h3 {
                if up.is_some() || down.is_some() {
                    button { class: "btn small", title: state::tr("move up"), disabled: up.is_none(), onclick: move |_| if let Some(a) = up { move_game(release_id, gid, a, on_change) }, "▲" }
                    button { class: "btn small", title: state::tr("move down"), disabled: down.is_none(), onclick: move |_| if let Some(a) = down { move_game(release_id, gid, a, on_change) }, "▼" }
                    " "
                }
                Html { html: pills }
                " {shown} "
                Html { html: html::region_badges(&s(&game, "region")) }
                if let Some(c) = api::so(&game, "category") { " " Html { html: badge(&c, "dim") } }
                " " Html { html: complete }
                if !serial.is_empty() { " " Html { html: serial.clone() } }
                if missing > 0 { " " Html { html: badge(&state::trf("missing {n}", &[("n", &missing.to_string())]), "bad") } }
                " " span { class: "muted", "{game_no}" }
            }
            if let Some(d) = dir { div { class: "muted mono", title: state::tr("the directory it is in (its priority: Config › Directories)"), "{d}" } }
            if gname != rname { div { class: "localname", "{scanned_as}" } }
            div { class: "toolbar",
                super::sheets::SheetButton { game_id: Some(gid), target: romexplorer_types::sheets::SheetTarget::Sheet }
                super::sheets::SheetButton { game_id: Some(gid), target: romexplorer_types::sheets::SheetTarget::M3u }
            }
            if let Some(p) = playlist {
                div { class: "muted", {state::tr("playlist: ")} span { class: "mono", "{s(&p, \"path\")}" } " "
                    OpenWithButton { label: state::tr("▶ open game"), head: gname.clone(), target: vec![("game_id", gid.to_string())] }
                }
            }
            for rom in roms {
                {
                    let rid = u(&rom, "id");
                    let path = s(&rom, "main_path");
                    let files = api::arr(&rom, "files").to_vec();
                    let disc = api::uo(&rom, "disc_number");
                    let head = common::file_name(&path);
                    let mut spec = GridSpec::new(&format!("romfiles_{rid}"), widgets::file_columns(), widgets::static_fetch(files)).hover(widgets::archive_hover);
                    spec.bare = true;
                    spec.search = false;
                    spec.page_size = 500;
                    let head2 = head.clone();
                    rsx! {
                        div { style: "margin:8px 0 4px",
                            oncontextmenu: move |e| {
                                e.prevent_default();
                                let p = e.client_coordinates();
                                let head = head2.clone();
                                spawn(async move {
                                    let mut items = vec![ui::MenuItem::head(head)];
                                    items.extend(common::open_with_items(vec![("rom_id", rid.to_string())]).await);
                                    ui::open_menu(p.x, p.y, items);
                                });
                            },
                            Html { html: if let Some(d) = disc { badge(&state::trf("disc {n}", &[("n", &d.to_string())]), "") } else { badge(&state::tr("rom"), "") } }
                            " "
                            span { class: "mono link", title: state::tr("open the rom page"), onclick: move |e| common::go(Route::Rom { id: rid }, &e), "{path}" }
                            " "
                            if api::b(&rom, "alt") { Html { html: badge(&state::tr("alt"), "warn") } " " }
                            if api::b(&rom, "deleted") { Html { html: badge(&state::tr("deleted"), "bad") } " " }
                            Html { html: html::cbadge("platform", &s(&rom, "platform"), None, None) }
                            " "
                            OpenWithButton { label: state::tr("▶ open"), head, target: vec![("rom_id", rid.to_string())], title: state::tr("open with an emulator") }
                        }
                        Grid { spec: Spec(Rc::new(spec)) }
                    }
                }
            }
        }
    }
}
