//! Sequenced music on PlayStation discs, played from the disc's own sequence and instrument
//! files through VGMTrans ([`super::vgmtrans`]): no set, no BIOS, no per-game map.
//!
//! Sony's sound library keeps a song as a SEQ (the notes, magic `pQES`) and a VAB: a header
//! (`pBAV`, the VH) with the instruments, and a body (the VB) with their samples. A game
//! keeps them as files of their own (`BGM01.SEQ` + `BGM01.VH` + `BGM01.VB`, or one `.VAB`),
//! or packed into its own archives and executables. Each song is scanned together with its
//! instruments: a file that holds both is scanned alone, and a SEQ on its own is scanned
//! with the VH and VB that share its name, or failing that the bank of its folder.
//!
//! A track is recorded by the files scanned together and the song's number among what the
//! scan finds, so it plays again from the disc each time.

use std::collections::HashMap;
use std::path::Path;

use super::disc_files::{self, DiscFile, DiscView};
use super::rom_audio::{self, classify, FoundTrack, PcmStream, Shape, Thresholds, TrackLocation};
use super::vgmtrans;

/// The engine name a track is stored under: `disc-seq@<view>@<body ms>@<fade ms>@<files>`,
/// the files `|`-separated in the order they are scanned.
pub const ENGINE: &str = "disc-seq";

/// Larger files are not looked into: a disc's large files are movies and streamed audio, and
/// a sound archive of this size would be one of those as well.
const MAX_FILE_BYTES: u64 = 64 << 20;
const PLAY_RATE: u32 = 48_000;
const FADE_MS: u64 = 8_000;
/// After a song that does not loop: the release of its last notes.
const RELEASE_MS: u64 = 2_000;
/// A loop shorter than this is a held note, not the song coming round again.
const MIN_LOOP_MS: u64 = 1_000;
/// Files scanned together start at sector boundaries, as they would on the disc.
const ALIGN: usize = 2048;

/// What a file holds, as far as the magic numbers inside it tell.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Holds {
    pub seq: bool,
    pub vab: bool,
}

pub fn holds(bytes: &[u8]) -> Holds {
    let has = |magic: &[u8]| bytes.windows(magic.len()).any(|w| w == magic);
    Holds { seq: has(b"pQES"), vab: has(b"pBAV") }
}

fn stem(path: &str) -> &str {
    let name = disc_files::strip_iso_version(path.rsplit('/').next().unwrap_or(path));
    name.rsplit_once('.').map_or(name, |(s, _)| s)
}

fn dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

fn extension_is(path: &str, ext: &str) -> bool {
    let name = disc_files::strip_iso_version(path);
    name.rsplit_once('.').is_some_and(|(_, e)| e.eq_ignore_ascii_case(ext))
}

fn common_prefix(a: &str, b: &str) -> usize {
    a.bytes().zip(b.bytes()).take_while(|(x, y)| x.eq_ignore_ascii_case(y)).count()
}

/// Which files to scan together. `found` is every file that holds a SEQ or a VAB header,
/// `all` every file path on the disc (for the VB bodies, which carry no magic).
pub fn groups(found: &[(String, Holds)], all: &[String]) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for (path, h) in found {
        if !h.seq {
            continue;
        }
        if h.vab {
            out.push(vec![path.clone()]);
            continue;
        }
        let banks: Vec<&String> = found.iter().filter(|(p, h)| h.vab && !h.seq && dir(p) == dir(path)).map(|(p, _)| p).collect();
        let bank = banks
            .iter()
            .find(|p| stem(p).eq_ignore_ascii_case(stem(path)))
            .or_else(|| banks.iter().max_by_key(|p| common_prefix(stem(p), stem(path))))
            .copied();
        let mut group = vec![path.clone()];
        if let Some(bank) = bank {
            group.push(bank.clone());
            let body = all.iter().find(|p| dir(p) == dir(bank) && stem(p).eq_ignore_ascii_case(stem(bank)) && extension_is(p, "VB"));
            if let Some(body) = body {
                group.push(body.clone());
            }
        }
        out.push(group);
    }
    out
}

/// The files of a group laid end to end, each from a sector boundary.
fn joined(parts: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for p in parts {
        out.extend_from_slice(p);
        out.resize(out.len().div_ceil(ALIGN) * ALIGN, 0);
    }
    out
}

pub fn read_whole(view: &DiscView, file: &DiscFile) -> Result<Vec<u8>, String> {
    let mut bytes = vec![0u8; usize::try_from(file.size).map_err(|_| "too large")?];
    let n = file.read(view, 0, &mut bytes).map_err(|e| e.to_string())?;
    bytes.truncate(n);
    Ok(bytes)
}

/// Whether the disc boots as a PlayStation (1) game: its SYSTEM.CNF names a `BOOT`, not the
/// PS2's `BOOT2`.
fn is_ps1(view: &DiscView, listing: &[DiscFile]) -> bool {
    let Some(cnf) = listing.iter().find(|f| disc_files::strip_iso_version(&f.path).eq_ignore_ascii_case("SYSTEM.CNF")) else { return false };
    let Ok(bytes) = read_whole(view, cnf) else { return false };
    let text = String::from_utf8_lossy(&bytes).to_ascii_uppercase();
    text.contains("BOOT") && !text.contains("BOOT2")
}

/// The song timing of one group: (body, fade, looping, intro, loop), in ms.
type Timing = (u64, u64, bool, u64, u64);

fn timing(once: &vgmtrans::Song, twice: Option<&vgmtrans::Song>) -> Timing {
    let len0 = u64::from(once.length_ms);
    if once.looping {
        if let Some(twice) = twice {
            let lp = u64::from(twice.length_ms).saturating_sub(len0);
            if lp >= MIN_LOOP_MS && lp <= len0 {
                let intro = len0 - lp;
                return (intro + 2 * lp, FADE_MS, true, intro, lp);
            }
        }
        return (len0, FADE_MS, true, 0, 0);
    }
    (len0 + RELEASE_MS, 0, false, 0, 0)
}

/// Every sequenced song on data view `view` (number `n`) of a PlayStation disc.
pub fn index(view: &DiscView, listing: &[DiscFile], n: u32, t: &Thresholds, progress: &mut dyn FnMut(&str), out: &mut Vec<FoundTrack>) {
    if !vgmtrans::available() || !is_ps1(view, listing) {
        return;
    }
    let mut found = Vec::new();
    let mut bytes_of: HashMap<String, Vec<u8>> = HashMap::new();
    for file in listing {
        if file.size < 16 || file.size > MAX_FILE_BYTES || rom_audio::is_video(file.name()) {
            continue;
        }
        let Ok(bytes) = read_whole(view, file) else { continue };
        let h = holds(&bytes);
        if h.seq || h.vab {
            found.push((file.path.clone(), h));
            bytes_of.insert(file.path.clone(), bytes);
        }
    }
    let all: Vec<String> = listing.iter().map(|f| f.path.clone()).collect();
    for group in groups(&found, &all) {
        progress(&group[0]);
        let parts: Vec<Vec<u8>> = group
            .iter()
            .filter_map(|p| bytes_of.get(p).cloned().or_else(|| listing.iter().find(|f| &f.path == p).and_then(|f| read_whole(view, f).ok())))
            .collect();
        if parts.len() != group.len() {
            continue;
        }
        let data = joined(&parts);
        let Ok(once) = vgmtrans::songs(&data, &group[0], vgmtrans::PS1, 0) else { continue };
        let twice = vgmtrans::songs(&data, &group[0], vgmtrans::PS1, 1).unwrap_or_default();
        let files = group.join("|");
        for (i, song) in once.iter().enumerate() {
            if song.length_ms == 0 {
                continue;
            }
            let (body, fade, looping, intro, lp) = timing(song, twice.get(i).filter(|s| s.offset == song.offset));
            let duration_ms = if looping && lp > 0 { intro + lp } else { body };
            let name = if once.len() == 1 { stem(&group[0]).to_string() } else { format!("{} #{}", stem(&group[0]), i + 1) };
            let shape = Shape { looping, duration_ms, channels: 2, cd_track: 0, rip: true };
            let rate = u64::from(PLAY_RATE);
            out.push(FoundTrack {
                location: TrackLocation::RomSong { engine: format!("{ENGINE}@{n}@{body}@{fade}@{files}"), song: i as u32 },
                kind: classify(&shape, &format!("{}/{name}", group[0]), t),
                name,
                sample_rate: PLAY_RATE,
                channels: 2,
                duration_ms,
                looping,
                loop_start: intro * rate / 1000,
                loop_end: (intro + lp) * rate / 1000,
                codec: "PlayStation sequence".into(),
                format: "SEQ/VAB".into(),
                bytes: 0,
                video: false,
            });
        }
    }
}

/// Every sequenced song on the disc at `path`, over all its data views.
pub fn index_disc(path: &Path, t: &Thresholds, progress: &mut dyn FnMut(&str)) -> Result<Vec<FoundTrack>, String> {
    let rom = rom_audio::open_rom(path)?;
    let mut out = Vec::new();
    for (n, view) in rom.data.iter().enumerate() {
        let Ok(listing) = disc_files::list(view) else { continue };
        index(view, &listing.files, n as u32, t, progress, &mut out);
    }
    Ok(out)
}

/// A song, played for `left` more frames, the last `fade` of them fading out.
struct SeqPcm {
    player: vgmtrans::Player,
    left: u64,
    fade: u64,
    tail: u64,
    stop_at_end: bool,
}

impl PcmStream for SeqPcm {
    fn sample_rate(&self) -> u32 {
        self.player.sample_rate()
    }
    fn channels(&self) -> u16 {
        2
    }
    fn total_samples(&self) -> Option<u64> {
        Some(self.left)
    }
    fn next_chunk(&mut self) -> Result<Option<Vec<i16>>, String> {
        if self.left == 0 || (self.stop_at_end && self.player.ended()) {
            return Ok(None);
        }
        let frames = self.left.min(4096) as usize;
        let mut buf = vec![0i16; frames * 2];
        self.player.render(&mut buf);
        if self.fade > 0 {
            for (i, pair) in buf.chunks_exact_mut(2).enumerate() {
                let g = super::track_timing::fade_gain(self.left - i as u64, self.fade, self.tail);
                if g < 1.0 {
                    for s in pair {
                        *s = (f32::from(*s) * g) as i16;
                    }
                }
            }
        }
        self.left -= frames as u64;
        Ok(Some(buf))
    }
}

/// Opens song `song` of the track stored as `engine` on the disc at `path`.
pub fn open(path: &Path, engine: &str, song: u32) -> Result<Box<dyn PcmStream>, String> {
    let mut parts = engine.splitn(5, '@');
    let (_, view, body, fade, files) = (parts.next(), parts.next(), parts.next(), parts.next(), parts.next());
    let bad = || format!("a damaged disc song record: {engine}");
    let view: usize = view.and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let body: u64 = body.and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let fade: u64 = fade.and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let files: Vec<&str> = files.ok_or_else(bad)?.split('|').collect();
    let rom = rom_audio::open_rom(path)?;
    let disc = rom.data.get(view).ok_or_else(|| format!("the ROM has no data track {}", view + 1))?;
    let listing = disc_files::list(disc)?.files;
    let mut read = Vec::with_capacity(files.len());
    for name in &files {
        let file = listing.iter().find(|f| f.path == *name).ok_or_else(|| format!("{name} is not on the disc"))?;
        read.push(read_whole(disc, file)?);
    }
    let data = joined(&read);
    // Two more times through the loop than the body plays, so the fade has music under it.
    let player = vgmtrans::Player::open(&data, files[0], vgmtrans::PS1, 3, song, PLAY_RATE)?;
    let rate = u64::from(PLAY_RATE);
    let tail = if fade > 0 { super::track_timing::FADE_TAIL_MS } else { 0 };
    Ok(Box::new(SeqPcm { player, left: (body + fade + tail) * rate / 1000, fade: fade * rate / 1000, tail: tail * rate / 1000, stop_at_end: fade == 0 }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(seq: bool, vab: bool) -> Holds {
        Holds { seq, vab }
    }

    #[test]
    fn a_seq_goes_with_the_bank_of_its_own_name() {
        let found = vec![
            ("SOUND/BGM01.SEQ".to_string(), h(true, false)),
            ("SOUND/BGM02.SEQ".to_string(), h(true, false)),
            ("SOUND/BGM01.VH".to_string(), h(false, true)),
            ("SOUND/BGM02.VH".to_string(), h(false, true)),
            ("PACK.BIN".to_string(), h(true, true)),
        ];
        let all: Vec<String> = ["SOUND/BGM01.SEQ", "SOUND/BGM02.SEQ", "SOUND/BGM01.VH", "SOUND/BGM02.VH", "SOUND/BGM01.VB", "SOUND/BGM02.VB", "PACK.BIN"].map(String::from).to_vec();
        let g = groups(&found, &all);
        assert_eq!(g, vec![
            vec!["SOUND/BGM01.SEQ".to_string(), "SOUND/BGM01.VH".into(), "SOUND/BGM01.VB".into()],
            vec!["SOUND/BGM02.SEQ".to_string(), "SOUND/BGM02.VH".into(), "SOUND/BGM02.VB".into()],
            vec!["PACK.BIN".to_string()],
        ]);
    }

    #[test]
    fn a_seq_without_a_bank_of_its_name_takes_its_folders() {
        let found = vec![("MUS/TITLE.SEQ;1".to_string(), h(true, false)), ("MUS/MUSIC.VAB;1".to_string(), h(false, true))];
        let all: Vec<String> = found.iter().map(|(p, _)| p.clone()).collect();
        assert_eq!(groups(&found, &all), vec![vec!["MUS/TITLE.SEQ;1".to_string(), "MUS/MUSIC.VAB;1".into()]]);
    }

    #[test]
    fn files_are_joined_on_sector_boundaries() {
        let j = joined(&[vec![1; 3], vec![2; 2049]]);
        assert_eq!(j.len(), 2048 * 3);
        assert_eq!(j[2048], 2);
    }

    #[test]
    fn a_looping_song_plays_its_loop_twice() {
        let once = vgmtrans::Song { name: String::new(), format: "PS1".into(), file: 0, offset: 0, looping: true, length_ms: 30_000 };
        let twice = vgmtrans::Song { length_ms: 50_000, ..once.clone() };
        assert_eq!(timing(&once, Some(&twice)), (10_000 + 2 * 20_000, FADE_MS, true, 10_000, 20_000));
    }
}
