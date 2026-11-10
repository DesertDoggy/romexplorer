//! Cartridge ROMs: NES, N64 and SNES dumps, rewritten into the layout picked in the settings.
//!
//! Unlike a disc image, a cartridge dump has no one right container: emulators, flash carts
//! and DATs each want a different header or byte order. So nothing here is automatic -- each
//! platform has a setting ([`Cartridge`]), and a dump already in that layout is left alone.
//! The conversions and their checks are romexplorer's (`rom::convert::cartridge`):
//!
//! - NES: with the iNES header (`.nes`), or without it (`.nes` or `.unh`). Whether a dump has
//!   one is its hash's entry in `fcdb.bin`; a hash not there is an error that names it.
//! - N64: big-endian `.z64`, byte-swapped `.v64` or little-endian `.n64`, by the first word.
//! - SNES: bare `.sfc`, or `.smc` with a 512-byte copier header. A header is there when the
//!   size is whole KB plus 512 bytes, and the cartridge's own header has to agree.

use std::path::Path;

pub use romexplorer::rom::convert::cartridge::{Converted, Layout};
use romexplorer::rom::models::{N64Type, NesHeader, NesUnheaderedExtension, SnesHeader};

use crate::settings::Cartridge;

/// What [`Cartridge::nes`] takes: headered `.nes`, headerless `.nes`, headerless `.unh`.
pub const NES_FORMATS: [&str; 3] = ["headered", "headerless", "unh"];
pub const N64_FORMATS: [&str; 3] = ["z64", "v64", "n64"];
pub const SNES_FORMATS: [&str; 2] = ["sfc", "smc"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cart {
    Nes,
    N64,
    Snes,
}

impl Cart {
    pub fn of(path: &Path) -> Option<Self> {
        Some(match crate::classify::extension(path).as_str() {
            "nes" | "unh" => Self::Nes,
            "z64" | "v64" | "n64" => Self::N64,
            "sfc" | "smc" | "swc" => Self::Snes,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Nes => "NES",
            Self::N64 => "N64",
            Self::Snes => "SNES",
        }
    }

    /// The layout the settings pick for this platform.
    pub fn layout(self, settings: &Cartridge) -> Result<Layout, String> {
        let unknown = |platform: &str, value: &str, all: &[&str]| format!("unknown {platform} format '{value}' ({})", all.join(", "));
        let value = |text: &str| text.trim().to_ascii_lowercase();
        Ok(match self {
            Self::Nes => match value(&settings.nes).as_str() {
                "headered" | "" => Layout::Nes { header: NesHeader::Headered, extension: NesUnheaderedExtension::Nes },
                "headerless" => Layout::Nes { header: NesHeader::Unheadered, extension: NesUnheaderedExtension::Nes },
                "unh" => Layout::Nes { header: NesHeader::Unheadered, extension: NesUnheaderedExtension::Unh },
                other => return Err(unknown("NES", other, &NES_FORMATS)),
            },
            Self::N64 => Layout::N64(match value(&settings.n64).as_str() {
                "z64" | "" => N64Type::BigEndian,
                "v64" => N64Type::ByteSwapped,
                "n64" => N64Type::LittleEndian,
                other => return Err(unknown("N64", other, &N64_FORMATS)),
            }),
            Self::Snes => Layout::Snes(match value(&settings.snes).as_str() {
                "sfc" | "" => SnesHeader::Sfc,
                "smc" => SnesHeader::Smc,
                other => return Err(unknown("SNES", other, &SNES_FORMATS)),
            }),
        })
    }
}

/// The three choices in one line, for the main window: "NES headered .nes · N64 .z64 · SNES .sfc".
/// A value the settings file got wrong shows as such.
pub fn summary(settings: &Cartridge) -> String {
    let nes = match Cart::Nes.layout(settings) {
        Ok(Layout::Nes { header: NesHeader::Headered, .. }) => "headered .nes".to_string(),
        Ok(layout) => format!("headerless .{}", layout.extension()),
        Err(_) => format!("'{}'?", settings.nes),
    };
    let short = |cart: Cart, value: &str| cart.layout(settings).map(|layout| format!(".{}", layout.extension())).unwrap_or_else(|_| format!("'{value}'?"));
    format!("NES {nes} · N64 {} · SNES {}", short(Cart::N64, &settings.n64), short(Cart::Snes, &settings.snes))
}

/// What the dump at `path` becomes under `settings`; reads it whole (a cartridge is a few MB
/// at most), refusing a file far larger than one of its kind can be.
pub fn convert(cart: Cart, path: &Path, settings: &Cartridge) -> Result<Converted, String> {
    let layout = cart.layout(settings)?;
    let size = std::fs::metadata(path).map_err(|err| format!("{}: {err}", path.display()))?.len();
    if size == 0 || size > layout.max_file_size() {
        return Err(format!("{size} bytes is not the size of a {} cartridge dump", layout.platform()));
    }
    let bytes = std::fs::read(path).map_err(|err| format!("{}: {err}", path.display()))?;
    romexplorer::rom::convert::cartridge::convert(path, bytes, layout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_names_each_choice() {
        assert_eq!(summary(&Cartridge::default()), "NES headered .nes · N64 .z64 · SNES .sfc");
        let settings = Cartridge { nes: "unh".into(), n64: "v64".into(), snes: "smc".into() };
        assert_eq!(summary(&settings), "NES headerless .unh · N64 .v64 · SNES .smc");
    }

    #[test]
    fn every_setting_value_is_a_layout_and_nothing_else_is() {
        let mut settings = Cartridge::default();
        for nes in NES_FORMATS {
            settings.nes = nes.into();
            assert!(Cart::Nes.layout(&settings).is_ok());
        }
        assert_eq!(Cart::Nes.layout(&settings).unwrap().extension(), "unh");
        for n64 in N64_FORMATS {
            settings.n64 = n64.into();
            assert_eq!(Cart::N64.layout(&settings).unwrap().extension(), n64);
        }
        for snes in SNES_FORMATS {
            settings.snes = snes.into();
            assert_eq!(Cart::Snes.layout(&settings).unwrap().extension(), snes);
        }
        settings.snes = "fig".into();
        assert!(Cart::Snes.layout(&settings).is_err());
    }

    #[test]
    fn a_nes_dump_not_in_the_database_is_refused_with_its_hash() {
        let dir = std::env::temp_dir().join(format!("chdlite_cart_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("unknown.nes");
        let bytes = vec![0x5Au8; 40 * 1024];
        std::fs::write(&path, &bytes).unwrap();
        let err = convert(Cart::Nes, &path, &Cartridge::default()).unwrap_err();
        assert!(err.contains(&format!("{:032x}", romexplorer::rom::convert::nes::xxh3_128(&bytes))), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
