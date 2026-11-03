//! About: the version and the release notes (the console's `showAbout`).

use dioxus::prelude::*;

use crate::api::{self, s};
use crate::state;
use crate::ui::HtmlDiv;

#[component]
pub fn About() -> Element {
    let data = use_resource(|| async { api::get(&format!("/api/v1/about?lang={}", state::setting("ui_language").as_str().unwrap_or("en"))).await });
    let app_version = env!("CARGO_PKG_VERSION");
    let state = data.read().clone();
    match state {
        None => rsx! { div { class: "card", h2 { "romexplorer {app_version}" } div { class: "muted", {state::tr("loading…")} } } },
        Some(Err(e)) => rsx! { div { class: "card", h2 { "romexplorer {app_version}" } div { class: "bad", "{e.message}" } } },
        Some(Ok(v)) => {
            let server_version = s(&v, "version");
            rsx! {
                div { class: "card",
                    h2 { "romexplorer {app_version}" }
                    if server_version != app_version { p { class: "muted", {state::trf("server: {version}", &[("version", &server_version)])} } }
                    p { class: "muted", {state::tr("Release notes")} }
                    HtmlDiv { class: "notes", html: s(&v, "notes_html") }
                }
            }
        }
    }
}
