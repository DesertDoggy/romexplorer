//! The Mega Drive (Genesis) as a [`Machine`] for the music search ([`super::music_search`]):
//! the 68000 ([`super::m68k`]) and the Z80 sound CPU ([`super::z80`]) side by side, the
//! controller, the Z80's bank window and bus handshake, the FM chip's timers, and just enough
//! of the video chip for the timing games wait on (the vertical and horizontal interrupts,
//! the HV counter). Nothing is drawn. No BIOS. The YM2612 and the PSG go to libvgm's chips
//! ([`super::vgm_chips`]).
//!
//! The sound routine can be on either CPU: Z80 routines are numbered `Z80 | address`.

use std::sync::Arc;

use super::m68k;
use super::music_search::{Machine, Step, Write};
use super::vgm_chips::{Chips, Kind};
use super::z80;

/// 68000 cycles a second (also the YM2612's clock), the Z80's, per scanline, scanlines a
/// frame (NTSC).
const CLOCK: u32 = 7_670_453;
const Z80_CLOCK: u32 = 3_579_545;
const LINE: u32 = 488;
const Z80_LINE: u32 = 228;
const LINES: u32 = 262;
const FRAME: u32 = LINE * LINES;
const VBLANK_LINE: u32 = 224;
pub const RATE: f64 = CLOCK as f64 / FRAME as f64;
/// 68000 cycles of one FM sample (the timers count these).
const FM_SAMPLE: i64 = 144;

/// Routines on the Z80 are numbered from here.
pub const Z80: u32 = 0x0100_0000;
/// Sound ports: the YM2612's registers (`half << 8 | register`) and the PSG.
const PSG: u32 = 0x1000;

/// Everything around the CPUs. Plain data: cloned to run again from any moment.
#[derive(Clone)]
struct Board {
    rom: Arc<Vec<u8>>,
    ram: Vec<u8>,
    zram: Vec<u8>,
    sram: Vec<u8>,
    /// The Z80's window into the 68000's memory ($8000-$FFFF): bits 15-23 of the address.
    bank: u32,
    z80_busreq: bool,
    z80_reset: bool,
    vdp: [u8; 32],
    vdp_pending: Option<u16>,
    vint: bool,
    hint: bool,
    hint_counter: i32,
    /// 68000 cycles into the frame, and the Z80's.
    cycle: u32,
    zcycle: u32,
    th: bool,
    buttons: bool,
    version: u8,
    /// The YM2612: the address latched for each half, its registers, timers and status.
    ym_addr: [u8; 2],
    ym: [[u8; 256]; 2],
    timer_a: i64,
    timer_b: i64,
    ym_status: u8,
    psg: [u16; 8],
    psg_latch: u8,
    sound: Vec<Write>,
    /// Which CPU is writing (for the time of its writes).
    z80_running: bool,
    stack68: (u32, u32),
    stackz: (u16, u16),
    /// RAM accesses, when traced: (address as the 68000 sees it, written, value).
    trace: Option<Vec<(u32, bool, u8)>>,
}

impl Board {
    fn line(&self) -> u32 {
        self.cycle / LINE
    }

    fn now(&self) -> u32 {
        if self.z80_running {
            (self.zcycle as u64 * CLOCK as u64 / Z80_CLOCK as u64) as u32
        } else {
            self.cycle
        }
    }

    fn traced(&mut self, addr: u32, write: bool, v: u8) {
        if let Some(t) = self.trace.as_mut() {
            t.push((addr, write, v));
        }
    }

    fn timer_a_period(&self) -> i64 {
        let n = (self.ym[0][0x24] as i64) << 2 | (self.ym[0][0x25] as i64 & 3);
        (1024 - n) * FM_SAMPLE
    }

    fn timer_b_period(&self) -> i64 {
        (256 - self.ym[0][0x26] as i64) * 16 * FM_SAMPLE
    }

    /// The FM chip's timers, run on by `cycles` 68000 cycles.
    fn clock_timers(&mut self, cycles: u32) {
        let ctl = self.ym[0][0x27];
        if ctl & 1 != 0 {
            self.timer_a -= cycles as i64;
            while self.timer_a <= 0 {
                self.timer_a += self.timer_a_period();
                if ctl & 4 != 0 {
                    self.ym_status |= 1;
                }
            }
        }
        if ctl & 2 != 0 {
            self.timer_b -= cycles as i64;
            while self.timer_b <= 0 {
                self.timer_b += self.timer_b_period();
                if ctl & 8 != 0 {
                    self.ym_status |= 2;
                }
            }
        }
    }

    fn ym_write(&mut self, a: u32, v: u8) {
        let half = ((a >> 1) & 1) as usize;
        if a & 1 == 0 {
            self.ym_addr[half] = v;
            return;
        }
        let reg = self.ym_addr[half];
        if half == 0 && reg == 0x27 {
            let old = self.ym[0][0x27];
            if v & 1 != 0 && old & 1 == 0 {
                self.timer_a = self.timer_a_period();
            }
            if v & 2 != 0 && old & 2 == 0 {
                self.timer_b = self.timer_b_period();
            }
            self.ym_status &= !((v >> 4) & 3);
        }
        self.ym[half][reg as usize] = v;
        self.sound.push(Write {
            at: self.now(),
            port: (half as u32) << 8 | reg as u32,
            value: v as u16,
        });
    }

    fn psg_write(&mut self, v: u8) {
        self.sound.push(Write {
            at: self.now(),
            port: PSG,
            value: v as u16,
        });
        if v & 0x80 != 0 {
            self.psg_latch = (v >> 4) & 7;
            let r = &mut self.psg[self.psg_latch as usize];
            *r = (*r & !0x0F) | (v & 0x0F) as u16;
        } else {
            let r = &mut self.psg[self.psg_latch as usize];
            if self.psg_latch & 1 == 0 && self.psg_latch < 6 {
                *r = (*r & 0x0F) | ((v & 0x3F) as u16) << 4;
            } else {
                *r = (v & 0x0F) as u16;
            }
        }
    }

    fn vdp_control(&mut self, v: u16) {
        if self.vdp_pending.take().is_some() {
            return;
        }
        if v & 0xC000 == 0x8000 {
            let r = ((v >> 8) & 0x1F) as usize;
            self.vdp[r] = v as u8;
        } else {
            self.vdp_pending = Some(v);
        }
    }

    fn vdp_status(&mut self) -> u16 {
        self.vdp_pending = None;
        let l = self.line();
        let vblank = !(0..VBLANK_LINE).contains(&l) || self.vdp[1] & 0x40 == 0;
        let hblank = self.cycle % LINE > LINE * 3 / 4;
        0x3600 | (self.vint as u16) << 7 | (vblank as u16) << 3 | (hblank as u16) << 2
    }

    fn hv(&self) -> u16 {
        let l = self.line();
        let v = if l <= 0xEA { l } else { l - 6 } as u16 & 0xFF;
        let h = ((self.cycle % LINE) * 256 / LINE) as u16;
        v << 8 | h
    }

    fn pad(&self) -> u8 {
        // Three-button pad: TH high reads C B R L D U, low reads Start A 0 0 D U.
        if self.th {
            0x7F
        } else if self.buttons {
            0x13
        } else {
            0x33
        }
    }

    /// The new scanline `l`: the horizontal counter and the vertical interrupt.
    fn new_line(&mut self, l: u32) {
        if l <= VBLANK_LINE {
            self.hint_counter -= 1;
            if self.hint_counter < 0 {
                self.hint_counter = self.vdp[10] as i32;
                self.hint = true;
            }
        } else {
            self.hint_counter = self.vdp[10] as i32;
        }
        if l == VBLANK_LINE {
            self.vint = true;
        }
    }

    fn irq_level(&self) -> u8 {
        if self.vint && self.vdp[1] & 0x20 != 0 {
            6
        } else if self.hint && self.vdp[0] & 0x10 != 0 {
            4
        } else {
            0
        }
    }

    fn z80_area_read(&mut self, a: u32) -> u8 {
        match a & 0xFFFF {
            0x0000..=0x3FFF => {
                let i = (a & 0x1FFF) as usize;
                let v = self.zram[i];
                self.traced(0xA0_0000 | i as u32, false, v);
                v
            }
            0x4000..=0x5FFF => self.ym_status,
            _ => 0xFF,
        }
    }

    fn z80_area_write(&mut self, a: u32, v: u8) {
        match a & 0xFFFF {
            0x0000..=0x3FFF => {
                let i = (a & 0x1FFF) as usize;
                self.zram[i] = v;
                self.traced(0xA0_0000 | i as u32, true, v);
            }
            0x4000..=0x5FFF => self.ym_write(a & 3, v),
            0x6000..=0x60FF => self.bank = (self.bank >> 1) | ((v as u32 & 1) << 8),
            0x7F11 => self.psg_write(v),
            _ => {}
        }
    }

    fn has_sram(&self) -> bool {
        !self.sram.is_empty()
    }

    /// A byte as the 68000 reads it.
    fn r8(&mut self, a: u32) -> u8 {
        let a = a & 0xFF_FFFF;
        match a {
            0x20_0000..=0x20_FFFF if self.has_sram() && self.rom.len() <= 0x20_0000 => {
                self.sram[(a & 0xFFFF) as usize]
            }
            0x00_0000..=0x3F_FFFF => self.rom[a as usize % self.rom.len()],
            0xA0_0000..=0xA0_FFFF => self.z80_area_read(a),
            0xA1_0000..=0xA1_001F => match a & 0x1E {
                0x00 => self.version,
                0x02 => self.pad(),
                0x04 => 0x7F,
                _ => 0,
            },
            0xA1_1100 => (!(self.z80_busreq && !self.z80_reset)) as u8,
            0xA1_1101 => 0,
            0xC0_0000..=0xC0_001F => {
                let w = self.r16(a & !1);
                if a & 1 == 0 {
                    (w >> 8) as u8
                } else {
                    w as u8
                }
            }
            0xE0_0000..=0xFF_FFFF => {
                let v = self.ram[(a & 0xFFFF) as usize];
                self.traced(0xFF_0000 | (a & 0xFFFF), false, v);
                v
            }
            _ => 0xFF,
        }
    }

    fn r16(&mut self, a: u32) -> u16 {
        let a = a & 0xFF_FFFE;
        match a {
            0xC0_0000..=0xC0_0003 => 0,
            0xC0_0004..=0xC0_0007 => self.vdp_status(),
            0xC0_0008..=0xC0_000F => self.hv(),
            0xA0_0000..=0xA0_FFFF => {
                let v = self.z80_area_read(a);
                (v as u16) << 8 | v as u16
            }
            0xA1_1100 => ((!(self.z80_busreq && !self.z80_reset)) as u16) << 8,
            _ => (self.r8(a) as u16) << 8 | self.r8(a + 1) as u16,
        }
    }

    fn w8(&mut self, a: u32, v: u8) {
        let a = a & 0xFF_FFFF;
        match a {
            0x20_0000..=0x20_FFFF if self.has_sram() && self.rom.len() <= 0x20_0000 => {
                self.sram[(a & 0xFFFF) as usize] = v;
            }
            0xA0_0000..=0xA0_FFFF => self.z80_area_write(a, v),
            0xA1_0000..=0xA1_001F => {
                if a & 0x1E == 0x02 {
                    self.th = v & 0x40 != 0;
                }
            }
            0xA1_1100 => self.z80_busreq = v & 1 != 0,
            0xA1_1200 => self.set_z80_reset(v & 1 == 0),
            0xC0_0000..=0xC0_0007 => {
                if a & 4 != 0 {
                    self.vdp_control((v as u16) << 8 | v as u16);
                }
            }
            0xC0_0011..=0xC0_0017 if a & 1 == 1 => self.psg_write(v),
            0xE0_0000..=0xFF_FFFF => {
                self.ram[(a & 0xFFFF) as usize] = v;
                self.traced(0xFF_0000 | (a & 0xFFFF), true, v);
            }
            _ => {}
        }
    }

    fn w16(&mut self, a: u32, v: u16) {
        let a = a & 0xFF_FFFE;
        match a {
            0xC0_0004..=0xC0_0007 => self.vdp_control(v),
            0xC0_0000..=0xC0_0003 => {}
            0xC0_0010..=0xC0_0017 => self.psg_write(v as u8),
            0xA0_0000..=0xA0_FFFF => self.z80_area_write(a, (v >> 8) as u8),
            0xA1_1100 => self.z80_busreq = v & 0x100 != 0,
            0xA1_1200 => self.set_z80_reset(v & 0x100 == 0),
            _ => {
                self.w8(a, (v >> 8) as u8);
                self.w8(a + 1, v as u8);
            }
        }
    }

    fn set_z80_reset(&mut self, on: bool) {
        self.z80_reset = on;
    }
}

impl m68k::Bus for Board {
    fn read8(&mut self, a: u32) -> u8 {
        self.r8(a)
    }
    fn read16(&mut self, a: u32) -> u16 {
        self.r16(a)
    }
    fn write8(&mut self, a: u32, v: u8) {
        self.w8(a, v)
    }
    fn write16(&mut self, a: u32, v: u16) {
        self.w16(a, v)
    }
}

impl z80::Bus for Board {
    fn read(&mut self, a: u16) -> u8 {
        match a {
            0x0000..=0x7FFF => self.z80_area_read(a as u32),
            _ => {
                let addr = self.bank << 15 | (a as u32 & 0x7FFF);
                if addr >= 0xA0_0000 && addr < 0xA1_0000 {
                    return 0xFF;
                }
                self.r8(addr)
            }
        }
    }
    fn write(&mut self, a: u16, v: u8) {
        match a {
            0x0000..=0x7FFF => self.z80_area_write(a as u32, v),
            _ => {
                let addr = self.bank << 15 | (a as u32 & 0x7FFF);
                if !(0xA0_0000..0xA1_0000).contains(&addr) {
                    self.w8(addr, v);
                }
            }
        }
    }
    fn input(&mut self, _port: u16) -> u8 {
        0xFF
    }
    fn output(&mut self, _port: u16, _v: u8) {}
}

/// A moment a CPU entered a routine: enough to call it again from there.
#[derive(Clone)]
pub struct Entry {
    main: Option<m68k::Cpu>,
    sound: Option<z80::Cpu>,
    bank: u32,
    /// What the call put on the stack (return address, and the status for an interrupt).
    frame: Vec<u8>,
    /// 68000 cycle into the frame.
    cycle: u32,
}

/// A Mega Drive: both CPUs and the board.
#[derive(Clone)]
pub struct MegaDrive {
    main: m68k::Cpu,
    sound: z80::Cpu,
    board: Board,
    /// Which CPU ran the last instruction.
    last_z80: bool,
}

impl MegaDrive {
    fn z80_active(&self) -> bool {
        !self.board.z80_reset && !self.board.z80_busreq
    }

    fn step_main(&mut self) -> u32 {
        let level = self.board.irq_level();
        let mut c = 0;
        if level != 0 {
            let took = self.main.interrupt(&mut self.board, level);
            if took > 0 {
                if level == 6 {
                    self.board.vint = false;
                } else {
                    self.board.hint = false;
                }
                c += took;
            }
        }
        if let Some(t) = self.board.trace.as_mut() {
            t.clear();
        }
        self.board.z80_running = false;
        c += self.main.step(&mut self.board);
        let before = self.board.line();
        self.board.cycle += c;
        self.board.clock_timers(c);
        for l in before + 1..=self.board.line() {
            if l < LINES {
                self.board.new_line(l);
            }
        }
        c
    }

    fn step_sound(&mut self) -> u32 {
        if !self.z80_active() {
            self.board.zcycle += 4;
            return 4;
        }
        let mut c = 0;
        // The Z80's interrupt is held for the first line of the vertical blank.
        let zline = self.board.zcycle / Z80_LINE;
        if zline == VBLANK_LINE {
            c += self.sound.interrupt(&mut self.board, false);
        }
        if let Some(t) = self.board.trace.as_mut() {
            t.clear();
        }
        self.board.z80_running = true;
        c += self.sound.step(&mut self.board);
        self.board.z80_running = false;
        self.board.zcycle += c;
        c
    }

    fn release_z80_reset(&mut self, was_reset: bool) {
        if was_reset && !self.board.z80_reset {
            self.sound = z80::Cpu::reset();
            self.sound.sp = 0x1FFF;
        }
    }

    /// Runs the routine of `entry` alone, from its own registers, until it returns. False
    /// when it doesn't within `budget` 68000 cycles.
    fn call(&mut self, entry: &Entry, set: Option<(u8, u8)>, budget: u32) -> bool {
        self.board.bank = entry.bank;
        let start = self.board.cycle;
        if let Some(cpu) = &entry.main {
            let mut cpu = cpu.clone();
            if let Some((reg, v)) = set {
                cpu.d[reg as usize & 7] = (cpu.d[reg as usize & 7] & !0xFF) | v as u32;
            }
            let sp = cpu.a[7];
            for (i, b) in entry.frame.iter().enumerate() {
                self.board.w8(sp.wrapping_add(i as u32), *b);
            }
            cpu.calls = vec![(cpu.pc, sp.wrapping_add(entry.frame.len() as u32))];
            cpu.stopped = false;
            self.main = cpu;
            self.board.z80_running = false;
            while !self.main.calls.is_empty() {
                if self.board.cycle - start > budget || self.main.halted || self.main.stopped {
                    return false;
                }
                let c = self.main.step(&mut self.board);
                self.board.cycle += c;
                self.board.clock_timers(c);
            }
            return true;
        }
        let Some(cpu) = &entry.sound else {
            return false;
        };
        let mut cpu = cpu.clone();
        if let Some((reg, v)) = set {
            match reg {
                0 => cpu.a = v,
                1 => cpu.b = v,
                2 => cpu.c = v,
                3 => cpu.d = v,
                4 => cpu.e = v,
                5 => cpu.h = v,
                6 => cpu.l = v,
                _ => return false,
            }
        }
        let sp = cpu.sp;
        for (i, b) in entry.frame.iter().enumerate() {
            z80::Bus::write(&mut self.board, sp.wrapping_add(i as u16), *b);
        }
        cpu.calls = vec![(cpu.pc, sp.wrapping_add(entry.frame.len() as u16))];
        cpu.halted = false;
        self.sound = cpu;
        let zstart = self.board.zcycle;
        let zbudget = (budget as u64 * Z80_CLOCK as u64 / CLOCK as u64) as u32;
        self.board.z80_running = true;
        let mut ok = true;
        while !self.sound.calls.is_empty() {
            if self.board.zcycle - zstart > zbudget || self.sound.halted {
                ok = false;
                break;
            }
            let c = self.sound.step(&mut self.board);
            self.board.zcycle += c;
            self.board
                .clock_timers((c as u64 * CLOCK as u64 / Z80_CLOCK as u64) as u32);
        }
        self.board.z80_running = false;
        ok
    }
}

/// The YM2612 and the PSG.
pub struct Chip(Chips);

impl Machine for MegaDrive {
    type Entry = Entry;
    type Chip = Chip;
    const REGISTERS: u8 = 8;
    const CHANNELS: u16 = 2;

    fn boot(rom: &[u8]) -> Result<Self, String> {
        if rom.len() < 0x200 {
            return Err("not a Mega Drive ROM (too small)".into());
        }
        let mut rom = rom.to_vec();
        // Interleaved (.smd) dumps: a 512-byte header, then 16 KB blocks of odd/even bytes.
        if rom.len() % 0x4000 == 0x200 && rom[8] == 0xAA && rom[9] == 0xBB {
            let body = &rom[0x200..];
            let mut out = vec![0u8; body.len()];
            for (b, block) in body.chunks(0x4000).enumerate() {
                let half = block.len() / 2;
                for i in 0..half {
                    out[b * 0x4000 + i * 2 + 1] = block[i];
                    out[b * 0x4000 + i * 2] = block[half + i];
                }
            }
            rom = out;
        }
        if rom.len() % 2 == 1 {
            rom.push(0);
        }
        let region = &rom[0x1F0..0x1F3];
        let japan_only = region.iter().any(|&c| c == b'J')
            && !region
                .iter()
                .any(|&c| matches!(c, b'U' | b'E' | b'4' | b'8' | b'F' | b'A'))
            || (region[0] == b'1' && region[1] == b' ');
        let sram = rom.len() >= 0x1B2 && &rom[0x1B0..0x1B2] == b"RA";
        let mut board = Board {
            rom: Arc::new(rom),
            ram: vec![0; 0x10000],
            zram: vec![0; 0x2000],
            sram: if sram { vec![0; 0x10000] } else { Vec::new() },
            bank: 0,
            z80_busreq: false,
            z80_reset: true,
            vdp: [0; 32],
            vdp_pending: None,
            vint: false,
            hint: false,
            hint_counter: 0,
            cycle: 0,
            zcycle: 0,
            th: true,
            buttons: false,
            version: if japan_only { 0x20 } else { 0xA0 },
            ym_addr: [0; 2],
            ym: [[0; 256]; 2],
            timer_a: 0,
            timer_b: 0,
            ym_status: 0,
            psg: [0, 0x0F, 0, 0x0F, 0, 0x0F, 0, 0x0F],
            psg_latch: 0,
            sound: Vec::new(),
            z80_running: false,
            stack68: (0xFF_FFFF, 0),
            stackz: (0xFFFF, 0),
            trace: None,
        };
        let main = m68k::Cpu::reset(&mut board);
        let mut sound = z80::Cpu::reset();
        sound.sp = 0x1FFF;
        Ok(Self {
            main,
            sound,
            board,
            last_z80: false,
        })
    }

    fn run_frame(
        &mut self,
        mut watch: Option<&mut dyn FnMut(&Step)>,
        stop_after: Option<u64>,
    ) -> bool {
        if self.board.cycle < LINE {
            self.board.new_line(0);
        }
        let mut n = 0u64;
        let mut calls: Vec<(u32, u32)> = Vec::new();
        let mut sound: Vec<Write> = Vec::new();
        let mut ram: Vec<(u32, bool, u8)> = Vec::new();
        while self.board.cycle < FRAME {
            let line_end = (self.board.line() + 1) * LINE;
            let zline_end = (self.board.line() + 1) * Z80_LINE;
            // The 68000 to the end of this line, then the Z80.
            let mut turn_z80 = false;
            loop {
                let (before, sound_at);
                if !turn_z80 {
                    if self.board.cycle >= line_end {
                        turn_z80 = true;
                        continue;
                    }
                    before = self.main.calls.len();
                    sound_at = self.board.sound.len();
                    let was_reset = self.board.z80_reset;
                    self.step_main();
                    self.release_z80_reset(was_reset);
                    self.last_z80 = false;
                    if self.main.calls.len() > before {
                        let sp = self.main.a[7];
                        let (lo, hi) = self.board.stack68;
                        self.board.stack68 = (lo.min(sp), hi.max(sp + 3));
                    }
                } else {
                    if self.board.zcycle >= zline_end {
                        break;
                    }
                    let active = self.z80_active();
                    before = self.sound.calls.len();
                    sound_at = self.board.sound.len();
                    self.step_sound();
                    if !active {
                        continue;
                    }
                    self.last_z80 = true;
                    if self.sound.calls.len() > before {
                        let sp = self.sound.sp;
                        let (lo, hi) = self.board.stackz;
                        self.board.stackz = (lo.min(sp), hi.max(sp.wrapping_add(1)));
                    }
                }
                n += 1;
                if let Some(w) = watch.as_deref_mut() {
                    calls.clear();
                    if turn_z80 {
                        calls.extend(
                            self.sound
                                .calls
                                .iter()
                                .map(|&(r, s)| (Z80 | r as u32, Z80 | s as u32)),
                        );
                    } else {
                        calls.extend(self.main.calls.iter().copied());
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
        }
        self.board.cycle -= FRAME;
        self.board.zcycle = self.board.zcycle.saturating_sub(Z80_LINE * LINES);
        false
    }

    fn take_sound(&mut self) -> Vec<Write> {
        std::mem::take(&mut self.board.sound)
    }

    fn set_trace(&mut self, on: bool) {
        self.board.trace = on.then(Vec::new);
    }

    fn press_start(&mut self, held: bool) {
        self.board.buttons = held;
    }

    fn entry(&self) -> Entry {
        if self.last_z80 {
            let (_, ret_sp) = *self.sound.calls.last().expect("inside a routine");
            let sp = self.sound.sp;
            let len = ret_sp.wrapping_sub(sp).min(8) as usize;
            let frame = (0..len)
                .map(|i| self.board.zram[(sp.wrapping_add(i as u16) & 0x1FFF) as usize])
                .collect();
            let mut cpu = self.sound.clone();
            cpu.calls.clear();
            return Entry {
                main: None,
                sound: Some(cpu),
                bank: self.board.bank,
                frame,
                cycle: self.board.cycle,
            };
        }
        let (_, ret_sp) = *self.main.calls.last().expect("inside a routine");
        let sp = self.main.a[7];
        let len = ret_sp.wrapping_sub(sp).min(16) as usize;
        let frame = (0..len)
            .map(|i| self.board.ram[(sp.wrapping_add(i as u32) & 0xFFFF) as usize])
            .collect();
        let mut cpu = self.main.clone();
        cpu.calls.clear();
        Entry {
            main: Some(cpu),
            sound: None,
            bank: self.board.bank,
            frame,
            cycle: self.board.cycle,
        }
    }

    fn play_frame(&mut self, play: &Entry) -> Option<Vec<Write>> {
        self.board.cycle = play.cycle;
        self.board.zcycle = (play.cycle as u64 * Z80_CLOCK as u64 / CLOCK as u64) as u32;
        let ok = self.call(play, None, FRAME * 4);
        if play.main.is_some() && self.z80_active() {
            // The Z80 goes on with its own work (sampled drums), as it does in the game.
            self.board.zcycle = 0;
            while self.board.zcycle < Z80_LINE * LINES {
                self.step_sound();
            }
        } else {
            let rest = FRAME.saturating_sub(self.board.cycle);
            self.board.clock_timers(rest);
        }
        self.board.cycle = 0;
        self.board.zcycle = 0;
        let writes = self.take_sound();
        ok.then_some(writes)
    }

    fn call_with(&mut self, entry: &Entry, reg: u8, v: u8) -> bool {
        let cycle = self.board.cycle;
        let zcycle = self.board.zcycle;
        let bank = self.board.bank;
        let (main, sound) = (self.main.clone(), self.sound.clone());
        let ok = self.call(entry, Some((reg, v)), 800_000);
        self.board.cycle = cycle;
        self.board.zcycle = zcycle;
        self.board.bank = bank;
        self.main = main;
        self.sound = sound;
        for w in self.board.sound.iter_mut() {
            w.at = cycle;
        }
        ok
    }

    fn poke(&mut self, addr: u32, v: u8) {
        match addr {
            0xFF_0000..=0xFF_FFFF => self.board.ram[(addr & 0xFFFF) as usize] = v,
            0xA0_0000..=0xA0_1FFF => self.board.zram[(addr & 0x1FFF) as usize] = v,
            _ => {}
        }
    }

    fn incidental(w: &Write) -> bool {
        // Sampled drums (the DAC's data) and the timers say nothing about the music routine.
        matches!(w.port, 0x02A | 0x024..=0x027)
    }

    fn is_stack(&self, addr: u32) -> bool {
        let (lo, hi) = self.board.stack68;
        if addr >= 0xFF_0000 {
            let a = 0xFF_0000 | (lo & 0xFFFF);
            return lo <= hi
                && (a.saturating_sub(0x40)..=(0xFF_0000 | (hi & 0xFFFF))).contains(&addr);
        }
        let (zlo, zhi) = self.board.stackz;
        if (0xA0_0000..0xA0_2000).contains(&addr) && zlo <= zhi {
            let a = (addr & 0x1FFF) as u16;
            return (zlo.saturating_sub(0x20)..=zhi).contains(&a);
        }
        false
    }

    fn restore_writes(&self) -> Vec<Write> {
        let b = &self.board;
        let w = |port, v: u8| Write {
            at: 0,
            port,
            value: v as u16,
        };
        let mut out = Vec::new();
        for half in 0..2u32 {
            for reg in 0x30..=0xB6usize {
                if reg & 3 == 3 {
                    continue;
                }
                out.push(w(half << 8 | reg as u32, b.ym[half as usize][reg]));
            }
        }
        out.push(w(0x022, b.ym[0][0x22]));
        out.push(w(0x027, b.ym[0][0x27] & 0xC0));
        out.push(w(0x02B, b.ym[0][0x2B]));
        // Every channel's keys off: the song starts its own notes.
        for ch in [0u8, 1, 2, 4, 5, 6] {
            out.push(w(0x028, ch));
        }
        for r in 0..8u8 {
            let v = b.psg[r as usize];
            if r & 1 == 1 {
                out.push(w(PSG, 0x80 | r << 4 | 0x0F));
            } else if r < 6 {
                out.push(w(PSG, 0x80 | r << 4 | (v & 0x0F) as u8));
                out.push(w(PSG, ((v >> 4) & 0x3F) as u8));
            } else {
                out.push(w(PSG, 0x80 | r << 4 | (v & 0x0F) as u8));
            }
        }
        out
    }

    fn new_chip(&self, rate: u32) -> Result<Chip, String> {
        Ok(Chip(Chips::new(
            rate,
            CLOCK as f64,
            &[(Kind::Ym2612, CLOCK), (Kind::SegaPsg, Z80_CLOCK)],
        )?))
    }

    fn feed(&self, chip: &mut Chip, writes: &[Write], out: &mut Vec<i16>) {
        for w in writes {
            let at = w.at.min(FRAME - 1);
            if w.port == PSG {
                chip.0.write(at, 1, 0, w.value as u8);
            } else {
                let half = (w.port >> 8) as u8 & 1;
                chip.0.write(at, 0, half * 2, w.port as u8);
                chip.0.write(at, 0, half * 2 + 1, w.value as u8);
            }
        }
        chip.0.end_frame(FRAME, out);
    }

    fn frame_rate(&self) -> f64 {
        RATE
    }
}

#[cfg(test)]
mod tests {
    use super::super::music_search::{measure, search, Trigger};
    use super::*;

    /// A tiny game: the vertical interrupt calls a sound routine on the 68000 that plays song
    /// N (N written to $FF0100 by the main program, or 1 at boot) on the PSG at a pitch per
    /// song, changing note every 8 frames through 16 notes, then round again.
    fn tiny_game() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        let put32 = |rom: &mut Vec<u8>, at: usize, v: u32| {
            rom[at..at + 4].copy_from_slice(&v.to_be_bytes());
        };
        put32(&mut rom, 0, 0x00FF_FE00); // stack
        put32(&mut rom, 4, 0x200); // reset
        put32(&mut rom, 0x78, 0x300); // level 6: vertical interrupt
        rom[0x1F0..0x1F3].copy_from_slice(b"JUE");
        // $200: MOVE.B #1,$FF0100; MOVE.W #$8164,$C00004 (VInt on); MOVE.W #$2000,SR;
        // loop: STOP #$2000; BRA loop
        let main: [u16; 13] = [
            0x13FC, 0x0001, 0x00FF, 0x0100, 0x33FC, 0x8164, 0x00C0, 0x0004, 0x46FC, 0x2000, 0x4E72,
            0x2000, 0x60FA,
        ];
        // $300: JSR $400; RTE
        let irq: [u16; 4] = [0x4EB9, 0x0000, 0x0400, 0x4E73];
        // $400:
        //   MOVE.B $FF0100,D0; BEQ.S tick; MOVE.B D0,$FF0101; CLR.B $FF0100; CLR.B $FF0102
        // tick: TST.B $FF0101; BEQ.S done; ADDQ.B #1,$FF0102; MOVE.B $FF0102,D0;
        //   AND.B #7,D0; BNE.S done
        //   MOVE.B #$90,$C00011
        //   MOVE.B $FF0102,D0; LSR.B #3,D0; AND.B #$F,D0; ADD.B $FF0101,D0; LSL.B #3,D0
        //   MOVE.B D0,D1; AND.B #$F,D1; OR.B #$80,D1; MOVE.B D1,$C00011
        //   LSR.B #4,D0; OR.B #$10,D0; MOVE.B D0,$C00011
        // done: RTS
        let sound: &[u16] = &[
            0x1039, 0x00FF, 0x0100, // MOVE.B $FF0100,D0
            0x6712, // BEQ.S tick
            0x13C0, 0x00FF, 0x0101, // MOVE.B D0,$FF0101
            0x4239, 0x00FF, 0x0100, // CLR.B $FF0100
            0x4239, 0x00FF, 0x0102, // CLR.B $FF0102
            0x4A39, 0x00FF, 0x0101, // tick: TST.B $FF0101
            0x674A, // BEQ.S done
            0x5239, 0x00FF, 0x0102, // ADDQ.B #1,$FF0102
            0x1039, 0x00FF, 0x0102, // MOVE.B $FF0102,D0
            0x0200, 0x0007, // AND.B #7,D0
            0x6638, // BNE.S done
            0x13FC, 0x0090, 0x00C0, 0x0011, // MOVE.B #$90,$C00011
            0x1039, 0x00FF, 0x0102, // MOVE.B $FF0102,D0
            0xE608, // LSR.B #3,D0
            0x0200, 0x000F, // AND.B #$F,D0
            0xD039, 0x00FF, 0x0101, // ADD.B $FF0101,D0
            0xE708, // LSL.B #3,D0
            0x1200, // MOVE.B D0,D1
            0x0201, 0x000F, // AND.B #$F,D1
            0x0001, 0x0080, // OR.B #$80,D1
            0x13C1, 0x00C0, 0x0011, // MOVE.B D1,$C00011
            0xE808, // LSR.B #4,D0
            0x0000, 0x0010, // OR.B #$10,D0
        ];
        let tail: [u16; 4] = [0x13C0, 0x00C0, 0x0011, 0x4E75]; // MOVE.B D0,$C00011; RTS
        let mut at = 0x200;
        for w in main {
            rom[at..at + 2].copy_from_slice(&w.to_be_bytes());
            at += 2;
        }
        let mut at = 0x300;
        for w in irq {
            rom[at..at + 2].copy_from_slice(&w.to_be_bytes());
            at += 2;
        }
        let mut at = 0x400;
        for w in sound.iter().chain(tail.iter()) {
            rom[at..at + 2].copy_from_slice(&w.to_be_bytes());
            at += 2;
        }
        rom
    }

    #[test]
    fn the_tiny_game_has_its_songs() {
        if !super::super::vgm_chips::available() {
            return;
        }
        let rom = tiny_game();
        let found = search::<MegaDrive>(&rom).unwrap();
        assert_eq!(found.why, None);
        assert_eq!(found.recipe.play, 0x400);
        assert_eq!(found.recipe.trigger, Trigger::Byte(0xFF_0100));
        assert!(
            (30..=34).contains(&found.songs.len()),
            "{} songs",
            found.songs.len()
        );
        let t = measure::<MegaDrive>(&rom, &found.recipe, 3).unwrap();
        assert_eq!(t.looping.map(|l| l.1), Some(128), "{t:?}");
    }
}
