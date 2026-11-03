//! Batch scheduling: how many files a job processes at the same time, and (for CHD/RVZ)
//! how many decode threads each one gets. This is for genuine batches -- a library scan, a
//! bulk convert/archive/hash run -- deciding how hard to drive the machine when there's a
//! queue of work. It is not for `FileData`'s own hashing (`common::models::FileData::hash`/
//! `integrity`): that runs one file at a time for an integrity check or a scheduled full
//! check, which should stay low-load by design, not opt into this module's throughput-
//! oriented scheduling.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::common::models::{build_report_from_members, record_from_report, ArchiveFormat, FileData, Platform};

use super::archive::{self, ArchiveError, ArchiveOptions, ArchiveTarget};
use super::chdman_wrapper::{ChdReader, ChdmanCommand};
use super::content::{produce, ContainerPlan, ContentError, ContentFile, ContentPlan, ContentSink, InputKind, InputSet};
use super::convert::{converter_for, ConvertError, StreamConverter};
use super::decode_threads::DecodeThreads;
use super::detect::{
    self,
    sheet::{CueSheetOptions, SheetKind},
    HeuristicFlags,
};
use super::dolphin_wrapper::DiscReader;
use super::hash::{FileHashRecord, FileSizeClass, HashDigest, HashFlags, HashReport, RamClass, StorageKind};
use super::models::{ArchiveArgs, ConvertArgs, MetadataArgs, RomMetadata};
use super::pipeline::{ChunkPipeline, ChunkSink, PipelineOutcome};
use super::platforms::helpers::{is_chd_platform, is_rvz_platform};
use super::source::{pipe, Source};


/// Other work a job does on the same file besides hashing. `extract` runs before the hash;
/// `convert` and `archive` run before or after it. All three add disk and CPU load.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct JobStages {
    pub extract: bool,
    pub convert: bool,
    pub archive: bool,
}

impl JobStages {
    /// Archiving compresses (heavy, and its own worker threads), so it counts double.
    fn weight(self) -> u32 {
        self.extract as u32 + self.convert as u32 + 2 * self.archive as u32
    }
}

/// Threads one hashing job keeps busy: the reader plus one worker per algorithm (CRC32 and
/// XXH3-128 share a worker when both are requested, and RAHASH runs on its own thread).
fn threads_per_file(flags: HashFlags) -> usize {
    let flags = if flags.is_empty() { HashFlags::default() } else { flags };
    1 + (flags.intersects(HashFlags::CRC32 | HashFlags::XXH3_128) as usize)
        + flags.contains(HashFlags::MD5) as usize
        + flags.contains(HashFlags::SHA1) as usize
        + flags.contains(HashFlags::SHA256) as usize
        + flags.contains(HashFlags::RAHASH) as usize
}

/// Rounds `n` down to the nearest power of two (`0` and `1` both give `1`).
fn round_down_pow2(n: usize) -> usize {
    if n <= 1 {
        1
    } else {
        1 << n.ilog2()
    }
}

/// A recommended number of files to process at the same time, rounded down to a power of
/// two (1, 2, 4, 8, 16, ...).
///
/// Each factor gives an upper limit and the smallest one wins:
/// - **storage and file size**: a spinning disk wants one big sequential read at a time
///   (two for small files); SSD and network allow 4 large files, and don't limit small
///   files at all (a large batch of small reads is cheap on both -- `cpu_cap` below is what
///   actually governs how many run at once);
/// - **RAM**: Small allows 2 large files (4 small); Medium/Large don't limit large files
///   past 8, or small files at all (a small file's own read buffer is tiny -- see
///   `hash::recommended_buf_size`'s own size-based scaling -- so RAM isn't the constraint
///   there either);
/// - **CPU**: `cpu_cores` divided by the threads one file needs (see `flags`) -- for small
///   files with no other stages and fast-enough storage/RAM, this ends up being the only
///   real limit, so e.g. a 64-core machine hashing CRC32-only small files can recommend
///   32, not an arbitrary ceiling;
/// - **other stages**: none doesn't limit large files past 8 (small files at all), a stage
///   weight of 1 allows 4, 2 allows 2, 3 or more allows 1 (extract and convert weigh 1
///   each, archive 2 -- see [`JobStages::weight`]).
///
/// None of this raises what a *large* file with any other stage active, or on HDD/tight
/// RAM, was already getting: those factors' own caps never exceeded 4 to begin with, so
/// they were never the ones being clamped to 8 by the old fixed 1/2/4/8 rounding.
///
/// `cpu_cores` of `0` counts as `1`. Empty `flags` count as [`HashFlags::default`], as in
/// `hash::hash`.
pub fn recommended_parallel_files(
    size: FileSizeClass,
    storage: StorageKind,
    ram: RamClass,
    cpu_cores: usize,
    flags: HashFlags,
    stages: JobStages,
) -> usize {
    let storage_cap = match (storage, size) {
        (StorageKind::Hdd, FileSizeClass::Small) => 2,
        (StorageKind::Hdd, FileSizeClass::Large) => 1,
        (_, FileSizeClass::Small) => usize::MAX,
        (_, FileSizeClass::Large) => 4,
    };
    let ram_cap = match (ram, size) {
        (RamClass::Small, FileSizeClass::Small) => 4,
        (RamClass::Small, FileSizeClass::Large) => 2,
        (RamClass::Medium, FileSizeClass::Small) => usize::MAX,
        (RamClass::Medium, FileSizeClass::Large) => 4,
        (RamClass::Large, FileSizeClass::Small) => usize::MAX,
        (RamClass::Large, FileSizeClass::Large) => 8,
    };
    let cpu_cap = cpu_cores.max(1) / threads_per_file(flags);
    let stage_cap = match stages.weight() {
        0 if size == FileSizeClass::Small => usize::MAX,
        0 => 8,
        1 => 4,
        2 => 2,
        _ => 1,
    };

    round_down_pow2(storage_cap.min(ram_cap).min(cpu_cap).min(stage_cap))
}

/// A CHD/RVZ container's size class -- distinct from [`FileSizeClass`]'s 2-tier
/// Small/Large, since a container runs far bigger than an ordinary ROM file.
/// `api::parallel` uses [`Self::SMALL_MAX_BYTES`] as its medium tier's upper bound.
///
/// Thresholds are a fresh judgment call, not sourced from profiling data --
/// **`< 1 GiB` -> `Small`** (PS1-scale CHDs), **`1..8 GiB` -> `Medium`** (PS2/GameCube-
/// scale), **`>= 8 GiB` -> `Large`** (multi-layer Wii/PS3-scale) -- reasonable, easy to
/// retune later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContainerSizeClass {
    Small,
    Medium,
    Large,
}

impl ContainerSizeClass {
    pub const SMALL_MAX_BYTES: u64 = 1024 * 1024 * 1024;
    pub const MEDIUM_MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;

    pub fn of(len: u64) -> Self {
        if len < Self::SMALL_MAX_BYTES {
            Self::Small
        } else if len < Self::MEDIUM_MAX_BYTES {
            Self::Medium
        } else {
            Self::Large
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZES: [FileSizeClass; 2] = [FileSizeClass::Small, FileSizeClass::Large];
    const STORAGE: [StorageKind; 3] = [StorageKind::Hdd, StorageKind::Ssd, StorageKind::Network];
    const RAM: [RamClass; 3] = [RamClass::Small, RamClass::Medium, RamClass::Large];
    const CORES: [usize; 6] = [1, 2, 4, 8, 16, 64];

    fn stage_combos() -> Vec<JobStages> {
        (0..8).map(|bits| JobStages { extract: bits & 1 != 0, convert: bits & 2 != 0, archive: bits & 4 != 0 }).collect()
    }

    fn flag_sets() -> Vec<HashFlags> {
        vec![
            HashFlags::CRC32 | HashFlags::XXH3_128,
            HashFlags::default(),
            HashFlags::MD5 | HashFlags::SHA1 | HashFlags::SHA256,
            HashFlags::MD5 | HashFlags::SHA1 | HashFlags::SHA256 | HashFlags::RAHASH,
        ]
    }

    #[test]
    fn always_a_power_of_two() {
        for size in SIZES {
            for storage in STORAGE {
                for ram in RAM {
                    for cores in CORES {
                        for flags in flag_sets() {
                            for stages in stage_combos() {
                                let n = recommended_parallel_files(size, storage, ram, cores, flags, stages);
                                assert!(n >= 1 && n.is_power_of_two(), "{size:?} {storage:?} {ram:?} {cores} {flags:?} {stages:?} -> {n}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn large_files_or_any_active_stage_still_cap_at_eight() {
        // Every factor that can still bind above 4 for a Large file, or for any file with a
        // stage active, tops out at 8 -- unchanged by the small-file-only uncapping below.
        for size in SIZES {
            for storage in STORAGE {
                for ram in RAM {
                    for cores in CORES {
                        for flags in flag_sets() {
                            for stages in stage_combos() {
                                if size == FileSizeClass::Large || stages.weight() > 0 {
                                    let n = recommended_parallel_files(size, storage, ram, cores, flags, stages);
                                    assert!(n <= 8, "{size:?} {storage:?} {ram:?} {cores} {flags:?} {stages:?} -> {n}");
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn small_files_no_stages_fast_storage_can_exceed_eight() {
        // Plain hashing (no extract/convert/archive) of small files on SSD/network with
        // generous RAM is governed by CPU/hash-thread count alone, not an arbitrary ceiling.
        let n = recommended_parallel_files(FileSizeClass::Small, StorageKind::Ssd, RamClass::Large, 64, HashFlags::CRC32, JobStages::default());
        assert_eq!(n, 32); // 64 physical cores / 2 threads-per-file (reader + CRC32 worker)

        // Hdd and tight RAM stay real, binding constraints even for small files.
        assert_eq!(recommended_parallel_files(FileSizeClass::Small, StorageKind::Hdd, RamClass::Large, 64, HashFlags::CRC32, JobStages::default()), 2);
        assert_eq!(recommended_parallel_files(FileSizeClass::Small, StorageKind::Ssd, RamClass::Small, 64, HashFlags::CRC32, JobStages::default()), 4);
    }

    #[test]
    fn spot_checks() {
        let none = JobStages::default();
        let default_flags = HashFlags::default();
        let rp = recommended_parallel_files;

        // One spinning disk, big files: one at a time.
        assert_eq!(rp(FileSizeClass::Large, StorageKind::Hdd, RamClass::Large, 16, default_flags, none), 1);
        // Default flags need 4 threads a file, so 16 cores allow 4.
        assert_eq!(rp(FileSizeClass::Small, StorageKind::Ssd, RamClass::Large, 16, default_flags, none), 4);
        // CRC32 + XXH3 only share one worker: 2 threads a file, so 16 cores allow 8.
        assert_eq!(rp(FileSizeClass::Small, StorageKind::Ssd, RamClass::Large, 16, HashFlags::CRC32 | HashFlags::XXH3_128, none), 8);
        // Archiving weighs 2, so at most 2 at once.
        let archive = JobStages { archive: true, ..Default::default() };
        assert_eq!(rp(FileSizeClass::Large, StorageKind::Ssd, RamClass::Large, 16, HashFlags::MD5 | HashFlags::SHA1 | HashFlags::SHA256, archive), 2);
        // 4 cores / 4 threads = 1.
        assert_eq!(rp(FileSizeClass::Large, StorageKind::Network, RamClass::Medium, 4, default_flags, none), 1);
        // Small RAM allows 2 large files.
        let extract = JobStages { extract: true, ..Default::default() };
        assert_eq!(rp(FileSizeClass::Large, StorageKind::Ssd, RamClass::Small, 32, default_flags, extract), 2);
        // Extract + convert + archive weighs 4: one at a time.
        let all = JobStages { extract: true, convert: true, archive: true };
        assert_eq!(rp(FileSizeClass::Small, StorageKind::Ssd, RamClass::Large, 64, HashFlags::CRC32, all), 1);
    }

    #[test]
    fn more_cores_never_lower_the_limit() {
        for size in SIZES {
            for storage in STORAGE {
                for ram in RAM {
                    for flags in flag_sets() {
                        for stages in stage_combos() {
                            let results: Vec<usize> = CORES.iter().map(|&c| recommended_parallel_files(size, storage, ram, c, flags, stages)).collect();
                            assert!(results.windows(2).all(|pair| pair[1] >= pair[0]), "{size:?} {storage:?} {ram:?} {flags:?} {stages:?}: {results:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn adding_a_stage_never_raises_the_limit() {
        for size in SIZES {
            for storage in STORAGE {
                for ram in RAM {
                    for flags in flag_sets() {
                        let base = JobStages::default();
                        let baseline = recommended_parallel_files(size, storage, ram, 16, flags, base);
                        for stages in [JobStages { extract: true, ..base }, JobStages { convert: true, ..base }, JobStages { archive: true, ..base }] {
                            assert!(recommended_parallel_files(size, storage, ram, 16, flags, stages) <= baseline);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn adding_a_hash_never_raises_the_limit() {
        for cores in CORES {
            let mut previous = usize::MAX;
            for flags in [HashFlags::CRC32, HashFlags::CRC32 | HashFlags::MD5, HashFlags::CRC32 | HashFlags::MD5 | HashFlags::SHA1, HashFlags::CRC32 | HashFlags::MD5 | HashFlags::SHA1 | HashFlags::SHA256, HashFlags::all()] {
                let n = recommended_parallel_files(FileSizeClass::Small, StorageKind::Ssd, RamClass::Large, cores, flags, JobStages::default());
                assert!(n <= previous, "{cores} cores {flags:?}: {n} > {previous}");
                previous = n;
            }
        }
    }

    #[test]
    fn zero_cores_counts_as_one() {
        for flags in flag_sets() {
            assert_eq!(
                recommended_parallel_files(FileSizeClass::Small, StorageKind::Ssd, RamClass::Large, 0, flags, JobStages::default()),
                recommended_parallel_files(FileSizeClass::Small, StorageKind::Ssd, RamClass::Large, 1, flags, JobStages::default())
            );
        }
    }

    #[test]
    fn container_size_class_boundaries() {
        assert_eq!(ContainerSizeClass::of(0), ContainerSizeClass::Small);
        assert_eq!(ContainerSizeClass::of(ContainerSizeClass::SMALL_MAX_BYTES - 1), ContainerSizeClass::Small);
        assert_eq!(ContainerSizeClass::of(ContainerSizeClass::SMALL_MAX_BYTES), ContainerSizeClass::Medium);
        assert_eq!(ContainerSizeClass::of(ContainerSizeClass::MEDIUM_MAX_BYTES - 1), ContainerSizeClass::Medium);
        assert_eq!(ContainerSizeClass::of(ContainerSizeClass::MEDIUM_MAX_BYTES), ContainerSizeClass::Large);
    }
}


// ===========================================================================
// Single: one game (a file, a game directory, a cue/gdi sheet with its tracks, or a
// zip/7z/CHD/RVZ/WIA/GCZ container) through detect -> extract -> hash -> convert -> archive
// in ONE pass over its bytes. Detection always runs first (platform-driven decisions need
// it); every other stage is switchable, so the same struct serves an initial full scan
// and a later scheduled or occasional run. Which files to process is a higher layer's job.
//
// Data flow: `content::produce` streams each content file (a member, a track, the ISO,
// ...) into a `pipeline::ChunkPipeline` whose input digest hashes it, whose optional
// converter rewrites it, and whose sinks write it to the output directory and/or into a
// pipe that an archive creator (`archive::archive_from_sources`, on its own thread) reads
// as a `Source`. RVZ/GCZ/WIA creation is the one exception: Dolphin pulls a random-access
// source itself, and its in-order input callback is where the same pipeline is fed.
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    Detect,
    Extract,
    Hash,
    Convert,
    Archive,
}

impl Stage {
    const ALL: [Stage; 5] = [Stage::Detect, Stage::Extract, Stage::Hash, Stage::Convert, Stage::Archive];

    fn index(self) -> usize {
        match self {
            Self::Detect => 0,
            Self::Extract => 1,
            Self::Hash => 2,
            Self::Convert => 3,
            Self::Archive => 4,
        }
    }
}

/// Which stages an operation performs besides detection (always on).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct StageSwitches {
    pub extract: bool,
    pub hash: bool,
    pub convert: bool,
    pub archive: bool,
}

impl StageSwitches {
    pub const ALL: Self = Self { extract: true, hash: true, convert: true, archive: true };
    /// Detection/metadata only.
    pub const NONE: Self = Self { extract: false, hash: false, convert: false, archive: false };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// A stage this one depends on failed.
    DependencyFailed(Stage),
    /// Nothing to do for this input (a plain file has nothing to extract, the platform has
    /// no conversion, the input is already in the target form, ...).
    NotApplicable(String),
    /// Detection left the platform unknown, so a platform-driven stage can't decide.
    PlatformUnknown,
    /// The streaming native build this stage needs isn't available.
    Unavailable(String),
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DependencyFailed(stage) => write!(f, "{stage:?} failed"),
            Self::NotApplicable(message) => write!(f, "not applicable: {message}"),
            Self::PlatformUnknown => write!(f, "platform unknown"),
            Self::Unavailable(message) => write!(f, "unavailable: {message}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum StageResult {
    #[default]
    Off,
    Skipped(SkipReason),
    Ok,
    Err(String),
}

impl StageResult {
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok)
    }

    pub fn is_err(&self) -> bool {
        matches!(self, Self::Err(_))
    }

    /// On: planned to run (neither off nor skipped).
    fn is_active(&self) -> bool {
        matches!(self, Self::Ok | Self::Err(_))
    }
}

#[derive(Debug, Clone, Default)]
pub struct StageResults {
    pub detect: StageResult,
    pub extract: StageResult,
    pub hash: StageResult,
    pub convert: StageResult,
    pub archive: StageResult,
}

impl StageResults {
    pub fn get(&self, stage: Stage) -> &StageResult {
        match stage {
            Stage::Detect => &self.detect,
            Stage::Extract => &self.extract,
            Stage::Hash => &self.hash,
            Stage::Convert => &self.convert,
            Stage::Archive => &self.archive,
        }
    }

    fn get_mut(&mut self, stage: Stage) -> &mut StageResult {
        match stage {
            Stage::Detect => &mut self.detect,
            Stage::Extract => &mut self.extract,
            Stage::Hash => &mut self.hash,
            Stage::Convert => &mut self.convert,
            Stage::Archive => &mut self.archive,
        }
    }

    /// Marks every active stage after `failed` (in dependency order) as skipped because of it.
    fn fail(&mut self, failed: Stage, message: String, dependents: &[Stage]) {
        *self.get_mut(failed) = StageResult::Err(message);
        for &stage in dependents {
            if self.get(stage).is_active() {
                *self.get_mut(stage) = StageResult::Skipped(SkipReason::DependencyFailed(failed));
            }
        }
    }
}

/// One progress report. `stage_fraction` is that stage's own 0..=1; `total_fraction` blends
/// every planned stage by its expected cost (bytes it moves times a per-stage factor, see
/// [`CostModel`]), so it moves at a steady pace through the whole operation.
#[derive(Debug, Clone, Copy)]
pub struct ProgressEvent<'a> {
    pub stage: Stage,
    pub stage_fraction: f32,
    pub total_fraction: f32,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub message: Option<&'a str>,
}

/// Relative cost of one byte in each stage, for blending progress. Rough, deliberately: a
/// decode, a hash, an in-stream header rewrite and a compression are not the same work per
/// byte, and the blend only has to move smoothly, not be exact.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CostModel {
    /// Fixed cost of detection, in byte-equivalents.
    pub detect_bytes: u64,
    /// Per decoded byte for each container kind.
    pub extract_chd: f64,
    pub extract_disc: f64,
    pub extract_zip: f64,
    /// Per byte read from a plain file/directory/sheet (no decoding).
    pub read_plain: f64,
    /// Per hashed byte, per hash worker thread (`HashDigest::worker_thread_count`).
    pub hash_per_thread: f64,
    /// Per converted byte (streaming kernels / whole-file buffered conversions).
    pub convert_streaming: f64,
    pub convert_buffered: f64,
    /// Per byte fed to each archive creator.
    pub archive_chd: f64,
    pub archive_disc: f64,
    pub archive_7z: f64,
    pub archive_zip: f64,
}

impl Default for CostModel {
    fn default() -> Self {
        Self {
            detect_bytes: 4 << 20,
            extract_chd: 1.0,
            extract_disc: 1.0,
            extract_zip: 0.8,
            read_plain: 0.2,
            hash_per_thread: 0.15,
            convert_streaming: 0.05,
            convert_buffered: 0.3,
            archive_chd: 2.0,
            archive_disc: 2.0,
            archive_7z: 2.0,
            archive_zip: 1.2,
        }
    }
}

/// How a [`Single`] decides the platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Detect {
    /// Extension and path heuristics, then the deep header cycle (`MetadataArgs`).
    #[default]
    Full,
    /// Take this platform as given and read no headers -- for a caller that already decided
    /// (a scan's light guess). `Platform::Unknown` turns detection off: stages that need a
    /// platform skip, hashing and zip/7z do not.
    Trust(Platform),
    /// A platform only when its signature check is positive ([`detect::confirm_platform`]):
    /// the hint first (a directory's platform, a folder name), then each platform the
    /// extension allows. Nothing confirmed is `Platform::Unknown` -- no guess stands in for
    /// a check, and a DAT hash match identifies the file later. A zip/7z takes the hint as
    /// given: its members are hashed and matched on their own.
    Verify(Option<Platform>),
}

/// Reading back what a [`Single`] wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputCheck {
    /// The written files' own bytes, hashed with these.
    pub flags: HashFlags,
    /// Also decode a created archive and hash its members off that decode, running alongside
    /// the raw read rather than after it.
    pub members: bool,
}

/// One written file, read back.
#[derive(Debug, Clone)]
pub struct CheckedOutput {
    pub path: PathBuf,
    /// Hashes of the file's own bytes -- the identity a scan of it would record.
    pub container: Result<HashReport, String>,
    /// Its members decoded from disk (`OutputCheck::members`, archives only).
    pub members: Option<Result<Vec<FileHashRecord>, String>>,
}

/// Everything a [`Single`] needs besides its input path.
#[derive(Debug, Clone)]
pub struct SingleArgs {
    pub metadata: MetadataArgs,
    pub stages: StageSwitches,
    pub hash_flags: HashFlags,
    pub convert: ConvertArgs,
    pub archive: ArchiveArgs,
    /// Where outputs go: created archives, extracted content (only when no archive is
    /// created), converted files. `None`: nothing is written to disk at all.
    pub output_dir: Option<PathBuf>,
    /// Scratch directory for the native libraries' own temporary files (Dolphin's user
    /// directory). `None`: their defaults.
    pub temp_dir: Option<PathBuf>,
    /// Dialect, line endings and track naming of a cue/gdi sheet reconstructed for a
    /// CD/GD-ROM CHD (`CueSheetOptions::chdman()` to conform to chdman, the Redump default
    /// for database matching).
    pub cue_sheet: CueSheetOptions,
    /// Sheet kind a GD-ROM CHD is laid out as when its content is extracted/archived/hashed
    /// (any other CD is a cue). [`Self::with_sheet_style`] sets it with `cue_sheet`.
    pub cd_sheet: SheetKind,
    /// Decode threads per container; `DecodeThreads::recommended()` by default.
    pub decode_threads: DecodeThreads,
    /// Compression threads for the archive creators. `None`: each library's own default.
    pub max_workers: Option<usize>,
    pub cost_model: CostModel,
    pub detect: Detect,
    /// Take the input path as exactly one file (`InputSet::resolve_file`): a cue/gdi is not
    /// expanded into its tracks. For callers that handle every file on disk separately.
    pub whole_file: bool,
    /// Also hash the input file's own bytes (an archive's container, not its members) with
    /// these. Runs on its own thread alongside the decode -- a CHD/RVZ decode reads by random
    /// access, so the raw bytes cannot be teed off it, but the two reads overlap -- and is
    /// derived from the content hashes instead when the input is a plain file hashed anyway.
    pub container_hash: Option<HashFlags>,
    /// Read every written file back once it is complete.
    pub output_check: Option<OutputCheck>,
    /// The created archive's file name stem; `None`: the input's. For a caller that runs on a
    /// stand-in input (a sheet it generated for a lone track) but names the output after the
    /// real one.
    pub archive_stem: Option<String>,
}

impl SingleArgs {
    /// The cue dialect, line endings and GD-ROM sheet kind of one `CueSheetStyle` (the app's
    /// `sheet_style` setting).
    pub fn with_sheet_style(mut self, style: crate::common::models::CueSheetStyle) -> Self {
        self.cue_sheet = CueSheetOptions::for_style(style);
        self.cd_sheet = self.cue_sheet.gdrom_sheet();
        self
    }
}

impl Default for SingleArgs {
    fn default() -> Self {
        Self {
            metadata: MetadataArgs::default(),
            stages: StageSwitches::default(),
            hash_flags: HashFlags::default(),
            convert: ConvertArgs::default(),
            archive: ArchiveArgs::default(),
            output_dir: None,
            temp_dir: None,
            cue_sheet: CueSheetOptions::default(),
            cd_sheet: SheetKind::Cue,
            decode_threads: DecodeThreads::recommended(),
            max_workers: None,
            cost_model: CostModel::default(),
            detect: Detect::Full,
            whole_file: false,
            container_hash: None,
            output_check: None,
            archive_stem: None,
        }
    }
}

#[derive(Debug)]
pub enum SingleError {
    /// The input doesn't exist, or a sheet's track file is missing, or a directory is empty.
    Input(String),
    /// A planned output (or intermediate) path is one of the input's own files.
    OutputCollidesWithInput(PathBuf),
    /// A planned output file already exists.
    OutputExists(PathBuf),
    InvalidArgs(String),
    Cancelled,
}

impl std::fmt::Display for SingleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input(message) => write!(f, "input error: {message}"),
            Self::OutputCollidesWithInput(path) => write!(f, "output would overwrite an input file: {}", path.display()),
            Self::OutputExists(path) => write!(f, "output already exists: {}", path.display()),
            Self::InvalidArgs(message) => write!(f, "invalid arguments: {message}"),
            Self::Cancelled => write!(f, "operation cancelled"),
        }
    }
}

impl std::error::Error for SingleError {}

/// What a [`Single`] run produced.
#[derive(Debug)]
pub struct SingleOutcome {
    pub input: InputSet,
    pub metadata: RomMetadata,
    /// The candidate platforms still possible when detection didn't settle on one.
    pub remaining_candidates: Vec<Platform>,
    pub stages: StageResults,
    /// Hashes of the input's content: one record per content file (member path, track
    /// file, ISO, or the plain file itself); top-level fields mirror a single record.
    pub input_hashes: Option<HashReport>,
    /// Hashes of the converted output, when a conversion ran.
    pub converted_hashes: Option<HashReport>,
    /// Every file written (extracted/converted files, the created archive), each with the
    /// hash report tapped off the stream that produced it.
    pub outputs: Vec<FileData>,
    pub archive_target: Option<ArchiveTarget>,
    /// The input file's own bytes hashed (`SingleArgs::container_hash`).
    pub input_container: Option<Result<HashReport, String>>,
    /// Every written file read back (`SingleArgs::output_check`).
    pub checked_outputs: Vec<CheckedOutput>,
    /// The content files, in the order streamed: `input_hashes.content_hashes[i]` is the
    /// record of the one whose name matches (an on-disk file of a plain/sheet/directory
    /// input carries its `source_path`).
    pub content_files: Vec<ContentFile>,
    pub elapsed: Duration,
}

/// One game's operation. See the section comment above.
pub struct Single {
    input: PathBuf,
    args: SingleArgs,
}

/// The resolved plan for a run: what is on, what is skipped and why, and every output.
struct Plan {
    input: InputSet,
    metadata: RomMetadata,
    remaining_candidates: Vec<Platform>,
    stages: StageResults,
    content: Option<ContentPlan>,
    archive_target: Option<ArchiveTarget>,
    archive_output: Option<PathBuf>,
    archive_options: Option<ArchiveOptions>,
    chd_command: Option<ChdmanCommand>,
    /// `Source::name` of the primary input for a CHD (the sheet or the single image).
    primary_name: Option<String>,
    /// Where extracted (or converted) content is written, when it is.
    write_dir: Option<PathBuf>,
    converter: Option<StreamConverter>,
    /// Name (and size) of the converted content file, replacing the single content file's.
    converted: Option<(String, u64)>,
    planned_outputs: Vec<PathBuf>,
    weights: [f64; 5],
    totals: [u64; 5],
}

fn normalize(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => std::fs::canonicalize(parent).map(|p| p.join(name)).unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

fn extension_of(name: &str) -> String {
    Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

fn target_format(target: ArchiveTarget) -> ArchiveFormat {
    match target {
        ArchiveTarget::Chd => ArchiveFormat::Chd,
        ArchiveTarget::Rvz => ArchiveFormat::Rvz,
        ArchiveTarget::Gcz => ArchiveFormat::Gcz,
        ArchiveTarget::Wia => ArchiveFormat::Wia,
        ArchiveTarget::Zip => ArchiveFormat::Zip,
        ArchiveTarget::SevenZip => ArchiveFormat::SevenZip,
    }
}

impl Single {
    pub fn new(input: impl Into<PathBuf>, args: SingleArgs) -> Self {
        Self { input: input.into(), args }
    }

    pub fn input(&self) -> &Path {
        &self.input
    }

    pub fn args(&self) -> &SingleArgs {
        &self.args
    }

    /// Dry run: validates the arguments, detects the platform and lists every file the
    /// run would write (the created archive, extracted or converted files), with the same
    /// collision/exists checks [`Self::run`] applies -- without producing anything.
    pub fn planned_outputs(&self) -> Result<Vec<PathBuf>, SingleError> {
        self.plan().map(|plan| plan.planned_outputs)
    }

    /// Detection (mode 1: forced; otherwise extension -> path heuristics, each narrowing the
    /// candidates, then `detect::metadata_cycle` over what's left -- with a plausible
    /// `preferred_platform` hint tried first, see `detect::hinted_metadata_cycle`). A cycle
    /// that ends `Unknown` with exactly one candidate left settles on that candidate, as the
    /// heuristic answer (the same fallback `detect::read_metadata_for_input` makes).
    fn detect(&self) -> (RomMetadata, Vec<Platform>, StageResult) {
        if let Detect::Trust(platform) = self.args.detect {
            let known = platform != Platform::Unknown;
            let metadata = RomMetadata {
                platform,
                candidates: if known { vec![platform] } else { Vec::new() },
                file_size: std::fs::metadata(&self.input).ok().map(|m| m.len()),
                ..Default::default()
            };
            let result = if known { StageResult::Ok } else { StageResult::Skipped(SkipReason::NotApplicable("detection off".to_string())) };
            return (metadata, Vec::new(), result);
        }
        if let Detect::Verify(hint) = self.args.detect {
            return self.verify(hint);
        }
        let meta_args = &self.args.metadata;
        let flags = HeuristicFlags::new(meta_args.extension_heuristic, meta_args.path_heuristic);
        if let Some(forced) = meta_args.forced_platform {
            let metadata = detect::read_metadata_for_input(&self.input, Some(forced), flags.ext, flags.path);
            return (metadata, vec![forced], StageResult::Ok);
        }
        let outcome = detect::hinted_metadata_cycle(&self.input, meta_args.preferred_platform, flags);
        let mut metadata = outcome.metadata;
        let remaining = outcome.remaining;
        if outcome.platform == Platform::Unknown {
            // Same fallback as `read_metadata_for_input`: the heuristics' own answer (an
            // unambiguous extension, or a path-narrowed one) stands when no parser settled it.
            let size = std::fs::metadata(&self.input).ok().map(|m| m.len());
            let heuristic = detect::metadata_from_heuristics(&self.input, size, flags.ext, flags.path);
            if heuristic.platform != Platform::Unknown {
                metadata.platform = heuristic.platform;
                metadata.candidates = vec![heuristic.platform];
            } else if let [only] = remaining.as_slice() {
                metadata.platform = *only;
                metadata.candidates = vec![*only];
            }
        }
        let result = if metadata.platform == Platform::Unknown {
            StageResult::Err("platform could not be identified".to_string())
        } else {
            StageResult::Ok
        };
        (metadata, remaining, result)
    }

    /// [`Detect::Verify`].
    fn verify(&self, hint: Option<Platform>) -> (RomMetadata, Vec<Platform>, StageResult) {
        let hint = hint.filter(|p| *p != Platform::Unknown);
        let ext = extension_of(&self.input.to_string_lossy());
        if matches!(ext.as_str(), "zip" | "7z") {
            let mut trusted = self.args.clone();
            trusted.detect = Detect::Trust(hint.unwrap_or(Platform::Unknown));
            return Single::new(&self.input, trusted).detect();
        }
        let meta_args = &self.args.metadata;
        let flags = HeuristicFlags::new(meta_args.extension_heuristic, meta_args.path_heuristic);
        let mut tried: Vec<Platform> = hint.into_iter().collect();
        for candidate in detect::resolve_candidates(&self.input, None, flags) {
            if !tried.contains(&candidate) {
                tried.push(candidate);
            }
        }
        for &platform in &tried {
            if let Some(metadata) = detect::confirm_platform(&self.input, platform) {
                return (metadata, Vec::new(), StageResult::Ok);
            }
        }
        let metadata = RomMetadata { candidates: tried.clone(), file_size: std::fs::metadata(&self.input).ok().map(|m| m.len()), ..Default::default() };
        let reason = if tried.is_empty() { "no platform to check".to_string() } else { format!("no signature confirmed for {tried:?}") };
        (metadata, tried, StageResult::Skipped(SkipReason::NotApplicable(reason)))
    }

    /// Validates the arguments, detects the platform, lists the content and decides every
    /// stage, without touching the input's bytes beyond detection and container headers.
    fn plan(&self) -> Result<Plan, SingleError> {
        let args = &self.args;
        args.archive.validate().map_err(SingleError::InvalidArgs)?;
        if args.stages.archive && args.archive.any_enabled() && args.output_dir.is_none() {
            return Err(SingleError::InvalidArgs("archiving needs an output directory".to_string()));
        }
        let input = if args.whole_file { InputSet::resolve_file(&self.input) } else { InputSet::resolve(&self.input) }.map_err(SingleError::Input)?;

        let (metadata, remaining_candidates, detect_result) = self.detect();
        let platform_known = metadata.platform != Platform::Unknown;
        let mut stages = StageResults { detect: detect_result, ..Default::default() };

        // Content listing (also the extract stage's applicability).
        let mut content = None;
        if args.stages.extract || args.stages.hash || args.stages.convert || args.stages.archive {
            match ContentPlan::build(&input, &args.cue_sheet, args.cd_sheet) {
                Ok(plan) => content = Some(plan),
                Err(ContentError::Unavailable(message)) => {
                    if input.is_archive() {
                        stages.extract = StageResult::Skipped(SkipReason::Unavailable(message));
                    }
                }
                Err(err) => stages.extract = StageResult::Err(err.to_string()),
            }
        }
        let decodable = content.is_some();
        if args.stages.extract && matches!(stages.extract, StageResult::Off) {
            stages.extract = if input.is_archive() {
                StageResult::Ok
            } else {
                StageResult::Skipped(SkipReason::NotApplicable("input is not a container".to_string()))
            };
        }
        if args.stages.hash {
            stages.hash = if decodable || !input.is_archive() {
                StageResult::Ok
            } else {
                // No in-process decoder: the container's own bytes are hashed as a plain file.
                StageResult::Ok
            };
        }
        let content = match content {
            Some(plan) => plan,
            None => {
                // Fall back to treating the input as its on-disk file(s).
                let fallback = InputSet { kind: InputKind::PlainFile, ..input.clone() };
                let single = fallback.files.len() == 1;
                if single {
                    ContentPlan::build(&fallback, &args.cue_sheet, args.cd_sheet).map_err(|e| SingleError::Input(e.to_string()))?
                } else {
                    ContentPlan::build(&input, &args.cue_sheet, args.cd_sheet).map_err(|e| SingleError::Input(e.to_string()))?
                }
            }
        };
        let content_total = content.total_bytes();

        // Convert: only a single content file can be converted.
        let mut converter = None;
        let mut converted = None;
        if args.stages.convert {
            stages.convert = if !platform_known {
                StageResult::Skipped(SkipReason::PlatformUnknown)
            } else if content.files.len() != 1 {
                StageResult::Skipped(SkipReason::NotApplicable("conversion applies to a single content file".to_string()))
            } else {
                match converter_for(&metadata, &args.convert) {
                    None => StageResult::Skipped(SkipReason::NotApplicable(format!("no conversion registered for {:?}", metadata.platform))),
                    Some(convert) => {
                        let file = &content.files[0];
                        let head = match &file.source_path {
                            Some(path) => super::platforms::read_detection_prefix(path).unwrap_or_default(),
                            None => Vec::new(),
                        };
                        match StreamConverter::new(convert, file.size, &head) {
                            Ok(stream) => {
                                let stem = Path::new(&file.name).file_stem().and_then(|s| s.to_str()).unwrap_or(&input.stem).to_string();
                                converted = Some((format!("{stem}.{}", stream.output_extension()), stream.output_len()));
                                converter = Some(stream);
                                StageResult::Ok
                            }
                            Err(ConvertError::NotApplicable(message)) => StageResult::Skipped(SkipReason::NotApplicable(message)),
                            Err(ConvertError::Failed(message)) => StageResult::Err(message),
                        }
                    }
                }
            };
        }

        // Archive target, from the platform.
        let mut archive_target = None;
        let mut chd_command = None;
        let mut primary_name = None;
        if args.stages.archive {
            stages.archive = if !args.archive.any_enabled() {
                StageResult::Skipped(SkipReason::NotApplicable("no archive target enabled".to_string()))
            } else {
                // zip/7z take any platform's files (a bin/iso can be zipped too -- which
                // files get zipped is decided upstream), checked regardless of whether the
                // platform is known -- an unidentified file can still be zipped. CHD and
                // the Dolphin formats are picked by platform (the container's internal
                // layout depends on which one), so those genuinely need platform_known.
                let file_target = args.archive.file_target();
                // A sheet or an ISO says by itself what CHD it becomes (a CD or a DVD), so that
                // one needs no platform; a cartridge dump never gets that far.
                let disc_input = matches!(input.kind, InputKind::Sheet(_)) || (matches!(input.kind, InputKind::PlainFile) && extension_of(&input.path.to_string_lossy()) == "iso");
                let picked = file_target.or_else(|| {
                    if !platform_known {
                        return (args.archive.do_chd && disc_input).then_some(ArchiveTarget::Chd);
                    }
                    if is_chd_platform(metadata.platform) {
                        args.archive.do_chd.then_some(ArchiveTarget::Chd)
                    } else if is_rvz_platform(metadata.platform) {
                        args.archive.dolphin_target()
                    } else {
                        None
                    }
                });
                match picked {
                    None if !platform_known && file_target.is_none() => StageResult::Skipped(SkipReason::PlatformUnknown),
                    None => StageResult::Skipped(SkipReason::NotApplicable(format!("no enabled archive target applies to {:?}", metadata.platform))),
                    Some(target) => {
                        let repack = match target {
                            ArchiveTarget::Chd => args.archive.repack_chd,
                            ArchiveTarget::Rvz => args.archive.repack_rvz,
                            ArchiveTarget::Gcz => args.archive.repack_gcz,
                            ArchiveTarget::Wia => args.archive.repack_wia,
                            ArchiveTarget::Zip => args.archive.repack_zip,
                            ArchiveTarget::SevenZip => args.archive.repack_7z,
                        };
                        if input.archive_format() == Some(target_format(target)) && !repack {
                            StageResult::Skipped(SkipReason::NotApplicable(format!("already a {} and repack is off", target.extension())))
                        } else {
                            match self.archive_inputs(&input, &content, target, converted.as_ref()) {
                                Ok((command, primary)) => {
                                    archive_target = Some(target);
                                    chd_command = command;
                                    primary_name = primary;
                                    StageResult::Ok
                                }
                                Err(message) => StageResult::Skipped(SkipReason::NotApplicable(message)),
                            }
                        }
                    }
                }
            };
        }

        // Dependencies: extract feeds everything for a container input.
        if stages.extract.is_err() {
            let message = match &stages.extract {
                StageResult::Err(m) => m.clone(),
                _ => unreachable!(),
            };
            stages.fail(Stage::Extract, message, &[Stage::Hash, Stage::Convert, Stage::Archive]);
        }
        if stages.convert.is_err() && stages.archive.is_active() {
            stages.archive = StageResult::Skipped(SkipReason::DependencyFailed(Stage::Convert));
        }

        // Outputs and their safety checks.
        let mut planned_outputs = Vec::new();
        let mut archive_output = None;
        let mut archive_options = None;
        let write_dir = match (&args.output_dir, archive_target) {
            (Some(dir), None) if (stages.extract.is_ok() && input.is_archive()) || (stages.convert.is_ok()) => Some(dir.clone()),
            _ => None,
        };
        if let (Some(dir), Some(target)) = (&args.output_dir, archive_target) {
            let output = archive::output_path(dir, args.archive_stem.as_deref().unwrap_or(&input.stem), target);
            let mut options = args
                .archive
                .archive_options(target, metadata.platform, matches!(content.container, ContainerPlan::ChdCd { .. }) || matches!(input.kind, InputKind::Sheet(_)))
                .map_err(|err| SingleError::InvalidArgs(err.to_string()))?;
            options.num_processors = args.max_workers.map(|n| n as u32);
            options.compress_threads = args.max_workers.map(|n| n as u32);
            options.user_dir = args.temp_dir.clone();
            planned_outputs.push(output.clone());
            archive_output = Some(output);
            archive_options = Some(options);
        }
        if let Some(dir) = &write_dir {
            if stages.convert.is_ok() {
                if let Some((name, _)) = &converted {
                    planned_outputs.push(dir.join(name));
                }
            } else {
                for file in &content.files {
                    let out = dir.join(&file.name);
                    if !normalize(&out).starts_with(normalize(dir)) {
                        return Err(SingleError::InvalidArgs(format!("content name escapes the output directory: {}", file.name)));
                    }
                    planned_outputs.push(out);
                }
            }
        }
        let inputs: Vec<PathBuf> = input.files.iter().map(|p| normalize(p)).collect();
        for output in &planned_outputs {
            let normalized = normalize(output);
            if inputs.contains(&normalized) {
                return Err(SingleError::OutputCollidesWithInput(output.clone()));
            }
            if output.exists() {
                return Err(SingleError::OutputExists(output.clone()));
            }
        }

        // Cost weights.
        let model = &args.cost_model;
        let mut weights = [0f64; 5];
        let mut totals = [0u64; 5];
        weights[Stage::Detect.index()] = model.detect_bytes as f64;
        totals[Stage::Detect.index()] = model.detect_bytes;
        let extract_factor = match &content.container {
            ContainerPlan::ChdImage { .. } | ContainerPlan::ChdCd { .. } => model.extract_chd,
            ContainerPlan::Disc { .. } | ContainerPlan::Cso { .. } | ContainerPlan::Pbp { .. } => model.extract_disc,
            ContainerPlan::Zip { .. } => model.extract_zip,
            ContainerPlan::None => model.read_plain,
        };
        if stages.extract.is_ok() || stages.hash.is_ok() || stages.convert.is_ok() || stages.archive.is_ok() {
            let stage = if input.is_archive() && stages.extract.is_ok() { Stage::Extract } else { Stage::Hash };
            weights[stage.index()] += content_total as f64 * extract_factor;
            totals[Stage::Extract.index()] = content_total;
        }
        if stages.hash.is_ok() {
            let threads = HashDigest::worker_thread_count(args.hash_flags) as f64;
            let hashed = content_total + converted.as_ref().map_or(0, |(_, len)| *len);
            weights[Stage::Hash.index()] += hashed as f64 * model.hash_per_thread * threads;
            totals[Stage::Hash.index()] = content_total;
        }
        if stages.convert.is_ok() {
            let len = converted.as_ref().map_or(0, |(_, len)| *len);
            let factor = if converter.as_ref().is_some_and(|c| c.is_buffered()) { model.convert_buffered } else { model.convert_streaming };
            weights[Stage::Convert.index()] = len as f64 * factor;
            totals[Stage::Convert.index()] = len;
        }
        if let Some(target) = archive_target {
            let bytes = converted.as_ref().map_or(content_total, |(_, len)| *len);
            let factor = match target {
                ArchiveTarget::Chd => model.archive_chd,
                ArchiveTarget::Rvz | ArchiveTarget::Gcz | ArchiveTarget::Wia => model.archive_disc,
                ArchiveTarget::SevenZip => model.archive_7z,
                ArchiveTarget::Zip => model.archive_zip,
            };
            weights[Stage::Archive.index()] = bytes as f64 * factor;
            totals[Stage::Archive.index()] = bytes;
        }

        Ok(Plan {
            input,
            metadata,
            remaining_candidates,
            stages,
            content: Some(content),
            archive_target,
            archive_output,
            archive_options,
            chd_command,
            primary_name,
            write_dir,
            converter,
            converted,
            planned_outputs,
            weights,
            totals,
        })
    }

    /// Whether `target` can be created from this content, and with what primary input.
    /// Returns `(chdman command, primary source name)`.
    fn archive_inputs(
        &self,
        input: &InputSet,
        content: &ContentPlan,
        target: ArchiveTarget,
        converted: Option<&(String, u64)>,
    ) -> Result<(Option<ChdmanCommand>, Option<String>), String> {
        let names: Vec<String> = content.files.iter().map(|f| f.name.clone()).collect();
        let single_name = if names.len() == 1 { Some(converted.map_or(names[0].clone(), |(name, _)| name.clone())) } else { None };
        match target {
            ArchiveTarget::Chd => {
                let sheet = names.iter().find(|n| extension_of(n) == "gdi").or_else(|| names.iter().find(|n| extension_of(n) == "cue"));
                if let Some(sheet) = sheet {
                    return Ok((Some(ChdmanCommand::CreateCd), Some(sheet.clone())));
                }
                let Some(name) = single_name else {
                    return Err("a CHD needs a cue/gdi sheet or a single disc image".to_string());
                };
                match extension_of(&name).as_str() {
                    "iso" => Ok((Some(ChdmanCommand::CreateDvd), Some(name))),
                    "bin" | "img" | "raw" => Ok((Some(ChdmanCommand::CreateRaw), Some(name))),
                    other => Err(format!("cannot create a CHD from a .{other}")),
                }
            }
            ArchiveTarget::Rvz | ArchiveTarget::Gcz | ArchiveTarget::Wia => {
                let Some(name) = single_name else {
                    return Err(format!("{} takes a single disc image", target.extension()));
                };
                let is_disc = matches!(content.container, ContainerPlan::Disc { .. } | ContainerPlan::ChdImage { .. })
                    || (matches!(input.kind, InputKind::PlainFile) && extension_of(&name) == "iso");
                if !is_disc {
                    return Err(format!("{} takes a plain .iso, an RVZ/WIA/GCZ or a DVD CHD", target.extension()));
                }
                Ok((None, Some(name)))
            }
            ArchiveTarget::Zip | ArchiveTarget::SevenZip => Ok((None, None)),
        }
    }

    /// Runs the operation. `on_progress` is called on this thread; returning `false`
    /// cancels (a CHD creation already started still runs to completion, chdman having no
    /// cancel of its own).
    pub fn run(&self, on_progress: &mut dyn FnMut(&ProgressEvent) -> bool) -> Result<SingleOutcome, SingleError> {
        let started = Instant::now();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut progress = Progress { on_progress, weights: [0.0; 5], totals: [0; 5], done: [0; 5], settled: [false; 5], cancel: Arc::clone(&cancel), last_emit: None };
        progress.emit(Stage::Detect, Some("detecting"));
        let mut plan = self.plan()?;
        progress.weights = plan.weights;
        progress.totals = plan.totals;
        progress.done[Stage::Detect.index()] = plan.totals[Stage::Detect.index()];
        progress.settled[Stage::Detect.index()] = true;
        if !progress.emit(Stage::Detect, Some("detected")) {
            return Err(SingleError::Cancelled);
        }
        let content = plan.content.take().expect("plan always lists content");

        let streaming_needed = plan.stages.hash.is_ok() || plan.stages.convert.is_ok() || plan.stages.archive.is_ok() || (plan.stages.extract.is_ok() && plan.write_dir.is_some());
        // The container's own bytes: free when the input is one plain file hashed anyway
        // (its one content file *is* those bytes), else a raw read beside the decode.
        let single_file = matches!(plan.input.kind, InputKind::PlainFile | InputKind::Archive(_));
        let container_flags = self.args.container_hash.filter(|_| single_file);
        let derive_container = container_flags.is_some_and(|flags| {
            matches!(content.container, ContainerPlan::None) && content.files.len() == 1 && plan.stages.hash.is_ok() && self.args.hash_flags.contains(flags.difference(HashFlags::RAHASH))
        });
        let mut collected = Collected::default();
        let (result, raw_container) = std::thread::scope(|scope| {
            let tap = container_flags.filter(|_| !derive_container).map(|flags| {
                let path = plan.input.path.clone();
                let cancel = &cancel;
                scope.spawn(move || hash_raw(&path, flags, cancel))
            });
            let result = if streaming_needed {
                match plan.archive_target {
                    Some(ArchiveTarget::Rvz | ArchiveTarget::Gcz | ArchiveTarget::Wia) => self.run_dolphin(&mut plan, &content, &mut progress, &mut collected, &cancel),
                    _ => self.run_stream(&mut plan, &content, &mut progress, &mut collected, &cancel),
                }
            } else {
                Ok(())
            };
            let raw = tap.map(|handle| handle.join().unwrap_or_else(|_| Err("container hash thread panicked".to_string())));
            (result, raw)
        });
        if cancel.load(Ordering::Relaxed) {
            return Err(SingleError::Cancelled);
        }
        let input_container = match (raw_container, derive_container) {
            (Some(raw), _) => Some(raw),
            (None, true) => collected.records.first().map(|r| Ok(report_from_record(r))),
            (None, false) => None,
        };
        {
            if let Err(err) = result {
                if cancel.load(Ordering::Relaxed) {
                    return Err(SingleError::Cancelled);
                }
                let message = err.to_string();
                if plan.input.is_archive() && plan.stages.extract.is_active() {
                    plan.stages.fail(Stage::Extract, message, &[Stage::Hash, Stage::Convert, Stage::Archive]);
                } else {
                    plan.stages.fail(Stage::Hash, message, &[Stage::Convert, Stage::Archive]);
                }
            }
        }
        if let Some(err) = collected.convert_error.take() {
            match err {
                ConvertError::NotApplicable(message) => plan.stages.convert = StageResult::Skipped(SkipReason::NotApplicable(message)),
                ConvertError::Failed(message) => {
                    if plan.stages.archive.is_active() {
                        plan.stages.fail(Stage::Convert, message, &[Stage::Archive]);
                    } else {
                        plan.stages.convert = StageResult::Err(message);
                    }
                }
            }
        }
        if let Some(message) = collected.archive_error.take() {
            if plan.stages.archive.is_active() {
                plan.stages.archive = StageResult::Err(message);
            }
            if let Some(output) = &plan.archive_output {
                let _ = std::fs::remove_file(output);
            }
        }

        // Read back what was written: each file's own bytes, and a created archive's members
        // decoded beside that read.
        let mut checked_outputs = Vec::new();
        if let Some(check) = self.args.output_check {
            let mut written: Vec<(PathBuf, bool)> = collected.written.iter().map(|(path, _)| (path.clone(), false)).collect();
            if plan.stages.archive.is_ok() {
                if let Some(output) = &plan.archive_output {
                    written.push((output.clone(), check.members));
                }
            }
            for (path, members) in written {
                checked_outputs.push(self.check_output(&path, check.flags, members, &cancel));
                if cancel.load(Ordering::Relaxed) {
                    return Err(SingleError::Cancelled);
                }
            }
        }

        // Reports and outputs.
        let hashing = plan.stages.hash.is_ok();
        let input_hashes = hashing.then(|| build_report_from_members(collected.records.clone()));
        let converted_hashes = (hashing && !collected.converted_records.is_empty()).then(|| build_report_from_members(collected.converted_records.clone()));
        let mut outputs = Vec::new();
        for (path, report) in collected.written.drain(..) {
            let mut data = FileData::new(path);
            if let Some(report) = report {
                data.set_hash_report(report);
            }
            outputs.push(data);
        }
        if plan.stages.archive.is_ok() {
            if let Some(output) = &plan.archive_output {
                let mut data = FileData::new(output.clone());
                let members = if collected.converted_records.is_empty() { collected.records.clone() } else { collected.converted_records.clone() };
                // The archive's own identity is its bytes, known only once read back; the
                // members are what was sent into it. Without a read-back it carries the
                // members alone -- never a member's hash posing as the container's.
                let own = checked_outputs.iter().find(|c| &c.path == output).and_then(|c| c.container.as_ref().ok()).cloned();
                match own {
                    Some(report) => data.set_hash_report(HashReport { content_hashes: if hashing { members } else { Vec::new() }, ..report }),
                    None if hashing => data.set_hash_report(HashReport { content_hashes: members, ..Default::default() }),
                    None => {}
                }
                outputs.push(data);
            }
        }
        for stage in Stage::ALL {
            progress.settled[stage.index()] = true;
            progress.done[stage.index()] = progress.totals[stage.index()];
        }
        progress.emit(Stage::Archive, Some("done"));

        Ok(SingleOutcome {
            input: plan.input,
            metadata: plan.metadata,
            remaining_candidates: plan.remaining_candidates,
            stages: plan.stages,
            input_hashes,
            converted_hashes,
            outputs,
            archive_target: plan.archive_target,
            input_container,
            checked_outputs,
            content_files: content.files.clone(),
            elapsed: started.elapsed(),
        })
    }

    /// One written file read back: its raw bytes hashed and, for an archive, its members
    /// decoded and hashed on a second thread at the same time.
    fn check_output(&self, path: &Path, flags: HashFlags, members: bool, cancel: &AtomicBool) -> CheckedOutput {
        std::thread::scope(|scope| {
            let decode = members.then(|| scope.spawn(|| decode_members(path, flags, &self.args.cue_sheet, self.args.cd_sheet, &self.args.decode_threads, cancel)));
            let container = hash_raw(path, flags, cancel);
            let members = decode.map(|h| h.join().unwrap_or_else(|_| Err("member decode thread panicked".to_string())));
            CheckedOutput { path: path.to_path_buf(), container, members }
        })
    }

    /// Producer (`content::produce`) on this thread, optional CHD/zip/7z creator on another,
    /// joined by one pipe per content file.
    fn run_stream(
        &self,
        plan: &mut Plan,
        content: &ContentPlan,
        progress: &mut Progress<'_>,
        collected: &mut Collected,
        cancel: &Arc<AtomicBool>,
    ) -> Result<(), ContentError> {
        let archive_counter = Arc::new(AtomicU64::new(0));
        let mut pipes: Vec<Option<super::source::PipeWriter>> = Vec::new();
        let mut sources: Vec<Source> = Vec::new();
        if plan.archive_target.is_some() {
            for (index, file) in content.files.iter().enumerate() {
                let (writer, reader) = pipe(8);
                let (name, size) = match (&plan.converted, index) {
                    (Some((name, size)), 0) if content.files.len() == 1 => (name.clone(), *size),
                    _ => (file.name.clone(), file.size),
                };
                sources.push(Source::sequential(name, size, reader).counted(Arc::clone(&archive_counter)));
                pipes.push(Some(writer));
            }
        }
        let hash_flags = plan.stages.hash.is_ok().then_some(self.args.hash_flags);
        let mut sink = SinkImpl {
            plan,
            progress,
            hash_flags,
            pipes,
            collected,
            archive_counter: Arc::clone(&archive_counter),
            finished_bytes: 0,
        };

        let target = sink.plan.archive_target;
        let output = sink.plan.archive_output.clone();
        let options = sink.plan.archive_options.clone();
        let command = sink.plan.chd_command;
        let primary = sink.plan.primary_name.clone().unwrap_or_default();
        let cancel_for_consumer = Arc::clone(cancel);
        let input = sink.plan.input.clone();

        std::thread::scope(|scope| {
            let consumer = target.map(|target| {
                let output = output.clone().expect("archive output planned");
                let options = options.clone().expect("archive options planned");
                let cancel = cancel_for_consumer;
                let sources = &mut sources;
                scope.spawn(move || {
                    archive::archive_from_sources(&output, target, &options, command, &primary, sources, None, |_, _| !cancel.load(Ordering::Relaxed))
                })
            });
            let produced = produce(&input, content, &self.args.decode_threads, self.args.hash_flags, &mut sink, cancel);
            if let Some(handle) = consumer {
                while !handle.is_finished() {
                    std::thread::sleep(Duration::from_millis(50));
                    let done = archive_counter.load(Ordering::Relaxed);
                    sink.progress.done[Stage::Archive.index()] = done;
                    sink.progress.emit(Stage::Archive, Some("archiving"));
                }
                match handle.join() {
                    Ok(Ok(())) => {
                        sink.progress.done[Stage::Archive.index()] = sink.progress.totals[Stage::Archive.index()];
                    }
                    Ok(Err(ArchiveError::Cancelled)) => {
                        cancel.store(true, Ordering::Relaxed);
                    }
                    Ok(Err(err)) => sink.collected.archive_error = Some(err.to_string()),
                    Err(_) => sink.collected.archive_error = Some("archive creator thread panicked".to_string()),
                }
            }
            produced
        })
    }

    /// Dolphin pulls the input itself (random access); its in-order input callback feeds
    /// the single content file's pipeline.
    fn run_dolphin(
        &self,
        plan: &mut Plan,
        content: &ContentPlan,
        progress: &mut Progress<'_>,
        collected: &mut Collected,
        cancel: &Arc<AtomicBool>,
    ) -> Result<(), ContentError> {
        let file = &content.files[0];
        let name = plan.primary_name.clone().unwrap_or_else(|| file.name.clone());
        let mut source = match (&plan.input.kind, &content.container) {
            (InputKind::Archive(ArchiveFormat::Rvz | ArchiveFormat::Wia | ArchiveFormat::Gcz), ContainerPlan::Disc { data_size }) => {
                let reader = DiscReader::open(&plan.input.path, self.args.temp_dir.as_deref()).map_err(|e| ContentError::Failed(e.to_string()))?;
                Source::random(name, *data_size, reader)
            }
            (InputKind::Archive(ArchiveFormat::Chd), ContainerPlan::ChdImage { logical_bytes, .. }) => {
                let reader = ChdReader::open(&plan.input.path, None, 0).map_err(|e| ContentError::Failed(e.to_string()))?;
                Source::random(name, *logical_bytes, reader)
            }
            _ => Source::file(&plan.input.path, name).map_err(|e| ContentError::Io(e.to_string()))?,
        };
        let hash_flags = plan.stages.hash.is_ok().then_some(self.args.hash_flags);
        let target = plan.archive_target.expect("dolphin route needs a target");
        let output = plan.archive_output.clone().expect("archive output planned");
        let options = plan.archive_options.clone().expect("archive options planned");
        let mut sink = SinkImpl { plan, progress, hash_flags, pipes: Vec::new(), collected, archive_counter: Arc::new(AtomicU64::new(0)), finished_bytes: 0 };
        let mut pipeline = sink.begin(0, file)?;
        let mut failure: Option<ContentError> = None;
        let mut pushed = 0u64;
        let mut tap = |_offset: u64, bytes: &[u8]| {
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            match pipeline.push(Arc::from(bytes)) {
                Ok(()) => {
                    pushed += bytes.len() as u64;
                    sink.progress.done[Stage::Archive.index()] = pushed;
                    sink.tick(0, pushed)
                }
                Err(err) => {
                    failure = Some(err.into());
                    false
                }
            }
        };
        let result = archive::archive_from_sources(&output, target, &options, None, &file.name, std::slice::from_mut(&mut source), Some(&mut tap), |_, _| {
            !cancel.load(Ordering::Relaxed)
        });
        drop(tap);
        if let Some(err) = failure {
            pipeline.abort(&err.to_string());
            return Err(err);
        }
        match result {
            Ok(()) => {}
            Err(ArchiveError::Cancelled) => {
                cancel.store(true, Ordering::Relaxed);
                pipeline.abort("cancelled");
                return Err(ContentError::Cancelled);
            }
            Err(err) => sink.collected.archive_error = Some(err.to_string()),
        }
        let outcome = pipeline.finish()?;
        sink.end(0, file, outcome)
    }
}

/// Progress bookkeeping for one run; emits on the caller's thread.
struct Progress<'a> {
    on_progress: &'a mut dyn FnMut(&ProgressEvent) -> bool,
    weights: [f64; 5],
    totals: [u64; 5],
    done: [u64; 5],
    /// Stage finished (or decided not to run): counts as complete in the blend.
    settled: [bool; 5],
    cancel: Arc<AtomicBool>,
    last_emit: Option<Instant>,
}

impl Progress<'_> {
    fn fraction(&self, stage: Stage) -> f32 {
        let i = stage.index();
        if self.settled[i] || self.totals[i] == 0 {
            return 1.0;
        }
        (self.done[i] as f64 / self.totals[i] as f64).min(1.0) as f32
    }

    fn total_fraction(&self) -> f32 {
        let total: f64 = self.weights.iter().sum();
        if total <= 0.0 {
            // Nothing planned yet (the first event, before detection): nothing done.
            return 0.0;
        }
        let done: f64 = Stage::ALL.iter().map(|s| self.weights[s.index()] * self.fraction(*s) as f64).sum();
        (done / total).min(1.0) as f32
    }

    /// Emits (rate-limited to ~20/s, except messages) and returns `false` on cancel.
    fn emit(&mut self, stage: Stage, message: Option<&str>) -> bool {
        if self.cancel.load(Ordering::Relaxed) {
            return false;
        }
        let now = Instant::now();
        if message.is_none() {
            if let Some(last) = self.last_emit {
                if now.duration_since(last) < Duration::from_millis(50) {
                    return true;
                }
            }
        }
        self.last_emit = Some(now);
        let event = ProgressEvent {
            stage,
            stage_fraction: self.fraction(stage),
            total_fraction: self.total_fraction(),
            bytes_done: self.done[stage.index()],
            bytes_total: self.totals[stage.index()],
            message,
        };
        let keep_going = (self.on_progress)(&event);
        if !keep_going {
            self.cancel.store(true, Ordering::Relaxed);
        }
        keep_going
    }
}

/// What the sinks gathered over a run.
#[derive(Default)]
struct Collected {
    records: Vec<FileHashRecord>,
    converted_records: Vec<FileHashRecord>,
    /// Files written with the report of the stream that wrote them.
    written: Vec<(PathBuf, Option<HashReport>)>,
    convert_error: Option<ConvertError>,
    archive_error: Option<String>,
}

struct SinkImpl<'p, 'a> {
    plan: &'p mut Plan,
    progress: &'p mut Progress<'a>,
    hash_flags: Option<HashFlags>,
    pipes: Vec<Option<super::source::PipeWriter>>,
    collected: &'p mut Collected,
    archive_counter: Arc<AtomicU64>,
    /// Bytes of the content files already finished.
    finished_bytes: u64,
}

impl ContentSink for SinkImpl<'_, '_> {
    fn begin(&mut self, index: usize, file: &ContentFile) -> Result<ChunkPipeline, ContentError> {
        let mut sinks = Vec::new();
        let converter = if index == 0 { self.plan.converter.take() } else { None };
        if let Some(dir) = &self.plan.write_dir {
            let name = match (&converter, &self.plan.converted) {
                (Some(_), Some((name, _))) => name.clone(),
                _ => file.name.clone(),
            };
            let path = dir.join(name);
            sinks.push(ChunkSink::file(&path).map_err(|err| ContentError::Io(format!("create {}: {err}", path.display())))?);
        }
        if let Some(writer) = self.pipes.get_mut(index).and_then(Option::take) {
            sinks.push(ChunkSink::pipe(writer));
        }
        Ok(ChunkPipeline::new(self.hash_flags, converter, sinks))
    }

    fn tick(&mut self, _index: usize, bytes_in_file: u64) -> bool {
        let done = self.finished_bytes + bytes_in_file;
        self.progress.done[Stage::Extract.index()] = done;
        self.progress.done[Stage::Hash.index()] = done;
        self.progress.done[Stage::Convert.index()] = done.min(self.progress.totals[Stage::Convert.index()]);
        if self.plan.archive_target.is_some() && !matches!(self.plan.archive_target, Some(ArchiveTarget::Rvz | ArchiveTarget::Gcz | ArchiveTarget::Wia)) {
            self.progress.done[Stage::Archive.index()] = self.archive_counter.load(Ordering::Relaxed);
        }
        let stage = if self.plan.input.is_archive() && self.plan.stages.extract.is_ok() { Stage::Extract } else { Stage::Hash };
        self.progress.emit(stage, None)
    }

    fn end(&mut self, _index: usize, file: &ContentFile, outcome: PipelineOutcome) -> Result<(), ContentError> {
        self.finished_bytes += file.size;
        if let Some(report) = outcome.input_report.clone() {
            self.collected.records.push(record_from_report(file.name.clone(), outcome.input_bytes, report));
        }
        let converted_name = self.plan.converted.as_ref().map(|(name, _)| name.clone());
        if let (Some(report), Some(name)) = (outcome.output_report.clone(), &converted_name) {
            self.collected.converted_records.push(record_from_report(name.clone(), outcome.output_bytes, report));
        }
        if let Some(err) = outcome.convert_error {
            self.collected.convert_error = Some(err);
        }
        for path in outcome.written {
            let report = if converted_name.as_deref() == path.file_name().and_then(|n| n.to_str()) && outcome.output_report.is_some() {
                outcome.output_report.clone()
            } else {
                outcome.input_report.clone()
            };
            self.collected.written.push((path, report));
        }
        Ok(())
    }
}

/// A file's own bytes, read once in large sequential chunks into one digest per algorithm.
/// RAHASH is left out: it is a per-console value over a ROM, not over a container.
fn hash_raw(path: &Path, flags: HashFlags, cancel: &AtomicBool) -> Result<HashReport, String> {
    use std::io::Read;
    let digest = HashDigest::new(flags.difference(HashFlags::RAHASH));
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut buf = vec![0u8; super::hash::HASH_FILE_DEFAULT_BUF_SIZE];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".to_string());
        }
        let n = file.read(&mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        digest.update_arc(Arc::from(&buf[..n])).map_err(|e| e.to_string())?;
    }
    Ok(digest.finish())
}

/// The members of the container at `path`, decoded and hashed in one pass -- the same
/// listing and naming ([`ContentPlan`]) a [`Single`] run over that file would use.
pub fn decode_members(
    path: &Path,
    flags: HashFlags,
    cue: &CueSheetOptions,
    cd_sheet: SheetKind,
    threads: &DecodeThreads,
    cancel: &AtomicBool,
) -> Result<Vec<FileHashRecord>, String> {
    let input = InputSet::resolve_file(path)?;
    let plan = ContentPlan::build(&input, cue, cd_sheet).map_err(|e| e.to_string())?;
    let mut sink = MemberHasher { flags: flags.difference(HashFlags::RAHASH), records: Vec::new() };
    produce(&input, &plan, threads, flags, &mut sink, cancel).map_err(|e| e.to_string())?;
    Ok(sink.records)
}

/// A record's hashes as a report of their own.
pub fn report_from_record(record: &FileHashRecord) -> HashReport {
    HashReport {
        crc32: record.crc32.clone(),
        xxh3_128: record.xxh3_128.clone(),
        md5: record.md5.clone(),
        sha1: record.sha1.clone(),
        sha256: record.sha256.clone(),
        ..Default::default()
    }
}

/// Hash-only content sink: one record per content file.
struct MemberHasher {
    flags: HashFlags,
    records: Vec<FileHashRecord>,
}

impl ContentSink for MemberHasher {
    fn begin(&mut self, _index: usize, _file: &ContentFile) -> Result<ChunkPipeline, ContentError> {
        Ok(ChunkPipeline::new(Some(self.flags), None, Vec::new()))
    }

    fn tick(&mut self, _index: usize, _bytes_in_file: u64) -> bool {
        true
    }

    fn end(&mut self, _index: usize, file: &ContentFile, outcome: PipelineOutcome) -> Result<(), ContentError> {
        if let Some(report) = outcome.input_report {
            self.records.push(record_from_report(file.name.clone(), outcome.input_bytes, report));
        }
        Ok(())
    }
}
