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
    pub press_start: bool,
    /// The sound routine.
    pub play: u32,
    pub trigger: Trigger,
}

impl Recipe {
    pub fn encode(&self) -> String {
        format!(
            "{}@{}@{:04x}@{}",
            self.boot_frames,
            self.press_start as u8,
            self.play,
            self.trigger.encode()
        )
    }

    pub fn decode(s: &str) -> Option<Self> {
        let mut p = s.split('@');
        let boot_frames = p.next()?.parse().ok()?;
        let press_start = p.next()? == "1";
        let play = u32::from_str_radix(p.next()?, 16).ok()?;
        let trigger = Trigger::decode(p.next()?)?;
        Some(Self {
            boot_frames,
            press_start,
            play,
            trigger,
        })
    }
}

/// Whether Start is held in frame `frame` of boot: never, or for a few frames every two
/// seconds after the first three.
fn start_held(press_start: bool, frame: u32) -> bool {
    press_start && frame >= 180 && frame % 120 < 6
}

/// How long boot runs while the search looks for the sound routine.
pub const BOOT_FRAMES: u32 = 900;
/// Frames the sound routine runs alone and is compared with the game.
const VERIFY_FRAMES: usize = 240;
/// Share of those frames that must match.
const VERIFY_MATCH: f64 = 0.8;
/// Frames a song number is listened to while the search compares them.
const TRY_FRAMES: usize = 90;
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

fn boot<M: Machine>(rom: &[u8], press_start: bool, frames: u32) -> Result<M, String> {
    let mut m = M::boot(rom)?;
    for f in 0..frames {
        m.press_start(start_held(press_start, f));
        m.run_frame(None, None);
    }
    m.take_sound();
    Ok(m)
}

fn boot_log<M: Machine>(rom: &[u8], press_start: bool) -> Result<BootLog, String> {
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
    press_start: bool,
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

/// Runs the sound routine alone for `frames` frames from `m`.
fn run_alone<M: Machine>(m: &mut M, play: &M::Entry, frames: usize) -> Option<Vec<Vec<Write>>> {
    (0..frames).map(|_| m.play_frame(play)).collect()
}

/// What the search found.
#[derive(Debug, Clone)]
pub struct Found {
    pub recipe: Recipe,
    /// Song numbers, in order.
    pub songs: Vec<u8>,
    /// Why nothing was found, when nothing was.
    pub why: Option<String>,
}

/// Finds the sound routine of a game that played music during boot: the frame it was
/// verified at, and the routine.
fn find_play<M: Machine>(
    rom: &[u8],
    press_start: bool,
    log: &BootLog,
) -> Result<Option<(u32, u32)>, String> {
    if log.total_writes < 50 {
        return Ok(None);
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
    for (routine, _) in candidates {
        let Some((frames, total)) = log.entered_frames.get(&routine) else {
            continue;
        };
        // Called about once a frame from the start of the music on.
        let span = BOOT_FRAMES.saturating_sub(first_sound).max(1);
        let per_frame = *total as f64 / frames.len().max(1) as f64;
        if (frames.len() as f64) < 0.5 * span as f64 || per_frame > 1.5 {
            continue;
        }
        let latest = BOOT_FRAMES - VERIFY_FRAMES as u32 - 1;
        let mut at = (first_sound + 60).min(latest);
        while at <= latest && !frames.contains(&at) {
            at += 1;
        }
        if at > latest {
            continue;
        }
        let Some((mut m, entry)) = entry_at::<M>(rom, press_start, at, routine)? else {
            continue;
        };
        let Some(alone) = run_alone(&mut m, &entry, VERIFY_FRAMES) else {
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
            return Ok(Some((at, routine)));
        }
    }
    Ok(None)
}

/// Ways the game hands its sound code a number, seen during boot.
fn trigger_candidates<M: Machine>(
    rom: &[u8],
    press_start: bool,
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
    press_start: bool,
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

/// A song's first frames as heard: its writes' fingerprint, and whether anything sounded.
fn listen<M: Machine>(
    base: &M,
    play: &M::Entry,
    trigger: &Trigger,
    call: Option<&M::Entry>,
    v: u8,
    frames: usize,
) -> Option<(u64, bool)> {
    let mut m = base.clone();
    if !apply(&mut m, trigger, call, v) {
        return None;
    }
    let mut pre = m.restore_writes();
    pre.extend(m.take_sound().into_iter().map(|w| Write { at: 0, ..w }));
    let mut chip = m.new_chip(8000).ok()?;
    let mut samples = Vec::new();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut first = true;
    for _ in 0..frames {
        let mut writes = m.play_frame(play)?;
        frame_hash(&writes).hash(&mut h);
        if first {
            pre.append(&mut writes);
            writes = std::mem::take(&mut pre);
            first = false;
        }
        m.feed(&mut chip, &writes, &mut samples);
    }
    let loud = samples
        .iter()
        .filter(|s| s.unsigned_abs() > AUDIBLE)
        .count();
    Some((h.finish(), loud > samples.len() / 10))
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
    let mut why = "the game played no music in its first 15 seconds".to_string();
    for press_start in [false, true] {
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
            if press_start { " (Start)" } else { "" },
            log.total_writes,
            busiest.join(" ")
        ));
        let Some((at, play)) = find_play::<M>(rom, press_start, &log)? else {
            if log.total_writes >= 50 {
                why = "the routine that plays the music wasn't found".into();
            }
            continue;
        };
        let Some((base, entry)) = entry_at::<M>(rom, press_start, at, play)? else {
            continue;
        };
        let candidates = trigger_candidates::<M>(rom, press_start, play)?;
        note(format!(
            "sound routine {play:04x} (frame {at}); {} ways to start a song to try",
            candidates.len()
        ));
        // A quick round with a few numbers each, then all numbers for the best two ways.
        let baseline = listen(
            &base,
            &entry,
            &Trigger::Byte(u32::MAX),
            None,
            0,
            TRY_FRAMES / 2,
        )
        .map(|r| r.0);
        let mut screened: Vec<(usize, Trigger, Option<M::Entry>)> = Vec::new();
        for t in candidates {
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
                if let Some((h, audible)) =
                    listen(&base, &entry, &t, call.as_ref(), v, TRY_FRAMES / 2)
                {
                    if audible && Some(h) != baseline {
                        seen.insert(h);
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
        let mut best: Option<(Trigger, Vec<u8>)> = None;
        for (_, t, call) in screened.into_iter().take(2) {
            let mut songs = Vec::new();
            let mut seen = HashSet::new();
            for v in 0..=255u8 {
                if let Some((h, audible)) = listen(&base, &entry, &t, call.as_ref(), v, TRY_FRAMES)
                {
                    if audible && seen.insert(h) {
                        songs.push(v);
                    }
                }
            }
            note(format!("  {t:?}: {} songs", songs.len()));
            if best.as_ref().is_none_or(|b| songs.len() > b.1.len()) {
                best = Some((t, songs));
            }
        }
        if let Some((trigger, songs)) = best.filter(|b| !b.1.is_empty()) {
            let recipe = Recipe {
                boot_frames: at,
                press_start,
                play,
                trigger,
            };
            return Ok(Found {
                recipe,
                songs,
                why: None,
            });
        }
        why = "the sound routine was found, but not how the game picks a song".into();
    }
    Ok(Found {
        recipe: Recipe {
            boot_frames: 0,
            press_start: false,
            play: 0,
            trigger: Trigger::Byte(0),
        },
        songs: Vec::new(),
        why: Some(why),
    })
}

/// A song, ready to play: the machine at the sound routine, the song started.
pub struct Song<M: Machine> {
    m: M,
    play: M::Entry,
    chip: M::Chip,
    pre: Vec<Write>,
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
        Ok(Self { m, play, chip, pre })
    }

    pub fn frame_rate(&self) -> f64 {
        self.m.frame_rate()
    }

    /// One frame: its samples appended to `out` (interleaved when stereo), and the frame's
    /// sound writes.
    pub fn frame(&mut self, out: &mut Vec<i16>) -> Result<Vec<Write>, String> {
        let own = self
            .m
            .play_frame(&self.play)
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
            press_start: true,
            play: 0xC200,
            trigger: Trigger::Call {
                addr: 0x8123,
                reg: 1,
                nth: 2,
            },
        };
        assert_eq!(Recipe::decode(&r.encode()), Some(r));
        let r = Recipe {
            boot_frames: 5,
            press_start: false,
            play: 0x8000,
            trigger: Trigger::Byte(0x7F0),
        };
        assert_eq!(Recipe::decode(&r.encode()), Some(r));
    }
}
