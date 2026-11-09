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
use romexplorer::rom::{gb_gbs, gb_map, hes_map, sgc_map, music_map, nes_cart, nes_nsf, rip_match, rip_sets, rom_songs};

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
    if name.ends_with(".nes") {
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
    let hint = rip_sets::first_with_magic(set, magic).and_then(|d| Some((read(&d)?, d)));
    if verbose {
        println!(
            "    matched set: {} ({:.0}% matched){}",
            set.display(),
            share * 100.0,
            if hint.is_some() { "" } else { ", no NSF/GBS in it" }
        );
    }
    hint
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
    let disk = romexplorer::rom::fds::is_disk(rom);
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
    // (A Disk System game has no board: its files load into RAM.)
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
        "{name}: map, {:.1} s: {right} of {with_melody} tracks played right from the ROM (gaps from the ROM: {bridged}; gaps from the file: {oracle}; saved map: {saved}) ({} runs; scripts up to {calls} calls){}",
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
    };
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
            "usage: cart_music [--render <dir>] [--seconds N] [--verbose] [--probe SECONDS] [--originals <folder> [--layout | --mapcheck]] <rom or folder>..."
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
