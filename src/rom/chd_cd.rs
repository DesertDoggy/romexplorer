//! A CD/GD-ROM CHD as chdman's `extractcd` would lay it out on disk -- without chdman
//! doing the extraction. [`cd_layout`] turns the CHD's track table into the list of track
//! files (names, byte ranges) plus the cue/gdi sheet ([`super::detect::sheet::Sheet`]) that
//! `chdman extractcd` writes for it, and [`CdImageReader`] is a [`ReadAt`] over the
//! concatenation of those track files, decoded on demand through a [`ChdReader`] -- so the
//! parallel decode in [`super::parallel`] can produce the exact bytes of every track file,
//! split and hashed per track, with any number of threads and no chdman call lock held.
//!
//! "Exact" means the chdman this crate ships -- chdman-simd's `do_extract_cd`
//! (`deps/chdman-simd/_build/<variant>/<platform>/<arch>/src/src/tools/chdman.cpp`,
//! a patched older MAME): per frame only `datasize` bytes are written (subcode is dropped
//! for cue/gdi output), AUDIO frames are byte-swapped for cue output and for gdi output when
//! the CHD version is > 4, each track's trailing `padframes` (GD-ROM) are skipped, and gdi
//! LBAs are written without a pregap shift (unlike current MAME) and no `REM SESSION` lines.
//! For a GD-ROM written as cue, the fork first rewrites the table of contents to Redump's
//! layout ([`redump_gdrom_adjust`]): the next track's pregap frames are taken from the end
//! of the previous track (`splitframes`) so a track file may start with frames that
//! physically belong to the track before it -- hence [`FrameSegment`]s. `padframes` and
//! whether a pregap is stored in the track data (`pgdatasize`) aren't exposed by the native
//! reader, so they come from `chd_info::read_chd_metadata` (`TrackMetadata::pad`/
//! `pregap_in_file`).

use std::io;
use std::sync::Arc;

use super::chdman_wrapper::{ChdReader, ChdTrack, ChdmanError, CD_MAX_SECTOR_BYTES, CD_TRACK_AUDIO};
use super::detect::sheet::{CueSheetOptions, GdiFields, Sheet, SheetKind, SheetTrack};
use super::models::{ChdMetadata, ChdTrackSubtype, ChdTrackType};
use super::source::ReadAt;

/// chdman's `GDI_HIGH_DENSITY_AREA`: the physical frame the high-density area of a GD-ROM
/// starts at.
pub const GDI_HIGH_DENSITY_AREA: u32 = 45000;

/// A run of frames read from the CHD in physical order, as part of one track file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameSegment {
    /// Physical LBA of the first frame.
    pub physical_start: u32,
    pub frames: u32,
    /// `CD_TRACK_*` of the track the frames physically belong to (the read datatype).
    pub track_type: u32,
}

/// One track file of the extracted layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdTrackLayout {
    /// 0-based track index.
    pub index: u32,
    /// The CHD's own track entry (unadjusted).
    pub track: ChdTrack,
    /// Pregap frames as the sheet reports them (after any GD-ROM/Redump adjustment).
    pub pregap: u32,
    /// The pregap's frames are in the file (`INDEX 00`) rather than implied (`PREGAP`).
    pub pregap_in_file: bool,
    /// Trailing frames not written (after adjustment).
    pub pad_frames: u32,
    /// Frames at the start of the file that belong to the previous track (after adjustment).
    pub split_frames: u32,
    /// Bytes per written frame (`datasize`; subcode is never written).
    pub frame_bytes: u32,
    /// The frames written, in order.
    pub segments: Vec<FrameSegment>,
    /// Frames actually written: `frames - pad_frames + split_frames`.
    pub written_frames: u32,
    /// The track's byte range in the concatenated extracted image.
    pub start: u64,
    pub end: u64,
    /// Track file name (no directory), as the sheet refers to it.
    pub file_name: String,
    /// AUDIO frames (by their source track) are byte-swapped on the way out.
    pub swap_audio: bool,
}

impl CdTrackLayout {
    pub fn len(&self) -> u64 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.end == self.start
    }
}

/// The whole extracted layout of one CD/GD-ROM CHD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdLayout {
    pub kind: SheetKind,
    pub tracks: Vec<CdTrackLayout>,
    /// `{stem}.cue` / `{stem}.gdi` (or `.toc` for [`crate::common::models::CueSheetStyle::Mame`]).
    pub sheet_file_name: String,
    pub sheet: Sheet,
    /// Total bytes of all track files.
    pub total_bytes: u64,
}

impl CdLayout {
    /// The sheet text; pass the same options the layout was built with.
    pub fn sheet_bytes(&self, options: &CueSheetOptions) -> Vec<u8> {
        self.sheet.write(options)
    }

    /// The distinct track files in order, with their byte ranges (tracks sharing a file --
    /// a non-split cue -- are merged into one entry).
    pub fn files(&self) -> Vec<(String, u64, u64)> {
        let mut files: Vec<(String, u64, u64)> = Vec::new();
        for track in &self.tracks {
            match files.last_mut() {
                Some((name, _, end)) if *name == track.file_name => *end = track.end,
                _ => files.push((track.file_name.clone(), track.start, track.end)),
            }
        }
        files
    }
}

fn chd_track_type(raw: u32) -> ChdTrackType {
    match raw {
        0 => ChdTrackType::Mode1,
        1 => ChdTrackType::Mode1Raw,
        2 => ChdTrackType::Mode2,
        3 => ChdTrackType::Mode2Form1,
        4 => ChdTrackType::Mode2Form2,
        5 => ChdTrackType::Mode2FormMix,
        6 => ChdTrackType::Mode2Raw,
        _ => ChdTrackType::Audio,
    }
}

fn chd_subtype(raw: u32) -> Option<ChdTrackSubtype> {
    match raw {
        0 => Some(ChdTrackSubtype::Normal),
        1 => Some(ChdTrackSubtype::Raw),
        _ => None,
    }
}

/// Track file names as `do_extract_cd` builds them: gdi always splits and names
/// `{stem}NN.bin` (`.raw` for AUDIO); cue splits when there is more than one track (the
/// rule `extract::extract` uses for `--splitbin`, and what chdman forces for GD-ROM cue
/// output) as `{stem} (Track N).bin`, N zero-padded to 2 digits only with 10+ tracks; a
/// single-track cue gets `{stem}.bin` unless `single_track_suffix`.
fn track_file_name(stem: &str, kind: SheetKind, index: u32, track_count: u32, audio: bool, single_track_suffix: bool) -> String {
    match kind {
        SheetKind::Gdi => format!("{stem}{:02}.{}", index + 1, if audio { "raw" } else { "bin" }),
        SheetKind::Cue if track_count > 1 || single_track_suffix => {
            if track_count >= 10 {
                format!("{stem} (Track {:02}).bin", index + 1)
            } else {
                format!("{stem} (Track {}).bin", index + 1)
            }
        }
        SheetKind::Cue => format!("{stem}.bin"),
    }
}

/// The table of contents as `do_extract_cd` sees it: the CHD's entries plus the fields the
/// native reader doesn't expose, all of which the GD-ROM cue adjustment may rewrite.
#[derive(Debug, Clone, Copy)]
struct TocTrack {
    track: ChdTrack,
    frames: u32,
    pregap: u32,
    pad_frames: u32,
    split_frames: u32,
    pregap_in_file: bool,
}

/// chdman-simd's "modify TOC to match Redump cue/bin format as best as possible" block,
/// applied for a GD-ROM written as cue. TOSEC gdi-based CHDs carry the next track's pregap
/// in `padframes`; Redump-style ones don't (`has_physical_pregap`), and get a 150-frame
/// (`00:02:00`) pregap carved out of the previous track, 225 when the last track is data.
fn redump_gdrom_adjust(toc: &mut [TocTrack]) {
    let track_count = toc.len();
    let has_physical_pregap = toc[0].pad_frames == 0;
    for i in 1..track_count {
        // pgdatasize should never be set in GD-ROMs; if it is, the TOC already has proper pregaps.
        if toc[i].pregap_in_file {
            break;
        }
        // Never adjust the first track of the single-density and high-density areas.
        if toc[i].track.physical_start == GDI_HIGH_DENSITY_AREA {
            continue;
        }
        let last_is_data = i + 1 >= track_count && toc[i].track.track_type != CD_TRACK_AUDIO;
        if !has_physical_pregap {
            // PREGAP commands, the pregap data not being in the bins.
            toc[i].pregap += toc[i - 1].pad_frames;
            if last_is_data {
                if toc[i - 1].track.track_type != CD_TRACK_AUDIO {
                    // "type 3": the high-density area is two data tracks; the full 3s pregap
                    // is baked into the previous track.
                    toc[i - 1].pad_frames += 225;
                    toc[i].pregap += 225;
                    toc[i].split_frames = 225;
                    toc[i].pregap_in_file = true;
                } else {
                    // "type 3 split": data, audio..., data. chdman drops 75 frames of the
                    // previous track and reports them as pregap.
                    toc[i - 1].frames = toc[i - 1].frames.saturating_sub(75);
                    toc[i].pregap += 75;
                }
            }
        } else {
            let extra = if last_is_data { 150 + 75 } else { 150 };
            toc[i - 1].pad_frames = extra;
            toc[i].pregap += extra;
            toc[i].split_frames = extra;
            toc[i].pregap_in_file = true;
        }
    }
}

/// Builds the extracted layout of the CD/GD-ROM CHD open in `reader`. `metadata` supplies
/// the per-track `pad`/`pregap_in_file` bits the native reader lacks (`None` assumes no
/// padding and pregaps not in the file -- right for most CD CHDs, wrong for GD-ROMs).
/// `options` decides the track file naming (single-track suffix) and the sheet file's
/// extension (`Mame` -> `.toc`); the sheet text itself is rendered by
/// [`CdLayout::sheet_bytes`].
pub fn cd_layout(
    reader: &ChdReader,
    metadata: Option<&ChdMetadata>,
    stem: &str,
    kind: SheetKind,
    options: &CueSheetOptions,
) -> Result<CdLayout, ChdmanError> {
    let info = reader.info()?;
    if !info.is_cd || info.num_tracks == 0 {
        return Err(ChdmanError::InvalidArgument("not a CD/GD-ROM CHD".to_string()));
    }
    let track_count = info.num_tracks;

    let mut toc: Vec<TocTrack> = Vec::with_capacity(track_count as usize);
    for index in 0..track_count {
        let track = reader.track(index)?;
        let meta_track = metadata.and_then(|m| m.tracks.get(index as usize));
        toc.push(TocTrack {
            track,
            frames: track.frames,
            pregap: track.pregap,
            pad_frames: meta_track.and_then(|t| t.pad).unwrap_or(0) as u32,
            split_frames: 0,
            pregap_in_file: meta_track.is_some_and(|t| t.pregap_in_file),
        });
    }
    if info.is_gdrom && kind == SheetKind::Cue {
        redump_gdrom_adjust(&mut toc);
    }

    let mut tracks = Vec::with_capacity(track_count as usize);
    let mut sheet_tracks = Vec::with_capacity(track_count as usize);
    let mut rems = Vec::new();
    let mut cursor = 0u64;
    // chdman's `discoffs`: frames written so far -- into the current file for cue output
    // (reset whenever a new file starts), across the whole disc for gdi output.
    let mut disc_offset = 0u32;
    let mut previous_file: Option<String> = None;

    for (index, entry) in toc.iter().enumerate() {
        let index = index as u32;
        let track = entry.track;
        let audio = track.track_type == CD_TRACK_AUDIO;
        let file_name = track_file_name(stem, kind, index, track_count, audio, options.single_track_suffix);
        let new_file = previous_file.as_deref() != Some(file_name.as_str());
        if new_file && kind == SheetKind::Cue {
            disc_offset = 0;
        }
        previous_file = Some(file_name.clone());

        if kind == SheetKind::Cue && info.is_gdrom {
            if index == 0 {
                rems.push((index as usize, "SINGLE-DENSITY AREA".to_string()));
            } else if track.physical_start == GDI_HIGH_DENSITY_AREA {
                rems.push((index as usize, "HIGH-DENSITY AREA".to_string()));
            }
        }

        let (pregap, index0, index1) = if entry.pregap > 0 && !entry.pregap_in_file {
            (Some(entry.pregap), None, disc_offset)
        } else if entry.pregap > 0 {
            (None, Some(disc_offset), disc_offset + entry.pregap)
        } else {
            (None, None, disc_offset)
        };
        let gdi = (kind == SheetKind::Gdi).then(|| GdiFields { lba: disc_offset, kind: if audio { 0 } else { 4 }, sector_size: track.data_size, offset: 0 });
        sheet_tracks.push(SheetTrack {
            number: index + 1,
            file_name: file_name.clone(),
            track_type: chd_track_type(track.track_type),
            sector_size: track.data_size,
            subtype: chd_subtype(track.sub_type),
            frames: entry.frames,
            pregap,
            index0,
            index1,
            postgap: (track.postgap > 0).then_some(track.postgap),
            gdi,
        });

        let own_frames = entry.frames.saturating_sub(entry.pad_frames);
        let mut segments = Vec::with_capacity(2);
        if entry.split_frames > 0 && index > 0 {
            let previous = &toc[index as usize - 1];
            segments.push(FrameSegment {
                physical_start: previous.track.physical_start + previous.frames.saturating_sub(entry.split_frames),
                frames: entry.split_frames,
                track_type: previous.track.track_type,
            });
        }
        segments.push(FrameSegment { physical_start: track.physical_start, frames: own_frames, track_type: track.track_type });
        let written_frames = own_frames + entry.split_frames;
        let len = written_frames as u64 * track.data_size as u64;
        // Applies to whichever frames are AUDIO by their *source* track (a split segment
        // from an audio track ahead of a data track is swapped too, as chdman does).
        let swap_audio = kind == SheetKind::Cue || info.version > 4;
        tracks.push(CdTrackLayout {
            index,
            track,
            pregap: entry.pregap,
            pregap_in_file: entry.pregap_in_file,
            pad_frames: entry.pad_frames,
            split_frames: entry.split_frames,
            frame_bytes: track.data_size,
            segments,
            written_frames,
            start: cursor,
            end: cursor + len,
            file_name,
            swap_audio,
        });
        cursor += len;
        disc_offset += written_frames + entry.pad_frames;
    }

    let sheet_ext = if options.is_toc() { "toc" } else { kind.extension() };
    Ok(CdLayout {
        kind,
        tracks,
        sheet_file_name: format!("{stem}.{sheet_ext}"),
        sheet: Sheet::from_tracks(kind, sheet_tracks, rems),
        total_bytes: cursor,
    })
}

/// Random access to the extracted bytes of a CD/GD-ROM CHD (see the module doc). One
/// reader per thread, like [`ChdReader`] itself.
pub struct CdImageReader {
    reader: ChdReader,
    layout: Arc<[CdTrackLayout]>,
    sector: Vec<u8>,
}

impl CdImageReader {
    /// `layout` is a [`CdLayout`]'s `tracks`, shared between readers.
    pub fn new(reader: ChdReader, layout: Arc<[CdTrackLayout]>) -> Self {
        Self { reader, layout, sector: vec![0u8; CD_MAX_SECTOR_BYTES] }
    }

    fn track_at(&self, offset: u64) -> Option<usize> {
        let index = self.layout.partition_point(|t| t.end <= offset);
        (index < self.layout.len() && !self.layout[index].is_empty()).then_some(index)
    }

    /// The physical LBA and read datatype of frame `frame` of a track file.
    fn locate(track: &CdTrackLayout, mut frame: u32) -> Option<(u32, u32)> {
        for segment in &track.segments {
            if frame < segment.frames {
                return Some((segment.physical_start + frame, segment.track_type));
            }
            frame -= segment.frames;
        }
        None
    }
}

impl ReadAt for CdImageReader {
    fn read_at(&mut self, mut offset: u64, mut buf: &mut [u8]) -> io::Result<()> {
        while !buf.is_empty() {
            let Some(index) = self.track_at(offset) else {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, format!("offset {offset} is past the end of the disc image")));
            };
            let track = &self.layout[index];
            let frame_bytes = track.frame_bytes as u64;
            let in_track = offset - track.start;
            let frame = (in_track / frame_bytes) as u32;
            let within = (in_track % frame_bytes) as usize;
            let Some((lba, datatype)) = Self::locate(track, frame) else {
                return Err(io::Error::other(format!("frame {frame} is outside track {}'s segments", track.index + 1)));
            };
            self.reader.read_sector(lba, &mut self.sector, datatype, true).map_err(|err| io::Error::other(err.to_string()))?;
            let data = &mut self.sector[..frame_bytes as usize];
            if track.swap_audio && datatype == CD_TRACK_AUDIO {
                for pair in data.chunks_exact_mut(2) {
                    pair.swap(0, 1);
                }
            }
            let take = (data.len() - within).min(buf.len()).min((track.end - offset) as usize);
            buf[..take].copy_from_slice(&data[within..within + take]);
            buf = &mut buf[take..];
            offset += take as u64;
        }
        Ok(())
    }
}
