//! Platform names for the filesystem: what a destination subdirectory is called under each
//! of the four built-in schemes (`api::naming`, Config > Copy).
//!
//! Four exhaustive matches, **none of them with a `_` arm, on purpose**. Adding a
//! [`Platform`] variant is meant to be a hard compile error here until someone picks its
//! names -- that coupling is the whole reason these tables live next to the enum instead of
//! in the database. Do not "fix" a build break by adding a wildcard.
//!
//! - [`Platform::manufacturer`] / [`Platform::dat_folder_name`] -- the No-Intro / Redump /
//!   TOSEC spelling, `"Nintendo - Super Nintendo Entertainment System"`. Cross-checked
//!   against [`crate::database::platform_map`], which reads the same convention in the other
//!   direction: a test asserts no name here ever resolves to a *different* platform. Not
//!   every variant has a published pack (Neo Geo AES, DSiWare, Switch 2), so that map
//!   answering `None` for one of those is expected, not a failure.
//! - [`Platform::romm_slug_seed`] -- the IGDB platform slug RomM matches its library folders
//!   against. **A seed, not the authority:** `platforms.romm_slug` holds the live value and
//!   [`crate::database::icon_import`] refreshes it from IGDB, which may rename a slug this
//!   table cannot know about. Several of our platforms share one slug (MSX2 / MSX2+ /
//!   turbo R), which is why nothing here asserts uniqueness. `None` where IGDB has no
//!   equivalent at all.
//! - [`Platform::es_folder`] -- the EmulationStation / RetroArch short folder name. Unique
//!   across platforms (asserted), because it is a directory name and two platforms sharing
//!   one silently merges their games.
//!
//! `Display for Platform` is deliberately used by none of this: it mixes JP-canonical names
//! with lowercase slugs (`"Family Computer"`, `"MSX2+"`, `"unknown"`) and collapses several
//! variants onto one string.

use super::models::Platform;

/// A pair of platforms romexplorer tells apart but most folder conventions do not: the
/// Famicom and the NES, the DSi and its downloadable titles, MSX2 and its later revisions.
///
/// Splitting them is right in the catalogue -- they are different hardware, different
/// dumps, different DATs -- and usually wrong on disk, where an emulator frontend expects
/// one directory. So the copy layout folds a child into its parent's folder when the
/// group is switched on (`copy_platform_combine`, Config > Copy). `DEFAULT_ON` is whether
/// a fresh install folds it: off wherever EmulationStation or RomM genuinely ships a
/// separate folder for the child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CombineGroup {
    /// Stable key, as the `copy_platform_combine` setting stores it.
    pub key: &'static str,
    pub parent: Platform,
    pub children: &'static [Platform],
    pub default_on: bool,
}

/// Every combine group, in the order the config tab lists them.
pub const COMBINE_GROUPS: &[CombineGroup] = &[
    CombineGroup { key: "famicom", parent: Platform::Nes, children: &[Platform::Famicom], default_on: true },
    CombineGroup { key: "super_famicom", parent: Platform::Snes, children: &[Platform::SuperFamicom], default_on: true },
    CombineGroup { key: "fds", parent: Platform::Nes, children: &[Platform::FamicomDiskSystem], default_on: false },
    CombineGroup { key: "sufami_turbo", parent: Platform::Snes, children: &[Platform::SufamiTurbo], default_on: false },
    CombineGroup { key: "n64dd", parent: Platform::Nintendo64, children: &[Platform::Nintendo64Dd], default_on: false },
    CombineGroup { key: "dsiware", parent: Platform::NintendoDsi, children: &[Platform::DsiWare], default_on: true },
    CombineGroup { key: "new_3ds", parent: Platform::Nintendo3ds, children: &[Platform::NewNintendo3ds], default_on: true },
    CombineGroup { key: "switch2", parent: Platform::NintendoSwitch, children: &[Platform::NintendoSwitch2], default_on: true },
    CombineGroup { key: "psp_ps1", parent: Platform::PlayStationPortable, children: &[Platform::PspPs1Classic], default_on: true },
    CombineGroup { key: "pocketstation", parent: Platform::PlayStation, children: &[Platform::PocketStation], default_on: false },
    CombineGroup { key: "msx", parent: Platform::Msx2, children: &[Platform::Msx2Plus, Platform::MsxTurbo], default_on: true },
    CombineGroup { key: "wonderswan", parent: Platform::WonderSwan, children: &[Platform::WonderSwanColor], default_on: false },
    CombineGroup { key: "pico", parent: Platform::SegaGenesis, children: &[Platform::SegaPico], default_on: false },
    CombineGroup { key: "neogeo", parent: Platform::NeoGeoMvs, children: &[Platform::NeoGeoAes], default_on: true },
];

/// The group `platform` is the child of, if any.
pub fn combine_group_of(platform: Platform) -> Option<&'static CombineGroup> {
    COMBINE_GROUPS.iter().find(|g| g.children.contains(&platform))
}

/// The platform whose folder `platform` uses, given which groups are on. One level only --
/// no group's parent is another group's child, asserted in the tests.
pub fn combined_platform(platform: Platform, is_on: &dyn Fn(&str) -> bool) -> Platform {
    match combine_group_of(platform) {
        Some(g) if is_on(g.key) => g.parent,
        _ => platform,
    }
}

/// Whether two platforms are the same machine as far as a folder layout is concerned --
/// either equal, or one is the other's combine child. Used where a name that resolves to a
/// sibling is correct rather than a collision.
pub fn same_machine(a: Platform, b: Platform) -> bool {
    a == b || combine_group_of(a).is_some_and(|g| g.parent == b) || combine_group_of(b).is_some_and(|g| g.parent == a)
}

impl Platform {
    /// The vendor a pack publisher prefixes the system name with, or `None` for the systems
    /// they name without one (Arcade, 3DO, Linux).
    pub fn manufacturer(self) -> Option<&'static str> {
        match self {
            Self::Unknown => None,
            Self::Nes => Some("Nintendo"),
            Self::Famicom => Some("Nintendo"),
            Self::FamicomDiskSystem => Some("Nintendo"),
            Self::Snes => Some("Nintendo"),
            Self::SuperFamicom => Some("Nintendo"),
            Self::SufamiTurbo => Some("Nintendo"),
            Self::Nintendo64 => Some("Nintendo"),
            Self::Nintendo64Dd => Some("Nintendo"),
            Self::GameBoy => Some("Nintendo"),
            Self::GameBoyColor => Some("Nintendo"),
            Self::GameBoyAdvance => Some("Nintendo"),
            Self::VirtualBoy => Some("Nintendo"),
            Self::GameCube => Some("Nintendo"),
            Self::Wii => Some("Nintendo"),
            Self::WiiU => Some("Nintendo"),
            Self::NintendoDs => Some("Nintendo"),
            Self::NintendoDsi => Some("Nintendo"),
            Self::DsiWare => Some("Nintendo"),
            Self::Nintendo3ds => Some("Nintendo"),
            Self::NewNintendo3ds => Some("Nintendo"),
            Self::NintendoSwitch => Some("Nintendo"),
            Self::NintendoSwitch2 => Some("Nintendo"),
            Self::PokemonMini => Some("Nintendo"),
            Self::PlayStation => Some("Sony"),
            Self::PlayStation2 => Some("Sony"),
            Self::PlayStation3 => Some("Sony"),
            Self::PlayStation4 => Some("Sony"),
            Self::PlayStation5 => Some("Sony"),
            Self::PlayStationPortable => Some("Sony"),
            Self::PspPs1Classic => Some("Sony"),
            Self::PlayStationVita => Some("Sony"),
            Self::PlayStationVr => Some("Sony"),
            Self::PocketStation => Some("Sony"),
            Self::SegaGenesis => Some("Sega"),
            Self::Sega32x => Some("Sega"),
            Self::SegaCd => Some("Sega"),
            Self::SegaSaturn => Some("Sega"),
            Self::Dreamcast => Some("Sega"),
            Self::SegaMasterSystem => Some("Sega"),
            Self::SegaGameGear => Some("Sega"),
            Self::Sg1000 => Some("Sega"),
            Self::SegaPico => Some("Sega"),
            Self::Xbox => Some("Microsoft"),
            Self::Xbox360 => Some("Microsoft"),
            Self::XboxOne => Some("Microsoft"),
            Self::XboxSeries => Some("Microsoft"),
            Self::PcWindows => Some("Microsoft"),
            Self::Msx => Some("Microsoft"),
            Self::Msx2 => Some("Microsoft"),
            Self::Msx2Plus => Some("Microsoft"),
            Self::MsxTurbo => Some("Microsoft"),
            Self::PcDos => Some("IBM"),
            Self::Mac => Some("Apple"),
            Self::Linux => None,
            Self::GoogleStadia => Some("Google"),
            Self::TurboGrafx16 => Some("NEC"),
            Self::TurboGrafxCd => Some("NEC"),
            Self::SuperGrafx => Some("NEC"),
            Self::PcFx => Some("NEC"),
            Self::Pc8801 => Some("NEC"),
            Self::Pc98 => Some("NEC"),
            Self::Pc6000Series => Some("NEC"),
            Self::NeoGeoMvs => Some("SNK"),
            Self::NeoGeoAes => Some("SNK"),
            Self::NeoGeoCd => Some("SNK"),
            Self::NeoGeoPocket => Some("SNK"),
            Self::NeoGeoPocketColor => Some("SNK"),
            Self::Atari2600 => Some("Atari"),
            Self::Atari5200 => Some("Atari"),
            Self::Atari7800 => Some("Atari"),
            Self::Atari8Bit => Some("Atari"),
            Self::AtariLynx => Some("Atari"),
            Self::AtariJaguar => Some("Atari"),
            Self::AtariStSte => Some("Atari"),
            Self::WonderSwan => Some("Bandai"),
            Self::WonderSwanColor => Some("Bandai"),
            Self::FmTowns => Some("Fujitsu"),
            Self::Fm7 => Some("Fujitsu"),
            Self::SharpX68000 => Some("Sharp"),
            Self::Commodore64 => Some("Commodore"),
            Self::Amiga => Some("Commodore"),
            Self::AmstradCpc => Some("Amstrad"),
            Self::ZxSpectrum => Some("Sinclair"),
            Self::Intellivision => Some("Mattel"),
            Self::ColecoVision => Some("Coleco"),
            Self::BbcMicro => Some("Acorn"),
            Self::Vectrex => Some("GCE"),
            Self::Vic20 => Some("Commodore"),
            Self::Ouya => Some("Ouya"),
            Self::Android => Some("Google"),
            Self::Ios => Some("Apple"),
            Self::AppleII => Some("Apple"),
            Self::CommodorePet => Some("Commodore"),
            Self::Astrocade => Some("Bally"),
            Self::Commodore16 => Some("Commodore"),
            Self::AmigaCd32 => Some("Commodore"),
            Self::AppleIIgs => Some("Apple"),
            Self::AcornArchimedes => Some("Acorn"),
            Self::PhilipsCdi => Some("Philips"),
            Self::Trs80 => Some("Tandy"),
            Self::Ti99 => Some("Texas Instruments"),
            Self::Odyssey2 => Some("Magnavox"),
            Self::AcornElectron => Some("Acorn"),
            Self::Trs80CoCo => Some("Tandy"),
            Self::Dragon32 => Some("Dragon Data"),
            Self::CommodoreCdtv => Some("Commodore"),
            Self::Zx81 => Some("Sinclair"),
            Self::Oric => Some("Tangerine"),
            Self::ThomsonMoTo => Some("Thomson"),
            Self::SamCoupe => Some("MGT"),
            Self::Enterprise => Some("Enterprise"),
            Self::ThreeDo => None,
            Self::Arcade => None,
        }
    }

    /// The system name as a pack publisher spells it, without the vendor prefix.
    pub fn dat_system_name(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Nes => "Nintendo Entertainment System",
            Self::Famicom => "Family Computer",
            Self::FamicomDiskSystem => "Family Computer Disk System",
            Self::Snes => "Super Nintendo Entertainment System",
            Self::SuperFamicom => "Super Famicom",
            Self::SufamiTurbo => "Sufami Turbo",
            Self::Nintendo64 => "Nintendo 64",
            Self::Nintendo64Dd => "Nintendo 64DD",
            Self::GameBoy => "Game Boy",
            Self::GameBoyColor => "Game Boy Color",
            Self::GameBoyAdvance => "Game Boy Advance",
            Self::VirtualBoy => "Virtual Boy",
            Self::GameCube => "GameCube",
            Self::Wii => "Wii",
            Self::WiiU => "Wii U",
            Self::NintendoDs => "Nintendo DS",
            Self::NintendoDsi => "Nintendo DSi",
            Self::DsiWare => "Nintendo DSiWare",
            Self::Nintendo3ds => "Nintendo 3DS",
            Self::NewNintendo3ds => "New Nintendo 3DS",
            Self::NintendoSwitch => "Nintendo Switch",
            Self::NintendoSwitch2 => "Nintendo Switch 2",
            Self::PokemonMini => "Pokemon Mini",
            Self::PlayStation => "PlayStation",
            Self::PlayStation2 => "PlayStation 2",
            Self::PlayStation3 => "PlayStation 3",
            Self::PlayStation4 => "PlayStation 4",
            Self::PlayStation5 => "PlayStation 5",
            Self::PlayStationPortable => "PlayStation Portable",
            Self::PspPs1Classic => "PlayStation Portable - PS1 Classics",
            Self::PlayStationVita => "PlayStation Vita",
            Self::PlayStationVr => "PlayStation VR",
            Self::PocketStation => "PocketStation",
            Self::SegaGenesis => "Mega Drive - Genesis",
            Self::Sega32x => "32X",
            Self::SegaCd => "Mega CD - Sega CD",
            Self::SegaSaturn => "Saturn",
            Self::Dreamcast => "Dreamcast",
            Self::SegaMasterSystem => "Master System - Mark III",
            Self::SegaGameGear => "Game Gear",
            Self::Sg1000 => "SG-1000",
            Self::SegaPico => "Pico",
            Self::Xbox => "Xbox",
            Self::Xbox360 => "Xbox 360",
            Self::XboxOne => "Xbox One",
            Self::XboxSeries => "Xbox Series X",
            Self::PcWindows => "Windows",
            Self::Msx => "MSX",
            Self::Msx2 => "MSX2",
            Self::Msx2Plus => "MSX2 Plus",
            Self::MsxTurbo => "MSX turbo R",
            Self::PcDos => "PC compatible",
            Self::Mac => "Macintosh",
            Self::Linux => "Linux",
            Self::GoogleStadia => "Stadia",
            Self::TurboGrafx16 => "PC Engine - TurboGrafx-16",
            Self::TurboGrafxCd => "PC Engine CD - TurboGrafx-CD",
            Self::SuperGrafx => "PC Engine SuperGrafx",
            Self::PcFx => "PC-FX - PC-FXGA",
            Self::Pc8801 => "PC-88 series",
            Self::Pc98 => "PC-98 series",
            Self::Pc6000Series => "PC-6000 series",
            Self::NeoGeoMvs => "Neo Geo MVS",
            Self::NeoGeoAes => "Neo Geo AES",
            Self::NeoGeoCd => "Neo Geo CD",
            Self::NeoGeoPocket => "Neo Geo Pocket",
            Self::NeoGeoPocketColor => "Neo Geo Pocket Color",
            Self::Atari2600 => "Atari 2600",
            Self::Atari5200 => "Atari 5200",
            Self::Atari7800 => "Atari 7800",
            Self::Atari8Bit => "8-bit Family",
            Self::AtariLynx => "Atari Lynx",
            Self::AtariJaguar => "Atari Jaguar",
            Self::AtariStSte => "Atari ST",
            Self::WonderSwan => "WonderSwan",
            Self::WonderSwanColor => "WonderSwan Color",
            Self::FmTowns => "FM Towns",
            Self::Fm7 => "FM-7",
            Self::SharpX68000 => "X68000",
            Self::Commodore64 => "Commodore 64",
            Self::Amiga => "Amiga",
            Self::AmstradCpc => "CPC",
            Self::ZxSpectrum => "ZX Spectrum",
            Self::Intellivision => "Intellivision",
            Self::ColecoVision => "ColecoVision",
            Self::BbcMicro => "BBC Micro",
            Self::Vectrex => "Vectrex",
            Self::Vic20 => "VIC-20",
            Self::Ouya => "Ouya",
            Self::Android => "Android",
            Self::Ios => "iOS",
            Self::AppleII => "II",
            Self::CommodorePet => "PET",
            Self::Astrocade => "Astrocade",
            Self::Commodore16 => "Plus-4",
            Self::AmigaCd32 => "Amiga CD32",
            Self::AppleIIgs => "IIGS",
            Self::AcornArchimedes => "Archimedes",
            Self::PhilipsCdi => "CD-i",
            Self::Trs80 => "TRS-80",
            Self::Ti99 => "TI-99 4A",
            Self::Odyssey2 => "Odyssey 2",
            Self::AcornElectron => "Electron",
            Self::Trs80CoCo => "TRS-80 Color Computer",
            Self::Dragon32 => "Dragon",
            Self::CommodoreCdtv => "CDTV",
            Self::Zx81 => "ZX81",
            Self::Oric => "Oric",
            Self::ThomsonMoTo => "MO-TO",
            Self::SamCoupe => "SAM Coupe",
            Self::Enterprise => "Enterprise",
            Self::ThreeDo => "3DO Interactive Multiplayer",
            Self::Arcade => "Arcade",
        }
    }

    /// `"Nintendo - Super Nintendo Entertainment System"`: the folder name No-Intro, Redump
    /// and TOSEC packs use, and the one RetroArch's own database is keyed by.
    pub fn dat_folder_name(self) -> String {
        match self.manufacturer() {
            Some(vendor) => format!("{vendor} - {}", self.dat_system_name()),
            None => self.dat_system_name().to_string(),
        }
    }

    /// Seed value for `platforms.romm_slug` -- see the module doc: the live value is the
    /// column, which `import-icons` refreshes from IGDB.
    pub fn romm_slug_seed(self) -> Option<&'static str> {
        match self {
            Self::Unknown => None,
            Self::Nes => Some("nes"),
            Self::Famicom => Some("famicom"),
            Self::FamicomDiskSystem => Some("fds"),
            Self::Snes => Some("snes"),
            Self::SuperFamicom => Some("sfam"),
            Self::SufamiTurbo => Some("sufami-turbo"),
            Self::Nintendo64 => Some("n64"),
            Self::Nintendo64Dd => Some("64dd"),
            Self::GameBoy => Some("gb"),
            Self::GameBoyColor => Some("gbc"),
            Self::GameBoyAdvance => Some("gba"),
            Self::VirtualBoy => Some("virtualboy"),
            Self::GameCube => Some("ngc"),
            Self::Wii => Some("wii"),
            Self::WiiU => Some("wiiu"),
            Self::NintendoDs => Some("nds"),
            Self::NintendoDsi => Some("nintendo-dsi"),
            Self::DsiWare => Some("nintendo-dsi"),
            Self::Nintendo3ds => Some("3ds"),
            Self::NewNintendo3ds => Some("new-nintendo-3ds"),
            Self::NintendoSwitch => Some("switch"),
            Self::NintendoSwitch2 => Some("switch-2"),
            Self::PokemonMini => Some("pokemon-mini"),
            Self::PlayStation => Some("psx"),
            Self::PlayStation2 => Some("ps2"),
            Self::PlayStation3 => Some("ps3"),
            Self::PlayStation4 => Some("ps4"),
            Self::PlayStation5 => Some("ps5"),
            Self::PlayStationPortable => Some("psp"),
            Self::PspPs1Classic => Some("psp"),
            Self::PlayStationVita => Some("psvita"),
            Self::PlayStationVr => Some("psvr"),
            Self::PocketStation => Some("pocketstation"),
            Self::SegaGenesis => Some("genesis"),
            Self::Sega32x => Some("sega32"),
            Self::SegaCd => Some("segacd"),
            Self::SegaSaturn => Some("saturn"),
            Self::Dreamcast => Some("dc"),
            Self::SegaMasterSystem => Some("sms"),
            Self::SegaGameGear => Some("gamegear"),
            Self::Sg1000 => Some("sg1000"),
            Self::SegaPico => Some("pico"),
            Self::Xbox => Some("xbox"),
            Self::Xbox360 => Some("xbox360"),
            Self::XboxOne => Some("xboxone"),
            Self::XboxSeries => Some("series-x-s"),
            Self::PcWindows => Some("win"),
            Self::Msx => Some("msx"),
            Self::Msx2 => Some("msx2"),
            Self::Msx2Plus => Some("msx2"),
            Self::MsxTurbo => Some("msx2"),
            Self::PcDos => Some("dos"),
            Self::Mac => Some("mac"),
            Self::Linux => Some("linux"),
            Self::GoogleStadia => Some("stadia"),
            Self::TurboGrafx16 => Some("tg16"),
            Self::TurboGrafxCd => Some("turbografx-cd"),
            Self::SuperGrafx => Some("supergrafx"),
            Self::PcFx => Some("pc-fx"),
            Self::Pc8801 => Some("pc-8800-series"),
            Self::Pc98 => Some("pc-98"),
            Self::Pc6000Series => Some("nec-pc-6000-series"),
            Self::NeoGeoMvs => Some("neogeomvs"),
            Self::NeoGeoAes => Some("neogeoaes"),
            Self::NeoGeoCd => Some("neo-geo-cd"),
            Self::NeoGeoPocket => Some("neo-geo-pocket"),
            Self::NeoGeoPocketColor => Some("neo-geo-pocket-color"),
            Self::Atari2600 => Some("atari2600"),
            Self::Atari5200 => Some("atari5200"),
            Self::Atari7800 => Some("atari7800"),
            Self::Atari8Bit => Some("atari8bit"),
            Self::AtariLynx => Some("lynx"),
            Self::AtariJaguar => Some("jaguar"),
            Self::AtariStSte => Some("atari-st"),
            Self::WonderSwan => Some("wonderswan"),
            Self::WonderSwanColor => Some("wonderswan-color"),
            Self::FmTowns => Some("fm-towns"),
            Self::Fm7 => Some("fm-7"),
            Self::SharpX68000 => Some("sharp-x68000"),
            Self::Commodore64 => Some("c64"),
            Self::Amiga => Some("amiga"),
            Self::AmstradCpc => Some("acpc"),
            Self::ZxSpectrum => Some("zxs"),
            Self::Intellivision => Some("intellivision"),
            Self::ColecoVision => Some("colecovision"),
            Self::BbcMicro => Some("bbcmicro"),
            Self::Vectrex => Some("vectrex"),
            Self::Vic20 => Some("vic-20"),
            Self::Ouya => Some("ouya"),
            Self::Android => Some("android"),
            Self::Ios => Some("ios"),
            Self::AppleII => Some("appleii"),
            Self::CommodorePet => Some("cpet"),
            Self::Astrocade => Some("astrocade"),
            Self::Commodore16 => Some("c16"),
            Self::AmigaCd32 => Some("amiga-cd32"),
            Self::AppleIIgs => Some("apple-iigs"),
            Self::AcornArchimedes => Some("acorn-archimedes"),
            Self::PhilipsCdi => Some("philips-cd-i"),
            Self::Trs80 => Some("trs-80"),
            Self::Ti99 => Some("ti-99"),
            Self::Odyssey2 => Some("odyssey-2-slash-videopac-g7000"),
            Self::AcornElectron => Some("acorn-electron"),
            Self::Trs80CoCo => Some("trs-80-color-computer"),
            Self::Dragon32 => Some("dragon-32-slash-64"),
            Self::CommodoreCdtv => Some("commodore-cdtv"),
            Self::Zx81 => None,
            Self::Oric => None,
            Self::ThomsonMoTo => None,
            Self::SamCoupe => None,
            Self::Enterprise => None,
            Self::ThreeDo => Some("3do"),
            Self::Arcade => Some("arcade"),
        }
    }

    /// The EmulationStation / RetroArch short folder name (`snes`, `megadrive`, `psx`).
    pub fn es_folder(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Nes => "nes",
            Self::Famicom => "famicom",
            Self::FamicomDiskSystem => "fds",
            Self::Snes => "snes",
            Self::SuperFamicom => "sfc",
            Self::SufamiTurbo => "sufami",
            Self::Nintendo64 => "n64",
            Self::Nintendo64Dd => "n64dd",
            Self::GameBoy => "gb",
            Self::GameBoyColor => "gbc",
            Self::GameBoyAdvance => "gba",
            Self::VirtualBoy => "virtualboy",
            Self::GameCube => "gc",
            Self::Wii => "wii",
            Self::WiiU => "wiiu",
            Self::NintendoDs => "nds",
            Self::NintendoDsi => "dsi",
            Self::DsiWare => "dsiware",
            Self::Nintendo3ds => "3ds",
            Self::NewNintendo3ds => "n3ds",
            Self::NintendoSwitch => "switch",
            Self::NintendoSwitch2 => "switch2",
            Self::PokemonMini => "pokemini",
            Self::PlayStation => "psx",
            Self::PlayStation2 => "ps2",
            Self::PlayStation3 => "ps3",
            Self::PlayStation4 => "ps4",
            Self::PlayStation5 => "ps5",
            Self::PlayStationPortable => "psp",
            Self::PspPs1Classic => "psp-ps1",
            Self::PlayStationVita => "psvita",
            Self::PlayStationVr => "psvr",
            Self::PocketStation => "pocketstation",
            Self::SegaGenesis => "megadrive",
            Self::Sega32x => "sega32x",
            Self::SegaCd => "segacd",
            Self::SegaSaturn => "saturn",
            Self::Dreamcast => "dreamcast",
            Self::SegaMasterSystem => "mastersystem",
            Self::SegaGameGear => "gamegear",
            Self::Sg1000 => "sg-1000",
            Self::SegaPico => "pico",
            Self::Xbox => "xbox",
            Self::Xbox360 => "xbox360",
            Self::XboxOne => "xboxone",
            Self::XboxSeries => "xboxseries",
            Self::PcWindows => "windows",
            Self::Msx => "msx",
            Self::Msx2 => "msx2",
            Self::Msx2Plus => "msx2plus",
            Self::MsxTurbo => "msxturbor",
            Self::PcDos => "pc",
            Self::Mac => "macintosh",
            Self::Linux => "linux",
            Self::GoogleStadia => "stadia",
            Self::TurboGrafx16 => "pcengine",
            Self::TurboGrafxCd => "pcenginecd",
            Self::SuperGrafx => "supergrafx",
            Self::PcFx => "pcfx",
            Self::Pc8801 => "pc88",
            Self::Pc98 => "pc98",
            Self::Pc6000Series => "pc6000",
            Self::NeoGeoMvs => "neogeo",
            Self::NeoGeoAes => "neogeoaes",
            Self::NeoGeoCd => "neogeocd",
            Self::NeoGeoPocket => "ngp",
            Self::NeoGeoPocketColor => "ngpc",
            Self::Atari2600 => "atari2600",
            Self::Atari5200 => "atari5200",
            Self::Atari7800 => "atari7800",
            Self::Atari8Bit => "atari800",
            Self::AtariLynx => "lynx",
            Self::AtariJaguar => "atarijaguar",
            Self::AtariStSte => "atarist",
            Self::WonderSwan => "wonderswan",
            Self::WonderSwanColor => "wonderswancolor",
            Self::FmTowns => "fmtowns",
            Self::Fm7 => "fm7",
            Self::SharpX68000 => "x68000",
            Self::Commodore64 => "c64",
            Self::Amiga => "amiga",
            Self::AmstradCpc => "amstradcpc",
            Self::ZxSpectrum => "zxspectrum",
            Self::Intellivision => "intellivision",
            Self::ColecoVision => "colecovision",
            Self::BbcMicro => "bbcmicro",
            Self::Vectrex => "vectrex",
            Self::Vic20 => "vic20",
            Self::Ouya => "ouya",
            Self::Android => "android",
            Self::Ios => "ios",
            Self::AppleII => "apple2",
            Self::CommodorePet => "pet",
            Self::Astrocade => "astrocade",
            Self::Commodore16 => "plus4",
            Self::AmigaCd32 => "amigacd32",
            Self::AppleIIgs => "apple2gs",
            Self::AcornArchimedes => "archimedes",
            Self::PhilipsCdi => "cdimono1",
            Self::Trs80 => "trs-80",
            Self::Ti99 => "ti99",
            Self::Odyssey2 => "odyssey2",
            Self::AcornElectron => "electron",
            Self::Trs80CoCo => "coco",
            Self::Dragon32 => "dragon32",
            Self::CommodoreCdtv => "cdtv",
            Self::Zx81 => "zx81",
            Self::Oric => "oric",
            Self::ThomsonMoTo => "moto",
            Self::SamCoupe => "samcoupe",
            Self::Enterprise => "enterprise",
            Self::ThreeDo => "3do",
            Self::Arcade => "arcade",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::connection::all_platforms;
    use crate::database::platform_map::platform_from_dat_name;

    #[test]
    fn every_platform_has_filesystem_safe_names() {
        fn safe(s: &str) -> bool {
            s.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit()) && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        }
        for p in all_platforms() {
            assert!(safe(p.es_folder()), "{p:?}: ES folder {:?} is not [a-z0-9][a-z0-9-]*", p.es_folder());
            if let Some(slug) = p.romm_slug_seed() {
                assert!(safe(slug), "{p:?}: RomM slug {slug:?} is not [a-z0-9][a-z0-9-]*");
            }
            assert!(!p.dat_folder_name().is_empty(), "{p:?} has no pack folder name");
        }
    }

    /// Two platforms sharing an ES folder would silently merge their games on copy.
    #[test]
    fn es_folders_are_unique() {
        let mut seen = std::collections::HashMap::new();
        for p in all_platforms() {
            if let Some(other) = seen.insert(p.es_folder(), p) {
                panic!("{p:?} and {other:?} both use the ES folder {:?}", p.es_folder());
            }
        }
    }

    /// The pack name must never mean a *different* platform to the reader that consumes the
    /// same convention. `None` is fine -- not every system has a published pack.
    #[test]
    fn dat_folder_names_never_resolve_elsewhere() {
        for p in all_platforms() {
            let name = p.dat_folder_name();
            let got = platform_from_dat_name(&name);
            assert!(got == Platform::Unknown || same_machine(got, p), "{p:?}: pack folder name {name:?} resolves to {got:?}");
        }
    }

    /// The ES folder names are also what the scan's own directory heuristic reads, so one
    /// that it maps somewhere else would make a copied tree rescan as the wrong platform.
    #[test]
    fn es_folders_never_rescan_as_another_platform() {
        for p in all_platforms() {
            let dir = std::path::Path::new(p.es_folder()).join("game.bin");
            if let Some(got) = crate::rom::detect::infer_platform_from_parent_path_heuristics(&dir) {
                assert!(same_machine(got, p), "{p:?}: ES folder {:?} is detected as {got:?}", p.es_folder());
            }
        }
    }

    /// Combining is one level deep: `combined_platform` resolves in a single step, so a
    /// parent that were itself someone's child would silently land in the wrong folder.
    #[test]
    fn combine_groups_are_one_level() {
        for g in COMBINE_GROUPS {
            assert!(combine_group_of(g.parent).is_none(), "group {:?}'s parent {:?} is itself a child", g.key, g.parent);
            for c in g.children {
                assert_eq!(combine_group_of(*c).map(|x| x.key), Some(g.key), "{c:?} is claimed by more than one group");
            }
        }
    }

    #[test]
    fn combine_group_keys_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for g in COMBINE_GROUPS {
            assert!(seen.insert(g.key), "duplicate combine group key {:?}", g.key);
        }
    }

    #[test]
    fn unknown_has_a_home() {
        assert_eq!(Platform::Unknown.es_folder(), "unknown");
    }
}
