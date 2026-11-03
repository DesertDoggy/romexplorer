//! Xbox and Xbox 360 discs (`rom::xbox`): the XDVDFS walk, Xbox 360 images into `.zar`, and
//! Xbox images into XISOs through extract-xiso, on synthetic discs built here. No game data:
//! each disc is an XDVDFS volume holding a few patterned files, placed where a Redump image
//! puts its game partition (XGD1 for Xbox, XGD2 for Xbox 360), with filler in front of it
//! standing in for the video partition. The files are sparse, so the 400 MB image costs
//! little disk.
//!
//! The `.zar` tests skip when `cemuwua` is not built, the XISO tests when `exiso` is not.

use std::fs::File;
use std::path::PathBuf;

use romexplorer::common::models::Platform;
use romexplorer::rom::cemu_wrapper::{self, Verified};
use romexplorer::rom::xbox;
use sha1::{Digest, Sha1};

#[path = "support/disc_images.rs"]
mod disc_images;
use disc_images::{pattern, redump_image, XGD1, XGD2};


struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("romexplorer_test_xbox_{}_{}", std::process::id(), name));
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

fn sha1_hex(data: &[u8]) -> String {
    Sha1::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn the_walk_finds_every_file_and_the_console() {
    let dir = TempDir::new("walk");
    let x360 = dir.0.join("x360.iso");
    let xbox = dir.0.join("xbox.iso");
    redump_image(&x360, XGD2, "default.xex");
    redump_image(&xbox, XGD1, "default.xbe");
    assert_eq!(xbox::platform_of(&x360), Some(Platform::Xbox360));
    assert_eq!(xbox::platform_of(&xbox), Some(Platform::Xbox));

    let mut f = File::open(&x360).unwrap();
    let v = xbox::open_volume(&mut f).unwrap().unwrap();
    assert_eq!(v.base, XGD2);
    let paths: Vec<String> = xbox::entries(&mut f, &v).unwrap().into_iter().map(|e| e.path).collect();
    assert_eq!(paths, ["default.xex", "media", "media/a.bin", "media/empty.txt", "media/sub", "media/sub/b.txt", "z.dat"]);

    let hashes = xbox::file_hashes(&x360, &mut || true).unwrap();
    assert_eq!(hashes["MEDIA/SUB/B.TXT"], (6000, sha1_hex(&b"hello ".repeat(1000))));
    assert_eq!(xbox::platform_of(&dir.0.join("missing.iso")), None);
}

#[test]
fn an_xbox_360_image_becomes_a_zar_that_verifies() {
    if !xbox::zar_available() {
        eprintln!("skipped: cemuwua is not built");
        return;
    }
    let dir = TempDir::new("zar");
    let iso = dir.0.join("game.iso");
    let zar = dir.0.join("game.zar");
    redump_image(&iso, XGD2, "default.xex");
    let image = std::fs::read(&iso).unwrap();

    let mut last = (0, 0);
    let written = xbox::create_zar(&iso, &zar, |h, size| serde_json::json!({ "name": "game.iso", "size": size, "sha1": h.sha1 }), |d, t| {
        last = (d, t);
        true
    })
    .unwrap();
    assert_eq!(last, (image.len() as u64, image.len() as u64));
    assert_eq!(written.image.sha1.as_deref(), Some(sha1_hex(&image).as_str()), "the whole image is hashed in the same pass");
    assert_eq!(written.manifest.source["sha1"], sha1_hex(&image));
    assert_eq!(written.manifest.titles[0].files.len(), 5);

    // What Xenia would see: the game's files at the archive root.
    let got = std::cell::RefCell::new(std::collections::BTreeMap::<String, Vec<u8>>::new());
    let current = std::cell::RefCell::new(String::new());
    cemu_wrapper::zar_stream(
        &zar,
        |p, _| {
            *current.borrow_mut() = p.to_string();
            got.borrow_mut().insert(p.to_string(), Vec::new());
            true
        },
        |d| {
            got.borrow_mut().get_mut(&*current.borrow()).unwrap().extend_from_slice(d);
            true
        },
    )
    .unwrap();
    let got = got.into_inner();
    assert_eq!(got.keys().cloned().collect::<Vec<_>>(), ["default.xex", "media/a.bin", "media/empty.txt", "media/sub/b.txt", "z.dat"]);
    assert_eq!(got["media/sub/b.txt"], b"hello ".repeat(1000));
    assert_eq!(got["media/a.bin"], pattern(1 << 20, 2));

    match xbox::verify_zar(&zar, |_, _| true).unwrap() {
        Verified::Intact(m) => assert_eq!(m.tool, xbox::ZAR_TOOL),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_cancelled_zar_leaves_nothing() {
    if !xbox::zar_available() {
        return;
    }
    let dir = TempDir::new("zar_cancel");
    let iso = dir.0.join("game.iso");
    let zar = dir.0.join("game.zar");
    redump_image(&iso, XGD2, "default.xex");
    let err = xbox::create_zar(&iso, &zar, |_, _| serde_json::Value::Null, |d, _| d < 8 << 20).unwrap_err();
    assert_eq!(err, "cancelled");
    assert!(!zar.exists());
}

#[test]
fn something_that_is_not_an_xbox_disc_is_refused() {
    let dir = TempDir::new("not_xbox");
    let iso = dir.0.join("plain.iso");
    std::fs::write(&iso, pattern(1 << 20, 4)).unwrap();
    assert_eq!(xbox::platform_of(&iso), None);
    if xbox::zar_available() {
        assert!(xbox::create_zar(&iso, &dir.0.join("x.zar"), |_, _| serde_json::Value::Null, |_, _| true).unwrap_err().contains("not an Xbox"));
    }
    if xbox::xiso_available() {
        assert!(xbox::rewrite_xiso(&iso, &dir.0.join("x.iso"), |_, _| true).is_err());
    }
}

#[test]
fn an_xbox_image_becomes_an_xiso_with_the_same_files() {
    if !xbox::xiso_available() {
        eprintln!("skipped: exiso is not built");
        return;
    }
    let dir = TempDir::new("xiso");
    let iso = dir.0.join("game.iso");
    let out = dir.0.join("game.xiso.iso");
    redump_image(&iso, XGD1, "default.xbe");
    let mut last = (0, 0);
    xbox::rewrite_xiso(&iso, &out, |d, t| {
        last = (d, t);
        true
    })
    .unwrap();
    assert!(last.1 > 0 && last.0 == last.1, "progress reached the end: {last:?}");

    // The game partition alone, at offset 0: far smaller than the Redump image.
    let size = std::fs::metadata(&out).unwrap().len();
    assert!(size < 8 << 20, "{size}");
    let mut f = File::open(&out).unwrap();
    assert_eq!(xbox::open_volume(&mut f).unwrap().unwrap().base, 0);
    assert_eq!(xbox::platform_of(&out), Some(Platform::Xbox));
    assert_eq!(xbox::compare_files(&iso, &out, || true).unwrap(), None);
    // .xbe files are copied as they are: extract-xiso's media-flag patching is off.
    assert_eq!(xbox::file_hashes(&out, &mut || true).unwrap()["DEFAULT.XBE"].1, sha1_hex(&pattern(300_000, 1)));
}

#[test]
fn a_cancelled_xiso_leaves_nothing() {
    if !xbox::xiso_available() {
        return;
    }
    let dir = TempDir::new("xiso_cancel");
    let iso = dir.0.join("game.iso");
    let out = dir.0.join("game.xiso.iso");
    redump_image(&iso, XGD1, "default.xbe");
    let mut calls = 0;
    let err = xbox::rewrite_xiso(&iso, &out, |d, t| {
        calls += 1;
        d < t / 2
    })
    .unwrap_err();
    assert_eq!(err, "cancelled");
    assert!(calls > 1, "progress came part way through, not only at the end");
    assert!(!out.exists());
}
