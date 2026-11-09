//! A Game Boy game's music map, from a matched GBS file and the ROM: where the file's code
//! and song data are in the ROM, and per track the steps that replace the file's own init
//! and play (the writes its author's code makes, and the calls into the game's routines).
//! Only numbers are kept; the music plays from the user's ROM with no boot. The Game Boy
//! counterpart of `nes_nsf`'s map.

use std::collections::HashMap;

use super::music_map::Run;
use super::music_search::{self, Write};
use super::sm83::{Bus, Cpu};

/// A GBS file's header: where its data loads, its routines, its stack, song count and
/// timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub load: u16,
    pub init: u16,
    pub play: u16,
    pub sp: u16,
    pub songs: u8,
    pub tma: u8,
    pub tac: u8,
}

impl Header {
    pub fn read(gbs: &[u8]) -> Option<Self> {
        if gbs.len() < 0x70 || &gbs[..3] != b"GBS" {
            return None;
        }
        let word = |at: usize| u16::from_le_bytes([gbs[at], gbs[at + 1]]);
        let h = Header { load: word(0x06), init: word(0x08), play: word(0x0A), sp: word(0x0C), songs: gbs[0x04], tma: gbs[0x0E], tac: gbs[0x0F] };
        (h.load < 0x8000).then_some(h)
    }

    /// How many times play runs per 1/60 s: the timer's rate when the file sets one, else
    /// the screen's.
    fn rate(&self) -> f64 {
        if self.tac & 4 != 0 {
            let clock = [4096.0, 262144.0, 65536.0, 16384.0][(self.tac & 3) as usize] * if self.tac & 0x80 != 0 { 2.0 } else { 1.0 };
            clock / (256.0 - self.tma as f64) / 60.0
        } else {
            59.73 / 60.0
        }
    }

    /// Where the CPU sees data offset `d` when its bank is mapped.
    fn cpu_of(&self, d: usize) -> u16 {
        let i = d + self.load as usize;
        if i < 0x4000 {
            i as u16
        } else {
            0x4000 + (i % 0x4000) as u16
        }
    }
}

/// The file's code run on a bare SM83: its (padded) data with the bank at $4000 switched by
/// writes to $2000-$3FFF, RAM and a sound register file.
struct Player {
    load: usize,
    data_len: usize,
    rom: Vec<u8>,
    bank: usize,
    ram: Vec<u8>,
    io: [u8; 0x80],
    hram: [u8; 0x80],
    writes: Vec<Write>,
    /// Data bytes read, when tracked.
    used: Option<Vec<bool>>,
    /// Every write, while recording.
    log: Option<Vec<(u16, u8)>>,
    /// How writes to $2000-$3FFF pick the bank: the GBS's way, or the game's cartridge's.
    mbc: Mbc,
}

/// A cartridge's bank controller, as far as picking the ROM bank at $4000 goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mbc {
    /// A GBS: any value, 0 meaning 1.
    Gbs,
    /// MBC1 (and the rest that take 5-7 bits, 0 meaning 1).
    Mbc1,
    /// MBC5: $2000-$2FFF the low 8 bits, $3000-$3FFF the 9th; 0 is bank 0.
    Mbc5,
}

impl Player {
    fn new(h: &Header, data: &[u8]) -> Self {
        let mut rom = vec![0u8; h.load as usize];
        rom.extend_from_slice(data);
        let mut io = [0; 0x80];
        io[0x26] = 0x80;
        Player { load: h.load as usize, data_len: data.len(), rom, bank: 1, ram: vec![0; 0x4000], io, hram: [0; 0x80], writes: Vec::new(), used: None, log: None, mbc: Mbc::Gbs }
    }

    /// The game's own ROM in its cartridge, for the board player.
    fn board(rom: &[u8]) -> Self {
        let mut io = [0; 0x80];
        io[0x26] = 0x80;
        let mbc = if matches!(rom.get(0x147), Some(0x19..=0x1E)) { Mbc::Mbc5 } else { Mbc::Mbc1 };
        Player { load: 0, data_len: rom.len(), rom: rom.to_vec(), bank: 1, ram: vec![0; 0x4000], io, hram: [0; 0x80], writes: Vec::new(), used: None, log: None, mbc }
    }

    fn select(&mut self, addr: u16, v: u8) {
        let banks = (self.rom.len() / 0x4000).max(2);
        self.bank = match self.mbc {
            Mbc::Gbs => (v as usize).max(1),
            Mbc::Mbc1 => ((v & 0x7F) as usize).max(1) % banks,
            Mbc::Mbc5 if addr < 0x3000 => ((self.bank & 0x100) | v as usize) % banks,
            Mbc::Mbc5 => ((self.bank & 0xFF) | ((v as usize & 1) << 8)) % banks,
        };
    }

    /// The index into the padded data the CPU reads at `addr`.
    fn index(&self, addr: u16) -> Option<usize> {
        match addr {
            0x0000..=0x3FFF => Some(addr as usize),
            0x4000..=0x7FFF => Some(self.bank * 0x4000 + addr as usize - 0x4000),
            _ => None,
        }
    }

    /// 1 + the data offset the CPU reads at `addr` (0: none).
    fn src(&self, addr: u16) -> u32 {
        match self.index(addr).and_then(|i| i.checked_sub(self.load)) {
            Some(d) if d < self.data_len => d as u32 + 1,
            _ => 0,
        }
    }

    /// Runs the routine at `pc` with `regs` (A B C D E H L) and `pushed` on the stack above
    /// its return address, until it returns; false when it doesn't.
    fn call_with(&mut self, cpu: &mut Cpu, sp: u16, pc: u16, regs: [u8; 7], pushed: &[u8]) -> bool {
        cpu.sp = sp.wrapping_sub(2);
        self.write(cpu.sp, 0x00);
        self.write(cpu.sp.wrapping_add(1), 0x01);
        for &v in pushed {
            cpu.sp = cpu.sp.wrapping_sub(1);
            self.write(cpu.sp, v);
        }
        cpu.pc = pc;
        [cpu.a, cpu.b, cpu.c, cpu.d, cpu.e, cpu.h, cpu.l] = regs;
        cpu.halted = false;
        cpu.stopped = false;
        cpu.calls = vec![(pc, sp)];
        let mut used = 0u32;
        while !cpu.calls.is_empty() {
            if used > 1_000_000 || cpu.halted || cpu.stopped {
                return false;
            }
            used += cpu.step(self);
        }
        true
    }
}

impl Bus for Player {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x7FFF => {
                let Some(i) = self.index(addr) else { return 0xFF };
                if let (Some(used), Some(d)) = (self.used.as_mut(), i.checked_sub(self.load)) {
                    if let Some(u) = used.get_mut(d) {
                        *u = true;
                    }
                }
                self.rom.get(i).copied().unwrap_or(0xFF)
            }
            0xA000..=0xDFFF => self.ram[addr as usize - 0xA000],
            0xE000..=0xFDFF => self.ram[addr as usize - 0xC000],
            0xFF00..=0xFF7F => self.io[addr as usize - 0xFF00],
            0xFF80..=0xFFFF => self.hram[addr as usize - 0xFF80],
            _ => 0xFF,
        }
    }

    fn write(&mut self, addr: u16, v: u8) {
        if let Some(log) = self.log.as_mut() {
            log.push((addr, v));
        }
        match addr {
            0x2000..=0x3FFF => self.select(addr, v),
            0xA000..=0xDFFF => self.ram[addr as usize - 0xA000] = v,
            0xE000..=0xFDFF => self.ram[addr as usize - 0xC000] = v,
            0xFF00..=0xFF7F => {
                self.io[addr as usize - 0xFF00] = v;
                if (0xFF10..=0xFF3F).contains(&addr) {
                    self.writes.push(Write { at: 0, port: addr as u32, value: v as u16 });
                }
            }
            0xFF80..=0xFFFF => self.hram[addr as usize - 0xFF80] = v,
            _ => {}
        }
    }
}

/// One step of a track's script: a write the file's own code makes, a byte it leaves on the
/// stack for the next call, or a call into the game's routine at `addr` with A B C D E H L.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Poke(u16, u8),
    Push(u8),
    /// `src`: 1 + where in the file's data the routine starts.
    Call { addr: u16, regs: [u8; 7], src: u32 },
}

/// Which player plays a track from the ROM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Way {
    /// The file's layout rebuilt from the ROM (gaps between runs from the ROM too).
    Layout,
    /// The same with the gaps left empty.
    Zeros,
    /// The game's own ROM in its cartridge, the file's bank numbers and calls translated to
    /// where the game has them (a file whose author renumbered the banks).
    Board,
    None,
}

/// What fills the file's bytes no run places.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill<'a> {
    Zeros,
    /// A gap between two runs at the same distance in the ROM: the ROM's bytes there.
    Bridged,
    /// The file's own bytes (an oracle, for checks).
    File(&'a [u8]),
}

/// A Game Boy game's music map: numbers only, nothing of the game or the file.
#[derive(Debug, Clone)]
pub struct Map {
    pub header: Header,
    pub data_len: usize,
    pub runs: Vec<Run>,
    pub play: Vec<Op>,
    pub tracks: Vec<Option<Vec<Op>>>,
    /// Per track, its play steps where they differ from `play`.
    pub plays: Vec<Option<Vec<Op>>>,
    /// Per track, which player plays it right; empty until chosen.
    pub ways: Vec<Way>,
}

impl Map {
    pub fn play_of(&self, song: u8) -> &[Op] {
        self.plays.get(song as usize).and_then(|p| p.as_deref()).unwrap_or(&self.play)
    }
}

/// Bytes after a jump target within which running on into the game's code still counts as
/// entering it (the target's own instruction patched by the file's author).
/// Bytes from a call's target that must be placed for the call to be the game's.
const ENTRY_BYTES: usize = 3;
const PATCHED_BYTES: u16 = 3;

/// Play calls recorded per track: the first few take the start request, then play settles.
const PLAY_CALLS: usize = 6;

/// An SM83 instruction's length in bytes.
fn length(op: u8) -> u16 {
    match op {
        0x01 | 0x11 | 0x21 | 0x31 | 0x08 | 0xC2 | 0xC3 | 0xC4 | 0xCA | 0xCC | 0xCD | 0xD2 | 0xD4 | 0xDA | 0xDC | 0xEA | 0xFA => 3,
        0x06 | 0x0E | 0x16 | 0x1E | 0x26 | 0x2E | 0x36 | 0x3E | 0x18 | 0x20 | 0x28 | 0x30 | 0x38 | 0xC6 | 0xCE | 0xD6 | 0xDE | 0xE6 | 0xEE | 0xF6 | 0xFE | 0xE0
        | 0xF0 | 0xE8 | 0xF8 | 0xCB | 0x10 => 2,
        _ => 1,
    }
}

/// Calls, jumps, returns and restarts (not relative branches).
fn is_jump(op: u8) -> bool {
    matches!(op, 0xCD | 0xC4 | 0xCC | 0xD4 | 0xDC | 0xC3 | 0xC2 | 0xCA | 0xD2 | 0xDA | 0xE9 | 0xC9 | 0xC0 | 0xC8 | 0xD0 | 0xD8 | 0xD9) || op & 0xC7 == 0xC7
}

/// Runs the routine at `pc` with A = `a` and records it as a script, as `nes_nsf`'s
/// recorder does: while the code running isn't the game's (`placed`), its writes are pokes;
/// an entry into the game's code by a call, jump or return (or running on into it close
/// after one, or where it lies as the game's banks have it: `aligned`) is a call, run
/// through without recording. None when it doesn't return.
fn record(p: &mut Player, cpu: &mut Cpu, sp: u16, pc: u16, a: u8, placed: &[bool], aligned: &[bool]) -> Option<Vec<Op>> {
    cpu.sp = sp.wrapping_sub(2);
    p.write(cpu.sp, 0x00);
    p.write(cpu.sp.wrapping_add(1), 0x01);
    cpu.pc = pc;
    cpu.a = a;
    cpu.halted = false;
    cpu.stopped = false;
    cpu.calls = vec![(pc, sp)];
    // Code is the game's when it and the bytes after it are placed: a run that ends right
    // where a routine starts (the file's own code follows) doesn't make the routine the game's.
    let of = |p: &Player, addr: u16, set: &[bool]| {
        let s = p.src(addr);
        s > 0 && (0..ENTRY_BYTES).all(|k| set.get(s as usize - 1 + k).copied().unwrap_or(k > 0))
    };
    let mut ops = Vec::new();
    let mut in_game: Option<usize> = None;
    let mut used = 0u32;
    let mut jumped = true;
    let mut target: Option<(u16, u16)> = None;
    while !cpu.calls.is_empty() {
        if used > 1_000_000 || cpu.halted || cpu.stopped {
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
                // What the file's code pushed since it got here, still on the stack.
                if let Some((_, s)) = target.filter(|t| t.1 > cpu.sp) {
                    ops.extend((cpu.sp..s).rev().map(|i| Op::Push(p.read(i))));
                }
                ops.push(Op::Call { addr: at, regs: [cpu.a, cpu.b, cpu.c, cpu.d, cpu.e, cpu.h, cpu.l], src: p.src(at) });
                in_game = Some(cpu.calls.len());
            }
        }
        let op = p.read(at);
        p.log = in_game.is_none().then(Vec::new);
        used += cpu.step(p);
        jumped = is_jump(op) && cpu.pc != at.wrapping_add(length(op));
        if jumped && in_game.is_none() {
            target = Some((cpu.pc, cpu.sp));
        }
        if let Some(log) = p.log.take() {
            // The live stack (return addresses, pushes) is set up by each call itself.
            let live = cpu.sp;
            ops.extend(log.into_iter().filter(|&(a, _)| !(a >= live && a < sp)).map(|(a, v)| Op::Poke(a, v)));
        }
    }
    Some(ops)
}

/// Plays a script.
fn replay(p: &mut Player, cpu: &mut Cpu, sp: u16, ops: &[Op]) -> bool {
    let mut pushed = Vec::new();
    for op in ops {
        match *op {
            Op::Poke(a, v) => p.write(a, v),
            Op::Push(v) => pushed.push(v),
            Op::Call { addr, regs, .. } => {
                if !p.call_with(cpu, sp, addr, regs, &std::mem::take(&mut pushed)) {
                    return false;
                }
            }
        }
    }
    true
}

/// Which data bytes the file's tracks read in their first `frames` 1/60 s.
pub fn used_bytes(gbs: &[u8], frames: usize) -> Option<Vec<bool>> {
    let h = Header::read(gbs)?;
    let data = &gbs[0x70..];
    let mut used = vec![false; data.len()];
    for t in 0..h.songs {
        let mut p = Player::new(&h, data);
        p.used = Some(std::mem::take(&mut used));
        let mut cpu = Cpu::default();
        if p.call_with(&mut cpu, h.sp, h.init, [t, 0, 0, 0, 0, 0, 0], &[]) {
            let mut due = 0.0;
            'frames: for _ in 0..frames {
                due += h.rate();
                while due >= 1.0 {
                    due -= 1.0;
                    if !p.call_with(&mut cpu, h.sp, h.play, [0; 7], &[]) {
                        break 'frames;
                    }
                }
            }
        }
        used = p.used.take().unwrap_or_default();
    }
    Some(used)
}

/// The music map of a Game Boy ROM for the GBS it matches. None when the GBS doesn't run.
pub fn map(gbs: &[u8], rom: &[u8]) -> Option<Map> {
    let h = Header::read(gbs)?;
    let data = &gbs[0x70..];
    let used = used_bytes(gbs, music_search::HINT_TRACK_FRAMES)?;
    let layout = super::music_map::layout(data, &used, rom);
    let mut placed = vec![false; data.len()];
    for r in &layout.runs {
        placed[r.data..r.data + r.len].iter_mut().for_each(|b| *b = true);
    }
    // Runs where the game's banks put them: at the distance (within a 16 KB bank) most of
    // the runs' bytes share.
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
    Some(Map { header: h, data_len: data.len(), runs: layout.runs, play, tracks, plays, ways: Vec::new() })
}

/// Whether two play calls take the same steps: the same writes and the same routines,
/// whatever the registers the calls happen to leave from the last one (a routine's own
/// leftovers, not a request).
fn same_steps(a: &[Op], b: &[Op]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| match (x, y) {
            (Op::Call { addr: p, .. }, Op::Call { addr: q, .. }) => p == q,
            _ => x == y,
        })
}

/// The file's data as the map rebuilds it from the ROM.
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

/// A track playing from the ROM by its map: each 1/60 s frame's sound writes.
pub struct LayoutSong {
    p: Player,
    cpu: Cpu,
    sp: u16,
    rate: f64,
    due: f64,
    play: Vec<Op>,
    first: Vec<Write>,
    /// On the game's board: the map's header and runs, to translate by.
    board: Option<(Header, Vec<Run>)>,
}

/// Where the game has the file's data offset `d`: the ROM bank and the CPU address it is seen
/// at with that bank in place. None when no run holds it.
fn game_place(runs: &[Run], h: &Header, d: usize) -> Option<(usize, u16)> {
    let r = runs.iter().find(|r| (r.data..r.data + r.len).contains(&d))?;
    let rom = r.rom + d - r.data;
    let file_cpu = h.cpu_of(d);
    if rom < 0x4000 {
        // Bank 0 only where the file has it below $4000 too.
        return (file_cpu < 0x4000).then_some((1, rom as u16));
    }
    Some((rom / 0x4000, 0x4000 + (rom % 0x4000) as u16))
}

/// The game's ROM bank holding most of the file's bank `b`.
fn game_bank(runs: &[Run], h: &Header, b: u8) -> Option<usize> {
    let from = (b.max(1) as usize * 0x4000).checked_sub(h.load as usize)?;
    let to = from + 0x4000;
    runs.iter()
        .filter_map(|r| {
            let (lo, hi) = (r.data.max(from), (r.data + r.len).min(to));
            (hi > lo).then(|| (hi - lo, (r.rom + lo - r.data) / 0x4000))
        })
        .max()
        .map(|(_, bank)| bank)
}

/// [`replay`] on the game's board: the file's bank writes as the game's banks, its calls to
/// where the game has each routine.
fn board_replay(p: &mut Player, cpu: &mut Cpu, h: &Header, runs: &[Run], ops: &[Op]) -> bool {
    let mut pushed = Vec::new();
    for op in ops {
        match *op {
            Op::Poke(0x2000..=0x3FFF, b) => {
                if let Some(bank) = game_bank(runs, h, b) {
                    p.bank = bank.max(1);
                }
            }
            Op::Poke(0x0000..=0x7FFF, _) => {}
            Op::Poke(a, v) => p.write(a, v),
            Op::Push(v) => pushed.push(v),
            Op::Call { addr, regs, src } => {
                let at = match src.checked_sub(1).and_then(|d| game_place(runs, h, d as usize)) {
                    Some((bank, at)) => {
                        if at >= 0x4000 {
                            p.bank = bank;
                        }
                        at
                    }
                    None => addr,
                };
                if !p.call_with(cpu, h.sp, at, regs, &std::mem::take(&mut pushed)) {
                    return false;
                }
            }
        }
    }
    true
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
        Some(Self { p, cpu, sp: m.header.sp, rate: m.header.rate(), due: 0.0, play: m.play_of(song).to_vec(), first, board: None })
    }

    /// Track `song` on the game's own board ([`Way::Board`]).
    pub fn open_board(m: &Map, rom: &[u8], song: u8) -> Option<Self> {
        let script = m.tracks.get(song as usize)?.as_ref()?;
        let mut p = Player::board(rom);
        let mut cpu = Cpu::default();
        if !board_replay(&mut p, &mut cpu, &m.header, &m.runs, script) {
            return None;
        }
        let first = std::mem::take(&mut p.writes);
        Some(Self { p, cpu, sp: m.header.sp, rate: m.header.rate(), due: 0.0, play: m.play_of(song).to_vec(), first, board: Some((m.header, m.runs.clone())) })
    }

    pub fn frame(&mut self) -> Option<Vec<Write>> {
        self.due += self.rate;
        while self.due >= 1.0 {
            self.due -= 1.0;
            let ran = match &self.board {
                Some((h, runs)) => board_replay(&mut self.p, &mut self.cpu, h, runs, &self.play),
                None => replay(&mut self.p, &mut self.cpu, self.sp, &self.play),
            };
            if !ran {
                return None;
            }
        }
        let mut f = std::mem::take(&mut self.first);
        f.append(&mut self.p.writes);
        Some(f)
    }
}

/// Track `song` played on the game's own board: each 1/60 s frame's sound writes.
pub fn board_track_writes(m: &Map, rom: &[u8], song: u8, frames: usize) -> Option<Vec<Vec<Write>>> {
    let mut s = LayoutSong::open_board(m, rom, song)?;
    Some((0..frames).map_while(|_| s.frame()).collect())
}

/// Track `song` played from the ROM by its map: each 1/60 s frame's sound writes.
pub fn map_track_writes(m: &Map, rom: &[u8], song: u8, frames: usize, fill: Fill) -> Option<Vec<Vec<Write>>> {
    let mut s = LayoutSong::open(m, rom, song, fill)?;
    Some((0..frames).map_while(|_| s.frame()).collect())
}

/// How a map plays the GBS's tracks: per track, how alike its melody is to the file's own
/// (None for a track with no melody in the file).
pub fn map_check(gbs: &[u8], m: &Map, rom: &[u8], frames: usize, fill: Fill) -> Vec<Option<f64>> {
    use super::gb_gbs::{melody, track_writes};
    (0..m.header.songs)
        .map(|t| {
            let real = melody(&track_writes(gbs, t, frames)?);
            if real.len() < music_search::MIN_MELODY {
                return None;
            }
            let mine = map_track_writes(m, rom, t, frames, fill).map(|w| melody(&w)).unwrap_or_default();
            Some(music_search::alike(&real, &mine))
        })
        .collect()
}

/// Picks, per track, the layout that plays it like the file (at least `right`).
pub fn choose_ways(gbs: &[u8], m: &mut Map, rom: &[u8], frames: usize, right: f64) {
    let bridged = map_check(gbs, m, rom, frames, Fill::Bridged);
    let misses = |v: &[Option<f64>]| v.iter().any(|b| b.unwrap_or(0.0) < right);
    let zeros = if misses(&bridged) { map_check(gbs, m, rom, frames, Fill::Zeros) } else { Vec::new() };
    let both_miss = (0..bridged.len()).any(|i| {
        let s = |v: &Vec<Option<f64>>| v.get(i).copied().flatten().unwrap_or(0.0);
        s(&bridged) < right && s(&zeros) < right
    });
    let board = if both_miss { board_check(gbs, m, rom, frames) } else { Vec::new() };
    m.ways = (0..bridged.len())
        .map(|i| {
            let score = |v: &Vec<Option<f64>>| v.get(i).copied().flatten().unwrap_or(0.0);
            if score(&bridged) >= right {
                Way::Layout
            } else if score(&zeros) >= right {
                Way::Zeros
            } else if score(&board) >= right {
                Way::Board
            } else {
                Way::None
            }
        })
        .collect();
}

/// How the map plays the GBS's tracks on the game's own board ([`Way::Board`]).
pub fn board_check(gbs: &[u8], m: &Map, rom: &[u8], frames: usize) -> Vec<Option<f64>> {
    use super::gb_gbs::{melody, track_writes};
    (0..m.header.songs)
        .map(|t| {
            let real = melody(&track_writes(gbs, t, frames)?);
            if real.len() < music_search::MIN_MELODY {
                return None;
            }
            let mine = board_track_writes(m, rom, t, frames).map(|w| melody(&w)).unwrap_or_default();
            Some(music_search::alike(&real, &mine))
        })
        .collect()
}

/// How alike to the file a track must play to count as right.
pub const MAP_RIGHT: f64 = 0.9;
const WAY_FRAMES: usize = 600;

/// The map of `rom` for the GBS it matches, each track's player chosen.
pub fn build_map(gbs: &[u8], rom: &[u8]) -> Option<Map> {
    let mut m = map(gbs, rom)?;
    choose_ways(gbs, &mut m, rom, WAY_FRAMES, MAP_RIGHT);
    Some(m)
}

/// Track `song` by its map and chosen way: each 1/60 s frame's sound writes.
pub fn play_map(m: &Map, rom: &[u8], song: u8, frames: usize) -> Option<Vec<Vec<Write>>> {
    let fill = match m.ways.get(song as usize)? {
        Way::Layout => Fill::Bridged,
        Way::Zeros => Fill::Zeros,
        Way::Board => return board_track_writes(m, rom, song, frames),
        Way::None => return None,
    };
    map_track_writes(m, rom, song, frames, fill)
}

/// A track from the user's ROM by its map, as sound (stereo).
pub struct MapSong {
    song: LayoutSong,
    apu: super::gb_apu::Apu,
}

/// The Game Boy's clock ticks in one 1/60 s frame.
const FRAME_CLOCKS: u32 = 4_194_304 / 60;

impl MapSong {
    pub fn open(m: &Map, rom: &[u8], song: u8, rate: u32) -> Result<Self, String> {
        let song = match m.ways.get(song as usize) {
            Some(Way::Layout) => LayoutSong::open(m, rom, song, Fill::Bridged),
            Some(Way::Zeros) => LayoutSong::open(m, rom, song, Fill::Zeros),
            Some(Way::Board) => LayoutSong::open_board(m, rom, song),
            _ => return Err("the map has no way to play this song".into()),
        }
        .ok_or("the song didn't start")?;
        Ok(Self { song, apu: super::gb_apu::Apu::new(rate)? })
    }

    pub fn frame_rate(&self) -> f64 {
        60.0
    }

    /// One frame: its samples (interleaved stereo) appended to `out`, and its sound writes.
    pub fn frame(&mut self, out: &mut Vec<i16>) -> Option<Vec<Write>> {
        let w = self.song.frame()?;
        for x in &w {
            self.apu.write(x.at.min(FRAME_CLOCKS - 1), x.port as u16, x.value as u8);
        }
        self.apu.end_frame(FRAME_CLOCKS, out);
        Some(w)
    }
}

impl Map {
    /// The map as text: numbers only. See [`Self::decode`].
    pub fn encode(&self) -> String {
        let h = &self.header;
        let ops = |ops: &[Op]| {
            ops.iter()
                .map(|o| match *o {
                    Op::Poke(a, v) => format!("P{a:x}:{v:x}"),
                    Op::Push(v) => format!("U{v:x}"),
                    Op::Call { addr, regs, src } => {
                        format!("C{addr:x}:{}:{src:x}", regs.iter().map(|r| format!("{r:x}")).collect::<Vec<_>>().join(":"))
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        let mut out = vec![
            "gbmap 1".to_string(),
            format!("h {:x} {:x} {:x} {:x} {} {:x} {:x}", h.load, h.init, h.play, h.sp, h.songs, h.tma, h.tac),
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
        out.push(format!("s {}", self.ways.iter().map(|w| match w { Way::Layout => 'l', Way::Zeros => 'z', Way::Board => 'b', Way::None => '-' }).collect::<String>()));
        out.join("\n")
    }

    /// The map [`Self::encode`] wrote. None when the text isn't one.
    pub fn decode(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != "gbmap 1" {
            return None;
        }
        let num = |t: &str| u32::from_str_radix(t, 16).ok();
        let ops = |rest: &[&str]| -> Option<Vec<Op>> {
            rest.iter()
                .map(|t| {
                    let (k, v) = t.split_at(1);
                    let f: Vec<u32> = v.split(':').map(num).collect::<Option<_>>()?;
                    Some(match (k, f.as_slice()) {
                        ("P", [a, v]) => Op::Poke(*a as u16, *v as u8),
                        ("U", [v]) => Op::Push(*v as u8),
                        ("C", [addr, r @ .., src]) if r.len() == 7 => {
                            let mut regs = [0u8; 7];
                            regs.iter_mut().zip(r).for_each(|(d, s)| *d = *s as u8);
                            Op::Call { addr: *addr as u16, regs, src: *src }
                        }
                        _ => return None,
                    })
                })
                .collect()
        };
        let mut m = Map { header: Header { load: 0, init: 0, play: 0, sp: 0, songs: 0, tma: 0, tac: 0 }, data_len: 0, runs: Vec::new(), play: Vec::new(), tracks: Vec::new(), plays: Vec::new(), ways: Vec::new() };
        for line in lines {
            let f: Vec<&str> = line.split(' ').filter(|t| !t.is_empty()).collect();
            match f.first().copied() {
                Some("h") if f.len() == 8 => {
                    m.header = Header {
                        load: num(f[1])? as u16,
                        init: num(f[2])? as u16,
                        play: num(f[3])? as u16,
                        sp: num(f[4])? as u16,
                        songs: f[5].parse().ok()?,
                        tma: num(f[6])? as u8,
                        tac: num(f[7])? as u8,
                    };
                    m.tracks = vec![None; m.header.songs as usize];
                    m.plays = vec![None; m.header.songs as usize];
                }
                Some("d") => m.data_len = num(f.get(1)?)? as usize,
                Some("r") => {
                    for t in &f[1..] {
                        let v: Vec<u32> = t.split(':').map(num).collect::<Option<_>>()?;
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

    /// A tiny game: a sound routine at $4100 in bank 2 plays song A as a square wave whose
    /// pitch steps each call, and its own init at $4000 stores the song number to $C000. The
    /// GBS's author wrote their own init (switch bank, poke a variable, call the game's
    /// init) and play (call the game's play), at $0400 in the file.
    fn game_and_file() -> (Vec<u8>, Vec<u8>) {
        let mut rom = vec![0u8; 0x10000];
        // Noise in every byte the music doesn't use, so runs match only where they should.
        let mut x = 7u32;
        for b in rom.iter_mut() {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            *b = (x >> 16) as u8;
        }
        let bank2 = 0x8000;
        // init ($4000): LD ($C000),A; XOR A; LD ($C001),A; RET
        let init = [0xEA, 0x00, 0xC0, 0xAF, 0xEA, 0x01, 0xC0, 0xC9];
        // play ($4100): LD A,($C001); INC A; LD ($C001),A; LD B,A; LD A,($C000); ADD A,B;
        // LDH ($13),A; LD A,$87; LDH ($14),A; RET
        let play = [0xFA, 0x01, 0xC0, 0x3C, 0xEA, 0x01, 0xC0, 0x47, 0xFA, 0x00, 0xC0, 0x80, 0xE0, 0x13, 0x3E, 0x87, 0xE0, 0x14, 0xC9];
        rom[bank2..bank2 + init.len()].copy_from_slice(&init);
        rom[bank2 + 0x100..bank2 + 0x100 + play.len()].copy_from_slice(&play);
        // The file: load $0400; its bank 1 is the game's bank 2 (a 16 KB stretch of it).
        let mut f = vec![0u8; 0x70];
        f[..4].copy_from_slice(b"GBS\x01");
        f[4] = 3;
        f[6..8].copy_from_slice(&0x0400u16.to_le_bytes());
        f[8..10].copy_from_slice(&0x0400u16.to_le_bytes());
        f[10..12].copy_from_slice(&0x0420u16.to_le_bytes());
        f[12..14].copy_from_slice(&0xFFFEu16.to_le_bytes());
        let mut data = vec![0u8; 0x4000 - 0x400 + 0x4000];
        // init ($0400): LD B,A; LD A,1; LD ($2000),A; LD A,$40; LD ($C002),A; LD A,B;
        // ADD A,A; ADD A,$20; CALL $4000; RET
        let my_init = [0x47, 0x3E, 0x01, 0xEA, 0x00, 0x20, 0x3E, 0x40, 0xEA, 0x02, 0xC0, 0x78, 0x87, 0xC6, 0x20, 0xCD, 0x00, 0x40, 0xC9];
        data[..my_init.len()].copy_from_slice(&my_init);
        // play ($0420): CALL $4100; RET
        data[0x20..0x24].copy_from_slice(&[0xCD, 0x00, 0x41, 0xC9]);
        let b1 = 0x4000 - 0x400;
        data[b1..b1 + 0x4000].copy_from_slice(&rom[bank2..bank2 + 0x4000]);
        f.extend(data);
        (rom, f)
    }

    #[test]
    fn a_map_plays_the_files_tracks_from_the_rom_and_round_trips() {
        let (rom, f) = game_and_file();
        let mut m = map(&f, &rom).expect("map");
        // The author's init became pokes and one call into the game's init.
        let t1 = m.tracks[1].as_ref().unwrap();
        assert!(t1.contains(&Op::Poke(0x2000, 1)), "{t1:?}");
        assert!(t1.contains(&Op::Poke(0xC002, 0x40)), "{t1:?}");
        assert!(t1.iter().any(|o| matches!(o, Op::Call { addr: 0x4000, regs, .. } if regs[0] == 0x22)), "{t1:?}");
        let check = map_check(&f, &m, &rom, 60, Fill::Zeros);
        assert!(check.iter().all(|c| c.is_some_and(|a| a > 0.99)), "{check:?}");
        // On the game's board: the file's bank 1 is the game's bank 2.
        assert_eq!(game_bank(&m.runs, &m.header, 1), Some(2));
        for t in 0..3 {
            assert_eq!(board_track_writes(&m, &rom, t, 60), super::super::gb_gbs::track_writes(&f, t, 60));
        }
        choose_ways(&f, &mut m, &rom, 60, MAP_RIGHT);
        assert!(m.ways.iter().all(|w| *w != Way::None), "{:?}", m.ways);
        let text = m.encode();
        let back = Map::decode(&text).unwrap();
        assert_eq!(back.encode(), text);
        for t in 0..3 {
            assert_eq!(play_map(&back, &rom, t, 60), super::super::gb_gbs::track_writes(&f, t, 60));
        }
    }
}
