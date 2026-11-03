//! Archive creation: turns plain file(s) into a CHD/RVZ/WIA/GCZ/zip/7z on disk -- the
//! mirror image of [`super::extract`].
//!
//! Like extraction, every format is handled by dispatching to this crate's existing native
//! wrappers (`super::chdman_wrapper`, `super::dolphin_wrapper`, `super::sevenzip_wrapper`).
//! This module only decides *which* native command applies to an input, validates the
//! codec against the container, and names the output.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::chdman_wrapper::{self, ChdCompressionCodec, ChdmanArgs, ChdmanCommand};
use super::detect::descriptor;
use super::dolphin_wrapper::{self, DiscCompression, DiscFormat, DolphinConvertArgs};
use super::sevenzip_wrapper::{self, CompressionMethod, CreateArchiveArgs, CreateEntry};
use super::source::Source;

#[derive(Debug)]
pub enum ArchiveError {
    /// The input(s) can't go into the requested container (wrong file type, wrong count).
    UnsupportedInput(String),
    /// The codec isn't valid for the requested container (or for this kind of input).
    UnsupportedCodec(String),
    /// The native library for this format isn't available on this platform/build.
    Unavailable(String),
    /// `on_progress` returned `false`. Only possible for RVZ/WIA/GCZ (dolphin) and zip/7z
    /// (sevenzip) -- chdman's own progress callback has no way to signal cancellation, so
    /// creating a CHD always runs to completion once started.
    Cancelled,
    Io(String),
    Failed(String),
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedInput(message) => write!(f, "unsupported input: {message}"),
            Self::UnsupportedCodec(message) => write!(f, "unsupported codec: {message}"),
            Self::Unavailable(message) => write!(f, "archive creation unavailable: {message}"),
            Self::Cancelled => write!(f, "archive creation cancelled"),
            Self::Io(message) => write!(f, "archive io error: {message}"),
            Self::Failed(message) => write!(f, "archive creation failed: {message}"),
        }
    }
}

impl std::error::Error for ArchiveError {}

/// Where the created archive should land.
#[derive(Debug, Clone)]
pub enum ArchiveDestination {
    /// A caller-managed directory. Never cleaned up by this module.
    OutputDir(PathBuf),
    /// A scratch directory whose lifetime this module hands back to the caller via
    /// [`ArchivedPayload::cleanup`].
    Temp(PathBuf),
}

impl ArchiveDestination {
    fn dir(&self) -> &Path {
        match self {
            Self::OutputDir(dir) | Self::Temp(dir) => dir,
        }
    }

    fn cleanup_dir(&self) -> Option<PathBuf> {
        match self {
            Self::Temp(dir) => Some(dir.clone()),
            Self::OutputDir(_) => None,
        }
    }
}

/// What an archive creation produced.
#[derive(Debug, Clone)]
pub struct ArchivedPayload {
    /// The created container file.
    pub primary: PathBuf,
    /// Directory to delete once the caller is done, for [`ArchiveDestination::Temp`].
    pub cleanup: Option<PathBuf>,
}

impl ArchivedPayload {
    /// Deletes the scratch directory, if this creation owns one. Does nothing for an
    /// output directory.
    pub fn cleanup(&self) -> Result<(), ArchiveError> {
        let Some(dir) = self.cleanup.as_deref() else { return Ok(()) };
        fs::remove_dir_all(dir).map_err(|err| ArchiveError::Io(format!("failed to remove {}: {err}", dir.display())))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArchiveTarget {
    Chd,
    Rvz,
    Gcz,
    Wia,
    Zip,
    SevenZip,
}

impl ArchiveTarget {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Chd => "chd",
            Self::Rvz => "rvz",
            Self::Gcz => "gcz",
            Self::Wia => "wia",
            Self::Zip => "zip",
            Self::SevenZip => "7z",
        }
    }
}

/// Codec choice. `Option<Codec>::None` in [`ArchiveOptions`] means "the container's
/// default"; [`Codec::None`] means "explicitly no compression".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Codec {
    None,
    Zstd,
    Lzma,
    Lzma2,
    Bzip2,
    Deflate,
    Zlib,
    Flac,
    Huff,
}

/// How a bare `.bin`/`.img`/`.raw` (no descriptor to say what it is) becomes a CHD.
/// Defaults to [`ChdMedia::Raw`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChdMedia {
    Raw,
    Dvd,
    Hd,
}

#[derive(Debug, Clone, Default)]
pub struct ArchiveOptions {
    pub codec: Option<Codec>,
    /// Compression level where the container has one (RVZ/WIA, zip, 7z). `None` uses the
    /// container's default.
    pub level: Option<i32>,
    /// Block size for RVZ/WIA/GCZ. `None` uses the format's default.
    pub block_size: Option<u32>,
    /// Only consulted for a bare `.bin`/`.img`/`.raw` going to CHD.
    pub chd_media: Option<ChdMedia>,
    /// CHD only: every codec, best first (chdman takes up to 4 and tries each per hunk).
    /// Overrides `codec` when set; each is mapped to the command's own form (`Zstd` is
    /// `cdzs` for `createcd`).
    pub chd_codecs: Option<Vec<Codec>>,
    /// CHD hunk size in bytes. `None` uses chdman's default for the command.
    pub hunk_size: Option<u32>,
    /// CHD unit size in bytes (raw CHDs only). `None` picks 2048, or 512 when the input
    /// isn't a multiple of 2048.
    pub unit_size: Option<u32>,
    /// chdman compression threads (`-np`). `None` = chdman's own default.
    pub num_processors: Option<u32>,
    /// Dolphin compression threads for GCZ/WIA/RVZ. `None` = one per logical CPU.
    pub compress_threads: Option<u32>,
    /// Dolphin user directory (temporary processing files). `None` = Dolphin's default.
    pub user_dir: Option<PathBuf>,
    /// RVZ/WIA/GCZ: drop a disc's junk (padding) data. Smaller, but not reversible to the
    /// original image.
    pub scrub: bool,
}

const DEFAULT_LEVEL: i32 = 5;
const RVZ_DEFAULT_BLOCK_SIZE: u32 = 128 * 1024;
const WIA_DEFAULT_BLOCK_SIZE: u32 = 2 * 1024 * 1024;
const GCZ_DEFAULT_BLOCK_SIZE: u32 = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputKind {
    Cue,
    Gdi,
    Toc,
    Iso,
    Bin,
    Img,
    Raw,
    Other,
}

fn classify(path: &Path) -> InputKind {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
        "cue" => InputKind::Cue,
        "gdi" => InputKind::Gdi,
        "toc" => InputKind::Toc,
        "iso" => InputKind::Iso,
        "bin" => InputKind::Bin,
        "img" => InputKind::Img,
        "raw" => InputKind::Raw,
        _ => InputKind::Other,
    }
}

/// Creates a `target` container from `inputs` inside `destination`.
///
/// CHD/RVZ/WIA/GCZ take exactly one input; zip/7z take one or more files (a `.cue`/`.gdi`
/// input pulls in the track files it lists). The native command is chosen from the input:
/// a `.cue`/`.gdi`/`.toc` is `createcd`, an `.iso` is `createdvd`, and a bare
/// `.bin`/`.img`/`.raw` is `createraw` unless [`ArchiveOptions::chd_media`] says otherwise.
/// Anything else for CHD/RVZ/WIA/GCZ is [`ArchiveError::UnsupportedInput`]. The output is
/// named `<first input stem>.<container extension>`.
///
/// `on_progress(message, percent)` is called as the underlying native library reports
/// progress, with `percent` in `0.0..=100.0` -- except chdman (CHD), whose own callback
/// sometimes has no percent for a given log line (a message-only line, or a command with no
/// progress loop at all), reporting `-1.0` for those. chdman's callback also can't be
/// cancelled: returning `false` from `on_progress` while creating a CHD is observed but has
/// no effect. Returning `false` cancels creation for RVZ/WIA/GCZ and zip/7z, surfaced as
/// [`ArchiveError::Cancelled`].
pub fn archive<F>(inputs: &[PathBuf], target: ArchiveTarget, options: &ArchiveOptions, destination: ArchiveDestination, mut on_progress: F) -> Result<ArchivedPayload, ArchiveError>
where
    F: FnMut(&str, f32) -> bool,
{
    let Some(first) = inputs.first() else {
        return Err(ArchiveError::UnsupportedInput("no input given".to_string()));
    };
    for input in inputs {
        if !input.is_file() {
            return Err(ArchiveError::Io(format!("input is not a file: {}", input.display())));
        }
    }

    let out_dir = destination.dir();
    let stem = first.file_stem().and_then(|s| s.to_str()).unwrap_or("archive");
    let output = out_dir.join(format!("{stem}.{}", target.extension()));

    match target {
        ArchiveTarget::Chd => {
            let input = single_input(inputs, target)?;
            let command = chd_command(input, options)?;
            let args = chd_args(input, &output, command, options)?;
            prepare_output(out_dir)?;
            // chdman's own callback shape has no cancel (see ArchiveError::Cancelled's doc
            // comment); its bool return is simply discarded here.
            chdman_wrapper::run(command, &args, |text, percent| {
                on_progress(text, percent);
            })
            .map_err(map_chdman_error)?;
        }
        ArchiveTarget::Rvz | ArchiveTarget::Wia | ArchiveTarget::Gcz => {
            let input = single_input(inputs, target)?;
            if classify(input) != InputKind::Iso {
                return Err(ArchiveError::UnsupportedInput(format!("{} only accepts an .iso input: {}", target.extension(), input.display())));
            }
            let mut args = disc_args(input, &output, target, options)?;
            args.compress_threads = options.compress_threads;
            args.user_dir = options.user_dir.clone();
            prepare_output(out_dir)?;
            dolphin_wrapper::convert(&args, &mut on_progress).map_err(map_dolphin_error)?;
        }
        ArchiveTarget::Zip | ArchiveTarget::SevenZip => {
            let args = file_archive_args(inputs, target, options)?;
            prepare_output(out_dir)?;
            // A cancelled or failed write leaves a partial archive behind: never a finished one.
            sevenzip_wrapper::create_archive(&output, &args, &mut on_progress).map_err(|e| {
                let _ = fs::remove_file(&output);
                map_sevenzip_error(e)
            })?;
        }
    }

    if !output.is_file() {
        return Err(ArchiveError::Failed(format!("no output was produced at {}", output.display())));
    }
    Ok(ArchivedPayload { primary: output, cleanup: destination.cleanup_dir() })
}

fn prepare_output(out_dir: &Path) -> Result<(), ArchiveError> {
    fs::create_dir_all(out_dir).map_err(|err| ArchiveError::Io(format!("failed to create {}: {err}", out_dir.display())))
}

fn single_input(inputs: &[PathBuf], target: ArchiveTarget) -> Result<&Path, ArchiveError> {
    match inputs {
        [only] => Ok(only.as_path()),
        _ => Err(ArchiveError::UnsupportedInput(format!("{} takes exactly one input, got {}", target.extension(), inputs.len()))),
    }
}

// ---------------------------------------------------------------------------
// CHD
// ---------------------------------------------------------------------------

fn chd_command(input: &Path, options: &ArchiveOptions) -> Result<ChdmanCommand, ArchiveError> {
    let kind = classify(input);
    match kind {
        InputKind::Cue | InputKind::Gdi | InputKind::Toc | InputKind::Iso => {
            if options.chd_media.is_some() {
                return Err(ArchiveError::UnsupportedInput(format!("chd_media only applies to a bare .bin/.img/.raw, not {}", input.display())));
            }
            Ok(if kind == InputKind::Iso { ChdmanCommand::CreateDvd } else { ChdmanCommand::CreateCd })
        }
        InputKind::Bin | InputKind::Img | InputKind::Raw => Ok(match options.chd_media.unwrap_or(ChdMedia::Raw) {
            ChdMedia::Raw => ChdmanCommand::CreateRaw,
            ChdMedia::Dvd => ChdmanCommand::CreateDvd,
            ChdMedia::Hd => ChdmanCommand::CreateHd,
        }),
        InputKind::Other => Err(ArchiveError::UnsupportedInput(format!("cannot create a CHD from {}", input.display()))),
    }
}

fn chd_codec(codec: Codec, command: ChdmanCommand) -> Result<ChdCompressionCodec, ArchiveError> {
    let cd = command == ChdmanCommand::CreateCd;
    match (codec, cd) {
        (Codec::None, _) => Ok(ChdCompressionCodec::None),
        (Codec::Zstd, true) => Ok(ChdCompressionCodec::CdZstd),
        (Codec::Zstd, false) => Ok(ChdCompressionCodec::Zstd),
        (Codec::Lzma, true) => Ok(ChdCompressionCodec::CdLzma),
        (Codec::Lzma, false) => Ok(ChdCompressionCodec::Lzma),
        (Codec::Zlib, true) => Ok(ChdCompressionCodec::CdZlib),
        (Codec::Zlib, false) => Ok(ChdCompressionCodec::Zlib),
        (Codec::Flac, true) => Ok(ChdCompressionCodec::CdFlac),
        (Codec::Flac, false) => Ok(ChdCompressionCodec::Flac),
        (Codec::Huff, false) => Ok(ChdCompressionCodec::Huffman),
        (other, _) => Err(ArchiveError::UnsupportedCodec(format!(
            "{other:?} is not valid for a CHD made with {}",
            if cd { "createcd" } else { "this command" }
        ))),
    }
}

fn chd_args(input: &Path, output: &Path, command: ChdmanCommand, options: &ArchiveOptions) -> Result<ChdmanArgs, ArchiveError> {
    let input_len = if command == ChdmanCommand::CreateRaw && options.unit_size.is_none() {
        fs::metadata(input).map_err(|err| ArchiveError::Io(format!("failed to stat {}: {err}", input.display())))?.len()
    } else {
        0
    };
    chd_args_named(input, input_len, output, command, options)
}

/// [`chd_args`] where `input` may be a plain name (a `Source::name`) rather than a file on
/// disk; `input_len` stands in for the file's size (raw CHD unit-size guess).
fn chd_args_named(input: &Path, input_len: u64, output: &Path, command: ChdmanCommand, options: &ArchiveOptions) -> Result<ChdmanArgs, ArchiveError> {
    let compression = match (&options.chd_codecs, options.codec) {
        (Some(list), _) => {
            let mut tags = Vec::with_capacity(list.len());
            for codec in list {
                let tag = chd_codec(*codec, command)?;
                // `zstd` and `cdzs` both land on one tag for a given command; chdman wants it once.
                if !tags.contains(&tag) {
                    tags.push(tag);
                }
            }
            Some(tags)
        }
        (None, Some(codec)) => Some(vec![chd_codec(codec, command)?]),
        (None, None) => None,
    };

    let unit_size = if command == ChdmanCommand::CreateRaw {
        Some(options.unit_size.unwrap_or(if input_len % 2048 == 0 { 2048 } else { 512 }))
    } else {
        None
    };

    Ok(ChdmanArgs {
        input: Some(input.to_path_buf()),
        output: Some(output.to_path_buf()),
        output_force: true,
        compression,
        hunk_size: options.hunk_size,
        unit_size,
        num_processors: options.num_processors,
        ..Default::default()
    })
}

// ---------------------------------------------------------------------------
// RVZ / WIA / GCZ
// ---------------------------------------------------------------------------

fn disc_args(input: &Path, output: &Path, target: ArchiveTarget, options: &ArchiveOptions) -> Result<DolphinConvertArgs, ArchiveError> {
    disc_args_unscrubbed(input, output, target, options).map(|args| DolphinConvertArgs { scrub: options.scrub, ..args })
}

fn disc_args_unscrubbed(input: &Path, output: &Path, target: ArchiveTarget, options: &ArchiveOptions) -> Result<DolphinConvertArgs, ArchiveError> {
    let level = options.level.unwrap_or(DEFAULT_LEVEL);

    match target {
        ArchiveTarget::Gcz => {
            if let Some(codec) = options.codec {
                return Err(ArchiveError::UnsupportedCodec(format!("gcz has no codec choice, got {codec:?}")));
            }
            Ok(DolphinConvertArgs::new(input, output, DiscFormat::Gcz).with_block_size(options.block_size.unwrap_or(GCZ_DEFAULT_BLOCK_SIZE)))
        }
        ArchiveTarget::Rvz => {
            let compression = match options.codec.unwrap_or(Codec::Zstd) {
                Codec::Zstd => DiscCompression::Zstd,
                Codec::Lzma => DiscCompression::Lzma,
                Codec::Lzma2 => DiscCompression::Lzma2,
                Codec::Bzip2 => DiscCompression::Bzip2,
                Codec::None => DiscCompression::None,
                other => return Err(ArchiveError::UnsupportedCodec(format!("{other:?} is not valid for rvz"))),
            };
            Ok(disc_compressed(input, output, DiscFormat::Rvz, compression, level, options.block_size.unwrap_or(RVZ_DEFAULT_BLOCK_SIZE)))
        }
        ArchiveTarget::Wia => {
            let compression = match options.codec.unwrap_or(Codec::Lzma) {
                Codec::Lzma => DiscCompression::Lzma,
                Codec::Lzma2 => DiscCompression::Lzma2,
                Codec::Bzip2 => DiscCompression::Bzip2,
                Codec::None => DiscCompression::None,
                other => return Err(ArchiveError::UnsupportedCodec(format!("{other:?} is not valid for wia"))),
            };
            Ok(disc_compressed(input, output, DiscFormat::Wia, compression, level, options.block_size.unwrap_or(WIA_DEFAULT_BLOCK_SIZE)))
        }
        _ => unreachable!("disc_args is only called for rvz/wia/gcz"),
    }
}

fn disc_compressed(input: &Path, output: &Path, format: DiscFormat, compression: DiscCompression, level: i32, block_size: u32) -> DolphinConvertArgs {
    let level = if compression == DiscCompression::None { 0 } else { level };
    DolphinConvertArgs::new(input, output, format).with_block_size(block_size).with_compression(compression, level)
}

// ---------------------------------------------------------------------------
// zip / 7z
// ---------------------------------------------------------------------------

/// The zip/7z container format, codec and level for `target` under `options`.
fn file_archive_method(target: ArchiveTarget, options: &ArchiveOptions) -> Result<(sevenzip_wrapper::ArchiveFormat, CompressionMethod, i32), ArchiveError> {
    let seven = target == ArchiveTarget::SevenZip;
    let method = match options.codec.unwrap_or(if seven { Codec::Lzma2 } else { Codec::Deflate }) {
        Codec::None => CompressionMethod::Copy,
        Codec::Deflate => CompressionMethod::Deflate,
        Codec::Lzma => CompressionMethod::Lzma,
        Codec::Bzip2 => CompressionMethod::BZip2,
        Codec::Zstd => CompressionMethod::Zstd,
        Codec::Lzma2 if seven => CompressionMethod::Lzma2,
        other => return Err(ArchiveError::UnsupportedCodec(format!("{other:?} is not valid for {}", target.extension()))),
    };
    let format = if seven { sevenzip_wrapper::ArchiveFormat::SevenZ } else { sevenzip_wrapper::ArchiveFormat::Zip };
    Ok((format, method, options.level.unwrap_or(DEFAULT_LEVEL)))
}

fn file_archive_args(inputs: &[PathBuf], target: ArchiveTarget, options: &ArchiveOptions) -> Result<CreateArchiveArgs, ArchiveError> {
    let (format, method, level) = file_archive_method(target, options)?;

    let mut paths: Vec<PathBuf> = Vec::new();
    for input in inputs {
        paths.push(input.clone());
        if matches!(classify(input), InputKind::Cue | InputKind::Gdi) {
            paths.extend(descriptor::descriptor_payload_paths(input));
        }
    }

    let mut seen = HashSet::new();
    let mut entries = Vec::with_capacity(paths.len());
    for path in paths {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| ArchiveError::UnsupportedInput(format!("input has no usable file name: {}", path.display())))?
            .to_string();
        if !seen.insert(name.clone()) {
            return Err(ArchiveError::UnsupportedInput(format!("two inputs would share the archive member name '{name}'")));
        }
        entries.push(CreateEntry { input_path: path, archive_name: name });
    }

    Ok(CreateArchiveArgs { format, method, level, entries })
}

// ---------------------------------------------------------------------------
// Creation from streams
// ---------------------------------------------------------------------------

/// The output file for `stem` in `out_dir`: `<stem>.<container extension>` (the same rule
/// [`archive`] applies).
pub fn output_path(out_dir: &Path, stem: &str, target: ArchiveTarget) -> PathBuf {
    out_dir.join(format!("{stem}.{}", target.extension()))
}

/// The chdman create command for a CHD whose primary input is called `name` (a file name
/// or a `Source::name`): the same rule as [`archive`]'s (`.cue`/`.gdi`/`.toc` -> createcd,
/// `.iso` -> createdvd, bare `.bin`/`.img`/`.raw` -> per `chd_media`).
pub fn chd_command_for_name(name: &str, options: &ArchiveOptions) -> Result<ChdmanCommand, ArchiveError> {
    chd_command(Path::new(name), options)
}

/// [`archive`] with the input bytes coming from `sources` (see `super::source`) instead of
/// files on disk, written to `output` (whose directory is created). Which source is the
/// primary input:
/// - CHD: `primary` names the sheet (createcd; the track files are the other sources,
///   matched by name) or the single image source; `command` overrides the one derived from
///   that name.
/// - RVZ/WIA/GCZ: exactly one source (random access preferred -- a sequential source is
///   spooled to a temporary file by Dolphin). `on_input_data(offset, bytes)` receives every
///   input byte once, in order, while Dolphin reads it out of order: that is the place to
///   hash the input (a `Source::hashed` wrapper would see a gap). Ignored for other targets.
/// - zip/7z: every source becomes a member, named `Source::name`.
///
/// chdman's progress callback can't cancel (see [`ArchiveError::Cancelled`]).
pub fn archive_from_sources(
    output: &Path,
    target: ArchiveTarget,
    options: &ArchiveOptions,
    command: Option<ChdmanCommand>,
    primary: &str,
    sources: &mut [Source],
    mut on_input_data: Option<&mut dyn FnMut(u64, &[u8]) -> bool>,
    mut on_progress: impl FnMut(&str, f32) -> bool,
) -> Result<(), ArchiveError> {
    if sources.is_empty() {
        return Err(ArchiveError::UnsupportedInput("no source given".to_string()));
    }
    if let Some(parent) = output.parent() {
        prepare_output(parent)?;
    }
    match target {
        ArchiveTarget::Chd => {
            let command = match command {
                Some(command) => command,
                None => chd_command_for_name(primary, options)?,
            };
            let primary_len = sources.iter().find(|s| s.name == primary).map(|s| s.size).unwrap_or(0);
            let args = chd_args_named(Path::new(primary), primary_len, output, command, options)?;
            chdman_wrapper::create_from_sources(command, &args, sources, |text, percent| {
                on_progress(text, percent);
            })
            .map_err(map_chdman_error)?;
        }
        ArchiveTarget::Rvz | ArchiveTarget::Wia | ArchiveTarget::Gcz => {
            let [source] = sources else {
                return Err(ArchiveError::UnsupportedInput(format!("{} takes exactly one input, got {}", target.extension(), sources.len())));
            };
            let mut args = disc_args(Path::new(primary), output, target, options)?;
            args.compress_threads = options.compress_threads;
            args.user_dir = options.user_dir.clone();
            dolphin_wrapper::convert_from_source(
                &args,
                source,
                |_, _, offset, bytes| on_input_data.as_mut().map_or(true, |tap| tap(offset, bytes)),
                &mut on_progress,
            )
            .map_err(map_dolphin_error)?;
        }
        ArchiveTarget::Zip | ArchiveTarget::SevenZip => {
            let (format, method, level) = file_archive_method(target, options)?;
            let mut seen = HashSet::new();
            for source in sources.iter() {
                if !seen.insert(source.name.clone()) {
                    return Err(ArchiveError::UnsupportedInput(format!("two sources share the archive member name '{}'", source.name)));
                }
            }
            sevenzip_wrapper::create_archive_from_sources(output, format, method, level, sources, &mut on_progress).map_err(|e| {
                let _ = fs::remove_file(output);
                map_sevenzip_error(e)
            })?;
        }
    }
    if !output.is_file() {
        return Err(ArchiveError::Failed(format!("no output was produced at {}", output.display())));
    }
    Ok(())
}

impl super::models::ArchiveArgs {
    /// Maps this operation's per-format settings onto [`ArchiveOptions`] for `target`.
    /// `platform`/`is_cd` feed the CHD preset (`ChdArgs::apply_preset`).
    pub fn archive_options(&self, target: ArchiveTarget, platform: crate::common::models::Platform, is_cd: bool) -> Result<ArchiveOptions, ArchiveError> {
        use super::models::{ChdCodecs, RvzCodecs, SevenZipCodecs, ZipCodecs};
        let mut options = ArchiveOptions::default();
        match target {
            ArchiveTarget::Chd => {
                let mut chd = self.chd_args.clone();
                chd.select_media(is_cd);
                chd.apply_preset(platform);
                let to_codec = |codec: &ChdCodecs| match codec {
                    ChdCodecs::Zstd | ChdCodecs::Cdzs => Codec::Zstd,
                    ChdCodecs::Zlib | ChdCodecs::Cdzl => Codec::Zlib,
                    ChdCodecs::Lzma | ChdCodecs::Cdlz => Codec::Lzma,
                    ChdCodecs::Flac | ChdCodecs::Cdfl => Codec::Flac,
                    ChdCodecs::Huff => Codec::Huff,
                };
                // The whole list: only passing the first used to drop the rest, so the
                // pc preset's `cdzs,cdfl` reached chdman as `cdzs` alone.
                options.chd_codecs = chd.codecs.as_ref().map(|codecs| codecs.iter().map(to_codec).collect());
                options.codec = chd.codecs.as_ref().and_then(|codecs| codecs.first()).map(to_codec);
                options.hunk_size = chd.hunk_size.map(|v| v as u32);
                options.unit_size = chd.unit_size.map(|v| v as u32);
                options.num_processors = chd.num_processors.map(|v| v as u32);
            }
            ArchiveTarget::Rvz | ArchiveTarget::Wia | ArchiveTarget::Gcz => {
                let rvz = self.rvz_args;
                if target != ArchiveTarget::Gcz {
                    options.codec = Some(match rvz.algo {
                        RvzCodecs::Uncompressed => Codec::None,
                        RvzCodecs::Zstd => Codec::Zstd,
                        RvzCodecs::Bzip2 => Codec::Bzip2,
                        RvzCodecs::Lzma => Codec::Lzma,
                        RvzCodecs::Lzma2 => Codec::Lzma2,
                    });
                    options.level = Some(i32::from(rvz.compression_level));
                }
                options.block_size = Some(rvz.block_size.bytes());
                options.scrub = rvz.remove_junk_data;
            }
            ArchiveTarget::Zip => {
                let zip = self.zip_args;
                options.codec = Some(match zip.codec {
                    ZipCodecs::Store => Codec::None,
                    ZipCodecs::Deflate => Codec::Deflate,
                    ZipCodecs::Bzip2 => Codec::Bzip2,
                    ZipCodecs::Lzma => Codec::Lzma,
                    ZipCodecs::Zstd => Codec::Zstd,
                    other => return Err(ArchiveError::UnsupportedCodec(format!("{other:?} is not supported for zip creation"))),
                });
                options.level = Some(i32::from(zip.compression_level));
            }
            ArchiveTarget::SevenZip => {
                let seven = self.seven_zip_args;
                options.codec = Some(match seven.codec {
                    SevenZipCodecs::Copy => Codec::None,
                    SevenZipCodecs::Deflate => Codec::Deflate,
                    SevenZipCodecs::Bzip2 => Codec::Bzip2,
                    SevenZipCodecs::Lzma => Codec::Lzma,
                    SevenZipCodecs::Lzma2 => Codec::Lzma2,
                    SevenZipCodecs::Zstd => Codec::Zstd,
                    other => return Err(ArchiveError::UnsupportedCodec(format!("{other:?} is not supported for 7z creation"))),
                });
                options.level = Some(i32::from(seven.compression_level));
            }
        }
        Ok(options)
    }
}

// ---------------------------------------------------------------------------
// Error mapping
// ---------------------------------------------------------------------------

fn map_chdman_error(err: chdman_wrapper::ChdmanError) -> ArchiveError {
    match err {
        chdman_wrapper::ChdmanError::Unavailable(message) => ArchiveError::Unavailable(message),
        chdman_wrapper::ChdmanError::Cancelled { .. } => ArchiveError::Cancelled,
        other => ArchiveError::Failed(other.to_string()),
    }
}

fn map_dolphin_error(err: dolphin_wrapper::DolphinRvzError) -> ArchiveError {
    match err {
        dolphin_wrapper::DolphinRvzError::Unavailable(message) => ArchiveError::Unavailable(message),
        dolphin_wrapper::DolphinRvzError::Cancelled => ArchiveError::Cancelled,
        other => ArchiveError::Failed(other.to_string()),
    }
}

fn map_sevenzip_error(err: sevenzip_wrapper::SevenZipError) -> ArchiveError {
    match err {
        sevenzip_wrapper::SevenZipError::Unavailable(message) => ArchiveError::Unavailable(message),
        sevenzip_wrapper::SevenZipError::Cancelled => ArchiveError::Cancelled,
        other => ArchiveError::Failed(other.to_string()),
    }
}

#[cfg(test)]
mod chd_codec_list_tests {
    use super::*;

    /// Every codec reaches chdman, in order, each in the command's own spelling, once.
    #[test]
    fn the_whole_codec_list_reaches_chdman() {
        let options = ArchiveOptions { chd_codecs: Some(vec![Codec::Zstd, Codec::Flac, Codec::Zstd]), ..Default::default() };
        let cd = chd_args_named(Path::new("a.cue"), 0, Path::new("a.chd"), ChdmanCommand::CreateCd, &options).unwrap();
        assert_eq!(cd.compression, Some(vec![ChdCompressionCodec::CdZstd, ChdCompressionCodec::CdFlac]));
        let dvd = chd_args_named(Path::new("a.iso"), 0, Path::new("a.chd"), ChdmanCommand::CreateDvd, &options).unwrap();
        assert_eq!(dvd.compression, Some(vec![ChdCompressionCodec::Zstd, ChdCompressionCodec::Flac]));
    }
}
