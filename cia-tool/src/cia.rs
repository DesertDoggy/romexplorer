//! CIA (CTR Importable Archive) parsing and decryption.
//!
//! Instead of rebuilding the archive with makerom like the original batch
//! script, the CIA is copied section by section: each content has its CIA
//! layer (AES-CBC with the title key) and NCCH layer removed, the TMD content
//! records are updated (encrypted flag cleared, new SHA-256), and everything
//! else is kept byte for byte. Signatures are not re-signed, which is the same
//! result as makerom's `-ignoresign`.

use crate::keys::{Key, KeyStore};
use crate::ncch::{self, NcchHeader};
use crate::seeddb::SeedDb;
use aes::cipher::{BlockDecryptMut, KeyIvInit};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

fn align(v: u64, a: u64) -> u64 {
    v.div_ceil(a) * a
}
fn be16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}
fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes(b[o..o + 4].try_into().unwrap())
}
fn be64(b: &[u8], o: usize) -> u64 {
    u64::from_be_bytes(b[o..o + 8].try_into().unwrap())
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

/// Offset of the signed data after a signature block of the given type.
pub fn signature_data_offset(sig_type: u32) -> Result<usize> {
    Ok(match sig_type {
        0x010000 | 0x010003 => 4 + 0x200 + 0x3C,
        0x010001 | 0x010004 => 4 + 0x100 + 0x3C,
        0x010002 | 0x010005 => 4 + 0x3C + 0x40,
        t => bail!("unknown signature type {t:#x}"),
    })
}

#[derive(Clone, Debug)]
pub struct Content {
    pub id: u32,
    pub index: u16,
    pub ctype: u16,
    pub size: u64,
    /// Absolute offset in the CIA file.
    pub offset: u64,
    /// Offset of this content's chunk record inside the TMD.
    pub record_pos: usize,
}

impl Content {
    pub fn encrypted(&self) -> bool {
        self.ctype & 1 != 0
    }
}

pub struct Cia {
    pub header: Vec<u8>,
    pub ticket: Vec<u8>,
    pub tmd: Vec<u8>,
    pub meta: Vec<u8>,
    pub tmd_offset: u64,
    pub content_offset: u64,
    pub content_size: u64,
    pub meta_offset: u64,
    pub file_len: u64,
    pub title_id: u64,
    pub title_version: u16,
    pub save_size: u32,
    pub tmd_body: usize,
    pub contents: Vec<Content>,
    pub enc_title_key: Key,
    pub common_key_index: u8,
    pub ticket_title_id: [u8; 8],
}

pub fn is_cia(head: &[u8]) -> bool {
    head.len() >= 0x20 && le32(head, 0) == 0x2020
}

impl Cia {
    pub fn open(f: &mut File) -> Result<Self> {
        let file_len = f.metadata()?.len();
        f.seek(SeekFrom::Start(0))?;
        let mut header = vec![0u8; 0x2020];
        f.read_exact(&mut header)
            .context("file too small to be a CIA")?;
        if !is_cia(&header) {
            bail!("not a CIA file (bad header size)");
        }
        let cert_size = le32(&header, 0x08) as u64;
        let ticket_size = le32(&header, 0x0C) as u64;
        let tmd_size = le32(&header, 0x10) as u64;
        let meta_size = le32(&header, 0x14) as u64;
        let content_size = u64::from_le_bytes(header[0x18..0x20].try_into().unwrap());

        let cert_offset = align(0x2020, 64);
        let ticket_offset = cert_offset + align(cert_size, 64);
        let tmd_offset = ticket_offset + align(ticket_size, 64);
        let content_offset = tmd_offset + align(tmd_size, 64);
        let meta_offset = content_offset + align(content_size, 64);
        if content_offset + content_size > file_len {
            bail!(
                "CIA is truncated ({} bytes, header expects at least {})",
                file_len,
                content_offset + content_size
            );
        }

        let mut read = |off: u64, size: u64| -> Result<Vec<u8>> {
            let mut v = vec![0u8; size as usize];
            f.seek(SeekFrom::Start(off))?;
            f.read_exact(&mut v)?;
            Ok(v)
        };
        let ticket = read(ticket_offset, ticket_size).context("reading ticket")?;
        let tmd = read(tmd_offset, tmd_size).context("reading TMD")?;
        let meta = if meta_size > 0 && meta_offset + meta_size <= file_len {
            read(meta_offset, meta_size)?
        } else {
            Vec::new()
        };

        // Ticket
        if ticket.len() < 4 {
            bail!("ticket is missing");
        }
        let tb = signature_data_offset(be32(&ticket, 0))?;
        if ticket.len() < tb + 0xB2 {
            bail!("ticket is too small");
        }
        let enc_title_key: Key = ticket[tb + 0x7F..tb + 0x8F].try_into().unwrap();
        let ticket_title_id: [u8; 8] = ticket[tb + 0x9C..tb + 0xA4].try_into().unwrap();
        let common_key_index = ticket[tb + 0xB1];

        // TMD
        let mb = signature_data_offset(be32(&tmd, 0))?;
        if tmd.len() < mb + 0xC4 + 0x900 {
            bail!("TMD is too small");
        }
        let title_id = be64(&tmd, mb + 0x4C);
        let save_size = le32(&tmd, mb + 0x5A);
        let title_version = be16(&tmd, mb + 0x9C);
        let count = be16(&tmd, mb + 0x9E) as usize;
        let rec_base = mb + 0xC4 + 0x900;
        if tmd.len() < rec_base + count * 0x30 {
            bail!("TMD content records are truncated");
        }

        let present = |idx: u16| -> bool {
            let byte = 0x20 + (idx as usize) / 8;
            byte < header.len() && header[byte] & (0x80 >> (idx % 8)) != 0
        };
        let mut contents = Vec::new();
        let mut cur = content_offset;
        for i in 0..count {
            let r = rec_base + i * 0x30;
            let index = be16(&tmd, r + 4);
            if !present(index) {
                continue;
            }
            let size = be64(&tmd, r + 8);
            contents.push(Content {
                id: be32(&tmd, r),
                index,
                ctype: be16(&tmd, r + 6),
                size,
                offset: cur,
                record_pos: r,
            });
            cur += size;
        }
        if cur > content_offset + content_size {
            bail!("CIA content sizes in TMD exceed the content section");
        }

        Ok(Cia {
            header,
            ticket,
            tmd,
            meta,
            tmd_offset,
            content_offset,
            content_size,
            meta_offset,
            file_len,
            title_id,
            title_version,
            save_size,
            tmd_body: mb,
            contents,
            enc_title_key,
            common_key_index,
            ticket_title_id,
        })
    }

    pub fn title_key(&self, keys: &KeyStore) -> Result<Key> {
        let ck = keys.common_key(self.common_key_index)?;
        let mut iv = [0u8; 16];
        iv[..8].copy_from_slice(&self.ticket_title_id);
        let mut k = self.enc_title_key;
        Aes128CbcDec::new(&ck.into(), &iv.into())
            .decrypt_block_mut(aes::Block::from_mut_slice(&mut k));
        Ok(k)
    }

    pub fn any_encrypted(&self) -> bool {
        self.contents.iter().any(|c| c.encrypted())
    }

    /// SMDH from the CIA meta section, if present.
    pub fn meta_smdh(&self) -> Option<&[u8]> {
        let s = self.meta.get(0x400..0x400 + 0x36C0)?;
        if &s[..4] == b"SMDH" { Some(s) } else { None }
    }
}

pub fn content_iv(index: u16) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[..2].copy_from_slice(&index.to_be_bytes());
    iv
}

fn cbc_decrypt_in_place(dec: &mut Aes128CbcDec, buf: &mut [u8]) {
    let n = buf.len() / 16;
    // SAFETY: aes::Block is GenericArray<u8, U16>, a transparent [u8; 16] with alignment 1.
    let blocks: &mut [aes::Block] =
        unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut aes::Block, n) };
    dec.decrypt_blocks_mut(blocks);
}

/// Read plaintext (CIA layer removed) bytes of a content at a random offset.
pub fn read_content_at(
    f: &mut File,
    c: &Content,
    title_key: Option<&Key>,
    off: u64,
    out: &mut [u8],
) -> Result<()> {
    if off + out.len() as u64 > c.size {
        bail!("read past end of content {}", c.index);
    }
    match (c.encrypted(), title_key) {
        (false, _) => {
            f.seek(SeekFrom::Start(c.offset + off))?;
            f.read_exact(out)?;
        }
        (true, None) => bail!("content is encrypted but no title key is available"),
        (true, Some(k)) => {
            let start = off / 16 * 16;
            let end = align(off + out.len() as u64, 16).min(c.size);
            let iv: [u8; 16] = if start == 0 {
                content_iv(c.index)
            } else {
                let mut iv = [0u8; 16];
                f.seek(SeekFrom::Start(c.offset + start - 16))?;
                f.read_exact(&mut iv)?;
                iv
            };
            let mut buf = vec![0u8; (end - start) as usize];
            f.seek(SeekFrom::Start(c.offset + start))?;
            f.read_exact(&mut buf)?;
            let mut dec = Aes128CbcDec::new(k.into(), &iv.into());
            cbc_decrypt_in_place(&mut dec, &mut buf);
            let s = (off - start) as usize;
            out.copy_from_slice(&buf[s..s + out.len()]);
        }
    }
    Ok(())
}

/// Stream one content (both layers removed) to `sink`.
pub fn stream_content(
    f: &mut File,
    c: &Content,
    title_key: Option<&Key>,
    keys: &KeyStore,
    seeds: &SeedDb,
    sink: &mut dyn FnMut(&[u8]) -> Result<()>,
) -> Result<ncch::StreamInfo> {
    f.seek(SeekFrom::Start(c.offset))?;
    let mut dec = if c.encrypted() {
        let k = title_key.context("content is encrypted but no title key is available")?;
        Some(Aes128CbcDec::new(k.into(), &content_iv(c.index).into()))
    } else {
        None
    };
    let mut src = |buf: &mut [u8]| -> Result<()> {
        f.read_exact(buf)?;
        if let Some(d) = dec.as_mut() {
            cbc_decrypt_in_place(d, buf);
        }
        Ok(())
    };
    ncch::stream_ncch(c.size, &mut src, keys, seeds, sink)
}

/// Read the first 0x200 bytes of each content (CIA layer removed).
pub fn content_headers(
    f: &mut File,
    cia: &Cia,
    title_key: Option<&Key>,
) -> Vec<Option<NcchHeader>> {
    cia.contents
        .iter()
        .map(|c| {
            if c.size < 0x200 {
                return None;
            }
            let mut b = [0u8; 0x200];
            read_content_at(f, c, title_key, 0, &mut b).ok()?;
            NcchHeader::parse(&b).ok()
        })
        .collect()
}

/// Get the SMDH titles, from the meta section or from content 0's ExeFS icon.
pub fn read_smdh(f: &mut File, cia: &Cia, keys: &KeyStore, seeds: &SeedDb) -> Option<Vec<u8>> {
    if let Some(s) = cia.meta_smdh() {
        return Some(s.to_vec());
    }
    let c = cia.contents.iter().find(|c| c.index == 0)?.clone();
    let tk = if c.encrypted() {
        Some(cia.title_key(keys).ok()?)
    } else {
        None
    };
    let mut rd = |off: u64, buf: &mut [u8]| read_content_at(f, &c, tk.as_ref(), off, buf);
    ncch::read_ncch_icon(&mut rd, keys, seeds).ok().flatten()
}

pub enum CiaOutcome {
    Decrypted,
    AlreadyDecrypted,
}

fn copy_range(src: &mut File, dst: &mut File, off: u64, len: u64) -> Result<()> {
    src.seek(SeekFrom::Start(off))?;
    let mut take = src.take(len);
    let n = std::io::copy(&mut take, dst)?;
    if n != len {
        bail!("unexpected end of file");
    }
    Ok(())
}

/// Decrypt a CIA into a new CIA file.
pub fn decrypt_cia(
    f: &mut File,
    cia: &Cia,
    out: &mut File,
    keys: &KeyStore,
    seeds: &SeedDb,
    progress: &mut dyn FnMut(u64, u64) -> bool,
) -> Result<CiaOutcome> {
    let title_key = if cia.any_encrypted() {
        Some(cia.title_key(keys)?)
    } else {
        None
    };
    if title_key.is_none() {
        let hs = content_headers(f, cia, None);
        if hs
            .iter()
            .all(|h| h.as_ref().is_none_or(|h| h.is_decrypted()))
        {
            return Ok(CiaOutcome::AlreadyDecrypted);
        }
    }

    // Everything before the contents (header, certs, ticket, TMD) as-is for now.
    copy_range(f, out, 0, cia.content_offset)?;

    let mut tmd = cia.tmd.clone();
    let total: u64 = cia.contents.iter().map(|c| c.size).sum();
    let mut done = 0u64;
    for c in &cia.contents {
        let mut hasher = Sha256::new();
        let mut sink = |b: &[u8]| -> Result<()> {
            hasher.update(b);
            out.write_all(b)?;
            done += b.len() as u64;
            if !progress(done, total) {
                return Err(crate::Cancelled.into());
            }
            Ok(())
        };
        stream_content(f, c, title_key.as_ref(), keys, seeds, &mut sink)
            .with_context(|| format!("content {:04X} ({:08x})", c.index, c.id))?;
        let r = c.record_pos;
        let t = be16(&tmd, r + 6) & !1;
        tmd[r + 6..r + 8].copy_from_slice(&t.to_be_bytes());
        tmd[r + 16..r + 48].copy_from_slice(&hasher.finalize());
    }
    update_tmd_hashes(&mut tmd, cia.tmd_body);

    // Padding after contents and the meta section, unchanged.
    let after = cia.content_offset + cia.content_contents_len();
    if cia.file_len > after {
        copy_range(f, out, after, cia.file_len - after)?;
    }
    out.seek(SeekFrom::Start(cia.tmd_offset))?;
    out.write_all(&tmd)?;
    out.flush()?;
    Ok(CiaOutcome::Decrypted)
}

impl Cia {
    fn content_contents_len(&self) -> u64 {
        self.contents.iter().map(|c| c.size).sum()
    }
}

/// Recompute the content info record hashes and the TMD header hash.
pub fn update_tmd_hashes(tmd: &mut [u8], body: usize) {
    let count = be16(tmd, body + 0x9E) as usize;
    let info_base = body + 0xC4;
    let rec_base = info_base + 0x900;
    for i in 0..64 {
        let ir = info_base + i * 0x24;
        let start = be16(tmd, ir) as usize;
        let n = be16(tmd, ir + 2) as usize;
        if n == 0 {
            continue;
        }
        let a = rec_base + start * 0x30;
        let b = rec_base + (start + n).min(count) * 0x30;
        if a >= b {
            continue;
        }
        let h = Sha256::digest(&tmd[a..b]);
        tmd[ir + 4..ir + 0x24].copy_from_slice(&h);
    }
    let h = Sha256::digest(&tmd[info_base..info_base + 0x900]);
    tmd[body + 0xA4..body + 0xC4].copy_from_slice(&h);
}

/// What kind of title this is, using the same buckets as the original script.
pub fn title_kind(title_id: u64) -> &'static str {
    let hi = (title_id >> 32) as u32;
    match hi {
        0x0004_0000 => "Game",
        0x0004_0002 => "Demo",
        0x0004_000E => "Patch",
        0x0004_008C => "DLC",
        0x0004_0010 | 0x0004_001B | 0x0004_0030 | 0x0004_009B | 0x0004_00DB | 0x0004_0130
        | 0x0004_0138 => "System",
        h if h & 0xFFFF_8000 == 0x0004_8000 => "TWL",
        _ => "Unknown",
    }
}
