use std::fs;
use std::path::{Path, PathBuf};

use romexplorer::rom::detect::descriptor;
use romexplorer::rom::extract;

/// How a CHD's original reference is shaped.
pub enum OriginalRef {
    /// A `.cue` or `.gdi` descriptor -- extraction produces one file per track it lists,
    /// in the same order (see `descriptor::descriptor_payload_paths`, which resolves
    /// both).
    Descriptor(PathBuf),
    /// A single flat image with no track structure (a DVD-shaped disc).
    SingleFile(PathBuf),
}

pub struct ChdCase {
    pub platform: String,
    pub variant: String,
    pub chd_path: PathBuf,
    pub original: OriginalRef,
    /// Which chdman output mode this case extracts with -- always
    /// [`extract::CdOutputFormat::CueBin`] except when `original` is the `.gdi` half of a
    /// disc that has both.
    pub cd_output_format: extract::CdOutputFormat,
}

pub struct RvzCase {
    pub platform: String,
    /// A codec label -- see [`rvz_codec_label`].
    pub codec: String,
    pub rvz_path: PathBuf,
    pub original_iso: PathBuf,
}

#[derive(Default)]
pub struct Cases {
    pub chd: Vec<ChdCase>,
    pub rvz: Vec<RvzCase>,
}


pub fn discover_disc_archive_cases(samples_root: &Path) -> Cases {
    let mut cases = Cases::default();

    let Ok(platform_dirs) = fs::read_dir(samples_root) else { return cases };
    for platform_entry in platform_dirs.flatten() {
        let dir = platform_entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(platform) = dir.file_name().and_then(|name| name.to_str()) else { continue };
        let platform = platform.to_string();

        let Ok(files) = fs::read_dir(&dir) else { continue };
        for file_entry in files.flatten() {
            let path = file_entry.path();
            if !path.is_file() {
                continue;
            }
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();

            if ext == "chd" {
                let base_variant = path.file_stem().and_then(|s| s.to_str()).unwrap_or("chd").to_string();
                for original in originals_for_chd(&path) {
                    let cd_output_format = cd_output_format_for(&original);
                    let variant = match cd_output_format {
                        extract::CdOutputFormat::Gdi => format!("{base_variant} [gdi]"),
                        extract::CdOutputFormat::CueBin => base_variant.clone(),
                    };
                    cases.chd.push(ChdCase { platform: platform.clone(), variant, chd_path: path.clone(), original, cd_output_format });
                }
            } else if ext == "rvz" {
                if let Some(original_iso) = find_original_iso_for_rvz(&path) {
                    let codec = rvz_codec_label(&path);
                    cases.rvz.push(RvzCase { platform: platform.clone(), codec, rvz_path: path, original_iso });
                }
            }
        }
    }

    cases.chd.sort_by(|a, b| (&a.platform, &a.variant).cmp(&(&b.platform, &b.variant)));
    cases.rvz.sort_by(|a, b| (&a.platform, &a.codec).cmp(&(&b.platform, &b.codec)));
    cases
}

fn originals_for_chd(chd_path: &Path) -> Vec<OriginalRef> {
    let Some(stem) = chd_path.file_stem().and_then(|s| s.to_str()) else { return Vec::new() };
    let Some(dir) = chd_path.parent() else { return Vec::new() };

    let mut refs = Vec::new();
    let same_stem_cue = dir.join(format!("{stem}.cue"));
    if same_stem_cue.is_file() {
        refs.push(OriginalRef::Descriptor(same_stem_cue));
    }
    let same_stem_gdi = dir.join(format!("{stem}.gdi"));
    if same_stem_gdi.is_file() {
        refs.push(OriginalRef::Descriptor(same_stem_gdi));
    }
    if !refs.is_empty() {
        return refs;
    }

    let same_stem_iso = dir.join(format!("{stem}.iso"));
    if same_stem_iso.is_file() {
        return vec![OriginalRef::SingleFile(same_stem_iso)];
    }

    match (find_unique_with_extension(dir, "cue"), find_unique_with_extension(dir, "gdi"), find_unique_with_extension(dir, "iso")) {
        (Some(cue), _, _) => vec![OriginalRef::Descriptor(cue)],
        (None, Some(gdi), _) => vec![OriginalRef::Descriptor(gdi)],
        (None, None, Some(iso)) => vec![OriginalRef::SingleFile(iso)],
        (None, None, None) => Vec::new(),
    }
}

fn cd_output_format_for(original: &OriginalRef) -> extract::CdOutputFormat {
    let OriginalRef::Descriptor(path) = original else { return extract::CdOutputFormat::CueBin };
    let is_gdi = path.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("gdi")).unwrap_or(false);
    if is_gdi { extract::CdOutputFormat::Gdi } else { extract::CdOutputFormat::CueBin }
}

/// Same-stem `.iso` first (`X.rvz` -> `X.iso`), then the directory's one unambiguous
/// `.iso` -- covers codec-variant files (`bzip2.rvz`, `lzma.rvz`, ...) that all decode to
/// one shared original rather than each having their own.
fn find_original_iso_for_rvz(rvz_path: &Path) -> Option<PathBuf> {
    let stem = rvz_path.file_stem()?.to_str()?;
    let dir = rvz_path.parent()?;

    let same_stem_iso = dir.join(format!("{stem}.iso"));
    if same_stem_iso.is_file() {
        return Some(same_stem_iso);
    }

    find_unique_with_extension(dir, "iso")
}

fn find_unique_with_extension(dir: &Path, ext: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    let mut matches = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| path.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case(ext)).unwrap_or(false));

    let first = matches.next()?;
    if matches.next().is_some() {
        return None; // ambiguous -- more than one candidate, don't guess.
    }
    Some(first)
}


pub fn original_files(chd_cases: &[&ChdCase], rvz_cases: &[&RvzCase]) -> Vec<PathBuf> {
    let mut files = Vec::new();

    for case in chd_cases {
        match &case.original {
            OriginalRef::SingleFile(path) => files.push(path.clone()),
            OriginalRef::Descriptor(descriptor_path) => files.extend(descriptor::descriptor_payload_paths(descriptor_path)),
        }
    }
    for case in rvz_cases {
        files.push(case.original_iso.clone());
    }

    files
}


fn rvz_codec_label(rvz_path: &Path) -> String {
    let stem = rvz_path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
    match stem.as_str() {
        "no-comp" | "nocomp" => "uncompressed".to_string(),
        "bzip2" | "lzma" | "lzma2" => stem,
        _ => "zstd".to_string(),
    }
}
