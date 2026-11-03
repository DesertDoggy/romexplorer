//! Per-container decode thread-count presets: how many [`super::source::ReadAt`] workers
//! [`super::parallel`]'s functions (and, symmetrically, a caller's own `ChdReader`/
//! `DiscReader` handles) use to decode one file. This is the *decode* side -- separate from,
//! but not indifferent to, hashing threads: `extract_and_hash_to_file`
//! ([`super::parallel::extract_and_hash_to_file`], the recommended way to extract+hash) hands
//! decoded blocks to [`super::hash::HashDigest`], whose own 1-4 background worker threads
//! (see [`super::hash::HashDigest::worker_thread_count`]) compete with the decode workers
//! for the same physical cores, so every recommendation here takes the [`super::hash::HashFlags`]
//! that will be hashed alongside it and leaves room for those threads too.
//!
//! Benchmarked defaults (`tests/results/benchmark_streaming_hash_strategy_*.md`):
//! - **CHD DVD/raw: `physical cores - hash worker threads`, minimum 1.** CHD decode kept
//!   improving all the way to the highest thread count tried (20, this machine's full
//!   logical count) with no ceiling of its own -- but hyperthread/SMT siblings share
//!   execution units, and this machine mixes hyperthreaded P-cores with non-hyperthreaded
//!   E-cores (20 logical, 14 physical), so physical count is the safer base, and hashing
//!   is heavier CPU work than decoding (SSD/HDD writes are disk-bound regardless, so
//!   giving hash threads room doesn't cost decode-to-disk throughput the way it would on a
//!   RAM target) -- hence subtracting the hasher threads a given `HashFlags` will actually
//!   spawn, not just leaving a flat margin.
//! - **RVZ: 4, unless fewer than ~8 physical cores are available.** `hash_strategy_rvz`
//!   found 4 optimal on its own (already run with hashing happening alongside it) and
//!   *worse* at 20 (a single hasher thread can't drain more than ~4 decode producers) --
//!   so unlike CHD, more headroom from a bigger machine doesn't raise this number. It only
//!   comes down on a small machine, where 4 decode + up to 4 hash threads would already
//!   exceed what's available.
//! - **CHD CD: same formula as DVD/raw** (`physical cores - hash worker threads`, min 1).
//!   `hash_strategy_chd_cd`'s full 1..20T sweep (32 combinations x 3 runs) showed CD decode
//!   scaling the same shape as DVD/raw -- continuing to improve through
//!   20T with no RVZ-style early ceiling -- so it gets no separate cap. That run also found
//!   extract-while-hashing and extract-then-hash within ~1% of each other in total (111.3s
//!   vs 112.4s summed across the whole sweep) for CD specifically, unlike the large,
//!   consistent win fused hashing had for RVZ and CHD DVD/raw: CD decode (CD-specific
//!   codecs, e.g. FLAC for audio tracks, are noticeably heavier per hunk than DVD/raw's)
//!   dominates the total time enough that the second hash pass barely registers either way.
//!   The default stays extract-while-hashing regardless, since it's still never worse.

use super::hash::{HashDigest, HashFlags};
use super::native_lib;

/// Which kind of disc a CHD holds. CD/GD-ROM (track-based, random access served through
/// sector reads) and DVD/raw (one flat logical image) have separate presets -- see this
/// module's doc comment for why they scale differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChdKind {
    Cd,
    DvdRaw,
}

/// Per-container decode thread counts. `None` in a field means "use the recommended
/// default" (see this module's doc comment); `Some(n)` pins that container to exactly `n`
/// threads, bypassing the hash-thread-aware formula entirely -- an explicit override is
/// taken at face value. `Default`/[`Self::recommended`] leave every field `None`; override
/// just one with `DecodeThreads { rvz: Some(2), ..Default::default() }`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct DecodeThreads {
    pub chd_cd: Option<usize>,
    pub chd_dvd_raw: Option<usize>,
    pub rvz: Option<usize>,
}

impl DecodeThreads {
    /// Every container uses its recommended default (equivalent to `Self::default()`; a
    /// more discoverable name for the same thing).
    pub fn recommended() -> Self {
        Self::default()
    }

    /// Resolves the decode thread count for a CHD of the given kind. `hash_flags` is what
    /// will be hashed alongside the decode (e.g. via [`super::parallel::extract_and_hash_to_file`]);
    /// pass [`HashFlags::empty`] only if nothing will actually be hashed (`HashDigest`
    /// itself then falls back to its own default set, so this still leaves room for that).
    pub fn chd(&self, kind: ChdKind, hash_flags: HashFlags) -> usize {
        let field = match kind {
            ChdKind::Cd => self.chd_cd,
            ChdKind::DvdRaw => self.chd_dvd_raw,
        };
        field.unwrap_or_else(|| Self::recommended_chd(hash_flags))
    }

    /// Resolves the decode thread count for RVZ/WIA/GCZ. `hash_flags`: see [`Self::chd`].
    pub fn rvz(&self, hash_flags: HashFlags) -> usize {
        self.rvz.unwrap_or_else(|| Self::recommended_rvz(hash_flags))
    }

    fn recommended_chd(hash_flags: HashFlags) -> usize {
        recommended_chd_threads(native_lib::physical_core_count(), hash_flags)
    }

    fn recommended_rvz(hash_flags: HashFlags) -> usize {
        recommended_rvz_threads(native_lib::physical_core_count(), hash_flags)
    }
}

/// The CHD decode-thread recommendation (either kind -- see this module's doc comment) for
/// a file with `available_cores` physical cores to itself: `available_cores - hash worker
/// threads`, minimum 1. `available_cores` need not be the whole machine -- a caller running
/// several large files at once can pass each one its share of the cores; [`DecodeThreads::chd`] is the single-file convenience wrapper that
/// always passes the whole machine ([`native_lib::physical_core_count`]).
pub fn recommended_chd_threads(available_cores: usize, hash_flags: HashFlags) -> usize {
    available_cores.max(1).saturating_sub(HashDigest::worker_thread_count(hash_flags)).max(1)
}

/// Same as [`recommended_chd_threads`], capped at 4 -- see this module's doc comment for
/// why RVZ doesn't scale past that regardless of how many cores are available.
pub fn recommended_rvz_threads(available_cores: usize, hash_flags: HashFlags) -> usize {
    recommended_chd_threads(available_cores, hash_flags).min(4)
}
