//! What a GBS file says about its game's sound code, as a [`Hint`] for the music search.
//!
//! A GBS holds the game's sound driver at the addresses the game runs it from, with an init
//! routine (song number in A) and a play routine (called each frame or timer tick), like an
//! NSF does for the NES (see `nes_nsf`). Only the addresses are taken; the music plays from
//! the ROM.

use std::collections::HashSet;

use super::music_search::{self, hint_routines, Hint, Write};
use super::sm83::{Bus, Cpu};

const CODE_BYTES: usize = 12;
const SCAN_BYTES: usize = 96;

/// A GBS's program as the CPU sees it at start: bank 0 at $0000, bank 1 at $4000.
struct Gbs {
    songs: u8,
    init: u16,
    play: u16,
    image: Vec<Option<u8>>,
}

fn parse(gbs: &[u8]) -> Option<Gbs> {
    if gbs.len() < 0x70 || &gbs[..3] != b"GBS" || gbs[3] != 1 {
        return None;
    }
    let word = |at: usize| u16::from_le_bytes([gbs[at], gbs[at + 1]]);
    let (load, init, play) = (word(0x06), word(0x08), word(0x0A));
    if load >= 0x8000 {
        return None;
    }
    let data = &gbs[0x70..];
    let mut image = vec![None; 0x8000];
    for (i, slot) in image.iter_mut().enumerate().skip(load as usize) {
        *slot = data.get(i - load as usize).copied();
    }
    Some(Gbs { songs: gbs[0x04], init, play, image })
}

impl Gbs {
    fn byte(&self, addr: u16) -> Option<u8> {
        self.image.get(addr as usize).copied().flatten()
    }

    fn code(&self, addr: u16) -> Option<Vec<u8>> {
        (0..CODE_BYTES as u16).map(|i| self.byte(addr.wrapping_add(i))).collect()
    }

    /// CALL and JP targets in the straight-line code from `addr`.
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
                0xCD | 0xC4 | 0xCC | 0xD4 | 0xDC | 0xC3 | 0xC2 | 0xCA | 0xD2 | 0xDA => {
                    // ROM only: a jump into RAM is code the game copied there.
                    if let Some(t) = target().filter(|&t| t < 0x8000 && t >= 0x100) {
                        out.push(t);
                    }
                    if op == 0xC3 {
                        break;
                    }
                }
                0xC9 | 0xD9 | 0xE9 => break,
                _ => {}
            }
            pc = pc.wrapping_add(length(op));
        }
        out
    }
}

/// An SM83 instruction's length in bytes.
fn length(op: u8) -> u16 {
    match op {
        0x01 | 0x11 | 0x21 | 0x31 | 0x08 | 0xC2 | 0xC3 | 0xC4 | 0xCA | 0xCC | 0xCD | 0xD2
        | 0xD4 | 0xDA | 0xDC | 0xEA | 0xFA => 3,
        0x06 | 0x0E | 0x16 | 0x1E | 0x26 | 0x2E | 0x36 | 0x3E | 0x18 | 0x20 | 0x28 | 0x30
        | 0x38 | 0xC6 | 0xCE | 0xD6 | 0xDE | 0xE6 | 0xEE | 0xF6 | 0xFE | 0xE0 | 0xF0 | 0xE8
        | 0xF8 | 0xCB | 0x10 => 2,
        _ => 1,
    }
}

/// The search hint a GBS gives. `None` when it isn't a GBS.
pub fn hint(gbs: &[u8]) -> Option<Hint> {
    let g = parse(gbs)?;
    let start = hint_routines(g.init, |a| g.calls_in(a), |a| g.code(a));
    // The number each track hands the routines init calls (see `nes_nsf::hint`).
    let mut ids = vec![(g.init as u32, 0, (0..g.songs).collect())];
    let inner: Vec<u16> = start.iter().skip(1).map(|r| r.0 as u16).collect();
    for (addr, seen) in inner.iter().zip(entries(gbs, g.songs, &inner)) {
        let got: Vec<[u8; 7]> = seen.into_iter().flatten().collect();
        if got.len() * 2 < g.songs as usize {
            continue;
        }
        if let Some(reg) = (0..7).find(|&r| got.iter().any(|x| x[r] != got[0][r])) {
            ids.push((*addr as u32, reg as u8, got.iter().map(|x| x[reg]).collect()));
        }
    }
    Some(Hint {
        play: hint_routines(g.play, |a| g.calls_in(a), |a| g.code(a)),
        start,
        songs: g.songs,
        ids,
    })
}

/// A GBS, run on its own (a bare SM83 with the file's banks, RAM and a sound register file),
/// for the sound writes of its tracks: the known answer found songs are checked against.
struct Player {
    rom: Vec<u8>,
    bank: usize,
    ram: Vec<u8>,
    io: [u8; 0x80],
    hram: [u8; 0x80],
    writes: Vec<Write>,
}

impl Bus for Player {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x3FFF => self.rom.get(addr as usize).copied().unwrap_or(0xFF),
            0x4000..=0x7FFF => self
                .rom
                .get(self.bank * 0x4000 + addr as usize - 0x4000)
                .copied()
                .unwrap_or(0xFF),
            0xA000..=0xDFFF => self.ram[addr as usize - 0xA000],
            0xE000..=0xFDFF => self.ram[addr as usize - 0xC000],
            0xFF00..=0xFF7F => self.io[addr as usize - 0xFF00],
            0xFF80..=0xFFFF => self.hram[addr as usize - 0xFF80],
            _ => 0xFF,
        }
    }

    fn write(&mut self, addr: u16, v: u8) {
        match addr {
            0x2000..=0x3FFF => self.bank = (v as usize).max(1),
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

impl Player {
    /// Runs the routine at `pc` with A = `a` until it returns, showing `watch` the CPU before
    /// each instruction; false when it doesn't return.
    fn call(&mut self, cpu: &mut Cpu, sp: u16, pc: u16, a: u8, watch: &mut dyn FnMut(&Cpu)) -> bool {
        // A return address the routine's RET takes (never run).
        cpu.sp = sp.wrapping_sub(2);
        self.write(cpu.sp, 0x00);
        self.write(cpu.sp.wrapping_add(1), 0x01);
        cpu.pc = pc;
        cpu.a = a;
        cpu.halted = false;
        cpu.calls = vec![(pc, sp)];
        let mut used = 0u32;
        while !cpu.calls.is_empty() {
            if used > 1_000_000 || cpu.halted || cpu.stopped {
                return false;
            }
            watch(cpu);
            used += cpu.step(self);
        }
        true
    }
}

/// A GBS's player, loaded as at start, with its stack, init and play addresses, and how many
/// times play runs per 1/60 s.
fn load(gbs: &[u8]) -> Option<(Player, u16, u16, u16, f64)> {
    if gbs.len() < 0x70 || &gbs[..3] != b"GBS" {
        return None;
    }
    let word = |at: usize| u16::from_le_bytes([gbs[at], gbs[at + 1]]);
    let (load, init, play, sp) = (word(0x06), word(0x08), word(0x0A), word(0x0C));
    let (tma, tac) = (gbs[0x0E], gbs[0x0F]);
    let mut rom = vec![0u8; load as usize];
    rom.extend_from_slice(&gbs[0x70..]);
    // Play's rate: the timer's when the file sets one, else the screen's.
    let rate = if tac & 4 != 0 {
        let clock = [4096.0, 262144.0, 65536.0, 16384.0][(tac & 3) as usize]
            * if tac & 0x80 != 0 { 2.0 } else { 1.0 };
        clock / (256.0 - tma as f64) / 60.0
    } else {
        59.73 / 60.0
    };
    let mut p = Player { rom, bank: 1, ram: vec![0; 0x4000], io: [0; 0x80], hram: [0; 0x80], writes: Vec::new() };
    p.io[0x26] = 0x80;
    Some((p, sp, init, play, rate))
}

/// Track `song` (from 0) of a GBS, `frames` 1/60 s long: each frame's sound writes.
pub fn track_writes(gbs: &[u8], song: u8, frames: usize) -> Option<Vec<Vec<Write>>> {
    let (mut p, sp, init, play, rate) = load(gbs)?;
    let mut cpu = Cpu::default();
    if !p.call(&mut cpu, sp, init, song, &mut |_| {}) {
        return None;
    }
    let mut out = Vec::with_capacity(frames);
    let mut due = 0.0;
    let mut f = std::mem::take(&mut p.writes);
    for _ in 0..frames {
        due += rate;
        while due >= 1.0 {
            due -= 1.0;
            if !p.call(&mut cpu, sp, play, 0, &mut |_| {}) {
                out.push(f);
                return Some(out);
            }
        }
        f.append(&mut p.writes);
        out.push(std::mem::take(&mut f));
    }
    Some(out)
}

/// A melody's fingerprint (see [`music_search::melody`]) on the Game Boy's tone channels:
/// the two square waves and the wave channel.
pub fn melody(frames: &[Vec<Write>]) -> HashSet<u64> {
    music_search::melody(frames, &[(0xFF13, 0xFF14), (0xFF18, 0xFF19), (0xFF1D, 0xFF1E)])
}

/// The registers (A, B, C, D, E, H, L) each of `routines` is first entered with while init
/// starts each track.
fn entries(gbs: &[u8], songs: u8, routines: &[u16]) -> Vec<Vec<Option<[u8; 7]>>> {
    let mut out = vec![vec![None; songs as usize]; routines.len()];
    for t in 0..songs {
        let Some((mut p, sp, init, _, _)) = load(gbs) else { break };
        let mut cpu = Cpu::default();
        p.call(&mut cpu, sp, init, t, &mut |c| {
            if let Some(i) = routines.iter().position(|&r| r == c.pc) {
                out[i][t as usize].get_or_insert([c.a, c.b, c.c, c.d, c.e, c.h, c.l]);
            }
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_and_play_lead_to_the_routines_they_call() {
        let mut f = vec![0u8; 0x70];
        f[..4].copy_from_slice(b"GBS\x01");
        f[4] = 5;
        f[6..8].copy_from_slice(&0x0400u16.to_le_bytes());
        f[8..10].copy_from_slice(&0x0400u16.to_le_bytes());
        f[10..12].copy_from_slice(&0x0410u16.to_le_bytes());
        let mut data = vec![0u8; 0x100];
        // init: CALL $0440; RET
        data[0..4].copy_from_slice(&[0xCD, 0x40, 0x04, 0xC9]);
        // play: LD A,1; JP $0480
        data[0x10..0x15].copy_from_slice(&[0x3E, 0x01, 0xC3, 0x80, 0x04]);
        data[0x40] = 0xC9;
        data[0x80] = 0xC9;
        f.extend(data);
        let h = hint(&f).unwrap();
        assert_eq!(h.songs, 5);
        let addrs = |v: &[(u32, Vec<u8>)]| v.iter().map(|r| r.0).collect::<Vec<_>>();
        assert_eq!(addrs(&h.start), vec![0x400, 0x440]);
        assert_eq!(addrs(&h.play), vec![0x410, 0x480]);
        assert!(h.ids.iter().any(|i| i.0 == 0x440 && i.1 == 0 && i.2 == vec![0, 1, 2, 3, 4]));
        // play: LD A,1 then JP to a RET: runs, writes nothing.
        let w = track_writes(&f, 0, 10).unwrap();
        assert_eq!(w.len(), 10);
    }
}
