//! Cartridge music played straight from the ROM, by running the game's own sound code: the
//! search every console shares. A console plugs in as a [`Machine`] (its CPU, memory map,
//! timing and sound chip); this finds its music:
//!
//! 1. **The sound routine.** Every write to a sound register is traced back to the routines
//!    in progress. The routine called about once a frame that, run on its own from where the
//!    game called it, writes the same registers the game did, is the one that plays the music.
//! 2. **The song number.** A song starts when the game hands its sound code a number: written
//!    to a byte of RAM the sound routine reads, or passed in a register to a routine the game
//!    calls. Each way seen during boot is tried with numbers 0-255, the sound routine running
//!    alone; the way that gives the most different, audible results is the game's, and those
//!    numbers are its songs.
//! 3. **Timing.** A song is listened to until it falls silent or its writes repeat (its loop).
//!
//! Playback does the same: boot to the same moment, hand over the number, run the sound
//! routine once a frame and send its writes to the sound chip. Nothing is copied out of the
//! ROM.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

/// A sound register write: CPU cycle into the frame, the register (a console's own numbering:
/// chip and address), the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Write {
    pub at: u32,
    pub port: u32,
    pub value: u16,
}

/// What happened during one instruction, for the search.
pub struct Step<'a> {
    /// The routines in progress, outermost first: (entry address, stack pointer it returns to).
    pub calls: &'a [(u32, u32)],
    /// Routines entered by this instruction (an interrupt and a call can come together).
    pub entered: &'a [(u32, u32)],
    /// Sound writes made by this instruction.
    pub sound: &'a [Write],
    /// RAM accesses, when traced: (address, written, value).
    pub ram: &'a [(u32, bool, u8)],
}

/// A console the search can run a game on. Cloning copies the whole machine.
pub trait Machine: Clone + Sized {
    /// A moment the CPU entered a routine: enough to call it again from there.
    type Entry: Clone;
    /// The emulated sound chip.
    type Chip;

    /// The game, powered on.
    fn boot(rom: &[u8]) -> Result<Self, String>;
    /// Runs a frame, showing every instruction to `watch` when given. With `stop_after`,
    /// stops right after that many instructions of this frame and returns true.
    fn run_frame(&mut self, watch: Option<&mut dyn FnMut(&Step)>, stop_after: Option<u64>) -> bool;
    /// The sound writes made since the last call.
    fn take_sound(&mut self) -> Vec<Write>;
    /// Traces RAM accesses into [`Step::ram`].
    fn set_trace(&mut self, on: bool);
    /// Holds Start (or the console's equivalent) on the controller, or nothing.
    fn press_start(&mut self, held: bool);
    /// The routine the CPU just entered, as an [`Self::Entry`].
    fn entry(&self) -> Self::Entry;
    /// One frame of the sound routine alone, called at its usual moment. None when it
    /// didn't return.
    fn play_frame(&mut self, play: &Self::Entry) -> Option<Vec<Write>>;
    /// Calls the routine of `entry` with register `reg` set to `v`, then puts the CPU and
    /// banks back as they were; its sound writes count at the current moment.
    fn call_with(&mut self, entry: &Self::Entry, reg: u8, v: u8) -> bool;
    /// Registers a song number can be passed in.
    const REGISTERS: u8;
    /// Writes a byte of RAM.
    fn poke(&mut self, addr: u32, v: u8);
    /// Registers that say nothing about which routine plays the music (set once at boot).
    fn incidental(w: &Write) -> bool;
    /// The stack's addresses (never a song request).
    fn is_stack(&self, addr: u32) -> bool;
    /// Writes that bring a fresh chip to the registers' current values.
    fn restore_writes(&self) -> Vec<Write>;
    fn new_chip(&self, rate: u32) -> Result<Self::Chip, String>;
    /// Sends a frame's writes to the chip and appends the frame's samples.
    fn feed(&self, chip: &mut Self::Chip, writes: &[Write], out: &mut Vec<i16>);
    /// Channels in the samples (1 or 2).
    const CHANNELS: u16;
    fn frame_rate(&self) -> f64;
    /// A frame's length in the units of [`Write::at`].
    fn frame_length(&self) -> u32;
}

/// How the game is handed a song number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// Written to this RAM byte.
    Byte(u32),
    /// Passed in a register to the routine at this address, as called the `nth` time during
    /// boot.
    Call { addr: u32, reg: u8, nth: u32 },
}

impl Trigger {
    fn encode(&self) -> String {
        match self {
            Trigger::Byte(a) => format!("m{a:04x}"),
            Trigger::Call { addr, reg, nth } => format!("c{addr:04x}.{reg}.{nth}"),
        }
    }

    fn decode(s: &str) -> Option<Self> {
        if let Some(a) = s.strip_prefix('m') {
            return u32::from_str_radix(a, 16).ok().map(Trigger::Byte);
        }
        let mut p = s.strip_prefix('c')?.split('.');
        let addr = u32::from_str_radix(p.next()?, 16).ok()?;
        let reg = p.next()?.parse().ok()?;
        let nth = p.next()?.parse().ok()?;
        Some(Trigger::Call { addr, reg, nth })
    }
}

/// Where a game's music is: the moment to boot to, the sound routine, how songs are started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipe {
    /// Frames booted before the sound routine's call that playback starts from.
    pub boot_frames: u32,
    /// Start was pressed during boot (some games only start their music then).
    pub press_start: u8,
    /// The sound routine.
    pub play: u32,
    pub trigger: Trigger,
    /// Times the sound routine runs a frame (games that tick their music from a timer
    /// faster than the screen); 1 for most.
    pub calls: u8,
}

impl Recipe {
    pub fn encode(&self) -> String {
        let mut s = format!(
            "{}@{}@{:04x}@{}",
            self.boot_frames,
            self.press_start,
            self.play,
            self.trigger.encode()
        );
        if self.calls > 1 {
            s += &format!("@{}", self.calls);
        }
        s
    }

    pub fn decode(s: &str) -> Option<Self> {
        let mut p = s.split('@');
        let boot_frames = p.next()?.parse().ok()?;
        let press_start = p.next()?.parse().ok().filter(|&s| s <= 2)?;
        let play = u32::from_str_radix(p.next()?, 16).ok()?;
        let trigger = Trigger::decode(p.next()?)?;
        let calls = match p.next() {
            Some(n) => n.parse().ok().filter(|&n| n >= 1)?,
            None => 1,
        };
        Some(Self {
            boot_frames,
            press_start,
            play,
            trigger,
            calls,
        })
    }
}

/// Whether Start is held in frame `frame` of boot, in start pattern `press_start`: 0 never;
/// 1 a few frames at the 3, 5 and 7 second marks (enough for most title screens, and no
/// presses after that to pause a game that is playing); 2 every two seconds from the third
/// on (games that need more presses to start their music).
fn start_held(press_start: u8, frame: u32) -> bool {
    let pressing = match press_start {
        0 => false,
        1 => (180..=420).contains(&frame),
        _ => frame >= 180,
    };
    pressing && frame % 120 < 6
}

/// How long boot runs while the search looks for the sound routine.
pub const BOOT_FRAMES: u32 = 900;
/// Frames the sound routine runs alone and is compared with the game.
const VERIFY_FRAMES: usize = 240;
/// Share of those frames that must match.
const VERIFY_MATCH: f64 = 0.8;
/// A search gives up after this long (a game whose code sends it round and round).
const SEARCH_TIME_LIMIT: std::time::Duration = std::time::Duration::from_secs(600);
/// Frames a song number is listened to while the search compares them.
const TRY_FRAMES: usize = 90;
/// Frames a song number that sounded is listened to before it counts as a song.
const SONG_FRAMES: usize = 300;
/// A song still sounds in its last this many frames of [`SONG_FRAMES`]; a sound effect has
/// stopped by then.
const SONG_TAIL_FRAMES: usize = 60;
/// Song numbers past a run of this many that play nothing are not taken: games read
/// numbers past the end of their song table as garbage that often still makes noise.
const SONG_GAP: usize = 16;
/// How a way of starting songs ranks: songs that last, a song table that ends, then the
/// number of songs.
type Rank = (bool, bool, usize);
/// Numbers sounding different past which a way of starting songs is taken to read past
/// its song table.
const UNBOUNDED_SONGS: usize = 128;
/// Songs that last a search stops at; with fewer it tries the other routines and boots.
const GOOD_SONGS: usize = 4;
/// Ways to start a song that are tried with every number, best screened first.
const SCREENED_TRIED: usize = 3;
/// Ways tried at most, while none tried has songs that last from a song table that ends.
const SCREENED_TRIED_MORE: usize = 6;
/// Frames left out of a song's sound fingerprint: where songs that sound alike still differ
/// as their channels are set up.
const SOUND_SKIP_FRAMES: usize = 8;
/// A song is listened to for at most this long while measuring it (frames at 60 Hz).
const MEASURE_FRAMES: usize = 60 * 300;
/// Silence this long ends a song.
const SILENT_END_FRAMES: usize = 180;
/// A loop must repeat this many frames to count.
const LOOP_WINDOW: usize = 240;
/// Shortest loop taken.
const MIN_LOOP_FRAMES: usize = 60;
/// A sample louder than this is sound.
const AUDIBLE: u16 = 300;

fn frame_hash(writes: &[Write]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for w in writes {
        (w.port, w.value).hash(&mut h);
    }
    h.finish()
}

/// The boot run's record: per frame, the sound writes; per routine, how it shows up in them.
struct BootLog {
    frames: Vec<Vec<Write>>,
    /// Sound writes made inside each routine (any depth), and their total.
    writes_in: HashMap<u32, u32>,
    total_writes: u32,
    /// How deep each routine sat when writes happened inside it (sum of depths, count).
    depth: HashMap<u32, (u64, u32)>,
    /// Frames each routine was entered in, and how often in all.
    entered_frames: HashMap<u32, (HashSet<u32>, u32)>,
}

fn boot<M: Machine>(rom: &[u8], press_start: u8, frames: u32) -> Result<M, String> {
    let mut m = M::boot(rom)?;
    for f in 0..frames {
        m.press_start(start_held(press_start, f));
        m.run_frame(None, None);
    }
    m.take_sound();
    Ok(m)
}

fn boot_log<M: Machine>(rom: &[u8], press_start: u8) -> Result<BootLog, String> {
    let mut m = M::boot(rom)?;
    let mut log = BootLog {
        frames: Vec::new(),
        writes_in: HashMap::new(),
        total_writes: 0,
        depth: HashMap::new(),
        entered_frames: HashMap::new(),
    };
    for f in 0..BOOT_FRAMES {
        m.press_start(start_held(press_start, f));
        let mut watch = |s: &Step| {
            for &(r, _) in s.entered {
                let e = log.entered_frames.entry(r).or_default();
                e.0.insert(f);
                e.1 += 1;
            }
            for w in s.sound {
                if M::incidental(w) {
                    continue;
                }
                log.total_writes += 1;
                let mut seen = HashSet::new();
                for (depth, &(r, _)) in s.calls.iter().enumerate() {
                    if seen.insert(r) {
                        *log.writes_in.entry(r).or_default() += 1;
                        let d = log.depth.entry(r).or_default();
                        d.0 += depth as u64;
                        d.1 += 1;
                    }
                }
            }
        };
        m.run_frame(Some(&mut watch), None);
        log.frames.push(m.take_sound());
    }
    Ok(log)
}

/// The machine right after the first instruction of frame `frame` that enters `routine`
/// (input as `press_start` says), and the entry there.
fn entry_at<M: Machine>(
    rom: &[u8],
    press_start: u8,
    frame: u32,
    routine: u32,
) -> Result<Option<(M, M::Entry)>, String> {
    let mut m = boot::<M>(rom, press_start, frame)?;
    m.press_start(start_held(press_start, frame));
    Ok(enter_in_frame(
        &mut m,
        |s| s.entered.iter().any(|e| e.0 == routine),
        0,
    ))
}

/// Runs `m` into the current frame up to the instruction that is the `nth` (from 0) to
/// satisfy `hit`, and returns the machine and the entry there; `m` is left untouched when the
/// frame has no such instruction (`hit` counts are consumed either way).
fn enter_in_frame<M: Machine>(
    m: &mut M,
    mut hit: impl FnMut(&Step) -> bool,
    nth: u32,
) -> Option<(M, M::Entry)> {
    let mut probe = m.clone();
    let mut count = 0u64;
    let mut seen = 0;
    let mut target = None;
    let mut watch = |s: &Step| {
        count += 1;
        if target.is_none() && hit(s) {
            if seen == nth {
                target = Some(count);
            }
            seen += 1;
        }
    };
    probe.run_frame(Some(&mut watch), None);
    let target = target?;
    m.run_frame(None, Some(target));
    let entry = m.entry();
    m.take_sound();
    Some((m.clone(), entry))
}

/// One frame of the sound routine, run `calls` times spread over the frame.
fn play_frame<M: Machine>(m: &mut M, play: &M::Entry, calls: u8) -> Option<Vec<Write>> {
    if calls <= 1 {
        return m.play_frame(play);
    }
    let len = m.frame_length();
    let mut out = Vec::new();
    for i in 0..calls as u32 {
        let shift = (i as u64 * len as u64 / calls as u64) as u32;
        out.extend(
            m.play_frame(play)?
                .into_iter()
                .map(|w| Write { at: (w.at + shift) % len, ..w }),
        );
    }
    out.sort_by_key(|w| w.at);
    Some(out)
}

/// Runs the sound routine alone for `frames` frames from `m`.
fn run_alone<M: Machine>(
    m: &mut M,
    play: &M::Entry,
    frames: usize,
    calls: u8,
) -> Option<Vec<Vec<Write>>> {
    (0..frames).map(|_| play_frame(m, play, calls)).collect()
}

/// Verified sound routines tried in turn, per boot.
const MAX_PLAYS: usize = 3;
/// Most times a frame a sound routine is taken to run.
const MAX_CALLS: f64 = 16.0;

/// What the search found.
#[derive(Debug, Clone)]
pub struct Found {
    pub recipe: Recipe,
    /// Song numbers, in order.
    pub songs: Vec<u8>,
    /// Why nothing was found, when nothing was.
    pub why: Option<String>,
}

/// Finds the sound routine of a game that played music during boot: the routines that
/// verified (best first), each with the frame it was verified at and its calls a frame.
fn find_play<M: Machine>(
    rom: &[u8],
    press_start: u8,
    log: &BootLog,
    note: &mut dyn FnMut(String),
) -> Result<Vec<(u32, u32, u8)>, String> {
    let mut found = Vec::new();
    if log.total_writes < 50 {
        return Ok(found);
    }
    // Routines most writes happened inside, innermost first.
    let mut candidates: Vec<(u32, f64)> = log
        .writes_in
        .iter()
        .filter(|(_, &n)| n as f64 >= 0.6 * log.total_writes as f64)
        .map(|(&r, _)| {
            let (sum, n) = log.depth[&r];
            (r, sum as f64 / n.max(1) as f64)
        })
        .collect();
    candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let first_sound = log
        .frames
        .iter()
        .position(|f| f.iter().any(|w| !M::incidental(w)))
        .unwrap_or(0) as u32;
    // Routines run once a frame first: a routine entered a few times a frame is often a
    // helper of the real one (a channel's update), which plays the boot music alone but not
    // a new song. Timer-driven routines get their turn after.
    for (pass, &(routine, _)) in (0..2)
        .flat_map(|pass| candidates.iter().map(move |c| (pass, c)))
    {
        let Some((frames, total)) = log.entered_frames.get(&routine) else {
            continue;
        };
        let per_frame = *total as f64 / frames.len().max(1) as f64;
        if (per_frame.round() > 1.0) != (pass == 1) {
            continue;
        }
        // Called about once a frame from the start of the music on.
        let span = BOOT_FRAMES.saturating_sub(first_sound).max(1);
        // Run a steady number of times a frame: once, or a timer's few times.
        let calls = per_frame.round();
        if (frames.len() as f64) < 0.5 * span as f64
            || calls > MAX_CALLS
            || (calls > 1.0 && (per_frame - calls).abs() > 0.25)
        {
            note(format!(
                "  routine {routine:04x}: entered in {} of {span} frames, {per_frame:.1} times a frame",
                frames.len()
            ));
            continue;
        }
        let latest = BOOT_FRAMES - VERIFY_FRAMES as u32 - 1;
        let mut at = (first_sound + 60).min(latest);
        while at <= latest && !frames.contains(&at) {
            at += 1;
        }
        if at > latest {
            note(format!("  routine {routine:04x}: not entered late enough to check"));
            continue;
        }
        let Some((mut m, entry)) = entry_at::<M>(rom, press_start, at, routine)? else {
            note(format!("  routine {routine:04x}: not entered again in frame {at}"));
            continue;
        };
        let calls = calls.max(1.0) as u8;
        let Some(alone) = run_alone(&mut m, &entry, VERIFY_FRAMES, calls) else {
            note(format!("  routine {routine:04x}: crashed when run alone"));
            continue;
        };
        let reference = &log.frames[at as usize..at as usize + VERIFY_FRAMES];
        let key = |f: &[Write]| {
            f.iter()
                .filter(|w| !M::incidental(w))
                .map(|w| (w.port, w.value))
                .collect::<Vec<_>>()
        };
        let same = alone
            .iter()
            .zip(reference)
            .filter(|(a, b)| key(a) == key(b))
            .count();
        if same as f64 >= VERIFY_MATCH * VERIFY_FRAMES as f64 {
            found.push((at, routine, calls));
            if found.len() == MAX_PLAYS {
                break;
            }
            continue;
        }
        note(format!(
            "  routine {routine:04x}: run alone ({calls} a frame) from frame {at}, {same} of {VERIFY_FRAMES} frames match"
        ));
    }
    Ok(found)
}

/// Ways the game hands its sound code a number, seen during boot.
fn trigger_candidates<M: Machine>(
    rom: &[u8],
    press_start: u8,
    play: u32,
) -> Result<Vec<Trigger>, String> {
    let mut m = M::boot(rom)?;
    m.set_trace(true);
    let mut read_by_play: HashSet<u32> = HashSet::new();
    // Addresses the sound routine writes before it reads them are its own working state.
    let mut written_by_play: HashSet<u32> = HashSet::new();
    let mut written_by_game: HashMap<u32, u32> = HashMap::new();
    // Routines called outside the sound routine, how often, and the addresses written inside.
    let mut calls: HashMap<u32, u32> = HashMap::new();
    let mut writes_inside: HashMap<u32, HashSet<u32>> = HashMap::new();
    let mut handlers: HashSet<u32> = HashSet::new();
    for f in 0..BOOT_FRAMES {
        m.press_start(start_held(press_start, f));
        let stack = m.clone();
        let mut watch = |s: &Step| {
            let in_play = s.calls.iter().any(|c| c.0 == play);
            for (i, &(r, _)) in s.entered.iter().enumerate() {
                if !in_play {
                    *calls.entry(r).or_default() += 1;
                }
                // The outermost entries are interrupt handlers (the main program is in none).
                if s.calls.len() - s.entered.len() + i == 0 {
                    handlers.insert(r);
                }
            }
            for &(a, write, _) in s.ram {
                if stack.is_stack(a) {
                    continue;
                }
                if in_play {
                    if write {
                        written_by_play.insert(a);
                    } else if !written_by_play.contains(&a) {
                        read_by_play.insert(a);
                    }
                } else if write {
                    *written_by_game.entry(a).or_default() += 1;
                    for &(r, _) in s.calls {
                        writes_inside.entry(r).or_default().insert(a);
                    }
                }
            }
        };
        m.run_frame(Some(&mut watch), None);
        m.take_sound();
    }
    let mut out = Vec::new();
    let mut bytes: Vec<(u32, u32)> = written_by_game
        .into_iter()
        .filter(|(a, _)| read_by_play.contains(a))
        .collect();
    // Bytes the game writes rarely are requests; ones it writes every frame are game state.
    bytes.sort_by_key(|&(a, n)| (n, a));
    out.extend(
        bytes
            .into_iter()
            .filter(|&(_, n)| n < BOOT_FRAMES / 2)
            .take(24)
            .map(|(a, _)| Trigger::Byte(a)),
    );
    let mut routines: Vec<(u32, u32)> = calls
        .into_iter()
        .filter(|(r, n)| {
            *r != play
                && !handlers.contains(r)
                && *n <= 16
                && writes_inside
                    .get(r)
                    .is_some_and(|w| w.iter().any(|a| read_by_play.contains(a)))
        })
        .collect();
    routines.sort_by_key(|&(r, n)| (n, r));
    for (r, _) in routines.into_iter().take(8) {
        for reg in 0..M::REGISTERS {
            out.push(Trigger::Call {
                addr: r,
                reg,
                nth: 0,
            });
        }
    }
    Ok(out)
}

/// The entry of the `nth` call to `routine` during boot.
fn call_entry<M: Machine>(
    rom: &[u8],
    press_start: u8,
    routine: u32,
    nth: u32,
) -> Result<Option<M::Entry>, String> {
    let mut m = M::boot(rom)?;
    let mut seen = 0;
    for f in 0..BOOT_FRAMES {
        m.press_start(start_held(press_start, f));
        let mut probe = m.clone();
        let mut in_frame = 0;
        let mut watch = |s: &Step| {
            if s.entered.iter().any(|e| e.0 == routine) {
                in_frame += 1;
            }
        };
        probe.run_frame(Some(&mut watch), None);
        if seen + in_frame > nth {
            let want = nth - seen;
            return Ok(
                enter_in_frame(&mut m, |s| s.entered.iter().any(|e| e.0 == routine), want)
                    .map(|(_, e)| e),
            );
        }
        seen += in_frame;
        probe.take_sound();
        m = probe;
    }
    Ok(None)
}

/// Starts song `v` by `trigger` on `m` (the machine at the sound routine's entry).
fn apply<M: Machine>(m: &mut M, trigger: &Trigger, call: Option<&M::Entry>, v: u8) -> bool {
    match trigger {
        Trigger::Byte(a) => {
            m.poke(*a, v);
            true
        }
        Trigger::Call { reg, .. } => call.is_some_and(|e| m.call_with(e, *reg, v)),
    }
}

/// A song's first frames as heard.
struct Heard {
    /// Fingerprint of the sound after its first few frames. Not of the writes: numbers whose
    /// writes differ only in what can't be heard (a byte that only changes which path the
    /// sound code takes, a bit-field request with several bits set) sound the same.
    sound: u64,
    /// Whether anything sounded.
    audible: bool,
    /// Whether it still sounded in the last [`SONG_TAIL_FRAMES`] frames.
    lasts: bool,
}

/// Song `v` being listened to: the machine, the chip and what it has played so far.
struct Listening<M: Machine> {
    m: M,
    chip: M::Chip,
    pre: Vec<Write>,
    samples: Vec<i16>,
    /// Where each frame's samples start.
    starts: Vec<usize>,
    calls: u8,
}

impl<M: Machine> Listening<M> {
    fn start(base: &M, trigger: &Trigger, call: Option<&M::Entry>, v: u8, calls: u8) -> Option<Self> {
        let mut m = base.clone();
        if !apply(&mut m, trigger, call, v) {
            return None;
        }
        let mut pre = m.restore_writes();
        pre.extend(m.take_sound().into_iter().map(|w| Write { at: 0, ..w }));
        let chip = m.new_chip(8000).ok()?;
        Some(Self {
            m,
            chip,
            pre,
            samples: Vec::new(),
            starts: Vec::new(),
            calls,
        })
    }

    /// Plays on until `frames` frames have played. None when the sound routine stopped
    /// returning.
    fn until(&mut self, play: &M::Entry, frames: usize) -> Option<()> {
        while self.starts.len() < frames {
            self.starts.push(self.samples.len());
            let mut writes = play_frame(&mut self.m, play, self.calls)?;
            if !self.pre.is_empty() {
                self.pre.append(&mut writes);
                writes = std::mem::take(&mut self.pre);
            }
            self.m.feed(&mut self.chip, &writes, &mut self.samples);
        }
        Some(())
    }

    /// How what has played so far sounds.
    fn heard(&self) -> Heard {
        let frames = self.starts.len();
        let at = |f: usize| self.starts.get(f).copied().unwrap_or(self.samples.len());
        let loud = |s: &[i16]| s.iter().filter(|s| s.unsigned_abs() > AUDIBLE).count();
        let tail = &self.samples[at(frames.saturating_sub(SONG_TAIL_FRAMES))..];
        let mut sh = std::collections::hash_map::DefaultHasher::new();
        for s in &self.samples[at(SOUND_SKIP_FRAMES.min(frames / 2))..] {
            (s >> 6).hash(&mut sh);
        }
        Heard {
            sound: sh.finish(),
            audible: loud(&self.samples) > self.samples.len() / 10,
            lasts: loud(tail) > tail.len() / 10,
        }
    }
}

/// Listens to song `v` for `frames` frames.
fn listen<M: Machine>(
    base: &M,
    play: &M::Entry,
    trigger: &Trigger,
    call: Option<&M::Entry>,
    v: u8,
    frames: usize,
    calls: u8,
) -> Option<Heard> {
    let mut l = Listening::start(base, trigger, call, v, calls)?;
    l.until(play, frames)?;
    Some(l.heard())
}

/// The song numbers worth keeping out of everything that played: up to the first run of
/// [`SONG_GAP`] numbers that played nothing.
fn song_run(played: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    for &v in played {
        if out.last().is_some_and(|&l| (v - l) as usize > SONG_GAP) {
            break;
        }
        out.push(v);
    }
    out
}

/// Searches a ROM image for its music.
pub fn search<M: Machine>(rom: &[u8]) -> Result<Found, String> {
    search_logged::<M>(rom, &mut |_| {})
}

/// [`search`], telling `note` what it tries and finds (for the PC test tools).
pub fn search_logged<M: Machine>(
    rom: &[u8],
    note: &mut dyn FnMut(String),
) -> Result<Found, String> {
    // The furthest the search got, over both boots.
    let mut why = (0, "the game played no music in its first 15 seconds");
    let started = std::time::Instant::now();
    let timed_out = || started.elapsed() > SEARCH_TIME_LIMIT;
    // The best result so far, ranked as songs that last first, then by count.
    let mut found: Option<(Rank, Found)> = None;
    'boots: for press_start in [0u8, 1, 2] {
        if timed_out() {
            note("search took too long; stopped".into());
            why = (why.0.max(3), "the search took too long");
            break;
        }
        let log = boot_log::<M>(rom, press_start)?;
        let busiest: Vec<String> = {
            let mut v: Vec<(&u32, &u32)> = log.writes_in.iter().collect();
            v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
            v.iter()
                .take(4)
                .map(|(r, n)| format!("{r:04x}:{n}"))
                .collect()
        };
        note(format!(
            "boot{}: {} sound writes; routines {}",
            ["", " (Start)", " (Start, steady)"][press_start as usize],
            log.total_writes,
            busiest.join(" ")
        ));
        let plays = find_play::<M>(rom, press_start, &log, note)?;
        if plays.is_empty() && log.total_writes >= 50 && why.0 < 1 {
            why = (1, "the routine that plays the music wasn't found");
        }
        // The next verified routine when one leads to no songs.
        for (at, play, calls) in plays {
            if timed_out() {
                break;
            }
            let Some((base, entry)) = entry_at::<M>(rom, press_start, at, play)? else {
                continue;
            };
            let candidates = trigger_candidates::<M>(rom, press_start, play)?;
            note(format!(
                "sound routine {play:04x} (frame {at}); {} ways to start a song to try",
                candidates.len()
            ));
            // A quick round with a few numbers each, then all numbers for the best few ways.
            let baseline = listen(
                &base,
                &entry,
                &Trigger::Byte(u32::MAX),
                None,
                0,
                TRY_FRAMES / 2,
                calls,
            )
            .map(|r| r.sound);
            let mut screened: Vec<(usize, Trigger, Option<M::Entry>)> = Vec::new();
            for t in candidates {
                if timed_out() {
                    break;
                }
                let call = match &t {
                    Trigger::Call { addr, nth, .. } => {
                        match call_entry::<M>(rom, press_start, *addr, *nth)? {
                            Some(e) => Some(e),
                            None => continue,
                        }
                    }
                    Trigger::Byte(_) => None,
                };
                let mut seen = HashSet::new();
                for v in 1..=8u8 {
                    if let Some(h) = listen(&base, &entry, &t, call.as_ref(), v, TRY_FRAMES / 2, calls) {
                        if h.audible && Some(h.sound) != baseline {
                            seen.insert(h.sound);
                        }
                    }
                }
                if !seen.is_empty() {
                    note(format!(
                        "  {t:?}: {} of 8 numbers sound different",
                        seen.len()
                    ));
                    screened.push((seen.len(), t, call));
                }
            }
            screened.sort_by_key(|s| std::cmp::Reverse(s.0));
            // The best way, as [`Rank`] orders them.
            let mut best: Option<(Rank, Trigger, Vec<u8>)> = None;
            for (i, (_, t, call)) in screened.into_iter().enumerate() {
                // The best few; more while none has a song table that ends.
                let settled = best.as_ref().is_some_and(|b| b.0 .0 && b.0 .1);
                if i >= SCREENED_TRIED_MORE || (i >= SCREENED_TRIED && settled) || timed_out() {
                    break;
                }
                // A number the game ignores leaves the boot music playing on.
                let mut seen: HashSet<u64> = listen(
                    &base,
                    &entry,
                    &Trigger::Byte(u32::MAX),
                    None,
                    0,
                    TRY_FRAMES,
                    calls,
                )
                .map(|h| h.sound)
                .into_iter()
                .collect();
                let mut played = Vec::new();
                // Why the other numbers were left out: crashed, silent, sounded as an earlier
                // one, crashed later. Numbers that stopped within seconds are jingles (or
                // sound effects).
                let mut dropped = [0usize; 4];
                let mut short = Vec::new();
                for v in 0..=255u8 {
                    if timed_out() {
                        break;
                    }
                    let Some(mut l) = Listening::start(&base, &t, call.as_ref(), v, calls)
                        .and_then(|mut l| l.until(&entry, TRY_FRAMES).map(|_| l))
                    else {
                        dropped[0] += 1;
                        continue;
                    };
                    let quick = l.heard();
                    if !quick.audible {
                        dropped[1] += 1;
                        continue;
                    }
                    if !seen.insert(quick.sound) {
                        dropped[2] += 1;
                        continue;
                    }
                    // Music still sounds seconds later; a sound effect has stopped.
                    match l.until(&entry, SONG_FRAMES).map(|_| l.heard()) {
                        None => dropped[3] += 1,
                        Some(h) if !(h.audible && h.lasts) => short.push(v),
                        Some(_) => played.push(v),
                    }
                    // Past this many, the way reads past its song table (see `bounded`):
                    // the rest would only be more of the same.
                    if played.len() + short.len() >= UNBOUNDED_SONGS {
                        break;
                    }
                }
                // A game whose every tune is a few seconds long (older games are mostly
                // jingles): its short ones are its music.
                let songs = if played.is_empty() {
                    song_run(&short)
                } else {
                    song_run(&played)
                };
                note(format!(
                    "  {t:?}: {} songs ({} numbers played music, {} jingles; left out: {} crashed, {} silent, \
                     {} same as another, {} crashed later)",
                    songs.len(),
                    played.len(),
                    short.len(),
                    dropped[0],
                    dropped[1],
                    dropped[2],
                    dropped[3],
                ));
                // A way where most numbers sound different reads past the end of its song
                // table (or isn't a song number at all): its "songs" are noise from whatever
                // follows. A way with a table that ends beats it.
                let bounded = played.len() + short.len() < UNBOUNDED_SONGS;
                let rank = (!played.is_empty(), bounded, songs.len());
                if best.as_ref().is_none_or(|b| rank > b.0) {
                    best = Some((rank, t, songs));
                }
            }
            if let Some((rank, trigger, songs)) = best.filter(|b| !b.2.is_empty()) {
                let recipe = Recipe {
                    boot_frames: at,
                    press_start,
                    play,
                    trigger,
                    calls,
                };
                let good = rank.0 && rank.1 && songs.len() >= GOOD_SONGS;
                if found.as_ref().is_none_or(|f: &(Rank, Found)| rank > f.0) {
                    found = Some((
                        rank,
                        Found {
                            recipe,
                            songs,
                            why: None,
                        },
                    ));
                }
                // Enough songs that last: no need to look further. Fewer, or jingles only,
                // and the other routines and boots may do better (a game that only starts its
                // song code once Start is pressed).
                if good {
                    break 'boots;
                }
                continue;
            }
            why = if timed_out() {
                (3, "the search took too long")
            } else {
                (2, "the sound routine was found, but not how the game picks a song")
            };
        }
    }
    if let Some((_, f)) = found {
        return Ok(f);
    }
    Ok(Found {
        recipe: Recipe {
            boot_frames: 0,
            press_start: 0,
            play: 0,
            trigger: Trigger::Byte(0),
            calls: 1,
        },
        songs: Vec::new(),
        why: Some(why.1.to_string()),
    })
}

/// A song, ready to play: the machine at the sound routine, the song started.
pub struct Song<M: Machine> {
    m: M,
    play: M::Entry,
    chip: M::Chip,
    pre: Vec<Write>,
    calls: u8,
}

impl<M: Machine> Song<M> {
    /// Boots `rom` as `recipe` says and starts song `v`, rendering at `rate`.
    pub fn open(rom: &[u8], recipe: &Recipe, v: u8, rate: u32) -> Result<Self, String> {
        let (mut m, play) =
            entry_at::<M>(rom, recipe.press_start, recipe.boot_frames, recipe.play)?
                .ok_or("the game no longer reaches its sound routine")?;
        let call = match &recipe.trigger {
            Trigger::Call { addr, nth, .. } => Some(
                call_entry::<M>(rom, recipe.press_start, *addr, *nth)?
                    .ok_or("the game no longer calls its song routine")?,
            ),
            Trigger::Byte(_) => None,
        };
        if !apply(&mut m, &recipe.trigger, call.as_ref(), v) {
            return Err("the song didn't start".into());
        }
        let mut pre = m.restore_writes();
        pre.extend(m.take_sound().into_iter().map(|w| Write { at: 0, ..w }));
        let chip = m.new_chip(rate)?;
        Ok(Self {
            m,
            play,
            chip,
            pre,
            calls: recipe.calls,
        })
    }

    pub fn frame_rate(&self) -> f64 {
        self.m.frame_rate()
    }

    /// One frame: its samples appended to `out` (interleaved when stereo), and the frame's
    /// sound writes.
    pub fn frame(&mut self, out: &mut Vec<i16>) -> Result<Vec<Write>, String> {
        let own = play_frame(&mut self.m, &self.play, self.calls)
            .ok_or("the sound routine stopped returning")?;
        let mut writes = std::mem::take(&mut self.pre);
        writes.extend_from_slice(&own);
        self.m.feed(&mut self.chip, &writes, out);
        Ok(own)
    }
}

/// How a song plays: its length in frames, and where it loops (start, length) if it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    pub frames: usize,
    pub looping: Option<(usize, usize)>,
}

/// Listens to a song to time it: until it falls silent, repeats itself, or runs five minutes.
pub fn measure<M: Machine>(rom: &[u8], recipe: &Recipe, v: u8) -> Result<Timing, String> {
    let mut song = Song::<M>::open(rom, recipe, v, 8000)?;
    let max_frames = (MEASURE_FRAMES as f64 * song.frame_rate() / 60.0) as usize;
    let mut hashes: Vec<u64> = Vec::new();
    let mut last_loud = 0usize;
    let mut samples = Vec::new();
    let mut windows: HashMap<u64, usize> = HashMap::new();
    for f in 0..max_frames {
        samples.clear();
        let writes = song.frame(&mut samples)?;
        hashes.push(frame_hash(&writes));
        if samples.iter().any(|s| s.unsigned_abs() > AUDIBLE) {
            last_loud = f;
        }
        if f > last_loud + SILENT_END_FRAMES {
            return Ok(Timing {
                frames: last_loud + 1,
                looping: None,
            });
        }
        if f + 1 >= LOOP_WINDOW {
            let start = f + 1 - LOOP_WINDOW;
            let mut h = std::collections::hash_map::DefaultHasher::new();
            hashes[start..=f].hash(&mut h);
            let key = h.finish();
            match windows.get(&key) {
                Some(&first)
                    if start - first >= MIN_LOOP_FRAMES
                        && hashes[first..first + LOOP_WINDOW] == hashes[start..=f] =>
                {
                    return Ok(Timing {
                        frames: start,
                        looping: Some((first, start - first)),
                    });
                }
                Some(_) => {}
                None => {
                    windows.insert(key, start);
                }
            }
        }
    }
    Ok(Timing {
        frames: max_frames,
        looping: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recipes_round_trip() {
        let r = Recipe {
            boot_frames: 123,
            press_start: 1,
            play: 0xC200,
            trigger: Trigger::Call {
                addr: 0x8123,
                reg: 1,
                nth: 2,
            },
            calls: 1,
        };
        assert_eq!(Recipe::decode(&r.encode()), Some(r));
        let r = Recipe {
            boot_frames: 5,
            press_start: 0,
            play: 0x8000,
            trigger: Trigger::Byte(0x7F0),
            calls: 6,
        };
        assert_eq!(r.encode(), "5@0@8000@m07f0@6");
        assert_eq!(Recipe::decode(&r.encode()), Some(r));
    }
}
