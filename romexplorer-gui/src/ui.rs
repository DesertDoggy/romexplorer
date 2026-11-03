//! Shared UI services, the console's globals: toasts, the right-click menu, the modal
//! dialog, the hover popover, and raw-HTML fragments (from `html`).

use std::rc::Rc;

use dioxus::prelude::*;

pub async fn sleep(ms: u64) {
    futures_timer::Delay::new(std::time::Duration::from_millis(ms)).await;
}

// --- raw HTML fragments ---

/// A `<span>` holding display-only markup from `html::*` (badges, flags, hashes).
#[component]
pub fn Html(html: String, #[props(default)] class: String) -> Element {
    rsx! { span { class: "{class}", dangerous_inner_html: "{html}" } }
}

/// A `<div>` holding display-only markup.
#[component]
pub fn HtmlDiv(html: String, #[props(default)] class: String) -> Element {
    rsx! { div { class: "{class}", dangerous_inner_html: "{html}" } }
}

// --- "!" explanations ---

/// A round "!" button holding a long explanation, shown in a popover at the button when
/// pressed and closed by pressing it again or clicking anywhere else. Its children are the
/// explanation, links and bold text included -- it replaces a grey paragraph that used to
/// sit in the page. Placed right after a card's heading, CSS sets it beside the heading.
/// An [`Explain`] whose text is in the language files (`explain.*`), by key: each key's
/// text in turn, a paragraph per blank-line-separated block. Nothing when none has text.
#[component]
pub fn KeyExplain(keys: Vec<String>) -> Element {
    let paragraphs: Vec<String> = keys.iter().map(|k| crate::state::t(k, "")).filter(|t| !t.is_empty()).flat_map(|t| t.split("\n\n").map(|p| p.trim().to_string()).collect::<Vec<_>>()).filter(|p| !p.is_empty()).collect();
    if paragraphs.is_empty() {
        return rsx! {};
    }
    rsx! { Explain { for p in paragraphs { p { "{p}" } } } }
}

/// [`KeyExplain`] for one key.
pub fn key_explain(key: &str) -> Element {
    rsx! { KeyExplain { keys: vec![key.to_string()] } }
}

#[component]
pub fn Explain(children: Element) -> Element {
    let mut open = use_signal(|| None::<(f64, f64)>);
    rsx! {
        span { class: "explain",
            button { class: if open().is_some() { "info on" } else { "info" }, r#type: "button", "aria-label": crate::state::tr("explain"),
                onclick: move |e: MouseEvent| {
                    e.stop_propagation();
                    e.prevent_default();
                    if open().is_some() {
                        open.set(None);
                    } else {
                        let p = e.client_coordinates();
                        open.set(Some((p.x, p.y)));
                    }
                },
                "!"
            }
            if let Some((x, y)) = open() {
                // A transparent layer under the popover: a click anywhere else closes it.
                div { class: "infoveil", onclick: move |e| { e.stop_propagation(); open.set(None); } }
                // Left of the pointer, kept inside the window; below it, or as far down as
                // still leaves the popover room.
                div { class: "infopop",
                    style: "left:max(8px, min({x - 12.0}px, calc(100vw - 460px)));top:min({y + 14.0}px, calc(100vh - 240px))",
                    onclick: move |e| e.stop_propagation(),
                    {children}
                }
            }
        }
    }
}

// --- toasts ---

#[derive(Clone, PartialEq)]
pub struct Toast {
    id: u64,
    text: String,
    bad: bool,
}

static TOASTS: GlobalSignal<Vec<Toast>> = Signal::global(Vec::new);
static NEXT_TOAST: GlobalSignal<u64> = Signal::global(|| 0);

pub fn toast(text: impl Into<String>, bad: bool) {
    // A job button that refused (nothing picked, a server error) started nothing: a queue
    // button's request must not wait for some later job.
    if bad {
        crate::api::queue_next(false);
    }
    let id = {
        let mut n = NEXT_TOAST.write();
        *n += 1;
        *n
    };
    TOASTS.write().push(Toast { id, text: text.into(), bad });
    // `spawn_forever`, not `spawn`: the timer otherwise belongs to whichever component raised
    // the toast, and dies with it -- a dialog that closes or a list that reloads on success
    // left its toast on screen for good.
    dioxus::core::spawn_forever(async move {
        sleep(if bad { 7000 } else { 3500 }).await;
        TOASTS.write().retain(|t| t.id != id);
    });
}

#[component]
pub fn ToastHost() -> Element {
    let list = TOASTS.read().clone();
    let style = if list.is_empty() { "display:none" } else { "display:block" };
    rsx! {
        div { id: "toast", style: "{style}",
            for t in list {
                div { key: "{t.id}", class: if t.bad { "bad" } else { "" }, "{t.text}" }
            }
        }
    }
}

// --- right-click menu ---

#[derive(Clone)]
pub struct MenuItem {
    pub label: String,
    pub head: bool,
    pub action: Option<Rc<dyn Fn()>>,
    /// A fold: clicking it shows these indented under it, and clicking again hides them; the
    /// menu stays open.
    pub children: Vec<MenuItem>,
}

impl MenuItem {
    pub fn head(label: impl Into<String>) -> Self {
        Self { label: label.into(), head: true, action: None, children: Vec::new() }
    }
    pub fn new(label: impl Into<String>, action: impl Fn() + 'static) -> Self {
        Self { label: label.into(), head: false, action: Some(Rc::new(action)), children: Vec::new() }
    }
    pub fn fold(label: impl Into<String>, children: Vec<MenuItem>) -> Self {
        Self { label: label.into(), head: false, action: None, children }
    }
}

#[derive(Clone)]
struct MenuState {
    x: f64,
    y: f64,
    items: Vec<MenuItem>,
    /// Which folds are open, by index in `items`.
    open: Vec<usize>,
}

static MENU: GlobalSignal<Option<MenuState>> = Signal::global(|| None);

pub fn open_menu(x: f64, y: f64, items: Vec<MenuItem>) {
    if items.is_empty() {
        return;
    }
    *MENU.write() = Some(MenuState { x, y, items, open: Vec::new() });
}

pub fn close_menu() {
    if MENU.read().is_some() {
        *MENU.write() = None;
    }
}

fn toggle_fold(i: usize) {
    if let Some(m) = MENU.write().as_mut() {
        match m.open.iter().position(|&o| o == i) {
            Some(p) => {
                m.open.remove(p);
            }
            None => m.open.push(i),
        }
    }
}

#[component]
pub fn MenuHost() -> Element {
    let Some(m) = MENU.read().clone() else { return rsx! { div { id: "ctxmenu", style: "display:none" } } };
    // An open fold can make the menu taller than the room below it: it scrolls instead.
    let max_h = format!("calc(100vh - {}px - 8px)", m.y.max(0.0));
    rsx! {
        div { id: "ctxmenu", style: "display:block;left:{m.x}px;top:{m.y}px;max-height:{max_h}",
            onclick: move |e| e.stop_propagation(),
            for (i, it) in m.items.into_iter().enumerate() {
                if it.head {
                    div { key: "{i}", class: "head", "{it.label}" }
                } else if !it.children.is_empty() {
                    div { key: "{i}", class: "fold",
                        onclick: move |_| toggle_fold(i),
                        {format!("{} {}", if m.open.contains(&i) { "▾" } else { "▸" }, it.label)}
                    }
                    if m.open.contains(&i) {
                        for (j, c) in it.children.into_iter().enumerate() {
                            div { key: "{i}-{j}", class: "sub",
                                onclick: move |_| {
                                    close_menu();
                                    if let Some(a) = &c.action { a(); }
                                },
                                "{c.label}"
                            }
                        }
                    }
                } else {
                    div { key: "{i}",
                        onclick: move |_| {
                            close_menu();
                            if let Some(a) = &it.action { a(); }
                        },
                        "{it.label}"
                    }
                }
            }
        }
    }
}

// --- modal ---

pub type Render = Rc<dyn Fn() -> Element>;

static MODAL: GlobalSignal<Option<Render>> = Signal::global(|| None);
/// A popup over the popup: what a dialog opens for itself (the folder browser of a path field
/// in the copy dialog). Opening it with `open_modal` would replace the dialog -- dropping its
/// state, which the popup then writes back into: a crash, not just a lost form.
static SUB_MODAL: GlobalSignal<Option<Render>> = Signal::global(|| None);

/// A popup only its own buttons close: a click beside it does nothing (the first-start
/// language picker, which has to be answered).
static MODAL_PINNED: GlobalSignal<bool> = Signal::global(|| false);

pub fn open_modal(render: impl Fn() -> Element + 'static) {
    *SUB_MODAL.write() = None;
    *MODAL_PINNED.write() = false;
    *MODAL.write() = Some(Rc::new(render));
}

/// `open_modal` for a popup that must be answered: only a button inside it closes it.
pub fn open_pinned_modal(render: impl Fn() -> Element + 'static) {
    open_modal(render);
    *MODAL_PINNED.write() = true;
}

pub fn close_modal() {
    *SUB_MODAL.write() = None;
    *MODAL_PINNED.write() = false;
    *MODAL.write() = None;
}

/// Opens `render` above the popup already showing, or as the popup when none is.
pub fn open_over_modal(render: impl Fn() -> Element + 'static) {
    if modal_is_open() {
        *SUB_MODAL.write() = Some(Rc::new(render));
    } else {
        open_modal(render);
    }
}

/// Closes the topmost popup: the one over a dialog when there is one, else the dialog.
pub fn close_top_modal() {
    if SUB_MODAL.peek().is_some() {
        *SUB_MODAL.write() = None;
    } else {
        close_modal();
    }
}

pub fn modal_is_open() -> bool {
    MODAL.peek().is_some()
}

#[component]
pub fn ModalHost() -> Element {
    let Some(render) = MODAL.read().clone() else { return rsx! { div { id: "modal", style: "display:none" } } };
    let sub = SUB_MODAL.read().clone();
    rsx! {
        div { id: "modal", style: "display:flex",
            onclick: move |_| if !*MODAL_PINNED.peek() { close_modal() },
            div { class: "box", onclick: move |e| e.stop_propagation(), {render()} }
        }
        if let Some(sub) = sub {
            div { id: "submodal", style: "display:flex",
                onclick: move |_| close_top_modal(),
                div { class: "box", onclick: move |e| e.stop_propagation(), {sub()} }
            }
        }
    }
}

// --- hover popover ---

static POPOVER: GlobalSignal<Option<(f64, f64, String)>> = Signal::global(|| None);

pub fn show_popover(x: f64, y: f64, html: String) {
    *POPOVER.write() = Some((x, y, html));
}

pub fn hide_popover() {
    if POPOVER.read().is_some() {
        *POPOVER.write() = None;
    }
}

#[component]
pub fn PopoverHost() -> Element {
    let Some((x, y, html)) = POPOVER.read().clone() else { return rsx! { div { id: "popover", style: "display:none" } } };
    rsx! { div { id: "popover", style: "display:block;left:{x + 14.0}px;top:{y + 14.0}px", dangerous_inner_html: "{html}" } }
}

/// The question asked before a lossy output (`OutputFormat::is_lossy`) is chosen, with a
/// "don't ask again" box. `on_answer(yes, dont_ask)`; closing the popup is a no. The web
/// console's `askLossy` is the same dialog.
pub fn ask_lossy(format: &str, on_answer: impl Fn(bool, bool) + 'static) {
    let format = format.to_string();
    let on_answer = Rc::new(on_answer);
    open_modal(move || {
        let on_answer = on_answer.clone();
        rsx! { LossyAsk { format: format.clone(), on_answer: move |(yes, dont): (bool, bool)| on_answer(yes, dont) } }
    });
}

#[component]
fn LossyAsk(format: String, on_answer: EventHandler<(bool, bool)>) -> Element {
    let mut dont = use_signal(|| false);
    rsx! {
        h3 { {crate::state::tr("Lossy format")} }
        p { {crate::state::trf("{format} keeps the game's files but not the disc, so it can't be turned back into the original image. It is not recommended for bit-perfect archiving. Use it anyway?", &[("format", &format)])} }
        label { style: "display:block; margin:8px 0",
            input { r#type: "checkbox", checked: dont(), onchange: move |e| dont.set(e.checked()) }
            " " {crate::state::tr("Don't ask again")}
        }
        div { class: "row",
            button { class: "btn", onclick: move |_| { close_modal(); on_answer.call((true, dont())); }, {crate::state::tr("Yes")} }
            button { class: "btn", onclick: move |_| { close_modal(); on_answer.call((false, false)); }, {crate::state::tr("No")} }
        }
    }
}

/// `confirm()` and `prompt()` as the browser offers them (the webview has both).
pub async fn confirm(text: &str) -> bool {
    let js = format!("return confirm({});", serde_json::to_string(text).unwrap_or_default());
    let yes = document::eval(&js).await.ok().and_then(|v| v.as_bool()).unwrap_or(false);
    // Declined: whatever a queue button asked for is not going to happen.
    if !yes {
        crate::api::queue_next(false);
    }
    yes
}

/// "+ queue", placed right after a job's own button (the console's `queueable`): the job
/// waits its turn -- it starts when nothing else is running or waiting -- instead of
/// running alongside the other lanes. It presses the button before it, with
/// `api::queue_next` armed, so the job's own handler (its checks, its confirm) is the one
/// that runs and no handler needs a second copy of itself.
#[component]
pub fn QueueButton() -> Element {
    let id = use_hook(|| {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        format!("queue-button-{}", NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    });
    let press = {
        let id = id.clone();
        move |_| {
            let js = format!("const b = document.getElementById({}).previousElementSibling; if (!b || b.disabled) return false; b.click(); return true;", serde_json::to_string(&id).unwrap_or_default());
            spawn(async move {
                crate::api::queue_next(true);
                if !document::eval(&js).await.ok().and_then(|v| v.as_bool()).unwrap_or(false) {
                    crate::api::queue_next(false);
                    return;
                }
                // The button may have opened a dialog instead of starting the job. Closing it
                // starts nothing, so the next click outside a dialog and the queue buttons
                // disarms the request, as the web console does; otherwise the next job
                // started anywhere would be queued instead. Only a real click counts: another
                // queue button's own `b.click()` must not disarm the request it just armed.
                let mut outside = document::eval(
                    "const h = (e) => { if (!e.isTrusted || (e.target.closest && e.target.closest('[data-queue-button], #modal, #submodal'))) return; \
                     document.removeEventListener('click', h, true); dioxus.send(true); }; \
                     document.addEventListener('click', h, true);",
                );
                if outside.recv::<bool>().await.is_ok() {
                    crate::api::queue_next(false);
                }
            });
        }
    };
    rsx! {
        button { id: "{id}", class: "btn small", r#type: "button", "data-queue-button": "1", title: crate::state::tr("add to queue: starts when the jobs already running or waiting have finished, instead of alongside them"), onclick: press, {crate::state::tr("+ queue")} }
    }
}

/// A media upload's file name is taken (`message` names it): overwrite it, save under a new
/// name, or cancel (`None`). Two plain confirms, so it works the same in every webview.
pub async fn media_on_conflict(message: &str) -> Option<&'static str> {
    if confirm(&format!("{message}\n\n{}", crate::state::tr("Overwrite it? (Cancel for other options)"))).await {
        return Some("overwrite");
    }
    if confirm(&crate::state::tr("Save it under a new name instead? (Cancel to stop)")).await {
        return Some("rename");
    }
    None
}

pub async fn prompt(text: &str, default: &str) -> Option<String> {
    let js = format!("return prompt({}, {});", serde_json::to_string(text).unwrap_or_default(), serde_json::to_string(default).unwrap_or_default());
    document::eval(&js).await.ok().and_then(|v| v.as_str().map(str::to_string))
}
