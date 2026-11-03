//! Sega cartridge header parsers: Genesis/Mega Drive, Pico, 32X, Master System, Game Gear,
//! SG-1000.
//!
//! Sega's disc platforms (Sega CD, Saturn, Dreamcast) carry no bespoke parser: they route
//! through the shared disc path in `super::deep_try_disc_platform`, same as in
//! `reference/old_archiver.rs`.

use std::fs;
use std::path::Path;

use crate::common::models::Platform;

use super::super::models::{sanitize_serial_opt, sanitize_title_opt};
use super::super::detect::disc_fs::read_exact_at;
use super::{read_ascii_field, read_detection_prefix};

type LeafMetadata = Option<(Option<String>, Option<String>, Option<String>)>;

// ---------------------------------------------------------------------------
// Genesis / Mega Drive / Pico / 32X
// ---------------------------------------------------------------------------

/// Ported verbatim from `reference/old_archiver.rs`'s
/// `parse_genesis_header_metadata_from_bytes` (line 5909), with its caller's Genesis/Pico
/// gate (line 5355) folded in so the dispatch matches on platform only once. Genesis and
/// Pico share a header layout and differ only by the console name at 0x100. Prefers the
/// domestic (Japanese) title at 0x120 over the overseas one at 0x150.
pub fn parse_genesis_header_metadata_from_bytes(bytes: &[u8], target: Platform) -> LeafMetadata {
    let console = read_ascii_field(bytes.get(0x100..0x110)?)?;
    if !console.starts_with("SEGA MEGA DRIVE") && !console.starts_with("SEGA PICO") {
        return None;
    }
    if matches!(target, Platform::SegaGenesis) && console.starts_with("SEGA PICO") {
        return None;
    }
    if matches!(target, Platform::SegaPico) && !console.starts_with("SEGA PICO") {
        return None;
    }

    parse_sega_common_title_serial(bytes)
}

/// Ported verbatim from `reference/old_archiver.rs`'s
/// `parse_sega_32x_header_metadata_from_bytes` (line 6054) -- identical layout to Genesis,
/// distinguished only by the console name.
pub fn parse_sega_32x_header_metadata_from_bytes(bytes: &[u8]) -> LeafMetadata {
    let console = read_ascii_field(bytes.get(0x100..0x110)?)?;
    if !console.starts_with("SEGA 32X") {
        return None;
    }

    parse_sega_common_title_serial(bytes)
}

/// The title/serial half shared verbatim by Genesis, Pico and 32X headers (old code had this
/// same block duplicated across `parse_genesis_header_metadata_from_bytes` and
/// `parse_sega_32x_header_metadata_from_bytes`).
fn parse_sega_common_title_serial(bytes: &[u8]) -> LeafMetadata {
    let domestic = read_ascii_field(bytes.get(0x120..0x150)?);
    let overseas = read_ascii_field(bytes.get(0x150..0x180)?);
    let title = domestic.or(overseas).and_then(|value| sanitize_title_opt(Some(value)));
    let serial = read_ascii_field(bytes.get(0x180..0x18E)?)
        .map(|value| value.replace(" - ", "-").replace("- ", "-"))
        .and_then(|value| sanitize_serial_opt(Some(value)));

    if title.is_none() && serial.is_none() {
        return None;
    }

    Some((title, serial, None))
}

// ---------------------------------------------------------------------------
// Master System / Game Gear
// ---------------------------------------------------------------------------

/// The `TMR SEGA` header sits at one of three offsets depending on ROM size. The region
/// nibble at 0x0F distinguishes Game Gear from Master System. Ported verbatim from
/// `reference/old_archiver.rs`'s `parse_sms_gg_header_metadata` (line 6004).
pub fn parse_sms_gg_header_metadata(path: &Path, target: Platform) -> LeafMetadata {
    let file_size = fs::metadata(path).ok()?.len() as usize;
    for offset in [0x1FF0usize, 0x3FF0usize, 0x7FF0usize] {
        let end = offset.checked_add(16)?;
        if end > file_size {
            continue;
        }
        let Ok(header) = read_exact_at(path, offset as u64, 16) else { continue };
        if header.get(0..8) != Some(&b"TMR SEGA"[..]) {
            continue;
        }

        let region_nibble = header.get(0x0F).copied()? >> 4;
        let is_game_gear = matches!(region_nibble, 0x5 | 0x6 | 0x7);
        if matches!(target, Platform::SegaGameGear) && !is_game_gear {
            continue;
        }
        if matches!(target, Platform::SegaMasterSystem) && is_game_gear {
            continue;
        }

        let b0 = header.get(0x0C).copied()?;
        let b1 = header.get(0x0D).copied()?;
        let b2 = header.get(0x0E).copied()?;
        let serial = decode_sms_product_code(b0, b1, b2).and_then(|value| sanitize_serial_opt(Some(value)));
        return Some((None, serial.clone(), serial));
    }

    None
}

/// The product code is stored as packed BCD across three bytes. Ported verbatim from
/// `reference/old_archiver.rs`'s `decode_sms_product_code` (line 6036).
fn decode_sms_product_code(b0: u8, b1: u8, b2: u8) -> Option<String> {
    let d0 = b0 & 0x0F;
    let d1 = (b0 >> 4) & 0x0F;
    let d2 = b1 & 0x0F;
    let d3 = (b1 >> 4) & 0x0F;
    if [d0, d1, d2, d3].iter().any(|d| *d > 9) {
        return None;
    }

    let lead = (b2 >> 4) & 0x0F;
    let body = format!("{d3}{d2}{d1}{d0}");
    if lead == 0 { Some(format!("SMS-{body}")) } else { Some(format!("SMS-{lead:X}{body}")) }
}

// ---------------------------------------------------------------------------
// SG-1000
// ---------------------------------------------------------------------------

/// SG-1000 carts are headerless, so identification rests entirely on shape: one of the
/// handful of real cart sizes, a plausible Z80 entry opcode, and the *absence* of a
/// `TMR SEGA` header (which would make it a Master System ROM instead). Ported from
/// `reference/old_archiver.rs`'s `parse_sg1000_rom_shape` (line 6076) minus its `.sg`
/// extension requirement -- see the extension note in [`super`].
pub fn parse_sg1000_rom_shape(path: &Path) -> LeafMetadata {
    let file_size = fs::metadata(path).ok()?.len() as usize;
    if !matches!(file_size, 0x2000 | 0x4000 | 0x6000 | 0x8000 | 0xA000 | 0xC000) {
        return None;
    }

    let head = read_detection_prefix(path).ok()?;
    let first = *head.first()?;
    // Common SG-1000/Z80 entry opcodes seen in dumped carts.
    if !matches!(first, 0xC3 | 0x31 | 0xF3 | 0xED) {
        return None;
    }

    if file_size >= 0x2000 {
        let header = read_exact_at(path, 0x1FF0, 8).ok();
        if header.as_deref() == Some(&b"TMR SEGA"[..]) {
            return None;
        }
    }

    Some((None, None, None))
}
