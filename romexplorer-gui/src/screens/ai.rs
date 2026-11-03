//! Config › AI (the console's `showAiPage`): which AI answers in the app and with which model,
//! the Ask menu's first link, the AI providers' keys with a Test button each, the price table
//! the approximate cost uses, and the saved answers (stats, clear, export / import).

use std::collections::BTreeMap;

use dioxus::prelude::*;
use serde_json::{json, Value};

use crate::api::{self, s, u};
use crate::state;
use crate::ui;

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
        div { class: "card",
            super::data::Credentials { on_change: move |_| overview.restart(), scope: "ai".to_string() }
            AiTests { o: o.clone() }
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
            match api::post("/api/v1/ai/clear", json!({ "purpose": if purpose.is_empty() { Value::Null } else { json!(purpose) }, "keep_pinned": keep_pinned() })).await {
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
