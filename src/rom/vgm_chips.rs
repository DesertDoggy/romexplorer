//! Sound chips from libvgm (`deps/rommusic/chip/rchip.cpp`, in the `rommusic` library), fed
//! one register write at a time by a game's own code: the Master System and Game Gear's PSG
//! and the Japanese Master System's FM unit, the Mega Drive's FM chip, the PC Engine's sound unit, the MSX's PSG and Konami's SCC, the WonderSwan's sound unit. Loaded at run time; [`available`] is false when
//! the library has not been built for this machine.

use std::ffi::{c_double, c_int, c_long, c_void};
use std::sync::OnceLock;

use libloading::Library;

use super::native_lib;

struct Api {
    _lib: Library,
    new: unsafe extern "C" fn(c_int, c_double) -> *mut c_void,
    delete: unsafe extern "C" fn(*mut c_void),
    add: unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int) -> c_int,
    write: unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int, c_int),
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
                new: *lib.get(b"rchip_new\0").ok()?,
                delete: *lib.get(b"rchip_delete\0").ok()?,
                add: *lib.get(b"rchip_add\0").ok()?,
                write: *lib.get(b"rchip_write\0").ok()?,
                end_frame: *lib.get(b"rchip_end_frame\0").ok()?,
                read: *lib.get(b"rchip_read\0").ok()?,
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

/// A chip, as libvgm numbers them (`DEVID_` in `emu/SoundDevs.h`), and its variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The Master System's and Mega Drive's PSG.
    SegaPsg,
    /// The Game Gear's PSG, with its stereo register (port 1).
    GameGearPsg,
    /// The YM2413 (the Japanese Master System's FM unit). Port 0 is the address, 1 the data.
    Ym2413,
    /// The YM2612 (the Mega Drive's FM chip). Ports 0-3: address and data of its two halves.
    Ym2612,
    /// The HuC6280's sound unit (the PC Engine's). Ports 0-9: its registers.
    Huc6280,
    /// The AY-3-8910 (the MSX's PSG). Port 0 is the address, 1 the data.
    Ay8910,
    /// Konami's SCC (in MSX cartridges). Even ports pick a register, odd ones write it: 0/1
    /// waveforms, 2/3 frequencies, 4/5 volumes, 6/7 key on, 10/11 test.
    Scc,
    /// The WonderSwan's sound unit.
    WonderSwan,
}

impl Kind {
    fn code(self) -> (c_int, c_int) {
        match self {
            Kind::SegaPsg => (0x00, 0),
            Kind::GameGearPsg => (0x00, 1),
            Kind::Ym2413 => (0x01, 0),
            Kind::Ym2612 => (0x02, 0),
            Kind::Huc6280 => (0x1B, 0),
            Kind::Ay8910 => (0x12, 0),
            Kind::Scc => (0x19, 0),
            Kind::WonderSwan => (0x21, 0),
        }
    }
}

/// A set of chips mixed to stereo.
pub struct Chips {
    api: &'static Api,
    handle: *mut c_void,
}

// The handle is only used through `&mut self`.
unsafe impl Send for Chips {}

impl Chips {
    /// Chips rendering at `rate`; write times count `clock` a second.
    pub fn new(rate: u32, clock: f64, chips: &[(Kind, u32)]) -> Result<Self, String> {
        let api = api().ok_or("the sound chip emulator (rommusic) is not installed")?;
        let handle = unsafe { (api.new)(rate as c_int, clock) };
        if handle.is_null() {
            return Err("the sound chip emulator would not start".into());
        }
        let me = Self { api, handle };
        for &(kind, chip_clock) in chips {
            let (dev, variant) = kind.code();
            if unsafe { (api.add)(handle, dev, chip_clock as c_int, variant) } < 0 {
                return Err(format!("the {kind:?} sound chip would not start"));
            }
        }
        Ok(me)
    }

    /// A write to chip `chip` (in the order given to [`Chips::new`]), at `time` into the frame.
    pub fn write(&mut self, time: u32, chip: u8, port: u8, value: u8) {
        unsafe {
            (self.api.write)(
                self.handle,
                time as c_int,
                chip as c_int,
                port as c_int,
                value as c_int,
            )
        }
    }

    /// Ends the frame at `time` and appends its samples (interleaved stereo) to `out`.
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

impl Drop for Chips {
    fn drop(&mut self) {
        unsafe { (self.api.delete)(self.handle) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fm_unit_sounds() {
        if !available() {
            return;
        }
        let clock = 3_579_545.0;
        let mut c = Chips::new(44_100, clock, &[(Kind::Ym2413, 3_579_545)]).unwrap();
        // Channel 0: instrument 1 at full volume, A4-ish, key on.
        for (r, v) in [(0x30u8, 0x10u8), (0x10, 0xAC), (0x20, 0x18 | 0x01)] {
            c.write(0, 0, 0, r);
            c.write(0, 0, 1, v);
        }
        let mut out = Vec::new();
        for _ in 0..10 {
            c.end_frame(59_736, &mut out);
        }
        let peak = out.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        assert!(peak > 300, "peak {peak}");
        // A tenth of a second, stereo.
        assert!(
            (out.len() as i64 - 2 * 44_100 / 6).abs() < 20,
            "{}",
            out.len()
        );
    }

    #[test]
    fn the_mega_drive_fm_chip_sounds() {
        if !available() {
            return;
        }
        let clock = 7_670_453;
        let mut c = Chips::new(44_100, clock as f64, &[(Kind::Ym2612, clock)]).unwrap();
        let mut regs = vec![(0xB0u8, 0x07u8), (0xB4, 0xC0), (0xA4, 0x22), (0xA0, 0x69)];
        for op in [0x00u8, 0x04, 0x08, 0x0C] {
            regs.push((0x30 + op, 0x01));
            regs.push((0x40 + op, 0x00));
            regs.push((0x50 + op, 0x1F));
            regs.push((0x80 + op, 0x0F));
        }
        regs.push((0x28, 0xF0));
        for (i, (r, v)) in regs.into_iter().enumerate() {
            let t = i as u32 * 200;
            c.write(t, 0, 0, r);
            c.write(t, 0, 1, v);
        }
        let mut out = Vec::new();
        for _ in 0..10 {
            c.end_frame(127_856, &mut out);
        }
        let peak = out.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        assert!(peak > 300, "peak {peak}");
    }
}
