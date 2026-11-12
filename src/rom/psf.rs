//! The PSF family of rip files: one track per file, a console's own code and data cut out of
//! the game, played by emulating the console's sound hardware, all in the `rommusic` library:
//!
//! | console | files | player |
//! |---|---|---|
//! | N64 | `usf`, `miniusf`, `usflib` | lazyusf2 (`rusf_`, `deps/rommusic/usf/rusf.c`) |
//! | PS1 | `psf`, `minipsf`, `psflib` | Highly Experimental (`rpsx_`, `deps/rommusic/psx/rpsx.c`) |
//! | PS2 | `psf2`, `minipsf2`, `psf2lib` | the same |
//! | Saturn | `ssf`, `minissf`, `ssflib` | Highly Theoretical (`rsega_`, `deps/rommusic/sega/rsega.c`) |
//! | Dreamcast | `dsf`, `minidsf`, `dsflib` | the same |
//! | GBA | `gsf`, `minigsf`, `gsflib` | mGBA's GBA core (`rgsf_`, `deps/rommusic/gsf/rgsf.c`) |
//!
//! Highly Experimental runs on a kernel built from a real PS2 BIOS, which never ships with
//! romexplorer: [`set_ps2_bios`] takes one from the user's own files. Without it PS1 and PS2
//! rips are not tracks at all, so an index skips them.
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

/// Track files, by extension, and the console each is for.
const TRACK_EXTENSIONS: &[(&str, System)] = &[
    ("usf", System::N64),
    ("miniusf", System::N64),
    ("psf", System::Ps1),
    ("minipsf", System::Ps1),
    ("psf2", System::Ps2),
    ("minipsf2", System::Ps2),
    ("ssf", System::Saturn),
    ("minissf", System::Saturn),
    ("dsf", System::Dreamcast),
    ("minidsf", System::Dreamcast),
    ("gsf", System::Gba),
    ("minigsf", System::Gba),
];
/// Library files, which are not tracks themselves.
const LIB_EXTENSIONS: &[&str] = &["usflib", "psflib", "psf2lib", "ssflib", "dsflib", "gsflib"];
/// Nothing in this family is larger.
const MAX_FILE_BYTES: usize = 32 << 20;
/// How deep `_lib` files may name further libraries.
const MAX_DEPTH: usize = 10;

fn ext_of(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

/// Whether `name` is a track of this family that a player here can play.
pub fn is_track_file(name: &str) -> bool {
    let ext = ext_of(name);
    TRACK_EXTENSIONS.iter().any(|(e, system)| *e == ext && system.playable())
}

/// Whether `name` is a library file of this family: needed by tracks, not a track itself.
pub fn is_lib_file(name: &str) -> bool {
    LIB_EXTENSIONS.contains(&ext_of(name).as_str())
}

/// Which console a file is for, from its version byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum System {
    N64,
    Ps1,
    Ps2,
    Saturn,
    Dreamcast,
    Gba,
}

impl System {
    fn of(version: u8) -> Option<Self> {
        match version {
            0x01 => Some(System::Ps1),
            0x02 => Some(System::Ps2),
            0x11 => Some(System::Saturn),
            0x12 => Some(System::Dreamcast),
            0x21 => Some(System::N64),
            0x22 => Some(System::Gba),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            System::N64 => "Nintendo 64",
            System::Ps1 => "PlayStation",
            System::Ps2 => "PlayStation 2",
            System::Saturn => "Saturn",
            System::Dreamcast => "Dreamcast",
            System::Gba => "Game Boy Advance",
        }
    }

    pub fn codec(self) -> &'static str {
        match self {
            System::N64 => "USF",
            System::Ps1 => "PSF",
            System::Ps2 => "PSF2",
            System::Saturn => "SSF",
            System::Dreamcast => "DSF",
            System::Gba => "GSF",
        }
    }

    /// Whether this machine can play it now: the player is installed and, for PS1 and PS2,
    /// a BIOS has been given.
    pub fn playable(self) -> bool {
        match self {
            System::N64 => usf_api().is_some(),
            System::Ps1 | System::Ps2 => psx_api().is_some_and(|api| unsafe { (api.bios_ready)() } != 0),
            System::Saturn | System::Dreamcast => sega_api().is_some(),
            System::Gba => gsf_api().is_some(),
        }
    }

    /// The rate the console's sound chip runs at, which its player renders at.
    fn native_rate(self) -> Option<u32> {
        match self {
            System::N64 => None,
            System::Ps1 | System::Saturn | System::Dreamcast | System::Gba => Some(44_100),
            System::Ps2 => Some(48_000),
        }
    }
}

/// One file of the family, read.
#[derive(Debug, Clone, Default)]
struct PsfFile {
    version: u8,
    reserved: Vec<u8>,
    /// The unpacked program area: a PS-X EXE for PS1, a load address and sound RAM bytes
    /// for Saturn and Dreamcast, an entry point, offset and size and the cartridge bytes for
    /// GBA. USF and PSF2 keep everything in `reserved`.
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
pub(crate) fn parse_time(text: &str) -> Option<u32> {
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

/// The files of a PSF2's file system (its reserved area), by path with `/` between parts.
///
/// A directory is a count and 48-byte entries: a 36-byte name, then the offset, the unpacked
/// size and the block size. An entry with only an offset is a subdirectory at that offset;
/// a file's offset leads to its blocks' packed sizes and then the zlib blocks themselves.
fn psf2_files(reserved: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    const CORRUPT: &str = "the PSF2 file system is damaged";
    fn dir(fs: &[u8], at: usize, prefix: &str, depth: usize, out: &mut Vec<(String, Vec<u8>)>) -> Result<(), String> {
        if depth > 16 {
            return Err(CORRUPT.into());
        }
        let count = u32_at(fs, at).ok_or(CORRUPT)? as usize;
        let mut entry = at + 4;
        for _ in 0..count {
            let raw = fs.get(entry..entry + 48).ok_or(CORRUPT)?;
            let name_len = raw[..36].iter().position(|&b| b == 0).unwrap_or(36);
            let name = String::from_utf8_lossy(&raw[..name_len]).to_string();
            let (offset, size, block) = (u32_at(raw, 36).unwrap_or(0) as usize, u32_at(raw, 40).unwrap_or(0) as usize, u32_at(raw, 44).unwrap_or(0) as usize);
            entry += 48;
            let path = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
            if size == 0 && block == 0 && offset != 0 {
                if offset < entry {
                    return Err(CORRUPT.into());
                }
                dir(fs, offset, &path, depth + 1, out)?;
            } else if size == 0 || block == 0 || offset == 0 {
                out.push((path, Vec::new()));
            } else {
                if size > MAX_FILE_BYTES {
                    return Err(CORRUPT.into());
                }
                let blocks = size.div_ceil(block);
                let mut data = Vec::with_capacity(size);
                let mut packed = offset.checked_add(4 * blocks).ok_or(CORRUPT)?;
                for b in 0..blocks {
                    let len = u32_at(fs, offset + 4 * b).ok_or(CORRUPT)? as usize;
                    let chunk = fs.get(packed..packed.checked_add(len).ok_or(CORRUPT)?).ok_or(CORRUPT)?;
                    let want = block.min(size - data.len());
                    let unpacked = zlib_ng::zlib_inflate(chunk, want).filter(|u| u.len() == want).ok_or(CORRUPT)?;
                    data.extend_from_slice(&unpacked);
                    packed += len;
                }
                out.push((path, data));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    if !reserved.is_empty() {
        dir(reserved, 0, "", 0, &mut out)?;
    }
    Ok(out)
}

/// The cartridge image a GSF's sections make: each program area is an entry point, an
/// offset into the cartridge, a size and that many bytes, laid in load order so a later one
/// overwrites an earlier one.
fn gsf_image(sections: &[PsfFile]) -> Result<Vec<u8>, String> {
    const DAMAGED: &str = "the rip's data is damaged";
    let mut image = Vec::new();
    for section in sections.iter().filter(|s| !s.program.is_empty()) {
        let p = &section.program;
        let offset = (u32_at(p, 4).ok_or(DAMAGED)? & 0x01FF_FFFF) as usize;
        let size = u32_at(p, 8).ok_or(DAMAGED)? as usize;
        let data = p.get(12..).ok_or(DAMAGED)?;
        let data = &data[..size.min(data.len())];
        let end = offset + data.len();
        if end > 0x0200_0000 {
            return Err(DAMAGED.into());
        }
        if image.len() < end {
            image.resize(end, 0);
        }
        image[offset..end].copy_from_slice(data);
    }
    if image.is_empty() {
        return Err("the rip holds no cartridge data".into());
    }
    Ok(image)
}

/// The cartridge bytes a GSF file carries itself (its program area past the entry point,
/// offset and size), for matching it to ROMs. `None` for anything else.
pub fn gsf_cart_bytes(data: &[u8]) -> Option<Vec<u8>> {
    let file = parse(data).ok()?;
    if System::of(file.version) != Some(System::Gba) {
        return None;
    }
    let size = u32_at(&file.program, 8)? as usize;
    let bytes = file.program.get(12..)?;
    Some(bytes[..size.min(bytes.len())].to_vec())
}

/// The ROM bytes a USF file carries in its reserved area (lazyusf2's little-endian word
/// order), for matching it to ROMs. `None` for anything else.
pub fn usf_rom_bytes(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 16 || &data[..4] != b"PSF\x21" {
        return None;
    }
    let len = u32_at(data, 4)? as usize;
    let reserved = data.get(16..16 + len)?;
    if u32_at(reserved, 0)? != 0x3436_5253 {
        return None;
    }
    let mut out = Vec::new();
    let mut at = 4;
    loop {
        let n = u32_at(reserved, at)? as usize;
        if n == 0 {
            break;
        }
        out.extend_from_slice(reserved.get(at + 8..at + 8 + n)?);
        at += 8 + n;
    }
    (!out.is_empty()).then_some(out)
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
        // Some sets name a library without its extension.
        let bytes = read_lib(name).or_else(|| LIB_EXTENSIONS.iter().find_map(|e| read_lib(&format!("{name}.{e}")))).ok_or_else(|| format!("its library {name} is missing"))?;
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
    /// A GBA file's sections in load order: each one's offset into the cartridge and its
    /// bytes. Empty for other consoles.
    /// An N64 file's reserved sections in load order (the deepest library first); empty for
    /// other consoles.
    pub fn usf_sections(&self) -> Vec<&[u8]> {
        if self.system != System::N64 {
            return Vec::new();
        }
        self.sections.iter().map(|s| s.reserved.as_slice()).filter(|r| !r.is_empty()).collect()
    }

    /// An N64 file's `_enablecompare` and `_enablefifofull` settings.
    pub fn usf_flags(&self) -> (bool, bool) {
        (self.flag("_enablecompare"), self.flag("_enablefifofull"))
    }

    pub fn gba_sections(&self) -> Vec<(usize, &[u8])> {
        if self.system != System::Gba {
            return Vec::new();
        }
        self.sections
            .iter()
            .filter_map(|s| {
                let p = &s.program;
                let offset = (u32_at(p, 4)? & 0x01FF_FFFF) as usize;
                let size = u32_at(p, 8)? as usize;
                let data = p.get(12..)?;
                Some((offset, &data[..size.min(data.len())]))
            })
            .filter(|(o, d)| !d.is_empty() && o + d.len() <= 0x0200_0000)
            .collect()
    }

    fn flag(&self, name: &str) -> bool {
        self.flags.iter().any(|(k, v)| k.eq_ignore_ascii_case(name) && v.trim() != "0")
    }

    /// What the top file's tags say, in the shape the rest of the rip-set code reads.
    /// The `_refresh` tag (50 or 60), which overrides what a PS-X EXE header names.
    fn refresh(&self) -> Option<i32> {
        self.flags.iter().rev().find(|(k, _)| k.eq_ignore_ascii_case("_refresh")).and_then(|(_, v)| v.trim().parse().ok())
    }

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

type BiosFn = unsafe extern "C" fn(*const u8, usize) -> i32;
type ReadyFn = unsafe extern "C" fn() -> i32;
type PsxNewFn = unsafe extern "C" fn(i32) -> *mut c_void;
type ExeFn = unsafe extern "C" fn(*mut c_void, *const u8, usize) -> i32;
type AddFileFn = unsafe extern "C" fn(*mut c_void, *const c_char, *const u8, usize) -> i32;
type StartFn = unsafe extern "C" fn(*mut c_void, i32);
type PsxRenderFn = unsafe extern "C" fn(*mut c_void, *mut i16, usize) -> i32;

struct PsxApi {
    _lib: Library,
    set_bios: BiosFn,
    bios_ready: ReadyFn,
    new: PsxNewFn,
    upload_exe: ExeFn,
    add_file: AddFileFn,
    start: StartFn,
    render: PsxRenderFn,
    free: FreeFn,
}

fn psx_api() -> Option<&'static PsxApi> {
    static API: OnceLock<Option<PsxApi>> = OnceLock::new();
    API.get_or_init(|| {
        let path = native_lib::rommusic_library()?;
        unsafe {
            let lib = Library::new(&path).ok()?;
            Some(PsxApi {
                set_bios: *lib.get(b"rpsx_set_bios\0").ok()?,
                bios_ready: *lib.get(b"rpsx_bios_ready\0").ok()?,
                new: *lib.get(b"rpsx_new\0").ok()?,
                upload_exe: *lib.get(b"rpsx_upload_exe\0").ok()?,
                add_file: *lib.get(b"rpsx_add_file\0").ok()?,
                start: *lib.get(b"rpsx_start\0").ok()?,
                render: *lib.get(b"rpsx_render\0").ok()?,
                free: *lib.get(b"rpsx_free\0").ok()?,
                _lib: lib,
            })
        }
    })
    .as_ref()
}

type SegaNewFn = unsafe extern "C" fn(i32) -> *mut c_void;
type SegaUploadFn = unsafe extern "C" fn(*mut c_void, *const u8, usize) -> i32;
type SegaRenderFn = unsafe extern "C" fn(*mut c_void, *mut i16, usize) -> i32;

struct SegaApi {
    _lib: Library,
    new: SegaNewFn,
    upload: SegaUploadFn,
    render: SegaRenderFn,
    free: FreeFn,
}

fn sega_api() -> Option<&'static SegaApi> {
    static API: OnceLock<Option<SegaApi>> = OnceLock::new();
    API.get_or_init(|| {
        let path = native_lib::rommusic_library()?;
        unsafe {
            let lib = Library::new(&path).ok()?;
            Some(SegaApi {
                new: *lib.get(b"rsega_new\0").ok()?,
                upload: *lib.get(b"rsega_upload\0").ok()?,
                render: *lib.get(b"rsega_render\0").ok()?,
                free: *lib.get(b"rsega_free\0").ok()?,
                _lib: lib,
            })
        }
    })
    .as_ref()
}

type GsfNewFn = unsafe extern "C" fn(*const u8, usize) -> *mut c_void;
type GsfRenderFn = unsafe extern "C" fn(*mut c_void, *mut i16, usize) -> i32;

struct GsfApi {
    _lib: Library,
    new: GsfNewFn,
    render: GsfRenderFn,
    free: FreeFn,
}

fn gsf_api() -> Option<&'static GsfApi> {
    static API: OnceLock<Option<GsfApi>> = OnceLock::new();
    API.get_or_init(|| {
        let path = native_lib::rommusic_library()?;
        unsafe {
            let lib = Library::new(&path).ok()?;
            Some(GsfApi {
                new: *lib.get(b"rgsf_new\0").ok()?,
                render: *lib.get(b"rgsf_render\0").ok()?,
                free: *lib.get(b"rgsf_free\0").ok()?,
                _lib: lib,
            })
        }
    })
    .as_ref()
}

/// A GBA cartridge image played as it is (a ROM with a music map's bytes laid over it), by
/// the same mGBA core the GSF files play on, at 44,100 Hz stereo.
pub struct GbaImage {
    state: *mut c_void,
}

// The state is only used through `&mut self`.
unsafe impl Send for GbaImage {}

impl GbaImage {
    pub const RATE: u32 = 44_100;

    pub fn open(image: &[u8]) -> Result<Self, String> {
        let api = gsf_api().ok_or("the GBA music player (rommusic) is not installed")?;
        let state = unsafe { (api.new)(image.as_ptr(), image.len()) };
        if state.is_null() {
            return Err("the GBA player would not start this cartridge".into());
        }
        Ok(GbaImage { state })
    }

    /// Fills `out` (interleaved stereo). An error when a minute passed with no sound.
    pub fn render(&mut self, out: &mut [i16]) -> Result<(), String> {
        let api = gsf_api().ok_or("the GBA music player (rommusic) is not installed")?;
        if unsafe { (api.render)(self.state, out.as_mut_ptr(), out.len() / 2) } < 0 {
            return Err("the music stopped making sound".into());
        }
        Ok(())
    }
}

impl Drop for GbaImage {
    fn drop(&mut self) {
        if let Some(api) = gsf_api() {
            unsafe { (api.free)(self.state) };
        }
    }
}

/// An N64 song from reserved sections built elsewhere (a music map's rebuilt ROM parts and
/// save state), played on the same lazyusf2 core as a USF.
pub struct UsfImage {
    state: *mut c_void,
}

// The state is only used through `&mut self`.
unsafe impl Send for UsfImage {}

impl UsfImage {
    pub fn open(sections: &[&[u8]], compare: bool, fifo_full: bool) -> Result<Self, String> {
        let api = usf_api().ok_or("the N64 music player (rommusic) is not installed")?;
        let state = unsafe { (api.new)() };
        if state.is_null() {
            return Err("out of memory".into());
        }
        let me = UsfImage { state };
        for s in sections {
            if unsafe { (api.upload)(state, s.as_ptr(), s.len()) } < 0 {
                return Err("the N64 song's data is damaged".into());
            }
        }
        unsafe { (api.flags)(state, i32::from(compare), i32::from(fifo_full), 1) };
        Ok(me)
    }

    /// Fills `out` (interleaved stereo at `rate`).
    pub fn render(&mut self, out: &mut [i16], rate: u32) -> Result<(), String> {
        let api = usf_api().ok_or("the N64 music player (rommusic) is not installed")?;
        let mut err = [0 as c_char; 256];
        let r = unsafe { (api.render)(self.state, out.as_mut_ptr(), out.len() / 2, rate as i32, err.as_mut_ptr(), err.len()) };
        if r != 0 {
            return Err(unsafe { CStr::from_ptr(err.as_ptr()) }.to_string_lossy().into_owned());
        }
        Ok(())
    }
}

impl Drop for UsfImage {
    fn drop(&mut self) {
        if let Some(api) = usf_api() {
            unsafe { (api.free)(self.state) };
        }
    }
}

/// Whether the N64 player is installed.
pub fn usf_available() -> bool {
    usf_api().is_some()
}

/// Whether the GBA player is installed.
pub fn gba_available() -> bool {
    gsf_api().is_some()
}

/// The size of a PS2 BIOS image, the only kind the PS1/PS2 player can build its kernel from.
pub const PS2_BIOS_BYTES: usize = 0x40_0000;

/// Hands the PS1/PS2 player a PS2 BIOS image from the user's own files. The first one that
/// works stays for the life of the process. `Ok(false)` when the player is not installed.
pub fn set_ps2_bios(image: &[u8]) -> Result<bool, String> {
    let Some(api) = psx_api() else { return Ok(false) };
    if image.len() != PS2_BIOS_BYTES {
        return Err("a PS2 BIOS image is 4 MB".into());
    }
    match unsafe { (api.set_bios)(image.as_ptr(), image.len()) } {
        0 => Ok(true),
        _ => Err("this PS2 BIOS image could not be used".into()),
    }
}

/// Whether the PS1/PS2 player has a BIOS.
pub fn has_ps2_bios() -> bool {
    System::Ps1.playable()
}

/// Whether the PS1/PS2 player is installed, BIOS or not.
pub fn psx_installed() -> bool {
    psx_api().is_some()
}

/// Whether any player of this family is in this machine's `rommusic` library.
pub fn available() -> bool {
    usf_api().is_some() || psx_api().is_some() || sega_api().is_some() || gsf_api().is_some()
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

    /// `rate` is for players that resample; the PS1 and PS2 players render at the chip's own.
    pub fn open(loaded: Loaded, rate: u32) -> Result<Self, String> {
        let rate = loaded.system.native_rate().unwrap_or(rate);
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
            System::Ps1 | System::Ps2 => {
                let api = psx_api().ok_or("the PlayStation music player (rommusic) is not installed")?;
                let ps2 = self.loaded.system == System::Ps2;
                let state = unsafe { (api.new)(if ps2 { 2 } else { 1 }) };
                if state.is_null() {
                    return Err("PlayStation music needs a PS2 BIOS in your collection".into());
                }
                self.state = state;
                let mut refresh = 0;
                for section in &self.loaded.sections {
                    if ps2 {
                        for (path, data) in psf2_files(&section.reserved)? {
                            let path = std::ffi::CString::new(path).map_err(|_| "the PSF2 file system is damaged")?;
                            if unsafe { (api.add_file)(state, path.as_ptr(), data.as_ptr(), data.len()) } < 0 {
                                return Err("out of memory".into());
                            }
                        }
                    } else if !section.program.is_empty() {
                        match unsafe { (api.upload_exe)(state, section.program.as_ptr(), section.program.len()) } {
                            r if r < 0 => return Err("the rip's data is damaged".into()),
                            0 => {}
                            r if refresh == 0 => refresh = r,
                            _ => {}
                        }
                    }
                }
                unsafe { (api.start)(state, self.loaded.refresh().unwrap_or(refresh)) };
            }
            System::Saturn | System::Dreamcast => {
                let api = sega_api().ok_or("the Saturn/Dreamcast music player (rommusic) is not installed")?;
                let state = unsafe { (api.new)(if self.loaded.system == System::Saturn { 1 } else { 2 }) };
                if state.is_null() {
                    return Err("out of memory".into());
                }
                self.state = state;
                for section in &self.loaded.sections {
                    if !section.program.is_empty() && unsafe { (api.upload)(state, section.program.as_ptr(), section.program.len()) } < 0 {
                        return Err("the rip's data is damaged".into());
                    }
                }
            }
            System::Gba => {
                let api = gsf_api().ok_or("the GBA music player (rommusic) is not installed")?;
                let image = gsf_image(&self.loaded.sections)?;
                let state = unsafe { (api.new)(image.as_ptr(), image.len()) };
                if state.is_null() {
                    return Err("the GBA player would not start this rip".into());
                }
                self.state = state;
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
            System::Ps1 | System::Ps2 => {
                let api = psx_api().ok_or("the PlayStation music player (rommusic) is not installed")?;
                // 1: the program stopped, and the rest is silence.
                if unsafe { (api.render)(self.state, out.as_mut_ptr(), out.len() / 2) } < 0 {
                    return Err("the PlayStation player stopped on an error".into());
                }
            }
            System::Saturn | System::Dreamcast => {
                let api = sega_api().ok_or("the Saturn/Dreamcast music player (rommusic) is not installed")?;
                if unsafe { (api.render)(self.state, out.as_mut_ptr(), out.len() / 2) } < 0 {
                    return Err("the Saturn/Dreamcast player stopped on an error".into());
                }
            }
            System::Gba => {
                let api = gsf_api().ok_or("the GBA music player (rommusic) is not installed")?;
                if unsafe { (api.render)(self.state, out.as_mut_ptr(), out.len() / 2) } < 0 {
                    return Err("the music stopped making sound".into());
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
            System::Ps1 | System::Ps2 => {
                if let Some(api) = psx_api() {
                    unsafe { (api.free)(self.state) };
                }
            }
            System::Saturn | System::Dreamcast => {
                if let Some(api) = sega_api() {
                    unsafe { (api.free)(self.state) };
                }
            }
            System::Gba => {
                if let Some(api) = gsf_api() {
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

    /// A zlib stream of one stored (unpacked) block.
    fn stored(data: &[u8]) -> Vec<u8> {
        let mut v = vec![0x78, 0x01, 0x01];
        v.extend_from_slice(&(data.len() as u16).to_le_bytes());
        v.extend_from_slice(&(!(data.len() as u16)).to_le_bytes());
        v.extend_from_slice(data);
        let (mut a, mut b) = (1u32, 0u32);
        for &x in data {
            a = (a + u32::from(x)) % 65_521;
            b = (b + a) % 65_521;
        }
        v.extend_from_slice(&((b << 16) | a).to_be_bytes());
        v
    }

    fn entry(name: &str, offset: u32, size: u32, block: u32) -> Vec<u8> {
        let mut v = name.as_bytes().to_vec();
        v.resize(36, 0);
        for n in [offset, size, block] {
            v.extend_from_slice(&n.to_le_bytes());
        }
        v
    }

    #[test]
    fn a_psf2_file_system_unpacks_into_paths() {
        if !zlib_ng::available() {
            return;
        }
        // Root: "PSF2.IRX" (two blocks of 4) and "sub/"; sub: "a.bin".
        let file = b"0123456789";
        let (b1, b2, b3) = (stored(&file[..4]), stored(&file[4..8]), stored(&file[8..]));
        let root_len = 4 + 2 * 48;
        let file_at = root_len;
        let blocks_at = file_at + 3 * 4;
        let sub_at = blocks_at + b1.len() + b2.len() + b3.len();
        let a = stored(b"A!");
        let a_at = sub_at + 4 + 48;
        let mut fs = 2u32.to_le_bytes().to_vec();
        fs.extend(entry("PSF2.IRX", file_at as u32, 10, 4));
        fs.extend(entry("sub", sub_at as u32, 0, 0));
        for b in [&b1, &b2, &b3] {
            fs.extend_from_slice(&(b.len() as u32).to_le_bytes());
        }
        for b in [&b1, &b2, &b3] {
            fs.extend_from_slice(b);
        }
        fs.extend_from_slice(&1u32.to_le_bytes());
        fs.extend(entry("a.bin", a_at as u32 , 2, 0x8000));
        fs.extend_from_slice(&(a.len() as u32).to_le_bytes());
        fs.extend_from_slice(&a);
        let files = psf2_files(&fs).unwrap();
        assert_eq!(files, vec![("PSF2.IRX".to_string(), file.to_vec()), ("sub/a.bin".to_string(), b"A!".to_vec())]);
        assert!(psf2_files(&fs[..fs.len() - 3]).is_err(), "a cut-off block");
    }

    #[test]
    fn gsf_sections_overlay_into_one_cartridge() {
        let section = |offset: u32, data: &[u8]| {
            let mut p = 0x0800_0000u32.to_le_bytes().to_vec();
            p.extend_from_slice(&offset.to_le_bytes());
            p.extend_from_slice(&(data.len() as u32).to_le_bytes());
            p.extend_from_slice(data);
            PsfFile { version: 0x22, program: p, ..Default::default() }
        };
        let image = gsf_image(&[section(0x0800_0000, b"ABCDEF"), section(0x0800_0002, b"xy")]).unwrap();
        assert_eq!(image, b"ABxyEF");
        assert!(gsf_image(&[]).is_err());
    }

    #[test]
    fn ps1_and_ps2_rips_are_their_own_consoles() {
        let top = psf(0x02, b"", "_lib=x.psf2lib\n_refresh=50\n");
        let lib = psf(0x02, b"", "");
        let loaded = load(&top, &mut |_| Some(lib.clone())).unwrap();
        assert_eq!((loaded.system, loaded.refresh()), (System::Ps2, Some(50)));
        assert_eq!(System::of(0x01), Some(System::Ps1));
        assert!(is_lib_file("Set.PSF2LIB") && is_lib_file("a.psflib"));
    }

    #[test]
    fn a_tag_given_on_several_lines_is_one_value() {
        let file = parse(&psf(0x21, b"", "comment=one\ncomment=two\ntitle=T\n")).unwrap();
        assert_eq!(file.tag("comment"), Some("one\ntwo"));
        assert_eq!(file.tag("TITLE"), Some("T"));
    }
}
