//! PlayStation discs out of a PSP "PS1 classic" `EBOOT.PBP`, through `psxpbp`
//! (`deps/beetle-psx-libretro/psxpbp.h`): Beetle PSX's own PBP reader, RetroArch's PS1 core,
//! built as a library. Loaded at run time; when it is not built [`available`] is false and a
//! PBP stays an opaque file.
//!
//! A PBP keeps each disc as one run of raw 2352-byte sectors plus the disc's TOC, not the
//! track files a Redump dump has. [`disc_layout`] cuts that run back into Redump's shape -- a
//! cue sheet and one `.bin` per track, a two-second pregap at the start of every track after
//! the first -- so a scan can hash each track and match the DAT, and a copy can turn the disc
//! into a CHD. A single-track disc (most PS1 games) comes back exactly; for an audio track
//! the pregap is assumed, as the PBP does not record it, and Sony's own PSN PBPs keep their CD
//! audio as ATRAC3, which is not decoded (Beetle reads those discs as their data track only).
//!
//! [`DiscReader`] reads a disc's sectors by random access ([`super::source::ReadAt`]); every
//! call goes through one lock in the library, so it is safe from several threads but never
//! parallel.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use libloading::Library;

use super::native_lib;
use super::source::ReadAt;

pub const SECTOR: u64 = 2352;
/// The pregap a Redump track file starts with, for every track after the first.
const PREGAP: i64 = 150;
const ABI_VERSION: c_int = 1;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct RawTrack {
    number: c_int,
    audio: c_int,
    lba: i32,
    sectors: i32,
}

type OpenFn = unsafe extern "C" fn(*const c_char, *mut c_char, usize) -> *mut c_void;
type CloseFn = unsafe extern "C" fn(*mut c_void);
type CountFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type TracksFn = unsafe extern "C" fn(*mut c_void, c_int, *mut RawTrack, c_int, *mut i32) -> c_int;
type ReadFn = unsafe extern "C" fn(*mut c_void, c_int, i32, i32, *mut u8) -> c_int;

struct Api {
    _lib: Library,
    open: OpenFn,
    close: CloseFn,
    count: CountFn,
    tracks: TracksFn,
    read: ReadFn,
}

fn find_library() -> Option<PathBuf> {
    native_lib::find_app_root_library("psxpbp").or_else(|| native_lib::find_native_library(&native_lib::source_root().join("deps/beetle-psx-libretro/release"), "psxpbp"))
}

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| {
        let path = find_library()?;
        unsafe {
            let lib = Library::new(&path).map_err(|e| log::warn!("psxpbp: {} would not load: {e}", path.display())).ok()?;
            let version = *lib.get::<unsafe extern "C" fn() -> c_int>(b"psxpbp_abi_version").ok()?;
            if version() != ABI_VERSION {
                log::warn!("psxpbp: {} is ABI version {}, this build needs {ABI_VERSION}; rebuild it (deps/beetle-psx-libretro/scripts/build.sh)", path.display(), version());
                return None;
            }
            Some(Api {
                open: *lib.get::<OpenFn>(b"psxpbp_open").ok()?,
                close: *lib.get::<CloseFn>(b"psxpbp_close").ok()?,
                count: *lib.get::<CountFn>(b"psxpbp_disc_count").ok()?,
                tracks: *lib.get::<TracksFn>(b"psxpbp_tracks").ok()?,
                read: *lib.get::<ReadFn>(b"psxpbp_read").ok()?,
                _lib: lib,
            })
        }
    })
    .as_ref()
}

/// Whether the library is present on this machine.
pub fn available() -> bool {
    api().is_some()
}

/// Whether `head` (a file's first bytes) starts a PBP.
pub fn is_pbp(head: &[u8]) -> bool {
    head.len() >= 4 && &head[..4] == b"\0PBP"
}

/// Whether the PBP at `path` holds PlayStation discs: its PSAR section (offset at 0x24)
/// starts with `PSISOIMG` (one disc) or `PSTITLEIMG` (several). A PSP game, its DLC
/// (`PARAM.pbp`) or homebrew is a PBP too, with an empty PSAR or `NPUMDIMG`, and is no disc
/// to decode: it is hashed as the file it is.
pub fn holds_ps1_disc(path: &Path) -> bool {
    let read = || -> std::io::Result<bool> {
        let mut f = File::open(path)?;
        let mut header = [0u8; 0x28];
        f.read_exact(&mut header)?;
        if !is_pbp(&header) {
            return Ok(false);
        }
        let psar = u32::from_le_bytes([header[0x24], header[0x25], header[0x26], header[0x27]]) as u64;
        let mut sig = [0u8; 10];
        f.seek(SeekFrom::Start(psar))?;
        if f.read_exact(&mut sig).is_err() {
            return Ok(false);
        }
        Ok(sig.starts_with(b"PSISOIMG") || &sig == b"PSTITLEIMG")
    };
    read().unwrap_or(false)
}

struct Handle {
    api: &'static Api,
    raw: *mut c_void,
}

// The library serialises every call on every handle (psxpbp.h).
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { (self.api.close)(self.raw) };
    }
}

/// An opened PBP.
#[derive(Clone)]
pub struct Pbp {
    handle: Arc<Handle>,
    path: PathBuf,
}

/// One track as the PBP's TOC has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Track {
    pub number: u32,
    pub audio: bool,
    /// Its INDEX 01, as the reader counts sectors.
    pub lba: i64,
}

impl Pbp {
    pub fn open(path: &Path) -> Result<Self, String> {
        let api = api().ok_or("the PBP library (psxpbp) is not built for this machine")?;
        let c = CString::new(path.to_string_lossy().as_bytes()).map_err(|_| format!("{} has a NUL in its name", path.display()))?;
        let mut error = [0u8; 256];
        let raw = unsafe { (api.open)(c.as_ptr(), error.as_mut_ptr() as *mut c_char, error.len()) };
        if raw.is_null() {
            let message = CStr::from_bytes_until_nul(&error).ok().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            return Err(format!("{}: {}", path.display(), if message.is_empty() { "not a readable PlayStation PBP".into() } else { message }));
        }
        Ok(Self { handle: Arc::new(Handle { api, raw }), path: path.to_path_buf() })
    }

    pub fn disc_count(&self) -> usize {
        unsafe { (self.handle.api.count)(self.handle.raw) }.max(1) as usize
    }

    /// Disc `disc`'s tracks (0-based) and its lead-out.
    pub fn tracks(&self, disc: usize) -> Result<(Vec<Track>, i64), String> {
        let mut out = [RawTrack::default(); 99];
        let mut leadout = 0i32;
        let n = unsafe { (self.handle.api.tracks)(self.handle.raw, disc as c_int, out.as_mut_ptr(), out.len() as c_int, &mut leadout) };
        if n <= 0 {
            return Err(format!("{}: disc {}'s table of contents is not readable", self.path.display(), disc + 1));
        }
        // The library's count is trusted no further than the table it was given to fill.
        let tracks = out[..(n as usize).min(out.len())].iter().map(|t| Track { number: t.number as u32, audio: t.audio != 0, lba: t.lba as i64 }).collect();
        Ok((tracks, leadout as i64))
    }

    fn read(&self, disc: usize, lba: i64, buf: &mut [u8]) -> std::io::Result<()> {
        debug_assert_eq!(buf.len() as u64 % SECTOR, 0);
        let count = (buf.len() as u64 / SECTOR) as i32;
        let code = unsafe { (self.handle.api.read)(self.handle.raw, disc as c_int, lba as i32, count, buf.as_mut_ptr()) };
        if code != 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{}: disc {} sector {lba} would not read -- the PBP may be damaged", self.path.display(), disc + 1)));
        }
        Ok(())
    }
}

/// One Redump-shaped track file of a disc: its name, and where it lies in the disc's run of
/// sectors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackFile {
    pub name: String,
    pub number: u32,
    pub audio: bool,
    pub first_sector: u64,
    pub sectors: u64,
}

/// A disc as Redump would have it: the cue sheet and its track files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscLayout {
    pub disc: usize,
    pub cue_name: String,
    pub cue: String,
    pub tracks: Vec<TrackFile>,
}

impl DiscLayout {
    pub fn total_bytes(&self) -> u64 {
        self.tracks.iter().map(|t| t.sectors * SECTOR).sum()
    }
}

/// How many sectors a converted (unencrypted) PBP stores for disc `disc`, read from its block
/// index: blocks of 16 sectors, so this is the disc's length rounded up to 16. `None` for an
/// encrypted (official) PBP, whose index only the library can read.
fn stored_sectors(path: &Path, disc: usize) -> Option<u64> {
    let mut f = File::open(path).ok()?;
    let mut header = [0u8; 0x28];
    f.read_exact(&mut header).ok()?;
    let psar = u32::from_le_bytes(header[0x24..0x28].try_into().ok()?) as u64;
    let mut sig = [0u8; 12];
    f.seek(SeekFrom::Start(psar)).ok()?;
    f.read_exact(&mut sig).ok()?;
    let mut iso = psar;
    if &sig == b"PSTITLEIMG00" {
        let mut map = [0u8; 4 * 5];
        f.seek(SeekFrom::Start(psar + 0x200)).ok()?;
        f.read_exact(&mut map).ok()?;
        if &map[..4] == b"\0PGD" {
            return None;
        }
        iso += u32::from_le_bytes(map.get(disc * 4..disc * 4 + 4)?.try_into().ok()?) as u64;
    }
    let mut head = [0u8; 4];
    f.seek(SeekFrom::Start(iso + 0x400)).ok()?;
    f.read_exact(&mut head).ok()?;
    if &head == b"\0PGD" {
        return None;
    }
    let mut table = vec![0u8; 0xAFC80];
    f.seek(SeekFrom::Start(iso + 0x4000)).ok()?;
    f.read_exact(&mut table).ok()?;
    let blocks = table.chunks_exact(32).take_while(|e| u16::from_le_bytes([e[4], e[5]]) != 0).count() as u64;
    Some(blocks * 16)
}

/// The name a PBP's discs are written under: its own stem, or for an `EBOOT.PBP` -- the
/// name every one has -- its folder's.
pub fn output_stem(pbp: &Path) -> String {
    let stem = pbp.file_stem().and_then(|s| s.to_str()).unwrap_or("disc");
    if stem.eq_ignore_ascii_case("eboot") {
        if let Some(folder) = pbp.parent().and_then(|d| d.file_name()).and_then(|n| n.to_str()) {
            return folder.to_string();
        }
    }
    stem.to_string()
}

/// Disc `disc` of `pbp` cut into Redump's cue and track files, named after `stem` (with
/// `(Disc N)` when the PBP holds more than one disc, and `(Track N)` when the disc has more
/// than one track).
pub fn disc_layout(pbp: &Pbp, disc: usize, stem: &str) -> Result<DiscLayout, String> {
    let (tracks, leadout) = pbp.tracks(disc)?;
    let stem = if pbp.disc_count() > 1 { format!("{stem} (Disc {})", disc + 1) } else { stem.to_string() };
    // A TOC counts from the disc's absolute start (track 1 at 00:02:00) or from track 1. Which
    // one this PBP used shows in its length: the sectors it stores are the disc's, rounded up
    // to a block of 16.
    let bias = match stored_sectors(&pbp.path, disc) {
        Some(stored) if leadout - 150 > stored as i64 - 16 && leadout - 150 <= stored as i64 => 150,
        Some(_) => 0,
        None => tracks[0].lba,
    };
    let total = leadout - bias;
    if total <= 0 {
        return Err(format!("{}: disc {} has no sectors", pbp.path.display(), disc + 1));
    }
    let single = tracks.len() == 1;
    let mut starts: Vec<i64> = tracks.iter().enumerate().map(|(i, t)| if i == 0 { 0 } else { (t.lba - bias - PREGAP).max(0) }).collect();
    starts.push(total);
    let mut files = Vec::with_capacity(tracks.len());
    let mut cue = String::new();
    for (i, t) in tracks.iter().enumerate() {
        let (start, end) = (starts[i], starts[i + 1]);
        if end <= start {
            return Err(format!("{}: disc {}'s tracks overlap", pbp.path.display(), disc + 1));
        }
        let name = if single { format!("{stem}.bin") } else { format!("{stem} (Track {}).bin", t.number) };
        cue.push_str(&format!("FILE \"{name}\" BINARY\r\n  TRACK {:02} {}\r\n", t.number, if t.audio { "AUDIO" } else { "MODE2/2352" }));
        if i == 0 {
            cue.push_str("    INDEX 01 00:00:00\r\n");
        } else {
            cue.push_str("    INDEX 00 00:00:00\r\n    INDEX 01 00:02:00\r\n");
        }
        files.push(TrackFile { name, number: t.number, audio: t.audio, first_sector: start as u64, sectors: (end - start) as u64 });
    }
    Ok(DiscLayout { disc, cue_name: format!("{stem}.cue"), cue, tracks: files })
}

/// A disc's whole run of sectors, by byte offset, for [`super::parallel::parallel_decode`].
pub struct DiscReader {
    pbp: Pbp,
    disc: usize,
    size: u64,
    buf: Vec<u8>,
}

impl DiscReader {
    pub fn new(pbp: &Pbp, layout: &DiscLayout) -> Self {
        Self { pbp: pbp.clone(), disc: layout.disc, size: layout.total_bytes(), buf: Vec::new() }
    }
}

impl ReadAt for DiscReader {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        if offset + buf.len() as u64 > self.size {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "read past the end of the disc"));
        }
        if buf.is_empty() {
            return Ok(());
        }
        let first = offset / SECTOR;
        let last = (offset + buf.len() as u64 - 1) / SECTOR;
        let count = (last - first + 1) as usize;
        if offset % SECTOR == 0 && buf.len() as u64 % SECTOR == 0 {
            return self.pbp.read(self.disc, first as i64, buf);
        }
        self.buf.resize(count * SECTOR as usize, 0);
        let mut tmp = std::mem::take(&mut self.buf);
        let result = self.pbp.read(self.disc, first as i64, &mut tmp);
        let within = (offset % SECTOR) as usize;
        if result.is_ok() {
            buf.copy_from_slice(&tmp[within..within + buf.len()]);
        }
        self.buf = tmp;
        result
    }
}
