//! RetroAchievements: the account block, the six lists (RAChecker's shape) and a game's page
//! with its achievements and compatibility patches (the console's `showRa` / `showRaGame`).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use dioxus::prelude::*;
use serde_json::{json, Value};

use super::common::{self, bump};
use crate::api::{self, s, u};
use crate::components::{Card, Column, Filter, Grid, GridCtl, GridSpec, Spec};
use crate::html::{self, badge, esc};
use crate::state;
use crate::ui::{self, Html, HtmlDiv};
use crate::Route;

thread_local! {
    static AUTO_REFRESHED: Cell<bool> = const { Cell::new(false) };
    /// The lists' base filter (console / genre and its value), kept across the RA subtabs.
    static BASE: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
}

fn ra_img(path: &str) -> String {
    api::media_url(path)
}

pub fn award_badge(kind: &str) -> String {
    let (label, color) = match kind {
        "mastered" => ("mastered", "#e0b020"),
        "completed" => ("completed", "#b8c0d0"),
        "beaten-hardcore" => ("beaten (hc)", "#4f9a6a"),
        "beaten-softcore" => ("beaten", "#4f9a6a"),
        _ => return String::new(),
    };
    format!("<span class=\"badge\" style=\"background:{color};color:#111\">{}</span>", esc(&state::tr(label)))
}

pub fn progress(r: &Value) -> String {
    let max = u(r, "max_possible");
    if max == 0 {
        return format!("<span class=\"muted\">{}</span>", esc(&state::tr("no achievements")));
    }
    let (got, hc_got) = (u(r, "num_awarded"), u(r, "num_awarded_hardcore"));
    let pct = ((100 * got) as f64 / max as f64).round() as u64;
    let hc = ((100 * hc_got) as f64 / max as f64).round() as u64;
    let tip = state::trf("{got} of {max} ({hc} hardcore)", &[("got", &got.to_string()), ("max", &max.to_string()), ("hc", &hc_got.to_string())]);
    format!(
        "<div title=\"{}\"><span class=\"num\">{got}/{max}</span> <span class=\"muted\">{pct}%</span><div class=\"rabar\"><div class=\"{}\" style=\"width:{pct}%\"></div></div></div>",
        esc(&tip),
        if hc >= pct && pct > 0 { "hc" } else { "" }
    )
}

fn n(v: &Value, k: &str) -> String {
    let x = u(v, k);
    // Thousands separators, like toLocaleString.
    let s = x.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The account block. `status`: /ra/status; `on_change`: reload it.
#[component]
fn RaStatusBlock(status: Value, on_change: EventHandler<()>) -> Element {
    let st = status;
    let p = st.get("profile").cloned().filter(|p| !p.is_null());
    let c = st.get("counts").cloned().unwrap_or_default();
    let pv = p.clone().unwrap_or_default();
    let genre_job = st.get("genre_job").cloned().filter(|j| !j.is_null());
    let sync = st.get("sync").cloned().filter(|j| !j.is_null());
    let left = u(&c, "ra_catalogue").saturating_sub(u(&st, "genre_checked"));
    let hours = ((left as f64) * 1.5 / 3600.0).ceil() as u64;
    let fetch_genres = move |all: bool| {
        spawn(async move {
            match api::post(&format!("/api/v1/ra/genres/fetch{}", if all { "?all=true" } else { "" }), json!({})).await {
                Ok(r) => {
                    ui::toast(if api::b(&r, "already_queued") { state::tr("a genre fetch is already queued") } else { state::trf("genre fetch queued #{id} (Jobs tab shows progress; cancel any time, it resumes)", &[("id", &u(&r, "job_id").to_string())]) }, false);
                    on_change.call(());
                }
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    let member_since = api::so(&pv, "member_since").map(|ms| state::trf("member since {date}", &[("date", &ms.chars().take(10).collect::<String>())]));
    let fetch_all = state::trf("fetch whole list (~{hours} h)", &[("hours", &hours.to_string())]);
    let synced_at = state::trf("synced {date}", &[("date", &html::fmt_date(&s(&pv, "fetched_at")))]);
    rsx! {
        div { class: "card", div { class: "rahead",
            if p.as_ref().is_some_and(|p| api::b(p, "has_avatar")) { img { class: "avatar", src: ra_img("/api/v1/ra/avatar"), alt: "" } } else { div { class: "avatar" } }
            div {
                h2 { style: "margin:0", "{s(&st, \"user\")} " {ui::key_explain("explain.ra.account")} }
                div { class: "muted", if p.is_some() { "{s(&pv, \"rich_presence\")}" } else { {state::tr("not synced yet")} } }
                if let Some(ms) = member_since { div { class: "muted", "{ms}" } }
            }
            div { class: "stat", span { class: "muted", {state::tr("points (hardcore)")} } b { "{n(&pv, \"total_points\")}" } }
            div { class: "stat", span { class: "muted", {state::tr("softcore")} } b { "{n(&pv, \"total_softcore_points\")}" } }
            div { class: "stat", span { class: "muted", "RetroPoints" } b { "{n(&pv, \"total_true_points\")}" } }
            div { class: "stat", span { class: "muted", {state::tr("local games")} } b { "{n(&c, \"local_games\")}" } }
            div { class: "stat", span { class: "muted", {state::tr("owned with RA set")} } b { "{n(&c, \"local_ra_games\")}" } }
            div { class: "stat", span { class: "muted", {state::tr("played on RA")} } b { "{n(&c, \"account_games\")}" } }
            div { class: "stat", span { class: "muted", {state::tr("RA games (cached)")} } b { "{n(&c, \"ra_catalogue\")}" } }
            div { class: "stat", span { class: "muted", {state::tr("mastered / completed / beaten")} } b { "{n(&c, \"mastered\")} / {n(&c, \"completed\")} / {n(&c, \"beaten\")}" } }
            div { class: "stat", title: state::tr("RetroAchievements hands out genres one game at a time: local games are fetched automatically after each account sync, the whole list on request (about 1.5 s per game; resumable)"),
                span { class: "muted", {state::tr("genres known (local / all)")} }
                b { "{n(&st, \"genre_checked_local\")}/{n(&c, \"local_ra_games\")} · {n(&st, \"genre_checked\")}/{n(&c, \"ra_catalogue\")}" }
                if let Some(j) = genre_job {
                    Html { html: badge(&api::so(&j, "progress_message").unwrap_or_else(|| s(&j, "status")), "warn") }
                } else {
                    span {
                        span { class: "link", onclick: move |_| fetch_genres(false), {state::tr("fetch local")} }
                        " · "
                        span { class: "link", onclick: move |_| fetch_genres(true), "{fetch_all}" }
                    }
                }
            }
            div { style: "margin-left:auto;text-align:right",
                if let Some(j) = sync {
                    Html { html: badge(&state::trf("syncing: {status}", &[("status", &api::so(&j, "progress_message").unwrap_or_else(|| s(&j, "status")))]), "warn") }
                } else if p.is_some() {
                    span { class: "muted", "{synced_at}" }
                }
                br {}
                button { class: "btn small",
                    onclick: move |_| { spawn(async move { match api::post("/api/v1/ra/refresh", json!({})).await { Ok(r) => { ui::toast(if api::b(&r, "already_queued") { state::tr("already syncing") } else { state::trf("sync queued #{id}", &[("id", &u(&r, "job_id").to_string())]) }, false); on_change.call(()); } Err(e) => ui::toast(e.message, true) } }); },
                    {state::tr("refresh from RetroAchievements")}
                }
            }
        } }
    }
}

fn no_account() -> Element {
    rsx! {
        div { class: "card",
            h2 { "RetroAchievements" }
            p { class: "muted", {state::tr("No account: set ")} span { class: "mono", "ra_username" } {state::tr(" and ")} span { class: "mono", "ra_api_key" } {state::tr(" in api.env, then restart the server.")} }
        }
    }
}

/// Loads /ra/status (once auto-refreshing a stale account per session) and polls it while a
/// sync runs; `synced` is bumped when a sync finishes.
fn use_ra_status(synced: Signal<u64>) -> Resource<Result<Value, String>> {
    let mut status = use_resource(|| async {
        let mut st = api::get("/api/v1/ra/status").await.map_err(|e| e.message)?;
        if api::b(&st, "available") && api::b(&st, "stale") && st.get("sync").is_none_or(|x| x.is_null()) && !AUTO_REFRESHED.with(|a| a.replace(true)) {
            if api::post("/api/v1/ra/refresh", json!({})).await.is_ok() {
                if let Ok(s2) = api::get("/api/v1/ra/status").await {
                    st = s2;
                }
            }
        }
        Ok(st)
    });
    use_future(move || async move {
        loop {
            ui::sleep(3000).await;
            let syncing = matches!(&*status.peek(), Some(Ok(st)) if st.get("sync").is_some_and(|x| !x.is_null()));
            if syncing {
                if let Ok(s2) = api::get("/api/v1/ra/status").await {
                    let done = s2.get("sync").is_none_or(|x| x.is_null());
                    status.restart();
                    if done {
                        bump(synced);
                    }
                }
            }
        }
    });
    status
}

#[component]
fn RaBaseFilter(consoles: Vec<(String, String)>, genres: Vec<String>) -> Element {
    let ctl = use_context::<GridCtl>();
    let (kind, value) = BASE.with(|b| b.borrow().clone()).unwrap_or_default();
    let mut kind_sig = use_signal(|| kind.clone());
    let apply = move |k: String, v: String| {
        BASE.with(|b| *b.borrow_mut() = Some((k.clone(), v.clone())));
        ctl.set_filter("console_id", if k == "console" { v.clone() } else { String::new() });
        ctl.set_filter("genre", if k == "genre" { v } else { String::new() });
    };
    let k = kind_sig();
    rsx! {
        select { title: state::tr("base filter"), onchange: move |e| { kind_sig.set(e.value()); apply(e.value(), String::new()); },
            option { value: "console", selected: k == "console", {state::tr("by console")} }
            option { value: "genre", selected: k == "genre", {state::tr("by genre")} }
        }
        select { title: state::tr("filter value"), onchange: move |e| apply(kind_sig(), e.value()),
            if k == "console" {
                option { value: "", {state::tr("all consoles")} }
                for (cid, name) in consoles { option { value: "{cid}", selected: cid == value, "{name}" } }
            } else {
                option { value: "", {state::tr("all genres")} }
                for g in genres { option { value: "{g}", selected: g == value, "{g}" } }
            }
        }
    }
}

/// An RA row's LOCAL as the catalogue's rows give it, for `common::status_badges`.
fn ra_local(r: &Value) -> Value {
    serde_json::json!({ "local_games": if api::b(r, "local") { u(r, "local_games").max(1) } else { 0 } })
}

fn ra_spec(list: &str, st: &Value) -> GridSpec {
    if BASE.with(|b| b.borrow().is_none()) {
        let k = api::so(st, "base_filter_default").unwrap_or_else(|| "console".into());
        BASE.with(|b| *b.borrow_mut() = Some((k, String::new())));
    }
    let (bk, bv) = BASE.with(|b| b.borrow().clone()).unwrap_or_default();
    let consoles: Vec<(String, String)> = api::arr(st, "consoles").iter().filter_map(|c| c.as_array()).map(|c| (c.first().map(|x| x.to_string().trim_matches('"').to_string()).unwrap_or_default(), c.get(1).and_then(|x| x.as_str()).unwrap_or("").to_string())).collect();
    let genres: Vec<String> = api::arr(st, "genres").iter().filter_map(|g| g.as_str().map(str::to_string)).collect();
    let columns = vec![
        Column::render("icon", "", |r| rsx! { img { loading: "lazy", src: ra_img(&format!("/api/v1/ra/games/{}/image/icon", u(r, "game_id"))), alt: "", style: "width:32px;height:32px;border-radius:4px" } }),
        Column::html("title", &state::tr("title"), |r| esc(&api::so(r, "title").unwrap_or_else(|| state::trf("RA game {id}", &[("id", &u(r, "game_id").to_string())])))).sort("title"),
        Column::text("console_name", &state::tr("console")).sort("console"),
        Column::html("genre", &state::tr("genre"), |r| s(r, "genre").split(',').filter(|g| !g.is_empty()).map(|g| badge(g, "dim")).collect::<Vec<_>>().join(" ")),
        Column::html("where", "", |r| format!("{} {}", super::common::status_badges(&ra_local(r), true), if api::b(r, "in_account") { String::new() } else { badge(&state::tr("not played"), "dim") })),
        Column::html("progress", &state::tr("progress"), progress).sort("progress"),
        Column::html("award", &state::tr("award"), |r| format!("{}{}", award_badge(&s(r, "highest_award_kind")), api::so(r, "highest_award_date").map(|d| format!(" <span class=\"muted\">{}</span>", html::fmt_date(&d))).unwrap_or_default())).sort("award"),
        Column::html("recent", &state::tr("last unlock"), |r| html::fmt_date(&s(r, "most_recent_awarded"))).sort("recent"),
    ];
    let mut filters = vec![
        Filter::Select {
            key: "source",
            label: state::tr("games"),
            options: Rc::new(|| {
                [("all", "all RetroAchievements games (local first)"), ("local", "local only")].iter().map(|(v, l)| (v.to_string(), state::tr(l))).collect()
            }),
        },
        Filter::Select { key: "set", label: state::tr("sets"), options: Rc::new(|| vec![("all".into(), state::tr("base sets + subsets")), ("base".into(), state::tr("base sets only")), ("subset".into(), state::tr("subsets only"))]) },
    ];
    if list == "in_progress" {
        filters.push(Filter::Check { key: "include_beaten", label: state::tr("include beaten"), off: "false".into() });
    }
    let by_title = list == "titles" || list == "to_play";
    let list_q = list.to_string();
    let list_c = list.to_string();
    let mut spec = GridSpec::new("ra", columns, move |q| {
        let list = list_q.clone();
        async move {
            let p = vec![
                ("list", list),
                ("source", q.f("source")),
                ("console_id", q.f("console_id")),
                ("genre", q.f("genre")),
                ("set", q.f("set")),
                ("include_beaten", q.f("include_beaten")),
                ("q", q.search.clone()),
                ("sort", q.sort.clone()),
                ("desc", if q.desc { "true".into() } else { "false".into() }),
                ("limit", q.limit.to_string()),
                ("offset", q.offset.to_string()),
            ];
            api::get(&format!("/api/v1/ra/games{}", api::qs(&p))).await
        }
    })
    .filters(filters)
    .link(|r| Some(Route::RaGame { id: u(r, "game_id") }))
    // Into a set: each game adds the release you hold of it, else its top-priority release.
    .context(|r, sel| async move {
        let id = u(&r, "game_id");
        let title = api::so(&r, "title").unwrap_or_else(|| state::trf("RA game {id}", &[("id", &id.to_string())]));
        let label = match &sel { Some(ids) => state::trf("add {n} games to a set…", &[("n", &ids.len().to_string())]), None => state::tr("add to set…") };
        let ids = sel.unwrap_or_else(|| vec![id]);
        vec![crate::ui::MenuItem::head(title), crate::ui::MenuItem::new(&label, move || super::common::pick_set(serde_json::json!({ "ra_game_ids": ids.clone() }), None))]
    })
    .select(
        |r| u(r, "game_id"),
        vec![crate::components::SelAction::new(&state::tr("add to set"), |ids| async move {
            super::common::pick_set(serde_json::json!({ "ra_game_ids": ids }), None);
            false
        })],
    )
    .local(|r| if api::b(r, "local") { "local" } else { "" })
    .cards("ra", |r, mode| {
        let gid = u(r, "game_id");
        let img = match mode {
            "icon" => Some(ra_img(&format!("/api/v1/ra/games/{gid}/image/icon"))),
            "screenshot" => Some(ra_img(&format!("/api/v1/ra/games/{gid}/image/ingame"))),
            "title" => Some(ra_img(&format!("/api/v1/ra/games/{gid}/image/title"))),
            "badges" => Some(ra_img(&format!("/api/v1/ra/games/{gid}/badges"))),
            _ => api::uo(r, "cover_media_id").is_none().then(|| ra_img(&format!("/api/v1/ra/games/{gid}/image/boxart"))),
        };
        let award = s(r, "highest_award_kind");
        let cls = format!(
            "{}{}",
            match mode {
                "icon" => "square icon",
                "badges" => "square badges",
                "screenshot" | "title" => "shot",
                _ => "",
            },
            if award.is_empty() { String::new() } else { format!(" award-{award}") }
        );
        Card {
            title: api::so(r, "title").unwrap_or_else(|| state::trf("RA game {id}", &[("id", &gid.to_string())])),
            media: if mode == "grid" { api::uo(r, "cover_media_id") } else { None },
            img,
            cls,
            held: if api::b(r, "local") { "local" } else { "" },
            flags: super::common::status_badges(&ra_local(r), true),
            chips: award_badge(&award),
            sub: format!("<span class=\"muted\">{}</span>{}", esc(&s(r, "console_name")), progress(r)),
            media2: None,
            dual: String::new(),
        }
    })
    .toolbar(move || rsx! { RaBaseFilter { consoles: consoles.clone(), genres: genres.clone() } });
    spec.modes = vec![("list", "list"), ("grid", "cover grid"), ("icon", "RA icon grid"), ("title", "RA title screen grid"), ("screenshot", "RA screenshot grid"), ("badges", "RA badges grid")];
    spec.default_sort = if by_title { "title".into() } else { "recent".into() };
    spec.default_desc = !by_title;
    spec.initial_filters.insert("source".into(), s(st, "source_default"));
    spec.initial_filters.insert("include_beaten".into(), if api::b(st, "include_beaten_default") { "true".into() } else { "false".into() });
    spec.initial_filters.insert("console_id".into(), if bk == "console" { bv.clone() } else { String::new() });
    spec.initial_filters.insert("genre".into(), if bk == "genre" { bv } else { String::new() });
    spec.loaded = Some(Rc::new(move |page: &Value| {
        if let Some(tc) = page.get("tab_counts").and_then(|t| t.as_object()) {
            let mut c = state::RA_COUNTS.write();
            for (k, v) in tc {
                c.insert(k.clone(), v.as_u64().unwrap_or(0));
            }
        }
        let _ = &list_c;
    }));
    spec
}

#[component]
pub fn RaList(list: String) -> Element {
    rsx! { for k in [list] { RaListPage { key: "{k}", list: k } } }
}

#[component]
fn RaListPage(list: String) -> Element {
    let synced = use_signal(|| 0u64);
    let mut status = use_ra_status(synced);
    let st = match &*status.read() {
        None => return rsx! { div { class: "muted", {state::tr("loading…")} } },
        Some(Err(e)) => return rsx! { div { class: "card bad", "{e}" } },
        Some(Ok(st)) => st.clone(),
    };
    if !api::b(&st, "available") {
        return no_account();
    }
    rsx! {
        RaStatusBlock { status: st.clone(), on_change: move |_| status.restart() }
        div { class: "card", RaGrid { list: list.clone(), status: st, synced } }
    }
}

/// The list itself, built once from the first status (its defaults).
#[component]
fn RaGrid(list: String, status: Value, synced: Signal<u64>) -> Element {
    let spec = use_hook(|| Spec(Rc::new(ra_spec(&list, &status))));
    rsx! { Grid { spec, reload: Some(synced) } }
}

// --- one game ---

fn patch_cell(x: &Value, on_download: EventHandler<String>) -> Element {
    let url = s(x, "patch_url");
    let patch = x.get("patch").cloned().filter(|p| !p.is_null());
    if url.is_empty() && patch.is_none() {
        return rsx! {};
    }
    if let Some(j) = api::so(x, "patch_job").or_else(|| api::uo(x, "patch_job").map(|j| j.to_string())) {
        return rsx! { Html { html: badge(&state::trf("downloading… {job}", &[("job", &j)]), "warn") } };
    }
    let md5 = s(x, "ra_md5");
    if let Some(p) = patch {
        let tip = format!(
            "{}{}{}",
            s(&p, "rel_path"),
            api::so(&p, "patch_rom_name").map(|n| state::trf(" — patch files: {name}", &[("name", &n)])).unwrap_or_default(),
            if api::b(&p, "names_consistent") { String::new() } else { state::tr(" (files inside are named differently)") }
        );
        return if api::b(&p, "file_present") {
            rsx! { span { class: "badge ok", title: "{tip}", {state::tr("patch file ✓")} } }
        } else {
            rsx! {
                span { class: "badge bad", title: "{tip}", {state::tr("patch file missing")} }
                " "
                button { class: "btn small", onclick: move |e| { e.stop_propagation(); on_download.call(md5.clone()); }, {state::tr("download again")} }
            }
        };
    }
    rsx! { button { class: "btn small", title: "{url}", onclick: move |e| { e.stop_propagation(); on_download.call(md5.clone()); }, {state::tr("download patch")} } }
}

#[component]
pub fn RaGame(id: u64) -> Element {
    rsx! { for k in [id] { RaGamePage { key: "{k}", id: k } } }
}

#[component]
fn RaGamePage(id: u64) -> Element {
    // "refresh unlocks": the next load asks RetroAchievements again (once).
    let refresh: Rc<Cell<bool>> = use_hook(|| Rc::new(Cell::new(false)));
    let mut gen = use_signal(|| 0u64);
    let synced = use_signal(|| 0u64);
    let mut status = use_ra_status(synced);
    let refresh_load = refresh.clone();
    let data = use_resource(move || {
        let r = refresh_load.replace(false);
        let _ = gen();
        async move { api::get(&format!("/api/v1/ra/games/{id}{}", if r { "?refresh=true" } else { "" })).await }
    });
    // Two independent filters: unlock state, and kind (missable or any).
    let mut filter = use_signal(|| "all".to_string());
    let mut kind_filter = use_signal(|| "all".to_string());
    // While a patch download runs, look again every few seconds.
    use_future(move || async move {
        loop {
            ui::sleep(3000).await;
            let running = matches!(&*data.peek(), Some(Ok(g)) if api::arr(g, "releases").iter().chain(api::arr(g, "other_patches")).any(|x| x.get("patch_job").is_some_and(|j| !j.is_null())));
            if running {
                *gen.write() += 1;
            }
        }
    });
    let g = match &*data.read() {
        None => return rsx! { div { class: "muted", {state::tr("loading achievements… (the first view of a game asks RetroAchievements; a few seconds)")} } },
        Some(Err(e)) => return rsx! { div { class: "card bad", "{e.message}" } },
        Some(Ok(g)) => g.clone(),
    };
    let achievements = api::arr(&g, "achievements").to_vec();
    let earned = |a: &Value| api::so(a, "date_earned").is_some() || api::so(a, "date_earned_hardcore").is_some();
    let row = g.get("row").cloned().filter(|r| !r.is_null()).unwrap_or_else(|| {
        json!({ "max_possible": achievements.len(), "num_awarded": achievements.iter().filter(|a| earned(a)).count(), "num_awarded_hardcore": achievements.iter().filter(|a| api::so(a, "date_earned_hardcore").is_some()).count() })
    });
    let points: u64 = achievements.iter().map(|a| u(a, "points")).sum();
    let got: u64 = achievements.iter().filter(|a| earned(a)).map(|a| u(a, "points")).sum();
    let title = api::so(&g, "title").unwrap_or_else(|| state::trf("RA game {id}", &[("id", &id.to_string())]));
    let award = g.get("row").map(|r| s(r, "highest_award_kind")).unwrap_or_default();
    let releases = api::arr(&g, "releases").to_vec();
    let others = api::arr(&g, "other_patches").to_vec();
    let download = EventHandler::new(move |md5: String| {
        spawn(async move {
            match api::post("/api/v1/ra/patches/download", json!({ "md5": md5 })).await {
                Ok(r) => {
                    ui::toast(if api::b(&r, "already_queued") { state::tr("already downloading") } else { state::trf("patch download queued #{id}", &[("id", &u(&r, "job_id").to_string())]) }, false);
                    *gen.write() += 1;
                }
                Err(e) => ui::toast(e.message, true),
            }
        });
    });
    let img = |kind: &str| ra_img(&format!("/api/v1/ra/games/{id}/image/{kind}"));
    let (cur, cur_kind) = (filter(), kind_filter());
    let state_ok = |a: &Value, k: &str| match k {
        "achieved" => earned(a),
        "unachieved" => !earned(a),
        _ => true,
    };
    let kind_ok = |a: &Value, k: &str| k != "missable" || s(a, "kind") == "missable";
    let shown: Vec<Value> = achievements.iter().filter(|a| state_ok(a, &cur) && kind_ok(a, &cur_kind)).cloned().collect();
    // Each button counts what it would show alongside the other filter's current choice.
    let filters: Vec<(&str, String, usize)> = [("all", "all"), ("achieved", "achieved"), ("unachieved", "unachieved")].iter().map(|(k, l)| (*k, state::tr(l), achievements.iter().filter(|a| state_ok(a, k) && kind_ok(a, &cur_kind)).count())).collect();
    let kind_filters: Vec<(&str, String, usize)> = [("all", "all kinds"), ("missable", "missable only")].iter().map(|(k, l)| (*k, state::tr(l), achievements.iter().filter(|a| state_ok(a, &cur) && kind_ok(a, k)).count())).collect();
    let points_line = state::trf("{got} / {points} points", &[("got", &got.to_string()), ("points", &points.to_string())]);
    let achievements_h = state::trf("Achievements ({n})", &[("n", &achievements.len().to_string())]);
    rsx! {
        match &*status.read() {
            Some(Ok(st)) if api::b(st, "available") => rsx! { RaStatusBlock { status: st.clone(), on_change: move |_| status.restart() } },
            _ => rsx! {},
        }
        super::PagePlayer { query: format!("ra_game_id={id}") }
        div { class: "crumbs",
            a { onclick: move |_| navigator().go_back(), {state::tr("‹ back")} }
            " · "
            a { onclick: move |_| { navigator().push(Route::RaList { list: "titles".into() }); }, "RetroAchievements" }
            " › {title}"
        }
        div { class: "card",
            div { class: "rahead",
                img { loading: "lazy", src: img("icon"), alt: "", style: "max-height:160px;border-radius:4px" }
                div {
                    h2 { style: "margin:0", "{title} " Html { html: award_badge(&award) } }
                    div { class: "muted",
                        "{s(&g, \"console_name\")} · {points_line} · "
                        a { class: "link", href: "https://retroachievements.org/game/{id}", target: "_blank", rel: "noopener", "retroachievements.org" }
                        " · "
                        span { class: "link", onclick: move |_| { refresh.set(true); *gen.write() += 1; }, {state::tr("refresh unlocks")} }
                    }
                    HtmlDiv { html: progress(&row) }
                }
                div { style: "margin-left:auto;display:flex;gap:8px",
                    for k in ["title", "ingame", "boxart"] { img { loading: "lazy", src: img(k), alt: "", style: "max-height:160px;border-radius:4px" } }
                }
            }
            if releases.is_empty() {
                p { class: "muted", {state::tr("No catalog release is in RetroAchievements’ hash list for this game.")} }
            } else {
                h3 { {state::tr("Supported releases")} }
                ui::Explain { p { {state::tr("Releases whose hash is in RetroAchievements’ own list for this game (or that you linked by hand) — the ones that earn achievements. A patched release needs RetroAchievements’ compatibility patch: ")} b { {state::tr("download patch")} } {state::tr(" saves it as is into your patch directory (Config › Directories, type “patch”).")} } }
                table { class: "grid",
                    for x in releases {
                        {
                            let rid = u(&x, "release_id");
                            let local = u(&x, "local_games");
                            let conf = s(&x, "confidence");
                            let tid = api::uo(&x, "title_id");
                            rsx! {
                                tr { class: if local > 0 { "click local" } else { "click" }, onclick: move |e| common::go(Route::Release { id: rid }, &e),
                                    td { "{s(&x, \"name\")} " {patch_cell(&x, download)} }
                                    td { "{s(&x, \"platform\")}" }
                                    td { Html { html: if local > 0 { badge(&state::trf("{n} local", &[("n", &local.to_string())]), "ok") } else { badge(&state::tr("not held"), "dim") } } }
                                    td { Html { html: badge(&conf, if ["manual", "exact", "hash"].contains(&conf.as_str()) { "ok" } else { "warn" }) } }
                                    td { if let Some(t) = tid { span { class: "link", onclick: move |e| { e.stop_propagation(); navigator().push(Route::TitleDetail { id: t }); }, {state::tr("title →")} } } }
                                }
                            }
                        }
                    }
                }
            }
            if !others.is_empty() {
                h3 { {state::tr("Other patched hashes")} }
                ui::Explain { p { {state::tr("Patched ROMs RetroAchievements supports that no release in your catalog carries (yet).")} } }
                table { class: "grid",
                    for x in others {
                        tr {
                            td { "{api::so(&x, \"ra_name\").unwrap_or_else(|| s(&x, \"ra_md5\"))} " {patch_cell(&x, download)} }
                            td { class: "mono muted", "{s(&x, \"ra_md5\")}" }
                        }
                    }
                }
            }
        }
        div { class: "card",
            h2 { {state::tr("Badges")} }
            if achievements.is_empty() { span { class: "muted", {state::tr("no achievements")} } } else { img { class: "mosaic", src: ra_img(&format!("/api/v1/ra/games/{id}/badges")), alt: "" } }
        }
        div { class: "card",
            h2 { "{achievements_h}" }
            div { class: "toolbar",
                for (k, l, c) in filters {
                    button { class: if k == cur { "btn small on" } else { "btn small" }, onclick: move |_| filter.set(k.to_string()), "{l} ({c})" }
                }
                span { class: "muted", style: "margin:0 6px", "·" }
                for (k, l, c) in kind_filters {
                    button { class: if k == cur_kind { "btn small on" } else { "btn small" }, onclick: move |_| kind_filter.set(k.to_string()), "{l} ({c})" }
                }
            }
            div {
                if shown.is_empty() { span { class: "muted", {state::tr("none")} } }
                for a in shown {
                    {
                        let e = earned(&a);
                        let hc = api::so(&a, "date_earned_hardcore");
                        let sc = api::so(&a, "date_earned");
                        let kind = s(&a, "kind");
                        let badge_url = ra_img(&format!("/api/v1/ra/badges/{}{}", api::encode(&api::so(&a, "badge_name").unwrap_or_else(|| "00000".into())), if e { "" } else { "?locked=true" }));
                        let when = if let Some(d) = &hc { state::trf("hardcore {date}", &[("date", &html::fmt_date(d))]) } else if let Some(d) = &sc { state::trf("softcore {date}", &[("date", &html::fmt_date(d))]) } else { state::tr("locked") };
                        rsx! {
                            div { class: if hc.is_some() { "ach hc" } else if sc.is_some() { "ach sc" } else { "ach" },
                                img { loading: "lazy", src: "{badge_url}", alt: "" }
                                div {
                                    b { "{s(&a, \"title\")}" }
                                    " "
                                    if !kind.is_empty() { Html { html: badge(&kind.replacen('_', " ", 1), if kind == "missable" { "warn" } else { "dim" }) } }
                                    div { class: "muted", "{s(&a, \"description\")}" }
                                }
                                div { style: "text-align:right",
                                    b { "{u(&a, \"points\")}" }
                                    " " span { class: "muted", "({u(&a, \"true_ratio\")})" }
                                    div { class: "muted", "{when}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
