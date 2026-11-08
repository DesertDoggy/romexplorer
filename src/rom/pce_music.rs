//! The PC Engine as a [`Machine`] for the music search ([`super::music_search`]): the HuC6280
//! ([`super::huc6280`]), HuCard banks (Street Fighter II's mapper included), the interrupt
//! controller, the CPU's timer, the joypad, and just enough of the video chip for the timing
//! games wait on (the vertical blank and raster interrupts, its DMA flags). Nothing is
//! drawn. No BIOS (HuCards need none). The sound unit goes to libvgm's chip
//! ([`super::vgm_chips`]).

use std::sync::Arc;

use super::huc6280::{Bus, Cpu, VECTOR_IRQ1, VECTOR_TIMER};
use super::music_search::{Machine, Step, Write};
use super::vgm_chips::{Chips, Kind};

/// Master clocks a second, per scanline, and scanlines per frame. The CPU runs at a third of
/// the master clock (CSH) or a twelfth (CSL).
const MASTER: u32 = 21_477_270;
const LINE: u32 = 1365;
const LINES: u32 = 262;
const FRAME: u32 = LINE * LINES;
pub const RATE: f64 = MASTER as f64 / FRAME as f64;
/// The sound unit's clock.
const PSG_CLOCK: u32 = 3_579_545;
/// The timer counts down once every 1024 cycles at 7.16 MHz.
const TIMER_TICK: u32 = 1024 * 3;
/// The scanline the vertical blank starts on, and the first displayed one.
const VBLANK_LINE: u32 = 240;
const FIRST_LINE: u32 = 14;

/// The video chip's status bits.
const VD: u8 = 0x20;
const DV: u8 = 0x10;
const DS: u8 = 0x08;
const RR: u8 = 0x04;

/// RAM as the search sees it: logical $2000-$3FFF (bank $F8 behind MPR1, as every game maps
/// it), so zero page is $2000 and the stack $2100.
const RAM_BASE: u32 = 0x2000;

/// US cards are wired with their data lines reversed; their dumps usually are too. A real
/// reset vector points into the top bank ($E000 and up).
fn unscramble(rom: &mut [u8]) {
    if rom.len() >= 0x2000 && rom[0x1FFF] < 0xE0 && rom[0x1FFF].reverse_bits() >= 0xE0 {
        for b in rom.iter_mut() {
            *b = b.reverse_bits();
        }
    }
}

/// Everything around the CPU. Plain data: cloned to run again from any moment.
#[derive(Clone)]
struct Board {
    rom: Arc<Vec<u8>>,
    /// Street Fighter II's bank for $40-$7F.
    sf2: u8,
    ram: Vec<u8>,
    /// The video chip: the register picked, the registers, the status flags.
    vdc_reg: u8,
    vdc: [u16; 32],
    status: u8,
    /// The sprite table copy waits for the next vertical blank.
    satb_pending: bool,
    /// The interrupt controller's mask ($1402) and the timer.
    irq_mask: u8,
    timer_reload: u8,
    timer_on: bool,
    timer_count: u8,
    timer_clock: u32,
    timer_irq: bool,
    /// The joypad's select line; Run held.
    pad_sel: bool,
    buttons: bool,
    /// Master clocks into the frame.
    cycle: u32,
    sound: Vec<Write>,
    /// The sound unit's registers: the channel picked, the main balance, the LFO, and per
    /// channel its frequency, control, balance, noise and waveform (with its write position).
    psg_ch: u8,
    psg_main: u8,
    psg_lfo: [u8; 2],
    psg_regs: [[u8; 6]; 6],
    wave: [[u8; 32]; 6],
    wave_at: [u8; 6],
    stack: (u8, u8),
    /// RAM accesses, when traced: (address, written, value).
    trace: Option<Vec<(u32, bool, u8)>>,
}

impl Board {
    fn line(&self) -> u32 {
        self.cycle / LINE
    }

    fn rom_at(&self, bank: u8, off: u32) -> u8 {
        let pages = self.rom.len() / 0x2000;
        let b = bank as usize;
        let page = match self.rom.len() {
            // 384 KB: 256 KB then 128 KB, each mirrored in its half.
            0x60000 => {
                if b < 0x40 {
                    b & 0x1F
                } else {
                    0x20 + (b & 0x0F)
                }
            }
            // 768 KB: 512 KB, then 256 KB mirrored.
            0xC0000 => {
                if b < 0x40 {
                    b
                } else {
                    0x40 + (b & 0x1F)
                }
            }
            n if n > 0x100000 => {
                if b < 0x40 {
                    b
                } else {
                    0x40 + self.sf2 as usize * 0x40 + (b - 0x40)
                }
            }
            _ => b,
        };
        self.rom[(page % pages) * 0x2000 + off as usize]
    }

    /// The start of scanline `l`: the vertical blank and raster interrupts.
    fn new_line(&mut self, l: u32) {
        let cr = self.vdc[5];
        if l >= FIRST_LINE && l < VBLANK_LINE {
            let counter = l - FIRST_LINE + 64;
            if cr & 0x04 != 0 && counter == (self.vdc[6] & 0x3FF) as u32 {
                self.status |= RR;
            }
        }
        if l == VBLANK_LINE {
            if cr & 0x08 != 0 {
                self.status |= VD;
            }
            let dcr = self.vdc[0x0F];
            if self.satb_pending || dcr & 0x10 != 0 {
                self.satb_pending = false;
                if dcr & 0x01 != 0 {
                    self.status |= DS;
                }
            }
        }
    }

    fn advance(&mut self, clocks: u32) {
        let before = self.line();
        self.cycle += clocks;
        let after = self.line();
        for l in before + 1..=after {
            if l < LINES {
                self.new_line(l);
            }
        }
        if self.timer_on {
            self.timer_clock += clocks;
            while self.timer_clock >= TIMER_TICK {
                self.timer_clock -= TIMER_TICK;
                if self.timer_count == 0 {
                    self.timer_count = self.timer_reload;
                    self.timer_irq = true;
                } else {
                    self.timer_count -= 1;
                }
            }
        }
    }

    fn irq1(&self) -> bool {
        self.status & 0x3F != 0 && self.irq_mask & 0x02 == 0
    }

    fn timer(&self) -> bool {
        self.timer_irq && self.irq_mask & 0x04 == 0
    }

    fn sound(&mut self, reg: u32, v: u8) {
        self.sound.push(Write {
            at: self.cycle,
            port: reg,
            value: v as u16,
        });
        let ch = self.psg_ch as usize;
        match reg {
            0 => self.psg_ch = v & 7,
            1 => self.psg_main = v,
            8 | 9 => self.psg_lfo[reg as usize - 8] = v,
            2..=7 if ch < 6 => {
                let r = &mut self.psg_regs[ch];
                match reg {
                    4 => {
                        if r[2] & 0x40 != 0 && v & 0x40 == 0 {
                            self.wave_at[ch] = 0;
                        }
                        r[2] = v;
                    }
                    6 => {
                        if r[2] & 0x40 == 0 {
                            self.wave[ch][self.wave_at[ch] as usize] = v & 0x1F;
                            self.wave_at[ch] = (self.wave_at[ch] + 1) & 0x1F;
                        }
                    }
                    _ => r[reg as usize - 2] = v,
                }
            }
            _ => {}
        }
    }

    fn traced(&mut self, addr: u32, write: bool, v: u8) {
        if let Some(t) = self.trace.as_mut() {
            t.push((addr, write, v));
        }
    }

    fn io_read(&mut self, a: u32) -> u8 {
        match a {
            0x0000..=0x03FF => match a & 3 {
                0 => {
                    let s = self.status;
                    self.status = 0;
                    s
                }
                2 => self.vdc[self.vdc_reg as usize & 0x1F] as u8,
                3 => (self.vdc[self.vdc_reg as usize & 0x1F] >> 8) as u8,
                _ => 0,
            },
            0x0C00..=0x0FFF => self.timer_count & 0x7F,
            0x1000..=0x13FF => {
                // A Japanese PC Engine with no CD unit; Run is the only button ever held.
                let pad = if !self.pad_sel && self.buttons {
                    0x07
                } else {
                    0x0F
                };
                0xB0 | 0x40 | pad
            }
            0x1400..=0x17FF => match a & 3 {
                2 => self.irq_mask,
                3 => (self.timer_irq as u8) << 2 | ((self.status & 0x3F != 0) as u8) << 1,
                _ => 0,
            },
            _ => 0xFF,
        }
    }

    fn io_write(&mut self, a: u32, v: u8) {
        match a {
            0x0000..=0x03FF => match a & 3 {
                0 => self.vdc_reg = v & 0x1F,
                2 => {
                    let r = &mut self.vdc[self.vdc_reg as usize];
                    *r = (*r & 0xFF00) | v as u16;
                }
                3 => {
                    let reg = self.vdc_reg as usize;
                    self.vdc[reg] = (self.vdc[reg] & 0x00FF) | (v as u16) << 8;
                    match reg {
                        // A VRAM copy finishes at once.
                        0x12 if self.vdc[0x0F] & 0x02 != 0 => self.status |= DV,
                        0x13 => self.satb_pending = true,
                        _ => {}
                    }
                }
                _ => {}
            },
            0x0800..=0x0BFF => {
                let reg = a & 0x0F;
                if reg <= 9 {
                    self.sound(reg, v);
                }
            }
            0x0C00..=0x0FFF => {
                if a & 1 == 0 {
                    self.timer_reload = v & 0x7F;
                } else {
                    let on = v & 1 != 0;
                    if on && !self.timer_on {
                        self.timer_count = self.timer_reload;
                        self.timer_clock = 0;
                    }
                    self.timer_on = on;
                }
            }
            0x1000..=0x13FF => self.pad_sel = v & 1 != 0,
            0x1400..=0x17FF => match a & 3 {
                2 => self.irq_mask = v & 7,
                3 => self.timer_irq = false,
                _ => {}
            },
            _ => {}
        }
    }
}

impl Bus for Board {
    fn read(&mut self, addr: u32) -> u8 {
        let bank = (addr >> 13) as u8;
        let off = addr & 0x1FFF;
        match bank {
            0x00..=0x7F => self.rom_at(bank, off),
            0xF8..=0xFB => {
                let v = self.ram[off as usize];
                self.traced(RAM_BASE + off, false, v);
                v
            }
            0xFF => self.io_read(off),
            _ => 0xFF,
        }
    }

    fn write(&mut self, addr: u32, v: u8) {
        let bank = (addr >> 13) as u8;
        let off = addr & 0x1FFF;
        match bank {
            0x00 if (0x1FF0..=0x1FF3).contains(&off) && self.rom.len() > 0x100000 => {
                self.sf2 = (off & 3) as u8;
            }
            0xF8..=0xFB => {
                self.ram[off as usize] = v;
                self.traced(RAM_BASE + off, true, v);
            }
            0xFF => self.io_write(off, v),
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
    s_before: u8,
    /// Master clocks into the frame.
    cycle: u32,
}

/// A PC Engine: CPU and board.
#[derive(Clone)]
pub struct PcEngine {
    cpu: Cpu,
    board: Board,
}

impl PcEngine {
    fn clocks(&self, cycles: u32) -> u32 {
        cycles * if self.cpu.high_speed { 3 } else { 12 }
    }

    fn step_once(&mut self) -> u32 {
        let mut t = 0;
        if self.cpu.p & 0x04 == 0 {
            if self.board.timer() {
                t += self.cpu.interrupt(&mut self.board, VECTOR_TIMER);
            } else if self.board.irq1() {
                t += self.cpu.interrupt(&mut self.board, VECTOR_IRQ1);
            }
        }
        if let Some(tr) = self.board.trace.as_mut() {
            tr.clear();
        }
        t += self.cpu.step(&mut self.board);
        let clocks = self.clocks(t);
        self.board.advance(clocks);
        clocks
    }

    /// Runs the routine of `entry` once, from its own registers and banks, until it returns.
    /// False when it doesn't within `budget` master clocks.
    fn call(&mut self, entry: &Entry, set: Option<(u8, u8)>, budget: u32) -> bool {
        self.cpu = entry.cpu.clone();
        if let Some((reg, v)) = set {
            match reg {
                0 => self.cpu.a = v,
                1 => self.cpu.x = v,
                _ => self.cpu.y = v,
            }
        }
        let s = self.cpu.s;
        for (i, &b) in entry.pushed.iter().enumerate() {
            let at = 0x100 | s.wrapping_add(1 + i as u8) as usize;
            self.board.ram[at] = b;
        }
        let pc = self.cpu.physical(self.cpu.pc);
        self.cpu.calls = vec![(pc, entry.s_before)];
        let start = self.board.cycle;
        while !self.cpu.calls.is_empty() {
            if self.board.cycle - start > budget {
                return false;
            }
            let t = self.cpu.step(&mut self.board);
            self.board.cycle += self.clocks(t);
        }
        true
    }
}

/// The sound unit.
pub struct Chip(Chips);

impl Machine for PcEngine {
    type Entry = Entry;
    type Chip = Chip;
    const REGISTERS: u8 = 3;
    const CHANNELS: u16 = 2;

    fn boot(rom: &[u8]) -> Result<Self, String> {
        // A copier's 512-byte header goes.
        let rom = if rom.len() % 0x2000 == 512 {
            &rom[512..]
        } else {
            rom
        };
        if rom.len() < 0x2000 {
            return Err("not a PC Engine HuCard (too small)".into());
        }
        let mut r = rom.to_vec();
        r.resize(r.len().div_ceil(0x2000) * 0x2000, 0xFF);
        unscramble(&mut r);
        let mut board = Board {
            rom: Arc::new(r),
            sf2: 0,
            ram: vec![0; 0x2000],
            vdc_reg: 0,
            vdc: [0; 32],
            status: 0,
            satb_pending: false,
            irq_mask: 0,
            timer_reload: 0,
            timer_on: false,
            timer_count: 0,
            timer_clock: 0,
            timer_irq: false,
            pad_sel: false,
            buttons: false,
            cycle: 0,
            sound: Vec::new(),
            psg_ch: 0,
            psg_main: 0,
            psg_lfo: [0; 2],
            psg_regs: [[0; 6]; 6],
            wave: [[0; 32]; 6],
            wave_at: [0; 6],
            stack: (0xFF, 0xFF),
            trace: None,
        };
        let cpu = Cpu::reset(&mut board);
        Ok(Self { cpu, board })
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
                let s = self.cpu.s;
                let (lo, hi) = self.board.stack;
                self.board.stack = (lo.min(s), hi.max(s.saturating_add(3)));
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
        self.board.buttons = held;
    }

    fn entry(&self) -> Entry {
        let mut cpu = self.cpu.clone();
        let s_before = self.cpu.calls.last().map_or(self.cpu.s, |c| c.1);
        cpu.calls.clear();
        let n = s_before.wrapping_sub(self.cpu.s);
        let pushed = (1..=n.min(8))
            .map(|i| self.board.ram[0x100 | self.cpu.s.wrapping_add(i) as usize])
            .collect();
        Entry {
            cpu,
            pushed,
            s_before,
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
        if (RAM_BASE..RAM_BASE + 0x2000).contains(&addr) {
            self.board.ram[(addr - RAM_BASE) as usize] = v;
        }
    }

    fn incidental(_w: &Write) -> bool {
        false
    }

    fn is_stack(&self, addr: u32) -> bool {
        let (lo, hi) = self.board.stack;
        let base = RAM_BASE + 0x100;
        (base + lo.saturating_sub(0x10) as u32..=base + hi as u32).contains(&addr)
    }

    fn restore_writes(&self) -> Vec<Write> {
        let w = |port, v: u8| Write {
            at: 0,
            port,
            value: v as u16,
        };
        let b = &self.board;
        let mut out = vec![w(1, b.psg_main), w(8, b.psg_lfo[0]), w(9, b.psg_lfo[1])];
        for ch in 0..6 {
            let r = &b.psg_regs[ch];
            out.push(w(0, ch as u8));
            // Rewind the waveform's write position, load it, then leave the channel off: the
            // song turns on its own notes.
            out.push(w(4, 0x40));
            out.push(w(4, 0x00));
            out.extend(b.wave[ch].iter().map(|&s| w(6, s)));
            out.push(w(2, r[0]));
            out.push(w(3, r[1]));
            out.push(w(5, r[3]));
            out.push(w(7, r[5]));
        }
        out.push(w(0, b.psg_ch));
        out
    }

    fn new_chip(&self, rate: u32) -> Result<Chip, String> {
        Ok(Chip(Chips::new(
            rate,
            MASTER as f64,
            &[(Kind::Huc6280, PSG_CLOCK)],
        )?))
    }

    fn feed(&self, chip: &mut Chip, writes: &[Write], out: &mut Vec<i16>) {
        for w in writes {
            chip.0
                .write(w.at.min(FRAME - 1), 0, w.port as u8, w.value as u8);
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
    use super::super::music_search::{Trigger, measure, search};
    use super::*;

    /// A tiny game: the vertical blank interrupt calls a sound routine that plays song N (N
    /// written to $2200 by the main program, or 1 at boot) on channel 0, loading a square
    /// waveform and changing note every 8 frames through 16 notes, then round again.
    fn tiny_game() -> Vec<u8> {
        let mut rom = vec![0u8; 0x2000];
        // $E000: SEI; CSH; banks $FF and $F8 into MPR0/1; LDX #$FF; TXS; $2200 = 1;
        // vertical blank interrupt on (ST0 #5, ST1 #8, ST2 #0); only IRQ1 unmasked; CLI;
        // loop: BRA loop
        let main = [
            0x78, 0xD4, 0xA9, 0xFF, 0x53, 0x01, 0xA9, 0xF8, 0x53, 0x02, 0xA2, 0xFF, 0x9A, 0xA9,
            0x01, 0x8D, 0x00, 0x22, 0x03, 0x05, 0x13, 0x08, 0x23, 0x00, 0xA9, 0x05, 0x8D, 0x02,
            0x14, 0x58, 0x80, 0xFE,
        ];
        rom[0..main.len()].copy_from_slice(&main);
        // $E100: PHA; LDA $0000 (status); JSR $E200; PLA; RTI
        let irq = [0x48, 0xAD, 0x00, 0x00, 0x20, 0x00, 0xE2, 0x68, 0x40];
        rom[0x100..0x100 + irq.len()].copy_from_slice(&irq);
        // $E200: take a new song from $2200; every 8th frame pick channel 0, load a square
        // waveform, set the period to $100 + song * 16 + note, turn it on at full volume.
        let sound = [
            0xAD, 0x00, 0x22, 0xF0, 0x09, 0x8D, 0x01, 0x22, 0x9C, 0x00, 0x22, 0x9C, 0x02, 0x22,
            0xAD, 0x01, 0x22, 0xF0, 0x4F, 0xEE, 0x02, 0x22, 0xAD, 0x02, 0x22, 0x29, 0x07, 0xD0,
            0x45, 0x9C, 0x00, 0x08, 0xA9, 0xFF, 0x8D, 0x01, 0x08, 0x8D, 0x05, 0x08, 0xA9, 0x40,
            0x8D, 0x04, 0x08, 0x9C, 0x04, 0x08, 0xA2, 0x00, 0x8A, 0x29, 0x10, 0xF0, 0x02, 0xA9,
            0x1F, 0x8D, 0x06, 0x08, 0xE8, 0xE0, 0x20, 0xD0, 0xF1, 0xAD, 0x02, 0x22, 0x4A, 0x4A,
            0x4A, 0x29, 0x0F, 0x85, 0x10, 0xAD, 0x01, 0x22, 0x0A, 0x0A, 0x0A, 0x0A, 0x18, 0x65,
            0x10, 0x8D, 0x02, 0x08, 0xA9, 0x01, 0x8D, 0x03, 0x08, 0xA9, 0x9F, 0x8D, 0x04, 0x08,
            0x60,
        ];
        rom[0x200..0x200 + sound.len()].copy_from_slice(&sound);
        // Vectors: every interrupt to $E100, reset to $E000.
        for v in [0x1FF6, 0x1FF8, 0x1FFA, 0x1FFC] {
            rom[v] = 0x00;
            rom[v + 1] = 0xE1;
        }
        rom[0x1FFE] = 0x00;
        rom[0x1FFF] = 0xE0;
        rom
    }

    #[test]
    fn the_tiny_game_has_its_songs() {
        if !super::super::vgm_chips::available() {
            return;
        }
        let rom = tiny_game();
        let found = search::<PcEngine>(&rom).unwrap();
        assert_eq!(found.why, None);
        assert_eq!(found.recipe.play, 0x0200);
        assert_eq!(found.recipe.trigger, Trigger::Byte(0x2200));
        assert_eq!(found.songs.len(), 16, "{:?}", found.songs);
        let t = measure::<PcEngine>(&rom, &found.recipe, 3).unwrap();
        assert_eq!(t.looping.map(|l| l.1), Some(128), "{t:?}");
    }

    #[test]
    fn us_cards_are_unscrambled() {
        let mut rom = tiny_game();
        for b in rom.iter_mut() {
            *b = b.reverse_bits();
        }
        let m = PcEngine::boot(&rom).unwrap();
        assert_eq!(m.cpu.pc, 0xE000);
    }

    #[test]
    fn the_timer_interrupts() {
        let mut m = PcEngine::boot(&tiny_game()).unwrap();
        m.board.io_write(0x0C00, 2);
        m.board.io_write(0x0C01, 1);
        m.board.advance(TIMER_TICK * 2);
        assert!(!m.board.timer_irq);
        m.board.advance(TIMER_TICK);
        assert!(m.board.timer_irq);
        m.board.irq_mask = 0;
        assert!(m.board.timer());
        m.board.io_write(0x1403, 0);
        assert!(!m.board.timer());
    }
}
