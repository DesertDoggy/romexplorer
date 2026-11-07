//! The NES sound chip and the Famicom cartridge sound chips, from libgme
//! (`deps/rommusic/nes/rnes.cpp`, in the `rommusic` library), fed one register write at a time
//! by a game's own code running on [`super::m6502`]. Loaded at run time; [`available`] is false
//! when the library has not been built for this machine.

use std::ffi::{c_int, c_long, c_uint, c_void};
use std::sync::OnceLock;

use libloading::Library;

use super::native_lib;

type ReaderFn = unsafe extern "C" fn(*mut c_void, c_uint) -> c_int;

struct Api {
    _lib: Library,
    new: unsafe extern "C" fn(c_int, c_int, c_int) -> *mut c_void,
    delete: unsafe extern "C" fn(*mut c_void),
    set_reader: unsafe extern "C" fn(*mut c_void, Option<ReaderFn>, *mut c_void),
    write: unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int, c_int),
    read_status: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    read_n163: unsafe extern "C" fn(*mut c_void) -> c_int,
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
                new: *lib.get(b"rnes_new\0").ok()?,
                delete: *lib.get(b"rnes_delete\0").ok()?,
                set_reader: *lib.get(b"rnes_set_reader\0").ok()?,
                write: *lib.get(b"rnes_write\0").ok()?,
                read_status: *lib.get(b"rnes_read_status\0").ok()?,
                read_n163: *lib.get(b"rnes_read_n163\0").ok()?,
                end_frame: *lib.get(b"rnes_end_frame\0").ok()?,
                read: *lib.get(b"rnes_read\0").ok()?,
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

/// Where a sound register write goes (see `rnes.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    /// $4000-$4017.
    Apu,
    /// Konami VRC6: oscillator 0-2, register 0-2.
    Vrc6 {
        osc: u8,
        reg: u8,
    },
    N163Addr,
    N163Data,
    Fme7Latch,
    Fme7Data,
    /// $5000-$5015.
    Mmc5,
}

impl Target {
    fn code(self) -> (c_int, c_int) {
        match self {
            Target::Apu => (0, 0),
            Target::Vrc6 { osc, reg } => (1, (osc as c_int) * 4 + reg as c_int),
            Target::N163Addr => (2, 0),
            Target::N163Data => (3, 0),
            Target::Fme7Latch => (4, 0),
            Target::Fme7Data => (5, 0),
            Target::Mmc5 => (6, 0),
        }
    }
}

/// Cartridge sound chips, as `rnes_new` takes them.
pub const CHIP_VRC6: u8 = 1;
pub const CHIP_N163: u8 = 2;
pub const CHIP_FME7: u8 = 4;
pub const CHIP_MMC5: u8 = 8;

/// One emulated set of sound chips.
pub struct Apu {
    api: &'static Api,
    handle: *mut c_void,
}

// The handle is only used through `&mut self`.
unsafe impl Send for Apu {}

impl Apu {
    pub fn new(rate: u32, pal: bool, chips: u8) -> Result<Self, String> {
        let api = api().ok_or("the NES sound chip emulator (rommusic) is not installed")?;
        let handle = unsafe { (api.new)(rate as c_int, pal as c_int, chips as c_int) };
        if handle.is_null() {
            return Err("the NES sound chip emulator would not start".into());
        }
        Ok(Self { api, handle })
    }

    /// Where the DMC channel reads its samples: `reader(data, addr)`, valid until the next
    /// call that replaces it. Only consulted during [`Apu::write`] and [`Apu::end_frame`].
    pub fn set_reader(&mut self, reader: Option<ReaderFn>, data: *mut c_void) {
        unsafe { (self.api.set_reader)(self.handle, reader, data) }
    }

    /// A register write at CPU clock `time` of the current frame.
    pub fn write(&mut self, time: u32, target: Target, addr: u16, value: u8) {
        let (t, a) = target.code();
        let addr = if matches!(target, Target::Apu | Target::Mmc5) {
            addr as c_int
        } else {
            a
        };
        unsafe { (self.api.write)(self.handle, time as c_int, t, addr, value as c_int) }
    }

    pub fn read_status(&mut self, time: u32) -> u8 {
        unsafe { (self.api.read_status)(self.handle, time as c_int) as u8 }
    }

    pub fn read_n163(&mut self) -> u8 {
        unsafe { (self.api.read_n163)(self.handle) as u8 }
    }

    /// Ends the frame at CPU clock `time` and appends its samples (mono) to `out`.
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
