//! Display data on the release and title pages: how a release is meant to be shown (aspect,
//! widescreen, resolutions, refresh), each value marked Default (the system's) or Confirmed
//! (recorded by the user or a source), and the dialog that sets them for one or many releases.

use std::rc::Rc;

use dioxus::prelude::*;
use serde_json::{json, Value};

use crate::api::{self, s};
use crate::components::SelAction;
use crate::html::badge;
use crate::state;
use crate::ui::{self, Html};

/// Presets (`common::display::ASPECTS` / `REFRESH`); the server accepts any W:H or Hz.
const ASPECTS: [&str; 15] = ["4:3", "8:7", "16:9", "3:4", "3:2", "10:9", "5:3", "16:10", "16:15", "10:7", "14:9", "20:19", "12:7", "80:51", "1:1"];
const WIDESCREEN: [&str; 4] = ["none", "option", "native", "patch"];
const REFRESH: [&str; 7] = ["60", "50", "50,60", "56", "55", "75", "60,120"];

fn field_label(field: &str) -> String {
    state::tr(match field {
        "aspect" => "aspect",
        "widescreen" => "widescreen",
        "modes" => "resolutions",
        "refresh" => "refresh rate",
        other => other,
    })
}

fn output_label(output: &str) -> String {
    state::tr(match output {
        "tv" => "TV",
        "screen" => "screen",
        "top" => "top screen",
        "bottom" => "bottom screen",
        "gamepad" => "GamePad",
        other => other,
    })
}

/// `screen:960x544p,720x480p` -> `[("screen", ["960x544p"]), ("tv", ["720x480p"])]`: a
/// resolution without an output is on the TV.
fn resolution_groups<'a>(list: impl IntoIterator<Item = &'a str>) -> Vec<(String, Vec<String>)> {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for p in list {
        let (o, r) = p.split_once(':').unwrap_or(("tv", p));
        match groups.iter_mut().find(|g| g.0 == o) {
            Some(g) => g.1.push(r.to_string()),
            None => groups.push((o.to_string(), vec![r.to_string()])),
        }
    }
    groups
}

fn resolutions_label<'a>(list: impl IntoIterator<Item = &'a str>) -> String {
    let groups = resolution_groups(list);
    if groups.len() == 1 && groups[0].0 == "tv" {
        return groups[0].1.join(" · ");
    }
    groups.iter().map(|(o, rs)| format!("{} {}", output_label(o), rs.join(" · "))).collect::<Vec<_>>().join(" — ")
}

pub fn value_label(field: &str, value: Option<&str>) -> String {
    let Some(v) = value else { return "—".into() };
    match field {
        "widescreen" => match v {
            "none" => state::tr("no"),
            "option" => state::tr("16:9 option"),
            "native" => state::tr("16:9 native"),
            "patch" => state::tr("widescreen patch"),
            other => other.to_string(),
        },
        "refresh" => format!("{} Hz", v.split(',').collect::<Vec<_>>().join("/")),
        "modes" => resolutions_label(v.split(',')),
        _ => v.to_string(),
    }
}

fn source_label(source: &str) -> String {
    match source {
        "user" => state::tr("set by you"),
        "sfo" => state::tr("from the game's PARAM.SFO"),
        "mame" => state::tr("from MAME's machine list"),
        "pcsx2" => state::tr("PCSX2's widescreen patches"),
        "consolemods" => state::tr("from a ConsoleMods list"),
        "ipbin" => state::tr("from the disc's IP.BIN"),
        other => other.to_string(),
    }
}

/// A value's `system` list (the resolutions its system can draw).
fn system_of(v: &Value) -> Vec<String> {
    v["system"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

async fn set_display(release_ids: &[u64], field: &str, value: Option<String>) -> Result<(), String> {
    api::post("/api/v1/catalog/releases/display", json!({ "release_ids": release_ids, "field": field, "value": value })).await.map(|_| ()).map_err(|e| e.message)
}

/// The Display card: each value with its badge, confirm (keep the default as yours), reset
/// (take your value back) and change… (the dialog). Under the resolutions, what else the
/// system can draw. `note`: on a title page, that its values are its top release's and a
/// change applies to all its releases.
///
/// `media`: the page's media-strip query (`release_id=…` / `title_id=…`), for the screenshot
/// comparison of systems whose aspect is a per-game question (NES / SNES 4:3 or 8:7).
#[component]
pub fn DisplayCard(values: Vec<Value>, release_ids: Vec<u64>, note: String, media: String, on_change: EventHandler<()>) -> Element {
    if values.is_empty() {
        return rsx! {};
    }
    let aspect_value = values.iter().find(|v| s(v, "field") == "aspect").cloned();
    let compare_ids = release_ids.clone();
    let ids = Rc::new(release_ids.clone());
    let dialog_values = values.clone();
    let dialog_ids = release_ids.clone();
    rsx! {
        div { class: "card",
            h2 {
                {state::tr("Display")}
                if !note.is_empty() { " " span { class: "muted", style: "font-weight:normal;font-size:12px", "{note}" } }
                " "
                button { class: "btn small",
                    onclick: move |_| {
                        let (ids, values) = (dialog_ids.clone(), dialog_values.clone());
                        ui::open_modal(move || rsx! { DisplayDialog { release_ids: ids.clone(), values: values.clone(), on_done: on_change } })
                    },
                    {state::tr("change…")}
                }
            }
            div { class: "kv",
                for v in values {
                    {
                        let field = s(&v, "field");
                        let value = api::so(&v, "value");
                        let confirmed = s(&v, "state") == "confirmed";
                        let source = api::so(&v, "source").unwrap_or_default();
                        let hz = api::so(&v, "hz").map(|h| format!(" ({h} Hz)")).unwrap_or_default();
                        let shown: Vec<String> = value.as_deref().unwrap_or("").split(',').map(str::to_string).collect();
                        let more: Vec<String> = system_of(&v).into_iter().filter(|r| !shown.contains(r)).collect();
                        let more_label = if more.is_empty() { String::new() } else { format!("{} {}", state::tr("system can also:"), resolutions_label(more.iter().map(String::as_str))) };
                        let state_badge = if confirmed { format!("<span class=\"badge ok\" title=\"{}\">{}</span>", crate::html::esc(&source_label(&source)), state::tr("Confirmed")) } else { badge(&state::tr("Default"), "dim") };
                        let (ids_c, ids_r) = (ids.clone(), ids.clone());
                        let (fc, fr, vc) = (field.clone(), field.clone(), value.clone());
                        rsx! {
                            div { "{field_label(&field)}" }
                            div {
                                "{value_label(&field, value.as_deref())}"
                                if !hz.is_empty() { span { class: "muted", "{hz}" } }
                                " "
                                Html { html: state_badge }
                                if !confirmed && value.is_some() {
                                    " "
                                    button { class: "btn small",
                                        onclick: move |_| {
                                            let (ids, f, v) = (ids_c.clone(), fc.clone(), vc.clone());
                                            spawn(async move { match set_display(&ids, &f, v).await { Ok(_) => on_change.call(()), Err(e) => ui::toast(e, true) } });
                                        },
                                        {state::tr("confirm")}
                                    }
                                }
                                if source == "user" {
                                    " "
                                    button { class: "btn small", title: state::tr("take your value back"),
                                        onclick: move |_| {
                                            let (ids, f) = (ids_r.clone(), fr.clone());
                                            spawn(async move { match set_display(&ids, &f, None).await { Ok(_) => on_change.call(()), Err(e) => ui::toast(e, true) } });
                                        },
                                        {state::tr("reset")}
                                    }
                                }
                                if !more_label.is_empty() { div { class: "muted", style: "font-size:12px", "{more_label}" } }
                            }
                        }
                    }
                }
            }
            if let Some(a) = aspect_value.filter(|a| api::so(a, "compare").is_some()) {
                AspectCompare { value: api::so(&a, "value").unwrap_or_default(), kind: api::so(&a, "compare").unwrap_or_default(), media: media.clone(), release_ids: compare_ids.clone(), on_change }
            }
        }
    }
}

/// Aspects a square-pixel screenshot snaps to (within 2%), else its own reduced W:H.
const ASPECT_SNAP: [&str; 6] = ["8:7", "16:15", "10:7", "3:2", "4:3", "5:4"];

fn square_aspect(w: u32, h: u32) -> String {
    if w == 0 || h == 0 {
        return "4:3".into();
    }
    let r = w as f64 / h as f64;
    let near = ASPECT_SNAP
        .iter()
        .map(|a| {
            let (x, y) = a.split_once(':').unwrap_or(("4", "3"));
            (*a, ((x.parse::<f64>().unwrap_or(4.0) / y.parse::<f64>().unwrap_or(3.0)) - r).abs() / r)
        })
        .min_by(|a, b| a.1.total_cmp(&b.1));
    if let Some((a, _)) = near.filter(|n| n.1 < 0.02) {
        return a.to_string();
    }
    fn gcd(a: u32, b: u32) -> u32 {
        if b == 0 { a } else { gcd(b, a % b) }
    }
    let g = gcd(w, h);
    format!("{}:{}", w / g, h / g)
}

/// The page's screenshot at 4:3 beside 8:7 (`kind` `8:7`) or its own square-pixel shape
/// (`square`), the same width, a button under each to choose; ◀ ▶ through the screenshots.
#[component]
pub fn AspectCompare(value: String, kind: String, media: String, release_ids: Vec<u64>, on_change: EventHandler<()>) -> Element {
    let mut at = use_signal(|| 0usize);
    let shots = use_resource(use_reactive!(|media| async move {
        let all = api::get(&format!("/api/v1/media/strip?{media}")).await.ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
        let rank = |m: &Value| match s(m, "media_type").as_str() {
            "screenshot" => 0,
            "title_screen" => 1,
            "box" => 2,
            "cover" => 3,
            _ => 9,
        };
        // A box's back or spine shows nothing of the picture.
        let mut list: Vec<Value> = all.into_iter().filter(|m| s(m, "kind") == "image" && rank(m) < 9 && !(s(m, "media_type") == "box" && !matches!(s(m, "sub_category").as_str(), "" | "front"))).collect();
        list.sort_by_key(|m| rank(m));
        list
    }));
    let list = shots.read().clone().unwrap_or_default();
    let n = list.len();
    let current = list.get(at().min(n.saturating_sub(1))).map(|m| api::media_url(&s(m, "file")));
    // Only a box or cover to show: a sample, not something to judge the pixel shape by.
    let no_screenshot = n > 0 && !list.iter().any(|m| matches!(s(m, "media_type").as_str(), "screenshot" | "title_screen"));
    // The screenshot's own size, for its square-pixel shape.
    let size = use_resource(use_reactive!(|current| async move {
        let src = current?;
        let js = format!("const i = new Image(); i.src = {}; await i.decode(); return [i.naturalWidth, i.naturalHeight];", serde_json::to_string(&src).unwrap_or_default());
        let v = document::eval(&js).await.ok()?;
        Some((v[0].as_u64()? as u32, v[1].as_u64()? as u32))
    }));
    let natural = size.read().clone().flatten();
    let other = if kind == "square" { natural.map(|(w, h)| square_aspect(w, h)) } else { Some(kind.clone()) };
    let mut note = natural.filter(|_| kind == "square").map(|(w, h)| format!(" {w}×{h}")).unwrap_or_default();
    if kind == "square" && other.as_deref() == Some("4:3") {
        note.push_str(&format!(" · {}", state::tr("this image is already 4:3")));
    }
    let mut choices: Vec<(String, String)> = vec![("4:3".into(), state::tr("4:3 (TV)"))];
    if let Some(o) = other.filter(|o| o != "4:3") {
        let label = state::trf("{aspect} (square pixels)", &[("aspect", &o)]);
        choices.push((o, label));
    }
    let heading = if no_screenshot {
        state::tr("The box, as a sample only:")
    } else if current.is_some() {
        state::tr("Which looks right? The same picture at each aspect:")
    } else {
        state::tr("No screenshot to compare; choose the aspect:")
    };
    rsx! {
        if no_screenshot {
            div { class: "card", style: "margin-top:8px;font-weight:bold;border:2px solid var(--bad);color:var(--bad)", {state::tr("No screenshot of this game yet. A box or cover is drawn to its own shape, not the console's pixels, so it cannot show which aspect is right: choose from a screenshot (download one, or add your own).")} }
        }
        div { class: "muted", style: "font-size:12px;margin-top:8px", "{heading}{note}" }
        div { class: "aspect-compare",
            for (aspect, label) in choices {
                {
                    let on = aspect == value;
                    let (x, y) = aspect.split_once(':').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or(("4".into(), "3".into()));
                    let ids = release_ids.clone();
                    let pick = aspect.clone();
                    rsx! {
                        figure { class: if on { "on" } else { "" },
                            if let Some(src) = current.clone() {
                                div { class: "ac-box", style: "aspect-ratio:{x}/{y}", img { src: "{src}", alt: "" } }
                            }
                            figcaption {
                                button { class: if on { "btn small primary" } else { "btn small" },
                                    onclick: move |_| {
                                        let (ids, v) = (ids.clone(), pick.clone());
                                        spawn(async move { match set_display(&ids, "aspect", Some(v)).await { Ok(_) => on_change.call(()), Err(e) => ui::toast(e, true) } });
                                    },
                                    "{label}"
                                }
                            }
                        }
                    }
                }
            }
            if n > 1 {
                div {
                    button { class: "btn small", onclick: move |_| at.set((at() + n - 1) % n), "◀" }
                    span { class: "muted", " {at() % n + 1} / {n} " }
                    button { class: "btn small", onclick: move |_| at.set((at() + 1) % n), "▶" }
                }
            }
        }
    }
}

/// Sets the display fields of one or many releases: each unchanged, a value, or reset. The
/// resolutions offered are the system's (on a single release's page) and any typed in.
#[component]
pub fn DisplayDialog(release_ids: Vec<u64>, values: Vec<Value>, on_done: EventHandler<()>) -> Element {
    let current = |f: &str| values.iter().find(|v| s(v, "field") == f).and_then(|v| api::so(v, "value"));
    let aspect = use_signal(String::new);
    let widescreen = use_signal(String::new);
    let refresh = use_signal(String::new);
    let mut modes_mode = use_signal(String::new);
    let start_modes: Vec<String> = current("modes").map(|m| m.split(',').map(str::to_string).collect()).unwrap_or_default();
    let mut offered: Vec<String> = values.iter().find(|v| s(v, "field") == "modes").map(system_of).unwrap_or_default();
    for m in &start_modes {
        if !offered.contains(m) {
            offered.push(m.clone());
        }
    }
    let groups = resolution_groups(offered.iter().map(String::as_str));
    let single_tv = groups.len() == 1 && groups[0].0 == "tv";
    let mut modes = use_signal(move || start_modes);
    let mut more = use_signal(String::new);
    let heading = if release_ids.len() > 1 { state::trf("Display of {n} releases", &[("n", &release_ids.len().to_string())]) } else { state::tr("Display") };
    let ids = release_ids.clone();
    let save = move |_| {
        let ids = ids.clone();
        let mut writes: Vec<(&'static str, Option<String>)> = Vec::new();
        for (f, v) in [("aspect", aspect()), ("widescreen", widescreen()), ("refresh", refresh())] {
            match v.as_str() {
                "" => {}
                "-" => writes.push((f, None)),
                other => writes.push((f, Some(other.to_string()))),
            }
        }
        match modes_mode().as_str() {
            "set" => {
                let mut ms = modes();
                ms.extend(more().split([',', ' ']).map(str::trim).filter(|x| !x.is_empty()).map(str::to_string));
                if ms.is_empty() {
                    ui::toast(state::tr("pick at least one resolution"), true);
                    return;
                }
                writes.push(("modes", Some(ms.join(","))));
            }
            "-" => writes.push(("modes", None)),
            _ => {}
        }
        spawn(async move {
            for (f, v) in writes {
                if let Err(e) = set_display(&ids, f, v).await {
                    ui::toast(e, true);
                    return;
                }
            }
            ui::close_modal();
            ui::toast(state::tr("saved"), false);
            on_done.call(());
        });
    };
    let choice = |field: &'static str, options: &'static [&'static str], mut sig: Signal<String>| {
        rsx! {
            select { onchange: move |e| sig.set(e.value()),
                option { value: "", {state::tr("— unchanged —")} }
                for o in options { option { value: "{o}", "{value_label(field, Some(o))}" } }
                option { value: "-", {state::tr("reset (your value taken back)")} }
            }
        }
    };
    rsx! {
        h2 { "{heading}" }
        div { class: "kv",
            div { {state::tr("aspect")} }
            div { {choice("aspect", &ASPECTS, aspect)} " " span { class: "muted", "{value_label(\"aspect\", current(\"aspect\").as_deref())}" } }
            div { {state::tr("widescreen")} }
            div { {choice("widescreen", &WIDESCREEN, widescreen)} " " span { class: "muted", "{value_label(\"widescreen\", current(\"widescreen\").as_deref())}" } }
            div { {state::tr("resolutions")} }
            div {
                select { value: "{modes_mode}", onchange: move |e| modes_mode.set(e.value()),
                    option { value: "", {state::tr("— unchanged —")} }
                    option { value: "set", {state::tr("these:")} }
                    option { value: "-", {state::tr("reset (your value taken back)")} }
                }
                for (output, list) in groups {
                    div {
                        if !single_tv { span { class: "muted", "{output_label(&output)}: " } }
                        for r in list {
                            {
                                let key = if output == "tv" { r.clone() } else { format!("{output}:{r}") };
                                let k2 = key.clone();
                                rsx! {
                                    label { style: "margin-right:6px;white-space:nowrap",
                                        input { r#type: "checkbox", checked: modes().iter().any(|x| *x == key),
                                            onchange: move |e| {
                                                let on = e.checked();
                                                let k = k2.clone();
                                                modes.with_mut(|v| { v.retain(|x| *x != k); if on { v.push(k.clone()); } });
                                                modes_mode.set("set".into());
                                            }
                                        }
                                        " {r}"
                                    }
                                }
                            }
                        }
                    }
                }
                div {
                    input { size: "34", value: "{more}", placeholder: state::tr("more, e.g. 512x448i, screen:960x544p"),
                        oninput: move |e| { more.set(e.value()); modes_mode.set("set".into()); }
                    }
                }
            }
            div { {state::tr("refresh rate")} }
            div { {choice("refresh", &REFRESH, refresh)} " " span { class: "muted", "{value_label(\"refresh\", current(\"refresh\").as_deref())}" } }
        }
        div { class: "fsfoot",
            span { class: "grow" }
            button { class: "btn", onclick: move |_| ui::close_modal(), {state::tr("cancel")} }
            button { class: "btn primary", onclick: save, {state::tr("save")} }
        }
    }
}

/// The selection-bar action: "set display…" for the selected releases.
pub fn display_action() -> SelAction {
    SelAction::new(&state::tr("set display…"), |ids: Vec<u64>| async move {
        ui::open_modal(move || rsx! { DisplayDialog { release_ids: ids.clone(), values: Vec::new(), on_done: move |_| {} } });
        false
    })
}
