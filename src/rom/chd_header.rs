//! A CHD's header and metadata read in Rust, without chdman.
//!
//! chdman serialises every call in the process (`chdman_wrapper`'s lock), so asking it for
//! `info` waits behind any running CHD conversion -- tens of minutes, with a scan stalled
//! behind it. What `info` prints comes from two places in the file that need no
//! decompression: the fixed header at offset 0 and the metadata chain it points at. This
//! reads those, the way MAME's `chd_file` and libchdr do (`external/libchdr/src/
//! libchdr_chd.c`, `header_read` and `chd_get_metadata`), into the same [`ChdMetadata`]
//! `chd_info` builds from chdman's text.
//!
//! Versions 3, 4 and 5 are read. 1 and 2 (no metadata, no SHA-1) are refused, so the caller
//! asks chdman instead; so is anything this does not recognise.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use super::models::{ChdCodecs, ChdMetadata, ChdTrackTag, HardDiskGeometry};

const MAGIC: &[u8; 8] = b"MComprHD";
/// Header length by version, 1-based (libchdr's `header_sizes`).
const HEADER_SIZES: [u32; 5] = [76, 80, 120, 108, 124];
/// A metadata chain longer than this is a loop or garbage, not a disc.
const MAX_METADATA_ENTRIES: usize = 4096;
/// Largest metadata entry read (a track line is under 100 bytes; the length field is 24 bits).
const MAX_METADATA_BYTES: u32 = 1 << 20;
/// `CD_FRAME_SIZE`: 2352 bytes of sector plus 96 of subcode.
const CD_FRAME_SIZE: usize = 2448;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}

fn be64(b: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(b[at..at + 8].try_into().unwrap())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn tag(text: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*text)
}

/// One metadata entry: its tag, its index among entries of that tag, and its bytes.
struct Entry {
    tag: u32,
    index: usize,
    data: Vec<u8>,
}

/// The metadata chain from `offset`, in file order.
fn metadata(file: &mut File, mut offset: u64, file_len: u64) -> io::Result<Vec<Entry>> {
    let mut entries: Vec<Entry> = Vec::new();
    while offset != 0 {
        if entries.len() >= MAX_METADATA_ENTRIES {
            return Err(invalid("metadata chain does not end"));
        }
        if offset.checked_add(16).is_none_or(|end| end > file_len) {
            return Err(invalid(format!("metadata entry at {offset} is past the end of the file")));
        }
        let mut head = [0u8; 16];
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut head)?;
        let entry_tag = be32(&head, 0);
        // The top byte is flags (checksummed or not); the length is the low 24 bits.
        let length = be32(&head, 4) & 0x00ff_ffff;
        let next = be64(&head, 8);
        if length > MAX_METADATA_BYTES || offset + 16 + length as u64 > file_len {
            return Err(invalid(format!("metadata entry at {offset} runs past the end of the file")));
        }
        let mut data = vec![0u8; length as usize];
        file.read_exact(&mut data)?;
        let index = entries.iter().filter(|e| e.tag == entry_tag).count();
        entries.push(Entry { tag: entry_tag, index, data });
        if next != 0 && next <= offset {
            return Err(invalid("metadata chain points backwards"));
        }
        offset = next;
    }
    Ok(entries)
}

/// Metadata text as stored: NUL-terminated ASCII.
fn text(data: &[u8]) -> String {
    let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    String::from_utf8_lossy(&data[..end]).trim().to_string()
}

fn codec(raw: u32) -> Option<ChdCodecs> {
    Some(match &raw.to_be_bytes() {
        b"zlib" => ChdCodecs::Zlib,
        b"zstd" => ChdCodecs::Zstd,
        b"lzma" => ChdCodecs::Lzma,
        b"huff" => ChdCodecs::Huff,
        b"flac" => ChdCodecs::Flac,
        b"cdzl" => ChdCodecs::Cdzl,
        b"cdlz" => ChdCodecs::Cdlz,
        b"cdfl" => ChdCodecs::Cdfl,
        b"cdzs" => ChdCodecs::Cdzs,
        _ => return None,
    })
}

/// Reads `path`'s header and metadata. An error means "ask chdman": a version this does not
/// read, a codec it does not know, or a file that is not a sound CHD.
pub fn read(path: &Path) -> io::Result<ChdMetadata> {
    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let mut start = [0u8; 16];
    file.read_exact(&mut start)?;
    if &start[..8] != MAGIC {
        return Err(invalid("not a CHD"));
    }
    let length = be32(&start, 8);
    let version = be32(&start, 12);
    if !(3..=5).contains(&version) {
        return Err(invalid(format!("CHD version {version} is read by chdman only")));
    }
    if length != HEADER_SIZES[version as usize - 1] {
        return Err(invalid(format!("a v{version} header of {length} bytes")));
    }
    let mut h = vec![0u8; length as usize];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut h)?;

    let mut meta = ChdMetadata { file_version: version as usize, ..Default::default() };
    let (logical, hunk_bytes, total_hunks, meta_offset, unit_bytes, compressed, sha1, parent, raw_sha1);
    match version {
        3 => {
            logical = be64(&h, 28);
            meta_offset = be64(&h, 36);
            hunk_bytes = be32(&h, 76) as u64;
            total_hunks = be32(&h, 24) as u64;
            unit_bytes = None;
            compressed = old_compression(be32(&h, 20))?;
            sha1 = Some(&h[80..100]);
            parent = Some(&h[100..120]);
            raw_sha1 = None;
        }
        4 => {
            logical = be64(&h, 28);
            meta_offset = be64(&h, 36);
            hunk_bytes = be32(&h, 44) as u64;
            total_hunks = be32(&h, 24) as u64;
            unit_bytes = None;
            compressed = old_compression(be32(&h, 20))?;
            sha1 = Some(&h[48..68]);
            parent = Some(&h[68..88]);
            raw_sha1 = Some(&h[88..108]);
        }
        _ => {
            logical = be64(&h, 32);
            meta_offset = be64(&h, 48);
            hunk_bytes = be32(&h, 56) as u64;
            if hunk_bytes == 0 {
                return Err(invalid("hunk size 0"));
            }
            total_hunks = logical.div_ceil(hunk_bytes);
            unit_bytes = Some(be32(&h, 60) as u64);
            let mut codecs = Vec::new();
            for at in [16, 20, 24, 28] {
                let raw = be32(&h, at);
                if raw != 0 {
                    codecs.push(codec(raw).ok_or_else(|| invalid(format!("codec {:?}", String::from_utf8_lossy(&raw.to_be_bytes()))))?);
                }
            }
            compressed = codecs;
            raw_sha1 = Some(&h[64..84]);
            sha1 = Some(&h[84..104]);
            parent = Some(&h[104..124]);
        }
    }
    if hunk_bytes == 0 {
        return Err(invalid("hunk size 0"));
    }

    let entries = metadata(&mut file, meta_offset, file_len)?;
    for entry in &entries {
        let content = text(&entry.data);
        let track = |kind| super::chd_info::parse_track_line(kind, entry.index, &content);
        match &entry.tag.to_be_bytes() {
            b"CHTR" => meta.tracks.extend(track(ChdTrackTag::Chtr)),
            b"CHT2" => meta.tracks.extend(track(ChdTrackTag::Cht2)),
            b"CHGD" => meta.tracks.extend(track(ChdTrackTag::Chgd)),
            b"DVD " => meta.is_dvd = true,
            b"GDDD" => meta.hard_disk_geometry = super::chd_info::parse_hard_disk_geometry(&content),
            // The old binary CD table (v3) has no text form here: chdman reads those.
            b"CHCD" | b"CHGT" => return Err(invalid("an old binary track table")),
            _ => {}
        }
    }
    meta.tracks.sort_by_key(|track| track.track_number);

    // Before v5 the unit size is not stored: libchdr's `header_guess_unitbytes`.
    let unit = match unit_bytes {
        Some(unit) => unit,
        None => match &meta.hard_disk_geometry {
            Some(geometry) => geometry.bytes_per_sector as u64,
            None if entries.iter().any(|e| [tag(b"CHTR"), tag(b"CHT2"), tag(b"CHGD")].contains(&e.tag)) => CD_FRAME_SIZE as u64,
            None => hunk_bytes,
        },
    };
    if unit == 0 {
        return Err(invalid("unit size 0"));
    }

    meta.logical_size = logical as usize;
    meta.hunk_size = hunk_bytes as usize;
    meta.total_hunks = total_hunks as usize;
    meta.unit_size = unit as usize;
    meta.total_units = logical.div_ceil(unit) as usize;
    meta.chd_size = file_len as usize;
    if !compressed.is_empty() && logical > 0 {
        // chdman prints it to one decimal.
        meta.ratio = format!("{:.1}", 100.0 * file_len as f64 / logical as f64).parse().unwrap_or(0.0);
    }
    meta.compression = compressed;
    // An all-zero hash is one never written (an uncompressed CHD), which chdman leaves out.
    let set = |h: Option<&[u8]>| h.filter(|h| h.iter().any(|&b| b != 0)).map(hex);
    meta.sha1 = set(sha1).unwrap_or_default();
    meta.data_sha1 = set(raw_sha1).unwrap_or_default();
    meta.parent_sha1 = set(parent);
    Ok(meta)
}

/// Where the Rust reading and chdman's disagree, one line per field, for the report that
/// the checked mode writes. Empty when they agree.
pub fn differences(rust: &ChdMetadata, chdman: &ChdMetadata) -> Vec<String> {
    let mut out = Vec::new();
    let mut field = |name: &str, a: String, b: String| {
        if a != b {
            out.push(format!("{name}: rust {a}, chdman {b}"));
        }
    };
    field("version", rust.file_version.to_string(), chdman.file_version.to_string());
    field("logical size", rust.logical_size.to_string(), chdman.logical_size.to_string());
    field("hunk size", rust.hunk_size.to_string(), chdman.hunk_size.to_string());
    field("hunks", rust.total_hunks.to_string(), chdman.total_hunks.to_string());
    field("unit size", rust.unit_size.to_string(), chdman.unit_size.to_string());
    field("units", rust.total_units.to_string(), chdman.total_units.to_string());
    field("compression", format!("{:?}", rust.compression), format!("{:?}", chdman.compression));
    field("file size", rust.chd_size.to_string(), chdman.chd_size.to_string());
    field("ratio", format!("{:.1}", rust.ratio), format!("{:.1}", chdman.ratio));
    field("sha1", rust.sha1.clone(), chdman.sha1.clone());
    field("data sha1", rust.data_sha1.clone(), chdman.data_sha1.clone());
    field("parent sha1", format!("{:?}", rust.parent_sha1), format!("{:?}", chdman.parent_sha1));
    field("dvd", rust.is_dvd.to_string(), chdman.is_dvd.to_string());
    field("hard disk", format!("{:?}", rust.hard_disk_geometry), format!("{:?}", chdman.hard_disk_geometry));
    field("track count", rust.tracks.len().to_string(), chdman.tracks.len().to_string());
    for (a, b) in rust.tracks.iter().zip(&chdman.tracks) {
        field(&format!("track {}", a.track_number), format!("{a:?}"), format!("{b:?}"));
    }
    out
}

/// v3/v4's single compression number, as MAME maps it to a codec.
fn old_compression(raw: u32) -> io::Result<Vec<ChdCodecs>> {
    match raw {
        0 => Ok(Vec::new()),
        1 | 2 => Ok(vec![ChdCodecs::Zlib]),
        other => Err(invalid(format!("old compression {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A v5 header and a metadata chain built by hand: what the reader takes from each field.
    #[test]
    fn reads_a_hand_built_v5_cd() {
        let dir = std::env::temp_dir().join(format!("chd_header_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hand.chd");
        let mut h = vec![0u8; 124];
        h[..8].copy_from_slice(MAGIC);
        h[8..12].copy_from_slice(&124u32.to_be_bytes());
        h[12..16].copy_from_slice(&5u32.to_be_bytes());
        h[16..20].copy_from_slice(b"cdlz");
        h[20..24].copy_from_slice(b"cdfl");
        let logical: u64 = 19584 * 10;
        h[32..40].copy_from_slice(&logical.to_be_bytes());
        h[48..56].copy_from_slice(&124u64.to_be_bytes());
        h[56..60].copy_from_slice(&19584u32.to_be_bytes());
        h[60..64].copy_from_slice(&2448u32.to_be_bytes());
        h[64..84].copy_from_slice(&[0xaa; 20]);
        h[84..104].copy_from_slice(&[0xbb; 20]);
        let line = b"TRACK:1 TYPE:MODE2_RAW SUBTYPE:NONE FRAMES:80 PREGAP:0 PGTYPE:MODE1 PGSUB:RW POSTGAP:0\0";
        let mut entry = Vec::new();
        entry.extend_from_slice(b"CHT2");
        entry.extend_from_slice(&(0x0100_0000u32 | line.len() as u32).to_be_bytes());
        entry.extend_from_slice(&0u64.to_be_bytes());
        entry.extend_from_slice(line);
        h.extend_from_slice(&entry);
        std::fs::write(&path, &h).unwrap();

        let meta = read(&path).unwrap();
        assert_eq!(meta.file_version, 5);
        assert_eq!(meta.compression, vec![ChdCodecs::Cdlz, ChdCodecs::Cdfl]);
        assert_eq!((meta.logical_size, meta.hunk_size, meta.total_hunks, meta.unit_size, meta.total_units), (195_840, 19584, 10, 2448, 80));
        assert_eq!(meta.data_sha1, "aa".repeat(20));
        assert_eq!(meta.sha1, "bb".repeat(20));
        assert_eq!(meta.parent_sha1, None);
        assert_eq!(meta.tracks.len(), 1);
        assert_eq!((meta.tracks[0].track_number, meta.tracks[0].frames, meta.tracks[0].postgap), (1, 80, Some(0)));

        // A chain that points back at itself is refused, not followed for ever.
        let mut looped = h.clone();
        looped[124 + 8..124 + 16].copy_from_slice(&124u64.to_be_bytes());
        std::fs::write(&path, &looped).unwrap();
        assert!(read(&path).is_err());
        // Neither is a v2 file, which chdman reads.
        let mut v2 = h;
        v2[12..16].copy_from_slice(&2u32.to_be_bytes());
        std::fs::write(&path, &v2).unwrap();
        assert!(read(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
