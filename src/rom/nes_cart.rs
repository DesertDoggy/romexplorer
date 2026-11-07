//! NES cartridges as the CPU sees them: the iNES header, and the boards' program banking,
//! interrupt timers and sound chips. Graphics banking is left out, since nothing is drawn.
//!
//! A board's whole state is plain data, so a machine can be copied at any moment and run
//! again from there.

use std::sync::Arc;

use super::nes_apu::{Target, CHIP_FME7, CHIP_MMC5, CHIP_N163, CHIP_VRC6};

/// What the iNES header says.
#[derive(Debug, Clone)]
pub struct Header {
    pub mapper: u16,
    pub submapper: u8,
    pub prg: Arc<Vec<u8>>,
    /// A PAL game (NES 2.0 timing byte, or "(E)"-style regions the caller knows of).
    pub pal: bool,
    /// Bytes of work RAM at $6000 (8 KB when the header doesn't say).
    pub prg_ram: usize,
}

/// Reads an iNES / NES 2.0 image.
pub fn parse(rom: &[u8]) -> Result<Header, String> {
    if rom.len() < 16 || &rom[0..4] != b"NES\x1A" {
        return Err("not an iNES ROM image".into());
    }
    let h = &rom[0..16];
    let nes2 = h[7] & 0x0C == 0x08;
    let mut prg_units = h[4] as usize;
    let mut mapper = ((h[6] >> 4) | (h[7] & 0xF0)) as u16;
    let mut submapper = 0;
    let mut pal = false;
    let mut prg_ram = 0x2000;
    if nes2 {
        mapper |= ((h[8] & 0x0F) as u16) << 8;
        submapper = h[8] >> 4;
        prg_units |= ((h[9] & 0x0F) as usize) << 8;
        pal = h[12] & 0x03 == 1;
        let shift = |n: u8| if n == 0 { 0 } else { 64usize << n };
        prg_ram = (shift(h[10] & 0x0F) + shift(h[10] >> 4)).max(0x2000);
    } else if h[12..16].iter().any(|&b| b != 0) {
        // An old header with junk ("DiskDude!") in its last bytes: the high mapper nibble is
        // junk too.
        mapper &= 0x0F;
    }
    let start = 16 + if h[6] & 0x04 != 0 { 512 } else { 0 };
    let size = prg_units * 0x4000;
    if size == 0 {
        return Err("the ROM image has no program".into());
    }
    let prg = rom
        .get(start..start + size)
        .or_else(|| rom.get(start..))
        .ok_or("the ROM image is cut short")?
        .to_vec();
    if prg.len() < 0x2000 {
        return Err("the ROM image is cut short".into());
    }
    Ok(Header {
        mapper,
        submapper,
        prg: Arc::new(prg),
        pal,
        prg_ram,
    })
}

/// The boards this knows, by mapper number.
pub fn supported(mapper: u16) -> bool {
    matches!(
        mapper,
        0 | 1
            | 2
            | 3
            | 4
            | 5
            | 7
            | 9
            | 10
            | 11
            | 13
            | 16
            | 18
            | 19
            | 21
            | 22
            | 23
            | 24
            | 25
            | 26
            | 32
            | 33
            | 34
            | 48
            | 64
            | 65
            | 66
            | 67
            | 68
            | 69
            | 70
            | 71
            | 73
            | 75
            | 76
            | 78
            | 79
            | 80
            | 82
            | 87
            | 88
            | 89
            | 93
            | 94
            | 95
            | 97
            | 101
            | 107
            | 113
            | 118
            | 119
            | 140
            | 152
            | 154
            | 159
            | 180
            | 184
            | 185
            | 206
            | 207
            | 210
            | 228
            | 232
    )
}

/// A cartridge: program ROM, work RAM and the board's registers.
#[derive(Clone)]
pub struct Cart {
    pub mapper: u16,
    submapper: u8,
    prg: Arc<Vec<u8>>,
    pub ram: Vec<u8>,
    /// Where each 8 KB window $6000, $8000, $A000, $C000, $E000 reads from: an offset into
    /// the program ROM, or `usize::MAX` for work RAM at $6000.
    windows: [usize; 5],
    regs: [u8; 32],
    /// Shift register and count (MMC1).
    shift: u8,
    shift_n: u8,
    /// The instruction that last wrote the board (MMC1 ignores a second write in one).
    last_write: u64,
    irq_latch: u16,
    irq_counter: u16,
    irq_enabled: bool,
    irq_reload: bool,
    irq_prescaler: i32,
    irq_mode: u8,
    pub irq: bool,
    /// Work RAM writes allowed (MMC3 $A001, MMC5...).
    ram_enabled: bool,
}

const RAM_WINDOW: usize = usize::MAX;

impl Cart {
    pub fn new(h: &Header) -> Result<Self, String> {
        if !supported(h.mapper) {
            return Err(format!("NES board (mapper {}) not supported yet", h.mapper));
        }
        let mut c = Self {
            mapper: h.mapper,
            submapper: h.submapper,
            prg: h.prg.clone(),
            ram: vec![0; h.prg_ram],
            windows: [RAM_WINDOW, 0, 0, 0, 0],
            regs: [0; 32],
            shift: 0,
            shift_n: 0,
            last_write: u64::MAX,
            irq_latch: 0,
            irq_counter: 0,
            irq_enabled: false,
            irq_reload: false,
            irq_prescaler: 0,
            irq_mode: 0,
            irq: false,
            ram_enabled: true,
        };
        c.power_on();
        Ok(c)
    }

    /// The sound chips this board carries.
    pub fn chips(&self) -> u8 {
        match self.mapper {
            24 | 26 => CHIP_VRC6,
            19 => CHIP_N163,
            69 => CHIP_FME7,
            5 => CHIP_MMC5,
            _ => 0,
        }
    }

    fn banks8(&self) -> usize {
        (self.prg.len() / 0x2000).max(1)
    }

    /// Maps 8 KB window `w` (1-4 for $8000-$E000, 0 for $6000) to 8 KB bank `bank` (negative:
    /// from the end).
    fn map8(&mut self, w: usize, bank: isize) {
        let n = self.banks8() as isize;
        let b = bank.rem_euclid(n) as usize;
        self.windows[w] = b * 0x2000;
    }

    fn map16(&mut self, w: usize, bank: isize) {
        self.map8(w, bank * 2);
        self.map8(w + 1, bank * 2 + 1);
    }

    fn map32(&mut self, bank: isize) {
        for i in 0..4 {
            self.map8(1 + i, bank * 4 + i as isize);
        }
    }

    fn power_on(&mut self) {
        // Most boards start with the last bank fixed at $C000-$FFFF and the first at $8000.
        self.map8(1, 0);
        self.map8(2, 1);
        self.map8(3, -2);
        self.map8(4, -1);
        match self.mapper {
            1 => {
                self.regs[0] = 0x0C;
                self.mmc1_update();
            }
            4 | 118 | 119 | 206 | 88 | 95 | 154 | 76 => {
                self.regs[8] = 0;
                self.regs[6] = 0;
                self.regs[7] = 1;
                self.mmc3_update();
            }
            7 | 11 | 34 | 66 | 140 | 107 | 113 | 228 => self.map32(0),
            5 => {
                self.regs[0] = 3;
                self.regs[0x17] = 0xFF;
                self.mmc5_update();
            }
            180 => {
                self.map16(1, 0);
                self.map16(3, 0);
            }
            232 => self.map32(0),
            _ => {}
        }
    }

    /// Reads $4020-$FFFF. `None`: nothing answers (open bus).
    pub fn read(&self, addr: u16) -> Option<u8> {
        match addr {
            0x6000..=0x7FFF => {
                let w = self.windows[0];
                if w == RAM_WINDOW {
                    if self.ram.is_empty() {
                        None
                    } else {
                        Some(self.ram[(addr as usize - 0x6000) % self.ram.len()])
                    }
                } else {
                    Some(self.prg[w + (addr as usize & 0x1FFF)])
                }
            }
            0x8000..=0xFFFF => {
                let w = self.windows[1 + ((addr as usize - 0x8000) >> 13)];
                Some(self.prg[(w + (addr as usize & 0x1FFF)) % self.prg.len()])
            }
            0x5C00..=0x5FFF if self.mapper == 5 => Some(self.regs[0x18]),
            _ => None,
        }
    }

    /// A program-space read with no side effects, for the DMC channel and the search.
    pub fn peek(&self, addr: u16) -> u8 {
        self.read(addr).unwrap_or((addr >> 8) as u8)
    }

    /// A write to $4020-$FFFF during instruction `instr`. Returns the sound chip register it
    /// reaches, if any (the caller hands it to the chip with its time).
    pub fn write(&mut self, addr: u16, v: u8, instr: u64) -> Option<Target> {
        let consecutive = self.last_write == instr;
        self.last_write = instr;
        if (0x6000..=0x7FFF).contains(&addr) {
            if self.windows[0] == RAM_WINDOW && self.ram_enabled && !self.ram.is_empty() {
                let n = self.ram.len();
                self.ram[(addr as usize - 0x6000) % n] = v;
            }
            // Boards whose registers sit at $6000-$7FFF.
            match self.mapper {
                34 if addr >= 0x7FFD => {
                    // NINA-001: $7FFD selects the 32 KB bank.
                    if addr == 0x7FFD {
                        self.map32(v as isize & 1);
                    }
                }
                140 => self.map32(((v >> 4) & 3) as isize),
                80 | 207 => self.taito_x1(addr, v),
                82 => self.taito_x1_017(addr, v),
                _ => {}
            }
            return None;
        }
        match self.mapper {
            0 | 3 | 13 | 185 => {}
            1 => {
                if addr >= 0x8000 && !consecutive {
                    self.mmc1_write(addr, v);
                }
            }
            2 | 94 => {
                let bank = if self.mapper == 94 { v >> 2 } else { v };
                self.map16(1, bank as isize);
            }
            71 => {
                if addr >= 0xC000 {
                    self.map16(1, (v & 0x0F) as isize);
                }
            }
            180 => self.map16(3, (v & 7) as isize),
            7 => self.map32((v & 0x0F) as isize),
            11 => self.map32((v & 3) as isize),
            66 => self.map32(((v >> 4) & 3) as isize),
            34 => {
                if addr >= 0x8000 {
                    self.map32(v as isize);
                }
            }
            107 => self.map32((v >> 1) as isize),
            113 => {
                if (0x4100..0x6000).contains(&addr) && addr & 0x100 != 0 {
                    self.map32(((v >> 3) & 7) as isize);
                }
            }
            228 => {
                if addr >= 0x8000 {
                    let chip = ((addr >> 11) & 3) as isize;
                    let chip = if chip == 3 { 2 } else { chip };
                    let bank = ((addr >> 6) & 0x1F) as isize | (chip << 5);
                    if addr & 0x20 != 0 {
                        self.map16(1, bank);
                        self.map16(3, bank);
                    } else {
                        self.map32(bank >> 1);
                    }
                }
            }
            232 => {
                if addr < 0xC000 {
                    self.regs[0] = (v >> 3) & 3;
                } else {
                    self.regs[1] = v & 3;
                }
                let outer = (self.regs[0] as isize) << 2;
                self.map16(1, outer | self.regs[1] as isize);
                self.map16(3, outer | 3);
            }
            78 | 70 | 152 => {
                if addr >= 0x8000 {
                    let bank = if self.mapper == 152 {
                        (v >> 4) & 7
                    } else {
                        v & 7
                    };
                    let bank = if self.mapper == 70 { v >> 4 } else { bank };
                    self.map16(1, bank as isize);
                }
            }
            87 | 101 | 184 => {}
            89 | 93 => {
                if addr >= 0x8000 {
                    self.map16(1, ((v >> 4) & 7) as isize);
                }
            }
            97 => {
                if addr >= 0x8000 {
                    self.map16(1, -1);
                    self.map16(3, (v & 0x0F) as isize);
                }
            }
            79 => {
                if (0x4100..0x6000).contains(&addr) && addr & 0x100 != 0 {
                    self.map32(((v >> 3) & 1) as isize);
                }
            }
            4 | 118 | 119 | 206 | 88 | 95 | 154 | 76 => self.mmc3_write(addr, v),
            64 => self.rambo_write(addr, v),
            9 => {
                if (0xA000..0xB000).contains(&addr) {
                    self.map8(1, (v & 0x0F) as isize);
                    self.map8(2, -3);
                }
            }
            10 => {
                if (0xA000..0xB000).contains(&addr) {
                    self.map16(1, (v & 0x0F) as isize);
                }
            }
            5 => return self.mmc5_write(addr, v),
            16 | 159 => self.bandai_write(addr, v),
            18 => self.jaleco_write(addr, v),
            19 => return self.n163_write(addr, v),
            21 | 22 | 23 | 25 => self.vrc24_write(addr, v),
            24 | 26 => return self.vrc6_write(addr, v),
            32 => self.irem_g101_write(addr, v),
            33 | 48 => self.taito_tc0190_write(addr, v),
            65 => self.irem_h3001_write(addr, v),
            67 => {
                if addr & 0xF800 == 0xF800 {
                    self.map16(1, v as isize);
                } else if addr & 0xF800 == 0xC800 {
                    // IRQ counter, written high then low.
                    if self.regs[0] == 0 {
                        self.irq_counter = (self.irq_counter & 0x00FF) | ((v as u16) << 8);
                    } else {
                        self.irq_counter = (self.irq_counter & 0xFF00) | v as u16;
                    }
                    self.regs[0] ^= 1;
                } else if addr & 0xF800 == 0xD800 {
                    self.regs[0] = 0;
                    self.irq_enabled = v & 0x10 != 0;
                    self.irq = false;
                }
            }
            68 => {
                if addr >= 0xF000 {
                    self.map16(1, v as isize);
                }
            }
            69 => return self.fme7_write(addr, v),
            73 => self.vrc3_write(addr, v),
            75 => {
                if (0x8000..0x9000).contains(&addr) {
                    self.map8(1, (v & 0x0F) as isize);
                } else if (0xA000..0xB000).contains(&addr) {
                    self.map8(2, (v & 0x0F) as isize);
                } else if (0xC000..0xD000).contains(&addr) {
                    self.map8(3, (v & 0x0F) as isize);
                }
            }
            210 => {
                if (0xE000..0xE800).contains(&addr) {
                    self.map8(1, (v & 0x3F) as isize);
                } else if (0xE800..0xF000).contains(&addr) {
                    self.map8(2, (v & 0x3F) as isize);
                } else if (0xF000..0xF800).contains(&addr) {
                    self.map8(3, (v & 0x3F) as isize);
                }
            }
            80 | 207 => self.taito_x1(addr, v),
            82 => self.taito_x1_017(addr, v),
            _ => {}
        }
        None
    }

    /// Reads that reach the board below $6000 (sound chip registers, IRQ status).
    pub fn read_low(&mut self, addr: u16) -> Option<u8> {
        match self.mapper {
            5 => match addr {
                0x5204 => {
                    // In-frame and IRQ pending flags.
                    let v = (self.irq as u8) << 7 | 0x40;
                    self.irq = false;
                    Some(v)
                }
                0x5205 => Some((self.regs[0x1A] as u16 * self.regs[0x1B] as u16) as u8),
                0x5206 => Some(((self.regs[0x1A] as u16 * self.regs[0x1B] as u16) >> 8) as u8),
                _ => None,
            },
            _ => None,
        }
    }

    // --- MMC1 ---------------------------------------------------------------------

    fn mmc1_write(&mut self, addr: u16, v: u8) {
        if v & 0x80 != 0 {
            self.shift = 0;
            self.shift_n = 0;
            self.regs[0] |= 0x0C;
            self.mmc1_update();
            return;
        }
        self.shift |= (v & 1) << self.shift_n;
        self.shift_n += 1;
        if self.shift_n == 5 {
            let reg = ((addr >> 13) & 3) as usize;
            self.regs[reg] = self.shift;
            self.shift = 0;
            self.shift_n = 0;
            self.mmc1_update();
        }
    }

    fn mmc1_update(&mut self) {
        let mode = (self.regs[0] >> 2) & 3;
        // SUROM/SXROM: CHR register bit 4 picks the 256 KB half of a 512 KB program.
        let outer = if self.prg.len() > 0x40000 {
            ((self.regs[1] >> 4) & 1) as isize * 16
        } else {
            0
        };
        let bank = (self.regs[3] & 0x0F) as isize;
        match mode {
            0 | 1 => self.map32((outer | (bank & !1)) >> 1),
            2 => {
                self.map16(1, outer);
                self.map16(3, outer | bank);
            }
            _ => {
                self.map16(1, outer | bank);
                self.map16(3, outer | 15);
            }
        }
        self.ram_enabled = self.regs[3] & 0x10 == 0;
    }

    // --- MMC3 and relatives ---------------------------------------------------------

    fn mmc3_write(&mut self, addr: u16, v: u8) {
        if self.mapper == 206
            || self.mapper == 88
            || self.mapper == 95
            || self.mapper == 154
            || self.mapper == 76
        {
            // Namco 108: bank select and data only, no mode bits, no IRQ.
            if addr < 0xA000 {
                if addr & 1 == 0 {
                    self.regs[8] = v & 7;
                } else {
                    let r = self.regs[8] as usize;
                    self.regs[r] = v;
                    self.mmc3_update();
                }
            }
            return;
        }
        match (addr & 0xE001, addr) {
            (0x8000, _) => {
                self.regs[8] = v;
                self.mmc3_update();
            }
            (0x8001, _) => {
                let r = (self.regs[8] & 7) as usize;
                self.regs[r] = v;
                self.mmc3_update();
            }
            (0xA001, _) => self.ram_enabled = v & 0x80 != 0 || self.submapper == 1,
            (0xC000, _) => self.irq_latch = v as u16,
            (0xC001, _) => {
                self.irq_counter = 0;
                self.irq_reload = true;
            }
            (0xE000, _) => {
                self.irq_enabled = false;
                self.irq = false;
            }
            (0xE001, _) => self.irq_enabled = true,
            _ => {}
        }
    }

    fn mmc3_update(&mut self) {
        let mode = self.mapper != 206
            && self.mapper != 88
            && self.mapper != 95
            && self.mapper != 154
            && self.mapper != 76
            && self.regs[8] & 0x40 != 0;
        let (r6, r7) = (self.regs[6] as isize & 0x3F, self.regs[7] as isize & 0x3F);
        if mode {
            self.map8(1, -2);
            self.map8(3, r6);
        } else {
            self.map8(1, r6);
            self.map8(3, -2);
        }
        self.map8(2, r7);
        self.map8(4, -1);
    }

    /// One scanline with rendering on: the MMC3 counter clocks (from A12 rising).
    pub fn scanline(&mut self) {
        match self.mapper {
            4 | 118 | 119 | 64 => {
                if self.irq_counter == 0 || self.irq_reload {
                    self.irq_counter = self.irq_latch;
                    self.irq_reload = false;
                } else {
                    self.irq_counter -= 1;
                }
                if self.irq_counter == 0 && self.irq_enabled {
                    self.irq = true;
                }
            }
            5 => {
                // MMC5 scanline compare.
                self.regs[0x1C] = self.regs[0x1C].wrapping_add(1);
                if self.regs[0x1C] == self.regs[0x1D] && self.regs[0x1E] & 0x80 != 0 {
                    self.irq = true;
                }
            }
            48 => {
                if self.irq_counter == 0 || self.irq_reload {
                    self.irq_counter = self.irq_latch;
                    self.irq_reload = false;
                } else {
                    self.irq_counter -= 1;
                }
                if self.irq_counter == 0 && self.irq_enabled {
                    self.irq = true;
                }
            }
            _ => {}
        }
    }

    /// The start of a frame (MMC5 resets its line counter).
    pub fn frame_start(&mut self) {
        if self.mapper == 5 {
            self.regs[0x1C] = 0;
        }
    }

    /// `cycles` CPU cycles passed: the cycle-counting IRQ timers.
    pub fn clock(&mut self, cycles: u32) {
        if !self.irq_enabled {
            return;
        }
        match self.mapper {
            21 | 22 | 23 | 25 | 24 | 26 | 73 | 16 | 159 | 18 | 19 | 65 | 67 | 69 => {}
            _ => return,
        }
        for _ in 0..cycles {
            match self.mapper {
                21 | 22 | 23 | 25 | 24 | 26 => {
                    // VRC IRQ: scanline mode prescales by 341/3; cycle mode counts every cycle.
                    let tick = if self.irq_mode & 4 != 0 {
                        true
                    } else {
                        self.irq_prescaler -= 3;
                        if self.irq_prescaler <= 0 {
                            self.irq_prescaler += 341;
                            true
                        } else {
                            false
                        }
                    };
                    if tick {
                        if self.irq_counter == 0xFF {
                            self.irq_counter = self.irq_latch;
                            self.irq = true;
                        } else {
                            self.irq_counter += 1;
                        }
                    }
                }
                73 => {
                    if self.irq_mode & 4 != 0 {
                        let low = (self.irq_counter & 0xFF) as u8;
                        if low == 0xFF {
                            self.irq_counter =
                                (self.irq_counter & 0xFF00) | (self.irq_latch & 0xFF);
                            self.irq = true;
                        } else {
                            self.irq_counter =
                                (self.irq_counter & 0xFF00) | low.wrapping_add(1) as u16;
                        }
                    } else if self.irq_counter == 0xFFFF {
                        self.irq_counter = self.irq_latch;
                        self.irq = true;
                    } else {
                        self.irq_counter += 1;
                    }
                }
                16 | 159 | 18 | 65 | 67 | 69 => {
                    if self.irq_counter == 0 {
                        self.irq = true;
                        if self.mapper == 65 || self.mapper == 18 {
                            self.irq_enabled = false;
                        }
                        self.irq_counter = if self.mapper == 69 { 0xFFFF } else { 0 };
                        if self.mapper != 69 {
                            self.irq_enabled = false;
                        }
                    } else {
                        self.irq_counter -= 1;
                    }
                }
                19 => {
                    if self.irq_counter >= 0x7FFF {
                        self.irq = true;
                    } else {
                        self.irq_counter += 1;
                    }
                }
                _ => {}
            }
        }
    }

    // --- MMC5 (program banking and its sound) ----------------------------------------

    fn mmc5_write(&mut self, addr: u16, v: u8) -> Option<Target> {
        match addr {
            0x5000..=0x5015 => return Some(Target::Mmc5),
            0x5100 => self.regs[0] = v & 3,
            0x5113..=0x5117 => {
                self.regs[0x13 + (addr - 0x5113) as usize] = v;
                self.mmc5_update();
            }
            0x5203 => self.regs[0x1D] = v,
            0x5204 => self.regs[0x1E] = v,
            0x5205 => self.regs[0x1A] = v,
            0x5206 => self.regs[0x1B] = v,
            0x5C00..=0x5FFF => self.regs[0x18] = v,
            _ => {}
        }
        None
    }

    fn mmc5_update(&mut self) {
        let r = |i: usize| self.regs[0x13 + i];
        let rom_or_ram = |v: u8| v & 0x80 != 0;
        // $6000 is always RAM here; ROM there is rare.
        let _ = r(0);
        let b = |v: u8| (v & 0x7F) as isize;
        let (r1, r2, r3, r4) = (r(1), r(2), r(3), r(4));
        match self.regs[0] & 3 {
            0 => self.map32(b(r4) >> 2),
            1 => {
                if rom_or_ram(r2) {
                    self.map16(1, b(r2) >> 1);
                }
                self.map16(3, b(r4) >> 1);
            }
            2 => {
                if rom_or_ram(r2) {
                    self.map16(1, b(r2) >> 1);
                }
                if rom_or_ram(r3) {
                    self.map8(3, b(r3));
                }
                self.map8(4, b(r4));
            }
            _ => {
                if rom_or_ram(r1) {
                    self.map8(1, b(r1));
                }
                if rom_or_ram(r2) {
                    self.map8(2, b(r2));
                }
                if rom_or_ram(r3) {
                    self.map8(3, b(r3));
                }
                self.map8(4, b(r4));
            }
        }
    }

    // --- Konami VRC2/VRC4, VRC3, VRC6 --------------------------------------------------

    /// The register a VRC2/4/6 address means: (high nibble, 0-3), from the board's wiring.
    fn vrc_reg(&self, addr: u16) -> (u16, u8) {
        let (a0, a1) = match (self.mapper, self.submapper) {
            (21, 2) => ((addr >> 6) & 1, (addr >> 7) & 1),
            (21, _) => (
                (addr >> 1) & 1 | (addr >> 6) & 1,
                (addr >> 2) & 1 | (addr >> 7) & 1,
            ),
            (22, _) => ((addr >> 1) & 1, addr & 1),
            (23, 3) => ((addr >> 2) & 1, (addr >> 3) & 1),
            (23, _) => (
                addr & 1 | (addr >> 2) & 1,
                (addr >> 1) & 1 | (addr >> 3) & 1,
            ),
            (25, 2) => ((addr >> 3) & 1, (addr >> 2) & 1),
            (25, _) => (
                (addr >> 1) & 1 | (addr >> 3) & 1,
                addr & 1 | (addr >> 2) & 1,
            ),
            (26, _) => ((addr >> 1) & 1, addr & 1),
            _ => (addr & 1, (addr >> 1) & 1),
        };
        (addr & 0xF000, (a0 | (a1 << 1)) as u8)
    }

    fn vrc24_write(&mut self, addr: u16, v: u8) {
        let (hi, reg) = self.vrc_reg(addr);
        match hi {
            0x8000 => {
                self.regs[0] = v & 0x1F;
                self.vrc24_update();
            }
            0x9000 => {
                if reg >= 2 && self.mapper != 22 {
                    self.regs[2] = v;
                    self.vrc24_update();
                }
            }
            0xA000 => {
                self.regs[1] = v & 0x1F;
                self.vrc24_update();
            }
            0xF000 => match reg {
                0 => self.irq_latch = (self.irq_latch & 0xF0) | (v as u16 & 0x0F),
                1 => self.irq_latch = (self.irq_latch & 0x0F) | ((v as u16 & 0x0F) << 4),
                2 => self.vrc_irq_control(v),
                _ => self.vrc_irq_ack(),
            },
            _ => {}
        }
    }

    fn vrc24_update(&mut self) {
        let swap = self.regs[2] & 2 != 0 && self.mapper != 22;
        if swap {
            self.map8(1, -2);
            self.map8(3, self.regs[0] as isize);
        } else {
            self.map8(1, self.regs[0] as isize);
            self.map8(3, -2);
        }
        self.map8(2, self.regs[1] as isize);
        self.map8(4, -1);
    }

    fn vrc_irq_control(&mut self, v: u8) {
        self.irq_mode = v;
        self.irq_enabled = v & 2 != 0;
        if self.irq_enabled {
            self.irq_counter = self.irq_latch;
            self.irq_prescaler = 341;
        }
        self.irq = false;
    }

    fn vrc_irq_ack(&mut self) {
        self.irq = false;
        self.irq_enabled = self.irq_mode & 1 != 0;
    }

    fn vrc6_write(&mut self, addr: u16, v: u8) -> Option<Target> {
        let (hi, reg) = self.vrc_reg(addr);
        match hi {
            0x8000 => self.map16(1, (v & 0x0F) as isize),
            0xC000 => self.map8(3, (v & 0x1F) as isize),
            0x9000 | 0xA000 | 0xB000 if reg < 3 => {
                let osc = ((hi - 0x9000) >> 12) as u8;
                return Some(Target::Vrc6 { osc, reg });
            }
            0xF000 => match reg {
                0 => self.irq_latch = v as u16,
                1 => self.vrc_irq_control(v),
                2 => self.vrc_irq_ack(),
                _ => {}
            },
            _ => {}
        }
        None
    }

    fn vrc3_write(&mut self, addr: u16, v: u8) {
        match addr & 0xF000 {
            0x8000 => self.irq_latch = (self.irq_latch & 0xFFF0) | (v as u16 & 0x0F),
            0x9000 => self.irq_latch = (self.irq_latch & 0xFF0F) | ((v as u16 & 0x0F) << 4),
            0xA000 => self.irq_latch = (self.irq_latch & 0xF0FF) | ((v as u16 & 0x0F) << 8),
            0xB000 => self.irq_latch = (self.irq_latch & 0x0FFF) | ((v as u16 & 0x0F) << 12),
            0xC000 => {
                self.irq_mode = v;
                self.irq_enabled = v & 2 != 0;
                if self.irq_enabled {
                    self.irq_counter = self.irq_latch;
                }
                self.irq = false;
            }
            0xD000 => {
                self.irq = false;
                self.irq_enabled = self.irq_mode & 1 != 0;
            }
            0xF000 => self.map16(1, (v & 7) as isize),
            _ => {}
        }
    }

    // --- Namco 163 ---------------------------------------------------------------------

    fn n163_write(&mut self, addr: u16, v: u8) -> Option<Target> {
        match addr & 0xF800 {
            0x4800 => return Some(Target::N163Data),
            0x5000 => {
                self.irq_counter = (self.irq_counter & 0x7F00) | v as u16;
                self.irq = false;
            }
            0x5800 => {
                self.irq_counter = (self.irq_counter & 0x00FF) | ((v as u16 & 0x7F) << 8);
                self.irq_enabled = v & 0x80 != 0;
                self.irq = false;
            }
            0xE000 => self.map8(1, (v & 0x3F) as isize),
            0xE800 => self.map8(2, (v & 0x3F) as isize),
            0xF000 => self.map8(3, (v & 0x3F) as isize),
            0xF800 => return Some(Target::N163Addr),
            _ => {}
        }
        None
    }

    // --- Sunsoft FME-7 / 5B ------------------------------------------------------------

    fn fme7_write(&mut self, addr: u16, v: u8) -> Option<Target> {
        match addr & 0xE000 {
            0x8000 => self.regs[16] = v & 0x0F,
            0xA000 => {
                let cmd = self.regs[16];
                match cmd {
                    8 => {
                        if v & 0x40 == 0 {
                            self.map8(0, (v & 0x3F) as isize);
                        } else {
                            self.windows[0] = RAM_WINDOW;
                            self.ram_enabled = v & 0x80 != 0;
                        }
                    }
                    9 => self.map8(1, (v & 0x3F) as isize),
                    10 => self.map8(2, (v & 0x3F) as isize),
                    11 => self.map8(3, (v & 0x3F) as isize),
                    13 => {
                        self.irq_enabled = v & 0x80 != 0;
                        self.irq_mode = v;
                        self.irq = false;
                    }
                    14 => self.irq_counter = (self.irq_counter & 0xFF00) | v as u16,
                    15 => self.irq_counter = (self.irq_counter & 0x00FF) | ((v as u16) << 8),
                    _ => {}
                }
            }
            0xC000 => return Some(Target::Fme7Latch),
            0xE000 => return Some(Target::Fme7Data),
            _ => {}
        }
        None
    }

    // --- The rest: Bandai, Jaleco, Irem, Taito, RAMBO-1 ---------------------------------

    fn bandai_write(&mut self, addr: u16, v: u8) {
        let reg = addr & 0x0F;
        if (self.mapper == 16 && addr < 0x6000) || addr < 0x8000 && self.submapper == 4 {
            return;
        }
        match reg {
            8 => self.map16(1, (v & 0x0F) as isize),
            0xA => {
                self.irq_enabled = v & 1 != 0;
                self.irq = false;
            }
            0xB => self.irq_counter = (self.irq_counter & 0xFF00) | v as u16,
            0xC => self.irq_counter = (self.irq_counter & 0x00FF) | ((v as u16) << 8),
            _ => {}
        }
    }

    fn jaleco_write(&mut self, addr: u16, v: u8) {
        let i = (((addr >> 11) & 0x0E) | (addr & 1)) as usize;
        let n = v & 0x0F;
        match addr & 0xF003 {
            0x8000..=0x9001 => {
                // PRG banks: pairs of nibbles for $8000, $A000, $C000.
                let w = i / 2;
                if w < 3 {
                    let r = &mut self.regs[w];
                    *r = if i & 1 == 0 {
                        (*r & 0xF0) | n
                    } else {
                        (*r & 0x0F) | (n << 4)
                    };
                    let bank = self.regs[w] as isize;
                    self.map8(1 + w, bank);
                }
            }
            0xE000..=0xE003 => {
                let shift = (addr & 3) * 4;
                self.irq_latch = (self.irq_latch & !(0xF << shift)) | ((n as u16) << shift);
            }
            0xF000 => {
                self.irq_counter = self.irq_latch;
                self.irq = false;
            }
            0xF001 => {
                self.irq_enabled = v & 1 != 0;
                self.irq = false;
            }
            _ => {}
        }
    }

    fn irem_g101_write(&mut self, addr: u16, v: u8) {
        match addr & 0xF000 {
            0x8000 => {
                self.regs[0] = v & 0x1F;
                self.irem_g101_update();
            }
            0x9000 => {
                self.regs[2] = v;
                self.irem_g101_update();
            }
            0xA000 => self.map8(2, (v & 0x1F) as isize),
            _ => {}
        }
    }

    fn irem_g101_update(&mut self) {
        if self.regs[2] & 2 != 0 && self.submapper != 1 {
            self.map8(1, -2);
            self.map8(3, self.regs[0] as isize);
        } else {
            self.map8(1, self.regs[0] as isize);
            self.map8(3, -2);
        }
        self.map8(4, -1);
    }

    fn taito_tc0190_write(&mut self, addr: u16, v: u8) {
        match addr & 0xE003 {
            0x8000 => self.map8(1, (v & 0x3F) as isize),
            0x8001 => self.map8(2, (v & 0x3F) as isize),
            0xC000 if self.mapper == 48 => self.irq_latch = (v ^ 0xFF) as u16,
            0xC001 if self.mapper == 48 => {
                self.irq_counter = 0;
                self.irq_reload = true;
            }
            0xC002 if self.mapper == 48 => self.irq_enabled = true,
            0xC003 if self.mapper == 48 => {
                self.irq_enabled = false;
                self.irq = false;
            }
            _ => {}
        }
    }

    fn irem_h3001_write(&mut self, addr: u16, v: u8) {
        match addr {
            0x8000 => self.map8(1, v as isize),
            0x9001 => {
                self.regs[2] = v;
            }
            0x9003 => {
                self.irq_enabled = v & 0x80 != 0;
                self.irq = false;
            }
            0x9004 => {
                self.irq_counter = self.irq_latch;
                self.irq = false;
            }
            0x9005 => self.irq_latch = (self.irq_latch & 0x00FF) | ((v as u16) << 8),
            0x9006 => self.irq_latch = (self.irq_latch & 0xFF00) | v as u16,
            0xA000 => self.map8(2, v as isize),
            0xC000 => self.map8(3, v as isize),
            _ => {}
        }
    }

    fn rambo_write(&mut self, addr: u16, v: u8) {
        match addr & 0xE001 {
            0x8000 => {
                self.regs[8] = v;
                self.rambo_update();
            }
            0x8001 => {
                let r = (self.regs[8] & 0x0F) as usize;
                if r == 15 {
                    self.regs[9] = v;
                } else if r < 8 {
                    self.regs[r] = v;
                } else {
                    self.regs[r] = v;
                }
                self.rambo_update();
            }
            0xC000 => self.irq_latch = v as u16,
            0xC001 => {
                self.irq_reload = true;
                self.irq_counter = 0;
            }
            0xE000 => {
                self.irq_enabled = false;
                self.irq = false;
            }
            0xE001 => self.irq_enabled = true,
            _ => {}
        }
    }

    fn rambo_update(&mut self) {
        let (r6, r7, rf) = (
            self.regs[6] as isize,
            self.regs[7] as isize,
            self.regs[9] as isize,
        );
        if self.regs[8] & 0x40 != 0 {
            self.map8(1, rf);
            self.map8(2, r6);
            self.map8(3, r7);
        } else {
            self.map8(1, r6);
            self.map8(2, r7);
            self.map8(3, rf);
        }
        self.map8(4, -1);
    }

    fn taito_x1(&mut self, addr: u16, v: u8) {
        match addr {
            0x7EFA | 0x7EFB => self.map8(1, v as isize),
            0x7EFC | 0x7EFD => self.map8(2, v as isize),
            0x7EFE | 0x7EFF => self.map8(3, v as isize),
            _ => {}
        }
    }

    fn taito_x1_017(&mut self, addr: u16, v: u8) {
        match addr {
            0x7EFA => self.map8(1, (v >> 2) as isize),
            0x7EFB => self.map8(2, (v >> 2) as isize),
            0x7EFC => self.map8(3, (v >> 2) as isize),
            _ => {}
        }
    }

    /// The program windows, for comparing board states.
    pub fn windows(&self) -> [usize; 5] {
        self.windows
    }

    /// Puts the program windows back as they were (the rest of the state stays).
    pub fn set_windows(&mut self, windows: [usize; 5]) {
        self.windows = windows;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(mapper: u8, banks16: u8) -> Vec<u8> {
        let mut rom = vec![0u8; 16 + banks16 as usize * 0x4000];
        rom[0..4].copy_from_slice(b"NES\x1A");
        rom[4] = banks16;
        rom[6] = mapper << 4;
        rom[7] = mapper & 0xF0;
        // Each 8 KB bank starts with its own number.
        for b in 0..banks16 as usize * 2 {
            rom[16 + b * 0x2000] = b as u8;
        }
        rom
    }

    #[test]
    fn mmc1_switches_the_low_bank_and_fixes_the_last() {
        let h = parse(&image(1, 8)).unwrap();
        let mut c = Cart::new(&h).unwrap();
        assert_eq!(c.peek(0xC000), 14);
        // Write 3 to the program register, one bit per write.
        for (i, bit) in [1u8, 1, 0, 0, 0].iter().enumerate() {
            c.write(0xE000, *bit, i as u64);
        }
        assert_eq!(c.peek(0x8000), 6);
        assert_eq!(c.peek(0xC000), 14);
    }

    #[test]
    fn mmc3_banks_and_its_mode_bit() {
        let h = parse(&image(4, 8)).unwrap();
        let mut c = Cart::new(&h).unwrap();
        c.write(0x8000, 6, 0);
        c.write(0x8001, 5, 1);
        assert_eq!(c.peek(0x8000), 5);
        assert_eq!(c.peek(0xC000), 14);
        c.write(0x8000, 0x46, 2);
        assert_eq!(c.peek(0xC000), 5);
        assert_eq!(c.peek(0x8000), 14);
        assert_eq!(c.peek(0xE000), 15);
    }

    #[test]
    fn vrc6_routes_sound_registers() {
        let h = parse(&image(24, 8)).unwrap();
        let mut c = Cart::new(&h).unwrap();
        assert_eq!(
            c.write(0xA001, 0x10, 0),
            Some(Target::Vrc6 { osc: 1, reg: 1 })
        );
        let h = parse(&image(26, 8)).unwrap();
        let mut c = Cart::new(&h).unwrap();
        // Mapper 26 swaps the two low address lines.
        assert_eq!(
            c.write(0xA001, 0x10, 0),
            Some(Target::Vrc6 { osc: 1, reg: 2 })
        );
    }
}
