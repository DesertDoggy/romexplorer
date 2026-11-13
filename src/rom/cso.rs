//! CSO (`CISO` v1): an ISO cut into fixed-size blocks, each raw-deflated or, when that would not
//! make it smaller, stored as it is. Lossless -- the ISO comes back byte for byte -- and read by
//! PCSX2 and PPSSPP. Both directions go through the zlib-ng this project ships
//! ([`super::zlib_ng`]).
//!
//! Layout: a 24-byte header (`CISO`, header size, ISO size, block size, version 1, index
//! shift), then `blocks + 1` little-endian u32 index entries -- each block's start in the file,
//! shifted right by the index shift, with the top bit set for a block stored uncompressed --
//! and the blocks. The last entry marks where the last block ends.
//!
//! [`CsoReader`] reads by random access ([`super::source::ReadAt`]), so a scan decodes a CSO on
//! several threads straight into hashing, never writing the ISO out.

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use rayon::prelude::*;

use super::source::ReadAt;
use super::zlib_ng;

pub const MAGIC: &[u8; 4] = b"CISO";
/// ZSO: the same layout, its blocks LZ4-compressed (PPSSPP and PCSX2 read it).
pub const ZSO_MAGIC: &[u8; 4] = b"ZISO";
const HEADER: usize = 24;
const PLAIN: u32 = 0x8000_0000;
/// PSP and PS2 sectors are 2048 bytes; PPSSPP reads only this block size.
pub const BLOCK: u32 = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub iso_size: u64,
    pub block_size: u32,
    pub version: u8,
    pub shift: u8,
}

fn bad(what: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, what.into())
}

/// Whether `head` (the file's first bytes) is a CSO header.
pub fn is_cso(head: &[u8]) -> bool {
    head.len() >= 4 && &head[..4] == MAGIC
}

fn parse_header(h: &[u8; HEADER]) -> std::io::Result<Header> {
    if !is_cso(h) && &h[..4] != ZSO_MAGIC {
        return Err(bad("not a CSO or ZSO file"));
    }
    let header = Header {
        iso_size: u64::from_le_bytes(h[8..16].try_into().unwrap()),
        block_size: u32::from_le_bytes(h[16..20].try_into().unwrap()),
        version: h[20],
        shift: h[21],
    };
    if header.version > 1 {
        return Err(bad(format!("CSO version {} is not supported (only version 1)", header.version)));
    }
    if header.block_size == 0 || header.block_size % 2048 != 0 || header.block_size > 1 << 20 || header.shift > 16 {
        return Err(bad("the CSO header is damaged"));
    }
    Ok(header)
}

/// A CSO opened for random reads of the ISO it holds.
pub struct CsoReader {
    file: File,
    pub header: Header,
    /// A ZSO: blocks are LZ4, not deflate.
    lz4: bool,
    index: Vec<u32>,
    file_len: u64,
    /// The last block decoded, for reads smaller than a block.
    cached: Option<(u64, Vec<u8>)>,
    raw: Vec<u8>,
}

impl CsoReader {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let mut file = File::open(path)?;
        let file_len = file.metadata()?.len();
        let mut h = [0u8; HEADER];
        file.read_exact(&mut h)?;
        let header = parse_header(&h)?;
        let blocks = header.iso_size.div_ceil(header.block_size as u64);
        // The index lies whole in the file: checked before it is allocated, so a damaged or
        // made-up iso_size can't ask for gigabytes.
        let index_len = blocks.checked_add(1).and_then(|n| n.checked_mul(4)).filter(|&n| (HEADER as u64).saturating_add(n) <= file_len).ok_or_else(|| bad("the CSO header is damaged (its index is longer than the file)"))?;
        let mut index_bytes = vec![0u8; index_len as usize];
        file.read_exact(&mut index_bytes)?;
        let index: Vec<u32> = index_bytes.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
        Ok(Self { file, header, lz4: &h[..4] == ZSO_MAGIC, index, file_len, cached: None, raw: Vec::new() })
    }

    /// The ISO's size.
    pub fn size(&self) -> u64 {
        self.header.iso_size
    }

    fn block(&mut self, n: u64) -> std::io::Result<&[u8]> {
        if self.cached.as_ref().is_some_and(|(at, _)| *at == n) {
            return Ok(&self.cached.as_ref().unwrap().1);
        }
        let bs = self.header.block_size as u64;
        let want = (self.header.iso_size - n * bs).min(bs) as usize;
        let entry = *self.index.get(n as usize).ok_or_else(|| bad("read past the end of the CSO"))?;
        let next = *self.index.get(n as usize + 1).ok_or_else(|| bad("the CSO index is cut short"))?;
        let start = ((entry & !PLAIN) as u64) << self.header.shift;
        let end = (((next & !PLAIN) as u64) << self.header.shift).min(self.file_len);
        if end < start {
            return Err(bad(format!("block {n}: the CSO index is damaged")));
        }
        self.raw.resize((end - start) as usize, 0);
        self.file.seek(SeekFrom::Start(start))?;
        self.file.read_exact(&mut self.raw)?;
        let data = if entry & PLAIN != 0 {
            if self.raw.len() < want {
                return Err(bad(format!("block {n}: cut short")));
            }
            self.raw[..want].to_vec()
        } else if self.lz4 {
            let mut out = lz4_block(&self.raw, bs as usize).ok_or_else(|| bad(format!("block {n}: does not decompress -- the ZSO may be damaged")))?;
            if out.len() < want {
                return Err(bad(format!("block {n}: cut short")));
            }
            out.truncate(want);
            out
        } else {
            // Blocks may carry alignment padding after the deflate stream; inflate stops at its end.
            let mut out = zlib_ng::raw_inflate(&self.raw, bs as usize).or_else(|| zlib_ng::raw_inflate(&self.raw, want));
            if out.is_none() && !zlib_ng::available() {
                return Err(std::io::Error::other("the zlib-ng library is not built for this machine"));
            }
            let mut out = out.take().ok_or_else(|| bad(format!("block {n}: does not decompress -- the CSO may be damaged")))?;
            out.truncate(want);
            out
        };
        self.cached = Some((n, data));
        Ok(&self.cached.as_ref().unwrap().1)
    }
}

/// Decodes one LZ4 block (the raw block format, no frame) of at most `max` bytes. Input after
/// the last sequence (a ZSO block's alignment padding) is left alone. Small and only ever
/// run over 2 KB blocks, so it is written here rather than taking a library in.
fn lz4_block(src: &[u8], max: usize) -> Option<Vec<u8>> {
    let mut out: Vec<u8> = Vec::with_capacity(max);
    let mut i = 0;
    let length = |i: &mut usize, mut n: usize| -> Option<usize> {
        if n == 15 {
            loop {
                let b = *src.get(*i)?;
                *i += 1;
                n += b as usize;
                if b != 255 {
                    break;
                }
            }
        }
        Some(n)
    };
    while i < src.len() && out.len() < max {
        let token = src[i];
        i += 1;
        let lit = length(&mut i, (token >> 4) as usize)?;
        let lits = src.get(i..i.checked_add(lit)?)?;
        out.extend_from_slice(lits);
        i += lit;
        if out.len() >= max || i + 2 > src.len() {
            break; // the last sequence is literals only
        }
        let offset = u16::from_le_bytes([src[i], src[i + 1]]) as usize;
        i += 2;
        if offset == 0 && src[i - 2..].iter().all(|&b| b == 0) {
            break; // alignment padding after the last literals
        }
        if offset == 0 || offset > out.len() {
            return None;
        }
        let len = length(&mut i, (token & 15) as usize)? + 4;
        if out.len() + len > max {
            return None;
        }
        let from = out.len() - offset;
        for k in 0..len {
            out.push(out[from + k]);
        }
    }
    (out.len() <= max).then_some(out)
}

impl ReadAt for CsoReader {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        if offset + buf.len() as u64 > self.header.iso_size {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "read past the end of the ISO"));
        }
        let bs = self.header.block_size as u64;
        let mut done = 0usize;
        while done < buf.len() {
            let at = offset + done as u64;
            let n = at / bs;
            let within = (at % bs) as usize;
            let block = self.block(n)?;
            let take = (block.len() - within).min(buf.len() - done);
            buf[done..done + take].copy_from_slice(&block[within..within + take]);
            done += take;
        }
        Ok(())
    }
}

/// Writes `iso` as a CSO at `out` (created or replaced), compressing blocks on all cores.
/// `level` is the deflate level (9 is what CSO tools use). `progress(done, total)` gets ISO
/// bytes compressed; returning false cancels and removes the output.
pub fn write(iso: &Path, out: &Path, level: i32, mut progress: impl FnMut(u64, u64) -> bool) -> Result<(), String> {
    if !zlib_ng::available() {
        return Err("the zlib-ng library is not built for this machine".into());
    }
    let shown = iso.display().to_string();
    let mut input = File::open(iso).map_err(|e| format!("{shown}: {e}"))?;
    let size = input.metadata().map_err(|e| format!("{shown}: {e}"))?.len();
    let bs = BLOCK as u64;
    let blocks = size.div_ceil(bs);
    let index_len = (blocks + 1) * 4;
    // Index entries are 31 bits: past 2 GiB of output, positions are stored shifted, and each
    // block starts on a multiple of 1 << shift.
    let worst = HEADER as u64 + index_len + size + blocks;
    let mut shift = 0u8;
    while worst >> shift >= PLAIN as u64 {
        shift += 1;
    }
    let align = 1u64 << shift;

    let result = (|| -> Result<(), String> {
        let file = File::create(out).map_err(|e| format!("{}: {e}", out.display()))?;
        let mut w = BufWriter::with_capacity(4 << 20, file);
        let mut header = [0u8; HEADER];
        header[..4].copy_from_slice(MAGIC);
        header[4..8].copy_from_slice(&(HEADER as u32).to_le_bytes());
        header[8..16].copy_from_slice(&size.to_le_bytes());
        header[16..20].copy_from_slice(&BLOCK.to_le_bytes());
        header[20] = 1;
        header[21] = shift;
        w.write_all(&header).map_err(|e| e.to_string())?;
        w.write_all(&vec![0u8; index_len as usize]).map_err(|e| e.to_string())?;
        let mut pos = HEADER as u64 + index_len;
        let mut index: Vec<u32> = Vec::with_capacity(blocks as usize + 1);
        let batch_blocks = 8192usize; // 16 MiB of ISO per batch
        let mut buf = vec![0u8; batch_blocks * BLOCK as usize];
        let mut done = 0u64;
        while done < size {
            let n = ((size - done) as usize).min(buf.len());
            input.read_exact(&mut buf[..n]).map_err(|e| format!("{shown}: {e}"))?;
            let packed: Vec<Option<Vec<u8>>> = buf[..n].par_chunks(BLOCK as usize).map(|block| zlib_ng::raw_deflate(block, level).filter(|c| c.len() < block.len())).collect();
            for (block, packed) in buf[..n].chunks(BLOCK as usize).zip(packed) {
                let pad = (align - pos % align) % align;
                if pad > 0 {
                    w.write_all(&vec![0u8; pad as usize]).map_err(|e| e.to_string())?;
                    pos += pad;
                }
                let entry = (pos >> shift) as u32;
                match packed {
                    Some(c) => {
                        index.push(entry);
                        w.write_all(&c).map_err(|e| e.to_string())?;
                        pos += c.len() as u64;
                    }
                    None => {
                        index.push(entry | PLAIN);
                        w.write_all(block).map_err(|e| e.to_string())?;
                        pos += block.len() as u64;
                    }
                }
            }
            done += n as u64;
            if !progress(done, size) {
                return Err("cancelled".into());
            }
        }
        let pad = (align - pos % align) % align;
        w.write_all(&vec![0u8; pad as usize]).map_err(|e| e.to_string())?;
        pos += pad;
        index.push((pos >> shift) as u32);
        let mut file = w.into_inner().map_err(|e| e.to_string())?;
        file.seek(SeekFrom::Start(HEADER as u64)).map_err(|e| e.to_string())?;
        let bytes: Vec<u8> = index.iter().flat_map(|e| e.to_le_bytes()).collect();
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        file.flush().map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(out);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lz4_blocks_decode_literals_and_overlapping_matches() {
        // "abc", then a match of 9 at offset 3 ("abcabcabc"), then "xyz" as the last literals.
        let block = [0x35, b'a', b'b', b'c', 3, 0, 0x30, b'x', b'y', b'z', 0, 0];
        assert_eq!(lz4_block(&block, 2048).unwrap(), b"abcabcabcabcxyz");
        assert_eq!(lz4_block(&block[..10], 2048).unwrap(), b"abcabcabcabcxyz");
        // An offset before the start, and output past the block, are refused.
        assert!(lz4_block(&[0x10, b'a', 5, 0], 2048).is_none());
        assert!(lz4_block(&[0x1f, b'a', 1, 0, 255, 255, 10], 64).is_none());
    }

    #[test]
    fn an_iso_comes_back_byte_for_byte() {
        if !zlib_ng::available() {
            eprintln!("skipped: zlib-ng is not built");
            return;
        }
        let dir = std::env::temp_dir().join(format!("romexplorer_cso_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Compressible and incompressible blocks, and a size that is not whole blocks.
        let mut iso = Vec::new();
        let mut x = 7u32;
        for i in 0..5000usize {
            for _ in 0..2048 {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                iso.push(if i % 3 == 0 { x as u8 } else { (x & 3) as u8 });
            }
        }
        iso.truncate(iso.len() - 100);
        let (src, out) = (dir.join("a.iso"), dir.join("a.cso"));
        std::fs::write(&src, &iso).unwrap();
        write(&src, &out, 9, |_, _| true).unwrap();
        let cso_len = std::fs::metadata(&out).unwrap().len();
        assert!(cso_len < iso.len() as u64, "{cso_len}");

        let mut r = CsoReader::open(&out).unwrap();
        assert_eq!(r.size(), iso.len() as u64);
        let mut back = vec![0u8; iso.len()];
        r.read_at(0, &mut back).unwrap();
        assert!(back == iso);
        let mut piece = [0u8; 5000];
        r.read_at(4_000_123, &mut piece).unwrap();
        assert_eq!(&piece[..], &iso[4_000_123..4_005_123]);
        assert!(r.read_at(iso.len() as u64 - 10, &mut [0u8; 20]).is_err());

        assert_eq!(write(&src, &out, 9, |d, _| d < 1 << 20).unwrap_err(), "cancelled");
        assert!(!out.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_header_claiming_a_huge_iso_is_refused_before_anything_is_allocated() {
        let dir = std::env::temp_dir().join(format!("romexplorer_cso_bad_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut h = vec![0u8; 1024];
        h[..4].copy_from_slice(MAGIC);
        h[8..16].copy_from_slice(&0x0000_1000_0000_0000u64.to_le_bytes());
        h[16..20].copy_from_slice(&2048u32.to_le_bytes());
        let path = dir.join("bad.cso");
        std::fs::write(&path, &h).unwrap();
        let err = CsoReader::open(&path).err().expect("refused");
        assert!(err.to_string().contains("damaged"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

