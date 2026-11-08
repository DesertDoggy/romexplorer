//! Where an original audio format file's code and song data sit in a ROM: the layout part of
//! a game's music map. A map lets the game's own sound code run straight from the user's
//! ROM, with no boot and nothing of the game shipped: only the ROM ranges and where the CPU
//! sees them.

use std::collections::HashMap;

/// Bytes a run must share before it counts (shorter ones match by chance).
const MIN_RUN: usize = 16;

/// A stretch of the file's data found in the ROM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    /// Offset in the file's data.
    pub data: usize,
    pub len: usize,
    /// Offset in the ROM's program data.
    pub rom: usize,
}

/// The file's data laid over the ROM.
#[derive(Debug, Clone, Default)]
pub struct Layout {
    pub runs: Vec<Run>,
    /// Bytes the file's tracks read (see `nes_nsf::used_bytes`), and how many of them are in
    /// a run.
    pub needed: usize,
    pub placed: usize,
    /// Stretches of needed bytes in no run (offset in the data, length): code or data the
    /// file's author wrote or patched.
    pub missing: Vec<(usize, usize)>,
}

/// Whether a key is mostly one byte (padding, a run of NOPs): three quarters of it or more.
fn is_filler(key: &[u8]) -> bool {
    let mut counts = [0u8; 256];
    key.iter().for_each(|&b| counts[b as usize] += 1);
    counts.iter().any(|&n| n as usize * 4 >= key.len() * 3)
}

/// Lays `data` over `rom`: greedy longest runs of at least [`MIN_RUN`] bytes, preferring one
/// that continues the previous run's place in the ROM. Only bytes `used` marks are counted
/// (all when it is shorter than the data).
pub fn layout(data: &[u8], used: &[bool], rom: &[u8]) -> Layout {
    let mut index: HashMap<&[u8], Vec<usize>> = HashMap::new();
    if rom.len() >= MIN_RUN {
        for i in 0..=rom.len() - MIN_RUN {
            let key = &rom[i..i + MIN_RUN];
            // Filler (mostly one byte repeated) places anywhere: not a match.
            if is_filler(key) {
                continue;
            }
            let v = index.entry(key).or_default();
            if v.len() < 64 {
                v.push(i);
            }
        }
    }
    let needed = |i: usize| used.get(i).copied().unwrap_or(used.len() < data.len());
    let mut out = Layout::default();
    let mut placed = vec![false; data.len()];
    let mut i = 0;
    let mut last: Option<isize> = None;
    while i + MIN_RUN <= data.len() {
        let Some(cands) = index.get(&data[i..i + MIN_RUN]) else {
            i += 1;
            continue;
        };
        let extend = |r: usize| {
            let mut n = MIN_RUN;
            while i + n < data.len() && r + n < rom.len() && data[i + n] == rom[r + n] {
                n += 1;
            }
            n
        };
        // The longest, and on a tie the one keeping the previous run's offset.
        let best = cands
            .iter()
            .map(|&r| (extend(r), last == Some(r as isize - i as isize), r))
            .max()
            .unwrap();
        let (len, _, r) = best;
        out.runs.push(Run { data: i, len, rom: r });
        placed[i..i + len].iter_mut().for_each(|p| *p = true);
        last = Some(r as isize - i as isize);
        i += len;
    }
    let mut gap: Option<usize> = None;
    for j in 0..=data.len() {
        let open = j < data.len() && needed(j) && !placed[j];
        if j < data.len() && needed(j) {
            out.needed += 1;
            out.placed += placed[j] as usize;
        }
        match (open, gap) {
            (true, None) => gap = Some(j),
            (false, Some(g)) => {
                out.missing.push((g, j - g));
                gap = None;
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_is_placed_where_the_rom_holds_it() {
        let mut x = 12345u32;
        let rom: Vec<u8> = (0..4096)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (x >> 16) as u8
            })
            .collect();
        // The file: 100 ROM bytes from 1000, 8 bytes of its own, 200 from 3000.
        let mut data = rom[1000..1100].to_vec();
        data.extend_from_slice(&[0xEE; 8]);
        data.extend_from_slice(&rom[3000..3200]);
        let l = layout(&data, &[], &rom);
        assert_eq!(l.runs[0], Run { data: 0, len: 100, rom: 1000 });
        assert_eq!(l.runs[1], Run { data: 108, len: 200, rom: 3000 });
        assert_eq!(l.missing, vec![(100, 8)]);
        assert_eq!((l.needed, l.placed), (308, 300));
    }
}
