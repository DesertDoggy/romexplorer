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
    if !a.o.originals.is_empty() {
        eprintln!("reading the original-format music sets in {}", a.o.originals.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "));
    }
    let library = rommusic::load_library(&a.o.originals, &mut |_| {});
    let cancel = AtomicBool::new(false);
    let mut failed = 0;
    for input in &a.inputs {
        let result = if a.list { list(input, &a.o, library.as_ref()) } else { rommusic::process(input, &a.o, library.as_ref(), &cancel, &mut print_event) };
        if let Err(e) = result {
            eprintln!("{}: {e}", input.display());
            failed += 1;
        }
    }
    finish(i32::from(failed > 0))
}

fn list(input: &std::path::Path, o: &Options, library: Option<&romexplorer::rom::rip_match::Library>) -> Result<(), String> {
    let plan = rommusic::prepare(input, o, library)?;
    println!("{} -> {}", input.display(), plan.folder.display());
    for (n, job) in plan.jobs.iter().enumerate() {
        println!("  {:02}  {}", n + 1, job.describe());
    }
    Ok(())
}

fn print_event(event: Event) {
    match event {
        Event::Found { input, folder, .. } => println!("{} -> {}", input.display(), folder.display()),
        Event::TrackStarted { number, total, name } => {
            let width = if total >= 100 { 3 } else { 2 };
            print!("  {number:0width$} {name} ... ");
            let _ = std::io::stdout().flush();
        }
        Event::TrackWritten { ms, .. } => println!("{}", rommusic::length(ms)),
        Event::TrackFailed { error, .. } => println!("failed: {error}"),
        Event::OriginalsWritten { count, folder } => println!("  {count} original file(s) in {}", folder.display()),
        Event::NoOriginals => println!("  (no original files: CD audio and music sequenced in a cartridge have none)"),
        Event::Library { .. } | Event::Reading { .. } | Event::TrackProgress { .. } => {}
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
