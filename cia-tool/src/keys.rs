//! Key storage, the 3DS AES key scrambler, and loaders for user-supplied key files.
//!
//! No console keys are shipped with this tool. They are read from:
//! * `aes_keys.txt` in the Citra / Azahar / Lime3DS format (`slot0x2CKeyX=...`,
//!   `slot0x3DKeyX=...`, `common0=...` ... `common5=...`), and/or
//! * a `boot9.bin` / `boot9_prot.bin` dump (provides KeyX for 0x2C and 0x3D).

use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub type Key = [u8; 16];

/// The public constant used by the 3DS hardware key scrambler.
const SCRAMBLER_C: u128 = 0x1FF9E9AAC5FE0408024591DC5D52768A;

/// Normal key = ROL(ROL(KeyX, 2) ^ KeyY + C, 87), all 128-bit big-endian.
pub fn scramble(key_x: &Key, key_y: &Key) -> Key {
    let x = u128::from_be_bytes(*key_x);
    let y = u128::from_be_bytes(*key_y);
    (x.rotate_left(2) ^ y)
        .wrapping_add(SCRAMBLER_C)
        .rotate_left(87)
        .to_be_bytes()
}

#[derive(Default, Clone)]
pub struct KeyStore {
    pub key_x: HashMap<u8, Key>,
    pub key_y: HashMap<u8, Key>,
    pub normal: HashMap<u8, Key>,
    /// KeyY for the common key slot 0x3D, by ticket common-key index (0 = eShop, 1 = system).
    pub common_y: [Option<Key>; 6],
    /// Normal common keys (precomputed), by index. Used when no KeyX/KeyY pair is present.
    pub common_normal: [Option<Key>; 6],
    /// Fixed key for NCCH system titles that use the "fixed crypto key" flag.
    pub fixed_system: Option<Key>,
    pub sources: Vec<PathBuf>,
}

fn parse_hex_key(s: &str) -> Option<Key> {
    let s = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    let clean: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if clean.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

impl KeyStore {
    /// Parse aes_keys.txt content. Unknown names are ignored.
    pub fn load_aes_keys_text(&mut self, text: &str) -> usize {
        let mut loaded = 0;
        for line in text.lines() {
            let line = line.trim().trim_start_matches('\u{FEFF}');
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            let Some((name, value)) = line.split_once('=') else {
                continue;
            };
            let Some(key) = parse_hex_key(value) else {
                continue;
            };
            let name = name.trim().to_ascii_lowercase();
            if let Some(idx) = name.strip_prefix("common") {
                // "common0".."common5" are KeyY values for slot 0x3D.
                // "commonN0".."commonN5" (some dumps) are precomputed normal keys.
                if let Some(n) = idx.strip_prefix('n') {
                    if let Ok(i) = n.parse::<usize>()
                        && i < 6
                    {
                        self.common_normal[i] = Some(key);
                        loaded += 1;
                    }
                } else if let Ok(i) = idx.parse::<usize>()
                    && i < 6
                {
                    self.common_y[i] = Some(key);
                    loaded += 1;
                }
                continue;
            }
            if name == "fixedsys" || name == "fixedsystemkey" || name == "fixed_system_key" {
                self.fixed_system = Some(key);
                loaded += 1;
                continue;
            }
            // slot0x2CKeyX / slot0x2CKeyY / slot0x2CKeyN
            if let Some(rest) = name.strip_prefix("slot0x")
                && rest.len() >= 6
            {
                let (slot_hex, kind) = rest.split_at(2);
                let Ok(slot) = u8::from_str_radix(slot_hex, 16) else {
                    continue;
                };
                let map = match kind {
                    "keyx" => &mut self.key_x,
                    "keyy" => &mut self.key_y,
                    "keyn" => &mut self.normal,
                    _ => continue,
                };
                map.insert(slot, key);
                loaded += 1;
            }
        }
        loaded
    }

    pub fn load_aes_keys_file(&mut self, path: &Path) -> Result<usize> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("cannot read key file {}", path.display()))?;
        let (text, _) = crate::text::decode_text(&bytes);
        let n = self.load_aes_keys_text(&text);
        self.sources.push(path.to_path_buf());
        Ok(n)
    }

    /// Load KeyX for slots 0x2C-0x2F and 0x3C-0x3F from a retail ARM9 bootROM dump.
    pub fn load_boot9(&mut self, path: &Path) -> Result<()> {
        let data = std::fs::read(path)
            .with_context(|| format!("cannot read bootROM {}", path.display()))?;
        let prot: &[u8] = match data.len() {
            0x10000 => &data[0x8000..],
            0x8000 => &data[..],
            n => bail!(
                "{} has size {n:#x}, expected 0x8000 or 0x10000",
                path.display()
            ),
        };
        let blob = &prot[0x5860..0x5860 + 0x400];
        let key_at = |off: usize| -> Key { blob[off..off + 16].try_into().unwrap() };
        for i in 0..4u8 {
            self.key_x.entry(0x2C + i).or_insert(key_at(0x170));
        }
        for i in 0..4u8 {
            self.key_x
                .entry(0x3C + i)
                .or_insert(key_at(0x1B0 + 16 * i as usize));
        }
        self.sources.push(path.to_path_buf());
        Ok(())
    }

    /// The normal key for the common key slot 0x3D with the given ticket index.
    pub fn common_key(&self, index: u8) -> Result<Key> {
        let i = index as usize;
        if i >= 6 {
            bail!("ticket uses unknown common key index {index}");
        }
        if let (Some(x), Some(y)) = (self.key_x.get(&0x3D), self.common_y[i]) {
            return Ok(scramble(x, &y));
        }
        if let Some(n) = self.common_normal[i] {
            return Ok(n);
        }
        bail!("missing common key {index}: aes_keys.txt needs slot0x3DKeyX and common{index}")
    }

    /// Generate a normal key for an NCCH keyslot from its KeyX and the given KeyY.
    pub fn ncch_key(&self, slot: u8, key_y: &Key) -> Result<Key> {
        match self.key_x.get(&slot) {
            Some(x) => Ok(scramble(x, key_y)),
            None => bail!("missing slot0x{slot:02X}KeyX in aes_keys.txt (needed for this title)"),
        }
    }
}

/// Places where emulators keep aes_keys.txt / seeddb.bin, plus the folder of
/// the executable and the current directory.
pub fn default_search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(d) = exe.parent()
    {
        dirs.push(d.to_path_buf());
        dirs.push(d.join("keys"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd.clone());
        dirs.push(cwd.join("keys"));
    }
    let emus = [
        "Azahar",
        "Lime3DS",
        "Citra",
        "citra-emu",
        "azahar-emu",
        "lime3ds-emu",
    ];
    let mut bases: Vec<PathBuf> = Vec::new();
    if let Some(a) = std::env::var_os("APPDATA") {
        bases.push(PathBuf::from(a));
    }
    if let Some(h) = std::env::var_os("HOME") {
        let h = PathBuf::from(h);
        bases.push(h.join(".local/share"));
        bases.push(h.join("Library/Application Support"));
        bases.push(h.join(".3ds"));
        dirs.push(h.join(".3ds"));
    }
    if let Some(x) = std::env::var_os("XDG_DATA_HOME") {
        bases.push(PathBuf::from(x));
    }
    for b in &bases {
        for e in emus {
            dirs.push(b.join(e).join("sysdata"));
        }
    }
    dirs
}

pub fn find_in_dirs(names: &[&str]) -> Option<PathBuf> {
    for d in default_search_dirs() {
        for n in names {
            let p = d.join(n);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrambler_matches_reference() {
        // Reference value computed with pyctr's CryptoEngine.keygen_manual.
        let x: Key = *b"0123456789abcdef";
        let y: Key = *b"fedcba9876543210";
        let n = scramble(&x, &y);
        let expect = 0x4D0DFB0D99634DCB2D3C59F2F66D0CA1u128;
        assert_eq!(u128::from_be_bytes(n), expect);
    }

    #[test]
    fn parse_aes_keys() {
        let mut k = KeyStore::default();
        let n = k.load_aes_keys_text(
            "# test\nslot0x2CKeyX=000102030405060708090A0B0C0D0E0F\r\ncommon1=FFEEDDCCBBAA99887766554433221100\nbogus=12\n",
        );
        assert_eq!(n, 2);
        assert_eq!(k.key_x[&0x2C][15], 0x0F);
        assert_eq!(k.common_y[1].unwrap()[0], 0xFF);
    }
}
