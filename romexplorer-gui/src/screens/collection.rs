//! Collection › Games, Roms, Files, Contents, Sets and their pages (the console's
//! `showGames` … `showSet`).

use std::rc::Rc;

use dioxus::prelude::*;
use serde_json::{json, Value};

use super::common::{self, bump, release_card, release_columns, release_context, CellLink, OpenWithButton};
use super::widgets;
use crate::api::{self, s, u};
use crate::components::{Card, Column, Filter, Grid, GridSpec, Spec};
use crate::html::{self, badge, esc};
use crate::state;
use crate::ui::{self, Html, MenuItem};
use crate::Route;

// --- games ---

fn game_hints(r: &Value) -> String {
    let mut v = String::new();
    if api::b(r, "merge_hint") {
        v.push_str(&html::hint_icon(&state::tr("missing match found: with the other incomplete local copies of this release the dump would be complete (their files belong in one game)")));
    }
    let mf = u(r, "missing_found");
    if mf > 0 {
        v.push_str(&html::hint_icon(&state::trf("missing files found: {n} missing file(s) are held elsewhere (moved / renamed / copied) — a rescan of that directory picks them up", &[("n", &mf.to_string())])));
    }
    v
}

pub fn game_chips(r: &Value) -> String {
    let mut v = game_hints(r);
    let missing = u(r, "missing_files");
    match r.get("complete").and_then(|c| c.as_bool()) {
        Some(true) => v.push_str(&badge(&state::tr("complete"), "ok")),
        Some(false) if missing == 0 => {
            let (p, q) = (u(r, "contents_present").to_string(), u(r, "contents_required").to_string());
            v.push_str(&format!(
                "<span class=\"badge warn\" title=\"{}\">{}</span>",
                esc(&state::trf("DAT files held {p} of {q}", &[("p", &p), ("q", &q)])),
                esc(&state::trf("incomplete {p}/{q}", &[("p", &p), ("q", &q)]))
            ))
        }
        _ => {}
    }
    if api::b(r, "dat_verified") {
        v.push_str(&badge("DAT", "ok"));
    }
    if api::b(r, "hasheous_verified") {
        v.push_str(&badge(&state::tr("✓ hasheous"), "ok"));
    }
    if missing > 0 {
        v.push_str(&badge(&state::trf("missing {n}", &[("n", &missing.to_string())]), "bad"));
    }
    v
}

fn name_with_local(r: &Value) -> String {
    let n = s(r, "name");
    match api::so(r, "display_name") {
        Some(d) if d != n => format!("{}<div class=\"localname\">{}</div>", esc(&d), esc(&n)),
        _ => esc(&n),
    }
}

fn release_link_col(key: &'static str, label: &str, sort: &'static str) -> Column {
    Column::render(key, &state::tr(label), |r| match api::uo(r, "release_id") {
        Some(id) => rsx! { CellLink { to: Route::Release { id }, text: s(r, "release_name") } },
        None => rsx! { Html { html: badge(&state::tr("unmatched"), "dim") } },
    })
    .sort(sort)
}

fn games_spec() -> GridSpec {
    let columns = vec![
        Column::html("name", &state::tr("game"), name_with_local).sort("name"),
        Column::html("platform", &state::tr("platform"), |r| html::cbadge("platform", &s(r, "platform"), None, None)).sort("platform"),
        Column::html("region", &state::tr("region"), |r| html::region_badges(&s(r, "region"))).sort("region"),
        Column::html("languages", &state::tr("languages"), |r| html::lang_badges(&s(r, "languages"))).sort("languages"),
        release_link_col("release_name", "release", "release"),
        Column::render("title_name", &state::tr("title"), |r| match api::uo(r, "title_id") {
            Some(id) => rsx! { CellLink { to: Route::TitleDetail { id }, text: s(r, "title_name") } },
            None => rsx! {},
        })
        .sort("title"),
        Column::html("status", &state::tr("status"), |r| format!("{} <span class=\"statuschips\">{}</span>", common::status_badges(&json!({ "ra": api::b(r, "ra") }), false), game_chips(r))),
        Column::text("rom_count", &state::tr("roms")).sort("rom_count").num(),
        Column::text("file_count", &state::tr("files")).sort("file_count").num(),
        Column::text("id", &state::tr("id")).sort("id").num(),
    ];
    let mut filters = vec![common::platform_filter(), common::status_filter(), common::include_deleted_filter("include deleted")];
    filters.extend(common::facet_filters());
    GridSpec::new("games", columns, |q| async move {
        let mut p = q.base();
        p.extend([("platforms", q.f("platform")), ("status", q.f("status")), ("include_deleted", q.f("include_deleted"))]);
        p.extend(common::facet_params(&q));
        api::get(&format!("/api/v1/catalog/games{}", api::qs(&p))).await
    })
    .filters(filters)
    .link(|r| api::uo(r, "release_id").map(|id| Route::Release { id }))
    .context(|r, sel| async move {
        let mut items = vec![MenuItem::head(api::so(&r, "display_name").unwrap_or_else(|| s(&r, "name")))];
        items.extend(common::open_with_items(vec![("game_id", u(&r, "id").to_string())]).await);
        if let Some(rel) = api::uo(&r, "release_id") {
            items.extend(release_context(rel, s(&r, "release_name"), None, None, None).await.into_iter().skip(1));
        }
        // The game rather than its release: a copy of *this* copy, from wherever it lives.
        items.push(super::copy::context_item("games", u(&r, "id"), &api::so(&r, "display_name").unwrap_or_else(|| s(&r, "name")), sel));
        items
    })
    .select(|r| u(r, "id"), vec![super::copy::copy_action("games")])
    .cards("games", |r, _| Card {
        title: api::so(r, "display_name").or_else(|| api::so(r, "release_name")).unwrap_or_else(|| s(r, "name")),
        media: api::uo(r, "cover_media_id"),
        held: "local",
        flags: common::status_badges(&json!({ "ra": api::b(r, "ra") }), false),
        chips: game_chips(r),
        sub: format!("{}{}", html::cbadge("platform", &s(r, "platform"), None, None), html::region_badges(&s(r, "region"))),
        ..Default::default()
    })
}

#[component]
pub fn Games() -> Element {
    let facets = use_resource(|| common::load_facets(false));
    let spec = use_hook(|| Spec(Rc::new(games_spec())));
    if facets.read().is_none() {
        return rsx! { div { class: "muted", {state::tr("loading…")} } };
    }
    rsx! { Grid { spec } }
}

// --- cross-platform ---

/// One row per IGDB game, whichever platforms it is on here: the titles the merge pass keeps
/// apart (a PS4 and an Xbox One title of one game) side by side.
fn crossplatform_spec() -> GridSpec {
    let columns = vec![
        common::status_column(),
        Column::text("name", &state::tr("game")).sort("name"),
        Column::html("platforms", &state::tr("platforms"), |r| s(r, "platforms").split(", ").filter(|p| !p.is_empty()).map(|p| html::cbadge("platform", p, None, None)).collect::<Vec<_>>().join(" ")),
        Column::text("title_count", &state::tr("titles")).sort("titles").num(),
        Column::render("titles", &state::tr("titles by platform"), |r| {
            let titles = api::arr(r, "titles").to_vec();
            rsx! {
                for t in titles {
                    div { key: "{u(&t, \"id\")}",
                        Html { html: common::status_badges(&t, false) }
                        " "
                        CellLink { to: Route::TitleDetail { id: u(&t, "id") }, text: s(&t, "name") }
                        " "
                        span { class: "muted", "{s(&t, \"platforms\")}" }
                    }
                }
            }
        }),
        Column::text("releases", &state::tr("releases")).num(),
        Column::text("igdb_id", &state::tr("IGDB id")).num(),
    ];
    GridSpec::new("crossplatform", columns, |q| async move {
        let mut p = q.base();
        p.extend([("platforms", q.f("platform")), ("platform_logic", q.f("platform_logic")), ("multi", q.f("multi"))]);
        p.extend(common::DISPLAY_KEYS.iter().map(|k| (*k, q.f(k))));
        api::get(&format!("/api/v1/catalog/crossplatform{}", api::qs(&p))).await
    })
    // Here alone the platforms can also be "all of them": a game on every ticked platform.
    .filters(vec![common::platform_filter().logic("platform_logic"), Filter::Check { key: "multi", label: state::tr("on several platforms only"), off: String::new() }].into_iter().chain(common::display_filters()).collect())
    .link(|r| Some(if u(r, "title_count") == 1 { Route::TitleDetail { id: u(r, "title_id") } } else { Route::Titles { q: format!("igdb={}", s(r, "igdb_id")) } }))
    .local(|r| if u(r, "local_games") > 0 { "local" } else { "" })
    // Into a set as the game, whichever platform: the best release you have when the set is read.
    .context(|r, sel| async move {
        let ids: Vec<String> = sel.map(|v| v.iter().map(u64::to_string).collect()).unwrap_or_else(|| vec![s(&r, "igdb_id")]);
        let head = if ids.len() > 1 { state::trf("{n} selected games", &[("n", &ids.len().to_string())]) } else { s(&r, "name") };
        let mut items = vec![MenuItem::head(head)];
        items.extend(common::set_menu_items(json!({ "igdb_ids": ids }), None).await);
        items
    })
    .select(
        |r| s(r, "igdb_id").parse().unwrap_or(0),
        vec![crate::components::SelAction::new(&state::tr("add to set"), |ids: Vec<u64>| async move {
            common::pick_set(json!({ "igdb_ids": ids.iter().map(u64::to_string).collect::<Vec<_>>() }), None);
            false
        })],
    )
    .cards("crossplatform", |r, _| Card {
        title: s(r, "name"),
        media: api::uo(r, "cover_media_id"),
        held: if u(r, "local_games") > 0 { "local" } else { "" },
        flags: common::status_badges(r, false),
        sub: s(r, "platforms").split(", ").filter(|p| !p.is_empty()).map(|p| html::cbadge("platform", p, None, None)).collect::<Vec<_>>().join(" "),
        ..Default::default()
    })
}

#[component]
pub fn Crossplatform() -> Element {
    let spec = use_hook(|| Spec(Rc::new(crossplatform_spec())));
    rsx! { Grid { spec } }
}

// --- roms ---

fn roms_spec() -> GridSpec {
    let columns = vec![
        Column::html("main_path", &state::tr("rom (main file)"), |r| {
            let mut v = format!("<span class=\"mono\">{}</span>", esc(&s(r, "main_path")));
            if api::b(r, "alt") {
                v.push_str(&format!(" {}", badge(&state::tr("alt"), "dim")));
            }
            if let Some(d) = api::uo(r, "disc_number") {
                v.push_str(&format!(" {}", badge(&state::trf("disc {n}", &[("n", &d.to_string())]), "dim")));
            }
            v
        })
        .sort("path"),
        Column::html("platform", &state::tr("platform"), |r| html::cbadge("platform", &s(r, "platform"), None, None)).sort("platform"),
        Column::text("game_name", &state::tr("game")).sort("game"),
        release_link_col("release", "release", "release"),
        Column::html("status", &state::tr("status"), |r| {
            let mf = u(r, "missing_found");
            let hint = if mf > 0 { html::hint_icon(&state::trf("missing files found: {n} of its missing file(s) are held elsewhere (moved / renamed / copied)", &[("n", &mf.to_string())])) } else { String::new() };
            let st = if api::b(r, "deleted") {
                badge(&state::tr("deleted"), "bad")
            } else if api::b(r, "complete") {
                badge(&state::tr("complete"), "ok")
            } else {
                badge(&state::trf("missing {n} of {total}", &[("n", &u(r, "missing_files").to_string()), ("total", &u(r, "file_count").to_string())]), "bad")
            };
            hint + &st
        })
        .sort("missing"),
        Column::text("file_count", &state::tr("files")).sort("file_count").num(),
        Column::html("member_count", &state::tr("archive members"), |r| match u(r, "member_count") {
            0 => String::new(),
            n => n.to_string(),
        })
        .num(),
        Column::text("id", &state::tr("id")).sort("id").num(),
    ];
    GridSpec::new("roms", columns, |q| async move {
        let mut p = q.base();
        p.extend([("platforms", q.f("platform")), ("status", q.f("status")), ("include_deleted", q.f("include_deleted"))]);
        api::get(&format!("/api/v1/catalog/roms{}", api::qs(&p))).await
    })
    .filters(vec![common::platform_filter(), common::status_filter(), common::include_deleted_filter("include deleted")])
    .link(|r| Some(Route::Rom { id: u(r, "id") }))
    .context(|r, sel| async move {
        let mut items = vec![MenuItem::head(common::file_name(&s(&r, "main_path")))];
        items.extend(common::open_with_items(vec![("rom_id", u(&r, "id").to_string())]).await);
        items.push(super::copy::context_item("roms", u(&r, "id"), &common::file_name(&s(&r, "main_path")), sel));
        items
    })
    .select(|r| u(r, "id"), vec![super::copy::copy_action("roms")])
}

#[component]
pub fn Roms() -> Element {
    let spec = use_hook(|| Spec(Rc::new(roms_spec())));
    rsx! { Grid { spec } }
}

#[component]
pub fn Rom(id: u64) -> Element {
    rsx! { for k in [id] { RomPage { key: "{k}", id: k } } }
}

#[component]
fn RomPage(id: u64) -> Element {
    let data = use_resource(move || async move { api::get(&format!("/api/v1/catalog/roms/{id}")).await });
    let d = match &*data.read() {
        None => return rsx! { div { class: "muted", {state::tr("loading…")} } },
        Some(Err(e)) => return rsx! { div { class: "card bad", "{e.message}" } },
        Some(Ok(d)) => d.clone(),
    };
    let r = d.get("rom").cloned().unwrap_or_default();
    let path = s(&r, "main_path");
    let files = api::arr(&r, "files").to_vec();
    let missing = files.iter().filter(|f| api::b(f, "deleted")).count();
    let state_html = if api::b(&r, "deleted") {
        badge(&state::tr("deleted"), "bad")
    } else if missing > 0 {
        badge(&state::trf("missing {n} file(s)", &[("n", &missing.to_string())]), "bad")
    } else {
        badge(&state::tr("complete"), "ok")
    } + &if api::b(&r, "alt") { format!(" {}", badge(&state::tr("alt dump"), "warn")) } else { String::new() };
    let releases = api::arr(&d, "releases").to_vec();
    let siblings = api::arr(&d, "siblings").to_vec();
    let game_id = api::uo(&d, "game_id");
    let game_name = api::so(&d, "game_name").unwrap_or_else(|| format!("#{}", game_id.unwrap_or(0)));
    let mut spec = GridSpec::new("rom_files", widgets::file_columns(), widgets::static_fetch(files.clone())).hover(widgets::archive_hover).link(|f| Some(Route::File { id: u(f, "id") }));
    spec.bare = true;
    spec.search = false;
    spec.page_size = 500;
    let local_files_h = state::trf("Local files ({n})", &[("n", &files.len().to_string())]);
    rsx! {
        div { class: "crumbs", a { onclick: move |_| { navigator().push(Route::Roms {}); }, {state::tr("Roms")} } " › " span { class: "mono", "{common::file_name(&path)}" } }
        div { class: "card",
            h2 { class: "mono", style: "font-size:15px", "{path} "
                OpenWithButton { label: state::tr("▶ open with…"), head: state::tr("Open with"), target: vec![("rom_id", id.to_string())] }
                " "
                // Resolves up to this rom's dump: a missing cue belongs to the game, never to
                // one of the tracks scanned as roms in its absence.
                super::sheets::SheetButton { rom_id: Some(id), target: romexplorer_types::sheets::SheetTarget::Sheet }
            }
            div { class: "kv",
                div { {state::tr("rom id")} } div { "{id}" }
                div { {state::tr("platform")} } div { Html { html: { let p = html::cbadge("platform", &s(&r, "platform"), None, None); if p.is_empty() { "—".into() } else { p } } } }
                div { {state::tr("disc")} } div { "{api::uo(&r, \"disc_number\").map(|x| x.to_string()).unwrap_or_else(|| \"—\".into())}" }
                div { {state::tr("state")} } div { Html { html: state_html } }
                div { {state::tr("game")} }
                div {
                    if let Some(g) = game_id { "{game_name} " span { class: "muted", "#{g}" } } else { "—" }
                }
                div { {state::tr("releases")} }
                div {
                    if releases.is_empty() { span { class: "muted", {state::tr("not matched to a DAT release")} } }
                    for (i, x) in releases.into_iter().enumerate() {
                        if i > 0 { ", " }
                        span { class: "link", onclick: move |e| common::go(Route::Release { id: u(&x, "id") }, &e), "{s(&x, \"name\")}" }
                    }
                }
                if !siblings.is_empty() {
                    div { {state::tr("other discs")} }
                    div {
                        for (i, x) in siblings.into_iter().enumerate() {
                            if i > 0 { " · " }
                            span { class: "link", onclick: move |e| common::go(Route::Rom { id: u(&x, "id") }, &e),
                                {api::uo(&x, "disc_number").map(|n| state::trf("disc {n}", &[("n", &n.to_string())])).unwrap_or_else(|| format!("#{}", u(&x, "id")))}
                            }
                        }
                    }
                }
            }
        }
        div { class: "card",
            h2 { "{local_files_h} " span { class: "muted", style: "font-weight:normal;font-size:12px", {state::tr("hover a file for its contents (archive members, a sheet's tracks) and their hashes")} } }
            Grid { spec: Spec(Rc::new(spec)) }
        }
    }
}

// --- contents ---

fn contents_spec() -> GridSpec {
    let columns = vec![
        Column::html("file_name", &state::tr("content (first seen as)"), common::member_name).sort("name"),
        Column::html("size", &state::tr("size"), |r| html::fmt_size(api::uo(r, "size"))).sort("size").num(),
        Column::html("platform", &state::tr("platform"), |r| html::cbadge("platform", &s(r, "platform"), None, None)).sort("platform"),
        Column::html("where", &state::tr("held as"), |r| {
            let mut v = Vec::new();
            if u(r, "local_files") > 0 {
                v.push(badge(&state::trf("{n} file(s)", &[("n", &u(r, "local_files").to_string())]), "ok"));
            }
            if u(r, "parents") > 0 {
                v.push(badge(&state::trf("inside {n} archive(s)", &[("n", &u(r, "parents").to_string())]), "ok"));
            }
            if u(r, "member_count") > 0 {
                v.push(badge(&state::trf("{n} member(s)", &[("n", &u(r, "member_count").to_string())]), "dim"));
            }
            v.join(" ")
        }),
        Column::html("releases", &state::tr("DAT releases"), |r| match u(r, "releases") {
            0 => badge(&state::tr("none"), "dim"),
            n => n.to_string(),
        })
        .num(),
        Column::html("hashes", &state::tr("hashes"), |r| html::hashes_html(r.get("hashes"))),
        Column::text("content_id", &state::tr("id")).sort("id").num(),
    ];
    GridSpec::new("contents", columns, |q| async move {
        let mut p = q.base();
        let view = q.f("view");
        p.extend([("platforms", q.f("platform")), ("view", if view.is_empty() { "local".into() } else { view })]);
        api::get(&format!("/api/v1/catalog/contents{}", api::qs(&p))).await
    })
    .filters(vec![
        Filter::Select { key: "view", label: state::tr("which"), options: Rc::new(|| vec![("local".into(), state::tr("held locally (files + archive members)")), ("all".into(), state::tr("all contents (incl. DAT-only)"))]) },
        common::platform_filter(),
    ])
    .link(|r| Some(Route::Content { id: u(r, "content_id") }))
}

#[component]
pub fn Contents() -> Element {
    let spec = use_hook(|| Spec(Rc::new(contents_spec())));
    rsx! {
        div { class: "card", ui::Explain { p { {state::tr("Contents are files’ bytes, whatever they are called and wherever they are: a file on disk, or a member inside an archive (a CHD’s tracks, a zip’s files — hashed from the stream on scan / rehash). Search by name, or paste a hash.")} } } }
        Grid { spec }
    }
}

fn file_rows(list: &[Value]) -> Element {
    rsx! {
        for f in list.to_vec() {
            tr { class: "click", onclick: move |e| common::go(Route::File { id: u(&f, "id") }, &e),
                td { class: "mono", "{s(&f, \"path\")}" }
                td { Html { html: if api::b(&f, "deleted") { badge(&state::tr("missing"), "bad") } else { badge(&state::tr("present"), "ok") } } }
            }
        }
    }
}

fn content_rows(list: &[Value]) -> Element {
    rsx! {
        for m in list.to_vec() {
            tr { class: "click", onclick: move |e| common::go(Route::Content { id: u(&m, "content_id") }, &e),
                td { class: "mono", Html { html: common::member_name(&m) } }
                td { class: "num", "{html::fmt_size(api::uo(&m, \"size\"))}" }
                td { Html { html: html::hashes_html(m.get("hashes")) } }
            }
        }
    }
}

#[component]
pub fn Content(id: u64) -> Element {
    rsx! { for k in [id] { ContentPage { key: "{k}", id: k } } }
}

#[component]
fn ContentPage(id: u64) -> Element {
    let data = use_resource(move || async move { api::get(&format!("/api/v1/catalog/contents/{id}")).await });
    let d = match &*data.read() {
        None => return rsx! { div { class: "muted", {state::tr("loading…")} } },
        Some(Err(e)) => return rsx! { div { class: "card bad", "{e.message}" } },
        Some(Ok(d)) => d.clone(),
    };
    let c = d.get("content").cloned().unwrap_or_default();
    let canon = api::so(&d, "canonical_name");
    let title = canon.clone().or_else(|| api::so(&c, "file_name")).unwrap_or_else(|| format!("#{}", u(&c, "content_id")));
    let files = api::arr(&d, "files").to_vec();
    let parents = api::arr(&d, "parents").to_vec();
    let parent_files = api::arr(&d, "parent_files").to_vec();
    let members = api::arr(&d, "members").to_vec();
    let releases = api::arr(&d, "releases").to_vec();
    let hashes = html::hashes_html(c.get("hashes"));
    let platform = html::cbadge("platform", &s(&c, "platform"), None, None);
    let local_files_h = state::trf("Local files ({n})", &[("n", &files.len().to_string())]);
    let inside_h = state::trf("Inside archives ({n})", &[("n", &parents.len().to_string())]);
    let members_h = state::trf("Members ({n})", &[("n", &members.len().to_string())]);
    let dat_releases_h = state::trf("DAT releases ({n})", &[("n", &releases.len().to_string())]);
    rsx! {
        div { class: "crumbs", a { onclick: move |_| { navigator().push(Route::Contents {}); }, {state::tr("Contents")} } " › {title}" }
        div { class: "card",
            h2 { "{canon.clone().or_else(|| api::so(&c, \"file_name\")).unwrap_or_default()}" }
            div { class: "kv",
                div { {state::tr("canonical name")} } div { "{canon.clone().unwrap_or_else(|| \"—\".into())} " span { class: "muted", {state::tr("(the name DATs give these bytes)")} } }
                div { {state::tr("first seen as")} } div { class: "mono", "{s(&c, \"file_name\")}" }
                div { {state::tr("content id")} } div { "{u(&c, \"content_id\")}" }
                div { {state::tr("size")} } div { "{html::fmt_size(api::uo(&c, \"size\"))}" }
                div { {state::tr("platform")} } div { Html { html: if platform.is_empty() { "—".into() } else { platform } } }
                div { {state::tr("hashes")} } div { Html { html: if hashes.is_empty() { "—".into() } else { hashes } } }
            }
        }
        div { class: "card",
            h2 { "{local_files_h}" }
            table { class: "grid", tbody {
                if files.is_empty() { tr { td { class: "muted", {state::tr("not held as a file of its own")} } } }
                {file_rows(&files)}
            } }
        }
        if !parents.is_empty() {
            div { class: "card",
                h2 { "{inside_h}" }
                table { class: "grid",
                    thead { tr { th { {state::tr("archive (path of this member inside it)")} } th { {state::tr("size")} } th { {state::tr("hashes")} } } }
                    tbody { {content_rows(&parents)} }
                }
                if !parent_files.is_empty() {
                    h3 { {state::tr("Their local files")} }
                    table { class: "grid", tbody { {file_rows(&parent_files)} } }
                }
            }
        }
        if !members.is_empty() {
            div { class: "card",
                h2 { "{members_h}" }
                table { class: "grid",
                    thead { tr { th { {state::tr("path inside")} } th { {state::tr("size")} } th { {state::tr("hashes")} } } }
                    tbody { {content_rows(&members)} }
                }
            }
        }
        div { class: "card",
            h2 { "{dat_releases_h}" }
            table { class: "grid",
                thead { tr { th { {state::tr("release")} } th { {state::tr("platform")} } th { {state::tr("file name in the DAT")} } th { {state::tr("source")} } } }
                tbody {
                    if releases.is_empty() { tr { td { colspan: "4", class: "muted", {state::tr("no DAT lists these bytes")} } } }
                    for r in releases {
                        tr { class: "click", onclick: move |e| common::go(Route::Release { id: u(&r, "release_id") }, &e),
                            td { "{s(&r, \"name\")}" }
                            td { Html { html: html::cbadge("platform", &s(&r, "platform"), None, None) } }
                            td { class: "mono", "{s(&r, \"file_name\")}" }
                            td { Html { html: html::cbadge("source", &s(&r, "source"), None, None) } }
                        }
                    }
                }
            }
        }
    }
}

// --- files ---

#[component]
pub fn Files() -> Element {
    let dirs = use_resource(|| async { api::get("/api/v1/directories").await.map(|v| api::items(&v)).unwrap_or_default() });
    let spec = use_memo(move || {
        let dirs = dirs.read().clone()?;
        let mut columns = widgets::file_columns();
        columns.push(Column::html("last_modified", &state::tr("modified"), |r| html::fmt_date(&s(r, "last_modified"))).sort("last_modified"));
        columns.push(Column::text("id", &state::tr("id")).sort("id").num());
        let dirs = Rc::new(dirs);
        let spec = GridSpec::new("files", columns, |q| async move {
            let mut p = q.base();
            p.extend([("platforms", q.f("platform")), ("directory_id", q.f("directory_id")), ("include_deleted", q.f("include_deleted"))]);
            api::get(&format!("/api/v1/catalog/files{}", api::qs(&p))).await
        })
        .filters(vec![
            common::platform_filter(),
            Filter::Select {
                key: "directory_id",
                label: state::tr("directory"),
                options: Rc::new(move || {
                    let mut o = vec![(String::new(), state::tr("all directories"))];
                    o.extend(dirs.iter().map(|d| (u(d, "id").to_string(), s(d, "path"))));
                    o
                }),
            },
            common::include_deleted_filter("include missing"),
        ])
        .link(|r| Some(Route::File { id: u(r, "id") }))
        .context(|r, sel| async move {
            vec![MenuItem::head(common::file_name(&s(&r, "path"))), super::copy::context_item("files", u(&r, "id"), &common::file_name(&s(&r, "path")), sel)]
        })
        .select(|r| u(r, "id"), vec![super::copy::copy_action("files")]);
        Some(Spec(Rc::new(spec)))
    });
    let Some(spec) = spec() else { return rsx! { div { class: "muted", {state::tr("loading…")} } } };
    rsx! { Grid { spec } }
}

#[component]
pub fn File(id: u64) -> Element {
    rsx! { for k in [id] { FilePage { key: "{k}", id: k } } }
}

#[component]
fn FilePage(id: u64) -> Element {
    let mut data = use_resource(move || async move { api::get(&format!("/api/v1/catalog/files/{id}")).await });
    // "Rescan this file": the job being watched, and the page read again when it ends.
    let rescan_job = use_signal(|| None::<u64>);
    let f = match &*data.read() {
        None => return rsx! { div { class: "muted", {state::tr("loading…")} } },
        Some(Err(e)) => return rsx! { div { class: "card bad", "{e.message}" } },
        Some(Ok(f)) => f.clone(),
    };
    let path = s(&f, "path");
    let owners = api::arr(&f, "owners").to_vec();
    let members = api::arr(&f, "members").to_vec();
    let fmt = api::so(&f, "archive_format");
    let platform = html::cbadge("platform", &s(&f, "platform"), None, None);
    let hashes = html::hashes_html(f.get("hashes"));
    let members_h = state::trf("Content members ({n})", &[("n", &members.len().to_string())]);
    // A failure is a hash that no longer matches what was recorded; it outranks the date of
    // the last pass, which is still shown because it says how long the file was known good.
    let (passed, failed) = (html::fmt_date(&s(&f, "last_integrity_check")), html::fmt_date(&s(&f, "last_integrity_failure")));
    let integrity = if !failed.is_empty() {
        let since = if passed.is_empty() { String::new() } else { format!(" <span class=\"muted\">{}</span>", esc(&state::trf("last ok {date}", &[("date", &passed)]))) };
        format!("{} {}{since}", badge(&state::tr("integrity fail"), "bad"), esc(&failed))
    } else if !passed.is_empty() {
        format!("{} {}", badge(&state::tr("integrity ok"), "ok"), esc(&passed))
    } else {
        format!("<span class=\"muted\">{}</span>", esc(&state::tr("never")))
    };
    rsx! {
        div { class: "crumbs", a { onclick: move |_| { navigator().push(Route::Files {}); }, {state::tr("Files")} } " › {path}" }
        div { class: "two",
            div { class: "card",
                h2 { {state::tr("File")} }
                div { class: "toolbar",
                    button { class: "btn", r#type: "button",
                        title: state::tr("read this file again (with the rest of its ROM: a sheet's tracks, a folder's files) and match its game to a release now"),
                        onclick: move |_| super::backup::start_job(format!("/api/v1/catalog/files/{id}/rescan"), rescan_job),
                        {state::tr("Rescan this file")}
                    }
                }
                if let Some(job) = rescan_job() {
                    super::backup::JobWatch { key: "{job}", job_id: job, on_done: move |_| data.restart() }
                }
                div { class: "kv",
                    div { {state::tr("id")} } div { "{id}" }
                    div { {state::tr("path")} } div { class: "mono", "{path}" }
                    div { {state::tr("size")} } div { "{html::fmt_size(api::uo(&f, \"size\"))}" }
                    div { {state::tr("format")} } div { if let Some(x) = fmt { Html { html: html::cbadge("archive_format", &x, None, None) } } else { "—" } }
                    div { {state::tr("platform")} } div { Html { html: if platform.is_empty() { "—".into() } else { platform } } }
                    div { {state::tr("directory")} } div { "{api::uo(&f, \"directory_id\").map(|x| x.to_string()).unwrap_or_else(|| \"—\".into())}" }
                    div { {state::tr("content id")} } div { "{s(&f, \"content_id\")}" }
                    div { {state::tr("state")} } div { Html { html: if api::b(&f, "deleted") { badge(&state::tr("missing"), "bad") } else { badge(&state::tr("present"), "ok") } } }
                    div { {state::tr("modified")} } div { "{html::fmt_date(&s(&f, \"last_modified\"))}" }
                    div { {state::tr("last full check")} } div { "{html::fmt_date(&s(&f, \"last_full_check\"))}" }
                    div { {state::tr("last integrity check")} } div { Html { html: integrity } }
                    div { {state::tr("hashes")} } div { Html { html: if hashes.is_empty() { "—".into() } else { hashes } } }
                }
            }
            div { class: "card",
                h2 { {state::tr("Belongs to")} }
                if owners.is_empty() {
                    span { class: "muted", {state::tr("not part of any rom")} }
                } else {
                    table { class: "grid",
                        for o in owners {
                            tr {
                                td { class: "mono", "{s(&o, \"main_path\")}" }
                                td { if let Some(g) = api::so(&o, "game_name") { "{g} " span { class: "muted", "#{s(&o, \"game_id\")}" } } else { span { class: "muted", {state::tr("no game")} } } }
                                td {
                                    if api::arr(&o, "releases").is_empty() { span { class: "muted", {state::tr("no release linked")} } }
                                    for r in api::arr(&o, "releases").to_vec() {
                                        div {
                                            a { onclick: { let rid = u(&r, "release_id"); move |e| common::go(Route::Release { id: rid }, &e) }, "{s(&r, \"release_name\")}" }
                                            " · "
                                            if let Some(tid) = api::uo(&r, "title_id") {
                                                a { onclick: move |e| common::go(Route::TitleDetail { id: tid }, &e), "{s(&r, \"title_name\")}" }
                                            } else {
                                                span { class: "muted", {state::tr("no title")} }
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
        div { class: "card",
            h2 { "{members_h}" }
            table { class: "grid",
                thead { tr { th { {state::tr("path inside the archive")} } th { {state::tr("size")} } th { {state::tr("platform")} } th { {state::tr("hashes")} } } }
                tbody {
                    if members.is_empty() { tr { td { colspan: "4", class: "muted", {state::tr("none recorded (not an archive, or members not scanned yet — a rehash records them)")} } } }
                    for m in members {
                        tr { class: "click", onclick: move |e| common::go(Route::Content { id: u(&m, "content_id") }, &e),
                            td { class: "mono", Html { html: common::member_name(&m) } }
                            td { class: "num", "{html::fmt_size(api::uo(&m, \"size\"))}" }
                            td { Html { html: html::cbadge("platform", &s(&m, "platform"), None, None) } }
                            td { Html { html: html::hashes_html(m.get("hashes")) } }
                        }
                    }
                }
            }
        }
    }
}

// --- sets ---

#[component]
pub fn Sets() -> Element {
    let mut sets = use_resource(common::load_sets);
    let mut name = use_signal(String::new);
    let mut desc = use_signal(String::new);
    let list = sets.read().clone().unwrap_or_default();
    let server_col = list.iter().any(|x| api::uo(x, "server_size").is_some());
    // Ticked sets: copied, downloaded for or exported together.
    let mut picked = use_signal(Vec::<u64>::new);
    let all_ids: Vec<u64> = list.iter().map(|x| u(x, "id")).collect();
    let all_ticked = !all_ids.is_empty() && all_ids.iter().all(|i| picked().contains(i));
    let picked_what = state::trf("{n} selected {kind}", &[("n", &picked().len().to_string()), ("kind", &state::kind_word("sets", false))]);
    rsx! {
        div { class: "card",
            h2 { {state::tr("Collection sets")} }
            ui::Explain { p { {state::tr("An ordered list of releases, like a playlist — for copying a whole set to another machine, or just grouping. Right-click any release (Releases tab, a title's release list, Games) to add it.")} } }
            form { class: "inline",
                onsubmit: move |e| {
                    e.prevent_default();
                    let body = json!({ "name": name(), "description": if desc().is_empty() { Value::Null } else { json!(desc()) } });
                    spawn(async move {
                        match api::post("/api/v1/sets", body).await {
                            Ok(_) => { ui::toast(state::trc("done", "created"), false); name.set(String::new()); desc.set(String::new()); sets.restart(); }
                            Err(e) => ui::toast(e.message, true),
                        }
                    });
                },
                label { {state::tr("name")} input { r#type: "text", required: true, value: "{name}", oninput: move |e| name.set(e.value()) } }
                label { {state::tr("description")} input { r#type: "text", style: "min-width:300px", value: "{desc}", oninput: move |e| desc.set(e.value()) } }
                button { class: "btn primary", {state::tr("create")} }
            }
        }
        SetsFileCard { on_done: move |_| sets.restart() }
        div { class: "card",
            if !picked().is_empty() {
                div { class: "toolbar", style: "margin-bottom:6px",
                    span { class: "selbar",
                        span { class: "muted", "{picked_what}" }
                        button { class: "btn small primary", onclick: { let w = picked_what.clone(); move |_| super::copy::copy_dialog(romexplorer_types::copy::CopySelection { set_ids: picked(), ..Default::default() }, w.clone()) }, {state::tr("copy to…")} }
                        button { class: "btn small primary", onclick: { let w = picked_what.clone(); move |_| super::art::art_dialog(romexplorer_types::copy::CopySelection { set_ids: picked(), ..Default::default() }, w.clone()) }, {state::tr("download art…")} }
                        button { class: "btn small primary", onclick: { let w = picked_what.clone(); move |_| super::art::export_dialog(romexplorer_types::copy::CopySelection { set_ids: picked(), ..Default::default() }, w.clone()) }, {state::tr("export art…")} }
                        button { class: "btn small", onclick: move |_| { spawn(export_sets_file(picked())); }, {state::tr("export as file")} }
                        button { class: "btn small", onclick: move |_| picked.write().clear(), {state::tr("clear")} }
                    }
                }
            }
            table { class: "grid",
                thead { tr {
                    th { input { r#type: "checkbox", checked: all_ticked, onchange: { let all_ids = all_ids.clone(); move |e: Event<FormData>| picked.set(if e.checked() { all_ids.clone() } else { Vec::new() }) } } }
                    th { {state::trc("sets", "set")} } th { {state::tr("description")} } th { {state::tr("titles")} } th { {state::tr("releases")} } th { {state::tr("held locally")} } th { {state::tr("platforms")} } th { {state::tr("size on disk")} } if server_col { th { {state::tr("size on server")} } } th { {state::tr("catalogue size (raw)")} } th { {state::tr("updated")} } th {} } }
                tbody {
                    if list.is_empty() { tr { td { colspan: "12", class: "muted", {state::tr("no sets yet")} } } }
                    for x in list {
                        {
                            let sid = u(&x, "id");
                            let (n0, d0) = (s(&x, "name"), s(&x, "description"));
                            let n1 = n0.clone();
                            let n2 = n0.clone();
                            let n3 = n0.clone();
                            let n4 = n0.clone();
                            rsx! {
                                tr { class: "click", onclick: move |e| common::go(Route::Set { id: sid }, &e),
                                    // The clicked set, or every ticked one when it is among them.
                                    oncontextmenu: move |e| {
                                        e.prevent_default();
                                        let p = e.client_coordinates();
                                        let ticked = picked.peek().clone();
                                        let ids = ticked.contains(&sid).then_some(ticked).filter(|v| v.len() > 1);
                                        let head = match &ids { Some(v) => state::trf("{n} selected {kind}", &[("n", &v.len().to_string()), ("kind", &state::kind_word("sets", false))]), None => n4.clone() };
                                        let mut items = vec![MenuItem::head(head)];
                                        items.extend(crate::components::grid::open_items(&Route::Set { id: sid }));
                                        items.push(super::copy::context_item("sets", sid, &n4, ids.clone()));
                                        items.push(super::art::context_item("sets", sid, &n4, ids.clone()));
                                        items.push(super::art::export_context_item("sets", sid, &n4, ids));
                                        ui::open_menu(p.x, p.y, items);
                                    },
                                    td { onclick: move |e| e.stop_propagation(),
                                        input { r#type: "checkbox", checked: picked().contains(&sid),
                                            onchange: move |e: Event<FormData>| { let mut v = picked.write(); v.retain(|x| *x != sid); if e.checked() { v.push(sid); } } }
                                    }
                                    td { "{n0}" }
                                    td { class: "muted", "{d0}" }
                                    td { class: "num", "{u(&x, \"title_count\")}" }
                                    td { class: "num", "{u(&x, \"release_count\")}" }
                                    td { class: "num", "{u(&x, \"local_releases\")}/{u(&x, \"release_count\")}" }
                                    td { class: "num", "{u(&x, \"platform_count\")}" }
                                    td { class: "num", title: "{set_size_note(&x)}", "{set_size(&x)}" }
                                    if server_col { td { class: "num", title: "{server_size_note(&x)}", "{server_size(&x)}" } }
                                    td { class: "num", title: state::tr(common::RAW_NOTE), "{html::fmt_size_fine(u(&x, \"catalog_size\"))}" }
                                    td { "{html::fmt_date(&s(&x, \"updated_at\"))}" }
                                    td {
                                        button { class: "btn small",
                                            onclick: move |e| { e.stop_propagation(); super::copy::copy_dialog(super::copy::set_selection(sid), n1.clone()); },
                                            {state::tr("copy to…")}
                                        }
                                        " "
                                        button { class: "btn small",
                                            onclick: move |e| { e.stop_propagation(); super::art::art_dialog(super::copy::set_selection(sid), n2.clone()); },
                                            {state::tr("art…")}
                                        }
                                        " "
                                        button { class: "btn small",
                                            onclick: move |e| { e.stop_propagation(); super::art::export_dialog(super::copy::set_selection(sid), n3.clone()); },
                                            {state::tr("export art…")}
                                        }
                                        " "
                                        button { class: "btn small",
                                            onclick: move |e| {
                                                e.stop_propagation();
                                                let (n0, d0) = (n0.clone(), d0.clone());
                                                spawn(async move {
                                                    let Some(name) = ui::prompt(&state::tr("Set name:"), &n0).await.filter(|n| !n.is_empty()) else { return };
                                                    // Cancelled, the description stays as it was.
                                                    let description = ui::prompt(&state::tr("Description:"), &d0).await.unwrap_or(d0);
                                                    match api::put(&format!("/api/v1/sets/{sid}"), json!({ "name": name, "description": description })).await {
                                                        Ok(_) => sets.restart(),
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
                                                spawn(async move {
                                                    if !ui::confirm(&state::tr("Delete this set? (releases themselves are untouched)")).await { return; }
                                                    match api::delete(&format!("/api/v1/sets/{sid}")).await { Ok(_) => sets.restart(), Err(e) => ui::toast(e.message, true) }
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

#[component]
pub fn Set(id: u64) -> Element {
    rsx! { for k in [id] { SetPage { key: "{k}", id: k } } }
}

#[component]
fn SetPage(id: u64) -> Element {
    let reload = use_signal(|| 0u64);
    let data = use_resource(move || {
        let _ = reload();
        async move { api::get(&format!("/api/v1/sets/{id}")).await }
    });
    let spec = use_hook(move || {
        let rows: Rc<std::cell::RefCell<Vec<u64>>> = Rc::new(std::cell::RefCell::new(Vec::new()));
        let rows_col = rows.clone();
        let pos_col = Column::render("set_position", "#", move |r| {
            let rid = u(r, "id");
            let (ru, rd) = (rows_col.clone(), rows_col.clone());
            let swap = move |ids: Rc<std::cell::RefCell<Vec<u64>>>, up: bool| {
                // Only this page is known here, so the two rows swap and the rest of the set
                // stays put.
                let v = ids.borrow().clone();
                let Some(i) = v.iter().position(|x| *x == rid) else { return };
                let j = if up { i.checked_sub(1) } else { Some(i + 1).filter(|j| *j < v.len()) };
                let Some(j) = j else { return };
                let pair = [v[i], v[j]];
                spawn(async move {
                    match api::post(&format!("/api/v1/sets/{id}/reorder"), json!({ "swap": pair })).await {
                        Ok(_) => bump(reload),
                        Err(e) => ui::toast(e.message, true),
                    }
                });
            };
            let swap2 = swap.clone();
            rsx! {
                "{s(r, \"set_position\")} "
                button { class: "btn small", onclick: move |e| { e.stop_propagation(); swap(ru.clone(), true); }, "▲" }
                button { class: "btn small", onclick: move |e| { e.stop_propagation(); swap2(rd.clone(), false); }, "▼" }
            }
        })
        .sort("position")
        .num();
        // What put the row in the set: D definitive version, C cross-platform game, T title, R release.
        let type_col = Column::html("set_via", &state::tr("type"), common::entry_type_badge);
        let mut cols = vec![pos_col, type_col];
        cols.extend(release_columns());
        // The Releases tab's filters, so part of a set can be picked out (and removed at
        // once). No kinds default: a set holds what was put in it, hacks included.
        let mut filters = vec![common::platform_filter(), common::view_filter(), common::held_filter(), common::machine_filter(), common::kinds_filter()];
        filters.extend(common::facet_filters());
        // The grid's D/C/T/R badge sits on the cover, so it is shown on request.
        filters.push(Filter::Check { key: "type_badges", label: state::tr("entry type on covers"), off: String::new() });
        filters.extend(super::definitive::rank_filters());
        // Every row loaded, by id: what the selection's entries are (`common::set_rows_body`).
        let by_id: Rc<std::cell::RefCell<std::collections::HashMap<u64, Value>>> = Rc::new(std::cell::RefCell::new(std::collections::HashMap::new()));
        let (by_id_fetch, by_id_add, by_id_menu) = (by_id.clone(), by_id.clone(), by_id.clone());
        let mut spec = GridSpec::new(&format!("set_{id}"), cols, move |q| {
            let rows = rows.clone();
            let by_id = by_id_fetch.clone();
            async move {
                let mut p = q.base();
                p.extend([("platforms", q.f("platform")), ("kinds", q.f("kinds")), ("view", q.f("view"))]);
                p.extend(super::definitive::rank_filter_params(&q));
                p.extend(common::facet_params(&q));
                let mut v = api::get(&format!("/api/v1/sets/{id}/releases{}", api::qs(&p))).await?;
                // The cards read the toggle off each row.
                if q.f("type_badges") == "true" {
                    if let Some(items) = v.get_mut("items").and_then(Value::as_array_mut) {
                        for r in items {
                            r["type_badge"] = json!(true);
                        }
                    }
                }
                *rows.borrow_mut() = api::items(&v).iter().map(|r| u(r, "id")).collect();
                for r in api::items(&v) {
                    by_id.borrow_mut().insert(u(&r, "id"), r.clone());
                }
                Ok(v)
            }
        })
        .filters(filters)
        .select(
            |r| u(r, "id"),
            vec![
                crate::components::SelAction::new(&state::tr("remove from this set"), move |ids: Vec<u64>| async move {
                    if !ui::confirm(&state::trf("Remove {n} release(s) from this set? (the releases themselves are untouched)", &[("n", &ids.len().to_string())])).await {
                        return false;
                    }
                    match api::post(&format!("/api/v1/sets/{id}/remove"), json!({ "release_ids": ids })).await {
                        Ok(r) => {
                            ui::toast(state::trf("{n} release(s) removed from the set", &[("n", &u(&r, "removed").to_string())]), false);
                            // The counts and sizes above come with the set: read it again.
                            bump(reload);
                            true
                        }
                        Err(e) => {
                            ui::toast(e.message, true);
                            false
                        }
                    }
                }),
                crate::components::SelAction::new(&state::tr("add to set"), move |ids: Vec<u64>| {
                    let rows: Vec<Value> = ids.iter().filter_map(|i| by_id_add.borrow().get(i).cloned()).collect();
                    async move {
                        common::pick_set(common::set_rows_body(&rows), None);
                        false
                    }
                }),
                super::copy::copy_action("releases"),
                super::art::art_action("releases"),
                super::art::export_art_action("releases"),
                super::display::display_action(),
                super::ai::batch_action("release_ids", "releases"),
            ],
        )
        .link(|r| Some(Route::Release { id: u(r, "id") }))
        .local(common::held_class)
        .context(move |r, sel| {
            let picked: Vec<u64> = sel.clone().unwrap_or_else(|| vec![u(&r, "id")]);
            let rows: Vec<Value> = picked.iter().filter_map(|i| by_id_menu.borrow().get(i).cloned()).collect();
            async move { common::set_page_context(u(&r, "id"), s(&r, "name"), sel, id, common::set_rows_body(&rows), Some(Rc::new(move || bump(reload)))).await }
        })
        .cards("sets", |r, mode| {
            let mut card = release_card(r, mode);
            if r.get("type_badge").and_then(Value::as_bool).unwrap_or(false) {
                card.chips = format!("{}{}", common::entry_type_emblem(r), card.chips);
            }
            card
        });
        spec.page_size = 500;
        Spec(Rc::new(spec))
    });
    let st = match &*data.read() {
        None => return rsx! { div { class: "muted", {state::tr("loading…")} } },
        Some(Err(e)) => return rsx! { div { class: "card bad", "{e.message}" } },
        Some(Ok(v)) => v.clone(),
    };
    let counts = state::trf("{n} release(s), {local} held locally", &[("n", &u(&st, "release_count").to_string()), ("local", &u(&st, "local_releases").to_string())]);
    let facts = [
        state::trf("{n} title(s)", &[("n", &u(&st, "title_count").to_string())]),
        state::trf("{n} release(s)", &[("n", &u(&st, "release_count").to_string())]),
        state::trf("{n} platform(s)", &[("n", &u(&st, "platform_count").to_string())]),
        format!("{} {}", state::tr("size on disk"), set_size(&st)),
        if api::uo(&st, "server_size").is_some() { format!("{} {}", state::tr("size on server"), server_size(&st)) } else { String::new() },
        format!("{} {}", state::tr("catalogue size (raw)"), html::fmt_size_fine(u(&st, "catalog_size"))),
    ];
    let size_note = set_size_note(&st);
    rsx! {
        div { class: "crumbs", a { onclick: move |_| { navigator().push(Route::Sets {}); }, {state::tr("Sets")} } " › {s(&st, \"name\")}" }
        div { class: "card",
            h2 { "{s(&st, \"name\")} " span { class: "muted", "{counts}" } }
            div { class: "muted", "{s(&st, \"description\")}" }
            div { class: "toolbar", style: "margin-top:6px", title: "{size_note}",
                for f in facts.into_iter().filter(|f| !f.is_empty()) { span { class: "badge dim", "{f}" } }
            }
            // The whole set; the selection bar below does the same for chosen rows.
            div { class: "toolbar", style: "margin-top:6px",
                button { class: "btn small", onclick: { let n = s(&st, "name"); move |_| super::copy::copy_dialog(super::copy::set_selection(id), n.clone()) }, {state::tr("copy to…")} }
                button { class: "btn small", onclick: { let n = s(&st, "name"); move |_| super::art::art_dialog(super::copy::set_selection(id), n.clone()) }, {state::tr("download art…")} }
                button { class: "btn small", onclick: { let n = s(&st, "name"); move |_| super::art::export_dialog(super::copy::set_selection(id), n.clone()) }, {state::tr("export art…")} }
                button { class: "btn small", title: state::tr("asks the in-app AI which version to play, for every game of this set"), onclick: { let n = s(&st, "name"); move |_| super::ai::batch_dialog(json!({ "set_id": id }), n.clone()) }, {state::tr("batch rate with AI…")} }
            }
        }
        div { class: "card", Grid { spec, reload: Some(reload) } }
    }
}

/// A set's size on disk: one local copy of each release (the one a copy takes), at its
/// files' real size; "≥" when some release is not held here.
fn set_size(set: &Value) -> String {
    format!("{}{}", if u(set, "size_unknown") > 0 { "≥ " } else { "" }, html::fmt_size_fine(u(set, "total_size")))
}

/// The same on the main server (`server_size`, absent with no main server): on a client,
/// where nothing is on disk, the figure that says what a copy of the set will take.
fn server_size(set: &Value) -> String {
    match api::uo(set, "server_size") {
        Some(n) => format!("{}{}", if u(set, "server_size_unknown") > 0 { "≥ " } else { "" }, html::fmt_size_fine(n)),
        None => String::new(),
    }
}

fn server_size_note(set: &Value) -> String {
    match u(set, "server_size_unknown") {
        0 => state::tr("one copy per release, the one a copy takes, at its real size"),
        n => state::trf("{n} release(s) the server does not have are not counted", &[("n", &n.to_string())]),
    }
}

fn set_size_note(set: &Value) -> String {
    match u(set, "size_unknown") {
        0 => String::new(),
        n => state::trf("{n} release(s) not held here are not counted", &[("n", &n.to_string())]),
    }
}

/// Saves the sets `ids` (every set when empty) as a sets file.
async fn export_sets_file(ids: Vec<u64>) {
    let path = if ids.is_empty() { "/api/v1/sets/export".to_string() } else { format!("/api/v1/sets/export{}", api::qs(&[("ids", ids.iter().map(u64::to_string).collect::<Vec<_>>().join(","))])) };
    match api::backend::backend().call(api::Method::Get, &path, None).await {
        Ok(text) => {
            let day = serde_json::from_str::<Value>(&text).ok().and_then(|v| v["exported_at"].as_str().and_then(|d| d.get(..10)).map(str::to_string)).unwrap_or_else(|| "export".to_string());
            super::data::save_text(&text, &format!("romexplorer-sets-{day}.json"), "application/json").await;
        }
        Err(e) => ui::toast(e.message, true),
    }
}

/// Sets as a file: every set exported, or a file's sets brought in -- into the set of the same
/// name, made when there is none -- with what this catalogue could not find listed.
#[component]
fn SetsFileCard(on_done: EventHandler<()>) -> Element {
    let mut chosen = use_signal(|| None::<dioxus::html::FileData>);
    let mut read = use_signal(|| None::<(String, Value)>);
    let mut ticked = use_signal(std::collections::BTreeSet::<String>::new);
    let mut result = use_signal(Vec::<Value>::new);
    let mut busy = use_signal(|| false);
    let open = move |_| {
        let Some(file) = chosen() else {
            ui::toast(state::tr("choose a file"), true);
            return;
        };
        spawn(async move {
            let doc = match file.read_bytes().await {
                Ok(bytes) => serde_json::from_slice::<Value>(&bytes).ok(),
                Err(e) => {
                    ui::toast(e.to_string(), true);
                    return;
                }
            };
            let Some(doc) = doc.filter(|d| d["format"] == "romexplorer-sets") else {
                ui::toast(state::tr("That is not a romexplorer sets file."), true);
                return;
            };
            ticked.set(api::arr(&doc, "sets").iter().map(|x| s(x, "name")).collect());
            result.set(Vec::new());
            read.set(Some((file.name(), doc)));
        });
    };
    let import = move |_| {
        let Some((_, doc)) = read() else { return };
        let names: Vec<String> = ticked().into_iter().collect();
        if names.is_empty() {
            ui::toast(state::tr("tick at least one set"), true);
            return;
        }
        busy.set(true);
        spawn(async move {
            match api::post("/api/v1/sets/import", json!({ "file": doc, "names": names })).await {
                Ok(r) => {
                    ui::toast(state::tr("sets imported"), false);
                    result.set(r.as_array().cloned().unwrap_or_default());
                    read.set(None);
                    on_done.call(());
                }
                Err(e) => ui::toast(e.message, true),
            }
            busy.set(false);
        });
    };
    let missing: Vec<(String, String, String, String)> = result().iter().flat_map(|x| api::arr(x, "missing").iter().map(|m| (s(x, "name"), s(m, "kind"), s(m, "name"), s(m, "reason"))).collect::<Vec<_>>()).collect();
    rsx! {
        div { class: "card",
            h2 { {state::tr("Sets file")} }
            ui::Explain { p { {state::tr("Every set as one JSON file, its entries named so another catalogue finds them again: a release by its DAT name, platform and content, a title by its IGDB game, top release and name, a game by its IGDB game. For a server started clean, or another machine. Importing adds each set's entries, in order, to the set of the same name (made when there is none) and lists what it could not find; it never removes.")} } }
            div { class: "toolbar",
                button { class: "btn primary", onclick: move |_| { spawn(export_sets_file(Vec::new())); }, {state::tr("export all sets")} }
                span { class: "grow" }
                input { r#type: "file", accept: ".json,application/json", onchange: move |e| chosen.set(e.files().into_iter().next()) }
                button { class: "btn", onclick: open, {state::tr("import sets…")} }
            }
            if let Some((name, doc)) = read() {
                p { {state::trf("{name}: exported {date} by romexplorer {version}. Bring in:", &[("name", &name), ("date", doc["exported_at"].as_str().unwrap_or("?")), ("version", doc["app_version"].as_str().unwrap_or("?"))])} }
                div { class: "toolbar",
                    for x in api::arr(&doc, "sets").to_vec() {
                        {
                            let n = s(&x, "name");
                            let count = api::arr(&x, "entries").len();
                            let key = n.clone();
                            rsx! {
                                label { key: "{key}",
                                    input { r#type: "checkbox", checked: ticked().contains(&n),
                                        onchange: move |e| { let mut t = ticked(); if e.checked() { t.insert(n.clone()); } else { t.remove(&n); } ticked.set(t); } }
                                    " {key} "
                                    span { class: "muted", "({count})" }
                                }
                            }
                        }
                    }
                    span { class: "grow" }
                    button { class: "btn primary", disabled: busy(), onclick: import, {state::tr("import")} }
                }
            }
            if !result().is_empty() {
                table { class: "grid",
                    thead { tr { th { {state::trc("sets", "set")} } th { class: "num", {state::tr("added")} } th { class: "num", {state::tr("already there")} } th { class: "num", {state::tr("not found")} } } }
                    tbody {
                        for x in result() {
                            tr { key: "{u(&x, \"set_id\")}",
                                td { "{s(&x, \"name\")}" if api::b(&x, "created") { " " Html { html: badge(&state::tr("new"), "ok") } } }
                                td { class: "num", "{u(&x, \"added\")}" }
                                td { class: "num", "{u(&x, \"already\")}" }
                                td { class: "num", "{api::arr(&x, \"missing\").len()}" }
                            }
                        }
                    }
                }
            }
            if !missing.is_empty() {
                table { class: "grid",
                    thead { tr { th { {state::trc("sets", "set")} } th { {state::tr("kind")} } th { {state::tr("not found")} } th { {state::tr("why")} } } }
                    tbody {
                        for (i, (set, kind, name, why)) in missing.into_iter().enumerate() {
                            tr { key: "{i}", td { "{set}" } td { "{kind}" } td { class: "mono", "{name}" } td { class: "bad", "{why}" } }
                        }
                    }
                }
            }
        }
    }
}
