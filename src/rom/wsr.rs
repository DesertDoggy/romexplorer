//! WonderSwan music rips (WSR): a cut-down cartridge image with a `WSRF` header 32 bytes from
//! its end, booted with the song number in AX. Played through Beetle WonderSwan, built into
//! the `rommusic` library as `rwsr_` (`deps/rommusic/wsr/rwsr.c`).
//!
//! A WSR gives neither a track count nor names: those come from the set's NEZplug-style
//! playlist (`file.wsr::WSR,$NN,title,length,loop,fade`), whose numbers are the song numbers. Without one, the song numbers from
//! the header's first song up are tried and the silent ones dropped.
//!
//! The core keeps its state in globals, so one WSR plays at a time in the process: opening a
//! second fails until the first is closed.

use std::ffi::c_void;
use std::sync::OnceLock;

use libloading::Library;

use super::gme::TrackInfo;
use super::native_lib;

/// The rate the player renders at.
pub const SAMPLE_RATE: u32 = 44_100;
/// Song numbers tried in a WSR with no playlist.
const UNLISTED_SONGS: u32 = 64;

type NewFn = unsafe extern "C" fn(*const u8, usize, i32) -> *mut c_void;
type FirstFn = unsafe extern "C" fn(*const u8, usize) -> i32;
type RenderFn = unsafe extern "C" fn(*mut c_void, *mut i16, usize) -> i32;
type FreeFn = unsafe extern "C" fn(*mut c_void);

struct Api {
    _lib: Library,
    new: NewFn,
    first: FirstFn,
    render: RenderFn,
    free: FreeFn,
}

/// Held while the core is opened or closed: its "already open" check is a plain C global,
/// so two threads opening at once could both pass it and share one machine.
static CORE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| {
        let path = native_lib::rommusic_library()?;
        unsafe {
            let lib = Library::new(&path).ok()?;
            Some(Api {
                new: *lib.get(b"rwsr_new\0").ok()?,
                first: *lib.get(b"rwsr_first_song\0").ok()?,
                render: *lib.get(b"rwsr_render\0").ok()?,
                free: *lib.get(b"rwsr_free\0").ok()?,
                _lib: lib,
            })
        }
    })
    .as_ref()
}

pub fn available() -> bool {
    api().is_some()
}

/// Whether `name` is a WSR this machine can play.
pub fn is_track_file(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, e)| e.eq_ignore_ascii_case("wsr")) && available()
}

/// One listed track: the song number given to the game, and what the playlist says.
#[derive(Debug, Clone, PartialEq)]
struct Track {
    song: u32,
    title: String,
    length_ms: Option<u32>,
    fade_ms: Option<u32>,
}

/// The tracks a NEZplug playlist lists. Numbers, `$hex` or decimal, are the song numbers the
/// game is given: WSR sets' playlists start from 0 or 1 as the game's songs do.
fn playlist_tracks(playlist: &[u8]) -> Vec<Track> {
    let text = String::from_utf8_lossy(playlist);
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((_, rest)) = line.split_once("::") else { continue };
        let fields = split_fields(rest);
        let Some(number) = fields.get(1).map(|n| n.trim()) else { continue };
        let song = match number.strip_prefix('$') {
            Some(hex) => u32::from_str_radix(hex, 16).ok(),
            None => number.parse::<u32>().ok(),
        };
        let Some(song) = song.filter(|&s| s < 256) else { continue };
        let field = |i: usize| fields.get(i).map(|s| s.trim()).unwrap_or("");
        out.push(Track {
            song,
            title: field(2).to_string(),
            length_ms: super::psf::parse_time(field(3)).filter(|&l| l > 0),
            fade_ms: super::psf::parse_time(field(5)),
        });
    }
    out
}

/// The song numbers a NEZplug playlist lists, each once, in its order.
pub fn listed_songs(playlist: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    for t in playlist_tracks(playlist) {
        if !out.contains(&(t.song as u8)) {
            out.push(t.song as u8);
        }
    }
    out
}

/// A playlist line's comma-separated fields; `\,` is a comma inside a field.
fn split_fields(text: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&',') => {
                chars.next();
                out.last_mut().unwrap().push(',');
            }
            ',' => out.push(String::new()),
            c => out.last_mut().unwrap().push(c),
        }
    }
    out
}

/// A WSR file, with the track being played.
pub struct Player {
    data: Vec<u8>,
    tracks: Vec<Track>,
    state: *mut c_void,
}

// The core's state is only touched through `&mut self`, and one player exists at a time.
unsafe impl Send for Player {}

impl Player {
    pub const CHANNELS: u16 = 2;

    pub fn open(data: Vec<u8>, playlist: &[u8]) -> Result<Self, String> {
        let api = api().ok_or("the WonderSwan music player (rommusic) is not installed")?;
        let first = unsafe { (api.first)(data.as_ptr(), data.len()) };
        if first < 0 {
            return Err("not a WSR file".into());
        }
        let mut tracks = playlist_tracks(playlist);
        if tracks.is_empty() {
            let first = first as u32;
            tracks = (first..(first + UNLISTED_SONGS).min(256)).map(|song| Track { song, title: String::new(), length_ms: None, fade_ms: None }).collect();
        }
        Ok(Player { data, tracks, state: std::ptr::null_mut() })
    }

    pub fn track_count(&self) -> u32 {
        self.tracks.len() as u32
    }

    pub fn track_info(&self, track: u32) -> Result<TrackInfo, String> {
        let t = self.tracks.get(track as usize).ok_or("no such track")?;
        Ok(TrackInfo {
            length_ms: t.length_ms,
            fade_ms: t.fade_ms,
            play_length_ms: t.length_ms.unwrap_or(150_000),
            system: "WonderSwan".into(),
            song: t.title.clone(),
            ..Default::default()
        })
    }

    pub fn start(&mut self, track: u32) -> Result<(), String> {
        self.close();
        let api = api().ok_or("the WonderSwan music player (rommusic) is not installed")?;
        let song = self.tracks.get(track as usize).ok_or("no such track")?.song;
        let state = {
            let _core = CORE.lock().unwrap_or_else(|p| p.into_inner());
            unsafe { (api.new)(self.data.as_ptr(), self.data.len(), song as i32) }
        };
        if state.is_null() {
            return Err("another WonderSwan track is playing; try again when it ends".into());
        }
        self.state = state;
        Ok(())
    }

    pub fn play(&mut self, out: &mut [i16]) -> Result<(), String> {
        let api = api().ok_or("the WonderSwan music player (rommusic) is not installed")?;
        if self.state.is_null() {
            return Err("no track started".into());
        }
        if unsafe { (api.render)(self.state, out.as_mut_ptr(), out.len() / 2) } < 0 {
            return Err("the WonderSwan player stopped on an error".into());
        }
        Ok(())
    }

    fn close(&mut self) {
        if !self.state.is_null() {
            if let Some(api) = api() {
                let _core = CORE.lock().unwrap_or_else(|p| p.into_inner());
                unsafe { (api.free)(self.state) };
            }
            self.state = std::ptr::null_mut();
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_playlist_gives_song_numbers_titles_and_times() {
        let list = b"# comment\ngame.wsr::WSR,$1A,Title\\, One,1:30,,5\ngame.wsr::WSR,3,Three,,,\nnot a track line\n";
        let tracks = playlist_tracks(list);
        assert_eq!(tracks.len(), 2);
        assert_eq!((tracks[0].song, tracks[0].title.as_str(), tracks[0].length_ms, tracks[0].fade_ms), (0x1A, "Title, One", Some(90_000), Some(5_000)));
        assert_eq!((tracks[1].song, tracks[1].length_ms), (3, None));
    }
}
