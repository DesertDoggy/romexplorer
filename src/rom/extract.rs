//! Extraction: turns a CHD/RVZ/WIA/GCZ/zip/7z input into real file(s) on disk.
//!
//! Every format is handled by dispatching to this crate's existing native wrappers --
//! `super::chdman_wrapper`, `super::dolphin_wrapper`, `super::sevenzip_wrapper` -- so no
//! container format is parsed or decompressed here. This is also the *only* way content
//! inside those containers becomes readable: detection (`super::detect`) deliberately never
//! decompresses, so anything needing a CHD/RVZ's actual payload extracts it first and then
//! works against the plain file this produces.

use std::fs;
use std::path::{Path, PathBuf};

use crate::common::models::ArchiveFormat;

use super::models::{ChdMetadata, ChdTrackType, TrackMetadata};
use super::chdman_wrapper::{self, ChdmanArgs, ChdmanCommand};
use super::dolphin_wrapper::{self, DiscFormat, DolphinConvertArgs};
use super::sevenzip_wrapper::{self, SevenZipArchive};

#[derive(Debug)]
pub enum ExtractError {
    /// The input isn't a container this module can extract (a raw ISO, say, which is
    /// already a plain file and needs no extraction).
    NotExtractable(ArchiveFormat),
    /// The native library for this format isn't available on this platform/build.
    Unavailable(String),
    /// `on_progress` returned `false`. Only possible for RVZ/WIA/GCZ (dolphin) and
    /// zip/7z (sevenzip) -- chdman's own progress callback has no way to signal
    /// cancellation, so a CHD extraction always runs to completion once started.
    Cancelled,
    Io(String),
    Failed(String),
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotExtractable(format) => write!(f, "{format:?} inputs are not extractable"),
            Self::Unavailable(message) => write!(f, "extractor unavailable: {message}"),
            Self::Cancelled => write!(f, "extraction cancelled"),
            Self::Io(message) => write!(f, "extraction io error: {message}"),
            Self::Failed(message) => write!(f, "extraction failed: {message}"),
        }
    }
}

impl std::error::Error for ExtractError {}

/// Where extracted files should land.
#[derive(Debug, Clone)]
pub enum ExtractDestination {
    /// A caller-managed directory (typically the run's output directory). Never cleaned
    /// up by this module.
    OutputDir(PathBuf),
    /// A scratch directory whose lifetime this module hands back to the caller via
    /// [`ExtractedPayload::cleanup`].
    Temp(PathBuf),
}

impl ExtractDestination {
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

/// What an extraction produced.
#[derive(Debug, Clone)]
pub struct ExtractedPayload {
    /// The file to treat as the extraction's result -- the disc image, or the archive's
    /// primary member.
    pub primary: PathBuf,
    /// Additional files produced alongside `primary` (a `.cue` written next to a `.bin`,
    /// the remaining members of a multi-file archive).
    pub siblings: Vec<PathBuf>,
    /// Directory to delete once the caller is done, for [`ExtractDestination::Temp`].
    pub cleanup: Option<PathBuf>,
}

impl ExtractedPayload {
    /// Deletes the scratch directory, if this extraction owns one. Safe to call on a
    /// payload extracted to an output directory (does nothing).
    pub fn cleanup(&self) -> Result<(), ExtractError> {
        let Some(dir) = self.cleanup.as_deref() else { return Ok(()) };
        fs::remove_dir_all(dir).map_err(|err| ExtractError::Io(format!("failed to remove {}: {err}", dir.display())))
    }
}

fn file_stem_or(path: &Path, fallback: &str) -> String {
    path.file_stem().and_then(|stem| stem.to_str()).unwrap_or(fallback).to_string()
}

/// Which container a CD/GD-ROM CHD's track data extracts to. Meaningless for DVD/raw CHDs
/// (no track structure to lay out) and for every other [`ArchiveFormat`] -- [`extract`]
/// ignores it for those.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CdOutputFormat {
    /// `.cue` + one `.bin` per track, or a single combined `.bin` for a 1-track disc.
    /// chdman's ordinary redump-style output; the default.
    #[default]
    CueBin,
    /// `.gdi` + one file per track, always split (GDI mode has no non-split form) and
    /// always 2-digit-numbered (`{stem}01.bin`, `{stem}02.raw`, ... -- an `AUDIO`-type
    /// track gets a `.raw` extension instead of `.bin`, GDI mode's own convention). This
    /// is the format Redump-style Dreamcast GD-ROM dumps ship in, and is a **different
    /// byte-for-byte encoding of the same disc** from that same disc's `.cue`/`.bin` set
    /// -- not just a renaming of it (see `GdiCase` in `tests/src/support/discover.rs` for
    /// where that's exercised end to end).
    Gdi,
}

/// Extracts `input` into `destination`. `chd_metadata` is only read for
/// [`ArchiveFormat::Chd`] inputs, where it decides which chdman extract command applies
/// (CD/DVD/raw) and, for CD/GD-ROM CHDs, whether `--splitbin` is used -- pass the result of
/// `super::chd_info::read_chd_metadata(input)`. `None` falls back to `extractraw`, which
/// only produces a correct result for a non-track CHD (DVD, generic raw, hard disk).
/// `cd_output_format` picks `.cue` vs `.gdi` track-data layout for a CD/GD-ROM CHD; ignored
/// otherwise.
///
/// `on_progress(message, percent)` is called as the underlying native library reports
/// progress, with `percent` in `0.0..=100.0` -- except:
/// - chdman (CHD) sometimes has no percent for a given log line (a message-only line, or a
///   command with no progress loop at all -- see `chdman_wrapper::run`'s own docs), in
///   which case `percent` is `-1.0`. chdman's callback also can't be cancelled: returning
///   `false` from `on_progress` during a CHD extraction is observed but has no effect.
/// - zip/7z opening an archive (before any member is extracted) always reports `-1.0`
///   ("Opening archive"), since 7-Zip's own open step doesn't track a byte-based percent.
/// - zip/7z member extraction blends each member's own percent with how many members
///   have already finished, so the overall number moves smoothly across a multi-file
///   archive rather than jumping from 0 to 100 once per member.
///
/// Returning `false` from `on_progress` cancels the extraction for RVZ/WIA/GCZ and zip/7z,
/// surfaced as [`ExtractError::Cancelled`] (see the CHD caveat above).
pub fn extract<F>(
    input: &Path,
    format: ArchiveFormat,
    destination: ExtractDestination,
    chd_metadata: Option<&ChdMetadata>,
    cd_output_format: CdOutputFormat,
    mut on_progress: F,
) -> Result<ExtractedPayload, ExtractError>
where
    F: FnMut(&str, f32) -> bool,
{
    let out_dir = destination.dir();
    fs::create_dir_all(out_dir).map_err(|err| ExtractError::Io(format!("failed to create {}: {err}", out_dir.display())))?;
    let on_progress: &mut dyn FnMut(&str, f32) -> bool = &mut on_progress;

    match format {
        ArchiveFormat::Chd => extract_chd(input, &destination, chd_metadata, cd_output_format, on_progress),
        ArchiveFormat::Rvz | ArchiveFormat::Wia | ArchiveFormat::Gcz => extract_rvz(input, &destination, on_progress),
        ArchiveFormat::Zip | ArchiveFormat::SevenZip => extract_archive(input, format, &destination, on_progress),
        other => Err(ExtractError::NotExtractable(other)),
    }
}

/// CD/GD-ROM CHDs extract to their track data (`.cue`/`.gdi`, per `cd_output_format`); DVD
/// and raw CHDs extract to a single flat image.
///
/// For the CD/GD-ROM case, whether `--splitbin` is used depends on the track count, not on
/// whether the disc happens to have exactly one physical track file: passing `--splitbin`
/// for a single-track disc still makes chdman name its one output file with a `(Track 01)`
/// suffix (chdman requires a `%t` placeholder in the output name whenever `--splitbin` is
/// set), which is wrong for a disc that only ever had one track to begin with. So for
/// [`CdOutputFormat::CueBin`], `--splitbin` is passed only when `metadata` reports more than
/// one track. [`CdOutputFormat::Gdi`] always splits -- chdman's GDI mode has no non-split
/// form (`is_splitbin = mode == MODE_GDI || ...` in `do_extract_cd`) -- so this always
/// passes it there regardless of track count.
fn extract_chd(
    input: &Path,
    destination: &ExtractDestination,
    metadata: Option<&ChdMetadata>,
    cd_output_format: CdOutputFormat,
    on_progress: &mut dyn FnMut(&str, f32) -> bool,
) -> Result<ExtractedPayload, ExtractError> {
    let out_dir = destination.dir();
    let stem = file_stem_or(input, "extracted");
    let track_count = metadata.map(|meta| meta.tracks.len()).unwrap_or(0);

    if track_count > 0 {
        return match cd_output_format {
            CdOutputFormat::CueBin => extract_chd_cue(input, out_dir, &stem, track_count, destination, on_progress),
            CdOutputFormat::Gdi => {
                let tracks = &metadata.expect("track_count > 0 implies metadata is Some").tracks;
                extract_chd_gdi(input, out_dir, &stem, tracks, destination, on_progress)
            }
        };
    }

    // DVD and generic-raw CHDs both extract via the same chdman handler (`extractdvd` is
    // chdman's own alias for `extractraw` -- see `do_extract_raw` in
    // `external/mame/src/tools/chdman.cpp`'s command table); only the output extension
    // differs, as a naming nicety.
    let is_dvd = metadata.map(|meta| meta.is_dvd).unwrap_or(false);
    let extension = if is_dvd { "iso" } else { "raw" };
    let primary = out_dir.join(format!("{stem}.{extension}"));
    let args = ChdmanArgs { input: Some(input.to_path_buf()), output: Some(primary.clone()), output_force: true, ..Default::default() };
    // chdman's own callback shape has no cancel (see ExtractError::Cancelled's doc comment);
    // its bool return is simply discarded here.
    chdman_wrapper::run(ChdmanCommand::ExtractRaw, &args, |text, percent| {
        on_progress(text, percent);
    })
    .map_err(map_chdman_error)?;

    Ok(ExtractedPayload { primary, siblings: Vec::new(), cleanup: destination.cleanup_dir() })
}

/// Names deliberately left to chdman's own default (no `-ob` passed): a single-track CHD
/// gets a plain `{stem}.bin`, a multi-track one gets `{stem} (Track {N}).bin` per track,
/// zero-padded to 2 digits once there are 10 or more tracks -- see the `--splitbin` naming
/// logic in `do_extract_cd` (`external/mame/src/tools/chdman.cpp`). [`cue_bin_names`]
/// reproduces that same rule so the caller knows the resulting sibling paths without
/// re-scanning the output directory.
fn extract_chd_cue(
    input: &Path,
    out_dir: &Path,
    stem: &str,
    track_count: usize,
    destination: &ExtractDestination,
    on_progress: &mut dyn FnMut(&str, f32) -> bool,
) -> Result<ExtractedPayload, ExtractError> {
    let cue = out_dir.join(format!("{stem}.cue"));
    let split = track_count > 1;

    let args = ChdmanArgs {
        input: Some(input.to_path_buf()),
        output: Some(cue.clone()),
        output_force: true,
        output_split_bin: split,
        ..Default::default()
    };
    chdman_wrapper::run(ChdmanCommand::ExtractCd, &args, |text, percent| {
        on_progress(text, percent);
    })
    .map_err(map_chdman_error)?;

    let siblings = if split { cue_bin_names(out_dir, stem, track_count) } else { vec![out_dir.join(format!("{stem}.bin"))] };

    Ok(ExtractedPayload { primary: cue, siblings, cleanup: destination.cleanup_dir() })
}

fn cue_bin_names(out_dir: &Path, stem: &str, track_count: usize) -> Vec<PathBuf> {
    let zero_pad = track_count >= 10;
    (1..=track_count)
        .map(|track| {
            let label = if zero_pad { format!("{track:02}") } else { track.to_string() };
            out_dir.join(format!("{stem} (Track {label}).bin"))
        })
        .collect()
}

/// Names deliberately left to chdman's own default (no `-ob` passed): GDI mode always
/// 2-digit-zero-pads the track number directly onto the stem (`{stem}01`, `{stem}02`, ...,
/// no `(Track ...)` wrapper -- `default_name += "%02t"` in `do_extract_cd`, unconditionally,
/// unlike `.cue` mode's count-dependent padding), and gives an `AUDIO`-type track a `.raw`
/// extension instead of `.bin` (also GDI-mode-specific: `.cue` mode always uses `.bin`).
/// [`gdi_track_names`] reproduces that rule using the track types `chd_info::read_chd_metadata`
/// already read, rather than re-scanning the output directory.
fn extract_chd_gdi(
    input: &Path,
    out_dir: &Path,
    stem: &str,
    tracks: &[TrackMetadata],
    destination: &ExtractDestination,
    on_progress: &mut dyn FnMut(&str, f32) -> bool,
) -> Result<ExtractedPayload, ExtractError> {
    let gdi = out_dir.join(format!("{stem}.gdi"));

    let args = ChdmanArgs {
        input: Some(input.to_path_buf()),
        output: Some(gdi.clone()),
        output_force: true,
        // GDI mode splits unconditionally either way (see this function's doc comment),
        // but pass it explicitly rather than relying on that implicit chdman behavior.
        output_split_bin: true,
        ..Default::default()
    };
    chdman_wrapper::run(ChdmanCommand::ExtractCd, &args, |text, percent| {
        on_progress(text, percent);
    })
    .map_err(map_chdman_error)?;

    let siblings = gdi_track_names(out_dir, stem, tracks);

    Ok(ExtractedPayload { primary: gdi, siblings, cleanup: destination.cleanup_dir() })
}

fn gdi_track_names(out_dir: &Path, stem: &str, tracks: &[TrackMetadata]) -> Vec<PathBuf> {
    tracks
        .iter()
        .map(|track| {
            let ext = if matches!(track.track_type, ChdTrackType::Audio) { "raw" } else { "bin" };
            out_dir.join(format!("{stem}{:02}.{ext}", track.track_number))
        })
        .collect()
}

fn extract_rvz(input: &Path, destination: &ExtractDestination, on_progress: &mut dyn FnMut(&str, f32) -> bool) -> Result<ExtractedPayload, ExtractError> {
    let primary = destination.dir().join(format!("{}.iso", file_stem_or(input, "extracted")));
    let args = DolphinConvertArgs::new(input, &primary, DiscFormat::Iso);

    dolphin_wrapper::convert(&args, on_progress).map_err(map_dolphin_error)?;

    Ok(ExtractedPayload { primary, siblings: Vec::new(), cleanup: destination.cleanup_dir() })
}

/// Extracts every regular member of a zip/7z. The first member becomes `primary`, except
/// that a `.gdi`/`.cue` descriptor wins if the archive holds one -- that's the file the rest
/// of the pipeline wants to read, and `super::detect::descriptor` already encodes the same
/// gdi-over-cue preference.
fn extract_archive(
    input: &Path,
    format: ArchiveFormat,
    destination: &ExtractDestination,
    on_progress: &mut dyn FnMut(&str, f32) -> bool,
) -> Result<ExtractedPayload, ExtractError> {
    let out_dir = destination.dir();
    let sevenzip_format = match format {
        ArchiveFormat::Zip => sevenzip_wrapper::ArchiveFormat::Zip,
        ArchiveFormat::SevenZip => sevenzip_wrapper::ArchiveFormat::SevenZ,
        other => return Err(ExtractError::NotExtractable(other)),
    };

    // Always -1.0 (see extract()'s own doc comment): 7-Zip's open step has no byte-based
    // percent to report, only the "Opening archive" message.
    let archive = SevenZipArchive::open(input, sevenzip_format, &mut *on_progress).map_err(map_sevenzip_error)?;

    let total_entries = archive.entry_count().max(1) as f32;
    let mut extracted = Vec::new();
    for index in 0..archive.entry_count() {
        let entry = archive.entry(index).map_err(map_sevenzip_error)?;
        if entry.is_dir {
            continue;
        }

        // Member paths come from the archive, so refuse anything that would escape the
        // destination directory (a "zip slip" path like `../../evil`).
        // Checked by component: `starts_with` on the joined path passed `../../evil`, since
        // `join` keeps the `..` and the comparison is lexical.
        let member = Path::new(&entry.path);
        if !member.components().all(|c| matches!(c, std::path::Component::Normal(_) | std::path::Component::CurDir)) {
            return Err(ExtractError::Failed(format!("archive member '{}' escapes the destination directory", entry.path)));
        }
        let out_path = out_dir.join(member);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).map_err(|err| ExtractError::Io(format!("failed to create {}: {err}", parent.display())))?;
        }

        // Blends this member's own percent (real, bytes-based -- see extract()'s doc
        // comment) with how many members are already done, so the overall percent moves
        // smoothly across the whole archive instead of resetting to 0 per member.
        archive
            .extract_to_file(index, &out_path, |text, percent| {
                let member_fraction = percent.max(0.0) / 100.0;
                let overall = (index as f32 + member_fraction) / total_entries * 100.0;
                on_progress(text, overall)
            })
            .map_err(map_sevenzip_error)?;
        extracted.push(out_path);
    }

    if extracted.is_empty() {
        return Err(ExtractError::Failed(format!("no regular member found in archive: {}", input.display())));
    }

    let primary_index = pick_primary_member(&extracted);
    let primary = extracted.remove(primary_index);

    Ok(ExtractedPayload { primary, siblings: extracted, cleanup: destination.cleanup_dir() })
}

fn pick_primary_member(paths: &[PathBuf]) -> usize {
    let ext_of = |path: &PathBuf| path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();

    if let Some(index) = paths.iter().position(|path| ext_of(path) == "gdi") {
        return index;
    }
    if let Some(index) = paths.iter().position(|path| ext_of(path) == "cue") {
        return index;
    }
    0
}

fn map_chdman_error(err: chdman_wrapper::ChdmanError) -> ExtractError {
    match err {
        chdman_wrapper::ChdmanError::Unavailable(message) => ExtractError::Unavailable(message),
        chdman_wrapper::ChdmanError::Cancelled { .. } => ExtractError::Cancelled,
        other => ExtractError::Failed(other.to_string()),
    }
}

fn map_dolphin_error(err: dolphin_wrapper::DolphinRvzError) -> ExtractError {
    match err {
        dolphin_wrapper::DolphinRvzError::Unavailable(message) => ExtractError::Unavailable(message),
        dolphin_wrapper::DolphinRvzError::Cancelled => ExtractError::Cancelled,
        other => ExtractError::Failed(other.to_string()),
    }
}

fn map_sevenzip_error(err: sevenzip_wrapper::SevenZipError) -> ExtractError {
    match err {
        sevenzip_wrapper::SevenZipError::Unavailable(message) => ExtractError::Unavailable(message),
        sevenzip_wrapper::SevenZipError::Cancelled => ExtractError::Cancelled,
        other => ExtractError::Failed(other.to_string()),
    }
}
