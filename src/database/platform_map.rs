//! Maps the platform names reference sources and providers use to `common::models::Platform`
//! (`platforms.id` -- the app's own identity, never a provider's).
//!
//! DAT sources name their platforms in the `<header><name>` (No-Intro/Redump-style
//! "Vendor - System", RetroAchievements' "RA - System", TOSEC's "Vendor System - Category",
//! MAME softlist short names like `pcecd`). Matching is on a lowercase alphanumeric-only
//! key so punctuation/spacing differences ("PC Engine - TurboGrafx-16" vs
//! "PC Engine CD & TurboGrafx CD") don't need one entry each. Unknown names map to
//! [`Platform::Unknown`] rather than failing: a DAT for a system this app doesn't model is
//! still imported, just unclassified.

use crate::common::models::Platform;

/// Lowercase, alphanumerics only.
pub fn norm_key(name: &str) -> String {
    name.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect()
}

/// Strips the trailing DAT-specific decorations sources add to a platform name:
/// " - Datfile (...)", "(123) (2026-..)", " - BIOS Images", " - BIOS Datfile", RA's
/// "RA - " prefix, No-Intro's "Aftermarket/" prefix, TOSEC's " - Category" suffix.
pub fn strip_dat_decorations(name: &str) -> String {
    let mut s = name.trim().to_string();
    // Lost Level Archive numbers its DATs: "040e - Lost Level Archive - Sega - Dreamcast".
    if let Some((number, rest)) = s.split_once(" - Lost Level Archive - ") {
        if number.len() <= 4 && number.chars().take(3).all(|c| c.is_ascii_digit()) {
            s = rest.to_string();
        }
    }
    // No-Intro's "Unofficial - Sony - PlayStation Portable (PSN) (Decrypted)", "Non-Redump -
    // Sony - PlayStation", Redump's "Handheld - Palm OS": a qualifier, then the system.
    for prefix in ["RA - ", "Aftermarket/", "No-Intro/", "Non-Redump/", "Unofficial/", "Source Code/", "Unofficial - ", "Non-Redump - ", "Handheld - ", "Mobile - "] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.to_string();
        }
    }
    // Drop every trailing "(...)"/"[...]" group (counts, dates, "(Decrypted)", TOSEC's
    // "[BIN]"/"[IMG]" format tags, ...).
    loop {
        let trimmed = s.trim_end().to_string();
        if trimmed.ends_with(')') {
            if let Some(open) = trimmed.rfind('(') {
                s = trimmed[..open].trim_end().to_string();
                continue;
            }
        }
        if trimmed.ends_with(']') {
            if let Some(open) = trimmed.rfind('[') {
                s = trimmed[..open].trim_end().trim_end_matches('-').trim_end().to_string();
                continue;
            }
        }
        s = trimmed;
        break;
    }
    for suffix in [" - Datfile", " - BIOS Images", " - BIOS Datfile", " - BIOS", " - Games", " - Applications", " - Demos", " - Educational", " - Firmware", " - Multimedia", " - Various", " - Compilations", " - Coverdiscs", " - Magazines", " - Samplers", " - Operating Systems", " - Diskmags"] {
        if let Some(rest) = s.strip_suffix(suffix) {
            s = rest.to_string();
        }
    }
    s.trim().to_string()
}

/// The platform a DAT's header name refers to, or `Platform::Unknown`.
pub fn platform_from_dat_name(name: &str) -> Platform {
    let stripped = strip_dat_decorations(name);
    // The key drops punctuation, and "MSX2+" would be MSX2 without its plus.
    if stripped.to_ascii_uppercase().contains("MSX2+") {
        return Platform::Msx2Plus;
    }
    let key = norm_key(&stripped);
    if let Some(p) = lookup(&key) {
        return p;
    }
    // TOSEC-style "Vendor System - Category[ - Subcategory]": try progressively shorter
    // heads, dropping one " - " segment from the right each time.
    let mut head = stripped.as_str();
    while let Some((shorter, _)) = head.rsplit_once(" - ") {
        if let Some(p) = lookup(&norm_key(shorter)) {
            return p;
        }
        head = shorter;
    }
    Platform::Unknown
}

/// What a DAT lists when it is not software: `audio`, `video`, `document`, `image` or
/// `source`, from the DAT's name -- `None` for everything a platform runs (games,
/// applications, BIOS, firmware, demos). Redump's "Audio CD" and "DVD-Video", No-Intro's
/// magazine and manual scans and TOSEC's "Magazines" are all in the catalogue, and none of
/// them is a title to play.
///
/// The spellings are the ones found among the 4,473 platform-less DATs of a full import
/// (redump, No-Intro, TOSEC, MAME-redump; 2026-10). `tosec_style` adds TOSEC's own way of
/// saying it, a " - Category" segment after the system; it is off for the other sources,
/// whose segments are vendors and systems.
///
/// Deliberately left as software: TOSEC "Music", "Graphics" and "Animations" (they are
/// programs and module disks on the machines that have them), "Multimedia", "Hardware", and
/// No-Intro's amiibo, Super Mario Maker courses, title updates and DLC.
pub fn material_from_dat_name(name: &str, tosec_style: bool) -> Option<&'static str> {
    let lower = name.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| lower.contains(n));
    if lower.starts_with("source code - ") || lower.starts_with("source code/") {
        return Some("source");
    }
    // "dvd-video" is "HD DVD-Video" too.
    if has(&["dvd-video", "bd-video", "video cd", "(umd video)", "videonow", "kiosk video"]) {
        return Some("video");
    }
    if has(&["audio cd", "super audio cd", "dvd-audio", "mp3 audio disc", "mp3 cd", "enhanced cd", "video game osts", "(umd music)"]) {
        return Some("audio");
    }
    if has(&["magazine scans", "manual scans", "video game scans", "video game documents", "electronic book"]) {
        return Some("document");
    }
    if has(&["photo cd", "wallpapers"]) {
        return Some("image");
    }
    // No-Intro's "Sony - PlayStation 3 (PSN) (Avatars)" and "... (Themes)".
    if has(&["(avatars)", "(themes)"]) {
        return Some("theme");
    }
    if tosec_style {
        // Every segment after the system, without its "(TOSEC-v...)" / "[ISO]" tail.
        for segment in lower.split(" - ").skip(1) {
            let segment = segment.split(['(', '[']).next().unwrap_or("").trim().trim_end_matches(".dat").trim();
            match segment {
                "magazines" | "manuals" | "books" | "catalogs" | "docs" | "documentation" | "newsletters" | "technical" | "comics" | "artbooks" | "game guides" | "guides" | "posters" | "brochures" | "leaflets"
                | "flyers" => return Some("document"),
                "video" | "tv series" | "commercials" => return Some("video"),
                "artwork" => return Some("image"),
                "sources" | "source" => return Some("source"),
                _ => {}
            }
        }
    }
    None
}

/// `document` for a DAT entry that is plainly reading matter whatever its DAT is called: every
/// file a PDF, a comic archive or an e-book, or the DAT's own category saying so. This is
/// what the DAT's name misses -- TOSEC's "Game Guides", No-Intro's printables and SDK
/// documents, a manual listed among a console's games: 40,928 releases of a full import have
/// nothing but such files. Scans kept as loose images are not told this way (a BIOS pack's
/// PNG icons are not documents); their DATs say what they are.
pub fn document_from_release<'a>(file_names: impl IntoIterator<Item = &'a str>, category: Option<&str>) -> Option<&'static str> {
    if category.is_some_and(|c| DOCUMENT_CATEGORIES.iter().any(|d| c.eq_ignore_ascii_case(d))) {
        return Some("document");
    }
    let mut any = false;
    for name in file_names {
        let lower = name.to_lowercase();
        if !DOCUMENT_EXTENSIONS.iter().any(|ext| lower.ends_with(ext)) {
            return None;
        }
        any = true;
    }
    any.then_some("document")
}

/// File types that are only ever read.
pub const DOCUMENT_EXTENSIONS: [&str; 5] = [".pdf", ".cbz", ".cbr", ".djvu", ".epub"];
/// DAT categories (No-Intro's `<category>`, the last part of a TOSEC DAT's name) that say
/// the same.
pub const DOCUMENT_CATEGORIES: [&str; 9] = ["Manuals", "Magazines", "Game Guides", "Books", "Catalogs", "Docs", "Newsletters", "Comics", "Artbooks"];

/// What a DAT entry is when a tag in its own name says it is not the game: the console
/// store's extras, which No-Intro lists beside the games they came with (the PS3 "Content"
/// DAT above all). `theme` for "(Theme)", "(Dynamic Theme)", "(Static Theme)", "(Avatar)" and
/// "(Avatar Item)" -- 4,400 PS3, 840 Vita and 217 PSP entries on a full import; `audio` for
/// "(Soundtrack)", `video` for "(Video)" and "(E3 Video)", `document` for "(Manual)", and `dlc`
/// for an "(Unlock Key)", which is nothing without its trial.
///
/// Whole tags only: Satellaview's "(SoundLink Magazine)" broadcasts and a "(Video Game)" are
/// software.
pub fn tagged_material(name: &str) -> Option<&'static str> {
    if name.starts_with("[BIOS]") {
        return None;
    }
    let mut rest = name;
    while let Some(open) = rest.find('(') {
        let Some(len) = rest[open..].find(')') else { break };
        let tag = rest[open + 1..open + len].to_lowercase();
        rest = &rest[open + len + 1..];
        match tag.as_str() {
            "theme" | "dynamic theme" | "static theme" | "avatar" | "avatar item" => return Some("theme"),
            "soundtrack" => return Some("audio"),
            "video" | "e3 video" => return Some("video"),
            "manual" => return Some("document"),
            "unlock key" => return Some("dlc"),
            _ => {}
        }
    }
    None
}

/// What an entry is, as the DATs `dat_convert` writes say it in `<category>` -- and, for the
/// first three, as RetroAchievements' own lists do.
///
/// A patched game is still a game: it is in the Titles list with a `game_kind`, under the
/// original it was made from. The rest are not games and carry a `material` instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EntryKind {
    /// `release.game_kind`: `hack` | `mod` | `translation` | `homebrew` | `arcade` | `wiiware`.
    pub game_kind: Option<&'static str>,
    /// `release.patch_type` for the patched ones.
    pub patch_type: Option<&'static str>,
    /// `release.material` for what is not a game.
    pub material: Option<&'static str>,
}

/// `release.game_kind`'s values: the games that are not commercial releases.
pub const GAME_KINDS: [&str; 6] = ["hack", "mod", "translation", "homebrew", "arcade", "wiiware"];

/// The game kinds made from an existing game (`community_patched`).
pub const PATCHED_KINDS: [&str; 3] = ["hack", "mod", "translation"];

/// The kind a `<category>` names. `full` is for the sources whose categories are this
/// vocabulary by construction (the manual ones); for any other DAT only a hack, a
/// translation and a homebrew are taken at their word -- "Arcade" is a genre in TOSEC and
/// "Tool" could be anything.
pub fn kind_from_category(category: Option<&str>, full: bool) -> EntryKind {
    let game = |kind: &'static str, patch: Option<&'static str>| EntryKind { game_kind: Some(kind), patch_type: patch, material: None };
    let material = |m: &'static str| EntryKind { game_kind: None, patch_type: None, material: Some(m) };
    match (category.map(|c| c.trim().to_ascii_lowercase()).as_deref(), full) {
        (Some("hack"), _) => game("hack", Some("gameplay")),
        (Some("translation"), _) => game("translation", Some("translation")),
        (Some("homebrew"), _) => game("homebrew", None),
        (Some("mod"), true) => game("mod", Some("gameplay")),
        (Some("arcade"), true) => game("arcade", None),
        (Some("wiiware"), true) => game("wiiware", None),
        (Some("map pack"), true) => material("mappack"),
        (Some("dlc"), true) => material("dlc"),
        (Some("tool"), true) => material("tool"),
        (Some("trainer"), true) => material("trainer"),
        (Some("save"), true) => material("save"),
        (Some("source"), true) => material("source"),
        (Some("patch"), true) => material("patch"),
        _ => EntryKind::default(),
    }
}

/// `patch` for an entry that is nothing but patch files: something to apply to a ROM, not a
/// ROM. By extension, like [`document_from_release`].
pub fn patch_from_release<'a>(file_names: impl IntoIterator<Item = &'a str>) -> Option<&'static str> {
    let mut any = false;
    for name in file_names {
        let lower = name.to_lowercase();
        if !PATCH_EXTENSIONS.iter().any(|ext| lower.ends_with(ext)) {
            return None;
        }
        any = true;
    }
    any.then_some("patch")
}

pub const PATCH_EXTENSIONS: [&str; 8] = [".ips", ".bps", ".ups", ".xdelta", ".xdelta3", ".ppf", ".aps", ".vcdiff"];

/// The kinds that are things in their own right rather than a game or part of one: disc
/// media, reading matter, source code and a machine's own system files. They have tabs of
/// their own (Collection › Multimedia, Documents, Source code, BIOS / firmware) and no title
/// -- a title is a game -- until one is linked to a game's by hand.
pub const STANDALONE: [&str; 7] = ["audio", "video", "image", "document", "source", "tool", "bios"];

/// `bios` for a machine's own system file rather than a game: a BIOS, firmware, a
/// co-processor (DSP) dump, a system update. Said by the entry's category ("BIOS",
/// "Firmware": retrobios, TOSEC, Redump), No-Intro's "[BIOS]" name, or a DAT that holds
/// nothing else ("... - BIOS Images", "... - BIOS Datfile", "... - Firmware"). Migration
/// 0052 and `release_bases::mark_bios` apply the same rule to rows already here; the three
/// are kept in step by hand.
pub fn bios_from_release(name: &str, category: Option<&str>, header_name: &str) -> Option<&'static str> {
    let said = category.is_some_and(|c| c.eq_ignore_ascii_case("bios") || c.eq_ignore_ascii_case("firmware"));
    let header = header_name.to_ascii_lowercase();
    let bios_dat = header.contains(" - bios") || header.contains("bios images") || header.contains("bios datfile") || header.contains(" - firmware");
    (said || name.starts_with("[BIOS]") || bios_dat).then_some("bios")
}

/// [`STANDALONE`]'s disc media: what Collection › Multimedia lists.
/// What a community makes around a console, listed together in Collection › Etc.
pub const ETC: [&str; 4] = ["mappack", "tool", "trainer", "save"];

pub const MULTIMEDIA: [&str; 3] = ["audio", "video", "image"];

/// The kinds that are a game's own extras: bundled with their game's title by name
/// (`title_merge::bundle_addons`) and listed on its releases' pages.
pub const GAME_EXTRAS: [&str; 7] = ["dlc", "update", "theme", "mappack", "trainer", "save", "patch"];

/// [`GAME_EXTRAS`] as the inside of an SQL `IN (...)`.
pub fn game_extras_sql() -> String {
    GAME_EXTRAS.iter().map(|m| format!("'{m}'")).collect::<Vec<_>>().join(", ")
}

/// `dlc` or `update` for a DAT entry that is an add-on to a game or a newer version of it
/// rather than the game: No-Intro's "(DLC)", "(Addon)", "(Update)", "(Title Update)" and
/// "(Patch)" tags, an "(Expansion ...)" or "(Add-On ...)" disc, and Redump's "Add-Ons" /
/// "Updates" categories. About 80,000 entries of a full import (PS3, Vita, PSP, 3DS, Wii U,
/// Xbox 360 above all), each of which used to be a release like the game itself.
///
/// Not a console's own system software ("[BIOS] PS3 System Software Update ... (Update)"),
/// and not a hack that calls itself an expansion. Migration 0043 applies the same rule to
/// the releases already imported; the two are kept in step by hand.
pub fn addon_from_release(name: &str, category: Option<&str>) -> Option<&'static str> {
    if name.starts_with("[BIOS]") || category.is_some_and(|c| c.eq_ignore_ascii_case("hack")) {
        return None;
    }
    let mut update = false;
    let mut rest = name;
    while let Some(open) = rest.find('(') {
        let Some(len) = rest[open..].find(')') else { break };
        let tag = rest[open + 1..open + len].to_lowercase();
        rest = &rest[open + len + 1..];
        if tag.starts_with("dlc") || tag == "addon" || tag.starts_with("add-on") || tag.starts_with("expansion") {
            return Some("dlc");
        }
        update |= tag == "update" || tag.starts_with("update ") || tag == "title update" || tag == "patch" || tag.starts_with("patch ");
    }
    if update {
        return Some("update");
    }
    match category.map(str::to_lowercase).as_deref() {
        Some("add-ons") => Some("dlc"),
        Some("updates") => Some("update"),
        _ => None,
    }
}

/// Every value `release.material` takes: what [`material_from_dat_name`] gives for a whole
/// DAT, and what [`document_from_release`], [`tagged_material`] and [`addon_from_release`]
/// give for one entry.
pub const MATERIALS: [&str; 14] = ["dlc", "update", "theme", "mappack", "trainer", "save", "patch", "tool", "audio", "video", "document", "image", "source", "bios"];

/// RetroAchievements' console names (`API_GetConsoleIDs`, `ConsoleName` fields): mostly
/// vendor-less ("Game Boy Advance", "SNES/Super Famicom", "PC Engine/TurboGrafx-16") --
/// the table carries those spellings, and a vendor-prefix retry covers the rest.
pub fn platform_from_ra_console_name(name: &str) -> Platform {
    let key = norm_key(name);
    if let Some(p) = lookup(&key) {
        return p;
    }
    for vendor in ["nintendo", "sega", "sony", "nec", "atari", "snk", "microsoft", "bandai", "fujitsu", "sharp"] {
        if let Some(p) = lookup(&format!("{vendor}{key}")) {
            return p;
        }
    }
    platform_from_dat_name(name)
}

fn lookup(key: &str) -> Option<Platform> {
    TABLE.iter().find(|(k, _)| *k == key).map(|(_, p)| *p)
}

macro_rules! table {
    ($($name:literal => $platform:expr),* $(,)?) => {
        &[$(($name, $platform)),*]
    };
}

/// Keys are `norm_key(name)`. Grouped by source convention; duplicates across sources are
/// harmless (first hit wins, they agree).
static TABLE: &[(&str, Platform)] = table! {
    // --- Nintendo (No-Intro / RA / Redump / TOSEC / MAME softlist) ---
    "nintendonintendoentertainmentsystem" => Platform::Nes,
    "nintendoentertainmentsystem" => Platform::Nes,
    "nintendofamilycomputer" => Platform::Famicom,
    "nintendofamicom" => Platform::Famicom,
    "nintendofamilycomputerdisksystem" => Platform::FamicomDiskSystem,
    "nintendofamicomdisksystem" => Platform::FamicomDiskSystem,
    "nintendosupernintendoentertainmentsystem" => Platform::Snes,
    "supernintendoentertainmentsystem" => Platform::Snes,
    "nintendosuperfamicom" => Platform::SuperFamicom,
    "nintendosufamiturbo" => Platform::SufamiTurbo,
    "nintendonintendo64" => Platform::Nintendo64,
    "nintendo64" => Platform::Nintendo64,
    "nintendonintendo64dd" => Platform::Nintendo64Dd,
    "nintendogameboy" => Platform::GameBoy,
    "nintendogameboycolor" => Platform::GameBoyColor,
    "nintendogameboyadvance" => Platform::GameBoyAdvance,
    "nintendogamecube" => Platform::GameCube,
    "nintendonintendogamecube" => Platform::GameCube,
    "nintendowii" => Platform::Wii,
    "nintendowiiu" => Platform::WiiU,
    "nintendonintendods" => Platform::NintendoDs,
    "nintendods" => Platform::NintendoDs,
    "nintendonintendodsi" => Platform::NintendoDsi,
    "nintendodsi" => Platform::NintendoDsi,
    "nintendonintendo3ds" => Platform::Nintendo3ds,
    "nintendo3ds" => Platform::Nintendo3ds,
    "nintendonewnintendo3ds" => Platform::NewNintendo3ds,
    "nintendonintendoswitch" => Platform::NintendoSwitch,
    "nintendoswitch" => Platform::NintendoSwitch,
    "nintendovirtualboy" => Platform::VirtualBoy,
    "nintendopokemonmini" => Platform::PokemonMini,
    // --- Sony ---
    "sonyplaystation" => Platform::PlayStation,
    "sonyplaystation2" => Platform::PlayStation2,
    "sonyplaystation3" => Platform::PlayStation3,
    "sonyplaystation3decrypted" => Platform::PlayStation3,
    "sonyplaystation4" => Platform::PlayStation4,
    "sonyplaystation5" => Platform::PlayStation5,
    "sonyplaystationportable" => Platform::PlayStationPortable,
    "sonyplaystationvita" => Platform::PlayStationVita,
    "sonypocketstation" => Platform::PocketStation,
    "psx" => Platform::PlayStation,
    // --- Sega ---
    "segamegadrivegenesis" => Platform::SegaGenesis,
    "segagenesis" => Platform::SegaGenesis,
    "segamegadrive" => Platform::SegaGenesis,
    "sega32x" => Platform::Sega32x,
    "segamegacdsegacd" => Platform::SegaCd,
    "segacd" => Platform::SegaCd,
    "segasegacd" => Platform::SegaCd,
    "segamegacd" => Platform::SegaCd,
    "megacd" => Platform::SegaCd,
    "segasaturn" => Platform::SegaSaturn,
    "saturn" => Platform::SegaSaturn,
    "segadreamcast" => Platform::Dreamcast,
    "dc" => Platform::Dreamcast,
    "segamastersystemmarkiii" => Platform::SegaMasterSystem,
    "segamastersystem" => Platform::SegaMasterSystem,
    "segagamegear" => Platform::SegaGameGear,
    "segasg1000sc3000" => Platform::Sg1000,
    "segasg1000" => Platform::Sg1000,
    "segapico" => Platform::SegaPico,
    // --- Microsoft ---
    "microsoftxbox" => Platform::Xbox,
    "microsoftxbox360" => Platform::Xbox360,
    "microsoftxboxone" => Platform::XboxOne,
    "microsoftxboxseriesx" => Platform::XboxSeries,
    "microsoftmsx" => Platform::Msx,
    "microsoftmsx2" => Platform::Msx2,
    "microsoftmsxturbor" => Platform::MsxTurbo,
    // --- NEC ---
    "necpcengineturbografx16" => Platform::TurboGrafx16,
    "necturbografx16" => Platform::TurboGrafx16,
    "necpcengine" => Platform::TurboGrafx16,
    "necpcengineturbografx" => Platform::TurboGrafx16,
    "necpcenginecd" => Platform::TurboGrafxCd,
    "necpcenginecdturbografx16cd" => Platform::TurboGrafxCd,
    "necpc6001" => Platform::Pc6000Series,
    "necpc8801" => Platform::Pc8801,
    "necpc9801" => Platform::Pc98,
    "necpcenginecdturbografxcd" => Platform::TurboGrafxCd,
    "necturbografxcd" => Platform::TurboGrafxCd,
    "pcecd" => Platform::TurboGrafxCd,
    "necpcenginesupergrafx" => Platform::SuperGrafx,
    "necpc88series" => Platform::Pc8801,
    "necpc88" => Platform::Pc8801,
    "necpc80008800" => Platform::Pc8801,
    "necpc98series" => Platform::Pc98,
    "necpc98" => Platform::Pc98,
    "pc98cd" => Platform::Pc98,
    "necpcfxpcfxga" => Platform::PcFx,
    "necpcfx" => Platform::PcFx,
    "pcfx" => Platform::PcFx,
    // --- SNK ---
    "snkneogeocd" => Platform::NeoGeoCd,
    "neocd" => Platform::NeoGeoCd,
    "snkneogeopocket" => Platform::NeoGeoPocket,
    "snkneogeopocketcolor" => Platform::NeoGeoPocketColor,
    // --- Atari ---
    "atariatari2600" => Platform::Atari2600,
    "atari2600" => Platform::Atari2600,
    "atariatari5200" => Platform::Atari5200,
    "atariatari7800" => Platform::Atari7800,
    "atari7800" => Platform::Atari7800,
    "atariatarijaguar" => Platform::AtariJaguar,
    "atarijaguar" => Platform::AtariJaguar,
    "atarijaguarcdinteractivemultimediasystem" => Platform::AtariJaguar,
    "atarijaguarcd" => Platform::AtariJaguar,
    "atariatarilynx" => Platform::AtariLynx,
    "atarilynx" => Platform::AtariLynx,
    "atari8bitfamily" => Platform::Atari8Bit,
    "atariatarist" => Platform::AtariStSte,
    "ataristseries" => Platform::AtariStSte,
    // --- Others ---
    "bandaiwonderswan" => Platform::WonderSwan,
    "bandaiwonderswancolor" => Platform::WonderSwanColor,
    "fujitsufmtownsseries" => Platform::FmTowns,
    "fujitsufmtowns" => Platform::FmTowns,
    "fmtownscd" => Platform::FmTowns,
    "fujitsufm7" => Platform::Fm7,
    "sharpx68000" => Platform::SharpX68000,
    "3dointeractivemultiplayer" => Platform::ThreeDo,
    "panasonic3dointeractivemultiplayer" => Platform::ThreeDo,
    "microsoftdos" => Platform::PcDos,
    "ibmpccompatible" => Platform::PcDos,
    "ibmpcandcompatibles" => Platform::PcDos,
    "applemacintosh" => Platform::Mac,
    "maccdrom" => Platform::Mac,
    "arcade" => Platform::Arcade,
    "arcadepcbasedarcade" => Platform::Arcade,
    "arcadepcbased" => Platform::Arcade,
    // --- RetroAchievements console names (vendor-less, slashes dropped by norm_key) ---
    "megadrive" => Platform::SegaGenesis,
    "genesismegadrive" => Platform::SegaGenesis,
    "snessuperfamicom" => Platform::Snes,
    "nesfamicom" => Platform::Nes,
    "pcengineturbografx16" => Platform::TurboGrafx16,
    "pcenginecdturbografxcd" => Platform::TurboGrafxCd,
    "gameboyadvance" => Platform::GameBoyAdvance,
    "gameboy" => Platform::GameBoy,
    "gameboycolor" => Platform::GameBoyColor,
    "32x" => Platform::Sega32x,
    "mastersystem" => Platform::SegaMasterSystem,
    "playstation" => Platform::PlayStation,
    "playstation2" => Platform::PlayStation2,
    "playstationportable" => Platform::PlayStationPortable,
    "neogeopocket" => Platform::NeoGeoPocket,
    "neogeocd" => Platform::NeoGeoCd,
    "gamegear" => Platform::SegaGameGear,
    "gamecube" => Platform::GameCube,
    "nintendods" => Platform::NintendoDs,
    "nintendodsi" => Platform::NintendoDsi,
    "wii" => Platform::Wii,
    "wiiu" => Platform::WiiU,
    "xbox" => Platform::Xbox,
    "pokemonmini" => Platform::PokemonMini,
    "virtualboy" => Platform::VirtualBoy,
    "msx" => Platform::Msx,
    "sg1000" => Platform::Sg1000,
    "atarist" => Platform::AtariStSte,
    "dreamcast" => Platform::Dreamcast,
    "pc80008800" => Platform::Pc8801,
    "pc9800" => Platform::Pc98,
    "atari5200" => Platform::Atari5200,
    "x68k" => Platform::SharpX68000,
    "wonderswan" => Platform::WonderSwan,
    "fmtowns" => Platform::FmTowns,
    "pc6000" => Platform::Pc6000Series,
    "segapico" => Platform::SegaPico,
    "dos" => Platform::PcDos,

    // --- TOSEC's own spellings of systems listed above ---
    "msxmsx" => Platform::Msx,
    "msxmsx2" => Platform::Msx2,
    "msxturbor" => Platform::MsxTurbo,
    "nintendofamicomentertainmentsystem" => Platform::Nes,
    "nintendosuperfamicomsuperentertainmentsystem" => Platform::Snes,
    "nintendo64dd" => Platform::Nintendo64Dd,
    "necpc88va" => Platform::Pc8801,
    "3do3dointeractivemultiplayer" => Platform::ThreeDo,
    "segamarkiiimastersystem" => Platform::SegaMasterSystem,
    "atari2600vcs" => Platform::Atari2600,
    "segagame1000" => Platform::Sg1000,
    "nintendosatellaview" => Platform::SuperFamicom,

    // --- Home computers and consoles (TOSEC "Vendor System", No-Intro "Vendor - System",
    // RetroAchievements' bare names) ---
    "commodorec64" => Platform::Commodore64,
    "commodorec128" => Platform::Commodore64,
    "commodorecommodore64" => Platform::Commodore64,
    "commodore64" => Platform::Commodore64,
    "commodoreamiga" => Platform::Amiga,
    "commodoreamigacd" => Platform::Amiga,
    "amiga" => Platform::Amiga,
    "commodoreamigacd32" => Platform::AmigaCd32,
    "amigacd32" => Platform::AmigaCd32,
    "commodoreamigacdtv" => Platform::CommodoreCdtv,
    "commodorecdtv" => Platform::CommodoreCdtv,
    "commodorevic20" => Platform::Vic20,
    "vic20" => Platform::Vic20,
    "commodorec16c116plus4" => Platform::Commodore16,
    "commodoreplus4" => Platform::Commodore16,
    "commodorepet" => Platform::CommodorePet,
    "sinclairzxspectrum" => Platform::ZxSpectrum,
    "sinclairzxspectrum3" => Platform::ZxSpectrum,
    "zxspectrum" => Platform::ZxSpectrum,
    "sinclairzx81" => Platform::Zx81,
    "appleii" => Platform::AppleII,
    "appleappleii" => Platform::AppleII,
    "appleiigs" => Platform::AppleIIgs,
    "amstradcpc" => Platform::AmstradCpc,
    "acornbbc" => Platform::BbcMicro,
    "acornbbcmicro" => Platform::BbcMicro,
    "acornelectron" => Platform::AcornElectron,
    "acornarchimedes" => Platform::AcornArchimedes,
    "tandyradioshacktrs80modeli" => Platform::Trs80,
    "tandyradioshacktrs80modeliii" => Platform::Trs80,
    "tandyradioshacktrs80model4" => Platform::Trs80,
    "tandyradioshacktrs80colorcomputer" => Platform::Trs80CoCo,
    "texasinstrumentsti994a" => Platform::Ti99,
    "dragondatadragon" => Platform::Dragon32,
    "mattelintellivision" => Platform::Intellivision,
    "intellivision" => Platform::Intellivision,
    "colecocolecovision" => Platform::ColecoVision,
    "colecovision" => Platform::ColecoVision,
    "gcevectrex" => Platform::Vectrex,
    "vectrex" => Platform::Vectrex,
    "magnavoxodyssey2" => Platform::Odyssey2,
    "philipsvideopac" => Platform::Odyssey2,
    "ballyprofessionalarcadeastrocade" => Platform::Astrocade,
    "ballyastrocade" => Platform::Astrocade,
    "philipscdi" => Platform::PhilipsCdi,
    "ouyaouya" => Platform::Ouya,
    "tangerineoric1oricatmos" => Platform::Oric,
    "thomsonmo5" => Platform::ThomsonMoTo,
    "thomsonmo6" => Platform::ThomsonMoTo,
    "thomsonto7" => Platform::ThomsonMoTo,
    "thomsonto770" => Platform::ThomsonMoTo,
    "thomsonto8to8dto9to9" => Platform::ThomsonMoTo,
    "mgtsamcoupe" => Platform::SamCoupe,
    "enterprise64128" => Platform::Enterprise,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tosec_and_no_intro_names_of_home_computers_resolve() {
        let cases = [
            ("Commodore C64 - Games - Arcade - [D64]", Platform::Commodore64),
            ("Commodore - Commodore 64 (Tapes)", Platform::Commodore64),
            ("Commodore Amiga - Games - [ADF]", Platform::Amiga),
            ("Sinclair ZX Spectrum - Games - [TZX]", Platform::ZxSpectrum),
            ("Sinclair - ZX Spectrum +3", Platform::ZxSpectrum),
            ("RA - Apple II", Platform::AppleII),
            ("Apple - II (A2R)", Platform::AppleII),
            ("MSX MSX2 - Games - [DSK]", Platform::Msx2),
            ("MSX MSX2+ - Games", Platform::Msx2Plus),
            ("MSX TurboR - Games", Platform::MsxTurbo),
            ("MSX MSX - Games - [ROM]", Platform::Msx),
            ("Nintendo Famicom & Entertainment System - Games - [NES]", Platform::Nes),
            ("Unofficial - Sony - PlayStation Portable (PSN) (Decrypted)", Platform::PlayStationPortable),
            ("Non-Redump - Sony - PlayStation", Platform::PlayStation),
            ("Mattel - Intellivision", Platform::Intellivision),
            ("Tandy Radio Shack TRS-80 Model III - Games", Platform::Trs80),
            ("Thomson TO8, TO8D, TO9, TO9+ - Games", Platform::ThomsonMoTo),
        ];
        for (name, want) in cases {
            assert_eq!(platform_from_dat_name(name), want, "{name}");
        }
        // Still nothing for what has no platform: a calculator, magazine scans.
        assert_eq!(platform_from_dat_name("Texas Instruments TI-83 - Games"), Platform::Unknown);
    }

    #[test]
    fn non_software_dats_are_told_apart_by_name() {
        let plain = |n: &str| material_from_dat_name(n, false);
        let tosec = |n: &str| material_from_dat_name(n, true);
        for (name, material) in [
            ("Audio CD", "audio"),
            ("Audio CD - Datfile (7545) (2026-09-24 20-22-35).zip", "audio"),
            ("Enhanced CD", "audio"),
            ("MP3 Audio Disc", "audio"),
            ("Non-Redump - Audio CD", "audio"),
            ("Audio CD - Spillover Tracks", "audio"),
            ("Unofficial - Video Game OSTs (Hardware Recordings)", "audio"),
            ("Unofficial - Sony - PlayStation Portable (UMD Music)", "audio"),
            ("DVD-Video", "video"),
            ("HD DVD-Video", "video"),
            ("BD-Video", "video"),
            ("Video CD", "video"),
            ("Hasbro - VideoNow Color", "video"),
            ("Unofficial - Sony - PlayStation Portable (UMD Video)", "video"),
            ("Unofficial - Sony - PlayStation 3 (BD-Video Extras)", "video"),
            ("Nintendo - Kiosk Video Compact Flash (CardImage)", "video"),
            ("Unofficial - Video Game Magazine Scans (PDF)", "document"),
            ("Unofficial - Video Game Manual Scans (JPEG)", "document"),
            ("Unofficial - Video Game Scans (RAW)", "document"),
            ("Unofficial - Video Game Documents (PDF)", "document"),
            ("Non-Redump - Sony Electronic Book", "document"),
            ("Photo CD", "image"),
            ("Nintendo - Wallpapers", "image"),
            ("Source Code - Nintendo - Game Boy Advance", "source"),
        ] {
            assert_eq!(plain(name), Some(material), "{name}");
        }
        for (name, material) in [
            ("Commodore Amiga - Magazines - Amiga Format", "document"),
            ("Commodore Amiga - Magazines - Amiga Format (TOSEC-v2019-05-10_CM).dat", "document"),
            ("Atari ST - Manuals", "document"),
            ("Sinclair ZX Spectrum - Books - [PDF]", "document"),
            ("Commodore C64 - Commercials", "video"),
            ("Atari 8bit - Artwork", "image"),
        ] {
            assert_eq!(tosec(name), Some(material), "{name}");
        }
        // Software, whatever its name suggests.
        for name in ["Sony - PlayStation", "Memorex - Visual Information System", "Philips - CD-i", "Nintendo - amiibo", "Sega - Prologue 21 Multimedia Karaoke System", "Unofficial - Microsoft - Xbox 360 (Title Updates)"] {
            assert_eq!(plain(name), None, "{name}");
        }
        for name in ["Commodore Amiga - Games - [ADF]", "Commodore Amiga - Demos - Music", "Memorex Video Information System - Games", "Commodore Amiga - Magazines"] {
            assert_eq!(tosec(name).is_some(), name.ends_with("Magazines"), "{name}");
        }
        // A vendor or system called like a category is not one for the other sources.
        assert_eq!(plain("Vendor - Books"), None);
    }

    #[test]
    fn reading_matter_is_told_by_its_files_and_category() {
        let files = |names: &[&'static str], category: Option<&str>| document_from_release(names.iter().copied(), category);
        assert_eq!(files(&["Gateway (1992)(Legend Entertainment).pdf"], Some("Game Guides")), Some("document"));
        assert_eq!(files(&["Alone in the Dark (1994)(Interplay)(US).PDF"], Some("Games")), Some("document"), "a manual filed under a console's games");
        assert_eq!(files(&["issue 01.cbz", "issue 02.cbr"], None), Some("document"));
        assert_eq!(files(&["Borderlands (Europe) (Manual).bin"], Some("Manuals")), Some("document"), "the DAT's category is enough");
        // A game that ships its manual, a BIOS pack's icons, and nothing at all.
        assert_eq!(files(&["game.iso", "manual.pdf"], Some("Games")), None);
        assert_eq!(files(&["AppIconLarge.png"], Some("BIOS")), None);
        assert_eq!(files(&[], None), None);
        assert_eq!(material_from_dat_name("Sega Saturn - Game Guides", true), Some("document"));
    }

    #[test]
    fn store_extras_are_told_by_their_tag() {
        for (name, expected) in [
            ("100-Man Ton no Barabara - Ashley (Japan) (Avatar)", Some("theme")),
            ("Avatar Threads - 8 Bit Bling (World) (Avatar Item)", Some("theme")),
            ("40's WWII Pinups (USA) (Theme)", Some("theme")),
            ("Abstract Art (Asia) (Dynamic Theme)", Some("theme")),
            ("Ad Sales - 300 (Europe) (Static Theme)", Some("theme")),
            ("Adam's Venture Chronicles (USA) (Soundtrack)", Some("audio")),
            ("3D Machine Series, The - Episode 1 (Europe) (eShop) (Video)", Some("video")),
            ("Animal Crossing (USA) (E3 Video)", Some("video")),
            ("F-Zero X Weilai Saiche (China) (v2) (Manual)", Some("document")),
            ("3 on 3 NHL Arcade (Europe) (Unlock Key)", Some("dlc")),
            // Games, whatever they are called or sold as.
            ("Theme Park (USA)", None),
            ("Avatar - The Game (Europe)", None),
            ("3 on 3 NHL Arcade (Europe) (Trial)", None),
            ("AK LIVE Hit Gang (Japan) (1995-6-28) (SoundLink Magazine)", None),
            ("[BIOS] Theme Settings (World) (Theme)", None),
        ] {
            assert_eq!(tagged_material(name), expected, "{name}");
        }
        assert_eq!(material_from_dat_name("Sony - PlayStation 3 (PSN) (Avatars)", false), Some("theme"));
        assert_eq!(material_from_dat_name("Sony - PlayStation 3 (PSN) (Themes)", false), Some("theme"));
        assert_eq!(material_from_dat_name("Sony - PlayStation 3 (PSN) (Content)", false), None, "games, demos and extras alike: told entry by entry");
        assert_eq!(game_extras_sql(), "'dlc', 'update', 'theme', 'mappack', 'trainer', 'save', 'patch'");
    }

    #[test]
    fn a_category_names_the_kind() {
        let k = |c: &str, full: bool| kind_from_category(Some(c), full);
        assert_eq!(k("Hack", false), EntryKind { game_kind: Some("hack"), patch_type: Some("gameplay"), material: None });
        assert_eq!(k("Translation", false).patch_type, Some("translation"));
        assert_eq!(k("Translation", false).game_kind, Some("translation"), "a translation is not a hack");
        assert_eq!(k("Mod", true).game_kind, Some("mod"));
        assert_eq!(k("Homebrew", false).game_kind, Some("homebrew"));
        assert_eq!((k("Trainer", true).material, k("Save", true).material, k("Tool", true).material, k("Map Pack", true).material), (Some("trainer"), Some("save"), Some("tool"), Some("mappack")));
        // Words another DAT uses for something else are only believed from the manual ones.
        for c in ["Arcade", "Tool", "Mod", "Trainer", "Source"] {
            assert_eq!(k(c, false), EntryKind::default(), "{c}");
        }
        assert_eq!(k("Games", true), EntryKind::default());
        assert_eq!(patch_from_release(["Game (Japan) [T-En].ips"]), Some("patch"));
        assert_eq!(patch_from_release(["Game.bps", "readme.txt"]), None);
        assert_eq!(patch_from_release(Vec::<&str>::new()), None);
    }

    #[test]
    fn dlc_and_updates_are_told_from_the_game() {
        for (name, category, expected) in [
            ("007 Legends - 007 Pack (USA) (En,Fr) (DLC)", None, Some("dlc")),
            ("Saints Row - The Third - The Full Package (Germany) (En,De) (DLC Installer)", Some("Add-Ons"), Some("dlc")),
            ("0 Day Attack on Earth - London Map Pack (World) (Addon)", None, Some("dlc")),
            ("Borderlands 2 - Game of the Year Edition (USA) (Add-On Disc)", Some("Games"), Some("dlc")),
            ("Age of Mythology - The Titans (Expansion) (USA, Europe)", Some("Add-Ons"), Some("dlc")),
            ("Star Trek - Starship Creator (USA)", Some("Add-Ons"), Some("dlc")),
            ("10 Second Ninja X (Europe) (v1.01) (Update)", None, Some("update")),
            ("0 Day Attack on Earth (World) (v2) (Title Update)", None, Some("update")),
            ("Accel World vs. Sword Art Online (USA) (v2.03) (Patch)", None, Some("update")),
            ("DrumMania 9th Mix (Japan) (Update Disc)", Some("Add-Ons"), Some("update")),
            ("Shovel Knight - Treasure Trove (Europe, Australia) (v4.0A, v5200)", Some("Updates"), Some("update")),
            // The game itself, system software, hacks, and words that only look like tags.
            ("007 Legends (USA) (En,Fr)", Some("Games"), None),
            ("[BIOS] PS3 System Software Update (World) (v4.88) (Update)", None, None),
            ("Culdcept Saga (Japan) (Title Update-ban)", Some("Games"), None),
            ("Bully - Scholarship Edition (USA) (Beta) (2007-11-13) (Patched)", None, None),
            ("Legend of Zelda, The - Link's Shadow (v1.5) (Updated GFX) (The3Dude)", Some("Hack"), None),
            ("Pokemon - FireRed Version (Beta 15) (Expansion Fix) (Aethestode)", Some("Hack"), None),
        ] {
            assert_eq!(addon_from_release(name, category), expected, "{name}");
        }
    }

    #[test]
    fn maps_source_style_names() {
        assert_eq!(platform_from_dat_name("Nintendo - Game Boy Advance"), Platform::GameBoyAdvance);
        assert_eq!(platform_from_dat_name("RA - Sega Genesis"), Platform::SegaGenesis);
        assert_eq!(platform_from_dat_name("Sony - PlayStation - Datfile (serial,version) (10896) (2026-09-15 14-42-35)"), Platform::PlayStation);
        assert_eq!(platform_from_dat_name("Sony - PlayStation - BIOS Images (24) (2026-06-16)"), Platform::PlayStation);
        assert_eq!(platform_from_dat_name("NEC - PC Engine CD & TurboGrafx CD (551)"), Platform::TurboGrafxCd);
        assert_eq!(platform_from_dat_name("Nintendo Game Boy Advance - Applications"), Platform::GameBoyAdvance);
        assert_eq!(platform_from_dat_name("Aftermarket/Sega - Mega Drive - Genesis"), Platform::SegaGenesis);
        assert_eq!(platform_from_dat_name("pcecd (483)"), Platform::TurboGrafxCd);
        assert_eq!(platform_from_dat_name("Sony - PlayStation 3 (Decrypted) (298)"), Platform::PlayStation3);
        assert_eq!(platform_from_dat_name("Sega Mega Drive & Genesis - Games - [BIN]"), Platform::SegaGenesis);
        assert_eq!(platform_from_dat_name("Sega Mega Drive & Genesis - Games - [Multipart]"), Platform::SegaGenesis);
        assert_eq!(platform_from_dat_name("Hasbro - VideoNow"), Platform::Unknown);
    }

    #[test]
    fn maps_ra_console_names() {
        assert_eq!(platform_from_ra_console_name("Game Boy Advance"), Platform::GameBoyAdvance);
        assert_eq!(platform_from_ra_console_name("SNES/Super Famicom"), Platform::Snes);
        assert_eq!(platform_from_ra_console_name("PC Engine/TurboGrafx-16"), Platform::TurboGrafx16);
        assert_eq!(platform_from_ra_console_name("Mega Drive"), Platform::SegaGenesis);
        assert_eq!(platform_from_ra_console_name("Virtual Boy"), Platform::VirtualBoy);
        assert_eq!(platform_from_ra_console_name("Saturn"), Platform::SegaSaturn);
        assert_eq!(platform_from_ra_console_name("Vectrex"), Platform::Vectrex);
        assert_eq!(platform_from_ra_console_name("Arduboy"), Platform::Unknown);
    }

    #[test]
    fn strips_decorations() {
        assert_eq!(strip_dat_decorations("Sega - Saturn - Datfile (2446) (2026-09-15 14-42-35)"), "Sega - Saturn");
        assert_eq!(strip_dat_decorations("RA - Nintendo 64"), "Nintendo 64");
    }
}
