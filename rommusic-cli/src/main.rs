//! rommusic: drop a ROM, disc image, dumped folder or music rip set on it and it writes the
//! music as FLAC, in a "<name> (music)" folder beside what was dropped. The same readers and
//! players the romexplorer app plays ROM music with (`rom::rom_audio`, `rom::rip_sets`,
//! `rom::rom_songs`, `rom::snes_capture`). See rommusic-cli/README.md.

mod resample;

use std::io::Write;
use std::path::{Path, PathBuf};

use romexplorer::common::models::Platform;
use romexplorer::rom::rom_audio::{self, FoundTrack, PcmStream, SoundKind, Thresholds, TrackLocation};
use romexplorer::rom::{detect, gme, rip_match, rip_sets, rom_songs, sevenzip_wrapper, snes_capture, vgmstream};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// SNES music taken from the sound chip after a run of the game has no length of its own.
const SPC_PLAY_MS: u32 = 120_000;
const SPC_FADE_MS: u32 = 10_000;
/// The SNES sound chip's own output rate.
const SPC_RATE: u32 = 32_000;

fn usage() -> String {
    format!(
        "rommusic {VERSION}: the music in a ROM, as FLAC.

Usage: rommusic [options] <ROM, disc image, folder or rip set>...

Dropping files on rommusic does the same as giving their paths with no options: the
background music of each, as FLAC at its original rate, in \"<name> (music)\" beside it.

Options:
  --all             jingles, sound effects and movie sound too (default: background music)
  --max N           at most N tracks from each input
  --out DIR         put the \"<name> (music)\" folders in DIR
  --rate R          native (default) | cd (44.1 kHz/16-bit) | dvd (48 kHz/24-bit) |
                    hires (96 kHz/24-bit) | <Hz>; --cd, --dvd and --hires are short for these
  --bits 16|24      bits per sample (default 16; 24 with --dvd and --hires)
  --format F        flac (default) | original | both. original copies the music as the game
                    stores it (the stream files on a disc, the rip set's files, an .spc
                    from a SNES cartridge) into an \"original\" folder
  --rips DIR        a folder of music rip sets (NSF, SPC, GBS, HES, KSS, SGC, zipped or not)
                    to match cartridges against by their content; may be given more than once
  --seconds N       SNES: how long the game runs before its music is taken (default 20)
  --start           SNES: press Start now and then while it runs, for games whose music
                    begins after the title screen
  --list            only list the tracks
  -h, --help        this text
  -V, --version     the version"
    )
}

#[derive(Clone, Copy, PartialEq)]
enum Format {
    Flac,
    Original,
    Both,
}

struct Args {
    inputs: Vec<PathBuf>,
    all: bool,
    max: Option<usize>,
    out: Option<PathBuf>,
    rate: Option<u32>,
    bits: u32,
    format: Format,
    rips: Vec<PathBuf>,
    seconds: f32,
    press_start: bool,
    list: bool,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut a = Args { inputs: Vec::new(), all: false, max: None, out: None, rate: None, bits: 0, format: Format::Flac, rips: Vec::new(), seconds: 20.0, press_start: false, list: false };
    let mut it = args.iter();
    let value = |it: &mut std::slice::Iter<String>, flag: &str| it.next().cloned().ok_or_else(|| format!("{flag} needs a value"));
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--all" => a.all = true,
            "--list" => a.list = true,
            "--start" => a.press_start = true,
            "--cd" => a.rate = Some(44_100),
            "--dvd" => (a.rate, a.bits) = (Some(48_000), if a.bits == 0 { 24 } else { a.bits }),
            "--hires" => (a.rate, a.bits) = (Some(96_000), if a.bits == 0 { 24 } else { a.bits }),
            "--max" => a.max = Some(value(&mut it, arg)?.parse().map_err(|_| "--max needs a number")?),
            "--out" => a.out = Some(PathBuf::from(value(&mut it, arg)?)),
            "--rips" => a.rips.push(PathBuf::from(value(&mut it, arg)?)),
            "--seconds" => a.seconds = value(&mut it, arg)?.parse().ok().filter(|s: &f32| (1.0..=600.0).contains(s)).ok_or("--seconds needs 1-600")?,
            "--bits" => a.bits = value(&mut it, arg)?.parse().ok().filter(|b| [16, 24].contains(b)).ok_or("--bits is 16 or 24")?,
            "--rate" => {
                let v = value(&mut it, arg)?;
                a.rate = match v.as_str() {
                    "native" => None,
                    "cd" => Some(44_100),
                    "dvd" => {
                        a.bits = if a.bits == 0 { 24 } else { a.bits };
                        Some(48_000)
                    }
                    "hires" => {
                        a.bits = if a.bits == 0 { 24 } else { a.bits };
                        Some(96_000)
                    }
                    hz => Some(hz.parse().ok().filter(|r| (8_000..=192_000).contains(r)).ok_or("--rate is native, cd, dvd, hires or 8000-192000")?),
                };
            }
            "--format" => {
                a.format = match value(&mut it, arg)?.as_str() {
                    "flac" => Format::Flac,
                    "original" => Format::Original,
                    "both" => Format::Both,
                    _ => return Err("--format is flac, original or both".into()),
                }
            }
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            path => a.inputs.push(PathBuf::from(path)),
        }
    }
    if a.bits == 0 {
        a.bits = 16;
    }
    if a.inputs.is_empty() {
        return Err("nothing to read".into());
    }
    Ok(a)
}

/// One thing to write.
enum Job {
    /// A track the app's readers found, opened from `base` (the ROM, or the folder a rip
    /// set's relative path starts from).
    Track { base: PathBuf, track: FoundTrack },
    /// What a SNES cartridge left in its sound chip, played on.
    Spc { spc: Vec<u8> },
}

impl Job {
    fn name(&self) -> String {
        match self {
            Job::Track { track, .. } => track.name.clone(),
            Job::Spc { .. } => "Title".to_string(),
        }
    }
}

/// What one input gives.
struct Plan {
    jobs: Vec<Job>,
    /// Files to copy as they are stored, for `--format original`: a name and its bytes.
    originals: Vec<(String, Vec<u8>)>,
    /// Said when nothing is found.
    why_none: Option<String>,
}

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{}", usage());
        return finish(0);
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("rommusic {VERSION}");
        return 0;
    }
    let a = match parse(&args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("rommusic: {e}\n\n{}", usage());
            return finish(2);
        }
    };
    if !vgmstream::available() && !gme::available() {
        eprintln!("rommusic: the music libraries (rvgmstream, rommusic) were not found in lib/ beside rommusic");
        return finish(1);
    }
    let library = (!a.rips.is_empty()).then(|| {
        eprintln!("reading the rip sets in {}", a.rips.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "));
        rip_match::Library::build(&a.rips, None, |_, _| {})
    });
    let mut failed = 0;
    for input in &a.inputs {
        match one(input, &a, library.as_ref()) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("{}: {e}", input.display());
                failed += 1;
            }
        }
    }
    finish(i32::from(failed > 0))
}

fn one(input: &Path, a: &Args, library: Option<&rip_match::Library>) -> Result<(), String> {
    if !input.exists() {
        return Err("no such file or folder".into());
    }
    let t = Thresholds::default();
    let mut plan = find(input, a, library, &t)?;
    plan.jobs.retain(|j| match j {
        Job::Track { track, .. } => a.all || (track.kind == SoundKind::Bgm && !track.video),
        Job::Spc { .. } => true,
    });
    if let Some(max) = a.max {
        plan.jobs.truncate(max);
    }
    if plan.jobs.is_empty() {
        return Err(plan.why_none.unwrap_or_else(|| if a.all { "no music found".into() } else { "no background music found (--all lists jingles and sound effects too)".into() }));
    }
    let stem = input.file_stem().or_else(|| input.file_name()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "rom".into());
    let stem = if input.is_dir() { input.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or(stem) } else { stem };
    let folder = a.out.clone().unwrap_or_else(|| input.parent().map(Path::to_path_buf).unwrap_or_default()).join(format!("{} (music)", clean(&stem)));
    println!("{} -> {}", input.display(), folder.display());
    if a.list {
        for (n, job) in plan.jobs.iter().enumerate() {
            match job {
                Job::Track { track, .. } => println!("  {:02}  {:<6} {:>6}  {}  ({})", n + 1, track.kind.name(), length(track.duration_ms), track.name, track.codec),
                Job::Spc { .. } => println!("  {:02}  bgm    {:>6}  Title  (SNES sound chip)", n + 1, length(u64::from(SPC_PLAY_MS + SPC_FADE_MS))),
            }
        }
        return Ok(());
    }
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let mut index = String::from("#\tkind\tlength\tloop\tsource\tname\n");
    let mut errors = 0;
    if a.format != Format::Original {
        let width = if plan.jobs.len() >= 100 { 3 } else { 2 };
        for (n, job) in plan.jobs.iter().enumerate() {
            let number = n + 1;
            let name = job.name();
            let file = folder.join(format!("{number:0width$} - {}.flac", clean(&name)));
            print!("  {number:0width$} {name} ... ");
            let _ = std::io::stdout().flush();
            match render(job, a, &stem, number, plan.jobs.len(), &file) {
                Ok(ms) => {
                    println!("{}", length(ms));
                    let (kind, looping, source) = match job {
                        Job::Track { track, .. } => (track.kind.name(), track.looping, track.codec.clone()),
                        Job::Spc { .. } => ("bgm", false, "SNES sound chip".to_string()),
                    };
                    index.push_str(&format!("{number}\t{kind}\t{}\t{}\t{source}\t{name}\n", length(ms), if looping { "looped twice, faded" } else { "once" }));
                }
                Err(e) => {
                    println!("failed: {e}");
                    let _ = std::fs::remove_file(&file);
                    errors += 1;
                }
            }
        }
        std::fs::write(folder.join("index.txt"), index).map_err(|e| format!("index.txt: {e}"))?;
    }
    if a.format != Format::Flac {
        let originals = folder.join("original");
        std::fs::create_dir_all(&originals).map_err(|e| format!("{}: {e}", originals.display()))?;
        for (name, bytes) in &plan.originals {
            let path = originals.join(clean_path(name));
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        if plan.originals.is_empty() {
            println!("  (no original files: CD audio and music sequenced in a cartridge have none)");
        } else {
            println!("  {} original file(s) in {}", plan.originals.len(), originals.display());
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

fn find(input: &Path, a: &Args, library: Option<&rip_match::Library>, t: &Thresholds) -> Result<Plan, String> {
    let mut plan = Plan { jobs: Vec::new(), originals: Vec::new(), why_none: None };
    let progress = |_: &str| {};
    if input.is_dir() {
        // A dumped disc, and any rip sets kept in it.
        if let Ok(found) = rom_audio::index_rom(input, t, progress) {
            plan.jobs.extend(tracks(input, found));
        }
        plan.jobs.extend(tracks(input, rip_sets::index(input, t, progress)));
        originals(&mut plan, a);
        return Ok(plan);
    }
    let name = input.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if gme::known_extension(&name) {
        // One rip file: its folder is its set, and only its own tracks are wanted.
        let parent = input.parent().map(Path::to_path_buf).unwrap_or_default();
        let me = input.to_string_lossy().into_owned();
        let found: Vec<FoundTrack> = rip_sets::index_set(&parent, t, progress).into_iter().filter(|f| matches!(&f.location, TrackLocation::Rip { path, .. } if *path == me)).collect();
        plan.jobs.extend(tracks(Path::new(""), found));
        originals(&mut plan, a);
        return Ok(plan);
    }
    if rom_audio::may_hold_audio(input) {
        plan.jobs.extend(tracks(input, rom_audio::index_rom(input, t, progress)?));
        originals(&mut plan, a);
        return Ok(plan);
    }
    let member = archive_format(input).and_then(|f| main_member(input, f));
    if archive_format(input).is_some() {
        // A zip of rip files is a set of its own.
        let found = rip_sets::index_set(input, t, progress);
        if !found.is_empty() {
            plan.jobs.extend(tracks(Path::new(""), found));
            originals(&mut plan, a);
            return Ok(plan);
        }
    }
    if member.as_deref().is_some_and(|m| rom_audio::may_hold_audio(Path::new(m))) {
        return Err("a disc image inside a zip or 7z is not read: extract it first".into());
    }
    let platform = platform_of(member.as_deref().unwrap_or(&name)).ok_or("not a ROM, disc image or rip set rommusic knows")?;
    // A cartridge: a matching rip set first, then what plays straight from the ROM.
    if let Some(library) = library.filter(|l| l.covers(platform as u16)) {
        let matches = library.match_rom(input, platform as u16)?;
        for set in rip_match::choose(&matches, |_| false, |_| false) {
            plan.jobs.extend(tracks(Path::new(""), rip_sets::index_set(&set, t, progress)));
        }
        if !plan.jobs.is_empty() {
            originals(&mut plan, a);
            return Ok(plan);
        }
    }
    match platform {
        Platform::GameBoyAdvance => {
            plan.jobs.extend(tracks(input, rom_songs::index(input, platform as u16, t)?));
            if plan.jobs.is_empty() {
                plan.why_none = Some("this game does not use the sound engine rommusic plays Game Boy Advance music with".into());
            }
        }
        Platform::Snes | Platform::SuperFamicom => {
            let rom = rom_songs::read_rom(input)?;
            let options = snes_capture::Options { seconds: a.seconds, keep_audio: false, press_start: a.press_start, watch_register: None };
            let capture = snes_capture::capture(&rom, &options)?;
            if capture.driver_starts.is_empty() {
                plan.why_none = Some(format!("the game started no music in {} s (try --start or more --seconds)", a.seconds));
            } else {
                plan.originals.push((format!("{}.spc", input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()), capture.spc.clone()));
                plan.jobs.push(Job::Spc { spc: capture.spc });
            }
        }
        other => {
            plan.why_none = Some(if a.rips.is_empty() {
                format!("{} music needs a rip set: pass --rips <folder>", other.display_name())
            } else {
                "no rip set in --rips matches this ROM".to_string()
            });
        }
    }
    Ok(plan)
}

/// The files the found tracks are stored in, read once each, for `--format original`.
fn originals(plan: &mut Plan, a: &Args) {
    if a.format == Format::Flac {
        return;
    }
    let mut seen = std::collections::HashSet::new();
    for job in &plan.jobs {
        let Job::Track { base, track } = job else { continue };
        if !a.all && (track.kind != SoundKind::Bgm || track.video) {
            continue;
        }
        match &track.location {
            TrackLocation::File { .. } => {
                if let Ok((name, bytes)) = rom_audio::stored_file(base, &track.location) {
                    if seen.insert(name.clone()) {
                        plan.originals.push((name, bytes));
                    }
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
                    if seen.insert(name.clone()) {
                        if let Ok(bytes) = std::fs::read(&f) {
                            plan.originals.push((name, bytes));
                        }
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
fn render(job: &Job, a: &Args, album: &str, number: usize, total: usize, out: &Path) -> Result<u64, String> {
    let mut stream = open(job)?;
    let (from, channels) = (stream.sample_rate(), stream.channels());
    let mut pcm: Vec<i16> = Vec::new();
    while let Some(chunk) = stream.next_chunk()? {
        pcm.extend_from_slice(&chunk);
    }
    if pcm.is_empty() {
        return Err("no audio".into());
    }
    let to = a.rate.unwrap_or(from);
    let samples: Vec<i32> = if to == from && a.bits == 16 {
        pcm.iter().map(|&s| i32::from(s)).collect()
    } else {
        let float: Vec<f32> = pcm.iter().map(|&s| f32::from(s) / 32768.0).collect();
        let float = if to == from { float } else { resample::resample(&float, usize::from(channels), from, to) };
        resample::quantize(&float, a.bits)
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
    let mut writer = flac_codec::encode::FlacSampleWriter::create(out, options, to, a.bits, u8::try_from(channels).map_err(|_| "too many channels")?, None).map_err(|e| format!("create {}: {e}", out.display()))?;
    writer.write(&samples).map_err(|e| format!("FLAC: {e}"))?;
    writer.finalize().map_err(|e| format!("FLAC: {e}"))?;
    Ok(ms)
}

// --- small things ---------------------------------------------------------------------

fn length(ms: u64) -> String {
    format!("{}:{:02}", ms / 60_000, ms / 1000 % 60)
}

/// A name that is safe as a file name everywhere.
fn clean(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c }).collect();
    let s = s.trim().trim_end_matches('.').trim();
    if s.is_empty() { "track".into() } else { s.chars().take(120).collect() }
}

/// A stored path made safe, keeping its folders.
fn clean_path(path: &str) -> PathBuf {
    path.split(['/', '\\']).filter(|p| !p.is_empty() && *p != "." && *p != "..").map(clean).collect()
}

/// Dropping files on rommusic opens a console that closes as soon as it ends: keep it open
/// until Enter so what happened can be read.
fn finish(code: i32) -> i32 {
    if owns_console() {
        print!("\nPress Enter to close...");
        let _ = std::io::stdout().flush();
        let _ = std::io::stdin().read_line(&mut String::new());
    }
    code
}

#[cfg(windows)]
fn owns_console() -> bool {
    extern "system" {
        fn GetConsoleProcessList(list: *mut u32, count: u32) -> u32;
    }
    let mut list = [0u32; 2];
    unsafe { GetConsoleProcessList(list.as_mut_ptr(), 2) == 1 }
}

#[cfg(not(windows))]
fn owns_console() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Args, String> {
        parse(&list.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn a_dropped_file_gets_the_defaults() {
        let a = args(&["game.sfc"]).unwrap();
        assert!(a.rate.is_none() && a.bits == 16 && a.format == Format::Flac && !a.all);
    }

    #[test]
    fn the_presets_set_rate_and_depth() {
        let a = args(&["--hires", "x"]).unwrap();
        assert_eq!((a.rate, a.bits), (Some(96_000), 24));
        let a = args(&["--rate", "dvd", "--bits", "16", "x"]).unwrap();
        assert_eq!((a.rate, a.bits), (Some(48_000), 16));
        let a = args(&["--cd", "x"]).unwrap();
        assert_eq!((a.rate, a.bits), (Some(44_100), 16));
        assert!(args(&["--format", "mp3", "x"]).is_err());
        assert!(args(&["--all"]).is_err(), "no input");
    }

    #[test]
    fn names_are_safe_file_names() {
        assert_eq!(clean("Boss: Final / Last?"), "Boss_ Final _ Last_");
        assert_eq!(clean("..."), "track");
        assert_eq!(clean_path("../BGM/../a.adx"), PathBuf::from("BGM").join("a.adx"));
    }
}
