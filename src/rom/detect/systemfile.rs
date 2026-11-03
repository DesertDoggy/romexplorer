//! BIOS/system-file identification -- see `docs/plan/bios.md` for the full design
//! (purpose/scope, per-platform data, NAND/SD-card class). This module holds:
//!
//! - The `MAX_BIOS_SIZE`/`MAX_BIOS_SIZE_<PLATFORM>` constants (moved here from
//!   `docs/plan/bios.md`'s own "Constants" section -- that doc now only points here).
//! - `biosdb.bin`'s format and loader (mirrors `convert::nes`'s `fcdb.bin`: magic+version
//!   header, generated offline by `tools/biosdb_gen.rs`, embedded via `include_bytes!`,
//!   with an on-disk-override-then-embedded-fallback load order).
//! - [`identify_system_file`], the detection entry point.
//!
//! **Not part of the per-file ROM detection cycle** -- a BIOS/system file is never a
//! candidate ROM, so this never runs through `platforms::deep_try_platform`. It expects
//! to be called from a directory-context-gated pre-filter (a `dir_type` the caller already
//! knows from scanning), the same way `nand_dispatch.rs`'s structural checks do.
//!
//! Unlike a NAND/SD dump, everything this module can positively identify is
//! deterministically hashable (official/community BIOS dumps and firmware packages are
//! byte-identical across consoles) -- `biosdb.bin` never has `Nand`/`Sdcard` entries; those
//! two [`SystemFileType`] variants exist only as `nand_dispatch.rs`'s structural-signature
//! *outcome*, reported here for a uniform return type.
//!
//! **No extension gate.** A file's size relative to the applicable ceiling is both
//! necessary and sufficient to decide whether a hash-lookup attempt is worth it -- a real
//! BIOS dump's extension varies too much in the wild to gate on reliably (confirmed
//! against real data: PlayStation 2's own BIOS-region dumps alone use `.BIN`/`.EROM`/
//! `.ROM1`/`.ROM2`/`.NVM`/`.MEC`), and a hash lookup that finds no match falls through to
//! `Unknown` regardless of extension/size overlap with a real game -- so there is no
//! correctness reason to also gate on extension, only a marginal, rejected efficiency one.

use std::collections::HashMap;
use std::fs;
use std::io::{self, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::common::models::Platform;

use super::super::hash::{hash, HashOptions, HashReport};
use super::super::content::archive_format_for_extension;

// ---------------------------------------------------------------------------
// Directory context / result types
// ---------------------------------------------------------------------------

/// What kind of directory `path` was found in -- supplied by the (not-yet-built)
/// directory-scanning pass, the same precondition `docs/plan/bios.md`'s heuristic has
/// always required.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirType {
    /// A dedicated BIOS/system-file directory -- not narrowed to one platform.
    Bios,
    /// An ordinary ROM directory for one already-resolved `platform`.
    Rom,
    /// Multiple platforms and/or BIOS+ROMs not cleanly separated -- out of scope, per
    /// `docs/plan/bios.md`'s own scope note (hash-first against the general DAT match
    /// later instead).
    Mixed,
}

/// A closed set of non-BIOS, non-firmware system files retrobios's `bios/` tree also
/// carries -- not a free-form string, so callers can match exhaustively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtherFileKind {
    /// Character-generator/font ROM (MSX `KANJI.rom`, PC-98 `font.rom`, 3DS
    /// `shared_font.bin`, SharpX68000 `cgrom.dat`).
    Font,
    /// Kanji dictionary/lookup data specifically (PC-8801 `KANJI1.ROM`, PC-98
    /// `n88jisho.rom`) -- kept distinct from `Font` since it's a different kind of data
    /// even though both are "Japanese text support" files.
    Kanji,
    /// Co-processor dump (Super Famicom `DSPetc.bin`).
    Dsp,
    /// A real, non-junk `bios/` entry that doesn't fit any of the above.
    Unspecified,
}

/// What kind of system file this is -- independent of size (a `Firmware` file can be a
/// few hundred KB, like NDS/DSi's `Firmware.bin`, or hundreds of MB, like PS3/Vita's
/// `.PUP`; see `docs/plan/bios.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemFileType {
    /// No match by either the hash-lookup or the structural-signature path.
    Unknown,
    /// A boot ROM / BIOS proper -- the default classification for a `biosdb.bin` match
    /// that isn't `Firmware` or `Other`.
    Bios,
    /// A firmware/configuration blob (`Firmware.bin`, `.PUP`) -- see the module doc.
    Firmware,
    /// A NAND/eMMC dump -- never a `biosdb.bin` entry; only ever the outcome of
    /// `nand_dispatch.rs`'s structural signature check.
    Nand,
    /// An SD-card image -- same as `Nand`, structural-signature-only.
    Sdcard,
    /// A known non-BIOS, non-firmware system file (font/kanji/DSP dump).
    Other(OtherFileKind),
}

/// [`identify_system_file`]'s result.
#[derive(Debug, Clone)]
pub struct SystemFileResult {
    pub platform: Option<Platform>,
    pub file_type: SystemFileType,
    /// The canonical/conventional name from `biosdb.bin` (retrobios's own filename,
    /// verbatim), when a hash lookup matched.
    pub name: Option<String>,
    /// Only ever `Some` when a hash lookup actually ran (never on the structural-
    /// signature-only path, and never for `Unknown`/`Mixed`/container-rejected files --
    /// hashing those would be exactly the wasted work this heuristic exists to avoid).
    pub hash_report: Option<HashReport>,
}

impl SystemFileResult {
    fn unknown() -> Self {
        Self { platform: None, file_type: SystemFileType::Unknown, name: None, hash_report: None }
    }
}

// ---------------------------------------------------------------------------
// biosdb.bin format
//
// offset  size  field
// 0       4     magic: b"BIOD"
// 4       1     version: 1
// 5       4     entry_count: u32 LE
// 9       ...   entry_count entries, each:
//               2     platform: u16 LE  -- Platform's own discriminant (`igdb_id()`,
//                                           which for this project's `Platform` enum
//                                           *is* `self as u16` -- see
//                                           `common::models::Platform::igdb_id`), never
//                                           a different provider's id
//               1     file_type: u8     -- 0=Bios, 1=Firmware, 2=Other (Nand/Sdcard/
//                                           Unknown are never generated)
//               1     other_kind: u8    -- only meaningful when file_type==2 (Other):
//                                           0=Font, 1=Kanji, 2=Dsp, 3=Unspecified
//               8     size: u64 LE      -- the entry's real (uncompressed) byte size
//               20    sha1: [u8; 20]    -- primary lookup key (`HashFlags::default()`
//                                           already guarantees every `hash::hash` caller
//                                           computes this, so no extra flags are needed
//                                           for a lookup)
//               1     name_len: u8
//               ..    name: [u8; name_len]  -- UTF-8, retrobios's own filename verbatim
// ---------------------------------------------------------------------------

pub const BIOSDB_MAGIC: &[u8; 4] = b"BIOD";
pub const BIOSDB_VERSION: u8 = 1;

/// The shipped database, compiled in as the last-resort source for [`BiosDb::load`].
/// Lives under `app_root_ex/assets/` for the same reason `fcdb.bin` does (see
/// `convert::nes`'s `EMBEDDED_FCDB`) -- the compiled-in copy and the on-disk layout
/// [`BiosDb::candidate_paths`] searches at runtime are the same file, not two that can
/// drift.
const EMBEDDED_BIOSDB: &[u8] = include_bytes!("../../../app_root_ex/assets/biosdb.bin");

/// Same override this crate's app-root-relative lookups all share -- see
/// `native_lib::EXE_ROOT_OVERRIDE_ENV`'s own doc comment (the canonical definition; this
/// and `convert::nes::FcDb` used to each keep their own separate copy, consolidated to
/// avoid drift).
pub use super::super::native_lib::EXE_ROOT_OVERRIDE_ENV;

#[derive(Debug, Clone)]
struct BiosDbEntry {
    platform: Platform,
    file_type: SystemFileType,
    size: u64,
    sha1: [u8; 20],
    name: String,
}

/// The loaded `biosdb.bin`, indexed for lookup both by hash (the `Bios`-dir case, which
/// isn't narrowed to one platform) and by platform (the `Rom`-dir case).
pub struct BiosDb {
    entries: Vec<BiosDbEntry>,
    by_sha1: HashMap<[u8; 20], usize>,
    by_platform: HashMap<Platform, Vec<usize>>,
}

impl BiosDb {
    /// Mirrors `convert::nes::FcDb::candidate_paths` exactly (same exe-root resolution,
    /// same override env var) -- `<exe_root>/biosdb.bin`, then
    /// `<exe_root>/assets/biosdb.bin`.
    pub fn candidate_paths() -> io::Result<Vec<PathBuf>> {
        // The app root, not the executable's own directory: they differ inside a macOS
        // `.app`, where the executable is in Contents/MacOS/ and assets/ in Contents/Resources/.
        let dir = crate::rom::native_lib::exe_root().ok_or_else(|| io::Error::new(ErrorKind::NotFound, "executable has no parent directory"))?;
        Ok(vec![dir.join("biosdb.bin"), dir.join("assets").join("biosdb.bin")])
    }

    pub fn load() -> io::Result<Self> {
        if let Ok(candidates) = Self::candidate_paths() {
            for path in &candidates {
                if path.is_file() {
                    return Self::load_from(path);
                }
            }
        }
        Self::parse(EMBEDDED_BIOSDB)
    }

    pub fn load_from(path: &Path) -> io::Result<Self> {
        let mut file = fs::File::open(path)?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        Self::parse(&buf)
    }

    fn parse(buf: &[u8]) -> io::Result<Self> {
        let bad = |msg: &str| io::Error::new(ErrorKind::InvalidData, format!("biosdb.bin: {msg}"));

        if buf.len() < 9 {
            return Err(bad("file too short for a header"));
        }
        if &buf[0..4] != BIOSDB_MAGIC {
            return Err(bad("bad magic (not a biosdb.bin file)"));
        }
        let version = buf[4];
        if version != BIOSDB_VERSION {
            return Err(bad(&format!("unsupported version {version} (expected {BIOSDB_VERSION})")));
        }
        let entry_count = u32::from_le_bytes(buf[5..9].try_into().unwrap()) as usize;

        let mut entries = Vec::with_capacity(entry_count);
        let mut pos = 9usize;
        for _ in 0..entry_count {
            let platform_id = u16::from_le_bytes(read_n::<2>(buf, &mut pos, &bad)?);
            let platform = Platform::from_igdb_id(platform_id).unwrap_or(Platform::Unknown);
            let file_type_tag = read_n::<1>(buf, &mut pos, &bad)?[0];
            let other_kind_tag = read_n::<1>(buf, &mut pos, &bad)?[0];
            let file_type = decode_file_type(file_type_tag, other_kind_tag);
            let size = u64::from_le_bytes(read_n::<8>(buf, &mut pos, &bad)?);
            let sha1 = read_n::<20>(buf, &mut pos, &bad)?;
            let name_len = read_n::<1>(buf, &mut pos, &bad)?[0] as usize;
            if pos + name_len > buf.len() {
                return Err(bad("truncated name"));
            }
            let name = String::from_utf8(buf[pos..pos + name_len].to_vec()).map_err(|_| bad("name is not valid UTF-8"))?;
            pos += name_len;
            entries.push(BiosDbEntry { platform, file_type, size, sha1, name });
        }

        let mut by_sha1: HashMap<[u8; 20], usize> = HashMap::with_capacity(entries.len());
        let mut by_platform: HashMap<Platform, Vec<usize>> = HashMap::new();
        for (i, entry) in entries.iter().enumerate() {
            // One file under several systems (MAME carries the Famicom Disk System's
            // `fdsbios.nes` for its own drivers): the console's entry is what the file is.
            // The entries are sorted by platform id, so plain last-wins handed it to Arcade (52)
            // over the NES (18).
            let weak = |p: Platform| p == Platform::Unknown || p.is_arcade_family();
            match by_sha1.get(&entry.sha1) {
                Some(&kept) if !weak(entries[kept].platform) || weak(entry.platform) => {}
                _ => {
                    by_sha1.insert(entry.sha1, i);
                }
            }
            by_platform.entry(entry.platform).or_default().push(i);
        }
        Ok(Self { entries, by_sha1, by_platform })
    }

    fn lookup_by_sha1(&self, sha1: &[u8; 20]) -> Option<&BiosDbEntry> {
        self.by_sha1.get(sha1).map(|&i| &self.entries[i])
    }

    fn lookup_by_sha1_and_platform(&self, sha1: &[u8; 20], platform: Platform) -> Option<&BiosDbEntry> {
        let indices = self.by_platform.get(&platform)?;
        indices.iter().map(|&i| &self.entries[i]).find(|entry| &entry.sha1 == sha1)
    }

    /// Largest entry's size across the whole database, regardless of platform/type -- the
    /// `Bios`-dir ceiling (`MAX_BIOS_SIZE` extended to also cover `Firmware`-class files
    /// legitimately living in the same directory; see `docs/plan/bios.md`). `None` on an
    /// empty database.
    pub fn max_entry_size(&self) -> Option<u64> {
        self.entries.iter().map(|e| e.size).max()
    }
}

fn read_n<const N: usize>(buf: &[u8], pos: &mut usize, bad: &impl Fn(&str) -> io::Error) -> io::Result<[u8; N]> {
    if *pos + N > buf.len() {
        return Err(bad("truncated entry"));
    }
    let out: [u8; N] = buf[*pos..*pos + N].try_into().unwrap();
    *pos += N;
    Ok(out)
}

fn decode_file_type(tag: u8, other_kind_tag: u8) -> SystemFileType {
    match tag {
        1 => SystemFileType::Firmware,
        2 => SystemFileType::Other(match other_kind_tag {
            0 => OtherFileKind::Font,
            1 => OtherFileKind::Kanji,
            2 => OtherFileKind::Dsp,
            _ => OtherFileKind::Unspecified,
        }),
        _ => SystemFileType::Bios,
    }
}

static BIOSDB: OnceLock<Option<BiosDb>> = OnceLock::new();

pub fn shared_biosdb() -> Option<&'static BiosDb> {
    BIOSDB.get_or_init(|| BiosDb::load().ok()).as_ref()
}

// ---------------------------------------------------------------------------
// Detection entry point
// ---------------------------------------------------------------------------

const MARGIN_NUMERATOR: u64 = 125;
const MARGIN_DENOMINATOR: u64 = 100;

/// `ceiling * 1.25`, the margin confirmed for this heuristic -- see `docs/plan/bios.md`.
fn with_margin(ceiling: u64) -> u64 {
    // `u64` headroom: even `MAX_BIOS_SIZE`'s upper neighborhood (low hundreds of MB) is
    // nowhere near overflow territory at this multiplier.
    ceiling.saturating_mul(MARGIN_NUMERATOR) / MARGIN_DENOMINATOR
}

/// Container formats exclusively used for real disc-image games in this codebase --
/// never a system-file candidate. See `docs/plan/bios.md`'s "Container handling".
fn is_disc_image_container(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else { return false };
    matches!(
        archive_format_for_extension(ext),
        Some(crate::common::models::ArchiveFormat::Chd | crate::common::models::ArchiveFormat::Rvz | crate::common::models::ArchiveFormat::Gcz | crate::common::models::ArchiveFormat::Wia)
    )
}

/// Which `Platform`s this heuristic can structurally identify a NAND/SD-card dump for --
/// mirrors `nand_dispatch.rs`'s actual coverage (3DS/New3DS, Switch, DSi, Xbox360). `Wii`
/// is listed in `docs/plan/bios.md`'s NAND-class platform list but has no structural
/// signature implemented in `nand_dispatch.rs` yet (only its SD-card folder markers are)
/// -- not included here until that exists, to avoid claiming a check this codebase
/// doesn't actually perform.
fn is_nand_sd_capable(platform: Platform) -> bool {
    matches!(platform, Platform::Nintendo3ds | Platform::NewNintendo3ds | Platform::NintendoSwitch | Platform::NintendoSwitch2 | Platform::NintendoDsi | Platform::Xbox360)
}

/// The per-platform `MAX_BIOS_SIZE_<PLATFORM>` ceiling, `None` for a platform with no
/// documented simple-system-file ceiling in `docs/plan/bios.md` (e.g. PlayStation3/
/// PlayStationVita -- their `.PUP` firmware is only reachable via the `Bios`-dir branch,
/// not this per-platform one; see that doc's "Rom dir" scope note).
fn max_bios_size_for_platform(platform: Platform) -> Option<u64> {
    use Platform::*;
    Some(match platform {
        Nintendo3ds | NewNintendo3ds => MAX_BIOS_SIZE_NINTENDO_3DS,
        Nintendo64Dd => MAX_BIOS_SIZE_NINTENDO_64_DD,
        Dreamcast => MAX_BIOS_SIZE_DREAMCAST,
        FamicomDiskSystem => MAX_BIOS_SIZE_FAMICOM_DISK_SYSTEM,
        GameBoy => MAX_BIOS_SIZE_GAME_BOY,
        GameBoyColor => MAX_BIOS_SIZE_GAME_BOY_COLOR,
        GameBoyAdvance => MAX_BIOS_SIZE_GAME_BOY_ADVANCE,
        SegaGenesis => MAX_BIOS_SIZE_SEGA_GENESIS,
        SegaCd => MAX_BIOS_SIZE_SEGA_CD,
        NintendoDs => MAX_BIOS_SIZE_NINTENDO_DS,
        NeoGeoCd => MAX_BIOS_SIZE_NEO_GEO_CD,
        NeoGeoPocket => MAX_BIOS_SIZE_NEO_GEO_POCKET,
        NeoGeoPocketColor => MAX_BIOS_SIZE_NEO_GEO_POCKET_COLOR,
        GameCube => MAX_BIOS_SIZE_GAME_CUBE,
        NintendoDsi => MAX_BIOS_SIZE_NINTENDO_DSI_SMALL,
        PocketStation => MAX_BIOS_SIZE_POCKET_STATION,
        PlayStation => MAX_BIOS_SIZE_PLAY_STATION,
        PlayStation2 => MAX_BIOS_SIZE_PLAY_STATION_2,
        SegaSaturn => MAX_BIOS_SIZE_SEGA_SATURN,
        Sega32x => MAX_BIOS_SIZE_SEGA_32X,
        SuperFamicom => MAX_BIOS_SIZE_SUPER_FAMICOM,
        SufamiTurbo => MAX_BIOS_SIZE_SUFAMI_TURBO,
        SegaMasterSystem => MAX_BIOS_SIZE_SEGA_MASTER_SYSTEM,
        TurboGrafxCd => MAX_BIOS_SIZE_TURBO_GRAFX_CD,
        WonderSwan => MAX_BIOS_SIZE_WONDER_SWAN,
        WonderSwanColor => MAX_BIOS_SIZE_WONDER_SWAN_COLOR,
        Xbox => MAX_BIOS_SIZE_XBOX,
        NintendoSwitch | NintendoSwitch2 => MAX_BIOS_SIZE_NINTENDO_SWITCH_SMALL,
        SegaGameGear => MAX_BIOS_SIZE_SEGA_GAME_GEAR,
        Msx => MAX_BIOS_SIZE_MSX,
        Msx2 => MAX_BIOS_SIZE_MSX_2,
        Msx2Plus => MAX_BIOS_SIZE_MSX_2_PLUS,
        Atari5200 => MAX_BIOS_SIZE_ATARI_5200,
        Atari7800 => MAX_BIOS_SIZE_ATARI_7800,
        AtariJaguar => MAX_BIOS_SIZE_ATARI_JAGUAR,
        AtariLynx => MAX_BIOS_SIZE_ATARI_LYNX,
        AtariStSte => MAX_BIOS_SIZE_ATARI_ST_STE,
        Atari8Bit => MAX_BIOS_SIZE_ATARI_8_BIT,
        SharpX68000 => MAX_BIOS_SIZE_SHARP_X68000,
        FmTowns => MAX_BIOS_SIZE_FM_TOWNS,
        Pc8801 => MAX_BIOS_SIZE_PC_8801,
        Pc98 => MAX_BIOS_SIZE_PC_98,
        PcFx => MAX_BIOS_SIZE_PC_FX,
        ThreeDo => MAX_BIOS_SIZE_THREE_DO,
        PokemonMini => MAX_BIOS_SIZE_POKEMON_MINI,
        NeoGeoMvs => MAX_BIOS_SIZE_NEO_GEO_MVS,
        NeoGeoAes => MAX_BIOS_SIZE_NEO_GEO_AES,
        // No documented simple-system-file ceiling (PS3/Vita's firmware is Bios-dir-only;
        // TurboGrafx16 has no BIOS/system file at all -- confirmed this session; every
        // other platform genuinely has none documented in docs/plan/bios.md).
        _ => return None,
    })
}

fn sha1_report(report: &HashReport) -> Option<[u8; 20]> {
    let hex = report.sha1.as_deref()?;
    let mut out = [0u8; 20];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Real file size on disk. **Known scope gap**: for a zip/7z archive *member* (not the
/// archive file itself), `docs/plan/bios.md`'s design calls for the member's extracted
/// size, not the archive's on-disk compressed size -- that requires per-member hashing
/// (reading just that member's bytes, not the whole archive), which this pass doesn't
/// wire up. `path` here is always treated as a plain file; archive-member support is not
/// implemented this pass.
fn file_size(path: &Path) -> io::Result<u64> {
    Ok(fs::metadata(path)?.len())
}

/// See the module doc for the overall design; see `docs/plan/bios.md` for the full
/// rationale. Never panics -- any I/O failure (unreadable file, no `biosdb.bin` loaded)
/// falls through to `Unknown`, the same as a genuine no-match.
pub fn identify_system_file(path: &Path, dir_type: DirType, platform: Option<Platform>) -> SystemFileResult {
    if dir_type == DirType::Mixed {
        return SystemFileResult::unknown();
    }
    if is_disc_image_container(path) {
        return SystemFileResult::unknown();
    }
    let Ok(size) = file_size(path) else { return SystemFileResult::unknown() };

    match dir_type {
        DirType::Mixed => unreachable!("handled above"),
        DirType::Bios => identify_in_bios_dir(path, size, platform),
        DirType::Rom => match platform {
            Some(platform) => identify_in_rom_dir(path, size, platform),
            None => SystemFileResult::unknown(),
        },
    }
}

fn identify_in_bios_dir(path: &Path, size: u64, platform_hint: Option<Platform>) -> SystemFileResult {
    let Some(db) = shared_biosdb() else { return SystemFileResult::unknown() };
    let ceiling = with_margin(db.max_entry_size().unwrap_or(MAX_BIOS_SIZE).max(MAX_BIOS_SIZE));
    if size > ceiling {
        return signature_check_or_unknown(path, platform_hint);
    }

    let Ok(report) = hash(path, &HashOptions::default(), |_| {}) else { return SystemFileResult::unknown() };
    let Some(sha1) = sha1_report(&report) else { return SystemFileResult::unknown() };
    match db.lookup_by_sha1(&sha1) {
        Some(entry) => SystemFileResult { platform: Some(entry.platform), file_type: entry.file_type, name: Some(entry.name.clone()), hash_report: Some(report) },
        None => SystemFileResult { platform: None, file_type: SystemFileType::Unknown, name: None, hash_report: Some(report) },
    }
}

fn identify_in_rom_dir(path: &Path, size: u64, platform: Platform) -> SystemFileResult {
    let Some(ceiling) = max_bios_size_for_platform(platform) else { return signature_check_or_unknown(path, Some(platform)) };
    let ceiling = with_margin(ceiling);
    if size > ceiling {
        return signature_check_or_unknown(path, Some(platform));
    }

    let Some(db) = shared_biosdb() else { return SystemFileResult::unknown() };
    let Ok(report) = hash(path, &HashOptions::default(), |_| {}) else { return SystemFileResult::unknown() };
    let Some(sha1) = sha1_report(&report) else { return SystemFileResult::unknown() };
    match db.lookup_by_sha1_and_platform(&sha1, platform) {
        Some(entry) => SystemFileResult { platform: Some(entry.platform), file_type: entry.file_type, name: Some(entry.name.clone()), hash_report: Some(report) },
        None => SystemFileResult { platform: None, file_type: SystemFileType::Unknown, name: None, hash_report: Some(report) },
    }
}

/// Oversized for any hash-lookup: try the NAND/SD structural signature check
/// (`nand_dispatch.rs`, already implemented) if the platform context supports it,
/// `Unknown` otherwise. No `HashReport` on this path -- a hash was never computed.
fn signature_check_or_unknown(path: &Path, platform_hint: Option<Platform>) -> SystemFileResult {
    let Some(platform) = platform_hint else { return SystemFileResult::unknown() };
    if !is_nand_sd_capable(platform) {
        return SystemFileResult::unknown();
    }

    let matched = match platform {
        Platform::Nintendo3ds | Platform::NewNintendo3ds => super::super::platforms::nintendo::is_3ds_nand_dump(path).unwrap_or(false),
        Platform::NintendoSwitch | Platform::NintendoSwitch2 => super::nand_dispatch::switch_nand_gpt_signature(path).unwrap_or(false),
        Platform::NintendoDsi => super::nand_dispatch::dsi_nand_footer_signature(path).unwrap_or(false),
        Platform::Xbox360 => super::nand_dispatch::xbox360_nand_signature(path).unwrap_or(false),
        _ => false,
    };
    if !matched {
        return SystemFileResult::unknown();
    }

    let file_type = if matches!(platform, Platform::NintendoDsi) && nand_dispatch::is_likely_dsi_sd_card_image(path).unwrap_or(false) {
        SystemFileType::Sdcard
    } else {
        SystemFileType::Nand
    };
    SystemFileResult { platform: Some(platform), file_type, name: None, hash_report: None }
}

use super::nand_dispatch;

// ---------------------------------------------------------------------------
// Constants (moved here from docs/plan/bios.md's own "Constants" section -- see that
// doc for the full per-platform research/sourcing behind each figure). All values in
// bytes.
// ---------------------------------------------------------------------------

/// Global ceiling: PS2's Bios.bin (uniformly 4MB across every revision, retrobios-
/// confirmed). Anything above this is NAND/SD-card/firmware-package class, not a simple
/// BIOS candidate -- no meaningful upper bound exists for that class, so there is no
/// `MAX_NAND_SIZE`. (The `Bios`-dir branch of `identify_system_file` extends past this
/// for `Firmware`-class files specifically -- see `identify_in_bios_dir`.)
pub const MAX_BIOS_SIZE: u64 = 4 * 1024 * 1024;

// Per-platform max, name matches the common::models::Platform variant it corresponds to.
pub const MAX_BIOS_SIZE_NINTENDO_3DS: u64 = 3 * 1024 * 1024; // shared_font.bin
pub const MAX_BIOS_SIZE_NEW_NINTENDO_3DS: u64 = MAX_BIOS_SIZE_NINTENDO_3DS;
pub const MAX_BIOS_SIZE_NINTENDO_64_DD: u64 = 4 * 1024 * 1024; // IPL.z64
pub const MAX_BIOS_SIZE_DREAMCAST: u64 = 2 * 1024 * 1024; // Boot.bin/Bios.bin
pub const MAX_BIOS_SIZE_FAMICOM_DISK_SYSTEM: u64 = 8 * 1024; // DiskSystem.rom
pub const MAX_BIOS_SIZE_GAME_BOY: u64 = 256; // BootRom.gb
pub const MAX_BIOS_SIZE_GAME_BOY_COLOR: u64 = 512 * 1024; // Touch Boy/Genius variant
pub const MAX_BIOS_SIZE_GAME_BOY_ADVANCE: u64 = 16 * 1024; // Bios.gba
pub const MAX_BIOS_SIZE_SEGA_GENESIS: u64 = 128 * 1024; // WonderMega/M2
pub const MAX_BIOS_SIZE_SEGA_CD: u64 = 128 * 1024; // Bios.md
pub const MAX_BIOS_SIZE_NINTENDO_DS: u64 = 512 * 1024; // Firmware.bin
pub const MAX_BIOS_SIZE_NEO_GEO_CD: u64 = 512 * 1024;
pub const MAX_BIOS_SIZE_NEO_GEO_POCKET: u64 = 64 * 1024;
pub const MAX_BIOS_SIZE_NEO_GEO_POCKET_COLOR: u64 = 64 * 1024;
pub const MAX_BIOS_SIZE_GAME_CUBE: u64 = 2 * 1024 * 1024; // IPL.bin
pub const MAX_BIOS_SIZE_NINTENDO_DSI_SMALL: u64 = 128 * 1024; // firmware.bin; NAND/Sdcard excluded, see above
pub const MAX_BIOS_SIZE_POCKET_STATION: u64 = 16 * 1024;
pub const MAX_BIOS_SIZE_PLAY_STATION: u64 = 512 * 1024;
pub const MAX_BIOS_SIZE_PLAY_STATION_2: u64 = 4 * 1024 * 1024; // == MAX_BIOS_SIZE
pub const MAX_BIOS_SIZE_SEGA_SATURN: u64 = 512 * 1024;
pub const MAX_BIOS_SIZE_SEGA_32X: u64 = 2 * 1024; // corrected -- was mistakenly MB-scale
pub const MAX_BIOS_SIZE_SUPER_FAMICOM: u64 = 160 * 1024; // DSPetc.bin upper bound
pub const MAX_BIOS_SIZE_SUFAMI_TURBO: u64 = 256 * 1024;
pub const MAX_BIOS_SIZE_SEGA_MASTER_SYSTEM: u64 = 8 * 1024; // standard; SMS_kr.sms (128KB) is an outlier
pub const MAX_BIOS_SIZE_TURBO_GRAFX_CD: u64 = 256 * 1024; // SystemCard.pce
pub const MAX_BIOS_SIZE_WONDER_SWAN: u64 = 4 * 1024;
pub const MAX_BIOS_SIZE_WONDER_SWAN_COLOR: u64 = 8 * 1024;
pub const MAX_BIOS_SIZE_XBOX: u64 = 1024 * 1024;
pub const MAX_BIOS_SIZE_NINTENDO_SWITCH_SMALL: u64 = 16 * 1024; // key files; firmware/NAND excluded, see above; title.keys is variable-length, not bounded here
pub const MAX_BIOS_SIZE_SEGA_GAME_GEAR: u64 = 1024;
pub const MAX_BIOS_SIZE_MSX: u64 = 128 * 1024; // KANJI.rom
pub const MAX_BIOS_SIZE_MSX_2: u64 = 64 * 1024; // MSXDOS2.rom
pub const MAX_BIOS_SIZE_MSX_2_PLUS: u64 = 32 * 1024;

// Added platforms (researched, see docs/plan/bios.md's "Platforms added").
pub const MAX_BIOS_SIZE_ATARI_5200: u64 = 2 * 1024;
pub const MAX_BIOS_SIZE_ATARI_7800: u64 = 16 * 1024; // PAL variant
pub const MAX_BIOS_SIZE_ATARI_JAGUAR: u64 = 256 * 1024; // boot1.rom
pub const MAX_BIOS_SIZE_ATARI_LYNX: u64 = 512;
pub const MAX_BIOS_SIZE_ATARI_ST_STE: u64 = 1024 * 1024; // EmuTOS upper bound; original TOS is 512KB
pub const MAX_BIOS_SIZE_ATARI_8_BIT: u64 = 32 * 1024;
pub const MAX_BIOS_SIZE_SHARP_X68000: u64 = 768 * 1024; // cgrom.dat
pub const MAX_BIOS_SIZE_FM_TOWNS: u64 = 512 * 1024; // per individual file; FMT_ALL.ROM combined variant is ~4MB
pub const MAX_BIOS_SIZE_PC_8801: u64 = 384 * 1024;
pub const MAX_BIOS_SIZE_PC_98: u64 = 512 * 1024;
pub const MAX_BIOS_SIZE_PC_FX: u64 = 1024 * 1024;
pub const MAX_BIOS_SIZE_THREE_DO: u64 = 1024 * 1024;
pub const MAX_BIOS_SIZE_POKEMON_MINI: u64 = 4 * 1024;
pub const MAX_BIOS_SIZE_NEO_GEO_MVS: u64 = 512 * 1024; // P-ROM; sound BIOS chip is 128KB
pub const MAX_BIOS_SIZE_NEO_GEO_AES: u64 = MAX_BIOS_SIZE_NEO_GEO_MVS;

#[cfg(test)]
mod tests {
    use super::*;

    fn write_entry(buf: &mut Vec<u8>, platform: Platform, file_type_tag: u8, other_kind_tag: u8, size: u64, sha1: [u8; 20], name: &str) {
        buf.extend_from_slice(&platform.igdb_id().to_le_bytes());
        buf.push(file_type_tag);
        buf.push(other_kind_tag);
        buf.extend_from_slice(&size.to_le_bytes());
        buf.extend_from_slice(&sha1);
        buf.push(name.len() as u8);
        buf.extend_from_slice(name.as_bytes());
    }

    fn synth_db(entries: &[(Platform, u8, u8, u64, [u8; 20], &str)]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(BIOSDB_MAGIC);
        buf.push(BIOSDB_VERSION);
        buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for (platform, ft, ok, size, sha1, name) in entries {
            write_entry(&mut buf, *platform, *ft, *ok, *size, *sha1, name);
        }
        buf
    }

    #[test]
    fn parses_empty_database() {
        let buf = synth_db(&[]);
        let db = BiosDb::parse(&buf).expect("parses");
        assert_eq!(db.entries.len(), 0);
        assert_eq!(db.max_entry_size(), None);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut buf = synth_db(&[]);
        buf[0] = b'X';
        assert!(BiosDb::parse(&buf).is_err());
    }

    #[test]
    fn rejects_unsupported_version() {
        let mut buf = synth_db(&[]);
        buf[4] = 99;
        assert!(BiosDb::parse(&buf).is_err());
    }

    #[test]
    fn looks_up_by_sha1_and_platform() {
        let sha1 = [0xAAu8; 20];
        let buf = synth_db(&[(Platform::PlayStation, 0, 0, 524288, sha1, "scph5501.bin")]);
        let db = BiosDb::parse(&buf).expect("parses");
        let found = db.lookup_by_sha1(&sha1).expect("found");
        assert_eq!(found.platform, Platform::PlayStation);
        assert_eq!(found.name, "scph5501.bin");
        assert!(db.lookup_by_sha1_and_platform(&sha1, Platform::PlayStation2).is_none());
        assert!(db.lookup_by_sha1_and_platform(&sha1, Platform::PlayStation).is_some());
    }

    /// One file under a console and under MAME: the console's entry is what a lookup gives,
    /// whichever comes later in the file (it is sorted by platform id, Arcade's 52 after the
    /// NES's 18).
    #[test]
    fn a_file_shared_with_mame_is_the_consoles() {
        let sha1 = [0xFDu8; 20];
        let buf = synth_db(&[(Platform::Nes, 0, 0, 8192, sha1, "fdsbios.nes"), (Platform::Arcade, 0, 0, 8192, sha1, "fdsbios.nes")]);
        let db = BiosDb::parse(&buf).expect("parses");
        assert_eq!(db.lookup_by_sha1(&sha1).unwrap().platform, Platform::Nes);
        let only_arcade = [0x52u8; 20];
        let buf = synth_db(&[(Platform::Arcade, 0, 0, 8192, only_arcade, "neogeo.zip")]);
        assert_eq!(BiosDb::parse(&buf).unwrap().lookup_by_sha1(&only_arcade).unwrap().platform, Platform::Arcade, "a file only MAME has stays Arcade");
    }

    #[test]
    fn firmware_type_independent_of_size() {
        // NDS's small Firmware.bin and a hypothetically huge one both decode as Firmware
        // -- the tag alone drives classification, not size (the bug the user flagged).
        let small = [0x01u8; 20];
        let huge = [0x02u8; 20];
        let buf = synth_db(&[
            (Platform::NintendoDs, 1, 0, 512 * 1024, small, "firmware.bin"),
            (Platform::PlayStation3, 1, 0, 206_126_236, huge, "PS3UPDAT.PUP"),
        ]);
        let db = BiosDb::parse(&buf).expect("parses");
        assert_eq!(db.lookup_by_sha1(&small).unwrap().file_type, SystemFileType::Firmware);
        assert_eq!(db.lookup_by_sha1(&huge).unwrap().file_type, SystemFileType::Firmware);
    }

    #[test]
    fn other_kind_round_trips() {
        let sha1 = [0x03u8; 20];
        let buf = synth_db(&[(Platform::Msx, 2, 1, 128 * 1024, sha1, "kanji.rom")]);
        let db = BiosDb::parse(&buf).expect("parses");
        assert_eq!(db.lookup_by_sha1(&sha1).unwrap().file_type, SystemFileType::Other(OtherFileKind::Kanji));
    }

    #[test]
    fn max_entry_size_is_the_largest_regardless_of_type() {
        let buf = synth_db(&[
            (Platform::NintendoDs, 1, 0, 512 * 1024, [1u8; 20], "firmware.bin"),
            (Platform::PlayStation3, 1, 0, 206_126_236, [2u8; 20], "PS3UPDAT.PUP"),
            (Platform::PlayStation, 0, 0, 524288, [3u8; 20], "scph5501.bin"),
        ]);
        let db = BiosDb::parse(&buf).expect("parses");
        assert_eq!(db.max_entry_size(), Some(206_126_236));
    }

    #[test]
    fn mixed_dir_type_is_always_unknown() {
        // No biosdb needed at all -- Mixed short-circuits before any lookup.
        let result = identify_system_file(Path::new("/nonexistent/Bios.bin"), DirType::Mixed, None);
        assert_eq!(result.file_type, SystemFileType::Unknown);
        assert!(result.hash_report.is_none());
    }

    #[test]
    fn chd_rvz_gcz_wia_are_auto_rejected() {
        for ext in ["chd", "rvz", "gcz", "wia"] {
            let path = PathBuf::from(format!("/nonexistent/game.{ext}"));
            let result = identify_system_file(&path, DirType::Bios, None);
            assert_eq!(result.file_type, SystemFileType::Unknown, "{ext} should auto-reject");
        }
    }

    #[test]
    fn rom_dir_without_platform_is_unknown() {
        let result = identify_system_file(Path::new("/nonexistent/Bios.bin"), DirType::Rom, None);
        assert_eq!(result.file_type, SystemFileType::Unknown);
    }

    #[test]
    fn with_margin_applies_25_percent() {
        assert_eq!(with_margin(MAX_BIOS_SIZE), MAX_BIOS_SIZE * 5 / 4);
        assert_eq!(with_margin(MAX_BIOS_SIZE_MSX), MAX_BIOS_SIZE_MSX * 5 / 4);
    }

    #[test]
    fn platform_without_documented_ceiling_falls_back_to_signature_path() {
        // PlayStation3 has no MAX_BIOS_SIZE_<PLATFORM> constant (its firmware is
        // Bios-dir-only, per docs/plan/bios.md) -- and it isn't NAND/SD-capable either,
        // so the Rom-dir branch must land on Unknown, never panic or wrongly hash-check.
        assert_eq!(max_bios_size_for_platform(Platform::PlayStation3), None);
        assert!(!is_nand_sd_capable(Platform::PlayStation3));
    }

    #[test]
    fn nand_sd_capable_platforms_match_nand_dispatch_coverage() {
        assert!(is_nand_sd_capable(Platform::Nintendo3ds));
        assert!(is_nand_sd_capable(Platform::NintendoSwitch));
        assert!(is_nand_sd_capable(Platform::NintendoDsi));
        assert!(is_nand_sd_capable(Platform::Xbox360));
        assert!(!is_nand_sd_capable(Platform::PlayStation2));
    }
}
