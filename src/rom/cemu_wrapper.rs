//! Wii U titles into Cemu's `.wua` archive, through `cemuwua` (`deps/cemu/cemuwua.h`): a thin
//! C API over Cemu's own disc, FST and ticket code (`external/cemu`) and the ZArchive writer
//! Cemu uses for `.wua`. Loaded at run time like [`super::xaac`]: when the library has not been
//! built for this machine [`available`] is false and a copy keeps the `.wud`/`.wux` as it is.
//!
//! A `.wua` holds a title's files decrypted, and nothing of the disc they came from, so it only
//! goes one way: no `.wud`/`.wux` can be rebuilt from one. And no DAT hashes a `.wua` -- Redump
//! hashes the encrypted disc, No-Intro the encrypted CDN files -- so every archive written here
//! carries `romexplorer.json` at its root: the SHA-1 of each file stored, plus the hashes of
//! the verified disc it was made from ([`Manifest`]). [`verify`] streams the archive back
//! against it, the way a CHD is checked against its own SHA-1. Cemu lists only root folders
//! named like a title, so it never sees the file.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use libloading::Library;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use super::native_lib;

/// The `CEMUWUA_ABI_VERSION` this module was written against.
const ABI_VERSION: c_int = 2;

/// The manifest's name at the archive root (`CEMUWUA_MANIFEST_NAME`).
pub const MANIFEST_NAME: &str = "romexplorer.json";

/// What went wrong, from `cemuwua_status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Cancelled,
    NoManifest,
    /// [`read_file`]: the archive has no file by that path.
    NotFound,
    /// A NULL or empty argument, or a buffer too small.
    Args,
    /// An input could not be opened or is not a Wii U title.
    Open,
    /// A disc image whose key is missing or wrong.
    Key,
    /// Reading or decrypting failed part way: a damaged image.
    Read,
    Write,
    /// The written archive did not read back.
    Verify,
    /// The library is not built for this machine.
    Unavailable,
}

impl Status {
    fn from_code(code: c_int) -> Option<Self> {
        Some(match code {
            0 => return None,
            1 => Self::Cancelled,
            2 => Self::NoManifest,
            3 => Self::NotFound,
            -1 => Self::Args,
            -2 => Self::Open,
            -3 => Self::Key,
            -4 => Self::Read,
            -5 => Self::Write,
            -6 => Self::Verify,
            _ => Self::Read,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Error {
    pub status: Status,
    pub message: String,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn unavailable() -> Error {
    Error { status: Status::Unavailable, message: "the Wii U archive library (cemuwua) is not built for this machine".into() }
}

#[repr(C)]
struct RawSource {
    path: *const c_char,
    key: *const u8,
}

type ProgressFn = unsafe extern "C" fn(u64, u64, *mut c_void) -> c_int;
type TitleFn = unsafe extern "C" fn(u64, u32, *mut c_void);
type FileFn = unsafe extern "C" fn(*const c_char, u64, *mut c_void) -> c_int;
type DataFn = unsafe extern "C" fn(*const u8, usize, *mut c_void) -> c_int;

type CreateFn = unsafe extern "C" fn(*const RawSource, usize, *const c_char, *const c_char, *const c_char, Option<ProgressFn>, *mut c_void, *mut c_char, usize) -> c_int;
type TitleInfoFn = unsafe extern "C" fn(*const c_char, *const u8, *const c_char, *mut u64, *mut u32, *mut c_char, usize) -> c_int;
type WuaTitlesFn = unsafe extern "C" fn(*const c_char, TitleFn, *mut c_void, *mut c_char, usize) -> c_int;
type WuaManifestFn = unsafe extern "C" fn(*const c_char, *mut c_char, usize, *mut usize, *mut c_char, usize) -> c_int;
type WuaStreamFn = unsafe extern "C" fn(*const c_char, FileFn, DataFn, *mut c_void, *mut c_char, usize) -> c_int;
type ZarCreateFn = unsafe extern "C" fn(*const c_char, *mut c_char, usize) -> *mut c_void;
type ZarPathFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int;
type ZarWriteFn = unsafe extern "C" fn(*mut c_void, *const u8, usize) -> c_int;
type ZarFinishFn = unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_char, usize) -> c_int;
type ZarAbortFn = unsafe extern "C" fn(*mut c_void);
type ReadFileFn = unsafe extern "C" fn(*const c_char, *const c_char, u64, *mut u8, usize, *mut usize, *mut u64, *mut c_char, usize) -> c_int;

struct Api {
    // Kept so the function pointers below stay valid.
    _lib: Library,
    create: CreateFn,
    title_info: TitleInfoFn,
    wua_titles: WuaTitlesFn,
    wua_manifest: WuaManifestFn,
    wua_stream: WuaStreamFn,
    zar_create: ZarCreateFn,
    zar_dir: ZarPathFn,
    zar_file: ZarPathFn,
    zar_write: ZarWriteFn,
    zar_finish: ZarFinishFn,
    zar_abort: ZarAbortFn,
    zar_stream: WuaStreamFn,
    /// Added after ABI version 2 without a bump: `None` from a library built before it.
    read_file: Option<ReadFileFn>,
}

fn find_library() -> Option<PathBuf> {
    if let Some(path) = native_lib::find_app_root_library("cemuwua") {
        return Some(path);
    }
    let release_root = native_lib::source_root().join("deps/cemu/release");
    native_lib::find_native_library(&release_root, "cemuwua")
}

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| {
        let path = find_library()?;
        unsafe {
            let lib = Library::new(&path).map_err(|e| log::warn!("cemuwua: {} would not load: {e}", path.display())).ok()?;
            let version = *lib.get::<unsafe extern "C" fn() -> c_int>(b"cemuwua_abi_version").ok()?;
            if version() != ABI_VERSION {
                log::warn!("cemuwua: {} is ABI version {}, this build needs {ABI_VERSION}; rebuild it (deps/cemu/scripts/build.sh)", path.display(), version());
                return None;
            }
            let create = *lib.get::<CreateFn>(b"cemuwua_create").ok()?;
            let title_info = *lib.get::<TitleInfoFn>(b"cemuwua_title_info").ok()?;
            let wua_titles = *lib.get::<WuaTitlesFn>(b"cemuwua_wua_titles").ok()?;
            let wua_manifest = *lib.get::<WuaManifestFn>(b"cemuwua_wua_manifest").ok()?;
            let wua_stream = *lib.get::<WuaStreamFn>(b"cemuwua_wua_stream").ok()?;
            let zar_create = *lib.get::<ZarCreateFn>(b"cemuwua_zar_create").ok()?;
            let zar_dir = *lib.get::<ZarPathFn>(b"cemuwua_zar_dir").ok()?;
            let zar_file = *lib.get::<ZarPathFn>(b"cemuwua_zar_file").ok()?;
            let zar_write = *lib.get::<ZarWriteFn>(b"cemuwua_zar_write").ok()?;
            let zar_finish = *lib.get::<ZarFinishFn>(b"cemuwua_zar_finish").ok()?;
            let zar_abort = *lib.get::<ZarAbortFn>(b"cemuwua_zar_abort").ok()?;
            let zar_stream = *lib.get::<WuaStreamFn>(b"cemuwua_zar_stream").ok()?;
            let read_file = lib.get::<ReadFileFn>(b"cemuwua_read_file").ok().map(|f| *f);
            Some(Api { _lib: lib, create, title_info, wua_titles, wua_manifest, wua_stream, zar_create, zar_dir, zar_file, zar_write, zar_finish, zar_abort, zar_stream, read_file })
        }
    })
    .as_ref()
}

/// Whether the library is present on this machine.
pub fn available() -> bool {
    api().is_some()
}

fn c_path(path: &Path) -> Result<CString> {
    CString::new(path.to_string_lossy().as_bytes()).map_err(|_| Error { status: Status::Args, message: format!("{} has a NUL in its name", path.display()) })
}

fn c_opt(path: Option<&Path>) -> Result<Option<CString>> {
    path.map(c_path).transpose()
}

const ERROR_LEN: usize = 512;

fn check(code: c_int, error: &[u8], what: &str) -> Result<()> {
    let Some(status) = Status::from_code(code) else { return Ok(()) };
    let message = CStr::from_bytes_until_nul(error).ok().map(|s| s.to_string_lossy().into_owned()).filter(|s| !s.is_empty());
    let message = message.unwrap_or_else(|| match status {
        Status::Cancelled => "cancelled".into(),
        Status::NoManifest => format!("{what} has no {MANIFEST_NAME}"),
        _ => format!("{what}: cemuwua error {code}"),
    });
    Err(Error { status, message })
}

/// One title to put into an archive (see `cemuwua_source`): a `.wud`/`.wux` disc image, a
/// NUS/CDN folder (`title.tmd`, `title.tik`, the `.app` files) or an extracted title folder
/// (`code/`, `content/`, `meta/`). `key` is the disc key and only used for a disc image; when
/// it is `None` the library looks in `keys.txt` and then a `<image>.key` beside the image.
#[derive(Debug, Clone)]
pub struct Source {
    pub path: PathBuf,
    pub key: Option<[u8; 16]>,
}

impl Source {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), key: None }
    }
}

/// Reads a disc key file as dumpers write it: 16 raw bytes, or (as some tools do) 32 hex
/// digits on one line.
pub fn read_key_file(path: &Path) -> Option<[u8; 16]> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() == 16 {
        return bytes.try_into().ok();
    }
    let text = std::str::from_utf8(&bytes).ok()?.trim();
    if text.len() != 32 {
        return None;
    }
    let mut key = [0u8; 16];
    for (i, b) in key.iter_mut().enumerate() {
        *b = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(key)
}

/// The key file a dumper writes beside a disc image: `Game.key` for `Game.wux`, or the
/// `game.key` Wii U dumpers write beside `game.wud` in a dump folder.
pub fn key_file_for(image: &Path) -> Option<PathBuf> {
    let beside = image.with_extension("key");
    if beside.is_file() {
        return Some(beside);
    }
    let dir = image.parent()?;
    ["game.key", "disc.key"].iter().map(|n| dir.join(n)).find(|p| p.is_file())
}

/// Catches a panic in a callback rather than letting it unwind into C++.
fn guarded<T>(fallback: T, f: impl FnOnce() -> T) -> T {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or(fallback)
}

/// Writes `sources` into one `.wua` at `out` (created or replaced), then reads it back.
///
/// `source_json` becomes the manifest's `"source"` member: what the caller knows of the
/// source, normally the hashes it was verified with. `progress(done, total)` returning false
/// cancels, and the output is removed.
pub fn create(sources: &[Source], keys_txt: Option<&Path>, source_json: Option<&serde_json::Value>, out: &Path, mut progress: impl FnMut(u64, u64) -> bool) -> Result<()> {
    let api = api().ok_or_else(unavailable)?;
    let paths = sources.iter().map(|s| c_path(&s.path)).collect::<Result<Vec<_>>>()?;
    let raw: Vec<RawSource> = paths.iter().zip(sources).map(|(p, s)| RawSource { path: p.as_ptr(), key: s.key.as_ref().map_or(std::ptr::null(), |k| k.as_ptr()) }).collect();
    let keys = c_opt(keys_txt)?;
    let json = source_json.map(|v| CString::new(v.to_string()).expect("serde_json never writes a NUL"));
    let out_c = c_path(out)?;

    unsafe extern "C" fn on_progress(done: u64, total: u64, user: *mut c_void) -> c_int {
        let f = &mut *(user as *mut &mut dyn FnMut(u64, u64) -> bool);
        guarded(1, || c_int::from(!f(done, total)))
    }
    let mut f: &mut dyn FnMut(u64, u64) -> bool = &mut progress;
    let mut error = [0u8; ERROR_LEN];
    let code = unsafe {
        (api.create)(
            raw.as_ptr(),
            raw.len(),
            keys.as_ref().map_or(std::ptr::null(), |k| k.as_ptr()),
            json.as_ref().map_or(std::ptr::null(), |j| j.as_ptr()),
            out_c.as_ptr(),
            Some(on_progress),
            &mut f as *mut _ as *mut c_void,
            error.as_mut_ptr() as *mut c_char,
            error.len(),
        )
    };
    check(code, &error, &out.display().to_string())
}

/// The title one source holds: its title id and version, from its `code/app.xml`. For a disc
/// image this also proves the key works.
pub fn title_info(source: &Source, keys_txt: Option<&Path>) -> Result<(u64, u32)> {
    let api = api().ok_or_else(unavailable)?;
    let path = c_path(&source.path)?;
    let keys = c_opt(keys_txt)?;
    let (mut id, mut version) = (0u64, 0u32);
    let mut error = [0u8; ERROR_LEN];
    let code = unsafe {
        (api.title_info)(
            path.as_ptr(),
            source.key.as_ref().map_or(std::ptr::null(), |k| k.as_ptr()),
            keys.as_ref().map_or(std::ptr::null(), |k| k.as_ptr()),
            &mut id,
            &mut version,
            error.as_mut_ptr() as *mut c_char,
            error.len(),
        )
    };
    check(code, &error, &source.path.display().to_string())?;
    Ok((id, version))
}

/// The titles in a `.wua`: title id and version of each `<id>_v<version>` root folder.
pub fn wua_titles(wua: &Path) -> Result<Vec<(u64, u32)>> {
    let api = api().ok_or_else(unavailable)?;
    let path = c_path(wua)?;
    unsafe extern "C" fn each(id: u64, version: u32, user: *mut c_void) {
        let v = &mut *(user as *mut Vec<(u64, u32)>);
        v.push((id, version));
    }
    let mut out = Vec::new();
    let mut error = [0u8; ERROR_LEN];
    let code = unsafe { (api.wua_titles)(path.as_ptr(), each, &mut out as *mut _ as *mut c_void, error.as_mut_ptr() as *mut c_char, error.len()) };
    check(code, &error, &wua.display().to_string())?;
    Ok(out)
}

/// The manifest's text, or `None` for a readable `.wua` without one (made by another tool).
pub fn manifest_text(wua: &Path) -> Result<Option<String>> {
    let api = api().ok_or_else(unavailable)?;
    let path = c_path(wua)?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let mut needed = 0usize;
        let mut error = [0u8; ERROR_LEN];
        let code = unsafe { (api.wua_manifest)(path.as_ptr(), buf.as_mut_ptr() as *mut c_char, buf.len(), &mut needed, error.as_mut_ptr() as *mut c_char, error.len()) };
        match Status::from_code(code) {
            None => {
                let text = CStr::from_bytes_until_nul(&buf).map_err(|_| Error { status: Status::Read, message: "the manifest was not terminated".into() })?;
                return Ok(Some(text.to_string_lossy().into_owned()));
            }
            Some(Status::NoManifest) => return Ok(None),
            Some(Status::Args) if needed > buf.len() => buf.resize(needed, 0),
            Some(_) => return check(code, &error, &wua.display().to_string()).map(|_| None),
        }
    }
}

/// Up to `max` bytes from the start of one file in a `.wua` or `.zar` (`inner`, `/`-separated
/// from the archive root), read without walking the rest of the archive: for a header such as
/// `default.xex` or a title's `meta/meta.xml`. `None` when the archive has no such file, or
/// when the library was built before it could do this (rebuild it: `deps/cemu/scripts/build.sh`).
pub fn read_file(archive: &Path, inner: &str, max: usize) -> Result<Option<Vec<u8>>> {
    let api = api().ok_or_else(unavailable)?;
    let Some(read) = api.read_file else { return Ok(None) };
    let path = c_path(archive)?;
    let name = CString::new(inner).map_err(|_| Error { status: Status::Args, message: format!("{inner} has a NUL in its name") })?;
    let mut out = vec![0u8; max];
    let mut got = 0usize;
    let mut size = 0u64;
    let mut error = [0u8; ERROR_LEN];
    let code = unsafe { read(path.as_ptr(), name.as_ptr(), 0, out.as_mut_ptr(), out.len(), &mut got, &mut size, error.as_mut_ptr() as *mut c_char, error.len()) };
    if Status::from_code(code) == Some(Status::NotFound) {
        return Ok(None);
    }
    check(code, &error, &archive.display().to_string())?;
    out.truncate(got);
    Ok(Some(out))
}

/// `romexplorer.json`: what a `.wua` written here holds, and what it was made from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub tool: String,
    /// Whatever the copy recorded about the source: see [`SourceRecord`].
    #[serde(default)]
    pub source: serde_json::Value,
    pub titles: Vec<ManifestTitle>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestTitle {
    /// `<title id>_v<version>`, the folder in the archive.
    pub folder: String,
    pub title_id: String,
    pub version: u32,
    pub files: Vec<ManifestFile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestFile {
    /// Relative to the title folder.
    pub path: String,
    pub size: u64,
    pub sha1: String,
}

/// What a copy puts in the manifest's `"source"`: the dump the archive was made from, as the
/// catalogue knew it. With these a scan can match the `.wua` to the same DAT entry the disc
/// matched, once [`verify`] has shown the files are still the ones written.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SourceRecord {
    /// The disc's file name, as it was.
    pub name: String,
    /// The decompressed disc's size (a `.wux` is a `.wud` with repeated sectors folded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crc32: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha1: Option<String>,
    /// Whether these hashes matched a DAT entry when the archive was written.
    #[serde(default)]
    pub verified: bool,
}

/// The parsed manifest, or `None` for a `.wua` without one.
pub fn manifest(wua: &Path) -> Result<Option<Manifest>> {
    let Some(text) = manifest_text(wua)? else { return Ok(None) };
    serde_json::from_str(&text).map(Some).map_err(|e| Error { status: Status::Read, message: format!("{MANIFEST_NAME} is not readable: {e}") })
}

/// Streams every file of every title, each folder in name order: `begin(path, size)` with
/// `<title folder>/<path>`, then `data` in chunks as it is decompressed. Either returning
/// false stops the walk (an [`Status::Cancelled`] error).
pub fn stream(wua: &Path, begin: impl FnMut(&str, u64) -> bool, data: impl FnMut(&[u8]) -> bool) -> Result<()> {
    let api = api().ok_or_else(unavailable)?;
    stream_with(api.wua_stream, wua, begin, data)
}

/// Every file of any ZArchive (a `.zar`) except the root manifest, with paths relative to the
/// root, each folder in name order; otherwise as [`stream`].
pub fn zar_stream(zar: &Path, begin: impl FnMut(&str, u64) -> bool, data: impl FnMut(&[u8]) -> bool) -> Result<()> {
    let api = api().ok_or_else(unavailable)?;
    stream_with(api.zar_stream, zar, begin, data)
}

fn stream_with(f: WuaStreamFn, wua: &Path, mut begin: impl FnMut(&str, u64) -> bool, mut data: impl FnMut(&[u8]) -> bool) -> Result<()> {
    let path = c_path(wua)?;
    struct Sinks<'a> {
        begin: &'a mut dyn FnMut(&str, u64) -> bool,
        data: &'a mut dyn FnMut(&[u8]) -> bool,
    }
    unsafe extern "C" fn on_file(path: *const c_char, size: u64, user: *mut c_void) -> c_int {
        let s = &mut *(user as *mut Sinks);
        let name = CStr::from_ptr(path).to_string_lossy();
        guarded(1, || c_int::from(!(s.begin)(&name, size)))
    }
    unsafe extern "C" fn on_data(ptr: *const u8, len: usize, user: *mut c_void) -> c_int {
        let s = &mut *(user as *mut Sinks);
        let chunk = if len == 0 { &[][..] } else { std::slice::from_raw_parts(ptr, len) };
        guarded(1, || c_int::from(!(s.data)(chunk)))
    }
    let mut sinks = Sinks { begin: &mut begin, data: &mut data };
    let mut error = [0u8; ERROR_LEN];
    let code = unsafe { f(path.as_ptr(), on_file, on_data, &mut sinks as *mut _ as *mut c_void, error.as_mut_ptr() as *mut c_char, error.len()) };
    check(code, &error, &wua.display().to_string())
}

/// What [`verify`] found.
#[derive(Debug, Clone, PartialEq)]
pub enum Verified {
    /// Every file the manifest lists is there, the same size and SHA-1, and nothing else is.
    Intact(Manifest),
    /// The manifest is there and something differs: the first file that does, and how.
    Mismatch { manifest: Manifest, path: String, reason: String },
    /// A `.wua` written elsewhere: nothing to check it against.
    NoManifest,
}

/// Streams the whole archive and checks each file against the manifest, never holding more
/// than one chunk. Files are matched by path: ZArchive keeps each folder sorted, so the order
/// they come back in is not the order they were written. `progress(done, total)` gets bytes
/// decompressed out of the manifest's total; returning false cancels.
pub fn verify(wua: &Path, progress: impl FnMut(u64, u64) -> bool) -> Result<Verified> {
    let Some(manifest) = manifest(wua)? else { return Ok(Verified::NoManifest) };
    let files = manifest.titles.iter().flat_map(|t| t.files.iter().map(move |f| (format!("{}/{}", t.folder, f.path), f)));
    match check_files(files, |begin, data| stream(wua, begin, data), progress)? {
        Some((path, reason)) => Ok(Verified::Mismatch { manifest, path, reason }),
        None => Ok(Verified::Intact(manifest)),
    }
}

/// Streams an archive through `stream` and checks each file against `files` (archive path,
/// manifest entry), matched by path. The first difference, or `None` when every file is
/// there with its size and SHA-1 and nothing else is.
pub(crate) fn check_files<'a>(
    files: impl Iterator<Item = (String, &'a ManifestFile)>,
    stream: impl FnOnce(&mut dyn FnMut(&str, u64) -> bool, &mut dyn FnMut(&[u8]) -> bool) -> Result<()>,
    mut progress: impl FnMut(u64, u64) -> bool,
) -> Result<Option<(String, String)>> {
    let mut expected: std::collections::HashMap<String, (u64, String)> = files.map(|(p, f)| (p, (f.size, f.sha1.to_ascii_lowercase()))).collect();
    let total: u64 = expected.values().map(|e| e.0).sum();

    struct Current {
        path: String,
        size: u64,
        sha1: String,
        hasher: Sha1,
        got: u64,
    }
    struct State {
        current: Option<Current>,
        done: u64,
        problem: Option<(String, String)>,
    }
    fn finish(st: &mut State) {
        let Some(c) = st.current.take() else { return };
        let digest: String = c.hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if st.problem.is_some() {
        } else if c.got != c.size {
            st.problem = Some((c.path, format!("{} bytes, the manifest says {}", c.got, c.size)));
        } else if digest != c.sha1 {
            st.problem = Some((c.path, format!("SHA-1 {digest}, the manifest says {}", c.sha1)));
        }
    }
    let state = std::cell::RefCell::new(State { current: None, done: 0, problem: None });
    let result = stream(
        &mut |path, size| {
            let mut st = state.borrow_mut();
            finish(&mut st);
            if st.problem.is_some() {
                return false;
            }
            match expected.remove(path) {
                Some((want, _)) if want != size => {
                    st.problem = Some((path.to_string(), format!("{size} bytes, the manifest says {want}")));
                    false
                }
                Some((_, sha1)) => {
                    st.current = Some(Current { path: path.to_string(), size, sha1, hasher: Sha1::new(), got: 0 });
                    true
                }
                None => {
                    st.problem = Some((path.to_string(), "not in the manifest".into()));
                    false
                }
            }
        },
        &mut |chunk| {
            let mut st = state.borrow_mut();
            if let Some(c) = st.current.as_mut() {
                c.hasher.update(chunk);
                c.got += chunk.len() as u64;
            }
            st.done += chunk.len() as u64;
            let done = st.done;
            drop(st);
            progress(done, total)
        },
    );
    let mut st = state.into_inner();
    match result {
        Ok(()) => finish(&mut st),
        Err(e) if e.status == Status::Cancelled && st.problem.is_some() => {}
        Err(e) => return Err(e),
    }
    if st.problem.is_none() {
        if let Some(missing) = expected.keys().min() {
            st.problem = Some((missing.clone(), "missing from the archive".into()));
        }
    }
    Ok(st.problem)
}

/// A `.zar` being written ([`zar_create`]): directories and files added in any order, then
/// [`ZarWriter::finish`]. Dropped unfinished, the output is removed.
pub struct ZarWriter {
    api: &'static Api,
    handle: *mut c_void,
}

// One handle is used from one thread at a time (cemuwua.h); moving it between threads is fine.
unsafe impl Send for ZarWriter {}

/// Starts a ZArchive at `out` (created or replaced).
pub fn zar_create(out: &Path) -> Result<ZarWriter> {
    let api = api().ok_or_else(unavailable)?;
    let path = c_path(out)?;
    let mut error = [0u8; ERROR_LEN];
    let handle = unsafe { (api.zar_create)(path.as_ptr(), error.as_mut_ptr() as *mut c_char, error.len()) };
    if handle.is_null() {
        check(-5, &error, &out.display().to_string())?;
    }
    Ok(ZarWriter { api, handle })
}

impl ZarWriter {
    fn path_call(&mut self, f: ZarPathFn, path: &str) -> Result<()> {
        let c = CString::new(path).map_err(|_| Error { status: Status::Args, message: format!("{path} has a NUL in its name") })?;
        let code = unsafe { f(self.handle, c.as_ptr()) };
        check(code, &[0], path).map_err(|e| Error { message: format!("{path}: could not be added to the archive"), ..e })
    }

    /// A directory, with its parents.
    pub fn dir(&mut self, path: &str) -> Result<()> {
        self.path_call(self.api.zar_dir, path)
    }

    /// Starts a file; its parent directory must already be there.
    pub fn file(&mut self, path: &str) -> Result<()> {
        self.path_call(self.api.zar_file, path)
    }

    /// Appends to the file last started.
    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        let code = unsafe { (self.api.zar_write)(self.handle, data.as_ptr(), data.len()) };
        check(code, &[0], "the archive").map_err(|e| Error { message: "writing the archive failed (disk full?)".into(), ..e })
    }

    /// Adds `manifest` as `romexplorer.json` (when given), writes the index and reads the
    /// archive back.
    pub fn finish(mut self, manifest: Option<&str>) -> Result<()> {
        let json = manifest.map(|m| CString::new(m).map_err(|_| Error { status: Status::Args, message: "the manifest has a NUL in it".into() })).transpose()?;
        let mut error = [0u8; ERROR_LEN];
        let handle = std::mem::replace(&mut self.handle, std::ptr::null_mut());
        let code = unsafe { (self.api.zar_finish)(handle, json.as_ref().map_or(std::ptr::null(), |j| j.as_ptr()), error.as_mut_ptr() as *mut c_char, error.len()) };
        check(code, &error, "the archive")
    }
}

impl Drop for ZarWriter {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { (self.api.zar_abort)(self.handle) };
        }
    }
}
