//! What one game input *is* and how its content files are produced as byte streams.
//!
//! [`InputSet::resolve`] classifies an input path (plain file, game directory, cue/gdi
//! sheet with its track files, or a zip/7z/CHD/RVZ/WIA/GCZ container) and lists every
//! on-disk file that belongs to it. [`ContentPlan::build`] then lists the *content files*
//! the input decodes to -- without decoding: a zip's members, a CHD's track files plus the
//! cue/gdi sheet `chdman extractcd` would write ([`super::chd_cd`]), a disc container's
//! ISO, a directory's files (relative paths), a sheet plus its tracks -- with their sizes,
//! so an operation can plan outputs, collisions and progress up front. [`produce`] finally
//! streams each content file, in that same order, into a per-file
//! [`super::pipeline::ChunkPipeline`] the caller supplies: one decode pass whatever the
//! number of consumers.
//!
//! Containers are decoded in-process by random access (`ChdReader`/`DiscReader` with
//! [`super::parallel::parallel_decode`], many threads, no chdman/dolphin call lock held)
//! or, for zip/7z, sequentially through `SevenZipArchive::extract_stream`. Nothing is
//! written to disk here; the pipelines' sinks decide that.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::common::models::ArchiveFormat;

use super::chd_cd::{cd_layout, CdImageReader, CdLayout, CdTrackLayout};
use super::chd_info;
use super::chdman_wrapper::{ChdReader, ChdmanError, CD_MAX_SECTOR_BYTES};
use super::decode_threads::{ChdKind, DecodeThreads};
use super::detect::sheet::{CueSheetOptions, Sheet, SheetKind};
use super::dolphin_wrapper::{DiscReader, DolphinRvzError};
use super::hash::{HashFlags, HASH_FILE_DEFAULT_BUF_SIZE};
use super::native_lib::LibVariant;
use super::parallel::{parallel_decode, Delivery};
use super::pipeline::{ChunkPipeline, PipelineError, PipelineOutcome};
use super::platforms;
use super::sevenzip_wrapper::{ArchiveFormat as NativeZipFormat, SevenZipArchive, SevenZipError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputKind {
    PlainFile,
    Directory,
    Sheet(SheetKind),
    /// A container this crate can decode in-process.
    Archive(ArchiveFormat),
}

/// An input and every on-disk file that belongs to it.
#[derive(Debug, Clone)]
pub struct InputSet {
    pub kind: InputKind,
    pub path: PathBuf,
    /// The input file itself (or every file of a directory / a sheet and its tracks).
    pub files: Vec<PathBuf>,
    pub stem: String,
    /// Sum of `files`' sizes.
    pub total_bytes: u64,
    pub sheet: Option<Sheet>,
}

/// Container formats with an in-process decoder, by extension.
pub fn archive_format_for_extension(ext: &str) -> Option<ArchiveFormat> {
    match ext.to_ascii_lowercase().as_str() {
        "zip" => Some(ArchiveFormat::Zip),
        "7z" => Some(ArchiveFormat::SevenZip),
        "chd" => Some(ArchiveFormat::Chd),
        "rvz" => Some(ArchiveFormat::Rvz),
        "wia" => Some(ArchiveFormat::Wia),
        "gcz" => Some(ArchiveFormat::Gcz),
        "cso" => Some(ArchiveFormat::Cso),
        "pbp" => Some(ArchiveFormat::Pbp),
        _ => None,
    }
}

fn file_len(path: &Path) -> Result<u64, String> {
    std::fs::metadata(path).map(|m| m.len()).map_err(|err| format!("failed to stat {}: {err}", path.display()))
}

impl InputSet {
    pub fn resolve(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Err(format!("input does not exist: {}", path.display()));
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("input").to_string();
        if path.is_dir() {
            let files = platforms::directory_payload_paths(path);
            if files.is_empty() {
                return Err(format!("directory has no files: {}", path.display()));
            }
            let total_bytes = files.iter().map(|f| file_len(f)).sum::<Result<u64, _>>()?;
            return Ok(Self { kind: InputKind::Directory, path: path.to_path_buf(), files, stem, total_bytes, sheet: None });
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if let Some(kind) = SheetKind::from_extension(ext) {
            let sheet = Sheet::parse(path)?;
            let base = path.parent().unwrap_or_else(|| Path::new("."));
            let mut files = vec![path.to_path_buf()];
            for track in sheet.payload_paths(base) {
                if !track.is_file() {
                    return Err(format!("sheet {} refers to a missing track file: {}", path.display(), track.display()));
                }
                files.push(track);
            }
            let total_bytes = files.iter().map(|f| file_len(f)).sum::<Result<u64, _>>()?;
            return Ok(Self { kind: InputKind::Sheet(kind), path: path.to_path_buf(), files, stem, total_bytes, sheet: Some(sheet) });
        }
        let kind = archive_format_for_extension(ext).map_or(InputKind::PlainFile, InputKind::Archive);
        let total_bytes = file_len(path)?;
        Ok(Self { kind, path: path.to_path_buf(), files: vec![path.to_path_buf()], stem, total_bytes, sheet: None })
    }

    /// `path` as exactly one file: a plain file, or a container this crate decodes. Unlike
    /// [`Self::resolve`] a cue/gdi is *not* expanded into its sheet set -- for callers that
    /// handle every file on disk separately (a scan records the sheet and each track as
    /// files of their own) and must not read the tracks twice.
    pub fn resolve_file(path: &Path) -> Result<Self, String> {
        if !path.is_file() {
            return Err(format!("not a file: {}", path.display()));
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("input").to_string();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let kind = archive_format_for_extension(ext).map_or(InputKind::PlainFile, InputKind::Archive);
        Ok(Self { kind, path: path.to_path_buf(), files: vec![path.to_path_buf()], stem, total_bytes: file_len(path)?, sheet: None })
    }

    pub fn is_archive(&self) -> bool {
        matches!(self.kind, InputKind::Archive(_))
    }

    pub fn archive_format(&self) -> Option<ArchiveFormat> {
        match self.kind {
            InputKind::Archive(format) => Some(format),
            _ => None,
        }
    }
}

/// One content file: what an extraction would name it (member path, track file name,
/// `{stem}.iso`...) and its decoded size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentFile {
    pub name: String,
    pub size: u64,
    /// The on-disk file this content *is* (plain/directory/sheet inputs), for consumers
    /// that can read it directly.
    pub source_path: Option<PathBuf>,
}

#[derive(Debug)]
pub enum ContentError {
    /// The streaming native build for this container isn't available.
    Unavailable(String),
    Cancelled,
    Io(String),
    Failed(String),
    Pipeline(PipelineError),
}

impl fmt::Display for ContentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(message) => write!(f, "content decoder unavailable: {message}"),
            Self::Cancelled => write!(f, "content production cancelled"),
            Self::Io(message) => write!(f, "content io error: {message}"),
            Self::Failed(message) => write!(f, "content production failed: {message}"),
            Self::Pipeline(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for ContentError {}

impl From<PipelineError> for ContentError {
    fn from(err: PipelineError) -> Self {
        Self::Pipeline(err)
    }
}

fn chd_err(err: ChdmanError) -> ContentError {
    match err {
        ChdmanError::Unavailable(message) => ContentError::Unavailable(message),
        other => ContentError::Failed(other.to_string()),
    }
}

fn dolphin_err(err: DolphinRvzError) -> ContentError {
    match err {
        DolphinRvzError::Unavailable(message) => ContentError::Unavailable(message),
        DolphinRvzError::Cancelled => ContentError::Cancelled,
        other => ContentError::Failed(other.to_string()),
    }
}

fn sevenzip_err(err: SevenZipError) -> ContentError {
    match err {
        SevenZipError::Unavailable(message) => ContentError::Unavailable(message),
        SevenZipError::Cancelled => ContentError::Cancelled,
        other => ContentError::Failed(other.to_string()),
    }
}

fn io_err(err: std::io::Error) -> ContentError {
    ContentError::Io(err.to_string())
}

/// Container-specific facts gathered by [`ContentPlan::build`], reused by [`produce`].
#[derive(Debug, Clone)]
pub enum ContainerPlan {
    None,
    Zip { format: NativeZipFormat, entries: Vec<(i32, String, u64)> },
    ChdImage { logical_bytes: u64, hunk_bytes: u32 },
    ChdCd { layout: CdLayout, sheet_bytes: Vec<u8> },
    Disc { data_size: u64 },
    /// A CSO's ISO, decoded block by block ([`super::cso::CsoReader`]).
    Cso { iso_size: u64 },
    /// Each disc of a PBP as Redump's cue and track files ([`super::pbp::disc_layout`]).
    Pbp { discs: Vec<super::pbp::DiscLayout> },
}

/// The content files an input decodes to, listed without decoding.
#[derive(Debug, Clone)]
pub struct ContentPlan {
    pub files: Vec<ContentFile>,
    pub container: ContainerPlan,
    /// CD/GD-ROM CHD (track-based) vs. anything else -- picks the decode thread preset.
    pub chd_kind: Option<ChdKind>,
    /// The CHD's own metadata (`chdman info`), when the input is a CHD.
    pub chd_metadata: Option<super::models::ChdMetadata>,
}

impl ContentPlan {
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// `cue` picks the reconstructed sheet dialect and track naming for a CD/GD-ROM CHD;
    /// `cd_sheet` which sheet kind a *GD-ROM* CHD is laid out as (Redump: `Cue`, chdman and
    /// MAME: `Gdi`); every other CD is a cue.
    ///
    /// Directory and zip/7z content is listed sheet first, then the sheet's tracks in the
    /// sheet's order, then the rest: a CHD creator reads the sheet to its end before any
    /// track, and then the tracks in sheet order, while [`produce`] streams one file at a
    /// time through bounded pipes -- any other order deadlocks the two.
    pub fn build(input: &InputSet, cue: &CueSheetOptions, cd_sheet: SheetKind) -> Result<Self, ContentError> {
        match input.kind {
            InputKind::PlainFile => Ok(Self {
                files: vec![ContentFile {
                    name: input.path.file_name().and_then(|n| n.to_str()).unwrap_or(&input.stem).to_string(),
                    size: input.total_bytes,
                    source_path: Some(input.path.clone()),
                }],
                container: ContainerPlan::None,
                chd_kind: None,
                chd_metadata: None,
            }),
            InputKind::Directory => {
                let mut files = Vec::with_capacity(input.files.len());
                for path in &input.files {
                    let name = path.strip_prefix(&input.path).unwrap_or(path).to_string_lossy().replace('\\', "/");
                    files.push(ContentFile { name, size: file_len(path).map_err(ContentError::Io)?, source_path: Some(path.clone()) });
                }
                let order = sheet_first_order(&files.iter().map(|f| f.name.clone()).collect::<Vec<_>>(), |i| std::fs::read(&input.files[i]).ok());
                let files = order.into_iter().map(|i| files[i].clone()).collect();
                Ok(Self { files, container: ContainerPlan::None, chd_kind: None, chd_metadata: None })
            }
            InputKind::Sheet(_) => {
                let mut files = Vec::with_capacity(input.files.len());
                for path in &input.files {
                    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
                    files.push(ContentFile { name, size: file_len(path).map_err(ContentError::Io)?, source_path: Some(path.clone()) });
                }
                Ok(Self { files, container: ContainerPlan::None, chd_kind: None, chd_metadata: None })
            }
            InputKind::Archive(ArchiveFormat::Zip) | InputKind::Archive(ArchiveFormat::SevenZip) => {
                let format = if input.kind == InputKind::Archive(ArchiveFormat::Zip) { NativeZipFormat::Zip } else { NativeZipFormat::SevenZ };
                let archive = SevenZipArchive::open_variant(&input.path, format, LibVariant::Streaming, |_, _| true).map_err(sevenzip_err)?;
                if !archive.supports_streaming() {
                    return Err(ContentError::Unavailable("sevenzip library is not a streaming build".to_string()));
                }
                let mut entries = Vec::new();
                let mut files = Vec::new();
                for index in 0..archive.entry_count() {
                    let entry = archive.entry(index).map_err(sevenzip_err)?;
                    if entry.is_dir {
                        continue;
                    }
                    files.push(ContentFile { name: entry.path.clone(), size: entry.size, source_path: None });
                    entries.push((index, entry.path, entry.size));
                }
                if files.is_empty() {
                    return Err(ContentError::Failed(format!("no regular member in archive: {}", input.path.display())));
                }
                let order = sheet_first_order(&files.iter().map(|f| f.name.clone()).collect::<Vec<_>>(), |i| archive.read_entry(entries[i].0).ok());
                let files = order.iter().map(|&i| files[i].clone()).collect();
                let entries = order.iter().map(|&i| entries[i].clone()).collect();
                Ok(Self { files, container: ContainerPlan::Zip { format, entries }, chd_kind: None, chd_metadata: None })
            }
            InputKind::Archive(ArchiveFormat::Chd) => {
                let reader = ChdReader::open(&input.path, None, 0).map_err(chd_err)?;
                let info = reader.info().map_err(chd_err)?;
                let metadata = chd_info::read_chd_metadata(&input.path).ok();
                if !info.is_cd || info.num_tracks == 0 {
                    let is_dvd = metadata.as_ref().is_some_and(|m| m.is_dvd);
                    let name = format!("{}.{}", input.stem, if is_dvd { "iso" } else { "raw" });
                    return Ok(Self {
                        files: vec![ContentFile { name, size: info.logical_bytes, source_path: None }],
                        container: ContainerPlan::ChdImage { logical_bytes: info.logical_bytes, hunk_bytes: info.hunk_bytes },
                        chd_kind: Some(ChdKind::DvdRaw),
                        chd_metadata: metadata,
                    });
                }
                // `cd_sheet` is the GD-ROM choice; any other CD is a cue.
                let kind = if info.is_gdrom { cd_sheet } else { SheetKind::Cue };
                let layout = cd_layout(&reader, metadata.as_ref(), &input.stem, kind, cue).map_err(chd_err)?;
                let sheet_bytes = layout.sheet_bytes(cue);
                let mut files = vec![ContentFile { name: layout.sheet_file_name.clone(), size: sheet_bytes.len() as u64, source_path: None }];
                files.extend(layout.files().into_iter().map(|(name, start, end)| ContentFile { name, size: end - start, source_path: None }));
                Ok(Self { files, container: ContainerPlan::ChdCd { layout, sheet_bytes }, chd_kind: Some(ChdKind::Cd), chd_metadata: metadata })
            }
            InputKind::Archive(ArchiveFormat::Rvz) | InputKind::Archive(ArchiveFormat::Wia) | InputKind::Archive(ArchiveFormat::Gcz) => {
                let reader = DiscReader::open(&input.path, None).map_err(dolphin_err)?;
                let info = reader.info().map_err(dolphin_err)?;
                Ok(Self {
                    files: vec![ContentFile { name: format!("{}.iso", input.stem), size: info.data_size, source_path: None }],
                    container: ContainerPlan::Disc { data_size: info.data_size },
                    chd_kind: None,
                    chd_metadata: None,
                })
            }
            InputKind::Archive(ArchiveFormat::Cso) => {
                let reader = super::cso::CsoReader::open(&input.path).map_err(|e| ContentError::Failed(format!("{}: {e}", input.path.display())))?;
                let iso_size = reader.size();
                Ok(Self {
                    files: vec![ContentFile { name: format!("{}.iso", input.stem), size: iso_size, source_path: None }],
                    container: ContainerPlan::Cso { iso_size },
                    chd_kind: None,
                    chd_metadata: None,
                })
            }
            InputKind::Archive(ArchiveFormat::Pbp) => {
                if !super::pbp::holds_ps1_disc(&input.path) {
                    return Err(ContentError::Unavailable("not a PlayStation disc PBP (a PSP game, DLC or homebrew)".into()));
                }
                if !super::pbp::available() {
                    return Err(ContentError::Unavailable("the PBP library (psxpbp) is not built for this machine".into()));
                }
                let pbp = super::pbp::Pbp::open(&input.path).map_err(ContentError::Failed)?;
                let stem = super::pbp::output_stem(&input.path);
                let discs = (0..pbp.disc_count()).map(|d| super::pbp::disc_layout(&pbp, d, &stem)).collect::<Result<Vec<_>, _>>().map_err(ContentError::Failed)?;
                let mut files = Vec::new();
                for disc in &discs {
                    files.push(ContentFile { name: disc.cue_name.clone(), size: disc.cue.len() as u64, source_path: None });
                    files.extend(disc.tracks.iter().map(|t| ContentFile { name: t.name.clone(), size: t.sectors * super::pbp::SECTOR, source_path: None }));
                }
                Ok(Self { files, container: ContainerPlan::Pbp { discs }, chd_kind: None, chd_metadata: None })
            }
            InputKind::Archive(other) => Err(ContentError::Unavailable(format!("no in-process decoder for {other}"))),
        }
    }
}

/// The order to stream `names` in: cue/gdi sheets first, each followed by the files it
/// names in its own order, then everything else as listed. `read(i)` gives file `i`'s bytes
/// (only asked for sheets).
fn sheet_first_order(names: &[String], read: impl Fn(usize) -> Option<Vec<u8>>) -> Vec<usize> {
    let mut order = Vec::with_capacity(names.len());
    let mut taken = vec![false; names.len()];
    let key = |name: &str| name.rsplit('/').next().unwrap_or(name).to_ascii_lowercase();
    for (i, name) in names.iter().enumerate() {
        let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("");
        let Some(kind) = SheetKind::from_extension(ext) else { continue };
        if taken[i] {
            continue;
        }
        taken[i] = true;
        order.push(i);
        let Some(text) = read(i).map(|b| String::from_utf8_lossy(&b).into_owned()) else { continue };
        let Ok(sheet) = Sheet::parse_text(kind, &text) else { continue };
        for track in sheet.payload_names() {
            let want = key(&track);
            if let Some(j) = (0..names.len()).find(|&j| !taken[j] && key(&names[j]) == want) {
                taken[j] = true;
                order.push(j);
            }
        }
    }
    order.extend((0..names.len()).filter(|&i| !taken[i]));
    order
}

/// Supplies and collects the per-file pipelines [`produce`] streams into.
pub trait ContentSink {
    /// Called before file `index`'s bytes.
    fn begin(&mut self, index: usize, file: &ContentFile) -> Result<ChunkPipeline, ContentError>;
    /// Called after every chunk pushed into file `index`'s pipeline, with the bytes of that
    /// file pushed so far (on the producing thread). Return `false` to cancel.
    fn tick(&mut self, index: usize, bytes_in_file: u64) -> bool;
    /// Called with the finished pipeline's outcome once file `index` ended.
    fn end(&mut self, index: usize, file: &ContentFile, outcome: PipelineOutcome) -> Result<(), ContentError>;
}

/// Streams every content file of `plan` through the pipelines `sink` hands out, in plan
/// order, on the calling thread (decode workers for CHD/RVZ run on `threads` extra threads
/// per [`DecodeThreads`]). `cancel` set from anywhere stops it with
/// [`ContentError::Cancelled`].
pub fn produce(
    input: &InputSet,
    plan: &ContentPlan,
    threads: &DecodeThreads,
    hash_flags: HashFlags,
    sink: &mut dyn ContentSink,
    cancel: &AtomicBool,
) -> Result<(), ContentError> {
    match &plan.container {
        ContainerPlan::None => produce_files(plan, sink, cancel),
        ContainerPlan::Zip { format, entries } => produce_zip(&input.path, *format, entries, plan, sink, cancel),
        ContainerPlan::ChdImage { logical_bytes, hunk_bytes } => {
            let hunk = (*hunk_bytes).max(1) as usize;
            let block = ((4usize << 20) / hunk).max(1) * hunk;
            let open = || ChdReader::open(&input.path, None, 0).map_err(|e| std::io::Error::other(e.to_string()));
            produce_single_decoded(plan, open, *logical_bytes, block, threads.chd(ChdKind::DvdRaw, hash_flags), sink, cancel)
        }
        ContainerPlan::ChdCd { layout, sheet_bytes } => produce_chd_cd(&input.path, layout, sheet_bytes, plan, threads.chd(ChdKind::Cd, hash_flags), sink, cancel),
        ContainerPlan::Disc { data_size } => {
            let open = || DiscReader::open(&input.path, None).map_err(|e| std::io::Error::other(e.to_string()));
            produce_single_decoded(plan, open, *data_size, 2usize << 20, threads.rvz(hash_flags), sink, cancel)
        }
        ContainerPlan::Cso { iso_size } => {
            let open = || super::cso::CsoReader::open(&input.path);
            produce_single_decoded(plan, open, *iso_size, 4usize << 20, threads.rvz(hash_flags), sink, cancel)
        }
        ContainerPlan::Pbp { discs } => produce_pbp(&input.path, discs, plan, sink, cancel),
    }
}

/// Each disc's cue (one chunk), then its track files split out of one in-order read of the
/// disc. The library reads one sector run at a time whatever the threads, so one decoder.
fn produce_pbp(path: &Path, discs: &[super::pbp::DiscLayout], plan: &ContentPlan, sink: &mut dyn ContentSink, cancel: &AtomicBool) -> Result<(), ContentError> {
    let pbp = super::pbp::Pbp::open(path).map_err(ContentError::Failed)?;
    let mut first = 0usize;
    for disc in discs {
        produce_bytes(first, &plan.files[first], disc.cue.as_bytes(), sink)?;
        let spans: Vec<(u64, u64)> = disc.tracks.iter().map(|t| (t.first_sector * super::pbp::SECTOR, (t.first_sector + t.sectors) * super::pbp::SECTOR)).collect();
        let open = || Ok::<_, std::io::Error>(super::pbp::DiscReader::new(&pbp, disc));
        produce_spans(open, disc.total_bytes(), super::pbp::SECTOR as usize * 256, 1, &spans, first + 1, plan, sink, cancel)?;
        first += 1 + disc.tracks.len();
    }
    Ok(())
}

/// One content file that is all in memory already (a sheet), as a single chunk.
fn produce_bytes(index: usize, file: &ContentFile, bytes: &[u8], sink: &mut dyn ContentSink) -> Result<(), ContentError> {
    let mut pipeline = sink.begin(index, file)?;
    if let Err(err) = pipeline.push(Arc::from(bytes)) {
        pipeline.abort(&err.to_string());
        return Err(err.into());
    }
    if !sink.tick(index, bytes.len() as u64) {
        pipeline.abort("cancelled");
        return Err(ContentError::Cancelled);
    }
    let outcome = pipeline.finish()?;
    sink.end(index, file, outcome)
}

fn produce_files(plan: &ContentPlan, sink: &mut dyn ContentSink, cancel: &AtomicBool) -> Result<(), ContentError> {
    let mut buf = vec![0u8; HASH_FILE_DEFAULT_BUF_SIZE];
    for (index, file) in plan.files.iter().enumerate() {
        let path = file.source_path.as_ref().ok_or_else(|| ContentError::Failed(format!("content file {} has no source path", file.name)))?;
        let mut pipeline = sink.begin(index, file)?;
        let mut reader = std::fs::File::open(path).map_err(io_err)?;
        let mut pushed = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                pipeline.abort("cancelled");
                return Err(ContentError::Cancelled);
            }
            let n = reader.read(&mut buf).map_err(io_err)?;
            if n == 0 {
                break;
            }
            if let Err(err) = pipeline.push(Arc::from(&buf[..n])) {
                pipeline.abort(&err.to_string());
                return Err(err.into());
            }
            pushed += n as u64;
            if !sink.tick(index, pushed) {
                pipeline.abort("cancelled");
                return Err(ContentError::Cancelled);
            }
        }
        let outcome = pipeline.finish()?;
        sink.end(index, file, outcome)?;
    }
    Ok(())
}

fn produce_zip(
    path: &Path,
    format: NativeZipFormat,
    entries: &[(i32, String, u64)],
    plan: &ContentPlan,
    sink: &mut dyn ContentSink,
    cancel: &AtomicBool,
) -> Result<(), ContentError> {
    let archive = SevenZipArchive::open_variant(path, format, LibVariant::Streaming, |_, _| true).map_err(sevenzip_err)?;
    for (index, (entry_index, _, _)) in entries.iter().enumerate() {
        let file = &plan.files[index];
        let mut pipeline = sink.begin(index, file)?;
        let mut failure: Option<PipelineError> = None;
        let mut pushed = 0u64;
        let result = archive.extract_stream(
            *entry_index,
            None,
            |_, _, bytes| {
                if cancel.load(Ordering::Relaxed) {
                    return false;
                }
                match pipeline.push(Arc::from(bytes)) {
                    Ok(()) => {
                        pushed += bytes.len() as u64;
                        sink.tick(index, pushed)
                    }
                    Err(err) => {
                        failure = Some(err);
                        false
                    }
                }
            },
            |_, _| !cancel.load(Ordering::Relaxed),
        );
        if let Some(err) = failure {
            pipeline.abort(&err.to_string());
            return Err(err.into());
        }
        if let Err(err) = result {
            let err = sevenzip_err(err);
            pipeline.abort(&err.to_string());
            return Err(err);
        }
        let outcome = pipeline.finish()?;
        sink.end(index, file, outcome)?;
    }
    Ok(())
}

/// One content file decoded by random access on `threads` workers, delivered in order.
fn produce_single_decoded<R, O>(
    plan: &ContentPlan,
    open: O,
    size: u64,
    block: usize,
    threads: usize,
    sink: &mut dyn ContentSink,
    cancel: &AtomicBool,
) -> Result<(), ContentError>
where
    R: super::source::ReadAt,
    O: Fn() -> std::io::Result<R> + Sync,
{
    let file = &plan.files[0];
    let mut pipeline = sink.begin(0, file)?;
    let mut failure: Option<PipelineError> = None;
    let mut on_block = |offset: u64, bytes: &[u8]| {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        match pipeline.push(Arc::from(bytes)) {
            Ok(()) => sink.tick(0, offset + bytes.len() as u64),
            Err(err) => {
                failure = Some(err);
                false
            }
        }
    };
    let completed = parallel_decode(open, size, block, threads, Delivery::InOrder(&mut on_block)).map_err(io_err);
    if let Some(err) = failure {
        pipeline.abort(&err.to_string());
        return Err(err.into());
    }
    match completed {
        Ok(true) => {}
        Ok(false) => {
            pipeline.abort("cancelled");
            return Err(ContentError::Cancelled);
        }
        Err(err) => {
            pipeline.abort(&err.to_string());
            return Err(err);
        }
    }
    let outcome = pipeline.finish()?;
    sink.end(0, file, outcome)
}

/// The sheet (one chunk), then every track file split out of one parallel decode of the
/// extracted image ([`CdImageReader`]).
fn produce_chd_cd(
    path: &Path,
    layout: &CdLayout,
    sheet_bytes: &[u8],
    plan: &ContentPlan,
    threads: usize,
    sink: &mut dyn ContentSink,
    cancel: &AtomicBool,
) -> Result<(), ContentError> {
    produce_bytes(0, &plan.files[0], sheet_bytes, sink)?;

    // Files 1..: the track files, contiguous ranges of the extracted image.
    let spans: Vec<(u64, u64)> = layout.files().into_iter().map(|(_, start, end)| (start, end)).collect();
    let tracks: Arc<[CdTrackLayout]> = layout.tracks.clone().into();
    let open = || ChdReader::open(path, None, 0).map(|reader| CdImageReader::new(reader, Arc::clone(&tracks))).map_err(|e| std::io::Error::other(e.to_string()));
    produce_spans(open, layout.total_bytes, CD_MAX_SECTOR_BYTES * 1024, threads, &spans, 1, plan, sink, cancel)
}

/// Content files `first..` as the contiguous byte ranges `spans` of one image, decoded once by
/// random access on `threads` workers and delivered in order.
#[allow(clippy::too_many_arguments)]
fn produce_spans<R, O>(
    open: O,
    total: u64,
    block: usize,
    threads: usize,
    spans: &[(u64, u64)],
    first: usize,
    plan: &ContentPlan,
    sink: &mut dyn ContentSink,
    cancel: &AtomicBool,
) -> Result<(), ContentError>
where
    R: super::source::ReadAt,
    O: Fn() -> std::io::Result<R> + Sync,
{
    let mut span_index = 0usize;
    let mut current: Option<ChunkPipeline> = None;
    let mut failure: Option<ContentError> = None;
    let mut on_block = |offset: u64, bytes: &[u8]| {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let mut pos = offset;
        let mut rest = bytes;
        while !rest.is_empty() && span_index < spans.len() {
            let (start, end) = spans[span_index];
            if current.is_none() {
                debug_assert_eq!(pos, start);
                match sink.begin(first + span_index, &plan.files[first + span_index]) {
                    Ok(pipeline) => current = Some(pipeline),
                    Err(err) => {
                        failure = Some(err);
                        return false;
                    }
                }
            }
            let take = (end - pos).min(rest.len() as u64) as usize;
            let pipeline = current.as_mut().expect("pipeline begun above");
            if let Err(err) = pipeline.push(Arc::from(&rest[..take])) {
                failure = Some(err.into());
                return false;
            }
            pos += take as u64;
            rest = &rest[take..];
            if !sink.tick(first + span_index, pos - start) {
                return false;
            }
            if pos >= end {
                let pipeline = current.take().expect("pipeline begun above");
                let outcome = match pipeline.finish() {
                    Ok(outcome) => outcome,
                    Err(err) => {
                        failure = Some(err.into());
                        return false;
                    }
                };
                if let Err(err) = sink.end(first + span_index, &plan.files[first + span_index], outcome) {
                    failure = Some(err);
                    return false;
                }
                span_index += 1;
            }
        }
        true
    };
    let completed = parallel_decode(open, total, block, threads, Delivery::InOrder(&mut on_block)).map_err(io_err);
    if let Some(pipeline) = current.take() {
        pipeline.abort("decode ended early");
    }
    if let Some(err) = failure {
        return Err(err);
    }
    match completed {
        Ok(true) => Ok(()),
        Ok(false) => Err(ContentError::Cancelled),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::sheet_first_order;

    #[test]
    fn sheets_come_first_and_pull_their_tracks_in_sheet_order() {
        let names: Vec<String> = ["Game (Track 2).bin", "readme.txt", "Game (Track 1).bin", "Game.cue"].iter().map(|s| s.to_string()).collect();
        let cue = b"FILE \"Game (Track 1).bin\" BINARY\r\n  TRACK 01 MODE1/2352\r\n    INDEX 01 00:00:00\r\nFILE \"Game (Track 2).bin\" BINARY\r\n  TRACK 02 AUDIO\r\n    INDEX 01 00:00:00\r\n".to_vec();
        let order = sheet_first_order(&names, |i| (i == 3).then(|| cue.clone()));
        assert_eq!(order, vec![3, 2, 0, 1]);
    }

    #[test]
    fn no_sheet_keeps_the_listed_order() {
        let names: Vec<String> = ["b.bin", "a.bin"].iter().map(|s| s.to_string()).collect();
        assert_eq!(sheet_first_order(&names, |_| None), vec![0, 1]);
    }
}
