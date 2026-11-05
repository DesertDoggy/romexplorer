//! Converted ROM tracks kept in memory (`audio_ram_cache_mb`), in front of the converted-audio
//! cache on disk.
//!
//! A ROM track is decoded on its first play. With this cache on, the result stays in RAM and
//! is served from there; when the cache is full, the least recently played track moves out to
//! the disk cache (the tier 1 cache folder when one is set) instead of being thrown away, so
//! it is read from disk next time rather than decoded again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub use bytes::Bytes;

/// The size in MiB when the setting is unset. A chip-music track in FLAC is 3-10 MB.
pub const DEFAULT_MB: u64 = 256;

struct Entry {
    bytes: Bytes,
    /// Where the track goes when it is pushed out: the disk cache file it would have been.
    spill_to: PathBuf,
    used: u64,
}

#[derive(Default)]
struct Ram {
    entries: HashMap<String, Entry>,
    total: u64,
    clock: u64,
}

static RAM: Mutex<Option<Ram>> = Mutex::new(None);

fn with<T>(f: impl FnOnce(&mut Ram) -> T) -> T {
    let mut guard = RAM.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(Ram::default))
}

/// The track stored under `name`, marked as just played.
pub fn get(name: &str) -> Option<Bytes> {
    with(|ram| {
        ram.clock += 1;
        let clock = ram.clock;
        ram.entries.get_mut(name).map(|e| {
            e.used = clock;
            e.bytes.clone()
        })
    })
}

/// Keeps `bytes` under `name` in a cache of `cap` bytes. Returns what no longer fits, least
/// recently played first, with where each one should be written on disk. A track bigger than
/// a quarter of the cache is not kept: it comes straight back to be written.
pub fn put(name: &str, bytes: Bytes, spill_to: PathBuf, cap: u64) -> Vec<(PathBuf, Bytes)> {
    if (bytes.len() as u64).saturating_mul(4) > cap {
        return vec![(spill_to, bytes)];
    }
    with(|ram| {
        ram.clock += 1;
        let entry = Entry { bytes, spill_to, used: ram.clock };
        ram.total += entry.bytes.len() as u64;
        if let Some(old) = ram.entries.insert(name.to_string(), entry) {
            ram.total -= old.bytes.len() as u64;
        }
        shrink(ram, cap)
    })
}

/// Applies a new size: everything over `cap` comes back to be written to disk.
pub fn resize(cap: u64) -> Vec<(PathBuf, Bytes)> {
    with(|ram| shrink(ram, cap))
}

fn shrink(ram: &mut Ram, cap: u64) -> Vec<(PathBuf, Bytes)> {
    let mut out = Vec::new();
    while ram.total > cap {
        let Some(oldest) = ram.entries.iter().min_by_key(|(_, e)| e.used).map(|(k, _)| k.clone()) else { break };
        if let Some(e) = ram.entries.remove(&oldest) {
            ram.total -= e.bytes.len() as u64;
            out.push((e.spill_to, e.bytes));
        }
    }
    out
}

/// Writes tracks pushed out of RAM into the disk cache, each beside its final name and then
/// renamed, so the disk cache only ever holds whole files. A track already there is left.
pub fn spill(tracks: &[(PathBuf, Bytes)]) {
    for (path, bytes) in tracks {
        if path.is_file() {
            continue;
        }
        let Some(dir) = path.parent() else { continue };
        if std::fs::create_dir_all(dir).is_err() {
            continue;
        }
        let partial = part_path(path);
        if std::fs::write(&partial, bytes).and_then(|()| std::fs::rename(&partial, path)).is_err() {
            let _ = std::fs::remove_file(&partial);
        }
    }
}

fn part_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_least_recently_played_track_moves_to_disk() {
        let cap = 100;
        // Unique names: the cache is process-wide and other tests may use it.
        assert!(put("lru-a", Bytes::from(vec![0u8; 20]), "a".into(), cap).is_empty());
        assert!(put("lru-b", Bytes::from(vec![0u8; 20]), "b".into(), cap).is_empty());
        assert!(get("lru-a").is_some());
        let out = resize(30);
        assert_eq!(out.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(), vec![PathBuf::from("b")]);
        assert!(get("lru-b").is_none() && get("lru-a").is_some());
        resize(0);
    }

    #[test]
    fn a_track_too_big_for_the_cache_goes_straight_to_disk() {
        let out = put("big", Bytes::from(vec![0u8; 50]), "big".into(), 100);
        assert_eq!(out.len(), 1);
        assert!(get("big").is_none());
    }
}
