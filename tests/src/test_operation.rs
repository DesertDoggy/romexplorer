//! End-to-end tests of the per-game operation flow (`romexplorer::Single`): detect ->
//! extract -> hash -> convert -> archive in one pass. Sample-backed tests use the samples
//! root configured in `support/inputs.rs` and skip when it isn't present; the synthetic ones
//! run anywhere. Per-case tables land in `tests/results/` (git-ignored).
//!
//! Run one: `cargo test --test test_operation <name> -- --nocapture`.

#[path = "support/mod.rs"]
mod support;

use std::path::{Path, PathBuf};
use std::time::Instant;

use romexplorer::common::models::{ArchiveFormat, Platform};
use romexplorer::rom::archive::ArchiveTarget;
use romexplorer::rom::detect::{
    self,
    sheet::{CueSheetOptions, SheetKind},
    HeuristicFlags,
};
use romexplorer::rom::extract::{self, CdOutputFormat, ExtractDestination};
use romexplorer::rom::hash::HashFlags;
use romexplorer::rom::models::{ArchiveArgs, ChdArgs, ChdPresets, ConvertArgs, ConvertFlags, SnesHeader};
use romexplorer::rom::native_lib::{self, LibVariant};
use romexplorer::rom::{chd_info, chdman_wrapper, dolphin_wrapper, sevenzip_wrapper};
use romexplorer::rom::operation::{Detect, OutputCheck};
use romexplorer::{ProgressEvent, Single, SingleArgs, SingleError, SkipReason, Stage, StageResult, StageSwitches};

use support::archive_cases;
use support::discover::{self, OriginalRef};
use support::hashing::sha256_file;
use support::report::{self, CaseResult, FileComparison};

fn samples_root() -> PathBuf {
    support::inputs::samples_root()
}

fn output_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/output")
}

fn sanitize(value: &str) -> String {
    value.chars().map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' }).collect()
}

fn scratch(name: &str) -> PathBuf {
    let dir = output_root().join(format!("operation_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn streaming_builds_present() -> bool {
    let caps = native_lib::detect_capabilities(LibVariant::Streaming);
    caps.all_streaming()
}

fn no_progress(_: &ProgressEvent) -> bool {
    true
}

fn hash_all() -> HashFlags {
    HashFlags::CRC32 | HashFlags::SHA256
}

/// `content_hashes` of an outcome as (name, sha256) pairs.
fn sha256_records(report: &romexplorer::rom::hash::HashReport) -> Vec<(String, String)> {
    report.content_hashes.iter().map(|r| (r.path.clone(), r.sha256.clone().unwrap_or_default())).collect()
}

fn compare_named(records: &[(String, String)], files: &[PathBuf]) -> Vec<FileComparison> {
    files
        .iter()
        .map(|file| {
            let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            let original_hash = sha256_file(file).unwrap_or_default();
            let extracted_hash = records.iter().find(|(n, _)| *n == name).map(|(_, h)| h.clone()).unwrap_or_default();
            FileComparison { original: name.clone(), original_hash: original_hash.clone(), extracted: name, extracted_hash: extracted_hash.clone(), matched: !original_hash.is_empty() && original_hash == extracted_hash }
        })
        .collect()
}

fn case_result(platform: &str, variant: &str, kind: &'static str, start: Instant, files: Vec<FileComparison>, detail: String) -> CaseResult {
    let passed = !files.is_empty() && files.iter().all(|f| f.matched) && detail.is_empty();
    CaseResult { platform: platform.to_string(), variant: variant.to_string(), kind, disc_kind: String::new(), passed, detail, elapsed: start.elapsed(), files }
}

// ---------------------------------------------------------------------------
// Sample-backed
// ---------------------------------------------------------------------------

/// A CD/GD-ROM CHD hashed by `Single` (no disk output) must yield exactly the files
/// `chdman extractcd` writes: the same sheet bytes and the same track bytes, under the same
/// names. This is the gate for the Rust cue/gdi writer and the frame-accurate track reader.
#[test]
fn chd_content_hashes_match_chdman_extractcd() {
    let samples_root = samples_root();
    if !samples_root.is_dir() || !streaming_builds_present() {
        eprintln!("samples root or streaming builds not present -- skipping");
        return;
    }
    let cases = discover::discover_disc_archive_cases(&samples_root);
    let mut results = Vec::new();
    for case in &cases.chd {
        let start = Instant::now();
        let dir = scratch(&format!("chdcontent_{}_{}", sanitize(&case.platform), sanitize(&case.variant)));
        let metadata = match chd_info::read_chd_metadata(&case.chd_path) {
            Ok(meta) => meta,
            Err(err) => {
                results.push(case_result(&case.platform, &case.variant, "chd content", start, Vec::new(), format!("chdman info failed: {err}")));
                continue;
            }
        };
        if metadata.tracks.is_empty() {
            continue; // DVD/raw: covered by the extract tests already
        }
        let cd_sheet = match case.cd_output_format {
            CdOutputFormat::Gdi => SheetKind::Gdi,
            CdOutputFormat::CueBin => SheetKind::Cue,
        };
        let extracted = match extract::extract(&case.chd_path, ArchiveFormat::Chd, ExtractDestination::OutputDir(dir.join("chdman")), Some(&metadata), case.cd_output_format, |_, _| true) {
            Ok(payload) => payload,
            Err(err) => {
                results.push(case_result(&case.platform, &case.variant, "chd content", start, Vec::new(), format!("chdman extract failed: {err}")));
                continue;
            }
        };
        let mut chdman_files = vec![extracted.primary.clone()];
        chdman_files.extend(extracted.siblings.iter().cloned());

        let args = SingleArgs {
            stages: StageSwitches { hash: true, ..StageSwitches::NONE },
            hash_flags: hash_all(),
            cue_sheet: CueSheetOptions::chdman(),
            cd_sheet,
            ..SingleArgs::default()
        };
        let outcome = match Single::new(&case.chd_path, args).run(&mut no_progress) {
            Ok(outcome) => outcome,
            Err(err) => {
                results.push(case_result(&case.platform, &case.variant, "chd content", start, Vec::new(), format!("Single failed: {err}")));
                continue;
            }
        };
        let mut detail = String::new();
        if let StageResult::Err(message) = &outcome.stages.hash {
            detail = format!("hash stage: {message}");
        }
        let records = outcome.input_hashes.as_ref().map(sha256_records).unwrap_or_default();
        if records.len() != chdman_files.len() {
            detail = format!("{detail} record count {} != chdman file count {}", records.len(), chdman_files.len());
        }
        let files = compare_named(&records, &chdman_files);
        let result = case_result(&case.platform, &case.variant, "chd content", start, files, detail.trim().to_string());
        if result.passed {
            let _ = std::fs::remove_dir_all(&dir);
        }
        results.push(result);
    }
    report::write_report_with_hash_label("operation_chd_content", &results, "sha256");
    if results.is_empty() {
        eprintln!("no CD/GD-ROM CHD sample -- nothing checked");
        return;
    }
    let failed = results.iter().filter(|r| !r.passed).count();
    assert_eq!(failed, 0, "{failed} of {} case(s) failed -- see tests/results/operation_chd_content.md", results.len());
}

/// zip -> hash -> 7z in one pass: the input's members are hashed from the decode stream
/// and the 7z is created from that same stream; the 7z's members must match the originals.
#[test]
fn single_zip_hash_to_7z() {
    let samples_root = samples_root();
    if !samples_root.is_dir() || !streaming_builds_present() {
        eprintln!("samples root or streaming builds not present -- skipping");
        return;
    }
    let Some(fixtures) = archive_cases::file_archive_fixtures(&samples_root) else {
        eprintln!("no zip/7z fixtures -- skipping");
        return;
    };
    let mut results = Vec::new();
    for archive in fixtures.archives.iter().filter(|p| p.extension().is_some_and(|e| e == "zip")) {
        let start = Instant::now();
        let stem = archive.file_stem().and_then(|s| s.to_str()).unwrap_or("zip").to_string();
        let dir = scratch(&format!("zip7z_{}", sanitize(&stem)));
        let args = SingleArgs {
            stages: StageSwitches { extract: true, hash: true, archive: true, convert: false },
            hash_flags: hash_all(),
            archive: ArchiveArgs { do_7z: true, ..ArchiveArgs::default() },
            output_dir: Some(dir.clone()),
            metadata: romexplorer::MetadataArgs { forced_platform: Some(Platform::Nes), ..romexplorer::MetadataArgs::default() },
            ..SingleArgs::default()
        };
        let mut events = Vec::new();
        let outcome = match Single::new(archive, args).run(&mut |e: &ProgressEvent| {
            events.push((e.stage, e.total_fraction));
            true
        }) {
            Ok(outcome) => outcome,
            Err(err) => {
                results.push(case_result("zip", &stem, "zip->7z", start, Vec::new(), format!("Single failed: {err}")));
                continue;
            }
        };
        let mut detail = String::new();
        if !outcome.stages.archive.is_ok() {
            detail = format!("archive stage: {:?}", outcome.stages.archive);
        }
        if !events.windows(2).all(|w| w[1].1 >= w[0].1) {
            detail = format!("{detail} progress went backwards");
        }
        if events.last().map_or(true, |(_, f)| (*f - 1.0).abs() > 1e-3) {
            detail = format!("{detail} progress did not reach 1.0");
        }
        let created = dir.join(format!("{stem}.7z"));
        let mut files = Vec::new();
        if created.is_file() {
            match extract::extract(&created, ArchiveFormat::SevenZip, ExtractDestination::OutputDir(dir.join("check")), None, CdOutputFormat::CueBin, |_, _| true) {
                Ok(payload) => {
                    let mut members = vec![payload.primary.clone()];
                    members.extend(payload.siblings.iter().cloned());
                    let records = outcome.input_hashes.as_ref().map(sha256_records).unwrap_or_default();
                    files = compare_named(&records, &members);
                    if let Some(archive_data) = outcome.outputs.iter().find(|o| o.path() == created) {
                        if archive_data.get_content_hashes().map_or(0, |c| c.len()) != records.len() {
                            detail = format!("{detail} output FileData content hashes not carried over");
                        }
                    }
                }
                Err(err) => detail = format!("{detail} 7z extract failed: {err}"),
            }
        } else {
            detail = format!("{detail} no 7z produced");
        }
        let result = case_result("zip", &stem, "zip->7z", start, files, detail.trim().to_string());
        if result.passed {
            let _ = std::fs::remove_dir_all(&dir);
        }
        results.push(result);
    }
    report::write_report_with_hash_label("operation_zip_to_7z", &results, "sha256");
    let failed = results.iter().filter(|r| !r.passed).count();
    assert_eq!(failed, 0, "{failed} of {} case(s) failed -- see tests/results/operation_zip_to_7z.md", results.len());
}

/// CD CHD -> hash -> CHD repack, streamed (no temp files): extracting the new CHD must give
/// the original descriptor's track files back.
#[test]
fn single_chd_cd_repack_streams() {
    let samples_root = samples_root();
    if !samples_root.is_dir() || !streaming_builds_present() {
        eprintln!("samples root or streaming builds not present -- skipping");
        return;
    }
    let cases = discover::discover_disc_archive_cases(&samples_root);
    let mut results = Vec::new();
    for case in cases.chd.iter().filter(|c| matches!(c.original, OriginalRef::Descriptor(_))) {
        let OriginalRef::Descriptor(descriptor) = &case.original else { continue };
        let start = Instant::now();
        let dir = scratch(&format!("chdrepack_{}_{}", sanitize(&case.platform), sanitize(&case.variant)));
        let cd_sheet = match case.cd_output_format {
            CdOutputFormat::Gdi => SheetKind::Gdi,
            CdOutputFormat::CueBin => SheetKind::Cue,
        };
        let args = SingleArgs {
            stages: StageSwitches { extract: true, hash: true, archive: true, convert: false },
            hash_flags: hash_all(),
            archive: ArchiveArgs { do_chd: true, repack_chd: true, chd_args: ChdArgs { preset: ChdPresets::ChdmanDefault, ..ChdArgs::default() }, ..ArchiveArgs::default() },
            output_dir: Some(dir.clone()),
            cd_sheet,
            ..SingleArgs::default()
        };
        let outcome = match Single::new(&case.chd_path, args).run(&mut no_progress) {
            Ok(outcome) => outcome,
            Err(err) => {
                results.push(case_result(&case.platform, &case.variant, "chd repack", start, Vec::new(), format!("Single failed: {err}")));
                continue;
            }
        };
        let mut detail = String::new();
        if outcome.metadata.platform == Platform::Unknown {
            detail = "platform unknown".to_string();
        }
        if !outcome.stages.archive.is_ok() {
            detail = format!("{detail} archive stage: {:?}", outcome.stages.archive);
        }
        let created = dir.join(format!("{}.chd", case.chd_path.file_stem().and_then(|s| s.to_str()).unwrap_or("x")));
        let mut files = Vec::new();
        if created.is_file() {
            let meta = chd_info::read_chd_metadata(&created).ok();
            match extract::extract(&created, ArchiveFormat::Chd, ExtractDestination::OutputDir(dir.join("check")), meta.as_ref(), case.cd_output_format, |_, _| true) {
                Ok(payload) => {
                    let originals = detect::descriptor::descriptor_payload_paths(descriptor);
                    let mut extracted = payload.siblings.clone();
                    extracted.sort();
                    let mut originals_sorted = originals.clone();
                    originals_sorted.sort();
                    for (original, new) in originals_sorted.iter().zip(extracted.iter()) {
                        let a = sha256_file(original).unwrap_or_default();
                        let b = sha256_file(new).unwrap_or_default();
                        files.push(FileComparison {
                            original: original.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string(),
                            original_hash: a.clone(),
                            extracted: new.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string(),
                            extracted_hash: b.clone(),
                            matched: !a.is_empty() && a == b,
                        });
                    }
                    if originals_sorted.len() != extracted.len() {
                        detail = format!("{detail} track count {} != {}", originals_sorted.len(), extracted.len());
                    }
                }
                Err(err) => detail = format!("{detail} extract of repacked CHD failed: {err}"),
            }
        } else {
            detail = format!("{detail} no CHD produced");
        }
        let result = case_result(&case.platform, &case.variant, "chd repack", start, files, detail.trim().to_string());
        if result.passed {
            let _ = std::fs::remove_dir_all(&dir);
        }
        results.push(result);
    }
    report::write_report_with_hash_label("operation_chd_repack", &results, "sha256");
    let failed = results.iter().filter(|r| !r.passed).count();
    assert_eq!(failed, 0, "{failed} of {} case(s) failed -- see tests/results/operation_chd_repack.md", results.len());
}

/// RVZ -> hash -> RVZ repack: the input hash equals the original ISO's, and the repacked RVZ
/// converts back to the same ISO bytes.
#[test]
fn single_rvz_repack() {
    let samples_root = samples_root();
    if !samples_root.is_dir() || !streaming_builds_present() {
        eprintln!("samples root or streaming builds not present -- skipping");
        return;
    }
    let cases = discover::discover_disc_archive_cases(&samples_root);
    let mut results = Vec::new();
    for case in &cases.rvz {
        let start = Instant::now();
        let dir = scratch(&format!("rvzrepack_{}_{}", sanitize(&case.platform), sanitize(&case.codec)));
        let args = SingleArgs {
            stages: StageSwitches { extract: true, hash: true, archive: true, convert: false },
            hash_flags: hash_all(),
            archive: ArchiveArgs { do_rvz: true, repack_rvz: true, ..ArchiveArgs::default() },
            output_dir: Some(dir.clone()),
            temp_dir: Some(dir.join("dolphin_user")),
            ..SingleArgs::default()
        };
        let outcome = match Single::new(&case.rvz_path, args).run(&mut no_progress) {
            Ok(outcome) => outcome,
            Err(err) => {
                results.push(case_result(&case.platform, &case.codec, "rvz repack", start, Vec::new(), format!("Single failed: {err}")));
                continue;
            }
        };
        let mut detail = String::new();
        if !outcome.stages.archive.is_ok() {
            detail = format!("archive stage: {:?}", outcome.stages.archive);
        }
        let mut files = Vec::new();
        let iso_hash = sha256_file(&case.original_iso).unwrap_or_default();
        let streamed = outcome.input_hashes.as_ref().and_then(|r| r.sha256.clone()).unwrap_or_default();
        files.push(FileComparison { original: "original iso".into(), original_hash: iso_hash.clone(), extracted: "streamed input hash".into(), extracted_hash: streamed.clone(), matched: iso_hash == streamed });
        let created = dir.join(format!("{}.rvz", case.rvz_path.file_stem().and_then(|s| s.to_str()).unwrap_or("x")));
        if created.is_file() {
            let back = dir.join("back.iso");
            let convert = dolphin_wrapper::DolphinConvertArgs::new(&created, &back, dolphin_wrapper::DiscFormat::Iso).with_user_dir(dir.join("dolphin_user"));
            match dolphin_wrapper::convert(&convert, |_, _| true) {
                Ok(()) => {
                    let back_hash = sha256_file(&back).unwrap_or_default();
                    files.push(FileComparison { original: "original iso".into(), original_hash: iso_hash.clone(), extracted: "repacked rvz -> iso".into(), extracted_hash: back_hash.clone(), matched: iso_hash == back_hash });
                }
                Err(err) => detail = format!("{detail} converting the repacked RVZ back failed: {err}"),
            }
        } else {
            detail = format!("{detail} no RVZ produced");
        }
        let result = case_result(&case.platform, &case.codec, "rvz repack", start, files, detail.trim().to_string());
        if result.passed {
            let _ = std::fs::remove_dir_all(&dir);
        }
        results.push(result);
    }
    report::write_report_with_hash_label("operation_rvz_repack", &results, "sha256");
    let failed = results.iter().filter(|r| !r.passed).count();
    assert_eq!(failed, 0, "{failed} of {} case(s) failed -- see tests/results/operation_rvz_repack.md", results.len());
}

/// A cue sheet with its track files -> hash -> CHD: the CHD extracts back to the same tracks.
#[test]
fn single_cue_set_to_chd() {
    let samples_root = samples_root();
    if !samples_root.is_dir() || !streaming_builds_present() {
        eprintln!("samples root or streaming builds not present -- skipping");
        return;
    }
    let inputs = archive_cases::select_disc_inputs(&samples_root);
    let Some(cue) = inputs.cue else {
        eprintln!("no multi-track cue sample -- skipping");
        return;
    };
    let start = Instant::now();
    let dir = scratch("cue_to_chd");
    let args = SingleArgs {
        stages: StageSwitches { extract: false, hash: true, archive: true, convert: false },
        hash_flags: hash_all(),
        archive: ArchiveArgs { do_chd: true, ..ArchiveArgs::default() },
        output_dir: Some(dir.clone()),
        ..SingleArgs::default()
    };
    let outcome = Single::new(&cue, args).run(&mut no_progress).expect("Single run");
    assert!(outcome.stages.archive.is_ok(), "archive stage: {:?}", outcome.stages.archive);
    let created = dir.join(format!("{}.chd", cue.file_stem().and_then(|s| s.to_str()).unwrap()));
    assert!(created.is_file(), "no CHD produced");
    let meta = chd_info::read_chd_metadata(&created).expect("chdman info");
    let payload = extract::extract(&created, ArchiveFormat::Chd, ExtractDestination::OutputDir(dir.join("check")), Some(&meta), CdOutputFormat::CueBin, |_, _| true).expect("extract back");
    let mut originals = detect::descriptor::descriptor_payload_paths(&cue);
    originals.sort();
    let mut extracted = payload.siblings.clone();
    extracted.sort();
    let mut files = Vec::new();
    for (original, new) in originals.iter().zip(extracted.iter()) {
        let a = sha256_file(original).unwrap();
        let b = sha256_file(new).unwrap();
        files.push(FileComparison {
            original: original.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string(),
            original_hash: a.clone(),
            extracted: new.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string(),
            extracted_hash: b.clone(),
            matched: a == b,
        });
    }
    let detail = if originals.len() == extracted.len() { String::new() } else { format!("track count {} != {}", originals.len(), extracted.len()) };
    let result = case_result("cue", "cue set", "cue->chd", start, files, detail);
    let passed = result.passed;
    report::write_report_with_hash_label("operation_cue_to_chd", &[result], "sha256");
    if passed {
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(passed, "see tests/results/operation_cue_to_chd.md");
}

/// Metadata-only runs agree with `detect::read_metadata_for_input` on the platform.
#[test]
fn single_metadata_only_matches_read_metadata_for_input() {
    let samples_root = samples_root();
    if !samples_root.is_dir() {
        eprintln!("samples root not present -- skipping");
        return;
    }
    let cases = support::metadata_cases::discover_metadata_cases(&samples_root);
    let mut mismatches = Vec::new();
    for case in &cases {
        let expected = detect::read_metadata_for_input(&case.input, None, true, true);
        let outcome = Single::new(&case.input, SingleArgs::default()).run(&mut no_progress).expect("metadata-only run");
        if outcome.metadata.platform != expected.platform {
            mismatches.push(format!("{}: {:?} vs {:?}", case.platform_dir, outcome.metadata.platform, expected.platform));
        }
        assert!(outcome.input_hashes.is_none(), "metadata-only run must not hash");
    }
    assert!(mismatches.is_empty(), "{} mismatch(es):\n{}", mismatches.len(), mismatches.join("\n"));
}

// ---------------------------------------------------------------------------
// Synthetic
// ---------------------------------------------------------------------------

fn temp_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn archive_args_exclusivity() {
    let ok = ArchiveArgs { do_chd: true, do_rvz: true, ..ArchiveArgs::default() };
    assert!(ok.validate().is_ok());
    let bad = [
        ArchiveArgs { do_zip: true, do_7z: true, ..ArchiveArgs::default() },
        ArchiveArgs { do_rvz: true, do_gcz: true, ..ArchiveArgs::default() },
        ArchiveArgs { do_chd: true, do_zip: true, ..ArchiveArgs::default() },
        ArchiveArgs { do_wia: true, do_7z: true, ..ArchiveArgs::default() },
    ];
    for args in bad {
        assert!(args.validate().is_err(), "{args:?} should be rejected");
    }
    let dir = scratch("args");
    let input = temp_file(&dir, "game.sfc", &[0u8; 1024]);
    let args = SingleArgs { stages: StageSwitches::ALL, archive: ArchiveArgs { do_zip: true, do_7z: true, ..ArchiveArgs::default() }, output_dir: Some(dir.clone()), ..SingleArgs::default() };
    assert!(matches!(Single::new(&input, args).run(&mut no_progress), Err(SingleError::InvalidArgs(_))));
    let args = SingleArgs { stages: StageSwitches::ALL, archive: ArchiveArgs { do_zip: true, ..ArchiveArgs::default() }, output_dir: None, ..SingleArgs::default() };
    assert!(matches!(Single::new(&input, args).run(&mut no_progress), Err(SingleError::InvalidArgs(_))));
    let _ = std::fs::remove_dir_all(dir);
}

/// A plain file nothing recognises: detect reports the failure, hash still runs, convert
/// is skipped (there's no platform-agnostic conversion), but zip archiving -- which needs
/// no platform, just the file's own bytes -- still runs. `Single::plan`'s archive-stage
/// gating used to block zip/7z here too (matching only `file_target()`'s check *after* an
/// unconditional `!platform_known` gate); fixed so only the genuinely platform-driven
/// targets (CHD, the Dolphin formats) require a known platform.
#[test]
fn unknown_platform_skips_convert_but_still_zips_and_hashes() {
    let dir = scratch("unknown");
    let bytes: Vec<u8> = (0..4096u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    let input = temp_file(&dir, "mystery.rom", &bytes);
    let args = SingleArgs {
        stages: StageSwitches::ALL,
        hash_flags: HashFlags::CRC32,
        convert: ConvertArgs { convert_flags: ConvertFlags::all(), ..ConvertArgs::default() },
        archive: ArchiveArgs { do_zip: true, ..ArchiveArgs::default() },
        output_dir: Some(dir.join("out")),
        ..SingleArgs::default()
    };
    let outcome = Single::new(&input, args).run(&mut no_progress).unwrap();
    assert_eq!(outcome.metadata.platform, Platform::Unknown);
    assert!(outcome.stages.detect.is_err());
    assert_eq!(outcome.stages.convert, StageResult::Skipped(SkipReason::PlatformUnknown));
    assert_eq!(outcome.stages.archive, StageResult::Ok, "zip needs no platform, unlike CHD/RVZ/GCZ/WIA");
    assert_eq!(outcome.archive_target, Some(romexplorer::rom::archive::ArchiveTarget::Zip));
    assert!(matches!(outcome.stages.extract, StageResult::Skipped(SkipReason::NotApplicable(_))));
    assert!(outcome.stages.hash.is_ok());
    let mut crc = crc32fast::Hasher::new();
    crc.update(&bytes);
    assert_eq!(outcome.input_hashes.unwrap().crc32.unwrap(), format!("{:08x}", crc.finalize()));
    assert_eq!(outcome.outputs.len(), 1, "the zip itself");
    assert!(outcome.outputs[0].path().extension().and_then(|e| e.to_str()) == Some("zip"));
    let _ = std::fs::remove_dir_all(dir);
}

/// Unlike zip/7z, CHD's/the Dolphin formats' internal layout depends on which platform the
/// content is -- so with only a platform-driven target enabled, an unknown platform still
/// correctly skips archiving with `PlatformUnknown`, not a silent no-op.
#[test]
fn unknown_platform_still_skips_a_platform_driven_only_archive_target() {
    let dir = scratch("unknown_chd_only");
    let bytes: Vec<u8> = (0..4096u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    let input = temp_file(&dir, "mystery.rom", &bytes);
    let args = SingleArgs {
        stages: StageSwitches::ALL,
        archive: ArchiveArgs { do_chd: true, ..ArchiveArgs::default() },
        output_dir: Some(dir.join("out")),
        ..SingleArgs::default()
    };
    let outcome = Single::new(&input, args).run(&mut no_progress).unwrap();
    assert_eq!(outcome.metadata.platform, Platform::Unknown);
    assert_eq!(outcome.stages.archive, StageResult::Skipped(SkipReason::PlatformUnknown));
    assert!(outcome.outputs.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

/// Forced platform + SNES conversion: a bare ROM gets an SMC header on the way to the
/// output directory, both hash sets are reported, and the written file carries the
/// converted hash.
#[test]
fn forced_snes_convert_writes_headered_copy_with_both_hash_sets() {
    let dir = scratch("snes_convert");
    let rom: Vec<u8> = (0..(64 * 1024u32)).map(|i| (i % 251) as u8).collect();
    let input = temp_file(&dir, "game.sfc", &rom);
    let out = dir.join("out");
    let args = SingleArgs {
        metadata: romexplorer::MetadataArgs { forced_platform: Some(Platform::Snes), ..romexplorer::MetadataArgs::default() },
        stages: StageSwitches { extract: false, hash: true, convert: true, archive: false },
        hash_flags: HashFlags::SHA256,
        convert: ConvertArgs { convert_flags: ConvertFlags::SNES, snes_header: SnesHeader::Smc, ..ConvertArgs::default() },
        output_dir: Some(out.clone()),
        ..SingleArgs::default()
    };
    let mut events: Vec<(Stage, f32)> = Vec::new();
    let outcome = Single::new(&input, args)
        .run(&mut |e: &ProgressEvent| {
            events.push((e.stage, e.total_fraction));
            true
        })
        .unwrap();
    assert!(outcome.stages.convert.is_ok(), "{:?}", outcome.stages.convert);
    assert!(outcome.stages.hash.is_ok());
    let written = out.join("game.smc");
    assert!(written.is_file(), "converted file not written");
    let converted = std::fs::read(&written).unwrap();
    assert_eq!(converted.len(), rom.len() + 512);
    assert_eq!(&converted[512..], &rom[..]);
    assert_eq!(outcome.input_hashes.as_ref().unwrap().sha256.as_deref().unwrap(), sha256_file(&input).unwrap());
    assert_eq!(outcome.converted_hashes.as_ref().unwrap().sha256.as_deref().unwrap(), sha256_file(&written).unwrap());
    let data = outcome.outputs.iter().find(|o| o.path() == written).expect("FileData for the written file");
    assert_eq!(data.get_sha256(), Some(sha256_file(&written).unwrap().as_str()));
    assert!(data.last_full_check().is_some());
    assert!(events.windows(2).all(|w| w[1].1 >= w[0].1), "progress went backwards: {events:?}");
    assert!((events.last().unwrap().1 - 1.0).abs() < 1e-3);
    let _ = std::fs::remove_dir_all(dir);
}

/// Output safety: an output that would overwrite an input file, or one that already
/// exists, is refused before any work.
#[test]
fn output_collision_and_exists_are_refused() {
    if sevenzip_wrapper::find_library(LibVariant::Streaming).is_none() {
        eprintln!("sevenzip streaming build not present -- skipping");
        return;
    }
    let dir = scratch("collide");
    let rom = temp_file(&dir, "game.sfc", &[7u8; 2048]);
    // A real zip, made by the library itself, so the input is a decodable container.
    let zip = romexplorer::archive(&[rom.clone()], ArchiveTarget::Zip, &Default::default(), romexplorer::rom::archive::ArchiveDestination::OutputDir(dir.clone()), |_, _| true)
        .expect("create fixture zip")
        .primary;
    assert_eq!(zip, dir.join("game.zip"));
    let forced = romexplorer::MetadataArgs { forced_platform: Some(Platform::Snes), ..romexplorer::MetadataArgs::default() };
    // Repacking into the input's own directory would overwrite the input.
    let args = SingleArgs {
        metadata: forced.clone(),
        stages: StageSwitches { extract: true, hash: true, archive: true, convert: false },
        archive: ArchiveArgs { do_zip: true, repack_zip: true, ..ArchiveArgs::default() },
        output_dir: Some(dir.clone()),
        ..SingleArgs::default()
    };
    match Single::new(&zip, args).run(&mut no_progress) {
        Err(SingleError::OutputCollidesWithInput(path)) => assert_eq!(path, zip),
        other => panic!("expected OutputCollidesWithInput, got {other:?}"),
    }
    // Extracting into a directory where the member already exists is refused too.
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("game.sfc"), b"existing").unwrap();
    let args = SingleArgs { metadata: forced, stages: StageSwitches { extract: true, hash: true, archive: false, convert: false }, output_dir: Some(out.clone()), ..SingleArgs::default() };
    match Single::new(&zip, args).run(&mut no_progress) {
        Err(SingleError::OutputExists(path)) => assert_eq!(path, out.join("game.sfc")),
        other => panic!("expected OutputExists, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// The detection cycle drops candidates whose parser positively rejects the input.
#[test]
fn metadata_cycle_shrinks_on_rejection() {
    let dir = scratch("cycle");
    let input = temp_file(&dir, "blank.gba", &vec![0u8; 64 * 1024]);
    let outcome = detect::metadata_cycle(&input, vec![Platform::GameBoy, Platform::GameBoyAdvance], HeuristicFlags::new(false, false));
    assert_eq!(outcome.platform, Platform::Unknown);
    assert!(!outcome.remaining.contains(&Platform::GameBoyAdvance), "GBA parser should reject a blank file: {:?}", outcome.trail);
    assert!(!outcome.remaining.contains(&Platform::GameBoy), "GB parser should reject a blank file: {:?}", outcome.trail);
    let _ = std::fs::remove_dir_all(dir);
}

/// Diagnostic (ignored): prints every CD/GD-ROM CHD sample's track table as chdman reports
/// it, as the reader reports it, and the layout/sheet this crate derives -- for comparing
/// against `chdman extractcd` output when a content-hash case fails.
/// `cargo test --test test_operation dump_cd_layouts -- --ignored --nocapture`
#[test]
#[ignore]
fn dump_cd_layouts() {
    let samples_root = samples_root();
    if !samples_root.is_dir() || !streaming_builds_present() {
        return;
    }
    for case in &discover::discover_disc_archive_cases(&samples_root).chd {
        let Ok(meta) = chd_info::read_chd_metadata(&case.chd_path) else { continue };
        if meta.tracks.is_empty() {
            continue;
        }
        let reader = chdman_wrapper::ChdReader::open(&case.chd_path, None, 0).unwrap();
        let info = reader.info().unwrap();
        println!("== {} [{}] version {} gdrom {} tracks {}", case.platform, case.variant, info.version, info.is_gdrom, info.num_tracks);
        for (i, t) in meta.tracks.iter().enumerate() {
            let r = reader.track(i as u32).unwrap();
            println!(
                "  track {:2}: info frames {:6} pad {:?} pregap {:?} pgtype {:?} in_file {} | reader frames {:6} pregap {:3} postgap {:3} phys {:6} log {:6} chd {:6} type {} data {} sub {}",
                t.track_number, t.frames, t.pad, t.pregap, t.pregap_type, t.pregap_in_file, r.frames, r.pregap, r.postgap, r.physical_start, r.logical_start, r.chd_frame_start, r.track_type, r.data_size, r.sub_size
            );
        }
        for kind in [SheetKind::Cue, SheetKind::Gdi] {
            let layout = romexplorer::rom::chd_cd::cd_layout(&reader, Some(&meta), "Sample", kind, &CueSheetOptions::chdman()).unwrap();
            for f in layout.files() {
                println!("  {kind:?} file {} bytes {}", f.0, f.2 - f.1);
            }
            println!("{}", String::from_utf8_lossy(&layout.sheet_bytes(&CueSheetOptions::chdman())));
        }
    }
}

#[test]
fn chdman_reader_lock_free_source_can_feed_creation() {
    // Sanity: the ChdReader-based random source does not take chdman's call lock, so a
    // reader-fed creation cannot deadlock. Exercised for real by the repack tests; here
    // just confirm the API shapes line up (a reader is a `ReadAt` source).
    fn assert_read_at<T: romexplorer::rom::source::ReadAt>() {}
    assert_read_at::<chdman_wrapper::ChdReader>();
    assert_read_at::<romexplorer::rom::chd_cd::CdImageReader>();
}

/// `Detect::Trust` takes the caller's platform and reads no headers.
#[test]
fn trusted_platform_skips_detection() {
    let dir = scratch("trust");
    let input = temp_file(&dir, "whatever.bin", &[7u8; 4096]);
    let args = SingleArgs { stages: StageSwitches { hash: true, ..StageSwitches::NONE }, hash_flags: HashFlags::CRC32, detect: Detect::Trust(Platform::PlayStation), ..SingleArgs::default() };
    let outcome = Single::new(&input, args).run(&mut no_progress).unwrap();
    assert_eq!(outcome.metadata.platform, Platform::PlayStation);
    assert!(outcome.stages.detect.is_ok());
    let off = SingleArgs { stages: StageSwitches { hash: true, ..StageSwitches::NONE }, hash_flags: HashFlags::CRC32, detect: Detect::Trust(Platform::Unknown), ..SingleArgs::default() };
    let outcome = Single::new(&input, off).run(&mut no_progress).unwrap();
    assert_eq!(outcome.metadata.platform, Platform::Unknown);
    assert!(matches!(outcome.stages.detect, StageResult::Skipped(_)));
    assert!(outcome.stages.hash.is_ok(), "hashing needs no platform");
    let _ = std::fs::remove_dir_all(dir);
}

/// The container's own bytes are hashed beside the decode, and what was written is read back:
/// its own bytes, and its members decoded -- the identities a scan of either file would record.
#[test]
fn container_hash_and_output_read_back() {
    if !streaming_builds_present() {
        eprintln!("streaming builds not present -- skipping");
        return;
    }
    let dir = scratch("container");
    let bytes: Vec<u8> = (0..300_000u32).map(|i| (i.wrapping_mul(2654435761) >> 11) as u8).collect();
    let rom = temp_file(&dir, "game.rom", &bytes);
    let flags = HashFlags::CRC32 | HashFlags::SHA1;
    let zip_args = SingleArgs {
        stages: StageSwitches { hash: true, archive: true, ..StageSwitches::NONE },
        hash_flags: flags,
        archive: ArchiveArgs { do_zip: true, ..ArchiveArgs::default() },
        output_dir: Some(dir.join("zip")),
        detect: Detect::Trust(Platform::Unknown),
        container_hash: Some(flags),
        output_check: Some(OutputCheck { flags, members: true }),
        ..SingleArgs::default()
    };
    let zipped = Single::new(&rom, zip_args).run(&mut no_progress).unwrap();
    // A plain file: its container hash is its one content record, no second read.
    let plain = romexplorer::hash(&rom, &romexplorer::rom::hash::HashOptions { flags, ..Default::default() }, |_| {}).unwrap();
    let got = zipped.input_container.clone().unwrap().unwrap();
    assert_eq!((got.crc32.clone(), got.sha1.clone()), (plain.crc32.clone(), plain.sha1.clone()));
    let zip = dir.join("zip").join("game.zip");
    let zip_raw = romexplorer::hash(&zip, &romexplorer::rom::hash::HashOptions { flags, ..Default::default() }, |_| {}).unwrap();
    let check = zipped.checked_outputs.iter().find(|c| c.path == zip).expect("the zip was read back");
    assert_eq!(check.container.as_ref().unwrap().sha1, zip_raw.sha1, "the zip's own bytes, not its member's");
    let members = check.members.clone().unwrap().unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].sha1, plain.sha1, "the member decoded back from disk is the rom");
    let report = zipped.outputs.iter().find(|o| o.path() == zip.as_path()).unwrap().get_hash_report().cloned().unwrap();
    assert_eq!(report.sha1, zip_raw.sha1, "the archive's identity is its own bytes");
    assert_eq!(report.content_hashes.len(), 1);

    // The zip as input, repacked to 7z: its container hash comes off a raw read beside the decode.
    let repack = SingleArgs {
        stages: StageSwitches { hash: true, archive: true, ..StageSwitches::NONE },
        hash_flags: flags,
        archive: ArchiveArgs { do_7z: true, ..ArchiveArgs::default() },
        output_dir: Some(dir.join("7z")),
        detect: Detect::Trust(Platform::Unknown),
        container_hash: Some(flags),
        ..SingleArgs::default()
    };
    let repacked = Single::new(&zip, repack).run(&mut no_progress).unwrap();
    assert_eq!(repacked.input_container.unwrap().unwrap().sha1, zip_raw.sha1);
    assert_eq!(repacked.input_hashes.unwrap().content_hashes[0].sha1, plain.sha1);
    assert!(repacked.checked_outputs.is_empty(), "no read-back unless asked");
    let _ = std::fs::remove_dir_all(dir);
}

/// A directory whose track sorts before its sheet, into a CHD. chdman reads the sheet to its
/// end before any track while the producer streams one file at a time through bounded pipes;
/// unless the sheet is streamed first, the two wait on each other forever.
#[test]
fn directory_with_tracks_before_the_sheet_to_chd() {
    if !streaming_builds_present() {
        eprintln!("streaming builds not present -- skipping");
        return;
    }
    let dir = scratch("bins_first");
    let game = dir.join("game");
    std::fs::create_dir_all(&game).unwrap();
    // Well past the pipes' 8 x 4 MiB, so a wrong order cannot slip through on buffering.
    let track: Vec<u8> = (0..2352u32 * 40_000).map(|i| (i % 251) as u8).collect();
    temp_file(&game, "Game (Track 1).bin", &track);
    temp_file(&game, "Game.cue", b"FILE \"Game (Track 1).bin\" BINARY\r\n  TRACK 01 MODE1/2352\r\n    INDEX 01 00:00:00\r\n");
    let out = dir.join("out");
    let args = SingleArgs {
        stages: StageSwitches { hash: true, archive: true, ..StageSwitches::NONE },
        hash_flags: HashFlags::CRC32,
        archive: ArchiveArgs { do_chd: true, ..ArchiveArgs::default() },
        output_dir: Some(out.clone()),
        detect: Detect::Trust(Platform::PlayStation),
        ..SingleArgs::default()
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let input = game.clone();
    std::thread::spawn(move || {
        let _ = tx.send(Single::new(&input, args).run(&mut no_progress).map(|o| (o.stages.archive.clone(), o.content_files.iter().map(|f| f.name.clone()).collect::<Vec<_>>())));
    });
    let (archive, order) = rx.recv_timeout(std::time::Duration::from_secs(300)).expect("deadlocked: no result in 5 minutes").unwrap();
    assert_eq!(order, vec!["Game.cue".to_string(), "Game (Track 1).bin".to_string()]);
    assert!(archive.is_ok(), "archive stage: {archive:?}");
    assert!(out.join("game.chd").is_file());
    let _ = std::fs::remove_dir_all(dir);
}
