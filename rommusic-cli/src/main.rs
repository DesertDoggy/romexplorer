//! rommusic: drop a ROM, disc image, dumped folder or set of music files in an original audio
//! format on it and it writes the music as FLAC, in a "<name> (music)" folder beside what was
//! dropped. The work itself is the library (src/lib.rs), shared with `rommusic-gui`. See
//! rommusic-cli/README.md.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use rommusic::{Event, Format, Options, VERSION};

fn usage() -> String {
    format!(
        "rommusic {VERSION}: the music in a ROM, as FLAC.

Usage: rommusic [options] <ROM, disc image, folder or original-format music set>...

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
                    stores it (the stream files on a disc, the original-format music files, an .spc
                    from a SNES cartridge) into an \"original\" folder
  --originals DIR   a folder of music in original audio formats (NSF, SPC, GBS, HES, KSS, SGC, zipped or not)
                    to match cartridges against by their content; may be given more than once
  --ps2-bios FILE   a PS2 BIOS image (4 MB) of your own, which PS1 and PS2 music files (PSF, PSF2)
                    need to play; without it they are skipped
  --seconds N       SNES: how long the game runs before its music is taken (default 20)
  --start           SNES: press Start now and then while it runs, for games whose music
                    begins after the title screen
  --list            only list the tracks
  --verbose         say why a file or track was skipped
  --no-pause        don't wait for Enter at the end (it only waits in a window opened
                    for a dropped file)
  -h, --help        this text
  -V, --version     the version"
    )
}

struct Args {
    inputs: Vec<PathBuf>,
    o: Options,
    list: bool,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut a = Args { inputs: Vec::new(), o: Options { bits: 0, ..Options::default() }, list: false };
    let mut it = args.iter();
    let value = |it: &mut std::slice::Iter<String>, flag: &str| it.next().cloned().ok_or_else(|| format!("{flag} needs a value"));
    while let Some(arg) = it.next() {
        let o = &mut a.o;
        match arg.as_str() {
            "--all" => o.all = true,
            "--list" => a.list = true,
            "--start" => o.press_start = true,
            "--no-pause" | "--verbose" => {}
            "--cd" => o.rate = Some(44_100),
            "--dvd" => (o.rate, o.bits) = (Some(48_000), if o.bits == 0 { 24 } else { o.bits }),
            "--hires" => (o.rate, o.bits) = (Some(96_000), if o.bits == 0 { 24 } else { o.bits }),
            "--max" => o.max = Some(value(&mut it, arg)?.parse().map_err(|_| "--max needs a number")?),
            "--out" => o.out = Some(PathBuf::from(value(&mut it, arg)?)),
            "--originals" | "--rips" => o.originals.push(PathBuf::from(value(&mut it, arg)?)),
            "--ps2-bios" => o.ps2_bios = Some(PathBuf::from(value(&mut it, arg)?)),
            "--seconds" => o.seconds = value(&mut it, arg)?.parse().ok().filter(|s: &f32| (1.0..=600.0).contains(s)).ok_or("--seconds needs 1-600")?,
            "--bits" => o.bits = value(&mut it, arg)?.parse().ok().filter(|b| [16, 24].contains(b)).ok_or("--bits is 16 or 24")?,
            "--rate" => {
                let v = value(&mut it, arg)?;
                o.rate = match v.as_str() {
                    "native" => None,
                    "cd" => Some(44_100),
                    "dvd" => {
                        o.bits = if o.bits == 0 { 24 } else { o.bits };
                        Some(48_000)
                    }
                    "hires" => {
                        o.bits = if o.bits == 0 { 24 } else { o.bits };
                        Some(96_000)
                    }
                    hz => Some(hz.parse().ok().filter(|r| (8_000..=192_000).contains(r)).ok_or("--rate is native, cd, dvd, hires or 8000-192000")?),
                };
            }
            "--format" => {
                o.format = match value(&mut it, arg)?.as_str() {
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
    if a.o.bits == 0 {
        a.o.bits = 16;
    }
    if a.inputs.is_empty() {
        return Err("nothing to read".into());
    }
    Ok(a)
}

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    NO_PAUSE.store(args.iter().any(|a| a == "--no-pause"), std::sync::atomic::Ordering::Relaxed);
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(if args.iter().any(|a| a == "--verbose") { log::LevelFilter::Debug } else { log::LevelFilter::Warn });
    if args.is_empty() {
        // Opened with nothing dropped on it: the usage stays up until Enter.
        println!("{}", usage());
        return finish(0);
    }
    if args.iter().any(|a| a == "-h" || a == "--help") {
        // Asked for in a shell (or by a script, which has no one to press Enter).
        println!("{}", usage());
        return 0;
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
    if !rommusic::libraries_available() {
        eprintln!("rommusic: the music libraries (rvgmstream, rommusic) were not found in lib/ beside rommusic");
        return finish(1);
    }
    if let Some(bios) = &a.o.ps2_bios {
        match rommusic::load_ps2_bios(bios) {
            Ok(true) => {}
            Ok(false) => eprintln!("rommusic: this build has no PS1/PS2 player; --ps2-bios is ignored"),
            Err(e) => {
                eprintln!("rommusic: {e}");
                return finish(2);
            }
        }
    }
    let mut printer = Printer::default();
    if !a.o.originals.is_empty() {
        eprintln!("reading the original-format music sets in {}", a.o.originals.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "));
    }
    let library = rommusic::load_library(&a.o.originals, &mut |event| printer.event(event));
    printer.clear();
    let cancel = AtomicBool::new(false);
    let mut failed = 0;
    let folders = rommusic::music_folders(&a.inputs, &a.o);
    let total = a.inputs.len();
    for (n, (input, folder)) in a.inputs.iter().zip(&folders).enumerate() {
        let name = input.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| input.display().to_string());
        printer.input = format!("input {} of {total}: {name}", n + 1);
        if !a.list {
            println!("[{}/{total}] {name}: looking for music", n + 1);
        }
        let result = if a.list { list(input, folder, &a.o, library.as_ref(), &mut printer) } else { rommusic::process(input, folder, &a.o, library.as_ref(), &cancel, &mut |event| printer.event(event)) };
        printer.clear();
        if let Err(e) = result {
            eprintln!("{}: {e}", input.display());
            failed += 1;
        }
    }
    finish(i32::from(failed > 0))
}

fn list(input: &std::path::Path, folder: &std::path::Path, o: &Options, library: Option<&romexplorer::rom::rip_match::Library>, printer: &mut Printer) -> Result<(), String> {
    let plan = rommusic::prepare_with(input, o, library, &mut |event| printer.event(event))?;
    printer.clear();
    println!("{} -> {}", input.display(), folder.display());
    for (n, job) in plan.jobs.iter().enumerate() {
        println!("  {:02}  {}", n + 1, job.describe());
    }
    Ok(())
}

/// What a run prints: finished lines on stdout, and one status line on stderr rewritten in
/// place (`\r`) while something long goes on.
#[derive(Default)]
struct Printer {
    /// "input 2 of 9: name.chd", ahead of what it is doing.
    input: String,
    /// Characters on the status line now.
    width: usize,
    /// "  03 name ... ", while that track is being written.
    track: Option<String>,
}

impl Printer {
    /// Kept short of a usual console's width: a wrapped line cannot be rewritten in place.
    const MAX: usize = 110;

    fn status(&mut self, text: &str) {
        let text: String = if text.chars().count() > Self::MAX { text.chars().take(Self::MAX - 1).chain(std::iter::once('…')).collect() } else { text.to_string() };
        let shown = text.chars().count();
        eprint!("\r{text}{:pad$}", "", pad = self.width.saturating_sub(shown));
        self.width = shown;
        let _ = std::io::stderr().flush();
    }

    fn clear(&mut self) {
        if self.width > 0 {
            eprint!("\r{:pad$}\r", "", pad = self.width);
            let _ = std::io::stderr().flush();
            self.width = 0;
        }
    }

    /// The track line, ending in `tail` ("42%", "encoding FLAC"), rewritten in place.
    fn track(&mut self, tail: &str) {
        if let Some(prefix) = &self.track {
            print!("\r{prefix}{tail}      ");
            let _ = std::io::stdout().flush();
        }
    }

    fn event(&mut self, event: Event) {
        let n = rommusic::thousands;
        match event {
            Event::LibraryListing { found } => self.status(&format!("walking original-format folders: {} set(s) found so far", n(found))),
            Event::Library { done, total, set: Some(set) } => self.status(&format!("reading original-format music sets: {} of {} ({set})", n(done + 1), n(total))),
            Event::Library { set: None, .. } => self.clear(),
            Event::Probing { file, count, .. } => {
                let input = self.input.clone();
                self.status(&format!("{input} - probing {}: {file}", n(count)));
            }
            Event::Running { second, of } => {
                let input = self.input.clone();
                self.status(&format!("{input} - running the game to start its music: {second} of {of} s"));
            }
            Event::Found { input, folder, .. } => {
                self.clear();
                println!("{} -> {}", input.display(), folder.display());
            }
            Event::TrackStarted { number, total, name } => {
                self.clear();
                let width = if total >= 100 { 3 } else { 2 };
                let prefix = format!("  {number:0width$} {name} ... ");
                print!("{prefix}");
                let _ = std::io::stdout().flush();
                self.track = Some(prefix);
            }
            Event::TrackProgress { fraction, .. } => self.track(&format!("{:.0}%", fraction * 100.0)),
            Event::TrackEncoding { .. } => self.track("encoding FLAC"),
            Event::TrackWritten { ms, .. } => {
                // Padded over whatever the line said last ("encoding FLAC").
                println!("\r{}{}{:20}", self.track.take().unwrap_or_default(), rommusic::length(ms), "");
            }
            Event::TrackFailed { error, .. } => {
                println!("\r{}failed: {error}{:20}", self.track.take().unwrap_or_default(), "");
            }
            Event::Copying { number, total, name, bytes } => self.status(&format!("copying original files: {} of {} ({name}, {})", n(number), n(total), rommusic::size(bytes))),
            Event::OriginalsWritten { count, folder } => {
                self.clear();
                println!("  {count} original file(s) in {}", folder.display());
            }
            Event::NoOriginals => {
                self.clear();
                println!("  (no original files: CD audio and music sequenced in a cartridge have none)");
            }
            Event::Reading { .. } => {}
        }
    }
}

/// romexplorer's own messages on stderr: warnings, and with --verbose why things were skipped.
struct Logger;
static LOGGER: Logger = Logger;

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.target().starts_with("romexplorer")
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            eprintln!("  ({}) {}", r.level().as_str().to_lowercase(), r.args());
        }
    }
    fn flush(&self) {}
}

/// Dropping files on rommusic opens a console that closes as soon as it ends: keep it open
/// until Enter so what happened can be read.
static NO_PAUSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn finish(code: i32) -> i32 {
    use std::io::IsTerminal;
    // Only a console opened for this run, read and written by a person: not a shell's, not
    // a pipe or a redirect, and not with --no-pause.
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if interactive && !NO_PAUSE.load(std::sync::atomic::Ordering::Relaxed) && owns_console() {
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
        assert!(a.o.rate.is_none() && a.o.bits == 16 && a.o.format == Format::Flac && !a.o.all);
    }

    #[test]
    fn the_presets_set_rate_and_depth() {
        let a = args(&["--hires", "x"]).unwrap();
        assert_eq!((a.o.rate, a.o.bits), (Some(96_000), 24));
        let a = args(&["--rate", "dvd", "--bits", "16", "x"]).unwrap();
        assert_eq!((a.o.rate, a.o.bits), (Some(48_000), 16));
        let a = args(&["--cd", "x"]).unwrap();
        assert_eq!((a.o.rate, a.o.bits), (Some(44_100), 16));
        assert!(args(&["--format", "mp3", "x"]).is_err());
        assert!(args(&["--all"]).is_err(), "no input");
    }
}
