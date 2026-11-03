//! The Japanese vintage-microcomputer family: MSX/MSX2/MSX turbo R/MSX2+, PC-8801, PC-98,
//! FM-7 and FM Towns.
//!
//! Each platform/media-container pair is detected on its own, the way PC Engine and PC
//! Engine CD are. Only signatures that identify a system by content are used here; anything
//! else is left to the extension/path heuristics (which is also where container shape --
//! D88/D77 layout, raw disk sizes, flux track folders -- belongs).
//!
//! Signature ranks:
//! - **S**: one signature identifies the system by itself.
//! - **A**: shared by several systems; identifies one only when combined with other info.
//!   Combinations of A-rank signals that together identify one system are used and count
//!   as S; an A-rank signal that still needs the extension/path is not used here.
//!
//! Per platform/media:
//!
//! | Platform / media         | S rank                                                     | A rank combined into S                                          | No logic                                        |
//! |--------------------------|------------------------------------------------------------|-----------------------------------------------------------------|-------------------------------------------------|
//! | MSX family, cassette     | CAS header (`1F A6 DE BA CC 13 7D 74`) at 0                |                                                                 | WAV, WavPack, FLAC audio                        |
//! | MSX family, ROM          |                                                            | `AB` id + INIT/STATEMENT/DEVICE/TEXT pointers + zero padding    | sub-ROM `CD`, mappers, BIOS                     |
//! | MSX family, disk         |                                                            |                                                                 | FAT boot sector, raw sizes (shared with PC DOS) |
//! | MSX / MSX2 / 2+ / turboR |                                                            |                                                                 | generation (no format records it)               |
//! | PC-8801, tape            | T88 header `PC-8801 Tape Image(T88)` at 0                  |                                                                 | CMT byte streams                                |
//! | PC-8801, CD              | `NEC Personal Computer PC-8801 Series CD` in the data track's first sector |                                                 |                                                 |
//! | PC-8801, disk            |                                                            |                                                                 | D88 boot code has no magic                      |
//! | PC-98, floppy            | `VFD1.00`, `T98FDDIMAGE.R0`/`.R1` at 0                     |                                                                 | FDI/HDI, raw sizes, D88 geometry, NHD           |
//! | PC-98, hard disk         |                                                            | IPL string at 4 (`IPL1`/`Linux 98`/`GRUB/98 `) + `55 AA` at 510 |                                                 |
//! | PC-98, CD                |                                                            |                                                                 | plain ISO 9660, no signature                    |
//! | FM-7, all media          |                                                            |                                                                 | D77 container, tape, BIOS                       |
//! | FM Towns, boot media     | `IPL4` at 0 of the first sector (disk image, CD data track)|                                                                 | non-bootable data discs                         |
//! | Flux containers          |                                                            |                                                                 | SCP / KryoFlux / HxC carry no system field      |
//!
//! Flux track counts and rotation speed only narrow the candidates to a family (see
//! [`flux_geometry_candidates`]); that is not a complete signature, so detection does not use it.
//!
//! A hash-match algorithm could be added later for the systems with no signature; that is
//! most realistically done at the hash database module level rather than in these parsers.

use std::path::Path;

use crate::common::models::Platform;

use super::super::models::{sanitize_serial_opt, sanitize_title_opt};
use super::super::detect::disc_fs::{read_exact_at, read_u16_le, read_u32_le};
use super::super::detect::reason::Evidence;
use super::{read_ascii_field, read_u24_be};

type LeafMetadata = Option<(Option<String>, Option<String>, Option<String>)>;

// ---------------------------------------------------------------------------
// Tape containers
// ---------------------------------------------------------------------------

/// Tries each tape container format in turn. Ported verbatim from
/// `reference/old_archiver.rs`'s `parse_audio_container_metadata_from_bytes` (line 5501).
pub fn parse_audio_container_metadata_from_bytes(bytes: &[u8]) -> LeafMetadata {
    if let Some(value) = parse_cas_container_metadata_from_bytes(bytes) {
        return Some(value);
    }
    if let Some(title) = parse_wav_container_title_from_bytes(bytes) {
        return Some((title, None, None));
    }
    if let Some(title) = parse_flac_container_title_from_bytes(bytes) {
        return Some((title, None, None));
    }
    if parse_wavpack_container_from_bytes(bytes).is_some() {
        return Some((None, None, None));
    }

    None
}

/// Ported verbatim from `reference/old_archiver.rs`'s
/// `parse_cas_container_metadata_from_bytes` (line 5518). A CAS header is the one tape
/// format that identifies its machine, so it yields a serial the others can't.
pub fn parse_cas_container_metadata_from_bytes(bytes: &[u8]) -> LeafMetadata {
    // openMSX/fMSX CAS header marker for MSX cassette images.
    const MSX_CAS_HEADER: [u8; 8] = [0x1F, 0xA6, 0xDE, 0xBA, 0xCC, 0x13, 0x7D, 0x74];
    if bytes.get(0..8) == Some(&MSX_CAS_HEADER) {
        return Some((None, Some("MSX-CAS".to_string()), Some("MSX-CAS".to_string())));
    }
    None
}

/// Reads a RIFF/WAVE file's `LIST`/`INFO`/`INAM` or `bext` title. Returns `Some(None)` when
/// the file *is* a WAV but carries no title -- distinguishing "not a WAV" from "a WAV with
/// nothing to say" matters to the caller. Ported verbatim from
/// `reference/old_archiver.rs`'s `parse_wav_container_title_from_bytes` (line 5527).
pub fn parse_wav_container_title_from_bytes(bytes: &[u8]) -> Option<Option<String>> {
    if bytes.get(0..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        return None;
    }

    let mut offset = 12usize;
    let mut title = None;
    while offset + 8 <= bytes.len() {
        let chunk_id = bytes.get(offset..offset + 4)?;
        let chunk_size = read_u32_le(bytes, offset + 4)? as usize;
        let chunk_start = offset + 8;
        let chunk_end = chunk_start.checked_add(chunk_size)?.min(bytes.len());

        if chunk_id == b"LIST" && chunk_end >= chunk_start + 4 && bytes.get(chunk_start..chunk_start + 4) == Some(b"INFO") {
            let mut info_offset = chunk_start + 4;
            while info_offset + 8 <= chunk_end {
                let info_id = bytes.get(info_offset..info_offset + 4)?;
                let info_size = read_u32_le(bytes, info_offset + 4)? as usize;
                let info_start = info_offset + 8;
                let info_end = info_start.checked_add(info_size)?.min(chunk_end);
                if info_id == b"INAM" {
                    title = read_ascii_field(bytes.get(info_start..info_end)?).and_then(|value| sanitize_title_opt(Some(value)));
                    if title.is_some() {
                        break;
                    }
                }
                // RIFF chunks are word-aligned.
                info_offset = (info_end + 1) & !1;
            }
        } else if chunk_id == b"bext" {
            title = read_ascii_field(bytes.get(chunk_start..chunk_end)?).and_then(|value| sanitize_title_opt(Some(value)));
        }

        if title.is_some() {
            return Some(title);
        }

        offset = (chunk_end + 1) & !1;
    }

    Some(None)
}

/// Ported verbatim from `reference/old_archiver.rs`'s
/// `parse_flac_container_title_from_bytes` (line 5569). Block type 4 is VORBIS_COMMENT.
pub fn parse_flac_container_title_from_bytes(bytes: &[u8]) -> Option<Option<String>> {
    if bytes.get(0..4) != Some(b"fLaC") {
        return None;
    }

    let mut offset = 4usize;
    while offset + 4 <= bytes.len() {
        let header = *bytes.get(offset)?;
        let block_type = header & 0x7F;
        let is_last = (header & 0x80) != 0;
        let block_len = read_u24_be(bytes, offset + 1)? as usize;
        let data_start = offset + 4;
        let data_end = data_start.checked_add(block_len)?.min(bytes.len());

        if block_type == 4 {
            let title = parse_flac_vorbis_comment_title(bytes.get(data_start..data_end)?);
            if title.is_some() {
                return title;
            }
        }

        offset = data_end;
        if is_last {
            break;
        }
    }

    Some(None)
}

/// Ported verbatim from `reference/old_archiver.rs`'s `parse_flac_vorbis_comment_title`
/// (line 5599).
fn parse_flac_vorbis_comment_title(bytes: &[u8]) -> Option<Option<String>> {
    let vendor_len = read_u32_le(bytes, 0)? as usize;
    let mut offset = 4usize.checked_add(vendor_len)?;
    let comment_count = read_u32_le(bytes, offset)? as usize;
    offset += 4;

    for _ in 0..comment_count {
        let comment_len = read_u32_le(bytes, offset)? as usize;
        offset += 4;
        let comment = bytes.get(offset..offset + comment_len)?;
        let text = String::from_utf8_lossy(comment).to_string();
        if let Some((key, value)) = text.split_once('=') {
            if key.eq_ignore_ascii_case("TITLE") {
                return Some(sanitize_title_opt(Some(value.trim().to_string())));
            }
        }
        offset += comment_len;
    }

    Some(None)
}

/// WavPack carries no title fields worth reading; this only confirms the container. Ported
/// verbatim from `reference/old_archiver.rs`'s `parse_wavpack_container_from_bytes` (line 5621).
pub fn parse_wavpack_container_from_bytes(bytes: &[u8]) -> Option<()> {
    if bytes.get(0..4) != Some(b"wvpk") {
        return None;
    }
    Some(())
}

// ---------------------------------------------------------------------------
// Floppy containers
// ---------------------------------------------------------------------------

/// D88/D77 images open with a 17-byte disk name field. Ported verbatim from
/// `reference/old_archiver.rs`'s `parse_d88_d77_disk_name` (line 5663).
pub fn parse_d88_d77_disk_name(bytes: &[u8]) -> Option<String> {
    read_ascii_field(bytes.get(0..17)?).and_then(|value| sanitize_title_opt(Some(value)))
}

/// Reads a FAT boot sector's volume label and serial, validating the BPB first so random
/// binary data can't produce a false match. Ported verbatim from
/// `reference/old_archiver.rs`'s `parse_dos_boot_sector_label_and_serial` (line 5668).
pub fn parse_dos_boot_sector_label_and_serial(bytes: &[u8]) -> (Option<String>, Option<String>) {
    if bytes.len() < 62 {
        return (None, None);
    }

    let jump_ok = matches!(bytes.first().copied(), Some(0xEB) | Some(0xE9));
    if !jump_ok {
        return (None, None);
    }

    let bps = read_u16_le(bytes, 11).unwrap_or(0);
    let spc = bytes.get(13).copied().unwrap_or(0);
    if !(128..=4096).contains(&bps) || spc == 0 {
        return (None, None);
    }

    let label = read_ascii_field(bytes.get(43..54).unwrap_or(&[]))
        .and_then(|value| sanitize_title_opt(Some(value)))
        .filter(|value| {
            if value.eq_ignore_ascii_case("NO NAME") {
                return false;
            }
            if value.len() < 3 {
                return false;
            }
            value.chars().any(|ch| ch.is_ascii_alphabetic())
        });

    // 0x29 is the extended-boot-signature byte; without it the serial field isn't present.
    let serial = if bytes.get(38).copied() == Some(0x29) {
        bytes.get(39..43).and_then(|raw| {
            let value = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
            sanitize_serial_opt(Some(format!("{value:08X}")))
        })
    } else {
        None
    };

    (label, serial)
}

// ---------------------------------------------------------------------------
// Platform detection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum System {
    MsxFamily,
    Pc8801,
    Pc98,
    FmTowns,
}

/// What content says about a candidate platform.
#[derive(Debug, Clone)]
pub enum MycomVerdict {
    /// A signature of this platform's own was found; the evidence is the signature.
    Match(Vec<Evidence>),
    /// A signature of a *different* system was found: this is not the candidate platform.
    Conflict,
    /// No signature either way; other heuristics decide.
    Unknown,
}

pub fn is_mycom_platform(platform: Platform) -> bool {
    matches!(
        platform,
        Platform::Msx
            | Platform::Msx2
            | Platform::MsxTurbo
            | Platform::Msx2Plus
            | Platform::Fm7
            | Platform::Pc8801
            | Platform::Pc98
            | Platform::FmTowns
    )
}

fn system_of(platform: Platform) -> Option<System> {
    match platform {
        Platform::Msx | Platform::Msx2 | Platform::MsxTurbo | Platform::Msx2Plus => Some(System::MsxFamily),
        Platform::Pc8801 => Some(System::Pc8801),
        Platform::Pc98 => Some(System::Pc98),
        Platform::FmTowns => Some(System::FmTowns),
        _ => None,
    }
}

fn verdict_for(target: Platform, found: Option<(System, Vec<Evidence>)>) -> MycomVerdict {
    match found {
        Some((system, evidence)) if Some(system) == system_of(target) => MycomVerdict::Match(evidence),
        Some(_) => MycomVerdict::Conflict,
        None => MycomVerdict::Unknown,
    }
}

const CAS_HEADER: [u8; 8] = [0x1F, 0xA6, 0xDE, 0xBA, 0xCC, 0x13, 0x7D, 0x74];
const T88_HEADER: &[u8] = b"PC-8801 Tape Image(T88)";
const PC98_VFD_MAGIC: &[u8] = b"VFD1.00";
const PC98_NFD_MAGIC_PREFIX: &[u8] = b"T98FDDIMAGE.R";
const FM_TOWNS_IPL: &[u8] = b"IPL4";
const PC8801_CD_HEADER: &[u8] = b"NEC Personal Computer PC-8801 Series CD";

/// Reads `path` and compares its content signatures against `target`. Directories and
/// unreadable files are `Unknown`.
pub fn verdict_for_file(path: &Path, target: Platform) -> MycomVerdict {
    if !path.is_file() {
        return MycomVerdict::Unknown;
    }
    verdict_for(target, find_file_signature(path))
}

/// The signature in a single image file, if any: the S-rank ones by themselves, plus the A-rank
/// combinations that together identify one system.
fn find_file_signature(path: &Path) -> Option<(System, Vec<Evidence>)> {
    let len = std::fs::metadata(path).ok()?.len() as usize;
    let head = read_exact_at(path, 0, len.min(0x200)).ok()?;
    let magic = |offset: usize, bytes: &[u8]| Evidence::Magic { offset, bytes: bytes.to_vec() };

    if head.starts_with(&CAS_HEADER) {
        return Some((System::MsxFamily, vec![magic(0, &CAS_HEADER)]));
    }
    if head.starts_with(T88_HEADER) {
        return Some((System::Pc8801, vec![magic(0, T88_HEADER)]));
    }
    if head.starts_with(PC98_VFD_MAGIC) {
        return Some((System::Pc98, vec![magic(0, PC98_VFD_MAGIC)]));
    }
    if head.starts_with(PC98_NFD_MAGIC_PREFIX) && matches!(head.get(13), Some(b'0') | Some(b'1')) {
        return Some((System::Pc98, vec![magic(0, &head[..14])]));
    }
    if head.starts_with(FM_TOWNS_IPL) {
        return Some((System::FmTowns, vec![magic(0, FM_TOWNS_IPL)]));
    }

    // PC-98 boot sector: an IPL string at 4 alone is shared with Fujitsu's FMR, but together
    // with the partition-table signature at 510 it is the PC-98 hard-disk layout.
    if head.len() >= 512 && head.get(510..512) == Some(&[0x55, 0xAA]) {
        for ipl in [&b"IPL1"[..], &b"Linux 98"[..], &b"GRUB/98 "[..]] {
            if head.get(4..4 + ipl.len()) == Some(ipl) {
                return Some((System::Pc98, vec![magic(4, ipl), magic(510, &[0x55, 0xAA])]));
            }
        }
    }

    if let Some(evidence) = msx_rom_header_evidence(path, len, &head) {
        return Some((System::MsxFamily, evidence));
    }
    None
}

/// An MSX cartridge header is a 16-byte block at the start of a 16K page: `AB`, then the
/// INIT, STATEMENT, DEVICE and TEXT addresses (each 0 or inside the cartridge slot area
/// 0x4000-0xBFFF), then six zero bytes. `AB` alone is two bytes; the address and padding
/// checks together are what make it a signature.
fn msx_rom_header_evidence(path: &Path, len: usize, head: &[u8]) -> Option<Vec<Evidence>> {
    let header_at = |offset: usize| -> Option<Vec<u8>> {
        if offset == 0 {
            head.get(0..16).map(<[u8]>::to_vec)
        } else if len >= offset + 16 {
            read_exact_at(path, offset as u64, 16).ok()
        } else {
            None
        }
    };
    for offset in [0usize, 0x4000] {
        let Some(header) = header_at(offset) else { continue };
        if &header[0..2] != b"AB" || header[10..16] != [0u8; 6] {
            continue;
        }
        let pointers = [
            u16::from_le_bytes([header[2], header[3]]),
            u16::from_le_bytes([header[4], header[5]]),
            u16::from_le_bytes([header[6], header[7]]),
            u16::from_le_bytes([header[8], header[9]]),
        ];
        let in_slot = |pointer: u16| pointer == 0 || (0x4000..=0xBFFF).contains(&pointer);
        if pointers.iter().all(|&p| in_slot(p)) && pointers.iter().any(|&p| p != 0) {
            return Some(vec![Evidence::Bytes {
                offset_start: offset,
                offset_end: offset + 16,
                bytes: header,
                note: "MSX cartridge header: AB id, INIT/STATEMENT/DEVICE/TEXT addresses in 0x4000-0xBFFF, zero padding".to_string(),
            }]);
        }
    }
    None
}

/// The signature in a CD track dump (`.bin` of a cue sheet), if any. The system's signature is
/// in the first sector that holds data: a track file may start with a run of empty pregap
/// sectors, and raw (2352-byte, sync-headed) and cooked (2048-byte) tracks both occur.
fn find_cd_track_signature(path: &Path) -> Option<(System, Vec<Evidence>)> {
    const WINDOW: usize = 0x10_0000;
    const SYNC: [u8; 12] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
    let len = std::fs::metadata(path).ok()?.len() as usize;
    let bytes = read_exact_at(path, 0, len.min(WINDOW)).ok()?;

    // Pregap sectors may be blank with no sync header, so the file size decides as well.
    let raw = bytes.starts_with(&SYNC) || (len % 2352 == 0 && len % 2048 != 0);
    let (sector_len, user_offset) = if raw { (2352usize, 16usize) } else { (2048, 0) };
    let mut sector = 0usize;
    while (sector + 1) * sector_len <= bytes.len() {
        let start = sector * sector_len + user_offset;
        let user = bytes.get(start..start + 2048.min(sector_len - user_offset))?;
        if user.iter().any(|&b| b != 0) {
            let magic = |signature: &[u8]| Evidence::Magic { offset: start, bytes: signature.to_vec() };
            if user.starts_with(FM_TOWNS_IPL) {
                return Some((System::FmTowns, vec![magic(FM_TOWNS_IPL)]));
            }
            if user.starts_with(PC8801_CD_HEADER) {
                return Some((System::Pc8801, vec![magic(PC8801_CD_HEADER)]));
            }
            return None;
        }
        sector += 1;
    }
    None
}

/// Same as [`verdict_for_file`] for the track files a cue/gdi sheet references: the first
/// payload carrying a signature decides.
pub fn verdict_for_cd_tracks(tracks: &[std::path::PathBuf], target: Platform) -> MycomVerdict {
    for track in tracks {
        if let Some(found) = find_cd_track_signature(track) {
            return verdict_for(target, Some(found));
        }
    }
    MycomVerdict::Unknown
}

// ---------------------------------------------------------------------------
// Flux geometry (not used by detection)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FluxRpm {
    Rpm300,
    Rpm360,
}

/// The platforms a flux dump's rotation speed and cylinder count are consistent with. Capture
/// tools often read a few cylinders past the disk's last, so the cylinder counts are ranges.
/// It narrows to a family, never to one system (PC-98 and FM Towns 2HD disks share one
/// layout, as do PC-88 and FM-7 2D disks), so it is not a complete signature and nothing in
/// detection consults it.
pub fn flux_geometry_candidates(rpm: FluxRpm, cylinders: u32) -> &'static [Platform] {
    match (rpm, cylinders) {
        (FluxRpm::Rpm360, 77..=84) => &[Platform::Pc98, Platform::FmTowns],
        (FluxRpm::Rpm300, 40..=42) => &[Platform::Pc8801, Platform::Fm7],
        (FluxRpm::Rpm300, 80..=84) => &[Platform::Pc8801, Platform::Msx, Platform::Msx2],
        _ => &[],
    }
}
