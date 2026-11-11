//! Lists the sequenced music found on disc images, the way a title page indexes it, and
//! optionally renders the start of each song to WAV for listening.
//!
//! ```text
//! disc_music <disc or folder>... [--wav <out dir>] [--seconds N] [--songs N]
//! ```
//!
//! `--probe <formats> [--magic A,B,...]` instead scans, per disc, the files holding any of
//! the magic strings (every file up to 64 MB without `--magic`) in the given VGMTrans
//! formats, all together and each alone, and lists what is found: for working out how a
//! custom engine's files sit on a disc.
//!
//! A folder is walked for `.chd`, `.cue`, `.iso` and `.bin` images (a `.bin` with a `.cue`
//! beside it is left to the sheet).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use romexplorer::rom::{disc_files, disc_seq, rom_audio, vgmtrans};

fn images(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let Ok(entries) = std::fs::read_dir(path) else { return };
        let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for e in entries {
            images(&e, out);
        }
        return;
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let keep = match ext.as_str() {
        "chd" | "cue" | "iso" => true,
        "bin" => !path.with_extension("cue").exists(),
        _ => false,
    };
    if keep {
        out.push(path.to_path_buf());
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut take = |flag: &str| -> Option<String> {
        let i = args.iter().position(|a| a == flag)?;
        let v = args.get(i + 1).cloned();
        args.drain(i..(i + 2).min(args.len()));
        v
    };
    let wav = take("--wav").map(PathBuf::from);
    let seconds: u64 = take("--seconds").and_then(|s| s.parse().ok()).unwrap_or(30);
    let probe = take("--probe");
    let magic: Vec<Vec<u8>> = take("--magic").map(|m| m.split(',').map(|x| x.as_bytes().to_vec()).collect()).unwrap_or_default();
    let max_songs: usize = take("--songs").and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
    if args.is_empty() {
        eprintln!("usage: disc_music <disc or folder>... [--wav <out dir>] [--seconds N] [--songs N]");
        std::process::exit(2);
    }
    let mut discs = Vec::new();
    for a in &args {
        images(Path::new(a), &mut discs);
    }
    if let Some(formats) = probe {
        for disc in &discs {
            probe_disc(disc, &formats, &magic, wav.as_deref(), seconds, max_songs);
        }
        return;
    }
    let t = rom_audio::Thresholds::default();
    let (mut total_discs, mut with_songs, mut total_songs) = (0, 0, 0);
    for disc in &discs {
        total_discs += 1;
        let started = Instant::now();
        let found = match disc_seq::index_disc(disc, &t, &mut |_| {}) {
            Ok(f) => f,
            Err(e) => {
                println!("{}: error: {e}", disc.display());
                continue;
            }
        };
        println!("{}: {} songs in {:.1}s", disc.display(), found.len(), started.elapsed().as_secs_f64());
        if !found.is_empty() {
            with_songs += 1;
            total_songs += found.len();
        }
        for (i, track) in found.iter().enumerate() {
            let rom_audio::TrackLocation::RomSong { engine, song } = &track.location else { continue };
            println!(
                "  {:3} {} [{}] {:.1}s{} {}",
                i,
                track.name,
                track.kind.name(),
                track.duration_ms as f64 / 1000.0,
                if track.looping { " loops" } else { "" },
                engine.rsplit('@').next().unwrap_or("")
            );
            let Some(dir) = &wav else { continue };
            if i >= max_songs {
                continue;
            }
            let stem = disc.file_stem().and_then(|s| s.to_str()).unwrap_or("disc");
            let out = dir.join(format!("{stem} - {i:03}.wav"));
            match disc_seq::open(disc, engine, *song) {
                Ok(mut pcm) => {
                    let rate = pcm.sample_rate();
                    let mut samples: Vec<i16> = Vec::new();
                    let cap = seconds * u64::from(rate) * 2;
                    while (samples.len() as u64) < cap {
                        match pcm.next_chunk() {
                            Ok(Some(c)) => samples.extend_from_slice(&c),
                            _ => break,
                        }
                    }
                    samples.truncate(cap as usize);
                    let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
                    let mut f = std::fs::File::create(&out).expect("create wav");
                    f.write_all(&rom_audio::wav_header(rate, 2, samples.len() as u64 / 2)).unwrap();
                    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
                    f.write_all(&bytes).unwrap();
                    println!("      -> {} (peak {peak})", out.display());
                }
                Err(e) => println!("      play error: {e}"),
            }
        }
    }
    println!("{total_discs} discs, {with_songs} with sequenced music, {total_songs} songs");
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn probe_disc(disc: &Path, formats: &str, magic: &[Vec<u8>], wav: Option<&Path>, seconds: u64, max_songs: usize) {
    let started = Instant::now();
    let rom = match rom_audio::open_rom(disc) {
        Ok(r) => r,
        Err(e) => {
            println!("{}: error: {e}", disc.display());
            return;
        }
    };
    println!("{}", disc.display());
    for (n, view) in rom.data.iter().enumerate() {
        let Ok(listing) = disc_files::list(view) else { continue };
        let mut picked: Vec<(String, Vec<u8>)> = Vec::new();
        for file in &listing.files {
            if file.size < 16 || file.size > 64 << 20 || rom_audio::is_video(file.name()) {
                continue;
            }
            let Ok(bytes) = disc_seq::read_whole(view, file) else { continue };
            let hits: Vec<String> = magic.iter().filter_map(|m| find(&bytes, m).map(|at| format!("{}@{at:#x}", String::from_utf8_lossy(m)))).collect();
            if !magic.is_empty() && hits.is_empty() {
                continue;
            }
            println!("  view {n} file {} ({} bytes) {}", file.path, bytes.len(), hits.join(" "));
            picked.push((file.path.clone(), bytes));
        }
        if picked.is_empty() {
            continue;
        }
        let files: Vec<(&str, &[u8])> = picked.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
        for (i, f) in files.iter().enumerate() {
            let alone = vgmtrans::songs_in(&files[i..=i], formats, 0).map(|s| s.len()).unwrap_or(0);
            if alone > 0 {
                println!("    alone: {} -> {alone} songs", f.0);
            }
        }
        let t0 = Instant::now();
        let songs = match vgmtrans::songs_in(&files, formats, 0) {
            Ok(s) => s,
            Err(e) => {
                println!("  scan error: {e}");
                continue;
            }
        };
        println!("  together: {} songs from {} files in {:.1}s", songs.len(), files.len(), t0.elapsed().as_secs_f64());
        for (i, song) in songs.iter().enumerate() {
            println!("    {i:3} {} [{}] {} @{:#x} {:.1}s{}", song.name, song.format, files[song.file as usize].0, song.offset, song.length_ms as f64 / 1000.0, if song.looping { " loops" } else { "" });
            let Some(dir) = wav else { continue };
            if i >= max_songs {
                continue;
            }
            match vgmtrans::Player::open_in(&files, formats, 2, i as u32, 48_000) {
                Ok(mut p) => {
                    let total = (seconds * 48_000 * 2) as usize;
                    let mut samples = vec![0i16; total];
                    for chunk in samples.chunks_mut(8192) {
                        p.render(chunk);
                    }
                    let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
                    let stem = disc.file_stem().and_then(|s| s.to_str()).unwrap_or("disc");
                    let out = dir.join(format!("{stem} - probe {i:03}.wav"));
                    let mut f = std::fs::File::create(&out).expect("create wav");
                    f.write_all(&rom_audio::wav_header(48_000, 2, samples.len() as u64 / 2)).unwrap();
                    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
                    f.write_all(&bytes).unwrap();
                    println!("        -> {} (peak {peak})", out.display());
                }
                Err(e) => println!("        play error: {e}"),
            }
        }
    }
    println!("  ({:.1}s)", started.elapsed().as_secs_f64());
}
