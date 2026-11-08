//! The Game Boy and Game Boy Color as a [`Machine`] for the music search
//! ([`super::music_search`]): the SM83 ([`super::sm83`]), the cartridge's bank controller,
//! the timer, and just enough of the LCD for the timing games wait on (LY, STAT, vertical
//! blank). Nothing is drawn. No boot ROM: the CPU starts as the boot ROM leaves it. The sound
//! goes to libgme's chip ([`super::gb_apu`]).

use std::sync::Arc;

use super::gb_apu::Apu;
use super::music_search::{Machine, Step, Write};
use super::sm83::{Bus, Cpu};

/// Clocks (4,194,304 a second) per frame and per scanline.
const FRAME: u32 = 70_224;
const LINE: u32 = 456;
pub const RATE: f64 = 4_194_304.0 / FRAME as f64;

/// The cartridge's bank controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    None,
    Mbc1,
    Mbc2,
    Mbc3,
    Mbc5,
}

/// The bank controller's registers.
#[derive(Clone, Debug)]
struct Mbc {
    kind: Kind,
    /// Low bank bits as written (MBC1: 5 bits; MBC5: 9 bits).
    rom_lo: u16,
    /// MBC1's upper bits / RAM bank; MBC3/MBC5's RAM bank.
    hi: u8,
    ram_on: bool,
    /// MBC1's banking mode.
    mode: u8,
}

impl Mbc {
    fn rom_bank(&self) -> usize {
        match self.kind {
            Kind::None => 1,
            Kind::Mbc1 => {
                let lo = (self.rom_lo & 0x1F).max(1);
                ((self.hi as usize & 3) << 5) | lo as usize
            }
            Kind::Mbc2 => (self.rom_lo & 0x0F).max(1) as usize,
            Kind::Mbc3 => (self.rom_lo & 0xFF).max(1) as usize,
            Kind::Mbc5 => (self.rom_lo & 0x1FF) as usize,
        }
    }

    fn rom0_bank(&self) -> usize {
        if self.kind == Kind::Mbc1 && self.mode == 1 {
            (self.hi as usize & 3) << 5
        } else {
            0
        }
    }

    /// The RAM bank, or None for MBC3's clock registers.
    fn ram_bank(&self) -> Option<usize> {
        match self.kind {
            Kind::Mbc1 => Some(if self.mode == 1 {
                self.hi as usize & 3
            } else {
                0
            }),
            Kind::Mbc3 => (self.hi < 8).then_some(self.hi as usize & 7),
            Kind::Mbc5 => Some(self.hi as usize & 0x0F),
            _ => Some(0),
        }
    }

    fn write(&mut self, addr: u16, v: u8) {
        match (self.kind, addr) {
            (Kind::None, _) => {}
            (Kind::Mbc2, 0x0000..=0x3FFF) => {
                if addr & 0x100 != 0 {
                    self.rom_lo = (v & 0x0F) as u16;
                } else {
                    self.ram_on = v & 0x0F == 0x0A;
                }
            }
            (Kind::Mbc2, _) => {}
            (_, 0x0000..=0x1FFF) => self.ram_on = v & 0x0F == 0x0A,
            (Kind::Mbc5, 0x2000..=0x2FFF) => self.rom_lo = (self.rom_lo & 0x100) | v as u16,
            (Kind::Mbc5, 0x3000..=0x3FFF) => {
                self.rom_lo = (self.rom_lo & 0xFF) | ((v as u16 & 1) << 8)
            }
            (Kind::Mbc1, 0x2000..=0x3FFF) => self.rom_lo = (v & 0x1F) as u16,
            (_, 0x2000..=0x3FFF) => self.rom_lo = v as u16,
            (_, 0x4000..=0x5FFF) => self.hi = v,
            (Kind::Mbc1, 0x6000..=0x7FFF) => self.mode = v & 1,
            _ => {}
        }
    }
}

/// The cartridge header's bank controller and RAM size.
fn cart_type(rom: &[u8]) -> Result<(Kind, usize), String> {
    if rom.len() < 0x150 {
        return Err("not a Game Boy ROM (too small)".into());
    }
    let kind = match rom[0x147] {
        0x00 | 0x08 | 0x09 => Kind::None,
        0x01..=0x03 | 0xFF => Kind::Mbc1,
        0x05 | 0x06 => Kind::Mbc2,
        0x0F..=0x13 | 0xFE => Kind::Mbc3,
        0x19..=0x1E | 0x22 | 0xFC => Kind::Mbc5,
        t => {
            return Err(format!(
                "this Game Boy cartridge type (${t:02X}) isn't supported yet"
            ))
        }
    };
    let ram = match (kind, rom[0x149]) {
        (Kind::Mbc2, _) => 0x200,
        (_, 3) => 0x8000,
        (_, 4) => 0x20000,
        (_, 5) => 0x10000,
        _ => 0x2000,
    };
    Ok((kind, ram))
}

/// Everything around the CPU. Plain data: cloned to run again from any moment.
#[derive(Clone)]
struct Board {
    rom: Arc<Vec<u8>>,
    mbc: Mbc,
    cram: Vec<u8>,
    wram: Vec<u8>,
    svbk: u8,
    vram: Vec<u8>,
    vbk: u8,
    oam: Vec<u8>,
    hram: Vec<u8>,
    io: Vec<u8>,
    cgb: bool,
    double: bool,
    key1: u8,
    iff: u8,
    ie: u8,
    /// The divider's counter: 4 a machine cycle.
    sys: u64,
    tima: u8,
    tma: u8,
    tac: u8,
    /// Clocks into the frame.
    cycle: u32,
    joyp: u8,
    buttons: u8,
    sound: Vec<Write>,
    /// The last value written to each sound register: what a freshly started chip is given
    /// before a song plays from the middle of the game.
    shadow: [u8; 0x30],
    /// The stack's range, from where calls were made.
    stack: (u16, u16),
    /// RAM accesses, when traced: (address, written, value).
    trace: Option<Vec<(u16, bool, u8)>>,
}

impl Board {
    fn lcd_on(&self) -> bool {
        self.io[0x40] & 0x80 != 0
    }

    fn ly(&self) -> u8 {
        if self.lcd_on() {
            (self.cycle / LINE).min(153) as u8
        } else {
            0
        }
    }

    fn mode(&self) -> u8 {
        if !self.lcd_on() {
            return 0;
        }
        let (line, dot) = (self.cycle / LINE, self.cycle % LINE);
        match (line, dot) {
            (144.., _) => 1,
            (_, 0..=79) => 2,
            (_, 80..=251) => 3,
            _ => 0,
        }
    }

    fn stat_irq(&mut self) {
        self.iff |= 2;
    }

    fn new_line(&mut self, line: u32) {
        let stat = self.io[0x41];
        if line as u8 == self.io[0x45] && stat & 0x40 != 0 {
            self.stat_irq();
        }
        if line == 144 {
            self.iff |= 1;
            if stat & 0x10 != 0 {
                self.stat_irq();
            }
        } else if line < 144 && stat & 0x20 != 0 {
            self.stat_irq();
        }
    }

    /// Runs the clocks and the timer on by `m` machine cycles.
    fn advance(&mut self, m: u32) {
        let old = self.cycle;
        let new = old + m * if self.double { 2 } else { 4 };
        self.cycle = new;
        if self.lcd_on() {
            let (lo, ln) = (old / LINE, new / LINE);
            for line in lo..ln {
                if line + 1 < 154 {
                    self.new_line(line + 1);
                }
            }
            if self.io[0x41] & 0x08 != 0 {
                for line in lo..=ln {
                    let p = line * LINE + 252;
                    if line < 144 && old < p && p <= new {
                        self.stat_irq();
                    }
                }
            }
        }
        let before = self.sys;
        self.sys += 4 * m as u64;
        if self.tac & 4 != 0 {
            let shift = [10, 4, 6, 8][(self.tac & 3) as usize];
            let ticks = (self.sys >> shift) - (before >> shift);
            for _ in 0..ticks {
                self.tima = self.tima.wrapping_add(1);
                if self.tima == 0 {
                    self.tima = self.tma;
                    self.iff |= 4;
                }
            }
        }
    }

    fn rom_at(&self, bank: usize, offset: u16) -> u8 {
        let banks = self.rom.len() / 0x4000;
        self.rom[(bank % banks) * 0x4000 + offset as usize]
    }

    fn cram_index(&self, addr: u16) -> Option<usize> {
        let bank = self.mbc.ram_bank()?;
        let n = self.cram.len();
        Some((bank * 0x2000 + (addr as usize - 0xA000)) % n)
    }

    fn wram_index(&self, addr: u16) -> usize {
        let a = addr as usize & 0x1FFF;
        if a < 0x1000 {
            a
        } else {
            let bank = if self.cgb { (self.svbk & 7).max(1) } else { 1 } as usize;
            bank * 0x1000 + (a - 0x1000)
        }
    }

    /// A byte of RAM, read without side effects.
    fn peek(&self, addr: u16) -> u8 {
        match addr {
            0xA000..=0xBFFF => self.cram_index(addr).map_or(0xFF, |i| self.cram[i]),
            0xC000..=0xFDFF => self.wram[self.wram_index(addr)],
            0xFF80..=0xFFFE => self.hram[(addr - 0xFF80) as usize],
            _ => 0xFF,
        }
    }

    fn traced(&mut self, addr: u16, write: bool, v: u8) {
        if let Some(t) = self.trace.as_mut() {
            t.push((addr, write, v));
        }
    }

    fn sound_write(&mut self, addr: u16, v: u8) {
        self.sound.push(Write {
            at: self.cycle,
            port: addr as u32,
            value: v as u16,
        });
        let i = (addr - 0xFF10) as usize;
        self.shadow[i] = v;
        // Powering the chip off clears its registers.
        if addr == 0xFF26 && v & 0x80 == 0 {
            self.shadow[..0x16].fill(0);
        }
    }

    fn io_read(&mut self, addr: u16) -> u8 {
        let r = (addr & 0x7F) as usize;
        match addr {
            0xFF00 => {
                let sel = self.joyp & 0x30;
                let mut low = 0x0F;
                if sel & 0x20 == 0 {
                    low &= !(self.buttons & 0x0F);
                }
                0xC0 | sel | low
            }
            0xFF04 => (self.sys >> 8) as u8,
            0xFF05 => self.tima,
            0xFF06 => self.tma,
            0xFF07 => 0xF8 | self.tac,
            0xFF0F => 0xE0 | self.iff,
            0xFF10..=0xFF3F => {
                const OR: [u8; 0x30] = [
                    0x80, 0x3F, 0x00, 0xFF, 0xBF, 0xFF, 0x3F, 0x00, 0xFF, 0xBF, 0x7F, 0xFF, 0x9F,
                    0xFF, 0xBF, 0xFF, 0xFF, 0x00, 0x00, 0xBF, 0x00, 0x00, 0x70, 0xFF, 0xFF, 0xFF,
                    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0,
                ];
                let i = (addr - 0xFF10) as usize;
                // The channels read as finished: a driver waiting for one to end goes on.
                let v = if addr == 0xFF26 {
                    self.shadow[i] & 0x80
                } else {
                    self.shadow[i]
                };
                v | OR[i]
            }
            0xFF41 => {
                let coincide = (self.ly() == self.io[0x45]) as u8;
                0x80 | (self.io[0x41] & 0x78) | coincide << 2 | self.mode()
            }
            0xFF44 => self.ly(),
            0xFF4D => 0x7E | (self.double as u8) << 7 | (self.key1 & 1),
            0xFF4F => 0xFE | self.vbk,
            0xFF55 => 0xFF,
            0xFF70 => 0xF8 | self.svbk,
            _ => self.io[r],
        }
    }

    fn io_write(&mut self, addr: u16, v: u8) {
        let r = (addr & 0x7F) as usize;
        match addr {
            0xFF00 => self.joyp = v & 0x30,
            0xFF04 => self.sys = 0,
            0xFF05 => self.tima = v,
            0xFF06 => self.tma = v,
            0xFF07 => self.tac = v & 7,
            0xFF0F => self.iff = v & 0x1F,
            0xFF10..=0xFF3F => self.sound_write(addr, v),
            0xFF46 => {
                let src = (v as u16) << 8;
                for i in 0..0xA0u16 {
                    let b = self.read(src + i);
                    self.oam[i as usize] = b;
                }
            }
            0xFF4D => self.key1 = v & 1,
            0xFF4F => self.vbk = v & 1,
            0xFF55 if self.cgb => {
                let src = u16::from_be_bytes([self.io[0x51], self.io[0x52]]) & 0xFFF0;
                let dst = 0x8000 | (u16::from_be_bytes([self.io[0x53], self.io[0x54]]) & 0x1FF0);
                let len = ((v as u16 & 0x7F) + 1) * 16;
                for i in 0..len {
                    let b = self.read(src.wrapping_add(i));
                    self.write(0x8000 | ((dst + i) & 0x1FFF), b);
                }
            }
            0xFF70 => self.svbk = v & 7,
            _ => self.io[r] = v,
        }
    }
}

impl Bus for Board {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x3FFF => self.rom_at(self.mbc.rom0_bank(), addr),
            0x4000..=0x7FFF => self.rom_at(self.mbc.rom_bank(), addr - 0x4000),
            0x8000..=0x9FFF => self.vram[self.vbk as usize * 0x2000 + (addr as usize - 0x8000)],
            0xA000..=0xBFFF => {
                if !self.mbc.ram_on {
                    return 0xFF;
                }
                let v = match self.cram_index(addr) {
                    Some(i) if self.mbc.kind == Kind::Mbc2 => 0xF0 | self.cram[i & 0x1FF],
                    Some(i) => self.cram[i],
                    None => 0,
                };
                self.traced(addr, false, v);
                v
            }
            0xC000..=0xFDFF => {
                let v = self.wram[self.wram_index(addr)];
                self.traced(0xC000 | (addr & 0x1FFF), false, v);
                v
            }
            0xFE00..=0xFE9F => self.oam[(addr - 0xFE00) as usize],
            0xFEA0..=0xFEFF => 0,
            0xFF00..=0xFF7F => self.io_read(addr),
            0xFF80..=0xFFFE => {
                let v = self.hram[(addr - 0xFF80) as usize];
                self.traced(addr, false, v);
                v
            }
            0xFFFF => self.ie,
        }
    }

    fn write(&mut self, addr: u16, v: u8) {
        match addr {
            0x0000..=0x7FFF => self.mbc.write(addr, v),
            0x8000..=0x9FFF => self.vram[self.vbk as usize * 0x2000 + (addr as usize - 0x8000)] = v,
            0xA000..=0xBFFF => {
                if self.mbc.ram_on {
                    if let Some(i) = self.cram_index(addr) {
                        let i = if self.mbc.kind == Kind::Mbc2 {
                            i & 0x1FF
                        } else {
                            i
                        };
                        self.cram[i] = v;
                        self.traced(addr, true, v);
                    }
                }
            }
            0xC000..=0xFDFF => {
                let i = self.wram_index(addr);
                self.wram[i] = v;
                self.traced(0xC000 | (addr & 0x1FFF), true, v);
            }
            0xFE00..=0xFE9F => self.oam[(addr - 0xFE00) as usize] = v,
            0xFEA0..=0xFEFF => {}
            0xFF00..=0xFF7F => self.io_write(addr, v),
            0xFF80..=0xFFFE => {
                self.hram[(addr - 0xFF80) as usize] = v;
                self.traced(addr, true, v);
            }
            0xFFFF => self.ie = v,
        }
    }
}

/// A moment the CPU entered a routine: enough to call it again from there.
#[derive(Clone)]
pub struct Entry {
    cpu: Cpu,
    mbc: Mbc,
    svbk: u8,
    /// The return address the call pushed.
    ret: [u8; 2],
    /// Clock into the frame.
    cycle: u32,
}

/// A Game Boy (or Game Boy Color): CPU and board.
#[derive(Clone)]
pub struct GameBoy {
    cpu: Cpu,
    board: Board,
}

impl GameBoy {
    fn step_once(&mut self) -> u32 {
        let mut m = 0;
        let pending = self.board.ie & self.board.iff & 0x1F;
        if pending != 0 {
            m += self
                .cpu
                .interrupt(&mut self.board, pending.trailing_zeros() as u8);
        }
        if let Some(t) = self.board.trace.as_mut() {
            t.clear();
        }
        if self.cpu.halted {
            // Nothing happens until an interrupt: skip ahead a little at a time.
            m += 8;
        } else {
            m += self.cpu.step(&mut self.board);
        }
        if self.cpu.stopped {
            // STOP switches speed on the Game Boy Color when asked to; otherwise it waits for
            // a button, which nothing here presses: go on.
            if self.board.cgb && self.board.key1 & 1 != 0 {
                self.board.double = !self.board.double;
                self.board.key1 = 0;
            }
            self.cpu.stopped = false;
        }
        self.board.advance(m);
        m
    }

    /// Runs the routine of `entry` once, from its own registers and banks, until it returns.
    /// False when it doesn't within `budget` clocks.
    fn call(&mut self, entry: &Entry, set: Option<(u8, u8)>, budget: u32) -> bool {
        self.cpu = entry.cpu.clone();
        if let Some((reg, v)) = set {
            match reg {
                0 => self.cpu.a = v,
                1 => self.cpu.b = v,
                2 => self.cpu.c = v,
                3 => self.cpu.d = v,
                4 => self.cpu.e = v,
                5 => self.cpu.h = v,
                _ => self.cpu.l = v,
            }
        }
        self.board.mbc = entry.mbc.clone();
        self.board.svbk = entry.svbk;
        let sp = self.cpu.sp;
        self.board.write(sp, entry.ret[0]);
        self.board.write(sp.wrapping_add(1), entry.ret[1]);
        self.cpu.calls = vec![(self.cpu.pc, sp.wrapping_add(2))];
        self.cpu.halted = false;
        let start = self.board.cycle;
        while !self.cpu.calls.is_empty() {
            if self.board.cycle - start > budget || self.cpu.halted {
                return false;
            }
            let m = self.cpu.step(&mut self.board);
            self.board.advance(m);
        }
        true
    }
}

/// The Game Boy's sound chip.
pub struct Chip(Apu);

impl Machine for GameBoy {
    type Entry = Entry;
    type Chip = Chip;
    const REGISTERS: u8 = 7;
    const CHANNELS: u16 = 2;

    fn boot(rom: &[u8]) -> Result<Self, String> {
        let (kind, ram) = cart_type(rom)?;
        let cgb = rom[0x143] & 0x80 != 0;
        let mut io = vec![0u8; 0x80];
        io[0x40] = 0x91;
        io[0x47] = 0xFC;
        let mut board = Board {
            rom: Arc::new({
                // Whole banks, at least two.
                let mut r = rom.to_vec();
                r.resize(r.len().div_ceil(0x4000).max(2) * 0x4000, 0xFF);
                r
            }),
            mbc: Mbc {
                kind,
                rom_lo: 1,
                hi: 0,
                ram_on: false,
                mode: 0,
            },
            cram: vec![0; ram],
            wram: vec![0; 0x8000],
            svbk: 1,
            vram: vec![0; 0x4000],
            vbk: 0,
            oam: vec![0; 0xA0],
            hram: vec![0; 0x7F],
            io,
            cgb,
            double: false,
            key1: 0,
            iff: 1,
            ie: 0,
            sys: 0xABCC,
            tima: 0,
            tma: 0,
            tac: 0,
            cycle: 0,
            joyp: 0x30,
            buttons: 0,
            sound: Vec::new(),
            shadow: [0; 0x30],
            stack: (0xFFFE, 0xFFFE),
            trace: None,
        };
        // The sound registers as the boot ROM leaves them (the start-up chime has played).
        for (a, v) in [(0xFF26u16, 0xF1u8), (0xFF24, 0x77), (0xFF25, 0xF3)] {
            board.shadow[(a - 0xFF10) as usize] = v;
        }
        Ok(Self {
            cpu: Cpu::after_boot(cgb),
            board,
        })
    }

    fn run_frame(
        &mut self,
        mut watch: Option<&mut dyn FnMut(&Step)>,
        stop_after: Option<u64>,
    ) -> bool {
        if self.board.lcd_on() && self.board.cycle < LINE {
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
            let new_calls = self.cpu.calls.len() > before;
            if new_calls {
                let sp = self.cpu.sp;
                let (lo, hi) = self.board.stack;
                self.board.stack = (lo.min(sp), hi.max(sp.wrapping_add(1)));
            }
            if let Some(w) = watch.as_deref_mut() {
                calls.clear();
                calls.extend(self.cpu.calls.iter().map(|&(r, s)| (r as u32, s as u32)));
                sound.clear();
                sound.extend_from_slice(&self.board.sound[sound_at..]);
                ram.clear();
                if let Some(t) = self.board.trace.as_ref() {
                    ram.extend(t.iter().map(|&(a, wr, v)| (a as u32, wr, v)));
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
        self.board.buttons = if held { 0x08 } else { 0 };
    }

    fn press(&mut self, buttons: u8) {
        // A, B, Select, Start: the button group's own bits.
        self.board.buttons = buttons & 0x0F;
    }

    fn entry(&self) -> Entry {
        let mut cpu = self.cpu.clone();
        cpu.calls.clear();
        let sp = self.cpu.sp;
        let ret = [self.board.peek(sp), self.board.peek(sp.wrapping_add(1))];
        Entry {
            cpu,
            mbc: self.board.mbc.clone(),
            svbk: self.board.svbk,
            ret,
            cycle: self.board.cycle,
        }
    }

    fn play_frame(&mut self, play: &Entry) -> Option<Vec<Write>> {
        self.board.cycle = play.cycle;
        let ok = self.call(play, None, FRAME * 4);
        if self.board.cycle < FRAME {
            let per = if self.board.double { 2 } else { 4 };
            self.board.advance((FRAME - self.board.cycle) / per);
        }
        self.board.cycle = 0;
        let writes = self.take_sound();
        ok.then_some(writes)
    }

    fn call_with(&mut self, entry: &Entry, reg: u8, v: u8) -> bool {
        let cycle = self.board.cycle;
        let mbc = self.board.mbc.clone();
        let svbk = self.board.svbk;
        let cpu = self.cpu.clone();
        let ok = self.call(entry, Some((reg, v)), 800_000);
        self.board.cycle = cycle;
        self.board.mbc = mbc;
        self.board.svbk = svbk;
        self.cpu = cpu;
        for w in self.board.sound.iter_mut() {
            w.at = cycle;
        }
        ok
    }

    fn call_at(&mut self, ctx: &Entry, addr: u32, reg: u8, v: u8) -> bool {
        let mut e = ctx.clone();
        e.cpu.pc = addr as u16;
        self.call_with(&e, reg, v)
    }

    fn code_at(&mut self, entry: &Entry, addr: u32, len: usize) -> Vec<u8> {
        let b = &self.board;
        (0..len as u32)
            .map(|i| {
                let a = (addr + i) as u16;
                match a {
                    0x0000..=0x3FFF => b.rom_at(entry.mbc.rom0_bank(), a),
                    0x4000..=0x7FFF => b.rom_at(entry.mbc.rom_bank(), a - 0x4000),
                    _ => b.peek(a),
                }
            })
            .collect()
    }

    fn poke(&mut self, addr: u32, v: u8) {
        let addr = addr as u16;
        match addr {
            0xA000..=0xBFFF => {
                if let Some(i) = self.board.cram_index(addr) {
                    self.board.cram[i] = v;
                }
            }
            0xC000..=0xFDFF => {
                let i = self.board.wram_index(addr);
                self.board.wram[i] = v;
            }
            0xFF80..=0xFFFE => self.board.hram[(addr - 0xFF80) as usize] = v,
            _ => {}
        }
    }

    fn incidental(w: &Write) -> bool {
        matches!(w.port, 0xFF24 | 0xFF26)
    }

    fn is_stack(&self, addr: u32) -> bool {
        let (lo, hi) = self.board.stack;
        (lo.saturating_sub(0x20) as u32..=hi as u32).contains(&addr)
    }

    fn restore_writes(&self) -> Vec<Write> {
        let s = &self.board.shadow;
        let w = |a: u16, v: u8| Write {
            at: 0,
            port: a as u32,
            value: v as u16,
        };
        let at = |a: u16| s[(a - 0xFF10) as usize];
        let mut out = vec![
            w(0xFF26, at(0xFF26)),
            w(0xFF24, at(0xFF24)),
            w(0xFF25, at(0xFF25)),
            // The wave channel off while its wave is loaded.
            w(0xFF1A, 0),
        ];
        out.extend((0xFF30..=0xFF3F).map(|a| w(a, at(a))));
        for a in 0xFF10..=0xFF23u16 {
            if matches!(a, 0xFF15 | 0xFF1F) {
                continue;
            }
            // The note-start bit is left off: the song starts its own notes.
            let v = if matches!(a, 0xFF14 | 0xFF19 | 0xFF1E | 0xFF23) {
                at(a) & 0x7F
            } else {
                at(a)
            };
            out.push(w(a, v));
        }
        out
    }

    fn new_chip(&self, rate: u32) -> Result<Chip, String> {
        Ok(Chip(Apu::new(rate)?))
    }

    fn feed(&self, chip: &mut Chip, writes: &[Write], out: &mut Vec<i16>) {
        for w in writes {
            chip.0
                .write(w.at.min(FRAME - 1), w.port as u16, w.value as u8);
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

    /// A tiny game: VBlank calls a sound routine that plays song N (N written to $C100 by the
    /// main program, or 1 at boot) as a square wave at a pitch per song, changing note every 8
    /// frames through 16 notes, then round again.
    fn tiny_game() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        // Entry: JP $0150.
        rom[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]);
        // VBlank vector: CALL $0200; RETI.
        rom[0x40..0x44].copy_from_slice(&[0xCD, 0x00, 0x02, 0xD9]);
        // Main: sound on, request song 1, VBlank on, EI, HALT loop.
        let main = [
            0x31, 0xFE, 0xFF, // LD SP,$FFFE
            0x3E, 0x80, 0xE0, 0x26, // LD A,$80; LDH ($26),A
            0x3E, 0x77, 0xE0, 0x24, // NR50
            0x3E, 0xFF, 0xE0, 0x25, // NR51
            0x3E, 0x01, 0xEA, 0x00, 0xC1, // LD A,1; LD ($C100),A
            0x3E, 0x01, 0xE0, 0xFF, // IE = VBlank
            0xFB, // EI
            0x76, 0x00, 0x18, 0xFC, // HALT; NOP; JR -4
        ];
        rom[0x150..0x150 + main.len()].copy_from_slice(&main);
        // Sound routine at $0200:
        //   LD A,($C100); OR A; JR Z,tick; LD ($C101),A; XOR A; LD ($C100),A; LD ($C102),A
        // tick: LD A,($C101); OR A; RET Z; LD HL,$C102; INC (HL); LD A,(HL); AND 7; RET NZ
        //   LD A,$F0; LDH ($12),A; LD A,$80; LDH ($11),A
        //   LD A,(HL); RRCA; RRCA; RRCA; AND $0F; LD B,A; LD A,($C101); ADD B; ADD A; ADD A;
        //   ADD A; LDH ($13),A; LD A,$86; LDH ($14),A; RET
        let sound = [
            0xFA, 0x00, 0xC1, 0xB7, 0x28, 0x0A, 0xEA, 0x01, 0xC1, 0xAF, 0xEA, 0x00, 0xC1, 0xEA,
            0x02, 0xC1, 0xFA, 0x01, 0xC1, 0xB7, 0xC8, 0x21, 0x02, 0xC1, 0x34, 0x7E, 0xE6, 0x07,
            0xC0, 0x3E, 0xF0, 0xE0, 0x12, 0x3E, 0x80, 0xE0, 0x11, 0x7E, 0x0F, 0x0F, 0x0F, 0xE6,
            0x0F, 0x47, 0xFA, 0x01, 0xC1, 0x80, 0x87, 0x87, 0x87, 0xE0, 0x13, 0x3E, 0x86, 0xE0,
            0x14, 0xC9,
        ];
        rom[0x200..0x200 + sound.len()].copy_from_slice(&sound);
        rom
    }

    #[test]
    fn the_tiny_game_has_its_songs() {
        if !super::super::gb_apu::available() {
            return;
        }
        let rom = tiny_game();
        let found = search::<GameBoy>(&rom).unwrap();
        assert_eq!(found.why, None);
        assert_eq!(found.recipe.play, 0x0200);
        assert_eq!(found.recipe.trigger, Trigger::Byte(0xC100));
        assert!(
            (30..=34).contains(&found.songs.len()),
            "{} songs",
            found.songs.len()
        );
        let t = measure::<GameBoy>(&rom, &found.recipe, 3).unwrap();
        assert_eq!(t.looping.map(|l| l.1), Some(128), "{t:?}");
    }

    #[test]
    fn banks_switch() {
        let mut rom = vec![0u8; 0x4000 * 8];
        for b in 0..8 {
            rom[b * 0x4000 + 1] = b as u8;
        }
        rom[0x147] = 0x19;
        let mut gb = GameBoy::boot(&rom).unwrap();
        assert_eq!(gb.board.read(0x4001), 1);
        gb.board.write(0x2000, 5);
        assert_eq!(gb.board.read(0x4001), 5);
        gb.board.write(0x2000, 0);
        assert_eq!(gb.board.read(0x4001), 0);
        rom[0x147] = 0x01;
        let mut gb = GameBoy::boot(&rom).unwrap();
        gb.board.write(0x2000, 0);
        assert_eq!(gb.board.read(0x4001), 1);
        gb.board.write(0x2000, 7);
        assert_eq!(gb.board.read(0x4001), 7);
    }

    #[test]
    fn the_timer_interrupts() {
        let rom = {
            let mut r = vec![0u8; 0x8000];
            r[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]);
            r[0x150..0x152].copy_from_slice(&[0x18, 0xFE]);
            r
        };
        let mut gb = GameBoy::boot(&rom).unwrap();
        gb.board.write(0xFF07, 0x05); // 262,144 Hz
        gb.board.iff = 0;
        gb.board.advance(256); // 1,024 clocks: 64 ticks of 16
        assert_eq!(gb.board.tima, 64);
        gb.board.write(0xFF05, 0xFF);
        gb.board.advance(4);
        assert_eq!(gb.board.iff & 4, 4);
    }
}
