//! tri-Ace's songs: an SLZ-compressed sequence followed straight by its own instrument set.
//! The u32 in front of the `SLZ` header is the packed sequence's size, so the set starts at
//! `SLZ - 4 + size` and its first u32 is its own size. Each song is scanned on just those
//! bytes, so it takes its own instruments and no neighbouring bank. Games keep them in one
//! large archive; it is searched a piece at a time.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::Path;

use super::disc_files::{self, DiscFile, DiscView};
use super::disc_seq::{seq_pcm, timing, MAX_SONG_MS, PLAY_RATE};
use super::rom_audio::{self, classify, FoundTrack, PcmStream, Shape, Thresholds, TrackLocation};
use super::vgmtrans;

/// Stored as `disc-triace@<view>@<body ms>@<fade ms>@<hex offset>@<hex length>@<file>`.
pub const ENGINE: &str = "disc-triace";
const FORMATS: &str = "TriAcePS1";
const NAME: &str = "song.bin";
/// The scanner's bounds: a sequence is at most this large packed or unpacked.
const MAX_SEQ: u32 = 0x30000;
/// An instrument set is at most this large (samples included).
const MAX_SET: u32 = 0x200000;
/// How much of a large file is searched at a time.
const CHUNK: usize = 16 << 20;

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// The packed sequence's size when an SLZ header passing the scanner's checks starts at
/// `at` (`b` holding at least the 4 bytes before it and its 0x14-byte header).
fn slz(b: &[u8], at: usize) -> Option<u32> {
    if at < 4 || b.get(at..at + 3)? != b"SLZ" || *b.get(at + 3)? > 3 || u16_at(b, at + 0x11)? != 0xFFFF {
        return None;
    }
    let (s1, s2, s3) = (u32_at(b, at + 4)?, u32_at(b, at + 8)?, u32_at(b, at + 12)?);
    if s1 > MAX_SEQ || s2 > MAX_SEQ || s3 > MAX_SEQ || s1 > s2 || s3 > s2 {
        return None;
    }
    let packed = u32_at(b, at - 4)?;
    (packed >= 0x14 && packed <= MAX_SEQ).then_some(packed)
}

/// The instrument set's size when one passing the scanner's checks starts at `b[0]`.
fn set_size(b: &[u8]) -> Option<u32> {
    let size = u32_at(b, 0)?;
    let sect = u16_at(b, 4)? as u32;
    if b[3] != 0 || u32_at(b, 8)? > 0xFF || sect <= 0x20 || sect > 0x4000 || size < 0x1000 || size <= sect || size > MAX_SET {
        return None;
    }
    let s = sect as usize;
    if s + 16 > b.len() {
        return None;
    }
    let zeros = (0..4).all(|k| u32_at(b, s + k * 4) == Some(0));
    (zeros && u32_at(b, s - 4) == Some(0xFFFF_FFFF)).then_some(size)
}

/// The SLZ headers in `b` that pass the checks: (offset, packed size).
pub fn headers(b: &[u8]) -> Vec<(usize, u32)> {
    let mut out = Vec::new();
    let mut at = 4;
    while let Some(k) = b.get(at..).and_then(|r| r.windows(3).position(|w| w == b"SLZ")) {
        let i = at + k;
        if let Some(packed) = slz(b, i) {
            out.push((i, packed));
        }
        at = i + 1;
    }
    out
}

/// A song's bytes in a file: the size word before its SLZ header up to the end of its set.
fn piece(read: &mut dyn FnMut(u64, &mut [u8]) -> bool, slz_at: u64, packed: u32) -> Option<Vec<u8>> {
    let start = slz_at - 4;
    // Enough for the largest instrument section; a set near the end of its file is at least
    // 0x1000 bytes, enough for the sections that fit in it.
    let mut head = vec![0u8; 0x4020];
    if !read(start + u64::from(packed), &mut head) {
        head.truncate(0x1000);
        if !read(start + u64::from(packed), &mut head) {
            return None;
        }
    }
    let size = set_size(&head)?;
    let mut out = vec![0u8; (packed + size) as usize];
    read(start, &mut out).then_some(out)
}

fn digest(b: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    b.hash(&mut h);
    h.finish()
}

/// Every tri-Ace song on the disc. `small`: the files already read whole that hold an SLZ
/// header; files too large for that are searched here a piece at a time.
#[allow(clippy::too_many_arguments)]
pub fn index(view: &DiscView, listing: &[DiscFile], small: Vec<(String, Vec<u8>)>, min_large: u64, n: u32, t: &Thresholds, progress: &mut dyn FnMut(&str), out: &mut Vec<FoundTrack>) {
    let mut seen = HashSet::new();
    for (path, bytes) in &small {
        for (at, packed) in headers(bytes) {
            let mut read = |o: u64, buf: &mut [u8]| {
                let (o, end) = (o as usize, o as usize + buf.len());
                match bytes.get(o..end) {
                    Some(src) => {
                        buf.copy_from_slice(src);
                        true
                    }
                    None => false,
                }
            };
            if let Some(p) = piece(&mut read, at as u64, packed) {
                add(path, at as u64 - 4, p, &mut seen, n, t, out);
            }
        }
    }
    for file in listing.iter().filter(|f| f.size > min_large && !rom_audio::is_video(f.name())) {
        let mut found = Vec::new();
        let mut buf = vec![0u8; CHUNK + 0x20];
        let mut pos = 0u64;
        while pos < file.size {
            let got = match file.read(view, pos, &mut buf) {
                Ok(n) if n > 0 => n,
                _ => break,
            };
            // A header starting in this chunk's last 0x20 bytes is found in the next.
            let limit = if pos + got as u64 >= file.size { got } else { got.saturating_sub(0x20) };
            found.extend(headers(&buf[..got]).into_iter().filter(|&(i, _)| i < limit).map(|(i, p)| (pos + i as u64, p)));
            // The next chunk starts 4 bytes early, for the size word before a header.
            pos += (limit as u64).saturating_sub(4).max(1);
        }
        found.dedup();
        if found.is_empty() {
            continue;
        }
        progress(&file.path);
        for (at, packed) in found {
            let mut read = |o: u64, b: &mut [u8]| file.read(view, o, b).map(|n| n == b.len()).unwrap_or(false);
            if let Some(p) = piece(&mut read, at, packed) {
                add(&file.path, at - 4, p, &mut seen, n, t, out);
            }
        }
    }
}

fn add(path: &str, offset: u64, piece: Vec<u8>, seen: &mut HashSet<u64>, n: u32, t: &Thresholds, out: &mut Vec<FoundTrack>) {
    if !seen.insert(digest(&piece)) {
        return;
    }
    let refs = [(NAME, piece.as_slice())];
    let Some(once) = vgmtrans::songs_in(&refs, FORMATS, 0).ok().and_then(|s| s.into_iter().next()) else { return };
    if once.length_ms == 0 || u64::from(once.length_ms) > MAX_SONG_MS {
        return;
    }
    let twice = vgmtrans::songs_in(&refs, FORMATS, 1).ok().and_then(|s| s.into_iter().next());
    let (body, fade, looping, intro, lp) = timing(&once, twice.as_ref());
    let duration_ms = if looping && lp > 0 { intro + lp } else { body };
    let file = path.rsplit('/').next().unwrap_or(path);
    let name = format!("{} {offset:x}", file.split('.').next().unwrap_or(file));
    let shape = Shape { looping, duration_ms, channels: 2, cd_track: 0, rip: true };
    let rate = u64::from(PLAY_RATE);
    out.push(FoundTrack {
        location: TrackLocation::RomSong { engine: format!("{ENGINE}@{n}@{body}@{fade}@{offset:x}@{:x}@{path}", piece.len()), song: 0 },
        kind: classify(&shape, &format!("{path}/{name}"), t),
        name,
        sample_rate: PLAY_RATE,
        channels: 2,
        duration_ms,
        looping,
        loop_start: intro * rate / 1000,
        loop_end: (intro + lp) * rate / 1000,
        codec: "PlayStation sequence".into(),
        format: once.format.clone(),
        bytes: 0,
        video: false,
    });
}

/// Opens the song stored as `engine` ([`ENGINE`]).
pub fn open(path: &Path, engine: &str, _song: u32) -> Result<Box<dyn PcmStream>, String> {
    let mut f = engine.splitn(7, '@');
    let (_, view, body, fade, offset, length, file) = (f.next(), f.next(), f.next(), f.next(), f.next(), f.next(), f.next());
    let bad = || format!("a damaged disc song record: {engine}");
    let view: usize = view.and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let body: u64 = body.and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let fade: u64 = fade.and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let offset = offset.and_then(|v| u64::from_str_radix(v, 16).ok()).ok_or_else(bad)?;
    let length = length.and_then(|v| usize::from_str_radix(v, 16).ok()).filter(|&l| l <= (MAX_SEQ + MAX_SET) as usize).ok_or_else(bad)?;
    let file = file.ok_or_else(bad)?;
    let rom = rom_audio::open_rom(path)?;
    let disc = rom.data.get(view).ok_or_else(|| format!("the ROM has no data track {}", view + 1))?;
    let listing = disc_files::list(disc)?.files;
    let entry = listing.iter().find(|f| f.path == file).ok_or_else(|| format!("{file} is not on the disc"))?;
    let mut piece = vec![0u8; length];
    if entry.read(disc, offset, &mut piece).map_err(|e| e.to_string())? != length {
        return Err(format!("{file} is shorter than the song needs"));
    }
    let player = vgmtrans::Player::open_in(&[(NAME, piece.as_slice())], FORMATS, 3, 0, PLAY_RATE)?;
    Ok(seq_pcm(player, body, fade))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A packed sequence of `packed` bytes (size word included) and a set of 0x1000 bytes.
    fn song(packed: u32) -> Vec<u8> {
        let mut b = packed.to_le_bytes().to_vec();
        b.extend_from_slice(b"SLZ\x01");
        b.extend_from_slice(&0x100u32.to_le_bytes());
        b.extend_from_slice(&0x200u32.to_le_bytes());
        b.extend_from_slice(&0x100u32.to_le_bytes());
        b.extend_from_slice(&[0, 0xFF, 0xFF]);
        b.resize(packed as usize, 0x55);
        let mut set = vec![0u8; 0x1000];
        set[..4].copy_from_slice(&0x1000u32.to_le_bytes());
        set[4..6].copy_from_slice(&0x40u16.to_le_bytes());
        set[0x3C..0x40].copy_from_slice(&[0xFF; 4]);
        set[0x50..].fill(0x11);
        b.extend_from_slice(&set);
        b
    }

    #[test]
    fn a_song_is_its_sequence_and_the_set_after_it() {
        let mut disc = vec![0u8; 0x20];
        disc.extend(song(0x80));
        disc.extend(vec![0u8; 0x4100]);
        let found = headers(&disc);
        assert_eq!(found, vec![(0x24, 0x80)]);
        let mut read = |o: u64, buf: &mut [u8]| match disc.get(o as usize..o as usize + buf.len()) {
            Some(s) => {
                buf.copy_from_slice(s);
                true
            }
            None => false,
        };
        let p = piece(&mut read, 0x24, 0x80).unwrap();
        assert_eq!(p.len(), 0x80 + 0x1000);
        assert_eq!(&p[4..7], b"SLZ");
    }

    #[test]
    fn a_header_failing_the_checks_is_skipped() {
        let mut b = song(0x80);
        b[4 + 0x11] = 0;
        assert!(headers(&b).is_empty());
    }
}
