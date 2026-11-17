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

/// The largest ROM read whole: N64 cartridges are 64 MB at most.
pub(crate) const MAX_ROM_BYTES: u64 = 64 << 20;
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
/// `wsmap@<map key>@<body ms>@<fade ms>`: the same for the WonderSwan
/// ([`super::ws_map::Map`]).
const WS_MAP: &str = "wsmap";
/// A Game Boy Advance song played from the ROM by its music map ([`super::gba_map`]).
const GBA_MAP: &str = "gbamap";
/// A SNES song played from the ROM by its music map ([`super::snes_map`]).
const SNES_MAP: &str = "snesmap";
/// An N64 song played from the ROM by its music map ([`super::n64_map`]).
const N64_MAP: &str = "n64map";
/// How long each track of a new N64 map is played to find one with sound.
const N64_AUDIBLE_SECONDS: u32 = 10;
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
        || (is_snes(platform) && super::snes_apu::available())
        || (platform == P::Nintendo64 as u16 && super::psf::usf_available())
        || (platform == P::Pc98 as u16 && super::pc98_music::available())
        || (is_gb(platform) && super::gb_apu::available())
        || ((platform == P::SegaMasterSystem as u16 || platform == P::SegaGameGear as u16 || platform == P::SegaGenesis as u16
            || platform == P::TurboGrafx16 as u16
            || platform == P::SuperGrafx as u16
            || platform == P::Sg1000 as u16
            || platform == P::Msx as u16
            || platform == P::Msx2 as u16
            || platform == P::Msx2Plus as u16
            || platform == P::MsxTurbo as u16
            || platform == P::WonderSwan as u16
            || platform == P::WonderSwanColor as u16
            || platform == P::VirtualBoy as u16
            || platform == P::NeoGeoPocket as u16
            || platform == P::NeoGeoPocketColor as u16)
            && super::vgm_chips::available())
}

/// A cartridge console: its .bin or .img is a ROM dump, never a disc image.
pub fn is_cartridge(platform: u16) -> bool {
    use crate::common::models::Platform as P;
    [
        P::Atari2600, P::Atari5200, P::Atari7800, P::AtariLynx, P::AtariJaguar, P::SegaGenesis, P::Sega32x, P::SegaMasterSystem,
        P::SegaGameGear, P::Sg1000, P::Nes, P::Famicom, P::Snes, P::SuperFamicom, P::Nintendo64, P::GameBoy, P::GameBoyColor,
        P::GameBoyAdvance, P::VirtualBoy, P::TurboGrafx16, P::SuperGrafx, P::NeoGeoPocket, P::NeoGeoPocketColor, P::WonderSwan,
        P::WonderSwanColor, P::Intellivision, P::ColecoVision,
    ]
    .iter()
    .any(|p| *p as u16 == platform)
}

fn is_snes(platform: u16) -> bool {
    use crate::common::models::Platform as P;
    platform == P::Snes as u16 || platform == P::SuperFamicom as u16
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
    let rom = read_rom_bytes(path)?;
    // A NES cartridge dumped without its iNES header gets back the header its headered dump
    // carries (fcdb), so it plays, and shares its maps, like that dump.
    if super::nes_nsf::is_headerless(&rom) {
        if let Some(headered) = super::convert::nes::add(&rom) {
            return Ok(headered);
        }
    }
    Ok(rom)
}

fn read_rom_bytes(path: &Path) -> Result<Vec<u8>, String> {
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

/// [`index_explained`], with the other copies of the same game (`others`: another region, the
/// original of a translation) to borrow a NES map from when this copy has none of its own.
pub fn index_with_others(path: &Path, platform: u16, t: &Thresholds, others: &[PathBuf]) -> Result<(Vec<FoundTrack>, Option<String>), String> {
    if is_nes(platform) && !others.is_empty() {
        if let Some(found) = borrowed_nes(path, t, others)? {
            return Ok((found, None));
        }
    }
    index_explained(path, platform, t)
}

/// Whether [`index_with_others`] can use other copies of a game on `platform`.
pub fn borrows_maps(platform: u16) -> bool {
    is_nes(platform)
}

/// Share of a track's sound fingerprint a copy shares with the map's own game for the track
/// to count as the same music there.
const SAME_MUSIC: f64 = 0.98;

/// A NES copy with no map of its own (a translation, a fix patch) plays by the original's
/// map: each of the map's tracks that still sounds on the copy is kept. Of several copies with
/// a map, the one whose tracks agree most with the copy's sound is taken. None only when no
/// other copy has a map or none of its tracks sounds on this one.
fn borrowed_nes(path: &Path, t: &Thresholds, others: &[PathBuf]) -> Result<Option<Vec<FoundTrack>>, String> {
    let rom = read_rom(path)?;
    let Some(own) = map_key(&rom) else { return Ok(None) };
    if kept_map("nes", &own).is_some() {
        return Ok(None);
    }
    // (key, map, tracks that sound here, how many of them agree with the original)
    let mut best: Option<(String, super::nes_nsf::Map, std::collections::HashSet<u8>, usize)> = None;
    for other in others {
        let Ok(theirs) = read_rom(other) else { continue };
        let Some(key) = map_key(&theirs) else { continue };
        if key == own || best.as_ref().is_some_and(|b| b.0 == key) {
            continue;
        }
        let Some(m) = load_map(&key) else { continue };
        let mut sounds = std::collections::HashSet::new();
        let mut agree = 0;
        for v in (0..m.header.songs).filter(|&v| !matches!(m.ways.get(v as usize), None | Some(super::nes_nsf::Way::None))) {
            let Some(here) = map_print(&m, &rom, v).filter(|p| p.len() >= music_search::MIN_MELODY) else { continue };
            sounds.insert(v);
            if map_print(&m, &theirs, v).is_some_and(|there| music_search::alike(&there, &here) >= SAME_MUSIC) {
                agree += 1;
            }
        }
        if !sounds.is_empty() && best.as_ref().map_or(true, |b| (agree, sounds.len()) > (b.3, b.2.len())) {
            best = Some((key, m, sounds, agree));
        }
    }
    let Some((key, m, sounds, _)) = best else { return Ok(None) };
    let found = index_map(&rom, &key, &m, t, Some(&sounds))?;
    Ok((!found.is_empty()).then_some(found))
}

/// A map track's [`music_search::writes_print`] played on `rom`.
fn map_print(m: &super::nes_nsf::Map, rom: &[u8], v: u8) -> Option<std::collections::HashSet<u64>> {
    let mut song = super::nes_nsf::MapSong::open(m, rom, v, MEASURE_RATE).ok()?;
    let mut samples = Vec::new();
    let mut frames = Vec::with_capacity(music_search::PRINT_FRAMES);
    for _ in 0..music_search::PRINT_FRAMES {
        frames.push(song.frame(&mut samples)?);
        samples.clear();
    }
    Some(music_search::writes_print(&frames))
}

/// [`index`], and when nothing was found, why.
pub fn index_explained(path: &Path, platform: u16, t: &Thresholds) -> Result<(Vec<FoundTrack>, Option<String>), String> {
    if !covers(platform) {
        return Ok((Vec::new(), None));
    }
    if is_nes(platform) {
        return index_nes(path, t);
    }
    // PC-98 music is read from the files on its floppies.
    if platform == crate::common::models::Platform::Pc98 as u16 {
        if !super::pc98_music::is_disk_path(path) {
            return Ok((Vec::new(), None));
        }
        return super::pc98_music::index(path, t);
    }
    // An N64 game plays only by a map, like a SNES game.
    if platform == crate::common::models::Platform::Nintendo64 as u16 {
        let rom = read_rom(path)?;
        let Some(native) = super::n64_map::native(&rom) else { return Ok((Vec::new(), None)) };
        let key = gb_key(&native);
        return Ok(match load_n64_map(&key) {
            Some(m) => (index_n64_map(&key, &m, t), None),
            None => (Vec::new(), Some("no music map for this N64 game: its music is found from a matched set in original audio formats".into())),
        });
    }
    // A SNES game plays only by a map: its sound driver runs on a chip the game feeds.
    if is_snes(platform) {
        let rom = read_rom(path)?;
        let image = super::snes_sound::rom_image(&rom);
        let key = gb_key(image);
        return Ok(match load_snes_map(&key) {
            Some(m) => (index_snes_map(&key, &m, t), None),
            None => (Vec::new(), Some("no music map for this SNES game: its music is found from a matched set in original audio formats".into())),
        });
    }
    if is_gb(platform) {
        let rom = read_rom(path)?;
        if let Some(m) = load_gb_map(&gb_key(&rom)) {
            let found = index_gb_map(&rom, &gb_key(&rom), &m, t)?;
            if !found.is_empty() {
                return Ok((found, None));
            }
        }
        return index_machine_with::<super::gb_music::GameBoy>(path, t, GB, "Game Boy", Some(super::gb_gbs::melody));
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
        // The SG-1000 has the Master System's sound chip and Z80 at the same ports.
        if platform == P::SegaMasterSystem as u16 || platform == P::Sg1000 as u16 {
            return index_machine::<super::sms_music::MasterSystem>(path, t, SMS, "Master System");
        }
        if platform == P::SegaGameGear as u16 {
            return index_machine::<super::sms_music::GameGear>(path, t, GG, "Game Gear");
        }
        if platform == P::SegaGenesis as u16 {
            return index_machine::<super::md_music::MegaDrive>(path, t, MD, "Mega Drive");
        }
        // A SuperGrafx game drives the same sound chip as a PC Engine one.
        if platform == P::SuperGrafx as u16 {
            return index_machine::<super::pce_music::PcEngine>(path, t, PCE, "PC Engine");
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
        if platform == P::Msx as u16 || platform == P::Msx2 as u16 || platform == P::Msx2Plus as u16 || platform == P::MsxTurbo as u16 {
            return index_machine::<super::msx_music::Msx>(path, t, MSX, "MSX");
        }
        if platform == P::WonderSwan as u16 || platform == P::WonderSwanColor as u16 {
            let rom = read_rom(path)?;
            let key = gb_key(&rom);
            if let Some(m) = load_ws_map(&key) {
                let found = index_ws_map(&rom, &key, &m, t)?;
                if !found.is_empty() {
                    return Ok((found, None));
                }
            }
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
    let key = gb_key(&rom);
    if let Some(m) = load_gba_map(&key).filter(|_| super::gba_map::available()) {
        let found = index_gba_map(&key, &m, t);
        if !found.is_empty() {
            return Ok((found, None));
        }
    }
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
        Some(super::disc_seq::ENGINE) => super::disc_seq::open(path, engine, song),
        Some(super::disc_seq::PARTS_ENGINE) => super::disc_seq::open_parts(path, engine, song),
        Some(super::akao_image::ENGINE) => super::akao_image::open(path, engine, song),
        Some(super::triace::ENGINE) => super::triace::open(path, engine, song),
        Some(super::pc98_music::ENGINE) => super::pc98_music::open(path, engine, song),
        Some(NES) => open_machine::<super::nes_music::Nes>(path, engine, song),
        Some(NES_MAP) => open_map(path, engine, song),
        Some(GB_MAP) => open_gb_map(path, engine, song),
        Some(SGC_MAP) => open_sgc_map(path, engine, song),
        Some(HES_MAP) => open_hes_map(path, engine, song),
        Some(WS_MAP) => open_ws_map(path, engine, song),
        Some(GBA_MAP) => open_gba_map(path, engine, song),
        Some(SNES_MAP) => open_snes_map(path, engine, song),
        Some(N64_MAP) => open_n64_map(path, engine, song),
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
        let found = index_map(&rom, &key, &m, t, None)?;
        if !found.is_empty() {
            return Ok((found, None));
        }
    }
    index_machine_with::<super::nes_music::Nes>(path, t, NES, "NES", Some(super::nes_nsf::melody))
}

// --- Music maps: where a game's music is in its ROM, as numbers (super::nes_nsf::Map) -----

static MAPS_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Where music maps built here are kept (`<dir>/nes/<key>.nesmap`). Without it, none are kept,
/// and only the bundled ones ([`BUNDLE_FILE`]) are read.
pub fn set_maps_dir(dir: PathBuf) {
    let _ = MAPS_DIR.set(dir);
}

/// The maps that ship with the app and rommusic, under `<app root>/assets/`: one zstd stream
/// per kind ([`bundle_file`]), each read on first use of that kind, so a console's maps cost
/// nothing until one of its ROMs is played. Numbers only, like each map. An older single
/// bundle of every kind ([`BUNDLE_FILE`]) is still read after them.
pub const BUNDLE_FILE: &str = "music_maps.zst";
const BUNDLE_MAGIC: &str = "romexplorer music maps 1";

/// The bundle file of one kind of map: `music_maps_gba.zst`.
pub fn bundle_file(kind: &str) -> String {
    format!("music_maps_{kind}.zst")
}

/// The bundle file of the part of a kind's maps whose keys start with a hex digit from `from`
/// to `to`: `music_maps_snes_0-5.zst`. A kind too big for one file ([`SPLIT_ABOVE`]) ships
/// this way.
pub fn shard_file(kind: &str, from: char, to: char) -> String {
    format!("music_maps_{kind}_{from}-{to}.zst")
}

/// The bundle of a kind's maps added or fixed after its main files were packed, read before
/// them: `music_maps_snes_updates.zst`. Numbered files (`music_maps_snes_2.zst`) do the same
/// for maps added later still ([`bundle_name`]).
pub fn updates_file(kind: &str) -> String {
    format!("music_maps_{kind}_updates.zst")
}

/// Packs the maps of each kind kept under `dir` into its updates bundle ([`updates_file`]),
/// split into parts past [`SPLIT_ABOVE`] like the main one. Returns each file to write: its name, its bytes and how many maps went in.
pub fn pack_updates(dir: &Path) -> Result<Vec<(String, Vec<u8>, usize)>, String> {
    pack_bundles_split(dir, SPLIT_ABOVE, "_updates")
}

/// A kind's bundle larger than this packed is split by the first hex digit of the key into as
/// few files as keep each at most this size: GitHub warns on files over 50 MB and refuses
/// them over 100 MB, and a game opens only its own part. The SNES maps (400 MB of text) pack
/// to 36 MB, so two files.
const SPLIT_ABOVE: usize = 24 << 20;
const HEX_DIGITS: &str = "0123456789abcdef";

type Bundle = std::collections::HashMap<String, String>;

/// The bundle in `<app root>/assets/<file>`, read once; empty when there is none.
fn bundled(file: &str) -> &'static Bundle {
    static BUNDLES: std::sync::Mutex<Option<std::collections::HashMap<String, &'static Bundle>>> = std::sync::Mutex::new(None);
    let mut all = BUNDLES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(b) = all.get_or_insert_with(Default::default).get(file) {
        return b;
    }
    let read = || -> Bundle {
        let Some(root) = super::native_lib::exe_root() else { return Default::default() };
        let path = root.join("assets").join(file);
        let Ok(packed) = std::fs::read(&path) else { return Default::default() };
        read_bundle(&packed).unwrap_or_else(|e| {
            log::warn!("{}: {e}", path.display());
            Default::default()
        })
    };
    let b: &'static Bundle = Box::leak(Box::new(read()));
    all.get_or_insert_with(Default::default).insert(file.to_string(), b);
    b
}

/// A bundle's maps by `<kind>/<key>` (`nes/<sha1>`).
pub fn read_bundle(packed: &[u8]) -> Result<std::collections::HashMap<String, String>, String> {
    let text = zstd::decode_all(packed).map_err(|e| e.to_string())?;
    let text = String::from_utf8(text).map_err(|_| "not text")?;
    let (head, mut rest) = text.split_once('\n').ok_or("empty")?;
    if head != BUNDLE_MAGIC {
        return Err("not a music map bundle".into());
    }
    let mut maps = std::collections::HashMap::new();
    while !rest.is_empty() {
        let (line, after) = rest.split_once('\n').ok_or("cut short")?;
        let (name, len) = line.rsplit_once(' ').ok_or("bad entry")?;
        let len: usize = len.parse().map_err(|_| "bad entry")?;
        let body = after.get(..len).ok_or("cut short")?;
        maps.insert(name.to_string(), body.to_string());
        rest = &after[len..];
    }
    Ok(maps)
}

/// Adds the track times kept under `times_dir` (`<kind>/<key>.times`, as [`time_map`] keeps
/// them) to the bundle `packed`, beside each map it holds; times of maps it does not hold are
/// left out. Returns the bundle repacked, how many maps it holds and how many of them now
/// have times.
/// `None` for the bundle when it would not change (no times for any of its maps, or the same
/// ones it holds already), so it is not rewritten.
pub fn add_times_to_bundle(packed: &[u8], times_dir: &Path) -> Result<(Option<Vec<u8>>, usize, usize), String> {
    let mut entries: std::collections::BTreeMap<String, String> = read_bundle(packed)?.into_iter().collect();
    let maps: Vec<String> = entries.keys().filter(|k| !k.ends_with(TIMES_SUFFIX)).cloned().collect();
    let mut changed = false;
    for name in &maps {
        let file = times_dir.join(format!("{name}.{TIMES_EXT}"));
        if let Ok(text) = std::fs::read_to_string(&file) {
            changed |= entries.insert(format!("{name}{TIMES_SUFFIX}"), text.clone()).as_ref() != Some(&text);
        }
    }
    let timed = maps.iter().filter(|m| entries.contains_key(&format!("{m}{TIMES_SUFFIX}"))).count();
    if !changed {
        return Ok((None, maps.len(), timed));
    }
    let mut text = format!("{BUNDLE_MAGIC}\n");
    for (name, body) in &entries {
        text.push_str(&format!("{name} {}\n", body.len()));
        text.push_str(body);
    }
    Ok((Some(pack_text(&text)?), maps.len(), timed))
}

/// Packs the maps of each kind kept under `dir` (as [`set_maps_dir`] lays them out) into its
/// own bundle ([`bundle_file`]), or, past [`SPLIT_ABOVE`], into parts ([`shard_file`]). Returns
/// each file to write: its name, its bytes and how many maps went in.
pub fn pack_bundles(dir: &Path) -> Result<Vec<(String, Vec<u8>, usize)>, String> {
    pack_bundles_split(dir, SPLIT_ABOVE, "")
}

/// `suffix` names the bundle: "" for the main one, "_updates" for [`updates_file`].
fn pack_bundles_split(dir: &Path, split_above: usize, suffix: &str) -> Result<Vec<(String, Vec<u8>, usize)>, String> {
    let mut out = Vec::new();
    for (kind, _) in MAP_KINDS {
        let base = format!("{kind}{suffix}");
        let (packed, counts) = pack_maps(dir, &[kind], None)?;
        let n = counts.iter().map(|(_, n)| n).sum::<usize>();
        if n == 0 {
            continue;
        }
        if packed.len() <= split_above {
            out.push((bundle_file(&base), packed, n));
            continue;
        }
        // Each digit's share packed alone, then neighbouring digits grouped while their
        // sizes add up to no more than the limit (packed together they come out a little
        // smaller), and each group packed once.
        let digits: Vec<char> = HEX_DIGITS.chars().collect();
        let mut sizes = Vec::new();
        for &d in &digits {
            let (packed, counts) = pack_maps(dir, &[kind], Some((d, d)))?;
            // A digit with no maps adds nothing to a group.
            sizes.push(if counts.iter().all(|(_, n)| *n == 0) { 0 } else { packed.len() });
        }
        let mut start = 0;
        while start < digits.len() {
            let mut end = start;
            let mut total = sizes[start];
            while end + 1 < digits.len() && total + sizes[end + 1] <= split_above {
                end += 1;
                total += sizes[end];
            }
            let (packed, counts) = pack_maps(dir, &[kind], Some((digits[start], digits[end])))?;
            let n = counts.iter().map(|(_, n)| n).sum::<usize>();
            if n > 0 {
                out.push((shard_file(&base, digits[start], digits[end]), packed, n));
            }
            start = end + 1;
        }
    }
    Ok(out)
}

/// Packs every map of `kinds` kept under `dir` into one bundle. Returns how many maps of each
/// kind went in.
pub fn pack_bundle(dir: &Path, kinds: &[&str]) -> Result<(Vec<u8>, Vec<(String, usize)>), String> {
    pack_maps(dir, kinds, None)
}

/// [`pack_bundle`], of only the maps whose key starts with a digit in `shard` when given.
fn pack_maps(dir: &Path, kinds: &[&str], shard: Option<(char, char)>) -> Result<(Vec<u8>, Vec<(String, usize)>), String> {
    let mut text = format!("{BUNDLE_MAGIC}\n");
    let mut counts = Vec::new();
    for (kind, ext) in MAP_KINDS.iter().filter(|(k, _)| kinds.contains(k)) {
        let Ok(entries) = std::fs::read_dir(dir.join(kind)) else { continue };
        let in_shard = |p: &Path| shard.is_none_or(|(a, b)| p.file_stem().and_then(|s| s.to_str()).and_then(|s| s.chars().next()).is_some_and(|c| (a..=b).contains(&c.to_ascii_lowercase())));
        // A map's track times ([`TIMES_EXT`]) go in beside it, under `<kind>/<key>.times`.
        let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == *ext || x == TIMES_EXT) && in_shard(p)).collect();
        // Sorted, so the same maps always pack to the same bytes.
        files.sort();
        let mut maps = 0;
        for f in &files {
            let body = std::fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display()))?;
            let key = f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let times = f.extension().is_some_and(|x| x == TIMES_EXT);
            maps += usize::from(!times);
            text.push_str(&format!("{kind}/{key}{} {}\n", if times { TIMES_SUFFIX } else { "" }, body.len()));
            text.push_str(&body);
        }
        counts.push((kind.to_string(), maps));
        if maps == 0 && !files.is_empty() {
            // Times alone (maps that ship in another bundle) still make a bundle.
            counts.push((format!("{kind} times"), files.len()));
        }
    }
    Ok((pack_text(&text)?, counts))
}

/// Writes a bundle's maps and times out under `dir` as [`set_maps_dir`] lays them out
/// (`<kind>/<key>.<ext>`, `<kind>/<key>.times`), for [`pack_bundles`] to pack again (another
/// split, say). Returns how many files it wrote.
pub fn unpack_bundle_to(packed: &[u8], dir: &Path) -> Result<usize, String> {
    let mut n = 0;
    for (name, body) in read_bundle(packed)? {
        let (kind, key) = name.split_once('/').ok_or_else(|| format!("bad bundle entry {name}"))?;
        let file = match key.strip_suffix(TIMES_SUFFIX) {
            Some(key) => format!("{key}.{TIMES_EXT}"),
            None => format!("{key}.{}", MAP_KINDS.iter().find(|(k, _)| *k == kind).ok_or_else(|| format!("unknown map kind {kind}"))?.1),
        };
        let path = dir.join(kind).join(file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&path, body).map_err(|e| format!("{}: {e}", path.display()))?;
        n += 1;
    }
    Ok(n)
}

/// A bundle packed again the current way ([`pack_text`]), entries in name order as packing
/// puts them. Its maps and times are unchanged.
pub fn repack_bundle(packed: &[u8]) -> Result<Vec<u8>, String> {
    let entries: std::collections::BTreeMap<String, String> = read_bundle(packed)?.into_iter().collect();
    let mut text = format!("{BUNDLE_MAGIC}\n");
    for (name, body) in &entries {
        text.push_str(&format!("{name} {}\n", body.len()));
        text.push_str(body);
    }
    pack_text(&text)
}

/// Window of [`pack_text`]: 128 MB, the most a zstd reader takes without being told to.
const BUNDLE_WINDOW_LOG: u32 = 27;

/// A bundle's text, packed: zstd -22 over a 128 MB window, with long-distance matching. Packed
/// once per release and read once per run, so size is what counts; a bundle holds many
/// near-copies of one map (regions, revisions), further apart than a small window reaches.
/// Measured on the shipped bundles (packed size, packing time; reading back 0.06-0.2 s at
/// every level):
///
/// | bundle (text) | -19 | -19 long | -22 |
/// |---|---|---|---|
/// | GBA (153 MB) | 16.6 MB, 55 s | 11.4 MB, 48 s | 10.9 MB, 87 s |
/// | N64 (202 MB) | 15.0 MB, 45 s | 9.1 MB, 31 s | 8.9 MB, 50 s |
/// | SNES e-f (52 MB) | 6.05 MB, 20 s | 5.61 MB, 21 s | 5.59 MB, 32 s |
/// | NES/GB/PCE (105 MB) | 2.65 MB, 12 s | 2.23 MB, 12 s | 2.16 MB, 17 s |
///
/// The same maps as binary were 1% bigger than as text once packed: the hex runs pack to a
/// byte a byte already.
fn pack_text(text: &str) -> Result<Vec<u8>, String> {
    use std::io::Write;
    let err = |e: std::io::Error| e.to_string();
    let mut e = zstd::stream::Encoder::new(Vec::new(), 22).map_err(err)?;
    e.long_distance_matching(true).map_err(err)?;
    e.window_log(BUNDLE_WINDOW_LOG).map_err(err)?;
    e.include_contentsize(true).map_err(err)?;
    e.set_pledged_src_size(Some(text.len() as u64)).map_err(err)?;
    e.write_all(text.as_bytes()).map_err(err)?;
    e.finish().map_err(err)
}

// --- Track times kept with a map ---------------------------------------------------------
//
// A map says where a game's songs are, not how long they run: that is found by playing each
// song until it falls silent or repeats ([`music_search::measure_frames`]), which is most of
// the time an index of a mapped game takes. Measured once, the times are kept beside the map
// (`<maps dir>/<kind>/<key>.times`, and in the bundles as `<kind>/<key>.times`), so the next
// index of that game, here or on any machine the bundle ships to, plays nothing.

/// The file extension of a map's track times in the maps dir.
const TIMES_EXT: &str = "times";
/// The bundle entry of a map's track times: `<kind>/<key>.times`.
const TIMES_SUFFIX: &str = ".times";
/// First word of a times file, and its version: raised when measuring changes, so times
/// measured the old way are measured again.
const TIMES_MAGIC: &str = "maptimes 1";

/// One song's measured time: the rate its frames run at, and its [`music_search::Timing`].
/// `None`: the song does not play (it could not be opened, or broke off while measured).
type SongTime = Option<(f64, music_search::Timing)>;

/// A short fingerprint of a map, so times kept for another version of it are not used.
fn map_fingerprint(encoded: &str) -> String {
    use sha1::{Digest, Sha1};
    Sha1::digest(encoded.as_bytes()).iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// The text of a times file: the magic and the map's fingerprint, then one line per song,
/// `<song> <frame rate> <frames> <loop start> <loop length>` (`-` for no loop), or
/// `<song> x` for a song that does not play.
fn encode_times(print: &str, times: &std::collections::BTreeMap<u8, SongTime>) -> String {
    let mut text = format!("{TIMES_MAGIC} {print}\n");
    for (v, t) in times {
        match t {
            None => text.push_str(&format!("{v} x\n")),
            Some((rate, t)) => {
                let (start, len) = t.looping.map_or(("-".to_string(), "-".to_string()), |(a, b)| (a.to_string(), b.to_string()));
                text.push_str(&format!("{v} {rate} {} {start} {len}\n", t.frames));
            }
        }
    }
    text
}

/// A times file's songs, when it was measured the current way for the map `print` names.
fn decode_times(text: &str, print: &str) -> Option<std::collections::BTreeMap<u8, SongTime>> {
    let mut lines = text.lines();
    if lines.next()? != format!("{TIMES_MAGIC} {print}") {
        return None;
    }
    let mut out = std::collections::BTreeMap::new();
    for line in lines.filter(|l| !l.trim().is_empty()) {
        let f: Vec<&str> = line.split_whitespace().collect();
        let v: u8 = f.first()?.parse().ok()?;
        let time = match f[1..] {
            ["x"] => None,
            [rate, frames, start, len] => {
                let looping = match (start, len) {
                    ("-", "-") => None,
                    (a, b) => Some((a.parse().ok()?, b.parse().ok()?)),
                };
                Some((rate.parse().ok()?, music_search::Timing { frames: frames.parse().ok()?, looping }))
            }
            _ => return None,
        };
        out.insert(v, time);
    }
    Some(out)
}

/// The times of `songs` of a map (`kind`, `key`, its text `encoded`): those kept with it, and
/// the rest measured now by `measure` and kept for next time (in the maps dir, when there is
/// one). With `keep` false (a map played on another copy of its game) nothing kept is read
/// or written. Returns each song that plays, in the order given.
fn map_times(kind: &str, key: &str, encoded: &str, songs: &[u8], keep: bool, mut measure: impl FnMut(u8) -> SongTime) -> Vec<(u8, f64, music_search::Timing)> {
    let print = map_fingerprint(encoded);
    let mut kept = if keep { kept_text(kind, key, TIMES_EXT, TIMES_SUFFIX).and_then(|t| decode_times(&t, &print)).unwrap_or_default() } else { Default::default() };
    // [`index_known`]: nothing is measured; a song with no kept time leaves the whole map to
    // a full index.
    if KNOWN_ONLY.with(|k| k.get()) && songs.iter().any(|v| !kept.contains_key(v)) {
        KNOWN_ONLY_MISSED.with(|m| m.set(true));
        return Vec::new();
    }
    let mut measured = false;
    let mut out = Vec::with_capacity(songs.len());
    for &v in songs {
        let time = match kept.get(&v) {
            Some(t) => *t,
            None => {
                let t = measure(v);
                kept.insert(v, t);
                measured = true;
                t
            }
        };
        if let Some((rate, t)) = time {
            out.push((v, rate, t));
        }
    }
    if keep && measured {
        if let Some(file) = MAPS_DIR.get().map(|d| d.join(kind).join(format!("{key}.{TIMES_EXT}"))) {
            let written = file.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&file, encode_times(&print, &kept)));
            if let Err(e) = written {
                log::warn!("music map times {}: {e}", file.display());
            }
        }
    }
    out
}

thread_local! {
    /// Set while [`index_known`] runs: [`map_times`] measures nothing.
    static KNOWN_ONLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Set by [`map_times`] when a song it was asked for has no kept time.
    static KNOWN_ONLY_MISSED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The songs of a cartridge whose music map is kept (shipped or built here) and needs nothing
/// measured: every song's time is kept with the map, or the map carries its own lengths (SNES,
/// N64, GBA). Takes as long as reading the ROM and hashing it, so a title page can list the
/// game's music at once instead of waiting for an index run.
/// `None` when the ROM has no such map, or a song of it would need measuring; the index run
/// then reads it the usual way.
pub fn index_known(path: &Path, platform: u16, t: &Thresholds) -> Result<Option<Vec<FoundTrack>>, String> {
    if !covers(platform) || !is_cartridge(platform) {
        return Ok(None);
    }
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            KNOWN_ONLY.with(|k| k.set(false));
        }
    }
    let rom = read_rom(path)?;
    KNOWN_ONLY.with(|k| k.set(true));
    KNOWN_ONLY_MISSED.with(|m| m.set(false));
    let _reset = Reset;
    let found = known_songs(&rom, platform, t)?;
    if KNOWN_ONLY_MISSED.with(|m| m.get()) {
        return Ok(None);
    }
    Ok(found.filter(|f| !f.is_empty()))
}

/// The songs of a cartridge whose music map is kept, by that map alone: times not kept with
/// it are measured (and kept), but the game's own sound code is never searched. `None` when
/// the ROM has no map or its map plays nothing; an index run then reads another copy of the
/// game first, and searches this one only when no copy has a map.
pub fn index_mapped(path: &Path, platform: u16, t: &Thresholds) -> Result<Option<Vec<FoundTrack>>, String> {
    if !covers(platform) || !is_cartridge(platform) {
        return Ok(None);
    }
    let rom = read_rom(path)?;
    Ok(known_songs(&rom, platform, t)?.filter(|f| !f.is_empty()))
}

/// [`index_known`]'s map lookup, per console: the same keys [`index_explained`] uses.
fn known_songs(rom: &[u8], platform: u16, t: &Thresholds) -> Result<Option<Vec<FoundTrack>>, String> {
    use crate::common::models::Platform as P;
    if is_nes(platform) {
        let Some(key) = map_key(rom) else { return Ok(None) };
        let Some(m) = load_map(&key) else { return Ok(None) };
        return index_map(rom, &key, &m, t, None).map(Some);
    }
    if is_snes(platform) {
        let key = gb_key(super::snes_sound::rom_image(rom));
        return Ok(load_snes_map(&key).map(|m| index_snes_map(&key, &m, t)));
    }
    if platform == P::Nintendo64 as u16 {
        let Some(native) = super::n64_map::native(rom) else { return Ok(None) };
        let key = gb_key(&native);
        return Ok(load_n64_map(&key).map(|m| index_n64_map(&key, &m, t)));
    }
    if is_gb(platform) {
        let key = gb_key(rom);
        let Some(m) = load_gb_map(&key) else { return Ok(None) };
        return index_gb_map(rom, &key, &m, t).map(Some);
    }
    if platform == P::SegaMasterSystem as u16 || platform == P::SegaGameGear as u16 {
        let key = gb_key(rom);
        let Some(m) = load_sgc_map(&key) else { return Ok(None) };
        return index_sgc_map(rom, &key, &m, t).map(Some);
    }
    if platform == P::TurboGrafx16 as u16 {
        let key = gb_key(super::hes_map::program(rom));
        let Some(m) = load_hes_map(&key) else { return Ok(None) };
        return index_hes_map(rom, &key, &m, t).map(Some);
    }
    if platform == P::WonderSwan as u16 || platform == P::WonderSwanColor as u16 {
        let key = gb_key(rom);
        let Some(m) = load_ws_map(&key) else { return Ok(None) };
        return index_ws_map(rom, &key, &m, t).map(Some);
    }
    if platform == P::GameBoyAdvance as u16 && super::gba_map::available() {
        let key = gb_key(rom);
        return Ok(load_gba_map(&key).map(|m| index_gba_map(&key, &m, t)));
    }
    Ok(None)
}

/// Measures and keeps the track times of the map the ROM at `path` plays by, where its kind
/// of map is timed when indexed (NES, Game Boy, PC Engine, Master System / Game Gear,
/// WonderSwan). Returns how many of its songs play, or `None` when no map is kept for it.
/// For the tool that packs the bundles, so they ship with times.
pub fn time_map(path: &Path, platform: u16) -> Result<Option<usize>, String> {
    use crate::common::models::Platform as P;
    let t = Thresholds::default();
    let rom = read_rom(path)?;
    let found = if is_nes(platform) {
        let Some((key, m)) = map_key(&rom).and_then(|k| Some((k.clone(), load_map(&k)?))) else { return Ok(None) };
        index_map(&rom, &key, &m, &t, None)?
    } else if is_gb(platform) {
        let key = gb_key(&rom);
        let Some(m) = load_gb_map(&key) else { return Ok(None) };
        index_gb_map(&rom, &key, &m, &t)?
    } else if platform == P::SegaMasterSystem as u16 || platform == P::SegaGameGear as u16 {
        let key = gb_key(&rom);
        let Some(m) = load_sgc_map(&key) else { return Ok(None) };
        index_sgc_map(&rom, &key, &m, &t)?
    } else if platform == P::TurboGrafx16 as u16 {
        let key = gb_key(super::hes_map::program(&rom));
        let Some(m) = load_hes_map(&key) else { return Ok(None) };
        index_hes_map(&rom, &key, &m, &t)?
    } else if platform == P::WonderSwan as u16 || platform == P::WonderSwanColor as u16 {
        let key = gb_key(&rom);
        let Some(m) = load_ws_map(&key) else { return Ok(None) };
        index_ws_map(&rom, &key, &m, &t)?
    } else {
        return Ok(None);
    };
    Ok(Some(found.len()))
}

/// Each kind of map: its folder under the maps dir and its file extension.
const MAP_KINDS: &[(&str, &str)] = &[("nes", "nesmap"), ("gb", "gbmap"), ("hes", "hesmap"), ("sgc", "sgcmap"), ("ws", "wsmap"), ("gba", "gbamap"), ("snes", "snesmap"), ("n64", "n64map")];

/// When the bundled music maps last changed (the newest `music_maps*.zst` in the app's
/// assets), as seconds since 1970: a ROM indexed with no music before then may play by a map
/// now. None when no bundle is shipped.
pub fn maps_changed_at() -> Option<i64> {
    let root = super::native_lib::exe_root()?;
    asset_files()
        .iter()
        .filter(|f| f.starts_with("music_maps") && f.ends_with(".zst"))
        .filter_map(|f| std::fs::metadata(root.join("assets").join(f)).ok()?.modified().ok())
        .max()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
}

/// The file names in `<app root>/assets/`, listed once.
fn asset_files() -> &'static [String] {
    static FILES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    FILES.get_or_init(|| {
        let Some(root) = super::native_lib::exe_root() else { return Vec::new() };
        let Ok(entries) = std::fs::read_dir(root.join("assets")) else { return Vec::new() };
        entries.flatten().filter_map(|e| e.file_name().into_string().ok()).collect()
    })
}

/// A kept map's text: one built here first, then the bundled one.
fn kept_map(kind: &str, key: &str) -> Option<String> {
    let ext = MAP_KINDS.iter().find(|(k, _)| *k == kind)?.1;
    kept_text(kind, key, ext, "")
}

/// A kept file of a map's game: `<key>.<ext>` in the maps dir first, then the bundle entry
/// `<kind>/<key><suffix>` ("" for the map itself, [`TIMES_SUFFIX`] for its track times).
fn kept_text(kind: &str, key: &str, ext: &str, suffix: &str) -> Option<String> {
    if let Some(dir) = MAPS_DIR.get() {
        if let Ok(text) = std::fs::read_to_string(dir.join(kind).join(format!("{key}.{ext}"))) {
            return Some(text);
        }
    }
    let name = format!("{kind}/{key}{suffix}");
    let digit = key.chars().next().map(|c| c.to_ascii_lowercase())?;
    // Every file of the kind that can hold the key, newest first: the first that has it wins.
    // Only those files are opened, and each once per run.
    let mut files: Vec<(BundleName, &String)> = asset_files().iter().filter_map(|f| Some((bundle_name(kind, f)?, f))).collect();
    if files.is_empty() {
        // The old all-kinds bundle (105 MB as text, held for the run) only stands in for a
        // kind with no file of its own: a miss in a kind's own files is a miss.
        return bundled(BUNDLE_FILE).get(&name).cloned();
    }
    files.retain(|(b, _)| b.range.is_none_or(|(a, z)| (a..=z).contains(&digit)));
    files.sort_by(|a, b| b.0.order.cmp(&a.0.order).then_with(|| a.1.cmp(b.1)));
    files.iter().find_map(|(_, f)| bundled(f).get(&name).cloned())
}

/// Where a bundle file of any kind comes in its kind's reading order ([`bundle_name`]): files
/// read later (lower) first, so a tool that merges them lets the later ones win. `None` for a
/// name that is no kind's bundle.
pub fn bundle_order(file: &str) -> Option<u32> {
    MAP_KINDS.iter().find_map(|(kind, _)| bundle_name(kind, file)).map(|b| b.order)
}

/// What a bundle file's name says about it, for one kind ([`bundle_name`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BundleName {
    /// The first hex digits of the keys it holds; `None` for every key.
    range: Option<(char, char)>,
    /// Which is read first when two can hold a key: the higher. The kind's main files are 0,
    /// files added later 1, 2, … by their number, and the updates files above them all.
    order: u32,
}

/// Parses a bundle file name of `kind`. Every form is `music_maps_<kind>[_<parts>].zst`:
///
/// - `music_maps_snes.zst`: every key ([`bundle_file`]).
/// - `music_maps_snes_0-7.zst`: keys starting with 0 to 7 ([`shard_file`]).
/// - `music_maps_snes_2.zst`, `music_maps_snes_0-7_2.zst`: added later, numbered from 1; a
///   higher number is read before a lower one and before the main files, so maps for new
///   games, or fixed ones, ship as a new file without repacking the others.
/// - `music_maps_snes_updates.zst`, `music_maps_snes_updates_0-7.zst`: read first of all
///   ([`updates_file`]).
fn bundle_name(kind: &str, file: &str) -> Option<BundleName> {
    let rest = file.strip_prefix("music_maps_")?.strip_suffix(".zst")?.strip_prefix(kind)?;
    let mut range = None;
    let mut order = 0;
    if rest.is_empty() {
        return Some(BundleName { range, order });
    }
    let rest = rest.strip_prefix('_')?;
    let mut parts = rest.split('_').peekable();
    if parts.peek() == Some(&"updates") {
        parts.next();
        order = u32::MAX;
    }
    let digit = |s: &str| s.chars().next().filter(|c| s.len() == 1 && c.is_ascii_hexdigit()).map(|c| c.to_ascii_lowercase());
    for part in parts {
        if let Some((a, b)) = part.split_once('-') {
            if range.is_some() {
                return None;
            }
            range = Some((digit(a)?, digit(b)?));
        } else if order == 0 {
            order = part.parse::<u32>().ok().filter(|n| *n >= 1 && *n < u32::MAX)?;
        } else {
            return None;
        }
    }
    Some(BundleName { range, order })
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
    super::nes_nsf::Map::decode(&kept_map("nes", key)?)
}

/// Whether a map is kept for the NES ROM `rom`.
pub fn has_map(rom: &[u8]) -> bool {
    map_key(rom).is_some_and(|k| kept_map("nes", &k).is_some())
}

/// Builds the map of the NES ROM `rom` from the NSF it matches (only to find and check where
/// its music is; nothing of the file is kept) and keeps it. False when none plays any track.
pub fn build_nes_map(rom: &[u8], nsf: &[u8]) -> Result<bool, String> {
    let key = map_key(rom).ok_or("not a NES cartridge or Disk System image")?;
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
        || ((platform == P::SegaMasterSystem as u16
            || platform == P::SegaGameGear as u16
            || platform == P::TurboGrafx16 as u16
            || platform == P::WonderSwan as u16
            || platform == P::WonderSwanColor as u16)
            && super::vgm_chips::available())
        || (platform == P::GameBoyAdvance as u16 && super::gba_map::available())
        || (is_snes(platform) && super::snes_apu::available())
        || (platform == P::Nintendo64 as u16 && super::psf::usf_available())
}

fn sgc_map_path(key: &str) -> Option<PathBuf> {
    Some(MAPS_DIR.get()?.join("sgc").join(format!("{key}.sgcmap")))
}

fn load_sgc_map(key: &str) -> Option<super::sgc_map::Map> {
    super::sgc_map::Map::decode(&kept_map("sgc", key)?)
}

/// The map of the Master System / Game Gear ROM `rom` from the first of `sets` holding an
/// SGC, unless one is kept already.
fn sgc_map_from_sets(rom: &[u8], sets: &[PathBuf]) -> Result<(), String> {
    let key = gb_key(rom);
    let file = sgc_map_path(&key).ok_or("no folder for music maps")?;
    if kept_map("sgc", &key).is_some() {
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
    let songs: Vec<u8> = (0..m.header.songs).filter(|&v| !matches!(m.ways.get(v as usize), None | Some(super::sgc_map::Way::None))).collect();
    let timed = map_times("sgc", key, &m.encode(), &songs, true, |v| {
        let mut song = super::sgc_map::MapSong::open(m, rom, v, MEASURE_RATE).ok()?;
        let rate = song.frame_rate();
        Some((rate, music_search::measure_frames_by_writes(rate, music_search::every_write, |samples| song.frame(samples).ok_or_else(|| "the song stopped".to_string())).ok()?))
    });
    for (v, rate, timing) in timed {
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
    super::hes_map::Map::decode(&kept_map("hes", key)?)
}

/// The map of the PC Engine ROM `rom` from the first of `sets` holding a HES, unless one is
/// kept already. False when no set holds one.
fn hes_map_from_sets(rom: &[u8], sets: &[PathBuf]) -> Result<bool, String> {
    let key = gb_key(super::hes_map::program(rom));
    let file = hes_map_path(&key).ok_or("no folder for music maps")?;
    let mut any = false;
    for set in sets {
        let Some(hes) = super::rip_sets::first_with_magic(set, b"HESM") else { continue };
        any = true;
        if kept_map("hes", &key).is_some() {
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
    let songs: Vec<u8> = (0..m.header.songs).filter(|&v| !matches!(m.ways.get(v as usize), None | Some(super::hes_map::Way::None))).collect();
    let timed = map_times("hes", key, &m.encode(), &songs, true, |v| {
        let mut song = super::hes_map::MapSong::open(m, rom, v, MEASURE_RATE).ok()?;
        let rate = song.frame_rate();
        Some((rate, music_search::measure_frames_by_writes(rate, music_search::every_write, |samples| song.frame(samples).ok_or_else(|| "the song stopped".to_string())).ok()?))
    });
    for (v, rate, timing) in timed {
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

fn ws_map_path(key: &str) -> Option<PathBuf> {
    Some(MAPS_DIR.get()?.join("ws").join(format!("{key}.wsmap")))
}

fn load_ws_map(key: &str) -> Option<super::ws_map::Map> {
    super::ws_map::Map::decode(&kept_map("ws", key)?)
}

/// The map of the WonderSwan ROM `rom` from the first of `sets` holding a WSR (its playlist's
/// songs, or 64 from its first), unless one is kept already. False when no set holds one.
fn ws_map_from_sets(rom: &[u8], sets: &[PathBuf]) -> Result<bool, String> {
    let key = gb_key(rom);
    let file = ws_map_path(&key).ok_or("no folder for music maps")?;
    let mut any = false;
    for set in sets {
        for (wsr, listed) in super::rip_sets::wsrs_with_songs(set) {
            any = true;
            if kept_map("ws", &key).is_some() {
                return Ok(true);
            }
            let songs: Vec<u8> = if listed.is_empty() {
                let first = super::ws_map::first_song(&wsr);
                (first..first.saturating_add(64)).collect()
            } else {
                listed
            };
            let Some(m) = super::ws_map::build_map(&wsr, rom, songs.into_iter()) else { continue };
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            std::fs::write(&file, m.encode()).map_err(|e| format!("{}: {e}", file.display()))?;
            return Ok(true);
        }
    }
    Ok(any)
}

fn index_ws_map(rom: &[u8], key: &str, m: &super::ws_map::Map, t: &Thresholds) -> Result<Vec<FoundTrack>, String> {
    let mut out = Vec::new();
    let songs: Vec<u8> = m.songs.iter().map(|&(v, _)| v).collect();
    let timed = map_times("ws", key, &m.encode(), &songs, true, |v| {
        let mut song = super::ws_map::MapSong::open(m, rom, v, MEASURE_RATE).ok()?;
        let rate = song.frame_rate();
        Some((rate, music_search::measure_frames_by_writes(rate, music_search::every_write, |samples| song.frame(samples).ok_or_else(|| "the song stopped".to_string())).ok()?))
    });
    for (v, rate, timing) in timed {
        if let Some(track) = map_track(WS_MAP, key, v, out.len(), rate, timing, 2, "WonderSwan", t) {
            out.push(track);
        }
    }
    Ok(out)
}

fn open_ws_map(path: &Path, engine: &str, song: u32) -> Result<Box<dyn PcmStream>, String> {
    let bad = || format!("a damaged ROM song record: {engine}");
    let parts: Vec<&str> = engine.split('@').collect();
    let [_, key, body, fade] = parts[..] else { return Err(bad()) };
    let body: u64 = body.parse().map_err(|_| bad())?;
    let fade: u64 = fade.parse().map_err(|_| bad())?;
    let m = load_ws_map(key).ok_or("this game's music map is no longer kept; scan its music again")?;
    let rom = read_rom(path)?;
    let v = u8::try_from(song).map_err(|_| bad())?;
    let song = super::ws_map::MapSong::open(&m, &rom, v, PLAY_RATE)?;
    let r = u64::from(PLAY_RATE);
    let tail = if fade > 0 { super::track_timing::FADE_TAIL_MS } else { 0 };
    Ok(Box::new(MapPcm { song: AnyMapSong::Ws(song), channels: 2, left: (body + fade + tail) * r / 1000, fade: fade * r / 1000, tail: tail * r / 1000, pending: Vec::new() }))
}

/// Whether a SNES music map is kept for the ROM `rom` (a copier header or none).
pub fn has_snes_map(rom: &[u8]) -> bool {
    kept_map("snes", &gb_key(super::snes_sound::rom_image(rom))).is_some()
}

fn load_snes_map(key: &str) -> Option<super::snes_map::Map> {
    super::snes_map::Map::decode(&kept_map("snes", key)?)
}

/// The map of the SNES ROM `rom` from the first of `sets` whose `.spc` tracks are this ROM's,
/// unless one is kept already. False when no set holds an `.spc`.
fn snes_map_from_sets(rom: &[u8], sets: &[PathBuf]) -> Result<bool, String> {
    let image = super::snes_sound::rom_image(rom);
    let key = gb_key(image);
    // A kept map first, before every SPC of the set is read (see the N64 one).
    if kept_map("snes", &key).is_some() {
        return Ok(true);
    }
    let mut any = false;
    for set in sets {
        let spcs = super::rip_sets::all_with_magic(set, b"SNES-SPC700");
        if spcs.is_empty() {
            continue;
        }
        any = true;
        let refs: Vec<&[u8]> = spcs.iter().map(Vec::as_slice).collect();
        let Some(mut m) = super::snes_map::Map::build(&refs, image) else { continue };
        super::snes_map::slim(&mut m, image)?;
        let file = MAPS_DIR.get().ok_or("no folder for music maps")?.join("snes").join(format!("{key}.snesmap"));
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&file, m.encode()).map_err(|e| format!("{}: {e}", file.display()))?;
        return Ok(true);
    }
    Ok(any)
}

/// A SNES map's songs, timed by the set's own tags.
fn index_snes_map(key: &str, m: &super::snes_map::Map, t: &Thresholds) -> Vec<FoundTrack> {
    m.tracks
        .iter()
        .enumerate()
        .filter(|(_, tr)| tr.length_ms > 0)
        .map(|(i, tr)| {
            let (body, fade) = (u64::from(tr.length_ms), u64::from(tr.fade_ms));
            let name = format!("Track {}", i + 1);
            let shape = Shape { looping: fade > 0, duration_ms: body, channels: 2, cd_track: 0, rip: true };
            let kind = classify(&shape, &name, t);
            FoundTrack {
                location: TrackLocation::RomSong { engine: format!("{SNES_MAP}@{key}@{body}@{fade}"), song: i as u32 },
                name,
                kind,
                sample_rate: super::snes_apu::SAMPLE_RATE,
                channels: 2,
                duration_ms: body + fade,
                looping: false,
                loop_start: 0,
                loop_end: 0,
                codec: "SNES".into(),
                format: format!("song {}", i + 1),
                bytes: 0,
                video: false,
            }
        })
        .collect()
}

/// A SNES song played from the ROM by its map ([`SNES_MAP`]).
fn open_snes_map(path: &Path, engine: &str, song: u32) -> Result<Box<dyn PcmStream>, String> {
    let bad = || format!("a damaged ROM song record: {engine}");
    let parts: Vec<&str> = engine.split('@').collect();
    let [_, key, body, fade] = parts[..] else { return Err(bad()) };
    let body: u64 = body.parse().map_err(|_| bad())?;
    let fade: u64 = fade.parse().map_err(|_| bad())?;
    let m = load_snes_map(key).ok_or("this game's music map is no longer kept; scan its music again")?;
    let rom = read_rom(path)?;
    let song = super::snes_map::MapSong::open(&m, super::snes_sound::rom_image(&rom), song as usize)?;
    let r = u64::from(super::snes_apu::SAMPLE_RATE);
    let tail = if fade > 0 { super::track_timing::FADE_TAIL_MS } else { 0 };
    Ok(Box::new(SnesMapPcm { song, left: (body + fade + tail) * r / 1000, fade: fade * r / 1000, tail: tail * r / 1000 }))
}

struct SnesMapPcm {
    song: super::snes_map::MapSong,
    left: u64,
    fade: u64,
    tail: u64,
}

impl PcmStream for SnesMapPcm {
    fn sample_rate(&self) -> u32 {
        super::snes_apu::SAMPLE_RATE
    }
    fn channels(&self) -> u16 {
        2
    }
    fn total_samples(&self) -> Option<u64> {
        Some(self.left)
    }
    fn next_chunk(&mut self) -> Result<Option<Vec<i16>>, String> {
        if self.left == 0 {
            return Ok(None);
        }
        let n = self.left.min(4096) as usize;
        let mut buf = vec![0i16; n * 2];
        self.song.render(&mut buf)?;
        if self.fade > 0 {
            for (i, frame) in buf.chunks_exact_mut(2).enumerate() {
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

fn load_n64_map(key: &str) -> Option<super::n64_map::Map> {
    super::n64_map::Map::decode(&kept_map("n64", key)?)
}

/// The map of the N64 ROM `rom` from the first of `sets` whose USF tracks are this ROM's,
/// unless one is kept already. False when no set holds a USF or the ROM is not an N64 dump.
fn n64_map_from_sets(rom: &[u8], sets: &[PathBuf]) -> Result<bool, String> {
    let Some(native) = super::n64_map::native(rom) else { return Ok(false) };
    let key = gb_key(&native);
    // A map shipped or built before is this ROM's already: checked before every USF of the set
    // is read and matched, which is slow on a big set and was thrown away when a map was kept.
    if kept_map("n64", &key).is_some() {
        return Ok(true);
    }
    let mut any = false;
    for set in sets {
        let mut builder = super::n64_map::Builder::new(&native);
        let mut tracks = 0;
        super::rip_sets::for_each_usf(set, |loaded| {
            tracks += 1;
            builder.add(loaded);
        });
        if tracks == 0 {
            continue;
        }
        any = true;
        let Some(mut m) = builder.finish() else { continue };
        // A set every track of which plays silent here makes no map: it would list songs
        // that play nothing.
        let audible = |t: usize| m.section(&native, t).and_then(|s| super::n64_map::render(&s, m.compare, m.fifo_full, N64_AUDIBLE_SECONDS)).is_ok_and(|o| o.iter().any(|&x| x != 0));
        if !(0..m.tracks.len()).any(audible) {
            continue;
        }
        super::n64_map::slim(&mut m, &native)?;
        let file = MAPS_DIR.get().ok_or("no folder for music maps")?.join("n64").join(format!("{key}.n64map"));
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&file, m.encode()).map_err(|e| format!("{}: {e}", file.display()))?;
        return Ok(true);
    }
    Ok(any)
}

/// An N64 map's songs, timed by the set's own tags.
fn index_n64_map(key: &str, m: &super::n64_map::Map, t: &Thresholds) -> Vec<FoundTrack> {
    m.tracks
        .iter()
        .enumerate()
        .filter(|(_, tr)| tr.length_ms > 0)
        .map(|(i, tr)| {
            let (body, fade) = (u64::from(tr.length_ms), u64::from(tr.fade_ms));
            let name = format!("Track {}", i + 1);
            let shape = Shape { looping: fade > 0, duration_ms: body, channels: 2, cd_track: 0, rip: true };
            let kind = classify(&shape, &name, t);
            FoundTrack {
                location: TrackLocation::RomSong { engine: format!("{N64_MAP}@{key}@{body}@{fade}"), song: i as u32 },
                name,
                kind,
                sample_rate: super::n64_map::RATE,
                channels: 2,
                duration_ms: body + fade,
                looping: false,
                loop_start: 0,
                loop_end: 0,
                codec: "Nintendo 64".into(),
                format: format!("song {}", i + 1),
                bytes: 0,
                video: false,
            }
        })
        .collect()
}

/// An N64 song played from the ROM by its map ([`N64_MAP`]).
fn open_n64_map(path: &Path, engine: &str, song: u32) -> Result<Box<dyn PcmStream>, String> {
    let bad = || format!("a damaged ROM song record: {engine}");
    let parts: Vec<&str> = engine.split('@').collect();
    let [_, key, body, fade] = parts[..] else { return Err(bad()) };
    let body: u64 = body.parse().map_err(|_| bad())?;
    let fade: u64 = fade.parse().map_err(|_| bad())?;
    let m = load_n64_map(key).ok_or("this game's music map is no longer kept; scan its music again")?;
    let rom = read_rom(path)?;
    let native = super::n64_map::native(&rom).ok_or("not an N64 ROM")?;
    let song = super::n64_map::MapSong::open(&m, &native, song as usize)?;
    let r = u64::from(super::n64_map::RATE);
    let tail = if fade > 0 { super::track_timing::FADE_TAIL_MS } else { 0 };
    Ok(Box::new(N64MapPcm { song, left: (body + fade + tail) * r / 1000, fade: fade * r / 1000, tail: tail * r / 1000 }))
}

struct N64MapPcm {
    song: super::n64_map::MapSong,
    left: u64,
    fade: u64,
    tail: u64,
}

impl PcmStream for N64MapPcm {
    fn sample_rate(&self) -> u32 {
        super::n64_map::RATE
    }
    fn channels(&self) -> u16 {
        2
    }
    fn total_samples(&self) -> Option<u64> {
        Some(self.left)
    }
    fn next_chunk(&mut self) -> Result<Option<Vec<i16>>, String> {
        if self.left == 0 {
            return Ok(None);
        }
        let n = self.left.min(4096) as usize;
        let mut buf = vec![0i16; n * 2];
        self.song.render(&mut buf)?;
        if self.fade > 0 {
            for (i, frame) in buf.chunks_exact_mut(2).enumerate() {
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

fn gba_map_path(key: &str) -> Option<PathBuf> {
    Some(MAPS_DIR.get()?.join("gba").join(format!("{key}.gbamap")))
}

fn load_gba_map(key: &str) -> Option<super::gba_map::Map> {
    super::gba_map::Map::decode(&kept_map("gba", key)?)
}

/// The map of the GBA ROM `rom` from the first of `sets` whose GSF tracks are this ROM's,
/// unless one is kept already. False when no set holds a GSF.
fn gba_map_from_sets(rom: &[u8], sets: &[PathBuf]) -> Result<bool, String> {
    let key = gb_key(rom);
    let file = gba_map_path(&key).ok_or("no folder for music maps")?;
    // A kept map first, before every GSF of the set is read and placed (see the N64 one).
    if kept_map("gba", &key).is_some() {
        return Ok(true);
    }
    let mut any = false;
    for set in sets {
        let mut tracks = Vec::new();
        let mut theirs = true;
        let mut placer = super::gba_map::Placer::default();
        super::rip_sets::for_each_gsf(set, |loaded| {
            if !theirs {
                return;
            }
            let info = loaded.info();
            match placer.track_runs(loaded, rom) {
                Some(runs) => tracks.push((runs, info.length_ms, info.fade_ms)),
                None => theirs = false,
            }
        });
        if tracks.is_empty() && theirs {
            continue;
        }
        any = true;
        if !theirs {
            continue;
        }
        let Some(mut m) = super::gba_map::Map::from_tracks(rom.len(), tracks) else { continue };
        super::gba_map::slim(&mut m, rom)?;
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&file, m.encode()).map_err(|e| format!("{}: {e}", file.display()))?;
        return Ok(true);
    }
    Ok(any)
}

/// A GBA map's songs, timed by the set's own tags: mGBA gives no sound writes to find a
/// loop in.
fn index_gba_map(key: &str, m: &super::gba_map::Map, t: &Thresholds) -> Vec<FoundTrack> {
    let r = u64::from(super::psf::GbaImage::RATE);
    m.tracks
        .iter()
        .enumerate()
        .filter(|(_, tr)| tr.length_ms > 0)
        .map(|(i, tr)| {
            let (body, fade) = (u64::from(tr.length_ms), u64::from(tr.fade_ms));
            let name = format!("Track {}", i + 1);
            // A fade means the song loops and was cut off.
            let shape = Shape { looping: fade > 0, duration_ms: body, channels: 2, cd_track: 0, rip: true };
            let kind = classify(&shape, &name, t);
            FoundTrack {
                location: TrackLocation::RomSong { engine: format!("{GBA_MAP}@{key}@{body}@{fade}"), song: i as u32 },
                name,
                kind,
                sample_rate: super::psf::GbaImage::RATE,
                channels: 2,
                duration_ms: body + fade,
                looping: false,
                loop_start: 0,
                loop_end: 0,
                codec: "Game Boy Advance".into(),
                format: format!("song {}", i + 1),
                bytes: 0,
                video: false,
            }
        })
        .filter(|_| r > 0)
        .collect()
}

/// A GBA song played from the ROM by its map ([`GBA_MAP`]).
fn open_gba_map(path: &Path, engine: &str, song: u32) -> Result<Box<dyn PcmStream>, String> {
    let bad = || format!("a damaged ROM song record: {engine}");
    let parts: Vec<&str> = engine.split('@').collect();
    let [_, key, body, fade] = parts[..] else { return Err(bad()) };
    let body: u64 = body.parse().map_err(|_| bad())?;
    let fade: u64 = fade.parse().map_err(|_| bad())?;
    let m = load_gba_map(key).ok_or("this game's music map is no longer kept; scan its music again")?;
    let rom = read_rom(path)?;
    let image = m.image(&rom, song as usize)?;
    let player = super::psf::GbaImage::open(&image)?;
    let r = u64::from(super::psf::GbaImage::RATE);
    let tail = if fade > 0 { super::track_timing::FADE_TAIL_MS } else { 0 };
    Ok(Box::new(GbaMapPcm { player, left: (body + fade + tail) * r / 1000, fade: fade * r / 1000, tail: tail * r / 1000 }))
}

struct GbaMapPcm {
    player: super::psf::GbaImage,
    left: u64,
    fade: u64,
    tail: u64,
}

impl PcmStream for GbaMapPcm {
    fn sample_rate(&self) -> u32 {
        super::psf::GbaImage::RATE
    }
    fn channels(&self) -> u16 {
        2
    }
    fn total_samples(&self) -> Option<u64> {
        Some(self.left)
    }
    fn next_chunk(&mut self) -> Result<Option<Vec<i16>>, String> {
        if self.left == 0 {
            return Ok(None);
        }
        let n = self.left.min(4096) as usize;
        let mut buf = vec![0i16; n * 2];
        if self.player.render(&mut buf).is_err() {
            // A minute of silence: the song is over.
            self.left = 0;
            return Ok(None);
        }
        if self.fade > 0 {
            for (i, frame) in buf.chunks_exact_mut(2).enumerate() {
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

/// A Game Boy track's notes as a loop is found in them: each note started (its channel and
/// pitch to the semitone) and each volume envelope set. Pitch moves within a note (vibrato,
/// slides), duty and length, panning and wave memory are left out: drivers run those on
/// counts that don't restart with the loop.
fn gb_notes() -> impl FnMut(&music_search::Write) -> Option<u64> {
    let mut low = [0u8; 3];
    move |w| {
        let ch = match w.port {
            0xFF13 | 0xFF14 => 0,
            0xFF18 | 0xFF19 => 1,
            0xFF1D | 0xFF1E => 2,
            0xFF12 | 0xFF17 | 0xFF1C | 0xFF21 => return Some(u64::from(w.port) << 16 | u64::from(w.value)),
            0xFF23 => return (w.value & 0x80 != 0).then_some(0xFF23 << 16),
            _ => return None,
        };
        if matches!(w.port, 0xFF13 | 0xFF18 | 0xFF1D) {
            low[ch] = w.value as u8;
            return None;
        }
        if w.value & 0x80 == 0 {
            return None;
        }
        let x = (u32::from(w.value & 7) << 8) | u32::from(low[ch]);
        let hz = 131_072.0 / f64::from(2048 - x.min(2047));
        let semitone = (12.0 * hz.log2()).round() as i64;
        Some(u64::from(w.port) << 16 | (semitone as u64 & 0xFFFF))
    }
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
    super::gb_map::Map::decode(&kept_map("gb", key)?)
}

/// Builds the map of the Game Boy ROM at `path` from the first of `sets` that holds a GBS
/// (only to find and check where its music is; nothing of the file is kept), unless one is
/// kept already.
fn gb_map_from_sets(path: &Path, sets: &[PathBuf]) -> Result<(), String> {
    let rom = read_rom(path)?;
    let key = gb_key(&rom);
    let file = gb_map_path(&key).ok_or("no folder for music maps")?;
    if kept_map("gb", &key).is_some() {
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
    let songs: Vec<u8> = (0..m.header.songs).filter(|&v| !matches!(m.ways.get(v as usize), None | Some(super::gb_map::Way::None))).collect();
    let timed = map_times("gb", key, &m.encode(), &songs, true, |v| {
        let mut song = super::gb_map::MapSong::open(m, rom, v, MEASURE_RATE).ok()?;
        let rate = song.frame_rate();
        Some((rate, music_search::measure_frames_by_writes(rate, gb_notes(), |samples| song.frame(samples).ok_or_else(|| "the song stopped".to_string())).ok()?))
    });
    for (v, rate, timing) in timed {
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
    map_from_sets_for(path, sets, None)
}

/// [`map_from_sets`] for a ROM of a known platform: only that platform's kind of map is
/// tried, so its sets are read once, not once by every kind in turn.
pub fn map_from_sets_for(path: &Path, sets: &[PathBuf], platform: Option<u16>) -> Result<(), String> {
    use crate::common::models::Platform as P;
    if let Some(p) = platform {
        let is = |q: P| p == q as u16;
        let done = |r: Result<bool, String>| r.map(|_| ());
        if is_nes(p) || is_gb(p) {
            return map_from_sets_for(path, sets, None);
        }
        let rom = read_rom(path)?;
        if is(P::TurboGrafx16) {
            return done(hes_map_from_sets(&rom, sets));
        }
        if is(P::WonderSwan) || is(P::WonderSwanColor) {
            return done(ws_map_from_sets(&rom, sets));
        }
        if is(P::GameBoyAdvance) {
            return done(gba_map_from_sets(&rom, sets));
        }
        if is_snes(p) {
            return done(snes_map_from_sets(&rom, sets));
        }
        if is(P::Nintendo64) {
            return done(n64_map_from_sets(&rom, sets));
        }
        if is(P::SegaMasterSystem) || is(P::SegaGameGear) {
            return sgc_map_from_sets(&rom, sets);
        }
    }
    let rom = read_rom(path)?;
    // A Game Boy ROM carries the Nintendo logo at $104.
    if rom.get(0x104..0x108) == Some(&[0xCE, 0xED, 0x66, 0x66]) {
        return gb_map_from_sets(path, sets);
    }
    // A headerless NES dump has nothing to tell it from other consoles' ROMs but the NSF it
    // matches.
    let headerless = super::nes_nsf::is_headerless(&rom);
    if super::nes_nsf::program(&rom).is_some() {
        if has_map(&rom) {
            return Ok(());
        }
        let mut any = false;
        for set in sets {
            if let Some(nsf) = super::rip_sets::first_with_magic(set, b"NESM\x1a") {
                any = true;
                if build_nes_map(&rom, &nsf)? {
                    return Ok(());
                }
            }
        }
        if any || !headerless {
            return Ok(());
        }
    }
    if hes_map_from_sets(&rom, sets)? {
        return Ok(());
    }
    if ws_map_from_sets(&rom, sets)? {
        return Ok(());
    }
    if gba_map_from_sets(&rom, sets)? {
        return Ok(());
    }
    if snes_map_from_sets(&rom, sets)? {
        return Ok(());
    }
    if n64_map_from_sets(&rom, sets)? {
        return Ok(());
    }
    sgc_map_from_sets(&rom, sets)
}

/// The songs a map plays, measured and classified like [`index_machine`]'s.
/// `only`: the tracks to list, when the map is another copy's (see [`borrowed_nes`]).
fn index_map(rom: &[u8], key: &str, m: &super::nes_nsf::Map, t: &Thresholds, only: Option<&std::collections::HashSet<u8>>) -> Result<Vec<FoundTrack>, String> {
    let mut out = Vec::new();
    let songs: Vec<u8> = (0..m.header.songs).filter(|&v| !matches!(m.ways.get(v as usize), None | Some(super::nes_nsf::Way::None)) && only.is_none_or(|o| o.contains(&v))).collect();
    // A map played on another copy of its game (`only`) is timed on that copy, not kept.
    let timed = map_times("nes", key, &m.encode(), &songs, only.is_none(), |v| {
        let mut song = super::nes_nsf::MapSong::open(m, rom, v, MEASURE_RATE).ok()?;
        let rate = song.frame_rate();
        Some((rate, music_search::measure_frames(rate, |samples| song.frame(samples).ok_or_else(|| "the song stopped".to_string())).ok()?))
    });
    for (v, rate, timing) in timed {
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

/// When two found songs are one piece. With a console's melody fingerprint (the notes its tone
/// channels play), both the writes and the notes must agree; songs that share their drums,
/// bass and tempo share most writes but not their notes. Without one (or with too few notes
/// to tell), the writes alone must agree nearly throughout.
const SAME_PIECE_WRITES: f64 = 0.9;
const SAME_PIECE_NOTES: f64 = 0.9;
const SAME_PIECE_WRITES_ALONE: f64 = 0.98;

type Melody = fn(&[Vec<music_search::Write>]) -> std::collections::HashSet<u64>;

/// A found song's (writes print, notes print).
type Print = (std::collections::HashSet<u64>, Option<std::collections::HashSet<u64>>);

/// Most two found songs' measured lengths differ, in percent, for them to be one piece.
const SAME_PIECE_LENGTH_PCT: usize = 3;

/// Whether `a` and `b` (writes print, notes print) are one piece ([`SAME_PIECE_WRITES`]).
fn same_piece(a: &Print, b: &Print) -> bool {
    let writes = music_search::alike(&a.0, &b.0);
    match (&a.1, &b.1) {
        (Some(x), Some(y)) if x.len().min(y.len()) >= music_search::MIN_MELODY => writes >= SAME_PIECE_WRITES && music_search::alike(x, y) >= SAME_PIECE_NOTES,
        _ => writes >= SAME_PIECE_WRITES_ALONE,
    }
}

fn index_machine<M: Machine>(path: &Path, t: &Thresholds, engine: &str, codec: &str) -> Result<(Vec<FoundTrack>, Option<String>), String> {
    index_machine_with::<M>(path, t, engine, codec, None)
}

fn index_machine_with<M: Machine>(path: &Path, t: &Thresholds, engine: &str, codec: &str, melody: Option<Melody>) -> Result<(Vec<FoundTrack>, Option<String>), String> {
    let rom = read_rom(path)?;
    let found = music_search::search::<M>(&rom)?;
    if found.songs.is_empty() {
        return Ok((Vec::new(), found.why));
    }
    let rate = M::boot(&rom)?.frame_rate();
    let ms = |frames: usize| (frames as f64 * 1000.0 / rate) as u64;
    let recipe = found.recipe.encode();
    let mut out = Vec::with_capacity(found.songs.len());
    let mut failed = None;
    let mut prints: Vec<(usize, Print)> = Vec::new();
    for &v in &found.songs {
        // A song that breaks off is left out, not the game.
        let timing = match music_search::measure::<M>(&rom, &found.recipe, v) {
            Ok(t) => t,
            Err(e) => {
                failed.get_or_insert(e);
                continue;
            }
        };
        // A song number that starts a piece already listed (the game's song variable read
        // only once, so every number plays the same) is that piece again: left out.
        if let Ok(frames) = music_search::first_writes::<M>(&rom, &found.recipe, v) {
            let print = (music_search::writes_print(&frames), melody.map(|f| f(&frames)));
            // A version of a tune at another tempo is a song of its own: one piece also lasts
            // as long.
            let len = timing.frames;
            if prints.iter().any(|(l, p)| len.abs_diff(*l) * 100 <= len.max(*l) * SAME_PIECE_LENGTH_PCT && same_piece(p, &print)) {
                continue;
            }
            prints.push((len, print));
        }
        let (looping, intro, lp) = match timing.looping {
            Some((start, len)) => (true, ms(start), ms(len)),
            None => (false, ms(timing.frames), 0),
        };
        let duration_ms = if looping { intro + lp } else { intro };
        // Played: the intro and the loop twice, then a fade; a song that ends plays to its end.
        let (body, fade) = if looping { (intro + lp * u64::from(PLAY_LOOPS), SONG_FADE_MS) } else { (duration_ms, 0) };
        let name = format!("Track {}", out.len() + 1);
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
    let why = if out.is_empty() { failed } else { None };
    Ok((out, why))
}

/// A song from the game's own code, played for `left` more sample frames, the last `fade` of
/// them fading out.
struct MachinePcm<M: Machine> {
    song: music_search::Song<M>,
    left: u64,
    fade: u64,
    tail: u64,
    pending: Vec<i16>,
    /// Some of the song has been handed out.
    played: bool,
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
            if let Err(e) = self.song.frame(&mut self.pending) {
                // A song that stops (a jingle reaching its end, a game that halts) before its
                // measured length ends there: what it played stands. An error before any sound
                // is the track's error.
                if !self.played && self.pending.is_empty() {
                    return Err(e);
                }
                self.left = self.left.min((self.pending.len() / ch) as u64);
                self.fade = self.fade.min(self.left);
                if self.left == 0 {
                    return Ok(None);
                }
                break;
            }
        }
        self.played = true;
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
    Ws(super::ws_map::MapSong),
}

impl AnyMapSong {
    fn frame(&mut self, out: &mut Vec<i16>) -> Option<Vec<music_search::Write>> {
        match self {
            AnyMapSong::Nes(s) => s.frame(out),
            AnyMapSong::Gb(s) => s.frame(out),
            AnyMapSong::Sgc(s) => s.frame(out),
            AnyMapSong::Hes(s) => s.frame(out),
            AnyMapSong::Ws(s) => s.frame(out),
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

/// A song record with its measured timing (body and fade) taken out: what stays the same
/// when a re-index measures a song a little differently, so a kind the user set on it is
/// found again. A booted engine's record ends in its timing; every other record has it
/// right after its first field.
pub fn without_timing(engine: &str) -> String {
    let mut parts: Vec<&str> = engine.split('@').collect();
    let n = parts.len();
    if machine_record(engine).is_some() {
        parts.truncate(n - 2);
    } else if n >= 4 && [parts[2], parts[3]].iter().all(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit())) {
        parts.drain(2..4);
    }
    parts.join("@")
}

/// A booted engine's song record: the engine's name, the recipe's four fields (five when the
/// sound routine is called more than once a frame), then body and fade.
fn machine_record(engine: &str) -> Option<(music_search::Recipe, u64, u64)> {
    let parts: Vec<&str> = engine.split('@').collect();
    let n = parts.len();
    if !(7..=8).contains(&n) {
        return None;
    }
    let recipe = music_search::Recipe::decode(&parts[1..n - 2].join("@"))?;
    Some((recipe, parts[n - 2].parse().ok()?, parts[n - 1].parse().ok()?))
}

fn open_machine<M: Machine + Send + 'static>(path: &Path, engine: &str, song: u32) -> Result<Box<dyn PcmStream>, String>
where
    M::Entry: Send,
    M::Chip: Send,
{
    let bad = || format!("a damaged ROM song record: {engine}");
    let (recipe, body, fade) = machine_record(engine).ok_or_else(bad)?;
    let rom = read_rom(path)?;
    let v = u8::try_from(song).map_err(|_| bad())?;
    let player = music_search::Song::<M>::open(&rom, &recipe, v, PLAY_RATE)?;
    let r = u64::from(PLAY_RATE);
    let tail = if fade > 0 { super::track_timing::FADE_TAIL_MS } else { 0 };
    Ok(Box::new(MachinePcm { song: player, left: (body + fade + tail) * r / 1000, fade: fade * r / 1000, tail: tail * r / 1000, pending: Vec::new(), played: false }))
}

#[cfg(test)]
mod bundle_tests {
    use super::*;

    #[test]
    fn a_song_record_without_its_timing_stays_the_same_when_remeasured() {
        assert_eq!(without_timing("md@120@1@1a2b@m0040@2@90000@8000"), without_timing("md@120@1@1a2b@m0040@2@91000@0"));
        assert_eq!(without_timing("gbamap@ab12@90000@8000"), "gbamap@ab12");
        assert_eq!(without_timing("triace@3@90000@8000@1f00@200@BGM/A.SEQ"), "triace@3@1f00@200@BGM/A.SEQ");
        assert_ne!(without_timing("gbamap@ab12@90000@8000"), without_timing("gbamap@ab13@90000@8000"));
    }

    #[test]
    fn a_song_record_reads_with_and_without_a_call_count() {
        let (r, body, fade) = machine_record("md@120@1@1a2b@m0040@90000@8000").unwrap();
        assert_eq!((r.calls, body, fade), (1, 90000, 8000));
        let (r, body, fade) = machine_record("md@120@1@1a2b@m0040@2@90000@8000").unwrap();
        assert_eq!((r.calls, body, fade), (2, 90000, 8000));
        assert!(machine_record("md@120@1@1a2b@m0040@90000").is_none());
    }

    #[test]
    fn map_times_read_back_only_for_the_map_they_were_measured_for() {
        let mut times = std::collections::BTreeMap::new();
        times.insert(0u8, Some((60.098_813_897_440_5, music_search::Timing { frames: 3600, looping: Some((120, 2400)) })));
        times.insert(1, Some((59.727_500_569_605_83, music_search::Timing { frames: 90, looping: None })));
        times.insert(4, None);
        let print = map_fingerprint("nesmap 1\nline two\n");
        let text = encode_times(&print, &times);
        assert_eq!(decode_times(&text, &print), Some(times.clone()), "rates, frames and loops come back exactly");
        assert_eq!(decode_times(&text, &map_fingerprint("nesmap 1\nline 2\n")), None, "times of another version of the map");
        assert_eq!(decode_times(&text.replacen(TIMES_MAGIC, "maptimes 0", 1), &print), None, "times measured the old way");
        assert_eq!(decode_times(&format!("{TIMES_MAGIC} {print}\n3 junk\n"), &print), None);
    }

    #[test]
    fn map_times_known_only_measure_nothing() {
        KNOWN_ONLY.with(|k| k.set(true));
        KNOWN_ONLY_MISSED.with(|m| m.set(false));
        let mut measured = 0;
        let got = map_times("nes", "00ff", "nesmap 1\n", &[0, 1], false, |_| {
            measured += 1;
            None
        });
        let missed = KNOWN_ONLY_MISSED.with(|m| m.get());
        KNOWN_ONLY.with(|k| k.set(false));
        assert_eq!(measured, 0, "nothing measured");
        assert!(got.is_empty() && missed, "a song without a kept time sends the map to a full index");
    }

    #[test]
    fn map_times_not_kept_measure_every_song() {
        let mut measured = Vec::new();
        let mut measure = |v: u8| {
            measured.push(v);
            (v != 2).then_some((60.0, music_search::Timing { frames: 600 + usize::from(v), looping: None }))
        };
        // Not kept (`keep` false): nothing read, nothing written, every song measured.
        let got = map_times("nes", "00ff", "nesmap 1\n", &[0, 1, 2], false, &mut measure);
        assert_eq!(got.iter().map(|(v, _, t)| (*v, t.frames)).collect::<Vec<_>>(), vec![(0, 600), (1, 601)]);
        assert_eq!(measured, vec![0, 1, 2]);
    }

    #[test]
    fn a_bundle_packs_each_maps_times_beside_it() {
        let dir = std::env::temp_dir().join(format!("music_map_times_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("gb")).unwrap();
        std::fs::write(dir.join("gb").join("aa.gbmap"), "gbmap 1\n").unwrap();
        std::fs::write(dir.join("gb").join("aa.times"), "maptimes 1 x\n").unwrap();
        std::fs::create_dir_all(dir.join("ws")).unwrap();
        // Times of a map shipped in another bundle still pack.
        std::fs::write(dir.join("ws").join("bb.times"), "maptimes 1 y\n").unwrap();
        let (packed, counts) = pack_bundle(&dir, &["gb"]).unwrap();
        let split = pack_bundles(&dir).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(counts, vec![("gb".to_string(), 1)], "times are not counted as maps");
        let maps = read_bundle(&packed).unwrap();
        assert_eq!(maps["gb/aa"], "gbmap 1\n");
        assert_eq!(maps["gb/aa.times"], "maptimes 1 x\n");
        assert_eq!(split.iter().map(|(k, _, _)| k.as_str()).collect::<Vec<_>>(), vec!["music_maps_gb.zst", "music_maps_ws.zst"]);
        assert_eq!(read_bundle(&split[1].1).unwrap().keys().collect::<Vec<_>>(), vec!["ws/bb.times"]);
    }

    #[test]
    fn times_are_added_to_a_shipped_bundle_beside_their_maps() {
        let dir = std::env::temp_dir().join(format!("music_map_add_times_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("maps").join("nes")).unwrap();
        std::fs::create_dir_all(dir.join("times").join("nes")).unwrap();
        std::fs::write(dir.join("maps").join("nes").join("aa.nesmap"), "nesmap 1\n").unwrap();
        std::fs::write(dir.join("maps").join("nes").join("bb.nesmap"), "nesmap 2\n").unwrap();
        std::fs::write(dir.join("times").join("nes").join("aa.times"), "maptimes 1 x\n").unwrap();
        std::fs::write(dir.join("times").join("nes").join("zz.times"), "maptimes 1 y\n").unwrap();
        let (packed, _) = pack_bundle(&dir.join("maps"), &["nes"]).unwrap();
        let (again, maps, timed) = add_times_to_bundle(&packed, &dir.join("times")).unwrap();
        let again = again.expect("times were added");
        // Run again over its own output: nothing to add, nothing rewritten.
        let (same, ..) = add_times_to_bundle(&again, &dir.join("times")).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert!(same.is_none());
        assert_eq!((maps, timed), (2, 1));
        let entries = read_bundle(&again).unwrap();
        assert_eq!(entries.len(), 3, "the maps, and times only for a map the bundle holds");
        assert_eq!(entries["nes/aa.times"], "maptimes 1 x\n");
        assert_eq!(entries["nes/bb"], "nesmap 2\n");
    }

    #[test]
    fn a_bundle_gives_back_every_map_packed_into_it() {
        let dir = std::env::temp_dir().join(format!("music_map_bundle_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("nes")).unwrap();
        std::fs::create_dir_all(dir.join("hes")).unwrap();
        std::fs::write(dir.join("nes").join("aa.nesmap"), "nesmap 1\nline two\n").unwrap();
        std::fs::write(dir.join("hes").join("bb.hesmap"), "hesmap 1\n").unwrap();
        std::fs::write(dir.join("nes").join("stray.txt"), "not a map").unwrap();
        let (packed, counts) = pack_bundle(&dir, &["nes", "hes"]).unwrap();
        let split = pack_bundles(&dir).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(counts.iter().map(|(_, n)| n).sum::<usize>(), 2);
        let maps = read_bundle(&packed).unwrap();
        assert_eq!(maps.len(), 2);
        assert_eq!(maps["nes/aa"], "nesmap 1\nline two\n");
        assert_eq!(maps["hes/bb"], "hesmap 1\n");
        // One bundle per kind, each holding only its own.
        assert_eq!(split.iter().map(|(k, _, n)| (k.as_str(), *n)).collect::<Vec<_>>(), vec![("music_maps_nes.zst", 1), ("music_maps_hes.zst", 1)]);
        assert_eq!(read_bundle(&split[1].1).unwrap().keys().collect::<Vec<_>>(), vec!["hes/bb"]);
    }

    #[test]
    fn a_kind_too_big_for_one_file_is_split_by_the_keys_first_digit() {
        let dir = std::env::temp_dir().join(format!("music_map_shards_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("snes")).unwrap();
        // Maps that don't pack together, so each digit's size adds up.
        let mut x = 1u32;
        for key in ["0a", "0b", "a1", "F2"] {
            let body: String = (0..4000).map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                char::from_digit((x >> 16) % 16, 16).unwrap()
            }).collect();
            std::fs::write(dir.join("snes").join(format!("{key}.snesmap")), format!("snesmap 1\n{body}\n")).unwrap();
        }
        let split = pack_bundles_split(&dir, 0, "").unwrap();
        let updates = pack_bundles_split(&dir, 0, "_updates").unwrap();
        // Room for digit 0's two maps, or for the a and f ones together.
        let zero = pack_maps(&dir, &["snes"], Some(('0', '0'))).unwrap().0.len();
        let grouped = pack_bundles_split(&dir, zero + 256, "").unwrap();
        let back: Vec<String> = grouped.iter().flat_map(|(_, packed, _)| read_bundle(packed).unwrap().into_keys()).collect();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(
            split.iter().map(|(f, _, n)| (f.as_str(), *n)).collect::<Vec<_>>(),
            vec![("music_maps_snes_0-0.zst", 2), ("music_maps_snes_a-a.zst", 1), ("music_maps_snes_f-f.zst", 1)]
        );
        assert_eq!(grouped.iter().map(|(f, _, n)| (f.as_str(), *n)).collect::<Vec<_>>(), vec![("music_maps_snes_0-9.zst", 2), ("music_maps_snes_a-f.zst", 2)]);
        assert_eq!(back.len(), 4);
        assert_eq!(updates[0].0, "music_maps_snes_updates_0-0.zst");
        assert_eq!(bundle_name("snes", &updates[0].0), Some(BundleName { range: Some(('0', '0')), order: u32::MAX }));
    }

    #[test]
    fn bundle_names_take_parts_and_files_added_later() {
        let name = |kind: &str, file: &str| bundle_name(kind, file).map(|b| (b.range, b.order));
        assert_eq!(name("snes", "music_maps_snes.zst"), Some((None, 0)));
        assert_eq!(name("snes", "music_maps_snes_a-f.zst"), Some((Some(('a', 'f')), 0)));
        assert_eq!(name("snes", "music_maps_snes_2.zst"), Some((None, 2)), "added later, every key");
        assert_eq!(name("snes", "music_maps_snes_0-7_3.zst"), Some((Some(('0', '7')), 3)));
        assert_eq!(name("snes", "music_maps_snes_updates.zst"), Some((None, u32::MAX)));
        assert_eq!(name("snes", "music_maps_snes_updates_0-0.zst"), Some((Some(('0', '0')), u32::MAX)));
        assert_eq!(name("gb", "music_maps_gba.zst"), None, "another kind whose name starts the same");
        assert_eq!(name("gb", "music_maps_gba_2.zst"), None);
        assert_eq!(name("gba", "music_maps_snes_a-f.zst"), None);
        assert_eq!(name("snes", "music_maps_snes_0.zst"), None, "numbers start at 1");
        assert_eq!(name("snes", "music_maps_snes_x-y.zst"), None);
        assert_eq!(name("snes", "music_maps.zst"), None);
    }
}
