//! A PC Engine game's music map, from a matched HES file and the ROM: the HuC6280
//! counterpart of `gb_map` (see there). A HES has no play routine: its music runs from the
//! vertical blank and timer interrupts. The file's own init and interrupt code become writes
//! and calls into the game's routines; its data is laid over the ROM's 8 KB banks; only
//! numbers are kept, and the music plays from the user's ROM with no boot.

use std::collections::{HashMap, HashSet};

use super::huc6280::{Bus, Cpu, VECTOR_IRQ1, VECTOR_TIMER};
use super::music_map::Run;
use super::music_search::{self, Write};

/// Song numbers tried: a HES doesn't say how many it has.
pub const MAX_SONGS: u8 = 128;
/// Timer ticks in a 1/60 s frame (7.16 MHz / 1024).
const TICKS_PER_FRAME: f64 = 7_159_090.0 / 1024.0 / 60.0;

/// A HES file's header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub init: u16,
    pub banks: [u8; 8],
    /// Where its data loads in the 21-bit address space.
    pub addr: u32,
    pub songs: u8,
}

impl Header {
    pub fn read(hes: &[u8]) -> Option<Self> {
        if hes.len() < 0x20 || &hes[..4] != b"HESM" {
            return None;
        }
        let mut banks = [0u8; 8];
        banks.copy_from_slice(&hes[8..16]);
        let addr = u32::from_le_bytes([hes[0x18], hes[0x19], hes[0x1A], hes[0x1B]]) & 0x1F_FFFF;
        Some(Header { init: u16::from_le_bytes([hes[6], hes[7]]), banks, addr, songs: MAX_SONGS })
    }

    /// The 8 KB bank (and offset in it) data offset `d` sits at.
    fn phys_of(&self, d: usize) -> u32 {
        self.addr + d as u32
    }
}

/// The file's data in the address space ($00-$7F banks), 32 KB of RAM, and just enough of
/// the video chip, timer and interrupt controller for its interrupts.
struct Player {
    addr: u32,
    data_len: usize,
    mem: Vec<u8>,
    ram: Vec<u8>,
    writes: Vec<Write>,
    used: Option<Vec<bool>>,
    log: Option<Vec<(u32, u8)>>,
    vdc_latch: u8,
    vdc_control: u8,
    timer_reload: u8,
    timer_on: bool,
    disables: u8,
    /// The time writes are stamped with (master clocks into the frame).
    at: u32,
}

impl Player {
    fn new(h: &Header, data: &[u8]) -> Self {
        let mut mem = vec![0u8; 0x10_0000];
        for (i, &b) in data.iter().enumerate() {
            if let Some(m) = mem.get_mut(h.addr as usize + i) {
                *m = b;
            }
        }
        Player {
            addr: h.addr,
            data_len: data.len(),
            mem,
            ram: vec![0; 0x8000],
            writes: Vec::new(),
            used: None,
            log: None,
            vdc_latch: 0,
            vdc_control: 0,
            timer_reload: 0,
            timer_on: false,
            disables: 0x06,
            at: 0,
        }
    }

    /// A player over the game's ROM at its own banks.
    fn board(h: &Header, rom: &[u8]) -> Self {
        let mut p = Player::new(h, &[]);
        for bank in 0..0x80u32 {
            if let Some(from) = rom_bank_at(rom.len(), bank) {
                let at = (bank as usize) << 13;
                p.mem[at..at + 0x2000].copy_from_slice(&rom[from..from + 0x2000]);
            }
        }
        p
    }

    fn src(&self, phys: u32) -> u32 {
        match phys.checked_sub(self.addr) {
            Some(d) if (d as usize) < self.data_len && phys < 0x10_0000 => d + 1,
            _ => 0,
        }
    }

    /// Runs the routine at `pc` with A X Y and the banks of `regs`, entered as a call or (with
    /// `irq`) as an interrupt, with `pushed` on the stack; false when it doesn't return.
    fn call_with(&mut self, cpu: &mut Cpu, pc: u16, regs: &Regs, irq: bool, pushed: &[u8]) -> bool {
        cpu.mpr = regs.mpr;
        cpu.s = 0xFF;
        let s0 = cpu.s;
        let mut push = |cpu: &mut Cpu, p: &mut Player, v: u8| {
            let at = cpu.physical(0x2100 | cpu.s as u16);
            p.write(at, v);
            cpu.s = cpu.s.wrapping_sub(1);
        };
        // A return address the routine's RTS (or RTI) takes, never run.
        push(cpu, self, 0x1F);
        push(cpu, self, if irq { 0xFF } else { 0xFE });
        if irq {
            push(cpu, self, 0x04);
        }
        for &v in pushed {
            push(cpu, self, v);
        }
        cpu.pc = pc;
        (cpu.a, cpu.x, cpu.y) = (regs.a, regs.x, regs.y);
        cpu.calls = vec![(cpu.physical(pc), s0)];
        let mut used = 0u32;
        while !cpu.calls.is_empty() {
            if used > 4_000_000 {
                return false;
            }
            used += cpu.step(self);
        }
        true
    }

    /// Interrupts due this frame: the timer's (as many as its rate gives) and the vertical
    /// blank's.
    fn timer_rate(&self) -> f64 {
        if self.timer_on && self.disables & 0x04 == 0 {
            TICKS_PER_FRAME / ((self.timer_reload & 0x7F) as f64 + 1.0)
        } else {
            0.0
        }
    }

    fn vblank(&self) -> bool {
        self.vdc_control & 0x08 != 0 && self.disables & 0x02 == 0
    }
}

impl Bus for Player {
    fn read(&mut self, addr: u32) -> u8 {
        let bank = (addr >> 13) as u8;
        let off = addr & 0x1FFF;
        match bank {
            0x00..=0x7F => {
                if let (Some(used), Some(d)) = (self.used.as_mut(), addr.checked_sub(self.addr)) {
                    if let Some(u) = used.get_mut(d as usize) {
                        *u = true;
                    }
                }
                self.mem[addr as usize]
            }
            0xF8..=0xFB => self.ram[((bank - 0xF8) as usize) << 13 | off as usize],
            // The video chip's status: in the vertical blank.
            0xFF if off < 0x400 => 0x20,
            0xFF if (0x1400..0x1800).contains(&off) && off & 3 == 2 => self.disables,
            _ => 0,
        }
    }

    fn write(&mut self, addr: u32, v: u8) {
        if let Some(log) = self.log.as_mut() {
            log.push((addr, v));
        }
        let bank = (addr >> 13) as u8;
        let off = addr & 0x1FFF;
        match bank {
            0xF8..=0xFB => self.ram[((bank - 0xF8) as usize) << 13 | off as usize] = v,
            0xFF => match off {
                0x0000..=0x03FF => match off & 3 {
                    0 => self.vdc_latch = v & 0x1F,
                    2 if self.vdc_latch == 5 => self.vdc_control = v,
                    _ => {}
                },
                0x0800..=0x0BFF => {
                    let reg = off & 0x0F;
                    if reg <= 9 {
                        self.writes.push(Write { at: self.at, port: reg, value: v as u16 });
                    }
                }
                0x0C00..=0x0FFF => {
                    if off & 1 == 0 {
                        self.timer_reload = v & 0x7F;
                    } else {
                        self.timer_on = v & 1 != 0;
                    }
                }
                0x1400..=0x17FF if off & 3 == 2 => self.disables = v & 7,
                _ => {}
            },
            _ => {}
        }
    }
}

/// The registers and banks a call into the game's code is made with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Regs {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub mpr: [u8; 8],
}

impl Regs {
    fn of(c: &Cpu) -> Self {
        Regs { a: c.a, x: c.x, y: c.y, mpr: c.mpr }
    }
}

/// One step of a script: a write the file's own code makes (21-bit address), a byte it
/// leaves on the stack, or a call into the game's routine at `addr`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Poke(u32, u8),
    Push(u8),
    Call { addr: u16, regs: Regs, irq: bool, src: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Way {
    Layout,
    Zeros,
    /// On the game's own banks: the file's bank numbers turned into the game's.
    Board,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill<'a> {
    Zeros,
    Bridged,
    File(&'a [u8]),
    /// The whole ROM at its own banks ([`Way::Board`]).
    Board,
}

/// A track's scripts: its init (with its first frames' one-off steps), and what the vertical
/// blank and timer interrupts do each time from then on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Track {
    pub init: Vec<Op>,
    pub vblank: Vec<Op>,
    pub timer: Vec<Op>,
}

/// A PC Engine game's music map: numbers only.
#[derive(Debug, Clone)]
pub struct Map {
    pub header: Header,
    pub data_len: usize,
    pub runs: Vec<Run>,
    /// Per song number: its scripts, None for a song number with no melody in the file.
    pub tracks: Vec<Option<Track>>,
    pub ways: Vec<Way>,
}

const PATCHED_BYTES: u16 = 3;
/// Frames whose interrupts are recorded before they settle.
const SETTLE_FRAMES: usize = 6;

fn is_jump(op: u8) -> bool {
    matches!(op, 0x20 | 0x44 | 0x4C | 0x6C | 0x7C | 0x60 | 0x40)
}

/// Records the routine at `pc` (entered as an interrupt with `irq`) as a script; see
/// `nes_nsf`'s recorder.
fn record(p: &mut Player, cpu: &mut Cpu, pc: u16, a: u8, irq: bool, placed: &[bool], aligned: &[bool]) -> Option<Vec<Op>> {
    let regs = Regs { a, x: cpu.x, y: cpu.y, mpr: cpu.mpr };
    cpu.s = 0xFF;
    let s0 = cpu.s;
    for v in [0x1F, if irq { 0xFF } else { 0xFE }].into_iter().chain(irq.then_some(0x04)) {
        let at = cpu.physical(0x2100 | cpu.s as u16);
        p.write(at, v);
        cpu.s = cpu.s.wrapping_sub(1);
    }
    cpu.pc = pc;
    cpu.a = regs.a;
    cpu.calls = vec![(cpu.physical(pc), s0)];
    // As in `gb_map`: the routine's own entry needs the bytes after it placed too.
    let of = |p: &Player, cpu: &Cpu, addr: u16, set: &[bool], bytes: usize| {
        let s = p.src(cpu.physical(addr));
        s > 0 && (0..bytes).all(|k| set.get(s as usize - 1 + k).copied().unwrap_or(k > 0))
    };
    let mut ops = Vec::new();
    let mut in_game: Option<usize> = None;
    let mut used = 0u32;
    let mut jumped = true;
    let mut target: Option<(u16, u8)> = None;
    let mut first = true;
    while !cpu.calls.is_empty() {
        if used > 4_000_000 {
            return None;
        }
        if in_game.is_some_and(|d| cpu.calls.len() < d) {
            in_game = None;
        }
        let at = cpu.pc;
        if in_game.is_none() {
            let near = target.is_some_and(|(t, _)| at > t && at - t <= PATCHED_BYTES);
            let bytes = if first { 3 } else { 1 };
            let entered = if jumped || near { of(p, cpu, at, placed, bytes) } else { of(p, cpu, at, aligned, bytes) };
            if entered {
                if let Some((_, s)) = target.filter(|t| t.1 > cpu.s) {
                    ops.extend((cpu.s as u16 + 1..=s as u16).rev().map(|i| {
                        let a = cpu.physical(0x2100 | i);
                        Op::Push(p.read(a))
                    }));
                }
                let src = p.src(cpu.physical(at));
                ops.push(Op::Call { addr: at, regs: Regs::of(cpu), irq: irq && first, src });
                in_game = Some(cpu.calls.len());
            }
        }
        first = false;
        let op = p.read(cpu.physical(at));
        p.log = in_game.is_none().then(Vec::new);
        used += cpu.step(p);
        jumped = is_jump(op);
        if jumped && in_game.is_none() {
            target = Some((cpu.pc, cpu.s));
        }
        if let Some(log) = p.log.take() {
            // The live stack is set up by each call itself.
            let live_lo = cpu.physical(0x2100 | cpu.s as u16);
            let live_hi = cpu.physical(0x21FF);
            ops.extend(log.into_iter().filter(|&(a, _)| !(a > live_lo && a <= live_hi)).map(|(a, v)| Op::Poke(a, v)));
        }
    }
    Some(ops)
}

fn replay(p: &mut Player, cpu: &mut Cpu, ops: &[Op]) -> bool {
    let mut pushed = Vec::new();
    for op in ops {
        match *op {
            Op::Poke(a, v) => p.write(a, v),
            Op::Push(v) => pushed.push(v),
            Op::Call { addr, regs, irq, .. } => {
                if !p.call_with(cpu, addr, &regs, irq, &std::mem::take(&mut pushed)) {
                    return false;
                }
            }
        }
    }
    true
}

fn same_steps(a: &[Op], b: &[Op]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| match (x, y) {
            (Op::Call { addr: p, irq: i, .. }, Op::Call { addr: q, irq: j, .. }) => p == q && i == j,
            _ => x == y,
        })
}

/// The vector an interrupt takes, as the banks are now.
fn vector(p: &mut Player, cpu: &Cpu, v: u16) -> u16 {
    u16::from_le_bytes([p.read(cpu.physical(v)), p.read(cpu.physical(v + 1))])
}

/// Runs a fresh player through song `t`'s init and `frames` frames of interrupts, showing
/// each frame's end to `each`.
fn run_track(h: &Header, data: &[u8], t: u8, frames: usize, mut each: impl FnMut(&mut Player)) -> Option<()> {
    let mut p = Player::new(h, data);
    let mut cpu = Cpu::with(h.init, 0xFF);
    let regs = Regs { a: t, mpr: h.banks, ..Default::default() };
    if !p.call_with(&mut cpu, h.init, &regs, false, &[]) {
        return None;
    }
    let mut due = 0.0;
    for _ in 0..frames {
        due += p.timer_rate();
        let n = due as u32;
        due -= n as f64;
        for k in 0..n {
            p.at = k * FRAME_CLOCKS / n.max(1);
            let to = vector(&mut p, &cpu, VECTOR_TIMER);
            if !{ let r = Regs::of(&cpu); p.call_with(&mut cpu, to, &r, true, &[]) } {
                return Some(());
            }
        }
        if p.vblank() {
            p.at = FRAME_CLOCKS - 1;
            let to = vector(&mut p, &cpu, VECTOR_IRQ1);
            if !{ let r = Regs::of(&cpu); p.call_with(&mut cpu, to, &r, true, &[]) } {
                return Some(());
            }
        }
        p.at = 0;
        each(&mut p);
    }
    Some(())
}

/// Song `song` of a HES played on its own, `frames` frames: each frame's sound writes.
pub fn track_writes(hes: &[u8], song: u8, frames: usize) -> Option<Vec<Vec<Write>>> {
    let h = Header::read(hes)?;
    let mut out = Vec::with_capacity(frames);
    run_track(&h, &hes[0x20..], song, frames, |p| out.push(std::mem::take(&mut p.writes)))?;
    Some(out)
}

/// A melody's fingerprint on the PSG's six channels: each channel's 12-bit period from its
/// channel-selected writes.
pub fn melody(frames: &[Vec<Write>]) -> HashSet<u64> {
    let mut ch = 0usize;
    let mut period = [0u16; 6];
    let as_ports: Vec<Vec<Write>> = frames
        .iter()
        .map(|f| {
            let mut out = Vec::new();
            for w in f {
                match w.port {
                    0 => ch = (w.value & 7) as usize,
                    2 if ch < 6 => period[ch] = (period[ch] & 0xF00) | (w.value & 0xFF),
                    3 if ch < 6 => period[ch] = (period[ch] & 0xFF) | ((w.value & 0x0F) << 8),
                    _ => continue,
                }
                if ch < 6 && matches!(w.port, 2 | 3) {
                    out.push(Write { at: 0, port: 0x3000 + ch as u32 * 2, value: period[ch] & 0xFF });
                    out.push(Write { at: 0, port: 0x3001 + ch as u32 * 2, value: (period[ch] >> 8) & 7 });
                }
            }
            out
        })
        .collect();
    let pairs: Vec<(u32, u32)> = (0..6).map(|c| (0x3000 + c * 2, 0x3001 + c * 2)).collect();
    music_search::melody(&as_ports, &pairs)
}

/// The song numbers with a melody in the file, and which data bytes they read.
fn songs_and_used(hes: &[u8], frames: usize) -> Option<(Vec<u8>, Vec<bool>)> {
    let h = Header::read(hes)?;
    let data = &hes[0x20..];
    let mut used = vec![false; data.len()];
    let mut songs = Vec::new();
    let mut seen: Vec<HashSet<u64>> = Vec::new();
    for t in 0..h.songs {
        let mut frames_out = Vec::new();
        let mut p_used = Some(std::mem::take(&mut used));
        {
            let mut p = Player::new(&h, data);
            p.used = p_used.take();
            let mut cpu = Cpu::with(h.init, 0xFF);
            let regs = Regs { a: t, mpr: h.banks, ..Default::default() };
            if p.call_with(&mut cpu, h.init, &regs, false, &[]) {
                let mut due = 0.0;
                for _ in 0..frames {
                    due += p.timer_rate();
                    let n = due as u32;
                    due -= n as f64;
                    let mut ok = true;
                    for _ in 0..n {
                        let to = vector(&mut p, &cpu, VECTOR_TIMER);
                        ok &= { let r = Regs::of(&cpu); p.call_with(&mut cpu, to, &r, true, &[]) };
                    }
                    if ok && p.vblank() {
                        let to = vector(&mut p, &cpu, VECTOR_IRQ1);
                        ok &= { let r = Regs::of(&cpu); p.call_with(&mut cpu, to, &r, true, &[]) };
                    }
                    frames_out.push(std::mem::take(&mut p.writes));
                    if !ok {
                        break;
                    }
                }
            }
            used = p.used.take().unwrap_or_default();
        }
        let m = melody(&frames_out);
        // A song number past the last plays one of the others again (or nothing).
        if m.len() >= music_search::MIN_MELODY && !seen.iter().any(|s| music_search::alike(s, &m) > 0.95) {
            songs.push(t);
            seen.push(m);
        }
    }
    Some((songs, used))
}

/// Frames each song plays when the file's songs are listed and their bytes found.
const LIST_FRAMES: usize = 600;

/// The music map of a PC Engine ROM (without its 512-byte header, if any) for the HES it
/// matches.
pub fn map(hes: &[u8], rom: &[u8]) -> Option<Map> {
    let h = Header::read(hes)?;
    let data = &hes[0x20..];
    let (songs, used) = songs_and_used(hes, LIST_FRAMES)?;
    let layout = super::music_map::layout(data, &used, rom);
    let mut placed = vec![false; data.len()];
    for r in &layout.runs {
        placed[r.data..r.data + r.len].iter_mut().for_each(|b| *b = true);
    }
    // Runs at the place in their 8 KB bank the file has them.
    let dist = |r: &Run| (r.rom as i64 - h.phys_of(r.data) as i64).rem_euclid(0x2000);
    let mut by: HashMap<i64, usize> = HashMap::new();
    layout.runs.iter().for_each(|r| *by.entry(dist(r)).or_default() += r.len);
    let main = by.iter().max_by_key(|e| (*e.1, -*e.0)).map(|e| *e.0);
    let mut aligned = vec![false; data.len()];
    for r in layout.runs.iter().filter(|r| Some(dist(r)) == main) {
        aligned[r.data..r.data + r.len].iter_mut().for_each(|b| *b = true);
    }
    let mut tracks = vec![None; h.songs as usize];
    for &t in &songs {
        let mut p = Player::new(&h, data);
        let mut cpu = Cpu::with(h.init, 0xFF);
        cpu.mpr = h.banks;
        let Some(mut init) = record(&mut p, &mut cpu, h.init, t, false, &placed, &aligned) else { continue };
        // The interrupts of the first frames, until each settles.
        let (mut vbl, mut tim): (Vec<Vec<Op>>, Vec<Vec<Op>>) = (Vec::new(), Vec::new());
        let mut due = 0.0;
        let mut one_off = Vec::new();
        for _ in 0..SETTLE_FRAMES {
            due += p.timer_rate();
            let n = due as u32;
            due -= n as f64;
            for _ in 0..n {
                let to = vector(&mut p, &cpu, VECTOR_TIMER);
                let a = cpu.a;
                if let Some(s) = record(&mut p, &mut cpu, to, a, true, &placed, &aligned) {
                    one_off.push(s.clone());
                    tim.push(s);
                }
            }
            if p.vblank() {
                let to = vector(&mut p, &cpu, VECTOR_IRQ1);
                let a = cpu.a;
                if let Some(s) = record(&mut p, &mut cpu, to, a, true, &placed, &aligned) {
                    one_off.push(s.clone());
                    vbl.push(s);
                }
            }
        }
        let vblank = vbl.last().cloned().unwrap_or_default();
        let timer = tim.last().cloned().unwrap_or_default();
        // The steps before both settle are the track's start, played once after its init.
        let settled = one_off.iter().rposition(|s| !same_steps(s, &vblank) && !same_steps(s, &timer)).map_or(0, |i| i + 1);
        one_off[..settled].iter().for_each(|s| init.extend(s.iter().cloned()));
        // (The interrupt controller and timer state the init set up go with it.)
        init.push(Op::Poke(0x1F_F402, p.disables));
        init.push(Op::Poke(0x1F_EC00, p.timer_reload));
        init.push(Op::Poke(0x1F_EC01, p.timer_on as u8));
        init.push(Op::Poke(0x1F_E000, 5));
        init.push(Op::Poke(0x1F_E002, p.vdc_control));
        tracks[t as usize] = Some(Track { init, vblank, timer });
    }
    Some(Map { header: h, data_len: data.len(), runs: layout.runs, tracks, ways: Vec::new() })
}

/// Where in a HuCard of `len` bytes the console finds bank `bank` ($00-$7F): a 384 KB card
/// is wired as 256 KB at $00 and 128 KB at $40, others repeat.
fn rom_bank_at(len: usize, bank: u32) -> Option<usize> {
    let banks = len / 0x2000;
    if banks == 0 {
        return None;
    }
    let b = bank as usize;
    let at = if len == 0x60000 {
        if b < 0x40 {
            (b & 0x1F) << 13
        } else {
            0x40000 + ((b & 0x0F) << 13)
        }
    } else {
        (b % banks) << 13
    };
    (at + 0x2000 <= len).then_some(at)
}

/// The bank the console sees ROM offset `off` of a `len`-byte HuCard at.
fn bank_of_rom(len: usize, off: usize) -> u8 {
    let b = off >> 13;
    if len == 0x60000 && b >= 0x20 {
        (0x40 + (b - 0x20)) as u8
    } else {
        b as u8
    }
}

/// Per file bank (the console's bank number the file has its data at), the game's bank
/// holding most of it at the same place in the bank.
fn game_banks(m: &Map, rom_len: usize) -> HashMap<u8, u8> {
    let mut count: HashMap<(u8, u8), usize> = HashMap::new();
    for r in &m.runs {
        let phys = m.header.addr as usize + r.data;
        if (phys ^ r.rom) & 0x1FFF != 0 {
            continue;
        }
        let mut k = 0;
        while k < r.len {
            let n = (0x2000 - ((phys + k) & 0x1FFF)).min(r.len - k);
            *count.entry((((phys + k) >> 13) as u8, bank_of_rom(rom_len, r.rom + k))).or_default() += n;
            k += n;
        }
    }
    let mut best: HashMap<u8, (u8, usize)> = HashMap::new();
    for ((f, g), n) in count {
        let e = best.entry(f).or_insert((g, 0));
        if n > e.1 || (n == e.1 && g < e.0) {
            *e = (g, n);
        }
    }
    best.into_iter().map(|(f, (g, _))| (f, g)).collect()
}

/// A script on the game's own banks: calls with their banks turned into the game's, and no
/// writes into the ROM's space.
fn on_board(ops: &[Op], banks: &HashMap<u8, u8>) -> Vec<Op> {
    ops.iter()
        .filter(|o| !matches!(o, Op::Poke(a, _) if *a < 0x10_0000))
        .map(|o| match *o {
            Op::Call { addr, mut regs, irq, src } => {
                for b in regs.mpr.iter_mut().filter(|b| **b < 0x80) {
                    *b = banks.get(b).copied().unwrap_or(*b);
                }
                Op::Call { addr, regs, irq, src }
            }
            ref o => o.clone(),
        })
        .collect()
}

fn rebuilt(m: &Map, rom: &[u8], fill: Fill) -> Vec<u8> {
    let mut data = match fill {
        Fill::File(d) => d.to_vec(),
        _ => vec![0u8; m.data_len],
    };
    data.resize(m.data_len, 0);
    for r in &m.runs {
        if let Some(src) = rom.get(r.rom..r.rom + r.len) {
            data[r.data..r.data + r.len].copy_from_slice(src);
        }
    }
    if fill == Fill::Bridged {
        for w in m.runs.windows(2) {
            let (a, b) = (w[0], w[1]);
            if b.data > a.data + a.len && b.rom as isize - b.data as isize == a.rom as isize - a.data as isize {
                let from = a.rom + a.len;
                if let Some(src) = rom.get(from..from + (b.data - a.data - a.len)) {
                    data[a.data + a.len..b.data].copy_from_slice(src);
                }
            }
        }
    }
    data
}

/// Master clocks in a 1/60 s frame.
const MASTER: u32 = 21_477_270;
const FRAME_CLOCKS: u32 = MASTER / 60;

/// A track playing from the ROM by its map: each frame's sound writes.
pub struct LayoutSong {
    p: Player,
    cpu: Cpu,
    track: Track,
    due: f64,
    first: Vec<Write>,
}

impl LayoutSong {
    pub fn open(m: &Map, rom: &[u8], song: u8, fill: Fill) -> Option<Self> {
        let mut track = m.tracks.get(song as usize)?.clone()?;
        let mut p = if fill == Fill::Board {
            let banks = game_banks(m, rom.len());
            track = Track { init: on_board(&track.init, &banks), vblank: on_board(&track.vblank, &banks), timer: on_board(&track.timer, &banks) };
            Player::board(&m.header, rom)
        } else {
            Player::new(&m.header, &rebuilt(m, rom, fill))
        };
        let mut cpu = Cpu::with(m.header.init, 0xFF);
        if !replay(&mut p, &mut cpu, &track.init) {
            return None;
        }
        let first = std::mem::take(&mut p.writes);
        Some(Self { p, cpu, track, due: 0.0, first })
    }

    pub fn frame(&mut self) -> Option<Vec<Write>> {
        self.due += self.p.timer_rate();
        let n = self.due as u32;
        self.due -= n as f64;
        for k in 0..n {
            self.p.at = k * FRAME_CLOCKS / n.max(1);
            if !replay(&mut self.p, &mut self.cpu, &self.track.timer) {
                return None;
            }
        }
        if self.p.vblank() {
            self.p.at = FRAME_CLOCKS - 1;
            if !replay(&mut self.p, &mut self.cpu, &self.track.vblank) {
                return None;
            }
        }
        self.p.at = 0;
        let mut f = std::mem::take(&mut self.first);
        f.append(&mut self.p.writes);
        Some(f)
    }
}

pub fn map_track_writes(m: &Map, rom: &[u8], song: u8, frames: usize, fill: Fill) -> Option<Vec<Vec<Write>>> {
    let mut s = LayoutSong::open(m, rom, song, fill)?;
    Some((0..frames).map_while(|_| s.frame()).collect())
}

/// How a map plays the HES's songs: per song number, how alike its melody is to the file's
/// (None for one the map leaves out).
pub fn map_check(hes: &[u8], m: &Map, rom: &[u8], frames: usize, fill: Fill) -> Vec<Option<f64>> {
    (0..m.header.songs)
        .map(|t| {
            m.tracks.get(t as usize)?.as_ref()?;
            let real = melody(&track_writes(hes, t, frames)?);
            if real.len() < music_search::MIN_MELODY {
                return None;
            }
            let mine = map_track_writes(m, rom, t, frames, fill).map(|w| melody(&w)).unwrap_or_default();
            Some(music_search::alike(&real, &mine))
        })
        .collect()
}

pub fn choose_ways(hes: &[u8], m: &mut Map, rom: &[u8], frames: usize, right: f64) {
    let bridged = map_check(hes, m, rom, frames, Fill::Bridged);
    let missed = bridged.iter().any(|b| b.is_some_and(|b| b < right));
    let board = if missed { map_check(hes, m, rom, frames, Fill::Board) } else { Vec::new() };
    let zeros = if missed { map_check(hes, m, rom, frames, Fill::Zeros) } else { Vec::new() };
    m.ways = (0..bridged.len())
        .map(|i| {
            let score = |v: &Vec<Option<f64>>| v.get(i).copied().flatten().unwrap_or(0.0);
            if score(&bridged) >= right {
                Way::Layout
            } else if score(&board) >= right {
                Way::Board
            } else if score(&zeros) >= right {
                Way::Zeros
            } else {
                Way::None
            }
        })
        .collect();
}

pub const MAP_RIGHT: f64 = 0.9;
const WAY_FRAMES: usize = 600;

/// A HuCard image without the 512-byte header some dumps carry.
pub fn program(rom: &[u8]) -> &[u8] {
    if rom.len() % 0x2000 == 512 {
        &rom[512..]
    } else {
        rom
    }
}

pub fn build_map(hes: &[u8], rom: &[u8]) -> Option<Map> {
    let rom = program(rom);
    let mut m = map(hes, rom)?;
    choose_ways(hes, &mut m, rom, WAY_FRAMES, MAP_RIGHT);
    Some(m)
}

pub fn play_map(m: &Map, rom: &[u8], song: u8, frames: usize) -> Option<Vec<Vec<Write>>> {
    let fill = match m.ways.get(song as usize)? {
        Way::Layout => Fill::Bridged,
        Way::Zeros => Fill::Zeros,
        Way::Board => Fill::Board,
        Way::None => return None,
    };
    map_track_writes(m, program(rom), song, frames, fill)
}

/// The PSG's clock.
const PSG_CLOCK: u32 = 3_579_545;

/// A track from the user's ROM by its map, as sound (stereo).
pub struct MapSong {
    song: LayoutSong,
    chips: super::vgm_chips::Chips,
}

impl MapSong {
    pub fn open(m: &Map, rom: &[u8], song: u8, rate: u32) -> Result<Self, String> {
        use super::vgm_chips::{Chips, Kind};
        let fill = match m.ways.get(song as usize) {
            Some(Way::Layout) => Fill::Bridged,
            Some(Way::Zeros) => Fill::Zeros,
            Some(Way::Board) => Fill::Board,
            _ => return Err("the map has no way to play this song".into()),
        };
        let song = LayoutSong::open(m, program(rom), song, fill).ok_or("the song didn't start")?;
        Ok(Self { song, chips: Chips::new(rate, MASTER as f64, &[(Kind::Huc6280, PSG_CLOCK)])? })
    }

    pub fn frame_rate(&self) -> f64 {
        60.0
    }

    pub fn frame(&mut self, out: &mut Vec<i16>) -> Option<Vec<Write>> {
        let w = self.song.frame()?;
        for x in &w {
            self.chips.write(x.at.min(FRAME_CLOCKS - 1), 0, x.port as u8, x.value as u8);
        }
        self.chips.end_frame(FRAME_CLOCKS, out);
        Some(w)
    }
}

impl Map {
    pub fn encode(&self) -> String {
        let h = &self.header;
        let hex = |v: &[u8]| v.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let ops = |ops: &[Op]| {
            ops.iter()
                .map(|o| match *o {
                    Op::Poke(a, v) => format!("P{a:x}:{v:x}"),
                    Op::Push(v) => format!("U{v:x}"),
                    Op::Call { addr, regs: r, irq, src } => format!("C{addr:x}:{:x}:{:x}:{:x}:{}:{}:{src:x}", r.a, r.x, r.y, hex(&r.mpr), irq as u8),
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        let mut out = vec![
            "hesmap 1".to_string(),
            format!("h {:x} {} {:x} {}", h.init, hex(&h.banks), h.addr, h.songs),
            format!("d {:x}", self.data_len),
            format!("r {}", self.runs.iter().map(|r| format!("{:x}:{:x}:{:x}", r.data, r.len, r.rom)).collect::<Vec<_>>().join(" ")),
        ];
        for (i, t) in self.tracks.iter().enumerate() {
            if let Some(t) = t {
                out.push(format!("i {i} {}", ops(&t.init)));
                out.push(format!("v {i} {}", ops(&t.vblank)));
                out.push(format!("m {i} {}", ops(&t.timer)));
            }
        }
        out.push(format!("s {}", self.ways.iter().map(|w| match w { Way::Layout => 'l', Way::Zeros => 'z', Way::Board => 'b', Way::None => '-' }).collect::<String>()));
        out.join("\n")
    }

    pub fn decode(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != "hesmap 1" {
            return None;
        }
        let num = |t: &str| u32::from_str_radix(t, 16).ok();
        let bytes = |t: &str| -> Option<[u8; 8]> {
            let mut b = [0u8; 8];
            if t.len() != 16 {
                return None;
            }
            for (i, c) in t.as_bytes().chunks(2).enumerate() {
                b[i] = u8::from_str_radix(std::str::from_utf8(c).ok()?, 16).ok()?;
            }
            Some(b)
        };
        let ops = |rest: &[&str]| -> Option<Vec<Op>> {
            rest.iter()
                .map(|t| {
                    let (k, v) = t.split_at(1);
                    let f: Vec<&str> = v.split(':').collect();
                    Some(match (k, f.as_slice()) {
                        ("P", [a, v]) => Op::Poke(num(a)?, num(v)? as u8),
                        ("U", [v]) => Op::Push(num(v)? as u8),
                        ("C", [addr, a, x, y, mpr, irq, src]) => Op::Call {
                            addr: num(addr)? as u16,
                            regs: Regs { a: num(a)? as u8, x: num(x)? as u8, y: num(y)? as u8, mpr: bytes(mpr)? },
                            irq: *irq == "1",
                            src: num(src)?,
                        },
                        _ => return None,
                    })
                })
                .collect()
        };
        let mut m = Map { header: Header { init: 0, banks: [0; 8], addr: 0, songs: 0 }, data_len: 0, runs: Vec::new(), tracks: Vec::new(), ways: Vec::new() };
        for line in lines {
            let f: Vec<&str> = line.split(' ').filter(|t| !t.is_empty()).collect();
            match f.first().copied() {
                Some("h") if f.len() == 5 => {
                    m.header = Header { init: num(f[1])? as u16, banks: bytes(f[2])?, addr: num(f[3])?, songs: f[4].parse().ok()? };
                    m.tracks = vec![None; m.header.songs as usize];
                }
                Some("d") => m.data_len = num(f.get(1)?)? as usize,
                Some("r") => {
                    for t in &f[1..] {
                        let v: Vec<u32> = t.split(':').map(num).collect::<Option<_>>()?;
                        let [data, len, rom] = v[..] else { return None };
                        m.runs.push(Run { data: data as usize, len: len as usize, rom: rom as usize });
                    }
                }
                Some(k @ ("i" | "v" | "m")) => {
                    let i: usize = f.get(1)?.parse().ok()?;
                    let list = ops(&f[2..])?;
                    let t = m.tracks.get_mut(i)?.get_or_insert_with(Track::default);
                    match k {
                        "i" => t.init = list,
                        "v" => t.vblank = list,
                        _ => t.timer = list,
                    }
                }
                Some("s") => {
                    m.ways = f.get(1).unwrap_or(&"").chars().map(|c| match c { 'l' => Way::Layout, 'z' => Way::Zeros, 'b' => Way::Board, _ => Way::None }).collect();
                }
                _ => {}
            }
        }
        Some(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny game: its sound code in ROM bank 3 (seen at $8000 through MPR4), an init that
    /// stores the song number, and a vertical blank routine that steps channel 0's period by it.
    /// The HES's author wrote their own init (set the bank, poke a variable, call the game's
    /// init, turn the vertical blank interrupt on) and an interrupt handler that calls the
    /// game's routine.
    fn game_and_file() -> (Vec<u8>, Vec<u8>) {
        let mut rom = vec![0u8; 0x10000];
        let mut x = 5u32;
        for b in rom.iter_mut() {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            *b = (x >> 16) as u8;
        }
        let bank3 = 0x6000;
        // init ($8000): STA $2000; STZ $2001; RTS
        let init = [0x8D, 0x00, 0x20, 0x9C, 0x01, 0x20, 0x60];
        // vbl ($8100): LDA $2001; CLC; ADC $2000; STA $2001; STZ $0800; STA $0802; STZ $0803; RTS
        let vbl = [0xAD, 0x01, 0x20, 0x18, 0x6D, 0x00, 0x20, 0x8D, 0x01, 0x20, 0x9C, 0x00, 0x08, 0x8D, 0x02, 0x08, 0x9C, 0x03, 0x08, 0x60];
        rom[bank3..bank3 + init.len()].copy_from_slice(&init);
        rom[bank3 + 0x100..bank3 + 0x100 + vbl.len()].copy_from_slice(&vbl);
        // The file: its own code in bank $10 (seen at $E000), the game's bank 3 as bank $11.
        let mut f = vec![0u8; 0x20];
        f[..4].copy_from_slice(b"HESM");
        f[6..8].copy_from_slice(&0xE000u16.to_le_bytes());
        f[8..16].copy_from_slice(&[0xFF, 0xF8, 0, 0, 0x11, 0, 0, 0x10]);
        f[0x18..0x1C].copy_from_slice(&(0x10u32 * 0x2000).to_le_bytes());
        let mut data = vec![0u8; 0x4000];
        // init ($E000): PHA; LDA #$40; STA $2002; PLA; ASL A; CLC; ADC #$20; JSR $8000;
        // LDA #5; STA $0000; LDA #8; STA $0002; LDA #5; STA $1402; RTS
        let my_init = [
            0x48, 0xA9, 0x40, 0x8D, 0x02, 0x20, 0x68, 0x0A, 0x18, 0x69, 0x20, 0x20, 0x00, 0x80, 0xA9, 0x05, 0x8D, 0x00, 0x00, 0xA9, 0x08, 0x8D, 0x02, 0x00, 0xA9, 0x05, 0x8D,
            0x02, 0x14, 0x60,
        ];
        data[..my_init.len()].copy_from_slice(&my_init);
        // irq1 ($E100): PHA; JSR $8100; PLA; RTI
        data[0x100..0x106].copy_from_slice(&[0x48, 0x20, 0x00, 0x81, 0x68, 0x40]);
        // Vectors: IRQ1 at $FFF8, the timer at $FFFA.
        data[0x1FF8..0x1FFC].copy_from_slice(&[0x00, 0xE1, 0x00, 0xE1]);
        data[0x2000..0x4000].copy_from_slice(&rom[bank3..bank3 + 0x2000]);
        f.extend(data);
        (rom, f)
    }

    #[test]
    fn a_map_plays_the_files_songs_from_the_rom_and_round_trips() {
        let (rom, f) = game_and_file();
        let mut m = map(&f, &rom).expect("map");
        let t1 = m.tracks[1].as_ref().expect("song 1");
        assert!(t1.init.contains(&Op::Poke(0x1F_0002, 0x40)), "{:?}", t1.init);
        assert!(t1.init.iter().any(|o| matches!(o, Op::Call { addr: 0x8000, regs, .. } if regs.a == 0x22 && regs.mpr[4] == 0x11)), "{:?}", t1.init);
        assert!(t1.vblank.iter().any(|o| matches!(o, Op::Call { addr: 0x8100, .. })), "{:?}", t1.vblank);
        let check = map_check(&f, &m, &rom, 60, Fill::Zeros);
        assert!(check.iter().flatten().count() >= 3 && check.iter().flatten().all(|&a| a > 0.99), "{check:?}");
        // On the game's own banks: the file's bank $11 is the game's bank 3.
        assert_eq!(game_banks(&m, rom.len()).get(&0x11), Some(&3));
        let board = map_check(&f, &m, &rom, 60, Fill::Board);
        assert!(board.iter().flatten().count() >= 3 && board.iter().flatten().all(|&a| a > 0.99), "{board:?}");
        choose_ways(&f, &mut m, &rom, 60, MAP_RIGHT);
        let text = m.encode();
        let back = Map::decode(&text).unwrap();
        assert_eq!(back.encode(), text);
        for t in 1..4 {
            let mine = play_map(&back, &rom, t, 60).map(|w| melody(&w));
            let real = track_writes(&f, t, 60).map(|w| melody(&w));
            assert_eq!(mine, real, "song {t}");
        }
    }
}
