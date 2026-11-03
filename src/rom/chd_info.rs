//! Reads a CHD's real metadata (track list, compression, hashes, DVD/hard-disk markers):
//! by parsing chdman's own `info` command output, or through the Rust reader
//! ([`super::chd_header`]) as the `chd_reader` setting says ([`read_chd_metadata`]).
//!
//! `chdman` already knows how to read every field of its own file format; rather than
//! re-implementing CHD's binary header and metadata-tag layout natively (the `chd_native`
//! approach this project's detection layer explicitly moved away from -- see
//! `detect::descriptor`'s module doc), this asks chdman to print what it knows and parses
//! that text. It's the piece `extract::extract`'s `chd_metadata` parameter was written to
//! expect: callers that need to know a CHD's track count (to decide whether `extractcd`
//! needs `--splitbin`) or whether it's a DVD/hard-disk image call [`read_chd_metadata`]
//! first and pass the result through.

use std::path::Path;

use super::models::{ChdCodecs, ChdMetadata, ChdTrackSubtype, ChdTrackTag, ChdTrackType, HardDiskGeometry, TrackMetadata};
use super::chdman_wrapper::{self, ChdmanArgs, ChdmanCommand, ChdmanError};

/// Who reads a CHD's header and metadata (the `chd_reader` setting).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChdReaderMode {
    /// chdman's `info` only: waits behind any running chdman call (a CHD conversion).
    Chdman,
    /// The Rust reader ([`super::chd_header`]), checked against chdman whenever chdman is
    /// free; a disagreement is recorded and chdman's answer used. The default.
    Checked,
    /// The Rust reader alone; chdman only for what it cannot read.
    Rust,
}

impl ChdReaderMode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Chdman => "chdman",
            Self::Checked => "checked",
            Self::Rust => "rust",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "chdman" => Self::Chdman,
            "checked" => Self::Checked,
            "rust" => Self::Rust,
            _ => return None,
        })
    }
}

static MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(1);
static COMPARED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static NOT_COMPARED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DIFFERENCES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
/// Differences kept for the next report; more are logged only.
const MAX_DIFFERENCES: usize = 200;

pub fn set_reader_mode(mode: ChdReaderMode) {
    MODE.store(mode as u8, std::sync::atomic::Ordering::Relaxed);
}

pub fn reader_mode() -> ChdReaderMode {
    match MODE.load(std::sync::atomic::Ordering::Relaxed) {
        0 => ChdReaderMode::Chdman,
        2 => ChdReaderMode::Rust,
        _ => ChdReaderMode::Checked,
    }
}

/// What the checked mode found since the last call: CHDs compared with chdman, ones read
/// while chdman was busy (not compared), and one report line per disagreement.
pub fn take_check_report() -> (u64, u64, Vec<String>) {
    let compared = COMPARED.swap(0, std::sync::atomic::Ordering::Relaxed);
    let skipped = NOT_COMPARED.swap(0, std::sync::atomic::Ordering::Relaxed);
    let differences = std::mem::take(&mut *DIFFERENCES.lock().unwrap_or_else(|p| p.into_inner()));
    (compared, skipped, differences)
}

/// A CHD's header and metadata, read the way the `chd_reader` setting says
/// ([`ChdReaderMode`]).
pub fn read_chd_metadata(path: &Path) -> Result<ChdMetadata, ChdmanError> {
    let mode = reader_mode();
    if mode == ChdReaderMode::Chdman {
        return read_with_chdman(path);
    }
    let rust = match super::chd_header::read(path) {
        Ok(meta) => meta,
        // A version, codec or layout it does not read: chdman's answer, as before.
        Err(e) => {
            log::debug!("chd reader: {} left to chdman: {e}", path.display());
            return read_with_chdman(path);
        }
    };
    if mode == ChdReaderMode::Rust {
        return Ok(rust);
    }
    let args = ChdmanArgs { input: Some(path.to_path_buf()), verbose: true, ..Default::default() };
    match chdman_wrapper::run_if_free(ChdmanCommand::Info, &args, |_, _| {}) {
        Ok(Some(output)) => {
            COMPARED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let chdman = parse_chd_info(&output);
            let differences = super::chd_header::differences(&rust, &chdman);
            if differences.is_empty() {
                return Ok(rust);
            }
            let line = format!("{}: {}", path.display(), differences.join("; "));
            log::warn!("chd reader: the Rust reader and chdman disagree on {line}");
            let mut kept = DIFFERENCES.lock().unwrap_or_else(|p| p.into_inner());
            if kept.len() < MAX_DIFFERENCES {
                kept.push(line);
            }
            Ok(chdman)
        }
        // chdman is busy (a conversion): the Rust answer now, rather than a wait.
        Ok(None) => {
            NOT_COMPARED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(rust)
        }
        Err(_) => Ok(rust),
    }
}

/// Runs `chdman info -v` against `path` and parses its output into a [`ChdMetadata`].
pub fn read_with_chdman(path: &Path) -> Result<ChdMetadata, ChdmanError> {
    let args = ChdmanArgs { input: Some(path.to_path_buf()), verbose: true, ..Default::default() };
    let output = chdman_wrapper::run(ChdmanCommand::Info, &args, |_, _| {})?;
    Ok(parse_chd_info(&output))
}

/// Strips chdman's `big_int_string` thousands separators (e.g. `"1,234,567"`) before
/// parsing.
fn parse_big_int(text: &str) -> usize {
    text.chars().filter(|ch| ch.is_ascii_digit()).collect::<String>().parse().unwrap_or(0)
}

fn parse_chd_codec(tag4: &str) -> Option<ChdCodecs> {
    match tag4 {
        "zstd" => Some(ChdCodecs::Zstd),
        "zlib" => Some(ChdCodecs::Zlib),
        "lzma" => Some(ChdCodecs::Lzma),
        "flac" => Some(ChdCodecs::Flac),
        "huff" => Some(ChdCodecs::Huff),
        "cdzs" => Some(ChdCodecs::Cdzs),
        "cdzl" => Some(ChdCodecs::Cdzl),
        "cdlz" => Some(ChdCodecs::Cdlz),
        "cdfl" => Some(ChdCodecs::Cdfl),
        _ => None,
    }
}

fn parse_track_type(text: &str) -> Option<ChdTrackType> {
    match text {
        "MODE1" => Some(ChdTrackType::Mode1),
        "MODE1_RAW" => Some(ChdTrackType::Mode1Raw),
        "MODE2" => Some(ChdTrackType::Mode2),
        "MODE2_FORM1" => Some(ChdTrackType::Mode2Form1),
        "MODE2_FORM2" => Some(ChdTrackType::Mode2Form2),
        "MODE2_FORM_MIX" => Some(ChdTrackType::Mode2FormMix),
        "MODE2_RAW" => Some(ChdTrackType::Mode2Raw),
        "AUDIO" => Some(ChdTrackType::Audio),
        _ => None,
    }
}

fn parse_track_subtype(text: &str) -> Option<ChdTrackSubtype> {
    match text {
        "RW" => Some(ChdTrackSubtype::Normal),
        "RW_RAW" => Some(ChdTrackSubtype::Raw),
        "NONE" => Some(ChdTrackSubtype::None),
        _ => None,
    }
}

/// Finds the `KEY:value` token starting with `key` (including the colon) among
/// whitespace-split fields, e.g. `field(&["TRACK:1", "TYPE:MODE1_RAW"], "TYPE:")` ->
/// `Some("MODE1_RAW")`.
fn field<'a>(fields: &[&'a str], key: &str) -> Option<&'a str> {
    fields.iter().find_map(|token| token.strip_prefix(key))
}

/// Parses one `TRACK:%d TYPE:%s SUBTYPE:%s FRAMES:%d [PAD:%d] [PREGAP:%d PGTYPE:%s
/// PGSUB:%s POSTGAP:%d]` content line (`CDROM_TRACK_METADATA2_FORMAT` /
/// `GDROM_TRACK_METADATA_FORMAT`, see `external/mame/src/lib/util/chd.cpp`). `CHTR`
/// entries (`CDROM_TRACK_METADATA_FORMAT`) have no PAD/PREGAP/POSTGAP fields, which is
/// fine here since those are all `Option`.
pub(crate) fn parse_track_line(tag: ChdTrackTag, index: usize, content: &str) -> Option<TrackMetadata> {
    let fields: Vec<&str> = content.split_whitespace().collect();

    let track_number: usize = field(&fields, "TRACK:")?.parse().ok()?;
    let track_type = parse_track_type(field(&fields, "TYPE:")?)?;
    let subtype = parse_track_subtype(field(&fields, "SUBTYPE:")?)?;
    let frames: usize = field(&fields, "FRAMES:")?.parse().ok()?;
    let pad = field(&fields, "PAD:").and_then(|value| value.parse().ok());
    let pregap: Option<usize> = field(&fields, "PREGAP:").and_then(|value| value.parse().ok());
    // A `V` prefix on PGTYPE means the pregap sectors are in the track data (chdman's
    // `pgdatasize > 0`); the type itself follows it. Only meaningful with a non-zero pregap.
    let raw_pregap_type = field(&fields, "PGTYPE:");
    let pregap_in_file = pregap.is_some_and(|frames| frames > 0) && raw_pregap_type.is_some_and(|value| value.starts_with('V'));
    let pregap_type = raw_pregap_type.map(|value| value.strip_prefix('V').unwrap_or(value)).and_then(parse_track_type);
    let pregap_subtype = field(&fields, "PGSUB:").and_then(parse_track_subtype);
    let postgap = field(&fields, "POSTGAP:").and_then(|value| value.parse().ok());

    Some(TrackMetadata { track_number, tag, index, track_type, subtype, frames, pad, pregap, pregap_type, pregap_subtype, postgap, pregap_in_file })
}

/// Parses `HARD_DISK_METADATA_FORMAT`'s `CYLS:%d,HEADS:%d,SECS:%d,BPS:%d`.
pub(crate) fn parse_hard_disk_geometry(content: &str) -> Option<HardDiskGeometry> {
    let fields: Vec<&str> = content.split(',').collect();
    Some(HardDiskGeometry {
        cylinders: fields.iter().find_map(|f| f.strip_prefix("CYLS:"))?.parse().ok()?,
        heads: fields.iter().find_map(|f| f.strip_prefix("HEADS:"))?.parse().ok()?,
        sectors: fields.iter().find_map(|f| f.strip_prefix("SECS:"))?.parse().ok()?,
        bytes_per_sector: fields.iter().find_map(|f| f.strip_prefix("BPS:"))?.parse().ok()?,
    })
}

/// Parses `chdman info -v`'s full text output. See `do_info` in
/// `external/mame/src/tools/chdman.cpp` for the exact format this mirrors.
fn parse_chd_info(text: &str) -> ChdMetadata {
    let mut meta = ChdMetadata::default();
    let lines: Vec<&str> = text.lines().collect();

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];

        if let Some(rest) = line.strip_prefix("File Version:") {
            meta.file_version = rest.trim().parse().unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("Logical size:") {
            meta.logical_size = parse_big_int(rest.trim_end_matches("bytes").trim());
        } else if let Some(rest) = line.strip_prefix("Hunk Size:") {
            meta.hunk_size = parse_big_int(rest.trim_end_matches("bytes").trim());
        } else if let Some(rest) = line.strip_prefix("Total Hunks:") {
            meta.total_hunks = parse_big_int(rest.trim());
        } else if let Some(rest) = line.strip_prefix("Unit Size:") {
            meta.unit_size = parse_big_int(rest.trim_end_matches("bytes").trim());
        } else if let Some(rest) = line.strip_prefix("Total Units:") {
            meta.total_units = parse_big_int(rest.trim());
        } else if let Some(rest) = line.strip_prefix("Compression:") {
            let rest = rest.trim();
            if rest != "none" {
                meta.compression = rest.split(',').filter_map(|segment| segment.trim().get(0..4)).filter_map(parse_chd_codec).collect();
            }
        } else if let Some(rest) = line.strip_prefix("CHD size:") {
            meta.chd_size = parse_big_int(rest.trim_end_matches("bytes").trim());
        } else if let Some(rest) = line.strip_prefix("Ratio:") {
            meta.ratio = rest.trim().trim_end_matches('%').parse().unwrap_or(0.0);
        } else if let Some(rest) = line.strip_prefix("Data SHA1:") {
            meta.data_sha1 = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("SHA1:") {
            meta.sha1 = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("Parent SHA1:") {
            meta.parent_sha1 = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("Metadata:") {
            // "     Tag='CHT2'  Index=0  Length=94 bytes", content on the following line.
            let tag = rest.split('\'').nth(1);
            let index = rest.split_whitespace().find_map(|tok| tok.strip_prefix("Index=")).and_then(|v| v.parse().ok()).unwrap_or(0);
            // chdman prints the entry's terminating NUL as '.', which glued itself to the last
            // field (`POSTGAP:0.`) and lost it. No metadata format ends in a dot.
            let content = lines.get(i + 1).map(|l| l.trim().trim_end_matches('.')).unwrap_or("");

            match tag {
                Some("CHTR") => {
                    if let Some(track) = parse_track_line(ChdTrackTag::Chtr, index, content) {
                        meta.tracks.push(track);
                    }
                }
                Some("CHT2") => {
                    if let Some(track) = parse_track_line(ChdTrackTag::Cht2, index, content) {
                        meta.tracks.push(track);
                    }
                }
                Some("CHGD") => {
                    if let Some(track) = parse_track_line(ChdTrackTag::Chgd, index, content) {
                        meta.tracks.push(track);
                    }
                }
                Some("DVD ") => meta.is_dvd = true,
                Some("GDDD") => meta.hard_disk_geometry = parse_hard_disk_geometry(content),
                _ => {}
            }

            // The content line was already consumed above.
            i += 1;
        }

        i += 1;
    }

    meta.tracks.sort_by_key(|track| track.track_number);
    meta
}
