use std::io::Write;
use std::path::{Path, PathBuf};

use romexplorer::rom::hash::{
    self, fallback_order, hash_file_other, rahash_file, recommended_buf_size, FileSizeClass, HashAlgorithms, HashDigest, HashFlags, HashOptions,
    HashReport, RahashError, RahashOutcome, RamClass, StorageKind, MAX_BUF_SIZE, MIN_BUF_SIZE,
};

const FLAGS: HashFlags = HashFlags::CRC32
    .union(HashFlags::MD5)
    .union(HashFlags::SHA1)
    .union(HashFlags::SHA256)
    .union(HashFlags::XXH3_128);

// RC_CONSOLE_GAMEBOY from rcheevos/include/rc_consoles.h. Game Boy routes through
// rc_hash_whole_file (see rcheevos/src/rhash/hash.c), a plain MD5 of the raw content
// with no header skip, so its expected hash is just MD5(content).
const RC_CONSOLE_GAMEBOY: u32 = 4;

struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str, contents: &[u8]) -> Self {
        let path = std::env::temp_dir().join(format!("romexplorer_test_hasher_{}_{}", std::process::id(), name));
        let mut file = std::fs::File::create(&path).expect("failed to create temp file");
        file.write_all(contents).expect("failed to write temp file");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn overwrite(&self, contents: &[u8]) {
        let mut file = std::fs::File::create(&self.0).expect("failed to overwrite temp file");
        file.write_all(contents).expect("failed to write temp file");
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// `rahash_file` needs the RAHasher native library built (see build_rahasher.sh); skip
/// rather than fail when it isn't present on this machine.
macro_rules! skip_if_rahash_unavailable {
    ($result:expr) => {
        match $result {
            Err(RahashError::Unavailable(message)) => {
                eprintln!("skipping: RAHasher native library not available on this machine ({message})");
                return;
            }
            other => other,
        }
    };
}

#[test]
fn known_vectors_for_abc_fed_as_single_chunk() {
    let digest = HashDigest::new(FLAGS);
    digest.update(b"abc").expect("update should succeed");
    let report = digest.finish();

    assert_eq!(report.crc32.as_deref(), Some("352441c2"));
    assert_eq!(report.md5.as_deref(), Some("900150983cd24fb0d6963f7d28e17f72"));
    assert_eq!(report.sha1.as_deref(), Some("a9993e364706816aba3e25717850c26c9cd0d89d"));
    assert_eq!(
        report.sha256.as_deref(),
        Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    );
}

#[test]
fn known_vectors_for_abc_fed_across_multiple_chunks() {
    // Exercises the "caller keeps the instance and feeds chunks repeatedly" pattern:
    // three separate update() calls should hash identically to one call with the whole slice.
    let digest = HashDigest::new(FLAGS);
    digest.update(b"a").expect("update should succeed");
    digest.update(b"b").expect("update should succeed");
    digest.update(b"c").expect("update should succeed");
    let report = digest.finish();

    assert_eq!(report.crc32.as_deref(), Some("352441c2"));
    assert_eq!(report.md5.as_deref(), Some("900150983cd24fb0d6963f7d28e17f72"));
    assert_eq!(report.sha1.as_deref(), Some("a9993e364706816aba3e25717850c26c9cd0d89d"));
    assert_eq!(
        report.sha256.as_deref(),
        Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    );
}

#[test]
fn chunked_and_unchunked_xxh3_128_agree() {
    let data = b"The quick brown fox jumps over the lazy dog".repeat(37);

    let whole = HashDigest::new(HashFlags::XXH3_128);
    whole.update(&data).expect("update should succeed");
    let whole_report = whole.finish();

    let chunked = HashDigest::new(HashFlags::XXH3_128);
    for chunk in data.chunks(7) {
        chunked.update(chunk).expect("update should succeed");
    }
    let chunked_report = chunked.finish();

    assert!(whole_report.xxh3_128.is_some());
    assert_eq!(whole_report.xxh3_128, chunked_report.xxh3_128);
}

#[test]
fn empty_input_produces_hashes_of_empty_string() {
    let digest = HashDigest::new(FLAGS);
    let report = digest.finish();

    // CRC32 of empty input is 0; MD5/SHA1/SHA256 of empty input are well-known constants.
    assert_eq!(report.crc32.as_deref(), Some("00000000"));
    assert_eq!(report.md5.as_deref(), Some("d41d8cd98f00b204e9800998ecf8427e"));
    assert_eq!(report.sha1.as_deref(), Some("da39a3ee5e6b4b0d3255bfef95601890afd80709"));
    assert_eq!(
        report.sha256.as_deref(),
        Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
    );
}

#[test]
fn hash_file_other_matches_known_vectors_and_reports_progress() {
    let file = TempFile::new("hash_file_other", b"abc");

    let mut last_processed = 0u64;
    let mut last_total = 0u64;
    let mut calls = 0u32;

    let report = hash_file_other(file.path(), FLAGS, Some(1), |processed, total| {
        calls += 1;
        last_processed = processed;
        last_total = total;
    })
    .expect("hash_file_other should succeed");

    assert_eq!(report.crc32.as_deref(), Some("352441c2"));
    assert_eq!(report.md5.as_deref(), Some("900150983cd24fb0d6963f7d28e17f72"));
    assert_eq!(report.sha1.as_deref(), Some("a9993e364706816aba3e25717850c26c9cd0d89d"));
    assert!(calls >= 3, "expected at least one progress call per byte with buf_size=1, got {calls}");
    assert_eq!(last_processed, last_total);
    assert_eq!(last_total, 3);
}

#[test]
fn rahash_file_for_gameboy_console_matches_plain_md5_of_content() {
    let file = TempFile::new("rahash_whole", b"abc");

    let outcome = skip_if_rahash_unavailable!(rahash_file(file.path(), RC_CONSOLE_GAMEBOY, 3));
    let outcome = outcome.expect("rahash_file should succeed");

    assert_eq!(outcome, RahashOutcome::Done("900150983cd24fb0d6963f7d28e17f72".to_string()));
}

#[test]
fn rahash_file_reports_need_more_data_until_file_is_complete() {
    let file = TempFile::new("rahash_partial", b"ab");

    // Only 2 of the eventual 3 bytes are on disk -- must not compute (and definitely
    // must not succeed with) a hash of the partial content.
    let outcome = skip_if_rahash_unavailable!(rahash_file(file.path(), RC_CONSOLE_GAMEBOY, 3));
    assert_eq!(outcome, Ok(RahashOutcome::NeedMoreData));

    file.overwrite(b"abc");

    let outcome = rahash_file(file.path(), RC_CONSOLE_GAMEBOY, 3).expect("rahash_file should succeed once complete");
    assert_eq!(outcome, RahashOutcome::Done("900150983cd24fb0d6963f7d28e17f72".to_string()));
}

#[test]
fn rahash_file_rejects_unknown_console_id() {
    let file = TempFile::new("rahash_unknown_console", b"abc");

    // RC_CONSOLE_UNKNOWN (0) is rejected regardless of whether the data is complete.
    let result = skip_if_rahash_unavailable!(rahash_file(file.path(), 0, 3));
    assert!(result.is_err());
}

#[test]
fn hash_file_combines_other_hashes_and_rahash_with_blended_progress() {
    let file = TempFile::new("hash_file_combined", b"abc");

    let mut progress_values: Vec<f32> = Vec::new();

    let flags = HashFlags::SHA1 | HashFlags::SHA256 | HashFlags::RAHASH;
    let options = HashOptions { flags, buf_size: Some(1), console_id: Some(RC_CONSOLE_GAMEBOY) };
    let result = hash::hash(file.path(), &options, |pct| {
        progress_values.push(pct);
    });

    let report = match result {
        Ok(report) => report,
        Err(err) if err.to_string().contains("RAHasher unavailable") => {
            eprintln!("skipping: RAHasher native library not available on this machine ({err})");
            return;
        }
        Err(err) => panic!("hash::hash should succeed: {err}"),
    };

    assert_eq!(report.sha1.as_deref(), Some("a9993e364706816aba3e25717850c26c9cd0d89d"));
    assert_eq!(
        report.sha256.as_deref(),
        Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    );
    assert_eq!(report.rahash.as_deref(), Some("900150983cd24fb0d6963f7d28e17f72"));

    assert!(!progress_values.is_empty(), "expected at least one progress callback");
    assert!(progress_values.iter().all(|&pct| (0.0..=100.0).contains(&pct)), "progress out of [0,100]: {progress_values:?}");
    assert_eq!(*progress_values.last().unwrap(), 100.0);
}

#[test]
fn default_options_hash_without_buf_size_or_console_id() {
    let file = TempFile::new("default_options", b"abc");

    let report = hash::hash(file.path(), &HashOptions::default(), |_| {}).expect("default options should hash");

    assert_eq!(report.crc32.as_deref(), Some("352441c2"));
    assert_eq!(report.md5.as_deref(), Some("900150983cd24fb0d6963f7d28e17f72"));
    assert_eq!(report.sha1.as_deref(), Some("a9993e364706816aba3e25717850c26c9cd0d89d"));
    assert_eq!(report.sha256, None);
    assert_eq!(report.rahash, None);
}

#[test]
fn rahash_without_a_console_id_is_an_invalid_input_error() {
    let file = TempFile::new("rahash_no_console", b"abc");

    let options = HashOptions { flags: HashFlags::SHA1 | HashFlags::RAHASH, ..Default::default() };
    let err = hash::hash(file.path(), &options, |_| {}).expect_err("RAHASH needs a console id");

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn a_zero_buffer_size_still_hashes_the_whole_file() {
    let file = TempFile::new("zero_buf", b"abc");

    let report = hash_file_other(file.path(), HashFlags::MD5, Some(0), |_, _| {}).expect("hashing should succeed");

    assert_eq!(report.md5.as_deref(), Some("900150983cd24fb0d6963f7d28e17f72"));
}

const ALL_SIZES: [FileSizeClass; 2] = [FileSizeClass::Small, FileSizeClass::Large];
const ALL_PARALLEL: [usize; 4] = [1, 2, 4, 8];
const ALL_STORAGE: [StorageKind; 3] = [StorageKind::Hdd, StorageKind::Ssd, StorageKind::Network];
const ALL_RAM: [RamClass; 3] = [RamClass::Small, RamClass::Medium, RamClass::Large];

const KIB: usize = 1024;
const MIB: usize = 1024 * 1024;

#[test]
fn every_preset_combination_is_a_power_of_two_within_bounds() {
    for size in ALL_SIZES {
        for parallel in ALL_PARALLEL {
            for storage in ALL_STORAGE {
                for ram in ALL_RAM {
                    let buf = recommended_buf_size(size, parallel, storage, ram);
                    assert!(buf.is_power_of_two(), "{size:?} x{parallel} {storage:?} {ram:?} -> {buf}");
                    assert!((MIN_BUF_SIZE..=MAX_BUF_SIZE).contains(&buf), "{size:?} x{parallel} {storage:?} {ram:?} -> {buf}");
                }
            }
        }
    }
}

#[test]
fn preset_spot_checks() {
    use FileSizeClass::{Large, Small};
    use RamClass::{Large as BigRam, Small as TinyRam};
    use StorageKind::{Hdd, Network, Ssd};

    assert_eq!(recommended_buf_size(Large, 1, Ssd, BigRam), 4 * MIB);
    assert_eq!(recommended_buf_size(Large, 1, Network, BigRam), 8 * MIB);
    assert_eq!(recommended_buf_size(Small, 1, Ssd, BigRam), 128 * KIB);
    assert_eq!(recommended_buf_size(Small, 1, Network, BigRam), MIB);
    // A shared spinning disk gets larger reads to amortize seeks.
    assert_eq!(recommended_buf_size(Large, 1, Hdd, BigRam), 4 * MIB);
    assert_eq!(recommended_buf_size(Large, 2, Hdd, BigRam), 8 * MIB);
    // 64 MiB budget / (8 streams x 8 chunks in flight) = 1 MiB per buffer.
    assert_eq!(recommended_buf_size(Large, 8, Ssd, TinyRam), MIB);
    assert_eq!(recommended_buf_size(Large, 8, Network, TinyRam), MIB);
}

#[test]
fn more_parallel_files_never_grow_the_buffer_for_ssd_and_network() {
    for size in ALL_SIZES {
        for storage in [StorageKind::Ssd, StorageKind::Network] {
            for ram in ALL_RAM {
                let sizes: Vec<usize> = ALL_PARALLEL.iter().map(|&p| recommended_buf_size(size, p, storage, ram)).collect();
                assert!(sizes.windows(2).all(|pair| pair[1] <= pair[0]), "{size:?} {storage:?} {ram:?}: {sizes:?}");
            }
        }
    }
}

#[test]
fn zero_parallel_files_counts_as_one() {
    assert_eq!(
        recommended_buf_size(FileSizeClass::Large, 0, StorageKind::Ssd, RamClass::Medium),
        recommended_buf_size(FileSizeClass::Large, 1, StorageKind::Ssd, RamClass::Medium)
    );
}

#[test]
fn file_size_class_boundary() {
    assert_eq!(FileSizeClass::of(0), FileSizeClass::Small);
    assert_eq!(FileSizeClass::of(FileSizeClass::SMALL_MAX_BYTES), FileSizeClass::Small);
    assert_eq!(FileSizeClass::of(FileSizeClass::SMALL_MAX_BYTES + 1), FileSizeClass::Large);
}

fn cpu_has_hardware_sha() -> bool {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        std::is_x86_feature_detected!("sha")
    }
    #[cfg(target_arch = "aarch64")]
    {
        std::arch::is_aarch64_feature_detected!("sha2")
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
    {
        false
    }
}

#[test]
fn fallback_errors_only_when_all_five_file_hashes_are_missing() {
    let empty = HashReport::default();
    assert_eq!(empty.get_fallback().unwrap_err().kind(), std::io::ErrorKind::NotFound);

    // RAHASH and per-member records are not fallback candidates.
    let only_others = HashReport { rahash: Some("r".into()), content_hashes: vec![Default::default()], ..Default::default() };
    assert!(only_others.get_fallback().is_err());

    for (algo, report) in [
        (HashAlgorithms::Crc32, HashReport { crc32: Some("c".into()), ..Default::default() }),
        (HashAlgorithms::Xxhash3_128, HashReport { xxh3_128: Some("x".into()), ..Default::default() }),
        (HashAlgorithms::Md5, HashReport { md5: Some("m".into()), ..Default::default() }),
        (HashAlgorithms::Sha1, HashReport { sha1: Some("s1".into()), ..Default::default() }),
        (HashAlgorithms::Sha256, HashReport { sha256: Some("s2".into()), ..Default::default() }),
    ] {
        let (found, value) = report.get_fallback().expect("one hash is enough");
        assert_eq!(found, algo);
        assert_eq!(Some(value), report.get(algo));
    }
}

#[test]
fn fallback_prefers_crc32_then_xxh3_over_the_slower_hashes() {
    let all = HashReport {
        crc32: Some("c".into()),
        xxh3_128: Some("x".into()),
        md5: Some("m".into()),
        sha1: Some("s1".into()),
        sha256: Some("s2".into()),
        ..Default::default()
    };
    assert_eq!(all.get_fallback().unwrap().0, HashAlgorithms::Crc32);

    let no_crc = HashReport { crc32: None, ..all.clone() };
    assert_eq!(no_crc.get_fallback().unwrap().0, HashAlgorithms::Xxhash3_128);
}

#[test]
fn md5_comes_before_sha1_only_without_hardware_sha() {
    let order = fallback_order();
    let position = |algo| order.iter().position(|&a| a == algo).expect("all five are in the order");

    assert!(position(HashAlgorithms::Crc32) < position(HashAlgorithms::Xxhash3_128));
    assert!(position(HashAlgorithms::Xxhash3_128) < position(HashAlgorithms::Md5).min(position(HashAlgorithms::Sha1)));
    assert!(position(HashAlgorithms::Sha1) < position(HashAlgorithms::Sha256));
    assert_eq!(position(HashAlgorithms::Md5) < position(HashAlgorithms::Sha1), !cpu_has_hardware_sha());

    // With SHA-1 and MD5 the only stored hashes, the one this CPU prefers wins.
    let report = HashReport { md5: Some("m".into()), sha1: Some("s".into()), ..Default::default() };
    let expected = if cpu_has_hardware_sha() { HashAlgorithms::Sha1 } else { HashAlgorithms::Md5 };
    assert_eq!(report.get_fallback().unwrap().0, expected);
}

/// The scan computes an RA hash only for consoles where it is not the file's MD5: NES drops
/// the 16-byte iNES header, so its hash is the MD5 of what follows, not of the file.
#[test]
fn ra_hash_of_a_nes_rom_skips_its_header_and_the_console_map_says_so() {
    use md5::{Digest, Md5};
    use romexplorer::common::models::Platform;

    let nes = hash::ra_console_id(Platform::Nes).expect("NES has an RA console");
    assert_eq!(nes, 7);
    assert!(!hash::ra_hash_is_file_md5(nes), "NES needs its own RA hash");
    assert!(hash::ra_hash_is_file_md5(hash::ra_console_id(Platform::GameBoy).unwrap()));
    assert!(!hash::ra_hash_is_file_md5(hash::ra_console_id(Platform::PlayStation).unwrap()));
    assert_eq!(hash::ra_console_id(Platform::PlayStation3), None);

    let body: Vec<u8> = (0..32 * 1024).map(|i| (i % 251) as u8).collect();
    let mut rom = b"NES\x1a\x02\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00".to_vec();
    rom.extend_from_slice(&body);
    let file = TempFile::new("rahash_nes.nes", &rom);
    let outcome = skip_if_rahash_unavailable!(rahash_file(file.path(), nes, 0));
    let expected: String = Md5::digest(&body).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(outcome, Ok(RahashOutcome::Done(expected)));
}
