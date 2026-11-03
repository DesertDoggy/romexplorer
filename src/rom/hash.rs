use std::ffi::{CStr, CString, c_char, c_int};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread;

use bitflags::bitflags;
use crc32fast::Hasher as Crc32;
use libloading::{Library, Symbol};
use md5::{Digest as _, Md5};
use sha1::Sha1;
use sha2::Sha256;
use xxhash_rust::xxh3::Xxh3;

pub use crate::common::models::{ArchiveFormat, CueSheetStyle, MediaType, Platform, HashAlgorithms};

use super::native_lib;

static HASH_WORKER_THREAD_OBS: OnceLock<Mutex<Vec<(&'static str, String)>>> = OnceLock::new();
static HASH_WORKER_THREAD_OBS_ENABLED: AtomicBool = AtomicBool::new(false);

fn worker_obs() -> &'static Mutex<Vec<(&'static str, String)>> {
    HASH_WORKER_THREAD_OBS.get_or_init(|| Mutex::new(Vec::new()))
}

fn record_worker_thread(label: &'static str) {
    if !HASH_WORKER_THREAD_OBS_ENABLED.load(Ordering::Relaxed) {
        return;
    }

    let tid = format!("{:?}", thread::current().id());
    let mut obs = worker_obs().lock().expect("worker observation mutex poisoned");
    obs.push((label, tid));
}

pub fn set_worker_thread_observation_enabled(enabled: bool) {
    HASH_WORKER_THREAD_OBS_ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn clear_worker_thread_observations() {
    let mut obs = worker_obs().lock().expect("worker observation mutex poisoned");
    obs.clear();
}

pub fn worker_thread_observations() -> Vec<(String, String)> {
    let obs = worker_obs().lock().expect("worker observation mutex poisoned");
    obs.iter().map(|(label, tid)| ((*label).to_string(), tid.clone())).collect()
}


bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct HashFlags: u32 {
        const CRC32 = 1 << 0;
        const MD5 = 1 << 1;
        const SHA1 = 1 << 2;
        const SHA256 = 1 << 3;
        const XXH3_128 = 1 << 4;
        const RAHASH = 1 << 5;
    }
}

impl Default for HashFlags {
    fn default() -> Self {
        Self::CRC32 | Self::MD5 | Self::SHA1
    }
}

impl HashFlags {
    pub fn algorithm_to_flag(algorithm: HashAlgorithms) -> Self {
        match algorithm {
            HashAlgorithms::Crc32 => Self::CRC32,
            HashAlgorithms::Md5 => Self::MD5,
            HashAlgorithms::Sha1 => Self::SHA1,
            HashAlgorithms::Sha256 => Self::SHA256,
            HashAlgorithms::Xxhash3_128 => Self::XXH3_128,
            HashAlgorithms::Rahash => Self::RAHASH,
        }
    }

    pub fn algorithms_to_flags(algorithms: Vec<HashAlgorithms>) -> Self {
        let mut flags = Self::empty();
        for algorithm in algorithms {
            flags |= Self::algorithm_to_flag(algorithm);
        }
        flags
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileHashRecord {
    pub path: String,
    pub size_bytes: Option<u64>,
    pub crc32: Option<String>,
    pub xxh3_128: Option<String>,
    pub md5: Option<String>,
    pub sha1: Option<String>,
    pub sha256: Option<String>,
    pub rahash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HashReport {
    pub crc32: Option<String>,
    pub xxh3_128: Option<String>,
    pub md5: Option<String>,
    pub sha1: Option<String>,
    pub sha256: Option<String>,
    pub rahash: Option<String>,
    pub content_hashes: Vec<FileHashRecord>,
}

impl HashReport {
    pub fn get(&self, algo: HashAlgorithms) -> Option<&str> {
        match algo {
            HashAlgorithms::Crc32 => self.crc32.as_deref(),
            HashAlgorithms::Md5 => self.md5.as_deref(),
            HashAlgorithms::Sha1 => self.sha1.as_deref(),
            HashAlgorithms::Sha256 => self.sha256.as_deref(),
            HashAlgorithms::Xxhash3_128 => self.xxh3_128.as_deref(),
            HashAlgorithms::Rahash => self.rahash.as_deref(),
        }
    }
    pub fn has_no_fallback(&self) -> bool {
        self.crc32.is_none() &&
        self.xxh3_128.is_none() &&
        self.md5.is_none() &&
        self.sha1.is_none() &&
        self.sha256.is_none()
    }
    /// The first stored whole-file hash in the cheapest-first order for this CPU (see
    /// [`fallback_order`]), together with the algorithm it belongs to, so the caller can
    /// re-hash with that same algorithm. Errors only when all five are `None`.
    pub fn get_fallback(&self) -> Result<(HashAlgorithms, &str), std::io::Error> {
        fallback_order()
            .iter()
            .find_map(|&algo| self.get(algo).map(|hash| (algo, hash)))
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "No fallback hash available"))
    }
}

/// Fallback order when the CPU has hardware SHA (x86 SHA-NI, ARM SHA-1/SHA-2 extensions):
/// SHA-1 and SHA-256 then beat MD5, which has no hardware path.
const FALLBACK_ORDER_HARDWARE_SHA: [HashAlgorithms; 5] =
    [HashAlgorithms::Crc32, HashAlgorithms::Xxhash3_128, HashAlgorithms::Sha1, HashAlgorithms::Sha256, HashAlgorithms::Md5];

/// Fallback order without hardware SHA: MD5 is faster than software SHA-1 and SHA-256.
const FALLBACK_ORDER_SOFTWARE_SHA: [HashAlgorithms; 5] =
    [HashAlgorithms::Crc32, HashAlgorithms::Xxhash3_128, HashAlgorithms::Md5, HashAlgorithms::Sha1, HashAlgorithms::Sha256];

/// Whether this CPU has hardware SHA instructions: x86 SHA-NI, or the ARMv8 SHA-1/SHA-2
/// extensions (Rust reports both under `sha2`). Detected once, on first use.
fn has_hardware_sha() -> bool {
    static HAS_HARDWARE_SHA: OnceLock<bool> = OnceLock::new();
    *HAS_HARDWARE_SHA.get_or_init(|| {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            std::is_x86_feature_detected!("sha")
        }
        #[cfg(target_arch = "aarch64")]
        {
            std::arch::is_aarch64_feature_detected!("sha2")
        }
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
        {
            false
        }
    })
}

/// Stored hashes in the order [`HashReport::get_fallback`] tries them on this CPU.
pub fn fallback_order() -> &'static [HashAlgorithms; 5] {
    if has_hardware_sha() { &FALLBACK_ORDER_HARDWARE_SHA } else { &FALLBACK_ORDER_SOFTWARE_SHA }
}



/// Result of [`rahash_file`]: either the completed hash, or a signal that the format's
/// hash algorithm needed data beyond what's currently on disk at the given path (only
/// possible when `expected_total_bytes` was passed as nonzero — call again later, once
/// more has been written to the file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RahashOutcome {
    Done(String),
    NeedMoreData,
}

/// Error from [`rahash_file`]. [`RahashError::Unavailable`] means the environment isn't
/// set up (library missing, wrong/outdated build without the expected exports, etc.) —
/// not a hashing failure, and typically worth treating as "skip RAHASH" rather than a
/// hard error. [`RahashError::Failed`] is a genuine hashing failure (corrupt/unsupported
/// data, an invalid `console_id`, etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RahashError {
    Unavailable(String),
    Failed(String),
}

impl fmt::Display for RahashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(message) => write!(f, "RAHasher unavailable: {message}"),
            Self::Failed(message) => write!(f, "RAHasher failed: {message}"),
        }
    }
}

impl std::error::Error for RahashError {}

/// Computes the RetroAchievements hash of `path` for `console_id`.
///
/// `path` does not need to be fully written yet: pass `expected_total_bytes` as the
/// file's eventual final size (0 if unknown), and call again later — after more has
/// been written to `path` — whenever this returns `Ok(RahashOutcome::NeedMoreData)`.
///
/// Stateless and safe to call repeatedly: opens, reads, and closes `path` fresh every
/// call. The caller owns `path`'s entire lifecycle (creation, writing, deletion); this
/// never creates, writes to, or deletes anything. Because `path` is a real path in its
/// real directory (not a copy in a throwaway temp file), sibling files referenced by
/// relative path (a `.cue`'s `.bin` tracks, `.m3u` playlists, `.iso`/`.chd` disc
/// images) resolve correctly.
pub fn rahash_file(path: &std::path::Path, console_id: u32, expected_total_bytes: u64) -> Result<RahashOutcome, RahashError> {
    let lib_path = find_rahash_library().ok_or_else(|| RahashError::Unavailable("RAHasher library not found".to_string()))?;
    let lib = unsafe { Library::new(&lib_path) }.map_err(|err| RahashError::Unavailable(err.to_string()))?;

    let path_str = path
        .to_str()
        .ok_or_else(|| RahashError::Failed(format!("path is not valid UTF-8: {}", path.display())))?;
    let c_path = CString::new(path_str).map_err(|err| RahashError::Failed(err.to_string()))?;
    let system_dir = CString::new(".").expect("\".\" has no interior NUL");

    // c_char, not i8: it is unsigned on aarch64 Linux, where `[0i8; _]` does not type-check.
    let mut out_hash: [c_char; 33] = [0; 33];
    let mut out_done: c_int = 0;

    unsafe {
        let hash_file: Symbol<RahasherHashFileFn> =
            lib.get(b"rahasher_hash_file").map_err(|err| RahashError::Unavailable(err.to_string()))?;
        let get_last_error: Symbol<RahasherGetLastErrorFn> =
            lib.get(b"rahasher_get_last_error").map_err(|err| RahashError::Unavailable(err.to_string()))?;

        let rc = hash_file(
            console_id,
            c_path.as_ptr(),
            system_dir.as_ptr(),
            expected_total_bytes,
            out_hash.as_mut_ptr() as *mut c_char,
            &mut out_done,
        );

        if rc != 0 {
            let message = CStr::from_ptr(get_last_error()).to_string_lossy().into_owned();
            return Err(RahashError::Failed(message));
        }
    }

    if out_done == 0 {
        return Ok(RahashOutcome::NeedMoreData);
    }

    let hash = unsafe { CStr::from_ptr(out_hash.as_ptr()) }
        .to_str()
        .map_err(|err| RahashError::Failed(err.to_string()))?
        .to_string();

    Ok(RahashOutcome::Done(hash))
}

/// Files served from memory to [`rahash_vfs`]: a disc container decoded sector by sector
/// as RAHasher asks, or an archive member already read. Names are base names, as a sheet
/// names its tracks.
pub trait RaFiles {
    /// A handle (> 0) and the size of file `name`, or `None` when there is none.
    fn open(&mut self, name: &str) -> Option<(i64, u64)>;
    /// Fills `buf` from `offset` of file `handle` (callers clamp to the file's size).
    fn read(&mut self, handle: i64, offset: u64, buf: &mut [u8]) -> std::io::Result<()>;
}

#[repr(C)]
struct RahasherVfs {
    userdata: *mut std::ffi::c_void,
    open: unsafe extern "C" fn(*mut std::ffi::c_void, *const c_char, *mut u64) -> i64,
    read: unsafe extern "C" fn(*mut std::ffi::c_void, i64, u64, *mut std::ffi::c_void, u64) -> i64,
    close: Option<unsafe extern "C" fn(*mut std::ffi::c_void, i64)>,
}

unsafe extern "C" fn vfs_open(userdata: *mut std::ffi::c_void, name: *const c_char, out_size: *mut u64) -> i64 {
    let files = &mut *(userdata as *mut &mut dyn RaFiles);
    let name = CStr::from_ptr(name).to_string_lossy();
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| files.open(&name))) {
        Ok(Some((handle, size))) => {
            *out_size = size;
            handle
        }
        _ => 0,
    }
}

unsafe extern "C" fn vfs_read(userdata: *mut std::ffi::c_void, handle: i64, offset: u64, buf: *mut std::ffi::c_void, len: u64) -> i64 {
    let files = &mut *(userdata as *mut &mut dyn RaFiles);
    let buf = std::slice::from_raw_parts_mut(buf as *mut u8, len as usize);
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| files.read(handle, offset, buf))) {
        Ok(Ok(())) => len as i64,
        _ => -1,
    }
}

/// The RetroAchievements hash of `name` (a cue, gdi, iso or cartridge) whose bytes, and those
/// of every file it names, come from `files`: nothing is read from or written to disk.
pub fn rahash_vfs(name: &str, console_id: u32, files: &mut dyn RaFiles) -> Result<String, RahashError> {
    let lib_path = find_rahash_library().ok_or_else(|| RahashError::Unavailable("RAHasher library not found".to_string()))?;
    let lib = unsafe { Library::new(&lib_path) }.map_err(|err| RahashError::Unavailable(err.to_string()))?;
    let c_name = CString::new(name).map_err(|err| RahashError::Failed(err.to_string()))?;
    let mut out_hash: [c_char; 33] = [0; 33];
    let mut files: &mut dyn RaFiles = files;
    let vfs = RahasherVfs { userdata: &mut files as *mut &mut dyn RaFiles as *mut std::ffi::c_void, open: vfs_open, read: vfs_read, close: None };
    unsafe {
        // A library built before this entry point reads containers it cannot open itself not at all.
        let hash_vfs: Symbol<RahasherHashVfsFn> = lib
            .get(b"rahasher_hash_vfs")
            .map_err(|err| RahashError::Unavailable(format!("RAHasher is too old to hash a CHD, RVZ or archive in memory ({err}); rebuild it with deps/RALibretro/build_rahasher.sh")))?;
        let get_last_error: Symbol<RahasherGetLastErrorFn> = lib.get(b"rahasher_get_last_error").map_err(|err| RahashError::Unavailable(err.to_string()))?;
        if hash_vfs(console_id, c_name.as_ptr(), &vfs, out_hash.as_mut_ptr()) != 0 {
            return Err(RahashError::Failed(CStr::from_ptr(get_last_error()).to_string_lossy().into_owned()));
        }
    }
    Ok(unsafe { CStr::from_ptr(out_hash.as_ptr()) }.to_str().map_err(|err| RahashError::Failed(err.to_string()))?.to_string())
}

/// RetroAchievements' console id for `platform` (rcheevos `rc_consoles.h`), or `None` for a
/// platform RetroAchievements does not cover.
pub fn ra_console_id(platform: crate::common::models::Platform) -> Option<u32> {
    use crate::common::models::Platform as P;
    Some(match platform {
        P::SegaGenesis => 1,
        P::Nintendo64 => 2,
        P::Snes | P::SuperFamicom => 3,
        P::GameBoy => 4,
        P::GameBoyAdvance => 5,
        P::GameBoyColor => 6,
        P::Nes | P::Famicom => 7,
        P::TurboGrafx16 | P::SuperGrafx => 8,
        P::SegaCd => 9,
        P::Sega32x => 10,
        P::SegaMasterSystem => 11,
        P::PlayStation => 12,
        P::AtariLynx => 13,
        P::NeoGeoPocket | P::NeoGeoPocketColor => 14,
        P::SegaGameGear => 15,
        P::GameCube => 16,
        P::AtariJaguar => 17,
        P::NintendoDs => 18,
        P::Wii => 19,
        P::WiiU => 20,
        P::PlayStation2 => 21,
        P::Xbox => 22,
        P::Odyssey2 => 23,
        P::PokemonMini => 24,
        P::Atari2600 => 25,
        P::PcDos => 26,
        P::Arcade => 27,
        P::VirtualBoy => 28,
        P::Msx | P::Msx2 | P::Msx2Plus | P::MsxTurbo => 29,
        P::Commodore64 => 30,
        P::Zx81 => 31,
        P::Oric => 32,
        P::Sg1000 => 33,
        P::Vic20 => 34,
        P::Amiga => 35,
        P::AtariStSte => 36,
        P::AmstradCpc => 37,
        P::AppleII => 38,
        P::SegaSaturn => 39,
        P::Dreamcast => 40,
        P::PlayStationPortable => 41,
        P::PhilipsCdi => 42,
        P::ThreeDo => 43,
        P::ColecoVision => 44,
        P::Intellivision => 45,
        P::Vectrex => 46,
        P::Pc8801 => 47,
        P::Pc98 => 48,
        P::PcFx => 49,
        P::Atari5200 => 50,
        P::Atari7800 => 51,
        P::SharpX68000 => 52,
        P::WonderSwan | P::WonderSwanColor => 53,
        P::NeoGeoCd => 56,
        P::FmTowns => 58,
        P::ZxSpectrum => 59,
        P::Nintendo3ds | P::NewNintendo3ds => 62,
        P::Pc6000Series => 67,
        P::SegaPico => 68,
        P::TurboGrafxCd => 76,
        P::NintendoDsi => 78,
        P::FamicomDiskSystem => 81,
        _ => return None,
    })
}

/// Whether RetroAchievements hashes a single file of this console as the MD5 of the whole
/// file (rcheevos `rc_hash_generate_from_file`'s `rc_hash_whole_file` cases). For these the
/// scan's own MD5 is already the RA hash, and nothing more needs reading.
pub fn ra_hash_is_file_md5(console_id: u32) -> bool {
    matches!(console_id, 1 | 4 | 5 | 6 | 10 | 11 | 14 | 15 | 17 | 23 | 24 | 25 | 28 | 29 | 30 | 32 | 33 | 37 | 38 | 44 | 45 | 46 | 47 | 53 | 57 | 59 | 63 | 65 | 69 | 72 | 73 | 74 | 75 | 79 | 80)
}

type RahasherHashFileFn =
    unsafe extern "C" fn(u32, *const c_char, *const c_char, u64, *mut c_char, *mut c_int) -> c_int;
type RahasherHashVfsFn = unsafe extern "C" fn(u32, *const c_char, *const RahasherVfs, *mut c_char) -> c_int;
type RahasherGetLastErrorFn = unsafe extern "C" fn() -> *const c_char;

/// Tries `app_root_ex/lib/<os>/` first (`scripts/move_libs_to_app_root.sh`'s flat layout),
/// then the newest `deps/RALibretro/release/<platform>/<arch>/<version>/dynamic/`
/// build (`build_rahasher.sh` names versions by build date), then the older unversioned
/// `<platform>/<arch>/dynamic/` layout so a build from before versioning still loads.
///
/// The file keeps its `lib` prefix on every platform (`libRAHasher.dll` on Windows too), so
/// the base name passed to `native_lib`'s generic helpers -- which add `lib` only off
/// Windows -- differs per OS.
fn find_rahash_library() -> Option<PathBuf> {
    let base_name = if cfg!(target_os = "windows") { "libRAHasher" } else { "RAHasher" };
    if let Some(path) = native_lib::find_app_root_library(base_name) {
        return Some(path);
    }
    let release_root = native_lib::source_root().join("deps/RALibretro/release");
    if let Some(path) = native_lib::find_native_library(&release_root, base_name) {
        return Some(path);
    }
    let (platform, arch) = native_lib::platform_arch_dirs()?;
    let filename = native_lib::dynamic_lib_filename(base_name)?;
    let legacy = release_root.join(platform).join(arch).join("dynamic").join(filename);
    legacy.exists().then_some(legacy)
}

#[derive(Debug)]
pub enum HashDigestError {
    WorkerDisconnected(&'static str),
}

impl fmt::Display for HashDigestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkerDisconnected(name) => write!(f, "hash worker '{name}' is disconnected"),
        }
    }
}

impl std::error::Error for HashDigestError {}

#[derive(Debug)]
enum FastHashWorkers {
    Combined(ChunkWorker<(Option<u32>, Option<u128>)>),
    Separate {
        crc32: Option<ChunkWorker<u32>>,
        xxh3_128: Option<ChunkWorker<u128>>,
    },
}

#[derive(Debug)]
struct ChunkWorker<T> {
    label: &'static str,
    sender: mpsc::SyncSender<Arc<[u8]>>,
    handle: thread::JoinHandle<T>,
}

impl<T: Send + 'static> ChunkWorker<T> {
    fn spawn<F>(label: &'static str, worker: F) -> Self
    where
        F: FnOnce(mpsc::Receiver<Arc<[u8]>>) -> T + Send + 'static,
    {
        // Bounded: a reader faster than this algorithm waits for it rather than queueing the
        // file in memory. The RAM budget behind the auto limits counts on this bound.
        let (sender, receiver) = mpsc::sync_channel(ASSUMED_CHUNKS_IN_FLIGHT);
        let handle = thread::spawn(move || {
            record_worker_thread(label);
            worker(receiver)
        });
        Self { label, sender, handle }
    }

    fn send_arc(&self, chunk: Arc<[u8]>) -> Result<(), HashDigestError> {
        self.sender.send(chunk).map_err(|_| HashDigestError::WorkerDisconnected(self.label))
    }

    fn finish(self) -> T {
        let ChunkWorker { sender, handle, .. } = self;
        drop(sender);
        handle.join().expect("hash worker thread panicked")
    }
}

/// Feeds byte chunks to CRC32/MD5/SHA1/SHA256/XXH3_128 in parallel (one worker thread
/// per requested algorithm). Does not compute RAHASH — RAHASH is not a byte-stream
/// algorithm (it may need to open sibling files by real path, e.g. a `.cue`'s `.bin`
/// tracks), so it isn't representable as chunk feeding; see [`rahash_file`] and
/// [`hash`], which runs both this and RAHASH concurrently on separate threads.
/// A `HashFlags::RAHASH` bit passed here is ignored.
#[derive(Debug)]
pub struct HashDigest {
    fast: Option<FastHashWorkers>,
    crc32: Option<ChunkWorker<u32>>,
    xxh3_128: Option<ChunkWorker<u128>>,
    md5: Option<ChunkWorker<[u8; 16]>>,
    sha1: Option<ChunkWorker<[u8; 20]>>,
    sha256: Option<ChunkWorker<[u8; 32]>>,
}

impl HashDigest {
    pub fn new(flags: HashFlags) -> Self {
        let flags = if flags.is_empty() { HashFlags::default() } else { flags };
        let has_other_hashes = flags.intersects(HashFlags::MD5 | HashFlags::SHA1 | HashFlags::SHA256);
        let combine_fast_hashes = has_other_hashes && flags.contains(HashFlags::CRC32) && flags.contains(HashFlags::XXH3_128);

        Self {
            fast: if combine_fast_hashes {
                Some(FastHashWorkers::Combined(spawn_crc32_xxh3_worker()))
            } else if flags.contains(HashFlags::CRC32) || flags.contains(HashFlags::XXH3_128) {
                Some(FastHashWorkers::Separate {
                    crc32: flags.contains(HashFlags::CRC32).then(spawn_crc32_worker),
                    xxh3_128: flags.contains(HashFlags::XXH3_128).then(spawn_xxh3_worker),
                })
            } else {
                None
            },
            crc32: None,
            xxh3_128: None,
            md5: if flags.contains(HashFlags::MD5) {
                Some(spawn_md5_worker())
            } else {
                None
            },
            sha1: if flags.contains(HashFlags::SHA1) {
                Some(spawn_sha1_worker())
            } else {
                None
            },
            sha256: if flags.contains(HashFlags::SHA256) {
                Some(spawn_sha256_worker())
            } else {
                None
            },
        }
    }

    /// How many background worker threads `Self::new(flags)` spawns -- 1..=4: CRC32 and
    /// XXH3_128 combine into a single thread whenever a heavier hash (MD5/SHA1/SHA256) is
    /// also requested (mirrors `Self::new`'s own `combine_fast_hashes` rule exactly, kept
    /// side by side with it so the two can't drift), otherwise each requested one of
    /// CRC32/XXH3_128/MD5/SHA1/SHA256 gets its own. RAHASH is not covered (see
    /// `hash_file_other`'s doc comment: `HashDigest` never computes it) -- [`hash`] runs
    /// RAHASH on a further, separate thread of its own when requested.
    ///
    /// For `rom::decode_threads`, which subtracts this from a decode-thread recommendation
    /// so decode workers and hash workers aren't both sized as if they owned the whole
    /// machine.
    pub fn worker_thread_count(flags: HashFlags) -> usize {
        let flags = if flags.is_empty() { HashFlags::default() } else { flags };
        let has_other_hashes = flags.intersects(HashFlags::MD5 | HashFlags::SHA1 | HashFlags::SHA256);
        let combine_fast_hashes = has_other_hashes && flags.contains(HashFlags::CRC32) && flags.contains(HashFlags::XXH3_128);

        let fast_threads =
            if combine_fast_hashes { 1 } else { usize::from(flags.contains(HashFlags::CRC32)) + usize::from(flags.contains(HashFlags::XXH3_128)) };
        fast_threads
            + usize::from(flags.contains(HashFlags::MD5))
            + usize::from(flags.contains(HashFlags::SHA1))
            + usize::from(flags.contains(HashFlags::SHA256))
    }

    pub fn update(&self, data_chunk: &[u8]) -> Result<(), HashDigestError> {
        self.update_arc(Arc::<[u8]>::from(data_chunk))
    }

    /// Same as [`Self::update`], but takes a chunk that's already behind an `Arc` so the
    /// caller can hand the *same* allocation to other consumers (see
    /// `super::pipeline::read_and_fan_out`, which feeds one read loop's chunks to this
    /// digest and to the convert/archive route at once). `update` copies into a fresh
    /// `Arc`; this doesn't.
    pub fn update_arc(&self, chunk: Arc<[u8]>) -> Result<(), HashDigestError> {
        if let Some(fast) = &self.fast {
            match fast {
                FastHashWorkers::Combined(worker) => {
                    worker.send_arc(Arc::clone(&chunk))?;
                }
                FastHashWorkers::Separate { crc32, xxh3_128 } => {
                    if let Some(worker) = crc32 {
                        worker.send_arc(Arc::clone(&chunk))?;
                    }
                    if let Some(worker) = xxh3_128 {
                        worker.send_arc(Arc::clone(&chunk))?;
                    }
                }
            }
        }

        if let Some(worker) = &self.md5 {
            worker.send_arc(Arc::clone(&chunk))?;
        }
        if let Some(worker) = &self.sha1 {
            worker.send_arc(Arc::clone(&chunk))?;
        }
        if let Some(worker) = &self.sha256 {
            worker.send_arc(chunk)?;
        }

        Ok(())
    }

    pub fn finish(self) -> HashReport {
        let mut report = HashReport::default();

        let debug_hash = std::env::var_os("ROMEXPLORER_HASH_DEBUG").is_some();

        if let Some(fast) = self.fast {
            match fast {
                FastHashWorkers::Combined(worker) => {
                    if debug_hash {
                        eprintln!("[hash-debug] waiting fast combined worker");
                    }
                    let (crc32, xxh3_128) = worker.finish();
                    if debug_hash {
                        eprintln!("[hash-debug] fast combined worker done");
                    }
                    report.crc32 = crc32.map(|value| format!("{value:08x}"));
                    report.xxh3_128 = xxh3_128.map(|value| format!("{value:032x}"));
                }
                FastHashWorkers::Separate { crc32, xxh3_128 } => {
                    if let Some(worker) = crc32 {
                        if debug_hash {
                            eprintln!("[hash-debug] waiting crc32 worker");
                        }
                        report.crc32 = Some(format!("{:08x}", worker.finish()));
                        if debug_hash {
                            eprintln!("[hash-debug] crc32 worker done");
                        }
                    }
                    if let Some(worker) = xxh3_128 {
                        if debug_hash {
                            eprintln!("[hash-debug] waiting xxh3_128 worker");
                        }
                        report.xxh3_128 = Some(format!("{:032x}", worker.finish()));
                        if debug_hash {
                            eprintln!("[hash-debug] xxh3_128 worker done");
                        }
                    }
                }
            }
        }

        if let Some(worker) = self.md5 {
            if debug_hash {
                eprintln!("[hash-debug] waiting md5 worker");
            }
            report.md5 = Some(hex_string(&worker.finish()));
            if debug_hash {
                eprintln!("[hash-debug] md5 worker done");
            }
        }
        if let Some(worker) = self.sha1 {
            if debug_hash {
                eprintln!("[hash-debug] waiting sha1 worker");
            }
            report.sha1 = Some(hex_string(&worker.finish()));
            if debug_hash {
                eprintln!("[hash-debug] sha1 worker done");
            }
        }
        if let Some(worker) = self.sha256 {
            if debug_hash {
                eprintln!("[hash-debug] waiting sha256 worker");
            }
            report.sha256 = Some(hex_string(&worker.finish()));
            if debug_hash {
                eprintln!("[hash-debug] sha256 worker done");
            }
        }
        report
    }
}

fn spawn_crc32_xxh3_worker() -> ChunkWorker<(Option<u32>, Option<u128>)> {
    ChunkWorker::spawn("crc32_xxh3", move |receiver| {
        let mut crc32 = Some(Crc32::new());
        let mut xxh3 = Some(Xxh3::new());

        while let Ok(chunk) = receiver.recv() {
            if let Some(hasher) = crc32.as_mut() {
                hasher.update(&chunk);
            }
            if let Some(hasher) = xxh3.as_mut() {
                hasher.update(&chunk);
            }
        }

        let crc32 = crc32.map(|hasher| hasher.finalize());
        let xxh3_128 = xxh3.map(|hasher| hasher.digest128());
        (crc32, xxh3_128)
    })
}

fn spawn_crc32_worker() -> ChunkWorker<u32> {
    ChunkWorker::spawn("crc32", move |receiver| {
        let mut hasher = Crc32::new();

        while let Ok(chunk) = receiver.recv() {
            hasher.update(&chunk);
        }

        hasher.finalize()
    })
}

fn spawn_xxh3_worker() -> ChunkWorker<u128> {
    ChunkWorker::spawn("xxh3_128", move |receiver| {
        let mut hasher = Xxh3::new();

        while let Ok(chunk) = receiver.recv() {
            hasher.update(&chunk);
        }

        hasher.digest128()
    })
}

fn spawn_md5_worker() -> ChunkWorker<[u8; 16]> {
    ChunkWorker::spawn("md5", move |receiver| {
        let mut hasher = Md5::new();

        while let Ok(chunk) = receiver.recv() {
            hasher.update(&chunk);
        }

        digest_to_array(hasher.finalize())
    })
}

fn spawn_sha1_worker() -> ChunkWorker<[u8; 20]> {
    ChunkWorker::spawn("sha1", move |receiver| {
        let mut hasher = Sha1::new();

        while let Ok(chunk) = receiver.recv() {
            hasher.update(&chunk);
        }

        digest_to_array(hasher.finalize())
    })
}

fn spawn_sha256_worker() -> ChunkWorker<[u8; 32]> {
    ChunkWorker::spawn("sha256", move |receiver| {
        let mut hasher = Sha256::new();

        while let Ok(chunk) = receiver.recv() {
            hasher.update(&chunk);
        }

        digest_to_array(hasher.finalize())
    })
}

fn digest_to_array<const N: usize>(digest: impl AsRef<[u8]>) -> [u8; N] {
    let bytes = digest.as_ref();
    let mut out = [0u8; N];
    out.copy_from_slice(bytes);
    out
}

fn hex_string(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);

    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }

    out
}

/// Default buffer size for [`hash_file_other`] (and [`hash`]) when no buffer size is given.
/// A few-MiB buffer amortizes syscall overhead for sequential reads to
/// near-zero and gives each hash worker thread large, SIMD-friendly chunks per channel
/// send.
pub const HASH_FILE_DEFAULT_BUF_SIZE: usize = 4 * 1024 * 1024;

/// Smallest and largest buffer [`recommended_buf_size`] will return.
pub const MIN_BUF_SIZE: usize = 64 * 1024;
pub const MAX_BUF_SIZE: usize = 8 * 1024 * 1024;

/// Whether a file is small enough that a big buffer buys nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileSizeClass {
    Small,
    Large,
}

impl FileSizeClass {
    /// Files up to this many bytes count as [`FileSizeClass::Small`].
    pub const SMALL_MAX_BYTES: u64 = 16 * 1024 * 1024;

    pub fn of(len: u64) -> Self {
        if len <= Self::SMALL_MAX_BYTES { Self::Small } else { Self::Large }
    }
}

/// What the file is read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageKind {
    Hdd,
    Ssd,
    Network,
}

/// Network filesystem names `Disk::file_system()` might report -- checked before
/// `Disk::kind()`, since a network share's `DiskKind` (from the OS's point of view) says
/// nothing about the actual network latency that matters for parallelism decisions here.
const NETWORK_FILE_SYSTEMS: &[&str] = &["nfs", "nfs4", "cifs", "smb", "smbfs", "afpfs"];

impl StorageKind {
    /// Which disk `path` lives on, classified for parallelism sizing
    /// (`operation::recommended_parallel_files`).
    /// Not exact science: `sysinfo`'s per-OS disk enumeration is a best-effort survey, not
    /// a guaranteed-authoritative source, and this is only ever used to *suggest* a
    /// default (`api::parallel`'s auto limits) -- a caller who knows better can always
    /// override the resulting job count directly.
    ///
    /// Falls back to [`Self::Hdd`] -- the conservative choice -- when `path` doesn't
    /// resolve to any known disk (doesn't exist yet, a sandboxed/containerized
    /// environment with no visible block devices, etc.) or reports an unrecognized
    /// [`sysinfo::DiskKind`]: under-parallelizing on an unknown medium is safer than
    /// over-parallelizing and thrashing a real HDD. Where `sysinfo` has no answer -- a ZFS
    /// dataset above all, whose device isn't a block device -- [`super::disk_kind`] reads
    /// the disks underneath before giving up. The `drive` field of the parallel settings
    /// (`api::parallel`) overrides all of this.
    pub fn for_path(path: &Path) -> Self {
        let target = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let disks = sysinfo::Disks::new_with_refreshed_list();
        let best = disks
            .list()
            .iter()
            .filter(|disk| target.starts_with(disk.mount_point()))
            .max_by_key(|disk| disk.mount_point().as_os_str().len());

        let Some(disk) = best else { return Self::Hdd };
        let fs_name = disk.file_system().to_string_lossy().to_ascii_lowercase();
        if NETWORK_FILE_SYSTEMS.iter().any(|known| fs_name == *known) {
            return Self::Network;
        }
        match disk.kind() {
            sysinfo::DiskKind::SSD => Self::Ssd,
            sysinfo::DiskKind::HDD => Self::Hdd,
            // `sysinfo` reads only `/sys/block/<device>`, which a ZFS dataset doesn't have.
            sysinfo::DiskKind::Unknown(_) => {
                let device = disk.name().to_string_lossy();
                let solid = if fs_name == "zfs" {
                    super::disk_kind::zfs_pool_solid_state(device.split('/').next().unwrap_or_default())
                } else if device.starts_with("/dev/") {
                    super::disk_kind::block_device_solid_state(Path::new(device.as_ref()))
                } else {
                    None
                };
                if solid == Some(true) { Self::Ssd } else { Self::Hdd }
            }
        }
    }

    /// The name settings and the API use: `hdd`, `ssd`, `network`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hdd => "hdd",
            Self::Ssd => "ssd",
            Self::Network => "network",
        }
    }
}

/// How much memory the hashing may use in total, across every file hashed at once
/// (about 64 MiB, 256 MiB and 1 GiB for Small, Medium and Large).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RamClass {
    Small,
    Medium,
    Large,
}

impl RamClass {
    fn budget_bytes(self) -> usize {
        match self {
            Self::Small => 64 * 1024 * 1024,
            Self::Medium => 256 * 1024 * 1024,
            Self::Large => 1024 * 1024 * 1024,
        }
    }

    /// This machine's total RAM, classified for parallelism sizing (see
    /// [`StorageKind::for_path`]'s doc comment -- same "suggestion, not authority"
    /// caveat applies). Thresholds are a fresh judgment call, not sourced from
    /// profiling data: **`< 8 GiB` -> `Small`, `8..32 GiB` -> `Medium`, `>= 32 GiB` ->
    /// `Large`** -- reasonable, easy to retune later if real-world numbers say
    /// otherwise.
    ///
    /// Falls back to [`Self::Medium`] -- a middle ground, not an extreme -- if
    /// `sysinfo` can't read total memory on this platform or reports `0`.
    pub fn detect() -> Self {
        const SMALL_MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;
        const MEDIUM_MAX_BYTES: u64 = 32 * 1024 * 1024 * 1024;

        let mut system = sysinfo::System::new();
        system.refresh_memory_specifics(sysinfo::MemoryRefreshKind::nothing().with_ram());
        match system.total_memory() {
            0 => Self::Medium,
            bytes if bytes < SMALL_MAX_BYTES => Self::Small,
            bytes if bytes < MEDIUM_MAX_BYTES => Self::Medium,
            _ => Self::Large,
        }
    }
}

#[cfg(test)]
mod resource_detection_tests {
    use super::*;

    #[test]
    fn ram_class_detect_is_deterministic_and_never_panics() {
        assert_eq!(RamClass::detect(), RamClass::detect(), "same machine, same call, must agree");
    }

    #[test]
    fn storage_kind_for_path_is_deterministic_for_a_real_path() {
        let dir = std::env::temp_dir();
        assert_eq!(StorageKind::for_path(&dir), StorageKind::for_path(&dir));
    }

    #[test]
    fn storage_kind_for_path_never_panics_for_a_nonexistent_path() {
        // A nonexistent path still resolves under some real mount point (root, on a
        // normal Linux/macOS/Windows setup -- every absolute path starts with "/" or a
        // drive letter), so this can't force the "no disk matched" fallback branch
        // portably; it only proves `for_path` handles a path that doesn't exist on disk
        // without panicking, returning one of the three real variants.
        let path = Path::new("/nonexistent-romexplorer-test-path/deeply/nested/file.bin");
        let kind = StorageKind::for_path(path);
        assert!(matches!(kind, StorageKind::Hdd | StorageKind::Ssd | StorageKind::Network));
    }
}

/// Chunks in flight per hash worker when spreading the RAM budget: each worker's queue holds
/// at most this many (`ChunkWorker::spawn`).
const ASSUMED_CHUNKS_IN_FLIGHT: usize = 8;

/// A starting-point read buffer size for [`hash_file_other`] / [`HashOptions::buf_size`],
/// always a power of two in `MIN_BUF_SIZE..=MAX_BUF_SIZE`.
///
/// The base size comes from the storage and file size (larger reads for slow-to-start
/// storage and big files; a spinning disk shared by several streams gets double, to
/// amortize seeks). It is then capped so that `parallel_files` streams with a few chunks
/// each in flight stay inside the RAM budget. `parallel_files` is how many files are hashed
/// at the same time (`0` counts as `1`).
pub fn recommended_buf_size(size: FileSizeClass, parallel_files: usize, storage: StorageKind, ram: RamClass) -> usize {
    let parallel = parallel_files.max(1);

    let mut base = match (storage, size) {
        (StorageKind::Hdd, FileSizeClass::Small) => 256 * 1024,
        (StorageKind::Hdd, FileSizeClass::Large) => 4 * 1024 * 1024,
        (StorageKind::Ssd, FileSizeClass::Small) => 128 * 1024,
        (StorageKind::Ssd, FileSizeClass::Large) => 4 * 1024 * 1024,
        (StorageKind::Network, FileSizeClass::Small) => 1024 * 1024,
        (StorageKind::Network, FileSizeClass::Large) => 8 * 1024 * 1024,
    };
    if storage == StorageKind::Hdd && parallel >= 2 {
        base *= 2;
    }

    let cap = ram.budget_bytes() / (parallel * ASSUMED_CHUNKS_IN_FLIGHT);
    let size = base.min(cap).clamp(MIN_BUF_SIZE, MAX_BUF_SIZE);
    // Round down to a power of two (cap is not necessarily one); stays >= MIN_BUF_SIZE.
    1usize << (usize::BITS - 1 - size.leading_zeros())
}

/// Reads `path` in `buf_size`-byte chunks (or [`HASH_FILE_DEFAULT_BUF_SIZE`] if
/// `buf_size` is `None`, or `Some(0)`, which could not read anything) and feeds them to a [`HashDigest`] covering every algorithm in
/// `flags` except RAHASH (RAHASH is computed separately — see [`rahash_file`] and
/// [`hash`], which runs both concurrently). Any `HashFlags::RAHASH` bit in `flags`
/// is ignored here.
///
/// Calls `on_progress(bytes_read, total_bytes)` after every read.
/// Hashes a buffer already in memory with `flags`, in one pass.
///
/// For the small, generated files this crate produces and then has to recognise -- a rebuilt
/// cue sheet, a generated playlist -- where the bytes never touch the disk before they are
/// compared. `hash` reads a path and is the wrong shape for that; this is the same digest
/// with the file left out.
pub fn hash_bytes(flags: HashFlags, data: &[u8]) -> HashReport {
    let digest = HashDigest::new(flags);
    // The only error `update` returns is a worker that has already gone, which cannot happen
    // between `new` and `finish` here.
    let _ = digest.update(data);
    digest.finish()
}

pub fn hash_file_other<F>(
    path: &std::path::Path,
    flags: HashFlags,
    buf_size: Option<usize>,
    mut on_progress: F,
) -> std::io::Result<HashReport>
where
    F: FnMut(u64, u64),
{
    use std::io::Read;

    let flags = flags.difference(HashFlags::RAHASH);
    let buf_size = buf_size.filter(|&size| size > 0).unwrap_or(HASH_FILE_DEFAULT_BUF_SIZE);
    let mut file = std::fs::File::open(path)?;
    let total = file.metadata()?.len();
    let digest = HashDigest::new(flags);
    let mut buf = vec![0u8; buf_size];
    let mut processed = 0u64;

    if total == 0 {
        on_progress(0, 0);
    } else {
        loop {
            let read = file.read(&mut buf)?;
            if read == 0 {
                break;
            }

            digest.update(&buf[..read]).map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err))?;
            processed += read as u64;
            on_progress(processed, total);
        }
    }

    Ok(digest.finish())
}

/// Settings for [`hash`]. `HashOptions::default()` hashes CRC32, MD5 and SHA-1 with the
/// default buffer size and no RAHASH.
#[derive(Debug, Clone, Copy)]
pub struct HashOptions {
    /// Algorithms to compute. An empty set falls back to [`HashFlags::default`].
    pub flags: HashFlags,
    /// Read buffer size for the byte-stream hashes. `None` uses
    /// [`HASH_FILE_DEFAULT_BUF_SIZE`].
    pub buf_size: Option<usize>,
    /// RetroAchievements console id. Only used by `HashFlags::RAHASH`, where it is required
    /// (and `0`, RC_CONSOLE_UNKNOWN, is rejected by the native library).
    pub console_id: Option<u32>,
}

impl Default for HashOptions {
    fn default() -> Self {
        Self { flags: HashFlags::default(), buf_size: None, console_id: None }
    }
}

/// Computes every hash in `flags` (CRC32/MD5/SHA1/SHA256/XXH3_128 and, if
/// `HashFlags::RAHASH` is set, RAHASH) for the file at `path`, running the byte-stream
/// hashes ([`hash_file_other`]) and RAHASH ([`rahash_file`], polled until done) on two
/// separate threads concurrently.
///
/// `on_progress` is called from the calling thread with the blended overall progress
/// as a percentage in `0.0..=100.0`. The blend weights each side by how many
/// algorithms it represents — e.g. requesting SHA1, SHA256, and RAHASH weighs the
/// byte-stream side (2 algorithms) twice as heavily as RAHASH (1 algorithm) when
/// combining their two progress fractions into one overall percentage.
///
/// See [`HashOptions`] for the optional settings. Requesting `HashFlags::RAHASH` without a
/// `console_id` is an `InvalidInput` error, returned before any work starts.
pub fn hash<F>(path: &std::path::Path, options: &HashOptions, mut on_progress: F) -> std::io::Result<HashReport>
where
    F: FnMut(f32),
{
    let flags = if options.flags.is_empty() { HashFlags::default() } else { options.flags };
    let buf_size = options.buf_size;
    if flags.contains(HashFlags::RAHASH) && options.console_id.is_none() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "RAHASH requires a console id"));
    }
    // Only read by the RAHASH thread, which only exists when RAHASH was requested (and so
    // a console id was checked to be present above).
    let console_id = options.console_id.unwrap_or(0);
    let other_flags = flags.difference(HashFlags::RAHASH);
    let other_weight = other_flags.iter().count() as f32;
    let want_rahash = flags.contains(HashFlags::RAHASH);
    let rahash_weight = if want_rahash { 1.0f32 } else { 0.0f32 };
    let total_weight = other_weight + rahash_weight;

    let other_progress_bits = Arc::new(AtomicU32::new(0f32.to_bits()));
    let rahash_progress_bits = Arc::new(AtomicU32::new(0f32.to_bits()));
    let other_done = Arc::new(AtomicBool::new(false));
    let rahash_done = Arc::new(AtomicBool::new(!want_rahash));

    let other_result: Arc<Mutex<Option<std::io::Result<HashReport>>>> = Arc::new(Mutex::new(None));
    let rahash_result: Arc<Mutex<Option<Result<RahashOutcome, RahashError>>>> = Arc::new(Mutex::new(None));

    let other_thread = {
        let path = path.to_path_buf();
        let progress_bits = Arc::clone(&other_progress_bits);
        let done_flag = Arc::clone(&other_done);
        let result_slot = Arc::clone(&other_result);
        thread::spawn(move || {
            record_worker_thread("hash_file_other");
            let result = hash_file_other(&path, other_flags, buf_size, |processed, total| {
                let fraction = if total == 0 { 1.0 } else { processed as f32 / total as f32 };
                progress_bits.store(fraction.to_bits(), Ordering::Relaxed);
            });
            *result_slot.lock().expect("other-hash result mutex poisoned") = Some(result);
            done_flag.store(true, Ordering::Release);
        })
    };

    let rahash_thread = want_rahash.then(|| {
        let path = path.to_path_buf();
        let progress_bits = Arc::clone(&rahash_progress_bits);
        let done_flag = Arc::clone(&rahash_done);
        let result_slot = Arc::clone(&rahash_result);
        thread::spawn(move || {
            record_worker_thread("rahash_file");
            let expected_total = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
            let (mut last_size, mut idle) = (expected_total, 0u32);

            loop {
                let current = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
                let fraction = if expected_total == 0 {
                    0.0
                } else {
                    (current as f32 / expected_total as f32).min(0.99)
                };
                progress_bits.store(fraction.to_bits(), Ordering::Relaxed);

                match rahash_file(&path, console_id, expected_total) {
                    Ok(RahashOutcome::Done(hash)) => {
                        progress_bits.store(1.0f32.to_bits(), Ordering::Relaxed);
                        *result_slot.lock().expect("rahash result mutex poisoned") = Some(Ok(RahashOutcome::Done(hash)));
                        break;
                    }
                    // Still wanting more from a file that has stopped growing for ten
                    // seconds: a truncated image (its header promises more than the disk
                    // holds). Waiting longer cannot help.
                    Ok(RahashOutcome::NeedMoreData) => {
                        if current == last_size {
                            idle += 1;
                        } else {
                            (last_size, idle) = (current, 0);
                        }
                        if idle >= 200 {
                            *result_slot.lock().expect("rahash result mutex poisoned") = Some(Err(RahashError::Failed(format!("{}: the image is shorter than its header says", path.display()))));
                            break;
                        }
                        thread::sleep(std::time::Duration::from_millis(50));
                    }
                    Err(message) => {
                        *result_slot.lock().expect("rahash result mutex poisoned") = Some(Err(message));
                        break;
                    }
                }
            }

            done_flag.store(true, Ordering::Release);
        })
    });

    loop {
        let other_fraction = f32::from_bits(other_progress_bits.load(Ordering::Relaxed));
        let rahash_fraction = f32::from_bits(rahash_progress_bits.load(Ordering::Relaxed));
        let blended = if total_weight > 0.0 {
            (other_fraction * other_weight + rahash_fraction * rahash_weight) / total_weight * 100.0
        } else {
            100.0
        };
        on_progress(blended);

        if other_done.load(Ordering::Acquire) && rahash_done.load(Ordering::Acquire) {
            break;
        }

        thread::sleep(std::time::Duration::from_millis(50));
    }

    other_thread.join().expect("other-hash thread panicked");
    if let Some(handle) = rahash_thread {
        handle.join().expect("rahash thread panicked");
    }

    let mut report = other_result
        .lock()
        .expect("other-hash result mutex poisoned")
        .take()
        .expect("other-hash thread did not report a result")?;

    if want_rahash {
        match rahash_result
            .lock()
            .expect("rahash result mutex poisoned")
            .take()
            .expect("rahash thread did not report a result")
        {
            Ok(RahashOutcome::Done(hash)) => report.rahash = Some(hash),
            Ok(RahashOutcome::NeedMoreData) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "rahash needed more data than the file ultimately contained",
                ));
            }
            Err(err) => {
                return Err(std::io::Error::new(std::io::ErrorKind::Other, err.to_string()));
            }
        }
    }

    on_progress(100.0);
    Ok(report)
}