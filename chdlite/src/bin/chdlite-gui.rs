//! chdlite's window: pick a mode, drop files.
//!
//! Everything it does goes through [`chdlite::batch`], always as worker processes, so Cancel
//! can stop a CHD creation part-way (chdman itself has no cancel hook).

// No console window behind the GUI in a release build; a debug build keeps one for panics.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use dioxus::desktop::wry::http::Response;
use dioxus::desktop::{use_asset_handler, Config, LogicalSize, WindowBuilder};
use dioxus::prelude::*;

use chdlite::batch::{self, Batch, BatchEvent, WORKER_FLAG};
use chdlite::classify::{expand, lossy_targets};
use chdlite::job::Mode;
use chdlite::settings::{CompMode, Jobs, LitePreset, Settings, CD_CODECS, CUE_STYLES, DVD_CODECS, HASHES, MAX_CODECS, RVZ_BLOCKS, RVZ_CODECS};

// Compiled in rather than shipped beside the executable, for the reason romexplorer-gui
// gives: `asset!()` needs `dx build`, and this is built with plain cargo.
const APP_CSS: &str = include_str!("../../assets/app.css");
const ICON_PNG: &[u8] = include_bytes!("../../assets/icon.png");
const ART: [(&str, &[u8]); 4] = [
    ("lite.png", include_bytes!("../../assets/lite.png")),
    ("comp.png", include_bytes!("../../assets/comp.png")),
    ("read.png", include_bytes!("../../assets/read.png")),
    ("hash.png", include_bytes!("../../assets/hash.png")),
];

const MODES: [(Mode, &str, &str, &str); 4] = [
    (Mode::Lite, "lite.png", "Lite", "Lite: fast. Disc image, .cso or .pbp → CHD / RVZ; Wii U / Xbox → WUA / ZAR / XISO. A .chd or .rvz is extracted."),
    (Mode::Comp, "comp.png", "Comp", "Comp: smallest file, slow. A .chd or .rvz is extracted."),
    (Mode::Read, "read.png", "Read", "Read: show what a file is."),
    (Mode::Hash, "hash.png", "Hash", "Hash: hash the disc inside a .chd / .rvz / .cso / .pbp, or the image itself."),
];

enum Ui {
    /// A drop, expanded into its inputs off the window's thread (a big folder takes a while).
    Expanded { with: Mode, inputs: Vec<PathBuf>, lossy: Vec<PathBuf> },
    Batch(BatchEvent),
    Done { failed: usize },
}

/// What the worker thread has to say, drained by the window a few times a second.
static EVENTS: Mutex<Vec<Ui>> = Mutex::new(Vec::new());
static CANCEL: AtomicBool = AtomicBool::new(false);
/// Paths given on the command line: files dropped onto `chdlite-gui` itself.
static STARTUP: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static STARTUP_ERROR: Mutex<Option<String>> = Mutex::new(None);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some(WORKER_FLAG) {
        std::process::exit(batch::worker_main(args.get(1).map(String::as_str).unwrap_or("")));
    }
    if let Err(err) = chdlite::natives::ensure() {
        *STARTUP_ERROR.lock().unwrap() = Some(err);
    }
    *STARTUP.lock().unwrap() = args.iter().map(PathBuf::from).filter(|path| path.exists()).collect();

    let mut window = WindowBuilder::new().with_title("chdlite").with_inner_size(LogicalSize::new(560.0, 760.0)).with_min_inner_size(LogicalSize::new(420.0, 520.0));
    if let Ok(decoded) = image::load_from_memory(ICON_PNG) {
        let rgba = decoded.to_rgba8();
        let (width, height) = rgba.dimensions();
        if let Ok(icon) = dioxus::desktop::tao::window::Icon::from_rgba(rgba.into_raw(), width, height) {
            window = window.with_window_icon(Some(icon));
        }
    }
    let cfg = Config::new().with_menu(None).with_window(window).with_background_color((0x14, 0x16, 0x1b, 0xff));
    dioxus::LaunchBuilder::desktop().with_cfg(cfg).launch(App);
}

#[derive(Clone, PartialEq)]
struct Line {
    class: &'static str,
    text: String,
}

fn line(class: &'static str, text: impl Into<String>) -> Line {
    Line { class, text: text.into() }
}

#[derive(Clone, Default, PartialEq)]
struct Run {
    names: Vec<String>,
    percents: Vec<f32>,
    done: usize,
}

impl Run {
    fn overall(&self) -> f32 {
        if self.percents.is_empty() {
            0.0
        } else {
            self.percents.iter().sum::<f32>() / self.percents.len() as f32
        }
    }
}

fn dropped(event: &DragEvent) -> Vec<PathBuf> {
    event.data().data_transfer().files().iter().map(|file| file.path()).collect()
}

#[component]
fn App() -> Element {
    let mut mode = use_signal(|| Mode::Lite);
    let mut settings = use_signal(Settings::load);
    let mut log = use_signal(|| vec![line("muted", "Pick a mode, then drop files or folders anywhere in this window.")]);
    let mut run = use_signal(Run::default);
    let mut running = use_signal(|| false);
    let mut show_settings = use_signal(|| false);
    let mut over = use_signal(|| None::<Mode>);
    let mut over_zone = use_signal(|| false);

    use_asset_handler("art", |request, responder| {
        let name = request.uri().path().rsplit('/').next().unwrap_or("");
        let response = match ART.iter().find(|(file, _)| *file == name) {
            Some((_, bytes)) => Response::builder().header("Content-Type", "image/png").body(bytes.to_vec()),
            None => Response::builder().status(404).body(Vec::new()),
        };
        if let Ok(response) = response {
            responder.respond(response);
        }
    });

    // A run that would make lossy outputs, waiting on the question.
    let mut asking = use_signal(|| None::<(Mode, Vec<PathBuf>, Vec<PathBuf>)>);
    // A drop being expanded.
    let mut preparing = use_signal(|| false);

    let mut run_now = move |with: Mode, inputs: Vec<PathBuf>| {
        mode.set(with);
        let names: Vec<String> = inputs.iter().map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).collect();
        log.write().push(line("muted", format!("── {} · {} file{} ──", with.label(), names.len(), if names.len() == 1 { "" } else { "s" })));
        run.set(Run { percents: vec![0.0; names.len()], names, done: 0 });
        running.set(true);
        CANCEL.store(false, Ordering::Relaxed);
        let batch = Batch { mode: with, inputs, settings: settings(), always_workers: true };
        std::thread::spawn(move || {
            let failed = batch::run(&batch, &CANCEL, &|event| EVENTS.lock().unwrap().push(Ui::Batch(event)));
            EVENTS.lock().unwrap().push(Ui::Done { failed });
        });
    };

    let mut start = move |with: Mode, paths: Vec<PathBuf>| {
        if paths.is_empty() {
            return;
        }
        if running() || preparing() || asking.read().is_some() {
            log.write().push(line("err", "Busy: wait for the current run, or cancel it."));
            return;
        }
        mode.set(with);
        preparing.set(true);
        let warn = matches!(with, Mode::Lite | Mode::Comp) && settings().output.lossy_warning;
        std::thread::spawn(move || {
            let inputs = expand(&paths);
            let lossy = if warn { lossy_targets(&inputs).into_iter().map(|(path, _)| path).collect() } else { Vec::new() };
            EVENTS.lock().unwrap().push(Ui::Expanded { with, inputs, lossy });
        });
    };

    // Worker events into the window, and whatever was dropped onto the executable itself.
    use_future(move || async move {
        if let Some(err) = STARTUP_ERROR.lock().unwrap().take() {
            log.write().push(line("err", err));
        }
        if let Some(problem) = Settings::problem() {
            log.write().push(line("err", format!("{problem}. Saving settings keeps the old file as .bak.")));
        }
        let startup = std::mem::take(&mut *STARTUP.lock().unwrap());
        start(Mode::Lite, startup);
        loop {
            let events = std::mem::take(&mut *EVENTS.lock().unwrap());
            for event in events {
                match event {
                    Ui::Expanded { with, inputs, lossy } => {
                        preparing.set(false);
                        if inputs.is_empty() {
                            log.write().push(line("err", "Nothing to do: no disc images, .chd, .rvz, .cso or .pbp in what was dropped."));
                        } else if !lossy.is_empty() {
                            asking.set(Some((with, inputs, lossy)));
                        } else {
                            run_now(with, inputs);
                        }
                    }
                    Ui::Batch(BatchEvent::Started { .. }) => {}
                    Ui::Batch(BatchEvent::Progress { index, percent }) => {
                        if let Some(slot) = run.write().percents.get_mut(index) {
                            *slot = percent.clamp(0.0, 99.9);
                        }
                    }
                    Ui::Batch(BatchEvent::Message { index, text }) => {
                        let name = run.read().names.get(index).cloned().unwrap_or_default();
                        log.write().push(line("", format!("{name}: {text}")));
                    }
                    Ui::Batch(BatchEvent::Finished { index, result }) => {
                        let name = run.read().names.get(index).cloned().unwrap_or_default();
                        {
                            let mut run = run.write();
                            if let Some(slot) = run.percents.get_mut(index) {
                                *slot = 100.0;
                            }
                            run.done += 1;
                        }
                        log.write().push(match result {
                            Ok(text) if text.contains('\n') => line("ok", text),
                            Ok(text) => line("ok", format!("{name}: {text}")),
                            Err(err) => line("err", format!("{name}: FAILED: {err}")),
                        });
                    }
                    Ui::Done { failed } => {
                        running.set(false);
                        let total = run.read().names.len();
                        log.write().push(if failed == 0 { line("ok", format!("Done: {total} of {total}.")) } else { line("err", format!("Completed with errors: {failed} of {total} failed.")) });
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        }
    });

    // Keep the newest log line in view.
    use_effect(move || {
        let _ = log.read().len();
        let _ = document::eval("const l = document.getElementById('log'); if (l) l.scrollTop = l.scrollHeight;");
    });

    let current = mode();
    let hint = MODES.iter().find(|(m, ..)| *m == current).map(|(.., hint)| *hint).unwrap_or("");
    let snapshot = run();
    let overall = snapshot.overall();
    let status = if snapshot.names.is_empty() { String::new() } else { format!("{:.0}%  ·  {}/{}", overall, snapshot.done, snapshot.names.len()) };
    let output_dir = settings().output.dir.clone();

    rsx! {
        document::Title { "chdlite" }
        document::Style { {APP_CSS} }
        div {
            class: "app",
            // Anywhere in the window is a drop target for the selected mode.
            ondragover: move |event| event.prevent_default(),
            ondrop: move |event| {
                event.prevent_default();
                over.set(None);
                over_zone.set(false);
                start(mode(), dropped(&event));
            },
            div { class: "top",
                h1 { "chdlite" }
                span { class: "ver", "{chdlite::VERSION}" }
                span { class: "spacer" }
                button { onclick: move |_| show_settings.set(true), "⚙ Settings" }
            }
            div { class: "modes",
                for (m, art, label, tip) in MODES {
                    button {
                        key: "{label}",
                        class: if m == current { "mode active" } else { "mode" },
                        class: if over() == Some(m) { "over" },
                        title: "{tip}",
                        onclick: move |_| mode.set(m),
                        ondragover: move |event| { event.prevent_default(); event.stop_propagation(); over.set(Some(m)); },
                        ondragleave: move |_| over.set(None),
                        // Dropping straight onto a mode selects it and runs it.
                        ondrop: move |event| {
                            event.prevent_default();
                            event.stop_propagation();
                            over.set(None);
                            start(m, dropped(&event));
                        },
                        img { src: "/art/{art}", alt: "{label}" }
                        span { "{label}" }
                    }
                }
            }
            div { class: "hint", "{hint}" }
            div {
                class: if over_zone() { "drop over" } else { "drop" },
                ondragover: move |event| { event.prevent_default(); over_zone.set(true); },
                ondragleave: move |_| over_zone.set(false),
                onclick: move |_| {
                    spawn(async move {
                        if let Some(files) = rfd::AsyncFileDialog::new().set_title("Choose files").pick_files().await {
                            start(mode(), files.iter().map(|f| f.path().to_path_buf()).collect());
                        }
                    });
                },
                b { "Drop files or folders here" }
                "CD → CD CHD · DVD → DVD CHD · GameCube / Wii → RVZ · .cso → DVD CHD · .pbp → CD CHD · Wii U + key → WUA · Xbox 360 → ZAR · Xbox → XISO — or click to choose files"
            }
            div { class: "row",
                label { "Output" }
                input {
                    r#type: "text",
                    placeholder: "next to each input",
                    value: "{output_dir}",
                    oninput: move |event| settings.write().output.dir = event.value(),
                    onchange: move |_| { let _ = settings.read().save(); },
                }
                button {
                    onclick: move |_| {
                        spawn(async move {
                            if let Some(folder) = rfd::AsyncFileDialog::new().set_title("Output folder").pick_folder().await {
                                settings.write().output.dir = folder.path().to_string_lossy().into_owned();
                                let _ = settings.read().save();
                            }
                        });
                    },
                    "Open…"
                }
            }
            div { class: "row",
                div { class: "bar", style: "flex:1", div { style: "width:{overall}%" } }
                span { class: "status", "{status}" }
                button { class: "danger", disabled: !running(), onclick: move |_| CANCEL.store(true, Ordering::Relaxed), "Cancel" }
            }
            div { class: "log", id: "log",
                for (index, entry) in log().into_iter().enumerate() {
                    div { key: "{index}", class: "{entry.class}", "{entry.text}" }
                }
            }
            if let Some((with, inputs, lossy)) = asking() {
                LossyAsk {
                    lossy: lossy.iter().map(|path| {
                        let target = chdlite::classify::classify(path).ok().and_then(|input| match input { chdlite::classify::Input::Image { target, .. } => Some(target.label()), _ => None }).unwrap_or("");
                        format!("{} → {target}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
                    }).collect::<Vec<_>>(),
                    rest: inputs.len() - lossy.len(),
                    on_answer: move |(answer, dont_ask): (Answer, bool)| {
                        asking.set(None);
                        if dont_ask {
                            settings.write().output.lossy_warning = false;
                            if let Err(err) = settings.read().save() {
                                log.write().push(line("err", format!("Settings not saved: {err}")));
                            }
                        }
                        let chosen: Vec<PathBuf> = match answer {
                            Answer::All => inputs.clone(),
                            Answer::Lossless => inputs.iter().filter(|path| !lossy.contains(path)).cloned().collect(),
                            Answer::Nothing => Vec::new(),
                        };
                        if chosen.is_empty() {
                            log.write().push(line("muted", "Not converted."));
                        } else {
                            run_now(with, chosen);
                        }
                    },
                }
            }
            if show_settings() {
                SettingsPanel {
                    initial: settings(),
                    on_close: move |saved: Option<Settings>| {
                        if let Some(saved) = saved {
                            match saved.save() {
                                Ok(path) => log.write().push(line("muted", format!("Settings saved to {}", path.display()))),
                                Err(err) => log.write().push(line("err", format!("Settings not saved: {err}"))),
                            }
                            settings.set(saved);
                        }
                        show_settings.set(false);
                    },
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Answer {
    /// Everything dropped, the lossy ones included.
    All,
    /// Only what keeps the disc.
    Lossless,
    Nothing,
}

/// Asked before a run that would make WUA, ZAR or XISO files. `on_answer(answer, dont_ask)`.
#[component]
fn LossyAsk(lossy: Vec<String>, rest: usize, on_answer: EventHandler<(Answer, bool)>) -> Element {
    let mut dont_ask = use_signal(|| false);
    rsx! {
        div { class: "overlay",
            div { class: "panel",
                h2 { "Lossy format" }
                p { "These keep the game's files but not the disc, so they can't be turned back into the original image. Not recommended for bit-perfect archiving. Convert anyway?" }
                div { class: "log",
                    for (index, name) in lossy.iter().enumerate() {
                        div { key: "{index}", "{name}" }
                    }
                }
                div { class: "row",
                    div { class: "checks",
                        label {
                            input { r#type: "checkbox", checked: dont_ask(), onchange: move |event| dont_ask.set(event.checked()) }
                            "Don't ask again"
                        }
                    }
                }
                div { class: "row",
                    span { class: "spacer" }
                    button { onclick: move |_| on_answer.call((Answer::Nothing, false)), "Cancel" }
                    if rest > 0 {
                        button { title: "Convert the other {rest} and leave these as they are", onclick: move |_| on_answer.call((Answer::Lossless, false)), "Skip these" }
                    }
                    button { class: "primary", onclick: move |_| on_answer.call((Answer::All, dont_ask())), "Convert anyway" }
                }
            }
        }
    }
}

fn number(text: &str) -> u32 {
    text.trim().parse().unwrap_or(0)
}

#[component]
fn Codecs(label: String, all: Vec<&'static str>, chosen: Vec<String>, disabled: bool, on_change: EventHandler<Vec<String>>) -> Element {
    rsx! {
        div { class: "row",
            label { "{label}" }
            div { class: "checks",
                for codec in all {
                    label {
                        key: "{codec}",
                        input {
                            r#type: "checkbox",
                            disabled,
                            checked: chosen.iter().any(|c| c == codec),
                            onchange: {
                                let chosen = chosen.clone();
                                move |event: FormEvent| {
                                    let mut next: Vec<String> = chosen.iter().filter(|c| *c != codec).cloned().collect();
                                    if event.checked() && next.len() < MAX_CODECS {
                                        next.push(codec.to_string());
                                    }
                                    on_change.call(next);
                                }
                            },
                        }
                        "{codec}"
                    }
                }
            }
        }
    }
}

#[component]
fn SettingsPanel(initial: Settings, on_close: EventHandler<Option<Settings>>) -> Element {
    let mut draft = use_signal(|| initial);
    let s = draft();
    let custom = s.comp.mode == CompMode::Custom;
    rsx! {
        div { class: "overlay",
            div { class: "panel",
                h2 { "Settings" }

                h3 { "Lite — fast create" }
                div { class: "row",
                    label { "Preset" }
                    select {
                        onchange: move |event| draft.write().lite.preset = if event.value() == "mobile" { LitePreset::Mobile } else { LitePreset::Pc },
                        option { value: "pc", selected: s.lite.preset == LitePreset::Pc, "PC (cdzs+cdfl / zstd)" }
                        option { value: "mobile", selected: s.lite.preset == LitePreset::Mobile, "Mobile (PS2 uses zlib)" }
                    }
                }

                h3 { "Comp — smallest file" }
                div { class: "row",
                    label { "Codecs" }
                    select {
                        onchange: move |event| {
                            draft.write().comp.mode = match event.value().as_str() {
                                "chdman" => CompMode::Chdman,
                                "custom" => CompMode::Custom,
                                _ => CompMode::Best,
                            }
                        },
                        option { value: "best", selected: s.comp.mode == CompMode::Best, "Best (cdlz+cdzs+cdzl+cdfl / lzma+zstd+zlib+huff)" }
                        option { value: "chdman", selected: s.comp.mode == CompMode::Chdman, "chdman default" }
                        option { value: "custom", selected: custom, "Custom" }
                    }
                }
                Codecs { label: "CD codecs", all: CD_CODECS.to_vec(), chosen: s.comp.cd_codecs.clone(), disabled: !custom, on_change: move |next| draft.write().comp.cd_codecs = next }
                Codecs { label: "DVD codecs", all: DVD_CODECS.to_vec(), chosen: s.comp.dvd_codecs.clone(), disabled: !custom, on_change: move |next| draft.write().comp.dvd_codecs = next }
                div { class: "row",
                    label { "CD hunk size" }
                    input { r#type: "number", min: "0", step: "2448", disabled: !custom, value: "{s.comp.cd_hunk_size}", oninput: move |event| draft.write().comp.cd_hunk_size = number(&event.value()) }
                    label { "DVD hunk size" }
                    input { r#type: "number", min: "0", step: "2048", disabled: !custom, value: "{s.comp.dvd_hunk_size}", oninput: move |event| draft.write().comp.dvd_hunk_size = number(&event.value()) }
                }
                div { class: "note", "Custom: up to {MAX_CODECS} codecs; hunk size in bytes, 0 = chdman's choice (a CD hunk is a multiple of 2448)." }

                h3 { "RVZ — GameCube / Wii" }
                div { class: "row",
                    label { "Codec" }
                    select {
                        onchange: move |event| draft.write().rvz.codec = event.value(),
                        for codec in RVZ_CODECS {
                            option { key: "{codec}", value: "{codec}", selected: s.rvz.codec == codec, "{codec}" }
                        }
                    }
                    label { "Level" }
                    input { r#type: "number", min: "1", max: "22", value: "{s.rvz.level}", oninput: move |event| draft.write().rvz.level = number(&event.value()) as i32 }
                }
                div { class: "row",
                    label { "Block size" }
                    select {
                        onchange: move |event| draft.write().rvz.block = event.value(),
                        for (name, _) in RVZ_BLOCKS {
                            option { key: "{name}", value: "{name}", selected: s.rvz.block == name, "{name}" }
                        }
                    }
                    div { class: "checks",
                        label {
                            input { r#type: "checkbox", checked: s.rvz.scrub, onchange: move |event| draft.write().rvz.scrub = event.checked() }
                            "Remove junk data (not reversible)"
                        }
                    }
                }

                h3 { "Extract and hash" }
                div { class: "row",
                    label { title: "The extracted sheet's dialect, and a GD-ROM's sheet: redump writes a .cue, chdman a .gdi", "Cue style" }
                    select {
                        onchange: move |event| draft.write().extract.cue_style = event.value(),
                        for style in CUE_STYLES {
                            option { key: "{style}", value: "{style}", selected: s.extract.cue_style == style, "{style}" }
                        }
                    }
                }
                Codecs { label: "Hashes", all: HASHES.to_vec(), chosen: s.hash.algorithms.clone(), disabled: false, on_change: move |next| draft.write().hash.algorithms = next }

                h3 { "Performance and output" }
                {
                    let threads_custom = s.jobs.threads > 0;
                    let files_custom = s.jobs.concurrent_files > 0;
                    let cores = if threads_custom { s.jobs.threads as usize } else { Jobs::auto_threads() };
                    let auto_files = Jobs::auto_files(cores);
                    let auto_threads = Jobs::auto_threads();
                    rsx! {
                        div { class: "row",
                            label { "Threads" }
                            select {
                                onchange: move |event| draft.write().jobs.threads = if event.value() == "custom" { auto_threads as u32 } else { 0 },
                                option { value: "auto", selected: !threads_custom, "Auto ({auto_threads}, every core)" }
                                option { value: "custom", selected: threads_custom, "Custom" }
                            }
                            input { r#type: "number", min: "1", max: "256", disabled: !threads_custom, value: "{cores}", oninput: move |event| draft.write().jobs.threads = number(&event.value()).max(1) }
                        }
                        div { class: "row",
                            label { "Files at once" }
                            select {
                                onchange: move |event| draft.write().jobs.concurrent_files = if event.value() == "custom" { auto_files as u32 } else { 0 },
                                option { value: "auto", selected: !files_custom, "Auto ({auto_files} here)" }
                                option { value: "custom", selected: files_custom, "Custom" }
                            }
                            input { r#type: "number", min: "1", max: "16", disabled: !files_custom, value: if files_custom { "{s.jobs.concurrent_files}" } else { "{auto_files}" }, oninput: move |event| draft.write().jobs.concurrent_files = number(&event.value()).clamp(1, 16) }
                        }
                        div { class: "note",
                            "Dropped files are converted this many at a time, each in its own process, with the threads shared between them. "
                            "Auto: a quarter of the threads, at most 4 — chdman and Dolphin already use every thread they get for one file, so more files at once mostly fills the gaps where a file is reading or finishing. "
                            "More helps with many small images; on a hard disk, fewer is faster."
                        }
                    }
                }
                div { class: "row",
                    label { "" }
                    div { class: "checks",
                        label {
                            input { r#type: "checkbox", checked: s.output.force, onchange: move |event| draft.write().output.force = event.checked() }
                            "Replace outputs that already exist"
                        }
                        label {
                            input { r#type: "checkbox", checked: s.output.lossy_warning, onchange: move |event| draft.write().output.lossy_warning = event.checked() }
                            "Ask before a lossy output (WUA, ZAR, XISO)"
                        }
                    }
                }

                div { class: "row",
                    span { class: "spacer" }
                    button { onclick: move |_| on_close.call(None), "Cancel" }
                    button { class: "primary", onclick: move |_| on_close.call(Some(draft())), "Save" }
                }
            }
        }
    }
}
