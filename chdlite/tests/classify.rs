//! The decisions chdlite makes without any native library: which container an image wants,
//! which files of a drop are one job, and which codecs a mode asks chdman for.

use std::path::{Path, PathBuf};

use chdlite::classify::{classify, expand, Input, Target};
use chdlite::job::{chd_args, parse_codecs, rvz_level, Mode};
use chdlite::settings::{CompMode, LitePreset, Settings};
use romexplorer::common::models::Platform;
use romexplorer::rom::dolphin_wrapper::DiscCompression;
use romexplorer::rom::models::ChdCodecs;

const SYNC: [u8; 12] = [0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0];

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("chdlite-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = name.split('/').fold(self.0.clone(), |path, part| path.join(part));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        path
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `sectors` raw 2352-byte sectors, the first opening with the sync pattern (mode 1).
fn raw_track(sectors: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; sectors * 2352];
    bytes[..12].copy_from_slice(&SYNC);
    bytes[15] = 1;
    bytes
}

fn target_of(path: &Path) -> Target {
    match classify(path).unwrap() {
        Input::Image { target, .. } => target,
        other => panic!("expected an image, got {other:?}"),
    }
}

#[test]
fn nintendo_headers_want_rvz() {
    let dir = Dir::new("nintendo");
    let mut gamecube = vec![0u8; 4096];
    gamecube[0x1C..0x20].copy_from_slice(&[0xC2, 0x33, 0x9F, 0x3D]);
    let mut wii = vec![0u8; 4096];
    wii[0x18..0x1C].copy_from_slice(&[0x5D, 0x1C, 0x9E, 0xA3]);
    assert_eq!(target_of(&dir.file("gc.iso", &gamecube)), Target::Rvz);
    assert_eq!(target_of(&dir.file("wii.iso", &wii)), Target::Rvz);
    assert_eq!(target_of(&dir.file("gc.gcm", &gamecube)), Target::Rvz);
}

#[test]
fn cooked_image_is_a_dvd_and_raw_sectors_are_a_cd() {
    let dir = Dir::new("media");
    assert_eq!(target_of(&dir.file("dvd.iso", &vec![0u8; 2048 * 32])), Target::DvdChd);

    let lone = dir.file("lone.bin", &raw_track(4));
    assert_eq!(classify(&lone).unwrap(), Input::Image { target: Target::CdChd, source: lone.clone(), needs_cue: true });
}

#[test]
fn a_cooked_iso_sized_like_raw_sectors_is_still_a_dvd() {
    let dir = Dir::new("cooked-2352");
    // 147 sectors of 2048 bytes are exactly 128 of 2352, with an ISO 9660 volume at sector 16.
    let mut bytes = vec![0u8; 2048 * 147];
    bytes[16 * 2048..16 * 2048 + 6].copy_from_slice(b"\x01CD001");
    assert_eq!(target_of(&dir.file("game.iso", &bytes)), Target::DvdChd);
}

#[test]
fn a_track_stands_for_the_sheet_that_lists_it() {
    let dir = Dir::new("sheet");
    let track = dir.file("Game (Track 1).bin", &raw_track(4));
    let cue = dir.file("Game.cue", b"FILE \"Game (Track 1).bin\" BINARY\r\n  TRACK 01 MODE1/2352\r\n    INDEX 01 00:00:00\r\n");
    assert_eq!(classify(&track).unwrap(), Input::Image { target: Target::CdChd, source: cue.clone(), needs_cue: false });
    assert_eq!(target_of(&cue), Target::CdChd);
}

#[test]
fn containers_and_junk() {
    let dir = Dir::new("other");
    assert_eq!(classify(&dir.file("a.chd", b"MComprHD")).unwrap(), Input::Chd);
    assert_eq!(classify(&dir.file("a.rvz", b"RVZ\x01")).unwrap(), Input::Rvz);
    assert!(classify(&dir.file("notes.txt", b"hello")).is_err());
    // Neither whole raw sectors nor 2048-aligned: not something to feed chdman.
    assert!(classify(&dir.file("odd.bin", &vec![7u8; 1000])).is_err());
}

#[test]
fn expand_makes_one_job_per_disc() {
    let dir = Dir::new("expand");
    let cue = dir.file("a/Game.cue", b"FILE \"Game.bin\" BINARY\r\n  TRACK 01 MODE1/2352\r\n    INDEX 01 00:00:00\r\n");
    let bin = dir.file("a/Game.bin", &raw_track(4));
    let iso = dir.file("b/deep/Disc.iso", &vec![0u8; 2048 * 16]);
    dir.file("b/readme.txt", b"not a disc");

    // A folder: the bin is covered by its cue, the text file is ignored, subfolders are walked.
    assert_eq!(expand(&[dir.0.clone()]), vec![cue.clone(), iso.clone()]);
    // The track on its own, and the track together with its sheet, are each the one disc.
    assert_eq!(expand(&[bin.clone()]), vec![cue.clone()]);
    assert_eq!(expand(&[bin, cue.clone(), iso.clone(), iso.clone()]), vec![cue, iso]);
}

#[test]
fn modes_pick_their_codecs() {
    let mut settings = Settings::default();

    let lite_cd = chd_args(Mode::Lite, &settings, Target::CdChd, Platform::PlayStation).unwrap();
    assert_eq!(lite_cd.codecs, Some(vec![ChdCodecs::Cdzs, ChdCodecs::Cdfl]));
    assert_eq!(lite_cd.hunk_size, Some(19584));
    let lite_gd = chd_args(Mode::Lite, &settings, Target::GdChd, Platform::Dreamcast).unwrap();
    assert_eq!(lite_gd.hunk_size, Some(2448));
    let lite_psp = chd_args(Mode::Lite, &settings, Target::DvdChd, Platform::PlayStationPortable).unwrap();
    assert_eq!((lite_psp.codecs, lite_psp.hunk_size), (Some(vec![ChdCodecs::Zstd]), Some(2048)));

    settings.lite.preset = LitePreset::Mobile;
    let mobile_ps2 = chd_args(Mode::Lite, &settings, Target::DvdChd, Platform::PlayStation2).unwrap();
    assert_eq!(mobile_ps2.codecs, Some(vec![ChdCodecs::Zlib]));

    let best = chd_args(Mode::Comp, &settings, Target::CdChd, Platform::Unknown).unwrap();
    assert_eq!(best.codecs, Some(vec![ChdCodecs::Cdlz, ChdCodecs::Cdzs, ChdCodecs::Cdzl, ChdCodecs::Cdfl]));
    let best_dvd = chd_args(Mode::Comp, &settings, Target::DvdChd, Platform::Unknown).unwrap();
    assert_eq!(best_dvd.codecs, Some(vec![ChdCodecs::Lzma, ChdCodecs::Zstd, ChdCodecs::Zlib, ChdCodecs::Huff]));

    settings.comp.mode = CompMode::Chdman;
    let chdman = chd_args(Mode::Comp, &settings, Target::CdChd, Platform::Unknown).unwrap();
    assert_eq!((chdman.codecs, chdman.hunk_size), (None, None));

    settings.comp.mode = CompMode::Custom;
    settings.comp.dvd_codecs = vec!["zstd".into(), "flac".into()];
    settings.comp.dvd_hunk_size = 4096;
    settings.comp.cd_hunk_size = 4096;
    let custom = chd_args(Mode::Comp, &settings, Target::DvdChd, Platform::Unknown).unwrap();
    assert_eq!((custom.codecs, custom.hunk_size), (Some(vec![ChdCodecs::Zstd, ChdCodecs::Flac]), Some(4096)));
    // 4096 is not a whole number of CD frames.
    assert!(chd_args(Mode::Comp, &settings, Target::CdChd, Platform::Unknown).is_err());
}

#[test]
fn codec_lists_are_checked() {
    let names = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert!(parse_codecs(&names(&["cdzs", "cdfl"]), true).is_ok());
    assert!(parse_codecs(&names(&["zstd"]), true).is_err(), "a DVD codec on a CD");
    assert!(parse_codecs(&names(&["cdzs"]), false).is_err(), "a CD codec on a DVD");
    assert!(parse_codecs(&names(&["nope"]), false).is_err());
    assert!(parse_codecs(&names(&["lzma", "zstd", "zlib", "huff", "flac"]), false).is_err(), "five codecs");
    assert_eq!(rvz_level(DiscCompression::Zstd, 40), 22);
    assert_eq!(rvz_level(DiscCompression::Lzma2, 40), 9);
    assert_eq!(rvz_level(DiscCompression::None, 5), 0);
}

#[test]
fn settings_round_trip_and_tolerate_a_partial_file() {
    let dir = Dir::new("settings");
    let path = dir.0.join("chdlite.toml");

    let mut settings = Settings::default();
    settings.output.dir = "D:/out".into();
    settings.comp.mode = CompMode::Custom;
    settings.comp.cd_codecs = vec!["cdzs".into()];
    settings.rvz.level = 19;
    settings.jobs.concurrent_files = 2;
    settings.save_to(&path).unwrap();
    assert_eq!(Settings::load_from(&path), settings);

    std::fs::write(&path, "[rvz]\ncodec = \"lzma2\"\n").unwrap();
    let partial = Settings::load_from(&path);
    assert_eq!(partial.rvz.codec, "lzma2");
    assert_eq!(partial.rvz.block, "128k");
    assert_eq!(partial.comp, Settings::default().comp);

    assert_eq!(Settings::load_from(&dir.0.join("missing.toml")), Settings::default());
    assert!(Settings::problem_with(&dir.0.join("missing.toml")).is_none());

    // A file that does not parse is reported, and kept aside rather than overwritten.
    std::fs::write(&path, "[rvz\nlevel = ").unwrap();
    assert!(Settings::problem_with(&path).is_some());
    Settings::default().save_to(&path).unwrap();
    assert_eq!(std::fs::read_to_string(dir.0.join("chdlite.toml.bak")).unwrap(), "[rvz\nlevel = ");
    assert!(Settings::problem_with(&path).is_none());
}

#[test]
fn files_at_once_follows_the_core_count() {
    let mut settings = Settings::default();
    settings.jobs.threads = 16;
    assert_eq!(settings.jobs.plan(10), (4, 4));
    assert_eq!(settings.jobs.plan(1), (1, 16));
    settings.jobs.threads = 4;
    assert_eq!(settings.jobs.plan(10), (1, 4));
    settings.jobs.concurrent_files = 2;
    assert_eq!(settings.jobs.plan(10), (2, 2));
}
