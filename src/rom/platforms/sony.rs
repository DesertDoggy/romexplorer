//! Sony's package and download formats: the PKG container (PS3, PSP and Vita share it), the
//! PARAM.SFO every one of them carries, and the Vita's own shapes (`.pkg`, `.vpk`, `.psv`,
//! game-card images). PS3/PSP game folders and PBP files are read in `super::download`, which
//! uses the PKG and SFO readers here.
//!
//! PS1/PS2/PS3/PSP discs are read in `super::super::detect::disc_dispatch`.
//!
//! **PKG** (big-endian; psdevwiki.com/ps3/PKG_files, and `pkg2zip.c` by mmozeiko, Unlicense):
//!
//! | offset | field |
//! |---|---|
//! | 0x00 | `7F 'PKG'` |
//! | 0x04 | u16 revision (`8000` retail) |
//! | 0x06 | u16 platform: 1 = PS3, 2 = PSP and Vita |
//! | 0x08 | u32 metadata offset; 0x0C u32 metadata count; 0x10 u32 metadata size |
//! | 0x14 | u32 item count |
//! | 0x30 | content id, 36 ASCII bytes (`EP0000-PCSE00000_00-LABEL0000000000`) |
//! | 0xC0 | `7F 'ext'` extended header (PSP and Vita); its key type at 0xE7 (low 3 bits): 1 PSP, 2-4 Vita |
//!
//! Metadata entries (`id`, `size`, data), in plain text: 2 is the content type (`0x06` PS1
//! classic, `0x07`/`0x0E`/`0x0F`/`0x10` PSP, `0x04` PS3 game data, `0x05` PS3 game, `0x15`
//! Vita game, `0x16` Vita DLC, `0x18`/`0x1D` PlayStation Mobile, `0x1F` Vita theme); 8 holds the
//! app version in its last two bytes (BCD, `01 02` = 1.02); 14 is the offset and size of a Vita
//! package's PARAM.SFO, which is stored unencrypted. A PS3 or PSP package's SFO sits inside the
//! encrypted body.
//!
//! **PARAM.SFO** (little-endian): `00 'PSF'`, version, key table offset (0x08), data table
//! offset (0x0C), entry count (0x10), then 16-byte entries (key offset u16, format u16 --
//! `0x0004` UTF-8 special, `0x0204` UTF-8, `0x0404` u32 -- length, max length, data offset).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use super::super::detect::disc_fs::read_exact_at;
use super::super::detect::reason::Evidence;
use super::super::models::{sanitize_serial_opt, sanitize_title_opt};
use super::{read_ascii_field, read_u16_be, read_u32_be, NonDiscHeaderMetadata};

type LeafMetadata = Option<(Option<String>, Option<String>, Option<String>)>;

// ---------------------------------------------------------------------------
// PARAM.SFO
// ---------------------------------------------------------------------------

/// One PARAM.SFO value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SfoValue {
    Text(String),
    Int(u32),
}

/// A parsed PARAM.SFO: every key, in file order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sfo(pub Vec<(String, SfoValue)>);

impl Sfo {
    pub const MAGIC: [u8; 4] = [0x00, b'P', b'S', b'F'];

    /// `None` unless `bytes` starts with a well-formed SFO header.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.get(0..4)? != Self::MAGIC {
            return None;
        }
        let u32_at = |offset: usize| bytes.get(offset..offset + 4).map(|raw| u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize);
        let key_table = u32_at(8)?;
        let data_table = u32_at(12)?;
        let count = u32_at(16)?;
        if count == 0 || count > 256 || key_table > bytes.len() || data_table > bytes.len() {
            return None;
        }
        let text = |raw: &[u8]| String::from_utf8_lossy(raw.split(|&b| b == 0).next().unwrap_or_default()).to_string();
        let mut entries = Vec::with_capacity(count);
        for index in 0..count {
            let base = 0x14 + index * 0x10;
            let key_offset = u16::from_le_bytes([*bytes.get(base)?, *bytes.get(base + 1)?]) as usize;
            let format = u16::from_le_bytes([*bytes.get(base + 2)?, *bytes.get(base + 3)?]);
            let data_len = u32_at(base + 4)?;
            let data_offset = u32_at(base + 12)?;
            let key = text(bytes.get(key_table + key_offset..)?);
            let start = data_table + data_offset;
            let data = bytes.get(start..(start + data_len).min(bytes.len()))?;
            let value = if format == 0x0404 && data.len() >= 4 {
                SfoValue::Int(u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
            } else {
                SfoValue::Text(text(data))
            };
            entries.push((key, value));
        }
        Some(Self(entries))
    }

    pub fn text(&self, key: &str) -> Option<&str> {
        self.0.iter().find_map(|(name, value)| match value {
            SfoValue::Text(text) if name == key && !text.trim().is_empty() => Some(text.trim()),
            _ => None,
        })
    }

    pub fn int(&self, key: &str) -> Option<u32> {
        self.0.iter().find_map(|(name, value)| match value {
            SfoValue::Int(n) if name == key => Some(*n),
            _ => None,
        })
    }

    pub fn has(&self, key: &str) -> bool {
        self.0.iter().any(|(name, _)| name == key)
    }

    /// The title, the long one first (`TITLE`, else the short `STITLE`).
    pub fn title(&self) -> Option<String> {
        self.text("TITLE").or_else(|| self.text("STITLE")).map(|t| t.replace(['\n', '\r'], " ")).and_then(|t| sanitize_title_opt(Some(t)))
    }

    /// The game's id: `TITLE_ID` (PS3, Vita, PSP downloads), else `DISC_ID` (PSP UMD, PSP DLC).
    pub fn title_id(&self) -> Option<String> {
        self.text("TITLE_ID").or_else(|| self.text("DISC_ID")).map(str::to_ascii_uppercase).and_then(|t| sanitize_serial_opt(Some(t)))
    }

    /// Whether this is a Vita SFO: it names a minimum Vita firmware.
    pub fn is_vita(&self) -> bool {
        self.has("PSP2_SYSTEM_VER") || self.has("PSP2_DISP_VER")
    }

    /// One line of what the SFO says beyond title and id, for the detection trail.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(category) = self.text("CATEGORY") {
            parts.push(format!("CATEGORY {category} ({})", category_label(category)));
        }
        for key in ["APP_VER", "DISC_VERSION", "VERSION", "PSP2_DISP_VER", "PSP_SYSTEM_VER", "PS3_SYSTEM_VER"] {
            if let Some(value) = self.text(key) {
                parts.push(format!("{key} {value}"));
            }
        }
        if let Some(bootable) = self.int("BOOTABLE") {
            parts.push(format!("BOOTABLE {bootable}"));
        }
        if let Some(content_id) = self.text("CONTENT_ID") {
            parts.push(format!("CONTENT_ID {content_id}"));
        }
        if let Some(date) = self.text("PUBTOOLINFO").and_then(|info| info.split(',').find_map(|part| part.strip_prefix("c_date="))) {
            parts.push(format!("built {date}"));
        }
        parts.join(", ")
    }

    /// What the SFO's own fields say this is: a game, an update or downloadable content.
    pub fn content_kind(&self) -> ContentKind {
        if self.int("BOOTABLE") == Some(0) {
            return ContentKind::Dlc;
        }
        match self.text("CATEGORY") {
            Some("gp") | Some("GP") => ContentKind::Update,
            Some("ac") | Some("AC") => ContentKind::Dlc,
            _ => ContentKind::Game,
        }
    }
}

/// The SFO `CATEGORY` codes this module reads, in words.
fn category_label(category: &str) -> &'static str {
    match category {
        "gd" => "Vita digital game",
        "gc" => "Vita game card",
        "gp" => "Vita update",
        "ac" => "Vita add-on",
        "gdc" | "gdd" => "Vita system application",
        "DG" => "PS3 disc game",
        "HG" => "PS3 hard-disk game",
        "GD" => "PS3 game data",
        "GP" => "PS3 update",
        "AC" => "PS3 add-on",
        "UG" => "PSP UMD game",
        "EG" => "PSP digital game",
        "MG" => "PSP memory-stick game or data",
        "ME" => "PS1 classic",
        _ => "other",
    }
}

/// What a package or folder holds, as its own header says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    Game,
    Update,
    Dlc,
    /// PS3 "game data" packages: an update or an add-on, the header does not say which.
    UpdateOrDlc,
    /// Themes, avatars, PlayStation Mobile runtimes: not software of their own.
    Other,
}

impl ContentKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Game => "game",
            Self::Update => "update",
            Self::Dlc => "add-on (DLC)",
            Self::UpdateOrDlc => "update or add-on",
            Self::Other => "other content",
        }
    }
}

// ---------------------------------------------------------------------------
// PKG
// ---------------------------------------------------------------------------

/// The system a PKG belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PkgSystem {
    Ps3,
    Psp,
    /// A PS1 classic packaged for PSP (and Vita's PSP emulator).
    PspPs1Classic,
    Vita,
}

/// What a PKG's plain-text header and metadata say.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pkg {
    /// Where the package starts in the file (a `.psv` embeds one).
    pub base: u64,
    pub platform: u16,
    /// `Some` when the `7F 'ext'` header is present: its key type.
    pub key_type: Option<u8>,
    pub content_id: Option<String>,
    pub content_type: Option<u32>,
    /// From metadata entry 8, `"01.02"`.
    pub app_version: Option<String>,
    /// A Vita package's unencrypted PARAM.SFO: offset from `base`, size.
    pub sfo: Option<(u64, u32)>,
    /// The encrypted body's offset from `base` (header 0x20), and its AES-CTR IV (0x70).
    pub data_offset: u64,
    pub iv: [u8; 16],
    pub item_count: u32,
    /// Metadata entry 13: the item table's offset in the body.
    pub items_offset: u64,
}

impl Pkg {
    pub const MAGIC: [u8; 4] = [0x7F, b'P', b'K', b'G'];
    const HEAD_LEN: usize = 0x100;

    /// Reads the package at `base` in `path`. `None` unless the PKG magic and a known platform
    /// are there.
    pub fn read_at(path: &Path, base: u64) -> Option<Self> {
        let head = read_exact_at(path, base, Self::HEAD_LEN).ok()?;
        let meta = (|| {
            let offset = read_u32_be(&head, 0x08)? as u64;
            let size = (read_u32_be(&head, 0x10)? as usize).clamp(0, 0x10000);
            read_exact_at(path, base + offset, size.max(16)).ok()
        })();
        Self::parse(&head, meta.as_deref().unwrap_or_default(), base)
    }

    pub fn read(path: &Path) -> Option<Self> {
        Self::read_at(path, 0)
    }

    /// From the first 0x100 bytes and the metadata block (empty when unread).
    pub fn parse(head: &[u8], meta: &[u8], base: u64) -> Option<Self> {
        if head.get(0..4)? != Self::MAGIC {
            return None;
        }
        let platform = read_u16_be(head, 6)?;
        if !matches!(platform, 1 | 2) {
            return None;
        }
        let key_type = (head.get(0xC0..0xC4) == Some(&[0x7F, b'e', b'x', b't'])).then(|| head.get(0xE7).map(|k| k & 7)).flatten();
        let content_id = head.get(0x30..0x30 + 36).and_then(read_ascii_field).filter(|id| id.len() >= 16);
        let mut pkg = Pkg {
            base,
            platform,
            key_type,
            content_id,
            data_offset: u64::from_be_bytes(head.get(0x20..0x28)?.try_into().ok()?),
            iv: head.get(0x70..0x80)?.try_into().ok()?,
            item_count: read_u32_be(head, 0x14)?,
            ..Default::default()
        };
        let count = read_u32_be(head, 0x0C).unwrap_or(0).min(64);
        let mut at = 0usize;
        for _ in 0..count {
            let (Some(id), Some(size)) = (read_u32_be(meta, at), read_u32_be(meta, at + 4)) else { break };
            let data = meta.get(at + 8..at + 8 + size as usize).unwrap_or_default();
            match id {
                2 => pkg.content_type = read_u32_be(data, 0),
                13 => pkg.items_offset = read_u32_be(data, 0).unwrap_or(0) as u64,
                8 if data.len() >= 8 => pkg.app_version = Some(format!("{:02X}.{:02X}", data[6], data[7])),
                14 => {
                    if let (Some(offset), Some(len)) = (read_u32_be(data, 0), read_u32_be(data, 4)) {
                        if len >= 0x14 && len <= 0x10000 {
                            pkg.sfo = Some((offset as u64, len));
                        }
                    }
                }
                _ => {}
            }
            at += 8 + size as usize;
        }
        Some(pkg)
    }

    /// Which system the package is for: the platform word separates PS3; the content type
    /// separates PSP from Vita, with the extended header's key type when there is none.
    pub fn system(&self) -> Option<PkgSystem> {
        if self.platform == 1 {
            return Some(PkgSystem::Ps3);
        }
        match self.content_type {
            Some(0x06) => Some(PkgSystem::PspPs1Classic),
            Some(0x07 | 0x0E | 0x0F | 0x10) => Some(PkgSystem::Psp),
            Some(0x15 | 0x16 | 0x17 | 0x18 | 0x1D | 0x1F) => Some(PkgSystem::Vita),
            _ => match self.key_type {
                Some(1) => Some(PkgSystem::Psp),
                Some(2..=4) => Some(PkgSystem::Vita),
                _ => None,
            },
        }
    }

    /// The content type in words.
    pub fn content_type_label(&self) -> &'static str {
        match (self.platform, self.content_type) {
            (_, Some(0x06)) => "PS1 classic",
            (_, Some(0x07)) => "PSP game",
            (_, Some(0x0E)) => "PSP go game",
            (_, Some(0x0F)) => "PSP minis",
            (_, Some(0x10)) => "PSP NeoGeo",
            (_, Some(0x15)) => "Vita application",
            (_, Some(0x16)) => "Vita add-on",
            (_, Some(0x17)) => "Vita LiveArea",
            (_, Some(0x18 | 0x1D)) => "PlayStation Mobile",
            (_, Some(0x1F)) => "Vita theme",
            (1, Some(0x04)) => "PS3 game data (update or add-on)",
            (1, Some(0x05)) => "PS3 game",
            (1, Some(0x09)) => "PS3 theme",
            (1, Some(0x0D)) => "PSN avatar",
            (1, Some(0x0B)) => "license",
            (1, Some(0x12)) => "PS2 classic",
            _ => "unknown content type",
        }
    }

    /// What the content type alone says the package is. A Vita application's SFO says more
    /// (`CATEGORY gp` is an update): see [`Sfo::content_kind`].
    pub fn content_kind(&self) -> ContentKind {
        match self.content_type {
            Some(0x16) => ContentKind::Dlc,
            Some(0x04) if self.platform == 1 => ContentKind::UpdateOrDlc,
            Some(0x17 | 0x18 | 0x1D | 0x1F | 0x09 | 0x0D | 0x0B) => ContentKind::Other,
            _ => ContentKind::Game,
        }
    }

    /// The title id inside the content id (`EP0000-PCSE00000_00-…` → `PCSE00000`).
    pub fn title_id(&self) -> Option<String> {
        let id = self.content_id.as_deref()?.split('-').nth(1)?.get(..9)?.to_string();
        (id.len() == 9 && id.chars().all(|c| c.is_ascii_alphanumeric())).then(|| id.to_ascii_uppercase()).and_then(|id| sanitize_serial_opt(Some(id)))
    }

    /// The region the content id's service prefix names (`UP`, `EP`, `JP`, `HP`, `KP`).
    pub fn region(&self) -> Option<&'static str> {
        content_id_region(self.content_id.as_deref()?)
    }

    /// A Vita package's plain-text PARAM.SFO.
    pub fn read_sfo(&self, path: &Path) -> Option<Sfo> {
        let (offset, len) = self.sfo?;
        Sfo::parse(&read_exact_at(path, self.base + offset, len as usize).ok()?)
    }

    /// A PS3 or PSP package's PARAM.SFO, out of its encrypted body. Only when
    /// [`set_read_package_titles`] is on: the keys are public (they ship in pkg2zip and every
    /// other package tool), but reading them is the user's choice.
    ///
    /// The body is AES-128-CTR with the IV at header 0x70, the counter advancing one per 16
    /// bytes from the body's start. Its item table (32-byte entries: name offset, name size,
    /// data offset, data size, flags; the flags' low byte `0x90` marks a PSP item, which uses
    /// the PSP key) leads to the SFO. Per `pkg2zip.c`'s `find_psp_sfo`.
    pub fn decrypt_sfo(&self, path: &Path) -> Option<Sfo> {
        if !read_package_titles() {
            return None;
        }
        let main_key = match (self.platform, self.key_type) {
            (1, _) => PKG_PS3_KEY,
            (2, Some(1)) => PKG_PSP_KEY,
            _ => return None,
        };
        let count = self.item_count.min(20_000) as usize;
        let body = self.base + self.data_offset;
        let table_at = self.items_offset;
        let mut table = read_exact_at(path, body + table_at, count * 32).ok()?;
        aes_ctr_xor(&main_key, &self.iv, table_at, &mut table);
        let mut best: Option<(usize, u64, u64, [u8; 16])> = None;
        for entry in table.chunks_exact(32) {
            let name_offset = read_u32_be(entry, 0)? as u64;
            let name_size = read_u32_be(entry, 4)? as usize;
            let data_offset = u64::from_be_bytes(entry[8..16].try_into().ok()?);
            let data_size = u64::from_be_bytes(entry[16..24].try_into().ok()?);
            if name_size == 0 || name_size > 0x400 || data_size < 0x14 || data_size > 0x10000 {
                continue;
            }
            let key = if self.platform == 2 && entry[24] == 0x90 { PKG_PSP_KEY } else { PKG_PS3_KEY };
            let key = if self.platform == 1 { main_key } else { key };
            let Ok(mut name) = read_exact_at(path, body + name_offset, name_size) else { continue };
            aes_ctr_xor(&key, &self.iv, name_offset, &mut name);
            let name = String::from_utf8_lossy(name.split(|&b| b == 0).next().unwrap_or_default()).to_string();
            if !name.to_ascii_uppercase().ends_with("PARAM.SFO") {
                continue;
            }
            // The game's own SFO is the one nearest the root (`PARAM.SFO` before
            // `USRDIR/.../PARAM.SFO`).
            let depth = name.matches('/').count();
            if best.is_none_or(|(d, ..)| depth < d) {
                best = Some((depth, data_offset, data_size, key));
            }
        }
        let (_, data_offset, data_size, key) = best?;
        let mut data = read_exact_at(path, body + data_offset, data_size as usize).ok()?;
        aes_ctr_xor(&key, &self.iv, data_offset, &mut data);
        Sfo::parse(&data)
    }

    /// Evidence for the detection trail: the magic, the platform word, the content type and
    /// the content id.
    pub fn evidence(&self) -> Vec<Evidence> {
        let at = |offset: u64| (self.base + offset) as usize;
        let mut out = vec![
            Evidence::Magic { offset: at(0), bytes: Self::MAGIC.to_vec() },
            Evidence::Bytes {
                offset_start: at(6),
                offset_end: at(8),
                bytes: self.platform.to_be_bytes().to_vec(),
                note: format!("PKG platform {}", if self.platform == 1 { "1 = PS3" } else { "2 = PSP or Vita" }),
            },
        ];
        match self.content_type {
            Some(t) => out.push(Evidence::Note(format!("PKG metadata entry 2, content type 0x{t:02X}: {}", self.content_type_label()))),
            None => {
                if let Some(key) = self.key_type {
                    out.push(Evidence::Note(format!("no content type; extended header key type {key} ({})", if key == 1 { "PSP" } else { "Vita" })))
                }
            }
        }
        if let Some(id) = &self.content_id {
            out.push(Evidence::Ascii { offset_start: at(0x30), offset_end: at(0x30) + id.len(), text: id.clone() });
        }
        if let Some(version) = &self.app_version {
            out.push(Evidence::Note(format!("PKG metadata entry 8: app version {version}")));
        }
        if let Some(region) = self.region() {
            out.push(Evidence::Note(format!("content id service prefix: {region}")));
        }
        out
    }
}

/// Whether PS3 and PSP packages' encrypted PARAM.SFO is read for their title (the
/// `scan_pkg_titles` setting; off unless a scan turns it on).
static READ_PACKAGE_TITLES: AtomicBool = AtomicBool::new(false);

pub fn set_read_package_titles(on: bool) {
    READ_PACKAGE_TITLES.store(on, Ordering::Relaxed);
}

pub fn read_package_titles() -> bool {
    READ_PACKAGE_TITLES.load(Ordering::Relaxed)
}

/// The package keys every PKG tool carries (`pkg2zip.c`: `pkg_ps3_key`, `pkg_psp_key`).
const PKG_PS3_KEY: [u8; 16] = [0x2e, 0x7b, 0x71, 0xd7, 0xc9, 0xc9, 0xa1, 0x4e, 0xa3, 0x22, 0x1f, 0x18, 0x88, 0x28, 0xb8, 0xf8];
const PKG_PSP_KEY: [u8; 16] = [0x07, 0xf2, 0xc6, 0x82, 0x90, 0xb5, 0x0d, 0x2c, 0x33, 0x81, 0x8d, 0x70, 0x9b, 0x60, 0xe6, 0x2b];

/// AES-128-CTR over `data`, which starts `offset` bytes (a multiple of 16) into the stream
/// whose first counter block is `iv`.
pub fn aes_ctr_xor(key: &[u8; 16], iv: &[u8; 16], offset: u64, data: &mut [u8]) {
    use aes::cipher::{BlockCipherEncrypt, KeyInit};
    let cipher = aes::Aes128::new(key.into());
    let mut counter = u128::from_be_bytes(*iv).wrapping_add((offset / 16) as u128);
    for chunk in data.chunks_mut(16) {
        let mut block = aes::Block::from(counter.to_be_bytes());
        cipher.encrypt_block(&mut block);
        for (byte, k) in chunk.iter_mut().zip(block.iter()) {
            *byte ^= k;
        }
        counter = counter.wrapping_add(1);
    }
}

/// The region a content id's two-letter service prefix names.
pub fn content_id_region(content_id: &str) -> Option<&'static str> {
    match content_id.get(..2)? {
        "UP" => Some("USA"),
        "EP" => Some("Europe"),
        "JP" => Some("Japan"),
        "HP" => Some("Asia"),
        "KP" => Some("Korea"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Vita
// ---------------------------------------------------------------------------

/// Reads a Vita file by its content: a PKG, a VPK, a `.psv` dump or a game-card image.
pub fn read_vita_metadata(path: &Path) -> Option<NonDiscHeaderMetadata> {
    let len = std::fs::metadata(path).ok()?.len();
    let head = read_exact_at(path, 0, 0x70.min(len as usize)).ok()?;
    if head.starts_with(&Pkg::MAGIC) {
        return vita_pkg_metadata(path, &Pkg::read(path)?, Vec::new());
    }
    if head.starts_with(b"PK\x03\x04") {
        return vpk_metadata(path);
    }
    if head.starts_with(PSV_MAGIC) {
        return psv_metadata(path, &head, len);
    }
    if head.starts_with(VITA_MBR_MAGIC) {
        return card_image_metadata(path, 0, Vec::new());
    }
    None
}

/// The title id and kind of a Sony file lenient matching looks up by serial: a PKG of any of
/// the three systems, a VPK, a `.psv`, a game-card image, or a `PARAM.SFO` of a game folder.
/// A PKG's title id is the one in its content id, which for a PS3 disc game's update is the
/// disc's serial.
pub fn serial_and_kind(path: &Path) -> Option<(String, ContentKind)> {
    let is_sfo = path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.eq_ignore_ascii_case("param.sfo"));
    if is_sfo {
        let len = std::fs::metadata(path).ok()?.len();
        let sfo = Sfo::parse(&read_exact_at(path, 0, len.min(0x10000) as usize).ok()?)?;
        return Some((sfo.title_id()?, sfo.content_kind()));
    }
    let head = read_exact_at(path, 0, 4).ok()?;
    if head.starts_with(&Pkg::MAGIC) {
        let pkg = Pkg::read(path)?;
        let sfo = pkg.read_sfo(path).or_else(|| pkg.decrypt_sfo(path));
        let kind = match (pkg.content_kind(), &sfo) {
            (ContentKind::Game | ContentKind::UpdateOrDlc, Some(sfo)) => sfo.content_kind(),
            (kind, _) => kind,
        };
        return Some((pkg.title_id().or_else(|| sfo.as_ref().and_then(Sfo::title_id))?, kind));
    }
    if head.starts_with(b"PK\x03\x04") {
        let sfo = vpk_sfo(path)?;
        return Some((sfo.title_id()?, sfo.content_kind()));
    }
    let meta = read_vita_metadata(path)?;
    Some((meta.serial?, ContentKind::Game))
}

/// A VPK is a zip with the game's `eboot.bin` at its root and its `sce_sys/param.sfo`. Both
/// have to be there: a zip holding only one of them is something else.
fn vpk_metadata(path: &Path) -> Option<NonDiscHeaderMetadata> {
    let sfo = vpk_sfo(path)?;
    let kind = sfo.content_kind();
    let evidence = vec![
        Evidence::FileInFilesystem { filesystem: "zip", path: "eboot.bin".to_string(), field: None },
        Evidence::FileInFilesystem { filesystem: "zip", path: "sce_sys/param.sfo".to_string(), field: Some(sfo.summary()) },
        Evidence::Note(format!("content: {}", kind.label())),
    ];
    Some(NonDiscHeaderMetadata { title: sfo.title(), serial: sfo.title_id(), evidence, ..Default::default() })
}

/// A VPK's `sce_sys/param.sfo`, when the zip also has an `eboot.bin` at its root.
fn vpk_sfo(path: &Path) -> Option<Sfo> {
    use super::super::sevenzip_wrapper::{ArchiveFormat, SevenZipArchive};
    let archive = SevenZipArchive::open(path, ArchiveFormat::Zip, |_, _| true).ok()?;
    let (mut sfo_index, mut has_eboot) = (None, false);
    for index in 0..archive.entry_count() {
        let Ok(entry) = archive.entry(index) else { continue };
        let name = entry.path.replace('\\', "/");
        if name.eq_ignore_ascii_case("eboot.bin") && !entry.is_dir {
            has_eboot = true;
        } else if name.eq_ignore_ascii_case("sce_sys/param.sfo") && !entry.is_dir && entry.size <= 0x10000 {
            sfo_index = Some(index);
        }
    }
    if !has_eboot {
        return None;
    }
    Sfo::parse(&archive.read_entry(sfo_index?).ok()?)
}

const PSV_MAGIC: &[u8] = b"PSV\0";
/// The first sector of a Vita game card (and of the Vita's own storage): its MBR.
const VITA_MBR_MAGIC: &[u8] = b"Sony Computer Entertainment Inc.";
const PSV_FLAG_TRIMMED: u32 = 1;
const PSV_FLAG_DIGITAL: u32 = 2;

/// A `.psv` (psvgamesd, `driver/psv_types.h`, MIT): `PSV\0`, version 1, flags, two keys, a
/// signature and a SHA-256, then the image size (0x60) and its offset in 512-byte sectors
/// (0x68). A digital dump holds a licence and then the package; a cartridge dump holds the raw
/// card image.
fn psv_metadata(path: &Path, head: &[u8], len: u64) -> Option<NonDiscHeaderMetadata> {
    let u32_le = |at: usize| head.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let u64_le = |at: usize| head.get(at..at + 8).map(|b| u64::from_le_bytes(b.try_into().unwrap_or_default()));
    let version = u32_le(4)?;
    let flags = u32_le(8)?;
    if version != 1 || flags > 7 {
        return None;
    }
    let image_offset = u64_le(0x68)?.checked_mul(512)?;
    let mut evidence = vec![
        Evidence::Magic { offset: 0, bytes: PSV_MAGIC.to_vec() },
        Evidence::Note(format!(
            ".psv header version 1, flags {flags}: {}",
            match (flags & PSV_FLAG_DIGITAL != 0, flags & PSV_FLAG_TRIMMED != 0) {
                (true, true) => "licence only, no package",
                (true, false) => "licence and package",
                (false, true) => "trimmed game-card image",
                (false, false) => "game-card image",
            }
        )),
    ];
    if flags & PSV_FLAG_DIGITAL != 0 && flags & PSV_FLAG_TRIMMED != 0 {
        // A licence backup: Vita, but nothing to read a title from.
        return Some(NonDiscHeaderMetadata { evidence, ..Default::default() });
    }
    if image_offset == 0 || image_offset >= len {
        return None;
    }
    if flags & PSV_FLAG_DIGITAL != 0 {
        return vita_pkg_metadata(path, &Pkg::read_at(path, image_offset)?, evidence);
    }
    if read_exact_at(path, image_offset, VITA_MBR_MAGIC.len()).ok()?.as_slice() != VITA_MBR_MAGIC {
        return None;
    }
    evidence.push(Evidence::Note(format!("game-card image at 0x{image_offset:X}")));
    card_image_metadata(path, image_offset, evidence)
}

/// A game-card image: the Vita MBR (`Sony Computer Entertainment Inc.`), whose partition table
/// at 0x50 (17 entries of 0x11 bytes: start sector, sector count, code, type, ...) points at the
/// game's exFAT partition. Its directory names are plain text: `app/<TITLE_ID>/`. The files in
/// it are encrypted, so the SFO is read only when it turns out to be plain.
fn card_image_metadata(path: &Path, base: u64, mut evidence: Vec<Evidence>) -> Option<NonDiscHeaderMetadata> {
    let mbr = read_exact_at(path, base, 0x200).ok()?;
    if mbr.get(..VITA_MBR_MAGIC.len())? != VITA_MBR_MAGIC {
        return None;
    }
    evidence.push(Evidence::Ascii { offset_start: base as usize, offset_end: base as usize + VITA_MBR_MAGIC.len(), text: String::from_utf8_lossy(VITA_MBR_MAGIC).to_string() });
    let mut found = None;
    for slot in 0..17 {
        let at = 0x50 + slot * 0x11;
        let (Some(start), Some(count)) = (mbr.get(at..at + 4), mbr.get(at + 4..at + 8)) else { break };
        let start = u32::from_le_bytes(start.try_into().ok()?) as u64;
        if start == 0 || u32::from_le_bytes(count.try_into().ok()?) == 0 {
            continue;
        }
        let Some(fs) = exfat::ExFat::open(path, base + start * 512) else { continue };
        if let Some(app) = fs.find(&fs.root(), "app").filter(|e| e.is_dir) {
            found = Some((fs, app, base + start * 512));
            break;
        }
    }
    let Some((fs, app, partition)) = found else {
        evidence.push(Evidence::Note("no exFAT partition with an app/ folder found; title id not read".to_string()));
        return Some(NonDiscHeaderMetadata { evidence, ..Default::default() });
    };
    let title_dir = fs.list(&app).into_iter().find(|e| e.is_dir && is_vita_title_id(&e.name));
    let Some(title_dir) = title_dir else {
        return Some(NonDiscHeaderMetadata { evidence, ..Default::default() });
    };
    evidence.push(Evidence::FileInFilesystem { filesystem: "exFAT", path: format!("app/{}/", title_dir.name), field: Some(format!("partition at 0x{partition:X}")) });
    let sfo = fs.find(&title_dir, "sce_sys").and_then(|d| fs.find(&d, "param.sfo")).and_then(|f| fs.read(&f, 0x10000)).and_then(|b| Sfo::parse(&b));
    if let Some(sfo) = &sfo {
        evidence.push(Evidence::FileInFilesystem { filesystem: "exFAT", path: format!("app/{}/sce_sys/param.sfo", title_dir.name), field: Some(sfo.summary()) });
    }
    let serial = sfo.as_ref().and_then(Sfo::title_id).or_else(|| sanitize_serial_opt(Some(title_dir.name.to_ascii_uppercase())));
    Some(NonDiscHeaderMetadata { title: sfo.as_ref().and_then(Sfo::title), serial, evidence, ..Default::default() })
}

/// Four letters and five digits (`PCSE00000`).
pub fn is_vita_title_id(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() == 9 && b[..4].iter().all(u8::is_ascii_alphabetic) && b[4..].iter().all(u8::is_ascii_digit)
}

/// Just enough exFAT to list directories and read a small file: the boot sector's geometry,
/// the FAT for cluster chains, and the file / stream / name directory entries.
mod exfat {
    use std::path::Path;

    use super::super::super::detect::disc_fs::read_exact_at;

    /// Directory listings and files larger than this are not read.
    const CAP: u64 = 4 << 20;

    pub struct ExFat<'a> {
        path: &'a Path,
        base: u64,
        cluster_size: u64,
        fat: u64,
        heap: u64,
        root_cluster: u32,
    }

    #[derive(Debug, Clone)]
    pub struct Entry {
        pub name: String,
        pub is_dir: bool,
        cluster: u32,
        size: u64,
        contiguous: bool,
    }

    impl<'a> ExFat<'a> {
        pub fn open(path: &'a Path, base: u64) -> Option<Self> {
            let boot = read_exact_at(path, base, 0x200).ok()?;
            if boot.get(3..11)? != b"EXFAT   " {
                return None;
            }
            let u32_at = |at: usize| boot.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64);
            let sector_shift = *boot.get(0x6C)? as u32;
            let cluster_shift = *boot.get(0x6D)? as u32;
            if !(9..=12).contains(&sector_shift) || cluster_shift > 25 {
                return None;
            }
            let sector = 1u64 << sector_shift;
            Some(Self {
                path,
                base,
                cluster_size: sector << cluster_shift,
                fat: u32_at(0x50)? * sector,
                heap: u32_at(0x58)? * sector,
                root_cluster: u32_at(0x60)? as u32,
            })
        }

        /// The root directory, which has no entry of its own: its size is unknown and it
        /// always follows the FAT.
        pub fn root(&self) -> Entry {
            Entry { name: String::new(), is_dir: true, cluster: self.root_cluster, size: CAP, contiguous: false }
        }

        fn chain(&self, entry: &Entry) -> Vec<u8> {
            let (contiguous, want) = (entry.contiguous, entry.size.min(CAP));
            let mut out = Vec::new();
            let mut cluster = entry.cluster;
            let mut steps = 0;
            while cluster >= 2 && (out.len() as u64) < want && steps < 4096 {
                let offset = self.base + self.heap + (cluster as u64 - 2) * self.cluster_size;
                let Ok(bytes) = read_exact_at(self.path, offset, self.cluster_size.min(want - out.len() as u64) as usize) else { break };
                out.extend_from_slice(&bytes);
                steps += 1;
                cluster = if contiguous {
                    cluster + 1
                } else {
                    match read_exact_at(self.path, self.base + self.fat + cluster as u64 * 4, 4) {
                        Ok(b) => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                        Err(_) => break,
                    }
                };
                if cluster >= 0xFFFF_FFF7 {
                    break;
                }
            }
            out
        }

        pub fn list(&self, dir: &Entry) -> Vec<Entry> {
            let bytes = self.chain(dir);
            let mut out = Vec::new();
            let mut at = 0;
            while at + 32 <= bytes.len() {
                let kind = bytes[at];
                if kind == 0x00 {
                    break;
                }
                if kind != 0x85 {
                    at += 32;
                    continue;
                }
                let secondary = bytes[at + 1] as usize;
                let attributes = u16::from_le_bytes([bytes[at + 4], bytes[at + 5]]);
                let stream = at + 32;
                if secondary < 2 || stream + 32 * secondary > bytes.len() || bytes[stream] != 0xC0 {
                    at += 32;
                    continue;
                }
                let name_len = bytes[stream + 3] as usize;
                let contiguous = bytes[stream + 1] & 0x02 != 0;
                let cluster = u32::from_le_bytes(bytes[stream + 0x14..stream + 0x18].try_into().unwrap_or_default());
                let size = u64::from_le_bytes(bytes[stream + 0x18..stream + 0x20].try_into().unwrap_or_default());
                let mut units = Vec::new();
                for part in 1..secondary {
                    let rec = stream + part * 32;
                    if bytes[rec] != 0xC1 {
                        break;
                    }
                    units.extend(bytes[rec + 2..rec + 32].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])));
                }
                units.truncate(name_len);
                out.push(Entry { name: String::from_utf16_lossy(&units), is_dir: attributes & 0x10 != 0, cluster, size, contiguous });
                at += 32 * (secondary + 1);
            }
            out
        }

        pub fn find(&self, dir: &Entry, name: &str) -> Option<Entry> {
            self.list(dir).into_iter().find(|e| e.name.eq_ignore_ascii_case(name))
        }

        pub fn read(&self, file: &Entry, cap: u64) -> Option<Vec<u8>> {
            (!file.is_dir && file.size <= cap).then(|| self.chain(file))
        }
    }
}

/// A Vita package: its header, then its SFO.
fn vita_pkg_metadata(path: &Path, pkg: &Pkg, mut evidence: Vec<Evidence>) -> Option<NonDiscHeaderMetadata> {
    if pkg.system() != Some(PkgSystem::Vita) {
        return None;
    }
    evidence.extend(pkg.evidence());
    let sfo = pkg.read_sfo(path);
    let mut kind = pkg.content_kind();
    if let (Some(sfo), Some((offset, _))) = (&sfo, pkg.sfo) {
        if kind == ContentKind::Game {
            kind = sfo.content_kind();
        }
        evidence.push(Evidence::Text { text: sfo.summary(), context: format!("PARAM.SFO at 0x{:X} (PKG metadata entry 14)", pkg.base + offset) });
    }
    evidence.push(Evidence::Note(format!("content: {}", kind.label())));
    let serial = sfo.as_ref().and_then(Sfo::title_id).or_else(|| pkg.title_id());
    let title = sfo.as_ref().and_then(Sfo::title);
    Some(NonDiscHeaderMetadata { title, serial, evidence, ..Default::default() })
}

/// The in-memory form of [`read_vita_metadata`] for a PKG: `bytes` holds the header and
/// metadata (and the SFO, when it is to be read). Kept for callers that already hold the bytes.
pub fn parse_psvita_pkg_metadata_from_bytes(bytes: &[u8]) -> LeafMetadata {
    let head = bytes.get(..Pkg::HEAD_LEN.min(bytes.len()))?;
    let meta_offset = read_u32_be(head, 0x08).unwrap_or(0) as usize;
    let pkg = Pkg::parse(head, bytes.get(meta_offset..).unwrap_or_default(), 0)?;
    if pkg.system() != Some(PkgSystem::Vita) {
        return None;
    }
    let sfo = pkg.sfo.and_then(|(offset, len)| Sfo::parse(bytes.get(offset as usize..offset as usize + len as usize)?));
    let serial = sfo.as_ref().and_then(Sfo::title_id).or_else(|| pkg.title_id());
    Some((sfo.as_ref().and_then(Sfo::title), serial.clone(), serial))
}

/// PocketStation "flash" dump: no precedent in `reference/old_archiver.rs` (unimplemented
/// there). A PocketStation file wraps a standard PS1 memory-card single-save container --
/// its Title sector's normally-zero-filled region at 0x50-0x5F carries PocketStation-only
/// fields, the load-bearing one being a 4-byte identifier at 0x52: `MCX0` (no snapshot) or
/// `MCX1` (with one), per the public spec
/// (problemkaputt.de/psxspx-pocketstation-file-header-icons.htm, "Title sector... PocketStation
/// files are identified by SC and MCX0 or MCX1 IDs"). The title text at 0x04 onward is Shift-JIS
/// (this project has no Shift-JIS decoder and isn't adding one just for this), so this
/// deliberately returns no title -- the `MCX0`/`MCX1` match alone is the real,
/// content-based identification.
pub fn parse_pocket_station_metadata_from_bytes(bytes: &[u8]) -> LeafMetadata {
    let id = bytes.get(0x52..0x56)?;
    if id == b"MCX0" || id == b"MCX1" {
        Some((None, None, None))
    } else {
        None
    }
}
