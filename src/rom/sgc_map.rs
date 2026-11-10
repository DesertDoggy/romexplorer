//! A Master System or Game Gear game's music map, from a matched SGC file and the ROM: the
//! Z80 counterpart of `gb_map` (see there). The file's own init and play become writes and
//! calls into the game's routines; its data is laid over the ROM; only numbers are kept, and
//! the music plays from the user's ROM with no boot.

use std::collections::{HashMap, HashSet};

use super::music_map::Run;
use super::music_search::{self, Write};
use super::z80::{Bus, Cpu};

/// Sound ports, as `sms_music` numbers them: the PSG and the Game Gear's stereo register
/// (chip 0), the FM unit's address and data (chip 1).
const PSG: u32 = 0x000;
const GG_STEREO: u32 = 0x001;
const FM_ADDR: u32 = 0x100;
const FM_DATA: u32 = 0x101;

/// An SGC file's header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub load: u16,
    pub init: u16,
    pub play: u16,
    pub sp: u16,
    /// RST $08-$38's targets.
    pub rst: [u16; 7],
    /// The Sega mapper's registers ($FFFC-$FFFF) at start.
    pub mapping: [u8; 4],
    pub songs: u8,
    /// 0 Master System, 1 Game Gear (2, the ColecoVision, isn't mapped).
    pub system: u8,
    pub pal: bool,
}

impl Header {
    pub fn read(sgc: &[u8]) -> Option<Self> {
        if sgc.len() < 0xA0 || &sgc[..4] != b"SGC\x1A" || sgc[0x28] > 1 {
            return None;
        }
        let word = |at: usize| u16::from_le_bytes([sgc[at], sgc[at + 1]]);
        let mut rst = [0u16; 7];
        rst.iter_mut().enumerate().for_each(|(i, r)| *r = word(0x12 + i * 2));
        let mut mapping = [0u8; 4];
        mapping.copy_from_slice(&sgc[0x20..0x24]);
        Some(Header { load: word(0x08), init: word(0x0A), play: word(0x0C), sp: word(0x0E), rst, mapping, songs: sgc[0x25], system: sgc[0x28], pal: sgc[0x05] == 1 })
    }

    /// Where the CPU sees data offset `d` with its bank in slot 1.
    fn cpu_of(&self, d: usize) -> u16 {
        let i = d + self.load as usize;
        if i < 0x8000 {
            i as u16
        } else {
            0x4000 + (i % 0x4000) as u16
        }
    }
}

/// The file's code run on a bare Z80: its (padded) data in 16 KB banks behind the Sega
/// mapper, 8 KB of RAM, and the sound ports.
struct Player {
    load: usize,
    data_len: usize,
    rom: Vec<u8>,
    /// The banks in slots 0-2, and RAM in slot 2 instead.
    banks: [usize; 3],
    ram_in_slot2: bool,
    ram: Vec<u8>,
    ram2: Vec<u8>,
    gg: bool,
    writes: Vec<Write>,
    used: Option<Vec<bool>>,
    /// Every write (memory, or a port as `0x10000 | port`), while recording.
    log: Option<Vec<(u32, u8)>>,
    /// Data offsets the game's own code reads, while recording.
    game_reads: Option<std::collections::BTreeSet<usize>>,
}

impl Player {
    fn new(h: &Header, data: &[u8]) -> Self {
        let mut rom = vec![0u8; h.load as usize];
        // RST $08-$38 jump where the file says (bytes below the load address are free).
        for (i, &t) in h.rst.iter().enumerate() {
            let at = (i + 1) * 8;
            if at + 3 <= rom.len() {
                rom[at..at + 3].copy_from_slice(&[0xC3, t as u8, (t >> 8) as u8]);
            }
        }
        rom.extend_from_slice(data);
        let mut p = Player {
            load: h.load as usize,
            data_len: data.len(),
            rom,
            banks: [0, 1, 2],
            ram_in_slot2: false,
            ram: vec![0; 0x2000],
            ram2: vec![0; 0x4000],
            gg: h.system == 1,
            writes: Vec::new(),
            used: None,
            log: None,
            game_reads: None,
        };
        for (i, &v) in h.mapping.iter().enumerate() {
            p.map(0xFFFC + i as u16, v);
        }
        p
    }

    fn map(&mut self, addr: u16, v: u8) {
        match addr {
            0xFFFC => self.ram_in_slot2 = v & 0x08 != 0,
            0xFFFD => self.banks[0] = v as usize,
            0xFFFE => self.banks[1] = v as usize,
            0xFFFF => self.banks[2] = v as usize,
            _ => {}
        }
    }

    /// The index into the padded data the CPU reads at `addr`.
    fn index(&self, addr: u16) -> Option<usize> {
        let slot = (addr >> 14) as usize;
        if slot > 2 || (slot == 2 && self.ram_in_slot2) {
            return None;
        }
        Some(self.banks[slot] * 0x4000 + (addr as usize & 0x3FFF))
    }

    fn src(&self, addr: u16) -> u32 {
        match self.index(addr).and_then(|i| i.checked_sub(self.load)) {
            Some(d) if d < self.data_len => d as u32 + 1,
            _ => 0,
        }
    }

    /// Runs the routine at `pc` with `regs` and `pushed` on the stack above its return
    /// address, until it returns; false when it doesn't.
    fn call_with(&mut self, cpu: &mut Cpu, sp: u16, pc: u16, regs: &Regs, pushed: &[u8]) -> bool {
        cpu.sp = sp.wrapping_sub(2);
        self.write(cpu.sp, 0x00);
        self.write(cpu.sp.wrapping_add(1), 0x00);
        for &v in pushed {
            cpu.sp = cpu.sp.wrapping_sub(1);
            self.write(cpu.sp, v);
        }
        cpu.pc = pc;
        regs.set(cpu);
        cpu.halted = false;
        cpu.calls = vec![(pc, sp)];
        let mut used = 0u32;
        while !cpu.calls.is_empty() {
            if used > 2_000_000 || cpu.halted {
                return false;
            }
            used += cpu.step(self);
        }
        true
    }
}

impl Bus for Player {
    fn read(&mut self, addr: u16) -> u8 {
        if addr >= 0xC000 {
            return self.ram[addr as usize & 0x1FFF];
        }
        if addr >= 0x8000 && self.ram_in_slot2 {
            return self.ram2[addr as usize & 0x3FFF];
        }
        let Some(i) = self.index(addr) else { return 0xFF };
        if let (Some(used), Some(d)) = (self.used.as_mut(), i.checked_sub(self.load)) {
            if let Some(u) = used.get_mut(d) {
                *u = true;
            }
        }
        if let (Some(r), Some(d)) = (self.game_reads.as_mut(), i.checked_sub(self.load)) {
            r.insert(d);
        }
        self.rom.get(i).copied().unwrap_or(0xFF)
    }

    fn write(&mut self, addr: u16, v: u8) {
        if let Some(log) = self.log.as_mut() {
            log.push((addr as u32, v));
        }
        if addr >= 0xC000 {
            self.ram[addr as usize & 0x1FFF] = v;
            if addr >= 0xFFFC {
                self.map(addr, v);
            }
        } else if addr >= 0x8000 && self.ram_in_slot2 {
            self.ram2[addr as usize & 0x3FFF] = v;
        }
    }

    fn input(&mut self, _port: u16) -> u8 {
        0
    }

    fn output(&mut self, port: u16, v: u8) {
        if let Some(log) = self.log.as_mut() {
            log.push((0x10000 | port as u32 & 0xFF, v));
        }
        let p = port as u8;
        let to = if self.gg && p == 0x06 {
            Some(GG_STEREO)
        } else {
            match p {
                0xF0 => Some(FM_ADDR),
                0xF1 => Some(FM_DATA),
                _ if p & 0xC1 == 0x40 || p & 0xC1 == 0x41 => Some(PSG),
                _ => None,
            }
        };
        if let Some(port) = to {
            self.writes.push(Write { at: 0, port, value: v as u16 });
        }
    }
}

/// The registers a call into the game's code is made with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Regs {
    pub a: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub h: u8,
    pub l: u8,
    pub ix: u16,
    pub iy: u16,
}

impl Regs {
    fn of(c: &Cpu) -> Self {
        Regs { a: c.a, b: c.b, c: c.c, d: c.d, e: c.e, h: c.h, l: c.l, ix: c.ix, iy: c.iy }
    }

    fn set(&self, c: &mut Cpu) {
        (c.a, c.b, c.c, c.d, c.e, c.h, c.l, c.ix, c.iy) = (self.a, self.b, self.c, self.d, self.e, self.h, self.l, self.ix, self.iy);
    }
}

/// One step of a track's script: a write or port output the file's own code makes, a byte
/// it leaves on the stack for the next call, or a call into the game's routine at `addr`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Poke(u16, u8),
    Out(u8, u8),
    Push(u8),
    Call { addr: u16, regs: Regs, src: u32 },
}

/// Which player plays a track from the ROM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Way {
    Layout,
    Zeros,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill<'a> {
    Zeros,
    Bridged,
    File(&'a [u8]),
}

/// A Master System or Game Gear game's music map: numbers only.
#[derive(Debug, Clone)]
pub struct Map {
    pub header: Header,
    pub data_len: usize,
    pub runs: Vec<Run>,
    pub play: Vec<Op>,
    pub tracks: Vec<Option<Vec<Op>>>,
    pub plays: Vec<Option<Vec<Op>>>,
    pub ways: Vec<Way>,
    /// Bytes of the file the game's code reads that differ from the ROM's where the layout
    /// puts them (a bank number or pointer its author changed): data offset and value.
    pub patches: Vec<(u32, u8)>,
}

impl Map {
    pub fn play_of(&self, song: u8) -> &[Op] {
        self.plays.get(song as usize).and_then(|p| p.as_deref()).unwrap_or(&self.play)
    }
}

const PATCHED_BYTES: u16 = 3;
/// Frames each track is played to find what the game's code reads of the file.
const PATCH_FRAMES: usize = 1800;
const PLAY_CALLS: usize = 6;

/// Calls, jumps, returns and restarts (not relative branches): the opcode and the byte
/// after it (for the ED and DD/FD prefixes).
fn is_jump(op: u8, next: u8) -> bool {
    match op {
        0xCD | 0xC3 | 0xE9 | 0xC9 => true,
        0xED => matches!(next, 0x4D | 0x45),
        0xDD | 0xFD => next == 0xE9,
        _ => op & 0xC7 == 0xC4 || op & 0xC7 == 0xC2 || op & 0xC7 == 0xC0 || op & 0xC7 == 0xC7,
    }
}

/// Runs the routine at `pc` with A = `a` and records it as a script (see `gb_map`'s).
fn record(p: &mut Player, cpu: &mut Cpu, sp: u16, pc: u16, a: u8, placed: &[bool], aligned: &[bool]) -> Option<Vec<Op>> {
    cpu.sp = sp.wrapping_sub(2);
    p.write(cpu.sp, 0x00);
    p.write(cpu.sp.wrapping_add(1), 0x00);
    cpu.pc = pc;
    cpu.a = a;
    cpu.halted = false;
    cpu.calls = vec![(pc, sp)];
    let of = |p: &Player, addr: u16, set: &[bool]| {
        let s = p.src(addr);
        s > 0 && set.get(s as usize - 1).copied().unwrap_or(false)
    };
    let mut ops = Vec::new();
    let mut in_game: Option<usize> = None;
    let mut used = 0u32;
    let mut jumped = true;
    let mut target: Option<(u16, u16)> = None;
    while !cpu.calls.is_empty() {
        if used > 2_000_000 || cpu.halted {
            return None;
        }
        if in_game.is_some_and(|d| cpu.calls.len() < d) {
            in_game = None;
        }
        let at = cpu.pc;
        if in_game.is_none() {
            let near = target.is_some_and(|(t, _)| at > t && at - t <= PATCHED_BYTES);
            let entered = if jumped || near { of(p, at, placed) } else { of(p, at, aligned) };
            if entered {
                if let Some((_, s)) = target.filter(|t| t.1 > cpu.sp) {
                    ops.extend((cpu.sp..s).rev().map(|i| Op::Push(p.read(i))));
                }
                ops.push(Op::Call { addr: at, regs: Regs::of(cpu), src: p.src(at) });
                in_game = Some(cpu.calls.len());
            }
        }
        let (op, next) = (p.read(at), p.read(at.wrapping_add(1)));
        p.log = in_game.is_none().then(Vec::new);
        used += cpu.step(p);
        jumped = is_jump(op, next) && !(cpu.pc > at && cpu.pc <= at.wrapping_add(4));
        if jumped && in_game.is_none() {
            target = Some((cpu.pc, cpu.sp));
        }
        if let Some(log) = p.log.take() {
            let live = cpu.sp as u32;
            for (a, v) in log {
                if a >= 0x10000 {
                    ops.push(Op::Out(a as u8, v));
                } else if !(a >= live && a < sp as u32) {
                    ops.push(Op::Poke(a as u16, v));
                }
            }
        }
    }
    Some(ops)
}

fn replay(p: &mut Player, cpu: &mut Cpu, sp: u16, ops: &[Op]) -> bool {
    let mut pushed = Vec::new();
    for op in ops {
        match *op {
            Op::Poke(a, v) => p.write(a, v),
            Op::Out(port, v) => p.output(port as u16, v),
            Op::Push(v) => pushed.push(v),
            Op::Call { addr, regs, .. } => {
                if !p.call_with(cpu, sp, addr, &regs, &std::mem::take(&mut pushed)) {
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
            (Op::Call { addr: p, .. }, Op::Call { addr: q, .. }) => p == q,
            _ => x == y,
        })
}

/// Runs a fresh player through track `t`'s init and `frames` plays, showing it to `each`.
fn run_track(h: &Header, data: &[u8], t: u8, frames: usize, mut each: impl FnMut(&mut Player)) -> Option<Player> {
    let mut p = Player::new(h, data);
    let mut cpu = Cpu::default();
    if !p.call_with(&mut cpu, h.sp, h.init, &Regs { a: t, ..Default::default() }, &[]) {
        return Some(p);
    }
    each(&mut p);
    for _ in 0..frames {
        if !p.call_with(&mut cpu, h.sp, h.play, &Regs::default(), &[]) {
            break;
        }
        each(&mut p);
    }
    Some(p)
}

/// Track `song` of an SGC played on its own, `frames` frames: each frame's sound writes.
pub fn track_writes(sgc: &[u8], song: u8, frames: usize) -> Option<Vec<Vec<Write>>> {
    let h = Header::read(sgc)?;
    let mut out = Vec::with_capacity(frames + 1);
    run_track(&h, &sgc[0xA0..], song, frames, |p| out.push(std::mem::take(&mut p.writes)))?;
    // The init's writes go with the first frame.
    if out.len() > 1 {
        let first = out.remove(0);
        out[0].splice(0..0, first);
    }
    Some(out)
}

/// A melody's fingerprint (see [`music_search::melody`]) on the PSG's three tone channels:
/// its latched writes turned into each channel's 10-bit period.
pub fn melody(frames: &[Vec<Write>]) -> HashSet<u64> {
    let mut period = [0u16; 3];
    let mut latched = 0usize;
    let mut tone = true;
    let as_ports: Vec<Vec<Write>> = frames
        .iter()
        .map(|f| {
            let mut out = Vec::new();
            for w in f.iter().filter(|w| w.port == PSG) {
                let v = w.value;
                if v & 0x80 != 0 {
                    latched = ((v >> 5) & 3) as usize;
                    tone = v & 0x10 == 0;
                    if latched < 3 && tone {
                        period[latched] = (period[latched] & 0x3F0) | (v & 0x0F);
                    } else {
                        continue;
                    }
                } else if latched < 3 && tone {
                    period[latched] = (period[latched] & 0x0F) | ((v & 0x3F) << 4);
                } else {
                    continue;
                }
                let ch = latched as u32;
                out.push(Write { at: 0, port: 0x2000 + ch * 2, value: period[latched] & 0xFF });
                out.push(Write { at: 0, port: 0x2001 + ch * 2, value: period[latched] >> 8 });
            }
            out
        })
        .collect();
    music_search::melody(&as_ports, &[(0x2000, 0x2001), (0x2002, 0x2003), (0x2004, 0x2005)])
}

/// Which data bytes the file's tracks read in their first `frames` frames.
pub fn used_bytes(sgc: &[u8], frames: usize) -> Option<Vec<bool>> {
    let h = Header::read(sgc)?;
    let data = &sgc[0xA0..];
    let mut used = vec![false; data.len()];
    for t in 0..h.songs {
        let mut first = true;
        let p = run_track(&h, data, t, frames, |p| {
            if first {
                p.used = Some(std::mem::take(&mut used));
                first = false;
            }
        });
        // (The init's reads before the hook are the init's own: counted by running it again.)
        if let Some(mut p) = p {
            if let Some(u) = p.used.take() {
                used = u;
            }
        }
    }
    // The inits' own reads.
    for t in 0..h.songs {
        let mut p = Player::new(&h, data);
        p.used = Some(std::mem::take(&mut used));
        let mut cpu = Cpu::default();
        p.call_with(&mut cpu, h.sp, h.init, &Regs { a: t, ..Default::default() }, &[]);
        used = p.used.take().unwrap_or_default();
    }
    Some(used)
}

/// The music map of a Master System or Game Gear ROM for the SGC it matches.
pub fn map(sgc: &[u8], rom: &[u8]) -> Option<Map> {
    let h = Header::read(sgc)?;
    let data = &sgc[0xA0..];
    let used = used_bytes(sgc, music_search::HINT_TRACK_FRAMES)?;
    let layout = super::music_map::layout(data, &used, rom);
    let mut placed = vec![false; data.len()];
    for r in &layout.runs {
        placed[r.data..r.data + r.len].iter_mut().for_each(|b| *b = true);
    }
    let dist = |r: &Run| (r.rom as i64 - h.cpu_of(r.data) as i64).rem_euclid(0x4000);
    let mut by: HashMap<i64, usize> = HashMap::new();
    layout.runs.iter().for_each(|r| *by.entry(dist(r)).or_default() += r.len);
    let main = by.iter().max_by_key(|e| (*e.1, -*e.0)).map(|e| *e.0);
    let mut aligned = vec![false; data.len()];
    for r in layout.runs.iter().filter(|r| Some(dist(r)) == main) {
        aligned[r.data..r.data + r.len].iter_mut().for_each(|b| *b = true);
    }
    let mut tracks = Vec::new();
    let mut steadies: Vec<Option<Vec<Op>>> = Vec::new();
    let mut play: Option<Vec<Op>> = None;
    for t in 0..h.songs {
        let mut p = Player::new(&h, data);
        let mut cpu = Cpu::default();
        let Some(mut script) = record(&mut p, &mut cpu, h.sp, h.init, t, &placed, &aligned) else {
            tracks.push(None);
            steadies.push(None);
            continue;
        };
        let mut calls = Vec::new();
        for _ in 0..PLAY_CALLS {
            match record(&mut p, &mut cpu, h.sp, h.play, 0, &placed, &aligned) {
                Some(f) => calls.push(f),
                None => break,
            }
        }
        if let Some(steady) = calls.last().cloned() {
            let settled = calls.iter().rposition(|f| !same_steps(f, &steady)).map_or(0, |i| i + 1);
            calls[..settled].iter().for_each(|f| script.extend(f.iter().cloned()));
            play.get_or_insert(steady.clone());
            steadies.push(Some(steady));
        } else {
            steadies.push(None);
        }
        tracks.push(Some(script));
    }
    let play = play?;
    let plays = steadies.into_iter().map(|s| s.filter(|s| !same_steps(s, &play))).collect();
    let mut m = Map { header: h, data_len: data.len(), runs: layout.runs, play, tracks, plays, ways: Vec::new(), patches: Vec::new() };
    // What the game's code reads of the file as its tracks play (the scripts call only the
    // game's code), kept where the file differs from the layout bridged from the ROM.
    let mut reads = std::collections::BTreeSet::new();
    for t in 0..h.songs {
        if let Some(mut s) = LayoutSong::open_reading(&m, data, t) {
            for _ in 0..PATCH_FRAMES {
                if s.frame().is_none() {
                    break;
                }
            }
            reads.append(&mut s.p.game_reads.take().unwrap_or_default());
        }
    }
    let bridged = rebuilt(&m, rom, Fill::Bridged);
    m.patches = reads.into_iter().filter(|&d| d < data.len() && !placed[d] && data[d] != bridged[d]).map(|d| (d as u32, data[d])).collect();
    Some(m)
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
    if !matches!(fill, Fill::File(_)) {
        for &(d, v) in &m.patches {
            if let Some(b) = data.get_mut(d as usize) {
                *b = v;
            }
        }
    }
    data
}

/// A track playing from the ROM by its map: each frame's sound writes.
pub struct LayoutSong {
    p: Player,
    cpu: Cpu,
    sp: u16,
    play: Vec<Op>,
    first: Vec<Write>,
}

impl LayoutSong {
    pub fn open(m: &Map, rom: &[u8], song: u8, fill: Fill) -> Option<Self> {
        let script = m.tracks.get(song as usize)?.as_ref()?;
        let mut p = Player::new(&m.header, &rebuilt(m, rom, fill));
        let mut cpu = Cpu::default();
        if !replay(&mut p, &mut cpu, m.header.sp, script) {
            return None;
        }
        let first = std::mem::take(&mut p.writes);
        Some(Self { p, cpu, sp: m.header.sp, play: m.play_of(song).to_vec(), first })
    }

    /// The song on the file's own data, noting what the game's code reads of it.
    fn open_reading(m: &Map, data: &[u8], song: u8) -> Option<Self> {
        let script = m.tracks.get(song as usize)?.as_ref()?;
        let mut p = Player::new(&m.header, data);
        p.game_reads = Some(Default::default());
        let mut cpu = Cpu::default();
        if !replay(&mut p, &mut cpu, m.header.sp, script) {
            return None;
        }
        let first = std::mem::take(&mut p.writes);
        Some(Self { p, cpu, sp: m.header.sp, play: m.play_of(song).to_vec(), first })
    }

    pub fn frame(&mut self) -> Option<Vec<Write>> {
        if !replay(&mut self.p, &mut self.cpu, self.sp, &self.play) {
            return None;
        }
        let mut f = std::mem::take(&mut self.first);
        f.append(&mut self.p.writes);
        Some(f)
    }
}

pub fn map_track_writes(m: &Map, rom: &[u8], song: u8, frames: usize, fill: Fill) -> Option<Vec<Vec<Write>>> {
    let mut s = LayoutSong::open(m, rom, song, fill)?;
    Some((0..frames).map_while(|_| s.frame()).collect())
}

/// How a map plays the SGC's tracks: per track, how alike its melody is to the file's.
pub fn map_check(sgc: &[u8], m: &Map, rom: &[u8], frames: usize, fill: Fill) -> Vec<Option<f64>> {
    (0..m.header.songs)
        .map(|t| {
            let real = melody(&track_writes(sgc, t, frames)?);
            if real.len() < music_search::MIN_MELODY {
                return None;
            }
            let mine = map_track_writes(m, rom, t, frames, fill).map(|w| melody(&w)).unwrap_or_default();
            Some(music_search::alike(&real, &mine))
        })
        .collect()
}

pub fn choose_ways(sgc: &[u8], m: &mut Map, rom: &[u8], frames: usize, right: f64) {
    let bridged = map_check(sgc, m, rom, frames, Fill::Bridged);
    let zeros = if bridged.iter().any(|b| b.unwrap_or(0.0) < right) { map_check(sgc, m, rom, frames, Fill::Zeros) } else { Vec::new() };
    m.ways = (0..bridged.len())
        .map(|i| {
            let score = |v: &Vec<Option<f64>>| v.get(i).copied().flatten().unwrap_or(0.0);
            if score(&bridged) >= right {
                Way::Layout
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

pub fn build_map(sgc: &[u8], rom: &[u8]) -> Option<Map> {
    let mut m = map(sgc, rom)?;
    choose_ways(sgc, &mut m, rom, WAY_FRAMES, MAP_RIGHT);
    Some(m)
}

pub fn play_map(m: &Map, rom: &[u8], song: u8, frames: usize) -> Option<Vec<Vec<Write>>> {
    let fill = match m.ways.get(song as usize)? {
        Way::Layout => Fill::Bridged,
        Way::Zeros => Fill::Zeros,
        Way::None => return None,
    };
    map_track_writes(m, rom, song, frames, fill)
}

/// Z80 cycles a second, and per frame (one play call per 1/60 s).
const CLOCK: u32 = 3_579_545;
const FRAME_CLOCKS: u32 = CLOCK / 60;

/// A track from the user's ROM by its map, as sound (stereo).
pub struct MapSong {
    song: LayoutSong,
    chips: super::vgm_chips::Chips,
    fm: bool,
}

impl MapSong {
    pub fn open(m: &Map, rom: &[u8], song: u8, rate: u32) -> Result<Self, String> {
        use super::vgm_chips::{Chips, Kind};
        let fill = match m.ways.get(song as usize) {
            Some(Way::Layout) => Fill::Bridged,
            Some(Way::Zeros) => Fill::Zeros,
            _ => return Err("the map has no way to play this song".into()),
        };
        let song = LayoutSong::open(m, rom, song, fill).ok_or("the song didn't start")?;
        let gg = m.header.system == 1;
        let mut chips = vec![(if gg { Kind::GameGearPsg } else { Kind::SegaPsg }, CLOCK)];
        if !gg {
            chips.push((Kind::Ym2413, CLOCK));
        }
        Ok(Self { song, chips: Chips::new(rate, CLOCK as f64, &chips)?, fm: !gg })
    }

    pub fn frame_rate(&self) -> f64 {
        60.0
    }

    pub fn frame(&mut self, out: &mut Vec<i16>) -> Option<Vec<Write>> {
        let w = self.song.frame()?;
        for x in &w {
            if x.port >> 8 == 1 && !self.fm {
                continue;
            }
            self.chips.write(x.at.min(FRAME_CLOCKS - 1), (x.port >> 8) as u8, (x.port & 0xFF) as u8, x.value as u8);
        }
        self.chips.end_frame(FRAME_CLOCKS, out);
        Some(w)
    }
}

impl Map {
    pub fn encode(&self) -> String {
        let h = &self.header;
        let ops = |ops: &[Op]| {
            ops.iter()
                .map(|o| match *o {
                    Op::Poke(a, v) => format!("P{a:x}:{v:x}"),
                    Op::Out(p, v) => format!("O{p:x}:{v:x}"),
                    Op::Push(v) => format!("U{v:x}"),
                    Op::Call { addr, regs: r, src } => {
                        format!("C{addr:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{src:x}", r.a, r.b, r.c, r.d, r.e, r.h, r.l, r.ix, r.iy)
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        let mut out = vec![
            "sgcmap 1".to_string(),
            format!(
                "h {:x} {:x} {:x} {:x} {} {} {} {} {}",
                h.load,
                h.init,
                h.play,
                h.sp,
                h.rst.iter().map(|r| format!("{r:x}")).collect::<Vec<_>>().join(":"),
                h.mapping.iter().map(|r| format!("{r:x}")).collect::<Vec<_>>().join(":"),
                h.songs,
                h.system,
                h.pal as u8
            ),
            format!("d {:x}", self.data_len),
            format!("r {}", self.runs.iter().map(|r| format!("{:x}:{:x}:{:x}", r.data, r.len, r.rom)).collect::<Vec<_>>().join(" ")),
            format!("p {}", ops(&self.play)),
        ];
        for (i, t) in self.tracks.iter().enumerate() {
            if let Some(t) = t {
                out.push(format!("t {i} {}", ops(t)));
            }
        }
        for (i, p) in self.plays.iter().enumerate() {
            if let Some(p) = p {
                out.push(format!("q {i} {}", ops(p)));
            }
        }
        out.push(format!("s {}", self.ways.iter().map(|w| match w { Way::Layout => 'l', Way::Zeros => 'z', Way::None => '-' }).collect::<String>()));
        if !self.patches.is_empty() {
            out.push(format!("x {}", self.patches.iter().map(|(d, v)| format!("{d:x}:{v:x}")).collect::<Vec<_>>().join(" ")));
        }
        out.join("\n")
    }

    pub fn decode(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != "sgcmap 1" {
            return None;
        }
        let num = |t: &str| u32::from_str_radix(t, 16).ok();
        let list = |t: &str| -> Option<Vec<u32>> { t.split(':').map(num).collect() };
        let ops = |rest: &[&str]| -> Option<Vec<Op>> {
            rest.iter()
                .map(|t| {
                    let (k, v) = t.split_at(1);
                    let f = list(v)?;
                    Some(match (k, f.as_slice()) {
                        ("P", [a, v]) => Op::Poke(*a as u16, *v as u8),
                        ("O", [p, v]) => Op::Out(*p as u8, *v as u8),
                        ("U", [v]) => Op::Push(*v as u8),
                        ("C", [addr, a, b, c, d, e, h, l, ix, iy, src]) => Op::Call {
                            addr: *addr as u16,
                            regs: Regs { a: *a as u8, b: *b as u8, c: *c as u8, d: *d as u8, e: *e as u8, h: *h as u8, l: *l as u8, ix: *ix as u16, iy: *iy as u16 },
                            src: *src,
                        },
                        _ => return None,
                    })
                })
                .collect()
        };
        let mut m = Map {
            header: Header { load: 0, init: 0, play: 0, sp: 0, rst: [0; 7], mapping: [0; 4], songs: 0, system: 0, pal: false },
            data_len: 0,
            runs: Vec::new(),
            play: Vec::new(),
            tracks: Vec::new(),
            plays: Vec::new(),
            ways: Vec::new(),
            patches: Vec::new(),
        };
        for line in lines {
            let f: Vec<&str> = line.split(' ').filter(|t| !t.is_empty()).collect();
            match f.first().copied() {
                Some("h") if f.len() == 10 => {
                    let rst = list(f[5])?;
                    let mapping = list(f[6])?;
                    if rst.len() != 7 || mapping.len() != 4 {
                        return None;
                    }
                    let mut h = Header {
                        load: num(f[1])? as u16,
                        init: num(f[2])? as u16,
                        play: num(f[3])? as u16,
                        sp: num(f[4])? as u16,
                        rst: [0; 7],
                        mapping: [0; 4],
                        songs: f[7].parse().ok()?,
                        system: f[8].parse().ok()?,
                        pal: f[9] == "1",
                    };
                    h.rst.iter_mut().zip(&rst).for_each(|(d, s)| *d = *s as u16);
                    h.mapping.iter_mut().zip(&mapping).for_each(|(d, s)| *d = *s as u8);
                    m.header = h;
                    m.tracks = vec![None; h.songs as usize];
                    m.plays = vec![None; h.songs as usize];
                }
                Some("d") => m.data_len = num(f.get(1)?)? as usize,
                Some("r") => {
                    for t in &f[1..] {
                        let v = list(t)?;
                        let [data, len, rom] = v[..] else { return None };
                        m.runs.push(Run { data: data as usize, len: len as usize, rom: rom as usize });
                    }
                }
                Some("p") => m.play = ops(&f[1..])?,
                Some(k @ ("t" | "q")) => {
                    let i: usize = f.get(1)?.parse().ok()?;
                    let list = if k == "t" { &mut m.tracks } else { &mut m.plays };
                    *list.get_mut(i)? = Some(ops(&f[2..])?);
                }
                Some("x") => {
                    for t in &f[1..] {
                        let v = list(t)?;
                        let [d, b] = v[..] else { return None };
                        m.patches.push((d, b as u8));
                    }
                }
                Some("s") => {
                    m.ways = f.get(1).unwrap_or(&"").chars().map(|c| match c { 'l' => Way::Layout, 'z' => Way::Zeros, _ => Way::None }).collect();
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

    /// A tiny game: a sound routine in bank 3 ($4000 in slot 1) stores the song number, and
    /// its play steps PSG channel 0's period each call. The SGC's author wrote their own init
    /// (map the bank, poke a variable, silence a channel through the port, call the game's
    /// init) and play.
    fn game_and_file() -> (Vec<u8>, Vec<u8>) {
        let mut rom = vec![0u8; 0x10000];
        let mut x = 11u32;
        for b in rom.iter_mut() {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            *b = (x >> 16) as u8;
        }
        let bank3 = 0xC000;
        // init ($4000): LD ($C000),A; XOR A; LD ($C001),A; RET
        let init = [0x32, 0x00, 0xC0, 0xAF, 0x32, 0x01, 0xC0, 0xC9];
        // play ($4100): LD A,($C001); INC A; LD ($C001),A; LD B,A; LD A,($C000); ADD A,B;
        // AND $0F; OR $80; OUT ($7F),A; LD A,B; AND $3F; OUT ($7F),A; RET
        let play = [0x3A, 0x01, 0xC0, 0x3C, 0x32, 0x01, 0xC0, 0x47, 0x3A, 0x00, 0xC0, 0x80, 0xE6, 0x0F, 0xF6, 0x80, 0xD3, 0x7F, 0x78, 0xE6, 0x3F, 0xD3, 0x7F, 0xC9];
        rom[bank3..bank3 + init.len()].copy_from_slice(&init);
        rom[bank3 + 0x100..bank3 + 0x100 + play.len()].copy_from_slice(&play);
        let mut f = vec![0u8; 0xA0];
        f[..4].copy_from_slice(b"SGC\x1A");
        f[4] = 1;
        f[0x08..0x0A].copy_from_slice(&0x0400u16.to_le_bytes());
        f[0x0A..0x0C].copy_from_slice(&0x0400u16.to_le_bytes());
        f[0x0C..0x0E].copy_from_slice(&0x0420u16.to_le_bytes());
        f[0x0E..0x10].copy_from_slice(&0xDFF0u16.to_le_bytes());
        f[0x20..0x24].copy_from_slice(&[0, 0, 1, 2]);
        f[0x25] = 3;
        // The file's banks: 0 (its code from $0400), then bank 1 = the game's bank 3.
        let mut data = vec![0u8; 0x4000 - 0x400 + 0x4000];
        // init ($0400): LD B,A; LD A,1; LD ($FFFE),A; LD A,$40; LD ($C002),A; LD A,$9F;
        // OUT ($7F),A; LD A,B; ADD A,A; ADD A,$20; CALL $4000; RET
        let my_init = [0x47, 0x3E, 0x01, 0x32, 0xFE, 0xFF, 0x3E, 0x40, 0x32, 0x02, 0xC0, 0x3E, 0x9F, 0xD3, 0x7F, 0x78, 0x87, 0xC6, 0x20, 0xCD, 0x00, 0x40, 0xC9];
        data[..my_init.len()].copy_from_slice(&my_init);
        // play ($0420): CALL $4100; RET
        data[0x20..0x24].copy_from_slice(&[0xCD, 0x00, 0x41, 0xC9]);
        let b1 = 0x4000 - 0x400;
        data[b1..b1 + 0x4000].copy_from_slice(&rom[bank3..bank3 + 0x4000]);
        f.extend(data);
        (rom, f)
    }

    #[test]
    fn a_map_plays_the_files_tracks_from_the_rom_and_round_trips() {
        let (rom, f) = game_and_file();
        let mut m = map(&f, &rom).expect("map");
        let t1 = m.tracks[1].as_ref().unwrap();
        assert!(t1.contains(&Op::Poke(0xFFFE, 1)), "{t1:?}");
        assert!(t1.contains(&Op::Out(0x7F, 0x9F)), "{t1:?}");
        assert!(t1.iter().any(|o| matches!(o, Op::Call { addr: 0x4000, regs, .. } if regs.a == 0x22)), "{t1:?}");
        let check = map_check(&f, &m, &rom, 60, Fill::Zeros);
        assert!(check.iter().all(|c| c.is_some_and(|a| a > 0.99)), "{check:?}");
        choose_ways(&f, &mut m, &rom, 60, MAP_RIGHT);
        let text = m.encode();
        let back = Map::decode(&text).unwrap();
        assert_eq!(back.encode(), text);
        for t in 0..3 {
            assert_eq!(play_map(&back, &rom, t, 60), track_writes(&f, t, 60));
        }
    }
}
