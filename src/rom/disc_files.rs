//! Every file on a disc image, listed by walking its filesystem through random reads: for
//! finding and then reading audio where it sits, without extracting anything.
//!
//! Three filesystems: ISO9660 (PlayStation 1/2/PSP/PS3, Saturn, Sega CD, PC Engine CD,
//! Neo Geo CD, Dreamcast), XDVDFS (Xbox, Xbox 360) and the GameCube FST. A Wii disc is
//! recognised and refused: its data partition is encrypted, and decrypting it needs the
//! console's key, which this code does not carry.
//!
//! [`detect::disc_fs`](super::detect::disc_fs) reads single named files the same way for
//! detection; this walks whole trees and keeps each file's location instead of its bytes.

use std::io;
use std::sync::{Arc, Mutex};

use super::source::ReadAt;

/// A window `[start, start + len)` onto a shared random-access reader. Cheap to clone:
/// every clone reads through the same reader, one call at a time.
#[derive(Clone)]
pub struct DiscView {
    inner: Arc<Mutex<Box<dyn ReadAt>>>,
    start: u64,
    len: u64,
}

impl DiscView {
    pub fn new(reader: Box<dyn ReadAt>, len: u64) -> Self {
        Self { inner: Arc::new(Mutex::new(reader)), start: 0, len }
    }

    pub fn size(&self) -> u64 {
        self.len
    }

    /// The part `[start, start + len)` of this view, clamped to it.
    pub fn slice(&self, start: u64, len: u64) -> Self {
        let start = start.min(self.len);
        Self { inner: self.inner.clone(), start: self.start + start, len: len.min(self.len - start) }
    }

    /// Exactly `buf.len()` bytes at `offset`.
    pub fn read(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        if offset.checked_add(buf.len() as u64).is_none_or(|end| end > self.len) {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "read past the end of the disc"));
        }
        let mut reader = self.inner.lock().map_err(|_| io::Error::other("disc reader poisoned"))?;
        reader.read_at(self.start + offset, buf)
    }

    fn bytes(&self, offset: u64, len: usize) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; len];
        self.read(offset, &mut buf).map_err(|e| e.to_string())?;
        Ok(buf)
    }
}

/// Where a file's bytes are on the disc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extent {
    /// One contiguous run starting at this offset.
    Linear { offset: u64 },
    /// 2048-byte sectors from `first` on, one every `stride` bytes (a raw 2352-byte CD image
    /// keeps a header before each sector's data).
    Sectors { first: u64, stride: u64 },
}

const SECTOR: u64 = 2048;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscFile {
    /// `/`-separated, without a leading `/`, as stored on the disc (case kept).
    pub path: String,
    pub size: u64,
    pub extent: Extent,
}

impl DiscFile {
    /// The file name, without its directory.
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// The directory part, without a trailing `/` ("" at the root).
    pub fn dir(&self) -> &str {
        self.path.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
    }

    /// Reads up to `dst.len()` bytes at `offset` of this file; returns how many.
    pub fn read(&self, view: &DiscView, offset: u64, dst: &mut [u8]) -> io::Result<usize> {
        if offset >= self.size {
            return Ok(0);
        }
        let want = dst.len().min((self.size - offset) as usize);
        match self.extent {
            Extent::Linear { offset: base } => {
                view.read(base + offset, &mut dst[..want])?;
            }
            Extent::Sectors { first, stride } => {
                let mut done = 0usize;
                while done < want {
                    let pos = offset + done as u64;
                    let (sector, within) = (pos / SECTOR, pos % SECTOR);
                    let take = ((SECTOR - within) as usize).min(want - done);
                    view.read(first + sector * stride + within, &mut dst[done..done + take])?;
                    done += take;
                }
            }
        }
        Ok(want)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsKind {
    Iso9660,
    Xdvdfs,
    GameCube,
    /// A Nintendo DS / DSi cartridge's NitroFS.
    Nitro,
    /// A 3DS cartridge's RomFS (decrypted images only).
    RomFs3ds,
}

impl FsKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Iso9660 => "iso9660",
            Self::Xdvdfs => "xdvdfs",
            Self::GameCube => "gamecube",
            Self::Nitro => "nitro",
            Self::RomFs3ds => "3ds_romfs",
        }
    }
}

pub struct Listing {
    pub fs: FsKind,
    pub files: Vec<DiscFile>,
}

/// Directories nested deeper than this are not followed (a damaged table can point back up).
const MAX_DEPTH: usize = 32;
/// More entries than any real disc has; a damaged table stops here instead of running away.
const MAX_FILES: usize = 500_000;

/// Lists every file of the filesystem on `view`, trying the GameCube header, then XDVDFS,
/// then ISO9660.
pub fn list(view: &DiscView) -> Result<Listing, String> {
    if view.size() >= 0x440 {
        let head = view.bytes(0, 0x440)?;
        // The Nintendo logo's CRC every DS / DSi cartridge header carries.
        if head[0x15C..0x15E] == [0x56, 0xCF] {
            return nitro(view, &head).map(|files| Listing { fs: FsKind::Nitro, files });
        }
        if &head[0x100..0x104] == b"NCSD" {
            return romfs_3ds(view, &head).map(|files| Listing { fs: FsKind::RomFs3ds, files });
        }
        if head[0x18..0x1C] == [0x5D, 0x1C, 0x9E, 0xA3] {
            return Err("a Wii disc: its data partition is encrypted and is not read yet".into());
        }
        if head[0x1C..0x20] == [0xC2, 0x33, 0x9F, 0x3D] {
            return gamecube(view, &head).map(|files| Listing { fs: FsKind::GameCube, files });
        }
    }
    if let Some(files) = xdvdfs(view)? {
        return Ok(Listing { fs: FsKind::Xdvdfs, files });
    }
    if let Some(files) = iso9660(view)? {
        return Ok(Listing { fs: FsKind::Iso9660, files });
    }
    Err("no ISO9660, XDVDFS, GameCube, DS or 3DS filesystem found".into())
}

// --- Nintendo DS NitroFS ----------------------------------------------------------------

/// The NitroFS of a DS / DSi cartridge: the name table (directories, then each one's
/// entries) and the allocation table of absolute start / end offsets.
fn nitro(view: &DiscView, head: &[u8]) -> Result<Vec<DiscFile>, String> {
    let fnt_at = u64::from(le32(head, 0x40).unwrap_or(0));
    let fnt_len = le32(head, 0x44).unwrap_or(0) as usize;
    let fat_at = u64::from(le32(head, 0x48).unwrap_or(0));
    let fat_len = le32(head, 0x4C).unwrap_or(0) as usize;
    if fnt_len < 8 || fat_len < 8 || fnt_len > 16 << 20 || fat_len > 16 << 20 || fnt_at + fnt_len as u64 > view.size() || fat_at + fat_len as u64 > view.size() {
        return Err("a DS cartridge with no file system".into());
    }
    let fnt = view.bytes(fnt_at, fnt_len)?;
    let fat = view.bytes(fat_at, fat_len)?;
    let dirs = usize::from(le16(&fnt, 6).unwrap_or(1)).clamp(1, fnt_len / 8);
    let mut files = Vec::new();
    let mut stack = vec![(0usize, String::new(), 0usize)];
    while let Some((dir, prefix, depth)) = stack.pop() {
        if dir >= dirs || depth > MAX_DEPTH {
            continue;
        }
        let mut at = le32(&fnt, dir * 8).unwrap_or(0) as usize;
        let mut id = usize::from(le16(&fnt, dir * 8 + 4).unwrap_or(0));
        while let Some(&kind) = fnt.get(at) {
            if kind == 0 || files.len() >= MAX_FILES {
                break;
            }
            let len = usize::from(kind & 0x7F);
            let Some(name) = fnt.get(at + 1..at + 1 + len) else { break };
            let name = String::from_utf8_lossy(name).into_owned();
            let path = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
            at += 1 + len;
            if kind & 0x80 != 0 {
                let Some(sub) = le16(&fnt, at) else { break };
                at += 2;
                stack.push((usize::from(sub & 0x0FFF), path, depth + 1));
            } else {
                let (start, end) = (le32(&fat, id * 8).unwrap_or(0), le32(&fat, id * 8 + 4).unwrap_or(0));
                id += 1;
                if end > start && u64::from(end) <= view.size() {
                    files.push(DiscFile { path, size: u64::from(end - start), extent: Extent::Linear { offset: u64::from(start) } });
                }
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

// --- Nintendo 3DS RomFS -----------------------------------------------------------------

const MEDIA_UNIT: u64 = 0x200;

fn le64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

fn utf16_name(b: &[u8]) -> String {
    let units: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    String::from_utf16_lossy(&units)
}

/// The RomFS of a 3DS cartridge image (NCSD): its first partition's NCCH, which must be
/// stored decrypted (the NoCrypto flag), then the IVFC tree's level 3, the files themselves.
fn romfs_3ds(view: &DiscView, head: &[u8]) -> Result<Vec<DiscFile>, String> {
    let ncch_at = u64::from(le32(head, 0x120).unwrap_or(0)) * MEDIA_UNIT;
    if ncch_at == 0 || ncch_at + 0x200 > view.size() {
        return Err("a 3DS image with no content partition".into());
    }
    let ncch = view.bytes(ncch_at, 0x200)?;
    if &ncch[0x100..0x104] != b"NCCH" {
        return Err("a 3DS image whose content partition is not NCCH".into());
    }
    if ncch[0x18F] & 0x04 == 0 {
        return Err("a 3DS image that is encrypted: decrypted images are read, this one is not yet".into());
    }
    let romfs_at = ncch_at + u64::from(le32(&ncch, 0x1B0).unwrap_or(0)) * MEDIA_UNIT;
    let romfs_len = u64::from(le32(&ncch, 0x1B4).unwrap_or(0)) * MEDIA_UNIT;
    if romfs_len < 0x60 || romfs_at + romfs_len > view.size() {
        return Err("a 3DS image with no RomFS".into());
    }
    let ivfc = view.bytes(romfs_at, 0x60)?;
    if &ivfc[..4] != b"IVFC" {
        return Err("a 3DS RomFS without its IVFC header".into());
    }
    let master = u64::from(le32(&ivfc, 0x08).unwrap_or(0));
    let block = 1u64 << le32(&ivfc, 0x4C).unwrap_or(12).min(24);
    let l3 = romfs_at + (0x60 + master).div_ceil(block) * block;
    if l3 + 0x28 > view.size() {
        return Err("a 3DS RomFS cut short".into());
    }
    let h = view.bytes(l3, 0x28)?;
    let part = |at: usize| -> Result<Vec<u8>, String> {
        let (off, len) = (u64::from(le32(&h, at).unwrap_or(0)), le32(&h, at + 4).unwrap_or(0) as usize);
        if len > 64 << 20 || l3 + off + len as u64 > view.size() {
            return Err("a 3DS RomFS with a damaged table".into());
        }
        view.bytes(l3 + off, len)
    };
    let dirs = part(0x0C)?;
    let metas = part(0x1C)?;
    let data_at = l3 + u64::from(le32(&h, 0x24).unwrap_or(0));
    let mut files = Vec::new();
    let mut stack = vec![(0usize, String::new(), 0usize)];
    const NONE: u32 = u32::MAX;
    while let Some((dir, prefix, depth)) = stack.pop() {
        if depth > MAX_DEPTH || dir + 0x18 > dirs.len() {
            continue;
        }
        let mut child = le32(&dirs, dir + 0x08).unwrap_or(NONE);
        while child != NONE && (child as usize) + 0x18 <= dirs.len() && stack.len() < MAX_FILES {
            let c = child as usize;
            let len = le32(&dirs, c + 0x14).unwrap_or(0) as usize;
            let name = utf16_name(dirs.get(c + 0x18..c + 0x18 + len).unwrap_or(&[]));
            stack.push((c, if prefix.is_empty() { name } else { format!("{prefix}/{name}") }, depth + 1));
            child = le32(&dirs, c + 0x04).unwrap_or(NONE);
        }
        let mut file = le32(&dirs, dir + 0x0C).unwrap_or(NONE);
        while file != NONE && (file as usize) + 0x20 <= metas.len() && files.len() < MAX_FILES {
            let f = file as usize;
            let (offset, size) = (le64(&metas, f + 0x08).unwrap_or(0), le64(&metas, f + 0x10).unwrap_or(0));
            let len = le32(&metas, f + 0x1C).unwrap_or(0) as usize;
            let name = utf16_name(metas.get(f + 0x20..f + 0x20 + len).unwrap_or(&[]));
            if size > 0 && data_at + offset + size <= view.size() {
                let path = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
                files.push(DiscFile { path, size, extent: Extent::Linear { offset: data_at + offset } });
            }
            file = le32(&metas, f + 0x04).unwrap_or(NONE);
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

// --- GameCube --------------------------------------------------------------------------

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn gamecube(view: &DiscView, head: &[u8]) -> Result<Vec<DiscFile>, String> {
    let fst_offset = u64::from(be32(head, 0x424).ok_or("no FST offset")?);
    let fst_size = u64::from(be32(head, 0x428).ok_or("no FST size")?);
    if fst_size < 12 || fst_offset + fst_size > view.size() || fst_size > 64 << 20 {
        return Err("the GameCube FST is out of range".into());
    }
    let fst = view.bytes(fst_offset, fst_size as usize)?;
    let count = be32(&fst, 8).ok_or("empty FST")? as usize;
    let strings_at = count.checked_mul(12).filter(|n| *n <= fst.len()).ok_or("the FST entry count is out of range")?;
    let name_at = |raw: u32| -> String {
        let at = strings_at + (raw & 0x00FF_FFFF) as usize;
        let bytes: Vec<u8> = fst.get(at..).unwrap_or(&[]).iter().take_while(|b| **b != 0).copied().collect();
        // GameCube names are Shift-JIS on Japanese discs; ASCII is all that matters here.
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let mut files = Vec::new();
    // (index one past the directory's last entry, its path)
    let mut dirs: Vec<(usize, String)> = vec![(count, String::new())];
    let mut i = 1;
    while i < count {
        while dirs.last().is_some_and(|(end, _)| i >= *end) {
            dirs.pop();
        }
        let prefix = dirs.last().map(|(_, p)| p.clone()).unwrap_or_default();
        let entry = &fst[i * 12..i * 12 + 12];
        let word0 = be32(entry, 0).unwrap_or(0);
        let name = name_at(word0);
        let path = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        if word0 >> 24 != 0 {
            let end = be32(entry, 8).unwrap_or(0) as usize;
            if end > i && end <= count && dirs.len() < MAX_DEPTH {
                dirs.push((end, path));
            }
        } else {
            let (offset, size) = (u64::from(be32(entry, 4).unwrap_or(0)), u64::from(be32(entry, 8).unwrap_or(0)));
            if offset + size <= view.size() {
                files.push(DiscFile { path, size, extent: Extent::Linear { offset } });
            }
        }
        i += 1;
        if files.len() >= MAX_FILES {
            break;
        }
    }
    Ok(files)
}

// --- XDVDFS ----------------------------------------------------------------------------

const XDVDFS_MAGIC: &[u8; 20] = b"MICROSOFT*XBOX*MEDIA";

fn xdvdfs(view: &DiscView) -> Result<Option<Vec<DiscFile>>, String> {
    // An XISO (0), then where Redump images of XGD1 / XGD2 / XGD3 keep the game partition.
    let Some(base) = [0u64, 0x1830_0000, 0x0FD9_0000, 0x0208_0000].into_iter().find(|base| {
        let at = base + 32 * SECTOR;
        at + SECTOR <= view.size() && view.bytes(at, 20).is_ok_and(|b| b == XDVDFS_MAGIC)
    }) else {
        return Ok(None);
    };
    let vd = view.bytes(base + 32 * SECTOR, SECTOR as usize)?;
    let (root_sector, root_size) = (le32(&vd, 0x14).unwrap_or(0), le32(&vd, 0x18).unwrap_or(0));
    let mut files = Vec::new();
    xdvdfs_dir(view, base, root_sector, root_size, "", 0, &mut files)?;
    Ok(Some(files))
}

fn xdvdfs_dir(view: &DiscView, base: u64, sector: u32, size: u32, prefix: &str, depth: usize, out: &mut Vec<DiscFile>) -> Result<(), String> {
    if size == 0 || depth > MAX_DEPTH || out.len() >= MAX_FILES {
        return Ok(());
    }
    let at = base + u64::from(sector) * SECTOR;
    if at + u64::from(size) > view.size() || size > 16 << 20 {
        return Ok(());
    }
    let table = view.bytes(at, size as usize)?;
    // The table is a binary tree; offsets are in 4-byte units from the table's start.
    let mut stack = vec![0usize];
    let mut seen = std::collections::HashSet::new();
    let mut entries = Vec::new();
    while let Some(word) = stack.pop() {
        let off = word * 4;
        if !seen.insert(off) || off + 14 > table.len() {
            continue;
        }
        let (left, right) = (le16(&table, off).unwrap_or(0), le16(&table, off + 2).unwrap_or(0));
        if left == 0xFFFF && right == 0xFFFF {
            continue;
        }
        let name_len = table[off + 13] as usize;
        let Some(name_bytes) = table.get(off + 14..off + 14 + name_len) else { continue };
        entries.push((String::from_utf8_lossy(name_bytes).into_owned(), le32(&table, off + 4).unwrap_or(0), le32(&table, off + 8).unwrap_or(0), table[off + 12]));
        if left != 0 && left != 0xFFFF {
            stack.push(left as usize);
        }
        if right != 0 && right != 0xFFFF {
            stack.push(right as usize);
        }
    }
    entries.sort_by(|a, b| a.0.to_ascii_uppercase().cmp(&b.0.to_ascii_uppercase()));
    for (name, start, len, attr) in entries {
        let path = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        if attr & 0x10 != 0 {
            xdvdfs_dir(view, base, start, len, &path, depth + 1, out)?;
        } else {
            let offset = base + u64::from(start) * SECTOR;
            if offset + u64::from(len) <= view.size() {
                out.push(DiscFile { path, size: u64::from(len), extent: Extent::Linear { offset } });
            }
        }
    }
    Ok(())
}

// --- ISO9660 ---------------------------------------------------------------------------

/// Where sector `n`'s 2048 data bytes start: `user + (n - lba_base) * stride`.
#[derive(Debug, Clone, Copy)]
struct IsoLayout {
    stride: u64,
    user: u64,
    lba_base: u64,
}

impl IsoLayout {
    fn at(&self, lba: u64) -> Option<u64> {
        Some(self.user + lba.checked_sub(self.lba_base)? * self.stride)
    }
}

fn iso9660(view: &DiscView) -> Result<Option<Vec<DiscFile>>, String> {
    let layouts = [(2048u64, 0u64), (2352, 16), (2352, 24), (2336, 8)];
    let mut found = None;
    'probe: for (stride, user) in layouts {
        let at = user + 16 * stride;
        if at + SECTOR > view.size() {
            continue;
        }
        let pvd = view.bytes(at, SECTOR as usize)?;
        if pvd[0] == 1 && &pvd[1..6] == b"CD001" {
            found = Some((pvd, stride, user));
            break 'probe;
        }
    }
    // A Dreamcast GD-ROM's high-density area, read as one image counted from the start of
    // the disc (`rom_audio`'s GD-ROM view): its descriptor is at sector 45016.
    if found.is_none() && view.size() >= (45_016 + 1) * SECTOR {
        let pvd = view.bytes(45_016 * SECTOR, SECTOR as usize)?;
        if pvd[0] == 1 && &pvd[1..6] == b"CD001" {
            found = Some((pvd, SECTOR, 0));
        }
    }
    let Some((pvd, stride, user)) = found else { return Ok(None) };
    let root = &pvd[156..156 + 34];
    let root_lba = u64::from(le32(root, 2).unwrap_or(0));
    let root_len = le32(root, 10).unwrap_or(0);
    // LBAs count from the start of the image: a GD-ROM's high-density area comes here as
    // `rom_audio`'s whole-area view, which keeps them disc-relative.
    let layout = IsoLayout { stride, user, lba_base: 0 };
    let mut files = Vec::new();
    iso_dir(view, layout, root_lba, root_len, "", 0, &mut files)?;
    Ok(Some(files))
}

/// Whether a directory record's CD-XA system use area marks the file as Mode 2 Form 2 or
/// interleaved (XA audio, STR video).
fn is_xa_form2(record: &[u8], name_len: usize) -> bool {
    // The system use area follows the name, padded to an even offset.
    let at = 33 + name_len + usize::from(name_len % 2 == 0);
    let Some(xa) = record.get(at..at + 14) else { return false };
    if &xa[6..8] != b"XA" {
        return false;
    }
    let attributes = u16::from_be_bytes([xa[4], xa[5]]);
    attributes & (0x1000 | 0x2000) != 0
}

fn iso_extent(view: &DiscView, layout: IsoLayout, lba: u64, len: u32) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(len as usize);
    let sectors = u64::from(len).div_ceil(SECTOR);
    for s in 0..sectors {
        let at = layout.at(lba + s).ok_or("directory before the start of the track")?;
        let take = (u64::from(len) - s * SECTOR).min(SECTOR) as usize;
        out.extend_from_slice(&view.bytes(at, take)?);
    }
    Ok(out)
}

fn iso_dir(view: &DiscView, layout: IsoLayout, lba: u64, len: u32, prefix: &str, depth: usize, out: &mut Vec<DiscFile>) -> Result<(), String> {
    if depth > MAX_DEPTH || out.len() >= MAX_FILES || len > 16 << 20 {
        return Ok(());
    }
    let Ok(bytes) = iso_extent(view, layout, lba, len) else { return Ok(()) };
    let mut i = 0usize;
    while i < bytes.len() {
        let record_len = bytes[i] as usize;
        if record_len == 0 {
            // Records never cross a sector; zeros pad to the next one.
            i = (i / SECTOR as usize + 1) * SECTOR as usize;
            continue;
        }
        let Some(record) = bytes.get(i..i + record_len) else { break };
        i += record_len;
        if record.len() < 34 {
            continue;
        }
        let name_len = record[32] as usize;
        let Some(raw_name) = record.get(33..33 + name_len) else { continue };
        if raw_name == [0] || raw_name == [1] {
            continue; // `.` and `..`
        }
        let name = String::from_utf8_lossy(raw_name);
        let name = name.split(';').next().unwrap_or(&name).trim_end_matches('.').to_string();
        let path = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        let (extent, size, flags) = (u64::from(le32(record, 2).unwrap_or(0)), le32(record, 10).unwrap_or(0), record[25]);
        if flags & 0x02 != 0 {
            if extent != lba {
                iso_dir(view, layout, extent, size, &path, depth + 1, out)?;
            }
        } else if let Some(first) = layout.at(extent) {
            let sectors = u64::from(size).div_ceil(SECTOR);
            if sectors == 0 || first + (sectors - 1) * layout.stride + SECTOR <= view.size() {
                if layout.stride != SECTOR && is_xa_form2(record, name_len) {
                    // CD-XA audio (and the audio in PS1 videos) is in Mode 2 Form 2 sectors,
                    // whose 2324 bytes do not fit the 2048-byte reading: hand over the raw
                    // sectors, headers and all, as a ripped .xa is.
                    out.push(DiscFile { path, size: sectors * layout.stride, extent: Extent::Linear { offset: first - layout.user } });
                } else {
                    let extent = if layout.stride == SECTOR { Extent::Linear { offset: first } } else { Extent::Sectors { first, stride: layout.stride } };
                    out.push(DiscFile { path, size: u64::from(size), extent });
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &[u8], attributes: u16) -> Vec<u8> {
        let mut r = vec![0u8; 33];
        r[32] = name.len() as u8;
        r.extend_from_slice(name);
        if name.len() % 2 == 0 {
            r.push(0);
        }
        let mut xa = [0u8; 14];
        xa[4..6].copy_from_slice(&attributes.to_be_bytes());
        xa[6..8].copy_from_slice(b"XA");
        r.extend_from_slice(&xa);
        r
    }

    #[test]
    fn xa_audio_and_video_are_told_from_data_by_their_attributes() {
        assert!(is_xa_form2(&record(b"MUSIC.XA;1", 0x3D55), 10), "interleaved form 2");
        assert!(is_xa_form2(&record(b"MOVIE.STR;1", 0x2555), 11), "interleaved");
        assert!(!is_xa_form2(&record(b"DATA.BIN;1", 0x0D55), 10), "mode 2 form 1 is plain data");
        let mut plain = vec![0u8; 33];
        plain[32] = 1;
        plain.push(b'A');
        assert!(!is_xa_form2(&plain, 1), "no system use area");
    }
}
