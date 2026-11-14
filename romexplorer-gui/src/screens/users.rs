//! Signing in, and Config › Users: who may use this machine over the network.
//!
//! The desktop app never signs in -- it runs on this machine's own database. These screens are
//! for an app talking to *another* machine (the web build, mobile), and for the admin managing
//! the accounts other devices sign in with.

use dioxus::core::spawn_forever;
use dioxus::prelude::*;
use romexplorer_types::auth::{Device, Me, PasswordChange, Role, SessionKind, User, UserSpec, MIN_PASSWORD_LEN};
use serde_json::Value;

use super::common::bump;
use crate::api;
use crate::html::{self, badge};
use crate::state;
use crate::ui::{self, Html, Explain};

/// `aa:bb:cc:...`, easier to compare by eye than 64 hex digits.
fn grouped(fingerprint: &str) -> String {
    fingerprint.as_bytes().chunks(2).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join(":")
}

/// Not signed in (or signed out, or the token was revoked): take a username and password.
///
/// On a native build the first connection to a server shows its certificate fingerprint to
/// confirm before anything is sent -- the server's certificate is self-signed, so this is
/// where trust in it comes from, the way SSH asks about a new host.
#[component]
pub fn SignIn(error: String, on_done: EventHandler<()>, #[props(default)] embedded: bool) -> Element {
    let ep = api::endpoint();
    let web = cfg!(target_arch = "wasm32");
    let mut server = use_signal(|| ep.base.clone());
    let mut username = use_signal(String::new);
    let mut password = use_signal(String::new);
    let mut use_setup = use_signal(|| false);
    let mut setup_token = use_signal(String::new);
    let mut pending = use_signal(|| None::<String>);
    let mut message = use_signal(|| None::<String>);
    let mut busy = use_signal(|| false);

    // Connect with `fingerprint` trusted, then sign in (or present the setup token).
    let finish = move |fingerprint: String| {
        spawn(async move {
            busy.set(true);
            let base = if web { String::new() } else { api::normalize_base(&server()) };
            if use_setup() {
                api::connect(&base, &setup_token(), &fingerprint);
                match api::get_as::<Me>("/api/v1/auth/me").await {
                    Ok(me) => {
                        *api::ME.write() = Some(me);
                        *api::NEEDS_SIGN_IN.write() = None;
                        on_done.call(());
                    }
                    Err(e) => message.set(Some(e.message)),
                }
            } else {
                api::connect(&base, "", &fingerprint);
                match api::sign_in(&username(), &password()).await {
                    Ok(_) => on_done.call(()),
                    Err(e) => message.set(Some(e.message)),
                }
            }
            busy.set(false);
        });
    };
    let mut submit = move |_| {
        message.set(None);
        spawn(async move {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let base = api::normalize_base(&server());
                let known = api::endpoint();
                if known.base == base && !known.fingerprint.is_empty() {
                    finish(known.fingerprint);
                    return;
                }
                match api::rest::probe_fingerprint(&base).await {
                    Ok(Some(fp)) => {
                        pending.set(Some(fp));
                        return;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        message.set(Some(e.message));
                        return;
                    }
                }
            }
            finish(String::new());
        });
    };
    let body = rsx! {
        if !error.is_empty() && message().is_none() { div { class: "card warn", p { "{error}" } } }
        if let Some(m) = message() { div { class: "card bad", p { "{m}" } } }
        if let Some(fp) = pending() {
            div { class: "card",
                h2 { {state::tr("Trust this server?")} }
                p { {state::tr("This is the first time this app has connected to ")} b { "{api::normalize_base(&server())}" } {state::tr(". Its certificate's fingerprint is:")} }
                p { class: "mono", style: "word-break:break-all", "{grouped(&fp)}" }
                Explain { {state::tr("Compare it with the one the server shows — in its log when it starts, or under Config › Server. If they match, trust it: from then on this app only talks to that server with that certificate.")} }
                div { class: "toolbar",
                    button { class: "btn primary", disabled: busy(), onclick: move |_| { let fp = fp.clone(); pending.set(None); finish(fp); }, {state::tr("trust and sign in")} }
                    button { class: "btn", onclick: move |_| pending.set(None), {state::tr("cancel")} }
                }
            }
        } else {
            if !web {
                NetworkServers { on_pick: move |base: String| { server.set(base); message.set(None); } }
            }
            div { class: "card",
                h2 { {state::tr("Sign in")} }
                form { class: "kv", style: "grid-template-columns:120px 1fr",
                    onsubmit: move |e| { e.prevent_default(); submit(()); },
                    if !web {
                        div { {state::tr("server")} }
                        div {
                            input { r#type: "text", style: "width:100%", placeholder: "192.168.1.10:7337", value: "{server}", oninput: move |e| server.set(e.value()) }
                            div { class: "muted", {state::tr("The romexplorer server's address (its port is 7337 unless changed).")} }
                        }
                    }
                    if use_setup() {
                        div { {state::tr("setup token")} }
                        div {
                            input { r#type: "password", style: "width:100%", autocomplete: "off", value: "{setup_token}", oninput: move |e| setup_token.set(e.value()) }
                            div { class: "muted", {state::tr("The contents of data/server_token on the server. It works only until an admin account exists — use it to create one.")} }
                        }
                    } else {
                        div { {state::tr("username")} }
                        div { input { r#type: "text", style: "width:100%", autocomplete: "username", value: "{username}", oninput: move |e| username.set(e.value()) } }
                        div { {state::tr("password")} }
                        div { input { r#type: "password", style: "width:100%", autocomplete: "current-password", value: "{password}", oninput: move |e| password.set(e.value()) } }
                    }
                    div {}
                    div {
                        button { class: "btn primary", disabled: busy(), if use_setup() { {state::tr("continue")} } else { {state::tr("sign in")} } }
                        " "
                        span { class: "link", onclick: move |_| use_setup.set(!use_setup()),
                            if use_setup() { {state::tr("sign in with an account instead")} } else { {state::tr("a new server? use the setup token")} }
                        }
                    }
                }
            }
        }
    };
    if embedded {
        return body;
    }
    rsx! {
        header { h1 { "romexplorer" } }
        main { id: "view", {body} }
    }
}

/// The romexplorer servers that answered a search of this network: one tap puts the address
/// in the sign-in form, which then asks for the account (or the setup token). Shares are left
/// out: they are for another romexplorer to mirror, not to sign in to.
#[component]
fn NetworkServers(on_pick: EventHandler<String>) -> Element {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut found = use_resource(|| async { api::discovery::search().await });
        let list: Option<Vec<romexplorer_types::discovery::Found>> = found.read().as_ref().map(|f| f.iter().filter(|f| f.announce.kind == "server").cloned().collect());
        rsx! {
            div { class: "card",
                h2 { {state::tr("On this network")} }
                match list {
                    None => rsx! { p { class: "muted", {state::tr("searching…")} } },
                    Some(l) if l.is_empty() => rsx! { p { class: "muted", {state::tr("No server answered. Type its address below.")} } },
                    Some(l) => rsx! {
                        div { class: "toolbar", style: "flex-wrap:wrap",
                            for f in l {
                                button { key: "{f.base()}", class: "btn", onclick: { let b = f.base(); move |_| on_pick.call(b.clone()) },
                                    b { "{f.announce.name}" } " " span { class: "muted", "{f.ip}:{f.announce.port}" }
                                }
                            }
                        }
                    },
                }
                button { class: "btn", r#type: "button", onclick: move |_| found.restart(), {state::tr("search again")} }
            }
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = on_pick;
        rsx! {}
    }
}

#[component]
pub fn ConfigUsers() -> Element {
    let reload = use_signal(|| 0u64);
    let me = use_resource(move || async move {
        let _ = reload();
        api::get_as::<Me>("/api/v1/auth/me").await
    });
    let me: Option<Me> = me.read().as_ref().and_then(|r| r.as_ref().ok()).cloned();
    let Some(me) = me else { return rsx! { div { class: "muted", {state::tr("loading…")} } } };
    let admin = me.admin;
    rsx! {
        SessionCard { me: me.clone(), reload }
        if admin { AccountsCard { me: me.clone(), reload } }
    }
}

/// Who this app is signed in as, and (for an account) its own password and devices.
#[component]
fn SessionCard(me: Me, reload: Signal<u64>) -> Element {
    match me.kind {
        SessionKind::Local => rsx! {
            div { class: "card",
                h2 { {state::tr("This app")} }
                Explain {
                    {state::tr("This app runs on this machine's own database, so it never signs in. The accounts below are for other devices that reach this machine over the network — the web app, the phone app, or another machine syncing from it.")}
                }
            }
        },
        SessionKind::Setup => rsx! {
            div { class: "card warn",
                h2 { {state::tr("Signed in with the setup token")} }
                p { {state::tr("Create an admin account below. The moment one exists, the setup token stops working and its file is deleted — this app then signs in with the new account.")} }
            }
        },
        SessionKind::User => {
            let user = me.user.clone();
            let name = user.as_ref().map(|u| u.username.clone()).unwrap_or_default();
            let role = user.as_ref().map(|u| u.role).unwrap_or_default();
            let upload = user.as_ref().is_some_and(|u| u.can_upload);
            rsx! {
                div { class: "card",
                    h2 { {state::tr("Your account")} }
                    p {
                        b { "{name}" } " "
                        Html { html: badge(role.name(), if role == Role::Admin { "ok" } else { "dim" }) }
                        if upload && role != Role::Admin { " " Html { html: badge(&state::tr("upload"), "ok") } }
                    }
                    ChangePassword {}
                    DeviceList { path: "/api/v1/auth/devices".to_string(), reload }
                }
            }
        }
    }
}

#[component]
fn ChangePassword() -> Element {
    let mut current = use_signal(String::new);
    let mut new = use_signal(String::new);
    let mut again = use_signal(String::new);
    let submit = move |_| {
        if new() != again() {
            ui::toast(state::tr("the new passwords differ"), true);
            return;
        }
        let change = PasswordChange { current: current(), new: new() };
        spawn(async move {
            match api::put_as::<PasswordChange, Value>("/api/v1/auth/password", &change).await {
                Ok(_) => {
                    ui::toast(state::tr("password changed — your other devices are signed out"), false);
                    current.set(String::new());
                    new.set(String::new());
                    again.set(String::new());
                }
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    rsx! {
        details {
            summary { {state::tr("change password")} }
            form { class: "kv", style: "grid-template-columns:140px 1fr", onsubmit: move |e| { e.prevent_default(); submit(()); },
                div { {state::tr("current")} } div { input { r#type: "password", autocomplete: "current-password", value: "{current}", oninput: move |e| current.set(e.value()) } }
                div { {state::tr("new")} } div { input { r#type: "password", autocomplete: "new-password", value: "{new}", oninput: move |e| new.set(e.value()) } }
                div { {state::tr("again")} } div { input { r#type: "password", autocomplete: "new-password", value: "{again}", oninput: move |e| again.set(e.value()) } }
                div {} div { button { class: "btn primary", {state::tr("change")} } }
            }
        }
    }
}

/// Signed-in devices under `path` (one's own, or an account's for an admin), each revocable.
#[component]
fn DeviceList(path: String, reload: Signal<u64>) -> Element {
    let list_path = path.clone();
    let devices = use_resource(move || {
        let p = list_path.clone();
        async move {
            let _ = reload();
            api::get_as::<Vec<Device>>(&p).await
        }
    });
    let list: Vec<Device> = match &*devices.read() {
        None => return rsx! { div { class: "muted", {state::tr("loading devices…")} } },
        Some(Err(e)) => return rsx! { div { class: "bad", "{e.message}" } },
        Some(Ok(l)) => l.clone(),
    };
    rsx! {
        h3 { {state::tr("Signed-in devices")} }
        table { class: "grid",
            thead { tr { th { {state::tr("device")} } th { {state::tr("signed in")} } th { {state::tr("last used")} } th {} } }
            tbody {
                if list.is_empty() { tr { td { colspan: 4, class: "muted", {state::tr("none")} } } }
                for d in list {
                    tr { key: "{d.id}",
                        td { "{d.device}" if d.current { " " Html { html: badge(&state::tr("this one"), "ok") } } }
                        td { {d.created_at.map(|t| html::fmt_date(&t.to_string())).unwrap_or_default()} }
                        td { {d.last_used_at.map(|t| html::fmt_date(&t.to_string())).unwrap_or_default()} }
                        td {
                            span { class: "link",
                                onclick: {
                                    let url = format!("{path}/{}", d.id);
                                    move |_| {
                                        let url = url.clone();
                                        spawn(async move {
                                            if !ui::confirm(&state::tr("Sign this device out? It will have to sign in again.")).await { return; }
                                            match api::delete(&url).await {
                                                Ok(_) => { ui::toast(state::tr("signed out"), false); bump(reload); }
                                                Err(e) => ui::toast(e.message, true),
                                            }
                                        });
                                    }
                                },
                                {state::tr("sign out")}
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn AccountsCard(me: Me, reload: Signal<u64>) -> Element {
    let users = use_resource(move || async move {
        let _ = reload();
        api::get_as::<Vec<User>>("/api/v1/users").await
    });
    let list: Vec<User> = match &*users.read() {
        None => return rsx! { div { class: "muted", {state::tr("loading…")} } },
        Some(Err(e)) => return rsx! { div { class: "card bad", "{e.message}" } },
        Some(Ok(l)) => l.clone(),
    };
    let setup = me.kind == SessionKind::Setup;
    rsx! {
        div { class: "card",
            h2 { {state::tr("Accounts")} }
            Explain {
                {state::tr("Who may use this machine over the network. An ")} b { {state::tr("admin")} } {state::tr(" can do everything. A ")} b { {state::tr("user")} }
                {state::tr(" can browse, sync, and fetch the media this server already has; with ")} b { {state::tr("upload")} }
                {state::tr(" they can also register new files they copied onto the server's share — never change what is already there. Passwords are stored only as hashes, and backups never carry them.")}
            }
            div { class: "toolbar",
                button { class: "btn primary", onclick: move |_| user_dialog(None, setup, EventHandler::new(move |_| bump(reload))), if setup && list.is_empty() { {state::tr("create the admin account")} } else { {state::tr("add account")} } }
            }
            table { class: "grid",
                thead { tr { th { {state::tr("username")} } th { {state::tr("role")} } th { {state::tr("upload")} } th { {state::tr("enabled")} } th { {state::tr("password")} } th { {state::tr("devices")} } th { {state::tr("last sign-in")} } th {} } }
                tbody {
                    if list.is_empty() { tr { td { colspan: 8, class: "muted", {state::tr("no accounts yet")} } } }
                    for u in list {
                        UserRow { key: "{u.id}", user: u.clone(), reload }
                    }
                }
            }
        }
    }
}

#[component]
fn UserRow(user: User, reload: Signal<u64>) -> Element {
    let id = user.id;
    let (edit, name) = (user.clone(), user.username.clone());
    rsx! {
        tr {
            td { "{user.username}" }
            td { Html { html: badge(user.role.name(), if user.role == Role::Admin { "ok" } else { "dim" }) } }
            td { Html { html: if user.can_upload || user.role == Role::Admin { badge(&state::tr("yes"), "ok") } else { badge(&state::tr("no"), "dim") } } }
            td { Html { html: if user.enabled { badge(&state::tr("yes"), "ok") } else { badge(&state::tr("no"), "dim") } } }
            td { Html { html: if user.has_password { badge(&state::tr("set"), "ok") } else { badge(&state::tr("needs one"), "bad") } } }
            td { class: "num", "{user.devices}" }
            td { {user.last_login_at.map(|t| html::fmt_date(&t.to_string())).unwrap_or_default()} }
            td { style: "white-space:nowrap",
                span { class: "link", onclick: move |_| user_dialog(Some(edit.clone()), false, EventHandler::new(move |_| bump(reload))), {state::tr("edit")} }
                " "
                span { class: "link", onclick: move |_| ui::open_modal(move || rsx! { h3 { {state::tr("Devices")} } DeviceList { path: format!("/api/v1/users/{id}/devices"), reload } div { class: "fsfoot", span { class: "grow" } button { class: "btn", onclick: move |_| ui::close_modal(), {state::tr("close")} } } }), {state::tr("devices")} }
                " "
                span { class: "link",
                    onclick: move |_| { let name = name.clone(); spawn(async move {
                        if !ui::confirm(&state::trf("Delete the account {name}? Its devices are signed out.", &[("name", &name)])).await { return; }
                        match api::delete(&format!("/api/v1/users/{id}")).await {
                            Ok(_) => { ui::toast(state::trc("done", "deleted"), false); bump(reload); }
                            Err(e) => ui::toast(e.message, true),
                        }
                    }); },
                    {state::tr("delete")}
                }
            }
        }
    }
}

/// Add (`None`) or edit an account. `setup`: this app is on the setup token, so creating an
/// admin also signs it in as that admin -- the setup token stops working the moment one exists.
fn user_dialog(existing: Option<User>, setup: bool, on_done: EventHandler<()>) {
    ui::open_modal(move || rsx! { UserDialog { existing: existing.clone(), setup, on_done } });
}

#[component]
fn UserDialog(existing: Option<User>, setup: bool, on_done: EventHandler<()>) -> Element {
    let id = existing.as_ref().map(|u| u.id);
    let mut username = use_signal(|| existing.as_ref().map(|u| u.username.clone()).unwrap_or_default());
    let mut password = use_signal(String::new);
    let mut role = use_signal(|| existing.as_ref().map(|u| u.role).unwrap_or(if setup { Role::Admin } else { Role::User }));
    let mut can_upload = use_signal(|| existing.as_ref().is_some_and(|u| u.can_upload));
    let mut enabled = use_signal(|| existing.as_ref().is_none_or(|u| u.enabled));
    let submit = move |_| {
        let pw = password();
        if id.is_none() && pw.chars().count() < MIN_PASSWORD_LEN {
            ui::toast(state::trf("a password needs at least {n} characters", &[("n", &MIN_PASSWORD_LEN.to_string())]), true);
            return;
        }
        let spec = UserSpec { username: username().trim().to_string(), password: Some(pw.clone()).filter(|p| !p.is_empty()), role: role(), can_upload: can_upload(), enabled: enabled() };
        spawn_forever(async move {
            let result = match id {
                Some(id) => api::put_as::<UserSpec, User>(&format!("/api/v1/users/{id}"), &spec).await,
                None => api::post_as::<UserSpec, User>("/api/v1/users", &spec).await,
            };
            match result {
                Ok(_) => {
                    ui::close_modal();
                    if setup && id.is_none() && spec.role == Role::Admin {
                        match api::sign_in(&spec.username, &pw).await {
                            Ok(_) => ui::toast(state::tr("admin account created — signed in with it; the setup token is retired"), false),
                            Err(e) => ui::toast(state::trf("account created, but signing in failed: {error}", &[("error", &e.message)]), true),
                        }
                    } else {
                        ui::toast(state::tr("saved"), false);
                    }
                    on_done.call(());
                }
                Err(e) => ui::toast(e.message, true),
            }
        });
    };
    rsx! {
        h3 { if id.is_some() { {state::tr("Edit account")} } else { {state::tr("Add account")} } }
        form { class: "kv", style: "grid-template-columns:110px 1fr", onsubmit: move |e| { e.prevent_default(); submit(()); },
            div { {state::tr("username")} } div { input { value: "{username}", required: true, style: "width:100%", autocomplete: "off", oninput: move |e| username.set(e.value()) } }
            div { {state::tr("password")} } div {
                input { r#type: "password", value: "{password}", style: "width:100%", autocomplete: "new-password",
                    placeholder: if id.is_some() { state::tr("leave blank to keep it") } else { String::new() },
                    oninput: move |e| password.set(e.value()) }
                div { class: "muted", {state::trf("At least {n} characters. Setting a new one signs the account's devices out.", &[("n", &MIN_PASSWORD_LEN.to_string())])} }
            }
            div { {state::tr("role")} } div {
                label { input { r#type: "radio", name: "role", checked: role() == Role::User, onchange: move |_| role.set(Role::User) } {state::tr(" user — browse, sync, fetch cached media")} }
                br {}
                label { input { r#type: "radio", name: "role", checked: role() == Role::Admin, onchange: move |_| role.set(Role::Admin) } {state::tr(" admin — everything")} }
            }
            div { {state::tr("upload")} } div { label { input { r#type: "checkbox", checked: can_upload(), disabled: role() == Role::Admin, onchange: move |e| can_upload.set(e.checked()) } {state::tr(" may register new files it copies onto the server's share")} } }
            div { {state::tr("enabled")} } div { label { input { r#type: "checkbox", checked: enabled(), onchange: move |e| enabled.set(e.checked()) } {state::tr(" enabled")} } }
            div {} div {
                button { class: "btn primary", {state::tr("save")} }
                " "
                button { class: "btn", r#type: "button", onclick: move |_| ui::close_modal(), {state::tr("cancel")} }
            }
        }
    }
}
