//! Rip-set playback through Game Music Emu (`rom::gme`), on chip-music files built by hand in
//! `support/chip_music.rs`. No game data. Skips when the gme library is not built.

use romexplorer::rom::gme::{self, render_track, Emu};

#[path = "support/chip_music.rs"]
mod chip_music;

fn need_lib() -> bool {
    if !gme::available() {
        eprintln!("skipped: gme is not built for this machine (deps/rommusic/scripts/build.sh)");
        return false;
    }
    true
}

fn peak(pcm: &[i16]) -> i32 {
    pcm.iter().map(|&s| i32::from(s).abs()).max().unwrap_or(0)
}

#[test]
fn extensions_are_known_without_the_library() {
    for name in ["a.nsf", "b.NSFE", "c.spc", "d.vgz", "e.gbs", "f.hes", "g.kss", "h.ay", "i.sap", "j.gym", "k.vgm"] {
        assert!(gme::known_extension(name), "{name}");
    }
    for name in ["a.wav", "b.nsf.txt", "spc", "c.psf"] {
        assert!(!gme::known_extension(name), "{name}");
    }
}

#[test]
fn vgm_reads_its_tag_and_loop() {
    if !need_lib() {
        return;
    }
    let mut emu = Emu::open(chip_music::vgm_psg(2, 3, "Title Theme", "Test Game", "Test System"), Emu::SAMPLE_RATE).expect("gme opens a VGM");
    assert_eq!(gme::identify(b"Vgm ").as_deref(), Some("VGM"));
    assert_eq!(emu.track_count(), 1);
    let info = emu.track_info(0).unwrap();
    assert_eq!((info.song.as_str(), info.game.as_str()), ("Title Theme", "Test Game"));
    assert_eq!(info.intro_ms, Some(2000), "{info:?}");
    assert_eq!(info.loop_ms, Some(3000), "{info:?}");
    assert!(info.looping() && info.has_length());

    let pcm = render_track(&mut emu, 0, 10_000, 1000).unwrap();
    // intro + two loops + fade, stereo.
    assert_eq!(pcm.len(), (2 + 2 * 3 + 1) * 44_100 * 2);
    assert!(peak(&pcm[..44_100 * 2]) > 1000, "the tone is audible");
    assert!(peak(&pcm[pcm.len() - 200..]) < peak(&pcm[..44_100 * 2]) / 4, "the end is faded");
}

#[test]
fn vgz_is_unpacked_before_gme_sees_it() {
    if !need_lib() {
        return;
    }
    let plain = chip_music::vgm_psg(1, 0, "Jingle", "Test Game", "Test System");
    let Some(packed) = chip_music::gzip(&plain) else {
        eprintln!("skipped: zlib-ng is not built for this machine");
        return;
    };
    assert_eq!(gme::gunzip(&packed).as_deref(), Some(plain.as_slice()));
    let emu = Emu::open(packed, Emu::SAMPLE_RATE).expect("gme opens a VGZ");
    let info = emu.track_info(0).unwrap();
    assert_eq!(info.song, "Jingle");
    assert!(!info.looping());
    assert_eq!(info.length_ms, Some(1000), "{info:?}");
}

#[test]
fn nsf_plays_every_song_without_a_length() {
    if !need_lib() {
        return;
    }
    let mut emu = Emu::open(chip_music::nsf(3, "Test Game"), Emu::SAMPLE_RATE).expect("gme opens an NSF");
    assert_eq!(emu.track_count(), 3);
    let info = emu.track_info(2).unwrap();
    assert_eq!(info.game, "Test Game");
    assert!(!info.has_length(), "an NSF gives no lengths: {info:?}");
    emu.start(2, None).unwrap();
    let mut buf = vec![0i16; 44_100 * 2];
    emu.play(&mut buf).unwrap();
    assert!(peak(&buf) > 1000, "pulse 1 is audible");
    assert!(!emu.ended());
    assert!(emu.tell_ms() >= 990);
}

#[test]
fn garbage_is_refused() {
    if !need_lib() {
        return;
    }
    assert!(Emu::open(b"not music at all".to_vec(), Emu::SAMPLE_RATE).is_err());
    assert!(Emu::open(vec![0x1F, 0x8B, 8, 0, 1, 2, 3], Emu::SAMPLE_RATE).is_err());
    assert!(gme::gunzip(b"plain").is_none());
}

/// A folder under the system temp directory, removed when dropped.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("romexplorer-gme-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn rip_sets_in_a_game_folder_are_indexed_and_played() {
    use romexplorer::rom::rip_sets;
    use romexplorer::rom::rom_audio::{self, SoundKind, Thresholds, TrackLocation};
    if !need_lib() {
        return;
    }
    let dir = TempDir::new("rips");
    let music = dir.0.join("music");
    std::fs::create_dir_all(&music).unwrap();
    std::fs::write(dir.0.join("game.rom"), [0u8; 64]).unwrap();
    std::fs::write(music.join("Game.nsf"), chip_music::nsf(4, "Test Game")).unwrap();
    // The playlist names two of the four songs, with a title and a length for each.
    std::fs::write(music.join("Game.m3u"), "Game.nsf::NSF,2,Overworld,1:10,,3\nGame.nsf::NSF,4,Fanfare,0:04,,1\n").unwrap();
    std::fs::write(dir.0.join("Title.vgm"), chip_music::vgm_psg(2, 3, "Title Theme", "Test Game", "Test System")).unwrap();

    let t = Thresholds::default();
    let mut tracks = rip_sets::index(&dir.0, &t, |_| {});
    tracks.sort_by(|a, b| a.name.cmp(&b.name));
    let names: Vec<&str> = tracks.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["Fanfare", "Overworld", "Title Theme"], "{tracks:#?}");

    let overworld = &tracks[1];
    assert_eq!(overworld.location, TrackLocation::Rip { path: "music/Game.nsf".into(), track: 0 });
    assert_eq!(overworld.duration_ms, 70_000);
    assert_eq!(overworld.kind, SoundKind::Bgm);
    assert_eq!(overworld.codec, "NSF");
    assert_eq!(tracks[0].kind, SoundKind::Jingle, "a fanfare by name");
    let title = &tracks[2];
    assert!(title.looping && title.kind == SoundKind::Bgm, "{title:?}");

    // Played from the folder: the playlist's 70 s plus its 3 s fade.
    let mut stream = rom_audio::open_track(&dir.0, &overworld.location, romexplorer::rom::vgmstream::PlayConfig::LISTEN).unwrap();
    assert_eq!((stream.sample_rate(), stream.channels()), (44_100, 2));
    assert_eq!(stream.total_samples(), Some(73 * 44_100));
    let first = stream.next_chunk().unwrap().unwrap();
    assert!(peak(&first) > 1000);
}

#[test]
fn a_track_with_no_length_is_measured() {
    use romexplorer::rom::rip_sets;
    use romexplorer::rom::rom_audio::Thresholds;
    if !need_lib() {
        return;
    }
    let dir = TempDir::new("measure");
    std::fs::write(dir.0.join("Game.nsf"), chip_music::nsf(1, "Test Game")).unwrap();
    let tracks = rip_sets::index(&dir.0, &Thresholds::default(), |_| {});
    assert_eq!(tracks.len(), 1);
    // The tone never stops: it runs on, so it loops.
    assert!(tracks[0].looping, "{:?}", tracks[0]);
    assert!(tracks[0].duration_ms >= u64::from(rip_sets::RUN_ON_MS) - 1000, "{:?}", tracks[0]);
}

#[test]
fn sgc_plays_through_the_second_library() {
    if !need_lib() || !gme::sgc_available() {
        eprintln!("skipped: the SGC player is not built for this machine (deps/rommusic/scripts/build.sh)");
        return;
    }
    assert!(gme::known_extension("a.sgc"));
    let data = chip_music::sgc(3, "Test Game");
    assert_eq!(gme::identify(&data).as_deref(), Some("SGC"));
    let mut emu = Emu::open(data, Emu::SAMPLE_RATE).expect("rommusic opens an SGC");
    assert_eq!(emu.track_count(), 3);
    let info = emu.track_info(1).unwrap();
    assert_eq!(info.game, "Test Game");
    assert_eq!(info.fade_ms, None);
    emu.start(1, Some((1000, 500))).unwrap();
    let mut buf = vec![0i16; 44_100 * 2];
    emu.play(&mut buf).unwrap();
    assert!(peak(&buf) > 1000, "the PSG tone is audible");
}

#[test]
fn a_rip_library_set_is_matched_to_every_rom_that_carries_it() {
    use romexplorer::common::models::Platform;
    use romexplorer::rom::rip_match::Library;
    use romexplorer::rom::rip_sets;
    use romexplorer::rom::rom_audio::{self, Thresholds, TrackLocation};
    if !need_lib() {
        return;
    }
    let lib_dir = TempDir::new("library");
    let roms = TempDir::new("library-roms");
    let set = lib_dir.0.join("Some Game (NSF)");
    std::fs::create_dir_all(&set).unwrap();
    // The NSF's code plus a block of song data that looks like real bytes.
    let mut nsf = chip_music::nsf(2, "Test Game");
    let mut x = 0x1234_5678u32;
    nsf.extend((0..4096).map(|_| {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        x as u8
    }));
    std::fs::write(set.join("game.nsf"), &nsf).unwrap();
    // Two regions: the same music at different places; and a ROM of another game.
    let image = &nsf[0x80..];
    for (name, pad) in [("Game (Japan).nes", 0x4010usize), ("Game (USA).nes", 0x8010)] {
        let mut rom = vec![0x55u8; pad];
        rom.extend_from_slice(image);
        rom.resize(rom.len() + 0x2000, 0xEA);
        std::fs::write(roms.0.join(name), rom).unwrap();
    }
    std::fs::write(roms.0.join("Other (USA).nes"), vec![0xA9u8; 0x10000]).unwrap();

    let cache = roms.0.join("cache.json");
    let library = Library::build(&[lib_dir.0.clone()], Some(&cache), |_, _| {});
    assert_eq!(library.len(), 1);
    let nes = Platform::Nes as u16;
    assert!(library.covers(nes) && !library.covers(Platform::Snes as u16));
    for name in ["Game (Japan).nes", "Game (USA).nes"] {
        let got = library.match_rom(&roms.0.join(name), nes).unwrap();
        assert_eq!(got.len(), 1, "{name}: {got:?}");
        assert_eq!(got[0].0, set);
    }
    assert!(library.match_rom(&roms.0.join("Other (USA).nes"), nes).unwrap().is_empty());
    // The cache is used the second time, and gives the same answer.
    assert!(cache.is_file());
    assert_eq!(Library::build(&[lib_dir.0.clone()], Some(&cache), |_, _| {}).len(), 1);

    let tracks = rip_sets::index_set(&set, &Thresholds::default(), |_| {});
    assert_eq!(tracks.len(), 2);
    let TrackLocation::Rip { path, .. } = &tracks[0].location else { panic!("{:?}", tracks[0].location) };
    assert!(std::path::Path::new(path).is_absolute(), "{path}");
    let mut stream = rom_audio::open_track(std::path::Path::new(""), &tracks[0].location, romexplorer::rom::vgmstream::PlayConfig::LISTEN).unwrap();
    assert!(peak(&stream.next_chunk().unwrap().unwrap()) > 1000);
}
