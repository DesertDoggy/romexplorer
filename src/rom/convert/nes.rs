//! # `fcdb.bin` format (version 3)
//!
//! ```text
//! offset  size  field
//! 0       4     magic: b"FCDB"
//! 4       1     version: 3
//! 5       4     pair_count: u32 LE
//! 9       ...   pair_count pair records, each:
//!                 16    unheadered: u128 LE  -- XXH3-128 of the full headerless ROM
//!                 16    headered:   u128 LE  -- XXH3-128 of the full headered ROM
//!                 2     header_len: u16 LE    -- 16 (plain iNES) or 528 (+512-byte trainer)
//!                 ..    header: [u8; header_len]
//! ...     4     solo_headered_count: u32 LE
//! ...     ...   solo_headered_count hashes, 16 bytes each (u128 LE)
//! ...     4     solo_headerless_count: u32 LE
//! ...     ...   solo_headerless_count hashes, 16 bytes each (u128 LE)
//! ```
//!


use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use xxhash_rust::xxh3::Xxh3;

const FCDB_MAGIC: &[u8; 4] = b"FCDB";
const FCDB_VERSION: u8 = 3;

/// The shipped database, compiled in as the last-resort source for [`FcDb::load`]. Lives
/// under `app_root_ex/assets/` -- this repo's stand-in for the real app's exe root (see
/// [`FcDb::candidate_paths`]) -- not a bare top-level `assets/`, so the compiled-in copy and
/// the on-disk layout [`candidate_paths`](Self::candidate_paths) searches at runtime are the
/// same file, not two copies that can drift.
const EMBEDDED_FCDB: &[u8] = include_bytes!("../../../app_root_ex/assets/fcdb.bin");

/// One matched headered/headerless game: `header` prepended to the headerless content
/// reproduces the headered content byte-for-byte (verified when the database was built,
/// not just assumed).
#[derive(Debug, Clone)]
pub struct HeaderPair {
    pub unheadered: u128,
    pub headered: u128,
    pub header: Vec<u8>,
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NesHashMatch {
    /// A known game, in headered (iNES/UNIF) form.
    Headered,
    /// A known game, in headerless form.
    Headerless,
    /// Not found in the database (an uncatalogued game, or no database loaded at all).
    NotNes,
}

/// The loaded `fcdb.bin` database, indexed for O(1) lookup.
pub struct FcDb {
    pairs: Vec<HeaderPair>,
    by_unheadered: HashMap<u128, usize>,
    by_headered: HashMap<u128, usize>,
    solo_headered: HashSet<u128>,
    solo_headerless: HashSet<u128>,
}

/// Same override this crate's app-root-relative lookups all share -- see
/// `native_lib::EXE_ROOT_OVERRIDE_ENV`'s own doc comment (the canonical definition; this
/// used to be its own separate copy here, consolidated to avoid drift).
pub use super::super::native_lib::EXE_ROOT_OVERRIDE_ENV;

impl FcDb {
    /// Candidate paths [`load`](Self::load) tries, in order: `<exe_root>/fcdb.bin` (exe
    /// root, matching the "app root" tier of this project's native-library search order --
    /// see `native_lib::find_app_root_library`), then `<exe_root>/assets/fcdb.bin`. `exe_root`
    /// is [`EXE_ROOT_OVERRIDE_ENV`] when set, otherwise the app root `AppPaths::detect` finds
    /// (`native_lib::exe_root`) -- never the current working directory, so production behavior is the same
    /// regardless of where the app is launched from.
    pub fn candidate_paths() -> io::Result<Vec<PathBuf>> {
        // The app root, not the executable's own directory: they differ inside a macOS
        // `.app`, where the executable is in Contents/MacOS/ and assets/ in Contents/Resources/.
        let dir = crate::rom::native_lib::exe_root().ok_or_else(|| io::Error::new(ErrorKind::NotFound, "executable has no parent directory"))?;
        Ok(vec![dir.join("fcdb.bin"), dir.join("assets").join("fcdb.bin")])
    }

    /// Loads the database from the first existing path in [`candidate_paths`](Self::candidate_paths),
    /// falling back to the copy embedded in the binary, so a match never depends on where the
    /// executable runs from. Returns an error (never panics) only if the file found (or the
    /// embedded copy) is malformed.
    pub fn load() -> io::Result<Self> {
        if let Ok(candidates) = Self::candidate_paths() {
            for path in &candidates {
                if path.is_file() {
                    return Self::load_from(path);
                }
            }
        }
        Self::parse(EMBEDDED_FCDB)
    }

    /// Loads the database from an explicit path -- mainly for tests/tools that don't want
    /// to depend on the running executable's location.
    pub fn load_from(path: &Path) -> io::Result<Self> {
        let mut file = fs::File::open(path)?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        Self::parse(&buf)
    }

    fn parse(buf: &[u8]) -> io::Result<Self> {
        let bad = |msg: &str| io::Error::new(ErrorKind::InvalidData, format!("fcdb.bin: {msg}"));

        if buf.len() < 9 {
            return Err(bad("file too short for a header"));
        }
        if &buf[0..4] != FCDB_MAGIC {
            return Err(bad("bad magic (not an fcdb.bin file)"));
        }
        let version = buf[4];
        if version != FCDB_VERSION {
            return Err(bad(&format!("unsupported version {version} (expected {FCDB_VERSION})")));
        }
        let pair_count = u32::from_le_bytes(buf[5..9].try_into().unwrap()) as usize;

        let mut pairs = Vec::with_capacity(pair_count);
        let mut offset = 9usize;
        for _ in 0..pair_count {
            let record_header_len = 16 + 16 + 2;
            let record_header = buf.get(offset..offset + record_header_len).ok_or_else(|| bad("truncated pair record"))?;
            let unheadered = u128::from_le_bytes(record_header[0..16].try_into().unwrap());
            let headered = u128::from_le_bytes(record_header[16..32].try_into().unwrap());
            let header_len = u16::from_le_bytes(record_header[32..34].try_into().unwrap()) as usize;
            offset += record_header_len;

            let header = buf.get(offset..offset + header_len).ok_or_else(|| bad("truncated header bytes"))?.to_vec();
            offset += header_len;

            pairs.push(HeaderPair { unheadered, headered, header });
        }

        let read_hash_set = |buf: &[u8], offset: &mut usize| -> io::Result<HashSet<u128>> {
            let count_bytes = buf.get(*offset..*offset + 4).ok_or_else(|| bad("truncated solo-hash count"))?;
            let count = u32::from_le_bytes(count_bytes.try_into().unwrap()) as usize;
            *offset += 4;
            let mut set = HashSet::with_capacity(count);
            for _ in 0..count {
                let hash_bytes = buf.get(*offset..*offset + 16).ok_or_else(|| bad("truncated solo hash"))?;
                set.insert(u128::from_le_bytes(hash_bytes.try_into().unwrap()));
                *offset += 16;
            }
            Ok(set)
        };

        let solo_headered = read_hash_set(buf, &mut offset)?;
        let solo_headerless = read_hash_set(buf, &mut offset)?;

        let by_unheadered = pairs.iter().enumerate().map(|(i, p)| (p.unheadered, i)).collect();
        let by_headered = pairs.iter().enumerate().map(|(i, p)| (p.headered, i)).collect();
        Ok(Self { pairs, by_unheadered, by_headered, solo_headered, solo_headerless })
    }

    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty() && self.solo_headered.is_empty() && self.solo_headerless.is_empty()
    }

    pub fn hash_count(&self) -> usize {
        self.pairs.len() * 2 + self.solo_headered.len() + self.solo_headerless.len()
    }


    /// still say [`NesHashMatch::Headerless`].
    pub fn header_for_unheadered_hash(&self, hash: u128) -> Option<&[u8]> {
        self.by_unheadered.get(&hash).map(|&i| self.pairs[i].header.as_slice())
    }

    /// [`Self::header_for_unheadered_hash`].
    pub fn header_for_headered_hash(&self, hash: u128) -> Option<&[u8]> {
        self.by_headered.get(&hash).map(|&i| self.pairs[i].header.as_slice())
    }


    pub fn classify(&self, hash: u128) -> NesHashMatch {
        if self.by_headered.contains_key(&hash) || self.solo_headered.contains(&hash) {
            NesHashMatch::Headered
        } else if self.by_unheadered.contains_key(&hash) || self.solo_headerless.contains(&hash) {
            NesHashMatch::Headerless
        } else {
            NesHashMatch::NotNes
        }
    }


    pub fn contains_hash(&self, hash: u128) -> bool {
        self.classify(hash) != NesHashMatch::NotNes
    }
}

pub fn xxh3_128(bytes: &[u8]) -> u128 {
    let mut hasher = Xxh3::new();
    hasher.update(bytes);
    hasher.digest128()
}

pub fn classify(bytes: &[u8]) -> NesHashMatch {
    let Some(db) = shared_fcdb() else { return NesHashMatch::NotNes };
    db.classify(xxh3_128(bytes))
}

pub fn add_with_db(db: &FcDb, headerless_bytes: &[u8]) -> Option<Vec<u8>> {
    let header = db.header_for_unheadered_hash(xxh3_128(headerless_bytes))?;
    let mut out = Vec::with_capacity(header.len() + headerless_bytes.len());
    out.extend_from_slice(header);
    out.extend_from_slice(headerless_bytes);
    Some(out)
}

pub fn strip_with_db(db: &FcDb, headered_bytes: &[u8]) -> Option<Vec<u8>> {
    let header = db.header_for_headered_hash(xxh3_128(headered_bytes))?;
    headered_bytes.get(header.len()..).map(|slice| slice.to_vec())
}

pub fn add(headerless_bytes: &[u8]) -> Option<Vec<u8>> {
    add_with_db(shared_fcdb()?, headerless_bytes)
}

pub fn strip(headered_bytes: &[u8]) -> Option<Vec<u8>> {
    strip_with_db(shared_fcdb()?, headered_bytes)
}

static FCDB: OnceLock<Option<FcDb>> = OnceLock::new();

pub fn shared_fcdb() -> Option<&'static FcDb> {
    FCDB.get_or_init(|| FcDb::load().ok()).as_ref()
}

/// What [`relayer`] made of a dump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relayered {
    /// Whether the input had a header, by the database.
    pub was_headered: bool,
    /// The output: the input itself when it was already in the layout asked for.
    pub data: Vec<u8>,
    pub changed: bool,
}

/// The iNES header's own length, trainer included -- for a known headered dump the database
/// keeps no header for.
pub fn ines_header_len(bytes: &[u8]) -> Option<usize> {
    if bytes.get(..4)? != b"NES\x1A" {
        return None;
    }
    Some(16 + if bytes.get(6)? & 0x04 != 0 { 512 } else { 0 })
}

/// Puts a whole dump into the layout asked for, headered or not. Whether it has a header now
/// is the database's answer, never the magic's: a dump whose hash is not there is an error that
/// names the hash, so it can be looked up or added.
pub fn relayer(db: &FcDb, bytes: Vec<u8>, want_header: bool) -> Result<Relayered, String> {
    let hash = xxh3_128(&bytes);
    let tag = format!("xxh3-128 {hash:032x}");
    let done = |was_headered, data, changed| Ok(Relayered { was_headered, data, changed });
    match db.classify(hash) {
        NesHashMatch::NotNes => Err(format!("not in the NES header database (fcdb.bin), so whether it has a header can't be told: {tag}")),
        NesHashMatch::Headered if want_header => done(true, bytes, false),
        NesHashMatch::Headerless if !want_header => done(false, bytes, false),
        NesHashMatch::Headered => {
            let len = db
                .header_for_headered_hash(hash)
                .map(<[u8]>::len)
                .or_else(|| ines_header_len(&bytes))
                .ok_or_else(|| format!("a known headered dump, but not an iNES one, so its header can't be removed: {tag}"))?;
            let rom = bytes.get(len..).filter(|rom| !rom.is_empty()).ok_or_else(|| format!("shorter than its own header: {tag}"))?;
            done(true, rom.to_vec(), true)
        }
        NesHashMatch::Headerless => {
            let header = db.header_for_unheadered_hash(hash).ok_or_else(|| format!("a known headerless dump, but the database has no header for it: {tag}"))?;
            let mut data = Vec::with_capacity(header.len() + bytes.len());
            data.extend_from_slice(header);
            data.extend_from_slice(&bytes);
            done(false, data, true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A database of one game, in `fcdb.bin`'s own format, so both directions run without a
    /// real dump.
    pub(crate) fn one_game_db(header: &[u8], rom: &[u8]) -> FcDb {
        let mut headered = header.to_vec();
        headered.extend_from_slice(rom);
        let mut db = b"FCDB\x03".to_vec();
        db.extend_from_slice(&1u32.to_le_bytes());
        db.extend_from_slice(&xxh3_128(rom).to_le_bytes());
        db.extend_from_slice(&xxh3_128(&headered).to_le_bytes());
        db.extend_from_slice(&(header.len() as u16).to_le_bytes());
        db.extend_from_slice(header);
        db.extend_from_slice(&0u32.to_le_bytes());
        db.extend_from_slice(&0u32.to_le_bytes());
        FcDb::parse(&db).unwrap()
    }

    fn game() -> (Vec<u8>, Vec<u8>) {
        let rom: Vec<u8> = (0..(40 * 1024u32)).map(|i| (i * 31 % 253) as u8).collect();
        let mut header = b"NES\x1A".to_vec();
        header.extend_from_slice(&[2, 1, 0x10, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        (header, rom)
    }

    #[test]
    fn header_comes_from_the_database_both_ways() {
        let (header, rom) = game();
        let db = one_game_db(&header, &rom);
        let added = relayer(&db, rom.clone(), true).unwrap();
        assert_eq!((&added.data[..16], &added.data[16..], added.was_headered, added.changed), (&header[..], &rom[..], false, true));
        let stripped = relayer(&db, added.data.clone(), false).unwrap();
        assert_eq!((stripped.data, stripped.was_headered, stripped.changed), (rom.clone(), true, true));
        assert!(!relayer(&db, rom, false).unwrap().changed, "already headerless");
        assert!(!relayer(&db, added.data, true).unwrap().changed, "already headered");
    }

    #[test]
    fn a_hash_not_in_the_database_is_an_error_naming_it() {
        let (header, rom) = game();
        let db = one_game_db(&header, &rom);
        // Magic and all, but not this game: the database decides, not the magic.
        let mut other = header.clone();
        other.extend_from_slice(&[0x5A; 1024]);
        let err = relayer(&db, other.clone(), false).unwrap_err();
        assert!(err.contains(&format!("{:032x}", xxh3_128(&other))), "{err}");
    }

    #[test]
    fn ines_header_length_counts_the_trainer() {
        let mut head = b"NES\x1A".to_vec();
        head.extend_from_slice(&[2, 1, 0, 0]);
        assert_eq!(ines_header_len(&head), Some(16));
        head[6] = 0x04;
        assert_eq!(ines_header_len(&head), Some(528));
        assert_eq!(ines_header_len(b"UNIF"), None);
    }
}
