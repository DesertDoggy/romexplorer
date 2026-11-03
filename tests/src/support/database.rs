
//! The extracted output is always re-hashed fresh every run -- that's the thing being
//! tested, so it is never the extracted side that gets cached, only the original it's
//! compared against.
//!
//! Stored as plain tab-separated text rather than JSON: the data is a flat list of
//! (path, size, mtime, hash) records, and this project has no JSON dependency to justify
//! pulling in for one small test-only cache file.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

struct Entry {
    size_bytes: u64,
    mtime_secs: u64,
    sha256: String,
}

pub struct Database {
    path: PathBuf,
    entries: HashMap<String, Entry>,
    dirty: bool,
}

impl Database {
    /// Opens (or, if absent, prepares to create) `tests/database/<name>.tsv`.
    pub fn open(name: &str) -> Self {
        let path = database_dir().join(format!("{name}.tsv"));
        let entries = load(&path);
        Self { path, entries, dirty: false }
    }

    /// Returns `file`'s SHA-256, from the cache if `file`'s size and modification time
    /// still match what's recorded, freshly computed (and cached) otherwise.
    pub fn hash_cached(&mut self, samples_root: &Path, file: &Path) -> std::io::Result<String> {
        let key = relative_key(samples_root, file);
        let metadata = fs::metadata(file)?;
        let size_bytes = metadata.len();
        let mtime_secs =
            metadata.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);

        if let Some(entry) = self.entries.get(&key) {
            if entry.size_bytes == size_bytes && entry.mtime_secs == mtime_secs {
                return Ok(entry.sha256.clone());
            }
        }

        let sha256 = super::hashing::sha256_file(file)?;
        self.entries.insert(key, Entry { size_bytes, mtime_secs, sha256: sha256.clone() });
        self.dirty = true;
        Ok(sha256)
    }

    /// Hashes every file in `files` -- from cache where still fresh, freshly (and cached)
    /// otherwise -- before the caller does anything else with them. Call this once, up
    /// front, with every original reference file a run will need (see
    /// `super::discover::original_files`), so that every later [`Self::hash_cached`] call
    /// during the actual extraction/comparison loop is a pure cache lookup: hashing a
    /// large original disc image is real work, and doing it once per run instead of
    /// interleaved with per-case extraction is the entire point of this cache existing.
    /// A file that fails to hash (missing, unreadable) is silently skipped here --
    /// [`Self::hash_cached`] will surface that same failure properly when the case that
    /// actually needs it runs.
    pub fn warm(&mut self, samples_root: &Path, files: &[PathBuf]) {
        for file in files {
            let _ = self.hash_cached(samples_root, file);
        }
    }

    /// Persists any new/changed entries. Safe to call even when nothing changed (a no-op)
    /// or when nothing was ever inserted (no file is created).
    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let mut keys: Vec<&String> = self.entries.keys().collect();
        keys.sort();
        let mut out = String::new();
        for key in keys {
            let entry = &self.entries[key];
            out.push_str(&format!("{key}\t{}\t{}\t{}\n", entry.size_bytes, entry.mtime_secs, entry.sha256));
        }

        let _ = fs::write(&self.path, out);
        self.dirty = false;
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        self.save();
    }
}

fn database_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/database")
}

fn relative_key(samples_root: &Path, file: &Path) -> String {
    file.strip_prefix(samples_root).unwrap_or(file).to_string_lossy().replace('\\', "/")
}

fn load(path: &Path) -> HashMap<String, Entry> {
    let mut map = HashMap::new();
    let Ok(text) = fs::read_to_string(path) else { return map };

    for line in text.lines() {
        let mut parts = line.splitn(4, '\t');
        let (Some(key), Some(size), Some(mtime), Some(sha256)) = (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let (Ok(size_bytes), Ok(mtime_secs)) = (size.parse(), mtime.parse()) else { continue };
        map.insert(key.to_string(), Entry { size_bytes, mtime_secs, sha256: sha256.to_string() });
    }

    map
}
