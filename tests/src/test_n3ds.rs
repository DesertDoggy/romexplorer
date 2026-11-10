//! 3DS conversions end to end (`rom::n3ds`): encrypted CIAs made from scratch with fake keys
//! (`cia_tool::testkit`), turned into every output, and checked against the plain bytes.

use std::path::{Path, PathBuf};

use cia_tool::testkit::{aes_keys_text, make_cia, make_ncch, seeddb_bytes, setup, smdh};
use romexplorer::rom::n3ds::{self, Container, Missing, Outcome, Target};

const GAME: u64 = 0x0004_0000_0012_3400;
const UPDATE: u64 = 0x0004_000E_0012_3400;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("romexplorer_test_n3ds_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A game CIA (main part + manual), its expected plain NCCHs, and the key files as bytes.
struct Fixture {
    cia: Vec<u8>,
    main: Vec<u8>,
    aes_keys: String,
    seeddb: Vec<u8>,
}

fn fixture(tid: u64, seeded: bool) -> Fixture {
    let (mut rng, keys, _) = setup();
    let icon = smdh("テスト");
    let seed = seeded.then(|| rng.key());
    let (e0, x0) = make_ncch(&mut rng, &keys, tid, 0x00, seed, &icon);
    let (e1, _) = make_ncch(&mut rng, &keys, tid | 0x0010_0000_0000_0001, 0x00, None, &icon);
    let contents = if (tid >> 32) as u32 == 0x0004_0000 { vec![(0, e0), (1, e1)] } else { vec![(0, e0)] };
    let cia = make_cia(&mut rng, &keys, tid, &contents, &icon);
    Fixture { cia, main: x0, aes_keys: aes_keys_text(&keys), seeddb: seed.map(|s| seeddb_bytes(&[(tid, s)])).unwrap_or_default() }
}

fn keys_for(f: &Fixture) -> n3ds::Keys {
    let mut keys = n3ds::Keys::default();
    keys.add_bytes(Some(f.aes_keys.as_bytes()), (!f.seeddb.is_empty()).then_some(f.seeddb.as_slice())).unwrap();
    keys
}

fn partition0(image: &[u8]) -> &[u8] {
    assert_eq!(&image[0x100..0x104], b"NCSD");
    let at = u32::from_le_bytes(image[0x120..0x124].try_into().unwrap()) as usize * 0x200;
    let len = u32::from_le_bytes(image[0x124..0x128].try_into().unwrap()) as usize * 0x200;
    &image[at..at + len]
}

fn convert(src: &Path, dir: &Path, target: Target, keys: &n3ds::Keys) -> PathBuf {
    let info = n3ds::inspect(src).unwrap();
    let dst = dir.join(format!("out_{}.{}", target.name(), n3ds::output_extension(&info, target)));
    n3ds::convert(src, &dst, target, keys, &mut |_, _| true).unwrap();
    dst
}

#[test]
fn a_game_becomes_each_output_and_the_compressed_ones_unpack_to_the_same_bytes() {
    let dir = scratch("game");
    let f = fixture(GAME, false);
    let src = dir.join("ゲーム.cia");
    std::fs::write(&src, &f.cia).unwrap();
    let info = n3ds::inspect(&src).unwrap();
    assert!(info.encrypted && info.is_game());

    // Without keys the check says so; with them, nothing is missing.
    assert!(matches!(n3ds::check(&src, &n3ds::Keys::default()).unwrap(), Some(Missing::Keys(_))));
    let keys = keys_for(&f);
    assert_eq!(n3ds::check(&src, &keys).unwrap(), None);

    let cci = std::fs::read(convert(&src, &dir, Target::Cci, &keys)).unwrap();
    assert_eq!(&partition0(&cci)[..f.main.len()], &f.main[..]);
    let three_ds = std::fs::read(convert(&src, &dir, Target::ThreeDs, &keys)).unwrap();
    assert_eq!(cci, three_ds, ".3ds is the same image as .cci");

    let zcci = convert(&src, &dir, Target::Zcci, &keys);
    assert_eq!(&std::fs::read(&zcci).unwrap()[0..8], b"Z3DSNCSD");
    let zinfo = n3ds::inspect(&zcci).unwrap();
    assert_eq!((zinfo.container, zinfo.compressed, zinfo.encrypted), (Container::Cci, true, false));
    // Unpacked again (no keys needed), it is the .cci byte for byte.
    let back = dir.join("back.cci");
    assert_eq!(n3ds::convert(&zcci, &back, Target::Cci, &n3ds::Keys::default(), &mut |_, _| true).unwrap(), Outcome::AlreadyDecrypted);
    assert_eq!(std::fs::read(&back).unwrap(), cci);

    let cxi = std::fs::read(convert(&src, &dir, Target::Cxi, &keys)).unwrap();
    assert_eq!(&cxi[..], partition0(&cci));
    let zcxi = convert(&src, &dir, Target::Zcxi, &keys);
    assert_eq!(&std::fs::read(&zcxi).unwrap()[0..8], b"Z3DSNCCH");
    let back = dir.join("back.cxi");
    n3ds::z3ds_decompress(&zcxi, &back, &mut |_, _| true).unwrap();
    assert_eq!(std::fs::read(&back).unwrap(), cxi);

    // The decrypted image converts again without keys.
    let plain = dir.join("plain.cci");
    std::fs::write(&plain, &cci).unwrap();
    assert!(!n3ds::inspect(&plain).unwrap().encrypted);
    assert_eq!(n3ds::check(&plain, &n3ds::Keys::default()).unwrap(), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_update_stays_a_cia_and_compresses_as_zcia() {
    let dir = scratch("update");
    let f = fixture(UPDATE, false);
    let src = dir.join("update.cia");
    std::fs::write(&src, &f.cia).unwrap();
    let keys = keys_for(&f);
    let info = n3ds::inspect(&src).unwrap();
    assert_eq!(info.title_kind, "Patch");
    assert_eq!(n3ds::output_extension(&info, Target::Cci), "cia");

    let cia = convert(&src, &dir, Target::Cci, &keys);
    assert_eq!(cia.extension().unwrap(), "cia");
    assert!(!n3ds::inspect(&cia).unwrap().encrypted);
    let zcia = convert(&src, &dir, Target::Zcci, &keys);
    assert_eq!(zcia.extension().unwrap(), "zcia");
    assert_eq!(&std::fs::read(&zcia).unwrap()[0..8], b"Z3DSCIA\0");
    // The title id is in the metadata, so a .zcia is known for an update without unpacking.
    assert_eq!(n3ds::inspect(&zcia).unwrap().title_kind, "Patch");
    let back = dir.join("back.cia");
    n3ds::z3ds_decompress(&zcia, &back, &mut |_, _| true).unwrap();
    assert_eq!(std::fs::read(&back).unwrap(), std::fs::read(&cia).unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_seeded_title_asks_for_seeddb_until_it_is_given() {
    let dir = scratch("seed");
    let f = fixture(GAME, true);
    let src = dir.join("seeded.cia");
    std::fs::write(&src, &f.cia).unwrap();
    let mut keys = n3ds::Keys::default();
    keys.add_bytes(Some(f.aes_keys.as_bytes()), None).unwrap();
    assert_eq!(n3ds::check(&src, &keys).unwrap(), Some(Missing::Seed(GAME)));
    keys.add_bytes(None, Some(&f.seeddb)).unwrap();
    assert_eq!(n3ds::check(&src, &keys).unwrap(), None);
    let cci = std::fs::read(convert(&src, &dir, Target::Cci, &keys)).unwrap();
    assert_eq!(&partition0(&cci)[..f.main.len()], &f.main[..]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_cancelled_conversion_stops() {
    let dir = scratch("cancel");
    let f = fixture(GAME, false);
    let src = dir.join("game.cia");
    std::fs::write(&src, &f.cia).unwrap();
    let err = n3ds::convert(&src, &dir.join("out.zcci"), Target::Zcci, &keys_for(&f), &mut |_, _| false).unwrap_err();
    assert_eq!(err, "cancelled");
    // Only the source and the half-written output the caller would remove: no scratch files.
    let left: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with('.')).collect();
    assert!(left.is_empty(), "{left:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn key_files_are_found_beside_the_input_or_in_an_assets_folder() {
    let dir = scratch("find");
    let f = fixture(GAME, true);
    let games = dir.join("games");
    let app = dir.join("app");
    std::fs::create_dir_all(&games).unwrap();
    std::fs::create_dir_all(app.join("assets")).unwrap();
    let src = games.join("seeded.cia");
    std::fs::write(&src, &f.cia).unwrap();
    std::fs::write(games.join("aes_keys.txt"), &f.aes_keys).unwrap();
    std::fs::write(app.join("assets").join("seeddb.bin"), &f.seeddb).unwrap();
    let dirs = n3ds::key_dirs(&src, std::slice::from_ref(&app));
    let found = n3ds::KeyFiles::find(&dirs);
    assert_eq!(found.aes_keys, Some(games.join("aes_keys.txt")));
    assert_eq!(found.seeddb, Some(app.join("assets").join("seeddb.bin")));
    let keys = n3ds::Keys::load(&found).unwrap();
    assert_eq!(n3ds::check(&src, &keys).unwrap(), None);
    let _ = std::fs::remove_dir_all(&dir);
}
