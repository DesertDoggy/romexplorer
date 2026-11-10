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
/// How long the game gets to upload a song before it is listened to.
const UPLOAD_SECONDS: f64 = 1.5;
/// Candidates tried at most.
const MAX_CANDIDATES: usize = 64;
/// Values each candidate is tried with, and how long each is listened to, to rank them.
const SCREEN_VALUES: u8 = 12;
const SCREEN_FRAMES: usize = 300;
/// Frames a poked request is held for, against code that rewrites it from its own copy.
const HOLD_FRAMES: usize = 6;

/// The song request found, and what each value of it plays.
pub struct Found {
    /// Work RAM address of the request byte.
    pub var: u32,
    /// Each value tried and the melody the game then played.
    pub songs: Vec<(u8, HashSet<u64>)>,
    /// Candidates tried before it.
    pub tried: usize,
}

/// The candidates for the song request: work RAM bytes written in the half second before a
/// command to the driver. Those written with the value the command carried come first (a
/// driver that takes the song number as is), then the others, each by how often.
fn candidates(s: &Session, after: f64) -> Vec<u32> {
    let ports = s.port_writes();
    let writes = s.ram_writes();
    let mut same: std::collections::HashMap<u32, usize> = Default::default();
    let mut near: std::collections::HashMap<u32, usize> = Default::default();
    // A command: a port write whose value changed, after the driver was running.
    let mut last = [None::<u8>; 4];
    for &(port, value, at) in &ports {
        let p = port as usize & 3;
        if last[p] == Some(value) {
            continue;
        }
        last[p] = Some(value);
        if at < after {
            continue;
        }
        let from = writes.partition_point(|w| w.2 < at - 0.5);
        let to = writes.partition_point(|w| w.2 <= at);
        let mut seen = HashSet::new();
        for &(addr, v, _) in &writes[from..to] {
            // The stack and the direct page's scratch bytes are written all the time.
            if (0x0100..0x0200).contains(&addr) || !seen.insert((addr, v == value && value != 0)) {
                continue;
            }
            if v == value && value != 0 {
                *same.entry(addr).or_default() += 1;
            } else {
                *near.entry(addr).or_default() += 1;
            }
        }
    }
    let ranked = |m: std::collections::HashMap<u32, usize>| {
        let mut c: Vec<(u32, usize)> = m.into_iter().collect();
        c.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        c.into_iter().map(|(a, _)| a).collect::<Vec<_>>()
    };
    let mut out = ranked(same);
    for a in ranked(near) {
        if !out.contains(&a) {
            out.push(a);
        }
    }
    out.truncate(MAX_CANDIDATES);
    out
}

/// The melody the game plays after `value` is put in `var` (and held a moment) from `saved`.
fn heard<'a>(s: &mut Session<'a>, saved: &Saved<'a>, var: u32, value: u8, frames: usize) -> HashSet<u64> {
    if s.restore(saved).is_err() {
        return HashSet::new();
    }
    for _ in 0..HOLD_FRAMES {
        s.poke(var as usize, value);
        if s.run(1.0 / 60.0).is_err() {
            return HashSet::new();
        }
    }
    if s.run(UPLOAD_SECONDS).is_err() {
        return HashSet::new();
    }
    listen(s, frames)
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

/// Runs `file` and finds its song request: of the candidates, the one whose values play the
/// most different tracks (`track_of` tells which track a melody is, from the known answer),
/// besides the one playing anyway. Then plays each of `values` through it for `frames`
/// frames.
pub fn find(
    file: &[u8],
    values: impl Iterator<Item = u8> + Clone,
    frames: usize,
    track_of: &dyn Fn(&HashSet<u64>) -> Option<usize>,
) -> Result<Found, String> {
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
    // The track playing anyway, poke or not.
    let playing = {
        s.restore(&saved)?;
        s.run(UPLOAD_SECONDS)?;
        track_of(&listen(&mut s, SCREEN_FRAMES))
    };
    let mut best: Option<(usize, u32)> = None;
    for &var in &cands {
        let mut tracks = HashSet::new();
        for v in 0..SCREEN_VALUES {
            if let Some(t) = track_of(&heard(&mut s, &saved, var, v, SCREEN_FRAMES)) {
                if Some(t) != playing {
                    tracks.insert(t);
                }
            }
        }
        if !tracks.is_empty() && best.is_none_or(|(n, _)| tracks.len() > n) {
            best = Some((tracks.len(), var));
        }
    }
    let Some((_, var)) = best else {
        return Err(format!("no song request found ({} candidates)", cands.len()));
    };
    let tried = cands.iter().position(|&c| c == var).map_or(0, |i| i + 1);
    let mut songs = Vec::new();
    for v in values {
        let m = heard(&mut s, &saved, var, v, frames);
        if m.len() >= music_search::MIN_MELODY {
            songs.push((v, m));
        }
    }
    Ok(Found { var, songs, tried })
}

/// For diagnosis: boots `file`, then plays each of `values` through the request byte `var`
/// given. Also how alike the game sounds after a restore to how it sounds run straight on (1.0
/// when a restore loses nothing).
pub fn try_var(file: &[u8], var: u32, values: impl Iterator<Item = u8>, frames: usize) -> Result<(f64, Vec<(u8, HashSet<u64>)>), String> {
    let mut s = Session::boot(file, true)?;
    s.run(BOOT_SECONDS)?;
    let saved = s.save();
    s.run(UPLOAD_SECONDS)?;
    let straight = listen(&mut s, frames);
    s.restore(&saved)?;
    s.run(UPLOAD_SECONDS)?;
    let restored = listen(&mut s, frames);
    let same = music_search::alike(&straight, &restored);
    let songs = values.map(|v| (v, heard(&mut s, &saved, var, v, frames))).collect();
    Ok((same, songs))
}
