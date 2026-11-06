//! Decoding audio files to 16-bit PCM with symphonia (pure Rust), for conversion and for
//! loudness measurement. MP3, AAC, ALAC, FLAC, WAV, AIFF and Vorbis; not WMA, APE or DSD
//! (DSD goes through rdsd2pcm first, see `api::audio_convert`).

use std::io::Cursor;
use std::path::Path;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// A whole track as interleaved 16-bit samples.
pub struct Decoded {
    pub pcm: Vec<i16>,
    pub sample_rate: u32,
    pub channels: u16,
}

/// Decodes the file at `path`. An error is a reason, for the log and for the "unsupported"
/// state: an unknown container, a codec this build has no decoder for, or nothing decodable.
pub fn decode_file(path: &Path) -> Result<Decoded, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    decode(Box::new(file), path.extension().and_then(|e| e.to_str()))
}

/// [`decode_file`] for bytes already in memory.
pub fn decode_bytes(bytes: Vec<u8>, extension: Option<&str>) -> Result<Decoded, String> {
    decode(Box::new(Cursor::new(bytes)), extension)
}

fn decode(source: Box<dyn MediaSource>, extension: Option<&str>) -> Result<Decoded, String> {
    let mut out = Decoded { pcm: Vec::new(), sample_rate: 0, channels: 0 };
    let (sample_rate, channels) = decode_with(source, extension, |pcm, _, _| {
        out.pcm.extend_from_slice(pcm);
        Ok(())
    })?;
    out.sample_rate = sample_rate;
    out.channels = channels;
    Ok(out)
}

/// Decodes the file at `path` a packet at a time, handing each run of interleaved samples
/// to `sink` with the stream's sample rate and channel count. For converting and measuring
/// without holding a whole track in memory: an hour of stereo is over 600 MB as PCM.
/// Returns the sample rate and channel count.
pub fn decode_file_with(path: &Path, sink: impl FnMut(&[i16], u32, u16) -> Result<(), String>) -> Result<(u32, u16), String> {
    let file = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    decode_with(Box::new(file), path.extension().and_then(|e| e.to_str()), sink)
}

fn decode_with(source: Box<dyn MediaSource>, extension: Option<&str>, mut sink: impl FnMut(&[i16], u32, u16) -> Result<(), String>) -> Result<(u32, u16), String> {
    let mut hint = Hint::new();
    if let Some(ext) = extension {
        hint.with_extension(ext);
    }
    let stream = MediaSourceStream::new(source, Default::default());
    let probed = symphonia::default::get_probe().format(&hint, stream, &FormatOptions::default(), &MetadataOptions::default()).map_err(|e| format!("not a container this build reads: {e}"))?;
    let mut format = probed.format;
    let track = format.tracks().iter().find(|t| t.codec_params.codec != CODEC_TYPE_NULL).ok_or("no audio track")?.clone();
    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()).map_err(|e| format!("no decoder for this codec: {e}"))?;

    let (mut sample_rate, mut channels, mut any) = (track.codec_params.sample_rate.unwrap_or(0), 0u16, false);
    let mut buffer: Option<SampleBuffer<i16>> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            // The end of the stream arrives as an I/O error.
            Err(SymphoniaError::IoError(_)) | Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(format!("reading: {e}")),
        };
        if packet.track_id() != track.id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                // A stream that changes shape midway cannot be one encoded stream.
                if any && (spec.rate != sample_rate || spec.channels.count() as u16 != channels) {
                    return Err("the sample rate or channel count changes inside the file".to_string());
                }
                sample_rate = spec.rate;
                channels = spec.channels.count() as u16;
                // Sized for the largest packet seen: a decoder may grow its frames.
                if buffer.as_ref().is_none_or(|b| b.capacity() < decoded.capacity() * spec.channels.count()) {
                    buffer = Some(SampleBuffer::new(decoded.capacity() as u64, spec));
                }
                let buffer = buffer.as_mut().expect("just set");
                buffer.copy_interleaved_ref(decoded);
                any |= !buffer.samples().is_empty();
                sink(buffer.samples(), sample_rate, channels)?;
            }
            // One damaged packet is skipped, as every player does.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("decoding: {e}")),
        }
    }
    if !any || channels == 0 || sample_rate == 0 {
        return Err("nothing decodable in the file".to_string());
    }
    Ok((sample_rate, channels))
}

/// The loudness a player normalises to, in LUFS. The same figure an xHE-AAC stream declares
/// (`xaac::DECLARED_LOUDNESS_LKFS`) and the target Windows, Apple and Android decoders use.
pub const LOUDNESS_TARGET_LUFS: f32 = -16.0;

/// A file's integrated loudness (EBU R128 / ITU-R BS.1770) in LUFS, decoding it once from
/// start to end. `None` for a file this build cannot decode, and for one too short or too
/// quiet to have a loudness (the measurement is gated: digital silence has none).
pub fn loudness_lufs(path: &Path) -> Option<f32> {
    measure(path)?.lufs
}

/// A sample this small or smaller (-60 dBFS) counts as silence for [`Measured::sound_end_ms`].
const SILENT_SAMPLE: u16 = 32;

/// What one decode of a file tells.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measured {
    /// Integrated loudness, as [`loudness_lufs`].
    pub lufs: Option<f32>,
    /// Where the last sound ends, in ms: what follows is the file's silent end.
    pub sound_end_ms: u32,
}

/// Decodes a file once for its loudness and where its sound ends. `None` when this build
/// cannot decode it.
pub fn measure(path: &Path) -> Option<Measured> {
    let mut meter: Option<ebur128::EbuR128> = None;
    let (mut frames, mut last_sound, mut rate_seen) = (0u64, 0u64, 0u32);
    decode_file_with(path, |pcm, rate, channels| {
        if meter.is_none() {
            meter = Some(ebur128::EbuR128::new(u32::from(channels), rate, ebur128::Mode::I).map_err(|e| format!("loudness meter: {e}"))?);
        }
        rate_seen = rate;
        for frame in pcm.chunks(usize::from(channels.max(1))) {
            frames += 1;
            if frame.iter().any(|v| v.unsigned_abs() > SILENT_SAMPLE) {
                last_sound = frames;
            }
        }
        meter.as_mut().expect("just set").add_frames_i16(pcm).map_err(|e| format!("loudness meter: {e}"))
    })
    .ok()?;
    let lufs = meter.and_then(|m| m.loudness_global().ok()).filter(|l| l.is_finite()).map(|l| l as f32);
    let sound_end_ms = if rate_seen > 0 { (last_sound * 1000 / u64::from(rate_seen)).min(u64::from(u32::MAX)) as u32 } else { 0 };
    Some(Measured { lufs, sound_end_ms })
}

/// What a player multiplies its volume by to bring a track of `lufs` to the target. Never
/// above 1: a quiet track is left as it is rather than amplified into clipping, so
/// normalising only ever turns the loud ones down.
pub fn normalise_gain(lufs: f32) -> f32 {
    10f32.powf((LOUDNESS_TARGET_LUFS - lufs) / 20.0).min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(amplitude: f32, seconds: u32) -> Vec<u8> {
        let rate = 44_100u32;
        let frames = rate * seconds;
        let mut out = Vec::new();
        out.extend(b"RIFF");
        out.extend((36 + frames * 4).to_le_bytes());
        out.extend(b"WAVEfmt ");
        out.extend(16u32.to_le_bytes());
        out.extend(1u16.to_le_bytes());
        out.extend(2u16.to_le_bytes());
        out.extend(rate.to_le_bytes());
        out.extend((rate * 4).to_le_bytes());
        out.extend(4u16.to_le_bytes());
        out.extend(16u16.to_le_bytes());
        out.extend(b"data");
        out.extend((frames * 4).to_le_bytes());
        for n in 0..frames {
            let v = ((std::f32::consts::TAU * 1000.0 * n as f32 / rate as f32).sin() * amplitude * 32767.0) as i16;
            out.extend(v.to_le_bytes());
            out.extend(v.to_le_bytes());
        }
        out
    }

    #[test]
    fn loudness_follows_the_level_and_silence_has_none() {
        let dir = std::env::temp_dir().join(format!("rx-loudness-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let measure = |name: &str, amplitude: f32| {
            let path = dir.join(name);
            std::fs::write(&path, wav(amplitude, 3)).unwrap();
            loudness_lufs(&path)
        };
        // A full-scale 1 kHz sine on both channels is -3.01 dBFS RMS per channel; BS.1770 sums
        // the two channels' power, which lands a stereo sine at about -3 LUFS... and one a
        // tenth the amplitude exactly 20 LU below it. The difference is what must hold.
        let loud = measure("loud.wav", 0.5).expect("a tone has a loudness");
        let quiet = measure("quiet.wav", 0.05).expect("a quieter tone has one too");
        assert!((loud - quiet - 20.0).abs() < 0.2, "{loud} vs {quiet}");
        assert!((-12.0..-5.0).contains(&loud), "half-scale stereo 1 kHz: {loud} LUFS");
        assert_eq!(measure("silence.wav", 0.0), None);
        std::fs::write(dir.join("junk.wma"), b"nothing").unwrap();
        assert_eq!(loudness_lufs(&dir.join("junk.wma")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn normalising_only_ever_turns_down() {
        assert!((normalise_gain(-16.0) - 1.0).abs() < 1e-6);
        assert!((normalise_gain(-10.0) - 0.501).abs() < 0.001, "6 dB over the target: half");
        assert!((normalise_gain(-6.0) - 0.316).abs() < 0.001);
        assert_eq!(normalise_gain(-30.0), 1.0, "a quiet track is not amplified");
    }
}
