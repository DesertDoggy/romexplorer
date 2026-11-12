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
    /// The header's sound chip byte: the cartridge chips' writes are kept too.
    chips: u8,
    n163_addr: u8,
    n163_ram: [u8; 128],
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
            // The Namco 163's memory, read through its data port (which counts up as on a
            // write): drivers read back what they change.
            0x4800..=0x4FFF if self.chips & 16 != 0 => {
                let v = self.n163_ram[(self.n163_addr & 0x7F) as usize];
                self.n163_step();
                v
            }
            _ => 0,
        }
    }

    fn write(&mut self, addr: u16, v: u8) {
        if let Some(log) = self.log.as_mut() {
            log.push((addr, v));
        }
        if let Some(t) = chip_target(self.chips, addr) {
            use super::nes_apu::Target;
            match t {
                Target::N163Addr => self.n163_addr = v,
                Target::N163Data => {
                    // Each write with its own address, so a chip fed only the writes lands
                    // them where reads moved the address to.
                    let a = self.n163_addr & 0x7F;
                    self.writes.push(Write { at: 0, port: super::nes_music::port(Target::N163Addr, 0xF800), value: a as u16 });
                    self.writes.push(Write { at: 0, port: super::nes_music::port(t, addr), value: v as u16 });
                    self.n163_ram[a as usize] = v;
                    self.n163_step();
                }
                _ => self.writes.push(Write { at: 0, port: super::nes_music::port(t, addr), value: v as u16 }),
            }
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
    fn n163_step(&mut self) {
        if self.n163_addr & 0x80 != 0 {
            self.n163_addr = 0x80 | (self.n163_addr.wrapping_add(1) & 0x7F);
        }
    }

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
    /// The header's sound chip byte: 1 VRC6, 2 VRC7, 4 Disk System, 8 MMC5, 16 Namco 163,
    /// 32 Sunsoft 5B.
    pub chips: u8,
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
            chips: nsf[0x7B],
        })
    }
}

/// Where a write to `addr` goes among the cartridge sound chips `chips` (an NSF header's
/// chip byte) at the addresses an NSF uses for them.
fn chip_target(chips: u8, addr: u16) -> Option<super::nes_apu::Target> {
    use super::nes_apu::Target;
    Some(match addr {
        0x9000..=0x9002 | 0xA000..=0xA002 | 0xB000..=0xB002 if chips & 1 != 0 => Target::Vrc6 { osc: ((addr >> 12) - 9) as u8, reg: (addr & 3) as u8 },
        0x5000..=0x5015 if chips & 8 != 0 => Target::Mmc5,
        0x4800..=0x4FFF if chips & 16 != 0 => Target::N163Data,
        0xF800..=0xFFFF if chips & 16 != 0 => Target::N163Addr,
        0xC000..=0xDFFF if chips & 32 != 0 => Target::Fme7Latch,
        0xE000..=0xFFFF if chips & 32 != 0 => Target::Fme7Data,
        _ => return None,
    })
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
        chips: h.chips,
        n163_addr: 0,
        n163_ram: [0; 128],
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

/// [`melody`] with the tone channels of the sound chips `chips` (an NSF header's chip byte)
/// too: a game whose music is on its cartridge's chip has little on the console's own. The
/// chips' writes are as [`super::nes_music`] tags them (the target in the port's high bits).
pub fn melody_chips(frames: &[Vec<Write>], chips: u8) -> HashSet<u64> {
    if chips & 0x3D == 0 {
        return melody(frames);
    }
    // Each chip channel's pitch as a synthetic channel: ports 0x100000 + 2n (low byte) and
    // + 1 (the next 3 bits), of the period or frequency shifted to about 11 bits.
    let (mut n163_addr, mut n163) = (0u8, [0u8; 128]);
    let (mut fme7_latch, mut fme7) = (0u8, [0u8; 16]);
    let mut vrc6 = [[0u8; 3]; 3];
    let mut regs: HashMap<u16, u8> = HashMap::new();
    let pitched: Vec<Vec<Write>> = frames
        .iter()
        .map(|f| {
            let mut out = Vec::with_capacity(f.len());
            let put = |out: &mut Vec<Write>, ch: u32, pitch: u32| {
                let v = (pitch & 0x7FF) as u16;
                out.push(Write { at: 0, port: 0x100000 + ch * 2, value: v & 0xFF });
                out.push(Write { at: 0, port: 0x100001 + ch * 2, value: v >> 8 });
            };
            for w in f {
                out.push(*w);
                let (code, addr, v) = (w.port >> 16, w.port as u16, w.value as u8);
                match code {
                    // VRC6: two pulses and the saw, 12-bit periods.
                    0x10..=0x1F if chips & 1 != 0 => {
                        let (osc, reg) = (((code >> 2) & 3) as usize, (code & 3) as usize);
                        if osc < 3 && reg < 3 {
                            vrc6[osc][reg] = v;
                            if reg > 0 {
                                put(&mut out, osc as u32, (vrc6[osc][2] as u32 & 0x0F) << 8 | vrc6[osc][1] as u32);
                            }
                        }
                    }
                    // Namco 163: its 18-bit frequencies in its own RAM, through an address
                    // port that can count up.
                    0x20 if chips & 16 != 0 => n163_addr = v,
                    0x21 if chips & 16 != 0 => {
                        let a = (n163_addr & 0x7F) as usize;
                        n163[a] = v;
                        if n163_addr & 0x80 != 0 {
                            n163_addr = 0x80 | ((n163_addr + 1) & 0x7F);
                        }
                        if a >= 0x40 && a & 7 <= 4 {
                            let b = a & !7;
                            let freq = (n163[b + 4] as u32 & 3) << 16 | (n163[b + 2] as u32) << 8 | n163[b] as u32;
                            put(&mut out, 3 + (b as u32 - 0x40) / 8, freq >> 7);
                        }
                    }
                    // Sunsoft 5B: three 12-bit tone periods through a latch.
                    0x22 if chips & 32 != 0 => fme7_latch = v & 0x0F,
                    0x23 if chips & 32 != 0 => {
                        fme7[fme7_latch as usize] = v;
                        if fme7_latch < 6 {
                            let c = (fme7_latch / 2) as usize;
                            put(&mut out, 11 + c as u32, (fme7[c * 2 + 1] as u32 & 0x0F) << 8 | fme7[c * 2] as u32);
                        }
                    }
                    // MMC5's two pulses, as the APU's.
                    0x24 if chips & 8 != 0 && matches!(addr, 0x5002 | 0x5003 | 0x5006 | 0x5007) => {
                        regs.insert(addr, v);
                        let base = addr & !1;
                        let reg = |a: u16| regs.get(&a).copied().unwrap_or(0) as u32;
                        put(&mut out, 14 + (base as u32 - 0x5002) / 4, (reg(base + 1) & 7) << 8 | reg(base));
                    }
                    // The Disk System's wave channel: a 12-bit frequency (untagged from a
                    // file, tagged from the board).
                    0 | 0x25 if chips & 4 != 0 && matches!(addr, 0x4082 | 0x4083) => {
                        regs.insert(addr, v);
                        let reg = |a: u16| regs.get(&a).copied().unwrap_or(0) as u32;
                        put(&mut out, 16, (reg(0x4083) & 0x0F) << 8 | reg(0x4082));
                    }
                    _ => {}
                }
            }
            out
        })
        .collect();
    let mut channels = vec![(0x4002, 0x4003), (0x4006, 0x4007), (0x400A, 0x400B)];
    channels.extend((0..17u32).map(|c| (0x100000 + c * 2, 0x100001 + c * 2)));
    music_search::melody(&pitched, &channels)
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
    /// How the map plays on the game's own board ([`cart_track_writes`]).
    pub board: BoardWay,
    /// Per track, which player plays it right ([`choose_ways`]); empty until chosen.
    pub ways: Vec<Way>,
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
    let mut m = Map { header: h, data_len: data.len(), runs: layout.runs, play, tracks, plays, moved: None, tried: None, board: BoardWay::default(), ways: Vec::new() };
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
    /// A Disk System game's gap from the disk file a neighbouring run lies in, at the gap's
    /// place in that file's load range.
    DiskFiles,
}

/// The file's data as a map rebuilds it from the ROM: its runs, and the gaps as `fill` says.
fn rebuilt(m: &Map, prg: &[u8], fill: Fill) -> Vec<u8> {
    let mut data = match fill {
        Fill::File(d) => d.to_vec(),
        _ => vec![0u8; m.data_len],
    };
    data.resize(m.data_len, 0);
    for r in &m.runs {
        if let (Some(src), Some(dst)) = (prg.get(r.rom..r.rom.saturating_add(r.len)), data.get_mut(r.data..r.data.saturating_add(r.len))) {
            dst.copy_from_slice(src);
        }
    }
    // A Disk System game's files each load at their own address: a gap is filled from the
    // file a neighbouring run lies in, at the gap's place in that file's load range.
    if fill == Fill::DiskFiles {
        if let Ok(disk) = super::fds::parse(prg) {
            let files: Vec<(u16, usize, usize)> =
                disk.sides.iter().flat_map(|s| &s.files).filter(|f| f.kind == 0).map(|f| (f.addr, f.at, f.data.len())).collect();
            let file_of = |rom: usize| files.iter().find(|f| (f.1..f.1 + f.2).contains(&rom)).copied();
            let mut placed = vec![false; m.data_len];
            m.runs.iter().for_each(|r| placed.get_mut(r.data..r.data.saturating_add(r.len)).into_iter().for_each(|p| p.fill(true)));
            for d in 0..m.data_len {
                if placed[d] {
                    continue;
                }
                let Some(cpu) = cpu_of(&m.header, d) else { continue };
                // The runs just before and after the gap, nearest first.
                let before = m.runs.iter().filter(|r| r.data + r.len <= d).max_by_key(|r| r.data + r.len);
                let after = m.runs.iter().filter(|r| r.data > d).min_by_key(|r| r.data);
                let byte = [before.map(|r| r.rom + r.len - 1), after.map(|r| r.rom)].into_iter().flatten().find_map(|rom| {
                    let (addr, at, len) = file_of(rom)?;
                    let i = (cpu as usize).checked_sub(addr as usize).filter(|&i| i < len)?;
                    prg.get(at + i).copied()
                });
                if let Some(b) = byte {
                    data[d] = b;
                }
            }
            return data;
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

/// A track playing from the ROM by its map, through the file's layout rebuilt from the ROM.
pub struct LayoutSong {
    p: Player,
    cpu: Cpu,
    shift: i32,
    play: Vec<Op>,
    first: Vec<Write>,
}

impl LayoutSong {
    /// Track `song`, started. None when its script doesn't run.
    pub fn open(m: &Map, prg: &[u8], song: u8, fill: Fill) -> Option<Self> {
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
        let first = std::mem::take(&mut p.writes);
        Some(Self { p, cpu, shift, play: m.play_of(song).to_vec(), first })
    }

    /// The next frame's sound writes; None once the play routine stops returning.
    pub fn frame(&mut self) -> Option<Vec<Write>> {
        if !replay(&mut self.p, &mut self.cpu, &self.play, self.shift) {
            return None;
        }
        let mut f = std::mem::take(&mut self.first);
        f.append(&mut self.p.writes);
        Some(f)
    }

    /// The byte the CPU sees at `addr` ($6000-$FFFF), for the DMC channel's samples.
    fn peek(&self, addr: u16) -> u8 {
        addr.checked_sub(0x6000).and_then(|i| self.p.mem.get(i as usize)).copied().unwrap_or(0)
    }
}

/// Track `song` played from the ROM by its map: each frame's sound writes.
pub fn map_track_writes(m: &Map, prg: &[u8], song: u8, frames: usize, fill: Fill) -> Option<Vec<Vec<Write>>> {
    let mut s = LayoutSong::open(m, prg, song, fill)?;
    Some((0..frames).map_while(|_| s.frame()).collect())
}

/// How a map plays the NSF's tracks: per track, how alike its melody is to the NSF's own
/// (None for a track whose script didn't record, or that has no melody in the NSF either).
pub fn map_check(nsf: &[u8], m: &Map, prg: &[u8], frames: usize, fill: Fill) -> Vec<Option<f64>> {
    (0..m.header.songs)
        .map(|t| {
            let chips = nsf.get(0x7B).copied().unwrap_or(0);
            let real = melody_chips(&track_writes(nsf, t, frames)?, chips);
            if real.len() < music_search::MIN_MELODY {
                return None;
            }
            let mine = map_track_writes(m, prg, t, frames, fill).map(|w| melody_chips(&w, chips)).unwrap_or_default();
            Some(alike(&real, &mine))
        })
        .collect()
}

/// Choices for playing a map on the game's own board, made per game with the file's help
/// ([`choose_board`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BoardWay {
    /// A bank the board maps from power on stays in that window, wherever the file has it.
    pub fixed: bool,
    /// The windows the map sets keep their banks while the game's code runs (a routine that
    /// switches banks from a register the file doesn't set).
    pub pin: Pin,
}

/// Which windows [`BoardWay::pin`] holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Pin {
    /// None: the game's code switches banks as it likes.
    #[default]
    Off,
    /// Every window the map sets.
    All,
    /// Only the window the code being run sits in, so the code switches its data banks itself.
    Code,
}

/// Frames each track plays when [`choose_board`] tries the ways.
const BOARD_CHOICE_FRAMES: usize = 120;

/// Picks the [`BoardWay`] that plays the most of the file's tracks on the game's board.
pub fn choose_board(nsf: &[u8], m: &mut Map, rom: &[u8]) {
    let mut best = (-1.0, BoardWay::default());
    for fixed in [false, true] {
        for pin in [Pin::Off, Pin::All, Pin::Code] {
            m.board = BoardWay { fixed, pin };
            let score: f64 = cart_check(nsf, m, rom, BOARD_CHOICE_FRAMES).iter().flatten().sum();
            if score > best.0 {
                best = (score, m.board);
            }
        }
    }
    m.board = best.1;
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
    let mut s = BoardSong::open(m, rom, song)?;
    Some((0..frames).map_while(|_| s.frame()).collect())
}

/// A track playing on the game's own board ([`cart_track_writes`]).
pub struct BoardSong {
    nes: super::nes_music::Nes,
    m: Map,
    play: Vec<Op>,
    prg_len: usize,
    boot: [usize; 5],
    pinned: [Option<usize>; 5],
    first: Vec<Write>,
}

impl BoardSong {
    /// Track `song` of the whole iNES file `rom`, started. None when the board isn't
    /// supported or the script fails.
    pub fn open(m: &Map, rom: &[u8], song: u8) -> Option<Self> {
        use super::music_search::Machine;
        let script = m.tracks.get(song as usize)?.as_ref()?.clone();
        let nes = super::nes_music::Nes::boot(rom).ok()?;
        let prg_len = super::nes_cart::parse(rom).ok()?.prg.len();
        let boot = nes.windows();
        let mut s = Self { nes, m: m.clone(), play: m.play_of(song).to_vec(), prg_len, boot, pinned: [None; 5], first: Vec::new() };
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
                if let Some((_, w, base)) = s.place(d, a) {
                    if w > 0 && !set[w] {
                        windows[w] = base;
                        set[w] = true;
                    }
                }
                d += 0x2000 - (a as usize & 0x1FFF);
            }
        }
        s.nes.set_windows(windows);
        if m.board.pin != Pin::Off {
            (1..5).filter(|&k| set[k]).for_each(|k| s.pinned[k] = Some(windows[k]));
        }
        s.nes.write_bus(0x4015, 0x0F);
        if !s.run(&script) {
            return None;
        }
        s.first = s.nes.take_sound();
        Some(s)
    }

    /// Where the game has the file's byte at data offset `d` (CPU address `a` in the file), as
    /// [`game_place`] says; a bank the board maps from power on (a fixed bank) stays in its
    /// window when the way says so.
    fn place(&self, d: usize, a: u16) -> Option<(u16, usize, usize)> {
        let (g, w, base) = game_place(&self.m, self.prg_len, d, a)?;
        match (1..5).find(|&k| self.m.board.fixed && self.boot[k] == base) {
            Some(k) if k != w => Some(((0x6000 + k * 0x2000) as u16 | (g & 0x1FFF), k, base)),
            _ => Some((g, w, base)),
        }
    }

    fn run(&mut self, ops: &[Op]) -> bool {
        let budget = 400_000;
        let banked = self.m.header.banks.iter().any(|&b| b != 0);
        let pin = self.m.board.pin;
        for op in ops {
            match *op {
                // The file's own bank registers: the board's window where the game has
                // that bank (through the run holding most of it).
                Op::Poke(r @ 0x5FF8..=0x5FFF, b) if banked => {
                    let slot = r as usize - 0x5FF8;
                    let d0 = (b as usize * 0x1000).checked_sub((self.m.header.load & 0xFFF) as usize);
                    let Some(d0) = d0 else { continue };
                    let best = self
                        .m
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
                        if let Some((_, w, base)) = self.place(d, a) {
                            let mut win = self.nes.windows();
                            win[w] = base;
                            self.nes.set_windows(win);
                            if pin != Pin::Off && w > 0 {
                                self.pinned[w] = Some(base);
                            }
                        }
                    }
                }
                Op::Poke(0x5FF6..=0x5FFF, _) => {}
                Op::Poke(a @ (0x0000..=0x1FFF | 0x4000..=0x4017), v) => self.nes.write_bus(a, v),
                Op::Poke(a @ 0x6000..=0x7FFF, v) => {
                    use super::music_search::Machine;
                    self.nes.poke(a as u32, v)
                }
                Op::Poke(..) => {}
                // Bytes the file's own code pushed: the game's code didn't (its routine
                // is entered where the game enters it).
                Op::Push(_) => {}
                Op::Call { addr, a, x, y, src } => {
                    let at = match src.checked_sub(1).and_then(|d| self.place(d as usize, addr)) {
                        Some((g, w, base)) => {
                            let mut win = self.nes.windows();
                            if w > 0 {
                                win[w] = base;
                                if pin != Pin::Off {
                                    self.pinned[w] = Some(base);
                                }
                            }
                            self.nes.set_windows(win);
                            g
                        }
                        None => addr,
                    };
                    if !self.nes.run_routine(at, [a, x, y], &[], budget, &self.pinned, pin == Pin::Code) {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// The next frame's sound writes; None once the play routine stops returning.
    pub fn frame(&mut self) -> Option<Vec<Write>> {
        use super::music_search::Machine;
        let play = std::mem::take(&mut self.play);
        let ran = self.run(&play);
        self.play = play;
        if !ran {
            return None;
        }
        let mut f = std::mem::take(&mut self.first);
        f.append(&mut self.nes.take_sound());
        Some(f)
    }
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
    played_check(nsf, m.header.songs, frames, |t| cart_track_writes(m, rom, t, frames))
}

/// How a saved map plays the NSF's tracks from the ROM, each by its chosen way ([`play_map`]),
/// compared as [`cart_check`] does.
pub fn saved_check(nsf: &[u8], m: &Map, rom: &[u8], frames: usize) -> Vec<Option<f64>> {
    played_check(nsf, m.header.songs, frames, |t| play_map(m, rom, t, frames))
}

fn played_check(nsf: &[u8], songs: u8, frames: usize, play: impl Fn(u8) -> Option<Vec<Vec<Write>>>) -> Vec<Option<f64>> {
    (0..songs)
        .map(|t| {
            let chips = nsf.get(0x7B).copied().unwrap_or(0);
            let real = melody_chips(&track_writes(nsf, t, frames)?, chips);
            if real.len() < music_search::MIN_MELODY {
                return None;
            }
            let Some(mine) = play(t) else { return Some(0.0) };
            // The periods as written, and by notes at either clock: a PAL release may keep
            // the NTSC period table or have its own (the header doesn't always say PAL).
            let raw = alike(&real, &melody_chips(&mine, chips));
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

/// Which player plays a track from the ROM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Way {
    /// The file's layout rebuilt from the ROM (gaps between runs from the ROM too).
    Layout,
    /// The same with the gaps left empty (a gap the ROM fills with other bytes).
    Zeros,
    /// A Disk System game's layout, gaps filled per disk file ([`Fill::DiskFiles`]).
    DiskFiles,
    /// The game's own board ([`cart_track_writes`]).
    Board,
    /// Neither plays it right.
    None,
}

/// What a map's runs point into: an iNES cartridge's program data, or a Disk System image
/// (its files, as the disk holds them). None for anything else.
pub fn program(rom: &[u8]) -> Option<std::sync::Arc<Vec<u8>>> {
    if super::fds::is_disk(rom) {
        return Some(std::sync::Arc::new(super::fds::strip_header(rom).to_vec()));
    }
    if let Ok(h) = super::nes_cart::parse(rom) {
        return Some(h.prg);
    }
    // A headerless dump starts with its program data too; the character data after it is
    // never in a run.
    is_headerless(rom).then(|| std::sync::Arc::new(rom.to_vec()))
}

/// Whether `rom` looks like a cartridge dumped without its iNES header (program then
/// character data, whole 8 KB banks). Such a dump has no board way: its mapper is unknown.
pub fn is_headerless(rom: &[u8]) -> bool {
    rom.len() >= 0x4000 && rom.len() % 0x2000 == 0 && super::nes_cart::parse(rom).is_err() && !super::fds::is_disk(rom)
}

/// Picks, per track, the player that plays it most like the file (at least `right`). A Disk
/// System game has no board to play on: only the layouts.
pub fn choose_ways(nsf: &[u8], m: &mut Map, rom: &[u8], frames: usize, right: f64) {
    let Some(prg) = program(rom) else { return };
    let layout = map_check(nsf, m, &prg, frames, Fill::Bridged);
    let board = if super::nes_cart::parse(rom).is_err() { vec![None; layout.len()] } else { cart_check(nsf, m, rom, frames) };
    // The empty-gap layout only where neither of the others plays a track right.
    let missed = layout.iter().zip(&board).any(|(l, b)| l.unwrap_or(0.0) < right && b.unwrap_or(0.0) < right);
    let zeros = if missed { map_check(nsf, m, &prg, frames, Fill::Zeros) } else { Vec::new() };
    let files = if missed && m.header.fds { map_check(nsf, m, &prg, frames, Fill::DiskFiles) } else { Vec::new() };
    m.ways = (0..layout.len())
        .map(|i| {
            let score = |v: &Vec<Option<f64>>| v.get(i).copied().flatten().unwrap_or(0.0);
            let (l, b, z, f) = (score(&layout), score(&board), score(&zeros), score(&files));
            if l >= right && l >= b {
                Way::Layout
            } else if b >= right {
                Way::Board
            } else if z >= right {
                Way::Zeros
            } else if f >= right {
                Way::DiskFiles
            } else {
                Way::None
            }
        })
        .collect();
}

/// How alike to the file a track must play to count as right.
pub const MAP_RIGHT: f64 = 0.9;
/// Frames each track plays when [`build_map`] chooses its way.
const WAY_FRAMES: usize = 600;

/// The map of `rom` (a whole iNES file, or a Disk System image) for the NSF it matches, its
/// board way and each track's player chosen. None when the NSF doesn't run.
pub fn build_map(nsf: &[u8], rom: &[u8]) -> Option<Map> {
    let prg = program(rom)?;
    let mut m = map(nsf, &prg)?;
    if super::nes_cart::parse(rom).is_ok() {
        choose_board(nsf, &mut m, rom);
    }
    choose_ways(nsf, &mut m, rom, WAY_FRAMES, MAP_RIGHT);
    Some(m)
}

/// Track `song` from the user's ROM (a whole iNES file) by its map and chosen way: each
/// frame's sound writes. None for a track no way plays right.
pub fn play_map(m: &Map, rom: &[u8], song: u8, frames: usize) -> Option<Vec<Vec<Write>>> {
    match m.ways.get(song as usize)? {
        Way::Board => cart_track_writes(m, rom, song, frames),
        Way::Layout => map_track_writes(m, &program(rom)?, song, frames, Fill::Bridged),
        Way::Zeros => map_track_writes(m, &program(rom)?, song, frames, Fill::Zeros),
        Way::DiskFiles => map_track_writes(m, &program(rom)?, song, frames, Fill::DiskFiles),
        Way::None => None,
    }
}

/// A track from the user's ROM by its map, as sound: the player its way names, and the
/// console's sound chip.
pub struct MapSong {
    player: MapPlayer,
    rate: f64,
}

enum MapPlayer {
    Board(BoardSong, super::nes_music::Chip),
    Layout(LayoutSong, super::nes_apu::Apu, u32),
}

impl MapSong {
    /// Track `song` of `rom` (a whole iNES file, or a Disk System image), rendered at `rate`.
    pub fn open(m: &Map, rom: &[u8], song: u8, rate: u32) -> Result<Self, String> {
        use super::music_search::Machine;
        // The console's clock: a PAL cartridge's own, else NTSC (every Disk System).
        let pal = super::nes_cart::parse(rom).map(|h| h.pal).unwrap_or(false);
        let (frame_rate, cycles) = if pal {
            (super::nes_music::PAL_RATE, super::nes_music::PAL_FRAME)
        } else {
            (super::nes_music::NTSC_RATE, super::nes_music::NTSC_FRAME)
        };
        let player = match m.ways.get(song as usize) {
            Some(Way::Board) => {
                let nes = super::nes_music::Nes::boot(rom)?;
                let s = BoardSong::open(m, rom, song).ok_or("the song didn't start")?;
                MapPlayer::Board(s, nes.new_chip(rate)?)
            }
            Some(way @ (Way::Layout | Way::Zeros | Way::DiskFiles)) => {
                let prg = program(rom).ok_or("not a NES cartridge or Disk System image")?;
                let fill = match way {
                    Way::Zeros => Fill::Zeros,
                    Way::DiskFiles => Fill::DiskFiles,
                    _ => Fill::Bridged,
                };
                let s = LayoutSong::open(m, &prg, song, fill).ok_or("the song didn't start")?;
                let c = m.header.chips | if m.header.fds { 4 } else { 0 };
                use super::nes_apu::{CHIP_FDS, CHIP_FME7, CHIP_MMC5, CHIP_N163, CHIP_VRC6};
                let chips = [(1, CHIP_VRC6), (4, CHIP_FDS), (8, CHIP_MMC5), (16, CHIP_N163), (32, CHIP_FME7)]
                    .iter()
                    .filter(|(bit, _)| c & bit != 0)
                    .fold(0, |a, (_, chip)| a | chip);
                MapPlayer::Layout(s, super::nes_apu::Apu::new(rate, pal, chips)?, cycles)
            }
            _ => return Err("the map has no way to play this song".into()),
        };
        Ok(Self { player, rate: frame_rate })
    }

    pub fn frame_rate(&self) -> f64 {
        self.rate
    }

    /// One frame: its samples appended to `out`, and its sound writes. None once the song's
    /// code stops returning.
    pub fn frame(&mut self, out: &mut Vec<i16>) -> Option<Vec<Write>> {
        use super::music_search::Machine;
        match &mut self.player {
            MapPlayer::Board(s, chip) => {
                let w = s.frame()?;
                s.nes.feed(chip, &w, out);
                Some(w)
            }
            MapPlayer::Layout(s, apu, cycles) => {
                let w = s.frame()?;
                extern "C" fn read(data: *mut std::ffi::c_void, addr: std::ffi::c_uint) -> std::ffi::c_int {
                    // SAFETY: set to a live LayoutSong for the writes and end of one frame.
                    let s = unsafe { &*(data as *const LayoutSong) };
                    s.peek(addr as u16) as std::ffi::c_int
                }
                apu.set_reader(Some(read), &*s as *const LayoutSong as *mut std::ffi::c_void);
                for x in &w {
                    let (t, addr) = match x.port {
                        0x4040..=0x40FF => (super::nes_apu::Target::Fds, x.port as u16),
                        0..=0xFFFF => (super::nes_apu::Target::Apu, x.port as u16),
                        // A cartridge chip's, tagged by the file's player.
                        _ => super::nes_music::target(x.port),
                    };
                    apu.write(x.at.min(*cycles), t, addr, x.value as u8);
                }
                apu.end_frame(*cycles, out);
                apu.set_reader(None, std::ptr::null_mut());
                Some(w)
            }
        }
    }
}

/// A map as text: numbers only (addresses, lengths, offsets, register values), nothing of
/// the game or of the file. One line per part; see [`Map::decode`].
impl Map {
    pub fn encode(&self) -> String {
        let h = &self.header;
        let hex = |v: &[u8]| v.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let ops = |ops: &[Op]| {
            ops.iter()
                .map(|o| match *o {
                    Op::Poke(a, v) => format!("P{a:x}:{v:x}"),
                    Op::Push(v) => format!("U{v:x}"),
                    Op::Call { addr, a, x, y, src } => format!("C{addr:x}:{a:x}:{x:x}:{y:x}:{src:x}"),
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        let mut out = vec![
            "nesmap 1".to_string(),
            format!("h {:x} {:x} {:x} {} {} {} {:x}", h.load, h.init, h.play, hex(&h.banks), h.fds as u8, h.songs, h.chips),
            format!("d {:x}", self.data_len),
            format!("r {}", self.runs.iter().map(|r| format!("{:x}:{:x}:{:x}", r.data, r.len, r.rom)).collect::<Vec<_>>().join(" ")),
            format!("w {} {}", self.board.fixed as u8, self.board.pin as u8),
            format!("p {}", ops(&self.play)),
        ];
        if let Some((d, windows)) = &self.moved {
            out.push(format!("m {d} {}", windows.iter().map(|(b, r)| format!("{b:x}:{r:x}")).collect::<Vec<_>>().join(" ")));
        }
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
        out.push(format!(
            "s {}",
            self.ways.iter().map(|w| match w { Way::Layout => 'l', Way::Zeros => 'z', Way::DiskFiles => 'f', Way::Board => 'b', Way::None => '-' }).collect::<String>()
        ));
        out.join("\n")
    }

    /// The map [`Self::encode`] wrote. None when the text isn't one.
    pub fn decode(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != "nesmap 1" {
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
                        ("C", [addr, a, x, y, src]) => Op::Call { addr: *addr as u16, a: *a as u8, x: *x as u8, y: *y as u8, src: *src },
                        _ => return None,
                    })
                })
                .collect()
        };
        let mut m = Map {
            header: Header { load: 0, init: 0, play: 0, banks: [0; 8], fds: false, songs: 0, chips: 0 },
            data_len: 0,
            runs: Vec::new(),
            play: Vec::new(),
            tracks: Vec::new(),
            plays: Vec::new(),
            moved: None,
            tried: None,
            board: BoardWay::default(),
            ways: Vec::new(),
        };
        for line in lines {
            let f: Vec<&str> = line.split(' ').filter(|t| !t.is_empty()).collect();
            match f.first().copied() {
                Some("h") if f.len() == 7 || f.len() == 8 => {
                    let b = f[4].as_bytes();
                    let mut banks = [0u8; 8];
                    for (i, c) in b.chunks(2).enumerate().take(8) {
                        banks[i] = u8::from_str_radix(std::str::from_utf8(c).ok()?, 16).ok()?;
                    }
                    m.header = Header {
                        load: num(f[1])? as u16,
                        init: num(f[2])? as u16,
                        play: num(f[3])? as u16,
                        banks,
                        fds: f[5] == "1",
                        songs: f[6].parse().ok()?,
                        chips: match f.get(7) {
                            Some(c) => num(c)? as u8,
                            None => if f[5] == "1" { 4 } else { 0 },
                        },
                    };
                    m.tracks = vec![None; m.header.songs as usize];
                    m.plays = vec![None; m.header.songs as usize];
                }
                Some("d") => m.data_len = Some(num(f.get(1)?)? as usize).filter(|&n| n as u64 <= super::rom_songs::MAX_ROM_BYTES)?,
                Some("r") => {
                    for t in &f[1..] {
                        let v: Vec<u32> = t.split(':').map(num).collect::<Option<_>>()?;
                        let [data, len, rom] = v[..] else { return None };
                        m.runs.push(super::music_map::Run { data: data as usize, len: len as usize, rom: rom as usize });
                    }
                }
                Some("w") => m.board = BoardWay { fixed: f.get(1) == Some(&"1"), pin: match f.get(2) {
                    Some(&"1") => Pin::All,
                    Some(&"2") => Pin::Code,
                    _ => Pin::Off,
                } },
                Some("p") => m.play = ops(&f[1..])?,
                Some("m") => {
                    let d: i32 = f.get(1)?.parse().ok()?;
                    let w = f[2..]
                        .iter()
                        .map(|t| {
                            let (b, r) = t.split_once(':')?;
                            Some((num(b)? as u16, num(r)? as usize))
                        })
                        .collect::<Option<_>>()?;
                    m.moved = Some((d, w));
                }
                Some(k @ ("t" | "q")) => {
                    let i: usize = f.get(1)?.parse().ok()?;
                    let list = if k == "t" { &mut m.tracks } else { &mut m.plays };
                    *list.get_mut(i)? = Some(ops(&f[2..])?);
                }
                Some("s") => {
                    m.ways = f.get(1).unwrap_or(&"").chars().map(|c| match c { 'l' => Way::Layout, 'z' => Way::Zeros, 'f' => Way::DiskFiles, 'b' => Way::Board, _ => Way::None }).collect();
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
        // Written down as numbers and read back, the map plays the same.
        let mut m = m;
        choose_ways(&f, &mut m, &ines, 60, 0.9);
        assert!(m.ways.iter().all(|w| *w != Way::None), "{:?}", m.ways);
        let text = m.encode();
        let back = Map::decode(&text).unwrap();
        assert_eq!(back.encode(), text);
        for t in 0..3 {
            assert_eq!(play_map(&back, &ines, t, 60), play_map(&m, &ines, t, 60));
        }
        // As sound: the same writes, frame by frame.
        if super::super::nes_apu::available() {
            for t in 0..3 {
                let mut song = MapSong::open(&back, &ines, t, 8000).unwrap();
                let mut pcm = Vec::new();
                let frames: Vec<_> = (0..60).map_while(|_| song.frame(&mut pcm)).collect();
                assert_eq!(Some(frames), play_map(&m, &ines, t, 60));
                assert!(!pcm.is_empty());
            }
        }
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
