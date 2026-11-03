//! Per-platform disc title/serial extraction, shared between the on-disk-file path
//! (`disc_metadata_from_disc_file`, filesystem-aware: reads named entries out of the disc's
//! ISO9660/XDVDFS filesystem) and the raw-bytes probe path (`disc_metadata_from_bytes`,
//! byte-scan only -- used for a cheap read-once probe before a candidate is confirmed).
//!
//! Ported from `reference/old_archiver.rs`'s `disc_metadata_from_disc_file` (line 4058),
//! `disc_metadata_from_bytes` (3346) and `disc_metadata_from_probe_bytes` (3403) -- the
//! latter two merge into [`disc_metadata_from_bytes`] here since they differed only in the
//! PS2 title/serial special case (`is_probe` below). Every native CHD/RVZ logical-range
//! fallback (`chd_native`/`rvz_native` prefix reads, `parse_playstation_boot_serial_from_chd_prefix`)
//! is dropped entirely: this design always extracts CHD/RVZ to a real file before disc
//! detection runs (see `super::super::extract`), so `disc_metadata_from_disc_file` only ever
//! reads from a plain `&Path` -- there is no more "backend" to special-case.

use std::path::Path;

use crate::common::models::Platform;

use super::super::models::{sanitize_serial_opt, sanitize_text_opt, sanitize_title_opt};
use super::super::platforms::read_ascii_field;
use super::disc_fs;

/// (title, serial, manufacturer_id) -- `manufacturer_id` is nearly always a clone of
/// `serial` (see old code's own pattern), kept distinct only because some callers merge it
/// separately.
pub type DiscTitleSerial = (Option<String>, Option<String>, Option<String>);

// ---------------------------------------------------------------------------
// Shared byte helpers (disc-parser-local; not general enough for disc_fs.rs)
// ---------------------------------------------------------------------------

fn read_u32_be(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?))
}

fn find_ascii_case_insensitive(bytes: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || bytes.len() < needle.len() {
        return None;
    }
    bytes.windows(needle.len()).position(|window| {
        window.iter().zip(needle.iter()).all(|(left, right)| left.to_ascii_lowercase() == right.to_ascii_lowercase())
    })
}

fn read_null_terminated_ascii(bytes: &[u8], start: usize, max_len: usize) -> Option<String> {
    // Clamp to the buffer end rather than failing outright (old code required the full
    // `max_len` window to be in bounds, so a PARAM.SFO whose last key sits within 0x40
    // bytes of EOF -- an exactly-sized extraction, rather than a padded one -- parsed as
    // nothing). The NUL scan below still terminates the string correctly either way.
    let end = start.checked_add(max_len)?.min(bytes.len());
    let slice = bytes.get(start..end)?;
    let nul = slice.iter().position(|byte| *byte == 0).unwrap_or(slice.len());
    let text = String::from_utf8_lossy(&slice[..nul]).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

fn read_fixed_ascii_string(bytes: &[u8], start: usize, len: usize) -> Option<String> {
    let slice = bytes.get(start..start + len)?;
    let nul = slice.iter().position(|byte| *byte == 0).unwrap_or(slice.len());
    let text = String::from_utf8_lossy(&slice[..nul]).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

fn decode_utf16_string(bytes: &[u8], little_endian: bool) -> Option<String> {
    let mut values = Vec::new();
    for chunk in bytes.chunks_exact(2) {
        let code_unit = if little_endian { u16::from_le_bytes([chunk[0], chunk[1]]) } else { u16::from_be_bytes([chunk[0], chunk[1]]) };
        if code_unit == 0 {
            break;
        }
        values.push(code_unit);
    }
    if values.is_empty() {
        return None;
    }
    Some(String::from_utf16_lossy(&values).trim().to_string())
}

fn utf16_printability_score(value: &str) -> usize {
    value.chars().filter(|ch| !ch.is_control() && *ch != '\u{fffd}').count()
}

/// Tries both byte orders and keeps whichever decodes to more printable characters --
/// title fields in these disc/executable formats aren't reliably tagged with an endianness.
fn read_utf16_best_effort(bytes: &[u8]) -> Option<String> {
    let le = decode_utf16_string(bytes, true);
    let be = decode_utf16_string(bytes, false);
    match (le, be) {
        (Some(left), Some(right)) => {
            if utf16_printability_score(&left) >= utf16_printability_score(&right) { Some(left) } else { Some(right) }
        }
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

// ---------------------------------------------------------------------------
// PlayStation 1/2 BOOT/BOOT2 serial scan (SYSTEM.CNF)
// ---------------------------------------------------------------------------

fn is_likely_playstation_serial(value: &str) -> bool {
    let value = value.trim().to_ascii_uppercase();
    let bytes = value.as_bytes();

    // e.g. SLUS_000.00, SCES_000.00
    let classic = bytes.len() == 11
        && bytes[0..4].iter().all(|b| b.is_ascii_uppercase())
        && matches!(bytes[4], b'_' | b'-')
        && bytes[5..8].iter().all(|b| b.is_ascii_digit())
        && bytes[8] == b'.'
        && bytes[9..11].iter().all(|b| b.is_ascii_digit());

    // e.g. BASLUS-00000
    let alt = bytes.len() == 11
        && bytes[0..6].iter().all(|b| b.is_ascii_uppercase())
        && bytes[6] == b'-'
        && bytes[7..11].iter().all(|b| b.is_ascii_digit());

    classic || alt
}

fn contains_ascii_needle(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|window| window == needle)
}

fn extract_playstation_serial_candidate(text: &str) -> Option<String> {
    let upper = text.to_ascii_uppercase();
    let bytes = upper.as_bytes();

    for i in 0..bytes.len() {
        if i + 11 > bytes.len() {
            break;
        }
        let candidate = &bytes[i..i + 11];
        let classic = candidate[0..4].iter().all(|b| b.is_ascii_uppercase())
            && matches!(candidate[4], b'_' | b'-')
            && candidate[5..8].iter().all(|b| b.is_ascii_digit())
            && candidate[8] == b'.'
            && candidate[9..11].iter().all(|b| b.is_ascii_digit());
        let alt = candidate[0..6].iter().all(|b| b.is_ascii_uppercase())
            && candidate[6] == b'-'
            && candidate[7..11].iter().all(|b| b.is_ascii_digit());

        if classic || alt {
            return Some(String::from_utf8_lossy(candidate).to_string());
        }
    }

    None
}

fn find_playstation_serial_near_boot_key(bytes: &[u8], boot_key_upper: &str) -> Option<String> {
    let upper = bytes.iter().map(|b| b.to_ascii_uppercase()).collect::<Vec<_>>();
    let key = boot_key_upper.as_bytes().iter().map(|b| b.to_ascii_uppercase()).collect::<Vec<_>>();

    for i in 0..upper.len() {
        if i + 11 > upper.len() {
            break;
        }
        let candidate = &upper[i..i + 11];
        let classic = candidate[0..4].iter().all(|b| b.is_ascii_uppercase())
            && matches!(candidate[4], b'_' | b'-')
            && candidate[5..8].iter().all(|b| b.is_ascii_digit())
            && candidate[8] == b'.'
            && candidate[9..11].iter().all(|b| b.is_ascii_digit());
        let alt = candidate[0..6].iter().all(|b| b.is_ascii_uppercase())
            && candidate[6] == b'-'
            && candidate[7..11].iter().all(|b| b.is_ascii_digit());
        if !(classic || alt) {
            continue;
        }

        let context_start = i.saturating_sub(256);
        let context = &upper[context_start..i];
        if contains_ascii_needle(context, &key) {
            let serial = String::from_utf8_lossy(candidate).to_string();
            if is_likely_playstation_serial(&serial) {
                return Some(serial);
            }
        }
    }

    None
}

/// Scans `SYSTEM.CNF`-shaped bytes for a `BOOT`/`BOOT2` line's serial (PS1 uses `BOOT`,
/// PS2 uses `BOOT2` then falls back to `BOOT`). Ported verbatim from
/// `reference/old_archiver.rs`'s `parse_playstation_boot_serial_from_bytes` (line 3469).
pub fn parse_playstation_boot_serial_from_bytes(bytes: &[u8], boot_key: &str) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let upper = text.to_ascii_uppercase();
    let boot_key = boot_key.to_ascii_uppercase();

    let mut search_start = 0usize;
    while search_start < upper.len() {
        let rel = match upper[search_start..].find(&boot_key) {
            Some(v) => v,
            None => break,
        };
        let marker_offset = search_start + rel;
        let mut value = &text[marker_offset + boot_key.len()..];
        value = value.trim_start_matches(|ch: char| ch.is_whitespace() || ch == '=' || ch == ':');
        for prefix in ["cdrom0:\\", "cdrom:\\"] {
            if value.get(..prefix.len()).map(|head| head.eq_ignore_ascii_case(prefix)).unwrap_or(false) {
                value = value.get(prefix.len()..).unwrap_or("");
                break;
            }
        }

        if let Some(candidate) = extract_playstation_serial_candidate(value) {
            if let Some(serial) = sanitize_serial_opt(Some(candidate)) {
                if is_likely_playstation_serial(&serial) {
                    return Some(serial);
                }
            }
        }

        let serial = value.split(|ch: char| ch == ';' || ch.is_whitespace() || ch == '\0').next().unwrap_or("").trim();
        if let Some(serial) = sanitize_serial_opt(Some(serial.to_string())) {
            if is_likely_playstation_serial(&serial) {
                return Some(serial);
            }
        }

        search_start = marker_offset.saturating_add(boot_key.len());
    }

    find_playstation_serial_near_boot_key(bytes, &boot_key)
}

// ---------------------------------------------------------------------------
// PC Engine CD / TurboGrafx-CD serial
// ---------------------------------------------------------------------------

/// Ported verbatim from `reference/old_archiver.rs`'s `parse_pce_cd_serial_from_bytes`
/// (line 3652) -- PCE-CD discs carry their serial as a `<SERIAL>.CNF` sibling file name
/// next to `SYSTEM.CNF`.
pub fn parse_pce_cd_serial_from_bytes(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    for token in text.split(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))) {
        let upper = token.to_ascii_uppercase();
        if upper.ends_with(".CNF") && upper != "SYSTEM.CNF" {
            let stem = upper.trim_end_matches(".CNF").trim();
            if let Some(serial) = sanitize_serial_opt(Some(stem.to_string())) {
                return Some(serial);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// PARAM.SFO (PS3 / PSP)
// ---------------------------------------------------------------------------

fn try_parse_ps3_param_sfo_at(bytes: &[u8], start: usize) -> Option<(Option<String>, Option<String>)> {
    let key_table_start = disc_fs::read_u32_le(bytes, start + 0x08)? as usize;
    let data_table_start = disc_fs::read_u32_le(bytes, start + 0x0C)? as usize;
    let tables_entries = disc_fs::read_u32_le(bytes, start + 0x10)? as usize;

    if tables_entries == 0 || tables_entries > 128 {
        return None;
    }
    if key_table_start < 0x14 + tables_entries * 0x10 || data_table_start <= key_table_start {
        return None;
    }
    if start.checked_add(data_table_start)? > bytes.len() {
        return None;
    }

    let mut title = None;
    let mut localized_title = None;
    let mut serial = None;

    for index in 0..tables_entries {
        let entry_offset = start + 0x14 + index * 0x10;
        let key_offset = disc_fs::read_u16_le(bytes, entry_offset)? as usize;
        let data_len = disc_fs::read_u32_le(bytes, entry_offset + 0x04)? as usize;
        let data_offset = disc_fs::read_u32_le(bytes, entry_offset + 0x0C)? as usize;

        let key_start = start + key_table_start + key_offset;
        let key = read_null_terminated_ascii(bytes, key_start, 0x40)?;
        let value_start = start + data_table_start + data_offset;
        let value_end = value_start.checked_add(data_len)?;
        let raw_value = bytes.get(value_start..value_end)?;
        let value = sanitize_text_opt(Some(String::from_utf8_lossy(raw_value).to_string()));

        match key.as_str() {
            "TITLE_ID" | "DISC_ID" => serial = sanitize_serial_opt(value),
            "TITLE" => title = sanitize_title_opt(value),
            _ if key.starts_with("TITLE_") && title.is_none() => localized_title = sanitize_title_opt(value),
            _ => {}
        }
    }

    let title = title.or(localized_title);
    if title.is_none() && serial.is_none() {
        return None;
    }

    Some((title, serial))
}

/// Ported verbatim from `reference/old_archiver.rs`'s `parse_ps3_param_sfo_from_bytes`
/// (line 3667). Scans backward from every `TITLE_ID\0`/`TITLE\0` key-name occurrence for a
/// plausible SFO header start, since PARAM.SFO's own header offset within a probe buffer
/// isn't known ahead of time.
pub fn parse_ps3_param_sfo_from_bytes(bytes: &[u8]) -> Option<(Option<String>, Option<String>)> {
    let mut key_positions = Vec::new();
    for needle in [b"TITLE_ID\0".as_slice(), b"TITLE\0".as_slice()] {
        let mut offset = 0;
        while offset < bytes.len() {
            let haystack = &bytes[offset..];
            if let Some(found) = haystack.windows(needle.len()).position(|window| window == needle) {
                let key_pos = offset + found;
                key_positions.push(key_pos);
                offset = key_pos.saturating_add(1);
            } else {
                break;
            }
        }
    }

    key_positions.sort_unstable();
    key_positions.dedup();

    for key_pos in key_positions {
        let start_min = key_pos.saturating_sub(0x800);
        let start_max = key_pos.saturating_sub(0x14);
        for start in (start_min..=start_max).step_by(4) {
            if let Some(result) = try_parse_ps3_param_sfo_at(bytes, start) {
                return Some(result);
            }
        }
    }

    None
}

/// PSP's `PARAM.SFO` uses the identical binary layout as PS3's. Ported verbatim from
/// `reference/old_archiver.rs`'s `parse_psp_param_sfo_from_bytes` (line 3465).
pub fn parse_psp_param_sfo_from_bytes(bytes: &[u8]) -> Option<(Option<String>, Option<String>)> {
    parse_ps3_param_sfo_from_bytes(bytes)
}

// ---------------------------------------------------------------------------
// Xbox XBE
// ---------------------------------------------------------------------------

fn format_xbe_title_id(raw: u32) -> Option<String> {
    let bytes = raw.to_be_bytes();
    let publisher = [bytes[0], bytes[1]];
    if !publisher.iter().all(|byte| byte.is_ascii_uppercase()) {
        return None;
    }
    let game_number = u16::from_be_bytes([bytes[2], bytes[3]]);
    Some(format!("{}{}-{:03}", publisher[0] as char, publisher[1] as char, game_number))
}

fn try_parse_xbe_at(bytes: &[u8], start: usize) -> Option<(Option<String>, Option<String>)> {
    let base_address = disc_fs::read_u32_le(bytes, start + 0x0104)? as usize;
    let certificate_address = disc_fs::read_u32_le(bytes, start + 0x0118)? as usize;
    if certificate_address < base_address {
        return None;
    }

    let certificate_offset = certificate_address - base_address;
    let certificate_end = certificate_offset.checked_add(0x120)?;
    let certificate = bytes.get(certificate_offset..certificate_end)?;

    let title_id_raw = u32::from_le_bytes(certificate.get(0x08..0x0C)?.try_into().ok()?);
    let title_name_bytes = certificate.get(0x0C..0x5C)?;
    let serial = format_xbe_title_id(title_id_raw).and_then(|value| sanitize_serial_opt(Some(value)));
    let title = sanitize_title_opt(read_utf16_best_effort(title_name_bytes));

    if serial.is_none() {
        return None;
    }

    Some((title, serial))
}

fn parse_xbe_region(bytes: &[u8]) -> Option<(Option<String>, Option<String>)> {
    let mut search_start = 0;
    while search_start < bytes.len() {
        let haystack = &bytes[search_start..];
        let found = haystack.windows(4).position(|window| window == b"XBEH")?;
        let start = search_start + found;
        if let Some(result) = try_parse_xbe_at(bytes, start) {
            return Some(result);
        }
        search_start = start.saturating_add(1);
    }
    None
}

/// Ported verbatim from `reference/old_archiver.rs`'s `parse_xbe_metadata_from_bytes`
/// (line 3747). First narrows the search window around a `default.xbe` path string (fast
/// path for descriptor-payload/directory-bundle probes), then falls back to scanning the
/// full buffer for the `XBEH` magic.
pub fn parse_xbe_metadata_from_bytes(bytes: &[u8]) -> Option<(Option<String>, Option<String>)> {
    if let Some(path_offset) = find_ascii_case_insensitive(bytes, b"default.xbe") {
        let search_end = path_offset.saturating_add(0x40000).min(bytes.len());
        if let Some(result) = parse_xbe_region(&bytes[path_offset..search_end]) {
            return Some(result);
        }
    }
    let mut search_start = 0;
    while search_start < bytes.len() {
        let haystack = &bytes[search_start..];
        let found = haystack.windows(4).position(|window| window == b"XBEH")?;
        let start = search_start + found;
        if let Some(result) = try_parse_xbe_at(bytes, start) {
            return Some(result);
        }
        search_start = start.saturating_add(1);
    }
    None
}

// ---------------------------------------------------------------------------
// Xbox 360 XEX / GameInfo.bin
// ---------------------------------------------------------------------------

fn trim_xex_module_name(value: &str) -> String {
    value.trim_end_matches(".pe").trim_end_matches(".xex").trim().to_string()
}

fn read_xex_execution_title_id(bytes: &[u8], start: usize) -> Option<u32> {
    let header_size = read_u32_be(bytes, start + 0x08)? as usize;
    let header_count = read_u32_be(bytes, start + 0x14)? as usize;
    let header_end = start.checked_add(header_size)?;
    let header_bytes = bytes.get(start..header_end)?;

    for index in 0..header_count {
        let entry_offset = 0x18 + index * 8;
        let key = read_u32_be(header_bytes, entry_offset)?;
        if key != 0x00040006 {
            continue;
        }
        let opt_offset = read_u32_be(header_bytes, entry_offset + 4)? as usize;
        let opt_bytes = header_bytes.get(opt_offset..opt_offset + 0x18)?;
        return read_u32_be(opt_bytes, 0x0C);
    }
    None
}

fn read_xex_original_pe_name(bytes: &[u8], start: usize) -> Option<String> {
    let header_size = read_u32_be(bytes, start + 0x08)? as usize;
    let header_count = read_u32_be(bytes, start + 0x14)? as usize;
    let header_end = start.checked_add(header_size)?;
    let header_bytes = bytes.get(start..header_end)?;

    for index in 0..header_count {
        let entry_offset = 0x18 + index * 8;
        let key = read_u32_be(header_bytes, entry_offset)?;
        if key != 0x0001_83FF {
            continue;
        }
        let opt_offset = read_u32_be(header_bytes, entry_offset + 4)? as usize;
        let opt = header_bytes.get(opt_offset..)?;
        let size = read_u32_be(opt, 0)? as usize;
        let name_bytes = opt.get(4..size)?;
        let nul = name_bytes.iter().position(|byte| *byte == 0).unwrap_or(name_bytes.len());
        let name = String::from_utf8_lossy(&name_bytes[..nul]).trim().to_string();
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

/// Ported verbatim from `reference/old_archiver.rs`'s `parse_xbox360_xex_metadata_from_bytes`
/// (line 3772) -- `DEFAULT.XEX`'s execution-header title id plus the original PE module name
/// (best-effort title, since XEX has no separate localized-title table like GameInfo.bin).
pub fn parse_xbox360_xex_metadata_from_bytes(bytes: &[u8]) -> Option<(Option<String>, Option<String>)> {
    let xex_offset = bytes.windows(4).position(|window| window == b"XEX2")?;
    let title_id = read_xex_execution_title_id(bytes, xex_offset)?;
    let title = read_xex_original_pe_name(bytes, xex_offset).and_then(|name| sanitize_title_opt(Some(trim_xex_module_name(&name))));
    let serial = sanitize_serial_opt(Some(format!("{:08X}", title_id)));
    Some((title, serial))
}

/// Serial-only convenience over [`parse_xbox360_xex_metadata_from_bytes`], matching old
/// code's `parse_xbox360_title_id_from_xex_bytes` (line 3768).
pub fn parse_xbox360_title_id_from_xex_bytes(bytes: &[u8]) -> Option<Option<String>> {
    parse_xbox360_xex_metadata_from_bytes(bytes).map(|(_, serial)| serial)
}

/// Ported verbatim from `reference/old_archiver.rs`'s `parse_xbox360_gameinfo_from_bytes`
/// (line 3837) -- `GameInfo.bin`'s own `EXEC`/`COMM`/`TITL` chunked format, preferred over
/// XEX when present since it carries a proper localized title.
pub fn parse_xbox360_gameinfo_from_bytes(bytes: &[u8]) -> Option<(Option<String>, Option<String>)> {
    let mut title: Option<String> = None;
    let mut serial: Option<String> = None;

    let mut offset = 0usize;
    while offset + 8 <= bytes.len() {
        let magic = bytes.get(offset..offset + 4)?;
        let data_offset = offset + 8;

        if magic == b"EXEC" {
            let data_end = data_offset.checked_add(33 + 43 + 65)?;
            let data = bytes.get(data_offset..data_end)?;
            let virtual_titleid = read_fixed_ascii_string(data, 0, 33);
            let module_name = read_fixed_ascii_string(data, 33, 43);
            if title.is_none() {
                title = sanitize_title_opt(module_name.or(virtual_titleid.clone()));
            }
            offset = data_end;
            continue;
        }

        let block_size = read_u32_be(bytes, offset + 4)? as usize;
        let data_end = data_offset.checked_add(block_size)?;
        let data = bytes.get(data_offset..data_end)?;

        if magic == b"COMM" {
            if let Some(title_id) = read_u32_be(data, 0) {
                serial = sanitize_serial_opt(Some(format!("{:08X}", title_id)));
            }
        } else if magic == b"TITL" {
            let title_bytes = data.get(0..128 * 2)?;
            if title.is_none() {
                title = sanitize_title_opt(read_utf16_best_effort(title_bytes));
            }
        }

        offset = data_end;
    }

    if title.is_none() && serial.is_none() {
        return None;
    }

    Some((title, serial))
}

// ---------------------------------------------------------------------------
// Sega "IP.BIN" hardware ID (Dreamcast/Saturn/Sega CD) and GameCube/Wii disc magic words
// ---------------------------------------------------------------------------
//
// New: `reference/old_archiver.rs` never validated these six platforms' disc content at
// all -- `disc_title_from_bytes`/`disc_serial_from_bytes` just read two fixed byte ranges
// unconditionally and accepted whatever came back, so nothing here actually distinguished
// (say) a GameCube disc from a Wii disc, or rejected a non-Saturn disc offered as a Saturn
// candidate. These are documented signatures: the CD-based three sit at absolute byte
// offset 0x10 (past the 16-byte Mode 1 sync/header every raw 2352-byte-sector track begins
// with -- offset 0x00 for a "cooked" 2048-byte-sector rip with no sync header), and
// GameCube/Wii's 4-byte magic words sit at 0x1C/0x18.

/// Sega Saturn/Mega-CD/Dreamcast disc hardware-ID strings, ASCII, 16 bytes each
/// (space-padded). Source: Sega's own Disc Format Standards Specification Sheet (Saturn),
/// retrodev.com (Sega CD), mc.pp.se/dc/ip.bin.html (Dreamcast).
const SEGA_CD_HARDWARE_IDS: [&[u8]; 4] = [b"SEGADISCSYSTEM", b"SEGABOOTDISC", b"SEGADATADISC", b"SEGADISC"];
const SATURN_HARDWARE_ID: &[u8] = b"SEGA SEGASATURN";
const DREAMCAST_HARDWARE_ID: &[u8] = b"SEGA SEGAKATANA";

/// Finds `needle` within the first `search_len` bytes, returning the offset it starts at.
/// Real CD-track rips come in both "raw" (2352-byte sectors, a 16-byte Mode 1 sync/header
/// before user data -- hardware ID at 0x10) and "cooked" (2048-byte, no sync header --
/// hardware ID at 0x00) forms, so this searches a small window rather than trusting one
/// fixed offset.
fn find_ascii_within(bytes: &[u8], needle: &[u8], search_len: usize) -> Option<usize> {
    let window = bytes.get(..search_len.min(bytes.len()))?;
    window.windows(needle.len()).position(|w| w == needle)
}

/// Returns the offset Sega's hardware-ID string was found at for `target`, or `None` if
/// this buffer isn't really that platform's disc -- the actual validating check (previously
/// nonexistent) for Dreamcast/Saturn/Sega CD.
pub fn sega_disc_hardware_id_offset(bytes: &[u8], target: Platform) -> Option<usize> {
    match target {
        Platform::Dreamcast => find_ascii_within(bytes, DREAMCAST_HARDWARE_ID, 0x20),
        Platform::SegaSaturn => find_ascii_within(bytes, SATURN_HARDWARE_ID, 0x20),
        // Mega CD / Sega CD boot sectors carry one of four hardware ids (`SEGADISCSYSTEM` on
        // bootable discs, `SEGADATADISC` on data-only ones, and two rarer forms).
        Platform::SegaCd => SEGA_CD_HARDWARE_IDS.iter().find_map(|needle| find_ascii_within(bytes, needle, 0x20)),
        _ => None,
    }
}

/// 3DO's Opera filesystem volume header at sector 0: record type 1, five `0x5A` sync
/// bytes, version 1 -- no ISO9660 at all. Returns the offset it matched at (0, or the
/// start of the sector's user data in a raw 2352-byte layout).
pub fn three_do_signature_offset(bytes: &[u8]) -> Option<usize> {
    const OPERA_SYNC: [u8; 7] = [0x01, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x01];
    [0usize, 16, 24].into_iter().find(|&at| bytes.get(at..at + OPERA_SYNC.len()) == Some(&OPERA_SYNC[..]))
}

/// The 3DO volume label: 32 bytes at 0x28 of the Opera volume header.
fn three_do_title_from_bytes(bytes: &[u8], signature_offset: usize) -> Option<String> {
    read_ascii_field(bytes.get(signature_offset + 0x28..signature_offset + 0x48)?)
}

/// PC-FX boot sector: `PC-FX:Hu_CD-ROM` at the start of the first data sector (the same
/// place PC Engine CD keeps its IPL string one sector later).
pub const PCFX_SIGNATURE: &[u8] = b"PC-FX:Hu_CD-ROM";

pub fn pcfx_signature_offset(bytes: &[u8]) -> Option<usize> {
    let window = bytes.get(..0x100000.min(bytes.len()))?;
    window.windows(PCFX_SIGNATURE.len()).position(|w| w == PCFX_SIGNATURE)
}

/// PC Engine CD fallback when the IPL string is absent: the ISO9660 primary volume
/// descriptor's system id names the platform (`PC ENGINE` or `NEC`). Returns the volume id
/// as the title.
fn pce_cd_pvd_fallback(bytes: &[u8]) -> Option<String> {
    let window = bytes.get(..0x40000.min(bytes.len()))?;
    let mut at = 0;
    while let Some(pos) = window[at..].windows(5).position(|w| w == b"CD001") {
        let descriptor = at + pos - 1; // "CD001" sits at offset 1 of the descriptor
        if let Some(system_id) = window.get(descriptor + 8..descriptor + 40) {
            let system_id = String::from_utf8_lossy(system_id).to_ascii_uppercase();
            if system_id.contains("PC ENGINE") || system_id.contains("NEC") {
                let volume_id = window.get(descriptor + 40..descriptor + 72).and_then(read_ascii_field);
                return Some(volume_id.unwrap_or_default());
            }
        }
        at += pos + 5;
    }
    None
}

/// PC Engine CD / TurboGrafx-CD boot signature, ASCII, at offset 0x20 of the sector after the
/// data track's first sector (the IPL sector). Where that lands depends on the sector size
/// (2048 cooked, 2352 raw) and on whether the track file includes its pregap (typically 150
/// or 225 sectors), so the first 1MB is searched rather than one fixed offset.
pub const PCE_CD_SIGNATURE: &[u8] = b"PC Engine CD-ROM SYSTEM";
const PCE_CD_TITLE_OFFSET_IN_SECTOR: usize = 0x6A;
const PCE_CD_TITLE_LEN: usize = 22;

pub fn pce_cd_signature_offset(bytes: &[u8]) -> Option<usize> {
    let window = bytes.get(..0x100000.min(bytes.len()))?;
    window.windows(PCE_CD_SIGNATURE.len()).position(|w| w == PCE_CD_SIGNATURE)
}

/// The game title, 22 bytes at 0x6A of the same sector the signature sits in. Only printable
/// ASCII is kept: a Shift-JIS title decodes to nothing rather than to mojibake.
fn pce_cd_title_from_bytes(bytes: &[u8], signature_offset: usize) -> Option<String> {
    let sector_start = signature_offset.checked_sub(0x20)?;
    let start = sector_start + PCE_CD_TITLE_OFFSET_IN_SECTOR;
    read_ascii_field(bytes.get(start..start + PCE_CD_TITLE_LEN)?)
}

const GAMECUBE_MAGIC: [u8; 4] = [0xC2, 0x33, 0x9F, 0x3D];
const GAMECUBE_MAGIC_OFFSET: usize = 0x1C;
const WII_MAGIC: [u8; 4] = [0x5D, 0x1C, 0x9E, 0xA3];
const WII_MAGIC_OFFSET: usize = 0x18;

/// GameCube's and Wii's disc header magic words -- mutually exclusive fixed 4-byte values
/// at fixed offsets (Nintendo's own disc format). Returns the offset it matched at (always the same
/// constant per platform, but kept as a return value for a uniform call shape with
/// [`sega_disc_hardware_id_offset`]).
pub fn nintendo_disc_magic_offset(bytes: &[u8], target: Platform) -> Option<usize> {
    match target {
        Platform::GameCube => (bytes.get(GAMECUBE_MAGIC_OFFSET..GAMECUBE_MAGIC_OFFSET + 4)? == GAMECUBE_MAGIC).then_some(GAMECUBE_MAGIC_OFFSET),
        Platform::Wii => (bytes.get(WII_MAGIC_OFFSET..WII_MAGIC_OFFSET + 4)? == WII_MAGIC).then_some(WII_MAGIC_OFFSET),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Generic disc-header fallback (weak last-resort guess; matches old code's own
// always-constant `disc_header_offset`/`disc_title_offset`)
// ---------------------------------------------------------------------------

/// Ported verbatim from `reference/old_archiver.rs`'s `disc_title_from_bytes` (line 4924).
/// Old code parameterized this by `ArchiveFormat` but every arm resolved to the same fixed
/// offset (0x20) -- ported as a plain fixed-offset read.
fn disc_title_from_bytes(bytes: &[u8]) -> Option<String> {
    let slice = bytes.get(0x20..0x40)?;
    let title = String::from_utf8_lossy(slice).trim_end_matches('\0').trim().to_string();
    if title.is_empty() { None } else { Some(title) }
}

/// Ported verbatim from `reference/old_archiver.rs`'s `disc_serial_from_bytes` (line 4939),
/// same fixed-offset simplification as [`disc_title_from_bytes`].
fn disc_serial_from_bytes(bytes: &[u8]) -> Option<String> {
    let slice = bytes.get(0..8)?;
    let text = String::from_utf8_lossy(slice).trim_end_matches('\0').to_string();
    let cleaned = text.chars().filter(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit()).take(8).collect::<String>();
    if cleaned.is_empty() { None } else { Some(cleaned) }
}

// ---------------------------------------------------------------------------
// Top-level dispatch
// ---------------------------------------------------------------------------

/// Extracts (title, serial, manufacturer_id) from a byte buffer that may be a probe (a
/// prefix, or a small chunk read for cheap deep-cycle scoring) or a full descriptor/archive
/// payload -- both cases route through the same per-platform overrides below, differing
/// only in the PS2 title source (`is_probe`: a probe buffer skips straight to the volume-id
/// scan since a full-file ISO9660 walk isn't worth it there; a full payload does the fixed
/// generic read since the volume-id read only works when the whole ISO's PVD is present at
/// its expected LBA, which a truncated probe buffer may not have).
///
/// Merges `reference/old_archiver.rs`'s `disc_metadata_from_bytes` (line 3346) and
/// `disc_metadata_from_probe_bytes` (line 3403), which otherwise duplicated every match arm.
pub fn disc_metadata_from_bytes(bytes: &[u8], target: Platform, is_probe: bool) -> DiscTitleSerial {
    // Real validation for the six platforms that otherwise had none: reject outright when
    // this buffer doesn't carry the platform's own disc magic, instead of blindly reading
    // the two generic fixed offsets below and accepting whatever text happens to be there.
    if matches!(target, Platform::Dreamcast | Platform::SegaSaturn | Platform::SegaCd) && sega_disc_hardware_id_offset(bytes, target).is_none()
    {
        return (None, None, None);
    }
    if matches!(target, Platform::GameCube | Platform::Wii) && nintendo_disc_magic_offset(bytes, target).is_none() {
        return (None, None, None);
    }
    if target == Platform::TurboGrafxCd {
        let Some(signature_offset) = pce_cd_signature_offset(bytes) else {
            // Rare releases without the IPL string: the PVD system id still names the platform.
            return match pce_cd_pvd_fallback(bytes) {
                Some(volume_id) => (Some(volume_id), None, None),
                None => (None, None, None),
            };
        };
        let serial = parse_pce_cd_serial_from_bytes(bytes);
        return (pce_cd_title_from_bytes(bytes, signature_offset), serial.clone(), serial);
    }
    if target == Platform::ThreeDo {
        let Some(signature_offset) = three_do_signature_offset(bytes) else { return (None, None, None) };
        // No serial on 3DO discs; the volume label (or the sync itself) is the evidence.
        let title = three_do_title_from_bytes(bytes, signature_offset).or_else(|| Some("3DO".to_string()));
        return (title, None, None);
    }
    if target == Platform::PcFx {
        let Some(signature_offset) = pcfx_signature_offset(bytes) else { return (None, None, None) };
        // The boot sector carries the title right after the 16-byte signature block.
        let title = read_ascii_field(bytes.get(signature_offset + 0x10..signature_offset + 0x30).unwrap_or(&[])).or_else(|| Some("PC-FX".to_string()));
        return (title, None, None);
    }

    let mut title = if is_probe && target == Platform::PlayStation2 {
        disc_fs::read_iso_volume_id_from_bytes(bytes).or_else(|| disc_title_from_bytes(bytes))
    } else {
        disc_title_from_bytes(bytes)
    };
    let mut serial = disc_serial_from_bytes(bytes);

    match target {
        Platform::PlayStation => {
            if let Some(ps1_serial) = parse_playstation_boot_serial_from_bytes(bytes, "BOOT") {
                serial = Some(ps1_serial);
            }
        }
        Platform::PlayStation2 => {
            if let Some(ps2_serial) = parse_playstation_boot_serial_from_bytes(bytes, "BOOT2") {
                serial = Some(ps2_serial);
            } else if let Some(ps1_serial) = parse_playstation_boot_serial_from_bytes(bytes, "BOOT") {
                serial = Some(ps1_serial);
            }
        }
        Platform::PlayStationPortable => {
            if let Some((psp_title, psp_serial)) = parse_psp_param_sfo_from_bytes(bytes) {
                title = psp_title.or(title);
                serial = psp_serial.or(serial);
            }
        }
        Platform::PlayStation3 => {
            if let Some((ps3_title, ps3_serial)) = parse_ps3_param_sfo_from_bytes(bytes) {
                title = ps3_title.or(title);
                serial = ps3_serial.or(serial);
            }
        }
        Platform::Xbox => {
            if let Some((xbox_title, xbox_serial)) = parse_xbe_metadata_from_bytes(bytes) {
                title = xbox_title.or(title);
                serial = xbox_serial.or(serial);
            }
        }
        Platform::Xbox360 => {
            if let Some((xbox_title, xbox_serial)) = parse_xbox360_gameinfo_from_bytes(bytes) {
                title = xbox_title.or(title);
                serial = xbox_serial.or(serial);
            } else if let Some(xbox_serial) = parse_xbox360_title_id_from_xex_bytes(bytes) {
                serial = xbox_serial.or(serial);
            }
        }
        _ => {}
    }

    let manufacturer_id = serial.clone();
    (title, serial, manufacturer_id)
}

/// Extracts (title, serial, manufacturer_id) by walking the disc's own filesystem
/// (ISO9660, or XDVDFS for Xbox/Xbox 360) for a platform-specific named entry -- much more
/// reliable than the byte-scan path above when the whole disc image is available on disk as
/// a real file. Ported from `reference/old_archiver.rs`'s `disc_metadata_from_disc_file`
/// (line 4058): the `format`-based backend dispatch (raw ISO / CHD logical range / RVZ
/// logical range) and the CHD-prefix serial fallback are both dropped, since `path` here is
/// always a real, already-extracted file.
pub fn disc_metadata_from_disc_file(path: &Path, target: Platform) -> Option<DiscTitleSerial> {
    let read_xdvdfs = |components: &[&str]| disc_fs::read_xdvdfs_file_bytes_case_insensitive(path, target, components);

    match target {
        Platform::PlayStationPortable | Platform::PlayStation3 | Platform::PlayStation | Platform::PlayStation2 | Platform::NeoGeoCd => {
            let mut reader = |offset: u64, len: usize| disc_fs::read_exact_at(path, offset, len);
            disc_metadata_with_reader(&mut reader, target)
        }
        Platform::Xbox => {
            let xbe = read_xdvdfs(&["DEFAULT.XBE"])?;
            let (title, serial) = parse_xbe_metadata_from_bytes(&xbe)?;
            let manufacturer_id = serial.clone();
            Some((title, serial, manufacturer_id))
        }
        Platform::Xbox360 => {
            let gameinfo = read_xdvdfs(&["GameInfo.bin"]).and_then(|bytes| parse_xbox360_gameinfo_from_bytes(&bytes));
            if let Some((title, serial)) = gameinfo {
                let manufacturer_id = serial.clone();
                Some((title, serial, manufacturer_id))
            } else {
                let xex = read_xdvdfs(&["DEFAULT.XEX"])?;
                let (title, serial) = parse_xbox360_xex_metadata_from_bytes(&xex)?;
                let manufacturer_id = serial.clone();
                Some((title, serial, manufacturer_id))
            }
        }
        _ => None,
    }
}

/// The ISO9660-based arms of [`disc_metadata_from_disc_file`] (PSP, PS3, PS1, PS2) over any
/// byte reader -- e.g. the data track of a CHD read sector by sector, without extracting it.
/// Returns `None` for other platforms.
pub fn disc_metadata_with_reader<F: disc_fs::DiscByteReader>(reader: &mut F, target: Platform) -> Option<DiscTitleSerial> {
    match target {
        Platform::PlayStationPortable => {
            let sfo = disc_fs::read_iso_file_bytes_with(reader, &["PSP_GAME", "PARAM.SFO"])?;
            let (title, serial) = parse_ps3_param_sfo_from_bytes(&sfo)?;
            let manufacturer_id = serial.clone();
            Some((title, serial, manufacturer_id))
        }
        Platform::PlayStation3 => {
            match disc_fs::read_iso_file_bytes_with(reader, &["PS3_GAME", "PARAM.SFO"]).and_then(|sfo| parse_ps3_param_sfo_from_bytes(&sfo)) {
                Some((title, serial)) => {
                    let manufacturer_id = serial.clone();
                    Some((title, serial, manufacturer_id))
                }
                // PS3 discs also carry a plain `PlayStation3` string at the start of sector 1.
                None => {
                    let sector1 = reader(2048, 12).ok()?;
                    (sector1 == b"PlayStation3").then(|| (disc_fs::read_iso_volume_id_with(reader), None, None))
                }
            }
        }
        Platform::NeoGeoCd => {
            // Every Neo Geo CD carries the loader script `IPL.TXT` in the ISO9660 root
            // (`ABS.TXT` is the usual companion); the volume id is the only title the disc offers.
            let ipl = disc_fs::read_iso_file_bytes_with(reader, &["IPL.TXT"]).or_else(|| disc_fs::read_iso_file_bytes_with(reader, &["ABS.TXT"]))?;
            let title = disc_fs::read_iso_volume_id_with(reader).or_else(|| (!ipl.is_empty()).then(|| "NEO-GEO CD".to_string()));
            // The game id (NGH number) sits behind the `NEO-GEO` marker in the program files
            // the loader script lists (`NAME.PRG,bank,offset` per line): read each in turn.
            let serial = String::from_utf8_lossy(&ipl)
                .lines()
                .filter_map(|line| line.split(',').next().map(|name| name.trim().to_string()))
                .filter(|name| !name.is_empty())
                .find_map(|name| {
                    let bytes = disc_fs::read_iso_file_bytes_with(reader, &[name.as_str()])?;
                    super::super::platforms::other::infer_neogeo_cd_game_id_from_bytes(&bytes)
                });
            Some((title, serial.clone(), serial))
        }
        Platform::PlayStation => {
            let serial = disc_fs::read_iso_file_bytes_with(reader, &["SYSTEM.CNF"]).and_then(|bytes| parse_playstation_boot_serial_from_bytes(&bytes, "BOOT"))?;
            let manufacturer_id = serial.clone();
            Some((None, Some(serial), Some(manufacturer_id)))
        }
        Platform::PlayStation2 => {
            let title = disc_fs::read_iso_volume_id_with(reader);
            let serial = disc_fs::read_iso_file_bytes_with(reader, &["SYSTEM.CNF"]).and_then(|bytes| {
                parse_playstation_boot_serial_from_bytes(&bytes, "BOOT2").or_else(|| parse_playstation_boot_serial_from_bytes(&bytes, "BOOT"))
            })?;
            let manufacturer_id = serial.clone();
            Some((title, Some(serial), Some(manufacturer_id)))
        }
        _ => None,
    }
}
