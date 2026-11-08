//! What a GBS file says about its game's sound code, as a [`Hint`] for the music search.
//!
//! A GBS holds the game's sound driver at the addresses the game runs it from, with an init
//! routine (song number in A) and a play routine (called each frame or timer tick), like an
//! NSF does for the NES (see `nes_nsf`). Only the addresses are taken; the music plays from
//! the ROM.

use super::music_search::{hint_routines, Hint};

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
    Some(Hint {
        play: hint_routines(g.play, |a| g.calls_in(a), |a| g.code(a)),
        start: hint_routines(g.init, |a| g.calls_in(a), |a| g.code(a)),
        songs: g.songs,
    })
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
    }
}
