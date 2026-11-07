//! The Game Boy sound chip, from libgme (`deps/rommusic/gb/rgb.cpp`, in the `rommusic`
//! library), fed one register write at a time by a game's own code running on
//! [`super::sm83`]. Loaded at run time; [`available`] is false when the library has not been
//! built for this machine.

use std::ffi::{c_int, c_long, c_void};
use std::sync::OnceLock;

use libloading::Library;

use super::native_lib;

struct Api {
    _lib: Library,
    new: unsafe extern "C" fn(c_int) -> *mut c_void,
    delete: unsafe extern "C" fn(*mut c_void),
    write: unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int),
    end_frame: unsafe extern "C" fn(*mut c_void, c_int) -> c_long,
    read: unsafe extern "C" fn(*mut c_void, *mut i16, c_long) -> c_long,
}

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| {
        let path = native_lib::rommusic_library()?;
        unsafe {
            let lib = Library::new(&path)
                .map_err(|e| log::warn!("rommusic: {} would not load: {e}", path.display()))
                .ok()?;
            Some(Api {
                new: *lib.get(b"rgb_new\0").ok()?,
                delete: *lib.get(b"rgb_delete\0").ok()?,
                write: *lib.get(b"rgb_write\0").ok()?,
                end_frame: *lib.get(b"rgb_end_frame\0").ok()?,
                read: *lib.get(b"rgb_read_samples\0").ok()?,
                _lib: lib,
            })
        }
    })
    .as_ref()
}

/// Whether the sound chip library is present on this machine.
pub fn available() -> bool {
    api().is_some()
}

/// One emulated Game Boy sound chip, stereo.
pub struct Apu {
    api: &'static Api,
    handle: *mut c_void,
}

// The handle is only used through `&mut self`.
unsafe impl Send for Apu {}

impl Apu {
    pub fn new(rate: u32) -> Result<Self, String> {
        let api = api().ok_or("the Game Boy sound chip emulator (rommusic) is not installed")?;
        let handle = unsafe { (api.new)(rate as c_int) };
        if handle.is_null() {
            return Err("the Game Boy sound chip emulator would not start".into());
        }
        Ok(Self { api, handle })
    }

    /// A write to $FF10-$FF3F at clock `time` (4,194,304 Hz) of the current frame.
    pub fn write(&mut self, time: u32, addr: u16, value: u8) {
        unsafe { (self.api.write)(self.handle, time as c_int, addr as c_int, value as c_int) }
    }

    /// Ends the frame at clock `time` and appends its samples (interleaved stereo) to `out`.
    pub fn end_frame(&mut self, time: u32, out: &mut Vec<i16>) {
        let n = unsafe { (self.api.end_frame)(self.handle, time as c_int) };
        if n > 0 {
            let at = out.len();
            out.resize(at + n as usize, 0);
            let got = unsafe { (self.api.read)(self.handle, out[at..].as_mut_ptr(), n) };
            out.truncate(at + got.max(0) as usize);
        }
    }
}

impl Drop for Apu {
    fn drop(&mut self) {
        unsafe { (self.api.delete)(self.handle) }
    }
}
