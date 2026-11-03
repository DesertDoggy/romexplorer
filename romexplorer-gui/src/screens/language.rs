//! The UI language, like a web page's: the switcher at the top right of the header and the
//! first-start picker (the console's `renderLangSwitch` / `askLanguage`). Both write
//! `ui_language`, the setting Config › Settings edits, so all three always agree.

use std::rc::Rc;

use dioxus::core::spawn_forever;
use dioxus::prelude::*;
use serde_json::{json, Value};

use crate::{api, html, state, ui};

/// The languages the server has UI text for (the vocabulary's `ui_languages`): the two
/// built in, plus any `assets/i18n/<lang>.json` dropped beside it.
pub fn ui_languages() -> Vec<String> {
    state::vocab()
        .get("ui_languages")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>())
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| vec!["en".into(), "ja".into()])
}

/// "🇯🇵 JA  日本語": flag, code, and the language's own name.
pub fn choice_text(code: &str) -> String {
    let flag = html::lang_flag(code);
    format!("{}{}  {}", if flag.is_empty() { String::new() } else { format!("{flag} ") }, code.to_uppercase(), state::language_native(code))
}

/// Saves `ui_language` and shows every screen again in it.
pub async fn set_ui_language(code: String) {
    match api::put("/api/v1/settings/ui_language", json!(code)).await {
        Ok(_) => {
            state::reload_settings().await;
            *state::REDRAW.write() += 1;
        }
        Err(e) => ui::toast(e.message, true),
    }
}

/// The header's language menu. Only for an account that may change settings; the server
/// refuses everyone else.
#[component]
pub fn LanguageSwitch() -> Element {
    let _ = state::LOADED();
    if !api::is_admin() {
        return rsx! {};
    }
    let current = state::setting("ui_language").as_str().unwrap_or("en").to_string();
    let list = ui_languages();
    rsx! {
        select { class: "lang", title: state::tr("display language"),
            onchange: move |e| { let code = e.value(); spawn_forever(async move { set_ui_language(code).await; }); },
            for code in list {
                option { key: "{code}", value: "{code}", selected: code == current, {choice_text(&code)} }
            }
        }
    }
}

/// Whether to ask: never chosen (`ui_language` unset), and this account may set it.
pub fn should_ask() -> bool {
    api::is_admin() && state::setting("ui_language").is_null()
}

/// Opens the first-start picker, which has to be answered (a click beside it does not
/// close it); `then` runs after the chosen language is saved, from a detached task, so it
/// must not capture a component's handlers.
pub fn ask(then: impl Fn() + 'static) {
    let then = Then(Rc::new(then));
    let langs = ui_languages();
    ui::open_pinned_modal(move || rsx! { LanguageDialog { langs: langs.clone(), then: then.clone() } });
}

/// What runs after the picker: a plain `Rc`, not an `EventHandler`, because it fires after
/// the dialog that holds it has closed (see `mode::Proceed`).
#[derive(Clone)]
struct Then(Rc<dyn Fn()>);

impl PartialEq for Then {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

#[component]
fn LanguageDialog(langs: Vec<String>, then: Then) -> Element {
    rsx! {
        h2 { "Language · 言語" }
        div { class: "langpick",
            for code in langs {
                {
                    let then = then.clone();
                    let pick = code.clone();
                    rsx! {
                        button { key: "{code}", r#type: "button", class: "btn",
                            onclick: move |_| {
                                ui::close_modal();
                                let code = pick.clone();
                                let then = then.clone();
                                spawn_forever(async move { set_ui_language(code).await; (then.0)(); });
                            },
                            span { class: "flag", {html::lang_flag(&code)} }
                            " {state::language_native(&code)} "
                            span { class: "muted", {code.to_uppercase()} }
                        }
                    }
                }
            }
        }
        p { class: "muted", {state::tr("Change it any time from the language menu at the top right, or under Config › Settings.")} }
    }
}
