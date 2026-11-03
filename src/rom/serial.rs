//! Serials (product codes, title ids) of formats no DAT hashes: a `.cia`, an `.xiso`, a `.zar`,
//! a `.wua`. Each is read from the file's own header, from the manifest a copy here wrote
//! into it, or from its file name, and says which ([`SerialOrigin`]), because the three are
//! not equally sure. They are what lenient matching looks up in the DATs' serials.
//!
//! What each format carries, and the forms the DATs use for it:
//!
//! - **Xbox** (`default.xbe`): the certificate's title id, two publisher letters and a game
//!   number (`0x4D530004`), which Redump writes `MS-004` ([`xbox_title_id_serial`]). The XBE
//!   image header gives the base address at 0x104 and the certificate's address at 0x118; the
//!   title id is at 0x08 in the certificate (xboxdevwiki.net/XBE).
//! - **Xbox 360** (`default.xex`, big-endian XEX2): optional header `0x00040006`, the execution
//!   info, holds the title id at 0x0C (free60.org, XEX). The same id written `MS-2022` (game
//!   number in decimal) is the Redump form; the 8 hex digits are kept as well.
//! - **Wii U** (`.wua`): each title folder is `<title id>_v<version>`; the base game's
//!   `meta/meta.xml` has `<product_code>WUP-P-XXXX</product_code>`.
//! - **3DS** (`.cia`): the first content's NCCH product code (`CTR-P-XXXX`) and the TMD's
//!   title id, read by `platforms::nintendo`.
//! - **Vita, PSP, PS3** (`.vpk`, `.pkg`, `.psv`, a game-card `.img`, a game folder's
//!   `PARAM.SFO`): the title id (`PCSE00000`), read by `platforms::sony`. SFOs write it bare,
//!   Redump with a dash after the letters (`BLES-00000`); `database::serial_match` tries both.
//! - **PS1, PS2, PS3 discs**: the serial the disc's own reader finds (`SLUS_123.45`).
//! - **Switch** (`.nsp`, `.nsz`, `.xci`, `.xcz`): the title id, from a ticket's name inside,
//!   else from the file name, since the NCAs holding it are encrypted (`platforms::switch`).
//!
//! Every reader is best effort: a file that is damaged, or not what its extension says, gives
//! no serial rather than an error.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

use super::{cemu_wrapper, xbox};
use crate::common::models::Platform;

/// Where a serial came from, surest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerialOrigin {
    /// The file's own header (an XBE certificate, a XEX, an NCCH, a `meta.xml`).
    Header,
    /// The `romexplorer.json` a copy here wrote into the archive.
    Manifest,
    /// Only the file name says so.
    FileName,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FoundSerial {
    /// Upper case, in the form the DATs write it (`MS-2022`, `CTR-P-XXXX`, `SLUS-12345`).
    pub serial: String,
    pub origin: SerialOrigin,
}

/// Headers bigger than this are not read: none of the ones here comes close.
const HEADER_CAP: usize = 16 << 20;

/// Every serial `path` gives, header (and manifest) ones before file-name ones, each once.
/// `platform` narrows which file-name forms are looked for; `Unknown` tries every form that
/// cannot be mistaken for something else.
pub fn read(path: &Path, platform: Platform) -> Vec<FoundSerial> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let is_sfo = path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.eq_ignore_ascii_case("param.sfo"));
    let mut out = match ext.as_str() {
        _ if is_sfo => sony_serials(path),
        _ if is_ps_disc(&ext, platform) => disc_serials(path, platform),
        "vpk" | "pkg" | "psv" => sony_serials(path),
        "nsp" | "nsz" | "xci" | "xcz" => switch_serials(path),
        "img" if platform == Platform::PlayStationVita => sony_serials(path),
        "cia" => cia_serials(path),
        "xiso" | "iso" => xdvdfs_serials(path),
        "zar" => zar_serials(path),
        "wua" => wua_serials(path),
        _ => Vec::new(),
    };
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        out.extend(from_file_name(name, platform).into_iter().map(|serial| FoundSerial { serial, origin: SerialOrigin::FileName }));
    }
    dedupe(out)
}

/// Whether `ext` on `platform` is a PS1, PS2 or PS3 disc image, whose serial the disc itself
/// carries (`SYSTEM.CNF`, `PS3_DISC.SFB`).
pub fn is_ps_disc(ext: &str, platform: Platform) -> bool {
    matches!(platform, Platform::PlayStation | Platform::PlayStation2 | Platform::PlayStation3) && matches!(ext, "iso" | "bin" | "cue" | "chd" | "img")
}

/// A Vita, PSP or PS3 file's title id: a package's content id, a VPK's or a game folder's
/// `PARAM.SFO`, a `.psv`, a game-card image (`platforms::sony`).
fn sony_serials(path: &Path) -> Vec<FoundSerial> {
    found(super::platforms::sony::serial_and_kind(path).map(|(serial, _)| serial), SerialOrigin::Header)
}

/// A Switch package's title id: from the ticket inside, else from the `[0100…]` in its name
/// (`platforms::switch`; the NCAs that hold it are encrypted).
fn switch_serials(path: &Path) -> Vec<FoundSerial> {
    super::platforms::switch::title_id_of(path)
        .map(|(id, from_ticket)| FoundSerial { serial: id, origin: if from_ticket { SerialOrigin::Header } else { SerialOrigin::FileName } })
        .into_iter()
        .collect()
}

/// The serial on a PS1, PS2 or PS3 disc, as the platform's own reader finds it.
fn disc_serials(path: &Path, platform: Platform) -> Vec<FoundSerial> {
    found(super::detect::confirm_platform(path, platform).and_then(|m| m.serial_product_id), SerialOrigin::Header)
}

/// Keeps each serial once, at its surest origin, in first-seen order.
fn dedupe(found: Vec<FoundSerial>) -> Vec<FoundSerial> {
    let mut out: Vec<FoundSerial> = Vec::with_capacity(found.len());
    for f in found {
        match out.iter_mut().find(|o| o.serial == f.serial) {
            Some(o) => o.origin = o.origin.min(f.origin),
            None => out.push(f),
        }
    }
    out
}

fn found(serials: impl IntoIterator<Item = String>, origin: SerialOrigin) -> Vec<FoundSerial> {
    serials.into_iter().map(|serial| FoundSerial { serial, origin }).collect()
}

// ---------------------------------------------------------------------------
// Xbox / Xbox 360
// ---------------------------------------------------------------------------

/// An Xbox or Xbox 360 title id as Redump writes it: the two publisher letters, a dash and the
/// game number in decimal, at least three digits (`0x4D530004` → `MS-004`, `0x4D5307E6` →
/// `MS-2022`). `None` when the top two bytes are not letters (a homebrew or made-up id).
pub fn xbox_title_id_serial(raw: u32) -> Option<String> {
    let [a, b, hi, lo] = raw.to_be_bytes();
    if !(a.is_ascii_uppercase() && b.is_ascii_uppercase()) {
        return None;
    }
    Some(format!("{}{}-{:03}", a as char, b as char, u16::from_be_bytes([hi, lo])))
}

/// The serials of one title id: the Redump form, then the 8 hex digits.
fn xbox_serials(raw: u32) -> Vec<String> {
    xbox_title_id_serial(raw).into_iter().chain(std::iter::once(format!("{raw:08X}"))).collect()
}

fn u32_le(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

fn u32_be(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

/// The title id in an XBE's certificate. `bytes` is the start of the file, enough of it to
/// hold the headers (their size is at 0x108).
pub fn xbe_title_id(bytes: &[u8]) -> Option<u32> {
    if bytes.get(..4)? != b"XBEH" {
        return None;
    }
    let base = u32_le(bytes, 0x104)?;
    let cert = u32_le(bytes, 0x118)?.checked_sub(base)? as usize;
    u32_le(bytes, cert.checked_add(0x08)?)
}

/// How much of an XBE to read for [`xbe_title_id`]: its header size, from the first 0x10C
/// bytes.
fn xbe_header_len(start: &[u8]) -> Option<usize> {
    (start.get(..4)? == b"XBEH").then_some(())?;
    Some((u32_le(start, 0x108)? as usize).min(HEADER_CAP))
}

/// The title id in a XEX2's execution info. `bytes` is the start of the file, through the end
/// of its headers (the PE data offset at 0x08).
pub fn xex_title_id(bytes: &[u8]) -> Option<u32> {
    if bytes.get(..4)? != b"XEX2" {
        return None;
    }
    let count = u32_be(bytes, 0x14)? as usize;
    for i in 0..count.min(4096) {
        let entry = 0x18 + i * 8;
        if u32_be(bytes, entry)? == 0x0004_0006 {
            let info = u32_be(bytes, entry + 4)? as usize;
            return u32_be(bytes, info.checked_add(0x0C)?);
        }
    }
    None
}

/// How much of a XEX to read for [`xex_title_id`]: through its headers.
fn xex_header_len(start: &[u8]) -> Option<usize> {
    (start.get(..4)? == b"XEX2").then_some(())?;
    Some((u32_be(start, 0x08)? as usize).min(HEADER_CAP))
}

/// The boot file's title id, from a reader of its first `n` bytes: a first look to learn how
/// big its headers are, then those headers.
fn boot_title_id(mut read: impl FnMut(usize) -> Option<Vec<u8>>) -> Option<u32> {
    let start = read(0x200)?;
    if let Some(len) = xbe_header_len(&start) {
        let bytes = if len > start.len() { read(len)? } else { start };
        return xbe_title_id(&bytes);
    }
    let len = xex_header_len(&start)?;
    let bytes = if len > start.len() { read(len)? } else { start };
    xex_title_id(&bytes)
}

/// An Xbox or Xbox 360 disc image, Redump layout or XISO: the boot file's title id, read
/// through the image's own XDVDFS.
pub fn xdvdfs_serials(path: &Path) -> Vec<FoundSerial> {
    found(image_title_id(path).map(xbox_serials).unwrap_or_default(), SerialOrigin::Header)
}

/// The title id of an Xbox or Xbox 360 disc image's boot file (`default.xex`, else
/// `default.xbe`), read through its XDVDFS.
pub fn image_title_id(path: &Path) -> Option<u32> {
    let mut file = File::open(path).ok()?;
    let volume = xbox::open_volume(&mut file).ok()??;
    let entries = xbox::entries(&mut file, &volume).ok()?;
    let boot = ["default.xex", "default.xbe"].iter().find_map(|name| entries.iter().find(|e| !e.is_dir && e.path.eq_ignore_ascii_case(name)))?.clone();
    boot_title_id(|n| {
        let mut buf = vec![0u8; n.min(boot.size as usize)];
        file.seek(SeekFrom::Start(boot.offset)).ok()?;
        file.read_exact(&mut buf).ok()?;
        Some(buf)
    })
}

/// A `.zar` (Xbox 360): the title id its manifest records when a copy here wrote it, else
/// `default.xex`'s, read out of the archive.
pub fn zar_serials(path: &Path) -> Vec<FoundSerial> {
    if !cemu_wrapper::available() {
        return Vec::new();
    }
    let from_manifest = cemu_wrapper::manifest(path).ok().flatten().and_then(|m| m.titles.iter().find_map(|t| u32::from_str_radix(t.title_id.trim(), 16).ok()));
    if let Some(raw) = from_manifest {
        return found(xbox_serials(raw), SerialOrigin::Manifest);
    }
    let raw = boot_title_id(|n| cemu_wrapper::read_file(path, "default.xex", n).ok().flatten());
    found(raw.map(xbox_serials).unwrap_or_default(), SerialOrigin::Header)
}

// ---------------------------------------------------------------------------
// Wii U
// ---------------------------------------------------------------------------

/// `<product_code>` from a title's `meta/meta.xml` (`WUP-P-XXXX`).
pub fn wiiu_product_code(meta_xml: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(meta_xml);
    let start = text.find("<product_code")?;
    let open_end = start + text[start..].find('>')? + 1;
    let close = open_end + text[open_end..].find("</product_code>")?;
    let code = text[open_end..close].trim().to_ascii_uppercase();
    (!code.is_empty() && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).then_some(code)
}

/// What a Wii U title id is: the high half says base game, update or DLC.
fn wiiu_kind(title_id: u64) -> u32 {
    (title_id >> 32) as u32
}

/// A `.wua`: each title's product code from its `meta.xml`, the base game's first, then the
/// title ids themselves. Any `.wua` has these, so a manifest adds nothing here.
pub fn wua_serials(path: &Path) -> Vec<FoundSerial> {
    if !cemu_wrapper::available() {
        return Vec::new();
    }
    let Ok(mut titles) = cemu_wrapper::wua_titles(path) else { return Vec::new() };
    // The base game (0005_0000), then updates (0005_000E), then DLC (0005_000C).
    titles.sort_by_key(|(id, _)| match wiiu_kind(*id) {
        0x0005_0000 => 0,
        0x0005_000E => 1,
        _ => 2,
    });
    let mut serials = Vec::new();
    for (id, version) in &titles {
        let meta = cemu_wrapper::read_file(path, &format!("{id:016x}_v{version}/meta/meta.xml"), 64 * 1024).ok().flatten();
        serials.extend(meta.as_deref().and_then(wiiu_product_code));
    }
    serials.extend(titles.iter().map(|(id, _)| format!("{id:016X}")));
    found(serials, SerialOrigin::Header)
}

// ---------------------------------------------------------------------------
// 3DS
// ---------------------------------------------------------------------------

/// A `.cia`: the product code, then the title id.
pub fn cia_serials(path: &Path) -> Vec<FoundSerial> {
    let Some(meta) = super::platforms::nintendo::read_3ds_cia_metadata(path) else { return Vec::new() };
    // `serial` is "<product code> / <title id>" when both were read.
    let mut serials: Vec<String> = meta.serial.iter().flat_map(|s| s.split(" / ")).map(|s| s.trim().to_ascii_uppercase()).filter(|s| !s.is_empty()).collect();
    serials.extend(meta.manufacturer_id.map(|s| s.to_ascii_uppercase()));
    found(serials, SerialOrigin::Header)
}

// ---------------------------------------------------------------------------
// File names
// ---------------------------------------------------------------------------

/// A family of serial forms and the platforms it belongs to.
struct NameForm {
    platforms: &'static [Platform],
    /// Looked for only when the platform is known to be one of `platforms`: a form that a
    /// CRC or an unrelated code in a name could match by chance.
    needs_platform: bool,
    regex: &'static str,
    normalize: fn(&regex::Captures) -> Option<Vec<String>>,
}

const NINTENDO: &[Platform] = &[
    Platform::Nintendo3ds,
    Platform::NewNintendo3ds,
    Platform::NintendoDs,
    Platform::NintendoDsi,
    Platform::GameBoyAdvance,
    Platform::GameBoy,
    Platform::GameBoyColor,
    Platform::WiiU,
    Platform::Wii,
    Platform::GameCube,
    Platform::NintendoSwitch,
    Platform::NintendoSwitch2,
    Platform::Nintendo64,
];
const SONY: &[Platform] = &[
    Platform::PlayStation,
    Platform::PlayStation2,
    Platform::PlayStation3,
    Platform::PlayStation4,
    Platform::PlayStation5,
    Platform::PlayStationPortable,
    Platform::PlayStationVita,
    Platform::PspPs1Classic,
];
const XBOX: &[Platform] = &[Platform::Xbox, Platform::Xbox360];
const TITLE_ID: &[Platform] = &[Platform::Nintendo3ds, Platform::NewNintendo3ds, Platform::WiiU, Platform::NintendoSwitch, Platform::NintendoSwitch2];

fn whole(c: &regex::Captures) -> Option<Vec<String>> {
    Some(vec![c[0].to_ascii_uppercase()])
}

/// `SLUS_123.45`, `SLUS 12345`, `slus-12345` → `SLUS-12345`.
fn sony(c: &regex::Captures) -> Option<Vec<String>> {
    Some(vec![format!("{}-{}{}", c[1].to_ascii_uppercase(), &c[2], &c[3])])
}

fn xbox_hex(c: &regex::Captures) -> Option<Vec<String>> {
    let raw = u32::from_str_radix(&c[1], 16).ok()?;
    xbox_title_id_serial(raw)?;
    Some(xbox_serials(raw))
}

/// Title ids of the consoles that name their digital titles by one: 3DS (`0004…`), Wii U
/// (`0005…`), Switch (`01…`, a base game ending in `000`).
fn title_id(c: &regex::Captures) -> Option<Vec<String>> {
    let id = c[1].to_ascii_uppercase();
    let known = id.starts_with("00040000") || id.starts_with("0004000E") || id.starts_with("0004008C") || id.starts_with("0005000") || (id.starts_with("01") && id.ends_with("000"));
    known.then(|| vec![id])
}

const NAME_FORMS: &[NameForm] = &[
    // Nintendo's own: CTR-P-XXXX, WUP-P-XXXX, NTR-P-XXXX (and the region suffix some add).
    NameForm { platforms: NINTENDO, needs_platform: false, regex: r"(?i)\b(?:CTR|KTR|TWL|NTR|AGB|WUP|HAC|BEE|NUS|CGB|DMG)-[A-Z]-[A-Z0-9]{4}(?:-[A-Z]{3})?\b", normalize: whole },
    // Disc codes: DOL-XXXX-USA, RVL-XXXX-EUR.
    NameForm { platforms: NINTENDO, needs_platform: false, regex: r"(?i)\b(?:DOL|RVL|WUP|NTR|TWL|CTR|AGB)-[A-Z0-9]{4}-[A-Z]{3}\b", normalize: whole },
    NameForm {
        platforms: SONY,
        needs_platform: false,
        regex: r"(?i)\b(SLUS|SCUS|SLES|SCES|SLPS|SLPM|SCPS|SCAJ|SLAJ|SLKA|SCKA|SCED|SLED|PAPX|BLUS|BCUS|BLES|BCES|BLJM|BCJS|BLJS|BLAS|BCAS|BLKS|BCKS|NPUB|NPEB|NPJB|NPUA|NPEA|NPJA|ULUS|UCUS|ULES|UCES|ULJM|UCJS|ULJS|ULAS|UCAS|ULKS|NPUH|NPEH|NPJH|NPUG|NPEG|NPJG|PCSE|PCSA|PCSB|PCSG|PCSH|PCSF|PCSC|PCSD|CUSA|PLJS|PLJM|PPSA|ECAS|ELAS|ELJM|ELJS)[-_ ]?(\d{3})\.?(\d{2})\b",
        normalize: sony,
    },
    NameForm { platforms: TITLE_ID, needs_platform: false, regex: r"(?i)\b([0-9A-F]{16})\b", normalize: title_id },
    // An Xbox title id, 8 hex digits: only when the platform is known, since a CRC-32 in
    // brackets looks the same.
    NameForm { platforms: XBOX, needs_platform: true, regex: r"(?i)\b([0-9A-F]{8})\b", normalize: xbox_hex },
    NameForm { platforms: XBOX, needs_platform: true, regex: r"\b[A-Z]{2}-\d{3,5}\b", normalize: whole },
];

fn name_regexes() -> &'static [Regex] {
    static RES: OnceLock<Vec<Regex>> = OnceLock::new();
    RES.get_or_init(|| NAME_FORMS.iter().map(|f| Regex::new(f.regex).expect("serial pattern")).collect())
}

/// The serials a file name spells out, for `platform` (`Unknown`: every form that needs no
/// platform). In the order they appear in the name, each once.
pub fn from_file_name(name: &str, platform: Platform) -> Vec<String> {
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    let mut hits: Vec<(usize, String)> = Vec::new();
    for (form, re) in NAME_FORMS.iter().zip(name_regexes()) {
        let applies = if platform == Platform::Unknown { !form.needs_platform } else { form.platforms.contains(&platform) };
        if !applies {
            continue;
        }
        for c in re.captures_iter(stem) {
            for s in (form.normalize)(&c).unwrap_or_default() {
                hits.push((c.get(0).map_or(0, |m| m.start()), s));
            }
        }
    }
    hits.sort_by_key(|(at, _)| *at);
    let mut out: Vec<String> = Vec::new();
    for (_, s) in hits {
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xbox_title_ids_in_the_redump_form() {
        assert_eq!(xbox_title_id_serial(0x4D53_0004).as_deref(), Some("MS-004"));
        assert_eq!(xbox_title_id_serial(0x4D53_07E6).as_deref(), Some("MS-2022"));
        assert_eq!(xbox_title_id_serial(0x0000_1234), None);
        assert_eq!(xbox_serials(0x4541_0017), ["EA-023", "45410017"]);
    }

    #[test]
    fn an_xbe_certificate_and_a_xex_execution_info() {
        let mut xbe = vec![0u8; 0x400];
        xbe[..4].copy_from_slice(b"XBEH");
        xbe[0x104..0x108].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        xbe[0x108..0x10C].copy_from_slice(&0x400u32.to_le_bytes());
        xbe[0x118..0x11C].copy_from_slice(&0x0001_0200u32.to_le_bytes());
        xbe[0x208..0x20C].copy_from_slice(&0x4D53_0004u32.to_le_bytes());
        assert_eq!(xbe_title_id(&xbe), Some(0x4D53_0004));
        assert_eq!(boot_title_id(|n| Some(xbe[..n.min(xbe.len())].to_vec())), Some(0x4D53_0004));

        let mut xex = vec![0u8; 0x300];
        xex[..4].copy_from_slice(b"XEX2");
        xex[0x08..0x0C].copy_from_slice(&0x300u32.to_be_bytes());
        xex[0x14..0x18].copy_from_slice(&2u32.to_be_bytes());
        xex[0x18..0x1C].copy_from_slice(&0x0001_83FFu32.to_be_bytes());
        xex[0x20..0x24].copy_from_slice(&0x0004_0006u32.to_be_bytes());
        xex[0x24..0x28].copy_from_slice(&0x250u32.to_be_bytes());
        xex[0x25C..0x260].copy_from_slice(&0x4D53_07E6u32.to_be_bytes());
        assert_eq!(xex_title_id(&xex), Some(0x4D53_07E6));
        assert_eq!(boot_title_id(|n| Some(xex[..n.min(xex.len())].to_vec())), Some(0x4D53_07E6));
        assert_eq!(xex_title_id(b"XEX2"), None);
    }

    #[test]
    fn a_meta_xml_product_code() {
        let xml = b"<?xml version=\"1.0\"?><menu><product_code type=\"string\" length=\"32\">WUP-P-XXXX</product_code></menu>";
        assert_eq!(wiiu_product_code(xml).as_deref(), Some("WUP-P-XXXX"));
        assert_eq!(wiiu_product_code(b"<menu></menu>"), None);
    }

    #[test]
    fn serials_in_file_names() {
        assert_eq!(from_file_name("Game Title (USA) [CTR-P-ABCD].cia", Platform::Unknown), ["CTR-P-ABCD"]);
        assert_eq!(from_file_name("Game Title (Japan) (SLPS_012.34).chd", Platform::PlayStation), ["SLPS-01234"]);
        assert_eq!(from_file_name("Game [0004000000ABCD00].cia", Platform::Nintendo3ds), ["0004000000ABCD00"]);
        // A CRC-32 is not taken for an Xbox title id unless the platform says Xbox.
        assert!(from_file_name("Game [4D5307E6].zar", Platform::Unknown).is_empty());
        assert_eq!(from_file_name("Game [4D5307E6].zar", Platform::Xbox360), ["MS-2022", "4D5307E6"]);
        assert!(from_file_name("Game [DEADBEEF].zar", Platform::Xbox360).is_empty());
        assert_eq!(from_file_name("Game (MS-004).xiso", Platform::Xbox), ["MS-004"]);
        // A form for another console is not looked for.
        assert!(from_file_name("Game [CTR-P-ABCD].iso", Platform::PlayStation2).is_empty());
        assert!(from_file_name("Game Title (USA).cia", Platform::Nintendo3ds).is_empty());
    }

    #[test]
    fn a_serial_keeps_its_surest_origin() {
        let got = dedupe(vec![
            FoundSerial { serial: "A".into(), origin: SerialOrigin::FileName },
            FoundSerial { serial: "B".into(), origin: SerialOrigin::Header },
            FoundSerial { serial: "A".into(), origin: SerialOrigin::Header },
        ]);
        assert_eq!(got, [FoundSerial { serial: "A".into(), origin: SerialOrigin::Header }, FoundSerial { serial: "B".into(), origin: SerialOrigin::Header }]);
    }
}
