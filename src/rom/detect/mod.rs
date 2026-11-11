//! ROM identity detection: the 4 modes from `docs/plan/rom_module.md`.
//!
//! 1. **Forced** -- caller asserted the platform; skip all heuristics/cycling.
//! 2. **Path heuristic** -- parent directory name names the platform.
//! 3. **Extension heuristic** -- file extension names the platform (possibly narrowed to a
//!    single candidate by the path heuristic).
//! 4. **Deep cycle** -- actually read the file, trying each narrowed candidate platform's
//!    own header/descriptor parser until one produces real metadata.
//!
//! Ported from `reference/old_archiver.rs`'s `read_metadata_for_input` (line 2344) and its
//! supporting heuristic functions, **minus** `strict_metadata_mode`/`quick_metadata_mode`
//! (no caller in the new design needs either; both only ever suppressed work this flow now
//! simply doesn't do). Per the agreed design, extension/path narrowing happens entirely
//! *before* the cycle, and the cycle itself only performs per-candidate metadata
//! extraction -- it never re-derives candidates.

pub mod chd_disc;
pub mod descriptor;
pub mod disc_dispatch;
pub mod disc_fs;
pub mod nand_dispatch;
pub mod reason;
pub mod systemfile;
pub mod rvz_disc;
pub mod sheet;

use std::fs;
use std::path::Path;

use crate::common::models::{ArchiveFormat, MediaType, Platform};

use super::models::{metadata_score, normalize_metadata, push_detection_reason, RomMetadata};
use super::platforms;
use reason::{DetectionStepKind, Evidence, RomReason};

fn extension_lowercase(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

/// Which heuristics the caller allowed.
///
/// These must be threaded all the way down into the deep cycle. `reference/old_archiver.rs`
/// took them as parameters at the top (`read_metadata_for_input` line 2344) but then
/// hardcoded `metadata_from_heuristics(path, size, true, true)` inside `deep_try_disc_platform`
/// (3262) and `deep_read_header_metadata` (5138) -- so disabling the path heuristic didn't
/// actually disable it once the cycle started, and a file in a misleadingly-named directory
/// was still resolved by that directory's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeuristicFlags {
    pub ext: bool,
    pub path: bool,
}

impl HeuristicFlags {
    pub const ALL: Self = Self { ext: true, path: true };

    pub fn new(ext: bool, path: bool) -> Self {
        Self { ext, path }
    }

    /// The path heuristic is suppressed for tape images: an audio container says nothing
    /// about its machine, so letting a directory name decide would fabricate confidence
    /// the content doesn't support. Matches old code's
    /// `!is_audio_container_extension(ext)` at line 5138.
    pub fn for_header_read(self, ext: &str) -> Self {
        Self { ext: self.ext, path: self.path && !is_audio_container_extension(ext) }
    }
}

impl Default for HeuristicFlags {
    fn default() -> Self {
        Self::ALL
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Resolves a ROM's identity metadata, taking whichever of the 4 modes applies. `path` must
/// be a real file on disk -- CHD/RVZ/zip/7z inputs whose *contents* need reading are
/// extracted first (see `super::extract`), so nothing here decodes a container in-process.
pub fn read_metadata_for_input(
    path: &Path,
    forced_platform: Option<Platform>,
    ext_heuristics: bool,
    path_heuristics: bool,
) -> RomMetadata {
    let input_file_size = fs::metadata(path).ok().map(|m| m.len());
    let ext = extension_lowercase(path);
    let flags = HeuristicFlags::new(ext_heuristics, path_heuristics);

    // Mode 1.
    if let Some(forced) = forced_platform {
        return metadata_for_forced_platform(path, &ext, input_file_size, forced, flags);
    }

    let heuristic = enrich_heuristic_metadata(path, metadata_from_heuristics(path, input_file_size, ext_heuristics, path_heuristics));
    let cycle_candidates = merged_heuristic_cycle_candidates(path, ext_heuristics, path_heuristics);

    // Modes 2/3: heuristics narrowed to exactly one non-disc candidate. Still try that one
    // platform's parser (a cartridge header read is cheap and yields title/serial the
    // heuristics can't), but fall back to the heuristic answer rather than cycling further.
    if cycle_candidates.len() == 1 && !is_disc_hint(&ext, &cycle_candidates) {
        if let Some(deep) = platforms::deep_try_platform(path, &ext, input_file_size, cycle_candidates[0], flags) {
            if deep.title.is_some() || deep.serial_product_id.is_some() {
                return finalize_metadata(path, normalize_metadata(deep), RomReason::DeepCycle);
            }
        }
        return finalize_metadata(path, normalize_metadata(heuristic), infer_metadata_reason(path, ext_heuristics, path_heuristics));
    }

    // Mode 4.
    if let Some(deep) = run_metadata_cycle(path, &ext, input_file_size, &cycle_candidates, flags) {
        return finalize_metadata(path, normalize_metadata(deep), RomReason::DeepCycle);
    }

    finalize_metadata(path, normalize_metadata(heuristic), infer_metadata_reason(path, ext_heuristics, path_heuristics))
}

/// Mode 1. No direct precedent in `reference/old_archiver.rs` (it had no true forced-platform
/// short-circuit at this layer): the caller's platform is treated as ground truth, and the
/// only work left is running that one platform's own parser for title/serial.
fn metadata_for_forced_platform(
    path: &Path,
    ext: &str,
    input_file_size: Option<u64>,
    forced: Platform,
    flags: HeuristicFlags,
) -> RomMetadata {
    let mut meta = platforms::deep_try_platform(path, ext, input_file_size, forced, flags).unwrap_or_default();
    meta.platform = forced;
    meta.candidates = vec![forced];
    meta.file_size = meta.file_size.or(input_file_size);
    push_detection_reason(&mut meta, DetectionStepKind::Finalization, Evidence::Note(format!("forced platform {forced:?}")));

    let mut meta = normalize_metadata(meta);
    meta.media_type = infer_media_type_from_heuristics(ext, forced);
    meta.reason = RomReason::Forced;
    meta.reason_string = format!("forced platform {forced:?}");
    meta
}

/// Applies the media-type inference and stamps the final [`RomReason`]. Ported from
/// `reference/old_archiver.rs`'s `finalize_metadata` (line 2487).
fn finalize_metadata(path: &Path, mut meta: RomMetadata, reason: RomReason) -> RomMetadata {
    let ext = extension_lowercase(path);
    meta.media_type = infer_media_type_from_heuristics(&ext, meta.platform);
    meta.reason = reason;
    meta.reason_string = format!("{reason:?} for .{ext}");
    push_detection_reason(&mut meta, DetectionStepKind::Finalization, Evidence::Note(format!("finalized as {reason:?} for .{ext}")));
    meta
}

/// Decides which heuristic mode (2 or 3) gets credit when the deep cycle didn't resolve
/// anything. Ported from `reference/old_archiver.rs`'s `infer_metadata_finalization_method`
/// (line 2449) -- same branching, returning [`RomReason`] instead of the old
/// `MetadataFinalizationMethod`.
fn infer_metadata_reason(path: &Path, ext_heuristics: bool, path_heuristics: bool) -> RomReason {
    if ext_heuristics {
        let ext = extension_lowercase(path);
        let ext_candidates = infer_platforms_from_extension_heuristics(&ext);
        if !ext_candidates.is_empty() {
            if ext_candidates.len() == 1 {
                return RomReason::ExtensionHeuristic;
            }
            if path_heuristics {
                if let Some(path_platform) = infer_platform_from_parent_path_heuristics(path) {
                    if narrow_extension_candidates_by_path(&ext_candidates, path_platform).is_some() {
                        return RomReason::PathHeuristic;
                    }
                }
            }
            return RomReason::ExtensionHeuristic;
        }
    }

    if path_heuristics && infer_platform_from_parent_path_heuristics(path).is_some() {
        return RomReason::PathHeuristic;
    }

    if ext_heuristics { RomReason::ExtensionHeuristic } else { RomReason::PathHeuristic }
}

// ---------------------------------------------------------------------------
// Heuristic metadata (modes 2/3)
// ---------------------------------------------------------------------------

/// Per-format codec label. Old code (`metadata_codecs_for_format`, line 2516) opened CHDs
/// with its native header reader here to list real codec names; that reader is gone, and
/// paying a native call on every input just to label the heuristic path isn't worth it --
/// real codec detail lands in [`RomMetadata::chd_metadata`]/`seven_zip_metadata` etc. when
/// an operation actually opens the file.
fn metadata_codecs_for_format(format: ArchiveFormat) -> Vec<String> {
    match format {
        ArchiveFormat::Chd => vec!["chd".to_string()],
        ArchiveFormat::Rvz => vec!["rvz".to_string()],
        ArchiveFormat::Wia => vec!["wia".to_string()],
        ArchiveFormat::Gcz => vec!["gcz".to_string()],
        ArchiveFormat::Zip => vec!["zip".to_string()],
        ArchiveFormat::SevenZip => vec!["7z".to_string()],
        _ => Vec::new(),
    }
}

/// Builds the mode-2/3 metadata from extension and parent-path heuristics alone, without
/// reading a byte of the file's content. Ported from `reference/old_archiver.rs`'s
/// `metadata_from_heuristics` (line 2566).
pub fn metadata_from_heuristics(path: &Path, file_size: Option<u64>, ext_heuristics: bool, path_heuristics: bool) -> RomMetadata {
    let ext = extension_lowercase(path);

    let ext_candidates = if ext_heuristics { infer_platforms_from_extension_heuristics(&ext) } else { Vec::new() };
    let platform_from_path = if path_heuristics { infer_platform_from_parent_path_heuristics(path) } else { None };

    let mut candidates = Vec::new();
    for candidate in ext_candidates.iter().copied() {
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    if let Some(path_platform) = platform_from_path {
        if !candidates.contains(&path_platform) {
            candidates.push(path_platform);
        }
    }

    let narrowed_by_path = if ext_candidates.len() > 1 {
        platform_from_path.and_then(|path_platform| narrow_extension_candidates_by_path(&ext_candidates, path_platform))
    } else {
        None
    };

    let platform = if !ext_candidates.is_empty() {
        if ext_candidates.len() == 1 {
            ext_candidates[0]
        } else if let Some(narrowed) = narrowed_by_path {
            narrowed
        } else {
            Platform::Unknown
        }
    } else if let Some(path_platform) = platform_from_path {
        path_platform
    } else {
        Platform::Unknown
    };

    let format = descriptor::detect_archive_format_quick(path).unwrap_or(ArchiveFormat::Unknown);
    let codecs = metadata_codecs_for_format(format);

    let mut meta = RomMetadata {
        platform,
        media_type: infer_media_type_from_heuristics(&ext, platform),
        codec: codecs.first().cloned(),
        codecs,
        file_size,
        candidates,
        ..Default::default()
    };

    if ext_heuristics {
        if ext_candidates.is_empty() {
            push_detection_reason(&mut meta, DetectionStepKind::ExtensionHeuristic, Evidence::Note(format!("ext .{ext} had no candidates")));
        } else {
            push_detection_reason(
                &mut meta,
                DetectionStepKind::ExtensionHeuristic,
                Evidence::Note(format!("ext .{ext} candidates={ext_candidates:?}")),
            );
        }
    }
    if path_heuristics {
        if let Some(path_platform) = platform_from_path {
            push_detection_reason(
                &mut meta,
                DetectionStepKind::PathHeuristic,
                Evidence::Note(format!("parent path inferred {path_platform:?}")),
            );
        }
    }
    if let Some(narrowed) = narrowed_by_path {
        push_detection_reason(
            &mut meta,
            DetectionStepKind::ExtensionPathNarrow,
            Evidence::Note(format!("narrowed candidates by path to {narrowed:?}")),
        );
    }

    meta
}

/// Backfills a NeoGeo CD game id from sibling/descriptor payload paths once the platform is
/// already known -- **centralized here**, where old code duplicated this same inference at
/// four separate call sites. Ported from `reference/old_archiver.rs`'s
/// `enrich_heuristic_metadata` (line 2534).
fn enrich_heuristic_metadata(path: &Path, mut meta: RomMetadata) -> RomMetadata {
    if meta.platform != Platform::NeoGeoCd || meta.serial_product_id.is_some() {
        return meta;
    }

    // Real evidence for what's actually found: a scanned `NEO-GEO` marker plus the two raw
    // BCD bytes decoded into the game id right after it (see
    // `platforms::other::infer_neogeo_cd_game_id_with_evidence_from_bytes`), not just the
    // decoded id text with no indication of where it came from.
    let marker_evidence = |candidate: &Path| -> Option<(String, usize, Vec<u8>)> {
        let bytes = fs::read(candidate).ok()?;
        let (offset, id) = platforms::other::infer_neogeo_cd_game_id_with_evidence_from_bytes(&bytes)?;
        Some((id, offset, bytes.get(offset..offset + 10).unwrap_or_default().to_vec()))
    };

    let mut found = marker_evidence(path);
    if found.is_none() {
        let ext = extension_lowercase(path);
        if matches!(ext.as_str(), "cue" | "gdi") {
            found = descriptor::descriptor_payload_paths(path).iter().find_map(|p| marker_evidence(p));
        }
        if matches!(ext.as_str(), "chd" | "rvz") {
            if let Some(sidecar) = find_sidecar_descriptor(path) {
                found = descriptor::descriptor_payload_paths(&sidecar).iter().find_map(|p| marker_evidence(p));
            }
        }
    }

    if let Some((game_id, offset, bytes)) = found {
        push_detection_reason(
            &mut meta,
            DetectionStepKind::Inference,
            Evidence::Bytes {
                offset_start: offset,
                offset_end: offset + 10,
                bytes,
                note: "ASCII 'NEO-GEO' marker; NGH id decoded from BCD bytes at marker+8/+9".to_string(),
            },
        );
        meta.serial_product_id = Some(game_id);
    }
    meta
}

/// Finds a `.cue`/`.gdi` sitting next to `path` with the same stem. Ported from
/// `reference/old_archiver.rs`'s `find_sidecar_descriptor` (line 6686) with its lookup
/// order inverted to check `.gdi` first, for the same reason
/// [`descriptor::preferred_archive_descriptor_member_name`] does: `docs/plan/rom_module.md`
/// specifies gdi is preferred over cue when both describe the same disc.
pub fn find_sidecar_descriptor(path: &Path) -> Option<std::path::PathBuf> {
    let stem = path.file_stem()?.to_str()?;
    let dir = path.parent()?;

    let gdi = dir.join(format!("{stem}.gdi"));
    if gdi.is_file() {
        return Some(gdi);
    }
    let cue = dir.join(format!("{stem}.cue"));
    if cue.is_file() {
        return Some(cue);
    }

    None
}

// ---------------------------------------------------------------------------
// Candidate narrowing
// ---------------------------------------------------------------------------

/// The candidate list the deep cycle will walk, after extension/path narrowing. Ported from
/// `reference/old_archiver.rs`'s `merged_heuristic_cycle_candidates` (line 2674).
fn merged_heuristic_cycle_candidates(path: &Path, ext_heuristics: bool, path_heuristics: bool) -> Vec<Platform> {
    let ext = extension_lowercase(path);

    if ext_heuristics {
        let ext_candidates = infer_platforms_from_extension_heuristics(&ext);
        if !ext_candidates.is_empty() {
            // Audio containers (tape dumps) keep the full candidate list: the container
            // format says nothing about which machine's tape it holds, so the cycle has to
            // try each and pick by evidence.
            if is_audio_container_extension(&ext) {
                return ext_candidates;
            }
            if ext_candidates.len() == 1 {
                return ext_candidates;
            }
            if path_heuristics {
                if let Some(path_platform) = infer_platform_from_parent_path_heuristics(path) {
                    if let Some(narrowed) = narrow_extension_candidates_by_path(&ext_candidates, path_platform) {
                        return vec![narrowed];
                    }
                }
            }
            return ext_candidates;
        }
    }

    if path_heuristics {
        if let Some(platform) = infer_platform_from_parent_path_heuristics(path) {
            return vec![platform];
        }
    }

    Vec::new()
}

// ---------------------------------------------------------------------------
// Candidate narrowing and the detection cycle as separate, public steps (for
// `operation::Single`, which runs them explicitly: forced? -> extension -> path -> cycle).
// `read_metadata_for_input` above is the same flow in one call, unchanged.
// ---------------------------------------------------------------------------

/// Extension heuristic: every platform `path`'s extension can belong to, or empty when the
/// extension says nothing (a directory, an unknown extension). Audio containers keep their
/// full list on purpose (see [`merged_heuristic_cycle_candidates`]).
pub fn candidates_from_extension(path: &Path) -> Vec<Platform> {
    infer_platforms_from_extension_heuristics(&extension_lowercase(path))
}

/// Path heuristic, applied to what the extension step produced: narrows `candidates` to the
/// one the parent directory names (or the compatible one), keeps them as they are when the
/// path names nothing that fits, and supplies the path's platform alone when `candidates`
/// is empty. An audio container's list is never narrowed.
pub fn narrow_candidates_by_path(path: &Path, candidates: Vec<Platform>) -> Vec<Platform> {
    let ext = extension_lowercase(path);
    let Some(path_platform) = infer_platform_from_parent_path_heuristics(path) else {
        return candidates;
    };
    if candidates.is_empty() {
        return vec![path_platform];
    }
    if candidates.len() == 1 || is_audio_container_extension(&ext) {
        return candidates;
    }
    match narrow_extension_candidates_by_path(&candidates, path_platform) {
        Some(narrowed) => vec![narrowed],
        None => candidates,
    }
}

/// The candidates the cycle should walk: `[forced]` when a platform is forced (no
/// heuristics run), otherwise the extension step (if `flags.ext`) narrowed by the path step
/// (if `flags.path`). Empty means "no idea -- cycle everything".
pub fn resolve_candidates(path: &Path, forced: Option<Platform>, flags: HeuristicFlags) -> Vec<Platform> {
    if let Some(forced) = forced {
        return vec![forced];
    }
    merged_heuristic_cycle_candidates(path, flags.ext, flags.path)
}

/// What one platform's method said about the input during [`metadata_cycle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CycleStep {
    /// Produced title/serial: the cycle stopped here.
    Matched,
    /// Checked the content and it is not this platform (dropped from `remaining`).
    Rejected,
    /// Nothing learned either way.
    Inconclusive,
}

/// Result of [`metadata_cycle`].
#[derive(Debug, Clone)]
pub struct CycleOutcome {
    /// The platform that matched, or `Unknown` when none did.
    pub platform: Platform,
    /// The candidates still possible: the input list minus every platform whose method
    /// rejected the input (the whole input list, in cycle order, when it was empty).
    pub remaining: Vec<Platform>,
    /// The matched platform's metadata, or the heuristic metadata with `platform` left as
    /// `Unknown` when nothing matched.
    pub metadata: RomMetadata,
    /// Every platform tried, in order, with its verdict.
    pub trail: Vec<(Platform, CycleStep)>,
}

/// The "large cycle": runs each candidate platform's own detection/metadata method in cycle
/// order, skipping every platform not in `candidates` (an empty list means all platforms),
/// and stops at the first that finds real content evidence. Platforms whose method
/// positively rejects the input (see [`platforms::rejects_on_miss`]) are removed from
/// `remaining`, so a caller that ends up with `Unknown` still gets a shorter list. Same
/// acceptance rules as `read_metadata_for_input`'s mode 4 (audio containers need a serial to
/// be decisive; otherwise title or serial).
pub fn metadata_cycle(path: &Path, candidates: Vec<Platform>, flags: HeuristicFlags) -> CycleOutcome {
    let input_file_size = fs::metadata(path).ok().map(|m| m.len());
    let ext = extension_lowercase(path);
    let audio_ext = is_audio_container_extension(&ext);
    let cycle = if candidates.is_empty() { full_deep_cycle_platforms(&ext, &[]) } else { order_candidates_for_cycle(&ext, &candidates) };
    let mut remaining = cycle.clone();
    let mut trail = Vec::with_capacity(cycle.len());
    let mut best_audio_generic: Option<RomMetadata> = None;
    let mut best_audio_score = 0usize;

    for platform in cycle {
        let Some(meta) = platforms::deep_try_platform(path, &ext, input_file_size, platform, flags) else {
            let step = if platforms::rejects_on_miss(path, &ext, platform) {
                remaining.retain(|p| *p != platform);
                CycleStep::Rejected
            } else {
                CycleStep::Inconclusive
            };
            trail.push((platform, step));
            continue;
        };

        if audio_ext && !audio_platform_specific_evidence(&meta) {
            if audio_generic_evidence(&meta) {
                let score = metadata_score(&meta);
                if score > best_audio_score {
                    best_audio_score = score;
                    best_audio_generic = Some(meta);
                }
            }
            trail.push((platform, CycleStep::Inconclusive));
            continue;
        }
        if !audio_ext && !has_content_evidence(&meta) {
            trail.push((platform, CycleStep::Inconclusive));
            continue;
        }
        let mut selected = meta;
        let score = metadata_score(&selected);
        push_detection_reason(&mut selected, DetectionStepKind::DeepCycle, Evidence::Note(format!("selected {platform:?} with score {score}")));
        trail.push((platform, CycleStep::Matched));
        let selected = finalize_metadata(path, normalize_metadata(selected), RomReason::DeepCycle);
        return CycleOutcome { platform: selected.platform, remaining, metadata: selected, trail };
    }

    let mut metadata = match best_audio_generic {
        Some(mut meta) => {
            meta.platform = Platform::Unknown;
            push_detection_reason(
                &mut meta,
                DetectionStepKind::DeepCycle,
                Evidence::Note("kept best audio generic evidence but platform remains unknown".to_string()),
            );
            meta
        }
        None => enrich_heuristic_metadata(path, metadata_from_heuristics(path, input_file_size, flags.ext, flags.path)),
    };
    metadata.platform = Platform::Unknown;
    metadata.candidates = remaining.clone();
    let metadata = finalize_metadata(path, normalize_metadata(metadata), infer_metadata_reason(path, flags.ext, flags.path));
    CycleOutcome { platform: Platform::Unknown, remaining, metadata, trail }
}

/// The platform a hint should actually be tried as, or `None` when the file's extension
/// rules it out. Same rule the file-level path heuristic already follows
/// ([`narrow_extension_candidates_by_path`]): a hint only ever narrows among the
/// extension's own candidates (or a format-compatible sibling), never overrides a clear
/// extension -- a stray `.gba` under a PlayStation folder is a misplaced file, not a
/// PlayStation game, and trying the hint anyway would risk the generic disc title fallback
/// "matching" it. With the extension heuristic off, or no known extension (directories,
/// extensionless dumps), there is nothing to contradict the hint.
fn plausible_hint(path: &Path, hint: Platform, flags: HeuristicFlags) -> Option<Platform> {
    if hint == Platform::Unknown {
        return None;
    }
    if !flags.ext {
        return Some(hint);
    }
    let ext_candidates = infer_platforms_from_extension_heuristics(&extension_lowercase(path));
    if ext_candidates.is_empty() {
        return Some(hint);
    }
    narrow_extension_candidates_by_path(&ext_candidates, hint)
}

/// [`resolve_candidates`] + [`metadata_cycle`] with an optional platform hint
/// (`MetadataArgs::preferred_platform`): a plausible hint (see [`plausible_hint`]) is tried
/// first, on its own; real content evidence settles it ([`RomReason::Hint`]), anything
/// else falls back to exactly the unhinted detection, without re-trying the hint. Unlike
/// a forced platform, a wrong hint (a mislabeled folder) costs one parser attempt, never a
/// wrong answer.
pub fn hinted_metadata_cycle(path: &Path, hint: Option<Platform>, flags: HeuristicFlags) -> CycleOutcome {
    let mut candidates = resolve_candidates(path, None, flags);
    let Some(tried) = hint.and_then(|hint| plausible_hint(path, hint, flags)) else {
        return metadata_cycle(path, candidates, flags);
    };

    let hinted = metadata_cycle(path, vec![tried], flags);
    if hinted.platform != Platform::Unknown {
        let mut metadata = hinted.metadata;
        push_detection_reason(&mut metadata, DetectionStepKind::Finalization, Evidence::Note(format!("hinted platform {tried:?} matched")));
        metadata.reason = RomReason::Hint;
        metadata.reason_string = format!("hinted platform {tried:?}");
        return CycleOutcome { metadata, ..hinted };
    }

    // An empty candidate list means "every platform" -- "all but the hint" isn't expressible,
    // so the hint gets re-tried once there: a deterministic repeat miss, not a behavior change.
    if candidates.is_empty() {
        return metadata_cycle(path, candidates, flags);
    }
    let hint_was_candidate = candidates.contains(&tried);
    candidates.retain(|platform| *platform != tried);
    if candidates.is_empty() {
        // The hint was the only candidate: the hinted attempt already *is* the unhinted cycle.
        return hinted;
    }
    let mut outcome = metadata_cycle(path, candidates, flags);
    if outcome.platform == Platform::Unknown && hint_was_candidate && hinted.remaining.contains(&tried) {
        // Inconclusive, not rejected: still possible, exactly as the unhinted cycle would report.
        outcome.remaining.insert(0, tried);
        outcome.metadata.candidates = outcome.remaining.clone();
    }
    outcome.trail.splice(0..0, hinted.trail);
    outcome
}

/// `platform` confirmed by its own signature check, or `None`.
///
/// Positive means the platform's parser found real evidence in this file: a cartridge
/// header/magic/checksum ([`DetectionStepKind::Header`]), a disc magic, a file in the disc's
/// filesystem, or a boot serial found in its bytes ([`DetectionStepKind::Descriptor`]). Not
/// positive: a platform with no signature check at all (its answer would only repeat the
/// guess), a parser that found nothing, and the generic fixed-offset disc title read, which
/// any printable bytes satisfy. Heuristics are off for the check, so a file's name or folder
/// can neither veto nor stand in for its content.
///
/// The returned metadata carries `platform` itself even when a format twin's parser did the
/// checking (SNES has no parser of its own; its Super Famicom twin's reads the same header).
pub fn confirm_platform(path: &Path, platform: Platform) -> Option<RomMetadata> {
    if platform == Platform::Unknown {
        return None;
    }
    let ext = extension_lowercase(path);
    let size = fs::metadata(path).ok().map(|m| m.len());
    let checked_as = match platform {
        Platform::Snes => Platform::SuperFamicom,
        other => other,
    };
    let mut meta = platforms::deep_try_platform(path, &ext, size, checked_as, HeuristicFlags::new(false, false))?;
    if !has_signature_evidence(&meta) {
        return None;
    }
    meta.platform = platform;
    meta.candidates = vec![platform];
    meta.reason = RomReason::Signature;
    meta.reason_string = format!("{platform:?} signature confirmed");
    push_detection_reason(&mut meta, DetectionStepKind::Finalization, Evidence::Note(format!("{platform:?} signature confirmed")));
    Some(meta)
}

/// Whether a parser's trail holds evidence found in the file itself (see [`confirm_platform`]).
fn has_signature_evidence(meta: &RomMetadata) -> bool {
    meta.detection_trail.iter().any(|step| match (&step.kind, &step.evidence) {
        (_, Evidence::Note(_)) => false,
        (DetectionStepKind::Header, _) => true,
        // A disc's text evidence comes from scanning raw bytes when the filesystem walk found
        // nothing, and the generic fixed-offset title/serial read lands there too -- any
        // printable bytes satisfy it. A real PlayStation/Xbox disc has its filesystem, which
        // the walk reports as `FileInFilesystem`; text alone confirms nothing.
        (DetectionStepKind::Descriptor, Evidence::Text { .. }) => false,
        (DetectionStepKind::Descriptor | DetectionStepKind::Inference, Evidence::Magic { .. } | Evidence::FileInFilesystem { .. } | Evidence::Verified { .. } | Evidence::Bytes { .. }) => true,
        _ => false,
    })
}

/// Ported verbatim from `reference/old_archiver.rs`'s `narrow_extension_candidates_by_path`
/// (line 2710).
pub fn narrow_extension_candidates_by_path(ext_candidates: &[Platform], path_platform: Platform) -> Option<Platform> {
    if ext_candidates.contains(&path_platform) {
        return Some(path_platform);
    }
    ext_candidates.iter().copied().find(|candidate| platform_compatible_for_cycle(path_platform, *candidate))
}

/// Whether a platform detected from content may stand in for the platform being tried --
/// covers the regional/revision pairs that share an identical ROM format (SNES/Super
/// Famicom, NES/Famicom, DS/DSi, 3DS/New 3DS, TG16/TG-CD, NGP/NGPC). Ported verbatim from
/// `reference/old_archiver.rs`'s `platform_compatible_for_cycle` (line 6386).
pub fn platform_compatible_for_cycle(detected: Platform, target: Platform) -> bool {
    if detected == Platform::Unknown || detected == target {
        return true;
    }

    matches!(
        (detected, target),
        (Platform::SuperFamicom, Platform::Snes)
            | (Platform::Snes, Platform::SuperFamicom)
            | (Platform::Nes, Platform::Famicom)
            | (Platform::Famicom, Platform::Nes)
            | (Platform::Nintendo3ds, Platform::NewNintendo3ds)
            | (Platform::NewNintendo3ds, Platform::Nintendo3ds)
            | (Platform::NintendoDs, Platform::NintendoDsi)
            | (Platform::NintendoDsi, Platform::NintendoDs)
            | (Platform::TurboGrafx16, Platform::TurboGrafxCd)
            | (Platform::TurboGrafxCd, Platform::TurboGrafx16)
            | (Platform::NeoGeoPocket, Platform::NeoGeoPocketColor)
            | (Platform::NeoGeoPocketColor, Platform::NeoGeoPocket)
    )
}

fn normalize_heuristic_token(value: &str) -> String {
    value.to_ascii_lowercase().chars().filter(|ch| ch.is_ascii_alphanumeric()).collect()
}

pub fn is_audio_container_extension(ext: &str) -> bool {
    matches!(ext, "wav" | "wv" | "flac" | "t77" | "cas")
}

/// Ported verbatim from `reference/old_archiver.rs`'s `infer_media_type_from_heuristics`
/// (line 6408).
pub fn infer_media_type_from_heuristics(ext: &str, platform: Platform) -> MediaType {
    match ext {
        "gdi" => MediaType::GdRom,
        // `cas` is added to old code's list (line 6411 there, which omitted it): a CAS file
        // is an MSX *cassette* image by definition, and both `is_audio_container_extension`
        // and the extension heuristic already treat it as one, so leaving it out here made
        // the media type disagree with the rest of detection.
        "wav" | "wv" | "flac" | "t77" | "cas" => MediaType::Tape,
        "d77" | "d88" | "88d" | "fdd" | "nfd" | "hdm" | "tfd" | "fdi" => MediaType::FloppyDisk,
        "scp" => MediaType::Raw,
        "xiso" | "xbox" | "xex" | "xbe" | "xgs" | "xbox360" | "xbox360iso" => MediaType::Dvd,
        "iso" | "rvz" | "nsp" | "xci" | "nsz" | "xcz" | "wud" | "wux" | "wad" | "gc" | "gcm" | "wii" | "ps3" | "ps2" => MediaType::Dvd,
        "chd" | "cso" | "cue" | "bin" | "img" | "raw" => match platform {
            Platform::Dreamcast => MediaType::GdRom,
            Platform::PocketStation => MediaType::Digital,
            Platform::PlayStation
            | Platform::PlayStation2
            | Platform::PlayStationPortable
            | Platform::SegaSaturn
            | Platform::SegaCd
            | Platform::TurboGrafx16
            | Platform::TurboGrafxCd
            | Platform::NeoGeoCd
            | Platform::ThreeDo
            | Platform::PcFx => MediaType::CdRom,
            Platform::PlayStation3 | Platform::GameCube | Platform::Wii | Platform::Xbox | Platform::Xbox360 => MediaType::Dvd,
            _ => MediaType::Unknown,
        },
        "nes" | "unf" | "unh" | "fds" | "qd" | "sfc" | "smc" | "st" | "fig" | "swc" | "gb" | "gbc" | "gba" | "nds" | "dsi"
        | "3ds" | "cia" | "cci" | "cxi" | "zcci" | "zcia" | "zcxi" | "md" | "gen" | "smd" | "sms" | "gg" | "sg" | "ngp" | "ngpc" | "ngc" | "pce" | "pcfx" | "sgx" | "ws"
        | "wsc" | "pico" | "v64" | "z64" | "rom" | "vb" | "32x" => MediaType::Cartridge,
        "pkg" | "vpk" | "pbp" => MediaType::Digital,
        _ => MediaType::Unknown,
    }
}

// Platform lists shared by several extensions (formerly `a | b` match arms). Candidate order
// inside each list is significant -- the deep cycle tries candidates in this order, so it
// decides ties -- and is preserved exactly from the original match.

const ISO_PLATFORMS: &[Platform] = &[
    Platform::PlayStation2,
    Platform::PlayStationPortable,
    Platform::PlayStation3,
    Platform::GameCube,
    Platform::Wii,
    Platform::Xbox,
    Platform::Xbox360,
    Platform::ThreeDo,
];

// GD-ROM dumps most commonly ship as .gdi, but .cue+.bin is a legitimate alternate encoding
// of the same disc -- Dreamcast belongs in this list for exactly the same reason every other
// CD-based platform here does. Missing from this list, an ambiguous .cue in a
// "dreamcast"-named directory couldn't be narrowed by the path heuristic (its narrowing only
// ever picks from *this* list), so the deep cycle fell through to its generic fixed-offset
// title fallback -- which isn't platform-specific and "matches" every candidate identically
// -- and picked whichever candidate happened to be tried first, not whichever the content
// actually was.
const CUE_PLATFORMS: &[Platform] = &[
    Platform::PlayStation,
    Platform::PlayStation2,
    Platform::SegaSaturn,
    Platform::SegaCd,
    Platform::TurboGrafxCd,
    Platform::NeoGeoCd,
    Platform::Dreamcast,
    Platform::ThreeDo,
    Platform::PcFx,
];

const BIN_PLATFORMS: &[Platform] = &[
    Platform::PlayStation,
    Platform::PlayStation2,
    Platform::SegaSaturn,
    Platform::SegaCd,
    Platform::TurboGrafxCd,
    Platform::NeoGeoCd,
    Platform::Dreamcast,
    Platform::ThreeDo,
    Platform::PcFx,
    Platform::Pc8801,
    Platform::Pc98,
    Platform::Fm7,
    Platform::FmTowns,
    Platform::Msx,
    Platform::Msx2,
    Platform::PocketStation,
];

const CHD_PLATFORMS: &[Platform] = &[
    Platform::PlayStation,
    Platform::PlayStation2,
    Platform::PlayStationPortable,
    Platform::Dreamcast,
    Platform::SegaSaturn,
    Platform::SegaCd,
    Platform::TurboGrafxCd,
    Platform::NeoGeoCd,
    Platform::Xbox,
    Platform::Xbox360,
    Platform::ThreeDo,
    Platform::PcFx,
];

const FLAC_PLATFORMS: &[Platform] =
    &[Platform::Msx, Platform::Msx2, Platform::Fm7, Platform::SegaCd, Platform::TurboGrafxCd, Platform::NeoGeoCd];
const TAPE_PLATFORMS: &[Platform] = &[Platform::Msx, Platform::Msx2, Platform::Fm7];
const NES: &[Platform] = &[Platform::Nes];
const FDS: &[Platform] = &[Platform::FamicomDiskSystem];
const SNES: &[Platform] = &[Platform::SuperFamicom];
const N64: &[Platform] = &[Platform::Nintendo64];
const NEO_GEO_POCKET_COLOR: &[Platform] = &[Platform::NeoGeoPocketColor];
const NEO_GEO: &[Platform] = &[Platform::NeoGeoAes];
const PC98: &[Platform] = &[Platform::Pc98];
const PC8801: &[Platform] = &[Platform::Pc8801];
const THREE_DS: &[Platform] = &[Platform::NewNintendo3ds, Platform::Nintendo3ds];
const GENESIS: &[Platform] = &[Platform::SegaGenesis, Platform::SegaPico];
const WIIU: &[Platform] = &[Platform::WiiU];
const SWITCH: &[Platform] = &[Platform::NintendoSwitch];

/// Extension (lowercase, no dot) -> candidate platforms, in candidate order. The one source of
/// truth for both directions: [`infer_platforms_from_extension_heuristics`] reads it forward,
/// [`extensions_for_platform`] in reverse, [`is_known_rom_extension`] as a membership test --
/// so the forward and reverse views can never drift apart.
///
/// Container formats whose platform comes from their members (zip, 7z) and descriptor/
/// playlist files resolved elsewhere (m3u) deliberately aren't here.
pub const EXTENSION_TABLE: &[(&str, &[Platform])] = &[
    ("gdi", &[Platform::Dreamcast]),
    ("key", WIIU),
    ("rvz", &[Platform::GameCube, Platform::Wii]),
    ("xiso", &[Platform::Xbox, Platform::Xbox360]),
    ("iso", ISO_PLATFORMS),
    ("cue", CUE_PLATFORMS),
    ("bin", BIN_PLATFORMS),
    ("raw", BIN_PLATFORMS),
    ("chd", CHD_PLATFORMS),
    ("cso", &[Platform::PlayStation, Platform::PlayStation2, Platform::PlayStationPortable]),
    ("nsp", SWITCH),
    ("xci", SWITCH),
    // The same packages compressed by nsz: PFS0 / HEAD outside, zstd inside.
    ("nsz", SWITCH),
    ("xcz", SWITCH),
    ("pkg", &[Platform::PlayStationVita, Platform::PlayStation3, Platform::PlayStationPortable, Platform::PspPs1Classic]),
    ("vpk", &[Platform::PlayStationVita]),
    // Vita game-card dumps: psvgamesd's `.psv`, or a raw image of the card.
    ("psv", &[Platform::PlayStationVita]),
    ("img", &[Platform::PlayStationVita]),
    ("pbp", &[Platform::PlayStationPortable, Platform::PspPs1Classic]),
    ("wud", WIIU),
    ("wux", WIIU),
    ("wua", WIIU),
    ("zar", &[Platform::Xbox360]),
    ("wad", &[Platform::Wii]),
    ("nes", NES),
    ("unf", NES),
    ("unh", NES),
    ("fds", FDS),
    ("qd", FDS),
    ("smc", SNES),
    ("sfc", SNES),
    ("fig", SNES),
    ("swc", SNES),
    ("st", &[Platform::SufamiTurbo]),
    ("32x", &[Platform::Sega32x]),
    ("vb", &[Platform::VirtualBoy]),
    ("mx1", &[Platform::Msx]),
    ("mx2", &[Platform::Msx2]),
    ("rom", &[Platform::Msx2, Platform::Msx, Platform::TurboGrafx16]),
    ("wav", TAPE_PLATFORMS),
    ("wv", TAPE_PLATFORMS),
    ("cas", TAPE_PLATFORMS),
    ("t77", &[Platform::Fm7]),
    ("scp", &[Platform::FmTowns, Platform::Pc98, Platform::Pc8801]),
    ("flac", FLAC_PLATFORMS),
    ("d77", &[Platform::Fm7]),
    ("d88", PC8801),
    ("88d", PC8801),
    ("fdd", PC98),
    ("nfd", PC98),
    ("hdm", PC98),
    ("tfd", PC98),
    ("fdi", PC98),
    ("hdi", PC98),
    ("nhd", PC98),
    ("gb", &[Platform::GameBoy]),
    ("gbc", &[Platform::GameBoyColor]),
    ("gba", &[Platform::GameBoyAdvance]),
    ("nds", &[Platform::NintendoDs]),
    ("dsi", &[Platform::NintendoDsi]),
    ("3ds", THREE_DS),
    ("cia", THREE_DS),
    ("cci", THREE_DS),
    ("cxi", THREE_DS),
    ("zcci", THREE_DS),
    ("zcia", THREE_DS),
    ("zcxi", THREE_DS),
    ("md", GENESIS),
    ("gen", GENESIS),
    ("smd", GENESIS),
    ("ndd", &[Platform::Nintendo64Dd]),
    ("sms", &[Platform::SegaMasterSystem]),
    ("gg", &[Platform::SegaGameGear]),
    ("sg", &[Platform::Sg1000]),
    ("v64", N64),
    ("z64", N64),
    ("n64", N64),
    ("pce", &[Platform::TurboGrafx16]),
    ("pcfx", &[Platform::PcFx]),
    ("sgx", &[Platform::SuperGrafx]),
    ("ws", &[Platform::WonderSwan]),
    ("wsc", &[Platform::WonderSwanColor]),
    ("pico", &[Platform::SegaPico]),
    ("ngp", &[Platform::NeoGeoPocket]),
    ("ngc", NEO_GEO_POCKET_COLOR),
    ("ngpc", NEO_GEO_POCKET_COLOR),
    ("neo", NEO_GEO),
    ("aes", NEO_GEO),
    ("mvs", NEO_GEO),
];

/// Candidate platforms for a lowercase extension, in candidate order (empty if unknown).
/// Ported from `reference/old_archiver.rs`'s `infer_platforms_from_extension_heuristics`
/// (line 2721); now a forward read of [`EXTENSION_TABLE`].
pub fn infer_platforms_from_extension_heuristics(ext: &str) -> Vec<Platform> {
    EXTENSION_TABLE.iter().find(|(key, _)| *key == ext).map(|(_, platforms)| platforms.to_vec()).unwrap_or_default()
}

/// Every lowercase extension [`EXTENSION_TABLE`] maps to `platform` -- the reverse of
/// [`infer_platforms_from_extension_heuristics`], used when a scan's platform is forced and
/// only that platform's files should be picked up. Empty for a platform with no extension of
/// its own (e.g. TurboGrafx-CD, reachable only through shared disc extensions -- which are
/// included, since `cue`/`bin`/`chd` do list it).
pub fn extensions_for_platform(platform: Platform) -> Vec<&'static str> {
    EXTENSION_TABLE.iter().filter(|(_, platforms)| platforms.contains(&platform)).map(|(key, _)| *key).collect()
}

/// Whether a lowercase extension belongs to any platform in [`EXTENSION_TABLE`].
pub fn is_known_rom_extension(ext: &str) -> bool {
    EXTENSION_TABLE.iter().any(|(key, _)| *key == ext)
}

/// Ported verbatim from `reference/old_archiver.rs`'s
/// `infer_platform_from_parent_path_heuristics` (line 2836). Order matters throughout: more
/// specific names must be tested before the shorter names they contain (e.g. "playstation3"
/// before "playstation", "neogeopocketcolor" before "neogeopocket").
pub fn infer_platform_from_parent_path_heuristics(path: &Path) -> Option<Platform> {
    let parent_segment = path.parent().and_then(|p| p.file_name()).and_then(|s| s.to_str()).unwrap_or("");
    let raw = parent_segment.to_ascii_lowercase();
    let token = normalize_heuristic_token(parent_segment);

    if token.contains("playstation3") || token == "ps3" {
        return Some(Platform::PlayStation3);
    }
    if token.contains("playstation2") || token == "ps2" {
        return Some(Platform::PlayStation2);
    }
    if token.contains("playstationportable") || token == "psp" {
        return Some(Platform::PlayStationPortable);
    }
    if token.contains("playstationvita") || token == "psvita" || token == "vita" || token == "psp2" {
        return Some(Platform::PlayStationVita);
    }
    if token.contains("playstation") || token == "psx" || token == "ps1" {
        return Some(Platform::PlayStation);
    }

    if raw.contains("xbox360") || token == "xbox360" || token == "360" {
        return Some(Platform::Xbox360);
    }
    // Before the bare "xbox" test, which these all contain.
    if token.contains("xboxseries") || token == "xbsx" || token == "xbss" {
        return Some(Platform::XboxSeries);
    }
    if token.contains("xboxone") || token == "xone" || token == "xbone" {
        return Some(Platform::XboxOne);
    }
    if raw.contains("xbox") || token == "xbox" {
        return Some(Platform::Xbox);
    }

    if token.contains("newnintendo3ds") || token == "new3ds" || token == "n3ds" {
        return Some(Platform::NewNintendo3ds);
    }
    if token.contains("nintendo3ds") || token == "3ds" {
        return Some(Platform::Nintendo3ds);
    }

    if token.contains("nintendodsi") || token == "dsi" {
        return Some(Platform::NintendoDsi);
    }
    if token.contains("nintendods") || token == "nds" {
        return Some(Platform::NintendoDs);
    }

    if token.contains("superfamicom") || token.contains("supernintendo") || token == "snes" || token == "sfc" {
        return Some(Platform::SuperFamicom);
    }
    if token.contains("sufami") || token.contains("sufamiturbo") {
        return Some(Platform::SufamiTurbo);
    }
    if token.contains("famicomdisk") || token == "fds" {
        return Some(Platform::FamicomDiskSystem);
    }
    if token.contains("famicom") || token.contains("familycomputer") || token == "nes" || token == "fc" {
        return Some(Platform::Nes);
    }

    if raw.contains("turbografxcd")
        || raw.contains("pcenginecd")
        || token.contains("turbografxcd")
        || token.contains("pcenginecd")
        || token == "pcecd"
    {
        return Some(Platform::TurboGrafxCd);
    }
    if raw.contains("turbografx")
        || raw.contains("pcengine")
        || raw.contains("pce")
        || token.contains("turbografx")
        || token.contains("pcengine")
        || token.contains("pce")
    {
        return Some(Platform::TurboGrafx16);
    }

    if token.contains("neogeocd") || token == "ngcd" {
        return Some(Platform::NeoGeoCd);
    }
    if token.contains("neogeopocketcolor") || token == "ngpc" {
        return Some(Platform::NeoGeoPocketColor);
    }
    if token.contains("neogeopocket") || token == "ngp" {
        return Some(Platform::NeoGeoPocket);
    }

    if token.contains("gameboycolor") || token == "gbc" {
        return Some(Platform::GameBoyColor);
    }
    if token.contains("gameboyadvance") || token == "gba" {
        return Some(Platform::GameBoyAdvance);
    }
    if token.contains("gameboy") || token == "gb" {
        return Some(Platform::GameBoy);
    }

    if token.contains("segagenesis") || token == "genesis" || token == "megadrive" || token == "md" {
        return Some(Platform::SegaGenesis);
    }
    if token.contains("segapico") || token == "pico" {
        return Some(Platform::SegaPico);
    }
    if token.contains("segamastersystem") || token == "mastersystem" || token == "sms" {
        return Some(Platform::SegaMasterSystem);
    }
    if token.contains("segagamegear") || token == "gamegear" || token == "gg" {
        return Some(Platform::SegaGameGear);
    }
    if token == "sg1000" {
        return Some(Platform::Sg1000);
    }

    if token.contains("segasaturn") || token == "saturn" {
        return Some(Platform::SegaSaturn);
    }
    if token.contains("segacd") || token == "megacd" {
        return Some(Platform::SegaCd);
    }
    if token.contains("dreamcast") || token == "dc" {
        return Some(Platform::Dreamcast);
    }
    if token.contains("gamecube") || token == "gc" {
        return Some(Platform::GameCube);
    }
    if raw.contains("wiiu") || token == "wiu" || token.contains("wiiu") {
        return Some(Platform::WiiU);
    }
    if token.contains("supergrafx") || token == "sgx" {
        return Some(Platform::SuperGrafx);
    }
    if token.contains("wonderswancolor") || token == "wsc" || token == "swancolor" {
        return Some(Platform::WonderSwanColor);
    }
    if token.contains("wonderswan") || token == "ws" || token == "swan" {
        return Some(Platform::WonderSwan);
    }
    if token == "wii" {
        return Some(Platform::Wii);
    }

    if token.contains("nintendo64dd") || raw.contains("n64dd") || token == "ndd" {
        return Some(Platform::Nintendo64Dd);
    }
    if token.contains("nintendo64") || token == "n64" {
        return Some(Platform::Nintendo64);
    }
    if raw.contains("virtualboy") || raw.contains("virtual boy") || token.contains("virtualboy") || token == "vb" {
        return Some(Platform::VirtualBoy);
    }
    if token.contains("pocketstation") || token == "pocket" {
        return Some(Platform::PocketStation);
    }
    if raw.contains("msxturbo") || raw.contains("msx-turbo") || token.contains("msxturbo") || token == "msxt" {
        return Some(Platform::MsxTurbo);
    }
    if raw.contains("msx2plus") || raw.contains("msx2+") || token.contains("msx2plus") || token == "msx2+" {
        return Some(Platform::Msx2Plus);
    }
    if token == "msx2" {
        return Some(Platform::Msx2);
    }
    if token == "msx" {
        return Some(Platform::Msx);
    }
    if token == "pc88" || token == "pc8801" {
        return Some(Platform::Pc8801);
    }
    if token == "pc98" || token == "pc9801" {
        return Some(Platform::Pc98);
    }
    if token == "fm7" {
        return Some(Platform::Fm7);
    }
    if token == "fmtowns" {
        return Some(Platform::FmTowns);
    }

    None
}

// ---------------------------------------------------------------------------
// Deep cycle (mode 4)
// ---------------------------------------------------------------------------

/// Whether a candidate read actually recovered identifying content.
///
/// Deliberately *not* `metadata_score(&meta) > 0`, which is what
/// `reference/old_archiver.rs` tested (line 3038). That score counts `platform` and
/// `media_type`, but the cycle fills both in from the candidate it was handed -- so every
/// candidate scores at least 2 whether or not it read anything, and the cycle would always
/// accept whichever platform it happened to try first. For an unidentifiable disc image
/// that meant confidently reporting, say, GameCube purely because GameCube leads the cycle
/// order. Only a title or a serial comes from the file itself, so only those count as
/// evidence -- the same test the mode-2/3 branch of [`read_metadata_for_input`] already
/// applies.
///
/// [`metadata_score`] keeps its own job: ranking several *candidate reads* against each
/// other, where those fields do differ.
fn has_content_evidence(meta: &RomMetadata) -> bool {
    meta.title.is_some() || meta.serial_product_id.is_some()
}

fn audio_platform_specific_evidence(meta: &RomMetadata) -> bool {
    meta.serial_product_id.is_some()
}

fn audio_generic_evidence(meta: &RomMetadata) -> bool {
    meta.title.is_some() || audio_platform_specific_evidence(meta)
}

/// Walks the narrowed candidate list, running each platform's own parser until one produces
/// usable metadata. Ported from `reference/old_archiver.rs`'s `run_metadata_cycle` (line
/// 3006), minus `strict_metadata_mode`.
///
/// Audio containers get special handling: a tape dump's container format says nothing about
/// which machine it's for, so a mere title match isn't decisive -- the cycle keeps looking
/// for platform-specific evidence (a serial), and only if none of the candidates produces
/// that does it fall back to the best generic match, with the platform left `Unknown`.
fn run_metadata_cycle(
    path: &Path,
    ext: &str,
    input_file_size: Option<u64>,
    narrowed_candidates: &[Platform],
    flags: HeuristicFlags,
) -> Option<RomMetadata> {
    let audio_ext = is_audio_container_extension(ext);
    let cycle = if narrowed_candidates.is_empty() {
        full_deep_cycle_platforms(ext, &[])
    } else {
        order_candidates_for_cycle(ext, narrowed_candidates)
    };

    let mut best_audio_generic: Option<RomMetadata> = None;
    let mut best_audio_score = 0usize;

    for platform in cycle {
        let Some(meta) = platforms::deep_try_platform(path, ext, input_file_size, platform, flags) else {
            continue;
        };

        if audio_ext {
            if audio_platform_specific_evidence(&meta) {
                let mut selected = meta;
                push_detection_reason(
                    &mut selected,
                    DetectionStepKind::DeepCycle,
                    Evidence::Note(format!("selected {platform:?} with audio-specific evidence")),
                );
                return Some(selected);
            }
            if audio_generic_evidence(&meta) {
                let score = metadata_score(&meta);
                if score > best_audio_score {
                    best_audio_score = score;
                    best_audio_generic = Some(meta);
                }
            }
            continue;
        }

        if has_content_evidence(&meta) {
            let mut selected = meta;
            let score = metadata_score(&selected);
            push_detection_reason(
                &mut selected,
                DetectionStepKind::DeepCycle,
                Evidence::Note(format!("selected {platform:?} with score {score}")),
            );
            return Some(selected);
        }
    }

    if let Some(mut meta) = best_audio_generic {
        meta.platform = Platform::Unknown;
        push_detection_reason(
            &mut meta,
            DetectionStepKind::DeepCycle,
            Evidence::Note("kept best audio generic evidence but platform remains unknown".to_string()),
        );
        return Some(meta);
    }

    None
}

/// The full platform cycle order, disc-first or cartridge-first depending on what the
/// extension/candidates hint at. Ported verbatim from `reference/old_archiver.rs`'s
/// `full_deep_cycle_platforms` (line 3076).
fn full_deep_cycle_platforms(ext: &str, candidates: &[Platform]) -> Vec<Platform> {
    let disc_first = is_disc_hint(ext, candidates);
    let disc = if ext == "chd" {
        vec![
            Platform::SegaCd,
            Platform::SegaSaturn,
            Platform::TurboGrafxCd,
            Platform::NeoGeoCd,
            Platform::Dreamcast,
            Platform::PlayStation,
            Platform::PlayStation2,
            Platform::PlayStationPortable,
            Platform::PlayStation3,
            Platform::GameCube,
            Platform::Wii,
            Platform::WiiU,
            Platform::Xbox360,
            Platform::Xbox,
            Platform::ThreeDo,
            Platform::PcFx,
        ]
    } else {
        vec![
            Platform::GameCube,
            Platform::Wii,
            Platform::WiiU,
            Platform::PlayStation2,
            Platform::PlayStation3,
            Platform::PlayStationPortable,
            Platform::PlayStation,
            Platform::Dreamcast,
            Platform::SegaSaturn,
            Platform::SegaCd,
            Platform::TurboGrafxCd,
            Platform::NeoGeoCd,
            Platform::Xbox360,
            Platform::Xbox,
            Platform::ThreeDo,
            Platform::PcFx,
        ]
    };

    let non_disc = vec![
        Platform::GameBoy,
        Platform::GameBoyColor,
        Platform::GameBoyAdvance,
        Platform::VirtualBoy,
        Platform::NintendoDsi,
        Platform::NintendoDs,
        Platform::NewNintendo3ds,
        Platform::Nintendo3ds,
        Platform::Nintendo64,
        Platform::Nintendo64Dd,
        Platform::SuperFamicom,
        Platform::SufamiTurbo,
        Platform::Nes,
        Platform::Famicom,
        Platform::FamicomDiskSystem,
        Platform::Sega32x,
        Platform::SegaGenesis,
        Platform::SegaPico,
        Platform::SegaMasterSystem,
        Platform::SegaGameGear,
        Platform::Sg1000,
        Platform::NeoGeoPocketColor,
        Platform::NeoGeoPocket,
        Platform::PocketStation,
        Platform::PlayStationVita,
        Platform::NintendoSwitch,
        Platform::WonderSwan,
        Platform::WonderSwanColor,
        Platform::SuperGrafx,
        Platform::WiiU,
        Platform::Msx,
        Platform::Msx2,
        Platform::MsxTurbo,
        Platform::Msx2Plus,
        Platform::Pc8801,
        Platform::Pc98,
        Platform::Fm7,
        Platform::FmTowns,
        Platform::TurboGrafx16,
    ];

    if disc_first { [disc, non_disc].concat() } else { [non_disc, disc].concat() }
}

/// Restricts the full cycle order to just the narrowed candidates, preserving the cycle's
/// own ordering. Ported verbatim from `reference/old_archiver.rs`'s
/// `order_candidates_for_cycle` (line 3162).
fn order_candidates_for_cycle(ext: &str, candidates: &[Platform]) -> Vec<Platform> {
    let ordered = full_deep_cycle_platforms(ext, candidates);
    let mut result = Vec::new();
    for platform in ordered {
        if candidates.contains(&platform) && !result.contains(&platform) {
            result.push(platform);
        }
    }
    result
}

/// Ported verbatim from `reference/old_archiver.rs`'s `is_disc_hint` (line 3173).
pub fn is_disc_hint(ext: &str, candidates: &[Platform]) -> bool {
    if matches!(ext, "chd" | "rvz" | "iso" | "cue" | "gdi" | "bin" | "raw" | "cso" | "xiso") {
        return true;
    }
    candidates.iter().any(|platform| {
        matches!(
            platform,
            Platform::PlayStation
                | Platform::PlayStation2
                | Platform::PlayStation3
                | Platform::PlayStationPortable
                | Platform::Dreamcast
                | Platform::GameCube
                | Platform::Wii
                | Platform::SegaSaturn
                | Platform::SegaCd
                | Platform::TurboGrafxCd
                | Platform::NeoGeoCd
                | Platform::Xbox
                | Platform::Xbox360
                | Platform::ThreeDo
                | Platform::PcFx
        )
    })
}

#[cfg(test)]
mod extension_table_tests {
    use super::*;

    #[test]
    fn every_extension_key_is_unique_and_lowercase() {
        let mut seen = std::collections::HashSet::new();
        for (key, platforms) in EXTENSION_TABLE {
            assert!(seen.insert(*key), "duplicate extension key {key:?}");
            assert_eq!(*key, key.to_ascii_lowercase(), "extension keys are matched lowercase");
            assert!(!platforms.is_empty(), "{key:?} maps to no platform");
        }
    }

    #[test]
    fn reverse_lookup_round_trips_against_forward() {
        for (key, platforms) in EXTENSION_TABLE {
            for platform in *platforms {
                assert!(extensions_for_platform(*platform).contains(key), "{platform:?} should list {key:?}");
            }
        }
        for ext in extensions_for_platform(Platform::Dreamcast) {
            assert!(infer_platforms_from_extension_heuristics(ext).contains(&Platform::Dreamcast));
        }
    }

    #[test]
    fn shared_disc_extensions_reach_platforms_without_their_own() {
        // TurboGrafx-CD has no extension of its own; it must still be reachable.
        let exts = extensions_for_platform(Platform::TurboGrafxCd);
        for ext in ["cue", "bin", "chd"] {
            assert!(exts.contains(&ext), "{ext} missing for TurboGrafxCd: {exts:?}");
        }
    }

    #[test]
    fn containers_and_playlists_are_not_rom_extensions() {
        for ext in ["zip", "7z", "m3u", "txt", ""] {
            assert!(!is_known_rom_extension(ext), "{ext:?}");
            assert!(infer_platforms_from_extension_heuristics(ext).is_empty(), "{ext:?}");
        }
        assert!(is_known_rom_extension("cue"));
        assert!(is_known_rom_extension("gba"));
    }
}
