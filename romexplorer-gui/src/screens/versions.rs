//! A title's versions (the console's `versionsCard` / `versionGroupBadge`): what it is a minor
//! version of, what it covers, its version set and which version is shown, and the user's
//! corrections.

use dioxus::prelude::*;
use serde_json::{json, Value};

use crate::api::{self, s, u};
use crate::html::{badge, esc};
use crate::state;
use crate::ui::{self, Html};
use crate::Route;

fn source_label(source: &str) -> String {
    match source {
        "igdb" => state::tr("IGDB edition"),
        "name" => state::tr("by name"),
        "data" => state::tr("built-in list"),
        "user" => state::tr("set by you"),
        other => other.to_string(),
    }
}

/// A title row's version set: how many versions are not listed (the badge; the 🎲 is the
/// row's own button, `RerollButton`).
pub fn group_badge(r: &Value) -> String {
    match r.get("version_group").filter(|g| u(g, "hidden") > 0) {
        Some(g) => format!(r#" <span class="badge dim" title="{}">{}</span>"#, esc(&state::tr("one version of this set is listed; the others are on the title page")), esc(&state::trf("+{n} versions", &[("n", &u(g, "hidden").to_string())]))),
        None => String::new(),
    }
}

/// The set key of a title row, when its set has versions not listed.
pub fn group_key(r: &Value) -> Option<String> {
    r.get("version_group").filter(|g| u(g, "hidden") > 0).map(|g| s(g, "key"))
}

/// The 🎲: another version of the set is shown.
pub async fn reroll(key: &str) -> bool {
    match api::post(&format!("/api/v1/catalog/version-groups/{}/reroll", api::encode(key)), json!({})).await {
        Ok(_) => true,
        Err(e) => {
            ui::toast(e.message, true);
            false
        }
    }
}

#[component]
fn OtherLink(o: Value) -> Element {
    let id = u(&o, "title_id");
    rsx! {
        a { class: "link", onclick: move |_| { navigator().push(Route::TitleDetail { id }); }, "{s(&o, \"name\")}" }
        if let Some(p) = api::so(&o, "platforms") { " " span { class: "muted", "{p}" } }
    }
}

/// The title page's Versions card. `info` is `GET /catalog/titles/{id}/versions`;
/// `on_change` reads it again.
#[component]
pub fn VersionsCard(id: u64, info: Value, on_change: EventHandler<()>) -> Element {
    let post = move |body: Value| {
        spawn(async move {
            match api::post(&format!("/api/v1/catalog/titles/{id}/versions"), body).await {
                Ok(_) => on_change.call(()),
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    let set = info.get("set").filter(|x| !x.is_null()).cloned();
    let set_key = set.as_ref().map(|x| s(x, "key")).unwrap_or_default();
    let pinned = set.as_ref().and_then(|x| api::uo(x, "pinned"));
    let minor_of = api::arr(&info, "minor_of").to_vec();
    let fuller_than = api::arr(&info, "fuller_than").to_vec();
    let relations = api::arr(&info, "relations").to_vec();
    let kind_label = |k: &str| match k {
        "minor" => state::tr("minor version of"),
        "not_minor" => state::tr("not a minor version of"),
        "group" => state::tr("same version set as"),
        _ => state::tr("in no version set"),
    };
    let pair_rows = |list: Vec<Value>| {
        rsx! {
            ul {
                for o in list {
                    {
                        let other = u(&o, "title_id");
                        let src = s(&o, "source");
                        rsx! {
                            li {
                                OtherLink { o: o.clone() }
                                " "
                                Html { html: badge(&source_label(&src), "dim") }
                                if src != "user" {
                                    " "
                                    button { class: "btn small", title: state::tr("they are not minor and fuller versions of each other"), onclick: move |_| post(json!({ "kind": "not_minor", "other_title_id": other })), {state::tr("not a minor version")} }
                                }
                            }
                        }
                    }
                }
            }
        }
    };
    let pick = move |title: Option<u64>| {
        let key = set_key.clone();
        spawn(async move {
            match api::put(&format!("/api/v1/catalog/version-groups/{}/pick", api::encode(&key)), json!({ "title_id": title })).await {
                Ok(_) => on_change.call(()),
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    let key_for_roll = set.as_ref().map(|x| s(x, "key")).unwrap_or_default();
    rsx! {
        h2 { {state::tr("Versions")} }
        if api::b(&info, "hidden") {
            p { class: "notice", {state::tr("\"Hide minor versions\" leaves this title out: a fuller version covers all its platforms and regions.")} }
        }
        if !minor_of.is_empty() {
            h3 { {state::tr("Minor version of")} }
            {pair_rows(minor_of.clone())}
        }
        if !fuller_than.is_empty() {
            h3 { {state::tr("Fuller version of")} }
            {pair_rows(fuller_than.clone())}
        }
        if let Some(set) = &set {
            h3 {
                {state::tr("Version set")}
                " "
                button { class: "btn small", title: state::tr("show another version of this set"),
                    onclick: move |_| { let key = key_for_roll.clone(); spawn(async move { if reroll(&key).await { on_change.call(()); } }); },
                    "🎲"
                }
            }
            ul {
                for x in api::arr(set, "versions").to_vec() {
                    {
                        let titles = api::arr(&x, "titles").to_vec();
                        let first = titles.first().map(|t| u(t, "title_id"));
                        let is_pinned = pinned.is_some_and(|p| titles.iter().any(|t| u(t, "title_id") == p));
                        let (unpin, show) = (pick.clone(), pick.clone());
                        rsx! {
                            li {
                                for (i, t) in titles.iter().enumerate() {
                                    if i > 0 { ", " }
                                    OtherLink { o: t.clone() }
                                }
                                if api::b(&x, "shown") { " " Html { html: badge(&state::tr("shown"), "ok") } }
                                if api::b(&x, "owned") { " " Html { html: badge(&state::tr("held"), "dim") } }
                                " "
                                if is_pinned {
                                    button { class: "btn small", onclick: move |_| unpin(None), {state::tr("unpin")} }
                                } else if let Some(f) = first {
                                    button { class: "btn small", onclick: move |_| show(Some(f)), {state::tr("show this one")} }
                                }
                            }
                        }
                    }
                }
            }
            div { class: "muted", {state::tr("\"One of each version group\" lists one version: one you hold, else your pick, else a random one that stays until you press 🎲.")} }
        }
        if minor_of.is_empty() && fuller_than.is_empty() && set.is_none() {
            p { class: "muted", {state::tr("No other versions known.")} }
        }
        if !relations.is_empty() {
            h3 { {state::tr("Your corrections")} }
            ul {
                for r in relations {
                    {
                        let rid = u(&r, "id");
                        let label = kind_label(&s(&r, "kind"));
                        let other = r.get("other").filter(|o| !o.is_null()).cloned();
                        rsx! {
                            li {
                                "{label} "
                                if let Some(o) = other { OtherLink { o } }
                                " "
                                span { class: "link", title: state::tr("remove this correction"),
                                    onclick: move |_| {
                                        spawn(async move {
                                            match api::delete(&format!("/api/v1/catalog/titles/versions/relations/{rid}")).await {
                                                Ok(_) => on_change.call(()),
                                                Err(e) => ui::toast(e.message, true),
                                            }
                                        });
                                    },
                                    "✕"
                                }
                            }
                        }
                    }
                }
            }
        }
        div { class: "toolbar", style: "margin-top:6px",
            button { class: "btn small",
                onclick: move |_| ui::open_modal(move || rsx! { super::patched::PickTitle { heading: state::tr("This title is a minor version of"), on_pick: move |other: u64| post(json!({ "kind": "minor", "other_title_id": other })) } }),
                {state::tr("mark as minor version of…")}
            }
            " "
            button { class: "btn small",
                onclick: move |_| ui::open_modal(move || rsx! { super::patched::PickTitle { heading: state::tr("Same version set as"), on_pick: move |other: u64| post(json!({ "kind": "group", "other_title_id": other })) } }),
                {state::tr("add to a version set with…")}
            }
            if set.is_some() {
                " "
                button { class: "btn small", onclick: move |_| post(json!({ "kind": "ungroup" })), {state::tr("take out of its version set")} }
            }
        }
    }
}
