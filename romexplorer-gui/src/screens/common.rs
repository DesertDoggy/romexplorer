//! What several catalog screens share, ported from the console: the filters (platform
//! combo, view, release kinds, provider, facets), release columns / cards, the keyword-tag
//! and set actions (with the set picker), "Open with ..." items, and the release menu.

use std::rc::Rc;
use std::sync::RwLock;

use dioxus::prelude::*;
use serde_json::{json, Value};

use crate::api::{self, s, u};
use crate::components::combo::ComboOpt;
use crate::components::{Card, Column, Filter, GridSpec};
use crate::html::{self, badge, esc};
use crate::state;
use crate::ui::{self, MenuItem};
use crate::Route;

// --- facets (genres, franchises, companies, keywords with title counts) ---

static FACETS: RwLock<Option<Value>> = RwLock::new(None);

pub async fn load_facets(force: bool) {
    if !force && FACETS.read().map(|f| f.is_some()).unwrap_or(false) {
        return;
    }
    if let Ok(v) = api::get("/api/v1/catalog/facets").await {
        *FACETS.write().unwrap_or_else(|p| p.into_inner()) = Some(v);
    }
}

pub fn forget_facets() {
    *FACETS.write().unwrap_or_else(|p| p.into_inner()) = None;
}

fn facets() -> Value {
    FACETS.read().ok().and_then(|f| f.clone()).unwrap_or_default()
}

pub const FACET_KEYS: [&str; 21] = [
    "genre_major", "genre_major_logic", "genres", "genre_logic", "franchises", "franchise_logic", "companies", "company_logic", "keywords", "keyword_logic", "tags", "tag_logic", "tag_source", "region", "language",
    "display_aspect", "display_widescreen", "display_res", "display_scan", "display_refresh", "display_state",
];

/// The display filters' keys (`api::display::filter_sql` on the server).
pub const DISPLAY_KEYS: [&str; 6] = ["display_aspect", "display_widescreen", "display_res", "display_scan", "display_refresh", "display_state"];

/// Display filters: each release's value, recorded or its system's default; a title passes
/// when one of its game releases does.
pub fn display_filters() -> Vec<Filter> {
    let vocab = |key: &'static str| -> Vec<String> { state::vocab().get("display").and_then(|d| d.get(key)).and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default() };
    let select = move |key: &'static str, label: &str, any: &str, values: Rc<dyn Fn() -> Vec<(String, String)>>| {
        let any = any.to_string();
        Filter::Select {
            key,
            label: label.to_string(),
            options: Rc::new(move || {
                let mut o = vec![(String::new(), any.clone())];
                o.extend(values());
                o
            }),
        }
    };
    vec![
        select("display_aspect", &state::tr("aspect"), &state::tr("any aspect"), Rc::new(move || vocab("aspects").into_iter().map(|a| (a.clone(), a)).collect())),
        select("display_widescreen", &state::tr("widescreen"), &state::tr("any widescreen"), Rc::new(|| ["none", "option", "native", "patch"].iter().map(|w| (w.to_string(), super::display::value_label("widescreen", Some(w)))).collect())),
        select("display_res", &state::tr("resolutions"), &state::tr("any resolution"), Rc::new(move || vocab("resolutions").into_iter().map(|r| (r.clone(), super::display::value_label("modes", Some(&r)))).collect())),
        select("display_scan", &state::tr("scan"), &state::tr("progressive or interlaced"), Rc::new(|| vec![("p".into(), state::tr("has a progressive mode")), ("i".into(), state::tr("has an interlaced mode"))])),
        select("display_refresh", &state::tr("refresh rate"), &state::tr("any refresh rate"), Rc::new(move || vocab("refresh").into_iter().map(|r| (r.clone(), super::display::value_label("refresh", Some(&r)))).collect())),
        select(
            "display_state",
            &state::tr("display values"),
            &state::tr("default or confirmed"),
            Rc::new(|| {
                [("aspect", "aspect"), ("widescreen", "widescreen"), ("modes", "resolutions"), ("refresh", "refresh rate")]
                    .iter()
                    .flat_map(|(f, label)| {
                        let l = state::tr(label);
                        [(format!("{f}:default"), state::trf("{field}: default (undecided)", &[("field", &l)])), (format!("{f}:confirmed"), state::trf("{field}: confirmed", &[("field", &l)]))]
                    })
                    .collect()
            }),
        ),
    ]
}

/// The display columns: sortable, a release's value (a title: its first release's).
pub fn display_columns() -> Vec<Column> {
    vec![
        Column::text("display_aspect", &state::tr("aspect")).sort("aspect"),
        Column::html("display_modes", &state::tr("resolutions"), |r| api::so(r, "display_modes").map(|m| format!("<span style=\"font-size:12px\">{}</span>", esc(&super::display::value_label("modes", Some(&m))))).unwrap_or_default()).sort("resolutions"),
        Column::html("display_refresh", &state::tr("Hz"), |r| esc(&s(r, "display_refresh").split(',').collect::<Vec<_>>().join("/"))).sort("refresh").num(),
    ]
}

fn named_counts(key: &str) -> Vec<ComboOpt> {
    api::arr(&facets(), key).iter().filter(|x| u(x, "titles") > 0).map(|x| ComboOpt { value: s(x, "name"), label: s(x, "name"), group: None, count: Some(u(x, "titles")) }).collect()
}

pub fn facet_filters() -> Vec<Filter> {
    vec![
        Filter::Combo {
            key: "genre_major",
            label: state::tr("genre"),
            multi: true,
            logic_key: Some("genre_major_logic"),
            options: Rc::new(|| {
                let f = facets();
                api::arr(&f, "genre_majors")
                    .iter()
                    .filter_map(|m| m.as_str())
                    .map(|m| ComboOpt { value: m.into(), label: m.into(), group: None, count: Some(api::arr(&f, "genres").iter().filter(|g| s(g, "major") == m).map(|g| u(g, "titles")).sum()) })
                    .collect()
            }),
        },
        Filter::Combo {
            key: "genres",
            label: state::tr("subgenres"),
            multi: true,
            logic_key: Some("genre_logic"),
            options: Rc::new(|| api::arr(&facets(), "genres").iter().filter(|x| u(x, "titles") > 0).map(|x| ComboOpt { value: s(x, "name"), label: s(x, "name"), group: api::so(x, "major"), count: Some(u(x, "titles")) }).collect()),
        },
        Filter::Combo { key: "franchises", label: state::tr("franchises"), multi: true, logic_key: Some("franchise_logic"), options: Rc::new(|| named_counts("franchises")) },
        Filter::Combo { key: "companies", label: state::tr("companies"), multi: true, logic_key: Some("company_logic"), options: Rc::new(|| named_counts("companies")) },
        Filter::Combo { key: "keywords", label: state::tr("keywords"), multi: true, logic_key: Some("keyword_logic"), options: Rc::new(|| named_counts("keywords")) },
        // Keyword tags: the user's and the auto-tagger's, apart from providers' keywords.
        Filter::Combo { key: "tags", label: state::tr("keyword tags"), multi: true, logic_key: Some("tag_logic"), options: Rc::new(|| named_counts("tags")) },
        Filter::Select {
            key: "tag_source",
            label: state::tr("tag source"),
            options: Rc::new(|| vec![(String::new(), state::tr("tags from anyone")), ("user".into(), state::tr("your tags")), ("auto".into(), state::tr("automatic tags"))]),
        },
        Filter::Select {
            key: "region",
            label: state::tr("region"),
            options: Rc::new(|| {
                let mut o = vec![(String::new(), state::tr("any region"))];
                o.extend(api::arr(&state::vocab(), "regions").iter().map(|r| {
                    let code = s(r, "code");
                    let flag = html::region_flag(&code);
                    (code.clone(), format!("{}{code} — {}", if flag.is_empty() { String::new() } else { format!("{flag} ") }, s(r, "name")))
                }));
                o
            }),
        },
        Filter::Select {
            key: "language",
            label: state::tr("language"),
            options: Rc::new(|| {
                let mut o = vec![(String::new(), state::tr("any language"))];
                o.extend(api::arr(&state::vocab(), "languages").iter().map(|l| {
                    let code = s(l, "code");
                    let flag = html::lang_flag(&code);
                    (code.clone(), format!("{}{code} — {}", if flag.is_empty() { String::new() } else { format!("{flag} ") }, s(l, "english")))
                }));
                o
            }),
        },
    ]
    .into_iter()
    .chain(display_filters())
    .collect()
}

/// One entry per system: the members of a platform group (Famicom / NES, ...) are offered as
/// the group's first platform, labelled with every member's name; the server matches them all.
fn platform_filter_options() -> Vec<ComboOpt> {
    let all = state::platforms();
    let groups: Vec<Vec<u64>> = state::vocab().get("platform_groups").and_then(|g| g.as_array()).map(|g| g.iter().map(|ids| ids.as_array().into_iter().flatten().filter_map(|id| id.as_u64()).collect()).collect()).unwrap_or_default();
    let name_of = |id: u64| all.iter().find(|p| u(p, "id") == id).map(|p| s(p, "name")).filter(|n| !n.is_empty());
    all.iter()
        .filter(|p| u(p, "id") > 0 && !groups.iter().any(|g| g.iter().skip(1).any(|id| *id == u(p, "id"))))
        .map(|p| {
            let id = u(p, "id");
            let mut names: Vec<String> = match groups.iter().find(|g| g.first() == Some(&id)) {
                Some(group) => group.iter().filter_map(|member| name_of(*member)).collect(),
                None => vec![s(p, "name")],
            };
            names.dedup();
            ComboOpt::new(id.to_string(), names.join(" / "))
        })
        .collect()
}

pub fn platform_filter() -> Filter {
    // Tick boxes: any of the ticked platforms (the API's `platforms`); the key stays
    // `platform`, so a filter remembered from before still applies.
    Filter::Combo { key: "platform", label: state::tr("platform"), multi: true, logic_key: None, options: Rc::new(platform_filter_options) }
}

/// How a list is ordered and whether RetroAchievements narrows it. Where the rows are held is
/// the Held and Machine filters' ([`held_filter`], [`machine_filter`]); the API still reads
/// the old `local` / `server` / `missing_on_server` views.
pub fn view_filter() -> Filter {
    Filter::Select {
        key: "view",
        label: state::tr("view"),
        options: Rc::new(|| vec![("collection_local_first".into(), state::tr("collection (local first)")), ("collection".into(), state::tr("collection")), ("local_ra_local_first".into(), state::tr("local + RetroAchievements (local first)")), ("local_ra_ra_first".into(), state::tr("local + RetroAchievements (RA first)")), ("ra_only".into(), state::tr("RetroAchievements only"))]),
    }
}

/// The machines the Machine filter offers (the vocabulary's `machines`): this machine first,
/// then in client mode the main server and the other clients.
fn vocab_machines() -> Vec<Value> {
    api::arr(&state::vocab(), "machines").to_vec()
}

/// Held or not, on the machine the Machine filter picks. On a client, also the backlog still
/// to copy up to the main server.
pub fn held_filter() -> Filter {
    Filter::Select {
        key: "held",
        label: state::tr("Held"),
        options: Rc::new(held_options),
    }
}

/// [`held_filter`]'s choices, also Big Picture's.
pub fn held_options() -> Vec<(String, String)> {
    let mut o: Vec<(String, String)> = [("", "anywhere"), ("held", "held"), ("not_held", "not held (catalogue only)")].iter().map(|(v, l)| (v.to_string(), state::tr(l))).collect();
    if vocab_machines().iter().any(|m| s(m, "role") == "main") {
        o.push(("missing_on_main".into(), state::tr("not on the main server")));
    }
    o
}

/// Whose holdings count: this machine, or on a client also the main server and each other
/// client. A server and a standalone machine have only themselves, so the one choice is
/// "this machine".
pub fn machine_filter() -> Filter {
    Filter::Select {
        key: "machine",
        label: state::tr("Machine"),
        options: Rc::new(machine_options),
    }
}

/// [`machine_filter`]'s choices, also Big Picture's.
pub fn machine_options() -> Vec<(String, String)> {
    let machines = vocab_machines();
    let this = machines.first().map(|m| s(m, "name")).filter(|n| !n.is_empty());
    let this_label = match this {
        Some(n) => state::trf("this machine ({name})", &[("name", &n)]),
        None => state::tr("this machine"),
    };
    if machines.len() <= 1 {
        return vec![(String::new(), this_label)];
    }
    let mut o = vec![(String::new(), state::tr("any machine")), (romexplorer_types::mobile::THIS_MACHINE.to_string(), this_label)];
    for m in machines.iter().skip(1) {
        let role = if s(m, "role") == "main" { state::tr("main server") } else { state::tr("client") };
        o.push((s(m, "id"), format!("{} ({role})", s(m, "name"))));
    }
    o
}

/// What the releases are: games and other software unless asked for the music, films and
/// scans the DATs also list.
pub fn material_filter() -> Filter {
    Filter::Select {
        key: "material",
        label: state::tr("material"),
        options: Rc::new(|| {
            [("", "games & software"), ("unbundled", "DLC, updates & themes not bundled with a game"), ("dlc", "DLC & add-ons"), ("update", "updates & patches"), ("theme", "themes & avatars")]
                .iter()
                .map(|(v, l)| (v.to_string(), state::tr(l)))
                .collect()
        }),
    }
}

/// A title page's sections below its releases, in this order.
pub const MATERIAL_SECTIONS: [(&str, &str); 12] =
    [("dlc", "DLC & add-ons ({n})"), ("update", "Updates & patches ({n})"), ("theme", "Themes & avatars ({n})"), ("patch", "Patches ({n})"), ("trainer", "Trainers ({n})"), ("save", "Saves ({n})"), ("mappack", "Map packs ({n})"), ("audio", "Audio ({n})"), ("video", "Video ({n})"), ("document", "Documents ({n})"), ("image", "Images ({n})"), ("source", "Source code ({n})")];

/// One list per kind of `releases` that has any, with its heading: what a title or release
/// page shows below the game's own releases.
pub fn material_sections(releases: &[Value]) -> Vec<(String, Rc<GridSpec>)> {
    MATERIAL_SECTIONS
        .iter()
        .filter_map(|(material, heading)| {
            let items: Vec<Value> = releases.iter().filter(|r| api::so(r, "material").as_deref() == Some(*material)).cloned().collect();
            if items.is_empty() {
                return None;
            }
            let heading = state::trf(heading, &[("n", &items.len().to_string())]);
            let mut spec = GridSpec::new(&format!("material_{material}"), release_columns(), super::widgets::static_fetch(items))
                .link(|r| Some(Route::Release { id: u(r, "id") }))
                .local(held_class)
                .context(|r, _| async move { release_context(u(&r, "id"), s(&r, "name"), None, None, None).await })
                .cards("releases", release_card);
            spec.search = false;
            spec.page_size = 500;
            Some((heading, Rc::new(spec)))
        })
        .collect()
}

/// What a release is (the API's catalog `KINDS`), in the order the pop-up lists them.
pub const RELEASE_KINDS: [(&str, &str); 15] = [
    ("retail", "retail"),
    ("unlicensed", "unlicensed"),
    ("aftermarket", "aftermarket"),
    ("homebrew", "homebrew"),
    ("arcade", "arcade ports"),
    ("wiiware", "WiiWare"),
    ("translation", "translations"),
    ("hack", "hacks"),
    ("mod", "mods"),
    ("patched", "other patched (RetroAchievements only)"),
    ("pirate", "pirate copies"),
    ("demo", "demos"),
    ("beta", "betas"),
    ("proto", "prototypes"),
    ("subset", "RetroAchievements subsets"),
];

/// What the filter's older "commercial" meant: everything sold as it was.
const COMMERCIAL_KINDS: [&str; 10] = ["retail", "unlicensed", "aftermarket", "homebrew", "arcade", "wiiware", "pirate", "demo", "beta", "proto"];

/// A `kinds` value as the kinds it ticks: a comma list, or one of the three choices the
/// filter used to be.
pub fn kinds_of(value: &str) -> Vec<String> {
    let all = || RELEASE_KINDS.iter().map(|(k, _)| k.to_string());
    match value {
        "" | "all" => all().collect(),
        "patches" => all().filter(|k| k != "subset").collect(),
        list => {
            let mut out: Vec<String> = Vec::new();
            for k in list.split(',').map(str::trim).filter(|k| !k.is_empty()) {
                let more: Vec<String> = if k == "commercial" { COMMERCIAL_KINDS.iter().map(|c| c.to_string()).collect() } else { vec![k.to_string()] };
                for m in more {
                    if !out.contains(&m) {
                        out.push(m);
                    }
                }
            }
            out
        }
    }
}

/// A Titles list row's kind (`TitleRow::bucket`): nothing for retail, else a badge -- a
/// translation row names its languages.
pub fn bucket_badge(r: &Value) -> String {
    let bucket = s(r, "bucket");
    let (label, cls) = match bucket.as_str() {
        "unlicensed" => ("unlicensed", "dim"),
        "aftermarket" => ("aftermarket", "dim"),
        "homebrew" => ("homebrew", ""),
        "arcade" => ("arcade port", ""),
        "wiiware" => ("WiiWare", ""),
        "translation" => ("translation", "ok"),
        "hack" => ("hack", "warn"),
        "mod" => ("mod", "warn"),
        "patched" => ("patched ROMs", "warn"),
        "pirate" => ("pirate", "warn"),
        "demo" => ("demo", "dim"),
        "beta" => ("beta", "dim"),
        "proto" => ("prototype", "dim"),
        "subset" => ("subset", "dim"),
        _ => return String::new(),
    };
    let langs = api::so(r, "row_languages").filter(|_| bucket == "translation").map(|l| format!(" ({})", l.replace(',', ", "))).unwrap_or_default();
    badge(&format!("{}{langs}", state::tr(label)), cls)
}

/// The titles of Titles-list rows (their `row_id`s), each once: for what works on whole
/// titles -- keyword tags, merging, IGDB metadata.
pub async fn title_ids_of_rows(row_ids: Vec<u64>) -> Vec<u64> {
    match api::post("/api/v1/catalog/title_rows", json!({ "row_ids": row_ids })).await {
        Ok(v) => {
            let mut ids: Vec<u64> = v.as_array().into_iter().flatten().map(|r| u(r, "title_id")).filter(|id| *id > 0).collect();
            ids.dedup();
            let mut seen = std::collections::HashSet::new();
            ids.retain(|id| seen.insert(*id));
            ids
        }
        Err(e) => {
            ui::toast(e.message, true);
            Vec::new()
        }
    }
}

/// Tick boxes, any of which may match: with this many kinds a pulldown of combinations
/// would never have the one wanted.
pub fn kinds_filter() -> Filter {
    Filter::Checks {
        key: "kinds",
        label: state::tr("kinds"),
        options: Rc::new(|| RELEASE_KINDS.iter().map(|(v, l)| (v.to_string(), state::tr(l))).collect()),
        extra: Rc::new(|| vec![("guessed".into(), state::tr("guessed language"))]),
        // Never touched: what the setting says. Nothing ticked is no filter, so "all" is
        // every box empty and ticking "commercial" shows commercial releases -- with every
        // box ticked to start with, that same click hid them.
        ticked: Rc::new(|value| {
            let value = if value.is_empty() { state::release_kinds_default() } else { value.to_string() };
            if value.is_empty() || value == "all" { Vec::new() } else { kinds_of(&value) }
        }),
        none: "all",
    }
}

/// The badge of a game that is not a commercial release; a translation is not a hack.
pub fn kind_badge(r: &Value) -> String {
    let kind = s(r, "game_kind");
    let (label, cls) = match kind.as_str() {
        "translation" => ("translation", "ok"),
        "hack" => ("hack", "warn"),
        "mod" => ("mod", "warn"),
        "homebrew" => ("homebrew", ""),
        "arcade" => ("arcade port", ""),
        "wiiware" => ("WiiWare", ""),
        "patched" => ("patched ROMs", "warn"),
        _ => ("", ""),
    };
    // Placed under its game by name alone: said, until the user confirms or unlinks it.
    let suggested = if s(r, "base_status") == "suggested" {
        format!(r#"<span class="badge dim" title="{}">{}</span>"#, esc(&state::tr("placed under this game by name; confirm or unlink it on its page")), esc(&state::tr("suggested")))
    } else {
        String::new()
    };
    format!("{}{suggested}", if label.is_empty() { String::new() } else { badge(&state::tr(label), cls) })
}

/// A title's category, or what it is when it holds no release of a game as it was sold: a
/// title of nothing but hacks is not a "base game".
pub fn title_kind_badge(t: &Value) -> String {
    match api::so(t, "game_kind") {
        Some(kind) => kind_badge(&serde_json::json!({ "game_kind": kind })),
        None => html::cbadge("category", &s(t, "category"), None, None),
    }
}

/// A patched release's language is the one it was translated into: stated by its name, or
/// a guess, which is marked.
pub fn release_lang_badges(r: &Value) -> String {
    let languages = s(r, "languages");
    let guessed = !languages.is_empty() && s(r, "languages_state") == "guessed";
    format!("{}{}", html::lang_badges(&languages), if guessed { format!(r#"<span class="muted" title="{}">?</span>"#, esc(&state::tr("guessed from a folder or a loose word, not stated by the name"))) } else { String::new() })
}

pub fn status_filter() -> Filter {
    Filter::Select { key: "status", label: state::tr("status"), options: Rc::new(|| vec![(String::new(), state::tr("any status")), ("complete".into(), state::tr("complete (all files present)")), ("missing".into(), state::tr("missing files"))]) }
}

pub fn include_deleted_filter(label: &str) -> Filter {
    Filter::Check { key: "include_deleted", label: state::tr(label), off: String::new() }
}

// --- badges / columns ---

pub fn local_badge(n: u64) -> String {
    if n > 0 {
        badge(&state::trf("{n} local", &[("n", &n.to_string())]), "ok")
    } else {
        badge(&state::tr("not local"), "dim")
    }
}

/// Which ownership state a catalog row is in, as a CSS class: `unsynced` (the main server
/// hasn't got it but somebody has -- the loud one, the backlog still to copy up), `local`
/// (held here and on the main server), `remoteonly` (on the main server, not here), or
/// nothing.
///
/// Only the **main** server decides `unsynced`: a copy on a peer never quiets it, and a
/// copy that exists *only* on a peer is just as loud as one that exists only here -- either
/// way the main server is missing it. Rows from lists that don't carry the mirror counts
/// (games, roms, files) have no `remote_games` field at all and keep the old two-state
/// highlight; claiming "not on the server" there would be a false alarm on every row.
pub fn held_class(r: &Value) -> &'static str {
    let local = u(r, "local_games") > 0;
    let Some(main) = api::uo(r, "remote_games") else {
        return if local { "local" } else { "" };
    };
    match (local, main > 0, u(r, "peer_games") > 0) {
        (_, false, true) | (true, false, false) => "unsynced",
        (true, true, _) => "local",
        (false, true, _) => "remoteonly",
        (false, false, false) => "",
    }
}

/// The ownership badges for a catalog row: where it is held, and -- when the main server
/// hasn't got it -- that it still needs copying up. Falls back to [`local_badge`] for rows
/// without the mirror counts.
/// The two answers every catalogue row gives first, loud on purpose (the console's
/// `statusBadges`): LOCAL (with a count for several copies) and RA (RetroAchievements supports
/// it); in client mode beside them, only on the server / not on the server / only on a peer.
/// `no_ra`: the RetroAchievements tab, where every row is RA.
pub fn status_badges(r: &Value, no_ra: bool) -> String {
    let n = u(r, "local_games");
    let mut out: Vec<String> = Vec::new();
    if n > 0 {
        out.push(format!(r#"<span class="lb local" title="{}">{}{}</span>"#, esc(&state::trf("{n} local cop(ies)", &[("n", &n.to_string())])), esc(&state::tr("LOCAL")), if n > 1 { format!(" ×{n}") } else { String::new() }));
    }
    if let Some(main) = r.get("remote_games").and_then(|v| v.as_u64()) {
        if n == 0 && main > 0 {
            out.push(format!(r#"<span class="lb server">{}</span>"#, esc(&state::tr("ON SERVER"))));
        } else if n > 0 && main == 0 {
            out.push(format!(r#"<span class="lb warn">{}</span>"#, esc(&state::tr("NOT ON SERVER"))));
        } else if n == 0 && main == 0 && u(r, "peer_games") > 0 {
            out.push(format!(r#"<span class="lb peer">{}</span>"#, esc(&state::tr("ON A PEER"))));
        }
    }
    if api::b(r, "ra") && !no_ra {
        out.push(format!(r#"<span class="lb ra" title="{}">RA</span>"#, esc(&state::tr("RetroAchievements supports it"))));
    }
    out.join(" ")
}

/// The list column for [`status_badges`], first in every catalogue list.
pub fn status_column() -> Column {
    Column::html("status", "", |r| status_badges(r, false)).sort("local_games")
}

pub fn held_badge(r: &Value) -> String {
    let n = u(r, "local_games");
    let Some(main) = api::uo(r, "remote_games") else {
        return local_badge(n);
    };
    let peer = u(r, "peer_games");
    let n_local = state::trf("{n} local", &[("n", &n.to_string())]);
    let mut out = match (n > 0, main > 0) {
        (true, false) => format!("{} {}", badge(&n_local, "warn"), badge(&state::tr("not on server"), "warn")),
        (true, true) => badge(&n_local, "ok"),
        (false, true) => badge(&state::tr("on server"), "dim"),
        // Only a peer has it: the main server is still missing it, so this is loud too.
        (false, false) if peer > 0 => format!("{} {}", badge(&state::tr("on a peer"), "warn"), badge(&state::tr("not on server"), "warn")),
        (false, false) => badge(&state::tr("not local"), "dim"),
    };
    if peer > 0 && (n > 0 || main > 0) {
        out.push(' ');
        out.push_str(&badge(&state::trf("+{n} peer", &[("n", &peer.to_string())]), "dim"));
    }
    out
}

pub fn edition_badges(r: &Value) -> String {
    let mut v = Vec::new();
    if api::b(r, "is_budget") {
        v.push(badge(&state::tr("budget"), "ok"));
    }
    if api::b(r, "is_special_edition") {
        v.push(badge(&state::tr("special edition"), "warn"));
    }
    if api::b(r, "is_aftermarket") {
        v.push(badge(&state::tr("aftermarket"), ""));
    }
    if api::b(r, "is_rerelease") {
        v.push(badge(&state::tr("re-release"), "dim"));
    }
    v.join(" ")
}

pub fn release_chips(r: &Value) -> String {
    let mut v = String::new();
    if u(r, "local_verified") > 0 {
        v.push_str(&badge(&state::tr("✓ verified"), "ok"));
    }
    if u(r, "local_missing") > 0 {
        v.push_str(&badge(&state::trf("missing {n}", &[("n", &u(r, "local_missing").to_string())]), "bad"));
    }
    if api::b(r, "is_budget") {
        v.push_str(&badge(&state::tr("budget"), "ok"));
    }
    if api::b(r, "is_special_edition") {
        v.push_str(&badge(&state::tr("special"), "warn"));
    }
    if api::b(r, "is_aftermarket") {
        v.push_str(&badge(&state::tr("aftermarket"), ""));
    }
    if api::b(r, "is_rerelease") {
        v.push_str(&badge(&state::tr("re-release"), "dim"));
    }
    if u(r, "combined_count") > 0 {
        v.push_str(&badge(&state::trf("+{n} combined", &[("n", &u(r, "combined_count").to_string())]), ""));
    }
    v
}

pub fn release_name_html(r: &Value) -> String {
    let ed = [kind_badge(r), edition_badges(r)].into_iter().filter(|b| !b.is_empty()).collect::<Vec<_>>().join(" ");
    let sub = state::release_sub_name(r).map(|o| format!(r#"<div class="localname">{}</div>"#, esc(&o))).unwrap_or_default();
    format!("{}{}{sub}", esc(&state::release_display_name(r)), if ed.is_empty() { String::new() } else { format!(" {ed}") })
}

/// What a catalogue size is: the raw dump as the DATs list it.
pub const RAW_NOTE: &str = "the raw dump as the DATs list it (one dump; CHD / RVZ entries left out) — not the size of a zip, 7z or compressed disc image";

/// A title's size: with several releases, all of them and its first (by region priority) on
/// two lines, "—" where there is nothing; with one, the one number.
pub fn size_cell(all: Option<u64>, first: Option<u64>, several: bool) -> String {
    let f = |n: Option<u64>| n.filter(|n| *n > 0).map(|n| html::fmt_size(Some(n))).unwrap_or_else(|| "—".into());
    if several {
        format!("<div>{}</div><div class=\"muted\">{}</div>", esc(&state::trf("all: {size}", &[("size", &f(all))])), esc(&state::trf("first: {size}", &[("size", &f(first))])))
    } else {
        all.map(|n| html::fmt_size(Some(n))).unwrap_or_default()
    }
}

/// What put a set's row in it: its letter, its short name and what it stands for -- D a
/// definitive version (a family), C a cross-platform game, T a title, R a release.
pub fn entry_type(r: &Value) -> (&'static str, String, String) {
    match s(r, "set_via").as_str() {
        "family" => ("D", state::tr("Def"), state::tr("definitive version")),
        "game" => ("C", state::tr("Cross"), state::tr("cross-platform game")),
        "title" => ("T", state::tr("Title"), state::tr("title")),
        _ => ("R", state::tr("Release"), state::tr("release")),
    }
}

/// [`entry_type`] as the list's word.
pub fn entry_type_badge(r: &Value) -> String {
    let (letter, word, label) = entry_type(r);
    format!(r#"<span class="badge entry-type {letter}" title="{}">{}</span>"#, esc(&label), esc(&word))
}

/// [`entry_type`] as the round emblem on a cover.
pub fn entry_type_emblem(r: &Value) -> String {
    let (letter, word, label) = entry_type(r);
    format!(r#"<span class="entry-emblem {letter}" title="{}: {}">{letter}</span>"#, esc(&word), esc(&label))
}

pub fn release_card(r: &Value, _mode: &str) -> Card {
    let sub = state::release_sub_name(r).map(|o| format!(r#"<div class="localname">{}</div>"#, esc(&o))).unwrap_or_default();
    Card {
        title: state::release_display_name(r),
        media: api::uo(r, "cover_media_id"),
        held: held_class(r),
        flags: status_badges(r, false),
        chips: format!("{}{}", kind_badge(r), release_chips(r)),
        sub: format!("{sub}{}{}", html::cbadge("platform", &s(r, "platform"), None, None), html::region_badges(&s(r, "region"))),
        ..Default::default()
    }
}

/// A link to another page inside a cell (doesn't trigger the row's own click).
#[component]
pub fn CellLink(to: Route, text: String) -> Element {
    rsx! {
        span { class: "link", onclick: move |e| { e.stop_propagation(); navigator().push(to.clone()); }, "{text}" }
    }
}

pub fn release_columns() -> Vec<Column> {
    vec![
        status_column(),
        Column::html("name", &state::tr("release"), release_name_html).sort("name"),
        Column::html("platform", &state::tr("platform"), |r| html::cbadge("platform", &s(r, "platform"), None, None)).sort("platform"),
        Column::html("region", &state::tr("region"), |r| html::region_badges(&s(r, "region"))).sort("region"),
        Column::html("languages", &state::tr("languages"), release_lang_badges).sort("languages"),
        Column::text("year", &state::tr("year")).sort("year").num(),
        Column::text("serial", &state::tr("serial")).sort("serial"),
        Column::text("disc_count", &state::tr("discs")).sort("disc_count").num(),
        Column::html("source", &state::tr("sources"), |r| html::source_badges(&s(r, "source"))).sort("source"),
        Column::render("title_name", &state::tr("title"), |r| match api::uo(r, "title_id") {
            Some(id) => rsx! { CellLink { to: Route::TitleDetail { id }, text: s(r, "title_name") } },
            None => rsx! {},
        })
        .sort("title"),
        Column::html("local_size", &state::tr("size on disk"), |r| api::uo(r, "local_size").map(|n| html::fmt_size(Some(n))).unwrap_or_default()).sort("local_size").num(),
        Column::html("catalog_size", &state::tr("catalogue size (raw)"), |r| api::uo(r, "catalog_size").map(|n| format!("<span title=\"{}\">{}</span>", esc(&state::tr(RAW_NOTE)), html::fmt_size(Some(n)))).unwrap_or_default()).sort("catalog_size").num(),
    ]
    .into_iter()
    .chain(display_columns())
    .chain([Column::html("dump", &state::tr("dump"), release_chips)])
    .collect()
}

// --- actions ---

/// The user's keyword tag on titles, picked the way audio tags are (type to search the tags
/// that exist, or enter a new one). Returns whether it was added.
pub async fn add_keyword_tag(ids: Vec<u64>) -> bool {
    let q = if ids.len() > 1 { state::trf("Keyword tag for {n} titles:", &[("n", &ids.len().to_string())]) } else { state::tr("Keyword tag:") };
    let Some(name) = super::audio::pick_tag(q, "", Vec::new()).await.filter(|n| !n.trim().is_empty()) else { return false };
    match api::post("/api/v1/catalog/titles/keywords", json!({ "title_ids": ids, "name": name.trim() })).await {
        Ok(r) => {
            ui::toast(state::trf("tagged {n} title(s) \"{name}\"", &[("n", &u(&r, "titles").to_string()), ("name", name.trim())]), false);
            forget_facets();
            true
        }
        Err(e) => {
            ui::toast(e.message, true);
            false
        }
    }
}

/// Removes a keyword tag (the user's or an automatic one): the picker offers the tags these
/// titles carry.
pub async fn remove_keyword_tag(ids: Vec<u64>) -> bool {
    let q = if ids.len() > 1 { state::trf("Remove which keyword tag from {n} titles?", &[("n", &ids.len().to_string())]) } else { state::tr("Remove which keyword tag?") };
    let only = ids.iter().take(500).map(u64::to_string).collect::<Vec<_>>().join(",");
    let Some(name) = super::audio::pick_tag(q, "tags", vec![("title_ids", only)]).await.filter(|n| !n.trim().is_empty()) else { return false };
    match api::post("/api/v1/catalog/titles/keywords/remove", json!({ "title_ids": ids, "name": name.trim() })).await {
        Ok(r) => {
            let n = u(&r, "titles");
            ui::toast(if n > 0 { state::trf("removed \"{name}\" from {n} title(s)", &[("name", name.trim()), ("n", &n.to_string())]) } else { state::trf("none of them had \"{name}\"", &[("name", name.trim())]) }, n == 0);
            forget_facets();
            true
        }
        Err(e) => {
            ui::toast(e.message, true);
            false
        }
    }
}

pub async fn load_sets() -> Vec<Value> {
    api::get("/api/v1/sets").await.map(|v| api::items(&v)).unwrap_or_default()
}

async fn add_to_set(set_id: u64, body: Value) -> Result<Value, String> {
    api::post(&format!("/api/v1/sets/{set_id}/releases"), body).await.map_err(|e| e.message)
}

/// How many sets the "Add to set" menus list before folding: from `FOLD_SETS_AT` sets on, the
/// `RECENT_SETS` last added to are listed, then a fold holding every set.
const FOLD_SETS_AT: usize = 4;
const RECENT_SETS: usize = 3;

/// "Add to set: X" for each set, plus "New set…". `body`: {release_id} / {release_ids} /
/// {title_ids} (each stays a title: its best release when the set is read) / {igdb_ids}
/// (cross-platform games).
pub async fn set_menu_items(body: Value, after: Option<Rc<dyn Fn()>>) -> Vec<MenuItem> {
    set_menu_items_except(body, after, None).await
}

/// [`set_menu_items`] leaving out one set (the one a set page shows).
pub async fn set_menu_items_except(body: Value, after: Option<Rc<dyn Fn()>>, except: Option<u64>) -> Vec<MenuItem> {
    let titles = body.get("title_ids").is_some() || body.get("title_row_ids").is_some() || body.get("title_rows").is_some();
    let suffix = if body.get("family_ids").is_some() {
        format!(" {}", state::tr("(as the definitive version)"))
    } else if titles {
        format!(" {}", state::tr("(as a title)"))
    } else {
        String::new()
    };
    let sets: Vec<Value> = load_sets().await.into_iter().filter(|x| Some(u(x, "id")) != except).collect();
    let item = |x: &Value, label: String| {
        let (id, name) = (u(x, "id"), s(x, "name"));
        let (body, after) = (body.clone(), after.clone());
        MenuItem::new(label, move || {
            let (body, after, name) = (body.clone(), after.clone(), name.clone());
            spawn(async move {
                match add_to_set(id, body).await {
                    Ok(r) => {
                        ui::toast(state::trf("added {n} release(s) to {name}", &[("n", &u(&r, "added").to_string()), ("name", &name)]), false);
                        if let Some(a) = after {
                            a();
                        }
                    }
                    Err(e) => ui::toast(e, true),
                }
            });
        })
    };
    let full = |x: &Value| format!("{}{suffix}", state::trf("Add to set: {name}", &[("name", &s(x, "name"))]));
    let mut items: Vec<MenuItem> = if sets.len() < FOLD_SETS_AT {
        sets.iter().map(|x| item(x, full(x))).collect()
    } else {
        // `updated_at` is bumped whenever something is added to a set.
        let mut recent: Vec<&Value> = sets.iter().collect();
        recent.sort_by(|a, b| s(b, "updated_at").cmp(&s(a, "updated_at")));
        let mut items: Vec<MenuItem> = recent.into_iter().take(RECENT_SETS).map(|x| item(x, full(x))).collect();
        let all = sets.iter().map(|x| item(x, s(x, "name"))).collect();
        items.push(MenuItem::fold(format!("{}{suffix}", state::trf("Add to set: all sets ({n})", &[("n", &sets.len().to_string())])), all));
        items
    };
    items.push(MenuItem::new(state::tr("New set…"), move || {
        let (body, after) = (body.clone(), after.clone());
        spawn(async move {
            let Some(name) = ui::prompt(&state::tr("New set name:"), "").await.filter(|n| !n.trim().is_empty()) else { return };
            match api::post("/api/v1/sets", json!({ "name": name.trim() })).await {
                Ok(x) => match add_to_set(u(&x, "id"), body).await {
                    Ok(r) => {
                        ui::toast(state::trf("added {n} release(s) to {name}", &[("n", &u(&r, "added").to_string()), ("name", name.trim())]), false);
                        if let Some(a) = after {
                            a();
                        }
                    }
                    Err(e) => ui::toast(e, true),
                },
                Err(e) => ui::toast(e.message, true),
            }
        });
    }));
    items
}

/// The set picker dialog (selection bars).
pub fn pick_set(body: Value, done: Option<Rc<dyn Fn()>>) {
    ui::open_modal(move || rsx! { PickSet { body: body.clone(), done: DoneFn(done.clone()) } });
}

#[derive(Clone)]
pub struct DoneFn(pub Option<Rc<dyn Fn()>>);

impl PartialEq for DoneFn {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

#[component]
fn PickSet(body: Value, done: DoneFn) -> Element {
    let sets = use_resource(load_sets);
    let mut name = use_signal(String::new);
    let rows = body.get("title_row_ids").or_else(|| body.get("title_rows")).and_then(|t| t.as_array()).map(Vec::len);
    let what = if let Some(n) = rows {
        state::trf("{n} title row(s) — each stays that kind: its best release of that kind, one you have first, never another kind's", &[("n", &n.to_string())])
    } else if let Some(t) = body.get("title_ids").and_then(|t| t.as_array()) {
        state::trf("{n} title(s) — each stays a title: its best release, one you have first, whenever the set is opened or copied", &[("n", &t.len().to_string())])
    } else if let Some(g) = body.get("igdb_ids").and_then(|g| g.as_array()) {
        state::trf("{n} game(s), any platform — the best release you have, else the top-priority one", &[("n", &g.len().to_string())])
    } else if let Some(f) = body.get("family_ids").and_then(|f| f.as_array()) {
        state::trf("{n} game(s) as their definitive version — copy_definitive (Settings) decides which version's release", &[("n", &f.len().to_string())])
    } else if let Some(g) = body.get("ra_game_ids").and_then(|g| g.as_array()) {
        state::trf("{n} RetroAchievements game(s) — each adds the release you hold, else its top-priority release", &[("n", &g.len().to_string())])
    } else {
        state::trf("{n} release(s)", &[("n", &body.get("release_ids").and_then(|t| t.as_array()).map(Vec::len).unwrap_or(1).to_string())])
    };
    let add = {
        let body = body.clone();
        let done = done.0.clone();
        move |set_id: u64| {
            let (body, done) = (body.clone(), done.clone());
            spawn(async move {
                match add_to_set(set_id, body).await {
                    Ok(r) => {
                        ui::toast(state::trf("added {n} release(s) to {name}", &[("n", &u(&r, "added").to_string()), ("name", &s(r.get("set").unwrap_or(&Value::Null), "name"))]), false);
                        ui::close_modal();
                        if let Some(d) = done {
                            d();
                        }
                    }
                    Err(e) => ui::toast(e, true),
                }
            });
        }
    };
    let add2 = add.clone();
    rsx! {
        h3 { {state::tr("Add to set")} }
        p { class: "muted", "{what}" }
        div { style: "display:flex;flex-wrap:wrap;gap:6px",
            match &*sets.read() {
                None => rsx! { span { class: "muted", {state::tr("loading…")} } },
                Some(list) if list.is_empty() => rsx! { span { class: "muted", {state::tr("no sets yet")} } },
                Some(list) => rsx! {
                    for x in list.clone() {
                        {
                            let add = add.clone();
                            let id = u(&x, "id");
                            rsx! { button { class: "btn", onclick: move |_| add(id), "{s(&x, \"name\")} " span { class: "muted", "({u(&x, \"release_count\")})" } } }
                        }
                    }
                },
            }
        }
        form { class: "inline", style: "margin-top:10px",
            onsubmit: move |e| {
                e.prevent_default();
                let add = add2.clone();
                let n = name();
                spawn(async move {
                    match api::post("/api/v1/sets", json!({ "name": n.trim() })).await {
                        Ok(x) => add(u(&x, "id")),
                        Err(e) => ui::toast(e.message, true),
                    }
                });
            },
            input { r#type: "text", placeholder: state::tr("new set name"), required: true, value: "{name}", oninput: move |e| name.set(e.value()) }
            button { class: "btn primary", {state::tr("create & add")} }
        }
    }
}

/// "Open with <emulator>" for a rom ({rom_id}) or a game ({game_id}).
pub async fn open_with_items(target: Vec<(&'static str, String)>) -> Vec<MenuItem> {
    let o = match api::get(&format!("/api/v1/launch/options{}", api::qs(&target))).await {
        Ok(o) => o,
        Err(e) => {
            let m = e.message.clone();
            return vec![MenuItem::new(state::trf("Open with… — {error}", &[("error", &e.message)]), move || ui::toast(m.clone(), true))];
        }
    };
    let emulators = api::arr(&o, "emulators").to_vec();
    if emulators.is_empty() {
        return vec![MenuItem::new(state::trf("Open with… (no emulator for {platform} — set one up)", &[("platform", &api::so(&o, "platform").unwrap_or_else(|| state::tr("this platform")))]), || {
            navigator().push(Route::ConfigEmulators {});
        })];
    }
    emulators
        .into_iter()
        .map(|x| {
            let label = format!("{}{}{}", state::trf("Open with {name}", &[("name", &s(&x, "name"))]), if api::b(&x, "is_default") { state::tr(" (default)") } else { String::new() }, api::so(&x, "unusable").map(|w| format!(" — {w}")).unwrap_or_default());
            let target = target.clone();
            MenuItem::new(label, move || {
                if let Some(w) = api::so(&x, "unusable") {
                    ui::toast(format!("{} {w}", s(&x, "name")), true);
                    return;
                }
                let mut body = serde_json::Map::new();
                for (k, v) in &target {
                    body.insert(k.to_string(), json!(v.parse::<u64>().unwrap_or(0)));
                }
                body.insert("emulator_id".into(), json!(u(&x, "id")));
                spawn(async move {
                    match api::post("/api/v1/launch", Value::Object(body)).await {
                        Ok(r) => ui::toast(state::trf("started {name}", &[("name", &s(&r, "emulator"))]), false),
                        Err(e) => ui::toast(e.message, true),
                    }
                });
            })
        })
        .collect()
}

/// A release's menu: sets (for the whole selection when several are selected), remove from
/// the set it's listed in, open.
pub async fn release_context(release_id: u64, name: String, ids: Option<Vec<u64>>, in_set: Option<u64>, after: Option<Rc<dyn Fn()>>) -> Vec<MenuItem> {
    let many = ids.as_ref().is_some_and(|i| i.len() > 1);
    let mut items = vec![MenuItem::head(if many { state::trf("{n} selected releases", &[("n", &ids.as_ref().map(Vec::len).unwrap_or(0).to_string())]) } else if name.is_empty() { state::trf("release #{id}", &[("id", &release_id.to_string())]) } else { name })];
    let body = if many { json!({ "release_ids": ids }) } else { json!({ "release_id": release_id }) };
    items.extend(set_menu_items(body, after.clone()).await);
    if let Some(set) = in_set {
        let after = after.clone();
        items.push(MenuItem::new(state::tr("Remove from this set"), move || {
            let after = after.clone();
            spawn(async move {
                match api::delete(&format!("/api/v1/sets/{set}/releases/{release_id}")).await {
                    Ok(_) => {
                        ui::toast(state::tr("removed"), false);
                        if let Some(a) = after {
                            a();
                        }
                    }
                    Err(e) => ui::toast(e.message, true),
                }
            });
        }));
    }
    items.push(super::copy::context_item("releases", release_id, "", ids.clone()));
    items.push(super::art::context_item("releases", release_id, "", ids.clone()));
    items.push(super::art::export_context_item("releases", release_id, "", ids.clone()));
    items.push(MenuItem::new(state::tr("Open release"), move || {
        navigator().push(Route::Release { id: release_id });
    }));
    items
}

/// The entries a set page's rows stand for, as an add-to-set body: a title or a game entry
/// stays a title or a game in the other set, a release a release. Kinds with nothing are left
/// out (a `title_ids` key, even empty, labels the menu "(as a title)").
pub fn set_rows_body(rows: &[Value]) -> Value {
    let (mut releases, mut titles, mut games): (Vec<u64>, Vec<u64>, Vec<String>) = (Vec::new(), Vec::new(), Vec::new());
    let mut families: Vec<u64> = Vec::new();
    let mut title_rows: Vec<Value> = Vec::new();
    for r in rows {
        let via = s(r, "set_via");
        match (via.as_str(), api::uo(r, "set_title_id"), api::so(r, "set_igdb_id")) {
            // A title row stays that row in the other set.
            ("title", Some(t), _) if !s(r, "set_row_key").is_empty() => {
                let row = json!({ "title_id": t, "row_key": s(r, "set_row_key") });
                if !title_rows.contains(&row) {
                    title_rows.push(row);
                }
            }
            ("title", Some(t), _) if !titles.contains(&t) => titles.push(t),
            ("title", Some(_), _) => {}
            ("game", _, Some(g)) if !games.contains(&g) => games.push(g),
            ("game", _, Some(_)) => {}
            ("family", ..) if api::uo(r, "set_family_title_id").is_some() => {
                let f = u(r, "set_family_title_id");
                if !families.contains(&f) {
                    families.push(f);
                }
            }
            _ => {
                let id = u(r, "id");
                if !releases.contains(&id) {
                    releases.push(id);
                }
            }
        }
    }
    let mut body = serde_json::Map::new();
    if !releases.is_empty() {
        body.insert("release_ids".into(), json!(releases));
    }
    if !titles.is_empty() {
        body.insert("title_ids".into(), json!(titles));
    }
    if !title_rows.is_empty() {
        body.insert("title_rows".into(), json!(title_rows));
    }
    if !games.is_empty() {
        body.insert("igdb_ids".into(), json!(games));
    }
    if !families.is_empty() {
        body.insert("family_ids".into(), json!(families));
    }
    Value::Object(body)
}

/// A set page row's menu: add the selected rows' entries (`body`, from [`set_rows_body`]) to
/// another set -- not this one, `set_id` -- remove them from this one, copy, open.
pub async fn set_page_context(release_id: u64, name: String, ids: Option<Vec<u64>>, set_id: u64, body: Value, after: Option<Rc<dyn Fn()>>) -> Vec<MenuItem> {
    let many = ids.as_ref().is_some_and(|i| i.len() > 1);
    let mut items = vec![MenuItem::head(if many { state::trf("{n} selected releases", &[("n", &ids.as_ref().map(Vec::len).unwrap_or(0).to_string())]) } else if name.is_empty() { state::trf("release #{id}", &[("id", &release_id.to_string())]) } else { name })];
    items.extend(set_menu_items_except(body, after.clone(), Some(set_id)).await);
    let targets: Vec<u64> = if many { ids.clone().unwrap_or_default() } else { vec![release_id] };
    let label = if many { state::trf("Remove {n} from this set", &[("n", &targets.len().to_string())]) } else { state::tr("Remove from this set") };
    let after2 = after.clone();
    items.push(MenuItem::new(label, move || {
        let (after, targets) = (after2.clone(), targets.clone());
        spawn(async move {
            match api::post(&format!("/api/v1/sets/{set_id}/remove"), json!({ "release_ids": targets })).await {
                Ok(_) => {
                    ui::toast(state::tr("removed"), false);
                    if let Some(a) = after {
                        a();
                    }
                }
                Err(e) => ui::toast(e.message, true),
            }
        });
    }));
    items.push(super::copy::context_item("releases", release_id, "", ids.clone()));
    items.push(super::art::context_item("releases", release_id, "", ids.clone()));
    items.push(super::art::export_context_item("releases", release_id, "", ids.clone()));
    items.push(MenuItem::new(state::tr("Open release"), move || {
        navigator().push(Route::Release { id: release_id });
    }));
    items
}

/// A reload trigger for a grid: bump it to refetch.
pub fn bump(mut sig: Signal<u64>) {
    *sig.write() += 1;
}

/// A route's query string (`basegame=12&keywords=x`) as filters.
pub fn parse_q(q: &str) -> std::collections::BTreeMap<String, String> {
    q.trim_start_matches('?')
        .split('&')
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            (!v.is_empty()).then(|| (k.to_string(), decode(v)))
        })
        .collect()
}

/// Percent-decoding for a query value (`+` is a space).
pub fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                if let Ok(x) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    out.push(x);
                    i += 2;
                } else {
                    out.push(b'%');
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The facet filters' query pairs.
pub fn facet_params(q: &crate::components::Query) -> Vec<(&'static str, String)> {
    FACET_KEYS.iter().map(|k| (*k, q.f(k))).collect()
}

/// The release-kinds filter's value, or the setting's default.
pub fn kinds(q: &crate::components::Query) -> String {
    let k = q.f("kinds");
    if k.is_empty() {
        state::release_kinds_default()
    } else {
        k
    }
}

/// Follows a link click: ctrl / cmd / shift opens a new tab (web).
pub fn go(route: Route, e: &MouseEvent) {
    if e.modifiers().ctrl() || e.modifiers().meta() || e.modifiers().shift() {
        crate::components::open_new_tab(&route);
    } else {
        navigator().push(route);
    }
}

/// A "▶ open" button that pops the Open-with menu for a rom / game where it's clicked.
#[component]
pub fn OpenWithButton(label: String, head: String, target: Vec<(&'static str, String)>, #[props(default)] title: String) -> Element {
    rsx! {
        button { class: "btn small", title: "{title}",
            onclick: move |e| {
                e.stop_propagation();
                let p = e.client_coordinates();
                let (target, head) = (target.clone(), head.clone());
                spawn(async move {
                    let mut items = vec![MenuItem::head(head)];
                    items.extend(open_with_items(target).await);
                    ui::open_menu(p.x, p.y, items);
                });
            },
            "{label}"
        }
    }
}

/// A content's name in a member or content list, as markup: the name DATs give its bytes when
/// one does, with the name it is stored under (its path inside the archive, else the name it
/// was first seen as) after it when that differs. A CHD's inner image is "x.raw" or
/// "DRAGONQUEST5.bin" on disk and "... (Japan, Asia).iso" to Redump; both are worth seeing.
pub fn member_name(m: &Value) -> String {
    let stored = api::so(m, "member_path").unwrap_or_else(|| s(m, "file_name"));
    match api::so(m, "canonical_name").filter(|c| *c != stored) {
        Some(canonical) => format!("{} <span class=\"muted\">{}</span>", esc(&canonical), esc(&stored)),
        None => esc(&stored),
    }
}

pub fn file_name(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

#[cfg(test)]
mod tests {
    use super::{held_badge, held_class};
    use serde_json::json;

    fn row(local: u64, main: u64, peer: u64) -> serde_json::Value {
        json!({ "local_games": local, "remote_games": main, "peer_games": peer })
    }

    #[test]
    fn only_the_main_server_decides_whether_a_local_row_is_unsynced() {
        // Held here, main server hasn't got it -- the loud one.
        assert_eq!(held_class(&row(1, 0, 0)), "unsynced");
        // A peer holding a copy must NOT quiet it: a client share is not a backup.
        assert_eq!(held_class(&row(1, 0, 3)), "unsynced");
        // On the main server too -- nothing to do.
        assert_eq!(held_class(&row(1, 1, 0)), "local");
        assert_eq!(held_class(&row(1, 1, 2)), "local");
    }

    #[test]
    fn a_row_only_a_peer_has_is_loud_too() {
        // Nobody but a peer holds it, so the main server is still missing it -- just as
        // actionable as something held only here.
        assert_eq!(held_class(&row(0, 0, 1)), "unsynced");
        let b = held_badge(&row(0, 0, 1));
        assert!(b.contains("on a peer") && b.contains("not on server") && b.contains("warn"), "{b}");
    }

    #[test]
    fn rows_the_main_server_already_has_are_never_loud() {
        assert_eq!(held_class(&row(0, 1, 0)), "remoteonly");
        // On the main server AND a peer, but not here: still just "remote only".
        assert_eq!(held_class(&row(0, 1, 1)), "remoteonly");
        assert_eq!(held_class(&row(0, 0, 0)), "");
    }

    #[test]
    fn lists_without_mirror_counts_keep_the_old_two_states() {
        // games / roms / files are local-only by construction and carry no remote_games,
        // so claiming "not on the server" there would be a false alarm on every row.
        assert_eq!(held_class(&json!({ "local_games": 1 })), "local");
        assert_eq!(held_class(&json!({ "local_games": 0 })), "");
        assert!(held_badge(&json!({ "local_games": 1 })).contains("1 local"));
        assert!(!held_badge(&json!({ "local_games": 1 })).contains("not on server"));
    }

    #[test]
    fn the_badge_says_what_still_needs_copying_up() {
        let loud = held_badge(&row(1, 0, 0));
        assert!(loud.contains("not on server") && loud.contains("warn"), "{loud}");
        let both = held_badge(&row(1, 1, 0));
        assert!(both.contains("1 local") && !both.contains("not on server"), "{both}");
        assert!(held_badge(&row(0, 1, 0)).contains("on server"));
        assert!(held_badge(&row(0, 0, 0)).contains("not local"));
        // The peer count rides along as a quiet extra, never as the headline.
        assert!(held_badge(&row(1, 0, 2)).contains("+2 peer"));
    }
}

/// Selection actions moving releases between titles: all of them into one new title, or into
/// a title picked by name.
pub fn release_title_actions() -> Vec<crate::components::SelAction> {
    use crate::components::SelAction;
    vec![
        SelAction::new(&state::tr("split into a new title"), |ids: Vec<u64>| {
            async move {
                if !ui::confirm(&state::trf("Put the {n} selected release(s) together in a new title of their own? It will not be combined with their old titles again automatically.", &[("n", &ids.len().to_string())])).await {
                    return false;
                }
                match api::post("/api/v1/catalog/releases/split", json!({ "release_ids": ids })).await {
                    Ok(r) => {
                        navigator().push(crate::Route::TitleDetail { id: u(&r, "title_id") });
                        true
                    }
                    Err(e) => {
                        ui::toast(e.message, true);
                        false
                    }
                }
            }
        }),
        SelAction::new(&state::tr("combine into one release"), |ids: Vec<u64>| async move {
            if ids.len() < 2 {
                ui::toast(state::tr("select at least two releases"), true);
                return false;
            }
            if !ui::confirm(&state::trf("Combine {n} releases into one? For the discs of one game that are listed apart: one release stands for them all under its title, and copying it copies every disc.", &[("n", &ids.len().to_string())])).await {
                return false;
            }
            match api::post("/api/v1/catalog/releases/combine", json!({ "release_ids": ids })).await {
                Ok(r) => {
                    ui::toast(state::trf("combined into release #{id}", &[("id", &u(&r, "release_id").to_string())]), false);
                    true
                }
                Err(e) => {
                    ui::toast(e.message, true);
                    false
                }
            }
        }),
        SelAction::new(&state::tr("move to another title…"), |ids: Vec<u64>| async move {
            let heading = state::trf("Move {n} release(s) to", &[("n", &ids.len().to_string())]);
            ui::open_modal(move || {
                let ids = ids.clone();
                rsx! { super::patched::PickTitle { heading: heading.clone(), release_ids: ids.clone(), on_pick: move |title: u64| {
                    let ids = ids.clone();
                    spawn(async move {
                        match api::post("/api/v1/catalog/releases/move-title", json!({ "release_ids": ids, "title_id": title })).await {
                            Ok(_) => { navigator().push(crate::Route::TitleDetail { id: title }); }
                            Err(e) => ui::toast(e.message, true),
                        }
                    });
                } } }
            });
            false
        }),
    ]
}
