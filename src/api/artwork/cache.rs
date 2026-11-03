//! Converted pictures, kept so a second export -- to another emulator, or the same one again --
//! only copies. In `<media dir>/_export_cache/<sha1[..2]>/`, named by what they are made from
//! and how: `<source sha1>-<format>-w<width>.<ext>`. Content-addressed, so the same box linked
//! to many releases is converted once, and a replaced original simply stops being asked for and
//! ages out. Like the converted-audio cache (`api::audio_convert`): written to a `.part` file
//! and renamed, the least recently used removed first once `media_export_cache_max_mb` is
//! passed (0 = no cap), a hit counting as a use.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::convert::Codec;

/// Default cap, MiB.
pub const DEFAULT_MAX_MB: u64 = 1024;

pub fn dir(media_dir: &Path) -> PathBuf {
    media_dir.join("_export_cache")
}

/// Where the conversion of `source_sha1` to `to` at `width` lives.
pub fn path_for(root: &Path, source_sha1: &str, to: Codec, width: Option<u32>) -> PathBuf {
    let fan = source_sha1.get(..2).unwrap_or("00");
    root.join(fan).join(format!("{source_sha1}-{}-w{}.{}", to.name(), width.unwrap_or(0), to.ext()))
}

/// Removes the main server's originals an older build kept here (`<sha1>-original`): the
/// cache holds conversions only, and a client fetches an original when it needs one.
pub fn drop_originals(root: &Path) {
    let Ok(fans) = std::fs::read_dir(root) else { return };
    for fan in fans.flatten() {
        let Ok(entries) = std::fs::read_dir(fan.path()) else { continue };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().ends_with("-original") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// A cached conversion, its use recorded.
pub fn hit(path: &Path) -> Option<PathBuf> {
    if !path.is_file() {
        return None;
    }
    if let Ok(file) = std::fs::File::options().append(true).open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
    Some(path.to_path_buf())
}

/// Stores `bytes` at `path` (via a `.part` file), then trims the cache to `max_mb`.
pub fn put(root: &Path, path: &Path, bytes: &[u8], max_mb: u64) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let part = path.with_extension("part");
    std::fs::write(&part, bytes)?;
    std::fs::rename(&part, path)?;
    if max_mb > 0 {
        evict(root, max_mb * 1024 * 1024);
    }
    Ok(())
}

/// Removes the least recently used files until the cache is within `max_bytes`.
fn evict(root: &Path, max_bytes: u64) {
    let mut files: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    let Ok(fans) = std::fs::read_dir(root) else { return };
    for fan in fans.flatten() {
        let Ok(entries) = std::fs::read_dir(fan.path()) else { continue };
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    files.push((meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len(), entry.path()));
                }
            }
        }
    }
    let mut total: u64 = files.iter().map(|(_, len, _)| len).sum();
    files.sort_by_key(|(at, _, _)| *at);
    for (_, len, path) in files {
        if total <= max_bytes {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_say_what_a_file_is_made_from() {
        let p = path_for(Path::new("/c"), "3f2a00", Codec::Png, Some(512));
        assert_eq!(p, Path::new("/c").join("3f").join("3f2a00-png-w512.png"));
        assert_eq!(path_for(Path::new("/c"), "ab", Codec::Jpg, None).file_name().unwrap(), "ab-jpg-w0.jpg");
    }

    #[test]
    fn the_oldest_go_first_once_past_the_cap() {
        let root = std::env::temp_dir().join(format!("export-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let old = path_for(&root, "aa11", Codec::Png, None);
        let new = path_for(&root, "bb22", Codec::Png, None);
        put(&root, &old, &vec![0u8; 700 * 1024], 0).unwrap();
        let _ = std::fs::File::options().append(true).open(&old).unwrap().set_modified(SystemTime::UNIX_EPOCH);
        put(&root, &new, &vec![0u8; 700 * 1024], 1).unwrap();
        assert!(!old.exists(), "the older one went");
        assert!(new.exists());
        assert_eq!(hit(&new), Some(new.clone()));
        let _ = std::fs::remove_dir_all(&root);
    }
}
