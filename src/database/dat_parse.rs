//! Parses one source's raw reference data (a `samples/dats/<source>/...` file, or whatever
//! [`super::dat_download`] fetches at runtime) into the normalized [`ParsedDatFile`] shape
//! [`super::import::import_dat`] loads.
//!
//! Five of the six sources (redump, no-intro, tosec, retroachievements, mame-redump) are
//! all the same Logiqx-DTD XML family, differing only in which tag names and fields they
//! actually use -- [`parse_dat_xml`] handles all of them:
//! - element/child names: `<game>`/`<rom>` (redump, no-intro, tosec) vs `<machine>`/
//!   `<disk>` (mame-redump's CHD/RVZ dats, retroachievements) -- aliases for the same shape.
//! - `<category>`: present per game for redump/no-intro/retroachievements/mame-redump,
//!   absent for tosec (whose dats are one category each, named in the header instead --
//!   the header's `<category>` is "TOSEC", the real category is the last " - " part of
//!   the header name).
//! - `<release name=... region=... language=.../>`: structured region, only in
//!   retroachievements dats in practice (No-Intro's schema allows it but the daily packs
//!   don't emit it) -- the parser leaves `region` `None` otherwise and `import` falls back
//!   to `matching::parse_name`.
//! - `cloneof`/`cloneofid`, `<ra_hash>`, `<source>`: read generically wherever present.
//! - `id` attribute: the source's own game id (redump/no-intro numeric or "xB04"-style).
//! - hash attributes per `<rom>`/`<disk>`: `crc`/`md5`/`sha1`/`sha256`, whichever the
//!   source computed; `serial`/`version` attributes where a source puts them on the rom.
//!
//! retrobios is the exception: its `database.json` isn't a Logiqx dat at all (see
//! [`parse_retrobios_json`]).
//!
//! [`parse_dat_path`] is the entry point for a file on disk: `.dat`/`.xml` directly, `.zip`
//! packs (every `.dat`/`.xml` member -- redump ships one zip per system, no-intro/tosec one
//! zip for everything), `.json` for retrobios.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use serde::Deserialize;

use super::error::DatabaseError;

#[derive(Debug, Clone, Default)]
pub struct DatHeader {
    pub name: String,
    pub description: String,
    pub version: Option<String>,
    pub category: Option<String>,
    pub date: Option<String>,
    pub author: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedFile {
    pub name: String,
    pub size: Option<u64>,
    pub crc32: Option<String>,
    pub md5: Option<String>,
    pub sha1: Option<String>,
    pub sha256: Option<String>,
    pub serial: Option<String>,
    pub version: Option<String>,
    /// `<disk>` (a CHD/RVZ container hash) rather than `<rom>`.
    pub is_disk: bool,
    /// Which build of a patched work this file is (`variant="v1.02"`): the versions of one
    /// patch are one release, and its files with the same variant are one dump of it. Only
    /// the DATs `dat_convert` writes carry it; the import works it out from the name
    /// otherwise (`patch_names::analyze`).
    pub variant: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedGame {
    pub name: String,
    pub description: Option<String>,
    pub category: Option<String>,
    /// Redump's `<serial>` element: one per game, where No-Intro puts a `serial` attribute
    /// on the rom instead.
    pub serial: Option<String>,
    /// The source's own id attribute, when given.
    pub source_game_id: Option<String>,
    /// Structured `<release region>` (see module doc); `None` when the source only bakes the
    /// region into `name`.
    pub region: Option<String>,
    pub language: Option<String>,
    /// `<release ... guessed="1">`: the language is an inference (a folder's name), not
    /// something the entry states.
    pub language_guessed: bool,
    /// `<system>`: the platform of this entry, for a DAT that covers several (`hacks.dat`).
    /// The header names the platform everywhere else.
    pub system: Option<String>,
    pub cloneof: Option<String>,
    /// RetroAchievements' `<source>` element (which DAT source RA's entry derives from).
    pub ra_source: Option<String>,
    pub files: Vec<ParsedFile>,
    /// RetroAchievements' own hash, given once per game (not per file) in its dats.
    pub ra_hash: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedDatFile {
    pub header: DatHeader,
    pub games: Vec<ParsedGame>,
    /// The XML ended with elements still open: a download cut short. What it holds is
    /// imported, but nothing it leaves out is taken off a release (`import::supersede_unlisted`).
    pub truncated: bool,
}

impl ParsedDatFile {
    pub fn file_count(&self) -> usize {
        self.games.iter().map(|g| g.files.len()).sum()
    }
}

fn attr(tag: &BytesStart, name: &[u8]) -> Option<String> {
    tag.attributes().flatten().find(|a| a.key.as_ref() == name).map(|a| a.unescape_value().map(|v| v.into_owned()).unwrap_or_default())
}

fn text_of(reader: &mut Reader<&[u8]>, buf: &mut Vec<u8>) -> Result<String, DatabaseError> {
    buf.clear();
    match reader.read_event_into(buf).map_err(|err| DatabaseError::Parse(err.to_string()))? {
        Event::Text(text) => text.unescape().map(|v| v.into_owned()).map_err(|err| DatabaseError::Parse(err.to_string())),
        Event::CData(cdata) => Ok(String::from_utf8_lossy(&cdata).into_owned()),
        _ => Ok(String::new()),
    }
}

fn file_from_tag(tag: &BytesStart, is_disk: bool) -> ParsedFile {
    ParsedFile {
        name: attr(tag, b"name").unwrap_or_default(),
        size: attr(tag, b"size").and_then(|v| v.parse().ok()),
        crc32: attr(tag, b"crc").filter(|v| !v.is_empty()),
        md5: attr(tag, b"md5").filter(|v| !v.is_empty()),
        sha1: attr(tag, b"sha1").filter(|v| !v.is_empty()),
        sha256: attr(tag, b"sha256").filter(|v| !v.is_empty()),
        serial: attr(tag, b"serial").filter(|v| !v.is_empty()),
        version: attr(tag, b"version").filter(|v| !v.is_empty()),
        is_disk,
        variant: attr(tag, b"variant").filter(|v| !v.is_empty()),
    }
}

/// Parses one Logiqx-family dat's XML text (see this module's doc comment for the field
/// variance across sources). Tolerant of the leading UTF-8 BOM some sources' files have
/// (TOSEC's do) and of unknown elements/attributes (skipped, not an error).
pub fn parse_dat_xml(xml: &str) -> Result<ParsedDatFile, DatabaseError> {
    let xml = xml.strip_prefix('\u{feff}').unwrap_or(xml);
    let mut reader = Reader::from_str(xml);
    reader.trim_text(true);

    let mut header = DatHeader::default();
    let mut games = Vec::new();
    let mut buf = Vec::new();
    let mut in_header = false;
    let mut game: Option<ParsedGame> = None;
    // The root element's name, and whether it was closed: a download cut short ends without.
    let mut root: Option<Vec<u8>> = None;
    let mut closed = false;

    loop {
        buf.clear();
        let event = reader.read_event_into(&mut buf).map_err(|err| DatabaseError::Parse(err.to_string()))?;
        match event {
            Event::Eof => break,
            Event::Start(tag) => {
                if root.is_none() {
                    root = Some(tag.name().as_ref().to_vec());
                }
                let name = tag.name();
                match name.as_ref() {
                    b"header" => in_header = true,
                    b"name" if in_header => header.name = text_of(&mut reader, &mut buf)?,
                    b"description" if in_header => header.description = text_of(&mut reader, &mut buf)?,
                    b"version" if in_header => header.version = Some(text_of(&mut reader, &mut buf)?),
                    b"category" if in_header => header.category = Some(text_of(&mut reader, &mut buf)?),
                    b"date" if in_header => header.date = Some(text_of(&mut reader, &mut buf)?),
                    b"author" if in_header => header.author = Some(text_of(&mut reader, &mut buf)?),
                    b"game" | b"machine" => {
                        let name = attr(&tag, b"name").unwrap_or_default();
                        let cloneof = attr(&tag, b"cloneof").or_else(|| attr(&tag, b"cloneofid"));
                        let source_game_id = attr(&tag, b"id");
                        game = Some(ParsedGame { name, cloneof, source_game_id, ..Default::default() });
                    }
                    b"description" if game.is_some() => {
                        let value = text_of(&mut reader, &mut buf)?;
                        if let Some(g) = &mut game {
                            g.description = (!value.is_empty()).then_some(value);
                        }
                    }
                    b"category" if game.is_some() => {
                        let value = text_of(&mut reader, &mut buf)?;
                        if let Some(g) = &mut game {
                            g.category = (!value.is_empty()).then_some(value);
                        }
                    }
                    b"serial" if game.is_some() => {
                        let value = text_of(&mut reader, &mut buf)?;
                        if let Some(g) = &mut game {
                            g.serial = (!value.is_empty()).then_some(value);
                        }
                    }
                    b"source" if game.is_some() => {
                        let value = text_of(&mut reader, &mut buf)?;
                        if let Some(g) = &mut game {
                            g.ra_source = (!value.is_empty()).then_some(value);
                        }
                    }
                    b"system" if game.is_some() => {
                        let value = text_of(&mut reader, &mut buf)?;
                        if let Some(g) = &mut game {
                            g.system = (!value.is_empty()).then_some(value);
                        }
                    }
                    b"release" => {
                        if let Some(g) = &mut game {
                            if g.region.is_none() {
                                g.region = attr(&tag, b"region").filter(|v| !v.is_empty());
                            }
                            if g.language.is_none() {
                                g.language = attr(&tag, b"language").filter(|v| !v.is_empty());
                                g.language_guessed = attr(&tag, b"guessed").is_some_and(|v| v == "1");
                            }
                        }
                    }
                    b"ra_hash" if game.is_some() => {
                        let value = text_of(&mut reader, &mut buf)?;
                        if let Some(g) = &mut game {
                            g.ra_hash = (!value.is_empty()).then_some(value);
                        }
                    }
                    b"rom" | b"disk" => {
                        // Non-empty form (<rom ...></rom>) -- same attributes.
                        let is_disk = name.as_ref() == b"disk";
                        if let Some(g) = &mut game {
                            g.files.push(file_from_tag(&tag, is_disk));
                        }
                    }
                    _ => {}
                }
            }
            Event::Empty(tag) => {
                let name = tag.name();
                match name.as_ref() {
                    b"rom" | b"disk" => {
                        let is_disk = name.as_ref() == b"disk";
                        if let Some(g) = &mut game {
                            g.files.push(file_from_tag(&tag, is_disk));
                        }
                    }
                    b"release" => {
                        if let Some(g) = &mut game {
                            if g.region.is_none() {
                                g.region = attr(&tag, b"region").filter(|v| !v.is_empty());
                            }
                            if g.language.is_none() {
                                g.language = attr(&tag, b"language").filter(|v| !v.is_empty());
                                g.language_guessed = attr(&tag, b"guessed").is_some_and(|v| v == "1");
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::End(tag) => {
                closed |= root.as_deref() == Some(tag.name().as_ref());
                match tag.name().as_ref() {
                    b"header" => in_header = false,
                    b"game" | b"machine" => {
                        if let Some(g) = game.take() {
                            if !g.name.is_empty() {
                                games.push(g);
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    Ok(ParsedDatFile { header, games, truncated: root.is_some() && !closed })
}

// ---------------------------------------------------------------------------
// retrobios: a flat JSON hash catalog, not a Logiqx dat -- see this module's doc comment.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RetrobiosDatabase {
    files: HashMap<String, RetrobiosEntry>,
}

#[derive(Debug, Deserialize)]
struct RetrobiosEntry {
    path: String,
    name: String,
    size: u64,
    #[serde(default, deserialize_with = "string_or_null")]
    crc32: String,
    #[serde(default, deserialize_with = "string_or_null")]
    md5: String,
    #[serde(default, deserialize_with = "string_or_null")]
    sha1: String,
    #[serde(default, deserialize_with = "string_or_null")]
    sha256: String,
}

/// A hash retrobios leaves out, or gives as `null`, reads as empty.
fn string_or_null<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

/// Path substrings that mean "emulator internal state, not a real BIOS/firmware file".
const JUNK_PATH_SUBSTRINGS: &[&str] =
    &["Citra/", "Mupen64plus", "shaders/", "GLideN64", "dolphin-mmjr", "extdata", "sdmc/", "nand/", "title/", "sysdata", "datfiles", "sys_modules", "LocalFriendCodeSeed", "SecureInfo", "Firmware_batocera", "barhi"];

fn is_plausible_bios_path(path: &str) -> bool {
    if JUNK_PATH_SUBSTRINGS.iter().any(|s| path.contains(s)) {
        return false;
    }
    match path.rsplit('.').next().map(|ext| ext.to_ascii_lowercase()) {
        Some(ext) => !matches!(ext.as_str(), "zip" | "7z" | "exe" | "ttf" | "lst" | "dat" | "ini" | "cfg"),
        None => true,
    }
}

/// Parses retrobios' `database.json`, filtering out the emulator-state noise it mixes in
/// with real BIOS files and normalizing what's left to one [`ParsedGame`] per BIOS file
/// (`category = Some("BIOS")`, one file, no region/cloneof/ra_hash). The `bios/<vendor>/
/// <system>/` path is kept in `description` so import can map a platform from it.
pub fn parse_retrobios_json(json: &str) -> Result<ParsedDatFile, DatabaseError> {
    let database: RetrobiosDatabase = serde_json::from_str(json).map_err(|err| DatabaseError::Parse(err.to_string()))?;

    let mut games: Vec<ParsedGame> = database
        .files
        .into_values()
        .filter(|entry| entry.path.starts_with("bios/") && is_plausible_bios_path(&entry.path))
        // A file with no hash at all can't be matched to anything.
        .filter(|entry| [&entry.crc32, &entry.md5, &entry.sha1, &entry.sha256].iter().any(|h| !h.trim().is_empty()))
        .map(|entry| ParsedGame {
            name: entry.name.clone(),
            description: Some(entry.path.clone()),
            category: Some("BIOS".to_string()),
            files: vec![ParsedFile {
                name: entry.name,
                size: Some(entry.size),
                // An empty hash is a hash retrobios doesn't have, not one every file matches.
                crc32: Some(entry.crc32).filter(|h| !h.trim().is_empty()),
                md5: Some(entry.md5).filter(|h| !h.trim().is_empty()),
                sha1: Some(entry.sha1).filter(|h| !h.trim().is_empty()),
                sha256: Some(entry.sha256).filter(|h| !h.trim().is_empty()),
                ..Default::default()
            }],
            ..Default::default()
        })
        .collect();
    games.sort_by(|a, b| a.description.cmp(&b.description).then(a.name.cmp(&b.name)));

    Ok(ParsedDatFile { header: DatHeader { name: "retrobios".to_string(), description: "BIOS/firmware hash catalog".to_string(), ..Default::default() }, games, truncated: false })
}

// ---------------------------------------------------------------------------
// files on disk
// ---------------------------------------------------------------------------

/// One parsed dat plus where it came from (the file, or `zip member` inside a pack).
#[derive(Debug)]
pub struct ParsedDatEntry {
    /// File name, or `"<zip file name>::<member path>"` for a pack member.
    pub name: String,
    pub dat: ParsedDatFile,
}

/// Parses a `.dat`/`.xml` file, every `.dat`/`.xml` member of a `.zip`, or a retrobios
/// `.json`. `member_filter` (zip packs only) decides which members to parse -- e.g. to
/// import one platform out of a whole-catalog pack.
pub fn parse_dat_path(path: &Path, member_filter: &dyn Fn(&str) -> bool) -> Result<Vec<ParsedDatEntry>, DatabaseError> {
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
    let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "json" => {
            let text = std::fs::read_to_string(path)?;
            Ok(vec![ParsedDatEntry { name: file_name, dat: parse_retrobios_json(&text)? }])
        }
        "zip" => parse_zip(path, &file_name, member_filter),
        _ => {
            let text = std::fs::read_to_string(path)?;
            Ok(vec![ParsedDatEntry { name: file_name, dat: parse_dat_xml(&text)? }])
        }
    }
}

/// Whether a member is one this parser reads at all.
fn is_dat_member(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".dat") || lower.ends_with(".xml")
}

/// Reads a zip pack with the `zip` crate rather than the bundled 7-Zip.
///
/// Deliberate, and measured twice.
///
/// The intuition is that the bundled 7-Zip should win, since it is the build used everywhere
/// else. It does not, for two reasons that only showed up on a stopwatch:
///
/// - `7z.so` links no zlib at all (`ldd` confirms it) -- a zip's deflate members go through
///   7-Zip's own `DeflateDecoder.cpp`. The zlib-ng in this repo is a separate library, used
///   for CHD's `zlib`/`cdzl` codecs. So "use the native build, it has zlib-ng" is not true of
///   this path.
/// - Measured on real packs, 7-Zip's decoder is about half the speed: 205 MB/s against this
///   reader's 365 on No-Intro's 49.6 MB IBM PC datfile, 196 vs 350 on the 36.3 MB Audio CD
///   one. That was with `extract_to_buffer` already fixed, so it is not a workaround cost --
///   the decoder is simply slower here.
///
/// zlib-ng's real advantage is compression, which this code never does.
fn parse_zip(path: &Path, file_name: &str, member_filter: &dyn Fn(&str) -> bool) -> Result<Vec<ParsedDatEntry>, DatabaseError> {
    let file = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut out = Vec::new();
    for index in 0..archive.len() {
        let mut member = archive.by_index(index)?;
        if member.is_dir() {
            continue;
        }
        let member_name = member.name().to_string();
        if !is_dat_member(&member_name) || !member_filter(&member_name) {
            continue;
        }
        let mut text = String::new();
        member.read_to_string(&mut text).map_err(|err| DatabaseError::Parse(format!("{member_name}: {err}")))?;
        let dat = parse_dat_xml(&text).map_err(|err| DatabaseError::Parse(format!("{member_name}: {err}")))?;
        out.push(ParsedDatEntry { name: format!("{file_name}::{member_name}"), dat });
    }
    Ok(out)
}

/// Member names of a zip pack (without parsing them), for choosing what to import.
pub fn list_dat_members(path: &Path) -> Result<Vec<String>, DatabaseError> {
    let file = std::fs::File::open(path)?;
    let archive = zip::ZipArchive::new(file)?;
    Ok(archive.file_names().filter(|n| is_dat_member(n)).map(str::to_string).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A DAT cut off mid-download parses, but says so: its import may add, never take off.
    #[test]
    fn a_dat_cut_short_is_marked_truncated() {
        let whole = r#"<?xml version="1.0"?><datafile><header><name>T</name><version></version></header><game name="A"><rom name="a.bin" size="1" crc="00000000"/></game><game name="B"><rom name="b.bin" size="1" crc="00000001"/></game></datafile>"#;
        let parsed = parse_dat_xml(whole).unwrap();
        assert!(!parsed.truncated, "an empty <version></version> must not read as cut short");
        assert_eq!(parsed.games.len(), 2);
        let cut = &whole[..whole.find("<game name=\"B\"").unwrap() + 20];
        let parsed = parse_dat_xml(cut).unwrap();
        assert!(parsed.truncated);
        assert_eq!(parsed.games.len(), 1);
    }

    #[test]
    fn parses_game_rom_shape_with_release_region() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <header><name>Test</name><description>Test dat</description><version>1</version></header>
    <game name="Example (Europe)" cloneof="Example (USA)" id="12">
        <category>Retail</category>
        <description>Example (Europe)</description>
        <release name="Example (Europe)" region="Europe"/>
        <rom name="Example (Europe).a26" size="2048" crc="7322EBC6" md5="e3600be9eb98146adafdc12d91323d0f" sha1="6b163aa967e4204a5bd98a59bd8e80f159004e34"/>
        <ra_hash>e3600be9eb98146adafdc12d91323d0f</ra_hash>
    </game>
</datafile>"#;
        let parsed = parse_dat_xml(xml).unwrap();
        assert_eq!(parsed.header.name, "Test");
        let game = &parsed.games[0];
        assert_eq!(game.name, "Example (Europe)");
        assert_eq!(game.cloneof.as_deref(), Some("Example (USA)"));
        assert_eq!(game.source_game_id.as_deref(), Some("12"));
        assert_eq!(game.category.as_deref(), Some("Retail"));
        assert_eq!(game.description.as_deref(), Some("Example (Europe)"));
        assert_eq!(game.region.as_deref(), Some("Europe"));
        assert_eq!(game.ra_hash.as_deref(), Some("e3600be9eb98146adafdc12d91323d0f"));
        assert_eq!(game.files[0].size, Some(2048));
        assert!(!game.files[0].is_disk);
    }

    #[test]
    fn reads_a_game_level_serial() {
        // Redump's "(serial,version)" DATs: the serial is an element of the game.
        let xml = "<datafile><header><name>x</name><description>x</description></header>            <game name=\"Example (Japan)\" id=\"1\"><category>Games</category><serial>VDS-293</serial><description>Example (Japan)</description>            <rom name=\"a.bin\" size=\"20\" crc=\"dd\" md5=\"ee\" sha1=\"ff\"/></game></datafile>";
        let parsed = parse_dat_xml(xml).unwrap();
        assert_eq!(parsed.games[0].serial.as_deref(), Some("VDS-293"));
        assert_eq!(parsed.games[0].files.len(), 1);
    }

    #[test]
    fn parses_machine_disk_shape() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <header><name>X - Y</name><description>d</description><category>TOSEC</category></header>
    <machine name="Example (Japan)">
        <category>Games</category>
        <source>Redump</source>
        <disk name="Example (Japan).chd" sha1="2e92a3760fec8d0020e7570975168f7b97ead974" />
    </machine>
</datafile>"#;
        let parsed = parse_dat_xml(xml).unwrap();
        assert_eq!(parsed.header.category.as_deref(), Some("TOSEC"));
        let game = &parsed.games[0];
        assert_eq!(game.region, None);
        assert_eq!(game.ra_source.as_deref(), Some("Redump"));
        assert!(game.files[0].is_disk);
        assert_eq!(game.files[0].size, None);
    }

    #[test]
    fn strips_bom_and_handles_multiple_roms() {
        let xml = "\u{feff}<?xml version=\"1.0\"?>\n<datafile><header><name>x</name><description>x</description></header>\
            <game name=\"Example (USA)\"><rom name=\"a.cue\" size=\"10\" crc=\"aa\" md5=\"bb\" sha1=\"cc\"/><rom name=\"a.bin\" size=\"20\" crc=\"dd\" md5=\"ee\" sha1=\"ff\" serial=\"XY-1\"/></game></datafile>";
        let parsed = parse_dat_xml(xml).unwrap();
        assert_eq!(parsed.games[0].files.len(), 2);
        assert_eq!(parsed.games[0].files[1].serial.as_deref(), Some("XY-1"));
    }

    #[test]
    fn retrobios_filters_noise() {
        let json = r#"{
            "schema_version": 1, "generated_at": "", "total_files": 2, "total_size": 0,
            "files": {
                "aaa": {"path": "bios/Vendor/System/firmware.bin", "name": "firmware.bin", "size": 524288, "crc32": "b1", "md5": "b2", "sha1": "b3", "sha256": "b4"},
                "bbb": {"path": "bios/Vendor/System/Citra/nand/private/movable.sed", "name": "movable.sed", "size": 320, "crc32": "c1", "md5": "c2", "sha1": "c3", "sha256": "c4"}
            }
        }"#;
        let parsed = parse_retrobios_json(json).unwrap();
        assert_eq!(parsed.games.len(), 1);
        assert_eq!(parsed.games[0].category.as_deref(), Some("BIOS"));
        assert_eq!(parsed.games[0].description.as_deref(), Some("bios/Vendor/System/firmware.bin"));
    }

    #[test]
    fn a_retrobios_hash_it_lacks_is_no_hash() {
        let json = r#"{
            "files": {
                "aaa": {"path": "bios/Vendor/System/a.bin", "name": "a.bin", "size": 10, "crc32": "", "md5": null, "sha1": "abc"},
                "bbb": {"path": "bios/Vendor/System/b.bin", "name": "b.bin", "size": 10, "crc32": "", "md5": "", "sha1": "", "sha256": ""}
            }
        }"#;
        let parsed = parse_retrobios_json(json).unwrap();
        assert_eq!(parsed.games.len(), 1, "a file with no hash at all is left out");
        let file = &parsed.games[0].files[0];
        assert_eq!((file.crc32.as_deref(), file.md5.as_deref(), file.sha1.as_deref(), file.sha256.as_deref()), (None, None, Some("abc"), None));
    }
}
