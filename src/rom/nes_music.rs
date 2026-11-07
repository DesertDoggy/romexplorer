//! The NES as a [`Machine`] for the music search ([`super::music_search`]): a 2A03
//! ([`super::m6502`]), the cartridge's banking and sound chips ([`super::nes_cart`]), and just
//! enough of the picture chip for the timing games wait on (vertical blank, sprite 0, the
//! scanline counters). Nothing is drawn. The sound goes to libgme's chips ([`super::nes_apu`]).

use std::collections::HashMap;

use super::m6502::{Bus, Cpu};
use super::music_search::{Machine, Step, Write};
use super::nes_apu::{Apu, Target};
use super::nes_cart::{self, Cart};

/// CPU cycles per frame and scanlines per frame.
const NTSC_FRAME: u32 = 29_781;
const PAL_FRAME: u32 = 33_248;
const NTSC_LINES: u32 = 262;
const PAL_LINES: u32 = 312;
const VBLANK_LINE: u32 = 241;
/// Frames per second.
pub const NTSC_RATE: f64 = 60.0988;
pub const PAL_RATE: f64 = 50.007;

/// A sound register write: CPU cycle into the frame, where it goes, address, value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SoundWrite {
    pub at: u32,
    pub target: Target,
    pub addr: u16,
    pub value: u8,
}

/// Everything around the CPU. Plain data: cloned to run again from any moment.
#[derive(Clone)]
struct Board {
    ram: Vec<u8>,
    cart: Cart,
    pal: bool,
    ppuctrl: u8,
    ppumask: u8,
    vblank: bool,
    sprite0: bool,
    oam0_y: u8,
    nmi_due: bool,
    /// CPU cycles into the frame.
    cycle: u32,
    /// Instructions run (MMC1 tells writes of one instruction apart by it).
    instr: u64,
    buttons: u8,
    strobe: bool,
    shift: u8,
    /// Cycles the CPU loses to an OAM DMA.
    stall: u32,
    /// The APU's frame interrupt.
    frame_irq: bool,
    frame_counter: u32,
    frame_mode: u8,
    last_4015: u8,
    sound: Vec<SoundWrite>,
    /// The last value written to each sound register, and the Namco 163's sound memory: what
    /// a freshly started chip is given before a song plays from the middle of the game.
    shadow: HashMap<(Target, u16), u8>,
    n163_addr: u8,
    n163_ram: Vec<u8>,
    /// RAM accesses, when traced: (address, written, value).
    trace: Option<Vec<(u16, bool, u8)>>,
}

impl Board {
    fn new(h: &nes_cart::Header) -> Result<Self, String> {
        Ok(Self {
            ram: vec![0; 0x800],
            cart: Cart::new(h)?,
            pal: h.pal,
            ppuctrl: 0,
            ppumask: 0,
            vblank: false,
            sprite0: false,
            oam0_y: 0xF0,
            nmi_due: false,
            cycle: 0,
            instr: 0,
            buttons: 0,
            strobe: false,
            shift: 0,
            stall: 0,
            frame_irq: false,
            frame_counter: 0,
            frame_mode: 0,
            last_4015: 0,
            sound: Vec::new(),
            shadow: HashMap::new(),
            n163_addr: 0,
            n163_ram: vec![0; 128],
            trace: None,
        })
    }

    fn frame_cycles(&self) -> u32 {
        if self.pal {
            PAL_FRAME
        } else {
            NTSC_FRAME
        }
    }

    fn lines(&self) -> u32 {
        if self.pal {
            PAL_LINES
        } else {
            NTSC_LINES
        }
    }

    fn rendering(&self) -> bool {
        self.ppumask & 0x18 != 0
    }

    fn sound_write(&mut self, target: Target, addr: u16, value: u8) {
        self.sound.push(SoundWrite {
            at: self.cycle,
            target,
            addr,
            value,
        });
        match target {
            Target::N163Addr => self.n163_addr = value,
            Target::N163Data => {
                self.n163_ram[(self.n163_addr & 0x7F) as usize] = value;
                if self.n163_addr & 0x80 != 0 {
                    self.n163_addr = 0x80 | (self.n163_addr.wrapping_add(1) & 0x7F);
                }
            }
            _ => {
                self.shadow.insert((target, addr), value);
            }
        }
    }

    /// Writes that bring a fresh chip to the registers' current values. Writes that start a
    /// note (the length registers) are left out: the song writes its own.
    fn restore_writes(&self) -> Vec<SoundWrite> {
        let mut out: Vec<SoundWrite> = Vec::new();
        let w = |target, addr, value| SoundWrite {
            at: 0,
            target,
            addr,
            value,
        };
        for first in [0x4015u16, 0x4017] {
            if let Some(&v) = self.shadow.get(&(Target::Apu, first)) {
                out.push(w(Target::Apu, first, v));
            }
        }
        let mut rest: Vec<(&(Target, u16), &u8)> = self
            .shadow
            .iter()
            .filter(|((t, a), _)| {
                !(*t == Target::Apu
                    && matches!(a, 0x4015 | 0x4017 | 0x4003 | 0x4007 | 0x400B | 0x400F))
            })
            .collect();
        rest.sort_by_key(|((t, a), _)| (format!("{t:?}"), *a));
        out.extend(rest.into_iter().map(|(&(t, a), &v)| w(t, a, v)));
        if self.n163_ram.iter().any(|&b| b != 0) {
            out.push(w(Target::N163Addr, 0xF800, 0x80));
            out.extend(
                self.n163_ram
                    .iter()
                    .map(|&v| w(Target::N163Data, 0x4800, v)),
            );
            out.push(w(Target::N163Addr, 0xF800, self.n163_addr));
        }
        out
    }

    /// The APU frame counter's interrupt, cycle by cycle (4-step mode only).
    fn clock_apu(&mut self, cycles: u32) {
        self.frame_counter += cycles;
        if self.frame_counter >= 29_830 {
            self.frame_counter -= 29_830;
            if self.frame_mode & 0xC0 == 0 {
                self.frame_irq = true;
            }
        }
    }
}

impl Bus for Board {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x1FFF => {
                let a = addr & 0x7FF;
                let v = self.ram[a as usize];
                if let Some(t) = self.trace.as_mut() {
                    t.push((a, false, v));
                }
                v
            }
            0x2000..=0x3FFF => match addr & 7 {
                2 => {
                    let v = (self.vblank as u8) << 7 | (self.sprite0 as u8) << 6;
                    self.vblank = false;
                    v
                }
                _ => 0,
            },
            0x4015 => {
                let v = (self.last_4015 & 0x1F) | (self.frame_irq as u8) << 6;
                self.frame_irq = false;
                v
            }
            0x4016 => {
                let v = if self.strobe {
                    self.buttons & 1
                } else {
                    self.shift & 1
                };
                if !self.strobe {
                    self.shift = (self.shift >> 1) | 0x80;
                }
                0x40 | v
            }
            0x4017 => 0x40,
            0x4020..=0x5FFF => self
                .cart
                .read_low(addr)
                .or_else(|| self.cart.read(addr))
                .unwrap_or((addr >> 8) as u8),
            0x6000..=0x7FFF => {
                let v = self.cart.read(addr).unwrap_or((addr >> 8) as u8);
                if let Some(t) = self.trace.as_mut() {
                    t.push((addr, false, v));
                }
                v
            }
            _ => self.cart.read(addr).unwrap_or((addr >> 8) as u8),
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0x1FFF => {
                let a = addr & 0x7FF;
                self.ram[a as usize] = value;
                if let Some(t) = self.trace.as_mut() {
                    t.push((a, true, value));
                }
            }
            0x2000..=0x3FFF => match addr & 7 {
                0 => {
                    // Turning NMI on during vertical blank raises one at once.
                    if value & 0x80 != 0 && self.ppuctrl & 0x80 == 0 && self.vblank {
                        self.nmi_due = true;
                    }
                    self.ppuctrl = value;
                }
                1 => self.ppumask = value,
                _ => {}
            },
            0x4014 => {
                let page = (value as u16) << 8;
                self.oam0_y = self.read(page);
                self.stall += 513;
            }
            0x4016 => {
                self.strobe = value & 1 != 0;
                if self.strobe {
                    self.shift = self.buttons;
                }
            }
            0x4000..=0x4017 => {
                match addr {
                    0x4015 => self.last_4015 = value,
                    0x4017 => {
                        self.frame_mode = value;
                        self.frame_counter = 0;
                        if value & 0x40 != 0 {
                            self.frame_irq = false;
                        }
                    }
                    _ => {}
                }
                self.sound_write(Target::Apu, addr, value);
            }
            0x6000..=0x7FFF => {
                if let Some(t) = self.trace.as_mut() {
                    t.push((addr, true, value));
                }
                let instr = self.instr;
                if let Some(target) = self.cart.write(addr, value, instr) {
                    self.sound_write(target, addr, value);
                }
            }
            _ => {
                let instr = self.instr;
                if let Some(target) = self.cart.write(addr, value, instr) {
                    self.sound_write(target, addr, value);
                }
            }
        }
    }
}

/// A moment the CPU entered a routine: enough to call it again from there.
#[derive(Clone)]
pub struct Entry {
    cpu: Cpu,
    windows: [usize; 5],
    /// The stack bytes the call pushed (return address, and flags for an interrupt).
    stack: [u8; 3],
    interrupt: bool,
    /// Cycle into the frame.
    cycle: u32,
}

/// A NES: CPU and board.
#[derive(Clone)]
pub struct Nes {
    cpu: Cpu,
    board: Board,
}

fn port(target: Target, addr: u16) -> u32 {
    let code = match target {
        Target::Apu => 0,
        Target::Vrc6 { osc, reg } => 0x10 | (osc as u32) << 2 | reg as u32,
        Target::N163Addr => 0x20,
        Target::N163Data => 0x21,
        Target::Fme7Latch => 0x22,
        Target::Fme7Data => 0x23,
        Target::Mmc5 => 0x24,
    };
    code << 16 | addr as u32
}

fn target(port: u32) -> (Target, u16) {
    let addr = port as u16;
    let t = match port >> 16 {
        0 => Target::Apu,
        c @ 0x10..=0x1F => Target::Vrc6 {
            osc: ((c >> 2) & 3) as u8,
            reg: (c & 3) as u8,
        },
        0x20 => Target::N163Addr,
        0x21 => Target::N163Data,
        0x22 => Target::Fme7Latch,
        0x23 => Target::Fme7Data,
        _ => Target::Mmc5,
    };
    (t, addr)
}

fn generic(w: &SoundWrite) -> Write {
    Write {
        at: w.at,
        port: port(w.target, w.addr),
        value: w.value as u16,
    }
}

impl Nes {
    pub fn pal(&self) -> bool {
        self.board.pal
    }

    fn step_once(&mut self) -> u32 {
        let mut cycles = 0;
        if self.board.nmi_due {
            self.board.nmi_due = false;
            cycles += self.cpu.interrupt(&mut self.board, true);
        } else if (self.board.cart.irq || self.board.frame_irq) && self.cpu.p & 0x04 == 0 {
            cycles += self.cpu.interrupt(&mut self.board, false);
        }
        if let Some(t) = self.board.trace.as_mut() {
            t.clear();
        }
        self.board.instr += 1;
        cycles += self.cpu.step(&mut self.board) + std::mem::take(&mut self.board.stall);
        self.board.cycle += cycles;
        self.board.cart.clock(cycles);
        self.board.clock_apu(cycles);
        cycles
    }

    /// Runs the routine of `entry` once, from its own registers and program banks, until it
    /// returns. False when it doesn't within `budget` cycles.
    fn call(&mut self, entry: &Entry, set: Option<(u8, u8)>, budget: u32) -> bool {
        self.cpu = entry.cpu.clone();
        if let Some((reg, v)) = set {
            match reg {
                0 => self.cpu.a = v,
                1 => self.cpu.x = v,
                _ => self.cpu.y = v,
            }
        }
        self.board.cart.set_windows(entry.windows);
        let s = self.cpu.s;
        for (i, b) in entry.stack.iter().enumerate() {
            self.board.ram[0x100 + s.wrapping_add(1 + i as u8) as usize] = *b;
        }
        let pushed = if entry.interrupt { 3 } else { 2 };
        self.cpu.calls = vec![(self.cpu.pc, s.wrapping_add(pushed))];
        let mut used = 0;
        while !self.cpu.calls.is_empty() {
            if used > budget || self.cpu.jammed {
                return false;
            }
            self.board.instr += 1;
            let c = self.cpu.step(&mut self.board) + std::mem::take(&mut self.board.stall);
            self.board.cycle += c;
            used += c;
        }
        true
    }
}

/// The NES's sound chips, with the cartridge's.
pub struct Chip(Apu);

extern "C" fn read_dmc(data: *mut std::ffi::c_void, addr: std::ffi::c_uint) -> std::ffi::c_int {
    // SAFETY: set to a live Cart for the duration of one feed() call.
    let cart = unsafe { &*(data as *const Cart) };
    cart.peek(addr as u16) as std::ffi::c_int
}

impl Machine for Nes {
    type Entry = Entry;
    type Chip = Chip;
    const REGISTERS: u8 = 3;
    const CHANNELS: u16 = 1;

    fn boot(rom: &[u8]) -> Result<Self, String> {
        let h = nes_cart::parse(rom)?;
        if !nes_cart::supported(h.mapper) {
            return Err(format!(
                "this NES board (mapper {}) isn't supported yet",
                h.mapper
            ));
        }
        let mut board = Board::new(&h)?;
        let cpu = Cpu::reset(&mut board);
        Ok(Self { cpu, board })
    }

    fn run_frame(
        &mut self,
        mut watch: Option<&mut dyn FnMut(&Step)>,
        stop_after: Option<u64>,
    ) -> bool {
        let lines = self.board.lines();
        let frame_cycles = self.board.frame_cycles();
        self.board.cart.frame_start();
        let mut n = 0u64;
        let mut calls: Vec<(u32, u32)> = Vec::new();
        let mut sound: Vec<Write> = Vec::new();
        let mut ram: Vec<(u32, bool, u8)> = Vec::new();
        for line in 0..lines {
            let line_end = ((line + 1) as u64 * frame_cycles as u64 / lines as u64) as u32;
            if self.board.cycle >= line_end {
                continue;
            }
            if line == VBLANK_LINE {
                self.board.vblank = true;
                if self.board.ppuctrl & 0x80 != 0 {
                    self.board.nmi_due = true;
                }
            }
            if line == lines - 1 {
                self.board.vblank = false;
                self.board.sprite0 = false;
            }
            if line < 240 && self.board.rendering() {
                self.board.cart.scanline();
                if line > self.board.oam0_y as u32 {
                    self.board.sprite0 = true;
                }
            }
            while self.board.cycle < line_end {
                let before = self.cpu.calls.len();
                let sound_at = self.board.sound.len();
                self.step_once();
                n += 1;
                if let Some(w) = watch.as_deref_mut() {
                    calls.clear();
                    calls.extend(self.cpu.calls.iter().map(|&(r, s)| (r as u32, s as u32)));
                    sound.clear();
                    sound.extend(self.board.sound[sound_at..].iter().map(generic));
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
        }
        self.board.cycle -= frame_cycles;
        false
    }

    fn take_sound(&mut self) -> Vec<Write> {
        std::mem::take(&mut self.board.sound)
            .iter()
            .map(generic)
            .collect()
    }

    fn set_trace(&mut self, on: bool) {
        self.board.trace = on.then(Vec::new);
    }

    fn press_start(&mut self, held: bool) {
        self.board.buttons = if held { 0x08 } else { 0 };
    }

    fn entry(&self) -> Entry {
        let (_, s_before) = *self.cpu.calls.last().expect("inside a routine");
        let interrupt = s_before.wrapping_sub(self.cpu.s) == 3;
        let mut stack = [0; 3];
        for (i, b) in stack.iter_mut().enumerate() {
            *b = self.board.ram[0x100 + self.cpu.s.wrapping_add(1 + i as u8) as usize];
        }
        let mut cpu = self.cpu.clone();
        cpu.calls.clear();
        Entry {
            cpu,
            windows: self.board.cart.windows(),
            stack,
            interrupt,
            cycle: self.board.cycle,
        }
    }

    fn play_frame(&mut self, play: &Entry) -> Option<Vec<Write>> {
        self.board.cycle = play.cycle;
        let ok = self.call(play, None, self.board.frame_cycles() * 4);
        let frame = self.board.frame_cycles();
        if self.board.cycle < frame {
            let rest = frame - self.board.cycle;
            self.board.cart.clock(rest);
            self.board.clock_apu(rest);
        }
        self.board.cycle = 0;
        let writes = self.take_sound();
        ok.then_some(writes)
    }

    fn call_with(&mut self, entry: &Entry, reg: u8, v: u8) -> bool {
        let cycle = self.board.cycle;
        let windows = self.board.cart.windows();
        let cpu = self.cpu.clone();
        let ok = self.call(entry, Some((reg, v)), 200_000);
        self.board.cycle = cycle;
        self.board.cart.set_windows(windows);
        self.cpu = cpu;
        for w in self.board.sound.iter_mut() {
            w.at = cycle;
        }
        ok
    }

    fn poke(&mut self, addr: u32, v: u8) {
        match addr {
            0x0000..=0x07FF => self.board.ram[addr as usize] = v,
            0x6000..=0x7FFF => {
                if !self.board.cart.ram.is_empty() {
                    let n = self.board.cart.ram.len();
                    self.board.cart.ram[(addr as usize - 0x6000) % n] = v;
                }
            }
            _ => {}
        }
    }

    fn incidental(w: &Write) -> bool {
        w.port == 0x4015 || w.port == 0x4017
    }

    fn is_stack(&self, addr: u32) -> bool {
        (0x100..0x200).contains(&addr)
    }

    fn restore_writes(&self) -> Vec<Write> {
        self.board.restore_writes().iter().map(generic).collect()
    }

    fn new_chip(&self, rate: u32) -> Result<Chip, String> {
        Ok(Chip(Apu::new(
            rate,
            self.board.pal,
            self.board.cart.chips(),
        )?))
    }

    fn feed(&self, chip: &mut Chip, writes: &[Write], out: &mut Vec<i16>) {
        let apu = &mut chip.0;
        apu.set_reader(
            Some(read_dmc),
            &self.board.cart as *const Cart as *mut std::ffi::c_void,
        );
        for w in writes {
            let (t, addr) = target(w.port);
            apu.write(w.at, t, addr, w.value as u8);
        }
        apu.end_frame(self.board.frame_cycles(), out);
        apu.set_reader(None, std::ptr::null_mut());
    }

    fn frame_rate(&self) -> f64 {
        if self.board.pal {
            PAL_RATE
        } else {
            NTSC_RATE
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::music_search::{measure, search, Trigger};
    use super::*;

    /// A tiny game: NMI calls a sound routine that plays song N (N written to $0300 by the
    /// main program, or 1 at boot) as a square wave at a pitch per song, changing note every
    /// 8 frames through 16 notes, then round again.
    pub(crate) fn tiny_game() -> Vec<u8> {
        let mut prg = vec![0xEAu8; 0x4000];
        let org = 0xC000u16;
        let at = |a: u16| (a - org) as usize;
        // Reset: enable sound and NMI, request song 1, loop forever.
        let reset = [
            0x78, 0xD8, 0xA2, 0xFF, 0x9A, // SEI CLD LDX #$FF TXS
            0xA9, 0x0F, 0x8D, 0x15, 0x40, // LDA #$0F STA $4015
            0xA9, 0x01, 0x8D, 0x00, 0x03, // LDA #1 STA $0300 (request)
            0xA9, 0x80, 0x8D, 0x00, 0x20, // LDA #$80 STA $2000
            0x4C, 0x14, 0xC0, // JMP *
        ];
        prg[0..reset.len()].copy_from_slice(&reset);
        // NMI at $C100: JSR $C200; RTI
        prg[at(0xC100)..at(0xC100) + 4].copy_from_slice(&[0x20, 0x00, 0xC2, 0x40]);
        // Sound routine at $C200:
        //   LDA $0300; BEQ tick; STA $0301 (song); LDA #0; STA $0300; STA $0302 (timer)
        // tick: LDA $0301; BEQ done; INC $0302; LDA $0302; AND #7; BNE done
        //   LDA #$BF; STA $4000; LDA $0302; LSR; LSR; LSR; AND #$0F; CLC; ADC $0301; ASL; ASL;
        //   ASL; STA $4002; LDA #$08; STA $4003
        // done: RTS
        let sound = [
            0xAD, 0x00, 0x03, 0xF0, 0x0B, 0x8D, 0x01, 0x03, 0xA9, 0x00, 0x8D, 0x00, 0x03, 0x8D,
            0x02, 0x03, 0xAD, 0x01, 0x03, 0xF0, 0x26, 0xEE, 0x02, 0x03, 0xAD, 0x02, 0x03, 0x29,
            0x07, 0xD0, 0x1C, 0xA9, 0xBF, 0x8D, 0x00, 0x40, 0xAD, 0x02, 0x03, 0x4A, 0x4A, 0x4A,
            0x29, 0x0F, 0x18, 0x6D, 0x01, 0x03, 0x0A, 0x0A, 0x0A, 0x8D, 0x02, 0x40, 0xA9, 0x08,
            0x8D, 0x03, 0x40, 0x60,
        ];
        prg[at(0xC200)..at(0xC200) + sound.len()].copy_from_slice(&sound);
        // Vectors.
        prg[0x3FFA..0x4000].copy_from_slice(&[0x00, 0xC1, 0x00, 0xC0, 0x00, 0xC1]);
        let mut rom = vec![0u8; 16];
        rom[0..4].copy_from_slice(b"NES\x1A");
        rom[4] = 1;
        rom.extend(prg);
        rom
    }

    #[test]
    fn the_tiny_game_has_its_songs() {
        if !super::super::nes_apu::available() {
            return;
        }
        let rom = tiny_game();
        let found = search::<Nes>(&rom).unwrap();
        assert_eq!(found.why, None);
        assert_eq!(found.recipe.play, 0xC200);
        assert_eq!(found.recipe.trigger, Trigger::Byte(0x0300));
        // The routine's pitch is (note + song) * 8 in a byte, so songs 32 apart sound the same:
        // 32 different songs, and the one already playing.
        assert!(
            (32..=34).contains(&found.songs.len()),
            "{} songs",
            found.songs.len()
        );
        // 16 notes of 8 frames, round and round.
        let t = measure::<Nes>(&rom, &found.recipe, 3).unwrap();
        assert_eq!(t.looping.map(|l| l.1), Some(128), "{t:?}");
    }
}
