//! Digital-download and loose-file games: installed-title folders, extracted game folders and
//! package files for Nintendo DSi, Nintendo 3DS, PS3, PSP and Vita, and Wii / Wii U CDN folders.
//!
//! A game folder is identified by content signatures only. Names count as evidence when they
//! are part of the game's own tree (`PS3_GAME/PARAM.SFO`, `PSP_GAME/PARAM.SFO`,
//! `PS3_DISC.SFB`, `sce_sys/param.sfo`); the name of the folder or file the dump sits in never does.
//!
//! Signatures used (each identifies its system by itself):
//!
//! | System | Signature |
//! |--------|-----------|
//! | DSi / DSiWare | TMD whose title-id high word is a DSi one (`0003xxxx`, or the TWL ids `00048004`/`00048005`/`0004800F`), DSiWare being `00030004`/`00048004`; SRL header with the Nintendo logo and platform word `03 00` at 0x236, file type `04` for DSiWare |
//! | 3DS    | TMD whose title-id high word is a 3DS one (`0004xxxx`); `NCCH`/`NCSD` at 0x100; `3DSX` at 0 |
//! | PS3    | PKG header with platform `0001` at 0x06 (`sony::Pkg`); `PS3_GAME/PARAM.SFO` (`\0PSF`, CATEGORY `DG`/`HG`/`GD`, TITLE_ID); `PS3_DISC.SFB` |
//! | PSP / PSP PS1 Classic | PKG with platform `0002` and a PSP (`07`/`0E`/`0F`/`10`) or PS1 (`06`) content type; `PSP_GAME/PARAM.SFO` (`\0PSF`, DISC_ID or TITLE_ID); PBP header (`\0PBP`), a PS1 classic when its PSAR starts with `PSISOIMG` |
//! | Vita   | `sce_sys/param.sfo` whose keys name a Vita firmware (`PSP2_SYSTEM_VER`/`PSP2_DISP_VER`) or whose CATEGORY is a Vita one (`gd`/`gc`/`gp`/`ac`); `sce_sys/package/work.bin` marks a NoNpDrm dump |
//! | Wii / Wii U (CDN folders) | TMD whose title-id high word is a Wii one (`0001xxxx`, vWii `0007xxxx`) or a Wii U one (`0005xxxx`) |
//!
//! Not used (no signature that separates the system): encrypted DSiWare exports, WAD files,
//! extracted 3DS ExeFS/RomFS trees, bare `\0PSF` files, `.rap` files, executables (a Vita
//! `eboot.bin` and a PS3 one both start `SCE\0`). A PSP or Vita package inside a folder does not
//! make the folder a game: those sets keep their packages as files.

use std::path::Path;

use crate::common::models::Platform;

use super::super::models::{sanitize_serial_opt, sanitize_title_opt};
use super::super::detect::disc_fs::read_exact_at;
use super::super::detect::reason::Evidence;
use super::sony::{ContentKind, Pkg, PkgSystem, Sfo};
use super::nintendo::{nintendo_logo_crc_ok, parse_tmd_signature_layout, NINTENDO_LOGO_LEN};
use super::{directory_payload_paths, read_ascii_field, read_u64_be, NonDiscHeaderMetadata};

const MAX_FILES_SCANNED: usize = 4096;
/// Files at a game tree's root that identify it on their own.
const ROOT_SIGNATURES: [&str; 5] = ["sce_sys/param.sfo", "sce_sys/package/work.bin", "PS3_GAME/PARAM.SFO", "PS3_DISC.SFB", "PSP_GAME/PARAM.SFO"];
const HEAD_LEN: usize = 0x240;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum System {
    Dsi,
    DsiWare,
    ThreeDs,
    Ps3,
    Psp,
    PspPs1Classic,
    /// A Wii (or vWii) title's CDN folder: its TMD.
    Wii,
    /// A Wii U title's CDN folder: its TMD.
    WiiU,
    /// A Vita game, update or add-on folder (`sce_sys/param.sfo`).
    Vita,
    /// Another system's signature (Vita packages, PS1 classics on PSP).
    Other,
}

/// What a game folder or package says about a candidate platform.
#[derive(Debug, Clone)]
pub enum DownloadVerdict {
    /// The platform's own signature was found.
    Match(NonDiscHeaderMetadata),
    /// A signature of a different system was found.
    Conflict,
    /// No signature either way.
    Unknown,
}

pub fn is_download_platform(platform: Platform) -> bool {
    system_of(platform).is_some()
}

/// The platform kept apart from `platform` for detection that is the same console: DSi and
/// DSiWare. A folder of one is a whole game under the other's folder too -- a DSi CDN dump's
/// TMD names it DSiWare, while the collection files it as DSi.
pub fn same_console(platform: Platform) -> Option<Platform> {
    match platform {
        Platform::NintendoDsi => Some(Platform::DsiWare),
        Platform::DsiWare => Some(Platform::NintendoDsi),
        // A Wii U's CDN holds vWii titles (Wii TMDs) beside its own.
        Platform::Wii => Some(Platform::WiiU),
        Platform::WiiU => Some(Platform::Wii),
        _ => None,
    }
}

fn system_of(platform: Platform) -> Option<System> {
    match platform {
        Platform::NintendoDsi => Some(System::Dsi),
        Platform::DsiWare => Some(System::DsiWare),
        Platform::Nintendo3ds | Platform::NewNintendo3ds => Some(System::ThreeDs),
        Platform::PlayStation3 => Some(System::Ps3),
        Platform::PlayStationPortable => Some(System::Psp),
        Platform::PspPs1Classic => Some(System::PspPs1Classic),
        Platform::Wii => Some(System::Wii),
        Platform::WiiU => Some(System::WiiU),
        Platform::PlayStationVita => Some(System::Vita),
        _ => None,
    }
}

struct Found {
    system: System,
    kind: &'static str,
    evidence: Vec<Evidence>,
    title: Option<String>,
    serial: Option<String>,
    /// Higher wins when several signatures each carry a serial.
    serial_priority: u8,
}

/// A directory input, or a package file (PKG/PBP magic): the shapes that go through this module
/// instead of the disc-image path for PS3 and PSP.
pub fn is_download_input(path: &Path) -> bool {
    if path.is_dir() {
        return true;
    }
    matches!(read_exact_at(path, 0, 4).ok().as_deref(), Some([0x7F, b'P', b'K', b'G']) | Some([0x00, b'P', b'B', b'P']))
}

pub fn verdict_for_directory(dir: &Path, target: Platform) -> DownloadVerdict {
    let Some(want) = system_of(target) else { return DownloadVerdict::Unknown };
    // A game tree's own root files first: a Vita or PS3 game holds thousands of files, and
    // its `sce_sys`/`PS3_GAME` can sort after all of them.
    let rooted: Vec<Found> = ROOT_SIGNATURES.iter().filter_map(|rel| classify_file(rel, &dir.join(rel))).collect();
    if rooted.iter().any(|item| item.system == want) {
        return decide(want, rooted);
    }
    let mut found = Vec::new();
    for file in directory_payload_paths(dir).into_iter().take(MAX_FILES_SCANNED) {
        let rel = file.strip_prefix(dir).unwrap_or(&file).to_string_lossy().replace('\\', "/");
        if let Some(mut item) = classify_file(&rel, &file) {
            // A PSP package in a folder says nothing about the folder: PSP and Vita sets keep
            // their packages as files of their own, while a PS3 one keeps a package and its
            // licence (`.rap`) together.
            if item.kind == "pkg" && item.system != System::Ps3 {
                item.system = System::Other;
            }
            found.push(item);
        }
    }
    decide(want, found)
}

pub fn verdict_for_file(path: &Path, target: Platform) -> DownloadVerdict {
    let Some(want) = system_of(target) else { return DownloadVerdict::Unknown };
    if !path.is_file() {
        return DownloadVerdict::Unknown;
    }
    // A lone file has no tree around it, so its own name is never consulted.
    decide(want, classify_file("", path).into_iter().collect())
}

fn decide(want: System, found: Vec<Found>) -> DownloadVerdict {
    let matches: Vec<&Found> = found.iter().filter(|item| item.system == want).collect();
    if matches.is_empty() {
        return if found.is_empty() { DownloadVerdict::Unknown } else { DownloadVerdict::Conflict };
    }

    let mut evidence = Vec::new();
    let mut seen_kinds: Vec<&str> = Vec::new();
    for item in &matches {
        if !seen_kinds.contains(&item.kind) {
            seen_kinds.push(item.kind);
            evidence.extend(item.evidence.iter().cloned());
        }
    }
    let title = matches.iter().find_map(|item| item.title.clone());
    let serial = matches.iter().filter(|item| item.serial.is_some()).max_by_key(|item| item.serial_priority).and_then(|item| item.serial.clone());
    DownloadVerdict::Match(NonDiscHeaderMetadata { title, serial, evidence, ..Default::default() })
}

fn classify_file(rel: &str, path: &Path) -> Option<Found> {
    let len = std::fs::metadata(path).ok()?.len() as usize;
    if len < 4 {
        return None;
    }
    let head = read_exact_at(path, 0, len.min(HEAD_LEN)).ok()?;

    if let Some(found) = classify_tmd(&head) {
        return Some(found);
    }
    if let Some(found) = head.starts_with(&Pkg::MAGIC).then(|| classify_pkg(path)).flatten() {
        return Some(found);
    }
    if let Some(found) = classify_pbp(path, len, &head) {
        return Some(found);
    }
    if let Some(found) = classify_srl(&head) {
        return Some(found);
    }
    if let Some(found) = classify_nintendo_3ds_container(&head) {
        return Some(found);
    }
    classify_in_tree_file(rel, path, &head)
}

// ---------------------------------------------------------------------------
// Nintendo
// ---------------------------------------------------------------------------

/// A TMD is recognized by a valid signature-type layout, a sane content count and a title-id
/// high word that belongs to a known system.
fn classify_tmd(head: &[u8]) -> Option<Found> {
    let header_offset = parse_tmd_signature_layout(head)?;
    let title_id = read_u64_be(head, header_offset + 0x4C)?;
    let content_count = super::read_u16_be(head, header_offset + 0x9E).unwrap_or(0);
    if content_count == 0 || content_count > 0x400 {
        return None;
    }

    let high = (title_id >> 32) as u32;
    let (system, label) = match high >> 16 {
        // 0003_0004 (and its 3DS-side twin 0004_8004) is downloadable DSiWare; the other 0003xxxx
        // and TWL ids are DSi system software and data.
        0x0003 if high & 0xFFFF == 0x0004 => (System::DsiWare, "DSiWare title"),
        0x0003 => (System::Dsi, "DSi title"),
        0x0004 if high & 0xFFFF == 0x8004 => (System::DsiWare, "DSiWare (TWL) title on 3DS"),
        0x0004 if matches!(high & 0xFFFF, 0x8005 | 0x800F) => (System::Dsi, "DSi (TWL) title on 3DS"),
        0x0004 => (System::ThreeDs, "3DS title"),
        0x0001 => (System::Wii, "Wii title"),
        0x0007 => (System::Wii, "vWii title"),
        0x0005 => (System::WiiU, "Wii U title"),
        _ => return None,
    };

    let serial = sanitize_serial_opt(Some(format!("{title_id:016X}")));
    Some(Found {
        system,
        kind: "tmd",
        evidence: vec![Evidence::Bytes {
            offset_start: header_offset + 0x4C,
            offset_end: header_offset + 0x54,
            bytes: head.get(header_offset + 0x4C..header_offset + 0x54)?.to_vec(),
            note: format!("TMD title id, high word {high:08X}: {label}"),
        }],
        title: None,
        serial,
        serial_priority: 0,
    })
}

/// A DSi executable (SRL): the Nintendo boot logo plus the platform word of its title id at
/// 0x236 (`03 00`); the word before it is the file type (`04` marks downloadable DSiWare).
fn classify_srl(head: &[u8]) -> Option<Found> {
    if head.len() < 0x238 || !nintendo_logo_crc_ok(head.get(0xC0..0xC0 + NINTENDO_LOGO_LEN)?) {
        return None;
    }
    if head.get(0x236..0x238)? != [0x03, 0x00] {
        return None;
    }
    let title = read_ascii_field(head.get(0x00..0x0C)?).and_then(|value| sanitize_title_opt(Some(value)));
    let serial = read_ascii_field(head.get(0x0C..0x10)?)
        .filter(|value| value.len() == 4 && value.chars().all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit()))
        .and_then(|value| sanitize_serial_opt(Some(value)));
    let file_type = head.get(0x234).copied().unwrap_or(0);
    Some(Found {
        system: if file_type == 0x04 { System::DsiWare } else { System::Dsi },
        kind: "srl",
        evidence: vec![
            Evidence::Verified { offset_start: 0xC0, offset_end: 0xC0 + NINTENDO_LOGO_LEN, description: "Nintendo boot logo (CRC-16 0xCF56)".to_string() },
            Evidence::Bytes {
                offset_start: 0x234,
                offset_end: 0x238,
                bytes: head.get(0x234..0x238)?.to_vec(),
                note: format!("DSi title-id file type and platform word ({})", if file_type == 0x04 { "DSiWare" } else { "cartridge or other" }),
            },
        ],
        title,
        serial,
        serial_priority: 2,
    })
}

fn classify_nintendo_3ds_container(head: &[u8]) -> Option<Found> {
    let magic = |offset: usize, bytes: &[u8]| Evidence::Magic { offset, bytes: bytes.to_vec() };
    if matches!(head.get(0x100..0x104), Some(b"NCCH") | Some(b"NCSD")) {
        return Some(Found {
            system: System::ThreeDs,
            kind: "ncch",
            evidence: vec![magic(0x100, head.get(0x100..0x104)?)],
            title: None,
            serial: None,
            serial_priority: 0,
        });
    }
    if head.starts_with(b"3DSX") {
        return Some(Found { system: System::ThreeDs, kind: "3dsx", evidence: vec![magic(0, b"3DSX")], title: None, serial: None, serial_priority: 0 });
    }
    None
}

// ---------------------------------------------------------------------------
// Sony
// ---------------------------------------------------------------------------

/// A PKG (`sony::Pkg`): the platform word separates PS3, the content type separates PSP from
/// Vita and PS1 classics. The title id comes from the content id; a PS3 or PSP package's SFO is
/// encrypted, so there is no title.
fn classify_pkg(path: &Path) -> Option<Found> {
    let pkg = Pkg::read(path)?;
    let system = match pkg.system() {
        Some(PkgSystem::Ps3) => System::Ps3,
        Some(PkgSystem::Psp) => System::Psp,
        Some(PkgSystem::PspPs1Classic) => System::PspPs1Classic,
        Some(PkgSystem::Vita) | None => System::Other,
    };
    let mut evidence = pkg.evidence();
    let mut title = None;
    let mut kind = pkg.content_kind();
    if system != System::Other {
        // The `scan_pkg_titles` setting: the SFO out of the encrypted body.
        if let Some(sfo) = pkg.decrypt_sfo(path) {
            evidence.push(Evidence::Text { text: sfo.summary(), context: "PARAM.SFO in the package's encrypted body".to_string() });
            title = sfo.title();
            if kind == ContentKind::UpdateOrDlc || kind == ContentKind::Game {
                kind = match sfo.text("CATEGORY") {
                    Some("GP") => ContentKind::Update,
                    Some("AC") => ContentKind::Dlc,
                    _ if sfo.content_kind() != ContentKind::Game => sfo.content_kind(),
                    _ => kind,
                };
            }
        }
        evidence.push(Evidence::Note(format!("content: {}", kind.label())));
    }
    Some(Found { system, kind: "pkg", evidence, title, serial: pkg.title_id(), serial_priority: 1 })
}

/// `00 'PBP'` at 0; the PSAR section offset is the u32 at 0x24. A PSAR that starts with
/// `PSISOIMG` is a PS1 classic ([`Platform::PspPs1Classic`]); anything else (an empty PSAR for homebrew, `NPUMDIMG` for a PSN
/// UMD) is a PSP game.
fn classify_pbp(path: &Path, len: usize, head: &[u8]) -> Option<Found> {
    if !head.starts_with(&[0x00, b'P', b'B', b'P']) || head.len() < 0x28 {
        return None;
    }
    let u32_le = |offset: usize| head.get(offset..offset + 4).map(|raw| u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize);
    let psar = u32_le(0x24)?;
    let psar_magic = if psar + 8 <= len { read_exact_at(path, psar as u64, 8).ok() } else { None };

    let mut evidence = vec![Evidence::Magic { offset: 0, bytes: head[0..4].to_vec() }];
    let system = match psar_magic.as_deref() {
        Some(b"PSISOIMG") => {
            evidence.push(Evidence::Magic { offset: psar, bytes: b"PSISOIMG".to_vec() });
            System::PspPs1Classic
        }
        Some(b"NPUMDIMG") => {
            evidence.push(Evidence::Magic { offset: psar, bytes: b"NPUMDIMG".to_vec() });
            System::Psp
        }
        _ => System::Psp,
    };

    // The embedded PARAM.SFO carries the title and id, and whether this is a game at all: a
    // DLC folder's `PARAM.pbp` says `BOOTABLE 0`, and its id is the game's.
    let sfo = match (u32_le(0x08), u32_le(0x0C)) {
        (Some(sfo), Some(next)) if sfo < next && next <= len && next - sfo <= 0x10000 => read_exact_at(path, sfo as u64, next - sfo).ok().and_then(|bytes| Sfo::parse(&bytes)),
        _ => None,
    };
    if let Some(sfo) = &sfo {
        evidence.push(Evidence::Text { text: sfo.summary(), context: "PBP's embedded PARAM.SFO".to_string() });
        evidence.push(Evidence::Note(format!("content: {}", sfo.content_kind().label())));
    }
    let title = sfo.as_ref().and_then(Sfo::title);
    let serial = sfo.as_ref().and_then(Sfo::title_id);
    Some(Found { system, kind: "pbp", evidence, title, serial, serial_priority: 2 })
}

/// Files whose meaning comes from their place in the game's own tree.
fn classify_in_tree_file(rel: &str, path: &Path, head: &[u8]) -> Option<Found> {
    let components: Vec<&str> = rel.split('/').filter(|part| !part.is_empty()).collect();
    let name = components.last()?;

    if name.eq_ignore_ascii_case("PS3_DISC.SFB") && head.starts_with(b".SFB") {
        return Some(Found {
            system: System::Ps3,
            kind: "sfb",
            evidence: vec![Evidence::FileInFilesystem { filesystem: "directory", path: "PS3_DISC.SFB".to_string(), field: Some("'.SFB' magic".to_string()) }],
            title: None,
            serial: None,
            serial_priority: 0,
        });
    }

    // NoNpDrm's fake licence: `sce_sys/package/work.bin`, 512 bytes.
    if name.eq_ignore_ascii_case("work.bin") && components.len() >= 3 && components[components.len() - 2].eq_ignore_ascii_case("package") && components[components.len() - 3].eq_ignore_ascii_case("sce_sys") {
        return Some(Found {
            system: System::Vita,
            kind: "work.bin",
            evidence: vec![Evidence::FileInFilesystem { filesystem: "directory", path: "sce_sys/package/work.bin".to_string(), field: Some("licence: a NoNpDrm dump".to_string()) }],
            title: None,
            serial: None,
            serial_priority: 0,
        });
    }

    if !name.eq_ignore_ascii_case("PARAM.SFO") || components.len() < 2 || !head.starts_with(&[0x00, b'P', b'S', b'F']) {
        return None;
    }
    let parent = components[components.len() - 2];
    let bytes = std::fs::read(path).ok()?;
    let sfo = Sfo::parse(&bytes)?;
    let value = |key: &str| sfo.text(key);
    let (title, serial) = (sfo.title(), sfo.title_id());

    let (system, kind, tree_path, field) = if parent.eq_ignore_ascii_case("sce_sys") {
        // A Vita SFO names a Vita firmware; its category is a Vita one.
        if !(sfo.is_vita() || matches!(value("CATEGORY"), Some("gd" | "gc" | "gp" | "ac"))) || value("TITLE_ID").is_none() {
            return None;
        }
        (System::Vita, "sfo", "sce_sys/param.sfo", format!("{}; content: {}", sfo.summary(), sfo.content_kind().label()))
    } else if parent.eq_ignore_ascii_case("PS3_GAME") {
        let category = value("CATEGORY")?;
        if !matches!(category, "DG" | "HG" | "GD") || value("TITLE_ID").is_none() {
            return None;
        }
        (System::Ps3, "sfo", "PS3_GAME/PARAM.SFO", format!("CATEGORY {category}"))
    } else if parent.eq_ignore_ascii_case("PSP_GAME") {
        if value("DISC_ID").is_none() && value("TITLE_ID").is_none() {
            return None;
        }
        (System::Psp, "sfo", "PSP_GAME/PARAM.SFO", "'\\0PSF' magic".to_string())
    } else {
        return None;
    };
    Some(Found {
        system,
        kind,
        evidence: vec![Evidence::FileInFilesystem { filesystem: "directory", path: tree_path.to_string(), field: Some(field) }],
        title,
        serial,
        serial_priority: 2,
    })
}

