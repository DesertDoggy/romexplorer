//! PC-98 floppy images read as the MS-DOS disks most of them are: the image's sectors laid
//! out in order, then the FAT12 file system on them. Games that boot their own loader keep
//! no file system and list no files here.
//!
//! Images:
//! - **D88** (`.d88`, `.88d`, `.d98`, `.98d`): a 0x2B0-byte header with each track's offset,
//!   then per sector a 16-byte header (C, H, R, N, sector count, ..., data size) and its data.
//!   Sectors are laid out track by track, each track's in sector-number order.
//! - **FDI** (Anex86 `.fdi`): a header giving its own size and the sector size, then the raw
//!   sectors.
//! - Raw images (`.hdm`, `.xdf`, `.dup`, `.tfd`, `.2hd`, `.img`): the sectors as they are.
//!
//! Music files are found by the extensions their drivers use: PMD's `.M`, `.M2`, `.MZ`,
//! `.MP`, `.MS` and FMP's `.OPI`, `.OVI`, `.OZI`.

/// One file on the disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskFile {
    /// `DIR/NAME.EXT`, as stored (upper case, 8.3).
    pub path: String,
    pub size: u32,
    cluster: u16,
}

/// A floppy's sectors and the files on them.
pub struct Disk {
    data: Vec<u8>,
    bps: usize,
    spc: usize,
    fat: Vec<u8>,
    data_start: usize,
    pub files: Vec<DiskFile>,
}

/// The extensions this reader opens.
pub const EXTENSIONS: &[&str] = &["d88", "88d", "d98", "98d", "fdi", "hdm", "xdf", "dup", "tfd", "2hd"];

/// PMD's and FMP's song file extensions.
const MUSIC_EXTENSIONS: &[&str] = &["M", "M2", "MZ", "MP", "MS", "OPI", "OVI", "OZI"];

fn u16_at(b: &[u8], at: usize) -> Option<usize> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?) as usize)
}

fn u32_at(b: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?) as usize)
}

/// The sectors of a D88 image, in order.
fn d88_sectors(b: &[u8]) -> Option<Vec<u8>> {
    let size = u32_at(b, 0x1C)?.min(b.len());
    let mut out = Vec::new();
    for t in 0..164 {
        let at = u32_at(b, 0x20 + t * 4)?;
        if at == 0 || at >= size {
            continue;
        }
        let count = u16_at(b, at + 4)?.max(1);
        let mut sectors: Vec<(u8, &[u8])> = Vec::with_capacity(count);
        let mut p = at;
        for _ in 0..count {
            let r = *b.get(p + 2)?;
            let len = u16_at(b, p + 0x0E)?;
            let data = b.get(p + 16..p + 16 + len)?;
            sectors.push((r, data));
            p += 16 + len;
        }
        sectors.sort_by_key(|s| s.0);
        sectors.iter().for_each(|s| out.extend_from_slice(s.1));
    }
    (!out.is_empty()).then_some(out)
}

/// The sectors of an image, by its extension.
fn sectors(bytes: &[u8], ext: &str) -> Option<Vec<u8>> {
    match ext {
        "d88" | "88d" | "d98" | "98d" => d88_sectors(bytes),
        "fdi" => {
            let header = u32_at(bytes, 8)?;
            let size = u32_at(bytes, 0x0C)?;
            bytes.get(header..header.checked_add(size)?.min(bytes.len())).map(<[u8]>::to_vec)
        }
        _ => Some(bytes.to_vec()),
    }
}

impl Disk {
    /// Reads the image `bytes` (its extension `ext`, lower case) and lists its files.
    pub fn open(bytes: &[u8], ext: &str) -> Result<Self, String> {
        let data = sectors(bytes, ext).ok_or("not a floppy image this reader knows")?;
        let boot = data.get(..0x20).ok_or("the image is empty")?;
        // The BPB, when the disk has a sane one; else the 1.2 MB 2HD layout PC-98 MS-DOS
        // formats (1024-byte sectors, 2 FATs of 2 sectors, 192 root entries).
        let bpb = (|| {
            let bps = u16_at(boot, 0x0B)?;
            let spc = boot[0x0D] as usize;
            let reserved = u16_at(boot, 0x0E)?;
            let fats = boot[0x10] as usize;
            let roots = u16_at(boot, 0x11)?;
            let per_fat = u16_at(boot, 0x16)?;
            let sane = matches!(bps, 256 | 512 | 1024) && spc.is_power_of_two() && spc <= 8 && (1..=2).contains(&fats) && roots > 0 && per_fat > 0 && reserved > 0;
            sane.then_some((bps, spc, reserved, fats, roots, per_fat))
        })();
        let (bps, spc, reserved, fats, roots, per_fat) = bpb.unwrap_or((1024, 1, 1, 2, 192, 2));
        let fat_at = reserved * bps;
        let fat = data.get(fat_at..fat_at + per_fat * bps).ok_or("no FAT on the disk")?.to_vec();
        let root_at = fat_at + fats * per_fat * bps;
        let root_len = roots * 32;
        let data_start = root_at + root_len.div_ceil(bps) * bps;
        let root = data.get(root_at..root_at + root_len).ok_or("no root directory on the disk")?.to_vec();
        let mut disk = Disk { data, bps, spc, fat, data_start, files: Vec::new() };
        let mut files = Vec::new();
        disk.walk(&root, "", 0, &mut files);
        disk.files = files;
        Ok(disk)
    }

    fn cluster_bytes(&self) -> usize {
        self.bps * self.spc
    }

    /// The FAT12 entry of `cluster`.
    fn next(&self, cluster: u16) -> u16 {
        let i = cluster as usize * 3 / 2;
        let (Some(&a), Some(&b)) = (self.fat.get(i), self.fat.get(i + 1)) else { return 0xFFF };
        let v = u16::from_le_bytes([a, b]);
        if cluster & 1 == 0 { v & 0xFFF } else { v >> 4 }
    }

    /// The bytes of the chain from `cluster`, at most `limit`.
    fn chain(&self, mut cluster: u16, limit: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let size = self.cluster_bytes();
        let mut steps = 0;
        while (2..0xFF0).contains(&cluster) && out.len() < limit && steps < 4096 {
            let at = self.data_start + (cluster as usize - 2) * size;
            let Some(bytes) = self.data.get(at..at + size) else { break };
            out.extend_from_slice(bytes);
            cluster = self.next(cluster);
            steps += 1;
        }
        out.truncate(limit);
        out
    }

    fn walk(&self, dir: &[u8], prefix: &str, depth: usize, out: &mut Vec<DiskFile>) {
        for e in dir.chunks_exact(32) {
            match e[0] {
                0 => break,
                0xE5 | b'.' => continue,
                _ => {}
            }
            let attr = e[0x0B];
            if attr & 0x08 != 0 {
                continue; // a volume label, or a long-name piece
            }
            let text = |b: &[u8]| String::from_utf8_lossy(b).trim_end().to_string();
            let (name, ext) = (text(&e[..8]), text(&e[8..11]));
            let name = if ext.is_empty() { name } else { format!("{name}.{ext}") };
            let path = format!("{prefix}{name}");
            let cluster = u16_at(e, 0x1A).unwrap_or(0) as u16;
            if attr & 0x10 != 0 {
                if depth < 8 {
                    let sub = self.chain(cluster, 64 * 1024);
                    self.walk(&sub, &format!("{path}/"), depth + 1, out);
                }
                continue;
            }
            out.push(DiskFile { path, size: u32_at(e, 0x1C).unwrap_or(0) as u32, cluster });
        }
    }

    /// A file's bytes.
    pub fn read(&self, file: &DiskFile) -> Vec<u8> {
        self.chain(file.cluster, file.size as usize)
    }

    /// The file at `path`, matched without regard to case.
    pub fn find(&self, path: &str) -> Option<&DiskFile> {
        self.files.iter().find(|f| f.path.eq_ignore_ascii_case(path))
    }

    /// The file named `name` (no folder) in the same folder as `beside`, any case.
    pub fn sibling(&self, beside: &DiskFile, name: &str) -> Option<&DiskFile> {
        let dir = beside.path.rsplit_once('/').map(|(d, _)| format!("{d}/")).unwrap_or_default();
        self.find(&format!("{dir}{name}")).or_else(|| self.files.iter().find(|f| f.path.rsplit('/').next().is_some_and(|n| n.eq_ignore_ascii_case(name))))
    }

    /// The song files of PMD and FMP on the disk.
    pub fn music_files(&self) -> Vec<&DiskFile> {
        self.files
            .iter()
            .filter(|f| f.size > 0 && f.path.rsplit_once('.').is_some_and(|(_, e)| MUSIC_EXTENSIONS.iter().any(|m| m.eq_ignore_ascii_case(e))))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2HD disk in PC-98 MS-DOS's layout with `files` in its root and one in a folder.
    fn disk(files: &[(&str, &str, &[u8])]) -> Vec<u8> {
        let bps = 1024;
        let mut d = vec![0u8; 1232 * bps];
        d[0x0B..0x0D].copy_from_slice(&1024u16.to_le_bytes());
        d[0x0D] = 1;
        d[0x0E..0x10].copy_from_slice(&1u16.to_le_bytes());
        d[0x10] = 2;
        d[0x11..0x13].copy_from_slice(&192u16.to_le_bytes());
        d[0x16..0x18].copy_from_slice(&2u16.to_le_bytes());
        let (fat, root, data) = (bps, 5 * bps, 11 * bps);
        let set_fat = |d: &mut Vec<u8>, c: usize, v: u16| {
            let i = fat + c * 3 / 2;
            let cur = u16::from_le_bytes([d[i], d[i + 1]]);
            let w = if c & 1 == 0 { (cur & 0xF000) | v } else { (cur & 0x000F) | (v << 4) };
            d[i..i + 2].copy_from_slice(&w.to_le_bytes());
        };
        let mut cluster = 2usize;
        for (n, (name, ext, body)) in files.iter().enumerate() {
            let e = root + n * 32;
            d[e..e + 11].fill(b' ');
            d[e..e + name.len()].copy_from_slice(name.as_bytes());
            d[e + 8..e + 8 + ext.len()].copy_from_slice(ext.as_bytes());
            d[e + 0x1A..e + 0x1C].copy_from_slice(&(cluster as u16).to_le_bytes());
            d[e + 0x1C..e + 0x20].copy_from_slice(&(body.len() as u32).to_le_bytes());
            let clusters = body.len().div_ceil(bps).max(1);
            for k in 0..clusters {
                let c = cluster + k;
                set_fat(&mut d, c, if k + 1 == clusters { 0xFFF } else { (c + 1) as u16 });
                let at = data + (c - 2) * bps;
                let piece = &body[k * bps..((k + 1) * bps).min(body.len())];
                d[at..at + piece.len()].copy_from_slice(piece);
            }
            cluster += clusters;
        }
        d
    }

    #[test]
    fn a_raw_2hd_disk_lists_and_reads_its_files() {
        let song: Vec<u8> = (0..3000u32).map(|i| i as u8).collect();
        let image = disk(&[("SONG01", "M", &song), ("README", "TXT", b"hello"), ("BGM", "OPI", b"fmp")]);
        let d = Disk::open(&image, "hdm").unwrap();
        assert_eq!(d.files.len(), 3);
        let f = d.find("song01.m").unwrap();
        assert_eq!(d.read(f), song);
        let music: Vec<&str> = d.music_files().iter().map(|f| f.path.as_str()).collect();
        assert_eq!(music, ["SONG01.M", "BGM.OPI"]);
        assert_eq!(d.sibling(f, "readme.txt").map(|f| d.read(f)), Some(b"hello".to_vec()));
    }

    #[test]
    fn a_d88_image_is_laid_out_track_by_track_in_sector_order() {
        let raw = disk(&[("A", "M", b"pmd song")]);
        // 77 cylinders x 2 heads x 8 sectors of 1024 bytes, each track's sectors stored in
        // reverse order to check they are sorted back.
        let mut d88 = vec![0u8; 0x2B0];
        for t in 0..154usize {
            let at = d88.len() as u32;
            d88[0x20 + t * 4..0x24 + t * 4].copy_from_slice(&at.to_le_bytes());
            for r in (1..=8u8).rev() {
                let mut h = [0u8; 16];
                h[0] = (t / 2) as u8;
                h[1] = (t % 2) as u8;
                h[2] = r;
                h[3] = 3;
                h[4..6].copy_from_slice(&8u16.to_le_bytes());
                h[0x0E..0x10].copy_from_slice(&1024u16.to_le_bytes());
                d88.extend_from_slice(&h);
                let lba = t * 8 + (r as usize - 1);
                d88.extend_from_slice(&raw[lba * 1024..(lba + 1) * 1024]);
            }
        }
        let len = d88.len() as u32;
        d88[0x1C..0x20].copy_from_slice(&len.to_le_bytes());
        let d = Disk::open(&d88, "d88").unwrap();
        assert_eq!(d.read(d.find("A.M").unwrap()), b"pmd song");
    }

    #[test]
    fn an_fdi_image_skips_its_header() {
        let raw = disk(&[("B", "M2", b"song")]);
        let mut fdi = vec![0u8; 4096];
        fdi[8..12].copy_from_slice(&4096u32.to_le_bytes());
        fdi[0x0C..0x10].copy_from_slice(&(raw.len() as u32).to_le_bytes());
        fdi.extend_from_slice(&raw);
        let d = Disk::open(&fdi, "fdi").unwrap();
        assert_eq!(d.read(d.find("B.M2").unwrap()), b"song");
    }
}
