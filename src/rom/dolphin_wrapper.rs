//! Rust wrapper around the dolphinrvz shared library built by
//! `deps/dolphin/scripts/build_dolphin_rvz.sh` (see that submodule's
//! `deps/dolphin/dolphinrvz.h`/`README.dolphinrvz.md`) -- Dolphin's ISO/GCZ/WIA/RVZ disc
//! converter (the same functionality as `dolphin-tool convert` / the GUI's "Convert
//! File..." dialog), exposed here as a typed [`DolphinConvertArgs`] struct instead of
//! CLI flags or GUI widgets -- paths are `PathBuf`, the container format and
//! compression method are enums, block size/compression level are plain integers.
//!
//! The native library is located fresh on every call ([`find_library`]): the deployed
//! `app_root_ex/lib/<os>/<arch>/` copy first, then the submodule's most recently built
//! version directory -- rebuilding dolphinrvz against a newer upstream Dolphin
//! commit is picked up automatically, no code changes needed here.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use libloading::{Library, Symbol};

use super::native_lib;

// ---------------------------------------------------------------------------
// FFI
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RawFormat {
    Iso = 0,
    Gcz = 1,
    Wia = 2,
    Rvz = 3,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RawCompression {
    None = 0,
    Purge = 1,
    Bzip2 = 2,
    Lzma = 3,
    Lzma2 = 4,
    Zstd = 5,
}

type RawProgressCb = extern "C" fn(text: *const c_char, percent: f32, user_data: *mut c_void) -> c_int;

#[repr(C)]
struct RawConvertOptions {
    input_path: *const c_char,
    output_path: *const c_char,
    format: RawFormat,
    user_dir: *const c_char,
    scrub: c_int,
    block_size: c_int,
    compression: RawCompression,
    compression_level: c_int,
    on_progress: Option<RawProgressCb>,
    user_data: *mut c_void,
}

type DolphinRvzConvertFn = unsafe extern "C" fn(*const RawConvertOptions) -> c_int;
type DolphinRvzGetLastErrorFn = unsafe extern "C" fn() -> *const c_char;
type DolphinRvzGetAllowedCompressionLevelsFn = unsafe extern "C" fn(RawCompression, *mut c_int, *mut c_int);

/// dolphinrvz's own docs: "Not safe to call concurrently with itself on multiple
/// threads at once (shares process-wide DiscIO/Common state)." Serialized here so
/// misuse from Rust can't hit that.
static DOLPHINRVZ_CALL_LOCK: Mutex<()> = Mutex::new(());

fn release_root() -> PathBuf {
    native_lib::source_root().join("deps/dolphin/release")
}

/// Locates libdolphinrvz for either build variant (default vs streaming build). Tries
/// `app_root_ex/lib/<os>/libdolphinrvz.<ext>` first (see `chdman_wrapper::find_library`'s
/// doc comment -- same reasoning), falling back to the dependency's own `deps/<name>/release/`
/// tree.
pub fn find_library(variant: native_lib::LibVariant) -> Option<PathBuf> {
    native_lib::find_app_root_library("dolphinrvz").or_else(|| native_lib::find_native_library(&variant.release_root(&release_root()), "dolphinrvz"))
}

/// The library for a call that needs no streaming symbol: the deployed copy, then either
/// submodule build, since the streaming build exports everything the plain one does.
///
/// These calls used to ask `native_lib::find_native_library(&release_root(), ..)` directly,
/// which is the *plain* submodule build and nothing else. That skipped the deployed
/// `app_root_ex/lib/<os>/<arch>/` copy -- the only one a packaged install has -- and found
/// nothing on a machine holding only the default (streaming) build. It went unnoticed where a
/// plain build happened to be left over in the submodule tree.
fn find_any_library() -> Option<PathBuf> {
    find_library(native_lib::LibVariant::Standard).or_else(|| find_library(native_lib::LibVariant::Streaming))
}

/// Exported only by the streaming build of libdolphinrvz (`build_dolphin_rvz.sh`'s default;
/// `--nostreaming` omits it); its presence is what makes a library "streamable" (see [`extract_stream`]).
pub const STREAMING_SYMBOL: &str = "dolphinrvz_extract_stream";

/// Exported only by the streaming build: the random-access [`DiscReader`] API.
pub const READER_SYMBOL: &str = "dolphinrvz_reader_open";

// Streaming build only (see dolphinrvz.h, DOLPHINRVZ_WITH_STREAMING).
type RawDataCb = extern "C" fn(
    file_index: u32,
    file_name: *const c_char,
    offset: u64,
    data: *const c_void,
    size: u32,
    user_data: *mut c_void,
) -> c_int;
type ExtractStreamFn = unsafe extern "C" fn(
    *const c_char,
    *const c_char,
    *const c_char,
    Option<RawDataCb>,
    Option<RawProgressCb>,
    *mut c_void,
) -> c_int;
type ConvertStreamFn = unsafe extern "C" fn(*const RawConvertOptions, Option<RawDataCb>) -> c_int;

#[repr(C)]
#[derive(Default)]
struct RawReaderInfo {
    data_size: u64,
    raw_size: u64,
    block_size: u64,
    blob_type: i32,
    data_size_accurate: i32,
}
type ReaderOpenFn = unsafe extern "C" fn(*const c_char, *const c_char) -> *mut c_void;
type ReaderCloseFn = unsafe extern "C" fn(*mut c_void);
type ReaderInfoFn = unsafe extern "C" fn(*mut c_void, *mut RawReaderInfo) -> c_int;
type ReaderReadFn = unsafe extern "C" fn(*mut c_void, u64, *mut c_void, u64) -> c_int;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DolphinRvzError {
    /// The dolphinrvz native library isn't built for this platform/arch (see
    /// `deps/dolphin/scripts/build_dolphin_rvz.sh`). Not a conversion
    /// failure.
    Unavailable(String),
    /// A malformed argument was caught on the Rust side before ever calling
    /// dolphinrvz (currently unused, reserved for future validation).
    InvalidArgument(String),
    /// The conversion was cancelled by `on_progress` returning `false`.
    Cancelled,
    /// dolphinrvz ran and reported a failure. `message` is
    /// `dolphinrvz_get_last_error()`'s text (the same validation dolphinrvz's own
    /// convert CLI performs: missing required fields, invalid block size/compression
    /// level for the chosen format, etc).
    ConvertFailed { code: i32, message: String },
}

impl std::fmt::Display for DolphinRvzError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(message) => write!(f, "dolphinrvz unavailable: {message}"),
            Self::InvalidArgument(message) => write!(f, "invalid dolphinrvz argument: {message}"),
            Self::Cancelled => write!(f, "dolphinrvz conversion cancelled"),
            Self::ConvertFailed { code, message } => write!(f, "dolphinrvz failed ({code}): {message}"),
        }
    }
}

impl std::error::Error for DolphinRvzError {}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// Output container format -- matches `DiscIO::BlobType`'s PLAIN/GCZ/WIA/RVZ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiscFormat {
    Iso,
    Gcz,
    Wia,
    Rvz,
}

impl DiscFormat {
    fn to_raw(self) -> RawFormat {
        match self {
            Self::Iso => RawFormat::Iso,
            Self::Gcz => RawFormat::Gcz,
            Self::Wia => RawFormat::Wia,
            Self::Rvz => RawFormat::Rvz,
        }
    }
}

/// Compression method for WIA/RVZ output -- matches `DiscIO::WIARVZCompressionType`.
/// WIA does not support [`Self::Zstd`]; RVZ does not support [`Self::Purge`] (same
/// restriction as the Dolphin GUI/CLI; dolphinrvz rejects the combination with
/// [`DolphinRvzError::ConvertFailed`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiscCompression {
    None,
    Purge,
    Bzip2,
    Lzma,
    Lzma2,
    Zstd,
}

impl DiscCompression {
    fn to_raw(self) -> RawCompression {
        match self {
            Self::None => RawCompression::None,
            Self::Purge => RawCompression::Purge,
            Self::Bzip2 => RawCompression::Bzip2,
            Self::Lzma => RawCompression::Lzma,
            Self::Lzma2 => RawCompression::Lzma2,
            Self::Zstd => RawCompression::Zstd,
        }
    }
}

/// Suggested block size bounds dolphinrvz warns about falling outside of (still valid,
/// just "not ideal for performance" per Dolphin's own convert CLI). Suggested value for
/// RVZ specifically: 131072 (128 KiB).
pub const PREFERRED_MIN_BLOCK_SIZE: u32 = 0x8000;
pub const PREFERRED_MAX_BLOCK_SIZE: u32 = 0x200000;

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

/// Settings for one disc conversion -- the same fields Dolphin's own convert CLI/GUI
/// dialog expose. Construct with [`DolphinConvertArgs::new`] and set the rest via
/// field assignment or the `with_*` builder methods.
#[derive(Debug, Clone)]
pub struct DolphinConvertArgs {
    /// Path to the source disc image (ISO/GCZ/WIA/RVZ/CISO/NFS/etc -- anything
    /// DiscIO recognizes).
    pub input: PathBuf,
    /// Path to write the converted file to.
    pub output: PathBuf,
    pub format: DiscFormat,
    /// Dolphin user directory for temporary processing files, created if it doesn't
    /// exist. `None` uses Dolphin's normal per-platform default user directory.
    pub user_dir: Option<PathBuf>,
    /// Scrub junk data during conversion. Only valid for GameCube/Wii disc images
    /// (ignored, with a warning in the error path, otherwise).
    pub scrub: bool,
    /// Required for GCZ/WIA/RVZ, ignored for ISO. See [`PREFERRED_MIN_BLOCK_SIZE`] /
    /// [`PREFERRED_MAX_BLOCK_SIZE`].
    pub block_size: Option<u32>,
    /// Required for WIA/RVZ, ignored otherwise.
    pub compression: Option<DiscCompression>,
    /// Required for WIA/RVZ when `compression` is set to anything but
    /// [`DiscCompression::None`], ignored otherwise. Valid range depends on the
    /// compression method -- see [`allowed_compression_levels`].
    pub compression_level: Option<i32>,
    /// Compression threads for GCZ/WIA/RVZ creation; `None` = one per logical CPU (Dolphin's
    /// own default). Honoured by the streaming-build calls ([`convert_stream`],
    /// [`convert_from_source`]) on Linux/macOS; ignored by [`convert`] and on Windows.
    pub compress_threads: Option<u32>,
}

impl DolphinConvertArgs {
    pub fn new(input: impl Into<PathBuf>, output: impl Into<PathBuf>, format: DiscFormat) -> Self {
        Self {
            input: input.into(),
            output: output.into(),
            format,
            user_dir: None,
            scrub: false,
            block_size: None,
            compression: None,
            compression_level: None,
            compress_threads: None,
        }
    }

    pub fn with_compress_threads(mut self, threads: u32) -> Self {
        self.compress_threads = Some(threads);
        self
    }

    pub fn with_user_dir(mut self, user_dir: impl Into<PathBuf>) -> Self {
        self.user_dir = Some(user_dir.into());
        self
    }

    pub fn with_scrub(mut self, scrub: bool) -> Self {
        self.scrub = scrub;
        self
    }

    pub fn with_block_size(mut self, block_size: u32) -> Self {
        self.block_size = Some(block_size);
        self
    }

    pub fn with_compression(mut self, compression: DiscCompression, level: i32) -> Self {
        self.compression = Some(compression);
        self.compression_level = Some(level);
        self
    }
}

/// Returns the `[min, max]` compression_level range dolphinrvz accepts for `compression`.
pub fn allowed_compression_levels(compression: DiscCompression) -> Result<(i32, i32), DolphinRvzError> {
    let lib_path = find_any_library()
        .ok_or_else(|| DolphinRvzError::Unavailable("dolphinrvz library not found".to_string()))?;
    let lib = unsafe { Library::new(&lib_path) }.map_err(|err| DolphinRvzError::Unavailable(err.to_string()))?;

    let mut min = 0;
    let mut max = 0;
    unsafe {
        let get_levels: Symbol<DolphinRvzGetAllowedCompressionLevelsFn> = lib
            .get(b"dolphinrvz_get_allowed_compression_levels")
            .map_err(|err| DolphinRvzError::Unavailable(err.to_string()))?;
        get_levels(compression.to_raw(), &mut min, &mut max);
    }
    Ok((min, max))
}

// ---------------------------------------------------------------------------
// Progress callback trampoline
// ---------------------------------------------------------------------------

/// Called periodically during conversion with a status message and progress in
/// `0.0..=100.0`. Return `true` to continue, `false` to cancel (surfaces as
/// [`DolphinRvzError::Cancelled`]).
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

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Runs one conversion, blocking the calling thread until it finishes. `on_progress`
/// is called live with a status message and percent complete (`0.0..=100.0`); return
/// `false` from it to cancel -- call [`convert`] from a thread you've spawned yourself
/// if you want it to run in the background.
pub fn convert<F>(args: &DolphinConvertArgs, mut on_progress: F) -> Result<(), DolphinRvzError>
where
    F: FnMut(&str, f32) -> bool,
{
    let lib_path = find_any_library()
        .ok_or_else(|| DolphinRvzError::Unavailable("dolphinrvz library not found".to_string()))?;
    let lib = unsafe { Library::new(&lib_path) }.map_err(|err| DolphinRvzError::Unavailable(err.to_string()))?;

    let strings = ConvertStrings::new(args)?;
    let raw_options = strings.raw_options(
        args,
        Some(progress_trampoline::<F>),
        &mut on_progress as *mut _ as *mut c_void,
    );

    let _guard = DOLPHINRVZ_CALL_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // The thread setting applies to a file-to-file conversion too, not only the stream ones.
    let _threads = CompressThreads::apply(&lib, args.compress_threads);

    let (rc, message) = unsafe {
        let convert_fn: Symbol<DolphinRvzConvertFn> =
            lib.get(b"dolphinrvz_convert").map_err(|err| DolphinRvzError::Unavailable(err.to_string()))?;
        let get_last_error: Symbol<DolphinRvzGetLastErrorFn> =
            lib.get(b"dolphinrvz_get_last_error").map_err(|err| DolphinRvzError::Unavailable(err.to_string()))?;

        let rc = convert_fn(&raw_options);
        let message = if rc == 0 {
            String::new()
        } else {
            CStr::from_ptr(get_last_error()).to_string_lossy().into_owned()
        };
        (rc, message)
    };

    if rc == 0 {
        Ok(())
    } else if rc == -6 {
        // dolphinrvz_convert's dedicated "on_progress returned 0" return code -- see
        // dolphinrvz.h/dolphinrvz.cpp.
        Err(DolphinRvzError::Cancelled)
    } else {
        Err(DolphinRvzError::ConvertFailed { code: rc, message })
    }
}

fn path_to_cstring(path: &Path) -> Result<CString, DolphinRvzError> {
    let s = path
        .to_str()
        .ok_or_else(|| DolphinRvzError::InvalidArgument(format!("path is not valid UTF-8: {}", path.display())))?;
    CString::new(s).map_err(|err| DolphinRvzError::InvalidArgument(err.to_string()))
}

/// The C strings a [`RawConvertOptions`] points into; must outlive the raw options.
struct ConvertStrings {
    input: CString,
    output: CString,
    user_dir: Option<CString>,
}

impl ConvertStrings {
    fn new(args: &DolphinConvertArgs) -> Result<Self, DolphinRvzError> {
        Ok(Self {
            input: path_to_cstring(&args.input)?,
            output: path_to_cstring(&args.output)?,
            user_dir: args.user_dir.as_deref().map(path_to_cstring).transpose()?,
        })
    }

    fn raw_options(
        &self,
        args: &DolphinConvertArgs,
        on_progress: Option<RawProgressCb>,
        user_data: *mut c_void,
    ) -> RawConvertOptions {
        RawConvertOptions {
            input_path: self.input.as_ptr(),
            output_path: self.output.as_ptr(),
            format: args.format.to_raw(),
            user_dir: self.user_dir.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            scrub: args.scrub as c_int,
            block_size: args.block_size.unwrap_or(0) as c_int,
            compression: args.compression.unwrap_or(DiscCompression::None).to_raw(),
            compression_level: args.compression_level.unwrap_or(0) as c_int,
            on_progress,
            user_data,
        }
    }
}

// ---------------------------------------------------------------------------
// Streaming (streaming build only)
// ---------------------------------------------------------------------------

type SetCompressThreadsFn = unsafe extern "C" fn(u32);

/// Applies `threads` (None = default) to the loaded streaming library for one call; resets
/// to the default when dropped. Must be held under `DOLPHINRVZ_CALL_LOCK`.
struct CompressThreads<'a>(&'a Library, bool);

impl<'a> CompressThreads<'a> {
    fn apply(lib: &'a Library, threads: Option<u32>) -> Self {
        let set = threads.is_some();
        if let (Some(n), Ok(f)) = (threads, unsafe { lib.get::<SetCompressThreadsFn>(b"dolphinrvz_set_compress_threads") }) {
            unsafe { f(n) };
        }
        Self(lib, set)
    }
}

impl Drop for CompressThreads<'_> {
    fn drop(&mut self) {
        if self.1 {
            if let Ok(f) = unsafe { self.0.get::<SetCompressThreadsFn>(b"dolphinrvz_set_compress_threads") } {
                unsafe { f(0) };
            }
        }
    }
}

fn load_variant(variant: native_lib::LibVariant) -> Result<Library, DolphinRvzError> {
    let lib_path = find_library(variant)
        .ok_or_else(|| DolphinRvzError::Unavailable(format!("dolphinrvz library ({variant:?}) not found")))?;
    unsafe { Library::new(&lib_path) }.map_err(|err| DolphinRvzError::Unavailable(err.to_string()))
}

fn not_streaming(_: libloading::Error) -> DolphinRvzError {
    DolphinRvzError::Unavailable("dolphinrvz library is not a streaming build".to_string())
}

unsafe fn last_error(lib: &Library) -> String {
    match lib.get::<DolphinRvzGetLastErrorFn>(b"dolphinrvz_get_last_error") {
        Ok(f) => {
            let p = f();
            if p.is_null() {
                String::new()
            } else {
                CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        }
        Err(_) => String::new(),
    }
}

fn result_from(rc: c_int, lib: &Library) -> Result<(), DolphinRvzError> {
    match rc {
        0 => Ok(()),
        -6 => Err(DolphinRvzError::Cancelled),
        code => Err(DolphinRvzError::ConvertFailed { code, message: unsafe { last_error(lib) } }),
    }
}

/// Both callbacks of one streaming call, behind the single `user_data` pointer the C API
/// hands to each.
struct StreamCallbacks<D, F> {
    on_data: D,
    on_progress: F,
}

extern "C" fn stream_data_trampoline<D, F>(
    file_index: u32,
    file_name: *const c_char,
    offset: u64,
    data: *const c_void,
    size: u32,
    user_data: *mut c_void,
) -> c_int
where
    D: FnMut(u32, &str, u64, &[u8]) -> bool,
{
    if user_data.is_null() {
        return 1;
    }
    let callbacks = unsafe { &mut *(user_data as *mut StreamCallbacks<D, F>) };
    let name = if file_name.is_null() {
        std::borrow::Cow::Borrowed("")
    } else {
        unsafe { CStr::from_ptr(file_name) }.to_string_lossy()
    };
    let chunk = if data.is_null() || size == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(data as *const u8, size as usize) }
    };
    c_int::from(native_lib::guarded(false, || (callbacks.on_data)(file_index, &name, offset, chunk)))
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

/// Decodes any disc image DiscIO reads (RVZ/WIA/GCZ/ISO/...) to its plain ISO bytes in one
/// pass, handing every block to `on_data(file_index, file_name, offset, bytes)` and, if
/// `output` is `Some`, also writing it there -- byte-identical to [`convert`] with
/// [`DiscFormat::Iso`]. A disc image is one file: `file_index` is always 0, `file_name` the
/// output path (`""` when streaming only). Blocks arrive in order, contiguous from 0.
///
/// Returning `false` from either callback stops ([`DolphinRvzError::Cancelled`]; a partial
/// output file is deleted). Needs the streaming build.
pub fn extract_stream<D, F>(
    input: &Path,
    output: Option<&Path>,
    user_dir: Option<&Path>,
    on_data: D,
    on_progress: F,
) -> Result<(), DolphinRvzError>
where
    D: FnMut(u32, &str, u64, &[u8]) -> bool,
    F: FnMut(&str, f32) -> bool,
{
    let lib = load_variant(native_lib::LibVariant::Streaming)?;
    let input_c = path_to_cstring(input)?;
    let output_c = output.map(path_to_cstring).transpose()?;
    let user_dir_c = user_dir.map(path_to_cstring).transpose()?;
    let mut callbacks = StreamCallbacks { on_data, on_progress };

    let _guard = DOLPHINRVZ_CALL_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let rc = unsafe {
        let extract: Symbol<ExtractStreamFn> = lib.get(STREAMING_SYMBOL.as_bytes()).map_err(not_streaming)?;
        extract(
            input_c.as_ptr(),
            output_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            user_dir_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            Some(stream_data_trampoline::<D, F>),
            Some(stream_progress_trampoline::<D, F>),
            &mut callbacks as *mut StreamCallbacks<D, F> as *mut c_void,
        )
    };
    result_from(rc, &lib)
}

/// [`convert`], additionally handing the INPUT disc image's plain bytes to
/// `on_input_data(file_index, file_name, offset, bytes)` -- e.g. to hash an ISO while it is
/// being compressed, without a second read pass. Every byte of the input is delivered
/// exactly once, in order (`file_index` 0, `file_name` = the input path), even though the
/// converter itself reads out of order and partly through a separate reader. The converted
/// output can't be streamed (GCZ/WIA/RVZ writers patch their headers at the end).
///
/// Returning `false` from `on_input_data` cancels at the converter's next progress point
/// ([`DolphinRvzError::Cancelled`]). Needs the streaming build.
pub fn convert_stream<D, F>(args: &DolphinConvertArgs, on_input_data: D, on_progress: F) -> Result<(), DolphinRvzError>
where
    D: FnMut(u32, &str, u64, &[u8]) -> bool,
    F: FnMut(&str, f32) -> bool,
{
    let lib = load_variant(native_lib::LibVariant::Streaming)?;
    let strings = ConvertStrings::new(args)?;
    let mut callbacks = StreamCallbacks { on_data: on_input_data, on_progress };
    let raw_options = strings.raw_options(
        args,
        Some(stream_progress_trampoline::<D, F>),
        &mut callbacks as *mut StreamCallbacks<D, F> as *mut c_void,
    );

    let _guard = DOLPHINRVZ_CALL_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let _threads = CompressThreads::apply(&lib, args.compress_threads);
    let rc = unsafe {
        let convert: Symbol<ConvertStreamFn> = lib.get(b"dolphinrvz_convert_stream").map_err(not_streaming)?;
        convert(&raw_options, Some(stream_data_trampoline::<D, F>))
    };
    result_from(rc, &lib)
}

// ---------------------------------------------------------------------------
// Random access reader (streaming build only)
// ---------------------------------------------------------------------------

/// A disc image's geometry, from [`DiscReader::info`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscInfo {
    /// Plain (ISO) size.
    pub data_size: u64,
    /// Size of the file on disk.
    pub raw_size: u64,
    /// Container block size, 0 if the format has none.
    pub block_size: u64,
    /// `DiscIO::BlobType`: 0 PLAIN, 3 GCZ, 7 WIA, 8 RVZ, ...
    pub blob_type: i32,
    /// `false` if `data_size` is only an upper bound (e.g. some WBFS files).
    pub data_size_accurate: bool,
}

/// Random access to a disc image's plain (ISO) contents without converting it -- e.g.
/// reading a GC/Wii filesystem out of an RVZ. Needs the streaming build.
///
/// A reader owns its own file handle and decoder state, so for parallel reads open one
/// reader per thread; there is no thread cap ([`native_lib::recommended_reader_threads`]
/// gives the machine's logical CPU count). `Send`, not `Sync`.
pub struct DiscReader {
    lib: Library,
    handle: *mut c_void,
}

// The native reader has no thread affinity; it just must not be used from two threads at
// once, which `&mut self` on reads (and no `Sync`) already guarantees.
unsafe impl Send for DiscReader {}

impl DiscReader {
    /// `user_dir`: as in [`DolphinConvertArgs::user_dir`].
    pub fn open(path: &Path, user_dir: Option<&Path>) -> Result<Self, DolphinRvzError> {
        let lib = load_variant(native_lib::LibVariant::Streaming)?;
        let path_c = path_to_cstring(path)?;
        let user_dir_c = user_dir.map(path_to_cstring).transpose()?;
        let handle = unsafe {
            let open: Symbol<ReaderOpenFn> = lib.get(READER_SYMBOL.as_bytes()).map_err(not_streaming)?;
            open(path_c.as_ptr(), user_dir_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()))
        };
        if handle.is_null() {
            let message = unsafe { last_error(&lib) };
            return Err(DolphinRvzError::ConvertFailed { code: -2, message });
        }
        Ok(Self { lib, handle })
    }

    pub fn info(&self) -> Result<DiscInfo, DolphinRvzError> {
        let mut raw = RawReaderInfo::default();
        let rc = unsafe {
            let get_info: Symbol<ReaderInfoFn> =
                self.lib.get(b"dolphinrvz_reader_get_info").map_err(not_streaming)?;
            get_info(self.handle, &mut raw)
        };
        result_from(rc, &self.lib)?;
        Ok(DiscInfo {
            data_size: raw.data_size,
            raw_size: raw.raw_size,
            block_size: raw.block_size,
            blob_type: raw.blob_type,
            data_size_accurate: raw.data_size_accurate != 0,
        })
    }

    /// Fills `buffer` with the plain bytes at `offset`.
    pub fn read(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), DolphinRvzError> {
        let rc = unsafe {
            let read: Symbol<ReaderReadFn> = self.lib.get(b"dolphinrvz_reader_read").map_err(not_streaming)?;
            read(self.handle, offset, buffer.as_mut_ptr() as *mut c_void, buffer.len() as u64)
        };
        result_from(rc, &self.lib)
    }
}

impl Drop for DiscReader {
    fn drop(&mut self) {
        if let Ok(close) = unsafe { self.lib.get::<ReaderCloseFn>(b"dolphinrvz_reader_close") } {
            unsafe { close(self.handle) };
        }
    }
}

// ---------------------------------------------------------------------------
// Creation from a source (streaming build only)
// ---------------------------------------------------------------------------

use super::source::{ReadAt, Source, SourceState};

impl ReadAt for DiscReader {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.read(offset, buf).map_err(std::io::Error::other)
    }
}

#[repr(C)]
struct RawSource {
    size: u64,
    read: Option<super::source::RawReadFn>,
    read_at: Option<super::source::RawReadAtFn>,
    user_data: *mut c_void,
}

type ConvertFromSourceFn = unsafe extern "C" fn(*const RawConvertOptions, *const RawSource, Option<RawDataCb>) -> c_int;

/// [`convert`] with the input disc image (plain ISO bytes) from `source` instead of
/// `args.input` (which is then only the name passed to `on_input_data`; `args.scrub` is not
/// supported). `on_input_data(file_index, file_name, offset, bytes)` receives every input
/// byte once, in order, as in [`convert_stream`].
///
/// GCZ/WIA/RVZ creation reads its input at arbitrary offsets: a random source (e.g.
/// [`DiscReader`], `ChdReader`, a file) is read in place; a sequential one is spooled to a
/// temporary file first (in `args.user_dir`, else the system temp directory). Source reads may
/// come from Dolphin's worker threads, one at a time. Needs the streaming build.
pub fn convert_from_source<D, F>(
    args: &DolphinConvertArgs,
    source: &mut Source,
    on_input_data: D,
    on_progress: F,
) -> Result<(), DolphinRvzError>
where
    D: FnMut(u32, &str, u64, &[u8]) -> bool,
    F: FnMut(&str, f32) -> bool,
{
    let lib = load_variant(native_lib::LibVariant::Streaming)?;
    let strings = ConvertStrings::new(args)?;
    let mut callbacks = StreamCallbacks { on_data: on_input_data, on_progress };
    let raw_options = strings.raw_options(
        args,
        Some(stream_progress_trampoline::<D, F>),
        &mut callbacks as *mut StreamCallbacks<D, F> as *mut c_void,
    );
    let mut state = SourceState::new(source);
    let (read, read_at) = state.callbacks();
    let raw_source = RawSource { size: state.source.size, read, read_at, user_data: &mut state as *mut SourceState as *mut c_void };

    let _guard = DOLPHINRVZ_CALL_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let _threads = CompressThreads::apply(&lib, args.compress_threads);
    let rc = unsafe {
        let convert: Symbol<ConvertFromSourceFn> =
            lib.get(b"dolphinrvz_convert_from_source").map_err(not_streaming)?;
        convert(&raw_options, &raw_source, Some(stream_data_trampoline::<D, F>))
    };
    if rc != 0 && rc != -6 {
        if let Some(err) = state.error.take() {
            return Err(DolphinRvzError::ConvertFailed { code: rc, message: format!("source: {err}") });
        }
    }
    result_from(rc, &lib)
}
