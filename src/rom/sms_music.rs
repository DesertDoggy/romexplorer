//! The Master System and Game Gear as a [`Machine`] for the music search
//! ([`super::music_search`]): the Z80 ([`super::z80`]), the cartridge's bank mapper (Sega's,
//! or Codemasters'), and just enough of the video chip for the timing games wait on (the
//! frame and line interrupts, the V counter). Nothing is drawn. No BIOS. The PSG and, on the
//! Master System, the Japanese FM unit go to libvgm's chips ([`super::vgm_chips`]).

use std::sync::Arc;

use super::music_search::{Machine, Step, Write};
use super::vgm_chips::{Chips, Kind};
use super::z80::{Bus, Cpu};

/// Z80 cycles a second, per scanline, and scanlines per frame (NTSC).
const CLOCK: u32 = 3_579_545;
const LINE: u32 = 228;
const LINES: u32 = 262;
const FRAME: u32 = LINE * LINES;
pub const RATE: f64 = CLOCK as f64 / FRAME as f64;

/// Sound ports: the PSG's data and the Game Gear's stereo register (chip 0); the FM unit's
/// address and data (chip 1).
const PSG: u32 = 0x000;
const GG_STEREO: u32 = 0x001;
const FM_ADDR: u32 = 0x100;
const FM_DATA: u32 = 0x101;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mapper {
    Sega,
    Codemasters,
}

/// Codemasters cartridges carry their own header at $7FE0, whose checksum and its
/// complement add up to $10000.
fn is_codemasters(rom: &[u8]) -> bool {
    if rom.len() < 0x8000 {
        return false;
    }
    let w = |a: usize| rom[a] as u32 | (rom[a + 1] as u32) << 8;
    let sum = w(0x7FE6) + w(0x7FE8);
    sum == 0x10000 && w(0x7FE6) != 0
}

/// Everything around the CPU. Plain data: cloned to run again from any moment.
#[derive(Clone)]
struct Board {
    rom: Arc<Vec<u8>>,
    mapper: Mapper,
    /// Banks in the three 16 KB slots, and the control register ($FFFC).
    banks: [u8; 3],
    control: u8,
    cram: Vec<u8>,
    ram: Vec<u8>,
    gg: bool,
    fm: bool,
    fm_latch: u8,
    fm_detect: u8,
    /// The video chip: its registers, the control port's first byte, the line counter.
    vdp: [u8; 16],
    vdp_latch: Option<u8>,
    frame_irq: bool,
    line_irq: bool,
    line_counter: u8,
    /// Z80 cycles into the frame.
    cycle: u32,
    buttons: bool,
    sound: Vec<Write>,
    /// The PSG's registers (tone 0-2, noise; volume 0-3) and its latched register, the Game
    /// Gear's stereo register, the FM unit's registers: what a freshly started chip is given
    /// before a song plays from the middle of the game.
    psg: [u16; 8],
    psg_latch: u8,
    stereo: u8,
    fm_regs: [u8; 0x40],
    stack: (u16, u16),
    /// RAM accesses, when traced: (address, written, value).
    trace: Option<Vec<(u16, bool, u8)>>,
}

impl Board {
    fn line(&self) -> u32 {
        self.cycle / LINE
    }

    fn v_counter(&self) -> u8 {
        let l = self.line();
        if l <= 0xDA {
            l as u8
        } else {
            (l - 6) as u8
        }
    }

    fn irq(&self) -> bool {
        (self.frame_irq && self.vdp[1] & 0x20 != 0) || (self.line_irq && self.vdp[0] & 0x10 != 0)
    }

    /// The start of scanline `l`: the frame interrupt and the line counter.
    fn new_line(&mut self, l: u32) {
        if l <= 192 {
            if self.line_counter == 0 {
                self.line_counter = self.vdp[10];
                self.line_irq = true;
            } else {
                self.line_counter -= 1;
            }
        } else {
            self.line_counter = self.vdp[10];
        }
        if l == 193 {
            self.frame_irq = true;
        }
    }

    fn slot(&self, addr: u16) -> usize {
        let banks = (self.rom.len() / 0x4000).max(1);
        let (slot, off) = ((addr >> 14) as usize, addr as usize & 0x3FFF);
        if self.mapper == Mapper::Sega && addr < 0x400 {
            return addr as usize % self.rom.len();
        }
        (self.banks[slot] as usize % banks) * 0x4000 + off
    }

    fn sound(&mut self, port: u32, v: u8) {
        self.sound.push(Write {
            at: self.cycle,
            port,
            value: v as u16,
        });
        match port {
            PSG => {
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
            GG_STEREO => self.stereo = v,
            FM_ADDR => self.fm_latch = v & 0x3F,
            _ => self.fm_regs[self.fm_latch as usize] = v,
        }
    }

    fn traced(&mut self, addr: u16, write: bool, v: u8) {
        if let Some(t) = self.trace.as_mut() {
            t.push((addr, write, v));
        }
    }
}

impl Bus for Board {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x8000..=0xBFFF if self.control & 0x08 != 0 => {
                let i = ((self.control as usize >> 2) & 1) * 0x4000 + (addr as usize - 0x8000);
                let v = self.cram[i];
                self.traced(addr, false, v);
                v
            }
            0x0000..=0xBFFF => self.rom[self.slot(addr)],
            _ => {
                let a = 0xC000 | (addr & 0x1FFF);
                let v = self.ram[(addr & 0x1FFF) as usize];
                self.traced(a, false, v);
                v
            }
        }
    }

    fn write(&mut self, addr: u16, v: u8) {
        match addr {
            0x0000 | 0x4000 | 0x8000 if self.mapper == Mapper::Codemasters => {
                self.banks[(addr >> 14) as usize] = v;
            }
            0x8000..=0xBFFF if self.control & 0x08 != 0 => {
                let i = ((self.control as usize >> 2) & 1) * 0x4000 + (addr as usize - 0x8000);
                self.cram[i] = v;
                self.traced(addr, true, v);
            }
            0x0000..=0xBFFF => {}
            _ => {
                self.ram[(addr & 0x1FFF) as usize] = v;
                self.traced(0xC000 | (addr & 0x1FFF), true, v);
                if addr >= 0xFFFC && self.mapper == Mapper::Sega {
                    match addr {
                        0xFFFC => self.control = v,
                        _ => self.banks[(addr - 0xFFFD) as usize] = v,
                    }
                }
            }
        }
    }

    fn input(&mut self, port: u16) -> u8 {
        let p = port as u8;
        if self.gg && p <= 0x06 {
            return match p {
                0 => {
                    if self.buttons {
                        0x40
                    } else {
                        0xC0
                    }
                }
                _ => 0xFF,
            };
        }
        if self.fm && p == 0xF2 {
            return self.fm_detect & 7;
        }
        match p & 0xC1 {
            0x40 => self.v_counter(),
            0x41 => ((self.cycle % LINE) * 256 / LINE / 2) as u8,
            0x80 => 0,
            0x81 => {
                let v = (self.frame_irq as u8) << 7;
                self.frame_irq = false;
                self.line_irq = false;
                self.vdp_latch = None;
                v | 0x1F
            }
            0xC0 => {
                // Button 1 starts most Master System games (Start is the Game Gear's).
                if self.buttons && !self.gg {
                    0xEF
                } else {
                    0xFF
                }
            }
            _ => 0xFF,
        }
    }

    fn output(&mut self, port: u16, v: u8) {
        let p = port as u8;
        if self.gg && p <= 0x06 {
            if p == 6 {
                self.sound(GG_STEREO, v);
            }
            return;
        }
        if self.fm {
            match p {
                0xF0 => return self.sound(FM_ADDR, v),
                0xF1 => return self.sound(FM_DATA, v),
                0xF2 => {
                    self.fm_detect = v;
                    return;
                }
                _ => {}
            }
        }
        match p & 0xC1 {
            0x40 | 0x41 => self.sound(PSG, v),
            0x81 => match self.vdp_latch.take() {
                None => self.vdp_latch = Some(v),
                Some(lo) => {
                    if v >> 6 == 2 {
                        self.vdp[(v & 0x0F) as usize] = lo;
                    }
                }
            },
            0x80 => self.vdp_latch = None,
            _ => {}
        }
    }
}

/// A moment the CPU entered a routine: enough to call it again from there.
#[derive(Clone)]
pub struct Entry {
    cpu: Cpu,
    banks: [u8; 3],
    control: u8,
    /// The return address the call pushed.
    ret: [u8; 2],
    /// Cycle into the frame.
    cycle: u32,
}

/// A Master System (`GG` false) or Game Gear (`GG` true): CPU and board.
#[derive(Clone)]
pub struct Sega8<const GG: bool> {
    cpu: Cpu,
    board: Board,
}

pub type MasterSystem = Sega8<false>;
pub type GameGear = Sega8<true>;

impl<const GG: bool> Sega8<GG> {
    fn step_once(&mut self) -> u32 {
        let mut t = 0;
        if self.board.irq() {
            t += self.cpu.interrupt(&mut self.board, false);
        }
        if let Some(tr) = self.board.trace.as_mut() {
            tr.clear();
        }
        t += self.cpu.step(&mut self.board);
        let before = self.board.line();
        self.board.cycle += t;
        let after = self.board.line();
        for l in before + 1..=after {
            if l < LINES {
                self.board.new_line(l);
            }
        }
        t
    }

    /// Runs the routine of `entry` once, from its own registers and banks, until it returns.
    /// False when it doesn't within `budget` cycles.
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
        self.board.banks = entry.banks;
        self.board.control = entry.control;
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
            self.board.cycle += self.cpu.step(&mut self.board);
        }
        true
    }
}

/// The PSG and, on a Master System, the FM unit.
pub struct Chip(Chips);

impl<const GG: bool> Machine for Sega8<GG> {
    type Entry = Entry;
    type Chip = Chip;
    const REGISTERS: u8 = 7;
    const CHANNELS: u16 = 2;

    fn boot(rom: &[u8]) -> Result<Self, String> {
        if rom.len() < 0x2000 {
            return Err("not a Master System or Game Gear ROM (too small)".into());
        }
        // Whole 16 KB banks.
        let mut r = rom.to_vec();
        r.resize(r.len().div_ceil(0x4000) * 0x4000, 0xFF);
        let mapper = if is_codemasters(rom) {
            Mapper::Codemasters
        } else {
            Mapper::Sega
        };
        let board = Board {
            rom: Arc::new(r),
            mapper,
            banks: [0, 1, 2],
            control: 0,
            cram: vec![0; 0x8000],
            ram: vec![0; 0x2000],
            gg: GG,
            fm: !GG,
            fm_latch: 0,
            fm_detect: 0,
            vdp: [0; 16],
            vdp_latch: None,
            frame_irq: false,
            line_irq: false,
            line_counter: 0,
            cycle: 0,
            buttons: false,
            sound: Vec::new(),
            psg: [0, 0x0F, 0, 0x0F, 0, 0x0F, 0, 0x0F],
            psg_latch: 0,
            stereo: 0xFF,
            fm_regs: [0; 0x40],
            stack: (0xDFF0, 0xDFF0),
            trace: None,
        };
        let mut cpu = Cpu::reset();
        cpu.sp = 0xDFF0;
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
        self.board.buttons = held;
    }

    fn entry(&self) -> Entry {
        let mut cpu = self.cpu.clone();
        cpu.calls.clear();
        let sp = self.cpu.sp;
        let peek = |a: u16| {
            if a >= 0xC000 {
                self.board.ram[(a & 0x1FFF) as usize]
            } else {
                0
            }
        };
        Entry {
            cpu,
            banks: self.board.banks,
            control: self.board.control,
            ret: [peek(sp), peek(sp.wrapping_add(1))],
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
        let (banks, control) = (self.board.banks, self.board.control);
        let cpu = self.cpu.clone();
        let ok = self.call(entry, Some((reg, v)), 800_000);
        self.board.cycle = cycle;
        self.board.banks = banks;
        self.board.control = control;
        self.cpu = cpu;
        for w in self.board.sound.iter_mut() {
            w.at = cycle;
        }
        ok
    }

    fn poke(&mut self, addr: u32, v: u8) {
        if addr >= 0xC000 {
            self.board.ram[(addr & 0x1FFF) as usize] = v;
        } else if (0x8000..0xC000).contains(&addr) {
            let i = ((self.board.control as usize >> 2) & 1) * 0x4000 + (addr as usize - 0x8000);
            self.board.cram[i] = v;
        }
    }

    fn incidental(w: &Write) -> bool {
        w.port == GG_STEREO
    }

    fn is_stack(&self, addr: u32) -> bool {
        let (lo, hi) = self.board.stack;
        (lo.saturating_sub(0x20) as u32..=hi as u32).contains(&addr)
    }

    fn restore_writes(&self) -> Vec<Write> {
        let w = |port, v: u8| Write {
            at: 0,
            port,
            value: v as u16,
        };
        let b = &self.board;
        let mut out = Vec::new();
        if GG {
            out.push(w(GG_STEREO, b.stereo));
        }
        for r in 0..8u8 {
            let v = b.psg[r as usize];
            if r & 1 == 1 {
                // Volumes silent: the song sets its own.
                out.push(w(PSG, 0x80 | r << 4 | 0x0F));
            } else if r < 6 {
                out.push(w(PSG, 0x80 | r << 4 | (v & 0x0F) as u8));
                out.push(w(PSG, ((v >> 4) & 0x3F) as u8));
            } else {
                out.push(w(PSG, 0x80 | r << 4 | (v & 0x0F) as u8));
            }
        }
        if b.fm {
            for r in (0x00..=0x07)
                .chain(0x10..=0x18)
                .chain(0x20..=0x28)
                .chain(0x30..=0x38)
            {
                // Keys off: the song starts its own notes.
                let v = if (0x20..=0x28).contains(&r) {
                    b.fm_regs[r] & !0x10
                } else {
                    b.fm_regs[r]
                };
                out.push(w(FM_ADDR, r as u8));
                out.push(w(FM_DATA, v));
            }
            out.push(w(FM_ADDR, 0x0E));
            out.push(w(FM_DATA, b.fm_regs[0x0E] & 0x20));
        }
        out
    }

    fn new_chip(&self, rate: u32) -> Result<Chip, String> {
        let psg = if GG { Kind::GameGearPsg } else { Kind::SegaPsg };
        let mut chips = vec![(psg, CLOCK)];
        if self.board.fm {
            chips.push((Kind::Ym2413, CLOCK));
        }
        Ok(Chip(Chips::new(rate, CLOCK as f64, &chips)?))
    }

    fn feed(&self, chip: &mut Chip, writes: &[Write], out: &mut Vec<i16>) {
        for w in writes {
            if w.port >> 8 == 1 && !self.board.fm {
                continue;
            }
            chip.0.write(
                w.at.min(FRAME - 1),
                (w.port >> 8) as u8,
                (w.port & 0xFF) as u8,
                w.value as u8,
            );
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

    /// A tiny game: the frame interrupt calls a sound routine that plays song N (N written to
    /// $C100 by the main program, or 1 at boot) as a square wave at a pitch per song,
    /// changing note every 8 frames through 16 notes, then round again.
    fn tiny_game() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        // $0000: DI; IM 1; LD SP,$DFF0; JP $0100
        rom[0..9].copy_from_slice(&[0xF3, 0xED, 0x56, 0x31, 0xF0, 0xDF, 0xC3, 0x00, 0x01]);
        // $0038: PUSH AF; IN A,($BF); CALL $0200; POP AF; EI; RETI
        rom[0x38..0x42]
            .copy_from_slice(&[0xF5, 0xDB, 0xBF, 0xCD, 0x00, 0x02, 0xF1, 0xFB, 0xED, 0x4D]);
        // $0100: LD A,1; LD ($C100),A; LD A,$20; OUT ($BF),A; LD A,$81; OUT ($BF),A; EI;
        // loop: HALT; JR loop
        let main = [
            0x3E, 0x01, 0x32, 0x00, 0xC1, 0x3E, 0x20, 0xD3, 0xBF, 0x3E, 0x81, 0xD3, 0xBF, 0xFB,
            0x76, 0x18, 0xFD,
        ];
        rom[0x100..0x100 + main.len()].copy_from_slice(&main);
        // $0200:
        //   LD A,($C100); OR A; JR Z,tick; LD ($C101),A; XOR A; LD ($C100),A; LD ($C102),A
        // tick: LD A,($C101); OR A; RET Z; LD HL,$C102; INC (HL); LD A,(HL); AND 7; RET NZ
        //   LD A,$90; OUT ($7F),A           ; tone 0 at full volume
        //   LD A,(HL); RRCA; RRCA; RRCA; AND $0F; LD B,A; LD A,($C101); ADD A,B; ADD A,A;
        //   ADD A,A; ADD A,A; LD B,A; AND $0F; OR $80; OUT ($7F),A; LD A,B; RRCA; RRCA;
        //   RRCA; RRCA; AND $0F; OR $10; OUT ($7F),A; RET
        let sound = [
            0x3A, 0x00, 0xC1, 0xB7, 0x28, 0x0A, 0x32, 0x01, 0xC1, 0xAF, 0x32, 0x00, 0xC1, 0x32,
            0x02, 0xC1, 0x3A, 0x01, 0xC1, 0xB7, 0xC8, 0x21, 0x02, 0xC1, 0x34, 0x7E, 0xE6, 0x07,
            0xC0, 0x3E, 0x90, 0xD3, 0x7F, 0x7E, 0x0F, 0x0F, 0x0F, 0xE6, 0x0F, 0x47, 0x3A, 0x01,
            0xC1, 0x80, 0x87, 0x87, 0x87, 0x47, 0xE6, 0x0F, 0xF6, 0x80, 0xD3, 0x7F, 0x78, 0x0F,
            0x0F, 0x0F, 0x0F, 0xE6, 0x0F, 0xF6, 0x10, 0xD3, 0x7F, 0xC9,
        ];
        rom[0x200..0x200 + sound.len()].copy_from_slice(&sound);
        rom
    }

    #[test]
    fn the_tiny_game_has_its_songs() {
        if !super::super::vgm_chips::available() {
            return;
        }
        let rom = tiny_game();
        let found = search::<MasterSystem>(&rom).unwrap();
        assert_eq!(found.why, None);
        assert_eq!(found.recipe.play, 0x0200);
        assert_eq!(found.recipe.trigger, Trigger::Byte(0xC100));
        assert!(
            (30..=34).contains(&found.songs.len()),
            "{} songs",
            found.songs.len()
        );
        let t = measure::<MasterSystem>(&rom, &found.recipe, 3).unwrap();
        assert_eq!(t.looping.map(|l| l.1), Some(128), "{t:?}");
    }

    #[test]
    fn sega_mapper_switches_banks() {
        let mut rom = vec![0u8; 0x4000 * 8];
        for b in 0..8 {
            rom[b * 0x4000 + 0x1000] = b as u8;
        }
        let mut m = MasterSystem::boot(&rom).unwrap();
        assert_eq!(m.board.read(0x9000), 2);
        m.board.write(0xFFFF, 5);
        assert_eq!(m.board.read(0x9000), 5);
        m.board.write(0xFFFD, 3);
        assert_eq!(m.board.read(0x1000), 3);
        // The first kilobyte never moves.
        rom[0x10] = 0xAA;
        let mut m = MasterSystem::boot(&rom).unwrap();
        m.board.write(0xFFFD, 3);
        assert_eq!(m.board.read(0x0010), 0xAA);
    }
}
