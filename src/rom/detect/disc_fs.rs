//! Shared ISO9660 and XDVDFS filesystem readers, generic over a `(offset, len) -> bytes`
//! reader closure. Ported (bodies verbatim) from `reference/old_archiver.rs`, collapsing
//! what was there 6 near-identical per-storage-backend wrapper functions (raw file / CHD
//! logical-range / RVZ logical-range, x2 filesystems) down to 2: since CHD/RVZ inputs are
//! now always extracted to a real file on disk before detection reads them (see
//! `super::super::extract`), every caller here just reads from a `&Path` -- there is no
//! more "native logical-range" backend to special-case.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::common::models::Platform;

// ---------------------------------------------------------------------------
// Low-level byte readers (shared with other detect/platforms modules)
// ---------------------------------------------------------------------------

pub fn read_u16_le(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(offset..offset + 2)?.try_into().ok()?))
}

pub fn read_u32_le(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?))
}

pub fn read_u64_le(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(offset..offset + 8)?.try_into().ok()?))
}

/// Reads `len` bytes at `offset` from a real file. The one and only "backend" reader
/// now that CHD/RVZ are always extracted to a real file before this runs.
pub fn read_exact_at(path: &Path, offset: u64, len: usize) -> Result<Vec<u8>, String> {
    let mut file = File::open(path).map_err(|err| format!("failed to open {}: {err}", path.display()))?;
    file.seek(SeekFrom::Start(offset)).map_err(|err| format!("failed to seek {}: {err}", path.display()))?;
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf).map_err(|err| format!("failed to read {}: {err}", path.display()))?;
    Ok(buf)
}

// ---------------------------------------------------------------------------
// ISO9660
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct IsoDirEntry {
    extent_lba: u32,
    data_len: u32,
    is_dir: bool,
    name: String,
}

#[derive(Debug, Clone, Copy)]
struct IsoLayout {
    sector_stride: u64,
    user_data_offset: u64,
}

impl IsoLayout {
    fn user_data_at_lba(self, lba: u32) -> u64 {
        self.user_data_offset + (lba as u64 * self.sector_stride)
    }
}

fn read_iso_primary_volume_descriptor<F>(reader: &mut F) -> Option<(Vec<u8>, IsoLayout)>
where
    F: FnMut(u64, usize) -> Result<Vec<u8>, String>,
{
    let layouts = [
        IsoLayout { sector_stride: 2048, user_data_offset: 0 },
        IsoLayout { sector_stride: 2352, user_data_offset: 16 },
        IsoLayout { sector_stride: 2352, user_data_offset: 24 },
    ];

    let mut probe = |layout: IsoLayout, lba: u64| {
        let sector = reader(layout.user_data_offset + lba * layout.sector_stride, 2048).ok()?;
        (sector.first().copied() == Some(0x01) && sector.get(1..6) == Some(b"CD001")).then_some((sector, layout))
    };

    // The standard locations first, in every layout (one sector each), before the slow scan:
    // a raw (2352-byte) image otherwise pays for ~500 failed 2048-stride reads first.
    for lba in [16u64, 166u64] {
        for layout in layouts {
            if let Some(found) = probe(layout, lba) {
                return Some(found);
            }
        }
    }
    for layout in layouts {
        for lba in 0u64..512u64 {
            if let Some(found) = probe(layout, lba) {
                return Some(found);
            }
        }
    }

    None
}

fn normalize_iso_name(name: &str) -> String {
    let no_version = name.split(';').next().unwrap_or(name);
    let trimmed = no_version.trim_end_matches('.');
    trimmed.to_ascii_uppercase()
}

fn parse_iso_dir_record(record: &[u8]) -> Option<IsoDirEntry> {
    if record.len() < 34 {
        return None;
    }
    let name_len = *record.get(32)? as usize;
    if 33 + name_len > record.len() {
        return None;
    }

    let extent_lba = u32::from_le_bytes(record.get(2..6)?.try_into().ok()?);
    let data_len = u32::from_le_bytes(record.get(10..14)?.try_into().ok()?);
    let flags = *record.get(25)?;
    let raw_name = String::from_utf8_lossy(record.get(33..33 + name_len)?).to_string();

    Some(IsoDirEntry { extent_lba, data_len, is_dir: (flags & 0x02) != 0, name: normalize_iso_name(&raw_name) })
}

fn read_iso_extent_with_layout<F>(reader: &mut F, layout: IsoLayout, extent_lba: u32, data_len: u32) -> Result<Vec<u8>, String>
where
    F: FnMut(u64, usize) -> Result<Vec<u8>, String>,
{
    let total = data_len as usize;
    if layout.sector_stride == 2048 && layout.user_data_offset == 0 {
        return reader(layout.user_data_at_lba(extent_lba), total);
    }

    let mut out = Vec::with_capacity(total);
    let mut remaining = total;
    let mut lba = extent_lba;
    while remaining > 0 {
        let take = remaining.min(2048);
        let chunk = reader(layout.user_data_at_lba(lba), take)?;
        out.extend_from_slice(&chunk);
        remaining -= take;
        lba = lba.saturating_add(1);
    }

    Ok(out)
}

fn read_iso_directory_entries_with_reader<F>(
    reader: &mut F,
    layout: IsoLayout,
    extent_lba: u32,
    data_len: u32,
) -> Result<Vec<IsoDirEntry>, String>
where
    F: FnMut(u64, usize) -> Result<Vec<u8>, String>,
{
    let bytes = read_iso_extent_with_layout(reader, layout, extent_lba, data_len)?;

    let mut out = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        let record_len = bytes[index] as usize;
        if record_len == 0 {
            let next_sector = ((index / 2048) + 1) * 2048;
            if next_sector <= index {
                break;
            }
            index = next_sector;
            continue;
        }
        if index + record_len > bytes.len() {
            break;
        }

        if let Some(entry) = parse_iso_dir_record(&bytes[index..index + record_len]) {
            let name_bytes = bytes[index + 33..index + 33 + (bytes[index + 32] as usize)].to_vec();
            if !(name_bytes.len() == 1 && (name_bytes[0] == 0 || name_bytes[0] == 1)) {
                out.push(entry);
            }
        }

        index += record_len;
    }

    Ok(out)
}

fn read_iso_file_bytes_with_reader<F>(reader: &mut F, components: &[&str]) -> Option<Vec<u8>>
where
    F: FnMut(u64, usize) -> Result<Vec<u8>, String>,
{
    if components.is_empty() {
        return None;
    }

    let (pvd, layout) = read_iso_primary_volume_descriptor(reader)?;
    let root = parse_iso_dir_record(pvd.get(156..)?)?;
    let mut current = root;

    for (index, component) in components.iter().enumerate() {
        let entries = read_iso_directory_entries_with_reader(reader, layout, current.extent_lba, current.data_len).ok()?;
        let wanted = normalize_iso_name(component);
        let entry = entries.into_iter().find(|item| item.name == wanted)?;
        let is_last = index + 1 == components.len();
        if !is_last && !entry.is_dir {
            return None;
        }
        current = entry;
    }

    read_iso_extent_with_layout(reader, layout, current.extent_lba, current.data_len).ok()
}

/// Reads a file at `components` (e.g. `["PSP_GAME", "PARAM.SFO"]`) out of an ISO9660
/// filesystem on a real file (the original raw ISO, or an already-extracted CHD/RVZ
/// payload -- both are just a plain file by this point).
pub fn read_iso_file_bytes_case_insensitive(path: &Path, components: &[&str]) -> Option<Vec<u8>> {
    let mut reader = |offset: u64, len: usize| read_exact_at(path, offset, len);
    read_iso_file_bytes_with_reader(&mut reader, components)
}

fn read_iso_volume_id_with_reader<F>(reader: &mut F) -> Option<String>
where
    F: FnMut(u64, usize) -> Result<Vec<u8>, String>,
{
    let (pvd, _) = read_iso_primary_volume_descriptor(reader)?;
    let raw = pvd.get(40..72)?;
    let text = String::from_utf8_lossy(raw).trim_end_matches('\0').trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

fn sanitize_iso_volume_id(value: &str) -> Option<String> {
    let cleaned = value
        .chars()
        .map(|ch| if ch.is_ascii_graphic() || ch == ' ' { Some(ch) } else { None })
        .collect::<Option<String>>()?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    if cleaned.is_empty() {
        return None;
    }

    let upper = cleaned.to_ascii_uppercase();
    if matches!(upper.as_str(), "PLAYSTATION" | "PLAYSTATION 2" | "PS2DVD") {
        return None;
    }

    Some(cleaned)
}

/// A reader over any byte source: `(offset, len) -> exactly len bytes at offset`. The
/// `*_with` functions below run the same ISO9660 logic as their path-based counterparts
/// against it (e.g. sector reads straight out of a compressed CHD).
pub trait DiscByteReader: FnMut(u64, usize) -> Result<Vec<u8>, String> {}
impl<F: FnMut(u64, usize) -> Result<Vec<u8>, String>> DiscByteReader for F {}

/// [`read_iso_file_bytes_case_insensitive`] over a reader.
pub fn read_iso_file_bytes_with<F: DiscByteReader>(reader: &mut F, components: &[&str]) -> Option<Vec<u8>> {
    read_iso_file_bytes_with_reader(reader, components)
}

/// [`read_iso_volume_id`] over a reader.
pub fn read_iso_volume_id_with<F: DiscByteReader>(reader: &mut F) -> Option<String> {
    sanitize_iso_volume_id(&read_iso_volume_id_with_reader(reader)?)
}

/// The primary volume descriptor's system identifier (bytes 8..40, e.g. "PLAYSTATION",
/// "PSP GAME") -- one sector read once the descriptor is found.
pub fn read_iso_system_id_with<F: DiscByteReader>(reader: &mut F) -> Option<String> {
    let (pvd, _) = read_iso_primary_volume_descriptor(reader)?;
    let text = String::from_utf8_lossy(pvd.get(8..40)?).trim_end_matches('\0').trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

/// Reads the ISO9660 primary volume descriptor's volume-id field (e.g. used as PS2's
/// title, since PS2 discs don't reliably expose a title any other way).
pub fn read_iso_volume_id(path: &Path) -> Option<String> {
    let mut reader = |offset: u64, len: usize| read_exact_at(path, offset, len);
    sanitize_iso_volume_id(&read_iso_volume_id_with_reader(&mut reader)?)
}

/// Same as [`read_iso_volume_id`] but against an in-memory probe buffer (e.g. a zip/7z
/// entry already extracted to a `Vec<u8>`).
pub fn read_iso_volume_id_from_bytes(bytes: &[u8]) -> Option<String> {
    let mut reader = |offset: u64, len: usize| {
        let start = offset as usize;
        let end = start.saturating_add(len);
        bytes.get(start..end).map(|s| s.to_vec()).ok_or_else(|| "probe bytes too short for ISO read".to_string())
    };
    sanitize_iso_volume_id(&read_iso_volume_id_with_reader(&mut reader)?)
}

// ---------------------------------------------------------------------------
// XDVDFS (Xbox / Xbox 360)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct XdvdfsDirEntry {
    extent_sector: u32,
    data_len: u32,
    is_dir: bool,
    name: String,
}

fn detect_xdvdfs_partition_base<F>(reader: &mut F, target: Platform) -> Option<u64>
where
    F: FnMut(u64, usize) -> Result<Vec<u8>, String>,
{
    const XDVDFS_MAGIC: &[u8; 20] = b"MICROSOFT*XBOX*MEDIA";

    let candidates: &[u64] = match target {
        // XGD2, XGD3 (Xbox 360), XGD1 (Xbox), and 0 for an XISO.
        Platform::Xbox360 => &[0x0FD9_0000, 0x0208_0000, 0x1830_0000, 0],
        Platform::Xbox => &[0x1830_0000, 0, 0x0FD9_0000],
        _ => &[],
    };

    for base_offset in candidates {
        let offset = base_offset + (32 * 2048);
        let sector = reader(offset, 2048).ok()?;
        if sector.get(..20) == Some(XDVDFS_MAGIC) {
            return Some(*base_offset);
        }
    }

    None
}

fn parse_xdvdfs_directory_entries(bytes: &[u8]) -> Option<Vec<XdvdfsDirEntry>> {
    let mut entries = Vec::new();
    let mut offset = 0usize;

    while offset + 14 <= bytes.len() {
        let left = read_u16_le(bytes, offset)?;
        let right = read_u16_le(bytes, offset + 2)?;
        let extent_sector = read_u32_le(bytes, offset + 4)?;
        let data_len = read_u32_le(bytes, offset + 8)?;
        let attributes = *bytes.get(offset + 12)?;
        let name_len = *bytes.get(offset + 13)? as usize;
        if left == 0xFFFF && right == 0xFFFF && extent_sector == u32::MAX && data_len == u32::MAX && attributes == 0xFF && name_len == 0xFF {
            break;
        }
        let name_start = offset + 14;
        let name_end = match name_start.checked_add(name_len) {
            Some(v) if v <= bytes.len() => v,
            _ => break,
        };
        let name = String::from_utf8_lossy(&bytes[name_start..name_end]).to_string();

        entries.push(XdvdfsDirEntry { extent_sector, data_len, is_dir: (attributes & 0x10) != 0, name });

        let entry_len = 14usize.checked_add(name_len)?;
        let aligned_len = (entry_len + 3) & !3;
        offset = offset.checked_add(aligned_len)?;
    }

    Some(entries)
}

fn read_xdvdfs_root_entries<F>(reader: &mut F, base_offset: u64) -> Option<Vec<XdvdfsDirEntry>>
where
    F: FnMut(u64, usize) -> Result<Vec<u8>, String>,
{
    let vd = reader(base_offset + (32 * 2048), 2048).ok()?;
    let root_sector = read_u32_le(&vd, 0x14)?;
    let root_size = read_u32_le(&vd, 0x18)?;
    let bytes = reader(base_offset + (root_sector as u64 * 2048), root_size as usize).ok()?;
    parse_xdvdfs_directory_entries(&bytes)
}

fn find_xdvdfs_entry<F>(reader: &mut F, base_offset: u64, entries: &[XdvdfsDirEntry], components: &[&str]) -> Option<XdvdfsDirEntry>
where
    F: FnMut(u64, usize) -> Result<Vec<u8>, String>,
{
    let wanted = components.first()?.to_ascii_uppercase();
    let entry = entries.iter().find(|entry| entry.name.eq_ignore_ascii_case(&wanted))?.clone();

    if components.len() == 1 {
        return Some(entry);
    }
    if !entry.is_dir {
        return None;
    }

    let dir_bytes = reader(base_offset + (entry.extent_sector as u64 * 2048), entry.data_len as usize).ok()?;
    let nested = parse_xdvdfs_directory_entries(&dir_bytes)?;
    find_xdvdfs_entry(reader, base_offset, &nested, &components[1..])
}

fn read_xdvdfs_file_bytes_with_reader<F>(reader: &mut F, target: Platform, components: &[&str]) -> Option<Vec<u8>>
where
    F: FnMut(u64, usize) -> Result<Vec<u8>, String>,
{
    if components.is_empty() {
        return None;
    }

    let base_offset = detect_xdvdfs_partition_base(reader, target)?;
    let root = read_xdvdfs_root_entries(reader, base_offset)?;
    let entry = find_xdvdfs_entry(reader, base_offset, &root, components)?;
    if entry.is_dir {
        return None;
    }

    reader(base_offset + (entry.extent_sector as u64 * 2048), entry.data_len as usize).ok()
}

/// Reads a file at `components` (e.g. `["DEFAULT.XBE"]`) out of an XDVDFS filesystem
/// (Xbox/Xbox 360) on a real file.
pub fn read_xdvdfs_file_bytes_case_insensitive(path: &Path, target: Platform, components: &[&str]) -> Option<Vec<u8>> {
    let mut reader = |offset: u64, len: usize| read_exact_at(path, offset, len);
    read_xdvdfs_file_bytes_with_reader(&mut reader, target, components)
}
