//! NCCH (CXI/CFA) parsing and in-stream decryption.
//!
//! Encrypted regions (all AES-128-CTR, counter = partition id << 64 | type << 56):
//! * ExHeader (type 1): main key (slot 0x2C)
//! * ExeFS header and the "icon"/"banner" files (type 2): main key
//! * Every other ExeFS file, e.g. ".code" (type 2): secondary key
//! * RomFS (type 3): secondary key
//!
//! The secondary key slot comes from flags[3]; seeded titles hash a seed into
//! the secondary KeyY. Fixed-key titles use an all-zero key (or the fixed
//! system key for system titles).

use crate::keys::{Key, KeyStore};
use crate::seeddb::SeedDb;
use aes::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
use anyhow::{Result, bail};
use sha2::{Digest, Sha256};

pub const MEDIA_UNIT: u64 = 0x200;
pub const FLAG_FIXED_KEY: u8 = 0x01;
pub const FLAG_NO_ROMFS: u8 = 0x02;
pub const FLAG_NO_CRYPTO: u8 = 0x04;
pub const FLAG_SEED: u8 = 0x20;

type Aes128Ctr = ctr::Ctr128BE<aes::Aes128>;

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn le64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

#[derive(Clone, Debug)]
pub struct NcchHeader {
    pub key_y: Key,
    pub content_size: u64,
    pub partition_id: u64,
    pub maker_code: String,
    pub version: u16,
    pub seed_check: [u8; 4],
    pub program_id: u64,
    pub product_code: String,
    pub exheader_size: u32,
    pub flags: [u8; 8],
    pub exefs_offset: u64,
    pub exefs_size: u64,
    pub romfs_offset: u64,
    pub romfs_size: u64,
}

pub fn is_ncch(buf: &[u8]) -> bool {
    buf.len() >= 0x200 && &buf[0x100..0x104] == b"NCCH"
}

impl NcchHeader {
    pub fn parse(h: &[u8]) -> Result<Self> {
        if !is_ncch(h) {
            bail!("not an NCCH container (missing NCCH magic)");
        }
        let ascii = |r: std::ops::Range<usize>| {
            String::from_utf8_lossy(&h[r])
                .trim_end_matches('\0')
                .to_string()
        };
        Ok(NcchHeader {
            key_y: h[0..16].try_into().unwrap(),
            content_size: le32(h, 0x104) as u64 * MEDIA_UNIT,
            partition_id: le64(h, 0x108),
            maker_code: ascii(0x110..0x112),
            version: u16::from_le_bytes([h[0x112], h[0x113]]),
            seed_check: h[0x114..0x118].try_into().unwrap(),
            program_id: le64(h, 0x118),
            product_code: ascii(0x150..0x160),
            exheader_size: le32(h, 0x180),
            flags: h[0x188..0x190].try_into().unwrap(),
            exefs_offset: le32(h, 0x1A0) as u64 * MEDIA_UNIT,
            exefs_size: le32(h, 0x1A4) as u64 * MEDIA_UNIT,
            romfs_offset: le32(h, 0x1B0) as u64 * MEDIA_UNIT,
            romfs_size: le32(h, 0x1B4) as u64 * MEDIA_UNIT,
        })
    }

    pub fn is_decrypted(&self) -> bool {
        self.flags[7] & FLAG_NO_CRYPTO != 0
    }

    pub fn uses_seed(&self) -> bool {
        self.flags[7] & FLAG_SEED != 0
    }

    pub fn uses_fixed_key(&self) -> bool {
        self.flags[7] & FLAG_FIXED_KEY != 0
    }

    pub fn crypto_description(&self) -> String {
        if self.is_decrypted() {
            return "None (decrypted)".into();
        }
        if self.uses_fixed_key() {
            return if self.program_id & (0x10 << 32) != 0 {
                "Fixed system key".into()
            } else {
                "Fixed zero key".into()
            };
        }
        let base = match self.flags[3] {
            0x00 => "Secure1 (slot 0x2C)",
            0x01 => "Secure2 (slot 0x25, 7.x)",
            0x0A => "Secure3 (slot 0x18, New 3DS 9.3)",
            0x0B => "Secure4 (slot 0x1B, New 3DS 9.6)",
            _ => "Unknown",
        };
        if self.uses_seed() {
            format!("{base} + seed")
        } else {
            base.into()
        }
    }

    /// Patch a raw 0x200-byte NCCH header (or a copy of bytes 0x100..0x200 at
    /// `flags_at` = 0x88) so it says "no crypto".
    pub fn mark_decrypted_at(raw: &mut [u8], flags_at: usize) {
        raw[flags_at + 3] = 0;
        raw[flags_at + 7] = (raw[flags_at + 7] & !(FLAG_FIXED_KEY | FLAG_SEED)) | FLAG_NO_CRYPTO;
    }
}

/// Keys and region layout needed to decrypt one NCCH.
pub struct NcchCrypto {
    main: Key,
    extra: Key,
    partition_id: u64,
    exheader: Option<(u64, u64)>,
    exefs: Option<(u64, u64)>,
    romfs: Option<(u64, u64)>,
    exefs_hdr: [u8; 0x200],
    exefs_hdr_filled: usize,
    /// Absolute ranges inside the ExeFS that use the secondary key.
    exefs_extra: Option<Vec<(u64, u64)>>,
}

impl NcchCrypto {
    pub fn new(h: &NcchHeader, keys: &KeyStore, seeds: &SeedDb) -> Result<Self> {
        let (main, extra) = if h.uses_fixed_key() {
            let k = if h.program_id & (0x10 << 32) != 0 {
                match keys.fixed_system {
                    Some(k) => k,
                    None => bail!(
                        "system title uses the fixed system key; add fixedSys=... to aes_keys.txt"
                    ),
                }
            } else {
                [0u8; 16]
            };
            (k, k)
        } else {
            let main = keys.ncch_key(0x2C, &h.key_y)?;
            let slot = match h.flags[3] {
                0x00 => 0x2C,
                0x01 => 0x25,
                0x0A => 0x18,
                0x0B => 0x1B,
                x => bail!("unknown NCCH crypto method {x:#04x}"),
            };
            let mut key_y = h.key_y;
            if h.uses_seed() {
                let Some(seed) = seeds.get(h.program_id) else {
                    bail!(
                        "title {:016X} uses seed crypto but its seed is not in seeddb.bin",
                        h.program_id
                    );
                };
                let mut v = Sha256::new();
                v.update(seed);
                v.update(h.program_id.to_le_bytes());
                if v.finalize()[..4] != h.seed_check {
                    bail!(
                        "seed for {:016X} in seeddb.bin does not match the title",
                        h.program_id
                    );
                }
                let mut s = Sha256::new();
                s.update(h.key_y);
                s.update(seed);
                key_y = s.finalize()[..16].try_into().unwrap();
            }
            (main, keys.ncch_key(slot, &key_y)?)
        };
        if h.version == 1 {
            bail!("NCCH version 1 (early development format) is not supported");
        }
        let region = |o: u64, s: u64| if s > 0 { Some((o, o + s)) } else { None };
        Ok(NcchCrypto {
            main,
            extra,
            partition_id: h.partition_id,
            exheader: if h.exheader_size > 0 {
                Some((0x200, 0xA00))
            } else {
                None
            },
            exefs: region(h.exefs_offset, h.exefs_size),
            romfs: if h.flags[7] & FLAG_NO_ROMFS != 0 {
                None
            } else {
                region(h.romfs_offset, h.romfs_size)
            },
            exefs_hdr: [0; 0x200],
            exefs_hdr_filled: 0,
            exefs_extra: None,
        })
    }

    fn iv(&self, ty: u8) -> u128 {
        ((self.partition_id as u128) << 64) | ((ty as u128) << 56)
    }

    /// XOR the keystream for region (`region_start`, counter `iv`) into the
    /// part of `buf` (which starts at NCCH offset `off`) that lies in [lo, hi).
    fn apply(key: &Key, iv: u128, region_start: u64, off: u64, buf: &mut [u8], lo: u64, hi: u64) {
        let end = off + buf.len() as u64;
        let lo = lo.max(off);
        let hi = hi.min(end);
        if lo >= hi {
            return;
        }
        let slice = &mut buf[(lo - off) as usize..(hi - off) as usize];
        let mut c = Aes128Ctr::new(key.into(), &iv.to_be_bytes().into());
        c.seek(lo - region_start);
        c.apply_keystream(slice);
    }

    fn parse_exefs_table(&mut self, exefs_start: u64) {
        let mut ranges = Vec::new();
        for i in 0..10 {
            let e = &self.exefs_hdr[i * 16..i * 16 + 16];
            let name: Vec<u8> = e[..8].iter().copied().take_while(|&c| c != 0).collect();
            let off = le32(e, 8) as u64;
            let size = le32(e, 12) as u64;
            if name.is_empty() || size == 0 {
                continue;
            }
            if name == b"icon" || name == b"banner" {
                continue;
            }
            let a = exefs_start + 0x200 + off;
            ranges.push((a, a + size));
        }
        self.exefs_extra = Some(ranges);
    }

    /// Decrypt `buf`, which holds NCCH bytes starting at offset `off`.
    /// Calls must come in increasing offset order so the ExeFS header is seen
    /// before the ExeFS files (which is the natural order of the file).
    pub fn process(&mut self, off: u64, buf: &mut [u8]) -> Result<()> {
        let end = off + buf.len() as u64;
        if let Some((a, b)) = self.exheader {
            Self::apply(&self.main, self.iv(1), a, off, buf, a, b);
        }
        if let Some((a, b)) = self.exefs {
            let hdr_end = (a + 0x200).min(b);
            Self::apply(&self.main, self.iv(2), a, off, buf, a, hdr_end);
            // Collect the decrypted ExeFS header (it may span several calls).
            let lo = a.max(off);
            let hi = hdr_end.min(end);
            if lo < hi && self.exefs_extra.is_none() {
                let src = &buf[(lo - off) as usize..(hi - off) as usize];
                let dst = (lo - a) as usize;
                self.exefs_hdr[dst..dst + src.len()].copy_from_slice(src);
                self.exefs_hdr_filled += src.len();
                if self.exefs_hdr_filled >= (hdr_end - a) as usize {
                    self.parse_exefs_table(a);
                }
            }
            if end > hdr_end && off < b {
                let Some(extra) = self.exefs_extra.clone() else {
                    bail!("ExeFS data reached before its header");
                };
                // Default: main key for the whole body, then override the
                // secondary-key file ranges.
                let mut cursor = hdr_end;
                let mut sorted = extra;
                sorted.sort();
                for (fa, fb) in sorted {
                    let fa = fa.clamp(hdr_end, b);
                    let fb = fb.clamp(hdr_end, b);
                    if fa > cursor {
                        Self::apply(&self.main, self.iv(2), a, off, buf, cursor, fa);
                    }
                    if fb > fa.max(cursor) {
                        Self::apply(&self.extra, self.iv(2), a, off, buf, fa.max(cursor), fb);
                    }
                    cursor = cursor.max(fb);
                }
                if cursor < b {
                    Self::apply(&self.main, self.iv(2), a, off, buf, cursor, b);
                }
            }
        }
        if let Some((a, b)) = self.romfs {
            Self::apply(&self.extra, self.iv(3), a, off, buf, a, b);
        }
        Ok(())
    }

    /// Encryption is the same operation as decryption in CTR mode; exposed for tests.
    pub fn encrypt(&mut self, off: u64, buf: &mut [u8]) -> Result<()> {
        // For encryption the ExeFS header must be parsed from plaintext first.
        if let Some((a, b)) = self.exefs {
            let hdr_end = (a + 0x200).min(b);
            if self.exefs_extra.is_none() && off <= a && off + buf.len() as u64 >= hdr_end {
                let s = (a - off) as usize;
                let n = (hdr_end - a) as usize;
                self.exefs_hdr[..n].copy_from_slice(&buf[s..s + n]);
                self.parse_exefs_table(a);
            }
        }
        self.process(off, buf)
    }
}

/// SMDH title strings (UTF-16LE in the file).
#[derive(Clone, Debug, Default)]
pub struct AppTitle {
    pub short: String,
    pub long: String,
    pub publisher: String,
}

pub const SMDH_LANGS: [&str; 12] = [
    "ja", "en", "fr", "de", "it", "es", "zh-hans", "ko", "nl", "pt", "ru", "zh-hant",
];

pub fn parse_smdh(data: &[u8]) -> Option<Vec<AppTitle>> {
    if data.len() < 0x2008 || &data[0..4] != b"SMDH" {
        return None;
    }
    let mut out = Vec::with_capacity(16);
    for i in 0..16 {
        let t = &data[8 + i * 0x200..8 + (i + 1) * 0x200];
        out.push(AppTitle {
            short: crate::text::utf16le_field(&t[0..0x80]),
            long: crate::text::utf16le_field(&t[0x80..0x180]),
            publisher: crate::text::utf16le_field(&t[0x180..0x200]),
        });
    }
    Some(out)
}

/// Read the SMDH icon from an NCCH's ExeFS. `read_at` reads plaintext (outer
/// container layer already removed) NCCH bytes at an offset.
pub fn read_ncch_icon(
    read_at: &mut dyn FnMut(u64, &mut [u8]) -> Result<()>,
    keys: &KeyStore,
    seeds: &SeedDb,
) -> Result<Option<Vec<u8>>> {
    let mut hdr = vec![0u8; 0x200];
    read_at(0, &mut hdr)?;
    let h = NcchHeader::parse(&hdr)?;
    if h.exefs_size < 0x200 {
        return Ok(None);
    }
    let mut crypto = if h.is_decrypted() {
        None
    } else {
        Some(NcchCrypto::new(&h, keys, seeds)?)
    };
    let mut eh = vec![0u8; 0x200];
    read_at(h.exefs_offset, &mut eh)?;
    if let Some(c) = crypto.as_mut() {
        c.process(h.exefs_offset, &mut eh)?;
    }
    for i in 0..10 {
        let e = &eh[i * 16..i * 16 + 16];
        if &e[..5] == b"icon\0" {
            let off = h.exefs_offset + 0x200 + le32(e, 8) as u64;
            let size = le32(e, 12) as usize;
            if size == 0 || size > 0x10000 {
                return Ok(None);
            }
            let mut icon = vec![0u8; size];
            read_at(off, &mut icon)?;
            if let Some(c) = crypto.as_mut() {
                c.process(off, &mut icon)?;
            }
            return Ok(Some(icon));
        }
    }
    Ok(None)
}

pub const CHUNK: usize = 4 * 1024 * 1024;

pub struct StreamInfo {
    pub header: Option<NcchHeader>,
    /// The first 0x200 bytes as written (patched if decrypted).
    pub first_block: Vec<u8>,
    pub decrypted_ncch: bool,
}

/// Stream `len` bytes from `src` (which yields plaintext with any outer layer
/// removed) to `sink`, decrypting the NCCH inside if there is one.
pub fn stream_ncch(
    len: u64,
    src: &mut dyn FnMut(&mut [u8]) -> Result<()>,
    keys: &KeyStore,
    seeds: &SeedDb,
    sink: &mut dyn FnMut(&[u8]) -> Result<()>,
) -> Result<StreamInfo> {
    let mut buf = vec![0u8; CHUNK.min(len.max(1) as usize)];
    let mut off = 0u64;
    let mut crypto: Option<NcchCrypto> = None;
    let mut info = StreamInfo {
        header: None,
        first_block: Vec::new(),
        decrypted_ncch: false,
    };
    while off < len {
        let n = ((len - off) as usize).min(buf.len());
        let chunk = &mut buf[..n];
        src(chunk)?;
        if off == 0 && is_ncch(chunk) {
            let h = NcchHeader::parse(chunk)?;
            if !h.is_decrypted() {
                crypto = Some(NcchCrypto::new(&h, keys, seeds)?);
                info.decrypted_ncch = true;
            }
            info.header = Some(h);
        }
        if let Some(c) = crypto.as_mut() {
            c.process(off, chunk)?;
            if off == 0 {
                NcchHeader::mark_decrypted_at(chunk, 0x188);
            }
        }
        if off == 0 {
            info.first_block = chunk[..n.min(0x200)].to_vec();
        }
        sink(chunk)?;
        off += n as u64;
    }
    Ok(info)
}
