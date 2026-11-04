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
