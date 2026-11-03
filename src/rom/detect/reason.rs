//! Detection confidence tier + structured audit trail, used by
//! [`super::super::models::RomMetadata`].
//!
//! Adapted from `reference/old_archiver.rs`'s `MetadataFinalizationMethod` (3 variants:
//! Ext/Path/Deep) and `MetadataDetectionReasonKind` (8 variants) -- renamed and reshaped
//! for the new architecture rather than copied verbatim, but the underlying capability
//! (which of the 4 modes in docs/plan/rom_module.md resolved this metadata, plus a
//! capped step-by-step audit trail of how) is the same. [`Evidence`] is new: old code's
//! trail entries were a bare `String`, so "why" always read as a fixed message like
//! "non-disc header metadata matched for target GameBoyAdvance" -- true, but not actually a
//! reason. [`Evidence`] is what a leaf parser *found*: a magic number, an ASCII field, a
//! named file inside a filesystem, a byte range read for a value that varies per dump --
//! each variant carries the real bytes/text/offsets the specific ROM produced, not a
//! description of where the parser *looked*.

use std::fmt;

/// How [`super::super::models::RomMetadata`] was finalized -- doubles as a confidence
/// tier (roughly `Forced` > `PathHeuristic`/`ExtensionHeuristic` > `DeepCycle`, since a
/// forced platform is caller-asserted ground truth while a deep-cycle match is inferred
/// from partial/ambiguous content). Matches rom_module.md's "4 modes". This is *not*
/// itself "the reason" a platform was chosen -- it only says which mode resolved it; see
/// [`DetectionStep::evidence`] for what was actually found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RomReason {
    /// Mode 1: `forced_platform` was set, short-circuiting all heuristics/cycling.
    Forced,
    /// A caller-supplied platform hint (`MetadataArgs::preferred_platform`, e.g. the platform
    /// directory a scan found the file under) was tried first and its parser found real
    /// content evidence. Same trust level as [`Self::PathHeuristic`] -- the hint is only
    /// ever tried when the file's extension admits it, exactly as the file-level path
    /// heuristic narrows -- just sourced from further up the tree than the file's
    /// immediate parent directory.
    Hint,
    /// Mode 2: resolved by parent-path heuristic alone (extension heuristic was off, or
    /// agreed without narrowing further).
    PathHeuristic,
    /// Mode 3: resolved by file-extension heuristic alone (possibly narrowed by path
    /// heuristic to a single candidate, without needing the deep cycle).
    ExtensionHeuristic,
    /// Mode 4: resolved via `run_metadata_cycle` actually reading file content.
    DeepCycle,
    /// The platform's own signature check was positive ([`super::confirm_platform`]): a
    /// header/magic/checksum, or a file in the disc's filesystem, actually found in this
    /// file. The strictest tier -- what a scan requires before it records a platform.
    Signature,
}

/// Which stage of detection produced one [`DetectionStep`] entry. Mirrors
/// `MetadataDetectionReasonKind` from `reference/old_archiver.rs` (line ~2508 onward).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DetectionStepKind {
    ExtensionHeuristic,
    PathHeuristic,
    /// Extension heuristic's candidate list was narrowed down using the path heuristic's
    /// single candidate.
    ExtensionPathNarrow,
    /// A cartridge/disc header was read and matched.
    Header,
    /// A `.cue`/`.gdi` descriptor (or an archive member picked via one) supplied the
    /// answer.
    Descriptor,
    /// The deep cycle tried this platform (regardless of whether it matched).
    DeepCycle,
    /// A value was backfilled from another already-known field (e.g. NeoGeo CD's game ID
    /// inferred from a sibling path once the platform is already known).
    Inference,
    /// The final normalize/sanitize/score pass.
    Finalization,
}

/// The actual thing a parser found, with the *real* runtime bytes/text/offsets for this
/// specific dump -- not a description of what it checked. `Display` renders each as prose
/// ("magic bytes 4E 45 53 1A at 0x00", "bytes 03 from offset 0x12-0x13 (unit code)", "has file
/// SYSTEM.CNF in ISO9660 filesystem"), filling in whatever this platform/ROM actually
/// produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Evidence {
    /// A fixed magic number matched at one offset (the value itself never varies by ROM --
    /// only whether it matched).
    Magic { offset: usize, bytes: Vec<u8> },
    /// An ASCII/text field read from a byte range, its content genuinely specific to this
    /// dump (a title, a serial, a console-name string).
    Ascii { offset_start: usize, offset_end: usize, text: String },
    /// A structural signature (a boot logo, a checksum/complement pair) that was computed and
    /// matched over a byte range. Used instead of [`Evidence::Bytes`] where the range is too
    /// long to print as hex, so what is shown is what was verified, not a truncated dump.
    Verified { offset_start: usize, offset_end: usize, description: String },
    /// A byte range whose specific value (not just its presence) matters and varies by
    /// dump -- e.g. which of N64's three byte-order patterns this file actually has.
    Bytes { offset_start: usize, offset_end: usize, bytes: Vec<u8>, note: String },
    /// A named file located inside a filesystem walk (ISO9660, XDVDFS). `field` names the
    /// specific field/line read out of that file when the file's mere presence wasn't the
    /// whole check (e.g. SYSTEM.CNF's `BOOT=`/`BOOT2=` line and the serial it yielded) --
    /// `None` when the file's presence alone (PARAM.SFO, DEFAULT.XBE) was the check.
    FileInFilesystem { filesystem: &'static str, path: String, field: Option<String> },
    /// Text actually matched (a title, a serial, a scanned marker string) with no single
    /// fixed byte offset to name -- a backward/forward scan, a value assembled from a
    /// variable-length field, or a value whose parser doesn't track the offset it came
    /// from.
    Text { text: String, context: String },
    /// A shape/size-based check with no single fixed offset to name (headerless dumps,
    /// opcode/size heuristics).
    Shape { description: String },
    /// Bookkeeping from the heuristic stages (candidate narrowing, finalization) -- not
    /// byte-level evidence, so it stays prose rather than being forced into a shape that
    /// doesn't fit.
    Note(String),
}

impl fmt::Display for Evidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Magic { offset, bytes } => write!(f, "magic bytes {} at 0x{offset:X}", hex(bytes)),
            Self::Ascii { offset_start, offset_end, text } => write!(f, "ASCII '{text}' at offset 0x{offset_start:X}-0x{offset_end:X}"),
            Self::Verified { offset_start, offset_end, description } => {
                write!(f, "{description} verified at offset 0x{offset_start:X}-0x{offset_end:X}")
            }
            Self::Bytes { offset_start, offset_end, bytes, note } => {
                write!(f, "bytes {} from offset 0x{offset_start:X}-0x{offset_end:X} ({note})", hex(bytes))
            }
            Self::FileInFilesystem { filesystem, path, field: None } => write!(f, "has file {path} in {filesystem} filesystem"),
            Self::FileInFilesystem { filesystem, path, field: Some(field) } => {
                write!(f, "has file {path} in {filesystem} filesystem, {field}")
            }
            Self::Text { text, context } => write!(f, "text '{text}' ({context})"),
            Self::Shape { description } => f.write_str(description),
            Self::Note(text) => f.write_str(text),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
}

/// One entry in [`super::super::models::RomMetadata`]'s detection audit trail.
#[derive(Debug, Clone)]
pub struct DetectionStep {
    pub kind: DetectionStepKind,
    pub evidence: Evidence,
}

/// Max entries kept in `RomMetadata::detection_trail`, matching
/// `reference/old_archiver.rs`'s `normalize_metadata` truncation (~line 6787-6789).
pub const DETECTION_TRAIL_CAP: usize = 32;

impl DetectionStep {
    pub fn new(kind: DetectionStepKind, evidence: Evidence) -> Self {
        Self { kind, evidence }
    }
}
