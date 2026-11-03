//! Collection › Definitive: one row per game with its ports, remakes, remasters, editions and
//! the collections that hold it, led by its definitive version (or, toggled, its base game);
//! the family's page, its versions in order; and the base game editor of a title page (the
//! console's `showDefinitive` / `showFamily` / `editBasegame`).

use std::rc::Rc;

use dioxus::prelude::*;
use serde_json::{json, Value};

use super::widgets;
use crate::api::{self, s, u};
use crate::components::{Card, Column, Filter, Grid, GridSpec, Spec};
use crate::html::{self, badge, esc};
use crate::state;
use crate::ui::{self, Html};
use crate::Route;

/// The relations a title can have to its base game, in the editor's order.
pub const RELATIONS: [&str; 9] = ["port", "remaster", "remake", "edition", "expansion", "standalone", "dlc", "mod", "bundle"];

/// A version note's text in the UI language, else English.
pub fn note_text(note: &Value, field: &str) -> String {
    let lang = state::setting("ui_language").as_str().unwrap_or("en").to_string();
    let texts = &note[field];
    texts.get(&lang).or_else(|| texts.get("en")).and_then(|v| v.as_str()).unwrap_or_default().to_string()
}

/// A note's rank as a badge: S red, A yellow, B grey.
pub fn rank_badge(rank: &str) -> String {
    let (cls, tip) = match rank {
        "S" => ("bad", state::tr("S: the older version is strongly recommended")),
        "A" => ("warn", state::tr("A: which version is better depends on preference")),
        _ => ("dim", state::tr("B: an older version has content the latest lacks; the latest is still the better one")),
    };
    format!(r#"<span class="badge {cls}" title="{}">{}</span>"#, esc(&tip), esc(&state::trf("note {rank}", &[("rank", rank)])))
}

fn platforms_html(v: &str) -> String {
    v.split(", ").filter(|p| !p.is_empty()).map(|p| html::cbadge("platform", p, None, None)).collect::<Vec<_>>().join(" ")
}

fn side_link(r: &Value, side: &'static str) -> Element {
    let t = &r[side];
    let id = u(t, "title_id");
    let name = s(t, "name");
    rsx! { super::common::CellLink { to: Route::TitleDetail { id }, text: name } }
}

fn definitive_spec() -> GridSpec {
    let columns = vec![
        Column::render("lead", &state::tr("title"), |r| side_link(r, "lead")).sort("name"),
        Column::html("lead_platforms", &state::tr("platform"), |r| platforms_html(&s(&r["lead"], "platforms"))),
        Column::render("other", &state::tr("base game / definitive version"), |r| if u(&r["lead"], "title_id") == u(&r["other"], "title_id") { rsx! { span { class: "muted", {state::tr("(the same)")} } } } else { side_link(r, "other") }),
        Column::html("other_platforms", &state::tr("its platform"), |r| if u(&r["lead"], "title_id") == u(&r["other"], "title_id") { String::new() } else { platforms_html(&s(&r["other"], "platforms")) }),
        Column::text("members", &state::tr("versions")).sort("members").num(),
        Column::html("note_rank", &state::tr("note"), |r| api::so(r, "note_rank").map(|k| rank_badge(&k)).unwrap_or_default()),
    ];
    let mut spec = GridSpec::new("definitive", columns, |q| async move {
        let mut p = q.base();
        p.extend([("platforms", q.f("platform")), ("lead", q.f("lead")), ("noted", q.f("noted"))]);
        p.extend(super::common::DISPLAY_KEYS.iter().map(|k| (*k, q.f(k))));
        api::get(&format!("/api/v1/catalog/definitive{}", api::qs(&p))).await
    })
    .filters(
        vec![
            Filter::Select { key: "lead", label: state::tr("show"), options: Rc::new(|| vec![(String::new(), state::tr("definitive version")), ("base".into(), state::tr("base game"))]) },
            Filter::Select {
                key: "noted",
                label: state::tr("version note"),
                options: Rc::new(|| vec![(String::new(), state::tr("no filter")), ("S".into(), state::tr("rank S")), ("A".into(), state::tr("rank A and above")), ("B".into(), state::tr("rank B and above"))]),
            },
            super::common::platform_filter(),
        ]
        .into_iter()
        .chain(super::common::display_filters())
        .collect(),
    )
    .link(|r| Some(Route::Family { id: u(r, "id") }))
    .local(|r| if u(r, "local_games") > 0 { "local" } else { "" })
    // Into a set as the game's definitive version: copy_definitive decides which release.
    .context(|r, sel| async move {
        let ids: Vec<u64> = sel.unwrap_or_else(|| vec![u(&r, "id")]);
        let head = if ids.len() > 1 { state::trf("{n} selected games", &[("n", &ids.len().to_string())]) } else { s(&r["lead"], "name") };
        let mut items = vec![ui::MenuItem::head(head)];
        items.extend(super::common::set_menu_items(json!({ "family_ids": ids }), None).await);
        items
    })
    .select(
        |r| u(r, "id"),
        vec![
            crate::components::SelAction::new(&state::tr("add to set"), |ids: Vec<u64>| async move {
                super::common::pick_set(json!({ "family_ids": ids }), None);
                false
            }),
            super::ai::batch_action("title_ids", "games"),
        ],
    )
    .cards("definitive", |r, mode| {
        let (lead, other) = (&r["lead"], &r["other"]);
        let same = u(lead, "title_id") == u(other, "title_id");
        let mut sub = platforms_html(&s(lead, "platforms"));
        if !same {
            sub.push_str(&format!(r#"<div class="muted">{}</div>"#, esc(&s(other, "name"))));
        }
        let dual = mode == "dual" && !same;
        Card {
            title: s(lead, "name"),
            media: api::uo(lead, "cover_media_id"),
            media2: if dual { api::uo(other, "cover_media_id") } else { None },
            dual: if dual { two_cover_layout() } else { String::new() },
            held: if u(r, "local_games") > 0 { "local" } else { "" },
            chips: api::so(r, "note_rank").map(|k| rank_badge(&k)).unwrap_or_default(),
            sub,
            ..Default::default()
        }
    });
    spec.modes = vec![("list", "list"), ("grid", "grid"), ("dual", "two-cover grid")];
    spec
}

/// `definitive_two_cover_layout`: top_bottom (default) | diagonal | left_right.
fn two_cover_layout() -> String {
    state::setting("definitive_two_cover_layout").as_str().unwrap_or("top_bottom").to_string()
}

#[component]
pub fn Definitive() -> Element {
    let spec = use_hook(|| Spec(Rc::new(definitive_spec())));
    rsx! { Grid { spec } }
}

/// A family's page: its versions in definitive order, dragged to reorder.
#[component]
pub fn Family(id: u64) -> Element {
    let mut gen = use_signal(|| 0u64);
    let data = use_resource(move || async move {
        let _ = gen();
        api::get(&format!("/api/v1/catalog/definitive/{id}")).await
    });
    let Some(res) = data.read().clone() else { return rsx! { div { class: "muted", {state::tr("loading…")} } } };
    let f = match res {
        Ok(f) => f,
        Err(e) => return rsx! { div { class: "card", "{e.message}" } },
    };
    let head = u(&f, "id");
    let members = api::arr(&f, "members").to_vec();
    let base_name = s(&f["base"], "name");
    let manual = api::b(&f, "manual_order");
    let note = f.get("note").filter(|n| !n.is_null()).cloned();
    let move_member = Rc::new(move |row: Value, after: Option<Value>| {
        spawn(async move {
            match api::post(&format!("/api/v1/catalog/definitive/{head}/order/move"), json!({ "title_id": u(&row, "title_id"), "after_title_id": after.as_ref().map(|a| u(a, "title_id")) })).await {
                Ok(_) => *gen.write() += 1,
                Err(e) => ui::toast(e.message, true),
            }
        });
    });
    let rows = Rc::new(members.clone());
    let order_col = {
        let (rows, mv) = (rows.clone(), move_member.clone());
        Column::render("order", "", move |r| {
            let i = rows.iter().position(|x| u(x, "title_id") == u(r, "title_id")).unwrap_or(0);
            let (rows_up, rows_dn, mv_up, mv_dn) = (rows.clone(), rows.clone(), mv.clone(), mv.clone());
            rsx! {
                span { class: "draghandle", title: state::tr("drag to reorder"), "⋮⋮" }
                button { class: "btn small", title: state::tr("move up"), onclick: move |e| { e.stop_propagation(); if i > 0 { mv_up(rows_up[i].clone(), if i >= 2 { Some(rows_up[i - 2].clone()) } else { None }); } }, "▲" }
                button { class: "btn small", title: state::tr("move down"), onclick: move |e| { e.stop_propagation(); if i + 1 < rows_dn.len() { mv_dn(rows_dn[i].clone(), Some(rows_dn[i + 1].clone())); } }, "▼" }
            }
        })
    };
    let columns = vec![
        order_col,
        Column::render("name", &state::tr("version"), |r| rsx! { super::common::CellLink { to: Route::TitleDetail { id: u(r, "title_id") }, text: s(r, "name") } }),
        Column::html("category", &state::tr("category"), member_badges),
        Column::html("platforms", &state::tr("platform"), |r| platforms_html(&s(r, "platforms"))),
        Column::text("year", &state::tr("year")).num(),
    ];
    let mut spec = GridSpec::new("family_members", columns, widgets::static_fetch(members.clone()))
        .link(|r| Some(Route::TitleDetail { id: u(r, "title_id") }))
        .local(|r| if u(r, "local_games") > 0 { "local" } else { "" })
        .cards("family_members", |r, _| Card {
            title: s(r, "name"),
            media: api::uo(r, "cover_media_id"),
            held: if u(r, "local_games") > 0 { "local" } else { "" },
            chips: member_badges(r),
            sub: format!("{} {}", platforms_html(&s(r, "platforms")), api::uo(r, "year").map(|y| y.to_string()).unwrap_or_default()),
            ..Default::default()
        });
    spec.search = false;
    spec.page_size = 500;
    let mv = move_member.clone();
    spec.reorder = Some(Rc::new(move |row, after| mv(row, after)));
    let key = format!("{head}-{}", members.iter().map(|r| u(r, "title_id").to_string()).collect::<Vec<_>>().join("."));
    let reset = move |_| {
        spawn(async move {
            match api::delete(&format!("/api/v1/catalog/definitive/{head}/order")).await {
                Ok(_) => *gen.write() += 1,
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    rsx! {
        div { class: "crumbs", a { onclick: move |_| { navigator().push(Route::Definitive {}); }, {state::tr("Definitive")} } " › {base_name}" }
        div { class: "card",
            h2 { "{base_name}" " " span { class: "muted", style: "font-size:14px", {state::trf("{n} versions", &[("n", &members.len().to_string())])} } }
            p { class: "muted", {state::tr("Newest version first; between versions on several platforms, your platform priority. Drag a version to put it where you want it.")} }
            if let Some(n) = note {
                div { class: "notice",
                    Html { html: rank_badge(&s(&n, "rank")) }
                    " "
                    b { "{note_text(&n, \"recommend\")}" }
                    div { "{note_text(&n, \"note\")}" }
                }
            }
            div { class: "toolbar",
                button { class: "btn small", onclick: move |_| { navigator().push(Route::TitleDetail { id: head }); }, {state::tr("base game page")} }
                if manual {
                    " "
                    Html { html: badge(&state::tr("your order"), "warn") }
                    " "
                    button { class: "btn small", title: state::tr("back to the newest-first order"), onclick: reset, {state::tr("reset order")} }
                }
            }
            div { key: "{key}", Grid { spec: Spec(Rc::new(spec)) } }
        }
        super::ai::FamilyAi { key: "{head}", head, curated: None }
    }
}

fn member_badges(r: &Value) -> String {
    let mut v = html::cbadge("category", &s(r, "category"), None, None);
    if api::b(r, "is_base") {
        v.push_str(&format!(" {}", badge(&state::tr("base game"), "")));
    }
    if api::b(r, "provider_only") {
        v.push_str(&format!(r#" <span class="badge dim" title="{}">{}</span>"#, esc(&state::tr("a version IGDB knows of; no DAT lists it")), esc(&state::tr("IGDB only"))));
    }
    if api::b(r, "manual") {
        v.push_str(&format!(" {}", badge(&state::tr("moved by you"), "warn")));
    }
    v
}

/// The title page's base game editor: what this title is of which game. `fixed_base`: opened
/// from the base game's page ("add port / remake…"), where the title is picked instead.
#[component]
pub fn EditBasegame(title_id: Option<u64>, category: String, bases: Vec<(u64, String)>, fixed_base: Option<(u64, String)>, on_saved: EventHandler<()>) -> Element {
    let mut target = use_signal(move || title_id.map(|id| (id, String::new())));
    let mut rel = use_signal(move || if category == "base_game" || category.is_empty() { "port".to_string() } else { category.clone() });
    let start_bases = match &fixed_base {
        Some(b) => vec![b.clone()],
        None => bases.clone(),
    };
    let mut picked = use_signal(move || start_bases);
    let picking_title = fixed_base.is_some();
    let save = move |_| {
        let Some((tid, _)) = target() else {
            ui::toast(state::tr("pick the title"), true);
            return;
        };
        let ids: Vec<u64> = picked.read().iter().map(|(id, _)| *id).collect();
        let category = rel();
        spawn(async move {
            match api::post(&format!("/api/v1/catalog/titles/{tid}/basegame"), json!({ "category": category, "base_title_ids": ids })).await {
                Ok(_) => {
                    ui::close_modal();
                    on_saved.call(());
                }
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    let bundle = rel() == "bundle";
    rsx! {
        h2 { if picking_title { {state::tr("Add port / remake")} } else { {state::tr("Base game")} } }
        p { class: "muted", {state::tr("Saved as your own: IGDB updates leave it alone until you reset it.")} }
        div { class: "kv",
            if picking_title {
                div { {state::tr("title")} }
                div {
                    if let Some((_, name)) = target() { span { class: "badge", "{name} " b { onclick: move |_| target.set(None), "×" } } }
                    widgets::BaseGameFilter { value: String::new(), on_pick: move |v: String| {
                        if let Ok(id) = v.parse::<u64>() {
                            spawn(async move {
                                let name = api::get(&format!("/api/v1/catalog/titles/{id}")).await.map(|t| api::so(&t, "display_name").unwrap_or_else(|| s(&t, "name"))).unwrap_or_else(|_| format!("#{id}"));
                                target.set(Some((id, name)));
                            });
                        }
                    } }
                }
            }
            div { {state::tr("is a")} }
            div {
                select { onchange: move |e| rel.set(e.value()),
                    for r in RELATIONS {
                        option { value: "{r}", selected: rel() == r, {state::t(&format!("relation.{r}"), r)} }
                    }
                }
            }
            div { if bundle { {state::tr("games it holds")} } else { {state::tr("of")} } }
            div {
                for (bid, bname) in picked() {
                    span { key: "{bid}", class: "badge", "{bname} "
                        if !picking_title { b { title: state::tr("remove"), onclick: move |_| picked.write().retain(|(i, _)| *i != bid), "×" } }
                    }
                    " "
                }
                if !picking_title && (bundle || picked.read().is_empty()) {
                    widgets::BaseGameFilter { value: String::new(), on_pick: move |v: String| {
                        if let Ok(id) = v.parse::<u64>() {
                            spawn(async move {
                                let name = api::get(&format!("/api/v1/catalog/titles/{id}")).await.map(|t| api::so(&t, "display_name").unwrap_or_else(|| s(&t, "name"))).unwrap_or_else(|_| format!("#{id}"));
                                let mut p = picked.write();
                                if !bundle { p.clear(); }
                                if !p.iter().any(|(i, _)| *i == id) { p.push((id, name)); }
                            });
                        }
                    } }
                }
            }
        }
        div { class: "toolbar",
            button { class: "btn", onclick: save, {state::tr("save")} }
            " "
            if !picking_title {
                if let Some(tid) = title_id {
                    button { class: "btn", title: state::tr("no base game: this title is a game of its own"),
                        onclick: move |_| {
                            spawn(async move {
                                match api::post(&format!("/api/v1/catalog/titles/{tid}/basegame"), json!({ "category": "base_game", "base_title_ids": [] })).await {
                                    Ok(_) => { ui::close_modal(); on_saved.call(()); }
                                    Err(e) => ui::toast(e.message, true),
                                }
                            });
                        },
                        {state::tr("no base game")}
                    }
                    " "
                }
            }
            button { class: "btn", onclick: move |_| ui::close_modal(), {state::tr("cancel")} }
        }
    }
}

/// Back to IGDB's base game and category.
pub fn reset_basegame(title_id: u64, mut on_done: impl FnMut() + 'static) {
    spawn(async move {
        match api::delete(&format!("/api/v1/catalog/titles/{title_id}/basegame")).await {
            Ok(_) => on_done(),
            Err(e) => ui::toast(e.message, true),
        }
    });
}
