//! Rip-set playback through Game Music Emu (`rom::gme`), on chip-music files built by hand in
//! `support/chip_music.rs`. No game data. Skips when the gme library is not built.

use romexplorer::rom::gme::{self, render_track, Emu};

#[path = "support/chip_music.rs"]
mod chip_music;

fn need_lib() -> bool {
    if !gme::available() {
        eprintln!("skipped: gme is not built for this machine (deps/gme/scripts/build.sh)");
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
