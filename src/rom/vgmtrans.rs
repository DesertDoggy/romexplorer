//! Sequenced music read from a disc's own files: Sony's SEQ/VAB and the other sequence and
//! instrument formats VGMTrans knows, turned into MIDI + SoundFont and played with
//! TinySoundFont. Built into the `rommusic` library as `rvt_` (`deps/rommusic/vgmtrans/rvt.cpp`).
//!
//! Nothing is emulated: no BIOS and no set is needed, only the bytes the game itself plays
//! the music from. A scan finds the songs in a run of bytes and converts them; the bytes are
//! scanned again to play one, which is quick next to the conversion.

use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::OnceLock;

use libloading::Library;

use super::native_lib;

type ScanFn = unsafe extern "C" fn(*const u8, usize, *const c_char, *const c_char, i32, i32) -> *mut c_void;
type CountFn = unsafe extern "C" fn(*mut c_void) -> i32;
type TextFn = unsafe extern "C" fn(*mut c_void, i32) -> *const c_char;
type U32Fn = unsafe extern "C" fn(*mut c_void, i32) -> u32;
type FlagFn = unsafe extern "C" fn(*mut c_void, i32) -> i32;
type FreeFn = unsafe extern "C" fn(*mut c_void);
type PlayFn = unsafe extern "C" fn(*mut c_void, i32, i32, f32) -> *mut c_void;
type RenderFn = unsafe extern "C" fn(*mut c_void, *mut i16, usize) -> i32;

struct Api {
    _lib: Library,
    scan: ScanFn,
    count: CountFn,
    name: TextFn,
    format: TextFn,
    offset: U32Fn,
    looping: FlagFn,
    length_ms: U32Fn,
    free: FreeFn,
    play: PlayFn,
    render: RenderFn,
    play_free: FreeFn,
}

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| {
        let path = native_lib::rommusic_library()?;
        unsafe {
            let lib = Library::new(&path).ok()?;
            Some(Api {
                scan: *lib.get(b"rvt_scan\0").ok()?,
                count: *lib.get(b"rvt_count\0").ok()?,
                name: *lib.get(b"rvt_name\0").ok()?,
                format: *lib.get(b"rvt_format\0").ok()?,
                offset: *lib.get(b"rvt_offset\0").ok()?,
                looping: *lib.get(b"rvt_looping\0").ok()?,
                length_ms: *lib.get(b"rvt_length_ms\0").ok()?,
                free: *lib.get(b"rvt_free\0").ok()?,
                play: *lib.get(b"rvt_play\0").ok()?,
                render: *lib.get(b"rvt_render\0").ok()?,
                play_free: *lib.get(b"rvt_play_free\0").ok()?,
                _lib: lib,
            })
        }
    })
    .as_ref()
}

pub fn available() -> bool {
    api().is_some()
}

/// VGMTrans's own names for the formats of the disc consoles' sound drivers.
pub const PS1: &str = "PS1";

/// One song a scan found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Song {
    pub name: String,
    /// VGMTrans's name of its format ("PS1", ...).
    pub format: String,
    /// Where its sequence starts in the scanned bytes.
    pub offset: u32,
    pub looping: bool,
    /// To its last event, with the loop played again `loops` times.
    pub length_ms: u32,
}

/// What a scan converts besides the MIDI.
#[derive(Clone, Copy)]
enum Banks {
    None,
    One(u32),
}

struct Scan {
    api: &'static Api,
    handle: *mut c_void,
}

impl Drop for Scan {
    fn drop(&mut self) {
        unsafe { (self.api.free)(self.handle) };
    }
}

fn run(data: &[u8], name: &str, formats: &str, loops: u32, banks: Banks) -> Result<Option<Scan>, String> {
    let api = api().ok_or("the sequence player (rommusic) is not installed")?;
    let name = CString::new(name.replace('\0', "")).map_err(|e| e.to_string())?;
    let formats = CString::new(formats.replace('\0', "")).map_err(|e| e.to_string())?;
    let bank_for = match banks {
        Banks::None => -2,
        Banks::One(i) => i32::try_from(i).map_err(|e| e.to_string())?,
    };
    let loops = i32::try_from(loops).unwrap_or(i32::MAX);
    let handle = unsafe { (api.scan)(data.as_ptr(), data.len(), name.as_ptr(), formats.as_ptr(), loops, bank_for) };
    Ok((!handle.is_null()).then_some(Scan { api, handle }))
}

fn text(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// The songs in `data` (a file of the disc, `name` its name) in the given formats
/// (comma-separated, "" for all), their lengths taken with the loop played `loops` more times.
pub fn songs(data: &[u8], name: &str, formats: &str, loops: u32) -> Result<Vec<Song>, String> {
    let Some(scan) = run(data, name, formats, loops, Banks::None)? else { return Ok(Vec::new()) };
    let api = scan.api;
    let n = unsafe { (api.count)(scan.handle) };
    Ok((0..n)
        .map(|i| unsafe {
            Song {
                name: text((api.name)(scan.handle, i)),
                format: text((api.format)(scan.handle, i)),
                offset: (api.offset)(scan.handle, i),
                looping: (api.looping)(scan.handle, i) != 0,
                length_ms: (api.length_ms)(scan.handle, i),
            }
        })
        .collect())
}

/// Song `index` of [`songs`] being played.
pub struct Player {
    api: &'static Api,
    handle: *mut c_void,
    rate: u32,
    ended: bool,
}

// The handle is only touched through `&mut self`.
unsafe impl Send for Player {}

impl Player {
    /// Song `index` of what [`songs`] finds in `data` with the same `name` and `formats`,
    /// its loop played `loops` more times, at `rate` Hz.
    pub fn open(data: &[u8], name: &str, formats: &str, loops: u32, index: u32, rate: u32) -> Result<Self, String> {
        let scan = run(data, name, formats, loops, Banks::One(index))?.ok_or("no sequenced music in the file")?;
        let handle = unsafe { (scan.api.play)(scan.handle, index as i32, rate as i32, 0.0) };
        if handle.is_null() {
            return Err(format!("song {index} has no instruments to play it with"));
        }
        Ok(Player { api: scan.api, handle, rate, ended: false })
    }

    pub fn sample_rate(&self) -> u32 {
        self.rate
    }

    /// Fills `out` (interleaved stereo); silence once the song has ended.
    pub fn render(&mut self, out: &mut [i16]) {
        let end = unsafe { (self.api.render)(self.handle, out.as_mut_ptr(), out.len() / 2) };
        if end != 0 {
            self.ended = true;
        }
    }

    /// The last event has passed and every note has died away.
    pub fn ended(&self) -> bool {
        self.ended
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        unsafe { (self.api.play_free)(self.handle) };
    }
}
