//! Music rip sets kept in a game's own folder (NSF, SPC, GBS, VGM/VGZ, HES, KSS, AY, SAP and
//! GYM, loose or in a zip / 7z as the music sites ship them), indexed and played through
//! [`super::gme`]. These are the music of cartridge games whose ROMs hold sequences rather
//! than recordings: the rip is the song data cut out of the ROM with its sound driver.
//!
//! A track's place is the file's path relative to the game folder, `<archive>|<member>` for
//! one inside a zip or 7z, and its track number in that file. A NEZplug-style `.m3u` beside a
//! multi-track file (in the same folder or archive) gives the tracks their titles and lengths.
//! A track with no length of its own is played until it falls silent: one that is still
//! playing after [`RUN_ON_MS`] loops, and is faded there.

use std::path::{Path, PathBuf};

use super::gme::{self, Emu};
use super::rom_audio::{classify, FoundTrack, PcmStream, Shape, SoundKind, Thresholds, TrackLocation};
use super::sevenzip_wrapper::{ArchiveFormat, SevenZipArchive};

/// The `data_track` a rip-set track is stored with: no disc has this many data tracks.
pub const RIP_VIEW: u32 = u16::MAX as u32;
/// Rip-set files are kilobytes; anything over this is something else.
const MAX_FILE_BYTES: u64 = 16 << 20;
/// How deep below the game folder rip sets are looked for.
const MAX_DEPTH: usize = 3;
/// A file with more tracks than this is cut off there.
const MAX_TRACKS: u32 = 256;
/// A track with no length that is still playing after this long runs on: it loops.
pub const RUN_ON_MS: u32 = 150_000;
/// How a looping track with no fade of its own is faded out.
const LOOP_FADE_MS: u32 = 8_000;
/// Below this a sample counts as silence when a track's own end is measured.
const SILENCE: i32 = 64;

fn ext_of(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

fn archive_format(name: &str) -> Option<ArchiveFormat> {
    match ext_of(name).as_str() {
        "zip" => Some(ArchiveFormat::Zip),
        "7z" => Some(ArchiveFormat::SevenZ),
        _ => None,
    }
}

fn base_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// One music file of a rip set, read whole, with the playlists that sit beside it.
struct RipFile {
    /// `dir/file.nsf`, or `dir/set.zip|inner/file.nsf`.
    rel: String,
    data: Vec<u8>,
    /// The `.m3u` lines that name this file.
    playlist: Vec<u8>,
}

/// A playlist's text as UTF-8. Rip sets' playlists are often not: Shift-JIS for Japanese
/// sets, Windows-1252 (a `©` byte) for the rest.
fn m3u_text(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_string();
    }
    let (sjis, _, bad) = encoding_rs::SHIFT_JIS.decode(bytes);
    if !bad && sjis.chars().any(|c| ('\u{3040}'..='\u{30ff}').contains(&c) || ('\u{4e00}'..='\u{9fff}').contains(&c)) {
        return sjis.into_owned();
    }
    encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned()
}

/// The lines of `m3u` files that name `file` (by its base name, any case), in file order.
fn playlist_for(file: &str, m3us: &[(String, Vec<u8>)]) -> Vec<u8> {
    let want = base_name(file).to_ascii_lowercase();
    let mut out = Vec::new();
    for (_, text) in m3us {
        for line in m3u_text(text).lines() {
            let line = line.trim_start_matches('\u{feff}').trim();
            let Some((name, _)) = line.split_once("::") else { continue };
            if line.starts_with('#') || base_name(name.trim()).to_ascii_lowercase() != want {
                continue;
            }
            out.extend_from_slice(line.as_bytes());
            out.push(b'\n');
        }
    }
    out
}

/// Groups music files with the playlists found next to them (same folder, or same archive).
fn with_playlists(music: Vec<(String, Vec<u8>)>, m3us: &[(String, String, Vec<u8>)]) -> Vec<RipFile> {
    music
        .into_iter()
        .map(|(rel, data)| {
            let group = group_of(&rel);
            let beside: Vec<(String, Vec<u8>)> = m3us.iter().filter(|(g, _, _)| *g == group).map(|(_, n, t)| (n.clone(), t.clone())).collect();
            let playlist = playlist_for(rel.rsplit('|').next().unwrap_or(&rel), &beside);
            RipFile { rel, data, playlist }
        })
        .collect()
}

/// What a file is grouped with for playlists: its archive, or its folder.
fn group_of(rel: &str) -> String {
    match rel.split_once('|') {
        Some((archive, _)) => archive.to_string(),
        None => rel.rsplit_once('/').map_or("", |(dir, _)| dir).to_string(),
    }
}

fn walk(folder: &Path) -> Vec<(String, PathBuf, u64)> {
    let mut out = Vec::new();
    let mut stack = vec![(folder.to_path_buf(), String::new(), 0usize)];
    while let Some((dir, prefix, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() && depth < MAX_DEPTH {
                stack.push((entry.path(), rel, depth + 1));
            } else if kind.is_file() {
                out.push((rel, entry.path(), entry.metadata().map(|m| m.len()).unwrap_or(0)));
            }
        }
        if out.len() > 20_000 {
            break;
        }
    }
    out.sort();
    out
}

/// Every rip-set music file under `folder`, read. Archives that hold none (a zipped ROM) are
/// only listed.
fn collect(folder: &Path) -> Vec<RipFile> {
    let mut music = Vec::new();
    let mut m3us: Vec<(String, String, Vec<u8>)> = Vec::new();
    for (rel, path, size) in walk(folder) {
        if gme::known_extension(&rel) && size <= MAX_FILE_BYTES {
            if let Ok(data) = std::fs::read(&path) {
                music.push((rel, data));
            }
        } else if ext_of(&rel) == "m3u" && size <= 1 << 20 {
            if let Ok(text) = std::fs::read(&path) {
                m3us.push((group_of(&rel), rel, text));
            }
        } else if let Some(format) = archive_format(&rel) {
            let Ok(archive) = SevenZipArchive::open(&path, format, |_, _| true) else { continue };
            for i in 0..archive.entry_count() {
                let Ok(entry) = archive.entry(i) else { continue };
                if entry.is_dir || entry.size > MAX_FILE_BYTES {
                    continue;
                }
                let is_m3u = ext_of(&entry.path) == "m3u";
                if !gme::known_extension(&entry.path) && !is_m3u {
                    continue;
                }
                let Ok(data) = archive.read_entry(i) else { continue };
                if is_m3u {
                    m3us.push((rel.clone(), format!("{rel}|{}", entry.path), data));
                } else {
                    music.push((format!("{rel}|{}", entry.path), data));
                }
            }
        }
    }
    with_playlists(music, &m3us)
}

// --- rip libraries ----------------------------------------------------------------------

/// Whether `name` is a file a rip set is made of: music gme plays, or a playlist.
fn is_rip_member(name: &str) -> bool {
    gme::known_extension(name) || ext_of(name) == "m3u"
}

/// The sets in a rip library folder: each zip or 7z holding rip files, and each folder
/// holding loose ones. Sorted, so a run is repeatable.
pub fn sets_in(library: &Path) -> Vec<PathBuf> {
    let mut sets = std::collections::BTreeSet::new();
    let mut stack = vec![(library.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            let name = entry.file_name().to_string_lossy().into_owned();
            if kind.is_dir() && depth < 4 {
                stack.push((entry.path(), depth + 1));
            } else if kind.is_file() {
                if archive_format(&name).is_some() {
                    sets.insert(entry.path());
                } else if gme::known_extension(&name) {
                    sets.insert(dir.clone());
                }
            }
        }
    }
    sets.into_iter().collect()
}

/// A set's files, read, placed by absolute path (`<archive>|<member>` inside a zip or 7z).
/// An archive with no rip files in it (a zipped ROM) gives nothing.
fn collect_set(set: &Path) -> Vec<RipFile> {
    let mut music = Vec::new();
    let mut m3us: Vec<(String, String, Vec<u8>)> = Vec::new();
    let base = set.to_string_lossy().into_owned();
    if let Some(format) = archive_format(&base) {
        let Ok(archive) = SevenZipArchive::open(set, format, |_, _| true) else { return Vec::new() };
        let wanted: Vec<i32> = (0..archive.entry_count()).filter(|&i| archive.entry(i).is_ok_and(|e| !e.is_dir && e.size <= MAX_FILE_BYTES && is_rip_member(&e.path))).collect();
        if !wanted.iter().any(|&i| archive.entry(i).is_ok_and(|e| gme::known_extension(&e.path))) {
            return Vec::new();
        }
        for i in wanted {
            let (Ok(entry), Ok(data)) = (archive.entry(i), archive.read_entry(i)) else { continue };
            let rel = format!("{base}|{}", entry.path);
            if ext_of(&entry.path) == "m3u" {
                m3us.push((String::new(), rel, data));
            } else {
                music.push((rel, data));
            }
        }
    } else {
        let Ok(entries) = std::fs::read_dir(set) else { return Vec::new() };
        let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
        files.sort();
        for path in files {
            let name = path.to_string_lossy().into_owned();
            let size = path.metadata().map(|m| m.len()).unwrap_or(u64::MAX);
            if !is_rip_member(&name) || size > MAX_FILE_BYTES {
                continue;
            }
            let Ok(data) = std::fs::read(&path) else { continue };
            if ext_of(&name) == "m3u" {
                m3us.push((String::new(), name, data));
            } else {
                music.push((name, data));
            }
        }
    }
    // One set is one group: its playlists may name any of its files.
    music
        .into_iter()
        .map(|(rel, data)| {
            let beside: Vec<(String, Vec<u8>)> = m3us.iter().map(|(_, n, t)| (n.clone(), t.clone())).collect();
            let playlist = playlist_for(rel.rsplit('|').next().unwrap_or(&rel), &beside);
            RipFile { rel, data, playlist }
        })
        .collect()
}

/// A set's fingerprint for matching it to ROMs by content; `None` when its files carry no
/// ROM bytes (VGM logs and the like).
pub fn set_fingerprint(set: &Path) -> Option<super::rip_match::Fingerprint> {
    let files = collect_set(set);
    super::rip_match::fingerprint(files.iter().map(|f| f.data.as_slice()))
}

/// Reads one rip-set file again, by its place, with its playlist.
fn read_one(folder: &Path, rel: &str) -> Result<RipFile, String> {
    let (data, m3us) = match rel.split_once('|') {
        None => {
            let data = std::fs::read(folder.join(rel)).map_err(|e| format!("{rel}: {e}"))?;
            let dir = folder.join(rel).parent().map(Path::to_path_buf).unwrap_or_else(|| folder.to_path_buf());
            let m3us = std::fs::read_dir(&dir)
                .map(|entries| {
                    let mut list: Vec<(String, Vec<u8>)> = entries
                        .flatten()
                        .filter(|e| ext_of(&e.file_name().to_string_lossy()) == "m3u")
                        .filter_map(|e| Some((e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).ok()?)))
                        .collect();
                    list.sort();
                    list
                })
                .unwrap_or_default();
            (data, m3us)
        }
        Some((archive_rel, member)) => {
            let format = archive_format(archive_rel).ok_or_else(|| format!("{archive_rel} is not a zip or 7z"))?;
            let archive = SevenZipArchive::open(&folder.join(archive_rel), format, |_, _| true).map_err(|e| format!("{archive_rel}: {e}"))?;
            let mut data = None;
            let mut m3us = Vec::new();
            for i in 0..archive.entry_count() {
                let Ok(entry) = archive.entry(i) else { continue };
                if entry.path == member {
                    data = Some(archive.read_entry(i).map_err(|e| format!("{rel}: {e}"))?);
                } else if ext_of(&entry.path) == "m3u" && !entry.is_dir && entry.size <= 1 << 20 {
                    if let Ok(text) = archive.read_entry(i) {
                        m3us.push((entry.path, text));
                    }
                }
            }
            m3us.sort();
            (data.ok_or_else(|| format!("{archive_rel} has no {member}"))?, m3us)
        }
    };
    let playlist = playlist_for(rel.rsplit('|').next().unwrap_or(rel), &m3us);
    Ok(RipFile { rel: rel.to_string(), data, playlist })
}

fn load(file: &RipFile) -> Result<Emu, String> {
    let mut emu = Emu::open(file.data.clone(), Emu::SAMPLE_RATE)?;
    if !file.playlist.is_empty() {
        // A playlist gme cannot use leaves the file's own tracks, which still play.
        if let Err(e) = emu.load_m3u(&file.playlist) {
            log::debug!("{}: playlist not used: {e}", file.rel);
        }
    }
    Ok(emu)
}

/// How long a track with no length of its own runs: played until gme hears it end (it falls
/// silent) or [`RUN_ON_MS`] passes. `(milliseconds to its last sound, still playing)`.
fn measure(emu: &mut Emu, track: u32) -> Result<(u32, bool), String> {
    emu.start(track, None)?;
    let rate = u64::from(emu.sample_rate());
    let cap = u64::from(RUN_ON_MS) * rate / 1000 * 2;
    let mut buf = vec![0i16; 8192];
    let (mut done, mut last_sound) = (0u64, 0u64);
    while !emu.ended() && done < cap {
        emu.play(&mut buf)?;
        if let Some(i) = buf.iter().rposition(|&s| i32::from(s).abs() > SILENCE) {
            last_sound = done + i as u64 + 1;
        }
        done += buf.len() as u64;
    }
    Ok(((last_sound / 2 * 1000 / rate) as u32, done >= cap && !emu.ended()))
}

/// What a track's own data says about how to play it: `(length before any fade, fade,
/// looping)`, or `None` when it gives no length.
fn own_length(info: &gme::TrackInfo) -> Option<(u32, u32, bool)> {
    if let (Some(intro), Some(lp)) = (info.intro_ms, info.loop_ms.filter(|&l| l > 0)) {
        return Some((intro + 2 * lp, info.fade_ms.filter(|&f| f > 0).unwrap_or(LOOP_FADE_MS), true));
    }
    info.length_ms.map(|len| (len, info.fade_ms.unwrap_or(0), false))
}

/// A track title from a playlist. Some sites write `<title> - <artist> - <game> - ©<date>
/// <company>` there; only the title is the track's name.
fn song_title(song: &str) -> &str {
    let parts: Vec<&str> = song.split(" - ").collect();
    if parts.len() >= 3 {
        parts[0].trim()
    } else {
        song.trim()
    }
}

fn track_name(file: &RipFile, info: &gme::TrackInfo, track: u32, count: u32) -> String {
    let song = song_title(&info.song);
    if !song.is_empty() {
        return song.to_string();
    }
    // The file's own title (an NSF's header) before its file name, which is often long.
    let file_name = base_name(file.rel.rsplit('|').next().unwrap_or(&file.rel));
    let stem = if info.game.is_empty() { file_name.rsplit_once('.').map_or(file_name, |(s, _)| s) } else { info.game.as_str() };
    if count > 1 {
        format!("{stem} #{}", track + 1)
    } else {
        stem.to_string()
    }
}

/// Track titles that name a jingle however short it is: rip sets title their tracks, and a
/// two-second "Stage Clear" is a jingle, not a sound effect.
const JINGLE_TITLES: [&str; 12] = ["game over", "stage clear", "level clear", "area clear", "round clear", "course clear", "get ready", "victory", "fanfare", "jingle", "you win", "game clear"];
/// Shorter than this is a click even with a jingle's title.
const TITLED_JINGLE_MIN_MS: u32 = 400;

fn titled_jingle(name: &str) -> bool {
    let name = name.to_lowercase();
    JINGLE_TITLES.iter().any(|t| name.contains(t))
}

/// Every track of every rip set under `folder`. `progress` is told which file is being read.
pub fn index(folder: &Path, t: &Thresholds, progress: impl FnMut(&str)) -> Vec<FoundTrack> {
    if !gme::available() {
        return Vec::new();
    }
    index_files(collect(folder), t, progress)
}

/// Every track of one set of a rip library ([`sets_in`]), placed by absolute path.
pub fn index_set(set: &Path, t: &Thresholds, progress: impl FnMut(&str)) -> Vec<FoundTrack> {
    if !gme::available() {
        return Vec::new();
    }
    index_files(collect_set(set), t, progress)
}

fn index_files(files: Vec<RipFile>, t: &Thresholds, mut progress: impl FnMut(&str)) -> Vec<FoundTrack> {
    let mut out = Vec::new();
    for file in files {
        progress(&file.rel);
        let mut emu = match load(&file) {
            Ok(emu) => emu,
            Err(e) => {
                log::debug!("{}: {e}", file.rel);
                continue;
            }
        };
        let count = emu.track_count().min(MAX_TRACKS);
        let codec = gme::identify(&gme::gunzip(&file.data).unwrap_or_else(|| file.data.clone())).unwrap_or_else(|| ext_of(&file.rel).to_ascii_uppercase());
        for track in 0..count {
            let Ok(info) = emu.track_info(track) else { continue };
            let (duration_ms, looping) = match own_length(&info) {
                Some((len, _, looping)) => (if looping { info.intro_ms.unwrap_or(0) + info.loop_ms.unwrap_or(0) } else { len }, looping),
                None => match measure(&mut emu, track) {
                    Ok(m) => m,
                    Err(e) => {
                        log::debug!("{} #{track}: {e}", file.rel);
                        continue;
                    }
                },
            };
            if duration_ms == 0 {
                continue;
            }
            let name = track_name(&file, &info, track, count);
            let shape = Shape { looping, duration_ms: u64::from(duration_ms), channels: Emu::CHANNELS, cd_track: 0, rip: true };
            let rate = u64::from(Emu::SAMPLE_RATE);
            let (loop_start, loop_end) = match (info.intro_ms, info.loop_ms) {
                (Some(i), Some(l)) if l > 0 => (u64::from(i) * rate / 1000, u64::from(i + l) * rate / 1000),
                _ => (0, 0),
            };
            let mut kind = classify(&shape, &format!("{}/{name}", file.rel), t);
            if kind == SoundKind::Se && duration_ms >= TITLED_JINGLE_MIN_MS && titled_jingle(&name) {
                kind = SoundKind::Jingle;
            }
            out.push(FoundTrack {
                kind,
                location: TrackLocation::Rip { path: file.rel.clone(), track },
                name,
                sample_rate: Emu::SAMPLE_RATE,
                channels: Emu::CHANNELS,
                duration_ms: u64::from(duration_ms),
                looping,
                loop_start,
                loop_end,
                codec: codec.clone(),
                format: info.system.clone(),
                bytes: file.data.len() as u64,
                video: false,
            });
        }
    }
    out
}

/// A rip-set track, rendered as it plays.
struct GmePcm {
    emu: Emu,
    /// Samples (both channels) left to give, when the length is known.
    left: Option<u64>,
}

impl PcmStream for GmePcm {
    fn sample_rate(&self) -> u32 {
        self.emu.sample_rate()
    }
    fn channels(&self) -> u16 {
        Emu::CHANNELS
    }
    fn total_samples(&self) -> Option<u64> {
        self.left.map(|l| l / 2)
    }
    fn next_chunk(&mut self) -> Result<Option<Vec<i16>>, String> {
        if self.emu.ended() || self.left == Some(0) {
            return Ok(None);
        }
        let take = self.left.map_or(8192, |l| l.min(8192)) as usize & !1;
        let mut buf = vec![0i16; take.max(2)];
        self.emu.play(&mut buf)?;
        if let Some(l) = &mut self.left {
            *l = l.saturating_sub(buf.len() as u64);
        }
        Ok(Some(buf))
    }
}

/// Opens track `track` of the rip-set file at `rel` under the game folder `folder` for
/// playing: to its own length and fade, its loop twice and faded, or until it falls silent
/// (a track that runs on is faded at [`RUN_ON_MS`]).
/// A library set's `rel` is absolute, and `folder` is then not used.
pub fn open(folder: &Path, rel: &str, track: u32) -> Result<Box<dyn PcmStream>, String> {
    if !gme::available() {
        return Err("the chip music player (gme) is not built for this machine".into());
    }
    let file = read_one(folder, rel)?;
    let mut emu = load(&file)?;
    let info = emu.track_info(track)?;
    let (body, fade) = match own_length(&info) {
        Some((len, fade, _)) => (len, fade),
        None => (RUN_ON_MS, LOOP_FADE_MS),
    };
    emu.start(track, Some((body, fade)))?;
    let left = u64::from(body + fade) * u64::from(emu.sample_rate()) / 1000 * 2;
    Ok(Box::new(GmePcm { emu, left: Some(left) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_playlist_line_is_kept_only_for_the_file_it_names() {
        let m3us = vec![
            ("a.m3u".to_string(), b"# comment\nGame.nsf::NSF,1,Title,2:00,,10\nOther.nsf::NSF,1,Nope\n".to_vec()),
            ("b.m3u".to_string(), b"sub/GAME.NSF::NSF,3,Ending,1:30\n".to_vec()),
        ];
        let got = String::from_utf8(playlist_for("music/Game.nsf", &m3us)).unwrap();
        assert_eq!(got, "Game.nsf::NSF,1,Title,2:00,,10\nsub/GAME.NSF::NSF,3,Ending,1:30\n");
    }

    #[test]
    fn a_playlist_title_keeps_only_the_title() {
        assert_eq!(song_title("BGM #01 - ? - Some Game - \u{a9}1991-08-02 Company"), "BGM #01");
        assert_eq!(song_title("Boss - Remix"), "Boss - Remix");
        assert_eq!(song_title("  Title  "), "Title");
    }

    #[test]
    fn a_playlist_not_in_utf8_is_decoded() {
        assert_eq!(m3u_text(b"a.gbs::GBS,1,Song - x - y - \xa91991"), "a.gbs::GBS,1,Song - x - y - \u{a9}1991");
        let (sjis, _, _) = encoding_rs::SHIFT_JIS.encode("a.nsf::NSF,1,タイトル");
        assert_eq!(m3u_text(&sjis), "a.nsf::NSF,1,タイトル");
    }

    #[test]
    fn short_tracks_titled_as_jingles_are_jingles() {
        assert!(titled_jingle("Stage Clear"));
        assert!(titled_jingle("GAME OVER"));
        assert!(!titled_jingle("Overworld"));
    }

    #[test]
    fn files_group_by_archive_or_folder() {
        assert_eq!(group_of("set.zip|inner/a.nsf"), "set.zip");
        assert_eq!(group_of("music/a.spc"), "music");
        assert_eq!(group_of("a.spc"), "");
    }

    #[test]
    fn an_own_length_beats_measuring() {
        let looped = gme::TrackInfo { intro_ms: Some(2000), loop_ms: Some(3000), ..Default::default() };
        assert_eq!(own_length(&looped), Some((8000, LOOP_FADE_MS, true)));
        let timed = gme::TrackInfo { length_ms: Some(90_000), fade_ms: Some(5000), ..Default::default() };
        assert_eq!(own_length(&timed), Some((90_000, 5000, false)));
        assert_eq!(own_length(&gme::TrackInfo::default()), None);
    }
}
