//! Rust wrapper around the chdman shared library built by
//! `deps/chdman-simd/scripts/build_chdman_lib.sh` (see that submodule's
//! `deps/chdman-simd/chdman_lib.h`/`README.chdman-lib.md`).
//!
//! Every chdman command (`createcd`, `extractcd`, `info`, ...) and option is exposed as
//! a typed [`ChdmanCommand`] + [`ChdmanArgs`] pair instead of a raw argv string array --
//! paths are `PathBuf`, flags are `bool`, numeric options are integers, and the
//! compression codec list is [`ChdCompressionCodec`] -- while still mapping 1:1 onto
//! chdman's own CLI surface (`chdman help <command>`), which the underlying C function
//! takes almost verbatim.
//!
//! The native library is located fresh on every call ([`find_library`]): the deployed
//! `app_root_ex/lib/<os>/<arch>/` copy first, then the submodule's most recently built
//! version directory -- rebuilding chdman-simd against a newer upstream MAME/chdman
//! commit is picked up automatically, no code changes needed here.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use libloading::{Library, Symbol};

use super::native_lib;

// ---------------------------------------------------------------------------
// FFI
// ---------------------------------------------------------------------------

type ChdmanProgressCb = extern "C" fn(text: *const c_char, percent: f32, user_data: *mut c_void);
type ChdmanRunFn =
    unsafe extern "C" fn(c_int, *const *const c_char, *mut *mut c_char, Option<ChdmanProgressCb>, *mut c_void) -> c_int;
type ChdmanFreeLogFn = unsafe extern "C" fn(*mut c_char);

// Streaming build only (see chdman_lib.h, CHDMAN_WITH_STREAMING).
type ChdmanDataCb = extern "C" fn(
    file_index: u32,
    file_name: *const c_char,
    offset_in_file: u64,
    data: *const c_void,
    size: u32,
    user_data: *mut c_void,
) -> c_int;
type ChdmanExtractStreamFn = unsafe extern "C" fn(
    c_int,
    *const *const c_char,
    c_int,
    Option<ChdmanDataCb>,
    *mut *mut c_char,
    Option<ChdmanProgressCb>,
    *mut c_void,
) -> c_int;

#[repr(C)]
#[derive(Default)]
struct RawReaderInfo {
    logical_bytes: u64,
    hunk_bytes: u32,
    hunk_count: u32,
    unit_bytes: u32,
    version: u32,
    is_cd: i32,
    is_gdrom: i32,
    num_tracks: u32,
}

#[repr(C)]
#[derive(Default)]
struct RawTrackInfo {
    track_type: u32,
    sub_type: u32,
    data_size: u32,
    sub_size: u32,
    frames: u32,
    pregap: u32,
    postgap: u32,
    session: u32,
    logical_start: u32,
    physical_start: u32,
    chd_frame_start: u32,
}

type ReaderOpenFn = unsafe extern "C" fn(*const c_char, *const c_char, u32) -> *mut c_void;
type ReaderCloseFn = unsafe extern "C" fn(*mut c_void);
type ReaderInfoFn = unsafe extern "C" fn(*mut c_void, *mut RawReaderInfo) -> c_int;
type ReaderReadHunkFn = unsafe extern "C" fn(*mut c_void, u32, *mut c_void) -> c_int;
type ReaderReadBytesFn = unsafe extern "C" fn(*mut c_void, u64, *mut c_void, u32) -> c_int;
type ReaderTrackFn = unsafe extern "C" fn(*mut c_void, u32, *mut RawTrackInfo) -> c_int;
type ReaderReadSectorFn = unsafe extern "C" fn(*mut c_void, u32, *mut c_void, u32, c_int) -> c_int;
type ReaderLastErrorFn = unsafe extern "C" fn() -> *const c_char;

/// chdman's own docs: "Not safe to call concurrently with itself on multiple threads at
/// once: chdman's command handlers use process-wide state (e.g. the shared CHD
/// compressor thread pool)." Serialized here so misuse from Rust can't hit that.
static CHDMAN_CALL_LOCK: Mutex<()> = Mutex::new(());

fn release_root() -> PathBuf {
    native_lib::source_root().join("deps/chdman-simd/release")
}

/// Locates libchdman for either build variant (default vs streaming build). Tries
/// `app_root_ex/lib/<os>/libchdman.<ext>` first (`scripts/move_libs_to_app_root.sh`'s
/// flat layout -- the streaming/standard distinction doesn't apply there, since the
/// script copies whichever variant it was asked to), falling back to the submodule's own
/// `deps/<name>/release/` tree so a dev who's only built chdman-simd directly still works.
pub fn find_library(variant: native_lib::LibVariant) -> Option<PathBuf> {
    native_lib::find_app_root_library("chdman").or_else(|| native_lib::find_native_library(&variant.release_root(&release_root()), "chdman"))
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

/// Exported only by the streaming build of libchdman (`build_chdman_lib.sh`'s default build; `--nostreaming` omits it);
/// its presence is what makes a library "streamable" (see [`run_stream`]).
pub const STREAMING_SYMBOL: &str = "chdman_extract_stream";

/// Exported only by the streaming build: the random-access [`ChdReader`] API.
pub const READER_SYMBOL: &str = "chdman_reader_open";

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChdmanError {
    /// The chdman native library isn't built for this platform/arch (see
    /// `deps/chdman-simd/scripts/build_chdman_lib.sh`). Not a command
    /// failure.
    Unavailable(String),
    /// A malformed argument was caught on the Rust side before ever calling chdman
    /// (e.g. a metadata tag that isn't exactly 4 characters).
    InvalidArgument(String),
    /// chdman ran and returned a non-zero exit code. `log` has its full captured
    /// stdout/stderr output, which almost always explains why.
    CommandFailed { exit_code: i32, log: String },
    /// A [`run_stream`] consumer returned `false`. `log` is chdman's output up to then.
    Cancelled { log: String },
    /// A [`ChdReader`] call failed; the text is the native library's error message.
    ReadFailed(String),
}

impl std::fmt::Display for ChdmanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(message) => write!(f, "chdman unavailable: {message}"),
            Self::InvalidArgument(message) => write!(f, "invalid chdman argument: {message}"),
            Self::CommandFailed { exit_code, log } => write!(f, "chdman exited with code {exit_code}: {log}"),
            Self::Cancelled { .. } => write!(f, "chdman extraction stopped by the stream consumer"),
            Self::ReadFailed(message) => write!(f, "CHD read failed: {message}"),
        }
    }
}

impl std::error::Error for ChdmanError {}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// One of chdman's commands (`chdman help` lists these). See [`ChdmanArgs`] for which
/// fields each command reads; unused fields are simply ignored when building the
/// underlying argv, mirroring how chdman's CLI itself only accepts certain options per
/// command (passing an option a command doesn't recognize is a chdman-side error, not a
/// Rust-side one -- chdman's own validation reports it via [`ChdmanError::CommandFailed`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChdmanCommand {
    /// Displays information about a CHD. Reads: `input`, `verbose`.
    Info,
    /// Verifies a CHD's integrity. Reads: `input`, `input_parent`, `fix`.
    Verify,
    /// Creates a raw CHD from an input file. Reads: `output`, `output_parent`,
    /// `output_force`, `input`, `input_start_byte`, `input_start_hunk`,
    /// `input_length_bytes`, `input_length_hunks`, `hunk_size`, `unit_size`,
    /// `compression`, `num_processors`.
    CreateRaw,
    /// Creates a hard disk CHD from an input file (or a blank one from `size`/`chs` if
    /// `input` is omitted). Reads: `output`, `output_parent`, `output_force`, `input`,
    /// `input_start_byte`, `input_start_hunk`, `input_length_bytes`,
    /// `input_length_hunks`, `hunk_size`, `compression`, `template_id`, `ident`, `chs`,
    /// `size`, `sector_size`, `num_processors`.
    CreateHd,
    /// Creates a CD CHD from an input file (.cue/.gdi/.toc/.iso). Reads: `output`,
    /// `output_parent`, `output_force`, `input`, `hunk_size`, `compression`,
    /// `num_processors`.
    CreateCd,
    /// Creates a DVD CHD from an input file. Reads: `output`, `output_parent`,
    /// `output_force`, `input`, `input_start_byte`, `input_start_hunk`,
    /// `input_length_bytes`, `input_length_hunks`, `hunk_size`, `compression`,
    /// `num_processors`.
    CreateDvd,
    /// Creates a laserdisc CHD from an input AVI. Reads: `output`, `output_parent`,
    /// `output_force`, `input`, `input_start_frame`, `input_length_frames`,
    /// `hunk_size`, `compression`, `num_processors`.
    CreateLd,
    /// Extracts a raw file from a CHD. Reads: `output`, `output_force`, `input`,
    /// `input_parent`, `input_start_byte`, `input_start_hunk`, `input_length_bytes`,
    /// `input_length_hunks`.
    ExtractRaw,
    /// Extracts a raw hard disk file from a CHD. Same fields as [`Self::ExtractRaw`].
    ExtractHd,
    /// Extracts a CD file (.cue + .bin(s)/.gdi) from a CHD. Reads: `output`,
    /// `output_bin`, `output_split_bin`, `output_force`, `input`, `input_parent`.
    ExtractCd,
    /// Extracts a DVD file from a CHD. Same fields as [`Self::ExtractRaw`].
    ExtractDvd,
    /// Extracts a laserdisc AVI from a CHD. Reads: `output`, `output_force`, `input`,
    /// `input_parent`, `input_start_frame`, `input_length_frames`.
    ExtractLd,
    /// Copies data from one CHD to another of the same type. Reads: `output`,
    /// `output_parent`, `output_force`, `input`, `input_parent`, `input_start_byte`,
    /// `input_start_hunk`, `input_length_bytes`, `input_length_hunks`, `hunk_size`,
    /// `compression`, `num_processors`.
    Copy,
    /// Adds metadata to a CHD. Reads: `input`, `tag` (required), `index`, `value_text`,
    /// `value_file`, `no_checksum`.
    AddMetadata,
    /// Removes metadata from a CHD. Reads: `input`, `tag` (required), `index`.
    DelMetadata,
    /// Dumps metadata from a CHD to stdout (captured in the returned log) or to
    /// `output` if given. Reads: `input`, `output`, `output_force`, `tag` (required),
    /// `index`.
    DumpMetadata,
    /// Lists the built-in hard disk templates (`create_hd`'s `template_id`). Reads
    /// nothing.
    ListTemplates,
}

impl ChdmanCommand {
    fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Verify => "verify",
            Self::CreateRaw => "createraw",
            Self::CreateHd => "createhd",
            Self::CreateCd => "createcd",
            Self::CreateDvd => "createdvd",
            Self::CreateLd => "createld",
            Self::ExtractRaw => "extractraw",
            Self::ExtractHd => "extracthd",
            Self::ExtractCd => "extractcd",
            Self::ExtractDvd => "extractdvd",
            Self::ExtractLd => "extractld",
            Self::Copy => "copy",
            Self::AddMetadata => "addmeta",
            Self::DelMetadata => "delmeta",
            Self::DumpMetadata => "dumpmeta",
            Self::ListTemplates => "listtemplates",
        }
    }
}

/// A CHD compression codec, by its chdman `-c` 4-character tag (e.g. `lzma`). Up to 4
/// may be given via [`ChdmanArgs::compression`]; chdman tries each in turn per hunk and
/// keeps whichever compresses best.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChdCompressionCodec {
    /// No compression (also selectable directly via `ChdmanArgs::compression = Some(vec![])`
    /// with chdman's `-c none`, which this maps to).
    None,
    Zlib,
    Zstd,
    Lzma,
    Huffman,
    Flac,
    /// CD-specific zlib variant (chdman tag `cdzl`).
    CdZlib,
    /// CD-specific zstd variant (chdman tag `cdzs`).
    CdZstd,
    /// CD-specific LZMA variant (chdman tag `cdlz`).
    CdLzma,
    /// CD-specific FLAC variant (chdman tag `cdfl`).
    CdFlac,
    /// A/V Huffman -- laserdisc CHDs' only supported codec (chdman tag `avhu`).
    AvHuff,
}

impl ChdCompressionCodec {
    fn as_tag(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Zlib => "zlib",
            Self::Zstd => "zstd",
            Self::Lzma => "lzma",
            Self::Huffman => "huff",
            Self::Flac => "flac",
            Self::CdZlib => "cdzl",
            Self::CdZstd => "cdzs",
            Self::CdLzma => "cdlz",
            Self::CdFlac => "cdfl",
            Self::AvHuff => "avhu",
        }
    }
}

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

/// All options any chdman command accepts, as typed fields instead of CLI flags. See
/// each [`ChdmanCommand`] variant's doc comment for which fields it actually reads --
/// the rest are ignored for that command. Construct with [`ChdmanArgs::default`] and
/// set only what you need.
#[derive(Debug, Clone, Default)]
pub struct ChdmanArgs {
    pub input: Option<PathBuf>,
    pub input_parent: Option<PathBuf>,
    pub output: Option<PathBuf>,
    pub output_bin: Option<PathBuf>,
    pub output_split_bin: bool,
    pub output_force: bool,
    pub output_parent: Option<PathBuf>,
    pub input_start_byte: Option<u64>,
    pub input_start_hunk: Option<u64>,
    pub input_start_frame: Option<u64>,
    pub input_length_bytes: Option<u64>,
    pub input_length_hunks: Option<u64>,
    pub input_length_frames: Option<u64>,
    pub hunk_size: Option<u32>,
    pub unit_size: Option<u32>,
    /// Up to 4 codecs, tried in order per hunk (chdman keeps whichever compresses
    /// best). `Some(vec![])` or `Some(vec![ChdCompressionCodec::None])` means "-c none"
    /// (uncompressed); `None` means "don't pass -c at all" (chdman picks its own
    /// per-command default).
    pub compression: Option<Vec<ChdCompressionCodec>>,
    pub ident: Option<PathBuf>,
    /// Cylinders, heads, sectors.
    pub chs: Option<(u32, u32, u32)>,
    pub sector_size: Option<u32>,
    /// Metadata tag: exactly 4 characters (validated before calling chdman).
    pub tag: Option<String>,
    pub index: Option<u32>,
    pub value_text: Option<String>,
    pub value_file: Option<PathBuf>,
    pub no_checksum: bool,
    pub fix: bool,
    pub verbose: bool,
    pub num_processors: Option<u32>,
    pub size: Option<u64>,
    /// Hard disk template id, as listed by [`ChdmanCommand::ListTemplates`].
    pub template_id: Option<u32>,
}

fn push_path(argv: &mut Vec<String>, flag: &str, path: &Option<PathBuf>) {
    if let Some(path) = path {
        argv.push(flag.to_string());
        argv.push(path.to_string_lossy().into_owned());
    }
}

fn push_num<T: ToString>(argv: &mut Vec<String>, flag: &str, value: &Option<T>) {
    if let Some(value) = value {
        argv.push(flag.to_string());
        argv.push(value.to_string());
    }
}

fn build_argv(command: ChdmanCommand, args: &ChdmanArgs) -> Result<Vec<String>, ChdmanError> {
    let mut argv = vec![command.as_str().to_string()];

    push_path(&mut argv, "-i", &args.input);
    push_path(&mut argv, "-ip", &args.input_parent);
    push_path(&mut argv, "-o", &args.output);
    push_path(&mut argv, "-ob", &args.output_bin);
    if args.output_split_bin {
        argv.push("-sb".to_string());
    }
    if args.output_force {
        argv.push("-f".to_string());
    }
    push_path(&mut argv, "-op", &args.output_parent);
    push_num(&mut argv, "-isb", &args.input_start_byte);
    push_num(&mut argv, "-ish", &args.input_start_hunk);
    push_num(&mut argv, "-isf", &args.input_start_frame);
    push_num(&mut argv, "-ib", &args.input_length_bytes);
    push_num(&mut argv, "-ih", &args.input_length_hunks);
    push_num(&mut argv, "-if", &args.input_length_frames);
    push_num(&mut argv, "-hs", &args.hunk_size);
    push_num(&mut argv, "-us", &args.unit_size);

    if let Some(codecs) = &args.compression {
        if codecs.len() > 4 {
            return Err(ChdmanError::InvalidArgument(format!(
                "compression accepts at most 4 codecs, got {}",
                codecs.len()
            )));
        }
        let joined = if codecs.is_empty() {
            "none".to_string()
        } else {
            codecs.iter().map(|c| c.as_tag()).collect::<Vec<_>>().join(",")
        };
        argv.push("-c".to_string());
        argv.push(joined);
    }

    push_path(&mut argv, "-id", &args.ident);
    if let Some((c, h, s)) = args.chs {
        argv.push("-chs".to_string());
        argv.push(format!("{c},{h},{s}"));
    }
    push_num(&mut argv, "-ss", &args.sector_size);

    if let Some(tag) = &args.tag {
        if tag.len() != 4 {
            return Err(ChdmanError::InvalidArgument(format!(
                "metadata tag must be exactly 4 characters, got {:?}",
                tag
            )));
        }
        argv.push("-t".to_string());
        argv.push(tag.clone());
    }
    push_num(&mut argv, "-ix", &args.index);
    if let Some(text) = &args.value_text {
        argv.push("-vt".to_string());
        argv.push(text.clone());
    }
    push_path(&mut argv, "-vf", &args.value_file);
    if args.no_checksum {
        argv.push("-nocs".to_string());
    }
    if args.fix {
        argv.push("-f".to_string());
    }
    if args.verbose {
        argv.push("-v".to_string());
    }
    push_num(&mut argv, "-np", &args.num_processors);
    push_num(&mut argv, "-s", &args.size);
    push_num(&mut argv, "-tp", &args.template_id);

    Ok(argv)
}

// ---------------------------------------------------------------------------
// Progress callback trampoline
// ---------------------------------------------------------------------------

/// Called for every line chdman writes, as it happens. `percent` is the parsed value
/// from a `"<float>% complete"` segment when the line has one, or `-1.0` otherwise
/// (treat that as "no change to the last known percent").
extern "C" fn progress_trampoline<F: FnMut(&str, f32)>(
    text: *const c_char,
    percent: f32,
    user_data: *mut c_void,
) {
    if text.is_null() || user_data.is_null() {
        return;
    }
    let text = unsafe { CStr::from_ptr(text) }.to_string_lossy();
    let callback = unsafe { &mut *(user_data as *mut F) };
    native_lib::guarded((), || callback(&text, percent));
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Runs a chdman command, blocking the calling thread until it finishes. `on_progress`
/// is called live, once per line chdman writes to stdout/stderr (see
/// [`progress_trampoline`]) -- call this from a thread you've spawned yourself if you
/// want it to run in the background.
///
/// Returns the full captured stdout/stderr text on success (exit code 0). A non-zero
/// exit code is reported as [`ChdmanError::CommandFailed`], with that same text
/// available in its `log` field.
pub fn run<F>(command: ChdmanCommand, args: &ChdmanArgs, on_progress: F) -> Result<String, ChdmanError>
where
    F: FnMut(&str, f32),
{
    run_with(command, args, on_progress, true).map(|log| log.unwrap_or_default())
}

/// [`run`], but only when no other chdman call is running: `Ok(None)` instead of waiting
/// behind one (a CHD conversion can hold chdman for tens of minutes). For a quick call
/// whose answer can be had another way, such as `info` checked against the Rust reader.
pub fn run_if_free<F>(command: ChdmanCommand, args: &ChdmanArgs, on_progress: F) -> Result<Option<String>, ChdmanError>
where
    F: FnMut(&str, f32),
{
    run_with(command, args, on_progress, false)
}

fn run_with<F>(command: ChdmanCommand, args: &ChdmanArgs, mut on_progress: F, wait: bool) -> Result<Option<String>, ChdmanError>
where
    F: FnMut(&str, f32),
{
    let argv = build_argv(command, args)?;
    // Opt-in diagnostic: dump the exact argv chdman receives, for debugging a command
    // that behaves unexpectedly (e.g. tracking down the `osd_get_command_line` issue
    // documented in `deps/chdman-simd/chdman_lib.cpp`).
    if std::env::var_os("ROMEXPLORER_DEBUG_CHDMAN_ARGV").is_some() {
        eprintln!("chdman argv: {argv:?}");
    }

    let lib_path = find_any_library()
        .ok_or_else(|| ChdmanError::Unavailable("chdman library not found".to_string()))?;
    let lib = unsafe { Library::new(&lib_path) }.map_err(|err| ChdmanError::Unavailable(err.to_string()))?;

    let argv_cstrings: Vec<CString> = argv
        .into_iter()
        .map(|s| CString::new(s).map_err(|err| ChdmanError::InvalidArgument(err.to_string())))
        .collect::<Result<_, _>>()?;
    let argv_ptrs: Vec<*const c_char> = argv_cstrings.iter().map(|s| s.as_ptr()).collect();

    let mut out_log: *mut c_char = std::ptr::null_mut();
    let user_data = &mut on_progress as *mut _ as *mut c_void;

    let _guard = if wait {
        CHDMAN_CALL_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    } else {
        match CHDMAN_CALL_LOCK.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return Ok(None),
        }
    };

    let rc = unsafe {
        let chdman_run: Symbol<ChdmanRunFn> =
            lib.get(b"chdman_run").map_err(|err| ChdmanError::Unavailable(err.to_string()))?;
        let chdman_free_log: Symbol<ChdmanFreeLogFn> =
            lib.get(b"chdman_free_log").map_err(|err| ChdmanError::Unavailable(err.to_string()))?;

        let rc = chdman_run(
            argv_ptrs.len() as c_int,
            argv_ptrs.as_ptr(),
            &mut out_log,
            Some(progress_trampoline::<F>),
            user_data,
        );

        let log = if out_log.is_null() {
            String::new()
        } else {
            let text = CStr::from_ptr(out_log).to_string_lossy().into_owned();
            chdman_free_log(out_log);
            text
        };

        (rc, log)
    };

    let (exit_code, log) = rc;
    if exit_code == 0 {
        Ok(Some(log))
    } else {
        Err(ChdmanError::CommandFailed { exit_code, log })
    }
}

// ---------------------------------------------------------------------------
// Streaming extract (streaming build only)
// ---------------------------------------------------------------------------

fn load_variant(variant: native_lib::LibVariant) -> Result<Library, ChdmanError> {
    let lib_path = find_library(variant)
        .ok_or_else(|| ChdmanError::Unavailable(format!("chdman library ({variant:?}) not found")))?;
    unsafe { Library::new(&lib_path) }.map_err(|err| ChdmanError::Unavailable(err.to_string()))
}

/// Both callbacks of one [`run_stream`] call, behind the single `user_data` pointer the C
/// API hands to each.
struct StreamCallbacks<D, F> {
    on_data: D,
    on_progress: F,
}

extern "C" fn stream_data_trampoline<D, F>(
    file_index: u32,
    file_name: *const c_char,
    offset_in_file: u64,
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
    c_int::from(native_lib::guarded(false, || (callbacks.on_data)(file_index, &name, offset_in_file, chunk)))
}

extern "C" fn stream_progress_trampoline<D, F>(text: *const c_char, percent: f32, user_data: *mut c_void)
where
    F: FnMut(&str, f32),
{
    if user_data.is_null() {
        return;
    }
    let callbacks = unsafe { &mut *(user_data as *mut StreamCallbacks<D, F>) };
    progress_trampoline::<F>(text, percent, &mut callbacks.on_progress as *mut F as *mut c_void);
}

/// [`run`] for [`ChdmanCommand::ExtractRaw`] / `ExtractHd` / `ExtractDvd` / `ExtractCd`,
/// additionally handing every block chdman writes to
/// `on_data(file_index, file_name, offset_in_file, bytes)` -- one decompression pass
/// feeding the disk and a hasher / re-archiver at once. Needs the streaming build.
///
/// `file_index` numbers output files in first-write order and `file_name` is the path
/// chdman writes that file to, so each member (every track bin of a split cue/gdi, then
/// the sheet itself for `ExtractCd`, delivered last) can be hashed or re-archived as its
/// own file. Blocks of one file arrive in order with contiguous offsets from 0. The bytes
/// are exactly what chdman writes: the hook sits at its own write calls.
///
/// `write_files = false` streams only: nothing is left on disk (chdman still opens its
/// output paths, so they must be writable -- a temp dir is fine -- and every file it
/// created is removed before returning).
///
/// Returning `false` from `on_data` stops and yields [`ChdmanError::Cancelled`].
pub fn run_stream<D, F>(
    command: ChdmanCommand,
    args: &ChdmanArgs,
    write_files: bool,
    on_data: D,
    on_progress: F,
) -> Result<String, ChdmanError>
where
    D: FnMut(u32, &str, u64, &[u8]) -> bool,
    F: FnMut(&str, f32),
{
    let argv = build_argv(command, args)?;
    let lib = load_variant(native_lib::LibVariant::Streaming)?;

    let argv_cstrings: Vec<CString> = argv
        .into_iter()
        .map(|s| CString::new(s).map_err(|err| ChdmanError::InvalidArgument(err.to_string())))
        .collect::<Result<_, _>>()?;
    let argv_ptrs: Vec<*const c_char> = argv_cstrings.iter().map(|s| s.as_ptr()).collect();

    let mut callbacks = StreamCallbacks { on_data, on_progress };
    let mut out_log: *mut c_char = std::ptr::null_mut();

    let _guard = CHDMAN_CALL_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let (exit_code, log) = unsafe {
        let extract_stream: Symbol<ChdmanExtractStreamFn> = lib
            .get(STREAMING_SYMBOL.as_bytes())
            .map_err(|_| ChdmanError::Unavailable("chdman library is not a streaming build".to_string()))?;
        let chdman_free_log: Symbol<ChdmanFreeLogFn> =
            lib.get(b"chdman_free_log").map_err(|err| ChdmanError::Unavailable(err.to_string()))?;

        let rc = extract_stream(
            argv_ptrs.len() as c_int,
            argv_ptrs.as_ptr(),
            c_int::from(write_files),
            Some(stream_data_trampoline::<D, F>),
            &mut out_log,
            Some(stream_progress_trampoline::<D, F>),
            &mut callbacks as *mut StreamCallbacks<D, F> as *mut c_void,
        );
        let log = if out_log.is_null() {
            String::new()
        } else {
            let text = CStr::from_ptr(out_log).to_string_lossy().into_owned();
            chdman_free_log(out_log);
            text
        };
        (rc, log)
    };

    match exit_code {
        0 => Ok(log),
        -6 => Err(ChdmanError::Cancelled { log }),
        exit_code => Err(ChdmanError::CommandFailed { exit_code, log }),
    }
}

// ---------------------------------------------------------------------------
// Random access reader (streaming build only)
// ---------------------------------------------------------------------------

/// A CHD's geometry, from [`ChdReader::info`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChdInfo {
    /// Decompressed size (for a DVD CHD, the ISO size).
    pub logical_bytes: u64,
    pub hunk_bytes: u32,
    pub hunk_count: u32,
    /// e.g. 2448 (one CD frame: 2352 data + 96 subcode), 2048 for DVD.
    pub unit_bytes: u32,
    /// CHD format version (3, 4, 5).
    pub version: u32,
    /// CD or GD-ROM: [`ChdReader::track`] / [`ChdReader::read_sector`] are valid.
    pub is_cd: bool,
    pub is_gdrom: bool,
    pub num_tracks: u32,
}

/// One CD/GD-ROM track, from [`ChdReader::track`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChdTrack {
    /// One of the `CD_TRACK_*` constants.
    pub track_type: u32,
    /// 0 = cooked, 1 = raw, 2 = none.
    pub sub_type: u32,
    pub data_size: u32,
    pub sub_size: u32,
    pub frames: u32,
    pub pregap: u32,
    pub postgap: u32,
    pub session: u32,
    /// First logical LBA of the track data.
    pub logical_start: u32,
    /// First physical frame of the track data (for `phys = true` sector reads).
    pub physical_start: u32,
    pub chd_frame_start: u32,
}

/// CD sector/track types, for [`ChdTrack::track_type`] and [`ChdReader::read_sector`].
pub const CD_TRACK_MODE1: u32 = 0; // 2048
pub const CD_TRACK_MODE1_RAW: u32 = 1; // 2352
pub const CD_TRACK_MODE2: u32 = 2; // 2336
pub const CD_TRACK_MODE2_FORM1: u32 = 3; // 2048
pub const CD_TRACK_MODE2_FORM2: u32 = 4; // 2324
pub const CD_TRACK_MODE2_FORM_MIX: u32 = 5; // 2336
pub const CD_TRACK_MODE2_RAW: u32 = 6; // 2352
pub const CD_TRACK_AUDIO: u32 = 7; // 2352
pub const CD_TRACK_RAW_DONTCARE: u32 = 8; // whatever the track stores

/// Largest sector [`ChdReader::read_sector`] can return.
pub const CD_MAX_SECTOR_BYTES: usize = 2352;

/// Random access to a CHD's decompressed contents without extracting it -- e.g. walking an
/// ISO9660 filesystem inside a CD/DVD CHD, or pulling metadata sectors. Needs the streaming
/// build.
///
/// Every CHD hunk is compressed independently and the hunk map is decoded at open, so any
/// hunk costs one seek plus one decompression, in any order. The CHD format is unchanged.
///
/// **Parallelism:** a reader shares nothing (file handle, decompressors, cache) with other
/// readers or with [`run`], so for parallel reads open one reader per thread -- the same
/// CHD any number of times. There is no thread cap; [`recommended_reader_threads`] gives
/// the machine's logical CPU count, past which throughput stops rising (decompression is
/// CPU-bound) and starts falling. A reader is `Send` but not `Sync`: move it into a thread,
/// don't share it.
///
/// Memory per reader is mostly decompressor state -- measured ~0.6 MiB for a CD CHD and
/// ~15 MiB for a DVD CHD -- plus `cache_hunks * hunk_bytes`.
pub struct ChdReader {
    lib: Library,
    handle: *mut c_void,
}

// The native reader has no thread affinity; it just must not be used from two threads at
// once, which `&mut self` on every read (and no `Sync`) already guarantees.
unsafe impl Send for ChdReader {}

/// How many [`ChdReader`]s to run in parallel by default: this machine's logical CPU count
/// (no fixed cap -- a 32-core machine gets 32). Callers may pass any other number.
pub fn recommended_reader_threads() -> usize {
    native_lib::recommended_reader_threads()
}

impl ChdReader {
    /// Opens `path` (with `parent` for a delta CHD). `cache_hunks` is how many decompressed
    /// hunks [`Self::read_bytes`] keeps (LRU) for reads that don't cover a whole hunk --
    /// worth a few when reading scattered small structures such as filesystem metadata;
    /// 0 keeps only chdman's own single-hunk cache.
    pub fn open(path: &Path, parent: Option<&Path>, cache_hunks: u32) -> Result<Self, ChdmanError> {
        let lib = load_variant(native_lib::LibVariant::Streaming)?;
        let to_c = |p: &Path| {
            p.to_str()
                .ok_or_else(|| ChdmanError::InvalidArgument(format!("path is not valid UTF-8: {}", p.display())))
                .and_then(|s| CString::new(s).map_err(|err| ChdmanError::InvalidArgument(err.to_string())))
        };
        let path_c = to_c(path)?;
        let parent_c = parent.map(to_c).transpose()?;

        let handle = unsafe {
            let open: Symbol<ReaderOpenFn> = lib
                .get(READER_SYMBOL.as_bytes())
                .map_err(|_| ChdmanError::Unavailable("chdman library is not a streaming build".to_string()))?;
            open(path_c.as_ptr(), parent_c.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()), cache_hunks)
        };
        if handle.is_null() {
            return Err(ChdmanError::ReadFailed(last_reader_error(&lib)));
        }
        Ok(Self { lib, handle })
    }

    fn check(&self, rc: c_int) -> Result<(), ChdmanError> {
        if rc == 0 {
            Ok(())
        } else {
            Err(ChdmanError::ReadFailed(last_reader_error(&self.lib)))
        }
    }

    fn symbol<T>(&self, name: &str) -> Result<Symbol<'_, T>, ChdmanError> {
        unsafe { self.lib.get(name.as_bytes()) }.map_err(|err| ChdmanError::Unavailable(err.to_string()))
    }

    pub fn info(&self) -> Result<ChdInfo, ChdmanError> {
        let mut raw = RawReaderInfo::default();
        let get_info: Symbol<ReaderInfoFn> = self.symbol("chdman_reader_get_info")?;
        self.check(unsafe { get_info(self.handle, &mut raw) })?;
        Ok(ChdInfo {
            logical_bytes: raw.logical_bytes,
            hunk_bytes: raw.hunk_bytes,
            hunk_count: raw.hunk_count,
            unit_bytes: raw.unit_bytes,
            version: raw.version,
            is_cd: raw.is_cd != 0,
            is_gdrom: raw.is_gdrom != 0,
            num_tracks: raw.num_tracks,
        })
    }

    /// Decompresses hunk `hunk` into `buffer`, which must be at least `hunk_bytes` long.
    pub fn read_hunk(&mut self, hunk: u32, buffer: &mut [u8]) -> Result<(), ChdmanError> {
        let hunk_bytes = self.info()?.hunk_bytes as usize;
        if buffer.len() < hunk_bytes {
            return Err(ChdmanError::InvalidArgument(format!("buffer {} < hunk size {hunk_bytes}", buffer.len())));
        }
        let read_hunk: Symbol<ReaderReadHunkFn> = self.symbol("chdman_reader_read_hunk")?;
        self.check(unsafe { read_hunk(self.handle, hunk, buffer.as_mut_ptr() as *mut c_void) })
    }

    /// Fills `buffer` with the decompressed bytes at `offset` (any alignment/length).
    pub fn read_bytes(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), ChdmanError> {
        let read_bytes: Symbol<ReaderReadBytesFn> = self.symbol("chdman_reader_read_bytes")?;
        for (i, chunk) in buffer.chunks_mut(u32::MAX as usize).enumerate() {
            let at = offset + (i as u64) * (u32::MAX as u64);
            self.check(unsafe { read_bytes(self.handle, at, chunk.as_mut_ptr() as *mut c_void, chunk.len() as u32) })?;
        }
        Ok(())
    }

    /// CD/GD-ROM only. `track` is 0-based.
    pub fn track(&self, track: u32) -> Result<ChdTrack, ChdmanError> {
        let mut raw = RawTrackInfo::default();
        let get_track: Symbol<ReaderTrackFn> = self.symbol("chdman_reader_get_track")?;
        self.check(unsafe { get_track(self.handle, track, &mut raw) })?;
        Ok(ChdTrack {
            track_type: raw.track_type,
            sub_type: raw.sub_type,
            data_size: raw.data_size,
            sub_size: raw.sub_size,
            frames: raw.frames,
            pregap: raw.pregap,
            postgap: raw.postgap,
            session: raw.session,
            logical_start: raw.logical_start,
            physical_start: raw.physical_start,
            chd_frame_start: raw.chd_frame_start,
        })
    }

    /// CD/GD-ROM only: reads sector `lba` (logical, or physical if `phys`) as `datatype`
    /// -- e.g. [`CD_TRACK_MODE1`] for the 2048-byte user data an ISO9660 walk wants, from a
    /// MODE1 or MODE1_RAW track. `buffer` must hold [`CD_MAX_SECTOR_BYTES`].
    pub fn read_sector(&mut self, lba: u32, buffer: &mut [u8], datatype: u32, phys: bool) -> Result<(), ChdmanError> {
        if buffer.len() < CD_MAX_SECTOR_BYTES {
            return Err(ChdmanError::InvalidArgument(format!("sector buffer must be >= {CD_MAX_SECTOR_BYTES} bytes")));
        }
        let read_sector: Symbol<ReaderReadSectorFn> = self.symbol("chdman_reader_read_sector")?;
        self.check(unsafe {
            read_sector(self.handle, lba, buffer.as_mut_ptr() as *mut c_void, datatype, c_int::from(phys))
        })
    }
}

impl Drop for ChdReader {
    fn drop(&mut self) {
        if let Ok(close) = unsafe { self.lib.get::<ReaderCloseFn>(b"chdman_reader_close") } {
            unsafe { close(self.handle) };
        }
    }
}

fn last_reader_error(lib: &Library) -> String {
    unsafe {
        match lib.get::<ReaderLastErrorFn>(b"chdman_reader_last_error") {
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
}

// ---------------------------------------------------------------------------
// Creation from sources (streaming build only)
// ---------------------------------------------------------------------------

use super::source::{ReadAt, Source, SourceState};

impl ReadAt for ChdReader {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.read_bytes(offset, buf).map_err(std::io::Error::other)
    }
}

#[repr(C)]
struct RawSource {
    name: *const c_char,
    size: u64,
    read: Option<super::source::RawReadFn>,
    read_at: Option<super::source::RawReadAtFn>,
    user_data: *mut c_void,
}

type CreateFromSourcesFn = unsafe extern "C" fn(
    c_int,
    *const *const c_char,
    *const RawSource,
    c_int,
    *mut *mut c_char,
    Option<ChdmanProgressCb>,
    *mut c_void,
) -> c_int;

/// [`run`] for `CreateRaw` / `CreateHd` / `CreateDvd` / `CreateCd` with inputs from `sources`
/// instead of files. `args.input` must be the [`Source::name`] of one source:
/// - raw/hd/dvd: that source is the image;
/// - cd: that source is the `.cue`/`.gdi`/`.toc` sheet, and the track files it names are
///   the other sources, matched by those names.
///
/// chdman reads each input front to back; random sources are read in place, sequential ones
/// through a history window (see `chdman_lib.h`). Wrap a source with [`Source::hashed`] to
/// hash it while it's compressed. Source callbacks run on chdman's IO thread. Needs the
/// streaming build.
pub fn create_from_sources<F>(
    command: ChdmanCommand,
    args: &ChdmanArgs,
    sources: &mut [Source],
    mut on_progress: F,
) -> Result<String, ChdmanError>
where
    F: FnMut(&str, f32),
{
    let argv = build_argv(command, args)?;
    let lib = load_variant(native_lib::LibVariant::Streaming)?;
    let argv_cstrings: Vec<CString> = argv
        .into_iter()
        .map(|s| CString::new(s).map_err(|err| ChdmanError::InvalidArgument(err.to_string())))
        .collect::<Result<_, _>>()?;
    let argv_ptrs: Vec<*const c_char> = argv_cstrings.iter().map(|s| s.as_ptr()).collect();

    let names: Vec<CString> = sources
        .iter()
        .map(|s| CString::new(s.name.as_str()).map_err(|err| ChdmanError::InvalidArgument(err.to_string())))
        .collect::<Result<_, _>>()?;
    let mut states: Vec<SourceState> = sources.iter_mut().map(SourceState::new).collect();
    let raw_sources: Vec<RawSource> = states
        .iter_mut()
        .zip(&names)
        .map(|(state, name)| {
            let (read, read_at) = state.callbacks();
            RawSource {
                name: name.as_ptr(),
                size: state.source.size,
                read,
                read_at,
                user_data: state as *mut SourceState as *mut c_void,
            }
        })
        .collect();

    let mut out_log: *mut c_char = std::ptr::null_mut();
    let _guard = CHDMAN_CALL_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (exit_code, log) = unsafe {
        let create: Symbol<CreateFromSourcesFn> = lib
            .get(b"chdman_create_from_sources")
            .map_err(|_| ChdmanError::Unavailable("chdman library is not a streaming build".to_string()))?;
        let free_log: Symbol<ChdmanFreeLogFn> =
            lib.get(b"chdman_free_log").map_err(|err| ChdmanError::Unavailable(err.to_string()))?;
        let rc = create(
            argv_ptrs.len() as c_int,
            argv_ptrs.as_ptr(),
            raw_sources.as_ptr(),
            raw_sources.len() as c_int,
            &mut out_log,
            Some(progress_trampoline::<F>),
            &mut on_progress as *mut _ as *mut c_void,
        );
        let log = if out_log.is_null() {
            String::new()
        } else {
            let text = CStr::from_ptr(out_log).to_string_lossy().into_owned();
            free_log(out_log);
            text
        };
        (rc, log)
    };
    if exit_code == 0 {
        return Ok(log);
    }
    let source_error = states.iter_mut().find_map(|s| s.error.take());
    let log = match source_error {
        Some(err) => format!("{log}\nsource: {err}"),
        None => log,
    };
    Err(ChdmanError::CommandFailed { exit_code, log })
}
