//! How a game is meant to be shown: its display aspect, widescreen support, the resolutions it
//! draws (each progressive or interlaced, and on which output) and its refresh rate -- per
//! release, since a PAL release is 640x512i at 50 Hz where its NTSC one is 640x448i at 60, and
//! one region's release can add 16:9 that another lacks.
//!
//! Values the catalogue holds come from a source (`release_display.source`): the user, the
//! game's own files (a PS3 game's PARAM.SFO), MAME's machine list, PCSX2's widescreen
//! patches. Where none says anything, the value is the system's [`display_default`] -- shown
//! as a default, never stored. Beside it, the system's whole list ([`DisplayDefaults::system`])
//! says what else the hardware can draw.

use super::models::Platform;

/// The four things recorded, as `release_display.field` stores them. `modes` is the list of
/// resolutions (see [`normalize_resolutions`]).
pub const FIELDS: [&str; 4] = ["aspect", "widescreen", "modes", "refresh"];

/// Display aspects offered as presets. Any `W:H` is accepted. `8:7` is square pixels on a
/// 256-wide NES / SNES picture; `3:4` a vertical arcade monitor; the rest are handheld screens
/// (WonderSwan 14:9, Neo Geo Pocket 20:19, Virtual Boy 12:7, Lynx 80:51).
pub const ASPECTS: [&str; 15] = ["4:3", "8:7", "16:9", "3:4", "3:2", "10:9", "5:3", "16:10", "16:15", "10:7", "14:9", "20:19", "12:7", "80:51", "1:1"];

/// none | option (a menu or system setting) | native (16:9 only) | patch (a community patch
/// exists, e.g. PCSX2's).
pub const WIDESCREEN: [&str; 4] = ["none", "option", "native", "patch"];

/// The outputs a resolution can be on, in the order they are listed. A resolution without one
/// is on the TV (a home console's only output; a handheld's TV-out).
pub const OUTPUTS: [&str; 4] = ["screen", "top", "bottom", "gamepad"];

/// Refresh rates offered as presets, nominal Hz. Any whole number from 1 to 500 is accepted, and
/// several (`50,60`: a PAL release with a 60 Hz option).
pub const REFRESH: [&str; 7] = ["60", "50", "50,60", "56", "55", "75", "60,120"];

/// Which source's value counts when several have one, highest first.
pub const SOURCE_RANK: [&str; 6] = ["user", "sfo", "consolemods", "ipbin", "mame", "pcsx2"];

/// One resolution: `[output:]WIDTHxHEIGHT(p|i)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    /// `None` = the TV.
    pub output: Option<&'static str>,
    pub width: u32,
    pub height: u32,
    pub interlaced: bool,
}

impl Resolution {
    pub fn parse(s: &str) -> Result<Resolution, String> {
        let t = s.trim().to_ascii_lowercase().replace('×', "x");
        let (output, rest) = match t.split_once(':') {
            Some((o, r)) => {
                let o = o.trim();
                if o == "tv" {
                    (None, r.trim())
                } else {
                    (Some(*OUTPUTS.iter().find(|x| **x == o).ok_or_else(|| format!("{s:?}: the output is one of tv, {}", OUTPUTS.join(", ")))?), r.trim())
                }
            }
            None => (None, t.as_str()),
        };
        let bad = || format!("{s:?}: a resolution is WIDTHxHEIGHT and p or i, e.g. 256x224p or 640x480i");
        let interlaced = match rest.chars().last() {
            Some('p') => false,
            Some('i') => true,
            _ => return Err(bad()),
        };
        let (w, h) = rest[..rest.len() - 1].split_once('x').ok_or_else(bad)?;
        let (width, height): (u32, u32) = (w.trim().parse().map_err(|_| bad())?, h.trim().parse().map_err(|_| bad())?);
        if !(1..=8192).contains(&width) || !(1..=8192).contains(&height) {
            return Err(bad());
        }
        Ok(Resolution { output, width, height, interlaced })
    }

    fn order(&self) -> (usize, u32, u32, bool) {
        // Screens first, the TV last: a list's last entry is its highest TV mode.
        // ... and 640x480i before 640x480p.
        (self.output.and_then(|o| OUTPUTS.iter().position(|x| *x == o)).unwrap_or(OUTPUTS.len()), self.height, self.width, !self.interlaced)
    }
}

impl std::fmt::Display for Resolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(o) = self.output {
            write!(f, "{o}:")?;
        }
        write!(f, "{}x{}{}", self.width, self.height, if self.interlaced { 'i' } else { 'p' })
    }
}

/// A list of resolutions as `release_display` stores it: each one parsed, then sorted
/// (screens first, then by lines and width) and de-duplicated, comma-separated.
pub fn normalize_resolutions(value: &str) -> Result<String, String> {
    let mut list = value.split([',', ' ', ';']).map(str::trim).filter(|p| !p.is_empty()).map(Resolution::parse).collect::<Result<Vec<_>, _>>()?;
    if list.is_empty() {
        return Err("resolutions: at least one, e.g. 256x224p".to_string());
    }
    list.sort_by_key(Resolution::order);
    list.dedup();
    Ok(list.iter().map(Resolution::to_string).collect::<Vec<_>>().join(","))
}

/// A value as written for `field`, tidied, or why it isn't one.
pub fn normalize(field: &str, value: &str) -> Result<String, String> {
    let v = value.trim();
    match field {
        "aspect" => {
            let bad = || format!("aspect {v:?}: W:H, e.g. {}", ASPECTS[..4].join(", "));
            let (w, h) = v.split_once(':').ok_or_else(bad)?;
            let (w, h): (u32, u32) = (w.trim().parse().map_err(|_| bad())?, h.trim().parse().map_err(|_| bad())?);
            if !(1..=999).contains(&w) || !(1..=999).contains(&h) {
                return Err(bad());
            }
            Ok(format!("{w}:{h}"))
        }
        "widescreen" => WIDESCREEN.iter().find(|a| a.eq_ignore_ascii_case(v)).map(|a| a.to_string()).ok_or_else(|| format!("widescreen {v:?}: one of {}", WIDESCREEN.join(", "))),
        "modes" => normalize_resolutions(v),
        "refresh" => {
            let mut hz = Vec::new();
            for p in v.split([',', ' ', '/']).map(str::trim).filter(|p| !p.is_empty()) {
                let n = p.trim_end_matches(['h', 'H', 'z', 'Z']).trim();
                let x: f64 = n.parse().map_err(|_| format!("refresh {p:?}: Hz, e.g. 60 or 50,60"))?;
                let x = x.round() as u32;
                if !(1..=500).contains(&x) {
                    return Err(format!("refresh {p:?}: Hz, 1 to 500"));
                }
                hz.push(x);
            }
            if hz.is_empty() {
                return Err("refresh: at least one, e.g. 60".to_string());
            }
            hz.sort_unstable();
            hz.dedup();
            Ok(hz.iter().map(u32::to_string).collect::<Vec<_>>().join(","))
        }
        _ => Err(format!("unknown display field {field:?}")),
    }
}

/// A system's display, before anything is known about one game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayDefaults {
    /// `None` where games pick their own (a PC's).
    pub aspect: Option<&'static str>,
    pub widescreen: &'static str,
    /// The resolutions every game of the system draws in (its default); empty where they
    /// differ per game with no common one (a PC, an arcade board) or aren't documented.
    pub modes: Vec<&'static str>,
    /// Everything the hardware can draw for a game in this region, the default's among them:
    /// what the editor offers and the filter lists.
    pub system: Vec<&'static str>,
    /// Nominal Hz; `None` where games set their own (Lynx, Pokemon Mini) or it isn't known.
    pub refresh: Option<&'static str>,
    /// The documented exact rate of the default modes, e.g. `60.0988` -- shown, not stored.
    pub hz: Option<&'static str>,
}

impl DisplayDefaults {
    /// The default for `field`, as `release_display.value` would hold it; `None` where the
    /// system has nothing to say.
    pub fn value(&self, field: &str) -> Option<String> {
        match field {
            "aspect" => self.aspect.map(str::to_string),
            "widescreen" => Some(self.widescreen.to_string()),
            "modes" => (!self.modes.is_empty()).then(|| self.modes.join(",")),
            "refresh" => self.refresh.map(str::to_string),
            _ => None,
        }
    }
}

/// Region codes of an NTSC (or NTSC-J) market.
const NTSC_CODES: [&str; 11] = ["US", "JP", "KR", "CA", "BR", "TW", "HK", "ASIA", "USA", "JAPAN", "WORLD"];
const PAL_CODES: [&str; 19] = ["EU", "UK", "AU", "NZ", "DE", "FR", "ES", "IT", "NL", "SE", "DK", "NO", "FI", "PT", "GR", "PL", "RU", "SCANDINAVIA", "EUROPE"];

fn region_codes(region: &str) -> Vec<String> {
    region.split([',', ' ']).map(|c| c.trim().to_ascii_uppercase()).filter(|c| !c.is_empty()).collect()
}

/// Whether a release's regions make it a PAL one: a European / Australian release and no
/// NTSC region beside it (a "USA, Europe" disc is shown as its NTSC self).
pub fn is_pal(region: &str) -> bool {
    let codes = region_codes(region);
    codes.iter().any(|c| PAL_CODES.contains(&c.as_str())) && !codes.iter().any(|c| NTSC_CODES.contains(&c.as_str()))
}

/// Whether a release names an NTSC region.
pub fn is_ntsc(region: &str) -> bool {
    region_codes(region).iter().any(|c| NTSC_CODES.contains(&c.as_str()))
}

/// One region's modes of a system.
#[derive(Clone, Copy)]
struct Modes {
    common: &'static [&'static str],
    /// Beyond `common`.
    more: &'static [&'static str],
    refresh: Option<&'static str>,
    hz: Option<&'static str>,
}

const fn m(common: &'static [&'static str], more: &'static [&'static str], refresh: &'static str, hz: Option<&'static str>) -> Modes {
    Modes { common, more, refresh: Some(refresh), hz }
}

const NONE: Modes = Modes { common: &[], more: &[], refresh: None, hz: None };

struct System {
    aspect: Option<&'static str>,
    widescreen: &'static str,
    ntsc: Modes,
    /// `None`: the same in every region (a handheld, an NTSC-only or HD system).
    pal: Option<Modes>,
    /// Sold mostly in PAL markets: a release that names no region is a PAL one.
    pal_market: bool,
}

const fn sys(aspect: &'static str, ntsc: Modes, pal: Option<Modes>) -> System {
    System { aspect: Some(aspect), widescreen: "none", ntsc, pal, pal_market: false }
}

const fn euro(aspect: &'static str, ntsc: Modes, pal: Modes) -> System {
    System { aspect: Some(aspect), widescreen: "none", ntsc, pal: Some(pal), pal_market: true }
}

const fn wide(ntsc: Modes, pal: Option<Modes>) -> System {
    System { aspect: Some("16:9"), widescreen: "native", ntsc, pal, pal_market: false }
}

// The modes below are from each system's developer documentation; the URL per row. Widths
// and lines are the picture the system draws (SNES 256x224, not "240p"); `i` where it is
// interlaced. HD systems list their output modes.

/// NES picture: always 256x240 (NTSC TVs show about 224 of it); pixel aspect 8:7.
/// https://www.nesdev.org/wiki/Overscan , https://www.nesdev.org/wiki/Cycle_reference_chart
const NES: System = sys("4:3", m(&["256x240p"], &[], "60", Some("60.0988")), Some(m(&["256x240p"], &[], "50", Some("50.0070"))));

/// SNES: SETINI picks 224 or 239 lines and interlace; BG modes 5/6 and pseudo-hires 512 wide.
/// https://snes.nesdev.org/wiki/PPU_registers , https://snes.nesdev.org/wiki/Timing , https://problemkaputt.de/fullsnes.htm
const SNES_MORE: &[&str] = &["256x239p", "512x224p", "512x239p", "256x448i", "256x478i", "512x448i", "512x478i"];
const SNES: System = sys("4:3", m(&["256x224p"], SNES_MORE, "60", Some("60.0988")), Some(m(&["256x224p"], SNES_MORE, "50", Some("50.0070"))));

/// N64 VI: any width (320 and 640 common), 240 lines or 480 interlaced; PAL's virtual
/// picture is 640x576. https://n64brew.dev/wiki/Video_Interface , libdragon display.h
const N64: System = sys(
    "4:3",
    m(&["320x240p"], &["256x240p", "512x240p", "640x240p", "512x480i", "640x480i"], "60", Some("59.826")),
    Some(m(&["320x240p"], &["256x288p", "320x288p", "512x288p", "640x288p", "640x480i", "512x576i", "640x576i"], "50", Some("49.92"))),
);

/// GameCube: 640x480i (PAL 576i); 240p / 288p double-strike; 480p per game (B at boot).
/// https://raw.githubusercontent.com/devkitPro/libogc/master/libogc/video.c , https://www.gc-forever.com/yagcd/chap5.html
const GAMECUBE: System = sys("4:3", m(&["640x480i"], &["640x240p", "640x480p"], "60", None), Some(m(&["640x576i"], &["640x288p", "640x480i", "640x480p"], "50", None)));

/// Wii: 480p in every game and region; a PAL Wii also 576i and 60 Hz.
/// https://www.wiibrew.org/wiki/Video_output
const WII: System = System {
    aspect: Some("4:3"),
    widescreen: "option",
    ntsc: m(&["640x480i", "640x480p"], &[], "60", None),
    pal: Some(m(&["640x480i", "640x480p", "640x576i"], &[], "50,60", None)),
    pal_market: false,
};

/// Wii U: the TV's modes plus the GamePad's 854x480. https://wiiubrew.org/wiki/Hardware
const WII_U: System = wide(
    m(&["gamepad:854x480p", "720x480i", "720x480p", "1280x720p", "1920x1080i", "1920x1080p"], &[], "60", None),
    Some(m(&["gamepad:854x480p", "720x480i", "720x480p", "720x576i", "1280x720p", "1920x1080i", "1920x1080p"], &[], "50,60", None)),
);

/// Switch: a 1280x720 screen; docked 480p / 720p / 1080p (not the Lite).
/// https://www.nintendo.com/us/gaming-systems/switch/tech-specs/
const SWITCH: System = wide(m(&["screen:1280x720p", "720x480p", "1280x720p", "1920x1080p"], &[], "60", None), None);

/// Switch 2: a 1920x1080 screen (VRR up to 120 Hz); docked up to 2160p, 120 Hz at 1080p / 1440p.
/// https://www.nintendo.com/us/gaming-systems/switch-2/tech-specs/
const SWITCH_2: System = wide(m(&["screen:1920x1080p", "1280x720p", "1920x1080p", "2560x1440p", "3840x2160p"], &[], "60", None), None);

/// PS1 GPU: 256 / 320 / 368 / 512 / 640 wide, 240 lines or 480 interlaced; PAL to 288 / 576.
/// https://psx-spx.consoledev.net/graphicsprocessingunitgpu/
const PS1: System = sys(
    "4:3",
    m(&["320x240p"], &["256x240p", "368x240p", "512x240p", "640x240p", "512x480i", "640x480i"], "60", Some("59.826")),
    Some(m(
        &["320x256p"],
        &["256x256p", "368x256p", "512x256p", "640x256p", "256x288p", "320x288p", "368x288p", "512x288p", "640x288p", "512x512i", "640x512i", "512x576i", "640x576i"],
        "50",
        Some("49.761"),
    )),
);

/// PS2 GS: NTSC 256x224 to 640x448, PAL 256x256 to 640x512; 480p / 720p / 1080i per game.
/// https://raw.githubusercontent.com/ps2dev/ps2sdk/master/ee/graph/include/graph.h
const PS2: System = sys(
    "4:3",
    m(&["640x448i"], &["512x224p", "640x224p", "512x448i", "720x480p", "1280x720p", "1920x1080i"], "60", None),
    Some(m(&["640x512i"], &["512x256p", "640x256p", "512x512i", "640x448i", "720x480p", "1920x1080i"], "50", None)),
);

/// PS3 output modes; a game's own are in its PARAM.SFO (RESOLUTION).
/// https://raw.githubusercontent.com/RPCS3/rpcs3/master/rpcs3/Emu/Cell/Modules/cellVideoOut.h
const PS3: System = wide(
    m(&["1280x720p"], &["720x480i", "720x480p", "1920x1080i", "1920x1080p"], "60", None),
    Some(m(&["1280x720p"], &["720x576i", "720x576p", "720x480p", "1920x1080i", "1920x1080p"], "50,60", None)),
);

/// PS4: 480p / 720p / 1080i / 1080p; the Pro 2160p.
/// https://blog.playstation.com/2013/10/30/ps4-the-ultimate-faq-north-america/
const PS4: System = wide(m(&["720x480p", "1280x720p", "1920x1080i", "1920x1080p"], &["3840x2160p"], "60", None), None);

/// PS5: 720p to 2160p, 1440p; 120 Hz per game.
/// https://www.playstation.com/en-us/support/hardware/ps5-4k-resolution-guide/
const PS5: System = wide(m(&["1280x720p", "1920x1080i", "1920x1080p", "2560x1440p", "3840x2160p"], &["7680x4320p"], "60", None), None);

/// PSP: a 480x272 screen; TV-out 480p over component (PSP-2000), 480i composite (PSP-3000).
/// https://manuals.playstation.net/document/en/psp/current/settings/aboutvo.html
const PSP: System = wide(m(&["screen:480x272p"], &["720x480i", "720x480p"], "60", None), None);

/// Vita: a 960x544 screen; PlayStation TV 480p / 720p / 1080i.
/// https://sonyinteractive.com/corporate/release/2013/130909c.html
const VITA: System = wide(m(&["screen:960x544p"], &["720x480p", "1280x720p", "1920x1080i"], "60", None), None);

/// Xbox: 480i; 480p / 720p / 1080i per game. https://xboxdevwiki.net/EEPROM
const XBOX: System = sys(
    "4:3",
    m(&["640x480i"], &["640x480p", "1280x720p", "1920x1080i"], "60", None),
    Some(m(&["640x576i"], &["640x480i", "640x480p", "1280x720p", "1920x1080i"], "50", None)),
);

const XBOX_360: System = wide(
    m(&["640x480i", "720x480p", "1280x720p", "1920x1080i", "1920x1080p"], &[], "60", None),
    Some(m(&["720x576i", "640x480i", "720x480p", "1280x720p", "1920x1080i", "1920x1080p"], &[], "50,60", None)),
);
const XBOX_ONE: System = wide(m(&["720x480p", "1280x720p", "1920x1080i", "1920x1080p"], &["2560x1440p", "3840x2160p"], "60", None), None);
/// https://www.xbox.com/en-US/consoles/xbox-series-x
const XBOX_SERIES: System = wide(m(&["1280x720p", "1920x1080p", "2560x1440p", "3840x2160p"], &["7680x4320p"], "60", None), None);

/// TMS9918 (SG-1000, ColecoVision, MSX1, TI-99/4A): 256x192, text 240x192.
/// https://www.msx.org/wiki/Screen_Modes_Description
const TMS9918: System = sys("4:3", m(&["256x192p"], &["240x192p"], "60", Some("59.92")), Some(m(&["256x192p"], &["240x192p"], "50", Some("50.16"))));

/// Master System: 256x192; 224 lines on the SMS2 VDP, 240 on a PAL one only.
/// https://www.smspower.org/Development/VDPRegisters
const SMS: System = sys("4:3", m(&["256x192p"], &["256x224p"], "60", Some("59.92")), Some(m(&["256x192p"], &["256x224p", "256x240p"], "50", None)));

/// Mega Drive: H32 / H40 by V28, V30 on PAL only; interlace mode 2 doubles the lines.
/// https://plutiedev.com/vdp-registers
const MEGA_DRIVE: System = sys(
    "4:3",
    m(&["320x224p"], &["256x224p", "256x448i", "320x448i"], "60", Some("59.92")),
    Some(m(&["320x224p"], &["256x224p", "256x240p", "320x240p", "256x448i", "320x448i", "256x480i", "320x480i"], "50", None)),
);

/// 32X: 320x224, PAL 320x240. https://github-wiki-see.page/m/viciious/32XDK/wiki
const SEGA_32X: System = sys("4:3", m(&["320x224p"], &["256x224p"], "60", None), Some(m(&["320x224p"], &["256x224p", "320x240p"], "50", None)));

/// Saturn VDP2: 320 / 352 / 640 / 704 wide, 224 / 240 lines or 448 / 480 interlaced; PAL 256 / 512.
/// https://www.infochunk.com/saturn/segahtml_en/hard/vdp2/hon/p02_10.htm
const SATURN: System = sys(
    "4:3",
    m(
        &["320x224p"],
        &["320x240p", "352x224p", "352x240p", "640x224p", "640x240p", "704x224p", "704x240p", "320x448i", "320x480i", "352x448i", "352x480i", "640x448i", "640x480i", "704x448i", "704x480i"],
        "60",
        None,
    ),
    Some(m(
        &["320x224p"],
        &[
            "320x240p", "320x256p", "352x224p", "352x240p", "352x256p", "640x224p", "640x240p", "640x256p", "704x224p", "704x240p", "704x256p", "320x448i", "320x480i", "320x512i", "352x448i", "352x480i",
            "352x512i", "640x448i", "640x480i", "640x512i", "704x448i", "704x480i", "704x512i",
        ],
        "50",
        None,
    )),
);

/// Dreamcast: 640x480i on a TV, 640x480p on VGA (a game's IP.BIN says if it allows it).
/// https://raw.githubusercontent.com/KallistiOS/KallistiOS/master/kernel/arch/dreamcast/hardware/video.c , http://mc.pp.se/dc/ip0000.bin.html
const DREAMCAST: System = sys("4:3", m(&["640x480i"], &["320x240p", "640x480p"], "60", None), Some(m(&["640x480i"], &["320x240p", "640x480p"], "50", None)));

/// PC Engine VCE: 256 / 336 / 352 / 512 wide by dot clock; about 90% of games 256x239.
/// https://pineight.com/mw/page/Dot_clock_rates.xhtml
const PC_ENGINE: System = sys("4:3", m(&["256x239p"], &["256x224p", "336x224p", "336x239p", "352x224p", "352x239p", "512x224p", "512x239p"], "60", None), None);

/// PC-FX: 256x240 and 341x240. https://allstar.jhuapl.edu/repo/p4/amd64/mednafen/doc/pcfx.html
const PC_FX: System = sys("4:3", m(&["256x240p"], &["341x240p"], "60", None), None);

/// Neo Geo: 320x224 (PAL draws 256 lines); MVS 59.1856 Hz, AES 59.599.
/// https://wiki.neogeodev.org/index.php?title=Display_timing , https://wiki.neogeodev.org/index.php?title=Framerate
const NEO_GEO_AES: System = sys("4:3", m(&["320x224p"], &[], "60", Some("59.599")), Some(m(&["320x224p"], &["320x256p"], "50", Some("50.429"))));
const NEO_GEO_MVS: System = sys("4:3", m(&["320x224p"], &[], "59", Some("59.1856")), None);

/// 3DO: 320x240 (PAL 384x288), interpolated to 480i / 576i. https://www.retrorgb.com/?p=1348
const THREE_DO: System = sys("4:3", m(&["320x240p"], &["640x480i"], "60", None), Some(m(&["384x288p"], &["768x576i"], "50", None)));

/// CD-i: 384x240 (PAL 384x280); double resolution 768 wide, interlaced 480 / 560.
/// https://icdia.co.uk/faq/cdifaq1.html
const CDI: System = sys("4:3", m(&["384x240p"], &["768x240p", "768x480i"], "60", None), Some(m(&["384x280p"], &["768x280p", "768x560i"], "50", None)));

/// Atari 2600: 160 wide, 192 lines NTSC, 228 PAL. https://alienbill.com/2600/101/docs/stella.html
const ATARI_2600: System = sys("4:3", m(&["160x192p"], &[], "60", None), Some(m(&["160x228p"], &[], "50", None)));
const ATARI_5200: System = sys("4:3", m(&["160x192p"], &["320x192p"], "60", None), None);
/// 7800 MARIA: 160 / 320 wide, 192 lines NTSC, 240 PAL. https://7800.8bitdev.org/index.php/7800_Software_Guide
const ATARI_7800: System = sys("4:3", m(&["160x192p"], &["320x192p"], "60", None), Some(m(&["160x240p"], &["320x240p"], "50", None)));

/// Amiga (and CD32, CDTV): lores / hires, interlace doubling the lines.
/// Amiga Hardware Reference Manual, Libraries Manual node0316.
const AMIGA: System = euro(
    "4:3",
    m(&["320x200p"], &["640x200p", "320x400i", "640x400i"], "60", None),
    m(&["320x256p"], &["640x256p", "320x512i", "640x512i"], "50", None),
);

/// MSX2 / 2+ / turbo R (V9938 / V9958): 256 / 512 wide, 192 / 212 lines, interlace doubled.
/// https://map.grauw.nl/resources/video/v9938/v9938.xhtml
const MSX2: System = sys(
    "4:3",
    m(&["256x212p"], &["256x192p", "512x192p", "512x212p", "256x424i", "512x424i"], "60", Some("59.92")),
    Some(m(&["256x212p"], &["256x192p", "512x192p", "512x212p", "256x424i", "512x424i"], "50", Some("50.16"))),
);

/// C64 VIC-II: 320x200 hires, 160x200 multicolour. https://www.zimmers.net/cbmpics/cbm/c64/vic-ii.txt
const C64: System = euro("4:3", m(&["320x200p"], &["160x200p"], "60", Some("59.826")), m(&["320x200p"], &["160x200p"], "50", Some("50.125")));
/// C16 / Plus/4 TED. https://en.wikipedia.org/wiki/MOS_Technology_TED
const C16: System = euro("4:3", m(&["320x200p"], &["160x200p"], "60", None), m(&["320x200p"], &["160x200p"], "50", None));
/// VIC-20: 22x23 characters, 176x184. https://en.wikipedia.org/wiki/MOS_Technology_VIC
const VIC20: System = euro("4:3", m(&["176x184p"], &[], "60", None), m(&["176x184p"], &[], "50", None));
/// Atari 8-bit ANTIC / GTIA. https://atarimagazines.com/v3n5/allmodes.php
const ATARI_8BIT: System = sys("4:3", m(&["160x192p"], &["320x192p"], "60", Some("59.94")), Some(m(&["160x192p"], &["320x192p"], "50", Some("49.86"))));
/// Atari ST: 320x200, 640x200, 640x400 mono (71 Hz). https://atari.org/hosted/quickfaq/stfaq_1.htm
const ATARI_ST: System = euro("4:3", m(&["320x200p"], &["640x200p", "640x400p"], "60", None), m(&["320x200p"], &["640x200p", "640x400p"], "50", None));

const fn pal_only(common: &'static [&'static str], more: &'static [&'static str], hz: Option<&'static str>) -> System {
    euro("4:3", m(common, more, "50", hz), m(common, more, "50", hz))
}

/// https://en.wikipedia.org/wiki/ZX_Spectrum_graphic_modes
const ZX_SPECTRUM: System = pal_only(&["256x192p"], &[], Some("50.08"));
const ZX81: System = pal_only(&["256x192p"], &[], None);
/// Amstrad CPC modes 0 / 1 / 2. https://oldwiki.cpcwiki.eu/index.php/Programming:Overscan
const CPC: System = pal_only(&["320x200p"], &["160x200p", "640x200p"], None);
/// BBC Micro / Electron: modes 0-6 on a 256-line raster. https://8bs.com/hints/073.txt
const BBC: System = pal_only(&["320x256p"], &["160x256p", "640x256p"], None);
/// Archimedes: MODE 13 320x256 for games; 640x256, 768x288, 640x512, VGA 640x480.
/// https://arcwiki.org.uk/index.php/Monitor_Types
const ARCHIMEDES: System = pal_only(&["320x256p"], &["640x256p", "768x288p", "640x512p", "640x480p"], None);
const SAM_COUPE: System = pal_only(&["256x192p"], &["512x192p"], None);
const ENTERPRISE: System = pal_only(&["320x256p"], &["640x256p", "640x512i"], None);
/// Oric: HIRES 240x200 plus 3 text lines. https://files.bedroomlan.org/ORIC/DOC/ULA/ula.html
const ORIC: System = pal_only(&["240x224p"], &[], None);
const THOMSON: System = pal_only(&["320x200p"], &["160x200p", "640x200p"], None);
const DRAGON: System = pal_only(&["256x192p"], &["128x192p"], None);

/// The display of `platform` for a release from `region` (`release.region`).
pub fn display_default(platform: Platform, region: &str) -> DisplayDefaults {
    use Platform as P;
    let handheld = |screen: &'static [&'static str], more: &'static [&'static str], aspect: &'static str, refresh: Option<&'static str>, hz: Option<&'static str>| System {
        aspect: Some(aspect),
        widescreen: "none",
        ntsc: Modes { common: screen, more, refresh, hz },
        pal: None,
        pal_market: false,
    };
    let none = |aspect: Option<&'static str>, ntsc: &'static str, pal: Option<&'static str>| System {
        aspect,
        widescreen: "none",
        ntsc: Modes { refresh: Some(ntsc), ..NONE },
        pal: pal.map(|p| Modes { refresh: Some(p), ..NONE }),
        pal_market: false,
    };
    let s: System = match platform {
        P::Nes | P::Famicom | P::FamicomDiskSystem => NES,
        P::Snes | P::SuperFamicom | P::SufamiTurbo => SNES,
        P::Nintendo64 | P::Nintendo64Dd => N64,
        P::GameCube => GAMECUBE,
        P::Wii => WII,
        P::WiiU => WII_U,
        P::NintendoSwitch => SWITCH,
        P::NintendoSwitch2 => SWITCH_2,
        // https://www.copetti.org/writings/consoles/virtual-boy/
        P::VirtualBoy => handheld(&["screen:384x224p"], &[], "12:7", Some("50"), None),
        // https://gbdev.io/pandocs/Specifications.html
        P::GameBoy | P::GameBoyColor => handheld(&["screen:160x144p"], &[], "10:9", Some("60"), Some("59.7275")),
        // https://problemkaputt.de/gbatek-lcd-dimensions-and-timings.htm
        P::GameBoyAdvance => handheld(&["screen:240x160p"], &[], "3:2", Some("60"), Some("59.737")),
        // https://www.pokemon-mini.net/documentation/prc/ -- frame rate set per game (72 Hz / n).
        P::PokemonMini => handheld(&["screen:96x64p"], &[], "3:2", None, None),
        // https://problemkaputt.de/gbatek-ds-video-stuff.htm
        P::NintendoDs | P::NintendoDsi | P::DsiWare => handheld(&["top:256x192p", "bottom:256x192p"], &[], "4:3", Some("60"), Some("59.8261")),
        // https://www.3dbrew.org/wiki/Hardware -- 800x240 is the wide 2D mode (and both eyes in 3D).
        P::Nintendo3ds | P::NewNintendo3ds => handheld(&["top:400x240p", "bottom:320x240p"], &["top:800x240p"], "5:3", Some("60"), Some("59.83")),
        P::PlayStation => PS1,
        P::PlayStation2 => PS2,
        P::PlayStation3 => PS3,
        P::PlayStation4 => PS4,
        P::PlayStation5 => PS5,
        P::PlayStationPortable | P::PspPs1Classic => PSP,
        P::PlayStationVita => VITA,
        // https://psx-spx.consoledev.net/pocketstation/
        P::PocketStation => handheld(&["screen:32x32p"], &[], "1:1", None, None),
        P::Xbox => XBOX,
        P::Xbox360 => XBOX_360,
        P::XboxOne => XBOX_ONE,
        P::XboxSeries => XBOX_SERIES,
        P::Sg1000 | P::ColecoVision | P::Msx | P::Ti99 => TMS9918,
        P::SegaMasterSystem => SMS,
        // https://www.smspower.org/uploads/Development/msvdp-20021112.txt -- 60 Hz in every region.
        P::SegaGameGear => handheld(&["screen:160x144p"], &[], "10:9", Some("60"), None),
        P::SegaGenesis | P::SegaCd | P::SegaPico => MEGA_DRIVE,
        P::Sega32x => SEGA_32X,
        P::SegaSaturn => SATURN,
        P::Dreamcast => DREAMCAST,
        P::TurboGrafx16 | P::SuperGrafx | P::TurboGrafxCd => PC_ENGINE,
        P::PcFx => PC_FX,
        P::NeoGeoAes | P::NeoGeoCd => NEO_GEO_AES,
        P::NeoGeoMvs => NEO_GEO_MVS,
        // MAME ngp.cpp: 6.144 MHz, 515 x 199.
        P::NeoGeoPocket | P::NeoGeoPocketColor => handheld(&["screen:160x152p"], &[], "20:19", Some("60"), Some("59.95")),
        // https://ws.nesdev.org/wiki/Display , https://ws.nesdev.org/wiki/Timing
        P::WonderSwan | P::WonderSwanColor => handheld(&["screen:224x144p"], &[], "14:9", Some("75"), Some("75.47")),
        P::ThreeDo => THREE_DO,
        P::PhilipsCdi => CDI,
        P::Atari2600 => ATARI_2600,
        P::Atari5200 => ATARI_5200,
        P::Atari7800 => ATARI_7800,
        // https://atarilynxvault.com/pages/atari-lynx-hardware-notes -- refresh set per game (50-75 Hz).
        P::AtariLynx => handheld(&["screen:160x102p"], &[], "80:51", None, None),
        // https://www.mulle-kybernetik.com/jagdox/video.html -- modes set per game, none documented as common.
        P::AtariJaguar => none(Some("4:3"), "60", Some("50")),
        // https://wiki.intellivision.us/index.php/STIC
        P::Intellivision => sys("4:3", m(&["159x96p"], &[], "60", None), None),
        // MAME i8244.cpp: object-based, no documented picture size.
        P::Odyssey2 => none(Some("4:3"), "60", Some("50")),
        // https://en.wikipedia.org/wiki/Bally_Astrocade
        P::Astrocade => sys("4:3", m(&["160x102p"], &[], "60", None), None),
        // A vector screen, upright; 50 Hz unless a game sets its own.
        P::Vectrex => none(Some("3:4"), "50", None),
        P::Amiga | P::AmigaCd32 | P::CommodoreCdtv => AMIGA,
        P::Msx2 | P::Msx2Plus | P::MsxTurbo => MSX2,
        P::Commodore64 => C64,
        P::Commodore16 => C16,
        P::Vic20 => VIC20,
        // Text only: 40 or 80 columns of 25 lines.
        P::CommodorePet => sys("4:3", m(&["320x200p"], &["640x200p"], "60", None), None),
        P::Atari8Bit => ATARI_8BIT,
        P::AtariStSte => ATARI_ST,
        P::ZxSpectrum => ZX_SPECTRUM,
        P::Zx81 => ZX81,
        P::AmstradCpc => CPC,
        P::BbcMicro | P::AcornElectron => BBC,
        P::AcornArchimedes => ARCHIMEDES,
        P::SamCoupe => SAM_COUPE,
        P::Enterprise => ENTERPRISE,
        P::Oric => ORIC,
        P::ThomsonMoTo => THOMSON,
        P::Dragon32 => DRAGON,
        // https://en.wikipedia.org/wiki/Apple_II_graphics
        P::AppleII => sys("4:3", m(&["280x192p"], &["560x192p"], "60", Some("59.92")), None),
        P::AppleIIgs => sys("4:3", m(&["320x200p"], &["640x200p", "280x192p", "560x192p"], "60", Some("59.92")), None),
        // https://www.trs-80.org/high-resolution-graphics-for-the-trs-80.html
        P::Trs80 => sys("4:3", m(&["384x192p"], &["512x192p", "640x240p"], "60", None), None),
        P::Trs80CoCo => sys("4:3", m(&["256x192p"], &["128x192p"], "60", None), None),
        // https://en.wikipedia.org/wiki/PC-8800_series -- 640x400 on a 24 kHz monitor.
        P::Pc8801 => sys("4:3", m(&["640x200p"], &["640x400p"], "60", None), None),
        // https://jp.sharp/products/llt2015/text/taiou.html -- 24.8 kHz, 56.4 Hz.
        P::Pc98 => sys("4:3", m(&["640x400p"], &["640x480p"], "56", Some("56.4")), None),
        P::Pc6000Series => sys("4:3", m(&["256x192p"], &["320x200p"], "60", None), None),
        P::Fm7 => sys("4:3", m(&["640x200p"], &["320x200p", "640x400p"], "60", None), None),
        // https://en.wikipedia.org/wiki/FM_Towns -- 15 / 24 / 31 kHz modes per game, no common one.
        P::FmTowns => System { aspect: Some("4:3"), widescreen: "none", ntsc: Modes { common: &[], more: &["256x240p", "320x240p", "640x400p", "640x480p"], refresh: None, hz: None }, pal: None, pal_market: false },
        // MAME x68k_crtc.cpp: 31 kHz 55.46 Hz; 15 kHz 512x512 interlaced.
        P::SharpX68000 => sys("4:3", m(&["512x512p"], &["256x256p", "512x256p", "768x512p", "512x512i"], "55", Some("55.46")), None),
        // CGA / EGA / VGA: each game picks its mode. https://en.wikipedia.org/wiki/Video_Graphics_Array
        P::PcDos => System {
            aspect: Some("4:3"),
            widescreen: "none",
            ntsc: Modes { common: &[], more: &["320x200p", "640x200p", "640x350p", "320x240p", "640x400p", "640x480p"], refresh: None, hz: None },
            pal: None,
            pal_market: false,
        },
        // Per game, from MAME's machine list (`display_data`).
        P::Arcade => System { aspect: Some("4:3"), widescreen: "none", ntsc: NONE, pal: None, pal_market: false },
        // Games pick their own; no system default.
        _ => System { aspect: None, widescreen: "none", ntsc: NONE, pal: None, pal_market: false },
    };
    let pal = is_pal(region) || (s.pal_market && !is_ntsc(region));
    let modes = match (pal, s.pal) {
        (true, Some(p)) => p,
        _ => s.ntsc,
    };
    // In the order `normalize_resolutions` keeps, so a default reads as a stored value would.
    let sorted = |list: Vec<&'static str>| {
        let mut list = list;
        list.sort_by_key(|r| Resolution::parse(r).map(|p| p.order()).unwrap_or_default());
        list.dedup();
        list
    };
    let system = sorted(modes.common.iter().chain(modes.more).copied().collect());
    DisplayDefaults { aspect: s.aspect, widescreen: s.widescreen, modes: sorted(modes.common.to_vec()), system, refresh: modes.refresh, hz: modes.hz }
}

/// Which aspect a release page shows its screenshot at beside 4:3, for the user to choose:
/// `8:7` for NES / SNES (square pixels on a 256-wide picture, or the TV's 4:3 -- the games
/// differ and no list says which), `square` for systems whose widths vary by game (PC
/// Engine 256 / 336 / 512, Mega Drive H32 / H40, Saturn, Neo Geo): the screenshot's own
/// width:height.
pub fn aspect_comparison(platform: Platform) -> Option<&'static str> {
    use Platform as P;
    match platform {
        P::Nes | P::Famicom | P::FamicomDiskSystem | P::Snes | P::SuperFamicom | P::SufamiTurbo => Some("8:7"),
        P::TurboGrafx16 | P::SuperGrafx | P::TurboGrafxCd | P::PcFx | P::SegaGenesis | P::SegaCd | P::Sega32x | P::SegaPico | P::SegaSaturn | P::NeoGeoAes | P::NeoGeoMvs | P::NeoGeoCd => Some("square"),
        _ => None,
    }
}

/// A PS3 game's PARAM.SFO `RESOLUTION` flags as its resolutions and whether it offers 16:9:
/// 1 = 480, 2 = 576, 4 = 720, 8 = 1080, 16 = 480 at 16:9, 32 = 576 at 16:9. 1080 is either
/// scan: the console's setting picks 1080i or 1080p (cellVideoOut keeps scan apart).
/// https://raw.githubusercontent.com/RPCS3/rpcs3/master/rpcs3/Loader/PSF.h
pub fn ps3_resolution(flags: u32) -> (Vec<&'static str>, bool) {
    let mut modes = Vec::new();
    if flags & (1 | 16) != 0 {
        modes.push("720x480p");
    }
    if flags & (2 | 32) != 0 {
        modes.push("720x576p");
    }
    if flags & 4 != 0 {
        modes.push("1280x720p");
    }
    if flags & 8 != 0 {
        modes.push("1920x1080i");
        modes.push("1920x1080p");
    }
    let widescreen = flags & (16 | 32 | 4 | 8) != 0;
    (modes, widescreen)
}

/// The number stored under `key` in a PARAM.SFO (an int32 entry), if there is one.
pub fn sfo_u32(bytes: &[u8], key: &str) -> Option<u32> {
    if bytes.get(0..4)? != [0x00, b'P', b'S', b'F'] {
        return None;
    }
    let u32_at = |o: usize| bytes.get(o..o + 4).map(|r| u32::from_le_bytes([r[0], r[1], r[2], r[3]]));
    let u16_at = |o: usize| bytes.get(o..o + 2).map(|r| u16::from_le_bytes([r[0], r[1]]));
    let key_table = u32_at(8)? as usize;
    let data_table = u32_at(12)? as usize;
    let count = u32_at(16)? as usize;
    if count > 256 {
        return None;
    }
    for i in 0..count {
        let base = 0x14 + i * 0x10;
        let key_offset = u16_at(base)? as usize;
        let fmt = u16_at(base + 2)?;
        let data_offset = u32_at(base + 12)? as usize;
        let name_bytes = bytes.get(key_table + key_offset..)?;
        let name = name_bytes.split(|&b| b == 0).next()?;
        if name == key.as_bytes() && fmt == 0x0404 {
            return u32_at(data_table + data_offset);
        }
    }
    None
}

/// A serial reduced to letters and digits, upper case: "SLUS-21323", "SLUS 21323" and
/// "SLUS_213.23" are one.
pub fn serial_key(serial: &str) -> String {
    serial.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect()
}

/// A MAME machine's display: vertical (rotated 90 / 270) is 3:4, else 4:3.
pub fn mame_aspect(rotate: u32) -> &'static str {
    if rotate % 180 == 90 {
        "3:4"
    } else {
        "4:3"
    }
}

/// A MAME display's refresh rate as `release_display` keeps it: nominal Hz.
pub fn mame_refresh(hz: f64) -> Option<String> {
    let n = hz.round();
    (1.0..=500.0).contains(&n).then(|| (n as u32).to_string())
}

/// A MAME display's resolution: its visible width and height, interlaced where a 15 kHz
/// board (pixel clock / horizontal total) draws more lines than a field holds. MAME's
/// `<display>` has no interlace attribute, so that is inferred.
/// https://github.com/mamedev/mame/blob/master/src/frontend/mame/infoxml.cpp
pub fn mame_resolution(width: u32, height: u32, pixclock: Option<f64>, htotal: Option<u32>) -> Option<String> {
    if width == 0 || height == 0 {
        return None;
    }
    let line_khz = match (pixclock, htotal) {
        (Some(p), Some(h)) if h > 0 => Some(p / h as f64 / 1000.0),
        _ => None,
    };
    let interlaced = height > 300 && line_khz.is_some_and(|k| k < 20.0);
    Some(format!("{width}x{height}{}", if interlaced { 'i' } else { 'p' }))
}

/// A Dreamcast disc's IP.BIN header (from its "SEGA SEGAKATANA" hardware id): whether the game
/// allows the VGA box (480p). The peripherals field at 0x38 is seven hex digits; VGA is the
/// low bit of the sixth (offset 0x3D). http://mc.pp.se/dc/ip0000.bin.html
pub fn dreamcast_vga(header: &[u8]) -> Option<bool> {
    if header.get(0..15)? != b"SEGA SEGAKATANA" {
        return None;
    }
    let digit = char::from(*header.get(0x3D)?).to_digit(16)?;
    Some(digit & 1 == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_checked_and_tidied() {
        assert_eq!(normalize("aspect", "8:7").unwrap(), "8:7");
        assert_eq!(normalize("aspect", "14:9").unwrap(), "14:9");
        assert!(normalize("aspect", "wide").is_err());
        assert_eq!(normalize("modes", "512x448I, 256x224p 256X239p").unwrap(), "256x224p,256x239p,512x448i");
        assert_eq!(normalize("modes", "1280x720p, screen:960x544p, tv:720x480p").unwrap(), "screen:960x544p,720x480p,1280x720p");
        assert!(normalize("modes", "480p").is_err());
        assert!(normalize("modes", "watch:100x100p").is_err());
        assert_eq!(normalize("refresh", "60 50").unwrap(), "50,60");
        assert_eq!(normalize("refresh", "59.94Hz").unwrap(), "60");
        assert_eq!(normalize("refresh", "75").unwrap(), "75");
        assert_eq!(normalize("widescreen", "Patch").unwrap(), "patch");
    }

    #[test]
    fn defaults_follow_the_system_and_a_pal_region() {
        let snes = display_default(Platform::Snes, "US");
        assert_eq!((snes.aspect, snes.value("modes").as_deref(), snes.refresh, snes.hz), (Some("4:3"), Some("256x224p"), Some("60"), Some("60.0988")));
        assert!(snes.system.contains(&"512x478i") && snes.system[0] == "256x224p");
        let pal = display_default(Platform::PlayStation2, "EU");
        assert_eq!((pal.value("modes").as_deref(), pal.refresh), (Some("640x512i"), Some("50")));
        // A disc for the USA and Europe is shown as its NTSC self.
        assert_eq!(display_default(Platform::PlayStation2, "US,EU").refresh, Some("60"));
        // 480p is a PS2 game's own, not the system's default.
        assert!(!display_default(Platform::PlayStation2, "US").modes.contains(&"720x480p"));
        assert_eq!(display_default(Platform::Wii, "EU").value("modes").as_deref(), Some("640x480i,640x480p,640x576i"));
        assert_eq!(display_default(Platform::Wii, "EU").refresh, Some("50,60"));
        assert_eq!(display_default(Platform::GameBoyAdvance, "EU").value("modes").as_deref(), Some("screen:240x160p"));
        assert!(display_default(Platform::PlayStationVita, "JP").system.contains(&"1920x1080i"));
        assert_eq!(display_default(Platform::PlayStationVita, "JP").value("modes").as_deref(), Some("screen:960x544p"));
        assert_eq!(display_default(Platform::Wii, "JP").widescreen, "option");
        // A computer of the PAL markets with no region is a PAL one.
        assert_eq!(display_default(Platform::Amiga, "").value("modes").as_deref(), Some("320x256p"));
        assert_eq!(display_default(Platform::Amiga, "US").value("modes").as_deref(), Some("320x200p"));
        // A PC has no default.
        assert_eq!(display_default(Platform::PcWindows, "US").value("modes"), None);
        assert_eq!(display_default(Platform::PcWindows, "US").aspect, None);
    }

    #[test]
    fn every_listed_resolution_parses() {
        for id in 0..=40000u16 {
            let Some(p) = Platform::from_igdb_id(id) else { continue };
            for region in ["US", "EU", ""] {
                let d = display_default(p, region);
                for r in d.modes.iter().chain(&d.system) {
                    assert!(Resolution::parse(r).is_ok(), "{p:?} {r}");
                }
                assert!(d.modes.iter().all(|m| d.system.contains(m)), "{p:?}: the default is among the system's");
                if let Some(a) = d.aspect {
                    assert!(normalize("aspect", a).is_ok());
                }
            }
        }
    }

    #[test]
    fn a_ps3_resolution_is_its_modes() {
        assert_eq!(ps3_resolution(63), (vec!["720x480p", "720x576p", "1280x720p", "1920x1080i", "1920x1080p"], true));
        assert_eq!(ps3_resolution(1 | 2), (vec!["720x480p", "720x576p"], false));
        assert_eq!(ps3_resolution(1 | 4), (vec!["720x480p", "1280x720p"], true));
    }

    #[test]
    fn a_number_is_read_from_param_sfo() {
        // Header, one int32 entry "RESOLUTION" = 0x3F.
        let mut b = vec![0x00, b'P', b'S', b'F', 1, 1, 0, 0];
        let key_table = 0x14 + 0x10;
        let data_table = key_table + 12;
        b.extend((key_table as u32).to_le_bytes());
        b.extend((data_table as u32).to_le_bytes());
        b.extend(1u32.to_le_bytes());
        b.extend(0u16.to_le_bytes());
        b.extend(0x0404u16.to_le_bytes());
        b.extend(4u32.to_le_bytes());
        b.extend(4u32.to_le_bytes());
        b.extend(0u32.to_le_bytes());
        b.extend(b"RESOLUTION\0\0");
        b.extend(0x3Fu32.to_le_bytes());
        assert_eq!(sfo_u32(&b, "RESOLUTION"), Some(63));
        assert_eq!(sfo_u32(&b, "SOUND_FORMAT"), None);
    }

    #[test]
    fn serials_and_mame_displays() {
        assert_eq!(serial_key("SLUS-21323"), serial_key("SLUS 21323"));
        assert_eq!(serial_key("slus_213.23"), "SLUS21323");
        assert_eq!(mame_aspect(90), "3:4");
        assert_eq!(mame_aspect(270), "3:4");
        assert_eq!(mame_aspect(0), "4:3");
        assert_eq!(mame_refresh(59.637405).as_deref(), Some("60"));
        assert_eq!(mame_refresh(57.5).as_deref(), Some("58"));
        assert_eq!(mame_refresh(0.0), None);
        // 6 MHz / 384 = 15.6 kHz: 224 lines progressive, 448 interlaced; 31 kHz 480 progressive.
        assert_eq!(mame_resolution(320, 224, Some(6_000_000.0), Some(384)).as_deref(), Some("320x224p"));
        assert_eq!(mame_resolution(512, 448, Some(8_000_000.0), Some(512)).as_deref(), Some("512x448i"));
        assert_eq!(mame_resolution(640, 480, Some(25_175_000.0), Some(800)).as_deref(), Some("640x480p"));
        assert_eq!(mame_resolution(0, 0, None, None), None);
    }

    #[test]
    fn a_dreamcast_header_says_vga() {
        let mut h = b"SEGA SEGAKATANA SEGA ENTERPRISES".to_vec();
        h.resize(0x100, b' ');
        h[0x38..0x3F].copy_from_slice(b"E000F10");
        assert_eq!(dreamcast_vga(&h), Some(true));
        h[0x38..0x3F].copy_from_slice(b"E000F00");
        assert_eq!(dreamcast_vga(&h), Some(false));
        assert_eq!(dreamcast_vga(b"not a header"), None);
    }
}
