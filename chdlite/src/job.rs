//! One input, one operation.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use romexplorer::common::models::{CueSheetStyle, Platform};
use romexplorer::rom::cemu_wrapper;
use romexplorer::rom::chd_info::read_chd_metadata;
use romexplorer::rom::chdman_wrapper::ChdReader;
use romexplorer::rom::content::{ContentPlan, InputSet};
use romexplorer::rom::decode_threads::DecodeThreads;
use romexplorer::rom::detect::sheet::CueSheetOptions;
use romexplorer::rom::detect::{hinted_metadata_cycle, HeuristicFlags};
use romexplorer::rom::dolphin_wrapper::{DiscCompression, DiscReader};
use romexplorer::rom::hash::HashFlags;
use romexplorer::rom::models::{ArchiveArgs, ChdArgs, ChdCodecs, ChdPresets, RvzArgs, RvzBlockSize, RvzCodecs};
use romexplorer::rom::operation::{Detect, ProgressEvent, Single, SingleArgs, SingleError, SingleOutcome, StageResult, StageSwitches};
use romexplorer::rom::platforms::helpers::{is_chd_platform, is_rvz_platform};
use romexplorer::rom::sheet_build::{rebuild_cue, Expected, TrackFile};

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    /// 0..=100.
    Progress(f32),
    Message(String),
    /// A file this job is about to write (so whoever supervises it can remove a partial one).
    Output(PathBuf),
}

/// Runs one job on the calling thread. `on_event` returning `false` asks it to stop, which
/// RVZ creation, extraction and hashing honour at once; chdman has no cancel hook, so a CHD
/// creation only stops this way when it runs as a worker process that can be killed
/// ([`crate::batch`]).
///
/// `Ok` carries the text to show for the finished job. On `Err` every output this job
/// started is removed.
pub fn run_one(spec: &JobSpec, on_event: &mut dyn FnMut(Event) -> bool) -> Result<String, String> {
    let mut outputs: Vec<PathBuf> = Vec::new();
    let result = run_inner(spec, &mut |event| {
        if let Event::Output(path) = &event {
            outputs.push(path.clone());
        }
        on_event(event)
    });
    if result.is_err() {
        remove_outputs(&outputs);
    }
    result
}

/// Removes what a job that did not finish was writing: files, and its scratch folders.
pub fn remove_outputs(outputs: &[PathBuf]) {
    for path in outputs {
        if path.is_dir() {
            let _ = std::fs::remove_dir_all(path);
        } else {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn run_inner(spec: &JobSpec, on_event: &mut dyn FnMut(Event) -> bool) -> Result<String, String> {
    let input = classify(&spec.input)?;
    match (spec.mode, input) {
        (Mode::Read, input) => read(&spec.input, &input),
        (Mode::Hash, _) => content_pass(spec, None, on_event),
        (Mode::Extract | Mode::Lite | Mode::Comp, Input::Chd | Input::Rvz) | (Mode::Extract, Input::Cso | Input::Pbp) => {
            let dir = output_dir(spec)?;
            content_pass(spec, Some(dir), on_event)
        }
        (Mode::Lite | Mode::Comp, Input::Cso | Input::Pbp) => create_from_container(spec, on_event),
        (Mode::Extract, Input::Image { .. }) => Err("nothing to extract: not a .chd, .rvz, .cso or .pbp".to_string()),
        (Mode::Lite | Mode::Comp, Input::Image { target: Target::Wua, source, .. }) => create_wua(spec, &source, on_event),
        (Mode::Lite | Mode::Comp, Input::Image { target: Target::Zar, source, .. }) => create_zar(spec, &source, on_event),
        (Mode::Lite | Mode::Comp, Input::Image { target: Target::Xiso, source, .. }) => create_xiso(spec, &source, on_event),
        (Mode::Lite | Mode::Comp, Input::Image { target, source, needs_cue }) => create(spec, target, &source, needs_cue, on_event),
    }
}

fn output_dir(spec: &JobSpec) -> Result<PathBuf, String> {
    let dir = spec.settings.output_dir().unwrap_or_else(|| spec.input.parent().unwrap_or_else(|| Path::new(".")).to_path_buf());
    std::fs::create_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    Ok(dir)
}

fn stem(path: &Path) -> String {
    // Lossy rather than a fixed fallback: a name that is not UTF-8 keeps every readable part.
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "output".to_string())
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
    let output = dir.join(format!("{}.{}", stem(&spec.input), target.extension()));
    let Some(claim) = claim_output(&output, spec.settings.output.force)? else {
        return Ok(format!("skipped, {} already exists", output.display()));
    };
    let input_bytes = InputSet::resolve(source).map(|set| set.total_bytes).unwrap_or(0);

    // A lone raw track: a CD CHD is made from a sheet, and the sheet names the track relative
    // to itself, so it has to sit beside the track for as long as the conversion runs.
    let mut temp_cue: Option<PathBuf> = None;
    let source = if needs_cue {
        let track = TrackFile::at(source).map_err(|err| format!("{}: {err}", source.display()))?;
        let rebuilt = rebuild_cue(&stem(source), &[track], &Expected::default(), &CueSheetOptions::default())?;
        // Hidden, so a folder dropped again after a killed run does not take it for a game.
        let cue = source.with_file_name(format!(".{}.chdlite.cue", stem(source)));
        // Told to the supervisor first, so a killed worker's sheet is removed too.
        on_event(Event::Output(cue.clone()));
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
        archive_stem: Some(stem(&spec.input)),
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
    let output = dir.join(format!("{}.wua", stem(&spec.input)));
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
    let output = output_dir(spec)?.join(format!("{}.{extension}", stem(&spec.input)));
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
        None => return Ok(format!("skipped, {}.zar already exists", stem(&spec.input))),
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
        None => return Ok(format!("skipped, {}.{} already exists", stem(&spec.input), Target::Xiso.extension())),
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
    let scratch = dir.join(format!(".chdlite-{}-{}", std::process::id(), stem(&spec.input)));
    let _ = std::fs::remove_dir_all(&scratch);
    // Told to the supervisor first, so a killed worker's scratch folder is removed too.
    on_event(Event::Output(scratch.clone()));
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
            let disc = JobSpec { mode: spec.mode, input: source.clone(), settings: settings.clone(), threads: spec.threads };
            let offset = 50.0 + 50.0 * i as f32 / count;
            done.push(create(&disc, target, source, false, &mut |event| relay(event, offset, 0.5 / count, &scratch, on_event))?);
        }
        Ok(done.join("\n"))
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    result
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
        let _ = std::fs::remove_file(previous_of(&self.output));
        let _ = std::fs::remove_file(marker_of(&self.output));
        self.finished = true;
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let _ = std::fs::remove_file(&self.output);
        let previous = previous_of(&self.output);
        if previous.exists() {
            let _ = std::fs::rename(&previous, &self.output);
        }
        let _ = std::fs::remove_file(marker_of(&self.output));
    }
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

/// Decodes the input's content once, writing it under `out_dir` (extract) or digesting it
/// (hash): one `Single`. For a CHD or RVZ that is the disc inside; for anything else, the
/// files themselves. The cue style also decides a GD-ROM's sheet (Redump: a cue, chdman: a
/// gdi).
fn content_pass(spec: &JobSpec, out_dir: Option<PathBuf>, on_event: &mut dyn FnMut(Event) -> bool) -> Result<String, String> {
    let extracting = out_dir.is_some();
    let flags = if extracting { HashFlags::empty() } else { hash_flags(&spec.settings.hash.algorithms)? };
    let style = cue_style(&spec.settings.extract.cue_style)?;
    let base = SingleArgs::default().with_sheet_style(style);

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

    let args = SingleArgs {
        stages: StageSwitches { extract: extracting, hash: !extracting, ..StageSwitches::NONE },
        hash_flags: flags,
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
    for record in outcome.input_hashes.map(|r| r.content_hashes).unwrap_or_default() {
        lines.push(format!("{}  ({} bytes)", record.path, record.size_bytes.unwrap_or(0)));
        for (label, value) in [("crc32", &record.crc32), ("md5", &record.md5), ("sha1", &record.sha1), ("sha256", &record.sha256), ("xxh3", &record.xxh3_128)] {
            if let Some(value) = value {
                lines.push(format!("  {label:<7}{value}"));
            }
        }
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

fn read(path: &Path, input: &Input) -> Result<String, String> {
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
            lines.push(format!("  iso size     {} ({iso} bytes)", human(iso)));
            lines.push(format!("  file size    {} ({:.1}%)", human(file_bytes), if iso > 0 { file_bytes as f64 * 100.0 / iso as f64 } else { 0.0 }));
            lines.push("  converts to  DVD CHD".to_string());
        }
        Input::Pbp => {
            let pbp = romexplorer::rom::pbp::Pbp::open(path)?;
            lines.push(format!("  type         PlayStation EBOOT.PBP, {} disc{}, {}", pbp.disc_count(), if pbp.disc_count() == 1 { "" } else { "s" }, human(file_bytes)));
            let stem = romexplorer::rom::pbp::output_stem(path);
            for disc in 0..pbp.disc_count() {
                let layout = romexplorer::rom::pbp::disc_layout(&pbp, disc, &stem)?;
                lines.push(format!("  disc {}       {}: {} track{}, {}", disc + 1, layout.cue_name, layout.tracks.len(), if layout.tracks.len() == 1 { "" } else { "s" }, human(layout.total_bytes())));
            }
            lines.push("  converts to  CD CHD".to_string());
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
