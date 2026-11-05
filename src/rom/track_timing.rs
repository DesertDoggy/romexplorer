//! How long a rendered track really is, from its sound: where it falls silent, and where it
//! starts repeating itself. For chip music with no length of its own (or a playlist length
//! that is only a guess), which otherwise plays a short phrase over and over, or seconds of
//! silence, until a fixed cut-off.
//!
//! The audio is reduced to the loudness of three bands -- low, middle and high -- per block of
//! about 0.1 s, and a loop is a period over which that envelope repeats, twice over, from some
//! point to the end of what was rendered. Loudness and not samples: a noise channel's drums
//! never repeat sample for sample (its generator runs on through the loop), but they do in
//! level. Three bands and not one: a passage at an even level matches any period by loudness
//! alone, while its notes still move the balance between the bands; and a stretch too even to
//! tell periods apart by is not taken for a loop at all.

/// Blocks per second of the envelope.
const BLOCKS_PER_SECOND: u32 = 10;
/// Below this a sample counts as silence.
const SILENCE: i32 = 64;
/// Silence this long after the last sound ends a track.
pub const SILENT_END_MS: u64 = 4_000;
/// What is left of the silence after a track's last sound.
const TAIL_MS: u64 = 500;
/// The shortest loop looked for.
const MIN_LOOP_MS: u64 = 1_000;
/// How alike two stretches must be to count as the same: one minus the correlation of their
/// band envelopes.
const SAME: f32 = 0.10;
/// A loop this alike is [`Timing::Loops::sure`]: enough to overrule a playlist's length.
const SURE: f32 = 0.05;
/// The same, for the short window that finds where the loop begins.
const SAME_LOCAL: f32 = 0.25;
/// That window, in ms.
const LOCAL_WINDOW_MS: u64 = 3_000;
/// The least that is compared to accept a loop: the last this many ms (or a whole period,
/// when that is longer) against the same a period back and two periods back.
const COMPARED_MS: u64 = 8_000;
/// A loop at least this long may be accepted from one period's match, when two do not fit in
/// what was rendered.
const LONG_LOOP_MS: u64 = 30_000;
/// How much the level must move within what is compared for periods to be told apart
/// (standard deviation over the mean).
const MIN_MOVEMENT: f32 = 0.12;
/// Silence after a fade-out: a track that stops the moment its fade does sounds cut off.
pub const FADE_TAIL_MS: u64 = 500;
/// How far a fade-out falls, in dB, before the silence.
const FADE_DEPTH_DB: f32 = 60.0;

/// The gain of a fade-out of `fade` frames followed by `tail` frames of silence, `remaining`
/// frames before the end. Even in decibels, as an ear hears a fade (and as gme fades): a
/// straight line in amplitude stays loud until the last moment.
pub fn fade_gain(remaining: u64, fade: u64, tail: u64) -> f32 {
    if remaining <= tail {
        return 0.0;
    }
    let into = remaining - tail;
    if fade == 0 || into >= fade {
        return 1.0;
    }
    10f32.powf(-FADE_DEPTH_DB / 20.0 * (1.0 - into as f32 / fade as f32))
}

/// Band edges, in Hz.
const LOW_HZ: f32 = 300.0;
const HIGH_HZ: f32 = 2_500.0;

/// One block of the envelope: the RMS of the low, middle and high bands.
type Block = [f32; 3];

/// Collects the envelope of interleaved stereo audio as it is rendered.
pub struct Listener {
    rate: u32,
    block: usize,
    blocks: Vec<Block>,
    // The block being filled.
    sum: [f64; 3],
    frames: usize,
    // The two one-pole low-passes the bands come from.
    low: f32,
    mid: f32,
    a_low: f32,
    a_mid: f32,
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
    /// Repeats from `start_ms`, every `loop_ms`. `sure`: the copies match closely.
    Loops { start_ms: u64, loop_ms: u64, sure: bool },
    /// Still going and not found to repeat.
    RunsOn,
}

impl Listener {
    pub fn new(rate: u32) -> Self {
        let rate = rate.max(1);
        let pole = |hz: f32| 1.0 - (-2.0 * std::f32::consts::PI * hz / rate as f32).exp();
        Self {
            rate,
            block: (rate / BLOCKS_PER_SECOND).max(1) as usize,
            blocks: Vec::new(),
            sum: [0.0; 3],
            frames: 0,
            low: 0.0,
            mid: 0.0,
            a_low: pole(LOW_HZ),
            a_mid: pole(HIGH_HZ),
            done_frames: 0,
            last_sound_frame: 0,
        }
    }

    /// Feeds interleaved stereo samples.
    pub fn feed(&mut self, stereo: &[i16]) {
        for pair in stereo.chunks_exact(2) {
            let (l, r) = (i32::from(pair[0]), i32::from(pair[1]));
            if l.abs() > SILENCE || r.abs() > SILENCE {
                self.last_sound_frame = self.done_frames + 1;
            }
            let m = (l + r) as f32 / 2.0;
            self.low += self.a_low * (m - self.low);
            self.mid += self.a_mid * (m - self.mid);
            let bands = [self.low, self.mid - self.low, m - self.mid];
            for (sum, b) in self.sum.iter_mut().zip(bands) {
                *sum += f64::from(b) * f64::from(b);
            }
            self.frames += 1;
            self.done_frames += 1;
            if self.frames == self.block {
                let n = self.block as f64;
                self.blocks.push(self.sum.map(|s| (s / n).sqrt() as f32));
                (self.sum, self.frames) = ([0.0; 3], 0);
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
        let block_ms = 1000 / u64::from(BLOCKS_PER_SECOND);
        let blocks = |ms: u64| ms.div_ceil(block_ms) as usize;
        match find_loop(&self.blocks, blocks(MIN_LOOP_MS), blocks(COMPARED_MS)) {
            Some((start, period, score)) => Timing::Loops { start_ms: start as u64 * block_ms, loop_ms: period as u64 * block_ms, sure: score < SURE },
            None => Timing::RunsOn,
        }
    }
}

/// `(first block of the loop, its length in blocks, how alike its copies are)`: the shortest
/// period over which the envelope's end repeats twice over, and how far back that goes.
///
/// The envelope compared is the overall level, one value per block, by correlation. Not the
/// bands one by one: a noise or sample channel jitters from copy to copy in its own band,
/// enough to hide a loop that repeats exactly in what is heard; and not a difference, which
/// counts that same jitter against every block.
fn find_loop(blocks: &[Block], min_period: usize, compared: usize) -> Option<(usize, usize, f32)> {
    let n = blocks.len();
    let per_second = BLOCKS_PER_SECOND as usize;
    // Shorter than this, a loop is always checked over two periods.
    let long = (LONG_LOOP_MS as usize * per_second) / 1000;
    let min_period = min_period.max(1);
    if n < compared + 2 * min_period {
        return None;
    }
    let level: Vec<f32> = blocks.iter().map(|b| (b[0] * b[0] + b[1] * b[1] + b[2] * b[2]).sqrt()).collect();
    // One minus the correlation of `from..to` with the same stretch `back` blocks earlier.
    let unlike = |from: usize, to: usize, back: usize| {
        let len = (to - from) as f32;
        let (mut sx, mut sy) = (0f32, 0f32);
        for i in from..to {
            sx += level[i];
            sy += level[i - back];
        }
        let (mx, my) = (sx / len, sy / len);
        let (mut cov, mut vx, mut vy) = (0f32, 0f32, 0f32);
        for i in from..to {
            let (x, y) = (level[i] - mx, level[i - back] - my);
            cov += x * y;
            vx += x * x;
            vy += y * y;
        }
        if vx <= 0.0 || vy <= 0.0 {
            return 1.0;
        }
        1.0 - cov / (vx * vy).sqrt()
    };
    let moves = |from: usize, to: usize| {
        let m = level[from..to].iter().sum::<f32>() / (to - from) as f32;
        let var = level[from..to].iter().map(|v| (v - m).powi(2)).sum::<f32>() / (to - from) as f32;
        var.sqrt() / m.max(1e-6) >= MIN_MOVEMENT
    };
    let (period, score) = (min_period..=(n - compared) / 2).find_map(|p| {
        let w = p.max(compared);
        if w + p > n || !moves(n - w, n) {
            return None;
        }
        if w + 2 * p <= n {
            let d = unlike(n - w, n, p).max(unlike(n - w, n, 2 * p));
            return (d < SAME).then_some((p, d));
        }
        // A loop too long to be heard three times in what was rendered: one whole period
        // matching the one before it, closely, will do.
        let d = unlike(n - w, n, p);
        (p >= long && d < SURE).then_some((p, d))
    })?;
    // Walking back from the end, the loop starts where a stretch of a few seconds stops
    // matching its copy one period later.
    let window = (LOCAL_WINDOW_MS as usize * per_second / 1000).clamp(2, period);
    let mut start = 0;
    let mut i = n - period;
    while i >= window {
        if unlike(i - window + period, i + period, period) >= SAME_LOCAL {
            // Within that stretch, the loop begins after the last block clearly unlike its copy.
            let mean = level.iter().sum::<f32>() / n as f32;
            let differs = |k: usize| (level[k] - level[k + period]).abs() > 0.5 * level[k].max(level[k + period]).max(0.1 * mean);
            start = (i - window..i).rev().find(|&k| differs(k)).map_or(i - window, |k| k + 1);
            break;
        }
        i -= 1;
    }
    // A loop that starts too late to be heard twice was not heard to loop; and a short beat
    // that only settles in after minutes of music is a passage keeping time, not the track's
    // loop.
    if start + 2 * period > n || (start > 20 * period && start > 60 * per_second) {
        return None;
    }
    Some((start, period, score))
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
            Timing::Loops { start_ms, loop_ms, .. } => {
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
            Timing::Loops { start_ms, loop_ms, .. } => {
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
    fn a_fade_falls_evenly_in_decibels_then_is_silent() {
        assert_eq!(fade_gain(2_000, 1_000, 100), 1.0);
        assert!((fade_gain(600, 1_000, 100) - 10f32.powf(-1.5)).abs() < 1e-4);
        assert!(fade_gain(101, 1_000, 100) < 0.0011);
        assert_eq!(fade_gain(50, 1_000, 100), 0.0);
    }

    #[test]
    fn a_long_loop_heard_not_quite_three_times_is_found() {
        let mut x = 9u32;
        let notes: Vec<(f32, f32)> = (0..40).map(|_| {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (200.0 + (x >> 16) as f32 % 800.0, 0.2 + ((x >> 8) & 0xFF) as f32 / 320.0)
        }).collect();
        match listen(melody(&notes, RATE as usize), 100).finish(false) {
            Timing::Loops { loop_ms, start_ms, .. } => {
                assert!((39_900..=40_100).contains(&loop_ms), "{loop_ms}");
                assert!(start_ms < 1_000, "{start_ms}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_even_tone_is_not_a_loop() {
        let l = listen(melody(&[(440.0, 0.7)], RATE as usize), 30);
        assert_eq!(l.finish(false), Timing::RunsOn);
    }

    #[test]
    fn a_short_beat_after_minutes_of_music_is_not_a_loop() {
        let mut x = 3u32;
        let notes: Vec<(f32, f32)> = (0..400).map(|_| {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (200.0 + (x >> 16) as f32 % 800.0, 0.2 + ((x >> 8) & 0xFF) as f32 / 320.0)
        }).collect();
        let intro: Vec<i16> = melody(&notes, RATE as usize / 4).take(RATE as usize * 2 * 90).collect();
        let l = listen(intro.into_iter().chain(melody(&[(440.0, 1.0), (330.0, 0.3)], RATE as usize / 2)), 110);
        assert_eq!(l.finish(false), Timing::RunsOn);
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
