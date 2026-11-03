//! Config › AI (the console's `showAiPage`): which AI answers in the app and with which model,
//! the Ask menu's first link, the AI providers' keys with a Test button each, the price table
//! the approximate cost uses, and the saved answers (stats, clear, export / import).

use std::collections::BTreeMap;

use dioxus::prelude::*;
use serde_json::{json, Value};

use crate::api::{self, s, u};
use crate::state;
use crate::ui::{self, Html};

fn provider_label(id: &str) -> &'static str {
    match id {
        "gemini" => "Gemini",
        "claude" => "Claude",
        "openai" => "OpenAI",
        _ => "Perplexity",
    }
}

/// Saves a setting; empty (or null) clears it, so the default applies again.
async fn set_key(key: &str, value: Value) -> Result<(), String> {
    let empty = value.is_null() || value.as_str().is_some_and(str::is_empty);
    let path = format!("/api/v1/settings/{key}");
    let r = if empty { api::delete(&path).await.map(|_| ()) } else { api::put(&path, value).await.map(|_| ()) };
    r.map_err(|e| e.message)
}

#[component]
pub fn ConfigAi() -> Element {
    if !api::is_admin() {
        return rsx! { div { class: "card muted", {state::tr("Only an administrator can change provider credentials.")} } };
    }
    let mut overview = use_resource(|| async { api::get("/api/v1/ai").await });
    let o = match &*overview.read() {
        None => return rsx! { div { class: "card muted", {state::tr("loading…")} } },
        Some(Err(e)) => return rsx! { div { class: "card bad", "{e.message}" } },
        Some(Ok(v)) => v.clone(),
    };
    rsx! {
        AiChoices { key: "{o}", o: o.clone(), on_saved: move |_| overview.restart() }
        {rsx! { AiSourceOrder { key: "{o}", o: o.clone(), on_saved: move |_| overview.restart() } }}
        div { class: "card",
            super::data::Credentials { on_change: move |_| overview.restart(), scope: "ai".to_string() }
            AiTests { o: o.clone() }
            p { class: "notice", {state::tr("Create a separate API key (or project) just for romexplorer, and set a budget or spending limit for it with the provider, so a mistake or a long batch cannot run up a large bill.")} }
            div { class: "toolbar",
                for (i, (id, url)) in AI_LIMIT_LINKS.iter().enumerate() {
                    if i > 0 { " · " }
                    a { class: "link", href: "{url}", target: "_blank", rel: "noopener", {state::trf("{provider}: billing and limits", &[("provider", provider_label(id))])} }
                }
            }
        }
        // Keyed by what was loaded, so a save or a reset shows the table as it now is.
        {rsx! { AiPrices { key: "{o}", o: o.clone(), on_saved: move |_| overview.restart() } }}
        AiCache { o: o.clone(), on_change: move |_| overview.restart() }
    }
}

#[component]
fn AiChoices(o: Value, on_saved: EventHandler<()>) -> Element {
    let providers = api::arr(&o, "providers").to_vec();
    let mut provider = use_signal(|| s(&o, "provider"));
    let mut ask_first = use_signal(|| s(&o, "ask_first"));
    let mut cache_days = use_signal(|| api::uo(&o, "cache_days").map(|d| d.to_string()).unwrap_or_default());
    let mut models = use_signal(|| providers.iter().map(|p| (s(p, "id"), p.get("model").and_then(Value::as_str).unwrap_or("").to_string())).collect::<BTreeMap<String, String>>());
    let links: Vec<(String, String)> = api::arr(&o, "ask_links").iter().filter_map(|l| Some((l.get(0)?.as_str()?.to_string(), l.get(1)?.as_str()?.to_string()))).collect();
    let save = move |e: Event<FormData>| {
        e.prevent_default();
        spawn(async move {
            let mut result = set_key("ai_provider", json!(provider())).await;
            for (id, model) in models() {
                if result.is_ok() {
                    result = set_key(&format!("ai_model_{id}"), json!(model.trim())).await;
                }
            }
            if result.is_ok() {
                result = set_key("ai_ask_first", json!(ask_first())).await;
            }
            if result.is_ok() {
                result = set_key("ai_cache_days", cache_days().trim().parse::<u64>().ok().filter(|d| *d > 0).map_or(Value::Null, |d| json!(d))).await;
            }
            match result {
                Ok(()) => {
                    ui::toast(state::tr("saved"), false);
                    on_saved.call(());
                }
                Err(e) => ui::toast(e, true),
            }
        });
    };
    rsx! {
        div { class: "card",
            h2 { {state::tr("AI")} }
            p { class: "muted", {state::tr("The AI that answers in the app (Merge review › What differs) is paid per use with your own API key. The Ask menu opens the same question in the browser instead, signed in as you, at no cost here.")} }
            form { class: "stack", onsubmit: save,
                label { {state::tr("answers in the app")} " "
                    select { value: "{provider}", onchange: move |e| provider.set(e.value()),
                        for p in providers.iter() {
                            option { value: "{s(p, \"id\")}", selected: s(p, "id") == provider(),
                                {provider_label(&s(p, "id"))}
                                if !api::b(p, "key_set") { " " {state::tr("(no key)")} }
                            }
                        }
                    }
                }
                table { class: "grid",
                    thead { tr { th { {state::tr("provider")} } th { {state::tr("model")} } } }
                    tbody {
                        for p in providers.iter() {
                            {
                                let id = s(p, "id");
                                let current = models().get(&id).cloned().unwrap_or_default();
                                rsx! {
                                    tr {
                                        td { {provider_label(&id)} }
                                        td { input { value: "{current}", placeholder: "{s(p, \"default_model\")}", style: "min-width:240px", oninput: move |e| { models.write().insert(id.clone(), e.value()); } } }
                                    }
                                }
                            }
                        }
                    }
                }
                label { {state::tr("Ask menu: first link")} " "
                    select { value: "{ask_first}", onchange: move |e| ask_first.set(e.value()),
                        for (id, label) in links {
                            option { value: "{id}", selected: id == ask_first(), "{label}" }
                        }
                    }
                }
                label { {state::tr("ask saved answers again after (days; empty: keep)")} " "
                    input { r#type: "number", min: "0", style: "width:90px", value: "{cache_days}", oninput: move |e| cache_days.set(e.value()) }
                }
                div { class: "toolbar actions", button { class: "btn primary", r#type: "submit", {state::tr("save")} } }
            }
        }
    }
}

/// Which saved answer is shown first when one question has several (`ai_source_priority`),
/// as an ordered list to move entries up and down in (the console's same list).
#[component]
fn AiSourceOrder(o: Value, on_saved: EventHandler<()>) -> Element {
    let mut order = use_signal(|| {
        let mut list: Vec<String> = api::arr(&o, "source_priority").iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
        if list.is_empty() {
            list.push("curated".into());
        }
        for k in api::arr(&o, "source_keys").iter().filter_map(|v| v.as_str()) {
            if !list.iter().any(|x| x == k) {
                list.push(k.to_string());
            }
        }
        list
    });
    let save = move |_| {
        spawn(async move {
            match set_key("ai_source_priority", json!(order())).await {
                Ok(()) => ui::toast(state::tr("saved"), false),
                Err(e) => ui::toast(e, true),
            }
        });
    };
    let reset = move |_| {
        spawn(async move {
            match set_key("ai_source_priority", Value::Null).await {
                Ok(()) => on_saved.call(()),
                Err(e) => ui::toast(e, true),
            }
        });
    };
    let n = order().len();
    rsx! {
        div { class: "card",
            h3 { {state::tr("Which answer is shown first")} }
            p { class: "muted", {state::tr("When one question has several saved answers (the curated note, API answers, answers pasted from the browser), the one highest here is shown and its rank used. Sources not moved keep the newest first.")} }
            ol {
                for (i, k) in order().into_iter().enumerate() {
                    li { key: "{k}",
                        {source_label(&k)}
                        " "
                        button { class: "btn small", r#type: "button", disabled: i == 0, onclick: move |_| { order.write().swap(i, i - 1); }, "▲" }
                        button { class: "btn small", r#type: "button", disabled: i + 1 == n, onclick: move |_| { order.write().swap(i, i + 1); }, "▼" }
                    }
                }
            }
            div { class: "toolbar",
                button { class: "btn small primary", r#type: "button", onclick: save, {state::tr("save")} }
                " "
                button { class: "btn small", r#type: "button", onclick: reset, {state::tr("reset to the default (curated first, then newest)")} }
            }
        }
    }
}

#[component]
fn AiTests(o: Value) -> Element {
    let mut busy = use_signal(|| None::<String>);
    let providers: Vec<String> = api::arr(&o, "providers").iter().map(|p| s(p, "id")).collect();
    rsx! {
        div { class: "toolbar",
            for id in providers {
                {
                    let running = busy().as_deref() == Some(id.as_str());
                    rsx! {
                        button { class: "btn small", r#type: "button", disabled: running, title: state::tr("asks a one-word question with this key and model"),
                            onclick: move |_| {
                                let id = id.clone();
                                busy.set(Some(id.clone()));
                                spawn(async move {
                                    match api::post(&format!("/api/v1/ai/test/{id}"), json!({})).await {
                                        Ok(r) if api::b(&r, "ok") => ui::toast(state::trf("{provider} answered ({model})", &[("provider", provider_label(&id)), ("model", &s(&r, "model"))]), false),
                                        Ok(r) => ui::toast(s(&r, "message"), true),
                                        Err(e) => ui::toast(e.message, true),
                                    }
                                    busy.set(None);
                                });
                            },
                            {state::trf("test {provider}", &[("provider", provider_label(&id))])}
                        }
                        " "
                    }
                }
            }
        }
    }
}

#[component]
fn AiPrices(o: Value, on_saved: EventHandler<()>) -> Element {
    // (model prefix, in, out), and an empty row to add one.
    let mut rows = use_signal(|| {
        let mut r: Vec<(String, String, String)> = o.get("prices").and_then(Value::as_object).into_iter().flatten().map(|(m, p)| (m.clone(), p["in"].to_string(), p["out"].to_string())).collect();
        r.push(Default::default());
        r
    });
    let save = move |_| {
        let mut out = serde_json::Map::new();
        for (m, i, o) in rows() {
            if let (false, Ok(i), Ok(o)) = (m.trim().is_empty(), i.trim().parse::<f64>(), o.trim().parse::<f64>()) {
                out.insert(m.trim().to_string(), json!({ "in": i, "out": o }));
            }
        }
        spawn(async move {
            match set_key("ai_prices", Value::Object(out)).await {
                Ok(()) => {
                    ui::toast(state::tr("saved"), false);
                    on_saved.call(());
                }
                Err(e) => ui::toast(e, true),
            }
        });
    };
    let reset = move |_| {
        spawn(async move {
            match set_key("ai_prices", Value::Null).await {
                Ok(()) => on_saved.call(()),
                Err(e) => ui::toast(e, true),
            }
        });
    };
    rsx! {
        div { class: "card",
            h2 { {state::tr("Prices")} }
            p { class: "muted", {state::tr("US dollars per million tokens, for the approximate cost shown with each answer. A model takes the row with the longest matching name prefix. Change a price, or add a row, and save.")} }
            table { class: "grid",
                thead { tr { th { {state::tr("model")} } th { class: "num", {state::tr("in")} } th { class: "num", {state::tr("out")} } } }
                tbody {
                    for (i, (m, pin, pout)) in rows().into_iter().enumerate() {
                        tr {
                            td { input { value: "{m}", placeholder: state::tr("model id prefix"), oninput: move |e| rows.write()[i].0 = e.value() } }
                            td { input { r#type: "number", step: "any", min: "0", style: "width:90px", value: "{pin}", oninput: move |e| rows.write()[i].1 = e.value() } }
                            td { input { r#type: "number", step: "any", min: "0", style: "width:90px", value: "{pout}", oninput: move |e| rows.write()[i].2 = e.value() } }
                        }
                    }
                }
            }
            div { class: "toolbar",
                button { class: "btn primary", r#type: "button", onclick: save, {state::tr("save")} }
                " "
                button { class: "btn", r#type: "button", onclick: reset, {state::tr("reset to the built-in prices")} }
            }
        }
    }
}

#[component]
fn AiCache(o: Value, on_change: EventHandler<()>) -> Element {
    let st = o.get("stats").cloned().unwrap_or_default();
    let mut what = use_signal(String::new);
    let mut from = use_signal(String::new);
    let source_keys: Vec<String> = api::arr(&o, "source_keys").iter().filter_map(|v| v.as_str().map(str::to_string)).filter(|k| k != "curated").collect();
    let mut keep_pinned = use_signal(|| true);
    let mut chosen = use_signal(|| None::<dioxus::html::FileData>);
    let purposes = api::arr(&st, "purposes").to_vec();
    let f = |k: &str| st.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    let clear = move |_| {
        spawn(async move {
            if !ui::confirm(&state::tr("Remove the saved AI answers? Asking again costs money.")).await {
                return;
            }
            let purpose = what();
            let chosen = from();
            let (source, provider) = chosen.split_once(':').unwrap_or(("", ""));
            let opt = |x: &str| if x.is_empty() { Value::Null } else { json!(x) };
            match api::post("/api/v1/ai/clear", json!({ "purpose": opt(&purpose), "keep_pinned": keep_pinned(), "source": opt(source), "provider": opt(provider) })).await {
                Ok(r) => {
                    ui::toast(state::trf("{n} answer(s) removed", &[("n", &u(&r, "removed").to_string())]), false);
                    on_change.call(());
                }
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    let export = move |_| {
        spawn(async move {
            match api::get("/api/v1/ai/export").await {
                Ok(list) => super::data::save_text(&serde_json::to_string_pretty(&list).unwrap_or_default(), "romexplorer-ai-answers.json", "application/json").await,
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    let import = move |_| {
        let Some(file) = chosen() else { ui::toast(state::tr("choose a file"), true); return; };
        spawn(async move {
            let list = match file.read_bytes().await {
                Ok(bytes) => serde_json::from_slice::<Value>(&bytes).ok(),
                Err(e) => { ui::toast(e.to_string(), true); return; }
            };
            let Some(list) = list else { ui::toast(state::tr("That file is not JSON."), true); return; };
            let list = if list.is_array() { list } else { json!([]) };
            match api::post("/api/v1/ai/import", list).await {
                Ok(r) => {
                    ui::toast(state::trf("{n} answer(s) imported", &[("n", &u(&r, "imported").to_string())]), false);
                    on_change.call(());
                }
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    rsx! {
        div { class: "card",
            h2 { {state::tr("Saved answers")} }
            p {
                {state::trf("{n} answer(s), {pinned} pinned, {size}", &[("n", &u(&st, "count").to_string()), ("pinned", &u(&st, "pinned").to_string()), ("size", &crate::html::fmt_size(Some(u(&st, "bytes"))))])}
                " · "
                {state::trf("approx. US${cost} this month, US${total} in all", &[("cost", &format!("{:.2}", f("cost_month_usd"))), ("total", &format!("{:.2}", f("cost_total_usd")))])}
            }
            div { class: "toolbar",
                select { value: "{what}", onchange: move |e| what.set(e.value()),
                    option { value: "", {state::tr("all features")} }
                    for p in purposes {
                        option { value: "{s(&p, \"purpose\")}", "{s(&p, \"purpose\")} ({u(&p, \"count\")})" }
                    }
                }
                select { value: "{from}", onchange: move |e| from.set(e.value()),
                    option { value: "", {state::tr("all sources")} }
                    for k in source_keys {
                        option { value: "{k}", {source_label(&k)} }
                    }
                    option { value: "api:", {state::tr("every API answer")} }
                    option { value: "browser:", {state::tr("every pasted answer")} }
                }
                label { style: "flex-direction:row;align-items:center;gap:4px",
                    input { r#type: "checkbox", checked: keep_pinned(), onchange: move |e| keep_pinned.set(e.checked()) }
                    {state::tr("keep pinned answers")}
                }
                button { class: "btn danger", r#type: "button", onclick: clear, {state::tr("clear saved answers")} }
                span { class: "grow" }
                button { class: "btn", r#type: "button", onclick: export, {state::tr("export JSON")} }
                input { r#type: "file", accept: ".json,application/json", onchange: move |e| chosen.set(e.files().into_iter().next()) }
                button { class: "btn", r#type: "button", onclick: import, {state::tr("import JSON")} }
            }
        }
    }
}

/// Where each provider sets a budget or spending limit (the console's AI_LIMIT_LINKS).
pub const AI_LIMIT_LINKS: [(&str, &str); 4] = [
    ("gemini", "https://console.cloud.google.com/billing/budgets"),
    ("claude", "https://console.anthropic.com/settings/limits"),
    ("openai", "https://platform.openai.com/settings/organization/limits"),
    ("perplexity", "https://www.perplexity.ai/settings/api"),
];

const AI_SITES: [(&str, &str); 4] = [("google", "Google AI Mode"), ("claude", "Claude"), ("chatgpt", "ChatGPT"), ("perplexity", "Perplexity")];

/// A saved AI answer's source as a label (the console's `aiSourceLabel`).
pub fn source_label(key: &str) -> String {
    if key == "curated" {
        return state::tr("curated note");
    }
    let (src, who) = key.split_once(':').unwrap_or((key, ""));
    if src == "api" {
        state::trf("{provider} API", &[("provider", provider_label(who))])
    } else {
        let site = AI_SITES.iter().find(|(id, _)| *id == who).map_or(who, |(_, l)| *l);
        state::trf("{site} (browser)", &[("site", site)])
    }
}

/// The AI's rank: dashed, so it never reads as the curated rank badge.
pub fn ai_rank_badge(rank: &str, source: &str) -> String {
    let rank = if rank == "none" { state::tr("none") } else { rank.to_string() };
    format!(
        r#"<span class="badge" style="border:1px dashed currentColor;background:transparent" title="{}">{} · {}</span>"#,
        crate::html::esc(&state::tr("AI rank: information only; the Definitive filter uses the curated notes")),
        crate::html::esc(&state::trf("AI {rank}", &[("rank", &rank)])),
        crate::html::esc(&source_label(source))
    )
}

/// Before the first paid AI question: the cost warning, once (`ai_cost_warning_seen`, shared
/// with the console). `true` to go ahead.
pub async fn cost_warning_ok() -> bool {
    if api::get("/api/v1/settings/ai_cost_warning_seen").await.ok().and_then(|v| v.get("value").and_then(Value::as_bool)) == Some(true) {
        return true;
    }
    if !ui::confirm(&state::tr("Asking the AI uses your own API key, and the provider bills you for it. The cost shown is an estimate, not a guarantee: actual charges can be higher (provider pricing, web-search fees, retries). The author of this software is not responsible for any charges. Tip: create a separate API key for romexplorer and set a budget or spending limit for it with the provider (Config › AI has the links).")).await {
        return false;
    }
    let _ = api::put("/api/v1/settings/ai_cost_warning_seen", json!(true)).await;
    true
}

/// Collection › Definitive › a family (the console's `familyAiPanel`): which version to
/// play, from the in-app AI or an answer pasted from the browser. Opening the page shows only
/// what is saved; asking is a click, since each ask costs money.
#[component]
pub fn FamilyAi(head: u64, curated: Option<String>) -> Element {
    // What the next request asks for ("", "ask", "again").
    let mut mode = use_signal(|| "");
    let mut answer = use_resource(move || async move {
        let m = mode();
        api::post(&format!("/api/v1/catalog/definitive/{head}/ask"), json!({ "lang": state::ui_language(), "ask": m == "ask", "again": m == "again" })).await
    });
    let mut pasted = use_signal(String::new);
    let mut site = use_signal(|| "google".to_string());
    let title = state::tr("Which version to play");
    let v = match &*answer.read() {
        None => return rsx! { div { class: "card", h2 { "{title}" } span { class: "muted", {state::tr("loading…")} } } },
        Some(Err(e)) => return rsx! { div { class: "card", h2 { "{title}" } span { class: "bad", "{e.message}" } } },
        Some(Ok(v)) => v.clone(),
    };
    let _ = curated;
    let saved = v.get("answer").filter(|x| !x.is_null()).cloned();
    let other = v.get("other_language").filter(|x| !x.is_null()).cloned();
    let shown = saved.clone().or_else(|| other.clone());
    let rank_html = match (api::so(&v, "rank"), api::so(&v, "rank_source")) {
        (Some(r), Some(src)) if src == "curated" => format!("{} <span class=\"muted\">{}</span>", super::definitive::rank_badge(&r), crate::html::esc(&source_label("curated"))),
        (Some(r), Some(src)) => ai_rank_badge(&r, &src),
        _ => String::new(),
    };
    let lang = s(&v, "lang");
    let links = api::arr(&v, "ask_links").to_vec();
    let pinned = saved.as_ref().is_some_and(|x| api::b(x, "pinned"));
    let saved_id = saved.as_ref().map(|x| u(x, "id"));
    let is_api = saved.as_ref().is_some_and(|x| s(x, "source") == "api");
    let ask = move |m: &'static str| {
        spawn(async move {
            if cost_warning_ok().await {
                mode.set(m);
                answer.restart();
            }
        });
    };
    let save_paste = move |_| {
        let text = pasted().trim().to_string();
        if text.is_empty() {
            ui::toast(state::tr("nothing pasted"), true);
            return;
        }
        spawn(async move {
            match api::post(&format!("/api/v1/catalog/definitive/{head}/paste"), json!({ "lang": state::ui_language(), "site": site(), "answer": text })).await {
                Ok(_) => {
                    ui::toast(state::tr("saved"), false);
                    pasted.set(String::new());
                    mode.set("");
                    answer.restart();
                }
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    rsx! {
        div { class: "card",
            h2 { "{title} " Html { html: rank_html } }
            if saved.is_none() {
                if let Some(o) = &other {
                    div { class: "muted", {state::trf("Saved in {language}.", &[("language", &state::language_native(&s(o, "lang")))])} }
                }
            }
            if let Some(x) = &shown {
                div { style: "white-space:pre-wrap;font-size:13px", "{s(x, \"answer\")}" }
                if !api::arr(x, "sources").is_empty() {
                    div { class: "muted", style: "font-size:12px;margin-top:4px",
                        {state::tr("sources:")}
                        for src in api::arr(x, "sources").to_vec() {
                            " "
                            a { class: "link", href: "{s(&src, \"url\")}", target: "_blank", rel: "noopener", {if s(&src, "title").is_empty() { s(&src, "url") } else { s(&src, "title") }} }
                        }
                    }
                }
                div { class: "muted", style: "font-size:12px;margin-top:4px",
                    {source_label(&format!("{}:{}", s(x, "source"), s(x, "provider")))}
                    " · "
                    {state::trf("saved on {date} · {model}", &[("date", &crate::html::fmt_date(&s(x, "created_at"))), ("model", &s(x, "model"))])}
                    if let Some(c) = x.get("cost_usd").and_then(Value::as_f64) {
                        " · " {state::trf("approx. US${cost}", &[("cost", &format!("{c:.4}"))])}
                    }
                    if api::b(x, "pinned") { " " Html { html: crate::html::badge(&state::tr("pinned"), "ok") } }
                }
            } else {
                p { class: "muted", {state::tr("No AI answer saved for this game yet. Ask the in-app AI, or ask in the browser and paste the answer below.")} }
            }
            if let Some(err) = api::so(&v, "error") { div { class: "muted", "{err}" } }
            div { class: "toolbar", style: "margin-top:6px",
                if saved.is_none() && other.is_none() {
                    button { class: "btn small primary", title: state::tr("asks the in-app AI (Config › AI); each question costs a little"), onclick: move |_| ask("ask"), {state::tr("Ask the AI")} }
                }
                if saved.is_none() && other.is_some() {
                    button { class: "btn small primary", onclick: move |_| ask("ask"), {state::trf("Ask in {language}", &[("language", &state::language_native(&lang))])} }
                }
                if is_api && !pinned {
                    button { class: "btn small", title: state::tr("asks the AI again and replaces this answer"), onclick: move |_| ask("again"), {state::tr("Ask again")} }
                }
                if let Some(id) = saved_id {
                    button { class: "btn small", title: state::tr("a pinned answer is kept: never asked again, expired or cleared with the cache"),
                        onclick: move |_| {
                            spawn(async move {
                                match api::put(&format!("/api/v1/ai/answers/{id}/pin"), json!({ "pinned": !pinned })).await {
                                    Ok(_) => { mode.set(""); answer.restart(); }
                                    Err(e) => ui::toast(e.message, true),
                                }
                            });
                        },
                        {if pinned { state::tr("unpin") } else { state::tr("pin") }}
                    }
                }
            }
            div { class: "toolbar", style: "margin-top:6px",
                span { class: "muted", title: state::tr("opens the question in the browser, signed in as you"), {state::tr("Ask:")} }
                for (i, l) in links.into_iter().enumerate() {
                    {
                        let id = s(&l, "id");
                        rsx! {
                            " "
                            a { class: if i == 0 { "btn small primary" } else { "btn small" }, href: "{s(&l, \"url\")}", target: "_blank", rel: "noopener", onclick: move |_| site.set(id.clone()), "{s(&l, \"label\")}" }
                        }
                    }
                }
                " "
                a { class: "link", href: "{s(&v, \"google_url\")}", target: "_blank", rel: "noopener", {state::tr("search")} }
            }
            details { style: "margin-top:6px",
                summary { {state::tr("Paste an answer from the browser")} }
                textarea { rows: "6", style: "width:100%", placeholder: state::tr("Paste the whole answer, starting with its RANK: line"), value: "{pasted}", oninput: move |e| pasted.set(e.value()) }
                div { class: "toolbar",
                    label { {state::tr("from")} " "
                        select { value: "{site}", onchange: move |e| site.set(e.value()),
                            for (id, label) in AI_SITES {
                                option { value: "{id}", selected: site() == id, "{label}" }
                            }
                        }
                    }
                    button { class: "btn small primary", r#type: "button", onclick: save_paste, {state::tr("save pasted answer")} }
                }
            }
        }
    }
}

/// The batch dialog's warning (the console's `AI_BATCH_WARNING`).
const AI_BATCH_WARNING: &str = "This sends one paid question per game to the AI provider, which can use many tokens. The cost shown is an estimate, not a guarantee: actual charges can be higher (provider pricing, web-search fees, retries). It is billed by the provider to your own API key. The author of this software is not responsible for any charges. Use a separate API key for romexplorer with a budget or spending limit set with the provider (Config › AI has the links).";

/// "Batch rate with AI" (the console's `aiBatchDialog`): the family question for every family
/// of `scope` (`title_ids`, `release_ids` or `set_id`), as an `ai_batch_rate` job. Shows what
/// it would ask and cost first, and starts only once the warning is ticked.
pub fn batch_dialog(scope: Value, what: String) {
    ui::open_modal(move || rsx! { BatchDialog { scope: scope.clone(), what: what.clone() } });
}

/// The selection-bar button: `key` is what the selected ids are (`title_ids`, `release_ids`).
pub fn batch_action(key: &'static str, kind: &'static str) -> crate::components::SelAction {
    crate::components::SelAction::new(&state::tr("batch rate with AI…"), move |ids: Vec<u64>| {
        let what = if kind == "games" { state::trf("{n} selected games", &[("n", &ids.len().to_string())]) } else { state::trf("{n} selected release(s)", &[("n", &ids.len().to_string())]) };
        let scope = json!({ key: ids });
        async move {
            batch_dialog(scope, what);
            false
        }
    })
}

#[component]
fn BatchDialog(scope: Value, what: String) -> Element {
    let mut reask = use_signal(|| false);
    let mut max = use_signal(String::new);
    let mut ok = use_signal(|| false);
    let mut job = use_signal(|| None::<u64>);
    let mut finished = use_signal(|| false);
    let scope2 = scope.clone();
    let body = move || {
        let mut b = scope2.clone();
        b["lang"] = json!(state::ui_language());
        b["reask"] = json!(reask());
        b["max"] = max().trim().parse::<u32>().ok().filter(|m| *m > 0).map_or(Value::Null, |m| json!(m));
        b
    };
    let body2 = body.clone();
    let est = use_resource(move || {
        let b = body2();
        async move { api::post("/api/v1/catalog/definitive/batch-estimate", b).await }
    });
    let e = match &*est.read() {
        Some(Ok(v)) => Some(v.clone()),
        _ => None,
    };
    let can = ok() && e.as_ref().is_some_and(|v| api::b(v, "has_key") && u(v, "to_ask") > 0);
    let start = move |_| {
        let mut b = body();
        b["job_type"] = json!("ai_batch_rate");
        spawn(async move {
            match api::post("/api/v1/jobs", b).await {
                Ok(r) => job.set(r.get("id").and_then(Value::as_u64)),
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    let busy = job().is_some();
    rsx! {
        h2 { {state::trf("Batch rate with AI: {what}", &[("what", &what)])} }
        match &*est.read() {
            None => rsx! { div { class: "muted", {state::tr("loading…")} } },
            Some(Err(err)) => rsx! { div { class: "bad", "{err.message}" } },
            Some(Ok(v)) => {
                let provider = s(v, "provider");
                let cost = match v.get("total_usd").and_then(Value::as_f64) {
                    Some(c) => state::trf("approx. US${cost}", &[("cost", &format!("{c:.2}"))]),
                    None => state::tr("unknown (no price for this model)"),
                };
                rsx! {
                    div { {state::trf("{n} game(s) to ask", &[("n", &u(v, "to_ask").to_string())])} " · " b { "{cost}" } " " span { class: "muted", "({provider_label(&provider)} {s(v, \"model\")})" } }
                    div { class: "muted", {state::trf("{families} game(s) selected: {one} with one version skipped, {cached} with a saved answer", &[("families", &u(v, "families").to_string()), ("one", &u(v, "one_version").to_string()), ("cached", &u(v, "cached").to_string())])} }
                    if !api::b(v, "has_key") {
                        div { class: "bad", {state::trf("no {provider} API key set (Config › AI)", &[("provider", provider_label(&provider))])} }
                    }
                }
            }
        }
        div { class: "kv", style: "grid-template-columns:max-content 1fr; gap:6px 10px; margin-top:8px",
            div {}
            div { label { input { r#type: "checkbox", disabled: busy, checked: reask(), onchange: move |e| reask.set(e.checked()) } " " {state::tr("re-ask games with a saved answer (replaces the API answer; pinned answers are kept)")} } }
            div { {state::tr("at most")} }
            div { input { r#type: "number", min: "1", size: "6", disabled: busy, placeholder: state::tr("all"), value: "{max}", onchange: move |e| max.set(e.value()) } " " span { class: "muted", {state::tr("games")} } }
        }
        p { class: "notice", {state::tr(AI_BATCH_WARNING)} }
        div { class: "toolbar",
            for (i, (id, url)) in AI_LIMIT_LINKS.iter().enumerate() {
                if i > 0 { " · " }
                a { class: "link", href: "{url}", target: "_blank", rel: "noopener", {state::trf("{provider}: billing and limits", &[("provider", provider_label(id))])} }
            }
        }
        label { style: "display:block;margin-top:8px", input { r#type: "checkbox", disabled: busy, checked: ok(), onchange: move |e| ok.set(e.checked()) } " " b { {state::tr("I understand")} } }
        if let Some(id) = job() {
            div { class: "card", super::backup::JobWatch { key: "{id}", job_id: id, on_done: move |_| finished.set(true) } }
        }
        div { class: "fsfoot",
            span { class: "grow" }
            if let Some(id) = job() {
                super::copy::RunningJobButtons { job_id: id, finished: finished() }
            } else {
                button { r#type: "button", class: "btn", onclick: move |_| ui::close_modal(), {state::tr("cancel")} }
                button { r#type: "button", class: "btn primary", disabled: !can, onclick: start, {state::tr("start")} }
            }
        }
    }
}
