//! The Virtual Boy as a [`Machine`] for the music search ([`super::music_search`]): the V810
//! ([`super::v810`]), work RAM, the cartridge's ROM and RAM, the timer, the controller, and
//! just enough of the video chip (VIP) for the timing games wait on: its frame, game-frame,
//! drawing-finished and display-finished interrupts and its status registers. Nothing is
//! drawn; video memory is plain memory. The console has no BIOS. Its behaviour follows
//! Mednafen's Virtual Boy core (`external/beetle-vb-libretro`). The sound unit (VSU) goes to
//! libvgm's chip ([`super::vgm_chips`]): every write to it is a sound write, its port the
//! register's byte address over 4.

use std::sync::Arc;

use super::music_search::{Machine, Step, Write};
use super::v810::{Bus, Cpu};
use super::vgm_chips::{Chips, Kind};

/// CPU cycles a second, and per frame: four display periods of 384 columns of 259 cycles
/// (50.27 frames a second).
const CLOCK: u32 = 20_000_000;
const COLUMN: u32 = 259;
const PERIOD: u32 = 384 * COLUMN;
const FRAME: u32 = 4 * PERIOD;
pub const RATE: f64 = CLOCK as f64 / FRAME as f64;
/// The VSU's clock (a quarter of the CPU's).
const VSU_CLOCK: u32 = 5_000_000;
/// Drawing a frame: 28 blocks of 8 rows.
const BLOCK: u32 = 1120 * 4;
const BLOCKS: u32 = 28;
/// After each block the "block drawn" flag stays up this long.
const SBOUT_TIME: u32 = 1120;

/// The VIP's interrupt bits.
const INT_LFB_END: u16 = 0x0002;
const INT_RFB_END: u16 = 0x0004;
const INT_GAME_START: u16 = 0x0008;
const INT_FRAME_START: u16 = 0x0010;
const INT_XP_END: u16 = 0x4000;

/// Interrupt levels by source.
const IRQ_INPUT: u8 = 0;
const IRQ_TIMER: u8 = 1;
const IRQ_VIP: u8 = 4;

/// RAM as the search sees it: work RAM at 05000000, the cartridge's at 06000000.
const WRAM: u32 = 0x0500_0000;
const CART_RAM: u32 = 0x0600_0000;

/// The controller's Start button, as the serial read shifts it in.
const PAD_START: u16 = 0x1000;
/// The controller's signature bit, always set.
const PAD_SIGNATURE: u16 = 0x0002;
/// A hardware controller read takes 16 bits of 640 cycles.
const PAD_READ: u32 = 640 * 16;

/// Everything around the CPU. Plain data: cloned to run again from any moment.
#[derive(Clone)]
struct Board {
    rom: Arc<Vec<u8>>,
    wram: Vec<u8>,
    cart_ram: Vec<u8>,
    /// Video memory, its frame buffers and character memory at their own addresses.
    vram: Vec<u8>,
    /// The VIP: interrupt pending and enabled, display and drawing control, frame cycle,
    /// and the plain registers.
    int_pending: u16,
    int_enable: u16,
    dpctrl: u16,
    xpctrl: u16,
    frmcyc: u16,
    game_frames: u16,
    display_on: bool,
    /// When this frame's drawing started, if it did.
    drawing_from: Option<u32>,
    vip_regs: [u16; 64],
    /// The timer: control, reload value, counter, cycles to its next tick, status.
    tcr: u8,
    reload: u16,
    counter: u16,
    divider: i32,
    zstat: bool,
    zstat_shadow: bool,
    reload_pending: bool,
    /// The controller: serial control, cycles left in a hardware read, its interrupt.
    scr: u8,
    pad_reading: u32,
    pad_int: bool,
    start: bool,
    wcr: u8,
    /// The VSU's registers as last written, by port.
    vsu: Vec<u8>,
    /// CPU cycles into the frame; whether this frame's start has happened.
    cycle: u32,
    started: bool,
    sound: Vec<Write>,
    /// The lowest and highest stack addresses seen.
    stack: (u32, u32),
    /// RAM accesses, when traced: (address, written, value).
    trace: Option<Vec<(u32, bool, u8)>>,
}

impl Board {
    fn traced(&mut self, addr: u32, write: bool, v: u8) {
        if let Some(t) = self.trace.as_mut() {
            t.push((addr, write, v));
        }
    }

    /// The highest interrupt level asserted, if any.
    fn irq_level(&self) -> Option<u8> {
        if self.int_pending & self.int_enable != 0 {
            Some(IRQ_VIP)
        } else if self.zstat_shadow && self.tcr & 0x08 != 0 {
            Some(IRQ_TIMER)
        } else if self.pad_int {
            Some(IRQ_INPUT)
        } else {
            None
        }
    }

    fn pad(&self) -> u16 {
        PAD_SIGNATURE | if self.start { PAD_START } else { 0 }
    }

    /// The start of a frame: the right eye's display ends, the next frame and (every
    /// FRMCYC + 1 frames) game frame begin, and drawing starts.
    fn frame_start(&mut self) {
        if self.display_on {
            self.int_pending |= INT_RFB_END;
        }
        self.display_on = self.dpctrl & 0x2 != 0;
        if self.display_on {
            self.int_pending |= INT_FRAME_START;
        }
        self.game_frames += 1;
        if self.game_frames > self.frmcyc {
            self.int_pending |= INT_GAME_START;
            self.drawing_from = (self.xpctrl & 0x2 != 0).then_some(0);
            self.game_frames = 0;
        }
    }

    fn drawing(&self) -> bool {
        self.drawing_from.is_some_and(|d| self.cycle < d + BLOCKS * BLOCK)
    }

    /// Runs the clocks on by `cycles`: the VIP's events, the timer, the controller read.
    fn advance(&mut self, cycles: u32) {
        let before = self.cycle;
        self.cycle += cycles;
        let crossed = |at: u32| before < at && self.cycle >= at;
        if let Some(d) = self.drawing_from {
            if crossed(d + BLOCKS * BLOCK) {
                self.int_pending |= INT_XP_END;
            }
        }
        if crossed(2 * PERIOD) && self.display_on {
            self.int_pending |= INT_LFB_END;
        }
        if self.tcr & 0x01 != 0 {
            self.divider -= cycles as i32;
            while self.divider <= 0 {
                if self.counter == 0 || self.reload_pending {
                    self.counter = self.reload;
                    self.reload_pending = false;
                }
                if self.counter != 0 {
                    self.counter -= 1;
                }
                if self.counter == 0 || self.zstat {
                    self.zstat = true;
                    self.zstat_shadow = true;
                }
                self.divider += if self.tcr & 0x10 != 0 { 400 } else { 2000 };
            }
        }
        if self.pad_reading > 0 {
            if self.pad_reading <= cycles {
                self.pad_reading = 0;
                if self.scr & 0x80 == 0 {
                    self.pad_int = true;
                }
            } else {
                self.pad_reading -= cycles;
            }
        }
    }

    /// Cycles until something can happen without the CPU: for skipping a HALT.
    fn idle_cycles(&self) -> u32 {
        let mut n = FRAME - self.cycle.min(FRAME - 1);
        let mut at = |t: u32| {
            if t > self.cycle {
                n = n.min(t - self.cycle);
            }
        };
        at(2 * PERIOD);
        if let Some(d) = self.drawing_from {
            at(d + BLOCKS * BLOCK);
        }
        if self.tcr & 0x01 != 0 {
            n = n.min(self.divider.max(1) as u32);
        }
        if self.pad_reading > 0 {
            n = n.min(self.pad_reading);
        }
        n.max(1)
    }

    fn vip_read(&self, a: u32) -> u16 {
        match a & 0x7E {
            0x00 => self.int_pending,
            0x02 => self.int_enable,
            0x20 => {
                // Which buffer is being shown, while one is; SCANRDY always.
                let mut v = (self.dpctrl & 0x702) | 0x40;
                let period = self.cycle / PERIOD;
                if self.display_on && period & 1 == 1 {
                    v |= 1 << (2 + (period >> 1));
                }
                v
            }
            0x30 => 0xFFFF,
            0x40 => {
                let mut v = self.xpctrl & 0x2;
                if self.drawing() {
                    v |= 1 << 2;
                }
                if let Some(d) = self.drawing_from {
                    let t = self.cycle.saturating_sub(d);
                    let done = t / BLOCK;
                    if self.cycle >= d && done >= 1 && done <= BLOCKS && t - done * BLOCK < SBOUT_TIME {
                        v |= 0x8000 | ((done - 1) as u16) << 8;
                    }
                }
                v
            }
            0x44 => 2,
            r => self.vip_regs[(r >> 1) as usize],
        }
    }

    fn vip_write(&mut self, a: u32, v: u16) {
        let r = a & 0x7E;
        match r {
            0x02 => self.int_enable = v & 0xE01F,
            0x04 => self.int_pending &= !v,
            0x22 => {
                self.dpctrl = v & 0x703;
                if v & 1 != 0 {
                    self.int_pending &= !0x801F;
                }
            }
            0x2E => self.frmcyc = v & 0xF,
            0x42 => {
                self.xpctrl = v & 0x2;
                if v & 1 != 0 {
                    self.drawing_from = None;
                    self.int_pending &= !0xE000;
                }
            }
            0x00 | 0x20 | 0x30 | 0x40 | 0x44 => {}
            _ => self.vip_regs[(r >> 1) as usize] = v,
        }
    }

    fn hw_read(&mut self, a: u32) -> u8 {
        if a & 3 != 0 {
            return 0;
        }
        match a & 0xFF {
            0x10 => self.pad() as u8,
            0x14 => (self.pad() >> 8) as u8,
            0x18 => self.counter as u8,
            0x1C => (self.counter >> 8) as u8,
            0x20 => self.tcr | 0xE4 | if self.zstat { 0x02 } else { 0 },
            0x24 => self.wcr | 0xFC,
            0x28 => self.scr | 0x4C | if self.pad_reading > 0 { 0x02 } else { 0 },
            _ => 0,
        }
    }

    fn hw_write(&mut self, a: u32, v: u8) {
        if a & 3 != 0 {
            return;
        }
        match a & 0xFF {
            0x18 => {
                self.reload = (self.reload & 0xFF00) | v as u16;
                self.reload_pending = true;
            }
            0x1C => {
                self.reload = (self.reload & 0x00FF) | (v as u16) << 8;
                self.reload_pending = true;
            }
            0x20 => {
                if v & 0x04 != 0 {
                    if !(self.tcr & 0x01 != 0 && self.counter == 0) {
                        self.zstat = false;
                    }
                    self.zstat_shadow = false;
                }
                if v & 0x01 != 0 && self.tcr & 0x01 == 0 {
                    self.divider = if v & 0x10 != 0 { 400 } else { 2000 };
                }
                self.tcr = v & 0x19;
                if self.tcr & 0x08 == 0 {
                    self.zstat = false;
                    self.zstat_shadow = false;
                }
            }
            0x24 => self.wcr = v & 3,
            0x28 => {
                if v & 0x04 != 0 && self.scr & 0x01 == 0 && self.pad_reading == 0 {
                    self.pad_reading = PAD_READ;
                }
                if v & 0x01 != 0 {
                    self.pad_reading = 0;
                }
                if v & 0x80 != 0 {
                    self.pad_int = false;
                }
                self.scr = v & 0xB1;
            }
            _ => {}
        }
    }

    fn vsu_write(&mut self, a: u32, v: u8) {
        let port = (a & 0x7FF) >> 2;
        self.vsu[port as usize] = v;
        self.sound.push(Write {
            at: self.cycle,
            port,
            value: v as u16,
        });
    }

    fn rom_at(&self, a: u32) -> u8 {
        self.rom[a as usize & (self.rom.len() - 1)]
    }
}

impl Bus for Board {
    fn read8(&mut self, a: u32) -> u8 {
        let a = a & 0x07FF_FFFF;
        match a >> 24 {
            0 => {
                let r = a & 0xFFFFF;
                if (0x5E000..0x60000).contains(&r) {
                    let w = self.vip_read(r);
                    if r & 1 == 0 { w as u8 } else { (w >> 8) as u8 }
                } else {
                    self.vram[(a & 0x7FFFF) as usize]
                }
            }
            2 => self.hw_read(a),
            5 => {
                let v = self.wram[(a & 0xFFFF) as usize];
                self.traced(WRAM | (a & 0xFFFF), false, v);
                v
            }
            6 => {
                let v = self.cart_ram[(a & 0xFFFF) as usize];
                self.traced(CART_RAM | (a & 0xFFFF), false, v);
                v
            }
            7 => self.rom_at(a),
            _ => 0,
        }
    }

    fn read16(&mut self, a: u32) -> u16 {
        let a = a & 0x07FF_FFFE;
        match a >> 24 {
            0 if (0x5E000..0x60000).contains(&(a & 0xFFFFF)) => self.vip_read(a & 0xFFFFF),
            2 => self.hw_read(a) as u16,
            _ => u16::from_le_bytes([self.read8(a), self.read8(a | 1)]),
        }
    }

    fn write8(&mut self, a: u32, v: u8) {
        let a = a & 0x07FF_FFFF;
        match a >> 24 {
            0 => {
                let r = a & 0xFFFFF;
                if (0x5E000..0x60000).contains(&r) {
                    // A byte write to a register writes it whole.
                    self.vip_write(r, v as u16);
                } else {
                    self.vram[(a & 0x7FFFF) as usize] = v;
                }
            }
            1 => self.vsu_write(a, v),
            2 => self.hw_write(a, v),
            5 => {
                self.wram[(a & 0xFFFF) as usize] = v;
                self.traced(WRAM | (a & 0xFFFF), true, v);
            }
            6 => {
                self.cart_ram[(a & 0xFFFF) as usize] = v;
                self.traced(CART_RAM | (a & 0xFFFF), true, v);
            }
            _ => {}
        }
    }

    fn write16(&mut self, a: u32, v: u16) {
        let a = a & 0x07FF_FFFE;
        match a >> 24 {
            0 if (0x5E000..0x60000).contains(&(a & 0xFFFFF)) => self.vip_write(a & 0xFFFFF, v),
            // The VSU and the hardware registers take the low byte.
            1 => self.vsu_write(a, v as u8),
            2 => self.hw_write(a, v as u8),
            _ => {
                self.write8(a, v as u8);
                self.write8(a | 1, (v >> 8) as u8);
            }
        }
    }
}

/// A moment the CPU entered a routine: enough to call it again from there. The V810 keeps
/// return addresses in a register (and an interrupt's in a system register), so the CPU's
/// state is all it takes.
#[derive(Clone)]
pub struct Entry {
    cpu: Cpu,
    /// CPU cycles into the frame.
    cycle: u32,
}

/// A Virtual Boy: CPU and board.
#[derive(Clone)]
pub struct VirtualBoy {
    cpu: Cpu,
    board: Board,
}

/// The registers a song number can be passed in: the first argument registers.
const ARG_REGS: [usize; 4] = [6, 7, 8, 9];

impl VirtualBoy {
    fn step_once(&mut self) -> u32 {
        let mut t = 0;
        if let Some(level) = self.board.irq_level() {
            if let Some(c) = self.cpu.interrupt(level) {
                t += c;
            }
        }
        if let Some(tr) = self.board.trace.as_mut() {
            tr.clear();
        }
        t += if self.cpu.halted {
            self.board.idle_cycles()
        } else {
            self.cpu.step(&mut self.board)
        };
        self.board.advance(t);
        t
    }

    /// Runs the routine of `entry` once, from its own registers, until it returns. False
    /// when it doesn't within `budget` cycles.
    fn call(&mut self, entry: &Entry, set: Option<(u8, u8)>, budget: u32) -> bool {
        self.cpu = entry.cpu.clone();
        self.cpu.halted = false;
        if let Some((reg, v)) = set {
            self.cpu.r[ARG_REGS[reg as usize & 3]] = v as u32;
        }
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

impl Machine for VirtualBoy {
    type Entry = Entry;
    type Chip = Chip;
    const REGISTERS: u8 = 4;
    const CHANNELS: u16 = 2;

    fn boot(rom: &[u8]) -> Result<Self, String> {
        if rom.len() < 0x400 {
            return Err("not a Virtual Boy cartridge (too small)".into());
        }
        // Mirrored up to a power of two, as the cartridge bus does.
        let size = rom.len().next_power_of_two();
        let mut r = Vec::with_capacity(size);
        while r.len() < size {
            let n = (size - r.len()).min(rom.len());
            r.extend_from_slice(&rom[..n]);
        }
        let board = Board {
            rom: Arc::new(r),
            wram: vec![0; 0x10000],
            cart_ram: vec![0; 0x10000],
            vram: vec![0; 0x80000],
            int_pending: 0,
            int_enable: 0,
            dpctrl: 2,
            xpctrl: 0,
            frmcyc: 0,
            game_frames: 0,
            display_on: false,
            drawing_from: None,
            vip_regs: [0; 64],
            tcr: 0,
            reload: 0xFFFF,
            counter: 0xFFFF,
            divider: 2000,
            zstat: false,
            zstat_shadow: false,
            reload_pending: false,
            scr: 0,
            pad_reading: 0,
            pad_int: false,
            start: false,
            wcr: 0,
            vsu: vec![0; 0x200],
            cycle: 0,
            started: false,
            sound: Vec::new(),
            stack: (u32::MAX, 0),
            trace: None,
        };
        Ok(Self {
            cpu: Cpu::reset(),
            board,
        })
    }

    fn run_frame(
        &mut self,
        mut watch: Option<&mut dyn FnMut(&Step)>,
        stop_after: Option<u64>,
    ) -> bool {
        if !self.board.started {
            self.board.started = true;
            self.board.frame_start();
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
                let s = self.cpu.r[3] & 0x07FF_FFFF;
                let (lo, hi) = self.board.stack;
                self.board.stack = (lo.min(s), hi.max(s + 8));
            }
            if let Some(w) = watch.as_deref_mut() {
                calls.clear();
                calls.extend_from_slice(&self.cpu.calls);
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
        if let Some(d) = self.board.drawing_from.as_mut() {
            // Drawing that runs past the frame's end ends early in the next.
            *d = d.saturating_sub(FRAME);
        }
        self.board.started = false;
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
        cpu.keep_innermost();
        Entry {
            cpu,
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
        let ok = self.call(entry, Some((reg, v)), 80_000_000);
        self.board.cycle = cycle;
        self.cpu = cpu;
        for w in self.board.sound.iter_mut() {
            w.at = cycle;
        }
        ok
    }

    fn poke(&mut self, addr: u32, v: u8) {
        match addr >> 24 {
            5 => self.board.wram[(addr & 0xFFFF) as usize] = v,
            6 => self.board.cart_ram[(addr & 0xFFFF) as usize] = v,
            _ => {}
        }
    }

    fn incidental(_w: &Write) -> bool {
        false
    }

    fn is_stack(&self, addr: u32) -> bool {
        let (lo, hi) = self.board.stack;
        lo != u32::MAX && (lo.saturating_sub(0x40)..=hi).contains(&addr)
    }

    fn restore_writes(&self) -> Vec<Write> {
        let v = &self.board.vsu;
        let w = |port: u32| Write {
            at: 0,
            port,
            value: v[port as usize] as u16,
        };
        // Waveforms and the modulation table first, while every channel is off (the VSU
        // ignores them otherwise), then each channel's settings, its enable last.
        let mut out: Vec<Write> = (0..0x100).map(w).collect();
        for ch in 0..6u32 {
            let base = 0x100 + ch * 0x10;
            out.extend((1..8).map(|r| w(base + r)));
            out.push(w(base));
        }
        out
    }

    fn new_chip(&self, rate: u32) -> Result<Chip, String> {
        Ok(Chip(Chips::new(
            rate,
            CLOCK as f64,
            &[(Kind::VirtualBoyVsu, VSU_CLOCK)],
        )?))
    }

    fn feed(&self, chip: &mut Chip, writes: &[Write], out: &mut Vec<i16>) {
        for w in writes {
            chip.0
                .write_wide(w.at.min(FRAME - 1), 0, w.port as u16, w.value as u8);
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
mod tests {
    use super::super::music_search::{measure, search, Trigger};
    use super::*;

    // A small assembler for the V810's formats.
    fn f1(code: u32, r1: u32, r2: u32) -> Vec<u16> {
        vec![(code << 10 | r2 << 5 | r1) as u16]
    }
    fn f2(code: u32, imm: i32, r2: u32) -> Vec<u16> {
        vec![(code << 10 | r2 << 5 | (imm as u32 & 0x1F)) as u16]
    }
    fn f5(code: u32, r1: u32, r2: u32, imm: u32) -> Vec<u16> {
        vec![(code << 10 | r2 << 5 | r1) as u16, imm as u16]
    }
    fn br(cond: u32, disp: i32) -> Vec<u16> {
        vec![(0b100 << 13 | cond << 9 | (disp as u32 & 0x1FF)) as u16]
    }
    fn jump(code: u32, disp: i32) -> Vec<u16> {
        let d = disp as u32 & 0x03FF_FFFF;
        vec![(code << 10 | d >> 16) as u16, d as u16]
    }
    const MOV_I: u32 = 0x10;
    const ADD_I: u32 = 0x11;
    const CMP: u32 = 0x03;
    const SHL_I: u32 = 0x14;
    const SHR_I: u32 = 0x15;
    const MOVEA: u32 = 0x28;
    const MOVHI: u32 = 0x2F;
    const ANDI: u32 = 0x2D;
    const LD_B: u32 = 0x30;
    const LD_W: u32 = 0x33;
    const ST_B: u32 = 0x34;
    const ST_H: u32 = 0x35;
    const ST_W: u32 = 0x37;
    const BE: u32 = 2;
    const BNE: u32 = 10;
    const BR: u32 = 5;

    /// Code laid out at addresses in a 64 KB cartridge (at 07000000, mirrored up to the top).
    struct Asm {
        rom: Vec<u8>,
        at: usize,
    }

    impl Asm {
        fn org(&mut self, at: usize) {
            self.at = at;
        }
        fn put(&mut self, w: Vec<u16>) -> usize {
            let here = self.at;
            for h in w {
                self.rom[self.at..self.at + 2].copy_from_slice(&h.to_le_bytes());
                self.at += 2;
            }
            here
        }
        /// Puts a branch to `to` at `at` (both offsets in the cartridge).
        fn branch_at(&mut self, at: usize, cond: u32, to: usize) {
            let save = self.at;
            self.at = at;
            self.put(br(cond, to as i32 - at as i32));
            self.at = save;
        }
    }

    /// A tiny game: the VIP's game-frame interrupt calls a sound routine that plays song N
    /// (N written to 05000200 by the main program, 1 at boot) on channel 1, loading a square
    /// waveform and changing note every 8 frames through 16 notes, then round again.
    fn tiny_game() -> Vec<u8> {
        let mut a = Asm {
            rom: vec![0; 0x10000],
            at: 0,
        };
        // Main, at 07000000: stack at 0500FFF0, r20 = work RAM, r21 = the VIP's registers;
        // song 1 requested; game-frame interrupt on; interrupts on; HALT; BR back.
        a.put(f5(MOVHI, 0, 3, 0x0501));
        a.put(f5(MOVEA, 3, 3, 0xFFF0));
        a.put(f5(MOVHI, 0, 20, 0x0500));
        a.put(f5(MOVHI, 0, 21, 0x0006));
        a.put(f5(MOVEA, 21, 21, 0xF800));
        a.put(f2(MOV_I, 1, 1));
        a.put(f5(ST_B, 20, 1, 0x200));
        a.put(f5(MOVEA, 0, 1, INT_GAME_START as u32));
        a.put(f5(ST_H, 21, 1, 2));
        a.put(f2(0x1C, 5, 0)); // LDSR r0, PSW
        let halt = a.put(f2(0x1A, 0, 0));
        let back = a.at;
        a.put(vec![0]);
        a.branch_at(back, BR, halt);
        // The handler, at 07000200: save lp and r1, call the sound routine, acknowledge.
        a.org(0x200);
        a.put(f5(MOVEA, 3, 3, 0xFFF8));
        a.put(f5(ST_W, 3, 31, 0));
        a.put(f5(ST_W, 3, 1, 4));
        let call = a.at;
        a.put(jump(0x2B, 0x300 - call as i32));
        a.put(f5(MOVEA, 0, 1, INT_GAME_START as u32));
        a.put(f5(ST_H, 21, 1, 4));
        a.put(f5(LD_W, 3, 1, 4));
        a.put(f5(LD_W, 3, 31, 0));
        a.put(f5(MOVEA, 3, 3, 8));
        a.put(f2(0x19, 0, 0)); // RETI
        // The sound routine, at 07000300.
        a.org(0x300);
        a.put(f5(LD_B, 20, 10, 0x200));
        a.put(f1(CMP, 0, 10));
        let to_skip = a.put(vec![0]);
        a.put(f5(ST_B, 20, 10, 0x201));
        a.put(f5(ST_B, 20, 0, 0x200));
        a.put(f5(ST_B, 20, 0, 0x202));
        let skip = a.at;
        a.branch_at(to_skip, BE, skip);
        a.put(f5(LD_B, 20, 10, 0x201));
        a.put(f1(CMP, 0, 10));
        let to_done1 = a.put(vec![0]);
        a.put(f5(LD_B, 20, 11, 0x202));
        a.put(f2(ADD_I, 1, 11));
        a.put(f5(ST_B, 20, 11, 0x202));
        a.put(f5(ANDI, 11, 12, 7));
        a.put(f1(CMP, 0, 12));
        let to_done2 = a.put(vec![0]);
        // Channel 1 off, then its waveform (32 samples, a word apart): 16 high, 16 low.
        a.put(f5(MOVHI, 0, 13, 0x0100));
        a.put(f5(ST_B, 13, 0, 0x400));
        a.put(f5(MOVEA, 0, 14, 0));
        a.put(f5(MOVEA, 0, 15, 0x3F));
        let fill = a.at;
        a.put(f1(0x00, 13, 16)); // MOV r13, r16
        a.put(f1(0x01, 14, 16)); // ADD r14, r16
        a.put(f5(ST_B, 16, 15, 0));
        a.put(f2(ADD_I, 4, 14));
        a.put(f5(ANDI, 14, 17, 0x3F));
        a.put(f1(CMP, 0, 17));
        let to_fill = a.put(vec![0]);
        a.branch_at(to_fill, BNE, fill);
        a.put(f5(MOVEA, 0, 15, 0));
        a.put(f5(MOVEA, 0, 17, 0x80));
        a.put(f1(CMP, 17, 14));
        let to_fill2 = a.put(vec![0]);
        a.branch_at(to_fill2, BNE, fill);
        // Period 0x600 + (song & 15) * 16 + note, where note = (count >> 3) & 15.
        a.put(f2(SHR_I, 3, 11));
        a.put(f5(ANDI, 11, 11, 15));
        a.put(f5(ANDI, 10, 10, 15));
        a.put(f2(SHL_I, 4, 10));
        a.put(f1(0x01, 11, 10));
        a.put(f5(MOVEA, 10, 10, 0x600));
        a.put(f5(ST_B, 13, 10, 0x408));
        a.put(f2(SHR_I, 8, 10));
        a.put(f5(ST_B, 13, 10, 0x40C));
        a.put(f5(MOVEA, 0, 15, 0xFF));
        a.put(f5(ST_B, 13, 15, 0x404));
        a.put(f5(MOVEA, 0, 15, 0xF0));
        a.put(f5(ST_B, 13, 15, 0x410));
        a.put(f5(ST_B, 13, 0, 0x414));
        a.put(f5(ST_B, 13, 0, 0x418));
        a.put(f5(MOVEA, 0, 15, 0x80));
        a.put(f5(ST_B, 13, 15, 0x400));
        let done = a.put(f1(0x06, 31, 0)); // JMP [lp]
        a.branch_at(to_done1, BE, done);
        a.branch_at(to_done2, BNE, done);
        // The game-frame interrupt's vector (level 4, FFFFFE40): to the handler.
        a.org(0xFE40);
        a.put(jump(0x2A, 0x200 - 0xFE40));
        // Reset, at FFFFFFF0: to 07000000.
        a.org(0xFFF0);
        a.put(f5(MOVHI, 0, 1, 0x0700));
        a.put(f1(0x06, 1, 0));
        a.rom
    }

    #[test]
    fn the_tiny_game_runs() {
        let mut m = VirtualBoy::boot(&tiny_game()).unwrap();
        for _ in 0..20 {
            m.run_frame(None, None);
        }
        let w = m.take_sound();
        assert!(w.iter().any(|w| w.port == 0x100 && w.value == 0x80), "{}", w.len());
        assert!(w.iter().any(|w| w.port == 0 && w.value == 0x3F));
        assert_eq!(m.board.wram[0x201], 1);
        assert_eq!(m.board.wram[0x202], 20);
    }

    #[test]
    fn the_tiny_game_has_its_songs() {
        if !super::super::vgm_chips::available() {
            return;
        }
        let rom = tiny_game();
        let found = search::<VirtualBoy>(&rom).unwrap();
        assert_eq!(found.why, None);
        // The interrupt vector's relative jump lands in the cartridge's top mirror, and the
        // handler's call stays there.
        assert_eq!(found.recipe.play, 0xFFFF_0300);
        assert_eq!(found.recipe.trigger, Trigger::Byte(WRAM | 0x200));
        assert_eq!(found.songs.len(), 16, "{:?}", found.songs);
        let t = measure::<VirtualBoy>(&rom, &found.recipe, 3).unwrap();
        assert_eq!(t.looping.map(|l| l.1), Some(128), "{t:?}");
    }

    #[test]
    fn the_sound_is_heard() {
        if !super::super::vgm_chips::available() {
            return;
        }
        let mut m = VirtualBoy::boot(&tiny_game()).unwrap();
        let mut chip = m.new_chip(44_100).unwrap();
        let mut out = Vec::new();
        for _ in 0..30 {
            m.run_frame(None, None);
            let w = m.take_sound();
            m.feed(&mut chip, &w, &mut out);
        }
        let peak = out.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        assert!(peak > 1000, "peak {peak}");
        // 30 frames at 50.27 a second, stereo.
        let want = (30.0 / RATE * 44_100.0 * 2.0) as i64;
        assert!((out.len() as i64 - want).abs() < 100, "{} vs {want}", out.len());
    }

    #[test]
    fn the_timer_interrupts() {
        let mut m = VirtualBoy::boot(&tiny_game()).unwrap();
        m.board.hw_write(0x0200_0018, 3);
        m.board.hw_write(0x0200_001C, 0);
        m.board.hw_write(0x0200_0020, 0x09);
        m.board.advance(2000 * 2);
        assert_eq!(m.board.irq_level(), None);
        m.board.advance(2000);
        assert_eq!(m.board.irq_level(), Some(IRQ_TIMER));
        m.board.hw_write(0x0200_0020, 0x0D);
        assert_eq!(m.board.irq_level(), None);
    }

    #[test]
    fn start_is_read_from_the_controller() {
        let mut m = VirtualBoy::boot(&tiny_game()).unwrap();
        m.press_start(true);
        assert_eq!(m.board.hw_read(0x0200_0014), 0x10);
        assert_eq!(m.board.hw_read(0x0200_0010), 0x02);
        m.board.hw_write(0x0200_0028, 0x04);
        assert_eq!(m.board.hw_read(0x0200_0028) & 0x02, 0x02);
        m.board.advance(PAD_READ);
        assert_eq!(m.board.hw_read(0x0200_0028) & 0x02, 0);
        assert_eq!(m.board.irq_level(), Some(IRQ_INPUT));
    }
}
