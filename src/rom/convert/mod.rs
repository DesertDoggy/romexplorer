//! ROM file-layout conversions that sit *inside* a stream: between what was read/extracted
//! and what gets written/archived (`operation::Single`'s convert stage). Every converter
//! here is a header prefix/strip and/or a fixed byte-order transform, so it runs chunk by
//! chunk ([`Shape::Streaming`]); NES is the one exception ([`Shape::Buffered`]: adding an
//! iNES header needs the whole file's hash to look the header up in the database, see
//! [`nes`]). No converter writes or reads a file itself.
//!
//! Registered conversions (`convert::converter_for`, gated by `ConvertFlags` bits):
//! - NES: iNES header add/strip (database-backed, [`nes`]).
//! - SNES: 512-byte copier header strip/add (SMC/SWC exact; FIG/UFO/GD7 signature-based --
//!   see [`SnesConverter`]).
//! - N64: z64 <-> v64 <-> n64 byte order.
//! - FDS: `FDS\x1A` 16-byte header add/strip. (Quick Disk re-layout is not a header
//!   operation and isn't implemented here.)
//! - NDS/3DS crypt: placeholders only.

pub mod cartridge;
pub mod nes;

use std::fmt;
use std::sync::Arc;

use crate::common::models::Platform;

use super::models::{ConvertArgs, ConvertFlags, N64Type, NesHeader, NesUnheaderedExtension, RomByteOrder, RomMetadata, SnesHeader};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConvertError {
    /// The input is already in the requested form (or the conversion doesn't apply to it):
    /// nothing to do, not a failure.
    NotApplicable(String),
    Failed(String),
}

impl fmt::Display for ConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotApplicable(message) => write!(f, "conversion not applicable: {message}"),
            Self::Failed(message) => write!(f, "conversion failed: {message}"),
        }
    }
}

impl std::error::Error for ConvertError {}

/// A fixed in-place byte transform over aligned data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kernel {
    /// Swap every pair of bytes (16-bit byte swap).
    Swap16,
    /// Reverse every group of 4 bytes (32-bit endianness flip).
    Reverse32,
}

impl Kernel {
    pub fn alignment(self) -> usize {
        match self {
            Self::Swap16 => 2,
            Self::Reverse32 => 4,
        }
    }

    /// `data.len()` must be a multiple of [`Self::alignment`].
    pub fn apply(self, data: &mut [u8]) {
        match self {
            Self::Swap16 => data.chunks_exact_mut(2).for_each(|pair| pair.swap(0, 1)),
            Self::Reverse32 => data.chunks_exact_mut(4).for_each(|quad| quad.reverse()),
        }
    }
}

/// How a conversion consumes its input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    /// Drop the first `strip_head` bytes, emit `prefix`, run `kernels` (in order) over the
    /// rest, emit `suffix`. Chunk boundaries are carried so kernels always see aligned data.
    Streaming { strip_head: u64, prefix: Vec<u8>, kernels: Vec<Kernel>, suffix: Vec<u8> },
    /// The whole input (at most `max_len` bytes) is needed before anything can be emitted.
    Buffered { max_len: u64 },
}

pub trait Convert: Send {
    /// Short human label, e.g. "nes: add iNES header".
    fn label(&self) -> String;

    /// Extension (no dot) the converted file should get.
    fn output_extension(&self) -> &'static str;

    /// Decides the conversion for an input of `input_len` bytes whose first bytes are
    /// `head` (may be empty when the input isn't a plain file). `Err(NotApplicable)` means
    /// the input is already in the requested form.
    fn shape(&mut self, input_len: u64, head: &[u8]) -> Result<Shape, ConvertError>;

    /// Expected output length, for progress estimates.
    fn output_len(&self, input_len: u64) -> u64;

    /// [`Shape::Buffered`] only: the whole conversion.
    fn finish_buffered(&mut self, _input: Vec<u8>) -> Result<Vec<u8>, ConvertError> {
        Err(ConvertError::Failed("converter is not buffered".to_string()))
    }
}

/// Runs a [`Convert`] over pushed chunks. Output chunks are shared (`Arc<[u8]>`) so the
/// pipeline can hand each to a hash digest and to one or more sinks without copying.
pub struct StreamConverter {
    convert: Box<dyn Convert>,
    shape: Shape,
    strip_remaining: u64,
    prefix_pending: bool,
    carry: Vec<u8>,
    buffer: Vec<u8>,
    input_len: u64,
}

impl StreamConverter {
    /// `head`: see [`Convert::shape`].
    pub fn new(mut convert: Box<dyn Convert>, input_len: u64, head: &[u8]) -> Result<Self, ConvertError> {
        let shape = convert.shape(input_len, head)?;
        let strip_remaining = match &shape {
            Shape::Streaming { strip_head, .. } => *strip_head,
            Shape::Buffered { .. } => 0,
        };
        Ok(Self { convert, shape, strip_remaining, prefix_pending: true, carry: Vec::new(), buffer: Vec::new(), input_len })
    }

    pub fn label(&self) -> String {
        self.convert.label()
    }

    pub fn output_extension(&self) -> &'static str {
        self.convert.output_extension()
    }

    pub fn output_len(&self) -> u64 {
        self.convert.output_len(self.input_len)
    }

    pub fn is_buffered(&self) -> bool {
        matches!(self.shape, Shape::Buffered { .. })
    }

    /// Feeds input; returns the output chunks ready so far (none while buffering).
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Arc<[u8]>>, ConvertError> {
        match &self.shape {
            Shape::Buffered { max_len } => {
                if self.buffer.len() as u64 + chunk.len() as u64 > *max_len {
                    return Err(ConvertError::Failed(format!("input exceeds the {max_len}-byte limit of this conversion")));
                }
                self.buffer.extend_from_slice(chunk);
                Ok(Vec::new())
            }
            Shape::Streaming { prefix, kernels, .. } => {
                let mut out = Vec::new();
                let mut data = chunk;
                if self.strip_remaining > 0 {
                    let skip = (self.strip_remaining).min(data.len() as u64) as usize;
                    self.strip_remaining -= skip as u64;
                    data = &data[skip..];
                }
                if data.is_empty() {
                    return Ok(out);
                }
                if self.prefix_pending {
                    self.prefix_pending = false;
                    if !prefix.is_empty() {
                        out.push(Arc::from(prefix.as_slice()));
                    }
                }
                if kernels.is_empty() {
                    out.push(Arc::from(data));
                    return Ok(out);
                }
                let alignment = kernels.iter().map(|k| k.alignment()).max().unwrap_or(1);
                let mut work = std::mem::take(&mut self.carry);
                work.extend_from_slice(data);
                let aligned = work.len() - work.len() % alignment;
                let tail = work.split_off(aligned);
                self.carry = tail;
                if !work.is_empty() {
                    for kernel in kernels {
                        kernel.apply(&mut work);
                    }
                    out.push(Arc::from(work));
                }
                Ok(out)
            }
        }
    }

    /// Flushes: the buffered conversion's whole output, or any unaligned tail (emitted
    /// untransformed, as chdman-style tools do with a trailing partial word) plus the suffix.
    pub fn finish(mut self) -> Result<Vec<Arc<[u8]>>, ConvertError> {
        match &self.shape {
            Shape::Buffered { .. } => {
                let input = std::mem::take(&mut self.buffer);
                let output = self.convert.finish_buffered(input)?;
                Ok(vec![Arc::from(output)])
            }
            Shape::Streaming { prefix, suffix, .. } => {
                let mut out = Vec::new();
                if self.prefix_pending && !prefix.is_empty() {
                    out.push(Arc::from(prefix.as_slice()));
                }
                if !self.carry.is_empty() {
                    out.push(Arc::from(std::mem::take(&mut self.carry)));
                }
                if !suffix.is_empty() {
                    out.push(Arc::from(suffix.as_slice()));
                }
                Ok(out)
            }
        }
    }
}

/// The converter for `meta.platform` under `args`, or `None` when the platform has none
/// registered or its `ConvertFlags` bit is off. Whether the input actually needs converting
/// is decided later by [`Convert::shape`] (it may answer `NotApplicable`).
pub fn converter_for(meta: &RomMetadata, args: &ConvertArgs) -> Option<Box<dyn Convert>> {
    let flags = args.convert_flags;
    match meta.platform {
        Platform::Nes | Platform::Famicom if flags.contains(ConvertFlags::NES) => Some(Box::new(NesConverter {
            target: args.nes_header,
            unheadered_extension: args.nes_unheadered_extension,
            known_headered: meta.headered,
        })),
        Platform::FamicomDiskSystem if flags.contains(ConvertFlags::FDS) => {
            Some(Box::new(FdsConverter { target: args.fds_header, known_headered: meta.headered }))
        }
        Platform::Snes | Platform::SuperFamicom if flags.contains(ConvertFlags::SNES) => {
            Some(Box::new(SnesConverter { target: args.snes_header, known_headered: meta.headered }))
        }
        Platform::Nintendo64 if flags.contains(ConvertFlags::N64) => {
            Some(Box::new(N64Converter { target: args.n64_type, known_order: meta.byte_order }))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// NES (iNES header)
// ---------------------------------------------------------------------------

/// iNES header size.
pub const INES_HEADER_LEN: u64 = 16;

/// Adds or strips the 16-byte iNES header. Buffered: the header to add comes from the
/// header database keyed by the bare ROM's hash ([`nes::relayer`]); stripping also goes through
/// the database so a file that isn't a known NES ROM is refused rather than blindly cut.
pub struct NesConverter {
    pub target: NesHeader,
    pub unheadered_extension: NesUnheaderedExtension,
    /// From detection (`RomMetadata::headered`), if it decided.
    pub known_headered: Option<bool>,
}

impl NesConverter {
    fn currently_headered(&self, head: &[u8]) -> Option<bool> {
        self.known_headered.or_else(|| (head.len() >= 4).then(|| &head[..4] == b"NES\x1A"))
    }
}

impl Convert for NesConverter {
    fn label(&self) -> String {
        match self.target {
            NesHeader::Headered => "nes: add iNES header".to_string(),
            NesHeader::Unheadered => "nes: strip iNES header".to_string(),
        }
    }

    fn output_extension(&self) -> &'static str {
        match (self.target, self.unheadered_extension) {
            (NesHeader::Headered, _) | (NesHeader::Unheadered, NesUnheaderedExtension::Nes) => "nes",
            (NesHeader::Unheadered, NesUnheaderedExtension::Unh) => "unh",
        }
    }

    fn shape(&mut self, _input_len: u64, head: &[u8]) -> Result<Shape, ConvertError> {
        let want_headered = self.target == NesHeader::Headered;
        if self.currently_headered(head) == Some(want_headered) {
            return Err(ConvertError::NotApplicable(format!("already {}", if want_headered { "headered" } else { "unheadered" })));
        }
        Ok(Shape::Buffered { max_len: super::platforms::nintendo::MAX_PLAUSIBLE_NES_FILE_SIZE })
    }

    fn output_len(&self, input_len: u64) -> u64 {
        match self.target {
            NesHeader::Headered => input_len + INES_HEADER_LEN,
            NesHeader::Unheadered => input_len.saturating_sub(INES_HEADER_LEN),
        }
    }

    fn finish_buffered(&mut self, input: Vec<u8>) -> Result<Vec<u8>, ConvertError> {
        let db = nes::shared_fcdb().ok_or_else(|| ConvertError::Failed("the NES header database (fcdb.bin) could not be loaded".to_string()))?;
        let relayered = nes::relayer(db, input, self.target == NesHeader::Headered).map_err(ConvertError::Failed)?;
        Ok(relayered.data)
    }
}

// ---------------------------------------------------------------------------
// FDS (`FDS\x1A` header)
// ---------------------------------------------------------------------------

pub const FDS_HEADER_LEN: u64 = 16;
/// Bytes per disk side in a `.fds` image.
pub const FDS_SIDE_LEN: u64 = 65500;

/// Adds or strips the 16-byte `FDS\x1A` header (`FDS\x1A`, side count, 11 zero bytes).
pub struct FdsConverter {
    pub target: NesHeader,
    pub known_headered: Option<bool>,
}

impl FdsConverter {
    fn currently_headered(&self, input_len: u64, head: &[u8]) -> bool {
        if let Some(known) = self.known_headered {
            return known;
        }
        if head.len() >= 4 {
            return &head[..4] == b"FDS\x1A";
        }
        input_len % FDS_SIDE_LEN == FDS_HEADER_LEN
    }
}

impl Convert for FdsConverter {
    fn label(&self) -> String {
        match self.target {
            NesHeader::Headered => "fds: add FDS header".to_string(),
            NesHeader::Unheadered => "fds: strip FDS header".to_string(),
        }
    }

    fn output_extension(&self) -> &'static str {
        "fds"
    }

    fn shape(&mut self, input_len: u64, head: &[u8]) -> Result<Shape, ConvertError> {
        let headered = self.currently_headered(input_len, head);
        match self.target {
            NesHeader::Headered if headered => Err(ConvertError::NotApplicable("already headered".to_string())),
            NesHeader::Unheadered if !headered => Err(ConvertError::NotApplicable("already unheadered".to_string())),
            NesHeader::Headered => {
                let sides = (input_len / FDS_SIDE_LEN).min(255) as u8;
                let mut prefix = vec![0u8; FDS_HEADER_LEN as usize];
                prefix[..4].copy_from_slice(b"FDS\x1A");
                prefix[4] = sides;
                Ok(Shape::Streaming { strip_head: 0, prefix, kernels: Vec::new(), suffix: Vec::new() })
            }
            NesHeader::Unheadered => Ok(Shape::Streaming { strip_head: FDS_HEADER_LEN, prefix: Vec::new(), kernels: Vec::new(), suffix: Vec::new() }),
        }
    }

    fn output_len(&self, input_len: u64) -> u64 {
        match self.target {
            NesHeader::Headered => input_len + FDS_HEADER_LEN,
            NesHeader::Unheadered => input_len.saturating_sub(FDS_HEADER_LEN),
        }
    }
}

// ---------------------------------------------------------------------------
// SNES (512-byte copier headers)
// ---------------------------------------------------------------------------

pub const SNES_COPIER_HEADER_LEN: u64 = 512;

/// Strips a 512-byte copier header (any kind -- they're all 512 bytes) and/or prepends the
/// requested one. SMC and SWC headers are built exactly (ROM size in 8 KiB units, little-
/// endian, at 0..2; SWC adds its `AA BB 04` id at 8..11). FIG, UFO and GD7 are written with
/// their well-known id bytes but without the per-mapping flag bytes those copiers also
/// stored (LoROM/HiROM, SRAM size): best effort, flagged in the label.
pub struct SnesConverter {
    pub target: SnesHeader,
    pub known_headered: Option<bool>,
}

/// Whether an SNES dump of `input_len` bytes carries a 512-byte copier header. Copier headers
/// are always exactly 512 bytes and a cartridge is whole KB, so the size says it; `internal`
/// is where detection found the cartridge's own header (`RomMetadata::headered`, from
/// `read_snes_rom_metadata`), and has to agree. A dump with no readable internal header (some
/// prototypes and homebrew) goes by size alone.
pub fn snes_copier_header(input_len: u64, internal: Option<bool>) -> Result<bool, String> {
    let by_size = match input_len % 1024 {
        0 => false,
        512 => true,
        rest => return Err(format!("{input_len} bytes is neither whole KB nor whole KB plus a 512-byte copier header ({rest} over)")),
    };
    match internal {
        Some(by_header) if by_header != by_size => Err(format!(
            "its size ({input_len} bytes) says {} copier header, but the cartridge's own header sits where it would {}",
            if by_size { "a" } else { "no" },
            if by_header { "with one" } else { "without one" }
        )),
        _ => Ok(by_size),
    }
}

impl SnesConverter {
    fn currently_headered(&self, input_len: u64) -> bool {
        snes_copier_header(input_len, self.known_headered).unwrap_or(input_len % 1024 == 512)
    }

    fn header(kind: SnesHeader, rom_len: u64) -> Vec<u8> {
        let mut header = vec![0u8; SNES_COPIER_HEADER_LEN as usize];
        let units = (rom_len / 8192).min(u16::MAX as u64) as u16;
        match kind {
            SnesHeader::Sfc => header.clear(),
            SnesHeader::Smc => header[..2].copy_from_slice(&units.to_le_bytes()),
            SnesHeader::Swc => {
                header[..2].copy_from_slice(&units.to_le_bytes());
                header[8..11].copy_from_slice(&[0xAA, 0xBB, 0x04]);
            }
            SnesHeader::Fig => {
                header[..2].copy_from_slice(&units.to_le_bytes());
                header[4] = 0x77;
                header[5] = 0x83;
            }
            SnesHeader::Ufo => {
                header[..2].copy_from_slice(&units.to_le_bytes());
                header[8..16].copy_from_slice(b"SUPERUFO");
            }
            SnesHeader::Gd7 => {
                header[..16].copy_from_slice(b"GAME DOCTOR SF 3");
                header[16] = (rom_len / (128 * 1024)).min(255) as u8;
            }
        }
        header
    }
}

impl Convert for SnesConverter {
    fn label(&self) -> String {
        let note = match self.target {
            SnesHeader::Fig | SnesHeader::Ufo | SnesHeader::Gd7 => " (best-effort header)",
            _ => "",
        };
        format!("snes: to {}{note}", self.output_extension())
    }

    fn output_extension(&self) -> &'static str {
        match self.target {
            SnesHeader::Sfc => "sfc",
            SnesHeader::Smc => "smc",
            SnesHeader::Fig => "fig",
            SnesHeader::Swc => "swc",
            SnesHeader::Ufo => "ufo",
            SnesHeader::Gd7 => "078",
        }
    }

    fn shape(&mut self, input_len: u64, _head: &[u8]) -> Result<Shape, ConvertError> {
        // Only the two layouts nearly every dump uses are wired up; the other copier headers
        // stay placeholders until their exact bytes are pinned down and tested.
        if matches!(self.target, SnesHeader::Fig | SnesHeader::Ufo | SnesHeader::Gd7) {
            return Err(ConvertError::Failed(format!("{} headers are not implemented yet (placeholder)", self.output_extension())));
        }
        let headered = snes_copier_header(input_len, self.known_headered).map_err(ConvertError::Failed)?;
        if self.target == SnesHeader::Sfc && !headered {
            return Err(ConvertError::NotApplicable("already a bare ROM".to_string()));
        }
        let strip_head = if headered { SNES_COPIER_HEADER_LEN } else { 0 };
        let rom_len = input_len.saturating_sub(strip_head);
        Ok(Shape::Streaming { strip_head, prefix: Self::header(self.target, rom_len), kernels: Vec::new(), suffix: Vec::new() })
    }

    fn output_len(&self, input_len: u64) -> u64 {
        let rom_len = if self.currently_headered(input_len) { input_len.saturating_sub(SNES_COPIER_HEADER_LEN) } else { input_len };
        if self.target == SnesHeader::Sfc {
            rom_len
        } else {
            rom_len + SNES_COPIER_HEADER_LEN
        }
    }
}

// ---------------------------------------------------------------------------
// N64 byte order
// ---------------------------------------------------------------------------

/// Converts between the three N64 ROM byte orders. The current order comes from detection
/// (`RomMetadata::byte_order`) or the first four bytes (`80 37 12 40` z64, `37 80 40 12`
/// v64, `40 12 37 80` n64).
pub struct N64Converter {
    pub target: N64Type,
    pub known_order: Option<RomByteOrder>,
}

fn order_of(t: N64Type) -> RomByteOrder {
    match t {
        N64Type::BigEndian => RomByteOrder::BigEndian,
        N64Type::ByteSwapped => RomByteOrder::ByteSwapped,
        N64Type::LittleEndian => RomByteOrder::LittleEndian,
    }
}

/// Kernels taking `from` to big-endian; the same kernels take big-endian to `from`
/// (each is its own inverse).
fn to_big_endian(from: RomByteOrder) -> Vec<Kernel> {
    match from {
        RomByteOrder::BigEndian => Vec::new(),
        RomByteOrder::ByteSwapped => vec![Kernel::Swap16],
        RomByteOrder::LittleEndian => vec![Kernel::Reverse32],
    }
}

impl N64Converter {
    fn current_order(&self, head: &[u8]) -> Option<RomByteOrder> {
        if let Some(order) = self.known_order {
            return Some(order);
        }
        match head.get(..4)? {
            [0x80, 0x37, 0x12, 0x40] => Some(RomByteOrder::BigEndian),
            [0x37, 0x80, 0x40, 0x12] => Some(RomByteOrder::ByteSwapped),
            [0x40, 0x12, 0x37, 0x80] => Some(RomByteOrder::LittleEndian),
            _ => None,
        }
    }
}

impl Convert for N64Converter {
    fn label(&self) -> String {
        format!("n64: to {}", self.output_extension())
    }

    fn output_extension(&self) -> &'static str {
        match self.target {
            N64Type::BigEndian => "z64",
            N64Type::ByteSwapped => "v64",
            N64Type::LittleEndian => "n64",
        }
    }

    fn shape(&mut self, _input_len: u64, head: &[u8]) -> Result<Shape, ConvertError> {
        let current = self.current_order(head).ok_or_else(|| ConvertError::Failed("N64 byte order of the input is unknown".to_string()))?;
        let target = order_of(self.target);
        if current == target {
            return Err(ConvertError::NotApplicable(format!("already {target:?}")));
        }
        let mut kernels = to_big_endian(current);
        kernels.extend(to_big_endian(target));
        // Swap16 then Reverse32 (or the reverse) is a 16-bit word swap; keep both steps --
        // they're cheap and the composition is exact either way.
        Ok(Shape::Streaming { strip_head: 0, prefix: Vec::new(), kernels, suffix: Vec::new() })
    }

    fn output_len(&self, input_len: u64) -> u64 {
        input_len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(convert: Box<dyn Convert>, input: &[u8], chunk: usize) -> Vec<u8> {
        let mut conv = StreamConverter::new(convert, input.len() as u64, &input[..input.len().min(16)]).unwrap();
        let mut out = Vec::new();
        for piece in input.chunks(chunk) {
            for c in conv.push(piece).unwrap() {
                out.extend_from_slice(&c);
            }
        }
        for c in conv.finish().unwrap() {
            out.extend_from_slice(&c);
        }
        out
    }

    #[test]
    fn n64_orders_round_trip_across_odd_chunks() {
        let z64: Vec<u8> = [0x80u8, 0x37, 0x12, 0x40].iter().copied().chain((0..1020u32).map(|i| (i * 7 % 251) as u8)).collect();
        let v64 = run(Box::new(N64Converter { target: N64Type::ByteSwapped, known_order: None }), &z64, 3);
        assert_eq!(&v64[..4], &[0x37, 0x80, 0x40, 0x12]);
        let n64 = run(Box::new(N64Converter { target: N64Type::LittleEndian, known_order: None }), &v64, 5);
        assert_eq!(&n64[..4], &[0x40, 0x12, 0x37, 0x80]);
        let back = run(Box::new(N64Converter { target: N64Type::BigEndian, known_order: None }), &n64, 7);
        assert_eq!(back, z64);
    }

    #[test]
    fn n64_same_order_is_not_applicable() {
        let z64 = [0x80u8, 0x37, 0x12, 0x40, 0, 0, 0, 0];
        let err = StreamConverter::new(Box::new(N64Converter { target: N64Type::BigEndian, known_order: None }), 8, &z64).err().unwrap();
        assert!(matches!(err, ConvertError::NotApplicable(_)));
    }

    #[test]
    fn snes_strip_and_add_headers() {
        let rom: Vec<u8> = (0..(32 * 1024u32)).map(|i| (i % 256) as u8).collect();
        let smc = run(Box::new(SnesConverter { target: SnesHeader::Smc, known_headered: Some(false) }), &rom, 4096);
        assert_eq!(smc.len(), rom.len() + 512);
        assert_eq!(&smc[..2], &[4, 0]); // 32 KiB = 4 units of 8 KiB
        assert_eq!(&smc[512..], &rom[..]);
        let sfc = run(Box::new(SnesConverter { target: SnesHeader::Sfc, known_headered: None }), &smc, 1000);
        assert_eq!(sfc, rom);
        let swc = run(Box::new(SnesConverter { target: SnesHeader::Swc, known_headered: None }), &smc, 100);
        assert_eq!(&swc[8..11], &[0xAA, 0xBB, 0x04]);
        assert_eq!(&swc[512..], &rom[..]);
    }

    #[test]
    fn fds_header_add_and_strip() {
        let sides: Vec<u8> = vec![0x5A; FDS_SIDE_LEN as usize * 2];
        let headered = run(Box::new(FdsConverter { target: NesHeader::Headered, known_headered: None }), &sides, 10_000);
        assert_eq!(&headered[..4], b"FDS\x1A");
        assert_eq!(headered[4], 2);
        assert_eq!(headered.len(), sides.len() + 16);
        let stripped = run(Box::new(FdsConverter { target: NesHeader::Unheadered, known_headered: None }), &headered, 7);
        assert_eq!(stripped, sides);
    }
}
