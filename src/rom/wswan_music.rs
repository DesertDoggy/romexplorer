//! The WonderSwan and WonderSwan Color as a [`Machine`] for the music search
//! ([`super::music_search`]): the V30MZ ([`super::v30mz`]), cartridge banks, internal RAM,
//! the interrupt controller, the scanline counter and its timers, the keypad, the DMA
//! units, and the sound unit's ports. Nothing is drawn. No boot ROM: the console starts the
//! cartridge in the state the boot ROM leaves it, as Mednafen does
//! (`external/beetle-wswan-libretro`, whose behaviour this follows). The sound unit goes
//! to libvgm's chip ([`super::vgm_chips`]), which reads its waveforms from the console's RAM:
//! RAM writes inside the waveform table are sent to it too.

use std::sync::Arc;

use super::music_search::{Machine, Step, Write};
use super::v30mz::{physical, Bus, Cpu, AX, BX, CX, DX, SP, SS};
use super::vgm_chips::{Chips, Kind};

/// CPU cycles a second, per scanline, and scanlines per frame (75.47 frames a second).
const CLOCK: u32 = 3_072_000;
const LINE: u32 = 256;
const LINES: u32 = 159;
pub(super) const FRAME: u32 = LINE * LINES;
pub const RATE: f64 = CLOCK as f64 / FRAME as f64;
/// The scanline the vertical blank starts on.
const VBLANK_LINE: u32 = 144;

/// Interrupt causes, by their bit in the controller's registers.
const INT_LINE: u8 = 4;
const INT_VBLANK_TIMER: u8 = 5;
const INT_VBLANK: u8 = 6;
const INT_HBLANK_TIMER: u8 = 7;

/// A sound write's port for a write to the sound unit's RAM (its waveforms): this bit, plus
/// the address.
const RAM_WRITE: u32 = 0x1_0000;

/// The I/O ports as the boot ROM leaves them (Mednafen's `start.inc`), for the ports this
/// models: the line compare, the timers, the keypad, the sweep step, the banks.
const START_PORTS: [(u8, u8); 9] = [
    (0x03, 0xBB),
    (0xA6, 0x4F),
    (0xA7, 0xFF),
    (0xB5, 0x40),
    (0x8D, 0x1F),
    (0xC0, 0x2F),
    (0xC1, 0x3F),
    (0xC2, 0xFF),
    (0xC3, 0xFF),
];

/// Everything around the CPU. Plain data: cloned to run again from any moment.
#[derive(Clone)]
struct Board {
    /// The cartridge, padded at the front to a power of two (the last bank holds the start).
    rom: Arc<Vec<u8>>,
    color: bool,
    ram: Vec<u8>,
    sram: Vec<u8>,
    banks: [u8; 4],
    /// The interrupt controller: vector base, enabled causes, latched causes.
    int_base: u8,
    int_enable: u8,
    int_status: u8,
    line_compare: u8,
    /// Timers: control (A2), periods and counters of the scanline and vertical-blank timers.
    timer_ctl: u8,
    hb_period: u16,
    hb_count: u16,
    vb_period: u16,
    vb_count: u16,
    /// The keypad's selected rows; Start held.
    keys_sel: u8,
    start: bool,
    /// The general DMA (WonderSwan Color): source, destination, length, control.
    dma: (u32, u16, u16),
    /// The sound DMA: source, length, both as last set (for repeats), control, wait.
    sdma: (u32, u32, u32, u32),
    sdma_ctl: u8,
    sdma_wait: u8,
    /// Other ports that read back what was written.
    ports: [u8; 256],
    eeprom_cmd: [u8; 2],
    /// CPU cycles into the frame.
    cycle: u32,
    sound: Vec<Write>,
    /// The lowest and highest stack addresses seen.
    stack: (u32, u32),
    /// RAM accesses, when traced: (address, written, value).
    trace: Option<Vec<(u32, bool, u8)>>,
    /// Port writes, when logged (for `ws_map`).
    outs: Option<Vec<(u8, u8)>>,
}

impl Board {
    fn line(&self) -> u32 {
        self.cycle / LINE
    }

    fn rom_bank(&self, bank: u32, off: u32) -> u8 {
        let n = (self.rom.len() >> 16) as u32;
        self.rom[(((bank & (n - 1)) << 16) | off) as usize]
    }

    /// The waveform table: 64 bytes at port 8F's address.
    fn wave_base(&self) -> u16 {
        (self.ports[0x8F] as u16) << 6
    }

    fn interrupt(&mut self, cause: u8) {
        self.int_status |= (1 << cause) & self.int_enable;
    }

    /// The interrupt to take: its vector number.
    fn pending(&self) -> Option<u8> {
        let on = self.int_status & self.int_enable;
        (on != 0).then(|| self.int_base.wrapping_add(on.trailing_zeros() as u8))
    }

    fn sound(&mut self, port: u32, v: u8, at: u32) {
        self.sound.push(Write {
            at,
            port,
            value: v as u16,
        });
    }

    /// One tick of the sound DMA (24,000 a second): a byte to the voice channel when due.
    fn sound_dma(&mut self, at: u32) {
        if self.sdma_ctl & 0x80 == 0 {
            return;
        }
        if self.sdma_wait > 0 {
            self.sdma_wait -= 1;
            return;
        }
        let (src, len, src0, len0) = self.sdma;
        let b = self.read(src);
        let port = if self.sdma_ctl & 0x10 != 0 { 0x95 } else { 0x89 };
        self.ports[port] = b;
        self.sound(port as u32, b, at);
        let src = if self.sdma_ctl & 0x40 != 0 {
            src.wrapping_sub(1)
        } else {
            src + 1
        } & 0xFFFFF;
        let len = len.wrapping_sub(1) & 0xFFFFF;
        self.sdma = (src, len, src0, len0);
        if len == 0 {
            if self.sdma_ctl & 0x08 != 0 {
                self.sdma = (src0, len0, src0, len0);
            } else {
                self.sdma_ctl &= !0x80;
            }
        }
        self.sdma_wait = [5, 3, 1, 0][(self.sdma_ctl & 3) as usize];
    }

    /// The start of scanline `l`: line match, vertical blank, the timers.
    fn new_line(&mut self, l: u32) {
        let at = l * LINE;
        if l == self.line_compare as u32 {
            self.interrupt(INT_LINE);
        }
        if l == VBLANK_LINE {
            self.interrupt(INT_VBLANK);
            if self.vb_count != 0 && self.timer_ctl & 0x04 != 0 {
                self.vb_count -= 1;
                if self.vb_count == 0 {
                    if self.timer_ctl & 0x08 != 0 {
                        self.vb_count = self.vb_period;
                    }
                    self.interrupt(INT_VBLANK_TIMER);
                }
            }
        }
        if self.hb_count != 0 && self.timer_ctl & 0x01 != 0 {
            self.hb_count -= 1;
            if self.hb_count == 0 {
                if self.timer_ctl & 0x02 != 0 {
                    self.hb_count = self.hb_period;
                }
                self.interrupt(INT_HBLANK_TIMER);
            }
        }
        self.sound_dma(at);
        self.sound_dma(at + LINE / 2);
    }

    fn advance(&mut self, cycles: u32) {
        let before = self.line();
        self.cycle += cycles;
        let after = self.line();
        for l in before + 1..=after {
            if l < LINES {
                self.new_line(l);
            }
        }
    }

    fn traced(&mut self, addr: u32, write: bool, v: u8) {
        if let Some(t) = self.trace.as_mut() {
            t.push((addr, write, v));
        }
    }

    fn general_dma(&mut self) {
        let (mut src, mut dst, mut len) = self.dma;
        let down = self.ports[0x48] & 0x40 != 0;
        while len > 0 {
            for i in 0..2u16 {
                let v = self.read(src + i as u32);
                self.write(dst.wrapping_add(i) as u32, v);
            }
            if down {
                src = src.wrapping_sub(2);
                dst = dst.wrapping_sub(2);
            } else {
                src += 2;
                dst = dst.wrapping_add(2);
            }
            src &= 0xFFFFE;
            len -= 2;
        }
        self.dma = (src, dst, len);
        self.ports[0x48] &= !0x80;
    }
}

impl Bus for Board {
    fn read(&mut self, addr: u32) -> u8 {
        let off = addr & 0xFFFF;
        match addr >> 16 {
            0 => {
                let v = self.ram[off as usize];
                self.traced(off, false, v);
                v
            }
            1 if !self.sram.is_empty() => {
                let at = ((self.banks[1] as usize) << 16 | off as usize) & (self.sram.len() - 1);
                self.sram[at]
            }
            1 => 0,
            b @ (2 | 3) => self.rom_bank(self.banks[b as usize] as u32, off),
            b => self.rom_bank(((self.banks[0] as u32 & 0x0F) << 4) | b, off),
        }
    }

    fn write(&mut self, addr: u32, v: u8) {
        let off = addr & 0xFFFF;
        match addr >> 16 {
            0 => {
                self.ram[off as usize] = v;
                self.traced(off, true, v);
                let base = self.wave_base() as u32;
                if (base..base + 64).contains(&off) {
                    self.sound(RAM_WRITE | off, v, self.cycle);
                }
            }
            1 if !self.sram.is_empty() => {
                let at = ((self.banks[1] as usize) << 16 | off as usize) & (self.sram.len() - 1);
                self.sram[at] = v;
            }
            _ => {}
        }
    }

    fn input(&mut self, port: u16) -> u8 {
        let p = (port & 0xFF) as usize;
        match p {
            0x02 => self.line() as u8,
            0x03 => self.line_compare,
            0x40 => self.dma.0 as u8,
            0x41 => (self.dma.0 >> 8) as u8,
            0x42 => (self.dma.0 >> 16) as u8,
            0x44 => self.dma.1 as u8,
            0x45 => (self.dma.1 >> 8) as u8,
            0x46 => self.dma.2 as u8,
            0x47 => (self.dma.2 >> 8) as u8,
            0x4A => self.sdma.0 as u8,
            0x4B => (self.sdma.0 >> 8) as u8,
            0x4C => (self.sdma.0 >> 16) as u8,
            0x4E => self.sdma.1 as u8,
            0x4F => (self.sdma.1 >> 8) as u8,
            0x50 => (self.sdma.1 >> 16) as u8,
            0x52 => self.sdma_ctl,
            0xA0 => {
                if self.color {
                    0x87
                } else {
                    0x86
                }
            }
            0xA2 => self.timer_ctl,
            0xA4 => self.hb_period as u8,
            0xA5 => (self.hb_period >> 8) as u8,
            0xA6 => self.vb_period as u8,
            0xA7 => (self.vb_period >> 8) as u8,
            0xA8 => self.hb_count as u8,
            0xA9 => (self.hb_count >> 8) as u8,
            0xAA => self.vb_count as u8,
            0xAB => (self.vb_count >> 8) as u8,
            0xB0 => self.int_base,
            0xB2 => self.int_enable,
            0xB4 | 0xB6 => {
                let on = self.int_status & self.int_enable;
                if on == 0 { 0 } else { 1 << on.trailing_zeros() }
            }
            // The serial port: nothing waiting, ready to send.
            0xB1 => 0,
            0xB3 => (self.ports[0xB3] & 0xF0) | if self.ports[0xB3] & 0x80 != 0 { 4 } else { 0 },
            0xB5 => {
                let sel = self.keys_sel;
                let pressed = if sel & 0x4 != 0 && self.start { 0x02 } else { 0 };
                sel << 4 | pressed
            }
            0xC0 => self.banks[0] | 0x20,
            0xC1..=0xC3 => self.banks[p - 0xC0],
            // EEPROMs: every command done at once; blank contents.
            0xBE => self.eeprom_cmd[0] | 3,
            0xC8 => self.eeprom_cmd[1] | 3,
            0xBA..=0xBD | 0xC4..=0xC7 => 0,
            // The clock chip: ready.
            0xCA => self.ports[0xCA] | 0x80,
            0xCB => 0x80,
            _ if p > 0xC8 => 0xD0,
            _ => self.ports[p],
        }
    }

    fn output(&mut self, port: u16, v: u8) {
        let p = (port & 0xFF) as usize;
        if let Some(o) = self.outs.as_mut() {
            o.push((p as u8, v));
        }
        self.ports[p] = v;
        match p {
            0x03 => self.line_compare = v,
            0x40 => self.dma.0 = (self.dma.0 & 0xFFF00) | (v & 0xFE) as u32,
            0x41 => self.dma.0 = (self.dma.0 & 0xF00FF) | (v as u32) << 8,
            0x42 => self.dma.0 = (self.dma.0 & 0x0FFFF) | ((v & 0x0F) as u32) << 16,
            0x44 => self.dma.1 = (self.dma.1 & 0xFF00) | (v & 0xFE) as u16,
            0x45 => self.dma.1 = (self.dma.1 & 0x00FF) | (v as u16) << 8,
            0x46 => self.dma.2 = (self.dma.2 & 0xFF00) | (v & 0xFE) as u16,
            0x47 => self.dma.2 = (self.dma.2 & 0x00FF) | (v as u16) << 8,
            0x48 => {
                if v & 0x80 != 0 {
                    self.general_dma();
                }
            }
            0x4A..=0x4C | 0x4E..=0x50 => {
                let (shift, mask) = match p {
                    0x4A | 0x4E => (0, 0xFF),
                    0x4B | 0x4F => (8, 0xFF),
                    _ => (16, 0x0F),
                };
                let field = ((v as u32) & mask) << shift;
                let keep = !((0xFF << shift) as u32);
                if p < 0x4E {
                    self.sdma.0 = (self.sdma.0 & keep) | field;
                    self.sdma.2 = self.sdma.0;
                } else {
                    self.sdma.1 = (self.sdma.1 & keep) | field;
                    self.sdma.3 = self.sdma.1;
                }
            }
            0x52 => self.sdma_ctl = v & !0x20,
            0x80..=0x9F => {
                self.sound(p as u32, v, self.cycle);
                if p == 0x8F {
                    // A new waveform table: the sound unit reads it from RAM.
                    let base = self.wave_base();
                    for i in 0..64u16 {
                        let a = base + i;
                        let b = self.ram[a as usize];
                        self.sound(RAM_WRITE | a as u32, b, self.cycle);
                    }
                }
            }
            0xA2 => self.timer_ctl = v,
            0xA4 => self.hb_period = (self.hb_period & 0xFF00) | v as u16,
            0xA5 => {
                self.hb_period = (self.hb_period & 0x00FF) | (v as u16) << 8;
                self.hb_count = self.hb_period;
            }
            0xA6 => self.vb_period = (self.vb_period & 0xFF00) | v as u16,
            0xA7 => {
                self.vb_period = (self.vb_period & 0x00FF) | (v as u16) << 8;
                self.vb_count = self.vb_period;
            }
            0xB0 => self.int_base = v,
            0xB2 => {
                self.int_enable = v;
                self.int_status &= v;
            }
            0xB6 => self.int_status &= !v,
            0xB5 => self.keys_sel = v >> 4,
            0xBE => self.eeprom_cmd[0] = v & 0xF0,
            0xC8 => self.eeprom_cmd[1] = v & 0xF0,
            0xC0 => self.banks[0] = v & 0x0F,
            0xC1..=0xC3 => self.banks[p - 0xC0] = v,
            _ => {}
        }
    }
}

/// A moment the CPU entered a routine: enough to call it again from there.
#[derive(Clone)]
pub struct Entry {
    cpu: Cpu,
    /// What the call (or interrupt) pushed, and the stack pointer before it did.
    pushed: Vec<u8>,
    sp_before: u16,
    /// CPU cycles into the frame.
    cycle: u32,
}

/// A WonderSwan: CPU and board.
#[derive(Clone)]
pub struct WonderSwan {
    cpu: Cpu,
    board: Board,
}

impl WonderSwan {
    /// A console with `image` (a power of two in size, the start in its last bank) in the
    /// cartridge slot, in the state the boot ROM leaves.
    pub(super) fn with_image(image: Vec<u8>, color: bool, sram: usize) -> Self {
        let mut board = Board {
            rom: Arc::new(image),
            color,
            ram: vec![0; 0x10000],
            sram: vec![0; sram],
            banks: [0; 4],
            int_base: 0,
            int_enable: 0,
            int_status: 0,
            line_compare: 0,
            timer_ctl: 0,
            hb_period: 0,
            hb_count: 0,
            vb_period: 0,
            vb_count: 0,
            keys_sel: 0,
            start: false,
            dma: (0, 0, 0),
            sdma: (0, 0, 0, 0),
            sdma_ctl: 0,
            sdma_wait: 0,
            ports: [0; 256],
            eeprom_cmd: [0; 2],
            cycle: 0,
            sound: Vec::new(),
            stack: (u32::MAX, 0),
            trace: None,
            outs: None,
        };
        for (p, v) in START_PORTS {
            board.output(p as u16, v);
        }
        board.sound.clear();
        let mut cpu = Cpu::reset();
        cpu.seg[SS] = 0;
        cpu.r[SP] = 0x2000;
        Self { cpu, board }
    }

    // For `ws_map`, which steps the console itself.

    /// The cartridge byte `addr` reads, as the banks stand now; None for RAM and save RAM.
    pub(super) fn rom_offset(&self, addr: u32) -> Option<usize> {
        let n = (self.board.rom.len() >> 16) as u32;
        let bank = match addr >> 16 {
            0 | 1 => return None,
            b @ (2 | 3) => self.board.banks[b as usize] as u32,
            b => ((self.board.banks[0] as u32 & 0x0F) << 4) | b,
        };
        Some((((bank & (n - 1)) << 16) | (addr & 0xFFFF)) as usize)
    }

    pub(super) fn cpu(&self) -> &Cpu {
        &self.cpu
    }

    pub(super) fn cpu_mut(&mut self) -> &mut Cpu {
        &mut self.cpu
    }

    /// The start of a frame's work: its first scanline.
    pub(super) fn begin_frame(&mut self) {
        if self.board.cycle < LINE {
            self.board.new_line(0);
        }
    }

    /// Whether the frame is over (and the next begun counting).
    pub(super) fn frame_over(&mut self) -> bool {
        let over = self.board.cycle >= FRAME;
        if over {
            self.board.cycle -= FRAME;
        }
        over
    }

    /// Whether an interrupt will be taken now.
    pub(super) fn interrupt_due(&self) -> bool {
        self.board.pending().is_some() && self.cpu.interrupts_on()
    }

    /// Takes an interrupt that is due (or wakes the CPU for one it masks).
    pub(super) fn dispatch(&mut self) -> bool {
        let Some(vector) = self.board.pending() else { return false };
        if self.cpu.interrupts_on() {
            let t = self.cpu.interrupt(&mut self.board, vector);
            self.board.advance(t);
            true
        } else {
            self.cpu.halted = false;
            false
        }
    }

    /// One instruction, or asleep to the next scanline.
    pub(super) fn exec(&mut self) -> u32 {
        let t = if self.cpu.halted { LINE - self.board.cycle % LINE } else { self.cpu.step(&mut self.board) };
        self.board.advance(t);
        t
    }

    pub(super) fn advance(&mut self, cycles: u32) {
        self.board.advance(cycles);
    }

    /// Logs RAM writes and port writes from now on (or stops).
    pub(super) fn log(&mut self, on: bool) {
        self.board.trace = on.then(Vec::new);
        self.board.outs = on.then(Vec::new);
    }

    /// The RAM writes and port writes logged since the last call.
    pub(super) fn take_log(&mut self) -> (Vec<(u16, u8)>, Vec<(u8, u8)>) {
        let ram = self.board.trace.as_mut().map(|t| t.drain(..).filter(|a| a.1).map(|a| (a.0 as u16, a.2)).collect()).unwrap_or_default();
        let outs = self.board.outs.as_mut().map(std::mem::take).unwrap_or_default();
        (ram, outs)
    }

    pub(super) fn out(&mut self, port: u8, v: u8) {
        self.board.output(port as u16, v);
    }

    pub(super) fn ram_write(&mut self, addr: u16, v: u8) {
        self.board.write(addr as u32, v);
    }

    pub(super) fn sound_writes(&mut self) -> Vec<Write> {
        std::mem::take(&mut self.board.sound)
    }

    fn step_once(&mut self) -> u32 {
        let mut t = 0;
        if let Some(vector) = self.board.pending() {
            if self.cpu.interrupts_on() {
                t += self.cpu.interrupt(&mut self.board, vector);
            } else {
                self.cpu.halted = false;
            }
        }
        if let Some(tr) = self.board.trace.as_mut() {
            tr.clear();
        }
        t += if self.cpu.halted {
            // Asleep until an interrupt: on to the next scanline.
            LINE - self.board.cycle % LINE
        } else {
            self.cpu.step(&mut self.board)
        };
        self.board.advance(t);
        t
    }

    fn stack_addr(&self, sp: u16) -> u32 {
        physical(self.cpu.seg[SS], sp)
    }

    /// Runs the routine of `entry` once, from its own registers, until it returns. False
    /// when it doesn't within `budget` cycles.
    fn call(&mut self, entry: &Entry, set: Option<(u8, u8)>, budget: u32) -> bool {
        self.cpu = entry.cpu.clone();
        self.cpu.halted = false;
        if let Some((reg, v)) = set {
            let r = [AX, BX, CX, DX][reg as usize & 3];
            self.cpu.r[r] = v as u16;
        }
        let sp = self.cpu.r[SP];
        for (i, &b) in entry.pushed.iter().enumerate() {
            let at = self.stack_addr(sp.wrapping_add(i as u16));
            self.board.write(at, b);
        }
        let pc = self.cpu.pc();
        self.cpu.calls = vec![(pc, entry.sp_before)];
        let start = self.board.cycle;
        while !self.cpu.calls.is_empty() {
            if self.board.cycle - start > budget || self.cpu.halted {
                return false;
            }
            let t = self.cpu.step(&mut self.board);
            self.board.cycle += t;
        }
        true
    }
}

/// The sound unit.
pub struct Chip(Chips);

impl Machine for WonderSwan {
    type Entry = Entry;
    type Chip = Chip;
    const REGISTERS: u8 = 4;
    const CHANNELS: u16 = 2;

    fn boot(rom: &[u8]) -> Result<Self, String> {
        if rom.len() < 0x10000 {
            return Err("not a WonderSwan cartridge (too small)".into());
        }
        let real = rom.len().div_ceil(0x10000) * 0x10000;
        let size = real.next_power_of_two();
        let mut r = vec![0xFF; size];
        r[size - real..size - real + rom.len()].copy_from_slice(rom);
        // The footer: minimum system (1: Color) and save type.
        let color = r[size - 9] & 1 != 0;
        let sram = match r[size - 5] {
            0x01 | 0x02 => 0x8000,
            0x03 => 0x20000,
            0x04 => 0x40000,
            0x05 => 0x80000,
            _ => 0,
        };
        Ok(Self::with_image(r, color, sram))
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
            let before = self.cpu.calls.len();
            let sound_at = self.board.sound.len();
            self.step_once();
            n += 1;
            if self.cpu.calls.len() > before {
                let s = self.stack_addr(self.cpu.r[SP]);
                let (lo, hi) = self.board.stack;
                self.board.stack = (lo.min(s), hi.max(s + 6));
            }
            if let Some(w) = watch.as_deref_mut() {
                calls.clear();
                calls.extend(self.cpu.calls.iter().map(|&(r, s)| (r, s as u32)));
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
        let mut cpu = self.cpu.clone();
        let sp = self.cpu.r[SP];
        let sp_before = self.cpu.calls.last().map_or(sp, |c| c.1);
        cpu.calls.clear();
        let n = sp_before.wrapping_sub(sp).min(16);
        let pushed = (0..n)
            .map(|i| {
                let a = self.stack_addr(sp.wrapping_add(i));
                if a < 0x10000 { self.board.ram[a as usize] } else { 0 }
            })
            .collect();
        Entry {
            cpu,
            pushed,
            sp_before,
            cycle: self.board.cycle,
        }
    }

    fn play_frame(&mut self, play: &Entry) -> Option<Vec<Write>> {
        self.board.cycle = play.cycle;
        let ok = self.call(play, None, FRAME * 4);
        self.board.cycle = 0;
        let writes = self.take_sound();
        ok.then_some(writes)
    }

    fn call_with(&mut self, entry: &Entry, reg: u8, v: u8) -> bool {
        let cycle = self.board.cycle;
        let cpu = self.cpu.clone();
        let ok = self.call(entry, Some((reg, v)), 8_000_000);
        self.board.cycle = cycle;
        self.cpu = cpu;
        for w in self.board.sound.iter_mut() {
            w.at = cycle;
        }
        ok
    }

    fn poke(&mut self, addr: u32, v: u8) {
        if addr < 0x10000 {
            self.board.ram[addr as usize] = v;
        }
    }

    fn incidental(_w: &Write) -> bool {
        false
    }

    fn is_stack(&self, addr: u32) -> bool {
        let (lo, hi) = self.board.stack;
        lo != u32::MAX && (lo.saturating_sub(0x10)..=hi).contains(&addr)
    }

    fn restore_writes(&self) -> Vec<Write> {
        let w = |port, v: u8| Write {
            at: 0,
            port,
            value: v as u16,
        };
        let b = &self.board;
        let base = b.wave_base();
        let mut out = vec![w(0x8F, b.ports[0x8F])];
        out.extend((0..64u16).map(|i| w(RAM_WRITE | (base + i) as u32, b.ram[(base + i) as usize])));
        out.extend((0x80..=0x94u32).filter(|&p| p != 0x8F).map(|p| w(p, b.ports[p as usize])));
        out
    }

    fn new_chip(&self, rate: u32) -> Result<Chip, String> {
        Ok(Chip(Chips::new(
            rate,
            CLOCK as f64,
            &[(Kind::WonderSwan, CLOCK)],
        )?))
    }

    fn feed(&self, chip: &mut Chip, writes: &[Write], out: &mut Vec<i16>) {
        for w in writes {
            let at = w.at.min(FRAME - 1);
            if w.port & RAM_WRITE != 0 {
                chip.0.write_ram(at, 0, w.port as u16, w.value as u8);
            } else {
                chip.0.write(at, 0, w.port as u8, w.value as u8);
            }
        }
        chip.0.end_frame(FRAME, out);
    }

    fn frame_length(&self) -> u32 {
        FRAME
    }

    fn frame_rate(&self) -> f64 {
        RATE
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::music_search::{measure, search, Trigger};
    use super::*;

    /// A tiny game in a 64 KB cartridge (code at F000:0000): the vertical blank interrupt
    /// calls a sound routine that plays song N (N written to 0200 by the main program, 1
    /// at boot) on channel 1, loading a square waveform and changing note every 8 frames
    /// through 16 notes, then round again.
    pub(in super::super) fn tiny_game() -> Vec<u8> {
        let mut rom = vec![0xFFu8; 0x10000];
        // CLI; SS = DS = ES = 0; SP = 2000; vector 26h (base 20h + vertical blank) to
        // F000:0100; [0200] = 1; interrupt base 20h, vertical blank on; STI; HLT; JMP $-3
        let main = [
            0xFA, 0x31, 0xC0, 0x8E, 0xD0, 0xBC, 0x00, 0x20, 0x8E, 0xD8, 0x8E, 0xC0, 0xC7, 0x06,
            0x98, 0x00, 0x00, 0x01, 0xC7, 0x06, 0x9A, 0x00, 0x00, 0xF0, 0xC6, 0x06, 0x00, 0x02,
            0x01, 0xB0, 0x20, 0xE6, 0xB0, 0xB0, 0x40, 0xE6, 0xB2, 0xFB, 0xF4, 0xEB, 0xFD,
        ];
        rom[..main.len()].copy_from_slice(&main);
        // F000:0100: PUSH AX; CALL 0200; acknowledge (OUT B6, 40h); POP AX; IRET
        let irq = [0x50, 0xE8, 0xFC, 0x00, 0xB0, 0x40, 0xE6, 0xB6, 0x58, 0xCF];
        rom[0x100..0x100 + irq.len()].copy_from_slice(&irq);
        // F000:0200: take a new song from [0200]; every 8th frame load a square waveform at
        // 0400 (port 8F = 10h), set channel 1's period to 0700h + song * 16 + note, full
        // volume, channel on.
        let mut sound = vec![
            0xA0, 0x00, 0x02, // MOV AL,[0200]
            0x08, 0xC0, // OR AL,AL
            0x74, 0x0F, // JZ +15
            0xA2, 0x01, 0x02, // MOV [0201],AL
            0xC6, 0x06, 0x00, 0x02, 0x00, // MOV BYTE [0200],0
            0xC6, 0x06, 0x02, 0x02, 0x00, // MOV BYTE [0202],0
            0x90, 0x90, // (padding to the jump target)
            0xA0, 0x01, 0x02, // MOV AL,[0201]
            0x08, 0xC0, // OR AL,AL
            0x74, 0x00, // JZ done (patched below)
            0xFE, 0x06, 0x02, 0x02, // INC BYTE [0202]
            0xA0, 0x02, 0x02, // MOV AL,[0202]
            0x24, 0x07, // AND AL,7
            0x75, 0x00, // JNZ done (patched below)
            0xB0, 0x10, 0xE6, 0x8F, // waveform table at 0400
            0xBF, 0x00, 0x04, 0xB9, 0x08, 0x00, 0xB0, 0xFF, 0xFC, 0xF3, 0xAA, // 8 x FF
            0xB9, 0x08, 0x00, 0x30, 0xC0, 0xF3, 0xAA, // 8 x 00
            0xA0, 0x02, 0x02, 0xB1, 0x03, 0xD2, 0xE8, 0x24, 0x0F, 0x88, 0xC3, // BL = note
            0xA0, 0x01, 0x02, 0xB1, 0x04, 0xD2, 0xE0, 0x00, 0xD8, // AL = song * 16 + note
            0xB4, 0x07, 0xE6, 0x80, 0x88, 0xE0, 0xE6, 0x81, // period
            0xB0, 0xFF, 0xE6, 0x88, // volume
            0xB0, 0x01, 0xE6, 0x90, // channel 1 on
            0xB0, 0x0F, 0xE6, 0x91, // output on
        ];
        let done = sound.len();
        sound.push(0xC3); // RET
        // The first JZ skips the 15 bytes after it; patch the two jumps to `done`.
        sound[28] = (done - 29) as u8;
        sound[39] = (done - 40) as u8;
        rom[0x200..0x200 + sound.len()].copy_from_slice(&sound);
        // Footer: JMP FAR F000:0000, mono, no save.
        rom[0xFFF0..0xFFF5].copy_from_slice(&[0xEA, 0x00, 0x00, 0x00, 0xF0]);
        rom[0xFFF5..].fill(0);
        rom
    }

    #[test]
    fn the_tiny_game_runs() {
        let mut m = WonderSwan::boot(&tiny_game()).unwrap();
        for _ in 0..20 {
            m.run_frame(None, None);
        }
        let w = m.take_sound();
        assert!(w.iter().any(|w| w.port == 0x90 && w.value == 1), "{}", w.len());
        assert!(w.iter().any(|w| w.port == RAM_WRITE | 0x400 && w.value == 0xFF));
        assert_eq!(m.board.ram[0x201], 1);
    }

    #[test]
    fn the_tiny_game_has_its_songs() {
        if !super::super::vgm_chips::available() {
            return;
        }
        let rom = tiny_game();
        let found = search::<WonderSwan>(&rom).unwrap();
        assert_eq!(found.why, None);
        assert_eq!(found.recipe.play, 0xF0200);
        assert_eq!(found.recipe.trigger, Trigger::Byte(0x0200));
        assert_eq!(found.songs.len(), 16, "{:?}", found.songs);
        let t = measure::<WonderSwan>(&rom, &found.recipe, 3).unwrap();
        assert_eq!(t.looping.map(|l| l.1), Some(128), "{t:?}");
    }

    #[test]
    fn banks_map_the_last_one_at_the_top() {
        let mut rom = vec![0u8; 0x40000];
        for (i, b) in rom.chunks_mut(0x10000).enumerate() {
            b[0] = i as u8;
        }
        let mut m = WonderSwan::boot(&rom).unwrap();
        assert_eq!(m.board.read(0xF0000), 3);
        m.board.output(0xC2, 1);
        assert_eq!(m.board.read(0x20000), 1);
        m.board.output(0xC0, 0);
        assert_eq!(m.board.read(0x40000), 0);
    }

    #[test]
    fn timers_interrupt() {
        let mut m = WonderSwan::boot(&tiny_game()).unwrap();
        m.board.output(0xB2, 0x80);
        m.board.output(0xA4, 2);
        m.board.output(0xA5, 0);
        m.board.output(0xA2, 0x03);
        m.board.advance(LINE);
        assert_eq!(m.board.pending(), None);
        m.board.advance(LINE);
        assert_eq!(m.board.pending(), Some(7));
        m.board.output(0xB6, 0x80);
        assert_eq!(m.board.pending(), None);
    }
}
