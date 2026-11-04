//! Game audio decoding through the vendored vgmstream (`external/vgmstream`), loaded at run
//! time like [`super::xaac`]: when the library has not been built for this machine
//! [`available`] is false and ROM audio is simply not offered.
//!
//! The library is `rvgmstream`, vgmstream plus a flat C API (`deps/vgmstream/rvgm.h`).
//! Input always comes through a [`VgmSource`], never a path, so a track is decoded where it
//! sits: inside a CHD, an RVZ, an ISO, or a cached archive member. Nothing is extracted
//! first.
//!
//! The build has vgmstream's built-in decoders only: ADX, HCA, Nintendo DSP and GC ADP,
//! PS-ADPCM, CD-XA, ATRAC9, MP3 (minimp3), Xbox/IMA/MS ADPCM, PCM and the rest of its own
//! `coding/`. Formats needing FFmpeg, libvorbis or mpg123 (ATRAC3, XMA, Wwise Vorbis) open
//! as unknown until those are added.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::PathBuf;
use std::sync::OnceLock;

use libloading::Library;

use super::native_lib;

/// The `RVGM_ABI_VERSION` this module was written against.
const ABI_VERSION: c_int = 1;

/// Where a stream's bytes come from. `open` is how vgmstream reaches companion files and
/// how it reopens this one: given this file's own name it must return a fresh reader of
/// the same bytes.
pub trait VgmSource: Send {
    /// Reads up to `dst.len()` bytes at `offset`; returns how many, 0 at the end.
    fn read_at(&mut self, offset: u64, dst: &mut [u8]) -> usize;
    fn size(&self) -> u64;
    /// The file named `name` beside this one (or this one again), or `None`.
    fn open(&self, name: &str) -> Option<Box<dyn VgmSource>>;
}

#[repr(C)]
struct RvgmIo {
    read: unsafe extern "C" fn(*mut c_void, *mut u8, i64, i32) -> i32,
    open: unsafe extern "C" fn(*mut c_void, *const c_char, *mut i64) -> *mut c_void,
    close: unsafe extern "C" fn(*mut c_void),
}

#[repr(C)]
struct RvgmInfo {
    channels: i32,
    sample_rate: i32,
    subsong_index: i32,
    subsong_count: i32,
    stream_samples: i64,
    loop_start: i64,
    loop_end: i64,
    loop_flag: i32,
    play_samples: i64,
    bitrate: i32,
    codec: [c_char; 128],
    meta: [c_char; 128],
    stream_name: [c_char; 256],
}

type OpenFn = unsafe extern "C" fn(*const RvgmIo, *mut c_void, i64, *const c_char, i32, f64, f64, i32) -> *mut c_void;
type InfoFn = unsafe extern "C" fn(*const c_void, *mut RvgmInfo);
type RenderFn = unsafe extern "C" fn(*mut c_void, *mut *const i16) -> i32;
type SeekFn = unsafe extern "C" fn(*mut c_void, i64);
type CloseFn = unsafe extern "C" fn(*mut c_void);
type KnownFn = unsafe extern "C" fn(*const c_char) -> i32;

struct Api {
    _lib: Library,
    open: OpenFn,
    info: InfoFn,
    render: RenderFn,
    seek: SeekFn,
    close: CloseFn,
    known: KnownFn,
}

fn find_library() -> Option<PathBuf> {
    if let Some(path) = native_lib::find_app_root_library("rvgmstream") {
        return Some(path);
    }
    let release_root = native_lib::source_root().join("deps/vgmstream/release");
    native_lib::find_native_library(&release_root, "rvgmstream")
}

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| {
        let path = find_library()?;
        unsafe {
            let lib = Library::new(&path).map_err(|e| log::warn!("rvgmstream: {} would not load: {e}", path.display())).ok()?;
            let version = *lib.get::<unsafe extern "C" fn() -> c_int>(b"rvgm_abi_version").ok()?;
            if version() != ABI_VERSION {
                log::warn!("rvgmstream: {} is ABI version {}, this build needs {ABI_VERSION}; rebuild it (deps/vgmstream/scripts/build.sh)", path.display(), version());
                return None;
            }
            let open = *lib.get::<OpenFn>(b"rvgm_open").ok()?;
            let info = *lib.get::<InfoFn>(b"rvgm_info_get").ok()?;
            let render = *lib.get::<RenderFn>(b"rvgm_render").ok()?;
            let seek = *lib.get::<SeekFn>(b"rvgm_seek").ok()?;
            let close = *lib.get::<CloseFn>(b"rvgm_close").ok()?;
            let known = *lib.get::<KnownFn>(b"rvgm_known_extension").ok()?;
            Some(Api { _lib: lib, open, info, render, seek, close, known })
        }
    })
    .as_ref()
}

/// Whether the decoder library is present on this machine.
pub fn available() -> bool {
    api().is_some()
}

/// Whether vgmstream knows `name`'s extension. Reads nothing; a first filter before
/// [`Stream::open`], which is the real test.
pub fn known_extension(name: &str) -> bool {
    let (Some(api), Ok(name)) = (api(), CString::new(name)) else { return false };
    unsafe { (api.known)(name.as_ptr()) != 0 }
}

// --- the callbacks: user data is a `*mut Box<dyn VgmSource>` --------------------------------

unsafe extern "C" fn cb_read(user: *mut c_void, dst: *mut u8, offset: i64, len: i32) -> i32 {
    if user.is_null() || dst.is_null() || offset < 0 || len <= 0 {
        return 0;
    }
    let source = &mut *(user as *mut Box<dyn VgmSource>);
    let buf = std::slice::from_raw_parts_mut(dst, len as usize);
    // A panic must not unwind into C.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| source.read_at(offset as u64, buf))).map(|n| n.min(len as usize) as i32).unwrap_or(0)
}

unsafe extern "C" fn cb_open(user: *mut c_void, name: *const c_char, size: *mut i64) -> *mut c_void {
    if user.is_null() || name.is_null() {
        return std::ptr::null_mut();
    }
    let source = &*(user as *mut Box<dyn VgmSource>);
    let name = CStr::from_ptr(name).to_string_lossy().into_owned();
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| source.open(&name))) {
        Ok(Some(opened)) => {
            if !size.is_null() {
                *size = opened.size().min(i64::MAX as u64) as i64;
            }
            Box::into_raw(Box::new(opened)) as *mut c_void
        }
        _ => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn cb_close(user: *mut c_void) {
    if !user.is_null() {
        drop(Box::from_raw(user as *mut Box<dyn VgmSource>));
    }
}

static IO: RvgmIo = RvgmIo { read: cb_read, open: cb_open, close: cb_close };

/// How a stream is played.
#[derive(Debug, Clone, Copy)]
pub struct PlayConfig {
    /// Times a looping stream plays its loop section (vgmstream's own default is 1).
    pub loops: f64,
    pub fade_seconds: f64,
    /// Play a looping stream once, as stored, with no fade: what indexing measures.
    pub ignore_loop: bool,
}

impl PlayConfig {
    /// Once through, as stored.
    pub const AS_STORED: Self = Self { loops: 1.0, fade_seconds: 0.0, ignore_loop: true };
    /// What a player expects of game BGM: the loop twice, then a ten-second fade.
    pub const LISTEN: Self = Self { loops: 2.0, fade_seconds: 10.0, ignore_loop: false };
}

/// What [`Stream::info`] reports.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamInfo {
    pub channels: u16,
    pub sample_rate: u32,
    /// 1-based; 0 when the format has no subsongs.
    pub subsong_index: u32,
    pub subsong_count: u32,
    /// One pass, per channel.
    pub stream_samples: u64,
    pub loop_start: u64,
    pub loop_end: u64,
    /// Whether the file itself loops, whatever this open's [`PlayConfig`] says.
    pub looping: bool,
    /// What [`Stream::render`] will produce with this open's [`PlayConfig`].
    pub play_samples: u64,
    pub bitrate: u32,
    pub codec: String,
    pub meta: String,
    pub stream_name: String,
}

impl StreamInfo {
    pub fn duration_ms(&self) -> u64 {
        if self.sample_rate == 0 {
            return 0;
        }
        self.stream_samples.saturating_mul(1000) / u64::from(self.sample_rate)
    }
}

fn c_text(raw: &[c_char]) -> String {
    let bytes: Vec<u8> = raw.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
    String::from_utf8_lossy(&bytes).trim().to_string()
}

/// One open stream.
pub struct Stream {
    api: &'static Api,
    handle: *mut c_void,
    info: StreamInfo,
}

// The handle is only ever used through `&mut self`; vgmstream keeps no thread-local state.
unsafe impl Send for Stream {}

impl Stream {
    /// Opens subsong `subsong` (0 = the first) of the file `name` read through `source`.
    /// `name` needs only to look like a file name; its extension is how vgmstream picks the
    /// format. `None` when vgmstream does not recognise it.
    pub fn open(source: Box<dyn VgmSource>, name: &str, subsong: u32, config: PlayConfig) -> Option<Self> {
        let api = api()?;
        let size = source.size();
        if size == 0 {
            return None;
        }
        let name = CString::new(name.replace('\0', "")).ok()?;
        // The root source only has to live through rvgm_open: vgmstream reopens what it
        // keeps through `open`, and those copies are owned (and closed) on the C side.
        let mut root: Box<Box<dyn VgmSource>> = Box::new(source);
        let user = (&mut *root) as *mut Box<dyn VgmSource> as *mut c_void;
        let handle = unsafe { (api.open)(&IO, user, size.min(i64::MAX as u64) as i64, name.as_ptr(), subsong.min(i32::MAX as u32) as i32, config.loops, config.fade_seconds, i32::from(config.ignore_loop)) };
        drop(root);
        if handle.is_null() {
            return None;
        }
        let mut raw: RvgmInfo = unsafe { std::mem::zeroed() };
        unsafe { (api.info)(handle, &mut raw) };
        let info = StreamInfo {
            channels: raw.channels.clamp(0, u16::MAX as i32) as u16,
            sample_rate: raw.sample_rate.max(0) as u32,
            subsong_index: raw.subsong_index.max(0) as u32,
            subsong_count: raw.subsong_count.max(0) as u32,
            stream_samples: raw.stream_samples.max(0) as u64,
            loop_start: raw.loop_start.max(0) as u64,
            loop_end: raw.loop_end.max(0) as u64,
            // With `ignore_loop` vgmstream reports the flag off but keeps the points: the file
            // still loops, it is just not being played that way.
            looping: raw.loop_flag != 0 || (config.ignore_loop && raw.loop_end > raw.loop_start),
            play_samples: raw.play_samples.max(0) as u64,
            bitrate: raw.bitrate.max(0) as u32,
            codec: c_text(&raw.codec),
            meta: c_text(&raw.meta),
            stream_name: c_text(&raw.stream_name),
        };
        if info.channels == 0 || info.sample_rate == 0 {
            unsafe { (api.close)(handle) };
            return None;
        }
        Some(Self { api, handle, info })
    }

    pub fn info(&self) -> &StreamInfo {
        &self.info
    }

    /// The next run of interleaved 16-bit samples, or `None` at the end.
    pub fn render(&mut self) -> Result<Option<&[i16]>, String> {
        let mut out: *const i16 = std::ptr::null();
        let n = unsafe { (self.api.render)(self.handle, &mut out) };
        if n < 0 {
            return Err(format!("vgmstream decode error {n}"));
        }
        if n == 0 || out.is_null() {
            return Ok(None);
        }
        let len = n as usize * usize::from(self.info.channels);
        Ok(Some(unsafe { std::slice::from_raw_parts(out, len) }))
    }

    /// Seeks to a sample (per channel) from the start.
    pub fn seek(&mut self, sample: u64) {
        unsafe { (self.api.seek)(self.handle, sample.min(i64::MAX as u64) as i64) };
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        unsafe { (self.api.close)(self.handle) };
    }
}

// --- sources --------------------------------------------------------------------------

/// A source over bytes in memory, with optional named siblings: tests, and archive members
/// already in the audio cache.
#[derive(Clone)]
pub struct MemorySource {
    pub name: String,
    pub bytes: std::sync::Arc<Vec<u8>>,
    pub siblings: std::sync::Arc<Vec<(String, std::sync::Arc<Vec<u8>>)>>,
}

impl MemorySource {
    pub fn new(name: &str, bytes: Vec<u8>) -> Self {
        Self { name: name.to_string(), bytes: std::sync::Arc::new(bytes), siblings: Default::default() }
    }
}

/// The last path component, `/` or `\` separated, compared case-insensitively: vgmstream
/// builds companion names from the name it was given, so only the file name matters.
pub fn file_name_of(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

impl VgmSource for MemorySource {
    fn read_at(&mut self, offset: u64, dst: &mut [u8]) -> usize {
        let Ok(start) = usize::try_from(offset) else { return 0 };
        if start >= self.bytes.len() {
            return 0;
        }
        let n = dst.len().min(self.bytes.len() - start);
        dst[..n].copy_from_slice(&self.bytes[start..start + n]);
        n
    }

    fn size(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn open(&self, name: &str) -> Option<Box<dyn VgmSource>> {
        let wanted = file_name_of(name);
        if wanted.eq_ignore_ascii_case(file_name_of(&self.name)) {
            return Some(Box::new(self.clone()));
        }
        let (sib_name, bytes) = self.siblings.iter().find(|(n, _)| file_name_of(n).eq_ignore_ascii_case(wanted))?;
        Some(Box::new(MemorySource { name: sib_name.clone(), bytes: bytes.clone(), siblings: self.siblings.clone() }))
    }
}

/// Decodes a whole stream to interleaved 16-bit PCM. For tests and short tracks; playback
/// streams through [`Stream::render`] instead.
pub fn decode_all(stream: &mut Stream) -> Result<Vec<i16>, String> {
    let mut out = Vec::new();
    while let Some(chunk) = stream.render()? {
        out.extend_from_slice(chunk);
    }
    Ok(out)
}
