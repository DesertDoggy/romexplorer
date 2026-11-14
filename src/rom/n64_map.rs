//! N64 music maps: what each track needs from an N64 ROM, found from a matched USF set and
//! kept as numbers and the bytes the ROM doesn't hold.
//!
//! A USF track is a save state of the console a moment into its song (registers and main
//! memory) and the parts of the ROM the song reads after it. The ROM parts are the ROM's own
//! bytes, so a map keeps only where they are. Main memory is mostly code and data the game
//! loaded from the ROM: where it is in the ROM as is, a map keeps where; the rest -- the
//! registers, the game's live variables, anything it unpacked -- is kept as bytes
//! (not slimmed: see [`slim`]). Played, the rebuilt state and ROM parts
//! are the track's own, on the same lazyusf2 core.
//!
//! lazyusf2 keeps the ROM and memory as little-endian words (bytes of each 32-bit word
//! reversed from the cartridge's order); a map works in that order throughout.

use std::collections::HashMap;

use super::gba_map::{self, Run};
use super::psf::{Loaded, UsfImage};

/// Where the ROM parts start in a map's address space; below it is the save state.
const ROM_BASE: u32 = 0x0100_0000;
/// The save state's size with 8 MB of memory: what lazyusf2 sets aside.
const STATE_MAX: usize = 0x80_275C;
/// The tag the reserved sections mark their ROM and save state parts with ("SR64").
const SR64: u32 = 0x3436_5253;
/// A memory stretch is taken from the ROM when at least this long.
const MIN_RUN: usize = 128;
/// The ROM is indexed at this step, by this many bytes.
const STEP: usize = 64;
const KEY: usize = 32;
/// A set is this ROM's when this share of its ROM parts is the ROM's own bytes.
const SAME_SHARE: f64 = 0.9;
pub const RATE: u32 = 44_100;
const DEFAULT_LENGTH_MS: u32 = 150_000;
const DEFAULT_FADE_MS: u32 = 8_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub runs: Vec<Run>,
    pub length_ms: u32,
    pub fade_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Map {
    pub rom_len: u32,
    pub state_len: u32,
    /// `_enablecompare` and `_enablefifofull`.
    pub compare: bool,
    pub fifo_full: bool,
    pub common: Vec<Run>,
    pub tracks: Vec<Track>,
}

/// The ROM in lazyusf2's order, from a cartridge dump in any of the three usual byte orders;
/// None for anything else.
pub fn native(rom: &[u8]) -> Option<Vec<u8>> {
    if rom.len() < 0x1000 || rom.len() % 4 != 0 {
        return None;
    }
    let mut out = rom.to_vec();
    match rom[..4] {
        // .z64: big-endian words.
        [0x80, 0x37, 0x12, 0x40] => out.chunks_exact_mut(4).for_each(|w| w.reverse()),
        // .v64: byte pairs swapped.
        [0x37, 0x80, 0x40, 0x12] => out.chunks_exact_mut(4).for_each(|w| {
            let [a, b, c, d] = [w[0], w[1], w[2], w[3]];
            w.copy_from_slice(&[c, d, a, b]);
        }),
        // .n64: already little-endian words.
        [0x40, 0x12, 0x37, 0x80] => {}
        _ => return None,
    }
    Some(out)
}

/// A track's reserved sections laid out as lazyusf2 loads them: the ROM parts (as a sparse
/// image with the stretches written) and the save state.
struct Parts {
    rom: Vec<u8>,
    rom_spans: Vec<(usize, usize)>,
    state: Vec<u8>,
    state_len: usize,
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn parts(sections: &[&[u8]]) -> Option<Parts> {
    let mut p = Parts { rom: Vec::new(), rom_spans: Vec::new(), state: vec![0u8; STATE_MAX], state_len: 0 };
    let mut wrote_state = 0usize;
    for s in sections {
        let mut at = 0;
        for part in 0..2 {
            let tag = u32_at(s, at)?;
            at += 4;
            if tag != SR64 {
                if part == 0 {
                    continue;
                }
                break;
            }
            loop {
                let len = u32_at(s, at)? as usize;
                at += 4;
                if len == 0 {
                    break;
                }
                let start = u32_at(s, at)? as usize;
                at += 4;
                let data = s.get(at..at + len)?;
                at += len;
                if part == 0 {
                    if p.rom.len() < start + len {
                        p.rom.resize(start + len, 0);
                    }
                    p.rom[start..start + len].copy_from_slice(data);
                    p.rom_spans.push((start, start + len));
                } else {
                    p.state.get_mut(start..start + len)?.copy_from_slice(data);
                    wrote_state = wrote_state.max(start + len);
                }
            }
        }
    }
    // The memory size the state names, and its registers before it.
    let rdram = u32_at(&p.state, 4).unwrap_or(0) as usize;
    p.state_len = if rdram == 0x40_0000 || rdram == 0x80_0000 { rdram + 0x275C } else { wrote_state };
    p.state.truncate(p.state_len.max(wrote_state).min(STATE_MAX));
    p.state_len = p.state.len();
    p.rom_spans.sort_unstable();
    Some(p)
}

/// The ROM indexed by [`KEY`]-byte stretches at every [`STEP`]: any stretch of memory at
/// least `STEP + KEY` long that is in the ROM holds one of them.
fn index(rom: &[u8]) -> HashMap<&[u8], Vec<u32>> {
    let mut map: HashMap<&[u8], Vec<u32>> = HashMap::new();
    let mut i = 0;
    while i + KEY <= rom.len() {
        let k = &rom[i..i + KEY];
        if k.iter().any(|&b| b != k[0]) {
            let v = map.entry(k).or_default();
            if v.len() < 8 {
                v.push(i as u32);
            }
        }
        i += STEP;
    }
    map
}

/// Where stretches of `data` are in `rom`: (data offset, length, ROM offset).
fn layout(data: &[u8], rom: &[u8], index: &HashMap<&[u8], Vec<u32>>) -> Vec<(usize, usize, usize)> {
    let mut out: Vec<(usize, usize, usize)> = Vec::new();
    let mut i = 0;
    let mut covered = 0;
    while i + KEY <= data.len() {
        let k = &data[i..i + KEY];
        if k.iter().all(|&b| b == k[0]) {
            i += 1;
            continue;
        }
        let Some(hits) = index.get(k) else {
            i += 1;
            continue;
        };
        let mut best = (0usize, 0usize, 0usize);
        for &r in hits {
            let r = r as usize;
            let mut back = 0;
            while i > back && r > back && i - back > covered && data[i - back - 1] == rom[r - back - 1] {
                back += 1;
            }
            let mut fwd = KEY;
            while i + fwd < data.len() && r + fwd < rom.len() && data[i + fwd] == rom[r + fwd] {
                fwd += 1;
            }
            if back + fwd > best.1 {
                best = (i - back, back + fwd, r - back);
            }
        }
        if best.1 >= MIN_RUN {
            out.push(best);
            covered = best.0 + best.1;
            i = covered;
        } else {
            i += 1;
        }
    }
    out
}

fn shift(run: Run, by: u32) -> Run {
    match run {
        Run::Bytes(a, b) => Run::Bytes(a.wrapping_add(by), b),
        Run::Fill(a, n, b) => Run::Fill(a.wrapping_add(by), n, b),
        Run::Copy(a, n, f) => Run::Copy(a.wrapping_add(by), n, f),
        Run::Reloc(a, n, f, k) => Run::Reloc(a.wrapping_add(by), n, f, k),
    }
}

/// One track's runs from its loaded USF, over the ROM in lazyusf2's order; None when the set
/// is not this ROM's.
pub fn track_runs(loaded: &Loaded, rom: &[u8], index: &HashMap<&[u8], Vec<u32>>) -> Option<(Vec<Run>, u32)> {
    let p = parts(&loaded.usf_sections())?;
    // The ROM parts: the ROM's own bytes where they are the same, the rest kept.
    let (mut same, mut total) = (0usize, 0usize);
    let mut runs = Vec::new();
    let mut laid = vec![0u8; p.rom.len()];
    for &(a, b) in &p.rom_spans {
        let src = rom.get(a..b.min(rom.len()))?;
        laid[a..a + src.len()].copy_from_slice(src);
        same += src.iter().zip(&p.rom[a..b]).filter(|(x, y)| x == y).count();
        total += b - a;
        runs.push(Run::Copy(ROM_BASE + a as u32, (b - a) as u32, a as u32));
    }
    if total > 0 && (same as f64) < total as f64 * SAME_SHARE {
        return None;
    }
    runs.extend(gba_map::diff(&p.rom, &p.rom_spans, &laid).into_iter().map(|r| shift(r, ROM_BASE)));
    // The save state: memory stretches from the ROM, the rest kept.
    let mut laid = vec![0u8; p.state.len()];
    for (d, n, r) in layout(&p.state, rom, index) {
        laid[d..d + n].copy_from_slice(&rom[r..r + n]);
        runs.push(Run::Copy(d as u32, n as u32, r as u32));
    }
    runs.extend(gba_map::diff(&p.state, &[(0, p.state.len())], &laid));
    Some((runs, p.state_len as u32))
}

/// Builds a map one loaded track at a time (a loaded USF holds its whole library).
pub struct Builder<'a> {
    rom: &'a [u8],
    index: HashMap<&'a [u8], Vec<u32>>,
    found: Vec<Track>,
    state_len: u32,
    flags: (bool, bool),
    theirs: bool,
}

impl<'a> Builder<'a> {
    /// For `rom` in lazyusf2's order (see [`native`]).
    pub fn new(rom: &'a [u8]) -> Self {
        Builder { rom, index: index(rom), found: Vec::new(), state_len: 0, flags: (false, false), theirs: true }
    }

    /// Adds a set's next track; false (and the set is not this ROM's) when it doesn't fit.
    pub fn add(&mut self, loaded: &Loaded) -> bool {
        if !self.theirs {
            return false;
        }
        let Some((runs, len)) = track_runs(loaded, self.rom, &self.index) else {
            self.theirs = false;
            return false;
        };
        if self.found.is_empty() {
            self.flags = loaded.usf_flags();
        }
        self.state_len = self.state_len.max(len);
        let info = loaded.info();
        let length = info.length_ms;
        self.found.push(Track {
            runs,
            length_ms: length.unwrap_or(DEFAULT_LENGTH_MS),
            fade_ms: info.fade_ms.unwrap_or(if length.is_some() { 0 } else { DEFAULT_FADE_MS }),
        });
        true
    }

    /// The map, or None when the set is not this ROM's or held no track.
    pub fn finish(self) -> Option<Map> {
        let mut found = self.found;
        if !self.theirs || found.is_empty() {
            return None;
        }
        use std::collections::HashSet;
        let others: Vec<HashSet<&Run>> = found[1..].iter().map(|t| t.runs.iter().collect()).collect();
        let common: Vec<Run> = found[0].runs.iter().filter(|r| others.iter().all(|o| o.contains(r))).cloned().collect();
        drop(others);
        let shared: HashSet<Run> = common.iter().cloned().collect();
        for t in &mut found {
            t.runs.retain(|r| !shared.contains(r));
        }
        Some(Map { rom_len: self.rom.len() as u32, state_len: self.state_len, compare: self.flags.0, fifo_full: self.flags.1, common, tracks: found })
    }
}

impl Map {
    /// Track `t`'s reserved section (its ROM parts and save state), rebuilt from the ROM.
    pub fn section(&self, rom: &[u8], t: usize) -> Result<Vec<u8>, String> {
        let track = self.tracks.get(t).ok_or("no such song in this game's music map")?;
        self.section_with(rom, &self.common, track)
    }

    fn section_with(&self, rom: &[u8], common: &[Run], track: &Track) -> Result<Vec<u8>, String> {
        if rom.len() != self.rom_len as usize {
            return Err("this ROM is not the one the music map was made for".into());
        }
        let mut state = vec![0u8; self.state_len as usize];
        let mut parts = Vec::new();
        for phase in 0..3 {
            for r in common.iter().chain(&track.runs).filter(|r| r.phase() == phase) {
                let at = match r {
                    Run::Bytes(a, _) | Run::Fill(a, _, _) | Run::Copy(a, _, _) | Run::Reloc(a, _, _, _) => *a,
                };
                if at >= ROM_BASE {
                    r.clone().apply_shifted(&mut parts, rom, ROM_BASE);
                } else {
                    r.apply(&mut state, rom);
                }
            }
        }
        state.truncate(self.state_len as usize);
        let mut out = Vec::with_capacity(parts.len() + state.len() + 40);
        let word = |out: &mut Vec<u8>, w: u32| out.extend_from_slice(&w.to_le_bytes());
        word(&mut out, SR64);
        if !parts.is_empty() {
            word(&mut out, parts.len() as u32);
            word(&mut out, 0);
            out.extend_from_slice(&parts);
        }
        word(&mut out, 0);
        word(&mut out, SR64);
        word(&mut out, state.len() as u32);
        word(&mut out, 0);
        out.extend_from_slice(&state);
        word(&mut out, 0);
        Ok(out)
    }

    /// `n64map 1`, `l <ROM length> <state length> <compare> <fifo full>`, `c <runs>` (shared),
    /// then `t <length ms> <fade ms> <runs>` per track, the runs as a GBA map writes them.
    pub fn encode(&self) -> String {
        let mut out = format!("n64map 1\nl {} {} {} {}\nc", self.rom_len, self.state_len, u8::from(self.compare), u8::from(self.fifo_full));
        gba_map::encode_runs(&mut out, &self.common);
        out.push('\n');
        for t in &self.tracks {
            out.push_str(&format!("t {} {}", t.length_ms, t.fade_ms));
            gba_map::encode_runs(&mut out, &t.runs);
            out.push('\n');
        }
        out
    }

    pub fn decode(text: &str) -> Option<Map> {
        let mut lines = text.lines();
        if lines.next()? != "n64map 1" {
            return None;
        }
        let mut l = lines.next()?.strip_prefix("l ")?.split_whitespace();
        let rom_len = l.next()?.parse().ok()?;
        let state_len = l.next()?.parse().ok()?;
        let compare = l.next()? == "1";
        let fifo_full = l.next()? == "1";
        let mut common_words = lines.next()?.split_whitespace();
        if common_words.next()? != "c" {
            return None;
        }
        let common = gba_map::decode_runs(common_words)?;
        let mut tracks = Vec::new();
        for line in lines.filter(|l| !l.is_empty()) {
            let mut w = line.split_whitespace();
            if w.next()? != "t" {
                return None;
            }
            let length_ms = w.next()?.parse().ok()?;
            let fade_ms = w.next()?.parse().ok()?;
            tracks.push(Track { runs: gba_map::decode_runs(w)?, length_ms, fade_ms });
        }
        Some(Map { rom_len, state_len, compare, fifo_full, common, tracks })
    }

    pub fn file_bytes(&self) -> usize {
        let n = |runs: &[Run]| runs.iter().map(|r| if let Run::Bytes(_, b) = r { b.len() } else { 0 }).sum::<usize>();
        n(&self.common) + self.tracks.iter().map(|t| n(&t.runs)).sum::<usize>()
    }
}

trait ApplyShifted {
    fn apply_shifted(self, image: &mut Vec<u8>, rom: &[u8], base: u32);
}

impl ApplyShifted for Run {
    fn apply_shifted(self, image: &mut Vec<u8>, rom: &[u8], base: u32) {
        shift(self, 0u32.wrapping_sub(base)).apply(image, rom);
    }
}

/// Renders `seconds` of a rebuilt section.
pub fn render(section: &[u8], compare: bool, fifo_full: bool, seconds: u32) -> Result<Vec<i16>, String> {
    let mut player = UsfImage::open(&[section], compare, fifo_full)?;
    let mut out = vec![0i16; RATE as usize * 2 * seconds as usize];
    player.render(&mut out, RATE)?;
    Ok(out)
}

/// Slimming an N64 map is not done: measured on a 10-game sample it took 54-88% of each
/// game's build (6 minutes for Quake II) to save a few hundred bytes (269 of 22,518), and a
/// trial with runs left out could send the player into a loop that never returned. Checks
/// that the map's ROM is the right one and leaves the map as built.
pub fn slim(map: &mut Map, rom: &[u8]) -> Result<usize, String> {
    if rom.len() != map.rom_len as usize {
        return Err("this ROM is not the one the music map was made for".into());
    }
    Ok(0)
}

/// Plays a track from the ROM (in lazyusf2's order) by its map.
pub struct MapSong {
    player: UsfImage,
}

impl MapSong {
    pub fn open(m: &Map, rom: &[u8], t: usize) -> Result<Self, String> {
        Ok(Self { player: UsfImage::open(&[&m.section(rom, t)?], m.compare, m.fifo_full)? })
    }

    pub fn render(&mut self, out: &mut [i16]) -> Result<(), String> {
        self.player.render(out, RATE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(seed: u32, len: usize) -> Vec<u8> {
        let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect()
    }

    #[test]
    fn byte_orders_come_to_little_endian_words() {
        let mut z = vec![0u8; 0x1000];
        z[..8].copy_from_slice(&[0x80, 0x37, 0x12, 0x40, 1, 2, 3, 4]);
        let n = native(&z).unwrap();
        assert_eq!(&n[..8], &[0x40, 0x12, 0x37, 0x80, 4, 3, 2, 1]);
        let mut v = vec![0u8; 0x1000];
        v[..8].copy_from_slice(&[0x37, 0x80, 0x40, 0x12, 2, 1, 4, 3]);
        assert_eq!(&native(&v).unwrap()[..8], &n[..8]);
        assert_eq!(&native(&n).unwrap()[..8], &n[..8]);
        assert!(native(&[0u8; 0x1000]).is_none());
    }

    #[test]
    fn a_section_rebuilds_from_the_rom() {
        let rom = noise(3, 0x10_0000);
        // The state: registers of its own, then memory holding two stretches of the ROM.
        let rdram = 0x40_0000usize;
        let mut state = vec![0u8; rdram + 0x275C];
        state[4..8].copy_from_slice(&(rdram as u32).to_le_bytes());
        state[16..64].copy_from_slice(&noise(9, 48));
        state[0x275C + 0x1000..0x275C + 0x9000].copy_from_slice(&rom[0x2_0000..0x2_8000]);
        state[0x275C + 0x2_0000..0x275C + 0x2_0400].copy_from_slice(&rom[0x5_1234..0x5_1634]);
        let mut section = Vec::new();
        let w = |s: &mut Vec<u8>, x: u32| s.extend_from_slice(&x.to_le_bytes());
        w(&mut section, SR64);
        w(&mut section, 0x2000);
        w(&mut section, 0x4_0000);
        section.extend_from_slice(&rom[0x4_0000..0x4_2000]);
        w(&mut section, 0);
        w(&mut section, SR64);
        w(&mut section, state.len() as u32);
        w(&mut section, 0);
        section.extend_from_slice(&state);
        w(&mut section, 0);
        let p = parts(&[&section]).unwrap();
        assert_eq!(p.state_len, state.len());
        let idx = index(&rom);
        let laid = layout(&p.state, &rom, &idx);
        assert!(laid.iter().map(|l| l.1).sum::<usize>() >= 0x8000 + 0x400 - 2 * KEY, "{laid:?}");
        // A map of it lays the same section back.
        let (runs, len) = {
            let p = parts(&[&section]).unwrap();
            let mut runs = vec![Run::Copy(ROM_BASE + 0x4_0000, 0x2000, 0x4_0000)];
            let mut l = vec![0u8; p.state.len()];
            for (d, n, r) in layout(&p.state, &rom, &idx) {
                l[d..d + n].copy_from_slice(&rom[r..r + n]);
                runs.push(Run::Copy(d as u32, n as u32, r as u32));
            }
            runs.extend(gba_map::diff(&p.state, &[(0, p.state.len())], &l));
            (runs, p.state_len as u32)
        };
        let m = Map { rom_len: rom.len() as u32, state_len: len, compare: false, fifo_full: false, common: vec![], tracks: vec![Track { runs, length_ms: 1000, fade_ms: 0 }] };
        let back = Map::decode(&m.encode()).unwrap();
        assert_eq!(back, m);
        let rebuilt = back.section(&rom, 0).unwrap();
        let again = parts(&[&rebuilt]).unwrap();
        assert_eq!(again.state, p.state);
        assert_eq!(&again.rom[0x4_0000..0x4_2000], &rom[0x4_0000..0x4_2000]);
        assert!(m.file_bytes() < 0x100, "{}", m.file_bytes());
    }
}
