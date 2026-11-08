//! The Neo Geo Pocket and Neo Geo Pocket Color as a [`Machine`] for the music search
//! ([`super::music_search`]): the TLCS-900/H ([`super::tlcs900h`]), the cartridge, work RAM,
//! the interrupt controller, the four 8-bit timers, the scanline counter, and the sound side:
//! the Z80 ([`super::z80`]) with its 4 KB window on work RAM, the byte the two CPUs pass
//! between them, and the T6W28 PSG. Nothing is drawn. Mono and colour games run the same way;
//! the colour flag only changes what the BIOS work area says.
//!
//! No BIOS: the console starts the cartridge in the state the BIOS leaves it, and a stand-in
//! answers the BIOS calls games make (system calls through SWI 1, and the default interrupt
//! handler), as Mednafen's NeoPop does (`beetle-ngp-libretro`, whose behaviour this follows).
//! Calls it doesn't know are recorded ([`NeoGeoPocket::unknown_bios_calls`]) and return at
//! once.
//!
//! The music is usually played by the Z80: the main CPU loads a driver into the shared RAM,
//! starts the Z80, and hands it commands through the shared byte (0xBC, the Z80's 0x8000)
//! with an NMI (a write to 0xBA), or through shared RAM. The Z80's interrupt comes from timer
//! 3. So every instruction of both CPUs is shown to the search, each with its own CPU's calls
//! in progress; Z80 routines are named by their address plus [`SOUND_CPU`]. Z80 accesses to
//! the shared RAM are traced at their main-CPU addresses (0x7000 + address), and the shared
//! byte at 0xBC from both sides, so one RAM byte means the same thing whichever CPU touched
//! it. Games that write the PSG straight from the main CPU (with the Z80 stopped) are found
//! the same way, with a main-CPU routine.
//!
//! The PSG's two ports are this module's ports 0 (left: the tone channels' periods and left
//! volumes) and 1 (right: the noise channel and right volumes). The two DACs (0xA2/0xA3) are
//! not modelled: there is no chip for them yet, and their writes are dropped.

use std::sync::Arc;

use super::music_search::{Machine, Step, Write};
use super::tlcs900h::{self, Cpu as MainCpu, XSP};
use super::vgm_chips::{Chips, Kind};
use super::z80::{self, Cpu as SoundCpu};

/// Main CPU clocks a second (the units of [`Write::at`]), per scanline, and scanlines per
/// frame (59.95 frames a second).
const CLOCK: u32 = 6_144_000;
const LINE: u32 = 515;
const LINES: u32 = 199;
const FRAME: u32 = LINE * LINES;
pub const RATE: f64 = CLOCK as f64 / FRAME as f64;
/// The scanline the vertical blank starts on, and the last line with a scanline interrupt
/// before it.
const VBLANK_LINE: u32 = 152;
/// The PSG's clock.
const PSG_CLOCK: u32 = 3_072_000;

/// Added to a Z80 address to name a sound-CPU routine (main-CPU addresses are 24-bit).
pub const SOUND_CPU: u32 = 0x0100_0000;

/// The PSG's ports, as [`Write::port`] numbers them.
const PSG_LEFT: u32 = 0;
const PSG_RIGHT: u32 = 1;

/// Where the BIOS keeps the joypad, and the buttons held for "Start": Option and A.
const JOYPAD: u32 = 0x6F82;
const START_BUTTONS: u8 = 0x40 | 0x10;

/// The interrupt table the BIOS dispatches through: 0x12 vectors.
const INT_TABLE: u32 = 0x6FB8;
/// The BIOS's default interrupt handler (a RETI).
const BIOS_RETI: u32 = 0xFF23DF;

/// The BIOS's system calls, by number (SWI 1 with RW3): where each lives in the BIOS.
const BIOS_CALLS: [u32; 0x1B] = [
    0xFF27A2, // 00 shutdown
    0xFF1030, // 01 clock gear
    0xFF1440, // 02 read the clock
    0xFF12B4, // 03 ?
    0xFF1222, // 04 interrupt level
    0xFF8D8A, // 05 system font
    0xFF6FD8, // 06 flash write
    0xFF7042, // 07 flash erase all
    0xFF7082, // 08 flash erase block
    0xFF149B, // 09 alarm
    0xFF1033, // 0A ?
    0xFF1487, // 0B power-on alarm
    0xFF731F, // 0C ?
    0xFF70CA, // 0D flash protect
    0xFF17C4, // 0E game mode
    0xFF1032, // 0F ?
    0xFF2BBD, // 10 link: init
    0xFF2C0C, // 11 link: send start
    0xFF2C44, // 12 link: receive start
    0xFF2C86, // 13 link: send a byte
    0xFF2CB4, // 14 link: receive a byte
    0xFF2D27, // 15 link: RTS on
    0xFF2D33, // 16 link: RTS off
    0xFF2D3A, // 17 link: send status
    0xFF2D4E, // 18 link: receive status
    0xFF2D6C, // 19 link: send a buffer
    0xFF2D85, // 1A link: receive a buffer
];

/// The internal I/O area as the BIOS leaves it (NeoPop's `systemMemory`), for the registers
/// this models: the timers, the interrupt levels, the sound and Z80 switches (both off).
const START_IO: [(u8, u8); 14] = [
    (0x20, 0x80),
    (0x22, 0x01),
    (0x23, 0x90),
    (0x24, 0x03),
    (0x26, 0x90),
    (0x27, 0x62),
    (0x28, 0x05),
    (0x70, 0x02),
    (0x71, 0x32),
    (0x72, 0x00),
    (0x73, 0x00),
    (0x74, 0x00),
    (0xB8, 0xAA),
    (0xB9, 0xAA),
];

/// The clock (0x91-0x97: year, month, day, hours, minutes, seconds, weekday), in BCD: a
/// fixed moment, so runs repeat exactly.
const CLOCK_REGS: [u8; 7] = [0x00, 0x01, 0x01, 0x12, 0x00, 0x00, 0x06];

/// One half of the T6W28, as its registers stand: for bringing a fresh chip up to date.
#[derive(Clone, Default)]
struct PsgHalf {
    latch: u8,
    period: [u16; 3],
    volume: [u8; 4],
    noise: u8,
}

impl PsgHalf {
    fn write(&mut self, v: u8) {
        if v & 0x80 != 0 {
            self.latch = (v >> 4) & 7;
        }
        let ch = (self.latch >> 1) as usize;
        match (self.latch & 1, ch) {
            (1, _) => self.volume[ch] = v & 15,
            (0, 3) => self.noise = v & 7,
            (0, _) if v & 0x80 != 0 => self.period[ch] = self.period[ch] & 0x3F0 | (v & 15) as u16,
            (0, _) => self.period[ch] = self.period[ch] & 15 | ((v & 0x3F) as u16) << 4,
            _ => {}
        }
    }

    fn restore(&self, port: u32, out: &mut Vec<Write>) {
        let mut w = |v: u8| {
            out.push(Write {
                at: 0,
                port,
                value: v as u16,
            })
        };
        for ch in 0..3u8 {
            let p = self.period[ch as usize];
            w(0x80 | ch << 5 | (p & 15) as u8);
            w((p >> 4) as u8 & 0x3F);
        }
        w(0xE0 | self.noise);
        for ch in 0..4u8 {
            w(0x90 | ch << 5 | self.volume[ch as usize]);
        }
    }
}

/// Everything around the two CPUs. Plain data: cloned to run again from any moment.
#[derive(Clone)]
struct Board {
    /// The cartridge: 0x200000 on, and past 2 MB at 0x800000.
    rom: Arc<Vec<u8>>,
    /// Work RAM, 0x4000-0x7FFF; the Z80 sees 0x7000-0x7FFF as its 0x0000-0x0FFF.
    ram: Vec<u8>,
    /// Video registers and memory, 0x8000-0xBFFF (read back as written, except the raster
    /// position and status).
    video: Vec<u8>,
    /// Interrupt requests waiting, by NeoPop's numbering (5: vertical blank, 6: Z80, 7-10:
    /// timers 0-3, 11-12: serial), and the levels set at 0x70-0x7A.
    pending: u16,
    prio: [u8; 11],
    /// The interrupts that start a micro DMA instead (0x7C-0x7F); the DMA itself is not
    /// modelled.
    hdma: [u8; 4],
    /// The timers: run flags, modes, counters, thresholds, and clocks into the next count.
    trun: u8,
    t01mod: u8,
    t23mod: u8,
    trdc: u8,
    timer: [u8; 4],
    threshold: [u8; 4],
    timer_clock: [u32; 4],
    /// The sound side: the Z80 running, a reset of it asked for, its NMI and interrupt
    /// line, the byte the CPUs share.
    z80_on: bool,
    z80_reset: bool,
    z80_nmi: bool,
    z80_irq: bool,
    comm: u8,
    psg: [PsgHalf; 2],
    sc0buf: u8,
    start: bool,
    /// Main CPU clocks into the frame.
    cycle: u32,
    sound: Vec<Write>,
    /// The lowest and highest stack addresses seen, of each CPU (the Z80's at main-CPU
    /// addresses).
    stack: (u32, u32),
    sound_stack: (u32, u32),
    /// RAM accesses, when traced: (address, written, value).
    trace: Option<Vec<(u32, bool, u8)>>,
    /// The Z80 instruction's first byte, whose bytes aren't traced as data.
    z80_fetch: Option<u16>,
    /// BIOS calls the stand-in doesn't answer, by BIOS address.
    unknown: Vec<u32>,
}

impl Board {
    fn line(&self) -> u32 {
        self.cycle / LINE
    }

    fn traced(&mut self, addr: u32, write: bool, v: u8) {
        if let Some(t) = self.trace.as_mut() {
            t.push((addr, write, v));
        }
    }

    fn psg(&mut self, port: u32, v: u8) {
        self.psg[port as usize].write(v);
        self.sound.push(Write {
            at: self.cycle,
            port,
            value: v as u16,
        });
    }

    /// An interrupt from a device (NeoPop's `TestIntHDMA`): taken unless it starts a micro
    /// DMA.
    fn request(&mut self, int: u8, vector: u8) {
        if !self.hdma.contains(&vector) {
            self.pending |= 1 << int;
        }
    }

    /// The interrupt to take at mask `iff`: its number and level.
    fn next_interrupt(&self, iff: u8) -> Option<(u8, u8)> {
        let levels = [
            (5, self.prio[1] & 7),
            (6, self.prio[1] >> 4 & 7),
            (7, self.prio[3] & 7),
            (8, self.prio[3] >> 4 & 7),
            (9, self.prio[4] & 7),
            (10, self.prio[4] >> 4 & 7),
            (11, self.prio[7] & 7),
            (12, self.prio[7] >> 4 & 7),
        ];
        levels
            .into_iter()
            .find(|&(i, p)| self.pending & 1 << i != 0 && iff <= p && p != 0 && p != 7)
    }

    fn io_read(&mut self, a: u8) -> u8 {
        let bit = |p: u16, i: u8, b: u8| if p & 1 << i != 0 { b } else { 0 };
        let p = self.pending;
        match a {
            0x20 => self.trun,
            0x29 => self.trdc,
            // NeoPop's reading of the other timer registers.
            0x21..=0x28 => 4,
            0x50 => self.sc0buf,
            0x71 => bit(p, 5, 0x08) | bit(p, 6, 0x80),
            0x73 => bit(p, 7, 0x08) | bit(p, 8, 0x80),
            0x74 => bit(p, 9, 0x08) | bit(p, 10, 0x80),
            0x77 => bit(p, 11, 0x08) | bit(p, 12, 0x80),
            0x91..=0x97 => CLOCK_REGS[(a - 0x91) as usize],
            0xBC => {
                let v = self.comm;
                self.traced(0xBC, false, v);
                v
            }
            _ => 0,
        }
    }

    fn io_write(&mut self, a: u8, v: u8) {
        match a {
            0x20 => {
                self.trun = v;
                for i in 0..4 {
                    if v & 1 << i == 0 {
                        self.timer[i] = 0;
                    }
                }
            }
            0x22 | 0x23 => self.threshold[(a - 0x22) as usize] = v,
            0x24 => self.t01mod = v,
            0x26 | 0x27 => self.threshold[(a - 0x24) as usize] = v,
            0x28 => self.t23mod = v,
            0x29 => self.trdc = v & 3,
            0x50 => self.sc0buf = v,
            0x70..=0x7F => {
                let clear = |p: &mut u16, i: u8, b: u8| {
                    if v & b == 0 {
                        *p &= !(1 << i);
                    }
                };
                match a {
                    0x71 => {
                        clear(&mut self.pending, 5, 0x08);
                        clear(&mut self.pending, 6, 0x80);
                    }
                    0x73 => {
                        clear(&mut self.pending, 7, 0x08);
                        clear(&mut self.pending, 8, 0x80);
                    }
                    0x74 => {
                        clear(&mut self.pending, 9, 0x08);
                        clear(&mut self.pending, 10, 0x80);
                    }
                    0x77 => {
                        clear(&mut self.pending, 11, 0x08);
                        clear(&mut self.pending, 12, 0x80);
                    }
                    0x7C..=0x7F => self.hdma[(a - 0x7C) as usize] = v,
                    _ => {}
                }
                if a <= 0x7A {
                    self.prio[(a - 0x70) as usize] = v;
                }
            }
            // The PSG straight from the main CPU, which only reaches it with the Z80 off.
            0xA0 if !self.z80_on => self.psg(PSG_RIGHT, v),
            0xA1 if !self.z80_on => self.psg(PSG_LEFT, v),
            0xB9 => match v {
                0x55 => self.z80_on = true,
                0xAA => {
                    self.z80_on = false;
                    self.z80_reset = true;
                }
                _ => {}
            },
            0xBA if self.z80_on => self.z80_nmi = true,
            0xBC => {
                self.traced(0xBC, true, v);
                self.comm = v;
            }
            _ => {}
        }
    }

    fn video_read(&self, a: u32) -> u8 {
        match a {
            0x8008 => (LINE - self.cycle % LINE).min(255 * 4) as u8 >> 2,
            0x8009 => self.line().min(255) as u8,
            0x8010 => {
                if self.line() >= VBLANK_LINE {
                    0x40
                } else {
                    0
                }
            }
            _ => self.video[(a - 0x8000) as usize],
        }
    }

    fn bios_read(a: u32) -> u8 {
        let off = a & 0xFFFF;
        if (0xFE00..0xFE00 + 4 * BIOS_CALLS.len() as u32).contains(&off) {
            let i = (off - 0xFE00) as usize;
            return (BIOS_CALLS[i / 4] >> (8 * (i % 4))) as u8;
        }
        if a == BIOS_RETI {
            return 0x07;
        }
        0
    }

    fn rom_at(&self, off: usize) -> u8 {
        self.rom.get(off).copied().unwrap_or(0)
    }

    /// Advances the clocks by `t`: scanlines (the vertical blank and scanline interrupts) and
    /// the timers, as NeoPop's `updateTimers` does.
    fn advance(&mut self, t: u32) {
        let mut left = t;
        while left > 0 {
            let d = left.min(LINE - self.cycle % LINE);
            self.cycle += d;
            left -= d;
            let mut h_int = false;
            if self.cycle % LINE == 0 {
                // The line that just ended.
                let line = self.cycle / LINE - 1;
                let control = self.video[0];
                let period = self.video[6] as u32;
                h_int = (line < VBLANK_LINE - 1 || line == period) && control & 0x40 != 0;
                if line + 1 == VBLANK_LINE && control & 0x80 != 0 {
                    self.request(5, 0x0B);
                }
            }
            self.timers(d, h_int);
        }
    }

    fn timers(&mut self, d: u32, h_int: bool) {
        const T1: u32 = 256;
        const T4: u32 = 1024;
        const T16: u32 = 4096;
        const T256: u32 = 65536;
        fn count(timer: &mut u8, clock: &mut u32, rate: u32) {
            let n = *clock / rate;
            *timer = timer.wrapping_add(n as u8);
            *clock -= n * rate;
        }
        for c in self.timer_clock.iter_mut() {
            *c = c.wrapping_add(d);
        }
        let mut timer0 = false;
        if self.trun & 1 != 0 {
            match self.t01mod & 3 {
                0 => {
                    if h_int {
                        self.timer[0] = self.timer[0].wrapping_add(1);
                        self.timer_clock[0] = 0;
                    }
                }
                m => count(&mut self.timer[0], &mut self.timer_clock[0], [0, T1, T4, T16][m as usize]),
            }
            if self.threshold[0] != 0 && self.timer[0] >= self.threshold[0] {
                self.timer[0] = 0;
                timer0 = true;
                self.request(7, 0x10);
            }
        }
        if self.trun & 2 != 0 {
            match self.t01mod >> 2 & 3 {
                0 => {
                    if timer0 {
                        self.timer[1] = self.timer[1].wrapping_add(1);
                        self.timer_clock[1] = 0;
                    }
                }
                m => count(&mut self.timer[1], &mut self.timer_clock[1], [0, T1, T16, T256][m as usize]),
            }
            if self.threshold[1] != 0 && self.timer[1] >= self.threshold[1] {
                self.timer[1] = 0;
                self.request(8, 0x11);
            }
        }
        let mut timer2 = false;
        if self.trun & 4 != 0 {
            match self.t23mod & 3 {
                0 => {}
                // NeoPop's rate for T1 on timer 2.
                m => count(&mut self.timer[2], &mut self.timer_clock[2], [0, T1 / 2, T4, T16][m as usize]),
            }
            if self.threshold[2] != 0 && self.timer[2] >= self.threshold[2] {
                self.timer[2] = 0;
                timer2 = true;
                self.request(9, 0x12);
            }
        }
        if self.trun & 8 != 0 {
            match self.t23mod >> 2 & 3 {
                0 => {
                    if timer2 {
                        self.timer[3] = self.timer[3].wrapping_add(1);
                        self.timer_clock[3] = 0;
                    }
                }
                m => count(&mut self.timer[3], &mut self.timer_clock[3], [0, T1, T16, T256][m as usize]),
            }
            if self.threshold[3] != 0 && self.timer[3] >= self.threshold[3] {
                self.timer[3] = 0;
                self.z80_irq = true;
                self.request(10, 0x13);
            }
        }
    }
}

impl tlcs900h::Bus for Board {
    fn read(&mut self, a: u32) -> u8 {
        match a {
            0x00..=0xFF => self.io_read(a as u8),
            0x4000..=0x7FFF => {
                let v = self.ram[(a - 0x4000) as usize];
                self.traced(a, false, v);
                v
            }
            0x8000..=0xBFFF => self.video_read(a),
            0x20_0000..=0x3F_FFFF => self.rom_at((a - 0x20_0000) as usize),
            0x80_0000..=0x9F_FFFF => self.rom_at((a - 0x80_0000) as usize + 0x20_0000),
            0xFF_0000..=0xFF_FFFF => Self::bios_read(a),
            _ => 0,
        }
    }

    fn write(&mut self, a: u32, v: u8) {
        match a {
            0x00..=0xFF => self.io_write(a as u8, v),
            0x4000..=0x7FFF => {
                // Loading the Z80's memory while it is held in reset is loading its program,
                // not handing it a request: not traced.
                if a < 0x7000 || self.z80_on {
                    self.traced(a, true, v);
                }
                self.ram[(a - 0x4000) as usize] = v;
            }
            0x8000..=0xBFFF => self.video[(a - 0x8000) as usize] = v,
            _ => {}
        }
    }
}

/// The Z80's view of the board.
struct SoundBus<'a>(&'a mut Board);

impl SoundBus<'_> {
    fn traced(&mut self, a: u16, write: bool, v: u8) {
        // The instruction's own bytes are code, not data.
        if let Some(pc) = self.0.z80_fetch {
            if !write && a.wrapping_sub(pc) < 4 {
                return;
            }
        }
        self.0.traced(0x7000 + a as u32, write, v);
    }
}

impl z80::Bus for SoundBus<'_> {
    fn read(&mut self, a: u16) -> u8 {
        match a {
            0x0000..=0x0FFF => {
                let v = self.0.ram[0x3000 + a as usize];
                self.traced(a, false, v);
                v
            }
            0x8000 => {
                let v = self.0.comm;
                self.0.traced(0xBC, false, v);
                v
            }
            _ => 0,
        }
    }

    fn write(&mut self, a: u16, v: u8) {
        match a {
            0x0000..=0x0FFF => {
                self.traced(a, true, v);
                self.0.ram[0x3000 + a as usize] = v;
            }
            0x8000 => {
                self.0.traced(0xBC, true, v);
                self.0.comm = v;
            }
            0x4000 => self.0.psg(PSG_RIGHT, v),
            0x4001 => self.0.psg(PSG_LEFT, v),
            0xC000 => self.0.request(6, 0x0C),
            _ => {}
        }
    }

    fn input(&mut self, _: u16) -> u8 {
        0
    }

    fn output(&mut self, _: u16, _: u8) {
        // Any port write acknowledges the timer interrupt.
        self.0.z80_irq = false;
    }
}

/// A moment a CPU entered a routine: enough to call it again from there.
#[derive(Clone)]
pub enum Entry {
    Main {
        cpu: MainCpu,
        /// What the call (or interrupt) pushed, and the stack pointer before it did.
        pushed: Vec<u8>,
        sp_before: u32,
        /// Main CPU clocks into the frame.
        cycle: u32,
    },
    Sound {
        cpu: SoundCpu,
        pushed: Vec<u8>,
        sp_before: u16,
        cycle: u32,
    },
}

/// Which CPU ran an instruction.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Main,
    Sound,
}

/// A Neo Geo Pocket (Color): both CPUs and the board.
#[derive(Clone)]
pub struct NeoGeoPocket {
    cpu: MainCpu,
    z80: SoundCpu,
    /// Main CPU clocks the Z80 is owed (it runs at half the main clock).
    z80_owed: i64,
    board: Board,
    last: Side,
}

/// Main-CPU register codes a song number can be passed in: A, W, C, B, E, D, L.
const MAIN_REGS: [u8; 7] = [0xE0, 0xE1, 0xE4, 0xE5, 0xE8, 0xE9, 0xEC];

impl NeoGeoPocket {
    /// BIOS calls the stand-in didn't answer, by their BIOS address (system calls through
    /// SWI 1 that have none: 0xFFFE00 + 4 x number; other software interrupts: 0xFFFF00 +
    /// 4 x number).
    pub fn unknown_bios_calls(&self) -> &[u32] {
        &self.board.unknown
    }

    fn unknown(&mut self, at: u32) {
        if !self.board.unknown.contains(&at) {
            log::debug!("ngp: BIOS call {at:06X} not answered");
            self.board.unknown.push(at);
        }
    }

    /// The BIOS at `pc`: the stand-in answers the system call there and returns.
    fn bios(&mut self) -> u32 {
        let pc = self.cpu.pc & 0xFF_FFFF;
        if pc == BIOS_RETI {
            self.cpu.reti(&mut self.board);
            return 12;
        }
        let call = BIOS_CALLS.iter().position(|&a| a == pc);
        let reg = |c: &MainCpu, code: u8, sz: u32| c.reg(code, sz);
        match call {
            // Shutdown: nothing more runs.
            Some(0x00) => {
                self.cpu.halted = true;
                return 8;
            }
            Some(0x01 | 0x0E | 0x05 | 0x11 | 0x12 | 0x15 | 0x16 | 0x1A) => {}
            Some(0x02) => {
                // The clock, to (XHL3).
                let at = reg(&self.cpu, 0x3C, 4);
                if at < 0xC000 {
                    for (i, &v) in CLOCK_REGS.iter().enumerate() {
                        tlcs900h::Bus::write(&mut self.board, at + i as u32, v);
                    }
                }
            }
            Some(0x04) => {
                // An interrupt's level: RC3 is the interrupt, RB3 the level.
                let level = reg(&self.cpu, 0x35, 1) as u8 & 7;
                let n = reg(&self.cpu, 0x34, 1) as u8;
                // 0: the clock's alarm, 1: the Z80, 2-5: timers 0-3, 6-9: DMA ends; two
                // to a register, the odd ones in the high half.
                let at = [0, 1, 3, 3, 4, 4, 9, 9, 10, 10];
                if let Some(&i) = at.get(n as usize) {
                    let old = self.board.prio[i];
                    let v = if n & 1 == 0 {
                        old & 0xF0 | level
                    } else {
                        old & 0x0F | level << 4
                    };
                    self.board.io_write(0x70 + i as u8, v);
                }
            }
            Some(0x06..=0x09 | 0x0B | 0x0D | 0x10 | 0x13) => self.cpu.set_reg(0x30, 1, 0),
            // Link cable: nothing is ever received.
            Some(0x14) => self.cpu.set_reg(0x30, 1, 1),
            Some(0x17 | 0x18) => self.cpu.set_reg(0x30, 2, 0),
            Some(0x19) => {
                let n = reg(&self.cpu, 0x35, 1);
                let at = reg(&self.cpu, 0x3C, 4);
                self.cpu.set_reg(0x3C, 4, at.wrapping_add(n));
                self.cpu.set_reg(0x35, 1, 0);
            }
            _ => self.unknown(pc),
        }
        self.cpu.ret_now(&mut self.board);
        40
    }

    /// A software interrupt the main CPU just made: SWI 1 is a system call (number in RW3),
    /// the rest do nothing here.
    fn software_interrupt(&mut self, n: u8) {
        if n == 1 {
            let call = self.cpu.reg(0x31, 1) & 0x1F;
            match BIOS_CALLS.get(call as usize) {
                Some(&at) => {
                    let pc = self.cpu.pc;
                    self.cpu.push(&mut self.board, 4, pc);
                    self.cpu.pc = at;
                }
                None => self.unknown(0xFF_FE00 + 4 * call),
            }
        } else {
            self.unknown(0xFF_FF00 + 4 * n as u32);
        }
    }

    /// One main-CPU instruction (with the BIOS stand-in and interrupts). Returns its clocks.
    fn main_step(&mut self) -> u32 {
        let mut t = 0;
        if let Some((int, level)) = self.board.next_interrupt(self.cpu.iff) {
            self.board.pending &= !(1 << int);
            let at = (INT_TABLE + 4 * int as u32 - 0x4000) as usize;
            let v = &self.board.ram[at..at + 3];
            let target = v[0] as u32 | (v[1] as u32) << 8 | (v[2] as u32) << 16;
            self.cpu.interrupt_to(&mut self.board, target, level);
            t += 36;
        }
        t += if self.cpu.pc & 0xFF_0000 == 0xFF_0000 {
            self.bios()
        } else if self.cpu.halted {
            // Asleep until an interrupt: on to the next scanline.
            LINE - self.board.cycle % LINE
        } else {
            let t = self.cpu.step(&mut self.board);
            if let Some(n) = self.cpu.swi.take() {
                self.software_interrupt(n);
            }
            t
        };
        t
    }

    /// One Z80 instruction (or its interrupt). Returns its T-states.
    fn sound_step(&mut self) -> u32 {
        let b = &mut self.board;
        let mut t = 0;
        if b.z80_nmi {
            b.z80_nmi = false;
            t = self.z80.interrupt(&mut SoundBus(b), true);
        } else if b.z80_irq {
            t = self.z80.interrupt(&mut SoundBus(b), false);
        }
        if t == 0 {
            b.z80_fetch = Some(self.z80.pc);
            t = self.z80.step(&mut SoundBus(b));
            b.z80_fetch = None;
        }
        t
    }

    /// The next instruction of either CPU, in step: the Z80 catches up after each main-CPU
    /// instruction, as in NeoPop.
    fn step_once(&mut self) -> Side {
        if self.board.z80_reset {
            self.board.z80_reset = false;
            self.z80 = SoundCpu::reset();
            self.z80_owed = 0;
        }
        if self.board.z80_on && self.z80_owed > 0 {
            let t = self.sound_step();
            self.z80_owed -= 2 * t as i64;
            return Side::Sound;
        }
        let t = self.main_step();
        self.board.advance(t);
        if self.board.z80_on {
            self.z80_owed += t as i64;
        }
        Side::Main
    }

    fn sound_stack_addr(sp: u16) -> u32 {
        0x7000 + (sp as u32 & 0xFFF)
    }

    /// Runs the routine of `entry` once, alone, from its own registers, until it returns.
    /// False when it doesn't within `budget` main-CPU clocks.
    fn call(&mut self, entry: &Entry, set: Option<(u8, u8)>, budget: u32) -> bool {
        let start = self.board.cycle;
        match entry {
            Entry::Main {
                cpu,
                pushed,
                sp_before,
                ..
            } => {
                self.cpu = cpu.clone();
                self.cpu.halted = false;
                if let Some((reg, v)) = set {
                    self.cpu.set_reg(MAIN_REGS[reg as usize % 7], 1, v as u32);
                }
                let sp = self.cpu.r[XSP];
                for (i, &b) in pushed.iter().enumerate() {
                    tlcs900h::Bus::write(&mut self.board, sp.wrapping_add(i as u32), b);
                }
                self.cpu.calls = vec![(self.cpu.pc, *sp_before)];
                while !self.cpu.calls.is_empty() {
                    if self.board.cycle - start > budget || self.cpu.halted {
                        return false;
                    }
                    let t = if self.cpu.pc & 0xFF_0000 == 0xFF_0000 {
                        self.bios()
                    } else {
                        let t = self.cpu.step(&mut self.board);
                        if let Some(n) = self.cpu.swi.take() {
                            self.software_interrupt(n);
                        }
                        t
                    };
                    self.board.cycle += t;
                }
                // A request it handed the Z80 with an NMI is taken now.
                if self.board.z80_nmi && self.board.z80_on {
                    let z = self.z80.clone();
                    let ok = self.sound_nmi(budget);
                    self.z80 = z;
                    return ok;
                }
                true
            }
            Entry::Sound {
                cpu,
                pushed,
                sp_before,
                ..
            } => {
                self.z80 = cpu.clone();
                self.z80.halted = false;
                if let Some((reg, v)) = set {
                    let z = &mut self.z80;
                    match reg {
                        0 => z.a = v,
                        1 => z.b = v,
                        2 => z.c = v,
                        3 => z.d = v,
                        4 => z.e = v,
                        5 => z.h = v,
                        _ => z.l = v,
                    }
                }
                let sp = self.z80.sp;
                for (i, &b) in pushed.iter().enumerate() {
                    z80::Bus::write(&mut SoundBus(&mut self.board), sp.wrapping_add(i as u16), b);
                }
                self.z80.calls = vec![(self.z80.pc, *sp_before)];
                self.run_sound(start, budget)
            }
        }
    }

    /// Runs the Z80's routines in progress until they return.
    fn run_sound(&mut self, start: u32, budget: u32) -> bool {
        while !self.z80.calls.is_empty() {
            if self.board.cycle - start > budget || self.z80.halted {
                return false;
            }
            self.board.z80_fetch = Some(self.z80.pc);
            let t = self.z80.step(&mut SoundBus(&mut self.board));
            self.board.z80_fetch = None;
            self.board.cycle += 2 * t;
        }
        true
    }

    /// Takes the Z80's NMI and runs its handler to the end.
    fn sound_nmi(&mut self, budget: u32) -> bool {
        self.board.z80_nmi = false;
        self.z80.calls.clear();
        self.z80.interrupt(&mut SoundBus(&mut self.board), true);
        let start = self.board.cycle;
        self.run_sound(start, budget)
    }
}

/// The PSG: the T6W28's two halves.
pub struct Chip(Chips);

impl Machine for NeoGeoPocket {
    type Entry = Entry;
    type Chip = Chip;
    const REGISTERS: u8 = 7;
    const CHANNELS: u16 = 2;

    fn boot(rom: &[u8]) -> Result<Self, String> {
        if rom.len() < 0x40 {
            return Err("not a Neo Geo Pocket cartridge (too small)".into());
        }
        let start = u32::from_le_bytes(rom[0x1C..0x20].try_into().unwrap()) & 0xFF_FFFF;
        let in_rom = |a: u32| {
            (0x20_0000..0x20_0000 + rom.len().min(0x20_0000) as u32).contains(&a)
                || rom.len() > 0x20_0000
                    && (0x80_0000..0x80_0000 + (rom.len() - 0x20_0000) as u32).contains(&a)
        };
        if !in_rom(start) {
            return Err("not a Neo Geo Pocket cartridge (no start address)".into());
        }
        let color = rom[0x23] == 0x10;
        let mut board = Board {
            rom: Arc::new(rom.to_vec()),
            ram: vec![0; 0x4000],
            video: vec![0; 0x4000],
            pending: 0,
            prio: [0; 11],
            hdma: [0; 4],
            trun: 0,
            t01mod: 0,
            t23mod: 0,
            trdc: 0,
            timer: [0; 4],
            threshold: [0; 4],
            timer_clock: [0; 4],
            z80_on: false,
            z80_reset: false,
            z80_nmi: false,
            z80_irq: false,
            comm: 0,
            psg: Default::default(),
            sc0buf: 0,
            start: false,
            cycle: 0,
            sound: Vec::new(),
            stack: (u32::MAX, 0),
            sound_stack: (u32::MAX, 0),
            trace: None,
            z80_fetch: None,
            unknown: Vec::new(),
        };
        for (a, v) in START_IO {
            board.io_write(a, v);
        }
        board.z80_reset = false;
        // The BIOS's work area (NeoPop's `reset_memory`).
        let mut put = |a: u32, v: u8| board.ram[(a - 0x4000) as usize] = v;
        for (i, b) in start.to_le_bytes().into_iter().enumerate() {
            put(0x6C00 + i as u32, b);
        }
        for (a, v) in [
            (0x6C04, rom[0x20]),
            (0x6C05, rom[0x21]),
            (0x6E82, rom[0x20]),
            (0x6E83, rom[0x21]),
            (0x6C06, rom[0x22]),
            (0x6E84, rom[0x22]),
            (0x6C58, 0x01),
            (0x6C59, (rom.len() > 0x20_0000) as u8),
            (0x6C55, 0x01),
            (0x6F80, 0xFF),
            (0x6F81, 0x03),
            (0x6F84, 0x40),
            (0x6F85, 0x00),
            (0x6F86, 0x00),
            // English.
            (0x6F87, 0x01),
            (0x6F91, rom[0x23]),
            (0x6F95, rom[0x23]),
        ] {
            put(a, v);
        }
        for i in 0..12 {
            put(0x6C08 + i, rom[0x24 + i as usize]);
        }
        for i in 0..0x12 {
            for (k, b) in BIOS_RETI.to_le_bytes().into_iter().enumerate() {
                put(INT_TABLE + 4 * i + k as u32, b);
            }
        }
        // Video: both interrupts on, the frame length, the window and colours as the BIOS
        // leaves them.
        for (a, v) in [
            (0x8000, 0xC0),
            (0x8004, 0xFF),
            (0x8005, 0xFF),
            (0x8006, 0xC6),
            (0x8118, 0x80),
            (0x83E0, 0xFF),
            (0x83E1, 0x0F),
            (0x83F0, 0xFF),
            (0x83F1, 0x0F),
            (0x8400, 0xFF),
            (0x8402, 0x80),
            (0x87E2, if color { 0x00 } else { 0x80 }),
        ] {
            board.video[a - 0x8000] = v;
        }
        let mut cpu = MainCpu::reset();
        cpu.pc = start;
        cpu.set_sr(0xF800);
        cpu.r[tlcs900h::XIX] = 0xFF23C3;
        cpu.r[tlcs900h::XIY] = 0xFF23DF;
        cpu.r[tlcs900h::XIZ] = 0x6480;
        cpu.r[XSP] = 0x6C00;
        cpu.swi_trap = true;
        Ok(Self {
            cpu,
            z80: SoundCpu::reset(),
            z80_owed: 0,
            board,
            last: Side::Main,
        })
    }

    fn run_frame(
        &mut self,
        mut watch: Option<&mut dyn FnMut(&Step)>,
        stop_after: Option<u64>,
    ) -> bool {
        let keys = if self.board.start { START_BUTTONS } else { 0 };
        self.board.ram[(JOYPAD - 0x4000) as usize] = keys;
        let mut n = 0u64;
        let mut calls: Vec<(u32, u32)> = Vec::new();
        let mut sound: Vec<Write> = Vec::new();
        let mut ram: Vec<(u32, bool, u8)> = Vec::new();
        while self.board.cycle < FRAME {
            let before = match self.board.z80_on && self.z80_owed > 0 {
                true => self.z80.calls.len(),
                false => self.cpu.calls.len(),
            };
            let sound_at = self.board.sound.len();
            if let Some(t) = self.board.trace.as_mut() {
                t.clear();
            }
            let side = self.step_once();
            self.last = side;
            n += 1;
            let now = match side {
                Side::Main => self.cpu.calls.len(),
                Side::Sound => self.z80.calls.len(),
            };
            if now > before {
                match side {
                    Side::Main => {
                        let s = self.cpu.r[XSP];
                        let (lo, hi) = self.board.stack;
                        self.board.stack = (lo.min(s), hi.max(s + 6));
                    }
                    Side::Sound => {
                        let s = Self::sound_stack_addr(self.z80.sp);
                        let (lo, hi) = self.board.sound_stack;
                        self.board.sound_stack = (lo.min(s), hi.max(s + 4));
                    }
                }
            }
            if let Some(w) = watch.as_deref_mut() {
                calls.clear();
                match side {
                    Side::Main => calls.extend_from_slice(&self.cpu.calls),
                    Side::Sound => calls.extend(
                        self.z80
                            .calls
                            .iter()
                            .map(|&(r, s)| (SOUND_CPU | r as u32, SOUND_CPU | s as u32)),
                    ),
                }
                sound.clear();
                sound.extend_from_slice(&self.board.sound[sound_at..]);
                ram.clear();
                if let Some(t) = self.board.trace.as_ref() {
                    ram.extend_from_slice(t);
                }
                let entered = calls.get(before.min(calls.len())..).unwrap_or(&[]);
                w(&Step {
                    calls: &calls,
                    entered,
                    sound: &sound,
                    ram: &ram,
                });
            }
            if stop_after == Some(n) {
                return true;
            }
        }
        self.board.cycle -= FRAME;
        false
    }

    fn take_sound(&mut self) -> Vec<Write> {
        std::mem::take(&mut self.board.sound)
    }

    fn set_trace(&mut self, on: bool) {
        self.board.trace = on.then(Vec::new);
    }

    fn press_start(&mut self, held: bool) {
        self.board.start = held;
    }

    fn entry(&self) -> Entry {
        match self.last {
            Side::Main => {
                let mut cpu = self.cpu.clone();
                let sp = self.cpu.r[XSP];
                let sp_before = self.cpu.calls.last().map_or(sp, |c| c.1);
                cpu.calls.clear();
                let n = sp_before.wrapping_sub(sp).min(16);
                let pushed = (0..n)
                    .map(|i| {
                        let a = sp.wrapping_add(i);
                        if (0x4000..0x8000).contains(&a) {
                            self.board.ram[(a - 0x4000) as usize]
                        } else {
                            0
                        }
                    })
                    .collect();
                Entry::Main {
                    cpu,
                    pushed,
                    sp_before,
                    cycle: self.board.cycle,
                }
            }
            Side::Sound => {
                let mut cpu = self.z80.clone();
                let sp = self.z80.sp;
                let sp_before = self.z80.calls.last().map_or(sp, |c| c.1);
                cpu.calls.clear();
                let n = sp_before.wrapping_sub(sp).min(16);
                let pushed = (0..n)
                    .map(|i| {
                        let a = sp.wrapping_add(i);
                        if a < 0x1000 {
                            self.board.ram[0x3000 + a as usize]
                        } else {
                            0
                        }
                    })
                    .collect();
                Entry::Sound {
                    cpu,
                    pushed,
                    sp_before,
                    cycle: self.board.cycle,
                }
            }
        }
    }

    fn play_frame(&mut self, play: &Entry) -> Option<Vec<Write>> {
        self.board.cycle = match play {
            Entry::Main { cycle, .. } | Entry::Sound { cycle, .. } => *cycle,
        };
        let ok = self.call(play, None, FRAME * 4);
        self.board.cycle = 0;
        let writes = self.take_sound();
        ok.then_some(writes)
    }

    fn call_with(&mut self, entry: &Entry, reg: u8, v: u8) -> bool {
        let cycle = self.board.cycle;
        let (cpu, z80) = (self.cpu.clone(), self.z80.clone());
        let ok = self.call(entry, Some((reg, v)), 8_000_000);
        self.board.cycle = cycle;
        self.cpu = cpu;
        self.z80 = z80;
        for w in self.board.sound.iter_mut() {
            w.at = cycle;
        }
        ok
    }

    fn poke(&mut self, addr: u32, v: u8) {
        match addr {
            0x4000..=0x7FFF => self.board.ram[(addr - 0x4000) as usize] = v,
            0xBC => self.board.comm = v,
            _ => {}
        }
    }

    fn incidental(_w: &Write) -> bool {
        false
    }

    fn is_stack(&self, addr: u32) -> bool {
        [self.board.stack, self.board.sound_stack]
            .iter()
            .any(|&(lo, hi)| lo != u32::MAX && (lo.saturating_sub(0x10)..=hi).contains(&addr))
    }

    fn restore_writes(&self) -> Vec<Write> {
        let mut out = Vec::new();
        self.board.psg[0].restore(PSG_LEFT, &mut out);
        self.board.psg[1].restore(PSG_RIGHT, &mut out);
        out
    }

    fn new_chip(&self, rate: u32) -> Result<Chip, String> {
        Ok(Chip(Chips::new(
            rate,
            CLOCK as f64,
            &[(Kind::T6W28Tone, PSG_CLOCK), (Kind::T6W28Noise, PSG_CLOCK)],
        )?))
    }

    fn feed(&self, chip: &mut Chip, writes: &[Write], out: &mut Vec<i16>) {
        for w in writes {
            let at = w.at.min(FRAME - 1);
            chip.0.write(at, w.port as u8, 0, w.value as u8);
        }
        chip.0.end_frame(FRAME, out);
    }

    fn frame_rate(&self) -> f64 {
        RATE
    }

    fn frame_length(&self) -> u32 {
        FRAME
    }
}

#[cfg(test)]
mod tests {
    use super::super::music_search::{measure, search, Trigger};
    use super::*;

    /// The Z80 sound driver of the tiny game, loaded at the Z80's 0000: it waits; its NMI
    /// (sent by the main CPU every vertical blank) calls the play routine at 0100, which
    /// takes a new song from the shared byte (8000) and, every 8 frames, sets channel 0's
    /// period to 0100h + song * 16 + note through 16 notes, at full volume on both sides.
    fn driver() -> Vec<u8> {
        let mut z = vec![0u8; 0x150];
        // DI; LD SP,0FF0h; IM 1; JR $
        z[..8].copy_from_slice(&[0xF3, 0x31, 0xF0, 0x0F, 0xED, 0x56, 0x18, 0xFE]);
        // NMI: PUSH AF; PUSH HL; CALL 0100h; POP HL; POP AF; RETN
        z[0x66..0x6F].copy_from_slice(&[0xF5, 0xE5, 0xCD, 0x00, 0x01, 0xE1, 0xF1, 0xED, 0x45]);
        let play = [
            0x3A, 0x00, 0x80, // LD A,(8000h)
            0xB7, // OR A
            0x28, 0x0A, // JR Z,+10
            0x32, 0x00, 0x02, // LD (0200h),A: the song
            0xAF, // XOR A
            0x32, 0x00, 0x80, // LD (8000h),A
            0x32, 0x01, 0x02, // LD (0201h),A: the frame count
            0x3A, 0x00, 0x02, // LD A,(0200h)
            0xB7, // OR A
            0xC8, // RET Z
            0x21, 0x01, 0x02, // LD HL,0201h
            0x7E, // LD A,(HL)
            0x34, // INC (HL)
            0xE6, 0x07, // AND 7
            0xC0, // RET NZ
            0x7E, // LD A,(HL)
            0x0F, 0x0F, 0x0F, // RRCA x3
            0xE6, 0x0F, // AND 0Fh: the note
            0x47, // LD B,A
            0x3A, 0x00, 0x02, // LD A,(0200h)
            0x87, 0x87, 0x87, 0x87, // ADD A,A x4
            0x80, // ADD A,B
            0x4F, // LD C,A
            0xE6, 0x0F, // AND 0Fh
            0xF6, 0x80, // OR 80h: channel 0's period, low bits
            0x32, 0x01, 0x40, // LD (4001h),A
            0x79, // LD A,C
            0x0F, 0x0F, 0x0F, 0x0F, // RRCA x4
            0xE6, 0x0F, // AND 0Fh
            0xF6, 0x10, // OR 10h: high bits
            0x32, 0x01, 0x40, // LD (4001h),A
            0x3E, 0x90, // LD A,90h: channel 0 at full volume
            0x32, 0x01, 0x40, // LD (4001h),A
            0x32, 0x00, 0x40, // LD (4000h),A
            0xC9, // RET
        ];
        z[0x100..0x100 + play.len()].copy_from_slice(&play);
        z
    }

    /// A tiny game: the main program loads the driver into the shared RAM with the Z80
    /// stopped, asks for song 1 through the shared byte, starts the Z80, and halts; its
    /// vertical blank handler sends the Z80 an NMI.
    fn tiny_game() -> Vec<u8> {
        let mut rom = vec![0u8; 0x400];
        rom[0x1C..0x20].copy_from_slice(&0x0020_0040u32.to_le_bytes());
        rom[0x23] = 0x10;
        rom[0x24..0x30].copy_from_slice(b"TINY GAME   ");
        let main = [
            0x08, 0xB9, 0xAA, // LD (B9h),AAh: Z80 off
            0x42, 0x00, 0x70, 0x00, 0x00, // LD XDE,7000h
            0x43, 0x00, 0x01, 0x20, 0x00, // LD XHL,200100h
            0x31, 0x50, 0x01, // LD BC,150h
            0x83, 0x11, // LDIR
            0x08, 0xBC, 0x01, // LD (BCh),1: song 1
            0x08, 0xB9, 0x55, // LD (B9h),55h: Z80 on
            0x40, 0x80, 0x00, 0x20, 0x00, // LD XWA,200080h
            0xF1, 0xCC, 0x6F, 0x60, // LD (6FCCh),XWA: the vertical blank vector
            0x06, 0x00, // EI 0
            0x05, // HALT
            0x68, 0xFD, // JR $-3
        ];
        rom[0x40..0x40 + main.len()].copy_from_slice(&main);
        // Vertical blank: LD (BAh),0 (the Z80's NMI); RETI
        rom[0x80..0x84].copy_from_slice(&[0x08, 0xBA, 0x00, 0x07]);
        let d = driver();
        rom[0x100..0x100 + d.len()].copy_from_slice(&d);
        rom
    }

    #[test]
    fn the_tiny_game_runs() {
        let mut m = NeoGeoPocket::boot(&tiny_game()).unwrap();
        for _ in 0..20 {
            m.run_frame(None, None);
        }
        let w = m.take_sound();
        assert!(w.iter().any(|w| w.port == PSG_LEFT && w.value == 0x90), "{}", w.len());
        assert!(w.iter().any(|w| w.port == PSG_RIGHT && w.value == 0x90));
        assert_eq!(m.board.ram[0x3200], 1);
        assert!(m.unknown_bios_calls().is_empty());
    }

    #[test]
    fn the_tiny_game_has_its_songs() {
        if !super::super::vgm_chips::available() {
            return;
        }
        let rom = tiny_game();
        let found = search::<NeoGeoPocket>(&rom).unwrap();
        assert_eq!(found.why, None);
        assert_eq!(found.recipe.play, SOUND_CPU | 0x100);
        assert_eq!(found.recipe.trigger, Trigger::Byte(0xBC));
        assert_eq!(found.songs.len(), 16, "{:?}", found.songs);
        let t = measure::<NeoGeoPocket>(&rom, &found.recipe, 3).unwrap();
        assert_eq!(t.looping.map(|l| l.1), Some(128), "{t:?}");
    }

    /// A tiny game without the Z80: its vertical blank handler calls a play routine on the
    /// main CPU that writes the PSG itself, the song number at 4100h (1 at boot), the same
    /// tune as [`driver`]'s.
    fn tiny_main_cpu_game() -> Vec<u8> {
        let mut rom = vec![0u8; 0x400];
        rom[0x1C..0x20].copy_from_slice(&0x0020_0040u32.to_le_bytes());
        rom[0x24..0x30].copy_from_slice(b"TINY GAME 2 ");
        let main = [
            0xF1, 0x00, 0x41, 0x00, 0x01, // LD (4100h),1
            0x40, 0x80, 0x00, 0x20, 0x00, // LD XWA,200080h
            0xF1, 0xCC, 0x6F, 0x60, // LD (6FCCh),XWA
            0x06, 0x00, // EI 0
            0x05, // HALT
            0x68, 0xFD, // JR $-3
        ];
        rom[0x40..0x40 + main.len()].copy_from_slice(&main);
        // Vertical blank: CALL 200200h; RETI
        rom[0x80..0x85].copy_from_slice(&[0x1D, 0x00, 0x02, 0x20, 0x07]);
        let play = [
            0xC1, 0x00, 0x41, 0x21, // LD A,(4100h)
            0xC9, 0xCC, 0xFF, // AND A,FFh
            0x66, 0x3C, // JR Z,done
            0xC1, 0x01, 0x41, 0x61, // INC 1,(4101h)
            0xC1, 0x01, 0x41, 0x21, // LD A,(4101h)
            0xC9, 0xCC, 0x07, // AND A,7
            0x6E, 0x2F, // JR NZ,done
            0xC1, 0x01, 0x41, 0x20, // LD W,(4101h)
            0xC8, 0xEF, 0x03, // SRL 3,W
            0xC8, 0xCC, 0x0F, // AND W,0Fh: the note
            0xC1, 0x00, 0x41, 0x21, // LD A,(4100h)
            0xC9, 0xEE, 0x04, // SLL 4,A
            0xC8, 0x81, // ADD A,W
            0xC9, 0x88, // LD W,A
            0xC8, 0xCC, 0x0F, // AND W,0Fh
            0xC8, 0xCE, 0x80, // OR W,80h
            0xF0, 0xA1, 0x40, // LD (A1h),W: channel 0's period, low bits
            0xC9, 0xEF, 0x04, // SRL 4,A
            0xC9, 0xCE, 0x10, // OR A,10h
            0xF0, 0xA1, 0x41, // LD (A1h),A: high bits
            0x21, 0x90, // LD A,90h
            0xF0, 0xA1, 0x41, // LD (A1h),A: full volume, left
            0xF0, 0xA0, 0x41, // LD (A0h),A: and right
            0x0E, // done: RET
        ];
        rom[0x200..0x200 + play.len()].copy_from_slice(&play);
        rom
    }

    #[test]
    fn a_main_cpu_driver_is_found_too() {
        if !super::super::vgm_chips::available() {
            return;
        }
        let rom = tiny_main_cpu_game();
        let found = search::<NeoGeoPocket>(&rom).unwrap();
        assert_eq!(found.why, None);
        assert_eq!(found.recipe.play, 0x20_0200);
        assert_eq!(found.recipe.trigger, Trigger::Byte(0x4100));
        assert_eq!(found.songs.len(), 16, "{:?}", found.songs);
    }

    #[test]
    fn system_calls_are_answered() {
        let mut rom = tiny_game();
        // LD RW3,4 (interrupt level); LD RC3,1 (the Z80's); LD RB3,5; SWI 1; then
        // LD RW3,1Fh; SWI 1 (no such call); HALT
        let code = [
            0xC7, 0x31, 0x03, 0x04, 0xC7, 0x34, 0x03, 0x01, 0xC7, 0x35, 0x03, 0x05, 0xF9, 0xC7,
            0x31, 0x03, 0x1F, 0xF9, 0x05,
        ];
        rom[0x40..0x40 + code.len()].copy_from_slice(&code);
        let mut m = NeoGeoPocket::boot(&rom).unwrap();
        m.run_frame(None, None);
        assert_eq!(m.board.prio[1], 0x52);
        assert_eq!(m.unknown_bios_calls(), &[0xFF_FE00 + 4 * 0x1F]);
        assert!(m.cpu.halted);
    }

    #[test]
    fn timer_three_interrupts_the_z80() {
        let mut m = NeoGeoPocket::boot(&tiny_game()).unwrap();
        // Timer 3 on T1 (256 clocks) with a threshold of 2.
        m.board.io_write(0x28, 0x04);
        m.board.io_write(0x27, 2);
        m.board.io_write(0x20, 0x08);
        m.board.advance(256);
        assert!(!m.board.z80_irq);
        m.board.advance(256);
        assert!(m.board.z80_irq);
        assert!(m.board.pending & 1 << 10 != 0);
        m.board.io_write(0x74, 0x00);
        assert!(m.board.pending & 1 << 10 == 0);
    }
}
