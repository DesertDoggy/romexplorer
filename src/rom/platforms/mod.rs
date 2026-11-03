//! Per-platform metadata extraction ("deep try"), dispatched by family.
//!
//! Replaces `reference/old_archiver.rs`'s `define_deep_try!` macro (line 4968) and the ~25
//! trivial one-line wrappers it generated, plus `deep_try_platform` (3197): a single match
//! in [`deep_try_platform`] routes each platform either to the shared disc path
//! ([`deep_try_disc_platform`]) or to the shared cartridge/container path
//! ([`deep_try_generic_platform`]), which in turn dispatches to this module's family
//! submodules for the actual header parsing.
//!
//! **Note on Sony/Microsoft:** the plan sketched a `sony.rs` holding PARAM.SFO and a
//! `microsoft.rs` holding XBE/XEX. Those parsers instead live in
//! `super::detect::disc_dispatch`, because they are disc-*content* parsers that
//! `disc_metadata_from_bytes` dispatches to directly -- splitting them out would have meant
//! a module that exists only to be called from one match arm in another module. Xbox is
//! disc-only, so it needs no file here at all; [`sony`] keeps only the PS Vita PKG parser,
//! which is the one Sony format that isn't a disc.

pub mod download;
pub mod helpers;
pub mod mycom;
pub mod nintendo;
pub mod other;
pub mod sega;
pub mod sony;
pub mod switch;

use std::fs;
use std::path::{Path, PathBuf};

use crate::common::models::{ArchiveFormat, MediaType, Platform};

use super::models::{metadata_score, normalize_metadata, push_detection_reason, RomByteOrder, RomMetadata};
use super::detect::disc_fs::read_exact_at;
use super::detect::reason::{DetectionStepKind, Evidence};
use super::detect::{
    self, descriptor, disc_dispatch, infer_media_type_from_heuristics, metadata_from_heuristics,
    platform_compatible_for_cycle, HeuristicFlags,
};

// ---------------------------------------------------------------------------
// Shared helpers used across family submodules
// ---------------------------------------------------------------------------

/// What a platform's own header parser found. Ported from
/// `reference/old_archiver.rs`'s `NonDiscHeaderMetadata` (line 5236).
#[derive(Debug, Clone, Default)]
pub struct NonDiscHeaderMetadata {
    pub title: Option<String>,
    pub serial: Option<String>,
    pub manufacturer_id: Option<String>,
    /// See [`RomMetadata::headered`]. Only ever `Some` for NES/Famicom and Super Famicom;
    /// every other leaf parser leaves this `None`.
    pub headered: Option<bool>,
    /// See [`RomMetadata::encrypted`]. Only ever `Some` for 3DS/New 3DS and DS/DSi.
    pub encrypted: Option<bool>,
    /// See [`RomMetadata::byte_order`]. Only ever `Some` for N64.
    pub byte_order: Option<RomByteOrder>,
    /// Absolute file offset (start, end) the product code / serial was actually read from,
    /// when that location isn't one of the format's fixed constants -- 3DS's NCCH product
    /// code sits at a per-file-computed offset (`ncch_offset + 0x150` for a direct NCSD
    /// read, `content_offset + 0x150` for a CIA's cert/ticket/TMD-walked content, and those
    /// two are almost never the same number even for a title's old-3DS/New-3DS sibling
    /// dumps) -- without this, evidence display would have to fall back to the
    /// relative-to-NCCH constant, which looks identical for every 3DS dump regardless of
    /// its real location. `None` for every leaf parser whose header offset is a true fixed
    /// constant.
    pub serial_offset: Option<(u64, u64)>,
    /// Absolute file offset of the header a parser matched when that location depends on the
    /// dump (Super Famicom's LoROM/HiROM/ExHiROM candidates plus copier bias).
    pub header_offset: Option<u64>,
    /// Ready-made evidence, for parsers whose signature is not tied to one fixed layout (the
    /// Mycom family's per-media signatures). Empty for every other parser.
    pub evidence: Vec<Evidence>,
    /// Whether this file's whole-content hash matched a known game in `convert::nes::FcDb`
    /// (loaded from `fcdb.bin` when present). See [`RomMetadata::hash_confirmed`] --
    /// carried through to there unchanged, the same way `headered`/`encrypted` are, plus
    /// also surfaced in the detection trail (see `header_evidence`'s Nes/Famicom arms). A
    /// hash match can be load-bearing rather than purely corroborating: when none of a
    /// file's shape/magic checks succeed on their own, `nintendo::read_nes_container_metadata`
    /// falls back to the database's own headered/headerless answer to set `headered`
    /// itself, rather than rejecting a real, hash-verified game outright just because its
    /// shape looks unusual. `None` when no hash check was attempted (every leaf parser but
    /// NES/Famicom's); `Some(false)` means the check ran and found no match -- not a
    /// rejection on its own, just "unconfirmed" (see `nintendo::read_nes_container_metadata`).
    pub hash_confirmed: Option<bool>,
}

impl NonDiscHeaderMetadata {
    /// Builds one from the `(title, serial, manufacturer_id)` tuple every leaf parser
    /// returns -- old code repeated this same closure at ~20 call sites.
    fn from_tuple(values: (Option<String>, Option<String>, Option<String>)) -> Self {
        Self { title: values.0, serial: values.1, manufacturer_id: values.2, ..Default::default() }
    }
}

/// Reads up to 4096 bytes from the start of a file for header parsing. Ported from
/// `reference/old_archiver.rs`'s `read_detection_prefix` (line 7981) minus its
/// `chd_native`/`rvz_native` arms (see `super::detect::descriptor` for why).
pub fn read_detection_prefix(path: &Path) -> Result<Vec<u8>, String> {
    const DETECTION_PREFIX_LEN: usize = 4096;
    let len = fs::metadata(path).map(|m| m.len() as usize).unwrap_or(DETECTION_PREFIX_LEN).min(DETECTION_PREFIX_LEN);
    if len == 0 {
        return Ok(Vec::new());
    }
    read_exact_at(path, 0, len)
}

/// Reads a fixed-width ASCII field, stopping at the first NUL or non-printable byte.
/// Ported verbatim from `reference/old_archiver.rs`'s `read_ascii_field` (line 6364).
pub fn read_ascii_field(bytes: &[u8]) -> Option<String> {
    let mut out = String::new();
    for &byte in bytes {
        if byte == 0 {
            break;
        }
        let ch = byte as char;
        if ch.is_ascii_graphic() || ch == ' ' {
            out.push(ch);
        } else {
            break;
        }
    }

    let text = out.trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

pub fn read_u16_be(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes(bytes.get(offset..offset + 2)?.try_into().ok()?))
}

pub fn read_u32_be(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?))
}

pub fn read_u64_be(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_be_bytes(bytes.get(offset..offset + 8)?.try_into().ok()?))
}

pub fn read_u24_be(bytes: &[u8], offset: usize) -> Option<u32> {
    Some((u32::from(*bytes.get(offset)?) << 16) | (u32::from(*bytes.get(offset + 1)?) << 8) | u32::from(*bytes.get(offset + 2)?))
}

pub fn align_up_u64(value: u64, alignment: u64) -> Option<u64> {
    if alignment == 0 {
        return Some(value);
    }
    let add = alignment.checked_sub(1)?;
    value.checked_add(add).map(|v| v & !add)
}

pub fn extension_lowercase(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

// NOTE ON EXTENSIONS: no leaf parser in this module's submodules looks at the file
// extension. A parser answers exactly one question -- "is this file *my* platform's, by its
// content?" -- and must answer it for a file with any extension, or none.
//
// Extension-based exclusion belongs solely to the extension heuristic
// (`detect::infer_platforms_from_extension_heuristics`), which narrows the candidate list
// *before* the cycle runs, and which the caller can switch off. Keeping it there means a
// misnamed dump is still identified (the cycle simply tries more candidates), and it means
// turning the heuristic off actually widens detection instead of leaving veto checks buried
// in the parsers.
//
// `reference/old_archiver.rs` did the opposite, hard-rejecting on extension inside
// `parse_sg1000_rom_shape` (line 6082), `parse_virtual_boy_rom_shape` (6114),
// `parse_wiiu_container_metadata_from_bytes` (5730), `parse_pce_hucard_metadata_from_bytes`
// (6137) and `parse_floppy_container_metadata_from_bytes` (5628) -- so those platforms were
// undetectable the moment a file was named wrong, whatever the heuristic settings said.

// ---------------------------------------------------------------------------
// Top-level dispatch
// ---------------------------------------------------------------------------

/// Runs `platform`'s own metadata extraction against `path`, returning `None` when this
/// file clearly isn't that platform's. Replaces `reference/old_archiver.rs`'s
/// `deep_try_platform` (line 3197) plus its `define_deep_try!`-generated wrappers.
pub fn deep_try_platform(
    path: &Path,
    ext: &str,
    input_file_size: Option<u64>,
    platform: Platform,
    flags: HeuristicFlags,
) -> Option<RomMetadata> {
    match platform {
        // Directory games and package files (PKG/PBP) are read by content, not as disc images.
        Platform::PlayStation3 | Platform::PlayStationPortable if download::is_download_input(path) => {
            deep_try_generic_platform(path, ext, input_file_size, platform, flags)
        }

        Platform::TurboGrafxCd
        | Platform::PlayStation
        | Platform::PlayStation2
        | Platform::PlayStation3
        | Platform::PlayStationPortable
        | Platform::Dreamcast
        | Platform::GameCube
        | Platform::Wii
        | Platform::SegaSaturn
        | Platform::SegaCd
        | Platform::NeoGeoCd
        | Platform::ThreeDo
        | Platform::PcFx => {
            deep_try_disc_platform(path, ext, input_file_size, platform, disc_read_source_for_ext(ext), flags)
        }

        // Xbox/Xbox 360 always use a prefix read: their XDVDFS partition base sits at a
        // fixed offset, so a full-payload read buys nothing over a targeted one.
        Platform::Xbox | Platform::Xbox360 => {
            deep_try_disc_platform(path, ext, input_file_size, platform, DiscReadSource::Prefix, flags)
        }

        _ => deep_try_generic_platform(path, ext, input_file_size, platform, flags),
    }
}

// ---------------------------------------------------------------------------
// Disc platforms
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub enum DiscReadSource {
    Prefix,
    FullPayload,
    DescriptorPayload,
}

/// Ported verbatim from `reference/old_archiver.rs`'s `disc_read_source_for_ext` (line 3247).
fn disc_read_source_for_ext(ext: &str) -> DiscReadSource {
    match ext {
        "cue" | "gdi" => DiscReadSource::DescriptorPayload,
        "bin" | "iso" | "img" | "raw" | "xiso" => DiscReadSource::FullPayload,
        _ => DiscReadSource::Prefix,
    }
}

/// Whether raw disc bytes contain `needle`, ASCII case-insensitively -- used only to report
/// which of two format variants a filesystem-matched file actually used (PS2's `BOOT2=` vs.
/// falling back to plain `BOOT=`), not to redo the actual parse (that already happened in
/// `disc_dispatch`).
fn contains_ascii_ci(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|window| window.eq_ignore_ascii_case(needle))
}

/// Reads `SYSTEM.CNF` out of `path`'s own ISO9660 filesystem, falling back to each of its
/// `.cue`/`.gdi` descriptor payloads when `path` itself isn't a raw ISO (mirroring how the
/// actual lookup in `deep_try_disc_platform` resolves a descriptor to its real disc file --
/// without this fallback, re-deriving evidence for a `.cue` input would read the `.cue` text
/// itself, find no ISO9660 filesystem, and silently fail).
fn read_system_cnf_bytes(path: &Path) -> Option<Vec<u8>> {
    if let Some(bytes) = detect::disc_fs::read_iso_file_bytes_case_insensitive(path, &["SYSTEM.CNF"]) {
        return Some(bytes);
    }
    for payload in descriptor::descriptor_payload_paths(path) {
        if let Some(bytes) = detect::disc_fs::read_iso_file_bytes_case_insensitive(&payload, &["SYSTEM.CNF"]) {
            return Some(bytes);
        }
    }
    None
}

/// Builds real [`Evidence`] for a disc-platform match: the actual title/serial text this
/// disc produced when a filesystem lookup or byte scan found one, plus (when available) the
/// actual disc magic that validated `target` in the first place -- not a description of
/// what was checked. `matched_via_filesystem` distinguishes a real ISO9660/XDVDFS lookup
/// (`disc_dispatch::disc_metadata_from_disc_file`) from the raw byte scan used when there's
/// no filesystem to walk (`disc_dispatch::disc_metadata_from_bytes`). `path` lets platforms
/// whose filesystem match isn't self-describing (PS1 vs PS2 both just "found SYSTEM.CNF";
/// Xbox 360 tries two different files) re-derive exactly which sub-check actually fired.
/// `disc_bytes` is the raw buffer the byte-scan path already read, reused here (not
/// re-read) to show the real magic bytes for Dreamcast/Saturn/Sega CD/GameCube/Wii, which
/// `disc_dispatch::disc_metadata_from_bytes` now validates for real (see
/// `disc_dispatch::sega_disc_hardware_id_offset`/`nintendo_disc_magic_offset`) instead of
/// blindly accepting whatever text sat at the generic offsets. The discriminating entry
/// (filesystem field detail, magic) is pushed last so it's the one the metadata test's
/// report picks as "primary" (see `primary_evidence_text` in `test_archiver.rs`).
fn disc_evidence(
    path: &Path,
    target: Platform,
    matched_via_filesystem: bool,
    title: Option<&str>,
    serial: Option<&str>,
    disc_bytes: Option<&[u8]>,
) -> Vec<Evidence> {
    if target == Platform::TurboGrafxCd {
        // The signature can sit in any of a cue/gdi's tracks (the data track is rarely the
        // first), so look through each payload's first 1MB -- the window the parser searched.
        let payloads = descriptor::descriptor_payload_paths(path);
        let candidates = if payloads.is_empty() { vec![path.to_path_buf()] } else { payloads };
        for candidate in candidates {
            let len = fs::metadata(&candidate).map(|m| m.len() as usize).unwrap_or(0).min(0x100000);
            let Ok(bytes) = read_exact_at(&candidate, 0, len) else { continue };
            if let Some(offset) = detect::disc_dispatch::pce_cd_signature_offset(&bytes) {
                let end = offset + detect::disc_dispatch::PCE_CD_SIGNATURE.len();
                return vec![Evidence::Magic { offset, bytes: bytes[offset..end].to_vec() }];
            }
        }
        return vec![Evidence::Note("no disc signature found".to_string())];
    }

    if matched_via_filesystem {
        return match target {
            Platform::PlayStationPortable => {
                vec![Evidence::FileInFilesystem { filesystem: "ISO9660", path: "PSP_GAME/PARAM.SFO".to_string(), field: None }]
            }
            Platform::PlayStation3 => {
                vec![Evidence::FileInFilesystem { filesystem: "ISO9660", path: "PS3_GAME/PARAM.SFO".to_string(), field: None }]
            }
            Platform::PlayStation => vec![Evidence::FileInFilesystem {
                filesystem: "ISO9660",
                path: "SYSTEM.CNF".to_string(),
                field: serial.map(|s| format!("'BOOT=' line -> {s}")),
            }],
            Platform::PlayStation2 => {
                // PS2 tries BOOT2= first and only falls back to PS1's BOOT= if that's
                // missing (see `disc_dispatch::parse_playstation_boot_serial_from_bytes`
                // callers) -- re-reading SYSTEM.CNF here just says which one this disc
                // actually had, since the parse itself doesn't report that back.
                let key = read_system_cnf_bytes(path)
                    .map(|bytes| if contains_ascii_ci(&bytes, b"BOOT2") { "BOOT2=" } else { "BOOT=" })
                    .unwrap_or("BOOT2=/BOOT=");
                vec![Evidence::FileInFilesystem {
                    filesystem: "ISO9660",
                    path: "SYSTEM.CNF".to_string(),
                    field: serial.map(|s| format!("'{key}' line -> {s}")),
                }]
            }
            Platform::Xbox => vec![Evidence::FileInFilesystem {
                filesystem: "XDVDFS",
                path: "DEFAULT.XBE".to_string(),
                field: serial.map(|s| format!("XBEH certificate title id -> {s}")),
            }],
            Platform::Xbox360 => {
                // GameInfo.bin is tried first and preferred; DEFAULT.XEX is only read when
                // that's absent (see `disc_dispatch::disc_metadata_from_disc_file`'s Xbox360
                // arm) -- report whichever one this disc actually had, not both.
                let matched_file = if detect::disc_fs::read_xdvdfs_file_bytes_case_insensitive(path, target, &["GameInfo.bin"]).is_some() {
                    "GameInfo.bin"
                } else {
                    "DEFAULT.XEX"
                };
                vec![Evidence::FileInFilesystem { filesystem: "XDVDFS", path: matched_file.to_string(), field: None }]
            }
            _ => vec![Evidence::FileInFilesystem { filesystem: "filesystem", path: "matched payload".to_string(), field: None }],
        };
    }

    // Byte-scan platforms with their own dedicated parser (a named search, not a fixed
    // offset): describe the search, since there's no single sector to point at.
    if let Some(context) = match target {
        Platform::PlayStation => Some("'BOOT=' line scanned in raw disc bytes"),
        Platform::PlayStation2 => Some("'BOOT2='/'BOOT=' line scanned in raw disc bytes"),
        Platform::PlayStationPortable | Platform::PlayStation3 => Some("PARAM.SFO scanned in raw disc bytes"),
        Platform::Xbox => Some("XDVDFS XBEH magic scanned in raw disc bytes"),
        Platform::Xbox360 => Some("GameInfo.bin chunks or XEX2 magic scanned in raw disc bytes"),
        _ => None,
    } {
        return match (serial, title) {
            (Some(serial), _) => vec![Evidence::Text { text: serial.to_string(), context: format!("serial, {context}") }],
            (None, Some(title)) => vec![Evidence::Text { text: title.to_string(), context: format!("title, {context}") }],
            (None, None) => vec![Evidence::Note(format!("no disc-specific field text found ({context})"))],
        };
    }

    // Dreamcast/Saturn/Sega CD/GameCube/Wii: `disc_dispatch::disc_metadata_from_bytes`
    // validates a real per-platform disc magic before reading anything, so that magic (real
    // bytes, real offset) is the whole evidence. Titles and serials are metadata.
    let mut evidence = Vec::new();
    if let Some(bytes) = disc_bytes {
        if let Some(offset) = detect::disc_dispatch::sega_disc_hardware_id_offset(bytes, target) {
            let len = match target {
                Platform::Dreamcast | Platform::SegaSaturn => 15,
                _ => 14,
            };
            evidence.push(Evidence::Magic { offset, bytes: bytes.get(offset..offset + len).unwrap_or_default().to_vec() });
        } else if let Some(offset) = detect::disc_dispatch::nintendo_disc_magic_offset(bytes, target) {
            evidence.push(Evidence::Magic { offset, bytes: bytes.get(offset..offset + 4).unwrap_or_default().to_vec() });
        }
    }
    if evidence.is_empty() {
        evidence.push(Evidence::Note("no disc signature found".to_string()));
    }
    evidence
}

/// NeoGeo CD's game id isn't read from a fixed header field -- it's decoded from two BCD
/// bytes that sit right after a scanned `NEO-GEO` marker (see
/// `other::infer_neogeo_cd_game_id_with_evidence_from_bytes`). Builds real evidence (the
/// actual marker + BCD bytes, at the real offset they were found) instead of just reporting
/// the decoded id text with no indication of where it came from.
fn neogeo_cd_marker_evidence(bytes: &[u8]) -> Option<(String, Evidence)> {
    let (offset, id) = other::infer_neogeo_cd_game_id_with_evidence_from_bytes(bytes)?;
    let evidence = Evidence::Bytes {
        offset_start: offset,
        offset_end: offset + 10,
        bytes: bytes.get(offset..offset + 10).unwrap_or_default().to_vec(),
        note: "ASCII 'NEO-GEO' marker; NGH id decoded from BCD bytes at marker+8/+9".to_string(),
    };
    Some((id, evidence))
}

/// Same as [`neogeo_cd_marker_evidence`] but tries each of `paths` in order, reading the
/// file content itself (not just trusting a filename).
fn neogeo_cd_marker_evidence_from_paths(paths: &[PathBuf]) -> Option<(String, Evidence)> {
    paths.iter().find_map(|path| fs::read(path).ok().and_then(|bytes| neogeo_cd_marker_evidence(&bytes)))
}

fn read_disc_bytes_for_source(path: &Path, source: DiscReadSource) -> Result<Vec<u8>, String> {
    match source {
        DiscReadSource::Prefix => read_detection_prefix(path),
        DiscReadSource::FullPayload => descriptor::read_input_payload_bytes(path),
        DiscReadSource::DescriptorPayload => match descriptor::descriptor_payload_paths(path).first() {
            Some(first) => descriptor::read_input_payload_bytes(first),
            None => read_detection_prefix(path),
        },
    }
}

/// Extracts disc metadata for one candidate platform. Ported from
/// `reference/old_archiver.rs`'s `deep_try_disc_platform` (line 3255), retargeted at the new
/// `RomMetadata` field shape.
///
/// The filesystem walk (`disc_metadata_from_disc_file`) runs for `RawIso` inputs. A CHD is
/// read by random access instead (`detect::chd_disc`, only the sectors the platform's method
/// needs, no extraction) when the streaming chdman build is available; without it a CHD falls
/// back to the byte probe and usually comes back empty. RVZ/WIA/GCZ inputs still do.
pub fn deep_try_disc_platform(
    path: &Path,
    ext: &str,
    input_file_size: Option<u64>,
    target: Platform,
    source: DiscReadSource,
    flags: HeuristicFlags,
) -> Option<RomMetadata> {
    let heuristic = metadata_from_heuristics(path, input_file_size, flags.ext, flags.path);
    if heuristic.platform != Platform::Unknown && !platform_compatible_for_cycle(heuristic.platform, target) {
        return None;
    }

    let format = descriptor::detect_archive_format_quick(path).unwrap_or(ArchiveFormat::Unknown);
    let chd_readable = matches!(format, ArchiveFormat::Chd) && detect::chd_disc::available();
    // RVZ/WIA/GCZ are read the same way through Dolphin's reader (plain ISO bytes on demand).
    let rvz_readable = matches!(format, ArchiveFormat::Rvz | ArchiveFormat::Wia | ArchiveFormat::Gcz) && detect::rvz_disc::available();
    let disc_specific = if chd_readable {
        detect::chd_disc::chd_disc_metadata(path, target)
    } else if rvz_readable {
        detect::rvz_disc::rvz_disc_metadata(path, target)
    } else if matches!(format, ArchiveFormat::RawIso) {
        disc_dispatch::disc_metadata_from_disc_file(path, target).or_else(|| {
            if matches!(source, DiscReadSource::DescriptorPayload) {
                disc_metadata_from_descriptor_payloads(path, target)
            } else {
                None
            }
        })
    } else if matches!(source, DiscReadSource::DescriptorPayload) {
        disc_metadata_from_descriptor_payloads(path, target)
    } else {
        None
    };

    let matched_via_filesystem = disc_specific.is_some();
    let mut disc_bytes = None;
    let (title, serial, manufacturer_id) = match disc_specific {
        Some(values) => values,
        // The filesystem is authoritative for PS3/PSP images: a byte scan would find the other
        // console's PARAM.SFO and claim the image for the wrong one.
        None if matches!(format, ArchiveFormat::RawIso) && matches!(target, Platform::PlayStation3 | Platform::PlayStationPortable) => (None, None, None),
        // Everything the container can tell was already asked of it; its compressed bytes can't be probed.
        None if chd_readable || rvz_readable => (None, None, None),
        None => {
            let bytes = read_disc_bytes_for_source(path, source).ok()?;
            let values = disc_dispatch::disc_metadata_from_bytes(&bytes, target, matches!(source, DiscReadSource::Prefix));
            disc_bytes = Some(bytes);
            values
        }
    };

    let mut meta = RomMetadata {
        title,
        serial_product_id: serial.or(manufacturer_id),
        platform: if heuristic.platform == Platform::Unknown { target } else { heuristic.platform },
        media_type: infer_media_type_from_heuristics(ext, target),
        codec: heuristic.codec,
        codecs: heuristic.codecs,
        file_size: input_file_size,
        candidates: if heuristic.candidates.is_empty() { vec![target] } else { heuristic.candidates },
        detection_trail: heuristic.detection_trail,
        ..Default::default()
    };

    // These platforms are validated by a disc signature, so a readable image without it is
    // not this platform. (A compressed CHD/RVZ can't be read here at all and stays lenient.)
    let readable = matches!(format, ArchiveFormat::RawIso) || matches!(source, DiscReadSource::DescriptorPayload) || chd_readable || rvz_readable;
    if readable && meta.title.is_none() && meta.serial_product_id.is_none() && disc_signature_validated(target) {
        return None;
    }

    if meta.title.is_some() || meta.serial_product_id.is_some() {
        for evidence in disc_evidence(path, target, matched_via_filesystem, meta.title.as_deref(), meta.serial_product_id.as_deref(), disc_bytes.as_deref())
        {
            push_detection_reason(&mut meta, DetectionStepKind::Descriptor, evidence);
        }
    } else if target != Platform::NeoGeoCd {
        // NeoGeo CD's evidence is its `NEO-GEO` marker, found by the inference below.
        push_detection_reason(
            &mut meta,
            DetectionStepKind::Descriptor,
            Evidence::Note(format!("no disc-specific evidence found (source={source:?} format={format:?})")),
        );
    }

    if !platform_compatible_for_cycle(meta.platform, target) {
        return None;
    }
    if meta.platform == Platform::Unknown {
        meta.platform = target;
    }

    if meta.platform == Platform::NeoGeoCd && meta.serial_product_id.is_none() {
        let found = disc_bytes
            .as_deref()
            .and_then(neogeo_cd_marker_evidence)
            .or_else(|| neogeo_cd_marker_evidence_from_paths(&descriptor::descriptor_payload_paths(path)))
            .or_else(|| neogeo_cd_marker_evidence_from_paths(&[path.to_path_buf()]));
        match found {
            Some((id, evidence)) => {
                push_detection_reason(&mut meta, DetectionStepKind::Inference, evidence);
                meta.serial_product_id = Some(id);
            }
            // A readable image without the marker is not a Neo Geo CD -- unless its
            // filesystem already proved it is (`IPL.TXT` found by the reader-based path).
            None if readable && !matched_via_filesystem => return None,
            None => {
                push_detection_reason(
                    &mut meta,
                    DetectionStepKind::Descriptor,
                    Evidence::Note(format!("no NEO-GEO marker readable (source={source:?} format={format:?})")),
                );
            }
        }
    }

    Some(normalize_metadata(meta))
}

/// Tries each payload a `.cue`/`.gdi` references and keeps the best-scoring result. Ported
/// from `reference/old_archiver.rs`'s `disc_metadata_from_descriptor_payloads` (line 4134).
fn disc_metadata_from_descriptor_payloads(path: &Path, target: Platform) -> Option<disc_dispatch::DiscTitleSerial> {
    let payloads = descriptor::descriptor_payload_paths(path);
    let mut best: Option<(usize, disc_dispatch::DiscTitleSerial)> = None;

    for payload in payloads {
        let format = descriptor::detect_archive_format_quick(&payload).unwrap_or(ArchiveFormat::Unknown);
        let values = if matches!(format, ArchiveFormat::RawIso) {
            // TurboGrafx-CD has no filesystem lookup: its signature is found by scanning the
            // track's bytes, and only the data track among the cue's payloads carries it.
            disc_dispatch::disc_metadata_from_disc_file(&payload, target).or_else(|| {
                if target != Platform::TurboGrafxCd {
                    return None;
                }
                let values = disc_dispatch::disc_metadata_from_bytes(&fs::read(&payload).ok()?, target, false);
                (values.0.is_some() || values.1.is_some()).then_some(values)
            })
        } else {
            descriptor::read_input_payload_bytes(&payload)
                .ok()
                .map(|bytes| disc_dispatch::disc_metadata_from_bytes(&bytes, target, false))
        };
        let Some(values) = values else { continue };

        let meta = normalize_metadata(RomMetadata {
            title: values.0.clone(),
            serial_product_id: values.1.clone().or(values.2.clone()),
            platform: target,
            candidates: vec![target],
            ..Default::default()
        });
        let score = metadata_score(&meta);

        match &best {
            Some((best_score, _)) if score <= *best_score => {}
            _ => best = Some((score, (meta.title, meta.serial_product_id.clone(), meta.serial_product_id))),
        }
    }

    best.map(|(_, values)| values)
}

// ---------------------------------------------------------------------------
// Cartridge / container platforms
// ---------------------------------------------------------------------------

/// Ported from `reference/old_archiver.rs`'s `deep_try_generic_platform_with_mode` (line
/// 5056), minus `strict_metadata_mode`.
fn deep_try_generic_platform(
    path: &Path,
    ext: &str,
    input_file_size: Option<u64>,
    target: Platform,
    flags: HeuristicFlags,
) -> Option<RomMetadata> {
    if matches!(ext, "cue" | "gdi") {
        // A cue/gdi sheet's own bytes say nothing; a signature of another system in one of
        // its tracks rules this platform out for the whole set.
        if mycom::is_mycom_platform(target)
            && matches!(mycom::verdict_for_cd_tracks(&descriptor::descriptor_payload_paths(path), target), mycom::MycomVerdict::Conflict)
        {
            return None;
        }
        if let Some(meta) = deep_try_descriptor_payload_metadata(path, input_file_size, target, flags) {
            return Some(meta);
        }
    }

    let mut merged = deep_read_header_metadata(path, ext, input_file_size, target, flags)?;
    if !platform_compatible_for_cycle(merged.platform, target) {
        return None;
    }

    if merged.platform == Platform::Unknown {
        merged.platform = target;
    }
    if merged.media_type == MediaType::Unknown {
        merged.media_type = infer_media_type_from_heuristics(ext, target);
    }
    Some(merged)
}

/// Ported from `reference/old_archiver.rs`'s `deep_try_descriptor_payload_metadata` (line 5091).
fn deep_try_descriptor_payload_metadata(
    path: &Path,
    input_file_size: Option<u64>,
    target: Platform,
    flags: HeuristicFlags,
) -> Option<RomMetadata> {
    let payloads = descriptor::descriptor_payload_paths(path);
    let mut picked = best_metadata_from_paths(&payloads, flags)?;
    if !platform_compatible_for_cycle(picked.platform, target) {
        return None;
    }

    if mycom::is_mycom_platform(target) {
        match mycom::verdict_for_cd_tracks(&payloads, target) {
            mycom::MycomVerdict::Match(evidence) => {
                for item in evidence {
                    push_detection_reason(&mut picked, DetectionStepKind::Descriptor, item);
                }
            }
            mycom::MycomVerdict::Conflict => return None,
            mycom::MycomVerdict::Unknown => {}
        }
    }

    if picked.platform == Platform::Unknown {
        picked.platform = target;
    }
    picked.file_size = input_file_size;

    if picked.platform == Platform::NeoGeoCd && picked.serial_product_id.is_none() {
        picked.serial_product_id = other::infer_neogeo_cd_game_id_from_paths(&payloads);
    }

    Some(normalize_metadata(picked))
}

/// Picks the best-scoring heuristic metadata among a set of sibling payload files. Ported
/// from `reference/old_archiver.rs`'s `best_metadata_from_paths` (line 6643).
fn best_metadata_from_paths(paths: &[PathBuf], flags: HeuristicFlags) -> Option<RomMetadata> {
    let mut best: Option<(usize, RomMetadata)> = None;

    for path in paths {
        let file_size = fs::metadata(path).ok().map(|m| m.len());
        let mut candidate = normalize_metadata(metadata_from_heuristics(path, file_size, flags.ext, flags.path));
        if candidate.platform == Platform::NeoGeoCd && candidate.serial_product_id.is_none() {
            candidate.serial_product_id = other::infer_neogeo_cd_game_id_from_paths(std::slice::from_ref(path));
        }
        let score = metadata_score(&candidate);

        match &best {
            Some((best_score, _)) if score <= *best_score => {}
            _ => best = Some((score, candidate)),
        }
    }

    best.map(|(_, meta)| meta)
}

/// Merges a platform's parsed header metadata onto the heuristic baseline. Ported from
/// `reference/old_archiver.rs`'s `deep_read_header_metadata` (line 5117), minus
/// `strict_metadata_mode` (and therefore minus its `matched` return flag, which only ever
/// gated strict-mode behavior).
fn deep_read_header_metadata(
    path: &Path,
    ext: &str,
    input_file_size: Option<u64>,
    target: Platform,
    flags: HeuristicFlags,
) -> Option<RomMetadata> {
    let header_flags = flags.for_header_read(ext);
    let mut merged = metadata_from_heuristics(path, input_file_size, header_flags.ext, header_flags.path);
    if merged.candidates.is_empty() {
        merged.candidates = vec![target];
    }

    if mycom::is_mycom_platform(target) && matches!(mycom::verdict_for_file(path, target), mycom::MycomVerdict::Conflict) {
        return None;
    }
    let header_meta = read_non_disc_header_metadata(path, target);
    if header_meta.is_none() && has_content_signature_parser(target) {
        // The platform has a parser that checks real content, and it found nothing: the
        // file is not this platform, whatever its name or location suggested.
        return None;
    }
    if let Some(meta) = header_meta {
        for evidence in header_evidence(target, path, &meta) {
            push_detection_reason(&mut merged, DetectionStepKind::Header, evidence);
        }
        if merged.title.is_none() {
            merged.title = meta.title;
        }
        if merged.serial_product_id.is_none() {
            merged.serial_product_id = meta.serial.or(meta.manufacturer_id);
        }
        merged.headered = meta.headered;
        merged.encrypted = meta.encrypted;
        merged.byte_order = meta.byte_order;
        merged.hash_confirmed = meta.hash_confirmed;
    }

    // New 3DS vs 3DS disambiguation by serial prefix -- **centralized here**, where old code
    // duplicated it at two call sites (`deep_read_header_metadata` and
    // `read_non_disc_header_metadata`'s own 3DS arm).
    if matches!(target, Platform::Nintendo3ds | Platform::NewNintendo3ds) {
        if let Some(platform) = nintendo::disambiguate_3ds_by_serial(merged.serial_product_id.as_deref()) {
            merged.platform = platform;
        }
    }

    // A CHD/RVZ whose own content we can't read may still have a .cue/.gdi sidecar whose
    // track files we can.
    if merged.serial_product_id.is_none() && matches!(ext, "chd" | "rvz") {
        if let Some(sidecar) = detect::find_sidecar_descriptor(path) {
            let candidates = descriptor::descriptor_payload_paths(&sidecar);
            if let Some(extra) = best_metadata_from_paths(&candidates, flags) {
                push_detection_reason(
                    &mut merged,
                    DetectionStepKind::Descriptor,
                    Evidence::Note(format!("descriptor sidecar metadata from {}", sidecar.display())),
                );
                if merged.serial_product_id.is_none() {
                    merged.serial_product_id = extra.serial_product_id;
                }
                if merged.title.is_none() {
                    merged.title = extra.title;
                }
                if merged.platform == Platform::Unknown && extra.platform != Platform::Unknown {
                    merged.platform = extra.platform;
                }
                if merged.media_type == MediaType::Unknown && extra.media_type != MediaType::Unknown {
                    merged.media_type = extra.media_type;
                }
            }

            if merged.platform == Platform::NeoGeoCd && merged.serial_product_id.is_none() {
                if let Some((game_id, evidence)) = neogeo_cd_marker_evidence_from_paths(&candidates) {
                    push_detection_reason(&mut merged, DetectionStepKind::Inference, evidence);
                    merged.serial_product_id = Some(game_id);
                }
            }
        }
    }

    if merged.platform == Platform::NeoGeoCd && merged.serial_product_id.is_none() {
        if let Some((game_id, evidence)) = neogeo_cd_marker_evidence_from_paths(&[path.to_path_buf()]) {
            push_detection_reason(&mut merged, DetectionStepKind::Inference, evidence);
            merged.serial_product_id = Some(game_id);
        }
    }

    if input_file_size.is_some() {
        merged.file_size = input_file_size;
    }

    Some(normalize_metadata(merged))
}

/// Reads exactly `len` bytes at `offset`, tolerating any failure (past EOF, unreadable) by
/// returning an empty vec -- evidence-construction is best-effort diagnostic output, never
/// the reason a whole detection fails.
fn read_bytes(path: &Path, offset: usize, len: usize) -> Vec<u8> {
    read_exact_at(path, offset as u64, len).unwrap_or_default()
}

/// Builds the [`Evidence`] for a non-disc header match: the discriminator that makes this file
/// this platform (a magic, a boot-logo check, a checksum pair), read fresh from `path`, plus --
/// only where two platforms share one layout -- the extra byte that tells them apart (GB/GBC's
/// CGB flag, NGP/NGPC's mode byte, DS/DSi's unit code, WS/WSC's system byte). Title, serial and
/// encryption state are metadata, not evidence, and stay out of it.
///
/// Nothing here looks at the file extension: which layout matched is read back from the bytes
/// themselves, the same bytes the leaf parser validated.
fn header_evidence(target: Platform, path: &Path, meta: &NonDiscHeaderMetadata) -> Vec<Evidence> {
    if !meta.evidence.is_empty() {
        return meta.evidence.clone();
    }
    let magic = |offset: usize, len: usize| Evidence::Magic { offset, bytes: read_bytes(path, offset, len) };
    let bytes_note = |start: usize, end: usize, note: String| Evidence::Bytes {
        offset_start: start,
        offset_end: end,
        bytes: read_bytes(path, start, end - start),
        note,
    };

    match target {
        Platform::FamicomDiskSystem => {
            let base = if read_bytes(path, 0, 4).as_slice() == b"FDS\x1A" { 0x10 } else { 0 };
            vec![magic(base + 0x01, 0x0E)]
        }
        Platform::GameBoy | Platform::GameBoyColor => {
            // The Nintendo logo is what makes this a Game Boy family cartridge; the CGB flag
            // is the only thing separating GB from GBC.
            let cgb_flag = read_bytes(path, 0x143, 1);
            let is_gbc = cgb_flag.first().map(|b| b & 0x80 != 0).unwrap_or(false);
            vec![
                Evidence::Bytes {
                    offset_start: 0x104,
                    offset_end: 0x134,
                    bytes: read_bytes(path, 0x104, 48),
                    note: "Nintendo boot logo".to_string(),
                },
                Evidence::Verified { offset_start: 0x134, offset_end: 0x14E, description: "header checksum".to_string() },
                Evidence::Bytes {
                    offset_start: 0x143,
                    offset_end: 0x144,
                    bytes: cgb_flag,
                    note: format!("CGB flag: {}", if is_gbc { "GBC-enhanced (bit 0x80 set)" } else { "plain GB (bit 0x80 clear)" }),
                },
            ]
        }
        Platform::GameBoyAdvance => vec![
            Evidence::Verified {
                offset_start: 0x04,
                offset_end: 0x04 + nintendo::NINTENDO_LOGO_LEN,
                description: "Nintendo boot logo (CRC-16 0xCF56)".to_string(),
            },
            magic(0xB2, 1),
            Evidence::Verified { offset_start: 0xA0, offset_end: 0xBE, description: "header complement checksum".to_string() },
        ],
        Platform::NintendoDs | Platform::NintendoDsi => {
            if path.is_dir() {
                return vec![Evidence::Note("directory bundle with a TMD (tmd*) file whose signature layout parsed".to_string())];
            }
            // DS vs. DSi-exclusive is decided by the unit code byte at 0x12 (0x03 == DSi).
            let unit_code = read_bytes(path, 0x12, 1);
            let is_dsi = unit_code.first().copied() == Some(0x03);
            vec![
                Evidence::Verified {
                    offset_start: 0xC0,
                    offset_end: 0xC0 + nintendo::NINTENDO_LOGO_LEN,
                    description: "Nintendo boot logo (CRC-16 0xCF56)".to_string(),
                },
                Evidence::Bytes {
                    offset_start: 0x12,
                    offset_end: 0x13,
                    bytes: unit_code,
                    note: format!("unit code: {}", if is_dsi { "0x03, DSi-exclusive" } else { "not 0x03, DS (or DS-compatible DSi)" }),
                },
            ]
        }
        Platform::SufamiTurbo => vec![magic(0x00, 0x0E)],
        Platform::Nintendo64 => {
            let note = match meta.byte_order {
                Some(RomByteOrder::BigEndian) => "big-endian (.z64): magic 80 37 12 40, no swap needed",
                Some(RomByteOrder::ByteSwapped) => "byte-swapped (.v64): magic 37 80 40 12, byte-pairs swapped",
                Some(RomByteOrder::LittleEndian) => "word-swapped (.n64): magic 40 12 37 80, 4-byte chunks reversed",
                None => "one of the three byte-order magics",
            };
            vec![bytes_note(0x00, 0x04, note.to_string())]
        }
        Platform::SegaGenesis | Platform::SegaPico => {
            // The system-type string at 0x100 is both what makes this a Sega cartridge and
            // what separates Genesis/Mega Drive from Pico.
            vec![bytes_note(0x100, 0x110, "system type string".to_string())]
        }
        Platform::Sega32x => vec![magic(0x100, 0x10)],
        Platform::NeoGeoPocket | Platform::NeoGeoPocketColor => {
            // The SNK license string is what makes this a NeoGeo Pocket cartridge; the mode
            // byte at 0x23 (0x10 == color) separates Pocket from Pocket Color.
            let mode = read_bytes(path, 0x23, 1);
            let is_color = mode.first().copied() == Some(0x10);
            vec![
                magic(0x00, 0x1C),
                Evidence::Bytes {
                    offset_start: 0x23,
                    offset_end: 0x24,
                    bytes: mode,
                    note: format!("mode byte: {}", if is_color { "0x10, color" } else { "not 0x10, mono" }),
                },
            ]
        }
        Platform::TurboGrafx16 | Platform::SuperGrafx => {
            // A HuCard has no header; the entry opcodes and size are only shape. What tells
            // SuperGrafx from TurboGrafx-16 is a SuperGrafx-exclusive VPC register write in
            // the boot code, or a whole-file CRC32 matching one of the five exclusive titles.
            let entry = bytes_note(0x00, 0x02, "HuC6280 entry opcodes; file size a multiple of 0x2000".to_string());
            // 6144 matches `find_supergrafx_vpc_init`'s own scan window.
            let mut evidence = vec![entry];
            let vpc = other::find_supergrafx_vpc_init(&read_bytes(path, 0, 6144));
            let crc = other::known_supergrafx_exclusive_crc32(path);
            if let Some(hits) = &vpc {
                evidence.push(bytes_note(
                    hits[0],
                    hits[0] + 3,
                    format!(
                        "SuperGrafx-exclusive VPC register write ({} such writes in the first 6KB); these ports are unmapped on TurboGrafx-16/PC Engine",
                        hits.len()
                    ),
                ));
            }
            match crc {
                Some(crc) => evidence.push(Evidence::Note(format!("whole-file CRC32 {crc:08X} matches a known SuperGrafx-exclusive title"))),
                None => evidence.push(Evidence::Note("whole-file CRC32 matches no known SuperGrafx-exclusive title".to_string())),
            }
            if vpc.is_none() {
                evidence.push(Evidence::Note("no SuperGrafx-exclusive VPC register write in the first 6KB".to_string()));
            }
            evidence
        }
        Platform::PlayStationVita => vec![magic(0x00, 0x04)],
        Platform::WiiU => vec![magic(0x00, 0x04)],
        Platform::Nintendo3ds | Platform::NewNintendo3ds => {
            // A cartridge dump has NCSD at 0x100; anything else that parsed is a CIA, whose
            // layout is found by walking its header sizes to the TMD.
            let mut evidence = Vec::new();
            if read_bytes(path, 0x100, 4).as_slice() == b"NCSD" {
                evidence.push(magic(0x100, 4));
            } else if let Some(tmd_offset) = meta.header_offset {
                evidence.push(bytes_note(
                    tmd_offset as usize,
                    tmd_offset as usize + 4,
                    "TMD signature type, reached by walking the CIA header/certificate/ticket sizes".to_string(),
                ));
            }
            // Old vs. New 3DS: the NCCH product code prefix (`CTR-` original, `KTR-` New 3DS).
            if let Some((start, _)) = meta.serial_offset.filter(|(start, end)| end - start == 16) {
                let prefix = read_bytes(path, start as usize, 4);
                let note = match prefix.as_slice() {
                    b"KTR-" => "product code prefix: KTR- is New 3DS",
                    b"CTR-" => "product code prefix: CTR- is original 3DS",
                    _ => "product code prefix",
                };
                evidence.push(bytes_note(start as usize, start as usize + 4, note.to_string()));
            } else {
                evidence.push(Evidence::Note("NCCH product code unreadable (content encrypted); old/New 3DS not determined from content".to_string()));
            }
            evidence
        }
        Platform::Nes | Platform::Famicom => {
            let head = read_bytes(path, 0, 4);
            let hash_matched = meta.hash_confirmed == Some(true);
            let mut evidence = Vec::new();
            if head.as_slice() == b"NES\x1A" || head.as_slice() == b"UNIF" {
                evidence.push(Evidence::Magic { offset: 0, bytes: head });
                if hash_matched {
                    evidence.push(Evidence::Note(NES_HASH_MATCH.to_string()));
                }
            } else if hash_matched {
                // Whole-file hash of a known game: the only evidence a headerless (or
                // header-mangled) dump needs.
                evidence.push(Evidence::Note(NES_HASH_MATCH.to_string()));
            } else {
                // No magic and no known hash: the lenient path. Size shape, plus the PPU
                // warm-up idiom real boot code contains, when it is there.
                let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                evidence.push(Evidence::Shape {
                    description: format!(
                        "headerless dump, hash not in the known-game database: file size 0x{size:X}, a nonzero multiple of 0x2000 (up to {:#X})",
                        nintendo::MAX_PLAUSIBLE_NES_FILE_SIZE
                    ),
                });
                if let Some(offset) = fs::read(path).ok().as_deref().and_then(nintendo::find_nes_ppu_init) {
                    evidence.push(bytes_note(
                        offset,
                        offset + 6,
                        "PPUCTRL ($2000)/PPUMASK ($2001) disabled in sequence -- the standard NES boot warm-up".to_string(),
                    ));
                }
            }
            evidence
        }
        Platform::SuperFamicom => match meta.header_offset {
            // The header's checksum/complement pair (which must sum to 0xFFFF) is the only
            // self-check an SNES header has; the map mode byte says which layout it sat in.
            Some(offset) => {
                let offset = offset as usize;
                vec![
                    bytes_note(offset + 0x1C, offset + 0x20, "checksum complement + checksum, summing to 0xFFFF".to_string()),
                    bytes_note(offset + 0x15, offset + 0x16, "map mode".to_string()),
                ]
            }
            None => vec![Evidence::Note("no header candidate at 0x7FC0/0xFFC0/0x40FFC0 passed the checksum check".to_string())],
        },
        Platform::SegaMasterSystem | Platform::SegaGameGear => {
            for offset in [0x1FF0usize, 0x3FF0, 0x7FF0] {
                let bytes = read_bytes(path, offset, 8);
                if bytes.starts_with(b"TMR SEGA") {
                    // Byte 0x0F's high nibble (5-7) marks Game Gear; anything else is SMS.
                    let region = read_bytes(path, offset + 0x0F, 1);
                    let nibble = region.first().map(|b| b >> 4).unwrap_or(0);
                    return vec![
                        Evidence::Magic { offset, bytes },
                        Evidence::Bytes {
                            offset_start: offset + 0x0F,
                            offset_end: offset + 0x10,
                            bytes: region,
                            note: format!("region nibble {nibble}: {}", if (5..=7).contains(&nibble) { "Game Gear" } else { "Master System" }),
                        },
                    ];
                }
            }
            vec![Evidence::Note("'TMR SEGA' not found at 0x1FF0/0x3FF0/0x7FF0".to_string())]
        }
        Platform::WonderSwan | Platform::WonderSwanColor => {
            let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0) as usize;
            if size >= 16 {
                let tail = read_bytes(path, size - 16, 16);
                // System byte at tail[7] (offset end-9) is the actual WS/WSC discriminator.
                let system_byte = tail.get(7).copied();
                let looks_color = matches!(system_byte, Some(0x01) | Some(0x02));
                vec![Evidence::Bytes {
                    offset_start: size - 16,
                    offset_end: size,
                    bytes: tail,
                    note: format!(
                        "footer starting with far-jump opcode 0xEA; system byte at end-9 = 0x{:02X} ({})",
                        system_byte.unwrap_or(0),
                        if looks_color { "WonderSwan Color" } else { "plain WonderSwan" }
                    ),
                }]
            } else {
                vec![Evidence::Note("file too small for WonderSwan footer".to_string())]
            }
        }
        Platform::Sg1000 => vec![bytes_note(0x00, 0x02, "Z80 entry opcode; file size 0x2000-0xC000; no 'TMR SEGA' at 0x1FF0".to_string())],
        Platform::VirtualBoy => {
            // No magic: the layout is fixed from the end of the file. Five reserved zero bytes
            // in the 544-byte-from-EOF header, plus a non-blank reset vector in the last 16.
            let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0) as usize;
            if size < 544 {
                return vec![Evidence::Shape { description: format!("file size 0x{size:X} too small for the 544-byte-from-EOF header") }];
            }
            let header = size - 544;
            vec![
                bytes_note(header + 0x14, header + 0x19, "reserved zero bytes of the header 544 bytes before EOF".to_string()),
                bytes_note(size - 16, size, "reset vector (non-blank code)".to_string()),
            ]
        }
        Platform::PocketStation => vec![magic(0x52, 4)],
        Platform::Nintendo64Dd => {
            let region = read_bytes(path, 0, 4);
            let region_note = match region.as_slice() {
                r if r == nintendo::N64DD_REGION_JP => "region code: JP retail",
                r if r == nintendo::N64DD_REGION_US => "region code: US retail",
                _ => "region code: development disk",
            };
            vec![
                bytes_note(0x00, 0x04, region_note.to_string()),
                bytes_note(0x04, 0x05, "system data format type, always 0x10".to_string()),
                bytes_note(0x18, 0x1C, "reserved word, always 0xFFFFFFFF".to_string()),
            ]
        }
        _ => vec![Evidence::Note(format!("non-disc header metadata matched for target {target:?}"))],
    }
}

const NES_HASH_MATCH: &str = "whole-file hash matches a known game in the NES header database";

/// Whether a `None` from [`deep_try_platform`] for `target` on this input means "checked
/// the content and it is not this platform" (so a detection cycle may drop `target` from
/// its candidates) rather than "nothing could be learned". Mirrors the two rejection
/// gates: disc platforms reject on a missing signature only when the disc bytes were
/// actually readable (see the `readable` check in [`deep_try_disc_platform`]); cartridge/
/// container platforms reject when they have a real content parser
/// ([`has_content_signature_parser`], the gate in `deep_read_header_metadata`).
pub fn rejects_on_miss(path: &Path, ext: &str, target: Platform) -> bool {
    let disc_signature_platform = disc_signature_validated(target);
    let is_disc_route = matches!(
        target,
        Platform::TurboGrafxCd
            | Platform::PlayStation
            | Platform::PlayStation2
            | Platform::PlayStation3
            | Platform::PlayStationPortable
            | Platform::Dreamcast
            | Platform::GameCube
            | Platform::Wii
            | Platform::SegaSaturn
            | Platform::SegaCd
            | Platform::NeoGeoCd
            | Platform::ThreeDo
            | Platform::PcFx
    ) && !(matches!(target, Platform::PlayStation3 | Platform::PlayStationPortable) && download::is_download_input(path));
    if is_disc_route {
        let format = descriptor::detect_archive_format_quick(path).unwrap_or(ArchiveFormat::Unknown);
        let readable = matches!(format, ArchiveFormat::RawIso)
            || matches!(disc_read_source_for_ext(ext), DiscReadSource::DescriptorPayload)
            || (matches!(format, ArchiveFormat::Chd) && detect::chd_disc::available())
            || (matches!(format, ArchiveFormat::Rvz | ArchiveFormat::Wia | ArchiveFormat::Gcz) && detect::rvz_disc::available());
        return readable && (disc_signature_platform || target == Platform::NeoGeoCd);
    }
    has_content_signature_parser(target)
}

/// Disc platforms validated by a signature in the disc bytes (Sega hardware ids, Nintendo
/// magic, the PC Engine IPL string, ISO9660 system ids / boot files, 3DO's Opera sync bytes,
/// PC-FX's boot string): a readable image without it is not this platform.
pub fn disc_signature_validated(target: Platform) -> bool {
    matches!(
        target,
        Platform::Dreamcast
            | Platform::SegaSaturn
            | Platform::SegaCd
            | Platform::GameCube
            | Platform::Wii
            | Platform::TurboGrafxCd
            | Platform::PlayStation
            | Platform::PlayStation2
            | Platform::PlayStation3
            | Platform::PlayStationPortable
            | Platform::ThreeDo
            | Platform::PcFx
    )
}

/// Whether `target` has a content parser in [`read_non_disc_header_metadata`] (as opposed to
/// falling through to `None` because detection for it is unimplemented). A miss from one of
/// these is a rejection; a miss from the rest just means nothing was learned.
pub fn has_content_signature_parser(target: Platform) -> bool {
    matches!(
        target,
        Platform::FamicomDiskSystem
            | Platform::GameBoy
            | Platform::GameBoyColor
            | Platform::GameBoyAdvance
            | Platform::NintendoDs
            | Platform::NintendoDsi
            | Platform::SufamiTurbo
            | Platform::Nintendo64
            | Platform::SegaGenesis
            | Platform::SegaPico
            | Platform::Sega32x
            | Platform::NeoGeoPocket
            | Platform::NeoGeoPocketColor
            | Platform::TurboGrafx16
            | Platform::SuperGrafx
            | Platform::PlayStationVita
            | Platform::NintendoSwitch
            | Platform::PlayStation3
            | Platform::PlayStationPortable
            | Platform::PspPs1Classic
            | Platform::DsiWare
            | Platform::WiiU
            | Platform::Nintendo3ds
            | Platform::NewNintendo3ds
            | Platform::PocketStation
            | Platform::Nes
            | Platform::Famicom
            | Platform::SuperFamicom
            | Platform::SegaMasterSystem
            | Platform::SegaGameGear
            | Platform::WonderSwan
            | Platform::WonderSwanColor
            | Platform::Sg1000
            | Platform::VirtualBoy
            | Platform::Nintendo64Dd
    )
}

/// Runs `target`'s own header parser. One flat match, grouped by **container kind** rather
/// than by manufacturer -- what a parser needs to read (a header prefix, a footer, a whole
/// file, a tape/floppy container, a directory bundle) drives the work far more than which
/// company made the machine. The family submodules are storage for the leaf parsers, not a
/// dispatch layer: every arm below calls its leaf parser directly, so a platform is matched
/// exactly once rather than once here and again inside a family function.
///
/// Ported from `reference/old_archiver.rs`'s `read_non_disc_header_metadata` (line 5242),
/// with each arm's repeated `.map(|(title, serial, manufacturer_id)| NonDiscHeaderMetadata
/// { .. })` closure collapsed into [`NonDiscHeaderMetadata::from_tuple`], and every
/// variant-discriminating gate (GB/GBC, DS/DSi, 3DS/New 3DS, Genesis/Pico) pushed down into
/// the leaf parser that already reads the byte the gate tests.
pub fn read_non_disc_header_metadata(path: &Path, target: Platform) -> Option<NonDiscHeaderMetadata> {
    // Directory bundles: loose-file dumps, handled before any single-file read.
    if path.is_dir() {
        return read_directory_non_disc_header_metadata(path, target);
    }

    let from_tuple = NonDiscHeaderMetadata::from_tuple;

    match target {
        // --- PS3/PSP package files (PKG/PBP); ISO images take the disc path instead ---
        Platform::PlayStation3 | Platform::PlayStationPortable | Platform::PspPs1Classic | Platform::DsiWare => match download::verdict_for_file(path, target) {
            download::DownloadVerdict::Match(meta) => Some(meta),
            _ => None,
        },

        // --- Mycom family: only content signatures (see `mycom`'s module doc); everything
        // else resolves by the extension/path heuristics ---
        Platform::Msx
        | Platform::Msx2
        | Platform::MsxTurbo
        | Platform::Msx2Plus
        | Platform::Fm7
        | Platform::Pc8801
        | Platform::Pc98
        | Platform::FmTowns => match mycom::verdict_for_file(path, target) {
            mycom::MycomVerdict::Match(evidence) => Some(NonDiscHeaderMetadata { evidence, ..Default::default() }),
            _ => None,
        },

        // --- Header-prefix cartridges: everything identifiable from the first 4KB ---
        Platform::FamicomDiskSystem => {
            nintendo::parse_fds_qd_metadata_from_bytes(&read_detection_prefix(path).ok()?).map(from_tuple)
        }
        Platform::GameBoy | Platform::GameBoyColor => {
            nintendo::parse_gameboy_metadata_from_bytes(&read_detection_prefix(path).ok()?, target).map(from_tuple)
        }
        Platform::GameBoyAdvance => {
            nintendo::parse_gba_header_metadata_from_bytes(&read_detection_prefix(path).ok()?).map(from_tuple)
        }
        Platform::NintendoDs | Platform::NintendoDsi => {
            nintendo::parse_nds_header_metadata_from_bytes(path, &read_detection_prefix(path).ok()?, target)
        }
        Platform::SufamiTurbo => {
            nintendo::parse_sufami_turbo_metadata_from_bytes(&read_detection_prefix(path).ok()?).map(from_tuple)
        }
        Platform::Nintendo64 => nintendo::parse_n64_header_metadata_from_bytes(&read_detection_prefix(path).ok()?),
        Platform::SegaGenesis | Platform::SegaPico => {
            sega::parse_genesis_header_metadata_from_bytes(&read_detection_prefix(path).ok()?, target).map(from_tuple)
        }
        Platform::Sega32x => {
            sega::parse_sega_32x_header_metadata_from_bytes(&read_detection_prefix(path).ok()?).map(from_tuple)
        }
        Platform::NeoGeoPocket | Platform::NeoGeoPocketColor => {
            other::parse_neogeo_pocket_header_metadata_from_bytes(&read_detection_prefix(path).ok()?, target).map(from_tuple)
        }
        Platform::TurboGrafx16 | Platform::SuperGrafx => {
            other::parse_pce_hucard_metadata_from_bytes(path, &read_detection_prefix(path).ok()?, target).map(from_tuple)
        }

        // --- Packaged / digital containers (identified from a prefix, but not a cartridge header) ---
        Platform::PlayStationVita => sony::read_vita_metadata(path),
        Platform::NintendoSwitch => switch::read_switch_metadata(path),
        Platform::WiiU => {
            nintendo::parse_wiiu_container_metadata_from_bytes(&read_detection_prefix(path).ok()?).map(from_tuple)
        }
        Platform::Nintendo3ds | Platform::NewNintendo3ds => nintendo::read_3ds_ncsd_metadata(path, target),
        Platform::PocketStation => sony::parse_pocket_station_metadata_from_bytes(&read_detection_prefix(path).ok()?).map(from_tuple),

        // --- Seek-based: header at a size-dependent offset, at EOF, or chunk-walked ---
        Platform::Nes | Platform::Famicom => nintendo::read_nes_container_metadata(path),
        Platform::SuperFamicom => nintendo::read_snes_rom_metadata(path),
        Platform::SegaMasterSystem | Platform::SegaGameGear => sega::parse_sms_gg_header_metadata(path, target).map(from_tuple),
        Platform::WonderSwan | Platform::WonderSwanColor => other::parse_wonderswan_footer_metadata(path, target).map(from_tuple),

        // --- Shape-only: headerless dumps identified by size/extension/opcode ---
        Platform::Sg1000 => sega::parse_sg1000_rom_shape(path).map(from_tuple),
        Platform::VirtualBoy => other::parse_virtual_boy_rom_shape(path).map(from_tuple),
        Platform::Nintendo64Dd => nintendo::read_n64dd_system_data_metadata(path),

        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Directory bundles (loose-file dumps for vintage PC platforms and DSi)
// ---------------------------------------------------------------------------

/// Ported verbatim from `reference/old_archiver.rs`'s `is_directory_bundle_target` (line 6498).
/// Public for directory scans (`rom::scan::find_bundle`).
pub fn is_directory_bundle_target(target: Platform) -> bool {
    matches!(
        target,
        Platform::Msx
            | Platform::Msx2
            | Platform::MsxTurbo
            | Platform::Msx2Plus
            | Platform::Pc8801
            | Platform::Pc98
            | Platform::Fm7
            | Platform::FmTowns
    ) || download::is_download_platform(target)
}

/// Every non-hidden file under `path`, recursively, sorted. Ported verbatim from
/// `reference/old_archiver.rs`'s `directory_payload_paths` (line 6466).
pub fn directory_payload_paths(path: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    collect_directory_payload_paths(path, &mut paths);
    paths.sort();
    paths
}

fn collect_directory_payload_paths(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.file_name().and_then(|name| name.to_str()).map(|name| name.starts_with('.')).unwrap_or(false) {
            continue;
        }

        if path.is_dir() {
            collect_directory_payload_paths(&path, out);
        } else if path.is_file() {
            out.push(path);
        }
    }
}

/// Ported verbatim from `reference/old_archiver.rs`'s
/// `read_directory_non_disc_header_metadata` (line 6513). Public for directory scans
/// (`rom::scan::find_bundle`) -- note it matches when *any* file inside fingerprints as
/// `target`, so on its own it can't tell one game from a folder of several; scans only
/// ask it about a leaf directory (see `find_bundle`).
pub fn read_directory_non_disc_header_metadata(path: &Path, target: Platform) -> Option<NonDiscHeaderMetadata> {
    if !path.is_dir() || !is_directory_bundle_target(target) {
        return None;
    }

    if download::is_download_platform(target) {
        return match download::verdict_for_directory(path, target) {
            download::DownloadVerdict::Match(meta) => Some(meta),
            _ => None,
        };
    }

    let payloads = directory_payload_paths(path);
    for payload in payloads.iter().filter(|payload| {
        matches!(
            extension_lowercase(payload).as_str(),
            "d77" | "d88" | "88d" | "fdd" | "scp" | "wav" | "wv" | "t77" | "cas" | "rom" | "dsk" | "bin" | "raw" | "flac"
        )
    }) {
        if let Some(meta) = read_non_disc_header_metadata(payload, target) {
            return Some(meta);
        }
    }

    None
}
