//! SNES music read from the ROM itself, with no rip.
//!
//! The SNES CPU cannot reach the sound chip's memory (ARAM) directly: every game sends its sound
//! driver, songs and samples through four I/O ports, to a boot ROM in the sound chip that takes
//! blocks of `(u16 length, u16 address, bytes)` and, on a block of length 0, jumps to that
//! block's address. Most games keep those blocks in the ROM exactly as they are sent, one
//! after another ("upload chains"), so scanning the ROM for them rebuilds the sound memory the
//! game would build: the same image an `.spc` rip is a snapshot of.
//!
//! This module finds the chains and lays them into an ARAM image. Which chains belong together
//! (driver, then the songs of one stage), and how a driver is told which song to play, depend
//! on the driver; that is decided per driver on top of this.

/// The sound chip's memory.
pub const ARAM_SIZE: usize = 0x10000;

/// I/O registers in ARAM; an upload never writes here.
const IO: std::ops::Range<usize> = 0xF0..0x100;
/// A chain with fewer bytes than this is noise that happened to parse.
const MIN_CHAIN_BYTES: usize = 256;
/// Real uploads send code, songs and samples, not a few loose bytes per block.
const MIN_BLOCK_BYTES: usize = 16;
/// More blocks than this in one chain is not a real upload.
const MAX_BLOCKS: usize = 256;

/// A ROM image without the 512-byte header some copier dumps carry.
pub fn rom_image(file: &[u8]) -> &[u8] {
    if file.len() % 0x400 == 0x200 {
        &file[0x200..]
    } else {
        file
    }
}

/// One block of a chain: where it lands in ARAM, and where its bytes are in the ROM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    pub aram: u16,
    pub rom: usize,
    pub len: usize,
}

/// One upload as the game sends it: blocks, then the address the sound chip jumps to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chain {
    /// Where the chain starts in the ROM image.
    pub offset: usize,
    pub blocks: Vec<Block>,
    /// Where the sound chip starts running when the upload ends: the driver's entry point.
    pub entry: u16,
}

impl Chain {
    /// Bytes the chain uploads.
    pub fn bytes(&self) -> usize {
        self.blocks.iter().map(|b| b.len).sum()
    }

    /// Where the chain ends in the ROM image (after its terminator).
    pub fn end(&self) -> usize {
        self.blocks.last().map(|b| b.rom + b.len).unwrap_or(self.offset) + 4
    }

    /// Whether the chain uploads code at its own entry point: a driver, not only song data.
    pub fn holds_entry(&self) -> bool {
        self.blocks.iter().any(|b| (b.aram as usize..b.aram as usize + b.len).contains(&(self.entry as usize)))
    }
}

fn u16_at(rom: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*rom.get(at)?, *rom.get(at + 1)?]))
}

/// The chain starting at `at`, if the bytes there parse as one: blocks of real size that fit
/// in ARAM, stay clear of its I/O registers and don't overlap or hold only filler, ending in a
/// terminator whose entry point lands on uploaded code.
pub fn chain_at(rom: &[u8], at: usize) -> Option<Chain> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut pos = at;
    loop {
        let len = u16_at(rom, pos)? as usize;
        let aram = u16_at(rom, pos + 2)?;
        if len == 0 {
            let chain = Chain { offset: at, blocks, entry: aram };
            let entry_byte = chain.blocks.iter().find(|b| (b.aram as usize..b.aram as usize + b.len).contains(&(aram as usize))).map(|b| rom[b.rom + (aram - b.aram) as usize]);
            let ok = chain.bytes() >= MIN_CHAIN_BYTES && (0x0200..0xFFC0).contains(&(aram as usize)) && entry_byte.is_some_and(|op| op != 0x00 && op != 0xFF);
            return ok.then_some(chain);
        }
        let start = aram as usize;
        if len < MIN_BLOCK_BYTES || start + len > ARAM_SIZE || (start < IO.end && start + len > IO.start) || pos + 4 + len > rom.len() || blocks.len() >= MAX_BLOCKS {
            return None;
        }
        if blocks.iter().any(|b| start < b.aram as usize + b.len && (b.aram as usize) < start + len) || filler(&rom[pos + 4..pos + 4 + len]) {
            return None;
        }
        blocks.push(Block { aram, rom: pos + 4, len });
        pos += 4 + len;
    }
}

/// Whether bytes are mostly 00 or FF: erased space, not something a game sends.
fn filler(bytes: &[u8]) -> bool {
    bytes.iter().filter(|&&b| b == 0x00 || b == 0xFF).count() * 10 >= bytes.len() * 9
}

/// The addresses the SNES CPU sees a ROM offset at, as the three bytes a pointer to it holds,
/// for both cartridge layouts: LoROM (32 KB banks at $8000) and HiROM (64 KB banks at $C0).
fn bus_addresses(offset: usize) -> [[u8; 3]; 3] {
    let lo_bank = (offset / 0x8000) as u8;
    let lo_addr = (0x8000 + offset % 0x8000) as u16;
    let hi_bank = (offset / 0x10000) as u8;
    let hi_addr = (offset % 0x10000) as u16;
    let le = |a: u16, bank: u8| [a as u8, (a >> 8) as u8, bank];
    [le(lo_addr, lo_bank), le(lo_addr, lo_bank | 0x80), le(hi_addr, hi_bank | 0xC0)]
}

/// Whether the ROM holds a pointer to the chain's start: the game's upload routine is handed
/// its address, so a chain nothing points at is probably data that happens to parse.
pub fn referenced(rom: &[u8], chain: &Chain) -> bool {
    let wanted = bus_addresses(chain.offset);
    rom.windows(3).any(|w| wanted.iter().any(|p| w == p))
}

/// Every upload chain in a ROM image, in ROM order. A chain found inside another is not
/// listed again.
pub fn find_chains(rom: &[u8]) -> Vec<Chain> {
    let mut chains = Vec::new();
    let mut at = 0;
    while at + 4 <= rom.len() {
        match chain_at(rom, at) {
            Some(chain) => {
                at = chain.end();
                chains.push(chain);
            }
            None => at += 1,
        }
    }
    chains
}

/// The chains that carry a driver: they upload code at their own entry point. Those the ROM
/// points at come first, then the largest. Song and sample uploads sent later to that driver
/// end in the same entry, so they are grouped by it.
pub fn drivers<'a>(rom: &[u8], chains: &'a [Chain]) -> Vec<&'a Chain> {
    let candidates: Vec<&Chain> = chains.iter().filter(|c| c.holds_entry()).collect();
    // One pass over the ROM for every candidate's pointers.
    let mut wanted: std::collections::HashMap<[u8; 3], Vec<usize>> = std::collections::HashMap::new();
    for (i, c) in candidates.iter().enumerate() {
        for p in bus_addresses(c.offset) {
            wanted.entry(p).or_default().push(i);
        }
    }
    let mut pointed = vec![false; candidates.len()];
    for w in rom.windows(3) {
        if let Some(ids) = wanted.get(w) {
            ids.iter().for_each(|&i| pointed[i] = true);
        }
    }
    let mut found: Vec<(&Chain, bool)> = candidates.into_iter().zip(pointed).collect();
    found.sort_by_key(|(c, r)| (!r, std::cmp::Reverse(c.bytes())));
    found.into_iter().map(|(c, _)| c).collect()
}

/// An ARAM image built by laying chains in order, with which bytes were written.
pub struct Aram {
    pub ram: Vec<u8>,
    pub written: Vec<bool>,
}

impl Default for Aram {
    fn default() -> Self {
        Self { ram: vec![0; ARAM_SIZE], written: vec![false; ARAM_SIZE] }
    }
}

impl Aram {
    pub fn apply(&mut self, rom: &[u8], chain: &Chain) {
        for b in &chain.blocks {
            let a = b.aram as usize;
            self.ram[a..a + b.len].copy_from_slice(&rom[b.rom..b.rom + b.len]);
            self.written[a..a + b.len].iter_mut().for_each(|w| *w = true);
        }
    }

    /// Of the bytes written here, the share equal to `other`'s at the same address (a rip's
    /// ARAM): how much of a snapshot this rebuild explains.
    pub fn agreement(&self, other: &[u8]) -> (usize, usize) {
        let mut same = 0;
        let mut total = 0;
        for (a, w) in self.written.iter().enumerate() {
            if *w && a < other.len() {
                total += 1;
                same += usize::from(self.ram[a] == other[a]);
            }
        }
        (same, total)
    }

    /// An `.spc` file that starts the driver at `entry`, as the sound chip would right after the
    /// upload. `ports` are the values the CPU has written to the four I/O ports (a driver reads
    /// its song number there).
    pub fn to_spc(&self, entry: u16, ports: [u8; 4]) -> Vec<u8> {
        let mut spc = vec![0u8; 0x10200];
        spc[..33].copy_from_slice(b"SNES-SPC700 Sound File Data v0.30");
        spc[0x21] = 0x1A;
        spc[0x22] = 0x1A;
        spc[0x23] = 27; // no ID666 tag
        spc[0x24] = 30;
        spc[0x25..0x27].copy_from_slice(&entry.to_le_bytes());
        spc[0x2A] = 0x02; // PSW: as the boot ROM leaves it
        spc[0x2B] = 0xEF; // SP: the boot ROM's stack
        let ram = &mut spc[0x100..0x10100];
        ram.copy_from_slice(&self.ram);
        ram[0xF1] = 0x80; // boot ROM mapped in, timers off
        ram[0xF4..0xF8].copy_from_slice(&ports);
        // DSP: muted with echo off until the driver sets it up.
        let dsp = &mut spc[0x10100..0x10180];
        dsp[0x6C] = 0xE0;
        // The file's last 64 bytes are the RAM under the boot ROM, not the boot ROM itself.
        spc[0x101C0..0x10200].copy_from_slice(&self.ram[0xFFC0..]);
        spc
    }
}

/// A rip's ARAM, from an `.spc` file.
pub fn spc_aram(spc: &[u8]) -> Option<&[u8]> {
    (spc.len() >= 0x10100 && spc.starts_with(b"SNES-SPC700 Sound File Data")).then(|| &spc[0x100..0x10100])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(aram: u16, data: &[u8]) -> Vec<u8> {
        let mut v = (data.len() as u16).to_le_bytes().to_vec();
        v.extend(aram.to_le_bytes());
        v.extend(data);
        v
    }

    fn rom_with_chain() -> (Vec<u8>, usize) {
        let mut rom = vec![0xFFu8; 1000];
        let at = rom.len();
        // The ROM points at the chain (a LoROM address in bank 0).
        rom[10..13].copy_from_slice(&[(0x8000 + at) as u8, ((0x8000 + at) >> 8) as u8, 0x80]);
        rom.extend(block(0x0500, &[0x8F; 200])); // driver code
        rom.extend(block(0x2000, &[0x11; 100])); // song data
        rom.extend(block(0x0000, &[]).iter().take(2).copied());
        rom.extend(0x0500u16.to_le_bytes());
        rom.extend(vec![0xFF; 500]);
        (rom, at)
    }

    #[test]
    fn a_chain_is_found_and_laid_into_aram() {
        let (rom, at) = rom_with_chain();
        let chains = find_chains(&rom);
        assert_eq!(chains.len(), 1);
        let c = &chains[0];
        assert_eq!((c.offset, c.entry, c.bytes()), (at, 0x0500, 300));
        assert!(c.holds_entry());
        assert!(referenced(&rom, c));
        assert_eq!(drivers(&rom, &chains).len(), 1);
        let mut aram = Aram::default();
        aram.apply(&rom, c);
        assert_eq!(aram.ram[0x0500], 0x8F);
        assert_eq!(aram.ram[0x2063], 0x11);
        assert!(!aram.written[0x2064]);
        let spc = aram.to_spc(c.entry, [1, 0, 0, 0]);
        assert_eq!(spc_aram(&spc).unwrap()[0x0500], 0x8F);
        assert_eq!(&spc[0x25..0x27], &[0x00, 0x05]);
    }

    #[test]
    fn noise_and_io_writes_are_not_chains() {
        // A block over the I/O registers is not an upload.
        let mut rom = block(0x00E0, &[0; 100]);
        rom.extend([0, 0, 0x00, 0x05]);
        assert!(find_chains(&rom).is_empty());
        // Too small to be real.
        let mut rom = block(0x0500, &[0x8F; 100]);
        rom.extend([0, 0, 0x00, 0x05]);
        assert!(find_chains(&rom).is_empty());
        // Erased space is not code.
        let mut rom = block(0x0500, &[0xFF; 400]);
        rom.extend([0, 0, 0x00, 0x05]);
        assert!(find_chains(&rom).is_empty());
        assert!(find_chains(&[0xFF; 4096]).is_empty());
    }

    #[test]
    fn copier_headers_are_dropped() {
        assert_eq!(rom_image(&[0u8; 0x8200]).len(), 0x8000);
        assert_eq!(rom_image(&[0u8; 0x8000]).len(), 0x8000);
    }
}
