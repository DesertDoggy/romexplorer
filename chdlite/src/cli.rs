//! The console front end behind `chdlite`, `chdcomp`, `chdread` and `chdhash`.
//!
//! The four differ only in what a bare path means, which is what a file dropped onto the
//! executable is. Every one of them accepts every command and flag.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

use crate::batch::{self, Batch, BatchEvent, WORKER_FLAG};
use crate::cart::{N64_FORMATS, NES_FORMATS, SNES_FORMATS};
use crate::classify::expand;
use crate::job::Mode;
use crate::settings::{CompMode, LitePreset, Settings, CUE_STYLES, GDROM_SHEETS, RVZ_BLOCKS, RVZ_CODECS};

/// What the binary does with a bare path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultMode {
    /// `chdlite`: create with the fast preset; a `.chd`/`.rvz` is extracted.
    Auto,
    /// `chdcomp`: create for size; a `.chd`/`.rvz` is extracted.
    Comp,
    /// `chdread`
    Read,
    /// `chdhash`
    Hash,
}

impl DefaultMode {
    fn mode(self) -> Mode {
        match self {
            Self::Auto => Mode::Lite,
            Self::Comp => Mode::Comp,
            Self::Read => Mode::Read,
            Self::Hash => Mode::Hash,
        }
    }
}

fn usage(name: &str) -> String {
    format!(
        "{name} {version} -- disc images to CHD / RVZ, and back; Wii U and Xbox discs to WUA / ZAR / XISO;
NES / N64 / SNES cartridge dumps into the header or byte order you pick; 3DS titles decrypted

Usage: {name} [command] <file|dir>... [options]
       drop files or folders onto the executable

Commands (default for {name}: {default}):
  auto      .chd/.rvz -> extract, anything else -> create (fast preset)
  create    create with the fast preset
  comp      create for the smallest file
  extract   .chd -> cue+bin / gdi+bin (GD-ROM) / iso, .rvz -> iso, .cso -> iso, .pbp -> cue+bin,
            .zcci / .zcia / .zcxi / .z3dsx -> .cci / .cia / .cxi / .3dsx
  read      show what a file is
  hash      hash the content inside a .chd/.rvz/.cso/.pbp (or the files themselves)

The container is chosen from the image: CD and GD-ROM -> CD CHD, DVD -> DVD CHD,
GameCube / Wii -> RVZ, .cso -> DVD CHD, .pbp -> CD CHD. Three are lossy, because their
emulators read no lossless container: Wii U .wud/.wux -> WUA, Xbox 360 -> ZAR, Xbox ->
XISO. Those keep the game's files but not the disc, so they can't be turned back into
the original image. A Wii U disc needs its key: the .key file beside it, or a keys.txt
beside it or this program. Only lossless inputs are taken: not .wua, .zar or an XISO.

Cartridge dumps (.nes .unh .z64 .v64 .n64 .sfc .smc .swc) are not compressed: create and
comp write them in the layout set for their platform, and skip one already in it. Whether
a NES dump has a header is looked up by its hash in the NES header database; a hash that
is not there is an error. An SNES copier header is told by the size (whole KB + 512), and
the cartridge's own header has to agree.

3DS titles (.cia .3ds .cci .cxi .3dsx and the compressed .zcci .zcia .zcxi .z3dsx) are
decrypted into the format set with --3ds: a game becomes .cci / .3ds, its main part alone
.cxi, or either compressed for Azahar (.zcci, .zcxi); updates, DLC, demos and system titles
stay a decrypted .cia (.zcia when compressed). No keys come with this program: put your
aes_keys.txt (or boot9.bin) and, for titles with seed crypto, seeddb.bin beside the files,
beside this program, or in its assets folder.

Options:
  -o, --output <dir>          write here instead of next to each input
  -f, --force                 replace outputs that already exist
  -np, --threads <n>          threads in total (default: auto, every core)
  -jf, --concurrent-files <n> files converted at once (default: auto, a quarter of the
                              threads, at most 4)
  --preset pc|mobile          fast preset to use for create
  --comp best|chdman|custom   what comp does
  -c, --compression <list>    CHD codecs, up to 4: cdlz,cdzs,cdzl,cdfl for a CD,
                              lzma,zstd,zlib,huff,flac for a DVD (implies comp custom)
  -hs, --hunksize <bytes>     CHD hunk size (implies comp custom): a multiple of 2448
                              sets the CD hunk, a multiple of 2048 the DVD hunk
  --rvz-codec <name>          {rvz_codecs}
  --rvz-level <n>             zstd 1-22, others 1-9
  --rvz-block <size>          {rvz_blocks}
  --rvz-scrub                 drop junk data (not reversible)
  --hash <list>               sha1,md5,crc32,sha256,xxh3,rahash (default sha1,rahash;
                              rahash also says whether RetroAchievements has achievements for it)
  --cue-style <style>         {cue_styles}: the extracted cue sheet's dialect
  --gdrom-sheet <kind>        {gdrom_sheets}: an extracted GD-ROM's sheet (default gdi)
  --nes <format>              {nes}: with the iNES header (.nes), without
                              it (.nes), or without it as .unh
  --n64 <format>              {n64}: big-endian, byte-swapped, little-endian
  --snes <format>             {snes}: without or with a 512-byte copier header
  --3ds <format>              {n3ds}: what a 3DS game becomes
  --version, --help

Defaults come from {settings}",
        version = crate::VERSION,
        default = match name {
            "chdcomp" => "comp",
            "chdread" => "read",
            "chdhash" => "hash",
            _ => "auto",
        },
        rvz_codecs = RVZ_CODECS.join("|"),
        rvz_blocks = RVZ_BLOCKS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join("|"),
        cue_styles = CUE_STYLES.join("|"),
        gdrom_sheets = GDROM_SHEETS.join("|"),
        nes = NES_FORMATS.join("|"),
        n64 = N64_FORMATS.join("|"),
        snes = SNES_FORMATS.join("|"),
        n3ds = crate::n3ds::FORMATS.join("|"),
        settings = Settings::path().display(),
    )
}

struct Parsed {
    mode: Mode,
    paths: Vec<PathBuf>,
    settings: Settings,
}

fn list(value: &str) -> Vec<String> {
    value.split(',').map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()).collect()
}

fn parse(args: &[String], default: DefaultMode, mut settings: Settings) -> Result<Parsed, String> {
    let mut mode = default.mode();
    let mut rest = args;
    // A command word that happens to also be a file in the current directory is the file.
    if let Some(first) = args.first() {
        let command = match first.as_str() {
            "auto" | "create" => Some(Mode::Lite),
            "comp" => Some(Mode::Comp),
            "extract" => Some(Mode::Extract),
            "read" | "info" => Some(Mode::Read),
            "hash" => Some(Mode::Hash),
            _ => None,
        };
        if let Some(command) = command.filter(|_| !Path::new(first).exists()) {
            mode = command;
            rest = &args[1..];
        }
    }

    let mut paths = Vec::new();
    let mut custom = false;
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        let mut value = |flag: &str| iter.next().cloned().ok_or_else(|| format!("{flag} needs a value"));
        let number = |flag: &str, text: String| text.parse::<u32>().map_err(|_| format!("{flag}: '{text}' is not a number"));
        match arg.as_str() {
            "-o" | "--output" => settings.output.dir = value(arg)?,
            "-f" | "--force" => settings.output.force = true,
            "-np" | "-j" | "--threads" | "--numprocessors" => settings.jobs.threads = number(arg, value(arg)?)?,
            "-jf" | "--concurrent-files" => settings.jobs.concurrent_files = number(arg, value(arg)?)?,
            "--preset" => {
                settings.lite.preset = match value(arg)?.as_str() {
                    "pc" => LitePreset::Pc,
                    "mobile" | "android" => LitePreset::Mobile,
                    other => return Err(format!("--preset: '{other}' is not pc or mobile")),
                }
            }
            "--comp" => {
                settings.comp.mode = match value(arg)?.as_str() {
                    "best" => CompMode::Best,
                    "chdman" => CompMode::Chdman,
                    "custom" => CompMode::Custom,
                    other => return Err(format!("--comp: '{other}' is not best, chdman or custom")),
                }
            }
            "-c" | "--compression" => {
                // One flag serves both media: CD codecs go to the CD list and the rest to the
                // DVD list, so a mixed folder can be given both at once.
                let (cd, dvd): (Vec<String>, Vec<String>) = list(&value(arg)?).into_iter().partition(|name| name.starts_with("cd"));
                if !cd.is_empty() {
                    settings.comp.cd_codecs = cd;
                }
                if !dvd.is_empty() {
                    settings.comp.dvd_codecs = dvd;
                }
                custom = true;
            }
            "-hs" | "--hunksize" => {
                // Like -c, one flag for both media: the size goes to whichever it is valid
                // for (a CD hunk is whole 2448-byte frames, a DVD hunk whole 2048-byte sectors).
                let hunk = number(arg, value(arg)?)?;
                let (cd, dvd) = (hunk > 0 && hunk % 2448 == 0, hunk > 0 && hunk % 2048 == 0);
                if !cd && !dvd {
                    return Err(format!("{arg}: {hunk} is neither a CD hunk (a multiple of 2448) nor a DVD hunk (a multiple of 2048)"));
                }
                if cd {
                    settings.comp.cd_hunk_size = hunk;
                }
                if dvd {
                    settings.comp.dvd_hunk_size = hunk;
                }
                custom = true;
            }
            "--rvz-codec" => settings.rvz.codec = value(arg)?.to_ascii_lowercase(),
            "--rvz-level" => settings.rvz.level = number(arg, value(arg)?)? as i32,
            "--rvz-block" => {
                let block = value(arg)?.to_ascii_lowercase();
                if !RVZ_BLOCKS.iter().any(|(name, _)| *name == block) {
                    return Err(format!("--rvz-block: '{block}' is not one of {}", RVZ_BLOCKS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")));
                }
                settings.rvz.block = block;
            }
            "--rvz-scrub" => settings.rvz.scrub = true,
            "--hash" | "-hash" => settings.hash.algorithms = list(&value(arg)?),
            "--cue-style" => settings.extract.cue_style = value(arg)?.to_ascii_lowercase(),
            "--gdrom-sheet" => {
                let sheet = value(arg)?.to_ascii_lowercase();
                if !GDROM_SHEETS.contains(&sheet.as_str()) {
                    return Err(format!("--gdrom-sheet: '{sheet}' is not one of {}", GDROM_SHEETS.join(", ")));
                }
                settings.extract.gdrom_sheet = sheet;
            }
            "--nes" | "--n64" | "--snes" => {
                let format = value(arg)?.to_ascii_lowercase();
                let (all, field): (&[&str], &mut String) = match arg.as_str() {
                    "--nes" => (&NES_FORMATS, &mut settings.cartridge.nes),
                    "--n64" => (&N64_FORMATS, &mut settings.cartridge.n64),
                    _ => (&SNES_FORMATS, &mut settings.cartridge.snes),
                };
                if !all.contains(&format.as_str()) {
                    return Err(format!("{arg}: '{format}' is not one of {}", all.join(", ")));
                }
                *field = format;
            }
            "--3ds" => {
                let format = value(arg)?.to_ascii_lowercase();
                crate::n3ds::target(&format).map_err(|err| format!("{arg}: {err}"))?;
                settings.n3ds.format = format;
            }
            "-i" | "--input" => paths.push(PathBuf::from(value(arg)?)),
            flag if flag.starts_with('-') && !Path::new(flag).exists() => return Err(format!("unknown option {flag}")),
            path => paths.push(PathBuf::from(path)),
        }
    }
    if custom {
        settings.comp.mode = CompMode::Custom;
        if mode == Mode::Lite {
            mode = Mode::Comp;
        }
    }
    Ok(Parsed { mode, paths, settings })
}

/// Whether this console exists only for us: started by a double-click or a drop in Explorer,
/// so it vanishes with the process and takes the output with it.
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

fn finish(code: i32) -> i32 {
    if owns_console() {
        print!("\nPress Enter to close...");
        let _ = std::io::stdout().flush();
        let _ = std::io::stdin().read_line(&mut String::new());
    }
    code
}

fn exe_name() -> String {
    std::env::current_exe().ok().and_then(|p| p.file_stem().map(|s| s.to_string_lossy().to_lowercase())).unwrap_or_else(|| "chdlite".to_string())
}

/// Runs the console tool and returns the process exit code: 0 on success, 1 when anything
/// failed, 2 for a usage error.
pub fn main(default: DefaultMode) -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some(WORKER_FLAG) {
        return batch::worker_main(args.get(1).map(String::as_str).unwrap_or(""));
    }
    let name = exe_name();
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{}", usage(&name));
        return finish(0);
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("{name} {}", crate::VERSION);
        println!("{}", crate::ra::list_summary());
        return 0;
    }
    if let Err(err) = crate::natives::ensure() {
        eprintln!("{name}: {err}");
        return finish(1);
    }
    if let Some(problem) = Settings::problem() {
        eprintln!("warning: {problem}");
    }
    let parsed = match parse(&args, default, Settings::load()) {
        Ok(parsed) => parsed,
        Err(err) => {
            eprintln!("{name}: {err}\n\n{}", usage(&name));
            return finish(2);
        }
    };
    if let Some(missing) = parsed.paths.iter().find(|path| !path.exists()) {
        eprintln!("{name}: no such file or directory: {}", missing.display());
        return finish(2);
    }
    let inputs = expand(&parsed.paths);
    if inputs.is_empty() {
        eprintln!("{name}: nothing to do -- no disc images, .chd, .rvz, .cso, .pbp, cartridge dumps or 3DS titles in what was given");
        return finish(2);
    }
    if matches!(parsed.mode, Mode::Lite | Mode::Comp) && parsed.settings.output.lossy_warning {
        let lossy: Vec<String> = crate::classify::lossy_targets(&inputs).into_iter().map(|(path, target)| format!("{} -> {}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), target.label())).collect();
        if !lossy.is_empty() {
            eprintln!("{name}: warning: lossy, not for bit-perfect archiving (the disc image can't be got back): {}", lossy.join(", "));
        }
    }

    let total = inputs.len();
    let label = |index: usize| inputs[index].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let batch = Batch { mode: parsed.mode, inputs: inputs.clone(), settings: parsed.settings, always_workers: false };
    // Per-file percent, for one status line under whatever has been printed so far.
    let percents: Mutex<Vec<f32>> = Mutex::new(vec![0.0; total]);
    let quiet = parsed.mode == Mode::Read;
    let status = |percents: &[f32]| {
        if quiet {
            return;
        }
        let done = percents.iter().filter(|p| **p >= 100.0).count();
        let overall = percents.iter().sum::<f32>() / total as f32;
        eprint!("\r  {overall:5.1}%  {done}/{total} done ");
        let _ = std::io::stderr().flush();
    };
    let clear = || eprint!("\r{:40}\r", "");
    let cancel = AtomicBool::new(false);
    let failed = batch::run(&batch, &cancel, &|event| {
        let mut percents = percents.lock().unwrap();
        match event {
            BatchEvent::Started { .. } => {}
            BatchEvent::Progress { index, percent } => {
                percents[index] = percent.clamp(0.0, 99.9);
                status(&percents);
            }
            BatchEvent::Message { index, text } => {
                clear();
                println!("[{}/{total}] {}: {text}", index + 1, label(index));
            }
            BatchEvent::Finished { index, result } => {
                percents[index] = 100.0;
                clear();
                match result {
                    // A multi-line result (read, hash) is a report of its own.
                    Ok(text) if text.contains('\n') => println!("{text}\n"),
                    Ok(text) => println!("[{}/{total}] {}: {text}", index + 1, label(index)),
                    Err(err) => println!("[{}/{total}] {}: FAILED: {err}", index + 1, label(index)),
                }
                if percents.iter().any(|p| *p < 100.0) {
                    status(&percents);
                }
            }
        }
    });
    if total > 1 {
        println!("{} done, {failed} failed", total - failed);
    }
    finish(if failed == 0 { 0 } else { 1 })
}
