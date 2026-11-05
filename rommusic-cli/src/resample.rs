//! Changing a track's sample rate (`--rate`) and writing it at 16 or 24 bits.
//!
//! A windowed-sinc filter (Blackman window, 32 zero crossings each side), read from a table
//! of 1024 phases with linear interpolation between them. That is far below what the
//! sources' own noise floor shows, and keeps a five-minute track to a second or two.

use std::f64::consts::PI;

const ZEROS: usize = 32;
const PHASES: usize = 1024;

/// The filter's right half, at `PHASES` points per zero crossing, plus one for interpolation.
fn table() -> &'static [f32] {
    static TABLE: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let n = ZEROS * PHASES + 1;
        (0..=n)
            .map(|i| {
                let x = i as f64 / PHASES as f64;
                if x >= ZEROS as f64 {
                    return 0.0;
                }
                let sinc = if i == 0 { 1.0 } else { (PI * x).sin() / (PI * x) };
                // Blackman over [-ZEROS, ZEROS], evaluated on its right half.
                let w = x / ZEROS as f64;
                let window = 0.42 + 0.5 * (PI * w).cos() + 0.08 * (2.0 * PI * w).cos();
                (sinc * window) as f32
            })
            .collect()
    })
}

fn kernel(table: &[f32], x: f64) -> f32 {
    let x = x.abs() * PHASES as f64;
    let i = x as usize;
    if i + 1 >= table.len() {
        return 0.0;
    }
    let frac = (x - i as f64) as f32;
    table[i] + (table[i + 1] - table[i]) * frac
}

/// `input` (interleaved, `channels` wide, at `from` Hz) at `to` Hz. Going down, the filter is
/// widened so nothing above the new Nyquist frequency folds back.
pub fn resample(input: &[f32], channels: usize, from: u32, to: u32) -> Vec<f32> {
    let frames = input.len() / channels;
    let table = table();
    let step = f64::from(from) / f64::from(to);
    // Cutoff relative to the input's Nyquist: below 1 when going down, with a little margin.
    let scale = if to < from { f64::from(to) / f64::from(from) * 0.97 } else { 1.0 };
    let reach = (ZEROS as f64 / scale).ceil() as isize;
    let out_frames = ((frames as f64) / step).floor() as usize;
    let mut out = vec![0.0f32; out_frames * channels];
    let mut weights = Vec::with_capacity(2 * reach as usize + 1);
    for n in 0..out_frames {
        let t = n as f64 * step;
        let center = t.floor() as isize;
        weights.clear();
        let mut sum = 0.0f32;
        for k in (center - reach + 1)..=(center + reach) {
            let w = kernel(table, (t - k as f64) * scale) * scale as f32;
            weights.push((k, w));
            sum += w;
        }
        // Normalised, so DC stays exactly where it was (it also absorbs the table's rounding).
        let norm = if sum.abs() > 1e-6 { 1.0 / sum } else { 1.0 };
        for c in 0..channels {
            let mut acc = 0.0f32;
            for &(k, w) in &weights {
                if k >= 0 && (k as usize) < frames {
                    acc += input[k as usize * channels + c] * w;
                }
            }
            out[n * channels + c] = acc * norm;
        }
    }
    out
}

/// Samples in [-1, 1) as integers of `bits` bits. 16-bit output gets triangular dither, since
/// the filter's result has more resolution than 16 bits hold; 24 bits needs none.
pub fn quantize(samples: &[f32], bits: u32) -> Vec<i32> {
    let full = (1i64 << (bits - 1)) as f32;
    let (lo, hi) = (-full, full - 1.0);
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut noise = || {
        // xorshift: cheap, and plenty for dither.
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 40) as f32 / (1u64 << 24) as f32
    };
    samples
        .iter()
        .map(|&s| {
            let dither = if bits <= 16 { noise() - noise() } else { 0.0 };
            (s * full + dither).round().clamp(lo, hi) as i32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, hz: f64, frames: usize) -> Vec<f32> {
        (0..frames).flat_map(|i| {
            let v = (2.0 * PI * hz * i as f64 / f64::from(rate)).sin() as f32 * 0.5;
            [v, v]
        }).collect()
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }

    #[test]
    fn a_tone_keeps_its_level_and_length() {
        let input = tone(32_000, 1_000.0, 32_000);
        let out = resample(&input, 2, 32_000, 48_000);
        assert_eq!(out.len() / 2, 48_000);
        // Away from the edges, the level is the same.
        let middle = &out[2 * 4_000..2 * 44_000];
        assert!((peak(middle) - 0.5).abs() < 0.01, "{}", peak(middle));
    }

    #[test]
    fn going_down_removes_what_no_longer_fits() {
        // 30 kHz cannot exist at 44.1 kHz: it has to be filtered out, not folded to 14.1 kHz.
        let input = tone(96_000, 30_000.0, 96_000);
        let out = resample(&input, 2, 96_000, 44_100);
        let middle = &out[2 * 4_000..2 * 40_000];
        assert!(peak(middle) < 0.01, "{}", peak(middle));
    }

    #[test]
    fn quantizing_maps_full_scale_and_clamps() {
        assert_eq!(quantize(&[0.0, 0.5, -1.0, 1.5], 24), vec![0, 1 << 22, -(1 << 23), (1 << 23) - 1]);
        let q = quantize(&[0.25; 1000], 16);
        assert!(q.iter().all(|&v| (8191..=8193).contains(&v)));
    }
}
