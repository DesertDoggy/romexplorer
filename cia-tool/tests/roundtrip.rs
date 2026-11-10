//! End-to-end test: build an encrypted CIA and CCI from scratch with fake keys
//! (`cia_tool::testkit`, its encryption written independently of the NCCH code), store them
//! under Japanese / Cyrillic file names, decrypt with the library, compare.

use cia_tool::process::{self, Options, Outcome};
use cia_tool::testkit::{make_cia, make_ncch, setup, smdh};
use sha2::{Digest, Sha256};
use std::path::Path;

fn read_cia_contents(path: &Path) -> Vec<Vec<u8>> {
    let mut f = std::fs::File::open(path).unwrap();
    let cia = cia_tool::cia::Cia::open(&mut f).unwrap();
    let b = std::fs::read(path).unwrap();
    cia.contents
        .iter()
        .map(|c| {
            let data = b[c.offset as usize..(c.offset + c.size) as usize].to_vec();
            let rec = &cia.tmd[c.record_pos..c.record_pos + 0x30];
            assert_eq!(rec[7] & 1, 0, "encrypted flag must be cleared");
            assert_eq!(
                &rec[16..48],
                Sha256::digest(&data).as_slice(),
                "TMD hash must match"
            );
            data
        })
        .collect()
}

#[test]
fn cia_with_seed_and_unicode_names() {
    let (mut rng, keys, mut seeds) = setup();
    let tid = 0x0004000000123400u64;
    let seed = rng.key();
    seeds.insert(tid, seed);
    let icon = smdh("ふがふが ゲーム");
    let (e0, x0) = make_ncch(&mut rng, &keys, tid, 0x01, Some(seed), &icon);
    let (e1, x1) = make_ncch(
        &mut rng,
        &keys,
        tid | 0x0010_0000_0000_0001,
        0x00,
        None,
        &icon,
    );
    let cia = make_cia(&mut rng, &keys, tid, &[(0, e0), (1, e1)], &icon);

    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("ほげ Ёлка 🎄.cia");
    std::fs::write(&input, cia).unwrap();

    let opts = Options::default();
    let out = match process::process_file(&input, &opts, &keys, &seeds, &mut |_, _| true).unwrap() {
        Outcome::Written(p) => p,
        o => panic!("unexpected {o:?}"),
    };
    assert_eq!(
        out.file_name().unwrap().to_str().unwrap(),
        "ほげ Ёлка 🎄 Game-decrypted.cia"
    );
    let got = read_cia_contents(&out);
    assert!(got[0] == x0, "content 0 mismatch");
    assert!(got[1] == x1, "content 1 mismatch");

    // Running again on the output reports it as already decrypted.
    let again = process::process_file(
        &out,
        &Options {
            out_dir: Some(dir.path().join("x")),
            ..opts.clone()
        },
        &keys,
        &seeds,
        &mut |_, _| true,
    )
    .unwrap();
    assert!(matches!(again, Outcome::AlreadyDecrypted));

    // Name from the SMDH title, CIA -> CCI.
    let opts = Options {
        to_cci: true,
        name_from_title: true,
        ..Default::default()
    };
    let out = match process::process_file(&input, &opts, &keys, &seeds, &mut |_, _| true).unwrap() {
        Outcome::Written(p) => p,
        o => panic!("unexpected {o:?}"),
    };
    assert_eq!(
        out.file_name().unwrap().to_str().unwrap(),
        "ふがふが ゲーム Game-decrypted.cci"
    );
    let b = std::fs::read(&out).unwrap();
    assert_eq!(&b[0x100..0x104], b"NCSD");
    let p0 = u32::from_le_bytes(b[0x120..0x124].try_into().unwrap()) as usize * 0x200;
    assert!(b[p0..p0 + x0.len()] == x0[..]);
}

#[test]
fn missing_seed_is_reported() {
    let (mut rng, keys, seeds) = setup();
    let tid = 0x0004000000777700u64;
    let s = rng.key();
    let (e0, _) = make_ncch(&mut rng, &keys, tid, 0x00, Some(s), &smdh("x"));
    let cia = make_cia(&mut rng, &keys, tid, &[(0, e0)], &smdh("x"));
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("シード.cia");
    std::fs::write(&input, cia).unwrap();
    let err = process::process_file(&input, &Options::default(), &keys, &seeds, &mut |_, _| true)
        .unwrap_err();
    assert!(format!("{err:#}").contains("seeddb"), "{err:#}");
    // No partial output left behind.
    let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert_eq!(left.len(), 1);
}
