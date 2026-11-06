//! Locates native libraries built under a submodule's own
//! `deps/<name>/release/{platform}/{arch}/{version}/dynamic/` layout (see chdman-simd's and
//! dolphin's `deps/<name>/scripts/build_*.sh`). Always resolves to the most recently built
//! version directory, so rebuilding chdman-simd/dolphin against a newer upstream commit
//! is picked up automatically -- no Rust-side path/version changes needed.

//!
//! Each submodule's build script produces a streaming-capable variant by default (its
//! `--nostreaming` flag builds the plain one instead), which lands in a parallel
//! `deps/<name>/release/with_streaming/{platform}/{arch}/{version}/` tree -- see [`LibVariant`].
//! Whether a given library build can actually stream is decided by symbol presence
//! ([`library_exports`]), not by which directory it came from, so a misplaced or stale
//! build is reported truthfully. [`detect_capabilities`] reports all three libraries at
//! once; it is plain runtime code, so a downstream app can call it from its own
//! `build.rs` (with this crate as a build-dependency) to pick streaming/non-streaming
//! build options.

use std::path::{Path, PathBuf};

use libloading::Library;

/// Overrides the "exe root" every app-root-relative lookup in this crate resolves
/// against (this function, `convert::nes::FcDb`, `detect::systemfile::BiosDb`), in place
/// of `std::env::current_exe()`'s parent directory. Unset in production -- a real
/// deployed app has no reason to set it. Exists so tests (and manual runs) can point
/// resolution at `app_root_ex/` (this repo's stand-in for the real app's exe root)
/// instead of the test binary's own location deep under `target/debug/deps/`, which
/// obviously isn't a real app root and can't hold fixtures/native libs.
///
/// One definition, reused everywhere -- this used to be defined identically in both
/// `nes.rs` and `systemfile.rs`; consolidated here since this module is the natural home
/// for "resolve something relative to the app root" logic.
pub const EXE_ROOT_OVERRIDE_ENV: &str = "ROMEXPLORER_EXE_ROOT";

/// `pub(crate)`, not private: `sevenzip_wrapper` needs it directly too, since 7z's
/// filename convention (no `lib` prefix, matching upstream 7-Zip's own naming) doesn't
/// fit [`dynamic_lib_filename`]'s generic `lib<name>.<ext>` shape that
/// [`find_app_root_library`] assumes for every other native library here.
pub(crate) fn exe_root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(EXE_ROOT_OVERRIDE_ENV) {
        return Some(PathBuf::from(dir));
    }
    // The app root `database::config::AppPaths` found, when it has a `lib/` to offer. These
    // two used to disagree: `AppPaths::detect` also walks *up* from the executable looking
    // for an `app_root_ex/` (the source tree, a macOS bundle's `Resources/`), while this
    // looked only beside the executable. Wherever the two differ the disagreement is
    // silent: MariaDB comes up (it asks `AppPaths`), while 7z, chdman, dolphinrvz and
    // RAHasher all quietly fall back to a `submodules/` tree that a user's machine has never
    // had, and the fast paths are simply gone. Asking `AppPaths` first is what keeps one
    // answer to "where is the app root".
    let root = crate::database::config::AppPaths::detect().root;
    if root.join("lib").is_dir() {
        return Some(root);
    }
    std::env::current_exe().ok()?.parent().map(Path::to_path_buf)
}

/// This build's `app_root_ex/lib/<os>/<arch>/` OS directory name -- `windows`/`macos`/`ios`/
/// `android`, else `linux`. **Not** the same string [`platform_arch_dirs`] uses for
/// macOS (`mac`) -- that one matches the submodule build scripts' own directory naming
/// convention, this one matches the deployed app's layout
/// (`database::config::AppPaths::os_dir_name`, which now just calls this). Both live in
/// this one file specifically so they can't silently drift apart again the way they had
/// before this was consolidated.
pub fn os_dir_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "ios") {
        "ios"
    } else if cfg!(target_os = "android") {
        "android"
    } else {
        "linux"
    }
}

/// This build's `app_root_ex/lib/<os>/<arch>/` arch directory name, using the same `x64` /
/// `arm64` spelling [`platform_arch_dirs`] uses for the submodule trees, so one build's
/// output lands under the name the app then looks for.
///
/// Unlike `platform_arch_dirs` this is total: an architecture neither branch covers falls
/// back to Rust's own name for it rather than giving up, so the path is always well-formed
/// and simply won't exist until something is built for that machine.
pub fn arch_dir_name() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        std::env::consts::ARCH
    }
}

/// Which build of a native library to look for. Both variants can coexist on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LibVariant {
    /// The default build: `deps/<name>/release/{platform}/{arch}/{version}/`.
    Standard,
    /// The streaming build: `deps/<name>/release/with_streaming/{platform}/{arch}/{version}/`.
    Streaming,
}

impl LibVariant {
    /// The subdirectory of `deps/<name>/release/` that holds this variant's builds.
    pub const STREAMING_SUBDIR: &'static str = "with_streaming";

    /// Maps a submodule's `deps/<name>/release` directory to this variant's release root.
    /// `Standard` returns it unchanged (the `with_streaming` sibling is never mistaken for
    /// a platform directory, since lookups always join `{platform}/{arch}` first).
    pub fn release_root(self, base: &Path) -> PathBuf {
        match self {
            LibVariant::Standard => base.to_path_buf(),
            LibVariant::Streaming => base.join(Self::STREAMING_SUBDIR),
        }
    }
}

/// How many independent readers to run in parallel by default: this machine's logical CPU
/// count, with no fixed cap (a 32-core machine gets 32). Random-access decoding is
/// CPU-bound, so throughput stops rising at this point and falls past it. Callers may pass
/// any other number.
pub fn recommended_reader_threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

/// This machine's physical core count -- excludes hyperthread/SMT siblings, unlike
/// [`recommended_reader_threads`]'s logical count. Needed because `logical / 2` is not a
/// safe stand-in: on a hybrid P/E CPU (e.g. this project's own dev machine, 6 hyperthreaded
/// P-cores + 8 non-hyperthreaded E-cores = 20 logical but 14 physical), that guess is
/// wrong. See `rom::decode_threads` for where this is used as a thread-count default.
/// Falls back to the logical count if physical detection isn't available on this platform.
pub fn physical_core_count() -> usize {
    num_cpus::get_physical().max(1)
}

/// True if the library at `path` loads and exports `symbol` (a plain C name, no NUL).
/// False if it's missing, fails to load, or lacks the symbol.
pub fn library_exports(path: &Path, symbol: &str) -> bool {
    let Ok(lib) = (unsafe { Library::new(path) }) else {
        return false;
    };
    unsafe { lib.get::<*const ()>(symbol.as_bytes()) }.is_ok()
}

/// What one native library build offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibCapabilities {
    /// The library file found for the requested variant, or `None` if not built.
    pub path: Option<PathBuf>,
    /// The library exports its streaming entry point (see each wrapper's
    /// `STREAMING_SYMBOL`). Always false when `path` is `None`.
    pub streaming: bool,
}

impl LibCapabilities {
    fn probe(path: Option<PathBuf>, streaming_symbol: &str) -> Self {
        let streaming = path.as_deref().is_some_and(|p| library_exports(p, streaming_symbol));
        Self { path, streaming }
    }
}

/// Capabilities of all three native libraries for one [`LibVariant`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeCapabilities {
    pub variant: LibVariant,
    /// 7z.so / 7z.dll / 7z.dylib (zip, 7z).
    pub sevenzip: LibCapabilities,
    /// libchdman (chd).
    pub chdman: LibCapabilities,
    /// libdolphinrvz (rvz, wia, gcz).
    pub dolphin: LibCapabilities,
}

impl NativeCapabilities {
    /// True if every library was found and can stream.
    pub fn all_streaming(&self) -> bool {
        self.sevenzip.streaming && self.chdman.streaming && self.dolphin.streaming
    }
}

/// Locates each native library for `variant` and probes whether it exports its streaming
/// entry point. Loads (and unloads) each found library once.
pub fn detect_capabilities(variant: LibVariant) -> NativeCapabilities {
    use super::{chdman_wrapper, dolphin_wrapper, sevenzip_wrapper};
    NativeCapabilities {
        variant,
        sevenzip: LibCapabilities::probe(
            sevenzip_wrapper::find_library(variant),
            sevenzip_wrapper::STREAMING_SYMBOL,
        ),
        chdman: LibCapabilities::probe(chdman_wrapper::find_library(variant), chdman_wrapper::STREAMING_SYMBOL),
        dolphin: LibCapabilities::probe(
            dolphin_wrapper::find_library(variant),
            dolphin_wrapper::STREAMING_SYMBOL,
        ),
    }
}

/// Returns this build's (platform, arch) directory names, matching the convention
/// `deps/<name>/scripts/build_*.sh` uses for its output layout. `None` on a target not covered
/// by that layout (windows/linux x64 and arm64, mac/android/ios arm64).
pub(crate) fn platform_arch_dirs() -> Option<(&'static str, &'static str)> {
    let platform = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else if cfg!(target_os = "android") {
        "android"
    } else if cfg!(target_os = "ios") {
        "ios"
    } else {
        return None;
    };

    let arch = if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        return None;
    };

    Some((platform, arch))
}

/// The dynamic library filename `build_*.sh` produces for `base_name` on this platform
/// (e.g. "chdman" -> "chdman.dll" on Windows, "libchdman.so" on Linux/Android,
/// "libchdman.dylib" on macOS/iOS).
pub(crate) fn dynamic_lib_filename(base_name: &str) -> Option<String> {
    if cfg!(target_os = "windows") {
        Some(format!("{base_name}.dll"))
    } else if cfg!(target_os = "macos") || cfg!(target_os = "ios") {
        Some(format!("lib{base_name}.dylib"))
    } else if cfg!(target_os = "linux") || cfg!(target_os = "android") {
        Some(format!("lib{base_name}.so"))
    } else {
        None
    }
}

/// Finds the most-recently-built version directory under
/// `<release_root>/<platform>/<arch>/` -- i.e. picks whichever `<version>` subdirectory
/// was modified last, by directory mtime. A version directory's mtime is set once, when
/// `build_*.sh` creates it, so this always resolves to whichever build actually ran most
/// recently -- exactly "the newest one you built", regardless of whether its version
/// string (a `git describe` tag, or "dev") sorts meaningfully as a version.
/// Whether a directory name looks like a version: `2.3.90`, `20260927`, or 7-Zip-zstd's
/// `v26.02-v1.5.7-R2-55-g3adff3e5`. Deliberately not "anything that isn't include/dynamic" --
/// an allowlist fails closed when a new unversioned sibling appears.
fn is_version_dir(name: &str) -> bool {
    let rest = name.strip_prefix('v').unwrap_or(name);
    rest.starts_with(|c: char| c.is_ascii_digit())
}

pub(crate) fn find_latest_version_dir(release_root: &Path) -> Option<PathBuf> {
    let (platform, arch) = platform_arch_dirs()?;
    let base = release_root.join(platform).join(arch);

    let versioned = std::fs::read_dir(&base)
        .ok()?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        // Only version directories. RALibretro's tree keeps an unversioned `include/` and
        // `dynamic/` beside its dated build, and taking the newest child blindly lands on one
        // of those and then fails to find the library inside it.
        //
        // Most versions start with a digit (2.3.90, 4.2.0, 11.8.3, 20260927), but 7-Zip-zstd's
        // are `v`-prefixed (`v26.02-v1.5.7-R2-55-g3adff3e5`) -- a digits-only filter rejected
        // every one of them, so the submodule fallback could never find `7z.so` and any build
        // not run from a deployed app root silently used the slow pure-Rust zip reader
        // instead. The non-version siblings are `include`/`dynamic`, which match neither
        // shape. Mirrored in scripts/move_libs_to_app_root.sh.
        .filter(|entry| is_version_dir(&entry.file_name().to_string_lossy()))
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, entry.path()))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path);
    // Not every tree is versioned: RALibretro's mac/arm64 build is just `dynamic/` and
    // `include/` directly under the arch. Fall back to the arch directory itself, which is
    // then what the caller appends `dynamic/<lib>` to.
    versioned.or(Some(base))
}

/// Finds `<release_root>/<platform>/<arch>/<latest-version>/dynamic/<lib file for base_name>`,
/// or `None` if no such directory/file exists yet (native library not built for this
/// platform/arch).
pub fn find_native_library(release_root: &Path, base_name: &str) -> Option<PathBuf> {
    let filename = dynamic_lib_filename(base_name)?;
    let version_dir = find_latest_version_dir(release_root)?;
    let candidate = version_dir.join("dynamic").join(filename);
    candidate.exists().then_some(candidate)
}

/// Finds the `include/` directory alongside the same version's `dynamic/` output --
/// mainly useful for tooling/documentation, not needed at runtime by the FFI bindings
/// themselves (which hardcode the C API shape in Rust already).
#[allow(dead_code)]
pub fn find_native_include_dir(release_root: &Path) -> Option<PathBuf> {
    let version_dir = find_latest_version_dir(release_root)?;
    let candidate = version_dir.join("include");
    candidate.is_dir().then_some(candidate)
}

/// The checkout this binary was built from: where the development fallbacks look for a
/// dependency's own `deps/<name>/release/` tree and for `app_root_ex/` when nothing is deployed.
///
/// A plain `env!("CARGO_MANIFEST_DIR")` compiles the absolute path of the build machine's
/// checkout -- account name included -- into every binary, shipped ones too, and
/// `--remap-path-prefix` does not touch it because it is an environment variable, not a
/// source location. `scripts/package.sh` therefore builds with `ROMEXPLORER_SOURCE_ROOT=.`,
/// which leaves a packaged binary with a relative path that resolves to nothing and names
/// no one; a plain `cargo build` in the tree is unchanged.
pub fn source_root() -> PathBuf {
    PathBuf::from(match option_env!("ROMEXPLORER_SOURCE_ROOT") {
        Some(root) => root,
        None => env!("CARGO_MANIFEST_DIR"),
    })
}

/// Finds `<exe_root>/lib/<os_dir_name>/<arch_dir_name>/<dynamic lib file for base_name>` --
/// the flat, per-OS-and-arch layout `scripts/move_libs_to_app_root.sh` produces (single-lib
/// submodules land directly under `<os>/<arch>/`, not nested by version the way each
/// dependency's own `deps/<name>/release/` tree is). The arch level is what lets one app root carry,
/// say, both x64 and arm64 Linux builds. Every native-library wrapper in this crate
/// (chdman/dolphinrvz/7z/RAHasher/zlib-ng) tries this first, falling back to its own
/// submodule-tree search (`find_native_library`, or a wrapper's own equivalent) when
/// nothing has been moved to the app root yet -- so a dev who's only built a submodule
/// directly still has everything work exactly as before this existed.
/// `rommusic`: the console sound-chip players in one library (`deps/rommusic`), used by
/// [`super::gme`], [`super::snes_apu`] and [`super::agbplay`].
pub fn rommusic_library() -> Option<PathBuf> {
    find_app_root_library("rommusic").or_else(|| find_native_library(&source_root().join("deps/rommusic/release"), "rommusic"))
}

pub fn find_app_root_library(base_name: &str) -> Option<PathBuf> {
    let filename = dynamic_lib_filename(base_name)?;
    let candidate = exe_root()?.join("lib").join(os_dir_name()).join(arch_dir_name()).join(filename);
    candidate.exists().then_some(candidate)
}

#[cfg(test)]
mod app_root_tests {
    use super::*;

    /// The regression guard for the two roots disagreeing.
    ///
    /// `AppPaths::detect` looks in more places than the executable's own directory, which is
    /// all `exe_root` used to check. Any layout where the root is somewhere else therefore
    /// found MariaDB (which asks `AppPaths`) and none of the `dlopen`ed libraries --
    /// silently, because each wrapper just falls back to a `submodules/` tree and reports
    /// itself unavailable.
    #[test]
    fn the_library_lookup_follows_the_app_root() {
        let tmp = std::env::temp_dir().join(format!("rx-approot-{}", std::process::id()));
        let libdir = tmp.join("lib").join(os_dir_name()).join(arch_dir_name());
        std::fs::create_dir_all(&libdir).unwrap();
        let name = dynamic_lib_filename("rxtestlib").unwrap();
        std::fs::write(libdir.join(&name), b"not a real library").unwrap();

        // No ROMEXPLORER_EXE_ROOT: the whole point is that the app root alone is enough.
        std::env::remove_var(EXE_ROOT_OVERRIDE_ENV);
        std::env::set_var("ROMEXPLORER_APP_ROOT", &tmp);
        let found = find_app_root_library("rxtestlib");
        std::env::remove_var("ROMEXPLORER_APP_ROOT");

        assert_eq!(found.as_deref(), Some(libdir.join(&name).as_path()), "the app root's lib/<os>/<arch>/ is where a shipped library lives");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn the_explicit_override_still_wins() {
        std::env::set_var(EXE_ROOT_OVERRIDE_ENV, "/somewhere/else");
        let root = exe_root();
        std::env::remove_var(EXE_ROOT_OVERRIDE_ENV);
        assert_eq!(root.as_deref(), Some(Path::new("/somewhere/else")));
    }
}

#[cfg(test)]
mod version_dir_tests {
    use super::is_version_dir;

    #[test]
    fn version_directories_of_both_shapes_are_recognized() {
        // Digit-led, as chdman / dolphin / openssl / mariadb name theirs.
        for name in ["2.3.90", "4.2.0", "11.8.3", "20260927"] {
            assert!(is_version_dir(name), "{name} is a version");
        }
        // 7-Zip-zstd's own shape. A digits-only filter rejected these, which is why the
        // submodule fallback could never find `7z.so`.
        assert!(is_version_dir("v26.02-v1.5.7-R2-55-g3adff3e5"));
    }

    #[test]
    fn unversioned_siblings_are_still_rejected() {
        // Taking one of these as "the newest build" is the bug the filter exists for.
        for name in ["include", "dynamic", "static", "_build", "vendor"] {
            assert!(!is_version_dir(name), "{name} is not a version");
        }
    }
}

/// Runs a Rust callback a native library called, `fallback` if it panics. A panic must not
/// unwind into C (that aborts the whole process); the fallback is "stop", which the library
/// reports as a cancel, and the panic itself has already been printed by the panic hook.
pub fn guarded<T>(fallback: T, f: impl FnOnce() -> T) -> T {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or(fallback)
}
