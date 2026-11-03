use std::{fmt, io::Result};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use crate::rom::hash::{HashFlags, HashOptions, FileHashRecord, HashReport, hash};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArchiveFormat {
    Unknown,
    RawIso,
    Zip,
    SevenZip,
    Chd,
    Cso,
    Pbp,
    Xiso,
    Rvz,
    Wia,
    Gcz,
}

impl Default for ArchiveFormat {
    fn default() -> Self {
        Self::Unknown
    }
}

impl fmt::Display for ArchiveFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tag = match self {
            Self::Unknown => "unknown",
            Self::RawIso => "raw_iso",
            Self::Zip => "zip",
            Self::SevenZip => "7z",
            Self::Chd => "chd",
            Self::Cso => "cso",
            Self::Xiso => "xiso",
            Self::Rvz => "rvz",
            Self::Wia => "wia",
            Self::Gcz => "gcz",
            Self::Pbp => "pbp",
        };
        write!(f, "{tag}")
    }
}



/// Dialect of a cue/gdi sheet written for a disc (see `rom::detect::sheet::CueSheetOptions`
/// for the knobs each one sets): `Redump` = Redump's naming (no `(Track 1)` suffix on a
/// single-track disc, CRLF), `RedumpCatalog` = the same plus a `CATALOG 0000000000000`
/// first line, `Chdman` = exactly what `chdman extractcd` writes on this platform, `Mame` =
/// chdman's `.toc` form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CueSheetStyle {
    Redump,
    RedumpCatalog,
    Chdman,
    Mame,
    Unknown,
}

impl Default for CueSheetStyle {
    fn default() -> Self {
        Self::Redump
    }
}

impl fmt::Display for CueSheetStyle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tag = match self {
            Self::Redump => "redump",
            Self::RedumpCatalog => "redump_catalog",
            Self::Chdman => "chdman",
            Self::Mame => "mame",
            Self::Unknown => "unknown",
        };
        write!(f, "{tag}")
    }
}

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    Unknown = 0,
    Linux = 3,
    Nintendo64 = 4,
    Wii = 5,
    PcWindows = 6,
    PlayStation = 7,
    PlayStation2 = 8,
    PlayStation3 = 9,
    Xbox = 11,
    Xbox360 = 12,
    PcDos = 13,
    Mac = 14,
    Nes = 18,
    Snes = 19,
    NintendoDs = 20,
    GameCube = 21,
    GameBoyColor = 22,
    Dreamcast = 23,
    GameBoyAdvance = 24,
    Msx = 27,
    SegaGenesis = 29,
    Sega32x = 30,
    SegaSaturn = 32,
    GameBoy = 33,
    SegaGameGear = 35,
    Nintendo3ds = 37,
    PlayStationPortable = 38,
    WiiU = 41,
    PlayStationVita = 46,
    PlayStation4 = 48,
    XboxOne = 49,
    Arcade = 52,
    Msx2 = 53,
    SuperFamicom = 58,
    Atari2600 = 59,
    Atari7800 = 60,
    AtariLynx = 61,
    AtariJaguar = 62,
    AtariStSte = 63,
    SegaMasterSystem = 64,
    Atari8Bit = 65,
    Atari5200 = 66,
    SegaCd = 78,
    NeoGeoMvs = 79,
    NeoGeoAes = 80,
    Sg1000 = 84,
    TurboGrafx16 = 86,
    VirtualBoy = 87,
    Famicom = 99,
    FmTowns = 118,
    NeoGeoPocket = 119,
    NeoGeoPocketColor = 120,
    SharpX68000 = 121,
    Pc8801 = 125,
    SuperGrafx = 128,
    NintendoSwitch = 130,
    NeoGeoCd = 136,
    NewNintendo3ds = 137,
    Pc98 = 149,
    TurboGrafxCd = 150,
    Fm7 = 152,
    Pc6000Series = 157,
    NintendoDsi = 159,
    PlayStationVr = 165,
    PokemonMini = 166,
    PlayStation5 = 167,
    XboxSeries = 169,
    GoogleStadia = 170,
    NintendoSwitch2 = 508,
    /// 3DO Interactive Multiplayer (IGDB 50): CD-based, Opera filesystem (no ISO9660).
    ThreeDo = 50,

    // Not present in the referenced IGDB list snapshot; keep as placeholder.
    PcFx = 2048,

    // IGDB exposes no public numeric ID for these platforms. These values are explicit
    // internal sentinels so the enum can still represent them without pretending they
    // are official IGDB IDs.
    FamicomDiskSystem = 30000,
    PocketStation = 30001,
    MsxTurbo = 30002,
    Msx2Plus = 30003,
    WonderSwan = 30004,
    WonderSwanColor = 30005,
    SegaPico = 30006,
    Nintendo64Dd = 30007,
    SufamiTurbo = 30008,
    /// PS1 classics packaged for PSP (PBP with a PS1 image), kept apart from PSP so callers can sort them as PSP, PS1 or PlayStation Classic.
    PspPs1Classic = 30009,
    /// Downloadable DSi titles (DSiWare), kept apart from DSi so callers can sort them either way.
    DsiWare = 30010,
    // Home computers TOSEC lists that IGDB has no platform of its own for.
    Zx81 = 30011,
    Oric = 30012,
    ThomsonMoTo = 30013,
    SamCoupe = 30014,
    Enterprise = 30015,
    // Home computers and consoles under their IGDB ids.
    Commodore64 = 15,
    Amiga = 16,
    AmstradCpc = 25,
    ZxSpectrum = 26,
    Intellivision = 67,
    ColecoVision = 68,
    BbcMicro = 69,
    Vectrex = 70,
    Vic20 = 71,
    Ouya = 72,
    // Phones: never emulated here, but official versions of a game live on them (the
    // Definitive tab's provider-only versions).
    Android = 34,
    Ios = 39,
    AppleII = 75,
    CommodorePet = 90,
    Astrocade = 91,
    Commodore16 = 93,
    AmigaCd32 = 114,
    AppleIIgs = 115,
    AcornArchimedes = 116,
    PhilipsCdi = 117,
    Trs80 = 126,
    Ti99 = 129,
    Odyssey2 = 133,
    AcornElectron = 134,
    Trs80CoCo = 151,
    Dragon32 = 153,
    CommodoreCdtv = 158,
}

impl Default for Platform {
    fn default() -> Self {
        Self::Unknown
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tag = match self {
            Self::Unknown => "unknown",
            Self::Nintendo64 => "Nintendo 64",
            Self::Wii => "Wii",
            Self::PlayStation => "PlayStation",
            Self::PlayStation2 => "PlayStation 2",
            Self::PlayStation3 => "PlayStation 3",
            Self::Xbox => "Xbox",
            Self::Xbox360 => "Xbox 360",
            Self::Nes | Self::Famicom => "Family Computer",
            Self::FamicomDiskSystem => "Family Computer Disk System",
            Self::Snes | Self::SuperFamicom => "Super Famicom",
            Self::NintendoDs | Self::NintendoDsi => "Nintendo DS",
            Self::GameCube => "Nintendo GameCube",
            Self::GameBoyColor => "Game Boy Color",
            Self::Dreamcast => "Sega Dreamcast",
            Self::GameBoyAdvance => "Game Boy Advance",
            Self::Msx => "MSX",
            Self::Msx2 => "MSX2",
            Self::MsxTurbo => "MSXturboR",
            Self::Msx2Plus => "MSX2+",
            Self::SegaGenesis => "Sega Genesis",
            Self::Sega32x => "Sega 32X",
            Self::SegaSaturn => "Sega Saturn",
            Self::GameBoy => "Game Boy",
            Self::SegaGameGear => "Sega Game Gear",
            Self::Nintendo3ds | Self::NewNintendo3ds => "Nintendo 3DS",
            Self::PlayStationPortable => "PlayStation Portable",
            Self::PocketStation => "PocketStation",
            Self::Nintendo64Dd => "Nintendo 64DD",
            Self::SufamiTurbo => "Sufami Turbo",
            Self::PspPs1Classic => "PSP PS1 Classic",
            Self::DsiWare => "Nintendo DSiWare",
            Self::WiiU => "Wii U",
            Self::Arcade => "Arcade",
            Self::SegaMasterSystem => "Sega Master System",
            Self::SegaCd => "Sega CD",
            Self::NeoGeoMvs | Self::NeoGeoAes => "Neo Geo AES",
            Self::NeoGeoCd => "Neo Geo CD",
            Self::NeoGeoPocket => "Neo Geo Pocket",
            Self::NeoGeoPocketColor => "Neo Geo Pocket Color",
            Self::TurboGrafx16 => "TurboGrafx-16",
            Self::TurboGrafxCd => "TurboGrafx-CD",
            Self::SuperGrafx => "SuperGrafx",
            Self::Pc8801 => "PC-8801",
            Self::Pc98 => "PC-98",
            Self::Fm7 => "FM-7",
            Self::FmTowns => "FM Towns",
            Self::PcFx => "pcfx",
            Self::Sg1000 => "sg1000",
            Self::WonderSwan => "WonderSwan",
            Self::WonderSwanColor => "WonderSwan Color",
            Self::SegaPico => "Sega Pico",
            Self::VirtualBoy => "Virtual Boy",
            Self::PokemonMini => "pokemonmini",
            Self::NintendoSwitch | Self::NintendoSwitch2 => "switch",
            Self::ThreeDo => "3DO",
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
            _ => "unknown",
        };

        write!(f, "{tag}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlatformNameStyle {
    Canonical,
    NorthAmerica,
    Europe,
    Japan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Region {
    World,
    Asia,
    Europe,
    Japan,
    Usa,
    Australia,
    Uk,
    France,
    Germany,
    Italy,
    Spain,
    Korea,
    Canada,
    Brazil,
    Taiwan,
    HongKong,
    China,
    Netherlands,
    Sweden,
    Russia,
    Poland,
    Other,
}

impl Region {
    pub fn code(self) -> &'static str {
        match self {
            Self::World => "WORLD",
            Self::Asia => "ASIA",
            Self::Europe => "EU",
            Self::Japan => "JP",
            Self::Usa => "US",
            Self::Australia => "AU",
            Self::Uk => "UK",
            Self::France => "FR",
            Self::Germany => "DE",
            Self::Italy => "IT",
            Self::Spain => "ES",
            Self::Korea => "KR",
            Self::Canada => "CA",
            Self::Brazil => "BR",
            Self::Taiwan => "TW",
            Self::HongKong => "HK",
            Self::China => "CN",
            Self::Netherlands => "NL",
            Self::Sweden => "SE",
            Self::Russia => "RU",
            Self::Poland => "PL",
            Self::Other => "OTHER",
        }
    }

    /// Inverse of [`Self::code`] (case-insensitive); unknown codes -> `Other`.
    pub fn from_code(code: &str) -> Self {
        match code.trim().to_ascii_uppercase().as_str() {
            "WORLD" => Self::World,
            "ASIA" => Self::Asia,
            "EU" => Self::Europe,
            "JP" => Self::Japan,
            "US" => Self::Usa,
            "AU" => Self::Australia,
            "UK" => Self::Uk,
            "FR" => Self::France,
            "DE" => Self::Germany,
            "IT" => Self::Italy,
            "ES" => Self::Spain,
            "KR" => Self::Korea,
            "CA" => Self::Canada,
            "BR" => Self::Brazil,
            "TW" => Self::Taiwan,
            "HK" => Self::HongKong,
            "CN" => Self::China,
            "NL" => Self::Netherlands,
            "SE" => Self::Sweden,
            "RU" => Self::Russia,
            "PL" => Self::Poland,
            _ => Self::Other,
        }
    }

    pub const ALL: [Region; 22] = [
        Self::World, Self::Asia, Self::Europe, Self::Japan, Self::Usa, Self::Australia, Self::Uk, Self::France, Self::Germany, Self::Italy, Self::Spain, Self::Korea, Self::Canada, Self::Brazil, Self::Taiwan, Self::HongKong, Self::China, Self::Netherlands,
        Self::Sweden, Self::Russia, Self::Poland, Self::Other,
    ];

    pub fn display_name(self) -> &'static str {
        match self {
            Self::World => "World",
            Self::Asia => "Asia",
            Self::Europe => "Europe",
            Self::Japan => "Japan",
            Self::Usa => "USA",
            Self::Australia => "Australia",
            Self::Uk => "United Kingdom",
            Self::France => "France",
            Self::Germany => "Germany",
            Self::Italy => "Italy",
            Self::Spain => "Spain",
            Self::Korea => "Korea",
            Self::Canada => "Canada",
            Self::Brazil => "Brazil",
            Self::Taiwan => "Taiwan",
            Self::HongKong => "Hong Kong",
            Self::China => "China",
            Self::Netherlands => "Netherlands",
            Self::Sweden => "Sweden",
            Self::Russia => "Russia",
            Self::Poland => "Poland",
            Self::Other => "Other",
        }
    }

    /// A region as a DAT's structured attribute spells it -- a full name ("USA",
    /// "Europe", "United Kingdom"), a 3-letter code ("JPN"), or one of `code`'s 2-letter
    /// codes -- case-insensitive. `None` for anything unrecognized (kept verbatim by the
    /// caller rather than collapsed to `Other`).
    pub fn from_name(name: &str) -> Option<Self> {
        let key = name.trim().to_ascii_lowercase();
        let by_name = match key.as_str() {
            "world" | "wor" => Some(Self::World),
            "asia" | "asi" => Some(Self::Asia),
            "europe" | "eur" | "pal" => Some(Self::Europe),
            "japan" | "jpn" => Some(Self::Japan),
            "usa" | "united states" | "united states of america" | "america" | "ntsc-u" => Some(Self::Usa),
            "australia" | "aus" => Some(Self::Australia),
            "united kingdom" | "uk" | "gbr" | "england" | "great britain" => Some(Self::Uk),
            "france" | "fra" => Some(Self::France),
            "germany" | "ger" | "deu" => Some(Self::Germany),
            "italy" | "ita" => Some(Self::Italy),
            "spain" | "esp" => Some(Self::Spain),
            "korea" | "south korea" | "kor" => Some(Self::Korea),
            "canada" | "can" => Some(Self::Canada),
            "brazil" | "brasil" | "bra" => Some(Self::Brazil),
            "taiwan" | "twn" => Some(Self::Taiwan),
            "hong kong" | "hongkong" | "hkg" => Some(Self::HongKong),
            "china" | "chn" => Some(Self::China),
            "netherlands" | "holland" | "nld" => Some(Self::Netherlands),
            "sweden" | "swe" => Some(Self::Sweden),
            "russia" | "rus" => Some(Self::Russia),
            "poland" | "pol" => Some(Self::Poland),
            _ => None,
        };
        by_name.or_else(|| {
            let code = Self::from_code(&key);
            (code != Self::Other).then_some(code)
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Language {
    Ja,
    En,
    Fr,
    Es,
    It,
    De,
    Kr,
    /// Chinese, script unspecified (what a DAT's "Zh" tag says); `ZhT`/`ZhS` when known.
    Zh,
    ZhT,
    ZhS,
    Pt,
    Nl,
    Sv,
    Da,
    No,
    Fi,
    Ru,
    Pl,
    Cs,
    Tr,
    Hu,
    El,
    Ca,
    Other,
}

impl Language {
    pub fn code(self) -> &'static str {
        match self {
            Self::Ja => "ja",
            Self::En => "en",
            Self::Fr => "fr",
            Self::Es => "es",
            Self::It => "it",
            Self::De => "de",
            Self::Kr => "kr",
            Self::Zh => "zh",
            Self::ZhT => "zh-t",
            Self::ZhS => "zh-s",
            Self::Pt => "pt",
            Self::Nl => "nl",
            Self::Sv => "sv",
            Self::Da => "da",
            Self::No => "no",
            Self::Fi => "fi",
            Self::Ru => "ru",
            Self::Pl => "pl",
            Self::Cs => "cs",
            Self::Tr => "tr",
            Self::Hu => "hu",
            Self::El => "el",
            Self::Ca => "ca",
            Self::Other => "other",
        }
    }

    /// Inverse of [`Self::code`] (case-insensitive; "ko" is accepted for `Kr`); unknown
    /// codes -> `Other`.
    pub fn from_code(code: &str) -> Self {
        match code.trim().to_ascii_lowercase().as_str() {
            "ja" => Self::Ja,
            "en" => Self::En,
            "fr" => Self::Fr,
            "es" => Self::Es,
            "it" => Self::It,
            "de" => Self::De,
            "kr" | "ko" => Self::Kr,
            "zh" => Self::Zh,
            "zh-t" | "zh-tw" | "zh-hant" => Self::ZhT,
            "zh-s" | "zh-cn" | "zh-hans" => Self::ZhS,
            "pt" => Self::Pt,
            "nl" => Self::Nl,
            "sv" => Self::Sv,
            "da" => Self::Da,
            "no" => Self::No,
            "fi" => Self::Fi,
            "ru" => Self::Ru,
            "pl" => Self::Pl,
            "cs" => Self::Cs,
            "tr" => Self::Tr,
            "hu" => Self::Hu,
            "el" => Self::El,
            "ca" => Self::Ca,
            _ => Self::Other,
        }
    }

    pub const ALL: [Language; 24] = [
        Self::Ja, Self::En, Self::Fr, Self::Es, Self::It, Self::De, Self::Kr, Self::Zh, Self::ZhT, Self::ZhS, Self::Pt, Self::Nl, Self::Sv, Self::Da, Self::No, Self::Fi, Self::Ru, Self::Pl, Self::Cs, Self::Tr, Self::Hu, Self::El, Self::Ca,
        Self::Other,
    ];

    pub fn english_name(self) -> &'static str {
        match self {
            Self::Ja => "Japanese",
            Self::En => "English",
            Self::Fr => "French",
            Self::Es => "Spanish",
            Self::It => "Italian",
            Self::De => "German",
            Self::Kr => "Korean",
            Self::Zh => "Chinese",
            Self::ZhT => "Chinese (Traditional)",
            Self::ZhS => "Chinese (Simplified)",
            Self::Pt => "Portuguese",
            Self::Nl => "Dutch",
            Self::Sv => "Swedish",
            Self::Da => "Danish",
            Self::No => "Norwegian",
            Self::Fi => "Finnish",
            Self::Ru => "Russian",
            Self::Pl => "Polish",
            Self::Cs => "Czech",
            Self::Tr => "Turkish",
            Self::Hu => "Hungarian",
            Self::El => "Greek",
            Self::Ca => "Catalan",
            Self::Other => "Other",
        }
    }

    /// The language's own name for itself, in its own script.
    pub fn native_name(self) -> &'static str {
        match self {
            Self::Ja => "日本語",
            Self::En => "English",
            Self::Fr => "Français",
            Self::Es => "Español",
            Self::It => "Italiano",
            Self::De => "Deutsch",
            Self::Kr => "한국어",
            Self::Zh => "中文",
            Self::ZhT => "繁體中文",
            Self::ZhS => "简体中文",
            Self::Pt => "Português",
            Self::Nl => "Nederlands",
            Self::Sv => "Svenska",
            Self::Da => "Dansk",
            Self::No => "Norsk",
            Self::Fi => "Suomi",
            Self::Ru => "Русский",
            Self::Pl => "Polski",
            Self::Cs => "Čeština",
            Self::Tr => "Türkçe",
            Self::Hu => "Magyar",
            Self::El => "Ελληνικά",
            Self::Ca => "Català",
            Self::Other => "Other",
        }
    }

    /// An English language *name* ("English", "Japanese", ...; case-insensitive) -- the
    /// form some DATs put in a structured attribute. Codes are deliberately not accepted
    /// here: a 2-letter token's meaning depends on where it appeared (see `from_code`).
    pub fn from_name(name: &str) -> Option<Self> {
        let key = name.trim().to_ascii_lowercase();
        Self::ALL.into_iter().filter(|l| *l != Self::Other).find(|l| l.english_name().to_ascii_lowercase() == key)
    }
}

impl Platform {
    /// The year the hardware came out (the first region's), for telling an original from its
    /// ports: a game is ported to newer machines far more often than to older ones. None for
    /// general-purpose PCs -- console games are ported to them all the time, and their
    /// launch year would make every PC version look like the original.
    pub fn launch_year(self) -> Option<u16> {
        Some(match self {
            Self::Arcade => 1971,
            Self::Atari2600 => 1977,
            Self::Atari8Bit => 1979,
            Self::Pc8801 => 1981,
            Self::Pc6000Series => 1981,
            Self::Atari5200 | Self::Pc98 | Self::Fm7 => 1982,
            Self::Msx | Self::Nes | Self::Famicom | Self::Sg1000 => 1983,
            Self::Atari7800 | Self::SegaMasterSystem => 1985,
            Self::FamicomDiskSystem | Self::AtariStSte => 1986,
            Self::Msx2 | Self::SharpX68000 | Self::TurboGrafx16 => 1987,
            Self::SegaGenesis | Self::TurboGrafxCd => 1988,
            Self::GameBoy | Self::AtariLynx | Self::FmTowns => 1989,
            Self::Snes | Self::SuperFamicom | Self::NeoGeoMvs | Self::NeoGeoAes | Self::SegaGameGear | Self::SuperGrafx => 1990,
            Self::SegaCd | Self::Msx2Plus => 1991,
            Self::AtariJaguar | Self::ThreeDo | Self::SegaPico => 1993,
            Self::PlayStation | Self::SegaSaturn | Self::Sega32x | Self::NeoGeoCd | Self::PcFx => 1994,
            Self::VirtualBoy | Self::SufamiTurbo => 1995,
            Self::Nintendo64 => 1996,
            Self::GameBoyColor | Self::Dreamcast | Self::NeoGeoPocket => 1998,
            Self::NeoGeoPocketColor | Self::WonderSwan | Self::Nintendo64Dd | Self::PocketStation => 1999,
            Self::PlayStation2 | Self::WonderSwanColor | Self::MsxTurbo => 2000,
            Self::GameBoyAdvance | Self::GameCube | Self::Xbox | Self::PokemonMini => 2001,
            Self::NintendoDs | Self::PlayStationPortable => 2004,
            Self::Xbox360 => 2005,
            Self::Wii | Self::PlayStation3 => 2006,
            Self::NintendoDsi | Self::DsiWare => 2008,
            Self::Nintendo3ds | Self::PlayStationVita => 2011,
            Self::WiiU | Self::PlayStation4 | Self::XboxOne => 2012,
            Self::NewNintendo3ds | Self::PlayStationVr => 2014,
            Self::NintendoSwitch => 2017,
            Self::GoogleStadia => 2019,
            Self::PlayStation5 | Self::XboxSeries => 2020,
            Self::NintendoSwitch2 => 2025,
            Self::Commodore64 => 1982,
            Self::Amiga => 1985,
            Self::AmstradCpc => 1984,
            Self::ZxSpectrum => 1982,
            Self::Intellivision => 1979,
            Self::ColecoVision => 1982,
            Self::BbcMicro => 1981,
            Self::Vectrex => 1982,
            Self::Vic20 => 1980,
            Self::Ouya => 2013,
            Self::Ios => 2007,
            Self::Android => 2008,
            Self::AppleII => 1977,
            Self::CommodorePet => 1977,
            Self::Astrocade => 1977,
            Self::Commodore16 => 1984,
            Self::AmigaCd32 => 1993,
            Self::AppleIIgs => 1986,
            Self::AcornArchimedes => 1987,
            Self::PhilipsCdi => 1991,
            Self::Trs80 => 1977,
            Self::Ti99 => 1981,
            Self::Odyssey2 => 1978,
            Self::AcornElectron => 1983,
            Self::Trs80CoCo => 1980,
            Self::Dragon32 => 1982,
            Self::CommodoreCdtv => 1991,
            Self::Zx81 => 1981,
            Self::Oric => 1983,
            Self::ThomsonMoTo => 1984,
            Self::SamCoupe => 1989,
            Self::Enterprise => 1985,
            // Original PlayStation games run on a PSP: never where one came out first.
            Self::PspPs1Classic => 2004,
            Self::Unknown | Self::Linux | Self::PcWindows | Self::PcDos | Self::Mac => return None,
        })
    }

    pub fn igdb_id(self) -> u16 {
        self as u16
    }

    pub fn from_igdb_id(id: u16) -> Option<Self> {
        Some(match id {
            0 => Self::Unknown,
            2048 => Self::PcFx,
            30000 => Self::FamicomDiskSystem,
            30001 => Self::PocketStation,
            30002 => Self::MsxTurbo,
            30003 => Self::Msx2Plus,
            30004 => Self::WonderSwan,
            30005 => Self::WonderSwanColor,
            30006 => Self::SegaPico,
            30007 => Self::Nintendo64Dd,
            30008 => Self::SufamiTurbo,
            30009 => Self::PspPs1Classic,
            30010 => Self::DsiWare,
            3 => Self::Linux,
            4 => Self::Nintendo64,
            5 => Self::Wii,
            6 => Self::PcWindows,
            7 => Self::PlayStation,
            8 => Self::PlayStation2,
            9 => Self::PlayStation3,
            11 => Self::Xbox,
            12 => Self::Xbox360,
            13 => Self::PcDos,
            14 => Self::Mac,
            18 => Self::Nes,
            19 => Self::Snes,
            20 => Self::NintendoDs,
            21 => Self::GameCube,
            22 => Self::GameBoyColor,
            23 => Self::Dreamcast,
            24 => Self::GameBoyAdvance,
            27 => Self::Msx,
            29 => Self::SegaGenesis,
            30 => Self::Sega32x,
            32 => Self::SegaSaturn,
            33 => Self::GameBoy,
            35 => Self::SegaGameGear,
            37 => Self::Nintendo3ds,
            38 => Self::PlayStationPortable,
            41 => Self::WiiU,
            46 => Self::PlayStationVita,
            48 => Self::PlayStation4,
            49 => Self::XboxOne,
            52 => Self::Arcade,
            53 => Self::Msx2,
            58 => Self::SuperFamicom,
            59 => Self::Atari2600,
            60 => Self::Atari7800,
            61 => Self::AtariLynx,
            62 => Self::AtariJaguar,
            63 => Self::AtariStSte,
            64 => Self::SegaMasterSystem,
            65 => Self::Atari8Bit,
            66 => Self::Atari5200,
            78 => Self::SegaCd,
            79 => Self::NeoGeoMvs,
            80 => Self::NeoGeoAes,
            84 => Self::Sg1000,
            86 => Self::TurboGrafx16,
            87 => Self::VirtualBoy,
            99 => Self::Famicom,
            118 => Self::FmTowns,
            119 => Self::NeoGeoPocket,
            120 => Self::NeoGeoPocketColor,
            121 => Self::SharpX68000,
            125 => Self::Pc8801,
            128 => Self::SuperGrafx,
            130 => Self::NintendoSwitch,
            136 => Self::NeoGeoCd,
            137 => Self::NewNintendo3ds,
            149 => Self::Pc98,
            150 => Self::TurboGrafxCd,
            152 => Self::Fm7,
            157 => Self::Pc6000Series,
            159 => Self::NintendoDsi,
            165 => Self::PlayStationVr,
            166 => Self::PokemonMini,
            167 => Self::PlayStation5,
            169 => Self::XboxSeries,
            170 => Self::GoogleStadia,
            508 => Self::NintendoSwitch2,
            50 => Self::ThreeDo,
            15 => Self::Commodore64,
            16 => Self::Amiga,
            25 => Self::AmstradCpc,
            26 => Self::ZxSpectrum,
            67 => Self::Intellivision,
            68 => Self::ColecoVision,
            69 => Self::BbcMicro,
            70 => Self::Vectrex,
            71 => Self::Vic20,
            72 => Self::Ouya,
            34 => Self::Android,
            39 => Self::Ios,
            75 => Self::AppleII,
            90 => Self::CommodorePet,
            91 => Self::Astrocade,
            93 => Self::Commodore16,
            114 => Self::AmigaCd32,
            115 => Self::AppleIIgs,
            116 => Self::AcornArchimedes,
            117 => Self::PhilipsCdi,
            126 => Self::Trs80,
            129 => Self::Ti99,
            133 => Self::Odyssey2,
            134 => Self::AcornElectron,
            151 => Self::Trs80CoCo,
            153 => Self::Dragon32,
            158 => Self::CommodoreCdtv,
            30011 => Self::Zx81,
            30012 => Self::Oric,
            30013 => Self::ThomsonMoTo,
            30014 => Self::SamCoupe,
            30015 => Self::Enterprise,
            _ => return None,
        })
    }

    pub fn is_planned_support(self) -> bool {
        matches!(
            self,
            Self::PlayStation
                | Self::PlayStation2
                | Self::PlayStation3
                | Self::PlayStation4
                | Self::PlayStation5
                | Self::PlayStationPortable
                | Self::Dreamcast
                | Self::SegaSaturn
                | Self::SegaCd
                | Self::NeoGeoCd
                | Self::TurboGrafxCd
                | Self::GameCube
                | Self::Wii
                | Self::WiiU
                | Self::NintendoSwitch
                | Self::NintendoSwitch2
                | Self::Pc8801
                | Self::Pc98
                | Self::PcFx
                | Self::FmTowns
                | Self::Msx
                | Self::Msx2
                | Self::MsxTurbo
                | Self::Msx2Plus
                | Self::FamicomDiskSystem
                | Self::PocketStation
                | Self::Nintendo64Dd
                | Self::SufamiTurbo
                | Self::PspPs1Classic
                | Self::DsiWare
        )
    }

    pub fn is_arcade_family(self) -> bool {
        matches!(self, Self::Arcade | Self::NeoGeoMvs)
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Linux => "Linux",
            Self::Nintendo64 => "Nintendo 64",
            Self::Wii => "Wii",
            Self::PcWindows => "PC (Microsoft Windows)",
            Self::PlayStation => "PlayStation",
            Self::PlayStation2 => "PlayStation 2",
            Self::PlayStation3 => "PlayStation 3",
            Self::Xbox => "Xbox",
            Self::Xbox360 => "Xbox 360",
            Self::PcDos => "PC DOS",
            Self::Mac => "Mac",
            Self::Nes => "Nintendo Entertainment System",
            Self::Snes => "Super Nintendo Entertainment System",
            Self::NintendoDs => "Nintendo DS",
            Self::GameCube => "Nintendo GameCube",
            Self::GameBoyColor => "Game Boy Color",
            Self::Dreamcast => "Dreamcast",
            Self::GameBoyAdvance => "Game Boy Advance",
            Self::Msx => "MSX",
            Self::SegaGenesis => "Sega Mega Drive/Genesis",
            Self::Sega32x => "Sega 32X",
            Self::SegaCd => "Sega CD",
            Self::NeoGeoMvs => "Neo Geo MVS",
            Self::NeoGeoAes => "Neo Geo AES",
            Self::NeoGeoCd => "Neo Geo CD",
            Self::SegaSaturn => "Sega Saturn",
            Self::GameBoy => "Game Boy",
            Self::SegaGameGear => "Sega Game Gear",
            Self::Nintendo3ds => "Nintendo 3DS",
            Self::PlayStationPortable => "PlayStation Portable",
            Self::PocketStation => "PocketStation",
            Self::WiiU => "Wii U",
            Self::PlayStationVita => "PlayStation Vita",
            Self::PlayStation4 => "PlayStation 4",
            Self::XboxOne => "Xbox One",
            Self::Arcade => "Arcade (MAME family)",
            Self::Msx2 => "MSX2",
            Self::MsxTurbo => "MSXturboR",
            Self::Msx2Plus => "MSX2+",
            Self::WonderSwan => "WonderSwan",
            Self::WonderSwanColor => "WonderSwan Color",
            Self::SegaPico => "Sega Pico",
            Self::Nintendo64Dd => "Nintendo 64DD",
            Self::SufamiTurbo => "Sufami Turbo",
            Self::PspPs1Classic => "PSP PS1 Classic",
            Self::DsiWare => "Nintendo DSiWare",
            Self::SuperFamicom => "Super Famicom",
            Self::Atari2600 => "Atari 2600",
            Self::Atari7800 => "Atari 7800",
            Self::AtariLynx => "Atari Lynx",
            Self::AtariJaguar => "Atari Jaguar",
            Self::AtariStSte => "Atari ST/STE",
            Self::SegaMasterSystem => "Sega Master System",
            Self::Atari8Bit => "Atari 8-bit",
            Self::Atari5200 => "Atari 5200",
            Self::Sg1000 => "SG-1000",
            Self::TurboGrafx16 => "TurboGrafx-16",
            Self::VirtualBoy => "Virtual Boy",
            Self::Famicom => "Family Computer (Famicom)",
            Self::FamicomDiskSystem => "Family Computer Disk System",
            Self::FmTowns => "FM Towns",
            Self::NeoGeoPocket => "Neo Geo Pocket",
            Self::NeoGeoPocketColor => "Neo Geo Pocket Color",
            Self::SharpX68000 => "Sharp X68000",
            Self::Pc8801 => "PC-8801",
            Self::SuperGrafx => "SuperGrafx",
            Self::NintendoSwitch => "Nintendo Switch",
            Self::NewNintendo3ds => "New Nintendo 3DS",
            Self::Pc98 => "PC-98",
            Self::TurboGrafxCd => "TurboGrafx-CD",
            Self::Fm7 => "FM-7",
            Self::Pc6000Series => "NEC PC-6000 Series",
            Self::NintendoDsi => "Nintendo DSi",
            Self::PlayStationVr => "PlayStation VR",
            Self::PokemonMini => "Pokemon mini",
            Self::PlayStation5 => "PlayStation 5",
            Self::XboxSeries => "Xbox Series",
            Self::GoogleStadia => "Google Stadia",
            Self::NintendoSwitch2 => "Nintendo Switch 2",
            Self::PcFx => "PC-FX",
            Self::ThreeDo => "3DO Interactive Multiplayer",
            Self::Commodore64 => "Commodore 64",
            Self::Amiga => "Amiga",
            Self::AmstradCpc => "Amstrad CPC",
            Self::ZxSpectrum => "ZX Spectrum",
            Self::Intellivision => "Intellivision",
            Self::ColecoVision => "ColecoVision",
            Self::BbcMicro => "BBC Micro",
            Self::Vectrex => "Vectrex",
            Self::Vic20 => "VIC-20",
            Self::Ouya => "Ouya",
            Self::Android => "Android",
            Self::Ios => "iOS",
            Self::AppleII => "Apple II",
            Self::CommodorePet => "Commodore PET",
            Self::Astrocade => "Bally Astrocade",
            Self::Commodore16 => "Commodore 16 / Plus/4",
            Self::AmigaCd32 => "Amiga CD32",
            Self::AppleIIgs => "Apple IIGS",
            Self::AcornArchimedes => "Acorn Archimedes",
            Self::PhilipsCdi => "Philips CD-i",
            Self::Trs80 => "TRS-80",
            Self::Ti99 => "TI-99/4A",
            Self::Odyssey2 => "Odyssey² / Videopac",
            Self::AcornElectron => "Acorn Electron",
            Self::Trs80CoCo => "TRS-80 Color Computer",
            Self::Dragon32 => "Dragon 32/64",
            Self::CommodoreCdtv => "Commodore CDTV",
            Self::Zx81 => "ZX81",
            Self::Oric => "Oric",
            Self::ThomsonMoTo => "Thomson MO/TO",
            Self::SamCoupe => "SAM Coupé",
            Self::Enterprise => "Enterprise 64/128",
        }
    }

    pub fn display_name_with_style(self, style: PlatformNameStyle, region: Region) -> &'static str {
        match (self, style, region) {
            (Self::Nes, PlatformNameStyle::Japan, _) => "Family Computer",
            (Self::Nes, PlatformNameStyle::NorthAmerica, _) => "Nintendo Entertainment System",
            (Self::Nes, PlatformNameStyle::Europe, _) => "Nintendo Entertainment System",

            (Self::Famicom, PlatformNameStyle::Japan, _) => "Family Computer",
            (Self::Famicom, PlatformNameStyle::NorthAmerica, _) => "Nintendo Entertainment System",
            (Self::Famicom, PlatformNameStyle::Europe, _) => "Nintendo Entertainment System",
            (Self::FamicomDiskSystem, PlatformNameStyle::Japan, _) => "Family Computer Disk System",
            (Self::FamicomDiskSystem, PlatformNameStyle::NorthAmerica, _) => "Nintendo Entertainment System",
            (Self::FamicomDiskSystem, PlatformNameStyle::Europe, _) => "Nintendo Entertainment System",

            (Self::Snes, PlatformNameStyle::Japan, _) => "Super Famicom",
            (Self::Snes, PlatformNameStyle::NorthAmerica, _) => "Super Nintendo Entertainment System",
            (Self::Snes, PlatformNameStyle::Europe, _) => "Super Nintendo Entertainment System",

            (Self::SuperFamicom, PlatformNameStyle::Japan, _) => "Super Famicom",
            (Self::SuperFamicom, PlatformNameStyle::NorthAmerica, _) => "Super Nintendo Entertainment System",
            (Self::SuperFamicom, PlatformNameStyle::Europe, _) => "Super Nintendo Entertainment System",

            (Self::SegaGenesis, PlatformNameStyle::Japan, _) => "Mega Drive",
            (Self::SegaGenesis, PlatformNameStyle::Europe, _) => "Mega Drive",
            (Self::SegaGenesis, PlatformNameStyle::NorthAmerica, _) => "Genesis",

            (Self::SegaCd, PlatformNameStyle::Japan, _) => "Mega-CD",
            (Self::SegaCd, PlatformNameStyle::Europe, _) => "Mega-CD",
            (Self::SegaCd, PlatformNameStyle::NorthAmerica, _) => "Sega CD",

            (Self::TurboGrafxCd, PlatformNameStyle::Japan, _) => "PC Engine CD-ROM²",
            (Self::TurboGrafxCd, PlatformNameStyle::Europe, _) => "TurboGrafx-CD",
            (Self::TurboGrafxCd, PlatformNameStyle::NorthAmerica, _) => "TurboGrafx-CD",

            (Self::TurboGrafx16, PlatformNameStyle::Japan, _) => "PC Engine",
            (Self::TurboGrafx16, PlatformNameStyle::Europe, _) => "PC Engine",
            (Self::TurboGrafx16, PlatformNameStyle::NorthAmerica, _) => "TurboGrafx-16",

            (Self::SuperGrafx, PlatformNameStyle::Japan, _) => "PC Engine SuperGrafx",
            (Self::SuperGrafx, PlatformNameStyle::Europe, _) => "SuperGrafx",
            (Self::SuperGrafx, PlatformNameStyle::NorthAmerica, _) => "SuperGrafx",

            // Canonical follows IGDB-friendly naming, with optional region-specific fallback.
            (Self::Nes, PlatformNameStyle::Canonical, Region::Japan) => "Family Computer",
            (Self::Famicom, PlatformNameStyle::Canonical, Region::Usa) => "Nintendo Entertainment System",
            (Self::Famicom, PlatformNameStyle::Canonical, Region::Europe) => "Nintendo Entertainment System",
            (Self::FamicomDiskSystem, PlatformNameStyle::Canonical, Region::Usa) => "Nintendo Entertainment System",
            (Self::FamicomDiskSystem, PlatformNameStyle::Canonical, Region::Europe) => "Nintendo Entertainment System",
            (Self::Snes, PlatformNameStyle::Canonical, Region::Japan) => "Super Famicom",
            (Self::SuperFamicom, PlatformNameStyle::Canonical, Region::Usa) => "Super Nintendo Entertainment System",
            (Self::SuperFamicom, PlatformNameStyle::Canonical, Region::Europe) => "Super Nintendo Entertainment System",
            (Self::SegaGenesis, PlatformNameStyle::Canonical, Region::Usa) => "Genesis",
            (Self::SegaCd, PlatformNameStyle::Canonical, Region::Usa) => "Sega CD",
            _ => self.display_name(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaType {
    Cartridge,
    CdRom,
    Dvd,
    GdRom,
    HardDisk,
    FloppyDisk,
    Tape,
    Digital,
    Raw,
    Unknown,
}

impl Default for MediaType {
    fn default() -> Self {
        Self::Unknown
    }
}

impl fmt::Display for MediaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tag = match self {
            Self::Cartridge => "cartridge",
            Self::CdRom => "cdrom",
            Self::Dvd => "dvd",
            Self::GdRom => "gdrom",
            Self::HardDisk => "hard_disk",
            Self::FloppyDisk => "floppy_disk",
            Self::Tape => "tape",
            Self::Digital => "digital",
            Self::Raw => "raw",
            Self::Unknown => "unknown",
        };
        write!(f, "{tag}")
    }
}

impl MediaType {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Cartridge => "Cartridge",
            Self::CdRom => "CD-ROM",
            Self::Dvd => "DVD",
            Self::GdRom => "GD-ROM",
            Self::HardDisk => "Hard Disk",
            Self::FloppyDisk => "Floppy Disk",
            Self::Tape => "Tape",
            Self::Digital => "Digital",
            Self::Raw => "Raw",
            Self::Unknown => "N/A",
        }
    }
}


#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HashAlgorithms {
    Rahash,
    Crc32,
    Xxhash3_128,
    Md5,
    Sha1,
    Sha256,
}

pub(crate) fn record_from_report(path: String, size: u64, report: HashReport) -> FileHashRecord {
    FileHashRecord {
        path,
        size_bytes: Some(size),
        crc32: report.crc32,
        xxh3_128: report.xxh3_128,
        md5: report.md5,
        sha1: report.sha1,
        sha256: report.sha256,
        // RAHASH isn't computed over archive content -- see hash_archive_content's doc comment.
        rahash: None,
    }
}

/// One member's hash becomes the whole report (mirrored into the top-level fields, so
/// `get_crc()` etc. stay meaningful for a single-member archive). More than one member
/// means there's no single "whole file" hash to report at the top level once the
/// container's own compressed bytes aren't what got hashed -- only `content_hashes` is
/// populated then.
pub(crate) fn build_report_from_members(members: Vec<FileHashRecord>) -> HashReport {
    let mut report = HashReport::default();
    if let [only] = members.as_slice() {
        report.crc32 = only.crc32.clone();
        report.xxh3_128 = only.xxh3_128.clone();
        report.md5 = only.md5.clone();
        report.sha1 = only.sha1.clone();
        report.sha256 = only.sha256.clone();
    }
    report.content_hashes = members;
    report
}

#[derive(Debug, Clone)]
pub struct FileData {
    path: PathBuf,
    /// `None` for a plain (non-archive) file. `Some(format)` for one of the extensions
    /// `new()` recognizes. Hashing an archive's members is `rom::operation::Single`'s job.
    archive_format: Option<ArchiveFormat>,
    size: u64,
    created: Option<SystemTime>, // File creation time
    last_modified: Option<SystemTime>,
    last_integrity_check: Option<SystemTime>, // Scheduled check using fast hash(config set). No extract.
    integrity: Option<bool>, // Result of the last integrity check
    last_full_check: Option<SystemTime>, // Full check including extraction. (hash types config set. multiple ok)
    hash_report: Option<HashReport>,
}

impl FileData {
    pub fn new(file_path: PathBuf) -> Self {
        let archive_format = match file_path.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
            "zip" => Some(ArchiveFormat::Zip),
            "7z" => Some(ArchiveFormat::SevenZip),
            "chd" => Some(ArchiveFormat::Chd),
            "cso" => Some(ArchiveFormat::Cso),
            "pbp" => Some(ArchiveFormat::Pbp),
            "xiso" => Some(ArchiveFormat::Xiso),
            "rvz" => Some(ArchiveFormat::Rvz),
            "wia" => Some(ArchiveFormat::Wia),
            "gcz" => Some(ArchiveFormat::Gcz),
            _ => None,
        };
        let size = std::fs::metadata(&file_path).map(|m| m.len()).unwrap_or(0);
        let created = std::fs::metadata(&file_path).and_then(|m| m.created()).ok();
        let last_modified = std::fs::metadata(&file_path).and_then(|m| m.modified()).ok();
        Self {
            path: file_path.clone(),
            archive_format,
            size: size,
            created: created,
            last_modified: last_modified,
            last_integrity_check: None,
            integrity: None, 
            last_full_check: None,
            hash_report: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn archive_format(&self) -> Option<ArchiveFormat> {
        self.archive_format
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn last_full_check(&self) -> Option<SystemTime> {
        self.last_full_check
    }

    /// Records a hash report computed elsewhere over this file's content (e.g. tapped off
    /// the stream that wrote the file -- `operation::Single`), as a full check done now.
    pub fn set_hash_report(&mut self, report: HashReport) {
        self.hash_report = Some(report);
        self.last_full_check = Some(SystemTime::now());
    }

    pub fn get_hash_report(&self) -> Option<&HashReport> {
        self.hash_report.as_ref()
    }

    pub fn get_content_hashes(&self) -> Option<&Vec<FileHashRecord>> {
        self.hash_report.as_ref().map(|report| &report.content_hashes)
    }

    pub fn get_crc(&self) -> Option<&str> {
        self.hash_report.as_ref().and_then(|report| report.get(HashAlgorithms::Crc32))
    }

    pub fn get_xxhash(&self) -> Option<&str> {
        self.hash_report.as_ref().and_then(|report| report.get(HashAlgorithms::Xxhash3_128))
    }

    pub fn get_md5(&self) -> Option<&str> {
        self.hash_report.as_ref().and_then(|report| report.get(HashAlgorithms::Md5))
    }

    pub fn get_sha1(&self) -> Option<&str> {
        self.hash_report.as_ref().and_then(|report| report.get(HashAlgorithms::Sha1))
    }

    pub fn get_sha256(&self) -> Option<&str> {
        self.hash_report.as_ref().and_then(|report| report.get(HashAlgorithms::Sha256))
    }

    pub fn is_modified(&self) -> bool {
        let data_time: SystemTime = self.last_modified.unwrap_or(SystemTime::UNIX_EPOCH);
        let file_time: SystemTime = std::fs::metadata(&self.path).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
        if data_time == file_time {
            false
        } else {
            true
        }
    }
    
    /// Re-hashes the file and compares the result with the stored hash. When the check
    /// completes (a match or a mismatch), `integrity` is set to `Some(matched)` and
    /// `last_integrity_check` to the current time. On any error nothing is updated.
    ///
    /// The stored hash for `algo` is used. If there is none, another stored hash is used
    /// instead (see `HashReport::get_fallback`), unless `fallback` is `Some(false)`.
    pub fn integrity<F>(&mut self, fallback: Option<bool>, algo: HashAlgorithms, buf_size: Option<usize>, console_id: Option<u32>, on_progress: F) -> Result<bool>
    where
        F: FnMut(f32),
    {
        use std::io::{Error, ErrorKind};

        if self.last_full_check.is_none() {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("File was never hashed. Cannot perform integrity check. File: {}", self.path.display()),
            ));
        }
        let no_hash = || {
            Error::new(
                ErrorKind::NotFound,
                format!("File has no stored hash to compare against. File: {}", self.path.display()),
            )
        };
        let report = self.hash_report.as_ref().ok_or_else(no_hash)?;

        let (algo, stored) = match report.get(algo) {
            Some(stored) => (algo, stored),
            None if fallback.unwrap_or(true) => report.get_fallback().map_err(|_| no_hash())?,
            None => return Err(no_hash()),
        };

        let options = HashOptions { flags: HashFlags::algorithm_to_flag(algo), buf_size, console_id };
        let new_report = hash(&self.path, &options, on_progress)?; // an error returns here, before any update
        let matched = new_report.get(algo) == Some(stored);

        self.last_integrity_check = Some(SystemTime::now());
        self.integrity = Some(matched);
        Ok(matched)
    }
    pub fn update(&mut self, new_contents: Vec<FileHashRecord>) {
        self.hash_report.as_mut().map(|report| report.content_hashes = new_contents);
    }

}