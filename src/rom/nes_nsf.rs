//! What an NSF file says about its game's sound code, as a [`Hint`] for the music search.
//!
//! An NSF carries the game's own sound driver at the addresses the game runs it from (6502
//! code isn't relocatable), with a small init and play routine written for the file or taken from
//! the game. Their addresses, and the routines they call, point the search straight at the
//! game's sound routine and the routine that starts a song, so it no longer has to find them
//! in the boot alone. The music still plays from the ROM: the NSF is only read for addresses.

use super::music_search::Hint;

/// Code bytes compared at each address, in the NSF and in the game.
const CODE_BYTES: usize = 12;
/// How far a routine is read for the routines it calls.
const SCAN_BYTES: usize = 96;
/// Most routines taken from each of init and play.
const MAX_ROUTINES: usize = 8;

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

    /// `from`, then the routines it calls or jumps to (two levels down), as far as straight-line
    /// reading finds them.
    fn routines(&self, from: u16) -> Vec<(u32, Vec<u8>)> {
        let mut out: Vec<u16> = vec![from];
        let mut i = 0;
        let mut level = vec![0u8];
        while i < out.len() && out.len() < MAX_ROUTINES {
            if level[i] < 2 {
                for t in self.calls_in(out[i]) {
                    if !out.contains(&t) && out.len() < MAX_ROUTINES {
                        out.push(t);
                        level.push(level[i] + 1);
                    }
                }
            }
            i += 1;
        }
        out.into_iter()
            .filter_map(|a| Some((a as u32, self.code(a)?)))
            .collect()
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
    Some(Hint {
        play: n.routines(n.play),
        start: n.routines(n.init),
        songs: n.songs,
    })
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
