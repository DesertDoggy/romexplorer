//! Wii U disc images into `.wua` through `cemuwua` (`rom::cemu_wrapper`), on a synthetic disc
//! built here: no game data, but laid out and encrypted the way a real `.wud` is, so it goes
//! through Cemu's own partition, ticket and FST code exactly as a dump would.
//!
//! The disc's title key is encrypted with the Wii U common key, as on a real disc. That key is
//! not in this repository: it is read at test time from Cemu's source (`external/cemu`), and
//! the tests skip when it, or the built library, is not there.
//!
//! The layout (0x8000-byte sectors):
//!
//! | sector | what |
//! |---|---|
//! | 0 | header A (product code) |
//! | 2 | header B (magic) |
//! | 3 | partition table, encrypted with the disc key |
//! | 4.. | SI partition: header, FST, then `01/title.tik` |
//! | 16.. | GM partition: header, FST, then the title's files as one unhashed cluster |

use std::path::{Path, PathBuf};

use aes::cipher::{BlockCipherEncrypt, KeyInit};
use romexplorer::rom::cemu_wrapper::{self, Source, Status, Verified};
use sha1::{Digest, Sha1};

const SECTOR: usize = 0x8000;
const TITLE_ID: u64 = 0x0005_0000_10FF_FF00;
const TITLE_VERSION: u32 = 16;
const DISC_KEY: [u8; 16] = *b"romexplorer-disc";
const TITLE_KEY: [u8; 16] = *b"romexplorer-titl";
const OFFSET_FACTOR: usize = 0x20;

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("romexplorer_test_wiiu_{}_{}", std::process::id(), name));
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

/// The Wii U common key, from Cemu's ticket code, or `None` when Cemu's source isn't checked out.
fn common_key() -> Option<[u8; 16]> {
    let src = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("external/cemu/src/Cemu/ncrypto/ncrypto.cpp")).ok()?;
    let line = src.lines().find(|l| l.contains("commonKey[16]"))?;
    let body = line.split('{').nth(1)?.split('}').next()?;
    let bytes: Vec<u8> = body.split(',').map(|b| u8::from_str_radix(b.trim().trim_start_matches("0x"), 16)).collect::<Result<_, _>>().ok()?;
    bytes.try_into().ok()
}

/// Both the library and the key, or a reason to skip.
fn ready() -> Option<[u8; 16]> {
    if !cemu_wrapper::available() {
        eprintln!("SKIP: cemuwua is not built (deps/cemu/scripts/build.sh)");
        return None;
    }
    let key = common_key();
    if key.is_none() {
        eprintln!("SKIP: external/cemu is not checked out, so there is no common key to build a disc with");
    }
    key
}

fn cbc_encrypt(key: &[u8; 16], iv: [u8; 16], data: &mut [u8]) {
    assert_eq!(data.len() % 16, 0);
    let cipher = aes::Aes128::new_from_slice(key).unwrap();
    let mut prev = iv;
    for chunk in data.chunks_mut(16) {
        for (b, p) in chunk.iter_mut().zip(prev) {
            *b ^= p;
        }
        let block: &mut aes::Block = chunk.try_into().unwrap();
        cipher.encrypt_block(block);
        prev.copy_from_slice(chunk);
    }
}

/// A repeatable pattern that compresses a little, like real data does.
fn pattern(len: usize, seed: u32) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x & 0x3f) as u8
        })
        .collect()
}

enum Node {
    Dir(&'static str, Vec<Node>),
    File(&'static str, Vec<u8>),
}

/// The files of one title, as `(path, bytes)` in FST order.
fn flatten(nodes: &[Node], prefix: &str, out: &mut Vec<(String, Vec<u8>)>) {
    for n in nodes {
        match n {
            Node::Dir(name, children) => flatten(children, &format!("{prefix}{name}/"), out),
            Node::File(name, data) => out.push((format!("{prefix}{name}"), data.clone())),
        }
    }
}

/// An FST and the one data cluster its files live in, plain. Cluster 0 is the FST itself at
/// partition sector 1, cluster 1 the files from sector 2 on.
fn build_fst(root: &[Node]) -> (Vec<u8>, Vec<u8>) {
    struct Entry {
        dir: bool,
        name_offset: u32,
        a: u32,
        b: u32,
    }
    let mut entries = vec![Entry { dir: true, name_offset: 0, a: 0, b: 0 }];
    let mut names = vec![0u8];
    let mut data = Vec::new();
    fn walk(nodes: &[Node], parent: u32, entries: &mut Vec<Entry>, names: &mut Vec<u8>, data: &mut Vec<u8>) {
        for n in nodes {
            let name_offset = names.len() as u32;
            let name = match n {
                Node::Dir(name, _) | Node::File(name, _) => name,
            };
            names.extend_from_slice(name.as_bytes());
            names.push(0);
            match n {
                Node::Dir(_, children) => {
                    let index = entries.len();
                    entries.push(Entry { dir: true, name_offset, a: parent, b: 0 });
                    walk(children, index as u32, entries, names, data);
                    entries[index].b = entries.len() as u32;
                }
                Node::File(_, bytes) => {
                    while data.len() % OFFSET_FACTOR != 0 {
                        data.push(0);
                    }
                    entries.push(Entry { dir: false, name_offset, a: (data.len() / OFFSET_FACTOR) as u32, b: bytes.len() as u32 });
                    data.extend_from_slice(bytes);
                }
            }
        }
    }
    walk(root, 0, &mut entries, &mut names, &mut data);
    entries[0].b = entries.len() as u32;
    data.resize(data.len().div_ceil(SECTOR).max(1) * SECTOR, 0);

    let mut fst = Vec::new();
    fst.extend_from_slice(&0x4653_5400u32.to_be_bytes());
    fst.extend_from_slice(&(OFFSET_FACTOR as u32).to_be_bytes());
    fst.extend_from_slice(&2u32.to_be_bytes());
    fst.push(1); // hashes disabled
    fst.resize(0x20, 0);
    for (offset, size) in [(1u32, SECTOR as u32), (2, data.len() as u32)] {
        let mut c = [0u8; 0x20];
        c[0..4].copy_from_slice(&offset.to_be_bytes());
        c[4..8].copy_from_slice(&size.to_be_bytes());
        c[8..16].copy_from_slice(&TITLE_ID.to_be_bytes());
        // hashMode 0 at +0x14: raw, unhashed.
        fst.extend_from_slice(&c);
    }
    for e in &entries {
        fst.extend_from_slice(&((u32::from(e.dir) << 24) | e.name_offset).to_be_bytes());
        fst.extend_from_slice(&e.a.to_be_bytes());
        fst.extend_from_slice(&e.b.to_be_bytes());
        fst.extend_from_slice(&0u16.to_be_bytes());
        fst.extend_from_slice(&u16::from(!e.dir).to_be_bytes()); // files in cluster 1
    }
    fst.extend_from_slice(&names);
    (fst, data)
}

/// One partition: plain header, FST encrypted at sector 1 (IV 0), files encrypted as one
/// CBC stream from sector 2 (IV = the cluster index).
fn build_partition(root: &[Node], key: &[u8; 16], encryption_type: u8) -> Vec<u8> {
    let (fst, mut data) = build_fst(root);
    assert!(fst.len() <= SECTOR);
    let mut part = vec![0u8; SECTOR];
    part[0..4].copy_from_slice(&0xCC93_A4F5u32.to_be_bytes());
    part[4..8].copy_from_slice(&(SECTOR as u32).to_be_bytes());
    part[0x14..0x18].copy_from_slice(&(fst.len() as u32).to_be_bytes());
    part[0x18..0x1C].copy_from_slice(&1u32.to_be_bytes());
    part[0x24] = 0; // FST hash type: unhashed
    part[0x25] = encryption_type;
    let mut fst_sector = fst.clone();
    fst_sector.resize(SECTOR, 0);
    cbc_encrypt(key, [0; 16], &mut fst_sector);
    part.extend_from_slice(&fst_sector);
    let mut iv = [0u8; 16];
    iv[1] = 1; // cluster 1
    cbc_encrypt(key, iv, &mut data);
    part.extend_from_slice(&data);
    part
}

fn ticket(common_key: &[u8; 16]) -> Vec<u8> {
    let mut t = vec![0u8; 0x350];
    t[0..4].copy_from_slice(&0x0001_0004u32.to_be_bytes());
    t[0x1BC] = 0; // format version 0
    let mut iv = [0u8; 16];
    iv[..8].copy_from_slice(&TITLE_ID.to_be_bytes());
    let mut key = TITLE_KEY;
    cbc_encrypt(common_key, iv, &mut key);
    t[0x1BF..0x1CF].copy_from_slice(&key);
    t[0x1DC..0x1E4].copy_from_slice(&TITLE_ID.to_be_bytes());
    t[0x1E6..0x1E8].copy_from_slice(&(TITLE_VERSION as u16).to_be_bytes());
    t
}

fn app_xml() -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<app type=\"complex\" access=\"777\">\n  <version type=\"unsignedInt\" length=\"4\">16</version>\n  <title_id type=\"hexBinary\" length=\"8\">{TITLE_ID:016X}</title_id>\n  <title_version type=\"hexBinary\" length=\"2\">{TITLE_VERSION:04X}</title_version>\n</app>\n"
    )
    .into_bytes()
}

/// The title's files: what a `.wua` made from the disc must hold, byte for byte.
fn title_tree() -> Vec<Node> {
    vec![
        Node::Dir("code", vec![Node::File("app.xml", app_xml()), Node::File("dummy.rpx", pattern(70_000, 7))]),
        Node::Dir("content", vec![Node::File("data.bin", pattern(150_000, 11)), Node::Dir("sub", vec![Node::File("small.txt", b"hello from a dummy disc\n".to_vec()), Node::File("empty.bin", Vec::new())])]),
        Node::Dir("meta", vec![Node::File("meta.xml", b"<?xml version=\"1.0\"?><menu><product_code>WUP-P-RXEE</product_code></menu>\n".to_vec())]),
    ]
}

fn title_files() -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    flatten(&title_tree(), "", &mut out);
    out
}

/// A whole `.wud`.
fn build_wud(common_key: &[u8; 16]) -> Vec<u8> {
    const SI_SECTOR: usize = 4;
    const GM_SECTOR: usize = 16;
    let si = build_partition(&[Node::Dir("01", vec![Node::File("title.tik", ticket(common_key))])], &DISC_KEY, 1);
    let gm = build_partition(&title_tree(), &TITLE_KEY, 2);
    assert!(SI_SECTOR * SECTOR + si.len() <= GM_SECTOR * SECTOR);

    let mut disc = vec![0u8; GM_SECTOR * SECTOR + gm.len()];
    disc[0..10].copy_from_slice(b"WUP-P-RXEE");
    disc[2 * SECTOR..2 * SECTOR + 4].copy_from_slice(&0xCC54_9EB9u32.to_be_bytes());
    let mut table = vec![0u8; SECTOR];
    table[0..4].copy_from_slice(&0xCCA6_E67Bu32.to_be_bytes());
    table[4..8].copy_from_slice(&(SECTOR as u32).to_be_bytes());
    table[0x1C..0x20].copy_from_slice(&2u32.to_be_bytes());
    for (i, (name, sector)) in [("SI", SI_SECTOR), (&*format!("GM{TITLE_ID:016X}"), GM_SECTOR)].into_iter().enumerate() {
        let e = 0x800 + i * 0x80;
        table[e..e + name.len()].copy_from_slice(name.as_bytes());
        table[e + 0x1F] = 1;
        table[e + 0x20..e + 0x24].copy_from_slice(&(sector as u32).to_be_bytes());
    }
    cbc_encrypt(&DISC_KEY, [0; 16], &mut table);
    disc[3 * SECTOR..4 * SECTOR].copy_from_slice(&table);
    disc[SI_SECTOR * SECTOR..SI_SECTOR * SECTOR + si.len()].copy_from_slice(&si);
    disc[GM_SECTOR * SECTOR..].copy_from_slice(&gm);
    disc
}

/// The same disc as a `.wux`: repeated sectors stored once.
fn to_wux(wud: &[u8]) -> Vec<u8> {
    let count = wud.len().div_ceil(SECTOR);
    let mut unique: Vec<&[u8]> = Vec::new();
    let mut index = Vec::with_capacity(count);
    for s in wud.chunks(SECTOR) {
        let i = unique.iter().position(|u| *u == s).unwrap_or_else(|| {
            unique.push(s);
            unique.len() - 1
        });
        index.push(i as u32);
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"WUX0");
    out.extend_from_slice(&0x1099_d02eu32.to_le_bytes());
    out.extend_from_slice(&(SECTOR as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // alignment for the u64
    out.extend_from_slice(&(wud.len() as u64).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // flags
    out.extend_from_slice(&0u32.to_le_bytes());
    for i in &index {
        out.extend_from_slice(&i.to_le_bytes());
    }
    out.resize(out.len().div_ceil(SECTOR) * SECTOR, 0);
    for s in unique {
        out.extend_from_slice(s);
        out.resize(out.len().div_ceil(SECTOR) * SECTOR, 0);
    }
    out
}

fn sha1_hex(data: &[u8]) -> String {
    Sha1::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

fn folder() -> String {
    format!("{TITLE_ID:016x}_v{TITLE_VERSION}")
}

/// Every file streamed out of a `.wua`, whole.
fn streamed(wua: &Path) -> Vec<(String, Vec<u8>)> {
    let files = std::cell::RefCell::new(Vec::<(String, Vec<u8>)>::new());
    cemu_wrapper::stream(
        wua,
        |path, size| {
            files.borrow_mut().push((path.to_string(), Vec::with_capacity(size as usize)));
            true
        },
        |chunk| {
            files.borrow_mut().last_mut().unwrap().1.extend_from_slice(chunk);
            true
        },
    )
    .unwrap();
    files.into_inner()
}

/// What a `.wua` made from the dummy disc has to hold: every file, the manifest listing each
/// with its SHA-1 and the source record, and a verify that passes.
fn check_archive(wua: &Path, source_name: &str) {
    assert_eq!(cemu_wrapper::wua_titles(wua).unwrap(), vec![(TITLE_ID, TITLE_VERSION)]);
    let expected = title_files();
    let got = streamed(wua);
    let want: Vec<(String, Vec<u8>)> = expected.iter().map(|(p, d)| (format!("{}/{p}", folder()), d.clone())).collect();
    let mut got_sorted = got.clone();
    got_sorted.sort();
    let mut want_sorted = want.clone();
    want_sorted.sort();
    assert_eq!(got_sorted, want_sorted, "the archive holds exactly the title's files");

    let manifest = cemu_wrapper::manifest(wua).unwrap().expect("a manifest");
    assert_eq!(manifest.tool, "cemuwua");
    assert_eq!(manifest.titles.len(), 1);
    assert_eq!(manifest.titles[0].folder, folder());
    assert_eq!(manifest.titles[0].title_id, format!("{TITLE_ID:016x}"));
    assert_eq!(manifest.titles[0].version, TITLE_VERSION);
    for (path, data) in &expected {
        let f = manifest.titles[0].files.iter().find(|f| &f.path == path).unwrap_or_else(|| panic!("{path} is in the manifest"));
        assert_eq!(f.size, data.len() as u64, "{path}");
        assert_eq!(f.sha1, sha1_hex(data), "{path}");
    }
    assert_eq!(manifest.source["name"], source_name);

    let mut last = (0, 0);
    match cemu_wrapper::verify(wua, |d, t| {
        last = (d, t);
        true
    })
    .unwrap()
    {
        Verified::Intact(m) => assert_eq!(m, manifest),
        other => panic!("verify: {other:?}"),
    }
    let total: u64 = expected.iter().map(|(_, d)| d.len() as u64).sum();
    assert_eq!(last, (total, total));
}

fn source_json(name: &str) -> serde_json::Value {
    serde_json::to_value(cemu_wrapper::SourceRecord { name: name.into(), size: Some(1), sha1: Some("00".repeat(20)), verified: true, ..Default::default() }).unwrap()
}

#[test]
fn a_wud_with_its_key_file_beside_it_becomes_a_wua() {
    let Some(ck) = ready() else { return };
    let dir = TempDir::new("wud");
    let wud = dir.0.join("Dummy (USA).wud");
    std::fs::write(&wud, build_wud(&ck)).unwrap();
    std::fs::write(dir.0.join("Dummy (USA).key"), DISC_KEY).unwrap();

    assert_eq!(cemu_wrapper::title_info(&Source::new(&wud), None).unwrap(), (TITLE_ID, TITLE_VERSION));
    let out = dir.0.join("Dummy (USA).wua");
    let mut calls = 0;
    cemu_wrapper::create(&[Source::new(&wud)], None, Some(&source_json("Dummy (USA).wud")), &out, |_, _| {
        calls += 1;
        true
    })
    .unwrap();
    assert!(calls > 0, "progress was reported");
    check_archive(&out, "Dummy (USA).wud");
}

#[test]
fn a_wux_with_an_explicit_key_holds_the_same_files_as_its_wud() {
    let Some(ck) = ready() else { return };
    let dir = TempDir::new("wux");
    let wud_bytes = build_wud(&ck);
    let wux_bytes = to_wux(&wud_bytes);
    assert!(wux_bytes.len() < wud_bytes.len(), "the empty sectors fold");
    let wux = dir.0.join("Dummy (USA).wux");
    std::fs::write(&wux, &wux_bytes).unwrap();
    let out = dir.0.join("Dummy (USA).wua");
    cemu_wrapper::create(&[Source { path: wux.clone(), key: Some(DISC_KEY) }], None, Some(&source_json("Dummy (USA).wux")), &out, |_, _| true).unwrap();
    check_archive(&out, "Dummy (USA).wux");
}

#[test]
fn a_key_from_keys_txt_is_found_too() {
    let Some(ck) = ready() else { return };
    let dir = TempDir::new("keystxt");
    let wud = dir.0.join("game.wud");
    std::fs::write(&wud, build_wud(&ck)).unwrap();
    let keys = dir.0.join("keys.txt");
    let hex: String = DISC_KEY.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(&keys, format!("# a comment\n{}\n{hex} # Dummy\n", "11".repeat(16))).unwrap();
    assert_eq!(cemu_wrapper::title_info(&Source::new(&wud), Some(&keys)).unwrap(), (TITLE_ID, TITLE_VERSION));
}

#[test]
fn a_wrong_or_missing_key_is_a_key_error_and_writes_nothing() {
    let Some(ck) = ready() else { return };
    let dir = TempDir::new("badkey");
    let wud = dir.0.join("game.wud");
    std::fs::write(&wud, build_wud(&ck)).unwrap();
    let out = dir.0.join("game.wua");
    let err = cemu_wrapper::create(&[Source { path: wud.clone(), key: Some([7; 16]) }], None, None, &out, |_, _| true).unwrap_err();
    assert_eq!(err.status, Status::Key, "{err}");
    assert!(!out.exists());
    let err = cemu_wrapper::create(&[Source::new(&wud)], None, None, &out, |_, _| true).unwrap_err();
    assert_eq!(err.status, Status::Key, "{err}");
    assert!(!out.exists());
}

#[test]
fn cancelling_removes_the_partial_archive() {
    let Some(ck) = ready() else { return };
    let dir = TempDir::new("cancel");
    let wud = dir.0.join("game.wud");
    std::fs::write(&wud, build_wud(&ck)).unwrap();
    let out = dir.0.join("game.wua");
    let err = cemu_wrapper::create(&[Source { path: wud, key: Some(DISC_KEY) }], None, None, &out, |done, _| done == 0).unwrap_err();
    assert_eq!(err.status, Status::Cancelled);
    assert!(!out.exists());
}

#[test]
fn an_extracted_title_folder_becomes_the_same_wua() {
    if !cemu_wrapper::available() {
        eprintln!("SKIP: cemuwua is not built");
        return;
    }
    let dir = TempDir::new("folder");
    let title = dir.0.join("Dummy");
    for (path, data) in title_files() {
        let p = title.join(&path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, data).unwrap();
    }
    let out = dir.0.join("Dummy.wua");
    cemu_wrapper::create(&[Source::new(&title)], None, Some(&source_json("Dummy")), &out, |_, _| true).unwrap();
    check_archive(&out, "Dummy");
}

#[test]
fn a_damaged_archive_fails_verify_or_reading() {
    let Some(ck) = ready() else { return };
    let dir = TempDir::new("damaged");
    let wud = dir.0.join("game.wud");
    std::fs::write(&wud, build_wud(&ck)).unwrap();
    let out = dir.0.join("game.wua");
    cemu_wrapper::create(&[Source { path: wud, key: Some(DISC_KEY) }], None, None, &out, |_, _| true).unwrap();
    // Flip a byte early on, inside the compressed file data (the index lives at the end).
    let mut bytes = std::fs::read(&out).unwrap();
    bytes[200] ^= 0xff;
    std::fs::write(&out, &bytes).unwrap();
    match cemu_wrapper::verify(&out, |_, _| true) {
        Ok(Verified::Intact(_)) => panic!("a damaged archive verified"),
        Ok(_) | Err(_) => {}
    }
}

/// Writes the dummy disc out, for trying the conversion by hand (in chdlite, or a copy):
///
/// ```text
/// WIIU_DUMMY_OUT=/some/dir cargo test --test test_wiiu -- --ignored write_dummy_disc
/// ```
///
/// gives `Dummy (USA).wud`, `Dummy (USA).wux` and `Dummy (USA).key` there.
#[test]
#[ignore]
fn write_dummy_disc() {
    let Some(ck) = common_key() else { panic!("external/cemu is not checked out") };
    let dir = PathBuf::from(std::env::var_os("WIIU_DUMMY_OUT").expect("set WIIU_DUMMY_OUT to a folder"));
    std::fs::create_dir_all(&dir).unwrap();
    let wud = build_wud(&ck);
    std::fs::write(dir.join("Dummy (USA).wux"), to_wux(&wud)).unwrap();
    std::fs::write(dir.join("Dummy (USA).wud"), &wud).unwrap();
    std::fs::write(dir.join("Dummy (USA).key"), DISC_KEY).unwrap();
    println!("{}", dir.display());
}
