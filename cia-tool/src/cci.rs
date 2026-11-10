//! CCI / NCSD (.3ds, .cci) decryption, and CIA -> CCI conversion.

use crate::cia::{self, Cia};
use crate::keys::KeyStore;
use crate::ncch::{self, MEDIA_UNIT, NcchHeader};
use crate::seeddb::SeedDb;
use anyhow::{Context, Result, bail};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

pub const PARTITION_NAMES: [&str; 8] = [
    "Main",
    "Manual",
    "DownloadPlay",
    "Partition4",
    "Partition5",
    "Partition6",
    "N3DSUpdateData",
    "UpdateData",
];

pub fn is_ncsd(head: &[u8]) -> bool {
    head.len() >= 0x200 && &head[0x100..0x104] == b"NCSD"
}

#[derive(Clone, Debug)]
pub struct Ncsd {
    pub header: Vec<u8>,
    pub media_id: u64,
    /// (index, offset, size) in bytes, for non-empty partitions, sorted by offset.
    pub partitions: Vec<(usize, u64, u64)>,
}

impl Ncsd {
    pub fn open(f: &mut File) -> Result<Self> {
        f.seek(SeekFrom::Start(0))?;
        let mut header = vec![0u8; 0x200];
        f.read_exact(&mut header)
            .context("file too small to be a CCI")?;
        if !is_ncsd(&header) {
            bail!("not a CCI/NCSD file (missing NCSD magic)");
        }
        let media_id = u64::from_le_bytes(header[0x108..0x110].try_into().unwrap());
        let mut partitions = Vec::new();
        for i in 0..8 {
            let o = 0x120 + i * 8;
            let off = u32::from_le_bytes(header[o..o + 4].try_into().unwrap()) as u64 * MEDIA_UNIT;
            let size =
                u32::from_le_bytes(header[o + 4..o + 8].try_into().unwrap()) as u64 * MEDIA_UNIT;
            if size > 0 {
                partitions.push((i, off, size));
            }
        }
        partitions.sort_by_key(|p| p.1);
        Ok(Ncsd {
            header,
            media_id,
            partitions,
        })
    }

    pub fn used_size(&self) -> u64 {
        self.partitions
            .iter()
            .map(|p| p.1 + p.2)
            .max()
            .unwrap_or(0x4000)
    }

    pub fn partition_header(&self, f: &mut File, idx: usize) -> Option<NcchHeader> {
        let (_, off, _) = *self.partitions.iter().find(|p| p.0 == idx)?;
        let mut b = [0u8; 0x200];
        f.seek(SeekFrom::Start(off)).ok()?;
        f.read_exact(&mut b).ok()?;
        NcchHeader::parse(&b).ok()
    }
}

pub fn read_smdh(f: &mut File, ncsd: &Ncsd, keys: &KeyStore, seeds: &SeedDb) -> Option<Vec<u8>> {
    let (_, base, _) = *ncsd.partitions.iter().find(|p| p.0 == 0)?;
    let mut rd = |off: u64, buf: &mut [u8]| -> Result<()> {
        f.seek(SeekFrom::Start(base + off))?;
        f.read_exact(buf)?;
        Ok(())
    };
    ncch::read_ncch_icon(&mut rd, keys, seeds).ok().flatten()
}

fn copy_range(src: &mut File, dst: &mut File, off: u64, len: u64) -> Result<()> {
    src.seek(SeekFrom::Start(off))?;
    let n = std::io::copy(&mut src.take(len), dst)?;
    if n != len {
        bail!("unexpected end of file");
    }
    Ok(())
}

pub enum CciOutcome {
    Decrypted,
    AlreadyDecrypted,
}

/// Decrypt every NCCH partition of a CCI. With `trim`, data after the last
/// partition (unused cartridge space) is dropped, like the original script.
pub fn decrypt_cci(
    f: &mut File,
    ncsd: &Ncsd,
    out: &mut File,
    trim: bool,
    keys: &KeyStore,
    seeds: &SeedDb,
    progress: &mut dyn FnMut(u64, u64) -> bool,
) -> Result<CciOutcome> {
    let file_len = f.metadata()?.len();
    let all_plain = ncsd.partitions.iter().all(|p| {
        ncsd.partition_header(f, p.0)
            .is_none_or(|h| h.is_decrypted())
    });
    if all_plain {
        return Ok(CciOutcome::AlreadyDecrypted);
    }
    let first = ncsd
        .partitions
        .first()
        .map(|p| p.1)
        .unwrap_or(file_len)
        .min(file_len);
    // Header area (NCSD header, card info, initial data with a copy of the
    // first NCCH header).
    f.seek(SeekFrom::Start(0))?;
    let mut head = vec![0u8; first as usize];
    f.read_exact(&mut head)?;
    if head.len() >= 0x1200
        && &head[0x1100..0x1104] == b"NCCH"
        && let Some(h) = ncsd.partition_header(f, 0)
        && !h.is_decrypted()
    {
        NcchHeader::mark_decrypted_at(&mut head[0x1100..0x1200], 0x88);
    }
    out.write_all(&head)?;
    let total: u64 = ncsd.partitions.iter().map(|p| p.2).sum();
    let mut done = 0u64;
    let mut pos = first;
    for &(idx, off, size) in &ncsd.partitions {
        if off > pos {
            copy_range(f, out, pos, off - pos)?;
        }
        if off + size > file_len {
            bail!("partition {idx} goes past the end of the file (truncated dump?)");
        }
        f.seek(SeekFrom::Start(off))?;
        let mut src = |b: &mut [u8]| -> Result<()> { Ok(f.read_exact(b)?) };
        let mut sink = |b: &[u8]| -> Result<()> {
            out.write_all(b)?;
            done += b.len() as u64;
            if !progress(done, total) {
                return Err(crate::Cancelled.into());
            }
            Ok(())
        };
        ncch::stream_ncch(size, &mut src, keys, seeds, &mut sink)
            .with_context(|| format!("partition {idx} ({})", PARTITION_NAMES[idx]))?;
        pos = off + size;
    }
    if !trim && file_len > pos {
        copy_range(f, out, pos, file_len - pos)?;
    }
    out.flush()?;
    Ok(CciOutcome::Decrypted)
}

/// Build a decrypted CCI from a (possibly encrypted) CIA game title.
/// Content index N becomes partition N (0 = game, 1 = manual, 2 = download play).
pub fn cia_to_cci(
    f: &mut File,
    cia: &Cia,
    out: &mut File,
    keys: &KeyStore,
    seeds: &SeedDb,
    progress: &mut dyn FnMut(u64, u64) -> bool,
) -> Result<()> {
    let kind = cia::title_kind(cia.title_id);
    if kind != "Game" {
        bail!("converting to CCI is only supported for game titles (this is {kind})");
    }
    let title_key = if cia.any_encrypted() {
        Some(cia.title_key(keys)?)
    } else {
        None
    };
    let parts: Vec<&cia::Content> = cia.contents.iter().filter(|c| c.index < 8).collect();
    if !parts.iter().any(|c| c.index == 0) {
        bail!("CIA has no main content (index 0)");
    }

    const HEADER_AREA: u64 = 0x4000;
    out.write_all(&vec![0u8; HEADER_AREA as usize])?;
    let mut table = [(0u64, 0u64); 8];
    let mut ids = [0u64; 8];
    let mut first_ncch_header: Option<Vec<u8>> = None;
    let mut pos = HEADER_AREA;
    let total: u64 = parts.iter().map(|c| c.size).sum();
    let mut done = 0u64;
    for c in parts {
        let mut written = 0u64;
        let mut sink = |b: &[u8]| -> Result<()> {
            out.write_all(b)?;
            written += b.len() as u64;
            done += b.len() as u64;
            if !progress(done, total) {
                return Err(crate::Cancelled.into());
            }
            Ok(())
        };
        let info = cia::stream_content(f, c, title_key.as_ref(), keys, seeds, &mut sink)
            .with_context(|| format!("content {:04X}", c.index))?;
        let Some(h) = info.header else {
            bail!(
                "content {} is not an NCCH; cannot place it in a CCI",
                c.index
            );
        };
        // Pad to a media unit boundary.
        let padded = written.div_ceil(MEDIA_UNIT) * MEDIA_UNIT;
        if padded > written {
            out.write_all(&vec![0u8; (padded - written) as usize])?;
        }
        let i = c.index as usize;
        table[i] = (pos, padded);
        ids[i] = h.partition_id;
        if i == 0 {
            first_ncch_header = Some(info.first_block.clone());
        }
        pos += padded;
    }
    let used = pos;

    // NCSD header
    let mut hdr = vec![0u8; HEADER_AREA as usize];
    hdr[0x100..0x104].copy_from_slice(b"NCSD");
    let mut media = 128u64 << 20;
    while media < used {
        media *= 2;
    }
    hdr[0x104..0x108].copy_from_slice(&((media / MEDIA_UNIT) as u32).to_le_bytes());
    hdr[0x108..0x110].copy_from_slice(&cia.title_id.to_le_bytes());
    for i in 0..8 {
        let o = 0x120 + i * 8;
        hdr[o..o + 4].copy_from_slice(&((table[i].0 / MEDIA_UNIT) as u32).to_le_bytes());
        hdr[o + 4..o + 8].copy_from_slice(&((table[i].1 / MEDIA_UNIT) as u32).to_le_bytes());
        hdr[0x190 + i * 8..0x198 + i * 8].copy_from_slice(&ids[i].to_le_bytes());
    }
    // Partition flags: backup write wait 0, card device NOR flash, platform CTR,
    // media type Card1 (Card2 for large saves), media unit size 0x200.
    let card2 = cia.save_size >= 1 << 20;
    hdr[0x188 + 3] = if card2 { 2 } else { 1 };
    hdr[0x188 + 4] = 1;
    hdr[0x188 + 5] = if card2 { 2 } else { 1 };
    // Card info header
    let writable: u32 = if card2 {
        (used.div_ceil(MEDIA_UNIT)) as u32
    } else {
        0xFFFF_FFFF
    };
    hdr[0x200..0x204].copy_from_slice(&writable.to_le_bytes());
    hdr[0x300..0x304].copy_from_slice(&(used.min(u32::MAX as u64) as u32).to_le_bytes());
    hdr[0x310..0x312].copy_from_slice(&cia.title_version.to_le_bytes());
    // Initial data: copy of the first NCCH header (without signature) at 0x1100.
    if let Some(h0) = first_ncch_header {
        hdr[0x1100..0x1200].copy_from_slice(&h0[0x100..0x200]);
    }
    out.seek(SeekFrom::Start(0))?;
    out.write_all(&hdr)?;
    out.flush()?;
    Ok(())
}
