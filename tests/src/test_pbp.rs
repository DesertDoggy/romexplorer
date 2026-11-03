//! PS1 discs out of an `EBOOT.PBP` (`rom::pbp`, through Beetle PSX's reader), on synthetic
//! PBPs built here the way PSX2PSP/popstation lay them out: the PSISOIMG header, its TOC in
//! absolute time (track 1 at 00:02:00), the block index, and the disc as raw sectors deflated
//! sixteen at a time. No game data. Skips when `psxpbp` or zlib-ng is not built.

use std::path::{Path, PathBuf};

use romexplorer::rom::pbp::{self, DiscReader, Pbp};
use romexplorer::rom::source::ReadAt;
use romexplorer::rom::zlib_ng;

#[path = "support/disc_images.rs"]
mod disc_images;
use disc_images::{pattern, pbp_file, psisoimg};

const SECTOR: usize = 2352;

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("romexplorer_test_pbp_{}_{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn ready() -> bool {
    if !pbp::available() || !zlib_ng::available() {
        eprintln!("skipped: psxpbp or zlib-ng is not built");
        return false;
    }
    true
}

fn read_all(pbp: &Pbp, layout: &pbp::DiscLayout) -> Vec<u8> {
    let mut r = DiscReader::new(pbp, layout);
    let mut out = vec![0u8; layout.total_bytes() as usize];
    r.read_at(0, &mut out).unwrap();
    out
}

#[test]
fn a_single_track_disc_comes_back_exactly() {
    if !ready() {
        return;
    }
    let dir = TempDir::new("single");
    let image = pattern(1000 * SECTOR, 1); // not a multiple of 16 sectors
    let path = dir.0.join("EBOOT.PBP");
    pbp_file(&path, &psisoimg(&image, &[(false, 0)]));
    let p = Pbp::open(&path).unwrap();
    assert_eq!(p.disc_count(), 1);
    let layout = pbp::disc_layout(&p, 0, "Game").unwrap();
    assert_eq!(layout.cue_name, "Game.cue");
    assert_eq!(layout.tracks.len(), 1);
    assert_eq!(layout.tracks[0].name, "Game.bin");
    assert_eq!(layout.tracks[0].sectors, 1000);
    assert!(layout.cue.contains("TRACK 01 MODE2/2352"));
    assert!(read_all(&p, &layout) == image);

    // Unaligned reads cross sector and block boundaries.
    let mut r = DiscReader::new(&p, &layout);
    let mut piece = vec![0u8; 50_000];
    r.read_at(37 * SECTOR as u64 + 11, &mut piece).unwrap();
    assert!(piece[..] == image[37 * SECTOR + 11..37 * SECTOR + 11 + 50_000]);
    assert!(r.read_at(image.len() as u64 - 4, &mut [0u8; 8]).is_err());
}

#[test]
fn audio_tracks_get_redump_files_with_their_pregap() {
    if !ready() {
        return;
    }
    let dir = TempDir::new("audio");
    // Track 1 data (800 sectors), track 2 audio from sector 800 (its pregap) for 500 sectors.
    let image = pattern(1300 * SECTOR, 2);
    let path = dir.0.join("EBOOT.PBP");
    pbp_file(&path, &psisoimg(&image, &[(false, 0), (true, 800)]));
    let p = Pbp::open(&path).unwrap();
    let layout = pbp::disc_layout(&p, 0, "Game").unwrap();
    let names: Vec<_> = layout.tracks.iter().map(|t| (t.name.as_str(), t.first_sector, t.sectors, t.audio)).collect();
    assert_eq!(names, [("Game (Track 1).bin", 0, 800, false), ("Game (Track 2).bin", 800, 500, true)]);
    assert!(layout.cue.contains("TRACK 02 AUDIO\r\n    INDEX 00 00:00:00\r\n    INDEX 01 00:02:00"));
    assert!(read_all(&p, &layout) == image);
}

#[test]
fn something_else_is_refused() {
    if !ready() {
        return;
    }
    let dir = TempDir::new("bad");
    let path = dir.0.join("EBOOT.PBP");
    std::fs::write(&path, pattern(4096, 3)).unwrap();
    assert!(Pbp::open(&path).is_err());
    assert!(!pbp::is_pbp(&pattern(16, 3)));
    assert!(pbp::is_pbp(b"\0PBP\0\0\x01\0"));
}

/// What a scan does with a container: decode it and hash each content file.
fn members(path: &Path) -> Vec<(String, Option<u64>, Option<String>)> {
    use romexplorer::rom::decode_threads::DecodeThreads;
    use romexplorer::rom::detect::sheet::{CueSheetOptions, SheetKind};
    use romexplorer::rom::hash::HashFlags;
    let cancel = std::sync::atomic::AtomicBool::new(false);
    romexplorer::rom::operation::decode_members(path, HashFlags::SHA1, &CueSheetOptions::default(), SheetKind::Cue, &DecodeThreads::recommended(), &cancel)
        .unwrap()
        .into_iter()
        .map(|r| (r.path, r.size_bytes, r.sha1))
        .collect()
}

fn sha1_hex(data: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    Sha1::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn a_scan_hashes_a_pbp_as_its_cue_and_tracks() {
    if !ready() {
        return;
    }
    let dir = TempDir::new("scan");
    let image = pattern(1300 * SECTOR, 5);
    let path = dir.0.join("Game.pbp");
    pbp_file(&path, &psisoimg(&image, &[(false, 0), (true, 800)]));
    let got = members(&path);
    assert_eq!(got.len(), 3);
    assert_eq!(got[0].0, "Game.cue");
    assert_eq!(got[1], ("Game (Track 1).bin".to_string(), Some(800 * SECTOR as u64), Some(sha1_hex(&image[..800 * SECTOR]))));
    assert_eq!(got[2], ("Game (Track 2).bin".to_string(), Some(500 * SECTOR as u64), Some(sha1_hex(&image[800 * SECTOR..]))));
}

#[test]
fn a_scan_hashes_a_cso_as_the_iso_it_holds() {
    if !zlib_ng::available() {
        return;
    }
    let dir = TempDir::new("cso");
    let iso: Vec<u8> = pattern(3 << 20, 6).into_iter().chain(std::iter::repeat(0).take(12345)).collect();
    let (src, cso) = (dir.0.join("Game.iso"), dir.0.join("Game.cso"));
    std::fs::write(&src, &iso).unwrap();
    romexplorer::rom::cso::write(&src, &cso, 9, |_, _| true).unwrap();
    assert_eq!(members(&cso), [("Game.iso".to_string(), Some(iso.len() as u64), Some(sha1_hex(&iso)))]);
}

/// A PSP game's, DLC's or homebrew's PBP (`PARAM.pbp`, an empty PSAR or `NPUMDIMG`) holds no
/// PlayStation disc: a scan hashes it as the file it is. It used to be opened as a PS1 disc,
/// fail, and be left "not hashed". Needs neither library.
#[test]
fn a_pbp_with_no_ps1_disc_is_hashed_as_it_is() {
    use romexplorer::rom::hash::HashFlags;
    use romexplorer::rom::operation::{Single, SingleArgs, StageSwitches};
    let dir = TempDir::new("psp");
    let param = dir.0.join("PARAM.pbp");
    pbp_file(&param, &[]);
    let umd = dir.0.join("EBOOT.PBP");
    pbp_file(&umd, b"NPUMDIMG\0\0\0\0\0\0\0\0");
    let ps1 = dir.0.join("Game.pbp");
    pbp_file(&ps1, b"PSISOIMG0000");
    assert!(!pbp::holds_ps1_disc(&param));
    assert!(!pbp::holds_ps1_disc(&umd));
    assert!(pbp::holds_ps1_disc(&ps1));
    for path in [&param, &umd] {
        // As a scan reads it (`api::scan::scan_args`).
        let args = SingleArgs { stages: StageSwitches { hash: true, ..StageSwitches::NONE }, hash_flags: HashFlags::SHA1, container_hash: Some(HashFlags::SHA1), ..SingleArgs::default() };
        let outcome = Single::new(path, args).run(&mut |_| true).unwrap();
        assert!(outcome.stages.hash.is_ok(), "{}: {:?}", path.display(), outcome.stages);
        let records = outcome.input_hashes.map(|r| r.content_hashes).unwrap_or_default();
        assert_eq!(records.len(), 1, "{}", path.display());
        assert_eq!(records[0].sha1.as_deref(), Some(sha1_hex(&std::fs::read(path).unwrap()).as_str()), "{}", path.display());
    }
}
