//! The Android app's own parts: no drag and drop or system file dialogs, so File…, Folder…
//! and every Choose… open an in-app file browser; Cancel asks first; and a controller moves
//! focus over whatever is on screen. Adapted from chdlite's (chdlite/src/gui/android.rs).
//!
//! Included by `rommusic-gui.rs` only when building for Android.

use std::path::{Path, PathBuf};

use dioxus::prelude::*;

/// What a browser is choosing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Purpose {
    /// Inputs: any number of files.
    Files,
    /// An input folder.
    Folder,
    /// The output folder.
    Output,
    /// The folder of original-format music.
    Originals,
    /// The PS2 BIOS image: one file.
    Bios,
}

/// The browser's state, kept by the window so the controller's B can step back a folder.
#[derive(Clone, PartialEq, Debug)]
pub struct Browse {
    pub purpose: Purpose,
    /// `None`: the list of storage roots.
    pub dir: Option<PathBuf>,
    pub selected: Vec<PathBuf>,
}

impl Browse {
    pub fn new(purpose: Purpose) -> Self {
        Self { purpose, dir: None, selected: Vec::new() }
    }

    /// One level up; `false` when already at the roots (the browser then closes).
    pub fn back(&mut self) -> bool {
        let Some(dir) = self.dir.take() else { return false };
        let roots = rommusic::android::storage_roots();
        if !roots.iter().any(|(_, root)| *root == dir) {
            self.dir = dir.parent().map(Path::to_path_buf);
        }
        true
    }

    fn picks_files(&self) -> bool {
        matches!(self.purpose, Purpose::Files | Purpose::Bios)
    }

    /// Only one file: a new pick replaces the old.
    fn picks_one(&self) -> bool {
        self.purpose == Purpose::Bios
    }
}

#[derive(Clone, PartialEq)]
struct Entry {
    path: PathBuf,
    name: String,
    is_dir: bool,
}

/// A folder's subfolders, then (when picking files) its files, each sorted by name. Hidden
/// entries (a leading dot) are left out. Every file is shown: what rommusic reads is told by
/// its content as much as by its name.
fn list(dir: &Path, with_files: bool) -> Result<Vec<Entry>, String> {
    let read = std::fs::read_dir(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    let mut entries: Vec<Entry> = read
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            let path = entry.path();
            let is_dir = path.is_dir();
            if !is_dir && !with_files {
                return None;
            }
            Some(Entry { path, name, is_dir })
        })
        .collect();
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(entries)
}

/// The in-app file and folder browser. `on_done(Some(paths))` with the files chosen, or the
/// one folder; `on_done(None)` when backed out of.
#[component]
pub fn Browser(state: Signal<Option<Browse>>, on_done: EventHandler<Option<Vec<PathBuf>>>) -> Element {
    let Some(browse) = state() else { return rsx! {} };
    let files = browse.picks_files();
    let entries = browse.dir.as_deref().map(|dir| list(dir, files));
    let title = match browse.purpose {
        Purpose::Files => "Choose files".to_string(),
        Purpose::Folder => "Choose a folder".to_string(),
        Purpose::Output => "Output folder".to_string(),
        Purpose::Originals => "Folder of original-format music".to_string(),
        Purpose::Bios => "PS2 BIOS image".to_string(),
    };
    let here = browse.dir.as_ref().map(|dir| dir.display().to_string()).unwrap_or_else(|| "Storage".to_string());
    let chosen = browse.selected.len();

    // A new folder starts at its top, with the focus on its first entry.
    let dir_key = here.clone();
    use_effect(use_reactive!(|dir_key| {
        let _ = dir_key;
        let _ = document::eval("setTimeout(() => { const l = document.querySelector('.browser .entries'); if (l) { l.scrollTop = 0; const b = l.querySelector('button'); if (b) b.focus(); } }, 0);");
    }));

    rsx! {
        div { class: "overlay",
            div { class: "panel browser",
                h2 { "{title}" }
                div { class: "where", "{here}" }
                div { class: "entries",
                    match &entries {
                        None => rsx! {
                            for (name, root) in rommusic::android::storage_roots() {
                                {
                                    let key = root.display().to_string();
                                    rsx! {
                                        button {
                                            key: "{key}",
                                            class: "entry dir",
                                            onclick: move |_| {
                                                if let Some(b) = state.write().as_mut() {
                                                    b.dir = Some(root.clone());
                                                }
                                            },
                                            "▸ {name}"
                                        }
                                    }
                                }
                            }
                        },
                        Some(Err(err)) => rsx! {
                            div { class: "note", "{err}" }
                        },
                        Some(Ok(list)) => rsx! {
                            button {
                                class: "entry up",
                                onclick: move |_| {
                                    if let Some(b) = state.write().as_mut() {
                                        b.back();
                                    }
                                },
                                "▴ .."
                            }
                            if list.is_empty() {
                                div { class: "note", if files { "No folders or files here." } else { "No folders here." } }
                            }
                            for entry in list.iter().cloned() {
                                {
                                    let picked = browse.selected.contains(&entry.path);
                                    let Entry { path, name, is_dir } = entry;
                                    let key = path.display().to_string();
                                    let class = if is_dir { "entry dir" } else if picked { "entry file picked" } else { "entry file" };
                                    let label = if is_dir { format!("▸ {name}") } else if picked { format!("☑ {name}") } else { format!("☐ {name}") };
                                    rsx! {
                                        button {
                                            key: "{key}",
                                            class,
                                            onclick: move |_| {
                                                let mut guard = state.write();
                                                let Some(b) = guard.as_mut() else { return };
                                                if is_dir {
                                                    b.dir = Some(path.clone());
                                                } else if let Some(at) = b.selected.iter().position(|p| *p == path) {
                                                    b.selected.remove(at);
                                                } else if b.picks_one() {
                                                    b.selected = vec![path.clone()];
                                                } else {
                                                    b.selected.push(path.clone());
                                                }
                                            },
                                            "{label}"
                                        }
                                    }
                                }
                            }
                        },
                    }
                }
                div { class: "row",
                    span { class: "spacer" }
                    button { onclick: move |_| on_done.call(None), "Back" }
                    if files {
                        button {
                            class: "primary",
                            disabled: chosen == 0,
                            onclick: move |_| on_done.call(state().map(|b| b.selected)),
                            if chosen == 1 { "Use 1 file" } else { "Use {chosen} files" }
                        }
                    } else {
                        button {
                            class: "primary",
                            disabled: browse.dir.is_none(),
                            onclick: move |_| on_done.call(state().and_then(|b| b.dir).map(|dir| vec![dir])),
                            "Use this folder"
                        }
                    }
                }
            }
        }
    }
}

/// Asked before Cancel stops a run. `on_answer(true)` stops it.
#[component]
pub fn ConfirmCancel(on_answer: EventHandler<bool>) -> Element {
    use_effect(|| {
        let _ = document::eval("setTimeout(() => { const b = document.querySelector('.confirm button'); if (b) b.focus(); }, 0);");
    });
    rsx! {
        div { class: "overlay",
            div { class: "panel confirm",
                h2 { "Cancel the run?" }
                p { "The track being written is not kept; the tracks already written stay." }
                div { class: "row",
                    span { class: "spacer" }
                    button { onclick: move |_| on_answer.call(false), "Keep going" }
                    button { class: "danger", onclick: move |_| on_answer.call(true), "Cancel run" }
                }
            }
        }
    }
}

/// Controller and keyboard navigation, installed once.
///
/// The activity (`android/MainActivity.kt`) turns the controller's buttons into keys the page
/// sees: A is Enter, B and the system Back are Escape, Start is F2, Select is F3. The arrows
/// move the focus to the nearest control in that direction, within the topmost overlay or
/// else the window; on the log they scroll it. Escape, F2 and F3 go to Rust as "back",
/// "settings" and "console".
pub const NAV_JS: &str = r#"
const SEL = 'button:not([disabled]), input:not([disabled]), select:not([disabled])';
const layer = () => { const o = document.querySelectorAll('.overlay'); return o.length ? o[o.length - 1] : document.querySelector('.app'); };
const items = () => { const l = layer(); return l ? Array.from(l.querySelectorAll(SEL)).filter(e => e.offsetParent !== null) : []; };
const move = (key) => {
  const list = items();
  if (!list.length) return;
  const cur = document.activeElement;
  if (!list.includes(cur)) { list[0].focus(); return; }
  const r = cur.getBoundingClientRect();
  const cx = r.left + r.width / 2, cy = r.top + r.height / 2;
  let best = null, score = Infinity;
  for (const e of list) {
    if (e === cur) continue;
    const q = e.getBoundingClientRect();
    const dx = q.left + q.width / 2 - cx, dy = q.top + q.height / 2 - cy;
    let along, across;
    if (key === 'ArrowRight') { if (dx <= 1) continue; along = dx; across = dy; }
    else if (key === 'ArrowLeft') { if (dx >= -1) continue; along = -dx; across = dy; }
    else if (key === 'ArrowDown') { if (dy <= 1) continue; along = dy; across = dx; }
    else { if (dy >= -1) continue; along = -dy; across = dx; }
    const s = along + 2 * Math.abs(across);
    if (s < score) { score = s; best = e; }
  }
  if (best) { best.focus(); best.scrollIntoView({ block: 'nearest' }); }
};
document.addEventListener('keydown', (ev) => {
  const cur = document.activeElement;
  const k = ev.key;
  if (cur && cur.id === 'log') {
    if (k === 'ArrowUp' || k === 'ArrowDown') { cur.scrollBy(0, k === 'ArrowUp' ? -80 : 80); ev.preventDefault(); return; }
    if (k === 'Escape' || k === 'F3') { const m = document.querySelector('.app button:not([disabled])'); if (m) m.focus(); ev.preventDefault(); return; }
  }
  if (k.startsWith('Arrow')) {
    const typing = cur && cur.tagName === 'INPUT' && (cur.type === 'text' || cur.type === 'number');
    if (typing && (k === 'ArrowLeft' || k === 'ArrowRight')) return;
    move(k); ev.preventDefault(); return;
  }
  if (k === 'Enter' && cur) {
    if (cur.tagName === 'INPUT' && cur.type === 'checkbox') { cur.click(); ev.preventDefault(); return; }
    if (cur.tagName === 'SELECT') { try { cur.showPicker(); } catch (_) { cur.click(); } ev.preventDefault(); return; }
    return;
  }
  const name = { Escape: 'back', F2: 'settings', F3: 'console' }[k];
  if (name) { dioxus.send(name); ev.preventDefault(); }
});
setTimeout(() => { const m = document.querySelector('.app button:not([disabled])'); if (m) m.focus(); }, 0);
"#;
