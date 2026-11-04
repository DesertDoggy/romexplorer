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

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::chd_cd::{cd_layout, CdImageReader};
use super::chdman_wrapper::{ChdReader, CD_TRACK_AUDIO};
use super::detect::sheet::{CueSheetOptions, Sheet, SheetKind};
use super::disc_files::{self, DiscFile, DiscView};
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

/// The lengths [`classify`] decides by, in milliseconds. Settings
/// (`audio_rom_loop_min_ms`, `audio_rom_bgm_min_ms`, `audio_rom_jingle_min_ms`).
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
        Self { loop_min_ms: 3_000, bgm_min_ms: 20_000, jingle_min_ms: 8_000 }
    }
}

/// Words in a path that say what a sound is. Matched against whole path components and
/// their `_`/`-`/digit-separated parts, case-insensitively.
const BGM_WORDS: [&str; 7] = ["bgm", "music", "mus", "stream", "strm", "song", "bgms"];
const SE_WORDS: [&str; 8] = ["se", "sfx", "snd", "sound", "voice", "vo", "voices", "sounds"];

fn path_words(path: &str) -> Vec<String> {
    path.split(['/', '\\', '_', '-', '.', ' '])
        .flat_map(|part| part.split(|c: char| c.is_ascii_digit()))
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

/// BGM, jingle or sound effect, by loop and length; names in the path move a sound that is
/// close to a line (looping but short, or long but not looping) to the side they point to.
pub fn classify(looping: bool, duration_ms: u64, path: &str, t: &Thresholds) -> SoundKind {
    let words = path_words(path);
    let says_bgm = words.iter().any(|w| BGM_WORDS.contains(&w.as_str()));
    let says_se = !says_bgm && words.iter().any(|w| SE_WORDS.contains(&w.as_str()));
    if looping && duration_ms >= t.loop_min_ms {
        // A looping voice or ambience loop in an SE folder is not music.
        return if says_se && duration_ms < t.bgm_min_ms { SoundKind::Se } else { SoundKind::Bgm };
    }
    if duration_ms >= t.bgm_min_ms {
        return if says_se { SoundKind::Jingle } else { SoundKind::Bgm };
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
    for track in &layout.tracks {
        if track.is_empty() {
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
    Ok(out)
}

fn open_sheet(path: &Path) -> Result<OpenedRom, String> {
    let sheet = Sheet::parse(path)?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut out = OpenedRom { data: Vec::new(), cdda: Vec::new(), folder: None };
    let mut opened: std::collections::HashMap<String, DiscView> = std::collections::HashMap::new();
    for (i, track) in sheet.tracks.iter().enumerate() {
        let file = match opened.get(&track.file_name) {
            Some(v) => v.clone(),
            None => {
                let v = whole_file(&dir.join(&track.file_name))?;
                opened.insert(track.file_name.clone(), v.clone());
                v
            }
        };
        let sector = u64::from(track.sector_size.max(1));
        let base = track.gdi.map(|g| g.offset).unwrap_or(0);
        let start = base + u64::from(track.index1) * sector;
        // The track runs to where the next track in the same file starts, or the file's end.
        let end = sheet.tracks[i + 1..]
            .iter()
            .find(|t| t.file_name == track.file_name)
            .map(|t| base + u64::from(t.index0.unwrap_or(t.index1)) * sector)
            .unwrap_or(file.size());
        if end <= start {
            continue;
        }
        let view = file.slice(start, end - start);
        let audio = track.gdi.map(|g| g.kind == 0).unwrap_or(false) || matches!(track.track_type, super::models::ChdTrackType::Audio);
        if audio {
            out.cdda.push((track.number, view));
        } else {
            out.data.push(view);
        }
    }
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
    FoundTrack {
        location,
        name,
        kind: classify(info.looping, duration_ms, path_for_words, t),
        sample_rate: info.sample_rate,
        channels: info.channels,
        duration_ms,
        looping: info.looping,
        loop_start: info.loop_start,
        loop_end: info.loop_end,
        codec: info.codec.clone(),
        format: info.meta.clone(),
        bytes,
    }
}

/// Every sound in one file, one per subsong.
fn index_file(open: &dyn Fn() -> Box<dyn VgmSource>, path: &str, size: u64, view: u32, t: &Thresholds, out: &mut Vec<FoundTrack>) {
    let Some(first) = Stream::open(open(), path, 0, PlayConfig::AS_STORED) else { return };
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
    for subsong in 1..=count.min(MAX_SUBSONGS) {
        let Some(stream) = Stream::open(open(), path, subsong, PlayConfig::AS_STORED) else { continue };
        let info = stream.info().clone();
        let words = format!("{path}/{}", info.stream_name);
        out.push(found(TrackLocation::File { view, path: path.to_string(), subsong }, display(&info), &info, size, &words, t));
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
        for file in listing.iter() {
            if file.size < MIN_FILE_BYTES || !vgmstream::known_extension(file.name()) {
                continue;
            }
            progress(&file.path);
            let source = DiscFileSource { view: view.clone(), listing: listing.clone(), file: file.clone() };
            index_file(&|| Box::new(source.clone()), &file.path, file.size, n as u32, t, &mut out);
        }
    }
    if let Some(root) = &rom.folder {
        for (rel, size) in folder_files(root) {
            if size < MIN_FILE_BYTES || !vgmstream::known_extension(vgmstream::file_name_of(&rel)) {
                continue;
            }
            progress(&rel);
            let (root, rel2) = (root.clone(), rel.clone());
            index_file(&move || FolderSource::open_rel(&root, &rel2).map(|s| Box::new(s) as Box<dyn VgmSource>).unwrap_or_else(|| Box::new(vgmstream::MemorySource::new("missing", Vec::new()))), &rel, size, 0, t, &mut out);
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
pub fn open_track(path: &Path, location: &TrackLocation, config: PlayConfig) -> Result<Box<dyn PcmStream>, String> {
    let rom = open_rom(path)?;
    match location {
        TrackLocation::Cdda { track } => {
            let view = rom.cdda.iter().find(|(n, _)| n == track).map(|(_, v)| v.clone()).ok_or_else(|| format!("the ROM has no audio track {track}"))?;
            Ok(Box::new(CddaStream { view, pos: 0 }))
        }
        TrackLocation::File { view, path: inner, subsong } => {
            let source: Box<dyn VgmSource> = if let Some(root) = &rom.folder {
                Box::new(FolderSource::open_rel(root, inner).ok_or_else(|| format!("{inner} is not in the folder"))?)
            } else {
                let disc = rom.data.get(*view as usize).ok_or_else(|| format!("the ROM has no data track {}", view + 1))?;
                let listing = Arc::new(disc_files::list(disc)?.files);
                let file = listing.iter().find(|f| f.path == *inner).cloned().ok_or_else(|| format!("{inner} is not on the disc"))?;
                Box::new(DiscFileSource { view: disc.clone(), listing, file })
            };
            let stream = Stream::open(source, inner, *subsong, config).ok_or_else(|| format!("{inner} would not open"))?;
            Ok(Box::new(VgmPcm(stream)))
        }
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
