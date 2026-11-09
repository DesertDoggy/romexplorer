//! Music sequenced inside a cartridge ROM, found by reading the game's own sound engine and
//! played from the ROM by song number: no rip, no copy.
//!
//! Engines so far: GBA games made with Nintendo's standard engine, through [`super::agbplay`],
//! and the sequences in a DS game's sound archive (SDAT), through [`super::sseq`].
//!
//! A track is stored like a rip-set track (`location = 'file'`), with [`ROM_SONG_VIEW`] as its
//! data view, the engine (and where it found the songs) as its path, and the song number as its
//! subsong. Playback opens the engine again from that, without searching the ROM.

use std::path::{Path, PathBuf};

use super::agbplay;
use super::music_search::{self, Machine};
use super::rom_audio::{classify, FoundTrack, PcmStream, Shape, SoundKind, Thresholds, TrackLocation};

/// The data view a ROM song is stored under: no disc has this many data tracks, and it is one
/// below the rip-set view.
pub const ROM_SONG_VIEW: u32 = u16::MAX as u32 - 1;

/// The largest ROM read whole: GBA cartridges are 32 MB at most.
const MAX_ROM_BYTES: u64 = 32 << 20;
/// Songs are measured at this rate: the length doesn't depend on it, and it is far cheaper.
const MEASURE_RATE: u32 = 8000;
/// A song still playing after this long is music that runs on; it is measured no further.
const MEASURE_CAP_MS: u64 = 150_000;
/// Played back at this rate.
const PLAY_RATE: u32 = 48000;
/// How many times a looping song plays its loop before it ends.
const PLAY_LOOPS: u8 = 2;

/// The engine a stored path names.
const GBA_M4A: &str = "gba-m4a";
/// `nes@<recipe>@<body ms>@<fade ms>`: the game's own sound code (see [`super::nes_music`]).
const NES: &str = "nes";
/// `nesmap@<map key>@<body ms>@<fade ms>`: a NES song played from the ROM by its music map
/// ([`super::nes_nsf::Map`], kept in the maps folder by [`map_key`]), with no boot.
const NES_MAP: &str = "nesmap";
/// `gbmap@<map key>@<body ms>@<fade ms>`: the same for the Game Boy ([`super::gb_map::Map`]).
const GB_MAP: &str = "gbmap";
/// `sgcmap@<map key>@<body ms>@<fade ms>`: the same for the Master System and Game Gear
/// ([`super::sgc_map::Map`]).
const SGC_MAP: &str = "sgcmap";
/// `hesmap@<map key>@<body ms>@<fade ms>`: the same for the PC Engine
/// ([`super::hes_map::Map`]).
const HES_MAP: &str = "hesmap";
/// `gb@<recipe>@<body ms>@<fade ms>`: the same, for the Game Boy and Game Boy Color.
const GB: &str = "gb";
/// The same, for the Master System and the Game Gear.
const SMS: &str = "sms";
const GG: &str = "gg";
/// The same, for the Mega Drive.
const MD: &str = "md";
/// The same, for the PC Engine.
const PCE: &str = "pce";
/// The same, for the MSX.
const MSX: &str = "msx";
/// The same, for the WonderSwan and WonderSwan Color.
const WS: &str = "ws";
/// The same, for the Virtual Boy.
const VB: &str = "vb";
/// The same, for Game Boy Advance games not on the standard sound engine.
const GBA: &str = "gba";
/// The same, for the Neo Geo Pocket and Neo Geo Pocket Color.
const NGP: &str = "ngp";
/// How long a looping song from a game's own sound code fades out.
const SONG_FADE_MS: u64 = 8_000;
/// `nds-sseq@<data view>@<body ms>@<fade ms>@<sound archive's path on the disc>`.
const NDS_SSEQ: &str = "nds-sseq";
/// DS songs are listened to at this rate to time them: enough to hear the drums' level.
const SSEQ_MEASURE_RATE: u32 = 16_000;
/// How long a DS song is listened to for its loop.
const SSEQ_LISTEN_MS: u64 = 120_000;
/// A DS song that neither ends nor is heard to repeat is faded here.
const SSEQ_RUN_ON_MS: u64 = 150_000;
/// How a looping DS song fades out.
const SSEQ_FADE_MS: u64 = 8_000;
/// How far into a song its sequence is followed to find its loop: past any intro and two
/// passes of the longest loop.
const SSEQ_TIMING_MAX_S: f64 = 900.0;
/// A loop shorter than this is not taken from the sequence (a pulse, not the song's loop): the
/// song is listened to instead.
const SSEQ_MIN_LOOP_MS: u64 = 1_000;
/// The largest sound archive read: they are tens of megabytes at most.
pub const MAX_SDAT_BYTES: u64 = 128 << 20;

/// Which engines can read ROMs of `platform` (`platforms.id`) on this machine.
pub fn covers(platform: u16) -> bool {
    use crate::common::models::Platform as P;
    (platform == P::GameBoyAdvance as u16 && (agbplay::available() || super::gb_apu::available())) || (is_nes(platform) && super::nes_apu::available())
        || (is_gb(platform) && super::gb_apu::available())
        || ((platform == P::SegaMasterSystem as u16 || platform == P::SegaGameGear as u16 || platform == P::SegaGenesis as u16
            || platform == P::TurboGrafx16 as u16
            || platform == P::Msx as u16
            || platform == P::Msx2 as u16
            || platform == P::WonderSwan as u16
            || platform == P::WonderSwanColor as u16
            || platform == P::VirtualBoy as u16
            || platform == P::NeoGeoPocket as u16
            || platform == P::NeoGeoPocketColor as u16)
            && super::vgm_chips::available())
}

fn is_gb(platform: u16) -> bool {
    use crate::common::models::Platform as P;
    platform == P::GameBoy as u16 || platform == P::GameBoyColor as u16
}

fn is_nes(platform: u16) -> bool {
    use crate::common::models::Platform as P;
    platform == P::Nes as u16 || platform == P::Famicom as u16 || platform == P::FamicomDiskSystem as u16
}

/// How many ROM images [`read_rom`] keeps in memory. A cartridge is at most
/// [`MAX_ROM_BYTES`]; playing through an album opens the same one for every track, and a zip
/// or 7z would otherwise be unpacked again each time.
const KEPT_ROMS: usize = 4;

/// A ROM's bytes, kept for the next track: see [`KEPT_ROMS`]. A file that changed (size or
/// time) is read again.
pub fn read_rom(path: &Path) -> Result<std::sync::Arc<Vec<u8>>, String> {
    type Kept = std::collections::VecDeque<((PathBuf, u64, Option<std::time::SystemTime>), std::sync::Arc<Vec<u8>>)>;
    static KEPT: std::sync::Mutex<Kept> = std::sync::Mutex::new(Kept::new());
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let key = (path.to_path_buf(), meta.len(), meta.modified().ok());
    {
        let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = kept.iter().position(|(k, _)| *k == key) {
            let hit = kept.remove(i).expect("found above");
            let bytes = hit.1.clone();
            kept.push_back(hit);
            return Ok(bytes);
        }
    }
    let bytes = std::sync::Arc::new(read_rom_uncached(path)?);
    let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    kept.push_back((key, bytes.clone()));
    while kept.len() > KEPT_ROMS {
        kept.pop_front();
    }
    Ok(bytes)
}

/// A ROM's bytes: the file itself, or the one ROM inside a zip / 7z.
fn read_rom_uncached(path: &Path) -> Result<Vec<u8>, String> {
    let name = path.to_string_lossy().to_ascii_lowercase();
    let format = if name.ends_with(".zip") {
        Some(super::sevenzip_wrapper::ArchiveFormat::Zip)
    } else if name.ends_with(".7z") {
        Some(super::sevenzip_wrapper::ArchiveFormat::SevenZ)
    } else {
        None
    };
    let Some(format) = format else {
        if std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) > MAX_ROM_BYTES {
            return Err(format!("{} is larger than a cartridge", path.display()));
        }
        return std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()));
    };
    let archive = super::sevenzip_wrapper::SevenZipArchive::open_variant(path, format, super::native_lib::LibVariant::Streaming, |_, _| true).map_err(|e| e.to_string())?;
    let mut best: Option<Vec<u8>> = None;
    for i in 0..archive.entry_count() {
        let Ok(entry) = archive.entry(i) else { continue };
        if entry.is_dir || entry.size > MAX_ROM_BYTES || best.as_ref().is_some_and(|b| b.len() as u64 >= entry.size) {
            continue;
        }
        let mut bytes = Vec::with_capacity(entry.size as usize);
        archive
            .extract_stream(i, None, |_, _, chunk| {
                bytes.extend_from_slice(chunk);
                true
            }, |_, _| true)
            .map_err(|e| e.to_string())?;
        best = Some(bytes);
    }
    best.ok_or_else(|| format!("{} holds no ROM", path.display()))
}

/// How long song `number` plays through once, in ms, capped at [`MEASURE_CAP_MS`].
fn measure(player: &mut agbplay::Player, number: u32) -> Result<u64, String> {
    player.start(number, 0)?;
    let mut buf = vec![0i16; MEASURE_RATE as usize / 5 * 2];
    let mut frames = 0u64;
    let cap = MEASURE_CAP_MS * u64::from(MEASURE_RATE) / 1000;
    while frames < cap {
        let n = player.render(&mut buf) as u64;
        frames += n;
        if n < buf.len() as u64 / 2 {
            break;
        }
    }
    Ok(frames * 1000 / u64::from(MEASURE_RATE))
}

/// The songs in the ROM at `path`, measured and classified. Empty when no engine on this
/// machine reads the ROM's music.
pub fn index(path: &Path, platform: u16, t: &Thresholds) -> Result<Vec<FoundTrack>, String> {
    index_explained(path, platform, t).map(|(found, _)| found)
}

/// [`index`], and when nothing was found, why.
pub fn index_explained(path: &Path, platform: u16, t: &Thresholds) -> Result<(Vec<FoundTrack>, Option<String>), String> {
    if !covers(platform) {
        return Ok((Vec::new(), None));
    }
    if is_nes(platform) {
        return index_nes(path, t);
    }
    if is_gb(platform) {
        let rom = read_rom(path)?;
        if let Some(m) = load_gb_map(&gb_key(&rom)) {
            let found = index_gb_map(&rom, &gb_key(&rom), &m, t)?;
            if !found.is_empty() {
                return Ok((found, None));
            }
        }
        return index_machine::<super::gb_music::GameBoy>(path, t, GB, "Game Boy");
    }
    {
        use crate::common::models::Platform as P;
        if platform == P::SegaMasterSystem as u16 || platform == P::SegaGameGear as u16 {
            let rom = read_rom(path)?;
            let key = gb_key(&rom);
            if let Some(m) = load_sgc_map(&key) {
                let found = index_sgc_map(&rom, &key, &m, t)?;
                if !found.is_empty() {
                    return Ok((found, None));
                }
            }
        }
        if platform == P::SegaMasterSystem as u16 {
            return index_machine::<super::sms_music::MasterSystem>(path, t, SMS, "Master System");
        }
        if platform == P::SegaGameGear as u16 {
            return index_machine::<super::sms_music::GameGear>(path, t, GG, "Game Gear");
        }
        if platform == P::SegaGenesis as u16 {
            return index_machine::<super::md_music::MegaDrive>(path, t, MD, "Mega Drive");
        }
        if platform == P::TurboGrafx16 as u16 {
            let rom = read_rom(path)?;
            let key = gb_key(super::hes_map::program(&rom));
            if let Some(m) = load_hes_map(&key) {
                let found = index_hes_map(&rom, &key, &m, t)?;
                if !found.is_empty() {
                    return Ok((found, None));
                }
            }
            return index_machine::<super::pce_music::PcEngine>(path, t, PCE, "PC Engine");
        }
        if platform == P::Msx as u16 || platform == P::Msx2 as u16 {
            return index_machine::<super::msx_music::Msx>(path, t, MSX, "MSX");
        }
        if platform == P::WonderSwan as u16 || platform == P::WonderSwanColor as u16 {
            return index_machine::<super::wswan_music::WonderSwan>(path, t, WS, "WonderSwan");
        }
        if platform == P::VirtualBoy as u16 {
            return index_machine::<super::vb_music::VirtualBoy>(path, t, VB, "Virtual Boy");
        }
        if platform == P::NeoGeoPocket as u16 || platform == P::NeoGeoPocketColor as u16 {
            return index_machine::<super::ngp_music::NeoGeoPocket>(path, t, NGP, "Neo Geo Pocket");
        }
    }
    let rom = read_rom(path)?;
    let mut player = match agbplay::Player::open(&rom, MEASURE_RATE) {
        Ok(p) => p,
        // No standard engine: the game's own sound code is run instead.
        Err(_) if super::gb_apu::available() => return index_machine::<super::gba_music::GameBoyAdvance>(path, t, GBA, "Game Boy Advance"),
        Err(_) => return Ok((Vec::new(), Some("this game does not use the sound engine rommusic plays Game Boy Advance music with".into()))),
    };
    let songs = player.songs();
    // The music player most multi-track songs use carries the BGM; the others carry effects.
    let mut by_player: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    songs.iter().filter(|s| s.tracks > 1).for_each(|s| *by_player.entry(s.player).or_default() += 1);
    let bgm_player = by_player.into_iter().max_by_key(|(_, n)| *n).map(|(p, _)| p);
    let recipe = format!("{GBA_M4A}@{:x}", player.table_offset());
    let mut found = Vec::with_capacity(songs.len());
    for s in songs {
        let duration_ms = measure(&mut player, s.number).unwrap_or(0);
        // Kinds are worked out again at play time from the name and length, so a song on an
        // effects player says so in its name.
        // A long song on another player is music all the same, and is named so.
        let shape = Shape { looping: false, duration_ms, channels: 2, cd_track: 0, rip: true };
        let mut name = format!("Track {}", s.number);
        let mut kind = classify(&shape, &name, t);
        if bgm_player.is_some_and(|p| p != s.player) && kind != SoundKind::Bgm {
            name = format!("SE {}", s.number);
            kind = classify(&shape, &name, t);
        }
        found.push(FoundTrack {
            location: TrackLocation::RomSong { engine: recipe.clone(), song: s.number },
            name,
            kind,
            sample_rate: PLAY_RATE,
            channels: 2,
            duration_ms,
            looping: false,
            loop_start: 0,
            loop_end: 0,
            codec: "GBA m4a".into(),
            format: format!("{} tracks", s.tracks),
            bytes: 0,
            video: false,
        });
    }
    let why = found.is_empty().then(|| "this game does not use the sound engine rommusic plays Game Boy Advance music with".to_string());
    Ok((found, why))
}

struct AgbPcm(agbplay::Player);

impl PcmStream for AgbPcm {
    fn sample_rate(&self) -> u32 {
        self.0.sample_rate()
    }
    fn channels(&self) -> u16 {
        2
    }
    fn total_samples(&self) -> Option<u64> {
        None
    }
    fn next_chunk(&mut self) -> Result<Option<Vec<i16>>, String> {
        if self.0.ended() {
            return Ok(None);
        }
        let mut buf = vec![0i16; 8192];
        let n = self.0.render(&mut buf);
        if n == 0 {
            return Ok(None);
        }
        buf.truncate(n * 2);
        Ok(Some(buf))
    }
}

/// The songs in a DS sound archive (`sdat`, stored at `path` on data view `view`). A song whose
/// sequence jumps back plays that loop twice and fades; the rest are listened to once
/// ([`super::track_timing`]) and play to where they end.
pub fn index_sdat(sdat: &[u8], view: u32, path: &str, t: &Thresholds) -> Vec<FoundTrack> {
    let Ok(songs) = super::sseq::songs(sdat) else { return Vec::new() };
    let mut found = Vec::with_capacity(songs.len());
    for song in songs {
        // The sequence says where it jumps back, exactly and without playing it.
        if let Ok(super::sseq::SongTiming::Loops { intro, period }) = super::sseq::timing(sdat, song.id, SSEQ_TIMING_MAX_S) {
            let (intro, lp) = ((intro * 1000.0) as u64, (period * 1000.0) as u64);
            if lp >= SSEQ_MIN_LOOP_MS {
                found.push(sseq_track(&song, view, path, t, (intro + 2 * lp, SSEQ_FADE_MS, true, intro, lp)));
                continue;
            }
        }
        let Ok(mut player) = super::sseq::Player::open(sdat, song.id, SSEQ_MEASURE_RATE) else { continue };
        let mut ear = super::track_timing::Listener::new(SSEQ_MEASURE_RATE);
        let mut buf = vec![0i16; 4096];
        while !player.ended() && ear.played_ms() < SSEQ_LISTEN_MS && !ear.fell_silent() {
            if player.render(&mut buf).is_err() {
                break;
            }
            ear.feed(&buf);
        }
        let (body, fade, looping, intro, lp) = match ear.finish(player.ended()) {
            super::track_timing::Timing::Silent => continue,
            super::track_timing::Timing::Ends { ms } => (ms, 0, false, 0, 0),
            super::track_timing::Timing::Loops { start_ms, loop_ms, .. } => (start_ms + 2 * loop_ms, SSEQ_FADE_MS, true, start_ms, loop_ms),
            super::track_timing::Timing::RunsOn => (SSEQ_RUN_ON_MS, SSEQ_FADE_MS, true, 0, 0),
        };
        found.push(sseq_track(&song, view, path, t, (body, fade, looping, intro, lp)));
    }
    found
}

/// The index entry of DS song `song`, played `body` ms then faded over `fade`; `intro`/`lp`
/// where it loops.
fn sseq_track(song: &super::sseq::Song, view: u32, path: &str, t: &Thresholds, (body, fade, looping, intro, lp): (u64, u64, bool, u64, u64)) -> FoundTrack {
    let duration_ms = if looping && lp > 0 { intro + lp } else { body };
    let name = if song.name.is_empty() { format!("Song {}", song.id) } else { song.name.clone() };
    let shape = Shape { looping, duration_ms, channels: 2, cd_track: 0, rip: true };
    let kind = classify(&shape, &format!("{path}/{name}"), t);
    let rate = u64::from(PLAY_RATE);
    FoundTrack {
        location: TrackLocation::RomSong { engine: format!("{NDS_SSEQ}@{view}@{body}@{fade}@{path}"), song: song.id },
        name,
        kind,
        sample_rate: PLAY_RATE,
        channels: 2,
        duration_ms,
        looping,
        loop_start: intro * rate / 1000,
        loop_end: (intro + lp) * rate / 1000,
        codec: "DS sequence".into(),
        format: "SSEQ".into(),
        bytes: 0,
        video: false,
    }
}

/// A DS song, played for `left` more frames, the last `fade` of them fading out.
struct SseqPcm {
    player: super::sseq::Player,
    left: u64,
    fade: u64,
    /// Silent frames after the fade.
    tail: u64,
    /// Stops at the song's own end too.
    stop_at_end: bool,
}

impl PcmStream for SseqPcm {
    fn sample_rate(&self) -> u32 {
        self.player.sample_rate()
    }
    fn channels(&self) -> u16 {
        2
    }
    fn total_samples(&self) -> Option<u64> {
        Some(self.left)
    }
    fn next_chunk(&mut self) -> Result<Option<Vec<i16>>, String> {
        if self.left == 0 || (self.stop_at_end && self.player.ended()) {
            return Ok(None);
        }
        let frames = self.left.min(4096) as usize;
        let mut buf = vec![0i16; frames * 2];
        self.player.render(&mut buf)?;
        if self.fade > 0 {
            for (i, pair) in buf.chunks_exact_mut(2).enumerate() {
                let g = super::track_timing::fade_gain(self.left - i as u64, self.fade, self.tail);
                if g < 1.0 {
                    for s in pair {
                        *s = (f32::from(*s) * g) as i16;
                    }
                }
            }
        }
        self.left -= frames as u64;
        Ok(Some(buf))
    }
}

fn open_sseq(path: &Path, engine: &str, song: u32) -> Result<Box<dyn PcmStream>, String> {
    let mut parts = engine.splitn(5, '@');
    let (_, view, body, fade, inner) = (parts.next(), parts.next(), parts.next(), parts.next(), parts.next());
    let bad = || format!("a damaged DS song record: {engine}");
    let view: u32 = view.and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let body: u64 = body.and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let fade: u64 = fade.and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let inner = inner.ok_or_else(bad)?;
    let sdat = sdat_bytes(path, view, inner)?;
    let player = super::sseq::Player::open(&sdat, song, PLAY_RATE)?;
    let rate = u64::from(PLAY_RATE);
    let tail = if fade > 0 { super::track_timing::FADE_TAIL_MS } else { 0 };
    Ok(Box::new(SseqPcm { player, left: (body + fade + tail) * rate / 1000, fade: fade * rate / 1000, tail: tail * rate / 1000, stop_at_end: fade == 0 }))
}

/// A sound archive's bytes, kept for the next song of the same game, like [`read_rom`].
fn sdat_bytes(path: &Path, view: u32, inner: &str) -> Result<std::sync::Arc<Vec<u8>>, String> {
    type Kept = Option<((PathBuf, u32, String), std::sync::Arc<Vec<u8>>)>;
    static KEPT: std::sync::Mutex<Kept> = std::sync::Mutex::new(None);
    let key = (path.to_path_buf(), view, inner.to_string());
    if let Some((k, bytes)) = KEPT.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if *k == key {
            return Ok(bytes.clone());
        }
    }
    let (_, bytes) = super::rom_audio::stored_file(path, &TrackLocation::File { view, path: inner.to_string(), subsong: 0 })?;
    let bytes = std::sync::Arc::new(bytes);
    *KEPT.lock().unwrap_or_else(|e| e.into_inner()) = Some((key, bytes.clone()));
    Ok(bytes)
}

/// Opens song `song` of the ROM at `path`, as the engine named by `engine` (a stored path)
/// plays it.
pub fn open(path: &Path, engine: &str, song: u32) -> Result<Box<dyn PcmStream>, String> {
    match engine.split('@').next() {
        Some(NDS_SSEQ) => open_sseq(path, engine, song),
        Some(NES) => open_machine::<super::nes_music::Nes>(path, engine, song),
        Some(NES_MAP) => open_map(path, engine, song),
        Some(GB_MAP) => open_gb_map(path, engine, song),
        Some(SGC_MAP) => open_sgc_map(path, engine, song),
        Some(HES_MAP) => open_hes_map(path, engine, song),
        Some(GB) => open_machine::<super::gb_music::GameBoy>(path, engine, song),
        Some(SMS) => open_machine::<super::sms_music::MasterSystem>(path, engine, song),
        Some(GG) => open_machine::<super::sms_music::GameGear>(path, engine, song),
        Some(MD) => open_machine::<super::md_music::MegaDrive>(path, engine, song),
        Some(PCE) => open_machine::<super::pce_music::PcEngine>(path, engine, song),
        Some(MSX) => open_machine::<super::msx_music::Msx>(path, engine, song),
        Some(WS) => open_machine::<super::wswan_music::WonderSwan>(path, engine, song),
        Some(VB) => open_machine::<super::vb_music::VirtualBoy>(path, engine, song),
        Some(NGP) => open_machine::<super::ngp_music::NeoGeoPocket>(path, engine, song),
        Some(GBA) => open_machine::<super::gba_music::GameBoyAdvance>(path, engine, song),
        Some(GBA_M4A) => {
            let rom = read_rom(path)?;
            let mut player = agbplay::Player::open(&rom, PLAY_RATE)?;
            player.start(song, PLAY_LOOPS)?;
            Ok(Box::new(AgbPcm(player)))
        }
        _ => Err(format!("unknown ROM music engine {engine}")),
    }
}

// --- Consoles whose own sound code plays the music (super::music_search) -----------------

fn index_nes(path: &Path, t: &Thresholds) -> Result<(Vec<FoundTrack>, Option<String>), String> {
    let rom = read_rom(path)?;
    if let Some((key, m)) = map_key(&rom).and_then(|k| Some((k.clone(), load_map(&k)?))) {
        let found = index_map(&rom, &key, &m, t)?;
        if !found.is_empty() {
            return Ok((found, None));
        }
    }
    index_machine::<super::nes_music::Nes>(path, t, NES, "NES")
}

// --- Music maps: where a game's music is in its ROM, as numbers (super::nes_nsf::Map) -----

static MAPS_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Where music maps are kept (`<dir>/nes/<key>.nesmap`). Without it, none are read or kept.
pub fn set_maps_dir(dir: PathBuf) {
    let _ = MAPS_DIR.set(dir);
}

/// What a NES ROM's map is kept under: the SHA-1 of its program data (a Disk System image's
/// disk data), so a header changed by a tool still finds it. None for anything else.
pub fn map_key(rom: &[u8]) -> Option<String> {
    use sha1::{Digest, Sha1};
    let prg = super::nes_nsf::program(rom)?;
    Some(Sha1::digest(prg.as_slice()).iter().map(|b| format!("{b:02x}")).collect())
}

fn map_path(key: &str) -> Option<PathBuf> {
    Some(MAPS_DIR.get()?.join("nes").join(format!("{key}.nesmap")))
}

fn load_map(key: &str) -> Option<super::nes_nsf::Map> {
    super::nes_nsf::Map::decode(&std::fs::read_to_string(map_path(key)?).ok()?)
}

/// Whether a map is kept for the NES ROM `rom`.
pub fn has_map(rom: &[u8]) -> bool {
    map_key(rom).and_then(|k| map_path(&k)).is_some_and(|p| p.is_file())
}

/// Builds the map of the NES ROM `rom` from the NSF it matches (only to find and check where
/// its music is; nothing of the file is kept) and keeps it. False when none plays any track.
pub fn build_nes_map(rom: &[u8], nsf: &[u8]) -> Result<bool, String> {
    let key = map_key(rom).ok_or("not an iNES cartridge")?;
    let path = map_path(&key).ok_or("no folder for music maps")?;
    let Some(m) = super::nes_nsf::build_map(nsf, rom) else { return Ok(false) };
    if m.ways.iter().all(|w| *w == super::nes_nsf::Way::None) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&path, m.encode()).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(true)
}

/// Whether ROMs of `platform` get music maps from matched sets ([`map_from_sets`]).
pub fn builds_maps(platform: u16) -> bool {
    use crate::common::models::Platform as P;
    (is_nes(platform) && super::nes_apu::available())
        || (is_gb(platform) && super::gb_apu::available())
        || ((platform == P::SegaMasterSystem as u16 || platform == P::SegaGameGear as u16 || platform == P::TurboGrafx16 as u16) && super::vgm_chips::available())
}

fn sgc_map_path(key: &str) -> Option<PathBuf> {
    Some(MAPS_DIR.get()?.join("sgc").join(format!("{key}.sgcmap")))
}

fn load_sgc_map(key: &str) -> Option<super::sgc_map::Map> {
    super::sgc_map::Map::decode(&std::fs::read_to_string(sgc_map_path(key)?).ok()?)
}

/// The map of the Master System / Game Gear ROM `rom` from the first of `sets` holding an
/// SGC, unless one is kept already.
fn sgc_map_from_sets(rom: &[u8], sets: &[PathBuf]) -> Result<(), String> {
    let file = sgc_map_path(&gb_key(rom)).ok_or("no folder for music maps")?;
    if file.is_file() {
        return Ok(());
    }
    for set in sets {
        let Some(sgc) = super::rip_sets::first_with_magic(set, b"SGC\x1a") else { continue };
        let Some(m) = super::sgc_map::build_map(&sgc, rom) else { continue };
        if m.ways.iter().all(|w| *w == super::sgc_map::Way::None) {
            continue;
        }
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        return std::fs::write(&file, m.encode()).map_err(|e| format!("{}: {e}", file.display()));
    }
    Ok(())
}

fn index_sgc_map(rom: &[u8], key: &str, m: &super::sgc_map::Map, t: &Thresholds) -> Result<Vec<FoundTrack>, String> {
    let mut out = Vec::new();
    let codec = if m.header.system == 1 { "Game Gear" } else { "Master System" };
    for v in 0..m.header.songs {
        if matches!(m.ways.get(v as usize), None | Some(super::sgc_map::Way::None)) {
            continue;
        }
        let Ok(mut song) = super::sgc_map::MapSong::open(m, rom, v, MEASURE_RATE) else { continue };
        let rate = song.frame_rate();
        let Ok(timing) = music_search::measure_frames(rate, |samples| song.frame(samples).ok_or_else(|| "the song stopped".to_string())) else { continue };
        if let Some(track) = map_track(SGC_MAP, key, v, out.len(), rate, timing, 2, codec, t) {
            out.push(track);
        }
    }
    Ok(out)
}

fn open_sgc_map(path: &Path, engine: &str, song: u32) -> Result<Box<dyn PcmStream>, String> {
    let bad = || format!("a damaged ROM song record: {engine}");
    let parts: Vec<&str> = engine.split('@').collect();
    let [_, key, body, fade] = parts[..] else { return Err(bad()) };
    let body: u64 = body.parse().map_err(|_| bad())?;
    let fade: u64 = fade.parse().map_err(|_| bad())?;
    let m = load_sgc_map(key).ok_or("this game's music map is no longer kept; scan its music again")?;
    let rom = read_rom(path)?;
    let v = u8::try_from(song).map_err(|_| bad())?;
    let song = super::sgc_map::MapSong::open(&m, &rom, v, PLAY_RATE)?;
    let r = u64::from(PLAY_RATE);
    let tail = if fade > 0 { super::track_timing::FADE_TAIL_MS } else { 0 };
    Ok(Box::new(MapPcm { song: AnyMapSong::Sgc(song), channels: 2, left: (body + fade + tail) * r / 1000, fade: fade * r / 1000, tail: tail * r / 1000, pending: Vec::new() }))
}

fn hes_map_path(key: &str) -> Option<PathBuf> {
    Some(MAPS_DIR.get()?.join("hes").join(format!("{key}.hesmap")))
}

fn load_hes_map(key: &str) -> Option<super::hes_map::Map> {
    super::hes_map::Map::decode(&std::fs::read_to_string(hes_map_path(key)?).ok()?)
}

/// The map of the PC Engine ROM `rom` from the first of `sets` holding a HES, unless one is
/// kept already. False when no set holds one.
fn hes_map_from_sets(rom: &[u8], sets: &[PathBuf]) -> Result<bool, String> {
    let file = hes_map_path(&gb_key(super::hes_map::program(rom))).ok_or("no folder for music maps")?;
    let mut any = false;
    for set in sets {
        let Some(hes) = super::rip_sets::first_with_magic(set, b"HESM") else { continue };
        any = true;
        if file.is_file() {
            return Ok(true);
        }
        let Some(m) = super::hes_map::build_map(&hes, rom) else { continue };
        if m.ways.iter().all(|w| *w == super::hes_map::Way::None) {
            continue;
        }
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&file, m.encode()).map_err(|e| format!("{}: {e}", file.display()))?;
        return Ok(true);
    }
    Ok(any)
}

fn index_hes_map(rom: &[u8], key: &str, m: &super::hes_map::Map, t: &Thresholds) -> Result<Vec<FoundTrack>, String> {
    let mut out = Vec::new();
    for v in 0..m.header.songs {
        if matches!(m.ways.get(v as usize), None | Some(super::hes_map::Way::None)) {
            continue;
        }
        let Ok(mut song) = super::hes_map::MapSong::open(m, rom, v, MEASURE_RATE) else { continue };
        let rate = song.frame_rate();
        let Ok(timing) = music_search::measure_frames(rate, |samples| song.frame(samples).ok_or_else(|| "the song stopped".to_string())) else { continue };
        if let Some(track) = map_track(HES_MAP, key, v, out.len(), rate, timing, 2, "PC Engine", t) {
            out.push(track);
        }
    }
    Ok(out)
}

fn open_hes_map(path: &Path, engine: &str, song: u32) -> Result<Box<dyn PcmStream>, String> {
    let bad = || format!("a damaged ROM song record: {engine}");
    let parts: Vec<&str> = engine.split('@').collect();
    let [_, key, body, fade] = parts[..] else { return Err(bad()) };
    let body: u64 = body.parse().map_err(|_| bad())?;
    let fade: u64 = fade.parse().map_err(|_| bad())?;
    let m = load_hes_map(key).ok_or("this game's music map is no longer kept; scan its music again")?;
    let rom = read_rom(path)?;
    let v = u8::try_from(song).map_err(|_| bad())?;
    let song = super::hes_map::MapSong::open(&m, &rom, v, PLAY_RATE)?;
    let r = u64::from(PLAY_RATE);
    let tail = if fade > 0 { super::track_timing::FADE_TAIL_MS } else { 0 };
    Ok(Box::new(MapPcm { song: AnyMapSong::Hes(song), channels: 2, left: (body + fade + tail) * r / 1000, fade: fade * r / 1000, tail: tail * r / 1000, pending: Vec::new() }))
}

/// What a Game Boy (or Master System / Game Gear) ROM's map is kept under: the SHA-1 of the
/// whole ROM.
fn gb_key(rom: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    Sha1::digest(rom).iter().map(|b| format!("{b:02x}")).collect()
}

fn gb_map_path(key: &str) -> Option<PathBuf> {
    Some(MAPS_DIR.get()?.join("gb").join(format!("{key}.gbmap")))
}

fn load_gb_map(key: &str) -> Option<super::gb_map::Map> {
    super::gb_map::Map::decode(&std::fs::read_to_string(gb_map_path(key)?).ok()?)
}

/// Builds the map of the Game Boy ROM at `path` from the first of `sets` that holds a GBS
/// (only to find and check where its music is; nothing of the file is kept), unless one is
/// kept already.
fn gb_map_from_sets(path: &Path, sets: &[PathBuf]) -> Result<(), String> {
    let rom = read_rom(path)?;
    let key = gb_key(&rom);
    let file = gb_map_path(&key).ok_or("no folder for music maps")?;
    if file.is_file() {
        return Ok(());
    }
    for set in sets {
        let Some(gbs) = super::rip_sets::first_with_magic(set, b"GBS") else { continue };
        let Some(m) = super::gb_map::build_map(&gbs, &rom) else { continue };
        if m.ways.iter().all(|w| *w == super::gb_map::Way::None) {
            continue;
        }
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        return std::fs::write(&file, m.encode()).map_err(|e| format!("{}: {e}", file.display()));
    }
    Ok(())
}

/// The songs a Game Boy map plays, measured and classified like [`index_machine`]'s.
fn index_gb_map(rom: &[u8], key: &str, m: &super::gb_map::Map, t: &Thresholds) -> Result<Vec<FoundTrack>, String> {
    let mut out = Vec::new();
    for v in 0..m.header.songs {
        if matches!(m.ways.get(v as usize), None | Some(super::gb_map::Way::None)) {
            continue;
        }
        let Ok(mut song) = super::gb_map::MapSong::open(m, rom, v, MEASURE_RATE) else { continue };
        let rate = song.frame_rate();
        let Ok(timing) = music_search::measure_frames(rate, |samples| song.frame(samples).ok_or_else(|| "the song stopped".to_string())) else { continue };
        if let Some(track) = map_track(GB_MAP, key, v, out.len(), rate, timing, 2, "Game Boy", t) {
            out.push(track);
        }
    }
    Ok(out)
}

/// A found track from a map's song `v`, timed as `timing` says (frames at `rate`).
#[allow(clippy::too_many_arguments)]
fn map_track(engine: &str, key: &str, v: u8, index: usize, rate: f64, timing: music_search::Timing, channels: u16, codec: &str, t: &Thresholds) -> Option<FoundTrack> {
    let ms = |frames: usize| (frames as f64 * 1000.0 / rate) as u64;
    let (looping, intro, lp) = match timing.looping {
        Some((start, len)) => (true, ms(start), ms(len)),
        None => (false, ms(timing.frames), 0),
    };
    let duration_ms = if looping { intro + lp } else { intro };
    if duration_ms == 0 {
        return None;
    }
    let (body, fade) = if looping { (intro + lp * u64::from(PLAY_LOOPS), SONG_FADE_MS) } else { (duration_ms, 0) };
    let name = format!("Track {}", index + 1);
    let shape = Shape { looping, duration_ms, channels, cd_track: 0, rip: true };
    let kind = classify(&shape, &name, t);
    let r = u64::from(PLAY_RATE);
    Some(FoundTrack {
        location: TrackLocation::RomSong { engine: format!("{engine}@{key}@{body}@{fade}"), song: u32::from(v) },
        name,
        kind,
        sample_rate: PLAY_RATE,
        channels,
        duration_ms,
        looping,
        loop_start: intro * r / 1000,
        loop_end: (intro + lp) * r / 1000,
        codec: codec.into(),
        format: format!("song {v}"),
        bytes: 0,
        video: false,
    })
}

/// A Game Boy song played from the ROM by its map ([`GB_MAP`]).
fn open_gb_map(path: &Path, engine: &str, song: u32) -> Result<Box<dyn PcmStream>, String> {
    let bad = || format!("a damaged ROM song record: {engine}");
    let parts: Vec<&str> = engine.split('@').collect();
    let [_, key, body, fade] = parts[..] else { return Err(bad()) };
    let body: u64 = body.parse().map_err(|_| bad())?;
    let fade: u64 = fade.parse().map_err(|_| bad())?;
    let m = load_gb_map(key).ok_or("this game's music map is no longer kept; scan its music again")?;
    let rom = read_rom(path)?;
    let v = u8::try_from(song).map_err(|_| bad())?;
    let song = super::gb_map::MapSong::open(&m, &rom, v, PLAY_RATE)?;
    let r = u64::from(PLAY_RATE);
    let tail = if fade > 0 { super::track_timing::FADE_TAIL_MS } else { 0 };
    Ok(Box::new(MapPcm { song: AnyMapSong::Gb(song), channels: 2, left: (body + fade + tail) * r / 1000, fade: fade * r / 1000, tail: tail * r / 1000, pending: Vec::new() }))
}

/// Builds the map of the ROM at `path` from the first of `sets` that holds an NSF (a GBS for
/// a Game Boy ROM), unless one is kept already.
pub fn map_from_sets(path: &Path, sets: &[PathBuf]) -> Result<(), String> {
    let rom = read_rom(path)?;
    if super::nes_nsf::program(&rom).is_none() {
        // A Game Boy ROM carries the Nintendo logo at $104.
        if rom.get(0x104..0x108) == Some(&[0xCE, 0xED, 0x66, 0x66]) {
            return gb_map_from_sets(path, sets);
        }
        if hes_map_from_sets(&rom, sets)? {
            return Ok(());
        }
        return sgc_map_from_sets(&rom, sets);
    }
    if map_key(&rom).is_none() || has_map(&rom) {
        return Ok(());
    }
    for set in sets {
        if let Some(nsf) = super::rip_sets::first_with_magic(set, b"NESM\x1a") {
            if build_nes_map(&rom, &nsf)? {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// The songs a map plays, measured and classified like [`index_machine`]'s.
fn index_map(rom: &[u8], key: &str, m: &super::nes_nsf::Map, t: &Thresholds) -> Result<Vec<FoundTrack>, String> {
    let mut out = Vec::new();
    for v in 0..m.header.songs {
        if matches!(m.ways.get(v as usize), None | Some(super::nes_nsf::Way::None)) {
            continue;
        }
        let Ok(mut song) = super::nes_nsf::MapSong::open(m, rom, v, MEASURE_RATE) else { continue };
        let rate = song.frame_rate();
        let Ok(timing) = music_search::measure_frames(rate, |samples| song.frame(samples).ok_or_else(|| "the song stopped".to_string())) else { continue };
        let ms = |frames: usize| (frames as f64 * 1000.0 / rate) as u64;
        let (looping, intro, lp) = match timing.looping {
            Some((start, len)) => (true, ms(start), ms(len)),
            None => (false, ms(timing.frames), 0),
        };
        let duration_ms = if looping { intro + lp } else { intro };
        if duration_ms == 0 {
            continue;
        }
        let (body, fade) = if looping { (intro + lp * u64::from(PLAY_LOOPS), SONG_FADE_MS) } else { (duration_ms, 0) };
        let name = format!("Track {}", out.len() + 1);
        let shape = Shape { looping, duration_ms, channels: 1, cd_track: 0, rip: true };
        let kind = classify(&shape, &name, t);
        let r = u64::from(PLAY_RATE);
        out.push(FoundTrack {
            location: TrackLocation::RomSong { engine: format!("{NES_MAP}@{key}@{body}@{fade}"), song: u32::from(v) },
            name,
            kind,
            sample_rate: PLAY_RATE,
            channels: 1,
            duration_ms,
            looping,
            loop_start: intro * r / 1000,
            loop_end: (intro + lp) * r / 1000,
            codec: "NES".into(),
            format: format!("song {v}"),
            bytes: 0,
            video: false,
        });
    }
    Ok(out)
}

/// A song played from the ROM by its map ([`NES_MAP`]).
fn open_map(path: &Path, engine: &str, song: u32) -> Result<Box<dyn PcmStream>, String> {
    let bad = || format!("a damaged ROM song record: {engine}");
    let parts: Vec<&str> = engine.split('@').collect();
    let [_, key, body, fade] = parts[..] else { return Err(bad()) };
    let body: u64 = body.parse().map_err(|_| bad())?;
    let fade: u64 = fade.parse().map_err(|_| bad())?;
    let m = load_map(key).ok_or("this game's music map is no longer kept; scan its music again")?;
    let rom = read_rom(path)?;
    let v = u8::try_from(song).map_err(|_| bad())?;
    let song = super::nes_nsf::MapSong::open(&m, &rom, v, PLAY_RATE)?;
    let r = u64::from(PLAY_RATE);
    let tail = if fade > 0 { super::track_timing::FADE_TAIL_MS } else { 0 };
    Ok(Box::new(MapPcm { song: AnyMapSong::Nes(song), channels: 1, left: (body + fade + tail) * r / 1000, fade: fade * r / 1000, tail: tail * r / 1000, pending: Vec::new() }))
}

fn index_machine<M: Machine>(path: &Path, t: &Thresholds, engine: &str, codec: &str) -> Result<(Vec<FoundTrack>, Option<String>), String> {
    let rom = read_rom(path)?;
    let found = music_search::search::<M>(&rom)?;
    if found.songs.is_empty() {
        return Ok((Vec::new(), found.why));
    }
    let rate = M::boot(&rom)?.frame_rate();
    let ms = |frames: usize| (frames as f64 * 1000.0 / rate) as u64;
    let recipe = found.recipe.encode();
    let mut out = Vec::with_capacity(found.songs.len());
    for (i, &v) in found.songs.iter().enumerate() {
        let timing = music_search::measure::<M>(&rom, &found.recipe, v)?;
        let (looping, intro, lp) = match timing.looping {
            Some((start, len)) => (true, ms(start), ms(len)),
            None => (false, ms(timing.frames), 0),
        };
        let duration_ms = if looping { intro + lp } else { intro };
        // Played: the intro and the loop twice, then a fade; a song that ends plays to its end.
        let (body, fade) = if looping { (intro + lp * u64::from(PLAY_LOOPS), SONG_FADE_MS) } else { (duration_ms, 0) };
        let name = format!("Track {}", i + 1);
        let shape = Shape { looping, duration_ms, channels: M::CHANNELS, cd_track: 0, rip: true };
        let kind = classify(&shape, &name, t);
        let r = u64::from(PLAY_RATE);
        out.push(FoundTrack {
            location: TrackLocation::RomSong { engine: format!("{engine}@{recipe}@{body}@{fade}"), song: u32::from(v) },
            name,
            kind,
            sample_rate: PLAY_RATE,
            channels: M::CHANNELS,
            duration_ms,
            looping,
            loop_start: intro * r / 1000,
            loop_end: (intro + lp) * r / 1000,
            codec: codec.into(),
            format: format!("song {v}"),
            bytes: 0,
            video: false,
        });
    }
    Ok((out, None))
}

/// A song from the game's own code, played for `left` more sample frames, the last `fade` of
/// them fading out.
struct MachinePcm<M: Machine> {
    song: music_search::Song<M>,
    left: u64,
    fade: u64,
    tail: u64,
    pending: Vec<i16>,
}

impl<M: Machine + Send> PcmStream for MachinePcm<M>
where
    M::Entry: Send,
    M::Chip: Send,
{
    fn sample_rate(&self) -> u32 {
        PLAY_RATE
    }
    fn channels(&self) -> u16 {
        M::CHANNELS
    }
    fn total_samples(&self) -> Option<u64> {
        Some(self.left)
    }
    fn next_chunk(&mut self) -> Result<Option<Vec<i16>>, String> {
        if self.left == 0 {
            return Ok(None);
        }
        let ch = M::CHANNELS as usize;
        while self.pending.len() < 4096 * ch {
            self.song.frame(&mut self.pending)?;
        }
        let n = (self.left as usize).min(self.pending.len() / ch);
        let mut buf: Vec<i16> = self.pending.drain(..n * ch).collect();
        if self.fade > 0 {
            for (i, frame) in buf.chunks_exact_mut(ch).enumerate() {
                let g = super::track_timing::fade_gain(self.left - i as u64, self.fade, self.tail);
                if g < 1.0 {
                    for s in frame {
                        *s = (f32::from(*s) * g) as i16;
                    }
                }
            }
        }
        self.left -= n as u64;
        Ok(Some(buf))
    }
}

/// A song played from the ROM by its map, like [`MachinePcm`] (mono).
struct MapPcm {
    song: AnyMapSong,
    channels: u16,
    left: u64,
    fade: u64,
    tail: u64,
    pending: Vec<i16>,
}

/// A song from either console's map.
enum AnyMapSong {
    Nes(super::nes_nsf::MapSong),
    Gb(super::gb_map::MapSong),
    Sgc(super::sgc_map::MapSong),
    Hes(super::hes_map::MapSong),
}

impl AnyMapSong {
    fn frame(&mut self, out: &mut Vec<i16>) -> Option<Vec<music_search::Write>> {
        match self {
            AnyMapSong::Nes(s) => s.frame(out),
            AnyMapSong::Gb(s) => s.frame(out),
            AnyMapSong::Sgc(s) => s.frame(out),
            AnyMapSong::Hes(s) => s.frame(out),
        }
    }
}

impl PcmStream for MapPcm {
    fn sample_rate(&self) -> u32 {
        PLAY_RATE
    }
    fn channels(&self) -> u16 {
        self.channels
    }
    fn total_samples(&self) -> Option<u64> {
        Some(self.left)
    }
    fn next_chunk(&mut self) -> Result<Option<Vec<i16>>, String> {
        if self.left == 0 {
            return Ok(None);
        }
        let ch = self.channels as usize;
        while self.pending.len() < 4096 * ch {
            if self.song.frame(&mut self.pending).is_none() {
                // The song's code stopped: what was rendered is the end.
                self.left = self.left.min((self.pending.len() / ch) as u64);
                break;
            }
        }
        let n = (self.left as usize).min(self.pending.len() / ch);
        if n == 0 {
            return Ok(None);
        }
        let mut buf: Vec<i16> = self.pending.drain(..n * ch).collect();
        if self.fade > 0 {
            for (i, frame) in buf.chunks_exact_mut(ch).enumerate() {
                let g = super::track_timing::fade_gain(self.left - i as u64, self.fade, self.tail);
                if g < 1.0 {
                    for s in frame {
                        *s = (f32::from(*s) * g) as i16;
                    }
                }
            }
        }
        self.left -= n as u64;
        Ok(Some(buf))
    }
}

fn open_machine<M: Machine + Send + 'static>(path: &Path, engine: &str, song: u32) -> Result<Box<dyn PcmStream>, String>
where
    M::Entry: Send,
    M::Chip: Send,
{
    let bad = || format!("a damaged ROM song record: {engine}");
    // The engine's name, the recipe's four fields, then body and fade.
    let parts: Vec<&str> = engine.split('@').collect();
    if parts.len() != 7 {
        return Err(bad());
    }
    let recipe = music_search::Recipe::decode(&parts[1..5].join("@")).ok_or_else(bad)?;
    let body: u64 = parts[5].parse().map_err(|_| bad())?;
    let fade: u64 = parts[6].parse().map_err(|_| bad())?;
    let rom = read_rom(path)?;
    let v = u8::try_from(song).map_err(|_| bad())?;
    let player = music_search::Song::<M>::open(&rom, &recipe, v, PLAY_RATE)?;
    let r = u64::from(PLAY_RATE);
    let tail = if fade > 0 { super::track_timing::FADE_TAIL_MS } else { 0 };
    Ok(Box::new(MachinePcm { song: player, left: (body + fade + tail) * r / 1000, fade: fade * r / 1000, tail: tail * r / 1000, pending: Vec::new() }))
}
