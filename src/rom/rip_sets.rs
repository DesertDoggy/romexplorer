//! Music rip sets kept in a game's own folder (NSF, SPC, GBS, VGM/VGZ, HES, KSS, AY, SAP and
//! GYM, loose or in a zip / 7z as the music sites ship them), indexed and played through
//! [`super::gme`]. These are the music of cartridge games whose ROMs hold sequences rather
//! than recordings: the rip is the song data cut out of the ROM with its sound driver.
//!
//! A track's place is the file's path relative to the game folder, `<archive>|<member>` for
//! one inside a zip or 7z, and its track number in that file. A NEZplug-style `.m3u` beside a
//! multi-track file (in the same folder or archive) gives the tracks their titles and lengths.
//! A track with no length of its own, or a playlist length of a minute or more, is listened
//! to once ([`super::track_timing`]): it ends where it falls silent, a track that repeats
//! plays its loop twice and fades, and one that does neither is faded at [`RUN_ON_MS`].

use std::path::{Path, PathBuf};

use super::gme::{self, Emu};
use super::track_timing::{self, Listener, Timing};
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

/// The lines of `m3u` files that name `file` (by its base name, any case), in file order,
/// numbered from 1 ([`counts_from_zero`]). Whether the numbers count from 0 is decided over all of
/// `m3us`: a set's playlists are written one way, and one for a file whose first track has
/// no line of its own shows no track 0 by itself.
fn playlist_for(file: &str, m3us: &[(String, Vec<u8>)]) -> Vec<u8> {
    let from_zero = m3us.iter().any(|(_, text)| counts_from_zero(m3u_text(text).as_bytes()));
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
    if from_zero { moved_up(&out) } else { out }
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

/// The track number field of a playlist line (`file::TYPE,<number>,title,...`): where it is.
fn number_field(line: &[u8]) -> Option<std::ops::Range<usize>> {
    let after = line.windows(2).position(|w| w == b"::")? + 2;
    let start = after + line[after..].iter().position(|&b| b == b',')? + 1;
    let end = start + line[start..].iter().position(|&b| b == b',').unwrap_or(line.len() - start);
    Some(start..end)
}

/// A playlist line's decimal track number.
fn decimal_number(line: &[u8]) -> Option<u32> {
    if line.starts_with(b"#") {
        return None;
    }
    number_field(line).and_then(|r| std::str::from_utf8(&line[r]).ok()?.trim().parse::<u32>().ok())
}

/// gme reads a playlist's decimal track numbers as counting from 1 (as NEZplug defines
/// them), but many sets' playlists count from 0: every title then lands on the track before
/// it, and the first plays nothing. A playlist with a track 0 is one of those.
/// (`$`-prefixed hex numbers count from 0 either way.)
fn counts_from_zero(playlist: &[u8]) -> bool {
    playlist.split(|&b| b == b'\n').any(|line| decimal_number(line) == Some(0))
}

/// `playlist` with its decimal track numbers moved up by one.
fn moved_up(playlist: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(playlist.len() + 16);
    for (i, line) in playlist.split(|&b| b == b'\n').enumerate() {
        if i > 0 {
            out.push(b'\n');
        }
        match (decimal_number(line), number_field(line)) {
            (Some(n), Some(r)) => {
                out.extend_from_slice(&line[..r.start]);
                out.extend_from_slice((n + 1).to_string().as_bytes());
                out.extend_from_slice(&line[r.end..]);
            }
            _ => out.extend_from_slice(line),
        }
    }
    out
}

/// How a track is played: `body` ms, then a `fade`; `intro`/`lp` where it loops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Plan {
    body: u32,
    fade: u32,
    looping: bool,
    intro: Option<u32>,
    lp: Option<u32>,
}

impl Plan {
    /// The length the index records: one pass through the loop, or the whole track.
    fn duration_ms(&self) -> u32 {
        match (self.intro, self.lp) {
            (Some(i), Some(l)) => i + l,
            _ => self.body,
        }
    }
}

/// A playlist length shorter than this is taken as it is: a jingle has no room to repeat.
const CHECK_LISTED_MIN_MS: u32 = 60_000;
/// How long a track with no length is listened to for a loop: two passes of a long one.
const LISTEN_MS: u32 = 200_000;
/// The fade of a listed length that gives none: a track cut there would stop dead.
const LISTED_FADE_MS: u32 = 2_000;

/// A listed fade, or `fallback` where the listing has none.
fn or_fade(fade: u32, fallback: u32) -> u32 {
    if fade > 0 { fade } else { fallback }
}

/// How track `track` plays. A length of the track's own with an intro and a loop is used as
/// it is. Otherwise the track is played through once, quietly, and its sound decides
/// ([`track_timing`]): it stops where it falls silent, and one that repeats plays its loop
/// twice and fades -- also when a playlist gives a length, which for many sets is a guess
/// that leaves seconds of silence or a short phrase going round. `None` for a silent track.
fn plan(emu: &mut Emu, file: &RipFile, track: u32) -> Result<Option<Plan>, String> {
    let info = emu.track_info(track)?;
    let own = own_length(&info);
    if let Some((body, fade, true)) = own {
        return Ok(Some(Plan { body, fade, looping: true, intro: info.intro_ms, lp: info.loop_ms }));
    }
    if let Some((body, fade, false)) = own.filter(|o| o.0 < CHECK_LISTED_MIN_MS) {
        return Ok(Some(Plan { body, fade: or_fade(fade, LISTED_FADE_MS), looping: false, intro: None, lp: None }));
    }
    let key = (fingerprint(file), track);
    if let Some(known) = PLANS.lock().ok().and_then(|m| m.get(&key).copied()) {
        return Ok(known);
    }
    let limit = own.map_or(LISTEN_MS, |o| o.0);
    let found = match listen(emu, track, limit)? {
        Timing::Silent => None,
        Timing::Ends { ms } => {
            let ms = u32::try_from(ms).unwrap_or(u32::MAX);
            Some(match own {
                // Ends within a second of its listed length: the listing is right.
                Some((body, fade, _)) if ms + 1_000 >= body => Plan { body, fade: or_fade(fade, LISTED_FADE_MS), looping: false, intro: None, lp: None },
                _ => Plan { body: ms, fade: 0, looping: false, intro: None, lp: None },
            })
        }
        // A loop that only roughly matches does not overrule a listed length.
        Timing::Loops { sure: false, .. } if own.is_some() => {
            let (body, fade, _) = own.unwrap_or_default();
            Some(Plan { body, fade: or_fade(fade, LOOP_FADE_MS), looping: false, intro: None, lp: None })
        }
        Timing::Loops { start_ms, loop_ms, .. } => {
            let (intro, lp) = (u32::try_from(start_ms).unwrap_or(u32::MAX), u32::try_from(loop_ms).unwrap_or(u32::MAX));
            let body = intro.saturating_add(lp.saturating_mul(2)).min(limit);
            Some(match own {
                // Two loops would take about as long as listed anyway; or the listing goes
                // round a long loop only a few times, which is a choice, not a padded guess.
                Some((len, fade, _)) if body + 5_000 >= len || intro.saturating_add(lp.saturating_mul(4)) >= len => Plan { body: len, fade: or_fade(fade, LOOP_FADE_MS), looping: false, intro: None, lp: None },
                Some((_, fade, _)) => Plan { body, fade: or_fade(fade, LOOP_FADE_MS), looping: true, intro: Some(intro), lp: Some(lp) },
                None => Plan { body, fade: LOOP_FADE_MS, looping: true, intro: Some(intro), lp: Some(lp) },
            })
        }
        Timing::RunsOn => Some(match own {
            // Still playing at its listed length: faded there, not cut.
            Some((body, fade, _)) => Plan { body, fade: or_fade(fade, LOOP_FADE_MS), looping: false, intro: None, lp: None },
            // Not heard to loop, so not called one: it is faded where it is cut.
            None => Plan { body: RUN_ON_MS, fade: LOOP_FADE_MS, looping: false, intro: None, lp: None },
        }),
    };
    if let Ok(mut m) = PLANS.lock() {
        if m.len() > 8_192 {
            m.clear();
        }
        m.insert(key, found);
    }
    Ok(found)
}

/// Plans already worked out, by file content and track: the index finds them, and playing
/// the track then needs no second listen.
static PLANS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<(u64, u32), Option<Plan>>>> = std::sync::LazyLock::new(Default::default);

/// The file and the playlist read with it, which numbers its tracks.
fn fingerprint(file: &RipFile) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    file.data.hash(&mut h);
    file.playlist.hash(&mut h);
    h.finish()
}

/// Plays `track` through, unheard, for up to `limit_ms`, and says what its sound did.
fn listen(emu: &mut Emu, track: u32, limit_ms: u32) -> Result<Timing, String> {
    // Silence is judged here, over a longer stretch than the player's own detection.
    emu.ignore_silence(true);
    emu.start(track, None)?;
    let mut ear = Listener::new(emu.sample_rate());
    let mut buf = vec![0i16; 8192];
    while !emu.ended() && ear.played_ms() < u64::from(limit_ms) && !ear.fell_silent() {
        emu.play(&mut buf)?;
        ear.feed(&buf);
    }
    Ok(ear.finish(emu.ended()))
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
            let plan = match plan(&mut emu, &file, track) {
                Ok(Some(plan)) => plan,
                Ok(None) => continue,
                Err(e) => {
                    log::debug!("{} #{track}: {e}", file.rel);
                    continue;
                }
            };
            let (duration_ms, looping) = (plan.duration_ms(), plan.looping);
            if duration_ms == 0 {
                continue;
            }
            let name = track_name(&file, &info, track, count);
            let shape = Shape { looping, duration_ms: u64::from(duration_ms), channels: Emu::CHANNELS, cd_track: 0, rip: true };
            let rate = u64::from(Emu::SAMPLE_RATE);
            let (loop_start, loop_end) = match (plan.intro, plan.lp) {
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
    /// Before the last `tail` of them, this many fade out ([`track_timing::fade_gain`]).
    /// Done here and not by gme, whose fade some sets' tracks ended before reaching.
    fade: u64,
    tail: u64,
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
            if self.fade > 0 {
                for (i, pair) in buf.chunks_exact_mut(2).enumerate() {
                    let g = track_timing::fade_gain(l.saturating_sub(2 * i as u64), self.fade, self.tail);
                    if g < 1.0 {
                        pair[0] = (f32::from(pair[0]) * g) as i16;
                        pair[1] = (f32::from(pair[1]) * g) as i16;
                    }
                }
            }
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
    let Plan { body, fade, .. } = plan(&mut emu, &file, track)?.ok_or("the track is silent")?;
    // The plan says where it ends; a quiet passage before that is not the end.
    emu.ignore_silence(true);
    emu.start(track, None)?;
    let samples = |ms: u32| u64::from(ms) * u64::from(emu.sample_rate()) / 1000 * 2;
    let tail = if fade > 0 { track_timing::FADE_TAIL_MS as u32 } else { 0 };
    Ok(Box::new(GmePcm { left: Some(samples(body + fade + tail)), fade: samples(fade), tail: samples(tail), emu }))
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
    fn a_playlist_counting_from_zero_is_moved_up_by_one() {
        let zero = b"# x\nGame.gbs::GBS,0,Prologue,1:00\nGame.gbs::GBS,12,Town\nGame.gbs::GBS,$0C,Hex\n".to_vec();
        let m3us = vec![("a.m3u".to_string(), zero)];
        assert_eq!(playlist_for("Game.gbs", &m3us), b"Game.gbs::GBS,1,Prologue,1:00\nGame.gbs::GBS,13,Town\nGame.gbs::GBS,$0C,Hex\n");
        let one = vec![("b.m3u".to_string(), b"Game.nsf::NSF,1,Title\nGame.nsf::NSF,2,Field\n".to_vec())];
        assert_eq!(playlist_for("Game.nsf", &one), b"Game.nsf::NSF,1,Title\nGame.nsf::NSF,2,Field\n");
    }

    #[test]
    fn a_set_counting_from_zero_moves_up_every_file_of_it() {
        // The second file's playlist has no track 0 of its own; the set's first one does.
        let m3us = vec![
            ("main.m3u".to_string(), b"Main.gbs::GBS,0,Title\nMain.gbs::GBS,1,Field\n".to_vec()),
            ("other.m3u".to_string(), b"Other.gbs::GBS,59,Song\n".to_vec()),
        ];
        assert_eq!(playlist_for("Other.gbs", &m3us), b"Other.gbs::GBS,60,Song\n");
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
