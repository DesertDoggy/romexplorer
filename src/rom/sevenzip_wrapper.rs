//! Rust wrapper around the sevenzip shared library built by
//! `deps/7-Zip-zstd/scripts/build.sh` (see that submodule's
//! `deps/7-Zip-zstd/sevenzip_lib.h`) -- a zip+7z-only archive library (read+write, no other
//! container formats, no password/encryption), exposed here as a typed [`ArchiveFormat`]
//! + [`CompressionMethod`] pair instead of raw C enums, with a stateful [`SevenZipArchive`]
//! handle for reading (open once, list/extract multiple entries, close) and a one-shot
//! [`create_archive`] function for writing.
//!
//! The native library is located fresh on every call via [`find_sevenzip_library`],
//! which always resolves to the most recently built version directory -- rebuilding
//! against a newer upstream 7-Zip-zstd commit is picked up automatically, no code
//! changes needed here.
//!
//! Unlike [`super::chdman_wrapper`]/[`super::dolphin_wrapper`], calls here are not
//! serialized behind a process-wide lock: sevenzip_lib.cpp has no shared mutable state
//! across archive handles (its only global, `g_lastError`, is thread-local, and every
//! `IInArchive`/`IOutArchive` instance is independent), so different handles/calls on
//! different threads are safe concurrently. A single [`SevenZipArchive`] handle is not
//! safe to use from multiple threads at once -- normal single-owner Rust usage already
//! avoids that.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::{Path, PathBuf};

use libloading::{Library, Symbol};

use super::native_lib;

// ---------------------------------------------------------------------------
// Native library discovery
// ---------------------------------------------------------------------------

fn release_root() -> PathBuf {
    native_lib::source_root().join("deps/7-Zip-zstd/release")
}

/// The filename `build_sevenzip_lib.sh` produces on this platform: `7z.dll` /
/// `7z.so` / `7z.dylib` -- matching upstream 7-Zip's own naming (no `lib` prefix),
/// unlike chdman/dolphinrvz's `lib{name}.so` convention.
fn sevenzip_lib_filename() -> Option<&'static str> {
    if cfg!(target_os = "windows") {
        Some("7z.dll")
    } else if cfg!(target_os = "macos") || cfg!(target_os = "ios") {
        Some("7z.dylib")
    } else if cfg!(target_os = "linux") || cfg!(target_os = "android") {
        Some("7z.so")
    } else {
        None
    }
}

/// Finds `<release_root>/<platform>/<arch>/<latest-version>/<7z.dll|7z.so|7z.dylib>`.
/// Reuses [`native_lib::find_latest_version_dir`]'s mtime-based "newest build" logic
/// (layout-agnostic), but joins our own filename directly under the version dir instead
/// of chdman/dolphinrvz's `dynamic/lib{name}.{ext}` layout -- build_sevenzip_lib.sh
/// intentionally does not use a `dynamic/` subdirectory or a `lib` prefix, to match
/// upstream 7-Zip's own `7z.dll`/`7z.so`/`7z.dylib` naming.
///
/// Either build: the streaming one is the standard one plus the stream entry points, so a
/// machine with only that build has zip and 7z all the same (as chdman and dolphinrvz do).
fn find_sevenzip_library() -> Option<PathBuf> {
    find_library(native_lib::LibVariant::Standard).or_else(|| find_library(native_lib::LibVariant::Streaming))
}

/// [`find_sevenzip_library`] for either build variant (`build.sh --nostreaming` vs the default `build.sh`).
/// Tries `app_root_ex/lib/<os>/7z.<ext>` first (see `chdman_wrapper::find_library`'s doc
/// comment -- same reasoning), falling back to the dependency's own `deps/<name>/release/` tree.
/// **Not** `native_lib::find_app_root_library` -- that assumes the generic `lib<name>.
/// <ext>` shape (right for chdman/dolphinrvz/RAHasher/zlib-ng), but 7z's own filename has
/// no `lib` prefix (see [`sevenzip_lib_filename`]'s doc comment), so the app-root
/// candidate is built here directly with the correct name instead.
pub fn find_library(variant: native_lib::LibVariant) -> Option<PathBuf> {
    let filename = sevenzip_lib_filename()?;
    if let Some(exe_root) = native_lib::exe_root() {
        let candidate = exe_root.join("lib").join(native_lib::os_dir_name()).join(native_lib::arch_dir_name()).join(filename);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    let version_dir = native_lib::find_latest_version_dir(&variant.release_root(&release_root()))?;
    let candidate = version_dir.join(filename);
    candidate.exists().then_some(candidate)
}

/// Exported only by the streaming build (`-DSEVENZIP_WITH_STREAMING`, see
/// `deps/7-Zip-zstd/sevenzip_lib.h`); its presence is what makes a library "streamable".
pub const STREAMING_SYMBOL: &str = "sevenzip_extract_entry_stream";

// ---------------------------------------------------------------------------
// FFI
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RawFormat {
    SevenZ = 0,
    Zip = 1,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RawMethod {
    Copy = 0,
    Deflate = 1,
    Deflate64 = 2,
    BZip2 = 3,
    Lzma = 4,
    Lzma2 = 5,
    Zstd = 6,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct RawFileTime {
    filetime_100ns: u64,
    extra_ns: u8,
    is_defined: c_int,
}

#[repr(C)]
struct RawEntry {
    path: *const c_char,
    size: u64,
    packed_size: u64,
    attributes: u32,
    mtime: RawFileTime,
    is_dir: c_int,
}

/// Matches `SevenZipProgressCb` in sevenzip_lib.h: return 0 to cancel, nonzero to continue.
type RawProgressCb = extern "C" fn(text: *const c_char, percent: f32, user_data: *mut c_void) -> c_int;

#[repr(C)]
struct RawCreateOptions {
    format: RawFormat,
    method: RawMethod,
    level: c_int,
    input_paths: *const *const c_char,
    archive_names: *const *const c_char,
    count: c_int,
    on_progress: Option<RawProgressCb>,
    user_data: *mut c_void,
}

type SevenzipOpenFn =
    unsafe extern "C" fn(*const c_char, RawFormat, Option<RawProgressCb>, *mut c_void) -> *mut c_void;
type SevenzipCloseFn = unsafe extern "C" fn(*mut c_void);
type SevenzipGetEntryCountFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type SevenzipGetEntryFn = unsafe extern "C" fn(*mut c_void, c_int, *mut RawEntry) -> c_int;
type SevenzipExtractEntryToFileFn =
    unsafe extern "C" fn(*mut c_void, c_int, *const c_char, Option<RawProgressCb>, *mut c_void) -> c_int;
type SevenzipExtractEntryToBufferFn = unsafe extern "C" fn(
    *mut c_void,
    c_int,
    *mut *mut u8,
    *mut usize,
    Option<RawProgressCb>,
    *mut c_void,
) -> c_int;
type SevenzipFreeBufferFn = unsafe extern "C" fn(*mut u8);
/// Matches `SevenZipDataCb` (streaming build only): return 0 to stop, nonzero to continue.
type RawDataCb = extern "C" fn(
    entry_index: u32,
    offset_in_entry: u64,
    data: *const c_void,
    size: u32,
    user_data: *mut c_void,
) -> c_int;
type SevenzipExtractEntryStreamFn = unsafe extern "C" fn(
    *mut c_void,
    c_int,
    *const c_char,
    Option<RawDataCb>,
    Option<RawProgressCb>,
    *mut c_void,
) -> c_int;
type SevenzipCreateArchiveFn = unsafe extern "C" fn(*const c_char, *const RawCreateOptions) -> c_int;
type SevenzipGetLastErrorFn = unsafe extern "C" fn() -> *const c_char;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SevenZipError {
    /// The sevenzip native library isn't built for this platform/arch (see
    /// `deps/7-Zip-zstd/scripts/build.sh`). Not an
    /// operation failure.
    Unavailable(String),
    /// A malformed argument was caught on the Rust side before ever calling into the
    /// native library (e.g. a non-UTF-8 path, or an out-of-range entry index).
    InvalidArgument(String),
    /// The operation was cancelled by `on_progress` returning `false`.
    Cancelled,
    /// The native call ran and reported a failure. `message` is
    /// `sevenzip_get_last_error()`'s text.
    OperationFailed { code: i32, message: String },
}

impl std::fmt::Display for SevenZipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(message) => write!(f, "sevenzip unavailable: {message}"),
            Self::InvalidArgument(message) => write!(f, "invalid sevenzip argument: {message}"),
            Self::Cancelled => write!(f, "sevenzip operation cancelled"),
            Self::OperationFailed { code, message } => write!(f, "sevenzip failed ({code}): {message}"),
        }
    }
}

impl std::error::Error for SevenZipError {}

fn result_from_code(code: c_int, get_message: impl FnOnce() -> String) -> Result<(), SevenZipError> {
    if code == 0 {
        Ok(())
    } else if code == -6 {
        // sevenzip_lib.cpp's dedicated "on_progress returned 0" return code, matching
        // dolphinrvz's own -6 convention -- see sevenzip_lib.h.
        Err(SevenZipError::Cancelled)
    } else {
        Err(SevenZipError::OperationFailed { code: code as i32, message: get_message() })
    }
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// Container format. This build only registers Zip and 7z handlers -- no
/// Rar/Tar/Iso/Wim/Cab/etc, and no password/encryption support in either direction (an
/// encrypted archive fails to open/extract with an error instead of prompting for a
/// password).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArchiveFormat {
    SevenZ,
    Zip,
}

impl ArchiveFormat {
    fn to_raw(self) -> RawFormat {
        match self {
            Self::SevenZ => RawFormat::SevenZ,
            Self::Zip => RawFormat::Zip,
        }
    }
}

/// Compression method for archive creation. Reading supports whatever method(s) an
/// existing archive already uses -- this enum only constrains what [`create_archive`]
/// can produce.
///
/// [`Self::Lzma2`] is valid only with [`ArchiveFormat::SevenZ`]: plain ZIP has no
/// standard LZMA2 method id (only plain LZMA), so `create_archive` rejects
/// `ArchiveFormat::Zip` + `CompressionMethod::Lzma2` itself, with
/// [`SevenZipError::InvalidArgument`], before ever calling into the native library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompressionMethod {
    Copy,
    Deflate,
    Deflate64,
    BZip2,
    Lzma,
    Lzma2,
    Zstd,
}

impl CompressionMethod {
    fn to_raw(self) -> RawMethod {
        match self {
            Self::Copy => RawMethod::Copy,
            Self::Deflate => RawMethod::Deflate,
            Self::Deflate64 => RawMethod::Deflate64,
            Self::BZip2 => RawMethod::BZip2,
            Self::Lzma => RawMethod::Lzma,
            Self::Lzma2 => RawMethod::Lzma2,
            Self::Zstd => RawMethod::Zstd,
        }
    }
}

/// Last-modified time at full precision, mirroring the native library's own
/// `SevenZipFileTime` (itself mirroring 7-Zip's internal `CArcTime`) rather than
/// collapsing to Unix seconds, which would silently discard everything below
/// 1-second resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileTime {
    /// Windows FILETIME: 100ns ticks since 1601-01-01 UTC. Meaningless when
    /// `is_defined` is `false`.
    pub filetime_100ns: u64,
    /// Additional nanoseconds (0-99) within that last 100ns tick, when the archive and
    /// source filesystem recorded finer-than-100ns precision. 0 when no finer
    /// precision than the 100ns tick itself is available.
    pub extra_ns: u8,
    /// Whether this entry actually has a modification time recorded at all.
    pub is_defined: bool,
}

impl FileTime {
    fn from_raw(raw: RawFileTime) -> Self {
        Self { filetime_100ns: raw.filetime_100ns, extra_ns: raw.extra_ns, is_defined: raw.is_defined != 0 }
    }

    /// Converts to a [`std::time::SystemTime`], losing the sub-100ns `extra_ns`
    /// remainder (`SystemTime`'s own resolution is platform-dependent and generally
    /// no finer than 100ns anyway). Returns `None` when `is_defined` is `false`.
    pub fn to_system_time(self) -> Option<std::time::SystemTime> {
        if !self.is_defined {
            return None;
        }
        // FILETIME epoch (1601-01-01 UTC) to Unix epoch (1970-01-01 UTC) is exactly
        // 11644473600 seconds; filetime_100ns counts in 100ns ticks.
        const UNIX_EPOCH_OFFSET_100NS: u64 = 11_644_473_600 * 10_000_000;
        let since_unix_epoch_100ns = self.filetime_100ns.checked_sub(UNIX_EPOCH_OFFSET_100NS)?;
        let duration = std::time::Duration::from_nanos(since_unix_epoch_100ns.checked_mul(100)?);
        Some(std::time::UNIX_EPOCH + duration)
    }
}

/// One archive entry's metadata, as returned by [`SevenZipArchive::entry`]. `path` uses
/// `/` separators, relative to the archive root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub size: u64,
    pub packed_size: u64,
    /// Raw Windows-style file attributes (`FILE_ATTRIBUTE_*`) as stored in the
    /// archive; POSIX-only archives may report 0.
    pub attributes: u32,
    pub mtime: FileTime,
    pub is_dir: bool,
}

// ---------------------------------------------------------------------------
// Progress callback trampoline
// ---------------------------------------------------------------------------

/// Called periodically during an open/extract/create call with a status message and
/// progress in `0.0..=100.0` (or `-1.0` when not meaningfully known yet, e.g. while
/// still scanning archive headers on open). Return `true` to continue, `false` to
/// cancel (surfaces as [`SevenZipError::Cancelled`]).
extern "C" fn progress_trampoline<F: FnMut(&str, f32) -> bool>(
    text: *const c_char,
    percent: f32,
    user_data: *mut c_void,
) -> c_int {
    if user_data.is_null() {
        return 1;
    }
    let text = if text.is_null() {
        std::borrow::Cow::Borrowed("")
    } else {
        unsafe { CStr::from_ptr(text) }.to_string_lossy()
    };
    let callback = unsafe { &mut *(user_data as *mut F) };
    if native_lib::guarded(false, || callback(&text, percent)) {
        1
    } else {
        0
    }
}

fn path_to_cstring(path: &Path) -> Result<CString, SevenZipError> {
    let s = path
        .to_str()
        .ok_or_else(|| SevenZipError::InvalidArgument(format!("path is not valid UTF-8: {}", path.display())))?;
    CString::new(s).map_err(|err| SevenZipError::InvalidArgument(err.to_string()))
}

fn load_library() -> Result<Library, SevenZipError> {
    let lib_path =
        find_sevenzip_library().ok_or_else(|| SevenZipError::Unavailable("sevenzip library not found".to_string()))?;
    unsafe { Library::new(&lib_path) }.map_err(|err| SevenZipError::Unavailable(err.to_string()))
}

fn load_library_variant(variant: native_lib::LibVariant) -> Result<Library, SevenZipError> {
    let lib_path = find_library(variant)
        .ok_or_else(|| SevenZipError::Unavailable(format!("sevenzip library ({variant:?}) not found")))?;
    unsafe { Library::new(&lib_path) }.map_err(|err| SevenZipError::Unavailable(err.to_string()))
}

/// Both callbacks of one [`SevenZipArchive::extract_stream`] call, behind the single
/// `user_data` pointer the C API passes to each.
struct StreamCallbacks<D, F> {
    on_data: D,
    on_progress: F,
}

extern "C" fn stream_data_trampoline<D, F>(
    entry_index: u32,
    offset_in_entry: u64,
    data: *const c_void,
    size: u32,
    user_data: *mut c_void,
) -> c_int
where
    D: FnMut(u32, u64, &[u8]) -> bool,
{
    if user_data.is_null() {
        return 1;
    }
    let callbacks = unsafe { &mut *(user_data as *mut StreamCallbacks<D, F>) };
    let chunk = if data.is_null() || size == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(data as *const u8, size as usize) }
    };
    c_int::from(native_lib::guarded(false, || (callbacks.on_data)(entry_index, offset_in_entry, chunk)))
}

extern "C" fn stream_progress_trampoline<D, F>(text: *const c_char, percent: f32, user_data: *mut c_void) -> c_int
where
    F: FnMut(&str, f32) -> bool,
{
    if user_data.is_null() {
        return 1;
    }
    let callbacks = unsafe { &mut *(user_data as *mut StreamCallbacks<D, F>) };
    progress_trampoline::<F>(text, percent, &mut callbacks.on_progress as *mut F as *mut c_void)
}

unsafe fn get_last_error(lib: &Library) -> String {
    let Ok(get_last_error): Result<Symbol<SevenzipGetLastErrorFn>, _> = lib.get(b"sevenzip_get_last_error") else {
        return String::new();
    };
    let ptr = get_last_error();
    if ptr.is_null() {
        String::new()
    } else {
        CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }
}

// ---------------------------------------------------------------------------
// Reading: a stateful handle (open once, list/extract multiple entries, close)
// ---------------------------------------------------------------------------

/// A handle to an open archive. Not safe to use from multiple threads at once; safe to
/// use concurrently with other, independent `SevenZipArchive` handles (see this
/// module's doc comment).
pub struct SevenZipArchive {
    lib: Library,
    handle: *mut c_void,
}

impl SevenZipArchive {
    /// Opens an existing archive for reading. `format` is the container format to try
    /// -- callers that don't already know it (e.g. from the file extension) should try
    /// [`ArchiveFormat::SevenZ`] then [`ArchiveFormat::Zip`], or vice versa.
    ///
    /// `on_progress` is called during the open (mainly useful for slow multi-volume
    /// archives; usually near-instant for a single zip/7z) -- see
    /// [`progress_trampoline`]. Solid 7z archives are read transparently like any other.
    pub fn open<F>(path: &Path, format: ArchiveFormat, mut on_progress: F) -> Result<Self, SevenZipError>
    where
        F: FnMut(&str, f32) -> bool,
    {
        Self::open_with(load_library()?, path, format, &mut on_progress)
    }

    /// [`Self::open`] against a specific build variant. Use
    /// [`native_lib::LibVariant::Streaming`] to get a handle whose
    /// [`Self::extract_stream`] works (check [`Self::supports_streaming`]).
    pub fn open_variant<F>(
        path: &Path,
        format: ArchiveFormat,
        variant: native_lib::LibVariant,
        mut on_progress: F,
    ) -> Result<Self, SevenZipError>
    where
        F: FnMut(&str, f32) -> bool,
    {
        Self::open_with(load_library_variant(variant)?, path, format, &mut on_progress)
    }

    fn open_with<F>(lib: Library, path: &Path, format: ArchiveFormat, on_progress: &mut F) -> Result<Self, SevenZipError>
    where
        F: FnMut(&str, f32) -> bool,
    {
        let path_c = path_to_cstring(path)?;

        let handle = unsafe {
            let open_fn: Symbol<SevenzipOpenFn> =
                lib.get(b"sevenzip_open").map_err(|err| SevenZipError::Unavailable(err.to_string()))?;
            open_fn(
                path_c.as_ptr(),
                format.to_raw(),
                Some(progress_trampoline::<F>),
                on_progress as *mut F as *mut c_void,
            )
        };

        if handle.is_null() {
            let message = unsafe { get_last_error(&lib) };
            return Err(SevenZipError::OperationFailed { code: -1, message });
        }

        Ok(Self { lib, handle })
    }

    pub fn entry_count(&self) -> i32 {
        unsafe {
            let Ok(count_fn): Result<Symbol<SevenzipGetEntryCountFn>, _> = self.lib.get(b"sevenzip_get_entry_count")
            else {
                return 0;
            };
            count_fn(self.handle) as i32
        }
    }

    /// Fetches entry `index`'s metadata (`0 <= index < entry_count()`).
    pub fn entry(&self, index: i32) -> Result<Entry, SevenZipError> {
        let mut raw = RawEntry {
            path: std::ptr::null(),
            size: 0,
            packed_size: 0,
            attributes: 0,
            mtime: RawFileTime { filetime_100ns: 0, extra_ns: 0, is_defined: 0 },
            is_dir: 0,
        };

        let code = unsafe {
            let get_entry: Symbol<SevenzipGetEntryFn> =
                self.lib.get(b"sevenzip_get_entry").map_err(|err| SevenZipError::Unavailable(err.to_string()))?;
            get_entry(self.handle, index as c_int, &mut raw)
        };

        if code != 0 {
            let message = unsafe { get_last_error(&self.lib) };
            return Err(SevenZipError::OperationFailed { code, message });
        }

        let path = if raw.path.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(raw.path) }.to_string_lossy().into_owned()
        };

        Ok(Entry {
            path,
            size: raw.size,
            packed_size: raw.packed_size,
            attributes: raw.attributes,
            mtime: FileTime::from_raw(raw.mtime),
            is_dir: raw.is_dir != 0,
        })
    }

    /// Extracts entry `index` to a new file at `out_path` (overwritten if it already
    /// exists; parent directories are created as needed).
    pub fn extract_to_file<F>(&self, index: i32, out_path: &Path, mut on_progress: F) -> Result<(), SevenZipError>
    where
        F: FnMut(&str, f32) -> bool,
    {
        let out_path_c = path_to_cstring(out_path)?;

        let code = unsafe {
            let extract_fn: Symbol<SevenzipExtractEntryToFileFn> = self
                .lib
                .get(b"sevenzip_extract_entry_to_file")
                .map_err(|err| SevenZipError::Unavailable(err.to_string()))?;
            extract_fn(
                self.handle,
                index as c_int,
                out_path_c.as_ptr(),
                Some(progress_trampoline::<F>),
                &mut on_progress as *mut _ as *mut c_void,
            )
        };

        // A cancelled or failed extract (a CRC error) leaves a partial member: not a file.
        let result = result_from_code(code, || unsafe { get_last_error(&self.lib) });
        if result.is_err() {
            let _ = std::fs::remove_file(out_path);
        }
        result
    }

    /// True if the loaded library is a streaming build (exports [`STREAMING_SYMBOL`]).
    pub fn supports_streaming(&self) -> bool {
        unsafe { self.lib.get::<*const ()>(STREAMING_SYMBOL.as_bytes()) }.is_ok()
    }

    /// Extracts entry `index` in one decompression pass, handing every decompressed chunk
    /// to `on_data(entry_index, offset_in_entry, bytes)` and, if `out_path` is `Some`,
    /// also writing the same bytes to that file. Chunks arrive in order with contiguous
    /// offsets from 0; `entry_index` is always `index` (for a solid 7z, the entries
    /// decoded-through before it are skipped, never handed to `on_data`).
    ///
    /// Returning `false` from `on_data` or `on_progress` stops the extraction; both
    /// surface as [`SevenZipError::Cancelled`]. Needs a streaming build (see
    /// [`Self::open_variant`]); otherwise returns [`SevenZipError::Unavailable`].
    pub fn extract_stream<D, F>(
        &self,
        index: i32,
        out_path: Option<&Path>,
        on_data: D,
        on_progress: F,
    ) -> Result<(), SevenZipError>
    where
        D: FnMut(u32, u64, &[u8]) -> bool,
        F: FnMut(&str, f32) -> bool,
    {
        let out_path_c = out_path.map(path_to_cstring).transpose()?;
        let mut callbacks = StreamCallbacks { on_data, on_progress };

        let code = unsafe {
            let extract_fn: Symbol<SevenzipExtractEntryStreamFn> =
                self.lib.get(STREAMING_SYMBOL.as_bytes()).map_err(|_| {
                    SevenZipError::Unavailable("sevenzip library is not a streaming build".to_string())
                })?;
            extract_fn(
                self.handle,
                index as c_int,
                out_path_c.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
                Some(stream_data_trampoline::<D, F>),
                Some(stream_progress_trampoline::<D, F>),
                &mut callbacks as *mut StreamCallbacks<D, F> as *mut c_void,
            )
        };

        let result = result_from_code(code, || unsafe { get_last_error(&self.lib) });
        if let (Err(_), Some(out)) = (&result, out_path) {
            let _ = std::fs::remove_file(out);
        }
        result
    }

    /// Reads a whole entry into memory, at any size.
    ///
    /// The one to use for **non-ROM** members -- a DAT inside a No-Intro pack, a config or
    /// metadata file -- where the caller wants the bytes rather than a stream. The other two
    /// extract calls are each wrong for that:
    ///
    /// - [`Self::extract_stream`] exists for ROM content, feeding chunks to hashing and
    ///   detection ([`crate::rom::content`]). Driving it just to append into a `Vec` borrows a
    ///   pipeline abstraction for something that is not a pipeline, and it needs the streaming
    ///   build besides.
    /// - [`Self::extract_to_buffer`] is what this calls. It used to be unusable here: the
    ///   native side released the in-memory stream in `SetOperationResult` and then copied
    ///   from the freed buffer, which survived by luck under glibc's 128 KB mmap threshold and
    ///   segfaulted above it. Fixed in `deps/7-Zip-zstd/sevenzip_lib.cpp` by holding an owning reference
    ///   (`MemStreamOwner`) for the whole extraction.
    ///
    /// Kept as a named entry point rather than telling callers to use `extract_to_buffer`
    /// directly, because *which* of the three extract calls suits a given job is the thing
    /// worth writing down once.
    pub fn read_entry(&self, index: i32) -> Result<Vec<u8>, SevenZipError> {
        self.extract_to_buffer(index, |_, _| true)
    }

    /// Extracts entry `index` into memory. Intended for the "pull one small metadata
    /// file out of an archive" case -- extracting a very large entry this way holds the
    /// whole decompressed entry in memory at once.
    ///
    /// Safe at any size since the use-after-free in `sevenzip_lib.cpp` was fixed (the native
    /// side used to free the buffer before copying it out, which only looked like it worked
    /// below glibc's 128 KB mmap threshold). [`Self::read_entry`] is the same call under a
    /// name that says what it is for.
    pub fn extract_to_buffer<F>(&self, index: i32, mut on_progress: F) -> Result<Vec<u8>, SevenZipError>
    where
        F: FnMut(&str, f32) -> bool,
    {
        let mut out_data: *mut u8 = std::ptr::null_mut();
        let mut out_len: usize = 0;

        let code = unsafe {
            let extract_fn: Symbol<SevenzipExtractEntryToBufferFn> = self
                .lib
                .get(b"sevenzip_extract_entry_to_buffer")
                .map_err(|err| SevenZipError::Unavailable(err.to_string()))?;
            extract_fn(
                self.handle,
                index as c_int,
                &mut out_data,
                &mut out_len,
                Some(progress_trampoline::<F>),
                &mut on_progress as *mut _ as *mut c_void,
            )
        };

        if code != 0 {
            let message = unsafe { get_last_error(&self.lib) };
            return result_from_code(code, || message).map(|_| Vec::new());
        }

        let data = if out_data.is_null() || out_len == 0 {
            Vec::new()
        } else {
            let slice = unsafe { std::slice::from_raw_parts(out_data, out_len) };
            let owned = slice.to_vec();
            unsafe {
                if let Ok(free_fn) = self.lib.get::<SevenzipFreeBufferFn>(b"sevenzip_free_buffer") {
                    free_fn(out_data);
                }
            }
            owned
        };

        Ok(data)
    }
}

impl Drop for SevenZipArchive {
    fn drop(&mut self) {
        if self.handle.is_null() {
            return;
        }
        unsafe {
            if let Ok(close_fn) = self.lib.get::<SevenzipCloseFn>(b"sevenzip_close") {
                close_fn(self.handle);
            }
        }
        self.handle = std::ptr::null_mut();
    }
}

// ---------------------------------------------------------------------------
// Writing: one-shot archive creation
// ---------------------------------------------------------------------------

/// One file to add when creating an archive: `input_path` is a real filesystem path to
/// read from, `archive_name` is the path it should have inside the archive
/// (`/`-separated, relative).
#[derive(Debug, Clone)]
pub struct CreateEntry {
    pub input_path: PathBuf,
    pub archive_name: String,
}

/// Settings for [`create_archive`]. Always non-solid (each file its own compressed
/// block) -- there is no option to change this, so that any single entry can later be
/// extracted without touching the others. Directories are not expanded recursively:
/// list every file explicitly in `entries`.
#[derive(Debug, Clone)]
pub struct CreateArchiveArgs {
    pub format: ArchiveFormat,
    pub method: CompressionMethod,
    /// Compression level, 0 (store/fastest) to 9 (ultra/slowest).
    pub level: i32,
    pub entries: Vec<CreateEntry>,
}

/// Creates a new archive at `out_path` from `args.entries`, blocking the calling thread
/// until it finishes. `on_progress` is called with a status message and percent
/// complete (`0.0..=100.0`); return `false` from it to cancel.
pub fn create_archive<F>(out_path: &Path, args: &CreateArchiveArgs, mut on_progress: F) -> Result<(), SevenZipError>
where
    F: FnMut(&str, f32) -> bool,
{
    if args.format == ArchiveFormat::Zip && args.method == CompressionMethod::Lzma2 {
        return Err(SevenZipError::InvalidArgument(
            "LZMA2 is only valid for the 7z format, not ZIP".to_string(),
        ));
    }

    let lib = load_library()?;
    let out_path_c = path_to_cstring(out_path)?;

    let input_path_cs: Vec<CString> =
        args.entries.iter().map(|e| path_to_cstring(&e.input_path)).collect::<Result<_, _>>()?;
    let archive_name_cs: Vec<CString> = args
        .entries
        .iter()
        .map(|e| CString::new(e.archive_name.as_str()).map_err(|err| SevenZipError::InvalidArgument(err.to_string())))
        .collect::<Result<_, _>>()?;

    let input_path_ptrs: Vec<*const c_char> = input_path_cs.iter().map(|c| c.as_ptr()).collect();
    let archive_name_ptrs: Vec<*const c_char> = archive_name_cs.iter().map(|c| c.as_ptr()).collect();

    let raw_options = RawCreateOptions {
        format: args.format.to_raw(),
        method: args.method.to_raw(),
        level: args.level as c_int,
        input_paths: input_path_ptrs.as_ptr(),
        archive_names: archive_name_ptrs.as_ptr(),
        count: args.entries.len() as c_int,
        on_progress: Some(progress_trampoline::<F>),
        user_data: &mut on_progress as *mut _ as *mut c_void,
    };

    let code = unsafe {
        let create_fn: Symbol<SevenzipCreateArchiveFn> =
            lib.get(b"sevenzip_create_archive").map_err(|err| SevenZipError::Unavailable(err.to_string()))?;
        create_fn(out_path_c.as_ptr(), &raw_options)
    };

    result_from_code(code, || unsafe { get_last_error(&lib) })
}

// ---------------------------------------------------------------------------
// Streaming build only: creation from sources, and pull-style entry reading
// ---------------------------------------------------------------------------

use super::source::{ReadAt, Source, SourceState};

#[repr(C)]
struct RawSource {
    archive_name: *const c_char,
    size: u64,
    mtime_filetime: u64,
    read: Option<super::source::RawReadFn>,
    read_at: Option<super::source::RawReadAtFn>,
    user_data: *mut c_void,
}

type CreateFromSourcesFn = unsafe extern "C" fn(*const c_char, *const RawCreateOptions, *const RawSource, c_int) -> c_int;

fn not_streaming(_: libloading::Error) -> SevenZipError {
    SevenZipError::Unavailable("sevenzip library is not a streaming build".to_string())
}

/// [`create_archive`] with members from `sources` (member name = [`Source::name`]) instead
/// of files. Sequential sources are read front to back; random ones are read in order too,
/// since 7-Zip consumes input strictly forward. Wrap a source with [`Source::hashed`] to hash
/// its bytes as they are compressed. Needs the streaming build.
pub fn create_archive_from_sources<F>(
    out_path: &Path,
    format: ArchiveFormat,
    method: CompressionMethod,
    level: i32,
    sources: &mut [Source],
    mut on_progress: F,
) -> Result<(), SevenZipError>
where
    F: FnMut(&str, f32) -> bool,
{
    if format == ArchiveFormat::Zip && method == CompressionMethod::Lzma2 {
        return Err(SevenZipError::InvalidArgument("LZMA2 is only valid for the 7z format, not ZIP".to_string()));
    }
    let lib = load_library_variant(native_lib::LibVariant::Streaming)?;
    let out_path_c = path_to_cstring(out_path)?;
    let names: Vec<CString> = sources
        .iter()
        .map(|s| CString::new(s.name.as_str()).map_err(|err| SevenZipError::InvalidArgument(err.to_string())))
        .collect::<Result<_, _>>()?;
    let mut states: Vec<SourceState> = sources.iter_mut().map(SourceState::new).collect();
    let raw_sources: Vec<RawSource> = states
        .iter_mut()
        .zip(&names)
        .map(|(state, name)| {
            let (read, read_at) = state.callbacks();
            RawSource {
                archive_name: name.as_ptr(),
                size: state.source.size,
                mtime_filetime: 0,
                read,
                read_at,
                user_data: state as *mut SourceState as *mut c_void,
            }
        })
        .collect();

    let raw_options = RawCreateOptions {
        format: format.to_raw(),
        method: method.to_raw(),
        level: level as c_int,
        input_paths: std::ptr::null(),
        archive_names: std::ptr::null(),
        count: 0,
        on_progress: Some(progress_trampoline::<F>),
        user_data: &mut on_progress as *mut _ as *mut c_void,
    };

    let code = unsafe {
        let create: Symbol<CreateFromSourcesFn> =
            lib.get(b"sevenzip_create_archive_from_sources").map_err(not_streaming)?;
        create(out_path_c.as_ptr(), &raw_options, raw_sources.as_ptr(), raw_sources.len() as c_int)
    };
    if let Some(err) = states.iter_mut().find_map(|s| s.error.take()) {
        return Err(SevenZipError::OperationFailed { code, message: format!("source: {err}") });
    }
    result_from_code(code, || unsafe { get_last_error(&lib) })
}

type EntryReaderOpenFn = unsafe extern "C" fn(*const c_char, RawFormat, c_int) -> *mut c_void;
type EntryReaderSizeFn = unsafe extern "C" fn(*mut c_void) -> u64;
type EntryReaderReadFn = unsafe extern "C" fn(*mut c_void, *mut c_void, u64) -> i64;
type EntryReaderReadAtFn = unsafe extern "C" fn(*mut c_void, u64, *mut c_void, u64) -> c_int;
type EntryReaderCloseFn = unsafe extern "C" fn(*mut c_void);

/// Pull-style reader for one zip/7z entry, with its own archive handle (independent of
/// other readers and of [`SevenZipArchive`]; one per thread for parallel use). Decoding runs
/// on a background thread. [`std::io::Read`] is sequential; [`ReadAt::read_at`] is random
/// access *emulated* over the compressed stream -- forward seeks decode and discard,
/// backward seeks restart from the entry's start -- so it costs O(offset). Needs the
/// streaming build.
pub struct EntryReader {
    lib: Library,
    handle: *mut c_void,
    size: u64,
}

// The native reader has no thread affinity; `&mut self` on reads and no `Sync` keep it to
// one thread at a time.
unsafe impl Send for EntryReader {}

impl EntryReader {
    pub fn open(archive: &Path, format: ArchiveFormat, index: i32) -> Result<Self, SevenZipError> {
        let lib = load_library_variant(native_lib::LibVariant::Streaming)?;
        let path_c = path_to_cstring(archive)?;
        let (handle, size) = unsafe {
            let open: Symbol<EntryReaderOpenFn> = lib.get(b"sevenzip_entry_reader_open").map_err(not_streaming)?;
            let handle = open(path_c.as_ptr(), format.to_raw(), index as c_int);
            if handle.is_null() {
                let message = get_last_error(&lib);
                return Err(SevenZipError::OperationFailed { code: -1, message });
            }
            let size_fn: Symbol<EntryReaderSizeFn> = lib.get(b"sevenzip_entry_reader_size").map_err(not_streaming)?;
            (handle, size_fn(handle))
        };
        Ok(Self { lib, handle, size })
    }

    /// Uncompressed size of the entry.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Wraps this reader as a sequential [`Source`] named `name`.
    pub fn into_source(self, name: impl Into<String>) -> Source {
        let size = self.size;
        Source::sequential(name, size, self)
    }
}

impl std::io::Read for EntryReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = unsafe {
            let read: Symbol<EntryReaderReadFn> =
                self.lib.get(b"sevenzip_entry_reader_read").map_err(std::io::Error::other)?;
            read(self.handle, buf.as_mut_ptr() as *mut c_void, buf.len() as u64)
        };
        if n < 0 {
            return Err(std::io::Error::other(unsafe { get_last_error(&self.lib) }));
        }
        Ok(n as usize)
    }
}

impl ReadAt for EntryReader {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        let rc = unsafe {
            let read_at: Symbol<EntryReaderReadAtFn> =
                self.lib.get(b"sevenzip_entry_reader_read_at").map_err(std::io::Error::other)?;
            read_at(self.handle, offset, buf.as_mut_ptr() as *mut c_void, buf.len() as u64)
        };
        if rc != 0 {
            return Err(std::io::Error::other(unsafe { get_last_error(&self.lib) }));
        }
        Ok(())
    }
}

impl Drop for EntryReader {
    fn drop(&mut self) {
        if let Ok(close) = unsafe { self.lib.get::<EntryReaderCloseFn>(b"sevenzip_entry_reader_close") } {
            unsafe { close(self.handle) };
        }
    }
}
