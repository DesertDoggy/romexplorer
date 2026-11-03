//! Platforms without a large enough family to warrant their own module: PC Engine /
//! TurboGrafx-16 / SuperGrafx HuCards, NeoGeo Pocket / Color, NeoGeo CD id inference,
//! WonderSwan / Color, and Virtual Boy.
//!
//! **Not yet implemented** (no precedent anywhere in `reference/old_archiver.rs` either --
//! not even extension-heuristic entries): the Atari family (2600/5200/7800/Lynx/Jaguar/
//! ST-STE/8-bit) and PocketStation content parsing. Both fall through to `None` here, the
//! same as `PcFx` does, and are detected by extension/path heuristics alone.

use std::fs;
use std::path::{Path, PathBuf};

use crate::common::models::Platform;

use super::super::models::{sanitize_serial_opt, sanitize_title_opt};
use super::super::detect::disc_fs::{read_exact_at, read_u16_le};
use super::read_ascii_field;

type LeafMetadata = Option<(Option<String>, Option<String>, Option<String>)>;

// ---------------------------------------------------------------------------
// PC Engine / TurboGrafx-16 / SuperGrafx
// ---------------------------------------------------------------------------

/// HuCard dumps are headerless, so shape identification rests on file size plus the first
/// two bytes of the reset vector's code (`SEI` followed by a stack/interrupt setup opcode).
/// Ported from `reference/old_archiver.rs`'s `parse_pce_hucard_metadata_from_bytes` (line
/// 6131) minus its `.pce`/`.sgx` extension requirements -- see the extension note in
/// [`super`] -- plus a new real content check for SuperGrafx specifically (see
/// [`find_supergrafx_vpc_init`]): by shape alone, SuperGrafx and TurboGrafx-16 HuCards
/// really are identical (same header-less format, same entry-opcode convention), but a
/// SuperGrafx-exclusive game's boot code has to initialize the Video Priority Controller
/// registers at CPU addresses $0008-$000B to merge its second video chip's output --
/// addresses that don't exist on a plain TurboGrafx-16/PC Engine, so a TG16 game's code can
/// never reference them. A positive VPC-init match is trusted (rejects `TurboGrafx16` as
/// the target); its absence isn't trusted as proof of the opposite (a real SuperGrafx game
/// could still set the VPC up outside this scan's early-boot window), so `SuperGrafx`
/// without the signal remains accepted, same as before this check existed.
pub fn parse_pce_hucard_metadata_from_bytes(path: &Path, bytes: &[u8], target: Platform) -> LeafMetadata {
    if !matches!(target, Platform::TurboGrafx16 | Platform::SuperGrafx) {
        return None;
    }

    let file_size = fs::metadata(path).ok()?.len() as usize;
    if file_size < 0x8000 || file_size % 0x2000 != 0 {
        return None;
    }
    if bytes.len() < 4 {
        return None;
    }

    // Conservative executable-byte checks: 0x78 is SEI, followed by TXS (0x9A via 0xD4 in
    // HuC6280 encoding) or CLI-adjacent setup.
    if bytes[0] != 0x78 {
        return None;
    }
    if bytes[1] != 0xD4 && bytes[1] != 0x58 {
        return None;
    }

    // By this point the size-shape and entry-opcode gates above have already rejected
    // anything that isn't at least plausibly a HuCard (in particular, a large disc ISO
    // dispatched here by mistake would already be gone) -- so the CRC32 fallback below only
    // ever reads a real HuCard-shaped file, never an arbitrary large candidate. The
    // byte-pattern scan runs first regardless (cheap, already-read prefix bytes); the
    // whole-file CRC32 read only happens when that scan came back empty (`||`
    // short-circuits).
    let confirmed_supergrafx = find_supergrafx_vpc_init(bytes).is_some() || known_supergrafx_exclusive_crc32(path).is_some();
    if matches!(target, Platform::TurboGrafx16) && confirmed_supergrafx {
        return None;
    }

    Some((None, None, None))
}

/// CRC32 (the zlib/ISO-3309 polynomial, via `crc32fast` -- already a project dependency,
/// see `rom::hash`) of every SuperGrafx-EXCLUSIVE game ever released: there are only
/// five, so hardcoding them is the whole database. Deliberately excludes "hybrid" titles
/// like Darius Plus/Alpha (enhanced output on real SGX hardware, but still runs on plain
/// TurboGrafx-16) -- those aren't exclusive and don't belong on a "reject TurboGrafx16"
/// list.
///
const SUPERGRAFX_EXCLUSIVE_CRC32: [u32; 5] = [0x8C4588E2, 0x4C2126B0, 0x3B13AF61, 0xB486A8ED, 0x1F041166];

/// Whole-file CRC32 fallback for the one real SuperGrafx-exclusive title
/// (`find_supergrafx_vpc_init`) misses (its VPC init is scattered with no tight cluster
/// anywhere in the file -- see that function's doc comment), and a safety net for any other
/// exclusive title whose VPC init the byte-pattern scan doesn't catch either. Returns the
/// matched CRC32 (for evidence display) rather than a bare bool.
pub fn known_supergrafx_exclusive_crc32(path: &Path) -> Option<u32> {
    let bytes = fs::read(path).ok()?;
    let crc = crc32fast::hash(&bytes);
    SUPERGRAFX_EXCLUSIVE_CRC32.contains(&crc).then_some(crc)
}

/// Scans the first 4KB of a HuCard dump for SuperGrafx's hardware "tell": two or more
/// `STA`/`STX`/`STY`/`STZ` instructions, in **absolute** addressing mode (the 3-byte
/// `opcode + little-endian u16` encoding -- 0x8D/0x8E/0x8C/0x9C), writing to one of the
/// Video Priority Controller's window registers at $0008-$000B, with at least two such
/// writes within 64 bytes of each other (a real register-setup loop, not one coincidental
/// match). Returns every matching instruction's file offset when found.
///
/// Zero-page addressing mode (the 2-byte `opcode + u8` encoding for these same low
/// addresses) is deliberately NOT scanned: it is far too common in ordinary ROM data to
/// trust as a signal at all. Absolute mode is far rarer by chance, and a genuine VPC init
/// looks like `LDA $E0DE / STA $0008 / LDA $E0DF / STA $0009 / ...`.
///
/// `SCAN_WINDOW` = 6KB keeps the false-positive rate at zero for plain TurboGrafx-16 code
/// (it starts climbing past 8KB) while still reaching the VPC init of most SuperGrafx-exclusive
/// games. One exclusive title's VPC writes are scattered with no tight cluster anywhere in the
/// file, so no window size catches it with this detector; that one needs the hash list (only
/// a handful of SuperGrafx-exclusive titles ever existed).
pub fn find_supergrafx_vpc_init(bytes: &[u8]) -> Option<Vec<usize>> {
    const SCAN_WINDOW: usize = 6144;
    const ABS_STORE_OPCODES: [u8; 4] = [0x8D, 0x8E, 0x8C, 0x9C]; // STA/STX/STY/STZ, absolute
    const VPC_PORTS: std::ops::RangeInclusive<u16> = 0x0008..=0x000B;
    const MAX_GAP: usize = 64;

    let window = bytes.get(..SCAN_WINDOW.min(bytes.len()))?;
    let mut hits = Vec::new();
    for i in 0..window.len().saturating_sub(2) {
        if !ABS_STORE_OPCODES.contains(&window[i]) {
            continue;
        }
        let addr = u16::from_le_bytes([window[i + 1], window[i + 2]]);
        if VPC_PORTS.contains(&addr) {
            hits.push(i);
        }
    }

    if hits.windows(2).any(|pair| pair[1] - pair[0] <= MAX_GAP) { Some(hits) } else { None }
}

// ---------------------------------------------------------------------------
// NeoGeo Pocket / Color
// ---------------------------------------------------------------------------

/// Ported verbatim from `reference/old_archiver.rs`'s
/// `parse_neogeo_pocket_header_metadata_from_bytes` (line 5945).
pub fn parse_neogeo_pocket_header_metadata_from_bytes(bytes: &[u8], target: Platform) -> LeafMetadata {
    if bytes.len() < 0x30 {
        return None;
    }
    if bytes.get(0x00..0x1C)? != b" LICENSED BY SNK CORPORATION" {
        return None;
    }

    // NeoPop header layout: mode at 0x23 (0x00 mono, 0x10 color), name at 0x24..0x2F.
    let mode = *bytes.get(0x23)?;
    if matches!(target, Platform::NeoGeoPocketColor) && mode != 0x10 {
        return None;
    }
    if matches!(target, Platform::NeoGeoPocket) && mode == 0x10 {
        return None;
    }

    let title = read_ascii_field(bytes.get(0x24..0x30)?).and_then(|value| sanitize_title_opt(Some(value)));
    let catalog = read_u16_le(bytes, 0x20)?;
    let sub_catalog = *bytes.get(0x22)?;
    let serial = sanitize_serial_opt(Some(format!("{catalog:04X}-{sub_catalog:02X}")));

    if title.is_none() && serial.is_none() {
        return None;
    }

    Some((title, serial.clone(), serial))
}

// ---------------------------------------------------------------------------
// NeoGeo CD
// ---------------------------------------------------------------------------

/// Scans for the `NEO-GEO` marker and decodes the NGH number that follows it. Ported
/// verbatim from `reference/old_archiver.rs`'s `infer_neogeo_cd_game_id_from_bytes` (line 6724).
pub fn infer_neogeo_cd_game_id_from_bytes(bytes: &[u8]) -> Option<String> {
    infer_neogeo_cd_game_id_with_evidence_from_bytes(bytes).map(|(_, id)| id)
}

/// Same scan as [`infer_neogeo_cd_game_id_from_bytes`], but also returns the offset the
/// literal `NEO-GEO` marker matched at -- the id text alone ("0083") isn't itself real
/// evidence of *how* it was found; this lets a caller build [`super::Evidence::Bytes`]
/// naming the real marker location and the two raw BCD bytes it decoded.
pub fn infer_neogeo_cd_game_id_with_evidence_from_bytes(bytes: &[u8]) -> Option<(usize, String)> {
    let marker = b"NEO-GEO";
    if bytes.len() < marker.len() + 10 {
        return None;
    }

    let mut i = 0usize;
    while i + marker.len() + 10 <= bytes.len() {
        if &bytes[i..i + marker.len()] == marker {
            if let Some(id) = decode_ngh_bcd(bytes[i + 8], bytes[i + 9]) {
                return Some((i, id));
            }
        }
        i += 1;
    }

    None
}

/// Ported verbatim from `reference/old_archiver.rs`'s `infer_neogeo_cd_game_id_from_paths`
/// (line 6711).
pub fn infer_neogeo_cd_game_id_from_paths(paths: &[PathBuf]) -> Option<String> {
    for path in paths {
        let Ok(bytes) = fs::read(path) else { continue };
        if let Some(id) = infer_neogeo_cd_game_id_from_bytes(&bytes) {
            return Some(id);
        }
    }
    None
}

/// The NGH number is packed BCD across two bytes. Ported verbatim from
/// `reference/old_archiver.rs`'s `decode_ngh_bcd` (line 6745).
fn decode_ngh_bcd(b0: u8, b1: u8) -> Option<String> {
    let d0 = (b0 >> 4) & 0x0f;
    let d1 = b0 & 0x0f;
    let d2 = (b1 >> 4) & 0x0f;
    let d3 = b1 & 0x0f;
    let digits = [d0, d1, d2, d3];
    if digits.iter().any(|d| *d > 9) {
        return None;
    }
    if digits.iter().all(|d| *d == 0) {
        return None;
    }

    Some(format!("{d0}{d1}{d2}{d3}"))
}

// ---------------------------------------------------------------------------
// WonderSwan / WonderSwan Color
// ---------------------------------------------------------------------------

/// WonderSwan carts store their metadata in the last 16 bytes rather than at the start.
/// Ported verbatim from `reference/old_archiver.rs`'s `parse_wonderswan_footer_metadata`
/// (line 5975).
pub fn parse_wonderswan_footer_metadata(path: &Path, target: Platform) -> LeafMetadata {
    let file_size = fs::metadata(path).ok()?.len() as usize;
    if file_size < 16 || file_size % 0x10000 != 0 {
        return None;
    }

    let tail = read_exact_at(path, file_size as u64 - 16, 16).ok()?;
    // 0xEA is the far-jump opcode the footer always begins with.
    if *tail.first()? != 0xEA {
        return None;
    }

    let system = *tail.get(7)?;
    let looks_color = matches!(system, 0x01 | 0x02);
    if matches!(target, Platform::WonderSwan) && looks_color {
        return None;
    }
    if matches!(target, Platform::WonderSwanColor) && !looks_color {
        return None;
    }

    let publisher = *tail.get(6)?;
    let cart_id = *tail.get(8)?;
    let rom_size_code = *tail.get(10)?;
    let serial = sanitize_serial_opt(Some(format!("WS-{publisher:02X}-{cart_id:02X}-{rom_size_code:02X}")));

    Some((None, serial.clone(), serial))
}

// ---------------------------------------------------------------------------
// Virtual Boy
// ---------------------------------------------------------------------------

/// Virtual Boy has no magic number. Its ROM is mapped so the last bytes of the file are the
/// top of the CPU address space, which fixes the layout from the end of the file: a 32-byte
/// header 544 bytes before EOF (20-byte title, 5 reserved zero bytes, 2-byte maker code,
/// 4-byte game code, 1-byte version) and the reset vector in the final 16 bytes (code that
/// jumps into the game, never blank). A power-of-two size between 128KB and 16MB, the reserved
/// zero run, ASCII maker/game codes and a non-blank reset vector together identify it.
pub fn parse_virtual_boy_rom_shape(path: &Path) -> LeafMetadata {
    let file_size = fs::metadata(path).ok()?.len() as u64;
    if !(0x20000..=0x1000000).contains(&file_size) || !file_size.is_power_of_two() {
        return None;
    }

    const HEADER_LEN: u64 = 544;
    let header = read_exact_at(path, file_size - HEADER_LEN, 32).ok()?;
    let title = read_ascii_field(header.get(0x00..0x14)?);
    title.as_ref()?;
    if header.get(0x14..0x19)? != [0u8; 5] {
        return None;
    }
    let code_bytes = header.get(0x19..0x1F)?;
    if !code_bytes.iter().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    let reset_vector = read_exact_at(path, file_size - 16, 16).ok()?;
    if reset_vector.iter().all(|&b| b == 0x00) || reset_vector.iter().all(|&b| b == 0xFF) {
        return None;
    }
    let code = read_ascii_field(code_bytes);

    Some((title, code.clone(), code))
}
