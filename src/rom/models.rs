use std::path::{Path, PathBuf};

use bitflags::bitflags;

use crate::common::models::{MediaType, Platform};
use super::hash::{HashFlags, FileHashRecord, HashReport};
use super::dolphin_wrapper::DiscCompression;
use super::detect::reason::{DetectionStep, DetectionStepKind, Evidence, RomReason, DETECTION_TRAIL_CAP};

/// Detection settings for one operation (`operation::Single`): a forced platform skips the
/// heuristics entirely; otherwise the extension and parent-path heuristics narrow the
/// candidate platforms before the detection cycle (`detect::metadata_cycle`).
#[derive(Debug, Clone)]
pub struct MetadataArgs {
    pub forced_platform: Option<Platform>,
    /// A platform to try first, without trusting it: unlike `forced_platform`, a miss falls
    /// back to normal detection. Meant for a scan's platform-directory context (the file sits
    /// somewhere under a folder resolved as this platform) -- see
    /// `detect::hinted_metadata_cycle`. Ignored when `forced_platform` is set.
    pub preferred_platform: Option<Platform>,
    pub path_heuristic: bool,
    pub extension_heuristic: bool,
    /// Reserved; not applied yet.
    pub timeout_seconds: u64,
}

impl Default for MetadataArgs {
    fn default() -> Self {
        Self {
            forced_platform: None,
            preferred_platform: None,
            path_heuristic: true,
            extension_heuristic: true,
            timeout_seconds: 0,
        }
    }
}

bitflags! {
    /// Which platforms' conversions an operation performs (`convert::converter_for`). A bit
    /// that's off leaves that platform's files untouched.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct ConvertFlags: u32 {
        const NES = 1 << 0;
        const SNES = 1 << 1;
        const N64 = 1 << 2;
        const NDS = 1 << 3;
        const NDSi = 1 << 4;
        const O3DS = 1 << 5;
        const N3DS = 1 << 6;
        const FDS = 1 << 7;
    }
}

/// iNES (NES) / `FDS\x1A` (Famicom Disk System) header presence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NesHeader {
    Unheadered = 0,
    Headered = 1,
}

/// SNES file layout: bare ROM (`sfc`) or one of the 512-byte copier headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SnesHeader {
    Sfc = 0,
    Smc = 1,
    Fig = 2,
    Swc = 3,
    Ufo = 4,
    Gd7 = 5,
}

/// N64 ROM byte order: `z64` (big-endian, native), `v64` (16-bit byte-swapped), `n64`
/// (32-bit little-endian).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum N64Type {
    BigEndian = 0,
    ByteSwapped = 1,
    LittleEndian = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RomCrypt {
    Decrypted = 0,
    Encrypted = 1,
}

/// File extension for an unheadered NES ROM. `.nes` by default: some emulators don't
/// recognise `.unh` (if they take unheadered ROMs at all).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NesUnheaderedExtension {
    Nes,
    Unh,
}

/// Conversion targets, per platform (only the platforms whose bit is set in
/// `convert_flags` are converted). The crypt fields are placeholders: no converter exists
/// for them yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertArgs {
    pub convert_flags: ConvertFlags,
    pub nes_header: NesHeader,
    pub nes_unheadered_extension: NesUnheaderedExtension,
    pub fds_header: NesHeader,
    pub snes_header: SnesHeader,
    pub n64_type: N64Type,
    pub nds_crypt: RomCrypt,
    pub ndsi_crypt: RomCrypt,
    pub o3ds_crypt: RomCrypt,
    pub n3ds_crypt: RomCrypt,
}

impl Default for ConvertArgs {
    fn default() -> Self {
        Self {
            convert_flags: ConvertFlags::empty(),
            nes_header: NesHeader::Unheadered,
            nes_unheadered_extension: NesUnheaderedExtension::Nes,
            fds_header: NesHeader::Unheadered,
            snes_header: SnesHeader::Sfc,
            n64_type: N64Type::BigEndian,
            nds_crypt: RomCrypt::Decrypted,
            ndsi_crypt: RomCrypt::Decrypted,
            o3ds_crypt: RomCrypt::Decrypted,
            n3ds_crypt: RomCrypt::Decrypted,
        }
    }
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct ArchiveFlags: u32 {
        const RVZ = 1 << 0;
        const CHD = 1 << 1;
        const ZIP = 1 << 2;
        const SEVEN_Z = 1 << 3;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RvzCodecs {
    Uncompressed = 0,
    Zstd = 1,
    Bzip2 = 2,
    Lzma = 3,
    Lzma2 = 4,
}

#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RvzBlockSize {
    Block_32k = 0,
    Block_64k = 1,
    Block_128k = 2,
    Block_256k = 3,
    Block_512k = 4,
    Block_1m = 5,
    Block_2m = 6,
}

impl RvzBlockSize {
    pub fn bytes(self) -> u32 {
        match self {
            Self::Block_32k => 32 * 1024,
            Self::Block_64k => 64 * 1024,
            Self::Block_128k => 128 * 1024,
            Self::Block_256k => 256 * 1024,
            Self::Block_512k => 512 * 1024,
            Self::Block_1m => 1024 * 1024,
            Self::Block_2m => 2 * 1024 * 1024,
        }
    }
}

/// Settings for the Dolphin container formats (RVZ, and GCZ/WIA, which share them: GCZ
/// has no codec choice, so `algo`/`compression_level` are ignored for it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RvzArgs {
    pub algo: RvzCodecs,
    pub block_size: RvzBlockSize,
    pub compression_level: i8,
    pub remove_junk_data: bool,
}

impl Default for RvzArgs {
    fn default() -> Self {
        Self {
            algo: RvzCodecs::Zstd,
            block_size: RvzBlockSize::Block_128k,
            compression_level: 5,
            remove_junk_data: false,
        }
    }
}

impl RvzArgs {
    pub fn check_compression_level(&self) -> bool {
        match self.algo {
            RvzCodecs::Uncompressed => self.compression_level == 0,
            RvzCodecs::Zstd => (1..=22).contains(&self.compression_level),
            RvzCodecs::Bzip2 => (1..=9).contains(&self.compression_level),
            RvzCodecs::Lzma => (1..=9).contains(&self.compression_level),
            RvzCodecs::Lzma2 => (1..=9).contains(&self.compression_level),
        }
    }

    pub fn correct_compression_level(&mut self) {
        if !self.check_compression_level() {
            self.compression_level = match self.algo {
                RvzCodecs::Uncompressed => 0,
                RvzCodecs::Zstd => 5,
                RvzCodecs::Bzip2 => 5,
                RvzCodecs::Lzma => 5,
                RvzCodecs::Lzma2 => 5,
            };
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChdCodecs {
    Zstd = 0,
    Zlib = 1,
    Lzma = 2,
    Flac = 3,
    Huff = 4,
    Cdzs = 5,
    Cdzl = 6,
    Cdlz = 7,
    Cdfl = 8,
}

/// Which CHD tuning `apply_preset` writes into [`ChdArgs`].
///
/// `ChdmanDefault` used to be spelled `Custom`, which said the opposite of what it does:
/// it means "set nothing, let chdman choose". The discriminants are unchanged, so a
/// stored 0 still means what it always did, and `Custom` is now the variant that actually
/// keeps the caller's codecs and sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChdPresets {
    /// No codec list, no hunk size -- whatever chdman picks for the media.
    ChdmanDefault = 0,
    AppRecommendPc = 1,
    AppRecommendAndroid = 2,
    /// Use the `codecs` / `hunk_size` / `unit_size` already on the args, untouched.
    Custom = 3,
}

/// CHD creation settings. `is_cd` is set by the operation from the input's own layout
/// before [`ChdArgs::apply_preset`] runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChdArgs {
    pub preset: ChdPresets,
    pub is_cd: bool,
    pub num_processors: Option<usize>,
    pub hunk_size: Option<usize>,
    pub unit_size: Option<usize>,
    pub codecs: Option<Vec<ChdCodecs>>,
    /// [`ChdPresets::Custom`]'s values for a non-CD image, for a caller that holds both sets
    /// up front (`hunk_size`/`codecs` are the CD ones then) and so can only choose once the
    /// input is read: [`ChdArgs::select_media`]. A caller that already knows the media
    /// (chdlite) just sets `hunk_size`/`codecs` and leaves these `None`.
    pub dvd_hunk_size: Option<usize>,
    pub dvd_codecs: Option<Vec<ChdCodecs>>,
}

impl Default for ChdArgs {
    fn default() -> Self {
        Self {
            preset: ChdPresets::ChdmanDefault,
            is_cd: false,
            num_processors: None,
            hunk_size: None,
            unit_size: None,
            codecs: None,
            dvd_hunk_size: None,
            dvd_codecs: None,
        }
    }
}

/// Recommended CD hunk size (in bytes, always a multiple of the 2448-byte CD frame
/// size chdman requires): 2448 for platforms whose games are known for frequent small,
/// scattered reads (real hardware was seek-latency sensitive -- Neo Geo CD in
/// particular streams constantly from a slow 1x drive with minimal RAM to cache into;
/// PC Engine CD is similarly constrained; Dreamcast's GD-ROM access patterns lean the
/// same way), minimizing decompression wasted on data the caller didn't ask for.
/// 19584 (chdman's own default, 8 frames/hunk) for platforms leaning more on long
/// sequential reads (CD-DA background music, FMV cutscenes) and/or where better
/// compression matters more, where it wins on both compression ratio and total
/// decompress-call count.
fn cd_recommended_hunk_size(platform: Platform) -> usize {
    match platform {
        Platform::NeoGeoCd | Platform::TurboGrafxCd | Platform::Dreamcast => 2448,
        _ => 19584,
    }
}

impl ChdArgs {
    /// Sets `is_cd` and, for a custom preset, makes the DVD pair the one in effect for a
    /// non-CD image -- blank included, which lets chdman choose rather than reusing the CD
    /// values. For a caller holding both sets (romexplorer's copy, via `archive_options`);
    /// one that set `codecs`/`hunk_size` for a media it already knew (chdlite) must not call
    /// it, and doesn't.
    pub fn select_media(&mut self, is_cd: bool) {
        self.is_cd = is_cd;
        if self.preset == ChdPresets::Custom && !is_cd {
            self.hunk_size = self.dvd_hunk_size;
            self.codecs = self.dvd_codecs.clone();
        }
    }

    pub fn apply_preset(&mut self, platform: Platform) {
        match self.preset {
            ChdPresets::ChdmanDefault => {
                // chdman's own choice: say nothing about codecs or sizes.
                self.codecs = None;
                self.hunk_size = None;
                self.unit_size = None;
            }
            ChdPresets::Custom => {
                // The caller's own codecs and sizes, kept exactly as given -- including the
                // ones it left unset, which stay `None` so chdman picks its own default for
                // each. "Custom" means "these, and chdman for the rest", not "all of these".
                // (`codecs`/`hunk_size` are whatever the caller put there: a caller holding
                // separate CD and DVD values picks between them with `select_media` first.)
            }
            ChdPresets::AppRecommendPc => {
                if self.is_cd {
                    self.codecs = Some(vec![ChdCodecs::Cdzs, ChdCodecs::Cdfl]);
                    self.hunk_size = Some(cd_recommended_hunk_size(platform));
                }
                else {
                    if platform == Platform::PlayStationPortable {
                        self.hunk_size = Some(2048);
                    }
                    else {
                        self.hunk_size = Some(4096);
                    };
                    self.codecs = Some(vec![ChdCodecs::Zstd]);
                }
            }

            ChdPresets::AppRecommendAndroid => {
                if self.is_cd {
                    self.hunk_size = Some(cd_recommended_hunk_size(platform));
                    if platform == Platform::PlayStation2 {
                        self.codecs = Some(vec![ChdCodecs::Cdzl, ChdCodecs::Cdfl]);
                    } else {
                        self.codecs = Some(vec![ChdCodecs::Cdzs, ChdCodecs::Cdfl]);
                    }
                } else {
                    if platform == Platform::PlayStationPortable {
                        self.hunk_size = Some(2048);
                    } else {
                        self.hunk_size = Some(4096);
                    };
                    if platform == Platform::PlayStation2 {
                        self.codecs = Some(vec![ChdCodecs::Zlib]);
                    } else {
                        self.codecs = Some(vec![ChdCodecs::Zstd]);
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZipCodecs {
    Store = 0,
    Deflate = 1,
    Deflate64 = 2,
    Bzip2 = 3,
    Lzma = 4,
    Lzma2 = 5,
    Zstd = 6,
    Brotli = 7,
    Lz4 = 8,
    Lizard = 9,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ZipArgs {
    pub codec: ZipCodecs,
    pub compression_level: u8,
}

impl Default for ZipArgs {
    fn default() -> Self {
        Self {
            codec: ZipCodecs::Zstd,
            compression_level: 5,
        }
    }
}

impl ZipArgs {
    pub fn check_compression_level(&self) -> bool {
        match self.codec {
            ZipCodecs::Store => self.compression_level == 0,
            ZipCodecs::Deflate | ZipCodecs::Deflate64 | ZipCodecs::Bzip2 | ZipCodecs::Lzma | ZipCodecs::Lzma2 => {
                (1..=9).contains(&self.compression_level)
            }
            ZipCodecs::Brotli => (0..=11).contains(&self.compression_level),
            ZipCodecs::Lz4 => (1..=12).contains(&self.compression_level),
            ZipCodecs::Lizard => (10..=49).contains(&self.compression_level),
            ZipCodecs::Zstd => (1..=22).contains(&self.compression_level),
        }
    }

    pub fn correct_compression_level(&mut self) {
        if !self.check_compression_level() {
            self.compression_level = match self.codec {
                ZipCodecs::Store => 0,
                ZipCodecs::Deflate | ZipCodecs::Deflate64 | ZipCodecs::Bzip2 | ZipCodecs::Lzma | ZipCodecs::Lzma2 => 5,
                ZipCodecs::Brotli => 5,
                ZipCodecs::Lz4 => 4,
                ZipCodecs::Lizard => 20,
                ZipCodecs::Zstd => 5,
            };
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SevenZipCodecs {
    Copy = 0,
    Deflate = 1,
    Deflate64 = 2,
    Bzip2 = 3,
    Lzma = 4,
    Lzma2 = 5,
    Ppmd = 6,
    Delta = 7,
    Brotli = 8,
    Lz4 = 9,
    Lz5 = 10,
    Lizard = 11,
    FastLzma2 = 12,
    Zstd = 13,
    Bcj = 14,
    Bcj2 = 15,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SevenZipArgs {
    pub codec: SevenZipCodecs,
    pub compression_level: u8,
}

impl Default for SevenZipArgs {
    fn default() -> Self {
        Self {
            codec: SevenZipCodecs::Zstd,
            compression_level: 5,
        }
    }
}

impl SevenZipArgs {
    pub fn check_compression_level(&self) -> bool {
        match self.codec {
            SevenZipCodecs::Copy | SevenZipCodecs::Bcj | SevenZipCodecs::Bcj2 => {
                self.compression_level == 0
            }
            SevenZipCodecs::Deflate | SevenZipCodecs::Deflate64 | SevenZipCodecs::Bzip2 | SevenZipCodecs::Lzma | SevenZipCodecs::Lzma2 => {
                (1..=9).contains(&self.compression_level)
            }
            SevenZipCodecs::Ppmd => (1..=32).contains(&self.compression_level),
            SevenZipCodecs::Delta => (1..=255).contains(&self.compression_level),
            SevenZipCodecs::Brotli => (0..=11).contains(&self.compression_level),
            SevenZipCodecs::Lz4 => (1..=12).contains(&self.compression_level),
            SevenZipCodecs::Lz5 => (1..=15).contains(&self.compression_level),
            SevenZipCodecs::Lizard => (10..=49).contains(&self.compression_level),
            SevenZipCodecs::FastLzma2 => (1..=9).contains(&self.compression_level),
            SevenZipCodecs::Zstd => (1..=22).contains(&self.compression_level),
        }
    }

    pub fn correct_compression_level(&mut self) {
        if !self.check_compression_level() {
            self.compression_level = match self.codec {
                SevenZipCodecs::Copy | SevenZipCodecs::Bcj | SevenZipCodecs::Bcj2 => 0,
                SevenZipCodecs::Deflate | SevenZipCodecs::Deflate64 | SevenZipCodecs::Bzip2 | SevenZipCodecs::Lzma | SevenZipCodecs::Lzma2 => 5,
                SevenZipCodecs::Ppmd => 6,
                SevenZipCodecs::Delta => 1,
                SevenZipCodecs::Brotli => 5,
                SevenZipCodecs::Lz4 => 4,
                SevenZipCodecs::Lz5 => 8,
                SevenZipCodecs::Lizard => 20,
                SevenZipCodecs::FastLzma2 => 5,
                SevenZipCodecs::Zstd => 5,
            };
        }
    }
}

/// Archive targets for one operation. `do_xxx` enables creating that format for the
/// platforms it applies to (CHD for disc platforms, a Dolphin format for GameCube/Wii,
/// zip/7z for everything else -- see `platforms::helpers`); `repack_xxx` additionally
/// re-creates an input that is *already* that format with these settings (off: such an
/// input is left alone). Targets are mutually exclusive, with one exception: `do_chd` may
/// be combined with exactly one of `do_rvz`/`do_gcz`/`do_wia`, since the platform decides
/// between them -- see [`ArchiveArgs::validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveArgs {
    pub do_rvz: bool,
    pub repack_rvz: bool,
    pub do_gcz: bool,
    pub repack_gcz: bool,
    pub do_wia: bool,
    pub repack_wia: bool,
    /// Shared by RVZ/GCZ/WIA.
    pub rvz_args: RvzArgs,
    pub do_chd: bool,
    pub repack_chd: bool,
    pub chd_args: ChdArgs,
    pub do_zip: bool,
    pub repack_zip: bool,
    pub zip_args: ZipArgs,
    pub do_7z: bool,
    pub repack_7z: bool,
    pub seven_zip_args: SevenZipArgs,
}

impl Default for ArchiveArgs {
    fn default() -> Self {
        Self {
            do_rvz: false,
            repack_rvz: false,
            do_gcz: false,
            repack_gcz: false,
            do_wia: false,
            repack_wia: false,
            rvz_args: RvzArgs::default(),
            do_chd: false,
            repack_chd: false,
            chd_args: ChdArgs::default(),
            do_zip: false,
            repack_zip: false,
            zip_args: ZipArgs::default(),
            do_7z: false,
            repack_7z: false,
            seven_zip_args: SevenZipArgs::default(),
        }
    }
}

impl ArchiveArgs {
    /// Any target enabled at all.
    pub fn any_enabled(&self) -> bool {
        self.do_rvz || self.do_gcz || self.do_wia || self.do_chd || self.do_zip || self.do_7z
    }

    /// The one Dolphin format enabled, if any.
    pub fn dolphin_target(&self) -> Option<super::archive::ArchiveTarget> {
        use super::archive::ArchiveTarget;
        match (self.do_rvz, self.do_gcz, self.do_wia) {
            (true, false, false) => Some(ArchiveTarget::Rvz),
            (false, true, false) => Some(ArchiveTarget::Gcz),
            (false, false, true) => Some(ArchiveTarget::Wia),
            _ => None,
        }
    }

    /// The one file-archive format enabled, if any.
    pub fn file_target(&self) -> Option<super::archive::ArchiveTarget> {
        use super::archive::ArchiveTarget;
        match (self.do_zip, self.do_7z) {
            (true, false) => Some(ArchiveTarget::Zip),
            (false, true) => Some(ArchiveTarget::SevenZip),
            _ => None,
        }
    }

    /// The exclusivity rule (see the type's doc comment) plus compression-level sanity.
    pub fn validate(&self) -> Result<(), String> {
        let dolphin = usize::from(self.do_rvz) + usize::from(self.do_gcz) + usize::from(self.do_wia);
        if dolphin > 1 {
            return Err("only one of rvz/gcz/wia may be enabled".to_string());
        }
        if self.do_zip && self.do_7z {
            return Err("zip and 7z are mutually exclusive".to_string());
        }
        let file = usize::from(self.do_zip || self.do_7z);
        if file > 0 && (self.do_chd || dolphin > 0) {
            return Err("zip/7z cannot be combined with chd or rvz/gcz/wia".to_string());
        }
        if (self.do_rvz || self.do_gcz || self.do_wia) && !self.rvz_args.check_compression_level() {
            return Err("rvz/gcz/wia compression level is out of range for the codec".to_string());
        }
        if self.do_zip && !self.zip_args.check_compression_level() {
            return Err("zip compression level is out of range for the codec".to_string());
        }
        if self.do_7z && !self.seven_zip_args.check_compression_level() {
            return Err("7z compression level is out of range for the codec".to_string());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// CHD metadata -- all of this is read directly from a CHD's metadata table
// (chd_file::read_metadata / chdman's "info" command), never from the compressed hunk
// data, so it's obtainable without extracting/decompressing anything. See
// external/mame/src/lib/util/chd.h's metadata tag constants and
// external/mame/src/lib/util/cdrom.{h,cpp} for the format strings this mirrors.
// ---------------------------------------------------------------------------

/// Which metadata tag a track entry came from. All three use the same TRACK/TYPE/
/// SUBTYPE/FRAMES fields; CHT2 adds PREGAP/PGTYPE/PGSUB/POSTGAP, and CHGD (GD-ROM) adds
/// PAD on top of that -- see [`TrackMetadata`]'s optional fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChdTrackTag {
    /// 'CHTR' -- legacy per-track CD metadata (no pregap/postgap).
    Chtr,
    /// 'CHT2' -- current per-track CD metadata.
    Cht2,
    /// 'CHGD' -- per-track GD-ROM metadata.
    Chgd,
}

/// A track's data format (chdman's TYPE field, e.g. "MODE1_RAW"). Also reused for a
/// track's pregap format (PGTYPE), which is written with the exact same vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChdTrackType {
    /// "MODE1" -- 2048 bytes/sector.
    Mode1,
    /// "MODE1_RAW" -- 2352 bytes/sector.
    Mode1Raw,
    /// "MODE2" -- 2336 bytes/sector.
    Mode2,
    /// "MODE2_FORM1" -- 2048 bytes/sector.
    Mode2Form1,
    /// "MODE2_FORM2" -- 2324 bytes/sector.
    Mode2Form2,
    /// "MODE2_FORM_MIX" -- 2336 bytes/sector.
    Mode2FormMix,
    /// "MODE2_RAW" -- 2352 bytes/sector.
    Mode2Raw,
    /// "AUDIO" -- redbook audio, 2352 bytes/sector (588 samples).
    Audio,
}

/// A track's subchannel format (chdman's SUBTYPE field). Also reused for a track's
/// pregap subchannel format (PGSUB).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChdTrackSubtype {
    /// "RW" -- cooked 96 bytes/sector.
    Normal,
    /// "RW_RAW" -- raw uninterleaved 96 bytes/sector.
    Raw,
    /// No subcode data stored. Not normally written to a track's own SUBTYPE field
    /// (chdman always writes Normal or Raw there) -- included for completeness since
    /// it's a real value in MAME's cdrom_file::subcode_t, e.g. reachable via other APIs.
    None,
}

/// One track's metadata entry (one per track in the disc's table of contents).
#[derive(Debug, Clone)]
pub struct TrackMetadata {
    pub track_number: usize,
    /// Which tag this entry was read from -- determines which of the fields below are
    /// populated (`Chtr` has none of them; `Cht2` has all but `pad`; `Chgd` has all).
    pub tag: ChdTrackTag,
    /// This tag's index among metadata entries sharing the same tag (chd_file's
    /// metadata index, 0-based) -- distinct from `track_number`, though they're the
    /// same sequence in practice since chdman writes one entry per track in order.
    pub index: usize,
    pub track_type: ChdTrackType,
    pub subtype: ChdTrackSubtype,
    pub frames: usize,
    /// GD-ROM only (`Chgd`): padding frames.
    pub pad: Option<usize>,
    /// Absent for legacy `Chtr` entries.
    pub pregap: Option<usize>,
    pub pregap_type: Option<ChdTrackType>,
    pub pregap_subtype: Option<ChdTrackSubtype>,
    pub postgap: Option<usize>,
    /// The pregap's sectors are stored in the track data (chdman's `pgdatasize > 0`: the
    /// `PGTYPE` value carries a `V` prefix -- see `cdrom_file::parse_metadata` in
    /// `external/mame/src/lib/util/cdrom.cpp`). Decides `INDEX 00` vs `PREGAP` when a
    /// cue sheet is written for the track (`detect::sheet`). Always false when `pregap`
    /// is `None`/0.
    pub pregap_in_file: bool,
}

/// Hard disk geometry, from the 'GDDD' metadata tag (`CYLS:%d,HEADS:%d,SECS:%d,BPS:%d`)
/// -- present on `createhd` CHDs, absent on CD/GD-ROM/DVD/laserdisc CHDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HardDiskGeometry {
    pub cylinders: usize,
    pub heads: usize,
    pub sectors: usize,
    pub bytes_per_sector: usize,
}

#[derive(Debug, Clone, Default)]
pub struct ChdMetadata {
    pub file_version: usize,
    pub logical_size: usize, //bytes
    pub hunk_size: usize,    //bytes
    pub total_hunks: usize,
    pub unit_size: usize, //bytes
    pub total_units: usize,
    pub compression: Vec<ChdCodecs>,
    pub chd_size: usize, //bytes
    pub ratio: f32,      //percentage
    pub sha1: String,      //SHA-1 hash extracted from chd metadata, not computed.
    pub data_sha1: String, //SHA-1 hash extracted from chd metadata, not computed. Only present in v4+.
    /// Present if this CHD is a diff against a parent CHD.
    pub parent_sha1: Option<String>,
    /// 'DVD ' metadata tag present (chdman's `createdvd`) -- a plain marker, no payload.
    /// Note this only distinguishes chdman's own DVD command from `createraw`; both
    /// otherwise look like a single untracked data stream.
    pub is_dvd: bool,
    /// Present on `createhd` CHDs (the 'GDDD' tag), absent otherwise.
    pub hard_disk_geometry: Option<HardDiskGeometry>,
    /// One entry per track, in track order. Empty for hard disk/DVD/laserdisc CHDs.
    pub tracks: Vec<TrackMetadata>,
    /// Session number (from the 'CHSE' tag) for each track index, for multi-session
    /// (mixed-mode) discs. Empty if the CHD has no session metadata (single session).
    pub track_sessions: Vec<u32>,
}

// ---------------------------------------------------------------------------
// RVZ/WIA metadata -- read from the container's own header (WIAHeader1/WIAHeader2),
// before any group/chunk data is decompressed. See
// external/dolphin/Source/Core/DiscIO/WIABlob.h.
// ---------------------------------------------------------------------------

/// From WIAHeader2::disc_type (0/1/2) -- which kind of disc this RVZ (or WIA) contains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RvzDiscType {
    Unknown,
    GameCube,
    Wii,
}

impl Default for RvzDiscType {
    fn default() -> Self {
        Self::Unknown
    }
}

#[derive(Debug, Clone)]
pub struct RvzMetadata {
    pub disc_type: RvzDiscType,
    /// WIA never uses Zstd; RVZ never uses Purge (see `dolphin_wrapper::DiscCompression`).
    pub compression: DiscCompression,
    /// Informative only, per WIAHeader2's own comment -- not required to decompress.
    pub compression_level: i32,
    /// Block/chunk size (`DolphinConvertArgs::block_size` at creation time).
    pub chunk_size: u32,
    /// Logical (decompressed) disc image size.
    pub iso_file_size: u64,
    /// Actual size of this RVZ (or WIA) file on disk.
    pub rvz_file_size: u64,
    pub partition_entry_count: u32,
    pub raw_data_entry_count: u32,
    pub group_entry_count: u32,
    /// The disc's own header, embedded directly in WIAHeader2 (0x80 bytes) --
    /// contains the game ID, maker code, disc/version numbers, and title at their
    /// normal GC/Wii disc header offsets, readable without decompressing anything else.
    pub disc_header: [u8; 0x80],
}

impl Default for RvzMetadata {
    fn default() -> Self {
        Self {
            disc_type: RvzDiscType::default(),
            compression: DiscCompression::None,
            compression_level: 0,
            chunk_size: 0,
            iso_file_size: 0,
            rvz_file_size: 0,
            partition_entry_count: 0,
            raw_data_entry_count: 0,
            group_entry_count: 0,
            disc_header: [0u8; 0x80],
        }
    }
}

// ---------------------------------------------------------------------------
// ZIP metadata -- read from the central directory (minizip-ng's mz_zip_file), never
// from entries' compressed data. See
// external/dolphin/Externals/minizip-ng/minizip-ng/mz.h.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ZipCompressionMethod {
    Store,
    Deflate,
    Bzip2,
    Lzma,
    Zstd,
    Xz,
    /// WinZip AES encryption wrapper -- the entry's *actual* compression method is
    /// stored inside the AES extra field, not directly in this field.
    Aes,
    /// Any method code minizip-ng doesn't have a named constant for.
    Other(u16),
}

#[derive(Debug, Clone)]
pub struct ZipEntryMetadata {
    pub filename: String,
    pub compression_method: ZipCompressionMethod,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
    pub crc32: u32,
    pub is_directory: bool,
    /// General purpose bit flag's encryption bit, or an AES extra field present.
    pub is_encrypted: bool,
    /// Zip64 extension in use for this entry (needed when size/offset exceed 32-bit
    /// limits).
    pub is_zip64: bool,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ZipMetadata {
    pub entries: Vec<ZipEntryMetadata>,
    /// The archive's global comment, if any.
    pub comment: Option<String>,
}

// ---------------------------------------------------------------------------
// 7z metadata -- read from the archive's header database (7-Zip's CSzArEx/CSzFolder),
// never from folder payloads. See external/7-Zip-zstd/C/7z.h.
// ---------------------------------------------------------------------------

/// One coder (compression/filter stage) within a solid block, in application order --
/// a folder with more than one entry here means chained codecs (e.g. a BCJ filter
/// followed by LZMA).
#[derive(Debug, Clone)]
pub struct SevenZipCoderMetadata {
    pub codec: SevenZipCodecs,
}

/// One solid block ("folder" in 7z terminology) -- a unit of files compressed
/// together. A folder containing more than one file means those files are stored
/// solid (compressed as one stream; extracting any one of them requires decompressing
/// from the start of the folder).
#[derive(Debug, Clone)]
pub struct SevenZipFolderMetadata {
    pub coders: Vec<SevenZipCoderMetadata>,
    /// Total compressed (on-disk) size of this folder's pack streams.
    pub packed_size: u64,
    /// Total decompressed size of this folder's output.
    pub unpacked_size: u64,
    /// CRC-32 of the folder's decompressed output, if the archive stored one.
    pub crc32: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct SevenZipEntryMetadata {
    pub filename: String,
    pub is_directory: bool,
    pub uncompressed_size: u64,
    /// Per-file CRC-32, if the archive stored one (always present for non-empty,
    /// non-directory files in a normally-created 7z; absent is legal for empty files).
    pub crc32: Option<u32>,
    /// Which folder (solid block) this file's data lives in. `None` for empty files,
    /// which don't occupy any folder.
    pub folder_index: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct SevenZipMetadata {
    pub entries: Vec<SevenZipEntryMetadata>,
    pub folders: Vec<SevenZipFolderMetadata>,
    /// True if any folder groups more than one file together (solid archiving).
    pub is_solid: bool,
}

/// Base ROM identity metadata, produced by `detect::read_metadata_for_input` (see
/// `src/rom/detect/mod.rs`). Fields are `Option`-tolerant where old_archiver.rs's
/// design (and the ported detection logic) genuinely needs to represent "not yet known" /
/// "not found" distinctly from an empty match -- see docs/plan for the 4 detection modes
/// this backs.
#[derive(Debug, Clone, Default)]
pub struct RomMetadata {
    /// Candidate platforms narrowed by extension/path heuristics, before (or in lieu of)
    /// the deep cycle. A single-element list after modes 1-3 resolve unambiguously; the
    /// full narrowed list going into mode 4 otherwise.
    pub candidates: Vec<Platform>,
    pub platform: Platform,
    pub reason: RomReason,
    pub reason_string: String,
    pub is_archival: bool,
    pub title: Option<String>,
    pub serial_product_id: Option<String>, // Serial or product ID or any non-title identifier
    pub media_type: MediaType,
    /// Primary/first compression codec name, for display (e.g. "zstd").
    pub codec: Option<String>,
    /// Full codec list -- some archive formats (CHD) can use up to 4 in fallback order.
    pub codecs: Vec<String>,
    pub file_size: Option<u64>,
    /// Whether the dump carries a format-specific header on top of the raw cartridge image
    /// -- an iNES/UNIF wrapper for NES/Famicom, a 512-byte copier header for Super Famicom.
    /// `None` for every platform without such a concept (most of them).
    pub headered: Option<bool>,
    /// Whether the dump's cartridge-specific encryption layer is still applied -- a raw
    /// retail 3DS NCCH's AES-CTR encryption, an NDS/DSi cart's KEY1-encrypted secure area.
    /// `None` for every platform without such a concept.
    pub encrypted: Option<bool>,
    /// Byte order the header was found in. Only N64 dumps circulate in more than one
    /// (`.z64` big-endian, `.v64` byte-swapped, `.n64` word-swapped/little-endian); `None`
    /// for every other platform.
    pub byte_order: Option<RomByteOrder>,
    /// Whether this dump's whole-content hash matched a known game in a platform's own
    /// hash database (currently only NES/Famicom, via `convert::nes::FcDb`/`fcdb.bin` -- see
    /// `platforms::nintendo::read_nes_container_metadata`). `None` for every platform
    /// without such a database. `Some(true)` is strong evidence beyond whatever
    /// shape/magic check the format allows; for NES/Famicom specifically a hash match can
    /// even be *the* reason `headered` above was determined at all, when the file's own
    /// shape didn't otherwise resolve it (see that function's doc comment).
    pub hash_confirmed: Option<bool>,
    /// Capped audit trail of how detection arrived at this result -- see
    /// `detect::reason::DETECTION_TRAIL_CAP`.
    pub detection_trail: Vec<DetectionStep>,
    pub rvz_metadata: RvzMetadata,
    pub chd_metadata: ChdMetadata,
    pub zip_metadata: ZipMetadata,
    pub seven_zip_metadata: SevenZipMetadata,
}

/// See [`RomMetadata::byte_order`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RomByteOrder {
    BigEndian,
    ByteSwapped,
    LittleEndian,
}

impl Default for RomReason {
    fn default() -> Self {
        Self::DeepCycle
    }
}

/// Appends one entry to `meta.detection_trail` -- ported from `reference/old_archiver.rs`'s
/// `push_detection_reason` (line 2508), retargeted at the new `DetectionStep`/
/// `DetectionStepKind` shape (see `detect::reason`). Truncation to
/// [`DETECTION_TRAIL_CAP`] happens once, in [`normalize_metadata`], not on every push --
/// matches the old code's own timing.
pub(crate) fn push_detection_reason(meta: &mut RomMetadata, kind: DetectionStepKind, evidence: Evidence) {
    meta.detection_trail.push(DetectionStep::new(kind, evidence));
}

/// Ranks a [`RomMetadata`] candidate by how much it actually found -- ported verbatim from
/// `reference/old_archiver.rs`'s `metadata_score` (line 6669), used to pick the best result
/// among several candidate reads (e.g. `disc_metadata_from_descriptor_payloads` trying
/// multiple `.cue`/`.gdi` member payloads).
pub(crate) fn metadata_score(meta: &RomMetadata) -> usize {
    let mut score = 0usize;
    if meta.serial_product_id.is_some() {
        score += 4;
    }
    if meta.title.is_some() {
        score += 2;
    }
    if meta.platform != Platform::Unknown {
        score += 1;
    }
    if meta.media_type != MediaType::Unknown {
        score += 1;
    }
    score
}

/// Strips control characters and collapses whitespace -- ported verbatim from
/// `reference/old_archiver.rs`'s `sanitize_text_opt` (line 6806).
pub(crate) fn sanitize_text_opt(value: Option<String>) -> Option<String> {
    let value = value?;
    let cleaned = value
        .chars()
        .filter_map(|ch| {
            if ch == '\0' {
                None
            } else if ch.is_control() {
                Some(' ')
            } else {
                Some(ch)
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    if cleaned.is_empty() { None } else { Some(cleaned) }
}

/// Rejects common false-positive title matches (track/index listing artifacts) on top of
/// [`sanitize_text_opt`] -- ported verbatim from `reference/old_archiver.rs`'s
/// `sanitize_title_opt` (line 6794).
pub(crate) fn sanitize_title_opt(value: Option<String>) -> Option<String> {
    let value = sanitize_text_opt(value)?;
    let upper = value.to_ascii_uppercase();
    if upper.contains(",0,0") {
        return None;
    }
    if upper.starts_with("FILE ") || upper.starts_with("TRACK ") || upper.starts_with("INDEX ") {
        return None;
    }
    Some(value)
}

/// Rejects common false-positive serial matches -- ported verbatim from
/// `reference/old_archiver.rs`'s `sanitize_serial_opt` (line 6831).
pub(crate) fn sanitize_serial_opt(value: Option<String>) -> Option<String> {
    let value = sanitize_text_opt(value)?;
    let upper = value.to_ascii_uppercase();
    if matches!(upper.as_str(), "FILE" | "TRACK" | "INDEX" | "IND") {
        return None;
    }
    if upper.contains(",0,0") {
        return None;
    }
    Some(value)
}

/// Final normalize/sanitize pass applied to every [`RomMetadata`] before it's returned from
/// detection -- ported verbatim from `reference/old_archiver.rs`'s `normalize_metadata`
/// (line 6767), retargeted at the new field names (`candidates` for `platform_candidates`,
/// `detection_trail` for `detection_reasons`).
pub(crate) fn normalize_metadata(mut meta: RomMetadata) -> RomMetadata {
    meta.title = sanitize_title_opt(meta.title);
    meta.serial_product_id = sanitize_serial_opt(meta.serial_product_id);

    if meta.candidates.is_empty() && meta.platform != Platform::Unknown {
        meta.candidates = vec![meta.platform];
    }

    if meta.codecs.is_empty() {
        if let Some(codec) = meta.codec.clone() {
            meta.codecs.push(codec);
        }
    }
    meta.codecs.dedup();
    if meta.codec.is_none() {
        meta.codec = meta.codecs.first().cloned();
    }

    if meta.detection_trail.len() > DETECTION_TRAIL_CAP {
        meta.detection_trail.truncate(DETECTION_TRAIL_CAP);
    }

    meta
}


