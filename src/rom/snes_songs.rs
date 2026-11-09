//! How a SNES game starts its songs, found by running it: the work RAM byte its code reads a
//! song request from, and the values that start each song. The game's own code then uploads
//! the song to the sound chip and starts it, as it does in play, so nothing of the sound
//! chip's memory has to be rebuilt.
//!
//! Found the way NES songs were before maps: after the boot, the bytes the game wrote just
//! before it sent the sound driver a command are the candidates; one that makes the game
//! send a command when changed is the request; each value of it is then played and heard.

use std::collections::HashSet;

use super::music_search::{self, Write};
use super::snes_capture::{Saved, Session};
use super::spc_map;

/// How long the game runs before the search: past its title, Start pressed now and then.
pub const BOOT_SECONDS: f64 = 30.0;
/// How long a song request has to make the game send the driver a command.
const ANSWER_SECONDS: f64 = 1.0;
/// How long the game gets to upload a song before it is listened to.
const UPLOAD_SECONDS: f64 = 1.5;
/// Candidates tried at most.
const MAX_CANDIDATES: usize = 24;

/// The song request found, and what each value of it plays.
pub struct Found {
    /// Work RAM address of the request byte.
    pub var: u32,
    /// Each value tried and the melody the game then played.
    pub songs: Vec<(u8, HashSet<u64>)>,
    /// Candidates tried before it.
    pub tried: usize,
}

/// The candidates for the song request: work RAM bytes written, in the half second before a
/// command to the driver, with the value the command carried. Most often first.
fn candidates(s: &Session, after: f64) -> Vec<u32> {
    let ports = s.port_writes();
    let writes = s.ram_writes();
    let mut count: std::collections::HashMap<u32, usize> = Default::default();
    // A command: a port write whose value changed, after the driver was running.
    let mut last = [None::<u8>; 4];
    for &(port, value, at) in &ports {
        let p = port as usize & 3;
        if last[p] == Some(value) {
            continue;
        }
        last[p] = Some(value);
        if at < after || value == 0 {
            continue;
        }
        let from = writes.partition_point(|w| w.2 < at - 0.5);
        let to = writes.partition_point(|w| w.2 <= at);
        let mut seen = HashSet::new();
        for &(addr, v, _) in &writes[from..to] {
            if v == value && seen.insert(addr) {
                *count.entry(addr).or_default() += 1;
            }
        }
    }
    let mut c: Vec<(u32, usize)> = count.into_iter().collect();
    c.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    c.into_iter().map(|(a, _)| a).take(MAX_CANDIDATES).collect()
}

/// The port commands the game sends in `seconds` from `saved`, with `poke` made first.
fn commands<'a>(s: &mut Session<'a>, saved: &Saved<'a>, poke: Option<(u32, u8)>, seconds: f64) -> Option<Vec<(u8, u8)>> {
    s.restore(saved).ok()?;
    let before = s.port_writes().len();
    if let Some((at, v)) = poke {
        s.poke(at as usize, v);
    }
    s.run(seconds).ok()?;
    Some(s.port_writes()[before..].iter().map(|&(p, v, _)| (p, v)).collect())
}

/// The melody the game plays `frames` 1/60 s frames long, from where `s` is.
pub fn listen(s: &mut Session, frames: usize) -> HashSet<u64> {
    let mut out: Vec<Vec<Write>> = Vec::with_capacity(frames);
    let mut last = [u16::MAX; 8];
    for _ in 0..frames {
        if s.run(1.0 / 60.0).is_err() {
            break;
        }
        let spc = s.spc_now();
        let Some(dsp) = spc.get(spc_map::SPC_DSP..spc_map::SPC_DSP + 128) else { break };
        out.push(spc_map::dsp_frame(dsp, &mut last));
    }
    spc_map::melody(&out)
}

/// Runs `file` and finds its song request; then plays each value of `values` through it for
/// `frames` frames.
pub fn find(file: &[u8], values: impl Iterator<Item = u8> + Clone, frames: usize) -> Result<Found, String> {
    let mut s = Session::boot(file, true)?;
    s.log_ram(true);
    s.run(BOOT_SECONDS)?;
    if !s.driver_running() {
        return Err("no sound driver started".into());
    }
    let driver_at = s.port_writes().first().map(|w| w.2).unwrap_or(0.0);
    let cands = candidates(&s, driver_at);
    s.log_ram(false);
    let saved = s.save();
    let quiet = commands(&mut s, &saved, None, ANSWER_SECONDS).unwrap_or_default();
    let mut tried = 0;
    for &var in &cands {
        tried += 1;
        s.restore(&saved)?;
        let now = s.wram().get(var as usize).copied().unwrap_or(0);
        // Live when a changed request makes the game send something it otherwise wouldn't.
        let live = [now.wrapping_add(1), now.wrapping_add(2), 1]
            .iter()
            .filter(|&&v| v != now)
            .any(|&v| commands(&mut s, &saved, Some((var, v)), ANSWER_SECONDS).is_some_and(|c| c != quiet));
        if !live {
            continue;
        }
        let mut songs = Vec::new();
        for v in values.clone() {
            if s.restore(&saved).is_err() {
                break;
            }
            s.poke(var as usize, v);
            if s.run(UPLOAD_SECONDS).is_err() {
                continue;
            }
            let m = listen(&mut s, frames);
            if m.len() >= music_search::MIN_MELODY {
                songs.push((v, m));
            }
        }
        if !songs.is_empty() {
            return Ok(Found { var, songs, tried });
        }
    }
    Err(format!("no song request found ({} candidates)", cands.len()))
}
