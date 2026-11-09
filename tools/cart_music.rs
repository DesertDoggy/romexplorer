//! Developer tool: the cartridge music search (`rom::music_search`) over ROMs on this PC, to
//! see how many games it finds music in and why not in the rest.
//!
//!   cart_music [--render <dir>] [--seconds N] [--verbose] [--probe SECONDS] [--originals <folder> [--layout | --mapcheck]] <rom or folder>...
//!
//! For each NES/Famicom (cartridge or Disk System), Game Boy/Color, Master System, Game Gear,
//! Mega Drive, PC Engine, MSX, WonderSwan/Color, Virtual Boy, Game Boy Advance or Neo Geo Pocket/Color ROM (loose, or the first one in a `.zip`/`.7z`): the
//! console, how long the search took, the song numbers it found, or why none. `--render` also
//! writes the first N seconds (default 20) of every song as a WAV, to listen to. `--originals`
//! (or `--nsf`) matches each NES/Famicom/Disk System and Game Boy/Color ROM to the original
//! audio format files (NSF, GBS) under a folder, and points the search at the sound code the
//! matched file names (the music still plays from the ROM). A summary ends the
//! run.

use std::io::Write;
use std::path::{Path, PathBuf};

use romexplorer::rom::gb_music::GameBoy;
use romexplorer::rom::md_music::MegaDrive;
use romexplorer::rom::music_search::{self, Machine, Song};
use romexplorer::rom::nes_music::Nes;
use romexplorer::rom::msx_music::Msx;
use romexplorer::rom::pce_music::PcEngine;
use romexplorer::rom::sms_music::{GameGear, MasterSystem};
use romexplorer::rom::gba_music::GameBoyAdvance;
use romexplorer::rom::ngp_music::NeoGeoPocket;
use romexplorer::rom::vb_music::VirtualBoy;
use romexplorer::rom::wswan_music::WonderSwan;
use romexplorer::common::models::Platform;
use romexplorer::rom::{gb_gbs, gb_map, hes_map, sgc_map, snes_sound, spc_map, music_map, nes_cart, nes_nsf, rip_match, rip_sets, rom_songs};

fn wav(path: &Path, pcm: &[i16], rate: u32, channels: u16) -> std::io::Result<()> {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let data = (pcm.len() * 2) as u32;
    let block = channels as u32 * 2;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&channels.to_le_bytes())?;
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * block).to_le_bytes())?;
    f.write_all(&(block as u16).to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data.to_le_bytes())?;
    for s in pcm {
        f.write_all(&s.to_le_bytes())?;
    }
    Ok(())
}

fn collect(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(path)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .collect();
        entries.sort();
        for e in entries {
            collect(&e, out);
        }
    } else {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(
            ext.as_str(),
            "nes"
                | "fds"
                | "gb"
                | "gbc"
                | "sms"
                | "gg"
                | "md"
                | "gen"
                | "smd"
                | "pce"
                | "mx1"
                | "mx2"
                | "rom"
                | "ws"
                | "wsc"
                | "vb"
                | "gba"
                | "sfc"
                | "smc"
                | "ngp"
                | "ngc"
                | "npc"
                | "zip"
                | "7z"
        ) {
            out.push(path.to_path_buf());
        }
    }
}

/// Which console a ROM is for, from its header (or, for a Sega 8-bit ROM, the file name).
fn console(rom: &[u8], path: &Path) -> Option<&'static str> {
    let lower = path.to_string_lossy().to_ascii_lowercase();
    if lower.ends_with(".gba") || lower.contains("game boy advance") {
        return Some("GBA");
    }
    if nes_cart::parse(rom).is_ok() {
        return Some("NES");
    }
    if romexplorer::rom::fds::is_disk(rom) {
        return Some("FDS");
    }
    // The Nintendo logo every Game Boy cartridge carries.
    if rom.len() >= 0x150 && rom[0x104..0x108] == [0xCE, 0xED, 0x66, 0x66] {
        return Some(if rom[0x143] & 0x80 != 0 { "GBC" } else { "GB" });
    }
    // The Mega Drive header.
    if rom.len() >= 0x200 && (&rom[0x100..0x104] == b"SEGA" || &rom[0x101..0x105] == b"SEGA") {
        return Some("MD");
    }
    let name = path.to_string_lossy().to_ascii_lowercase();
    if name.ends_with(".md") || name.ends_with(".gen") || name.ends_with(".smd") {
        return Some("MD");
    }
    if name.ends_with(".mx1")
        || name.ends_with(".mx2")
        || (name.contains("msx")
            && (name.ends_with(".rom") || name.ends_with(".zip") || name.ends_with(".7z")))
    {
        return Some("MSX");
    }
    if name.ends_with(".sfc") || name.ends_with(".smc") || name.contains("super famicom") || name.contains("super nintendo") {
        return Some("SNES");
    }
    if name.ends_with(".pce")
        || name.contains("pc engine")
        || name.contains("pcengine")
        || name.contains("turbografx")
    {
        return Some("PCE");
    }
    if name.ends_with(".ngc") || name.ends_with(".npc") || name.contains("neo geo pocket color") {
        return Some("NGPC");
    }
    if name.ends_with(".ngp") || name.contains("neo geo pocket") {
        return Some("NGP");
    }
    if name.ends_with(".vb") || name.contains("virtual boy") {
        return Some("VB");
    }
    if name.ends_with(".wsc") || name.contains("wonderswan color") {
        return Some("WSC");
    }
    if name.ends_with(".ws") || name.contains("wonderswan") {
        return Some("WS");
    }
    if (name.ends_with(".nes") || name.ends_with(".unh")) && nes_nsf::is_headerless(rom) {
        return Some("NES?");
    }
    if name.ends_with(".gg") || name.contains("game gear") || name.contains("gamegear") {
        return Some("GG");
    }
    if name.ends_with(".sms") || name.contains("master system") || name.contains("mastersystem") {
        return Some("SMS");
    }
    None
}

/// What the run found.
#[derive(Default)]
struct Tally {
    games: usize,
    with_music: usize,
    songs: usize,
    reasons: std::collections::BTreeMap<String, usize>,
    /// Against the matched NSF's tracks: games checked, their songs, songs like a track,
    /// tracks, tracks some song is like.
    checked: [usize; 5],
    /// Games whose songs are like none of the file's tracks (kept, unchecked), and their songs.
    unchecked: [usize; 2],
    /// `--mapcheck`: tracks played right on the game's own board.
    board: usize,
    /// `--mapcheck`: tracks the saved maps play right (each by its chosen player).
    saved: usize,
}

/// Frames of a song and of a track compared by melody.
const CHECK_FRAMES: usize = 600;
/// A song this alike a track (see `nes_nsf::alike`) is that track.
const CHECK_ALIKE: f64 = 0.5;

type Melody = std::collections::HashSet<u64>;
type Writes = Vec<Vec<music_search::Write>>;

/// The known answer found songs are checked against: the melodies of an NSF's or GBS's
/// tracks (song count at `count_at`), and how a song's melody is taken.
struct Answers {
    tracks: Vec<Melody>,
    melody: fn(&[Vec<music_search::Write>]) -> Melody,
}

fn answers(
    file: &[u8],
    count_at: usize,
    track: fn(&[u8], u8, usize) -> Option<Writes>,
    melody: fn(&[Vec<music_search::Write>]) -> Melody,
) -> Answers {
    let songs = file.get(count_at).copied().unwrap_or(0);
    let tracks = (0..songs)
        .map(|t| track(file, t, CHECK_FRAMES).map(|f| melody(&f)).unwrap_or_default())
        .collect();
    Answers { tracks, melody }
}

struct Options {
    render: Option<PathBuf>,
    seconds: u32,
    verbose: bool,
    /// `--probe N`: instead of searching, boot N seconds under each input pattern and print
    /// when steady sound first starts.
    probe: Option<u32>,
    /// `--originals <folder>`: original audio format files (NSF, GBS) to match ROMs to and
    /// take the sound code's addresses from.
    nsf: Option<PathBuf>,
    /// `--layout`: with `--originals`, instead of searching, lay each matched NSF's bytes its
    /// tracks read over the ROM and print how much of it is found where (a music map's first
    /// part).
    layout: bool,
    /// `--mapcheck`: with `--originals`, build each NES ROM's music map from its matched NSF and
    /// play every track from the ROM by it, against the NSF's own.
    mapcheck: bool,
    /// `--write-maps <dir>`: with `--originals`, build each matched ROM's music map the way the
    /// app does and keep it under `<dir>` (`nes/<key>.nesmap`, ...), for `--pack-maps`.
    write_maps: Option<PathBuf>,
    /// `--spcprobe`: with `--originals`, boot each SNES ROM, then start songs by writing each
    /// value to each port of its sound chip, and count the `.spc` tracks that start that way.
    spcprobe: bool,
    /// `--spcsearch`: with `--originals`, run each SNES ROM, find the work RAM byte it asks
    /// for songs with, play each value of it and count the matched set's tracks heard.
    spcsearch: bool,
    /// `--spcpoke <hex address>`: play values 0-127 through that work RAM byte, for diagnosis.
    spcpoke: Option<u32>,
    /// `--spcbank`: boot each SNES ROM, lay each track's song and samples in from the ROM,
    /// send the driver the track's command, and count the tracks heard.
    spcbank: bool,
}

fn run<M: Machine>(path: &Path, rom: &[u8], what: &str, o: &Options, t: &mut Tally) {
    run_hinted::<M>(path, rom, what, None, None, o, t)
}

/// The search hint for a ROM from the `--originals` library: the best-matching set's first
/// file starting with `magic`, read by `read`.
fn original_hint(
    lib: Option<&rip_match::Library>,
    path: &Path,
    platform: Platform,
    magic: &[u8],
    read: fn(&[u8]) -> Option<music_search::Hint>,
    verbose: bool,
) -> Option<(music_search::Hint, Vec<u8>)> {
    let set = matched_set(lib, path, platform, verbose)?;
    let hint = rip_sets::first_with_magic(&set, magic).and_then(|d| Some((read(&d)?, d)));
    if verbose && hint.is_none() {
        println!("    (no NSF/GBS in it)");
    }
    hint
}

/// The original audio format set a ROM matches best.
fn matched_set(lib: Option<&rip_match::Library>, path: &Path, platform: Platform, verbose: bool) -> Option<PathBuf> {
    let lib = lib?;
    let matches = lib
        .match_rom(path, platform as u16)
        .map_err(|e| println!("    match failed: {e}"))
        .ok()?;
    // Sets that match equally well (games sharing a sound engine): the one whose name
    // shares most words with the ROM's.
    let words = |p: &Path| -> std::collections::HashSet<String> {
        p.file_stem()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 1)
            .map(str::to_string)
            .collect()
    };
    let mine = words(path);
    let top = matches.first()?.1;
    let (set, share) = matches
        .iter()
        .filter(|m| m.1 >= top - 0.005)
        .max_by(|a, b| {
            let n = |m: &(PathBuf, f32)| words(&m.0).intersection(&mine).count();
            // On equal names the better match, and among those the first listed.
            n(a).cmp(&n(b)).then(a.1.total_cmp(&b.1)).then(std::cmp::Ordering::Greater)
        })?;
    // A set of the game's sound effects alone ("(SFX)") gives way to the same game's music
    // set, however much less of the ROM that one matched.
    let sfx = |p: &Path| p.to_string_lossy().to_lowercase().contains("(sfx)");
    let title = |p: &Path| {
        let mut w = words(p);
        w.retain(|w| !matches!(w.as_str(), "sfx" | "emu"));
        w
    };
    let (set, share) = match sfx(set) {
        true => matches
            .iter()
            .filter(|m| !sfx(&m.0) && title(&m.0) == title(set))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|m| (m.0.as_path(), &m.1))
            // Matched too little to be listed: still the same game's music.
            .or_else(|| lib.set_paths().find(|p| !sfx(p) && title(p) == title(set) && p.parent() == set.parent()).map(|p| (p, &0.0)))
            .unwrap_or((set.as_path(), share)),
        false => (set.as_path(), share),
    };
    if verbose {
        println!("    matched set: {} ({:.0}% matched)", set.display(), share * 100.0);
    }
    Some(set.to_path_buf())
}

/// `--layout` for a NES/Famicom ROM and its matched NSF.
fn print_layout(path: &Path, rom: &[u8], nsf: &[u8], t: &mut Tally) {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    t.games += 1;
    let Ok(h) = nes_cart::parse(rom) else { return };
    let Some(used) = nes_nsf::used_bytes(nsf, CHECK_FRAMES) else {
        println!("{name}: layout: the NSF doesn't run");
        return;
    };
    let l = music_map::layout(&nsf[0x80..], &used, &h.prg);
    let missing: usize = l.missing.iter().map(|m| m.1).sum();
    let biggest = l.missing.iter().map(|m| m.1).max().unwrap_or(0);
    let banks: std::collections::BTreeSet<usize> = l.runs.iter().map(|r| r.rom / 0x2000).collect();
    let share = l.placed as f64 * 100.0 / l.needed.max(1) as f64;
    println!(
        "{name}: layout: {:.1}% of {} needed bytes in the ROM ({} runs, {} 8 KB banks); missing {} bytes in {} stretches, biggest {}",
        share,
        l.needed,
        l.runs.len(),
        banks.len(),
        missing,
        l.missing.len(),
        biggest
    );
    if share >= 99.0 {
        t.with_music += 1;
    }
    let bucket = match share {
        s if s >= 99.0 => "99-100%",
        s if s >= 95.0 => "95-99%",
        s if s >= 80.0 => "80-95%",
        s if s >= 50.0 => "50-80%",
        _ => "under 50%",
    };
    *t.reasons.entry(format!("needed bytes in the ROM: {bucket}")).or_default() += 1;
}

/// A track the map plays this alike the NSF's own is played right.
const MAP_RIGHT: f64 = 0.9;

/// `--mapcheck` for a NES/Famicom ROM and its matched NSF.
fn print_mapcheck(path: &Path, rom: &[u8], nsf: &[u8], t: &mut Tally) {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    t.games += 1;
    let Some(prg) = nes_nsf::program(rom) else { return };
    // (A Disk System game has no board: its files load into RAM. A headerless dump's mapper
    // is unknown.)
    let disk = nes_cart::parse(rom).is_err();
    let started = std::time::Instant::now();
    let Some(m) = nes_nsf::map(nsf, &prg) else {
        println!("{name}: map: the NSF doesn't run");
        *t.reasons.entry("the NSF doesn't run".into()).or_default() += 1;
        return;
    };
    let check = nes_nsf::map_check(nsf, &m, &prg, CHECK_FRAMES, nes_nsf::Fill::Zeros);
    // Why tracks play wrong: the gaps filled with the game's own bytes, or with the file's.
    let right_with = |fill| {
        nes_nsf::map_check(nsf, &m, &prg, CHECK_FRAMES, fill)
            .iter()
            .flatten()
            .filter(|&&a| a >= MAP_RIGHT)
            .count()
    };
    let (bridged, oracle) = (right_with(nes_nsf::Fill::Bridged), right_with(nes_nsf::Fill::File(&nsf[0x80..])));
    // On the game's own board: its code where the ROM has it, the calls moved there.
    let mut m = m;
    let board = if disk {
        0
    } else {
        nes_nsf::choose_board(nsf, &mut m, rom);
        nes_nsf::cart_check(nsf, &m, rom, CHECK_FRAMES).iter().flatten().filter(|&&a| a >= MAP_RIGHT).count()
    };
    t.board += board;
    // The map as the app keeps it: each track's way chosen, written down and read back.
    nes_nsf::choose_ways(nsf, &mut m, rom, CHECK_FRAMES, MAP_RIGHT);
    let saved = nes_nsf::Map::decode(&m.encode()).map_or(0, |back| {
        nes_nsf::saved_check(nsf, &back, rom, CHECK_FRAMES).iter().flatten().filter(|&&a| a >= MAP_RIGHT).count()
    });
    t.saved += saved;
    let with_melody = check.iter().flatten().count();
    let right = check.iter().flatten().filter(|&&a| a >= MAP_RIGHT).count();
    let no_script = m.tracks.iter().filter(|s| s.is_none()).count();
    let calls = m.tracks.iter().flatten().map(|s| s.iter().filter(|o| matches!(o, nes_nsf::Op::Call { .. })).count()).max().unwrap_or(0);
    let pokes = m.tracks.iter().flatten().map(|s| s.len()).max().unwrap_or(0);
    let wrong: Vec<String> = check
        .iter()
        .enumerate()
        .filter_map(|(i, c)| c.filter(|&a| a < MAP_RIGHT).map(|a| format!("{i}:{a:.2}")))
        .take(12)
        .collect();
    println!(
        "{name}: map, {:.1} s: {right} of {with_melody} tracks played right from the ROM (gaps from the ROM: {bridged}; gaps from the file: {oracle}; on the game's board: {board} {:?}; saved map: {saved}) ({} songs; {} runs; scripts up to {} steps, {} calls; {} tracks unscripted; play {} steps{}){}",
        started.elapsed().as_secs_f32(),
        m.board,
        m.header.songs,
        m.runs.len(),
        pokes,
        calls,
        no_script,
        m.play.len(),
        match m.tried {
            Some((d, before, after)) => format!(
                "; moved {d:+} tried: {before:.1} -> {after:.1}{}",
                if m.moved.is_some() { ", taken" } else { "" }
            ),
            None => String::new(),
        },
        if wrong.is_empty() { String::new() } else { format!("; off: {}", wrong.join(" ")) }
    );
    t.checked[0] += 1;
    t.checked[1] += bridged;
    t.checked[2] += oracle;
    t.checked[3] += with_melody;
    t.checked[4] += right;
    if with_melody > 0 && right == with_melody {
        t.with_music += 1;
    }
    let bucket = match right as f64 / with_melody.max(1) as f64 {
        _ if with_melody == 0 => "no track with a melody",
        s if s >= 1.0 => "all tracks right",
        s if s >= 0.9 => "90-99% right",
        s if s >= 0.5 => "50-90% right",
        s if s > 0.0 => "under 50% right",
        _ => "none right",
    };
    *t.reasons.entry(bucket.into()).or_default() += 1;
}

/// `--mapcheck` for a Game Boy ROM and its matched GBS.
fn print_gb_mapcheck(path: &Path, rom: &[u8], gbs: &[u8], t: &mut Tally) {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    t.games += 1;
    let started = std::time::Instant::now();
    let Some(mut m) = gb_map::map(gbs, rom) else {
        println!("{name}: map: the GBS doesn't run");
        *t.reasons.entry("the GBS doesn't run".into()).or_default() += 1;
        return;
    };
    let right_with = |m: &gb_map::Map, fill| gb_map::map_check(gbs, m, rom, CHECK_FRAMES, fill).iter().flatten().filter(|&&a| a >= MAP_RIGHT).count();
    let check = gb_map::map_check(gbs, &m, rom, CHECK_FRAMES, gb_map::Fill::Zeros);
    let with_melody = check.iter().flatten().count();
    let right = check.iter().flatten().filter(|&&a| a >= MAP_RIGHT).count();
    let bridged = right_with(&m, gb_map::Fill::Bridged);
    let oracle = right_with(&m, gb_map::Fill::File(&gbs[0x70..]));
    let board = gb_map::board_check(gbs, &m, rom, CHECK_FRAMES).iter().flatten().filter(|&&a| a >= MAP_RIGHT).count();
    t.board += board;
    gb_map::choose_ways(gbs, &mut m, rom, CHECK_FRAMES, MAP_RIGHT);
    let saved = gb_map::Map::decode(&m.encode()).map_or(0, |back| {
        (0..back.header.songs)
            .filter(|&s| {
                let Some(real) = gb_gbs::track_writes(gbs, s, CHECK_FRAMES).map(|w| gb_gbs::melody(&w)) else { return false };
                let mine = gb_map::play_map(&back, rom, s, CHECK_FRAMES).map(|w| gb_gbs::melody(&w)).unwrap_or_default();
                real.len() >= music_search::MIN_MELODY && music_search::alike(&real, &mine) >= MAP_RIGHT
            })
            .count()
    });
    t.saved += saved;
    let calls = m.tracks.iter().flatten().map(|s| s.iter().filter(|o| matches!(o, gb_map::Op::Call { .. })).count()).max().unwrap_or(0);
    let wrong: Vec<String> = check.iter().enumerate().filter_map(|(i, c)| c.filter(|&a| a < MAP_RIGHT).map(|a| format!("{i}:{a:.2}"))).take(12).collect();
    println!(
        "{name}: map, {:.1} s: {right} of {with_melody} tracks played right from the ROM (gaps from the ROM: {bridged}; gaps from the file: {oracle}; on the game's board: {board}; saved map: {saved}) ({} songs; {} runs; scripts up to {calls} calls; {} tracks unscripted; play {} steps){}",
        started.elapsed().as_secs_f32(),
        m.header.songs,
        m.runs.len(),
        m.tracks.iter().filter(|s| s.is_none()).count(),
        m.play.len(),
        if wrong.is_empty() { String::new() } else { format!("; off: {}", wrong.join(" ")) }
    );
    t.checked[0] += 1;
    t.checked[1] += bridged;
    t.checked[2] += oracle;
    t.checked[3] += with_melody;
    t.checked[4] += right;
    if with_melody > 0 && right == with_melody {
        t.with_music += 1;
    }
    let bucket = match right.max(saved) as f64 / with_melody.max(1) as f64 {
        _ if with_melody == 0 => "no track with a melody",
        s if s >= 1.0 => "all tracks right",
        s if s >= 0.9 => "90-99% right",
        s if s >= 0.5 => "50-90% right",
        s if s > 0.0 => "under 50% right",
        _ => "none right",
    };
    *t.reasons.entry(bucket.into()).or_default() += 1;
}

/// `--mapcheck` for a Master System or Game Gear ROM and its matched SGC.
fn print_sgc_mapcheck(path: &Path, rom: &[u8], sgc: &[u8], t: &mut Tally) {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    t.games += 1;
    let started = std::time::Instant::now();
    let Some(mut m) = sgc_map::map(sgc, rom) else {
        println!("{name}: map: the SGC doesn't run");
        *t.reasons.entry("the SGC doesn't run".into()).or_default() += 1;
        return;
    };
    let right_with = |m: &sgc_map::Map, fill| sgc_map::map_check(sgc, m, rom, CHECK_FRAMES, fill).iter().flatten().filter(|&&a| a >= MAP_RIGHT).count();
    let check = sgc_map::map_check(sgc, &m, rom, CHECK_FRAMES, sgc_map::Fill::Zeros);
    let with_melody = check.iter().flatten().count();
    let right = check.iter().flatten().filter(|&&a| a >= MAP_RIGHT).count();
    let bridged = right_with(&m, sgc_map::Fill::Bridged);
    let oracle = right_with(&m, sgc_map::Fill::File(&sgc[0xA0..]));
    sgc_map::choose_ways(sgc, &mut m, rom, CHECK_FRAMES, MAP_RIGHT);
    let saved = sgc_map::Map::decode(&m.encode()).map_or(0, |back| {
        (0..back.header.songs)
            .filter(|&s| {
                let Some(real) = sgc_map::track_writes(sgc, s, CHECK_FRAMES).map(|w| sgc_map::melody(&w)) else { return false };
                let mine = sgc_map::play_map(&back, rom, s, CHECK_FRAMES).map(|w| sgc_map::melody(&w)).unwrap_or_default();
                real.len() >= music_search::MIN_MELODY && music_search::alike(&real, &mine) >= MAP_RIGHT
            })
            .count()
    });
    t.saved += saved;
    let calls = m.tracks.iter().flatten().map(|s| s.iter().filter(|o| matches!(o, sgc_map::Op::Call { .. })).count()).max().unwrap_or(0);
    let wrong: Vec<String> = check.iter().enumerate().filter_map(|(i, c)| c.filter(|&a| a < MAP_RIGHT).map(|a| format!("{i}:{a:.2}"))).take(12).collect();
    println!(
        "{name}: map, {:.1} s: {right} of {with_melody} tracks played right from the ROM (gaps from the ROM: {bridged}; gaps from the file: {oracle}; saved map: {saved}) ({} songs; {} runs; scripts up to {calls} calls; {} tracks unscripted; play {} steps){}",
        started.elapsed().as_secs_f32(),
        m.header.songs,
        m.runs.len(),
        m.tracks.iter().filter(|s| s.is_none()).count(),
        m.play.len(),
        if wrong.is_empty() { String::new() } else { format!("; off: {}", wrong.join(" ")) }
    );
    t.checked[0] += 1;
    t.checked[1] += bridged;
    t.checked[2] += oracle;
    t.checked[3] += with_melody;
    t.checked[4] += right;
    if with_melody > 0 && right == with_melody {
        t.with_music += 1;
    }
    let bucket = match right.max(saved) as f64 / with_melody.max(1) as f64 {
        _ if with_melody == 0 => "no track with a melody",
        s if s >= 1.0 => "all tracks right",
        s if s >= 0.9 => "90-99% right",
        s if s >= 0.5 => "50-90% right",
        s if s > 0.0 => "under 50% right",
        _ => "none right",
    };
    *t.reasons.entry(bucket.into()).or_default() += 1;
}

/// `--mapcheck` for a SNES ROM and its matched set's `.spc` files: how much of each track's
/// sound memory is in the ROM as is, and how many tracks play right rebuilt from it.
fn print_spc_mapcheck(path: &Path, rom: &[u8], set: &Path, t: &mut Tally) {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    t.games += 1;
    let started = std::time::Instant::now();
    let rom = snes_sound::rom_image(rom);
    let spcs = rip_sets::all_with_magic(set, b"SNES-SPC700");
    if spcs.is_empty() {
        println!("{name}: map: no .spc in the set");
        *t.reasons.entry("no .spc in the set".into()).or_default() += 1;
        return;
    }
    let refs: Vec<&[u8]> = spcs.iter().map(Vec::as_slice).collect();
    let maps = spc_map::tracks(&refs, rom);
    let (mut got, mut total) = (0usize, 0usize);
    let (mut with_melody, mut zeros, mut bridged, mut oracle, mut best) = (0, 0, 0, 0, 0);
    let mut off = Vec::new();
    for (i, (spc, m)) in spcs.iter().zip(&maps).enumerate() {
        let Some(m) = m else { continue };
        let (g, n) = spc_map::coverage(spc, &m.runs);
        got += g;
        total += n;
        let check = |fill| spc_map::check(spc, m, rom, fill, CHECK_FRAMES);
        let Some(z) = check(spc_map::Fill::Zeros) else { continue };
        with_melody += 1;
        let b = check(spc_map::Fill::Bridged).unwrap_or(0.0);
        let f = check(spc_map::Fill::File(spc)).unwrap_or(0.0);
        zeros += (z >= MAP_RIGHT) as usize;
        bridged += (b >= MAP_RIGHT) as usize;
        oracle += (f >= MAP_RIGHT) as usize;
        best += (z.max(b) >= MAP_RIGHT) as usize;
        if z.max(b) < MAP_RIGHT && off.len() < 12 {
            off.push(format!("{i}:{:.2}/{:.0}%", z.max(b), g as f64 * 100.0 / n.max(1) as f64));
        }
    }
    let share = got as f64 * 100.0 / total.max(1) as f64;
    t.saved += best;
    println!(
        "{name}: map, {:.1} s: {best} of {with_melody} tracks played right from the ROM (as-is: {zeros}; gaps from the ROM: {bridged}; gaps from the file: {oracle}) ({} .spc; {share:.1}% of sound memory in the ROM){}",
        started.elapsed().as_secs_f32(),
        spcs.len(),
        if off.is_empty() { String::new() } else { format!("; off: {}", off.join(" ")) }
    );
    t.checked[0] += 1;
    t.checked[1] += bridged;
    t.checked[2] += oracle;
    t.checked[3] += with_melody;
    t.checked[4] += zeros;
    if with_melody > 0 && best == with_melody {
        t.with_music += 1;
    }
    let bucket = match best as f64 / with_melody.max(1) as f64 {
        _ if with_melody == 0 => "no track with a melody",
        s if s >= 1.0 => "all tracks right",
        s if s >= 0.9 => "90-99% right",
        s if s >= 0.5 => "50-90% right",
        s if s > 0.0 => "under 50% right",
        _ => "none right",
    };
    *t.reasons.entry(bucket.into()).or_default() += 1;
    let cover = match share {
        s if s >= 99.0 => "sound memory in the ROM: 99-100%",
        s if s >= 90.0 => "sound memory in the ROM: 90-99%",
        s if s >= 50.0 => "sound memory in the ROM: 50-90%",
        _ => "sound memory in the ROM: under 50%",
    };
    *t.reasons.entry(cover.into()).or_default() += 1;
}

/// `--spcprobe` for a SNES ROM and its matched set: after the game's own boot (Start pressed
/// now and then), each value 0-127 written to each of the four ports, and the tracks of the
/// set whose melody the chip then plays.
fn print_spc_probe(path: &Path, rom: &[u8], set: &Path, seconds: u32, t: &mut Tally) {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    t.games += 1;
    let started = std::time::Instant::now();
    let spcs = rip_sets::all_with_magic(set, b"SNES-SPC700");
    let real: Vec<std::collections::HashSet<u64>> = spcs
        .iter()
        .map(|s| spc_map::frames(s, CHECK_FRAMES).map(|f| spc_map::melody(&f)).unwrap_or_default())
        .filter(|m| m.len() >= music_search::MIN_MELODY)
        .collect();
    if real.is_empty() {
        println!("{name}: probe: no .spc with a melody in the set");
        *t.reasons.entry("no .spc with a melody".into()).or_default() += 1;
        return;
    }
    let options = romexplorer::rom::snes_capture::Options { seconds: seconds as f32, keep_audio: false, press_start: true, watch_register: None };
    let capture = match romexplorer::rom::snes_capture::capture(rom, &options) {
        Ok(c) if !c.driver_starts.is_empty() => c,
        Ok(_) => {
            println!("{name}: probe: no sound driver started");
            *t.reasons.entry("no sound driver started".into()).or_default() += 1;
            return;
        }
        Err(e) => {
            println!("{name}: probe: {e}");
            *t.reasons.entry("the boot failed".into()).or_default() += 1;
            return;
        }
    };
    let boot_s = started.elapsed().as_secs_f32();
    // Which track each (port, value) starts, best match only.
    let mut right = vec![None::<(usize, u8)>; real.len()];
    let at_boot = spc_map::melody(&spc_map::frames(&capture.spc, CHECK_FRAMES).unwrap_or_default());
    let booted = real.iter().filter(|r| music_search::alike(r, &at_boot) >= MAP_RIGHT).count();
    for port in 0..4 {
        for value in 0..=0x7Fu8 {
            let m = spc_map::probe(&capture.spc, port, value, CHECK_FRAMES);
            if m.len() < music_search::MIN_MELODY {
                continue;
            }
            for (i, r) in real.iter().enumerate() {
                if right[i].is_none() && music_search::alike(r, &m) >= MAP_RIGHT {
                    right[i] = Some((port, value));
                }
            }
        }
    }
    let found = right.iter().flatten().count();
    let mut by_port = [0usize; 4];
    right.iter().flatten().for_each(|(p, _)| by_port[*p] += 1);
    println!(
        "{name}: probe, {:.1} s (boot {boot_s:.1} s): {found} of {} tracks started by a port write (by port 0-3: {by_port:?}); playing at boot: {booted}",
        started.elapsed().as_secs_f32(),
        real.len()
    );
    t.checked[0] += 1;
    t.checked[3] += real.len();
    t.checked[4] += found;
    t.saved += found;
    let bucket = match found as f64 / real.len() as f64 {
        s if s >= 1.0 => "all tracks started",
        s if s >= 0.9 => "90-99% started",
        s if s >= 0.5 => "50-90% started",
        s if s > 0.0 => "under 50% started",
        _ => "none started",
    };
    *t.reasons.entry(bucket.into()).or_default() += 1;
}

/// `--spcbank` for a SNES ROM and its matched set: the boot's sound chip with each track's
/// banks laid in from the ROM, then the track's port command.
fn print_spc_bank(path: &Path, file: &[u8], set: &Path, t: &mut Tally) {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    t.games += 1;
    let started = std::time::Instant::now();
    let spcs = rip_sets::all_with_magic(set, b"SNES-SPC700");
    let options = romexplorer::rom::snes_capture::Options { seconds: 30.0, keep_audio: false, press_start: true, watch_register: None };
    let boot = match romexplorer::rom::snes_capture::capture(file, &options) {
        Ok(c) if !c.driver_starts.is_empty() => c.spc,
        Ok(_) => {
            println!("{name}: bank: no sound driver started");
            *t.reasons.entry("no sound driver started".into()).or_default() += 1;
            return;
        }
        Err(e) => {
            println!("{name}: bank: {e}");
            *t.reasons.entry("the boot failed".into()).or_default() += 1;
            return;
        }
    };
    let rom = snes_sound::rom_image(file);
    let refs: Vec<&[u8]> = spcs.iter().map(Vec::as_slice).collect();
    let maps = spc_map::tracks(&refs, rom);
    let (mut tracks, mut heard, mut by) = (0usize, 0usize, [0usize; 3]);
    for (spc, m) in spcs.iter().zip(&maps) {
        let Some(m) = m else { continue };
        let real = spc_map::melody(&spc_map::frames(spc, CHECK_FRAMES).unwrap_or_default());
        if real.len() < music_search::MIN_MELODY {
            continue;
        }
        tracks += 1;
        let state = spc_map::with_bank(&boot, spc, &m.runs, rom);
        let ports = spc_map::ports_of(spc);
        // The command as the four ports, then each port alone, then the bank alone.
        let all: Vec<(usize, u8)> = ports.iter().enumerate().map(|(p, &v)| (p, v)).collect();
        let tries: Vec<Vec<(usize, u8)>> = std::iter::once(all.clone()).chain((0..4).map(|p| vec![(p, ports[p])])).chain(std::iter::once(Vec::new())).collect();
        for (k, w) in tries.iter().enumerate() {
            if music_search::alike(&real, &spc_map::command(&state, w, CHECK_FRAMES)) >= MAP_RIGHT {
                heard += 1;
                by[match k { 0 => 0, 5 => 2, _ => 1 }] += 1;
                break;
            }
        }
    }
    println!(
        "{name}: bank, {:.1} s: {heard} of {tracks} tracks heard (by all four ports {}, one port {}, the bank alone {})",
        started.elapsed().as_secs_f32(),
        by[0],
        by[1],
        by[2]
    );
    t.checked[0] += 1;
    t.checked[3] += tracks;
    t.checked[4] += heard;
    t.saved += heard;
    let bucket = match heard as f64 / tracks.max(1) as f64 {
        _ if tracks == 0 => "no track with a melody",
        s if s >= 1.0 => "all tracks heard",
        s if s >= 0.9 => "90-99% heard",
        s if s >= 0.5 => "50-90% heard",
        s if s > 0.0 => "under 50% heard",
        _ => "none heard",
    };
    *t.reasons.entry(bucket.into()).or_default() += 1;
}

/// `--spcpoke`: each value 0-127 through `var`, and the track of the set each one plays.
fn print_spc_poke(path: &Path, rom: &[u8], set: &Path, var: u32) {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let started = std::time::Instant::now();
    let real: Vec<std::collections::HashSet<u64>> = rip_sets::all_with_magic(set, b"SNES-SPC700")
        .iter()
        .map(|s| spc_map::frames(s, CHECK_FRAMES).map(|f| spc_map::melody(&f)).unwrap_or_default())
        .collect();
    match romexplorer::rom::snes_songs::try_var(rom, var, 0..=0x7Fu8, CHECK_FRAMES) {
        Err(e) => println!("{name}: poke: {e}"),
        Ok((same, songs)) => {
            println!("{name}: poke ${var:05X}, {:.1} s: after a restore the game sounds {same:.2} alike to straight on", started.elapsed().as_secs_f32());
            let mut tracks = std::collections::BTreeSet::new();
            for (v, m) in &songs {
                let best = real.iter().enumerate().map(|(i, r)| (i, music_search::alike(r, m))).max_by(|a, b| a.1.total_cmp(&b.1));
                if let Some((i, a)) = best {
                    if a >= MAP_RIGHT {
                        tracks.insert(i);
                    }
                    println!("  {v:02X}: melody {} notes; best track {i} at {a:.2}", m.len());
                }
            }
            println!("  {} of {} tracks heard", tracks.len(), real.len());
        }
    }
}

/// `--spcsearch` for a SNES ROM and its matched set (see [`snes_songs`]).
fn print_spc_search(path: &Path, rom: &[u8], set: &Path, t: &mut Tally) {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    t.games += 1;
    let started = std::time::Instant::now();
    let real: Vec<std::collections::HashSet<u64>> = rip_sets::all_with_magic(set, b"SNES-SPC700")
        .iter()
        .map(|s| spc_map::frames(s, CHECK_FRAMES).map(|f| spc_map::melody(&f)).unwrap_or_default())
        .filter(|m| m.len() >= music_search::MIN_MELODY)
        .collect();
    if real.is_empty() {
        println!("{name}: search: no .spc with a melody in the set");
        *t.reasons.entry("no .spc with a melody".into()).or_default() += 1;
        return;
    }
    let track_of = |m: &std::collections::HashSet<u64>| {
        real.iter()
            .enumerate()
            .map(|(i, r)| (i, music_search::alike(r, m)))
            .filter(|&(_, a)| a >= MAP_RIGHT)
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    };
    let found = match romexplorer::rom::snes_songs::find(rom, 0..=0x7Fu8, CHECK_FRAMES, &track_of) {
        Ok(f) => f,
        Err(e) => {
            println!("{name}: search, {:.1} s: {e}", started.elapsed().as_secs_f32());
            *t.reasons.entry(e).or_default() += 1;
            return;
        }
    };
    let mut right = vec![None::<u8>; real.len()];
    for (v, m) in &found.songs {
        for (i, r) in real.iter().enumerate() {
            if right[i].is_none() && music_search::alike(r, m) >= MAP_RIGHT {
                right[i] = Some(*v);
            }
        }
    }
    let n = right.iter().flatten().count();
    let mut values: Vec<u8> = right.iter().flatten().copied().collect();
    values.sort_unstable();
    values.dedup();
    println!(
        "{name}: search, {:.1} s: {n} of {} tracks by the request at ${:05X} (candidate {}; {} values with a melody; values heard right: {})",
        started.elapsed().as_secs_f32(),
        real.len(),
        found.var,
        found.tried,
        found.songs.len(),
        values.iter().map(|v| format!("{v:02X}")).collect::<Vec<_>>().join(" ")
    );
    t.checked[0] += 1;
    t.checked[3] += real.len();
    t.checked[4] += n;
    t.saved += n;
    let bucket = match n as f64 / real.len() as f64 {
        s if s >= 1.0 => "all tracks heard",
        s if s >= 0.9 => "90-99% heard",
        s if s >= 0.5 => "50-90% heard",
        s if s > 0.0 => "under 50% heard",
        _ => "none heard",
    };
    *t.reasons.entry(bucket.into()).or_default() += 1;
}

/// `--mapcheck` for a PC Engine HuCard and its matched HES.
fn print_hes_mapcheck(path: &Path, rom: &[u8], hes: &[u8], t: &mut Tally) {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    t.games += 1;
    let started = std::time::Instant::now();
    let rom = hes_map::program(rom);
    let Some(mut m) = hes_map::map(hes, rom) else {
        println!("{name}: map: the HES doesn't run");
        *t.reasons.entry("the HES doesn't run".into()).or_default() += 1;
        return;
    };
    let right_with = |m: &hes_map::Map, fill| hes_map::map_check(hes, m, rom, CHECK_FRAMES, fill).iter().flatten().filter(|&&a| a >= MAP_RIGHT).count();
    let check = hes_map::map_check(hes, &m, rom, CHECK_FRAMES, hes_map::Fill::Zeros);
    let with_melody = check.iter().flatten().count();
    let right = check.iter().flatten().filter(|&&a| a >= MAP_RIGHT).count();
    let bridged = right_with(&m, hes_map::Fill::Bridged);
    let oracle = right_with(&m, hes_map::Fill::File(&hes[0x20..]));
    let board = right_with(&m, hes_map::Fill::Board);
    t.board += board;
    hes_map::choose_ways(hes, &mut m, rom, CHECK_FRAMES, MAP_RIGHT);
    let saved = hes_map::Map::decode(&m.encode()).map_or(0, |back| {
        (0..back.header.songs)
            .filter(|&s| {
                if back.tracks.get(s as usize).is_none_or(|t| t.is_none()) {
                    return false;
                }
                let Some(real) = hes_map::track_writes(hes, s, CHECK_FRAMES).map(|w| hes_map::melody(&w)) else { return false };
                let mine = hes_map::play_map(&back, rom, s, CHECK_FRAMES).map(|w| hes_map::melody(&w)).unwrap_or_default();
                real.len() >= music_search::MIN_MELODY && music_search::alike(&real, &mine) >= MAP_RIGHT
            })
            .count()
    });
    t.saved += saved;
    let calls = m
        .tracks
        .iter()
        .flatten()
        .map(|s| [&s.init, &s.vblank, &s.timer].iter().map(|v| v.iter().filter(|o| matches!(o, hes_map::Op::Call { .. })).count()).sum::<usize>())
        .max()
        .unwrap_or(0);
    let wrong: Vec<String> = check.iter().enumerate().filter_map(|(i, c)| c.filter(|&a| a < MAP_RIGHT).map(|a| format!("{i}:{a:.2}"))).take(12).collect();
    println!(
        "{name}: map, {:.1} s: {right} of {with_melody} tracks played right from the ROM (gaps from the ROM: {bridged}; gaps from the file: {oracle}; on the game's banks: {board}; saved map: {saved}) ({} runs; scripts up to {calls} calls){}",
        started.elapsed().as_secs_f32(),
        m.runs.len(),
        if wrong.is_empty() { String::new() } else { format!("; off: {}", wrong.join(" ")) }
    );
    t.checked[0] += 1;
    t.checked[1] += bridged;
    t.checked[2] += oracle;
    t.checked[3] += with_melody;
    t.checked[4] += right;
    if with_melody > 0 && right == with_melody {
        t.with_music += 1;
    }
    let bucket = match right.max(saved) as f64 / with_melody.max(1) as f64 {
        _ if with_melody == 0 => "no track with a melody",
        s if s >= 1.0 => "all tracks right",
        s if s >= 0.9 => "90-99% right",
        s if s >= 0.5 => "50-90% right",
        s if s > 0.0 => "under 50% right",
        _ => "none right",
    };
    *t.reasons.entry(bucket.into()).or_default() += 1;
}

fn run_hinted<M: Machine>(
    path: &Path,
    rom: &[u8],
    what: &str,
    hint: Option<&music_search::Hint>,
    answers: Option<&Answers>,
    o: &Options,
    t: &mut Tally,
) {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    t.games += 1;
    if let Some(secs) = o.probe {
        let started = std::time::Instant::now();
        let mut parts = Vec::new();
        let mut any = false;
        for (input, held) in music_search::INPUTS {
            match music_search::probe::<M>(rom, *held, secs * 60) {
                Ok(Some(f)) => {
                    any = true;
                    parts.push(format!("{input}:{:.1}s", f as f32 / 60.0));
                }
                Ok(None) => parts.push(format!("{input}:-")),
                Err(e) => {
                    println!("{name}: {what}: {e}");
                    return;
                }
            }
        }
        if any {
            t.with_music += 1;
        }
        println!(
            "{name}: {what}, {:.1} s: probe {}",
            started.elapsed().as_secs_f32(),
            parts.join(" ")
        );
        return;
    }
    let started = std::time::Instant::now();
    let mut notes = Vec::new();
    let found = music_search::search_hinted::<M>(rom, hint, &mut |n| notes.push(n));
    let what = if hint.is_some() { format!("{what}, NSF") } else { what.to_string() };
    let took = started.elapsed().as_secs_f32();
    match found {
        Ok(f) if !f.songs.is_empty() => {
            t.with_music += 1;
            t.songs += f.songs.len();
            println!(
                "{name}: {what}, {took:.1} s: {} songs {:?} [{}]",
                f.songs.len(),
                f.songs,
                f.recipe.encode()
            );
            // Each song against the known answer: the track whose melody it shares.
            if let Some(Answers { tracks: answers, melody }) = answers.filter(|a| !a.tracks.is_empty()) {
                let mut found_tracks = std::collections::BTreeSet::new();
                let mut like = 0;
                let mut lines = Vec::new();
                for &v in &f.songs {
                    let Ok(mut song) = Song::<M>::open(rom, &f.recipe, v, 8000) else {
                        continue;
                    };
                    let mut pcm = Vec::new();
                    let frames: Vec<_> = (0..CHECK_FRAMES).map_while(|_| song.frame(&mut pcm).ok()).collect();
                    pcm.clear();
                    let mine = melody(&frames);
                    let best = answers
                        .iter()
                        .enumerate()
                        .map(|(i, a)| (i, music_search::alike(&mine, a)))
                        .max_by(|a, b| a.1.total_cmp(&b.1));
                    if let Some((track, score)) = best {
                        if score >= CHECK_ALIKE {
                            like += 1;
                            found_tracks.insert(track);
                        }
                        lines.push(format!("{v}->{track}:{score:.2}"));
                    }
                }
                let unchecked = f.checked.is_some_and(|c| c.0 == 0);
                println!(
                    "    check: {like} of {} songs are like a track; {} of {} tracks found{}",
                    f.songs.len(),
                    found_tracks.len(),
                    answers.len(),
                    if unchecked { " (unchecked: no song like a track in the search)" } else { "" }
                );
                if unchecked {
                    t.unchecked[0] += 1;
                    t.unchecked[1] += f.songs.len();
                }
                if o.verbose {
                    println!("    check songs: {}", lines.join(" "));
                }
                t.checked[0] += 1;
                t.checked[1] += f.songs.len();
                t.checked[2] += like;
                t.checked[3] += answers.len();
                t.checked[4] += found_tracks.len();
            }
            if let Some(dir) = &o.render {
                let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
                let dir = dir.join(&stem);
                let _ = std::fs::create_dir_all(&dir);
                for (i, &v) in f.songs.iter().enumerate() {
                    let timing = music_search::measure::<M>(rom, &f.recipe, v);
                    let Ok(mut song) = Song::<M>::open(rom, &f.recipe, v, 44_100) else {
                        continue;
                    };
                    let frames = (o.seconds as f64 * song.frame_rate()) as usize;
                    let mut pcm = Vec::new();
                    for _ in 0..frames {
                        if song.frame(&mut pcm).is_err() {
                            break;
                        }
                    }
                    let _ = wav(
                        &dir.join(format!("{:02} - song {v}.wav", i + 1)),
                        &pcm,
                        44_100,
                        M::CHANNELS,
                    );
                    if o.verbose {
                        println!("    song {v}: {timing:?}");
                    }
                }
            }
        }
        Ok(f) => {
            let why = f.why.unwrap_or_default();
            println!("{name}: {what}, {took:.1} s: no music: {why}");
            *t.reasons.entry(why).or_default() += 1;
        }
        Err(e) => {
            println!("{name}: {what}: {e}");
            let key = if e.contains("supported yet") {
                "cartridge board not supported yet".to_string()
            } else {
                e
            };
            *t.reasons.entry(key).or_default() += 1;
        }
    }
    if o.verbose {
        for n in notes {
            println!("    {n}");
        }
    }
}

/// `--write-maps`: the music map of `path`, from its matched set, built and kept as the app
/// keeps it (under `dir`).
fn write_map(
    dir: &Path,
    path: &Path,
    rom: &[u8],
    lib: Option<&rip_match::Library>,
    verbose: bool,
    tally: &mut std::collections::BTreeMap<&str, Tally>,
) {
    let (platform, what) = match console(rom, path) {
        Some("NES") | Some("NES?") => (Platform::Famicom, "NES"),
        Some("FDS") => (Platform::Famicom, "Disk System"),
        Some("GB") => (Platform::GameBoy, "Game Boy"),
        Some("GBC") => (Platform::GameBoyColor, "Game Boy Color"),
        Some("PCE") => (Platform::TurboGrafx16, "PC Engine"),
        Some("SMS") => (Platform::SegaMasterSystem, "Master System"),
        Some("GG") => (Platform::SegaGameGear, "Game Gear"),
        _ => return,
    };
    let t = tally.entry(what).or_default();
    t.games += 1;
    let Some(set) = matched_set(lib, path, platform, verbose) else {
        *t.reasons.entry("no matching set".into()).or_default() += 1;
        return;
    };
    rom_songs::set_maps_dir(dir.to_path_buf());
    let before = count_maps(dir);
    match rom_songs::map_from_sets(path, &[set]) {
        Ok(()) if count_maps(dir) > before => {
            t.with_music += 1;
            println!("{}: map kept", path.display());
        }
        Ok(()) => {
            *t.reasons.entry("no map (nothing played, or kept already)".into()).or_default() += 1;
            println!("{}: no map", path.display());
        }
        Err(e) => {
            *t.reasons.entry(e.clone()).or_default() += 1;
            println!("{}: {e}", path.display());
        }
    }
}

fn count_maps(dir: &Path) -> usize {
    ["nes", "gb", "hes", "sgc"].iter().map(|k| std::fs::read_dir(dir.join(k)).map_or(0, |d| d.count())).sum()
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut o = Options {
        render: None,
        seconds: 20,
        verbose: args.iter().any(|a| a == "--verbose"),
        probe: None,
        nsf: None,
        layout: args.iter().any(|a| a == "--layout"),
        mapcheck: args.iter().any(|a| a == "--mapcheck"),
        write_maps: None,
        spcprobe: args.iter().any(|a| a == "--spcprobe"),
        spcsearch: args.iter().any(|a| a == "--spcsearch"),
        spcpoke: None,
        spcbank: args.iter().any(|a| a == "--spcbank"),
    };
    args.retain(|a| a != "--spcbank");
    if let Some(i) = args.iter().position(|a| a == "--spcpoke") {
        o.spcpoke = args.get(i + 1).and_then(|a| u32::from_str_radix(a.trim_start_matches('$'), 16).ok());
        args.drain(i..(i + 2).min(args.len()));
    }
    args.retain(|a| a != "--spcprobe" && a != "--spcsearch");
    // `--pack-maps <dir> <out>`: every map kept under <dir> into the one bundle the app and
    // rommusic ship (assets/music_maps.zst).
    if let Some(i) = args.iter().position(|a| a == "--pack-maps") {
        let (Some(dir), Some(out)) = (args.get(i + 1), args.get(i + 2)) else {
            eprintln!("usage: cart_music --pack-maps <maps folder> <bundle file>");
            std::process::exit(2);
        };
        match rom_songs::pack_bundle(Path::new(dir)) {
            Ok((packed, counts)) => {
                if let Err(e) = std::fs::write(out, &packed) {
                    eprintln!("{out}: {e}");
                    std::process::exit(1);
                }
                for (kind, n) in counts {
                    println!("{kind}: {n} maps");
                }
                println!("{out}: {} bytes", packed.len());
                return;
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }
    if let Some(i) = args.iter().position(|a| a == "--write-maps") {
        o.write_maps = args.get(i + 1).map(PathBuf::from);
        args.drain(i..(i + 2).min(args.len()));
    }
    args.retain(|a| a != "--layout" && a != "--mapcheck");
    args.retain(|a| a != "--verbose");
    if let Some(i) = args.iter().position(|a| a == "--render") {
        o.render = args.get(i + 1).map(PathBuf::from);
        args.drain(i..(i + 2).min(args.len()));
    }
    if let Some(i) = args.iter().position(|a| a == "--probe") {
        o.probe = Some(args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(60));
        args.drain(i..(i + 2).min(args.len()));
    }
    if let Some(i) = args.iter().position(|a| a == "--originals" || a == "--nsf") {
        o.nsf = args.get(i + 1).map(PathBuf::from);
        args.drain(i..(i + 2).min(args.len()));
    }
    if let Some(i) = args.iter().position(|a| a == "--seconds") {
        o.seconds = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(20);
        args.drain(i..(i + 2).min(args.len()));
    }
    if args.is_empty() {
        eprintln!(
            "usage: cart_music [--render <dir>] [--seconds N] [--verbose] [--probe SECONDS] [--originals <folder> [--layout | --mapcheck | --write-maps <dir>]] <rom or folder>...\n       cart_music --pack-maps <maps folder> <bundle file>"
        );
        std::process::exit(2);
    }
    if !romexplorer::rom::nes_apu::available()
        || !romexplorer::rom::gb_apu::available()
        || !romexplorer::rom::vgm_chips::available()
    {
        eprintln!("the rommusic library (sound chips) was not found: set ROMEXPLORER_EXE_ROOT or put it in lib/ beside this tool");
        std::process::exit(1);
    }
    let mut files = Vec::new();
    for a in &args {
        collect(Path::new(a), &mut files);
    }
    let nsf_lib = o.nsf.as_ref().map(|dir| {
        let cache = std::env::temp_dir().join("cart_music_original_fingerprints.json");
        let lib = rip_match::Library::build(&[dir.clone()], Some(&cache), |_, _| {});
        eprintln!("{} sets read from {}", lib.len(), dir.display());
        lib
    });
    let mut tally: std::collections::BTreeMap<&str, Tally> = Default::default();
    for path in files {
        let rom = match rom_songs::read_rom(&path) {
            Ok(r) => r,
            Err(e) => {
                println!("{}: unreadable: {e}", path.display());
                let t = tally.entry("Not recognised").or_default();
                t.games += 1;
                *t.reasons.entry("unreadable (no ROM inside, or a broken archive)".into()).or_default() += 1;
                continue;
            }
        };
        if let Some(dir) = &o.write_maps {
            write_map(dir, &path, &rom, nsf_lib.as_ref(), o.verbose, &mut tally);
            continue;
        }
        match console(&rom, &path) {
            Some("NES") => {
                let what = format!(
                    "mapper {}",
                    nes_cart::parse(&rom).map(|h| h.mapper).unwrap_or(0)
                );
                let hint = original_hint(nsf_lib.as_ref(), &path, Platform::Famicom, b"NESM\x1a", nes_nsf::hint, o.verbose);
                if o.layout || o.mapcheck {
                    if let Some(h) = &hint {
                        let t = tally.entry("NES").or_default();
                        if o.layout {
                            print_layout(&path, &rom, &h.1, t);
                        } else {
                            print_mapcheck(&path, &rom, &h.1, t);
                        }
                    }
                    continue;
                }
                let answers = hint.as_ref().map(|h| answers(&h.1, 6, nes_nsf::track_writes, nes_nsf::melody));
                run_hinted::<Nes>(&path, &rom, &what, hint.as_ref().map(|h| &h.0), answers.as_ref(), &o, tally.entry("NES").or_default());
            }
            Some("SMS") => {
                if o.mapcheck {
                    let sgc = original_hint(nsf_lib.as_ref(), &path, Platform::SegaMasterSystem, b"SGC\x1a", |_| Some(Default::default()), o.verbose);
                    if let Some(h) = &sgc {
                        print_sgc_mapcheck(&path, &rom, &h.1, tally.entry("Master System").or_default());
                    }
                    continue;
                }
                run::<MasterSystem>(&path, &rom, "Master System", &o, tally.entry("Master System").or_default())
            }
            Some("MD") => run::<MegaDrive>(
                &path,
                &rom,
                "Mega Drive",
                &o,
                tally.entry("Mega Drive").or_default(),
            ),
            Some("FDS") => {
                let hint = original_hint(nsf_lib.as_ref(), &path, Platform::Famicom, b"NESM\x1a", nes_nsf::hint, o.verbose);
                if o.mapcheck {
                    if let Some(h) = &hint {
                        print_mapcheck(&path, &rom, &h.1, tally.entry("Disk System").or_default());
                    }
                    continue;
                }
                let answers = hint.as_ref().map(|h| answers(&h.1, 6, nes_nsf::track_writes, nes_nsf::melody));
                run_hinted::<Nes>(
                    &path,
                    &rom,
                    "Disk System",
                    hint.as_ref().map(|h| &h.0),
                    answers.as_ref(),
                    &o,
                    tally.entry("Disk System").or_default(),
                );
                // BIOS routines the stand-in doesn't know, met in the first 15 seconds.
                if let Ok(mut m) = Nes::boot(&rom) {
                    for _ in 0..900 {
                        m.run_frame(None, None);
                        m.take_sound();
                    }
                    let unknown = m.unknown_bios_calls();
                    if !unknown.is_empty() {
                        let list: Vec<String> =
                            unknown.iter().map(|a| format!("${a:04X}")).collect();
                        println!("    BIOS calls not handled: {}", list.join(" "));
                    }
                    // Where it stands after 15 seconds, and what it read of the drive.
                    let pcs: Vec<String> = (0..2)
                        .filter_map(|_| {
                            m.run_frame(None, None);
                            m.take_sound();
                            m.disk_status()
                        })
                        .collect();
                    println!("    disk after 15 s: {}", pcs.join(" | "));
                }
            }
            Some("MSX") => {
                run::<Msx>(&path, &rom, "MSX", &o, tally.entry("MSX").or_default());
                // Stand-in BIOS routines the game reached that it doesn't know.
                if let Ok(mut m) = Msx::boot(&rom) {
                    for _ in 0..900 {
                        m.run_frame(None, None);
                        m.take_sound();
                    }
                    let unknown = m.unknown_bios_calls();
                    if !unknown.is_empty() {
                        let list: Vec<String> =
                            unknown.iter().map(|a| format!("${a:04X}")).collect();
                        println!("    BIOS calls not handled: {}", list.join(" "));
                    }
                }
            }
            Some(c @ ("WS" | "WSC")) => {
                let what = if c == "WS" { "WonderSwan" } else { "WonderSwan Color" };
                run::<WonderSwan>(&path, &rom, what, &o, tally.entry(what).or_default())
            }
            Some(c @ ("NGP" | "NGPC")) => {
                let what = if c == "NGP" { "Neo Geo Pocket" } else { "Neo Geo Pocket Color" };
                run::<NeoGeoPocket>(&path, &rom, what, &o, tally.entry(what).or_default());
                // BIOS calls the stand-in doesn't answer, met in the first 15 seconds.
                if let Ok(mut m) = NeoGeoPocket::boot(&rom) {
                    for _ in 0..900 {
                        m.run_frame(None, None);
                        m.take_sound();
                    }
                    let unknown = m.unknown_bios_calls();
                    if !unknown.is_empty() {
                        let list: Vec<String> =
                            unknown.iter().map(|a| format!("${a:06X}")).collect();
                        println!("    BIOS calls not handled: {}", list.join(" "));
                    }
                }
            }
            Some("GBA") => run::<GameBoyAdvance>(
                &path,
                &rom,
                "Game Boy Advance",
                &o,
                tally.entry("Game Boy Advance").or_default(),
            ),
            Some("VB") => run::<VirtualBoy>(
                &path,
                &rom,
                "Virtual Boy",
                &o,
                tally.entry("Virtual Boy").or_default(),
            ),
            Some("SNES") => {
                if o.spcbank {
                    if let Some(set) = matched_set(nsf_lib.as_ref(), &path, Platform::Snes, o.verbose) {
                        print_spc_bank(&path, &rom, &set, tally.entry("SNES").or_default());
                    }
                } else if let Some(var) = o.spcpoke {
                    if let Some(set) = matched_set(nsf_lib.as_ref(), &path, Platform::Snes, o.verbose) {
                        print_spc_poke(&path, &rom, &set, var);
                    }
                } else if o.spcsearch {
                    if let Some(set) = matched_set(nsf_lib.as_ref(), &path, Platform::Snes, o.verbose) {
                        print_spc_search(&path, &rom, &set, tally.entry("SNES").or_default());
                    }
                } else if o.spcprobe {
                    if let Some(set) = matched_set(nsf_lib.as_ref(), &path, Platform::Snes, o.verbose) {
                        print_spc_probe(&path, &rom, &set, o.seconds, tally.entry("SNES").or_default());
                    }
                } else if o.mapcheck {
                    if let Some(set) = matched_set(nsf_lib.as_ref(), &path, Platform::Snes, o.verbose) {
                        print_spc_mapcheck(&path, &rom, &set, tally.entry("SNES").or_default());
                    }
                } else {
                    println!("{}: SNES: only --mapcheck", path.display());
                }
            }
            Some("PCE") if o.mapcheck => {
                let hes = original_hint(nsf_lib.as_ref(), &path, Platform::TurboGrafx16, b"HESM", |_| Some(Default::default()), o.verbose);
                if let Some(h) = &hes {
                    print_hes_mapcheck(&path, &rom, &h.1, tally.entry("PC Engine").or_default());
                }
            }
            Some("PCE") => run::<PcEngine>(
                &path,
                &rom,
                "PC Engine",
                &o,
                tally.entry("PC Engine").or_default(),
            ),
            Some("NES?") if o.mapcheck => {
                let hint = original_hint(nsf_lib.as_ref(), &path, Platform::Famicom, b"NESM\x1a", nes_nsf::hint, o.verbose);
                if let Some(h) = &hint {
                    print_mapcheck(&path, &rom, &h.1, tally.entry("NES (headerless)").or_default());
                }
            }
            Some("NES?") => {
                println!(
                    "{}: no iNES header (headerless dump): skipped",
                    path.display()
                );
                *tally
                    .entry("NES")
                    .or_default()
                    .reasons
                    .entry("no iNES header (headerless dump)".into())
                    .or_default() += 1;
                tally.entry("NES").or_default().games += 1;
            }
            Some("GG") => {
                if o.mapcheck {
                    let sgc = original_hint(nsf_lib.as_ref(), &path, Platform::SegaGameGear, b"SGC\x1a", |_| Some(Default::default()), o.verbose);
                    if let Some(h) = &sgc {
                        print_sgc_mapcheck(&path, &rom, &h.1, tally.entry("Game Gear").or_default());
                    }
                    continue;
                }
                run::<GameGear>(&path, &rom, "Game Gear", &o, tally.entry("Game Gear").or_default())
            }
            Some(c) => {
                let what = format!("{c}, cartridge ${:02X}", rom[0x147]);
                let console = if c == "GBC" { "Game Boy Color" } else { "Game Boy" };
                let platform = if c == "GBC" { Platform::GameBoyColor } else { Platform::GameBoy };
                let hint = original_hint(nsf_lib.as_ref(), &path, platform, b"GBS", gb_gbs::hint, o.verbose);
                if o.mapcheck {
                    if let Some(h) = &hint {
                        print_gb_mapcheck(&path, &rom, &h.1, tally.entry(console).or_default());
                    }
                    continue;
                }
                let answers = hint.as_ref().map(|h| answers(&h.1, 4, gb_gbs::track_writes, gb_gbs::melody));
                run_hinted::<GameBoy>(&path, &rom, &what, hint.as_ref().map(|h| &h.0), answers.as_ref(), &o, tally.entry(console).or_default());
            }
            None => {
                println!("{}: console not recognised: skipped", path.display());
                *tally
                    .entry("Not recognised")
                    .or_default()
                    .reasons
                    .entry("console not recognised from the header or file name".into())
                    .or_default() += 1;
                tally.entry("Not recognised").or_default().games += 1;
            }
        }
    }
    for (console, t) in tally {
        println!();
        println!(
            "{console}: {} games, music found in {} ({:.0}%), {} songs",
            t.games,
            t.with_music,
            t.with_music as f64 * 100.0 / t.games.max(1) as f64,
            t.songs
        );
        if t.checked[0] > 0 && o.mapcheck {
            println!(
                "  map check in {} games: {} of {} tracks played right from the ROM; with the gaps from the ROM {}, from the file {}; on the game's board {}; by the saved maps {}",
                t.checked[0], t.checked[4], t.checked[3], t.checked[1], t.checked[2], t.board, t.saved
            );
        } else if t.checked[0] > 0 {
            println!(
                "  checked against NSF/GBS tracks in {} games: {} of {} songs like a track, {} of {} tracks found",
                t.checked[0], t.checked[2], t.checked[1], t.checked[4], t.checked[3]
            );
            println!(
                "  unchecked (kept, no song like any track): {} games, {} songs",
                t.unchecked[0], t.unchecked[1]
            );
        }
        for (why, n) in t.reasons {
            println!("  {n}: {why}");
        }
    }
}
