//! What an NSF file says about its game's sound code, as a [`Hint`] for the music search.
//!
//! An NSF carries the game's own sound driver at the addresses the game runs it from (6502
//! code isn't relocatable), with a small init and play routine written for the file or taken from
//! the game. Their addresses, and the routines they call, point the search straight at the
//! game's sound routine and the routine that starts a song, so it no longer has to find them
//! in the boot alone. The music still plays from the ROM: the NSF is only read for addresses.

use super::m6502::{Bus, Cpu};
use super::music_search::{hint_routines, Hint, Write};

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
}

impl Bus for Player {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x1FFF => self.ram[addr as usize & 0x7FF],
            0x6000..=0xFFFF => self.mem[addr as usize - 0x6000],
            _ => 0,
        }
    }

    fn write(&mut self, addr: u16, v: u8) {
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
            0x6000..=0x7FFF => self.mem[addr as usize - 0x6000] = v,
            0x8000..=0xFFFF if self.fds => self.mem[addr as usize - 0x6000] = v,
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
        }
    }

    /// Runs the routine at `pc` with A and X until it returns; false when it doesn't.
    fn call(&mut self, cpu: &mut Cpu, pc: u16, a: u8, x: u8) -> bool {
        self.call_watched(cpu, pc, a, x, &mut |_| {})
    }

    /// [`Self::call`], showing `watch` the CPU before each instruction.
    fn call_watched(&mut self, cpu: &mut Cpu, pc: u16, a: u8, x: u8, watch: &mut dyn FnMut(&Cpu)) -> bool {
        // A return address on the stack the routine's RTS takes (never run).
        self.ram[0x1FF] = 0x5F;
        self.ram[0x1FE] = 0xF5;
        cpu.s = 0xFD;
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
    if nsf.len() < 0x80 || &nsf[..5] != b"NESM\x1a" {
        return None;
    }
    let word = |at: usize| u16::from_le_bytes([nsf[at], nsf[at + 1]]);
    let (load, init, play) = (word(0x08), word(0x0A), word(0x0C));
    let banks: [u8; 8] = nsf[0x70..0x78].try_into().ok()?;
    let banked = banks.iter().any(|&b| b != 0);
    let fds = nsf[0x7B] & 4 != 0;
    let mut p = Player {
        ram: [0; 0x800],
        mem: vec![0; 0xA000],
        data: Vec::new(),
        banked,
        fds,
        writes: Vec::new(),
    };
    if banked {
        p.data = vec![0; (load & 0xFFF) as usize];
        p.data.extend_from_slice(&nsf[0x80..]);
        for (i, &b) in banks.iter().enumerate() {
            p.bank(i + 2, b);
        }
        if fds {
            p.bank(0, banks[6]);
            p.bank(1, banks[7]);
        }
    } else {
        let at = (load as usize).checked_sub(0x6000)?;
        for (i, &b) in nsf[0x80..].iter().enumerate() {
            if let Some(m) = p.mem.get_mut(at + i) {
                *m = b;
            }
        }
    }
    Some((p, init, play))
}

/// A melody's fingerprint: the runs of four pitches in a row on each of the NES's tone
/// channels (pulse 1 and 2, triangle), as the writes set them. Two plays of one song share
/// most of theirs whatever the timing or volume, two songs few.
pub fn melody(frames: &[Vec<Write>]) -> std::collections::HashSet<u64> {
    use std::hash::{Hash, Hasher};
    let mut regs = [0u8; 12];
    let mut last = [u16::MAX; 3];
    let mut runs: [Vec<u16>; 3] = Default::default();
    let mut out = std::collections::HashSet::new();
    for f in frames {
        for w in f {
            let a = w.port;
            if !(0x4000..0x400C).contains(&a) {
                continue;
            }
            regs[(a - 0x4000) as usize] = w.value as u8;
            let ch = ((a - 0x4000) / 4) as usize;
            if a & 3 < 2 {
                continue;
            }
            let period = (regs[ch * 4 + 3] as u16 & 7) << 8 | regs[ch * 4 + 2] as u16;
            if period == last[ch] || period < 8 {
                continue;
            }
            last[ch] = period;
            let run = &mut runs[ch];
            run.push(period);
            if run.len() >= 4 {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                (ch, &run[run.len() - 4..]).hash(&mut h);
                out.insert(h.finish());
            }
        }
    }
    out
}

/// How alike two [`melody`] fingerprints are: the share of the smaller one found in the
/// other (0 when either is empty).
pub fn alike(a: &std::collections::HashSet<u64>, b: &std::collections::HashSet<u64>) -> f64 {
    let small = a.len().min(b.len());
    if small == 0 {
        return 0.0;
    }
    a.intersection(b).count() as f64 / small as f64
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
        let mut data = vec![0xEAu8; 0x100];
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
