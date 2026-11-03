//! Rebuilding and checking cue/gdi/m3u sidecars, on a real filesystem.
//!
//! The unit tests in `rom::sheet_build` cover the geometry; these cover the parts only a disk
//! can answer -- that what is written lands where it should, that it survives a round trip
//! through the parser, and that the checker agrees with the rebuilder about what correct means.

use std::path::{Path, PathBuf};

use romexplorer::rom::detect::sheet::{CueSheetOptions, Sheet, SheetKind};
use romexplorer::rom::sheet_build::{build_m3u, check_m3u, check_sheet, rebuild_cue, rebuild_gdi, Convention, Expected, SheetVerdict, TrackFile};

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("romexplorer_test_sheets_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn file(&self, name: &str, body: &[u8]) -> TrackFile {
        let path = self.0.join(name);
        std::fs::write(&path, body).expect("write");
        TrackFile::at(path).expect("stat")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A raw data sector: the 12-byte sync pattern, then a header whose last byte is the mode.
fn data(mode: u8, sectors: usize) -> Vec<u8> {
    let mut sector = vec![0u8; 2352];
    sector[..12].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
    sector[15] = mode;
    sector.repeat(sectors)
}

fn audio(sectors: usize) -> Vec<u8> {
    (0..2352 * sectors).map(|i| (i % 251) as u8).collect()
}

#[test]
fn a_rebuilt_cue_is_written_and_reads_back_as_sound() {
    let dir = Scratch::new("write");
    let tracks = vec![dir.file("Game (Track 1).bin", &data(1, 20)), dir.file("Game (Track 2).bin", &audio(8)), dir.file("Game (Track 3).bin", &audio(6))];

    let rebuilt = rebuild_cue("Game", &tracks, &Expected::default(), &CueSheetOptions::redump()).expect("rebuild");
    let cue = dir.path().join(&rebuilt.file_name);
    std::fs::write(&cue, &rebuilt.bytes).expect("write cue");

    assert_eq!(cue.file_name().unwrap(), "Game.cue");
    // The checker and the rebuilder have to agree, or every sheet the app writes is instantly
    // flagged by the app's own verify job.
    assert_eq!(check_sheet(&cue, &Expected::default()), SheetVerdict::StructureOk { hash_known: false });

    let parsed = Sheet::parse(&cue).expect("parse");
    assert_eq!(parsed.tracks.len(), 3);
    assert_eq!(parsed.files, vec!["Game (Track 1).bin".to_string(), "Game (Track 2).bin".to_string(), "Game (Track 3).bin".to_string()]);
}

/// The whole reason the rebuilder tries more than one convention.
#[test]
fn the_rebuild_the_dat_confirms_is_the_one_written() {
    let dir = Scratch::new("confirm");
    let tracks = vec![dir.file("G (Track 1).bin", &data(1, 12)), dir.file("G (Track 2).bin", &audio(5))];
    let options = CueSheetOptions::redump();

    // Whichever convention is *not* the default, offered as what the DAT recorded.
    let default = rebuild_cue("G", &tracks, &Expected::default(), &options).expect("default");
    assert_eq!(default.convention, Convention::GapInFile);

    let alternative = {
        // Render the declared-gap form by hand, the way a Redump cue of this shape would read.
        let text = "FILE \"G (Track 1).bin\" BINARY\r\n  TRACK 01 MODE1/2352\r\n    INDEX 01 00:00:00\r\nFILE \"G (Track 2).bin\" BINARY\r\n  TRACK 02 AUDIO\r\n    PREGAP 00:02:00\r\n    INDEX 01 00:00:00\r\n";
        text.as_bytes().to_vec()
    };
    let expected = Expected {
        size: Some(alternative.len() as u64),
        hashes: vec![("sha1".to_string(), romexplorer::rom::hash::hash_bytes(romexplorer::rom::hash::HashFlags::SHA1, &alternative).sha1.unwrap())],
    };

    let chosen = rebuild_cue("G", &tracks, &expected, &options).expect("rebuild");
    assert!(chosen.matched, "the DAT's own sheet was reproducible and should have been reproduced");
    assert_eq!(chosen.convention, Convention::GapDeclared);
    assert_eq!(chosen.bytes, alternative, "byte for byte, not merely equivalent");

    // And once written, the checker upgrades it from "structure ok" to "verified".
    let cue = dir.path().join(&chosen.file_name);
    std::fs::write(&cue, &chosen.bytes).unwrap();
    assert_eq!(check_sheet(&cue, &expected), SheetVerdict::Verified);
}

/// A sheet that plays but is not the DAT's is the case the whole two-level verdict exists for.
#[test]
fn a_working_sheet_that_isnt_the_dats_is_never_called_broken() {
    let dir = Scratch::new("lenient");
    let tracks = vec![dir.file("G (Track 1).bin", &data(1, 12)), dir.file("G (Track 2).bin", &audio(5))];
    let rebuilt = rebuild_cue("G", &tracks, &Expected::default(), &CueSheetOptions::redump()).unwrap();
    let cue = dir.path().join(&rebuilt.file_name);
    std::fs::write(&cue, &rebuilt.bytes).unwrap();

    let wrong = Expected { size: Some(1), hashes: vec![("sha1".to_string(), "0".repeat(40))] };
    let verdict = check_sheet(&cue, &wrong);
    assert_eq!(verdict, SheetVerdict::StructureOk { hash_known: true });
    assert!(verdict.usable());
    assert_eq!(verdict.tone(), "warn", "not bad -- the disc plays");
}

#[test]
fn a_gdi_is_written_with_its_track_count_and_absolute_lbas() {
    let dir = Scratch::new("gdi");
    let tracks = vec![dir.file("disc01.bin", &data(1, 100)), dir.file("disc02.raw", &audio(40)), dir.file("disc03.bin", &data(1, 300))];
    let rebuilt = rebuild_gdi("disc", &tracks, &Expected::default(), &CueSheetOptions::redump()).expect("rebuild");
    let gdi = dir.path().join(&rebuilt.file_name);
    std::fs::write(&gdi, &rebuilt.bytes).unwrap();

    let text = std::fs::read_to_string(&gdi).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "3", "the track count leads the file");
    assert!(lines[3].contains("45000"), "the high-density area starts where the format fixes it: {}", lines[3]);
    assert_eq!(check_sheet(&gdi, &Expected::default()), SheetVerdict::StructureOk { hash_known: false });

    let parsed = Sheet::parse_text(SheetKind::Gdi, &text).expect("parse");
    assert_eq!(parsed.tracks.len(), 3);
}

#[test]
fn a_playlist_is_written_relative_and_checks_out() {
    let dir = Scratch::new("m3u");
    for disc in ["G (Disc 1).chd", "G (Disc 2).chd", "G (Disc 3).chd"] {
        std::fs::write(dir.path().join(disc), b"disc").unwrap();
    }
    let names: Vec<String> = ["G (Disc 1).chd", "G (Disc 2).chd", "G (Disc 3).chd"].iter().map(|s| s.to_string()).collect();
    let body = build_m3u(&names, romexplorer::rom::detect::sheet::LineEnding::Lf);
    let m3u = dir.path().join("G.m3u");
    std::fs::write(&m3u, &body).unwrap();

    let text = std::fs::read_to_string(&m3u).unwrap();
    assert_eq!(text, "G (Disc 1).chd\nG (Disc 2).chd\nG (Disc 3).chd\n");
    assert!(!text.contains(dir.path().to_str().unwrap()), "relative, so it survives being copied elsewhere");
    assert_eq!(check_m3u(&m3u, &Expected::default()), SheetVerdict::StructureOk { hash_known: false });
}

/// The failure a playlist actually has: a line that stopped resolving.
#[test]
fn a_playlist_pointing_at_a_disc_that_moved_is_broken() {
    let dir = Scratch::new("m3u_broken");
    std::fs::write(dir.path().join("G (Disc 1).chd"), b"disc").unwrap();
    let m3u = dir.path().join("G.m3u");
    std::fs::write(&m3u, "G (Disc 1).chd\nG (Disc 2).chd\n").unwrap();
    match check_m3u(&m3u, &Expected::default()) {
        SheetVerdict::Broken { problems } => assert!(problems.iter().any(|p| p.contains("Disc 2")), "{problems:?}"),
        other => panic!("{other:?}"),
    }
}

/// A single-track disc gets a plain `{stem}.bin` cue, not a `(Track 1)` one -- the Redump form
/// rather than chdman's `--splitbin` form.
#[test]
fn a_single_track_disc_keeps_the_redump_naming() {
    let dir = Scratch::new("single");
    let tracks = vec![dir.file("Game.bin", &data(2, 50))];
    let rebuilt = rebuild_cue("Game", &tracks, &Expected::default(), &CueSheetOptions::redump()).unwrap();
    let text = String::from_utf8(rebuilt.bytes.clone()).unwrap();
    assert!(text.contains("FILE \"Game.bin\" BINARY"), "{text}");
    assert!(text.contains("TRACK 01 MODE2/2352"), "the mode is read off the sector, not assumed: {text}");
    assert!(text.ends_with("\r\n"), "Redump writes CRLF");
}

/// Ten or more tracks is where a plain string sort would silently reorder the disc.
#[test]
fn a_twelve_track_disc_stays_in_order() {
    let dir = Scratch::new("twelve");
    let mut tracks = vec![dir.file("G (Track 01).bin", &data(1, 10))];
    for n in 2..=12 {
        tracks.push(dir.file(&format!("G (Track {n:02}).bin"), &audio(3)));
    }
    let rebuilt = rebuild_cue("G", &tracks, &Expected::default(), &CueSheetOptions::redump()).unwrap();
    let numbers: Vec<u32> = rebuilt.sheet.tracks.iter().map(|t| t.number).collect();
    assert_eq!(numbers, (1..=12).collect::<Vec<u32>>());
    let cue = dir.path().join(&rebuilt.file_name);
    std::fs::write(&cue, &rebuilt.bytes).unwrap();
    assert!(check_sheet(&cue, &Expected::default()).usable());
}

/// The repair a "fix hash" button performs, in the part that needs no database: a sheet whose
/// notation is the only thing wrong can be rewritten into the DAT's own form, because the
/// search has only a few legal spellings to try and the hash says which is right.
#[test]
fn a_sheet_whose_only_fault_is_its_notation_can_be_rewritten() {
    let dir = Scratch::new("fix");
    let tracks = vec![dir.file("G (Track 1).bin", &data(1, 12)), dir.file("G (Track 2).bin", &audio(5))];
    let options = CueSheetOptions::redump();

    // The DAT's form: the gap declared rather than carved out of the file.
    let dats = b"FILE \"G (Track 1).bin\" BINARY\r\n  TRACK 01 MODE1/2352\r\n    INDEX 01 00:00:00\r\nFILE \"G (Track 2).bin\" BINARY\r\n  TRACK 02 AUDIO\r\n    PREGAP 00:02:00\r\n    INDEX 01 00:00:00\r\n";
    let expected = Expected {
        size: Some(dats.len() as u64),
        hashes: vec![("sha1".to_string(), romexplorer::rom::hash::hash_bytes(romexplorer::rom::hash::HashFlags::SHA1, dats).sha1.unwrap())],
    };

    // What is on disk is the *other* legal form: sound, and not the DAT's.
    let cue = dir.path().join("G.cue");
    let wrong = rebuild_cue("G", &tracks, &Expected::default(), &options).unwrap();
    std::fs::write(&cue, &wrong.bytes).unwrap();
    assert_eq!(check_sheet(&cue, &expected), SheetVerdict::StructureOk { hash_known: true }, "sound, but not the DAT's");

    // The repair is the same search, now with something to aim at.
    let fixed = rebuild_cue("G", &tracks, &expected, &options).unwrap();
    assert!(fixed.matched);
    std::fs::write(&cue, &fixed.bytes).unwrap();
    assert_eq!(check_sheet(&cue, &expected), SheetVerdict::Verified);
    assert_eq!(std::fs::read(&cue).unwrap(), dats.to_vec(), "byte for byte the DAT's own sheet");
}

/// The refusal that matters. A sheet naming files that are not named as the DAT names them can
/// never hash-match, because the names in a sheet have to be the names on disk -- so no amount
/// of rewriting helps, and the fix has to stop rather than churn.
#[test]
fn a_sheet_over_differently_named_tracks_can_never_match_however_it_is_written() {
    let dir = Scratch::new("fix_names");
    // The same disc, under the user's own naming.
    let tracks = vec![dir.file("g_t1.bin", &data(1, 12)), dir.file("g_t2.bin", &audio(5))];
    let options = CueSheetOptions::redump();

    let dats = b"FILE \"G (Track 1).bin\" BINARY\r\n  TRACK 01 MODE1/2352\r\n    INDEX 01 00:00:00\r\nFILE \"G (Track 2).bin\" BINARY\r\n  TRACK 02 AUDIO\r\n    PREGAP 00:02:00\r\n    INDEX 01 00:00:00\r\n";
    let expected = Expected {
        size: Some(dats.len() as u64),
        hashes: vec![("sha1".to_string(), romexplorer::rom::hash::hash_bytes(romexplorer::rom::hash::HashFlags::SHA1, dats).sha1.unwrap())],
    };

    // Every convention, and none of them can match: the names are baked into the bytes.
    let attempt = rebuild_cue("G", &tracks, &expected, &options).unwrap();
    assert!(!attempt.matched, "no spelling of the notation can change the file names");

    // And the sheet built over the real names is still perfectly sound -- which is what the
    // refusal has to say, or someone goes and re-dumps a disc that is fine.
    let cue = dir.path().join("G.cue");
    std::fs::write(&cue, &attempt.bytes).unwrap();
    let verdict = check_sheet(&cue, &expected);
    assert_eq!(verdict, SheetVerdict::StructureOk { hash_known: true });
    assert!(verdict.usable());
}

/// A playlist's bytes are its names plus its line endings, so the same search applies over a
/// smaller space.
#[test]
fn a_playlist_can_be_fixed_by_its_line_endings() {
    let dir = Scratch::new("fix_m3u");
    for disc in ["G (Disc 1).chd", "G (Disc 2).chd"] {
        std::fs::write(dir.path().join(disc), b"disc").unwrap();
    }
    let names: Vec<String> = vec!["G (Disc 1).chd".into(), "G (Disc 2).chd".into()];
    let crlf = build_m3u(&names, romexplorer::rom::detect::sheet::LineEnding::Crlf);
    let expected = Expected {
        size: Some(crlf.len() as u64),
        hashes: vec![("sha1".to_string(), romexplorer::rom::hash::hash_bytes(romexplorer::rom::hash::HashFlags::SHA1, &crlf).sha1.unwrap())],
    };

    let fixed = romexplorer::rom::sheet_build::rebuild_m3u("G", &names, &expected, romexplorer::rom::detect::sheet::LineEnding::Lf);
    assert!(fixed.matched, "the other ending was tried and found");
    assert_eq!(fixed.bytes, crlf);
}
