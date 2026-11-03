//! Archive-format sniffing and `.cue`/`.gdi` descriptor helpers.
//!
//! Ported from `reference/old_archiver.rs`'s `detect_archive_format_quick`/
//! `detect_archive_format_from_bytes` (7410/7429), `descriptor_payload_paths`/
//! `cue_payload_paths`/`gdi_payload_paths` (6452/6580/6612), `read_input_payload_bytes_with_progress`
//! (7454), and `preferred_archive_descriptor_member_name` (1579) -- **with the GD-ROM
//! cue/gdi bug fixed**: old code did `cue_member.or(gdi_member)`, always preferring `.cue`
//! when a listing had both, contradicting `docs/plan/rom_module.md`'s explicit "gdi is
//! preferred over cue." This port does `gdi_member.or(cue_member)`.
//!
//! Old code's zip/7z member listing and extraction shelled out to `unzip`/`bsdtar`
//! subprocesses (`list_members_with_unzip`, `list_members_with_bsdtar`,
//! `extract_member_with_unzip`, `extract_member_with_bsdtar`) -- all replaced here with
//! `super::super::sevenzip_wrapper::SevenZipArchive`, this session's native wrapper around
//! the bundled 7-Zip-zstd library, so archive handling no longer depends on external CLI
//! tools being on `PATH`.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::common::models::ArchiveFormat;

use super::super::sevenzip_wrapper::{self, SevenZipArchive};

fn to_sevenzip_format(format: ArchiveFormat) -> Option<sevenzip_wrapper::ArchiveFormat> {
    match format {
        ArchiveFormat::Zip => Some(sevenzip_wrapper::ArchiveFormat::Zip),
        ArchiveFormat::SevenZip => Some(sevenzip_wrapper::ArchiveFormat::SevenZ),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Archive format sniffing
// ---------------------------------------------------------------------------

/// Reads up to `len` bytes from the start of `path` for magic-byte sniffing. Ported from
/// `reference/old_archiver.rs`'s `read_detection_prefix` (line 7981), minus its
/// `chd_native`/`rvz_native` prefix-read special cases: [`detect_archive_format_quick`]
/// already resolves `.chd`/`.rvz` by extension before this is ever called for them, so that
/// native-logical-read path was dead for this call site anyway, and no other caller in the
/// new design needs a *logical* (decompressed) CHD/RVZ prefix -- just the container's own
/// leading bytes, which a plain file read already gives.
fn read_detection_prefix(path: &Path, len: usize) -> Result<Vec<u8>, String> {
    let mut file = File::open(path).map_err(|err| format!("failed to open {} for prefix read: {err}", path.display()))?;
    let mut buf = vec![0u8; len];
    let read_now = file.read(&mut buf).map_err(|err| format!("failed to read {}: {err}", path.display()))?;
    buf.truncate(read_now);
    Ok(buf)
}

/// Ported verbatim from `reference/old_archiver.rs`'s `detect_archive_format_from_bytes`
/// (line 7429).
pub fn detect_archive_format_from_bytes(bytes: &[u8], path: &Path) -> ArchiveFormat {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();

    if bytes.starts_with(b"MComprHD") || ext == "chd" {
        return ArchiveFormat::Chd;
    }
    if ext == "rvz" {
        return ArchiveFormat::Rvz;
    }
    if ext == "wia" {
        return ArchiveFormat::Wia;
    }
    if ext == "gcz" {
        return ArchiveFormat::Gcz;
    }
    if bytes.starts_with(&[0x50, 0x4b, 0x03, 0x04]) || ext == "zip" {
        return ArchiveFormat::Zip;
    }
    if bytes.starts_with(&[0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c]) || ext == "7z" {
        return ArchiveFormat::SevenZip;
    }
    if matches!(ext.as_str(), "iso" | "bin" | "img" | "raw" | "cue" | "gdi") {
        return ArchiveFormat::RawIso;
    }
    ArchiveFormat::Unknown
}

/// Cheap format sniff: extension first (avoids ever opening `.chd`/`.rvz`, which are large
/// and whose own container header a naive prefix read wouldn't usefully identify beyond the
/// extension anyway), then a 4096-byte magic-number probe. Ported from
/// `reference/old_archiver.rs`'s `detect_archive_format_quick` (line 7410).
pub fn detect_archive_format_quick(path: &Path) -> Result<ArchiveFormat, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if ext == "chd" {
        return Ok(ArchiveFormat::Chd);
    }
    if ext == "rvz" {
        return Ok(ArchiveFormat::Rvz);
    }
    if ext == "wia" {
        return Ok(ArchiveFormat::Wia);
    }
    if ext == "gcz" {
        return Ok(ArchiveFormat::Gcz);
    }

    let bytes = read_detection_prefix(path, 4096)?;
    Ok(detect_archive_format_from_bytes(&bytes, path))
}

// ---------------------------------------------------------------------------
// Zip / 7z member listing and extraction (via sevenzip_wrapper, no CLI dependency)
// ---------------------------------------------------------------------------

/// Lists every member path in a zip/7z archive. Replaces old code's `list_archive_members`
/// (which shelled out to `unzip -Z1` / `bsdtar -tf`) with `SevenZipArchive`.
pub fn list_archive_member_names(path: &Path, format: ArchiveFormat) -> Result<Vec<String>, String> {
    let sevenzip_format = to_sevenzip_format(format)
        .ok_or_else(|| format!("unsupported archive format for member listing: {format:?}"))?;
    let archive = SevenZipArchive::open(path, sevenzip_format, |_, _| true).map_err(|err| err.to_string())?;

    let count = archive.entry_count();
    let mut names = Vec::with_capacity(count.max(0) as usize);
    for index in 0..count {
        let entry = archive.entry(index).map_err(|err| err.to_string())?;
        if !entry.is_dir {
            names.push(entry.path);
        }
    }

    if names.is_empty() {
        return Err(format!("archive member list is empty: {}", path.display()));
    }
    Ok(names)
}

/// Extracts one named member's full bytes from a zip/7z archive.
pub fn read_archive_member_payload_by_name(path: &Path, format: ArchiveFormat, member_name: &str) -> Result<Vec<u8>, String> {
    let sevenzip_format = to_sevenzip_format(format)
        .ok_or_else(|| format!("unsupported archive format for member extraction: {format:?}"))?;
    let archive = SevenZipArchive::open(path, sevenzip_format, |_, _| true).map_err(|err| err.to_string())?;

    let count = archive.entry_count();
    for index in 0..count {
        let entry = archive.entry(index).map_err(|err| err.to_string())?;
        if entry.is_dir || entry.path != member_name {
            continue;
        }
        return archive.read_entry(index).map_err(|err| err.to_string());
    }

    Err(format!("member '{member_name}' not found in archive: {}", path.display()))
}

/// Extracts the archive's first regular (non-directory) member's bytes. Replaces old
/// code's `read_archive_member_payload` (unzip/bsdtar-based).
pub fn read_archive_member_payload(path: &Path, format: ArchiveFormat) -> Result<Vec<u8>, String> {
    let sevenzip_format = to_sevenzip_format(format)
        .ok_or_else(|| format!("unsupported archive format for member extraction: {format:?}"))?;
    let archive = SevenZipArchive::open(path, sevenzip_format, |_, _| true).map_err(|err| err.to_string())?;

    let count = archive.entry_count();
    for index in 0..count {
        let entry = archive.entry(index).map_err(|err| err.to_string())?;
        if entry.is_dir {
            continue;
        }
        return archive.read_entry(index).map_err(|err| err.to_string());
    }

    Err(format!("no regular member found in archive: {}", path.display()))
}

/// Reads a whole input's logical payload bytes regardless of container -- CHD/RVZ arms
/// return an error here (those formats no longer have a native in-process decoder; callers
/// needing their content must go through `super::super::extract` to get a real file first),
/// zip/7z extract their first regular member via [`read_archive_member_payload`], anything
/// else is read as a plain file. Ported from `reference/old_archiver.rs`'s
/// `read_input_payload_bytes_with_progress` (line 7454), minus its `chd_native`/`rvz_native`
/// streaming arms and its `on_progress` parameter (no caller in the new design streams this
/// large a read incrementally -- see `super::super::pipeline` for the one place that does,
/// which reads a real file, not an archive-member payload).
pub fn read_input_payload_bytes(path: &Path) -> Result<Vec<u8>, String> {
    match detect_archive_format_quick(path).unwrap_or(ArchiveFormat::Unknown) {
        format @ (ArchiveFormat::Chd | ArchiveFormat::Rvz | ArchiveFormat::Wia | ArchiveFormat::Gcz) => {
            Err(format!("{} is a {format:?} container -- extract it to a real file before reading its payload", path.display()))
        }
        ArchiveFormat::Zip => read_archive_member_payload(path, ArchiveFormat::Zip),
        ArchiveFormat::SevenZip => read_archive_member_payload(path, ArchiveFormat::SevenZip),
        _ => fs::read(path).map_err(|err| format!("failed to read {}: {err}", path.display())),
    }
}

// ---------------------------------------------------------------------------
// .cue / .gdi descriptor parsing
// ---------------------------------------------------------------------------

/// Resolves a `.cue`/`.gdi` descriptor file to the sibling payload files it references (its
/// actual track/bin data), in sheet order, each once, keeping only the ones that exist next
/// to the sheet. Returns an empty list for any other extension or an unparsable sheet. The
/// parsing itself is [`super::sheet::Sheet::parse`], which also keeps the names as written
/// (what an archive creator needs); this is the on-disk view of the same list.
pub fn descriptor_payload_paths(path: &Path) -> Vec<PathBuf> {
    let Ok(sheet) = super::sheet::Sheet::parse(path) else {
        return Vec::new();
    };
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    sheet.payload_paths(base).into_iter().filter(|candidate| candidate.is_file()).collect()
}

fn archive_member_has_extension(member: &str, expected_ext: &str) -> bool {
    Path::new(member).extension().and_then(|ext| ext.to_str()).map(|ext| ext.eq_ignore_ascii_case(expected_ext)).unwrap_or(false)
}

/// Picks which member of a zip/7z archive to treat as the disc descriptor when both a
/// `.cue` and a `.gdi` are present. **Fixed from old_archiver.rs**: old code did
/// `cue_member.or(gdi_member)` (line 1596 there), always preferring `.cue` -- this
/// contradicts `docs/plan/rom_module.md`'s explicit "gdi is preferred over cue" (GD-ROM
/// dumps are more completely and accurately described by their `.gdi` than by a `.cue`
/// sheet). This port does `gdi_member.or(cue_member)` instead.
pub fn preferred_archive_descriptor_member_name(archive_path: &Path, format: ArchiveFormat) -> Result<Option<String>, String> {
    let members = list_archive_member_names(archive_path, format)?;
    let mut cue_member: Option<String> = None;
    let mut gdi_member: Option<String> = None;

    for member in members.into_iter().filter(|name| !name.ends_with('/')) {
        if cue_member.is_none() && archive_member_has_extension(&member, "cue") {
            cue_member = Some(member.clone());
        }
        if gdi_member.is_none() && archive_member_has_extension(&member, "gdi") {
            gdi_member = Some(member);
        }
        if cue_member.is_some() && gdi_member.is_some() {
            break;
        }
    }

    Ok(gdi_member.or(cue_member))
}
