//! rommusic's window: choose ROMs, disc images, folders or music files, FLAC or the original
//! audio, an output folder, and Start. The work is the library's ([`rommusic::process`]), run
//! on a thread of its own; its events come back to the window a few times a second.
//!
//! The same window is the Android app (`dx build --android`, see `rommusic-cli/Dioxus.toml`),
//! with the differences kept behind `cfg(target_os = "android")`: no drag and drop or system
//! file dialogs, so File… and Folder… open an in-app browser (`src/gui/android.rs`); Cancel
//! asks first; a controller moves the focus; and a run holds a foreground service so Android
//! does not stop it.

// No console window behind the GUI in a release build; a debug build keeps one for panics.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
use dioxus::html::HasFileData;
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use rommusic::{Event, Format, Options};

#[cfg(target_os = "android")]
#[path = "../gui/android.rs"]
mod android_ui;
#[cfg(target_os = "android")]
use android_ui::{Browse, Browser, ConfirmCancel, Purpose};

const ANDROID: bool = cfg!(target_os = "android");

// Compiled in rather than shipped beside the executable: `asset!()` needs `dx build`, and the
// desktop window is built with plain cargo.
const APP_CSS: &str = include_str!("../../assets/app.css");
// The window's icon (assets/icon.svg, rendered). Android draws the launcher icon itself.
#[cfg_attr(target_os = "android", allow(dead_code))]
const ICON_PNG: &[u8] = include_bytes!("../../assets/icon.png");

// --- settings -----------------------------------------------------------------------------

/// What the window remembers between runs, as JSON in the per-user config folder (the app's
/// own files folder on Android).
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    /// "flac", "original" or "both".
    format: String,
    /// Empty: a "<name> (music)" folder beside each input.
    out_dir: String,
    /// Jingles, sound effects and movie sound too.
    all: bool,
    /// "native", "cd", "dvd" or "hires".
    rate: String,
    /// A folder of music in original audio formats, to match cartridges against.
    originals: String,
    /// A PS2 BIOS image of the user's own, for PS1 and PS2 music.
    ps2_bios: String,
    /// SNES: seconds the game runs before its music is taken.
    seconds: u32,
    /// SNES: press Start now and then while it runs.
    press_start: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { format: "flac".into(), out_dir: String::new(), all: false, rate: "native".into(), originals: String::new(), ps2_bios: String::new(), seconds: 20, press_start: false }
    }
}

const RATES: [(&str, &str); 4] = [("native", "As stored"), ("cd", "CD (44.1 kHz, 16-bit)"), ("dvd", "DVD (48 kHz, 24-bit)"), ("hires", "Hi-res (96 kHz, 24-bit)")];
const FORMATS: [(&str, &str); 3] = [("flac", "FLAC"), ("original", "Original audio"), ("both", "Both")];

impl Settings {
    fn path() -> PathBuf {
        config_dir().join("rommusic-gui.json")
    }

    fn load() -> Self {
        std::fs::read(Self::path()).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
    }

    fn save(&self) -> Result<PathBuf, String> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|err| err.to_string())?;
        std::fs::write(&path, text).map_err(|err| format!("{}: {err}", path.display()))?;
        Ok(path)
    }

    fn format(&self) -> Format {
        match self.format.as_str() {
            "original" => Format::Original,
            "both" => Format::Both,
            _ => Format::Flac,
        }
    }

    fn options(&self) -> Options {
        let (rate, bits) = match self.rate.as_str() {
            "cd" => (Some(44_100), 16),
            "dvd" => (Some(48_000), 24),
            "hires" => (Some(96_000), 24),
            _ => (None, 16),
        };
        let path = |text: &str| (!text.trim().is_empty()).then(|| PathBuf::from(text.trim()));
        Options {
            all: self.all,
            max: None,
            out: path(&self.out_dir),
            rate,
            bits,
            format: self.format(),
            originals: path(&self.originals).into_iter().collect(),
            ps2_bios: path(&self.ps2_bios),
            seconds: self.seconds.clamp(1, 600) as f32,
            press_start: self.press_start,
        }
    }
}

/// The per-user config folder by each platform's convention, or the Android app's files.
fn config_dir() -> PathBuf {
    #[cfg(target_os = "android")]
    {
        rommusic::android::files_dir()
    }
    #[cfg(not(target_os = "android"))]
    {
        let env = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
        let home = || env("HOME").unwrap_or_else(std::env::temp_dir);
        let base = if cfg!(windows) {
            env("APPDATA").unwrap_or_else(std::env::temp_dir)
        } else if cfg!(target_os = "macos") {
            home().join("Library").join("Application Support")
        } else {
            env("XDG_CONFIG_HOME").unwrap_or_else(|| home().join(".config"))
        };
        base.join("rommusic")
    }
}

// --- the worker ---------------------------------------------------------------------------

enum Ui {
    Event(Event),
    /// An input finished: `Ok`, or why not.
    InputDone { index: usize, result: Result<(), String> },
    /// romexplorer's own warnings, for the log.
    Log(String),
    Done { failed: usize, cancelled: bool },
}

/// What the worker thread has to say, drained by the window a few times a second.
static EVENTS: Mutex<Vec<Ui>> = Mutex::new(Vec::new());
static CANCEL: AtomicBool = AtomicBool::new(false);
/// Paths given on the command line: files dropped onto `rommusic-gui` itself.
static STARTUP: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static STARTUP_ERROR: Mutex<Option<String>> = Mutex::new(None);

fn push(ui: Ui) {
    EVENTS.lock().unwrap().push(ui);
}

/// romexplorer's warnings (a file it could not read, a track it skipped) into the log.
struct Logger;
static LOGGER: Logger = Logger;

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.target().starts_with("romexplorer") && m.level() <= log::Level::Warn
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            push(Ui::Log(format!("({}) {}", r.level().as_str().to_lowercase(), r.args())));
        }
    }
    fn flush(&self) {}
}

fn work(inputs: Vec<PathBuf>, o: Options) {
    if let Some(bios) = &o.ps2_bios {
        match rommusic::load_ps2_bios(bios) {
            Ok(true) => {}
            Ok(false) => push(Ui::Log("This build has no PS1/PS2 player; the PS2 BIOS is not used.".into())),
            Err(err) => push(Ui::Log(format!("PS2 BIOS not loaded: {err}"))),
        }
    }
    let library = rommusic::load_library(&o.originals, &mut |event| push(Ui::Event(event)));
    let mut failed = 0;
    let mut cancelled = false;
    for (index, input) in inputs.iter().enumerate() {
        if CANCEL.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        let result = rommusic::process(input, &o, library.as_ref(), &CANCEL, &mut |event| push(Ui::Event(event)));
        if result.as_ref().err().map(String::as_str) == Some(rommusic::CANCELLED) {
            cancelled = true;
            push(Ui::InputDone { index, result });
            break;
        }
        failed += usize::from(result.is_err());
        push(Ui::InputDone { index, result });
    }
    push(Ui::Done { failed, cancelled });
}

// --- the window ---------------------------------------------------------------------------

fn main() {
    #[cfg(target_os = "android")]
    rommusic::android::mark_app();
    // On Android `main` is called from inside the app's process with no command line, and
    // reading the arguments crashed there (chdlite found this): there are none to read.
    let args: Vec<String> = if ANDROID { Vec::new() } else { std::env::args().skip(1).collect() };
    #[cfg(target_os = "android")]
    if let Err(err) = rommusic::natives::ensure() {
        *STARTUP_ERROR.lock().unwrap() = Some(err);
    }
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(log::LevelFilter::Warn);
    *STARTUP.lock().unwrap() = args.iter().map(PathBuf::from).filter(|path| path.exists()).collect();

    #[allow(unused_mut)]
    let mut window = WindowBuilder::new().with_title("rommusic").with_inner_size(LogicalSize::new(560.0, 700.0)).with_min_inner_size(LogicalSize::new(420.0, 480.0));
    #[cfg(not(target_os = "android"))]
    if let Ok(decoded) = image::load_from_memory(ICON_PNG) {
        let rgba = decoded.to_rgba8();
        let (width, height) = rgba.dimensions();
        if let Ok(icon) = dioxus::desktop::tao::window::Icon::from_rgba(rgba.into_raw(), width, height) {
            window = window.with_window_icon(Some(icon));
        }
    }
    let cfg = Config::new().with_menu(None).with_window(window).with_background_color((0x14, 0x16, 0x1b, 0xff));
    // The webview's own data (cache, local storage) beside the settings, not wherever the
    // platform's default would put it.
    #[cfg(not(target_os = "android"))]
    let cfg = cfg.with_data_directory(config_dir().join("webview"));
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

fn name_of(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
}

/// Where a run is, for the bar and the status line.
#[derive(Clone, Default, PartialEq)]
struct Run {
    inputs: usize,
    /// Inputs finished.
    done: usize,
    /// The input being read, 0-based.
    current: usize,
    tracks: usize,
    track: usize,
    track_name: String,
    fraction: f32,
    /// What it is doing when it is not writing a track.
    doing: String,
}

impl Run {
    fn overall(&self) -> f32 {
        if self.inputs == 0 {
            return 0.0;
        }
        let within = if self.tracks == 0 { 0.0 } else { (self.track.saturating_sub(1) as f32 + self.fraction) / self.tracks as f32 };
        ((self.done as f32 + within.min(1.0)) / self.inputs as f32 * 100.0).min(100.0)
    }

    fn status(&self) -> String {
        if self.inputs == 0 {
            return String::new();
        }
        let input = format!("{} of {}", (self.current + 1).min(self.inputs), self.inputs);
        if self.track > 0 && !self.track_name.is_empty() {
            format!("{input} · track {} of {}: {}", self.track, self.tracks, self.track_name)
        } else if !self.doing.is_empty() {
            format!("{input} · {}", self.doing)
        } else {
            input
        }
    }
}

/// The dropped paths, from the window's native drop event.
fn dropped(event: &DragEvent) -> Vec<PathBuf> {
    event.data().files().iter().map(|file| file.path()).collect()
}

#[component]
fn App() -> Element {
    let mut settings = use_signal(Settings::load);
    let mut inputs = use_signal(Vec::<PathBuf>::new);
    let mut log = use_signal(|| vec![line("muted", if ANDROID { "Choose files or a folder, then Start." } else { "Drop ROMs, disc images, folders or music files here, or choose them, then Start." })]);
    let mut run = use_signal(Run::default);
    let mut running = use_signal(|| false);
    let mut show_options = use_signal(|| false);
    let mut over = use_signal(|| false);
    #[cfg(target_os = "android")]
    let mut browse = use_signal(|| None::<Browse>);
    #[cfg(target_os = "android")]
    let mut confirm_cancel = use_signal(|| false);
    #[cfg(target_os = "android")]
    let mut file_access = use_signal(rommusic::android::has_file_access);

    let mut add_inputs = move |paths: Vec<PathBuf>| {
        if running() {
            log.write().push(line("err", "Busy: wait for the current run, or cancel it."));
            return;
        }
        let mut list = inputs.write();
        for path in paths {
            if !list.contains(&path) {
                list.push(path);
            }
        }
    };

    let mut start = move || {
        let chosen = inputs();
        if chosen.is_empty() || running() {
            return;
        }
        let s = settings();
        let _ = s.save();
        let o = s.options();
        log.write().push(line("muted", format!("── {} · {} input{} ──", s.format().label(), chosen.len(), if chosen.len() == 1 { "" } else { "s" })));
        run.set(Run { inputs: chosen.len(), ..Run::default() });
        running.set(true);
        CANCEL.store(false, Ordering::Relaxed);
        #[cfg(target_os = "android")]
        rommusic::android::run_started(&format!("rommusic · {} input{}", chosen.len(), if chosen.len() == 1 { "" } else { "s" }));
        std::thread::spawn(move || work(chosen, o));
    };

    // Worker events into the window, and whatever was dropped onto the executable itself.
    use_future(move || async move {
        if let Some(err) = STARTUP_ERROR.lock().unwrap().take() {
            log.write().push(line("err", err));
        }
        if !rommusic::libraries_available() {
            log.write().push(line("err", "The music libraries (rommusic, rvgmstream) were not found: nothing can be played. They belong in lib/ beside rommusic-gui."));
        }
        let startup = std::mem::take(&mut *STARTUP.lock().unwrap());
        add_inputs(startup);
        #[cfg(target_os = "android")]
        if !file_access() {
            log.write().push(line("err", "rommusic needs All files access to read your ROMs and write the music: tap Allow above."));
        }
        #[cfg(target_os = "android")]
        let mut tick = 0u32;
        loop {
            let events = std::mem::take(&mut *EVENTS.lock().unwrap());
            for event in events {
                match event {
                    Ui::Event(Event::Library { done, total }) => {
                        run.write().doing = format!("reading original-format music, {done} of {total} files");
                    }
                    Ui::Event(Event::Reading { input }) => {
                        let mut r = run.write();
                        r.tracks = 0;
                        r.track = 0;
                        r.track_name.clear();
                        r.fraction = 0.0;
                        r.doing = format!("looking for music in {}", name_of(&input));
                    }
                    Ui::Event(Event::Found { input, folder, tracks }) => {
                        {
                            let mut r = run.write();
                            r.tracks = tracks;
                            r.doing.clear();
                        }
                        log.write().push(line("", format!("{}: {tracks} track{} → {}", name_of(&input), if tracks == 1 { "" } else { "s" }, folder.display())));
                    }
                    Ui::Event(Event::TrackStarted { number, total, name }) => {
                        let mut r = run.write();
                        r.track = number;
                        r.tracks = total;
                        r.track_name = name;
                        r.fraction = 0.0;
                    }
                    Ui::Event(Event::TrackProgress { number, fraction, .. }) => {
                        let mut r = run.write();
                        if r.track == number {
                            r.fraction = fraction;
                        }
                    }
                    Ui::Event(Event::TrackWritten { number, total, name, ms }) => {
                        run.write().fraction = 1.0;
                        let width = if total >= 100 { 3 } else { 2 };
                        log.write().push(line("ok", format!("  {number:0width$} {name}  {}", rommusic::length(ms))));
                    }
                    Ui::Event(Event::TrackFailed { number, total, name, error }) => {
                        let width = if total >= 100 { 3 } else { 2 };
                        log.write().push(line("err", format!("  {number:0width$} {name}: failed: {error}")));
                    }
                    Ui::Event(Event::OriginalsWritten { count, folder }) => {
                        log.write().push(line("ok", format!("  {count} original file{} in {}", if count == 1 { "" } else { "s" }, folder.display())));
                    }
                    Ui::Event(Event::NoOriginals) => {
                        log.write().push(line("muted", "  No original files: CD audio and music sequenced in a cartridge have none."));
                    }
                    Ui::Log(text) => log.write().push(line("muted", text)),
                    Ui::InputDone { index, result } => {
                        let name = inputs.peek().get(index).map(|p| name_of(p)).unwrap_or_default();
                        {
                            let mut r = run.write();
                            r.done += 1;
                            r.current = (index + 1).min(r.inputs.saturating_sub(1));
                            r.track = 0;
                            r.tracks = 0;
                            r.track_name.clear();
                        }
                        match result {
                            Ok(()) => {}
                            Err(err) if err == rommusic::CANCELLED => log.write().push(line("err", format!("{name}: cancelled; the track being written was removed."))),
                            Err(err) => log.write().push(line("err", format!("{name}: {err}"))),
                        }
                    }
                    Ui::Done { failed, cancelled } => {
                        running.set(false);
                        #[cfg(target_os = "android")]
                        rommusic::android::run_finished();
                        let total = run.peek().inputs;
                        let finished = run.peek().done;
                        run.write().doing.clear();
                        log.write().push(if cancelled {
                            line("err", format!("Cancelled after {finished} of {total}."))
                        } else if failed == 0 {
                            line("ok", format!("Done: {total} of {total}."))
                        } else {
                            line("err", format!("Completed with errors: {failed} of {total} failed."))
                        });
                    }
                }
            }
            // The notification's progress about once a second, and whether access was
            // granted in the system screen meanwhile.
            #[cfg(target_os = "android")]
            {
                tick = tick.wrapping_add(1);
                if *running.peek() && tick % 12 == 0 {
                    let r = run.peek();
                    rommusic::android::run_progress(r.overall(), &r.status());
                }
                if !*file_access.peek() && tick % 20 == 0 && rommusic::android::has_file_access() {
                    file_access.set(true);
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        }
    });

    // The controller and the Back key: what is on top closes first.
    #[cfg(target_os = "android")]
    use_future(move || async move {
        let mut keys = document::eval(android_ui::NAV_JS);
        while let Ok(key) = keys.recv::<String>().await {
            let overlay = confirm_cancel() || show_options() || browse.peek().is_some();
            match key.as_str() {
                "back" => {
                    if confirm_cancel() {
                        confirm_cancel.set(false);
                    } else if browse.peek().is_some() {
                        let stays = browse.write().as_mut().is_some_and(|b| b.back());
                        if !stays {
                            browse.set(None);
                        }
                    } else if show_options() {
                        show_options.set(false);
                    } else {
                        rommusic::android::leave();
                    }
                }
                "settings" if !overlay => show_options.set(true),
                "console" if !overlay => {
                    let _ = document::eval("const l = document.getElementById('log'); if (l) l.focus();");
                }
                _ => {}
            }
        }
    });

    // Keep the newest log line in view.
    use_effect(move || {
        let _ = log.read().len();
        let _ = document::eval("const l = document.getElementById('log'); if (l) l.scrollTop = l.scrollHeight;");
    });

    let snapshot = run();
    let overall = snapshot.overall();
    let status = snapshot.status();
    let s = settings();
    let chosen = inputs();

    #[cfg(target_os = "android")]
    let needs_access = !file_access();
    #[cfg(not(target_os = "android"))]
    let needs_access = false;

    // Opens a browser (Android) or the system dialog (desktop) for each thing to choose.
    #[allow(unused_mut)] // Android's branch sets a signal
    let mut choose = move |what: Choose| {
        #[cfg(target_os = "android")]
        browse.set(Some(Browse::new(match what {
            Choose::Files => Purpose::Files,
            Choose::Folder => Purpose::Folder,
            Choose::Output => Purpose::Output,
            Choose::Originals => Purpose::Originals,
            Choose::Bios => Purpose::Bios,
        })));
        #[cfg(not(target_os = "android"))]
        spawn(async move {
            let dialog = rfd::AsyncFileDialog::new();
            match what {
                Choose::Files => {
                    if let Some(files) = dialog.set_title("Choose ROMs, disc images or music files").pick_files().await {
                        add_inputs(files.iter().map(|f| f.path().to_path_buf()).collect());
                    }
                }
                Choose::Folder => {
                    if let Some(folder) = dialog.set_title("Choose a folder").pick_folder().await {
                        add_inputs(vec![folder.path().to_path_buf()]);
                    }
                }
                Choose::Output => {
                    if let Some(folder) = dialog.set_title("Output folder").pick_folder().await {
                        settings.write().out_dir = folder.path().to_string_lossy().into_owned();
                        let _ = settings.read().save();
                    }
                }
                Choose::Originals => {
                    if let Some(folder) = dialog.set_title("Folder of original-format music").pick_folder().await {
                        settings.write().originals = folder.path().to_string_lossy().into_owned();
                    }
                }
                Choose::Bios => {
                    if let Some(file) = dialog.set_title("PS2 BIOS image").pick_file().await {
                        settings.write().ps2_bios = file.path().to_string_lossy().into_owned();
                    }
                }
            }
        });
    };

    #[cfg(target_os = "android")]
    let android_overlays = rsx! {
        Browser {
            state: browse,
            on_done: move |paths: Option<Vec<PathBuf>>| {
                let purpose = browse.peek().as_ref().map(|b| b.purpose);
                browse.set(None);
                let (Some(purpose), Some(paths)) = (purpose, paths) else { return };
                let first = paths.first().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                match purpose {
                    Purpose::Files | Purpose::Folder => add_inputs(paths),
                    Purpose::Output => {
                        settings.write().out_dir = first;
                        let _ = settings.read().save();
                    }
                    Purpose::Originals => settings.write().originals = first,
                    Purpose::Bios => settings.write().ps2_bios = first,
                }
            },
        }
        if confirm_cancel() {
            ConfirmCancel {
                on_answer: move |stop: bool| {
                    confirm_cancel.set(false);
                    if stop {
                        CANCEL.store(true, Ordering::Relaxed);
                    }
                },
            }
        }
    };
    #[cfg(not(target_os = "android"))]
    let android_overlays = rsx! {};

    rsx! {
        document::Title { "rommusic" }
        document::Style { {APP_CSS} }
        div {
            class: if ANDROID { "app android" } else { "app" },
            class: if running() { "running" },
            // Anywhere in the window is a drop target.
            ondragover: move |event| event.prevent_default(),
            ondrop: move |event| {
                event.prevent_default();
                over.set(false);
                add_inputs(dropped(&event));
            },
            div { class: "top",
                h1 { "rommusic" }
                span { class: "ver", "{rommusic::VERSION}" }
                span { class: "spacer" }
                button { disabled: running(), onclick: move |_| show_options.set(true), "⚙ Options" }
            }
            if needs_access {
                div { class: "row access",
                    span { "rommusic needs All files access to read your ROMs and write the music on your storage and SD card." }
                    button {
                        class: "primary",
                        onclick: move |_| {
                            #[cfg(target_os = "android")]
                            if let Err(err) = rommusic::android::request_file_access() {
                                log.write().push(line("err", format!("Could not open the access screen: {err}")));
                            }
                        },
                        "Allow"
                    }
                }
            }

            // What to read.
            div {
                class: if over() { "drop over" } else { "drop" },
                ondragover: move |event| { event.prevent_default(); over.set(true); },
                ondragleave: move |_| over.set(false),
                if chosen.is_empty() {
                    b { if ANDROID { "Nothing chosen yet" } else { "Drop files or folders here" } }
                    "ROMs, disc images (CHD, RVZ, ISO, CUE…), dumped disc folders, or music in original audio formats (NSF, SPC, GBS, PSF, VGM…)"
                } else {
                    div { class: "inputs",
                        for (index, path) in chosen.iter().enumerate() {
                            div { key: "{index}", class: "input", title: "{path.display()}",
                                span { if path.is_dir() { "▸ " } else { "♪ " } "{name_of(path)}" }
                                button {
                                    class: "small",
                                    disabled: running(),
                                    title: "Remove",
                                    onclick: move |_| { inputs.write().remove(index); },
                                    "✕"
                                }
                            }
                        }
                    }
                }
            }
            div { class: "row",
                button { disabled: running(), onclick: move |_| choose(Choose::Files), "File…" }
                button { disabled: running(), onclick: move |_| choose(Choose::Folder), "Folder…" }
                span { class: "spacer" }
                button { disabled: running() || chosen.is_empty(), onclick: move |_| inputs.write().clear(), "Clear" }
            }

            // How to write it.
            div { class: "row",
                label { "Format" }
                div { class: "segmented",
                    for (value, label) in FORMATS {
                        button {
                            key: "{value}",
                            class: if s.format == value { "seg active" } else { "seg" },
                            disabled: running(),
                            onclick: move |_| {
                                settings.write().format = value.to_string();
                                let _ = settings.read().save();
                            },
                            "{label}"
                        }
                    }
                }
            }
            div { class: "row",
                label { "Output" }
                input {
                    r#type: "text",
                    placeholder: "next to each input",
                    value: "{s.out_dir}",
                    disabled: running(),
                    oninput: move |event| settings.write().out_dir = event.value(),
                    onchange: move |_| { let _ = settings.read().save(); },
                }
                button { disabled: running(), onclick: move |_| choose(Choose::Output), "Choose…" }
            }

            div { class: "row",
                button { class: "primary", disabled: running() || chosen.is_empty(), onclick: move |_| start(), "Start" }
                div { class: "bar", style: "flex:1", div { style: "width:{overall}%" } }
                button {
                    class: "danger",
                    disabled: !running(),
                    onclick: move |_| {
                        // Android asks first: a stray tap or button press would throw away a
                        // long run.
                        #[cfg(target_os = "android")]
                        confirm_cancel.set(true);
                        #[cfg(not(target_os = "android"))]
                        CANCEL.store(true, Ordering::Relaxed);
                    },
                    "Cancel"
                }
            }
            div { class: "status wide", "{status}" }
            // Focusable on Android, where Select moves the focus here and the D-pad scrolls it.
            div { class: "log", id: "log", tabindex: if ANDROID { "0" },
                for (index, entry) in log().into_iter().enumerate() {
                    div { key: "{index}", class: "{entry.class}", "{entry.text}" }
                }
            }
            if show_options() {
                OptionsPanel {
                    initial: settings(),
                    on_choose: move |what| choose(what),
                    chosen_originals: s.originals.clone(),
                    chosen_bios: s.ps2_bios.clone(),
                    on_close: move |saved: Option<Settings>| {
                        if let Some(saved) = saved {
                            match saved.save() {
                                Ok(path) => log.write().push(line("muted", format!("Options saved to {}", path.display()))),
                                Err(err) => log.write().push(line("err", format!("Options not saved: {err}"))),
                            }
                            settings.set(saved);
                        }
                        show_options.set(false);
                    },
                }
            }
            {android_overlays}
        }
    }
}

/// Something a dialog or the Android browser chooses.
#[derive(Clone, Copy, PartialEq)]
enum Choose {
    Files,
    Folder,
    Output,
    Originals,
    Bios,
}

/// The options behind the button: what to take, at what rate, and what some systems need.
/// The two paths chosen with a dialog while it is open arrive as `chosen_*` and are taken
/// into the draft.
#[component]
fn OptionsPanel(initial: Settings, chosen_originals: String, chosen_bios: String, on_choose: EventHandler<Choose>, on_close: EventHandler<Option<Settings>>) -> Element {
    let mut draft = use_signal(|| initial);
    // A folder or file picked through on_choose lands in the window's settings; take it.
    use_effect(use_reactive!(|chosen_originals| {
        if draft.peek().originals != chosen_originals {
            draft.write().originals = chosen_originals;
        }
    }));
    use_effect(use_reactive!(|chosen_bios| {
        if draft.peek().ps2_bios != chosen_bios {
            draft.write().ps2_bios = chosen_bios;
        }
    }));
    let s = draft();
    rsx! {
        div { class: "overlay",
            div { class: "panel",
                h2 { "Options" }

                h3 { "What to take" }
                div { class: "row",
                    label { "" }
                    div { class: "checks",
                        label {
                            input { r#type: "checkbox", checked: s.all, onchange: move |event| draft.write().all = event.checked() }
                            "Jingles, sound effects and movie sound too"
                        }
                    }
                }
                div { class: "note", "Off: background music only." }

                h3 { "FLAC" }
                div { class: "row",
                    label { "Sample rate" }
                    select {
                        onchange: move |event| draft.write().rate = event.value(),
                        for (value, label) in RATES {
                            option { key: "{value}", value: "{value}", selected: s.rate == value, "{label}" }
                        }
                    }
                }
                div { class: "note", "As stored keeps each track's own rate, 16-bit." }

                h3 { "Cartridges" }
                div { class: "row",
                    label { title: "NSF, SPC, GBS, HES, KSS, SGC and the like, zipped or not", "Original audio" }
                    input { r#type: "text", placeholder: "a folder of music in original audio formats", value: "{s.originals}", oninput: move |event| draft.write().originals = event.value() }
                    button { onclick: move |_| on_choose.call(Choose::Originals), "Choose…" }
                }
                div { class: "note", "Most cartridges need their music in an original audio format to play it; the files are matched to each ROM by their content. Game Boy Advance and SNES music plays straight from the ROM." }
                div { class: "row",
                    label { "SNES run" }
                    input { r#type: "number", min: "1", max: "600", value: "{s.seconds}", oninput: move |event| draft.write().seconds = event.value().trim().parse().unwrap_or(20) }
                    span { class: "note", "seconds" }
                }
                div { class: "row",
                    label { "" }
                    div { class: "checks",
                        label {
                            input { r#type: "checkbox", checked: s.press_start, onchange: move |event| draft.write().press_start = event.checked() }
                            "Press Start while it runs (music that begins after the title screen)"
                        }
                    }
                }

                h3 { "PS1 and PS2" }
                div { class: "row",
                    label { "PS2 BIOS" }
                    input { r#type: "text", placeholder: "a 4 MB PS2 BIOS image of your own", value: "{s.ps2_bios}", oninput: move |event| draft.write().ps2_bios = event.value() }
                    button { onclick: move |_| on_choose.call(Choose::Bios), "Choose…" }
                }
                div { class: "note", "PSF and PSF2 music files need it; without it they are skipped." }

                div { class: "row",
                    span { class: "spacer" }
                    button { onclick: move |_| on_close.call(None), "Cancel" }
                    button { class: "primary", onclick: move |_| on_close.call(Some(draft())), "Save" }
                }
            }
        }
    }
}
