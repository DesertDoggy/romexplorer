//! AAC and xHE-AAC encoding through the vendored libxaac (`external/libxaac`), loaded at
//! run time like [`super::zlib_ng`]: when the library has not been built for this machine
//! [`available`] is false and callers keep the original file instead of converting it.
//!
//! The library is not libxaac itself but `rxaac`, a flat C API over it
//! (`deps/libxaac/rxaac.h`). libxaac's own API takes two large structs the caller
//! must mirror, and in xHE-AAC mode reads a third through a pointer that crashes when left
//! NULL (which is what the published `xaac-rs` binding does); the shim owns all of that.
//!
//! Measured here (Windows 11, x64, one core, a synthetic stereo minute at 44.1 kHz -- real
//! music will differ in size, less so in speed):
//!
//! | format | asked | got | speed |
//! |---|---|---|---|
//! | AAC-LC | 128 kbps | 130 kbps | ~130x realtime |
//! | xHE-AAC | 128 kbps | 98 kbps | ~45x realtime |
//! | AAC-LC | 64 kbps | 66 kbps | ~145x realtime |
//! | xHE-AAC | 64 kbps | 66 kbps | ~50x realtime |
//!
//! So a four-minute track costs about two seconds as AAC and five as xHE-AAC, once.

use std::ffi::c_int;
use std::path::PathBuf;
use std::sync::OnceLock;

use libloading::Library;

use super::native_lib;

/// The `RXAAC_ABI_VERSION` this module was written against.
const ABI_VERSION: c_int = 1;

/// The loudness an xHE-AAC stream declares, in LKFS.
///
/// xHE-AAC always carries one, and a decoder turns the track up or down by the distance to
/// its own target. Measured on Windows 11: a stream declaring 0 played 16 dB quieter than
/// the same audio as AAC, and one declaring -16 played at the same level -- so -16 is the
/// target there (it is also Apple's and Android's), and declaring exactly that means "leave
/// the level alone". A converted copy then plays as loud as its original, and as loud as the
/// tracks beside it that were copied unconverted.
pub const DECLARED_LOUDNESS_LKFS: f64 = -16.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Aac,
    XheAac,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Self::Aac => "aac",
            Self::XheAac => "xhe_aac",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "aac" => Some(Self::Aac),
            "xhe_aac" => Some(Self::XheAac),
            _ => None,
        }
    }
}

type OpenFn = unsafe extern "C" fn(i32, i32, i32, i32, f64, *mut i32) -> *mut std::ffi::c_void;
type FrameSamplesFn = unsafe extern "C" fn(*const std::ffi::c_void) -> i32;
type ConfigFn = unsafe extern "C" fn(*const std::ffi::c_void, *mut i32) -> *const u8;
type EncodeFn = unsafe extern "C" fn(*mut std::ffi::c_void, *const i16, *mut *const u8) -> i32;
type CloseFn = unsafe extern "C" fn(*mut std::ffi::c_void);

struct Api {
    // Kept so the function pointers below stay valid.
    _lib: Library,
    open: OpenFn,
    frame_samples: FrameSamplesFn,
    config: ConfigFn,
    encode: EncodeFn,
    close: CloseFn,
}

fn find_library() -> Option<PathBuf> {
    if let Some(path) = native_lib::find_app_root_library("rxaac") {
        return Some(path);
    }
    let release_root = native_lib::source_root().join("deps/libxaac/release");
    native_lib::find_native_library(&release_root, "rxaac")
}

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| {
        let path = find_library()?;
        unsafe {
            let lib = Library::new(&path).map_err(|e| log::warn!("rxaac: {} would not load: {e}", path.display())).ok()?;
            let version = *lib.get::<unsafe extern "C" fn() -> c_int>(b"rxaac_abi_version").ok()?;
            if version() != ABI_VERSION {
                log::warn!("rxaac: {} is ABI version {}, this build needs {ABI_VERSION}; rebuild it (deps/libxaac/scripts/build.sh)", path.display(), version());
                return None;
            }
            let open = *lib.get::<OpenFn>(b"rxaac_open").ok()?;
            let frame_samples = *lib.get::<FrameSamplesFn>(b"rxaac_frame_samples").ok()?;
            let config = *lib.get::<ConfigFn>(b"rxaac_config").ok()?;
            let encode = *lib.get::<EncodeFn>(b"rxaac_encode").ok()?;
            let close = *lib.get::<CloseFn>(b"rxaac_close").ok()?;
            Some(Api { _lib: lib, open, frame_samples, config, encode, close })
        }
    })
    .as_ref()
}

/// Whether the encoder library is present on this machine.
pub fn available() -> bool {
    api().is_some()
}

/// The sample rates AAC has an index for; anything else cannot be encoded.
pub const SAMPLE_RATES: [u32; 12] = [96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000];

/// One stream being encoded. 16-bit interleaved PCM in, access units out.
pub struct Encoder {
    api: &'static Api,
    handle: *mut std::ffi::c_void,
    channels: usize,
    frame_samples: usize,
    config: Vec<u8>,
}

// The handle is only ever used through `&mut self`, and libxaac keeps no thread-local state.
unsafe impl Send for Encoder {}

impl Encoder {
    /// `bitrate` in bits per second for the whole stream. Mono and stereo only: libxaac wants
    /// a channel layout for anything wider, and nothing here has one to give.
    pub fn new(sample_rate: u32, channels: u16, bitrate: u32, format: Format) -> Result<Self, String> {
        let api = api().ok_or_else(|| "the AAC encoder library (rxaac) is not built for this machine".to_string())?;
        if !(1..=2).contains(&channels) {
            return Err(format!("{channels} channels: only mono and stereo are encoded"));
        }
        if !SAMPLE_RATES.contains(&sample_rate) {
            return Err(format!("{sample_rate} Hz is not a sample rate AAC can carry"));
        }
        let mut error = 0i32;
        let handle = unsafe { (api.open)(sample_rate as i32, i32::from(channels), bitrate as i32, i32::from(format == Format::XheAac), DECLARED_LOUDNESS_LKFS, &mut error) };
        if handle.is_null() {
            return Err(format!("libxaac refused {} at {sample_rate} Hz, {channels} ch, {bitrate} bps (error {error:#x})", format.name()));
        }
        let frame_samples = unsafe { (api.frame_samples)(handle) } as usize;
        let mut len = 0i32;
        let ptr = unsafe { (api.config)(handle, &mut len) };
        let config = if ptr.is_null() || len <= 0 { Vec::new() } else { unsafe { std::slice::from_raw_parts(ptr, len as usize) }.to_vec() };
        let encoder = Self { api, handle, channels: usize::from(channels), frame_samples, config };
        if encoder.frame_samples == 0 || encoder.config.is_empty() {
            return Err("libxaac opened an encoder with no frame size or no stream header".to_string());
        }
        Ok(encoder)
    }

    /// Samples per channel one [`Self::encode`] call takes.
    pub fn frame_samples(&self) -> usize {
        self.frame_samples
    }

    /// The AudioSpecificConfig: what a player needs before the first access unit.
    pub fn config(&self) -> &[u8] {
        &self.config
    }

    /// Encodes exactly one frame (`frame_samples * channels` samples). `None` while the
    /// encoder is still priming and has nothing to hand out yet.
    pub fn encode(&mut self, pcm: &[i16]) -> Result<Option<Vec<u8>>, String> {
        if pcm.len() != self.frame_samples * self.channels {
            return Err(format!("a frame is {} samples, got {}", self.frame_samples * self.channels, pcm.len()));
        }
        let mut out: *const u8 = std::ptr::null();
        let len = unsafe { (self.api.encode)(self.handle, pcm.as_ptr(), &mut out) };
        if len < 0 {
            return Err(format!("libxaac failed on a frame (error {len})"));
        }
        if len == 0 || out.is_null() {
            return Ok(None);
        }
        Ok(Some(unsafe { std::slice::from_raw_parts(out, len as usize) }.to_vec()))
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { (self.api.close)(self.handle) }
    }
}

/// A whole stream, encoded.
pub struct Encoded {
    pub frames: Vec<Vec<u8>>,
    pub config: Vec<u8>,
    pub frame_samples: u32,
}

/// Encodes a stream handed over in pieces of any size: [`Self::push`] as samples arrive,
/// [`Self::finish`] at the end. Only the compressed frames are kept, so the PCM never has to
/// exist whole.
pub struct StreamEncoder {
    encoder: Encoder,
    pending: Vec<i16>,
    per_frame: usize,
    frames: Vec<Vec<u8>>,
}

impl StreamEncoder {
    pub fn new(sample_rate: u32, channels: u16, bitrate: u32, format: Format) -> Result<Self, String> {
        let encoder = Encoder::new(sample_rate, channels, bitrate, format)?;
        let per_frame = encoder.frame_samples() * usize::from(channels);
        Ok(Self { encoder, pending: Vec::with_capacity(per_frame * 2), per_frame, frames: Vec::new() })
    }

    pub fn push(&mut self, pcm: &[i16]) -> Result<(), String> {
        self.pending.extend_from_slice(pcm);
        let mut at = 0;
        while self.pending.len() - at >= self.per_frame {
            self.frames.extend(self.encoder.encode(&self.pending[at..at + self.per_frame])?);
            at += self.per_frame;
        }
        self.pending.drain(..at);
        Ok(())
    }

    /// Pads the last partial frame with silence, then one more silent frame to push out what
    /// the encoder was still holding.
    pub fn finish(mut self) -> Result<Encoded, String> {
        if !self.pending.is_empty() {
            self.pending.resize(self.per_frame, 0);
            self.frames.extend(self.encoder.encode(&self.pending)?);
        }
        self.frames.extend(self.encoder.encode(&vec![0i16; self.per_frame])?);
        Ok(Encoded { frames: self.frames, config: self.encoder.config().to_vec(), frame_samples: self.encoder.frame_samples() as u32 })
    }
}

/// Encodes all of `pcm` (16-bit interleaved). The last partial frame is padded with silence,
/// and one more silent frame follows it to push out what the encoder was still holding.
pub fn encode_all(pcm: &[i16], sample_rate: u32, channels: u16, bitrate: u32, format: Format) -> Result<Encoded, String> {
    let mut stream = StreamEncoder::new(sample_rate, channels, bitrate, format)?;
    stream.push(pcm)?;
    stream.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A second of two sines, stereo, loud enough to need real bits.
    fn tone(sample_rate: u32, seconds: u32) -> Vec<i16> {
        (0..sample_rate * seconds)
            .flat_map(|n| {
                let t = n as f32 / sample_rate as f32;
                let l = (std::f32::consts::TAU * 440.0 * t).sin() * 0.4 + (std::f32::consts::TAU * 1320.0 * t).sin() * 0.2;
                let r = (std::f32::consts::TAU * 554.4 * t).sin() * 0.5;
                [(l * 30000.0) as i16, (r * 30000.0) as i16]
            })
            .collect()
    }

    #[test]
    fn format_names_round_trip() {
        for format in [Format::Aac, Format::XheAac] {
            assert_eq!(Format::from_name(format.name()), Some(format));
        }
        assert_eq!(Format::from_name("opus"), None);
    }

    #[test]
    fn both_formats_encode_a_stream_of_the_expected_length_and_size() {
        if !available() {
            eprintln!("skipped: rxaac is not built for this machine (deps/libxaac/scripts/build.sh)");
            return;
        }
        let pcm = tone(44_100, 4);
        for format in [Format::Aac, Format::XheAac] {
            let out = encode_all(&pcm, 44_100, 2, 96_000, format).unwrap();
            assert_eq!(out.frame_samples, 1024, "{format:?}");
            assert!(!out.config.is_empty(), "{format:?}");
            // 4 s = 172.3 frames; plus the flush frame, minus what priming swallowed.
            assert!((171..=175).contains(&out.frames.len()), "{format:?}: {} frames", out.frames.len());
            let bytes: usize = out.frames.iter().map(Vec::len).sum();
            let kbps = bytes as f64 * 8.0 / 4.0 / 1000.0;
            assert!((20.0..=130.0).contains(&kbps), "{format:?}: {kbps:.0} kbps for 96 asked");
        }
        // AAC-LC, 44.1 kHz, stereo: object type 2, frequency index 4, channel configuration 2.
        assert_eq!(encode_all(&pcm, 44_100, 2, 96_000, Format::Aac).unwrap().config, [0x12, 0x10]);
    }

    #[test]
    fn what_cannot_be_encoded_is_refused_not_crashed_on() {
        if !available() {
            return;
        }
        assert!(Encoder::new(44_100, 6, 128_000, Format::Aac).is_err());
        assert!(Encoder::new(44_000, 2, 128_000, Format::Aac).is_err());
        let mut encoder = Encoder::new(48_000, 1, 64_000, Format::XheAac).unwrap();
        assert!(encoder.encode(&[0i16; 100]).is_err(), "a short frame");
        assert!(encoder.encode(&vec![0i16; encoder.frame_samples()]).is_ok());
    }
}
