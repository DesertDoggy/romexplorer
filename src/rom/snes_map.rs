//! SNES music maps: what each track's sound chip snapshot needs from a SNES ROM, found from a
//! matched `.spc` set and kept as numbers and the few bytes the ROM doesn't hold.
//!
//! An `.spc` is the sound chip's whole state while a track plays: 64 KB of sound memory (the
//! driver, the samples, the song), the DSP's registers and the sound CPU's. Most of that
//! memory is in the ROM as it is ([`super::spc_map`] finds where). The rest -- the driver's
//! live variables, a song unpacked at boot, a decompressed driver -- is kept in the map as
//! bytes, the same way a GBA map keeps a set's boot patches ([`super::gba_map`]); bytes the
//! sound never needs are left out by [`slim`]. Played, the ROM's bytes with those laid over
//! them are the track's own snapshot, played on the same chip emulator.
//!
//! Bytes every track shares are kept once; each track keeps its own on top, with the chip's
//! registers and its length and fade from the `.spc`'s tag.

use super::gba_map::{self, Run};
use super::snes_apu::{Apu, SAMPLE_RATE, SPC_FILE_SIZE};
use super::spc_map;

/// Where the sound memory, the DSP registers and the 64 bytes under the boot ROM sit in an
/// `.spc`.
const ARAM: usize = 0x100;
const DSP: usize = 0x10100;
const EXTRA: usize = 0x101C0;
/// A track's image: the sound memory, then the 64 bytes under the boot ROM.
const IMAGE: usize = 0x10040;

/// A track kept only when at least this share of the sound memory it needs is in the ROM:
/// below it the set is not of this ROM, or its sound is packed.
const MIN_SHARE: f64 = 0.5;
/// A track's length when the `.spc` has no tag.
const DEFAULT_LENGTH_MS: u32 = 150_000;
const DEFAULT_FADE_MS: u32 = 8_000;
/// Bytes most tracks agree on go in the shared part only in stretches at least this long.
const MIN_SHARED_STRETCH: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub runs: Vec<Run>,
    /// The sound CPU's PC, then A, X, Y, PSW, SP.
    pub pc: u16,
    pub regs: [u8; 5],
    pub dsp: [u8; 128],
    pub length_ms: u32,
    pub fade_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Map {
    /// The ROM image's length (without a copier header).
    pub rom_len: u32,
    pub common: Vec<Run>,
    pub tracks: Vec<Track>,
}

/// The sound memory bytes a track needs, as ranges of its image: all but the echo buffer
/// (rewritten as it plays), plus the 64 bytes under the boot ROM.
fn spans(spc: &[u8]) -> Vec<(usize, usize)> {
    let need = spc_map::needed(spc);
    let mut out = Vec::new();
    let mut i = 0;
    while i < need.len() {
        if !need[i] {
            i += 1;
            continue;
        }
        let start = i;
        while i < need.len() && need[i] {
            i += 1;
        }
        out.push((start, i));
    }
    out.push((0x10000, IMAGE));
    out
}

fn image_of(spc: &[u8]) -> Vec<u8> {
    let mut image = spc[ARAM..ARAM + 0x10000].to_vec();
    image.extend_from_slice(&spc[EXTRA..EXTRA + 64]);
    image
}

/// The track's length and fade from the `.spc`'s ID666 tag, text or binary; None without one.
pub fn tag_times(spc: &[u8]) -> (Option<u32>, Option<u32>) {
    if spc.len() < 0xB1 || spc[0x23] != 26 {
        return (None, None);
    }
    let field = &spc[0xA9..0xB1];
    let text = field.iter().all(|&b| b.is_ascii_digit() || b == 0 || b == b' ');
    let number = |b: &[u8]| -> Option<u32> {
        let s: String = b.iter().take_while(|&&c| c != 0).map(|&c| c as char).collect();
        s.trim().parse().ok()
    };
    let (secs, fade) = if text {
        (number(&field[..3]), number(&field[3..8]))
    } else {
        (Some(u32::from_le_bytes([field[0], field[1], field[2], 0])), Some(u32::from_le_bytes([field[3], field[4], field[5], field[6]])))
    };
    let secs = secs.filter(|&s| s > 0 && s < 3600);
    (secs.map(|s| s * 1000), fade.filter(|&f| f < 60_000).filter(|_| secs.is_some()))
}

impl Map {
    /// The map of `rom` (without a copier header) from a set's `.spc` files (in the set's
    /// order), or None when none of its tracks is enough in this ROM. Files that are not an
    /// `.spc`, or too little of whose memory is in the ROM, are left out.
    pub fn build(spcs: &[&[u8]], rom: &[u8]) -> Option<Map> {
        // Two ways of sharing a game's bytes between its tracks; neither wins everywhere
        // (measured on the 20 biggest maps: byte by byte took one to a third, and another to
        // four times, of run by run), so each game keeps the smaller.
        let placed = spc_map::tracks(spcs, rom);
        let by_run = Self::build_with(spcs, rom, placed.clone(), false)?;
        let by_byte = Self::build_with(spcs, rom, placed, true)?;
        Some(if by_byte.encode().len() < by_run.encode().len() { by_byte } else { by_run })
    }

    fn build_with(spcs: &[&[u8]], rom: &[u8], placed: Vec<Option<spc_map::Track>>, by_byte: bool) -> Option<Map> {
        // Per track: its ROM copies, the image it needs and where it needs it.
        struct Wanted {
            copies: Vec<Run>,
            image: Vec<u8>,
            spans: Vec<(usize, usize)>,
            pc: u16,
            regs: [u8; 5],
            dsp: [u8; 128],
            length_ms: u32,
            fade_ms: u32,
        }
        let mut wanted = Vec::new();
        for (spc, t) in spcs.iter().zip(placed) {
            let Some(t) = t else { continue };
            let (got, total) = spc_map::coverage(spc, &t.runs);
            if (got as f64) < total as f64 * MIN_SHARE {
                continue;
            }
            let (length, fade) = tag_times(spc);
            wanted.push(Wanted {
                copies: t.runs.iter().map(|r| Run::Copy(r.data as u32, r.len as u32, r.rom as u32)).collect(),
                image: image_of(spc),
                spans: spans(spc),
                pc: t.pc,
                regs: t.regs,
                dsp: t.dsp,
                length_ms: length.unwrap_or(DEFAULT_LENGTH_MS),
                fade_ms: fade.unwrap_or(if length.is_some() { 0 } else { DEFAULT_FADE_MS }),
            });
        }
        if wanted.is_empty() {
            return None;
        }
        if !by_byte {
            return Some(Self::shared_by_run(wanted.into_iter().map(|w| {
                let mut runs = w.copies;
                let mut laid = vec![0u8; IMAGE];
                for r in &runs {
                    r.apply(&mut laid, rom);
                }
                runs.extend(gba_map::diff(&w.image, &w.spans, &laid));
                Track { runs, pc: w.pc, regs: w.regs, dsp: w.dsp, length_ms: w.length_ms, fade_ms: w.fade_ms }
            }).collect(), rom.len()));
        }
        // Copies every track has are kept once.
        use std::collections::HashSet;
        let others: Vec<HashSet<&Run>> = wanted[1..].iter().map(|w| w.copies.iter().collect()).collect();
        let shared_copies: Vec<Run> = wanted[0].copies.iter().filter(|r| others.iter().all(|o| o.contains(r))).cloned().collect();
        drop(others);
        let shared: HashSet<Run> = shared_copies.iter().cloned().collect();
        let lay = |runs: &[Run]| {
            let mut image = vec![0u8; IMAGE];
            for phase in 0..3 {
                for r in runs.iter().filter(|r| r.phase() == phase) {
                    r.apply(&mut image, rom);
                }
            }
            image.truncate(IMAGE);
            image
        };
        // Bytes shared byte by byte, not run by run: tracks of one game hold the same driver
        // and samples, with a few live variables apart, and a run that differs in one byte
        // was kept whole by every track. Each byte the most tracks agree on (two at least,
        // or the only track's) goes in once; each track keeps where it differs.
        let base = lay(&shared_copies);
        let mut common_image = base.clone();
        let mut common_need = vec![false; IMAGE];
        {
            let mut need: Vec<Vec<bool>> = Vec::with_capacity(wanted.len());
            for w in &wanted {
                let mut n = vec![false; IMAGE];
                for &(a, b) in &w.spans {
                    n[a..b.min(IMAGE)].iter_mut().for_each(|x| *x = true);
                }
                need.push(n);
            }
            let mut counts = [0u16; 256];
            for i in 0..IMAGE {
                counts.iter_mut().for_each(|c| *c = 0);
                for (w, n) in wanted.iter().zip(&need) {
                    if n[i] {
                        counts[w.image[i] as usize] += 1;
                    }
                }
                let (value, &count) = counts.iter().enumerate().max_by_key(|(v, &c)| (c, std::cmp::Reverse(*v))).expect("256 values");
                // Most of the tracks that need the byte agree on it.
                let needing = need.iter().filter(|n| n[i]).count();
                if count as usize * 2 > needing && (count >= 2 || wanted.len() == 1) {
                    common_image[i] = value as u8;
                    common_need[i] = true;
                }
            }
        }
        // Short agreeing stretches cost more as their own runs than they save.
        let mut i = 0;
        while i < IMAGE {
            if !common_need[i] {
                i += 1;
                continue;
            }
            let a = i;
            while i < IMAGE && common_need[i] {
                i += 1;
            }
            if i - a < MIN_SHARED_STRETCH {
                common_need[a..i].iter_mut().for_each(|x| *x = false);
            }
        }
        let mut common_spans = Vec::new();
        let mut i = 0;
        while i < IMAGE {
            if !common_need[i] {
                i += 1;
                continue;
            }
            let a = i;
            while i < IMAGE && common_need[i] {
                i += 1;
            }
            common_spans.push((a, i));
        }
        let mut common = shared_copies;
        common.extend(gba_map::diff(&common_image, &common_spans, &base));
        let mut tracks = Vec::new();
        for w in wanted {
            let mut runs: Vec<Run> = w.copies.into_iter().filter(|r| !shared.contains(r)).collect();
            let laid = lay(&common.iter().chain(&runs).cloned().collect::<Vec<_>>());
            runs.extend(gba_map::diff(&w.image, &w.spans, &laid));
            tracks.push(Track { runs, pc: w.pc, regs: w.regs, dsp: w.dsp, length_ms: w.length_ms, fade_ms: w.fade_ms });
        }
        Some(Map { rom_len: rom.len() as u32, common, tracks })
    }

    /// Runs every track has, kept once.
    fn shared_by_run(mut tracks: Vec<Track>, rom_len: usize) -> Map {
        use std::collections::HashSet;
        let others: Vec<HashSet<&Run>> = tracks[1..].iter().map(|t| t.runs.iter().collect()).collect();
        let common: Vec<Run> = tracks[0].runs.iter().filter(|r| others.iter().all(|o| o.contains(r))).cloned().collect();
        drop(others);
        let shared: HashSet<Run> = common.iter().cloned().collect();
        for t in &mut tracks {
            t.runs.retain(|r| !shared.contains(r));
        }
        Map { rom_len: rom_len as u32, common, tracks }
    }

    /// Track `t`'s `.spc`, rebuilt from the ROM.
    pub fn spc(&self, rom: &[u8], t: usize) -> Result<Vec<u8>, String> {
        let track = self.tracks.get(t).ok_or("no such song in this game's music map")?;
        self.spc_with(rom, &self.common, track)
    }

    fn spc_with(&self, rom: &[u8], common: &[Run], track: &Track) -> Result<Vec<u8>, String> {
        if rom.len() != self.rom_len as usize {
            return Err("this ROM is not the one the music map was made for".into());
        }
        let mut image = vec![0u8; IMAGE];
        for phase in 0..3 {
            for r in common.iter().chain(&track.runs).filter(|r| r.phase() == phase) {
                r.apply(&mut image, rom);
            }
        }
        image.truncate(IMAGE);
        let mut spc = vec![0u8; SPC_FILE_SIZE];
        spc[..33].copy_from_slice(b"SNES-SPC700 Sound File Data v0.30");
        spc[0x21] = 26;
        spc[0x22] = 26;
        spc[0x23] = 27; // no ID666 tag
        spc[0x24] = 30;
        spc[0x25..0x27].copy_from_slice(&track.pc.to_le_bytes());
        spc[0x27..0x2C].copy_from_slice(&track.regs);
        spc[ARAM..ARAM + 0x10000].copy_from_slice(&image[..0x10000]);
        spc[DSP..DSP + 128].copy_from_slice(&track.dsp);
        spc[EXTRA..EXTRA + 64].copy_from_slice(&image[0x10000..]);
        Ok(spc)
    }

    /// `snesmap 1`, `l <ROM length>`, `c <runs>` (shared), then per track `t <length ms> <fade
    /// ms> <pc> <A X Y PSW SP> <DSP registers> <runs>`, the registers in hex (the DSP's as one
    /// word) and the runs as a GBA map writes them.
    pub fn encode(&self) -> String {
        let mut out = format!("snesmap 1\nl {}\nc", self.rom_len);
        gba_map::encode_runs(&mut out, &self.common);
        out.push('\n');
        for t in &self.tracks {
            let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
            out.push_str(&format!("t {} {} {:x} {} {}", t.length_ms, t.fade_ms, t.pc, hex(&t.regs), hex(&t.dsp)));
            gba_map::encode_runs(&mut out, &t.runs);
            out.push('\n');
        }
        out
    }

    pub fn decode(text: &str) -> Option<Map> {
        fn hex<const N: usize>(w: &str) -> Option<[u8; N]> {
            if w.len() != N * 2 {
                return None;
            }
            let mut out = [0u8; N];
            for (i, b) in out.iter_mut().enumerate() {
                *b = u8::from_str_radix(w.get(i * 2..i * 2 + 2)?, 16).ok()?;
            }
            Some(out)
        }
        let mut lines = text.lines();
        if lines.next()? != "snesmap 1" {
            return None;
        }
        let rom_len = lines.next()?.strip_prefix("l ")?.parse().ok()?;
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
            let pc = u16::from_str_radix(w.next()?, 16).ok()?;
            let regs = hex::<5>(w.next()?)?;
            let dsp = hex::<128>(w.next()?)?;
            tracks.push(Track { runs: gba_map::decode_runs(w)?, pc, regs, dsp, length_ms, fade_ms });
        }
        Some(Map { rom_len, common, tracks })
    }

    /// How many bytes the map keeps that are not in the ROM (not counting fills).
    pub fn file_bytes(&self) -> usize {
        let n = |runs: &[Run]| runs.iter().map(|r| if let Run::Bytes(_, b) = r { b.len() } else { 0 }).sum::<usize>();
        n(&self.common) + self.tracks.iter().map(|t| n(&t.runs)).sum::<usize>()
    }
}

/// Renders `seconds` of an `.spc` (stereo at [`SAMPLE_RATE`]).
pub fn render(spc: &[u8], seconds: u32) -> Result<Vec<i16>, String> {
    let mut apu = Apu::from_spc(spc)?;
    let mut out = vec![0i16; SAMPLE_RATE as usize * 2 * seconds as usize];
    apu.play(&mut out)?;
    Ok(out)
}

/// Leaves out the shared byte and fill runs the music does not need (see
/// [`gba_map::slim`]). Returns how many of the kept bytes it left out.
pub fn slim(map: &mut Map, rom: &[u8]) -> Result<usize, String> {
    if rom.len() != map.rom_len as usize {
        return Err("this ROM is not the one the music map was made for".into());
    }
    let play = |common: &[Run], t: usize, seconds: u32| -> Option<Vec<i16>> {
        let spc = map.spc_with(rom, common, &map.tracks[t]).ok()?;
        render(&spc, seconds).ok().filter(|s| s.iter().any(|&x| x != 0))
    };
    let dropped = gba_map::slim_common(&map.common, map.tracks.len(), &play);
    let before = map.file_bytes();
    map.common = map.common.iter().enumerate().filter(|(i, _)| !dropped.contains(i)).map(|(_, r)| r.clone()).collect();
    Ok(before - map.file_bytes())
}

/// Plays a track from the ROM by its map.
pub struct MapSong {
    apu: Apu,
}

impl MapSong {
    pub fn open(m: &Map, rom: &[u8], t: usize) -> Result<Self, String> {
        Ok(Self { apu: Apu::from_spc(&m.spc(rom, t)?)? })
    }

    /// Fills `out` (stereo at [`SAMPLE_RATE`]).
    pub fn render(&mut self, out: &mut [i16]) -> Result<(), String> {
        self.apu.play(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A made-up `.spc`: noise-like sound memory, echo off (the whole memory is needed).
    fn spc() -> Vec<u8> {
        let mut x = 7u32;
        let mut spc = vec![0u8; SPC_FILE_SIZE];
        spc[..27].copy_from_slice(b"SNES-SPC700 Sound File Data");
        for b in spc[ARAM..ARAM + 0x10000].iter_mut() {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            *b = (x >> 16) as u8;
        }
        spc[DSP + 0x6C] = 0x20;
        spc[0x25] = 0x34;
        spc[0x26] = 0x12;
        spc
    }

    #[test]
    fn a_track_rebuilds_its_whole_snapshot() {
        let spc = spc();
        let aram = &spc[ARAM..ARAM + 0x10000];
        // Most of the memory in the ROM; the zero page and a stretch past it only in the file.
        let mut rom = vec![0xFFu8; 0x40000];
        rom[0x1000..0x9000].copy_from_slice(&aram[0x0200..0x8200]);
        rom[0x20000..0x27000].copy_from_slice(&aram[0x9000..0x10000]);
        let m = Map::build(&[&spc], &rom).unwrap();
        let back = Map::decode(&m.encode()).unwrap();
        assert_eq!(back, m);
        let rebuilt = back.spc(&rom, 0).unwrap();
        assert_eq!(&rebuilt[ARAM..ARAM + 0x10000], aram);
        assert_eq!(&rebuilt[0x25..0x27], &spc[0x25..0x27]);
        assert!(m.file_bytes() < 0x1100, "{}", m.file_bytes());
    }

    #[test]
    fn bytes_the_tracks_share_are_kept_once() {
        let a = spc();
        let mut b = a.clone();
        let mut c = a.clone();
        // Each track has its own live variables in the middle of the data the ROM lacks.
        b[ARAM + 0x0100] ^= 0x5A;
        c[ARAM + 0x0180] ^= 0xA5;
        let aram = &a[ARAM..ARAM + 0x10000];
        let mut rom = vec![0xFFu8; 0x40000];
        rom[0x1000..0x9000].copy_from_slice(&aram[0x0200..0x8200]);
        rom[0x20000..0x27000].copy_from_slice(&aram[0x9000..0x10000]);
        let m = Map::build(&[&a, &b, &c], &rom).unwrap();
        let back = Map::decode(&m.encode()).unwrap();
        for (t, want) in [&a, &b, &c].iter().enumerate() {
            let rebuilt = back.spc(&rom, t).unwrap();
            assert_eq!(&rebuilt[ARAM..ARAM + 0x10000], &want[ARAM..ARAM + 0x10000], "track {t}");
        }
        // About one track's missing bytes in all, not three.
        assert!(m.file_bytes() < 0x1200, "{}", m.file_bytes());
    }

    #[test]
    fn a_set_of_another_rom_makes_no_map() {
        assert!(Map::build(&[&spc()], &vec![0x55u8; 0x40000]).is_none());
    }

    #[test]
    fn tag_times_text_and_binary() {
        let mut s = spc();
        s[0x23] = 26;
        s[0xA9..0xAC].copy_from_slice(b"125");
        s[0xAC..0xB1].copy_from_slice(b"10000");
        assert_eq!(tag_times(&s), (Some(125_000), Some(10_000)));
        s[0xA9..0xB1].copy_from_slice(&[0x5A, 0, 0, 0x10, 0x27, 0, 0, 0xFF]);
        assert_eq!(tag_times(&s), (Some(90_000), Some(10_000)));
    }
}
