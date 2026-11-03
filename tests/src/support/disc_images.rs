//! Synthetic disc images for tests, shared by romexplorer's and chdlite's: no game data.
//!
//! - PlayStation EBOOT.PBP, laid out the way PSX2PSP/popstation write one: the PSISOIMG
//!   header, its TOC in absolute time (track 1 at 00:02:00), the block index, and the disc as
//!   raw sectors deflated sixteen at a time.
//! - Xbox / Xbox 360 images: an XDVDFS volume holding a few patterned files, placed where a
//!   Redump image puts its game partition, with filler in front standing in for the video
//!   partition. Sparse, so a 400 MB image costs little disk.

#![allow(dead_code)]

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use romexplorer::rom::zlib_ng;

pub const CD_SECTOR: usize = 2352;
pub const SECTOR: usize = 2048;
pub const XGD1: u64 = 0x1830_0000;
pub const XGD2: u64 = 0x0FD9_0000;

pub fn pattern(len: usize, seed: u32) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x & 0x0F) as u8
        })
        .collect()
}

pub fn bcd(v: u32) -> u8 {
    (((v / 10) << 4) | (v % 10)) as u8
}

pub fn msf(lba: u32) -> [u8; 3] {
    [bcd(lba / 4500), bcd(lba / 75 % 60), bcd(lba % 75)]
}

/// One disc's PSISOIMG: `tracks` are (audio, first sector) in the disc image `image`.
pub fn psisoimg(image: &[u8], tracks: &[(bool, u32)]) -> Vec<u8> {
    let sectors = (image.len() / CD_SECTOR) as u32;
    let mut out = vec![0u8; 0x100000];
    out[..12].copy_from_slice(b"PSISOIMG0000");
    out[0x400..0x40B].copy_from_slice(b"_SLUS_00000");
    // TOC at 0x800: A0, A1 (track count), A2 (lead-out), then the tracks, in absolute time.
    let mut toc = Vec::new();
    let entry = |kind: u8, track: u8, i0: [u8; 3], i1: [u8; 3]| {
        let mut e = vec![kind, 0, track];
        e.extend_from_slice(&i0);
        e.push(0);
        e.extend_from_slice(&i1);
        e
    };
    toc.extend(entry(0x41, 0xA0, [0; 3], [1, 0x20, 0]));
    toc.extend(entry(0x01, 0xA1, [0; 3], [bcd(tracks.len() as u32), 0, 0]));
    toc.extend(entry(0x01, 0xA2, [0; 3], msf(sectors + 150)));
    for (i, (audio, start)) in tracks.iter().enumerate() {
        let index1 = start + 150 + if i == 0 { 0 } else { 150 };
        toc.extend(entry(if *audio { 0x01 } else { 0x41 }, bcd(i as u32 + 1), msf(index1.saturating_sub(150)), msf(index1)));
    }
    out[0x800..0x800 + toc.len()].copy_from_slice(&toc);
    // Blocks of 16 sectors, the last padded with zeros, each raw-deflated.
    let block = 16 * CD_SECTOR;
    let mut data = Vec::new();
    let mut index = Vec::new();
    for chunk in image.chunks(block) {
        let mut full = chunk.to_vec();
        full.resize(block, 0);
        let packed = zlib_ng::raw_deflate(&full, 9).unwrap();
        let stored = if packed.len() < block { packed } else { full };
        index.extend_from_slice(&(data.len() as u32).to_le_bytes());
        index.extend_from_slice(&(stored.len() as u16).to_le_bytes());
        index.extend_from_slice(&1u16.to_le_bytes());
        index.extend_from_slice(&[0u8; 24]);
        data.extend_from_slice(&stored);
    }
    // The entry after the last block has no size, and its offset is where the data ends.
    index.extend_from_slice(&(data.len() as u32).to_le_bytes());
    index.extend_from_slice(&[0u8; 28]);
    out[0x4000..0x4000 + index.len()].copy_from_slice(&index);
    out.extend_from_slice(&data);
    out
}

pub fn pbp_file(path: &Path, psar: &[u8]) {
    let mut out = Vec::new();
    out.extend_from_slice(b"\0PBP");
    out.extend_from_slice(&0x10000u32.to_le_bytes());
    let psar_at = 0x100u32;
    for _ in 0..7 {
        out.extend_from_slice(&0x28u32.to_le_bytes());
    }
    out.extend_from_slice(&psar_at.to_le_bytes());
    out.resize(psar_at as usize, 0);
    out.extend_from_slice(psar);
    std::fs::write(path, out).unwrap();
}

pub enum Node {
    File(&'static str, Vec<u8>),
    Dir(&'static str, Vec<Node>),
}

/// An XDVDFS volume: sector 32 is the volume descriptor, then the directory tables and the
/// file data. Each directory's entries are chained through their right-child links, a
/// degenerate but valid tree, in name order as a real disc sorts them.
pub fn volume(root: &[Node]) -> Vec<u8> {
    let mut sectors: Vec<Vec<u8>> = Vec::new(); // index = sector - 33
    fn place(nodes: &[Node], sectors: &mut Vec<Vec<u8>>) -> (u32, u32) {
        let mut sorted: Vec<&Node> = nodes.iter().collect();
        let name = |n: &Node| match n {
            Node::File(n, _) | Node::Dir(n, _) => n.to_ascii_uppercase(),
        };
        sorted.sort_by_key(|n| name(n));
        // Reserve this table's sector first, then lay out what it points to.
        let table_index = sectors.len();
        sectors.push(Vec::new());
        let mut table = Vec::new();
        for (i, n) in sorted.iter().enumerate() {
            let (nm, attr, start, size) = match n {
                Node::File(nm, data) => {
                    let start = 33 + sectors.len() as u32;
                    let count = data.len().div_ceil(SECTOR).max(1);
                    for c in 0..count {
                        let mut s = data[(c * SECTOR).min(data.len())..((c + 1) * SECTOR).min(data.len())].to_vec();
                        s.resize(SECTOR, 0);
                        sectors.push(s);
                    }
                    (*nm, 0x20u8, if data.is_empty() { 0 } else { start }, data.len() as u32)
                }
                Node::Dir(nm, kids) => {
                    let (s, z) = place(kids, sectors);
                    (*nm, 0x10u8, s, z)
                }
            };
            let entry_len = (14 + nm.len() + 3) & !3;
            let next = if i + 1 < sorted.len() { ((table.len() + entry_len) / 4) as u16 } else { 0 };
            table.extend_from_slice(&0u16.to_le_bytes());
            table.extend_from_slice(&next.to_le_bytes());
            table.extend_from_slice(&start.to_le_bytes());
            table.extend_from_slice(&size.to_le_bytes());
            table.push(attr);
            table.push(nm.len() as u8);
            table.extend_from_slice(nm.as_bytes());
            while table.len() % 4 != 0 {
                table.push(0xFF);
            }
        }
        assert!(table.len() <= SECTOR, "the test builder keeps each table in one sector");
        let size = table.len() as u32;
        table.resize(SECTOR, 0xFF);
        sectors[table_index] = table;
        (33 + table_index as u32, size)
    }
    let (root_sector, root_size) = place(root, &mut sectors);
    let mut out = vec![0u8; 32 * SECTOR];
    let mut vd = vec![0u8; SECTOR];
    vd[..20].copy_from_slice(b"MICROSOFT*XBOX*MEDIA");
    vd[0x14..0x18].copy_from_slice(&root_sector.to_le_bytes());
    vd[0x18..0x1C].copy_from_slice(&root_size.to_le_bytes());
    vd[0x7EC..0x800].copy_from_slice(b"MICROSOFT*XBOX*MEDIA");
    out.extend_from_slice(&vd);
    for s in sectors {
        out.extend_from_slice(&s);
    }
    out
}

pub fn tree(boot: &'static str) -> Vec<Node> {
    vec![
        Node::File(boot, pattern(300_000, 1)),
        Node::Dir("media", vec![Node::File("a.bin", pattern(1 << 20, 2)), Node::File("empty.txt", Vec::new()), Node::Dir("sub", vec![Node::File("b.txt", b"hello ".repeat(1000))])]),
        Node::File("z.dat", pattern(5000, 3)),
    ]
}

/// A Redump-shaped image: filler at the start, the volume at `base`, padding after it.
pub fn redump_image(path: &Path, base: u64, boot: &'static str) {
    let mut f = File::create(path).unwrap();
    f.write_all(&pattern(64 * 1024, 9)).unwrap();
    f.seek(SeekFrom::Start(base)).unwrap();
    let v = volume(&tree(boot));
    f.write_all(&v).unwrap();
    f.set_len(base + v.len() as u64 + 1024 * 1024).unwrap();
}

