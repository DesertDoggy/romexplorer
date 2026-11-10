//! chdlite's Xbox, CSO and PBP handling, end to end on synthetic discs (no game data): which
//! target each wants, the lossy ones it warns about, and each conversion's output. A test
//! skips when the native library it needs is not built.

use std::path::{Path, PathBuf};

use chdlite::classify::{classify, expand, lossy_targets, Input, Target};
use chdlite::job::{run_one, Event, JobSpec, Mode};
use chdlite::settings::Settings;
use romexplorer::rom::{cemu_wrapper, cso, pbp, xbox, zlib_ng};

#[path = "../../tests/src/support/disc_images.rs"]
mod disc_images;
use disc_images::{pattern, pbp_file, psisoimg, redump_image, CD_SECTOR, XGD1, XGD2};

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("chdlite-formats-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn job(mode: Mode, input: &Path, out: &Path) -> Result<String, String> {
    let mut settings = Settings::default();
    settings.output.dir = out.to_string_lossy().into_owned();
    let spec = JobSpec { mode, input: input.to_path_buf(), settings, threads: Some(2) };
    run_one(&spec, &mut |event| {
        if let Event::Message(m) = event {
            eprintln!("  {m}");
        }
        true
    })
}

fn chdman_ready() -> bool {
    let caps = romexplorer::rom::native_lib::detect_capabilities(romexplorer::rom::native_lib::LibVariant::Streaming);
    caps.chdman.path.is_some()
}

#[test]
fn xbox_images_want_zar_or_xiso_and_an_xiso_is_refused() {
    let dir = Dir::new("xbox");
    let x360 = dir.0.join("x360.iso");
    let xbox_iso = dir.0.join("xbox.iso");
    let already = dir.0.join("already.iso");
    redump_image(&x360, XGD2, "default.xex");
    redump_image(&xbox_iso, XGD1, "default.xbe");
    redump_image(&already, 0, "default.xbe");
    let target = |p: &Path| match classify(p).unwrap() {
        Input::Image { target, .. } => target,
        other => panic!("{other:?}"),
    };
    assert_eq!(target(&x360), Target::Zar);
    assert_eq!(target(&xbox_iso), Target::Xiso);
    assert!(classify(&already).unwrap_err().contains("XISO"));
    assert!(Target::Zar.is_lossy() && Target::Xiso.is_lossy() && Target::Wua.is_lossy());
    assert!(!Target::DvdChd.is_lossy() && !Target::Rvz.is_lossy());

    // The warning names exactly the lossy ones.
    let plain = dir.0.join("plain.iso");
    std::fs::write(&plain, vec![0u8; 2048 * 64]).unwrap();
    let lossy: Vec<Target> = lossy_targets(&[x360.clone(), xbox_iso.clone(), plain]).into_iter().map(|(_, t)| t).collect();
    assert_eq!(lossy, [Target::Zar, Target::Xiso]);
}

#[test]
fn only_lossless_inputs_are_taken() {
    let dir = Dir::new("inputs");
    for name in ["a.wua", "b.zar", "c.cso", "d.pbp"] {
        std::fs::write(dir.0.join(name), b"x").unwrap();
    }
    let found: Vec<String> = expand(&[dir.0.clone()]).iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(found, ["c.cso", "d.pbp"]);
    assert_eq!(classify(&dir.0.join("c.cso")).unwrap(), Input::Cso);
    assert_eq!(classify(&dir.0.join("d.pbp")).unwrap(), Input::Pbp);
    assert!(classify(&dir.0.join("a.wua")).is_err());
}

#[test]
fn an_xbox_360_image_becomes_a_zar_and_an_xbox_image_an_xiso() {
    let dir = Dir::new("xbox_out");
    let out = dir.0.join("out");
    if xbox::zar_available() {
        let iso = dir.0.join("game360.iso");
        redump_image(&iso, XGD2, "default.xex");
        let said = job(Mode::Lite, &iso, &out).unwrap();
        assert!(said.starts_with("ZAR:"), "{said}");
        match xbox::verify_zar(&out.join("game360.zar"), |_, _| true).unwrap() {
            cemu_wrapper::Verified::Intact(m) => assert_eq!(m.source["name"], "game360.iso"),
            other => panic!("{other:?}"),
        }
    } else {
        eprintln!("skipped zar: cemuwua is not built");
    }
    if xbox::xiso_available() {
        let iso = dir.0.join("gamexbox.iso");
        redump_image(&iso, XGD1, "default.xbe");
        job(Mode::Lite, &iso, &out).unwrap();
        let xiso = out.join("gamexbox.xiso.iso");
        assert_eq!(xbox::compare_files(&iso, &xiso, || true).unwrap(), None);
    } else {
        eprintln!("skipped xiso: exiso is not built");
    }
}

#[test]
fn a_cso_becomes_a_dvd_chd_and_extracts_to_its_iso() {
    if !zlib_ng::available() || !chdman_ready() {
        eprintln!("skipped: zlib-ng or chdman is not built");
        return;
    }
    let dir = Dir::new("cso");
    let out = dir.0.join("out");
    let iso: Vec<u8> = pattern(2048 * 3000, 5);
    let iso_path = dir.0.join("game.iso");
    std::fs::write(&iso_path, &iso).unwrap();
    let cso_path = dir.0.join("game.cso");
    cso::write(&iso_path, &cso_path, 9, |_, _| true).unwrap();
    std::fs::remove_file(&iso_path).unwrap();

    assert!(job(Mode::Read, &cso_path, &out).unwrap().contains("CSO v1"));
    job(Mode::Extract, &cso_path, &out).unwrap();
    assert!(std::fs::read(out.join("game.iso")).unwrap() == iso);
    std::fs::remove_file(out.join("game.iso")).unwrap();

    job(Mode::Lite, &cso_path, &out).unwrap();
    let chd = out.join("game.chd");
    assert!(chd.is_file());
    let left: Vec<_> = std::fs::read_dir(&out).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(left.len(), 1, "the scratch ISO is gone: {left:?}");
    let back = dir.0.join("back");
    job(Mode::Extract, &chd, &back).unwrap();
    assert!(std::fs::read(back.join("game.iso")).unwrap() == iso);
}

#[test]
fn a_pbp_becomes_a_cd_chd_and_extracts_to_cue_and_bin() {
    if !pbp::available() || !zlib_ng::available() || !chdman_ready() {
        eprintln!("skipped: psxpbp, zlib-ng or chdman is not built");
        return;
    }
    let dir = Dir::new("pbp");
    let out = dir.0.join("out");
    let mut image = pattern(CD_SECTOR * 400, 7);
    // A mode 2 sync and header on every sector, so chdman takes it as a data track.
    for (i, s) in image.chunks_mut(CD_SECTOR).enumerate() {
        s[..12].copy_from_slice(&[0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0]);
        let lba = i as u32 + 150;
        let bcd = |v: u32| (((v / 10) << 4) | (v % 10)) as u8;
        s[12] = bcd(lba / 4500);
        s[13] = bcd(lba / 75 % 60);
        s[14] = bcd(lba % 75);
        s[15] = 2;
    }
    let game = dir.0.join("Game");
    std::fs::create_dir_all(&game).unwrap();
    let eboot = game.join("EBOOT.PBP");
    pbp_file(&eboot, &psisoimg(&image, &[(false, 0)]));

    let read = job(Mode::Read, &eboot, &out).unwrap();
    assert!(read.contains("1 disc") && read.contains("Game.cue"), "{read}");
    job(Mode::Extract, &eboot, &out).unwrap();
    assert!(std::fs::read(out.join("Game.bin")).unwrap() == image);
    for f in ["Game.bin", "Game.cue"] {
        std::fs::remove_file(out.join(f)).unwrap();
    }

    job(Mode::Lite, &eboot, &out).unwrap();
    let chd = out.join("Game.chd");
    assert!(chd.is_file());
    let back = dir.0.join("back");
    job(Mode::Extract, &chd, &back).unwrap();
    assert!(std::fs::read(back.join("Game.bin")).unwrap() == image);
}

fn convert_cart(input: &Path, out: Option<&Path>, set: impl FnOnce(&mut Settings)) -> Result<String, String> {
    let mut settings = Settings::default();
    if let Some(out) = out {
        settings.output.dir = out.to_string_lossy().into_owned();
    }
    set(&mut settings);
    run_one(&JobSpec { mode: Mode::Lite, input: input.to_path_buf(), settings, threads: Some(1) }, &mut |_| true)
}

#[test]
fn a_cartridge_dump_goes_to_the_chosen_layout_and_one_already_in_it_is_skipped() {
    let dir = Dir::new("cart");
    let z64: Vec<u8> = [0x80u8, 0x37, 0x12, 0x40].into_iter().chain((0..8188u32).map(|i| (i * 7 % 251) as u8)).collect();
    let input = dir.0.join("cart.z64");
    std::fs::write(&input, &z64).unwrap();
    assert!(matches!(classify(&input).unwrap(), Input::Cartridge(_)));

    // Beside the input, under the new extension.
    convert_cart(&input, None, |s| s.cartridge.n64 = "v64".into()).unwrap();
    let v64 = std::fs::read(dir.0.join("cart.v64")).unwrap();
    assert_eq!(&v64[..4], &[0x37, 0x80, 0x40, 0x12]);

    // Already in the layout: nothing is written, even with an output folder.
    let out = dir.0.join("out");
    let skipped = convert_cart(&input, Some(&out), |s| s.cartridge.n64 = "z64".into()).unwrap();
    assert!(skipped.starts_with("skipped, already"), "{skipped}");
    assert!(!out.join("cart.z64").exists());

    // The right content under the wrong extension gets a copy under the right one.
    let misnamed = dir.0.join("misnamed.n64");
    std::fs::write(&misnamed, &z64).unwrap();
    convert_cart(&misnamed, Some(&out), |s| s.cartridge.n64 = "z64".into()).unwrap();
    assert_eq!(std::fs::read(out.join("misnamed.z64")).unwrap(), z64);

    // A file with the input's own name is only replaced when asked to be.
    let mut rom = vec![0u8; 64 * 1024];
    rom[0x7FC0..0x7FC0 + 21].copy_from_slice(b"CHDLITE TEST CART    ");
    rom[0x7FD5] = 0x20;
    rom[0x7FD9] = 0x01;
    rom[0x7FDC..0x7FE0].copy_from_slice(&[0x34, 0x12, 0xCB, 0xED]);
    let mut smc = vec![0u8; 512];
    smc.extend_from_slice(&rom);
    let named_sfc = dir.0.join("headered.sfc");
    std::fs::write(&named_sfc, &smc).unwrap();
    let refused = convert_cart(&named_sfc, None, |_| {}).unwrap();
    assert!(refused.contains("over the input itself"), "{refused}");
    assert_eq!(std::fs::read(&named_sfc).unwrap(), smc);
    convert_cart(&named_sfc, None, |s| s.output.force = true).unwrap();
    assert_eq!(std::fs::read(&named_sfc).unwrap(), rom);
    assert_eq!(std::fs::read_dir(&dir.0).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with('.')).count(), 0, "no marks left");
}
