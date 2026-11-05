//! Developer tool: runs a SNES ROM headless (`rom::snes_capture`) and writes what it left in
//! the sound chip as an `.spc`, plus what the sound chip played as a `.wav`. With a reference
//! `.spc` it also prints how much of the sound memory matches it.
//!
//!   snes_capture <rom.sfc> <seconds> <out-stem> [reference.spc]

use std::io::Write;

use romexplorer::rom::{snes_capture, snes_sound};

fn wav(path: &str, pcm: &[i16], rate: u32) -> std::io::Result<()> {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let data = (pcm.len() * 2) as u32;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * 4).to_le_bytes())?;
    f.write_all(&4u16.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data.to_le_bytes())?;
    for s in pcm {
        f.write_all(&s.to_le_bytes())?;
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: snes_capture <rom.sfc> <seconds> <out-stem> [reference.spc]");
        std::process::exit(2);
    }
    let rom = std::fs::read(&args[1]).expect("read the ROM");
    let seconds: f32 = args[2].parse().expect("seconds");
    let started = std::time::Instant::now();
    let c = match snes_capture::capture(&rom, seconds, true) {
        Ok(c) => c,
        Err(e) => {
            println!("error: {e}");
            std::process::exit(1);
        }
    };
    let peak = c.audio.iter().map(|s| (*s as i32).abs()).max().unwrap_or(0);
    println!(
        "ran {:.1} s of console time in {:.1} s ({} instructions); drivers started: {:?}; port writes: {}; peak {}; stopped: {:?}",
        seconds,
        started.elapsed().as_secs_f32(),
        c.instructions,
        c.driver_starts.iter().map(|d| format!("{:04x}@{:.2}s", d.entry, d.at as f64 / 21_477_272.0)).collect::<Vec<_>>(),
        c.port_writes.len(),
        peak,
        c.stopped
    );
    std::fs::write(format!("{}.spc", args[3]), &c.spc).expect("write the .spc");
    wav(&format!("{}.wav", args[3]), &c.audio, 32_000).expect("write the .wav");
    if let Some(reference) = args.get(4) {
        let reference = std::fs::read(reference).expect("read the reference .spc");
        let (Some(ours), Some(theirs)) = (snes_sound::spc_aram(&c.spc), snes_sound::spc_aram(&reference)) else {
            println!("not an .spc");
            return;
        };
        // Sound memory both have in use (not left at the power-on fill), and where they agree.
        let used = (0..0x10000).filter(|&i| !(0xF0..0x100).contains(&i) && theirs[i] != 0xFF && theirs[i] != 0).count();
        let same = (0..0x10000).filter(|&i| !(0xF0..0x100).contains(&i) && theirs[i] != 0xFF && theirs[i] != 0 && ours[i] == theirs[i]).count();
        println!("sound memory agreement with the reference: {same} of {used} bytes ({:.1}%)", same as f64 * 100.0 / used.max(1) as f64);
    }
}
