//! Synthetic game-audio files for the ROM audio tests: tones and silence wrapped in the
//! containers consoles use, built byte by byte here. No game data.

/// A sine tone, interleaved 16-bit, `channels` wide.
pub fn tone(sample_rate: u32, channels: u16, samples: usize, hz: f64) -> Vec<i16> {
    let mut out = Vec::with_capacity(samples * channels as usize);
    for n in 0..samples {
        let v = ((n as f64 * hz * std::f64::consts::TAU / sample_rate as f64).sin() * 8000.0) as i16;
        for _ in 0..channels {
            out.push(v);
        }
    }
    out
}

/// A RIFF WAVE of 16-bit PCM, with a `smpl` chunk looping `[loop_start, loop_end]` (inclusive
/// sample numbers) when given: how many PC and console tools mark a loop in a WAV.
pub fn wav(sample_rate: u32, channels: u16, pcm: &[i16], looped: Option<(u32, u32)>) -> Vec<u8> {
    let data_len = (pcm.len() * 2) as u32;
    let mut smpl = Vec::new();
    if let Some((start, end)) = looped {
        smpl.extend_from_slice(b"smpl");
        smpl.extend_from_slice(&60u32.to_le_bytes());
        for v in [0u32, 0, 1_000_000_000 / sample_rate, 60, 0, 0, 0, 1, 0] {
            smpl.extend_from_slice(&v.to_le_bytes());
        }
        for v in [0u32, 0, start, end, 0, 0] {
            smpl.extend_from_slice(&v.to_le_bytes());
        }
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(4 + 24 + 8 + data_len + smpl.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * channels as u32 * 2).to_le_bytes());
    out.extend_from_slice(&(channels * 2).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out.extend_from_slice(&smpl);
    out
}

/// A Sony `VAGp` file of `frames` silent PS-ADPCM frames (28 samples each), mono: the
/// format PS1/PS2/PSP games keep voices and many streams in.
pub fn vag(sample_rate: u32, frames: usize) -> Vec<u8> {
    let data_len = (frames * 16) as u32;
    let mut out = Vec::new();
    out.extend_from_slice(b"VAGp");
    out.extend_from_slice(&0x20u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&data_len.to_be_bytes());
    out.extend_from_slice(&sample_rate.to_be_bytes());
    out.resize(0x20, 0);
    out.extend_from_slice(b"synthetic\0\0\0\0\0\0\0");
    out.resize(0x30, 0);
    for f in 0..frames {
        let mut frame = [0u8; 16];
        if f + 1 == frames {
            frame[1] = 0x01; // end
        }
        out.extend_from_slice(&frame);
    }
    out
}

/// A file or a directory of a test disc.
pub enum Entry {
    File(&'static str, Vec<u8>),
    Dir(&'static str, Vec<Entry>),
}

fn dir_record(name: &[u8], lba: u32, len: u32, dir: bool) -> Vec<u8> {
    let mut r = vec![0u8; 33];
    r[2..6].copy_from_slice(&lba.to_le_bytes());
    r[6..10].copy_from_slice(&lba.to_be_bytes());
    r[10..14].copy_from_slice(&len.to_le_bytes());
    r[14..18].copy_from_slice(&len.to_be_bytes());
    r[25] = if dir { 2 } else { 0 };
    r[28] = 1;
    r[32] = name.len() as u8;
    r.extend_from_slice(name);
    if r.len() % 2 == 1 {
        r.push(0);
    }
    r[0] = r.len() as u8;
    r
}

/// A minimal ISO9660 image (2048-byte sectors): the volume descriptor at sector 16, one
/// sector per directory, files after them. Names get the `;1` version suffix discs carry.
pub fn iso9660(root: &[Entry]) -> Vec<u8> {
    const S: usize = 2048;
    let mut sectors: Vec<Vec<u8>> = vec![vec![0u8; S]; 18];
    // Lays out a directory: reserves its sector, then its children; returns (lba, size).
    fn place(entries: &[Entry], parent: u32, sectors: &mut Vec<Vec<u8>>) -> u32 {
        let me = sectors.len() as u32;
        sectors.push(Vec::new());
        let mut table = dir_record(&[0], me, S as u32, true);
        table.extend(dir_record(&[1], parent, S as u32, true));
        for e in entries {
            match e {
                Entry::File(name, data) => {
                    let lba = sectors.len() as u32;
                    for chunk in data.chunks(S) {
                        let mut s = chunk.to_vec();
                        s.resize(S, 0);
                        sectors.push(s);
                    }
                    table.extend(dir_record(format!("{name};1").as_bytes(), lba, data.len() as u32, false));
                }
                Entry::Dir(name, kids) => {
                    let lba = place(kids, me, sectors);
                    table.extend(dir_record(name.as_bytes(), lba, S as u32, true));
                }
            }
        }
        assert!(table.len() <= S);
        table.resize(S, 0);
        sectors[me as usize] = table;
        me
    }
    let root_lba = place(root, 18, &mut sectors);
    let mut pvd = vec![0u8; S];
    pvd[0] = 1;
    pvd[1..6].copy_from_slice(b"CD001");
    pvd[6] = 1;
    pvd[40..48].copy_from_slice(b"TESTDISC");
    let root_rec = dir_record(&[0], root_lba, S as u32, true);
    pvd[156..156 + 34].copy_from_slice(&root_rec[..34]);
    sectors[16] = pvd;
    let mut term = vec![0u8; S];
    term[0] = 255;
    term[1..6].copy_from_slice(b"CD001");
    sectors[17] = term;
    sectors.concat()
}

/// `iso` as a raw MODE1/2352 track: a 16-byte header before and 288 bytes of EDC/ECC after
/// each sector's data (left zero; nothing here checks them).
pub fn raw_2352(iso: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(iso.len() / 2048 * 2352);
    for sector in iso.chunks(2048) {
        let mut head = [0u8; 16];
        head[1..11].fill(0xFF);
        head[15] = 1;
        out.extend_from_slice(&head);
        out.extend_from_slice(sector);
        out.extend_from_slice(&[0u8; 288]);
    }
    out
}

/// A GameCube disc: the header's magic and FST pointer, the FST, and the files.
pub fn gamecube(files: &[(&'static str, Vec<u8>)]) -> Vec<u8> {
    let mut disc = vec![0u8; 0x2440];
    disc[0x1C..0x20].copy_from_slice(&[0xC2, 0x33, 0x9F, 0x3D]);
    // One directory "audio" holding every file: root, the dir, then the files.
    let count = 2 + files.len() as u32;
    let mut entries = Vec::new();
    let mut strings = Vec::new();
    entries.extend_from_slice(&[1u8, 0, 0, 0]);
    entries.extend_from_slice(&0u32.to_be_bytes());
    entries.extend_from_slice(&count.to_be_bytes());
    entries.extend_from_slice(&(0x0100_0000u32 | strings.len() as u32).to_be_bytes());
    strings.extend_from_slice(b"audio\0");
    entries.extend_from_slice(&0u32.to_be_bytes());
    entries.extend_from_slice(&count.to_be_bytes());
    let mut data_at = 0x10000u32;
    let mut blobs = Vec::new();
    for (name, data) in files {
        entries.extend_from_slice(&(strings.len() as u32).to_be_bytes());
        strings.extend_from_slice(name.as_bytes());
        strings.push(0);
        entries.extend_from_slice(&data_at.to_be_bytes());
        entries.extend_from_slice(&(data.len() as u32).to_be_bytes());
        blobs.push((data_at, data.clone()));
        data_at += (data.len() as u32).div_ceil(0x8000) * 0x8000;
    }
    let fst_at = 0x2440u32;
    let fst = [entries, strings].concat();
    disc[0x424..0x428].copy_from_slice(&fst_at.to_be_bytes());
    disc[0x428..0x42C].copy_from_slice(&(fst.len() as u32).to_be_bytes());
    disc.extend_from_slice(&fst);
    for (at, data) in blobs {
        if disc.len() < at as usize {
            disc.resize(at as usize, 0);
        }
        disc.extend_from_slice(&data);
    }
    disc
}

/// CD audio: `seconds` of a stereo tone as a `.bin` track holds it (little-endian), padded
/// to whole 2352-byte frames.
pub fn cdda(seconds: f64) -> Vec<u8> {
    let samples = (44_100.0 * seconds) as usize;
    let mut out: Vec<u8> = tone(44_100, 2, samples, 330.0).iter().flat_map(|s| s.to_le_bytes()).collect();
    out.resize(out.len().div_ceil(2352) * 2352, 0);
    out
}
