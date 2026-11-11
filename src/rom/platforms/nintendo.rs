//! Nintendo platform header parsers: NES/Famicom (iNES + UNIF), Famicom Disk System,
//! Game Boy/Color/Advance, DS/DSi, 3DS/New 3DS (NCSD + CIA), Super Famicom, Sufami Turbo,
//! Nintendo 64, Wii U, and GameCube/Wii disc boot headers.
//!
//! Every parser here is ported from `reference/old_archiver.rs` with its logic unchanged;
//! only the calling convention differs (tuple-returning leaf parsers, assembled into
//! [`NonDiscHeaderMetadata`] by `super::read_non_disc_header_metadata`).

use std::fs;
use std::path::Path;

use crate::common::models::Platform;

use super::super::models::{sanitize_serial_opt, sanitize_title_opt, RomByteOrder};
use super::super::detect::disc_fs::{read_exact_at, read_u16_le, read_u32_le, read_u64_le};
use super::{
    align_up_u64, read_ascii_field, read_detection_prefix, read_u16_be, read_u32_be, read_u64_be, NonDiscHeaderMetadata,
};

type LeafMetadata = Option<(Option<String>, Option<String>, Option<String>)>;

// ---------------------------------------------------------------------------
// NES / Famicom / Famicom Disk System
// ---------------------------------------------------------------------------

/// A file this large (or empty) can't plausibly be a real NES/Famicom cartridge dump under
/// any known container (iNES, UNIF, or a bare headerless dump) -- known real dumps (large
/// multicarts included) top out in the low single-digit megabytes, so this cap sits
/// comfortably above any legitimate size while still excluding a large unrelated file
/// merely named `something.nes` (a real false-positive risk for `forced_platform`/a
/// single-candidate extension match, which otherwise do no content validation for this
/// platform at all) from being accepted as a real dump.
pub const MAX_PLAUSIBLE_NES_FILE_SIZE: u64 = 0x800000; // 8MB

pub fn is_plausible_nes_file_size(file_size: Option<u64>) -> bool {
    matches!(file_size, Some(size) if size > 0 && size <= MAX_PLAUSIBLE_NES_FILE_SIZE)
}

/// Ported from `reference/old_archiver.rs`'s `read_nes_container_metadata` (line 5450),
/// plus two checks old code never had:
///
/// 1. The [`MAX_PLAUSIBLE_NES_FILE_SIZE`] gate up front -- old code had no size check for
///    the iNES/UNIF branches at all, so a file of any size starting with the right 4-byte
///    magic was accepted outright.
/// 2. A hash check against the known-game database (`convert::nes::classify`, backed
///    by `fcdb.bin` when present), which does two jobs, not just one: it confirms/enriches
///    a file the shape/magic checks below already recognized (`hash_confirmed` records
///    that, purely as evidence -- see [`NonDiscHeaderMetadata::hash_confirmed`]'s doc
///    comment), but it's also consulted as a real fallback *classifier* -- `headered` is
///    itself set from the database's answer when none of the shape/magic checks below
///    matched at all. A hash match is real, verified evidence (this exact byte-for-byte
///    content was checked against a known game when the database was built), stronger than
///    any of the shape/magic heuristics, so a file the heuristics can't place at all (an
///    unusual mapper, a corrupted/nonstandard header) but whose hash IS in the database
///    still gets classified correctly rather than rejected. A hash miss is never a
///    rejection on its own, though: an uncatalogued game, or no database loaded at all,
///    still passes when the shape/magic checks succeed on their own merits.
pub fn read_nes_container_metadata(path: &Path) -> Option<NonDiscHeaderMetadata> {
    let file_size = fs::metadata(path).ok()?.len();
    if !is_plausible_nes_file_size(Some(file_size)) {
        return None;
    }

    let bytes = read_detection_prefix(path).ok()?;
    // Read once, up front, and reuse for both the hash classification below and (if the
    // shape/magic checks all fail) the fallback classification itself -- rather than the
    // shape/magic path succeeding, then a *second*, separate full-file read just to compute
    // `hash_confirmed` afterwards.
    let full_bytes = fs::read(path).ok();
    let hash_match =
        full_bytes.as_deref().map(super::super::convert::nes::classify).unwrap_or(super::super::convert::nes::NesHashMatch::NotNes);

    let mut meta = if bytes.get(0..4) == Some(b"UNIF") {
        read_unif_metadata(path).map(|mut meta| {
            meta.headered = Some(true);
            meta
        })
    } else if bytes.get(0..4) == Some(b"NES\x1A") {
        Some(NonDiscHeaderMetadata { title: read_nes_tail_title(path), headered: Some(true), ..Default::default() })
    } else {
        read_nes_headerless_shape(path)
    }
    .or_else(|| match hash_match {
        super::super::convert::nes::NesHashMatch::Headered => Some(NonDiscHeaderMetadata { headered: Some(true), ..Default::default() }),
        super::super::convert::nes::NesHashMatch::Headerless => Some(NonDiscHeaderMetadata { headered: Some(false), ..Default::default() }),
        super::super::convert::nes::NesHashMatch::NotNes => None,
    })?;

    meta.hash_confirmed = Some(hash_match != super::super::convert::nes::NesHashMatch::NotNes);
    Some(meta)
}

/// A headerless NES/Famicom dump (no iNES/UNIF magic) has no field left to check, so this
/// validates *shape* instead, the same way [`super::sega::parse_sg1000_rom_shape`] and
/// [`super::other::parse_pce_hucard_metadata_from_bytes`] do for their own headerless
/// cartridges: PRG-ROM banks are 0x4000 (16KB) and CHR-ROM banks are 0x2000 (8KB), so a
/// real dump's size is always a whole multiple of 0x2000. The overall size ceiling already
/// ran in [`read_nes_container_metadata`] before this is ever called, so this only checks
/// the bank-size shape constraint that's specific to a headerless dump.
fn read_nes_headerless_shape(path: &Path) -> Option<NonDiscHeaderMetadata> {
    let file_size = fs::metadata(path).ok()?.len();
    if file_size == 0 || file_size % 0x2000 != 0 {
        return None;
    }
    Some(NonDiscHeaderMetadata { headered: Some(false), ..Default::default() })
}

/// Scans for the NES PPU warm-up idiom -- absolute-mode stores to PPUCTRL ($2000) then
/// PPUMASK ($2001) within a few bytes of each other -- the near-universal first thing a
/// real NES game's boot code does (disable NMI, disable rendering, before the two-vblank
/// wait almost every official/homebrew NES init routine performs). A genuinely headerless
/// dump gives no iNES-declared PRG/CHR split or mapper number to compute a precise reset
/// vector from, so unlike [`super::other::find_supergrafx_vpc_init`]'s bounded early-boot
/// window, this scans the WHOLE buffer -- which works regardless of mapper/bank layout,
/// and is still safe from false positives: CHR-ROM tile data is graphics, not code, and is
/// very unlikely to coincidentally form this exact 6-byte instruction pair. Returns the
/// first match's offset.
pub fn find_nes_ppu_init(bytes: &[u8]) -> Option<usize> {
    const ABS_STORE_OPCODES: [u8; 3] = [0x8D, 0x8E, 0x8C]; // STA/STX/STY, absolute
    const GAP: usize = 8;

    for i in 0..bytes.len().saturating_sub(6) {
        if !ABS_STORE_OPCODES.contains(&bytes[i]) || bytes.get(i + 1..i + 3) != Some(&[0x00, 0x20]) {
            continue;
        }
        let window_end = (i + 3 + GAP).min(bytes.len().saturating_sub(2));
        for j in i + 3..window_end {
            if ABS_STORE_OPCODES.contains(&bytes[j]) && bytes.get(j + 1..j + 3) == Some(&[0x01, 0x20]) {
                return Some(i);
            }
        }
    }
    None
}

/// Reads UNIF's `NAME` (title) and `MAPR` (board name, used as the serial) chunks. Ported
/// verbatim from `reference/old_archiver.rs`'s `read_unif_metadata` (line 5740).
fn read_unif_metadata(path: &Path) -> Option<NonDiscHeaderMetadata> {
    let bytes = fs::read(path).ok()?;
    if bytes.get(0..4) != Some(b"UNIF") || bytes.len() < 32 {
        return None;
    }

    let mut offset = 32usize;
    let mut title = None;
    let mut board = None;
    while offset + 8 <= bytes.len() {
        let chunk_type = bytes.get(offset..offset + 4)?;
        let chunk_len = read_u32_le(&bytes, offset + 4)? as usize;
        let data_start = offset + 8;
        let data_end = data_start.checked_add(chunk_len)?;
        let data = bytes.get(data_start..data_end)?;

        match chunk_type {
            b"NAME" => title = read_ascii_field(data).and_then(|value| sanitize_title_opt(Some(value))),
            b"MAPR" => board = read_ascii_field(data).and_then(|value| sanitize_serial_opt(Some(value))),
            _ => {}
        }

        offset = data_end;
    }

    if title.is_none() && board.is_none() {
        return None;
    }

    Some(NonDiscHeaderMetadata { title, serial: board, ..Default::default() })
}

/// Some iNES dumps carry a 128-byte title block at EOF. Ported verbatim from
/// `reference/old_archiver.rs`'s `read_nes_tail_title` (line 5772).
fn read_nes_tail_title(path: &Path) -> Option<String> {
    let file_size = fs::metadata(path).ok()?.len();
    if file_size < 128 {
        return None;
    }

    let tail = read_exact_at(path, file_size - 128, 128).ok()?;
    let text = read_ascii_field(&tail)?;
    if text.len() < 6 {
        return None;
    }
    sanitize_title_opt(Some(text))
}

/// Ported verbatim from `reference/old_archiver.rs`'s `parse_fds_qd_metadata_from_bytes`
/// (line 5476). Accepts both the headered (`FDS\x1A`) and raw QD layouts.
pub fn parse_fds_qd_metadata_from_bytes(bytes: &[u8]) -> LeafMetadata {
    let payload = if bytes.get(0..4) == Some(b"FDS\x1A") { bytes.get(16..)? } else { bytes };
    if payload.len() < 0x20 {
        return None;
    }

    if payload.get(1..15)? != b"*NINTENDO-HVC*" {
        return None;
    }

    let serial = read_ascii_field(payload.get(0x10..0x13)?).and_then(|value| sanitize_serial_opt(Some(value)));
    let manufacturer_id = payload
        .get(0x0F)
        .copied()
        .filter(|value| *value != 0x00 && *value != 0xFF)
        .map(|value| format!("{value:02X}"));

    Some((None, serial, manufacturer_id))
}

// ---------------------------------------------------------------------------
// Game Boy / Game Boy Color / Game Boy Advance
// ---------------------------------------------------------------------------

/// Reads a Game Boy header, rejecting the file if the CGB flag at 0x143 disagrees with
/// `target` (GB and GBC share one header layout and differ only by that flag). Ported from
/// `reference/old_archiver.rs`'s `read_non_disc_header_metadata`'s GameBoy arm (line 5260),
/// with the GB/GBC gate folded in so the caller matches on platform only once.
pub fn parse_gameboy_metadata_from_bytes(bytes: &[u8], target: Platform) -> LeafMetadata {
    let cgb_flag = bytes.get(0x143).copied().unwrap_or(0);
    if matches!(target, Platform::GameBoy) && (cgb_flag & 0x80) != 0 {
        return None;
    }
    if matches!(target, Platform::GameBoyColor) && (cgb_flag & 0x80) == 0 {
        return None;
    }

    let title = parse_gameboy_title_from_bytes(bytes)?;
    Some((Some(title), None, None))
}

/// Game Boy header checksum: the byte at 0x14D is `-(sum of 0x134..=0x14C) - 1 (per byte)`,
/// which the boot ROM verifies.
pub fn gameboy_header_checksum_ok(bytes: &[u8]) -> bool {
    let Some(range) = bytes.get(0x134..=0x14C) else { return false };
    let sum = range.iter().fold(0u8, |acc, &b| acc.wrapping_sub(b).wrapping_sub(1));
    bytes.get(0x14D).copied() == Some(sum)
}

/// Requires the Nintendo boot logo and header checksum to match before trusting the title field -- the logo is
/// checked by the console's own boot ROM, so a dump that has it is almost certainly a real
/// GB cartridge. Ported verbatim from `reference/old_archiver.rs`'s
/// `parse_gameboy_title_from_bytes` (line 5856).
pub fn parse_gameboy_title_from_bytes(bytes: &[u8]) -> Option<String> {
    const NINTENDO_LOGO: [u8; 48] = [
        0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83, 0x00, 0x0C, 0x00, 0x0D, 0x00, 0x08, 0x11,
        0x1F, 0x88, 0x89, 0x00, 0x0E, 0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD, 0xD9, 0x99, 0xBB, 0xBB, 0x67, 0x63, 0x6E, 0x0E,
        0xEC, 0xCC, 0xDD, 0xDC, 0x99, 0x9F, 0xBB, 0xB9, 0x33, 0x3E,
    ];
    if bytes.get(0x104..0x134) != Some(&NINTENDO_LOGO[..]) || !gameboy_header_checksum_ok(bytes) {
        return None;
    }
    read_ascii_field(bytes.get(0x134..0x144)?)
}

/// The 156-byte Nintendo boot logo shared by GBA (at 0x04) and DS/DSi (at 0xC0). Both consoles'
/// boot ROMs verify it, and its CRC-16 (MODBUS: init 0xFFFF, reflected poly 0xA001) is the
/// constant 0xCF56, so a match proves the logo without embedding it.
pub const NINTENDO_LOGO_CRC16: u16 = 0xCF56;
pub const NINTENDO_LOGO_LEN: usize = 156;

pub fn crc16_modbus(bytes: &[u8]) -> u16 {
    let mut crc = 0xFFFFu16;
    for &byte in bytes {
        crc ^= u16::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xA001 } else { crc >> 1 };
        }
    }
    crc
}

pub fn nintendo_logo_crc_ok(logo: &[u8]) -> bool {
    logo.len() == NINTENDO_LOGO_LEN && crc16_modbus(logo) == NINTENDO_LOGO_CRC16
}

/// GBA header complement check: the byte at 0xBD is `-(sum of 0xA0..=0xBC) - 0x19`.
pub fn gba_header_complement_ok(bytes: &[u8]) -> bool {
    let Some(range) = bytes.get(0xA0..=0xBC) else { return false };
    let sum = range.iter().fold(0u8, |acc, &b| acc.wrapping_sub(b)).wrapping_sub(0x19);
    bytes.get(0xBD).copied() == Some(sum)
}

/// A GBA header is recognized by three things the boot ROM itself enforces: the Nintendo
/// logo at 0x04, the fixed byte 0x96 at 0xB2, and the header complement at 0xBD.
pub fn parse_gba_header_metadata_from_bytes(bytes: &[u8]) -> LeafMetadata {
    if bytes.get(0x0B2).copied() != Some(0x96) {
        return None;
    }
    if !nintendo_logo_crc_ok(bytes.get(0x04..0x04 + NINTENDO_LOGO_LEN)?) || !gba_header_complement_ok(bytes) {
        return None;
    }

    let title = read_ascii_field(bytes.get(0x0A0..0x0AC)?).and_then(|value| sanitize_title_opt(Some(value)));
    let serial = read_ascii_field(bytes.get(0x0AC..0x0B0)?).and_then(|value| sanitize_serial_opt(Some(value)));
    let manufacturer_id = read_ascii_field(bytes.get(0x0B0..0x0B2)?).and_then(|value| sanitize_serial_opt(Some(value)));

    if title.is_none() && serial.is_none() && manufacturer_id.is_none() {
        return None;
    }

    Some((title, serial, manufacturer_id))
}

// ---------------------------------------------------------------------------
// Nintendo DS / DSi
// ---------------------------------------------------------------------------

/// Ported from `reference/old_archiver.rs`'s `parse_nds_header_metadata_from_bytes` (line
/// 5889), with the DS/DSi unit-code gate from its caller (line 5280) folded in so the
/// dispatch matches on platform only once, plus a new `encrypted` read (no precedent in old
/// code). The Nintendo boot logo at 0xC0 (CRC-checked) is the signature; the 4-character
/// game code at 0x0C must additionally be well-formed.
pub fn parse_nds_header_metadata_from_bytes(path: &Path, bytes: &[u8], target: Platform) -> Option<NonDiscHeaderMetadata> {
    // Unit code 0x03 marks a DSi-exclusive title.
    let unit_code = bytes.get(0x12).copied().unwrap_or(0);
    if matches!(target, Platform::NintendoDs) && unit_code == 0x03 {
        return None;
    }
    if matches!(target, Platform::NintendoDsi) && unit_code != 0x03 {
        return None;
    }
    // A DSi title-id file type of 04 marks downloadable DSiWare, which is its own platform.
    if matches!(target, Platform::NintendoDsi) && bytes.get(0x234).copied() == Some(0x04) && bytes.get(0x236..0x238) == Some(&[0x03, 0x00][..]) {
        return None;
    }

    if !nintendo_logo_crc_ok(bytes.get(0xC0..0xC0 + NINTENDO_LOGO_LEN)?) {
        return None;
    }

    let title = read_ascii_field(bytes.get(0x00..0x0C)?).and_then(|value| sanitize_title_opt(Some(value)));
    let serial = read_ascii_field(bytes.get(0x0C..0x10)?).and_then(|value| sanitize_serial_opt(Some(value)));
    let manufacturer_id = read_ascii_field(bytes.get(0x10..0x12)?).and_then(|value| sanitize_serial_opt(Some(value)));

    let serial = match serial {
        Some(value) if value.len() == 4 && value.chars().all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit()) => Some(value),
        _ => None,
    };
    serial.as_ref()?;

    Some(NonDiscHeaderMetadata { title, serial, manufacturer_id, encrypted: nds_secure_area_encrypted(path, bytes), ..Default::default() })
}

/// The NDS "secure area" is the 0x800 bytes at ROM offset 0x4000, the start of the ARM9 binary
/// on a retail cartridge. On the cartridge it is KEY1-encrypted, and its first 8 bytes hold the
/// ID `encryObj` (double-encrypted). Decrypting it replaces those 8 bytes with two
/// `E7FFDEFF` words (a Thumb undefined-instruction filler), so that pair -- or `encryObj` seen
/// in the clear, a half-decrypted state -- means the dump is decrypted, and anything else
/// there is still ciphertext.
///
/// A ROM whose ARM9 binary does not start at 0x4000 (homebrew, for one) has no secure area at
/// all, so there is no encryption state to report and this returns `None`.
fn nds_secure_area_encrypted(path: &Path, header: &[u8]) -> Option<bool> {
    if read_u32_le(header, 0x20)? != 0x4000 {
        return None;
    }
    let marker = read_exact_at(path, 0x4000, 8).ok()?;
    let decrypted = marker == [0xFF, 0xDE, 0xFF, 0xE7, 0xFF, 0xDE, 0xFF, 0xE7] || marker == *b"encryObj";
    Some(!decrypted)
}

// ---------------------------------------------------------------------------
// Nintendo 64DD
// ---------------------------------------------------------------------------

/// 64DD System Data (LBA 0) region codes: JP retail, US retail, and all-zero for development
/// disks.
pub const N64DD_REGION_JP: [u8; 4] = [0xE8, 0x48, 0xD3, 0x16];
pub const N64DD_REGION_US: [u8; 4] = [0x22, 0x63, 0xEE, 0x56];
pub const N64DD_REGION_DEV: [u8; 4] = [0; 4];

/// A 64DD disk image begins with its System Data: a region code at 0x00 (see the
/// `N64DD_REGION_*` constants), a format-type byte at 0x04 that is always 0x10, and a reserved
/// word at 0x18 that is always 0xFFFFFFFF. Retail disks also carry a reserved 0xFFFF at
/// 0xE6 (development disks only use the first 0xC0 bytes).
pub fn read_n64dd_system_data_metadata(path: &Path) -> Option<NonDiscHeaderMetadata> {
    let bytes = read_exact_at(path, 0, 0xE8).ok()?;
    let region: [u8; 4] = bytes.get(0..4)?.try_into().ok()?;
    let retail = region == N64DD_REGION_JP || region == N64DD_REGION_US;
    if !retail && region != N64DD_REGION_DEV {
        return None;
    }
    if bytes.get(0x04).copied() != Some(0x10) || bytes.get(0x18..0x1C)? != [0xFF; 4] {
        return None;
    }
    if retail && bytes.get(0xE6..0xE8)? != [0xFF; 2] {
        return None;
    }
    Some(NonDiscHeaderMetadata::default())
}

// ---------------------------------------------------------------------------
// Nintendo 3DS / New 3DS
// ---------------------------------------------------------------------------

/// `KTR-` prefixed product codes are New 3DS exclusives; `CTR-` are original 3DS.
/// **Centralized here** -- old code duplicated this check at two call sites
/// (`deep_read_header_metadata` line 5164 and `read_non_disc_header_metadata` line 5333).
pub fn disambiguate_3ds_by_serial(serial: Option<&str>) -> Option<Platform> {
    let serial = serial?;
    if serial.starts_with("KTR-") {
        Some(Platform::NewNintendo3ds)
    } else if serial.starts_with("CTR-") {
        Some(Platform::Nintendo3ds)
    } else {
        None
    }
}

/// Ported from `reference/old_archiver.rs`'s `read_3ds_ncsd_metadata` (line 6216), with its
/// caller's New-3DS gate (line 5327) folded in (a New 3DS exclusive must have a `KTR-`
/// product code, so anything else is rejected when that's the target).
pub fn read_3ds_ncsd_metadata(path: &Path, target: Platform) -> Option<NonDiscHeaderMetadata> {
    let meta = read_3ds_container_metadata(path)?;
    if matches!(target, Platform::NewNintendo3ds) {
        let is_new_3ds =
            meta.serial.as_deref().or(meta.manufacturer_id.as_deref()).map(|value| value.starts_with("KTR-")).unwrap_or(false);
        if !is_new_3ds {
            return None;
        }
    }
    Some(meta)
}

/// Tries both 3DS container layouts, NCSD (a cartridge dump) then CIA (an installable
/// title). Old code (line 6222) chose between them on the extension alone and so couldn't
/// read a misnamed container at all; each layout validates its own magic, so trying both
/// costs one cheap failed read and never mismatches.
///
/// A bare NCCH (`.cxi`) is read as well, and so is Azahar's compressed form of any of the
/// three (`.zcci`, `.zcia`, `.zcxi`): its first megabytes are unpacked and read the same way.
/// No offset into a compressed file means anything, so it has no `serial_offset`; its
/// `titleinfo` metadata gives the title id when the header in it doesn't.
pub(crate) fn read_3ds_container_metadata(path: &Path) -> Option<NonDiscHeaderMetadata> {
    if let Some(head) = super::super::n3ds::z3ds_head(path, Z3DS_HEAD) {
        let src = Src::Bytes(&head);
        let mut meta = read_3ds_ncsd_container_metadata(src).or_else(|| read_3ds_ncch_metadata(src)).or_else(|| cia_metadata(src));
        let title_id = super::super::n3ds::z3ds_title_id(path).map(|id| format!("{id:016X}"));
        match &mut meta {
            Some(m) => {
                m.serial_offset = None;
                m.header_offset = None;
                if m.manufacturer_id.is_none() {
                    m.manufacturer_id = title_id;
                }
            }
            None => meta = title_id.map(|id| NonDiscHeaderMetadata { serial: Some(id.clone()), manufacturer_id: Some(id), encrypted: Some(false), ..Default::default() }),
        }
        return meta;
    }
    let src = Src::File(path);
    read_3ds_ncsd_container_metadata(src).or_else(|| read_3ds_ncch_metadata(src)).or_else(|| cia_metadata(src))
}

/// How much of a compressed 3DS file is unpacked to read its headers: past a CIA's
/// certificates, ticket and TMD to its first content, or a cartridge image's first partition.
const Z3DS_HEAD: usize = 1 << 20;

/// Where the 3DS readers read from: the file, or the unpacked start of a compressed one.
#[derive(Clone, Copy)]
enum Src<'a> {
    File(&'a Path),
    Bytes(&'a [u8]),
}

impl Src<'_> {
    fn at(self, offset: u64, len: usize) -> Option<Vec<u8>> {
        match self {
            Src::File(path) => read_exact_at(path, offset, len).ok(),
            Src::Bytes(bytes) => bytes.get(usize::try_from(offset).ok()?..usize::try_from(offset).ok()?.checked_add(len)?).map(<[u8]>::to_vec),
        }
    }
}

/// A bare NCCH: a `.cxi` (or `.cfa`), the header at the start.
fn read_3ds_ncch_metadata(src: Src) -> Option<NonDiscHeaderMetadata> {
    let ncch = src.at(0, 0x200)?;
    if ncch.get(0x100..0x104) != Some(b"NCCH") {
        return None;
    }
    let product_code = read_ascii_field(ncch.get(0x150..0x160)?).and_then(|value| sanitize_serial_opt(Some(value)));
    let program_id = read_u64_le(&ncch, 0x118).map(|value| format!("{value:016X}")).and_then(|value| sanitize_serial_opt(Some(value)));
    let serial = combine_product_code_and_id(product_code.clone(), program_id.clone());
    serial.as_ref()?;
    let encrypted = ncch.get(0x18F).map(|flags| (flags & 0x04) == 0);
    let serial_offset = product_code.is_some().then_some((0x150, 0x160));
    Some(NonDiscHeaderMetadata { serial, manufacturer_id: program_id, encrypted, serial_offset, ..Default::default() })
}

fn read_3ds_ncsd_container_metadata(src: Src) -> Option<NonDiscHeaderMetadata> {
    let ncsd = src.at(0x100, 0x100)?;
    if ncsd.get(0..4) != Some(b"NCSD") {
        return None;
    }

    let part0_media_units = read_u32_le(&ncsd, 0x20)? as u64;
    if part0_media_units == 0 {
        return None;
    }

    let ncch_offset = part0_media_units.checked_mul(0x200)?;
    let ncch = src.at(ncch_offset, 0x200)?;
    if ncch.get(0x100..0x104) != Some(b"NCCH") {
        return None;
    }

    let product_code = read_ascii_field(ncch.get(0x150..0x160)?).and_then(|value| sanitize_serial_opt(Some(value)));
    let program_id = read_u64_le(&ncch, 0x118).map(|value| format!("{value:016X}")).and_then(|value| sanitize_serial_opt(Some(value)));

    let serial = combine_product_code_and_id(product_code.clone(), program_id.clone());
    if serial.is_none() && program_id.is_none() {
        return None;
    }

    // NCCH "Flags" bitmask byte (NCCH header offset 0x18F): bit 2 (0x04) is "NoCrypto" --
    // when set, this NCCH's content is *not* AES-CTR encrypted (a decrypted dump or
    // homebrew build); when clear, it's the normal retail-encrypted form.
    let encrypted = ncch.get(0x18F).map(|flags| (flags & 0x04) == 0);

    // The product code's real absolute file offset -- `ncch_offset` is computed per-file
    // from the NCSD partition table above, so this is never the same number across
    // different dumps (unlike the `0x150` NCCH-relative constant used to read it).
    let serial_offset = product_code.is_some().then_some((ncch_offset + 0x150, ncch_offset + 0x160));

    Some(NonDiscHeaderMetadata { serial, manufacturer_id: program_id, encrypted, serial_offset, ..Default::default() })
}

/// Which container kind an NCSD header describes -- 3DS NAND/eMMC dumps and ROM
/// cartridge dumps share the exact same `NCSD` container format, distinguished only by
/// this field. Confirmed via GBATEK's NCSD Format page (media-type index, header offset
/// `0x18D`): `0` = `InnerDevice` (eMMC/NAND), `1`/`2` = Card1/Card2 (a cartridge dump),
/// `3` = `ExtendedDevice`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreeDsMediaType {
    InnerDevice,
    Card1,
    Card2,
    ExtendedDevice,
}

/// Standalone from [`read_3ds_ncsd_container_metadata`] above: a NAND/eMMC dump has no
/// game serial to extract, so this only classifies which container kind the header
/// describes, reusing the same `0x100`-byte header window that function already reads.
/// Used by the NAND/SD-image classification pass (`detect::nand_dispatch`), not by the
/// ordinary ROM detection cycle.
pub fn read_3ds_ncsd_media_type(path: &Path) -> Option<ThreeDsMediaType> {
    let ncsd = read_exact_at(path, 0x100, 0x100).ok()?;
    if ncsd.get(0..4) != Some(b"NCSD") {
        return None;
    }
    match ncsd.get(0x8D)? {
        // 0x18D absolute == 0x8D within this 0x100-byte window starting at file offset 0x100.
        0 => Some(ThreeDsMediaType::InnerDevice),
        1 => Some(ThreeDsMediaType::Card1),
        2 => Some(ThreeDsMediaType::Card2),
        3 => Some(ThreeDsMediaType::ExtendedDevice),
        _ => None,
    }
}

/// Whether the NCSD header at `path` describes a NAND/eMMC dump rather than a ROM
/// cartridge. High confidence -- reuses the already-implemented, already-verified NCSD
/// magic check above, just reading one further discriminator byte.
pub fn is_3ds_nand_dump(path: &Path) -> Option<bool> {
    Some(matches!(read_3ds_ncsd_media_type(path)?, ThreeDsMediaType::InnerDevice))
}

/// Walks a CIA's cert/ticket/TMD chain to the first content NCCH. Ported from
/// `reference/old_archiver.rs`'s `read_3ds_cia_metadata` (line 6264), plus a new
/// `encrypted` read from the same TMD content_type bit old code already inspected (to
/// decide whether the NCCH was even readable) but didn't keep.
fn cia_metadata(src: Src) -> Option<NonDiscHeaderMetadata> {
    let header = src.at(0, 0x20)?;
    let archive_header_size = read_u32_le(&header, 0x00)? as u64;
    let cert_chain_size = read_u32_le(&header, 0x08)? as u64;
    let ticket_size = read_u32_le(&header, 0x0C)? as u64;
    let tmd_size = read_u32_le(&header, 0x10)? as u64;

    let cert_offset = align_up_u64(archive_header_size, 64)?;
    let ticket_offset = align_up_u64(cert_offset.checked_add(cert_chain_size)?, 64)?;
    let tmd_offset = align_up_u64(ticket_offset.checked_add(ticket_size)?, 64)?;
    let tmd_bytes = src.at(tmd_offset, tmd_size as usize)?;

    let header_offset = parse_tmd_signature_layout(&tmd_bytes)?;
    let title_id = read_u64_be(&tmd_bytes, header_offset + 0x4C)
        .map(|value| format!("{value:016X}"))
        .and_then(|value| sanitize_serial_opt(Some(value)));
    let content_count = read_u16_be(&tmd_bytes, header_offset + 0x9E)? as usize;
    let chunk_records_offset = header_offset.checked_add(0x9C4)?;

    let mut product_code = None;
    let mut encrypted = None;
    let mut serial_offset = None;
    let mut content_offset = align_up_u64(tmd_offset.checked_add(tmd_size)?, 64)?;
    for index in 0..content_count {
        let chunk_offset = chunk_records_offset.checked_add(index.checked_mul(0x30)?)?;
        let content_index = read_u16_be(&tmd_bytes, chunk_offset + 0x04)?;
        let content_type = read_u16_be(&tmd_bytes, chunk_offset + 0x06)?;
        let content_size = read_u64_be(&tmd_bytes, chunk_offset + 0x08)?;

        if content_index == 0 {
            // Bit 0 of content_type marks an encrypted content; its NCCH header isn't
            // readable in the clear, so only an unencrypted one yields a product code.
            let content_encrypted = (content_type & 0x0001) != 0;
            encrypted = Some(content_encrypted);
            if !content_encrypted {
                let ncch = src.at(content_offset, 0x200)?;
                if ncch.get(0x100..0x104) == Some(b"NCCH") {
                    product_code = read_ascii_field(ncch.get(0x150..0x160)?).and_then(|value| sanitize_serial_opt(Some(value)));
                    if product_code.is_some() {
                        // Real absolute offset -- `content_offset` is walked from the
                        // cert/ticket/TMD chain above, so it depends on this specific CIA's
                        // ticket/TMD sizes and is essentially never the same number as a
                        // direct NCSD dump's `ncch_offset + 0x150` for the same title.
                        serial_offset = Some((content_offset + 0x150, content_offset + 0x160));
                    }
                }
            }
            break;
        }

        content_offset = align_up_u64(content_offset.checked_add(content_size)?, 64)?;
    }

    // No readable product code (content still encrypted, or its NCCH didn't parse): the
    // title id read directly from the TMD header is the fallback serial, at its own real
    // absolute offset.
    if serial_offset.is_none() && title_id.is_some() {
        serial_offset = Some((tmd_offset + header_offset as u64 + 0x4C, tmd_offset + header_offset as u64 + 0x54));
    }

    let serial = combine_product_code_and_id(product_code, title_id.clone());
    if serial.is_none() && title_id.is_none() {
        return None;
    }

    Some(NonDiscHeaderMetadata { serial, manufacturer_id: title_id, encrypted, serial_offset, header_offset: Some(tmd_offset), ..Default::default() })
}

fn combine_product_code_and_id(product_code: Option<String>, id: Option<String>) -> Option<String> {
    match (product_code, id) {
        (Some(product_code), Some(id)) => sanitize_serial_opt(Some(format!("{product_code} / {id}"))),
        (Some(product_code), None) => Some(product_code),
        (None, Some(id)) => Some(id),
        (None, None) => None,
    }
}

/// TMD signatures are variable-length by type; this returns the offset where the TMD header
/// proper begins. Ported verbatim from `reference/old_archiver.rs`'s
/// `parse_tmd_signature_layout` (line 6320).
pub fn parse_tmd_signature_layout(bytes: &[u8]) -> Option<usize> {
    let signature_type = read_u32_be(bytes, 0)?;
    let (signature_size, padding_size) = match signature_type {
        0x0001_0000 | 0x0001_0003 => (0x200usize, 0x3Cusize),
        0x0001_0001 | 0x0001_0004 => (0x100usize, 0x3Cusize),
        0x0001_0002 | 0x0001_0005 => (0x3Cusize, 0x40usize),
        _ => return None,
    };
    4usize.checked_add(signature_size)?.checked_add(padding_size)
}

/// Ported verbatim from `reference/old_archiver.rs`'s `parse_tmd_title_id_from_bytes` (line 6575).
pub fn parse_tmd_title_id_from_bytes(bytes: &[u8]) -> Option<String> {
    let header_offset = parse_tmd_signature_layout(bytes)?;
    read_u64_be(bytes, header_offset + 0x4C).map(|value| format!("{value:016X}"))
}

// ---------------------------------------------------------------------------
// Super Famicom / Sufami Turbo
// ---------------------------------------------------------------------------

/// SNES carts have no fixed header location -- LoROM, HiROM and ExHiROM put it at different
/// offsets, and copier dumps prepend a 512-byte header. This scores every candidate offset
/// on how well its fields cohere (checksum complement, map mode, country code) and keeps the
/// best. Ported verbatim from `reference/old_archiver.rs`'s `read_snes_rom_metadata` (line 5786).
pub fn read_snes_rom_metadata(path: &Path) -> Option<NonDiscHeaderMetadata> {
    let file_size = fs::metadata(path).ok()?.len() as usize;
    let copier_biases: &[usize] = &[0, 0x200];
    let base_offsets = [0x7FC0usize, 0xFFC0usize, 0x40FFC0usize];
    let mut best: Option<(usize, Option<String>, bool, usize)> = None;

    for &bias in copier_biases {
        for base_offset in base_offsets {
            let Some(offset) = base_offset.checked_add(bias) else { continue };
            let Some(end) = offset.checked_add(0x40) else { continue };
            if end > file_size {
                continue;
            }

            let Ok(header) = read_exact_at(path, offset as u64, 0x40) else { continue };
            let title = read_ascii_field(header.get(0..21)?).and_then(|value| sanitize_title_opt(Some(value)));
            let map_mode = header.get(0x15).copied().unwrap_or(0);
            let country = header.get(0x19).copied().unwrap_or(0xFF);
            let checksum = read_u16_le(&header, 0x1E).unwrap_or(0) as u32;
            let checksum_complement = read_u16_le(&header, 0x1C).unwrap_or(0) as u32;

            let mut score = 0usize;
            if title.is_some() {
                score += 3;
            }
            if matches!(map_mode >> 4, 0x2 | 0x3) {
                score += 2;
            } else {
                continue;
            }
            if country <= 0x14 {
                score += 1;
            } else {
                continue;
            }
            // The checksum/complement pair is the header's only self-check; a candidate
            // without it is just bytes that happen to look plausible.
            if checksum + checksum_complement == 0xFFFF {
                score += 4;
            } else {
                continue;
            }
            if map_mode != 0xFF {
                score += 1;
            }

            if score == 0 {
                continue;
            }

            match &best {
                Some((best_score, _, _, _)) if score <= *best_score => {}
                _ => best = Some((score, title, bias != 0, offset)),
            }
        }
    }

    let (_, title, headered, header_offset) = best?;
    title.as_ref()?;
    Some(NonDiscHeaderMetadata { title, headered: Some(headered), header_offset: Some(header_offset as u64), ..Default::default() })
}

/// Ported verbatim from `reference/old_archiver.rs`'s
/// `parse_sufami_turbo_metadata_from_bytes` (line 5716).
pub fn parse_sufami_turbo_metadata_from_bytes(bytes: &[u8]) -> LeafMetadata {
    if bytes.get(0..14)? != b"BANDAI SFC-ADX" {
        return None;
    }
    Some((None, None, Some("BANDAI".to_string())))
}

// ---------------------------------------------------------------------------
// Nintendo 64
// ---------------------------------------------------------------------------

/// Ported from `reference/old_archiver.rs`'s `parse_n64_header_metadata_from_bytes` (line
/// 5931), plus surfacing the byte order `normalize_n64_header_prefix` already detects
/// internally but old code discarded once normalization was done.
pub fn parse_n64_header_metadata_from_bytes(bytes: &[u8]) -> Option<NonDiscHeaderMetadata> {
    let (normalized, byte_order) = normalize_n64_header_prefix(bytes)?;
    let title = read_ascii_field(normalized.get(0x20..0x34)?).and_then(|value| sanitize_title_opt(Some(value)));
    let serial = read_ascii_field(normalized.get(0x3B..0x3F)?).and_then(|value| sanitize_serial_opt(Some(value)));

    if title.is_none() && serial.is_none() {
        return None;
    }

    Some(NonDiscHeaderMetadata { title, serial, byte_order: Some(byte_order), ..Default::default() })
}

/// N64 dumps circulate in three byte orders (.z64 big-endian, .v64 byte-swapped, .n64
/// little-endian/word-swapped); the leading magic word identifies which, and the header is
/// normalized to big-endian before any field read. Ported from
/// `reference/old_archiver.rs`'s `normalize_n64_header_prefix` (line 6339), now also
/// returning which order it found rather than discarding that once normalized.
pub fn normalize_n64_header_prefix(bytes: &[u8]) -> Option<(Vec<u8>, RomByteOrder)> {
    let prefix = bytes.get(..0x40.min(bytes.len()))?;
    if prefix.len() < 0x40 {
        return None;
    }

    let mut normalized = prefix.to_vec();
    let byte_order = match prefix.get(0..4)? {
        [0x80, 0x37, 0x12, 0x40] => RomByteOrder::BigEndian,
        [0x37, 0x80, 0x40, 0x12] => {
            for chunk in normalized.chunks_exact_mut(2) {
                chunk.swap(0, 1);
            }
            RomByteOrder::ByteSwapped
        }
        [0x40, 0x12, 0x37, 0x80] => {
            for chunk in normalized.chunks_exact_mut(4) {
                chunk.reverse();
            }
            RomByteOrder::LittleEndian
        }
        _ => return None,
    };

    Some((normalized, byte_order))
}

// ---------------------------------------------------------------------------
// Wii U
// ---------------------------------------------------------------------------

/// Confirms a WUX container without extracting identity fields (WUX is a compressed wrapper
/// around a WUD image). Ported from `reference/old_archiver.rs`'s
/// `parse_wiiu_container_metadata_from_bytes` (line 5724), minus its `.wux` extension
/// requirement: the `WUX0` magic is proof on its own, so a misnamed file is still detected.
pub fn parse_wiiu_container_metadata_from_bytes(bytes: &[u8]) -> LeafMetadata {
    if bytes.get(0..4) != Some(b"WUX0") {
        return None;
    }
    Some((None, None, None))
}

// ---------------------------------------------------------------------------
// GameCube / Wii disc boot header
// ---------------------------------------------------------------------------

/// Parses a GameCube/Wii disc boot header (`boot.bin`'s first 0x80 bytes): the 6-character
/// game code at 0x00 and the game title at 0x20.
///
/// **New in this port** -- `reference/old_archiver.rs` had no GC/Wii header parser at all,
/// only the vestigial `disc_header_offset`/`disc_title_offset` pair (line 4956) that
/// returned the same constant for every format. Reading this costs nothing for RVZ inputs,
/// whose first 0x80 bytes are already captured in `RvzMetadata::disc_header` by
/// `dolphin_wrapper`, so GC/Wii identification never needs an extraction.
pub fn parse_gc_wii_boot_header(bytes: &[u8]) -> LeafMetadata {
    // Magic words: 0xC2339F3D at 0x1C for GameCube, 0x5D1C9EA3 at 0x18 for Wii. A real disc
    // header has exactly one of them.
    let gamecube_magic = read_u32_be(bytes, 0x1C) == Some(0xC233_9F3D);
    let wii_magic = read_u32_be(bytes, 0x18) == Some(0x5D1C_9EA3);
    if !gamecube_magic && !wii_magic {
        return None;
    }

    let serial = read_ascii_field(bytes.get(0x00..0x06)?).and_then(|value| sanitize_serial_opt(Some(value)));
    let title = read_ascii_field(bytes.get(0x20..0x60)?).and_then(|value| sanitize_title_opt(Some(value)));

    if title.is_none() && serial.is_none() {
        return None;
    }

    Some((title, serial, None))
}
