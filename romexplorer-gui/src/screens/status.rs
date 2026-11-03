//! Status: database, jobs, providers, hash lookup, table row counts.

use dioxus::prelude::*;
use serde_json::Value;

use crate::api::{self, s, u};
use crate::html;
use crate::state;
use crate::ui::Html;
use crate::Route;

#[component]
pub fn Status() -> Element {
    rsx! { StatusPage {} }
}

#[component]
pub fn StatusPage() -> Element {
    let status = use_resource(|| async { api::get("/api/v1/status").await });
    let nav = navigator();
    let read = status.read();
    let st = match &*read {
        None => return rsx! { div { class: "muted", {state::tr("loading…")} } },
        Some(Err(e)) => return rsx! { div { class: "card bad", "{e}" } },
        Some(Ok(v)) => v.clone(),
    };
    let db = st.get("database").cloned().unwrap_or_default();
    rsx! {
        div { class: "cards",
            div { class: "card",
                h2 { {state::tr("Database")} " " {crate::ui::key_explain("explain.status.database")} }
                div { class: "kv",
                    div { {state::tr("app root")} } div { class: "mono", "{s(&st, \"app_root\")}" }
                    div { {state::tr("server")} } div { "{s(&db, \"host\")}:{u(&db, \"port\")} ({s(&db, \"server_version\")})" }
                    div { {state::tr("database")} } div { "{s(&db, \"database\")}" }
                    div { {state::tr("user")} } div { "{s(&db, \"user\")}" }
                    div { {state::tr("directories")} } div { "{u(&st, \"directories\")}" }
                    div { {state::tr("local files")} } div { "{u(&st, \"local_files\")}" }
                }
            }
            div { class: "card",
                h2 { {state::tr("Jobs")} " " {crate::ui::key_explain("explain.status.jobs")} }
                div { class: "kv",
                    for j in api::arr(&st, "jobs") {
                        div { "{s(j, \"status\")}" } div { "{u(j, \"count\")}" }
                    }
                }
                p { Link { class: "link", to: Route::Jobs {}, {state::tr("open jobs →")} } }
            }
            div { class: "card",
                h2 { {state::tr("Providers")} " " {crate::ui::key_explain("explain.status.providers")} }
                for p in api::arr(&st, "providers") {
                    span {
                        style: if api::b(p, "available") { "opacity:1" } else { "opacity:.35" },
                        Html { html: html::provider_icon_titled(&s(p, "name"), &format!("{}: {}", s(p, "name"), provider_state(p))) }
                        " "
                    }
                }
                p { Link { class: "link", to: Route::DataProviders {}, {state::tr("details →")} } }
            }
            div { class: "card", h2 { {state::tr("Hash lookup")} " " {crate::ui::key_explain("explain.status.lookup")} } HashLookup {} }
        }
        div { class: "card",
            h2 { {state::tr("Table row counts")} " " {crate::ui::key_explain("explain.status.tables")} }
            table { class: "grid",
                thead { tr { th { {state::tr("table")} } th { {state::tr("rows")} } } }
                tbody {
                    for t in api::arr(&st, "tables").to_vec() {
                        tr { class: "click", onclick: move |_| { nav.push(Route::DataDbTable { table: s(&t, "table") }); },
                            td { "{s(&t, \"table\")}" } td { class: "num", "{u(&t, \"rows\")}" }
                        }
                    }
                }
            }
        }
    }
}

fn provider_state(p: &Value) -> String {
    if api::b(p, "available") {
        state::tr("available")
    } else {
        api::so(p, "reason").unwrap_or_else(|| state::tr("unavailable"))
    }
}

#[component]
fn HashLookup() -> Element {
    let mut algorithm = use_signal(String::new);
    let mut value = use_signal(String::new);
    let mut out = use_signal(|| None::<Result<Vec<Value>, String>>);
    let nav = navigator();
    rsx! {
        form { class: "inline",
            onsubmit: move |e| {
                e.prevent_default();
                spawn(async move {
                    let path = format!("/api/v1/catalog/lookup{}", api::qs(&[("algorithm", algorithm()), ("value", value())]));
                    out.set(Some(api::get(&path).await.map(|v| api::items(&v)).map_err(|e| e.message)));
                });
            },
            label { {state::tr("algorithm")}
                select { onchange: move |e| algorithm.set(e.value()),
                    option { value: "", {state::tr("auto")} } option { "crc32" } option { "md5" } option { "sha1" } option { "sha256" } option { "ra_hash" }
                }
            }
            label { {state::tr("hex")} input { r#type: "text", placeholder: state::tr("hash"), style: "min-width:300px", value: "{value}", oninput: move |e| value.set(e.value()) } }
            button { class: "btn primary", {state::tr("lookup")} }
        }
        match out() {
            None => rsx! {},
            Some(Err(e)) => rsx! { span { class: "bad", "{e}" } },
            Some(Ok(rows)) if rows.is_empty() => rsx! { span { class: "muted", {state::tr("no match")} } },
            Some(Ok(rows)) => rsx! {
                table { class: "grid",
                    for r in rows {
                        tr { class: "click", onclick: move |_| { nav.push(Route::Release { id: u(&r, "release_id") }); },
                            td { "{s(&r, \"algorithm\")}" }
                            td { "{s(&r, \"file_name\")}" }
                            td { Html { html: html::cbadge("platform", &s(&r, "platform"), None, None) } }
                            td { "{s(&r, \"release_name\")}" }
                            td { Html { html: html::region_badges(&s(&r, "region")) } }
                            td { Html { html: html::cbadge("source", &s(&r, "source"), None, None) } }
                        }
                    }
                }
            },
        }
    }
}
