//! Ground truth for the metadata-detection test (`test_archiver.rs`'s
//! `metadata_platform_detection`): which platform each sample file/directory *should*
//! detect as, from the `disc` / `ext` / `file` / `dirgames` / `pkgdir` / `exclude` records
//! of the test input configuration (see `inputs.rs`).

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use romexplorer::common::models::Platform;

use super::discover::{self, OriginalRef};
use super::inputs;

pub struct MetadataCase {
    /// The sample directory's name (for filtering/reporting only).
    pub platform_dir: String,
    /// The file to feed to `detect::read_metadata_for_input` -- for a CHD/RVZ platform,
    /// its original reference (`.cue`/`.gdi`/`.iso`), never the `.chd`/`.rvz` itself
    /// (detection never decompresses).
    pub input: PathBuf,
    pub expected: Platform,
}

/// Expected platform for a CHD/RVZ sample directory (`disc` records) -- the original file
/// itself comes from `discover`'s own original-file resolution, so this test never
/// re-derives which file is "the original".
fn disc_platform(dir: &str) -> Option<Platform> {
    inputs::rows("disc").find(|r| r.len() >= 2 && r[0] == dir).and_then(|r| inputs::platform(&r[1]))
}

/// Builds every metadata-detection test case: the CHD/RVZ platforms' original references,
/// every mapped plain file in every other (non-excluded) platform directory, the directory
/// games, and the download packages.
pub fn discover_metadata_cases(samples_root: &Path) -> Vec<MetadataCase> {
    let mut cases = Vec::new();
    let mut claimed_dirs: Vec<String> = inputs::rows("exclude").filter_map(|r| r.first().cloned()).collect();

    // Detection only cares about the original reference file, not which codec variant
    // pointed at it -- RVZ's 5 codec variants per platform (zstd/uncompressed/bzip2/lzma/
    // lzma2) all share one `original_iso`, so this dedupes rather than running (and
    // reporting) the identical detection call 5 times over.
    let mut seen_inputs: HashSet<PathBuf> = HashSet::new();

    let disc_cases = discover::discover_disc_archive_cases(samples_root);
    for case in &disc_cases.chd {
        let Some(expected) = disc_platform(&case.platform) else { continue };
        let input = match &case.original {
            OriginalRef::Descriptor(path) | OriginalRef::SingleFile(path) => path.clone(),
        };
        claimed_dirs.push(case.platform.clone());
        if !seen_inputs.insert(input.clone()) {
            continue;
        }
        cases.push(MetadataCase { platform_dir: case.platform.clone(), input, expected });
    }
    for case in &disc_cases.rvz {
        let Some(expected) = disc_platform(&case.platform) else { continue };
        claimed_dirs.push(case.platform.clone());
        if !seen_inputs.insert(case.original_iso.clone()) {
            continue;
        }
        cases.push(MetadataCase { platform_dir: case.platform.clone(), input: case.original_iso.clone(), expected });
    }

    let Ok(platform_dirs) = fs::read_dir(samples_root) else { return cases };
    for entry in platform_dirs.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(platform_dir) = dir.file_name().and_then(|name| name.to_str()) else { continue };
        if claimed_dirs.iter().any(|claimed| claimed == platform_dir) {
            continue;
        }

        let Ok(files) = fs::read_dir(&dir) else { continue };
        for file_entry in files.flatten() {
            let path = file_entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(expected) = expected_platform(platform_dir, &path) else { continue };
            cases.push(MetadataCase { platform_dir: platform_dir.to_string(), input: path, expected });
        }
    }

    // Directory games: every subdirectory of these platform directories is one game folder.
    for row in inputs::rows("dirgames") {
        let [platform_dir, platform] = row else { continue };
        let Some(expected) = inputs::platform(platform) else { continue };
        let Ok(entries) = fs::read_dir(samples_root.join(platform_dir)) else { continue };
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                cases.push(MetadataCase { platform_dir: platform_dir.clone(), input: entry.path(), expected });
            }
        }
    }

    // Download packages: a directory whose subdirectories each hold one directly-parseable `.pkg`.
    for row in inputs::rows("pkgdir") {
        let [platform_dir, platform] = row else { continue };
        let Some(expected) = inputs::platform(platform) else { continue };
        if let Some(pkg) = find_pkg(&samples_root.join(platform_dir)) {
            cases.push(MetadataCase { platform_dir: platform_dir.clone(), input: pkg, expected });
        }
    }

    cases.sort_by(|a, b| (&a.platform_dir, &a.input).cmp(&(&b.platform_dir, &b.input)));
    cases
}

/// `file` records (exact name) win over `ext` records: some platforms share an extension
/// and are told apart only by content, so their samples are keyed by name.
fn expected_platform(platform_dir: &str, path: &Path) -> Option<Platform> {
    let lookup = |kind: &str, key: &str| {
        inputs::rows(kind).find(|r| r.len() >= 3 && r[0] == platform_dir && r[1] == key).and_then(|r| inputs::platform(&r[2]))
    };
    let filename = path.file_name()?.to_str()?;
    if let Some(expected) = lookup("file", filename) {
        return Some(expected);
    }
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    lookup("ext", &ext)
}

/// The first `.pkg` directly inside any subdirectory of `dir`.
fn find_pkg(dir: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_pkg_in(&path) {
                return Some(found);
            }
        }
    }
    None
}

fn find_pkg_in(dir: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    entries
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.is_file() && path.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("pkg")).unwrap_or(false))
}
