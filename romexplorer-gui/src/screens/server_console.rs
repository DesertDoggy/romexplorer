//! The Server console tab: a client machine's way into its main server's own screens.
//!
//! A client takes its catalogue from a main server and keeps its own database, so nothing
//! else in this app shows the *server's* jobs, accounts or settings. This tab does, the way a
//! browser pointed at the server would: one button, a sign-in with an account on that server,
//! and then the server's console itself, inside the tab.
//! The same console can also be opened in the system browser from here; the two are offered
//! side by side.
//!
//! The page is not loaded from the server by the webview. It is served to the webview by this
//! process (`api::assets`), which forwards each of its requests over the connection the
//! source was added with -- the only way past a self-signed certificate that an embedded
//! browser will not accept, and the reason the account's token never reaches the page.
//!
//! The button is greyed out, with the reason beside it, where it cannot lead anywhere: a
//! standalone machine is its own server (its screens are this app's), and a client with no
//! main server set has nothing to sign in to.

use dioxus::prelude::*;
use serde_json::Value;

use crate::api::{self, s};
use crate::state;
use crate::ui;
use crate::Route;

#[component]
pub fn ServerConsole() -> Element {
    let data = use_resource(|| async {
        let mode = api::get("/api/v1/mode").await?;
        let sources = api::get("/api/v1/sources").await?;
        Ok::<_, api::ApiError>((mode, sources))
    });
    // Bumped when the session starts or ends, so the tab redraws around it.
    let mut session_gen = use_signal(|| 0u64);
    let (mode, sources) = match &*data.read() {
        None => return rsx! { div { class: "muted", {state::tr("loading…")} } },
        Some(Err(e)) => return rsx! { div { class: "card bad", "{e.message}" } },
        Some(Ok(v)) => v.clone(),
    };
    let _ = session_gen();
    let client = s(&mode, "mode") == "client";
    let main: Option<Value> = api::items(&sources).into_iter().find(|x| s(x, "role") == "main" && api::b(x, "enabled"));
    let label = state::tr("log in to the server console");
    // Plain strings up front: the click handler takes one, and the row shows both.
    let main: Option<(String, String)> = main.map(|m| (s(&m, "name"), s(&m, "base_url")));

    #[cfg(feature = "local")]
    if let Some(session) = api::assets::console_session() {
        let who = state::trf("signed in to {server} as {user}", &[("server", &session.name), ("user", &session.username)]);
        return rsx! {
            div { class: "toolbar",
                span { class: "muted", "{who} — " span { class: "mono", "{session.base}" } }
                span { class: "grow" }
                a { class: "btn small", href: "{session.base}/", target: "_blank", rel: "noopener", {state::tr("open in browser")} }
                button { class: "btn small", r#type: "button",
                    onclick: move |_| {
                        let session = session.clone();
                        spawn(async move {
                            session.logout().await;
                            api::assets::set_console_session(None);
                            *session_gen.write() += 1;
                        });
                    },
                    {state::tr("leave the server console")}
                }
            }

            iframe { src: "/rxserver/", style: "width:100%; height:calc(100vh - 150px); border:1px solid var(--line, #333); border-radius:6px; background:#fff" }
        };
    }

    rsx! {
        div { class: "card",
            h2 { {state::tr("Server console")} }
            p { class: "muted", {state::tr("The main server's own screens — its jobs, accounts and settings — shown here once you sign in with an account on that server. The password is used for that sign-in only and is not kept.")} }
            if !client {
                div { class: "toolbar",
                    button { class: "btn primary", r#type: "button", disabled: true, "{label}" }
                    span { class: "muted", {state::tr("Only in client mode. This machine is standalone, so it is its own server: its screens are the ones in this app.")} }
                }
            } else if let Some((name, base)) = main {
                div { class: "toolbar",
                    button { class: "btn primary", r#type: "button",
                        onclick: {
                            let name = name.clone();
                            move |_| {
                            let name = name.clone();
                            ui::open_modal(move || rsx! { SignIn { server: name.clone(), on_done: move |_| *session_gen.write() += 1 } });
                            }
                        },
                        "{label}"
                    }
                    // The same console in the system browser, where the server's own
                    // sign-in page asks for the account. Offered beside the in-app way,
                    // not instead of it.
                    a { class: "btn", href: "{base}/", target: "_blank", rel: "noopener", {state::tr("open in browser")} }
                    span { class: "muted", "{name} — " span { class: "mono", "{base}" } }
                }
            } else {
                div { class: "toolbar",
                    button { class: "btn primary", r#type: "button", disabled: true, "{label}" }
                    span { class: "muted", {state::tr("No main server is set. ")} }
                    Link { to: Route::ConfigSources {}, class: "link", {state::tr("Add one under Config › Share")} }
                }
            }
        }
    }
}

/// The credentials dialog: an account on the server, not on this machine.
#[component]
fn SignIn(server: String, on_done: EventHandler<()>) -> Element {
    let mut username = use_signal(String::new);
    let mut password = use_signal(String::new);
    let mut error = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let heading = state::trf("Sign in to {server}", &[("server", &server)]);
    rsx! {
        h3 { "{heading}" }
        form { class: "kv", style: "grid-template-columns:110px 1fr",
            onsubmit: move |e| {
                e.prevent_default();
                busy.set(true);
                error.set(String::new());
                let (user, pass) = (username().trim().to_string(), password());
                // `spawn_forever`: the dialog closes itself on success, and the task must
                // outlive it to report back.
                dioxus::core::spawn_forever(async move {
                    match sign_in(&user, &pass).await {
                        Ok(()) => {
                            ui::close_modal();
                            on_done.call(());
                        }
                        Err(message) => {
                            error.set(message);
                            busy.set(false);
                        }
                    }
                });
            },
            div { {state::tr("username")} } div { input { autocomplete: "username", style: "width:100%", value: "{username}", oninput: move |e| username.set(e.value()) } }
            div { {state::tr("password")} } div { input { r#type: "password", autocomplete: "current-password", style: "width:100%", value: "{password}", oninput: move |e| password.set(e.value()) } }
            div {} div {
                button { class: "btn primary", disabled: busy(), {state::tr("sign in")} }
                " "
                button { class: "btn", r#type: "button", onclick: move |_| ui::close_modal(), {state::tr("cancel")} }
            }
        }
        if !error().is_empty() { div { class: "bad", "{error}" } }
    }
}

#[cfg(feature = "local")]
async fn sign_in(username: &str, password: &str) -> Result<(), String> {
    let state = api::local::state().ok_or_else(|| "no in-process backend".to_string())?;
    let session = romexplorer::api::sources::console_login(&state.app, username, password).await.map_err(|e| e.message)?;
    api::assets::set_console_session(Some(session));
    Ok(())
}

/// A build with no database of its own is already a client of one server and shows that
/// server's screens everywhere; it has no second server to open a console on.
#[cfg(not(feature = "local"))]
async fn sign_in(_username: &str, _password: &str) -> Result<(), String> {
    Err("the server console is only available in the desktop app".to_string())
}
