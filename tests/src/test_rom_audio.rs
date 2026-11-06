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

// --- disc filesystems and indexing -----------------------------------------------------

use std::path::{Path, PathBuf};

use game_audio::Entry;
use romexplorer::rom::disc_files::{self, DiscView, FsKind};
use romexplorer::rom::rom_audio::{self, classify, Shape, SoundKind, Thresholds, TrackLocation};

fn shape(looping: bool, duration_ms: u64, channels: u16) -> Shape {
    Shape { looping, duration_ms, channels, cd_track: 0, rip: false }
}

#[path = "support/disc_images.rs"]
#[allow(dead_code)]
mod disc_images;

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        // Tests run in parallel and may share a name: each gets its own folder.
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("romexplorer_test_rom_audio_{}_{n}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn view_of(bytes: Vec<u8>) -> DiscView {
    let len = bytes.len() as u64;
    struct Mem(Vec<u8>);
    impl romexplorer::rom::source::ReadAt for Mem {
        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
            let start = offset as usize;
            buf.copy_from_slice(self.0.get(start..start + buf.len()).ok_or(std::io::ErrorKind::UnexpectedEof)?);
            Ok(())
        }
    }
    DiscView::new(Box::new(Mem(bytes)), len)
}

/// A title loop of 25 s, a 10 s fanfare, a 1 s menu blip and a readme: one of each kind.
fn sound_tree() -> Vec<Entry> {
    let bgm = game_audio::wav(22_050, 2, &game_audio::tone(22_050, 2, 22_050 * 25, 440.0), Some((22_050, 22_050 * 25 - 1)));
    let fanfare = game_audio::wav(22_050, 1, &game_audio::tone(22_050, 1, 22_050 * 10, 660.0), None);
    vec![
        Entry::Dir("SOUND", vec![Entry::Dir("BGM", vec![Entry::File("TITLE.WAV", bgm)]), Entry::File("FANFARE.WAV", fanfare), Entry::File("BLIP.VAG", game_audio::vag(22_050, 800))]),
        Entry::File("README.TXT", b"not audio".repeat(20)),
    ]
}

fn read_file(view: &DiscView, file: &disc_files::DiscFile) -> Vec<u8> {
    let mut buf = vec![0u8; file.size as usize];
    assert_eq!(file.read(view, 0, &mut buf).unwrap(), buf.len());
    buf
}

#[test]
fn iso9660_lists_every_file_in_both_sector_layouts() {
    let iso = game_audio::iso9660(&[Entry::Dir("DATA", vec![Entry::File("A.BIN", disc_images::pattern(5000, 1))]), Entry::File("B.BIN", disc_images::pattern(100, 2))]);
    for (bytes, label) in [(iso.clone(), "2048"), (game_audio::raw_2352(&iso), "2352")] {
        let view = view_of(bytes);
        let listing = disc_files::list(&view).unwrap();
        assert_eq!(listing.fs, FsKind::Iso9660, "{label}");
        let paths: Vec<&str> = listing.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["DATA/A.BIN", "B.BIN"], "{label}");
        assert_eq!(read_file(&view, &listing.files[0]), disc_images::pattern(5000, 1), "{label}: a file spanning sectors reads back whole");
    }
}

#[test]
fn xdvdfs_and_gamecube_are_listed() {
    let xbox = view_of(disc_images::volume(&disc_images::tree("default.xbe")));
    let listing = disc_files::list(&xbox).unwrap();
    assert_eq!(listing.fs, FsKind::Xdvdfs);
    let paths: Vec<&str> = listing.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["default.xbe", "media/a.bin", "media/empty.txt", "media/sub/b.txt", "z.dat"]);
    let b = listing.files.iter().find(|f| f.path == "media/sub/b.txt").unwrap();
    assert_eq!(read_file(&xbox, b), b"hello ".repeat(1000));

    let gc = view_of(game_audio::gamecube(&[("bgm.dsp", disc_images::pattern(70_000, 4)), ("se.dsp", disc_images::pattern(300, 5))]));
    let listing = disc_files::list(&gc).unwrap();
    assert_eq!(listing.fs, FsKind::GameCube);
    let paths: Vec<&str> = listing.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["audio/bgm.dsp", "audio/se.dsp"]);
    assert_eq!(read_file(&gc, &listing.files[0]), disc_images::pattern(70_000, 4));
}

#[test]
fn a_wii_disc_is_refused_not_misread() {
    let mut disc = vec![0u8; 0x1000];
    disc[0x18..0x1C].copy_from_slice(&[0x5D, 0x1C, 0x9E, 0xA3]);
    let err = disc_files::list(&view_of(disc)).err().unwrap();
    assert!(err.contains("Wii"), "{err}");
}

#[test]
fn classify_by_loop_length_and_folder() {
    let t = Thresholds::default();
    assert_eq!(classify(&shape(true, 3_500, 2), "x/y.adx", &t), SoundKind::Bgm, "a short loop is still music");
    assert_eq!(classify(&shape(false, 65_000, 2), "x/y.adx", &t), SoundKind::Bgm);
    assert_eq!(classify(&shape(false, 45_000, 2), "x/y.adx", &t), SoundKind::Jingle, "under a minute without a loop is not BGM");
    assert_eq!(classify(&shape(false, 2_000, 2), "sound/me_levelup.aac", &t), SoundKind::Jingle, "a music effect by name");
    assert_eq!(classify(&shape(true, 6_000, 2), "sound/me_shop.aac", &t), SoundKind::Jingle, "even when it loops");
    assert_eq!(classify(&shape(false, 300, 2), "sound/me_blank.aac", &t), SoundKind::Se, "too short to be one");
    assert_eq!(classify(&Shape { cd_track: 5, ..shape(false, 45_000, 2) }, "/Track 05", &t), SoundKind::Bgm, "but a CD audio track is from 20 s");
    assert_eq!(classify(&Shape { cd_track: 2, ..shape(false, 25_000, 2) }, "/Track 02", &t), SoundKind::Se, "the spoken disc warning");
    assert_eq!(classify(&Shape { cd_track: 2, ..shape(false, 90_000, 2) }, "/Track 02", &t), SoundKind::Bgm, "a long first track is music");
    assert_eq!(classify(&Shape { rip: true, ..shape(false, 52_000, 2) }, "set.zip|game.sgc/Old Town", &t), SoundKind::Bgm, "a rip-set track is music from 20 s too");
    assert_eq!(classify(&Shape { rip: true, ..shape(false, 9_000, 2) }, "set.zip|game.sgc/Cue", &t), SoundKind::Jingle);
    assert_eq!(classify(&shape(false, 10_000, 2), "x/y.adx", &t), SoundKind::Jingle);
    assert_eq!(classify(&shape(false, 2_000, 2), "x/y.adx", &t), SoundKind::Se);
    assert_eq!(classify(&shape(true, 5_000, 2), "SE/rain_loop.adx", &t), SoundKind::Se, "a looping ambience in an SE folder");
    assert_eq!(classify(&shape(false, 12_000, 2), "BGM/ending.adx", &t), SoundKind::Bgm, "a BGM folder lifts a jingle-length track");
    assert_eq!(classify(&shape(false, 30_000, 2), "voice/v_001.at3", &t), SoundKind::Se, "a voice line is not music");
    assert_eq!(classify(&shape(false, 400_000, 2), "VOICE/event_01.adx", &t), SoundKind::Se, "however long it runs");
    assert_eq!(classify(&shape(false, 4_000, 2), "music/sting01.ads", &t), SoundKind::Jingle);
    assert_eq!(classify(&shape(false, 90_000, 2), "Data/Sound/stage1.wma", &t), SoundKind::Bgm, "a Sound folder holds music too");
    assert_eq!(classify(&shape(false, 400_000, 2), "SE/ambience.adx", &t), SoundKind::Bgm, "far past the BGM length, the folder does not decide");
    assert_eq!(classify(&shape(false, 400_000, 1), "x/talk.adx", &t), SoundKind::Jingle, "long mono without a loop is likely speech");
    assert_eq!(classify(&shape(false, 30_000, 1), "bgm/b01.adx", &t), SoundKind::Bgm, "unless its name says music");
    assert_eq!(classify(&shape(false, 10_000, 2), "STREAM/s_03.adx", &t), SoundKind::Jingle, "a stream folder is not a BGM name");
}

fn by_name(tracks: &[rom_audio::FoundTrack], name: &str) -> rom_audio::FoundTrack {
    tracks.iter().find(|t| t.name.eq_ignore_ascii_case(name)).unwrap_or_else(|| panic!("{name} not in {tracks:#?}")).clone()
}

#[test]
fn an_iso_is_indexed_and_its_tracks_play_from_the_image() {
    if !need_lib() {
        return;
    }
    let dir = TempDir::new("iso");
    let iso = dir.path("game.iso");
    std::fs::write(&iso, game_audio::iso9660(&sound_tree())).unwrap();
    let tracks = rom_audio::index_rom(&iso, &Thresholds::default(), |_| {}).unwrap();
    assert_eq!(tracks.len(), 3, "the readme is not a sound: {tracks:#?}");
    let title = by_name(&tracks, "TITLE.WAV");
    assert_eq!((title.kind, title.looping, title.duration_ms, title.channels), (SoundKind::Bgm, true, 25_000, 2));
    assert_eq!(title.location, TrackLocation::File { view: 0, path: "SOUND/BGM/TITLE.WAV".into(), subsong: 0 });
    assert_eq!(by_name(&tracks, "FANFARE.WAV").kind, SoundKind::Jingle);
    assert_eq!(by_name(&tracks, "synthetic").kind, SoundKind::Se, "a VAG is named by the name in its header");

    let mut stream = rom_audio::open_track(&iso, &title.location, vgmstream::PlayConfig::AS_STORED).unwrap();
    assert_eq!((stream.sample_rate(), stream.channels()), (22_050, 2));
    let mut total = 0usize;
    let mut first = None;
    while let Some(chunk) = stream.next_chunk().unwrap() {
        first.get_or_insert_with(|| chunk[..64].to_vec());
        total += chunk.len();
    }
    assert_eq!(total, 22_050 * 25 * 2);
    assert_eq!(first.unwrap(), game_audio::tone(22_050, 2, 32, 440.0), "the samples are the ones stored on the disc");
}

#[test]
fn a_cue_sheet_gives_its_cd_audio_tracks_and_its_data_track() {
    if !need_lib() {
        return;
    }
    let dir = TempDir::new("cue");
    std::fs::write(dir.path("game (Track 1).bin"), game_audio::raw_2352(&game_audio::iso9660(&sound_tree()))).unwrap();
    std::fs::write(dir.path("game (Track 2).bin"), game_audio::cdda(72.0)).unwrap();
    std::fs::write(dir.path("game (Track 3).bin"), game_audio::cdda(0.5)).unwrap();
    let cue = dir.path("game.cue");
    std::fs::write(
        &cue,
        "FILE \"game (Track 1).bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\nFILE \"game (Track 2).bin\" BINARY\n  TRACK 02 AUDIO\n    INDEX 00 00:00:00\n    INDEX 01 00:02:00\nFILE \"game (Track 3).bin\" BINARY\n  TRACK 03 AUDIO\n    INDEX 01 00:00:00\n",
    )
    .unwrap();
    let tracks = rom_audio::index_rom(&cue, &Thresholds::default(), |_| {}).unwrap();
    let cd = by_name(&tracks, "Track 02");
    assert_eq!(cd.location, TrackLocation::Cdda { track: 2 });
    assert_eq!(cd.kind, SoundKind::Bgm);
    assert_eq!(cd.duration_ms / 1000, 70, "played from INDEX 01, two seconds in");
    assert!(tracks.iter().all(|t| t.name != "Track 03"), "a half-second track is skipped");
    assert_eq!(by_name(&tracks, "TITLE.WAV").location, TrackLocation::File { view: 0, path: "SOUND/BGM/TITLE.WAV".into(), subsong: 0 });

    let mut stream = rom_audio::open_track(&cue, &cd.location, vgmstream::PlayConfig::LISTEN).unwrap();
    assert_eq!((stream.sample_rate(), stream.channels()), (44_100, 2));
    let first = stream.next_chunk().unwrap().unwrap();
    let skipped = 2 * 44_100;
    let expected = game_audio::tone(44_100, 2, skipped + 16, 330.0);
    assert_eq!(&first[..32], &expected[skipped * 2..skipped * 2 + 32]);
}

#[test]
fn a_folder_dump_is_indexed() {
    if !need_lib() {
        return;
    }
    let dir = TempDir::new("folder");
    let root = dir.path("GAME00001");
    std::fs::create_dir_all(root.join("USRDIR/sound")).unwrap();
    std::fs::write(root.join("USRDIR/sound/stage.wav"), game_audio::wav(48_000, 2, &game_audio::tone(48_000, 2, 48_000 * 4, 200.0), Some((0, 48_000 * 4 - 1)))).unwrap();
    std::fs::write(root.join("USRDIR/eboot.bin"), b"code".repeat(100)).unwrap();
    let tracks = rom_audio::index_rom(&root, &Thresholds::default(), |_| {}).unwrap();
    assert_eq!(tracks.len(), 1, "{tracks:#?}");
    assert_eq!(tracks[0].location, TrackLocation::File { view: 0, path: "USRDIR/sound/stage.wav".into(), subsong: 0 });
    let mut stream = rom_audio::open_track(&root, &tracks[0].location, vgmstream::PlayConfig::AS_STORED).unwrap();
    assert_eq!(stream.total_samples(), Some(48_000 * 4));
    assert!(stream.next_chunk().unwrap().is_some());
}

/// An AFS archive (Dreamcast, Naomi) of `members`, 2048-aligned like the real ones.
fn afs(members: &[Vec<u8>]) -> Vec<u8> {
    let align = |n: usize| n.div_ceil(2048) * 2048;
    let mut out = b"AFS\0".to_vec();
    out.extend_from_slice(&(members.len() as u32).to_le_bytes());
    let mut at = align(8 + members.len() * 8 + 8);
    for m in members {
        out.extend_from_slice(&(at as u32).to_le_bytes());
        out.extend_from_slice(&(m.len() as u32).to_le_bytes());
        at = align(at + m.len());
    }
    for m in members {
        out.resize(align(out.len()), 0);
        out.extend_from_slice(m);
    }
    out
}

#[test]
fn afs_members_are_indexed_and_play_and_videos_are_marked() {
    if !need_lib() {
        return;
    }
    let song = game_audio::wav(22_050, 2, &game_audio::tone(22_050, 2, 22_050 * 30, 330.0), None);
    let archive = afs(&[b"not audio at all".repeat(10), song.clone()]);
    let iso = game_audio::iso9660(&[Entry::Dir("SOUND", vec![Entry::File("BGM.AFS", archive)]), Entry::File("INTRO.XMV", song)]);
    let dir = TempDir::new("afs");
    let image = dir.path("game.iso");
    std::fs::write(&image, iso).unwrap();
    let tracks = rom_audio::index_rom(&image, &Thresholds::default(), |_| {}).unwrap();
    assert_eq!(tracks.len(), 2, "one playable member and the video: {tracks:#?}");
    let member = tracks.iter().find(|t| !t.video).expect("the AFS member");
    assert_eq!(member.location, TrackLocation::File { view: 0, path: "SOUND/BGM.AFS#1.wav".into(), subsong: 0 });
    assert_eq!((member.kind, member.duration_ms), (SoundKind::Bgm, 30_000));
    let video = tracks.iter().find(|t| t.video).expect("the video's sound, marked");
    assert!(matches!(&video.location, TrackLocation::File { path, .. } if path == "INTRO.XMV"), "{video:?}");
    let mut stream = rom_audio::open_track(&image, &member.location, vgmstream::PlayConfig::AS_STORED).unwrap();
    assert_eq!(stream.total_samples(), Some(22_050 * 30));
    assert!(stream.next_chunk().unwrap().is_some());
}

#[test]
fn wav_header_matches_the_samples() {
    let h = rom_audio::wav_header(44_100, 2, 1000);
    assert_eq!(h.len(), 44);
    assert_eq!(&h[40..44], &4000u32.to_le_bytes());
    let _ = Path::new("");
}

#[test]
fn a_track_is_decoded_into_a_flac_for_the_player() {
    if !need_lib() {
        return;
    }
    use romexplorer::api::audio_convert::Target;
    let dir = TempDir::new("cache");
    let iso = dir.path("game.iso");
    std::fs::write(&iso, game_audio::iso9660(&sound_tree())).unwrap();
    let tracks = rom_audio::index_rom(&iso, &Thresholds::default(), |_| {}).unwrap();
    let title = by_name(&tracks, "TITLE.WAV");
    let out = dir.path("cache/rom1-flac-0.flac");
    romexplorer::api::rom_audio::decode_to(&iso, &title.location, &out, Target::Flac, vgmstream::PlayConfig::LISTEN, false).unwrap();
    assert!(!dir.path("cache/rom1-flac-0.flac.part").exists(), "only the whole file is left");
    let mut samples = 0usize;
    let (rate, channels) = romexplorer::rom::audio_decode::decode_file_with(&out, |pcm, _, _| {
        samples += pcm.len();
        Ok(())
    })
    .unwrap();
    assert_eq!((rate, channels), (22_050, 2));
    // Played as a game plays it: the loop twice and a ten-second fade, not the 25 s stored.
    let seconds = samples as f64 / 2.0 / 22_050.0;
    assert!((55.0..=65.0).contains(&seconds), "{seconds} s");
}

#[test]
fn a_gdi_reads_its_high_density_area_as_one_filesystem() {
    if !need_lib() {
        return;
    }
    let dir = TempDir::new("gdi");
    // The filesystem starts in track 3 and its files carry on in track 5, after an audio
    // track: split, as a GD-ROM is.
    let area = game_audio::iso9660_at(&sound_tree(), 45_000);
    let split = 22 * 2048;
    let (head, tail) = area.split_at(split);
    std::fs::write(dir.path("track01.bin"), game_audio::raw_2352(&game_audio::iso9660(&[Entry::File("ABS.TXT", b"low density".repeat(10))]))).unwrap();
    std::fs::write(dir.path("track02.raw"), game_audio::cdda(4.0)).unwrap();
    std::fs::write(dir.path("track03.bin"), game_audio::raw_2352(head)).unwrap();
    std::fs::write(dir.path("track04.raw"), game_audio::cdda(12.0)).unwrap();
    std::fs::write(dir.path("track05.bin"), game_audio::raw_2352(tail)).unwrap();
    let gdi = dir.path("disc.gdi");
    std::fs::write(&gdi, "5\n1 0 4 2352 track01.bin 0\n2 600 0 2352 track02.raw 0\n3 45000 4 2352 track03.bin 0\n4 45022 0 2352 track04.raw 0\n5 45022 4 2352 track05.bin 0\n").unwrap();
    let tracks = rom_audio::index_rom(&gdi, &Thresholds::default(), |_| {}).unwrap();
    assert_eq!(by_name(&tracks, "Track 04").duration_ms / 1000, 12, "a gdi track is its whole file");
    assert_eq!(by_name(&tracks, "Track 02").duration_ms / 1000, 4);
    let title = by_name(&tracks, "TITLE.WAV");
    assert_eq!(title.location, TrackLocation::File { view: 1, path: "SOUND/BGM/TITLE.WAV".into(), subsong: 0 });
    let mut stream = rom_audio::open_track(&gdi, &title.location, vgmstream::PlayConfig::AS_STORED).unwrap();
    let first = stream.next_chunk().unwrap().unwrap();
    assert_eq!(&first[..64], &game_audio::tone(22_050, 2, 32, 440.0)[..], "read across both data tracks");
}

/// The codecs vgmstream hands to FFmpeg (WMA on Xbox, AAC, AC-3, Opus...). Skips when
/// rvgmstream was built without FFmpeg, or when no `ffmpeg` command is on PATH to make the
/// samples: FFmpeg's own encoders are not in our audio-only build.
#[test]
fn ffmpeg_codecs_decode_when_linked_in() {
    if !need_lib() {
        return;
    }
    if !vgmstream::has_ffmpeg() {
        eprintln!("skipped: rvgmstream was built without FFmpeg (deps/ffmpeg/scripts/build.sh)");
        return;
    }
    let dir = std::env::temp_dir().join(format!("romexplorer_test_rom_audio_ffmpeg_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (ext, codec) in [("wma", "wmav2"), ("ac3", "ac3"), ("m4a", "aac"), ("opus", "libopus")] {
        let path = dir.join(format!("tone.{ext}"));
        let made = std::process::Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=440:duration=2:sample_rate=48000", "-ac", "2", "-c:a", codec])
            .arg(&path)
            .status();
        if !matches!(made, Ok(s) if s.success()) {
            eprintln!("skipped {ext}: the ffmpeg command could not make a sample");
            continue;
        }
        let name = format!("tone.{ext}");
        let bytes = std::fs::read(&path).unwrap();
        let mut stream = Stream::open(Box::new(MemorySource::new(&name, bytes)), &name, 0, PlayConfig::AS_STORED).unwrap_or_else(|| panic!("vgmstream opens {name}"));
        let info = stream.info().clone();
        assert_eq!(info.channels, 2, "{name}: {info:?}");
        let pcm = decode_all(&mut stream).unwrap();
        let seconds = pcm.len() as f64 / 2.0 / info.sample_rate as f64;
        assert!((1.9..2.2).contains(&seconds), "{name}: {seconds} s");
        assert!(pcm.iter().any(|&s| s.abs() > 1000), "{name} decodes to silence");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Indexes `image` (written as `name`) and returns the tracks with their names.
fn index_image(name: &str, image: Vec<u8>) -> (TempDir, PathBuf, Vec<rom_audio::FoundTrack>) {
    let dir = TempDir::new("inner");
    let path = dir.path(name);
    std::fs::write(&path, image).unwrap();
    let tracks = rom_audio::index_rom(&path, &Thresholds::default(), |_| {}).unwrap();
    (dir, path, tracks)
}

fn plays(path: &Path, track: &rom_audio::FoundTrack, samples: u64) {
    let mut stream = rom_audio::open_track(path, &track.location, vgmstream::PlayConfig::AS_STORED).unwrap();
    assert_eq!(stream.total_samples(), Some(samples));
    assert!(stream.next_chunk().unwrap().is_some());
}

#[test]
fn ds_sound_archive_streams_are_found_on_the_cartridge_and_named() {
    if !need_lib() {
        return;
    }
    let pcm = game_audio::tone(16_000, 1, 16_000 * 25, 330.0);
    let archive = game_audio::sdat(&[("STRM_TITLE", game_audio::nds_strm(16_000, &pcm))]);
    let (_dir, path, tracks) = index_image("game.nds", game_audio::nds("sound", &[("sound_data.sdat", archive)]));
    assert_eq!(tracks.len(), 1, "{tracks:#?}");
    let t = &tracks[0];
    assert_eq!(t.location, TrackLocation::File { view: 0, path: "sound/sound_data.sdat#0.strm".into(), subsong: 0 });
    assert_eq!((t.name.as_str(), t.kind, t.looping), ("STRM_TITLE", SoundKind::Bgm, true));
    plays(&path, t, 16_000 * 25);
}

#[test]
fn gamecube_archives_stored_or_yaz0_give_up_their_sounds() {
    if !need_lib() {
        return;
    }
    let song = game_audio::wav(22_050, 2, &game_audio::tone(22_050, 2, 22_050 * 30, 330.0), None);
    let arc = game_audio::rarc(&[("readme.txt", b"not audio".repeat(20)), ("stage_bgm.wav", song.clone())]);
    let disc = game_audio::gamecube(&[("plain.arc", arc.clone()), ("packed.szs", game_audio::yaz0_stored(&arc))]);
    let (_dir, path, tracks) = index_image("game.iso", disc);
    let mut paths: Vec<_> = tracks.iter().map(|t| match &t.location {
        TrackLocation::File { path, .. } => path.clone(),
        other => panic!("{other:?}"),
    }).collect();
    paths.sort();
    assert_eq!(paths, ["audio/packed.szs#1.wav", "audio/plain.arc#1.wav"]);
    assert!(tracks.iter().all(|t| t.name == "stage_bgm.wav" && t.kind == SoundKind::Bgm), "{tracks:#?}");
    for t in &tracks {
        plays(&path, t, 22_050 * 30);
    }
}

#[test]
fn cpk_members_are_found_by_their_table_of_contents() {
    if !need_lib() {
        return;
    }
    let song = game_audio::wav(22_050, 2, &game_audio::tone(22_050, 2, 22_050 * 30, 330.0), None);
    let cpk = game_audio::cpk(&[("data", "font.bin", b"no".repeat(100)), ("bgm", "title.wav", song)]);
    let iso = game_audio::iso9660(&[Entry::File("DATA.CPK", cpk)]);
    let (_dir, path, tracks) = index_image("game.iso", iso);
    assert_eq!(tracks.len(), 1, "{tracks:#?}");
    assert_eq!(tracks[0].location, TrackLocation::File { view: 0, path: "DATA.CPK#1.wav".into(), subsong: 0 });
    assert_eq!(tracks[0].name, "title.wav");
    plays(&path, &tracks[0], 22_050 * 30);
}

#[test]
fn a_decrypted_3ds_image_has_its_romfs_read() {
    if !need_lib() {
        return;
    }
    let song = game_audio::wav(32_000, 2, &game_audio::tone(32_000, 2, 32_000 * 30, 330.0), None);
    let (_dir, path, tracks) = index_image("game.3ds", game_audio::ctr_romfs(&[("notes.bin", b"x".repeat(300)), ("field.wav", song)]));
    assert_eq!(tracks.len(), 1, "{tracks:#?}");
    assert_eq!(tracks[0].location, TrackLocation::File { view: 0, path: "field.wav".into(), subsong: 0 });
    plays(&path, &tracks[0], 32_000 * 30);
    let mut encrypted = game_audio::ctr_romfs(&[]);
    encrypted[0x4000 + 0x18F] = 0;
    let dir = TempDir::new("enc");
    let p = dir.path("enc.3ds");
    std::fs::write(&p, encrypted).unwrap();
    assert!(rom_audio::index_rom(&p, &Thresholds::default(), |_| {}).unwrap_err().contains("encrypted"));
}
