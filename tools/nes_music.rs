//! Developer tool: the NES music search (`rom::music_search` on `rom::nes_music`) over ROMs on this PC, to see how
//! many games it finds music in and why not in the rest.
//!
//!   nes_music [--render <dir>] [--seconds N] [--verbose] <rom or folder>...
//!
//! For each `.nes` (loose, or the first one in a `.zip`/`.7z`): the board, how long the search
//! took, the song numbers it found, or why none. `--render` also writes the first N seconds
//! (default 20) of every song as a WAV, to listen to. A summary ends the run.

use std::io::Write;
use std::path::{Path, PathBuf};

use romexplorer::rom::music_search::{self, Song};
use romexplorer::rom::nes_music::Nes;
use romexplorer::rom::{nes_cart, rom_songs};

fn wav(path: &Path, pcm: &[i16], rate: u32) -> std::io::Result<()> {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let data = (pcm.len() * 2) as u32;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * 2).to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?;
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
        if matches!(ext.as_str(), "nes" | "zip" | "7z") {
            out.push(path.to_path_buf());
        }
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut render: Option<PathBuf> = None;
    let mut seconds = 20u32;
    let verbose = args.iter().any(|a| a == "--verbose");
    args.retain(|a| a != "--verbose");
    if let Some(i) = args.iter().position(|a| a == "--render") {
        render = args.get(i + 1).map(PathBuf::from);
        args.drain(i..(i + 2).min(args.len()));
    }
    if let Some(i) = args.iter().position(|a| a == "--seconds") {
        seconds = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(20);
        args.drain(i..(i + 2).min(args.len()));
    }
    if args.is_empty() {
        eprintln!("usage: nes_music [--render <dir>] [--seconds N] [--verbose] <rom or folder>...");
        std::process::exit(2);
    }
    if !romexplorer::rom::nes_apu::available() {
        eprintln!("the rommusic library (NES sound chips) was not found: set ROMEXPLORER_EXE_ROOT or put it in lib/ beside this tool");
        std::process::exit(1);
    }
    let mut files = Vec::new();
    for a in &args {
        collect(Path::new(a), &mut files);
    }
    let (mut games, mut with_music, mut songs_total) = (0, 0, 0);
    let mut reasons: std::collections::BTreeMap<String, usize> = Default::default();
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let rom = match rom_songs::read_rom(&path) {
            Ok(r) => r,
            Err(e) => {
                println!("{name}: unreadable: {e}");
                continue;
            }
        };
        if nes_cart::parse(&rom).is_err() {
            continue;
        }
        games += 1;
        let board = nes_cart::parse(&rom).map(|h| h.mapper).unwrap_or(0);
        let started = std::time::Instant::now();
        let mut notes = Vec::new();
        let found = music_search::search_logged::<Nes>(&rom, &mut |n| notes.push(n));
        let took = started.elapsed().as_secs_f32();
        match found {
            Ok(f) if !f.songs.is_empty() => {
                with_music += 1;
                songs_total += f.songs.len();
                println!(
                    "{name}: mapper {board}, {took:.1} s: {} songs {:?} [{}]",
                    f.songs.len(),
                    f.songs,
                    f.recipe.encode()
                );
                if let Some(dir) = &render {
                    let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
                    let dir = dir.join(&stem);
                    let _ = std::fs::create_dir_all(&dir);
                    for (i, &v) in f.songs.iter().enumerate() {
                        let timing = music_search::measure::<Nes>(&rom, &f.recipe, v);
                        let Ok(mut song) = Song::<Nes>::open(&rom, &f.recipe, v, 44_100) else {
                            continue;
                        };
                        let frames = (seconds as f64 * song.frame_rate()) as usize;
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
                        );
                        if verbose {
                            println!("    song {v}: {timing:?}");
                        }
                    }
                }
            }
            Ok(f) => {
                let why = f.why.unwrap_or_default();
                println!("{name}: mapper {board}, {took:.1} s: no music: {why}");
                *reasons.entry(why).or_default() += 1;
            }
            Err(e) => {
                println!("{name}: mapper {board}: {e}");
                let key = if e.contains("mapper") {
                    "board not supported yet".to_string()
                } else {
                    e
                };
                *reasons.entry(key).or_default() += 1;
            }
        }
        if verbose {
            for n in notes {
                println!("    {n}");
            }
        }
    }
    println!();
    println!(
        "{games} games: music found in {with_music} ({:.0}%), {songs_total} songs",
        with_music as f64 * 100.0 / games.max(1) as f64
    );
    for (why, n) in reasons {
        println!("  {n}: {why}");
    }
}
