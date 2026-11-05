//! How long a rendered track really is, from its sound: where it falls silent, and where it
//! starts repeating itself. For chip music with no length of its own (or a playlist length
//! that is only a guess), which otherwise plays a short phrase over and over, or seconds of
//! silence, until a fixed cut-off.
//!
//! The audio is reduced to its loudness (RMS) per block of [`BLOCK`] frames, and a loop is a
//! period over which that envelope repeats from some point to the end of what was rendered.
//! Only loudness: a noise channel's drums never repeat sample for sample (its generator runs
//! on through the loop), but they do repeat in level, and blocks of about 0.1 s average the
//! rest of the noise out.

/// Frames per block of the envelope: about 93 ms at 44.1 kHz.
pub const BLOCK: usize = 4096;
/// Below this a sample counts as silence.
const SILENCE: i32 = 64;
/// Silence this long after the last sound ends a track.
pub const SILENT_END_MS: u64 = 4_000;
/// What is left of the silence after a track's last sound.
const TAIL_MS: u64 = 500;
/// The shortest loop looked for.
const MIN_LOOP_MS: u64 = 1_000;
/// How alike two stretches must be (mean difference, relative to the mean level) to count as
/// the same.
const SAME: f32 = 0.08;
/// The same, for the short window that finds where the loop begins.
const SAME_LOCAL: f32 = 0.15;
/// The least that is compared to accept a loop: the last this many ms against a period back
/// (or half the period, when that is longer).
const COMPARED_MS: f64 = 8_000.0;

/// One block of the envelope.
type Block = f32;

/// Collects the envelope of interleaved stereo audio as it is rendered.
pub struct Listener {
    rate: u32,
    blocks: Vec<Block>,
    // The block being filled.
    sum: f64,
    frames: usize,
    done_frames: u64,
    last_sound_frame: u64,
}

/// What [`Listener::finish`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timing {
    /// Nothing audible at all.
    Silent,
    /// Ends by itself: play this long and stop.
    Ends { ms: u64 },
    /// Repeats from `start_ms`, every `loop_ms`.
    Loops { start_ms: u64, loop_ms: u64 },
    /// Still going and not found to repeat.
    RunsOn,
}

impl Listener {
    pub fn new(rate: u32) -> Self {
        Self { rate: rate.max(1), blocks: Vec::new(), sum: 0.0, frames: 0, done_frames: 0, last_sound_frame: 0 }
    }

    /// Feeds interleaved stereo samples.
    pub fn feed(&mut self, stereo: &[i16]) {
        for pair in stereo.chunks_exact(2) {
            let (l, r) = (i32::from(pair[0]), i32::from(pair[1]));
            if l.abs() > SILENCE || r.abs() > SILENCE {
                self.last_sound_frame = self.done_frames + 1;
            }
            let m = (l + r) / 2;
            self.sum += f64::from(m) * f64::from(m);
            self.frames += 1;
            self.done_frames += 1;
            if self.frames == BLOCK {
                self.blocks.push((self.sum / BLOCK as f64).sqrt() as f32);
                (self.sum, self.frames) = (0.0, 0);
            }
        }
    }

    pub fn played_ms(&self) -> u64 {
        self.done_frames * 1000 / u64::from(self.rate)
    }

    /// Whether the track has been silent for [`SILENT_END_MS`] after making a sound (or never
    /// made one in that long): rendering can stop.
    pub fn fell_silent(&self) -> bool {
        (self.done_frames - self.last_sound_frame) * 1000 / u64::from(self.rate) >= SILENT_END_MS
    }

    /// `ended`: the player stopped by itself, or [`fell_silent`](Self::fell_silent).
    pub fn finish(&self, ended: bool) -> Timing {
        if self.last_sound_frame == 0 {
            return Timing::Silent;
        }
        if ended || self.fell_silent() {
            return Timing::Ends { ms: self.last_sound_frame * 1000 / u64::from(self.rate) + TAIL_MS };
        }
        let block_ms = BLOCK as f64 * 1000.0 / f64::from(self.rate);
        match find_loop(&self.blocks, (MIN_LOOP_MS as f64 / block_ms).ceil() as usize, (COMPARED_MS / block_ms).ceil() as usize) {
            Some((start, period)) => Timing::Loops { start_ms: (start as f64 * block_ms) as u64, loop_ms: (period as f64 * block_ms) as u64 },
            None => Timing::RunsOn,
        }
    }
}

/// `(first block of the loop, its length in blocks)`: the shortest period over which the
/// envelope's end repeats, and how far back that repetition goes.
fn find_loop(blocks: &[Block], min_period: usize, compared: usize) -> Option<(usize, usize)> {
    let n = blocks.len();
    let min_period = min_period.max(1);
    if n < min_period + compared {
        return None;
    }
    let mean = (blocks.iter().sum::<f32>() / n as f32).max(1e-6);
    let differ = |i: usize, j: usize| (blocks[i] - blocks[j]).abs() / mean;
    let period = (min_period..=n - compared).find(|&p| {
        // The last stretch (half a period, at least `compared`) against the one a period back.
        let w = (p / 2).max(compared).min(n - p);
        (n - w..n).map(|i| differ(i, i - p)).sum::<f32>() / (w as f32) < SAME
    })?;
    // Walking back from the end, the loop starts after the last window that does not match
    // its copy one period later.
    let window = min_period.max(1);
    let mut start = 0;
    let mut i = n - period;
    while i >= window {
        let from = i - window;
        let d = (from..i).map(|k| differ(k, k + period)).sum::<f32>() / window as f32;
        if d >= SAME_LOCAL {
            // The loop begins after the last block in that window that differs from its copy.
            start = (from..i).rev().find(|&k| differ(k, k + period) >= SAME_LOCAL).map_or(i, |k| k + 1);
            break;
        }
        i -= 1;
    }
    Some((start, period))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44_100;

    /// A tone that changes pitch and level every `step` frames, through `notes`.
    fn melody(notes: &[(f32, f32)], step: usize) -> impl Iterator<Item = i16> + '_ {
        (0..).flat_map(move |i: usize| {
            let (hz, level) = notes[(i / step) % notes.len()];
            let v = ((i as f32 * 2.0 * std::f32::consts::PI * hz / RATE as f32).sin() * level * 20_000.0) as i16;
            [v, v]
        })
    }

    fn listen(samples: impl Iterator<Item = i16>, seconds: u64) -> Listener {
        let mut l = Listener::new(RATE);
        let all: Vec<i16> = samples.take((RATE as u64 * seconds * 2) as usize).collect();
        l.feed(&all);
        l
    }

    #[test]
    fn a_short_phrase_played_over_and_over_is_a_loop() {
        // Four notes of half a second: the phrase is 2 s long.
        let l = listen(melody(&[(440.0, 1.0), (660.0, 0.5), (330.0, 0.8), (550.0, 0.3)], RATE as usize / 2), 30);
        match l.finish(false) {
            Timing::Loops { start_ms, loop_ms } => {
                assert!((1_950..=2_050).contains(&loop_ms), "{loop_ms}");
                assert!(start_ms < 500, "{start_ms}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_intro_is_kept_before_the_loop() {
        let intro: Vec<i16> = melody(&[(200.0, 0.9), (900.0, 0.2), (250.0, 0.6), (700.0, 0.4)], RATE as usize).take(RATE as usize * 2 * 4).collect();
        let l = listen(intro.into_iter().chain(melody(&[(440.0, 1.0), (660.0, 0.5), (330.0, 0.8)], RATE as usize / 2)), 30);
        match l.finish(false) {
            Timing::Loops { start_ms, loop_ms } => {
                assert!((1_450..=1_550).contains(&loop_ms), "{loop_ms}");
                assert!((3_500..=4_600).contains(&start_ms), "{start_ms}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn drums_of_noise_that_never_repeat_do_not_hide_a_loop() {
        // A 6 s phrase with a noise channel over it whose samples never come round again.
        let mut x = 7u32;
        let phrase = melody(&[(440.0, 0.8), (660.0, 0.4), (330.0, 0.7), (550.0, 0.2), (300.0, 0.9), (500.0, 0.5)], RATE as usize).map(move |v| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            v.saturating_add(((x >> 20) as i16) - 2048)
        });
        match listen(phrase, 40).finish(false) {
            Timing::Loops { loop_ms, .. } => assert!((5_900..=6_100).contains(&loop_ms), "{loop_ms}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn music_that_does_not_repeat_runs_on() {
        // Every note different: a slow glide in pitch and level.
        let mut x = 1u32;
        let notes: Vec<(f32, f32)> = (0..200).map(|_| {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (200.0 + (x >> 16) as f32 % 800.0, 0.2 + ((x >> 8) & 0xFF) as f32 / 320.0)
        }).collect();
        let l = listen(melody(&notes, RATE as usize / 4), 40);
        assert_eq!(l.finish(false), Timing::RunsOn);
    }

    #[test]
    fn silence_after_the_music_ends_it() {
        let tune: Vec<i16> = melody(&[(440.0, 1.0)], RATE as usize).take(RATE as usize * 2 * 3).collect();
        let l = listen(tune.into_iter().chain(std::iter::repeat(0)), 10);
        assert!(l.fell_silent());
        assert_eq!(l.finish(false), Timing::Ends { ms: 3_000 + TAIL_MS });
        assert_eq!(listen(std::iter::repeat(0), 5).finish(false), Timing::Silent);
    }
}
