//! What an NSF file says about its game's sound code, as a [`Hint`] for the music search.
//!
//! An NSF carries the game's own sound driver at the addresses the game runs it from (6502
//! code isn't relocatable), with a small init and play routine written for the file or taken from
//! the game. Their addresses, and the routines they call, point the search straight at the
//! game's sound routine and the routine that starts a song, so it no longer has to find them
//! in the boot alone. The music still plays from the ROM: the NSF is only read for addresses.

use super::m6502::{Bus, Cpu};
use std::collections::{HashMap, HashSet};

use super::music_search::{self, hint_routines, Hint, Write};

/// Code bytes compared at each address, in the NSF and in the game.
const CODE_BYTES: usize = 12;
/// How far a routine is read for the routines it calls.
const SCAN_BYTES: usize = 96;

/// An NSF's header and its program, laid out as the 6502 sees it at start.
struct Nsf {
    songs: u8,
    init: u16,
    play: u16,
    /// $6000-$FFFF; `None` where the file puts nothing.
    image: Vec<Option<u8>>,
}

fn parse(nsf: &[u8]) -> Option<Nsf> {
    if nsf.len() < 0x80 || &nsf[..5] != b"NESM\x1a" {
        return None;
    }
    let word = |at: usize| u16::from_le_bytes([nsf[at], nsf[at + 1]]);
    let (load, init, play) = (word(0x08), word(0x0A), word(0x0C));
    let banks = &nsf[0x70..0x78];
    let data = &nsf[0x80..];
    let mut image = vec![None; 0xA000];
    if banks.iter().all(|&b| b == 0) {
        if load < 0x6000 {
            return None;
        }
        for (i, &b) in data.iter().enumerate() {
            let at = load as usize - 0x6000 + i;
            if at >= image.len() {
                break;
            }
            image[at] = Some(b);
        }
    } else {
        // Banked: 4 KB banks of the data, padded in front to `load`'s place in its bank. A
        // Disk System NSF also banks $6000-$7FFF (from 0x76, 0x77).
        let pad = (load & 0xFFF) as usize;
        let fds = nsf[0x7B] & 4 != 0;
        let slots: Vec<(usize, u8)> = (0..8)
            .map(|i| (0x2000 + i * 0x1000, banks[i]))
            .chain(fds.then(|| [(0, banks[6]), (0x1000, banks[7])]).into_iter().flatten())
            .collect();
        for (base, bank) in slots {
            for i in 0..0x1000 {
                let src = (bank as usize * 0x1000 + i).checked_sub(pad);
                image[base + i] = src.and_then(|s| data.get(s)).copied();
            }
        }
    }
    Some(Nsf { songs: nsf[0x06], init, play, image })
}

impl Nsf {
    fn byte(&self, addr: u16) -> Option<u8> {
        self.image.get((addr as usize).checked_sub(0x6000)?).copied().flatten()
    }

    fn code(&self, addr: u16) -> Option<Vec<u8>> {
        (0..CODE_BYTES as u16).map(|i| self.byte(addr.wrapping_add(i))).collect()
    }

    /// `from`, then the routines it calls or jumps to, as far as straight-line reading finds
    /// them.
    fn routines(&self, from: u16) -> Vec<(u32, Vec<u8>)> {
        hint_routines(from, |a| self.calls_in(a), |a| self.code(a))
    }

    /// The JSR and JMP targets in the straight-line code from `addr`, in ROM space.
    fn calls_in(&self, addr: u16) -> Vec<u16> {
        let mut out = Vec::new();
        let mut pc = addr;
        while (pc.wrapping_sub(addr) as usize) < SCAN_BYTES {
            let Some(op) = self.byte(pc) else { break };
            let target = || {
                Some(u16::from_le_bytes([
                    self.byte(pc.wrapping_add(1))?,
                    self.byte(pc.wrapping_add(2))?,
                ]))
            };
            match op {
                0x20 | 0x4C => {
                    if let Some(t) = target().filter(|&t| t >= 0x6000) {
                        out.push(t);
                    }
                    if op == 0x4C {
                        break;
                    }
                }
                0x00 | 0x40 | 0x60 | 0x6C => break,
                _ => {}
            }
            pc = pc.wrapping_add(length(op));
        }
        out
    }
}

/// A 6502 instruction's length in bytes, from its opcode (unofficial ones by their pattern).
fn length(op: u8) -> u16 {
    let (bbb, cc) = ((op >> 2) & 7, op & 3);
    match (cc, bbb) {
        (0, 0) => match op {
            0x20 => 3,
            0x00 | 0x40 | 0x60 => 1,
            _ => 2,
        },
        (_, 3) | (_, 7) => 3,
        (1 | 3, 6) => 3,
        (0, 2) | (0, 6) | (2, 2) | (2, 6) | (2, 4) => 1,
        _ => 2,
    }
}

/// The search hint an NSF gives: its play routine and init routine with what they call, and
/// its song count. `None` when it isn't an NSF.
pub fn hint(nsf: &[u8]) -> Option<Hint> {
    let n = parse(nsf)?;
    let start = n.routines(n.init);
    // The number each track hands the routines init calls: where init looks the game's song
    // id up in a table of its own, the ids differ from the track numbers.
    let mut ids = vec![(n.init as u32, 0, (0..n.songs).collect())];
    let inner: Vec<u16> = start.iter().skip(1).map(|r| r.0 as u16).collect();
    for (addr, seen) in inner.iter().zip(entries(nsf, n.songs, &inner)) {
        let got: Vec<[u8; 3]> = seen.into_iter().flatten().collect();
        if got.len() * 2 < n.songs as usize {
            continue;
        }
        // The first register that changes with the track carries the id.
        if let Some(reg) = (0..3).find(|&r| got.iter().any(|g| g[r] != got[0][r])) {
            ids.push((*addr as u32, reg as u8, got.iter().map(|g| g[reg]).collect()));
        }
    }
    Some(Hint {
        play: n.routines(n.play),
        start,
        songs: n.songs,
        ids,
        tracks: (0..n.songs)
            .map(|t| track_writes(nsf, t, music_search::HINT_TRACK_FRAMES).map(|f| melody(&f)).unwrap_or_default())
            .collect(),
        melody: Some(melody),
    })
}

/// An NSF, run on its own (a bare 6502 with RAM, the NSF's banks and nothing else), for the
/// sound writes of its tracks: the known answer a ROM's found songs are checked against.
struct Player {
    ram: [u8; 0x800],
    /// $6000-$FFFF.
    mem: Vec<u8>,
    data: Vec<u8>,
    banked: bool,
    fds: bool,
    writes: Vec<Write>,
    /// For each byte of `mem`, 1 + its offset in the file's data (after the header); 0 for
    /// none.
    src: Vec<u32>,
    /// Bytes before the data in bank 0 (banked files).
    pad: usize,
    /// When set, marks each data byte read ([`used_bytes`]).
    used: Option<Vec<bool>>,
    /// When set, the writes made, for a start script ([`record`]).
    log: Option<Vec<(u16, u8)>>,
}

impl Bus for Player {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x1FFF => self.ram[addr as usize & 0x7FF],
            0x6000..=0xFFFF => {
                let i = addr as usize - 0x6000;
                if let (Some(used), s @ 1..) = (self.used.as_mut(), self.src[i]) {
                    if let Some(u) = used.get_mut(s as usize - 1) {
                        *u = true;
                    }
                }
                self.mem[i]
            }
            _ => 0,
        }
    }

    fn write(&mut self, addr: u16, v: u8) {
        if let Some(log) = self.log.as_mut() {
            log.push((addr, v));
        }
        match addr {
            0x0000..=0x1FFF => self.ram[addr as usize & 0x7FF] = v,
            0x4000..=0x4013 | 0x4015 | 0x4040..=0x408A => self.writes.push(Write {
                at: 0,
                port: addr as u32,
                value: v as u16,
            }),
            0x5FF6..=0x5FFF if self.banked => {
                let slot = addr as usize - 0x5FF6;
                // $5FF6/7 bank $6000/$7000 (Disk System NSFs only); $5FF8-F bank $8000-$F000.
                if slot >= 2 || self.fds {
                    self.bank(slot, v);
                }
            }
            0x6000..=0x7FFF => {
                self.mem[addr as usize - 0x6000] = v;
                self.src[addr as usize - 0x6000] = 0;
            }
            0x8000..=0xFFFF if self.fds => {
                self.mem[addr as usize - 0x6000] = v;
                self.src[addr as usize - 0x6000] = 0;
            }
            _ => {}
        }
    }
}

impl Player {
    /// Puts 4 KB bank `bank` of the (padded) data at slot `slot` ($6000 + slot * 4 KB).
    fn bank(&mut self, slot: usize, bank: u8) {
        let from = bank as usize * 0x1000;
        for i in 0..0x1000 {
            self.mem[slot * 0x1000 + i] = self.data.get(from + i).copied().unwrap_or(0);
            self.src[slot * 0x1000 + i] = match (from + i).checked_sub(self.pad) {
                Some(d) if from + i < self.data.len() => d as u32 + 1,
                _ => 0,
            };
        }
    }

    /// Runs the routine at `pc` with A and X until it returns; false when it doesn't.
    fn call(&mut self, cpu: &mut Cpu, pc: u16, a: u8, x: u8) -> bool {
        self.call_watched(cpu, pc, a, x, &mut |_| {})
    }

    /// [`Self::call`], showing `watch` the CPU before each instruction.
    fn call_watched(&mut self, cpu: &mut Cpu, pc: u16, a: u8, x: u8, watch: &mut dyn FnMut(&Cpu)) -> bool {
        self.call_with(cpu, pc, a, x, &[], watch)
    }

    /// [`Self::call_watched`], with `pushed` on the stack first (in the order pushed), for
    /// the routine to pull.
    fn call_with(&mut self, cpu: &mut Cpu, pc: u16, a: u8, x: u8, pushed: &[u8], watch: &mut dyn FnMut(&Cpu)) -> bool {
        // A return address on the stack the routine's RTS takes (never run).
        self.ram[0x1FF] = 0x5F;
        self.ram[0x1FE] = 0xF5;
        cpu.s = 0xFD;
        for &v in pushed {
            self.ram[0x100 | cpu.s as usize] = v;
            cpu.s = cpu.s.wrapping_sub(1);
        }
        cpu.pc = pc;
        cpu.a = a;
        cpu.x = x;
        cpu.calls = vec![(pc, 0xFF)];
        let mut used = 0u32;
        while !cpu.calls.is_empty() {
            if used > 400_000 || cpu.jammed {
                return false;
            }
            watch(cpu);
            used += cpu.step(self);
        }
        true
    }
}

/// Track `song` (from 0) of an NSF, played `frames` frames: each frame's sound writes. `None`
/// when it isn't an NSF or its code doesn't return.
pub fn track_writes(nsf: &[u8], song: u8, frames: usize) -> Option<Vec<Vec<Write>>> {
    let (mut p, init, play) = load(nsf)?;
    let mut cpu = Cpu::with(init, 0xFD);
    p.write(0x4015, 0x0F);
    if !p.call(&mut cpu, init, song, 0) {
        return None;
    }
    let mut out = Vec::with_capacity(frames);
    let mut first = std::mem::take(&mut p.writes);
    for _ in 0..frames {
        if !p.call(&mut cpu, play, 0, 0) {
            break;
        }
        let mut f = std::mem::take(&mut first);
        f.append(&mut p.writes);
        out.push(f);
    }
    Some(out)
}

/// Which bytes of the file's data (after the header) its tracks read, running each track's
/// init and `frames` frames of play: the code and song data a map of the game must place.
pub fn used_bytes(nsf: &[u8], frames: usize) -> Option<Vec<bool>> {
    let songs = *nsf.get(6)?;
    let mut used = vec![false; nsf.len().checked_sub(0x80)?];
    for t in 0..songs {
        let (mut p, init, play) = load(nsf)?;
        p.used = Some(std::mem::take(&mut used));
        let mut cpu = Cpu::with(init, 0xFD);
        p.write(0x4015, 0x0F);
        if p.call(&mut cpu, init, t, 0) {
            for _ in 0..frames {
                if !p.call(&mut cpu, play, 0, 0) {
                    break;
                }
            }
        }
        used = p.used.take().unwrap_or_default();
    }
    Some(used)
}

/// For each routine in `routines`, the registers (A, X, Y) it is first entered with while the
/// NSF's init starts each track: `None` for a track that never enters it.
fn entries(nsf: &[u8], songs: u8, routines: &[u16]) -> Vec<Vec<Option<[u8; 3]>>> {
    let mut out = vec![vec![None; songs as usize]; routines.len()];
    for t in 0..songs {
        let Some((mut p, init, _)) = load(nsf) else { break };
        let mut cpu = Cpu::with(init, 0xFD);
        p.call_watched(&mut cpu, init, t, 0, &mut |c| {
            if let Some(i) = routines.iter().position(|&r| r == c.pc) {
                out[i][t as usize].get_or_insert([c.a, c.x, c.y]);
            }
        });
    }
    out
}

/// An NSF's player, loaded and banked as at start, with its init and play addresses.
fn load(nsf: &[u8]) -> Option<(Player, u16, u16)> {
    let h = Header::read(nsf)?;
    Some((player(&h, &nsf[0x80..])?, h.init, h.play))
}

/// What a map keeps of an NSF's header: where its data loads, its starting banks, whether it
/// is for the Disk System, its song count (and its init and play, for building the map).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub load: u16,
    pub init: u16,
    pub play: u16,
    pub banks: [u8; 8],
    pub fds: bool,
    pub songs: u8,
}

impl Header {
    fn read(nsf: &[u8]) -> Option<Self> {
        if nsf.len() < 0x80 || &nsf[..5] != b"NESM\x1a" {
            return None;
        }
        let word = |at: usize| u16::from_le_bytes([nsf[at], nsf[at + 1]]);
        Some(Self {
            load: word(0x08),
            init: word(0x0A),
            play: word(0x0C),
            banks: nsf[0x70..0x78].try_into().ok()?,
            fds: nsf[0x7B] & 4 != 0,
            songs: nsf[6],
        })
    }
}

/// A player for `data` laid out as header `h` says.
fn player(h: &Header, data: &[u8]) -> Option<Player> {
    let (load, banks, fds) = (h.load, h.banks, h.fds);
    let banked = banks.iter().any(|&b| b != 0);
    let mut p = Player {
        ram: [0; 0x800],
        mem: vec![0; 0xA000],
        data: Vec::new(),
        banked,
        fds,
        writes: Vec::new(),
        src: vec![0; 0xA000],
        pad: (load & 0xFFF) as usize,
        used: None,
        log: None,
    };
    if banked {
        p.data = vec![0; (load & 0xFFF) as usize];
        p.data.extend_from_slice(data);
        for (i, &b) in banks.iter().enumerate() {
            p.bank(i + 2, b);
        }
        if fds {
            p.bank(0, banks[6]);
            p.bank(1, banks[7]);
        }
    } else {
        let at = (load as usize).checked_sub(0x6000)?;
        for (i, &b) in data.iter().enumerate() {
            if let Some(m) = p.mem.get_mut(at + i) {
                *m = b;
                p.src[at + i] = i as u32 + 1;
            }
        }
    }
    Some(p)
}

/// A melody's fingerprint (see [`music_search::melody`]) on the NES's tone channels: pulse 1
/// and 2, and the triangle.
pub fn melody(frames: &[Vec<Write>]) -> HashSet<u64> {
    music_search::melody(frames, &[(0x4002, 0x4003), (0x4006, 0x4007), (0x400A, 0x400B)])
}

pub use music_search::alike;

/// One step of a start script: what the file's own init (or play) does, in terms of the
/// game's code: a write it makes itself, or a call into the game's routine at `addr`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Poke(u16, u8),
    /// A byte the file's code left on the stack for the game's routine (the next call) to
    /// pull.
    Push(u8),
    /// `src`: 1 + where in the file's data the routine starts (0: not known), to find it in
    /// the ROM.
    Call { addr: u16, a: u8, x: u8, y: u8, src: u32 },
}

/// A game's music map, from a matched NSF and the ROM: the NSF's needed data as ROM ranges,
/// and per track the start script that replaces the file's own init (play's when play isn't
/// the game's own routine). Only numbers: no byte of the game or of the file.
#[derive(Debug, Clone)]
pub struct Map {
    pub header: Header,
    /// Bytes of the file's data, and where its runs are in the ROM's program data.
    pub data_len: usize,
    pub runs: Vec<super::music_map::Run>,
    pub play: Vec<Op>,
    pub tracks: Vec<Option<Vec<Op>>>,
    /// Per track, its play steps where they differ from `play` (a file's play that sets the
    /// banks of the track playing).
    pub plays: Vec<Option<Vec<Op>>>,
    /// When the file was made from another version of the game whose sound code sits at
    /// other addresses (most runs at one distance in the CPU's view, not 0): that distance,
    /// and the game's own 8 KB windows ($6000-$FFFF base, offset in the ROM) to run it in.
    pub moved: Option<(i32, Vec<(u16, usize)>)>,
    /// The moved driver tried, if any: its distance, and the file's tracks' summed likeness
    /// without and with it.
    pub tried: Option<(i32, f64, f64)>,
}

impl Map {
    /// The play steps of track `song`.
    pub fn play_of(&self, song: u8) -> &[Op] {
        self.plays.get(song as usize).and_then(|p| p.as_deref()).unwrap_or(&self.play)
    }
}

/// Runs the routine at `pc` and records it as a start script: while the code running isn't
/// the game's, its writes are pokes; an entry into the game's code is a call, run through
/// without recording. The game's code is what the ROM holds (`placed`): entered by a jump, a
/// call or a return, or by running on from a jump target a few bytes before it (a byte the
/// file's author patched there runs as the file's); entered any other way (running on, a
/// branch) only where it lies as the game's banks have it (`aligned`): the file's own code
/// may run on into bytes that happen to match the ROM elsewhere. None when it doesn't
/// return.
fn record(p: &mut Player, cpu: &mut Cpu, pc: u16, a: u8, x: u8, placed: &[bool], aligned: &[bool]) -> Option<Vec<Op>> {
    p.ram[0x1FF] = 0x5F;
    p.ram[0x1FE] = 0xF5;
    cpu.s = 0xFD;
    cpu.pc = pc;
    cpu.a = a;
    cpu.x = x;
    cpu.calls = vec![(pc, 0xFF)];
    let of = |p: &Player, addr: u16, set: &[bool]| {
        let s = addr.checked_sub(0x6000).and_then(|i| p.src.get(i as usize)).copied().unwrap_or(0);
        s > 0 && set.get(s as usize - 1).copied().unwrap_or(false)
    };
    let held = |p: &Player, addr: u16| of(p, addr, placed);
    let mut ops = Vec::new();
    let mut in_game: Option<usize> = None;
    let mut used = 0u32;
    let mut jumped = true;
    // Where the last jump (call, return) outside the game's code landed, and the stack
    // pointer then.
    let mut target: Option<(u16, u8)> = None;
    while !cpu.calls.is_empty() {
        if used > 400_000 || cpu.jammed {
            return None;
        }
        if in_game.is_some_and(|d| cpu.calls.len() < d) {
            in_game = None;
        }
        let at = cpu.pc;
        if in_game.is_none() {
            let near = target.is_some_and(|(t, _)| at > t && at - t <= PATCHED_BYTES);
            let entered = if jumped || near { held(p, at) } else { of(p, at, aligned) };
            if entered {
                // What the file's code pushed since it got here, still on the stack.
                if let Some((_, s)) = target.filter(|t| t.1 > cpu.s) {
                    ops.extend((cpu.s as u16 + 1..=s as u16).rev().map(|i| Op::Push(p.ram[0x100 | i as usize])));
                }
                let src = at.checked_sub(0x6000).and_then(|i| p.src.get(i as usize)).copied().unwrap_or(0);
                ops.push(Op::Call { addr: at, a: cpu.a, x: cpu.x, y: cpu.y, src });
                in_game = Some(cpu.calls.len());
            }
        }
        let op = if at >= 0x6000 { p.mem[at as usize - 0x6000] } else { p.ram[at as usize & 0x7FF] };
        jumped = matches!(op, 0x20 | 0x4C | 0x6C | 0x60 | 0x40);
        p.log = in_game.is_none().then(Vec::new);
        used += cpu.step(p);
        if jumped && in_game.is_none() {
            target = Some((cpu.pc, cpu.s));
        }
        if let Some(log) = p.log.take() {
            // Pushes (JSR, PHA) land in the live stack, at or above S; a store into the rest
            // of the stack page is the file's own variable (some games keep a song request
            // there).
            let live = 0x100 + cpu.s as u16;
            ops.extend(log.into_iter().filter(|&(a, _)| !(a > live && a < 0x200)).map(|(a, v)| Op::Poke(a, v)));
        }
    }
    Some(ops)
}

/// Bytes after a jump target within which running on into the game's code still counts as
/// entering it: the target's own instruction patched by the file's author (it runs as the
/// file's, then the game's code goes on).
const PATCHED_BYTES: u16 = 3;

/// Plays a start script.
fn replay(p: &mut Player, cpu: &mut Cpu, ops: &[Op], shift: i32) -> bool {
    let mut pushed = Vec::new();
    for op in ops {
        match *op {
            Op::Poke(a, v) => p.write(a, v),
            Op::Push(v) => pushed.push(v),
            Op::Call { addr, a, x, y, .. } => {
                cpu.y = y;
                if !p.call_with(cpu, (addr as i32 + shift) as u16, a, x, &std::mem::take(&mut pushed), &mut |_| {}) {
                    return false;
                }
            }
        }
    }
    true
}

/// Frames of play recorded per track: a file's play often takes the start request on its
/// first frame or two, then settles into the same steps every frame.
const PLAY_FRAMES: usize = 6;

/// The music map of a ROM (`prg`, its program data) for the NSF it matches. None when the
/// NSF doesn't run.
pub fn map(nsf: &[u8], prg: &[u8]) -> Option<Map> {
    let h = Header::read(nsf)?;
    let data = &nsf[0x80..];
    let used = used_bytes(nsf, music_search::HINT_TRACK_FRAMES)?;
    let layout = super::music_map::layout(data, &used, prg);
    let mut placed = vec![false; data.len()];
    for r in &layout.runs {
        placed[r.data..r.data + r.len].iter_mut().for_each(|b| *b = true);
    }
    // Runs where the game's banks put them: at the distance (within a 4 KB bank) most of the
    // runs' bytes share.
    let dist = |r: &super::music_map::Run| cpu_of(&h, r.data).map(|a| (r.rom as i64 - a as i64).rem_euclid(0x1000));
    let mut by: HashMap<i64, usize> = HashMap::new();
    layout.runs.iter().for_each(|r| {
        if let Some(d) = dist(r) {
            *by.entry(d).or_default() += r.len;
        }
    });
    let main = by.iter().max_by_key(|e| (*e.1, -*e.0)).map(|e| *e.0);
    let mut aligned = vec![false; data.len()];
    for r in layout.runs.iter().filter(|r| main.is_some() && dist(r) == main) {
        aligned[r.data..r.data + r.len].iter_mut().for_each(|b| *b = true);
    }
    let mut tracks = Vec::new();
    let mut steadies: Vec<Option<Vec<Op>>> = Vec::new();
    let mut play: Option<Vec<Op>> = None;
    for t in 0..h.songs {
        let mut p = player(&h, data)?;
        let mut cpu = Cpu::with(h.init, 0xFD);
        p.write(0x4015, 0x0F);
        let Some(mut script) = record(&mut p, &mut cpu, h.init, t, 0, &placed, &aligned) else {
            tracks.push(None);
            steadies.push(None);
            continue;
        };
        let mut frames = Vec::new();
        for _ in 0..PLAY_FRAMES {
            match record(&mut p, &mut cpu, h.play, 0, 0, &placed, &aligned) {
                Some(f) => frames.push(f),
                None => break,
            }
        }
        // The steps play repeats (the last frame's); the frames before that differ are this
        // track's one-off start, played once after its init.
        if let Some(steady) = frames.last().cloned() {
            let settled = frames.iter().rposition(|f| *f != steady).map_or(0, |i| i + 1);
            frames[..settled].iter().for_each(|f| script.extend(f.iter().cloned()));
            play.get_or_insert(steady.clone());
            steadies.push(Some(steady));
        } else {
            steadies.push(None);
        }
        tracks.push(Some(script));
    }
    let play = play?;
    let plays = steadies.into_iter().map(|s| s.filter(|s| *s != play)).collect();
    let mut m = Map { header: h, data_len: data.len(), runs: layout.runs, play, tracks, plays, moved: None, tried: None };
    // A moved driver only where it plays more of the file's tracks than the file's layout.
    if let Some(mv) = moved(&h, &m.runs, prg.len(), &calls(&m)) {
        let score = |m: &Map| map_check(nsf, m, prg, MOVED_CHECK_FRAMES, Fill::Bridged).iter().flatten().sum::<f64>();
        let before = score(&m);
        let mut with = m.clone();
        let d = mv.0;
        with.moved = Some(mv);
        let after = score(&with);
        m.tried = Some((d, before, after));
        if after > before {
            with.tried = m.tried;
            m = with;
        }
    }
    Some(m)
}

/// Frames each track plays when a map is checked for a moved driver.
const MOVED_CHECK_FRAMES: usize = 120;

/// The game routines a map's scripts call (in the file's view).
fn calls(m: &Map) -> Vec<u16> {
    let mut out: Vec<u16> = m
        .tracks
        .iter()
        .flatten()
        .chain(m.plays.iter().flatten())
        .chain(std::iter::once(&m.play))
        .flatten()
        .filter_map(|o| match *o {
            Op::Call { addr, .. } => Some(addr),
            Op::Poke(..) | Op::Push(_) => None,
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// The CPU address data byte `i` sits at when the file starts (its first banks).
fn cpu_of(h: &Header, i: usize) -> Option<u16> {
    if h.banks.iter().all(|&b| b == 0) {
        return u16::try_from(h.load as usize + i).ok();
    }
    let padded = i + (h.load & 0xFFF) as usize;
    let bank = (padded / 0x1000) as u8;
    let slot = h.banks.iter().position(|&b| b == bank)?;
    Some(0x8000 + (slot * 0x1000 + padded % 0x1000) as u16)
}

/// Where the game's own code sits if the file's runs lie at one distance from it in the CPU's
/// view (see [`Map::moved`]): the distance, taken in 8 KB banks (each ROM byte sits at the
/// CPU address nearest the file's with the same place in its bank), and the windows.
fn moved(h: &Header, runs: &[super::music_map::Run], prg_len: usize, calls: &[u16]) -> Option<(i32, Vec<(u16, usize)>)> {
    if h.fds || prg_len < 0x2000 {
        return None;
    }
    let dist = |r: &super::music_map::Run, a: u16| {
        let d = (r.rom as i64 - a as i64).rem_euclid(0x2000) as i32;
        if d >= 0x1000 { d - 0x2000 } else { d }
    };
    // The vote: the runs holding the routines the scripts call (the driver's code; its song
    // data may well sit where the other version has it), else every run by its length.
    let mut by: HashMap<i32, usize> = HashMap::new();
    let mut total = 0;
    for r in runs {
        let Some(a) = cpu_of(h, r.data) else { continue };
        let holds = calls.iter().filter(|&&c| c >= a && (c as usize) < a as usize + r.len).count();
        if holds > 0 {
            *by.entry(dist(r, a)).or_default() += holds;
            total += holds;
        }
    }
    if total == 0 {
        for r in runs {
            let Some(a) = cpu_of(h, r.data) else { continue };
            *by.entry(dist(r, a)).or_default() += r.len;
            total += r.len;
        }
    }
    let (&d, &n) = by.iter().max_by_key(|e| (*e.1, *e.0 == 0, -e.0.abs()))?;
    if d == 0 || n * 10 < total * 8 {
        return None;
    }
    // The game's windows the runs fall in, then the rest of $8000-$FFFF as the ROM goes on
    // from the nearest (a fixed last bank, a small ROM mirrored).
    let mut windows: Vec<(u16, usize)> = Vec::new();
    for r in runs {
        let Some(a) = cpu_of(h, r.data) else { continue };
        let rd = (r.rom as i64 - a as i64).rem_euclid(0x2000) as i32;
        if rd != d.rem_euclid(0x2000) {
            continue;
        }
        let g = a as i64 + d as i64;
        if !(0x6000..0x10000).contains(&g) {
            continue;
        }
        let base = (g as u16) & 0xE000;
        let Some(rom) = r.rom.checked_sub((g as usize) & 0x1FFF) else { continue };
        if !windows.iter().any(|w| w.0 == base) {
            windows.push((base, rom));
        }
    }
    let known = windows.clone();
    for base in (0x8000u32..0x10000).step_by(0x2000) {
        if windows.iter().any(|w| w.0 as u32 == base) {
            continue;
        }
        let near = known.iter().min_by_key(|w| (w.0 as i64 - base as i64).abs())?;
        let rom = (near.1 as i64 + base as i64 - near.0 as i64).rem_euclid(prg_len as i64) as usize;
        windows.push((base as u16, rom - rom % 0x2000 + near.1 % 0x2000));
    }
    Some((d, windows))
}

/// How a map fills the file's data between its runs (for finding why a track plays wrong).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill<'a> {
    /// Nothing: zeros.
    Zeros,
    /// A gap between two runs at the same distance in the ROM from the file: the ROM's bytes
    /// there (the game's own, where the file's author patched a few).
    Bridged,
    /// The file's own bytes (an oracle: what the map would play if it held them).
    File(&'a [u8]),
}

/// The file's data as a map rebuilds it from the ROM: its runs, and the gaps as `fill` says.
fn rebuilt(m: &Map, prg: &[u8], fill: Fill) -> Vec<u8> {
    let mut data = match fill {
        Fill::File(d) => d.to_vec(),
        _ => vec![0u8; m.data_len],
    };
    data.resize(m.data_len, 0);
    for r in &m.runs {
        if let Some(src) = prg.get(r.rom..r.rom + r.len) {
            data[r.data..r.data + r.len].copy_from_slice(src);
        }
    }
    if fill == Fill::Bridged {
        for w in m.runs.windows(2) {
            let (a, b) = (w[0], w[1]);
            let gap = a.data + a.len..b.data;
            if b.data > a.data + a.len && b.rom as isize - b.data as isize == a.rom as isize - a.data as isize {
                let from = a.rom + a.len;
                if let Some(src) = prg.get(from..from + gap.len()) {
                    data[gap].copy_from_slice(src);
                }
            }
        }
    }
    data
}

/// Track `song` played from the ROM by its map: each frame's sound writes.
pub fn map_track_writes(m: &Map, prg: &[u8], song: u8, frames: usize, fill: Fill) -> Option<Vec<Vec<Write>>> {
    let script = m.tracks.get(song as usize)?.as_ref()?;
    let (mut p, shift) = match (&m.moved, fill) {
        // The game's code where the game has it: the ROM's own windows, no file layout.
        (Some((d, windows)), Fill::Zeros | Fill::Bridged) => {
            let mut h = m.header;
            h.banks = [0; 8];
            h.load = 0x8000;
            let mut p = player(&h, &[])?;
            for &(base, rom) in windows {
                for i in 0..0x2000 {
                    p.mem[base as usize - 0x6000 + i] = prg.get(rom + i).copied().unwrap_or(0);
                }
            }
            (p, *d)
        }
        _ => (player(&m.header, &rebuilt(m, prg, fill))?, 0),
    };
    let mut cpu = Cpu::with(0, 0xFD);
    p.write(0x4015, 0x0F);
    if !replay(&mut p, &mut cpu, script, shift) {
        return None;
    }
    let mut out = Vec::with_capacity(frames);
    let mut first = std::mem::take(&mut p.writes);
    for _ in 0..frames {
        if !replay(&mut p, &mut cpu, m.play_of(song), shift) {
            break;
        }
        let mut f = std::mem::take(&mut first);
        f.append(&mut p.writes);
        out.push(f);
    }
    Some(out)
}

/// How a map plays the NSF's tracks: per track, how alike its melody is to the NSF's own
/// (None for a track whose script didn't record, or that has no melody in the NSF either).
pub fn map_check(nsf: &[u8], m: &Map, prg: &[u8], frames: usize, fill: Fill) -> Vec<Option<f64>> {
    (0..m.header.songs)
        .map(|t| {
            let real = melody(&track_writes(nsf, t, frames)?);
            if real.len() < music_search::MIN_MELODY {
                return None;
            }
            let mine = map_track_writes(m, prg, t, frames, fill).map(|w| melody(&w)).unwrap_or_default();
            Some(alike(&real, &mine))
        })
        .collect()
}

/// Where the game has the file's byte at data offset `data` (the file's CPU address `addr`):
/// its CPU address (in the file's window, or the next one), the 8 KB window, and the ROM
/// offset that window reads from. None when no run holds it.
fn game_place(m: &Map, prg_len: usize, data: usize, addr: u16) -> Option<(u16, usize, usize)> {
    let r = m.runs.iter().find(|r| (r.data..r.data + r.len).contains(&data))?;
    let rom = r.rom + (data - r.data);
    let mut g = (addr & 0xE000) as i32 | (rom & 0x1FFF) as i32;
    if g - addr as i32 > 0x1000 {
        g -= 0x2000;
    } else if addr as i32 - g > 0x1000 {
        g += 0x2000;
    }
    if !(0x6000..0x10000).contains(&g) || rom >= prg_len {
        return None;
    }
    Some((g as u16, (g as usize - 0x6000) >> 13, rom & !0x1FFF))
}

/// Track `song` played on the game's own board: the user's ROM in its cartridge (its own
/// bank switching), the game's code where the ROM has it, and the script's calls moved to
/// where the game has each routine. For a file made from another version of the game, whose
/// addresses differ area by area. None when the board isn't supported or the script fails.
pub fn cart_track_writes(m: &Map, rom: &[u8], song: u8, frames: usize) -> Option<Vec<Vec<Write>>> {
    use super::music_search::Machine;
    let script = m.tracks.get(song as usize)?.as_ref()?;
    let mut nes = super::nes_music::Nes::boot(rom).ok()?;
    let prg_len = super::nes_cart::parse(rom).ok()?.prg.len();
    // A bank the board puts in a window from power on (a fixed bank) stays there, wherever
    // the file has it.
    let boot = nes.windows();
    let place = |d: usize, a: u16| {
        let (g, w, base) = game_place(m, prg_len, d, a)?;
        match (1..5).find(|&k| boot[k] == base) {
            Some(k) if k != w => Some(((0x6000 + k * 0x2000) as u16 | (g & 0x1FFF), k, base)),
            _ => Some((g, w, base)),
        }
    };
    // The windows the runs ask for, the longest first; the rest as the board starts.
    let mut windows = boot;
    let mut set = [false; 5];
    let mut runs: Vec<_> = m.runs.iter().collect();
    runs.sort_by_key(|r| std::cmp::Reverse(r.len));
    for r in runs {
        // Every 8 KB window the run covers, from where each part of it starts.
        let mut d = r.data;
        while d < r.data + r.len {
            let Some(a) = cpu_of(&m.header, d) else { break };
            if let Some((_, w, base)) = place(d, a) {
                if w > 0 && !set[w] {
                    windows[w] = base;
                    set[w] = true;
                }
            }
            d += 0x2000 - (a as usize & 0x1FFF);
        }
    }
    nes.set_windows(windows);
    let budget = 400_000;
    let banked = m.header.banks.iter().any(|&b| b != 0);
    let mut run = |nes: &mut super::nes_music::Nes, ops: &[Op]| -> bool {
        for op in ops {
            match *op {
                // The file's own bank registers: the board's window where the game has
                // that bank (through the run holding most of it).
                Op::Poke(r @ 0x5FF8..=0x5FFF, b) if banked => {
                    let slot = r as usize - 0x5FF8;
                    let d0 = (b as usize * 0x1000).checked_sub((m.header.load & 0xFFF) as usize);
                    let Some(d0) = d0 else { continue };
                    let best = m
                        .runs
                        .iter()
                        .filter_map(|r| {
                            let from = r.data.max(d0);
                            let to = (r.data + r.len).min(d0 + 0x1000);
                            (to > from).then_some((to - from, from))
                        })
                        .max();
                    if let Some((_, d)) = best {
                        let a = (0x8000 + slot * 0x1000 + (d - d0)) as u16;
                        if let Some((_, w, base)) = place(d, a) {
                            let mut win = nes.windows();
                            win[w] = base;
                            nes.set_windows(win);
                        }
                    }
                }
                Op::Poke(0x5FF6..=0x5FFF, _) => {}
                Op::Poke(a @ (0x0000..=0x1FFF | 0x4000..=0x4017), v) => nes.write_bus(a, v),
                Op::Poke(a @ 0x6000..=0x7FFF, v) => nes.poke(a as u32, v),
                Op::Poke(..) => {}
                // Bytes the file's own code pushed: the game's code didn't (its routine
                // is entered where the game enters it).
                Op::Push(_) => {}
                Op::Call { addr, a, x, y, src } => {
                    let at = match src.checked_sub(1).and_then(|d| place(d as usize, addr)) {
                        Some((g, w, base)) => {
                            let mut win = nes.windows();
                            if w > 0 {
                                win[w] = base;
                            }
                            nes.set_windows(win);
                            g
                        }
                        None => addr,
                    };
                    if !nes.run_routine(at, [a, x, y], &[], budget) {
                        return false;
                    }
                }
            }
        }
        true
    };
    nes.write_bus(0x4015, 0x0F);
    if !run(&mut nes, script) {
        return None;
    }
    let mut out = Vec::with_capacity(frames);
    let mut first = nes.take_sound();
    for _ in 0..frames {
        if !run(&mut nes, m.play_of(song)) {
            break;
        }
        let mut f = std::mem::take(&mut first);
        f.append(&mut nes.take_sound());
        out.push(f);
    }
    Some(out)
}

/// A [`melody`] by notes rather than periods: each tone channel's period turned into the
/// nearest semitone at the console's clock (`pal`), so a PAL release (its own period table,
/// about 7% lower) matches the NTSC file it was made from. Notes are taken at each write
/// (`per_write`) or as each frame ends: the low and high period bytes are written one after
/// the other, and the period between the two (other bytes in another table) is no note.
pub fn note_melody(frames: &[Vec<Write>], pal: bool, per_write: bool) -> HashSet<u64> {
    const PAIRS: [(u32, u32); 3] = [(0x4002, 0x4003), (0x4006, 0x4007), (0x400A, 0x400B)];
    let clock = if pal { 1_662_607.0 } else { 1_789_773.0 };
    let mut lo = [0u16; 3];
    let mut hi = [0u16; 3];
    let note = |ch: usize, lo: &[u16; 3], hi: &[u16; 3], out: &mut Vec<Write>| {
        let period = (hi[ch] << 8 | lo[ch]) as f64;
        // The note number, written as a period the fingerprint takes (8 and up).
        let note = (12.0 * (clock / (16.0 * (period + 1.0)) / 27.5).log2()).round();
        let n = (note.clamp(-100.0, 400.0) + 108.0) as u16;
        out.push(Write { at: 0, port: PAIRS[ch].0, value: n & 0xFF });
        out.push(Write { at: 0, port: PAIRS[ch].1, value: n >> 8 });
    };
    let notes: Vec<Vec<Write>> = frames
        .iter()
        .map(|f| {
            let mut out = Vec::new();
            let mut touched = [false; 3];
            for w in f {
                let Some(ch) = PAIRS.iter().position(|p| p.0 == w.port || p.1 == w.port) else { continue };
                if PAIRS[ch].0 == w.port {
                    lo[ch] = w.value & 0xFF;
                } else {
                    hi[ch] = w.value & 7;
                }
                touched[ch] = true;
                if per_write {
                    note(ch, &lo, &hi, &mut out);
                }
            }
            if !per_write {
                (0..3).filter(|&c| touched[c]).for_each(|ch| note(ch, &lo, &hi, &mut out));
            }
            out
        })
        .collect();
    melody(&notes)
}

/// [`map_check`] on the game's own board ([`cart_track_writes`]): the best of the periods
/// as written and by notes ([`note_melody`]) at the NTSC or PAL clock.
pub fn cart_check(nsf: &[u8], m: &Map, rom: &[u8], frames: usize) -> Vec<Option<f64>> {
    (0..m.header.songs)
        .map(|t| {
            let real = melody(&track_writes(nsf, t, frames)?);
            if real.len() < music_search::MIN_MELODY {
                return None;
            }
            let Some(mine) = cart_track_writes(m, rom, t, frames) else { return Some(0.0) };
            // The periods as written, and by notes at either clock: a PAL release may keep
            // the NTSC period table or have its own (the header doesn't always say PAL).
            let raw = alike(&real, &melody(&mine));
            let real_writes = track_writes(nsf, t, frames)?;
            let mut best = raw;
            for per_write in [false, true] {
                let notes = note_melody(&real_writes, false, per_write);
                for pal in [false, true] {
                    best = best.max(alike(&notes, &note_melody(&mine, pal, per_write)));
                }
            }
            Some(best)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nsf(code: &[(u16, &[u8])]) -> Vec<u8> {
        let mut f = vec![0u8; 0x80];
        f[..5].copy_from_slice(b"NESM\x1a");
        f[6] = 3;
        f[8..10].copy_from_slice(&0x8000u16.to_le_bytes());
        f[0x0A..0x0C].copy_from_slice(&0x8000u16.to_le_bytes());
        f[0x0C..0x0E].copy_from_slice(&0x8010u16.to_le_bytes());
        // Code in a sea of other bytes, as in a game.
        let mut x = 99u32;
        let mut data: Vec<u8> = (0..0x100)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (x >> 16) as u8
            })
            .collect();
        for (at, bytes) in code {
            let o = (*at - 0x8000) as usize;
            data[o..o + bytes.len()].copy_from_slice(bytes);
        }
        f.extend(data);
        f
    }

    #[test]
    fn init_and_play_lead_to_the_routines_they_call() {
        let f = nsf(&[
            // init: JSR $8040; RTS
            (0x8000, &[0x20, 0x40, 0x80, 0x60]),
            // play: LDA #0; JMP $8080
            (0x8010, &[0xA9, 0x00, 0x4C, 0x80, 0x80]),
            (0x8040, &[0x60]),
            (0x8080, &[0x60]),
        ]);
        let h = hint(&f).unwrap();
        assert_eq!(h.songs, 3);
        let addrs = |v: &[(u32, Vec<u8>)]| v.iter().map(|r| r.0).collect::<Vec<_>>();
        assert_eq!(addrs(&h.start), vec![0x8000, 0x8040]);
        assert_eq!(addrs(&h.play), vec![0x8010, 0x8080]);
        assert_eq!(h.play[0].1[..3], [0xA9, 0x00, 0x4C]);
        // init hands its routine the track number in A unchanged.
        assert!(h.ids.iter().any(|i| i.0 == 0x8040 && i.1 == 0 && i.2 == vec![0, 1, 2]));
    }

    #[test]
    fn a_track_plays_on_its_own_and_matches_itself() {
        let f = nsf(&[
            // init: STA $00; RTS
            (0x8000, &[0x85, 0x00, 0x60]),
            // play: LDX $01; INX; INX; STX $01; TXA; ADC $00; STA $4002; LDA #0; STA $4003; RTS
            (0x8010, &[
                0xA6, 0x01, 0xE8, 0xE8, 0x86, 0x01, 0x8A, 0x65, 0x00, 0x8D, 0x02, 0x40, 0xA9,
                0x00, 0x8D, 0x03, 0x40, 0x60,
            ]),
        ]);
        let a = track_writes(&f, 0, 60).unwrap();
        assert_eq!(a.len(), 60);
        assert!(a[5].iter().any(|w| w.port == 0x4002));
        let ma = melody(&a);
        assert!(ma.len() > 20);
        assert_eq!(alike(&ma, &ma), 1.0);
        // Another song number shifts every pitch: a different melody.
        let mb = melody(&track_writes(&f, 1, 60).unwrap());
        assert!(alike(&ma, &mb) < 0.5);
    }

    #[test]
    fn a_map_plays_the_tracks_from_the_rom_without_the_files_init() {
        // The game's routines: start ($8040: STA $00; CLC; SEC; ...; JSR $8060; RTS), a
        // subroutine ($8060: RTS) and play ($8070: LDX $01; INX; INX; STX $01; TXA; ADC $00;
        // STA $4002; LDA #0; STA $4003; RTS).
        let mut start = vec![0x85, 0x00];
        (0..7).for_each(|_| start.extend([0x18, 0x38]));
        start.extend([0x20, 0x60, 0x80, 0x60]);
        let play: &[u8] = &[
            0xA6, 0x01, 0xE8, 0xE8, 0x86, 0x01, 0x8A, 0x65, 0x00, 0x8D, 0x02, 0x40, 0xA9, 0x00,
            0x8D, 0x03, 0x40, 0x60,
        ];
        // The file's own init: ASL A; JSR $8040; RTS (doubles the track into the game's id).
        let f = nsf(&[
            (0x8000, &[0x0A, 0x20, 0x40, 0x80, 0x60]),
            (0x8040, &start),
            (0x8060, &[0x60]),
            (0x8010, &[0x20, 0x70, 0x80, 0x60]),
            (0x8070, play),
        ]);
        // A ROM holding the game's routines, around other bytes; `shift` moves them (another
        // version of the game, its sound code a little further on, its own addresses in it).
        let rom = |shift: usize| {
            let mut x = 7u32;
            let mut prg: Vec<u8> = (0..0x4000)
                .map(|_| {
                    x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    (x >> 16) as u8
                })
                .collect();
            let data = &f[0x80..];
            prg[0x40 + shift..0x100 + shift].copy_from_slice(&data[0x40..0x100]);
            prg[0x51 + shift..0x53 + shift].copy_from_slice(&(0x8060 + shift as u16).to_le_bytes());
            prg
        };
        let prg = rom(0);
        let m = map(&f, &prg).unwrap();
        // Track 1's script calls the game's start routine with A = 2, as the file's init does.
        assert!(m.tracks[1].as_ref().unwrap().iter().any(|o| matches!(o, Op::Call { addr: 0x8040, a: 2, x: 0, y: 0, .. })));
        let check = map_check(&f, &m, &prg, 60, Fill::Zeros);
        assert!(check.iter().all(|c| c.is_some_and(|a| a > 0.99)), "{check:?}");
        // The same from a version with the sound code $2C further on: run where it sits.
        let prg = rom(0x2C);
        let m = map(&f, &prg).unwrap();
        assert_eq!(m.moved.as_ref().map(|m| m.0), Some(0x2C));
        let check = map_check(&f, &m, &prg, 60, Fill::Zeros);
        assert!(check.iter().all(|c| c.is_some_and(|a| a > 0.99)), "{check:?}");
        // And on the game's own board, from the ROM as a cartridge.
        let mut ines = b"NES\x1a\x01\x00\x00\x00".to_vec();
        ines.resize(16, 0);
        ines.extend_from_slice(&prg);
        let check = cart_check(&f, &m, &ines, 60);
        assert!(check.iter().all(|c| c.is_some_and(|a| a > 0.99)), "{check:?}");
    }

    #[test]
    fn a_script_keeps_stack_page_requests_and_moves_one_off_play_steps_into_the_track() {
        let play: &[u8] = &[
            0xA6, 0x01, 0xE8, 0xE8, 0x86, 0x01, 0x8A, 0x65, 0x00, 0x8D, 0x02, 0x40, 0xA9, 0x00,
            0x8D, 0x03, 0x40, 0x60,
        ];
        let f = nsf(&[
            // init: STA $0110; PHA; PLA; RTS (a request kept in the stack page)
            (0x8000, &[0x8D, 0x10, 0x01, 0x48, 0x68, 0x60]),
            // play: LDA $02; BNE +7; LDA #1; STA $02; STA $00 (once); JMP $8070
            (0x8010, &[0xA5, 0x02, 0xD0, 0x06, 0xA9, 0x01, 0x85, 0x02, 0x85, 0x00, 0x4C, 0x70, 0x80]),
            (0x8070, play),
        ]);
        let mut prg = vec![0u8; 0x4000];
        prg[0x1000..0x1000 + 0x90].copy_from_slice(&f[0x80 + 0x70..0x80 + 0x100]);
        let m = map(&f, &prg).unwrap();
        assert!(matches!(m.play[..], [Op::Call { addr: 0x8070, a: 1, x: 0, y: 0, .. }]), "{:?}", m.play);
        let t = m.tracks[2].as_ref().unwrap();
        assert_eq!(t[0], Op::Poke(0x110, 2));
        assert!(t.contains(&Op::Poke(0x02, 1)) && t.contains(&Op::Poke(0x00, 1)), "{t:?}");
        assert!(!t.iter().any(|o| matches!(o, Op::Poke(a, _) if *a >= 0x1F0 && *a < 0x200)), "{t:?}");
    }

    #[test]
    fn a_patched_first_byte_runs_as_the_files_and_the_call_starts_after_it() {
        // The game's routine at $8040 (CLC; SEC; ...; STA $4002; RTS), its first byte
        // patched by the file's author into STA $00 (85 00 in place of 18 38).
        let mut game = vec![0x18, 0x38];
        (0..10).for_each(|_| game.extend([0xE8, 0xC8]));
        game.extend([0x8D, 0x02, 0x40, 0x60]);
        let mut patched = game.clone();
        patched[..2].copy_from_slice(&[0x85, 0x00]);
        // init: JMP $8040; play: RTS.
        let f = nsf(&[(0x8000, &[0x4C, 0x40, 0x80]), (0x8010, &[0x60]), (0x8040, &patched)]);
        let mut prg = vec![0u8; 0x4000];
        let data = &f[0x80..];
        prg[0x40..0x100].copy_from_slice(&data[0x40..0x100]);
        prg[0x40..0x40 + game.len()].copy_from_slice(&game);
        let m = map(&f, &prg).unwrap();
        let t = m.tracks[1].as_ref().unwrap();
        assert_eq!(t[0], Op::Poke(0x00, 1), "{t:?}");
        assert!(matches!(t[1], Op::Call { addr: 0x8042, .. }), "{t:?}");
    }

    #[test]
    fn a_push_the_files_patched_byte_makes_is_handed_to_the_call() {
        // The game's routine at $8040 pulls A (PLA; STA $00; ...; RTS); the file's author
        // patched the byte before it into PHA.
        let mut game = vec![0xEA, 0x68, 0x85, 0x00];
        (0..8).for_each(|_| game.extend([0xE8, 0xC8]));
        game.push(0x60);
        let mut patched = game.clone();
        patched[0] = 0x48;
        let f = nsf(&[(0x8000, &[0x4C, 0x40, 0x80]), (0x8010, &[0x60]), (0x8040, &patched)]);
        let mut prg = vec![0u8; 0x4000];
        prg[0x40..0x100].copy_from_slice(&f[0x80 + 0x40..0x80 + 0x100]);
        prg[0x40..0x40 + game.len()].copy_from_slice(&game);
        let m = map(&f, &prg).unwrap();
        let t = m.tracks[2].as_ref().unwrap();
        assert!(matches!(t[..2], [Op::Push(2), Op::Call { addr: 0x8041, a: 2, x: 0, y: 0, .. }]), "{t:?}");
        let (mut p, _, _) = load(&f).unwrap();
        let mut cpu = Cpu::with(0, 0xFD);
        assert!(replay(&mut p, &mut cpu, t, 0));
        assert_eq!(p.ram[0], 2);
    }

    #[test]
    fn lengths_of_common_instructions() {
        for (op, len) in [
            (0xA9, 2), (0xAD, 3), (0xBD, 3), (0xB9, 3), (0x85, 2), (0x91, 2), (0xEA, 1),
            (0x0A, 1), (0xD0, 2), (0x20, 3), (0x60, 1), (0x6C, 3), (0xA2, 2), (0xE8, 1),
            (0x9A, 1), (0xBE, 3), (0x96, 2),
        ] {
            assert_eq!(length(op), len, "{op:02x}");
        }
    }
}
