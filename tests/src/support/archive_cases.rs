//! Input selection for the archive-creation tests. Inputs are chosen by what they *are*
//! (descriptor type, detected platform, or "the non-archive files next to archives"),
//! never by name.

use std::path::{Path, PathBuf};

use romexplorer::rom::detect;
use romexplorer::rom::platforms;
use romexplorer::common::models::Platform;

use super::discover::{self, OriginalRef};

pub struct DiscInputs {
    /// A `.gdi` descriptor.
    pub gdi: Option<PathBuf>,
    /// A multi-track `.cue` for a disc-based console (content-verified).
    pub cue: Option<PathBuf>,
    /// A single-file `.iso` for a handheld disc format (content-verified).
    pub dvd_iso: Option<PathBuf>,
    /// A GameCube `.iso` (content-verified).
    pub dolphin_iso: Option<PathBuf>,
}

/// Whether `path`'s own content is recognised as `platform` by that platform's parser
/// (heuristics off, so only content counts).
fn is_platform(path: &Path, platform: Platform) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let size = std::fs::metadata(path).ok().map(|m| m.len());
    platforms::deep_try_platform(path, &ext, size, platform, detect::HeuristicFlags::new(false, false)).map(|meta| meta.platform) == Some(platform)
}

pub fn select_disc_inputs(samples_root: &Path) -> DiscInputs {
    let cases = discover::discover_disc_archive_cases(samples_root);
    let mut inputs = DiscInputs { gdi: None, cue: None, dvd_iso: None, dolphin_iso: None };

    for case in &cases.chd {
        match &case.original {
            OriginalRef::Descriptor(path) => {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
                if ext == "gdi" && inputs.gdi.is_none() {
                    inputs.gdi = Some(path.clone());
                } else if ext == "cue" && inputs.cue.is_none() && is_platform(path, Platform::TurboGrafxCd) {
                    inputs.cue = Some(path.clone());
                }
            }
            OriginalRef::SingleFile(path) => {
                if inputs.dvd_iso.is_none() && is_platform(path, Platform::PlayStationPortable) {
                    inputs.dvd_iso = Some(path.clone());
                }
            }
        }
    }

    for case in &cases.rvz {
        if inputs.dolphin_iso.is_none() && is_platform(&case.original_iso, Platform::GameCube) {
            inputs.dolphin_iso = Some(case.original_iso.clone());
        }
    }

    inputs
}

pub struct FileArchiveFixtures {
    /// Existing `.zip` / `.7z` files, sorted by path.
    pub archives: Vec<PathBuf>,
    /// The non-archive files next to them (what they were made from), sorted by path.
    pub controls: Vec<PathBuf>,
}

fn is_archive(path: &Path) -> bool {
    matches!(path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(), Some("zip") | Some("7z"))
}

/// Finds the sample directory that holds zip/7z fixtures.
pub fn file_archive_fixtures(samples_root: &Path) -> Option<FileArchiveFixtures> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(samples_root).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();

    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
        if files.iter().any(|p| is_archive(p)) {
            let (mut archives, mut controls): (Vec<PathBuf>, Vec<PathBuf>) = files.into_iter().partition(|p| is_archive(p));
            archives.sort();
            controls.sort();
            return Some(FileArchiveFixtures { archives, controls });
        }
    }
    None
}
