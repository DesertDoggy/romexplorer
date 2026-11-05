//! Music sequenced inside a cartridge ROM, found by reading the game's own sound engine and
//! played from the ROM by song number: no rip, no copy.
//!
//! Engines so far: GBA games made with Nintendo's standard engine, through [`super::agbplay`].
//!
//! A track is stored like a rip-set track (`location = 'file'`), with [`ROM_SONG_VIEW`] as its
//! data view, the engine (and where it found the songs) as its path, and the song number as its
//! subsong. Playback opens the engine again from that, without searching the ROM.

use std::path::Path;

use super::agbplay;
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

/// Which engines can read ROMs of `platform` (`platforms.id`) on this machine.
pub fn covers(platform: u16) -> bool {
    use crate::common::models::Platform as P;
    platform == P::GameBoyAdvance as u16 && agbplay::available()
}

/// A ROM's bytes: the file itself, or the one ROM inside a zip / 7z.
fn read_rom(path: &Path) -> Result<Vec<u8>, String> {
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
    if !covers(platform) {
        return Ok(Vec::new());
    }
    let rom = read_rom(path)?;
    let mut player = match agbplay::Player::open(&rom, MEASURE_RATE) {
        Ok(p) => p,
        // No standard engine: the game's music is made another way.
        Err(_) => return Ok(Vec::new()),
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
    Ok(found)
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

/// Opens song `song` of the ROM at `path`, as the engine named by `engine` (a stored path)
/// plays it.
pub fn open(path: &Path, engine: &str, song: u32) -> Result<Box<dyn PcmStream>, String> {
    match engine.split('@').next() {
        Some(GBA_M4A) => {
            let rom = read_rom(path)?;
            let mut player = agbplay::Player::open(&rom, PLAY_RATE)?;
            player.start(song, PLAY_LOOPS)?;
            Ok(Box::new(AgbPcm(player)))
        }
        _ => Err(format!("unknown ROM music engine {engine}")),
    }
}
