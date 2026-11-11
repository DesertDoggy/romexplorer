//! Nintendo 3DS titles: `.cia`, `.3ds` / `.cci`, `.cxi` and Azahar's compressed `.zcci` /
//! `.zcia` / `.zcxi`, decrypted and rewritten into the form an emulator reads.
//!
//! The decryption is cia-tool's (`cia-tool/` in this repo, pure Rust). No console keys come
//! with it: `aes_keys.txt` (or a `boot9.bin`) and, for titles with seed crypto, `seeddb.bin`
//! are the user's own, looked for beside the file, then in the program's folder and its
//! `assets` folder ([`KeyFiles::find`], [`key_dirs`]). Nothing asks for them: a file whose
//! keys are not there fails, and [`check`] says which one it lacks.
//!
//! What a title becomes ([`Target`]): a game is made a decrypted cartridge image, `.cci` or
//! `.3ds` (the same bytes under either name), or that image compressed as `.zcci`; or just its
//! main part (the NCCH Azahar boots, without the manual or download-play parts) as `.cxi`, or
//! compressed as `.zcxi`. An update, DLC, demo or system title cannot be either -- its title
//! id says which it is -- so it is kept a CIA, decrypted: `.cia`, or `.zcia` when the target
//! compresses. A lone NCCH stays one: `.cxi`, or `.zcxi`; homebrew (`.3dsx`, never encrypted)
//! too: `.3dsx`, or `.z3dsx`.
//!
//! The compressed files are Azahar's Z3DS format, written here from its layout (Azahar's own
//! code is not used): a 0x20-byte header (`Z3DS`, the plain file's magic, version 1, header
//! and metadata sizes, compressed and plain sizes), a metadata block of named items padded
//! to 16 bytes, then the plain file as independent zstd frames followed by a zstd seekable
//! seek table, so the emulator can read any part without unpacking the rest. Frames are
//! 256 KiB for a cartridge image and 32 MiB for a CIA, and zstd level 3, all as Azahar
//! writes them; level 3 is also this repo's measured choice for zstd (CLAUDE.md).

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use cia_tool::cci::{self, CciOutcome, Ncsd};
use cia_tool::cia::{self, Cia, CiaOutcome};
use cia_tool::keys::KeyStore;
use cia_tool::ncch::{self, NcchCrypto, NcchHeader};
use cia_tool::seeddb::SeedDb;

/// Extensions taken as 3DS titles.
pub const EXTENSIONS: [&str; 10] = ["cia", "3ds", "cci", "cxi", "zcci", "zcia", "zcxi", "cfa", "3dsx", "z3dsx"];

/// What a game is written as. Updates, DLC, demos and system titles stay a CIA (see the module
/// docs), compressed when the target is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Target {
    /// Decrypted cartridge image named `.cci`.
    #[default]
    Cci,
    /// The same image named `.3ds`.
    ThreeDs,
    /// The image in Azahar's compressed format.
    Zcci,
    /// The game's main NCCH alone, decrypted.
    Cxi,
    /// That NCCH in Azahar's compressed format.
    Zcxi,
}

impl Target {
    pub const ALL: [Target; 5] = [Target::Cci, Target::ThreeDs, Target::Zcci, Target::Cxi, Target::Zcxi];

    pub fn name(self) -> &'static str {
        match self {
            Self::Cci => "cci",
            Self::ThreeDs => "3ds",
            Self::Zcci => "zcci",
            Self::Cxi => "cxi",
            Self::Zcxi => "zcxi",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.name().eq_ignore_ascii_case(name.trim().trim_start_matches('.')))
    }

    pub fn compresses(self) -> bool {
        matches!(self, Self::Zcci | Self::Zcxi)
    }
}

/// The key material one conversion runs with.
#[derive(Default, Clone)]
pub struct Keys {
    keys: KeyStore,
    seeds: SeedDb,
    /// Where each part came from, for the job's log ("aes_keys.txt beside the file").
    pub sources: Vec<String>,
}

/// Key files found on disk.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct KeyFiles {
    pub aes_keys: Option<PathBuf>,
    pub boot9: Option<PathBuf>,
    pub seeddb: Option<PathBuf>,
}

/// Where a conversion of `input` looks for key files, in order: the input's own folder, the
/// folder of the running program, and `extra` (an app's root, say).
pub fn key_dirs(input: &Path, extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(dir) = input.parent() {
        dirs.push(dir.to_path_buf());
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) {
        dirs.push(dir);
    }
    for dir in extra {
        if !dirs.contains(dir) {
            dirs.push(dir.clone());
        }
    }
    dirs
}

impl KeyFiles {
    /// The key files in `dirs`, each folder and then its `assets` folder. The first of each
    /// kind wins.
    pub fn find(dirs: &[PathBuf]) -> Self {
        let all: Vec<PathBuf> = dirs.iter().flat_map(|dir| [dir.clone(), dir.join("assets")]).collect();
        let first = |names: &[&str]| all.iter().flat_map(|d| names.iter().map(move |n| d.join(n))).find(|p| p.is_file());
        Self { aes_keys: first(&["aes_keys.txt"]), boot9: first(&["boot9.bin", "boot9_prot.bin"]), seeddb: first(&["seeddb.bin"]) }
    }
}

impl Keys {
    pub fn load(files: &KeyFiles) -> Result<Self, String> {
        let mut out = Self::default();
        if let Some(path) = &files.aes_keys {
            out.keys.load_aes_keys_file(path).map_err(|e| format!("{e:#}"))?;
            out.sources.push(path.display().to_string());
        }
        if let Some(path) = &files.boot9 {
            out.keys.load_boot9(path).map_err(|e| format!("{e:#}"))?;
            out.sources.push(path.display().to_string());
        }
        if let Some(path) = &files.seeddb {
            out.seeds = SeedDb::load(path).map_err(|e| format!("{e:#}"))?;
            out.sources.push(path.display().to_string());
        }
        Ok(out)
    }

    /// Key files as bytes (tests). Each one given replaces what was there for that kind.
    pub fn add_bytes(&mut self, aes_keys: Option<&[u8]>, seeddb: Option<&[u8]>) -> Result<(), String> {
        if let Some(bytes) = aes_keys {
            let (text, _) = cia_tool::text::decode_text(bytes);
            if self.keys.load_aes_keys_text(&text) == 0 {
                return Err("the keys file has no AES keys in it (expected aes_keys.txt as Citra or Azahar reads it)".into());
            }
            self.sources.push("aes_keys.txt".into());
        }
        if let Some(bytes) = seeddb {
            self.seeds = SeedDb::parse(bytes).map_err(|e| format!("{e:#}"))?;
            self.sources.push("seeddb.bin".into());
        }
        Ok(())
    }

    pub fn has_keys(&self) -> bool {
        !self.keys.key_x.is_empty() || self.keys.common_y.iter().chain(&self.keys.common_normal).any(Option::is_some) || !self.keys.normal.is_empty()
    }

    pub fn has_seeds(&self) -> bool {
        !self.seeds.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Cia,
    /// A cartridge image (NCSD).
    Cci,
    /// A single NCCH (`.cxi` / `.cfa` / `.app`).
    Ncch,
    /// Homebrew (`.3dsx`).
    Homebrew,
}

/// What a file is, read from its headers without any keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    pub container: Container,
    /// In Azahar's compressed format.
    pub compressed: bool,
    /// "Game", "Demo", "Patch" (an update), "DLC", "System" or "TWL" (a DSi title), from the
    /// title id; a cartridge image is always "Game".
    pub title_kind: &'static str,
    pub encrypted: bool,
}

impl Info {
    /// Whether this can become a cartridge image: a game, as a CIA or already one.
    pub fn is_game(&self) -> bool {
        self.title_kind == "Game"
    }
}

/// The extension `target` gives a file of this kind.
pub fn output_extension(info: &Info, target: Target) -> &'static str {
    let compress = target.compresses();
    match (info.container, info.is_game(), target) {
        (Container::Ncch, ..) => if compress { "zcxi" } else { "cxi" },
        (Container::Homebrew, ..) => if compress { "z3dsx" } else { "3dsx" },
        (Container::Cia, false, _) => if compress { "zcia" } else { "cia" },
        (_, _, Target::Cci) => "cci",
        (_, _, Target::ThreeDs) => "3ds",
        (_, _, Target::Zcci) => "zcci",
        (_, _, Target::Cxi) => "cxi",
        (_, _, Target::Zcxi) => "zcxi",
    }
}

fn err(e: anyhow::Error) -> String {
    if e.downcast_ref::<cia_tool::Cancelled>().is_some() {
        "cancelled".to_string()
    } else {
        format!("{e:#}")
    }
}

fn open(path: &Path) -> Result<File, String> {
    File::open(path).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn inspect(path: &Path) -> Result<Info, String> {
    let mut f = open(path)?;
    if let Some(header) = Z3dsHeader::read(&mut f)? {
        let container = match &header.underlying {
            b"NCSD" => Container::Cci,
            b"NCCH" => Container::Ncch,
            b"CIA\0" => Container::Cia,
            b"3DSX" => Container::Homebrew,
            other => return Err(format!("{}: compressed, but holds an unknown kind of file ({})", path.display(), String::from_utf8_lossy(other))),
        };
        // The title id is in the metadata Azahar (and this module) writes; without it a CIA
        // is taken for a game, and the conversion finds out for certain once it is unpacked.
        let title_kind = match (container, header.title_id(&mut f)) {
            (Container::Cia, Some(tid)) => cia::title_kind(tid),
            _ => "Game",
        };
        return Ok(Info { container, compressed: true, title_kind, encrypted: false });
    }
    let mut magic = [0u8; 4];
    if f.read_exact(&mut magic).is_ok() && &magic == b"3DSX" {
        return Ok(Info { container: Container::Homebrew, compressed: false, title_kind: "Homebrew", encrypted: false });
    }
    let fmt = cia_tool::process::detect(&mut f).map_err(|e| format!("{}: {e:#}", path.display()))?;
    Ok(match fmt {
        cia_tool::process::Format::Cia => {
            let c = Cia::open(&mut f).map_err(|e| format!("{}: {e:#}", path.display()))?;
            let encrypted = c.any_encrypted() || cia::content_headers(&mut f, &c, None).iter().flatten().any(|h| !h.is_decrypted());
            Info { container: Container::Cia, compressed: false, title_kind: cia::title_kind(c.title_id), encrypted }
        }
        cia_tool::process::Format::Cci => {
            let n = Ncsd::open(&mut f).map_err(|e| format!("{}: {e:#}", path.display()))?;
            let encrypted = n.partitions.iter().any(|p| n.partition_header(&mut f, p.0).is_some_and(|h| !h.is_decrypted()));
            Info { container: Container::Cci, compressed: false, title_kind: "Game", encrypted }
        }
        cia_tool::process::Format::Ncch => {
            let mut h = [0u8; 0x200];
            f.read_exact(&mut h).map_err(|e| format!("{}: {e}", path.display()))?;
            let h = NcchHeader::parse(&h).map_err(|e| format!("{}: {e:#}", path.display()))?;
            Info { container: Container::Ncch, compressed: false, title_kind: "Game", encrypted: !h.is_decrypted() }
        }
    })
}

/// What a conversion of the file would still need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Missing {
    /// aes_keys.txt (or boot9.bin), with what exactly is lacking.
    Keys(String),
    /// seeddb.bin, or a newer one: the title (by id) uses seed crypto and its seed is not in it.
    Seed(u64),
}

impl std::fmt::Display for Missing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Keys(why) => write!(f, "needs your 3DS keys (aes_keys.txt): {why}"),
            Self::Seed(tid) => write!(f, "title {tid:016X} uses seed crypto: needs seeddb.bin with its seed"),
        }
    }
}

/// An encrypted CIA's product code (`CTR-P-XXXX`), read with the keys found beside it or
/// beside the program. `None` when it is not a CIA, the keys aren't there, or it has none.
pub fn cia_product_code(path: &Path) -> Option<String> {
    let mut f = open(path).ok()?;
    let c = Cia::open(&mut f).ok()?;
    let title_key = if c.any_encrypted() {
        let keys = Keys::load(&KeyFiles::find(&key_dirs(path, &[]))).ok()?;
        if !keys.has_keys() {
            return None;
        }
        Some(c.title_key(&keys.keys).ok()?)
    } else {
        None
    };
    let first = c.contents.iter().position(|content| content.index == 0).unwrap_or(0);
    let header = cia::content_headers(&mut f, &c, title_key.as_ref()).into_iter().nth(first)??;
    let code = header.product_code.trim().trim_end_matches('\0').to_ascii_uppercase();
    (code.len() > 3 && code.is_ascii()).then_some(code)
}

/// `Ok(None)` when `keys` are enough to convert the file. A compressed file is taken as
/// decrypted, as Azahar only compresses decrypted ones.
pub fn check(path: &Path, keys: &Keys) -> Result<Option<Missing>, String> {
    let info = inspect(path)?;
    if !info.encrypted {
        return Ok(None);
    }
    let mut f = open(path)?;
    let headers: Vec<NcchHeader> = match info.container {
        Container::Cia => {
            let c = Cia::open(&mut f).map_err(|e| format!("{}: {e:#}", path.display()))?;
            let tk = if c.any_encrypted() {
                match c.title_key(&keys.keys) {
                    Ok(tk) => Some(tk),
                    Err(e) => return Ok(Some(Missing::Keys(format!("{e:#}")))),
                }
            } else {
                None
            };
            cia::content_headers(&mut f, &c, tk.as_ref()).into_iter().flatten().collect()
        }
        Container::Cci => {
            let n = Ncsd::open(&mut f).map_err(|e| format!("{}: {e:#}", path.display()))?;
            n.partitions.iter().filter_map(|p| n.partition_header(&mut f, p.0)).collect()
        }
        Container::Ncch => {
            let mut h = [0u8; 0x200];
            f.read_exact(&mut h).map_err(|e| format!("{}: {e}", path.display()))?;
            vec![NcchHeader::parse(&h).map_err(|e| format!("{e:#}"))?]
        }
        Container::Homebrew => Vec::new(),
    };
    for h in headers.iter().filter(|h| !h.is_decrypted()) {
        if h.uses_seed() && keys.seeds.get(h.program_id).is_none() {
            return Ok(Some(Missing::Seed(h.program_id)));
        }
        if let Err(e) = NcchCrypto::new(h, &keys.keys, &keys.seeds) {
            let text = format!("{e:#}");
            // A seed that is there but wrong is a seeddb problem, not a keys one.
            return Ok(Some(if text.contains("seeddb") { Missing::Seed(h.program_id) } else { Missing::Keys(text) }));
        }
    }
    Ok(None)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Decrypted (and compressed, for a compressing target).
    Converted,
    /// Already decrypted: re-containered or copied as it is.
    AlreadyDecrypted,
}

/// A scratch file beside `dst`, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn beside(dst: &Path, tag: &str) -> Self {
        let name = dst.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Self(dst.with_file_name(format!(".{name}.{tag}.part")))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Converts `src` into `dst` (whose extension the caller took from [`output_extension`]).
/// `progress(done, total)` is in parts of `total`, and returning `false` stops it with the
/// error `"cancelled"`. On an error `dst` may be left half-written: the caller writes to a
/// part file and moves it into place.
pub fn convert(src: &Path, dst: &Path, target: Target, keys: &Keys, progress: &mut dyn FnMut(u64, u64) -> bool) -> Result<Outcome, String> {
    const SCALE: u64 = 1000;
    let info = inspect(src)?;
    let wanted = output_extension(&info, target);
    let compress = wanted.starts_with('z');
    // The game's main NCCH out of a cartridge image: decrypted whole first, then cut out.
    let cut = wanted.ends_with("cxi") && info.container != Container::Ncch;
    // Up to four passes: unpack a compressed input, decrypt, cut, compress. Each its share.
    let passes = info.compressed as u64 + 1 + cut as u64 + compress as u64;
    let mut pass = 0u64;
    let mut step = |pass: u64, done: u64, total: u64| -> bool {
        let within = (done.min(total) * SCALE).checked_div(total).unwrap_or(0);
        progress(pass * SCALE + within, passes * SCALE)
    };

    // A compressed input: unpacked first, unless it is already what is wanted.
    let unpacked;
    let plain_src: &Path = if info.compressed {
        let current = src.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        if wanted == current {
            copy_file(src, dst, &mut |d, t| step(0, d, t))?;
            return Ok(Outcome::AlreadyDecrypted);
        }
        unpacked = Scratch::beside(dst, "unpacked");
        z3ds_decompress(src, &unpacked.0, &mut |d, t| step(pass, d, t))?;
        pass += 1;
        &unpacked.0
    } else {
        src
    };
    let info = Info { compressed: false, ..inspect(plain_src)? };

    let plain;
    let plain_dst: &Path = if compress {
        plain = Scratch::beside(dst, "plain");
        &plain.0
    } else {
        dst
    };
    let outcome = if cut {
        let image = Scratch::beside(dst, "image");
        let outcome = decrypt(plain_src, &image.0, &info, keys, &mut |d, t| step(pass, d, t))?;
        pass += 1;
        main_ncch(&image.0, plain_dst, &mut |d, t| step(pass, d, t))?;
        outcome
    } else {
        decrypt(plain_src, plain_dst, &info, keys, &mut |d, t| step(pass, d, t))?
    };
    pass += 1;
    if compress {
        let magic: [u8; 4] = match wanted {
            "zcia" => *b"CIA\0",
            "zcxi" => *b"NCCH",
            "z3dsx" => *b"3DSX",
            _ => *b"NCSD",
        };
        z3ds_compress(plain_dst, dst, magic, &mut |d, t| step(pass, d, t))?;
    }
    Ok(outcome)
}

/// Partition 0 of a decrypted cartridge image: the game's NCCH, as a `.cxi`.
fn main_ncch(image: &Path, dst: &Path, progress: &mut dyn FnMut(u64, u64) -> bool) -> Result<(), String> {
    let mut f = open(image)?;
    let n = Ncsd::open(&mut f).map_err(err)?;
    let Some(&(_, offset, size)) = n.partitions.iter().find(|p| p.0 == 0) else { return Err(format!("{}: no game partition", image.display())) };
    f.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
    let mut input = f.take(size);
    let mut out = BufWriter::with_capacity(8 << 20, File::create(dst).map_err(|e| format!("{}: {e}", dst.display()))?);
    let mut buf = vec![0u8; 4 << 20];
    let mut done = 0u64;
    loop {
        let read = input.read(&mut buf).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        out.write_all(&buf[..read]).map_err(|e| format!("{}: {e}", dst.display()))?;
        done += read as u64;
        if !progress(done, size) {
            return Err("cancelled".into());
        }
    }
    if done != size {
        return Err(format!("{}: the game partition is cut short", image.display()));
    }
    out.flush().map_err(|e| e.to_string())
}

/// The decrypted plain file: a game as a cartridge image, any other CIA as a CIA, an NCCH as
/// an NCCH.
fn decrypt(src: &Path, dst: &Path, info: &Info, keys: &Keys, progress: &mut dyn FnMut(u64, u64) -> bool) -> Result<Outcome, String> {
    let mut f = open(src)?;
    let mut out = File::create(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    let (k, s) = (&keys.keys, &keys.seeds);
    let outcome = match info.container {
        Container::Cia if info.is_game() => {
            let c = Cia::open(&mut f).map_err(err)?;
            cci::cia_to_cci(&mut f, &c, &mut out, k, s, progress).map_err(err)?;
            if info.encrypted { Outcome::Converted } else { Outcome::AlreadyDecrypted }
        }
        Container::Cia => {
            let c = Cia::open(&mut f).map_err(err)?;
            match cia::decrypt_cia(&mut f, &c, &mut out, k, s, progress).map_err(err)? {
                CiaOutcome::Decrypted => Outcome::Converted,
                CiaOutcome::AlreadyDecrypted => {
                    drop(out);
                    copy_file(src, dst, progress)?;
                    Outcome::AlreadyDecrypted
                }
            }
        }
        Container::Cci => {
            let n = Ncsd::open(&mut f).map_err(err)?;
            match cci::decrypt_cci(&mut f, &n, &mut out, true, k, s, progress).map_err(err)? {
                CciOutcome::Decrypted => Outcome::Converted,
                CciOutcome::AlreadyDecrypted => {
                    drop(out);
                    copy_file(src, dst, progress)?;
                    Outcome::AlreadyDecrypted
                }
            }
        }
        Container::Homebrew => {
            drop(out);
            copy_file(src, dst, progress)?;
            Outcome::AlreadyDecrypted
        }
        Container::Ncch => {
            if !info.encrypted {
                drop(out);
                copy_file(src, dst, progress)?;
                return Ok(Outcome::AlreadyDecrypted);
            }
            let len = f.metadata().map_err(|e| e.to_string())?.len();
            let mut source = |b: &mut [u8]| -> anyhow::Result<()> { Ok(f.read_exact(b)?) };
            let mut done = 0u64;
            let mut sink = |b: &[u8]| -> anyhow::Result<()> {
                out.write_all(b)?;
                done += b.len() as u64;
                if !progress(done, len) {
                    return Err(cia_tool::Cancelled.into());
                }
                Ok(())
            };
            ncch::stream_ncch(len, &mut source, k, s, &mut sink).map_err(err)?;
            Outcome::Converted
        }
    };
    Ok(outcome)
}

fn copy_file(src: &Path, dst: &Path, progress: &mut dyn FnMut(u64, u64) -> bool) -> Result<(), String> {
    let mut input = open(src)?;
    let total = input.metadata().map(|m| m.len()).unwrap_or(0);
    let mut output = File::create(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    let mut buf = vec![0u8; 4 << 20];
    let mut done = 0u64;
    loop {
        let n = input.read(&mut buf).map_err(|e| format!("{}: {e}", src.display()))?;
        if n == 0 {
            break;
        }
        output.write_all(&buf[..n]).map_err(|e| format!("{}: {e}", dst.display()))?;
        done += n as u64;
        if !progress(done, total) {
            return Err("cancelled".into());
        }
    }
    output.flush().map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Z3DS
// ---------------------------------------------------------------------------

const Z3DS_MAGIC: &[u8; 4] = b"Z3DS";
const Z3DS_HEADER_SIZE: u16 = 0x20;
const SEEKABLE_SKIPPABLE: u32 = 0x184D_2A5E;
const SEEKABLE_FOOTER: u32 = 0x8F92_EAB1;
/// Azahar's frame sizes: small for a cartridge image, which is read at random, larger for a
/// CIA, which is only ever installed from start to end.
pub const FRAME_SIZE: usize = 256 << 10;
pub const CIA_FRAME_SIZE: usize = 32 << 20;
/// Azahar writes `ZSTD_CLEVEL_DEFAULT`, which is 3.
const LEVEL: i32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Z3dsHeader {
    underlying: [u8; 4],
    header_size: u16,
    metadata_size: u32,
    compressed_size: u64,
    uncompressed_size: u64,
}

impl Z3dsHeader {
    /// `Ok(None)`: not a Z3DS file. Leaves `f` at its start.
    fn read(f: &mut File) -> Result<Option<Self>, String> {
        let mut raw = [0u8; 0x20];
        f.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let n = f.read(&mut raw).map_err(|e| e.to_string())?;
        f.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        if n < raw.len() || &raw[0..4] != Z3DS_MAGIC {
            return Ok(None);
        }
        if raw[8] != 1 {
            return Err(format!("compressed 3DS file of version {}, which this does not know", raw[8]));
        }
        Ok(Some(Self {
            underlying: raw[4..8].try_into().unwrap(),
            header_size: u16::from_le_bytes([raw[10], raw[11]]),
            metadata_size: u32::from_le_bytes(raw[12..16].try_into().unwrap()),
            compressed_size: u64::from_le_bytes(raw[16..24].try_into().unwrap()),
            uncompressed_size: u64::from_le_bytes(raw[24..32].try_into().unwrap()),
        }))
    }

    fn bytes(&self) -> [u8; 0x20] {
        let mut raw = [0u8; 0x20];
        raw[0..4].copy_from_slice(Z3DS_MAGIC);
        raw[4..8].copy_from_slice(&self.underlying);
        raw[8] = 1;
        raw[10..12].copy_from_slice(&self.header_size.to_le_bytes());
        raw[12..16].copy_from_slice(&self.metadata_size.to_le_bytes());
        raw[16..24].copy_from_slice(&self.compressed_size.to_le_bytes());
        raw[24..32].copy_from_slice(&self.uncompressed_size.to_le_bytes());
        raw
    }

    fn metadata(&self, f: &mut File) -> Vec<(String, Vec<u8>)> {
        let mut raw = vec![0u8; self.metadata_size as usize];
        if f.seek(SeekFrom::Start(self.header_size as u64)).is_err() || f.read_exact(&mut raw).is_err() {
            return Vec::new();
        }
        parse_metadata(&raw)
    }

    /// The title id in the `titleinfo` item.
    fn title_id(&self, f: &mut File) -> Option<u64> {
        let items = self.metadata(f);
        let (_, value) = items.iter().find(|(name, _)| name == "titleinfo")?;
        Some(u64::from_le_bytes(value.get(0..8)?.try_into().ok()?))
    }
}

fn metadata_bytes(items: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut out = vec![1u8];
    for (name, value) in items {
        let name = &name.as_bytes()[..name.len().min(0xFF)];
        let value = &value[..value.len().min(0xFFFF)];
        out.push(1);
        out.push(name.len() as u8);
        out.extend_from_slice(&(value.len() as u16).to_le_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(value);
    }
    out.extend_from_slice(&[0; 4]);
    out.resize(out.len().div_ceil(16) * 16, 0);
    out
}

fn parse_metadata(raw: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut items = Vec::new();
    if raw.first() != Some(&1) {
        return items;
    }
    let mut at = 1usize;
    while at + 4 <= raw.len() {
        let (kind, name_len, data_len) = (raw[at], raw[at + 1] as usize, u16::from_le_bytes([raw[at + 2], raw[at + 3]]) as usize);
        at += 4;
        if kind == 0 || at + name_len + data_len > raw.len() {
            break;
        }
        if kind == 1 {
            items.push((String::from_utf8_lossy(&raw[at..at + name_len]).into_owned(), raw[at + name_len..at + name_len + data_len].to_vec()));
        }
        at += name_len + data_len;
    }
    items
}

/// Azahar's `titleinfo` item: title id, size, version, two zero fields (24 bytes, LE). Read
/// from the first NCCH of a cartridge image or NCCH, or the TMD of a CIA.
fn title_info(plain: &Path, magic: [u8; 4]) -> Option<Vec<u8>> {
    let mut f = File::open(plain).ok()?;
    let (tid, size, version, kind) = match &magic {
        b"CIA\0" => {
            let c = Cia::open(&mut f).ok()?;
            let at = c.tmd_body + 0x54;
            let kind = u32::from_be_bytes(c.tmd.get(at..at + 4)?.try_into().ok()?);
            (c.title_id, c.contents.iter().map(|x| x.size).sum::<u64>(), c.title_version, kind)
        }
        _ => {
            let mut head = [0u8; 0x200];
            f.read_exact(&mut head).ok()?;
            if &magic == b"NCSD" {
                let n = Ncsd::open(&mut f).ok()?;
                let h = n.partition_header(&mut f, 0)?;
                (h.program_id, h.content_size, h.version, 0)
            } else {
                let h = NcchHeader::parse(&head).ok()?;
                (h.program_id, h.content_size, h.version, 0)
            }
        }
    };
    let mut out = Vec::with_capacity(24);
    out.extend_from_slice(&tid.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    Some(out)
}

/// `plain` into Azahar's compressed format, its frames compressed on every core.
pub fn z3ds_compress(plain: &Path, dst: &Path, underlying: [u8; 4], progress: &mut dyn FnMut(u64, u64) -> bool) -> Result<(), String> {
    use rayon::prelude::*;

    let frame = if &underlying == b"CIA\0" { CIA_FRAME_SIZE } else { FRAME_SIZE };
    let total = std::fs::metadata(plain).map_err(|e| format!("{}: {e}", plain.display()))?.len();
    let mut items: Vec<(&str, Vec<u8>)> = Vec::new();
    if let Some(info) = title_info(plain, underlying) {
        items.push(("titleinfo", info));
    }
    items.push(("compressor", format!("romexplorer {}", env!("CARGO_PKG_VERSION")).into_bytes()));
    items.push(("date", chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string().into_bytes()));
    items.push(("maxframesize", frame.to_string().into_bytes()));
    let metadata = metadata_bytes(&items);

    let mut input = BufReader::with_capacity(8 << 20, open(plain)?);
    let file = File::create(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    let mut out = BufWriter::with_capacity(8 << 20, file);
    let write_err = |e: std::io::Error| format!("{}: {e}", dst.display());
    out.write_all(&[0u8; 0x20]).map_err(write_err)?;
    out.write_all(&metadata).map_err(write_err)?;

    // A batch of frames per round, compressed side by side, written in order.
    let batch = rayon::current_num_threads().max(1) * if frame == FRAME_SIZE { 16 } else { 1 };
    let mut table: Vec<(u32, u32)> = Vec::new();
    let mut done = 0u64;
    let mut compressed = 0u64;
    loop {
        let mut blocks = Vec::with_capacity(batch);
        for _ in 0..batch {
            let mut block = Vec::with_capacity(frame);
            (&mut input).take(frame as u64).read_to_end(&mut block).map_err(|e| format!("{}: {e}", plain.display()))?;
            if block.is_empty() {
                break;
            }
            blocks.push(block);
        }
        if blocks.is_empty() {
            break;
        }
        let frames: Vec<std::io::Result<Vec<u8>>> = blocks.par_iter().map(|b| zstd::bulk::compress(b, LEVEL)).collect();
        for (block, frame_bytes) in blocks.iter().zip(frames) {
            let frame_bytes = frame_bytes.map_err(|e| format!("zstd: {e}"))?;
            out.write_all(&frame_bytes).map_err(write_err)?;
            table.push((frame_bytes.len() as u32, block.len() as u32));
            compressed += frame_bytes.len() as u64;
            done += block.len() as u64;
        }
        if !progress(done, total) {
            return Err("cancelled".into());
        }
    }
    // The seek table, as zstd's seekable format puts it: a skippable frame, no checksums.
    let mut seek = Vec::with_capacity(table.len() * 8 + 17);
    seek.extend_from_slice(&SEEKABLE_SKIPPABLE.to_le_bytes());
    seek.extend_from_slice(&((table.len() * 8 + 9) as u32).to_le_bytes());
    for (c, d) in &table {
        seek.extend_from_slice(&c.to_le_bytes());
        seek.extend_from_slice(&d.to_le_bytes());
    }
    seek.extend_from_slice(&(table.len() as u32).to_le_bytes());
    seek.push(0);
    seek.extend_from_slice(&SEEKABLE_FOOTER.to_le_bytes());
    out.write_all(&seek).map_err(write_err)?;

    let header = Z3dsHeader { underlying, header_size: Z3DS_HEADER_SIZE, metadata_size: metadata.len() as u32, compressed_size: compressed + seek.len() as u64, uncompressed_size: done };
    let mut file = out.into_inner().map_err(|e| e.to_string())?;
    file.seek(SeekFrom::Start(0)).map_err(write_err)?;
    file.write_all(&header.bytes()).map_err(write_err)?;
    file.flush().map_err(write_err)
}

/// A Z3DS file back to the plain file it holds.
/// The first `len` bytes of a compressed 3DS file, unpacked (fewer when it is shorter), or
/// `None` when `path` is not one. For reading the headers inside without unpacking it all.
pub fn z3ds_head(path: &Path, len: usize) -> Option<Vec<u8>> {
    let mut f = File::open(path).ok()?;
    let header = Z3dsHeader::read(&mut f).ok()??;
    f.seek(SeekFrom::Start(header.header_size as u64 + header.metadata_size as u64)).ok()?;
    let body = BufReader::new(f).take(header.compressed_size);
    let mut decoder = zstd::stream::read::Decoder::with_buffer(body).ok()?;
    let mut out = Vec::with_capacity(len.min(header.uncompressed_size as usize));
    (&mut decoder).take(len as u64).read_to_end(&mut out).ok()?;
    Some(out)
}

/// The title id a compressed 3DS file's metadata gives, if it has one.
pub fn z3ds_title_id(path: &Path) -> Option<u64> {
    let mut f = File::open(path).ok()?;
    let header = Z3dsHeader::read(&mut f).ok()??;
    header.title_id(&mut f)
}

pub fn z3ds_decompress(src: &Path, dst: &Path, progress: &mut dyn FnMut(u64, u64) -> bool) -> Result<(), String> {
    let mut f = open(src)?;
    let Some(header) = Z3dsHeader::read(&mut f)? else { return Err(format!("{}: not a compressed 3DS file", src.display())) };
    let start = header.header_size as u64 + header.metadata_size as u64;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    if start + header.compressed_size > len {
        return Err(format!("{}: cut short (its header says {} bytes more)", src.display(), start + header.compressed_size - len));
    }
    f.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
    // The frames and the seek table are one zstd stream: the decoder skips the skippable frame.
    let body = BufReader::with_capacity(8 << 20, f).take(header.compressed_size);
    let mut decoder = zstd::stream::read::Decoder::with_buffer(body).map_err(|e| e.to_string())?;
    let mut out = BufWriter::with_capacity(8 << 20, File::create(dst).map_err(|e| format!("{}: {e}", dst.display()))?);
    let mut buf = vec![0u8; 4 << 20];
    let mut done = 0u64;
    loop {
        let n = decoder.read(&mut buf).map_err(|e| format!("{}: {e}", src.display()))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| format!("{}: {e}", dst.display()))?;
        done += n as u64;
        if !progress(done, header.uncompressed_size) {
            return Err("cancelled".into());
        }
    }
    if done != header.uncompressed_size {
        return Err(format!("{}: unpacked to {done} bytes, its header says {}", src.display(), header.uncompressed_size));
    }
    out.flush().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("romexplorer_n3ds_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn metadata_round_trips_and_is_padded_to_16() {
        let raw = metadata_bytes(&[("compressor", b"x".to_vec()), ("maxframesize", b"262144".to_vec())]);
        assert_eq!(raw.len() % 16, 0);
        let items = parse_metadata(&raw);
        assert_eq!(items, vec![("compressor".to_string(), b"x".to_vec()), ("maxframesize".to_string(), b"262144".to_vec())]);
    }

    #[test]
    fn z3ds_round_trips_with_a_valid_seek_table() {
        let dir = scratch("z3ds");
        let plain = dir.join("plain.bin");
        // Not a 3DS file: the header names whatever magic it is given.
        let data: Vec<u8> = (0..(FRAME_SIZE * 3 + 1234)).map(|i| (i % 251) as u8 ^ (i / 4096) as u8).collect();
        std::fs::write(&plain, &data).unwrap();
        let packed = dir.join("packed.zcci");
        z3ds_compress(&plain, &packed, *b"NCSD", &mut |_, _| true).unwrap();
        let bytes = std::fs::read(&packed).unwrap();
        assert_eq!(&bytes[0..8], b"Z3DSNCSD");
        let mut f = File::open(&packed).unwrap();
        let header = Z3dsHeader::read(&mut f).unwrap().unwrap();
        assert_eq!(header.uncompressed_size, data.len() as u64);
        assert_eq!(header.header_size as u64 + header.metadata_size as u64 + header.compressed_size, bytes.len() as u64);
        // Seek table: 4 frames, footer magic, entries summing to the plain size.
        let n = bytes.len();
        assert_eq!(u32::from_le_bytes(bytes[n - 4..].try_into().unwrap()), SEEKABLE_FOOTER);
        let frames = u32::from_le_bytes(bytes[n - 9..n - 5].try_into().unwrap()) as usize;
        assert_eq!(frames, 4);
        let table = &bytes[n - 9 - frames * 8..n - 9];
        let plain_sum: u64 = table.chunks(8).map(|e| u32::from_le_bytes(e[4..8].try_into().unwrap()) as u64).sum();
        assert_eq!(plain_sum, data.len() as u64);
        let meta = header.metadata(&mut f);
        assert!(meta.iter().any(|(k, v)| k == "maxframesize" && v == b"262144"));

        let back = dir.join("back.bin");
        z3ds_decompress(&packed, &back, &mut |_, _| true).unwrap();
        assert_eq!(std::fs::read(&back).unwrap(), data);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_game_becomes_a_cartridge_image_and_anything_else_stays_a_cia() {
        let game = Info { container: Container::Cia, compressed: false, title_kind: "Game", encrypted: true };
        let update = Info { title_kind: "Patch", ..game.clone() };
        assert_eq!(output_extension(&game, Target::Cci), "cci");
        assert_eq!(output_extension(&game, Target::ThreeDs), "3ds");
        assert_eq!(output_extension(&game, Target::Zcci), "zcci");
        assert_eq!(output_extension(&update, Target::Cci), "cia");
        assert_eq!(output_extension(&update, Target::ThreeDs), "cia");
        assert_eq!(output_extension(&update, Target::Zcci), "zcia");
        assert_eq!(output_extension(&game, Target::Cxi), "cxi");
        assert_eq!(output_extension(&game, Target::Zcxi), "zcxi");
        assert_eq!(output_extension(&update, Target::Zcxi), "zcia");
        let ncch = Info { container: Container::Ncch, ..game.clone() };
        assert_eq!(output_extension(&ncch, Target::Cci), "cxi");
        assert_eq!(output_extension(&ncch, Target::Zcci), "zcxi");
        let homebrew = Info { container: Container::Homebrew, title_kind: "Homebrew", encrypted: false, ..game.clone() };
        assert_eq!(output_extension(&homebrew, Target::Zcci), "z3dsx");
        assert_eq!(output_extension(&homebrew, Target::Cci), "3dsx");
        assert_eq!(Target::from_name(".3DS"), Some(Target::ThreeDs));
    }
}
