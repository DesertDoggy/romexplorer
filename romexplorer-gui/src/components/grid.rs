//! The console's `makeGrid`: a server-paged list with a list and a card-grid mode, sort,
//! filters, page size, selection (checkbox, ctrl/cmd+click, drag a rectangle), right-click
//! menus (acting on the selection when the clicked row is in it), hover popovers, row
//! reordering, resizable columns, and search / filters / sort / page remembered per page
//! for the session (back / forward and links return to the same list).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use dioxus::prelude::*;
use serde_json::Value;

use super::combo::{Combo, ComboOptions, Options as ComboOpts};
use crate::api::{self, ApiResult};
use crate::state;
use crate::ui::{self, HtmlDiv, MenuItem};
use crate::Route;

pub type Fut<T> = Pin<Box<dyn Future<Output = T>>>;

/// What a list asks the server for.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Query {
    pub search: String,
    pub sort: String,
    pub desc: bool,
    pub offset: u64,
    pub limit: u64,
    pub filters: BTreeMap<String, String>,
}

impl Query {
    pub fn f(&self, key: &str) -> String {
        self.filters.get(key).cloned().unwrap_or_default()
    }
    /// search / sort / desc / limit / offset as query pairs.
    pub fn base(&self) -> Vec<(&'static str, String)> {
        vec![
            ("search", self.search.clone()),
            ("sort", self.sort.clone()),
            ("desc", if self.desc { "true".into() } else { String::new() }),
            ("limit", self.limit.to_string()),
            ("offset", self.offset.to_string()),
        ]
    }
}

#[derive(Clone)]
pub struct Column {
    pub key: &'static str,
    pub label: String,
    pub sort: Option<&'static str>,
    pub cls: &'static str,
    pub render: Rc<dyn Fn(&Value) -> Element>,
}

impl Column {
    /// A column showing the row's `key` field as text.
    pub fn text(key: &'static str, label: &str) -> Self {
        Self { key, label: label.to_string(), sort: None, cls: "", render: Rc::new(move |r: &Value| rsx! { "{api::s(r, key)}" }) }
    }
    /// A column showing HTML built from the row.
    pub fn html(key: &'static str, label: &str, f: impl Fn(&Value) -> String + 'static) -> Self {
        Self { key, label: label.to_string(), sort: None, cls: "", render: Rc::new(move |r: &Value| rsx! { crate::ui::Html { html: f(r) } }) }
    }
    pub fn render(key: &'static str, label: &str, f: impl Fn(&Value) -> Element + 'static) -> Self {
        Self { key, label: label.to_string(), sort: None, cls: "", render: Rc::new(f) }
    }
    pub fn sort(mut self, s: &'static str) -> Self {
        self.sort = Some(s);
        self
    }
    pub fn num(mut self) -> Self {
        self.cls = "num";
        self
    }
}

#[derive(Clone)]
pub enum Filter {
    Select { key: &'static str, label: String, options: Rc<dyn Fn() -> Vec<(String, String)>> },
    Check { key: &'static str, label: String, off: String },
    Combo { key: &'static str, label: String, multi: bool, logic_key: Option<&'static str>, options: ComboOpts },
    /// Tick boxes behind a button, any of which may match: the value is the ticked ones,
    /// comma separated. `extra` are listed below a rule (ticks that are not one of the
    /// kinds); `ticked` turns the stored value -- empty when never touched -- into the
    /// boxes ticked; `none` is the value when none is.
    Checks { key: &'static str, label: String, options: CheckOpts, extra: CheckOpts, ticked: Rc<dyn Fn(&str) -> Vec<String>>, none: &'static str },
}

pub type CheckOpts = Rc<dyn Fn() -> Vec<(String, String)>>;

/// A card in grid mode. `chips` / `sub` are HTML.
impl Filter {
    /// A multi combo with an any / all switch stored under `key`.
    pub fn logic(self, key: &'static str) -> Self {
        match self {
            Filter::Combo { key: k, label, multi, options, .. } => Filter::Combo { key: k, label, multi, logic_key: Some(key), options },
            f => f,
        }
    }
}

#[derive(Clone, Default)]
pub struct Card {
    pub title: String,
    pub media: Option<u64>,
    /// A second cover beside `media` (Collection › Definitive's two-cover grid), laid out by
    /// `dual`: top_bottom | diagonal | left_right.
    pub media2: Option<u64>,
    pub dual: String,
    pub img: Option<String>,
    /// Ownership class from `screens::common::held_class`: `local` / `unsynced` /
    /// `remoteonly`, or empty for none.
    pub held: &'static str,
    pub chips: String,
    /// LOCAL / RA pills (`screens::common::status_badges`), top right on the art.
    pub flags: String,
    pub sub: String,
    pub cls: String,
}

/// A row's sizes for the selection total: a title's four (on disk / catalogue, all its
/// releases and its first), a release's two. None for rows that carry none.
fn row_sizes(r: &Value) -> Option<Vec<(String, u64)>> {
    let n = |k: &str| r.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    if r.get("local_size_all").is_some() {
        return Some(vec![
            (state::tr("on disk, all releases"), n("local_size_all")),
            (state::tr("on disk, first release"), n("local_size_top")),
            (state::tr("catalogue, all releases (raw)"), n("catalog_size_all")),
            (state::tr("catalogue, first release (raw)"), n("catalog_size_top")),
        ]);
    }
    if r.get("local_size").is_some() {
        return Some(vec![(state::tr("on disk"), n("local_size")), (state::tr("catalogue (raw)"), n("catalog_size"))]);
    }
    None
}

/// A button on the selection bar: runs on the selected ids; `true` clears the selection
/// and reloads.
#[derive(Clone)]
pub struct SelAction {
    pub label: String,
    pub run: Rc<dyn Fn(Vec<u64>) -> Fut<bool>>,
}

impl SelAction {
    pub fn new<F: Future<Output = bool> + 'static>(label: &str, f: impl Fn(Vec<u64>) -> F + 'static) -> Self {
        Self { label: label.to_string(), run: Rc::new(move |ids| Box::pin(f(ids))) }
    }
}

#[derive(Clone)]
pub struct Selection {
    pub key: Rc<dyn Fn(&Value) -> u64>,
    pub actions: Vec<SelAction>,
}

#[derive(Clone)]
pub struct GridSpec {
    /// Identifies the list (remembered state, view mode, column widths).
    pub key: String,
    /// Its `view_defaults` entry (titles / releases / games / sets / ra); empty = list only.
    pub view_key: String,
    pub columns: Vec<Column>,
    pub filters: Vec<Filter>,
    pub fetch: Rc<dyn Fn(Query) -> Fut<ApiResult>>,
    pub row_link: Option<Rc<dyn Fn(&Value) -> Option<Route>>>,
    pub card: Option<Rc<dyn Fn(&Value, &str) -> Card>>,
    pub modes: Vec<(&'static str, &'static str)>,
    pub context: Option<Rc<dyn Fn(Value, Option<Vec<u64>>) -> Fut<Vec<MenuItem>>>>,
    pub selection: Option<Selection>,
    pub hover: Option<Rc<dyn Fn(Value) -> Fut<Option<String>>>>,
    pub toolbar: Option<Rc<dyn Fn() -> Element>>,
    pub search: bool,
    pub page_size: u64,
    pub default_sort: String,
    pub default_desc: bool,
    pub initial_filters: BTreeMap<String, String>,
    /// The row's ownership class (`screens::common::held_class`), for the left bar.
    pub is_local: Option<Rc<dyn Fn(&Value) -> &'static str>>,
    pub reorder: Option<Rc<dyn Fn(Value, Option<Value>)>>,
    pub loaded: Option<Rc<dyn Fn(&Value)>>,
    /// Without toolbar and pager (a detail page's static list).
    pub bare: bool,
}

/// Where a dragged item lands: dropped on one further down, right after it; further up,
/// right before it (after the one above it, or at the top). Returns the item it goes after.
fn drop_after(rows: &[Value], from: usize, onto: usize) -> Option<Value> {
    if from < onto {
        Some(rows[onto].clone())
    } else if onto > 0 {
        Some(rows[onto - 1].clone())
    } else {
        None
    }
}

/// What a list is, behind a "!" at the start of its toolbar: language-file keys by list.
fn explain_keys(key: &str) -> &'static [&'static str] {
    match key {
        "titles" => &["explain.collection.titles", "explain.list.ownership"],
        "releases" => &["explain.collection.releases", "explain.list.ownership"],
        "games" => &["explain.collection.games", "explain.list.ownership"],
        "roms" => &["explain.collection.roms"],
        "files" => &["explain.collection.files"],
        "material_etc" => &["explain.collection.etc", "explain.list.ownership"],
        "patches" => &["explain.collection.patches"],
        "bios" => &["explain.collection.bios"],
        "ra" => &["explain.ra.list", "explain.list.ownership"],
        "definitive" => &["explain.collection.definitive", "explain.definitive.notes", "explain.list.ownership"],
        _ => &[],
    }
}

impl GridSpec {
    pub fn new<F: Future<Output = ApiResult> + 'static>(key: &str, columns: Vec<Column>, fetch: impl Fn(Query) -> F + 'static) -> Self {
        Self {
            key: key.to_string(),
            view_key: String::new(),
            columns,
            filters: Vec::new(),
            fetch: Rc::new(move |q| Box::pin(fetch(q))),
            row_link: None,
            card: None,
            modes: vec![("list", "list"), ("grid", "grid")],
            context: None,
            selection: None,
            hover: None,
            toolbar: None,
            search: true,
            page_size: 50,
            default_sort: String::new(),
            default_desc: false,
            initial_filters: BTreeMap::new(),
            is_local: None,
            reorder: None,
            loaded: None,
            bare: false,
        }
    }
    pub fn link(mut self, f: impl Fn(&Value) -> Option<Route> + 'static) -> Self {
        self.row_link = Some(Rc::new(f));
        self
    }
    pub fn cards(mut self, view_key: &str, f: impl Fn(&Value, &str) -> Card + 'static) -> Self {
        self.view_key = view_key.to_string();
        self.card = Some(Rc::new(f));
        self
    }
    pub fn context<F: Future<Output = Vec<MenuItem>> + 'static>(mut self, f: impl Fn(Value, Option<Vec<u64>>) -> F + 'static) -> Self {
        self.context = Some(Rc::new(move |r, sel| Box::pin(f(r, sel))));
        self
    }
    pub fn hover<F: Future<Output = Option<String>> + 'static>(mut self, f: impl Fn(Value) -> F + 'static) -> Self {
        self.hover = Some(Rc::new(move |r| Box::pin(f(r))));
        self
    }
    pub fn select(mut self, key: impl Fn(&Value) -> u64 + 'static, actions: Vec<SelAction>) -> Self {
        self.selection = Some(Selection { key: Rc::new(key), actions });
        self
    }
    pub fn filters(mut self, f: Vec<Filter>) -> Self {
        self.filters = f;
        self
    }
    pub fn toolbar(mut self, f: impl Fn() -> Element + 'static) -> Self {
        self.toolbar = Some(Rc::new(f));
        self
    }
    pub fn local(mut self, f: impl Fn(&Value) -> &'static str + 'static) -> Self {
        self.is_local = Some(Rc::new(f));
        self
    }
}

/// Given to everything rendered inside a grid (toolbar widgets): its list state.
#[derive(Clone, Copy)]
pub struct GridCtl {
    pub st: Signal<Query>,
}

impl GridCtl {
    pub fn filter(&self, key: &str) -> String {
        self.st.read().f(key)
    }
    pub fn set_filter(&self, key: &str, value: String) {
        let mut st = self.st;
        let mut s = st.write();
        s.filters.insert(key.to_string(), value);
        s.offset = 0;
    }
}

/// Props wrapper: a spec is the same list when its key is.
#[derive(Clone)]
pub struct Spec(pub Rc<GridSpec>);

impl PartialEq for Spec {
    fn eq(&self, other: &Self) -> bool {
        self.0.key == other.0.key
    }
}

/// How wide a column's widest cell wants to be, for the resize cap (TABLE and COL are
/// defined by the caller). Each cell's content is cloned into an unconstrained max-content
/// box inside it, so the answer doesn't depend on how the table is laid out at that moment
/// (scrollWidth did: a cell still capped by the automatic layout reported the cap).
const WIDEST_JS: &str = "const th = document.querySelector('#' + TABLE + ' th[data-col=' + COL + ']'); if (!th) return 1e9; const i = [...th.parentElement.children].indexOf(th) + 1; const natural = c => { const m = document.createElement('span'); m.style.cssText = 'position:absolute;visibility:hidden;white-space:nowrap;width:max-content;left:0;top:0;pointer-events:none'; for (const n of c.childNodes) m.appendChild(n.cloneNode(true)); c.appendChild(m); const cs = getComputedStyle(c); const x = m.offsetWidth + parseFloat(cs.paddingLeft) + parseFloat(cs.paddingRight) + 2; m.remove(); return x; }; return Math.max(40, ...[...th.closest('table').querySelectorAll('tr > *:nth-child(' + i + ')')].map(natural));";

thread_local! {
    /// Remembered list state per page + list, for the app session.
    static MEMORY: RefCell<HashMap<String, Query>> = RefCell::new(HashMap::new());
    /// View mode picked per list this session.
    static MODES: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    /// Column widths per list.
    static WIDTHS: RefCell<HashMap<String, BTreeMap<String, f64>>> = RefCell::new(HashMap::new());
}

/// Opens `route` in a new browser tab (web); on desktop, just goes there.
pub fn open_new_tab(route: &Route) {
    #[cfg(target_arch = "wasm32")]
    {
        let url = format!("/app{route}");
        let _ = document::eval(&format!("window.open({}, '_blank', 'noopener')", serde_json::to_string(&url).unwrap_or_default()));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        navigator().push(route.clone());
    }
}

/// The standard right-click entries for a row that links somewhere.
pub fn open_items(route: &Route) -> Vec<MenuItem> {
    let (a, b) = (route.clone(), route.clone());
    vec![MenuItem::new(state::tr("Open"), move || { navigator().push(a.clone()); }), MenuItem::new(state::tr("Open in new tab"), move || open_new_tab(&b))]
}

#[component]
pub fn Grid(spec: Spec, #[props(default)] reload: Option<Signal<u64>>) -> Element {
    let spec = spec.0.clone();
    let route = use_route::<Route>();
    let mem_key = format!("{}|{}", route, spec.key);
    let initial = {
        let spec = spec.clone();
        let mem_key = mem_key.clone();
        move || {
            MEMORY.with(|m| m.borrow().get(&mem_key).cloned()).unwrap_or_else(|| Query {
                sort: spec.default_sort.clone(),
                desc: spec.default_desc,
                offset: 0,
                limit: spec.page_size,
                search: String::new(),
                filters: spec.initial_filters.clone(),
            })
        }
    };
    let mut st = use_signal(initial);
    use_context_provider(|| GridCtl { st });
    let spec_for_mode = spec.clone();
    let mut mode = use_signal(move || {
        let s = &spec_for_mode;
        if s.card.is_none() {
            return "list".to_string();
        }
        let fallback = if s.view_key == "ra" { "badges" } else { "grid" };
        let m = MODES.with(|m| m.borrow().get(&s.key).cloned()).unwrap_or_else(|| crate::state::view_default(&s.view_key, fallback));
        if s.modes.iter().any(|(k, _)| *k == m) {
            m
        } else {
            s.modes.last().map(|(k, _)| k.to_string()).unwrap_or_else(|| "list".into())
        }
    });
    let mut selected = use_signal(BTreeSet::<u64>::new);
    // Sizes of every row seen (`row_sizes`), by selection key: the selection's total spans pages.
    let size_cache = use_hook(|| Rc::new(RefCell::new(HashMap::<u64, Vec<(String, u64)>>::new())));
    let mut search_text = use_signal(move || st.peek().search.clone());
    let mut search_gen = use_signal(|| 0u64);
    let spec_w = spec.clone();
    let mut widths = use_signal(move || WIDTHS.with(|w| w.borrow().get(&spec_w.key).cloned()));
    // (column, mouse x at grab, width at grab, widest content in the column).
    let mut resize = use_signal(|| None::<(String, f64, f64, f64)>);
    let mut marquee = use_signal(|| None::<(f64, f64, f64, f64)>);
    let mounts: Rc<RefCell<HashMap<usize, Rc<MountedData>>>> = use_hook(|| Rc::new(RefCell::new(HashMap::new())));
    let th_mounts: Rc<RefCell<HashMap<String, Rc<MountedData>>>> = use_hook(|| Rc::new(RefCell::new(HashMap::new())));
    let mut suppress_click = use_signal(|| false);
    let mut drag_from = use_signal(|| None::<usize>);
    let mut show_filters = use_signal(|| false);

    let spec_fetch = spec.clone();
    let mem_key_save = mem_key.clone();
    let page = use_resource(move || {
        let q = st();
        if let Some(r) = reload {
            let _ = r();
        }
        let fetch = spec_fetch.fetch.clone();
        let loaded = spec_fetch.loaded.clone();
        MEMORY.with(|m| m.borrow_mut().insert(mem_key_save.clone(), q.clone()));
        async move {
            let v = fetch(q).await?;
            if let Some(l) = loaded {
                l(&v);
            }
            Ok::<Value, api::ApiError>(v)
        }
    });

    let (rows, total, error) = match &*page.read() {
        Some(Ok(v)) => (api::items(v), v.get("total").and_then(|t| t.as_u64()).unwrap_or_else(|| api::items(v).len() as u64), None),
        Some(Err(e)) => (Vec::new(), 0, Some(e.message.clone())),
        None => (Vec::new(), 0, None),
    };
    let rows = Rc::new(rows);
    let q = st();
    let is_grid = mode() != "list";
    let sel_key = spec.selection.as_ref().map(|s| s.key.clone());
    let n_sel = selected.read().len();

    // --- helpers shared by rows and cards ---
    let key_of = {
        let k = sel_key.clone();
        move |r: &Value| k.as_ref().map(|k| k(r))
    };
    let selection_for = {
        let key_of = key_of.clone();
        move |r: &Value| -> Option<Vec<u64>> {
            let id = key_of(r)?;
            let sel = selected.read();
            (sel.len() > 1 && sel.contains(&id)).then(|| sel.iter().copied().collect())
        }
    };

    let set_filter = move |k: String, v: String| {
        let mut s = st.write();
        s.filters.insert(k, v);
        s.offset = 0;
    };

    let from = if total > 0 { q.offset + 1 } else { 0 };
    let to = (q.offset + q.limit).min(total);
    let w = widths();
    let has_widths = w.is_some();
    // Fixed layout only applies with a definite table width (see app.css): the columns' sum.
    let table_width = w.as_ref().map(|w| format!("width:{}px", spec.columns.iter().filter_map(|c| w.get(c.key)).sum::<f64>() + if sel_key.is_some() { 32.0 } else { 0.0 })).unwrap_or_default();
    let pager_text = state::trf("{from}–{to} of {total}", &[("from", &from.to_string()), ("to", &to.to_string()), ("total", &total.to_string())]);
    // Every "select all on this page" box -- the list header's and the one beside each pager
    // (the only one a card grid has) -- shows the same state and does the same thing.
    let page_ids: Vec<u64> = rows.iter().filter_map(|r| key_of(r)).collect();
    let page_selected = !page_ids.is_empty() && { let sel = selected.read(); page_ids.iter().all(|id| sel.contains(id)) };
    let select_page = {
        let ids = page_ids.clone();
        move |on: bool| {
            let mut s = selected.write();
            for id in &ids {
                if on { s.insert(*id); } else { s.remove(id); }
            }
        }
    };
    let has_selection = sel_key.is_some();
    let prev_page = move |_| { let mut s = st.write(); s.offset = s.offset.saturating_sub(s.limit); };
    let next_page = move |_| { let mut s = st.write(); s.offset += s.limit; };
    let n_sel_text = state::trf("{n} selected ", &[("n", &n_sel.to_string())]);
    // What the selection adds up to: not held counts 0 on disk and its full catalogue size;
    // "≥" while some selected row was never loaded.
    let sel_sizes = {
        let mut cache = size_cache.borrow_mut();
        for r in rows.iter() {
            if let (Some(id), Some(z)) = (key_of(r), row_sizes(r)) {
                cache.insert(id, z);
            }
        }
        let sel = selected.read();
        let known: Vec<&Vec<(String, u64)>> = sel.iter().filter_map(|id| cache.get(id)).collect();
        match known.first() {
            None => String::new(),
            Some(first) => {
                let more = if known.len() < sel.len() { "≥ " } else { "" };
                first.iter().enumerate().map(|(i, (label, _))| format!("{label} {more}{}", crate::html::fmt_size(Some(known.iter().map(|z| z[i].1).sum())))).collect::<Vec<_>>().join(" · ")
            }
        }
    };

    let explain = explain_keys(&spec.key);
    rsx! {
        if !spec.bare {
            div { class: "toolbar",
                if !explain.is_empty() { ui::KeyExplain { keys: explain.iter().map(|k| k.to_string()).collect::<Vec<_>>() } }
                if spec.search {
                    input { r#type: "text", placeholder: state::tr("search…"), value: "{search_text}",
                        oninput: move |e| {
                            search_text.set(e.value());
                            let gen = { let mut g = search_gen.write(); *g += 1; *g };
                            spawn(async move {
                                ui::sleep(300).await;
                                if search_gen() == gen {
                                    let mut s = st.write();
                                    s.search = search_text();
                                    s.offset = 0;
                                }
                            });
                        }
                    }
                }
                // Phones: the filters fold behind a toggle (mobile.css); elsewhere the span
                // is `display: contents`, so the toolbar looks as before.
                if !spec.filters.is_empty() {
                    button { class: if show_filters() { "btn small filtertoggle on" } else { "btn small filtertoggle" }, onclick: move |_| show_filters.toggle(), {state::tr("filters")} }
                }
                span { class: if show_filters() { "filters open" } else { "filters" },
                    for f in spec.filters.clone() {
                        {filter_widget(f, &q, set_filter)}
                    }
                }
                select { value: "{q.limit}", onchange: move |e| { let mut s = st.write(); s.limit = e.value().parse().unwrap_or(50); s.offset = 0; },
                    for n in [25u64, 50, 100, 200, 500] { option { value: "{n}", selected: q.limit == n, "{n}" } }
                }
                if spec.card.is_some() {
                    for (m, label) in spec.modes.clone() {
                        button { class: if mode() == m { "btn small on" } else { "btn small" },
                            onclick: {
                                let key = spec.key.clone();
                                move |_| { mode.set(m.to_string()); MODES.with(|x| x.borrow_mut().insert(key.clone(), m.to_string())); }
                            },
                            {state::tr(label)}
                        }
                    }
                }
                button { class: "btn", onclick: move |_| { let mut p = page; p.restart(); }, {state::tr("refresh")} }
                if let Some(sel) = spec.selection.clone() {
                    if n_sel > 0 {
                        span { class: "selbar",
                            span { class: "muted", "{n_sel_text}" }
                            if !sel_sizes.is_empty() { span { class: "muted", "{sel_sizes} " } }
                            for a in sel.actions.clone() {
                                button { class: "btn small primary",
                                    onclick: move |_| {
                                        let run = a.run.clone();
                                        let ids: Vec<u64> = selected.read().iter().copied().collect();
                                        spawn(async move {
                                            if run(ids).await {
                                                selected.write().clear();
                                                let mut p = page;
                                                p.restart();
                                            }
                                        });
                                    },
                                    {state::tr(&a.label)}
                                }
                            }
                            button { class: "btn small", onclick: move |_| selected.write().clear(), {state::tr("clear")} }
                        }
                    }
                }
                if let Some(t) = spec.toolbar.clone() { {t()} }
            }
        }
        if let Some(e) = error { div { class: "bad", "{e}" } }
        // The same pager above the list too, so a long page need not be scrolled to its end.
        if !spec.bare {
            Pager { text: pager_text.clone(), prev_disabled: q.offset == 0, next_disabled: to >= total, on_prev: prev_page, on_next: next_page, select_all: has_selection.then_some(page_selected), on_select_all: select_page.clone() }
        }
        div {
            // A drag on empty space (or ctrl/cmd + drag anywhere) selects a rectangle.
            onmousedown: move |e| {
                if sel_key.is_none() || e.trigger_button() != Some(dioxus::html::input_data::MouseButton::Primary) { return; }
                let p = e.client_coordinates();
                marquee.set(Some((p.x, p.y, p.x, p.y)));
            },
            if is_grid {
                div { class: "cardgrid",
                    if rows.is_empty() { div { class: "muted", {state::tr("no rows")} } }
                    for (i, r) in rows.iter().cloned().enumerate() {
                        {
                            let card = spec.card.as_ref().map(|c| c(&r, &mode())).unwrap_or_default();
                            let link = spec.row_link.as_ref().and_then(|l| l(&r));
                            let id = key_of(&r);
                            let is_sel = id.is_some_and(|id| selected.read().contains(&id));
                            let src = card.img.clone().or_else(|| card.media.map(api::thumb));
                            let initials: String = card.title.chars().take(2).collect();
                            let mounts = mounts.clone();
                            let ctx_spec = spec.clone();
                            let selection_for = selection_for.clone();
                            let r_ctx = r.clone();
                            let link_ctx = link.clone();
                            let link_click = link.clone();
                            // Cards reorder by drag like table rows (a title's releases open as cards).
                            let reorder_card = spec.reorder.clone();
                            let rows_for_card = rows.clone();
                            rsx! {
                                div { key: "{i}", class: format!("gcard {} {} {}", card.held, card.cls, if is_sel { "selected" } else { "" }),
                                    draggable: reorder_card.is_some(),
                                    ondragstart: move |_| drag_from.set(Some(i)),
                                    ondragover: move |e| e.prevent_default(),
                                    ondrop: move |e| {
                                        e.prevent_default();
                                        let Some(from) = drag_from() else { return };
                                        drag_from.set(None);
                                        if from == i { return; }
                                        if let Some(f) = &reorder_card {
                                            f(rows_for_card[from].clone(), drop_after(&rows_for_card, from, i));
                                        }
                                    },
                                    onmounted: move |e| { mounts.borrow_mut().insert(i, e.data()); },
                                    onmousedown: move |e| { if !(e.modifiers().ctrl() || e.modifiers().meta()) { e.stop_propagation(); } },
                                    onclick: move |e| {
                                        if suppress_click() { suppress_click.set(false); return; }
                                        if (e.modifiers().ctrl() || e.modifiers().meta()) && id.is_some() {
                                            let id = id.unwrap_or_default();
                                            let mut s = selected.write();
                                            if !s.insert(id) { s.remove(&id); }
                                            return;
                                        }
                                        if let Some(l) = &link_click {
                                            if e.modifiers().shift() { open_new_tab(l); } else { navigator().push(l.clone()); }
                                        }
                                    },
                                    oncontextmenu: move |e| {
                                        e.prevent_default();
                                        let p = e.client_coordinates();
                                        let head = link_ctx.as_ref().map(open_items).unwrap_or_default();
                                        let ctx = ctx_spec.context.clone();
                                        let r = r_ctx.clone();
                                        let sel = selection_for(&r);
                                        spawn(async move {
                                            let mut items = Vec::new();
                                            let extra = match ctx { Some(c) => c(r, sel).await, None => Vec::new() };
                                            let mut extra = extra.into_iter().peekable();
                                            if extra.peek().is_some_and(|x| x.head) { items.extend(extra.next()); }
                                            items.extend(head);
                                            items.extend(extra);
                                            ui::open_menu(p.x, p.y, items);
                                        });
                                    },
                                    div { class: if card.media2.is_some() { format!("art dual {}", card.dual) } else { "art".to_string() },
                                        if let Some(src) = src { img { class: "a", loading: "lazy", src: "{src}", alt: "" } } else { "{initials}" }
                                        if let Some(m2) = card.media2 { img { class: "b", loading: "lazy", src: "{api::thumb(m2)}", alt: "" } }
                                        if !card.chips.is_empty() { HtmlDiv { class: "chips", html: card.chips.clone() } }
                                        if !card.flags.is_empty() { HtmlDiv { class: "flags", html: card.flags.clone() } }
                                        if id.is_some() {
                                            input { r#type: "checkbox", class: "rowsel cardsel", checked: is_sel,
                                                onclick: move |e| e.stop_propagation(),
                                                onchange: move |e| { let id = id.unwrap_or_default(); if e.checked() { selected.write().insert(id); } else { selected.write().remove(&id); } },
                                            }
                                        }
                                    }
                                    div { class: "cap", div { class: "t", title: "{card.title}", "{card.title}" } HtmlDiv { html: card.sub.clone() } }
                                }
                            }
                        }
                    }
                }
            } else {
                div { class: if has_widths { "gridscroll fixed" } else { "gridscroll" },
                table { class: if has_widths { "grid fixedcols" } else { "grid" }, id: "grid-{spec.key}", style: "{table_width}",
                    thead {
                        tr {
                            if sel_key.is_some() {
                                th { style: if has_widths { "width:32px" } else { "" },
                                    input { r#type: "checkbox", class: "selall", title: state::tr("select all on this page"), checked: page_selected,
                                        onchange: { let mut select_page = select_page.clone(); move |e: FormEvent| select_page(e.checked()) }
                                    }
                                }
                            }
                            for c in spec.columns.clone() {
                                {
                                    let width = w.as_ref().and_then(|w| w.get(c.key)).map(|x| format!("width:{x}px")).unwrap_or_default();
                                    let sorted = c.sort.is_some() && c.sort.map(str::to_string) == Some(q.sort.clone());
                                    let arrow = if sorted { if q.desc { " ▼" } else { " ▲" } } else { "" };
                                    let th_mounts = th_mounts.clone();
                                    let th_mounts2 = th_mounts.clone();
                                    let col_key = c.key.to_string();
                                    let spec_k = spec.key.clone();
                                    let head = format!("{}{arrow}", state::tr(&c.label));
                                    rsx! {
                                        th { key: "{c.key}", class: if sorted { "sorted" } else { "" }, style: "{width}", "data-col": "{c.key}",
                                            onmounted: move |e| { th_mounts.borrow_mut().insert(c.key.to_string(), e.data()); },
                                            onclick: move |_| {
                                                if let Some(k) = c.sort { let mut s = st.write(); if s.sort == k { s.desc = !s.desc; } else { s.sort = k.to_string(); s.desc = false; } s.offset = 0; }
                                            },
                                            "{head}"
                                            span { class: "colgrip", title: state::tr("drag to resize · double-click: automatic widths"),
                                                onclick: move |e| e.stop_propagation(),
                                                ondoubleclick: {
                                                    let k = spec_k.clone();
                                                    move |e: MouseEvent| { e.stop_propagation(); widths.set(None); WIDTHS.with(|x| x.borrow_mut().remove(&k)); }
                                                },
                                                onmousedown: move |e| {
                                                    e.stop_propagation();
                                                    e.prevent_default();
                                                    let x0 = e.client_coordinates().x;
                                                    let th_mounts = th_mounts2.clone();
                                                    let col = col_key.clone();
                                                    let table_id = format!("grid-{}", spec_k);
                                                    spawn(async move {
                                                        // Freeze every column at its width first, then drag this one.
                                                        let handles: Vec<(String, Rc<MountedData>)> = th_mounts.borrow().iter().map(|(k, m)| (k.clone(), m.clone())).collect();
                                                        let mut w = widths().unwrap_or_default();
                                                        let mut w0 = 100.0;
                                                        for (k, m) in handles {
                                                            if let Ok(rect) = m.get_client_rect().await {
                                                                w.entry(k.clone()).or_insert(rect.size.width);
                                                                if k == col { w0 = rect.size.width; }
                                                            }
                                                        }
                                                        widths.set(Some(w));
                                                        // The widest content in the column: the drag stops there, so a badge
                                                        // column cannot be stretched past its badge while a path column can go as
                                                        // far as its longest path.
                                                        let js = format!("const TABLE = {}, COL = {}; {}", serde_json::to_string(&table_id).unwrap_or_default(), serde_json::to_string(&serde_json::to_string(&col).unwrap_or_default()).unwrap_or_default(), WIDEST_JS);
                                                        let widest = document::eval(&js).await.ok().and_then(|v| v.as_f64()).unwrap_or(1e9).max(w0);
                                                        resize.set(Some((col, x0, w0, widest)));
                                                    });
                                                },
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    tbody {
                        if rows.is_empty() { tr { td { colspan: "99", class: "muted", {state::tr("no rows")} } } }
                        for (i, r) in rows.iter().cloned().enumerate() {
                            {
                                let link = spec.row_link.as_ref().and_then(|l| l(&r));
                                let id = key_of(&r);
                                let is_sel = id.is_some_and(|id| selected.read().contains(&id));
                                let held = spec.is_local.as_ref().map_or("", |f| f(&r));
                                let cls = format!("{}{}{}{}", if link.is_some() { "click" } else if spec.hover.is_some() { "hover" } else { "" }, if held.is_empty() { "" } else { " " }, held, if is_sel { " selected" } else { "" });
                                let mounts = mounts.clone();
                                let ctx_spec = spec.clone();
                                let hover = spec.hover.clone();
                                let reorder = spec.reorder.clone();
                                let rows_for_drop = rows.clone();
                                let selection_for = selection_for.clone();
                                let r_ctx = r.clone();
                                let r_hover = r.clone();
                                let link_ctx = link.clone();
                                let link_click = link.clone();
                                let columns = spec.columns.clone();
                                rsx! {
                                    tr { key: "{i}", class: "{cls}", draggable: reorder.is_some(),
                                        onmounted: move |e| { mounts.borrow_mut().insert(i, e.data()); },
                                        onmousedown: move |e| { if !(e.modifiers().ctrl() || e.modifiers().meta()) { e.stop_propagation(); } },
                                        onclick: move |e| {
                                            if suppress_click() { suppress_click.set(false); return; }
                                            if (e.modifiers().ctrl() || e.modifiers().meta()) && id.is_some() {
                                                let id = id.unwrap_or_default();
                                                let mut s = selected.write();
                                                if !s.insert(id) { s.remove(&id); }
                                                return;
                                            }
                                            if let Some(l) = &link_click {
                                                if e.modifiers().shift() { open_new_tab(l); } else { navigator().push(l.clone()); }
                                            }
                                        },
                                        oncontextmenu: move |e| {
                                            e.prevent_default();
                                            let p = e.client_coordinates();
                                            let head = link_ctx.as_ref().map(open_items).unwrap_or_default();
                                            let ctx = ctx_spec.context.clone();
                                            let r = r_ctx.clone();
                                            let sel = selection_for(&r);
                                            spawn(async move {
                                                let mut items = Vec::new();
                                                let extra = match ctx { Some(c) => c(r, sel).await, None => Vec::new() };
                                                let mut extra = extra.into_iter().peekable();
                                                if extra.peek().is_some_and(|x| x.head) { items.extend(extra.next()); }
                                                items.extend(head);
                                                items.extend(extra);
                                                ui::open_menu(p.x, p.y, items);
                                            });
                                        },
                                        onmouseenter: move |e| {
                                            if let Some(h) = hover.clone() {
                                                let p = e.client_coordinates();
                                                let r = r_hover.clone();
                                                spawn(async move { if let Some(html) = h(r).await { ui::show_popover(p.x, p.y, html); } });
                                            }
                                        },
                                        onmouseleave: move |_| ui::hide_popover(),
                                        ondragstart: move |_| drag_from.set(Some(i)),
                                        ondragover: move |e| e.prevent_default(),
                                        ondrop: move |e| {
                                            e.prevent_default();
                                            let Some(from) = drag_from() else { return };
                                            drag_from.set(None);
                                            if from == i { return; }
                                            if let Some(f) = &reorder {
                                                f(rows_for_drop[from].clone(), drop_after(&rows_for_drop, from, i));
                                            }
                                        },
                                        if let Some(id) = id {
                                            td {
                                                input { r#type: "checkbox", class: "rowsel", checked: is_sel,
                                                    onclick: move |e| e.stop_propagation(),
                                                    onchange: move |e| { if e.checked() { selected.write().insert(id); } else { selected.write().remove(&id); } },
                                                }
                                            }
                                        }
                                        for c in columns {
                                            td { key: "{c.key}", class: "{c.cls}", {(c.render)(&r)} }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                }
            }
        }
        if !spec.bare {
            Pager { text: pager_text, prev_disabled: q.offset == 0, next_disabled: to >= total, on_prev: prev_page, on_next: next_page, select_all: has_selection.then_some(page_selected), on_select_all: select_page }
        }
        // While resizing a column or drawing a selection rectangle: a transparent overlay
        // takes the mouse until it's released.
        if let Some((col, x0, w0, widest)) = resize() {
            div { style: "position:fixed;inset:0;cursor:col-resize;z-index:60",
                onmousemove: move |e| {
                    let wv = (w0 + e.client_coordinates().x - x0).clamp(40.0, widest.max(40.0));
                    let mut w = widths().unwrap_or_default();
                    w.insert(col.clone(), wv);
                    widths.set(Some(w));
                },
                onmouseup: {
                    let key = spec.key.clone();
                    move |_| { resize.set(None); if let Some(w) = widths() { WIDTHS.with(|x| x.borrow_mut().insert(key.clone(), w)); } }
                },
            }
        }
        if let Some((x0, y0, x1, y1)) = marquee() {
            div { style: "position:fixed;inset:0;z-index:60",
                onmousemove: move |e| { let p = e.client_coordinates(); marquee.set(Some((x0, y0, p.x, p.y))); },
                onmouseup: {
                    let mounts = mounts.clone();
                    let rows = rows.clone();
                    let key_of = key_of.clone();
                    move |_| {
                        marquee.set(None);
                        let (l, t, r, b) = (x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1));
                        if (r - l) + (b - t) < 6.0 { return; }
                        suppress_click.set(true);
                        let handles: Vec<(usize, Rc<MountedData>)> = mounts.borrow().iter().map(|(i, m)| (*i, m.clone())).collect();
                        let rows = rows.clone();
                        let key_of = key_of.clone();
                        spawn(async move {
                            for (i, m) in handles {
                                if let (Ok(rect), Some(row)) = (m.get_client_rect().await, rows.get(i)) {
                                    let (rl, rt) = (rect.origin.x, rect.origin.y);
                                    let (rr, rb) = (rl + rect.size.width, rt + rect.size.height);
                                    if rr >= l && rl <= r && rb >= t && rt <= b {
                                        if let Some(id) = key_of(row) { selected.write().insert(id); }
                                    }
                                }
                            }
                            ui::sleep(50).await;
                            suppress_click.set(false);
                        });
                    }
                },
                div { class: "marquee", style: "left:{x0.min(x1)}px;top:{y0.min(y1)}px;width:{(x1 - x0).abs()}px;height:{(y1 - y0).abs()}px" }
            }
        }
    }
}

fn filter_widget(f: Filter, q: &Query, mut set: impl FnMut(String, String) + Copy + 'static) -> Element {
    match f {
        Filter::Select { key, label, options } => {
            let cur = q.f(key);
            let label = state::tr(&label);
            rsx! {
                select { title: "{label}", onchange: move |e| set(key.to_string(), e.value()),
                    for (v, l) in options() { option { value: "{v}", selected: v == cur, {state::tr(&l)} } }
                }
            }
        }
        Filter::Check { key, label, off } => {
            let on = q.f(key) == "true";
            let label = state::tr(&label);
            rsx! {
                label { class: "muted",
                    input { r#type: "checkbox", checked: on, onchange: move |e| set(key.to_string(), if e.checked() { "true".into() } else { off.clone() }) }
                    " {label}"
                }
            }
        }
        Filter::Checks { key, label, options, extra, ticked, none } => {
            let (main, more) = (options(), extra());
            let selected = ticked(&q.f(key));
            let label = state::tr(&label);
            let shown = if main.iter().all(|(v, _)| selected.contains(v)) || selected.is_empty() {
                state::tr("all")
            } else {
                main.iter().chain(more.iter()).filter(|(v, _)| selected.contains(v)).map(|(_, l)| l.clone()).collect::<Vec<_>>().join(", ")
            };
            let row = move |v: String, l: String, selected: Vec<String>| {
                let on = selected.contains(&v);
                rsx! {
                    label { key: "{v}",
                        input { r#type: "checkbox", checked: on, onchange: move |e| {
                            let mut next: Vec<String> = selected.iter().filter(|x| **x != v).cloned().collect();
                            if e.checked() { next.push(v.clone()); }
                            set(key.to_string(), if next.is_empty() { none.to_string() } else { next.join(",") });
                        } }
                        " {l}"
                    }
                }
            };
            rsx! {
                details { class: "checks",
                    // Opened near the window's right edge: moved left until it fits (as the
                    // web console's tick-box filter does).
                    ontoggle: move |_| {
                        let _ = document::eval("for (const d of document.querySelectorAll('details.checks[open]')) { const p = d.querySelector('.pop'); if (!p) continue; p.style.left = '0'; const over = p.getBoundingClientRect().right - (document.documentElement.clientWidth - 8); if (over > 0) p.style.left = -over + 'px'; }");
                    },
                    summary { title: "{label}", "{label}: {shown}" }
                    div { class: "pop",
                        div { class: "muted", style: "white-space:normal;max-width:240px;margin-bottom:4px", {state::tr("Tick what to show; with nothing ticked, everything is shown.")} }
                        // As the pulldowns' "clear all": back to nothing ticked, which shows everything.
                        if !selected.is_empty() {
                            div { style: "margin-bottom:4px",
                                span { class: "link", onclick: move |_| set(key.to_string(), none.to_string()), {state::tr("clear all")} }
                            }
                        }
                        for (v, l) in main { {row(v, l, selected.clone())} }
                        if !more.is_empty() { div { class: "sep" } }
                        for (v, l) in more { {row(v, l, selected.clone())} }
                    }
                }
            }
        }
        Filter::Combo { key, label, multi, logic_key, options } => {
            let value = q.f(key);
            let logic = logic_key.map(|k| q.f(k)).filter(|l| !l.is_empty()).unwrap_or_else(|| "or".into());
            rsx! {
                Combo { label: state::tr(&label), multi, has_logic: logic_key.is_some(), value, logic, options: ComboOptions(options.clone()),
                    on_change: move |(v, l): (String, String)| { set(key.to_string(), v); if let Some(k) = logic_key { set(k.to_string(), l); } }
                }
            }
        }
    }
}

/// A grid's pager: prev / "1–50 of 900" / next, and with a selection the page's
/// "select all" box (`select_all` is whether the whole page is selected). Shown above and
/// below the list.
#[component]
fn Pager(text: String, prev_disabled: bool, next_disabled: bool, on_prev: EventHandler<()>, on_next: EventHandler<()>, select_all: Option<bool>, on_select_all: EventHandler<bool>) -> Element {
    rsx! {
        div { class: "pager",
            if let Some(all) = select_all {
                label { class: "muted",
                    input { r#type: "checkbox", class: "selall", checked: all, onchange: move |e: FormEvent| on_select_all.call(e.checked()) }
                    " {state::tr(\"select all on this page\")}"
                }
            }
            button { class: "btn", disabled: prev_disabled, onclick: move |_| on_prev.call(()), {state::tr("‹ prev")} }
            span { "{text}" }
            button { class: "btn", disabled: next_disabled, onclick: move |_| on_next.call(()), {state::tr("next ›")} }
        }
    }
}
