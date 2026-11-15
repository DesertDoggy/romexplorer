//! PC-98 music from floppy images: the PMD and FMP song files on the disk ([`super::pc98_disk`]),
//! played by 98fmplayer's ports of the two drivers on its YM2608 emulation, built into the
//! `rommusic` library as `rpc98_` (`deps/rommusic/pc98/rpc98.c`).
//!
//! A song's PCM files (PMD's `.PPC`/`.PVI`/`.PZI`, FMP's `.PVI`) are read from the same disk,
//! else from another disk of the same game. The chip's rhythm sounds need its 8 KB ROM, taken
//! from the user's own files when there is one ([`set_drums`]); without it the rhythm part is
//! silent and everything else plays.
//!
//! The drivers count their own loops, so a song is timed by playing it: the first pass ends at
//! the first loop count, the loop is the stretch to the second.

use std::ffi::{c_char, c_void, CStr};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use libloading::Library;

use super::native_lib;
use super::pc98_disk::{self, Disk, DiskFile};
use super::rom_audio::{classify, FoundTrack, PcmStream, Shape, Thresholds, TrackLocation};

pub const ENGINE: &str = "pc98";
/// The rate the player renders at.
pub const PLAY_RATE: u32 = 44_100;
/// The longest a song is played to find its loop.
const MEASURE_CAP_MS: u64 = 600_000;
/// A song that neither loops nor ends inside the cap plays this long.
const RUN_ON_MS: u64 = 150_000;
const FADE_MS: u64 = 8_000;
/// A loop shorter than this is a pulse, not the song's loop.
const MIN_LOOP_MS: u64 = 1_000;
/// The largest floppy image read; a 2HD D88 is about 1.3 MB.
const MAX_IMAGE_BYTES: u64 = 8 << 20;
/// The YM2608 rhythm ROM's size.
pub const DRUM_ROM_BYTES: usize = 0x2000;
/// The names the rhythm ROM goes by in emulator BIOS folders.
pub const DRUM_ROM_NAMES: &[&str] = &["ym2608_adpcm_rom.bin", "2608_rom.bin", "ym2608.rom"];
/// A driver's loop count once the song has stopped.
const ENDED: u32 = 0xff;

type NewFn = unsafe extern "C" fn(*const u8, usize) -> *mut c_void;
type DrumsFn = unsafe extern "C" fn(*mut c_void, *const u8, usize) -> i32;
type PcmNameFn = unsafe extern "C" fn(*mut c_void, i32, *mut c_char, usize, *mut c_char, usize) -> i32;
type PcmLoadFn = unsafe extern "C" fn(*mut c_void, i32, *const c_char, *const u8, usize) -> i32;
type RenderFn = unsafe extern "C" fn(*mut c_void, *mut i16, usize) -> i32;
type StatusFn = unsafe extern "C" fn(*mut c_void, *mut u32, *mut i32, *mut u32);
type CommentFn = unsafe extern "C" fn(*mut c_void, i32) -> *const c_char;
type FreeFn = unsafe extern "C" fn(*mut c_void);

struct Api {
    _lib: Library,
    new: NewFn,
    drums: DrumsFn,
    pcm_name: PcmNameFn,
    pcm_load: PcmLoadFn,
    render: RenderFn,
    status: StatusFn,
    comment: CommentFn,
    free: FreeFn,
}

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| {
        let path = native_lib::rommusic_library()?;
        unsafe {
            let lib = Library::new(&path).ok()?;
            Some(Api {
                new: *lib.get(b"rpc98_new\0").ok()?,
                drums: *lib.get(b"rpc98_set_drums\0").ok()?,
                pcm_name: *lib.get(b"rpc98_pcm_name\0").ok()?,
                pcm_load: *lib.get(b"rpc98_pcm_load\0").ok()?,
                render: *lib.get(b"rpc98_render\0").ok()?,
                status: *lib.get(b"rpc98_status\0").ok()?,
                comment: *lib.get(b"rpc98_comment\0").ok()?,
                free: *lib.get(b"rpc98_free\0").ok()?,
                _lib: lib,
            })
        }
    })
    .as_ref()
}

pub fn available() -> bool {
    api().is_some()
}

static DRUMS: Mutex<Option<Vec<u8>>> = Mutex::new(None);

/// Hands over the YM2608 rhythm ROM; `false` when `image` is not one.
pub fn set_drums(image: &[u8]) -> bool {
    if image.len() != DRUM_ROM_BYTES || image.iter().all(|&b| b == image[0]) {
        return false;
    }
    *DRUMS.lock().unwrap_or_else(|e| e.into_inner()) = Some(image.to_vec());
    true
}

pub fn has_drums() -> bool {
    DRUMS.lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

/// The rhythm ROM when none was handed over: looked for beside the image, then in the app root
/// and its `assets/`.
fn drums_near(path: &Path) -> Option<Vec<u8>> {
    let mut dirs: Vec<std::path::PathBuf> = path.parent().map(Path::to_path_buf).into_iter().collect();
    if let Some(root) = native_lib::exe_root() {
        dirs.push(root.join("assets"));
        dirs.push(root);
    }
    for dir in dirs {
        for name in DRUM_ROM_NAMES {
            if let Ok(image) = std::fs::read(dir.join(name)) {
                if set_drums(&image) {
                    return Some(image);
                }
            }
        }
    }
    None
}

/// Whether `path` names a floppy image (or a zip / 7z, which may hold some).
pub fn is_disk_path(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    pc98_disk::EXTENSIONS.contains(&ext.as_str()) || ext == "zip" || ext == "7z"
}

/// The floppy images at `path`: the file itself, or each image inside a zip / 7z, in archive
/// order.
pub fn disks(path: &Path) -> Result<Vec<Disk>, String> {
    let ext_of = |name: &str| name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    let name = path.to_string_lossy().to_string();
    let ext = ext_of(&name);
    let format = match ext.as_str() {
        "zip" => super::sevenzip_wrapper::ArchiveFormat::Zip,
        "7z" => super::sevenzip_wrapper::ArchiveFormat::SevenZ,
        _ => {
            if std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) > MAX_IMAGE_BYTES {
                return Err(format!("{} is larger than a floppy", path.display()));
            }
            let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
            return Ok(Disk::open(&bytes, &ext).into_iter().collect());
        }
    };
    let archive = super::sevenzip_wrapper::SevenZipArchive::open_variant(path, format, native_lib::LibVariant::Streaming, |_, _| true).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for i in 0..archive.entry_count() {
        let Ok(entry) = archive.entry(i) else { continue };
        let e = ext_of(&entry.path);
        if entry.is_dir || entry.size > MAX_IMAGE_BYTES || !pc98_disk::EXTENSIONS.contains(&e.as_str()) {
            continue;
        }
        let mut bytes = Vec::with_capacity(entry.size as usize);
        archive
            .extract_stream(
                i,
                None,
                |_, _, chunk| {
                    bytes.extend_from_slice(chunk);
                    true
                },
                |_, _| true,
            )
            .map_err(|e| e.to_string())?;
        if let Ok(disk) = Disk::open(&bytes, &e) {
            out.push(disk);
        }
    }
    Ok(out)
}

/// One song being played.
pub struct Player {
    state: *mut c_void,
}

// The state is the library's own heap block, touched only through `&mut self`.
unsafe impl Send for Player {}

impl Drop for Player {
    fn drop(&mut self) {
        if let Some(api) = api() {
            unsafe { (api.free)(self.state) };
        }
    }
}

impl Player {
    /// Song `file` of `disks[disk]`, with its PCM files from the same disk or another one.
    pub fn open(disks: &[Disk], disk: usize, file: &DiskFile) -> Result<Self, String> {
        let api = api().ok_or("the PC-98 music player (rommusic) is not installed")?;
        let song = disks[disk].read(file);
        let state = unsafe { (api.new)(song.as_ptr(), song.len()) };
        if state.is_null() {
            return Err(format!("{} is not a PMD or FMP song", file.path));
        }
        let player = Player { state };
        let drums = DRUMS.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(drums) = drums {
            unsafe { (api.drums)(state, drums.as_ptr(), drums.len()) };
        }
        for slot in 0..3 {
            let (mut name, mut exts) = ([0 as c_char; 64], [0 as c_char; 32]);
            if unsafe { (api.pcm_name)(state, slot, name.as_mut_ptr(), name.len(), exts.as_mut_ptr(), exts.len()) } == 0 {
                continue;
            }
            let name = unsafe { CStr::from_ptr(name.as_ptr()) }.to_string_lossy().trim().to_string();
            let exts = unsafe { CStr::from_ptr(exts.as_ptr()) }.to_string_lossy().to_string();
            player.load_pcm(api, disks, disk, file, slot, &name, &exts);
        }
        Ok(player)
    }

    /// Finds the PCM file `name` (its extension may be left out) and hands it over.
    #[allow(clippy::too_many_arguments)]
    fn load_pcm(&self, api: &Api, disks: &[Disk], disk: usize, beside: &DiskFile, slot: i32, name: &str, exts: &str) {
        let (stem, given) = match name.rsplit_once('.') {
            Some((s, e)) => (s, Some(format!(".{}", e.to_ascii_uppercase()))),
            None => (name, None),
        };
        let tries: Vec<String> = given.into_iter().chain(exts.split('.').filter(|e| !e.is_empty()).map(|e| format!(".{e}"))).collect();
        // This disk first, then the game's other disks.
        let order = std::iter::once(disk).chain((0..disks.len()).filter(|&d| d != disk));
        for d in order {
            for ext in &tries {
                let wanted = format!("{stem}{ext}");
                let found = if d == disk { disks[d].sibling(beside, &wanted) } else { disks[d].files.iter().find(|f| f.path.rsplit('/').next().is_some_and(|n| n.eq_ignore_ascii_case(&wanted))) };
                let Some(found) = found else { continue };
                let bytes = disks[d].read(found);
                let Ok(ext_c) = std::ffi::CString::new(ext.as_str()) else { continue };
                if unsafe { (api.pcm_load)(self.state, slot, ext_c.as_ptr(), bytes.as_ptr(), bytes.len()) } == 0 {
                    return;
                }
            }
        }
    }

    /// Fills `out` (interleaved stereo at [`PLAY_RATE`]).
    pub fn render(&mut self, out: &mut [i16]) {
        if let Some(api) = api() {
            unsafe { (api.render)(self.state, out.as_mut_ptr(), out.len() / 2) };
        }
    }

    /// `(loops so far, stopped)`.
    pub fn status(&self) -> (u32, bool) {
        let Some(api) = api() else { return (0, true) };
        let (mut loops, mut playing, mut ticks) = (0u32, 0i32, 0u32);
        unsafe { (api.status)(self.state, &mut loops, &mut playing, &mut ticks) };
        (loops, playing == 0 || loops == ENDED)
    }

    /// The song's title: PMD's `#Title`, FMP's first comment line.
    pub fn title(&self) -> Option<String> {
        let api = api()?;
        let line = unsafe { (api.comment)(self.state, 0) };
        if line.is_null() {
            return None;
        }
        let raw = unsafe { CStr::from_ptr(line) }.to_bytes();
        let text = clean_text(&encoding_rs::SHIFT_JIS.decode(raw).0);
        (!text.is_empty()).then_some(text)
    }
}

/// A comment line without its escape sequences (colour codes) and control characters.
fn clean_text(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if !c.is_control() {
            out.push(c);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// How a song plays: `(body ms, fade ms, looping, intro ms, loop ms)`.
fn measure(player: &mut Player) -> Option<(u64, u64, bool, u64, u64)> {
    let rate = u64::from(PLAY_RATE);
    let mut buf = vec![0i16; PLAY_RATE as usize / 10 * 2];
    let (mut frames, mut first, mut heard, mut last_sound) = (0u64, None::<u64>, false, 0u64);
    while frames * 1000 / rate < MEASURE_CAP_MS {
        player.render(&mut buf);
        frames += (buf.len() / 2) as u64;
        if buf.iter().any(|&s| s.unsigned_abs() > 64) {
            heard = true;
            last_sound = frames;
        }
        let (loops, stopped) = player.status();
        if stopped {
            return heard.then(|| (last_sound * 1000 / rate + super::track_timing::FADE_TAIL_MS, 0, false, 0, 0));
        }
        match (loops, first) {
            (1.., None) => first = Some(frames),
            (2.., Some(at)) => {
                let lp = (frames - at) * 1000 / rate;
                let intro = (at * 1000 / rate).saturating_sub(lp);
                if !heard {
                    return None;
                }
                if lp < MIN_LOOP_MS {
                    return Some((RUN_ON_MS, FADE_MS, true, 0, 0));
                }
                return Some((intro + 2 * lp, FADE_MS, true, intro, lp));
            }
            _ => {}
        }
    }
    heard.then_some((RUN_ON_MS, FADE_MS, true, 0, 0))
}

/// The songs on the floppy images at `path`, measured and classified.
pub fn index(path: &Path, t: &Thresholds) -> Result<(Vec<FoundTrack>, Option<String>), String> {
    if !available() {
        return Ok((Vec::new(), None));
    }
    if !has_drums() {
        drums_near(path);
    }
    let disks = disks(path)?;
    if disks.is_empty() {
        return Ok((Vec::new(), Some("no PC-98 floppy image with a file system".into())));
    }
    let shown = path.to_string_lossy().to_string();
    let mut seen = std::collections::HashSet::new();
    let mut found = Vec::new();
    for (d, disk) in disks.iter().enumerate() {
        for file in disk.music_files() {
            let bytes = disk.read(file);
            let mut h = std::collections::hash_map::DefaultHasher::new();
            bytes.hash(&mut h);
            if !seen.insert(h.finish()) {
                continue;
            }
            let Ok(mut player) = Player::open(&disks, d, file) else { continue };
            let title = player.title();
            let Some(timing) = measure(&mut player) else { continue };
            found.push(track(d, file, title, &shown, t, timing));
        }
    }
    let why = found.is_empty().then(|| "no PMD or FMP songs on the floppy images".to_string());
    Ok((found, why))
}

fn track(disk: usize, file: &DiskFile, title: Option<String>, path: &str, t: &Thresholds, (body, fade, looping, intro, lp): (u64, u64, bool, u64, u64)) -> FoundTrack {
    let duration_ms = if looping && lp > 0 { intro + lp } else { body };
    let file_name = file.path.rsplit('/').next().unwrap_or(&file.path).to_string();
    let name = title.unwrap_or_else(|| file_name.clone());
    let shape = Shape { looping, duration_ms, channels: 2, cd_track: 0, rip: true };
    let kind = classify(&shape, &format!("{path}/{file_name}"), t);
    let rate = u64::from(PLAY_RATE);
    let format = if file_name.to_ascii_uppercase().rsplit_once('.').is_some_and(|(_, e)| e.starts_with('O')) { "FMP" } else { "PMD" };
    FoundTrack {
        location: TrackLocation::RomSong { engine: format!("{ENGINE}@{disk}@{body}@{fade}@{}", file.path), song: 0 },
        name,
        kind,
        sample_rate: PLAY_RATE,
        channels: 2,
        duration_ms,
        looping,
        loop_start: intro * rate / 1000,
        loop_end: (intro + lp) * rate / 1000,
        codec: "PC-98 FM".into(),
        format: format.into(),
        bytes: u64::from(file.size),
        video: false,
    }
}

/// Opens a song stored by [`index`] (`pc98@disk@body@fade@file`).
pub fn open(path: &Path, engine: &str, _song: u32) -> Result<Box<dyn PcmStream>, String> {
    let mut f = engine.splitn(5, '@');
    let (_, disk, body, fade, file) = (f.next(), f.next(), f.next(), f.next(), f.next());
    let bad = || format!("a damaged PC-98 song record: {engine}");
    let disk: usize = disk.and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let body: u64 = body.and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let fade: u64 = fade.and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let file = file.ok_or_else(bad)?;
    if !has_drums() {
        drums_near(path);
    }
    let disks = disks(path)?;
    let entry = disks.get(disk).and_then(|d| d.find(file)).ok_or_else(|| format!("{file} is not on the disk"))?.clone();
    let player = Player::open(&disks, disk, &entry)?;
    let rate = u64::from(PLAY_RATE);
    let tail = if fade > 0 { super::track_timing::FADE_TAIL_MS } else { 0 };
    Ok(Box::new(Pc98Pcm { player, left: (body + fade + tail) * rate / 1000, fade: fade * rate / 1000, tail: tail * rate / 1000, stop_at_end: fade == 0 }))
}

/// A song, played for `left` more frames, the last `fade` of them fading out.
struct Pc98Pcm {
    player: Player,
    left: u64,
    fade: u64,
    tail: u64,
    stop_at_end: bool,
}

impl PcmStream for Pc98Pcm {
    fn sample_rate(&self) -> u32 {
        PLAY_RATE
    }
    fn channels(&self) -> u16 {
        2
    }
    fn total_samples(&self) -> Option<u64> {
        Some(self.left)
    }
    fn next_chunk(&mut self) -> Result<Option<Vec<i16>>, String> {
        if self.left == 0 || (self.stop_at_end && self.player.status().1) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comment_lines_lose_their_colour_codes() {
        assert_eq!(clean_text("\x1b[32mSTAGE 1\x1b[0m  Theme\r\n"), "STAGE 1 Theme");
    }

    #[test]
    fn a_flat_image_is_not_a_drum_rom() {
        assert!(!set_drums(&[0u8; DRUM_ROM_BYTES]));
        assert!(!set_drums(&[1u8; 16]));
    }
}
