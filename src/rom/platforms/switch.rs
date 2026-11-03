//! Nintendo Switch packages, read without keys: what the containers say in plain text.
//!
//! **NSP / NSZ** (switchbrew.org/wiki/NCA, "PFS0"): a PFS0 partition, little-endian.
//!
//! | offset | field |
//! |---|---|
//! | 0x00 | `PFS0` |
//! | 0x04 | u32 entry count |
//! | 0x08 | u32 string table size |
//! | 0x10 | entries of 0x18 bytes: u64 data offset, u64 size, u32 name offset, u32 reserved |
//! | then | the string table, then the data (offsets count from its start) |
//!
//! The members are NCAs named by the first 16 bytes of their SHA-256 (`<32 hex>.nca`, `.ncz`
//! in an NSZ), a `.cnmt.nca` with the content meta, and for titlekey-encrypted content a
//! ticket and a certificate named by the rights id (`<title id><key generation>.tik`). The NCA
//! headers are encrypted with the console's header key, so without keys the title id comes
//! from the ticket's name when there is one, else from the `[0100…]` a dump's file name
//! carries. A title id's last three hex digits say what it is: `000` the game, `800` its
//! update, anything else an add-on.
//!
//! **XCI / XCZ** (switchbrew.org/wiki/XCI): `HEAD` at 0x100, or at 0x1100 when the card's
//! key area is dumped in front. Its title id is in the encrypted part of the header.
//!
//! No-Intro's Switch DATs hash the cartridge XCI and, for digital titles, each NCA on its own:
//! a whole NSP is hashed by no one, which is why `api::scan` hashes the NCAs inside it.

use std::path::Path;

use super::super::detect::disc_fs::read_exact_at;
use super::super::detect::reason::Evidence;
use super::super::hash::{HashDigest, HashFlags, HashReport};
use super::sony::ContentKind;
use super::NonDiscHeaderMetadata;

pub const PFS0_MAGIC: &[u8; 4] = b"PFS0";
const XCI_MAGIC: &[u8; 4] = b"HEAD";
/// More entries than any real package has; a count above it is not a PFS0.
const MAX_ENTRIES: u32 = 4096;

/// One file in a PFS0, its offset counted from the start of the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pfs0Entry {
    pub name: String,
    pub offset: u64,
    pub size: u64,
}

/// The files of the PFS0 at the start of `path`, or `None` when it is not one (or an entry
/// runs past the end of the file).
pub fn pfs0_entries(path: &Path) -> Option<Vec<Pfs0Entry>> {
    let len = std::fs::metadata(path).ok()?.len();
    let head = read_exact_at(path, 0, 0x10.min(len as usize)).ok()?;
    if head.get(..4)? != PFS0_MAGIC {
        return None;
    }
    let count = u32::from_le_bytes(head[4..8].try_into().ok()?);
    let table = u32::from_le_bytes(head[8..12].try_into().ok()?) as u64;
    if count == 0 || count > MAX_ENTRIES || table > 0x10_0000 {
        return None;
    }
    let header_len = 0x10 + count as u64 * 0x18 + table;
    if header_len > len {
        return None;
    }
    let header = read_exact_at(path, 0, header_len as usize).ok()?;
    let names = &header[0x10 + count as usize * 0x18..];
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        let e = &header[0x10 + i * 0x18..0x10 + (i + 1) * 0x18];
        let offset = u64::from_le_bytes(e[0..8].try_into().ok()?);
        let size = u64::from_le_bytes(e[8..16].try_into().ok()?);
        let name_at = u32::from_le_bytes(e[16..20].try_into().ok()?) as usize;
        let name = names.get(name_at..)?;
        let name = String::from_utf8(name[..name.iter().position(|&b| b == 0).unwrap_or(name.len())].to_vec()).ok()?;
        let offset = header_len.checked_add(offset)?;
        if name.is_empty() || offset.checked_add(size)? > len {
            return None;
        }
        out.push(Pfs0Entry { name, offset, size });
    }
    Some(out)
}

/// Whether a member is an NCA (or an NSZ's compressed `.ncz`).
pub fn is_nca(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".nca") || lower.ends_with(".ncz")
}

/// A Switch title id: 16 hex digits starting `01`, upper case.
fn title_id(s: &str) -> Option<String> {
    (s.len() == 16 && s.starts_with("01") && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| s.to_ascii_uppercase())
}

/// The title id a ticket's name carries (`<title id><key generation 16 hex>.tik`).
fn ticket_title_id(entries: &[Pfs0Entry]) -> Option<String> {
    entries.iter().find_map(|e| {
        let stem = e.name.strip_suffix(".tik")?;
        (stem.len() == 32).then(|| title_id(&stem[..16])).flatten()
    })
}

/// The title id in a file name's brackets (`[0100ABCD12345000]`), the way dumping tools name
/// their output.
pub fn title_id_in_name(name: &str) -> Option<String> {
    name.split('[').skip(1).find_map(|part| title_id(part.split(']').next()?))
}

/// What a title id is: its last three hex digits are `000` for the game, `800` for its update,
/// anything else for an add-on.
pub fn kind_of_title_id(id: &str) -> ContentKind {
    match id.get(13..) {
        Some("000") => ContentKind::Game,
        Some("800") => ContentKind::Update,
        _ => ContentKind::Dlc,
    }
}

/// The title id of an NSP / NSZ / XCI / XCZ and where it came from: the ticket inside (`true`)
/// or the file name (`false`).
pub fn title_id_of(path: &Path) -> Option<(String, bool)> {
    if let Some(id) = pfs0_entries(path).as_deref().and_then(ticket_title_id) {
        return Some((id, true));
    }
    Some((title_id_in_name(path.file_name()?.to_str()?)?, false))
}

/// Hashes each NCA in an NSP on its own, the way No-Intro's digital DATs list them. Only an
/// `.nsp`: an NSZ's `.ncz` members are compressed and match nothing.
pub fn hash_ncas(path: &Path, flags: HashFlags) -> std::io::Result<Vec<(Pfs0Entry, HashReport)>> {
    use std::io::{Read, Seek, SeekFrom};
    let Some(entries) = pfs0_entries(path) else { return Ok(Vec::new()) };
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 4 << 20];
    let mut out = Vec::new();
    for entry in entries.into_iter().filter(|e| e.name.to_ascii_lowercase().ends_with(".nca")) {
        file.seek(SeekFrom::Start(entry.offset))?;
        let digest = HashDigest::new(flags);
        let mut left = entry.size;
        while left > 0 {
            let n = left.min(buf.len() as u64) as usize;
            file.read_exact(&mut buf[..n])?;
            digest.update(&buf[..n]).map_err(|e| std::io::Error::other(format!("{e:?}")))?;
            left -= n as u64;
        }
        out.push((entry, digest.finish()));
    }
    Ok(out)
}

/// Reads a Switch package by its content: a PFS0 (NSP, NSZ) or a cartridge image (XCI, XCZ).
pub fn read_switch_metadata(path: &Path) -> Option<NonDiscHeaderMetadata> {
    if let Some(entries) = pfs0_entries(path) {
        let ncas = entries.iter().filter(|e| is_nca(&e.name)).count();
        if ncas == 0 {
            return None;
        }
        let meta = entries.iter().any(|e| e.name.to_ascii_lowercase().ends_with(".cnmt.nca") || e.name.to_ascii_lowercase().ends_with(".cnmt.ncz"));
        let mut evidence = vec![
            Evidence::Magic { offset: 0, bytes: PFS0_MAGIC.to_vec() },
            Evidence::Note(format!("PFS0 with {} file(s), {ncas} of them NCAs{}", entries.len(), if meta { ", one the content meta" } else { "" })),
        ];
        let serial = ticket_title_id(&entries);
        match &serial {
            Some(id) => evidence.push(Evidence::Note(format!("ticket for title {id}: {}", kind_of_title_id(id).label()))),
            None => evidence.push(Evidence::Note("no ticket: the title id is in the encrypted NCAs".to_string())),
        }
        return Some(NonDiscHeaderMetadata { serial, evidence, ..Default::default() });
    }
    for at in [0x100u64, 0x1100] {
        if read_exact_at(path, at, 4).ok().as_deref() == Some(XCI_MAGIC.as_slice()) {
            let note = if at == 0x100 { "cartridge image" } else { "cartridge image with its key area in front" };
            return Some(NonDiscHeaderMetadata { evidence: vec![Evidence::Magic { offset: at as usize, bytes: XCI_MAGIC.to_vec() }, Evidence::Note(note.to_string())], ..Default::default() });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_ids_and_what_they_are() {
        assert_eq!(title_id_in_name("TEST TITLE [0100ABCD12345000][v0].nsp").as_deref(), Some("0100ABCD12345000"));
        assert_eq!(title_id_in_name("TEST TITLE [v0].nsp"), None);
        assert_eq!(title_id_in_name("TEST [0004000012345000].cia"), None, "a 3DS id is not a Switch one");
        assert_eq!(kind_of_title_id("0100ABCD12345000"), ContentKind::Game);
        assert_eq!(kind_of_title_id("0100ABCD12345800"), ContentKind::Update);
        assert_eq!(kind_of_title_id("0100ABCD12346001"), ContentKind::Dlc);
    }
}
