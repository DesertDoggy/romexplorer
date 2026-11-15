//! One input, one operation.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use romexplorer::common::models::{CueSheetStyle, Platform};
use romexplorer::rom::cemu_wrapper;
use romexplorer::rom::chd_info::read_chd_metadata;
use romexplorer::rom::chdman_wrapper::ChdReader;
use romexplorer::rom::content::{ContentPlan, InputSet};
use romexplorer::rom::decode_threads::DecodeThreads;
use romexplorer::rom::detect::sheet::{CueSheetOptions, SheetKind};
use romexplorer::rom::detect::{hinted_metadata_cycle, HeuristicFlags};
use romexplorer::rom::dolphin_wrapper::{DiscCompression, DiscReader};
use romexplorer::rom::hash::HashFlags;
use romexplorer::rom::models::{ArchiveArgs, ChdArgs, ChdCodecs, ChdPresets, RvzArgs, RvzBlockSize, RvzCodecs};
use romexplorer::rom::operation::{Detect, ProgressEvent, Single, SingleArgs, SingleError, SingleOutcome, StageResult, StageSwitches};
use romexplorer::rom::platforms::helpers::{is_chd_platform, is_rvz_platform};
use romexplorer::rom::sheet_build::{rebuild_cue, Expected, TrackFile};

use crate::cart::Cart;
use crate::classify::{classify, Input, Target};
use crate::settings::{CompMode, LitePreset, Settings};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Create, tuned for speed. A `.chd`/`.rvz` is extracted instead.
    Lite,
    /// Create, tuned for size. A `.chd`/`.rvz` is extracted instead.
    Comp,
    Extract,
    Read,
    Hash,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Lite => "Lite",
            Self::Comp => "Comp",
            Self::Extract => "Extract",
            Self::Read => "Read",
            Self::Hash => "Hash",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSpec {
    pub mode: Mode,
    pub input: PathBuf,
    pub settings: Settings,
    /// Compression/decode threads for this job; `None` = every core.
    pub threads: Option<u32>,
    /// The name its outputs take in place of the input's own stem: set by a batch when another
    /// of its jobs would write the same file ([`crate::batch`]).
    #[serde(default)]
    pub output_stem: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    /// 0..=100.
    Progress(f32),
    Message(String),
    /// An output this job has claimed ([`claim_output`]) and is about to write, so whoever
    /// supervises it can roll a partial one back ([`Writing::undo`]).
    Output(PathBuf),
    /// A scratch file or folder of this job's own, removed when the job does not finish.
    Scratch(PathBuf),
}

/// What a running job has announced it is writing: its outputs and its scratch files.
#[derive(Debug, Default, Clone)]
pub struct Writing {
    claimed: Vec<PathBuf>,
    scratch: Vec<PathBuf>,
}

impl Writing {
    pub fn note(&mut self, event: &Event) {
        match event {
            Event::Output(path) => self.claimed.push(path.clone()),
            Event::Scratch(path) => self.scratch.push(path.clone()),
            _ => {}
        }
    }

    /// Undoes a job that did not finish: each output still being written goes and the file it
    /// was replacing comes back; one whose claim already finished or was rolled back is left
    /// as it is. Scratch files and folders go.
    pub fn undo(&self) {
        for output in &self.claimed {
            roll_back(output);
        }
        for path in &self.scratch {
            if path.is_dir() {
                let _ = std::fs::remove_dir_all(path);
            } else {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

/// Runs one job on the calling thread. `on_event` returning `false` asks it to stop, which
/// RVZ creation, extraction and hashing honour at once; chdman has no cancel hook, so a CHD
/// creation only stops this way when it runs as a worker process that can be killed
/// ([`crate::batch`]).
///
/// `Ok` carries the text to show for the finished job. On `Err` every output this job
/// started is rolled back -- a file it was replacing is back where it was -- and its scratch
/// files are removed.
pub fn run_one(spec: &JobSpec, on_event: &mut dyn FnMut(Event) -> bool) -> Result<String, String> {
    let mut writing = Writing::default();
    let result = run_inner(spec, &mut |event| {
        writing.note(&event);
        on_event(event)
    });
    if result.is_err() {
        writing.undo();
    }
    result
}

fn run_inner(spec: &JobSpec, on_event: &mut dyn FnMut(Event) -> bool) -> Result<String, String> {
    let input = classify(&spec.input)?;
    match (spec.mode, input) {
        (Mode::Read, input) => read(&spec.input, &input, &spec.settings),
        (Mode::Hash, _) => content_pass(spec, None, on_event),
        (Mode::Extract | Mode::Lite | Mode::Comp, Input::Chd | Input::Rvz) | (Mode::Extract, Input::Cso | Input::Pbp) => {
            let dir = output_dir(spec)?;
            content_pass(spec, Some(dir), on_event)
        }
        (Mode::Lite | Mode::Comp, Input::Cso | Input::Pbp) => create_from_container(spec, on_event),
        (Mode::Extract, Input::N3ds) => extract_n3ds(spec, on_event),
        (Mode::Extract, Input::Image { .. } | Input::Cartridge(_)) => Err("nothing to extract: not a .chd, .rvz, .cso or .pbp".to_string()),
        (Mode::Lite | Mode::Comp, Input::N3ds) => convert_n3ds(spec, on_event),
        (Mode::Lite | Mode::Comp, Input::Cartridge(cart)) => convert_cartridge(spec, cart, on_event),
        (Mode::Lite | Mode::Comp, Input::Image { target: Target::Wua, source, .. }) => create_wua(spec, &source, on_event),
        (Mode::Lite | Mode::Comp, Input::Image { target: Target::Zar, source, .. }) => create_zar(spec, &source, on_event),
        (Mode::Lite | Mode::Comp, Input::Image { target: Target::Xiso, source, .. }) => create_xiso(spec, &source, on_event),
        (Mode::Lite | Mode::Comp, Input::Image { target, source, needs_cue }) => create(spec, target, &source, needs_cue, on_event),
    }
}

/// Where a job writes ([`Settings::output_dir_for`]), not yet made.
fn planned_dir(spec: &JobSpec) -> PathBuf {
    spec.settings.output_dir_for(&spec.input)
}

/// What a job will write, worked out before it runs, so a batch can tell two jobs that would
/// write the same file apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Planned {
    /// One output named after the job's stem ([`JobSpec::output_stem`] renames it).
    Named {
        dir: PathBuf,
        stem: String,
        /// `None` when it depends on the content (a cartridge's layout, a 3DS title's kind).
        extension: Option<String>,
    },
    /// Files named by the content itself (what a disc image's sheet calls its tracks).
    Files(Vec<PathBuf>),
    /// Writes nothing, or fails before it would.
    Nothing,
}

/// [`Planned`] for `spec`: classifies the input and, for an extraction, lists the content.
pub fn plan(spec: &JobSpec) -> Planned {
    let Ok(input) = classify(&spec.input) else { return Planned::Nothing };
    let dir = planned_dir(spec);
    let named = |extension: Option<&str>| Planned::Named { dir: dir.clone(), stem: out_stem(spec), extension: extension.map(str::to_string) };
    let content = |chds: bool| {
        let Ok(base) = cue_style(&spec.settings.extract.cue_style).map(|style| SingleArgs::default().with_sheet_style(style)) else { return Planned::Nothing };
        let mut base = base;
        if !chds {
            let Ok(sheet) = gdrom_sheet(&spec.settings.extract.gdrom_sheet) else { return Planned::Nothing };
            base.cd_sheet = sheet;
        }
        let Ok(set) = InputSet::resolve(&spec.input) else { return Planned::Nothing };
        let Ok(plan) = ContentPlan::build(&set, &base.cue_sheet, base.cd_sheet) else { return Planned::Nothing };
        let names = plan.files.iter().map(|file| Path::new(&file.name));
        Planned::Files(if chds {
            names.filter(|name| matches!(crate::classify::extension(name).as_str(), "cue" | "iso")).map(|name| dir.join(format!("{}.chd", stem(name)))).collect()
        } else {
            names.map(|name| dir.join(name)).collect()
        })
    };
    match (spec.mode, input) {
        (Mode::Read | Mode::Hash, _) => Planned::Nothing,
        (Mode::Extract | Mode::Lite | Mode::Comp, Input::Chd | Input::Rvz) | (Mode::Extract, Input::Cso | Input::Pbp) => content(false),
        (Mode::Lite | Mode::Comp, Input::Cso | Input::Pbp) => content(true),
        (Mode::Extract, Input::N3ds) => named(crate::classify::extension(&spec.input).strip_prefix('z')),
        (Mode::Extract, Input::Image { .. } | Input::Cartridge(_)) => Planned::Nothing,
        (Mode::Lite | Mode::Comp, Input::N3ds | Input::Cartridge(_)) => named(None),
        (Mode::Lite | Mode::Comp, Input::Image { target, .. }) => named(Some(target.extension())),
    }
}

fn output_dir(spec: &JobSpec) -> Result<PathBuf, String> {
    let dir = planned_dir(spec);
    std::fs::create_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    Ok(dir)
}

fn stem(path: &Path) -> String {
    // Lossy rather than a fixed fallback: a name that is not UTF-8 keeps every readable part.
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "output".to_string())
}

/// The stem a job's outputs are named after: the input's, unless the batch gave it another.
fn out_stem(spec: &JobSpec) -> String {
    spec.output_stem.clone().unwrap_or_else(|| stem(&spec.input))
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

// ---------------------------------------------------------------------------
// Create
// ---------------------------------------------------------------------------

fn chd_codec(name: &str) -> Option<ChdCodecs> {
    Some(match name.trim().to_ascii_lowercase().as_str() {
        "zstd" => ChdCodecs::Zstd,
        "zlib" => ChdCodecs::Zlib,
        "lzma" => ChdCodecs::Lzma,
        "flac" => ChdCodecs::Flac,
        "huff" | "huffman" => ChdCodecs::Huff,
        "cdzs" => ChdCodecs::Cdzs,
        "cdzl" => ChdCodecs::Cdzl,
        "cdlz" => ChdCodecs::Cdlz,
        "cdfl" => ChdCodecs::Cdfl,
        _ => return None,
    })
}

fn is_cd_codec(codec: &ChdCodecs) -> bool {
    matches!(codec, ChdCodecs::Cdzs | ChdCodecs::Cdzl | ChdCodecs::Cdlz | ChdCodecs::Cdfl)
}

/// A user's codec list for one media kind: unknown names are an error, as is a CD codec on a
/// DVD (or the reverse), which chdman would otherwise reject with a far less direct message.
pub fn parse_codecs(names: &[String], is_cd: bool) -> Result<Vec<ChdCodecs>, String> {
    let mut codecs = Vec::new();
    for name in names.iter().filter(|n| !n.trim().is_empty()) {
        let codec = chd_codec(name).ok_or_else(|| format!("unknown CHD codec '{name}'"))?;
        if is_cd_codec(&codec) != is_cd {
            return Err(format!("codec '{name}' is not valid for a {} CHD", if is_cd { "CD" } else { "DVD" }));
        }
        if !codecs.contains(&codec) {
            codecs.push(codec);
        }
    }
    if codecs.len() > crate::settings::MAX_CODECS {
        return Err(format!("at most {} codecs, got {}", crate::settings::MAX_CODECS, codecs.len()));
    }
    Ok(codecs)
}

/// The codecs and hunk size a create job passes to chdman.
pub fn chd_args(mode: Mode, settings: &Settings, target: Target, platform: Platform) -> Result<ChdArgs, String> {
    let is_cd = target.is_cd();
    let mut args = ChdArgs { is_cd, ..Default::default() };
    if mode == Mode::Comp {
        match settings.comp.mode {
            CompMode::Chdman => args.preset = ChdPresets::ChdmanDefault,
            CompMode::Best => {
                args.preset = ChdPresets::Custom;
                args.codecs = Some(if is_cd {
                    vec![ChdCodecs::Cdlz, ChdCodecs::Cdzs, ChdCodecs::Cdzl, ChdCodecs::Cdfl]
                } else {
                    vec![ChdCodecs::Lzma, ChdCodecs::Zstd, ChdCodecs::Zlib, ChdCodecs::Huff]
                });
            }
            CompMode::Custom => {
                args.preset = ChdPresets::Custom;
                let (names, hunk) = if is_cd { (&settings.comp.cd_codecs, settings.comp.cd_hunk_size) } else { (&settings.comp.dvd_codecs, settings.comp.dvd_hunk_size) };
                let codecs = parse_codecs(names, is_cd)?;
                args.codecs = (!codecs.is_empty()).then_some(codecs);
                if hunk > 0 {
                    if is_cd && hunk % 2448 != 0 {
                        return Err(format!("CD hunk size must be a multiple of 2448, got {hunk}"));
                    }
                    if !is_cd && hunk % 2048 != 0 {
                        return Err(format!("DVD hunk size must be a multiple of 2048, got {hunk}"));
                    }
                    args.hunk_size = Some(hunk as usize);
                }
            }
        }
    } else {
        args.preset = match settings.lite.preset {
            LitePreset::Pc => ChdPresets::AppRecommendPc,
            LitePreset::Mobile => ChdPresets::AppRecommendAndroid,
        };
    }
    args.apply_preset(platform);
    Ok(args)
}

/// The platform only steers the fast presets' hunk size and, on mobile, PS2's codec, so a
/// miss costs a slightly less tuned file and nothing else.
fn guess_platform(source: &Path, target: Target) -> Platform {
    if target == Target::GdChd {
        return Platform::Dreamcast;
    }
    hinted_metadata_cycle(source, None, HeuristicFlags::ALL).platform
}

fn rvz_compression(name: &str) -> Result<DiscCompression, String> {
    Ok(match name.trim().to_ascii_lowercase().as_str() {
        "zstd" => DiscCompression::Zstd,
        "lzma" => DiscCompression::Lzma,
        "lzma2" => DiscCompression::Lzma2,
        "bzip2" | "bz2" => DiscCompression::Bzip2,
        "none" => DiscCompression::None,
        other => return Err(format!("unknown RVZ codec '{other}'")),
    })
}

/// Clamps a level into what the codec takes (zstd 1-22, the others 1-9, none 0).
pub fn rvz_level(compression: DiscCompression, level: i32) -> i32 {
    match compression {
        DiscCompression::None | DiscCompression::Purge => 0,
        DiscCompression::Zstd => level.clamp(1, 22),
        _ => level.clamp(1, 9),
    }
}

fn create(spec: &JobSpec, target: Target, source: &Path, needs_cue: bool, on_event: &mut dyn FnMut(Event) -> bool) -> Result<String, String> {
    let dir = output_dir(spec)?;
    let output = dir.join(format!("{}.{}", out_stem(spec), target.extension()));
    let Some(claim) = claim_output(&output, spec.settings.output.force)? else {
        return Ok(format!("skipped, {} already exists", output.display()));
    };
    let input_bytes = InputSet::resolve(source).map(|set| set.total_bytes).unwrap_or(0);

    // A lone raw track: a CD CHD is made from a sheet, written for as long as the conversion
    // runs. In the output folder when one is set (the track's own may be read-only), naming the
    // track by a path relative to the sheet: chdman joins the sheet's folder and the name, so
    // an absolute one would not do. Beside the track when there is no such path (another
    // drive) or no output folder.
    let mut temp_cue: Option<PathBuf> = None;
    let source = if needs_cue {
        let mut track = TrackFile::at(source).map_err(|err| format!("{}: {err}", source.display()))?;
        let in_output = spec.settings.output_dir().is_some().then(|| relative_path(&dir, source)).flatten();
        // Hidden, so a folder dropped again after a killed run does not take it for a game.
        let cue = match in_output {
            Some(relative) => {
                track.name = relative.to_string_lossy().into_owned();
                dir.join(format!(".{}.chdlite.cue", out_stem(spec)))
            }
            None => source.with_file_name(format!(".{}.chdlite.cue", source.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())),
        };
        let rebuilt = rebuild_cue(&stem(source), &[track], &Expected::default(), &CueSheetOptions::default())?;
        // Told to the supervisor first, so a killed worker's sheet is removed too.
        on_event(Event::Scratch(cue.clone()));
        std::fs::write(&cue, &rebuilt.bytes).map_err(|err| format!("{}: {err}", cue.display()))?;
        temp_cue = Some(cue.clone());
        cue
    } else {
        source.to_path_buf()
    };

    on_event(Event::Message(format!("{} -> {}", target.label(), output.display())));
    on_event(Event::Output(output.clone()));

    let result = create_args(spec, target, &source).and_then(|args| {
        if let Some(codecs) = &args.archive.chd_args.codecs {
            if args.archive.do_chd {
                on_event(Event::Message(format!("codecs {}", codecs.iter().map(|c| format!("{c:?}").to_lowercase()).collect::<Vec<_>>().join("+"))));
            }
        }
        run_single(&source, args, on_event)
    });
    if let Some(cue) = temp_cue {
        let _ = std::fs::remove_file(cue);
    }
    let outcome = result?;
    stage_ok(&outcome.stages.archive, "create")?;
    claim.finish();

    let output_bytes = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
    let ratio = if input_bytes > 0 { format!(" ({:.1}%)", output_bytes as f64 * 100.0 / input_bytes as f64) } else { String::new() };
    Ok(format!("{}: {} -> {}{ratio}", target.label(), human(input_bytes), human(output_bytes)))
}

/// `target` as a path relative to the folder `base`, or `None` when there is none (another
/// drive) or either cannot be resolved.
fn relative_path(base: &Path, target: &Path) -> Option<PathBuf> {
    let (base, target) = (base.canonicalize().ok()?, target.canonicalize().ok()?);
    let (base, target): (Vec<_>, Vec<_>) = (base.components().collect(), target.components().collect());
    let common = base.iter().zip(&target).take_while(|(a, b)| a == b).count();
    if common == 0 {
        return None;
    }
    let mut relative: PathBuf = base[common..].iter().map(|_| "..").collect();
    relative.extend(&target[common..]);
    Some(relative)
}

/// What one create job hands `Single`: archive only, the platform guessed here taken as given
/// (it steers the fast presets, and picks RVZ for a GameCube/Wii image), the output named
/// after the job's own input.
fn create_args(spec: &JobSpec, target: Target, source: &Path) -> Result<SingleArgs, String> {
    let mut archive = ArchiveArgs::default();
    let platform = guess_platform(source, target);
    let trusted = if target == Target::Rvz {
        archive.do_rvz = true;
        archive.rvz_args = rvz_args(spec)?;
        // Dolphin reads the image itself; the platform only has to say "a Nintendo disc".
        if is_rvz_platform(platform) { platform } else { Platform::GameCube }
    } else {
        archive.do_chd = true;
        let mut chd = chd_args(spec.mode, &spec.settings, target, platform)?;
        // Settled here, with the media known: `Single` must pass these on untouched.
        chd.preset = ChdPresets::Custom;
        if !chd.is_cd {
            chd.dvd_codecs = chd.codecs.clone();
            chd.dvd_hunk_size = chd.hunk_size;
        }
        chd.num_processors = spec.threads.map(|n| n as usize);
        archive.chd_args = chd;
        // A sheet or an ISO makes a CHD whatever the platform; a guess outside the CHD
        // platforms would only refuse it.
        if is_chd_platform(platform) { platform } else { Platform::Unknown }
    };
    Ok(SingleArgs {
        stages: StageSwitches { archive: true, ..StageSwitches::NONE },
        archive,
        output_dir: Some(output_dir(spec)?),
        detect: Detect::Trust(trusted),
        max_workers: spec.threads.map(|n| n as usize),
        decode_threads: decode_threads(spec),
        archive_stem: Some(out_stem(spec)),
        ..SingleArgs::default()
    }
    .with_sheet_style(cue_style(&spec.settings.extract.cue_style)?))
}

// ---------------------------------------------------------------------------
// Wii U: .wud / .wux + key -> .wua
// ---------------------------------------------------------------------------

/// A Cemu-style `keys.txt` for disc keys: beside the image, else beside chdlite.
fn keys_txt(image: &Path) -> Option<PathBuf> {
    let beside = image.parent().map(|dir| dir.join("keys.txt"));
    let exe = crate::settings::exe_dir().map(|dir| dir.join("keys.txt"));
    [beside, exe].into_iter().flatten().find(|path| path.is_file())
}

/// The disc's size as a `.wud`: a `.wux` says it in its header.
fn wud_size(image: &Path) -> Option<u64> {
    use std::io::Read;
    let mut head = [0u8; 0x18];
    std::fs::File::open(image).ok()?.read_exact(&mut head).ok()?;
    if &head[0..4] == b"WUX0" {
        return Some(u64::from_le_bytes(head[0x10..0x18].try_into().ok()?));
    }
    std::fs::metadata(image).ok().map(|m| m.len())
}

/// The same conversion Cemu's "Convert to compressed Wii U archive" does, through Cemu's own
/// code. The key is the `<image>.key` (or `game.key`) beside the image, or a line of a
/// `keys.txt`. Lite and Comp write the same file: the archive's zstd level is fixed.
fn create_wua(spec: &JobSpec, image: &Path, on_event: &mut dyn FnMut(Event) -> bool) -> Result<String, String> {
    let dir = output_dir(spec)?;
    let output = dir.join(format!("{}.wua", out_stem(spec)));
    let Some(claim) = claim_output(&output, spec.settings.output.force)? else {
        return Ok(format!("skipped, {} already exists", output.display()));
    };
    let key = cemu_wrapper::key_file_for(image).and_then(|path| cemu_wrapper::read_key_file(&path));
    let keys = keys_txt(image);
    if key.is_none() && keys.is_none() {
        return Err(format!("no key for {}: put its .key file beside it (or a keys.txt beside it or chdlite)", image.display()));
    }
    let source = cemu_wrapper::Source { path: image.to_path_buf(), key };
    let (title_id, version) = cemu_wrapper::title_info(&source, keys.as_deref()).map_err(|err| err.to_string())?;
    on_event(Event::Message(format!("WUA {title_id:016x} v{version} -> {}", output.display())));
    on_event(Event::Output(output.clone()));

    let wud_bytes = wud_size(image);
    // chdlite checks no DAT, so the record says where it came from and nothing more.
    let record = cemu_wrapper::SourceRecord { name: image.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), size: wud_bytes, ..Default::default() };
    let record = serde_json::to_value(record).map_err(|err| err.to_string())?;
    let mut last = -1.0f32;
    cemu_wrapper::create(&[source], keys.as_deref(), Some(&record), &output, |done, total| {
        let percent = if total > 0 { done as f32 * 100.0 / total as f32 } else { 0.0 };
        if percent - last < 0.2 {
            return true;
        }
        last = percent;
        on_event(Event::Progress(percent))
    })
    .map_err(|err| err.to_string())?;
    claim.finish();

    let input_bytes = wud_bytes.unwrap_or(0);
    let output_bytes = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
    let ratio = if input_bytes > 0 { format!(" ({:.1}%)", output_bytes as f64 * 100.0 / input_bytes as f64) } else { String::new() };
    Ok(format!("WUA: {} -> {}{ratio}", human(input_bytes), human(output_bytes)))
}

// ---------------------------------------------------------------------------
// Xbox: Xbox 360 -> .zar, Xbox -> XISO
// ---------------------------------------------------------------------------

/// Where a create job writes, claimed ([`claim_output`]), or `Ok(None)` when it is already
/// there and not to be replaced.
fn fresh_output(spec: &JobSpec, extension: &str) -> Result<Option<(PathBuf, Claim)>, String> {
    let output = output_dir(spec)?.join(format!("{}.{extension}", out_stem(spec)));
    Ok(claim_output(&output, spec.settings.output.force)?.map(|claim| (output, claim)))
}

fn ratio(input_bytes: u64, output: &Path) -> String {
    let output_bytes = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
    let ratio = if input_bytes > 0 { format!(" ({:.1}%)", output_bytes as f64 * 100.0 / input_bytes as f64) } else { String::new() };
    format!("{} -> {}{ratio}", human(input_bytes), human(output_bytes))
}

/// Xenia's archive of the game partition, the way xenia-canary's own "Convert to ZAR" does
/// it. The image is hashed in the same pass, for the manifest a later Hash checks against.
fn create_zar(spec: &JobSpec, image: &Path, on_event: &mut dyn FnMut(Event) -> bool) -> Result<String, String> {
    let (output, claim) = match fresh_output(spec, Target::Zar.extension())? {
        Some(claimed) => claimed,
        None => return Ok(format!("skipped, {}.zar already exists", out_stem(spec))),
    };
    on_event(Event::Message(format!("ZAR -> {}", output.display())));
    on_event(Event::Output(output.clone()));
    let name = image.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // chdlite checks no DAT, so the record says where it came from and nothing more.
    let record = |hashes: &romexplorer::rom::hash::HashReport, size: u64| {
        let record = cemu_wrapper::SourceRecord { name, size: Some(size), crc32: hashes.crc32.clone(), md5: hashes.md5.clone(), sha1: hashes.sha1.clone(), verified: false };
        serde_json::to_value(record).unwrap_or_default()
    };
    let mut last = -1.0f32;
    let written = romexplorer::rom::xbox::create_zar(image, &output, record, |done, total| {
        let percent = if total > 0 { done as f32 * 100.0 / total as f32 } else { 0.0 };
        if percent - last < 0.2 {
            return true;
        }
        last = percent;
        on_event(Event::Progress(percent))
    })?;
    claim.finish();
    let files: usize = written.manifest.titles.iter().map(|t| t.files.len()).sum();
    Ok(format!("ZAR: {}, {files} files", ratio(written.image_size, &output)))
}

/// The game partition alone, through extract-xiso's own rewrite.
fn create_xiso(spec: &JobSpec, image: &Path, on_event: &mut dyn FnMut(Event) -> bool) -> Result<String, String> {
    let (output, claim) = match fresh_output(spec, Target::Xiso.extension())? {
        Some(claimed) => claimed,
        None => return Ok(format!("skipped, {}.{} already exists", out_stem(spec), Target::Xiso.extension())),
    };
    on_event(Event::Message(format!("XISO -> {}", output.display())));
    on_event(Event::Output(output.clone()));
    let mut last = -1.0f32;
    romexplorer::rom::xbox::rewrite_xiso(image, &output, |done, total| {
        let percent = if total > 0 { done as f32 * 100.0 / total as f32 } else { 0.0 };
        if percent - last < 0.2 {
            return true;
        }
        last = percent;
        on_event(Event::Progress(percent))
    })?;
    claim.finish();
    let input_bytes = std::fs::metadata(image).map(|m| m.len()).unwrap_or(0);
    Ok(format!("XISO: {}", ratio(input_bytes, &output)))
}

// ---------------------------------------------------------------------------
// CSO and PBP -> CHD
// ---------------------------------------------------------------------------

/// A CSO made a DVD CHD, a PBP a CD CHD per disc. The ISO (or each disc's cue and tracks) is
/// extracted into a scratch folder beside the output first -- chdman reads files, not a
/// stream -- made a CHD as if it had been dropped, and removed.
fn create_from_container(spec: &JobSpec, on_event: &mut dyn FnMut(Event) -> bool) -> Result<String, String> {
    let dir = output_dir(spec)?;
    // Every CHD this would make already there: skipped before anything is extracted.
    if !spec.settings.output.force {
        let base = SingleArgs::default().with_sheet_style(cue_style(&spec.settings.extract.cue_style)?);
        let input = InputSet::resolve(&spec.input)?;
        let plan = ContentPlan::build(&input, &base.cue_sheet, base.cd_sheet).map_err(|err| err.to_string())?;
        let chds: Vec<PathBuf> = plan
            .files
            .iter()
            .map(|file| Path::new(&file.name))
            .filter(|name| matches!(crate::classify::extension(name).as_str(), "cue" | "iso"))
            .map(|name| dir.join(format!("{}.chd", stem(name))))
            .collect();
        if !chds.is_empty() && chds.iter().all(|chd| chd.is_file() && !marker_of(chd).exists()) {
            let shown = chds.iter().map(|chd| chd.display().to_string()).collect::<Vec<_>>().join(", ");
            return Ok(format!("skipped, {shown} already exist{}", if chds.len() == 1 { "s" } else { "" }));
        }
    }
    remove_stale_scratch(&dir);
    let scratch = dir.join(format!("{SCRATCH_PREFIX}{}-{}", std::process::id(), out_stem(spec)));
    let _ = std::fs::remove_dir_all(&scratch);
    // Told to the supervisor first, so a killed worker's scratch folder is removed too.
    on_event(Event::Scratch(scratch.clone()));
    std::fs::create_dir_all(&scratch).map_err(|err| format!("{}: {err}", scratch.display()))?;
    // Half the bar each: the extract, then the CHDs. Scratch files go with the scratch
    // folder; only the CHDs are this job's outputs.
    fn relay(event: Event, offset: f32, scale: f32, scratch: &Path, on_event: &mut dyn FnMut(Event) -> bool) -> bool {
        match event {
            Event::Progress(p) => on_event(Event::Progress(offset + p * scale)),
            Event::Output(path) if path.starts_with(scratch) => true,
            other => on_event(other),
        }
    }
    let result = (|| {
        content_pass(spec, Some(scratch.clone()), &mut |event| relay(event, 0.0, 0.5, &scratch, on_event))?;
        let mut sources: Vec<PathBuf> = std::fs::read_dir(&scratch)
            .map_err(|err| format!("{}: {err}", scratch.display()))?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| matches!(crate::classify::extension(path).as_str(), "cue" | "iso"))
            .collect();
        sources.sort();
        if sources.is_empty() {
            return Err("nothing came out to convert".to_string());
        }
        let settings = Settings { output: crate::settings::Output { dir: dir.to_string_lossy().into_owned(), ..spec.settings.output.clone() }, ..spec.settings.clone() };
        let mut done = Vec::new();
        let count = sources.len() as f32;
        for (i, source) in sources.iter().enumerate() {
            let target = if crate::classify::extension(source) == "cue" { Target::CdChd } else { Target::DvdChd };
            let disc = JobSpec { mode: spec.mode, input: source.clone(), settings: settings.clone(), threads: spec.threads, output_stem: None };
            let offset = 50.0 + 50.0 * i as f32 / count;
            done.push(create(&disc, target, source, false, &mut |event| relay(event, offset, 0.5 / count, &scratch, on_event))?);
        }
        Ok(done.join("\n"))
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

/// A scratch folder is `.chdlite-<pid>-<stem>`, `<pid>` the process that made it.
const SCRATCH_PREFIX: &str = ".chdlite-";

/// Removes the scratch folders in `dir` that a process no longer running left behind (one
/// killed with nobody left to clean up after it: a power cut, or the GUI killed with it).
fn remove_stale_scratch(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(pid) = name.strip_prefix(SCRATCH_PREFIX).and_then(|rest| rest.split_once('-')).and_then(|(pid, _)| pid.parse::<u32>().ok()) else { continue };
        if pid != std::process::id() && !process_alive(pid) && entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else { return false };
    // Signal 0 only checks: EPERM is a process there that belongs to someone else.
    let sent = unsafe { libc::kill(pid, 0) } == 0;
    sent || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use std::ffi::c_void;
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn GetExitCodeProcess(process: *mut c_void, code: *mut u32) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn GetLastError() -> u32;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    const ERROR_ACCESS_DENIED: u32 = 5;
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return GetLastError() == ERROR_ACCESS_DENIED;
        }
        let mut code = 0u32;
        let alive = GetExitCodeProcess(process, &mut code) != 0 && code == STILL_ACTIVE;
        CloseHandle(process);
        alive
    }
}

#[cfg(not(any(unix, windows)))]
fn process_alive(_pid: u32) -> bool {
    true
}

// ---------------------------------------------------------------------------
// Cartridge dumps: NES / N64 / SNES into the settings' layout
// ---------------------------------------------------------------------------

/// Whether two paths name one file: same folder, and the same name (ignoring case where the
/// file system does).
fn same_file_path(a: &Path, b: &Path) -> bool {
    let folder = |path: &Path| path.parent().map(|dir| if dir.as_os_str().is_empty() { Path::new(".") } else { dir }).and_then(|dir| dir.canonicalize().ok());
    let (Some(da), Some(db)) = (folder(a), folder(b)) else { return false };
    let (Some(na), Some(nb)) = (a.file_name(), b.file_name()) else { return false };
    let (na, nb) = (na.to_string_lossy(), nb.to_string_lossy());
    da == db && if cfg!(any(windows, target_os = "macos")) { na.eq_ignore_ascii_case(&nb) } else { na == nb }
}

/// Lite and Comp alike: a cartridge dump is not compressed, only rewritten into the layout the
/// settings pick for its platform. One already in it -- content and extension -- is skipped,
/// even with an output folder set; one whose content is already right but whose extension is
/// not gets a copy under the right name.
fn convert_cartridge(spec: &JobSpec, cart: Cart, on_event: &mut dyn FnMut(Event) -> bool) -> Result<String, String> {
    let converted = crate::cart::convert(cart, &spec.input, &spec.settings.cartridge)?;
    let dir = output_dir(spec)?;
    let output = dir.join(format!("{}.{}", out_stem(spec), converted.extension));
    // Already there: nothing is written, wherever the output would go.
    if !converted.changed && crate::classify::extension(&spec.input) == converted.extension {
        return Ok(format!("skipped, already {}", converted.to));
    }
    let in_place = same_file_path(&output, &spec.input);
    if in_place && !spec.settings.output.force {
        return Ok(format!(
            "skipped: {} would be written over the input itself -- choose an output folder, or turn on replacing outputs",
            output.display()
        ));
    }
    let Some(claim) = claim_output(&output, spec.settings.output.force)? else {
        return Ok(format!("skipped, {} already exists", output.display()));
    };
    on_event(Event::Message(format!("{} -> {}", cart.label(), output.display())));
    // The input itself too: rolling a claim back puts back what it was replacing.
    on_event(Event::Output(output.clone()));
    std::fs::write(&output, &converted.data).map_err(|err| format!("{}: {err}", output.display()))?;
    claim.finish();
    let what = if converted.changed { format!("{} -> {}", converted.from, converted.to) } else { format!("already {}, copied as .{}", converted.to, converted.extension) };
    Ok(format!("{}: {what} ({})", cart.label(), human(converted.data.len() as u64)))
}

// ---------------------------------------------------------------------------
// 3DS: decrypted into the settings' format
// ---------------------------------------------------------------------------

/// Lite and Comp alike (the compressed formats have one zstd level, Azahar's). A file already
/// in the wanted form -- decrypted, same extension -- is skipped.
fn convert_n3ds(spec: &JobSpec, on_event: &mut dyn FnMut(Event) -> bool) -> Result<String, String> {
    use romexplorer::rom::n3ds;
    let target = crate::n3ds::target(&spec.settings.n3ds.format)?;
    let info = n3ds::inspect(&spec.input)?;
    let extension = n3ds::output_extension(&info, target);
    if !info.encrypted && crate::classify::extension(&spec.input) == extension {
        return Ok(format!("skipped, already a decrypted .{extension}"));
    }
    let keys = crate::n3ds::keys_for(&spec.input)?;
    if let Some(missing) = n3ds::check(&spec.input, &keys)? {
        return Err(format!("{missing} -- put it beside the file, beside chdlite, or in chdlite's assets folder"));
    }
    let output = output_dir(spec)?.join(format!("{}.{extension}", out_stem(spec)));
    if same_file_path(&output, &spec.input) {
        return Ok(format!("skipped: {} would be written over the input itself -- choose an output folder", output.display()));
    }
    let Some(claim) = claim_output(&output, spec.settings.output.force)? else {
        return Ok(format!("skipped, {} already exists", output.display()));
    };
    on_event(Event::Message(format!("3DS {} -> {}", info.title_kind, output.display())));
    on_event(Event::Output(output.clone()));
    let mut last = -1.0f32;
    let outcome = n3ds::convert(&spec.input, &output, target, &keys, &mut |done, total| {
        let percent = if total > 0 { done as f32 * 100.0 / total as f32 } else { 0.0 };
        if percent - last < 0.2 {
            return true;
        }
        last = percent;
        on_event(Event::Progress(percent))
    })?;
    claim.finish();
    let input_bytes = std::fs::metadata(&spec.input).map(|m| m.len()).unwrap_or(0);
    let what = match outcome {
        n3ds::Outcome::Converted => "decrypted",
        n3ds::Outcome::AlreadyDecrypted => "already decrypted",
    };
    Ok(format!("3DS {}, {what}: .{} -> .{extension}{}", info.title_kind, crate::classify::extension(&spec.input), ratio(input_bytes, &output)))
}

/// A compressed 3DS file back to the plain one: `.zcci` -> `.cci`, `.zcia` -> `.cia`,
/// `.zcxi` -> `.cxi`, `.z3dsx` -> `.3dsx`.
fn extract_n3ds(spec: &JobSpec, on_event: &mut dyn FnMut(Event) -> bool) -> Result<String, String> {
    let ext = crate::classify::extension(&spec.input);
    let Some(plain) = ext.strip_prefix('z').filter(|_| matches!(ext.as_str(), "zcci" | "zcia" | "zcxi" | "z3dsx")) else {
        return Err("nothing to extract: not a compressed 3DS file (.zcci, .zcia, .zcxi, .z3dsx)".to_string());
    };
    let output = output_dir(spec)?.join(format!("{}.{plain}", out_stem(spec)));
    let Some(claim) = claim_output(&output, spec.settings.output.force)? else {
        return Ok(format!("skipped, {} already exists", output.display()));
    };
    on_event(Event::Output(output.clone()));
    let mut last = -1.0f32;
    romexplorer::rom::n3ds::z3ds_decompress(&spec.input, &output, &mut |done, total| {
        let percent = if total > 0 { done as f32 * 100.0 / total as f32 } else { 0.0 };
        if percent - last < 0.2 {
            return true;
        }
        last = percent;
        on_event(Event::Progress(percent))
    })?;
    claim.finish();
    Ok(format!(".{ext} -> .{plain} ({})", human(std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0))))
}

// ---------------------------------------------------------------------------
// Outputs: never a half-written file taken for a finished one
// ---------------------------------------------------------------------------

/// The hidden file beside `output` that says it is being written. A run that is killed (the
/// GUI's cancel, Ctrl-C, a crash, a power cut) leaves it behind, so the next run knows the
/// output there is not finished.
fn marker_of(output: &Path) -> PathBuf {
    let name = output.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    output.with_file_name(format!(".{name}.chdlite-partial"))
}

/// Where a replaced output waits until its replacement is finished.
fn previous_of(output: &Path) -> PathBuf {
    let name = output.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    output.with_file_name(format!(".{name}.chdlite-old"))
}

/// One output being written. [`Claim::finish`] once it is complete; dropped unfinished, the
/// partial file goes and a file it was replacing comes back.
pub(crate) struct Claim {
    output: PathBuf,
    finished: bool,
}

impl Claim {
    pub(crate) fn finish(mut self) {
        // The marker first: once it is gone the output counts as finished, and a run killed
        // before the old file is removed leaves only a stale `.chdlite-old`, cleared next time.
        let _ = std::fs::remove_file(marker_of(&self.output));
        let _ = std::fs::remove_file(previous_of(&self.output));
        self.finished = true;
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        if !self.finished {
            roll_back(&self.output);
        }
    }
}

/// Undoes an unfinished claim on `output`: the partial file goes and the file it was replacing
/// comes back. Nothing happens when its marker is gone -- the claim finished, or was already
/// rolled back -- so this is safe to call again, by whoever supervises a job.
fn roll_back(output: &Path) {
    let marker = marker_of(output);
    if !marker.exists() {
        return;
    }
    let _ = std::fs::remove_file(output);
    let previous = previous_of(output);
    if previous.exists() {
        let _ = std::fs::rename(&previous, output);
    }
    let _ = std::fs::remove_file(marker);
}

/// Claims `output` for writing: `Ok(None)` when a finished one is there and not to be
/// replaced. One a killed run left half-written is cleared first (and what it was replacing
/// put back); one to be replaced is kept aside until the new one is finished.
pub(crate) fn claim_output(output: &Path, force: bool) -> Result<Option<Claim>, String> {
    let marker = marker_of(output);
    if marker.exists() {
        let _ = std::fs::remove_file(output);
        let previous = previous_of(output);
        if previous.exists() {
            std::fs::rename(&previous, output).map_err(|err| format!("{}: {err}", previous.display()))?;
        }
        let _ = std::fs::remove_file(&marker);
    } else if output.exists() {
        // Left by a run killed while finishing: the output is the finished one.
        let _ = std::fs::remove_file(previous_of(output));
    }
    if output.exists() {
        if !force {
            return Ok(None);
        }
        std::fs::rename(output, previous_of(output)).map_err(|err| format!("{}: {err}", output.display()))?;
    }
    std::fs::write(&marker, b"").map_err(|err| format!("{}: {err}", marker.display()))?;
    Ok(Some(Claim { output: output.to_path_buf(), finished: false }))
}

fn rvz_args(spec: &JobSpec) -> Result<RvzArgs, String> {
    let rvz = &spec.settings.rvz;
    let compression = rvz_compression(&rvz.codec)?;
    let algo = match compression {
        DiscCompression::Zstd => RvzCodecs::Zstd,
        DiscCompression::Lzma => RvzCodecs::Lzma,
        DiscCompression::Lzma2 => RvzCodecs::Lzma2,
        DiscCompression::Bzip2 => RvzCodecs::Bzip2,
        DiscCompression::None | DiscCompression::Purge => RvzCodecs::Uncompressed,
    };
    let bytes = rvz.block_bytes();
    let block_size = [RvzBlockSize::Block_32k, RvzBlockSize::Block_64k, RvzBlockSize::Block_128k, RvzBlockSize::Block_256k, RvzBlockSize::Block_512k, RvzBlockSize::Block_1m, RvzBlockSize::Block_2m]
        .into_iter()
        .find(|b| b.bytes() == bytes)
        .ok_or_else(|| format!("unsupported RVZ block size {bytes}"))?;
    Ok(RvzArgs { algo, block_size, compression_level: rvz_level(compression, rvz.level) as i8, remove_junk_data: rvz.scrub })
}

fn decode_threads(spec: &JobSpec) -> DecodeThreads {
    match spec.threads {
        Some(n) => {
            let n = n.max(1) as usize;
            DecodeThreads { chd_cd: Some(n), chd_dvd_raw: Some(n), rvz: Some(n.min(4)) }
        }
        None => DecodeThreads::recommended(),
    }
}

/// Runs one `Single`, its progress as this job's events; `on_event` returning `false`
/// cancels it.
fn run_single(input: &Path, args: SingleArgs, on_event: &mut dyn FnMut(Event) -> bool) -> Result<SingleOutcome, String> {
    let mut last = -1.0f32;
    Single::new(input, args)
        .run(&mut |event: &ProgressEvent| {
            let percent = event.total_fraction * 100.0;
            // A listener only needs to hear about visible movement.
            if percent - last < 0.2 {
                return true;
            }
            last = percent;
            on_event(Event::Progress(percent))
        })
        .map_err(|err| match err {
            SingleError::Cancelled => "cancelled".to_string(),
            other => other.to_string(),
        })
}

fn stage_ok(result: &StageResult, what: &str) -> Result<(), String> {
    match result {
        StageResult::Ok => Ok(()),
        StageResult::Err(message) => Err(message.clone()),
        StageResult::Skipped(reason) => Err(format!("{what} skipped: {reason}")),
        StageResult::Off => Err(format!("{what} did not run")),
    }
}

// ---------------------------------------------------------------------------
// Extract and hash: one decode pass over the content, into files and/or digests
// ---------------------------------------------------------------------------

pub fn hash_flags(names: &[String]) -> Result<HashFlags, String> {
    let mut flags = HashFlags::empty();
    for name in names {
        flags |= match name.trim().to_ascii_lowercase().as_str() {
            "sha1" => HashFlags::SHA1,
            "md5" => HashFlags::MD5,
            "crc32" | "crc" => HashFlags::CRC32,
            "sha256" => HashFlags::SHA256,
            "xxh3" | "xxh3_128" => HashFlags::XXH3_128,
            "rahash" | "ra" => HashFlags::RAHASH,
            "" => HashFlags::empty(),
            other => return Err(format!("unknown hash '{other}'")),
        };
    }
    Ok(if flags.is_empty() { HashFlags::SHA1 } else { flags })
}

fn cue_style(style: &str) -> Result<CueSheetStyle, String> {
    Ok(match style.trim().to_ascii_lowercase().as_str() {
        "redump" | "" => CueSheetStyle::Redump,
        "redump_catalog" | "catalog" => CueSheetStyle::RedumpCatalog,
        "chdman" => CueSheetStyle::Chdman,
        other => return Err(format!("unknown cue style '{other}'")),
    })
}

fn gdrom_sheet(sheet: &str) -> Result<SheetKind, String> {
    Ok(match sheet.trim().to_ascii_lowercase().as_str() {
        "gdi" | "" => SheetKind::Gdi,
        "cue" => SheetKind::Cue,
        other => return Err(format!("unknown GD-ROM sheet '{other}' (gdi or cue)")),
    })
}

/// Decodes the input's content once, writing it under `out_dir` (extract) or digesting it
/// (hash): one `Single`. For a CHD or RVZ that is the disc inside; for anything else, the
/// files themselves. An extracted GD-ROM gets the `gdrom_sheet` setting's sheet (a gdi by
/// default); a hashed one keeps the cue style's own choice (Redump: a cue), so the sheet's hash
/// is the one the Redump DAT lists.
fn content_pass(spec: &JobSpec, out_dir: Option<PathBuf>, on_event: &mut dyn FnMut(Event) -> bool) -> Result<String, String> {
    let extracting = out_dir.is_some();
    let flags = if extracting { HashFlags::empty() } else { hash_flags(&spec.settings.hash.algorithms)? };
    let style = cue_style(&spec.settings.extract.cue_style)?;
    let mut base = SingleArgs::default().with_sheet_style(style);
    if extracting {
        base.cd_sheet = gdrom_sheet(&spec.settings.extract.gdrom_sheet)?;
    }

    // What it will write, listed without decoding: to skip, or to clear the way for, files
    // already there.
    let input = InputSet::resolve(&spec.input)?;
    let plan = ContentPlan::build(&input, &base.cue_sheet, base.cd_sheet).map_err(|err| err.to_string())?;
    let mut claims = Vec::new();
    if let Some(dir) = &out_dir {
        for path in plan.files.iter().map(|file| dir.join(&file.name)) {
            match claim_output(&path, spec.settings.output.force)? {
                Some(claim) => claims.push(claim),
                None => return Ok(format!("skipped, {} already exists", path.display())),
            }
        }
        on_event(Event::Message(format!("extract -> {}", dir.join(&plan.files[0].name).display())));
        for file in &plan.files {
            on_event(Event::Output(dir.join(&file.name)));
        }
    }

    // The RA hash is worked out apart (`crate::ra`). For most cartridge systems it is the
    // file's MD5, so the pass works one out for it, shown only when asked for.
    let want_ra = flags.contains(HashFlags::RAHASH);
    let shown = flags.difference(HashFlags::RAHASH);
    let args = SingleArgs {
        stages: StageSwitches { extract: extracting, hash: !extracting, ..StageSwitches::NONE },
        hash_flags: if want_ra { shown | HashFlags::MD5 } else { shown },
        output_dir: out_dir,
        detect: Detect::Trust(Platform::Unknown),
        decode_threads: decode_threads(spec),
        ..base
    };
    let outcome = run_single(&spec.input, args, on_event)?;
    if extracting {
        if input.is_archive() {
            stage_ok(&outcome.stages.extract, "extract")?;
        }
        claims.into_iter().for_each(Claim::finish);
        let names: Vec<&str> = plan.files.iter().map(|file| file.name.as_str()).collect();
        let shown = if names.len() > 3 { format!("{} and {} more", names[0], names.len() - 1) } else { names.join(", ") };
        return Ok(format!("extracted {shown} ({})", human(plan.total_bytes())));
    }
    stage_ok(&outcome.stages.hash, "hash")?;
    let mut lines = Vec::new();
    let records = outcome.input_hashes.map(|r| r.content_hashes).unwrap_or_default();
    for record in &records {
        lines.push(format!("{}  ({} bytes)", record.path, record.size_bytes.unwrap_or(0)));
        for (label, value, flag) in [
            ("crc32", &record.crc32, HashFlags::CRC32),
            ("md5", &record.md5, HashFlags::MD5),
            ("sha1", &record.sha1, HashFlags::SHA1),
            ("sha256", &record.sha256, HashFlags::SHA256),
            ("xxh3", &record.xxh3_128, HashFlags::XXH3_128),
        ] {
            if let Some(value) = value.as_ref().filter(|_| shown.contains(flag)) {
                lines.push(format!("  {label:<7}{value}"));
            }
        }
    }
    if want_ra {
        // A cue and its tracks, or a CHD's several files, have no one MD5 to stand for them.
        let file_md5 = match records.as_slice() {
            [one] => one.md5.as_deref(),
            _ => None,
        };
        if records.len() > 1 {
            lines.push(spec.input.display().to_string());
        }
        lines.extend(crate::ra::lines(&spec.input, file_md5));
    }
    Ok(lines.join("\n"))
}

// ---------------------------------------------------------------------------
// Read
// ---------------------------------------------------------------------------

fn ascii_field(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    bytes[..end].iter().map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '?' }).collect::<String>().trim().to_string()
}

/// Platform, serial / product code and title, from romexplorer's own detection: the same
/// per-platform readers a scan uses, straight on the file (a CHD or RVZ by random access, a
/// cue or gdi through its tracks). Only what was found is listed; a disc no platform
/// recognises gets a single "unknown" line.
fn identity_lines(path: &Path, hint: Option<Platform>) -> Vec<String> {
    let found = hinted_metadata_cycle(path, hint, HeuristicFlags::ALL);
    let meta = found.metadata;
    let mut lines = Vec::new();
    if found.platform == Platform::Unknown {
        lines.push("  platform     unknown".to_string());
    } else {
        lines.push(format!("  platform     {}", found.platform.display_name()));
    }
    let clean = |value: &Option<String>| value.as_deref().map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
    if let Some(serial) = clean(&meta.serial_product_id) {
        lines.push(format!("  serial       {serial}"));
    }
    if let Some(title) = clean(&meta.title) {
        lines.push(format!("  title        {title}"));
    }
    lines
}

fn read(path: &Path, input: &Input, settings: &Settings) -> Result<String, String> {
    let file_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut lines = vec![format!("{}", path.display())];
    match input {
        Input::Chd => {
            let meta = read_chd_metadata(path).map_err(|err| err.to_string())?;
            let kind = if !meta.tracks.is_empty() {
                let gdrom = ChdReader::open(path, None, 0).and_then(|reader| reader.info()).map(|info| info.is_gdrom).unwrap_or(false);
                if gdrom { "GD-ROM" } else { "CD" }
            } else if meta.is_dvd {
                "DVD"
            } else if meta.hard_disk_geometry.is_some() {
                "hard disk"
            } else {
                "raw"
            };
            lines.push(format!("  type         CHD v{} ({kind})", meta.file_version));
            lines.extend(identity_lines(path, (kind == "GD-ROM").then_some(Platform::Dreamcast)));
            lines.push(format!("  logical size {} ({} bytes)", human(meta.logical_size as u64), meta.logical_size));
            lines.push(format!("  file size    {} ({:.1}%)", human(file_bytes), if meta.logical_size > 0 { file_bytes as f64 * 100.0 / meta.logical_size as f64 } else { 0.0 }));
            lines.push(format!("  hunk size    {} bytes, {} hunks", meta.hunk_size, meta.total_hunks));
            lines.push(format!("  unit size    {} bytes", meta.unit_size));
            let codecs = meta.compression.iter().map(|c| format!("{c:?}").to_lowercase()).collect::<Vec<_>>();
            lines.push(format!("  compression  {}", if codecs.is_empty() { "none".to_string() } else { codecs.join(", ") }));
            lines.push(format!("  sha1         {}", meta.sha1));
            if !meta.data_sha1.is_empty() {
                lines.push(format!("  data sha1    {}", meta.data_sha1));
            }
            if let Some(parent) = &meta.parent_sha1 {
                lines.push(format!("  parent sha1  {parent}"));
            }
            for track in &meta.tracks {
                lines.push(format!("  track {:02}     {:?}, {} frames", track.track_number, track.track_type, track.frames));
            }
        }
        Input::Rvz => {
            let mut reader = DiscReader::open(path, None).map_err(|err| err.to_string())?;
            let info = reader.info().map_err(|err| err.to_string())?;
            let mut header = [0u8; 0x80];
            reader.read(0, &mut header).map_err(|err| err.to_string())?;
            let system = if header[0x18..0x1C] == [0x5D, 0x1C, 0x9E, 0xA3] {
                "Wii"
            } else if header[0x1C..0x20] == [0xC2, 0x33, 0x9F, 0x3D] {
                "GameCube"
            } else {
                "unknown"
            };
            lines.push(format!("  type         RVZ ({system})"));
            lines.push(format!("  game id      {}", ascii_field(&header[..6])));
            lines.push(format!("  title        {}", ascii_field(&header[0x20..0x60])));
            lines.push(format!("  disc size    {} ({} bytes)", human(info.data_size), info.data_size));
            lines.push(format!("  file size    {} ({:.1}%)", human(file_bytes), if info.data_size > 0 { file_bytes as f64 * 100.0 / info.data_size as f64 } else { 0.0 }));
            lines.push(format!("  block size   {} bytes", info.block_size));
        }
        Input::Cso => {
            let reader = romexplorer::rom::cso::CsoReader::open(path).map_err(|err| format!("{}: {err}", path.display()))?;
            let iso = reader.size();
            lines.push(format!("  type         CSO v{}, {} blocks of {} bytes", reader.header.version, iso.div_ceil(reader.header.block_size as u64), reader.header.block_size));
            lines.extend(identity_lines(path, None));
            lines.push(format!("  iso size     {} ({iso} bytes)", human(iso)));
            lines.push(format!("  file size    {} ({:.1}%)", human(file_bytes), if iso > 0 { file_bytes as f64 * 100.0 / iso as f64 } else { 0.0 }));
            lines.push("  converts to  DVD CHD".to_string());
        }
        Input::Pbp => {
            let pbp = romexplorer::rom::pbp::Pbp::open(path)?;
            lines.push(format!("  type         PlayStation EBOOT.PBP, {} disc{}, {}", pbp.disc_count(), if pbp.disc_count() == 1 { "" } else { "s" }, human(file_bytes)));
            lines.extend(identity_lines(path, None));
            let stem = romexplorer::rom::pbp::output_stem(path);
            for disc in 0..pbp.disc_count() {
                let layout = romexplorer::rom::pbp::disc_layout(&pbp, disc, &stem)?;
                lines.push(format!("  disc {}       {}: {} track{}, {}", disc + 1, layout.cue_name, layout.tracks.len(), if layout.tracks.len() == 1 { "" } else { "s" }, human(layout.total_bytes())));
            }
            lines.push("  converts to  CD CHD".to_string());
        }
        Input::Cartridge(cart) => {
            lines.push(format!("  type         {} cartridge dump, {}", cart.label(), human(file_bytes)));
            match crate::cart::convert(*cart, path, &settings.cartridge) {
                Ok(converted) => {
                    lines.push(format!("  layout       {}", converted.from));
                    lines.push(if converted.changed { format!("  converts to  {}", converted.to) } else { format!("  converts to  nothing, already {}", converted.to) });
                }
                Err(err) => lines.push(format!("  layout       unknown: {err}")),
            }
        }
        Input::N3ds => {
            use romexplorer::rom::n3ds;
            match n3ds::inspect(path) {
                Ok(info) => {
                    let container = match info.container {
                        n3ds::Container::Cia => "CIA",
                        n3ds::Container::Cci => "cartridge image (NCSD)",
                        n3ds::Container::Ncch => "NCCH",
                        n3ds::Container::Homebrew => "homebrew (3DSX)",
                    };
                    lines.push(format!("  type         3DS {container}{}, {}", if info.compressed { ", compressed (Z3DS)" } else { "" }, human(file_bytes)));
                    lines.push(format!("  title        {}", info.title_kind));
                    lines.push(format!("  encrypted    {}", if info.encrypted { "yes" } else { "no" }));
                    match crate::n3ds::target(&settings.n3ds.format) {
                        Ok(target) => lines.push(format!("  converts to  .{}", n3ds::output_extension(&info, target))),
                        Err(err) => lines.push(format!("  converts to  unknown: {err}")),
                    }
                }
                Err(err) => lines.push(format!("  type         3DS file, unreadable: {err}")),
            }
        }
        Input::Image { target: Target::Wua, .. } => {
            lines.push(format!("  type         Wii U disc image, {}", human(file_bytes)));
            if let Some(size) = wud_size(path).filter(|size| *size != file_bytes) {
                lines.push(format!("  disc size    {} ({size} bytes)", human(size)));
            }
            let key = cemu_wrapper::key_file_for(path).and_then(|p| cemu_wrapper::read_key_file(&p));
            let source = cemu_wrapper::Source { path: path.to_path_buf(), key };
            match cemu_wrapper::title_info(&source, keys_txt(path).as_deref()) {
                Ok((id, version)) => lines.push(format!("  title        {id:016x} v{version}")),
                Err(err) => lines.push(format!("  title        unreadable: {err}")),
            }
            lines.push("  converts to  WUA".to_string());
        }
        Input::Image { target, source, needs_cue } => {
            lines.push(format!("  type         disc image, {}", human(file_bytes)));
            // A track read on its own stands for the disc its sheet describes.
            let hint = match target {
                Target::GdChd => Some(Platform::Dreamcast),
                Target::Zar => Some(Platform::Xbox360),
                Target::Xiso => Some(Platform::Xbox),
                _ => None,
            };
            lines.extend(identity_lines(source, hint));
            lines.push(format!("  converts to  {}", target.label()));
            if source != path {
                lines.push(format!("  via          {}", source.display()));
            } else if *needs_cue {
                lines.push("  via          a generated single-track cue".to_string());
            }
        }
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_half_written_output_is_not_taken_for_a_finished_one() {
        let dir = std::env::temp_dir().join(format!("chdlite_claim_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("Game.chd");

        // A run killed part way: its mark stays, so the next run does not skip the file.
        let claim = claim_output(&out, false).unwrap().unwrap();
        std::fs::write(&out, b"half").unwrap();
        std::mem::forget(claim);
        assert!(claim_output(&out, false).unwrap().is_some_and(|c| {
            assert!(!out.exists(), "the half-written file is cleared");
            std::fs::write(&out, b"whole").unwrap();
            c.finish();
            true
        }));
        assert!(claim_output(&out, false).unwrap().is_none(), "a finished one is skipped");

        // Replacing: the old file stays until the new one is finished, and comes back if it
        // never is.
        let claim = claim_output(&out, true).unwrap().unwrap();
        std::fs::write(&out, b"new, half").unwrap();
        drop(claim);
        assert_eq!(std::fs::read(&out).unwrap(), b"whole");
        let claim = claim_output(&out, true).unwrap().unwrap();
        std::fs::write(&out, b"new").unwrap();
        claim.finish();
        assert_eq!(std::fs::read(&out).unwrap(), b"new");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no marks or old copies left");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn fresh_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chdlite_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A replace that fails puts the old file back, and the job's own clean-up afterwards must
    /// not remove it again.
    #[test]
    fn a_failed_replace_keeps_the_file_it_was_replacing() {
        let dir = fresh_dir("failed_replace");
        let input = dir.join("Game.cue");
        std::fs::write(&input, "not a sheet").unwrap();
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("Game.chd"), b"previous").unwrap();
        let mut settings = Settings::default();
        settings.output.dir = out.to_string_lossy().into_owned();
        settings.output.force = true;
        let spec = JobSpec { mode: Mode::Lite, input, settings, threads: Some(1), output_stem: None };
        assert!(run_one(&spec, &mut |_| true).is_err());
        assert_eq!(std::fs::read(out.join("Game.chd")).unwrap(), b"previous");
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1, "no marks or old copies left");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A killed worker's claims are rolled back by its supervisor from what it announced; one
    /// that finished is left alone.
    #[test]
    fn undoing_a_killed_job_puts_back_what_it_was_replacing() {
        let dir = fresh_dir("undo");
        let (replaced, finished, scratch) = (dir.join("A.chd"), dir.join("B.chd"), dir.join(".A.bin.chdlite.cue"));
        std::fs::write(&replaced, b"previous").unwrap();
        let mut writing = Writing::default();
        let claim = claim_output(&replaced, true).unwrap().unwrap();
        writing.note(&Event::Output(replaced.clone()));
        std::fs::write(&replaced, b"half").unwrap();
        std::mem::forget(claim);
        let claim = claim_output(&finished, false).unwrap().unwrap();
        writing.note(&Event::Output(finished.clone()));
        std::fs::write(&finished, b"whole").unwrap();
        claim.finish();
        writing.note(&Event::Scratch(scratch.clone()));
        std::fs::write(&scratch, b"sheet").unwrap();

        writing.undo();
        assert_eq!(std::fs::read(&replaced).unwrap(), b"previous");
        assert_eq!(std::fs::read(&finished).unwrap(), b"whole");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2, "no marks, old copies or scratch left");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scratch_of_a_process_no_longer_running_is_removed() {
        let dir = fresh_dir("stale");
        let mine = dir.join(format!("{SCRATCH_PREFIX}{}-Game", std::process::id()));
        let gone = dir.join(format!("{SCRATCH_PREFIX}4000000000-Game"));
        let other = dir.join(".chdlite-notes");
        for folder in [&mine, &gone, &other] {
            std::fs::create_dir_all(folder).unwrap();
        }
        remove_stale_scratch(&dir);
        assert!(mine.exists() && other.exists());
        assert!(!gone.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_track_is_named_relative_to_a_sheet_elsewhere() {
        let dir = fresh_dir("relative");
        let (source, out) = (dir.join("in").join("sub"), dir.join("out"));
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(source.join("Game.bin"), b"").unwrap();
        assert_eq!(relative_path(&out, &source.join("Game.bin")).unwrap(), Path::new("..").join("in").join("sub").join("Game.bin"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// chdlite knows the media before it builds the args, so its codecs and hunk size must
    /// come through `apply_preset` exactly as set -- every codec, for a CD and a DVD alike.
    #[test]
    fn chd_args_keep_every_codec_and_the_hunk_for_both_media() {
        let mut settings = Settings::default();
        settings.comp.mode = CompMode::Best;
        let dvd = chd_args(Mode::Comp, &settings, Target::DvdChd, Platform::PlayStation2).unwrap();
        assert_eq!(dvd.codecs, Some(vec![ChdCodecs::Lzma, ChdCodecs::Zstd, ChdCodecs::Zlib, ChdCodecs::Huff]));
        let cd = chd_args(Mode::Comp, &settings, Target::CdChd, Platform::PlayStation).unwrap();
        assert_eq!(cd.codecs, Some(vec![ChdCodecs::Cdlz, ChdCodecs::Cdzs, ChdCodecs::Cdzl, ChdCodecs::Cdfl]));

        settings.comp.mode = CompMode::Custom;
        settings.comp.dvd_codecs = vec!["zstd".into(), "lzma".into()];
        settings.comp.dvd_hunk_size = 4096;
        settings.comp.cd_codecs = vec!["cdzs".into(), "cdfl".into()];
        settings.comp.cd_hunk_size = 19584;
        let dvd = chd_args(Mode::Comp, &settings, Target::DvdChd, Platform::PlayStation2).unwrap();
        assert_eq!((dvd.codecs, dvd.hunk_size), (Some(vec![ChdCodecs::Zstd, ChdCodecs::Lzma]), Some(4096)));
        let cd = chd_args(Mode::Comp, &settings, Target::CdChd, Platform::PlayStation).unwrap();
        assert_eq!((cd.codecs, cd.hunk_size), (Some(vec![ChdCodecs::Cdzs, ChdCodecs::Cdfl]), Some(19584)));
    }
}
