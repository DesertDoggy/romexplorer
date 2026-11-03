//! Generates `app_root_ex/assets/biosdb.bin` from the already-downloaded retrobios JSON.
//! See `docs/plan/bios.md` and `src/rom/detect/systemfile.rs`'s module doc for the full
//! design (format, why this is a Rust binary reusing the crate's own platform-mapping
//! code rather than an external script).
//!
//! ```text
//! cargo run --bin biosdb_gen
//! ```
//! Reads `app_root_ex/assets/download/dats/retrobios/database_*.json` (the newest one, by
//! filename sort -- retrobios's own filenames are timestamped), writes
//! `app_root_ex/assets/biosdb.bin`, and prints a summary to stdout.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use romexplorer::common::models::Platform;
use romexplorer::database::dat_download::Source;
use romexplorer::database::dat_parse::parse_retrobios_json;
use romexplorer::database::import::{platform_for, ImportOptions};

/// Beyond what `parse_retrobios_json` already filters (`JUNK_PATH_SUBSTRINGS` and a
/// junk-extension list, `src/database/dat_parse.rs`) -- `.yaml` slips through as a real
/// extension (emulator config/index files like `GameIndex.yaml`, not BIOS data).
const EXTRA_JUNK_EXTENSIONS: &[&str] = &["yaml", "yml"];

fn main() {
    let json_path = find_latest_retrobios_json().expect("no retrobios database_*.json found under app_root_ex/assets/download/dats/retrobios/ -- run the DAT download first");
    println!("reading {}", json_path.display());
    let json = fs::read_to_string(&json_path).expect("read retrobios json");
    let dat = parse_retrobios_json(&json).expect("parse retrobios json");
    println!("retrobios reports {} candidate entries before extra filtering", dat.games.len());

    let mut entries: Vec<Entry> = Vec::new();
    let mut skipped_junk_ext = 0usize;
    let mut skipped_no_sha1 = 0usize;
    let mut skipped_unknown_platform = 0usize;
    let mut by_type: HashMap<&'static str, usize> = HashMap::new();

    for game in &dat.games {
        let Some(file) = game.files.first() else { continue };
        if is_extra_junk(&file.name) {
            skipped_junk_ext += 1;
            continue;
        }
        let Some(sha1_hex) = file.sha1.as_deref() else {
            skipped_no_sha1 += 1;
            continue;
        };
        let Some(sha1) = parse_sha1(sha1_hex) else {
            skipped_no_sha1 += 1;
            continue;
        };
        let platform = platform_for(&dat, game, Source::Retrobios, &ImportOptions::default());
        if platform == Platform::Unknown {
            skipped_unknown_platform += 1;
            continue;
        }
        let (file_type, other_kind) = classify(&file.name);
        let size = file.size.unwrap_or(0);

        *by_type.entry(type_label(file_type)).or_default() += 1;
        entries.push(Entry { platform, file_type, other_kind, size, sha1, name: file.name.clone() });
    }

    entries.sort_by(|a, b| (a.platform.igdb_id(), &a.name).cmp(&(b.platform.igdb_id(), &b.name)));

    println!("kept {} entries -- {:?}", entries.len(), by_type);
    println!(
        "skipped: {skipped_junk_ext} extra-junk-extension, {skipped_no_sha1} missing/unparseable sha1, {skipped_unknown_platform} unmapped platform"
    );

    let out_path = PathBuf::from("app_root_ex/assets/biosdb.bin");
    let bytes = encode(&entries);
    fs::write(&out_path, &bytes).expect("write biosdb.bin");
    println!("wrote {} ({} bytes)", out_path.display(), bytes.len());

    print_spot_checks(&entries);
}

struct Entry {
    platform: Platform,
    file_type: FileType,
    other_kind: OtherKind,
    size: u64,
    sha1: [u8; 20],
    name: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FileType {
    Bios,
    Firmware,
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum OtherKind {
    None,
    Font,
    Kanji,
    Dsp,
    /// Not produced by `classify` today (no keyword rule falls through to it) -- kept as
    /// a real tag (encoded as 3, matching `systemfile.rs`'s `OtherFileKind::Unspecified`)
    /// for a future generator rule that can't be neatly typed as Font/Kanji/Dsp.
    #[allow(dead_code)]
    Unspecified,
}

fn type_label(t: FileType) -> &'static str {
    match t {
        FileType::Bios => "Bios",
        FileType::Firmware => "Firmware",
        FileType::Other => "Other",
    }
}

fn find_latest_retrobios_json() -> Option<PathBuf> {
    let dir = PathBuf::from("app_root_ex/assets/download/dats/retrobios");
    let mut candidates: Vec<PathBuf> = fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    candidates.sort();
    candidates.pop()
}

fn is_extra_junk(name: &str) -> bool {
    match name.rsplit('.').next().map(|e| e.to_ascii_lowercase()) {
        Some(ext) => EXTRA_JUNK_EXTENSIONS.contains(&ext.as_str()),
        None => false,
    }
}

/// Classifies by filename pattern, not size (a `Firmware` file can be small, like NDS/
/// DSi's `Firmware.bin`, or huge, like PS3/Vita's `.PUP` -- see `docs/plan/bios.md`).
fn classify(name: &str) -> (FileType, OtherKind) {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".pup") {
        return (FileType::Firmware, OtherKind::None);
    }
    if lower == "firmware.bin" {
        return (FileType::Firmware, OtherKind::None);
    }
    if lower.contains("kanji") {
        return (FileType::Other, OtherKind::Kanji);
    }
    if lower.contains("font") || lower.contains("cgrom") || lower == "shared_font.bin" {
        return (FileType::Other, OtherKind::Font);
    }
    if lower.contains("dsp") {
        return (FileType::Other, OtherKind::Dsp);
    }
    (FileType::Bios, OtherKind::None)
}

fn parse_sha1(hex: &str) -> Option<[u8; 20]> {
    if hex.len() != 40 {
        return None;
    }
    let mut out = [0u8; 20];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

fn encode(entries: &[Entry]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"BIOD");
    buf.push(1u8); // version
    buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for entry in entries {
        buf.extend_from_slice(&entry.platform.igdb_id().to_le_bytes());
        let file_type_tag: u8 = match entry.file_type {
            FileType::Bios => 0,
            FileType::Firmware => 1,
            FileType::Other => 2,
        };
        buf.push(file_type_tag);
        let other_kind_tag: u8 = match entry.other_kind {
            OtherKind::Font => 0,
            OtherKind::Kanji => 1,
            OtherKind::Dsp => 2,
            OtherKind::Unspecified | OtherKind::None => 3,
        };
        buf.push(other_kind_tag);
        buf.extend_from_slice(&entry.size.to_le_bytes());
        buf.extend_from_slice(&entry.sha1);
        let name_bytes = entry.name.as_bytes();
        let name_len = name_bytes.len().min(255);
        buf.push(name_len as u8);
        buf.extend_from_slice(&name_bytes[..name_len]);
    }
    buf
}

/// A handful of known-platform/type sanity checks (per docs/plan/bios.md's Verification
/// section) -- printed only, no filenames/hashes written to any tracked file (the
/// no-sample-traces rule only restricts *tracked* files; stdout from a manually-run tool
/// is fine, same as this project's other dev-tool CLIs).
fn print_spot_checks(entries: &[Entry]) {
    let ps2_bios_max = entries.iter().filter(|e| e.platform == Platform::PlayStation2 && e.file_type == FileType::Bios).map(|e| e.size).max();
    println!("PlayStation2 Bios-type max size: {ps2_bios_max:?} (expect 4194304 to be present among entries)");

    let ps3_firmware = entries.iter().find(|e| e.platform == Platform::PlayStation3 && e.file_type == FileType::Firmware);
    println!("PlayStation3 Firmware entry found: {} (expect ~206MB, PS3UPDAT.PUP-class)", ps3_firmware.is_some());

    let msx_kanji = entries.iter().find(|e| e.platform == Platform::Msx && e.other_kind == OtherKind::Kanji);
    println!("Msx Other(Kanji) entry found: {}", msx_kanji.is_some());

    let nds_firmware = entries.iter().find(|e| e.platform == Platform::NintendoDs && e.file_type == FileType::Firmware);
    println!("NintendoDs Firmware entry found (small, per the user's correction): {}", nds_firmware.is_some());
}
