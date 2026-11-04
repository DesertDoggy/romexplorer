//! Audio inside ROMs: finding it ([`index_rom`]) and playing it where it sits
//! ([`open_track`]). Nothing is extracted or copied: a track is recorded by where it is
//! (a CD audio track's number, or a file's path on the disc plus a subsong), and opened
//! again from the ROM each time it plays.
//!
//! What can be opened:
//!
//! | ROM | how |
//! |---|---|
//! | CHD (CD, GD-ROM) | every track through [`CdImageReader`]: audio tracks as CD audio, data tracks as filesystems |
//! | CHD (DVD) | the image, as a filesystem |
//! | cue/bin, gdi | the same, from the track files the sheet names |
//! | ISO, BIN, IMG, GCM, XISO | the image, as a filesystem |
//! | RVZ, WIA, GCZ | the image through Dolphin's reader (GameCube; Wii is encrypted, see [`disc_files`]) |
//! | CSO, ZSO | the ISO inside |
//! | a folder (PS3, Vita, Wii U dumps) | its files |
//!
//! Files are handed to vgmstream ([`super::vgmstream`]) by extension first, then by content:
//! whatever it opens is a track, and a bank holding many sounds gives one track per subsong.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::chd_cd::{cd_layout, CdImageReader};
use super::chdman_wrapper::{ChdReader, CD_TRACK_AUDIO};
use super::detect::sheet::{CueSheetOptions, Sheet, SheetKind};
use super::disc_files::{self, DiscFile, DiscView};
use super::inner_archives;
use super::source::ReadAt;
use super::vgmstream::{self, PlayConfig, Stream, StreamInfo, VgmSource};

/// CD audio: 44.1 kHz, stereo, 16-bit, 2352 bytes a frame.
pub const CDDA_RATE: u32 = 44_100;
pub const CDDA_FRAME: u64 = 2352;
const CDDA_BYTES_PER_SECOND: u64 = 44_100 * 4;

/// What a sound is for, as far as its length and loop tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SoundKind {
    Bgm,
    Jingle,
    Se,
}

impl SoundKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Bgm => "bgm",
            Self::Jingle => "jingle",
            Self::Se => "se",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "bgm" => Some(Self::Bgm),
            "jingle" => Some(Self::Jingle),
            "se" => Some(Self::Se),
            _ => None,
        }
    }
}

/// The lengths [`classify`] decides by, in milliseconds. Settings, in seconds
/// (`audio_rom_loop_min_s`, `audio_rom_bgm_min_s`, `audio_rom_jingle_min_s`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thresholds {
    /// A looping sound this long or longer is BGM.
    pub loop_min_ms: u64,
    /// A sound that does not loop is BGM from this length.
    pub bgm_min_ms: u64,
    /// ...and a jingle from this one.
    pub jingle_min_ms: u64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self { loop_min_ms: 3_000, bgm_min_ms: 60_000, jingle_min_ms: 8_000 }
    }
}

/// Words in a path that say what a sound is. Matched against whole path components and
/// their `_`/`-`/digit-separated parts, case-insensitively.
const BGM_WORDS: [&str; 5] = ["bgm", "music", "mus", "song", "bgms"];
/// "sound" and "sounds" are not here: games name the folder holding all their audio, music
/// included, that way.
const SE_WORDS: [&str; 3] = ["se", "sfx", "snd"];
/// Jingles by name: "me" is the "music effect" (fanfare, short tune) of many games' file names.
const JINGLE_WORDS: [&str; 5] = ["me", "jingle", "jingles", "fanfare", "fanfares"];
/// A jingle by name still has to be this long; shorter is a click or silence.
const NAMED_JINGLE_MIN_MS: u64 = 1_000;
/// Speech is never music, however long it runs.
const VOICE_WORDS: [&str; 5] = ["voice", "vo", "voices", "vox", "speech"];
/// A stereo sound that does not loop and runs this many times the BGM length is music,
/// whatever an SE folder says: the words only settle sounds near a line.
const LONG_BGM_FACTOR: u64 = 3;

fn path_words(path: &str) -> Vec<String> {
    path.split(['/', '\\', '_', '-', '.', ' '])
        .flat_map(|part| part.split(|c: char| c.is_ascii_digit()))
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

/// What [`classify`] goes by, besides the path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Shape {
    pub looping: bool,
    pub duration_ms: u64,
    /// 0 when not known.
    pub channels: u16,
    /// The CD audio track's number; 0 for a file.
    pub cd_track: u16,
    /// A track of a music rip set, whose length is the ripper's chosen play length.
    pub rip: bool,
}

/// The longest BGM length a CD audio track or a rip-set track is held to.
const CD_AUDIO_BGM_MIN_MS: u64 = 20_000;
/// A disc's first audio track (1, or 2 after the data track) shorter than this is the
/// spoken "this is a game disc" warning a CD player plays, not music.
const CD_WARNING_MAX_MS: u64 = 45_000;

/// BGM, jingle or sound effect, by loop, length and channels; names in the path move a
/// sound that is close to a line (looping but short, or long but not looping) to the side
/// they point to. Speech is a sound effect. A long mono sound that does not loop is more
/// likely speech than music, so only a BGM name makes it BGM. `channels` 0 is unknown.
pub fn classify(sound: &Shape, path: &str, t: &Thresholds) -> SoundKind {
    let Shape { looping, duration_ms, channels, cd_track, rip } = *sound;
    let cd_audio = cd_track > 0;
    if cd_track <= 2 && cd_audio && duration_ms < CD_WARNING_MAX_MS {
        return SoundKind::Se;
    }
    // CD audio tracks are the disc's soundtrack far more often than not, and many run under
    // a minute: they keep the old 20 s line even when the BGM length is set higher. So do
    // rip-set tracks, whose loops a ripper often plays for well under a minute.
    let t = &Thresholds { bgm_min_ms: if cd_audio || rip { t.bgm_min_ms.min(CD_AUDIO_BGM_MIN_MS) } else { t.bgm_min_ms }, ..*t };
    let words = path_words(path);
    let has = |list: &[&str]| words.iter().any(|w| list.contains(&w.as_str()));
    let says_bgm = has(&BGM_WORDS);
    if !says_bgm && has(&VOICE_WORDS) {
        return SoundKind::Se;
    }
    if !says_bgm && has(&JINGLE_WORDS) {
        return if duration_ms >= NAMED_JINGLE_MIN_MS { SoundKind::Jingle } else { SoundKind::Se };
    }
    let says_se = !says_bgm && has(&SE_WORDS);
    if looping && duration_ms >= t.loop_min_ms {
        // A looping ambience in an SE folder is not music.
        return if says_se && duration_ms < t.bgm_min_ms { SoundKind::Se } else { SoundKind::Bgm };
    }
    if duration_ms >= t.bgm_min_ms {
        let mono = channels == 1;
        let long = !mono && duration_ms >= t.bgm_min_ms.saturating_mul(LONG_BGM_FACTOR);
        return if (says_se || mono) && !says_bgm && !long { SoundKind::Jingle } else { SoundKind::Bgm };
    }
    if duration_ms >= t.jingle_min_ms {
        return if says_bgm { SoundKind::Bgm } else { SoundKind::Jingle };
    }
    if says_bgm && duration_ms >= t.loop_min_ms {
        return SoundKind::Jingle;
    }
    SoundKind::Se
}

/// Where a track is in its ROM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackLocation {
    /// CD audio track `track` (1-based, as on the disc).
    Cdda { track: u32 },
    /// Subsong `subsong` (0 = the file has none) of the file at `path` in data view `view`
    /// (0 = the first data track, or the whole image; for a folder, always 0).
    File { view: u32, path: String, subsong: u32 },
    /// Track `track` (0-based) of a rip-set file in the game's folder, by its path there
    /// (`<archive>|<member>` inside a zip or 7z); see [`super::rip_sets`].
    Rip { path: String, track: u32 },
}

/// One sound found by [`index_rom`].
#[derive(Debug, Clone, PartialEq)]
pub struct FoundTrack {
    pub location: TrackLocation,
    /// The stream's own name when it has one, else the file name (or "Track N").
    pub name: String,
    pub kind: SoundKind,
    pub sample_rate: u32,
    pub channels: u16,
    pub duration_ms: u64,
    pub looping: bool,
    pub loop_start: u64,
    pub loop_end: u64,
    pub codec: String,
    pub format: String,
    /// The stored size: the whole file for a bank (shared by its subsongs), the track for CD audio.
    pub bytes: u64,
    /// The sound of a video file (a title movie), played only when asked for.
    pub video: bool,
}

/// A ROM opened for reading its audio.
pub struct OpenedRom {
    /// Data tracks, or the whole image, in disc order.
    pub data: Vec<DiscView>,
    /// CD audio tracks: number and the track's PCM (little-endian, as in a `.bin`).
    pub cdda: Vec<(u32, DiscView)>,
    /// A folder dump.
    pub folder: Option<PathBuf>,
}

struct ChdBytes(ChdReader);

impl ReadAt for ChdBytes {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.0.read_bytes(offset, buf).map_err(|e| io::Error::other(e.to_string()))
    }
}

struct DolphinBytes(super::dolphin_wrapper::DiscReader);

impl ReadAt for DolphinBytes {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.0.read(offset, buf).map_err(|e| io::Error::other(e.to_string()))
    }
}

fn whole_file(path: &Path) -> Result<DiscView, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    Ok(DiscView::new(Box::new(file), len))
}

fn extension(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

/// ROM files [`open_rom`] can find audio in: disc images and their sheets, and the DS / 3DS
/// cartridges, which carry a filesystem. Other cartridges hold sequenced music only, and
/// archives are not read yet.
pub const DISC_EXTENSIONS: [&str; 17] = ["chd", "cue", "gdi", "iso", "bin", "img", "gcm", "xiso", "rvz", "wia", "gcz", "cso", "zso", "nds", "dsi", "3ds", "cci"];

/// Whether [`index_rom`] is worth running on `path`: a folder dump or a disc image.
pub fn may_hold_audio(path: &Path) -> bool {
    path.is_dir() || DISC_EXTENSIONS.contains(&extension(path).as_str())
}

/// Opens `path` (a ROM file, a cue/gdi sheet, or a folder dump) for reading its audio.
pub fn open_rom(path: &Path) -> Result<OpenedRom, String> {
    if path.is_dir() {
        return Ok(OpenedRom { data: Vec::new(), cdda: Vec::new(), folder: Some(path.to_path_buf()) });
    }
    match extension(path).as_str() {
        "chd" => open_chd(path),
        "cue" | "gdi" => open_sheet(path),
        "rvz" | "wia" | "gcz" => {
            let reader = super::dolphin_wrapper::DiscReader::open(path, None).map_err(|e| e.to_string())?;
            let size = reader.info().map_err(|e| e.to_string())?.data_size;
            Ok(OpenedRom { data: vec![DiscView::new(Box::new(DolphinBytes(reader)), size)], cdda: Vec::new(), folder: None })
        }
        "cso" | "zso" => {
            let reader = super::cso::CsoReader::open(path).map_err(|e| e.to_string())?;
            let size = reader.size();
            Ok(OpenedRom { data: vec![DiscView::new(Box::new(reader), size)], cdda: Vec::new(), folder: None })
        }
        "zip" | "7z" | "rar" => Err("audio in zip/7z archives is not read yet".into()),
        _ => Ok(OpenedRom { data: vec![whole_file(path)?], cdda: Vec::new(), folder: None }),
    }
}

fn open_chd(path: &Path) -> Result<OpenedRom, String> {
    let reader = ChdReader::open(path, None, 8).map_err(|e| e.to_string())?;
    let info = reader.info().map_err(|e| e.to_string())?;
    if !info.is_cd || info.num_tracks == 0 {
        let size = info.logical_bytes;
        return Ok(OpenedRom { data: vec![DiscView::new(Box::new(ChdBytes(reader)), size)], cdda: Vec::new(), folder: None });
    }
    let metadata = super::chd_info::read_chd_metadata(path).ok();
    let kind = if info.is_gdrom { SheetKind::Gdi } else { SheetKind::Cue };
    let layout = cd_layout(&reader, metadata.as_ref(), "disc", kind, &CueSheetOptions::chdman()).map_err(|e| e.to_string())?;
    let total = layout.total_bytes;
    let tracks: Arc<[_]> = layout.tracks.clone().into();
    let image = DiscView::new(Box::new(CdImageReader::new(reader, tracks)), total);
    let mut out = OpenedRom { data: Vec::new(), cdda: Vec::new(), folder: None };
    let mut high_density = Vec::new();
    for (i, track) in layout.tracks.iter().enumerate() {
        if track.is_empty() {
            continue;
        }
        // A GD-ROM's high-density data tracks make one filesystem; each starts at the
        // sector the sheet gives it.
        let gd_lba = layout.sheet.tracks.get(i).and_then(|t| t.gdi).map(|g| g.lba).filter(|lba| *lba >= GD_HIGH_DENSITY);
        if let (Some(lba), true) = (gd_lba, track.track.track_type != CD_TRACK_AUDIO) {
            high_density.push(GdTrack::new(lba, image.slice(track.start, track.len()), track.frame_bytes));
            continue;
        }
        // From INDEX 01, as a cue sheet's track starts: a pregap kept in the file comes first.
        let skip = if track.pregap_in_file { u64::from(track.pregap) * u64::from(track.frame_bytes) } else { 0 };
        let view = image.slice(track.start, track.len()).slice(skip, u64::MAX);
        if track.track.track_type == CD_TRACK_AUDIO {
            out.cdda.push((track.index + 1, view));
        } else {
            out.data.push(view);
        }
    }
    out.data.extend(gd_area(high_density));
    Ok(out)
}

/// Where a GD-ROM's high-density area starts.
const GD_HIGH_DENSITY: u32 = 45_000;

/// One data track of a GD-ROM's high-density area: the sector it starts at on the disc, and
/// how its sectors sit in `view`.
struct GdTrack {
    lba: u64,
    view: DiscView,
    stride: u64,
    user: u64,
}

impl GdTrack {
    fn new(lba: u32, view: DiscView, sector_size: u32) -> Self {
        let (stride, user) = match sector_size {
            2048 => (2048, 0),
            2336 => (2336, 8),
            _ => (2352, 16),
        };
        Self { lba: u64::from(lba), view, stride, user }
    }

    fn sectors(&self) -> u64 {
        self.view.size() / self.stride
    }
}

/// A GD-ROM's high-density data tracks as one image of 2048-byte sectors counted from the
/// start of the disc, as its filesystem counts them: sectors no data track holds (the low-
/// density area, audio tracks, gaps) read as zeros.
struct GdArea(Vec<GdTrack>);

impl ReadAt for GdArea {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        const SECTOR: u64 = 2048;
        let mut done = 0usize;
        while done < buf.len() {
            let pos = offset + done as u64;
            let (lba, within) = (pos / SECTOR, pos % SECTOR);
            let take = ((SECTOR - within) as usize).min(buf.len() - done);
            let out = &mut buf[done..done + take];
            match self.0.iter().find(|t| lba >= t.lba && lba - t.lba < t.sectors()) {
                Some(t) => t.view.read(t.user + (lba - t.lba) * t.stride + within, out)?,
                None => out.fill(0),
            }
            done += take;
        }
        Ok(())
    }
}

fn gd_area(tracks: Vec<GdTrack>) -> Option<DiscView> {
    let end = tracks.iter().map(|t| t.lba + t.sectors()).max()?;
    Some(DiscView::new(Box::new(GdArea(tracks)), end * 2048))
}

fn open_sheet(path: &Path) -> Result<OpenedRom, String> {
    let sheet = Sheet::parse(path)?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut out = OpenedRom { data: Vec::new(), cdda: Vec::new(), folder: None };
    let mut opened: std::collections::HashMap<String, DiscView> = std::collections::HashMap::new();
    let mut high_density = Vec::new();
    for (i, track) in sheet.tracks.iter().enumerate() {
        let file = match opened.get(&track.file_name) {
            Some(v) => v.clone(),
            None => {
                let v = whole_file(&dir.join(&track.file_name))?;
                opened.insert(track.file_name.clone(), v.clone());
                v
            }
        };
        let audio = track.gdi.map(|g| g.kind == 0).unwrap_or(false) || matches!(track.track_type, super::models::ChdTrackType::Audio);
        // A gdi gives each track a file of its own, starting at the track's first sector;
        // the number in it is where the track is on the disc, not in the file.
        if let Some(g) = track.gdi {
            let view = file.slice(g.offset, u64::MAX);
            if audio {
                out.cdda.push((track.number, view));
            } else if g.lba >= GD_HIGH_DENSITY {
                high_density.push(GdTrack::new(g.lba, view, g.sector_size));
            } else {
                out.data.push(view);
            }
            continue;
        }
        let sector = u64::from(track.sector_size.max(1));
        let start = u64::from(track.index1) * sector;
        // The track runs to where the next track in the same file starts, or the file's end.
        let end = sheet.tracks[i + 1..]
            .iter()
            .find(|t| t.file_name == track.file_name)
            .map(|t| u64::from(t.index0.unwrap_or(t.index1)) * sector)
            .unwrap_or(file.size());
        if end <= start {
            continue;
        }
        let view = file.slice(start, end - start);
        if audio {
            out.cdda.push((track.number, view));
        } else {
            out.data.push(view);
        }
    }
    out.data.extend(gd_area(high_density));
    Ok(out)
}

// --- vgmstream sources over disc files and folders ------------------------------------

/// A file on a disc, for vgmstream; reaches the files beside it through `listing`.
#[derive(Clone)]
struct DiscFileSource {
    view: DiscView,
    listing: Arc<Vec<DiscFile>>,
    file: DiscFile,
}

fn find_named<'a>(listing: &'a [DiscFile], current: &DiscFile, name: &str) -> Option<&'a DiscFile> {
    let wanted = name.replace('\\', "/");
    let wanted = wanted.trim_start_matches('/');
    if let Some(f) = listing.iter().find(|f| f.path.eq_ignore_ascii_case(wanted)) {
        return Some(f);
    }
    // vgmstream builds names from the one it was given; failing a full match, the same
    // file name in the same directory.
    let file_name = vgmstream::file_name_of(wanted);
    listing.iter().find(|f| f.dir().eq_ignore_ascii_case(current.dir()) && f.name().eq_ignore_ascii_case(file_name))
}

impl VgmSource for DiscFileSource {
    fn read_at(&mut self, offset: u64, dst: &mut [u8]) -> usize {
        self.file.read(&self.view, offset, dst).unwrap_or(0)
    }

    fn size(&self) -> u64 {
        self.file.size
    }

    fn open(&self, name: &str) -> Option<Box<dyn VgmSource>> {
        let file = find_named(&self.listing, &self.file, name)?.clone();
        Some(Box::new(DiscFileSource { view: self.view.clone(), listing: self.listing.clone(), file }))
    }
}

struct FolderSource {
    root: PathBuf,
    rel: String,
    file: std::fs::File,
    size: u64,
}

impl FolderSource {
    fn open_rel(root: &Path, rel: &str) -> Option<Self> {
        let rel = rel.replace('\\', "/");
        let rel = rel.trim_start_matches('/').to_string();
        if rel.split('/').any(|c| c == "..") {
            return None;
        }
        let file = std::fs::File::open(root.join(&rel)).ok()?;
        let size = file.metadata().ok()?.len();
        Some(Self { root: root.to_path_buf(), rel, file, size })
    }
}

impl VgmSource for FolderSource {
    fn read_at(&mut self, offset: u64, dst: &mut [u8]) -> usize {
        if offset >= self.size {
            return 0;
        }
        let n = dst.len().min((self.size - offset) as usize);
        self.file.read_at(offset, &mut dst[..n]).map(|_| n).unwrap_or(0)
    }

    fn size(&self) -> u64 {
        self.size
    }

    fn open(&self, name: &str) -> Option<Box<dyn VgmSource>> {
        let wanted = name.replace('\\', "/");
        if let Some(found) = Self::open_rel(&self.root, &wanted) {
            return Some(Box::new(found));
        }
        // Case differs between what a game asks for and what a dump on a case-sensitive
        // filesystem holds: look in this file's directory.
        let dir = self.rel.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        let file_name = vgmstream::file_name_of(&wanted);
        let entries = std::fs::read_dir(self.root.join(dir)).ok()?;
        let hit = entries.flatten().find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(file_name))?;
        let rel = if dir.is_empty() { hit.file_name().to_string_lossy().into_owned() } else { format!("{dir}/{}", hit.file_name().to_string_lossy()) };
        Self::open_rel(&self.root, &rel).map(|s| Box::new(s) as Box<dyn VgmSource>)
    }
}

/// Companion files (a bank's cue names, its header beside the data) read whole into memory
/// once per ROM and shared by every open: a bank of thousands of subsongs opens them for
/// each one, and they are read in scattered pieces.
#[derive(Clone, Default)]
struct Companions(Arc<std::sync::Mutex<HashMap<String, Arc<Vec<u8>>>>>);

/// Companions larger than this are read where they are.
const COMPANION_MAX_BYTES: u64 = 16 << 20;

/// A source whose companion files come from `cache`, or with none at all (`alone`).
struct Shared {
    inner: Box<dyn VgmSource>,
    name: String,
    cache: Companions,
    alone: bool,
}

impl VgmSource for Shared {
    fn read_at(&mut self, offset: u64, dst: &mut [u8]) -> usize {
        self.inner.read_at(offset, dst)
    }

    fn size(&self) -> u64 {
        self.inner.size()
    }

    fn open(&self, name: &str) -> Option<Box<dyn VgmSource>> {
        let wanted = vgmstream::file_name_of(name);
        if wanted.eq_ignore_ascii_case(vgmstream::file_name_of(&self.name)) {
            let inner = self.inner.open(name)?;
            return Some(Box::new(Shared { inner, name: self.name.clone(), cache: self.cache.clone(), alone: self.alone }));
        }
        if self.alone {
            return None;
        }
        let key = name.replace('\\', "/").to_ascii_lowercase();
        if let Some(bytes) = self.cache.0.lock().ok()?.get(&key).cloned() {
            return Some(Box::new(vgmstream::MemorySource { name: name.to_string(), bytes, siblings: Default::default() }));
        }
        let mut file = self.inner.open(name)?;
        let size = file.size();
        if size > COMPANION_MAX_BYTES {
            return Some(file);
        }
        let mut bytes = vec![0u8; size as usize];
        let mut done = 0;
        while done < bytes.len() {
            let n = file.read_at(done as u64, &mut bytes[done..]);
            if n == 0 {
                break;
            }
            done += n;
        }
        bytes.truncate(done);
        let bytes = Arc::new(bytes);
        if let Ok(mut cache) = self.cache.0.lock() {
            cache.insert(key, bytes.clone());
        }
        Some(Box::new(vgmstream::MemorySource { name: name.to_string(), bytes, siblings: Default::default() }))
    }
}

/// Video files: their sound is a film's mix of voice, effects and music. Indexed, marked
/// `video`, and played only when `audio_rom_videos` or the title's own choice asks for it.
const VIDEO_EXTENSIONS: [&str; 17] = ["xmv", "sfd", "bik", "bk2", "usm", "str", "pss", "pmf", "mps", "wmv", "avi", "mp4", "mov", "m2v", "thp", "mods", "vid"];

pub fn is_video(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, ext)| VIDEO_EXTENSIONS.iter().any(|v| ext.eq_ignore_ascii_case(v)))
}

fn folder_files(root: &Path) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new(), 0usize)];
    while let Some((dir, prefix, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() && depth < 32 {
                stack.push((entry.path(), rel, depth + 1));
            } else if kind.is_file() {
                out.push((rel, entry.metadata().map(|m| m.len()).unwrap_or(0)));
            }
        }
    }
    out.sort();
    out
}

// --- indexing --------------------------------------------------------------------------

/// Banks with more sounds than this are cut off: past it a "bank" is a whole sound effects
/// library, and a title page has no use for its two-thousandth footstep.
const MAX_SUBSONGS: u32 = 4096;
/// Smaller than any real stream header plus a frame.
const MIN_FILE_BYTES: u64 = 0x40;

fn found(location: TrackLocation, name: String, info: &StreamInfo, bytes: u64, path_for_words: &str, t: &Thresholds) -> FoundTrack {
    let duration_ms = info.duration_ms();
    let inner = location_path(&location);
    let video = is_video(inner_archives::split(inner).map_or(inner, |(base, _)| base));
    let shape = Shape { looping: info.looping, duration_ms, channels: info.channels, cd_track: match location {
        TrackLocation::Cdda { track } => u16::try_from(track).unwrap_or(u16::MAX),
        TrackLocation::File { .. } | TrackLocation::Rip { .. } => 0,
    }, rip: false };
    FoundTrack {
        location,
        name,
        kind: classify(&shape, path_for_words, t),
        sample_rate: info.sample_rate,
        channels: info.channels,
        duration_ms,
        looping: info.looping,
        loop_start: info.loop_start,
        loop_end: info.loop_end,
        codec: info.codec.clone(),
        format: info.meta.clone(),
        bytes,
        video,
    }
}

fn location_path(location: &TrackLocation) -> &str {
    match location {
        TrackLocation::File { path, .. } | TrackLocation::Rip { path, .. } => path,
        TrackLocation::Cdda { .. } => "",
    }
}

/// Banks with more subsongs than this are read without their companion files: those only
/// name the sounds, and parsing a cue file of thousands of names again for every subsong
/// is most of the time such a bank takes. Their sounds are numbered instead.
const NAMED_SUBSONGS: u32 = 512;

/// Every sound in one file, one per subsong. `open(alone)` gives a fresh reader of the
/// file; `alone` leaves out its companion files.
fn index_file(open: &dyn Fn(bool) -> Box<dyn VgmSource>, path: &str, size: u64, view: u32, t: &Thresholds, out: &mut Vec<FoundTrack>) {
    let Some(first) = Stream::open(open(false), path, 0, PlayConfig::AS_STORED) else { return };
    let count = first.info().subsong_count;
    let file_name = vgmstream::file_name_of(path).to_string();
    let display = |info: &StreamInfo| if info.stream_name.is_empty() { file_name.clone() } else { info.stream_name.clone() };
    if count <= 1 {
        let info = first.info().clone();
        let subsong = if count == 1 { 1 } else { 0 };
        out.push(found(TrackLocation::File { view, path: path.to_string(), subsong }, display(&info), &info, size, path, t));
        return;
    }
    drop(first);
    let alone = count > NAMED_SUBSONGS;
    // Banks sometimes point two entries at the same sound: subsongs alike in length, loop
    // and format are compared by their first samples, and a repeat is left out.
    let mut alike: HashMap<AlikeKey, Vec<(u32, Option<u64>)>> = HashMap::new();
    for subsong in 1..=count.min(MAX_SUBSONGS) {
        let Some(mut stream) = Stream::open(open(alone), path, subsong, PlayConfig::AS_STORED) else { continue };
        let info = stream.info().clone();
        let key = (info.stream_samples, info.loop_start, info.loop_end, info.looping, info.channels, info.sample_rate, info.codec.clone());
        let earlier = alike.entry(key).or_default();
        if !earlier.is_empty() {
            let mine = first_samples_hash(&mut stream);
            let mut repeat = false;
            for (other, hash) in earlier.iter_mut() {
                if hash.is_none() {
                    *hash = Stream::open(open(true), path, *other, PlayConfig::AS_STORED).map(|mut s| first_samples_hash(&mut s));
                }
                if *hash == Some(mine) {
                    repeat = true;
                    break;
                }
            }
            if repeat {
                continue;
            }
            earlier.push((subsong, Some(mine)));
        } else {
            earlier.push((subsong, None));
        }
        let words = format!("{path}/{}", info.stream_name);
        let name = if info.stream_name.is_empty() { format!("{file_name} #{subsong}") } else { info.stream_name.clone() };
        out.push(found(TrackLocation::File { view, path: path.to_string(), subsong }, name, &info, size, &words, t));
    }
}

/// Length, loop and format: subsongs that share these may be the same sound.
type AlikeKey = (u64, u64, u64, bool, u16, u32, String);

/// A hash of a stream's first second or so of samples: enough to tell two sounds apart.
fn first_samples_hash(stream: &mut Stream) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut taken = 0usize;
    while taken < 96_000 {
        match stream.render() {
            Ok(Some(chunk)) => {
                chunk.hash(&mut h);
                taken += chunk.len();
            }
            _ => break,
        }
    }
    h.finish()
}

/// Every playable member of the archive at `path` (AFS, SDAT, RARC/U8, CPK), as
/// `<path>#<index>.<ext>`, named by the member's own name where the archive has one.
fn index_members(open: &dyn Fn() -> Box<dyn VgmSource>, path: &str, kind: inner_archives::Kind, view: u32, t: &Thresholds, out: &mut Vec<FoundTrack>) {
    for m in inner_archives::members(open().as_mut(), kind) {
        let member = inner_archives::member_path(path, &m);
        let source = |_alone| inner_archives::open_member(open(), path, &member, &m);
        let from = out.len();
        index_file(&source, &member, m.size(), view, t, out);
        if let Some(name) = &m.name {
            let own = vgmstream::file_name_of(name);
            let numbered = vgmstream::file_name_of(&member).to_string();
            for track in &mut out[from..] {
                if let Some(rest) = track.name.strip_prefix(&numbered) {
                    track.name = format!("{own}{rest}");
                    // Its own name says more than the number did; the player classifies by both.
                    let shape = Shape { looping: track.looping, duration_ms: track.duration_ms, channels: track.channels, cd_track: 0, rip: false };
                    track.kind = classify(&shape, &format!("{member}/{}", track.name), t);
                }
            }
        }
    }
}

/// Finds every sound in the ROM at `path`. `progress` is told what is being looked at.
/// A ROM with nothing vgmstream reads gives an empty list, not an error.
pub fn index_rom(path: &Path, t: &Thresholds, mut progress: impl FnMut(&str)) -> Result<Vec<FoundTrack>, String> {
    if !vgmstream::available() {
        return Err("the game audio decoder (rvgmstream) is not built for this machine".into());
    }
    let rom = open_rom(path)?;
    let mut out = Vec::new();
    for (number, view) in &rom.cdda {
        let bytes = view.size();
        let duration_ms = bytes * 1000 / CDDA_BYTES_PER_SECOND;
        if duration_ms < 1000 {
            continue;
        }
        let info = StreamInfo { channels: 2, sample_rate: CDDA_RATE, stream_samples: bytes / 4, codec: "CD audio".into(), meta: "CD-DA".into(), ..Default::default() };
        out.push(found(TrackLocation::Cdda { track: *number }, format!("Track {number:02}"), &info, bytes, "", t));
    }
    for (n, view) in rom.data.iter().enumerate() {
        let listing = match disc_files::list(view) {
            Ok(listing) => Arc::new(listing.files),
            Err(e) => {
                // A disc of CD audio has data tracks a game never reads audio from; only the
                // first data view not being readable is worth saying.
                if n == 0 && rom.cdda.is_empty() {
                    return Err(e);
                }
                continue;
            }
        };
        let cache = Companions::default();
        for file in listing.iter() {
            if file.size < MIN_FILE_BYTES {
                continue;
            }
            let source = DiscFileSource { view: view.clone(), listing: listing.clone(), file: file.clone() };
            if let Some(kind) = inner_archives::kind_of(&file.path) {
                progress(&file.path);
                index_members(&|| Box::new(source.clone()), &file.path, kind, n as u32, t, &mut out);
                continue;
            }
            if !vgmstream::known_extension(file.name()) {
                continue;
            }
            progress(&file.path);
            let shared = |alone| Box::new(Shared { inner: Box::new(source.clone()), name: file.path.clone(), cache: cache.clone(), alone }) as Box<dyn VgmSource>;
            index_file(&shared, &file.path, file.size, n as u32, t, &mut out);
        }
    }
    if let Some(root) = &rom.folder {
        let cache = Companions::default();
        for (rel, size) in folder_files(root) {
            let name = vgmstream::file_name_of(&rel);
            if size < MIN_FILE_BYTES {
                continue;
            }
            let file = || FolderSource::open_rel(root, &rel).map(|s| Box::new(s) as Box<dyn VgmSource>).unwrap_or_else(|| Box::new(vgmstream::MemorySource::new("missing", Vec::new())));
            if let Some(kind) = inner_archives::kind_of(&rel) {
                progress(&rel);
                index_members(&file, &rel, kind, 0, t, &mut out);
                continue;
            }
            if !vgmstream::known_extension(name) {
                continue;
            }
            progress(&rel);
            let shared = |alone| Box::new(Shared { inner: file(), name: rel.clone(), cache: cache.clone(), alone }) as Box<dyn VgmSource>;
            index_file(&shared, &rel, size, 0, t, &mut out);
        }
    }
    Ok(out)
}

// --- playing ---------------------------------------------------------------------------

/// Decoded audio coming out of a ROM, a run of interleaved 16-bit samples at a time.
pub trait PcmStream: Send {
    fn sample_rate(&self) -> u32;
    fn channels(&self) -> u16;
    /// Samples per channel this will produce in total, when known.
    fn total_samples(&self) -> Option<u64>;
    fn next_chunk(&mut self) -> Result<Option<Vec<i16>>, String>;
}

struct CddaStream {
    view: DiscView,
    pos: u64,
}

impl PcmStream for CddaStream {
    fn sample_rate(&self) -> u32 {
        CDDA_RATE
    }
    fn channels(&self) -> u16 {
        2
    }
    fn total_samples(&self) -> Option<u64> {
        Some(self.view.size() / 4)
    }
    fn next_chunk(&mut self) -> Result<Option<Vec<i16>>, String> {
        let left = self.view.size().saturating_sub(self.pos) / 4 * 4;
        if left == 0 {
            return Ok(None);
        }
        let take = left.min(CDDA_FRAME * 32);
        let mut buf = vec![0u8; take as usize];
        self.view.read(self.pos, &mut buf).map_err(|e| e.to_string())?;
        self.pos += take;
        Ok(Some(buf.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect()))
    }
}

struct VgmPcm(Stream);

impl PcmStream for VgmPcm {
    fn sample_rate(&self) -> u32 {
        self.0.info().sample_rate
    }
    fn channels(&self) -> u16 {
        self.0.info().channels
    }
    fn total_samples(&self) -> Option<u64> {
        Some(self.0.info().play_samples)
    }
    fn next_chunk(&mut self) -> Result<Option<Vec<i16>>, String> {
        Ok(self.0.render()?.map(<[i16]>::to_vec))
    }
}

/// Opens the track at `location` in the ROM at `path` for playing, with `config`'s loops.
/// For a [`TrackLocation::Rip`] `path` is the game's folder, not the ROM.
pub fn open_track(path: &Path, location: &TrackLocation, config: PlayConfig) -> Result<Box<dyn PcmStream>, String> {
    if let TrackLocation::Rip { path: rel, track } = location {
        return super::rip_sets::open(path, rel, *track);
    }
    let rom = open_rom(path)?;
    match location {
        TrackLocation::Cdda { track } => {
            let view = rom.cdda.iter().find(|(n, _)| n == track).map(|(_, v)| v.clone()).ok_or_else(|| format!("the ROM has no audio track {track}"))?;
            Ok(Box::new(CddaStream { view, pos: 0 }))
        }
        TrackLocation::File { view, path: inner, subsong } => {
            let member = inner_archives::split(inner);
            let file_path = member.map_or(inner.as_str(), |(base, _)| base);
            let mut source: Box<dyn VgmSource> = if let Some(root) = &rom.folder {
                Box::new(FolderSource::open_rel(root, file_path).ok_or_else(|| format!("{file_path} is not in the folder"))?)
            } else {
                let disc = rom.data.get(*view as usize).ok_or_else(|| format!("the ROM has no data track {}", view + 1))?;
                let listing = Arc::new(disc_files::list(disc)?.files);
                let file = listing.iter().find(|f| f.path == file_path).cloned().ok_or_else(|| format!("{file_path} is not on the disc"))?;
                Box::new(DiscFileSource { view: disc.clone(), listing, file })
            };
            if let Some((base, index)) = member {
                let kind = inner_archives::kind_of(base).ok_or_else(|| format!("{base} is not an archive"))?;
                let m = inner_archives::members(source.as_mut(), kind).into_iter().find(|m| m.index == index).ok_or_else(|| format!("{base} has no member {index}"))?;
                source = inner_archives::open_member(source, base, inner, &m);
            }
            let stream = Stream::open(source, inner, *subsong, config).ok_or_else(|| format!("{inner} would not open"))?;
            Ok(Box::new(VgmPcm(stream)))
        }
        TrackLocation::Rip { .. } => unreachable!("handled above"),
    }
}

/// A WAV header for `samples` frames of 16-bit PCM: what the player is sent ahead of the
/// samples, which follow as they decode.
pub fn wav_header(sample_rate: u32, channels: u16, samples: u64) -> Vec<u8> {
    let data = (samples * u64::from(channels) * 2).min(u64::from(u32::MAX - 36)) as u32;
    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(36 + data).to_le_bytes());
    h.extend_from_slice(b"WAVEfmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes());
    h.extend_from_slice(&channels.to_le_bytes());
    h.extend_from_slice(&sample_rate.to_le_bytes());
    h.extend_from_slice(&(sample_rate * u32::from(channels) * 2).to_le_bytes());
    h.extend_from_slice(&(channels * 2).to_le_bytes());
    h.extend_from_slice(&16u16.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data.to_le_bytes());
    h
}
