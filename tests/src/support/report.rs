

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// One original/extracted file pair's comparison, kept regardless of outcome so a failing
/// case's report still shows every file it checked, not just the first mismatch.
pub struct FileComparison {
    /// Path relative to the samples root, for readability.
    pub original: String,
    pub original_hash: String,
    /// Path relative to `tests/output/`.
    pub extracted: String,
    pub extracted_hash: String,
    pub matched: bool,
}

pub struct CaseResult {
    pub platform: String,
    pub variant: String,
    pub kind: &'static str,
    pub disc_kind: String,
    pub passed: bool,
    /// Empty on success; a short, non-sensitive failure reason otherwise (a track-count
    /// mismatch, a chdman/dolphin error message, ...). Per-file mismatches show up in
    /// `files` instead, not duplicated here.
    pub detail: String,
    pub elapsed: Duration,
    pub files: Vec<FileComparison>,
}

/// Writes `tests/results/<test_name>.md`, overwriting any previous run's report for the
/// same test. Reflects exactly the cases that were run (a filtered subset stays a small,
/// focused report).
pub fn write_report(test_name: &str, results: &[CaseResult]) {
    write_report_with_hash_label(test_name, results, "SHA-256");
}

/// Same as [`write_report`], with `hash_label` naming the hash algorithm the
/// `original_hash`/`extracted_hash` strings hold.
pub fn write_report_with_hash_label(test_name: &str, results: &[CaseResult], hash_label: &str) {
    let total = results.len();
    let passed = results.iter().filter(|r| r.passed).count();

    let mut out = String::new();
    out.push_str(&format!("# {test_name}\n\n"));
    out.push_str(&format!("Run at (unix seconds): {}\n\n", SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)));
    out.push_str(&format!("**{passed}/{total} passed**\n\n"));

    if total == 0 {
        out.push_str("No cases matched (check `ROMEXPLORER_SAMPLE_PLATFORM`/`ROMEXPLORER_SAMPLE_CODEC`, or that the samples root is populated).\n");
        write_file(test_name, &out);
        return;
    }

    out.push_str("## Summary\n\n");
    out.push_str("| Platform | Variant | Kind | Disc | Files | Result | Time (ms) | Detail |\n");
    out.push_str("|---|---|---|---|---|---|---|---|\n");
    for r in results {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
            escape(&r.platform),
            escape(&r.variant),
            r.kind,
            escape(&r.disc_kind),
            r.files.len(),
            if r.passed { "PASS" } else { "FAIL" },
            r.elapsed.as_millis(),
            escape(&r.detail),
        ));
    }

    out.push_str(&format!("\n## File hashes ({hash_label})\n\n"));
    for r in results {
        if r.files.is_empty() {
            continue;
        }
        out.push_str(&format!("### {} / {} ({}, {})\n\n", escape(&r.platform), escape(&r.variant), r.kind, escape(&r.disc_kind)));
        out.push_str(&format!("| # | Original | Original {hash_label} | Extracted | Extracted {hash_label} | Match |\n"));
        out.push_str("|---|---|---|---|---|---|\n");
        for (index, file) in r.files.iter().enumerate() {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                index + 1,
                escape(&file.original),
                file.original_hash,
                escape(&file.extracted),
                file.extracted_hash,
                if file.matched { "✓" } else { "MISMATCH" },
            ));
        }
        out.push('\n');
    }

    write_file(test_name, &out);
}

/// One `detection_trail` entry, as plain strings so this module doesn't need to depend on
/// `romexplorer::rom::detect::reason` types -- the caller formats `DetectionStepKind`
/// and the step's `Evidence` (via its `Display` impl -- see `reason::Evidence`) into this.
pub struct TrailEntry {
    pub kind: String,
    /// `Display` text of the step's `Evidence` -- the actual matched bytes/ASCII/filename
    /// this specific sample produced, e.g. "ASCII 'TITLE' at offset 0x7FC0-0x7FD5",
    /// not a description of what was checked.
    pub evidence: String,
}

pub struct MetadataResult {
    pub platform_dir: String,
    /// Path relative to the samples root.
    pub input: String,
    /// `Platform` debug string.
    pub expected: String,
    /// `Platform` debug string.
    pub actual: String,
    pub passed: bool,
    /// The real evidence that identified this platform -- the `Display` text of the
    /// detection trail's Header/Descriptor step (real matched bytes/ASCII/filename for this
    /// specific sample), not `RomReason` (which only says *which of the 4 detection modes*
    /// resolved it -- always "DeepCycle" in this test, since heuristics are off, and
    /// therefore not itself informative here).
    pub reason: String,
    pub title: Option<String>,
    pub serial: Option<String>,
    /// Whether the dump carries a format-specific header (`RomMetadata::headered`) --
    /// `None` for platforms with no such concept.
    pub headered: Option<bool>,
    /// Whether the dump's cartridge-specific encryption is still applied
    /// (`RomMetadata::encrypted`) -- `None` for platforms with no such concept.
    pub encrypted: Option<bool>,
    /// `RomByteOrder` debug string (`RomMetadata::byte_order`) -- `None` for every
    /// platform but N64.
    pub byte_order: Option<String>,
    /// Whether this dump's hash matched a known game in a platform's own hash database
    /// (`RomMetadata::hash_confirmed`) -- `None` for every platform without one (currently
    /// only NES/Famicom, via `convert::nes::FcDb`).
    pub hash_confirmed: Option<bool>,
    pub trail: Vec<TrailEntry>,
    pub elapsed: Duration,
}

/// Writes `tests/results/<test_name>.md`. Same overwrite-per-run behavior as
/// [`write_report`].
pub fn write_metadata_report(test_name: &str, results: &[MetadataResult]) {
    let total = results.len();
    let passed = results.iter().filter(|r| r.passed).count();

    let mut out = String::new();
    out.push_str(&format!("# {test_name}\n\n"));
    out.push_str(&format!("Run at (unix seconds): {}\n\n", SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)));
    out.push_str(&format!("**{passed}/{total} passed**\n\n"));

    if total == 0 {
        out.push_str("No cases matched (check `ROMEXPLORER_SAMPLE_PLATFORM`, or that the samples root is populated).\n");
        write_file(test_name, &out);
        return;
    }

    out.push_str("## Summary\n\n");
    out.push_str("| Platform dir | Input | Expected | Actual | Result | Evidence | Title | Serial | Format |\n");
    out.push_str("|---|---|---|---|---|---|---|---|---|\n");
    for r in results {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            escape(&r.platform_dir),
            escape(&r.input),
            r.expected,
            r.actual,
            if r.passed { "PASS" } else { "FAIL" },
            r.reason,
            r.title.as_deref().map(escape).unwrap_or_default(),
            r.serial.as_deref().map(escape).unwrap_or_default(),
            format_summary(r),
        ));
    }

    out.push_str("\n## Detection detail\n\n");
    for r in results {
        out.push_str(&format!(
            "### {} / {} -- expected {}, got {} ({})\n\n",
            escape(&r.platform_dir),
            escape(&r.input),
            r.expected,
            r.actual,
            if r.passed { "PASS" } else { "FAIL" }
        ));
        if let Some(title) = &r.title {
            out.push_str(&format!("- Title: {}\n", escape(title)));
        }
        if let Some(serial) = &r.serial {
            out.push_str(&format!("- Serial/Product ID: {}\n", escape(serial)));
        }
        if let Some(headered) = r.headered {
            out.push_str(&format!("- Headered: {}\n", if headered { "yes" } else { "no" }));
        }
        if let Some(encrypted) = r.encrypted {
            out.push_str(&format!("- Encrypted: {}\n", if encrypted { "yes" } else { "no" }));
        }
        if let Some(byte_order) = &r.byte_order {
            out.push_str(&format!("- Byte order: {byte_order}\n"));
        }
        if let Some(hash_confirmed) = r.hash_confirmed {
            out.push_str(&format!("- Hash confirmed: {}\n", if hash_confirmed { "yes" } else { "no" }));
        }
        out.push_str(&format!("- Evidence: {}\n", escape(&r.reason)));
        if r.trail.is_empty() {
            out.push_str("- Detection trail: (empty)\n");
        } else {
            out.push_str("- Detection trail:\n");
            for step in &r.trail {
                out.push_str(&format!("  - [{}] {}\n", step.kind, escape(&step.evidence)));
            }
        }
        out.push_str(&format!("- Time: {}ms\n\n", r.elapsed.as_millis()));
    }

    write_file(test_name, &out);
}

/// A compact one-cell summary of `headered`/`encrypted`/`byte_order`/`hash_confirmed` for
/// the Summary table's "Format" column -- only the ones actually `Some` for this case,
/// since most platforms have none of them. The full breakdown is in that case's own detail
/// section.
fn format_summary(r: &MetadataResult) -> String {
    let mut parts = Vec::new();
    if let Some(headered) = r.headered {
        parts.push(if headered { "headered".to_string() } else { "unheadered".to_string() });
    }
    if let Some(encrypted) = r.encrypted {
        parts.push(if encrypted { "encrypted".to_string() } else { "decrypted".to_string() });
    }
    if let Some(byte_order) = &r.byte_order {
        parts.push(byte_order.clone());
    }
    if let Some(hash_confirmed) = r.hash_confirmed {
        parts.push(if hash_confirmed { "hash-confirmed".to_string() } else { "hash-unconfirmed".to_string() });
    }
    parts.join(", ")
}

fn write_file(test_name: &str, contents: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/results");
    let _ = fs::create_dir_all(&dir);
    let _ = fs::write(dir.join(format!("{test_name}.md")), contents);
}

fn escape(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}
