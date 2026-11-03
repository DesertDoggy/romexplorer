//! Config › Mode: standalone or client, the consent dialog a catalogue replacement needs,
//! and the first-run prompt (the console's `showMode` / `applyMode` / `maybeFirstRun`).

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use dioxus::core::spawn_forever;
use dioxus::prelude::*;
use romexplorer_types::sources::{MediaOnReplace, ModeBody, ModeView};
use serde_json::Value;

use crate::api;
use crate::html::badge;
use crate::state;
use crate::ui::{self, Html, Explain};

/// A callback that crosses a dialog boundary, so it must not be a Dioxus `EventHandler`.
///
/// `EventHandler::new` stores its closure in the *creating component's* owner and `call()`
/// panics once that component is gone -- and every one of these is created by a dialog that
/// has closed by the time it fires (the source dialog's 409 resend runs from the consent
/// dialog that replaced it; the first-run prompt's completion runs from a detached task
/// after the prompt closed). A plain `Rc<dyn Fn>` belongs to no scope. `PartialEq` by
/// pointer, which is what a prop needs and what "same callback" means.
#[derive(Clone)]
pub struct Proceed(pub Rc<dyn Fn(MediaOnReplace)>);

impl PartialEq for Proceed {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl Proceed {
    pub fn new(f: impl Fn(MediaOnReplace) + 'static) -> Self {
        Self(Rc::new(f))
    }
}

/// The server's refusal (a 409 with exactly what a catalogue replacement costs) shown
/// verbatim, the art choice, then a native confirm -- the same two-stage shape as removing
/// a directory tree. `on_proceed(media)` resends whatever was refused, with consent.
pub fn consent_dialog(title: String, message: String, on_proceed: Proceed) {
    ui::open_modal(move || rsx! { ConsentDialog { title: title.clone(), message: message.clone(), on_proceed: on_proceed.clone() } });
}

#[component]
fn ConsentDialog(title: String, message: String, on_proceed: Proceed) -> Element {
    let mut media = use_signal(|| MediaOnReplace::Relink);
    rsx! {
        h2 { "{title}" }
        pre { style: "white-space:pre-wrap; font:inherit; max-height:50vh; overflow:auto; margin:6px 0 10px", "{message}" }
        div { class: "kv", style: "grid-template-columns:max-content 1fr",
            div { {state::tr("downloaded art")} }
            div {
                label { input { r#type: "radio", name: "media", checked: media() == MediaOnReplace::Relink, onchange: move |_| media.set(MediaOnReplace::Relink) } {state::tr(" keep the files and re-attach them after the sync (relink)")} }
                br {}
                label { input { r#type: "radio", name: "media", checked: media() == MediaOnReplace::Redownload, onchange: move |_| media.set(MediaOnReplace::Redownload) } {state::tr(" forget them; art comes back on the next provider query (redownload)")} }
            }
        }
        div { class: "fsfoot",
            span { class: "grow" }
            button { r#type: "button", class: "btn", onclick: move |_| ui::close_modal(), {state::tr("cancel")} }
            button { r#type: "button", class: "btn danger",
                onclick: move |_| { let m = media(); let proceed = on_proceed.clone(); spawn(async move {
                    if !ui::confirm(&state::tr("Are you sure? This machine's own catalogue is deleted and replaced by the server's on the next sync. Files, folders and settings stay.")).await { return; }
                    ui::close_modal();
                    (proceed.0)(m);
                }); },
                {state::tr("replace the catalogue")}
            }
        }
    }
}

/// `PUT /api/v1/mode`. The view already says whether the switch is destructive (`losses`),
/// so the consent dialog comes before the request rather than after a refusal; the refusal
/// is still handled, for a catalogue that appeared since the view loaded.
pub fn apply_mode(mode: String, view: ModeView, first_run: bool, done: impl Fn() + Clone + 'static) {
    // `spawn_forever`, not `spawn`, throughout: a request started from a dialog's button must
    // outlive the dialog. `spawn` ties the future to the calling component's scope, and the
    // first-run prompt closes itself before calling this -- with `spawn` the PUT was dropped
    // unsent, the modal vanished, and nothing was recorded.
    let put = {
        let mode = mode.clone();
        move |extra: Option<MediaOnReplace>| {
            let mode = mode.clone();
            let done = done.clone();
            spawn_forever(async move {
                let body = ModeBody { mode: mode.clone(), replace_catalog: extra.is_some(), media: extra.unwrap_or_default() };
                match api::put_as::<ModeBody, Value>("/api/v1/mode", &body).await {
                    Ok(r) => {
                        let cleared = api::u(&r, "catalog_rows_cleared");
                        ui::toast(format!("{}{}", state::trf("now {mode}", &[("mode", &api::s(&r, "mode"))]), if cleared > 0 { state::trf("; {n} catalogue rows cleared — add the main server under Config › Share and sync", &[("n", &cleared.to_string())]) } else { String::new() }), false);
                        done();
                    }
                    Err(e) if e.status == 409 => {
                        let put_again = Proceed::new(move |m: MediaOnReplace| {
                            let mode = mode.clone();
                            let done = done.clone();
                            spawn_forever(async move {
                                match api::put_as::<ModeBody, Value>("/api/v1/mode", &ModeBody { mode, replace_catalog: true, media: m }).await {
                                    Ok(_) => { ui::toast(state::tr("now client — add the main server under Config › Share and sync"), false); done(); }
                                    Err(e) => ui::toast(e.message, true),
                                }
                            });
                        });
                        consent_dialog(state::tr("Switch to client mode?"), e.message, put_again);
                    }
                    Err(e) => ui::toast(e.message, true),
                }
            });
        }
    };
    if !first_run && mode == view.mode {
        ui::toast(state::trf("already {mode}", &[("mode", &mode)]), false);
        return;
    }
    if mode == "client" {
        if let Some(losses) = view.losses.clone() {
            let put = put.clone();
            consent_dialog(state::tr("Switch to client mode?"), losses, Proceed::new(move |m| put(Some(m))));
            return;
        }
    }
    spawn_forever(async move {
        if !first_run {
            let text = if mode == "client" {
                state::tr("Switch to client mode? This machine takes its catalogue from its main server and never builds one itself: DAT imports and provider syncs are refused here.")
            } else {
                state::tr("Switch to standalone mode? This machine builds its own catalogue from now on; syncing from a main server later means replacing it again.")
            };
            if !ui::confirm(&text).await {
                return;
            }
        }
        put(None);
    });
}

/// The two options as radio cards, shared by the screen and the first-run prompt.
#[component]
fn ModeOptions(options: Value, selected: Signal<String>) -> Element {
    let list: Vec<Value> = options.as_array().cloned().unwrap_or_default();
    rsx! {
        div { style: "margin:10px 0",
            for o in list {
                label { class: "card", style: "display:block; cursor:pointer",
                    input { r#type: "radio", name: "mode", value: "{api::s(&o, \"value\")}", checked: selected() == api::s(&o, "value"), onchange: { let v = api::s(&o, "value"); move |_| selected.set(v.clone()) } }
                    " " b { "{api::s(&o, \"label\")}" }
                    div { class: "muted", "{api::s(&o, \"detail\")}" }
                }
            }
        }
    }
}

#[component]
pub fn ConfigMode() -> Element {
    let mut view = use_resource(|| async { api::get_as::<ModeView>("/api/v1/mode").await });
    let mut selected = use_signal(String::new);
    let m = match &*view.read() {
        None => return rsx! { div { class: "muted", {state::tr("loading…")} } },
        Some(Err(e)) => return rsx! { div { class: "card bad", "{e.message}" } },
        Some(Ok(m)) => m.clone(),
    };
    if selected().is_empty() {
        selected.set(m.mode.clone());
    }
    let catalogue = match (&m.catalog_from, m.releases) {
        (Some(from), _) => state::trf("{n} releases, synced from {from}", &[("n", &m.releases.to_string()), ("from", from)]),
        (None, 0) => state::tr("0 releases"),
        (None, n) => state::trf("{n} releases, built here", &[("n", &n.to_string())]),
    };
    let apply_view = m.clone();
    // Over REST this is a server, which is always standalone and refuses the change: say so,
    // as the web console does, rather than offer a button that can only fail.
    if !crate::api::backend::backend().in_process() {
        return rsx! {
            div { class: "card",
                h2 { {state::tr("Instance mode")} }
                div { class: "kv",
                    div { {state::tr("current")} }
                    div { Html { html: badge(&m.mode, "ok") } }
                    div { {state::tr("catalogue")} }
                    div { "{catalogue}" }
                }
                p { class: "muted", {state::tr("A server is always standalone: it builds the catalogue that client machines take from it. Whether a desktop machine is standalone or a client is chosen in the desktop app on that machine, under Config › Mode.")} }
            }
        };
    }
    rsx! {
        div { class: "card",
            h2 { {state::tr("Instance mode")} }
            div { class: "kv",
                div { {state::tr("current")} }
                div { Html { html: badge(&m.mode, if m.mode == "client" { "ok" } else { "warn" }) } " " if !m.chosen { Html { html: badge(&state::tr("never chosen — the default"), "dim") } } }
                div { {state::tr("catalogue")} }
                div { "{catalogue}" }
            }
            ModeOptions { options: m.options.clone(), selected }
            details { summary { {state::tr("changing this later")} } p { class: "muted", style: "white-space:pre-wrap", "{m.switch_warning}" } }
            if let Some(l) = m.losses.clone() { details { summary { {state::tr("what switching to client would replace")} } p { style: "white-space:pre-wrap", "{l}" } } }
            div { class: "toolbar",
                button { class: "btn primary", onclick: move |_| apply_mode(selected(), apply_view.clone(), false, move || { let mut view = view; view.restart() }), {state::tr("apply")} }
            }
        }
    }
}

static FIRST_RUN_CHECKED: AtomicBool = AtomicBool::new(false);

/// Once per run of the app: an installation that was never asked picks its language, then
/// gets the mode choice up front, because changing that later is the expensive path. "later" keeps the default (standalone)
/// and asks again next time; choosing standalone records the choice and stops the asking.
pub async fn maybe_first_run() {
    if FIRST_RUN_CHECKED.swap(true, Ordering::SeqCst) {
        return;
    }
    // The language first, so the mode question is asked in it.
    if super::language::should_ask() {
        super::language::ask(|| { spawn_forever(ask_mode()); });
        return;
    }
    ask_mode().await;
}

async fn ask_mode() {
    let Ok(m) = api::get_as::<ModeView>("/api/v1/mode").await else { return };
    if m.chosen {
        ask_media_reuse().await;
        return;
    }
    ui::open_modal(move || rsx! { FirstRunDialog { view: m.clone() } });
}

/// First run, after the mode: the media folder already holds art (an earlier install's,
/// under a new database). Asked once; the answer is `media_reuse_existing`. The console asks
/// the same (`askMediaReuse`).
async fn ask_media_reuse() {
    if !api::is_admin() {
        return;
    }
    let Ok(v) = api::get("/api/v1/media/reuse_prompt").await else { return };
    if !v.get("ask").and_then(Value::as_bool).unwrap_or(false) {
        return;
    }
    ui::open_pinned_modal(|| rsx! { MediaReuseDialog {} });
}

#[component]
fn MediaReuseDialog() -> Element {
    let answer = |yes: bool| {
        ui::close_modal();
        spawn_forever(async move {
            match api::put("/api/v1/settings/media_reuse_existing", serde_json::json!(yes)).await {
                Ok(_) => state::reload_settings().await,
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    rsx! {
        h2 { {state::tr("Use the media files already in this folder?")} }
        p { {state::tr("The media folder already holds art, probably from an earlier install. Yes: provider syncs take those files as they are and download only what is missing. No: everything is downloaded again; the old files are left where they are.")} }
        p { class: "muted", {state::tr("Change it any time under Config › Settings (media_reuse_existing).")} }
        div { class: "fsfoot",
            span { class: "grow" }
            button { r#type: "button", class: "btn", onclick: move |_| answer(false), {state::tr("No")} }
            button { r#type: "button", class: "btn primary", onclick: move |_| answer(true), {state::tr("Yes (recommended)")} }
        }
    }
}

#[component]
fn FirstRunDialog(view: ModeView) -> Element {
    let selected = use_signal(|| "standalone".to_string());
    let v = view.clone();
    rsx! {
        h2 { {state::tr("How will this machine be used?")} }
        Explain { {state::tr("Chosen once. It decides whether this machine builds its own catalogue or takes one from a main server.")} }
        ModeOptions { options: view.options.clone(), selected }
        details { summary { {state::tr("changing this later")} } p { class: "muted", style: "white-space:pre-wrap", "{view.switch_warning}" } }
        div { class: "fsfoot",
            span { class: "grow" }
            button { r#type: "button", class: "btn", onclick: move |_| { ui::close_modal(); spawn_forever(ask_media_reuse()); }, {state::tr("later")} }
            button { r#type: "button", class: "btn primary",
                onclick: move |_| { ui::close_modal(); apply_mode(selected(), v.clone(), true, || { spawn_forever(ask_media_reuse()); }); },
                {state::tr("choose")}
            }
        }
    }
}
