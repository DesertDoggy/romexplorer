//! The SNES sound chip, emulated by blargg's Snes_Spc (`deps/rommusic/spc`, in the `rommusic` library) and
//! driven the way the SNES drives it: through its four I/O ports, in time. Loaded at run time;
//! [`available`] is false when the library has not been built for this machine.
//!
//! [`Apu::upload`] sends blocks through the boot ROM's transfer protocol, exactly as a game's
//! upload routine does, so a driver rebuilt from a ROM starts the way it does on the console.

use std::ffi::{c_int, c_long, c_void};
use std::sync::OnceLock;

use libloading::Library;

use super::native_lib;

type NewFn = unsafe extern "C" fn() -> *mut c_void;
type DeleteFn = unsafe extern "C" fn(*mut c_void);
type LoadFn = unsafe extern "C" fn(*mut c_void, *const c_void, c_long) -> c_int;
type ReadPortFn = unsafe extern "C" fn(*mut c_void, c_int, c_int) -> c_int;
type WritePortFn = unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int);
type EndFrameFn = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type SamplesFn = unsafe extern "C" fn(*mut c_void) -> *const i16;
type PlayFn = unsafe extern "C" fn(*mut c_void, c_int, *mut i16) -> c_int;
type RamFn = unsafe extern "C" fn(*mut c_void) -> *mut u8;
type PcFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type SaveFn = unsafe extern "C" fn(*mut c_void, *mut u8, c_long) -> c_long;

struct Api {
    _lib: Library,
    new: NewFn,
    delete: DeleteFn,
    load_spc: LoadFn,
    read_port: ReadPortFn,
    write_port: WritePortFn,
    end_frame: EndFrameFn,
    samples: SamplesFn,
    play: PlayFn,
    ram: RamFn,
    pc: PcFn,
    save_spc: SaveFn,
}


fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| {
        let path = native_lib::rommusic_library()?;
        unsafe {
            let lib = Library::new(&path).map_err(|e| log::warn!("rommusic: {} would not load: {e}", path.display())).ok()?;
            Some(Api {
                new: *lib.get(b"rspc_new\0").ok()?,
                delete: *lib.get(b"rspc_delete\0").ok()?,
                load_spc: *lib.get(b"rspc_load_spc\0").ok()?,
                read_port: *lib.get(b"rspc_read_port\0").ok()?,
                write_port: *lib.get(b"rspc_write_port\0").ok()?,
                end_frame: *lib.get(b"rspc_end_frame\0").ok()?,
                samples: *lib.get(b"rspc_samples\0").ok()?,
                play: *lib.get(b"rspc_play\0").ok()?,
                ram: *lib.get(b"rspc_ram\0").ok()?,
                pc: *lib.get(b"rspc_pc\0").ok()?,
                save_spc: *lib.get(b"rspc_save_spc\0").ok()?,
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

/// Sound-chip clocks per second.
pub const CLOCK_RATE: u32 = 1_024_000;
/// Output samples per second (stereo pairs).
pub const SAMPLE_RATE: u32 = 32_000;
/// A frame of the SNES's 60 Hz video: how often the time base restarts.
pub const FRAME_CLOCKS: i32 = (CLOCK_RATE / 60) as i32;
/// Bytes in an `.spc` file without its extended tag.
pub const SPC_FILE_SIZE: usize = 0x10200;
/// Clocks between two port accesses of a polling loop: about what a CPU wait loop takes.
const POLL_CLOCKS: i32 = 8;
/// A wait for the chip's answer longer than this means it is not answering.
const MAX_WAIT_CLOCKS: u64 = CLOCK_RATE as u64 / 2;

/// One emulated sound chip.
pub struct Apu {
    api: &'static Api,
    handle: *mut c_void,
    /// Clocks into the current frame.
    time: i32,
    /// Output of the frames ended so far, when kept.
    pub keep_output: bool,
    pub output: Vec<i16>,
}

// The handle is only used through `&mut self`.
unsafe impl Send for Apu {}

impl Apu {
    /// A chip just powered on, its boot ROM waiting for an upload.
    pub fn new() -> Result<Self, String> {
        let api = api().ok_or("the SNES sound chip emulator (rommusic) is not installed")?;
        let handle = unsafe { (api.new)() };
        if handle.is_null() {
            return Err("the SNES sound chip emulator would not start".into());
        }
        Ok(Self { api, handle, time: 0, keep_output: false, output: Vec::new() })
    }

    /// A chip started from an `.spc` snapshot.
    pub fn from_spc(spc: &[u8]) -> Result<Self, String> {
        let apu = Self::new()?;
        if unsafe { (apu.api.load_spc)(apu.handle, spc.as_ptr().cast(), spc.len() as c_long) } != 0 {
            return Err("not an .spc snapshot".into());
        }
        Ok(apu)
    }

    /// Moves time on by `clocks`, ending frames as they fill.
    pub fn advance(&mut self, clocks: i32) {
        self.time += clocks;
        while self.time >= FRAME_CLOCKS {
            let n = unsafe { (self.api.end_frame)(self.handle, FRAME_CLOCKS) } as usize;
            if self.keep_output && n > 0 {
                let s = unsafe { std::slice::from_raw_parts((self.api.samples)(self.handle), n) };
                self.output.extend_from_slice(s);
            }
            self.time -= FRAME_CLOCKS;
        }
    }

    pub fn read_port(&mut self, port: usize) -> u8 {
        self.advance(POLL_CLOCKS);
        self.read_port_now(port)
    }

    pub fn write_port(&mut self, port: usize, value: u8) {
        self.advance(1);
        self.write_port_now(port, value)
    }

    /// Reads a port at the current time, for a caller that keeps time itself.
    pub fn read_port_now(&mut self, port: usize) -> u8 {
        unsafe { (self.api.read_port)(self.handle, self.time, (port & 3) as c_int) as u8 }
    }

    /// Writes a port at the current time, for a caller that keeps time itself.
    pub fn write_port_now(&mut self, port: usize, value: u8) {
        unsafe { (self.api.write_port)(self.handle, self.time, (port & 3) as c_int, c_int::from(value)) }
    }

    /// Waits until `port` reads `value`, as a CPU loop would.
    fn wait_for(&mut self, port: usize, value: u8) -> Result<(), String> {
        let mut waited = 0u64;
        while self.read_port(port) != value {
            waited += POLL_CLOCKS as u64;
            if waited > MAX_WAIT_CLOCKS {
                return Err(format!("the sound chip stopped answering (port {port}, waiting for {value:#04x})"));
            }
        }
        Ok(())
    }

    /// Sends `blocks` (ARAM address, bytes) through the boot ROM's transfer protocol and starts
    /// the code at `entry`. The chip must be running its boot ROM: just made, or sent back to it
    /// by its driver.
    pub fn upload(&mut self, blocks: &[(u16, &[u8])], entry: u16) -> Result<(), String> {
        self.wait_for(0, 0xAA)?;
        self.wait_for(1, 0xBB)?;
        let mut kick: u8 = 0xCC;
        for (addr, bytes) in blocks {
            self.write_port(2, *addr as u8);
            self.write_port(3, (*addr >> 8) as u8);
            self.write_port(1, 1);
            self.write_port(0, kick);
            self.wait_for(0, kick)?;
            for (i, b) in bytes.iter().enumerate() {
                self.write_port(1, *b);
                self.write_port(0, i as u8);
                self.wait_for(0, i as u8)?;
            }
            // The next command is the last index plus two (never zero).
            kick = (bytes.len() as u8).wrapping_add(1).max(1);
        }
        self.write_port(2, entry as u8);
        self.write_port(3, (entry >> 8) as u8);
        self.write_port(1, 0);
        self.write_port(0, kick);
        self.wait_for(0, kick)?;
        Ok(())
    }

    /// The chip's 64 KB of sound memory as it stands.
    pub fn ram(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts((self.api.ram)(self.handle), 0x10000) }
    }

    /// Where the sound CPU is running.
    pub fn pc(&self) -> u16 {
        unsafe { (self.api.pc)(self.handle) as u16 }
    }

    /// Clocks run into the current frame.
    pub fn clocks_into_frame(&self) -> i32 {
        self.time
    }

    /// The chip's state as an `.spc` file, taken at the next frame boundary.
    pub fn save_spc(&mut self) -> Vec<u8> {
        if self.time > 0 {
            self.advance(FRAME_CLOCKS - self.time);
        }
        let mut out = vec![0u8; SPC_FILE_SIZE];
        let n = unsafe { (self.api.save_spc)(self.handle, out.as_mut_ptr(), out.len() as c_long) };
        out.truncate(n.max(0) as usize);
        out
    }

    /// Plays `out.len()` samples (stereo, 32 kHz) with the ports left as they are.
    pub fn play(&mut self, out: &mut [i16]) -> Result<(), String> {
        if unsafe { (self.api.play)(self.handle, (out.len() & !1) as c_int, out.as_mut_ptr()) } != 0 {
            return Err("the sound chip emulator failed".into());
        }
        Ok(())
    }
}

impl Drop for Apu {
    fn drop(&mut self) {
        unsafe { (self.api.delete)(self.handle) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_upload_lands_in_sound_memory_and_runs() {
        if !available() {
            eprintln!("rommusic not built; skipped");
            return;
        }
        let mut apu = Apu::new().unwrap();
        // A tiny driver at $0400: write $5A to port 0, then loop forever.
        // MOV A,#$5A ; MOV $F4,A ; BRA -2
        let code = [0xE8, 0x5A, 0xC4, 0xF4, 0x2F, 0xFE];
        let data: Vec<u8> = (0..300u32).map(|i| (i * 7) as u8).collect();
        apu.upload(&[(0x0400, &code), (0x2000, &data)], 0x0400).unwrap();
        assert_eq!(&apu.ram()[0x0400..0x0406], &code);
        assert_eq!(&apu.ram()[0x2000..0x2000 + 300], &data[..]);
        apu.advance(1000);
        assert_eq!(apu.read_port(0), 0x5A);
        // The snapshot plays on as the chip did.
        let spc = apu.save_spc();
        assert_eq!(spc.len(), SPC_FILE_SIZE);
        assert_eq!(&spc[0x100 + 0x0400..0x100 + 0x0406], &code);
        // $F4 holds what the main CPU last wrote (the end-of-upload command), as .spc files do.
        assert_eq!(spc[0x100 + 0xF4], 0x2D);
        let mut again = Apu::from_spc(&spc).unwrap();
        again.advance(1000);
        assert!((0x0402..0x0406).contains(&again.pc()), "{:#x}", again.pc());
    }
}
