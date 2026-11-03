//! Test input configuration, read from `tests/results/test_inputs.tsv`.
//!
//! Tab-separated, one record per line, first field = record kind; blank lines and lines
//! starting with `#` are ignored:
//!
//! ```text
//! root      <samples root, absolute or relative to the crate root>
//! dats      <reference DAT root (test_database); default `samples/dats`>
//! disc     <dir>  <Platform>          CHD/RVZ sample directory -> expected platform
//! ext       <dir>  <ext>  <Platform>   plain files in <dir> with <ext> -> expected platform
//! file      <dir>  <name> <Platform>   exact file name in <dir> (checked before `ext`)
//! dirgames  <dir>  <Platform>          every subdirectory of <dir> is one directory game
//! pkgdir    <dir>  <Platform>          <dir>/<sub>/ each hold one `.pkg`
//! exclude   <dir>                      directory skipped by metadata detection
//! ```
//!
//! `<Platform>` is a `romexplorer::common::models::Platform` variant name. Without the file
//! (or without a `root` record) the samples root doesn't resolve and sample-based tests skip.
//!
//! `root`/`dats` may repeat, so one file can list e.g. a Linux and a Windows location
//! (`/home/me/samples/objects`, `D:\samples\objects`): the first that exists on this machine
//! wins. Either separator works in any path field.

use std::path::PathBuf;
use std::sync::OnceLock;

use romexplorer::common::models::Platform;

const INPUTS_FILE: &str = "tests/results/test_inputs.tsv";

fn records() -> &'static [Vec<String>] {
    static RECORDS: OnceLock<Vec<Vec<String>>> = OnceLock::new();
    RECORDS.get_or_init(|| {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(INPUTS_FILE);
        let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| line.split('\t').map(|f| f.trim().to_string()).filter(|f| !f.is_empty()).collect())
            .collect()
    })
}

/// Every record of `kind`, without the kind field itself.
pub fn rows(kind: &str) -> impl Iterator<Item = &'static [String]> {
    let kind = kind.to_string();
    records().iter().filter(move |r| r.first() == Some(&kind)).map(|r| &r[1..])
}

/// The samples root, or an empty path (which never exists) when not configured.
pub fn samples_root() -> PathBuf {
    first_existing("root").unwrap_or_default()
}

/// The reference DAT root (`<dats>/<source>/...`), falling back to `samples/dats`.
#[allow(dead_code)]
pub fn dats_root() -> PathBuf {
    first_existing("dats").unwrap_or_else(|| resolve("samples/dats"))
}

/// The first `kind` record whose path exists here, else the first one listed (so a missing
/// location still shows up in skip messages), else `None`.
fn first_existing(kind: &str) -> Option<PathBuf> {
    let candidates: Vec<PathBuf> = rows(kind).filter_map(|r| r.first()).map(|p| resolve(p)).collect();
    candidates.iter().find(|p| p.exists()).or(candidates.first()).cloned()
}

/// A path field: `\` and `/` both accepted; relative paths are against the crate root. A
/// path that's only absolute on another OS (`D:\...` on Linux, `/home/...` on Windows)
/// simply resolves somewhere that doesn't exist.
fn resolve(field: &str) -> PathBuf {
    let path = PathBuf::from(field.replace(if cfg!(windows) { '/' } else { '\\' }, std::path::MAIN_SEPARATOR_STR));
    if path.is_absolute() {
        path
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path)
    }
}

/// Parses a `Platform` variant name (its `Debug` form).
pub fn platform(name: &str) -> Option<Platform> {
    static BY_NAME: OnceLock<std::collections::HashMap<String, Platform>> = OnceLock::new();
    BY_NAME
        .get_or_init(|| (0..=u16::MAX).filter_map(Platform::from_igdb_id).map(|p| (format!("{p:?}"), p)).collect())
        .get(name)
        .copied()
}
