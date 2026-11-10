//! Builders for tests: encrypted NCCH and CIA files made from scratch with fake keys, the
//! encryption written independently of the crate's own NCCH code. Behind the `testkit`
//! feature, for this crate's tests and for romexplorer's.

use crate::keys::{KeyStore, scramble};
use crate::seeddb::SeedDb;
use aes::cipher::{BlockEncryptMut, KeyIvInit, StreamCipher, StreamCipherSeek};
use sha2::{Digest, Sha256};

type Ctr = ctr::Ctr128BE<aes::Aes128>;
type CbcEnc = cbc::Encryptor<aes::Aes128>;

pub struct Rng(pub u64);
impl Rng {
    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n)
            .map(|_| {
                self.0 ^= self.0 << 13;
                self.0 ^= self.0 >> 7;
                self.0 ^= self.0 << 17;
                self.0 as u8
            })
            .collect()
    }
    pub fn key(&mut self) -> [u8; 16] {
        self.bytes(16).try_into().unwrap()
    }
}

pub fn ctr_xor(key: &[u8; 16], iv: u128, rel: u64, data: &mut [u8]) {
    let mut c = Ctr::new(key.into(), &iv.to_be_bytes().into());
    c.seek(rel);
    c.apply_keystream(data);
}

pub fn smdh(title: &str) -> Vec<u8> {
    let mut s = vec![0u8; 0x36C0];
    s[..4].copy_from_slice(b"SMDH");
    for i in 0..16 {
        let enc: Vec<u8> = title.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        s[8 + i * 0x200..8 + i * 0x200 + enc.len()].copy_from_slice(&enc);
    }
    s
}

/// Returns (encrypted, expected decrypted).
pub fn make_ncch(
    rng: &mut Rng,
    keys: &KeyStore,
    pid: u64,
    method: u8,
    seed: Option<[u8; 16]>,
    icon: &[u8],
) -> (Vec<u8>, Vec<u8>) {
    let exefs_off = 0xC00usize;
    let files: Vec<(&[u8], Vec<u8>)> = vec![
        (b".code", rng.bytes(0x1530)),
        (b"icon", icon.to_vec()),
        (b"banner", rng.bytes(0x280)),
    ];
    let mut exefs = vec![0u8; 0x200];
    for (i, (name, data)) in files.iter().enumerate() {
        let off = exefs.len() - 0x200;
        exefs[i * 16..i * 16 + name.len()].copy_from_slice(name);
        exefs[i * 16 + 8..i * 16 + 12].copy_from_slice(&(off as u32).to_le_bytes());
        exefs[i * 16 + 12..i * 16 + 16].copy_from_slice(&(data.len() as u32).to_le_bytes());
        exefs.extend_from_slice(data);
        let padded = exefs.len().div_ceil(0x200) * 0x200;
        let pad = rng.bytes(padded - exefs.len());
        exefs.extend_from_slice(&pad);
    }
    let romfs_off = (exefs_off + exefs.len()).div_ceil(0x1000) * 0x1000;
    let romfs_len = 0x6000;
    let total = romfs_off + romfs_len;
    let mut plain = rng.bytes(total);
    plain[exefs_off..exefs_off + exefs.len()].copy_from_slice(&exefs);
    let key_y = rng.key();
    let h = &mut plain[..0x200];
    h[..16].copy_from_slice(&key_y);
    h[0x100..0x104].copy_from_slice(b"NCCH");
    h[0x104..0x108].copy_from_slice(&((total / 0x200) as u32).to_le_bytes());
    h[0x108..0x110].copy_from_slice(&pid.to_le_bytes());
    h[0x112..0x114].copy_from_slice(&2u16.to_le_bytes());
    h[0x118..0x120].copy_from_slice(&pid.to_le_bytes());
    h[0x180..0x184].copy_from_slice(&0x400u32.to_le_bytes());
    h[0x188..0x190].fill(0);
    h[0x18B] = method;
    h[0x18F] = if seed.is_some() { 0x20 } else { 0 };
    h[0x1A0..0x1A4].copy_from_slice(&((exefs_off / 0x200) as u32).to_le_bytes());
    h[0x1A4..0x1A8].copy_from_slice(&((exefs.len() / 0x200) as u32).to_le_bytes());
    h[0x1B0..0x1B4].copy_from_slice(&((romfs_off / 0x200) as u32).to_le_bytes());
    h[0x1B4..0x1B8].copy_from_slice(&((romfs_len / 0x200) as u32).to_le_bytes());
    let mut ky2 = key_y;
    if let Some(s) = seed {
        let chk = Sha256::new()
            .chain_update(s)
            .chain_update(pid.to_le_bytes())
            .finalize();
        h[0x114..0x118].copy_from_slice(&chk[..4]);
        ky2 = Sha256::new().chain_update(key_y).chain_update(s).finalize()[..16]
            .try_into()
            .unwrap();
    }
    let slot = match method {
        0 => 0x2C,
        1 => 0x25,
        0x0A => 0x18,
        _ => 0x1B,
    };
    let main = scramble(&keys.key_x[&0x2C], &key_y);
    let extra = scramble(&keys.key_x[&slot], &ky2);
    let iv = |t: u128| ((pid as u128) << 64) | (t << 56);

    let mut enc = plain.clone();
    ctr_xor(&main, iv(1), 0, &mut enc[0x200..0xA00]);
    ctr_xor(
        &main,
        iv(2),
        0,
        &mut enc[exefs_off..exefs_off + exefs.len()],
    );
    // .code (file 0) uses the secondary key: undo main, apply extra.
    let code = exefs_off + 0x200..exefs_off + 0x200 + files[0].1.len();
    enc[code.clone()].copy_from_slice(&plain[code.clone()]);
    ctr_xor(&extra, iv(2), 0x200, &mut enc[code]);
    ctr_xor(&extra, iv(3), 0, &mut enc[romfs_off..total]);

    let mut expect = plain;
    expect[0x18B] = 0;
    expect[0x18F] = (expect[0x18F] & !0x21) | 4;
    (enc, expect)
}

pub fn align64(v: &mut Vec<u8>) {
    v.resize(v.len().div_ceil(64) * 64, 0);
}

pub fn sig() -> Vec<u8> {
    let mut v = 0x10004u32.to_be_bytes().to_vec();
    v.resize(4 + 0x100 + 0x3C, 0);
    v
}

pub fn make_cia(
    rng: &mut Rng,
    keys: &KeyStore,
    tid: u64,
    contents: &[(u16, Vec<u8>)],
    icon: &[u8],
) -> Vec<u8> {
    let title_key = rng.key();
    let ck = scramble(&keys.key_x[&0x3D], &keys.common_y[0].unwrap());
    let mut iv = [0u8; 16];
    iv[..8].copy_from_slice(&tid.to_be_bytes());
    let mut etk = title_key;
    CbcEnc::new(&ck.into(), &iv.into()).encrypt_block_mut((&mut etk).into());

    let mut ticket = sig();
    let mut td = vec![0u8; 0x164];
    td[0x7F..0x8F].copy_from_slice(&etk);
    td[0x9C..0xA4].copy_from_slice(&tid.to_be_bytes());
    ticket.extend(td);

    let mut tmd = sig();
    let mut hdr = vec![0u8; 0xC4];
    hdr[0x4C..0x54].copy_from_slice(&tid.to_be_bytes());
    hdr[0x9C..0x9E].copy_from_slice(&3u16.to_be_bytes());
    hdr[0x9E..0xA0].copy_from_slice(&(contents.len() as u16).to_be_bytes());
    tmd.extend(hdr);
    let mut info = vec![0u8; 0x900];
    info[2..4].copy_from_slice(&(contents.len() as u16).to_be_bytes());
    tmd.extend(info);
    let mut body = Vec::new();
    let mut header = vec![0u8; 0x2020];
    for (idx, data) in contents {
        let mut rec = vec![0u8; 0x30];
        rec[4..6].copy_from_slice(&idx.to_be_bytes());
        rec[6..8].copy_from_slice(&1u16.to_be_bytes());
        rec[8..16].copy_from_slice(&(data.len() as u64).to_be_bytes());
        tmd.extend(rec);
        let mut e = data.clone();
        let mut civ = [0u8; 16];
        civ[..2].copy_from_slice(&idx.to_be_bytes());
        let mut c = CbcEnc::new(&title_key.into(), &civ.into());
        for b in e.chunks_exact_mut(16) {
            c.encrypt_block_mut(b.into());
        }
        body.extend(e);
        header[0x20 + *idx as usize / 8] |= 0x80 >> (idx % 8);
    }
    let mut meta = vec![0u8; 0x3AC0];
    meta[0x400..0x400 + icon.len()].copy_from_slice(icon);
    let cert = rng.bytes(0xA00);
    header[0..4].copy_from_slice(&0x2020u32.to_le_bytes());
    header[8..12].copy_from_slice(&(cert.len() as u32).to_le_bytes());
    header[12..16].copy_from_slice(&(ticket.len() as u32).to_le_bytes());
    header[16..20].copy_from_slice(&(tmd.len() as u32).to_le_bytes());
    header[20..24].copy_from_slice(&(meta.len() as u32).to_le_bytes());
    header[24..32].copy_from_slice(&(body.len() as u64).to_le_bytes());
    let mut out = header;
    for part in [cert, ticket, tmd, body] {
        align64(&mut out);
        out.extend(part);
    }
    align64(&mut out);
    out.extend(meta);
    out
}

pub fn setup() -> (Rng, KeyStore, SeedDb) {
    let mut rng = Rng(0x9E3779B97F4A7C15);
    let mut keys = KeyStore::default();
    for s in [0x2Cu8, 0x25, 0x18, 0x1B, 0x3D] {
        keys.key_x.insert(s, rng.key());
    }
    keys.common_y[0] = Some(rng.key());
    (rng, keys, SeedDb::default())
}

/// `keys` as an `aes_keys.txt` (KeyX slots and the common KeyYs it holds).
pub fn aes_keys_text(keys: &KeyStore) -> String {
    let hex = |k: &[u8; 16]| k.iter().map(|b| format!("{b:02X}")).collect::<String>();
    let mut out = String::new();
    let mut slots: Vec<_> = keys.key_x.iter().collect();
    slots.sort();
    for (slot, k) in slots {
        out.push_str(&format!("slot0x{slot:02X}KeyX={}\n", hex(k)));
    }
    for (i, k) in keys.common_y.iter().enumerate() {
        if let Some(k) = k {
            out.push_str(&format!("common{i}={}\n", hex(k)));
        }
    }
    out
}

/// `seeds` as a `seeddb.bin`.
pub fn seeddb_bytes(seeds: &[(u64, [u8; 16])]) -> Vec<u8> {
    let mut out = (seeds.len() as u32).to_le_bytes().to_vec();
    out.resize(0x10, 0);
    for (tid, seed) in seeds {
        out.extend_from_slice(&tid.to_le_bytes());
        out.extend_from_slice(seed);
        out.extend_from_slice(&[0; 8]);
    }
    out
}
