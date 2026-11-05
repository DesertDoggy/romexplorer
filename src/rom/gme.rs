//! Playback of rip sets for older consoles through Game Music Emu (`external/game-music-emu`,
//! LGPL-2.1), loaded at run time like [`super::vgmstream`]: when the library has not been built
//! for this machine [`available`] is false and these files are simply not offered.
//!
//! Formats: NSF/NSFE (NES), SPC (SNES), GBS (Game Boy), VGM/VGZ and GYM (Mega Drive, Master
//! System and other chips), HES (PC Engine), KSS (MSX), AY (ZX Spectrum), SAP (Atari), and SGC
//! (Game Gear, Master System) from kode54's fork, built into the same library.
//!
//! The library is `rommusic` (`deps/rommusic`), which exports gme.h's flat C API as it is. It is built without zlib: a gzipped `.vgz` is unpacked here through
//! [`super::zlib_ng`] before it is handed over.

use std::ffi::{c_char, c_int, c_long, c_void, CStr};
use std::sync::OnceLock;

use libloading::Library;

use super::{native_lib, zlib_ng};

/// What a rip-set file's extension says it is, for files gme plays. Lowercase, without the dot.
pub const EXTENSIONS: &[&str] = &["nsf", "nsfe", "spc", "gbs", "vgm", "vgz", "gym", "hes", "kss", "ay", "sap", "sgc"];

/// Whether `name`'s extension is one gme plays. Reads nothing.
pub fn known_extension(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, ext)| EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(ext)))
}

#[repr(C)]
struct GmeInfo {
    length: c_int,
    intro_length: c_int,
    loop_length: c_int,
    play_length: c_int,
    fade_length: c_int,
    _reserved_i: [c_int; 11],
    system: *const c_char,
    game: *const c_char,
    song: *const c_char,
    author: *const c_char,
    copyright: *const c_char,
    comment: *const c_char,
    dumper: *const c_char,
    _reserved_s: [*const c_char; 9],
}

type Err = *const c_char;
type OpenDataFn = unsafe extern "C" fn(*const c_void, c_long, *mut *mut c_void, c_int) -> Err;
type TrackCountFn = unsafe extern "C" fn(*const c_void) -> c_int;
type StartTrackFn = unsafe extern "C" fn(*mut c_void, c_int) -> Err;
type PlayFn = unsafe extern "C" fn(*mut c_void, c_int, *mut i16) -> Err;
type DeleteFn = unsafe extern "C" fn(*mut c_void);
type SetFadeMsecsFn = unsafe extern "C" fn(*mut c_void, c_int, c_int);
type SetAutoloadLimitFn = unsafe extern "C" fn(*mut c_void, c_int);
type IgnoreSilenceFn = unsafe extern "C" fn(*mut c_void, c_int);
type TrackEndedFn = unsafe extern "C" fn(*const c_void) -> c_int;
type TellFn = unsafe extern "C" fn(*const c_void) -> c_int;
type SeekFn = unsafe extern "C" fn(*mut c_void, c_int) -> Err;
type TrackInfoFn = unsafe extern "C" fn(*const c_void, *mut *mut GmeInfo, c_int) -> Err;
type FreeInfoFn = unsafe extern "C" fn(*mut GmeInfo);
type IdentifyHeaderFn = unsafe extern "C" fn(*const c_void) -> *const c_char;
type LoadM3uDataFn = unsafe extern "C" fn(*mut c_void, *const c_void, c_long) -> Err;

struct Api {
    _lib: Library,
    open_data: OpenDataFn,
    track_count: TrackCountFn,
    start_track: StartTrackFn,
    play: PlayFn,
    delete: DeleteFn,
    set_fade_msecs: SetFadeMsecsFn,
    set_autoload_limit: SetAutoloadLimitFn,
    /// Missing from a library built before it was exported.
    ignore_silence: Option<IgnoreSilenceFn>,
    track_ended: TrackEndedFn,
    tell: TellFn,
    seek: SeekFn,
    track_info: TrackInfoFn,
    free_info: FreeInfoFn,
    identify_header: IdentifyHeaderFn,
    load_m3u_data: LoadM3uDataFn,
}

/// Loads the functions of upstream gme.h's API under the prefix `prefix` from rommusic.
fn load_api(base: &str, prefix: &str) -> Option<Api> {
    let path = native_lib::rommusic_library()?;
    unsafe {
        let lib = Library::new(&path).map_err(|e| log::warn!("{base}: {} would not load: {e}", path.display())).ok()?;
        let sym = |name: &str| format!("{prefix}{name}\0").into_bytes();
        Some(Api {
            open_data: *lib.get(&sym("open_data")).ok()?,
            track_count: *lib.get(&sym("track_count")).ok()?,
            start_track: *lib.get(&sym("start_track")).ok()?,
            play: *lib.get(&sym("play")).ok()?,
            delete: *lib.get(&sym("delete")).ok()?,
            set_fade_msecs: *lib.get(&sym("set_fade_msecs")).ok()?,
            set_autoload_limit: *lib.get(&sym("set_autoload_playback_limit")).ok()?,
            ignore_silence: lib.get(&sym("ignore_silence")).ok().map(|f| *f),
            track_ended: *lib.get(&sym("track_ended")).ok()?,
            tell: *lib.get(&sym("tell")).ok()?,
            seek: *lib.get(&sym("seek")).ok()?,
            track_info: *lib.get(&sym("track_info")).ok()?,
            free_info: *lib.get(&sym("free_info")).ok()?,
            identify_header: *lib.get(&sym("identify_header")).ok()?,
            load_m3u_data: *lib.get(&sym("load_m3u_data")).ok()?,
            _lib: lib,
        })
    }
}

/// Upstream libgme.
fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| load_api("rommusic", "gme_")).as_ref()
}

/// SGC files (Game Gear, Master System), which upstream does not play, from kode54's fork
/// behind `rsgc_` functions (`deps/rommusic/sgc/rsgc.cpp`).
fn sgc_api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| load_api("rommusic (SGC)", "rsgc_")).as_ref()
}

/// Whether the SGC player is present on this machine.
pub fn sgc_available() -> bool {
    sgc_api().is_some()
}

fn is_sgc(data: &[u8]) -> bool {
    data.starts_with(b"SGC\x1A")
}

/// Whether the player library is present on this machine.
pub fn available() -> bool {
    api().is_some()
}

fn err_text(err: Err) -> Option<String> {
    (!err.is_null()).then(|| unsafe { CStr::from_ptr(err) }.to_string_lossy().into_owned())
}

fn text(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().trim().to_string()
}

/// The format gme sees in a file's first four bytes ("NSF", "SPC", "VGM" ...), or `None`.
/// A gzipped `.vgz` reads as `None` here; [`Emu::open`] unpacks it first.
pub fn identify(header: &[u8]) -> Option<String> {
    if is_sgc(header) {
        return sgc_available().then(|| "SGC".to_string());
    }
    let api = api()?;
    let mut four = [0u8; 4];
    let n = header.len().min(4);
    four[..n].copy_from_slice(&header[..n]);
    let t = text(unsafe { (api.identify_header)(four.as_ptr().cast()) });
    (!t.is_empty()).then_some(t)
}

/// Unpacks a gzip member (a `.vgz`) through zlib-ng. `None` when `data` is not gzip or does
/// not inflate to the size its trailer gives.
pub fn gunzip(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 18 || data[0] != 0x1F || data[1] != 0x8B || data[2] != 8 {
        return None;
    }
    let flags = data[3];
    let mut pos = 10usize;
    if flags & 0x04 != 0 {
        let xlen = u16::from_le_bytes([*data.get(pos)?, *data.get(pos + 1)?]) as usize;
        pos += 2 + xlen;
    }
    for bit in [0x08u8, 0x10] {
        if flags & bit != 0 {
            pos += data.get(pos..)?.iter().position(|&b| b == 0)? + 1;
        }
    }
    if flags & 0x02 != 0 {
        pos += 2;
    }
    let body = data.get(pos..data.len().checked_sub(8)?)?;
    let size = u32::from_le_bytes(data[data.len() - 4..].try_into().ok()?) as usize;
    // A rip-set file is a few MB at most; a trailer claiming more is not one.
    if size > 256 << 20 {
        return None;
    }
    zlib_ng::raw_inflate(body, size)
}

/// What a file says about one of its tracks. Times are in milliseconds.
#[derive(Debug, Clone, Default)]
pub struct TrackInfo {
    /// The length the file gives, if it gives one.
    pub length_ms: Option<u32>,
    pub intro_ms: Option<u32>,
    pub loop_ms: Option<u32>,
    /// `length_ms`, else intro + two loops, else gme's default of 2.5 minutes.
    pub play_length_ms: u32,
    pub fade_ms: Option<u32>,
    pub system: String,
    pub game: String,
    pub song: String,
    pub author: String,
    pub copyright: String,
    pub comment: String,
    pub dumper: String,
}

impl TrackInfo {
    /// Whether the file itself says how long this track is (or where its loop is), rather than
    /// gme falling back to its default.
    pub fn has_length(&self) -> bool {
        self.length_ms.is_some() || self.loop_ms.is_some()
    }

    /// Whether the track loops: it has a loop section the file marks.
    pub fn looping(&self) -> bool {
        self.loop_ms.is_some_and(|l| l > 0)
    }
}

fn ms(v: c_int) -> Option<u32> {
    (v >= 0).then_some(v as u32)
}

/// One loaded file. gme keeps pointers into the bytes it was given (SPC, VGM, GYM), so they
/// live here as long as the emulator does.
pub struct Emu {
    api: &'static Api,
    handle: *mut c_void,
    _data: Vec<u8>,
    sample_rate: u32,
}

// The handle is only ever used through `&mut self` (or `&self` for read-only queries gme
// documents as such); gme keeps no global mutable state.
unsafe impl Send for Emu {}

impl Emu {
    /// The output rate used everywhere in the app for these files.
    pub const SAMPLE_RATE: u32 = 44_100;
    /// gme always renders interleaved stereo.
    pub const CHANNELS: u16 = 2;

    /// Loads a whole file. A gzipped `.vgz` is unpacked first.
    pub fn open(data: Vec<u8>, sample_rate: u32) -> Result<Self, String> {
        let data = if data.starts_with(&[0x1F, 0x8B]) { gunzip(&data).ok_or("the gzipped file would not unpack")? } else { data };
        let api = if is_sgc(&data) { sgc_api().ok_or("the SGC player (rommusic) is not available")? } else { api().ok_or("the chip music library (rommusic) is not available")? };
        if data.is_empty() || data.len() > c_long::MAX as usize {
            return Err("empty or oversized file".into());
        }
        let mut handle: *mut c_void = std::ptr::null_mut();
        let err = unsafe { (api.open_data)(data.as_ptr().cast(), data.len() as c_long, &mut handle, sample_rate as c_int) };
        if let Some(e) = err_text(err) {
            if !handle.is_null() {
                unsafe { (api.delete)(handle) };
            }
            return Err(e);
        }
        if handle.is_null() {
            return Err("gme returned no emulator".into());
        }
        // Without this gme cuts a track at its "play length" by itself; the app decides that.
        unsafe { (api.set_autoload_limit)(handle, 0) };
        Ok(Self { api, handle, _data: data, sample_rate })
    }

    /// Applies a playlist (the NEZplug-style `.m3u` rip sets ship beside an NSF, GBS, HES or
    /// KSS): its entries become the tracks, with their titles and lengths.
    pub fn load_m3u(&mut self, text: &[u8]) -> Result<(), String> {
        if text.is_empty() || text.len() > c_long::MAX as usize {
            return Err("empty or oversized playlist".into());
        }
        err_text(unsafe { (self.api.load_m3u_data)(self.handle, text.as_ptr().cast(), text.len() as c_long) }).map_or(Ok(()), Err)
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn track_count(&self) -> u32 {
        unsafe { (self.api.track_count)(self.handle) }.max(0) as u32
    }

    pub fn track_info(&self, track: u32) -> Result<TrackInfo, String> {
        let mut raw: *mut GmeInfo = std::ptr::null_mut();
        let err = unsafe { (self.api.track_info)(self.handle, &mut raw, track as c_int) };
        if let Some(e) = err_text(err) {
            return Err(e);
        }
        if raw.is_null() {
            return Err("gme returned no track info".into());
        }
        let r = unsafe { &*raw };
        let info = TrackInfo {
            length_ms: ms(r.length).filter(|&v| v > 0),
            intro_ms: ms(r.intro_length),
            loop_ms: ms(r.loop_length),
            play_length_ms: ms(r.play_length).unwrap_or(150_000),
            fade_ms: ms(r.fade_length),
            system: text(r.system),
            game: text(r.game),
            song: text(r.song),
            author: text(r.author),
            copyright: text(r.copyright),
            comment: text(r.comment),
            dumper: text(r.dumper),
        };
        unsafe { (self.api.free_info)(raw) };
        Ok(info)
    }

    /// Starts `track` from the beginning. With `fade` the track fades out over `fade.1` ms
    /// starting at `fade.0` ms, after which [`Emu::ended`] turns true.
    pub fn start(&mut self, track: u32, fade: Option<(u32, u32)>) -> Result<(), String> {
        if let Some(e) = err_text(unsafe { (self.api.start_track)(self.handle, track as c_int) }) {
            return Err(e);
        }
        if let Some((at, len)) = fade {
            unsafe { (self.api.set_fade_msecs)(self.handle, at.min(i32::MAX as u32) as c_int, len.min(i32::MAX as u32) as c_int) };
        }
        Ok(())
    }

    /// Plays on through silence instead of ending the track there. For a track whose length
    /// is known (a playlist or the file says it): some start with a pause or have a quiet
    /// passage, which silence detection would take for the end.
    pub fn ignore_silence(&mut self, ignore: bool) {
        if let Some(f) = self.api.ignore_silence {
            unsafe { f(self.handle, c_int::from(ignore)) };
        }
    }

    /// Fills `out` with interleaved stereo samples. `out.len()` must be even.
    pub fn play(&mut self, out: &mut [i16]) -> Result<(), String> {
        let n = out.len() & !1;
        err_text(unsafe { (self.api.play)(self.handle, n.min(i32::MAX as usize) as c_int, out.as_mut_ptr()) }).map_or(Ok(()), Err)
    }

    /// Whether the track has ended: its fade finished, or it fell silent.
    pub fn ended(&self) -> bool {
        unsafe { (self.api.track_ended)(self.handle) != 0 }
    }

    /// Milliseconds played since the track started.
    pub fn tell_ms(&self) -> u32 {
        unsafe { (self.api.tell)(self.handle) }.max(0) as u32
    }

    /// Seeks to `msec` from the track's start (gme plays up to it, so far seeks take time).
    pub fn seek_ms(&mut self, msec: u32) -> Result<(), String> {
        err_text(unsafe { (self.api.seek)(self.handle, msec.min(i32::MAX as u32) as c_int) }).map_or(Ok(()), Err)
    }
}

impl Drop for Emu {
    fn drop(&mut self) {
        unsafe { (self.api.delete)(self.handle) };
    }
}

/// Renders a whole track: its own length when the file gives one (else `fallback_ms`), then a
/// fade of `fade_ms`. For export and tests; playback streams through [`Emu::play`].
pub fn render_track(emu: &mut Emu, track: u32, fallback_ms: u32, fade_ms: u32) -> Result<Vec<i16>, String> {
    let info = emu.track_info(track)?;
    let body = info.length_ms.or_else(|| Some(info.intro_ms? + 2 * info.loop_ms?)).unwrap_or(fallback_ms);
    emu.start(track, Some((body, fade_ms)))?;
    let total = (u64::from(body) + u64::from(fade_ms)) * u64::from(emu.sample_rate()) / 1000 * 2;
    let mut out = Vec::with_capacity(total as usize);
    let mut buf = vec![0i16; 4096];
    while !emu.ended() && (out.len() as u64) < total {
        emu.play(&mut buf)?;
        out.extend_from_slice(&buf);
    }
    out.truncate(total as usize);
    Ok(out)
}
