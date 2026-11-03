//! Content hashing for sample comparison.
//!


use std::path::Path;

use romexplorer::rom::hash::{hash_file_other, HashFlags};

/// SHA-256 of `path`'s full content.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let report = hash_file_other(path, HashFlags::SHA256, None, |_, _| {})?;
    report.sha256.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "sha256 not computed"))
}

/// XXH3-128 of `path`'s full content, as the hex string [`hash_file_other`] reports.
pub fn xxh3_file(path: &Path) -> std::io::Result<String> {
    let report = hash_file_other(path, HashFlags::XXH3_128, None, |_, _| {})?;
    report.xxh3_128.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "xxh3_128 not computed"))
}
