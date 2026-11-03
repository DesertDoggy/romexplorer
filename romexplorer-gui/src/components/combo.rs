//! The console's `comboBox`: a pulldown you can type into. Single choice (platform, major
//! genre) or several as chips (subgenres, franchises, ...) with an all/any switch.

use std::rc::Rc;

use dioxus::prelude::*;

use crate::state;

#[derive(Debug, Clone, PartialEq)]
pub struct ComboOpt {
    pub value: String,
    pub label: String,
    pub group: Option<String>,
    pub count: Option<u64>,
}

impl ComboOpt {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self { value: value.into(), label: label.into(), group: None, count: None }
    }
}

pub type Options = Rc<dyn Fn() -> Vec<ComboOpt>>;

/// `value`: the chosen values, comma-separated (the filter's query value). `logic`:
/// "and" | "or" for multi with a logic switch.
#[component]
pub fn Combo(label: String, multi: bool, has_logic: bool, value: String, logic: String, options: ComboOptions, on_change: EventHandler<(String, String)>) -> Element {
    let mut text = use_signal(String::new);
    let mut open = use_signal(|| false);
    let selected: Vec<String> = value.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect();
    // What was ticked when the list opened goes first. Taken then and not on every tick, so
    // an entry doesn't jump away from under the pointer as it is ticked.
    let mut pinned = use_signal(Vec::<String>::new);
    let all = (options.0)();
    let label_of = |v: &str| all.iter().find(|o| o.value == v).map(|o| o.label.clone()).unwrap_or_else(|| v.to_string());
    let q = text().trim().to_lowercase();
    // A list of tick boxes: what is ticked first, then the rest; typing narrows both.
    let matching: Vec<&ComboOpt> = all.iter().filter(|o| q.is_empty() || o.label.to_lowercase().contains(&q) || o.group.as_deref().unwrap_or("").to_lowercase().contains(&q)).collect();
    let first = pinned.read().clone();
    let shown: Vec<ComboOpt> = matching.iter().filter(|o| first.contains(&o.value)).chain(matching.iter().filter(|o| !first.contains(&o.value))).take(300).map(|o| (*o).clone()).collect();
    let sel_for_open = selected.clone();
    let placeholder = if !multi { selected.first().map(|v| label_of(v)).unwrap_or_else(|| label.clone()) } else { label.clone() };
    let chips: Vec<(usize, String)> = selected.iter().enumerate().map(|(i, v)| (i, label_of(v))).collect();
    let sel_for_remove = selected.clone();
    let sel_for_pick = selected.clone();
    let logic_now = logic.clone();
    let value_now = value.clone();
    let first = shown.first().map(|o| o.value.clone());
    rsx! {
        span { class: "combo", title: "{label}",
            span { class: "chips",
                for (i, l) in chips.clone() {
                    span { key: "{i}", class: "badge", title: state::tr("right-click to remove"),
                        // The whole chip comes off on right-click, not only its ×.
                        oncontextmenu: {
                            let sel = sel_for_remove.clone();
                            let logic = logic.clone();
                            move |e: MouseEvent| {
                                e.prevent_default();
                                e.stop_propagation();
                                let mut s = sel.clone();
                                s.remove(i);
                                on_change.call((s.join(","), logic.clone()));
                            }
                        },
                        "{l} "
                        b { title: state::tr("remove"),
                            onclick: {
                                let sel = sel_for_remove.clone();
                                let logic = logic.clone();
                                move |e: MouseEvent| {
                                    e.stop_propagation();
                                    let mut s = sel.clone();
                                    s.remove(i);
                                    on_change.call((s.join(","), logic.clone()));
                                }
                            },
                            "×"
                        }
                    }
                }
                if multi && chips.len() > 1 {
                    span { class: "badge dim", style: "cursor:pointer", title: state::tr("clear all"),
                        onclick: { let logic = logic.clone(); move |e: MouseEvent| { e.stop_propagation(); on_change.call((String::new(), logic.clone())); } },
                        {state::tr("clear all")} " ×"
                    }
                }
            }
            input { r#type: "text", placeholder: "{placeholder}", value: "{text}",
                onfocus: move |_| { pinned.set(sel_for_open.clone()); open.set(true); },
                oninput: move |e| { text.set(e.value()); open.set(true); },
                onblur: move |_| { spawn(async move { crate::ui::sleep(150).await; open.set(false); }); },
                onkeydown: {
                    let sel = selected.clone();
                    let logic = logic.clone();
                    let first = first.clone();
                    move |e: KeyboardEvent| {
                        match e.key() {
                            Key::Escape => open.set(false),
                            Key::Backspace if text().is_empty() && !sel.is_empty() => {
                                let mut s = sel.clone();
                                s.pop();
                                on_change.call((s.join(","), logic.clone()));
                            }
                            Key::Enter => {
                                if let Some(v) = &first {
                                    let mut s = if multi { sel.clone() } else { Vec::new() };
                                    s.push(v.clone());
                                    text.set(String::new());
                                    if !multi { open.set(false); }
                                    on_change.call((s.join(","), logic.clone()));
                                }
                            }
                            _ => {}
                        }
                    }
                },
            }
            if open() {
                div { class: "drop",
                    if multi && has_logic {
                        div { class: "muted logic",
                            for (val, text) in [("or", state::tr("any (OR)")), ("and", state::tr("all (AND)"))] {
                                // The whole label switches, its text as well as its circle.
                                label {
                                    onmousedown: {
                                        let v = value_now.clone();
                                        move |e: MouseEvent| { e.prevent_default(); on_change.call((v.clone(), val.to_string())) }
                                    },
                                    input { r#type: "radio", checked: (logic_now == "and") == (val == "and") }
                                    " {text}"
                                }
                            }
                        }
                    }
                    if shown.is_empty() {
                        div { class: "muted", style: "padding:4px 8px", {state::tr("no match")} }
                    }
                    for o in shown {
                        div { key: "{o.value}", class: "opt",
                            onmousedown: {
                                let sel = sel_for_pick.clone();
                                let logic = logic.clone();
                                let v = o.value.clone();
                                move |e: MouseEvent| {
                                    e.prevent_default();
                                    // Multi: a tick box, on or off. Single: the one choice.
                                    let s: Vec<String> = if !multi {
                                        vec![v.clone()]
                                    } else if sel.contains(&v) {
                                        sel.iter().filter(|x| **x != v).cloned().collect()
                                    } else {
                                        sel.iter().cloned().chain(std::iter::once(v.clone())).collect()
                                    };
                                    if !multi { text.set(String::new()); open.set(false); }
                                    on_change.call((s.join(","), logic.clone()));
                                }
                            },
                            if multi { input { r#type: "checkbox", checked: sel_for_pick.contains(&o.value) } }
                            "{o.label}"
                            if let Some(g) = &o.group { span { class: "muted", " · {g}" } }
                            if let Some(c) = o.count { span { class: "muted num", " {c}" } }
                        }
                    }
                }
            }
        }
    }
}

/// Options for [`Combo`], compared by pointer (a new closure is a new list).
#[derive(Clone)]
pub struct ComboOptions(pub Options);

impl PartialEq for ComboOptions {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}
