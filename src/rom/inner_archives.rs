//! Archives inside a game's filesystem that hold its sound: the members vgmstream can play,
//! for `rom_audio` to index and open.
//!
//! - AFS (Dreamcast, Naomi, PS2): offset table, members recognised by their first bytes.
//! - SDAT (Nintendo DS): the sound archive; its STRM streams are taken, named from its
//!   symbol table. Its sequences (SSEQ) are sequenced music and are not played.
//! - RARC and U8 (GameCube, Wii), either stored or Yaz0-compressed (`.szs`).
//! - CPK (CRI, many PS2 / PSP / later games): its table of contents, members stored or
//!   CRILAYLA-compressed.
//! - PSMF (PSP .pmf movies): the ATRAC3plus sound is taken out of the MPEG program stream
//!   and given to vgmstream as an .at3 (RIFF) in memory.
//!
//! A member is addressed as `<archive path>#<index>.<ext>`: the index is the archive's own
//! (AFS / SDAT file id, CPK id, the RARC / U8 entry), the extension the one vgmstream needs.

use std::sync::Arc;

use super::vgmstream::{self, VgmSource};

/// Whole archives read into memory (Yaz0, CRILAYLA, the tables) stop at this size.
const MAX_READ: u64 = 512 << 20;
/// Members smaller than this are not sounds.
const MIN_MEMBER: u64 = 0x40;
/// More than any real archive holds; a damaged table stops here.
const MAX_MEMBERS: usize = 100_000;

/// Where a member's bytes are.
#[derive(Clone)]
pub enum MemberData {
    /// A run of the archive file itself.
    Slice { start: u64, size: u64 },
    /// Decompressed bytes.
    Bytes(Arc<Vec<u8>>),
}

#[derive(Clone)]
pub struct Member {
    pub index: u32,
    /// The extension vgmstream knows the member by.
    pub ext: String,
    /// The member's own name, when the archive has one.
    pub name: Option<String>,
    pub data: MemberData,
}

impl Member {
    pub fn size(&self) -> u64 {
        match &self.data {
            MemberData::Slice { size, .. } => *size,
            MemberData::Bytes(b) => b.len() as u64,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Afs,
    Sdat,
    /// RARC or U8, maybe Yaz0-compressed: told apart by the bytes.
    Nintendo,
    Cpk,
    /// A PSP movie: one member, its sound.
    Psmf,
}

/// The archive kind `path`'s extension names, if it is one these are read for.
pub fn kind_of(path: &str) -> Option<Kind> {
    let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
    match ext.as_str() {
        "afs" => Some(Kind::Afs),
        "sdat" => Some(Kind::Sdat),
        "arc" | "szs" | "rarc" | "u8" | "carc" => Some(Kind::Nintendo),
        "cpk" => Some(Kind::Cpk),
        "pmf" => Some(Kind::Psmf),
        _ => None,
    }
}

/// `X.AFS#12.spsd` → (`X.AFS`, 12).
pub fn split(path: &str) -> Option<(&str, u32)> {
    let (base, member) = path.rsplit_once('#')?;
    kind_of(base)?;
    Some((base, member.split('.').next()?.parse().ok()?))
}

/// The path a member is indexed and reopened by.
pub fn member_path(archive: &str, m: &Member) -> String {
    format!("{archive}#{}.{}", m.index, m.ext)
}

/// The playable members of the archive `src`, read as `kind`.
pub fn members(src: &mut dyn VgmSource, kind: Kind) -> Vec<Member> {
    let found = match kind {
        Kind::Afs => afs(src),
        Kind::Sdat => sdat(src),
        Kind::Nintendo => read_all(src).map(|b| nintendo(Arc::new(b))).unwrap_or_default(),
        Kind::Cpk => cpk(src),
        Kind::Psmf => read_all(src).and_then(|b| psmf_audio(&b)).map(|riff| vec![Member { index: 0, ext: "at3".into(), name: None, data: MemberData::Bytes(Arc::new(riff)) }]).unwrap_or_default(),
    };
    found.into_iter().filter(|m| m.size() >= MIN_MEMBER).take(MAX_MEMBERS).collect()
}

/// A source over one member, given a fresh reader of the archive (for a slice).
pub fn open_member(archive: Box<dyn VgmSource>, archive_name: &str, member_name: &str, m: &Member) -> Box<dyn VgmSource> {
    match &m.data {
        MemberData::Slice { start, size } => Box::new(Slice { inner: archive, start: *start, size: *size, name: archive_name.to_string(), member: member_name.to_string() }),
        MemberData::Bytes(bytes) => Box::new(vgmstream::MemorySource { name: member_name.to_string(), bytes: bytes.clone(), siblings: Default::default() }),
    }
}

/// `size` bytes at `start` of another source.
struct Slice {
    inner: Box<dyn VgmSource>,
    start: u64,
    size: u64,
    /// The archive's own name, to reopen it by.
    name: String,
    /// The name vgmstream knows the member by, and reopens it by.
    member: String,
}

impl VgmSource for Slice {
    fn read_at(&mut self, offset: u64, dst: &mut [u8]) -> usize {
        if offset >= self.size {
            return 0;
        }
        let n = dst.len().min((self.size - offset) as usize);
        self.inner.read_at(self.start + offset, &mut dst[..n])
    }

    fn size(&self) -> u64 {
        self.size
    }

    fn open(&self, name: &str) -> Option<Box<dyn VgmSource>> {
        if !vgmstream::file_name_of(name).eq_ignore_ascii_case(vgmstream::file_name_of(&self.member)) {
            return None;
        }
        let base = self.inner.open(&self.name)?;
        Some(Box::new(Slice { inner: base, start: self.start, size: self.size, name: self.name.clone(), member: self.member.clone() }))
    }
}

// --- reading helpers -------------------------------------------------------------------

fn read(src: &mut dyn VgmSource, at: u64, len: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; len];
    let mut done = 0;
    while done < len {
        let n = src.read_at(at + done as u64, &mut buf[done..]);
        if n == 0 {
            return None;
        }
        done += n;
    }
    Some(buf)
}

fn read_all(src: &mut dyn VgmSource) -> Option<Vec<u8>> {
    let size = src.size();
    if size > MAX_READ {
        return None;
    }
    read(src, 0, size as usize)
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn be64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

fn cstr(b: &[u8], at: usize) -> Option<String> {
    let rest = b.get(at..)?;
    let end = rest.iter().position(|&c| c == 0).unwrap_or(rest.len());
    Some(String::from_utf8_lossy(&rest[..end]).into_owned())
}

/// The extension a member's first bytes call for, for archives without names.
fn ext_by_magic(head: &[u8]) -> Option<&'static str> {
    match head.get(..4)? {
        b"SPSD" => Some("spsd"),
        b"RIFF" => Some("wav"),
        b"STRM" => Some("strm"),
        b"CSTM" => Some("bcstm"),
        b"FSTM" => Some("bfstm"),
        b"RSTM" => Some("brstm"),
        b"CWAV" => Some("bcwav"),
        b"VAGp" => Some("vag"),
        b"HCA\0" => Some("hca"),
        [0xC8, 0xC3, 0xC1, 0x00] => Some("hca"),
        b"AFS2" => Some("awb"),
        [0x80, 0x00, ..] => Some("adx"),
        _ => None,
    }
}

/// The extension for a named member: its own when vgmstream knows it, else by its bytes.
fn ext_for(name: &str, head: &[u8]) -> Option<String> {
    if vgmstream::known_extension(name) {
        if let Some((_, ext)) = name.rsplit_once('.') {
            return Some(ext.to_ascii_lowercase());
        }
    }
    ext_by_magic(head).map(str::to_string)
}

// --- AFS -------------------------------------------------------------------------------

fn afs(src: &mut dyn VgmSource) -> Vec<Member> {
    let Some(head) = read(src, 0, 8) else { return Vec::new() };
    if &head[..4] != b"AFS\0" {
        return Vec::new();
    }
    let count = le32(&head, 4).unwrap_or(0).min(65_536) as usize;
    let Some(table) = read(src, 8, count * 8) else { return Vec::new() };
    let size = src.size();
    let mut out = Vec::new();
    for (i, e) in table.chunks_exact(8).enumerate() {
        let (start, len) = (u64::from(le32(e, 0).unwrap_or(0)), u64::from(le32(e, 4).unwrap_or(0)));
        if len < MIN_MEMBER || start.checked_add(len).is_none_or(|end| end > size) {
            continue;
        }
        let Some(magic) = read(src, start, 4) else { continue };
        if let Some(ext) = ext_by_magic(&magic) {
            out.push(Member { index: i as u32, ext: ext.into(), name: None, data: MemberData::Slice { start, size: len } });
        }
    }
    out
}

// --- Nintendo DS SDAT ------------------------------------------------------------------

/// The STRM streams of a DS sound archive, by their file ids, named from the symbol table.
fn sdat(src: &mut dyn VgmSource) -> Vec<Member> {
    let Some(head) = read(src, 0, 0x40) else { return Vec::new() };
    if &head[..4] != b"SDAT" {
        return Vec::new();
    }
    let size = src.size();
    let block = |src: &mut dyn VgmSource, at: usize| -> Option<Vec<u8>> {
        let (off, len) = (u64::from(le32(&head, at)?), le32(&head, at + 4)? as usize);
        if off == 0 || !(8..=32 << 20).contains(&len) || off + len as u64 > size {
            return None;
        }
        read(src, off, len)
    };
    let symb = block(src, 0x10);
    let info = block(src, 0x18);
    let Some(fat) = block(src, 0x20) else { return Vec::new() };
    // INFO and SYMB both hold 8 record offsets from 0x08 (the STRM record is the 8th);
    // a record is a count, then one offset per entry, all from the block's start.
    let record = |b: &[u8], kind: usize| -> Vec<u32> {
        let Some(at) = le32(b, 0x08 + kind * 4).map(|v| v as usize) else { return Vec::new() };
        let count = le32(b, at).unwrap_or(0).min(4096) as usize;
        (0..count).map(|i| le32(b, at + 4 + i * 4).unwrap_or(0)).collect()
    };
    const STRM: usize = 7;
    let names: Vec<Option<String>> = symb.as_deref().map(|b| record(b, STRM).iter().map(|&o| if o == 0 { None } else { cstr(b, o as usize) }).collect()).unwrap_or_default();
    let ids: Vec<Option<u32>> = info.as_deref().map(|b| record(b, STRM).iter().map(|&o| if o == 0 { None } else { le16(b, o as usize).map(u32::from) }).collect()).unwrap_or_default();
    let count = le32(&fat, 0x08).unwrap_or(0) as usize;
    let entry = |id: u32| -> Option<(u64, u64)> {
        let at = 0x0C + id as usize * 16;
        let (start, len) = (u64::from(le32(&fat, at)?), u64::from(le32(&fat, at + 4)?));
        (start + len <= size && (id as usize) < count).then_some((start, len))
    };
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (i, id) in ids.iter().enumerate() {
        let Some(id) = *id else { continue };
        let Some((start, len)) = entry(id) else { continue };
        if read(src, start, 4).as_deref() != Some(b"STRM") || !seen.insert(id) {
            continue;
        }
        let name = names.get(i).cloned().flatten();
        out.push(Member { index: id, ext: "strm".into(), name, data: MemberData::Slice { start, size: len } });
    }
    // No INFO to go by: any FAT entry that is a stream.
    if ids.is_empty() {
        for id in 0..count.min(MAX_MEMBERS) as u32 {
            let Some((start, len)) = entry(id) else { continue };
            if read(src, start, 4).as_deref() == Some(b"STRM") {
                out.push(Member { index: id, ext: "strm".into(), name: None, data: MemberData::Slice { start, size: len } });
            }
        }
    }
    out
}

// --- Yaz0, RARC and U8 -----------------------------------------------------------------

/// Nintendo's Yaz0: a run-length / back-reference scheme, one flag bit per item.
pub fn yaz0(src: &[u8]) -> Option<Vec<u8>> {
    if src.get(..4)? != b"Yaz0" {
        return None;
    }
    let size = be32(src, 4)? as usize;
    if size as u64 > MAX_READ {
        return None;
    }
    let mut out = Vec::with_capacity(size);
    let mut at = 16;
    while out.len() < size {
        let flags = *src.get(at)?;
        at += 1;
        for bit in (0..8).rev() {
            if out.len() >= size {
                break;
            }
            if flags & (1 << bit) != 0 {
                out.push(*src.get(at)?);
                at += 1;
            } else {
                let (b1, b2) = (usize::from(*src.get(at)?), usize::from(*src.get(at + 1)?));
                at += 2;
                let back = ((b1 & 0x0F) << 8 | b2) + 1;
                let len = if b1 >> 4 == 0 {
                    let n = usize::from(*src.get(at)?) + 0x12;
                    at += 1;
                    n
                } else {
                    (b1 >> 4) + 2
                };
                let from = out.len().checked_sub(back)?;
                for k in 0..len {
                    let byte = out[from + k];
                    out.push(byte);
                }
            }
        }
    }
    out.truncate(size);
    Some(out)
}

fn nintendo(bytes: Arc<Vec<u8>>) -> Vec<Member> {
    let bytes = match yaz0(&bytes) {
        Some(plain) => Arc::new(plain),
        None => bytes,
    };
    let files = if bytes.get(..4) == Some(b"RARC") {
        rarc(&bytes)
    } else if be32(&bytes, 0) == Some(0x55AA_382D) {
        u8_archive(&bytes)
    } else {
        Vec::new()
    };
    let mut out = Vec::new();
    for (i, (name, start, len)) in files.into_iter().enumerate() {
        let Some(data) = bytes.get(start..start + len) else { continue };
        // A member may itself be Yaz0 (a stream inside an .szs is rare, but a .yaz0 bank is not).
        let data = yaz0(data).unwrap_or_else(|| data.to_vec());
        let Some(ext) = ext_for(&name, &data) else { continue };
        out.push(Member { index: i as u32, ext, name: Some(name), data: MemberData::Bytes(Arc::new(data)) });
    }
    out
}

/// RARC: nodes (directories) of entries; an entry is a file or a link to a node.
fn rarc(b: &[u8]) -> Vec<(String, usize, usize)> {
    let info = 0x20;
    let (Some(nodes_n), Some(nodes_at), Some(entries_at), Some(strings_at), Some(data_at)) =
        (be32(b, info), be32(b, info + 4), be32(b, info + 0x0C), be32(b, info + 0x14), be32(b, 0x0C))
    else {
        return Vec::new();
    };
    let (nodes_at, entries_at, strings_at, data_at) = (info + nodes_at as usize, info + entries_at as usize, info + strings_at as usize, info + data_at as usize);
    let mut out = Vec::new();
    let mut stack = vec![(0usize, String::new(), 0usize)];
    while let Some((node, prefix, depth)) = stack.pop() {
        if node >= nodes_n as usize || depth > 32 || out.len() >= MAX_MEMBERS {
            continue;
        }
        let n = nodes_at + node * 16;
        let (Some(count), Some(first)) = (be16(b, n + 0x0A), be32(b, n + 0x0C)) else { continue };
        for e in 0..usize::from(count) {
            let at = entries_at + (first as usize + e) * 20;
            let (Some(flags), Some(name_at), Some(off), Some(size)) = (b.get(at + 4).copied(), be16(b, at + 6), be32(b, at + 8), be32(b, at + 12)) else { break };
            let name = cstr(b, strings_at + usize::from(name_at)).unwrap_or_default();
            if name == "." || name == ".." {
                continue;
            }
            let path = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
            if flags & 0x02 != 0 {
                stack.push((off as usize, path, depth + 1));
            } else {
                out.push((path, data_at + off as usize, size as usize));
            }
        }
    }
    out
}

/// U8: one flat node list; a directory's size is the index after its last child.
fn u8_archive(b: &[u8]) -> Vec<(String, usize, usize)> {
    let Some(root) = be32(b, 4).map(|v| v as usize) else { return Vec::new() };
    let Some(total) = be32(b, root + 8).map(|v| v as usize) else { return Vec::new() };
    if total == 0 || total > MAX_MEMBERS {
        return Vec::new();
    }
    let strings = root + total * 12;
    let mut out = Vec::new();
    // (end index, path) of the directories we are inside.
    let mut dirs: Vec<(usize, String)> = vec![(total, String::new())];
    for i in 1..total {
        while dirs.len() > 1 && dirs.last().is_some_and(|(end, _)| i >= *end) {
            dirs.pop();
        }
        let at = root + i * 12;
        let (Some(word), Some(off), Some(size)) = (be32(b, at), be32(b, at + 4), be32(b, at + 8)) else { break };
        let name = cstr(b, strings + (word & 0x00FF_FFFF) as usize).unwrap_or_default();
        let prefix = &dirs.last().map(|d| d.1.clone()).unwrap_or_default();
        let path = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        if word >> 24 == 1 {
            dirs.push((size as usize, path));
        } else {
            out.push((path, off as usize, size as usize));
        }
    }
    out
}

// --- CPK -------------------------------------------------------------------------------

/// One @UTF table: rows of named columns.
struct Utf {
    rows: Vec<Vec<(String, Value)>>,
}

#[derive(Clone, Debug)]
enum Value {
    Int(u64),
    Str(String),
    Data(u64, u64),
    None,
}

impl Utf {
    fn get(&self, row: usize, col: &str) -> Option<&Value> {
        self.rows.get(row)?.iter().find(|(n, _)| n == col).map(|(_, v)| v)
    }
    fn int(&self, row: usize, col: &str) -> Option<u64> {
        match self.get(row, col)? {
            Value::Int(v) => Some(*v),
            _ => None,
        }
    }
    fn str(&self, row: usize, col: &str) -> Option<String> {
        match self.get(row, col)? {
            Value::Str(s) => Some(s.clone()),
            _ => None,
        }
    }
}

/// CRI's @UTF table, as stored (CPK tables may be XOR-scrambled; those are not read).
fn utf(b: &[u8]) -> Option<Utf> {
    if b.get(..4)? != b"@UTF" {
        return None;
    }
    let base = 8;
    let rows_at = base + be16(b, 0x0A)? as usize;
    let strings_at = base + be32(b, 0x0C)? as usize;
    let data_at = base + be32(b, 0x10)? as usize;
    let cols = be16(b, 0x18)? as usize;
    let row_len = be16(b, 0x1A)? as usize;
    let n_rows = (be32(b, 0x1C)? as usize).min(MAX_MEMBERS);
    let s = |off: u32| cstr(b, strings_at + off as usize).unwrap_or_default();
    let width = |ty: u8| match ty {
        0 | 1 => 1,
        2 | 3 => 2,
        4 | 5 | 8 | 0xA => 4,
        6 | 7 | 0xB => 8,
        _ => 0,
    };
    let value_at = |at: usize, ty: u8| -> Option<Value> {
        Some(match ty {
            0 | 1 => Value::Int(u64::from(*b.get(at)?)),
            2 | 3 => Value::Int(u64::from(be16(b, at)?)),
            4 | 5 | 8 => Value::Int(u64::from(be32(b, at)?)),
            6 | 7 => Value::Int(be64(b, at)?),
            0xA => Value::Str(s(be32(b, at)?)),
            0xB => Value::Data(u64::from(be32(b, at)?) + data_at as u64, u64::from(be32(b, at + 4)?)),
            _ => Value::None,
        })
    };
    // A column: flags (storage << 4 | type) and its name. Storage 1 has no value, 3 one
    // value kept here for every row, 5 a value in each row.
    let mut columns: Vec<(String, u8, Option<Value>)> = Vec::new();
    let mut at = 0x20;
    for _ in 0..cols {
        let flags = *b.get(at)?;
        let name = s(be32(b, at + 1)?);
        at += 5;
        let (storage, ty) = (flags >> 4, flags & 0x0F);
        match storage {
            3 => {
                let v = value_at(at, ty)?;
                at += width(ty);
                columns.push((name, ty, Some(v)));
            }
            5 => columns.push((name, ty, None)),
            _ => columns.push((name, ty, Some(Value::None))),
        }
    }
    let mut rows = Vec::with_capacity(n_rows);
    for r in 0..n_rows {
        let mut at = rows_at + r * row_len;
        let mut row = Vec::with_capacity(columns.len());
        for (name, ty, constant) in &columns {
            let v = match constant {
                Some(v) => v.clone(),
                None => {
                    let v = value_at(at, *ty).unwrap_or(Value::None);
                    at += width(*ty);
                    v
                }
            };
            row.push((name.clone(), v));
        }
        rows.push(row);
    }
    Some(Utf { rows })
}

/// CRILAYLA: compressed backwards, with the first 0x100 bytes kept as they are after it.
pub fn crilayla(src: &[u8]) -> Option<Vec<u8>> {
    if src.get(..8)? != b"CRILAYLA" {
        return None;
    }
    let size = le32(src, 8)? as usize;
    let header_at = le32(src, 12)? as usize + 0x10;
    if size as u64 > MAX_READ {
        return None;
    }
    let mut out = vec![0u8; size + 0x100];
    out[..0x100].copy_from_slice(src.get(header_at..header_at + 0x100)?);
    // Bits are read from the end of the compressed data backwards.
    let mut pos = header_at - 1;
    let mut bits_left = 0u32;
    let mut cur = 0u8;
    let mut take = |n: u32, pos: &mut usize| -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            if bits_left == 0 {
                cur = *src.get(*pos)?;
                *pos = pos.checked_sub(1).unwrap_or(0);
                bits_left = 8;
            }
            v = (v << 1) | u32::from((cur >> 7) & 1);
            cur <<= 1;
            bits_left -= 1;
        }
        Some(v)
    };
    let end = 0x100;
    let mut w = out.len();
    while w > end {
        if take(1, &mut pos)? == 1 {
            let back = take(13, &mut pos)? as usize + 3;
            let mut len = 3usize;
            let widths = [2u32, 3, 5, 8];
            let mut i = 0;
            loop {
                let w_bits = widths[i.min(3)];
                let v = take(w_bits, &mut pos)? as usize;
                len += v;
                if v != (1 << w_bits) - 1 {
                    break;
                }
                if i < 3 {
                    i += 1;
                }
            }
            for _ in 0..len {
                if w <= end {
                    break;
                }
                w -= 1;
                out[w] = *out.get(w + back)?;
            }
        } else {
            w -= 1;
            out[w] = take(8, &mut pos)? as u8;
        }
    }
    Some(out)
}

/// A CPK's members from its TOC (by name) or ITOC (by id).
fn cpk(src: &mut dyn VgmSource) -> Vec<Member> {
    let Some(head) = read(src, 0, 0x800) else { return Vec::new() };
    if &head[..4] != b"CPK " {
        return Vec::new();
    }
    let table_at = |at: usize| -> Option<usize> { Some(le32(&head, at)? as usize) };
    let header_len = table_at(0x08).unwrap_or(0).min(head.len().saturating_sub(0x10));
    let Some(header) = utf(&head[0x10..0x10 + header_len]) else { return Vec::new() };
    let content = header.int(0, "ContentOffset").unwrap_or(0);
    let size = src.size();
    let table = |src: &mut dyn VgmSource, off: u64| -> Option<Utf> {
        if off == 0 || off + 0x10 > size {
            return None;
        }
        let h = read(src, off, 0x10)?;
        let len = le32(&h, 8)? as usize;
        if len > 64 << 20 {
            return None;
        }
        utf(&read(src, off + 0x10, len)?)
    };
    let mut out = Vec::new();
    let mut add = |src: &mut dyn VgmSource, index: u32, name: Option<String>, start: u64, stored: u64, extracted: u64| {
        if stored < MIN_MEMBER || start + stored > size || out.len() >= MAX_MEMBERS {
            return;
        }
        let Some(head) = read(src, start, 16) else { return };
        let data = if stored < extracted && &head[..8] == b"CRILAYLA" {
            let Some(packed) = read(src, start, stored as usize) else { return };
            let Some(plain) = crilayla(&packed) else { return };
            MemberData::Bytes(Arc::new(plain))
        } else {
            MemberData::Slice { start, size: stored }
        };
        let first = match &data {
            MemberData::Bytes(b) => b[..16.min(b.len())].to_vec(),
            MemberData::Slice { .. } => head,
        };
        let ext = match &name {
            Some(n) => ext_for(n, &first),
            None => ext_by_magic(&first).map(str::to_string),
        };
        if let Some(ext) = ext {
            out.push(Member { index, ext, name, data });
        }
    };
    // TOC offsets count from the TOC itself, or from the content when that comes first.
    let toc_at = header.int(0, "TocOffset").unwrap_or(0);
    let toc_base = if content > 0 { toc_at.min(content) } else { toc_at };
    if let Some(toc) = table(src, toc_at) {
        for row in 0..toc.rows.len() {
            let dir = toc.str(row, "DirName").unwrap_or_default();
            let file = toc.str(row, "FileName").unwrap_or_default();
            let name = if dir.is_empty() { file } else { format!("{dir}/{file}") };
            let start = toc.int(row, "FileOffset").unwrap_or(0) + toc_base;
            let id = toc.int(row, "ID").unwrap_or(row as u64) as u32;
            add(src, id, Some(name), start, toc.int(row, "FileSize").unwrap_or(0), toc.int(row, "ExtractSize").unwrap_or(0));
        }
    } else if let Some(itoc) = header.int(0, "ItocOffset").and_then(|o| table(src, o)) {
        // Files by id, laid out in id order from the content offset, each aligned.
        let align = header.int(0, "Align").unwrap_or(0x800).max(1);
        let mut entries: Vec<(u32, u64, u64)> = Vec::new();
        for key in ["DataL", "DataH"] {
            if let Some(Value::Data(at, len)) = itoc.get(0, key).cloned() {
                if let Some(sub) = read(src, header.int(0, "ItocOffset").unwrap_or(0) + 0x10 + at, len as usize).and_then(|b| utf(&b)) {
                    for r in 0..sub.rows.len() {
                        let id = sub.int(r, "ID").unwrap_or(0) as u32;
                        entries.push((id, sub.int(r, "FileSize").unwrap_or(0), sub.int(r, "ExtractSize").unwrap_or(0)));
                    }
                }
            }
        }
        entries.sort_by_key(|e| e.0);
        let mut at = content;
        for (id, stored, extracted) in entries {
            add(src, id, None, at, stored, extracted);
            at = (at + stored).div_ceil(align) * align;
        }
    }
    out
}

// --- PSMF -------------------------------------------------------------------------------

/// Samples in one ATRAC3plus frame.
const AT3P_FRAME_SAMPLES: u32 = 2048;

/// The ATRAC3plus sound of a PSP movie, as a RIFF .at3. The program stream carries it in
/// private stream 1 (0xBD), sub-stream 0, after a 4-byte prefix; the frames in it each have
/// an 8-byte header (0x0FD0, two bytes of codec setup, four zero bytes) that RIFF leaves out.
pub fn psmf_audio(b: &[u8]) -> Option<Vec<u8>> {
    if b.get(..4)? != b"PSMF" {
        return None;
    }
    let start = be32(b, 0x08)? as usize;
    let end = b.len().min(start.saturating_add(be32(b, 0x0C)? as usize));
    let mut es = Vec::new();
    let mut at = start;
    while at + 6 <= end {
        if b[at..at + 3] != [0, 0, 1] {
            at += 1;
            continue;
        }
        let id = b[at + 3];
        match id {
            // Pack header: 14 bytes and its stuffing.
            0xBA => at += 14 + usize::from(*b.get(at + 13)? & 7),
            0xB9 => at += 4,
            _ if id >= 0xBB => {
                let len = usize::from(be16(b, at + 4)?);
                let next = at + 6 + len;
                if id == 0xBD && len > 3 {
                    let payload = at + 9 + usize::from(*b.get(at + 8)?);
                    if payload + 4 <= next.min(end) && b[payload] == 0 {
                        es.extend_from_slice(&b[payload + 4..next.min(end)]);
                    }
                }
                at = next;
            }
            _ => at += 1,
        }
    }
    at3p_riff(&es)
}

/// RIFF .at3 from ATRAC3plus frames that each carry their 8-byte header.
fn at3p_riff(es: &[u8]) -> Option<Vec<u8>> {
    let first = es.windows(2).position(|w| w == [0x0F, 0xD0])?;
    let setup = be16(es, first + 2)?;
    let frame = (usize::from(setup & 0x3FF) + 1) * 8;
    let rate = [32_000u32, 44_100, 48_000, 88_200, 96_000].get(usize::from(setup >> 13)).copied()?;
    let channels = [0u16, 1, 2, 3, 4, 6, 7, 8][usize::from((setup >> 10) & 7)];
    if channels == 0 {
        return None;
    }
    let mut data = Vec::new();
    let mut at = first;
    while at + 8 + frame <= es.len() {
        if es[at..at + 2] == [0x0F, 0xD0] && be16(es, at + 2) == Some(setup) {
            data.extend_from_slice(&es[at + 8..at + 8 + frame]);
            at += 8 + frame;
        } else {
            // Lost sync: look for the next header with the same setup.
            let skip = es[at + 1..].windows(4).position(|w| w[..2] == [0x0F, 0xD0] && u16::from_be_bytes([w[2], w[3]]) == setup)?;
            at += 1 + skip;
        }
    }
    if data.is_empty() {
        return None;
    }
    let frames = (data.len() / frame) as u32;
    let mut fmt = Vec::with_capacity(0x34);
    fmt.extend_from_slice(&0xFFFEu16.to_le_bytes());
    fmt.extend_from_slice(&channels.to_le_bytes());
    fmt.extend_from_slice(&rate.to_le_bytes());
    fmt.extend_from_slice(&((frame as u64 * u64::from(rate) / u64::from(AT3P_FRAME_SAMPLES)) as u32).to_le_bytes());
    fmt.extend_from_slice(&(frame as u16).to_le_bytes());
    fmt.extend_from_slice(&0u16.to_le_bytes());
    fmt.extend_from_slice(&0x22u16.to_le_bytes());
    fmt.extend_from_slice(&(AT3P_FRAME_SAMPLES as u16).to_le_bytes());
    let mask: u32 = match channels {
        1 => 0x4,
        2 => 0x3,
        _ => 0,
    };
    fmt.extend_from_slice(&mask.to_le_bytes());
    fmt.extend_from_slice(&[0xBF, 0xAA, 0x23, 0xE9, 0x58, 0xCB, 0x71, 0x44, 0xA1, 0x19, 0xFF, 0xFA, 0x01, 0xE4, 0xCE, 0x62]);
    fmt.extend_from_slice(&[0x01, 0x00]);
    fmt.extend_from_slice(&setup.to_be_bytes());
    fmt.extend_from_slice(&[0; 8]);
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&((4 + 8 + fmt.len() + 8 + 8 + 8 + data.len()) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
    out.extend_from_slice(&fmt);
    out.extend_from_slice(b"fact");
    out.extend_from_slice(&8u32.to_le_bytes());
    out.extend_from_slice(&(frames * AT3P_FRAME_SAMPLES).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaz0_round_trips_literals_and_back_references() {
        // "abcabcabcX": 3 literals, a back-reference of 6 from 3 back, then a literal.
        let mut src = b"Yaz0".to_vec();
        src.extend_from_slice(&10u32.to_be_bytes());
        src.extend_from_slice(&[0; 8]);
        src.push(0b1110_1000);
        src.extend_from_slice(b"abc");
        src.extend_from_slice(&[0x40, 0x02]);
        src.push(b'X');
        assert_eq!(yaz0(&src).unwrap(), b"abcabcabcX");
    }

    #[test]
    fn psmf_sound_is_taken_out_of_the_program_stream() {
        // Two frames of a stereo 44.1 kHz setup (frame 744 bytes), split across three
        // private-stream packets between video packets.
        let setup: u16 = 0x285C;
        let mut es = Vec::new();
        for fill in [0x11u8, 0x22] {
            es.extend_from_slice(&[0x0F, 0xD0]);
            es.extend_from_slice(&setup.to_be_bytes());
            es.extend_from_slice(&[0; 4]);
            es.extend_from_slice(&[fill; 744]);
        }
        let pes = |id: u8, body: &[u8]| {
            let mut p = vec![0, 0, 1, id];
            p.extend_from_slice(&((3 + 4 + body.len()) as u16).to_be_bytes());
            p.extend_from_slice(&[0x81, 0x80, 0x00]);
            p.extend_from_slice(&[0, 0, 0, 0]);
            p.extend_from_slice(body);
            p
        };
        let mut file = b"PSMF0015".to_vec();
        file.extend_from_slice(&0x800u32.to_be_bytes());
        file.resize(0x800, 0);
        let mut ps = Vec::new();
        for chunk in es.chunks(600) {
            ps.extend_from_slice(&pes(0xE0, &[0x55; 100]));
            ps.extend_from_slice(&pes(0xBD, chunk));
        }
        file[0x0C..0x10].copy_from_slice(&(ps.len() as u32).to_be_bytes());
        file.extend_from_slice(&ps);
        let riff = psmf_audio(&file).expect("the sound");
        assert_eq!(&riff[..4], b"RIFF");
        let data = riff.windows(4).position(|w| w == b"data").unwrap() + 8;
        assert_eq!(riff.len() - data, 2 * 744);
        assert!(riff[data..data + 744].iter().all(|&b| b == 0x11) && riff[data + 744..].iter().all(|&b| b == 0x22));
        assert_eq!(u16::from_le_bytes([riff[0x16], riff[0x17]]), 2, "channels");
        assert_eq!(u32::from_le_bytes(riff[0x18..0x1C].try_into().unwrap()), 44_100);
    }

    #[test]
    fn u8_lists_files_in_their_directories() {
        // Root (3 nodes), dir "snd" (ends at 3), file "bgm.dsp".
        let names = b"\0snd\0bgm.dsp\0";
        let root = 0x20usize;
        let strings = root + 3 * 12;
        let data_at = (strings + names.len()).div_ceil(32) * 32;
        let mut b = vec![0u8; data_at + 0x80];
        b[0..4].copy_from_slice(&0x55AA_382Du32.to_be_bytes());
        b[4..8].copy_from_slice(&(root as u32).to_be_bytes());
        let node = |b: &mut Vec<u8>, i: usize, word: u32, off: u32, size: u32| {
            let at = root + i * 12;
            b[at..at + 4].copy_from_slice(&word.to_be_bytes());
            b[at + 4..at + 8].copy_from_slice(&off.to_be_bytes());
            b[at + 8..at + 12].copy_from_slice(&size.to_be_bytes());
        };
        node(&mut b, 0, 0x0100_0000, 0, 3);
        node(&mut b, 1, 0x0100_0001, 0, 3);
        node(&mut b, 2, 5, data_at as u32, 0x80);
        b[strings..strings + names.len()].copy_from_slice(names);
        assert_eq!(u8_archive(&b), vec![("snd/bgm.dsp".to_string(), data_at, 0x80)]);
    }
}
