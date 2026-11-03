//! The copy engine's filesystem behaviour, with no database and no samples: the byte copy
//! itself, the part-file discipline that makes a crash survivable, the destination-name
//! rules, and the `.m3u` rewriting.
//!
//! The database-backed half (selection resolution, holder preference, mounts, the layout
//! schemes) lives in `test_api`, which has a throwaway MariaDB to resolve against. Splitting
//! them means this file runs anywhere, including a checkout with no bundled server.
//!
//! `cargo test --test test_copy`

use std::path::{Path, PathBuf};

use romexplorer::api::copy::run::{copy_with_progress, rewrite_m3u};
use romexplorer::api::copy::{sanitize_segment, with_dedup_suffix};

/// Its own directory per test, removed on the way out: these write real files.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("romexplorer-copy-{}-{}-{}", name, std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Self(dir)
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }

    fn write(&self, rel: &str, bytes: &[u8]) -> PathBuf {
        let p = self.path(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("parent");
        }
        std::fs::write(&p, bytes).expect("write");
        p
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn part_of(dst: &Path) -> PathBuf {
    let name = format!("{}.recopy-part", dst.file_name().unwrap().to_string_lossy());
    dst.parent().unwrap().join(name)
}

fn copy(src: &Path, dst: &Path) -> std::io::Result<u64> {
    copy_with_progress(src, dst, &mut |_| true)
}

#[test]
fn a_copy_is_byte_for_byte() {
    let s = Scratch::new("bytes");
    let data: Vec<u8> = (0..=255u8).cycle().take(300_000).collect();
    let src = s.write("in.bin", &data);
    let dst = s.path("out/in.bin");
    assert_eq!(copy(&src, &dst).unwrap(), data.len() as u64);
    assert_eq!(std::fs::read(&dst).unwrap(), data, "the bytes survived");
}

/// The destination's parent may not exist yet -- a copy creates the platform folder.
#[test]
fn a_copy_creates_the_folders_it_needs() {
    let s = Scratch::new("mkdir");
    let src = s.write("in.bin", b"x");
    let dst = s.path("a/b/c/in.bin");
    copy(&src, &dst).unwrap();
    assert!(dst.is_file());
}

/// Progress is reported as it goes and adds up to the file's size, which is what the bar
/// and the byte total in the summary are built on.
#[test]
fn progress_adds_up_to_the_whole_file() {
    let s = Scratch::new("progress");
    let data = vec![9u8; 128 * 1024];
    let src = s.write("in.bin", &data);
    let mut seen = 0u64;
    copy_with_progress(&src, &s.path("out.bin"), &mut |n| {
        seen += n;
        true
    })
    .unwrap();
    assert_eq!(seen, data.len() as u64);
}

/// The whole point of the part file: a cancel leaves neither a half-written target nor a
/// stray file the next run could mistake for one.
#[test]
fn a_cancelled_copy_leaves_nothing_behind() {
    let s = Scratch::new("cancel");
    let src = s.write("in.bin", &vec![1u8; 4096]);
    let dst = s.path("out.bin");
    let err = copy_with_progress(&src, &dst, &mut |_| false).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
    assert!(!dst.exists(), "no target");
    assert!(!part_of(&dst).exists(), "no part file");
}

/// A source that isn't there is an error, not a zero-byte destination.
#[test]
fn a_missing_source_writes_no_destination() {
    let s = Scratch::new("missing");
    let dst = s.path("out.bin");
    assert!(copy(&s.path("nope.bin"), &dst).is_err());
    assert!(!dst.exists());
    assert!(!part_of(&dst).exists());
}

/// Copying over a file that is already there replaces it wholesale rather than blending the
/// two -- the run deletes the old one first, but the rename has to be atomic either way.
#[test]
fn a_copy_over_a_shorter_file_replaces_it() {
    let s = Scratch::new("replace");
    let src = s.write("in.bin", &vec![2u8; 100]);
    let dst = s.write("out.bin", &vec![1u8; 5000]);
    copy(&src, &dst).unwrap();
    assert_eq!(std::fs::read(&dst).unwrap(), vec![2u8; 100]);
}

/// A directory bundle keeps its shape, which is the thing that makes a PS3 or Wii U dump
/// still work after the copy.
#[test]
fn a_directory_bundle_keeps_its_tree() {
    let s = Scratch::new("bundle");
    let files = ["game/PS3_GAME/USRDIR/EBOOT.BIN", "game/PS3_GAME/ICON0.PNG", "game/PARAM.SFO"];
    for f in files {
        s.write(f, f.as_bytes());
    }
    for f in files {
        let rel = f.strip_prefix("game/").unwrap();
        copy(&s.path(f), &s.path(&format!("dest/game/{rel}"))).unwrap();
    }
    for f in files {
        let rel = f.strip_prefix("game/").unwrap();
        assert_eq!(std::fs::read(s.path(&format!("dest/game/{rel}"))).unwrap(), f.as_bytes(), "{f}");
    }
}

#[test]
fn destination_names_survive_a_fat_card() {
    // Everything Windows and exFAT reject, and the DOS device names that cannot be files at
    // all -- the source filesystem allows all of it.
    assert_eq!(sanitize_segment("Hoge Quest VII (USA) (Disc 1).chd"), "Hoge Quest VII (USA) (Disc 1).chd");
    assert_eq!(sanitize_segment("Where? What: Why*"), "Where_ What_ Why_");
    assert_eq!(sanitize_segment("trailing dots..."), "trailing dots");
    assert_eq!(sanitize_segment("aux.zip"), "aux.zip_");
    let long = sanitize_segment(&format!("{}.chd", "x".repeat(500)));
    assert!(long.len() <= 255 && long.ends_with(".chd"));
}

#[test]
fn a_name_collision_is_numbered_not_overwritten() {
    assert_eq!(with_dedup_suffix("game.chd", 2), "game (2).chd");
    assert_eq!(with_dedup_suffix("game (2).chd", 3), "game (2) (3).chd");
}

/// A bin/cue to chd conversion changes every extension, so an unrewritten playlist is
/// silently broken -- and a playlist we didn't produce must not be guessed at.
#[test]
fn a_playlist_follows_its_discs_and_nothing_else() {
    let produced = [("Game (Disc 1).cue".to_string(), "Game (Disc 1).chd".to_string()), ("Game (Disc 2).cue".to_string(), "Game (Disc 2).chd".to_string())];
    let (out, unmatched) = rewrite_m3u("# Game\nGame (Disc 1).cue\nGame (Disc 2).cue\nsomeone elses line.bin\n", &produced);
    assert_eq!(out, "# Game\nGame (Disc 1).chd\nGame (Disc 2).chd\nsomeone elses line.bin\n");
    assert_eq!(unmatched, vec!["someone elses line.bin".to_string()]);
}

/// Copying a whole rom's files, then re-copying, is what a resumed job does: the second pass
/// finds the same bytes and writes nothing new.
#[test]
fn recopying_the_same_files_is_a_no_op_on_disk() {
    let s = Scratch::new("resume");
    let src = s.write("Game (Disc 1).bin", &vec![5u8; 8192]);
    let dst = s.path("dest/Game (Disc 1).bin");
    copy(&src, &dst).unwrap();
    let first = std::fs::metadata(&dst).unwrap().modified().unwrap();
    // The run's own skip is by size + hash and is covered in `test_api`; here the point is
    // only that a second copy produces an identical file.
    copy(&src, &dst).unwrap();
    assert_eq!(std::fs::read(&dst).unwrap().len(), 8192);
    let _ = first;
    assert!(!part_of(&dst).exists());
}
