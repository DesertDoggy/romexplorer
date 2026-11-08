//! The Game Boy Advance as a [`Machine`] for the music search ([`super::music_search`]), for
//! games whose music is not in the common sound engine (the m4a path plays those): the
//! ARM7TDMI ([`super::arm7`]), the memory map, the interrupt controller, the four timers and
//! DMA channels, the keypad, and the display's timing (vertical and horizontal blank, the
//! line counter). Nothing is drawn. No BIOS file is needed: a stand-in takes its place, its
//! interrupt handler real ARM code (calling the game's handler from 03007FFC, as the BIOS
//! does) and its calls (SWI) done natively.
//!
//! The sound is the Game Boy's four channels ([`super::gb_apu`]), whose registers map onto
//! the Game Boy's, and two 8-bit sample channels the game fills itself: its sound code
//! mixes each frame's samples into a buffer in RAM, which DMA then feeds to the hardware a
//! sample at a time. The search needs the sound writes to happen inside the sound routine,
//! and the routine run alone has no time passing for DMA to play anything, so the samples
//! are taken where the game writes them: a write into a buffer a sample-channel DMA reads
//! from is a sound write (its channel and place in the buffer as the port), and each
//! frame's buffer writes, in the order first made, are that frame's samples.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use super::arm7::{Bus, Cpu, MODE_IRQ, MODE_SVC, MODE_SYS, VECTOR_SWI};
use super::gb_apu::Apu;
use super::music_search::{Machine, Step, Write};

/// CPU cycles a second, per scanline, and scanlines per frame (59.73 frames a second).
const CLOCK: u32 = 16_777_216;
const LINE: u32 = 1232;
const LINES: u32 = 228;
const FRAME: u32 = LINE * LINES;
pub const RATE: f64 = CLOCK as f64 / FRAME as f64;
/// The cycle in each line the horizontal blank starts at, and the line the vertical one
/// starts on.
const HBLANK: u32 = 1006;
const VBLANK_LINE: u32 = 160;

/// Interrupt sources, by bit.
const INT_VBLANK: u16 = 1 << 0;
const INT_HBLANK: u16 = 1 << 1;
const INT_VCOUNT: u16 = 1 << 2;
const INT_TIMER0: u16 = 1 << 3;
const INT_DMA0: u16 = 1 << 8;

/// Where the BIOS keeps the flags IntrWait waits on (the game's interrupt handler is at
/// 03007FFC, read by the stand-in's own code).
const BIOS_IF: u32 = 0x0300_7FF8;

/// A sound write's port for SOUNDCNT_H (the mixer's volumes), and for a sample written to
/// a buffer: this bit, the channels (1 A, 2 B, 3 both) at bit 24, the place in the buffer.
const SOUNDCNT_H: u32 = 0x0400_0082;
const SAMPLE: u32 = 0x8000_0000;

/// The BIOS stand-in: the vectors, and the interrupt handler at 18h, which saves the
/// registers the game's handler may change, calls it (BX, with LR set, as a call), and
/// returns from the interrupt. SWIs are trapped at 08h and done natively.
const BIOS_CODE: [(u32, u32); 9] = [
    (0x00, 0xEAFF_FFFE), // B . (reset: never used)
    (0x08, 0xE1B0_F00E), // MOVS PC, LR (SWI: trapped before it runs)
    (0x18, 0xE92D_500F), // STMFD SP!, {R0-R3, R12, LR}
    (0x1C, 0xE3A0_0301), // MOV R0, #0x04000000
    (0x20, 0xE510_0004), // LDR R0, [R0, #-4]
    (0x24, 0xE1A0_E00F), // MOV LR, PC
    (0x28, 0xE12F_FF10), // BX R0
    (0x2C, 0xE8BD_500F), // LDMFD SP!, {R0-R3, R12, LR}
    (0x30, 0xE25E_F004), // SUBS PC, LR, #4
];

/// The Game Boy sound register a GBA sound register (offset in I/O) maps onto.
fn psg_register(off: u32) -> Option<u16> {
    Some(match off {
        0x60 => 0xFF10,
        0x62 => 0xFF11,
        0x63 => 0xFF12,
        0x64 => 0xFF13,
        0x65 => 0xFF14,
        0x68 => 0xFF16,
        0x69 => 0xFF17,
        0x6C => 0xFF18,
        0x6D => 0xFF19,
        0x70 => 0xFF1A,
        0x72 => 0xFF1B,
        0x73 => 0xFF1C,
        0x74 => 0xFF1D,
        0x75 => 0xFF1E,
        0x78 => 0xFF20,
        0x79 => 0xFF21,
        0x7C => 0xFF22,
        0x7D => 0xFF23,
        0x80 => 0xFF24,
        0x81 => 0xFF25,
        0x84 => 0xFF26,
        0x90..=0x9F => 0xFF30 + (off - 0x90) as u16,
        _ => return None,
    })
}

#[derive(Clone, Copy, Default)]
struct Timer {
    reload: u16,
    counter: u16,
    ctl: u8,
    /// Cycles counted towards the next tick.
    frac: u32,
}

#[derive(Clone, Copy, Default)]
struct Dma {
    sad: u32,
    dad: u32,
    cnt: u16,
    ctl: u16,
    /// The running source, destination and count.
    src: u32,
    dst: u32,
    count: u32,
}

/// Everything around the CPU. Plain data: cloned to run again from any moment.
#[derive(Clone)]
struct Board {
    rom: Arc<Vec<u8>>,
    bios: Arc<Vec<u8>>,
    ewram: Vec<u8>,
    iwram: Vec<u8>,
    sram: Vec<u8>,
    io: Vec<u8>,
    ie: u16,
    iflags: u16,
    timers: [Timer; 4],
    dma: [Dma; 4],
    /// The sample channels' queues.
    fifo: [Vec<i8>; 2],
    /// The buffer each sample channel's DMA reads: lowest address, and the highest read.
    bufs: [Option<(u32, u32)>; 2],
    start: bool,
    /// HALTCNT was written.
    halt: bool,
    /// CPU cycles into the frame.
    cycle: u32,
    sound: Vec<Write>,
    /// The Game Boy sound registers as last written (FF10-FF3F).
    psg: [u8; 0x30],
    /// The lowest and highest stack addresses seen.
    stack: (u32, u32),
    /// RAM accesses, when traced: (address, written, value).
    trace: Option<Vec<(u32, bool, u8)>>,
}

impl Board {
    fn line(&self) -> u32 {
        self.cycle / LINE % LINES
    }

    fn traced(&mut self, addr: u32, write: bool, v: u32, size: u32) {
        if let Some(t) = self.trace.as_mut() {
            for i in 0..size {
                t.push((addr + i, write, (v >> (i * 8)) as u8));
            }
        }
    }

    fn io16(&self, off: usize) -> u16 {
        u16::from_le_bytes([self.io[off], self.io[off + 1]])
    }

    fn soundcnt_h(&self) -> u16 {
        self.io16(0x82)
    }

    fn interrupt(&mut self, bits: u16) {
        self.iflags |= bits;
    }

    fn pending(&self) -> bool {
        self.io[0x208] & 1 != 0 && self.ie & self.iflags != 0
    }

    // Memory.

    fn read(&mut self, a: u32, size: u32) -> u32 {
        let le = |m: &[u8], i: usize| -> u32 {
            let mut v = 0u32;
            for k in 0..size as usize {
                v |= (m[(i + k) % m.len()] as u32) << (k * 8);
            }
            v
        };
        match a >> 24 {
            0 if a < 0x4000 => le(&self.bios, a as usize),
            2 => {
                let v = le(&self.ewram, (a & 0x3FFFF) as usize);
                self.traced(0x0200_0000 | (a & 0x3FFFF), false, v, size);
                v
            }
            3 => {
                let v = le(&self.iwram, (a & 0x7FFF) as usize);
                self.traced(0x0300_0000 | (a & 0x7FFF), false, v, size);
                v
            }
            4 => {
                let mut v = 0;
                for k in 0..size {
                    v |= (self.io_read((a + k) & 0x00FF_FFFF) as u32) << (k * 8);
                }
                v
            }
            8..=0x0D => {
                let off = (a & 0x01FF_FFFF) as usize;
                if off < self.rom.len() {
                    le(&self.rom, off)
                } else {
                    // Past the end: the address bus, as the cartridge leaves it.
                    let h = |o: usize| ((o >> 1) & 0xFFFF) as u32;
                    match size {
                        4 => h(off) | h(off + 2) << 16,
                        2 => h(off),
                        _ => h(off) >> ((off & 1) * 8) & 0xFF,
                    }
                }
            }
            0x0E | 0x0F => {
                let b = self.sram[(a & 0xFFFF) as usize] as u32;
                b * match size {
                    4 => 0x0101_0101,
                    2 => 0x0101,
                    _ => 1,
                }
            }
            _ => 0,
        }
    }

    fn write(&mut self, a: u32, size: u32, v: u32) {
        match a >> 24 {
            2 | 3 => {
                let (mem, at, base) = if a >> 24 == 2 {
                    (&mut self.ewram, (a & 0x3FFFF) as usize, 0x0200_0000)
                } else {
                    (&mut self.iwram, (a & 0x7FFF) as usize, 0x0300_0000)
                };
                let len = mem.len();
                for k in 0..size as usize {
                    mem[(at + k) % len] = (v >> (k * 8)) as u8;
                }
                let addr = base | at as u32;
                self.traced(addr, true, v, size);
                self.buffer_write(addr, size, v);
            }
            4 => {
                let off = a & 0x00FF_FFFF;
                if (0xA0..0xA8).contains(&off) {
                    // The sample channels' queues.
                    let ch = ((off - 0xA0) / 4) as usize;
                    for k in 0..size {
                        if self.fifo[ch].len() < 32 {
                            self.fifo[ch].push((v >> (k * 8)) as u8 as i8);
                        }
                    }
                    return;
                }
                for k in 0..size {
                    self.io_write(off + k, (v >> (k * 8)) as u8);
                }
            }
            0x0E | 0x0F => self.sram[(a & 0xFFFF) as usize] = v as u8,
            _ => {}
        }
    }

    /// A write into a sample buffer: a sound write per byte.
    fn buffer_write(&mut self, addr: u32, size: u32, v: u32) {
        if self.bufs == [None, None] {
            return;
        }
        for k in 0..size {
            let at = addr + k;
            let mut ch = 0;
            let mut lo = 0;
            for (i, b) in self.bufs.iter().enumerate() {
                if let Some((l, h)) = *b {
                    if (l..h).contains(&at) {
                        if ch == 0 {
                            lo = l;
                        }
                        ch |= 1 << i;
                    }
                }
            }
            if ch != 0 {
                self.sound.push(Write {
                    at: self.cycle,
                    port: SAMPLE | ch << 24 | (at - lo),
                    value: (v >> (k * 8)) as u8 as u16,
                });
            }
        }
    }

    fn io_read(&mut self, off: u32) -> u8 {
        let off = off as usize;
        if off >= 0x400 {
            return 0;
        }
        match off {
            0x04 => {
                let l = self.line();
                let vblank = (VBLANK_LINE..LINES - 1).contains(&l);
                let hblank = self.cycle % LINE >= HBLANK;
                let vmatch = l == self.io[0x05] as u32;
                (self.io[0x04] & 0x38) | vblank as u8 | (hblank as u8) << 1 | (vmatch as u8) << 2
            }
            0x06 => self.line() as u8,
            0x07 => 0,
            0x100..=0x10F if off & 3 < 2 => {
                let t = &self.timers[(off - 0x100) / 4];
                (t.counter >> ((off & 1) * 8)) as u8
            }
            0x130 => !((self.start as u8) << 3),
            0x131 => 0x03,
            0x200 => self.ie as u8,
            0x201 => (self.ie >> 8) as u8,
            0x202 => self.iflags as u8,
            0x203 => (self.iflags >> 8) as u8,
            _ => self.io[off],
        }
    }

    fn io_write(&mut self, off: u32, v: u8) {
        let off = off as usize;
        if off >= 0x400 {
            return;
        }
        let old = self.io[off];
        self.io[off] = v;
        match off {
            0x60..=0x9F => {
                if let Some(reg) = psg_register(off as u32) {
                    self.psg[(reg - 0xFF10) as usize] = v;
                    self.sound.push(Write {
                        at: self.cycle,
                        port: reg as u32,
                        value: v as u16,
                    });
                }
                if off == 0x82 || off == 0x83 {
                    if off == 0x83 {
                        // The queues' resets.
                        if v & 0x08 != 0 {
                            self.fifo[0].clear();
                        }
                        if v & 0x80 != 0 {
                            self.fifo[1].clear();
                        }
                        self.io[0x83] &= 0x77;
                    }
                    self.sound.push(Write {
                        at: self.cycle,
                        port: SOUNDCNT_H,
                        value: self.soundcnt_h(),
                    });
                }
            }
            0xB0..=0xDF => {
                let n = (off - 0xB0) / 12;
                let r = (off - 0xB0) % 12;
                let d = &mut self.dma[n];
                match r {
                    0..=3 => {
                        let sh = r * 8;
                        d.sad = (d.sad & !(0xFF << sh)) | (v as u32) << sh;
                    }
                    4..=7 => {
                        let sh = (r - 4) * 8;
                        d.dad = (d.dad & !(0xFF << sh)) | (v as u32) << sh;
                    }
                    8 => d.cnt = (d.cnt & 0xFF00) | v as u16,
                    9 => d.cnt = (d.cnt & 0x00FF) | (v as u16) << 8,
                    10 => d.ctl = (d.ctl & 0xFF00) | v as u16,
                    _ => {
                        let was = d.ctl & 0x8000 != 0;
                        d.ctl = (d.ctl & 0x00FF) | (v as u16) << 8;
                        if !was && v & 0x80 != 0 {
                            self.dma_start(n);
                        }
                    }
                }
            }
            0x100..=0x10F => {
                let t = &mut self.timers[(off - 0x100) / 4];
                match off & 3 {
                    0 => t.reload = (t.reload & 0xFF00) | v as u16,
                    1 => t.reload = (t.reload & 0x00FF) | (v as u16) << 8,
                    2 => {
                        if old & 0x80 == 0 && v & 0x80 != 0 {
                            t.counter = t.reload;
                            t.frac = 0;
                        }
                        t.ctl = v;
                    }
                    _ => {}
                }
            }
            0x200 => self.ie = (self.ie & 0xFF00) | v as u16,
            0x201 => self.ie = (self.ie & 0x00FF) | (v as u16) << 8,
            0x202 => self.iflags &= !(v as u16),
            0x203 => self.iflags &= !((v as u16) << 8),
            0x301 => self.halt = true,
            _ => {}
        }
    }

    // DMA.

    fn dma_start(&mut self, n: usize) {
        let d = &mut self.dma[n];
        d.src = d.sad & if n == 0 { 0x07FF_FFFF } else { 0x0FFF_FFFF };
        d.dst = d.dad & if n == 3 { 0x0FFF_FFFF } else { 0x07FF_FFFF };
        d.count = match d.cnt {
            0 if n == 3 => 0x10000,
            0 => 0x4000,
            c => c as u32,
        };
        if let Some(ch) = self.fifo_of(n) {
            // A sample buffer starts here (or a double buffer's second half does).
            let src = self.dma[n].src;
            let b = &mut self.bufs[ch];
            *b = Some(match *b {
                Some((lo, hi)) if lo <= src && src <= hi + 0x100 => (lo, hi.max(src)),
                Some((lo, hi)) if src < lo && lo - src <= 0x2000 => (src, hi),
                _ => (src, src),
            });
        }
        if (self.dma[n].ctl >> 12) & 3 == 0 {
            self.dma_run(n);
        }
    }

    /// The sample channel DMA `n` feeds, if it feeds one.
    fn fifo_of(&self, n: usize) -> Option<usize> {
        let d = &self.dma[n];
        if (d.ctl >> 12) & 3 != 3 || !(n == 1 || n == 2) {
            return None;
        }
        match d.dad & 0x0FFF_FFFF {
            0x0400_00A0 => Some(0),
            0x0400_00A4 => Some(1),
            _ => None,
        }
    }

    fn dma_run(&mut self, n: usize) {
        let d = self.dma[n];
        let fifo = self.fifo_of(n);
        let word = d.ctl & 0x0400 != 0 || fifo.is_some();
        let size = if word { 4 } else { 2 };
        let count = if fifo.is_some() { 4 } else { d.count };
        let step = |adj: u16| -> u32 {
            match adj {
                0 | 3 => size,
                1 => size.wrapping_neg(),
                _ => 0,
            }
        };
        let ds = if fifo.is_some() { 0 } else { step((d.ctl >> 5) & 3) };
        let ss = step((d.ctl >> 7) & 3);
        let (mut src, mut dst) = (d.src & !(size - 1), d.dst & !(size - 1));
        for _ in 0..count {
            let v = self.read(src, size);
            self.write(dst, size, v);
            src = src.wrapping_add(ss);
            dst = dst.wrapping_add(ds);
        }
        if let Some(ch) = fifo {
            if let Some((lo, hi)) = self.bufs[ch] {
                self.bufs[ch] = Some((lo.min(d.src), hi.max(src)));
            }
        }
        let d = &mut self.dma[n];
        d.src = src;
        d.dst = dst;
        if d.ctl & 0x0200 != 0 && (d.ctl >> 12) & 3 != 0 {
            d.count = if d.cnt == 0 { 0x4000 } else { d.cnt as u32 };
            if (d.ctl >> 5) & 3 == 3 {
                d.dst = d.dad;
            }
        } else {
            d.ctl &= !0x8000;
            self.io[0xBB + n * 12] &= 0x7F;
        }
        if self.dma[n].ctl & 0x4000 != 0 {
            self.interrupt(INT_DMA0 << n);
        }
    }

    /// Runs the DMA channels waiting on `timing` (1 vertical blank, 2 horizontal).
    fn dma_event(&mut self, timing: u16) {
        for n in 0..4 {
            let d = &self.dma[n];
            if d.ctl & 0x8000 != 0 && (d.ctl >> 12) & 3 == timing {
                self.dma_run(n);
            }
        }
    }

    // Timers and the sample channels.

    fn overflow(&mut self, n: usize) {
        if self.timers[n].ctl & 0x40 != 0 {
            self.interrupt(INT_TIMER0 << n);
        }
        let h = self.soundcnt_h();
        for ch in 0..2 {
            if ((h >> (10 + ch * 4)) & 1) as usize != n {
                continue;
            }
            if !self.fifo[ch].is_empty() {
                self.fifo[ch].remove(0);
            }
            if self.fifo[ch].len() <= 16 {
                if let Some(d) = (1..=2).find(|&d| {
                    self.dma[d].ctl & 0x8000 != 0 && self.fifo_of(d) == Some(ch)
                }) {
                    self.dma_run(d);
                }
            }
        }
        if n < 3 && self.timers[n + 1].ctl & 0x84 == 0x84 {
            let next = &mut self.timers[n + 1];
            next.counter = next.counter.wrapping_add(1);
            if next.counter == 0 {
                next.counter = next.reload;
                self.overflow(n + 1);
            }
        }
    }

    fn tick_timers(&mut self, cycles: u32) {
        for n in 0..4 {
            let t = self.timers[n];
            if t.ctl & 0x80 == 0 || t.ctl & 0x04 != 0 {
                continue;
            }
            let shift = [0, 6, 8, 10][(t.ctl & 3) as usize];
            let total = t.frac + cycles;
            let mut ticks = total >> shift;
            self.timers[n].frac = total & ((1 << shift) - 1);
            while ticks > 0 {
                let t = &mut self.timers[n];
                let to_overflow = 0x10000 - t.counter as u32;
                if ticks >= to_overflow {
                    ticks -= to_overflow;
                    t.counter = t.reload;
                    self.overflow(n);
                } else {
                    t.counter += ticks as u16;
                    ticks = 0;
                }
            }
        }
    }

    // The display's timing.

    fn new_line(&mut self, l: u32) {
        let dispstat = self.io[0x04];
        if l == VBLANK_LINE {
            if dispstat & 0x08 != 0 {
                self.interrupt(INT_VBLANK);
            }
            self.dma_event(1);
        }
        if l == self.io[0x05] as u32 && dispstat & 0x20 != 0 {
            self.interrupt(INT_VCOUNT);
        }
    }

    fn hblank(&mut self, l: u32) {
        if self.io[0x04] & 0x10 != 0 {
            self.interrupt(INT_HBLANK);
        }
        if l < VBLANK_LINE {
            self.dma_event(2);
        }
    }

    /// The cycles from now to the next display event.
    fn to_next_event(&self) -> u32 {
        let pos = self.cycle % LINE;
        if pos < HBLANK {
            HBLANK - pos
        } else {
            LINE - pos
        }
    }

    /// Lets `cycles` pass: the timers, and the display's events on the way.
    fn advance(&mut self, cycles: u32) {
        self.tick_timers(cycles);
        let end = self.cycle + cycles;
        let mut t = self.cycle;
        loop {
            let line = t / LINE;
            let next = if t % LINE < HBLANK {
                line * LINE + HBLANK
            } else {
                (line + 1) * LINE
            };
            if next > end {
                break;
            }
            self.cycle = next;
            if next % LINE == HBLANK {
                self.hblank(line % LINES);
            } else {
                self.new_line((line + 1) % LINES);
            }
            t = next;
        }
        self.cycle = end;
    }
}

impl Bus for Board {
    fn read8(&mut self, a: u32) -> u8 {
        self.read(a, 1) as u8
    }
    fn read16(&mut self, a: u32) -> u16 {
        self.read(a, 2) as u16
    }
    fn read32(&mut self, a: u32) -> u32 {
        self.read(a, 4)
    }
    fn write8(&mut self, a: u32, v: u8) {
        self.write(a, 1, v as u32)
    }
    fn write16(&mut self, a: u32, v: u16) {
        self.write(a, 2, v as u32)
    }
    fn write32(&mut self, a: u32, v: u32) {
        self.write(a, 4, v)
    }
    fn cycles(&self, a: u32, size: u32) -> u32 {
        match a >> 24 {
            2 => {
                if size == 4 {
                    6
                } else {
                    3
                }
            }
            5..=7 if size == 4 => 2,
            8..=0x0D => {
                // Sequential wait states from WAITCNT (the prefetch buffer is not modelled).
                let ws = if self.io[0x204] & 0x10 != 0 { 1 } else { 2 };
                (1 + ws) * if size == 4 { 2 } else { 1 }
            }
            0x0E | 0x0F => 5,
            _ => 1,
        }
    }
}

// The BIOS's decompression calls.

/// A reader of bytes from `from` on.
struct Bytes<'a> {
    read: &'a mut dyn FnMut(u32) -> u8,
    at: u32,
}

impl Bytes<'_> {
    fn next(&mut self) -> u8 {
        let b = (self.read)(self.at);
        self.at = self.at.wrapping_add(1);
        b
    }
    fn word(&mut self) -> u32 {
        u32::from_le_bytes([self.next(), self.next(), self.next(), self.next()])
    }
}

/// The most a decompression call writes (the largest RAM, with room to spare).
const UNPACK_MAX: usize = 1 << 20;

fn decompress_lz77(read: &mut dyn FnMut(u32) -> u8, from: u32) -> Vec<u8> {
    let mut src = Bytes { read, at: from };
    let size = ((src.word() >> 8) as usize).min(UNPACK_MAX);
    let mut out = Vec::with_capacity(size);
    while out.len() < size {
        let flags = src.next();
        for bit in (0..8).rev() {
            if out.len() >= size {
                break;
            }
            if flags & (1 << bit) == 0 {
                out.push(src.next());
            } else {
                let a = src.next() as usize;
                let b = src.next() as usize;
                let disp = ((a & 0xF) << 8 | b) + 1;
                for _ in 0..(a >> 4) + 3 {
                    let v = if disp <= out.len() { out[out.len() - disp] } else { 0 };
                    out.push(v);
                }
            }
        }
    }
    out.truncate(size);
    out
}

fn decompress_rl(read: &mut dyn FnMut(u32) -> u8, from: u32) -> Vec<u8> {
    let mut src = Bytes { read, at: from };
    let size = ((src.word() >> 8) as usize).min(UNPACK_MAX);
    let mut out = Vec::with_capacity(size);
    while out.len() < size {
        let flag = src.next();
        if flag & 0x80 != 0 {
            let v = src.next();
            out.extend(std::iter::repeat(v).take((flag & 0x7F) as usize + 3));
        } else {
            for _ in 0..(flag & 0x7F) as usize + 1 {
                out.push(src.next());
            }
        }
    }
    out.truncate(size);
    out
}

fn decompress_huff(read: &mut dyn FnMut(u32) -> u8, from: u32) -> Vec<u8> {
    let mut src = Bytes { read, at: from };
    let header = src.word();
    let bits = header & 0xF;
    let size = ((header >> 8) as usize).min(UNPACK_MAX);
    let tree_size = src.next() as u32;
    let root = from + 5;
    let mut node_at = root;
    let mut node = src.next();
    src.at = from + 4 + (tree_size + 1) * 2;
    let mut acc: u32 = 0;
    let mut have = 0;
    let mut out = Vec::with_capacity(size);
    if bits == 0 || bits > 8 {
        return out;
    }
    // A tree that never reaches a leaf (garbage passed in) would read on forever; real data
    // needs far fewer than 16 words a byte.
    let mut words_left = size * 16 + 64;
    while out.len() < size && words_left > 0 {
        words_left -= 1;
        let word = src.word();
        for bit in (0..32).rev() {
            let right = word >> bit & 1 != 0;
            let next_at = (node_at & !1) + (node & 0x3F) as u32 * 2 + 2 + right as u32;
            let leaf = node & if right { 0x40 } else { 0x80 } != 0;
            let v = (src.read)(next_at);
            if leaf {
                acc |= (v as u32 & ((1 << bits) - 1)) << have;
                have += bits;
                if have >= 8 {
                    out.push(acc as u8);
                    acc >>= 8;
                    have -= 8;
                }
                node_at = root;
                node = (src.read)(root);
                if out.len() >= size {
                    break;
                }
            } else {
                node_at = next_at;
                node = v;
            }
        }
    }
    out.truncate(size);
    out
}

/// A moment the CPU entered a routine: enough to call it again from there.
#[derive(Clone)]
pub struct Entry {
    cpu: Cpu,
    /// Where the routine returns to, and whether it is an interrupt handler.
    ret: u32,
    exception: bool,
    /// CPU cycles into the frame.
    cycle: u32,
}

/// A Game Boy Advance: CPU and board.
#[derive(Clone)]
pub struct GameBoyAdvance {
    cpu: Cpu,
    board: Board,
    /// IntrWait in progress: the interrupts waited for, and where it returns to.
    wait: Option<(u16, u32)>,
    /// BIOS calls the stand-in doesn't do, met so far.
    unknown: BTreeSet<u8>,
}

impl GameBoyAdvance {
    /// BIOS calls the game made that the stand-in doesn't do.
    pub fn unknown_bios_calls(&self) -> Vec<u8> {
        self.unknown.iter().copied().collect()
    }

    /// The SWI just taken, done natively, then returned from.
    fn swi(&mut self) {
        // The call's number: the byte below LR in both states (bits 16-23 of an ARM SWI,
        // 0-7 of a Thumb one).
        let n = self.board.read(self.cpu.r[14].wrapping_sub(2), 1) as u8;
        let r = self.cpu.r;
        match n {
            0x01 => {
                if r[0] & 1 != 0 {
                    self.board.ewram.fill(0);
                }
                if r[0] & 2 != 0 {
                    self.board.iwram[..0x7E00].fill(0);
                }
            }
            0x02 | 0x03 => self.board.halt = true,
            0x04 | 0x05 => {
                let (discard, mask) = if n == 5 { (true, 1) } else { (r[0] != 0, r[1] as u16) };
                if discard {
                    let f = self.board.read(BIOS_IF, 2) as u16 & !mask;
                    self.board.write(BIOS_IF, 2, f as u32);
                }
                self.board.io[0x208] = 1;
                self.cpu.hle_exception_return();
                self.wait = Some((mask, self.cpu.r[15]));
                return;
            }
            0x06 | 0x07 => {
                let (num, den) = if n == 6 {
                    (r[0] as i32, r[1] as i32)
                } else {
                    (r[1] as i32, r[0] as i32)
                };
                if den != 0 {
                    let q = num.wrapping_div(den);
                    self.cpu.r[0] = q as u32;
                    self.cpu.r[1] = num.wrapping_rem(den) as u32;
                    self.cpu.r[3] = q.unsigned_abs();
                }
            }
            0x08 => self.cpu.r[0] = (r[0] as f64).sqrt() as u32,
            0x09 => {
                // GBATEK's polynomial.
                let x = r[0] as i32 as i64;
                let a = -((x * x) >> 14);
                let mut b = ((0xA9 * a) >> 14) + 0x390;
                for c in [0x91C, 0xFB6, 0x16AA, 0x2081, 0x3651, 0xA2F9] {
                    b = ((a * b) >> 14) + c;
                }
                self.cpu.r[0] = ((x * b) >> 16) as i32 as u32;
            }
            0x0A => {
                let x = r[0] as i16 as f64;
                let y = r[1] as i16 as f64;
                let a = y.atan2(x).rem_euclid(std::f64::consts::TAU);
                self.cpu.r[0] = ((a / std::f64::consts::TAU * 65536.0) as u32) & 0xFFFF;
            }
            0x0B | 0x0C => {
                let (mut src, mut dst, ctl) = (r[0], r[1], r[2]);
                let fill = ctl & (1 << 24) != 0;
                let size = if n == 0x0C || ctl & (1 << 26) != 0 { 4 } else { 2 };
                let mut count = ctl & 0x1F_FFFF;
                if n == 0x0C {
                    count = (count + 7) & !7;
                }
                src &= !(size - 1);
                dst &= !(size - 1);
                let first = self.board.read(src, size);
                for _ in 0..count.min(0x10_0000) {
                    let v = if fill { first } else { self.board.read(src, size) };
                    self.board.write(dst, size, v);
                    if !fill {
                        src = src.wrapping_add(size);
                    }
                    dst = dst.wrapping_add(size);
                }
            }
            0x0D => self.cpu.r[0] = 0xBAAE_187F,
            // Affine parameters and bit unpacking are for drawing; nothing is drawn here.
            0x0E..=0x10 => {}
            0x11..=0x15 => {
                let (src, mut dst) = (r[0], r[1]);
                let mut b = self.board.clone();
                b.trace = None;
                let mut read = |a: u32| b.read(a, 1) as u8;
                let data = match n {
                    0x11 | 0x12 => decompress_lz77(&mut read, src),
                    0x13 => decompress_huff(&mut read, src),
                    _ => decompress_rl(&mut read, src),
                };
                if n == 0x11 || n == 0x14 {
                    for v in data {
                        self.board.write(dst, 1, v as u32);
                        dst = dst.wrapping_add(1);
                    }
                } else {
                    // Video memory takes halfwords.
                    for pair in data.chunks(2) {
                        let v = pair[0] as u32 | (*pair.get(1).unwrap_or(&0) as u32) << 8;
                        self.board.write(dst & !1, 2, v);
                        dst = dst.wrapping_add(2);
                    }
                }
            }
            0x16..=0x18 => {
                let (src, mut dst) = (r[0], r[1]);
                let size = (self.board.read(src & !3, 4) >> 8).min(UNPACK_MAX as u32);
                let step = if n == 0x18 { 2 } else { 1 };
                let mut acc = 0u32;
                let mut at = src.wrapping_add(4);
                let mut i = 0;
                while i < size {
                    acc = acc.wrapping_add(self.board.read(at, step));
                    if n == 0x16 {
                        self.board.write(dst, 1, acc & 0xFF);
                    } else {
                        self.board.write(dst & !1, step, acc);
                    }
                    at = at.wrapping_add(step);
                    dst = dst.wrapping_add(step);
                    i += step;
                }
            }
            0x19 => {
                let level = if r[0] != 0 { 0x200 } else { 0 };
                let bias = (self.board.io16(0x88) & !0x3FF) | level;
                self.board.io[0x88] = bias as u8;
                self.board.io[0x89] = (bias >> 8) as u8;
            }
            0x1F => {
                // MidiKey2Freq: the sample rate that plays key r1 (and r2/256) of a wave.
                let wave_rate = self.board.read(r[0].wrapping_add(4) & !3, 4) as f64;
                let key = r[1] as f64 + (r[2] & 0xFF) as f64 / 256.0;
                self.cpu.r[0] = (wave_rate / 2f64.powf((180.0 - key) / 12.0)) as u32;
            }
            _ => {
                self.unknown.insert(n);
            }
        }
        self.cpu.hle_exception_return();
    }

    /// One instruction, with the BIOS stand-in's traps. Returns the cycles taken.
    fn exec(&mut self) -> u32 {
        if !self.cpu.thumb() && self.cpu.r[15] == VECTOR_SWI {
            self.swi();
            return 10;
        }
        let t = self.cpu.step(&mut self.board);
        if self.board.halt {
            self.board.halt = false;
            self.cpu.halted = true;
        }
        t
    }

    /// One instruction of the running game, interrupts and all.
    fn step_once(&mut self) {
        if let Some((mask, ret)) = self.wait {
            if !self.cpu.halted && self.cpu.r[15] == ret && self.cpu.mode() != MODE_IRQ {
                let f = self.board.read(BIOS_IF, 2) as u16;
                if f & mask != 0 {
                    self.board.write(BIOS_IF, 2, (f & !mask) as u32);
                    self.wait = None;
                } else {
                    self.cpu.halted = true;
                }
            }
        }
        if let Some(tr) = self.board.trace.as_mut() {
            tr.clear();
        }
        let mut t = 0;
        if self.board.pending() {
            t += self.cpu.irq();
        }
        t += if self.cpu.halted {
            // Asleep until an interrupt: on to the next display event (or a while, for the
            // timers).
            self.board.to_next_event().min(256)
        } else {
            self.exec()
        };
        self.board.advance(t);
    }

    /// Runs the routine of `entry` once, from its own registers, until it returns, with no
    /// interrupts and no time passing for the board. False when it doesn't return within
    /// `budget` cycles, or waits for an interrupt.
    fn call(&mut self, entry: &Entry, set: Option<(u8, u8)>, budget: u32) -> bool {
        self.cpu = entry.cpu.clone();
        self.cpu.halted = false;
        let wait = self.wait.take();
        if let Some((reg, v)) = set {
            self.cpu.r[reg as usize & 3] = v as u32;
        }
        self.cpu.begin_call(entry.ret, entry.exception);
        let mut spent = 0;
        let mut ok = true;
        while !self.cpu.calls.is_empty() {
            if spent > budget || self.cpu.halted || self.wait.is_some() {
                ok = false;
                break;
            }
            let t = self.exec();
            spent += t;
            self.board.cycle += t;
        }
        self.wait = wait;
        ok
    }
}

/// The sound: the Game Boy channels and the two sample channels, mixed.
pub struct Chip {
    psg: Apu,
    soundcnt_h: u16,
}

impl Machine for GameBoyAdvance {
    type Entry = Entry;
    type Chip = Chip;
    const REGISTERS: u8 = 4;
    const CHANNELS: u16 = 2;

    fn boot(rom: &[u8]) -> Result<Self, String> {
        if rom.len() < 0xC0 || rom.len() > 0x0200_0000 {
            return Err("not a Game Boy Advance cartridge".into());
        }
        let mut bios = vec![0u8; 0x4000];
        for (at, op) in BIOS_CODE {
            bios[at as usize..at as usize + 4].copy_from_slice(&op.to_le_bytes());
        }
        let mut board = Board {
            rom: Arc::new(rom.to_vec()),
            bios: Arc::new(bios),
            ewram: vec![0; 0x40000],
            iwram: vec![0; 0x8000],
            sram: vec![0xFF; 0x10000],
            io: vec![0; 0x400],
            ie: 0,
            iflags: 0,
            timers: [Timer::default(); 4],
            dma: [Dma::default(); 4],
            fifo: [Vec::new(), Vec::new()],
            bufs: [None; 2],
            start: false,
            halt: false,
            cycle: 0,
            sound: Vec::new(),
            psg: [0; 0x30],
            stack: (u32::MAX, 0),
            trace: None,
        };
        // As the BIOS leaves things: the sound bias at its middle, POSTFLG set.
        board.io[0x89] = 0x02;
        board.io[0x300] = 1;
        // The BIOS's stacks, then the cartridge's start in system mode.
        let mut cpu = Cpu::reset();
        cpu.set_sp_of(MODE_SVC, 0x0300_7FE0);
        cpu.set_sp_of(MODE_IRQ, 0x0300_7FA0);
        cpu.set_mode(MODE_SYS);
        cpu.cpsr = MODE_SYS;
        cpu.r[13] = 0x0300_7F00;
        cpu.r[15] = 0x0800_0000;
        Ok(Self {
            cpu,
            board,
            wait: None,
            unknown: BTreeSet::new(),
        })
    }

    fn run_frame(&mut self, mut watch: Option<&mut dyn FnMut(&Step)>, stop_after: Option<u64>) -> bool {
        let mut n = 0u64;
        let mut sound: Vec<Write> = Vec::new();
        let mut ram: Vec<(u32, bool, u8)> = Vec::new();
        while self.board.cycle < FRAME {
            let before = self.cpu.calls.len();
            let sound_at = self.board.sound.len();
            self.step_once();
            n += 1;
            let calls = &self.cpu.calls;
            if calls.len() > before {
                let s = calls[calls.len() - 1].1;
                let (lo, hi) = self.board.stack;
                self.board.stack = (lo.min(s), hi.max(s.saturating_add(0x40)));
            }
            if let Some(w) = watch.as_deref_mut() {
                sound.clear();
                sound.extend_from_slice(&self.board.sound[sound_at.min(self.board.sound.len())..]);
                ram.clear();
                if let Some(t) = self.board.trace.as_ref() {
                    ram.extend_from_slice(t);
                }
                let calls = &self.cpu.calls;
                w(&Step {
                    calls,
                    entered: &calls[before.min(calls.len())..],
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
        let (ret, exception) = self.cpu.top_call().unwrap_or((u32::MAX, false));
        let mut cpu = self.cpu.clone();
        cpu.calls.clear();
        Entry {
            cpu,
            ret,
            exception,
            cycle: self.board.cycle,
        }
    }

    fn play_frame(&mut self, play: &Entry) -> Option<Vec<Write>> {
        let cycle = self.board.cycle;
        self.board.cycle = play.cycle;
        let ok = self.call(play, None, FRAME * 4);
        self.board.cycle = cycle;
        let writes = self.take_sound();
        ok.then_some(writes)
    }

    fn call_with(&mut self, entry: &Entry, reg: u8, v: u8) -> bool {
        let cycle = self.board.cycle;
        let cpu = self.cpu.clone();
        let from = self.board.sound.len();
        let ok = self.call(entry, Some((reg, v)), 32_000_000);
        self.board.cycle = cycle;
        self.cpu = cpu;
        for w in &mut self.board.sound[from..] {
            w.at = cycle;
        }
        ok
    }

    fn poke(&mut self, addr: u32, v: u8) {
        match addr >> 24 {
            2 => self.board.ewram[(addr & 0x3FFFF) as usize] = v,
            3 => self.board.iwram[(addr & 0x7FFF) as usize] = v,
            _ => {}
        }
    }

    fn incidental(w: &Write) -> bool {
        // The master switch and volume, and the mixer's, set once at boot.
        matches!(w.port, 0xFF24 | 0xFF26 | SOUNDCNT_H)
    }

    fn is_stack(&self, addr: u32) -> bool {
        let (lo, hi) = self.board.stack;
        // The BIOS's stacks and flags too, at the top of IWRAM.
        lo != u32::MAX && (lo.saturating_sub(0x80)..=hi).contains(&addr)
            || (0x0300_7E00..0x0300_8000).contains(&addr)
    }

    fn restore_writes(&self) -> Vec<Write> {
        let s = &self.board.psg;
        let at = |a: u16| s[(a - 0xFF10) as usize];
        let w = |a: u16, v: u8| Write {
            at: 0,
            port: a as u32,
            value: v as u16,
        };
        let mut out = vec![
            w(0xFF26, at(0xFF26)),
            w(0xFF24, at(0xFF24)),
            w(0xFF25, at(0xFF25)),
            w(0xFF1A, 0),
        ];
        out.extend((0xFF30..=0xFF3F).map(|a| w(a, at(a))));
        for a in 0xFF10..=0xFF23u16 {
            if matches!(a, 0xFF15 | 0xFF1F) {
                continue;
            }
            // Without the restart bit: the channels pick up where they are, not from new.
            let v = if matches!(a, 0xFF14 | 0xFF19 | 0xFF1E | 0xFF23) {
                at(a) & 0x7F
            } else {
                at(a)
            };
            out.push(w(a, v));
        }
        out.push(Write {
            at: 0,
            port: SOUNDCNT_H,
            value: self.board.soundcnt_h(),
        });
        out
    }

    fn new_chip(&self, rate: u32) -> Result<Chip, String> {
        Ok(Chip {
            psg: Apu::new(rate)?,
            soundcnt_h: self.board.soundcnt_h(),
        })
    }

    fn feed(&self, chip: &mut Chip, writes: &[Write], out: &mut Vec<i16>) {
        // Each sample channel's bytes this frame, in the order first written, last value.
        let mut order: [Vec<u32>; 2] = [Vec::new(), Vec::new()];
        let mut value: [HashMap<u32, u8>; 2] = [HashMap::new(), HashMap::new()];
        for w in writes {
            if w.port & SAMPLE != 0 {
                let chs = (w.port >> 24) & 3;
                let off = w.port & 0x00FF_FFFF;
                for ch in 0..2 {
                    if chs & (1 << ch) != 0 && value[ch].insert(off, w.value as u8).is_none() {
                        order[ch].push(off);
                    }
                }
            } else if w.port == SOUNDCNT_H {
                chip.soundcnt_h = w.value;
            } else if (0xFF10..=0xFF3F).contains(&w.port) {
                // The Game Boy channels count the Game Boy's clock, a quarter of this one.
                chip.psg.write(w.at.min(FRAME - 1) / 4, w.port as u16, w.value as u8);
            }
        }
        let at = out.len();
        chip.psg.end_frame(FRAME / 4, out);
        let frames = (out.len() - at) / 2;
        let h = chip.soundcnt_h;
        // The Game Boy channels at 25, 50 or 100%.
        let psg_level = [0.25, 0.5, 1.0, 1.0][(h & 3) as usize];
        if psg_level != 1.0 {
            for s in &mut out[at..] {
                *s = (*s as f64 * psg_level) as i16;
            }
        }
        for ch in 0..2 {
            if order[ch].is_empty() || frames == 0 {
                continue;
            }
            let bytes: Vec<f64> = order[ch].iter().map(|o| value[ch][o] as i8 as f64).collect();
            let level = if h & (1 << (2 + ch)) != 0 { 64.0 } else { 32.0 };
            let right = h & (1 << (8 + ch * 4)) != 0;
            let left = h & (1 << (9 + ch * 4)) != 0;
            for i in 0..frames {
                // From the frame's samples to the output's, linearly.
                let pos = i as f64 * bytes.len() as f64 / frames as f64;
                let k = pos as usize;
                let b = *bytes.get(k + 1).unwrap_or(&bytes[k]);
                let s = (bytes[k] + (b - bytes[k]) * (pos - k as f64)) * level;
                for (side, on) in [(0, left), (1, right)] {
                    if on {
                        let o = &mut out[at + i * 2 + side];
                        *o = (*o as f64 + s).clamp(-32768.0, 32767.0) as i16;
                    }
                }
            }
        }
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

    fn put(rom: &mut [u8], at: usize, code: &[u32]) {
        for (i, op) in code.iter().enumerate() {
            rom[at + i * 4..at + i * 4 + 4].copy_from_slice(&op.to_le_bytes());
        }
    }

    /// A tiny game (ARM code): the vertical blank interrupt calls a sound routine that plays
    /// song N (N written to 03000100 by the main program, 1 at boot) on the first square
    /// channel at a pitch per song, changing note every 8 frames through 16 notes, then
    /// round again. The main program waits in VBlankIntrWait.
    fn tiny_game() -> Vec<u8> {
        let mut rom = vec![0u8; 0x400];
        put(&mut rom, 0, &[0xEA00_002E]); // B 0xC0
        put(
            &mut rom,
            0xC0,
            &[
                0xE3A0_0301, // MOV R0, #0x04000000
                0xE28F_1F4D, // ADD R1, PC, #0x134 (the handler at 0x200)
                0xE500_1004, // STR R1, [R0, #-4]
                0xE3A0_2403, // MOV R2, #0x03000000
                0xE3A0_3001, // MOV R3, #1
                0xE5C2_3100, // STRB R3, [R2, #0x100]
                0xE3A0_3008, // MOV R3, #8
                0xE1C0_30B4, // STRH R3, [R0, #4] (vertical blank interrupt on)
                0xE280_4C02, // ADD R4, R0, #0x200
                0xE3A0_3001, // MOV R3, #1
                0xE1C4_30B0, // STRH R3, [R4] (IE)
                0xE1C4_30B8, // STRH R3, [R4, #8] (IME)
                0xE3A0_3080, // MOV R3, #0x80
                0xE1C0_38B4, // STRH R3, [R0, #0x84] (sound on)
                0xE3A0_3077, // MOV R3, #0x77
                0xE383_3CFF, // ORR R3, R3, #0xFF00
                0xE383_3802, // ORR R3, R3, #0x20000
                0xE580_3080, // STR R3, [R0, #0x80] (volumes, both sides, 100%)
                0xEF05_0000, // loop: SWI VBlankIntrWait
                0xEAFF_FFFD, // B loop
            ],
        );
        put(
            &mut rom,
            0x200,
            &[
                0xE92D_4010, // STMFD SP!, {R4, LR}
                0xE3A0_0301, // MOV R0, #0x04000000
                0xE280_1C02, // ADD R1, R0, #0x200
                0xE3A0_2001, // MOV R2, #1
                0xE1C1_20B2, // STRH R2, [R1, #2] (IF)
                0xE510_3008, // LDR R3, [R0, #-8]
                0xE383_3001, // ORR R3, R3, #1
                0xE500_3008, // STR R3, [R0, #-8] (the BIOS's flags)
                0xEB00_0036, // BL 0x300
                0xE8BD_4010, // LDMFD SP!, {R4, LR}
                0xE12F_FF1E, // BX LR
            ],
        );
        put(
            &mut rom,
            0x300,
            &[
                0xE3A0_2403, // MOV R2, #0x03000000
                0xE5D2_3100, // LDRB R3, [R2, #0x100] (a new song?)
                0xE353_0000, // CMP R3, #0
                0x15C2_3101, // STRNEB R3, [R2, #0x101] (the song playing)
                0x13A0_1000, // MOVNE R1, #0
                0x15C2_1100, // STRNEB R1, [R2, #0x100]
                0x15C2_1102, // STRNEB R1, [R2, #0x102] (the frame count)
                0xE5D2_3101, // LDRB R3, [R2, #0x101]
                0xE353_0000, // CMP R3, #0
                0x012F_FF1E, // BXEQ LR
                0xE5D2_1102, // LDRB R1, [R2, #0x102]
                0xE281_1001, // ADD R1, R1, #1
                0xE5C2_1102, // STRB R1, [R2, #0x102]
                0xE311_0007, // TST R1, #7
                0x112F_FF1E, // BXNE LR
                0xE1A0_11A1, // MOV R1, R1, LSR #3
                0xE201_100F, // AND R1, R1, #15
                0xE081_1203, // ADD R1, R1, R3, LSL #4
                0xE201_10FF, // AND R1, R1, #0xFF
                0xE281_1C06, // ADD R1, R1, #0x600
                0xE381_1902, // ORR R1, R1, #0x8000 (restart)
                0xE3A0_0301, // MOV R0, #0x04000000
                0xE3A0_3080, // MOV R3, #0x80
                0xE383_3A0F, // ORR R3, R3, #0xF000 (duty 50%, volume 15)
                0xE1C0_36B2, // STRH R3, [R0, #0x62]
                0xE1C0_16B4, // STRH R1, [R0, #0x64]
                0xE12F_FF1E, // BX LR
            ],
        );
        rom
    }

    #[test]
    fn the_tiny_game_runs() {
        let mut m = GameBoyAdvance::boot(&tiny_game()).unwrap();
        for _ in 0..20 {
            m.run_frame(None, None);
        }
        let w = m.take_sound();
        assert!(w.iter().any(|w| w.port == 0xFF26 && w.value == 0x80), "{w:?}");
        assert!(w.iter().any(|w| w.port == 0xFF14 && w.value & 0x80 != 0), "{w:?}");
        assert_eq!(m.board.iwram[0x101], 1);
        assert!(m.unknown_bios_calls().is_empty());
    }

    #[test]
    fn the_tiny_game_has_its_songs() {
        if !super::super::gb_apu::available() {
            return;
        }
        let rom = tiny_game();
        let found = search::<GameBoyAdvance>(&rom).unwrap();
        assert_eq!(found.why, None);
        assert_eq!(found.recipe.play, 0x0800_0300);
        assert_eq!(found.recipe.trigger, Trigger::Byte(0x0300_0100));
        assert_eq!(found.songs.len(), 16, "{:?}", found.songs);
        let t = measure::<GameBoyAdvance>(&rom, &found.recipe, 3).unwrap();
        assert_eq!(t.looping.map(|l| l.1), Some(128), "{t:?}");
    }

    #[test]
    fn timers_count_cascade_and_interrupt() {
        let mut m = GameBoyAdvance::boot(&tiny_game()).unwrap();
        let b = &mut m.board;
        b.ie = INT_TIMER0 << 1;
        b.io[0x208] = 1;
        // Timer 0: from FFF0, every cycle; timer 1 counts its overflows, from FFFE.
        b.write(0x0400_0100, 2, 0xFFF0);
        b.write(0x0400_0104, 2, 0xFFFE);
        b.write(0x0400_0106, 2, 0xC4);
        b.write(0x0400_0102, 2, 0x80);
        b.advance(16);
        assert_eq!(b.read(0x0400_0104, 2), 0xFFFF);
        assert!(!b.pending());
        b.advance(16);
        assert!(b.pending());
        b.write(0x0400_0202, 2, (INT_TIMER0 << 1) as u32);
        assert!(!b.pending());
        // Every 64 cycles.
        b.write(0x0400_0102, 2, 0x00);
        b.write(0x0400_0100, 2, 0x0000);
        b.write(0x0400_0102, 2, 0x81);
        b.advance(64 * 10 + 63);
        assert_eq!(b.read(0x0400_0100, 2), 10);
    }

    #[test]
    fn the_display_interrupts_and_its_dma() {
        let mut m = GameBoyAdvance::boot(&tiny_game()).unwrap();
        let b = &mut m.board;
        b.write(0x0400_0004, 2, 0x0818 | 0x20); // vertical, horizontal, line 8
        b.advance(LINE * 8);
        assert_eq!(b.iflags, INT_HBLANK | INT_VCOUNT);
        assert_eq!(b.read(0x0400_0006, 1), 8);
        assert_eq!(b.read(0x0400_0004, 1) & 7, 4);
        // A copy at each horizontal blank.
        b.write(0x0300_0000, 2, 0xABCD);
        b.write(0x0400_00D4, 4, 0x0300_0000);
        b.write(0x0400_00D8, 4, 0x0200_0000);
        b.write(0x0400_00DC, 4, 0xA300_0001); // repeat, halfwords, source fixed, at horizontal blank
        assert_eq!(b.read(0x0200_0000, 2), 0);
        b.advance(LINE);
        assert_eq!(b.read(0x0200_0000, 2), 0xABCD);
        assert_eq!(b.read(0x0200_0002, 2), 0);
        b.advance(LINE);
        assert_eq!(b.read(0x0200_0002, 2), 0xABCD);
        b.advance(LINE * (VBLANK_LINE - 10));
        assert_ne!(b.iflags & INT_VBLANK, 0);
        assert_eq!(b.read(0x0400_0004, 1) & 1, 1);
    }

    #[test]
    fn sample_dma_feeds_the_queue_and_marks_its_buffer() {
        let mut m = GameBoyAdvance::boot(&tiny_game()).unwrap();
        let b = &mut m.board;
        for i in 0..64 {
            b.ewram[i] = i as u8;
        }
        // Channel A on timer 0; DMA1 from 02000000 to its queue, repeating, on request.
        b.write(0x0400_0082, 2, 0x0300);
        b.write(0x0400_00BC, 4, 0x0200_0000);
        b.write(0x0400_00C0, 4, 0x0400_00A0);
        b.write(0x0400_00C6, 2, 0xB600);
        b.write(0x0400_0100, 2, 0xFF00);
        b.write(0x0400_0102, 2, 0x80);
        b.advance(256 * 3);
        assert_eq!(b.fifo[0].len(), 16 + 16 - 1 - 1);
        assert_eq!(b.bufs[0].map(|r| r.0), Some(0x0200_0000));
        assert_eq!(b.bufs[0].unwrap().1, 0x0200_0020);
        // A write inside the buffer is a sample; one outside isn't.
        b.sound.clear();
        b.write(0x0200_0004, 1, 0x7F);
        b.write(0x0200_0100, 1, 0x7F);
        assert_eq!(b.sound.len(), 1);
        assert_eq!(b.sound[0].port, SAMPLE | 1 << 24 | 4);
    }

    #[test]
    fn immediate_dma_copies() {
        let mut m = GameBoyAdvance::boot(&tiny_game()).unwrap();
        let b = &mut m.board;
        b.write(0x0300_0000, 4, 0x1234_5678);
        b.write(0x0400_00D4, 4, 0x0300_0000);
        b.write(0x0400_00D8, 4, 0x0200_0010);
        b.write(0x0400_00DC, 4, 0x8400_0001);
        assert_eq!(b.read(0x0200_0010, 4), 0x1234_5678);
        assert_eq!(b.dma[3].ctl & 0x8000, 0);
    }

    #[test]
    fn the_bios_calls() {
        let mut m = GameBoyAdvance::boot(&tiny_game()).unwrap();
        let call = |m: &mut GameBoyAdvance, n: u8, r: [u32; 3]| {
            // As the SWI left things: supervisor mode, LR after an ARM SWI in IWRAM.
            m.board.write(0x0300_0000, 4, 0xEF00_0000 | (n as u32) << 16);
            m.cpu.r[..3].copy_from_slice(&r);
            m.cpu.r[15] = 0x0300_0000;
            m.exec();
            assert_eq!(m.cpu.r[15], VECTOR_SWI);
            m.exec();
            assert_eq!(m.cpu.r[15], 0x0300_0004);
            [m.cpu.r[0], m.cpu.r[1], m.cpu.r[3]]
        };
        assert_eq!(call(&mut m, 6, [(-7i32) as u32, 2, 0]), [(-3i32) as u32, (-1i32) as u32, 3]);
        assert_eq!(call(&mut m, 8, [144, 0, 0])[0], 12);
        // CpuSet: fill 4 halfwords.
        m.board.write(0x0200_0100, 2, 0x5A5A);
        call(&mut m, 0x0B, [0x0200_0100, 0x0200_0200, 4 | 1 << 24]);
        assert_eq!(m.board.read(0x0200_0206, 2), 0x5A5A);
        call(&mut m, 0x2A, [0, 0, 0]);
        assert_eq!(m.unknown_bios_calls(), vec![0x2A]);
    }

    #[test]
    fn lz77_and_rl_unpack() {
        // 8 bytes: "AB", then 6 copied from 2 back.
        let data = [0x10u8, 8, 0, 0, 0x20, b'A', b'B', 0x30, 0x01];
        let mut read = |a: u32| *data.get(a as usize).unwrap_or(&0);
        assert_eq!(decompress_lz77(&mut read, 0), b"ABABABAB");
        // 6 bytes: "xy", then 4 'z'.
        let data = [0x30u8, 6, 0, 0, 0x01, b'x', b'y', 0x81, b'z'];
        let mut read = |a: u32| *data.get(a as usize).unwrap_or(&0);
        assert_eq!(decompress_rl(&mut read, 0), b"xyzzzz");
    }
}
