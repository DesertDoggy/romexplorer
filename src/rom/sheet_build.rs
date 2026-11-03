//! Rebuilding a `.cue` / `.gdi` for a dump whose tracks are on disk but whose sheet is not.
//!
//! A cue/bin set is the one dump shape where the index is a separate file, so losing it loses
//! the dump: the tracks are still there, but nothing says what they are or what order they go
//! in. This rebuilds that index from the tracks themselves.
//!
//! **It conforms to the CHD path by construction, not by imitation.** `rom::chd_cd` builds a
//! `Vec<SheetTrack>` from a CHD's table of contents and renders it with `Sheet::write`; this
//! builds the same `Vec<SheetTrack>` from files on disk and renders it with the same writer.
//! Nothing here formats a line of cue or gdi text -- `detect::sheet` is the only place that
//! does, and its round-trip tests hold it byte-identical to `chdman extractcd`. What could not
//! be reused is `chd_cd::cd_layout` itself: it reads geometry from a `ChdReader`, and loose
//! track files have no table of contents to read. Recovering that geometry is this module's
//! whole job.
//!
//! **What has to be inferred, and what does not.** A track's length is arithmetic (file size
//! over sector size) and its type is a two-byte read (the sync pattern). The gaps are the hard
//! part: a disc's pregaps are not recoverable from the track bytes at all, because a cue may
//! either carve them out of the file (`INDEX 00`) or declare them as absent (`PREGAP`), and
//! both produce the same bytes on disk. Redump uses both, per dump.
//!
//! So this does not guess once. It renders every convention it knows (see [`Convention`]) and,
//! when the catalogue records what the real sheet hashed to, keeps whichever one reproduces
//! it. That turns an unanswerable question into a lookup for any dump a DAT covers -- which is
//! most of them -- and leaves a plainly-labelled best effort for the rest. A rebuilt sheet is
//! therefore never silently "probably right": [`Rebuilt::matched`] says whether the catalogue
//! confirmed it, and nothing else claims that it did.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::rom::detect::sheet::{CueSheetOptions, GdiFields, Sheet, SheetKind, SheetTrack};
use crate::rom::models::ChdTrackType;

/// The 12-byte sync pattern every raw (2352-byte) data sector starts with. Audio sectors have
/// no structure at all, which is exactly how they are told apart.
const SYNC: [u8; 12] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

/// Frames in the standard two-second gap between a data track and the audio that follows.
const STANDARD_PREGAP: u32 = 150;

/// chdman's `GDI_HIGH_DENSITY_AREA`, re-exported here so a rebuilt gdi places its
/// high-density area exactly where an extracted one does.
pub use crate::rom::chd_cd::GDI_HIGH_DENSITY_AREA;

/// One track file on disk, as the sheet will name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackFile {
    pub path: PathBuf,
    /// The name to write into the sheet: the file's own, never a path.
    pub name: String,
    pub size: u64,
}

impl TrackFile {
    pub fn at(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let size = std::fs::metadata(&path)?.len();
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        Ok(Self { path, name, size })
    }
}

/// What one track turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackProbe {
    pub track_type: ChdTrackType,
    pub sector_size: u32,
    pub frames: u32,
}

/// Reads a track's first sector and works out what it is.
///
/// A raw data sector opens with [`SYNC`] and carries its mode in byte 15; an audio track is
/// raw PCM with no header, so the absence of the pattern is the identification. A file whose
/// size is a multiple of 2048 but not of 2352 is a cooked `MODE1/2048` track (an ISO in all
/// but name), which some dumps of single-track discs use.
pub fn probe_track(path: &Path, size: u64) -> io::Result<TrackProbe> {
    let mut head = [0u8; 16];
    let read = {
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(0))?;
        read_up_to(&mut file, &mut head)?
    };
    let raw_sectors = size % 2352 == 0;
    if read >= 16 && head[..12] == SYNC {
        // Byte 15 is the sector's mode; anything else is a mode this format cannot express, so
        // it is read as MODE1, which is what every cue writer does with it.
        let track_type = if head[15] == 2 { ChdTrackType::Mode2Raw } else { ChdTrackType::Mode1Raw };
        return Ok(TrackProbe { track_type, sector_size: 2352, frames: frames_of(size, 2352) });
    }
    if !raw_sectors && size % 2048 == 0 {
        return Ok(TrackProbe { track_type: ChdTrackType::Mode1, sector_size: 2048, frames: frames_of(size, 2048) });
    }
    Ok(TrackProbe { track_type: ChdTrackType::Audio, sector_size: 2352, frames: frames_of(size, 2352) })
}

/// A short read at end-of-file is a fact about the file, not an error.
fn read_up_to(file: &mut File, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Sectors in a track, rounding up: a truncated final sector is still a sector the sheet has
/// to account for, and rounding down would silently shorten the disc.
fn frames_of(size: u64, sector_size: u32) -> u32 {
    size.div_ceil(sector_size as u64) as u32
}

/// How a cue expresses the gap before an audio track.
///
/// Both forms describe the same disc and the same bytes; they differ only in whether the gap's
/// frames were written into the track file. Nothing in the files themselves distinguishes
/// them, which is why [`rebuild_cue`] renders all of these and lets the catalogue choose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Convention {
    /// The gap is inside the following track's file: `INDEX 00 00:00:00`, `INDEX 01 00:02:00`.
    /// Redump's most common form for a mixed-mode disc.
    GapInFile,
    /// The gap is not in any file: `PREGAP 00:02:00`, `INDEX 01 00:00:00`.
    GapDeclared,
    /// No gaps at all -- every track starts at `INDEX 01 00:00:00`. Right for an all-data
    /// disc, and for dumps that simply carry none.
    NoGap,
}

impl Convention {
    /// Most-likely first: a rebuild with nothing to check against keeps the first, and a
    /// rebuild that can check tries them in this order.
    pub const ALL: [Convention; 3] = [Convention::GapInFile, Convention::GapDeclared, Convention::NoGap];

    pub fn name(self) -> &'static str {
        match self {
            Self::GapInFile => "gap_in_file",
            Self::GapDeclared => "gap_declared",
            Self::NoGap => "no_gap",
        }
    }

    /// What the preview says it did, in the user's terms.
    pub fn label(self) -> &'static str {
        match self {
            Self::GapInFile => "two-second gap inside the audio track (INDEX 00)",
            Self::GapDeclared => "two-second gap declared, not in the files (PREGAP)",
            Self::NoGap => "no gaps",
        }
    }
}

/// A rebuilt sheet and how much confidence it has earned.
#[derive(Debug, Clone)]
pub struct Rebuilt {
    pub sheet: Sheet,
    pub bytes: Vec<u8>,
    pub file_name: String,
    /// Which gap convention produced these bytes.
    pub convention: Convention,
    /// Whether `bytes` reproduce a sheet the catalogue already describes. `false` means it is
    /// structurally sound and unconfirmed -- never that it is wrong.
    pub matched: bool,
}

/// What the catalogue knows about the sheet that is missing: enough to recognise a correct
/// rebuild, when a DAT covers this dump.
#[derive(Debug, Clone, Default)]
pub struct Expected {
    pub size: Option<u64>,
    /// Lowercase hex, by algorithm name, exactly as `hashes` stores it.
    pub hashes: Vec<(String, String)>,
}

impl Expected {
    pub fn is_empty(&self) -> bool {
        self.size.is_none() && self.hashes.is_empty()
    }

    /// Whether `bytes` are the sheet this describes.
    ///
    /// Size alone never confirms: two cue sheets of one disc differ in a handful of digits and
    /// collide on length constantly. A hash confirms; with no hash, nothing does.
    pub fn confirms(&self, bytes: &[u8]) -> bool {
        if self.hashes.is_empty() {
            return false;
        }
        if self.size.is_some_and(|s| s != bytes.len() as u64) {
            return false;
        }
        // One digest over every algorithm the catalogue offers, then compare each: a pass per
        // algorithm would spawn a worker set per algorithm for a file of a few hundred bytes.
        let mut flags = crate::rom::hash::HashFlags::empty();
        let named: Vec<(crate::common::models::HashAlgorithms, &str)> = self
            .hashes
            .iter()
            .filter_map(|(algorithm, value)| crate::api::copy::algorithm_from_name(algorithm).map(|a| (a, value.as_str())))
            .collect();
        for (algorithm, _) in &named {
            flags |= crate::rom::hash::HashFlags::algorithm_to_flag(*algorithm);
        }
        if flags.is_empty() {
            return false;
        }
        let report = crate::rom::hash::hash_bytes(flags, bytes);
        named.iter().any(|(algorithm, value)| report.get(*algorithm).is_some_and(|got| got.eq_ignore_ascii_case(value)))
    }
}

/// Rebuilds a `.cue` for `tracks`, in the order given.
///
/// `expected` is what the catalogue records for the sheet that is missing; when it confirms
/// one of the conventions, that one is returned as `matched`. With nothing to check against,
/// the first convention is returned unconfirmed.
pub fn rebuild_cue(stem: &str, tracks: &[TrackFile], expected: &Expected, options: &CueSheetOptions) -> Result<Rebuilt, String> {
    if tracks.is_empty() {
        return Err("no track files to build a cue from".to_string());
    }
    let probes = probe_all(tracks)?;
    let mut first: Option<Rebuilt> = None;
    for convention in Convention::ALL {
        let sheet = cue_sheet(tracks, &probes, convention);
        let bytes = sheet.write(options);
        let rebuilt = Rebuilt { sheet, bytes, file_name: format!("{stem}.cue"), convention, matched: false };
        if expected.confirms(&rebuilt.bytes) {
            return Ok(Rebuilt { matched: true, ..rebuilt });
        }
        first.get_or_insert(rebuilt);
    }
    first.ok_or_else(|| "no cue could be built".to_string())
}

/// Rebuilds a `.gdi` for `tracks`, in the order given.
///
/// A GD-ROM's geometry is not a matter of convention the way a cue's gaps are: tracks 1-2 are
/// the single-density area from frame 0, and track 3 opens the high-density area at a fixed
/// frame ([`GDI_HIGH_DENSITY_AREA`]). So there is one candidate, not three, and `expected`
/// only confirms or does not.
pub fn rebuild_gdi(stem: &str, tracks: &[TrackFile], expected: &Expected, options: &CueSheetOptions) -> Result<Rebuilt, String> {
    if tracks.is_empty() {
        return Err("no track files to build a gdi from".to_string());
    }
    let probes = probe_all(tracks)?;
    let sheet = gdi_sheet(tracks, &probes);
    let bytes = sheet.write(options);
    let matched = expected.confirms(&bytes);
    Ok(Rebuilt { sheet, bytes, file_name: format!("{stem}.gdi"), convention: Convention::NoGap, matched })
}

fn probe_all(tracks: &[TrackFile]) -> Result<Vec<TrackProbe>, String> {
    tracks.iter().map(|t| probe_track(&t.path, t.size).map_err(|e| format!("{}: {e}", t.path.display()))).collect()
}

/// The cue's tracks under one gap convention.
///
/// `index1` is measured from the start of the track's own file, which is what makes a split
/// cue's numbers so small: `chd_cd` resets its `disc_offset` at every new `FILE`, and one file
/// per track means every track resets. A rebuild is always split (each track is its own file
/// on disk), so the only track that can carry a non-zero index is one whose gap lives in its
/// own file.
fn cue_sheet(tracks: &[TrackFile], probes: &[TrackProbe], convention: Convention) -> Sheet {
    let mut out = Vec::with_capacity(tracks.len());
    for (i, (track, probe)) in tracks.iter().zip(probes).enumerate() {
        let gap = convention != Convention::NoGap && i > 0 && probe.track_type == ChdTrackType::Audio && probes[i - 1].track_type != ChdTrackType::Audio;
        let (pregap, index0, index1) = match (gap, convention) {
            (true, Convention::GapInFile) => (None, Some(0), STANDARD_PREGAP),
            (true, Convention::GapDeclared) => (Some(STANDARD_PREGAP), None, 0),
            _ => (None, None, 0),
        };
        out.push(SheetTrack {
            number: i as u32 + 1,
            file_name: track.name.clone(),
            track_type: probe.track_type,
            sector_size: probe.sector_size,
            subtype: None,
            frames: probe.frames,
            pregap,
            index0,
            index1,
            postgap: None,
            gdi: None,
        });
    }
    Sheet::from_tracks(SheetKind::Cue, out, Vec::new())
}

/// The gdi's tracks, with absolute LBAs.
///
/// Unlike a cue, a gdi numbers every track from the start of the disc, so the LBAs accumulate:
/// track 1 at 0, each later track after the previous one plus the standard gap -- except track
/// 3, which is where the high-density area begins and is therefore fixed rather than computed.
fn gdi_sheet(tracks: &[TrackFile], probes: &[TrackProbe]) -> Sheet {
    let mut out = Vec::with_capacity(tracks.len());
    let mut lba = 0u32;
    for (i, (track, probe)) in tracks.iter().zip(probes).enumerate() {
        // Track 3 opens the high-density area at a frame the format fixes; everything before
        // it is the single-density area and everything after follows on from it.
        if i == 2 {
            lba = GDI_HIGH_DENSITY_AREA;
        } else if i > 0 {
            lba += STANDARD_PREGAP;
        }
        let audio = probe.track_type == ChdTrackType::Audio;
        out.push(SheetTrack {
            number: i as u32 + 1,
            file_name: track.name.clone(),
            track_type: probe.track_type,
            sector_size: probe.sector_size,
            subtype: None,
            frames: probe.frames,
            pregap: None,
            index0: None,
            index1: lba,
            postgap: None,
            gdi: Some(GdiFields { lba, kind: if audio { 0 } else { 4 }, sector_size: probe.sector_size, offset: 0 }),
        });
        lba += probe.frames;
    }
    Sheet::from_tracks(SheetKind::Gdi, out, Vec::new())
}

/// Rebuilds an `.m3u`, trying each line ending.
///
/// A playlist has no conventions to search the way a cue does -- it is a list of names -- but
/// it does have two spellings of the end of a line, and which one a DAT recorded is not
/// something the disc names can tell you. So the search is the same shape as [`rebuild_cue`]'s,
/// over a smaller space: render both, keep whichever the catalogue confirms.
pub fn rebuild_m3u(stem: &str, discs: &[String], expected: &Expected, preferred: crate::rom::detect::sheet::LineEnding) -> Rebuilt {
    let mut first: Option<Rebuilt> = None;
    // The configured ending first, so an unconfirmable playlist still comes out consistent
    // with everything else this collection generates.
    let endings = [preferred, crate::rom::detect::sheet::LineEnding::Lf, crate::rom::detect::sheet::LineEnding::Crlf];
    for ending in endings {
        let bytes = build_m3u(discs, ending);
        let rebuilt = Rebuilt {
            sheet: Sheet::from_tracks(SheetKind::Cue, Vec::new(), Vec::new()),
            bytes,
            file_name: format!("{stem}.m3u"),
            convention: Convention::NoGap,
            matched: false,
        };
        if expected.confirms(&rebuilt.bytes) {
            return Rebuilt { matched: true, ..rebuilt };
        }
        first.get_or_insert(rebuilt);
    }
    first.expect("the ending list is never empty")
}

/// Builds an `.m3u` from its discs, one path per line.
///
/// Deliberately the whole of it. A playlist is not a description of anything -- it is an
/// ordered list of files an emulator opens in turn -- so there is nothing to infer, nothing to
/// probe and no convention to choose between. The names are written relative, because a
/// playlist that names absolute paths stops working the moment the collection moves, which is
/// the one thing a copy to another machine guarantees.
pub fn build_m3u(discs: &[String], line_ending: crate::rom::detect::sheet::LineEnding) -> Vec<u8> {
    let newline = line_ending.as_str();
    let mut out = String::new();
    for disc in discs {
        out.push_str(disc);
        out.push_str(newline);
    }
    out.into_bytes()
}

/// How far a sheet got when it was checked.
///
/// Two independent questions, deliberately not collapsed into one verdict. **Structure** asks
/// whether the sheet works: does it parse, does it name files that are there, do their sizes
/// divide into whole sectors. **Hash** asks the stricter question of whether it is the exact
/// sheet a DAT records. A sheet can be perfectly playable and not byte-identical to Redump's
/// -- a different line ending, a gap written the other legal way -- and calling that "bad"
/// would condemn a working disc. So structure is the verdict that matters for use, and the
/// hash is a second, better answer when the catalogue can give one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SheetVerdict {
    /// Structurally sound *and* byte-identical to what the catalogue records.
    Verified,
    /// Structurally sound. `hash_known` distinguishes "the catalogue has nothing to compare
    /// against" from "it does, and this is not it" -- the second is worth showing differently
    /// even though neither stops the disc working.
    StructureOk { hash_known: bool },
    /// Not usable as it stands: each problem is one thing a person could go and fix.
    Broken { problems: Vec<String> },
}

impl SheetVerdict {
    /// Whether the sheet works, whatever the catalogue thinks of it.
    pub fn usable(&self) -> bool {
        !matches!(self, Self::Broken { .. })
    }

    /// The badge text the file carries.
    pub fn label(&self) -> String {
        match self {
            Self::Verified => "verified".to_string(),
            Self::StructureOk { hash_known: false } => "structure ok".to_string(),
            Self::StructureOk { hash_known: true } => "structure ok, not the DAT's".to_string(),
            Self::Broken { problems } => format!("broken ({})", problems.len()),
        }
    }

    /// `ok` / `warn` / `bad`, matching the badge classes the rest of the UI uses.
    pub fn tone(&self) -> &'static str {
        match self {
            Self::Verified => "ok",
            Self::StructureOk { .. } => "warn",
            Self::Broken { .. } => "bad",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::StructureOk { .. } => "structure_ok",
            Self::Broken { .. } => "broken",
        }
    }
}

/// Checks a `.cue` / `.gdi` already on disk.
///
/// Structure first, because that is what decides whether the disc plays: the sheet has to
/// parse, every track file it names has to exist beside it, and each of those files has to
/// divide into whole sectors of the size the sheet claims -- a track whose length is not a
/// multiple of its sector size means the sheet and the file disagree about what the file is,
/// which is the failure that actually breaks emulators.
///
/// Only then the hash, against whatever the catalogue records for this very sheet. A mismatch
/// is reported, never promoted to an error: see [`SheetVerdict`].
pub fn check_sheet(path: &Path, expected: &Expected) -> SheetVerdict {
    let sheet = match Sheet::parse(path) {
        Ok(sheet) => sheet,
        Err(e) => return SheetVerdict::Broken { problems: vec![e] },
    };
    let directory = path.parent().unwrap_or(Path::new("."));
    let mut problems = Vec::new();

    if sheet.tracks.is_empty() {
        problems.push("it lists no tracks".to_string());
    }
    for name in &sheet.files {
        let track = directory.join(name);
        match std::fs::metadata(&track) {
            Err(_) => problems.push(format!("{name}: named by the sheet but not here")),
            Ok(meta) if meta.len() == 0 => problems.push(format!("{name}: empty")),
            Ok(meta) => {
                // The sector size the sheet claims for this file, from its first track.
                if let Some(track) = sheet.tracks.iter().find(|t| &t.file_name == name) {
                    let sector = track.sector_size as u64;
                    if sector > 0 && meta.len() % sector != 0 {
                        problems.push(format!("{name}: {} bytes is not a whole number of {sector}-byte sectors", meta.len()));
                    }
                }
            }
        }
    }
    // Track numbers are how an emulator orders the disc; a gap or a repeat is not cosmetic.
    for (i, track) in sheet.tracks.iter().enumerate() {
        if track.number != i as u32 + 1 {
            problems.push(format!("track {} is numbered {} -- the numbering has to run 1, 2, 3", i + 1, track.number));
            break;
        }
    }

    if !problems.is_empty() {
        return SheetVerdict::Broken { problems };
    }
    match std::fs::read(path) {
        Ok(bytes) if expected.confirms(&bytes) => SheetVerdict::Verified,
        Ok(_) => SheetVerdict::StructureOk { hash_known: !expected.hashes.is_empty() },
        Err(e) => SheetVerdict::Broken { problems: vec![format!("can't read it back: {e}")] },
    }
}

/// Checks an `.m3u` already on disk.
///
/// A playlist has no structure to speak of, so there is exactly one thing to check and it is
/// the thing that breaks: every line has to name a file that is there. Blank lines and `#`
/// comments are skipped, since both are ordinary in a hand-written playlist.
pub fn check_m3u(path: &Path, expected: &Expected) -> SheetVerdict {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) => return SheetVerdict::Broken { problems: vec![format!("can't read it: {e}")] },
    };
    let directory = path.parent().unwrap_or(Path::new("."));
    let mut problems = Vec::new();
    let mut listed = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        listed += 1;
        // An absolute line is not an error -- it may well resolve -- but it is the thing that
        // stops working the moment the collection is copied anywhere.
        let target = if Path::new(line).is_absolute() { PathBuf::from(line) } else { directory.join(line) };
        if !target.exists() {
            problems.push(format!("{line}: listed but not here"));
        } else if Path::new(line).is_absolute() {
            problems.push(format!("{line}: an absolute path, which breaks as soon as the collection moves"));
        }
    }
    if listed == 0 {
        problems.push("it lists no discs".to_string());
    }
    if !problems.is_empty() {
        return SheetVerdict::Broken { problems };
    }
    match std::fs::read(path) {
        Ok(bytes) if expected.confirms(&bytes) => SheetVerdict::Verified,
        Ok(_) => SheetVerdict::StructureOk { hash_known: !expected.hashes.is_empty() },
        Err(e) => SheetVerdict::Broken { problems: vec![format!("can't read it back: {e}")] },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("romexplorer_sheet_build_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    /// A raw data sector: the sync pattern, then a header whose last byte is the mode.
    fn data_sector(mode: u8) -> Vec<u8> {
        let mut sector = vec![0u8; 2352];
        sector[..12].copy_from_slice(&SYNC);
        sector[15] = mode;
        sector
    }

    fn write(dir: &Path, name: &str, body: &[u8]) -> TrackFile {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        TrackFile::at(path).unwrap()
    }

    #[test]
    fn a_data_track_is_recognised_by_its_sync_pattern() {
        let dir = scratch("probe");
        let mode1 = write(&dir, "d1.bin", &data_sector(1).repeat(3));
        let mode2 = write(&dir, "d2.bin", &data_sector(2).repeat(2));
        assert_eq!(probe_track(&mode1.path, mode1.size).unwrap(), TrackProbe { track_type: ChdTrackType::Mode1Raw, sector_size: 2352, frames: 3 });
        assert_eq!(probe_track(&mode2.path, mode2.size).unwrap(), TrackProbe { track_type: ChdTrackType::Mode2Raw, sector_size: 2352, frames: 2 });
    }

    /// Audio has no structure to recognise, so it is what is left when the pattern is absent.
    #[test]
    fn audio_is_what_has_no_sync_pattern() {
        let dir = scratch("audio");
        let audio = write(&dir, "a.bin", &vec![0x5Au8; 2352 * 4]);
        assert_eq!(probe_track(&audio.path, audio.size).unwrap(), TrackProbe { track_type: ChdTrackType::Audio, sector_size: 2352, frames: 4 });
    }

    /// A cooked track has no sync pattern either, so only its sector size tells it from audio.
    #[test]
    fn a_cooked_2048_track_is_not_read_as_audio() {
        let dir = scratch("cooked");
        let iso = write(&dir, "c.bin", &vec![0u8; 2048 * 5]);
        assert_eq!(probe_track(&iso.path, iso.size).unwrap().track_type, ChdTrackType::Mode1);
        assert_eq!(probe_track(&iso.path, iso.size).unwrap().sector_size, 2048);
    }

    #[test]
    fn a_single_data_track_needs_no_gaps_at_all() {
        let dir = scratch("single");
        let track = write(&dir, "Game (Track 1).bin", &data_sector(1).repeat(10));
        let out = rebuild_cue("Game", &[track], &Expected::default(), &CueSheetOptions::redump()).unwrap();
        let text = String::from_utf8(out.bytes.clone()).unwrap();
        assert_eq!(text, "FILE \"Game (Track 1).bin\" BINARY\r\n  TRACK 01 MODE1/2352\r\n    INDEX 01 00:00:00\r\n", "{text}");
        assert!(!out.matched, "nothing recorded, so nothing is claimed");
    }

    /// The gap only ever belongs to audio that follows data -- audio after audio runs straight
    /// on, and a second data track never takes one.
    #[test]
    fn only_the_first_audio_track_after_data_gets_the_gap() {
        let dir = scratch("mixed");
        let tracks = vec![
            write(&dir, "G (Track 1).bin", &data_sector(1).repeat(10)),
            write(&dir, "G (Track 2).bin", &vec![1u8; 2352 * 4]),
            write(&dir, "G (Track 3).bin", &vec![2u8; 2352 * 4]),
        ];
        let out = rebuild_cue("G", &tracks, &Expected::default(), &CueSheetOptions::redump()).unwrap();
        assert_eq!(out.convention, Convention::GapInFile, "the most common Redump form is the default");
        assert_eq!(out.sheet.tracks[1].index0, Some(0));
        assert_eq!(out.sheet.tracks[1].index1, STANDARD_PREGAP);
        assert_eq!((out.sheet.tracks[2].index0, out.sheet.tracks[2].index1), (None, 0), "audio after audio has no gap");
    }

    /// The point of trying several: the catalogue picks, rather than the first guess winning.
    #[test]
    fn the_convention_the_catalogue_confirms_is_the_one_returned() {
        let dir = scratch("confirm");
        let tracks = vec![write(&dir, "G (Track 1).bin", &data_sector(1).repeat(10)), write(&dir, "G (Track 2).bin", &vec![1u8; 2352 * 4])];
        let options = CueSheetOptions::redump();

        // Render the *second* convention and offer its hash as what the DAT recorded.
        let declared = cue_sheet(&tracks, &probe_all(&tracks).unwrap(), Convention::GapDeclared).write(&options);
        let expected = Expected {
            size: Some(declared.len() as u64),
            hashes: vec![("sha1".to_string(), crate::rom::hash::hash_bytes(crate::rom::hash::HashFlags::SHA1, &declared).sha1.unwrap())],
        };
        let out = rebuild_cue("G", &tracks, &expected, &options).unwrap();
        assert!(out.matched);
        assert_eq!(out.convention, Convention::GapDeclared);
        assert_eq!(out.bytes, declared);
    }

    /// Size is not confirmation: it is a handful of digits' worth of entropy, and two
    /// conventions of one disc collide on it constantly.
    #[test]
    fn a_size_alone_never_confirms_a_rebuild() {
        let dir = scratch("sizeonly");
        let tracks = vec![write(&dir, "G (Track 1).bin", &data_sector(1).repeat(10))];
        let options = CueSheetOptions::redump();
        let bytes = cue_sheet(&tracks, &probe_all(&tracks).unwrap(), Convention::NoGap).write(&options);
        let expected = Expected { size: Some(bytes.len() as u64), hashes: Vec::new() };
        assert!(!expected.confirms(&bytes));
        assert!(!rebuild_cue("G", &tracks, &expected, &options).unwrap().matched);
    }

    /// A wrong hash is a refusal, not a near miss -- every convention is tried and none wins.
    #[test]
    fn a_hash_that_matches_nothing_leaves_the_rebuild_unconfirmed() {
        let dir = scratch("nomatch");
        let tracks = vec![write(&dir, "G (Track 1).bin", &data_sector(1).repeat(10)), write(&dir, "G (Track 2).bin", &vec![1u8; 2352 * 4])];
        let expected = Expected { size: None, hashes: vec![("sha1".to_string(), "0".repeat(40))] };
        let out = rebuild_cue("G", &tracks, &expected, &CueSheetOptions::redump()).unwrap();
        assert!(!out.matched);
        assert_eq!(out.convention, Convention::GapInFile, "and falls back to the most likely one");
    }

    #[test]
    fn a_gdi_puts_the_high_density_area_where_the_format_says() {
        let dir = scratch("gdi");
        let tracks = vec![
            write(&dir, "disc01.bin", &data_sector(1).repeat(100)),
            write(&dir, "disc02.raw", &vec![1u8; 2352 * 50]),
            write(&dir, "disc03.bin", &data_sector(1).repeat(200)),
        ];
        let out = rebuild_gdi("disc", &tracks, &Expected::default(), &CueSheetOptions::redump()).unwrap();
        let lbas: Vec<u32> = out.sheet.tracks.iter().map(|t| t.gdi.unwrap().lba).collect();
        assert_eq!(lbas, vec![0, 250, GDI_HIGH_DENSITY_AREA], "track 2 follows track 1 plus the gap; track 3 is fixed");
        assert_eq!(out.sheet.tracks.iter().map(|t| t.gdi.unwrap().kind).collect::<Vec<_>>(), vec![4, 0, 4]);
        let text = String::from_utf8(out.bytes).unwrap();
        assert!(text.starts_with("3\r\n"), "the track count leads the file: {text}");
    }

    /// The rebuilt sheet has to name the files that are actually on disk, or it describes a
    /// disc nobody has.
    #[test]
    fn a_rebuilt_sheet_names_the_files_it_was_built_from() {
        let dir = scratch("names");
        let tracks = vec![write(&dir, "Weird Name (Disc 1) (Track 1).bin", &data_sector(1).repeat(3)), write(&dir, "Weird Name (Disc 1) (Track 2).bin", &vec![9u8; 2352])];
        let out = rebuild_cue("Weird Name (Disc 1)", &tracks, &Expected::default(), &CueSheetOptions::redump()).unwrap();
        assert_eq!(out.sheet.files, vec!["Weird Name (Disc 1) (Track 1).bin".to_string(), "Weird Name (Disc 1) (Track 2).bin".to_string()]);
        assert_eq!(out.file_name, "Weird Name (Disc 1).cue");
    }

    /// Parsing a rebuild has to give back what built it: the verify job compares the two, and
    /// a rebuild the parser cannot read would fail its own check.
    #[test]
    fn a_rebuilt_cue_parses_back_to_itself() {
        let dir = scratch("roundtrip");
        let tracks = vec![
            write(&dir, "G (Track 1).bin", &data_sector(2).repeat(10)),
            write(&dir, "G (Track 2).bin", &vec![1u8; 2352 * 4]),
            write(&dir, "G (Track 3).bin", &vec![2u8; 2352 * 4]),
        ];
        for convention in Convention::ALL {
            let sheet = cue_sheet(&tracks, &probe_all(&tracks).unwrap(), convention);
            let text = String::from_utf8(sheet.write(&CueSheetOptions::redump())).unwrap();
            let back = Sheet::parse_text(SheetKind::Cue, &text).unwrap();
            assert_eq!(back.files, sheet.files, "{convention:?}");
            assert_eq!(back.tracks.iter().map(|t| (t.number, t.track_type, t.sector_size, t.pregap, t.index0, t.index1)).collect::<Vec<_>>(),
                       sheet.tracks.iter().map(|t| (t.number, t.track_type, t.sector_size, t.pregap, t.index0, t.index1)).collect::<Vec<_>>(), "{convention:?}");
        }
    }

    #[test]
    fn an_m3u_is_one_relative_name_per_line() {
        let out = build_m3u(&["G (Disc 1).chd".to_string(), "G (Disc 2).chd".to_string()], crate::rom::detect::sheet::LineEnding::Lf);
        assert_eq!(String::from_utf8(out).unwrap(), "G (Disc 1).chd\nG (Disc 2).chd\n");
    }

    /// A truncated last sector still has to be counted, or the sheet describes a shorter disc
    /// than the one on disk.
    #[test]
    fn a_partial_final_sector_still_counts_as_a_frame() {
        assert_eq!(frames_of(2352 * 3, 2352), 3);
        assert_eq!(frames_of(2352 * 3 + 1, 2352), 4);
        assert_eq!(frames_of(0, 2352), 0);
    }

    #[test]
    fn a_sheet_whose_tracks_are_all_there_is_structurally_ok() {
        let dir = scratch("check_ok");
        write(&dir, "G (Track 1).bin", &data_sector(1).repeat(10));
        let cue = dir.join("G.cue");
        std::fs::write(&cue, "FILE \"G (Track 1).bin\" BINARY\r\n  TRACK 01 MODE1/2352\r\n    INDEX 01 00:00:00\r\n").unwrap();
        assert_eq!(check_sheet(&cue, &Expected::default()), SheetVerdict::StructureOk { hash_known: false });
    }

    /// The whole point of the lenient verdict: not matching the DAT is not the same as broken.
    #[test]
    fn a_sheet_that_works_but_isnt_the_dats_is_not_broken() {
        let dir = scratch("check_lenient");
        write(&dir, "G (Track 1).bin", &data_sector(1).repeat(10));
        let cue = dir.join("G.cue");
        std::fs::write(&cue, "FILE \"G (Track 1).bin\" BINARY\r\n  TRACK 01 MODE1/2352\r\n    INDEX 01 00:00:00\r\n").unwrap();
        let expected = Expected { size: None, hashes: vec![("sha1".to_string(), "0".repeat(40))] };
        let verdict = check_sheet(&cue, &expected);
        assert_eq!(verdict, SheetVerdict::StructureOk { hash_known: true });
        assert!(verdict.usable(), "it still plays");
        assert_eq!(verdict.tone(), "warn");
    }

    #[test]
    fn a_sheet_matching_the_dat_is_verified() {
        let dir = scratch("check_verified");
        write(&dir, "G (Track 1).bin", &data_sector(1).repeat(10));
        let cue = dir.join("G.cue");
        let body = b"FILE \"G (Track 1).bin\" BINARY\r\n  TRACK 01 MODE1/2352\r\n    INDEX 01 00:00:00\r\n";
        std::fs::write(&cue, body).unwrap();
        let expected = Expected {
            size: Some(body.len() as u64),
            hashes: vec![("sha1".to_string(), crate::rom::hash::hash_bytes(crate::rom::hash::HashFlags::SHA1, body).sha1.unwrap())],
        };
        assert_eq!(check_sheet(&cue, &expected), SheetVerdict::Verified);
    }

    /// The failure that actually breaks emulators: the sheet and the file disagree about what
    /// the file is.
    #[test]
    fn a_track_that_isnt_whole_sectors_is_broken() {
        let dir = scratch("check_sectors");
        write(&dir, "G (Track 1).bin", &data_sector(1).repeat(10)[..2352 * 10 - 7]);
        let cue = dir.join("G.cue");
        std::fs::write(&cue, "FILE \"G (Track 1).bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n").unwrap();
        match check_sheet(&cue, &Expected::default()) {
            SheetVerdict::Broken { problems } => assert!(problems[0].contains("whole number of 2352-byte sectors"), "{problems:?}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_sheet_naming_a_track_that_isnt_there_is_broken() {
        let dir = scratch("check_missing");
        let cue = dir.join("G.cue");
        std::fs::write(&cue, "FILE \"G (Track 1).bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n").unwrap();
        match check_sheet(&cue, &Expected::default()) {
            SheetVerdict::Broken { problems } => assert!(problems[0].contains("not here"), "{problems:?}"),
            other => panic!("{other:?}"),
        }
    }

    /// A rebuild has to pass its own verifier, or the two halves disagree about what correct
    /// means and every created sheet is immediately flagged.
    #[test]
    fn what_the_rebuilder_writes_passes_the_checker() {
        let dir = scratch("selfcheck");
        let tracks = vec![
            write(&dir, "G (Track 1).bin", &data_sector(1).repeat(10)),
            write(&dir, "G (Track 2).bin", &vec![1u8; 2352 * 4]),
        ];
        let out = rebuild_cue("G", &tracks, &Expected::default(), &CueSheetOptions::redump()).unwrap();
        let cue = dir.join(&out.file_name);
        std::fs::write(&cue, &out.bytes).unwrap();
        assert_eq!(check_sheet(&cue, &Expected::default()), SheetVerdict::StructureOk { hash_known: false });
    }

    #[test]
    fn a_playlist_is_checked_line_by_line() {
        let dir = scratch("m3u");
        std::fs::write(dir.join("G (Disc 1).chd"), b"x").unwrap();
        std::fs::write(dir.join("G (Disc 2).chd"), b"x").unwrap();
        let m3u = dir.join("G.m3u");

        std::fs::write(&m3u, "# a playlist\n\nG (Disc 1).chd\nG (Disc 2).chd\n").unwrap();
        assert_eq!(check_m3u(&m3u, &Expected::default()), SheetVerdict::StructureOk { hash_known: false }, "comments and blanks are ordinary");

        std::fs::write(&m3u, "G (Disc 1).chd\nG (Disc 3).chd\n").unwrap();
        match check_m3u(&m3u, &Expected::default()) {
            SheetVerdict::Broken { problems } => assert!(problems[0].contains("Disc 3"), "{problems:?}"),
            other => panic!("{other:?}"),
        }

        std::fs::write(&m3u, "").unwrap();
        assert!(matches!(check_m3u(&m3u, &Expected::default()), SheetVerdict::Broken { .. }), "an empty playlist groups nothing");
    }

    /// It resolves, so it is not "missing" -- but it is the one thing that stops working the
    /// moment the collection is copied, which is what this app does for a living.
    #[test]
    fn an_absolute_line_is_flagged_even_though_it_resolves() {
        let dir = scratch("m3u_abs");
        let disc = dir.join("G (Disc 1).chd");
        std::fs::write(&disc, b"x").unwrap();
        let m3u = dir.join("G.m3u");
        std::fs::write(&m3u, format!("{}\n", disc.display())).unwrap();
        match check_m3u(&m3u, &Expected::default()) {
            SheetVerdict::Broken { problems } => assert!(problems[0].contains("absolute path"), "{problems:?}"),
            other => panic!("{other:?}"),
        }
    }
}
