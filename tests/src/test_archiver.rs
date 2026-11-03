//! Acceptance tests against real sample disc images under the samples root (configured in
//! `support/inputs.rs`; ask for the sample asset bundle if it isn't populated locally). These
//! exercise the real rom pipeline (chdman/dolphin/sevenzip extraction) against real
//! files. For pure-logic detection tests against small synthetic fixtures, see
//! `test_detect.rs`.
//!
//!
//! ## Running a subset
//!
//! A full run can take a while.
//! Narrow it with either env var (case-insensitive substring match), alone or combined:
//! ```text
//! ROMEXPLORER_SAMPLE_PLATFORM=<dir name under the samples root> cargo test --test test_archiver extract
//! ROMEXPLORER_SAMPLE_CODEC=<zstd|uncompressed|bzip2|lzma|lzma2> cargo test --test test_archiver extract
//! ```
//! Future test kinds (metadata/archive/hash) get their own `#[test]` fn here, each
//! independently runnable the same way via `cargo test --test test_archiver <name>` --
//! `extract` is the only one implemented so far.
//!
//! ## Inspecting extracted output
//!
//! Each case extracts into its own subdirectory under `tests/output/` (git-ignored), which
//! is deleted after a passing case and left behind after a failing one, so you can inspect
//! what went wrong. Set `ROMEXPLORER_KEEP_OUTPUT=1` to keep it for every case, pass or fail
//! -- e.g. to hash the extracted files yourself in another tool and cross-check against the
//! per-file table in `tests/results/extract_disc_archives.md`.

#[path = "support/mod.rs"]
mod support;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use romexplorer::rom::archive::{self, ArchiveDestination, ArchiveError, ArchiveOptions, ArchiveTarget, ChdMedia, Codec};
use romexplorer::rom::models::RomMetadata;
use romexplorer::rom::chd_info;
use romexplorer::rom::detect::reason::DetectionStepKind;
use romexplorer::rom::detect::{self, descriptor};
use romexplorer::rom::extract::{self, ExtractDestination, ExtractError};
use romexplorer::rom::platforms;
use romexplorer::common::models::{ArchiveFormat, Platform};

use support::database::Database;
use support::discover::{self, ChdCase, OriginalRef, RvzCase};
use support::metadata_cases::{self, MetadataCase};
use support::report::{self, CaseResult, FileComparison, MetadataResult, TrailEntry};

fn samples_root() -> PathBuf {
    support::inputs::samples_root()
}

fn output_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/output")
}

/// Positional read that doesn't share a cursor across threads: `pread` on unix, `seek_read`
/// (which moves this handle's own cursor, harmless here) on Windows.
#[cfg(unix)]
fn read_exact_at(file: &std::fs::File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

#[cfg(windows)]
fn read_exact_at(file: &std::fs::File, mut buf: &mut [u8], mut offset: u64) -> std::io::Result<()> {
    while !buf.is_empty() {
        match std::os::windows::fs::FileExt::seek_read(file, buf, offset) {
            Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn env_filter(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn passes_filters(platform_filter: &Option<String>, codec_filter: &Option<String>, platform: &str, codec: Option<&str>) -> bool {
    if let Some(filter) = platform_filter {
        if !platform.to_ascii_lowercase().contains(&filter.to_ascii_lowercase()) {
            return false;
        }
    }
    if let Some(filter) = codec_filter {
        let Some(codec) = codec else { return false };
        if !codec.to_ascii_lowercase().contains(&filter.to_ascii_lowercase()) {
            return false;
        }
    }
    true
}

fn sanitize(value: &str) -> String {
    value.chars().map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' }).collect()
}

fn passes_platform_filter(filter: &Option<String>, platform: &str) -> bool {
    filter.as_ref().map(|f| platform.to_ascii_lowercase().contains(&f.to_ascii_lowercase())).unwrap_or(true)
}

#[test]
fn extract_disc_archives_roundtrip() {
    let samples_root = samples_root();
    let output_root = output_root();
    if !samples_root.is_dir() {
        eprintln!("samples root not present -- skipping (configure it to run this test)");
        return;
    }

    let platform_filter = env_filter("ROMEXPLORER_SAMPLE_PLATFORM");
    let codec_filter = env_filter("ROMEXPLORER_SAMPLE_CODEC");
    let keep_output = std::env::var_os("ROMEXPLORER_KEEP_OUTPUT").is_some();

    let cases = discover::discover_disc_archive_cases(&samples_root);
    let chd_cases: Vec<&ChdCase> = cases.chd.iter().filter(|case| passes_filters(&platform_filter, &codec_filter, &case.platform, None)).collect();
    let rvz_cases: Vec<&RvzCase> =
        cases.rvz.iter().filter(|case| passes_filters(&platform_filter, &codec_filter, &case.platform, Some(&case.codec))).collect();

    // Build/refresh the original-hash cache for exactly the cases this run will use,
    // before extracting anything -- see Database::warm's doc comment for why this has to
    // happen as its own up-front pass rather than interleaved per case.
    let original_files = discover::original_files(&chd_cases, &rvz_cases);

    let mut db = Database::open("originals");
    let warm_start = Instant::now();
    db.warm(&samples_root, &original_files);
    eprintln!("warmed {} original hash(es) in {}ms", original_files.len(), warm_start.elapsed().as_millis());

    let mut results = Vec::new();
    for &case in &chd_cases {
        results.push(run_chd_case(&samples_root, &output_root, case, &mut db, keep_output));
    }
    for &case in &rvz_cases {
        results.push(run_rvz_case(&samples_root, &output_root, case, &mut db, keep_output));
    }

    db.save();
    report::write_report("extract_disc_archives", &results);

    if results.is_empty() {
        eprintln!("no sample cases matched -- check ROMEXPLORER_SAMPLE_PLATFORM/ROMEXPLORER_SAMPLE_CODEC and the samples root");
        return;
    }

    let failed = results.iter().filter(|r| !r.passed).count();
    assert_eq!(failed, 0, "{failed} of {} disc archive extraction case(s) failed -- see tests/results/extract_disc_archives.md", results.len());
}

fn run_chd_case(samples_root: &Path, output_root: &Path, case: &ChdCase, db: &mut Database, keep_output: bool) -> CaseResult {
    let start = Instant::now();
    let scratch = output_root.join(format!("chd_{}_{}", sanitize(&case.platform), sanitize(&case.variant)));
    let _ = std::fs::remove_dir_all(&scratch);

    let outcome = run_chd_case_inner(samples_root, output_root, case, &scratch, db);
    let elapsed = start.elapsed();

    let (disc_kind, files, structural_error) = match outcome {
        Ok((disc_kind, files)) => (disc_kind, files, String::new()),
        Err(error) => ("?".to_string(), Vec::new(), error),
    };
    let passed = structural_error.is_empty() && files.iter().all(|file| file.matched);
    let detail = if passed {
        String::new()
    } else if !structural_error.is_empty() {
        structural_error
    } else {
        let mismatched = files.iter().filter(|file| !file.matched).count();
        format!("{mismatched} of {} file(s) mismatched", files.len())
    };

    // Clean up scratch output on success; leave it behind on failure for inspection, or
    // always when ROMEXPLORER_KEEP_OUTPUT is set.
    if passed && !keep_output {
        let _ = std::fs::remove_dir_all(&scratch);
    }

    CaseResult { platform: case.platform.clone(), variant: case.variant.clone(), kind: "chd", disc_kind, passed, detail, elapsed, files }
}

fn run_chd_case_inner(
    samples_root: &Path,
    output_root: &Path,
    case: &ChdCase,
    scratch: &Path,
    db: &mut Database,
) -> Result<(String, Vec<FileComparison>), String> {
    // Track count decides chdman's --splitbin: passing it for a single-track disc would
    // still tag that one file "(Track 01)", which is wrong for a disc that only ever had
    // one track -- see extract::extract_chd's doc comment.
    let metadata = chd_info::read_chd_metadata(&case.chd_path).map_err(|err| format!("chdman info failed: {err}"))?;

    let payload = extract::extract(
        &case.chd_path,
        ArchiveFormat::Chd,
        ExtractDestination::Temp(scratch.to_path_buf()),
        Some(&metadata),
        case.cd_output_format,
        |_, _| true,
    )
    .map_err(|err| format!("extraction failed: {err}"))?;

    match &case.original {
        OriginalRef::SingleFile(original) => {
            let comparison = compare_one(samples_root, output_root, original, &payload.primary, db);
            Ok(("dvd".to_string(), vec![comparison]))
        }
        OriginalRef::Descriptor(descriptor_path) => {
            let originals = descriptor::descriptor_payload_paths(descriptor_path);
            if originals.len() != payload.siblings.len() {
                return Err(format!(
                    "track count mismatch: original descriptor lists {}, extraction produced {}",
                    originals.len(),
                    payload.siblings.len()
                ));
            }

            let files: Vec<FileComparison> = originals
                .iter()
                .zip(payload.siblings.iter())
                .map(|(original, extracted)| compare_one(samples_root, output_root, original, extracted, db))
                .collect();

            let disc_kind = if originals.len() > 1 { format!("cd ({} tracks)", originals.len()) } else { "cd (1 track)".to_string() };
            Ok((disc_kind, files))
        }
    }
}

fn run_rvz_case(samples_root: &Path, output_root: &Path, case: &RvzCase, db: &mut Database, keep_output: bool) -> CaseResult {
    let start = Instant::now();
    let scratch = output_root.join(format!("rvz_{}_{}", sanitize(&case.platform), sanitize(&case.codec)));
    let _ = std::fs::remove_dir_all(&scratch);

    let outcome = extract::extract(
        &case.rvz_path,
        ArchiveFormat::Rvz,
        ExtractDestination::Temp(scratch.clone()),
        None,
        extract::CdOutputFormat::default(),
        |_, _| true,
    )
    .map_err(|err| format!("extraction failed: {err}"));

    let (files, structural_error) = match outcome {
        Ok(payload) => (vec![compare_one(samples_root, output_root, &case.original_iso, &payload.primary, db)], String::new()),
        Err(error) => (Vec::new(), error),
    };
    let elapsed = start.elapsed();

    let passed = structural_error.is_empty() && files.iter().all(|file| file.matched);
    let detail = if passed { String::new() } else if !structural_error.is_empty() { structural_error } else { "content mismatch".to_string() };

    if passed && !keep_output {
        let _ = std::fs::remove_dir_all(&scratch);
    }

    CaseResult { platform: case.platform.clone(), variant: case.codec.clone(), kind: "rvz", disc_kind: "dvd".to_string(), passed, detail, elapsed, files }
}

/// Hashes both sides of one original/extracted pair and records the comparison
/// unconditionally -- a hashing failure shows up as an `ERROR: ...` placeholder hash (and
/// `matched: false`) rather than aborting the whole case, so a report always shows every
/// file a case checked.
fn compare_one(samples_root: &Path, output_root: &Path, original: &Path, extracted: &Path, db: &mut Database) -> FileComparison {
    let original_hash = db.hash_cached(samples_root, original).unwrap_or_else(|err| format!("ERROR: {err}"));
    let extracted_hash = support::hashing::sha256_file(extracted).unwrap_or_else(|err| format!("ERROR: {err}"));
    let matched = !original_hash.starts_with("ERROR:") && !extracted_hash.starts_with("ERROR:") && original_hash == extracted_hash;

    FileComparison {
        original: relative_display(samples_root, original),
        original_hash,
        extracted: relative_display(output_root, extracted),
        extracted_hash,
        matched,
    }
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

/// Calls `platforms::deep_try_platform` directly -- **not** `detect::read_metadata_for_input`
/// -- for every sample the ground-truth table in `support::metadata_cases` covers, with
/// `HeuristicFlags::new(false, false)` so the extension/path heuristics contribute nothing
/// to the outcome. This is deliberately a test of the per-platform content parser itself
/// (does *this specific platform's* own header/descriptor check recognize its own sample?),
/// not of the heuristic-driven candidate narrowing `read_metadata_for_input` layers on top
/// -- that's `read_metadata_for_input`'s own job, exercised by the small end-to-end cases in
/// `test_detect.rs` instead. See `metadata_cases`'s doc comment for why this test -- unlike
/// every other one here -- names sample directories and extensions directly.
///
/// Every case bypasses heuristics entirely, so `RomMetadata::reason` is left at its
/// `Default` (`RomReason::DeepCycle`) rather than being set by `read_metadata_for_input`'s
/// mode system -- expected, not a bug: there is no "mode" here, only the one parser call.
///
/// Only the final platform is asserted (a `None` return, or the platform ending up
/// reassigned away from `expected` by e.g. the 3DS KTR-/CTR- disambiguation, both count as
/// failures). Title/serial/detection-trail evidence are recorded in the report regardless
/// of outcome, not separately asserted -- several platforms genuinely have no content
/// parser yet (`Nintendo64Dd`, `PocketStation`, Sega CD/Saturn's generic disc fallback,
/// WiiU's `.key`), so an empty title/serial there is expected, not a bug.
#[test]
fn metadata_platform_detection() {
    let samples_root = samples_root();
    if !samples_root.is_dir() {
        eprintln!("samples root not present -- skipping (configure it to run this test)");
        return;
    }

    let platform_filter = env_filter("ROMEXPLORER_SAMPLE_PLATFORM");

    let cases: Vec<MetadataCase> = metadata_cases::discover_metadata_cases(&samples_root)
        .into_iter()
        .filter(|case| passes_platform_filter(&platform_filter, &case.platform_dir))
        .collect();

    let results: Vec<MetadataResult> = cases.iter().map(|case| run_metadata_case(&samples_root, case)).collect();

    report::write_metadata_report("metadata_platform_detection", &results);

    if results.is_empty() {
        eprintln!("no sample cases matched -- check ROMEXPLORER_SAMPLE_PLATFORM and the samples root");
        return;
    }

    let failed = results.iter().filter(|r| !r.passed).count();
    assert_eq!(
        failed,
        0,
        "{failed} of {} metadata detection case(s) failed -- see tests/results/metadata_platform_detection.md",
        results.len()
    );
}

fn run_metadata_case(samples_root: &Path, case: &MetadataCase) -> MetadataResult {
    let start = Instant::now();

    let ext = case.input.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let file_size = std::fs::metadata(&case.input).ok().map(|m| m.len());
    // Heuristics off: this tests whether the platform's own content parser recognizes its
    // own sample, not whether the extension/path heuristics can guess it.
    let no_heuristics = detect::HeuristicFlags::new(false, false);

    let result = platforms::deep_try_platform(&case.input, &ext, file_size, case.expected, no_heuristics);
    let elapsed = start.elapsed();

    let actual = result.as_ref().map(|meta| meta.platform).unwrap_or(Platform::Unknown);
    let trail = result
        .as_ref()
        .map(|meta| meta.detection_trail.iter().map(|step| TrailEntry { kind: format!("{:?}", step.kind), evidence: step.evidence.to_string() }).collect())
        .unwrap_or_default();

    MetadataResult {
        platform_dir: case.platform_dir.clone(),
        input: relative_display(samples_root, &case.input),
        expected: format!("{:?}", case.expected),
        actual: format!("{:?}", actual),
        passed: actual == case.expected,
        reason: match &result {
            Some(meta) => primary_evidence_text(meta),
            None => "parser rejected its own target platform (no evidence)".to_string(),
        },
        title: result.as_ref().and_then(|meta| meta.title.clone()),
        serial: result.as_ref().and_then(|meta| meta.serial_product_id.clone()),
        headered: result.as_ref().and_then(|meta| meta.headered),
        encrypted: result.as_ref().and_then(|meta| meta.encrypted),
        byte_order: result.as_ref().and_then(|meta| meta.byte_order).map(|order| format!("{order:?}")),
        hash_confirmed: result.as_ref().and_then(|meta| meta.hash_confirmed),
        trail,
        elapsed,
    }
}

/// The report's "Evidence" column: the actual matched bytes/ASCII/filename this sample
/// produced, from every Header/Descriptor/Inference step (the steps that actually read
/// platform-specific content) -- not `meta.reason` (`RomReason`), which only records *which
/// of the 4 detection modes* resolved the metadata and is always "DeepCycle" in this test,
/// since heuristics are off. Several formats need more than one step to actually
/// distinguish two sibling platforms (GB vs. GBC's CGB flag on top of the shared title
/// field, NGP vs. NGPC's mode byte, ...) -- joining all of them, rather than picking only
/// the last, keeps the Summary column from silently dropping half the real evidence.
fn primary_evidence_text(meta: &RomMetadata) -> String {
    let joined = meta
        .detection_trail
        .iter()
        .filter(|step| matches!(step.kind, DetectionStepKind::Header | DetectionStepKind::Descriptor | DetectionStepKind::Inference))
        .map(|step| step.evidence.to_string())
        .collect::<Vec<_>>()
        .join("; ");
    if !joined.is_empty() {
        return joined;
    }
    meta.detection_trail.last().map(|step| step.evidence.to_string()).unwrap_or_else(|| "no evidence recorded".to_string())
}

// ---------------------------------------------------------------------------
// Archive creation roundtrips: create with `archive::archive`, extract again with
// `extract::extract`, and compare content (XXH3-128) against the original input(s).
// ---------------------------------------------------------------------------

struct CreateCase {
    label: String,
    codec: Option<Codec>,
    inputs: Vec<PathBuf>,
    target: ArchiveTarget,
}

impl CreateCase {
    fn variant(&self) -> String {
        self.codec.map(|codec| format!("{codec:?}")).unwrap_or_else(|| "default".to_string())
    }
}

fn target_extension(target: ArchiveTarget) -> &'static str {
    match target {
        ArchiveTarget::Chd => "chd",
        ArchiveTarget::Rvz => "rvz",
        ArchiveTarget::Gcz => "gcz",
        ArchiveTarget::Wia => "wia",
        ArchiveTarget::Zip => "zip",
        ArchiveTarget::SevenZip => "7z",
    }
}

fn xxh3_cached(cache: &mut HashMap<PathBuf, String>, path: &Path) -> String {
    cache
        .entry(path.to_path_buf())
        .or_insert_with(|| support::hashing::xxh3_file(path).unwrap_or_else(|err| format!("ERROR: {err}")))
        .clone()
}

fn compare_xxh3(samples_root: &Path, output_root: &Path, original: &Path, extracted: &Path, cache: &mut HashMap<PathBuf, String>) -> FileComparison {
    let original_hash = xxh3_cached(cache, original);
    let extracted_hash = support::hashing::xxh3_file(extracted).unwrap_or_else(|err| format!("ERROR: {err}"));
    let matched = !original_hash.starts_with("ERROR:") && !extracted_hash.starts_with("ERROR:") && original_hash == extracted_hash;
    FileComparison {
        original: relative_display(samples_root, original),
        original_hash,
        extracted: relative_display(output_root, extracted),
        extracted_hash,
        matched,
    }
}

fn run_create_case(samples_root: &Path, output_root: &Path, case: &CreateCase, cache: &mut HashMap<PathBuf, String>, keep_output: bool) -> CaseResult {
    let start = Instant::now();
    let scratch = output_root.join(format!("create_{}_{}_{}", sanitize(&case.label), sanitize(&case.variant()), target_extension(case.target)));
    let _ = std::fs::remove_dir_all(&scratch);

    let options = ArchiveOptions { codec: case.codec, ..Default::default() };
    let mut disc_kind = "?".to_string();
    let outcome = archive::archive(&case.inputs, case.target, &options, ArchiveDestination::Temp(scratch.join("created")), |_, _| true)
        .map_err(|err| format!("creation failed: {err}"))
        .and_then(|created| {
            if let Ok(meta) = std::fs::metadata(&created.primary) {
                disc_kind = format!("{} B", meta.len());
            }
            verify_created(samples_root, output_root, case, &created.primary, &scratch.join("extracted"), cache)
        });
    let elapsed = start.elapsed();

    let (files, structural_error) = match outcome {
        Ok(files) => (files, String::new()),
        Err(error) => (Vec::new(), error),
    };
    let passed = structural_error.is_empty() && !files.is_empty() && files.iter().all(|file| file.matched);
    let detail = if passed {
        String::new()
    } else if !structural_error.is_empty() {
        structural_error
    } else {
        format!("{} of {} file(s) mismatched", files.iter().filter(|file| !file.matched).count(), files.len())
    };

    if passed && !keep_output {
        let _ = std::fs::remove_dir_all(&scratch);
    }

    CaseResult { platform: case.label.clone(), variant: case.variant(), kind: target_extension(case.target), disc_kind, passed, detail, elapsed, files }
}

fn verify_created(
    samples_root: &Path,
    output_root: &Path,
    case: &CreateCase,
    created: &Path,
    extract_dir: &Path,
    cache: &mut HashMap<PathBuf, String>,
) -> Result<Vec<FileComparison>, String> {
    let dest = || ExtractDestination::Temp(extract_dir.to_path_buf());
    let input = &case.inputs[0];
    let input_ext = input.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();

    match case.target {
        ArchiveTarget::Chd => {
            let metadata = chd_info::read_chd_metadata(created).map_err(|err| format!("chdman info failed: {err}"))?;
            let cd_format = if input_ext == "gdi" { extract::CdOutputFormat::Gdi } else { extract::CdOutputFormat::CueBin };
            let payload = extract::extract(created, ArchiveFormat::Chd, dest(), Some(&metadata), cd_format, |_, _| true).map_err(|err| format!("extraction failed: {err}"))?;

            if matches!(input_ext.as_str(), "cue" | "gdi") {
                let originals = descriptor::descriptor_payload_paths(input);
                if originals.len() != payload.siblings.len() {
                    return Err(format!("track count mismatch: original lists {}, extraction produced {}", originals.len(), payload.siblings.len()));
                }
                Ok(originals.iter().zip(payload.siblings.iter()).map(|(original, extracted)| compare_xxh3(samples_root, output_root, original, extracted, cache)).collect())
            } else {
                Ok(vec![compare_xxh3(samples_root, output_root, input, &payload.primary, cache)])
            }
        }
        ArchiveTarget::Rvz | ArchiveTarget::Wia | ArchiveTarget::Gcz => {
            let format = match case.target {
                ArchiveTarget::Rvz => ArchiveFormat::Rvz,
                ArchiveTarget::Wia => ArchiveFormat::Wia,
                _ => ArchiveFormat::Gcz,
            };
            let payload = extract::extract(created, format, dest(), None, extract::CdOutputFormat::default(), |_, _| true).map_err(|err| format!("extraction failed: {err}"))?;
            Ok(vec![compare_xxh3(samples_root, output_root, input, &payload.primary, cache)])
        }
        ArchiveTarget::Zip | ArchiveTarget::SevenZip => {
            let format = if case.target == ArchiveTarget::Zip { ArchiveFormat::Zip } else { ArchiveFormat::SevenZip };
            let payload = extract::extract(created, format, dest(), None, extract::CdOutputFormat::default(), |_, _| true).map_err(|err| format!("extraction failed: {err}"))?;
            let extracted: Vec<PathBuf> = std::iter::once(payload.primary.clone()).chain(payload.siblings.iter().cloned()).collect();
            let mut files = Vec::new();
            for original in &case.inputs {
                let extracted_path = extracted
                    .iter()
                    .find(|path| path.file_name() == original.file_name())
                    .ok_or_else(|| format!("member {} not found in the archive", original.file_name().and_then(|n| n.to_str()).unwrap_or("?")))?;
                files.push(compare_xxh3(samples_root, output_root, original, extracted_path, cache));
            }
            Ok(files)
        }
    }
}

fn run_create_cases(name: &str, cases: Vec<CreateCase>) {
    let samples_root = samples_root();
    let output_root = output_root();
    let platform_filter = env_filter("ROMEXPLORER_SAMPLE_PLATFORM");
    let codec_filter = env_filter("ROMEXPLORER_SAMPLE_CODEC");
    let keep_output = std::env::var_os("ROMEXPLORER_KEEP_OUTPUT").is_some();

    let mut cache = HashMap::new();
    let results: Vec<CaseResult> = cases
        .iter()
        .filter(|case| passes_filters(&platform_filter, &codec_filter, &case.label, Some(&case.variant())))
        .map(|case| run_create_case(&samples_root, &output_root, case, &mut cache, keep_output))
        .collect();

    report::write_report_with_hash_label(name, &results, "XXH3-128");

    if results.is_empty() {
        eprintln!("no cases ran -- check ROMEXPLORER_SAMPLE_PLATFORM/ROMEXPLORER_SAMPLE_CODEC and that the sample inputs exist");
        return;
    }
    let failed = results.iter().filter(|r| !r.passed).count();
    assert_eq!(failed, 0, "{failed} of {} case(s) failed -- see tests/results/{name}.md", results.len());
}

#[test]
fn archive_chd_roundtrip() {
    let samples_root = samples_root();
    if !samples_root.is_dir() {
        eprintln!("samples root not present -- skipping");
        return;
    }
    let inputs = support::archive_cases::select_disc_inputs(&samples_root);
    let mut cases = Vec::new();

    if let Some(path) = inputs.gdi {
        cases.push(CreateCase { label: "chd from gdi".to_string(), codec: None, inputs: vec![path], target: ArchiveTarget::Chd });
    } else {
        eprintln!("no gdi input found");
    }
    if let Some(path) = inputs.cue {
        for codec in [None, Some(Codec::Zstd), Some(Codec::Lzma), Some(Codec::Zlib), Some(Codec::Flac)] {
            cases.push(CreateCase { label: "chd from cue".to_string(), codec, inputs: vec![path.clone()], target: ArchiveTarget::Chd });
        }
    } else {
        eprintln!("no multi-track cue input found");
    }
    if let Some(path) = inputs.dvd_iso {
        for codec in [None, Some(Codec::Zstd), Some(Codec::Lzma), Some(Codec::Zlib), Some(Codec::Flac), Some(Codec::Huff)] {
            cases.push(CreateCase { label: "chd from iso".to_string(), codec, inputs: vec![path.clone()], target: ArchiveTarget::Chd });
        }
    } else {
        eprintln!("no iso input found");
    }

    run_create_cases("archive_chd_roundtrip", cases);
}

#[test]
fn archive_dolphin_roundtrip() {
    let samples_root = samples_root();
    if !samples_root.is_dir() {
        eprintln!("samples root not present -- skipping");
        return;
    }
    let Some(iso) = support::archive_cases::select_disc_inputs(&samples_root).dolphin_iso else {
        eprintln!("no disc-image input found for the Dolphin formats");
        return;
    };

    let mut cases = Vec::new();
    let mut add = |target: ArchiveTarget, codecs: &[Option<Codec>]| {
        for &codec in codecs {
            cases.push(CreateCase { label: format!("{} from iso", target_extension(target)), codec, inputs: vec![iso.clone()], target });
        }
    };
    add(ArchiveTarget::Rvz, &[Some(Codec::Zstd), Some(Codec::Lzma), Some(Codec::Lzma2), Some(Codec::Bzip2), Some(Codec::None)]);
    add(ArchiveTarget::Wia, &[Some(Codec::Bzip2), Some(Codec::Lzma), Some(Codec::Lzma2), Some(Codec::None)]);
    add(ArchiveTarget::Gcz, &[None]);

    run_create_cases("archive_dolphin_roundtrip", cases);
}

#[test]
fn archive_files_roundtrip() {
    let samples_root = samples_root();
    let Some(fixtures) = support::archive_cases::file_archive_fixtures(&samples_root) else {
        eprintln!("no file fixtures found -- skipping");
        return;
    };
    let controls = fixtures.controls;
    assert!(!controls.is_empty(), "no non-archive fixture files found");

    let mut cases = Vec::new();
    for codec in [Codec::Deflate, Codec::Lzma, Codec::Bzip2, Codec::Zstd] {
        cases.push(CreateCase { label: "zip from files".to_string(), codec: Some(codec), inputs: controls.clone(), target: ArchiveTarget::Zip });
    }
    for codec in [Codec::Deflate, Codec::Lzma, Codec::Bzip2, Codec::Zstd, Codec::Lzma2] {
        cases.push(CreateCase { label: "7z from files".to_string(), codec: Some(codec), inputs: controls.clone(), target: ArchiveTarget::SevenZip });
    }

    run_create_cases("archive_files_roundtrip", cases);
}

/// Requests the module must reject before touching any native library, so no samples are
/// needed: tiny placeholder files are enough.
#[test]
fn archive_rejects_invalid_requests() {
    let dir = output_root().join("archive_rejects");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let make = |name: &str| {
        let path = dir.join(name);
        std::fs::write(&path, b"x").unwrap();
        path
    };
    let (text, cue, iso, bin, tiny) = (make("a.txt"), make("b.cue"), make("c.iso"), make("d.bin"), make("e.iso"));
    let out = || ArchiveDestination::Temp(dir.join("out"));
    let default = ArchiveOptions::default();
    let with_codec = |codec| ArchiveOptions { codec: Some(codec), ..Default::default() };

    let run = |inputs: Vec<PathBuf>, target, options: &ArchiveOptions| archive::archive(&inputs, target, options, out(), |_, _| true);

    assert!(matches!(run(vec![text.clone()], ArchiveTarget::Chd, &default), Err(ArchiveError::UnsupportedInput(_))));
    assert!(matches!(run(vec![text.clone()], ArchiveTarget::Rvz, &default), Err(ArchiveError::UnsupportedInput(_))));
    assert!(matches!(run(vec![cue.clone()], ArchiveTarget::Wia, &default), Err(ArchiveError::UnsupportedInput(_))));
    assert!(matches!(run(vec![bin.clone()], ArchiveTarget::Gcz, &default), Err(ArchiveError::UnsupportedInput(_))));
    assert!(matches!(run(vec![iso.clone(), tiny.clone()], ArchiveTarget::Chd, &default), Err(ArchiveError::UnsupportedInput(_))));
    assert!(matches!(run(Vec::new(), ArchiveTarget::Zip, &default), Err(ArchiveError::UnsupportedInput(_))));
    let media = ArchiveOptions { chd_media: Some(ChdMedia::Dvd), ..Default::default() };
    assert!(matches!(run(vec![cue.clone()], ArchiveTarget::Chd, &media), Err(ArchiveError::UnsupportedInput(_))));

    assert!(matches!(run(vec![cue.clone()], ArchiveTarget::Chd, &with_codec(Codec::Huff)), Err(ArchiveError::UnsupportedCodec(_))));
    assert!(matches!(run(vec![iso.clone()], ArchiveTarget::Chd, &with_codec(Codec::Deflate)), Err(ArchiveError::UnsupportedCodec(_))));
    assert!(matches!(run(vec![iso.clone()], ArchiveTarget::Gcz, &with_codec(Codec::Zstd)), Err(ArchiveError::UnsupportedCodec(_))));
    assert!(matches!(run(vec![iso.clone()], ArchiveTarget::Wia, &with_codec(Codec::Zstd)), Err(ArchiveError::UnsupportedCodec(_))));
    assert!(matches!(run(vec![iso.clone()], ArchiveTarget::Rvz, &with_codec(Codec::Deflate)), Err(ArchiveError::UnsupportedCodec(_))));
    assert!(matches!(run(vec![text.clone()], ArchiveTarget::Zip, &with_codec(Codec::Lzma2)), Err(ArchiveError::UnsupportedCodec(_))));
    assert!(matches!(run(vec![text.clone()], ArchiveTarget::SevenZip, &with_codec(Codec::Flac)), Err(ArchiveError::UnsupportedCodec(_))));

    let _ = std::fs::remove_dir_all(&dir);
}

/// Proves extract::extract's progress callback actually receives real values from the
/// native library (not just that the parameter plumbs through and compiles), and that
/// returning `false` genuinely cancels a zip/7z extraction.
#[test]
fn extract_reports_progress_and_can_be_cancelled() {
    let samples_root = samples_root();
    let Some(fixtures) = support::archive_cases::file_archive_fixtures(&samples_root) else {
        eprintln!("no file fixtures found -- skipping");
        return;
    };
    let Some(archive_path) = fixtures.archives.first() else {
        eprintln!("no archive fixture found -- skipping");
        return;
    };
    let format = if archive_path.extension().and_then(|e| e.to_str()) == Some("zip") { ArchiveFormat::Zip } else { ArchiveFormat::SevenZip };
    let out = output_root().join("extract_progress_smoke");

    // Normal run: collect every (message, percent) call.
    let _ = std::fs::remove_dir_all(&out);
    let mut calls: Vec<(String, f32)> = Vec::new();
    let result = extract::extract(archive_path, format, ExtractDestination::Temp(out.clone()), None, extract::CdOutputFormat::default(), |msg, pct| {
        calls.push((msg.to_string(), pct));
        true
    });
    result.expect("extraction should succeed");
    let _ = std::fs::remove_dir_all(&out);

    assert!(!calls.is_empty(), "on_progress was never called");
    assert!(calls.iter().any(|(_, pct)| *pct >= 0.0), "every call reported -1.0 (no real progress reached us): {calls:?}");
    let last_real = calls.iter().map(|(_, pct)| *pct).filter(|pct| *pct >= 0.0).fold(f32::MIN, f32::max);
    assert!(last_real > 90.0, "progress never got close to done, last real value was {last_real}: {calls:?}");

    // Cancel on the very first call: extraction must stop and report Cancelled.
    let _ = std::fs::remove_dir_all(&out);
    let mut first_call = true;
    let result = extract::extract(archive_path, format, ExtractDestination::Temp(out.clone()), None, extract::CdOutputFormat::default(), |_, _| {
        if first_call {
            first_call = false;
            false
        } else {
            true
        }
    });
    assert!(matches!(result, Err(ExtractError::Cancelled)), "expected Cancelled, got {result:?}");
    let _ = std::fs::remove_dir_all(&out);
}

/// Proves archive::archive's progress callback receives real values from the native
/// library, and that returning `false` genuinely cancels a zip/7z creation.
#[test]
fn archive_reports_progress_and_can_be_cancelled() {
    let samples_root = samples_root();
    let Some(fixtures) = support::archive_cases::file_archive_fixtures(&samples_root) else {
        eprintln!("no file fixtures found -- skipping");
        return;
    };
    if fixtures.controls.is_empty() {
        eprintln!("no control files found -- skipping");
        return;
    }
    let out = output_root().join("archive_progress_smoke");
    let options = ArchiveOptions::default();

    let _ = std::fs::remove_dir_all(&out);
    let mut calls: Vec<(String, f32)> = Vec::new();
    let result = archive::archive(&fixtures.controls, ArchiveTarget::Zip, &options, ArchiveDestination::Temp(out.clone()), |msg, pct| {
        calls.push((msg.to_string(), pct));
        true
    });
    result.expect("archive creation should succeed");
    let _ = std::fs::remove_dir_all(&out);

    assert!(!calls.is_empty(), "on_progress was never called");
    let last_real = calls.iter().map(|(_, pct)| *pct).filter(|pct| *pct >= 0.0).fold(f32::MIN, f32::max);
    assert!(last_real > 90.0, "progress never got close to done, last real value was {last_real}: {calls:?}");

    let _ = std::fs::remove_dir_all(&out);
    let mut first_call = true;
    let result = archive::archive(&fixtures.controls, ArchiveTarget::Zip, &options, ArchiveDestination::Temp(out.clone()), |_, _| {
        if first_call {
            first_call = false;
            false
        } else {
            true
        }
    });
    assert!(matches!(result, Err(ArchiveError::Cancelled)), "expected Cancelled, got {result:?}");
    let _ = std::fs::remove_dir_all(&out);
}

/// The streaming flag is detectable: a default build never reports streaming, and a
/// streaming build (when present) reports it for 7z -- the only library with a streaming
/// build so far. Symbol presence decides, not directory, so this also catches a
/// non-streaming library copied into `with_streaming/` by mistake.
#[test]
fn native_capabilities_detect_streaming() {
    use romexplorer::{detect_capabilities, LibVariant};

    let standard = detect_capabilities(LibVariant::Standard);
    let streaming = detect_capabilities(LibVariant::Streaming);
    println!("{standard:#?}\n{streaming:#?}");

    // Only a library resolved from the submodule's plain release tree is known to be the
    // default build. The deployed app_root_ex copy wins for both variants, and it is
    // whichever one move_libs_to_app_root.sh deployed -- the streaming one by default -- so
    // when both variants resolve to the same file there is nothing variant-specific to check.
    for (lib, other) in [
        (&standard.sevenzip, &streaming.sevenzip),
        (&standard.chdman, &streaming.chdman),
        (&standard.dolphin, &streaming.dolphin),
    ] {
        if lib.path.is_some() && lib.path == other.path {
            continue;
        }
        assert!(!lib.streaming, "default build unexpectedly streams: {lib:?}");
    }
    for lib in [&streaming.sevenzip, &streaming.chdman, &streaming.dolphin] {
        assert!(lib.path.is_some() || !lib.streaming);
    }
    if streaming.sevenzip.path.is_some() {
        assert!(streaming.sevenzip.streaming, "with_streaming 7z lacks sevenzip_extract_entry_stream");
    }
    if streaming.chdman.path.is_some() {
        assert!(streaming.chdman.streaming, "with_streaming chdman lacks chdman_extract_stream");
    }
    if streaming.dolphin.path.is_some() {
        assert!(streaming.dolphin.streaming, "with_streaming dolphinrvz lacks dolphinrvz_extract_stream");
    }
}

/// End-to-end 7z/zip streaming through the Rust wrapper, against the streaming build.
///
/// Runs on a zip and a 7z created here from the fixture files, and on every existing
/// `.zip`/`.7z` fixture archive (drop a solid 7z in there to cover solid skip-through --
/// `create_archive` always writes non-solid 7z). For every entry, streamed with a file
/// sink in the same pass: every chunk carries the requested entry index, offsets are
/// contiguous from 0, the size matches, the file written in that pass equals the streamed
/// bytes, a standard-build `extract_to_file` agrees and, for created archives, the bytes
/// equal the original input. Then stream-only (`out_path = None`) on the last entry, and
/// stopping from `on_data`. Skips if no streaming build exists.
#[test]
fn sevenzip_stream_extract() {
    use romexplorer::rom::native_lib::LibVariant;
    use romexplorer::rom::sevenzip_wrapper::{
        create_archive, find_library, ArchiveFormat, CompressionMethod, CreateArchiveArgs, CreateEntry, SevenZipArchive,
        SevenZipError,
    };

    if find_library(LibVariant::Streaming).is_none() {
        eprintln!("no streaming 7z build -- skipping (run deps/7-Zip-zstd/scripts/build.sh, then scripts/move_libs_to_app_root.sh)");
        return;
    }
    let samples_root = samples_root();
    let Some(fixtures) = support::archive_cases::file_archive_fixtures(&samples_root) else {
        eprintln!("no file fixtures found -- skipping");
        return;
    };
    let dir = output_root().join("sevenzip_stream_extract");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let inputs: Vec<PathBuf> = fixtures.controls.iter().chain(fixtures.archives.iter()).cloned().collect();
    let entries: Vec<CreateEntry> = inputs
        .iter()
        .map(|p| CreateEntry { input_path: p.clone(), archive_name: p.file_name().unwrap().to_string_lossy().into_owned() })
        .collect();

    // (archive, format, originals to compare against -- None for fixture archives)
    let mut cases: Vec<(PathBuf, ArchiveFormat, Option<&[PathBuf]>)> = Vec::new();
    for (format, method, name) in [
        (ArchiveFormat::SevenZ, CompressionMethod::Lzma2, "created.7z"),
        (ArchiveFormat::Zip, CompressionMethod::Deflate, "created.zip"),
    ] {
        let archive_path = dir.join(name);
        let args = CreateArchiveArgs { format, method, level: 5, entries: entries.clone() };
        create_archive(&archive_path, &args, |_, _| true).unwrap();
        cases.push((archive_path, format, Some(&inputs)));
    }
    for archive in &fixtures.archives {
        let is_7z = archive.extension().is_some_and(|e| e.eq_ignore_ascii_case("7z"));
        cases.push((archive.clone(), if is_7z { ArchiveFormat::SevenZ } else { ArchiveFormat::Zip }, None));
    }

    for (case_no, (archive_path, format, originals)) in cases.into_iter().enumerate() {
        let name = archive_path.file_name().unwrap().to_string_lossy().into_owned();
        let streaming = SevenZipArchive::open_variant(&archive_path, format, LibVariant::Streaming, |_, _| true).unwrap();
        let standard = SevenZipArchive::open(&archive_path, format, |_, _| true).unwrap();
        assert!(streaming.supports_streaming(), "{name}: streaming handle can't stream");
        // Only meaningful when the two variants are different files: a deployed app_root_ex
        // library serves both, and is the streaming build unless deployed with --nostreaming.
        if find_library(LibVariant::Standard) != find_library(LibVariant::Streaming) {
            assert!(!standard.supports_streaming(), "{name}: standard build claims to stream");
        }

        let count = streaming.entry_count();
        if let Some(originals) = originals {
            assert_eq!(count as usize, originals.len(), "{name}: entry count");
        }
        let (mut last_file, mut first_file) = (None, None);

        for index in 0..count {
            let entry = streaming.entry(index).unwrap();
            if entry.is_dir {
                continue;
            }
            first_file.get_or_insert(index);
            last_file = Some((index, entry.size));

            let (mut streamed, mut next_offset, mut bad_index, mut gaps) = (Vec::new(), 0u64, 0, 0);
            let out_file = dir.join(format!("{case_no}.entry_{index}.out"));
            streaming
                .extract_stream(
                    index,
                    Some(&out_file),
                    |entry_index, offset, chunk| {
                        bad_index += (entry_index != index as u32) as usize;
                        gaps += (offset != next_offset) as usize;
                        next_offset = offset + chunk.len() as u64;
                        streamed.extend_from_slice(chunk);
                        true
                    },
                    |_, _| true,
                )
                .unwrap();

            assert_eq!(bad_index, 0, "{name}[{index}]: chunks tagged with another entry index");
            assert_eq!(gaps, 0, "{name}[{index}]: non-contiguous offsets");
            assert_eq!(streamed.len() as u64, entry.size, "{name}[{index}]: streamed size");
            assert!(std::fs::read(&out_file).unwrap() == streamed, "{name}[{index}]: written file differs from stream");

            let reference = dir.join(format!("{case_no}.entry_{index}.ref"));
            standard.extract_to_file(index, &reference, |_, _| true).unwrap();
            assert!(std::fs::read(&reference).unwrap() == streamed, "{name}[{index}]: standard extract disagrees");

            if let Some(originals) = originals {
                let original = originals.iter().find(|p| p.file_name().unwrap().to_string_lossy() == entry.path).unwrap();
                assert!(std::fs::read(original).unwrap() == streamed, "{name}[{index}]: streamed bytes differ from original");
            }
            let _ = std::fs::remove_file(&out_file);
            let _ = std::fs::remove_file(&reference);
        }

        // Stream only, last entry: in a solid 7z this decodes through every earlier entry.
        let (last, last_size) = last_file.expect("archive has no file entries");
        let mut total = 0u64;
        streaming
            .extract_stream(
                last,
                None,
                |entry_index, _, chunk| {
                    assert_eq!(entry_index, last as u32);
                    total += chunk.len() as u64;
                    true
                },
                |_, _| true,
            )
            .unwrap();
        assert_eq!(total, last_size, "{name}: stream-only size");

        // Stop from on_data.
        let result = streaming.extract_stream(first_file.unwrap(), None, |_, _, _| false, |_, _| true);
        assert_eq!(result, Err(SevenZipError::Cancelled), "{name}: stop from on_data");
        println!("{name}: {count} entries streamed and verified");
    }

    if std::env::var_os("ROMEXPLORER_KEEP_OUTPUT").is_none() {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

fn crc32_file(path: &Path) -> (u32, u64) {
    let data = std::fs::read(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    (crc32fast::hash(&data), data.len() as u64)
}

/// A CHD sample classified by its contents (probed with a streaming-build `ChdReader`),
/// so tests pick inputs by what they are, not by where they live.
struct ChdSample {
    path: PathBuf,
    size: u64,
    info: romexplorer::rom::chdman_wrapper::ChdInfo,
    /// Track 1's type, for CD/GD-ROM.
    first_track_type: Option<u32>,
}

/// Every `.chd` under the samples root (recursively), smallest first.
fn probe_chd_samples() -> Vec<ChdSample> {
    use romexplorer::rom::chdman_wrapper::ChdReader;
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("chd")) {
                out.push(path);
            }
        }
    }
    let mut paths = Vec::new();
    walk(&samples_root(), &mut paths);
    let mut samples: Vec<ChdSample> = paths
        .into_iter()
        .filter_map(|path| {
            let reader = ChdReader::open(&path, None, 0).ok()?;
            let info = reader.info().ok()?;
            let first_track_type = if info.is_cd { reader.track(0).ok().map(|t| t.track_type) } else { None };
            let size = std::fs::metadata(&path).ok()?.len();
            Some(ChdSample { path, size, info, first_track_type })
        })
        .collect();
    samples.sort_by_key(|s| s.size);
    samples
}

/// End-to-end chdman streaming through the Rust wrapper (`chdman_wrapper::run_stream`),
/// against the streaming build. For each sample, the reference is a plain `run()` extract by
/// the default build. Checks, per output file: streamed bytes == reference (CRC + size),
/// contiguous offsets from 0, the file written in the same pass == reference; the cue sheet
/// arrives last for extractcd; stream-only leaves nothing on disk; stopping from `on_data`
/// yields `Cancelled` and leaves nothing on disk. One case uses split bins (one file per
/// track) so per-file tagging is exercised across many files. Skips without a streaming build.
#[test]
fn chdman_stream_extract() {
    use romexplorer::rom::chdman_wrapper::{self, ChdmanArgs, ChdmanCommand, ChdmanError};
    use romexplorer::rom::native_lib::LibVariant;

    if chdman_wrapper::find_library(LibVariant::Streaming).is_none() {
        eprintln!("no streaming chdman build -- skipping (run build_chdman_lib.sh, then scripts/move_libs_to_app_root.sh)");
        return;
    }
    // Smallest sample of each kind: a single-track CD, a multi-track CD extracted as split
    // bins (one output file per track), and a DVD.
    let samples = probe_chd_samples();
    let cd = |multi: bool| samples.iter().find(|s| s.info.is_cd && !s.info.is_gdrom && (s.info.num_tracks > 1) == multi);
    let cases = [
        ("cd, 1 track", cd(false), ChdmanCommand::ExtractCd, "cue", false),
        ("cd, split bins", cd(true), ChdmanCommand::ExtractCd, "cue", true),
        ("dvd", samples.iter().find(|s| !s.info.is_cd), ChdmanCommand::ExtractDvd, "iso", false),
    ];
    let root = output_root().join("chdman_stream_extract");
    let _ = std::fs::remove_dir_all(&root);

    // (name, crc hasher, next expected offset) per file_index
    type Files = Vec<(String, crc32fast::Hasher, u64)>;
    fn collector<'a>(files: &'a mut Files, gaps: &'a mut usize) -> impl FnMut(u32, &str, u64, &[u8]) -> bool + 'a {
        move |index, name, offset, chunk| {
            if index as usize == files.len() {
                files.push((name.to_string(), crc32fast::Hasher::new(), 0));
            }
            let entry = &mut files[index as usize];
            assert_eq!(entry.0, name, "file_index {index} changed name");
            *gaps += (offset != entry.2) as usize;
            entry.2 = offset + chunk.len() as u64;
            entry.1.update(chunk);
            true
        }
    }

    for (case_no, (sample, found, command, ext, split)) in cases.into_iter().enumerate() {
        let Some(found) = found else {
            eprintln!("{sample}: no such sample -- skipping");
            continue;
        };
        let input = found.path.clone();
        let dir = |name: &str| {
            let d = root.join(format!("case{case_no}")).join(name);
            std::fs::create_dir_all(&d).unwrap();
            d
        };
        let (ref_dir, s_dir, n_dir, x_dir) = (dir("ref"), dir("stream"), dir("nowrite"), dir("stop"));
        let args = |out_dir: &Path| ChdmanArgs {
            input: Some(input.clone()),
            output: Some(out_dir.join(format!("disc.{ext}"))),
            output_split_bin: split,
            output_force: true,
            ..Default::default()
        };

        chdman_wrapper::run(command, &args(&ref_dir), |_, _| {}).unwrap();
        let ref_count = std::fs::read_dir(&ref_dir).unwrap().count();

        // 1. stream + write
        let (mut files, mut gaps): (Files, usize) = (Vec::new(), 0);
        chdman_wrapper::run_stream(command, &args(&s_dir), true, collector(&mut files, &mut gaps), |_, _| {}).unwrap();
        assert_eq!(gaps, 0, "{sample}: non-contiguous offsets");
        assert_eq!(files.len(), ref_count, "{sample}: streamed file count vs reference");
        if command == ChdmanCommand::ExtractCd {
            assert!(files.last().unwrap().0.ends_with(".cue"), "{sample}: cue sheet not delivered last");
        }
        let streamed: Vec<(String, u32, u64)> = files
            .into_iter()
            .map(|(name, hasher, size)| (name, hasher.finalize(), size))
            .collect();
        for (name, crc, size) in &streamed {
            let base = Path::new(name).file_name().unwrap();
            let (ref_crc, ref_size) = crc32_file(&ref_dir.join(base));
            assert_eq!((*crc, *size), (ref_crc, ref_size), "{sample}: {base:?} streamed != reference");
            assert_eq!(crc32_file(Path::new(name)), (ref_crc, ref_size), "{sample}: {base:?} written != reference");
        }

        // 2. stream only
        let (mut files, mut gaps): (Files, usize) = (Vec::new(), 0);
        chdman_wrapper::run_stream(command, &args(&n_dir), false, collector(&mut files, &mut gaps), |_, _| {}).unwrap();
        let only: Vec<(u32, u64)> = files.into_iter().map(|(_, h, size)| (h.finalize(), size)).collect();
        let with_write: Vec<(u32, u64)> = streamed.iter().map(|(_, crc, size)| (*crc, *size)).collect();
        assert_eq!(only, with_write, "{sample}: stream-only bytes differ from stream+write");
        assert_eq!(std::fs::read_dir(&n_dir).unwrap().count(), 0, "{sample}: stream-only left files behind");

        // 3. stop
        let result = chdman_wrapper::run_stream(command, &args(&x_dir), true, |_, _, _, _| false, |_, _| {});
        assert!(matches!(result, Err(ChdmanError::Cancelled { .. })), "{sample}: stop -> {result:?}");
        assert_eq!(std::fs::read_dir(&x_dir).unwrap().count(), 0, "{sample}: stopped extraction left files behind");

        println!("{sample}: {} files streamed and verified", streamed.len());
    }

    if std::env::var_os("ROMEXPLORER_KEEP_OUTPUT").is_none() {
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// `ChdReader` random access from many threads at once, one reader each, against a plain
/// extract: random `read_bytes` spans of a DVD CHD vs the extracted ISO, and random physical
/// sector reads of a CD CHD vs its track-1 bin. Thread count is
/// `recommended_reader_threads()` (this machine's logical CPUs, no fixed cap).
#[test]
fn chdman_reader_parallel_random_access() {
    use romexplorer::rom::chdman_wrapper::{self, ChdReader, ChdmanArgs, ChdmanCommand, CD_TRACK_RAW_DONTCARE};
    use romexplorer::rom::native_lib::LibVariant;

    if chdman_wrapper::find_library(LibVariant::Streaming).is_none() {
        eprintln!("no streaming chdman build -- skipping");
        return;
    }
    let samples = probe_chd_samples();
    let root = output_root().join("chdman_reader_parallel");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let threads = chdman_wrapper::recommended_reader_threads();

    // Smallest DVD, and smallest (non-GD-ROM) CD whose track 1 is data: the cue output
    // byte-swaps audio, so an audio track 1 can't be compared sector-for-sector.
    let dvd = samples.iter().find(|s| !s.info.is_cd);
    let cd = samples.iter().find(|s| {
        s.info.is_cd && !s.info.is_gdrom && s.first_track_type.is_some_and(|t| t != chdman_wrapper::CD_TRACK_AUDIO)
    });
    for (case_no, (sample, found, command, ext)) in
        [("dvd", dvd, ChdmanCommand::ExtractDvd, "iso"), ("cd", cd, ChdmanCommand::ExtractCd, "cue")].into_iter().enumerate()
    {
        let Some(found) = found else {
            eprintln!("{sample}: no such sample -- skipping");
            continue;
        };
        let input = found.path.clone();
        let out = root.join(format!("case{case_no}.{ext}"));
        let args = ChdmanArgs { input: Some(input.clone()), output: Some(out.clone()), output_force: true, ..Default::default() };
        chdman_wrapper::run(command, &args, |_, _| {}).unwrap();
        let reference = if command == ChdmanCommand::ExtractCd { out.with_extension("bin") } else { out.clone() };
        let ref_len = std::fs::metadata(&reference).unwrap().len();

        let info = ChdReader::open(&input, None, 0).unwrap().info().unwrap();
        assert_eq!(info.is_cd, command == ChdmanCommand::ExtractCd, "{sample}: is_cd");

        let bad: usize = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..threads)
                .map(|t| {
                    let (input, reference) = (&input, &reference);
                    scope.spawn(move || {
                        let mut reader = ChdReader::open(input, None, 4).unwrap();
                        let file = std::fs::File::open(reference).unwrap();
                        let track = if info.is_cd { Some(reader.track(0).unwrap()) } else { None };
                        let mut state = 0x9E37_79B9_7F4A_7C15u64 ^ (t as u64 + 1);
                        let mut next = move || {
                            state ^= state << 13;
                            state ^= state >> 7;
                            state ^= state << 17;
                            state
                        };
                        let (mut got, mut want, mut bad) = (vec![0u8; 1 << 18], vec![0u8; 1 << 18], 0usize);
                        for _ in 0..300 {
                            let (offset, len) = if let Some(track) = track {
                                let sectors = (ref_len / track.data_size as u64).min(track.frames as u64);
                                let sector = next() % sectors;
                                reader
                                    .read_sector(track.physical_start + sector as u32, &mut got, CD_TRACK_RAW_DONTCARE, true)
                                    .unwrap();
                                (sector * track.data_size as u64, track.data_size as usize)
                            } else {
                                let offset = next() % ref_len;
                                let len = (1 + next() % (got.len() as u64 - 1)).min(ref_len - offset) as usize;
                                reader.read_bytes(offset, &mut got[..len]).unwrap();
                                (offset, len)
                            };
                            read_exact_at(&file, &mut want[..len], offset).unwrap();
                            bad += (got[..len] != want[..len]) as usize;
                        }
                        bad
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).sum()
        });
        assert_eq!(bad, 0, "{sample}: {bad} random reads differed from the extracted file");
        println!("{sample}: {threads} threads x 300 random reads verified ({} hunks of {} bytes)", info.hunk_count, info.hunk_bytes);
    }

    if std::env::var_os("ROMEXPLORER_KEEP_OUTPUT").is_none() {
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// The smallest disc image of each container format (RVZ, WIA, GCZ) under the samples root
/// that the streaming dolphinrvz reader opens: (format extension, path, plain size).
fn probe_dolphin_samples() -> Vec<(&'static str, PathBuf, u64)> {
    use romexplorer::rom::dolphin_wrapper::DiscReader;
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| ["rvz", "wia", "gcz"].contains(&e.to_ascii_lowercase().as_str()))
            {
                out.push(path);
            }
        }
    }
    let mut paths = Vec::new();
    walk(&samples_root(), &mut paths);
    paths.sort_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(u64::MAX));
    ["rvz", "wia", "gcz"]
        .into_iter()
        .filter_map(|ext| {
            paths.iter().filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext))).find_map(|path| {
                let info = DiscReader::open(path, None).ok()?.info().ok()?;
                info.data_size_accurate.then(|| (ext, path.clone(), info.data_size))
            })
        })
        .collect()
}

/// End-to-end dolphinrvz streaming through the Rust wrapper, against the streaming build,
/// with a plain `convert` to ISO by the default build as the reference:
/// `extract_stream` (streamed == written == reference, contiguous, file_index 0, file_name =
/// output path; stream-only leaves no file; stop -> `Cancelled`, no file), and
/// `convert_stream` ISO -> RVZ (the input stream == the reference ISO, every byte once in
/// order; the RVZ written == a plain `convert` with the same settings; stop -> `Cancelled`).
#[test]
fn dolphin_stream_extract() {
    use romexplorer::rom::dolphin_wrapper::{self, DiscCompression, DiscFormat, DolphinConvertArgs, DolphinRvzError};
    use romexplorer::rom::native_lib::LibVariant;

    if dolphin_wrapper::find_library(LibVariant::Streaming).is_none() {
        eprintln!("no streaming dolphinrvz build -- skipping (run build_dolphin_rvz.sh, then scripts/move_libs_to_app_root.sh)");
        return;
    }
    let samples = probe_dolphin_samples();
    if samples.is_empty() {
        eprintln!("no RVZ/WIA/GCZ sample -- skipping");
        return;
    }
    let root = output_root().join("dolphin_stream_extract");
    let _ = std::fs::remove_dir_all(&root);
    for (format, input, plain_size) in samples {
    let dir = root.join(format);
    std::fs::create_dir_all(&dir).unwrap();
    let user_dir = dir.join("dolphin_user");
    let (reference, streamed_iso) = (dir.join("ref.iso"), dir.join("stream.iso"));

    dolphin_wrapper::convert(
        &DolphinConvertArgs::new(&input, &reference, DiscFormat::Iso).with_user_dir(&user_dir),
        |_, _| true,
    )
    .unwrap();
    let (ref_crc, ref_size) = crc32_file(&reference);
    assert_eq!(ref_size, plain_size, "reader data_size vs converted ISO size");

    // 1. extract_stream + write
    let (mut hasher, mut next, mut bad) = (crc32fast::Hasher::new(), 0u64, 0usize);
    let want = streamed_iso.to_str().unwrap().to_string();
    dolphin_wrapper::extract_stream(
        &input,
        Some(&streamed_iso),
        Some(&user_dir),
        |index, name, offset, chunk| {
            bad += (index != 0 || name != want || offset != next) as usize;
            next = offset + chunk.len() as u64;
            hasher.update(chunk);
            true
        },
        |_, _| true,
    )
    .unwrap();
    assert_eq!(bad, 0, "file_index/file_name/offset contiguity");
    assert_eq!((hasher.finalize(), next), (ref_crc, ref_size), "streamed != reference");
    assert_eq!(crc32_file(&streamed_iso), (ref_crc, ref_size), "written != reference");
    std::fs::remove_file(&streamed_iso).unwrap();

    // 2. stream only
    let mut hasher = crc32fast::Hasher::new();
    dolphin_wrapper::extract_stream(&input, None, Some(&user_dir), |_, _, _, chunk| {
        hasher.update(chunk);
        true
    }, |_, _| true)
    .unwrap();
    assert_eq!(hasher.finalize(), ref_crc, "stream-only != reference");
    assert!(!streamed_iso.exists(), "stream-only wrote a file");

    // 3. stop
    let result = dolphin_wrapper::extract_stream(&input, Some(&streamed_iso), Some(&user_dir), |_, _, _, _| false, |_, _| true);
    assert_eq!(result, Err(DolphinRvzError::Cancelled));
    assert!(!streamed_iso.exists(), "stopped extraction left a file");

    // 4. convert_stream ISO -> RVZ, observing the input
    let rvz = |name: &str| {
        DolphinConvertArgs::new(&reference, dir.join(name), DiscFormat::Rvz)
            .with_user_dir(&user_dir)
            .with_block_size(131072)
            .with_compression(DiscCompression::Zstd, 5)
    };
    let (mut hasher, mut next, mut bad) = (crc32fast::Hasher::new(), 0u64, 0usize);
    let want = reference.to_str().unwrap().to_string();
    dolphin_wrapper::convert_stream(
        &rvz("stream.rvz"),
        |index, name, offset, chunk| {
            bad += (index != 0 || name != want || offset != next) as usize;
            next = offset + chunk.len() as u64;
            hasher.update(chunk);
            true
        },
        |_, _| true,
    )
    .unwrap();
    assert_eq!(bad, 0, "input stream: file_index/file_name/offset contiguity");
    assert_eq!((hasher.finalize(), next), (ref_crc, ref_size), "input stream != reference ISO");
    dolphin_wrapper::convert(&rvz("plain.rvz"), |_, _| true).unwrap();
    assert_eq!(crc32_file(&dir.join("stream.rvz")), crc32_file(&dir.join("plain.rvz")), "RVZ differs from a plain convert");

    // 5. convert_stream stop
    let result = dolphin_wrapper::convert_stream(&rvz("stop.rvz"), |_, _, _, _| false, |_, _| true);
    assert_eq!(result, Err(DolphinRvzError::Cancelled));

    println!("{format}: {ref_size} plain bytes streamed and verified (extract + convert input)");
    if std::env::var_os("ROMEXPLORER_KEEP_OUTPUT").is_none() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    }
}

/// `DiscReader` random access from `recommended_reader_threads()` threads at once, one
/// reader each, against a plain ISO conversion of the same sample.
#[test]
fn dolphin_reader_parallel_random_access() {
    use romexplorer::rom::dolphin_wrapper::{self, DiscFormat, DiscReader, DolphinConvertArgs};
    use romexplorer::rom::native_lib::{self, LibVariant};

    if dolphin_wrapper::find_library(LibVariant::Streaming).is_none() {
        eprintln!("no streaming dolphinrvz build -- skipping");
        return;
    }
    let samples = probe_dolphin_samples();
    if samples.is_empty() {
        eprintln!("no RVZ/WIA/GCZ sample -- skipping");
        return;
    }
    let root = output_root().join("dolphin_reader_parallel");
    let _ = std::fs::remove_dir_all(&root);
    for (format, input, plain_size) in samples {
    let dir = root.join(format);
    std::fs::create_dir_all(&dir).unwrap();
    let user_dir = dir.join("dolphin_user");
    let reference = dir.join("ref.iso");
    dolphin_wrapper::convert(
        &DolphinConvertArgs::new(&input, &reference, DiscFormat::Iso).with_user_dir(&user_dir),
        |_, _| true,
    )
    .unwrap();

    let threads = native_lib::recommended_reader_threads();
    let bad: usize = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let (input, reference, user_dir) = (&input, &reference, &user_dir);
                scope.spawn(move || {
                    let mut reader = DiscReader::open(input, Some(user_dir)).unwrap();
                    let file = std::fs::File::open(reference).unwrap();
                    let mut state = 0x2545_F491_4F6C_DD1Du64 ^ (t as u64 + 1);
                    let mut next = move || {
                        state ^= state << 13;
                        state ^= state >> 7;
                        state ^= state << 17;
                        state
                    };
                    let (mut got, mut want, mut bad) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20], 0usize);
                    for _ in 0..200 {
                        let offset = next() % plain_size;
                        let len = (1 + next() % (got.len() as u64 - 1)).min(plain_size - offset) as usize;
                        reader.read(offset, &mut got[..len]).unwrap();
                        read_exact_at(&file, &mut want[..len], offset).unwrap();
                        bad += (got[..len] != want[..len]) as usize;
                    }
                    bad
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).sum()
    });
    assert_eq!(bad, 0, "{format}: {bad} random reads differed from the converted ISO");
    println!("{format}: {threads} threads x 200 random reads verified");
    if std::env::var_os("ROMEXPLORER_KEEP_OUTPUT").is_none() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    }
}

/// CRC32 + size of a byte stream read to the end.
fn crc32_read(mut r: impl std::io::Read) -> (u32, u64) {
    let (mut hasher, mut total, mut buf) = (crc32fast::Hasher::new(), 0u64, vec![0u8; 1 << 20]);
    loop {
        let n = r.read(&mut buf).unwrap();
        if n == 0 {
            return (hasher.finalize(), total);
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
}

fn report_crc(report: &romexplorer::rom::hash::HashReport) -> u32 {
    u32::from_str_radix(report.crc32.as_deref().expect("crc32 in report"), 16).unwrap()
}

/// zip/7z: `EntryReader` sequential and (emulated) random access against the original files,
/// then re-archiving zip -> 7z straight from `EntryReader` sources (sequential, hashed on the
/// way) and files -> zip from random-access file sources.
#[test]
fn sevenzip_entry_reader_and_sources() {
    use romexplorer::rom::hash::HashFlags;
    use romexplorer::rom::native_lib::LibVariant;
    use romexplorer::rom::sevenzip_wrapper::{
        create_archive, create_archive_from_sources, find_library, ArchiveFormat, CompressionMethod, CreateArchiveArgs,
        CreateEntry, EntryReader, SevenZipArchive,
    };
    use romexplorer::rom::source::{ReadAt, Source};

    if find_library(LibVariant::Streaming).is_none() {
        eprintln!("no streaming 7z build -- skipping");
        return;
    }
    let Some(fixtures) = support::archive_cases::file_archive_fixtures(&samples_root()) else {
        eprintln!("no file fixtures found -- skipping");
        return;
    };
    let dir = output_root().join("sevenzip_entry_reader_and_sources");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let inputs: Vec<PathBuf> = fixtures.controls.clone();
    let entries: Vec<CreateEntry> = inputs
        .iter()
        .map(|p| CreateEntry { input_path: p.clone(), archive_name: p.file_name().unwrap().to_string_lossy().into_owned() })
        .collect();
    let zip = dir.join("source.zip");
    create_archive(&zip, &CreateArchiveArgs { format: ArchiveFormat::Zip, method: CompressionMethod::Deflate, level: 5, entries }, |_, _| true)
        .unwrap();

    // 1. EntryReader: sequential, then random (forward, backward, repeated) vs the originals.
    let archive = SevenZipArchive::open(&zip, ArchiveFormat::Zip, |_, _| true).unwrap();
    for index in 0..archive.entry_count() {
        let entry = archive.entry(index).unwrap();
        let original = std::fs::read(inputs.iter().find(|p| p.file_name().unwrap().to_string_lossy() == entry.path).unwrap()).unwrap();
        let mut reader = EntryReader::open(&zip, ArchiveFormat::Zip, index).unwrap();
        assert_eq!(reader.size(), original.len() as u64);
        let mut all = Vec::new();
        std::io::Read::read_to_end(&mut reader, &mut all).unwrap();
        assert!(all == original, "{}: sequential read differs", entry.path);

        let mut state = 0x1234_5678u64 ^ index as u64;
        for _ in 0..20 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let offset = (state >> 11) % original.len() as u64;
            let len = ((state >> 40) % 70_000).min(original.len() as u64 - offset) as usize;
            let mut buf = vec![0u8; len];
            reader.read_at(offset, &mut buf).unwrap();
            assert!(buf[..] == original[offset as usize..offset as usize + len], "{}: read_at({offset}, {len}) differs", entry.path);
        }
    }

    // 2. zip -> 7z from sequential EntryReader sources, hashed while compressed.
    let mut hashes = Vec::new();
    let mut sources: Vec<Source> = (0..archive.entry_count())
        .map(|index| {
            let name = archive.entry(index).unwrap().path;
            let (source, hash) = EntryReader::open(&zip, ArchiveFormat::Zip, index).unwrap().into_source(&name).hashed(HashFlags::CRC32);
            hashes.push((name, hash));
            source
        })
        .collect();
    let seven = dir.join("rearchived.7z");
    create_archive_from_sources(&seven, ArchiveFormat::SevenZ, CompressionMethod::Zstd, 5, &mut sources, |_, _| true).unwrap();
    drop(sources);
    let rearchived = SevenZipArchive::open(&seven, ArchiveFormat::SevenZ, |_, _| true).unwrap();
    assert_eq!(rearchived.entry_count() as usize, hashes.len());
    for (name, hash) in hashes {
        let original = inputs.iter().find(|p| p.file_name().unwrap().to_string_lossy() == name).unwrap();
        let (crc, _) = crc32_file(original);
        assert_eq!(report_crc(&hash.finish().expect("complete hash")), crc, "{name}: hash taken while re-archiving");
        let index = (0..rearchived.entry_count()).find(|&i| rearchived.entry(i).unwrap().path == name).unwrap();
        let out = dir.join(format!("check_{index}"));
        rearchived.extract_to_file(index, &out, |_, _| true).unwrap();
        assert_eq!(crc32_file(&out).0, crc, "{name}: re-archived content differs");
    }

    // 3. files -> zip from random-access file sources.
    let mut sources: Vec<Source> =
        inputs.iter().map(|p| Source::file(p, p.file_name().unwrap().to_string_lossy()).unwrap()).collect();
    let zip2 = dir.join("from_files.zip");
    create_archive_from_sources(&zip2, ArchiveFormat::Zip, CompressionMethod::Deflate, 5, &mut sources, |_, _| true).unwrap();
    let from_files = SevenZipArchive::open(&zip2, ArchiveFormat::Zip, |_, _| true).unwrap();
    for index in 0..from_files.entry_count() {
        let entry = from_files.entry(index).unwrap();
        let original = inputs.iter().find(|p| p.file_name().unwrap().to_string_lossy() == entry.path).unwrap();
        let out = dir.join(format!("files_{index}"));
        from_files.extract_to_file(index, &out, |_, _| true).unwrap();
        assert_eq!(crc32_file(&out), crc32_file(original), "{}: zip from random sources differs", entry.path);
    }
    println!("7z: {} entries: reader seq+random ok, zip->7z from readers ok, files->zip from random sources ok", inputs.len());
    if std::env::var_os("ROMEXPLORER_KEEP_OUTPUT").is_none() {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// chdman creation from sources: DVD from a random source (a `ChdReader` over another CHD:
/// CHD -> CHD without extracting) and from a sequential source (a plain reader over an
/// ISO); CD from a cue sheet + track files given as sequential sources (split bins). Each
/// result is checked by content against the original, and the hash taken while
/// compressing against the original's.
#[test]
fn chdman_create_from_sources() {
    use romexplorer::rom::chdman_wrapper::{self, ChdReader, ChdmanArgs, ChdmanCommand};
    use romexplorer::rom::hash::HashFlags;
    use romexplorer::rom::native_lib::LibVariant;
    use romexplorer::rom::source::Source;

    if chdman_wrapper::find_library(LibVariant::Streaming).is_none() {
        eprintln!("no streaming chdman build -- skipping");
        return;
    }
    let samples = probe_chd_samples();
    let dir = output_root().join("chdman_create_from_sources");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let extract = |command, input: &Path, out: &Path, split: bool| {
        let args = ChdmanArgs { input: Some(input.to_path_buf()), output: Some(out.to_path_buf()), output_split_bin: split, output_force: true, ..Default::default() };
        chdman_wrapper::run(command, &args, |_, _| {}).unwrap();
    };

    if let Some(dvd) = samples.iter().find(|s| !s.info.is_cd) {
        let iso = dir.join("ref.iso");
        extract(ChdmanCommand::ExtractDvd, &dvd.path, &iso, false);
        let (iso_crc, iso_size) = crc32_file(&iso);

        for (label, mode) in [("random (ChdReader)", 0), ("sequential (reader)", 1)] {
            let source = if mode == 0 {
                Source::random("disc.iso", dvd.info.logical_bytes, ChdReader::open(&dvd.path, None, 0).unwrap())
            } else {
                Source::sequential("disc.iso", iso_size, std::fs::File::open(&iso).unwrap())
            };
            let (source, hash) = source.hashed(HashFlags::CRC32);
            let out = dir.join(format!("dvd_{mode}.chd"));
            let args = ChdmanArgs { input: Some("disc.iso".into()), output: Some(out.clone()), output_force: true, ..Default::default() };
            chdman_wrapper::create_from_sources(ChdmanCommand::CreateDvd, &args, &mut [source], |_, _| {}).unwrap();
            assert_eq!(report_crc(&hash.finish().expect("complete hash")), iso_crc, "dvd {label}: hash while compressing");
            let back = dir.join(format!("dvd_{mode}.iso"));
            extract(ChdmanCommand::ExtractDvd, &out, &back, false);
            assert_eq!(crc32_file(&back), (iso_crc, iso_size), "dvd {label}: content differs");
            let _ = std::fs::remove_file(&back);
            println!("chd dvd from {label}: ok");
        }
    } else {
        eprintln!("no DVD CHD sample -- skipping DVD cases");
    }

    if let Some(cd) = samples.iter().find(|s| s.info.is_cd && !s.info.is_gdrom && s.info.num_tracks > 1) {
        let src_dir = dir.join("cd_src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let cue = src_dir.join("disc.cue");
        extract(ChdmanCommand::ExtractCd, &cd.path, &cue, true);
        let originals: Vec<PathBuf> = std::fs::read_dir(&src_dir).unwrap().map(|e| e.unwrap().path()).collect();
        let mut sources: Vec<Source> = originals
            .iter()
            .map(|p| {
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                Source::sequential(name, std::fs::metadata(p).unwrap().len(), std::fs::File::open(p).unwrap())
            })
            .collect();
        let out = dir.join("cd.chd");
        let args = ChdmanArgs { input: Some("disc.cue".into()), output: Some(out.clone()), output_force: true, ..Default::default() };
        chdman_wrapper::create_from_sources(ChdmanCommand::CreateCd, &args, &mut sources, |_, _| {}).unwrap();
        let back_dir = dir.join("cd_back");
        std::fs::create_dir_all(&back_dir).unwrap();
        extract(ChdmanCommand::ExtractCd, &out, &back_dir.join("disc.cue"), true);
        for original in &originals {
            let back = back_dir.join(original.file_name().unwrap());
            assert_eq!(crc32_file(&back), crc32_file(original), "cd: {:?} differs after sources -> CHD -> extract", original.file_name().unwrap());
        }
        println!("chd cd ({} files) from sequential sources: ok", originals.len());
    } else {
        eprintln!("no multi-track CD CHD sample -- skipping CD case");
    }

    if std::env::var_os("ROMEXPLORER_KEEP_OUTPUT").is_none() {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// dolphin creation from a source: RVZ from a random source (a `DiscReader` over another
/// RVZ: RVZ -> RVZ without extracting) and from a sequential source (a plain reader over an
/// ISO, spooled internally). Both outputs must be byte-identical to a plain convert of the
/// ISO, and `on_input_data` must deliver the whole ISO in order.
#[test]
fn dolphin_convert_from_source() {
    use romexplorer::rom::dolphin_wrapper::{self, DiscCompression, DiscFormat, DiscReader, DolphinConvertArgs};
    use romexplorer::rom::native_lib::LibVariant;
    use romexplorer::rom::source::Source;

    if dolphin_wrapper::find_library(LibVariant::Streaming).is_none() {
        eprintln!("no streaming dolphinrvz build -- skipping");
        return;
    }
    let Some((_, input, plain_size)) = probe_dolphin_samples().into_iter().find(|(f, _, _)| *f == "rvz") else {
        eprintln!("no RVZ sample -- skipping");
        return;
    };
    let dir = output_root().join("dolphin_convert_from_source");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let user_dir = dir.join("dolphin_user");
    let iso = dir.join("ref.iso");
    dolphin_wrapper::convert(&DolphinConvertArgs::new(&input, &iso, DiscFormat::Iso).with_user_dir(&user_dir), |_, _| true).unwrap();
    let (iso_crc, _) = crc32_file(&iso);
    let rvz_args = |input: &Path, out: &str| {
        DolphinConvertArgs::new(input, dir.join(out), DiscFormat::Rvz)
            .with_user_dir(&user_dir)
            .with_block_size(131072)
            .with_compression(DiscCompression::Zstd, 5)
    };
    dolphin_wrapper::convert(&rvz_args(&iso, "plain.rvz"), |_, _| true).unwrap();
    let plain = crc32_file(&dir.join("plain.rvz"));

    for (label, mode) in [("random (DiscReader)", 0), ("sequential (reader, spooled)", 1)] {
        let mut source = if mode == 0 {
            Source::random("disc.iso", plain_size, DiscReader::open(&input, Some(&user_dir)).unwrap())
        } else {
            Source::sequential("disc.iso", plain_size, std::fs::File::open(&iso).unwrap())
        };
        let (mut hasher, mut next, mut gaps) = (crc32fast::Hasher::new(), 0u64, 0usize);
        let out = format!("from_source_{mode}.rvz");
        dolphin_wrapper::convert_from_source(
            &rvz_args(Path::new("disc.iso"), &out),
            &mut source,
            |_, _, offset, chunk| {
                gaps += (offset != next) as usize;
                next = offset + chunk.len() as u64;
                hasher.update(chunk);
                true
            },
            |_, _| true,
        )
        .unwrap();
        assert_eq!((gaps, next), (0, plain_size), "{label}: input stream incomplete");
        assert_eq!(hasher.finalize(), iso_crc, "{label}: input stream hash");
        assert_eq!(crc32_file(&dir.join(&out)), plain, "{label}: RVZ differs from a plain convert of the ISO");
        println!("rvz from {label}: ok");
    }
    if std::env::var_os("ROMEXPLORER_KEEP_OUTPUT").is_none() {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Stream-in -> archive across libraries: an RVZ's plain bytes, pushed by
/// `dolphin_wrapper::extract_stream` on a producer thread, flow through `source::pipe` into
/// a 7z being created (and are hashed on the way) -- no intermediate file.
#[test]
fn stream_pipe_rvz_to_7z() {
    use romexplorer::rom::dolphin_wrapper;
    use romexplorer::rom::hash::HashFlags;
    use romexplorer::rom::native_lib::LibVariant;
    use romexplorer::rom::sevenzip_wrapper::{self, create_archive_from_sources, ArchiveFormat, CompressionMethod, SevenZipArchive};
    use romexplorer::rom::source::{pipe, Source};

    if dolphin_wrapper::find_library(LibVariant::Streaming).is_none() || sevenzip_wrapper::find_library(LibVariant::Streaming).is_none() {
        eprintln!("streaming builds missing -- skipping");
        return;
    }
    let Some((_, input, plain_size)) = probe_dolphin_samples().into_iter().find(|(f, _, _)| *f == "rvz") else {
        eprintln!("no RVZ sample -- skipping");
        return;
    };
    let dir = output_root().join("stream_pipe_rvz_to_7z");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let user_dir = dir.join("dolphin_user");

    let (writer, reader) = pipe(16);
    let producer = {
        let (input, user_dir) = (input.clone(), user_dir.clone());
        std::thread::spawn(move || {
            let result = dolphin_wrapper::extract_stream(&input, None, Some(&user_dir), |_, _, _, chunk| writer.write(chunk), |_, _| true);
            if let Err(err) = result {
                writer.fail(err.to_string());
            }
        })
    };
    let (source, hash) = Source::sequential("disc.iso", plain_size, reader).hashed(HashFlags::CRC32);
    let seven = dir.join("disc.7z");
    create_archive_from_sources(&seven, ArchiveFormat::SevenZ, CompressionMethod::Zstd, 3, &mut [source], |_, _| true).unwrap();
    producer.join().unwrap();
    let streamed_crc = report_crc(&hash.finish().expect("complete hash"));

    let archive = SevenZipArchive::open(&seven, ArchiveFormat::SevenZ, |_, _| true).unwrap();
    let out = dir.join("check.iso");
    archive.extract_to_file(0, &out, |_, _| true).unwrap();
    assert_eq!(crc32_file(&out), (streamed_crc, plain_size), "7z member differs from the streamed bytes");
    println!("rvz -> pipe -> 7z: {plain_size} bytes, hash {streamed_crc:08x} ok");
    if std::env::var_os("ROMEXPLORER_KEEP_OUTPUT").is_none() {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Detection straight from a `.chd` (random access, `detect::chd_disc`): every CHD sample of a
/// disc platform must detect as that platform, with the same serial as detecting its original
/// image; and the parallel batch API must agree with the one-by-one results.
#[test]
fn chd_detection_random_access() {
    use romexplorer::rom::detect::{self, chd_disc};

    if !chd_disc::available() {
        eprintln!("no streaming chdman build -- skipping");
        return;
    }
    let cases = discover::discover_disc_archive_cases(&samples_root());
    let mut checked = 0;
    let mut batch_items = Vec::new();
    let mut singles = Vec::new();
    for case in &cases.chd {
        let Some(expected) = support::inputs::rows("disc")
            .find(|r| r.len() >= 2 && r[0] == case.platform)
            .and_then(|r| support::inputs::platform(&r[1]))
        else {
            continue;
        };
        let from_chd = detect::read_metadata_for_input(&case.chd_path, None, true, true);
        let original = match &case.original {
            OriginalRef::Descriptor(p) | OriginalRef::SingleFile(p) => p.clone(),
        };
        let from_original = detect::read_metadata_for_input(&original, None, true, true);
        assert_eq!(from_chd.platform, expected, "{}: platform from CHD", case.platform);
        assert_eq!(from_chd.serial_product_id, from_original.serial_product_id, "{}: serial from CHD vs original", case.platform);
        batch_items.push((case.chd_path.clone(), expected));
        singles.push(chd_disc::chd_disc_metadata(&case.chd_path, expected));
        checked += 1;
    }
    let batch = chd_disc::chd_disc_metadata_batch(&batch_items, None);
    assert_eq!(batch, singles, "batch results differ from one-by-one");
    println!("{checked} CHDs detected by random access, batch agrees");
}

/// `parallel::parallel_decode` (N readers, in-order or to-file delivery) must reproduce the
/// plain extract exactly: a DVD CHD (vs chdman extractdvd) and an RVZ (vs dolphin convert to
/// ISO), at 1, 4 and one-per-logical-CPU threads.
#[test]
fn parallel_decode_matches_extract() {
    use romexplorer::rom::chdman_wrapper::{self, ChdReader, ChdmanArgs, ChdmanCommand};
    use romexplorer::rom::dolphin_wrapper::{self, DiscFormat, DiscReader, DolphinConvertArgs};
    use romexplorer::rom::native_lib::{self, LibVariant};
    use romexplorer::rom::parallel::{parallel_decode, Delivery};

    if chdman_wrapper::find_library(LibVariant::Streaming).is_none() || dolphin_wrapper::find_library(LibVariant::Streaming).is_none() {
        eprintln!("streaming builds missing -- skipping");
        return;
    }
    let dir = output_root().join("parallel_decode_matches_extract");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let user_dir = dir.join("dolphin_user");
    let max = native_lib::recommended_reader_threads();

    let check = |label: &str, reference: &Path, size: u64, block: usize, open_reader: &(dyn Fn() -> Box<dyn romexplorer::rom::source::ReadAt> + Sync)| {
        let (ref_crc, ref_size) = crc32_file(reference);
        assert_eq!(ref_size, size, "{label}: size");
        for threads in [1, 4, max] {
            let mut hasher = crc32fast::Hasher::new();
            let (mut next, mut gaps) = (0u64, 0usize);
            let mut on_block = |offset: u64, bytes: &[u8]| {
                gaps += (offset != next) as usize;
                next = offset + bytes.len() as u64;
                hasher.update(bytes);
                true
            };
            assert!(parallel_decode(|| Ok(open_reader()), size, block, threads, Delivery::InOrder(&mut on_block)).unwrap());
            assert_eq!((gaps, hasher.finalize()), (0, ref_crc), "{label}: in-order, {threads} threads");
            let out = dir.join("out.bin");
            parallel_decode(|| Ok(open_reader()), size, block, threads, Delivery::ToFile(&out)).unwrap();
            assert_eq!(crc32_file(&out), (ref_crc, ref_size), "{label}: to-file, {threads} threads");
        }
        println!("{label}: in-order and to-file match at 1/4/{max} threads");
    };

    if let Some(dvd) = probe_chd_samples().into_iter().find(|s| !s.info.is_cd) {
        let iso = dir.join("chd.iso");
        let args = ChdmanArgs { input: Some(dvd.path.clone()), output: Some(iso.clone()), output_force: true, ..Default::default() };
        chdman_wrapper::run(ChdmanCommand::ExtractDvd, &args, |_, _| {}).unwrap();
        let block = (2 << 20) / dvd.info.hunk_bytes as usize * dvd.info.hunk_bytes as usize;
        let path = dvd.path.clone();
        check("CHD (DVD)", &iso, dvd.info.logical_bytes, block, &|| Box::new(ChdReader::open(&path, None, 0).unwrap()));
    }
    if let Some((_, rvz, plain)) = probe_dolphin_samples().into_iter().find(|(f, _, _)| *f == "rvz") {
        let iso = dir.join("rvz.iso");
        dolphin_wrapper::convert(&DolphinConvertArgs::new(&rvz, &iso, DiscFormat::Iso).with_user_dir(&user_dir), |_, _| true).unwrap();
        let (rvz, user_dir) = (rvz.clone(), user_dir.clone());
        check("RVZ", &iso, plain, 2 << 20, &|| Box::new(DiscReader::open(&rvz, Some(&user_dir)).unwrap()));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// The zlib-ng Deflate swap, and the buffer extract it sits next to
// ---------------------------------------------------------------------------
//
// These four are synthetic on purpose: they build their own archives from generated bytes, so
// unlike the rest of this file they never depend on the samples root and can never skip
// silently for want of a fixture. The only thing they need is the native library.

/// Deterministic bytes with a controllable amount of redundancy, so a test can ask for data
/// inflate has to work at (long back-references reaching across the 32 KB window) or data it
/// cannot compress at all (stored blocks). `pool` is the size of the repeating unit the output
/// is drawn from: a small pool repeats heavily, a pool as large as `len` never repeats.
fn filler(seed: u64, len: usize, pool: usize) -> Vec<u8> {
    let pool_len = pool.clamp(1, len.max(1));
    let mut state = seed | 1;
    let mut unit = Vec::with_capacity(pool_len);
    for _ in 0..pool_len {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        unit.push((state >> 33) as u8);
    }
    unit.iter().copied().cycle().take(len).collect()
}

/// Writes each member as a real file under `dir` and packs them into one archive. Returns the
/// archive path; `create_archive` is the only way to get an archive here without a fixture.
fn pack(
    dir: &Path,
    name: &str,
    format: romexplorer::rom::sevenzip_wrapper::ArchiveFormat,
    method: romexplorer::rom::sevenzip_wrapper::CompressionMethod,
    members: &[(String, Vec<u8>)],
) -> PathBuf {
    use romexplorer::rom::sevenzip_wrapper::{create_archive, CreateArchiveArgs, CreateEntry};

    let entries = members
        .iter()
        .map(|(member, bytes)| {
            let input_path = dir.join(member);
            std::fs::write(&input_path, bytes).unwrap();
            CreateEntry { input_path, archive_name: member.clone() }
        })
        .collect();
    let archive_path = dir.join(name);
    let args = CreateArchiveArgs { format, method, level: 5, entries };
    create_archive(&archive_path, &args, |_, _| true).unwrap();
    archive_path
}

/// Regression test for the use-after-free in `sevenzip_extract_entry_to_buffer`.
///
/// The native side released the in-memory stream in `SetOperationResult` and only then copied
/// out of it (`user/sevenzip_lib.cpp`, fixed by holding an owning `MemStreamOwner` reference
/// for the whole extraction). What made it treacherous is that it *looked* size-dependent:
/// below glibc's 128 KB mmap threshold the freed pages stayed mapped and the read came back
/// correct, so small members passed. Hence members either side of that threshold -- 2 KB is
/// the size that used to pass, 4 MB the size that used to segfault.
///
/// This also covers `read_entry`, which is `extract_to_buffer` under a name that says what it
/// is for, and had no test either.
#[test]
fn extract_to_buffer_survives_members_above_the_mmap_threshold() {
    use romexplorer::rom::native_lib::LibVariant;
    use romexplorer::rom::sevenzip_wrapper::{find_library, ArchiveFormat, CompressionMethod, SevenZipArchive};

    if find_library(LibVariant::Streaming).is_none() {
        eprintln!("no streaming 7z build -- skipping (run deps/7-Zip-zstd/scripts/build.sh, then scripts/move_libs_to_app_root.sh)");
        return;
    }
    let dir = output_root().join("extract_to_buffer_threshold");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // 2 KB: passed even when broken. 120 KB/136 KB: straddling the 128 KB threshold itself.
    // 4 MB: the size in the original reproduction.
    let members: Vec<(String, Vec<u8>)> = [("2k", 2 * 1024), ("120k", 120 * 1024), ("136k", 136 * 1024), ("4m", 4 << 20)]
        .iter()
        .map(|(label, len)| (format!("member_{label}.bin"), filler(*len as u64, *len, 4096)))
        .collect();

    for (format, method, name) in [
        (ArchiveFormat::Zip, CompressionMethod::Deflate, "buffer.zip"),
        (ArchiveFormat::SevenZ, CompressionMethod::Lzma2, "buffer.7z"),
    ] {
        let archive_path = pack(&dir, name, format, method, &members);
        let archive = SevenZipArchive::open_variant(&archive_path, format, LibVariant::Streaming, |_, _| true).unwrap();
        let count = archive.entry_count();
        assert_eq!(count as usize, members.len(), "{name}: entry count");

        let mut checked = 0;
        for index in 0..count {
            let entry = archive.entry(index).unwrap();
            if entry.is_dir {
                continue;
            }
            let (_, original) = members.iter().find(|(member, _)| *member == entry.path).unwrap();

            let buffered = archive.extract_to_buffer(index, |_, _| true).unwrap();
            assert_eq!(buffered.len(), original.len(), "{name}[{}]: extract_to_buffer length", entry.path);
            assert!(buffered == *original, "{name}[{}]: extract_to_buffer bytes differ", entry.path);

            let read = archive.read_entry(index).unwrap();
            assert!(read == *original, "{name}[{}]: read_entry bytes differ", entry.path);
            checked += 1;
        }
        assert_eq!(checked, members.len(), "{name}: not every member was checked");
        println!("{name}: {checked} members extracted to buffer intact, largest {} bytes", 4 << 20);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Deflate decoding, which this build routes through zlib-ng.
///
/// A pass here *is* evidence the zlib-ng decoder ran, because `Makefile.sevenzip_lib` drops
/// `DeflateRegister.o` from the link so exactly one codec claims method 0x40108 -- if the
/// bytes come back correct, that codec decoded them. That invariant lives in the Makefile
/// rather than in Rust, so `the_bundled_build_still_leaves_one_deflate_codec` below guards it.
///
/// The data shapes are chosen to make inflate do different work: a highly redundant member
/// forces long back-references reaching across the 32 KB window, an incompressible one forces
/// stored blocks, and both are large enough to span many blocks.
#[test]
fn deflate_members_decode_through_zlib_ng() {
    use romexplorer::rom::native_lib::LibVariant;
    use romexplorer::rom::sevenzip_wrapper::{find_library, ArchiveFormat, CompressionMethod, SevenZipArchive};

    if find_library(LibVariant::Streaming).is_none() {
        eprintln!("no streaming 7z build -- skipping (run deps/7-Zip-zstd/scripts/build.sh, then scripts/move_libs_to_app_root.sh)");
        return;
    }
    let dir = output_root().join("deflate_zlib_ng");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let members: Vec<(String, Vec<u8>)> = vec![
        // Empty and single-byte: the degenerate ends of the codec.
        ("empty.bin".to_string(), Vec::new()),
        ("one.bin".to_string(), vec![0x5a]),
        // A 64-byte pool over 2 MB: matches far longer than the window, repeated endlessly.
        ("redundant.bin".to_string(), filler(1, 2 << 20, 64)),
        // Pool == length: never repeats, so deflate falls back to stored/near-stored blocks.
        ("incompressible.bin".to_string(), filler(2, 1 << 20, 1 << 20)),
        // A middling mix, spanning the window several times over.
        ("mixed.bin".to_string(), filler(3, 3 << 20, 96 * 1024)),
    ];

    let archive_path = pack(&dir, "deflate.zip", ArchiveFormat::Zip, CompressionMethod::Deflate, &members);
    let archive =
        SevenZipArchive::open_variant(&archive_path, ArchiveFormat::Zip, LibVariant::Streaming, |_, _| true).unwrap();
    let count = archive.entry_count();
    assert_eq!(count as usize, members.len(), "entry count");

    let mut checked = 0;
    for index in 0..count {
        let entry = archive.entry(index).unwrap();
        if entry.is_dir {
            continue;
        }
        let (_, original) = members.iter().find(|(member, _)| *member == entry.path).unwrap();

        // Every one of the three decode paths, since they differ in how the output is handled
        // but share the codec.
        let buffered = archive.extract_to_buffer(index, |_, _| true).unwrap();
        assert!(buffered == *original, "{}: extract_to_buffer bytes differ", entry.path);

        let out_file = dir.join(format!("{}.out", entry.path));
        archive.extract_to_file(index, &out_file, |_, _| true).unwrap();
        assert!(std::fs::read(&out_file).unwrap() == *original, "{}: extract_to_file bytes differ", entry.path);

        let mut streamed = Vec::new();
        archive
            .extract_stream(
                index,
                None,
                |_, _, chunk| {
                    streamed.extend_from_slice(chunk);
                    true
                },
                |_, _| true,
            )
            .unwrap();
        assert!(streamed == *original, "{}: extract_stream bytes differ", entry.path);
        checked += 1;
    }
    assert_eq!(checked, members.len(), "not every member was checked");
    println!("deflate: {checked} members decoded identically through buffer, file and stream");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Guards the invariant `deflate_members_decode_through_zlib_ng` rests on.
///
/// zlib-ng only actually decodes Deflate because `DeflateRegister.o` is absent from the link,
/// leaving `user/zlibng_deflate.cpp` as the sole claimant of method 0x40108. Restore that
/// object and both codecs register: 7-Zip picks one, the round-trip test still passes, and it
/// silently stops proving anything. `DeflateDecoder.o` must stay, because `Deflate64Register`
/// and `ZlibDecoder` still use it.
///
/// Same idea as `backup.rs`'s `every_table_is_classified`, which parses `migrations/` rather
/// than trusting a hand-maintained list to have kept up.
#[test]
fn the_bundled_build_still_leaves_one_deflate_codec() {
    let makefile = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("deps/7-Zip-zstd/Makefile.sevenzip_lib");
    let text = std::fs::read_to_string(&makefile).unwrap_or_else(|err| panic!("{}: {err}", makefile.display()));

    // Object-list lines only. The comments in this Makefile discuss DeflateRegister.o by name,
    // and matching those would make this test pass on the strength of its own documentation.
    let objects: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("$O/"))
        .map(|line| line.trim_start_matches("$O/").trim_end_matches('\\').trim())
        .collect();
    assert!(objects.len() > 50, "parsed only {} object entries -- has the Makefile layout changed?", objects.len());

    assert!(
        !objects.contains(&"DeflateRegister.o"),
        "DeflateRegister.o is back in the link: 7-Zip's own Deflate decoder now also claims method 0x40108, so \
         deflate_members_decode_through_zlib_ng no longer proves zlib-ng decoded anything"
    );
    assert!(objects.contains(&"zlibng_deflate.o"), "zlibng_deflate.o is not linked -- nothing claims method 0x40108");
    assert!(
        objects.contains(&"DeflateDecoder.o"),
        "DeflateDecoder.o was dropped, but Deflate64Register and ZlibDecoder still need it"
    );
    assert!(objects.contains(&"Deflate64Register.o"), "Deflate64Register.o is not linked -- see the Deflate64 test");
}

/// Deflate64 keeps 7-Zip's own decoder, and must not have been caught by the swap.
///
/// zlib cannot do Deflate64's 64 KB window, so `user/zlibng_deflate.cpp` deliberately claims
/// only 0x40108 and leaves 0x40109 alone. Since the two share `DeflateDecoder.o`, dropping
/// `DeflateRegister.o` is exactly the kind of change that could take Deflate64 with it.
#[test]
fn deflate64_still_uses_7zips_own_decoder() {
    use romexplorer::rom::native_lib::LibVariant;
    use romexplorer::rom::sevenzip_wrapper::{find_library, ArchiveFormat, CompressionMethod, SevenZipArchive};

    if find_library(LibVariant::Streaming).is_none() {
        eprintln!("no streaming 7z build -- skipping (run deps/7-Zip-zstd/scripts/build.sh, then scripts/move_libs_to_app_root.sh)");
        return;
    }
    let dir = output_root().join("deflate64");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // Over 64 KB, so the entry actually uses the larger window that distinguishes Deflate64.
    let members = vec![("wide.bin".to_string(), filler(9, 1 << 20, 48 * 1024))];
    let archive_path = pack(&dir, "deflate64.zip", ArchiveFormat::Zip, CompressionMethod::Deflate64, &members);
    let archive =
        SevenZipArchive::open_variant(&archive_path, ArchiveFormat::Zip, LibVariant::Streaming, |_, _| true).unwrap();

    assert_eq!(archive.entry_count(), 1, "entry count");
    let entry = archive.entry(0).unwrap();
    assert_eq!(entry.path, members[0].0, "entry name");
    let buffered = archive.extract_to_buffer(0, |_, _| true).unwrap();
    assert!(buffered == members[0].1, "Deflate64 member did not round-trip -- 0x40109 may have lost its decoder");

    println!("deflate64: {} bytes round-tripped through 7-Zip's own decoder", buffered.len());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Copy's Unpack: one member of a zip or 7z written out as a loose file, part file first, and
/// a member that is no longer there a clear error rather than an empty file.
#[test]
fn unpack_member_writes_one_member_as_a_loose_file() {
    use romexplorer::api::copy::run::unpack_member;
    use romexplorer::rom::sevenzip_wrapper::{create_archive, find_library, ArchiveFormat, CompressionMethod, CreateArchiveArgs, CreateEntry};

    if find_library(romexplorer::rom::native_lib::LibVariant::Standard).is_none() && find_library(romexplorer::rom::native_lib::LibVariant::Streaming).is_none() {
        eprintln!("no 7z build -- skipping (run deps/7-Zip-zstd/scripts/build.sh, then scripts/move_libs_to_app_root.sh)");
        return;
    }
    let dir = output_root().join("unpack_member");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let track = filler(300_000, 300_000, 4096);
    std::fs::write(dir.join("track.bin"), &track).unwrap();
    std::fs::write(dir.join("sheet.cue"), b"FILE \"Game.bin\" BINARY\n").unwrap();

    for (format, method, name) in [(ArchiveFormat::Zip, CompressionMethod::Deflate, "game.zip"), (ArchiveFormat::SevenZ, CompressionMethod::Lzma2, "game.7z")] {
        let entries = vec![
            CreateEntry { input_path: dir.join("sheet.cue"), archive_name: "Game.cue".into() },
            CreateEntry { input_path: dir.join("track.bin"), archive_name: "disc/Game.bin".into() },
        ];
        let archive = dir.join(name);
        create_archive(&archive, &CreateArchiveArgs { format, method, level: 5, entries }, |_, _| true).unwrap();

        let out = dir.join(format!("out-{name}")).join("Game.bin");
        let mut moved = 0u64;
        let written = unpack_member(&archive, format, "disc/Game.bin", track.len() as u64, &out, &mut |n| {
            moved += n;
            true
        })
        .unwrap();
        assert_eq!(written, track.len() as u64, "{name}");
        assert_eq!(moved, track.len() as u64, "{name}: progress adds up to the member");
        assert_eq!(std::fs::read(&out).unwrap(), track, "{name}");
        assert_eq!(std::fs::read_dir(out.parent().unwrap()).unwrap().count(), 1, "{name}: no part file left");

        let missing = unpack_member(&archive, format, "Gone.bin", 1, &dir.join("gone.bin"), &mut |_| true);
        assert!(missing.is_err() && !dir.join("gone.bin").exists(), "{name}");
    }
}

/// The Rust CHD reader against chdman's own `info`, on CHDs chdman itself made: a two-track
/// CD (data and audio), a GD-ROM, a DVD, and an uncompressed DVD. Every field must agree.
#[test]
fn rust_chd_reader_agrees_with_chdman() {
    use romexplorer::rom::chd_header;
    use romexplorer::rom::chdman_wrapper::{run, ChdCompressionCodec, ChdmanArgs, ChdmanCommand};

    let dir = output_root().join("rust_chd_reader");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Data: 40 raw mode-1 sectors with a sync pattern; audio: 30 sectors of a ramp.
    let mut data = Vec::new();
    for i in 0..40u32 {
        let mut sector = vec![0u8; 2352];
        sector[..12].copy_from_slice(&[0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0]);
        sector[15] = 1;
        sector[16..20].copy_from_slice(&i.to_be_bytes());
        data.extend_from_slice(&sector);
    }
    let audio: Vec<u8> = (0..30 * 2352).map(|i| (i % 253) as u8).collect();
    std::fs::write(dir.join("Game (Track 1).bin"), &data).unwrap();
    std::fs::write(dir.join("Game (Track 2).bin"), &audio).unwrap();
    std::fs::write(
        dir.join("Game.cue"),
        "FILE \"Game (Track 1).bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\nFILE \"Game (Track 2).bin\" BINARY\n  TRACK 02 AUDIO\n    INDEX 00 00:00:00\n    INDEX 01 00:02:00\n",
    )
    .unwrap();
    let iso: Vec<u8> = (0..64 * 2048).map(|i| (i / 2048 % 7) as u8).collect();
    std::fs::write(dir.join("dvd.iso"), &iso).unwrap();

    let make = |command, input: &str, output: &str, compression: Option<Vec<ChdCompressionCodec>>| {
        let args = ChdmanArgs { input: Some(dir.join(input)), output: Some(dir.join(output)), output_force: true, compression, ..Default::default() };
        run(command, &args, |_, _| {})
    };
    if let Err(e) = make(ChdmanCommand::CreateCd, "Game.cue", "cd.chd", None) {
        eprintln!("chdman unavailable -- skipping ({e})");
        return;
    }
    make(ChdmanCommand::CreateDvd, "dvd.iso", "dvd.chd", None).unwrap();
    // A GD-ROM: its tracks are CHGD entries.
    std::fs::write(dir.join("track01.bin"), &data).unwrap();
    std::fs::write(dir.join("track02.raw"), &audio).unwrap();
    std::fs::write(dir.join("track03.bin"), &data).unwrap();
    std::fs::write(dir.join("disc.gdi"), "3\n1 0 4 2352 track01.bin 0\n2 190 0 2352 track02.raw 0\n3 45000 4 2352 track03.bin 0\n").unwrap();
    make(ChdmanCommand::CreateCd, "disc.gdi", "gd.chd", None).unwrap();
    make(ChdmanCommand::CreateDvd, "dvd.iso", "plain.chd", Some(vec![ChdCompressionCodec::None])).unwrap();

    for name in ["cd.chd", "dvd.chd", "plain.chd", "gd.chd"] {
        let path = dir.join(name);
        let rust = chd_header::read(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        let chdman = romexplorer::rom::chd_info::read_with_chdman(&path).unwrap();
        let diff = chd_header::differences(&rust, &chdman);
        assert!(diff.is_empty(), "{name}:\n{}", diff.join("\n"));
    }
    let cd = chd_header::read(&dir.join("cd.chd")).unwrap();
    assert_eq!(cd.tracks.len(), 2);
    assert!(chd_header::read(&dir.join("dvd.chd")).unwrap().is_dvd);
}

/// The RetroAchievements hash of a CHD (CD and DVD) and an RVZ, read in memory
/// (`rom::ra_disc`), is the one RAHasher gives the same disc as plain files on disk.
/// Needs xorriso to build the ISO filesystems; skipped without it, chdman or RAHasher.
#[test]
fn ra_hash_of_a_container_in_memory_matches_the_extracted_disc() {
    use romexplorer::rom::chdman_wrapper::{run, ChdmanArgs, ChdmanCommand};
    use romexplorer::rom::hash::{rahash_file, RahashError, RahashOutcome};
    use romexplorer::rom::ra_disc;

    let dir = output_root().join("ra_hash_in_memory");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // A PS1 and a PS2 disc: SYSTEM.CNF naming a boot executable, as RetroAchievements reads them.
    let iso = |name: &str, cnf: &str, exe: &str| -> Option<PathBuf> {
        let root = dir.join(format!("{name}_root"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("SYSTEM.CNF"), cnf).unwrap();
        let mut body = b"PS-X EXE".to_vec();
        body.resize(0x800 + 3 * 2048, 0);
        body[0x1c..0x20].copy_from_slice(&(3u32 * 2048).to_le_bytes());
        for (i, b) in body.iter_mut().enumerate().skip(0x800) {
            *b = (i % 251) as u8;
        }
        std::fs::write(root.join(exe), &body).unwrap();
        let out = dir.join(format!("{name}.iso"));
        let ok = std::process::Command::new("xorriso").args(["-as", "mkisofs", "-quiet", "-o"]).arg(&out).arg(&root).status().map(|s| s.success()).unwrap_or(false);
        ok.then_some(out)
    };
    let Some(ps1) = iso("ps1", "BOOT = cdrom:\\SLUS_123.45;1\r\n", "SLUS_123.45") else {
        eprintln!("no xorriso -- skipping");
        return;
    };
    let ps2 = iso("ps2", "BOOT2 = cdrom0:\\SLUS_200.01;1\r\nVER = 1.00\r\n", "SLUS_200.01").unwrap();
    std::fs::write(dir.join("ps1.cue"), "FILE \"ps1.iso\" BINARY\n  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\n").unwrap();

    let on_disk = |path: &Path, console: u32| match rahash_file(path, console, 0) {
        Ok(RahashOutcome::Done(h)) => Some(h.to_ascii_lowercase()),
        Err(RahashError::Unavailable(m)) => {
            eprintln!("no RAHasher -- skipping ({m})");
            None
        }
        other => panic!("{}: {other:?}", path.display()),
    };
    let Some(ps1_want) = on_disk(&dir.join("ps1.cue"), 12) else { return };
    let ps2_want = on_disk(&ps2, 21).unwrap();

    let make = |command, input: &Path, output: &str| {
        let args = ChdmanArgs { input: Some(input.to_path_buf()), output: Some(dir.join(output)), output_force: true, ..Default::default() };
        run(command, &args, |_, _| {})
    };
    if let Err(e) = make(ChdmanCommand::CreateCd, &dir.join("ps1.cue"), "ps1.chd") {
        eprintln!("chdman unavailable -- skipping ({e})");
        return;
    }
    make(ChdmanCommand::CreateDvd, &ps2, "ps2.chd").unwrap();
    assert_eq!(ra_disc::hash_of(&dir.join("ps1.chd"), 12, false).unwrap(), ps1_want, "PS1 CD CHD");
    assert_eq!(ra_disc::hash_of(&dir.join("ps2.chd"), 21, false).unwrap(), ps2_want, "PS2 DVD CHD");
    let _ = ps1;

    // A GameCube disc: the magic word, an empty apploader and a one-section main.dol.
    let mut gc = vec![0u8; 2 << 20];
    gc[..6].copy_from_slice(b"GTEST1");
    gc[0x1c..0x20].copy_from_slice(&[0xC2, 0x33, 0x9F, 0x3D]);
    gc[0x20..0x30].copy_from_slice(b"RA hash test GC\0");
    gc[0x420..0x424].copy_from_slice(&0x10000u32.to_be_bytes());
    gc[0x424..0x428].copy_from_slice(&0x20000u32.to_be_bytes()); // fst offset
    gc[0x428..0x42c].copy_from_slice(&0x0cu32.to_be_bytes()); // fst size
    gc[0x42c..0x430].copy_from_slice(&0x0cu32.to_be_bytes());
    gc[0x10000..0x10004].copy_from_slice(&0x10100u32.to_be_bytes());
    gc[0x10090..0x10094].copy_from_slice(&0x1000u32.to_be_bytes());
    for (i, b) in gc[0x10100..0x11100].iter_mut().enumerate() {
        *b = (i % 241) as u8;
    }
    gc[0x20000] = 1; // FST root: a directory with one entry, itself
    gc[0x20008..0x2000c].copy_from_slice(&1u32.to_be_bytes());
    let gc_iso = dir.join("gc.iso");
    std::fs::write(&gc_iso, &gc).unwrap();
    let gc_want = on_disk(&gc_iso, 16).unwrap();
    use romexplorer::rom::dolphin_wrapper::{self, DiscCompression, DiscFormat, DolphinConvertArgs};
    let rvz = dir.join("gc.rvz");
    let args = DolphinConvertArgs::new(&gc_iso, &rvz, DiscFormat::Rvz).with_user_dir(&dir.join("dolphin_user")).with_block_size(131072).with_compression(DiscCompression::Zstd, 5);
    match dolphin_wrapper::convert(&args, |_, _| true) {
        Ok(()) => assert_eq!(ra_disc::hash_of(&rvz, 16, false).unwrap(), gc_want, "GameCube RVZ"),
        Err(e) => eprintln!("dolphin could not make an RVZ of the test disc -- RVZ part skipped ({e})"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}
