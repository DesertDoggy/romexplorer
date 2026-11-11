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

/// A song of a custom engine (Square's, Konami's, ...), recorded by the runs of the disc it
/// is made of: `disc-parts@<view>@<body ms>@<fade ms>@<formats>@<runs>`, each run
/// `<file>|<hex offset>|<hex length>` and `;`-separated, sequence first; `*` as the file is
/// the data track itself (its user data), for music outside the file tree. Playing scans
/// just these runs again.
pub const PARTS_ENGINE: &str = "disc-parts";

/// The custom engines' magic numbers: a file holding one is scanned by them.
const CUSTOM_MAGIC: [&[u8]; 5] = [b"AKAO", b"KDT1", b"smds", b"wds ", b"HOSA"];

/// A data track scanned whole (when no file holds music) is read into memory twice over:
/// larger ones are left alone.
const MAX_TRACK_BYTES: u64 = 800 << 20;

/// Larger files are not looked into: a disc's large files are movies and streamed audio, and
/// a sound archive of this size would be one of those as well.
const MAX_FILE_BYTES: u64 = 64 << 20;
const PLAY_RATE: u32 = 48_000;
const FADE_MS: u64 = 8_000;
/// After a song that does not loop: the release of its last notes.
const RELEASE_MS: u64 = 2_000;
/// Longer than this is data read as a sequence by mistake, not a song.
const MAX_SONG_MS: u64 = 30 * 60 * 1000;
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

fn custom(bytes: &[u8]) -> bool {
    CUSTOM_MAGIC.iter().any(|m| bytes.windows(m.len()).any(|w| w == *m))
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
            // A VAB header without its body in the file: the body is the VB of the same name.
            let mut group = vec![path.clone()];
            group.extend(body_of(path, all));
            out.push(group);
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
            group.extend(body_of(bank, all));
        }
        out.push(group);
    }
    out
}

/// The VB beside a file holding a VAB header: same folder, same name.
fn body_of(path: &str, all: &[String]) -> Option<String> {
    all.iter().find(|p| *p != path && dir(p) == dir(path) && stem(p).eq_ignore_ascii_case(stem(path)) && extension_is(p, "VB")).cloned()
}

/// The files of a group laid end to end, each from a sector boundary, except a VB, which
/// goes straight after the file before it: that one ends with the VAB header the VB is the
/// body of, and the two together are one whole VAB.
fn joined(parts: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, p) in parts {
        if !extension_is(name, "VB") {
            out.resize(out.len().div_ceil(ALIGN) * ALIGN, 0);
        }
        out.extend_from_slice(p);
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
    let mut custom_files: Vec<(String, Vec<u8>)> = Vec::new();
    for file in listing {
        if file.size < 16 || file.size > MAX_FILE_BYTES || rom_audio::is_video(file.name()) {
            continue;
        }
        let Ok(bytes) = read_whole(view, file) else { continue };
        let h = holds(&bytes);
        if custom(&bytes) {
            custom_files.push((file.path.clone(), bytes.clone()));
        }
        if h.seq || h.vab {
            found.push((file.path.clone(), h));
            bytes_of.insert(file.path.clone(), bytes);
        }
    }
    let before = out.len();
    let all: Vec<String> = listing.iter().map(|f| f.path.clone()).collect();
    for group in groups(&found, &all) {
        progress(&group[0]);
        let parts: Vec<(&str, Vec<u8>)> = group
            .iter()
            .filter_map(|p| bytes_of.get(p).cloned().or_else(|| listing.iter().find(|f| &f.path == p).and_then(|f| read_whole(view, f).ok())).map(|b| (p.as_str(), b)))
            .collect();
        if parts.len() != group.len() {
            continue;
        }
        let data = joined(&parts);
        let Ok(once) = vgmtrans::songs(&data, &group[0], vgmtrans::PS1, 0) else { continue };
        let twice = vgmtrans::songs(&data, &group[0], vgmtrans::PS1, 1).unwrap_or_default();
        let files = group.join("|");
        for (i, song) in once.iter().enumerate() {
            if song.length_ms == 0 || u64::from(song.length_ms) > MAX_SONG_MS {
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
    let standard = out.len() - before;
    index_custom(view, listing, n, custom_files, standard == 0, t, progress, out);
}

/// Where a disc's 2048-byte user data sits in its data track: (bytes before each sector's
/// data, bytes from one sector to the next), from where its files' sectors start. `None`:
/// the track is user data already.
pub fn sector_layout(listing: &[DiscFile]) -> Option<(u64, u64)> {
    listing.iter().find_map(|f| match f.extent {
        disc_files::Extent::Sectors { first, stride } => Some((first % stride, stride)),
        _ => None,
    })
}

/// `len` bytes of the track's user data from `offset` on.
pub fn read_user_data(view: &DiscView, layout: Option<(u64, u64)>, offset: u64, len: usize) -> Result<Vec<u8>, String> {
    let mut out = vec![0u8; len];
    match layout {
        None => view.read(offset, &mut out).map_err(|e| e.to_string())?,
        Some((head, stride)) => {
            let mut done = 0usize;
            while done < len {
                let pos = offset + done as u64;
                let (sector, within) = (pos / 2048, pos % 2048);
                let take = ((2048 - within) as usize).min(len - done);
                view.read(sector * stride + head + within, &mut out[done..done + take]).map_err(|e| e.to_string())?;
                done += take;
            }
        }
    }
    Ok(out)
}

/// The size of the track's user data.
pub fn user_data_size(view: &DiscView, layout: Option<(u64, u64)>) -> u64 {
    match layout {
        None => view.size(),
        Some((_, stride)) => view.size() / stride * 2048,
    }
}

fn escape(path: &str) -> String {
    path.replace('%', "%25").replace('|', "%7C").replace(';', "%3B")
}

fn unescape(path: &str) -> String {
    path.replace("%3B", ";").replace("%7C", "|").replace("%25", "%")
}

/// One run of a disc a custom song is made of.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Run {
    /// A file of the disc, or `None` for the data track's user data.
    file: Option<String>,
    offset: u64,
    length: u64,
}

fn runs_text(runs: &[Run]) -> String {
    runs.iter().map(|r| format!("{}|{:x}|{:x}", r.file.as_deref().map(escape).unwrap_or_else(|| "*".into()), r.offset, r.length)).collect::<Vec<_>>().join(";")
}

fn parse_runs(text: &str) -> Option<Vec<Run>> {
    text.split(';')
        .map(|r| {
            let mut f = r.split('|');
            let file = f.next()?;
            let offset = u64::from_str_radix(f.next()?, 16).ok()?;
            let length = u64::from_str_radix(f.next()?, 16).ok()?;
            Some(Run { file: (file != "*").then(|| unescape(file)), offset, length })
        })
        .collect()
}

fn read_runs(view: &DiscView, listing: &[DiscFile], layout: Option<(u64, u64)>, runs: &[Run]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut out = Vec::with_capacity(runs.len());
    for (i, run) in runs.iter().enumerate() {
        let len = usize::try_from(run.length).map_err(|_| "a run too large")?;
        let bytes = match &run.file {
            Some(name) => {
                let file = listing.iter().find(|f| &f.path == name).ok_or_else(|| format!("{name} is not on the disc"))?;
                let mut buf = vec![0u8; len];
                let got = file.read(view, run.offset, &mut buf).map_err(|e| e.to_string())?;
                if got != len {
                    return Err(format!("{name} is shorter than the song needs"));
                }
                buf
            }
            None => read_user_data(view, layout, run.offset, len)?,
        };
        out.push((format!("run{i}.bin"), bytes));
    }
    Ok(out)
}

/// The songs a scan of runs finds, with their loop played once more and twice.
type RunScan = (Vec<vgmtrans::Song>, Vec<vgmtrans::Song>);

fn scan_runs(files: &[(String, Vec<u8>)], formats: &str) -> RunScan {
    let refs: Vec<(&str, &[u8])> = files.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    (vgmtrans::songs_in(&refs, formats, 0).unwrap_or_default(), vgmtrans::songs_in(&refs, formats, 1).unwrap_or_default())
}

/// The song a scan of its own runs finds as the one recorded: its sequence is in the first
/// run, `at` bytes in.
fn song_in(songs: &[vgmtrans::Song], at: u64) -> Option<(u32, &vgmtrans::Song)> {
    songs.iter().enumerate().find(|(_, s)| s.file == 0 && u64::from(s.offset) == at).map(|(i, s)| (i as u32, s))
}

/// A run longer than this is not made by joining a song's pieces of one file.
const MAX_SPAN: u64 = 32 << 20;

/// A song's pieces as runs to read: the pieces in one file joined into one run from the
/// first to the end of the last (formats pair a sequence with instruments in the same file
/// only, and find a bank by what follows it), the sequence's file first. Returns the runs
/// and where the sequence is in the first.
fn spans(pieces: &[Run]) -> (Vec<Run>, u64) {
    let mut out: Vec<Run> = Vec::new();
    for p in pieces {
        match out.iter_mut().find(|r| r.file == p.file) {
            Some(r) => {
                let start = r.offset.min(p.offset);
                let end = (r.offset + r.length).max(p.offset + p.length);
                if end - start > MAX_SPAN {
                    return (pieces.to_vec(), 0);
                }
                r.offset = start;
                r.length = end - start;
            }
            None => out.push(p.clone()),
        }
    }
    let at = pieces[0].offset - out[0].offset;
    (out, at)
}

/// A file up to this size is read whole for each of its songs.
const MAX_WHOLE_FILE: u64 = 64 << 20;

/// A song's pieces as whole files (the sequence's first), when every file is small enough:
/// every song of a bank then has the same runs, so they are read and checked once for all.
/// Returns the runs and where the sequence is in the first.
fn whole_files(pieces: &[Run], listing: &[DiscFile]) -> Option<(Vec<Run>, u64)> {
    let mut out: Vec<Run> = Vec::new();
    for p in pieces {
        let name = p.file.as_ref()?;
        if out.iter().any(|r| r.file.as_ref() == Some(name)) {
            continue;
        }
        let size = listing.iter().find(|f| &f.path == name)?.size;
        if size > MAX_WHOLE_FILE {
            return None;
        }
        out.push(Run { file: Some(name.clone()), offset: 0, length: size });
    }
    Some((out, pieces[0].offset))
}

/// The custom engines' songs: the files holding their magic numbers, scanned together (an
/// engine may keep its sequences and its samples in files of their own); when no file of
/// the disc holds music (`whole`), its data track's user data scanned whole. Each song is
/// recorded by the runs it is made of, checked by scanning just those again.
#[allow(clippy::too_many_arguments)]
fn index_custom(view: &DiscView, listing: &[DiscFile], n: u32, files: Vec<(String, Vec<u8>)>, whole: bool, t: &Thresholds, progress: &mut dyn FnMut(&str), out: &mut Vec<FoundTrack>) {
    let before = out.len();
    if !files.is_empty() {
        index_custom_in(view, listing, n, files, false, t, progress, out);
    }
    if whole && out.len() == before {
        index_custom_in(view, listing, n, Vec::new(), true, t, progress, out);
    }
}

#[allow(clippy::too_many_arguments)]
fn index_custom_in(view: &DiscView, listing: &[DiscFile], n: u32, files: Vec<(String, Vec<u8>)>, whole: bool, t: &Thresholds, progress: &mut dyn FnMut(&str), out: &mut Vec<FoundTrack>) {
    let formats = vgmtrans::PS1_CUSTOM_PLAYABLE;
    let layout = sector_layout(listing);
    let (names, datas, raw): (Vec<Option<String>>, Vec<Vec<u8>>, bool) = if !files.is_empty() {
        let (n, d) = files.into_iter().map(|(p, b)| (Some(p), b)).unzip();
        (n, d, false)
    } else if whole && user_data_size(view, layout) <= MAX_TRACK_BYTES {
        progress("the whole data track");
        match read_user_data(view, layout, 0, user_data_size(view, layout) as usize) {
            Ok(bytes) => (vec![None], vec![bytes], true),
            Err(_) => return,
        }
    } else {
        return;
    };
    let songs = {
        let refs: Vec<(&str, &[u8])> = names.iter().zip(&datas).map(|(n, d)| (n.as_deref().unwrap_or("track.bin"), d.as_slice())).collect();
        match vgmtrans::songs_in(&refs, formats, 0) {
            Ok(s) => s,
            Err(_) => return,
        }
    };
    drop(datas);
    let mut seen = std::collections::HashSet::new();
    // Songs of one bank share their runs (its files whole, or a file's pieces joined):
    // scanned once for all.
    let mut scanned: HashMap<Vec<Run>, Option<RunScan>> = HashMap::new();
    for song in songs {
        // Sony's own SEQ/VAB songs are the standard pass's, except over a whole track.
        if song.format == vgmtrans::PS1 && !raw {
            continue;
        }
        if song.length_ms == 0 || u64::from(song.length_ms) > MAX_SONG_MS || song.parts.is_empty() {
            continue;
        }
        let Some(pieces) = song
            .parts
            .iter()
            .map(|p| p.file.and_then(|f| names.get(f as usize)).map(|name| Run { file: name.clone(), offset: u64::from(p.offset), length: u64::from(p.length) }))
            .collect::<Option<Vec<Run>>>()
        else {
            continue;
        };
        if !seen.insert(pieces.clone()) {
            continue;
        }
        let (runs, at) = whole_files(&pieces, listing).unwrap_or_else(|| spans(&pieces));
        let place = runs[0].file.clone().unwrap_or_else(|| "track".into());
        progress(&place);
        let scan = scanned.entry(runs.clone()).or_insert_with(|| read_runs(view, listing, layout, &runs).ok().map(|read| scan_runs(&read, formats)));
        let Some((once_list, twice_list)) = scan.as_ref() else { continue };
        let Some((index, once)) = song_in(once_list, at) else { continue };
        let twice = song_in(twice_list, at).map(|(_, s)| s);
        let (body, fade, looping, intro, lp) = timing(once, twice);
        let duration_ms = if looping && lp > 0 { intro + lp } else { body };
        let name = if song.name.is_empty() { format!("{} {:x}", stem(&place), runs[0].offset) } else { song.name.clone() };
        let shape = Shape { looping, duration_ms, channels: 2, cd_track: 0, rip: true };
        let rate = u64::from(PLAY_RATE);
        out.push(FoundTrack {
            location: TrackLocation::RomSong { engine: format!("{PARTS_ENGINE}@{n}@{body}@{fade}@{formats}@{}", runs_text(&runs)), song: index },
            kind: classify(&shape, &format!("{place}/{name}"), t),
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

/// Opens song `song` of a custom engine's track stored as `engine` ([`PARTS_ENGINE`]).
pub fn open_parts(path: &Path, engine: &str, song: u32) -> Result<Box<dyn PcmStream>, String> {
    let mut f = engine.splitn(6, '@');
    let (_, view, body, fade, formats, runs) = (f.next(), f.next(), f.next(), f.next(), f.next(), f.next());
    let bad = || format!("a damaged disc song record: {engine}");
    let view: usize = view.and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let body: u64 = body.and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let fade: u64 = fade.and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let formats = formats.ok_or_else(bad)?;
    let runs = runs.and_then(parse_runs).ok_or_else(bad)?;
    let rom = rom_audio::open_rom(path)?;
    let disc = rom.data.get(view).ok_or_else(|| format!("the ROM has no data track {}", view + 1))?;
    let listing = disc_files::list(disc).map(|l| l.files).unwrap_or_default();
    let read = read_runs(disc, &listing, sector_layout(&listing), &runs)?;
    let refs: Vec<(&str, &[u8])> = read.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    let player = vgmtrans::Player::open_in(&refs, formats, 3, song, PLAY_RATE)?;
    let rate = u64::from(PLAY_RATE);
    let tail = if fade > 0 { super::track_timing::FADE_TAIL_MS } else { 0 };
    Ok(Box::new(SeqPcm { player, left: (body + fade + tail) * rate / 1000, fade: fade * rate / 1000, tail: tail * rate / 1000, stop_at_end: fade == 0 }))
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
        read.push((*name, read_whole(disc, file)?));
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
            ("D/AUTUMN.VA".to_string(), h(true, true)),
        ];
        let all: Vec<String> = ["SOUND/BGM01.SEQ", "SOUND/BGM02.SEQ", "SOUND/BGM01.VH", "SOUND/BGM02.VH", "SOUND/BGM01.VB", "SOUND/BGM02.VB", "PACK.BIN", "D/AUTUMN.VA", "D/AUTUMN.VB"].map(String::from).to_vec();
        let g = groups(&found, &all);
        assert_eq!(g, vec![
            vec!["SOUND/BGM01.SEQ".to_string(), "SOUND/BGM01.VH".into(), "SOUND/BGM01.VB".into()],
            vec!["SOUND/BGM02.SEQ".to_string(), "SOUND/BGM02.VH".into(), "SOUND/BGM02.VB".into()],
            vec!["PACK.BIN".to_string()],
            vec!["D/AUTUMN.VA".to_string(), "D/AUTUMN.VB".into()],
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
        let j = joined(&[("A.SEQ", vec![1; 3]), ("A.VH", vec![2; 2049]), ("A.VB", vec![3; 5])]);
        assert_eq!(j.len(), 2048 + 2049 + 5);
        assert_eq!(j[2048], 2);
        assert_eq!(j[2048 + 2049], 3);
    }

    #[test]
    fn pieces_of_one_file_join_into_one_run() {
        let r = |f: Option<&str>, o: u64, l: u64| Run { file: f.map(String::from), offset: o, length: l };
        let (runs, at) = spans(&[r(Some("A"), 0x900, 0x10), r(Some("B"), 0, 0x40), r(Some("A"), 0x100, 0x20), r(Some("A"), 0x2000, 0x100)]);
        assert_eq!(runs, vec![r(Some("A"), 0x100, 0x2000), r(Some("B"), 0, 0x40)]);
        assert_eq!(at, 0x800);
    }

    #[test]
    fn small_files_are_read_whole() {
        let r = |f: Option<&str>, o: u64, l: u64| Run { file: f.map(String::from), offset: o, length: l };
        let file = |p: &str, size: u64| DiscFile { path: p.into(), size, extent: disc_files::Extent::Linear { offset: 0 } };
        let listing = [file("A", 0x3000), file("B", 0x40), file("BIG", MAX_WHOLE_FILE + 1)];
        let (runs, at) = whole_files(&[r(Some("A"), 0x900, 0x10), r(Some("B"), 0, 0x40), r(Some("A"), 0x100, 0x20)], &listing).unwrap();
        assert_eq!(runs, vec![r(Some("A"), 0, 0x3000), r(Some("B"), 0, 0x40)]);
        assert_eq!(at, 0x900);
        assert!(whole_files(&[r(Some("BIG"), 0, 0x10)], &listing).is_none());
        assert!(whole_files(&[r(None, 0, 0x10)], &listing).is_none());
    }

    #[test]
    fn runs_round_trip() {
        let runs = vec![
            Run { file: Some("SOUND/A;B|C%.BIN".into()), offset: 0x1234, length: 0x80 },
            Run { file: None, offset: 0x1_0000_0000, length: 0x10 },
        ];
        let text = runs_text(&runs);
        assert!(!text.contains("A;B"));
        assert_eq!(parse_runs(&text), Some(runs));
    }

    #[test]
    fn a_looping_song_plays_its_loop_twice() {
        let once = vgmtrans::Song { name: String::new(), format: "PS1".into(), file: 0, offset: 0, looping: true, length_ms: 30_000, parts: Vec::new() };
        let twice = vgmtrans::Song { length_ms: 50_000, ..once.clone() };
        assert_eq!(timing(&once, Some(&twice)), (10_000 + 2 * 20_000, FADE_MS, true, 10_000, 20_000));
    }
}
