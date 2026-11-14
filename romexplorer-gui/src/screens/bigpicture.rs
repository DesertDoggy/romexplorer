//! Big Picture mode: the collection as a full-screen launcher for handhelds and TVs (Steam
//! Deck, ROG Ally, Legion Go, ARM consoles running Linux). Big covers, one Filter button,
//! a page per cross-platform game / title / release with its related releases, and Start on
//! every item -- driven by touch, a mouse or a game controller.
//!
//! It is a second view of screens the app already has, over the same API, so it adds no
//! server routes. The web console has no counterpart: CLAUDE.md makes the exception, and
//! docs/plan/handheld.md is the plan this is phase 1 of.
//!
//! Controller: the D-pad / left stick moves the focus, A opens, B goes back, X starts, Y
//! opens the item's menu, LB / RB switch tabs, Select opens the filters, Start the search.
//! The keyboard has the same keys (arrows, Enter, Esc, X, Y, Q / E, F, /).

use dioxus::prelude::*;
use serde_json::{json, Value};

use crate::api::{self, s, u};
use crate::state;
use crate::ui::{self, MenuItem};
use crate::Route;

/// The list tabs: the collection's browse lists, then the RetroAchievements lists.
const TABS: [(&str, &str); 13] = [
    ("crossplatform", "Cross-platform"),
    ("definitive", "Definitive"),
    ("titles", "Titles"),
    ("releases", "Releases"),
    ("games", "Games"),
    ("files", "Files"),
    ("contents", "Contents"),
    ("ra-titles", "Titles"),
    ("ra-mastery", "Mastery"),
    ("ra-complete", "Complete"),
    ("ra-beat", "Beat"),
    ("ra-in_progress", "In progress"),
    ("ra-to_play", "To be played"),
];

const PAGE: u64 = 60;

/// Whether this build starts games. The phone (romcompanion) browses its synced copy and
/// starts nothing yet: launching through Android's emulators is a later step.
const CAN_START: bool = cfg!(not(feature = "mobile"));

/// Whether a list tab is shown. On the phone, the tabs it synced (Games, Files and Contents
/// are off by default there: they clutter a phone); everywhere else, all of them.
fn tab_shown(key: &str) -> bool {
    #[cfg(feature = "mobile")]
    {
        let t = crate::api::companion::settings().options.tabs;
        match key {
            "crossplatform" => t.crossplatform,
            "definitive" => t.definitive,
            "titles" => t.titles,
            "releases" => t.releases,
            "games" => t.games,
            "files" => t.files,
            "contents" => t.contents,
            _ => t.ra,
        }
    }
    #[cfg(not(feature = "mobile"))]
    {
        let _ = key;
        true
    }
}

/// The tab a list opens on: the first one shown.
pub fn first_tab() -> String {
    TABS.iter()
        .map(|(k, _)| *k)
        .find(|k| tab_shown(k))
        .unwrap_or("titles")
        .to_string()
}

/// How the covers are laid out. Auto follows the screen: one column in portrait, one row in
/// landscape. Single does the same whatever the size; Multi is a grid.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Layout {
    Auto,
    Single,
    Multi,
}

impl Layout {
    fn key(self) -> &'static str {
        match self {
            Layout::Auto => "auto",
            Layout::Single => "single",
            Layout::Multi => "multi",
        }
    }
    fn from_key(k: &str) -> Self {
        match k {
            "single" => Layout::Single,
            "multi" => Layout::Multi,
            _ => Layout::Auto,
        }
    }
    fn label(self) -> String {
        state::tr(match self {
            Layout::Auto => "Auto (follows rotation)",
            Layout::Single => "Single column / row",
            Layout::Multi => "Grid",
        })
    }
    fn next(self) -> Self {
        match self {
            Layout::Auto => Layout::Single,
            Layout::Single => Layout::Multi,
            Layout::Multi => Layout::Auto,
        }
    }
}

static LAYOUT: GlobalSignal<Layout> = Signal::global(|| Layout::Auto);
/// The item the focus is on, for the details panel.
static FOCUSED: GlobalSignal<Option<Item>> = Signal::global(|| None);
/// Search and filters per list tab, kept while Big Picture is open.
static QUERIES: GlobalSignal<std::collections::HashMap<String, ListQuery>> = Signal::global(std::collections::HashMap::new);

#[derive(Clone, PartialEq, Default, Debug)]
struct ListQuery {
    search: String,
    /// Platform ids, comma-separated.
    platforms: String,
    /// The Held filter (`held=`): held, not held, ... on the Machine filter's machine.
    held: String,
    /// The Machine filter (`machine=`): whose holdings Held is about; alone, held there.
    machine: String,
}

impl ListQuery {
    fn count(&self) -> usize {
        usize::from(!self.platforms.is_empty()) + usize::from(!self.held.is_empty()) + usize::from(!self.machine.is_empty())
    }
}

/// Per-device preferences, in this webview's own storage: the layout, and whether the app
/// opens straight into Big Picture.
const PREF_LAYOUT: &str = "rx.bigpicture.layout";
const PREF_START: &str = "rx.bigpicture.start";

pub(crate) async fn pref(key: &str) -> Option<String> {
    let js = format!("try {{ return localStorage.getItem({}); }} catch (e) {{ return null; }}", serde_json::to_string(key).unwrap_or_default());
    document::eval(&js).await.ok().and_then(|v| v.as_str().map(str::to_string))
}

pub(crate) fn set_pref(key: &str, value: &str) {
    let js = format!("try {{ localStorage.setItem({}, {}); }} catch (e) {{}}", serde_json::to_string(key).unwrap_or_default(), serde_json::to_string(value).unwrap_or_default());
    let _ = document::eval(&js);
}

/// Whether this device asked to open in Big Picture (the app checks once per run).
pub async fn starts_in_big_picture() -> bool {
    pref(PREF_START).await.as_deref() == Some("1")
}

// --- items ---

/// What a Start or an Open with is pointed at, worked out when it is pressed: a list row
/// often names only a title or a release, and the game behind it costs a request or two.
#[derive(Clone, PartialEq, Debug)]
enum Play {
    /// A local game (its playlist, or first disc), and the release it is a copy of: its
    /// discs come from the release page's data.
    Game(u64, Option<u64>),
    /// The held release of a title / a release / a cross-platform game.
    Title(u64),
    Release(u64),
    Igdb(String),
    /// Not startable from here (a file, a release that isn't held).
    None,
}

/// One cover in a list or on a page.
#[derive(Clone, PartialEq, Debug)]
pub struct Item {
    key: String,
    name: String,
    /// The gray line under it: platform, region.
    sub: String,
    cover: Option<String>,
    /// Where A goes: a Big Picture page, else the app's own page.
    open: Option<Route>,
    /// Held here: shows Start.
    held: bool,
    play: Play,
    /// Label / value lines for the details panel.
    details: Vec<(String, String)>,
}

fn cover(media: Option<u64>) -> Option<String> {
    media.map(api::media)
}

fn platforms_line(r: &Value) -> String {
    s(r, "platforms").split(", ").filter(|p| !p.is_empty()).map(state::platform_label).collect::<Vec<_>>().join(" · ")
}

fn detail(out: &mut Vec<(String, String)>, label: &str, value: String) {
    if !value.is_empty() && value != "0" {
        out.push((state::tr(label), value));
    }
}

fn held_line(r: &Value) -> String {
    let mut out = Vec::new();
    let n = u(r, "local_games");
    if n > 0 {
        out.push(state::trf("here ({n})", &[("n", &n.to_string())]));
    }
    if api::uo(r, "remote_games").is_some_and(|m| m > 0) {
        out.push(state::tr("main server"));
    }
    if u(r, "peer_games") > 0 {
        out.push(state::tr("shared clients"));
    }
    out.join(", ")
}

fn title_item(r: &Value) -> Item {
    let id = u(r, "id");
    let mut details = Vec::new();
    detail(&mut details, "platforms", platforms_line(r));
    detail(&mut details, "category", s(r, "category"));
    detail(&mut details, "releases", u(r, "release_count").to_string());
    detail(&mut details, "held", held_line(r));
    if api::b(r, "ra") {
        details.push(("RetroAchievements".into(), state::tr("supported")));
    }
    Item {
        key: format!("t{id}"),
        name: api::so(r, "display_name").filter(|n| !n.is_empty()).unwrap_or_else(|| s(r, "name")),
        sub: platforms_line(r),
        cover: cover(api::uo(r, "cover_media_id")),
        open: Some(Route::BpTitle { id }),
        held: u(r, "local_games") > 0,
        play: Play::Title(id),
        details,
    }
}

fn release_item(r: &Value) -> Item {
    let id = u(r, "id");
    let name = state::release_display_name(r);
    let region = s(r, "region");
    let platform = state::platform_label(&s(r, "platform"));
    let mut details = Vec::new();
    detail(&mut details, "platform", platform.clone());
    detail(&mut details, "region", region.clone());
    detail(&mut details, "languages", s(r, "languages"));
    detail(&mut details, "year", s(r, "year"));
    detail(&mut details, "discs", if u(r, "disc_count") > 1 { u(r, "disc_count").to_string() } else { String::new() });
    detail(&mut details, "kind", s(r, "game_kind"));
    detail(&mut details, "held", held_line(r));
    if api::b(r, "ra") {
        details.push(("RetroAchievements".into(), state::tr("supported")));
    }
    Item {
        key: format!("r{id}"),
        name: if name.is_empty() { s(r, "name") } else { name },
        sub: [platform, region].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · "),
        cover: cover(api::uo(r, "cover_media_id")),
        open: Some(Route::BpRelease { id }),
        held: u(r, "local_games") > 0,
        play: Play::Release(id),
        details,
    }
}

fn game_item(r: &Value) -> Item {
    let id = u(r, "id");
    let platform = state::platform_label(&s(r, "platform"));
    let region = s(r, "region");
    let mut details = Vec::new();
    detail(&mut details, "platform", platform.clone());
    detail(&mut details, "region", region.clone());
    detail(&mut details, "roms", u(r, "rom_count").to_string());
    detail(&mut details, "files", u(r, "file_count").to_string());
    if api::b(r, "ra") {
        details.push(("RetroAchievements".into(), state::tr("supported")));
    }
    Item {
        key: format!("g{id}"),
        name: api::so(r, "display_name").or_else(|| api::so(r, "release_name")).unwrap_or_else(|| s(r, "name")),
        sub: [platform, region].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · "),
        cover: cover(api::uo(r, "cover_media_id")),
        open: api::uo(r, "release_id").map(|id| Route::BpRelease { id }),
        held: true,
        play: Play::Game(id, api::uo(r, "release_id")),
        details,
    }
}

fn cross_item(r: &Value) -> Item {
    let igdb = s(r, "igdb_id");
    let mut details = Vec::new();
    detail(&mut details, "platforms", platforms_line(r));
    detail(&mut details, "titles", u(r, "title_count").to_string());
    detail(&mut details, "releases", s(r, "releases"));
    Item {
        key: format!("x{igdb}"),
        name: s(r, "name"),
        sub: platforms_line(r),
        cover: cover(api::uo(r, "cover_media_id")),
        open: Some(Route::BpCross { igdb: igdb.clone() }),
        held: u(r, "local_games") > 0,
        play: Play::Igdb(igdb),
        details,
    }
}

fn definitive_item(r: &Value) -> Item {
    let lead = r.get("lead").cloned().unwrap_or(Value::Null);
    let other = r.get("other").cloned().unwrap_or(Value::Null);
    let id = u(&lead, "title_id");
    let mut details = Vec::new();
    detail(&mut details, "platforms", platforms_line(&lead));
    detail(&mut details, "versions", u(r, "members").to_string());
    detail(&mut details, "base game", s(&other, "name"));
    detail(&mut details, "rank", s(r, "note_rank"));
    Item {
        key: format!("d{}", u(r, "id")),
        name: s(&lead, "name"),
        sub: platforms_line(&lead),
        cover: cover(api::uo(&lead, "cover_media_id")),
        open: Some(Route::BpTitle { id }),
        held: u(r, "local_games") > 0,
        play: Play::Title(id),
        details,
    }
}

fn content_item(r: &Value) -> Item {
    let id = u(r, "id");
    let platform = state::platform_label(&s(r, "platform"));
    let mut details = Vec::new();
    detail(&mut details, "platform", platform.clone());
    detail(
        &mut details,
        "size",
        crate::html::fmt_size(api::uo(r, "size")),
    );
    Item {
        key: format!("c{id}"),
        name: api::so(r, "name")
            .or_else(|| api::so(r, "release_name"))
            .unwrap_or_else(|| format!("#{id}")),
        sub: platform,
        cover: None,
        open: api::uo(r, "release_id").map(|id| Route::BpRelease { id }),
        held: false,
        play: Play::None,
        details,
    }
}

fn file_item(r: &Value) -> Item {
    let id = u(r, "id");
    let path = s(r, "path");
    let platform = state::platform_label(&s(r, "platform"));
    let mut details = Vec::new();
    detail(&mut details, "platform", platform.clone());
    detail(&mut details, "path", path.clone());
    detail(&mut details, "size", crate::html::fmt_size(api::uo(r, "size")));
    Item {
        key: format!("f{id}"),
        name: super::common::file_name(&path),
        sub: platform,
        cover: None,
        open: Some(Route::File { id }),
        held: false,
        play: Play::None,
        details,
    }
}

fn ra_item(r: &Value) -> Item {
    let gid = u(r, "game_id");
    let release = api::uo(r, "release_id");
    let mut details = Vec::new();
    detail(&mut details, "console", s(r, "console_name"));
    let max = u(r, "max_possible");
    if max > 0 {
        details.push((state::tr("achievements"), format!("{} / {max}", u(r, "num_awarded"))));
    }
    detail(&mut details, "award", s(r, "highest_award_kind"));
    detail(&mut details, "genre", s(r, "genre"));
    Item {
        key: format!("a{gid}"),
        name: s(r, "title"),
        sub: s(r, "console_name"),
        cover: api::uo(r, "cover_media_id").map(api::media).or_else(|| Some(api::media_url(&format!("/api/v1/ra/games/{gid}/image/boxart")))),
        open: Some(release.map_or(Route::RaGame { id: gid }, |id| Route::BpRelease { id })),
        held: u(r, "local_games") > 0 && release.is_some(),
        play: release.map_or(Play::None, Play::Release),
        details,
    }
}

async fn fetch_list(tab: &str, q: &ListQuery, offset: u64) -> api::ApiResult<(Vec<Item>, u64)> {
    let base = |extra: Vec<(&'static str, String)>| {
        let mut p = vec![("search", q.search.clone()), ("limit", PAGE.to_string()), ("offset", offset.to_string()), ("platforms", q.platforms.clone())];
        p.extend(extra);
        api::qs(&p)
    };
    // The phone filters every list by Held and Machine; its copy answers them all.
    let held_of = || vec![("held", q.held.clone()), ("machine", q.machine.clone())];
    let held = || if cfg!(feature = "mobile") { held_of() } else { vec![] };
    let (path, map): (String, fn(&Value) -> Item) = match tab {
        "crossplatform" => (format!("/api/v1/catalog/crossplatform{}", base(held())), cross_item),
        "titles" => (format!("/api/v1/catalog/titles{}", base(held_of())), title_item),
        "releases" => (format!("/api/v1/catalog/releases{}", base(held_of())), release_item),
        "definitive" => (format!("/api/v1/catalog/definitive{}", base(held())), definitive_item),
        "contents" => (format!("/api/v1/catalog/contents{}", base(vec![])), content_item),
        "games" => (format!("/api/v1/catalog/games{}", base(vec![])), game_item),
        "files" => (format!("/api/v1/catalog/files{}", base(vec![])), file_item),
        ra => {
            let list = ra.strip_prefix("ra-").unwrap_or("titles");
            let mut p = vec![("list", list.to_string()), ("q", q.search.clone()), ("limit", PAGE.to_string()), ("offset", offset.to_string())];
            p.extend(held());
            (format!("/api/v1/ra/games{}", api::qs(&p)), ra_item)
        }
    };
    let v = api::get(&path).await?;
    Ok((api::items(&v).iter().map(map).collect(), u(&v, "total")))
}

// --- starting ---

/// What a Play comes to: the game to start, the platform it is on and its discs.
#[derive(Clone, Debug, Default)]
struct Target {
    game: Option<u64>,
    /// (disc number, rom id), in disc order; empty for a single-rom game.
    discs: Vec<(u64, u64)>,
}

async fn first_held_release(path: String) -> Option<u64> {
    let v = api::get(&path).await.ok()?;
    api::items(&v).first().map(|r| u(r, "id"))
}

async fn resolve(play: &Play) -> Option<Target> {
    if !CAN_START {
        return None;
    }
    let release = match play {
        Play::None => return None,
        Play::Game(g, None) => return Some(Target { game: Some(*g), ..Default::default() }),
        Play::Game(g, Some(release)) => {
            // The release lists its copies with their roms: this copy's discs.
            let r = api::get(&format!("/api/v1/catalog/releases/{release}")).await.ok();
            let game = r.as_ref().and_then(|r| api::arr(r, "games").iter().find(|x| u(x, "id") == *g).cloned());
            return Some(game.map_or(Target { game: Some(*g), ..Default::default() }, |x| target_of_game(&x)));
        }
        Play::Release(r) => *r,
        Play::Title(t) => first_held_release(format!("/api/v1/catalog/releases?title_id={t}&view=local&limit=1")).await?,
        Play::Igdb(igdb) => {
            let v = api::get(&format!("/api/v1/catalog/titles?igdb={}&view=local&limit=1", api::encode(igdb))).await.ok()?;
            let title = api::items(&v).first().map(|t| u(t, "id"))?;
            first_held_release(format!("/api/v1/catalog/releases?title_id={title}&view=local&limit=1")).await?
        }
    };
    let r = api::get(&format!("/api/v1/catalog/releases/{release}")).await.ok()?;
    let game = api::arr(&r, "games").first().cloned()?;
    Some(target_of_game(&game))
}

fn target_of_game(game: &Value) -> Target {
    let roms = api::arr(game, "roms");
    let discs: Vec<(u64, u64)> = if roms.len() > 1 { roms.iter().filter(|r| !api::b(r, "alt") && !api::b(r, "deleted")).enumerate().map(|(i, r)| (api::uo(r, "disc_number").unwrap_or(i as u64 + 1), u(r, "id"))).collect() } else { Vec::new() };
    Target { game: Some(u(game, "id")), discs }
}

async fn launch(target: Vec<(&'static str, u64)>, emulator: Option<u64>) {
    let mut body = serde_json::Map::new();
    for (k, v) in target {
        body.insert(k.into(), json!(v));
    }
    if let Some(e) = emulator {
        body.insert("emulator_id".into(), json!(e));
    }
    match api::post("/api/v1/launch", Value::Object(body)).await {
        Ok(r) => ui::toast(state::trf("started {name}", &[("name", &s(&r, "emulator"))]), false),
        Err(e) => ui::toast(e.message, true),
    }
}

/// Start with the platform's default emulator.
fn start(play: Play) {
    spawn(async move {
        match resolve(&play).await.and_then(|t| t.game) {
            Some(g) => launch(vec![("game_id", g)], None).await,
            None => ui::toast(state::tr("not held here, nothing to start"), true),
        }
    });
}

/// The Open with menu for one game or disc: every emulator set up for its platform, the
/// default marked, and a way to make another one the default.
async fn open_with_menu(x: f64, y: f64, head: String, target: (&'static str, u64)) {
    let o = match api::get(&format!("/api/v1/launch/options?{}={}", target.0, target.1)).await {
        Ok(o) => o,
        Err(e) => return ui::toast(e.message, true),
    };
    let emulators = api::arr(&o, "emulators").to_vec();
    let platform = api::so(&o, "platform").unwrap_or_default();
    let platform_id = u(&o, "platform_id");
    let mut items = vec![MenuItem::head(head)];
    if emulators.is_empty() {
        items.push(MenuItem::new(state::trf("Open with… (no emulator for {platform} — set one up)", &[("platform", &platform)]), || {
            navigator().push(Route::ConfigEmulators {});
        }));
        return ui::open_menu(x, y, items);
    }
    for e in &emulators {
        let (id, name) = (u(e, "id"), s(e, "name"));
        let mut label = state::trf("Open with {name}", &[("name", &name)]);
        if api::b(e, "is_default") {
            label.push_str(&state::tr(" (default)"));
        }
        if let Some(w) = api::so(e, "unusable") {
            label.push_str(&format!(" — {w}"));
        }
        items.push(MenuItem::new(label, move || {
            spawn(async move { launch(vec![target], Some(id)).await });
        }));
    }
    for e in emulators.iter().filter(|e| !api::b(e, "is_default")) {
        let (id, name) = (u(e, "id"), s(e, "name"));
        let label = state::trf("Make {name} the default for {platform}", &[("name", &name), ("platform", &platform)]);
        items.push(MenuItem::new(label, move || {
            spawn(async move { set_default(id, platform_id).await });
        }));
    }
    ui::open_menu(x, y, items);
}

/// Makes emulator `id` the default for platform `platform_id` (saving it does the rest:
/// a platform marked default stops being another emulator's).
async fn set_default(id: u64, platform_id: u64) {
    let list = match api::get("/api/v1/emulators").await {
        Ok(v) => api::items(&v),
        Err(e) => return ui::toast(e.message, true),
    };
    let Some(mut e) = list.into_iter().find(|e| u(e, "id") == id) else { return };
    if let Some(ps) = e.get_mut("platforms").and_then(Value::as_array_mut) {
        for p in ps.iter_mut().filter(|p| u(p, "app_platform_id") == platform_id) {
            p["is_default"] = json!(true);
        }
    }
    match api::post("/api/v1/emulators", e).await {
        Ok(_) => ui::toast(state::tr("default emulator changed"), false),
        Err(e) => ui::toast(e.message, true),
    }
}

/// The item's menu (Y / long press / right click): Start, Open with, each disc, Open page.
fn item_menu(item: Item, x: f64, y: f64) {
    spawn(async move {
        let mut items = vec![MenuItem::head(item.name.clone())];
        let target = if item.held { resolve(&item.play).await } else { None };
        if let Some(t) = target.clone() {
            if let Some(g) = t.game {
                items.push(MenuItem::new(state::tr("Start"), move || {
                    spawn(async move { launch(vec![("game_id", g)], None).await });
                }));
                let head = item.name.clone();
                items.push(MenuItem::new(state::tr("Open with…"), move || {
                    let head = head.clone();
                    spawn(async move { open_with_menu(x, y, head, ("game_id", g)).await });
                }));
            }
            for (n, rom) in t.discs.iter().copied() {
                items.push(MenuItem::new(state::trf("Start disc {n}", &[("n", &n.to_string())]), move || {
                    spawn(async move { launch(vec![("rom_id", rom)], None).await });
                }));
                let head = format!("{} · {}", item.name, state::trf("disc {n}", &[("n", &n.to_string())]));
                items.push(MenuItem::new(state::trf("Open disc {n} with…", &[("n", &n.to_string())]), move || {
                    let head = head.clone();
                    spawn(async move { open_with_menu(x, y, head, ("rom_id", rom)).await });
                }));
            }
        }
        if let Some(to) = item.open.clone() {
            items.push(MenuItem::new(state::tr("Open page"), move || {
                navigator().push(to.clone());
            }));
        }
        // The phone asks the machine it syncs from to fetch metadata and art, by hand only:
        // nothing on the phone queues these by itself.
        if !CAN_START {
            let refresh = match item.open.clone() {
                Some(Route::BpTitle { id }) => Some(format!("/api/v1/catalog/titles/{id}/refresh")),
                Some(Route::BpRelease { id }) => {
                    Some(format!("/api/v1/catalog/releases/{id}/refresh"))
                }
                _ => None,
            };
            if let Some(path) = refresh {
                items.push(MenuItem::new(
                    state::tr("Fetch metadata and art (on the server)"),
                    move || {
                        let path = path.clone();
                        spawn(async move {
                            match api::post(&path, Value::Null).await {
                                Ok(v) => ui::toast(
                                    state::trf(
                                        "{n} job(s) queued on the server; sync to see the results",
                                        &[("n", &api::items(&v).len().to_string())],
                                    ),
                                    false,
                                ),
                                Err(e) => ui::toast(e.message, true),
                            }
                        });
                    },
                ));
            }
        }
        if let Some(to) = app_page(&item) {
            items.push(MenuItem::new(state::tr("Open in the app"), move || {
                navigator().push(to.clone());
            }));
        }
        ui::open_menu(x, y, items);
    });
}

/// The same item's page in the normal app.
fn app_page(item: &Item) -> Option<Route> {
    // The phone has no app pages to go to: its whole UI is this one.
    if !CAN_START {
        return None;
    }
    match item.open.clone()? {
        Route::BpTitle { id } => Some(Route::TitleDetail { id }),
        Route::BpRelease { id } => Some(Route::Release { id }),
        Route::BpCross { igdb } => Some(Route::Titles { q: format!("igdb={igdb}") }),
        _ => None,
    }
}

// --- the shell ---

/// Moves the focus, reads the controller, and maps its buttons onto the keys above.
const NAV_JS: &str = r#"
if (!window.__rxBigPicture) {
  window.__rxBigPicture = true;
  const visible = el => { const r = el.getBoundingClientRect(); return r.width > 0 && r.height > 0; };
  const menuOpen = () => { const m = document.getElementById('ctxmenu'); return m && m.style.display === 'block' ? m : null; };
  const modalOpen = () => { const m = document.getElementById('modal'); return m && m.style.display === 'flex' ? m : null; };
  const scope = () => {
    const m = menuOpen();
    if (m) { for (const d of m.querySelectorAll('div:not(.head)')) d.tabIndex = 0; return [...m.querySelectorAll('div:not(.head)')]; }
    const root = modalOpen() || document.querySelector('.bp');
    return root ? [...root.querySelectorAll('[data-nav], button, input, select')].filter(visible) : [];
  };
  const focus = el => { if (!el) return; el.focus({ preventScroll: true }); el.scrollIntoView({ block: 'nearest', inline: 'nearest', behavior: 'smooth' }); };
  const move = dir => {
    const all = scope(); if (!all.length) return;
    const cur = document.activeElement;
    if (!all.includes(cur)) { focus(all[0]); return; }
    const a = cur.getBoundingClientRect(), ax = a.left + a.width / 2, ay = a.top + a.height / 2;
    let best = null, bestScore = Infinity;
    for (const el of all) {
      if (el === cur) continue;
      const b = el.getBoundingClientRect(), bx = b.left + b.width / 2, by = b.top + b.height / 2;
      const dx = bx - ax, dy = by - ay;
      const ok = dir === 'left' ? dx < -1 : dir === 'right' ? dx > 1 : dir === 'up' ? dy < -1 : dy > 1;
      if (!ok) continue;
      const along = dir === 'left' || dir === 'right' ? Math.abs(dx) : Math.abs(dy);
      const across = dir === 'left' || dir === 'right' ? Math.abs(dy) : Math.abs(dx);
      const score = along + across * 3;
      if (score < bestScore) { bestScore = score; best = el; }
    }
    focus(best);
  };
  const press = sel => { const el = document.querySelector(sel); if (el) el.click(); };
  const back = () => {
    if (menuOpen() || modalOpen()) { const root = document.querySelector('.bp'); if (modalOpen()) modalOpen().click(); else if (root) root.click(); return; }
    press('#bp-back');
  };
  const tab = step => {
    const tabs = [...document.querySelectorAll('.bp-tabs [data-tab]')]; if (!tabs.length) return;
    const i = tabs.findIndex(t => t.classList.contains('active'));
    tabs[(i + step + tabs.length) % tabs.length].click();
  };
  document.addEventListener('keydown', e => {
    if (!document.querySelector('.bp')) return;
    const typing = ['INPUT', 'SELECT', 'TEXTAREA'].includes(document.activeElement && document.activeElement.tagName);
    const k = e.key;
    if (typing && k !== 'Escape' && k !== 'Enter' && k !== 'ArrowUp' && k !== 'ArrowDown') return;
    let done = true;
    if (k === 'ArrowLeft') move('left');
    else if (k === 'ArrowRight') move('right');
    else if (k === 'ArrowUp') move('up');
    else if (k === 'ArrowDown') move('down');
    else if (k === 'Enter' && !typing) { const a = document.activeElement; if (a && a !== document.body) a.click(); }
    else if (k === 'Escape' || k === 'Backspace') { if (typing) document.activeElement.blur(); else back(); }
    else if (k === 'x' || k === 'X') { const a = document.activeElement; const b = a && a.querySelector('[data-start]'); if (b) b.click(); }
    else if (k === 'y' || k === 'Y') { const a = document.activeElement; if (a && a !== document.body) { const r = a.getBoundingClientRect(); a.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: r.left + r.width / 2, clientY: r.top + r.height / 2 })); } }
    else if (k === 'q' || k === 'Q' || k === 'PageUp') tab(-1);
    else if (k === 'e' || k === 'E' || k === 'PageDown') tab(1);
    else if (k === 'f' || k === 'F') press('#bp-filter');
    else if (k === '/') press('#bp-search');
    else done = false;
    if (done && !(typing && k === 'Enter')) e.preventDefault();
  });
  // The controller: standard mapping (A B X Y, LB RB, LT RT, Select Start, D-pad 12-15).
  const keyOf = { 0: 'Enter', 1: 'Escape', 2: 'x', 3: 'y', 4: 'q', 5: 'e', 8: 'f', 9: '/', 12: 'ArrowUp', 13: 'ArrowDown', 14: 'ArrowLeft', 15: 'ArrowRight' };
  const held = {}; const next = {};
  const send = key => document.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true }));
  const poll = t => {
    const pads = navigator.getGamepads ? [...navigator.getGamepads()].filter(Boolean) : [];
    const now = {};
    for (const p of pads) {
      p.buttons.forEach((b, i) => { if (b.pressed && keyOf[i]) now[keyOf[i]] = true; });
      const [x = 0, y = 0] = p.axes;
      if (x < -0.5) now.ArrowLeft = true; if (x > 0.5) now.ArrowRight = true;
      if (y < -0.5) now.ArrowUp = true; if (y > 0.5) now.ArrowDown = true;
    }
    for (const k of Object.keys(now)) {
      if (!held[k]) { send(k); next[k] = t + 400; }
      else if (k.startsWith('Arrow') && t >= next[k]) { send(k); next[k] = t + 120; }
      held[k] = true;
    }
    for (const k of Object.keys(held)) if (!now[k]) delete held[k];
    requestAnimationFrame(poll);
  };
  requestAnimationFrame(poll);
}
"#;

const BP_CSS: &str = include_str!("../../assets/bigpicture.css");

fn tab_label(key: &str, label: &str) -> String {
    if let Some(list) = key.strip_prefix("ra-") {
        format!("RA · {}", state::t(&format!("sub.ra.{list}"), label))
    } else {
        state::t(&format!("sub.collection.{key}"), label)
    }
}

/// Big Picture's frame: tabs, the toolbar (search, filter, layout, exit), the page, and the
/// overlays the normal shell would otherwise provide.
#[component]
pub fn BigPictureShell() -> Element {
    let route = use_route::<Route>();
    let current = if let Route::BigPicture { tab } = &route { tab.clone() } else { String::new() };
    // B on a list leaves Big Picture; on a page it goes back to where it came from.
    let on_list = matches!(route, Route::BigPicture { .. });
    use_effect(|| {
        let _ = document::eval(NAV_JS);
    });
    use_future(|| async {
        if let Some(l) = pref(PREF_LAYOUT).await {
            *LAYOUT.write() = Layout::from_key(&l);
        }
    });
    let layout = LAYOUT();
    rsx! {
        document::Style { {BP_CSS} }
        div { class: "bp layout-{layout.key()}", onclick: move |_| ui::close_menu(),
            div { class: "bp-top",
                button { id: "bp-back", class: "bp-btn", title: state::tr("back (B)"),
                    onclick: move |_| {
                        if on_list || !navigator().can_go_back() {
                            // The phone's lists are its home: there is nowhere further back.
                            if CAN_START { navigator().push(Route::Status {}); }
                        } else {
                            navigator().go_back();
                        }
                    },
                    "◀"
                }
                nav { class: "bp-tabs",
                    for (key, label) in TABS.into_iter().filter(|(k, _)| tab_shown(k)) {
                        button { key: "{key}", "data-tab": "{key}", class: if key == current { "active" } else { "" },
                            onclick: move |_| { navigator().replace(Route::BigPicture { tab: key.to_string() }); },
                            {tab_label(key, label)}
                        }
                    }
                }
                button { class: "bp-btn", title: state::tr("layout"), onclick: move |_| { let n = LAYOUT().next(); *LAYOUT.write() = n; set_pref(PREF_LAYOUT, n.key()); ui::toast(n.label(), false); }, "▦" }
                if cfg!(feature = "mobile") {
                    button { class: "bp-btn", title: state::tr("Sync"), onclick: move |_| ui::open_modal(|| rsx! { super::companion::SyncSheet {} }), "⟳" }
                    button { class: "bp-btn", title: state::tr("Settings"), onclick: move |_| ui::open_modal(|| rsx! { Settings {} }), "⚙" }
                } else {
                    button { class: "bp-btn", title: state::tr("Big Picture settings"), onclick: move |_| ui::open_modal(|| rsx! { Settings {} }), "⚙" }
                    button { class: "bp-btn", title: state::tr("leave Big Picture"), onclick: move |_| { navigator().push(Route::Status {}); }, "✕" }
                }
            }
            super::companion::SyncLine {}
            Outlet::<Route> {}
            ui::MenuHost {}
            ui::ModalHost {}
            ui::ToastHost {}
        }
    }
}

/// A sheet's title with its close button at the top right, where a thumb finds it and the
/// phone's navigation bar can't cover it. Stays in view while the sheet scrolls. B / Back
/// closes the sheet too.
#[component]
pub fn SheetHead(title: String) -> Element {
    rsx! {
        div { class: "bp-sheet-head",
            h3 { "{title}" }
            button { class: "bp-btn bp-close", title: state::tr("close (B)"), onclick: move |_| ui::close_modal(), "✕" }
        }
    }
}

#[component]
fn Settings() -> Element {
    let mut start = use_signal(|| false);
    use_future(move || async move { start.set(starts_in_big_picture().await) });
    rsx! {
        SheetHead { title: if CAN_START { state::tr("Big Picture settings") } else { state::tr("Settings") } }
        div { class: "bp-sheet",
            p { class: "muted", {state::tr("Saved on this device only.")} }
            h4 { {state::tr("Covers")} }
            for l in [Layout::Auto, Layout::Single, Layout::Multi] {
                button { class: if LAYOUT() == l { "bp-choice active" } else { "bp-choice" }, onclick: move |_| { *LAYOUT.write() = l; set_pref(PREF_LAYOUT, l.key()); }, {l.label()} }
            }
            if CAN_START {
                h4 { {state::tr("Starting")} }
                button { class: if start() { "bp-choice active" } else { "bp-choice" },
                    onclick: move |_| { let v = !start(); start.set(v); set_pref(PREF_START, if v { "1" } else { "0" }); },
                    if start() { "✓ " } {state::tr("Open the app in Big Picture mode")}
                }
                h4 { {state::tr("Emulators")} }
                p { class: "muted", {state::tr("Start uses each platform's default emulator. Choose the defaults in Config › Emulators, or from Open with… on any game.")} }
                button { class: "bp-choice", onclick: move |_| { ui::close_modal(); navigator().push(Route::ConfigEmulators {}); }, {state::tr("Config › Emulators")} }
            } else {
                super::companion::PhoneSettings {}
            }
        }
    }
}

// --- lists ---

#[component]
pub fn BigPicture(tab: String) -> Element {
    rsx! { for t in [tab] { BpList { key: "{t}", tab: t } } }
}

#[component]
fn BpList(tab: String) -> Element {
    let tab_key = tab.clone();
    let query = use_memo(move || QUERIES.read().get(&tab_key).cloned().unwrap_or_default());
    let mut items = use_signal(Vec::<Item>::new);
    let mut total = use_signal(|| 0u64);
    let mut loading = use_signal(|| true);
    let mut error = use_signal(|| None::<String>);
    let tab_load = tab.clone();
    // A new search or filter starts the list again.
    use_effect(move || {
        let q = query();
        let tab = tab_load.clone();
        loading.set(true);
        spawn(async move {
            match fetch_list(&tab, &q, 0).await {
                Ok((list, n)) => {
                    items.set(list);
                    total.set(n);
                    error.set(None);
                }
                Err(e) => error.set(Some(e.message)),
            }
            loading.set(false);
            let _ = document::eval("setTimeout(() => { const a = document.activeElement; if (!a || a === document.body || !a.closest('.bp')) { const f = document.querySelector('.bp-items [data-nav]'); if (f) f.focus({ preventScroll: true }); } }, 50);");
        });
    });
    let tab_more = tab.clone();
    let mut busy = use_signal(|| false);
    // The "more" tile loads the next page when pressed or reached with the focus.
    let mut more = move || {
        if busy() {
            return;
        }
        busy.set(true);
        let tab = tab_more.clone();
        let q = query();
        let offset = items.read().len() as u64;
        spawn(async move {
            if let Ok((list, _)) = fetch_list(&tab, &q, offset).await {
                items.write().extend(list);
            }
            busy.set(false);
        });
    };
    let mut more2 = more.clone();
    let shown = items.read().len() as u64;
    let n = total();
    let tab_for_tools = tab.clone();
    rsx! {
        Toolbar { tab: tab_for_tools, total: n }
        div { class: "bp-body",
            div { class: "bp-items",
                if let Some(e) = error() { div { class: "card bad", "{e}" } }
                if !loading() && items.read().is_empty() && error().is_none() { div { class: "muted bp-empty", {state::tr("nothing here")} } }
                for it in items.read().iter().cloned() { Cover { key: "{it.key}", item: it } }
                if shown < n {
                    button { class: "bp-more", "data-nav": "1", onclick: move |_| more(), onfocus: move |_| more2(), {state::trf("{n} more…", &[("n", &(n - shown).to_string())])} }
                }
            }
            DetailsPanel {}
        }
    }
}

#[component]
fn Toolbar(tab: String, total: u64) -> Element {
    let key = tab.clone();
    let q = QUERIES.read().get(&key).cloned().unwrap_or_default();
    let mut searching = use_signal(|| false);
    let ra = tab.starts_with("ra-");
    let filters = q.count();
    let tab_search = tab.clone();
    let tab_filter = tab.clone();
    let tab_clear = tab.clone();
    rsx! {
        div { class: "bp-toolbar",
            button { id: "bp-search", class: "bp-btn", onclick: move |_| searching.set(!searching()), "🔍 " {state::tr("Search")} }
            if searching() || !q.search.is_empty() {
                input { r#type: "search", class: "bp-search", autofocus: true, value: "{q.search}", placeholder: state::tr("search"),
                    onchange: move |e| { let tab = tab_search.clone(); QUERIES.write().entry(tab).or_default().search = e.value(); },
                }
            }
            if !ra {
                button { id: "bp-filter", class: "bp-btn", onclick: move |_| { let tab = tab_filter.clone(); ui::open_modal(move || rsx! { FilterSheet { tab: tab.clone() } }); },
                    "⚲ " {state::tr("Filter")} if filters > 0 { " ({filters})" }
                }
            }
            if filters > 0 || !q.search.is_empty() {
                button { class: "bp-btn", onclick: move |_| { QUERIES.write().remove(&tab_clear); searching.set(false); }, {state::tr("Clear")} }
            }
            span { class: "muted", {state::trf("{n} items", &[("n", &total.to_string())])} }
        }
    }
}

/// The one Filter button's sheet: pick a filter type, then its values; Apply filters.
#[component]
fn FilterSheet(tab: String) -> Element {
    let start = QUERIES.read().get(&tab).cloned().unwrap_or_default();
    let mut draft = use_signal(|| start.clone());
    let mut open = use_signal(|| None::<&'static str>);
    let has_view = matches!(tab.as_str(), "titles" | "releases") || (cfg!(feature = "mobile") && (matches!(tab.as_str(), "crossplatform" | "definitive") || tab.starts_with("ra-")));
    let machines: Vec<(String, String)> = if has_view { super::companion::machine_views() } else { Vec::new() };
    // A server or a standalone machine has only itself: no choice to offer.
    let has_machine = machines.len() > 1;
    let picked: std::collections::BTreeSet<String> = draft().platforms.split(',').filter(|p| !p.is_empty()).map(str::to_string).collect();
    let platforms: Vec<(String, String)> = state::platforms().iter().filter(|p| u(p, "id") > 0).map(|p| (u(p, "id").to_string(), state::platform_label(&s(p, "name")))).collect();
    let views: Vec<(String, String)> = super::companion::held_views();
    let apply_tab = tab.clone();
    rsx! {
        SheetHead { title: state::tr("Filter") }
        div { class: "bp-sheet",
            match open() {
                None => rsx! {
                    button { class: "bp-choice", onclick: move |_| open.set(Some("platform")),
                        {state::tr("Platform")} if !picked.is_empty() { " ({picked.len()})" }
                    }
                    if has_view {
                        button { class: "bp-choice", onclick: move |_| open.set(Some("view")),
                            {state::tr("Held")} if !draft().held.is_empty() { " ✓" }
                        }
                    }
                    if has_machine {
                        button { class: "bp-choice", onclick: move |_| open.set(Some("machine")),
                            {state::tr("Machine")} if !draft().machine.is_empty() { " ✓" }
                        }
                    }
                },
                Some("platform") => rsx! {
                    button { class: "bp-choice", onclick: move |_| open.set(None), "◀ " {state::tr("Platform")} }
                    div { class: "bp-choices",
                        for (id, name) in platforms {
                            {
                                let on = picked.contains(&id);
                                rsx! {
                                    button { key: "{id}", class: if on { "bp-choice active" } else { "bp-choice" },
                                        onclick: move |_| {
                                            let mut set: std::collections::BTreeSet<String> = draft().platforms.split(',').filter(|p| !p.is_empty()).map(str::to_string).collect();
                                            if !set.remove(&id) { set.insert(id.clone()); }
                                            draft.write().platforms = set.into_iter().collect::<Vec<_>>().join(",");
                                        },
                                        if on { "✓ " } "{name}"
                                    }
                                }
                            }
                        }
                    }
                },
                Some("machine") => rsx! {
                    button { class: "bp-choice", onclick: move |_| open.set(None), "◀ " {state::tr("Machine")} }
                    for (v, l) in machines.clone() {
                        button { key: "{v}", class: if draft().machine == v { "bp-choice active" } else { "bp-choice" }, onclick: move |_| draft.write().machine = v.clone(), "{l}" }
                    }
                },
                Some(_) => rsx! {
                    button { class: "bp-choice", onclick: move |_| open.set(None), "◀ " {state::tr("Held")} }
                    for (v, l) in views {
                        button { key: "{v}", class: if draft().held == v { "bp-choice active" } else { "bp-choice" }, onclick: move |_| draft.write().held = v.clone(), "{l}" }
                    }
                },
            }
            div { class: "bp-sheet-actions",
                button { class: "bp-choice primary", onclick: move |_| { QUERIES.write().insert(apply_tab.clone(), draft()); ui::close_modal(); }, {state::tr("Apply")} }
                button { class: "bp-choice", onclick: move |_| { let s = draft().search; draft.set(ListQuery { search: s, ..Default::default() }); }, {state::tr("Clear")} }
                button { class: "bp-choice", onclick: move |_| ui::close_modal(), {state::tr("cancel")} }
            }
        }
    }
}

/// One cover: art, name, a Start button (bottom right of the art in a grid, right of the
/// name in a list), and its menu on Y / long press / right click.
#[component]
fn Cover(item: Item) -> Element {
    let (open, menu, focus) = (item.clone(), item.clone(), item.clone());
    let play = item.play.clone();
    let initial: String = item.name.chars().next().map(|c| c.to_string()).unwrap_or_default();
    rsx! {
        div { class: "bp-card", tabindex: "0", "data-nav": "1",
            onclick: move |_| {
                if let Some(to) = open.open.clone() { navigator().push(to); }
            },
            oncontextmenu: move |e| {
                e.prevent_default();
                e.stop_propagation();
                let p = e.client_coordinates();
                item_menu(menu.clone(), p.x, p.y);
            },
            onfocus: move |_| *FOCUSED.write() = Some(focus.clone()),
            div { class: "bp-art",
                if let Some(src) = item.cover.clone() { img { loading: "lazy", src: "{src}", alt: "" } } else { div { class: "bp-noart", "{initial}" } }
                if item.held && CAN_START {
                    button { class: "bp-start", "data-start": "1", tabindex: "-1", title: state::tr("Start (X)"),
                        onclick: move |e| { e.stop_propagation(); start(play.clone()); },
                        "▶"
                    }
                }
            }
            div { class: "bp-meta",
                div { class: "bp-name", "{item.name}" }
                if !item.sub.is_empty() { div { class: "bp-sub", "{item.sub}" } }
                div { class: "bp-inline-details",
                    for (k, v) in item.details.iter().cloned() { div { span { class: "muted", "{k}" } " {v}" } }
                }
            }
        }
    }
}

/// The focused item's details, in the room the covers leave.
#[component]
fn DetailsPanel() -> Element {
    let Some(item) = FOCUSED() else { return rsx! { aside { class: "bp-details" } } };
    let play = item.play.clone();
    let menu = item.clone();
    rsx! {
        aside { class: "bp-details",
            h2 { "{item.name}" }
            if !item.sub.is_empty() { div { class: "bp-sub", "{item.sub}" } }
            div { class: "kv",
                for (k, v) in item.details.iter().cloned() { div { class: "muted", "{k}" } div { "{v}" } }
            }
            div { class: "bp-actions",
                if item.held && CAN_START {
                    button { class: "bp-choice primary", onclick: move |_| start(play.clone()), "▶ " {state::tr("Start")} }
                }
                button { class: "bp-choice",
                    onclick: move |e| { e.stop_propagation(); let p = e.client_coordinates(); item_menu(menu.clone(), p.x, p.y); },
                    {state::tr(if CAN_START { "Open with… / more" } else { "More" })}
                }
            }
        }
    }
}

// --- pages ---

/// A row of covers on a page: the related titles, releases or patched releases.
#[component]
fn Shelf(heading: String, items: Vec<Item>) -> Element {
    if items.is_empty() {
        return rsx! {};
    }
    rsx! {
        section { class: "bp-shelf",
            h3 { "{heading} " span { class: "muted", "({items.len()})" } }
            div { class: "bp-row", for it in items { Cover { key: "{it.key}", item: it } } }
        }
    }
}

/// A page's top: big cover, name, details, and Start / Open with / each disc.
#[component]
fn Hero(item: Item, target: Option<TargetProp>) -> Element {
    let initial: String = item.name.chars().next().map(|c| c.to_string()).unwrap_or_default();
    let name = item.name.clone();
    rsx! {
        div { class: "bp-hero",
            div { class: "bp-hero-art", if let Some(src) = item.cover.clone() { img { src: "{src}", alt: "" } } else { div { class: "bp-noart", "{initial}" } } }
            div { class: "bp-hero-meta",
                h1 { "{item.name}" }
                if !item.sub.is_empty() { div { class: "bp-sub", "{item.sub}" } }
                div { class: "kv", for (k, v) in item.details.iter().cloned() { div { class: "muted", "{k}" } div { "{v}" } } }
                if let Some(t) = target.map(|t| t.0) {
                    div { class: "bp-actions",
                        if let Some(g) = t.game {
                            button { class: "bp-choice primary", "data-nav": "1", autofocus: true, onclick: move |_| { spawn(async move { launch(vec![("game_id", g)], None).await }); }, "▶ " {state::tr("Start")} }
                            {
                                let head = name.clone();
                                rsx! { button { class: "bp-choice", "data-nav": "1", onclick: move |e| { e.stop_propagation(); let p = e.client_coordinates(); let head = head.clone(); spawn(async move { open_with_menu(p.x, p.y, head, ("game_id", g)).await }); }, {state::tr("Open with…")} } }
                            }
                        }
                        for (n, rom) in t.discs.iter().copied() {
                            button { key: "{rom}", class: "bp-choice", "data-nav": "1",
                                onclick: move |_| { spawn(async move { launch(vec![("rom_id", rom)], None).await }); },
                                oncontextmenu: move |e| { e.prevent_default(); e.stop_propagation(); let p = e.client_coordinates(); spawn(async move { open_with_menu(p.x, p.y, state::trf("disc {n}", &[("n", &n.to_string())]), ("rom_id", rom)).await }); },
                                "▶ " {state::trf("Disc {n}", &[("n", &n.to_string())])}
                            }
                        }
                    }
                } else if item.held && CAN_START {
                    div { class: "muted", {state::tr("loading…")} }
                }
            }
        }
    }
}

/// `Target` as a prop (it has no `PartialEq` of its own worth deriving elsewhere).
#[derive(Clone, Debug)]
pub struct TargetProp(Target);

impl PartialEq for TargetProp {
    fn eq(&self, o: &Self) -> bool {
        self.0.game == o.0.game && self.0.discs == o.0.discs
    }
}

fn page_wrap(body: Element) -> Element {
    rsx! { div { class: "bp-page", {body} } }
}

#[component]
pub fn BpCross(igdb: String) -> Element {
    let key = igdb.clone();
    let data = use_resource(move || {
        let igdb = key.clone();
        async move { api::get(&format!("/api/v1/catalog/titles?igdb={}&limit=100", api::encode(&igdb))).await }
    });
    let v = match &*data.read() {
        None => return page_wrap(rsx! { div { class: "muted", {state::tr("loading…")} } }),
        Some(Err(e)) => return page_wrap(rsx! { div { class: "card bad", "{e.message}" } }),
        Some(Ok(v)) => v.clone(),
    };
    let titles: Vec<Item> = api::items(&v).iter().map(title_item).collect();
    let Some(first) = titles.first().cloned() else { return page_wrap(rsx! { div { class: "muted", {state::tr("nothing here")} } }) };
    let hero = Item { open: None, details: vec![(state::tr("platforms"), titles.iter().map(|t| t.sub.clone()).filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · "))], ..first };
    page_wrap(rsx! {
        Hero { item: hero, target: None }
        Shelf { heading: state::tr("On each platform"), items: titles }
    })
}

#[component]
pub fn BpTitle(id: u64) -> Element {
    rsx! { for k in [id] { TitlePage { key: "{k}", id: k } } }
}

#[component]
fn TitlePage(id: u64) -> Element {
    let data = use_resource(move || async move { api::get(&format!("/api/v1/catalog/titles/{id}")).await });
    let target = use_resource(move || async move { resolve(&Play::Title(id)).await });
    let t = match &*data.read() {
        None => return page_wrap(rsx! { div { class: "muted", {state::tr("loading…")} } }),
        Some(Err(e)) => return page_wrap(rsx! { div { class: "card bad", "{e.message}" } }),
        Some(Ok(t)) => t.clone(),
    };
    let mut hero = title_item(&t);
    hero.open = None;
    let mut extra = Vec::new();
    for (k, l) in [("genres", "genre"), ("franchises", "franchise")] {
        let names: Vec<String> = api::arr(&t, k).iter().map(|g| s(g, "name")).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        detail(&mut extra, l, names.join(", "));
    }
    hero.details.extend(extra);
    let every = api::arr(&t, "releases").to_vec();
    let own: Vec<Item> = every.iter().filter(|r| api::so(r, "material").is_none() && !super::patched::is_patched(r)).map(release_item).collect();
    let patched: Vec<Item> = every.iter().filter(|r| super::patched::is_patched(r)).map(release_item).chain(api::arr(&t, "linked").iter().map(patched_item)).collect();
    let extras: Vec<Item> = every.iter().filter(|r| api::so(r, "material").is_some() && !super::patched::is_patched(r)).map(release_item).collect();
    let derived: Vec<Item> = api::arr(&t, "derived").iter().filter(|d| api::uo(d, "id").is_some()).map(|d| Item { open: Some(Route::BpTitle { id: u(d, "id") }), play: Play::Title(u(d, "id")), ..title_item(d) }).collect();
    let tgt = target.read().clone().flatten().map(TargetProp);
    page_wrap(rsx! {
        Hero { item: hero, target: tgt }
        Shelf { heading: state::tr("Releases"), items: own }
        Shelf { heading: state::tr("Patched releases"), items: patched }
        Shelf { heading: state::tr("Related titles"), items: derived }
        Shelf { heading: state::tr("DLC, updates and extras"), items: extras }
    })
}

fn patched_item(p: &Value) -> Item {
    let id = u(p, "id");
    let platform = state::platform_label(&s(p, "platform"));
    let mut details = Vec::new();
    detail(&mut details, "platform", platform.clone());
    detail(&mut details, "kind", s(p, "game_kind"));
    detail(&mut details, "languages", s(p, "languages"));
    Item { key: format!("p{id}"), name: s(p, "name"), sub: platform, cover: None, open: Some(Route::BpRelease { id }), held: u(p, "local_games") > 0, play: Play::Release(id), details }
}

#[component]
pub fn BpRelease(id: u64) -> Element {
    rsx! { for k in [id] { ReleasePage { key: "{k}", id: k } } }
}

#[component]
fn ReleasePage(id: u64) -> Element {
    let data = use_resource(move || async move { api::get(&format!("/api/v1/catalog/releases/{id}")).await });
    let r = match &*data.read() {
        None => return page_wrap(rsx! { div { class: "muted", {state::tr("loading…")} } }),
        Some(Err(e)) => return page_wrap(rsx! { div { class: "card bad", "{e.message}" } }),
        Some(Ok(r)) => r.clone(),
    };
    let title_id = api::uo(&r, "title_id");
    rsx! { ReleaseBody { release: r, title_id } }
}

#[component]
fn ReleaseBody(release: Value, title_id: Option<u64>) -> Element {
    let r = release;
    let id = u(&r, "id");
    let siblings = use_resource(move || async move {
        match title_id {
            Some(t) => api::get(&format!("/api/v1/catalog/releases?title_id={t}&view=collection&limit=200")).await.map(|v| api::items(&v)).unwrap_or_default(),
            None => Vec::new(),
        }
    });
    let mut hero = release_item(&r);
    hero.open = None;
    if let Some(t) = api::so(&r, "title_name") {
        hero.details.insert(0, (state::tr("title"), t));
    }
    detail(&mut hero.details, "players", match (api::uo(&r, "min_players"), api::uo(&r, "max_players")) {
        (Some(a), Some(b)) if a != b => format!("{a}–{b}"),
        (_, Some(b)) => b.to_string(),
        _ => String::new(),
    });
    let ra: Vec<String> = api::arr(&r, "ra_games").iter().filter_map(|g| api::so(g, "title")).collect();
    detail(&mut hero.details, "RetroAchievements", ra.join(", "));
    let games = api::arr(&r, "games").to_vec();
    let first = games.first().map(|g| TargetProp(target_of_game(g)));
    // A second held copy (another dump, another directory) gets its own row of buttons.
    let more_copies: Vec<(String, Target)> = games.iter().skip(1).map(|g| (format!("{} · {}", s(g, "name"), api::so(g, "directory_path").unwrap_or_default()), target_of_game(g))).collect();
    let others: Vec<Item> = siblings.read().clone().unwrap_or_default().iter().filter(|x| u(x, "id") != id && !super::patched::is_patched(x)).map(release_item).collect();
    let patched: Vec<Item> = api::arr(&r, "patched").iter().map(patched_item).collect();
    let back_to_title = title_id.map(|t| Item { key: format!("t{t}"), name: api::so(&r, "title_name").unwrap_or_else(|| state::tr("title")), sub: state::tr("all releases of this title"), cover: hero.cover.clone(), open: Some(Route::BpTitle { id: t }), held: false, play: Play::Title(t), details: Vec::new() });
    page_wrap(rsx! {
        Hero { item: hero, target: first }
        if !more_copies.is_empty() {
            section { class: "bp-shelf",
                h3 { {state::tr("Other copies")} }
                for (label, t) in more_copies {
                    div { class: "bp-actions",
                        span { class: "muted", "{label}" }
                        if let Some(g) = t.game {
                            button { class: "bp-choice", "data-nav": "1", onclick: move |_| { spawn(async move { launch(vec![("game_id", g)], None).await }); }, "▶ " {state::tr("Start")} }
                        }
                    }
                }
            }
        }
        Shelf { heading: state::tr("Other releases of this title"), items: others }
        Shelf { heading: state::tr("Patched releases"), items: patched }
        if let Some(t) = back_to_title { Shelf { heading: state::tr("Title"), items: vec![t] } }
    })
}
