//! Copying ROMs out of a collection: what to copy, where to put it, and what to convert it
//! to on the way.
//!
//! Four directions are designed; two are implemented (see [`CopyMode`]). They are one job
//! with a mode field rather than four job types, because the direction only picks three
//! policies -- whether to skip what this machine already holds, whether to register the
//! result, and which holder to prefer -- and shares everything else.

use serde::{Deserialize, Serialize};

/// A row in an *ownership* table, qualified by the machine that owns it. `source_id: None`
/// is this machine's own `local_*` tables; `Some(id)` is a `remote_sources` row.
///
/// The qualifier is not optional decoration. `migrations/0020_remote_mirror.sql` states
/// plainly that the mirror copies each source's ids verbatim and that **two sources will
/// collide on id**, so a bare `u64` for a game, rom or file is ambiguous the moment a
/// second source is configured -- and ambiguous in the silent way, resolving to whatever
/// unrelated row happens to hold that id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OwnedRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<u16>,
    pub id: u64,
}

impl OwnedRef {
    pub fn local(id: u64) -> Self {
        Self { source_id: None, id }
    }
}

/// One row of the Titles list: a title and its `row_key` (`retail`, `demo`, `hack:1234`, ...).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TitleRowRef {
    pub title_id: u64,
    pub row_key: String,
}

/// What the user picked, at whichever of the six levels they picked it.
///
/// Catalog ids (`set`/`title`/`release`) are shared across every machine and so are bare;
/// ownership ids are [`OwnedRef`]s. Any mix is legal -- a selection is resolved level by
/// level down to files and then deduplicated, so naming a release and one of its files twice
/// copies it once.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopySelection {
    #[serde(default)]
    pub set_ids: Vec<u64>,
    #[serde(default)]
    pub title_ids: Vec<u64>,
    /// Rows of the Titles list: one title's releases of one kind (its retail releases, its
    /// demos, one hack). Only that row's releases are taken -- never another kind's when the
    /// row has none.
    #[serde(default)]
    pub title_rows: Vec<TitleRowRef>,
    /// The same rows by the number the Titles list gives each (`TitleRow::row_id`).
    #[serde(default)]
    pub title_row_ids: Vec<u64>,
    #[serde(default)]
    pub release_ids: Vec<u64>,
    #[serde(default)]
    pub game_ids: Vec<OwnedRef>,
    #[serde(default)]
    pub rom_ids: Vec<OwnedRef>,
    #[serde(default)]
    pub file_ids: Vec<OwnedRef>,
}

impl CopySelection {
    pub fn is_empty(&self) -> bool {
        self.set_ids.is_empty() && self.title_ids.is_empty() && self.title_rows.is_empty() && self.title_row_ids.is_empty() && self.release_ids.is_empty() && self.game_ids.is_empty() && self.rom_ids.is_empty() && self.file_ids.is_empty()
    }

    pub fn len(&self) -> usize {
        self.set_ids.len() + self.title_ids.len() + self.title_rows.len() + self.title_row_ids.len() + self.release_ids.len() + self.game_ids.len() + self.rom_ids.len() + self.file_ids.len()
    }
}

/// Which direction a copy runs. Everything else about a copy is shared; the direction picks
/// where it reads, where it writes, and how careful it has to be about what is already there.
///
/// The care is the interesting part. A copy into the **authoritative** collection -- the
/// server's own, or a client's upload into it -- never overwrites: an existing file is left
/// alone and reported as a failure, because silently replacing the master copy of something
/// is not a thing a background job should be able to do. A copy into a **working** location
/// -- this machine's own collection, an SD card -- may overwrite, with the user having said
/// so per file in the dialog beforehand.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyMode {
    /// Into a configured collection directory on this machine, usually from a mounted
    /// server. Skips content this machine already holds, and the copied files are registered
    /// so they immediately count as held here.
    #[default]
    ToCollection,
    /// Onto an SD card, a phone or a USB stick: an arbitrary folder that is not part of any
    /// collection. Nothing is written to the database -- the point of a card is a second
    /// copy, so "you already have this" is not a reason to skip it.
    ToExternal,
    /// Within this machine's own collection, which is mostly how a conversion is done: read
    /// the ISOs, write the RVZs, keep both registered. Nothing is skipped for being held --
    /// a second copy in another format is the whole point -- and an existing destination is
    /// the user's to overwrite or skip, decided per file before the job starts.
    InLocal,
    /// Within the server's own collection, run from the server's own UI. Mechanically
    /// `InLocal`, with two rules it cannot waive: every file is **verified**, and an existing
    /// destination is **never** overwritten -- it is skipped and reported as a failure while
    /// the rest continue. This is the master copy of the collection.
    InServer,
    /// This machine's files up to the server, through the mount, for content the server does
    /// not have. Never overwrites. The server rehashes what arrived itself -- the client asks
    /// it for a `file_scan { new_only }` rather than claiming to know what it just sent.
    Upload,
}

impl CopyMode {
    pub const ALL: [CopyMode; 5] = [CopyMode::ToCollection, CopyMode::ToExternal, CopyMode::InLocal, CopyMode::InServer, CopyMode::Upload];

    pub fn name(self) -> &'static str {
        match self {
            Self::ToCollection => "to_collection",
            Self::ToExternal => "to_external",
            Self::InLocal => "in_local",
            Self::InServer => "in_server",
            Self::Upload => "upload",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == name)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ToCollection => "into my collection",
            Self::ToExternal => "to external media",
            Self::InLocal => "within my collection (convert in place)",
            Self::InServer => "within the server's collection",
            Self::Upload => "up to the server",
        }
    }

    /// Whether an existing destination is something to *ask* about rather than to resolve by
    /// policy.
    ///
    /// Only `InLocal`, and the asymmetry is deliberate. A copy out to a card or in from a
    /// server writes into a folder the user pointed at for that purpose, and `on_conflict`
    /// has always governed it. `InLocal` writes into the collection itself, where the file in
    /// the way is very likely one the app put there and something still points at -- so it
    /// gets a per-file decision, and an unanswered one skips. Never destroy a file nobody
    /// said to destroy.
    pub fn asks_about_conflicts(self) -> bool {
        matches!(self, Self::InLocal)
    }

    /// The one line under each mode's radio: what it does that the others do not.
    pub fn hint(self) -> &'static str {
        match self {
            Self::ToCollection => "from a mounted server into a collection folder here. Skips what this machine already holds, and records what lands so it counts as held.",
            Self::ToExternal => "an SD card, a phone, a USB stick. Nothing is written to the database, and nothing is skipped for being held here -- a second copy is the point.",
            Self::InLocal => "read from one folder here, write to another -- mostly how a conversion is done. Both stay registered. You decide what happens to any file already at the destination.",
            Self::InServer => "the server rearranging its own collection. Every file is checked, and nothing already there is ever overwritten: it is skipped and reported while the rest carry on.",
            Self::Upload => "your files up to the server, through the mount, for content it doesn't have. Never overwrites. The server hashes what arrived itself.",
        }
    }

    /// Whether content this machine already holds is skipped rather than copied again.
    /// Not for `InLocal`/`InServer`: a second copy in another format is what those are for.
    pub fn skips_local_holdings(self) -> bool {
        matches!(self, Self::ToCollection)
    }

    /// Whether content the *target server* already holds is skipped -- an upload is for what
    /// is not up there yet.
    pub fn skips_source_holdings(self) -> bool {
        matches!(self, Self::Upload)
    }

    /// Whether the copied files become rows in **this machine's** ownership tables. An upload
    /// does not: those files are the server's, and the server's own scan records them.
    pub fn registers(self) -> bool {
        matches!(self, Self::ToCollection | Self::InLocal | Self::InServer)
    }

    /// Whether every file is read back and checked, with no way to turn it off.
    pub fn mandatory_verify(self) -> bool {
        matches!(self, Self::InServer)
    }

    /// Whether an existing destination is left alone and reported as a failure, rather than
    /// being subject to `on_conflict`.
    pub fn never_overwrites(self) -> bool {
        matches!(self, Self::InServer | Self::Upload)
    }

    /// Whether both ends are on this machine, so no mount is involved.
    pub fn is_local_to_local(self) -> bool {
        matches!(self, Self::InLocal | Self::InServer)
    }
}

/// What to do about one destination that already exists, chosen in the dialog before the job
/// starts.
///
/// A job runs on the worker with nobody attached to answer a prompt, so the "skip / rename /
/// overwrite, for this file or for all" decision is made when Copy is pressed, against a
/// plan that lists every collision, and travels in the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictChoice {
    Overwrite,
    Skip,
    /// Keep the file there and write this one beside it, ` (2)` before the extension.
    Rename,
}

/// Which holder to copy from when more than one has the release. Completeness is weighed
/// first regardless -- see `api::copy`; this only breaks the remaining tie.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourcePreference {
    /// This machine's own files, then the main server, then peers. Reading a mount is slower
    /// than reading a local disk and can fail outright.
    #[default]
    LocalFirst,
    /// The main server first -- what you want when the local copy is the one you suspect.
    MainFirst,
    /// Only this source.
    Source(u16),
}

/// What to do when a destination path is taken by a *different* file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    /// ` (2)`, ` (3)` before the extension. Nothing is lost and the summary lists every
    /// rename, so a collision is visible rather than silent.
    Rename,
    /// The default: a file already there is never touched unless the user says so.
    #[default]
    Skip,
    Overwrite,
    /// Abort the whole job on the first collision.
    Fail,
}

impl ConflictPolicy {
    pub const ALL: [ConflictPolicy; 4] = [ConflictPolicy::Rename, ConflictPolicy::Skip, ConflictPolicy::Overwrite, ConflictPolicy::Fail];

    pub fn name(self) -> &'static str {
        match self {
            Self::Rename => "rename",
            Self::Skip => "skip",
            Self::Overwrite => "overwrite",
            Self::Fail => "fail",
        }
    }
}

/// How a destination subdirectory is named, per platform.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderScheme {
    /// `Nintendo - Super Nintendo Entertainment System` -- the No-Intro / Redump / TOSEC
    /// spelling, which is also how RetroArch's own database names its systems.
    Dat,
    /// `Super Nintendo Entertainment System` -- the platform's display name, no vendor.
    Display,
    /// `snes` -- the IGDB platform slug, which is what RomM matches library folders against.
    Slug,
    /// `snes` -- the EmulationStation / RetroArch short folder name. Differs from the IGDB
    /// slug more often than it looks (`megadrive` vs `genesis`, `pcengine` vs `tg16`).
    #[default]
    Es,
    /// Whatever the user typed per platform, falling back to `Es` where they typed nothing.
    Custom,
}

impl FolderScheme {
    pub const ALL: [FolderScheme; 5] = [FolderScheme::Dat, FolderScheme::Display, FolderScheme::Slug, FolderScheme::Es, FolderScheme::Custom];

    pub fn name(self) -> &'static str {
        match self {
            Self::Dat => "dat",
            Self::Display => "display",
            Self::Slug => "slug",
            Self::Es => "es",
            Self::Custom => "custom",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Dat => "pack name with manufacturer (No-Intro / Redump / RetroArch)",
            Self::Display => "display name, no manufacturer",
            Self::Slug => "IGDB slug (RomM)",
            Self::Es => "short name (EmulationStation / RetroArch)",
            Self::Custom => "custom",
        }
    }
}

/// What a file is written as at the destination.
///
/// One enum per platform rather than a set of booleans: `rom::models::ArchiveArgs` forbids
/// combining a file archive with a disc container, so a single choice is the only shape that
/// cannot express an invalid conversion.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    /// Copy the bytes as they are. Always available, always the default.
    #[default]
    Same,
    /// Unpack an archive into loose files.
    Raw,
    Zip,
    #[serde(rename = "7z")]
    SevenZip,
    Chd,
    Rvz,
    Gcz,
    Wia,
    /// Cemu's Wii U archive, from a `.wud`/`.wux` and its key. One way: a `.wua` keeps the
    /// decrypted files and nothing of the disc.
    Wua,
    /// Xenia's archive of an Xbox 360 disc's game files. One way, like `.wua`: the video
    /// partition and the disc's layout are gone.
    Zar,
    /// An Xbox disc's game partition alone, rewritten by extract-xiso. Named `.iso`; the
    /// video partition is dropped, so it is one way too.
    Xiso,
    /// A PS2 or PSP ISO in compressed blocks. Lossless: the ISO comes back byte for byte.
    Cso,
    /// A 3DS game as a decrypted cartridge image (`.cci`). These five are 3DS titles
    /// decrypted with the user's own keys (`rom::n3ds`); an update, DLC, demo or system title
    /// can't be a cartridge image, so under any of them it stays a decrypted `.cia` (`.zcia`
    /// for the compressed ones), and homebrew stays `.3dsx` (`.z3dsx`).
    Cci,
    /// The same image named `.3ds`.
    #[serde(rename = "3ds")]
    ThreeDs,
    /// The image in Azahar's compressed format.
    Zcci,
    /// The game's main part (NCCH) alone, decrypted: no manual or download-play parts.
    Cxi,
    /// That, compressed for Azahar.
    Zcxi,
}

impl OutputFormat {
    pub const ALL: [OutputFormat; 17] = [
        OutputFormat::Same,
        OutputFormat::Raw,
        OutputFormat::Zip,
        OutputFormat::SevenZip,
        OutputFormat::Chd,
        OutputFormat::Rvz,
        OutputFormat::Gcz,
        OutputFormat::Wia,
        OutputFormat::Wua,
        OutputFormat::Zar,
        OutputFormat::Xiso,
        OutputFormat::Cso,
        OutputFormat::Cci,
        OutputFormat::ThreeDs,
        OutputFormat::Zcci,
        OutputFormat::Cxi,
        OutputFormat::Zcxi,
    ];

    /// The five 3DS formats: a title decrypted (and maybe compressed), not re-containered.
    pub fn is_3ds(self) -> bool {
        matches!(self, Self::Cci | Self::ThreeDs | Self::Zcci | Self::Cxi | Self::Zcxi)
    }

    /// True for the formats that keep a disc's files but not the disc: what comes out can't
    /// be turned back into the original image, so it is no good for bit-perfect archiving.
    pub fn is_lossy(self) -> bool {
        matches!(self, Self::Wua | Self::Zar | Self::Xiso)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Same => "same",
            Self::Raw => "raw",
            Self::Zip => "zip",
            Self::SevenZip => "7z",
            Self::Chd => "chd",
            Self::Rvz => "rvz",
            Self::Gcz => "gcz",
            Self::Wia => "wia",
            Self::Wua => "wua",
            Self::Zar => "zar",
            Self::Xiso => "xiso",
            Self::Cso => "cso",
            Self::Cci => "cci",
            Self::ThreeDs => "3ds",
            Self::Zcci => "zcci",
            Self::Cxi => "cxi",
            Self::Zcxi => "zcxi",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.name() == name)
    }

    /// The `local_files.archive_format` tag for this format -- `common::models::ArchiveFormat`'s
    /// own spelling, which is what every reader of that column expects.
    ///
    /// Kept apart from [`Self::extension`]: they agree except for XISO (named `.iso`), and they are answers
    /// to different questions (what the file is called, versus what the database calls it),
    /// and a test in the backend asserts they stay in step rather than leaving it to luck.
    pub fn archive_tag(self) -> Option<&'static str> {
        match self {
            Self::Same | Self::Raw => None,
            Self::Zip => Some("zip"),
            Self::SevenZip => Some("7z"),
            Self::Chd => Some("chd"),
            Self::Rvz => Some("rvz"),
            Self::Gcz => Some("gcz"),
            Self::Wia => Some("wia"),
            Self::Wua => Some("wua"),
            Self::Zar => Some("zar"),
            Self::Xiso => Some("xiso"),
            Self::Cso => Some("cso"),
            // Plain files to everything that reads that column.
            Self::Cci | Self::ThreeDs | Self::Zcci | Self::Cxi | Self::Zcxi => None,
        }
    }

    /// The file extension this format produces, or `None` for the two that produce whatever
    /// the source was.
    pub fn extension(self) -> Option<&'static str> {
        match self {
            Self::Same | Self::Raw => None,
            Self::Zip => Some("zip"),
            Self::SevenZip => Some("7z"),
            Self::Chd => Some("chd"),
            Self::Rvz => Some("rvz"),
            Self::Gcz => Some("gcz"),
            Self::Wia => Some("wia"),
            Self::Wua => Some("wua"),
            Self::Zar => Some("zar"),
            Self::Xiso => Some("iso"),
            Self::Cso => Some("cso"),
            // A game's; an update keeps `.cia` / `.zcia`, decided per file by the plan.
            Self::Cci => Some("cci"),
            Self::ThreeDs => Some("3ds"),
            Self::Zcci => Some("zcci"),
            Self::Cxi => Some("cxi"),
            Self::Zcxi => Some("zcxi"),
        }
    }
}

/// CHD creation preset, mirroring `rom::models::ChdPresets` on the wire.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChdPreset {
    /// Let chdman pick everything -- no codec list, no hunk size.
    #[default]
    ChdmanDefault,
    /// Tuned for a desktop: zstd, and CD hunks sized per platform.
    Pc,
    /// Tuned for a phone or handheld, where decode speed beats ratio.
    Mobile,
    /// Use the codecs and sizes given below verbatim.
    Custom,
}

impl ChdPreset {
    pub const ALL: [ChdPreset; 4] = [ChdPreset::ChdmanDefault, ChdPreset::Pc, ChdPreset::Mobile, ChdPreset::Custom];

    pub fn name(self) -> &'static str {
        match self {
            Self::ChdmanDefault => "chdman_default",
            Self::Pc => "pc",
            Self::Mobile => "mobile",
            Self::Custom => "custom",
        }
    }
}

/// What the archive settings forms offer, one list per pulldown. Each is the wire spelling of
/// a `rom::models` enum (`ZipCodecs`, `SevenZipCodecs`, `RvzCodecs`, `RvzBlockSize`,
/// `ChdCodecs`), and `api::copy`'s tests parse every entry, so a list cannot offer a value
/// the server would refuse. The web console mirrors them as constants of the same names.
///
/// zip and 7z offer only what `rom::archive` can actually *write* (`ArchiveArgs::archive_options`):
/// the bundled 7-Zip reads deflate64, brotli, lz4, lizard and friends, but creating them is
/// refused, and offering them made a copy fail file by file.
pub const ZIP_CODECS: [&str; 5] = ["zstd", "deflate", "bzip2", "lzma", "store"];
pub const SEVEN_ZIP_CODECS: [&str; 6] = ["zstd", "lzma2", "lzma", "bzip2", "deflate", "copy"];
pub const RVZ_CODECS: [&str; 5] = ["zstd", "lzma2", "lzma", "bzip2", "uncompressed"];
/// RVZ and GCZ blocks alike (Dolphin's 32 KiB - 2 MiB). WIA's is fixed at 2 MiB.
pub const RVZ_BLOCK_SIZES: [&str; 7] = ["32k", "64k", "128k", "256k", "512k", "1m", "2m"];
/// What Dolphin can write WIA with. (Its "purge" mode isn't offered: the archive layer has
/// no way to ask for it yet.)
pub const WIA_CODECS: [&str; 4] = ["lzma", "lzma2", "bzip2", "uncompressed"];
/// chdman's codec tags. A CHD holds at most [`CHD_MAX_CODECS`] of them.
pub const CHD_CODECS: [&str; 9] = ["cdzs", "cdfl", "cdlz", "cdzl", "zstd", "lzma", "zlib", "flac", "huff"];
/// What the CD and DVD pulldowns offer: each media's own codecs. (Either list is accepted
/// for either image -- the writer maps `zstd` <-> `cdzs` and so on by the chdman command --
/// but offering each its own spelling is what makes the choice readable. `huff` has no CD
/// form.)
pub const CHD_CD_CODECS: [&str; 4] = ["cdzs", "cdfl", "cdlz", "cdzl"];
pub const CHD_DVD_CODECS: [&str; 5] = ["zstd", "lzma", "zlib", "flac", "huff"];
pub const CHD_MAX_CODECS: usize = 4;

/// (lowest, highest, default) compression level for a codec of [`ZIP_CODECS`],
/// [`SEVEN_ZIP_CODECS`] or [`RVZ_CODECS`] -- the ranges `rom::models`' `check_compression_level`
/// enforces, so a form can keep its level box inside them when the codec changes.
pub fn codec_levels(codec: &str) -> (i8, i8, i8) {
    match codec {
        "store" | "copy" | "uncompressed" => (0, 0, 0),
        "zstd" => (1, 22, 5),
        _ => (1, 9, 5),
    }
}

fn level_5() -> i8 {
    5
}

fn zstd() -> String {
    "zstd".to_string()
}

/// zip settings. The defaults are `rom::models::ZipArgs::default()` -- zstd at level 5 --
/// which CLAUDE.md's measurements already settled; nothing here re-argues them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZipSettings {
    #[serde(default = "zstd")]
    pub codec: String,
    #[serde(default = "level_5")]
    pub level: i8,
    /// Re-archive a source that is *already* a zip with these settings. Off: it is copied
    /// byte for byte instead, which is faster and cannot change the bytes.
    #[serde(default)]
    pub re_archive: bool,
}

impl Default for ZipSettings {
    fn default() -> Self {
        Self { codec: zstd(), level: 5, re_archive: false }
    }
}

/// 7z settings; defaults are `rom::models::SevenZipArgs::default()`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SevenZipSettings {
    #[serde(default = "zstd")]
    pub codec: String,
    #[serde(default = "level_5")]
    pub level: i8,
    #[serde(default)]
    pub re_archive: bool,
}

impl Default for SevenZipSettings {
    fn default() -> Self {
        Self { codec: zstd(), level: 5, re_archive: false }
    }
}

fn block_128k() -> String {
    "128k".to_string()
}

/// RVZ settings; defaults are `rom::models::RvzArgs::default()` -- zstd, 128 KiB blocks,
/// level 5. GCZ and WIA have settings of their own ([`GczSettings`], [`WiaSettings`]): Dolphin
/// gives the three different choices, and WIA in particular has no zstd.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RvzSettings {
    #[serde(default = "zstd")]
    pub codec: String,
    #[serde(default = "block_128k")]
    pub block_size: String,
    #[serde(default = "level_5")]
    pub level: i8,
    #[serde(default)]
    pub remove_junk_data: bool,
    #[serde(default)]
    pub re_archive: bool,
}

impl Default for RvzSettings {
    fn default() -> Self {
        Self { codec: zstd(), block_size: block_128k(), level: 5, remove_junk_data: false, re_archive: false }
    }
}

/// GCZ settings. GCZ is always deflate, so there is no codec to pick -- only the block size,
/// 32 KiB to 2 MiB like Dolphin's own convert dialog offers, 128 KiB by default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GczSettings {
    #[serde(default = "block_128k")]
    pub block_size: String,
    #[serde(default)]
    pub re_archive: bool,
}

impl Default for GczSettings {
    fn default() -> Self {
        Self { block_size: block_128k(), re_archive: false }
    }
}

fn lzma() -> String {
    "lzma".to_string()
}

/// WIA settings: one of [`WIA_CODECS`] (no zstd -- that is RVZ's) and its level. The block
/// size is fixed at 2 MiB, the only one Dolphin offers for WIA (its minimum).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WiaSettings {
    #[serde(default = "lzma")]
    pub codec: String,
    #[serde(default = "level_5")]
    pub level: i8,
    #[serde(default)]
    pub re_archive: bool,
}

impl Default for WiaSettings {
    fn default() -> Self {
        Self { codec: lzma(), level: 5, re_archive: false }
    }
}

/// CHD settings. Everything but `preset` is read only when `preset` is
/// [`ChdPreset::Custom`]; the other three presets fill them in themselves.
///
/// Custom values come in two sets because chdman makes two kinds of image that want
/// different ones: `hunk_size`/`codecs` for a CD (`createcd`: cue/gdi/toc, 2448-byte frames,
/// the `cd..` codecs), `dvd_hunk_size`/`dvd_codecs` for everything else (`createdvd` for an
/// ISO, and the raw / hard disk commands: 2048- or 512-byte units, the plain codecs). The
/// CD pair keeps its old names so settings saved before the split still mean what they did.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChdSettings {
    #[serde(default)]
    pub preset: ChdPreset,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hunk_size: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_size: Option<u32>,
    /// chdman codec tags for a CD, best first (up to [`CHD_MAX_CODECS`]): `cdzs`, `cdfl`, ...
    #[serde(default)]
    pub codecs: Vec<String>,
    /// Hunk size for a DVD / non-CD image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dvd_hunk_size: Option<u32>,
    /// Codec tags for a DVD / non-CD image, best first: `zstd`, `lzma`, ...
    #[serde(default)]
    pub dvd_codecs: Vec<String>,
    #[serde(default)]
    pub re_archive: bool,
}

/// The archive settings a copy uses, and the per-platform output formats that decide which
/// of them applies to a given file.
///
/// Stored whole under the `copy_output_defaults` setting, and sent whole in a request when
/// the user overrides them for one copy ("decide on copy").
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OutputSettings {
    /// `app_platform_id` -> format. Absent means [`OutputFormat::Same`].
    #[serde(default)]
    pub per_platform: Vec<(u16, OutputFormat)>,
    #[serde(default)]
    pub zip: ZipSettings,
    #[serde(default)]
    pub seven_zip: SevenZipSettings,
    #[serde(default)]
    pub chd: ChdSettings,
    #[serde(default)]
    pub rvz: RvzSettings,
    #[serde(default)]
    pub gcz: GczSettings,
    #[serde(default)]
    pub wia: WiaSettings,
}

impl OutputSettings {
    pub fn format_for(&self, app_platform_id: u16) -> OutputFormat {
        self.per_platform.iter().find(|(id, _)| *id == app_platform_id).map(|(_, f)| *f).unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CopyFilesRequest {
    pub selection: CopySelection,
    #[serde(default)]
    pub mode: CopyMode,
    /// Absolute, on the machine whose worker runs the job.
    pub destination: String,
    /// `None` = the configured scheme and per-platform overrides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_scheme: Option<FolderScheme>,
    /// `None` = the configured defaults. Set only by "decide on copy", and applies to this
    /// job alone -- nothing here is written back to the settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputSettings>,
    #[serde(default)]
    pub source_preference: SourcePreference,
    #[serde(default)]
    pub on_conflict: ConflictPolicy,
    /// A selected title means every release of it, rather than only its top-ranked one.
    /// Off by default: a set's releases are explicit, a title's are not, and copying
    /// fourteen regional dumps because someone right-clicked a title is a nasty surprise.
    #[serde(default)]
    pub titles_all_releases: bool,
    /// Read each copied file back and check it against what the catalogue records. `None`
    /// uses the configured default (`copy_verify_default`); `InServer` ignores it and always
    /// checks.
    ///
    /// Orthogonal to [`CopyMode`] on purpose -- the two together are the four things a copy
    /// can be: to the collection or to external media, checked or not. Off is the fast path
    /// and the usual one; on doubles the reads, which over SMB is the whole cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<bool>,
    /// Per-destination answers to the collisions the preview listed. Anything not named here
    /// follows `on_conflict`, which is what the dialog's "overwrite all" / "skip all" set.
    /// Ignored by the modes that never overwrite.
    #[serde(default)]
    pub conflict_choices: Vec<(String, ConflictChoice)>,
    /// `Upload` only: which server to send to. Defaults to the main source when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload_source_id: Option<u16>,
    /// Emulators to export art for once the copy is done (`artwork_export`, one job each),
    /// named after the files this copy wrote. Empty: none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub export_artwork: Vec<u64>,
}

/// One planned file, as the preview shows it and the run executes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedFile {
    /// Absolute source path, already rewritten through the mount when it is remote.
    pub source: String,
    /// Absolute destination path.
    pub destination: String,
    pub size: u64,
    /// `None` when this machine holds it, else the source it comes from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_source: Option<String>,
    /// What it will be written as; `Same` is a plain byte copy.
    #[serde(default)]
    pub format: OutputFormat,
    /// Set when the file will not actually be copied, and why: `already at the destination`,
    /// `already held locally`, `duplicate of an earlier item`, `no mount for <source>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
}

/// What a copy would do, with no side effects. The same planning pass the run uses.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CopyPlan {
    pub destination: String,
    pub mode: CopyMode,
    /// Playable units -- a cue+bin set or a directory dump is one item, not N files.
    pub items: u64,
    pub files: u64,
    pub bytes: u64,
    /// Files that will be copied, and their total size.
    pub files_to_copy: u64,
    pub bytes_to_copy: u64,
    pub deduped: u64,
    pub already_held: u64,
    pub already_at_destination: u64,
    pub conversions: u64,
    /// `(wanted, actual)` for each destination name a collision renamed.
    pub renamed: Vec<(String, String)>,
    /// Sources with files in the selection but no mount covering their paths.
    pub no_mount: Vec<String>,
    /// Formats asked for that this machine has no native library to produce; those files
    /// fall back to a plain copy.
    pub archiving_unavailable: Vec<String>,
    /// Free bytes at the destination, and what the plan needs including the safety margin.
    pub free_bytes: Option<u64>,
    pub required_bytes: u64,
    /// Destinations that already hold something else. The dialog lists these so the user can
    /// answer per file before the job starts; the modes that never overwrite show them as the
    /// failures they will be.
    pub conflicts: Vec<String>,
    /// At most a few hundred rows, for the preview table. `files` is the real count.
    pub sample: Vec<PlannedFile>,
    /// Anything that stopped a selection resolving, worth showing before the user commits.
    pub warnings: Vec<String>,
}

/// What a finished copy did. Stored as the job's `result`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CopySummary {
    pub destination: String,
    pub mode: CopyMode,
    pub items: u64,
    pub files_copied: u64,
    pub files_converted: u64,
    pub files_skipped_same: u64,
    /// Of those, the ones matched on size alone -- no hash is recorded for that content, or
    /// it is a converted output whose bytes cannot be checked against the source's. The one
    /// place the resume check guesses, so it is counted rather than hidden.
    pub skipped_size_only: u64,
    pub bytes_copied: u64,
    pub bytes_skipped: u64,
    pub deduped: u64,
    pub already_held: u64,
    /// One content written to more than one destination path, with those paths. Not an
    /// error: the layout is the user's intent, and a hardlink survives neither exFAT nor a
    /// deletion of one copy.
    pub duplicated_content: Vec<(u64, Vec<String>)>,
    pub renamed: Vec<(String, String)>,
    pub no_mount: Vec<String>,
    /// Roms that ended up only partly copied, by name. Their copied files are left in place,
    /// since deleting them would fight the resume path.
    pub incomplete_items: Vec<String>,
    pub archiving_unavailable: Vec<String>,
    /// `.m3u` lines that matched no file this job produced; left untouched.
    pub m3u_unmatched: Vec<String>,
    /// Files recorded in this machine's own ownership tables (`to_collection` only).
    ///
    /// Written directly rather than by a follow-up `file_scan`: a copy already knows each
    /// file's `content_id` from the row it came from, and content is shared catalogue data,
    /// so re-deriving it by re-hashing every byte is work with a known answer.
    pub registered: u64,
    /// RetroAchievements hashes carried from the source rom to the new one. The scalar
    /// hashes need no carrying -- they hang off the shared `content` row the copy reuses --
    /// but `ra_hash` is per playable unit and lives on a junction, so it does.
    pub ra_hashes_carried: u64,
    /// Destinations left untouched because something was already there, in a mode that never
    /// overwrites. Reported as failures rather than skips: the file the user asked for is not
    /// where they asked for it.
    pub blocked_existing: Vec<String>,
    /// Destinations left as they were because the user chose to skip them (or left the
    /// default, skip). Not failures.
    #[serde(default)]
    pub kept_existing: Vec<String>,
    /// Files checked by decoding them and comparing their *members*, because the container
    /// changed and the file's own bytes are new. See `api::copy::content_check`.
    pub content_checked: u64,
    /// Of those, the ones whose members did not match what the source's content links to.
    pub content_mismatched: Vec<String>,
    /// New archives given their own `content` row and whole-file hashes, so the next
    /// integrity check is a cheap whole-file comparison instead of another decode.
    pub archives_hashed: u64,
    /// Jobs queued to work out what the copy produced: the server's own `file_scan` after an
    /// upload, or a `hash_match` after a payload conversion.
    pub rehash_jobs: Vec<u64>,
    /// `artwork_export` jobs queued for the emulators the copy was for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artwork_jobs: Vec<u64>,
    /// Files read back and confirmed against a hash the catalogue already held.
    pub verified: u64,
    /// Files read back whose content had no recorded hash to compare against. Nothing was
    /// concluded and nothing was written -- see `api::copy::CheckOutcome::Unverifiable`.
    pub unverifiable: u64,
    /// Hashes filled in off a read that had already confirmed the bytes.
    pub hashes_recorded: u64,
    /// Files whose bytes did not match what the catalogue records. These are **not**
    /// registered: a wrong copy must not be recorded as a good one.
    pub verify_failed: Vec<String>,
    pub cancelled: bool,
    pub errors: Vec<String>,
}

/// A mount: where one source machine's paths appear on this one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mount {
    pub id: u16,
    pub source_id: u16,
    pub source_name: String,
    pub remote_prefix: String,
    /// `/` or `\` -- the separator the SOURCE machine writes.
    pub remote_sep: String,
    pub local_path: String,
    pub enabled: bool,
    pub last_verified_at: Option<chrono::NaiveDateTime>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MountSpec {
    pub remote_prefix: String,
    pub local_path: String,
    /// Auto-detected from `remote_prefix` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_sep: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_directory_id: Option<u64>,
    #[serde(default = "crate::copy::default_true")]
    pub enabled: bool,
}

pub(crate) fn default_true() -> bool {
    true
}

/// The answer to "is this mount actually there?".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MountCheck {
    pub ok: bool,
    pub detail: String,
    /// The file it tried, when it got as far as trying one. A mount pointing at the *wrong*
    /// share passes an `is_dir` check and fails here, which is the whole reason it probes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probed: Option<String>,
}

/// One platform's destination folder under every scheme, plus what a copy would really use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlatformFolder {
    pub app_platform_id: u16,
    pub name: String,
    pub dat: String,
    pub display: String,
    /// `None` where IGDB has no equivalent platform; the UI falls back to `es`.
    pub slug: Option<String>,
    pub es: String,
    /// What the user typed, if anything.
    pub custom: Option<String>,
    /// The folder a copy would actually use, combine groups applied.
    pub effective: String,
    /// Set when a combine group folds this platform into another's folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub combined_into: Option<String>,
    /// What this platform's files are written as.
    #[serde(default)]
    pub output_format: OutputFormat,
    /// Formats this platform can actually take.
    pub formats: Vec<OutputFormat>,
}

/// A combine group as the config tab shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CombineGroupView {
    pub key: String,
    pub label: String,
    pub parent: String,
    pub children: Vec<String>,
    pub on: bool,
}

/// `GET /api/v1/naming/folders`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamingView {
    pub scheme: FolderScheme,
    pub combine: Vec<CombineGroupView>,
    pub platforms: Vec<PlatformFolder>,
    /// Folders more than one platform resolves to. Legal -- someone may genuinely want one
    /// `nintendo/` -- so it is reported, not refused.
    pub shared_folders: Vec<(String, Vec<String>)>,
}

/// `PUT /api/v1/naming/folders`. Only what changed needs sending.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NamingUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheme: Option<FolderScheme>,
    /// Group key -> on.
    #[serde(default)]
    pub combine: Vec<(String, bool)>,
    /// `app_platform_id` -> folder name; an empty string clears the override.
    #[serde(default)]
    pub folders: Vec<(u16, String)>,
    /// `app_platform_id` -> output format.
    #[serde(default)]
    pub formats: Vec<(u16, OutputFormat)>,
}

/// Which file a copy takes when one copy of a release holds two dumps of one file -- a
/// headered and a headerless cartridge (No-Intro names the headerless one `.unh`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeaderPreference {
    /// The file in the directory first in priority (Config > Directories).
    #[default]
    Priority,
    Headered,
    Headerless,
}

impl HeaderPreference {
    pub const ALL: [HeaderPreference; 3] = [HeaderPreference::Priority, HeaderPreference::Headered, HeaderPreference::Headerless];

    pub fn name(self) -> &'static str {
        match self {
            HeaderPreference::Priority => "priority",
            HeaderPreference::Headered => "headered",
            HeaderPreference::Headerless => "headerless",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }

    /// Lower first: where a file of this kind goes among the dumps of one file.
    pub fn rank(self, headerless: bool) -> u8 {
        match self {
            HeaderPreference::Priority => 0,
            HeaderPreference::Headered => u8::from(headerless),
            HeaderPreference::Headerless => u8::from(!headerless),
        }
    }
}

/// Which file a copy takes when a release is held both as a Wii U disc image (`.wux` /
/// `.wud`) and as Cemu's archive (`.wua`) under one name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WiiuPreference {
    /// The file in the directory first in priority (Config > Directories).
    Priority,
    /// The `.wua`; the disc image's `.key` is then left behind too, as the archive is
    /// already decrypted.
    #[default]
    Wua,
    /// The disc image (`.wux` before `.wud`), with its `.key`.
    Wux,
}

impl WiiuPreference {
    pub const ALL: [WiiuPreference; 3] = [WiiuPreference::Priority, WiiuPreference::Wua, WiiuPreference::Wux];

    pub fn name(self) -> &'static str {
        match self {
            WiiuPreference::Priority => "priority",
            WiiuPreference::Wua => "wua",
            WiiuPreference::Wux => "wux",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }

    /// Lower first: where a file with this extension goes among one game's Wii U formats.
    /// `None` for an extension that is not one of them.
    pub fn rank(self, ext: &str) -> Option<u8> {
        let ext = ext.to_ascii_lowercase();
        let order: [&str; 3] = match self {
            WiiuPreference::Priority => return ["wua", "wux", "wud"].contains(&ext.as_str()).then_some(0),
            WiiuPreference::Wua => ["wua", "wux", "wud"],
            WiiuPreference::Wux => ["wux", "wud", "wua"],
        };
        order.iter().position(|e| *e == ext).map(|i| i as u8)
    }
}

/// One game held in several dump formats in one copy (`Game.fds` + `Game.qd`): a copy takes
/// the first of these that is there. Each list is one family of interchangeable formats; a
/// copy's files are alternatives only when they share a name and every extension among them
/// is in one list, so a disc's `.cue` and `.bin` are never two formats of one thing.
/// (`.nes` / `.unh` follow [`HeaderPreference`] instead.)
pub const DEFAULT_FORMAT_ORDER: [&[&str]; 5] = [&["fds", "qd"], &["z64", "v64", "n64"], &["sfc", "smc", "swc", "fig"], &["rom", "dsk", "cas", "wav"], &["t77", "wav"]];

pub fn default_format_order() -> Vec<Vec<String>> {
    DEFAULT_FORMAT_ORDER.iter().map(|g| g.iter().map(|e| e.to_string()).collect()).collect()
}

/// What the copy dialog needs to open: the configured settings and whether the user asked to
/// be shown them on every copy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CopyDefaults {
    pub scheme: FolderScheme,
    pub output: OutputSettings,
    pub decide_on_copy: bool,
    /// The algorithm a verified copy makes sure the catalogue ends up with
    /// (`copy_populate_hash`). Not the one it compares against -- that is chosen per file
    /// from whatever is already recorded, cheapest-first for this CPU.
    pub populate_hash: String,
    /// Whether `verify` defaults to on (`copy_verify_default`).
    pub verify_default: bool,
    /// Headered or headerless, when a copy holds both (`copy_header_preference`).
    #[serde(default)]
    pub header_preference: HeaderPreference,
    /// `.wua` or `.wux`, when a copy holds both (`copy_wiiu_preference`).
    #[serde(default)]
    pub wiiu_preference: WiiuPreference,
    /// Formats first-preferred, per family (`copy_format_order`; [`DEFAULT_FORMAT_ORDER`]).
    #[serde(default = "default_format_order")]
    pub format_order: Vec<Vec<String>>,
    /// Between copies of a release, a decrypted dump before an encrypted one -- told by
    /// "Encrypted" in its folder or file name (`copy_prefer_decrypted`, default on).
    #[serde(default = "default_true")]
    pub prefer_decrypted: bool,
    /// Whether choosing a lossy output ([`OutputFormat::is_lossy`]) asks first
    /// (`copy_lossy_warning`, default on).
    #[serde(default = "default_true")]
    pub lossy_warning: bool,
    pub recent_destinations: Vec<String>,
    /// Formats no native library on this machine can produce, so the dialog greys them out
    /// rather than promising a conversion that silently degrades to a plain copy.
    pub unavailable_formats: Vec<OutputFormat>,
    /// Which modes this machine can actually run, so the dialog offers those and no others.
    /// `InServer` is missing on a client (it rearranges a collection this machine does not
    /// own); `Upload` is missing with no mounted server to write through.
    #[serde(default)]
    pub modes: Vec<CopyMode>,
}

/// `PUT /api/v1/copy/defaults`. Either field alone is a valid update.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DefaultsUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decide_on_copy: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub populate_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_default: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_preference: Option<HeaderPreference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wiiu_preference: Option<WiiuPreference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_order: Option<Vec<Vec<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_decrypted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lossy_warning: Option<bool>,
}

/// The algorithms a copy check or an integrity check may be told to *populate*.
///
/// Which algorithm it *compares* against is not configurable: it is whichever recorded hash
/// is cheapest to compute on this CPU (`rom::hash::fallback_order`), because comparing
/// against something the catalogue already knows is the only way a check can conclude
/// anything. This setting says which hash every file should end up having, and xxh3_128 is
/// the default for it -- the fastest of the five by a wide margin, and 128 bits is far more
/// than enough for the failures this is actually for: a truncated write, a full card, a bad
/// cable.
pub const QUICK_HASHES: [&str; 5] = ["xxh3_128", "crc32", "md5", "sha1", "sha256"];

pub fn is_quick_hash(name: &str) -> bool {
    QUICK_HASHES.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The five modes differ only in policy, and the policy lives in these five predicates.
    /// Asserting the whole table in one place is what keeps "in-server never overwrites" from
    /// quietly becoming "in-server usually doesn't".
    #[test]
    fn each_mode_answers_the_five_policy_questions_the_same_way_every_time() {
        for (mode, local, source, registers, verify, never, asks) in [
            (CopyMode::ToCollection, true, false, true, false, false, false),
            (CopyMode::ToExternal, false, false, false, false, false, false),
            (CopyMode::InLocal, false, false, true, false, false, true),
            (CopyMode::InServer, false, false, true, true, true, false),
            (CopyMode::Upload, false, true, false, false, true, false),
        ] {
            assert_eq!(mode.skips_local_holdings(), local, "{mode:?} skips_local_holdings");
            assert_eq!(mode.skips_source_holdings(), source, "{mode:?} skips_source_holdings");
            assert_eq!(mode.registers(), registers, "{mode:?} registers");
            assert_eq!(mode.mandatory_verify(), verify, "{mode:?} mandatory_verify");
            assert_eq!(mode.never_overwrites(), never, "{mode:?} never_overwrites");
            assert_eq!(mode.asks_about_conflicts(), asks, "{mode:?} asks_about_conflicts");
        }
    }

    /// Nothing both asks the user about an existing file and refuses to overwrite one: the
    /// question would have no answer.
    #[test]
    fn asking_and_refusing_are_mutually_exclusive() {
        for mode in CopyMode::ALL {
            assert!(!(mode.asks_about_conflicts() && mode.never_overwrites()), "{mode:?}");
        }
    }

    /// An upload's rows are the server's to write; a client inventing them in its `remote_*`
    /// mirror would be contradicted by the next sync.
    #[test]
    fn an_upload_never_registers_locally() {
        assert!(!CopyMode::Upload.registers());
    }

    #[test]
    fn mode_names_round_trip() {
        for mode in CopyMode::ALL {
            assert_eq!(CopyMode::from_name(mode.name()), Some(mode));
            assert!(!mode.label().is_empty() && !mode.hint().is_empty(), "{mode:?}");
        }
        assert_eq!(CopyMode::from_name("nonsense"), None);
    }
}
