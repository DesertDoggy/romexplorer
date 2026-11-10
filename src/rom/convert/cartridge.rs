//! Whole-file cartridge conversions: one dump in memory, rewritten into the layout asked for.
//!
//! The same checks as the streaming converters next door (the NES header database, the SNES
//! copier-header size and internal-header agreement, the N64 first word), for a caller that
//! has the file itself rather than a stream out of an archive -- chdlite's cartridge mode. A
//! dump already in the layout asked for comes back unchanged, so the caller can skip it.

use std::path::Path;

use super::nes::{relayer, shared_fcdb};
use super::{snes_copier_header, N64Converter, SnesConverter, StreamConverter, SNES_COPIER_HEADER_LEN};
use crate::rom::models::{N64Type, NesHeader, NesUnheaderedExtension, RomByteOrder, SnesHeader};
use crate::rom::platforms::nintendo::{read_snes_rom_metadata, MAX_PLAUSIBLE_NES_FILE_SIZE};

/// The largest N64 cartridge is 64 MB; twice that leaves room for homebrew and overdumps.
pub const MAX_N64_FILE_SIZE: u64 = 128 << 20;
/// The largest SNES cartridge is 6 MB (ExHiROM); 16 MB is generous for any odd dump.
pub const MAX_SNES_FILE_SIZE: u64 = 16 << 20;

/// The layout a dump should end up in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Nes { header: NesHeader, extension: NesUnheaderedExtension },
    N64(N64Type),
    Snes(SnesHeader),
}

impl Layout {
    pub fn platform(self) -> &'static str {
        match self {
            Self::Nes { .. } => "NES",
            Self::N64(_) => "N64",
            Self::Snes(_) => "SNES",
        }
    }

    /// The largest file of the platform worth reading whole.
    pub fn max_file_size(self) -> u64 {
        match self {
            Self::Nes { .. } => MAX_PLAUSIBLE_NES_FILE_SIZE,
            Self::N64(_) => MAX_N64_FILE_SIZE,
            Self::Snes(_) => MAX_SNES_FILE_SIZE,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Nes { header: NesHeader::Unheadered, extension: NesUnheaderedExtension::Unh } => "unh",
            Self::Nes { .. } => "nes",
            Self::N64(N64Type::BigEndian) => "z64",
            Self::N64(N64Type::ByteSwapped) => "v64",
            Self::N64(N64Type::LittleEndian) => "n64",
            Self::Snes(SnesHeader::Sfc) => "sfc",
            Self::Snes(SnesHeader::Smc) => "smc",
            Self::Snes(SnesHeader::Fig) => "fig",
            Self::Snes(SnesHeader::Swc) => "swc",
            Self::Snes(SnesHeader::Ufo) => "ufo",
            Self::Snes(SnesHeader::Gd7) => "078",
        }
    }

    /// e.g. "headerless .unh".
    pub fn describe(self) -> String {
        match self {
            Self::Nes { header: NesHeader::Headered, .. } => "headered .nes".to_string(),
            Self::Nes { .. } => format!("headerless .{}", self.extension()),
            Self::N64(target) => order_name(order_of(target)).to_string(),
            Self::Snes(SnesHeader::Sfc) => "no copier header (.sfc)".to_string(),
            Self::Snes(_) => format!("copier header (.{})", self.extension()),
        }
    }
}

/// What a dump becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Converted {
    /// The layout it was in, e.g. "headered".
    pub from: String,
    /// [`Layout::describe`] of the one asked for.
    pub to: String,
    pub extension: &'static str,
    /// The output: the input itself when it was already in that layout.
    pub data: Vec<u8>,
    /// Whether the content changed (and not, at most, the extension).
    pub changed: bool,
}

/// Rewrites `bytes` (the dump at `path`, which SNES reads its internal header from) into
/// `layout`. An `Err` is one sentence saying why the dump can't be placed.
pub fn convert(path: &Path, bytes: Vec<u8>, layout: Layout) -> Result<Converted, String> {
    let (to, extension) = (layout.describe(), layout.extension());
    let (from, data, changed) = match layout {
        Layout::Nes { header, .. } => {
            let db = shared_fcdb().ok_or("the NES header database (fcdb.bin) could not be loaded")?;
            let done = relayer(db, bytes, header == NesHeader::Headered)?;
            (if done.was_headered { "headered" } else { "headerless" }.to_string(), done.data, done.changed)
        }
        Layout::N64(target) => n64(bytes, target)?,
        Layout::Snes(target) => {
            let internal = read_snes_rom_metadata(path).and_then(|meta| meta.headered);
            snes(bytes, internal, target)?
        }
    };
    Ok(Converted { from, to, extension, data, changed })
}

fn order_of(target: N64Type) -> RomByteOrder {
    match target {
        N64Type::BigEndian => RomByteOrder::BigEndian,
        N64Type::ByteSwapped => RomByteOrder::ByteSwapped,
        N64Type::LittleEndian => RomByteOrder::LittleEndian,
    }
}

fn order_name(order: RomByteOrder) -> &'static str {
    match order {
        RomByteOrder::BigEndian => "big-endian (.z64)",
        RomByteOrder::ByteSwapped => "byte-swapped (.v64)",
        RomByteOrder::LittleEndian => "little-endian (.n64)",
    }
}

/// Runs a streaming converter over a whole buffer.
fn stream(mut converter: StreamConverter, bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(converter.output_len() as usize);
    for chunk in converter.push(bytes).map_err(|err| err.to_string())? {
        out.extend_from_slice(&chunk);
    }
    for chunk in converter.finish().map_err(|err| err.to_string())? {
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

fn n64(bytes: Vec<u8>, target: N64Type) -> Result<(String, Vec<u8>, bool), String> {
    let current = match bytes.get(..4) {
        Some([0x80, 0x37, 0x12, 0x40]) => RomByteOrder::BigEndian,
        Some([0x37, 0x80, 0x40, 0x12]) => RomByteOrder::ByteSwapped,
        Some([0x40, 0x12, 0x37, 0x80]) => RomByteOrder::LittleEndian,
        _ => return Err("not an N64 dump: its first word is none of the three byte orders".to_string()),
    };
    let from = order_name(current).to_string();
    if current == order_of(target) {
        return Ok((from, bytes, false));
    }
    if bytes.len() % 4 != 0 {
        return Err(format!("{} bytes is not whole 32-bit words, so the byte order can't be changed exactly", bytes.len()));
    }
    let converter = StreamConverter::new(Box::new(N64Converter { target, known_order: Some(current) }), bytes.len() as u64, &bytes[..4]).map_err(|err| err.to_string())?;
    Ok((from, stream(converter, &bytes)?, true))
}

fn snes(bytes: Vec<u8>, internal: Option<bool>, target: SnesHeader) -> Result<(String, Vec<u8>, bool), String> {
    let headered = snes_copier_header(bytes.len() as u64, internal)?;
    let from = if headered { "copier header" } else { "no copier header" }.to_string();
    if headered == (target != SnesHeader::Sfc) {
        return Ok((from, bytes, false));
    }
    let head = &bytes[..bytes.len().min(SNES_COPIER_HEADER_LEN as usize)];
    let converter = StreamConverter::new(Box::new(SnesConverter { target, known_headered: Some(headered) }), bytes.len() as u64, head).map_err(|err| err.to_string())?;
    Ok((from, stream(converter, &bytes)?, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn n64_goes_to_each_order_and_back() {
        let z64: Vec<u8> = [0x80u8, 0x37, 0x12, 0x40].into_iter().chain((0..4092u32).map(|i| (i * 13 % 251) as u8)).collect();
        let path = Path::new("x.z64");
        let v64 = convert(path, z64.clone(), Layout::N64(N64Type::ByteSwapped)).unwrap();
        assert!(v64.changed);
        assert_eq!((&v64.data[..4], v64.extension), (&[0x37u8, 0x80, 0x40, 0x12][..], "v64"));
        let n64 = convert(path, v64.data, Layout::N64(N64Type::LittleEndian)).unwrap();
        assert_eq!(&n64.data[..4], &[0x40, 0x12, 0x37, 0x80]);
        let back = convert(path, n64.data, Layout::N64(N64Type::BigEndian)).unwrap();
        assert_eq!(back.data, z64);
        assert!(!convert(path, z64, Layout::N64(N64Type::BigEndian)).unwrap().changed, "already big-endian");
        assert!(convert(path, vec![0; 64], Layout::N64(N64Type::BigEndian)).is_err(), "no known first word");
    }

    /// A LoROM cartridge with a valid internal header (title, map mode, country, checksum
    /// pair), so `read_snes_rom_metadata` places it.
    fn lorom() -> Vec<u8> {
        let mut rom = vec![0u8; 64 * 1024];
        rom[0x7FC0..0x7FC0 + 21].copy_from_slice(b"CONVERT TEST CART    ");
        rom[0x7FD5] = 0x20;
        rom[0x7FD9] = 0x01;
        rom[0x7FDC..0x7FDE].copy_from_slice(&0x1234u16.to_le_bytes());
        rom[0x7FDE..0x7FE0].copy_from_slice(&(0xFFFF - 0x1234u16).to_le_bytes());
        rom
    }

    fn temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("romexplorer_cartridge_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn snes_header_is_added_and_removed_when_size_and_cartridge_header_agree() {
        let rom = lorom();
        let sfc = temp("cart.sfc", &rom);
        let smc = convert(&sfc, rom.clone(), Layout::Snes(SnesHeader::Smc)).unwrap();
        assert!(smc.changed);
        assert_eq!(smc.data.len(), rom.len() + 512);
        assert_eq!(&smc.data[..2], &[8, 0], "64 KB in 8 KB units");
        let smc_path = temp("cart.smc", &smc.data);
        let back = convert(&smc_path, smc.data.clone(), Layout::Snes(SnesHeader::Sfc)).unwrap();
        assert_eq!((back.data, back.extension), (rom.clone(), "sfc"));
        assert!(!convert(&sfc, rom.clone(), Layout::Snes(SnesHeader::Sfc)).unwrap().changed);
        assert!(!convert(&smc_path, smc.data, Layout::Snes(SnesHeader::Smc)).unwrap().changed);

        // 512 bytes over, but the cartridge header where a bare dump has it: they disagree.
        let mut odd = rom.clone();
        odd.extend_from_slice(&[0u8; 512]);
        let odd_path = temp("odd.smc", &odd);
        assert!(convert(&odd_path, odd, Layout::Snes(SnesHeader::Sfc)).unwrap_err().contains("own header"));
        // Neither whole KB nor KB + 512.
        let mut short = rom;
        short.truncate(64 * 1024 - 100);
        let short_path = temp("short.sfc", &short);
        assert!(convert(&short_path, short, Layout::Snes(SnesHeader::Smc)).is_err());
    }
}
