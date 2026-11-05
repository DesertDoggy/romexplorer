//! GBA music played straight from the ROM through agbplay (`external/agbplay`, LGPL-3.0), for
//! games made with Nintendo's standard sound engine (m4a). Loaded at run time like
//! [`super::gme`]: when the library has not been built for this machine [`available`] is false.
//!
//! The functions are `ragb_` in the `rommusic` library (`deps/rommusic/agbplay/ragb.cpp`), a flat C API over agbplay's own scanner
//! and player: the scanner finds the song table, music player table and mixing settings, so a
//! song is played by its number with no rip.

use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::OnceLock;

use libloading::Library;

use super::native_lib;

type OpenFn = unsafe extern "C" fn(*const u8, usize, u32, *mut c_char, usize) -> *mut c_void;
type OpenAtFn = unsafe extern "C" fn(*const u8, usize, u32, u64, u32, *mut c_char, usize) -> *mut c_void;
type CloseFn = unsafe extern "C" fn(*mut c_void);
type CountFn = unsafe extern "C" fn(*mut c_void) -> u32;
type TableFn = unsafe extern "C" fn(*mut c_void) -> u64;
type SongFn = unsafe extern "C" fn(*mut c_void, u32) -> u32;
type StartFn = unsafe extern "C" fn(*mut c_void, u32, c_int, *mut c_char, usize) -> c_int;
type RenderFn = unsafe extern "C" fn(*mut c_void, *mut i16, usize) -> usize;
type EndedFn = unsafe extern "C" fn(*mut c_void) -> c_int;

struct Api {
    _lib: Library,
    open: OpenFn,
    open_at: OpenAtFn,
    close: CloseFn,
    song_count: CountFn,
    song_table: TableFn,
    song_tracks: SongFn,
    song_player: SongFn,
    start: StartFn,
    render: RenderFn,
    ended: EndedFn,
}


fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| {
        let path = native_lib::rommusic_library()?;
        unsafe {
            let lib = Library::new(&path).map_err(|e| log::warn!("rommusic: {} would not load: {e}", path.display())).ok()?;
            Some(Api {
                open: *lib.get(b"ragb_open\0").ok()?,
                open_at: *lib.get(b"ragb_open_at\0").ok()?,
                close: *lib.get(b"ragb_close\0").ok()?,
                song_count: *lib.get(b"ragb_song_count\0").ok()?,
                song_table: *lib.get(b"ragb_song_table\0").ok()?,
                song_tracks: *lib.get(b"ragb_song_tracks\0").ok()?,
                song_player: *lib.get(b"ragb_song_player\0").ok()?,
                start: *lib.get(b"ragb_start\0").ok()?,
                render: *lib.get(b"ragb_render\0").ok()?,
                ended: *lib.get(b"ragb_ended\0").ok()?,
                _lib: lib,
            })
        }
    })
    .as_ref()
}

/// Whether the player library is present on this machine.
pub fn available() -> bool {
    api().is_some()
}

fn error_text(buf: &[c_char]) -> String {
    unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned()
}

/// One song as the ROM's table lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Song {
    pub number: u32,
    pub tracks: u32,
    /// The music player it plays on; BGM and sound effects usually use different ones.
    pub player: u32,
}

/// A ROM opened for playback: its song table found, ready to play songs by number.
pub struct Player {
    api: &'static Api,
    handle: *mut c_void,
    rate: u32,
}

// The handle is only used through `&mut self` (or `&self` for reads of the table), never shared
// between threads at once.
unsafe impl Send for Player {}

impl Player {
    /// Opens a GBA ROM image. Fails when the library is missing or the ROM has no song table of
    /// the standard engine (its music is made another way).
    pub fn open(rom: &[u8], sample_rate: u32) -> Result<Self, String> {
        let api = api().ok_or("the GBA music player (rommusic) is not installed")?;
        let mut err = [0 as c_char; 256];
        let handle = unsafe { (api.open)(rom.as_ptr(), rom.len(), sample_rate, err.as_mut_ptr(), err.len()) };
        if !handle.is_null() {
            return Ok(Self { api, handle, rate: sample_rate });
        }
        // agbplay's scanner also needs the engine's player table and mixing settings; a table
        // found by its shape alone plays with the engine's usual ones.
        let Some(table) = super::gba_m4a::song_table(rom) else { return Err(error_text(&err)) };
        let count = table.songs.last().map_or(0, |s| s.number + 1);
        let handle = unsafe { (api.open_at)(rom.as_ptr(), rom.len(), sample_rate, table.offset as u64, count, err.as_mut_ptr(), err.len()) };
        if handle.is_null() {
            return Err(error_text(&err));
        }
        Ok(Self { api, handle, rate: sample_rate })
    }

    pub fn sample_rate(&self) -> u32 {
        self.rate
    }

    /// Where the song table is in the ROM, as an offset.
    pub fn table_offset(&self) -> u64 {
        unsafe { (self.api.song_table)(self.handle) }
    }

    /// The songs in the table, empty slots left out.
    pub fn songs(&self) -> Vec<Song> {
        let count = unsafe { (self.api.song_count)(self.handle) };
        (0..count)
            .map(|n| Song { number: n, tracks: unsafe { (self.api.song_tracks)(self.handle, n) }, player: unsafe { (self.api.song_player)(self.handle, n) } })
            .filter(|s| s.tracks > 0)
            .collect()
    }

    /// Starts song `number`, playing its loop `loops` times before it ends.
    pub fn start(&mut self, number: u32, loops: u8) -> Result<(), String> {
        let mut err = [0 as c_char; 256];
        let ok = unsafe { (self.api.start)(self.handle, number, loops as c_int, err.as_mut_ptr(), err.len()) };
        if ok == 0 {
            return Err(error_text(&err));
        }
        Ok(())
    }

    /// Fills `out` with interleaved stereo samples; returns the frames written, fewer than
    /// `out.len() / 2` only once the song has ended.
    pub fn render(&mut self, out: &mut [i16]) -> usize {
        unsafe { (self.api.render)(self.handle, out.as_mut_ptr(), out.len() / 2) }
    }

    pub fn ended(&self) -> bool {
        unsafe { (self.api.ended)(self.handle) != 0 }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        unsafe { (self.api.close)(self.handle) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rom_without_the_engine_does_not_open() {
        if !available() {
            return;
        }
        let err = Player::open(&[0u8; 0x4000], 8000).err().expect("no song table in zeros");
        assert!(!err.is_empty());
    }
}
