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
    iso9660_at(root, 0)
}

/// [`iso9660`] for a track that starts at sector `base` of its disc (a GD-ROM's
/// high-density area starts at 45000): every sector number in it counts from the disc's
/// start, as the filesystem does.
pub fn iso9660_at(root: &[Entry], base: u32) -> Vec<u8> {
    const S: usize = 2048;
    let mut sectors: Vec<Vec<u8>> = vec![vec![0u8; S]; 18];
    // Lays out a directory: reserves its sector, then its children; returns its lba.
    fn place(entries: &[Entry], parent: u32, base: u32, sectors: &mut Vec<Vec<u8>>) -> u32 {
        let me = sectors.len() as u32;
        sectors.push(Vec::new());
        let mut table = dir_record(&[0], base + me, S as u32, true);
        table.extend(dir_record(&[1], parent, S as u32, true));
        for e in entries {
            match e {
                Entry::File(name, data) => {
                    let lba = base + sectors.len() as u32;
                    for chunk in data.chunks(S) {
                        let mut s = chunk.to_vec();
                        s.resize(S, 0);
                        sectors.push(s);
                    }
                    table.extend(dir_record(format!("{name};1").as_bytes(), lba, data.len() as u32, false));
                }
                Entry::Dir(name, kids) => {
                    let lba = place(kids, base + me, base, sectors);
                    table.extend(dir_record(name.as_bytes(), lba, S as u32, true));
                }
            }
        }
        assert!(table.len() <= S);
        table.resize(S, 0);
        sectors[me as usize] = table;
        base + me
    }
    let root_lba = place(root, base + 18, base, &mut sectors);
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

/// Raw Mode 2 Form 2 sectors (2352 bytes: sync, header, subheader, 2324 bytes of data, EDC)
/// holding `data`, as CD-XA audio is stored. The subheader says Form 2, audio.
pub fn xa_raw(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for (n, chunk) in data.chunks(2324).enumerate() {
        let mut s = vec![0u8; 2352];
        s[1..11].fill(0xFF);
        s[12..15].copy_from_slice(&[0, 2, n as u8]);
        s[15] = 2;
        let sub = [1u8, 1, 0x64, 0];
        s[16..20].copy_from_slice(&sub);
        s[20..24].copy_from_slice(&sub);
        s[24..24 + chunk.len()].copy_from_slice(chunk);
        out.extend_from_slice(&s);
    }
    out
}

/// A DS stream (STRM): PCM16, mono, looping from the start, in one block.
pub fn nds_strm(sample_rate: u16, pcm: &[i16]) -> Vec<u8> {
    let data: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut out = b"STRM".to_vec();
    out.extend_from_slice(&[0xFF, 0xFE, 0x00, 0x01]);
    out.extend_from_slice(&((0x68 + data.len()) as u32).to_le_bytes());
    out.extend_from_slice(&0x10u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    let mut head = vec![0u8; 0x50];
    head[..4].copy_from_slice(b"HEAD");
    head[4..8].copy_from_slice(&0x50u32.to_le_bytes());
    head[0x08] = 1; // PCM16
    head[0x09] = 1; // loops
    head[0x0A] = 1; // channels
    head[0x0C..0x0E].copy_from_slice(&sample_rate.to_le_bytes());
    head[0x14..0x18].copy_from_slice(&(pcm.len() as u32).to_le_bytes());
    head[0x18..0x1C].copy_from_slice(&0x68u32.to_le_bytes());
    head[0x1C..0x20].copy_from_slice(&1u32.to_le_bytes());
    head[0x20..0x24].copy_from_slice(&(data.len() as u32).to_le_bytes());
    head[0x24..0x28].copy_from_slice(&(pcm.len() as u32).to_le_bytes());
    head[0x28..0x2C].copy_from_slice(&(data.len() as u32).to_le_bytes());
    head[0x2C..0x30].copy_from_slice(&(pcm.len() as u32).to_le_bytes());
    out.extend_from_slice(&head);
    out.extend_from_slice(b"DATA");
    out.extend_from_slice(&((data.len() + 8) as u32).to_le_bytes());
    out.extend_from_slice(&data);
    out
}

/// A DS sound archive (SDAT) holding `streams` as its STRM entries, named in SYMB.
pub fn sdat(streams: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let n = streams.len();
    // SYMB: 8 record offsets, the STRM record (8th) lists name offsets.
    let mut symb = vec![0u8; 0x40];
    let rec = symb.len();
    symb[0x08 + 7 * 4..0x08 + 7 * 4 + 4].copy_from_slice(&(rec as u32).to_le_bytes());
    symb.extend_from_slice(&(n as u32).to_le_bytes());
    let names_at = symb.len() + n * 4;
    let mut names = Vec::new();
    for (name, _) in streams {
        symb.extend_from_slice(&((names_at + names.len()) as u32).to_le_bytes());
        names.extend_from_slice(name.as_bytes());
        names.push(0);
    }
    symb.extend_from_slice(&names);
    symb[..4].copy_from_slice(b"SYMB");
    // INFO: the STRM record's entries each start with their u16 file id.
    let mut info = vec![0u8; 0x40];
    let rec = info.len();
    info[0x08 + 7 * 4..0x08 + 7 * 4 + 4].copy_from_slice(&(rec as u32).to_le_bytes());
    info.extend_from_slice(&(n as u32).to_le_bytes());
    let entries_at = info.len() + n * 4;
    for i in 0..n {
        info.extend_from_slice(&((entries_at + i * 12) as u32).to_le_bytes());
    }
    for i in 0..n {
        let mut e = vec![0u8; 12];
        e[..2].copy_from_slice(&(i as u16).to_le_bytes());
        info.extend_from_slice(&e);
    }
    info[..4].copy_from_slice(b"INFO");
    let pad = |v: &mut Vec<u8>| v.resize(v.len().div_ceil(0x20) * 0x20, 0);
    pad(&mut symb);
    pad(&mut info);
    let symb_at = 0x40;
    let info_at = symb_at + symb.len();
    let fat_at = info_at + info.len();
    let fat_len = (0x0C + n * 16).div_ceil(0x20) * 0x20;
    let file_at = fat_at + fat_len;
    let mut fat = b"FAT ".to_vec();
    fat.extend_from_slice(&(fat_len as u32).to_le_bytes());
    fat.extend_from_slice(&(n as u32).to_le_bytes());
    let mut files = b"FILE".to_vec();
    files.extend_from_slice(&[0; 0x1C]);
    for (_, data) in streams {
        let at = file_at + files.len();
        fat.extend_from_slice(&(at as u32).to_le_bytes());
        fat.extend_from_slice(&(data.len() as u32).to_le_bytes());
        fat.extend_from_slice(&[0; 8]);
        files.extend_from_slice(data);
        pad(&mut files);
    }
    fat.resize(fat_len, 0);
    let mut out = vec![0u8; 0x40];
    out[..4].copy_from_slice(b"SDAT");
    out[4..8].copy_from_slice(&[0xFF, 0xFE, 0x00, 0x01]);
    let total = file_at + files.len();
    out[8..12].copy_from_slice(&(total as u32).to_le_bytes());
    for (i, (at, len)) in [(symb_at, symb.len()), (info_at, info.len()), (fat_at, fat_len), (file_at, files.len())].iter().enumerate() {
        out[0x10 + i * 8..0x14 + i * 8].copy_from_slice(&(*at as u32).to_le_bytes());
        out[0x14 + i * 8..0x18 + i * 8].copy_from_slice(&(*len as u32).to_le_bytes());
    }
    [out, symb, info, fat, files].concat()
}

/// A DS cartridge: a header with the logo CRC and file tables, `files` in one folder.
pub fn nds(folder: &str, files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    // FNT: two directories (root, the folder: id 0xF001), its files numbered from 0.
    let mut fnt = Vec::new();
    fnt.extend_from_slice(&16u32.to_le_bytes());
    fnt.extend_from_slice(&0u16.to_le_bytes());
    fnt.extend_from_slice(&2u16.to_le_bytes());
    fnt.extend_from_slice(&0u32.to_le_bytes()); // patched below
    fnt.extend_from_slice(&0u16.to_le_bytes());
    fnt.extend_from_slice(&0xF000u16.to_le_bytes());
    fnt.push(0x80 | folder.len() as u8);
    fnt.extend_from_slice(folder.as_bytes());
    fnt.extend_from_slice(&0xF001u16.to_le_bytes());
    fnt.push(0);
    let sub = fnt.len() as u32;
    fnt[8..12].copy_from_slice(&sub.to_le_bytes());
    for (name, _) in files {
        fnt.push(name.len() as u8);
        fnt.extend_from_slice(name.as_bytes());
    }
    fnt.push(0);
    let fnt_at = 0x4000usize;
    let fat_at = (fnt_at + fnt.len()).div_ceil(0x200) * 0x200;
    let mut data_at = (fat_at + files.len() * 8).div_ceil(0x200) * 0x200;
    let mut out = vec![0u8; data_at];
    out[0x15C..0x15E].copy_from_slice(&[0x56, 0xCF]);
    out[0x40..0x44].copy_from_slice(&(fnt_at as u32).to_le_bytes());
    out[0x44..0x48].copy_from_slice(&(fnt.len() as u32).to_le_bytes());
    out[0x48..0x4C].copy_from_slice(&(fat_at as u32).to_le_bytes());
    out[0x4C..0x50].copy_from_slice(&((files.len() * 8) as u32).to_le_bytes());
    out[fnt_at..fnt_at + fnt.len()].copy_from_slice(&fnt);
    for (i, (_, data)) in files.iter().enumerate() {
        let at = fat_at + i * 8;
        out[at..at + 4].copy_from_slice(&(data_at as u32).to_le_bytes());
        out[at + 4..at + 8].copy_from_slice(&((data_at + data.len()) as u32).to_le_bytes());
        out.resize(data_at, 0);
        out.extend_from_slice(data);
        data_at = out.len().div_ceil(0x200) * 0x200;
    }
    out
}

/// Yaz0 with every byte stored as a literal: what the decoder reads, without compressing.
pub fn yaz0_stored(data: &[u8]) -> Vec<u8> {
    let mut out = b"Yaz0".to_vec();
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(&[0; 8]);
    for chunk in data.chunks(8) {
        out.push(0xFF);
        out.extend_from_slice(chunk);
    }
    out
}

/// A RARC archive with one root node holding `files`.
pub fn rarc(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut strings = b".\0..\0".to_vec();
    let mut entries = Vec::new();
    let mut data = Vec::new();
    for (i, (name, bytes)) in files.iter().enumerate() {
        let mut e = vec![0u8; 20];
        e[0..2].copy_from_slice(&(i as u16).to_be_bytes());
        e[4] = 0x11;
        e[6..8].copy_from_slice(&(strings.len() as u16).to_be_bytes());
        e[8..12].copy_from_slice(&(data.len() as u32).to_be_bytes());
        e[12..16].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
        strings.extend_from_slice(name.as_bytes());
        strings.push(0);
        entries.extend_from_slice(&e);
        data.extend_from_slice(bytes);
        data.resize(data.len().div_ceil(32) * 32, 0);
    }
    // Info block at 0x20; nodes at +0x20 (one, 16 bytes), entries, strings, then data.
    let nodes_at = 0x20usize;
    let entries_at = nodes_at + 0x20;
    let strings_at = entries_at + entries.len().div_ceil(32) * 32;
    let data_rel = strings_at + strings.len().div_ceil(32) * 32;
    let mut info = vec![0u8; data_rel];
    info[0..4].copy_from_slice(&1u32.to_be_bytes());
    info[4..8].copy_from_slice(&(nodes_at as u32).to_be_bytes());
    info[8..12].copy_from_slice(&(files.len() as u32).to_be_bytes());
    info[0x0C..0x10].copy_from_slice(&(entries_at as u32).to_be_bytes());
    info[0x14..0x18].copy_from_slice(&(strings_at as u32).to_be_bytes());
    info[nodes_at..nodes_at + 4].copy_from_slice(b"ROOT");
    info[nodes_at + 0x0A..nodes_at + 0x0C].copy_from_slice(&(files.len() as u16).to_be_bytes());
    info[entries_at..entries_at + entries.len()].copy_from_slice(&entries);
    info[strings_at..strings_at + strings.len()].copy_from_slice(&strings);
    let mut out = vec![0u8; 0x20];
    out[0..4].copy_from_slice(b"RARC");
    out[0x0C..0x10].copy_from_slice(&(data_rel as u32).to_be_bytes());
    out.extend_from_slice(&info);
    out.extend_from_slice(&data);
    let len = out.len() as u32;
    out[4..8].copy_from_slice(&len.to_be_bytes());
    out
}

/// A CPK with a TOC listing `files`, stored, from the content offset.
pub fn cpk(files: &[(&str, &str, Vec<u8>)]) -> Vec<u8> {
    // One @UTF table: per-row DirName, FileName (strings), FileSize, ExtractSize (u32), FileOffset (u64), ID (u32).
    fn utf(cols: &[(&str, u8, bool)], rows: &[Vec<Vec<u8>>], strings: &mut Vec<u8>, consts: &[Vec<u8>]) -> Vec<u8> {
        let mut schema = Vec::new();
        let mut ci = 0;
        for (name, ty, per_row) in cols {
            let off = strings.len() as u32;
            strings.extend_from_slice(name.as_bytes());
            strings.push(0);
            schema.push(if *per_row { 0x50 } else { 0x30 } | ty);
            schema.extend_from_slice(&off.to_be_bytes());
            if !per_row {
                schema.extend_from_slice(&consts[ci]);
                ci += 1;
            }
        }
        let row_len: usize = rows.first().map(|r| r.iter().map(Vec::len).sum()).unwrap_or(0);
        let rows_off = 0x18 + schema.len();
        let mut body = schema;
        for r in rows {
            for v in r {
                body.extend_from_slice(v);
            }
        }
        let strings_off = 0x18 + body.len();
        let data_off = strings_off + strings.len();
        let mut t = b"@UTF".to_vec();
        t.extend_from_slice(&((data_off) as u32).to_be_bytes());
        t.extend_from_slice(&0u16.to_be_bytes());
        t.extend_from_slice(&(rows_off as u16).to_be_bytes());
        t.extend_from_slice(&(strings_off as u32).to_be_bytes());
        t.extend_from_slice(&(data_off as u32).to_be_bytes());
        t.extend_from_slice(&0u32.to_be_bytes());
        t.extend_from_slice(&(cols.len() as u16).to_be_bytes());
        t.extend_from_slice(&(row_len as u16).to_be_bytes());
        t.extend_from_slice(&(rows.len() as u32).to_be_bytes());
        t.extend_from_slice(&body);
        t.extend_from_slice(strings);
        t
    }
    let toc_at = 0x800u64;
    // TOC strings first, so their offsets are known before the rows are written.
    let mut toc_strings = b"<NULL>\0".to_vec();
    let mut name_offs = Vec::new();
    for (dir, file, _) in files {
        let d = toc_strings.len() as u32;
        toc_strings.extend_from_slice(dir.as_bytes());
        toc_strings.push(0);
        let f = toc_strings.len() as u32;
        toc_strings.extend_from_slice(file.as_bytes());
        toc_strings.push(0);
        name_offs.push((d, f));
    }
    let content_at = 0x4000u64;
    let mut rows = Vec::new();
    let mut at = content_at;
    let mut blobs = Vec::new();
    for (i, (_, _, data)) in files.iter().enumerate() {
        let (d, f) = name_offs[i];
        rows.push(vec![d.to_be_bytes().to_vec(), f.to_be_bytes().to_vec(), (data.len() as u32).to_be_bytes().to_vec(), (data.len() as u32).to_be_bytes().to_vec(), (at - toc_at).to_be_bytes().to_vec(), (i as u32).to_be_bytes().to_vec()]);
        blobs.push((at, data.clone()));
        at = (at + data.len() as u64).div_ceil(0x800) * 0x800;
    }
    let cols = [("DirName", 0xA, true), ("FileName", 0xA, true), ("FileSize", 4, true), ("ExtractSize", 4, true), ("FileOffset", 6, true), ("ID", 4, true)];
    let toc = utf(&cols, &rows, &mut toc_strings, &[]);
    let mut hs = b"<NULL>\0".to_vec();
    let header = utf(&[("ContentOffset", 6, false), ("TocOffset", 6, false)], &[vec![]], &mut hs, &[content_at.to_be_bytes().to_vec(), toc_at.to_be_bytes().to_vec()]);
    let packet = |magic: &[u8; 4], t: &[u8]| {
        let mut p = magic.to_vec();
        p.extend_from_slice(&[0xFF, 0, 0, 0]);
        p.extend_from_slice(&(t.len() as u32).to_le_bytes());
        p.extend_from_slice(&[0; 4]);
        p.extend_from_slice(t);
        p
    };
    let mut out = packet(b"CPK ", &header);
    out.resize(toc_at as usize, 0);
    out.extend_from_slice(&packet(b"TOC ", &toc));
    for (at, data) in blobs {
        out.resize(at as usize, 0);
        out.extend_from_slice(&data);
    }
    out
}

/// A 3DS cartridge image (NCSD) whose content partition is stored decrypted, with `files`
/// in the root of its RomFS.
pub fn ctr_romfs(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    const NONE: u32 = u32::MAX;
    let mut dirs = vec![0u8; 0x18];
    dirs[4..8].copy_from_slice(&NONE.to_le_bytes());
    dirs[8..12].copy_from_slice(&NONE.to_le_bytes());
    dirs[12..16].copy_from_slice(&(if files.is_empty() { NONE } else { 0 }).to_le_bytes());
    dirs[16..20].copy_from_slice(&NONE.to_le_bytes());
    let mut metas = Vec::new();
    let mut data = Vec::new();
    for (i, (name, bytes)) in files.iter().enumerate() {
        let name: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let len = (0x20 + name.len()).div_ceil(4) * 4;
        let next = if i + 1 < files.len() { (metas.len() + len) as u32 } else { NONE };
        let mut m = vec![0u8; len];
        m[4..8].copy_from_slice(&next.to_le_bytes());
        m[8..16].copy_from_slice(&(data.len() as u64).to_le_bytes());
        m[16..24].copy_from_slice(&(bytes.len() as u64).to_le_bytes());
        m[24..28].copy_from_slice(&NONE.to_le_bytes());
        m[28..32].copy_from_slice(&(name.len() as u32).to_le_bytes());
        m[32..32 + name.len()].copy_from_slice(&name);
        metas.extend_from_slice(&m);
        data.extend_from_slice(bytes);
        data.resize(data.len().div_ceil(16) * 16, 0);
    }
    let dir_at = 0x28usize;
    let meta_at = dir_at + dirs.len();
    let data_at = (meta_at + metas.len()).div_ceil(16) * 16;
    let mut l3 = vec![0u8; data_at];
    l3[0..4].copy_from_slice(&0x28u32.to_le_bytes());
    l3[0x0C..0x10].copy_from_slice(&(dir_at as u32).to_le_bytes());
    l3[0x10..0x14].copy_from_slice(&(dirs.len() as u32).to_le_bytes());
    l3[0x1C..0x20].copy_from_slice(&(meta_at as u32).to_le_bytes());
    l3[0x20..0x24].copy_from_slice(&(metas.len() as u32).to_le_bytes());
    l3[0x24..0x28].copy_from_slice(&(data_at as u32).to_le_bytes());
    l3[dir_at..dir_at + dirs.len()].copy_from_slice(&dirs);
    l3[meta_at..meta_at + metas.len()].copy_from_slice(&metas);
    l3.extend_from_slice(&data);
    let mut romfs = vec![0u8; 0x1000];
    romfs[0..4].copy_from_slice(b"IVFC");
    romfs[0x4C..0x50].copy_from_slice(&12u32.to_le_bytes());
    romfs.extend_from_slice(&l3);
    romfs.resize(romfs.len().div_ceil(0x200) * 0x200, 0);
    let mut ncch = vec![0u8; 0x200];
    ncch[0x100..0x104].copy_from_slice(b"NCCH");
    ncch[0x18F] = 0x04;
    ncch[0x1B0..0x1B4].copy_from_slice(&1u32.to_le_bytes());
    ncch[0x1B4..0x1B8].copy_from_slice(&((romfs.len() / 0x200) as u32).to_le_bytes());
    let mut out = vec![0u8; 0x4000];
    out[0x100..0x104].copy_from_slice(b"NCSD");
    out[0x120..0x124].copy_from_slice(&(0x4000u32 / 0x200).to_le_bytes());
    out.extend_from_slice(&ncch);
    out.extend_from_slice(&romfs);
    out
}
