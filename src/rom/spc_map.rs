//! A SNES game's music map, from its matched `.spc` set and the ROM: the SNES counterpart of
//! `nes_nsf`'s map. An `.spc` is the sound chip's whole state while a track plays: its 64 KB of
//! sound memory (driver, samples, the song), the DSP's registers and the sound CPU's. Where
//! that memory sits in the ROM as is, a map keeps where (ROM ranges to sound-memory addresses)
//! and the registers' values: numbers only. The track then plays from the user's ROM, with no
//! boot, by rebuilding the memory from it.

use std::collections::HashSet;

use super::music_map::{self, Run};
use super::music_search::{self, Write};
use super::snes_apu::{Apu, FRAME_CLOCKS, SPC_FILE_SIZE};

/// Where the 64 KB of sound memory, the DSP registers and the 64 bytes behind the boot ROM sit
/// in an `.spc`.
const ARAM: usize = 0x100;
const DSP: usize = 0x10100;
const EXTRA: usize = 0x101C0;

pub fn is_spc(spc: &[u8]) -> bool {
    spc.len() >= DSP + 128 && spc.starts_with(b"SNES-SPC700 Sound File Data")
}

/// The sound-memory bytes a track needs: all but the echo buffer (rewritten as it plays),
/// when echo writes are on.
pub fn needed(spc: &[u8]) -> Vec<bool> {
    let dsp = &spc[DSP..DSP + 128];
    let mut need = vec![true; 0x10000];
    if dsp[0x6C] & 0x20 == 0 {
        let start = (dsp[0x6D] as usize) << 8;
        let len = ((dsp[0x7D] & 0x0F) as usize * 2048).max(4);
        for i in 0..len {
            need[(start + i) & 0xFFFF] = false;
        }
    }
    need
}

/// How the gaps between runs are filled when the memory is rebuilt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill<'a> {
    Zeros,
    /// From the ROM, between two runs at the same distance.
    Bridged,
    /// From the `.spc` itself: what the runs alone would give with every gap right.
    File(&'a [u8]),
}

/// One track's map: where its sound memory is in the ROM, and the registers' values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub runs: Vec<Run>,
    /// The sound CPU's registers (PC, A, X, Y, PSW, SP), the DSP's 128, the four ports as the
    /// driver last saw them.
    pub pc: u16,
    pub regs: [u8; 5],
    pub dsp: [u8; 128],
    pub ports: [u8; 4],
    /// The I/O registers $F0-$FF (timers, control) as the snapshot has them.
    pub io: [u8; 16],
}

/// The share of a track's needed sound memory in its runs.
pub fn coverage(spc: &[u8], runs: &[Run]) -> (usize, usize) {
    let need = needed(spc);
    let mut placed = vec![false; 0x10000];
    runs.iter().for_each(|r| placed[r.data..r.data + r.len].iter_mut().for_each(|b| *b = true));
    let total = need.iter().filter(|&&n| n).count();
    let got = need.iter().zip(&placed).filter(|(&n, &p)| n && p).count();
    (got, total)
}

/// Track `spc`'s map over `rom` (the ROM image, without a copier header).
pub fn track(spc: &[u8], rom: &[u8]) -> Option<Track> {
    tracks(&[spc], rom).pop().flatten()
}

/// The maps of a set's tracks over `rom`, laid out together (the ROM is indexed once).
pub fn tracks(spcs: &[&[u8]], rom: &[u8]) -> Vec<Option<Track>> {
    let ok: Vec<&[u8]> = spcs.iter().copied().filter(|s| is_spc(s)).collect();
    let mut data = Vec::with_capacity(ok.len() * 0x10000);
    let mut need = Vec::with_capacity(ok.len() * 0x10000);
    for s in &ok {
        data.extend_from_slice(&s[ARAM..ARAM + 0x10000]);
        need.extend(needed(s));
    }
    let layout = music_map::layout(&data, &need, rom);
    let mut runs: Vec<Vec<Run>> = vec![Vec::new(); ok.len()];
    for r in &layout.runs {
        // A run can cross from one track's memory into the next's: cut it.
        let mut at = r.data;
        while at < r.data + r.len {
            let i = at / 0x10000;
            let end = ((i + 1) * 0x10000).min(r.data + r.len);
            if end - at >= 16 {
                runs[i].push(Run { data: at - i * 0x10000, len: end - at, rom: r.rom + (at - r.data) });
            }
            at = end;
        }
    }
    let mut runs = runs.into_iter();
    spcs.iter()
        .map(|s| {
            if !is_spc(s) {
                return None;
            }
            Some(track_of(s, runs.next()?))
        })
        .collect()
}

fn track_of(spc: &[u8], runs: Vec<Run>) -> Track {
    let aram = &spc[ARAM..ARAM + 0x10000];
    let mut dsp = [0u8; 128];
    dsp.copy_from_slice(&spc[DSP..DSP + 128]);
    let mut io = [0u8; 16];
    io.copy_from_slice(&aram[0xF0..0x100]);
    Track {
        runs,
        pc: u16::from_le_bytes([spc[0x25], spc[0x26]]),
        regs: [spc[0x27], spc[0x28], spc[0x29], spc[0x2A], spc[0x2B]],
        dsp,
        ports: [aram[0xF4], aram[0xF5], aram[0xF6], aram[0xF7]],
        io,
    }
}

/// The `.spc` a track's map rebuilds from the ROM.
pub fn rebuilt(t: &Track, rom: &[u8], fill: Fill) -> Vec<u8> {
    let mut spc = vec![0u8; SPC_FILE_SIZE];
    spc[..33].copy_from_slice(b"SNES-SPC700 Sound File Data v0.30");
    spc[0x21] = 26;
    spc[0x22] = 26;
    spc[0x23] = 27; // no ID666 tag
    spc[0x24] = 30;
    spc[0x25..0x27].copy_from_slice(&t.pc.to_le_bytes());
    spc[0x27..0x2C].copy_from_slice(&t.regs);
    let mut aram = match fill {
        Fill::File(s) => s[ARAM..ARAM + 0x10000].to_vec(),
        _ => vec![0u8; 0x10000],
    };
    for r in &t.runs {
        if let Some(src) = rom.get(r.rom..r.rom + r.len) {
            aram[r.data..r.data + r.len].copy_from_slice(src);
        }
    }
    if fill == Fill::Bridged {
        for w in t.runs.windows(2) {
            let (a, b) = (w[0], w[1]);
            if b.data > a.data + a.len && b.rom as isize - b.data as isize == a.rom as isize - a.data as isize {
                if let Some(src) = rom.get(a.rom + a.len..b.rom) {
                    aram[a.data + a.len..b.data].copy_from_slice(src);
                }
            }
        }
    }
    aram[0xF0..0x100].copy_from_slice(&t.io);
    spc[ARAM..ARAM + 0x10000].copy_from_slice(&aram);
    spc[DSP..DSP + 128].copy_from_slice(&t.dsp);
    // The 64 bytes under the boot ROM: the memory's own, as the chip shows them when the boot
    // ROM is switched off.
    spc[EXTRA..EXTRA + 64].copy_from_slice(&aram[0xFFC0..]);
    spc
}

/// Each 1/60 s frame's pitch of every voice playing, from the DSP's registers, as writes
/// [`melody`] reads.
pub fn frames(spc: &[u8], count: usize) -> Option<Vec<Vec<Write>>> {
    frames_of(&mut Apu::from_spc(spc).ok()?, count)
}

/// The melody a booted game's sound chip plays (`boot`, its `.spc` snapshot) once `value` is
/// written to `port`, as the game's main CPU would to start a song.
pub fn probe(boot: &[u8], port: usize, value: u8, count: usize) -> HashSet<u64> {
    let Ok(mut apu) = Apu::from_spc(boot) else { return HashSet::new() };
    apu.write_port(port, value);
    // A frame for the driver to take the command in before listening.
    apu.advance(FRAME_CLOCKS * 4);
    frames_of(&mut apu, count).map(|f| melody(&f)).unwrap_or_default()
}

fn frames_of(apu: &mut Apu, count: usize) -> Option<Vec<Vec<Write>>> {
    let mut out = Vec::with_capacity(count);
    let mut last = [u16::MAX; 8];
    for _ in 0..count {
        apu.advance(FRAME_CLOCKS);
        let state = apu.save_spc();
        out.push(dsp_frame(state.get(DSP..DSP + 128)?, &mut last));
    }
    Some(out)
}

/// A frame's writes, as [`melody`] reads them, from the DSP's registers (`dsp`, 128 bytes) at
/// its end: each voice's pitch where it changed since `last`.
pub fn dsp_frame(dsp: &[u8], last: &mut [u16; 8]) -> Vec<Write> {
    let mut f = Vec::new();
    for v in 0..8 {
        // A voice sounds while its envelope is above zero.
        let pitch = u16::from_le_bytes([dsp[v * 16 + 2], dsp[v * 16 + 3] & 0x3F]);
        let env = dsp[v * 16 + 8];
        let p = if env == 0 { 0 } else { pitch };
        if p != last[v] {
            last[v] = p;
            // 14-bit pitch, kept to 11 bits for the fingerprint.
            let q = p >> 3;
            f.push(Write { at: 0, port: 0x2000 + v as u32 * 2, value: q & 0xFF });
            f.push(Write { at: 0, port: 0x2001 + v as u32 * 2, value: q >> 8 });
        }
    }
    f
}

/// Where the DSP's registers are in an `.spc`.
pub const SPC_DSP: usize = DSP;

pub fn melody(frames: &[Vec<Write>]) -> HashSet<u64> {
    let pairs: Vec<(u32, u32)> = (0..8).map(|v| (0x2000 + v * 2, 0x2001 + v * 2)).collect();
    music_search::melody(frames, &pairs)
}

/// How alike the track a map rebuilds plays to the `.spc` (None when the `.spc` has no
/// melody in `count` frames).
pub fn check(spc: &[u8], t: &Track, rom: &[u8], fill: Fill, count: usize) -> Option<f64> {
    let real = melody(&frames(spc, count)?);
    if real.len() < music_search::MIN_MELODY {
        return None;
    }
    let mine = frames(&rebuilt(t, rom, fill), count).map(|f| melody(&f)).unwrap_or_default();
    Some(music_search::alike(&real, &mine))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_track_rebuilds_its_memory_from_the_rom() {
        // A "ROM" holding two stretches of a made-up sound memory, apart.
        let mut x = 7u32;
        let mut aram = vec![0u8; 0x10000];
        for b in aram.iter_mut() {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            *b = (x >> 16) as u8;
        }
        let mut spc = vec![0u8; SPC_FILE_SIZE];
        spc[..27].copy_from_slice(b"SNES-SPC700 Sound File Data");
        spc[ARAM..ARAM + 0x10000].copy_from_slice(&aram);
        // Echo off: the whole memory is needed.
        spc[DSP + 0x6C] = 0x20;
        let mut rom = vec![0xFFu8; 0x40000];
        rom[0x1000..0x9000].copy_from_slice(&aram[0x0200..0x8200]);
        rom[0x20000..0x27000].copy_from_slice(&aram[0x9000..0x10000]);
        let t = track(&spc, &rom).unwrap();
        let (got, total) = coverage(&spc, &t.runs);
        assert_eq!(total, 0x10000);
        assert!(got >= 0x8000 + 0x7000 - 64, "{got}");
        let back = rebuilt(&t, &rom, Fill::Zeros);
        assert_eq!(&back[ARAM + 0x0300..ARAM + 0x8000], &aram[0x0300..0x8000]);
        assert_eq!(&back[ARAM + 0x9100..ARAM + 0xFF00], &aram[0x9100..0xFF00]);
    }
}
