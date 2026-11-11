//! Collection › Audio (the library list, tagging, scanning) and the player that title,
//! release and RetroAchievements game pages carry.
//!
//! The player itself is not a Dioxus component: it is the script both UIs share
//! (`romexplorer_types::PLAYER_JS`), which owns one `<audio>` element, the bar and the
//! playlist panel. [`PagePlayer`] only fetches the page's playlist, hands it to that script,
//! and stops it when the page is left -- the music belongs to the page.

use std::rc::Rc;

use dioxus::prelude::*;
use serde_json::{json, Value};

use super::backup::JobWatch;
use super::common::bump;
use crate::api::{self, s, u};
use crate::components::{Column, Filter, Grid, GridSpec, SelAction, Spec};
use crate::html::{badge, esc};
use crate::state;
use crate::ui::{self, Html};

fn duration(ms: u64) -> String {
    if ms == 0 {
        return String::new();
    }
    let secs = (ms + 500) / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// The tag picker's dialog: type to search the tags that exist (those on audio files first,
/// then the catalogue's keywords; `scope` "audio" = only tags on audio files), click one to
/// take it, or press Enter to use what was typed. `on_pick(None)` is a cancel.
#[component]
fn TagPicker(question: String, scope: &'static str, extra: Vec<(&'static str, String)>, on_pick: EventHandler<Option<String>>) -> Element {
    let mut typed = use_signal(String::new);
    let found = use_resource(move || {
        let extra = extra.clone();
        async move {
            let q = typed();
            let mut p = vec![("q", q.trim().to_string()), ("scope", scope.to_string())];
            p.extend(extra);
            api::get(&format!("/api/v1/audio/tags{}", api::qs(&p))).await.ok().and_then(|v| v.as_array().cloned()).unwrap_or_default()
        }
    });
    let list = found.read().clone().unwrap_or_default();
    rsx! {
        h3 { "{question}" }
        form {
            onsubmit: move |e| {
                e.prevent_default();
                let name = typed().trim().to_string();
                on_pick.call((!name.is_empty()).then_some(name));
            },
            input { r#type: "text", autocomplete: "off", autofocus: true, style: "width:100%", placeholder: state::tr("type to search, or enter a new tag"), value: "{typed}", oninput: move |e| typed.set(e.value()) }
            div { style: "max-height:280px;overflow-y:auto;margin:6px 0",
                if list.is_empty() {
                    div { class: "muted", if typed().trim().is_empty() { {state::tr("no tags yet")} } else { {state::tr("no existing tag matches — Enter makes a new one")} } }
                }
                for tag in list {
                    {
                        let name = s(&tag, "name");
                        let counts = [(u(&tag, "files"), "{n} file(s)"), (u(&tag, "titles"), "{n} title(s)")].iter().filter(|(n, _)| *n > 0).map(|(n, text)| state::trf(text, &[("n", &n.to_string())])).collect::<Vec<_>>().join(" · ");
                        let pick = name.clone();
                        rsx! {
                            div { key: "{name}", style: "padding:4px 6px;cursor:pointer;border-bottom:1px solid var(--line)", onclick: move |_| on_pick.call(Some(pick.clone())),
                                Html { html: badge(&name, "ok") }
                                span { class: "muted", " {counts}" }
                            }
                        }
                    }
                }
            }
            button { class: "btn primary", r#type: "submit", {state::tr("use this tag")} }
            " "
            button { class: "btn", r#type: "button", onclick: move |_| on_pick.call(None), {state::tr("cancel")} }
        }
    }
}

/// Opens the picker and waits for its answer. `None`: cancelled, or closed by a click outside.
/// `extra`: more of the tag query (`title_ids` for `scope` "tags").
pub async fn pick_tag(question: String, scope: &'static str, extra: Vec<(&'static str, String)>) -> Option<String> {
    use std::cell::RefCell;
    // The dialog lives in the modal host, not in this future: its answer comes back through a
    // shared slot, looked at a few times a second. A dialog closed without answering (a click
    // outside) leaves the slot empty with no modal open, which is a cancel.
    let slot: Rc<RefCell<Option<Option<String>>>> = Rc::new(RefCell::new(None));
    let answer = slot.clone();
    ui::open_modal(move || {
        let answer = answer.clone();
        rsx! { TagPicker { question: question.clone(), scope, extra: extra.clone(), on_pick: move |name: Option<String>| { *answer.borrow_mut() = Some(name); ui::close_modal(); } } }
    });
    loop {
        if let Some(name) = slot.borrow_mut().take() {
            return name;
        }
        if !ui::modal_is_open() {
            return None;
        }
        futures_timer::Delay::new(std::time::Duration::from_millis(60)).await;
    }
}

/// Adds or removes one keyword tag on the selected files.
async fn tag_files(ids: Vec<u64>, path: &'static str, question: &'static str, scope: &'static str) -> bool {
    let Some(name) = pick_tag(state::trf(question, &[("n", &ids.len().to_string())]), scope, Vec::new()).await.filter(|n| !n.trim().is_empty()) else { return false };
    match api::post(path, json!({ "ids": ids, "name": name.trim() })).await {
        Ok(r) => {
            ui::toast(state::trf("{n} file(s) changed", &[("n", &u(&r, "changed").to_string())]), false);
            super::common::forget_facets();
            true
        }
        Err(e) => {
            ui::toast(e.message, true);
            false
        }
    }
}

fn audio_spec(facets: &Value) -> GridSpec {
    let list = |key: &str| -> Vec<String> { api::arr(facets, key).iter().map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).collect() };
    let select = |key: &'static str, label: &str, any: &str, values: Vec<String>, extra: Option<(String, String)>| {
        let any = state::tr(any);
        Filter::Select {
            key,
            label: state::tr(label),
            options: Rc::new(move || {
                let mut out = vec![(String::new(), any.clone())];
                out.extend(values.iter().map(|v| (v.clone(), v.clone())));
                out.extend(extra.clone());
                out
            }),
        }
    };
    let columns = vec![
        Column::html("title", &state::tr("title"), |r| format!("{}{}", esc(&s(r, "title")), if api::b(r, "playable") { String::new() } else { format!(" {}", badge(&state::tr("not playable here"), "warn")) })).sort("title"),
        Column::text("artist", &state::tr("artist")).sort("artist"),
        Column::text("album", &state::tr("album")).sort("album"),
        Column::html("disc_number", &state::tr("disc"), |r| api::uo(r, "disc_number").map(|n| n.to_string()).unwrap_or_default()).num(),
        Column::html("track_number", &state::tr("track"), |r| api::uo(r, "track_number").map(|n| n.to_string()).unwrap_or_default()).num(),
        Column::html("year", &state::tr("year"), |r| api::uo(r, "year").map(|n| n.to_string()).unwrap_or_default()).sort("year").num(),
        Column::html("duration_ms", &state::tr("length"), |r| duration(u(r, "duration_ms"))).sort("duration").num(),
        Column::html("format", &state::tr("format"), |r| badge(&s(r, "format"), if api::b(r, "playable") { "dim" } else { "warn" })).sort("format"),
        Column::html("tags", &state::tr("keyword tags"), |r| {
            // The auto-tagger's in grey with a gear, the user's own in green.
            let auto: Vec<String> = s(r, "auto_tags").split(',').filter(|x| !x.is_empty()).map(str::to_string).collect();
            s(r, "tags")
                .split(',')
                .filter(|x| !x.is_empty())
                .map(|x| if auto.iter().any(|a| a == x) { format!("<span title=\"{}\">{}</span>", esc(&state::tr("automatic tag")), badge(&format!("{x} ⚙"), "dim")) } else { badge(x, "ok") })
                .collect::<Vec<_>>()
                .join(" ")
        }),
        Column::html("file_name", &state::tr("file"), |r| format!("<span class=\"mono\" title=\"{}\">{}</span>", esc(&s(r, "path")), esc(&s(r, "file_name")))).sort("file_name"),
    ];
    let mut spec = GridSpec::new("audio", columns, |q| async move {
        let mut p = q.base();
        p.extend([("tag", q.f("tag")), ("tag_source", q.f("tag_source")), ("artist", q.f("artist")), ("album", q.f("album")), ("year", q.f("year"))]);
        api::get(&format!("/api/v1/audio{}", api::qs(&p))).await
    })
    .filters(vec![
        select("tag", "keyword tag", "any tag", list("tags"), Some(("-".to_string(), state::tr("(no tag)")))),
        Filter::Select {
            key: "tag_source",
            label: state::tr("tag source"),
            options: Rc::new(|| vec![(String::new(), state::tr("tags from anyone")), ("manual".into(), state::tr("your tags")), ("auto".into(), state::tr("automatic tags"))]),
        },
        select("artist", "artist", "any artist", list("artists"), None),
        select("album", "album", "any album", list("albums"), None),
        select("year", "year", "any year", list("years"), None),
    ])
    .select(
        |r| u(r, "id"),
        vec![
            SelAction::new(&state::tr("add keyword tag"), |ids| tag_files(ids, "/api/v1/audio/tags", "Keyword tag for {n} file(s):", "")),
            SelAction::new(&state::tr("remove keyword tag"), |ids| tag_files(ids, "/api/v1/audio/tags/remove", "Keyword tag to remove from {n} file(s):", "audio")),
        ],
    );
    spec.default_sort = "album".into();
    spec
}

#[component]
pub fn Audio() -> Element {
    let mut reload = use_signal(|| 0u64);
    let mut job = use_signal(|| None::<u64>);
    // Reloaded with the list: a scan or a new tag changes what the filters offer.
    let facets = use_resource(move || async move {
        let _ = reload();
        api::get("/api/v1/audio/facets").await.unwrap_or_else(|_| json!({}))
    });
    let scan = move |mode: &'static str| {
        spawn(async move {
            match api::post("/api/v1/jobs", json!({ "job_type": "audio_scan", "mode": mode })).await {
                Ok(r) => job.set(Some(u(&r, "id"))),
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    let fetch = move |_| {
        spawn(async move {
            match api::post("/api/v1/jobs", json!({ "job_type": "audio_sync" })).await {
                Ok(r) => job.set(Some(u(&r, "id"))),
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    let autotag = move |_| {
        spawn(async move {
            match api::post("/api/v1/jobs", json!({ "job_type": "audio_autotag" })).await {
                Ok(r) => job.set(Some(u(&r, "id"))),
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    let rom_index = move |mode: &'static str| {
        spawn(async move {
            match api::post("/api/v1/jobs", json!({ "job_type": "rom_audio_index", "mode": mode })).await {
                Ok(r) => job.set(Some(u(&r, "id"))),
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    let Some(facets) = facets.read().clone() else { return rsx! { div { class: "muted", {state::tr("loading…")} } } };
    let spec = Spec(Rc::new(audio_spec(&facets)));
    rsx! {
        div { class: "card",
            ui::Explain { p { {state::tr("The audio files under the folders set as audio under Config › Storage, with what their tags say. A scan walks every subfolder. Give files a keyword tag and they play on the pages of every title carrying the same tag.")} } }
            div { class: "toolbar",
                button { class: "btn primary", r#type: "button", onclick: move |_| scan("full"), {state::tr("Scan audio (full)")} }
                crate::ui::QueueButton {}
                button { class: "btn", r#type: "button", onclick: move |_| scan("new_only"), {state::tr("Quick scan (new only)")} }
                crate::ui::QueueButton {}
                button { class: "btn", r#type: "button", title: state::tr("tag albums with the title they are named after (translations included); your own tags are left alone, and the automatic ones can be filtered and removed under Edit › Keyword tags"), onclick: autotag, {state::tr("auto-tag by title")} }
                crate::ui::QueueButton {}
                button { class: "btn", r#type: "button", title: state::tr("Client mode: fetches a copy of every tagged track the main server has, in the format the settings name. Needs bgm ticked under media_client_keep."), onclick: fetch, {state::tr("Fetch from the main server")} }
                crate::ui::QueueButton {}
                button { class: "btn", r#type: "button", title: state::tr("find the music and sounds inside disc images and dumped folders, and the music files in original audio formats kept in a game's folder, so title pages can play them"), onclick: move |_| rom_index("full"), {state::tr("Index ROM audio")} }
                crate::ui::QueueButton {}
                button { class: "btn", r#type: "button", title: state::tr("find the music and sounds inside disc images and dumped folders, and the music files in original audio formats kept in a game's folder, so title pages can play them"), onclick: move |_| rom_index("new_only"), {state::tr("Index ROM audio (new only)")} }
                crate::ui::QueueButton {}
            }
            if let Some(id) = job() {
                JobWatch { key: "{id}", job_id: id, on_done: move |_| bump(reload) }
            }
        }
        // Keyed on the reload count: the filter lists are baked into the spec.
        for k in [reload()] { div { key: "{k}", Grid { spec: spec.clone(), reload } } }
    }
}

/// The player for a title / release / RA game page: `query` is `title_id=…`, `release_id=…`
/// or `ra_game_id=…`. Draws nothing unless the page has music (tagged files, and the music in
/// its ROMs); starts it (shuffled) when it does. When the page is left it stops, or with
/// `audio_keep_playing` moves to a strip at the bottom until another title page opens.
#[component]
pub fn PagePlayer(query: String) -> Element {
    let host = use_hook(|| format!("rx-player-host-{}", query.replace('=', "-")));
    let mut playlist = use_resource({
        let query = query.clone();
        move || {
            let query = query.clone();
            async move {
                let mut list = api::get(&format!("/api/v1/audio/playlist?{query}")).await.unwrap_or_else(|_| json!({}));
                // Each answer told apart from the last: while the page's ROMs are indexed the
                // playlist can come back the same (the same progress line), and the effect
                // below, which asks again, runs only on a change.
                static FETCHES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                if let Some(o) = list.as_object_mut() {
                    o.insert("_fetch".into(), json!(FETCHES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
                }
                list
            }
        }
    });
    let resource = playlist;
    // `None` until it has loaded.
    let loaded = playlist.read().clone();
    let start_host = host.clone();
    use_effect(use_reactive!(|loaded| {
        let Some(playlist) = loaded else { return };
        let list = api::arr(&playlist, "tracks").to_vec();
        // The page's ROMs are being indexed (they went ahead of the rest when the page
        // opened): their progress shows below instead of another game's music, and the
        // playlist is asked for again until they are done.
        if list.is_empty() && playlist.get("indexing").is_some_and(Value::is_object) {
            let _ = document::eval("if (window.rxPlayer) window.rxPlayer.stop();");
            let mut again = resource;
            spawn(async move {
                ui::sleep(2000).await;
                again.restart();
            });
            return;
        }
        if list.is_empty() && api::arr(&playlist, "hidden").is_empty() {
            // A title page with nothing to play ends music kept from an earlier one.
            let _ = document::eval("if (window.rxPlayer) window.rxPlayer.stop();");
            return;
        }
        let keep = api::b(&playlist, "keep_playing");
        // The page's own URL for each track, then the shared script takes over.
        let playable_list: Vec<Value> = list.iter().cloned().map(playable).collect();
        let hidden: Vec<Value> = api::arr(&playlist, "hidden").iter().map(|t| playable(t.clone())).collect();
        let hide_title = playlist.get("hide_title").and_then(Value::as_u64);
        let labels = json!({
            "hidden tracks": state::tr("hidden tracks"), "click to listen": state::tr("click to listen"), "restore": state::tr("restore"), "undo": state::tr("undo"),
            "don't play this track on this page again": state::tr("don't play this track on this page again"),
            "previous": state::tr("previous"), "next": state::tr("next"), "stop": state::tr("stop"), "shuffle": state::tr("shuffle"),
            "mute": state::tr("mute"), "playlist": state::tr("playlist"), "position": state::tr("position"), "play / pause": state::tr("play / pause"),
            "volume": state::tr("volume"), "normalise": state::tr("normalise"), "normalise: play every track at about the same loudness (loud tracks are turned down; quiet ROM tracks are turned up)": state::tr("normalise: play every track at about the same loudness (loud tracks are turned down; quiet ROM tracks are turned up)"),
            "crossfade": state::tr("crossfade"), "crossfade: the next track fades in while this one fades out, overlapping by this many ms (0 = off)": state::tr("crossfade: the next track fades in while this one fades out, overlapping by this many ms (0 = off)"),
        });
        // "Don't play this track" and "restore" come back here as messages, and are recorded
        // for this page's title.
        let on_hide = if hide_title.is_some() { "(t, hidden) => dioxus.send({ id: t.id, rom: !!t.rom, hidden })" } else { "null" };
        let js = format!(
            "{}\nwindow.rxPlayer.start(document.getElementById({}), {}, {}, {{ keep: {keep}, hidden: {}, onHide: {on_hide} }});",
            romexplorer_types::PLAYER_JS,
            serde_json::to_string(&start_host).unwrap_or_default(),
            serde_json::to_string(&playable_list).unwrap_or_default(),
            labels,
            serde_json::to_string(&hidden).unwrap_or_default(),
        );
        let mut eval = document::eval(&js);
        if let Some(title) = hide_title {
            spawn(async move {
                while let Ok(msg) = eval.recv::<Value>().await {
                    let hidden = api::b(&msg, "hidden");
                    let body = json!({ "title_id": title, "id": u(&msg, "id"), "rom": api::b(&msg, "rom"), "hidden": hidden });
                    match api::put("/api/v1/audio/hidden", body).await {
                        Ok(_) => ui::toast(state::tr(if hidden { "hidden from this page" } else { "restored" }), false),
                        Err(e) => ui::toast(e.to_string(), true),
                    }
                }
            });
        }
    }));
    use_drop(|| {
        let _ = document::eval("if (window.rxPlayer) window.rxPlayer.leave();");
    });
    // A title page whose ROMs have video sound (a title movie): this title's own on / off,
    // over the audio_rom_videos setting.
    let videos = playlist.read().as_ref().and_then(|p| p.get("title_videos").cloned()).filter(|tv| api::b(tv, "has_video") || s(tv, "choice") != "default");
    let switch = videos.map(|tv| {
        let title = u(&tv, "title_id");
        let choice = s(&tv, "choice");
        rsx! {
            div { class: "toolbar rx-videos",
                label { {state::tr("Video sound for this title")} " "
                    select {
                        onchange: move |e| {
                            let value = e.value();
                            spawn(async move {
                                match api::put(&format!("/api/v1/audio/rom/title/{title}"), json!({ "videos": value })).await {
                                    Ok(_) => {
                                        ui::toast(state::tr("saved"), false);
                                        playlist.restart();
                                    }
                                    Err(e) => ui::toast(e.to_string(), true),
                                }
                            });
                        },
                        for (v, l) in [("default", state::tr("as in Settings")), ("on", state::tr("on")), ("off", state::tr("off"))] {
                            option { value: "{v}", selected: choice == v, "{l}" }
                        }
                    }
                }
            }
        }
    });
    let indexing = playlist.read().as_ref().filter(|p| api::arr(p, "tracks").is_empty()).and_then(|p| p.get("indexing").filter(|i| i.is_object()).cloned()).map(|ix| {
        let progress = match ix.get("message").and_then(Value::as_str) {
            Some(m) if !m.is_empty() => m.to_string(),
            _ if s(&ix, "status") == "pending" => state::tr("waiting to start"),
            _ => "…".to_string(),
        };
        let text = state::trf("Indexing this game's music: {progress}", &[("progress", progress.as_str())]);
        rsx! { div { class: "toolbar rx-indexing muted", "{text}" } }
    });
    // An empty element the script draws the bar into. It holds nothing Dioxus renders, so a
    // re-render leaves what the script put there alone.
    rsx! { div { id: "{host}" } Html { html: String::new() } {indexing} {switch} }
}

/// A playlist track with the URL this app reads it from.
fn playable(mut t: Value) -> Value {
    // A track on the main server is streamed from it; one held here is read here.
    // A ROM track is decoded out of the ROM on its first play.
    let remote = if api::b(&t, "remote") { "remote/" } else { "" };
    let rom = api::b(&t, "rom");
    let route = format!("/api/v1/audio/{remote}{}{}/file", if rom { "rom/" } else { "" }, u(&t, "id"));
    t["url"] = json!(api::media_url(&route));
    if rom {
        let album = [s(&t, "album"), s(&t, "rom_kind")].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · ");
        t["album"] = json!(album);
    }
    t
}
