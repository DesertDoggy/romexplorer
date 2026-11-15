//! `chdlite.toml`: what the GUI's settings screen edits and the CLI reads its defaults from.
//!
//! Portable: the file, and everything else chdlite keeps ([`data_dir`]), sits beside the
//! executable. Only where that folder cannot be written to (or inside a macOS `.app` bundle,
//! whose signature a write would break) does it fall back to the per-user directories. Every
//! field has a default, so a missing or partial file is never an error.
//!
//! On Android there is no folder beside an executable: everything is in the app's own files
//! directory (`Context.getFilesDir()`), which only chdlite can see and which uninstalling
//! removes.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "chdlite.toml";

/// CD codecs chdman accepts for `createcd`, and the plain ones for `createdvd`.
pub const CD_CODECS: [&str; 4] = ["cdlz", "cdzs", "cdzl", "cdfl"];
pub const DVD_CODECS: [&str; 5] = ["lzma", "zstd", "zlib", "huff", "flac"];
pub const MAX_CODECS: usize = 4;

pub const RVZ_CODECS: [&str; 5] = ["zstd", "lzma2", "lzma", "bzip2", "none"];
pub const RVZ_BLOCKS: [(&str, u32); 7] =
    [("32k", 32 << 10), ("64k", 64 << 10), ("128k", 128 << 10), ("256k", 256 << 10), ("512k", 512 << 10), ("1m", 1 << 20), ("2m", 2 << 20)];
/// `rahash`: the RetroAchievements hash, and whether the bundled list has achievements for it.
pub const HASHES: [&str; 6] = ["sha1", "md5", "crc32", "sha256", "xxh3", "rahash"];
pub const CUE_STYLES: [&str; 3] = ["redump", "redump_catalog", "chdman"];
/// The sheet an extracted GD-ROM gets.
pub const GDROM_SHEETS: [&str; 2] = ["gdi", "cue"];

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub output: Output,
    pub lite: Lite,
    pub comp: Comp,
    pub rvz: Rvz,
    pub extract: Extract,
    pub hash: Hash,
    pub cartridge: Cartridge,
    pub n3ds: N3ds,
    pub jobs: Jobs,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Output {
    /// Empty: next to each input.
    pub dir: String,
    /// Replace an output that already exists instead of skipping the input.
    pub force: bool,
    /// Ask before making a lossy output (WUA, ZAR, XISO). The GUI's "don't ask again" turns
    /// it off; the console versions only print a warning.
    pub lossy_warning: bool,
}

impl Default for Output {
    fn default() -> Self {
        Self { dir: String::new(), force: false, lossy_warning: true }
    }
}

/// Which machine the fast preset tunes for (`romexplorer::rom::models::ChdPresets`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LitePreset {
    #[default]
    Pc,
    Mobile,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Lite {
    pub preset: LitePreset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompMode {
    /// Every codec chdman has for the media, so it keeps the smallest per hunk.
    #[default]
    Best,
    /// No codec list at all: whatever chdman itself defaults to.
    Chdman,
    /// `cd_codecs` / `dvd_codecs` and the hunk sizes below.
    Custom,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Comp {
    pub mode: CompMode,
    pub cd_codecs: Vec<String>,
    pub dvd_codecs: Vec<String>,
    /// Bytes; 0 lets chdman choose. A CD hunk must be a multiple of 2448, a DVD hunk of 2048.
    pub cd_hunk_size: u32,
    pub dvd_hunk_size: u32,
}

impl Default for Comp {
    fn default() -> Self {
        Self {
            mode: CompMode::Best,
            cd_codecs: CD_CODECS.iter().map(|c| c.to_string()).collect(),
            dvd_codecs: DVD_CODECS[..4].iter().map(|c| c.to_string()).collect(),
            cd_hunk_size: 0,
            dvd_hunk_size: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rvz {
    pub codec: String,
    pub level: i32,
    /// One of [`RVZ_BLOCKS`]' names.
    pub block: String,
    /// Dolphin's "remove junk data". Smaller, but the image no longer round-trips to the
    /// original ISO, so off unless asked for.
    pub scrub: bool,
}

impl Default for Rvz {
    fn default() -> Self {
        // romexplorer's own `RvzArgs::default()`: Dolphin's recommended settings.
        Self { codec: "zstd".into(), level: 5, block: "128k".into(), scrub: false }
    }
}

impl Rvz {
    pub fn block_bytes(&self) -> u32 {
        RVZ_BLOCKS.iter().find(|(name, _)| *name == self.block).map_or(128 << 10, |(_, bytes)| *bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Extract {
    /// One of [`CUE_STYLES`].
    pub cue_style: String,
    /// One of [`GDROM_SHEETS`]: what an extracted GD-ROM CHD is written as. A `.gdi` by
    /// default, which is what Dreamcast emulators and TOSEC expect; `cue` gives the Redump
    /// layout instead. Hashing always uses the cue style's own choice, so a hash still matches
    /// the Redump DAT.
    pub gdrom_sheet: String,
}

impl Default for Extract {
    fn default() -> Self {
        Self { cue_style: "redump".into(), gdrom_sheet: "gdi".into() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hash {
    pub algorithms: Vec<String>,
}

impl Default for Hash {
    fn default() -> Self {
        Self { algorithms: vec!["sha1".into(), "rahash".into()] }
    }
}

/// The layout each cartridge platform's dumps are written in (`crate::cart`). Chosen, never
/// automatic: a dump already in it is skipped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Cartridge {
    /// One of `cart::NES_FORMATS`: `headered` (.nes), `headerless` (.nes) or `unh`
    /// (headerless, .unh).
    pub nes: String,
    /// One of `cart::N64_FORMATS`: `z64` (big-endian), `v64` (byte-swapped), `n64`
    /// (little-endian).
    pub n64: String,
    /// One of `cart::SNES_FORMATS`: `sfc` (no copier header) or `smc` (512-byte copier header).
    pub snes: String,
}

impl Default for Cartridge {
    fn default() -> Self {
        // What current emulators and No-Intro expect.
        Self { nes: "headered".into(), n64: "z64".into(), snes: "sfc".into() }
    }
}

/// What a 3DS title is written as (`crate::n3ds`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct N3ds {
    /// One of `n3ds::FORMATS`: `cci` or `3ds` (a decrypted cartridge image), `zcci` (that
    /// image compressed), `cxi` (the game's main part alone), `zcxi` (that, compressed).
    /// Updates, DLC, demos and system titles stay a decrypted `.cia` (`.zcia` for the
    /// compressed formats).
    pub format: String,
}

impl Default for N3ds {
    fn default() -> Self {
        Self { format: "cci".into() }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Jobs {
    /// Compression threads in total; 0 = automatic ([`Jobs::auto_threads`]).
    pub threads: u32,
    /// Files converted at once; 0 = automatic (a quarter of the cores, at most 4).
    pub concurrent_files: u32,
}

impl Jobs {
    /// Threads in total when `threads` is automatic: the physical cores but 4, left for the
    /// rest of the machine, and at least 1.
    pub fn auto_threads() -> usize {
        romexplorer::rom::native_lib::physical_core_count().saturating_sub(4).max(1)
    }

    /// Files at once when `concurrent_files` is automatic, given `cores` threads in total: a
    /// quarter of them, at most 4. chdman and Dolphin already compress each file on every
    /// thread they are given, so more files at once mostly overlaps each file's single-threaded
    /// stretches (reading, parsing, finishing the header); beyond 4, the files only split the
    /// same cores finer.
    pub fn auto_files(cores: usize) -> usize {
        (cores / 4).clamp(1, 4)
    }

    /// (files at once, threads for each), for `inputs` files. The files together never use
    /// more threads than the total: no more files at once than threads.
    pub fn plan(&self, inputs: usize) -> (usize, u32) {
        let cores = if self.threads > 0 { self.threads as usize } else { Self::auto_threads() };
        let concurrent = if self.concurrent_files > 0 { self.concurrent_files as usize } else { Self::auto_files(cores) };
        let concurrent = concurrent.min(inputs.max(1)).min(cores).max(1);
        (concurrent, (cores / concurrent) as u32)
    }
}

impl Settings {
    pub fn output_dir(&self) -> Option<PathBuf> {
        let dir = self.output.dir.trim();
        (!dir.is_empty()).then(|| PathBuf::from(dir))
    }

    /// Where `input`'s outputs go: the output folder, a relative one taken from the input's
    /// own folder (the window's field has no working folder of its own to mean), or the
    /// input's own folder when none is set. Always absolute.
    pub fn output_dir_for(&self, input: &Path) -> PathBuf {
        let beside = input.parent().filter(|dir| !dir.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
        let dir = match self.output_dir() {
            Some(dir) if dir.is_absolute() => dir,
            Some(dir) => beside.join(dir),
            None => beside.to_path_buf(),
        };
        std::path::absolute(&dir).unwrap_or(dir)
    }

    /// Where the settings file is (or would be written): beside the executable, unless that
    /// folder is not writable (see [`portable_dir`]).
    pub fn path() -> PathBuf {
        if let Some(dir) = android_files_dir() {
            return dir.join(FILE_NAME);
        }
        if let Some(beside) = exe_dir().map(|dir| dir.join(FILE_NAME)).filter(|path| path.is_file()) {
            return beside;
        }
        match portable_dir() {
            Some(dir) => dir.join(FILE_NAME),
            None => user_dir(DirKind::Config).join("chdlite").join(FILE_NAME),
        }
    }

    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path).ok().and_then(|text| toml::from_str(&text).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> Result<PathBuf, String> {
        let path = Self::path();
        self.save_to(&path)?;
        Ok(path)
    }

    /// Why the settings file there was not used, when it exists but does not parse.
    pub fn problem_with(path: &Path) -> Option<String> {
        let text = std::fs::read_to_string(path).ok()?;
        toml::from_str::<Settings>(&text).err().map(|err| format!("{} could not be read, so the defaults are used: {}", path.display(), err.message()))
    }

    pub fn problem() -> Option<String> {
        Self::problem_with(&Self::path())
    }

    /// A file there that does not parse is kept, as `<name>.bak`, rather than overwritten.
    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
        }
        if Self::problem_with(path).is_some() {
            let mut backup = path.as_os_str().to_owned();
            backup.push(".bak");
            std::fs::rename(path, &backup).map_err(|err| format!("{}: {err}", path.display()))?;
        }
        let text = toml::to_string_pretty(self).map_err(|err| err.to_string())?;
        std::fs::write(path, text).map_err(|err| format!("{}: {err}", path.display()))
    }
}

/// The Android app's files directory, where its settings and data live; `None` elsewhere.
#[cfg(target_os = "android")]
fn android_files_dir() -> Option<PathBuf> {
    // The command-line tools are no app: they keep their files like on Linux.
    crate::android::is_app().then(crate::android::files_dir)
}

#[cfg(not(target_os = "android"))]
fn android_files_dir() -> Option<PathBuf> {
    None
}

pub fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(Path::to_path_buf)
}

/// The executable's folder, when chdlite may keep its files there: it can be written to, and
/// it is not inside a macOS `.app` bundle.
pub fn portable_dir() -> Option<PathBuf> {
    let dir = exe_dir()?;
    if dir.ancestors().any(|a| a.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("app"))) {
        return None;
    }
    let probe = dir.join(format!(".chdlite-write-test-{}", std::process::id()));
    let writable = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    writable.then_some(dir)
}

/// Where chdlite keeps everything besides the settings file (the unpacked native libraries,
/// the GUI's webview data): `chdlite-data` beside the executable, or the per-user cache
/// directory when that folder is not writable.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = android_files_dir() {
        return dir;
    }
    match portable_dir() {
        Some(dir) => dir.join("chdlite-data"),
        None => user_dir(DirKind::Cache).join("chdlite"),
    }
}

#[derive(Clone, Copy)]
pub enum DirKind {
    Config,
    Cache,
}

/// The per-user config or cache directory, by each platform's own convention.
pub fn user_dir(kind: DirKind) -> PathBuf {
    let env = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    let home = || env("HOME").unwrap_or_else(std::env::temp_dir);
    if cfg!(windows) {
        let var = match kind {
            DirKind::Config => "APPDATA",
            DirKind::Cache => "LOCALAPPDATA",
        };
        env(var).unwrap_or_else(std::env::temp_dir)
    } else if cfg!(target_os = "macos") {
        home().join("Library").join(match kind {
            DirKind::Config => "Application Support",
            DirKind::Cache => "Caches",
        })
    } else {
        match kind {
            DirKind::Config => env("XDG_CONFIG_HOME").unwrap_or_else(|| home().join(".config")),
            DirKind::Cache => env("XDG_CACHE_HOME").unwrap_or_else(|| home().join(".cache")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_at_once_never_take_more_threads_than_the_total() {
        let jobs = |threads, concurrent_files| Jobs { threads, concurrent_files };
        assert_eq!(jobs(2, 4).plan(10), (2, 1));
        assert_eq!(jobs(8, 4).plan(10), (4, 2));
        assert_eq!(jobs(8, 4).plan(1), (1, 8));
        assert_eq!(jobs(3, 2).plan(10), (2, 1));
        let auto = Jobs::auto_threads();
        assert_eq!(auto, romexplorer::rom::native_lib::physical_core_count().saturating_sub(4).max(1));
        let (concurrent, each) = jobs(0, 0).plan(10);
        assert!(concurrent as u32 * each <= auto as u32 && each >= 1);
    }
}
