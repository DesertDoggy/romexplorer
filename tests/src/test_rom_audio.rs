//! ROM audio: vgmstream decoding through `rom::vgmstream`, on synthetic files built in
//! `support/game_audio.rs`. No game data. Skips when `rvgmstream` is not built.

use romexplorer::rom::vgmstream::{self, decode_all, MemorySource, PlayConfig, Stream};

#[path = "support/game_audio.rs"]
mod game_audio;

fn need_lib() -> bool {
    if !vgmstream::available() {
        eprintln!("skipped: rvgmstream is not built for this machine (deps/vgmstream/scripts/build.sh)");
        return false;
    }
    true
}

#[test]
fn wav_with_loop_points_reports_the_loop() {
    if !need_lib() {
        return;
    }
    let pcm = game_audio::tone(32_000, 2, 32_000 * 3, 440.0);
    let bytes = game_audio::wav(32_000, 2, &pcm, Some((16_000, 95_999)));
    let stream = Stream::open(Box::new(MemorySource::new("bgm_title.wav", bytes)), "bgm_title.wav", 0, PlayConfig::AS_STORED).expect("vgmstream opens a looped WAV");
    let info = stream.info();
    assert_eq!((info.channels, info.sample_rate), (2, 32_000));
    assert_eq!(info.stream_samples, 96_000);
    assert!(info.looping, "{info:?}");
    assert_eq!((info.loop_start, info.loop_end), (16_000, 96_000), "loop end is exclusive on vgmstream's side");
    assert_eq!(info.duration_ms(), 3000);
}

#[test]
fn as_stored_decodes_one_pass_and_listen_adds_the_loop_and_fade() {
    if !need_lib() {
        return;
    }
    let pcm = game_audio::tone(22_050, 1, 22_050 * 2, 220.0);
    let bytes = game_audio::wav(22_050, 1, &pcm, Some((0, 44_099)));
    let src = MemorySource::new("loop.wav", bytes);
    let mut once = Stream::open(Box::new(src.clone()), "loop.wav", 0, PlayConfig::AS_STORED).unwrap();
    let decoded = decode_all(&mut once).unwrap();
    assert_eq!(decoded.len(), 44_100);
    assert_eq!(&decoded[..1000], &pcm[..1000], "PCM comes back as stored");

    let mut listen = Stream::open(Box::new(src), "loop.wav", 0, PlayConfig::LISTEN).unwrap();
    let expected = listen.info().play_samples;
    assert_eq!(expected, 44_100 * 2 + 22_050 * 10, "two loops plus a ten-second fade");
    assert_eq!(decode_all(&mut listen).unwrap().len() as u64, expected);
}

#[test]
fn ps_adpcm_vag_decodes() {
    if !need_lib() {
        return;
    }
    let bytes = game_audio::vag(44_100, 200);
    let mut stream = Stream::open(Box::new(MemorySource::new("voice.vag", bytes)), "voice.vag", 0, PlayConfig::AS_STORED).expect("vgmstream opens a VAG");
    let info = stream.info().clone();
    assert_eq!((info.channels, info.sample_rate), (1, 44_100));
    assert!(!info.looping);
    assert_eq!(info.stream_samples, 200 * 28);
    let pcm = decode_all(&mut stream).unwrap();
    assert_eq!(pcm.len(), 200 * 28);
    assert!(pcm.iter().all(|s| *s == 0), "silent frames decode to silence");
}

#[test]
fn unknown_bytes_are_not_a_stream() {
    if !need_lib() {
        return;
    }
    assert!(Stream::open(Box::new(MemorySource::new("readme.txt", b"just text, nothing to decode".to_vec())), "readme.txt", 0, PlayConfig::AS_STORED).is_none());
    assert!(vgmstream::known_extension("music.adx"));
    assert!(vgmstream::known_extension("BGM01.AT9"));
    assert!(!vgmstream::known_extension("boot.elf"));
}
