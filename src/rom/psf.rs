//! The PSF family of rip files: one track per file, a console's own code and data cut out of
//! the game, played by emulating the console's sound hardware. So far N64 (USF) through
//! lazyusf2, built into the `rommusic` library as `rusf_` (`deps/rommusic/usf/rusf.c`).
//!
//! A file is `PSF`, a version byte saying which console, a reserved area, a zlib-packed
//! program area, and an optional `[TAG]` text block of `name=value` lines. A `minixxx` file
//! names the shared `xxxlib` files it builds on in `_lib`, `_lib2`, ... tags; they load in the
//! order psflib does it: `_lib` (and its own libraries) first, then the file, then `_lib2` on.

use std::ffi::{c_char, c_void, CStr};
use std::sync::OnceLock;

use libloading::Library;

use super::gme::TrackInfo;
use super::{native_lib, zlib_ng};

/// Track files, by extension.
const TRACK_EXTENSIONS: &[&str] = &["usf", "miniusf"];
/// Library files, which are not tracks themselves.
const LIB_EXTENSIONS: &[&str] = &["usflib"];
/// Nothing in this family is larger.
const MAX_FILE_BYTES: usize = 32 << 20;
/// How deep `_lib` files may name further libraries.
const MAX_DEPTH: usize = 10;

fn ext_of(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

/// Whether `name` is a track of this family that a player here can play.
pub fn is_track_file(name: &str) -> bool {
    TRACK_EXTENSIONS.contains(&ext_of(name).as_str()) && available()
}

/// Whether `name` is a library file of this family: needed by tracks, not a track itself.
pub fn is_lib_file(name: &str) -> bool {
    LIB_EXTENSIONS.contains(&ext_of(name).as_str())
}

/// Which console a file is for, from its version byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum System {
    N64,
}

impl System {
    fn of(version: u8) -> Option<Self> {
        match version {
            0x21 => Some(System::N64),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            System::N64 => "Nintendo 64",
        }
    }

    pub fn codec(self) -> &'static str {
        match self {
            System::N64 => "USF",
        }
    }
}

/// One file of the family, read.
#[derive(Debug, Clone, Default)]
struct PsfFile {
    version: u8,
    reserved: Vec<u8>,
    /// The unpacked program area: the code and data the PS1, PS2, Saturn and Dreamcast
    /// players load. USF keeps everything in `reserved`.
    #[allow(dead_code)]
    program: Vec<u8>,
    /// In file order; a name given twice is a value of several lines.
    tags: Vec<(String, String)>,
}

impl PsfFile {
    fn tag(&self, name: &str) -> Option<&str> {
        self.tags.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

fn parse(data: &[u8]) -> Result<PsfFile, String> {
    if data.len() < 16 || &data[..3] != b"PSF" {
        return Err("not a PSF-family file".into());
    }
    let reserved_len = u32_at(data, 4).unwrap_or(0) as usize;
    let program_len = u32_at(data, 8).unwrap_or(0) as usize;
    let reserved_end = 16usize.checked_add(reserved_len).filter(|&e| e <= data.len()).ok_or("the reserved area runs past the end")?;
    let program_end = reserved_end.checked_add(program_len).filter(|&e| e <= data.len()).ok_or("the program area runs past the end")?;
    let program = if program_len == 0 {
        Vec::new()
    } else {
        zlib_ng::zlib_inflate(&data[reserved_end..program_end], MAX_FILE_BYTES).ok_or("the program area would not unpack")?
    };
    let mut tags: Vec<(String, String)> = Vec::new();
    if let Some(text) = data[program_end..].strip_prefix(b"[TAG]") {
        let text = String::from_utf8_lossy(text);
        let mut last: Option<String> = None;
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim().to_string(), v.trim().to_string());
            if k.is_empty() {
                continue;
            }
            match tags.iter_mut().rev().find(|(name, _)| *name == k).filter(|_| last.as_deref() == Some(k.as_str())) {
                Some((_, value)) => {
                    value.push('\n');
                    value.push_str(&v);
                }
                None => tags.push((k.clone(), v)),
            }
            last = Some(k);
        }
    }
    Ok(PsfFile { version: data[3], reserved: data[16..reserved_end].to_vec(), program, tags })
}

/// A time tag (`length`, `fade`): `[[h:]m:]s[.fff]`, also with `,` for the decimal point.
fn parse_time(text: &str) -> Option<u32> {
    let text = text.trim().replace(',', ".");
    if text.is_empty() {
        return None;
    }
    let mut ms = 0f64;
    for part in text.split(':') {
        let v: f64 = part.trim().parse().ok()?;
        if !v.is_finite() || v < 0.0 {
            return None;
        }
        ms = ms * 60.0 + v;
    }
    Some((ms * 1000.0).round() as u32)
}

/// A file and the libraries it builds on, in load order, with the top file's tags.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub system: System,
    sections: Vec<PsfFile>,
    top: PsfFile,
    /// `_enable*` and similar settings from any file in the chain.
    flags: Vec<(String, String)>,
}

/// Loads `data` (the file named `name`) and every library it names, read through `read_lib`
/// by the name the tag gives.
pub fn load(data: &[u8], read_lib: &mut dyn FnMut(&str) -> Option<Vec<u8>>) -> Result<Loaded, String> {
    fn lib(name: &str, version: u8, depth: usize, read_lib: &mut dyn FnMut(&str) -> Option<Vec<u8>>, out: &mut Vec<PsfFile>, flags: &mut Vec<(String, String)>) -> Result<(), String> {
        let bytes = read_lib(name).ok_or_else(|| format!("its library {name} is missing"))?;
        let parsed = parse(&bytes).map_err(|e| format!("{name}: {e}"))?;
        if parsed.version != version {
            return Err(format!("{name} is for another console"));
        }
        walk(parsed, depth + 1, read_lib, out, flags)
    }
    fn walk(file: PsfFile, depth: usize, read_lib: &mut dyn FnMut(&str) -> Option<Vec<u8>>, out: &mut Vec<PsfFile>, flags: &mut Vec<(String, String)>) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return Err("libraries nest too deep".into());
        }
        if let Some(name) = file.tag("_lib").map(str::to_string) {
            lib(&name, file.version, depth, read_lib, out, flags)?;
        }
        let numbered: Vec<String> = (2..100).map_while(|n| file.tag(&format!("_lib{n}")).map(str::to_string)).collect();
        flags.extend(file.tags.iter().filter(|(k, _)| k.starts_with('_') && !k.to_ascii_lowercase().starts_with("_lib")).cloned());
        let version = file.version;
        out.push(file);
        for name in numbered {
            lib(&name, version, depth, read_lib, out, flags)?;
        }
        Ok(())
    }
    let top = parse(data)?;
    let system = System::of(top.version).ok_or_else(|| format!("PSF version {:#04x} is not played here", top.version))?;
    let mut sections = Vec::new();
    let mut flags = Vec::new();
    walk(top.clone(), 0, read_lib, &mut sections, &mut flags)?;
    Ok(Loaded { system, sections, top, flags })
}

impl Loaded {
    fn flag(&self, name: &str) -> bool {
        self.flags.iter().any(|(k, v)| k.eq_ignore_ascii_case(name) && v.trim() != "0")
    }

    /// What the top file's tags say, in the shape the rest of the rip-set code reads.
    pub fn info(&self) -> TrackInfo {
        let tag = |name: &str| self.top.tag(name).unwrap_or_default().to_string();
        TrackInfo {
            length_ms: self.top.tag("length").and_then(parse_time).filter(|&l| l > 0),
            fade_ms: self.top.tag("fade").and_then(parse_time),
            play_length_ms: self.top.tag("length").and_then(parse_time).unwrap_or(150_000),
            system: self.system.name().to_string(),
            game: tag("game"),
            song: tag("title"),
            author: tag("artist"),
            copyright: tag("copyright"),
            comment: tag("comment"),
            dumper: tag("psfby"),
            ..Default::default()
        }
    }
}

// --- the native players -----------------------------------------------------------------

type NewFn = unsafe extern "C" fn() -> *mut c_void;
type UploadFn = unsafe extern "C" fn(*mut c_void, *const u8, usize) -> i32;
type FlagsFn = unsafe extern "C" fn(*mut c_void, i32, i32, i32);
type RenderFn = unsafe extern "C" fn(*mut c_void, *mut i16, usize, i32, *mut c_char, usize) -> i32;
type FreeFn = unsafe extern "C" fn(*mut c_void);

struct UsfApi {
    _lib: Library,
    new: NewFn,
    upload: UploadFn,
    flags: FlagsFn,
    render: RenderFn,
    free: FreeFn,
}

fn usf_api() -> Option<&'static UsfApi> {
    static API: OnceLock<Option<UsfApi>> = OnceLock::new();
    API.get_or_init(|| {
        let path = native_lib::rommusic_library()?;
        unsafe {
            let lib = Library::new(&path).ok()?;
            Some(UsfApi {
                new: *lib.get(b"rusf_new\0").ok()?,
                upload: *lib.get(b"rusf_upload\0").ok()?,
                flags: *lib.get(b"rusf_set_flags\0").ok()?,
                render: *lib.get(b"rusf_render\0").ok()?,
                free: *lib.get(b"rusf_free\0").ok()?,
                _lib: lib,
            })
        }
    })
    .as_ref()
}

/// Whether any player of this family is in this machine's `rommusic` library.
pub fn available() -> bool {
    usf_api().is_some()
}

/// One track, playing.
pub struct Player {
    loaded: Loaded,
    state: *mut c_void,
    rate: u32,
}

// The state is only used through `&mut self`.
unsafe impl Send for Player {}

impl Player {
    pub const CHANNELS: u16 = 2;

    pub fn open(loaded: Loaded, rate: u32) -> Result<Self, String> {
        let mut player = Player { loaded, state: std::ptr::null_mut(), rate };
        player.restart()?;
        Ok(player)
    }

    pub fn sample_rate(&self) -> u32 {
        self.rate
    }

    pub fn info(&self) -> TrackInfo {
        self.loaded.info()
    }

    pub fn system(&self) -> System {
        self.loaded.system
    }

    /// Back to the start of the track.
    pub fn restart(&mut self) -> Result<(), String> {
        self.close();
        match self.loaded.system {
            System::N64 => {
                let api = usf_api().ok_or("the N64 music player (rommusic) is not installed")?;
                let state = unsafe { (api.new)() };
                if state.is_null() {
                    return Err("out of memory".into());
                }
                self.state = state;
                for section in &self.loaded.sections {
                    if !section.reserved.is_empty() && unsafe { (api.upload)(state, section.reserved.as_ptr(), section.reserved.len()) } < 0 {
                        return Err("the rip's data is damaged".into());
                    }
                }
                let (compare, fifo) = (self.loaded.flag("_enablecompare"), self.loaded.flag("_enablefifofull"));
                // The faster high-level audio, as most players default to.
                unsafe { (api.flags)(state, i32::from(compare), i32::from(fifo), 1) };
            }
        }
        Ok(())
    }

    /// Fills `out` (interleaved stereo).
    pub fn play(&mut self, out: &mut [i16]) -> Result<(), String> {
        match self.loaded.system {
            System::N64 => {
                let api = usf_api().ok_or("the N64 music player (rommusic) is not installed")?;
                let mut err = [0 as c_char; 256];
                let r = unsafe { (api.render)(self.state, out.as_mut_ptr(), out.len() / 2, self.rate as i32, err.as_mut_ptr(), err.len()) };
                if r != 0 {
                    return Err(unsafe { CStr::from_ptr(err.as_ptr()) }.to_string_lossy().into_owned());
                }
            }
        }
        Ok(())
    }

    fn close(&mut self) {
        if self.state.is_null() {
            return;
        }
        match self.loaded.system {
            System::N64 => {
                if let Some(api) = usf_api() {
                    unsafe { (api.free)(self.state) };
                }
            }
        }
        self.state = std::ptr::null_mut();
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

    fn psf(version: u8, reserved: &[u8], tags: &str) -> Vec<u8> {
        let mut v = b"PSF".to_vec();
        v.push(version);
        v.extend_from_slice(&(reserved.len() as u32).to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(reserved);
        if !tags.is_empty() {
            v.extend_from_slice(b"[TAG]");
            v.extend_from_slice(tags.as_bytes());
        }
        v
    }

    #[test]
    fn times_read_in_any_of_their_forms() {
        assert_eq!(parse_time("1:02.5"), Some(62_500));
        assert_eq!(parse_time("1:00:00"), Some(3_600_000));
        assert_eq!(parse_time("10"), Some(10_000));
        assert_eq!(parse_time("0,25"), Some(250));
        assert_eq!(parse_time(""), None);
        assert_eq!(parse_time("x"), None);
    }

    #[test]
    fn libraries_load_in_psflib_order() {
        let top = psf(0x21, b"top", "_lib=a.usflib\n_lib2=c.usflib\ntitle=Song\nlength=1:30\nfade=10\n");
        let a = psf(0x21, b"a", "_lib=b.usflib\n_enablecompare=1\n");
        let b = psf(0x21, b"b", "");
        let c = psf(0x21, b"c", "");
        let mut read = |name: &str| match name {
            "a.usflib" => Some(a.clone()),
            "b.usflib" => Some(b.clone()),
            "c.usflib" => Some(c.clone()),
            _ => None,
        };
        let loaded = load(&top, &mut read).unwrap();
        let order: Vec<&[u8]> = loaded.sections.iter().map(|s| s.reserved.as_slice()).collect();
        assert_eq!(order, [b"b".as_slice(), b"a", b"top", b"c"]);
        assert!(loaded.flag("_enablecompare"));
        assert!(!loaded.flag("_enablefifofull"));
        let info = loaded.info();
        assert_eq!((info.song.as_str(), info.length_ms, info.fade_ms), ("Song", Some(90_000), Some(10_000)));
    }

    #[test]
    fn a_missing_or_foreign_library_is_an_error() {
        let top = psf(0x21, b"", "_lib=gone.usflib\n");
        assert!(load(&top, &mut |_| None).unwrap_err().contains("gone.usflib"));
        let other = psf(0x01, b"", "");
        assert!(load(&top, &mut |_| Some(other.clone())).unwrap_err().contains("another console"));
    }

    #[test]
    fn a_tag_given_on_several_lines_is_one_value() {
        let file = parse(&psf(0x21, b"", "comment=one\ncomment=two\ntitle=T\n")).unwrap();
        assert_eq!(file.tag("comment"), Some("one\ntwo"));
        assert_eq!(file.tag("TITLE"), Some("T"));
    }
}
