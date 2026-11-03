//! A minimal `.m4a` writer: one AAC or xHE-AAC track, not fragmented, the index (`moov`)
//! before the audio (`mdat`) so a player can start and seek without reading to the end.
//!
//! Written here rather than taken from a crate because it is a hundred lines for exactly one
//! track shape, and the two things that matter are specific to what [`super::xaac`] produces:
//! the stream header goes into `esds` verbatim, and xHE-AAC needs its independently decodable
//! frames listed (`stss`), which a generic AAC muxer does not write.

fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend(((body.len() + 8) as u32).to_be_bytes());
    out.extend(kind);
    out.extend(body);
    out
}

fn full(kind: &[u8; 4], version_and_flags: u32, body: &[u8]) -> Vec<u8> {
    let mut inner = Vec::with_capacity(body.len() + 4);
    inner.extend(version_and_flags.to_be_bytes());
    inner.extend(body);
    boxed(kind, &inner)
}

/// An MPEG-4 descriptor: tag, length in the four-byte form every reader accepts, body.
fn descriptor(tag: u8, body: &[u8]) -> Vec<u8> {
    let len = body.len() as u32;
    let mut out = vec![tag, 0x80 | (len >> 21) as u8 & 0x7f, 0x80 | (len >> 14) as u8 & 0x7f, 0x80 | (len >> 7) as u8 & 0x7f, len as u8 & 0x7f];
    out.extend(body);
    out
}

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

fn be16(v: u16) -> [u8; 2] {
    v.to_be_bytes()
}

/// What [`write`] needs to know about the stream.
pub struct Track<'a> {
    /// One access unit each, in order.
    pub frames: &'a [Vec<u8>],
    /// The AudioSpecificConfig (`xaac::Encoder::config`).
    pub config: &'a [u8],
    pub sample_rate: u32,
    pub channels: u16,
    /// Samples per channel each frame decodes to.
    pub frame_samples: u32,
    /// Nominal bits per second, for players that show it.
    pub bitrate: u32,
    /// The frames a decoder can start at, by index; `None` when every frame is one (AAC-LC).
    pub sync_frames: Option<&'a [u32]>,
}

/// The frames of an xHE-AAC stream a decoder can start from: those whose first bit
/// (`usacIndependencyFlag`) is set. Seeking lands on one of these.
pub fn xhe_sync_frames(frames: &[Vec<u8>]) -> Vec<u32> {
    frames.iter().enumerate().filter(|(_, f)| f.first().is_some_and(|b| b & 0x80 != 0)).map(|(i, _)| i as u32).collect()
}

/// The file, whole. A ten-minute track at 128 kbps is under 10 MB.
pub fn write(track: &Track<'_>) -> Vec<u8> {
    let count = track.frames.len() as u32;
    let duration = count * track.frame_samples;
    let largest = track.frames.iter().map(Vec::len).max().unwrap_or(0) as u32;

    let mut ftyp = Vec::new();
    ftyp.extend(b"M4A ");
    ftyp.extend(be32(0));
    for brand in [b"M4A ", b"mp42", b"isom"] {
        ftyp.extend(brand);
    }
    let ftyp = boxed(b"ftyp", &ftyp);

    let matrix: Vec<u8> = [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000].iter().flat_map(|v| v.to_be_bytes()).collect();
    let mvhd = full(b"mvhd", 0, &[&be32(0)[..], &be32(0), &be32(track.sample_rate), &be32(duration), &be32(0x0001_0000), &be16(0x0100), &[0; 10], &matrix, &[0; 24], &be32(2)].concat());
    // flags 7: enabled, in the movie, in the preview.
    let tkhd = full(b"tkhd", 7, &[&be32(0)[..], &be32(0), &be32(1), &be32(0), &be32(duration), &[0; 8], &be16(0), &be16(0), &be16(0x0100), &be16(0), &matrix, &be32(0), &be32(0)].concat());
    // 0x55c4: language "und".
    let mdhd = full(b"mdhd", 0, &[&be32(0)[..], &be32(0), &be32(track.sample_rate), &be32(duration), &be16(0x55c4), &be16(0)].concat());
    let hdlr = full(b"hdlr", 0, &[&be32(0)[..], b"soun", &[0; 12], b"SoundHandler\0"].concat());
    let smhd = full(b"smhd", 0, &[0; 4]);
    let dinf = boxed(b"dinf", &full(b"dref", 0, &[&be32(1)[..], &full(b"url ", 1, &[])].concat()));

    // 0x40: MPEG-4 audio. 0x15: an audio stream. Then buffer size, peak and average bitrate.
    let decoder_config = descriptor(0x04, &[&[0x40, 0x15][..], &[0, 0, 0], &be32((u64::from(largest) * 8 * u64::from(track.sample_rate) / u64::from(track.frame_samples.max(1))) as u32), &be32(track.bitrate), &descriptor(0x05, track.config)].concat());
    let es = descriptor(0x03, &[&be16(1)[..], &[0], &decoder_config, &descriptor(0x06, &[2])].concat());
    // The 16.16 sample-rate field cannot hold 88.2 or 96 kHz; readers take the rate from the
    // stream header, and the convention for a rate that does not fit is 0.
    let rate_field = if track.sample_rate <= 0xffff { track.sample_rate << 16 } else { 0 };
    let mp4a = boxed(b"mp4a", &[&[0; 6][..], &be16(1), &[0; 8], &be16(track.channels), &be16(16), &be16(0), &be16(0), &be32(rate_field), &full(b"esds", 0, &es)].concat());
    let stsd = full(b"stsd", 0, &[&be32(1)[..], &mp4a].concat());
    let stts = full(b"stts", 0, &[be32(1), be32(count), be32(track.frame_samples)].concat());
    let stsc = full(b"stsc", 0, &[be32(1), be32(1), be32(count), be32(1)].concat());
    let sizes: Vec<u8> = track.frames.iter().flat_map(|f| (f.len() as u32).to_be_bytes()).collect();
    let stsz = full(b"stsz", 0, &[&be32(0)[..], &be32(count), &sizes].concat());
    // Sample numbers are 1-based. Left out when every frame is a sync frame, which is what
    // the absence of the box means.
    let stss = match track.sync_frames {
        Some(sync) if sync.len() != track.frames.len() => {
            let numbers: Vec<u8> = sync.iter().flat_map(|i| (i + 1).to_be_bytes()).collect();
            full(b"stss", 0, &[&be32(sync.len() as u32)[..], &numbers].concat())
        }
        _ => Vec::new(),
    };

    // `stco` holds where the audio starts, which depends on the size of the index it is in:
    // built once to measure, once for real.
    let moov = |audio_offset: u32| {
        let stco = full(b"stco", 0, &[be32(1), be32(audio_offset)].concat());
        let stbl = boxed(b"stbl", &[&stsd[..], &stts, &stss, &stsc, &stsz, &stco].concat());
        let minf = boxed(b"minf", &[&smhd[..], &dinf, &stbl].concat());
        let mdia = boxed(b"mdia", &[&mdhd[..], &hdlr, &minf].concat());
        boxed(b"moov", &[&mvhd[..], &boxed(b"trak", &[&tkhd[..], &mdia].concat())].concat())
    };
    let index = moov((ftyp.len() + moov(0).len() + 8) as u32);

    let audio: usize = track.frames.iter().map(Vec::len).sum();
    let mut out = Vec::with_capacity(ftyp.len() + index.len() + 8 + audio);
    out.extend(ftyp);
    out.extend(index);
    out.extend(((audio + 8) as u32).to_be_bytes());
    out.extend(b"mdat");
    for frame in track.frames {
        out.extend(frame);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The boxes directly inside `data`: (type, body).
    fn boxes(data: &[u8]) -> Vec<(String, &[u8])> {
        let mut out = Vec::new();
        let mut at = 0;
        while at + 8 <= data.len() {
            let len = u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as usize;
            assert!(len >= 8 && at + len <= data.len(), "a box runs past its parent");
            out.push((String::from_utf8_lossy(&data[at + 4..at + 8]).into_owned(), &data[at + 8..at + len]));
            at += len;
        }
        assert_eq!(at, data.len(), "trailing bytes after the last box");
        out
    }

    fn child<'a>(data: &'a [u8], path: &[&str]) -> &'a [u8] {
        path.iter().fold(data, |inside, name| boxes(inside).into_iter().find(|(k, _)| k == name).unwrap_or_else(|| panic!("no {name} box")).1)
    }

    #[test]
    fn the_index_points_at_the_audio_and_lists_every_frame() {
        let frames = vec![vec![0x80, 1, 2], vec![0x00, 3], vec![0x80, 4, 5, 6]];
        let sync = xhe_sync_frames(&frames);
        assert_eq!(sync, [0, 2]);
        let file = write(&Track { frames: &frames, config: &[0x12, 0x10], sample_rate: 44_100, channels: 2, frame_samples: 1024, bitrate: 128_000, sync_frames: Some(&sync) });
        let top = boxes(&file);
        assert_eq!(top.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), ["ftyp", "moov", "mdat"], "index before audio");
        assert_eq!(top[2].1, [0x80, 1, 2, 0x00, 3, 0x80, 4, 5, 6]);

        let stbl = child(&file, &["moov", "trak", "mdia", "minf", "stbl"]);
        let stco = child(stbl, &["stco"]);
        let offset = u32::from_be_bytes(stco[8..12].try_into().unwrap()) as usize;
        assert_eq!(&file[offset..offset + 3], [0x80, 1, 2], "stco is the first frame's position in the file");
        let stsz = child(stbl, &["stsz"]);
        assert_eq!(&stsz[8..], [0, 0, 0, 3, 0, 0, 0, 3, 0, 0, 0, 2, 0, 0, 0, 4], "count, then each size");
        let stss = child(stbl, &["stss"]);
        assert_eq!(&stss[4..], [0, 0, 0, 2, 0, 0, 0, 1, 0, 0, 0, 3], "sync frames, 1-based");
        let mdhd = child(&file, &["moov", "trak", "mdia", "mdhd"]);
        assert_eq!(u32::from_be_bytes(mdhd[12..16].try_into().unwrap()), 44_100, "timescale is the sample rate");
        assert_eq!(u32::from_be_bytes(mdhd[16..20].try_into().unwrap()), 3 * 1024, "duration in samples");
    }

    #[test]
    fn a_stream_where_every_frame_is_a_sync_frame_has_no_sync_table() {
        let frames = vec![vec![1u8, 2], vec![3]];
        for sync in [None, Some(&[0u32, 1][..])] {
            let file = write(&Track { frames: &frames, config: &[0x12, 0x10], sample_rate: 48_000, channels: 2, frame_samples: 1024, bitrate: 96_000, sync_frames: sync });
            let stbl = child(&file, &["moov", "trak", "mdia", "minf", "stbl"]);
            assert!(!boxes(stbl).iter().any(|(k, _)| k == "stss"));
        }
    }

    /// The real thing: encode, wrap, and have an independent decoder read it back.
    #[test]
    fn an_encoded_aac_file_decodes_back_to_the_same_length_and_level() {
        use crate::rom::xaac;
        if !xaac::available() {
            eprintln!("skipped: rxaac is not built for this machine");
            return;
        }
        let rate = 44_100u32;
        let pcm: Vec<i16> = (0..rate * 3).flat_map(|n| { let v = ((std::f32::consts::TAU * 440.0 * n as f32 / rate as f32).sin() * 12000.0) as i16; [v, v] }).collect();
        let encoded = xaac::encode_all(&pcm, rate, 2, 128_000, xaac::Format::Aac).unwrap();
        let file = write(&Track { frames: &encoded.frames, config: &encoded.config, sample_rate: rate, channels: 2, frame_samples: encoded.frame_samples, bitrate: 128_000, sync_frames: None });

        let decoded = crate::rom::audio_decode::decode_bytes(file, Some("m4a")).expect("symphonia reads the file");
        assert_eq!((decoded.sample_rate, decoded.channels), (rate, 2));
        let seconds = decoded.pcm.len() as f64 / 2.0 / f64::from(rate);
        assert!((3.0..3.1).contains(&seconds), "{seconds} s for 3 s in");
        // Skip the encoder's start-up; the rest is a sine of amplitude 12000 (RMS 8485).
        let body = &decoded.pcm[decoded.pcm.len() / 4..decoded.pcm.len() * 3 / 4];
        let rms = (body.iter().map(|&s| f64::from(s) * f64::from(s)).sum::<f64>() / body.len() as f64).sqrt();
        assert!((8000.0..9000.0).contains(&rms), "RMS {rms:.0}");

        // xHE-AAC: no decoder here to read it back, so what is checked is that the stream
        // marks frames a player can seek to, and not all of them.
        let xhe = xaac::encode_all(&pcm, rate, 2, 128_000, xaac::Format::XheAac).unwrap();
        let sync = xhe_sync_frames(&xhe.frames);
        eprintln!("xHE-AAC: {} frames, {} sync, first at {:?}", xhe.frames.len(), sync.len(), &sync[..sync.len().min(6)]);
        assert_eq!(sync.first(), Some(&0), "the stream starts on a frame a decoder can start at");
    }
}
