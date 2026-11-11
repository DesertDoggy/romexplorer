//! Square's AKAO songs that only play over the game's sound memory as the console lays it
//! out: their sequences carry no samples of their own and take them from the instrument
//! banks the game loads at fixed addresses (`INSTR.ALL` and `INSTR.DAT`). Each file holding
//! sequences (LZS-compressed or not) is placed in such an image beside the two banks, and the
//! image is scanned as one file.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::Path;

use super::disc_files::{self, DiscFile, DiscView};
use super::disc_seq::{read_whole, seq_pcm, timing, MAX_SONG_MS, PLAY_RATE};
use super::rom_audio::{self, classify, FoundTrack, PcmStream, Shape, Thresholds, TrackLocation};
use super::vgmtrans;

/// Stored as `disc-akao-image@<view>@<body ms>@<fade ms>@<file>`, the song its number in
/// the file's image.
pub const ENGINE: &str = "disc-akao-image";

const FORMATS: &str = "Akao";
/// Where VGMTrans's AKAO scanner looks for the two banks in an image of sound memory.
const INSTR_ALL_AT: usize = 0xe0000;
const INSTR_DAT_AT: usize = 0x156000;
/// The smallest image it looks in.
const IMAGE_SIZE: usize = 0x1A8000;
const NAME: &str = "sound.bin";

/// FF7's LZS: a little-endian u32 with the compressed length, then groups of a flag byte
/// (bits from the lowest, 1 = a literal byte) and 8 items; a reference is 2 bytes, a 12-bit
/// position in a 4 KB window (starting at 0xFEE) and a 4-bit length + 3. `None` when the
/// length does not match the file's size (allowing for sector padding).
pub fn lzs(data: &[u8]) -> Option<Vec<u8>> {
    let len = u32::from_le_bytes(data.get(..4)?.try_into().ok()?) as usize;
    if len == 0 || len + 4 > data.len() || data.len() - (len + 4) >= 2048 {
        return None;
    }
    let src = &data[4..4 + len];
    let mut window = [0u8; 4096];
    let mut pos = 0xFEEusize;
    let mut out = Vec::with_capacity(len * 2);
    let mut i = 0;
    while i < src.len() {
        let flags = src[i];
        i += 1;
        for bit in 0..8 {
            if i >= src.len() {
                break;
            }
            if flags >> bit & 1 == 1 {
                let b = src[i];
                i += 1;
                out.push(b);
                window[pos] = b;
                pos = (pos + 1) & 0xFFF;
            } else {
                let (b1, b2) = (src[i] as usize, *src.get(i + 1)? as usize);
                i += 2;
                let from = b1 | (b2 & 0xF0) << 4;
                for k in 0..(b2 & 0x0F) + 3 {
                    let b = window[(from + k) & 0xFFF];
                    out.push(b);
                    window[pos] = b;
                    pos = (pos + 1) & 0xFFF;
                }
            }
            if out.len() > 16 << 20 {
                return None;
            }
        }
    }
    Some(out)
}

fn named<'a>(listing: &'a [DiscFile], name: &str) -> Option<&'a DiscFile> {
    listing.iter().find(|f| disc_files::strip_iso_version(f.name()).eq_ignore_ascii_case(name))
}

/// The two instrument banks, when the disc has them.
fn banks(view: &DiscView, listing: &[DiscFile]) -> Option<(Vec<u8>, Vec<u8>)> {
    let all = read_whole(view, named(listing, "INSTR.ALL")?).ok()?;
    let dat = read_whole(view, named(listing, "INSTR.DAT")?).ok()?;
    (all.len() <= INSTR_DAT_AT - INSTR_ALL_AT).then_some((all, dat))
}

/// A file's sequences laid out with the banks: the file (decompressed) from 0.
fn image(file: &[u8], all: &[u8], dat: &[u8]) -> Option<Vec<u8>> {
    if file.len() > INSTR_ALL_AT {
        return None;
    }
    let mut out = vec![0u8; IMAGE_SIZE.max(INSTR_DAT_AT + dat.len())];
    out[..file.len()].copy_from_slice(file);
    out[INSTR_ALL_AT..INSTR_ALL_AT + all.len()].copy_from_slice(all);
    out[INSTR_DAT_AT..INSTR_DAT_AT + dat.len()].copy_from_slice(dat);
    Some(out)
}

fn contents(bytes: Vec<u8>) -> Vec<u8> {
    lzs(&bytes).filter(|d| has_akao(d)).unwrap_or(bytes)
}

fn has_akao(b: &[u8]) -> bool {
    b.windows(4).any(|w| w == b"AKAO")
}

/// The bytes of the sequence at `at`: its 16-byte header and the length it gives.
fn sequence(b: &[u8], at: usize) -> &[u8] {
    let len = b.get(at + 6..at + 8).map(|l| u16::from_le_bytes([l[0], l[1]]) as usize).unwrap_or(0);
    &b[at..(at + 16 + len).min(b.len())]
}

fn digest(b: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    b.hash(&mut h);
    h.finish()
}

/// Every song of the disc's files holding AKAO sequences, played over its banks. Songs seen
/// in an earlier file (the same sequence bytes) are left out.
pub fn index(view: &DiscView, listing: &[DiscFile], n: u32, t: &Thresholds, progress: &mut dyn FnMut(&str), out: &mut Vec<FoundTrack>) {
    let Some((all, dat)) = banks(view, listing) else { return };
    let mut seen: HashSet<u64> = HashSet::new();
    for file in listing {
        if file.size < 16 || file.size > INSTR_ALL_AT as u64 {
            continue;
        }
        let Ok(raw) = read_whole(view, file) else { continue };
        let bytes = contents(raw);
        if !has_akao(&bytes) {
            continue;
        }
        let Some(img) = image(&bytes, &all, &dat) else { continue };
        progress(&file.path);
        let refs = [(NAME, img.as_slice())];
        let Ok(once) = vgmtrans::songs_in(&refs, FORMATS, 0) else { continue };
        let twice = vgmtrans::songs_in(&refs, FORMATS, 1).unwrap_or_default();
        for (i, song) in once.iter().enumerate() {
            let at = song.offset as usize;
            if song.file != 0 || at >= bytes.len() || song.length_ms == 0 || u64::from(song.length_ms) > MAX_SONG_MS {
                continue;
            }
            if !seen.insert(digest(sequence(&bytes, at))) {
                continue;
            }
            let (body, fade, looping, intro, lp) = timing(song, twice.iter().find(|s| s.file == 0 && s.offset == song.offset));
            let duration_ms = if looping && lp > 0 { intro + lp } else { body };
            let stem = file.name().split('.').next().unwrap_or(file.name());
            let name = format!("{stem} {:x}", at);
            let shape = Shape { looping, duration_ms, channels: 2, cd_track: 0, rip: true };
            let rate = u64::from(PLAY_RATE);
            out.push(FoundTrack {
                location: TrackLocation::RomSong { engine: format!("{ENGINE}@{n}@{body}@{fade}@{}", file.path), song: i as u32 },
                kind: classify(&shape, &format!("{}/{name}", file.path), t),
                name,
                sample_rate: PLAY_RATE,
                channels: 2,
                duration_ms,
                looping,
                loop_start: intro * rate / 1000,
                loop_end: (intro + lp) * rate / 1000,
                codec: "PlayStation sequence".into(),
                format: song.format.clone(),
                bytes: 0,
                video: false,
            });
        }
    }
}

/// Opens song `song` of the track stored as `engine` ([`ENGINE`]).
pub fn open(path: &Path, engine: &str, song: u32) -> Result<Box<dyn PcmStream>, String> {
    let mut f = engine.splitn(5, '@');
    let (_, view, body, fade, file) = (f.next(), f.next(), f.next(), f.next(), f.next());
    let bad = || format!("a damaged disc song record: {engine}");
    let view: usize = view.and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let body: u64 = body.and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let fade: u64 = fade.and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let file = file.ok_or_else(bad)?;
    let rom = rom_audio::open_rom(path)?;
    let disc = rom.data.get(view).ok_or_else(|| format!("the ROM has no data track {}", view + 1))?;
    let listing = disc_files::list(disc)?.files;
    let (all, dat) = banks(disc, &listing).ok_or("the disc has no instrument banks")?;
    let entry = listing.iter().find(|f| f.path == file).ok_or_else(|| format!("{file} is not on the disc"))?;
    let img = image(&contents(read_whole(disc, entry)?), &all, &dat).ok_or("the file is too large for sound memory")?;
    let player = vgmtrans::Player::open_in(&[(NAME, img.as_slice())], FORMATS, 3, song, PLAY_RATE)?;
    Ok(seq_pcm(player, body, fade))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lzs_literals_and_references() {
        // "ABC" as literals, then a reference to it (position 0xFEE, length 3), twice over.
        let body = [0b0000_0111u8, b'A', b'B', b'C', 0xEE, 0xF0, 0xEE, 0xF1];
        let mut data = (body.len() as u32).to_le_bytes().to_vec();
        data.extend_from_slice(&body);
        assert_eq!(lzs(&data).unwrap(), b"ABCABCABCA");
    }

    #[test]
    fn lzs_rejects_a_mismatched_length() {
        assert!(lzs(&[0xFF, 0, 0, 0, 1, 2]).is_none());
    }

    #[test]
    fn image_places_the_banks() {
        let img = image(b"AKAO", &[1; 4], &[2; 4]).unwrap();
        assert_eq!(img.len(), IMAGE_SIZE);
        assert_eq!(&img[..4], b"AKAO");
        assert_eq!(img[INSTR_ALL_AT], 1);
        assert_eq!(img[INSTR_DAT_AT + 3], 2);
    }
}
