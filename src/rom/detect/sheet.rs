//! Cue / gdi sheets as a model: parsed with the track file names *exactly as written* (so
//! they can name the `Source`s handed to `chdman_wrapper::create_from_sources`, which
//! matches a sheet's `FILE` names against source names), and written back in a chosen
//! [`CueSheetStyle`]. The writer reproduces chdman's own `output_track_metadata`
//! (`external/mame/src/tools/chdman.cpp`, MODE_CUEBIN / MODE_GDI / MODE_NORMAL) line for
//! line, so a sheet reconstructed for a CHD (`rom::chd_cd`) is byte-identical to what
//! `chdman extractcd` writes. Nothing here touches the disk except [`Sheet::parse`].

use std::path::{Path, PathBuf};

use crate::common::models::CueSheetStyle;
use crate::rom::models::{ChdTrackSubtype, ChdTrackType};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SheetKind {
    Cue,
    Gdi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineEnding {
    /// `\r\n` on Windows, `\n` elsewhere -- what chdman itself writes (its text stream
    /// translates `\n` to the platform newline).
    Native,
    Lf,
    Crlf,
}

impl LineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Native => {
                if cfg!(windows) {
                    "\r\n"
                } else {
                    "\n"
                }
            }
            Self::Lf => "\n",
            Self::Crlf => "\r\n",
        }
    }
}

/// How a sheet is written and its track files named. [`Self::for_style`] gives each
/// [`CueSheetStyle`]'s defaults; every knob can then be overridden (a future CLI conforming
/// to chdman, or the main app matching a Redump database, each pick their own).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CueSheetOptions {
    pub style: CueSheetStyle,
    pub line_ending: LineEnding,
    /// Name a single-track disc's file `{stem} (Track 1).bin` (chdman with `--splitbin`)
    /// instead of `{stem}.bin` (Redump, and chdman without `--splitbin`).
    pub single_track_suffix: bool,
    /// Write a `CATALOG 0000000000000` first line (Redump's catalog form).
    pub catalog_line: bool,
}

impl CueSheetOptions {
    pub fn for_style(style: CueSheetStyle) -> Self {
        match style {
            CueSheetStyle::Redump | CueSheetStyle::Unknown => Self { style, line_ending: LineEnding::Crlf, single_track_suffix: false, catalog_line: false },
            CueSheetStyle::RedumpCatalog => Self { style, line_ending: LineEnding::Crlf, single_track_suffix: false, catalog_line: true },
            CueSheetStyle::Chdman => Self { style, line_ending: LineEnding::Native, single_track_suffix: false, catalog_line: false },
            CueSheetStyle::Mame => Self { style, line_ending: LineEnding::Native, single_track_suffix: false, catalog_line: false },
        }
    }

    /// Exactly what `chdman extractcd` writes on this platform (`--splitbin` only for
    /// multi-track discs, as `extract::extract` passes it).
    pub fn chdman() -> Self {
        Self::for_style(CueSheetStyle::Chdman)
    }

    pub fn redump() -> Self {
        Self::for_style(CueSheetStyle::Redump)
    }

    pub fn is_toc(&self) -> bool {
        self.style == CueSheetStyle::Mame
    }

    /// The sheet kind a GD-ROM is laid out as under this style: Redump catalogues GD-ROMs as
    /// a cue (with its high-density adjustments), chdman and MAME as a gdi. Every other CD
    /// is a cue whatever the style.
    pub fn gdrom_sheet(&self) -> SheetKind {
        match self.style {
            CueSheetStyle::Chdman | CueSheetStyle::Mame => SheetKind::Gdi,
            CueSheetStyle::Redump | CueSheetStyle::RedumpCatalog | CueSheetStyle::Unknown => SheetKind::Cue,
        }
    }
}

impl Default for CueSheetOptions {
    fn default() -> Self {
        Self::for_style(CueSheetStyle::default())
    }
}

impl SheetKind {
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "cue" => Some(Self::Cue),
            "gdi" => Some(Self::Gdi),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Cue => "cue",
            Self::Gdi => "gdi",
        }
    }
}

/// Only meaningful for a [`SheetKind::Gdi`] sheet: the fields of one `.gdi` line besides
/// the track number and file name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GdiFields {
    /// Frame (LBA) the track starts at.
    pub lba: u32,
    /// 0 audio, 4 data.
    pub kind: u32,
    pub sector_size: u32,
    /// Byte offset within the file (always 0 for chdman's output).
    pub offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SheetTrack {
    /// 1-based.
    pub number: u32,
    /// Track file name as written in the sheet (no directory).
    pub file_name: String,
    pub track_type: ChdTrackType,
    /// Bytes per sector in the file (2048/2336/2352...).
    pub sector_size: u32,
    /// Subcode stored with each sector, if any (only ever `Some` for a CHD-derived sheet;
    /// cue/gdi files can't carry subcode, so the toc writer is the only consumer).
    pub subtype: Option<ChdTrackSubtype>,
    /// Total frames of the track as chdman counts them (for the toc writer only; 0 when
    /// parsed from a cue/gdi).
    pub frames: u32,
    /// `PREGAP` tag (pregap not stored in the file), in frames.
    pub pregap: Option<u32>,
    /// `INDEX 00` (pregap stored in the file), in frames from the start of the file.
    pub index0: Option<u32>,
    /// `INDEX 01`, in frames from the start of the file.
    pub index1: u32,
    pub postgap: Option<u32>,
    pub gdi: Option<GdiFields>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sheet {
    pub kind: SheetKind,
    pub tracks: Vec<SheetTrack>,
    /// `REM` lines, each attached to the index of the track it precedes (cue only).
    pub rems: Vec<(usize, String)>,
    /// Every track file the sheet names, in sheet order, each once, exactly as written
    /// (a cue's `FILE` lines, a gdi's name fields).
    pub files: Vec<String>,
}

fn parse_msf(text: &str) -> Option<u32> {
    let mut parts = text.split(':');
    let m: u32 = parts.next()?.trim().parse().ok()?;
    let s: u32 = parts.next()?.trim().parse().ok()?;
    let f: u32 = parts.next()?.trim().parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(m * 75 * 60 + s * 75 + f)
}

/// `msf_string_from_frames` (chdman.cpp L1078).
pub fn msf(frames: u32) -> String {
    format!("{:02}:{:02}:{:02}", frames / (75 * 60), (frames / 75) % 60, frames % 75)
}

fn quoted_value(line: &str) -> Option<&str> {
    let start = line.find('"')?;
    let rest = &line[start + 1..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// `MODE1/2352` -> (type, sector size), the same vocabulary chdman's cue parser accepts.
fn cue_mode(text: &str) -> Option<(ChdTrackType, u32)> {
    let upper = text.to_ascii_uppercase();
    if upper == "AUDIO" {
        return Some((ChdTrackType::Audio, 2352));
    }
    let (mode, size) = upper.split_once('/')?;
    let size: u32 = size.parse().ok()?;
    let track_type = match (mode, size) {
        ("MODE1", 2048) => ChdTrackType::Mode1,
        ("MODE1", 2352) => ChdTrackType::Mode1Raw,
        ("MODE2", 2048) => ChdTrackType::Mode2Form1,
        ("MODE2", 2324) => ChdTrackType::Mode2Form2,
        ("MODE2", 2336) => ChdTrackType::Mode2,
        ("MODE2", 2352) => ChdTrackType::Mode2Raw,
        _ => return None,
    };
    Some((track_type, size))
}

/// chdman's `cdrom_file::get_type_string` / `get_subtype_string` vocabulary.
pub fn type_string(track_type: ChdTrackType) -> &'static str {
    match track_type {
        ChdTrackType::Mode1 => "MODE1",
        ChdTrackType::Mode1Raw => "MODE1_RAW",
        ChdTrackType::Mode2 => "MODE2",
        ChdTrackType::Mode2Form1 => "MODE2_FORM1",
        ChdTrackType::Mode2Form2 => "MODE2_FORM2",
        ChdTrackType::Mode2FormMix => "MODE2_FORM_MIX",
        ChdTrackType::Mode2Raw => "MODE2_RAW",
        ChdTrackType::Audio => "AUDIO",
    }
}

fn subtype_string(subtype: ChdTrackSubtype) -> &'static str {
    match subtype {
        ChdTrackSubtype::Normal => "RW",
        ChdTrackSubtype::Raw => "RW_RAW",
        ChdTrackSubtype::None => "NONE",
    }
}

/// A sheet's text: UTF-8 (a leading BOM dropped), else Shift-JIS, which is what sheets from
/// Japanese sets that name their tracks in Japanese are written in.
pub fn sheet_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => encoding_rs::SHIFT_JIS.decode(bytes).0.into_owned(),
    }
}

impl Sheet {
    /// Reads and parses `path`, whose extension picks the kind.
    pub fn parse(path: &Path) -> Result<Self, String> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let kind = SheetKind::from_extension(ext).ok_or_else(|| format!("not a cue/gdi sheet: {}", path.display()))?;
        let bytes = std::fs::read(path).map_err(|err| format!("failed to read {}: {err}", path.display()))?;
        Self::parse_text(kind, &sheet_text(&bytes))
    }

    pub fn parse_text(kind: SheetKind, text: &str) -> Result<Self, String> {
        let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
        match kind {
            SheetKind::Cue => Self::parse_cue(text),
            SheetKind::Gdi => Self::parse_gdi(text),
        }
    }

    fn parse_cue(text: &str) -> Result<Self, String> {
        let mut tracks: Vec<SheetTrack> = Vec::new();
        let mut rems = Vec::new();
        let mut files: Vec<String> = Vec::new();
        let mut current_file: Option<String> = None;

        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            let mut words = line.splitn(2, char::is_whitespace);
            let keyword = words.next().unwrap_or("").to_ascii_uppercase();
            let rest = words.next().unwrap_or("").trim();
            match keyword.as_str() {
                "FILE" => {
                    let name = quoted_value(rest).map(str::to_string).or_else(|| rest.split_whitespace().next().map(|s| s.trim_matches('"').to_string()));
                    let name = name.ok_or_else(|| format!("FILE line without a name: {line}"))?;
                    if !files.contains(&name) {
                        files.push(name.clone());
                    }
                    current_file = Some(name);
                }
                "TRACK" => {
                    let mut fields = rest.split_whitespace();
                    let number: u32 = fields.next().and_then(|n| n.parse().ok()).ok_or_else(|| format!("bad TRACK line: {line}"))?;
                    let (track_type, sector_size) = fields.next().and_then(cue_mode).ok_or_else(|| format!("bad TRACK mode: {line}"))?;
                    let file_name = current_file.clone().ok_or_else(|| format!("TRACK {number} before any FILE line"))?;
                    tracks.push(SheetTrack {
                        number,
                        file_name,
                        track_type,
                        sector_size,
                        subtype: None,
                        frames: 0,
                        pregap: None,
                        index0: None,
                        index1: 0,
                        postgap: None,
                        gdi: None,
                    });
                }
                "INDEX" => {
                    let mut fields = rest.split_whitespace();
                    let index: u32 = fields.next().and_then(|n| n.parse().ok()).ok_or_else(|| format!("bad INDEX line: {line}"))?;
                    let frames = fields.next().and_then(parse_msf).ok_or_else(|| format!("bad INDEX time: {line}"))?;
                    let track = tracks.last_mut().ok_or_else(|| format!("INDEX before any TRACK: {line}"))?;
                    match index {
                        0 => track.index0 = Some(frames),
                        1 => track.index1 = frames,
                        _ => {} // higher indexes aren't representable in a CHD and are dropped, as chdman does
                    }
                }
                "PREGAP" => {
                    let frames = parse_msf(rest).ok_or_else(|| format!("bad PREGAP: {line}"))?;
                    tracks.last_mut().ok_or_else(|| format!("PREGAP before any TRACK: {line}"))?.pregap = Some(frames);
                }
                "POSTGAP" => {
                    let frames = parse_msf(rest).ok_or_else(|| format!("bad POSTGAP: {line}"))?;
                    tracks.last_mut().ok_or_else(|| format!("POSTGAP before any TRACK: {line}"))?.postgap = Some(frames);
                }
                "REM" => rems.push((tracks.len(), rest.to_string())),
                _ => {} // TITLE/PERFORMER/FLAGS/CATALOG/ISRC: not carried
            }
        }
        if files.is_empty() {
            return Err("cue sheet has no FILE lines".to_string());
        }
        Ok(Self { kind: SheetKind::Cue, tracks, rems, files })
    }

    fn parse_gdi(text: &str) -> Result<Self, String> {
        let mut tracks = Vec::new();
        for (idx, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            if idx == 0 && line.chars().all(|ch| ch.is_ascii_digit()) {
                continue; // track count
            }
            // number lba kind sector_size name offset -- the name may be quoted and contain spaces.
            // Columns may be lined up with several spaces or tabs.
            let mut rest = line;
            let mut field = || -> Option<&str> {
                rest = rest.trim_start();
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                let (word, after) = rest.split_at(end);
                rest = after;
                (!word.is_empty()).then_some(word)
            };
            let number: u32 = field().and_then(|n| n.parse().ok()).ok_or_else(|| format!("bad gdi line: {line}"))?;
            let lba: u32 = field().and_then(|n| n.parse().ok()).ok_or_else(|| format!("bad gdi line: {line}"))?;
            let kind: u32 = field().and_then(|n| n.parse().ok()).ok_or_else(|| format!("bad gdi line: {line}"))?;
            let sector_size: u32 = field().and_then(|n| n.parse().ok()).ok_or_else(|| format!("bad gdi line: {line}"))?;
            let tail = rest.trim();
            let (file_name, offset_text) = if let Some(stripped) = tail.strip_prefix('"') {
                let end = stripped.find('"').ok_or_else(|| format!("unterminated quote in gdi line: {line}"))?;
                (stripped[..end].to_string(), stripped[end + 1..].trim())
            } else {
                let mut parts = tail.splitn(2, char::is_whitespace);
                (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").trim())
            };
            if file_name.is_empty() {
                return Err(format!("gdi line without a file name: {line}"));
            }
            let offset: u64 = offset_text.split_whitespace().next().and_then(|n| n.parse().ok()).unwrap_or(0);
            let track_type = if kind == 0 {
                ChdTrackType::Audio
            } else {
                match sector_size {
                    2048 => ChdTrackType::Mode1,
                    _ => ChdTrackType::Mode1Raw,
                }
            };
            tracks.push(SheetTrack {
                number,
                file_name,
                track_type,
                sector_size,
                subtype: None,
                frames: 0,
                pregap: None,
                index0: None,
                index1: lba,
                postgap: None,
                gdi: Some(GdiFields { lba, kind, sector_size, offset }),
            });
        }
        if tracks.is_empty() {
            return Err("gdi sheet has no track lines".to_string());
        }
        let files = Self::unique_track_files(&tracks);
        Ok(Self { kind: SheetKind::Gdi, tracks, rems: Vec::new(), files })
    }

    fn unique_track_files(tracks: &[SheetTrack]) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for track in tracks {
            if !names.contains(&track.file_name) {
                names.push(track.file_name.clone());
            }
        }
        names
    }

    /// Builds a sheet from tracks (e.g. `rom::chd_cd::cd_layout`'s); `files` is derived.
    pub fn from_tracks(kind: SheetKind, tracks: Vec<SheetTrack>, rems: Vec<(usize, String)>) -> Self {
        let files = Self::unique_track_files(&tracks);
        Self { kind, tracks, rems, files }
    }

    /// The track file names in sheet order, each once, exactly as written.
    pub fn payload_names(&self) -> Vec<String> {
        self.files.clone()
    }

    /// [`Self::payload_names`] resolved against the directory the sheet lives in, as
    /// [`resolve_listed`] spells them: `\` read as a separator and each name matched to the
    /// directory's own entry case-insensitively, so a sheet written on Windows finds its
    /// tracks on Linux and names them as the walk lists them. No existence check.
    pub fn payload_paths(&self, sheet_dir: &Path) -> Vec<PathBuf> {
        let mut listings = Listings::default();
        let mut out: Vec<PathBuf> = Vec::new();
        for name in self.payload_names() {
            let path = resolve_listed(sheet_dir, &name, &mut listings);
            // Two spellings of one file in the sheet are one track file.
            if !out.contains(&path) {
                out.push(path);
            }
        }
        out
    }

    /// The sheet text: cue or gdi (per `kind`) in chdman's exact line format, or chdman's
    /// `.toc` form for [`CueSheetStyle::Mame`]; line endings and the optional `CATALOG`
    /// line per `options`. (Track file *naming* is decided when the sheet is built --
    /// `rom::chd_cd::cd_layout` -- from the same options.)
    pub fn write(&self, options: &CueSheetOptions) -> Vec<u8> {
        let mut out = String::new();
        if options.catalog_line && !options.is_toc() && self.kind == SheetKind::Cue {
            out.push_str("CATALOG 0000000000000\n");
        }
        match (options.is_toc(), self.kind) {
            (true, _) => self.write_toc(&mut out),
            (false, SheetKind::Cue) => self.write_cue(&mut out),
            (false, SheetKind::Gdi) => self.write_gdi(&mut out),
        }
        let newline = options.line_ending.as_str();
        if newline != "\n" {
            out = out.replace('\n', newline);
        }
        out.into_bytes()
    }

    /// MODE_CUEBIN of `output_track_metadata` (chdman.cpp L1544-1596): `FILE` only when a
    /// track starts a new file, two-space `TRACK`, four-space index lines.
    fn write_cue(&self, out: &mut String) {
        let mut previous_file: Option<&str> = None;
        for (i, track) in self.tracks.iter().enumerate() {
            for (_, rem) in self.rems.iter().filter(|(at, _)| *at == i) {
                out.push_str("REM ");
                out.push_str(rem);
                out.push('\n');
            }
            if previous_file != Some(track.file_name.as_str()) {
                out.push_str(&format!("FILE \"{}\" BINARY\n", track.file_name));
                previous_file = Some(track.file_name.as_str());
            }
            let mode = match track.track_type {
                ChdTrackType::Mode1 | ChdTrackType::Mode1Raw => format!("MODE1/{:04}", track.sector_size),
                ChdTrackType::Audio => "AUDIO".to_string(),
                _ => format!("MODE2/{:04}", track.sector_size),
            };
            out.push_str(&format!("  TRACK {:02} {mode}\n", track.number));
            match (track.pregap, track.index0) {
                (Some(pregap), _) if pregap > 0 => {
                    out.push_str(&format!("    PREGAP {}\n", msf(pregap)));
                    out.push_str(&format!("    INDEX 01 {}\n", msf(track.index1)));
                }
                (_, Some(index0)) => {
                    out.push_str(&format!("    INDEX 00 {}\n", msf(index0)));
                    out.push_str(&format!("    INDEX 01 {}\n", msf(track.index1)));
                }
                _ => out.push_str(&format!("    INDEX 01 {}\n", msf(track.index1))),
            }
            if let Some(postgap) = track.postgap.filter(|p| *p > 0) {
                out.push_str(&format!("    POSTGAP {}\n", msf(postgap)));
            }
        }
        for (_, rem) in self.rems.iter().filter(|(at, _)| *at >= self.tracks.len()) {
            out.push_str("REM ");
            out.push_str(rem);
            out.push('\n');
        }
    }

    /// MODE_GDI (chdman.cpp L1533-1542, count line L2833): `number lba kind size name offset`,
    /// the name quoted only when it contains a space.
    fn write_gdi(&self, out: &mut String) {
        out.push_str(&format!("{}\n", self.tracks.len()));
        for track in &self.tracks {
            let fields = track.gdi.unwrap_or(GdiFields {
                lba: track.index1,
                kind: if track.track_type == ChdTrackType::Audio { 0 } else { 4 },
                sector_size: track.sector_size,
                offset: 0,
            });
            let quote = if track.file_name.contains(' ') { "\"" } else { "" };
            out.push_str(&format!(
                "{} {} {} {} {quote}{}{quote} {}\n",
                track.number, fields.lba, fields.kind, fields.sector_size, track.file_name, fields.offset
            ));
        }
    }

    /// MODE_NORMAL (chdman.cpp L2835-2870 header, L1598-1632 per track).
    fn write_toc(&self, out: &mut String) {
        let mode1 = self.tracks.iter().any(|t| matches!(t.track_type, ChdTrackType::Mode1 | ChdTrackType::Mode1Raw));
        let mode2 = self.tracks.iter().any(|t| {
            matches!(
                t.track_type,
                ChdTrackType::Mode2 | ChdTrackType::Mode2Form1 | ChdTrackType::Mode2Form2 | ChdTrackType::Mode2FormMix | ChdTrackType::Mode2Raw
            )
        });
        let cdda = self.tracks.iter().any(|t| t.track_type == ChdTrackType::Audio);
        out.push_str(if mode2 {
            "CD_ROM_XA\n\n\n"
        } else if cdda && !mode1 {
            "CD_DA\n\n\n"
        } else {
            "CD_ROM\n\n\n"
        });

        let mut previous_file: Option<&str> = None;
        let mut output_offset = 0u64;
        for track in &self.tracks {
            out.push_str(&format!("// Track {}\n", track.number));
            let sub_size = match track.subtype {
                Some(ChdTrackSubtype::Normal) | Some(ChdTrackSubtype::Raw) => 96u32,
                _ => 0,
            };
            let modesubmode = match track.subtype {
                Some(subtype) if subtype != ChdTrackSubtype::None => format!("{} {}", type_string(track.track_type), subtype_string(subtype)),
                _ => type_string(track.track_type).to_string(),
            };
            out.push_str(&format!("TRACK {modesubmode}\n"));
            out.push_str("NO COPY\n");
            if track.track_type == ChdTrackType::Audio {
                out.push_str("NO PRE_EMPHASIS\n");
                out.push_str("TWO_CHANNEL_AUDIO\n");
            }
            let pregap = track.pregap.or(track.index0.map(|i0| track.index1.saturating_sub(i0))).unwrap_or(0);
            if pregap > 0 {
                out.push_str(&format!("ZERO {modesubmode} {}\n", msf(pregap)));
            }
            if previous_file != Some(track.file_name.as_str()) {
                output_offset = 0;
                previous_file = Some(track.file_name.as_str());
            }
            let length_bytes = track.frames as u64 * (track.sector_size + sub_size) as u64;
            if output_offset == 0 {
                out.push_str(&format!("DATAFILE \"{}\" {} // length in bytes: {length_bytes}\n", track.file_name, msf(track.frames)));
            } else {
                out.push_str(&format!(
                    "DATAFILE \"{}\" #{output_offset} {} // length in bytes: {length_bytes}\n",
                    track.file_name,
                    msf(track.frames)
                ));
            }
            if pregap > 0 {
                out.push_str(&format!("START {}\n", msf(pregap)));
            }
            out.push_str("\n\n");
            output_offset += length_bytes;
        }
    }
}


/// Directory listings [`resolve_listed`] has read, so a sheet or playlist naming many files
/// in one folder reads that folder once.
#[derive(Default)]
pub struct Listings(std::collections::HashMap<PathBuf, Vec<std::ffi::OsString>>);

impl Listings {
    fn entries(&mut self, dir: &Path) -> &[std::ffi::OsString] {
        self.0.entry(dir.to_path_buf()).or_insert_with(|| std::fs::read_dir(dir).map(|r| r.flatten().map(|e| e.file_name()).collect()).unwrap_or_default())
    }
}

/// A file a sheet (`FILE`) or playlist line names, relative to `base` (the directory it sits
/// in; an absolute name stays absolute), as it is on the disk:
///
/// - `\` is a separator everywhere: sheets and playlists written on Windows use it, and on
///   Linux it would otherwise be part of one file name;
/// - `.` and `..` are folded away without touching the disk, so `../x.bin` is the same path
///   as the `x.bin` the walk lists in the parent;
/// - each component below `base` is matched against its directory's entries: the exact name
///   first, else the one entry equal ignoring case (Unicode-aware). Linux finds a `GAME.BIN`
///   written for `Game.bin`, and Windows / macOS get the on-disk spelling rather than the
///   sheet's, so one file is never recorded under two spellings. A component nothing
///   matches is kept as written (the caller reports a missing file).
pub fn resolve_listed(base: &Path, name: &str, listings: &mut Listings) -> PathBuf {
    use std::path::Component;
    let written = PathBuf::from(name.replace('\\', "/"));
    let (mut out, rest): (PathBuf, PathBuf) = if written.is_absolute() { (PathBuf::new(), written) } else { (base.to_path_buf(), written) };
    // Matching stops at the first component the disk does not have; the rest is as written.
    let mut matching = !out.as_os_str().is_empty();
    for part in rest.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            Component::Normal(want) if matching => {
                let entries = listings.entries(&out);
                let found = entries.iter().find(|e| e.as_os_str() == want).or_else(|| {
                    let lower = want.to_string_lossy().to_lowercase();
                    let mut same = entries.iter().filter(|e| e.to_string_lossy().to_lowercase() == lower);
                    // Two entries differing only in case (a case-sensitive disk): neither is
                    // the one meant more than the other, so the name stays as written.
                    match (same.next(), same.next()) {
                        (Some(one), None) => Some(one),
                        _ => None,
                    }
                });
                match found {
                    Some(entry) => out.push(entry),
                    None => {
                        matching = false;
                        out.push(want);
                    }
                }
            }
            other => {
                // A root or prefix (absolute names): matched from there on.
                out.push(other.as_os_str());
                matching = true;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lf(style: CueSheetStyle) -> CueSheetOptions {
        CueSheetOptions { line_ending: LineEnding::Lf, ..CueSheetOptions::for_style(style) }
    }

    /// A sheet written on Windows: `\\` separators, another case than the disk, and a track
    /// one directory up. Each resolves to the entry as the directory lists it.
    #[test]
    fn track_names_resolve_to_the_listed_spelling() {
        let root = std::env::temp_dir().join(format!("romexplorer_test_sheet_resolve_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Game/Tracks")).unwrap();
        std::fs::write(root.join("Game/Tracks/Track 01.bin"), b"x").unwrap();
        std::fs::write(root.join("Shared.bin"), b"x").unwrap();
        let base = root.join("Game");
        let mut l = Listings::default();
        assert_eq!(resolve_listed(&base, "TRACKS\\track 01.BIN", &mut l), root.join("Game/Tracks/Track 01.bin"));
        assert_eq!(resolve_listed(&base, "..\\shared.bin", &mut l), root.join("Shared.bin"));
        assert_eq!(resolve_listed(&base, "./Tracks/Track 01.bin", &mut l), root.join("Game/Tracks/Track 01.bin"));
        assert_eq!(resolve_listed(&base, "missing\\a.bin", &mut l), root.join("Game/missing/a.bin"), "kept as written");
        let sheet = Sheet::parse_text(SheetKind::Cue, "FILE \"tracks\\TRACK 01.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n").unwrap();
        assert_eq!(sheet.payload_paths(&base), vec![root.join("Game/Tracks/Track 01.bin")]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn line_endings_and_catalog_follow_options() {
        let text = "FILE \"A.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n";
        let sheet = Sheet::parse_text(SheetKind::Cue, text).unwrap();
        let crlf = String::from_utf8(sheet.write(&CueSheetOptions::redump())).unwrap();
        assert_eq!(crlf, text.replace('\n', "\r\n"));
        let catalog = String::from_utf8(sheet.write(&lf(CueSheetStyle::RedumpCatalog))).unwrap();
        assert!(catalog.starts_with("CATALOG 0000000000000\n"));
        assert_eq!(&catalog["CATALOG 0000000000000\n".len()..], text);
        let toc = String::from_utf8(sheet.write(&lf(CueSheetStyle::Mame))).unwrap();
        assert!(toc.starts_with("CD_ROM\n\n\n// Track 1\nTRACK MODE1_RAW\nNO COPY\n"));
    }

    #[test]
    fn msf_round_trips() {
        for frames in [0u32, 1, 74, 75, 4500, 4501, 449_999] {
            assert_eq!(parse_msf(&msf(frames)), Some(frames));
        }
        assert_eq!(msf(4500), "01:00:00");
    }

    #[test]
    fn cue_parse_keeps_names_and_gaps() {
        let text = "FILE \"Disc (Track 1).bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\nFILE \"Disc (Track 2).bin\" BINARY\n  TRACK 02 AUDIO\n    PREGAP 00:02:00\n    INDEX 01 00:00:00\n  TRACK 03 AUDIO\n    INDEX 00 00:10:00\n    INDEX 01 00:12:00\n    POSTGAP 00:01:00\n";
        let sheet = Sheet::parse_text(SheetKind::Cue, text).unwrap();
        assert_eq!(sheet.payload_names(), vec!["Disc (Track 1).bin".to_string(), "Disc (Track 2).bin".to_string()]);
        assert_eq!(sheet.tracks[1].pregap, Some(150));
        assert_eq!(sheet.tracks[2].index0, Some(750));
        assert_eq!(sheet.tracks[2].index1, 900);
        assert_eq!(sheet.tracks[2].postgap, Some(75));
        assert_eq!(sheet.tracks[0].track_type, ChdTrackType::Mode1Raw);
        assert_eq!(String::from_utf8(sheet.write(&lf(CueSheetStyle::Chdman))).unwrap(), text);
    }

    #[test]
    fn gdi_parse_handles_quoted_names_and_writes_back() {
        let text = "3\n1 0 4 2352 \"Game 01.bin\" 0\n2 600 0 2352 \"Game 02.raw\" 0\n3 45000 4 2352 \"Game 03.bin\" 0\n";
        let sheet = Sheet::parse_text(SheetKind::Gdi, text).unwrap();
        assert_eq!(sheet.tracks.len(), 3);
        assert_eq!(sheet.tracks[1].file_name, "Game 02.raw");
        assert_eq!(sheet.tracks[1].track_type, ChdTrackType::Audio);
        assert_eq!(sheet.tracks[2].gdi.unwrap().lba, 45000);
        assert_eq!(String::from_utf8(sheet.write(&lf(CueSheetStyle::Chdman))).unwrap(), text);

        let unquoted = "1\n1 0 4 2048 track01.bin 0\n";
        let sheet = Sheet::parse_text(SheetKind::Gdi, unquoted).unwrap();
        assert_eq!(sheet.tracks[0].file_name, "track01.bin");
        assert_eq!(String::from_utf8(sheet.write(&lf(CueSheetStyle::Chdman))).unwrap(), unquoted);
    }

    #[test]
    fn a_gdi_with_lined_up_columns_parses() {
        let sheet = Sheet::parse_text(SheetKind::Gdi, "2\n1  0     4 2352  track01.bin 0\n2\t600\t0\t2352\t\"Game 02.raw\"\t0\n").unwrap();
        assert_eq!(sheet.tracks[0].file_name, "track01.bin");
        assert_eq!(sheet.tracks[1].file_name, "Game 02.raw");
        assert_eq!(sheet.tracks[1].gdi.unwrap().lba, 600);
    }

    #[test]
    fn a_cue_with_a_bom_or_in_shift_jis_parses() {
        let cue = "FILE \"ゲーム.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n";
        let mut bom = b"\xEF\xBB\xBF".to_vec();
        bom.extend_from_slice(cue.as_bytes());
        let (sjis, _, _) = encoding_rs::SHIFT_JIS.encode(cue);
        for bytes in [bom, sjis.into_owned()] {
            let sheet = Sheet::parse_text(SheetKind::Cue, &sheet_text(&bytes)).unwrap();
            assert_eq!(sheet.tracks[0].file_name, "ゲーム.bin");
        }
    }

    #[test]
    fn rems_are_placed_before_their_track() {
        let text = "REM SINGLE-DENSITY AREA\nFILE \"A.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\nREM HIGH-DENSITY AREA\nFILE \"B.bin\" BINARY\n  TRACK 02 MODE1/2352\n    INDEX 01 00:00:00\n";
        let sheet = Sheet::parse_text(SheetKind::Cue, text).unwrap();
        assert_eq!(sheet.rems, vec![(0, "SINGLE-DENSITY AREA".to_string()), (1, "HIGH-DENSITY AREA".to_string())]);
        assert_eq!(String::from_utf8(sheet.write(&lf(CueSheetStyle::Redump))).unwrap(), text);
    }
}
