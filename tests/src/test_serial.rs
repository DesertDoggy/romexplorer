//! Serials read from formats no DAT hashes (`rom::serial`), and which of them lenient matching
//! may use (`database::serial_match`), on synthetic files built here: an Xbox 360 disc and an
//! Xbox XISO with made-up boot headers, `.zar` archives with and without the manifest a copy
//! writes, and a `.wua` with a title folder. No game data; the title ids and product codes
//! are placeholders.
//!
//! The archive tests skip when `cemuwua` is not built, and the one reading an archive member
//! when the build predates `cemuwua_read_file`.

use std::path::{Path, PathBuf};

use romexplorer::common::models::Platform;
use romexplorer::database::serial_match::usable_serials;
use romexplorer::database::settings::SerialMatch;
use romexplorer::rom::serial::{self, FoundSerial, SerialOrigin};
use romexplorer::rom::{cemu_wrapper, xbox};

#[path = "support/disc_images.rs"]
mod disc_images;
use disc_images::{pattern, volume, Node, XGD2};

/// A placeholder Xbox 360 title id: "XX" and game 2001.
const X360_ID: u32 = 0x5858_07D1;
/// A placeholder Xbox title id: "XX" and game 4.
const XBOX_ID: u32 = 0x5858_0004;

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("romexplorer_test_serial_{}_{}", std::process::id(), name));
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

/// A XEX2 whose execution info holds `title_id`, followed by filler.
fn xex(title_id: u32) -> Vec<u8> {
    let mut x = vec![0u8; 0x1000];
    x[..4].copy_from_slice(b"XEX2");
    x[0x08..0x0C].copy_from_slice(&0x400u32.to_be_bytes());
    x[0x14..0x18].copy_from_slice(&1u32.to_be_bytes());
    x[0x18..0x1C].copy_from_slice(&0x0004_0006u32.to_be_bytes());
    x[0x1C..0x20].copy_from_slice(&0x100u32.to_be_bytes());
    x[0x10C..0x110].copy_from_slice(&title_id.to_be_bytes());
    x.extend(pattern(50_000, 7));
    x
}

/// An XBE whose certificate holds `title_id`.
fn xbe(title_id: u32) -> Vec<u8> {
    let mut x = vec![0u8; 0x1000];
    x[..4].copy_from_slice(b"XBEH");
    x[0x104..0x108].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    x[0x108..0x10C].copy_from_slice(&0x1000u32.to_le_bytes());
    x[0x118..0x11C].copy_from_slice(&0x0001_0180u32.to_le_bytes());
    x[0x188..0x18C].copy_from_slice(&title_id.to_le_bytes());
    x.extend(pattern(50_000, 8));
    x
}

/// A disc image with the volume `root` at `base`.
fn image(path: &Path, base: u64, root: &[Node]) {
    use std::io::{Seek, SeekFrom, Write};
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(&pattern(64 * 1024, 9)).unwrap();
    f.seek(SeekFrom::Start(base)).unwrap();
    let v = volume(root);
    f.write_all(&v).unwrap();
    f.set_len(base + v.len() as u64 + 64 * 1024).unwrap();
}

fn header(serials: &[&str], origin: SerialOrigin) -> Vec<FoundSerial> {
    serials.iter().map(|s| FoundSerial { serial: s.to_string(), origin }).collect()
}

#[test]
fn an_xbox_360_disc_and_an_xbox_xiso_give_their_title_ids() {
    let dir = TempDir::new("discs");
    let x360 = dir.0.join("game.iso");
    image(&x360, XGD2, &[Node::File("default.xex", xex(X360_ID)), Node::File("z.dat", pattern(5000, 3))]);
    assert_eq!(serial::read(&x360, Platform::Xbox360), header(&["XX-2001", "585807D1"], SerialOrigin::Header));

    let xiso = dir.0.join("game.xiso");
    image(&xiso, 0, &[Node::Dir("media", vec![Node::File("a.bin", pattern(4000, 2))]), Node::File("default.xbe", xbe(XBOX_ID))]);
    assert_eq!(serial::read(&xiso, Platform::Xbox), header(&["XX-004", "58580004"], SerialOrigin::Header));

    // A boot file that is neither gives nothing, and so does a file that is no disc.
    let junk = dir.0.join("junk.xiso");
    image(&junk, 0, &[Node::File("default.xbe", pattern(9000, 4))]);
    assert!(serial::read(&junk, Platform::Xbox).is_empty());
    let plain = dir.0.join("plain.xiso");
    std::fs::write(&plain, pattern(1 << 16, 5)).unwrap();
    assert!(serial::read(&plain, Platform::Xbox).is_empty());
}

#[test]
fn a_zar_this_app_wrote_records_its_title_id_in_the_manifest() {
    if !xbox::zar_available() {
        eprintln!("skipped: cemuwua is not built");
        return;
    }
    let dir = TempDir::new("zar_manifest");
    let iso = dir.0.join("game.iso");
    let zar = dir.0.join("game.zar");
    image(&iso, XGD2, &[Node::File("default.xex", xex(X360_ID)), Node::File("z.dat", pattern(5000, 3))]);
    let written = xbox::create_zar(&iso, &zar, |_, _| serde_json::Value::Null, |_, _| true).unwrap();
    assert_eq!(written.manifest.titles[0].title_id, "585807D1");
    assert_eq!(serial::read(&zar, Platform::Xbox360), header(&["XX-2001", "585807D1"], SerialOrigin::Manifest));
}

#[test]
fn a_zar_from_elsewhere_is_read_from_its_default_xex() {
    if !xbox::zar_available() {
        eprintln!("skipped: cemuwua is not built");
        return;
    }
    let dir = TempDir::new("zar_header");
    let zar = dir.0.join("game.zar");
    let mut w = cemu_wrapper::zar_create(&zar).unwrap();
    w.file("default.xex").unwrap();
    w.write(&xex(X360_ID)).unwrap();
    w.finish(None).unwrap();
    if cemu_wrapper::read_file(&zar, "default.xex", 4).unwrap().is_none() {
        eprintln!("skipped: this cemuwua build has no cemuwua_read_file; rebuild it");
        return;
    }
    assert_eq!(cemu_wrapper::read_file(&zar, "DEFAULT.XEX", 4).unwrap().as_deref(), Some(&b"XEX2"[..]), "names are found whatever their case");
    assert_eq!(cemu_wrapper::read_file(&zar, "missing.xex", 4).unwrap(), None);
    assert_eq!(serial::read(&zar, Platform::Xbox360), header(&["XX-2001", "585807D1"], SerialOrigin::Header));
}

#[test]
fn a_wua_gives_its_product_code_and_title_id() {
    if !xbox::zar_available() {
        eprintln!("skipped: cemuwua is not built");
        return;
    }
    let dir = TempDir::new("wua");
    let wua = dir.0.join("game.wua");
    let folder = "0005000010ffff00_v16";
    let mut w = cemu_wrapper::zar_create(&wua).unwrap();
    w.dir(&format!("{folder}/meta")).unwrap();
    w.dir(&format!("{folder}/code")).unwrap();
    w.file(&format!("{folder}/meta/meta.xml")).unwrap();
    w.write(b"<?xml version=\"1.0\"?><menu><product_code type=\"string\" length=\"32\">WUP-P-XXXX</product_code></menu>").unwrap();
    w.file(&format!("{folder}/code/app.xml")).unwrap();
    w.write(b"<app></app>").unwrap();
    w.finish(None).unwrap();
    if cemu_wrapper::read_file(&wua, &format!("{folder}/meta/meta.xml"), 4).unwrap().is_none() {
        eprintln!("skipped: this cemuwua build has no cemuwua_read_file; rebuild it");
        return;
    }
    assert_eq!(serial::read(&wua, Platform::WiiU), header(&["WUP-P-XXXX", "0005000010FFFF00"], SerialOrigin::Header));
}

#[test]
fn the_setting_decides_which_serials_may_be_used() {
    let dir = TempDir::new("setting");
    let xiso = dir.0.join("Game (USA) (XX-007).xiso");
    image(&xiso, 0, &[Node::File("default.xbe", xbe(XBOX_ID))]);
    let all = SerialMatch { formats: vec!["cia", "xiso", "wua", "zar"], manifest: true, file_name: true, file_name_platforms: Default::default() };
    let got: Vec<String> = usable_serials(&xiso, Platform::Xbox, &all).into_iter().map(|f| f.serial).collect();
    assert_eq!(got, ["XX-004", "58580004", "XX-007"], "header serials before the file name's");

    let no_names = SerialMatch { file_name: false, ..all.clone() };
    assert_eq!(usable_serials(&xiso, Platform::Xbox, &no_names).len(), 2);
    let no_xiso = SerialMatch { formats: vec!["cia", "wua", "zar"], ..all.clone() };
    assert!(usable_serials(&xiso, Platform::Xbox, &no_xiso).is_empty());
    // An unknown platform is taken from the format.
    assert_eq!(usable_serials(&xiso, Platform::Unknown, &no_names).len(), 2);

    assert_eq!(SerialMatch::from_value(&serde_json::Value::Null), SerialMatch { formats: vec!["cia", "3ds", "xiso", "wua", "zar", "vpk", "vita_folder", "nsp"], manifest: true, file_name: false, file_name_platforms: Default::default() });
    // Per platform: a platform's own choice wins, the rest follow file_name.
    let per = SerialMatch::from_value(&serde_json::json!({ "file_name": true, "file_name_platforms": { "41": false, "12": true } }));
    assert!(!per.file_name_for(romexplorer::common::models::Platform::WiiU));
    assert!(per.file_name_for(romexplorer::common::models::Platform::Xbox360));
    assert!(per.file_name_for(romexplorer::common::models::Platform::Nintendo3ds), "not listed: the default");
    // ...and it is what usable_serials goes by: Xbox off here although the default is on.
    let xbox_off = SerialMatch { file_name_platforms: [(Platform::Xbox.igdb_id(), false)].into_iter().collect(), ..all.clone() };
    assert_eq!(usable_serials(&xiso, Platform::Xbox, &xbox_off).len(), 2);
    assert_eq!(SerialMatch::from_value(&serde_json::json!({"zar": false, "file_name": true})).formats, ["cia", "3ds", "xiso", "wua", "vpk", "vita_folder", "nsp"]);
    assert_eq!(SerialMatch::from_value(&serde_json::json!({"pkg": true, "ps_disc": true})).formats, ["cia", "3ds", "xiso", "wua", "zar", "vpk", "vita_folder", "nsp", "pkg", "ps_disc"]);
}

/// A PARAM.SFO with text entries only (format 0x0204), as a Vita or PS3 game folder has.
fn sfo(entries: &[(&str, &str)]) -> Vec<u8> {
    let (mut keys, mut data, mut index) = (Vec::new(), Vec::new(), Vec::new());
    for (key, value) in entries {
        let bytes = [value.as_bytes(), &[0]].concat();
        index.extend_from_slice(&(keys.len() as u16).to_le_bytes());
        index.extend_from_slice(&0x0204u16.to_le_bytes());
        index.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        index.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        index.extend_from_slice(&(data.len() as u32).to_le_bytes());
        keys.extend_from_slice(key.as_bytes());
        keys.push(0);
        data.extend_from_slice(&bytes);
    }
    while keys.len() % 4 != 0 {
        keys.push(0);
    }
    let key_table = 0x14 + index.len();
    let mut out = b"\0PSF".to_vec();
    out.extend_from_slice(&0x0101u32.to_le_bytes());
    out.extend_from_slice(&(key_table as u32).to_le_bytes());
    out.extend_from_slice(&((key_table + keys.len()) as u32).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    out.extend_from_slice(&index);
    out.extend_from_slice(&keys);
    out.extend_from_slice(&data);
    out
}

#[test]
fn vita_and_ps3_game_folders_give_their_title_id_when_the_setting_allows() {
    let dir = TempDir::new("sony_folders");
    let vita = dir.0.join("TEST TITLE/sce_sys/param.sfo");
    let ps3 = dir.0.join("TEST TITLE PS3/PS3_GAME/PARAM.SFO");
    for (path, id, category) in [(&vita, "ABCD00001", "gd"), (&ps3, "ABCD00002", "DG")] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, sfo(&[("CATEGORY", category), ("TITLE", "TEST TITLE"), ("TITLE_ID", id)])).unwrap();
    }
    let defaults = SerialMatch::from_value(&serde_json::Value::Null);
    assert_eq!(usable_serials(&vita, Platform::PlayStationVita, &defaults), header(&["ABCD00001"], SerialOrigin::Header));
    assert!(usable_serials(&ps3, Platform::PlayStation3, &defaults).is_empty(), "PlayStation discs are off by default");
    let discs = SerialMatch::from_value(&serde_json::json!({"ps_disc": true}));
    assert_eq!(usable_serials(&ps3, Platform::PlayStation3, &discs), header(&["ABCD00002"], SerialOrigin::Header));
}
