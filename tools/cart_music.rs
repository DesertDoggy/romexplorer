//! Developer tool: the cartridge music search (`rom::music_search`) over ROMs on this PC, to
//! see how many games it finds music in and why not in the rest.
//!
//!   cart_music [--render <dir>] [--seconds N] [--verbose] <rom or folder>...
//!
//! For each NES/Famicom, Game Boy/Color, Master System, Game Gear or Mega Drive ROM (loose, or the first one in a `.zip`/`.7z`): the
//! console, how long the search took, the song numbers it found, or why none. `--render` also
//! writes the first N seconds (default 20) of every song as a WAV, to listen to. A summary
//! ends the run.

use std::io::Write;
use std::path::{Path, PathBuf};

use romexplorer::rom::gb_music::GameBoy;
use romexplorer::rom::md_music::MegaDrive;
use romexplorer::rom::music_search::{self, Machine, Song};
use romexplorer::rom::nes_music::Nes;
use romexplorer::rom::pce_music::PcEngine;
use romexplorer::rom::sms_music::{GameGear, MasterSystem};
use romexplorer::rom::{nes_cart, rom_songs};

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
                | "zip"
                | "7z"
        ) {
            out.push(path.to_path_buf());
        }
    }
}

/// Which console a ROM is for, from its header (or, for a Sega 8-bit ROM, the file name).
fn console(rom: &[u8], path: &Path) -> Option<&'static str> {
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
    if name.ends_with(".pce")
        || name.contains("pc engine")
        || name.contains("pcengine")
        || name.contains("turbografx")
    {
        return Some("PCE");
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
}

struct Options {
    render: Option<PathBuf>,
    seconds: u32,
    verbose: bool,
}

fn run<M: Machine>(path: &Path, rom: &[u8], what: &str, o: &Options, t: &mut Tally) {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    t.games += 1;
    let started = std::time::Instant::now();
    let mut notes = Vec::new();
    let found = music_search::search_logged::<M>(rom, &mut |n| notes.push(n));
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
    };
    args.retain(|a| a != "--verbose");
    if let Some(i) = args.iter().position(|a| a == "--render") {
        o.render = args.get(i + 1).map(PathBuf::from);
        args.drain(i..(i + 2).min(args.len()));
    }
    if let Some(i) = args.iter().position(|a| a == "--seconds") {
        o.seconds = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(20);
        args.drain(i..(i + 2).min(args.len()));
    }
    if args.is_empty() {
        eprintln!(
            "usage: cart_music [--render <dir>] [--seconds N] [--verbose] <rom or folder>..."
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
    let mut tally: std::collections::BTreeMap<&str, Tally> = Default::default();
    for path in files {
        let rom = match rom_songs::read_rom(&path) {
            Ok(r) => r,
            Err(e) => {
                println!("{}: unreadable: {e}", path.display());
                continue;
            }
        };
        match console(&rom, &path) {
            Some("NES") => {
                let what = format!(
                    "mapper {}",
                    nes_cart::parse(&rom).map(|h| h.mapper).unwrap_or(0)
                );
                run::<Nes>(&path, &rom, &what, &o, tally.entry("NES").or_default());
            }
            Some("SMS") => run::<MasterSystem>(
                &path,
                &rom,
                "Master System",
                &o,
                tally.entry("Master System").or_default(),
            ),
            Some("MD") => run::<MegaDrive>(
                &path,
                &rom,
                "Mega Drive",
                &o,
                tally.entry("Mega Drive").or_default(),
            ),
            Some("FDS") => {
                run::<Nes>(
                    &path,
                    &rom,
                    "Disk System",
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
            Some("GG") => run::<GameGear>(
                &path,
                &rom,
                "Game Gear",
                &o,
                tally.entry("Game Gear").or_default(),
            ),
            Some(c) => {
                let what = format!("{c}, cartridge ${:02X}", rom[0x147]);
                run::<GameBoy>(&path, &rom, &what, &o, tally.entry("Game Boy").or_default());
            }
            None => {}
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
        for (why, n) in t.reasons {
            println!("  {n}: {why}");
        }
    }
}
