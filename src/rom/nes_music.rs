//! NES music played straight from the ROM, by running the game's own sound code.
//!
//! The game boots on a minimal NES ([`super::m6502`] and [`super::nes_cart`]; the picture is
//! never drawn, only the timing the code waits on is kept). Then the search:
//!
//! 1. **The sound routine.** Every write to a sound register is traced back to the routines
//!    in progress. The routine called once a frame that, run on its own from where the game
//!    called it, writes the same registers the game did, is the one that plays the music.
//! 2. **The song number.** A song starts when the game hands its sound code a number: written
//!    to a byte of RAM the sound routine reads, or passed to a routine the game calls. Each way
//!    seen during boot is tried with numbers 0-255, the sound routine running alone; the way
//!    that gives the most different, audible results is the game's, and those numbers are its
//!    songs.
//!
//! Playback does the same: boot to the same moment, hand over the number, run the sound
//! routine once a frame and send its writes to the sound chip ([`super::nes_apu`]). Nothing is
//! copied out of the ROM.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use super::m6502::{Bus, Cpu};
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

/// A machine: CPU and board.
#[derive(Clone)]
pub struct Nes {
    cpu: Cpu,
    board: Board,
    /// Frames run since power on.
    pub frame: u32,
}

/// What happened during one instruction, for the search.
struct Step<'a> {
    cpu: &'a Cpu,
    /// Routines entered by this instruction (an interrupt and a call can come together).
    entered: &'a [(u16, u8)],
    /// Sound writes made by this instruction.
    sound: &'a [SoundWrite],
    ram: &'a [(u16, bool, u8)],
}

impl Nes {
    pub fn new(rom: &[u8]) -> Result<Self, String> {
        let h = nes_cart::parse(rom)?;
        let mut board = Board::new(&h)?;
        let cpu = Cpu::reset(&mut board);
        Ok(Self {
            cpu,
            board,
            frame: 0,
        })
    }

    pub fn pal(&self) -> bool {
        self.board.pal
    }

    pub fn chips(&self) -> u8 {
        self.board.cart.chips()
    }

    /// Runs one frame. `watch` sees every instruction when given (slower).
    fn run_frame(&mut self, mut watch: Option<&mut dyn FnMut(Step)>) {
        let lines = self.board.lines();
        let frame_cycles = self.board.frame_cycles();
        self.board.cart.frame_start();
        for line in 0..lines {
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
            let line_end = ((line + 1) as u64 * frame_cycles as u64 / lines as u64) as u32;
            while self.board.cycle < line_end {
                let before = self.cpu.calls.len();
                let sound_at = self.board.sound.len();
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
                if let Some(w) = watch.as_deref_mut() {
                    let entered = self.cpu.calls.get(before..).unwrap_or(&[]);
                    let ram = self.board.trace.as_deref().unwrap_or(&[]);
                    w(Step {
                        cpu: &self.cpu,
                        entered,
                        sound: &self.board.sound[sound_at..],
                        ram,
                    });
                }
            }
        }
        self.board.cycle -= frame_cycles;
        self.frame += 1;
    }

    /// The sound writes of the frames run since the last call.
    fn take_sound(&mut self) -> Vec<SoundWrite> {
        std::mem::take(&mut self.board.sound)
    }

    /// The moment the CPU is in now, as an entry to the routine it just entered.
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

    /// One frame of the sound routine alone: called at its usual moment, then the frame runs
    /// out. Returns the frame's sound writes; None when the routine didn't return.
    fn play_frame(&mut self, play: &Entry) -> Option<Vec<SoundWrite>> {
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

    fn poke(&mut self, addr: u16, v: u8) {
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
}

/// How the game is handed a song number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// Written to this RAM byte.
    Byte(u16),
    /// Passed in a register (0 A, 1 X, 2 Y) to the routine at this address, as called the
    /// `nth` time during boot.
    Call { addr: u16, reg: u8, nth: u32 },
}

impl Trigger {
    fn encode(&self) -> String {
        match self {
            Trigger::Byte(a) => format!("m{a:04x}"),
            Trigger::Call { addr, reg, nth } => format!("c{addr:04x}.{reg}.{nth}"),
        }
    }

    fn decode(s: &str) -> Option<Self> {
        if let Some(a) = s.strip_prefix('m') {
            return u16::from_str_radix(a, 16).ok().map(Trigger::Byte);
        }
        let mut p = s.strip_prefix('c')?.split('.');
        let addr = u16::from_str_radix(p.next()?, 16).ok()?;
        let reg = p.next()?.parse().ok()?;
        let nth = p.next()?.parse().ok()?;
        Some(Trigger::Call { addr, reg, nth })
    }
}

/// Where a game's music is: the moment to boot to, the sound routine, how songs are started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipe {
    /// Frames booted before the sound routine's call that playback starts from.
    pub boot_frames: u32,
    /// Start was pressed during boot (some games only start their music then).
    pub press_start: bool,
    /// The sound routine.
    pub play: u16,
    pub trigger: Trigger,
}

impl Recipe {
    pub fn encode(&self) -> String {
        format!(
            "{}@{}@{:04x}@{}",
            self.boot_frames,
            self.press_start as u8,
            self.play,
            self.trigger.encode()
        )
    }

    pub fn decode(s: &str) -> Option<Self> {
        let mut p = s.split('@');
        let boot_frames = p.next()?.parse().ok()?;
        let press_start = p.next()? == "1";
        let play = u16::from_str_radix(p.next()?, 16).ok()?;
        let trigger = Trigger::decode(p.next()?)?;
        Some(Self {
            boot_frames,
            press_start,
            play,
            trigger,
        })
    }
}

/// Buttons held on the controller in frame `frame` of boot: nothing, or Start for a few
/// frames every two seconds after the first three.
fn input(press_start: bool, frame: u32) -> u8 {
    if press_start && frame >= 180 && frame % 120 < 6 {
        0x08
    } else {
        0
    }
}

/// How long boot runs while the search looks for the sound routine.
const BOOT_FRAMES: u32 = 900;
/// Frames the sound routine runs alone and is compared with the game.
const VERIFY_FRAMES: usize = 240;
/// Share of those frames that must match.
const VERIFY_MATCH: f64 = 0.8;
/// Frames a song number is listened to while the search compares them.
const TRY_FRAMES: usize = 90;
/// A song is listened to for at most this long while measuring it.
const MEASURE_FRAMES: usize = 60 * 300;
/// Silence this long ends a song.
const SILENT_END_FRAMES: usize = 180;
/// A loop must repeat this many frames to count.
const LOOP_WINDOW: usize = 240;
/// Shortest loop taken.
const MIN_LOOP_FRAMES: usize = 60;

fn frame_hash(writes: &[SoundWrite]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for w in writes {
        (w.target, w.addr, w.value).hash(&mut h);
    }
    h.finish()
}

/// The boot run's record: per frame, the sound writes; per routine, how it shows up in them.
struct BootLog {
    frames: Vec<Vec<SoundWrite>>,
    /// Sound writes made inside each routine (any depth), and their total.
    writes_in: HashMap<u16, u32>,
    total_writes: u32,
    /// How deep each routine sat when writes happened inside it (sum of depths, for a mean).
    depth: HashMap<u16, (u64, u32)>,
    /// Frames each routine was entered in, and how often in all.
    entered_frames: HashMap<u16, (HashSet<u32>, u32)>,
}

fn boot_log(rom: &[u8], press_start: bool) -> Result<BootLog, String> {
    let mut nes = Nes::new(rom)?;
    let mut log = BootLog {
        frames: Vec::new(),
        writes_in: HashMap::new(),
        total_writes: 0,
        depth: HashMap::new(),
        entered_frames: HashMap::new(),
    };
    for f in 0..BOOT_FRAMES {
        nes.board.buttons = input(press_start, f);
        let mut watch = |s: Step| {
            for &(r, _) in s.entered {
                let e = log.entered_frames.entry(r).or_default();
                e.0.insert(f);
                e.1 += 1;
            }
            for w in s.sound {
                // The status and frame counter registers are set once and left: they say
                // nothing about which routine plays the music.
                if w.addr == 0x4015 || w.addr == 0x4017 {
                    continue;
                }
                log.total_writes += 1;
                let mut seen = HashSet::new();
                for (depth, &(r, _)) in s.cpu.calls.iter().enumerate() {
                    if seen.insert(r) {
                        *log.writes_in.entry(r).or_default() += 1;
                        let d = log.depth.entry(r).or_default();
                        d.0 += depth as u64;
                        d.1 += 1;
                    }
                }
            }
        };
        nes.run_frame(Some(&mut watch));
        log.frames.push(nes.take_sound());
    }
    Ok(log)
}

/// The machine at the first entry to `routine` in frame `frame` of boot.
fn entry_at(
    rom: &[u8],
    press_start: bool,
    frame: u32,
    routine: u16,
) -> Result<Option<(Nes, Entry)>, String> {
    let mut nes = Nes::new(rom)?;
    for f in 0..frame {
        nes.board.buttons = input(press_start, f);
        nes.run_frame(None);
    }
    nes.take_sound();
    nes.board.buttons = input(press_start, frame);
    let mut found: Option<(Nes, Entry)> = None;
    // Run the frame watching for the entry; the machine is copied at that instruction.
    let mut probe = nes.clone();
    let mut hit = None;
    let mut count = 0u64;
    {
        let mut watch = |s: Step| {
            count += 1;
            if hit.is_none() && s.entered.iter().any(|e| e.0 == routine) {
                hit = Some(count);
            }
        };
        probe.run_frame(Some(&mut watch));
    }
    let Some(target) = hit else { return Ok(None) };
    // Run again to that instruction and stop there.
    let mut n = 0u64;
    let lines = nes.board.lines();
    let frame_cycles = nes.board.frame_cycles();
    nes.board.cart.frame_start();
    'outer: for line in 0..lines {
        if line == VBLANK_LINE {
            nes.board.vblank = true;
            if nes.board.ppuctrl & 0x80 != 0 {
                nes.board.nmi_due = true;
            }
        }
        if line == lines - 1 {
            nes.board.vblank = false;
            nes.board.sprite0 = false;
        }
        if line < 240 && nes.board.rendering() {
            nes.board.cart.scanline();
            if line > nes.board.oam0_y as u32 {
                nes.board.sprite0 = true;
            }
        }
        let line_end = ((line + 1) as u64 * frame_cycles as u64 / lines as u64) as u32;
        while nes.board.cycle < line_end {
            let mut cycles = 0;
            if nes.board.nmi_due {
                nes.board.nmi_due = false;
                cycles += nes.cpu.interrupt(&mut nes.board, true);
            } else if (nes.board.cart.irq || nes.board.frame_irq) && nes.cpu.p & 0x04 == 0 {
                cycles += nes.cpu.interrupt(&mut nes.board, false);
            }
            nes.board.instr += 1;
            cycles += nes.cpu.step(&mut nes.board) + std::mem::take(&mut nes.board.stall);
            nes.board.cycle += cycles;
            nes.board.cart.clock(cycles);
            nes.board.clock_apu(cycles);
            n += 1;
            if n == target {
                let entry = nes.entry();
                nes.take_sound();
                found = Some((nes.clone(), entry));
                break 'outer;
            }
        }
    }
    Ok(found)
}

/// Runs the sound routine alone for `frames` frames from `nes`. The frames' write lists.
fn run_alone(nes: &mut Nes, play: &Entry, frames: usize) -> Option<Vec<Vec<SoundWrite>>> {
    let mut out = Vec::with_capacity(frames);
    // The rest of the entry frame first: the routine's first call is the one that frame made.
    for _ in 0..frames {
        out.push(nes.play_frame(play)?);
    }
    Some(out)
}

/// What the search found.
#[derive(Debug, Clone)]
pub struct Found {
    pub recipe: Recipe,
    /// Song numbers, in order.
    pub songs: Vec<u8>,
    /// Why nothing was found, when nothing was.
    pub why: Option<String>,
}

/// Finds the sound routine of a game that played music during boot. Returns the frame it was
/// verified at and the routine.
fn find_play(rom: &[u8], press_start: bool, log: &BootLog) -> Result<Option<(u32, u16)>, String> {
    if log.total_writes < 50 {
        return Ok(None);
    }
    // Routines most writes happened inside, innermost first.
    let mut candidates: Vec<(u16, f64)> = log
        .writes_in
        .iter()
        .filter(|(_, &n)| n as f64 >= 0.6 * log.total_writes as f64)
        .map(|(&r, _)| {
            let (sum, n) = log.depth[&r];
            (r, sum as f64 / n.max(1) as f64)
        })
        .collect();
    candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    // A frame well into the music, where the routine was entered.
    let first_sound = log
        .frames
        .iter()
        .position(|f| f.iter().any(|w| w.addr != 0x4015 && w.addr != 0x4017))
        .unwrap_or(0) as u32;
    for (routine, _) in candidates {
        let Some((frames, total)) = log.entered_frames.get(&routine) else {
            continue;
        };
        // Called about once a frame from the start of the music on.
        let span = BOOT_FRAMES.saturating_sub(first_sound).max(1);
        let per_frame = *total as f64 / frames.len().max(1) as f64;
        if (frames.len() as f64) < 0.5 * span as f64 || per_frame > 1.5 {
            continue;
        }
        let latest = BOOT_FRAMES - VERIFY_FRAMES as u32 - 1;
        let mut at = (first_sound + 60).min(latest);
        while at <= latest && !frames.contains(&at) {
            at += 1;
        }
        if at > latest {
            continue;
        }
        let Some((mut nes, entry)) = entry_at(rom, press_start, at, routine)? else {
            continue;
        };
        let Some(alone) = run_alone(&mut nes, &entry, VERIFY_FRAMES) else {
            continue;
        };
        let reference = &log.frames[at as usize..at as usize + VERIFY_FRAMES];
        let key = |f: &[SoundWrite]| {
            f.iter()
                .filter(|w| w.addr != 0x4017)
                .map(|w| (w.target, w.addr, w.value))
                .collect::<Vec<_>>()
        };
        let same = alone
            .iter()
            .zip(reference)
            .filter(|(a, b)| key(a) == key(b))
            .count();
        if same as f64 >= VERIFY_MATCH * VERIFY_FRAMES as f64 {
            return Ok(Some((at, routine)));
        }
    }
    Ok(None)
}

/// Ways the game hands its sound code a number, seen during boot.
fn trigger_candidates(rom: &[u8], press_start: bool, play: u16) -> Result<Vec<Trigger>, String> {
    let mut nes = Nes::new(rom)?;
    nes.board.trace = Some(Vec::new());
    let mut read_by_play: HashSet<u16> = HashSet::new();
    // Addresses the sound routine wrote before reading them in the same call are its own
    // working state, not requests.
    let mut written_by_play: HashSet<u16> = HashSet::new();
    let mut written_by_game: HashMap<u16, u32> = HashMap::new();
    // Routines called outside the sound routine, how often, and the addresses written inside
    // them.
    let mut calls: HashMap<u16, u32> = HashMap::new();
    let mut writes_inside: HashMap<u16, HashSet<u16>> = HashMap::new();
    let mut handlers: HashSet<u16> = HashSet::new();
    for f in 0..BOOT_FRAMES {
        nes.board.buttons = input(press_start, f);
        let mut watch = |s: Step| {
            let in_play = s.cpu.calls.iter().any(|c| c.0 == play);
            for (i, &(r, _)) in s.entered.iter().enumerate() {
                if !in_play {
                    *calls.entry(r).or_default() += 1;
                }
                // The outermost entries are interrupt handlers (the main program is in none).
                if s.cpu.calls.len() - s.entered.len() + i == 0 {
                    handlers.insert(r);
                }
            }
            for &(a, write, _) in s.ram {
                if (0x100..0x200).contains(&a) {
                    continue;
                }
                if in_play {
                    if write {
                        written_by_play.insert(a);
                    } else if !written_by_play.contains(&a) {
                        read_by_play.insert(a);
                    }
                } else if write {
                    *written_by_game.entry(a).or_default() += 1;
                    for &(r, _) in &s.cpu.calls {
                        writes_inside.entry(r).or_default().insert(a);
                    }
                }
            }
        };
        nes.run_frame(Some(&mut watch));
        nes.take_sound();
    }
    let mut out = Vec::new();
    let mut bytes: Vec<(u16, u32)> = written_by_game
        .into_iter()
        .filter(|(a, _)| read_by_play.contains(a))
        .collect();
    // Bytes the game writes rarely are requests; ones it writes every frame are game state.
    bytes.sort_by_key(|&(a, n)| (n, a));
    out.extend(
        bytes
            .into_iter()
            .filter(|&(_, n)| n < BOOT_FRAMES / 2)
            .take(24)
            .map(|(a, _)| Trigger::Byte(a)),
    );
    let mut routines: Vec<(u16, u32)> = calls
        .into_iter()
        .filter(|(r, n)| {
            *r != play
                && !handlers.contains(r)
                && *n <= 16
                && writes_inside
                    .get(r)
                    .is_some_and(|w| w.iter().any(|a| read_by_play.contains(a)))
        })
        .collect();
    routines.sort_by_key(|&(r, n)| (n, r));
    for (r, _) in routines.into_iter().take(8) {
        for reg in 0..3 {
            out.push(Trigger::Call {
                addr: r,
                reg,
                nth: 0,
            });
        }
    }
    Ok(out)
}

/// The entry of the `nth` call to `routine` during boot (input as `press_start` says).
fn call_entry(
    rom: &[u8],
    press_start: bool,
    routine: u16,
    nth: u32,
) -> Result<Option<Entry>, String> {
    let mut nes = Nes::new(rom)?;
    let mut seen = 0;
    for f in 0..BOOT_FRAMES {
        nes.board.buttons = input(press_start, f);
        let mut probe = nes.clone();
        let mut target = None;
        let mut count = 0u64;
        let mut watch = |s: Step| {
            count += 1;
            if target.is_none() && s.entered.iter().any(|e| e.0 == routine) {
                if seen == nth {
                    target = Some(count);
                }
                seen += 1;
            }
        };
        probe.run_frame(Some(&mut watch));
        match target {
            None => {
                probe.take_sound();
                nes = probe;
            }
            Some(t) => return Ok(entry_at_instruction(&mut nes, t).map(|(_, e)| e)),
        }
    }
    Ok(None)
}

/// Runs `nes` through the current frame up to its `target`th instruction and returns the
/// machine and the entry there.
fn entry_at_instruction(nes: &mut Nes, target: u64) -> Option<(Nes, Entry)> {
    let lines = nes.board.lines();
    let frame_cycles = nes.board.frame_cycles();
    nes.board.cart.frame_start();
    let mut n = 0u64;
    for line in 0..lines {
        if line == VBLANK_LINE {
            nes.board.vblank = true;
            if nes.board.ppuctrl & 0x80 != 0 {
                nes.board.nmi_due = true;
            }
        }
        if line == lines - 1 {
            nes.board.vblank = false;
            nes.board.sprite0 = false;
        }
        if line < 240 && nes.board.rendering() {
            nes.board.cart.scanline();
            if line > nes.board.oam0_y as u32 {
                nes.board.sprite0 = true;
            }
        }
        let line_end = ((line + 1) as u64 * frame_cycles as u64 / lines as u64) as u32;
        while nes.board.cycle < line_end {
            let mut cycles = 0;
            if nes.board.nmi_due {
                nes.board.nmi_due = false;
                cycles += nes.cpu.interrupt(&mut nes.board, true);
            } else if (nes.board.cart.irq || nes.board.frame_irq) && nes.cpu.p & 0x04 == 0 {
                cycles += nes.cpu.interrupt(&mut nes.board, false);
            }
            nes.board.instr += 1;
            cycles += nes.cpu.step(&mut nes.board) + std::mem::take(&mut nes.board.stall);
            nes.board.cycle += cycles;
            nes.board.cart.clock(cycles);
            nes.board.clock_apu(cycles);
            n += 1;
            if n == target {
                let entry = nes.entry();
                nes.take_sound();
                return Some((nes.clone(), entry));
            }
        }
    }
    None
}

/// Starts song `v` by `trigger` on `nes` (the machine at the sound routine's entry).
fn apply(nes: &mut Nes, trigger: &Trigger, call: Option<&Entry>, v: u8) -> bool {
    match trigger {
        Trigger::Byte(a) => {
            nes.poke(*a, v);
            true
        }
        Trigger::Call { reg, .. } => {
            let Some(e) = call else { return false };
            let cycle = nes.board.cycle;
            let windows = nes.board.cart.windows();
            let cpu = nes.cpu.clone();
            let ok = nes.call(e, Some((*reg, v)), 200_000);
            // The routine's own writes to the chip belong to the song's first frame.
            nes.board.cycle = cycle;
            nes.board.cart.set_windows(windows);
            nes.cpu = cpu;
            for w in nes.board.sound.iter_mut() {
                w.at = cycle;
            }
            ok
        }
    }
}

/// A song's first frames as heard: its writes' fingerprint, and whether anything sounded.
fn listen(
    base: &Nes,
    play: &Entry,
    trigger: &Trigger,
    call: Option<&Entry>,
    v: u8,
    frames: usize,
) -> Option<(u64, bool)> {
    let mut nes = base.clone();
    if !apply(&mut nes, trigger, call, v) {
        return None;
    }
    let mut pre = nes.board.restore_writes();
    pre.extend(nes.take_sound());
    let mut apu = Apu::new(8000, nes.pal(), nes.chips()).ok()?;
    let mut samples = Vec::new();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for w in &pre {
        apu.write(0, w.target, w.addr, w.value);
    }
    for _ in 0..frames {
        let writes = nes.play_frame(play)?;
        frame_hash(&writes).hash(&mut h);
        feed(&mut apu, &nes, &writes, &mut samples);
    }
    let loud = samples.iter().filter(|s| s.unsigned_abs() > 300).count();
    Some((h.finish(), loud > samples.len() / 10))
}

extern "C" fn read_dmc(data: *mut std::ffi::c_void, addr: std::ffi::c_uint) -> std::ffi::c_int {
    // SAFETY: set to a live Cart for the duration of one feed() call.
    let cart = unsafe { &*(data as *const Cart) };
    cart.peek(addr as u16) as std::ffi::c_int
}

/// Sends one frame of writes to the chip and appends the frame's samples.
fn feed(apu: &mut Apu, nes: &Nes, writes: &[SoundWrite], out: &mut Vec<i16>) {
    apu.set_reader(
        Some(read_dmc),
        &nes.board.cart as *const Cart as *mut std::ffi::c_void,
    );
    for w in writes {
        apu.write(w.at, w.target, w.addr, w.value);
    }
    apu.end_frame(nes.board.frame_cycles(), out);
    apu.set_reader(None, std::ptr::null_mut());
}

/// Searches a NES ROM image for its music.
pub fn search(rom: &[u8]) -> Result<Found, String> {
    search_logged(rom, &mut |_| {})
}

/// [`search`], telling `note` what it tries and finds (for the PC test tool).
pub fn search_logged(rom: &[u8], note: &mut dyn FnMut(String)) -> Result<Found, String> {
    let header = nes_cart::parse(rom)?;
    if !nes_cart::supported(header.mapper) {
        return Err(format!(
            "this NES board (mapper {}) isn't supported yet",
            header.mapper
        ));
    }
    let mut why = "the game played no music in its first 15 seconds".to_string();
    for press_start in [false, true] {
        let log = boot_log(rom, press_start)?;
        let busiest: Vec<String> = {
            let mut v: Vec<(&u16, &u32)> = log.writes_in.iter().collect();
            v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
            v.iter()
                .take(4)
                .map(|(r, n)| format!("{r:04x}:{n}"))
                .collect()
        };
        note(format!(
            "boot{}: {} sound writes; routines {}",
            if press_start { " (Start)" } else { "" },
            log.total_writes,
            busiest.join(" ")
        ));
        let Some((at, play)) = find_play(rom, press_start, &log)? else {
            if log.total_writes >= 50 {
                why = "the routine that plays the music wasn't found".into();
            }
            continue;
        };
        let Some((base, entry)) = entry_at(rom, press_start, at, play)? else {
            continue;
        };
        let mut best: Option<(Trigger, Vec<u8>)> = None;
        let candidates = trigger_candidates(rom, press_start, play)?;
        note(format!(
            "sound routine {play:04x} (frame {at}); {} ways to start a song to try",
            candidates.len()
        ));
        // A quick round with a few numbers each, then all numbers for the best two ways.
        let baseline =
            listen(&base, &entry, &Trigger::Byte(0xFFFF), None, 0, TRY_FRAMES).map(|r| r.0);
        let mut screened: Vec<(usize, Trigger, Option<Entry>)> = Vec::new();
        for t in candidates {
            let call = match &t {
                Trigger::Call { addr, nth, .. } => match call_entry(rom, press_start, *addr, *nth)?
                {
                    Some(e) => Some(e),
                    None => continue,
                },
                Trigger::Byte(_) => None,
            };
            let mut seen = HashSet::new();
            for v in 1..=8u8 {
                if let Some((h, audible)) =
                    listen(&base, &entry, &t, call.as_ref(), v, TRY_FRAMES / 2)
                {
                    if audible && Some(h) != baseline {
                        seen.insert(h);
                    }
                }
            }
            if !seen.is_empty() {
                note(format!(
                    "  {:?}: {} of 8 numbers sound different",
                    t,
                    seen.len()
                ));
                screened.push((seen.len(), t, call));
            }
        }
        screened.sort_by_key(|s| std::cmp::Reverse(s.0));
        for (_, t, call) in screened.into_iter().take(2) {
            let mut songs = Vec::new();
            let mut seen = HashSet::new();
            for v in 0..=255u8 {
                if let Some((h, audible)) = listen(&base, &entry, &t, call.as_ref(), v, TRY_FRAMES)
                {
                    if audible && seen.insert(h) {
                        songs.push(v);
                    }
                }
            }
            note(format!("  {:?}: {} songs", t, songs.len()));
            if best.as_ref().is_none_or(|b| songs.len() > b.1.len()) {
                best = Some((t, songs));
            }
        }
        if let Some((trigger, songs)) = best.filter(|b| !b.1.is_empty()) {
            let recipe = Recipe {
                boot_frames: at,
                press_start,
                play,
                trigger,
            };
            return Ok(Found {
                recipe,
                songs,
                why: None,
            });
        }
        why = "the sound routine was found, but not how the game picks a song".into();
    }
    Ok(Found {
        recipe: Recipe {
            boot_frames: 0,
            press_start: false,
            play: 0,
            trigger: Trigger::Byte(0),
        },
        songs: Vec::new(),
        why: Some(why),
    })
}

/// A song, ready to play: the machine at the sound routine, the song started.
pub struct Song {
    nes: Nes,
    play: Entry,
    apu: Apu,
    pre: Vec<SoundWrite>,
}

impl Song {
    /// Boots `rom` as `recipe` says and starts song `v`, rendering at `rate`.
    pub fn open(rom: &[u8], recipe: &Recipe, v: u8, rate: u32) -> Result<Self, String> {
        let (mut nes, play) = entry_at(rom, recipe.press_start, recipe.boot_frames, recipe.play)?
            .ok_or("the game no longer reaches its sound routine")?;
        let call = match &recipe.trigger {
            Trigger::Call { addr, nth, .. } => Some(
                call_entry(rom, recipe.press_start, *addr, *nth)?
                    .ok_or("the game no longer calls its song routine")?,
            ),
            Trigger::Byte(_) => None,
        };
        if !apply(&mut nes, &recipe.trigger, call.as_ref(), v) {
            return Err("the song didn't start".into());
        }
        let mut pre = nes.board.restore_writes();
        pre.extend(nes.take_sound());
        let apu = Apu::new(rate, nes.pal(), nes.chips())?;
        Ok(Self {
            nes,
            play,
            apu,
            pre,
        })
    }

    pub fn frame_rate(&self) -> f64 {
        if self.nes.pal() {
            PAL_RATE
        } else {
            NTSC_RATE
        }
    }

    /// One frame: its samples appended to `out`, and the frame's sound writes.
    pub fn frame(&mut self, out: &mut Vec<i16>) -> Result<Vec<SoundWrite>, String> {
        let mut writes = std::mem::take(&mut self.pre);
        writes.extend(
            self.nes
                .play_frame(&self.play)
                .ok_or("the sound routine stopped returning")?,
        );
        feed(&mut self.apu, &self.nes, &writes, out);
        Ok(writes)
    }
}

/// How a song plays: its length in frames, and where it loops (start, length) if it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    pub frames: usize,
    pub looping: Option<(usize, usize)>,
}

/// Listens to a song to time it: until it falls silent, repeats itself, or runs five minutes.
pub fn measure(rom: &[u8], recipe: &Recipe, v: u8) -> Result<Timing, String> {
    let mut song = Song::open(rom, recipe, v, 8000)?;
    let mut hashes: Vec<u64> = Vec::new();
    let mut last_loud = 0usize;
    let mut samples = Vec::new();
    // Rolling hash of the last LOOP_WINDOW frames → first frame index it was seen at.
    let mut windows: HashMap<u64, usize> = HashMap::new();
    for f in 0..MEASURE_FRAMES {
        samples.clear();
        let writes = song.frame(&mut samples)?;
        hashes.push(frame_hash(&writes));
        if samples.iter().any(|s| s.unsigned_abs() > 300) {
            last_loud = f;
        }
        if f > last_loud + SILENT_END_FRAMES {
            return Ok(Timing {
                frames: last_loud + 1,
                looping: None,
            });
        }
        if f + 1 >= LOOP_WINDOW {
            let start = f + 1 - LOOP_WINDOW;
            let mut h = std::collections::hash_map::DefaultHasher::new();
            hashes[start..=f].hash(&mut h);
            let key = h.finish();
            match windows.get(&key) {
                Some(&first)
                    if start - first >= MIN_LOOP_FRAMES
                        && hashes[first..first + LOOP_WINDOW] == hashes[start..=f] =>
                {
                    return Ok(Timing {
                        frames: start,
                        looping: Some((first, start - first)),
                    });
                }
                Some(_) => {}
                None => {
                    windows.insert(key, start);
                }
            }
        }
    }
    Ok(Timing {
        frames: MEASURE_FRAMES,
        looping: None,
    })
}

#[cfg(test)]
mod tests {
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
    fn the_tiny_game_boots_and_writes_its_sound() {
        let rom = tiny_game();
        let log = boot_log(&rom, false).unwrap();
        assert!(log.total_writes > 100, "{} writes", log.total_writes);
        assert!(log.writes_in.contains_key(&0xC200));
    }

    #[test]
    fn the_search_finds_the_routine_and_the_songs() {
        let rom = tiny_game();
        let log = boot_log(&rom, false).unwrap();
        let (at, play) = find_play(&rom, false, &log)
            .unwrap()
            .expect("sound routine");
        assert_eq!(play, 0xC200);
        assert!(at > 0);
        let triggers = trigger_candidates(&rom, false, play).unwrap();
        assert!(triggers.contains(&Trigger::Byte(0x0300)), "{triggers:?}");
    }

    #[test]
    fn recipes_round_trip() {
        let r = Recipe {
            boot_frames: 123,
            press_start: true,
            play: 0xC200,
            trigger: Trigger::Call {
                addr: 0x8123,
                reg: 1,
                nth: 2,
            },
        };
        assert_eq!(Recipe::decode(&r.encode()), Some(r));
        let r = Recipe {
            boot_frames: 5,
            press_start: false,
            play: 0x8000,
            trigger: Trigger::Byte(0x7F0),
        };
        assert_eq!(Recipe::decode(&r.encode()), Some(r));
    }

    /// The whole search, with the sound chip (needs the rommusic library).
    #[test]
    fn the_tiny_game_has_its_songs() {
        if !super::super::nes_apu::available() {
            return;
        }
        let rom = tiny_game();
        let found = search(&rom).unwrap();
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
        let t = measure(&rom, &found.recipe, 3).unwrap();
        assert_eq!(t.looping.map(|l| l.1), Some(128), "{t:?}");
    }
}
