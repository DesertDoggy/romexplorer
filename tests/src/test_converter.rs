//! Unit tests for `rom::convert::nes` (the `FcDb` binary format loader and NES
//! header add/strip functions) against small synthetic fixtures -- no dependency on a real
//! `assets/fcdb.bin` or real ROM data (see `examples/gen_fchash.rs`/`examples/verify_fchash.rs`,
//! not committed, for end-to-end verification against a real local collection).

use std::fs;
use std::path::{Path, PathBuf};

use romexplorer::rom::convert::nes::{self, FcDb, NesHashMatch};

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("romexplorer_test_converter_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("failed to create temp dir");
        Self(path)
    }

    fn write(&self, relative: &str, contents: &[u8]) -> PathBuf {
        let path = self.0.join(relative);
        fs::write(&path, contents).expect("failed to write temp file");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Hand-builds a well-formed `fcdb.bin` (version 3) buffer, matching the format
/// `convert/nes.rs`'s own doc comment documents: `pairs` become convertible entries (with
/// header bytes), `solo_headered`/`solo_headerless` become known-but-unconvertible entries
/// for one form only.
fn build_fcdb(pairs: &[(u128, u128, &[u8])], solo_headered: &[u128], solo_headerless: &[u128]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"FCDB");
    buf.push(3); // version
    buf.extend_from_slice(&(pairs.len() as u32).to_le_bytes());
    for &(unheadered, headered, header) in pairs {
        buf.extend_from_slice(&unheadered.to_le_bytes());
        buf.extend_from_slice(&headered.to_le_bytes());
        buf.extend_from_slice(&(header.len() as u16).to_le_bytes());
        buf.extend_from_slice(header);
    }
    buf.extend_from_slice(&(solo_headered.len() as u32).to_le_bytes());
    for &hash in solo_headered {
        buf.extend_from_slice(&hash.to_le_bytes());
    }
    buf.extend_from_slice(&(solo_headerless.len() as u32).to_le_bytes());
    for &hash in solo_headerless {
        buf.extend_from_slice(&hash.to_le_bytes());
    }
    buf
}

/// Convenience for the common single-pair, no-solo-hashes case.
fn build_single_entry_fcdb(unheadered: u128, headered: u128, header: &[u8]) -> Vec<u8> {
    build_fcdb(&[(unheadered, headered, header)], &[], &[])
}

#[test]
fn loads_a_well_formed_single_entry_database() {
    let dir = TempDir::new("well_formed");
    let header = [0x4E, 0x45, 0x53, 0x1A, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
    let unheadered_hash = 0x1111_2222_3333_4444_5555_6666_7777_8888u128;
    let headered_hash = 0x9999_AAAA_BBBB_CCCC_DDDD_EEEE_FFFF_0000u128;
    let buf = build_single_entry_fcdb(unheadered_hash, headered_hash, &header);
    let path = dir.write("fcdb.bin", &buf);

    let db = FcDb::load_from(&path).expect("well-formed database should load");
    assert_eq!(db.len(), 1);
    assert!(!db.is_empty());
    assert_eq!(db.hash_count(), 2); // one pair = two distinct hashes
    assert_eq!(db.header_for_unheadered_hash(unheadered_hash), Some(header.as_slice()));
    assert_eq!(db.header_for_headered_hash(headered_hash), Some(header.as_slice()));
    assert_eq!(db.header_for_unheadered_hash(0), None);
    assert_eq!(db.header_for_headered_hash(0), None);
    assert!(db.contains_hash(unheadered_hash));
    assert!(db.contains_hash(headered_hash));
    assert!(!db.contains_hash(0));
    assert_eq!(db.classify(unheadered_hash), NesHashMatch::Headerless);
    assert_eq!(db.classify(headered_hash), NesHashMatch::Headered);
    assert_eq!(db.classify(0), NesHashMatch::NotNes);
}

#[test]
fn solo_hashes_are_known_but_not_convertible() {
    let dir = TempDir::new("solo_hashes");
    let pair_header = [0x4E, 0x45, 0x53, 0x1A, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let pair_unheadered = 0x1111u128;
    let pair_headered = 0x2222u128;
    let solo_headered_hash = 0x3333u128;
    let solo_headerless_hash = 0x4444u128;
    let buf = build_fcdb(&[(pair_unheadered, pair_headered, &pair_header)], &[solo_headered_hash], &[solo_headerless_hash]);
    let path = dir.write("fcdb.bin", &buf);

    let db = FcDb::load_from(&path).expect("load");
    assert_eq!(db.len(), 1, "solo hashes shouldn't count as pairs");
    assert_eq!(db.hash_count(), 4); // 2 from the pair + 1 solo headered + 1 solo headerless
    assert!(!db.is_empty());

    // Solo hashes are known, and correctly classified by which form they actually are...
    assert!(db.contains_hash(solo_headered_hash));
    assert_eq!(db.classify(solo_headered_hash), NesHashMatch::Headered);
    assert!(db.contains_hash(solo_headerless_hash));
    assert_eq!(db.classify(solo_headerless_hash), NesHashMatch::Headerless);
    // ...but have no header to offer, unlike a real pair.
    assert_eq!(db.header_for_unheadered_hash(solo_headerless_hash), None);
    assert_eq!(db.header_for_headered_hash(solo_headered_hash), None);

    // The pair still works normally alongside the solo hashes.
    assert!(db.contains_hash(pair_unheadered));
    assert_eq!(db.header_for_unheadered_hash(pair_unheadered), Some(pair_header.as_slice()));

    assert!(!db.contains_hash(0x9999));
    assert_eq!(db.classify(0x9999), NesHashMatch::NotNes);
}

#[test]
fn an_empty_database_is_well_formed() {
    let dir = TempDir::new("empty");
    let buf = build_fcdb(&[], &[], &[]);
    let path = dir.write("fcdb.bin", &buf);

    let db = FcDb::load_from(&path).expect("an empty but well-formed database should still load");
    assert!(db.is_empty());
    assert_eq!(db.hash_count(), 0);
    assert!(!db.contains_hash(0));
}

#[test]
fn rejects_bad_magic_without_panicking() {
    let dir = TempDir::new("bad_magic");
    let mut buf = build_single_entry_fcdb(1, 2, &[0; 16]);
    buf[0..4].copy_from_slice(b"NOPE");
    let path = dir.write("fcdb.bin", &buf);

    let result = FcDb::load_from(&path);
    assert!(result.is_err(), "bad magic should be rejected, not accepted or panicked on");
}

#[test]
fn rejects_unsupported_version() {
    let dir = TempDir::new("bad_version");
    let mut buf = build_single_entry_fcdb(1, 2, &[0; 16]);
    buf[4] = 99;
    let path = dir.write("fcdb.bin", &buf);

    assert!(FcDb::load_from(&path).is_err());
}

#[test]
fn rejects_truncated_file_without_panicking() {
    let dir = TempDir::new("truncated");
    let buf = build_single_entry_fcdb(1, 2, &[0xAB; 16]);
    // Cut off partway through the header bytes -- a naive slice-index parser would panic
    // here instead of returning an error.
    let truncated = &buf[..buf.len() - 5];
    let path = dir.write("fcdb.bin", truncated);

    let result = FcDb::load_from(&path);
    assert!(result.is_err(), "truncated data should be a clean error, not a panic");
}

#[test]
fn missing_file_is_a_clean_error() {
    let result = FcDb::load_from(Path::new("this/path/does/not/exist/fcdb.bin"));
    assert!(result.is_err());
}

#[test]
fn nes_add_and_strip_header_round_trip() {
    let dir = TempDir::new("round_trip");
    let headerless: Vec<u8> = (0..0x2000u32).map(|i| (i % 251) as u8).collect();
    let header = [0x4E, 0x45, 0x53, 0x1A, 0x02, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
    let mut headered = header.to_vec();
    headered.extend_from_slice(&headerless);

    let unheadered_hash = nes::xxh3_128(&headerless);
    let headered_hash = nes::xxh3_128(&headered);
    let buf = build_single_entry_fcdb(unheadered_hash, headered_hash, &header);
    let path = dir.write("fcdb.bin", &buf);
    let db = FcDb::load_from(&path).expect("load");

    // `_with_db` variants: explicit database, no dependency on the shared process-wide
    // cache/real `fcdb.bin` -- what the synthetic-fixture unit tests use.
    let reconstructed = nes::add_with_db(&db, &headerless).expect("known headerless ROM should add a header");
    assert_eq!(reconstructed, headered);

    let stripped = nes::strip_with_db(&db, &headered).expect("known headered ROM should strip its header");
    assert_eq!(stripped, headerless);

    // An unrecognized ROM (not in the database) round-trips to None, not a wrong guess.
    let unknown = vec![0xFF; 100];
    assert_eq!(nes::add_with_db(&db, &unknown), None);
    assert_eq!(nes::strip_with_db(&db, &unknown), None);
}

#[test]
fn candidate_paths_check_exe_root_before_assets() {
    let candidates = FcDb::candidate_paths().expect("current_exe should resolve in a test binary");
    assert_eq!(candidates.len(), 2);
    assert!(candidates[0].ends_with("fcdb.bin") && !candidates[0].to_string_lossy().contains("assets"), "first candidate should be exe root: {}", candidates[0].display());
    assert!(
        candidates[1].ends_with("assets/fcdb.bin") || candidates[1].ends_with("assets\\fcdb.bin"),
        "second candidate should be the assets/ subfolder: {}",
        candidates[1].display()
    );
}

/// `app_root_ex/` (repo root) stands in for the real app's exe root: `app_root_ex/assets/
/// fcdb.bin` is the same real, checked-in database `nes.rs` compiles in via `include_bytes!`
/// (see that file's `EMBEDDED_FCDB` doc comment), not a synthetic fixture. This test and
/// `candidate_paths_check_exe_root_before_assets` both read `nes::EXE_ROOT_OVERRIDE_ENV` --
/// combined into one #[test] rather than two so a set/unset race between them can't happen
/// under Rust's default parallel test execution.
#[test]
fn exe_root_override_resolves_against_app_root_ex() {
    let app_root_ex = Path::new(env!("CARGO_MANIFEST_DIR")).join("app_root_ex");
    assert!(app_root_ex.join("assets").join("fcdb.bin").is_file(), "app_root_ex/assets/fcdb.bin should exist -- {}", app_root_ex.display());

    // Safety: no other test reads/writes this process's environment while this one runs
    // (the sibling candidate_paths test above never sets it, and nothing else in this file
    // touches env vars), so there's no cross-thread race despite tests running in parallel.
    unsafe {
        std::env::set_var(nes::EXE_ROOT_OVERRIDE_ENV, &app_root_ex);
    }
    let result = (|| {
        let candidates = FcDb::candidate_paths().expect("override should resolve without touching current_exe");
        assert_eq!(candidates, vec![app_root_ex.join("fcdb.bin"), app_root_ex.join("assets").join("fcdb.bin")]);

        // load(), not load_from(): exercises the same discovery FcDb::load uses in the real
        // app, against the real on-disk file, not a synthetic buffer via load_from.
        FcDb::load().expect("should load the real app_root_ex/assets/fcdb.bin")
    })();
    unsafe {
        std::env::remove_var(nes::EXE_ROOT_OVERRIDE_ENV);
    }
    let _db = result;
}
