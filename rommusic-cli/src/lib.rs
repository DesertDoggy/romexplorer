//! rommusic's work, shared by its two front ends: the console tool (`rommusic`, src/main.rs)
//! and the window (`rommusic-gui`, src/bin/rommusic-gui.rs, also the Android app).
//!
//! [`prepare`] finds the music in one input (a ROM, a disc image, a dumped folder, or music in
//! an original audio format) and [`write`] writes it: FLAC, the files as the game stores them,
//! or both, into a "<name> (music)" folder. Both report through [`Event`]s, so each front end
//! says what is going on in its own way. The readers and players are the ones the romexplorer
//! app plays ROM music with (`rom::rom_audio`, `rom::rip_sets`, `rom::rom_songs`,
//! `rom::snes_capture`).

pub mod resample;

#[cfg(target_os = "android")]
pub mod android;
#[cfg(target_os = "android")]
pub mod natives;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use romexplorer::common::models::Platform;
use romexplorer::rom::rom_audio::{self, FoundTrack, PcmStream, SoundKind, Thresholds, TrackLocation};
use romexplorer::rom::{detect, gme, rip_match, rip_sets, rom_songs, sevenzip_wrapper, snes_capture, vgmstream};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// SNES music taken from the sound chip after a run of the game has no length of its own.
const SPC_PLAY_MS: u32 = 120_000;
const SPC_FADE_MS: u32 = 10_000;
/// The SNES sound chip's own output rate.
const SPC_RATE: u32 = 32_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Format {
    #[default]
    Flac,
    Original,
    Both,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::Flac => "FLAC",
            Format::Original => "Original audio",
            Format::Both => "FLAC and original audio",
        }
    }
}

/// How to read and write, the same for every input of a run.
#[derive(Clone, Debug)]
pub struct Options {
    /// Jingles, sound effects and movie sound too, not only background music.
    pub all: bool,
    /// At most this many tracks from each input.
    pub max: Option<usize>,
    /// Where the "<name> (music)" folders go; `None`: beside each input.
    pub out: Option<PathBuf>,
    /// The output rate; `None`: the track's own.
    pub rate: Option<u32>,
    /// 16 or 24.
    pub bits: u32,
    pub format: Format,
    /// Folders of music in original audio formats, to match cartridges against.
    pub originals: Vec<PathBuf>,
    pub ps2_bios: Option<PathBuf>,
    /// SNES: how long the game runs before its music is taken.
    pub seconds: f32,
    /// SNES: press Start now and then while it runs.
    pub press_start: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { all: false, max: None, out: None, rate: None, bits: 16, format: Format::Flac, originals: Vec::new(), ps2_bios: None, seconds: 20.0, press_start: false }
    }
}

/// What a run says while it goes.
#[derive(Clone, Debug)]
pub enum Event {
    /// Listing the original-format music folders given: `found` sets so far.
    LibraryListing { found: usize },
    /// Reading the original-format music sets given: set `done + 1` of `total` (`set`, its
    /// path), or all of them read (`set` is `None`).
    Library { done: usize, total: usize, set: Option<String> },
    /// Looking for the music in an input.
    Reading { input: PathBuf },
    /// Looking at file `count` of what an input holds (`file`, its path in there).
    Probing { input: PathBuf, file: String, count: usize },
    /// SNES: the game has run `second` of `of` seconds, waiting for its music to start.
    Running { second: u32, of: u32 },
    /// The track's audio is all decoded; it is being encoded as FLAC.
    TrackEncoding { number: usize, total: usize },
    /// Copying the files as the game stores them: file `number` of `total`, `bytes` long.
    Copying { number: usize, total: usize, name: String, bytes: u64 },
    /// The tracks found, and where they go.
    Found { input: PathBuf, folder: PathBuf, tracks: usize },
    TrackStarted { number: usize, total: usize, name: String },
    /// How far the track being written is, 0.0 to 1.0, when its length is known.
    TrackProgress { number: usize, total: usize, fraction: f32 },
    TrackWritten { number: usize, total: usize, name: String, ms: u64 },
    TrackFailed { number: usize, total: usize, name: String, error: String },
    /// The files as the game stores them, copied.
    OriginalsWritten { count: usize, folder: PathBuf },
    /// `--format original` or both, but the music has no file of its own.
    NoOriginals,
}

/// Said when a run is cancelled part-way.
pub const CANCELLED: &str = "cancelled";

/// Whether the music libraries were found: without them nothing plays.
pub fn libraries_available() -> bool {
    vgmstream::available() || rip_sets::available()
}

/// Loads the PS2 BIOS PS1 and PS2 music needs. `Ok(false)`: this build has no player for it.
pub fn load_ps2_bios(path: &Path) -> Result<bool, String> {
    let image = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    romexplorer::rom::psf::set_ps2_bios(&image).map_err(|e| format!("{}: {e}", path.display()))
}

/// Reads the folders of original-format music once, for every input of a run. What the sets
/// hold is kept in [`library_cache`], so a later run reads only the sets that changed.
pub fn load_library(folders: &[PathBuf], on: &mut dyn FnMut(Event)) -> Option<rip_match::Library> {
    if folders.is_empty() {
        return None;
    }
    let cache = library_cache();
    Some(rip_match::Library::build_with(folders, cache.as_deref(), |step| match step {
        rip_match::BuildStep::Listing { found } => on(Event::LibraryListing { found }),
        rip_match::BuildStep::Reading { done, total, set } => on(Event::Library { done, total, set: set.map(|p| p.display().to_string()) }),
    }))
}

/// Where what the original-format music sets hold is kept between runs: the per-user cache
/// folder by each platform's convention (the app's files on Android), made if missing.
pub fn library_cache() -> Option<PathBuf> {
    #[cfg(target_os = "android")]
    let dir = android::files_dir().join("cache");
    #[cfg(not(target_os = "android"))]
    let dir = {
        let env = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
        let home = env("HOME").or_else(|| env("USERPROFILE"))?;
        let base = if cfg!(windows) {
            env("LOCALAPPDATA").unwrap_or_else(|| home.join("AppData").join("Local"))
        } else if cfg!(target_os = "macos") {
            home.join("Library").join("Caches")
        } else {
            env("XDG_CACHE_HOME").unwrap_or_else(|| home.join(".cache"))
        };
        base.join("rommusic")
    };
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("original-format-sets.json"))
}

/// One thing to write.
pub enum Job {
    /// A track the app's readers found, opened from `base` (the ROM, or the folder a set of
    /// original-format music files' relative path starts from).
    Track { base: PathBuf, track: FoundTrack },
    /// What a SNES cartridge left in its sound chip, played on.
    Spc { spc: Vec<u8> },
}

impl Job {
    pub fn name(&self) -> String {
        match self {
            Job::Track { track, .. } => track.name.clone(),
            Job::Spc { .. } => "Title".to_string(),
        }
    }

    /// One line for a listing: kind, length, whether it loops, name and source.
    pub fn describe(&self) -> String {
        match self {
            // A looping track's length is one pass; the file holds it twice, then a fade.
            Job::Track { track, .. } => format!("{:<6} {:>6}{}  {}  ({})", track.kind.name(), length(track.duration_ms), if track.looping { " loop" } else { "     " }, track.name, track.codec),
            Job::Spc { .. } => format!("bgm    {:>6}  Title  (SNES sound chip)", length(u64::from(SPC_PLAY_MS + SPC_FADE_MS))),
        }
    }
}

/// A file to copy as the game stores it, for [`Format::Original`]: read when it is written,
/// one at a time, not all held while the tracks are.
pub struct Original {
    /// Its path in the "original" folder.
    pub name: String,
    pub from: Stored,
}

pub enum Stored {
    /// Made here (a SNES cartridge's .spc).
    Bytes(Vec<u8>),
    /// A file of its own, copied straight across.
    File(PathBuf),
    /// A file on a disc or in a dumped folder, the one a track at `location` is in.
    InRom { base: PathBuf, location: TrackLocation },
}

/// What one input gives.
pub struct Plan {
    pub jobs: Vec<Job>,
    /// Files to copy as they are stored, for [`Format::Original`].
    pub originals: Vec<Original>,
    /// The folder the music goes into.
    pub folder: PathBuf,
    /// The album name the FLAC files carry.
    pub album: String,
}

/// Finds the music in `input`, keeps what `o` asks for, and decides where it goes. An error
/// when there is none, saying why.
pub fn prepare(input: &Path, o: &Options, library: Option<&rip_match::Library>) -> Result<Plan, String> {
    prepare_with(input, o, library, &mut |_| {})
}

/// [`prepare`], saying what it looks at as it goes ([`Event::Probing`], [`Event::Running`]).
pub fn prepare_with(input: &Path, o: &Options, library: Option<&rip_match::Library>, on: &mut dyn FnMut(Event)) -> Result<Plan, String> {
    if !input.exists() {
        return Err("no such file or folder".into());
    }
    let t = Thresholds::default();
    let (mut jobs, mut originals, why_none) = find(input, o, library, &t, on)?;
    jobs.retain(|j| match j {
        Job::Track { track, .. } => o.all || (track.kind == SoundKind::Bgm && !track.video),
        Job::Spc { .. } => true,
    });
    if let Some(max) = o.max {
        jobs.truncate(max);
    }
    // After the filter and the cap, so the originals are those of the tracks written.
    stored_files(&jobs, &mut originals, o);
    if jobs.is_empty() {
        return Err(why_none.unwrap_or_else(|| if o.all { "no music found".into() } else { "no background music found (jingles and sound effects are left out unless asked for)".into() }));
    }
    Ok(Plan { jobs, originals, folder: music_folder(input, o), album: album(input) })
}

/// The input's name: the album the FLAC files carry, and their folder's name.
fn album(input: &Path) -> String {
    let stem = input.file_stem().or_else(|| input.file_name()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "rom".into());
    if input.is_dir() { input.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or(stem) } else { stem }
}

/// The "<name> (music)" folder `input`'s music goes into, on its own.
pub fn music_folder(input: &Path, o: &Options) -> PathBuf {
    music_folder_named(input, o, &album(input))
}

fn music_folder_named(input: &Path, o: &Options, name: &str) -> PathBuf {
    o.out.clone().unwrap_or_else(|| input.parent().map(Path::to_path_buf).unwrap_or_default()).join(format!("{} (music)", clean(name)))
}

/// [`music_folder`] for each input of one run, kept apart: two that would share a folder
/// (`A/Game.sfc` and `B/Game.sfc` with one `--out`, or `Game.sfc` and `Game.nes` side by side)
/// would mix their tracks, so a later one gets "<name> (2) (music)". Compared ignoring case,
/// as the file systems that ignore it would.
pub fn music_folders(inputs: &[PathBuf], o: &Options) -> Vec<PathBuf> {
    let mut taken = std::collections::HashSet::new();
    inputs
        .iter()
        .map(|input| {
            let name = album(input);
            let folder = std::iter::once(music_folder_named(input, o, &name))
                .chain((2..).map(|n| music_folder_named(input, o, &format!("{name} ({n})"))))
                .find(|folder| !taken.contains(&folder.to_string_lossy().to_lowercase()))
                .expect("an unbounded list of names");
            taken.insert(folder.to_string_lossy().to_lowercase());
            folder
        })
        .collect()
}

/// Finds and writes one input into `folder` (its [`music_folders`] entry). `cancel` stops it
/// between chunks of a track; the track being written is then removed and the error is
/// [`CANCELLED`].
pub fn process(input: &Path, folder: &Path, o: &Options, library: Option<&rip_match::Library>, cancel: &AtomicBool, on: &mut dyn FnMut(Event)) -> Result<(), String> {
    on(Event::Reading { input: input.to_path_buf() });
    let mut plan = prepare_with(input, o, library, on)?;
    plan.folder = folder.to_path_buf();
    if cancel.load(Ordering::Relaxed) {
        return Err(CANCELLED.into());
    }
    on(Event::Found { input: input.to_path_buf(), folder: plan.folder.clone(), tracks: plan.jobs.len() });
    write(&plan, o, cancel, on)
}

/// Writes what [`prepare`] found.
pub fn write(plan: &Plan, o: &Options, cancel: &AtomicBool, on: &mut dyn FnMut(Event)) -> Result<(), String> {
    let folder = &plan.folder;
    std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let mut errors = 0;
    if o.format != Format::Original {
        let mut index = String::from("#\tkind\tlength\tloop\tsource\tname\n");
        let total = plan.jobs.len();
        let width = if total >= 100 { 3 } else { 2 };
        for (n, job) in plan.jobs.iter().enumerate() {
            let number = n + 1;
            let name = job.name();
            let file = folder.join(format!("{number:0width$} - {}.flac", clean(&name)));
            on(Event::TrackStarted { number, total, name: name.clone() });
            let mut progress = |fraction: Option<f32>| match fraction {
                Some(fraction) => on(Event::TrackProgress { number, total, fraction }),
                None => on(Event::TrackEncoding { number, total }),
            };
            match render(job, o, &plan.album, number, total, &file, cancel, &mut progress) {
                Ok(ms) => {
                    on(Event::TrackWritten { number, total, name: name.clone(), ms });
                    let (kind, looping, source) = match job {
                        Job::Track { track, .. } => (track.kind.name(), track.looping, track.codec.clone()),
                        Job::Spc { .. } => ("bgm", false, "SNES sound chip".to_string()),
                    };
                    index.push_str(&format!("{number}\t{kind}\t{}\t{}\t{source}\t{name}\n", length(ms), if looping { "looped twice, faded" } else { "once" }));
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&file);
                    if e == CANCELLED {
                        let _ = std::fs::write(folder.join("index.txt"), &index);
                        return Err(e);
                    }
                    on(Event::TrackFailed { number, total, name, error: e });
                    errors += 1;
                }
            }
        }
        std::fs::write(folder.join("index.txt"), index).map_err(|e| format!("index.txt: {e}"))?;
    }
    if o.format != Format::Flac {
        let originals = folder.join("original");
        std::fs::create_dir_all(&originals).map_err(|e| format!("{}: {e}", originals.display()))?;
        let total = plan.originals.len();
        let mut copied = 0;
        for (n, original) in plan.originals.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err(CANCELLED.into());
            }
            let path = originals.join(clean_path(&original.name));
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let announce = |bytes: u64, on: &mut dyn FnMut(Event)| on(Event::Copying { number: n + 1, total, name: original.name.clone(), bytes });
            let written = match &original.from {
                Stored::Bytes(bytes) => {
                    announce(bytes.len() as u64, on);
                    std::fs::write(&path, bytes)
                }
                // Streamed across, never held whole.
                Stored::File(from) => {
                    announce(std::fs::metadata(from).map(|m| m.len()).unwrap_or(0), on);
                    std::fs::copy(from, &path).map(|_| ())
                }
                Stored::InRom { base, location } => match rom_audio::stored_file(base, location) {
                    Ok((_, bytes)) => {
                        announce(bytes.len() as u64, on);
                        std::fs::write(&path, bytes)
                    }
                    // Unreadable now: left out, as it was when the files were read up front.
                    Err(_) => continue,
                },
            };
            written.map_err(|e| format!("{}: {e}", path.display()))?;
            copied += 1;
        }
        if copied == 0 {
            on(Event::NoOriginals);
        } else {
            on(Event::OriginalsWritten { count: copied, folder: originals });
        }
    }
    if errors > 0 {
        return Err(format!("{errors} track(s) could not be written"));
    }
    Ok(())
}

// --- finding the music --------------------------------------------------------------------

fn ext_of(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

fn archive_format(path: &Path) -> Option<sevenzip_wrapper::ArchiveFormat> {
    match ext_of(path).as_str() {
        "zip" => Some(sevenzip_wrapper::ArchiveFormat::Zip),
        "7z" => Some(sevenzip_wrapper::ArchiveFormat::SevenZ),
        _ => None,
    }
}

/// The name of the biggest file in a zip or 7z: the ROM in it.
fn main_member(path: &Path, format: sevenzip_wrapper::ArchiveFormat) -> Option<String> {
    let archive = sevenzip_wrapper::SevenZipArchive::open(path, format, |_, _| true).ok()?;
    (0..archive.entry_count()).filter_map(|i| archive.entry(i).ok()).filter(|e| !e.is_dir).max_by_key(|e| e.size).map(|e| e.path)
}

fn platform_of(name: &str) -> Option<Platform> {
    let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    detect::infer_platforms_from_extension_heuristics(&ext).into_iter().next()
}

fn tracks(base: &Path, found: Vec<FoundTrack>) -> Vec<Job> {
    found.into_iter().map(|track| Job::Track { base: base.to_path_buf(), track }).collect()
}

type Found = (Vec<Job>, Vec<Original>, Option<String>);

fn find(input: &Path, o: &Options, library: Option<&rip_match::Library>, t: &Thresholds, on: &mut dyn FnMut(Event)) -> Result<Found, String> {
    let mut jobs = Vec::new();
    let mut originals = Vec::new();
    // Which file is being looked at, and how many so far: a disc's listing can run to
    // thousands, each probed for audio.
    let mut count = 0usize;
    let mut progress = |file: &str| {
        count += 1;
        on(Event::Probing { input: input.to_path_buf(), file: file.to_string(), count });
    };
    if input.is_dir() {
        // A dumped disc, and any original-format music sets kept in it.
        if let Ok(found) = rom_audio::index_rom(input, t, &mut progress) {
            jobs.extend(tracks(input, found));
        }
        jobs.extend(tracks(input, rip_sets::index(input, t, &mut progress)));
        return Ok((jobs, originals, None));
    }
    let name = input.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if rip_sets::is_music(&name) {
        // One original-format file, with the playlist and libraries beside it.
        let input = std::path::absolute(input).unwrap_or_else(|_| input.to_path_buf());
        jobs.extend(tracks(Path::new(""), rip_sets::index_file(&input, t, &mut progress)));
        return Ok((jobs, originals, None));
    }
    if rom_audio::may_hold_audio(input) {
        jobs.extend(tracks(input, rom_audio::index_rom(input, t, &mut progress)?));
        return Ok((jobs, originals, None));
    }
    let member = archive_format(input).and_then(|f| main_member(input, f));
    if archive_format(input).is_some() {
        // A zip of original-format files is a set of its own.
        let found = rip_sets::index_set(input, t, &mut progress);
        if !found.is_empty() {
            jobs.extend(tracks(Path::new(""), found));
            return Ok((jobs, originals, None));
        }
        if member.as_deref().is_some_and(rip_sets::is_rip_member) {
            return Err("an original-format music set, but none of its tracks could be played (--verbose says why)".into());
        }
    }
    if member.as_deref().is_some_and(|m| rom_audio::may_hold_audio(Path::new(m))) {
        return Err("a disc image inside a zip or 7z is not read: extract it first".into());
    }
    let platform = platform_of(member.as_deref().unwrap_or(&name)).ok_or("not a ROM, disc image or original-format music set rommusic knows")?;
    // A cartridge: a matching original-format set first, then what plays straight from the ROM.
    if let Some(library) = library.filter(|l| l.covers(platform as u16)) {
        let matches = library.match_rom(input, platform as u16)?;
        for set in rip_match::choose(&matches, |_| false, |_| false) {
            jobs.extend(tracks(Path::new(""), rip_sets::index_set(&set, t, &mut progress)));
        }
        if !jobs.is_empty() {
            return Ok((jobs, originals, None));
        }
    }
    let mut why_none = None;
    // A SNES ROM is read once, for the music map and, without one, for the capture.
    let snes_rom = if matches!(platform, Platform::Snes | Platform::SuperFamicom) { Some(rom_songs::read_rom(input)?) } else { None };
    match platform {
        // Played by the game's own sound code or engine, straight from the ROM. A SNES game
        // with a music map plays each song by it; one without falls to the capture below.
        p if rom_songs::covers(p as u16) && !snes_rom.as_ref().is_some_and(|rom| !rom_songs::has_snes_map(rom)) => {
            let (found, why) = rom_songs::index_explained(input, p as u16, t)?;
            jobs.extend(tracks(input, found));
            if jobs.is_empty() {
                why_none = why;
            }
        }
        Platform::Snes | Platform::SuperFamicom => {
            let rom = match snes_rom {
                Some(rom) => rom,
                None => rom_songs::read_rom(input)?,
            };
            let options = snes_capture::Options { seconds: o.seconds, keep_audio: false, press_start: o.press_start, watch_register: None };
            let capture = snes_capture::capture_with(&rom, &options, &mut |second, of| on(Event::Running { second, of }))?;
            if capture.driver_starts.is_empty() {
                why_none = Some(format!("the game started no music in {} s (try pressing Start, or a longer run)", o.seconds));
            } else {
                originals.push(Original { name: format!("{}.spc", input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()), from: Stored::Bytes(capture.spc.clone()) });
                jobs.push(Job::Spc { spc: capture.spc });
            }
        }
        other => {
            why_none = Some(if o.originals.is_empty() {
                format!("{} music does not play straight from the ROM yet; a folder of its music in original audio formats can stand in", other.display_name())
            } else {
                "no original-format music set in the folders given matches this ROM".to_string()
            });
        }
    }
    Ok((jobs, originals, why_none))
}

/// The files the tracks are stored in, each once, for [`Format::Original`]. Only listed here:
/// [`write`] reads each as it copies it.
fn stored_files(jobs: &[Job], originals: &mut Vec<Original>, o: &Options) {
    if o.format == Format::Flac {
        return;
    }
    let mut seen: std::collections::HashSet<String> = originals.iter().map(|original| original.name.clone()).collect();
    for job in jobs {
        let Job::Track { base, track } = job else { continue };
        match &track.location {
            TrackLocation::File { path: inner, .. } => {
                // The file itself, for a member of an archive inside it (as `stored_file` names it).
                let name = romexplorer::rom::inner_archives::split(inner).map_or(inner.as_str(), |(file, _)| file).to_string();
                if seen.insert(name.clone()) {
                    originals.push(Original { name, from: Stored::InRom { base: base.clone(), location: track.location.clone() } });
                }
            }
            TrackLocation::Rip { path, .. } => {
                // A set in a zip or 7z is copied whole; a loose file with the playlists beside it.
                let file = path.split('|').next().unwrap_or(path);
                let full = if Path::new(file).is_absolute() { PathBuf::from(file) } else { base.join(file) };
                let mut copy = vec![full.clone()];
                if !path.contains('|') {
                    if let Some(Ok(entries)) = full.parent().map(std::fs::read_dir) {
                        copy.extend(entries.flatten().map(|e| e.path()).filter(|p| ext_of(p) == "m3u"));
                    }
                }
                for f in copy {
                    let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    if f.is_file() && seen.insert(name.clone()) {
                        originals.push(Original { name, from: Stored::File(f) });
                    }
                }
            }
            TrackLocation::Cdda { .. } | TrackLocation::RomSong { .. } => {}
        }
    }
}

// --- writing --------------------------------------------------------------------------

/// The SNES sound chip's state, played on from where the game left it.
struct SpcPcm {
    emu: gme::Emu,
    left: u64,
}

impl PcmStream for SpcPcm {
    fn sample_rate(&self) -> u32 {
        self.emu.sample_rate()
    }
    fn channels(&self) -> u16 {
        gme::Emu::CHANNELS
    }
    fn total_samples(&self) -> Option<u64> {
        Some(self.left / 2)
    }
    fn next_chunk(&mut self) -> Result<Option<Vec<i16>>, String> {
        if self.left == 0 || self.emu.ended() {
            return Ok(None);
        }
        let mut buf = vec![0i16; self.left.min(8192) as usize & !1];
        self.emu.play(&mut buf)?;
        self.left -= buf.len() as u64;
        Ok(Some(buf))
    }
}

fn open(job: &Job) -> Result<Box<dyn PcmStream>, String> {
    match job {
        Job::Track { base, track } => {
            let config = if track.looping { vgmstream::PlayConfig::LISTEN } else { vgmstream::PlayConfig::AS_STORED };
            rom_audio::open_track(base, &track.location, config)
        }
        Job::Spc { spc } => {
            let mut emu = gme::Emu::open(spc.clone(), SPC_RATE)?;
            emu.start(0, Some((SPC_PLAY_MS, SPC_FADE_MS)))?;
            let left = u64::from(SPC_PLAY_MS + SPC_FADE_MS) * u64::from(SPC_RATE) / 1000 * 2;
            Ok(Box::new(SpcPcm { emu, left }))
        }
    }
}

/// Writes one track; returns its length in ms.
#[allow(clippy::too_many_arguments)]
/// `progress` hears how far the decoding is (`Some`, 0.0 to 1.0), and `None` when the FLAC
/// encoding starts.
fn render(job: &Job, o: &Options, album: &str, number: usize, total: usize, out: &Path, cancel: &AtomicBool, progress: &mut dyn FnMut(Option<f32>)) -> Result<u64, String> {
    let mut stream = open(job)?;
    let (from, channels) = (stream.sample_rate(), stream.channels());
    // In samples across all channels, as the chunks come.
    let expected = stream.total_samples().map(|frames| frames * u64::from(channels)).filter(|n| *n > 0);
    let mut pcm: Vec<i16> = Vec::new();
    let mut reported = 0.0f32;
    while let Some(chunk) = stream.next_chunk()? {
        if cancel.load(Ordering::Relaxed) {
            return Err(CANCELLED.into());
        }
        pcm.extend_from_slice(&chunk);
        if let Some(expected) = expected {
            // The decoding is most of the time; the last 5% is the FLAC encoding.
            let fraction = (pcm.len() as f32 / expected as f32).min(1.0) * 0.95;
            if fraction - reported >= 0.01 {
                reported = fraction;
                progress(Some(fraction));
            }
        }
    }
    if pcm.is_empty() {
        return Err("no audio".into());
    }
    let to = o.rate.unwrap_or(from);
    let samples: Vec<i32> = if to == from && o.bits == 16 {
        pcm.iter().map(|&s| i32::from(s)).collect()
    } else {
        let float: Vec<f32> = pcm.iter().map(|&s| f32::from(s) / 32768.0).collect();
        let float = if to == from { float } else { resample::resample(&float, usize::from(channels), from, to) };
        resample::quantize(&float, o.bits)
    };
    let ms = (samples.len() / usize::from(channels)) as u64 * 1000 / u64::from(to);
    let source = match job {
        Job::Track { track, .. } => track.codec.clone(),
        Job::Spc { .. } => "SNES sound chip".into(),
    };
    let options = flac_codec::encode::Options::default()
        .tag("TITLE", job.name())
        .tag("ALBUM", album)
        .tag("TRACKNUMBER", number)
        .tag("TRACKTOTAL", total)
        .tag("COMMENT", format!("ROM music ({source}){}", if to != from { format!(", resampled from {from} Hz") } else { String::new() }))
        .tag("ENCODER", format!("rommusic {VERSION}"));
    progress(None);
    // A file left by an earlier run (or by another input with the same name) is replaced.
    let _ = std::fs::remove_file(out);
    let mut writer = flac_codec::encode::FlacSampleWriter::create(out, options, to, o.bits, u8::try_from(channels).map_err(|_| "too many channels")?, None).map_err(|e| format!("create {}: {e}", out.display()))?;
    writer.write(&samples).map_err(|e| format!("FLAC: {e}"))?;
    writer.finalize().map_err(|e| format!("FLAC: {e}"))?;
    progress(Some(1.0));
    Ok(ms)
}

// --- small things ---------------------------------------------------------------------

/// 12,400: a count with thousands separators, for progress lines.
pub fn thousands(n: usize) -> String {
    romexplorer::database::jobs::thousands(n as u64)
}

/// 38 MB: a file's size, for progress lines.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let (mut value, mut unit) = (bytes as f64, 0);
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}

pub fn length(ms: u64) -> String {
    format!("{}:{:02}", ms / 60_000, ms / 1000 % 60)
}

/// A name that is safe as a file name everywhere.
pub fn clean(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c }).collect();
    let s = s.trim().trim_end_matches('.').trim();
    if s.is_empty() { "track".into() } else { s.chars().take(120).collect() }
}

/// A stored path made safe, keeping its folders.
pub fn clean_path(path: &str) -> PathBuf {
    path.split(['/', '\\']).filter(|p| !p.is_empty() && *p != "." && *p != "..").map(clean).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_safe_file_names() {
        assert_eq!(clean("Boss: Final / Last?"), "Boss_ Final _ Last_");
        assert_eq!(clean("..."), "track");
        assert_eq!(clean_path("../BGM/../a.adx"), PathBuf::from("BGM").join("a.adx"));
    }

    #[test]
    fn inputs_of_one_name_get_folders_of_their_own() {
        let o = Options { out: Some(PathBuf::from("out")), ..Options::default() };
        let inputs = [PathBuf::from("a/Game.sfc"), PathBuf::from("b/game.sfc"), PathBuf::from("b/Other.nes"), PathBuf::from("c/Game.nes")];
        let names: Vec<PathBuf> = music_folders(&inputs, &o);
        let out = Path::new("out");
        assert_eq!(names, [out.join("Game (music)"), out.join("game (2) (music)"), out.join("Other (music)"), out.join("Game (3) (music)")]);
        // Each beside its own input: nothing shared.
        let names = music_folders(&inputs[..2], &Options::default());
        assert_eq!(names, [Path::new("a").join("Game (music)"), Path::new("b").join("game (music)")]);
    }

    #[test]
    fn a_missing_input_says_so() {
        let o = Options::default();
        assert_eq!(prepare(Path::new("/no/such/rom.sfc"), &o, None).err().as_deref(), Some("no such file or folder"));
    }
}
