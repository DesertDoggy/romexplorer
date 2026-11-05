//! SNES music captured by running the game itself: its main CPU ([`super::w65816`]) on a
//! minimal SNES (memory map, DMA, the math and timing registers, nothing drawn), wired to an
//! emulated sound chip ([`super::snes_apu`]). Whatever the game does to get its music into the
//! sound chip, unpacking included, it does here, and the sound chip's state afterwards is the
//! same snapshot an `.spc` rip is.
//!
//! What a capture keeps: the snapshot, the moments the sound chip started a newly uploaded
//! driver, and the values the game wrote to the ports (its commands to the driver).

use super::snes_apu::{Apu, CLOCK_RATE};
use super::snes_sound::rom_image;
use super::w65816::{Bus, Cpu};

/// Master clocks (21.477 MHz) per scanline, and scanlines per frame (NTSC).
const LINE_CLOCKS: u32 = 1364;
const LINES: u32 = 262;
/// The first line of vertical blank.
const VBLANK_LINE: u32 = 225;
const MASTER_RATE: u64 = 21_477_272;
/// Master clocks per CPU cycle: 8 for most memory, 6 for internal cycles, I/O and fast ROM.
/// Slow code averages about 7.5 (two cycles of 15 clocks).
const SLOW_PAIR_CLOCKS: u32 = 15;
const FAST_CLOCKS: u32 = 6;

/// How the cartridge maps its ROM onto the bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mapping {
    LoRom,
    HiRom,
    ExHiRom,
}

/// Which mapping the ROM's internal header says, by where a consistent header is found.
pub fn mapping(rom: &[u8]) -> Option<Mapping> {
    let score = |at: usize, mode: u8| -> i32 {
        let Some(h) = rom.get(at..at + 0x40) else { return i32::MIN };
        let mut s = 0;
        let checksum = u16::from_le_bytes([h[0x1E], h[0x1F]]);
        let complement = u16::from_le_bytes([h[0x1C], h[0x1D]]);
        if checksum ^ complement == 0xFFFF {
            s += 4;
        }
        if h[0x15] & 0x0F == mode {
            s += 2;
        }
        let reset = u16::from_le_bytes([h[0x3C], h[0x3D]]);
        if reset >= 0x8000 {
            s += 2;
        } else {
            s -= 4;
        }
        if h[0..21].iter().all(|b| (0x20..0x7F).contains(b)) {
            s += 1;
        }
        s
    };
    let candidates = [(0x7FC0, 0x0, Mapping::LoRom), (0xFFC0, 0x1, Mapping::HiRom), (0x40FFC0, 0x5, Mapping::ExHiRom)];
    candidates.iter().map(|&(at, mode, m)| (score(at, mode), m)).filter(|(s, _)| *s > 0).max_by_key(|(s, _)| *s).map(|(_, m)| m)
}

/// The cartridge's map mode byte says which chip, if any, runs the game instead of the SNES CPU
/// (SA-1, Super FX): those games are not captured.
fn coprocessor_cpu(rom: &[u8], mapping: Mapping) -> bool {
    let header = match mapping {
        Mapping::LoRom => 0x7FC0,
        Mapping::HiRom => 0xFFC0,
        Mapping::ExHiRom => 0x40FFC0,
    };
    let (Some(&mode), Some(&chips)) = (rom.get(header + 0x15), rom.get(header + 0x16)) else { return false };
    // SA-1 map mode, or a Super FX / SA-1 chipset value.
    mode & 0x0F == 0x3 || (0x13..=0x1A).contains(&chips) || (0x33..=0x36).contains(&chips)
}

/// The cartridge's save RAM, by its header: none, or 2^n KB. A game checks that save RAM it
/// does not have is not there, since copiers had it: giving every game some would stop those
/// games at a copy-protection screen.
fn save_ram_size(rom: &[u8], mapping: Mapping) -> usize {
    let header = match mapping {
        Mapping::LoRom => 0x7FC0,
        Mapping::HiRom => 0xFFC0,
        Mapping::ExHiRom => 0x40FFC0,
    };
    match rom.get(header + 0x18) {
        Some(&n) if (1..=8).contains(&n) => 1024 << n,
        _ => 0,
    }
}

/// The S-DD1 decompression chip (chipset $43 or $45).
fn s_dd1(rom: &[u8], mapping: Mapping) -> bool {
    let header = if mapping == Mapping::LoRom { 0x7FC0 } else { 0xFFC0 };
    matches!(rom.get(header + 0x16), Some(0x43 | 0x45))
}

#[derive(Debug, Clone, Copy, Default)]
struct Dma {
    params: u8,
    b_addr: u8,
    a_addr: u16,
    a_bank: u8,
    count: u16,
}

/// A port write: when (master clocks since power on), which port, what.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortWrite {
    pub at: u64,
    pub port: u8,
    pub value: u8,
}

/// The SNES around the CPU.
struct Snes<'a> {
    rom: &'a [u8],
    mapping: Mapping,
    wram: Vec<u8>,
    sram: Vec<u8>,
    vram: Vec<u8>,
    apu: Apu,
    /// Master clocks not yet given to the sound chip, scaled: sound clocks × MASTER_RATE.
    apu_debt: u64,
    master: u64,
    line: u32,
    line_clock: u32,
    nmi_enable: bool,
    /// An NMI that came due, waiting for the CPU (also when it came due inside a DMA).
    nmi_due: bool,
    /// $420D: banks $80 and up run at the fast ROM speed.
    fast_rom: bool,
    /// The last value written to $4200 (interrupt enables), for diagnosis.
    nmitimen: u8,
    nmi_flag: bool,
    irq_mode: u8,
    irq_flag: bool,
    htime: u16,
    vtime: u16,
    wram_addr: u32,
    vram_addr: u16,
    vram_inc: u8,
    m7a: u16,
    m7b: u8,
    m7_latch: u8,
    mul_a: u8,
    dividend: u16,
    quotient: u16,
    product_or_rest: u16,
    dma: [Dma; 8],
    open_bus: u8,
    writes: Vec<PortWrite>,
    /// What the main CPU read from the ports, each time a port's value changed.
    replies: Vec<PortWrite>,
    last_reply: [Option<u8>; 4],
    /// Buttons held on pad 1, as $4219:$4218 read them (B Y Select Start Up Down Left Right,
    /// then A X L R).
    buttons: u16,
    /// The pad's serial shift register, for games that read $4016 bit by bit.
    serial: u16,
    strobe: bool,
    /// I/O registers read since `watch` was last cleared, for telling what a stuck game polls.
    watch: Option<std::collections::HashMap<u16, u32>>,
}

/// Port writes kept at most; a game that streams samples writes far more, and only the first
/// commands matter for telling songs apart.
const MAX_PORT_WRITES: usize = 1 << 20;

impl Snes<'_> {
    fn rom_byte(&self, offset: usize) -> u8 {
        if self.rom.is_empty() {
            return 0;
        }
        self.rom[offset % self.rom.len()]
    }

    /// The cartridge: ROM or save RAM, or `None` where the address reaches neither.
    fn cart(&mut self, bank: u8, a: u16) -> Option<CartAt> {
        let b = bank & 0x7F;
        match self.mapping {
            Mapping::LoRom => {
                if (0x70..0x7E).contains(&b) && a < 0x8000 {
                    return Some(CartAt::Sram(((b as usize - 0x70) << 15) | a as usize));
                }
                if a >= 0x8000 || (0x40..0x7E).contains(&b) {
                    return Some(CartAt::Rom((b as usize) << 15 | (a as usize & 0x7FFF)));
                }
                None
            }
            Mapping::HiRom | Mapping::ExHiRom => {
                if (0x20..0x40).contains(&b) && (0x6000..0x8000).contains(&a) {
                    return Some(CartAt::Sram(((b as usize - 0x20) << 13) | (a as usize - 0x6000)));
                }
                if b >= 0x40 || a >= 0x8000 {
                    let mut offset = ((b as usize & 0x3F) << 16) | a as usize;
                    if self.mapping == Mapping::ExHiRom && bank < 0x80 {
                        offset |= 0x40_0000;
                    }
                    return Some(CartAt::Rom(offset));
                }
                None
            }
        }
    }

    fn catch_up_apu(&mut self) {
        let clocks = self.apu_debt / MASTER_RATE;
        if clocks > 0 {
            self.apu.advance(clocks as i32);
            self.apu_debt -= clocks * MASTER_RATE;
        }
    }

    fn read_io(&mut self, a: u16) -> u8 {
        if let Some(watch) = &mut self.watch {
            *watch.entry(a).or_default() += 1;
        }
        match a {
            0x2134..=0x2136 => {
                let p = (self.m7a as i16 as i32 * self.m7b as i8 as i32) as u32;
                (p >> ((a - 0x2134) * 8)) as u8
            }
            0x2139 | 0x213A => {
                let v = self.vram[(self.vram_addr as usize * 2 + (a - 0x2139) as usize) & 0xFFFF];
                let high = a == 0x213A;
                if high == (self.vram_inc & 0x80 != 0) {
                    self.vram_addr = self.vram_addr.wrapping_add(self.vram_step());
                }
                v
            }
            0x213E => 0x01,
            0x213F => 0x01,
            0x2140..=0x217F => {
                self.catch_up_apu();
                let port = a as usize & 3;
                let v = self.apu.read_port_now(port);
                if self.last_reply[port] != Some(v) && self.replies.len() < MAX_PORT_WRITES {
                    self.last_reply[port] = Some(v);
                    self.replies.push(PortWrite { at: self.master, port: port as u8, value: v });
                }
                v
            }
            0x2180 => {
                let v = self.wram[self.wram_addr as usize & 0x1FFFF];
                self.wram_addr = (self.wram_addr + 1) & 0x1FFFF;
                v
            }
            0x4016 => {
                let bit = if self.strobe { self.buttons >> 15 } else { self.serial >> 15 };
                if !self.strobe {
                    self.serial = self.serial << 1 | 1;
                }
                bit as u8 & 1
            }
            0x4017 => 0,
            0x4210 => {
                let v = if self.nmi_flag { 0x82 } else { 0x02 };
                self.nmi_flag = false;
                v
            }
            0x4211 => {
                let v = if self.irq_flag { 0x80 } else { 0 };
                self.irq_flag = false;
                v
            }
            0x4212 => {
                let vblank = self.line >= VBLANK_LINE;
                let hblank = self.line_clock >= 1096;
                let joypad = vblank && self.line < VBLANK_LINE + 3;
                (vblank as u8) << 7 | (hblank as u8) << 6 | joypad as u8
            }
            0x4214 => self.quotient as u8,
            0x4215 => (self.quotient >> 8) as u8,
            0x4216 => self.product_or_rest as u8,
            0x4217 => (self.product_or_rest >> 8) as u8,
            0x4218 => self.buttons as u8,
            0x4219 => (self.buttons >> 8) as u8,
            0x421A..=0x421F => 0,
            0x4300..=0x437F => {
                let d = &self.dma[(a as usize >> 4) & 7];
                match a & 0xF {
                    0x0 => d.params,
                    0x1 => d.b_addr,
                    0x2 => d.a_addr as u8,
                    0x3 => (d.a_addr >> 8) as u8,
                    0x4 => d.a_bank,
                    0x5 => d.count as u8,
                    0x6 => (d.count >> 8) as u8,
                    _ => self.open_bus,
                }
            }
            _ => self.open_bus,
        }
    }

    fn vram_step(&self) -> u16 {
        match self.vram_inc & 3 {
            0 => 1,
            1 => 32,
            _ => 128,
        }
    }

    fn write_io(&mut self, a: u16, v: u8) {
        match a {
            0x4016 => {
                self.strobe = v & 1 != 0;
                if self.strobe {
                    self.serial = self.buttons;
                }
            }
            0x2115 => self.vram_inc = v,
            0x2116 => self.vram_addr = (self.vram_addr & 0xFF00) | v as u16,
            0x2117 => self.vram_addr = (self.vram_addr & 0x00FF) | (v as u16) << 8,
            0x2118 | 0x2119 => {
                self.vram[(self.vram_addr as usize * 2 + (a - 0x2118) as usize) & 0xFFFF] = v;
                if (a == 0x2119) == (self.vram_inc & 0x80 != 0) {
                    self.vram_addr = self.vram_addr.wrapping_add(self.vram_step());
                }
            }
            0x211B => {
                self.m7a = (v as u16) << 8 | self.m7_latch as u16;
                self.m7_latch = v;
            }
            0x211C => self.m7b = v,
            0x2140..=0x217F => {
                self.catch_up_apu();
                let port = a as usize & 3;
                self.apu.write_port_now(port, v);
                if self.writes.len() < MAX_PORT_WRITES {
                    self.writes.push(PortWrite { at: self.master, port: port as u8, value: v });
                }
            }
            0x2180 => {
                self.wram[self.wram_addr as usize & 0x1FFFF] = v;
                self.wram_addr = (self.wram_addr + 1) & 0x1FFFF;
            }
            0x2181 => self.wram_addr = (self.wram_addr & 0x1FF00) | v as u32,
            0x2182 => self.wram_addr = (self.wram_addr & 0x100FF) | (v as u32) << 8,
            0x2183 => self.wram_addr = (self.wram_addr & 0x0FFFF) | ((v as u32 & 1) << 16),
            0x4200 => {
                self.nmitimen = v;
                self.nmi_enable = v & 0x80 != 0;
                self.irq_mode = (v >> 4) & 3;
                if self.irq_mode == 0 {
                    self.irq_flag = false;
                }
            }
            0x4202 => self.mul_a = v,
            0x4203 => {
                self.product_or_rest = self.mul_a as u16 * v as u16;
            }
            0x4204 => self.dividend = (self.dividend & 0xFF00) | v as u16,
            0x4205 => self.dividend = (self.dividend & 0x00FF) | (v as u16) << 8,
            0x4206 => {
                if v == 0 {
                    self.quotient = 0xFFFF;
                    self.product_or_rest = self.dividend;
                } else {
                    self.quotient = self.dividend / v as u16;
                    self.product_or_rest = self.dividend % v as u16;
                }
            }
            0x4207 => self.htime = (self.htime & 0x100) | v as u16,
            0x4208 => self.htime = (self.htime & 0xFF) | ((v as u16 & 1) << 8),
            0x4209 => self.vtime = (self.vtime & 0x100) | v as u16,
            0x420A => self.vtime = (self.vtime & 0xFF) | ((v as u16 & 1) << 8),
            0x420B => self.run_dma(v),
            0x420D => self.fast_rom = v & 1 != 0,
            0x4300..=0x437F => {
                let d = &mut self.dma[(a as usize >> 4) & 7];
                match a & 0xF {
                    0x0 => d.params = v,
                    0x1 => d.b_addr = v,
                    0x2 => d.a_addr = (d.a_addr & 0xFF00) | v as u16,
                    0x3 => d.a_addr = (d.a_addr & 0x00FF) | (v as u16) << 8,
                    0x4 => d.a_bank = v,
                    0x5 => d.count = (d.count & 0xFF00) | v as u16,
                    0x6 => d.count = (d.count & 0x00FF) | (v as u16) << 8,
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// The opcode the CPU runs next, read without side effects where that matters (I/O is
    /// never code).
    fn peek_opcode(&mut self, cpu: &Cpu) -> u8 {
        let bank = cpu.pbr;
        let a = cpu.pc;
        if bank & 0x7F < 0x40 && (0x2000..0x6000).contains(&a) {
            return 0xFF;
        }
        let open = self.open_bus;
        let v = self.read((bank as u32) << 16 | a as u32);
        self.open_bus = open;
        v
    }

    fn run_dma(&mut self, channels: u8) {
        const PATTERNS: [&[u8]; 8] = [&[0], &[0, 1], &[0, 0], &[0, 0, 1, 1], &[0, 1, 2, 3], &[0, 1, 0, 1], &[0, 0], &[0, 0, 1, 1]];
        for c in 0..8 {
            if channels & (1 << c) == 0 {
                continue;
            }
            let mut d = self.dma[c];
            let pattern = PATTERNS[d.params as usize & 7];
            let mut n: u32 = if d.count == 0 { 0x10000 } else { d.count as u32 };
            let mut i = 0;
            while n > 0 {
                let b = 0x2100 | d.b_addr.wrapping_add(pattern[i % pattern.len()]) as u16;
                let a = (d.a_bank as u32) << 16 | d.a_addr as u32;
                if d.params & 0x80 != 0 {
                    let v = self.read_io(b);
                    self.write(a, v);
                } else {
                    let v = self.read(a);
                    self.write_io(b, v);
                }
                if d.params & 0x08 == 0 {
                    d.a_addr = if d.params & 0x10 != 0 { d.a_addr.wrapping_sub(1) } else { d.a_addr.wrapping_add(1) };
                }
                i += 1;
                n -= 1;
                if self.tick(8) {
                    self.nmi_due = true;
                }
            }
            d.count = 0;
            self.dma[c] = d;
        }
    }

    /// Moves time on by `clocks` master clocks. Returns whether an NMI is due.
    fn tick(&mut self, clocks: u32) -> bool {
        self.master += clocks as u64;
        self.apu_debt += clocks as u64 * CLOCK_RATE as u64;
        let mut nmi = false;
        let before = self.line_clock;
        self.line_clock += clocks;
        if self.irq_mode & 1 != 0 && (self.irq_mode & 2 == 0 || self.line == self.vtime as u32) {
            let h = self.htime as u32 * 4;
            if before <= h && self.line_clock > h {
                self.irq_flag = true;
            }
        }
        while self.line_clock >= LINE_CLOCKS {
            self.line_clock -= LINE_CLOCKS;
            self.line += 1;
            if self.line == LINES {
                self.line = 0;
                self.nmi_flag = false;
                self.catch_up_apu();
            }
            if self.line == VBLANK_LINE {
                self.nmi_flag = true;
                nmi = self.nmi_enable;
            }
            if self.irq_mode == 2 && self.line == self.vtime as u32 {
                self.irq_flag = true;
            }
        }
        nmi
    }
}

enum CartAt {
    Rom(usize),
    Sram(usize),
}

impl Bus for Snes<'_> {
    fn read(&mut self, addr: u32) -> u8 {
        let bank = (addr >> 16) as u8;
        let a = addr as u16;
        let v = if bank == 0x7E || bank == 0x7F {
            self.wram[(addr as usize) & 0x1FFFF]
        } else if bank & 0x7F < 0x40 && a < 0x2000 {
            self.wram[a as usize]
        } else if bank & 0x7F < 0x40 && a < 0x6000 {
            self.read_io(a)
        } else {
            match self.cart(bank, a) {
                Some(CartAt::Rom(o)) => self.rom_byte(o),
                Some(CartAt::Sram(o)) if !self.sram.is_empty() => self.sram[o % self.sram.len()],
                Some(CartAt::Sram(_)) => self.open_bus,
                None => self.open_bus,
            }
        };
        self.open_bus = v;
        v
    }

    fn write(&mut self, addr: u32, value: u8) {
        let bank = (addr >> 16) as u8;
        let a = addr as u16;
        if bank == 0x7E || bank == 0x7F {
            self.wram[(addr as usize) & 0x1FFFF] = value;
        } else if bank & 0x7F < 0x40 && a < 0x2000 {
            self.wram[a as usize] = value;
        } else if bank & 0x7F < 0x40 && a < 0x6000 {
            self.write_io(a, value);
        } else if let Some(CartAt::Sram(o)) = self.cart(bank, a) {
            let n = self.sram.len();
            if n > 0 {
                self.sram[o % n] = value;
            }
        }
    }
}

/// When the sound chip started running code from its RAM after an upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverStart {
    /// Master clocks since power on.
    pub at: u64,
    pub entry: u16,
}

/// What a run of the game left in the sound chip.
#[derive(Debug, Clone)]
pub struct Capture {
    /// The sound chip's state at the end, as an `.spc` file.
    pub spc: Vec<u8>,
    pub driver_starts: Vec<DriverStart>,
    pub port_writes: Vec<PortWrite>,
    /// The sound chip's answers as the game read them: a port's value each time it changed.
    pub port_replies: Vec<PortWrite>,
    /// The game's output during the run (stereo, 32 kHz), when asked for.
    pub audio: Vec<i16>,
    /// Instructions the main CPU ran.
    pub instructions: u64,
    /// Why the run ended early, if it did (the CPU stopped, or crashed into empty memory).
    pub stopped: Option<String>,
    /// Over the last second: where the main CPU spent its time (bank:address, samples), most
    /// first, and the I/O registers it read most. A game that never starts its music is
    /// usually polling something here.
    pub busy_at: Vec<(u32, u32)>,
    pub busy_reads: Vec<(u16, u32)>,
    /// Interrupts the CPU took, the last interrupt-enable value written to $4200, and whether
    /// the CPU ended with IRQs masked.
    pub nmis: u32,
    pub irqs: u32,
    pub nmitimen: u8,
    pub irq_masked: bool,
    /// The last instructions (bank:address) before the CPU first ran into the vector table or
    /// a BRK, oldest first, when it did.
    pub crash_trace: Option<Vec<u32>>,
}

/// How a capture runs.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Console time to run.
    pub seconds: f32,
    /// Keep what the sound chip played.
    pub keep_audio: bool,
    /// Press Start now and then, for games whose music waits for it past an intro.
    pub press_start: bool,
}

/// When Start is pressed: from 2 s on, for 0.1 s every 1.5 s.
fn start_held(master: u64) -> bool {
    let t = master as f64 / MASTER_RATE as f64;
    t >= 2.0 && (t - 2.0) % 1.5 < 0.1
}

const START: u16 = 0x1000;

/// Runs the game from power on and returns what it left in the sound chip.
pub fn capture(file: &[u8], options: &Options) -> Result<Capture, String> {
    let Options { seconds, keep_audio, press_start } = *options;
    let rom = rom_image(file);
    let mapping = mapping(rom).ok_or("no SNES header found")?;
    if coprocessor_cpu(rom, mapping) {
        return Err("the game runs on a coprocessor CPU (SA-1 or Super FX), which is not emulated".into());
    }
    if s_dd1(rom, mapping) {
        return Err("the game unpacks its data with the S-DD1 chip, which is not emulated".into());
    }
    let mut apu = Apu::new()?;
    apu.keep_output = keep_audio;
    let mut snes = Snes {
        rom,
        mapping,
        wram: vec![0; 0x20000],
        sram: vec![0; save_ram_size(rom, mapping)],
        vram: vec![0; 0x10000],
        apu,
        apu_debt: 0,
        master: 0,
        line: 0,
        line_clock: 0,
        nmi_enable: false,
        nmi_due: false,
        fast_rom: false,
        nmitimen: 0,
        nmi_flag: false,
        irq_mode: 0,
        irq_flag: false,
        htime: 0x1FF,
        vtime: 0x1FF,
        wram_addr: 0,
        vram_addr: 0,
        vram_inc: 0,
        m7a: 0,
        m7b: 0,
        m7_latch: 0,
        mul_a: 0xFF,
        dividend: 0xFFFF,
        quotient: 0,
        product_or_rest: 0,
        dma: [Dma::default(); 8],
        open_bus: 0,
        writes: Vec::new(),
        replies: Vec::new(),
        last_reply: [None; 4],
        buttons: 0,
        serial: 0,
        strobe: false,
        watch: None,
    };
    let mut cpu = Cpu::reset(&mut snes);
    let end = (seconds.max(0.0) as f64 * MASTER_RATE as f64) as u64;
    let mut driver_starts = Vec::new();
    let mut in_boot_rom = true;
    let mut instructions = 0u64;
    let mut stopped = None;
    let watch_from = end.saturating_sub(MASTER_RATE);
    let mut busy: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    let (mut nmis, mut irqs) = (0u32, 0u32);
    let mut recent = [0u32; 64];
    let mut crash: Option<Vec<u32>> = None;
    while snes.master < end {
        if press_start {
            snes.buttons = if start_held(snes.master) { START } else { 0 };
        }
        if snes.master >= watch_from && snes.watch.is_none() {
            snes.watch = Some(Default::default());
        }
        let cycles = cpu.step(&mut snes);
        instructions += 1;
        let fast = snes.fast_rom && cpu.pbr >= 0x80;
        let clocks = if fast { cycles * FAST_CLOCKS } else { (cycles * SLOW_PAIR_CLOCKS).div_ceil(2) };
        if snes.tick(clocks) {
            snes.nmi_due = true;
        }
        if std::mem::take(&mut snes.nmi_due) {
            cpu.nmi(&mut snes);
            nmis += 1;
        }
        if snes.irq_flag {
            if cpu.p & 0x04 == 0 {
                irqs += 1;
            }
            cpu.irq(&mut snes);
        }
        recent[instructions as usize % recent.len()] = (cpu.pbr as u32) << 16 | cpu.pc as u32;
        // Running into the vector table, or a BRK, means the game went off the rails: keep the
        // way it got there.
        if crash.is_none() && ((cpu.pbr == 0 && cpu.pc >= 0xFFE0) || snes.peek_opcode(&cpu) == 0x00) {
            let n = recent.len();
            crash = Some((1..=n).map(|i| recent[(instructions as usize + i) % n]).collect());
        }
        if cpu.stopped {
            stopped = Some(format!("the CPU stopped at {:02x}:{:04x}", cpu.pbr, cpu.pc));
            break;
        }
        // Checked now and then: where the sound chip runs only changes on an upload's end.
        if instructions % 256 == 0 {
            snes.catch_up_apu();
            let pc = snes.apu.pc();
            let boot = pc >= 0xFFC0;
            if in_boot_rom && !boot {
                driver_starts.push(DriverStart { at: snes.master, entry: pc });
            }
            in_boot_rom = boot;
            if snes.watch.is_some() {
                *busy.entry((cpu.pbr as u32) << 16 | cpu.pc as u32).or_default() += 1;
            }
        }
    }
    let top = |m: Vec<(u32, u32)>| {
        let mut m = m;
        m.sort_by(|a, b| b.1.cmp(&a.1));
        m.truncate(5);
        m
    };
    let busy_at = top(busy.into_iter().collect());
    let busy_reads = top(snes.watch.take().unwrap_or_default().into_iter().map(|(a, n)| (a as u32, n)).collect()).into_iter().map(|(a, n)| (a as u16, n)).collect();
    snes.catch_up_apu();
    let spc = snes.apu.save_spc();
    let audio = std::mem::take(&mut snes.apu.output);
    Ok(Capture { spc, driver_starts, port_writes: snes.writes, port_replies: snes.replies, audio, instructions, stopped, busy_at, busy_reads, nmis, irqs, nmitimen: snes.nmitimen, irq_masked: cpu.p & 0x04 != 0, crash_trace: crash })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rom::snes_apu;

    /// A LoROM image whose code uploads `driver` through the boot ROM protocol, the way games
    /// do, then waits for vertical blanks forever.
    fn lorom_with_upload(driver: &[u8], aram: u16) -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000 * 4];
        // The data the upload loop sends, at $00:9000.
        rom[0x1000..0x1000 + driver.len()].copy_from_slice(driver);
        let len = driver.len() as u16;
        #[rustfmt::skip]
        let mut code: Vec<u8> = vec![
            0x78, 0x18, 0xFB,             // SEI; CLC; XCE
            0xC2, 0x10, 0xE2, 0x20,       // REP #$10; SEP #$20 (8-bit A, 16-bit X/Y)
            // wait for $AA/$BB
            0xAD, 0x40, 0x21, 0xC9, 0xAA, 0xD0, 0xF9,        // LDA $2140; CMP #$AA; BNE
            0xAD, 0x41, 0x21, 0xC9, 0xBB, 0xD0, 0xF9,        // LDA $2141; CMP #$BB; BNE
            // address, command 1, kick $CC
            0xA9, aram as u8, 0x8D, 0x42, 0x21,
            0xA9, (aram >> 8) as u8, 0x8D, 0x43, 0x21,
            0xA9, 0x01, 0x8D, 0x41, 0x21,
            0xA9, 0xCC, 0x8D, 0x40, 0x21,
            0xCD, 0x40, 0x21, 0xD0, 0xFB,                    // CMP $2140; BNE
            // bytes: X = index
            0xA2, 0x00, 0x00,                                // LDX #0
            // loop:
            0xBD, 0x00, 0x90, 0x8D, 0x41, 0x21,              // LDA $9000,X; STA $2141
            0x8A, 0x8D, 0x40, 0x21,                          // TXA; STA $2140
            0xCD, 0x40, 0x21, 0xD0, 0xFB,                    // CMP $2140; BNE
            0xE8, 0xE0, len as u8, (len >> 8) as u8,         // INX; CPX #len
            0xD0, 0xEB,                                      // BNE loop
            // end: entry address, command 0, kick = last index + 2
            0xA9, aram as u8, 0x8D, 0x42, 0x21,
            0xA9, (aram >> 8) as u8, 0x8D, 0x43, 0x21,
            0x9C, 0x41, 0x21,                                // STZ $2141
            0x8A, 0x1A, 0x8D, 0x40, 0x21,                    // TXA; INC; STA $2140
            0xA9, 0x80, 0x8D, 0x00, 0x42,                    // LDA #$80; STA $4200 (NMI on)
            0xCB, 0x80, 0xFD,                                // WAI; BRA -3
        ];
        code.resize(0x100, 0xEA);
        // NMI handler at $8100: count frames in $10, return.
        code.extend([0xEE, 0x10, 0x00, 0x40]); // INC $0010; RTI
        rom[..code.len()].copy_from_slice(&code);
        // Header: LoROM, checksum pair, vectors.
        rom[0x7FC0..0x7FD5].copy_from_slice(b"TEST CART            ");
        rom[0x7FD5] = 0x20;
        rom[0x7FDC..0x7FE0].copy_from_slice(&[0xFF, 0xFF, 0x00, 0x00]);
        rom[0x7FEA..0x7FEC].copy_from_slice(&[0x00, 0x81]); // native NMI
        rom[0x7FFC..0x7FFE].copy_from_slice(&[0x00, 0x80]); // reset
        rom
    }

    #[test]
    fn the_header_tells_the_mapping() {
        let rom = lorom_with_upload(&[0; 16], 0x0400);
        assert_eq!(mapping(&rom), Some(Mapping::LoRom));
    }

    #[test]
    fn a_game_upload_reaches_the_sound_chip() {
        if !snes_apu::available() {
            eprintln!("gmespc not built; skipped");
            return;
        }
        // MOV A,#$5A ; MOV $F4,A ; BRA -4, then data.
        let mut driver = vec![0xE8, 0x5A, 0xC4, 0xF4, 0x2F, 0xFA];
        driver.extend((0..500u32).map(|i| (i * 13) as u8));
        let rom = lorom_with_upload(&driver, 0x0400);
        let c = capture(&rom, &Options { seconds: 0.5, keep_audio: false, press_start: true }).unwrap();
        assert!(c.stopped.is_none(), "{:?}", c.stopped);
        let aram = &c.spc[0x100..0x100 + 0x10000];
        assert_eq!(&aram[0x0400..0x0400 + driver.len()], &driver[..]);
        assert_eq!(c.driver_starts.first().map(|d| d.entry & 0xFFF0), Some(0x0400));
    }
}
