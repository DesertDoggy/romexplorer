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
    /// Holds the buttons in `buttons` ([`START`], [`BUTTON_A`], ...). Consoles that only
    /// know Start hold just that.
    fn press(&mut self, buttons: u8) {
        self.press_start(buttons & START != 0);
    }
    /// [`Self::press`] holds more than Start (the A button patterns are worth a boot).
    const BUTTONS: bool = false;
    /// Games may take most of a minute to load (a disk), worth a minute-long boot when
    /// nothing else found music.
    fn slow_loading(&self) -> bool {
        false
    }
    /// The routine the CPU just entered, as an [`Self::Entry`].
    fn entry(&self) -> Self::Entry;
    /// One frame of the sound routine alone, called at its usual moment. None when it
    /// didn't return.
    fn play_frame(&mut self, play: &Self::Entry) -> Option<Vec<Write>>;
    /// Calls the routine of `entry` with register `reg` set to `v`, then puts the CPU and
    /// banks back as they were; its sound writes count at the current moment.
    fn call_with(&mut self, entry: &Self::Entry, reg: u8, v: u8) -> bool;
    /// Calls the routine at `addr` as [`Self::call_with`] does, from the moment of `ctx` (its
    /// banks and registers). For a song-start routine known from an original audio format
    /// file that the game itself may never be seen calling. False where unsupported.
    fn call_at(&mut self, _ctx: &Self::Entry, _addr: u32, _reg: u8, _v: u8) -> bool {
        false
    }
    /// `len` code bytes at `addr`, as the CPU sees them at the moment of `entry`; empty where
    /// unsupported.
    fn code_at(&mut self, _entry: &Self::Entry, _addr: u32, _len: usize) -> Vec<u8> {
        Vec::new()
    }
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

/// What a matched original audio format file (an NSF) says about where a game's sound code is: its play routine and its
/// song-start routine (each with the routines they call), by address with their first code
/// bytes, and its song count. Only addresses are taken from it; the music still plays
/// from the ROM.
#[derive(Debug, Clone, Default)]
pub struct Hint {
    pub play: Vec<(u32, Vec<u8>)>,
    pub start: Vec<(u32, Vec<u8>)>,
    pub songs: u8,
    /// Song-start routines (address, register) and the number each of the file's tracks
    /// passes them, in track order: the game's own song ids, where the file's init looks
    /// them up in a table of its own.
    pub ids: Vec<(u32, u8, Vec<u8>)>,
    /// The known answer: each track's [`melody`] over its first [`HINT_TRACK_FRAMES`]
    /// frames, as the file plays it, and how a song's melody is taken on this console.
    pub tracks: Vec<HashSet<u64>>,
    pub melody: Option<fn(&[Vec<Write>]) -> HashSet<u64>>,
}

/// Frames of each track a [`Hint`]'s known answer covers: as long as the search listens.
pub const HINT_TRACK_FRAMES: usize = SONG_FRAMES;

/// A melody's fingerprint: the runs of four pitches in a row on each tone channel, a channel
/// being the ports of its period's low byte and its high bits (the low 3 bits), as the writes
/// set them. Two plays of one song share most of theirs whatever their timing or volume, two
/// songs few. For checking found songs against an original audio format file's tracks.
pub fn melody(frames: &[Vec<Write>], channels: &[(u32, u32)]) -> HashSet<u64> {
    let mut lo = vec![0u16; channels.len()];
    let mut hi = vec![0u16; channels.len()];
    let mut last = vec![u16::MAX; channels.len()];
    let mut runs: Vec<Vec<u16>> = vec![Vec::new(); channels.len()];
    let mut out = HashSet::new();
    for f in frames {
        for w in f {
            let Some(ch) = channels.iter().position(|c| c.0 == w.port || c.1 == w.port) else {
                continue;
            };
            if channels[ch].0 == w.port {
                lo[ch] = w.value & 0xFF;
            } else {
                hi[ch] = w.value & 7;
            }
            let period = hi[ch] << 8 | lo[ch];
            if period == last[ch] || period < 8 {
                continue;
            }
            last[ch] = period;
            let run = &mut runs[ch];
            run.push(period);
            if run.len() >= 4 {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                (ch, &run[run.len() - 4..]).hash(&mut h);
                out.insert(h.finish());
            }
        }
    }
    out
}

/// Fewest steps a [`melody`] fingerprint needs to be compared: a few notes (an ending, a
/// near-silent track) are alike almost anything.
pub const MIN_MELODY: usize = 8;

/// How alike two [`melody`] fingerprints are: the share of the smaller one found in the
/// other (0 when either has fewer than [`MIN_MELODY`] steps).
pub fn alike(a: &HashSet<u64>, b: &HashSet<u64>) -> f64 {
    let small = a.len().min(b.len());
    if small < MIN_MELODY {
        return 0.0;
    }
    a.intersection(b).count() as f64 / small as f64
}

/// Most routines a [`Hint`] takes from each of play and init.
const HINT_ROUTINES: usize = 8;

/// `from`, then the routines it calls (as `calls_in` reads them), two levels down, each with
/// its code bytes (`code`; routines whose bytes aren't known are left out). For building a
/// [`Hint`] from an original audio format file's play and init addresses.
pub(crate) fn hint_routines(
    from: u16,
    calls_in: impl Fn(u16) -> Vec<u16>,
    code: impl Fn(u16) -> Option<Vec<u8>>,
) -> Vec<(u32, Vec<u8>)> {
    let mut out: Vec<(u16, u8)> = vec![(from, 0)];
    let mut i = 0;
    while i < out.len() && out.len() < HINT_ROUTINES {
        let (addr, level) = out[i];
        if level < 2 {
            for t in calls_in(addr) {
                if !out.iter().any(|o| o.0 == t) && out.len() < HINT_ROUTINES {
                    out.push((t, level + 1));
                }
            }
        }
        i += 1;
    }
    out.into_iter()
        .filter_map(|(a, _)| Some((a as u32, code(a)?)))
        .collect()
}

/// How the game is handed a song number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// Written to this RAM byte.
    Byte(u32),
    /// Passed in a register to the routine at this address, as called the `nth` time during
    /// boot.
    Call { addr: u32, reg: u8, nth: u32 },
    /// Passed in a register to the routine at this address, called from the sound routine's
    /// moment (an address an original audio format file gives).
    Jump { addr: u32, reg: u8 },
}

impl Trigger {
    fn encode(&self) -> String {
        match self {
            Trigger::Byte(a) => format!("m{a:04x}"),
            Trigger::Call { addr, reg, nth } => format!("c{addr:04x}.{reg}.{nth}"),
            Trigger::Jump { addr, reg } => format!("j{addr:04x}.{reg}"),
        }
    }

    fn decode(s: &str) -> Option<Self> {
        if let Some(a) = s.strip_prefix('m') {
            return u32::from_str_radix(a, 16).ok().map(Trigger::Byte);
        }
        if let Some(j) = s.strip_prefix('j') {
            let (a, r) = j.split_once('.')?;
            return Some(Trigger::Jump {
                addr: u32::from_str_radix(a, 16).ok()?,
                reg: r.parse().ok()?,
            });
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
        let press_start = p.next()?.parse().ok().filter(|&s| s <= LAST_INPUT)?;
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

/// The buttons held in frame `frame` of boot under input pattern `input` (a recipe's
/// `press_start`), from the PC probe of when games start their music:
/// - 0: nothing;
/// - 1: Start a few frames at the 3, 5 and 7 second marks (enough for most title screens, and
///   no presses after that to pause a game that is playing);
/// - 2: Start every two seconds from the third on;
/// - 3: Start, then A half a second later, every second from the third (with 4, the two that
///   start nearly every game that starts its music for some input);
/// - 4: A every second from the third;
/// - 5: nothing, for a longer boot (Disk System games loading from disk).
///
/// 0-2 boot 15 seconds, 3-4 30 seconds, 5 a minute (see [`boot_len`]).
fn held(input: u8, frame: u32) -> u8 {
    let every = |period: u32| frame >= 180 && frame % period < 6;
    match input {
        1 if (180..=420).contains(&frame) && every(120) => START,
        2 if every(120) => START,
        3 if every(60) => START,
        3 if frame >= 180 && (30..36).contains(&(frame % 60)) => BUTTON_A,
        4 if every(60) => BUTTON_A,
        _ => 0,
    }
}

/// How many frames boot runs under input pattern `input` (see [`held`]).
pub fn boot_len(input: u8) -> u32 {
    match input {
        0..=2 => BOOT_FRAMES,
        3 | 4 => 2 * BOOT_FRAMES,
        _ => 4 * BOOT_FRAMES,
    }
}

/// The last input pattern [`held`] knows.
const LAST_INPUT: u8 = 5;

/// Buttons for [`Machine::press`], in the Famicom's and Game Boy's order.
pub const BUTTON_A: u8 = 0x01;
pub const BUTTON_B: u8 = 0x02;
pub const SELECT: u8 = 0x04;
pub const START: u8 = 0x08;

/// Input patterns the probe tries: a name, and the buttons held in each frame.
pub const INPUTS: &[(&str, fn(u32) -> u8)] = &[
    ("none", |_| 0),
    ("start3", |f| if (180..=420).contains(&f) && f % 120 < 6 { START } else { 0 }),
    ("start2s", |f| if f >= 180 && f % 120 < 6 { START } else { 0 }),
    ("start1s", |f| if f >= 180 && f % 60 < 6 { START } else { 0 }),
    ("start+a", |f| match (f >= 180, f % 60) {
        (true, 0..=5) => START,
        (true, 30..=35) => BUTTON_A,
        _ => 0,
    }),
    ("a1s", |f| if f >= 180 && f % 60 < 6 { BUTTON_A } else { 0 }),
];

/// What a game does with sound under one input pattern: the first frame of steady sound
/// (sound writes in at least half the frames of a second), or None in `frames` frames.
pub fn probe<M: Machine>(rom: &[u8], input: fn(u32) -> u8, frames: u32) -> Result<Option<u32>, String> {
    let mut m = M::boot(rom)?;
    let window = 60usize;
    let mut busy: std::collections::VecDeque<bool> = std::collections::VecDeque::new();
    let mut count = 0usize;
    for f in 0..frames {
        m.press(input(f));
        m.run_frame(None, None);
        let any = m.take_sound().iter().any(|w| !M::incidental(w));
        busy.push_back(any);
        count += any as usize;
        if busy.len() > window {
            count -= busy.pop_front().unwrap() as usize;
        }
        if busy.len() == window && count * 2 >= window {
            return Ok(Some(f + 1 - window as u32));
        }
    }
    Ok(None)
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
/// How a way of starting songs ranks: the known answer's tracks it plays (when the search has
/// one, see [`Hint::tracks`]), songs that last, not so many numbers sounding different that
/// its songs themselves are past its table ([`CORE_SONGS`]), at least [`GOOD_SONGS`] songs, a
/// table that ends counting what sounds past its last song, then the number of songs.
type Rank = (usize, bool, bool, bool, bool, usize);
/// A song this alike a track of the known answer (see [`alike`]) is that track.
const TRACK_ALIKE: f64 = 0.5;
/// Numbers sounding different past which a way of starting songs is taken to read past
/// its song table.
const UNBOUNDED_SONGS: usize = 128;
/// Songs that last past which a way's song list itself reads past its table: more than any
/// cartridge of these consoles has tunes. (Jingles don't count: games put their sound
/// effects in the same table.)
const CORE_SONGS: usize = 100;
/// Past the gap that ends a way's songs, one number in this many is listened to, to tell
/// whether the way's numbers run out.
const PAST_GAP_STEP: usize = 4;
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
        m.press(held(press_start, f));
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
    for f in 0..boot_len(press_start) {
        m.press(held(press_start, f));
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
    m.press(held(press_start, frame));
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

/// Fewest boot frames an NSF-named routine must run in to be taken as the sound routine.
const HINT_MIN_FRAMES: usize = 30;
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
    /// Against a matched file's tracks (NSF, GBS): how many of them the songs play, and of
    /// how many. Some with 0 found: the songs are kept but unchecked (no song is like any of
    /// the file's tracks; often the file is from another version of the game). None: no
    /// file to check against.
    pub checked: Option<(usize, usize)>,
}

/// Finds the sound routine of a game that played music during boot: the routines that
/// verified (best first), each with the frame it was verified at and its calls a frame.
fn find_play<M: Machine>(
    rom: &[u8],
    press_start: u8,
    log: &BootLog,
    hint: Option<&Hint>,
    note: &mut dyn FnMut(String),
) -> Result<Vec<(u32, u32, u8)>, String> {
    let mut found = Vec::new();
    let first_sound = log
        .frames
        .iter()
        .position(|f| f.iter().any(|w| !M::incidental(w)))
        .unwrap_or(0) as u32;
    let latest = boot_len(press_start) - VERIFY_FRAMES as u32 - 1;
    // Routines a matched NSF names: the game's code at the same address, byte for byte, is
    // its sound routine even when it plays nothing yet (or its music can't be told apart
    // from the rest of the boot's sound).
    for (routine, code) in hint.map(|h| h.play.as_slice()).unwrap_or_default() {
        if found.len() == MAX_PLAYS {
            break;
        }
        let Some((frames, total)) = log.entered_frames.get(routine) else {
            continue;
        };
        if frames.len() < HINT_MIN_FRAMES {
            note(format!("  NSF routine {routine:04x}: entered in only {} frames", frames.len()));
            continue;
        }
        let calls = (*total as f64 / frames.len() as f64).round().clamp(1.0, MAX_CALLS) as u8;
        let mut at = (first_sound + 60).min(latest);
        while at <= latest && !frames.contains(&at) {
            at += 1;
        }
        if at > latest {
            at = (0..=latest).rev().find(|f| frames.contains(f)).unwrap_or(0);
        }
        let Some((mut m, entry)) = entry_at::<M>(rom, press_start, at, *routine)? else {
            continue;
        };
        if m.code_at(&entry, *routine, code.len()) != *code {
            note(format!("  NSF routine {routine:04x}: the game's code there differs"));
            continue;
        }
        if run_alone(&mut m, &entry, VERIFY_FRAMES, calls).is_none() {
            note(format!("  NSF routine {routine:04x}: crashed when run alone"));
            continue;
        }
        note(format!("  NSF routine {routine:04x}: the game's own, {calls} a frame"));
        found.push((at, *routine, calls));
    }
    if log.total_writes < 50 || found.len() == MAX_PLAYS {
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
    candidates.retain(|c| !found.iter().any(|f| f.1 == c.0));
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
        let span = boot_len(press_start).saturating_sub(first_sound).max(1);
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
    for f in 0..boot_len(press_start) {
        m.press(held(press_start, f));
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
            .filter(|&(_, n)| n < boot_len(press_start) / 2)
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
    for f in 0..boot_len(press_start) {
        m.press(held(press_start, f));
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
        Trigger::Jump { addr, reg } => call.is_some_and(|e| m.call_at(e, *addr, *reg, v)),
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
    /// Each frame's sound writes.
    writes: Vec<Vec<Write>>,
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
            writes: Vec::new(),
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
            self.writes.push(writes);
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
    search_hinted::<M>(rom, None, note)
}

/// [`search_logged`], pointed at the game's sound code by `hint` (from an original audio
/// format file that matched the ROM) when there is one.
pub fn search_hinted<M: Machine>(
    rom: &[u8],
    hint: Option<&Hint>,
    note: &mut dyn FnMut(String),
) -> Result<Found, String> {
    // The furthest the search got, over both boots.
    let mut why = (0, "the game played no music in its first 15 seconds");
    let started = std::time::Instant::now();
    let timed_out = || started.elapsed() > SEARCH_TIME_LIMIT;
    // The best result so far, ranked as songs that last first, then by count.
    let mut found: Option<(Rank, Found)> = None;
    let slow = M::boot(rom).map(|m| m.slow_loading()).unwrap_or(false);
    'boots: for press_start in 0..=LAST_INPUT {
        // Longer boots with other buttons only while nothing was found: the probe found
        // they start the music of most games still silent after the first three.
        let longer = match press_start {
            3 | 4 => M::BUTTONS,
            5 => slow,
            _ => true,
        };
        if !longer || (press_start >= 3 && found.is_some()) {
            continue;
        }
        if press_start >= 3 && why.0 == 0 {
            why.1 = if press_start == 5 {
                "the game played no music in its first minute"
            } else {
                "the game played no music in its first 30 seconds"
            };
        }
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
            [
                "",
                " (Start)",
                " (Start, steady)",
                " (Start and A, 30 s)",
                " (A, 30 s)",
                " (a minute)"
            ][press_start as usize],
            log.total_writes,
            busiest.join(" ")
        ));
        let plays = find_play::<M>(rom, press_start, &log, hint, note)?;
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
            let mut candidates = trigger_candidates::<M>(rom, press_start, play)?;
            // The NSF's song-start routines that are the game's own code, tried first.
            if let Some(h) = hint {
                let mut probe = base.clone();
                let jumps: Vec<Trigger> = h
                    .start
                    .iter()
                    .filter(|(a, code)| probe.code_at(&entry, *a, code.len()) == *code)
                    .flat_map(|(a, _)| (0..M::REGISTERS).map(move |reg| Trigger::Jump { addr: *a, reg }))
                    .collect();
                note(format!(
                    "NSF ({} songs): {} of {} song-start routines are the game's own code",
                    h.songs,
                    jumps.len() / M::REGISTERS.max(1) as usize,
                    h.start.len()
                ));
                candidates.splice(0..0, jumps);
            }
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
            // The exact song ids a Jump way takes, when the file's tracks give them.
            let ids_of = |t: &Trigger| -> Option<Vec<u8>> {
                let Trigger::Jump { addr, reg } = t else { return None };
                let (_, _, v) = hint?.ids.iter().find(|i| i.0 == *addr && i.1 == *reg)?;
                let mut out: Vec<u8> = Vec::new();
                for &x in v {
                    if !out.contains(&x) {
                        out.push(x);
                    }
                }
                Some(out)
            };
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
                    Trigger::Jump { .. } => Some(entry.clone()),
                    Trigger::Byte(_) => None,
                };
                let mut seen = HashSet::new();
                let tries: Vec<u8> = match ids_of(&t) {
                    Some(ids) => ids.into_iter().take(8).collect(),
                    None => (1..=8).collect(),
                };
                for v in tries {
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
            let mut best: Option<(Rank, Trigger, Vec<u8>, Option<usize>)> = None;
            for (i, (_, t, call)) in screened.into_iter().enumerate() {
                // The best few; more while none has a song table that ends.
                let settled = best.as_ref().is_some_and(|b| b.0 .2 && b.0 .3 && b.0 .4);
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
                let mut gap_at = None;
                // Each song's melody, to check against the known answer.
                let melody_of = hint.and_then(|h| h.melody).filter(|_| hint.is_some_and(|h| !h.tracks.is_empty()));
                let mut melodies: Vec<(u8, HashSet<u64>)> = Vec::new();
                let ids = ids_of(&t);
                let numbers: Vec<u8> = ids.clone().unwrap_or_else(|| (0..=255).collect());
                for v in numbers {
                    if timed_out() {
                        break;
                    }
                    // Songs end at the first long gap (see `song_run`): past it, nothing
                    // more can count. (Not for ids from the file: they are the songs.)
                    if ids.is_none() && played.last().is_some_and(|&l: &u8| (v - l) as usize > SONG_GAP) {
                        gap_at = Some(v);
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
                    if let (Some(f), true) = (melody_of, played.last() == Some(&v) || short.last() == Some(&v)) {
                        melodies.push((v, f(&l.writes)));
                    }
                    // Past this many songs that last, the way reads past its song table (see
                    // `core`): the rest would only be more of the same. Jingles don't count: a
                    // way still finding its first song that lasts may yet beat others.
                    if ids.is_none() && played.len() >= CORE_SONGS {
                        break;
                    }
                }
                // Whether the numbers past the gap run out too: a quick listen to every
                // `PAST_GAP_STEP`th one, each new sound counting for that many.
                let mut past_gap = 0;
                if let Some(from) = gap_at {
                    for v in (from as usize..=255).step_by(PAST_GAP_STEP) {
                        if timed_out() {
                            break;
                        }
                        if listen(&base, &entry, &t, call.as_ref(), v as u8, TRY_FRAMES, calls)
                            .is_some_and(|h| h.audible && seen.insert(h.sound))
                        {
                            past_gap += PAST_GAP_STEP;
                        }
                    }
                }
                // A game whose every tune is a few seconds long (older games are mostly
                // jingles): its short ones are its music.
                let songs = if let Some(ids) = &ids {
                    // The file's tracks, in its order: jingles too, since it lists them.
                    ids.iter().copied().filter(|v| played.contains(v) || short.contains(v)).collect()
                } else if played.is_empty() {
                    song_run(&short)
                } else {
                    song_run(&played)
                };
                note(format!(
                    "  {t:?}: {} songs ({} numbers played music, {} jingles; left out: {} crashed, {} silent, \
                     {} same as another, {} crashed later; ~{} more sound different past the gap)",
                    songs.len(),
                    played.len(),
                    short.len(),
                    dropped[0],
                    dropped[1],
                    dropped[2],
                    dropped[3],
                    past_gap,
                ));
                // A way where most numbers sound different reads past the end of its song
                // table (or isn't a song number at all): its "songs" are noise from whatever
                // follows. A way with a table that ends beats it.
                let bounded = played.len() + short.len() + past_gap < UNBOUNDED_SONGS;
                // ... but only between ways that both have a real song list: numbers past a
                // game's songs are often its sound effects, which sound different too.
                let list = played.len() >= GOOD_SONGS;
                let core = played.len() < CORE_SONGS;
                // Against the known answer: each track once, by the song most like it; songs
                // like no track (variants of one, sound effects, junk) are left out.
                let checked = hint.filter(|h| !h.tracks.is_empty() && !melodies.is_empty()).map(|h| h.tracks.len());
                let (songs, tracks_found) = match hint.filter(|h| !h.tracks.is_empty()) {
                    Some(h) if !melodies.is_empty() => {
                        let mut by_track: Vec<Option<(f64, u8)>> = vec![None; h.tracks.len()];
                        for (v, mine) in &melodies {
                            for (i, track) in h.tracks.iter().enumerate() {
                                let a = alike(mine, track);
                                if a >= TRACK_ALIKE && by_track[i].is_none_or(|b| a > b.0) {
                                    by_track[i] = Some((a, *v));
                                }
                            }
                        }
                        let mut kept: Vec<u8> = Vec::new();
                        for (_, v) in by_track.iter().flatten() {
                            if !kept.contains(v) {
                                kept.push(*v);
                            }
                        }
                        let n = by_track.iter().flatten().count();
                        note(format!("    like the known answer: {n} of {} tracks, by {} songs", h.tracks.len(), kept.len()));
                        // None like a track: kept, as unchecked (see `Found::checked`).
                        if kept.is_empty() { (songs, 0) } else { (kept, n) }
                    }
                    _ => (songs, 0),
                };
                let rank = (tracks_found, !played.is_empty(), core, list, bounded, songs.len());
                if best.as_ref().is_none_or(|b| rank > b.0) {
                    best = Some((rank, t, songs, checked));
                }
            }
            if let Some((rank, trigger, songs, checked)) = best.filter(|b| !b.2.is_empty()) {
                let recipe = Recipe {
                    boot_frames: at,
                    press_start,
                    play,
                    trigger,
                    calls,
                };
                let good = rank.2 && rank.3 && rank.4;
                if found.as_ref().is_none_or(|f: &(Rank, Found)| rank > f.0) {
                    found = Some((
                        rank,
                        Found {
                            recipe,
                            songs,
                            why: None,
                            checked: checked.map(|of| (rank.0, of)),
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
        checked: None,
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
            Trigger::Jump { .. } => Some(play.clone()),
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
    let rate = song.frame_rate();
    measure_frames(rate, |samples| song.frame(samples))
}

/// [`measure`] for any song given frame by frame (`next` appends a frame's samples, at 8 kHz,
/// and gives its sound writes), at `frame_rate` frames a second.
pub fn measure_frames(frame_rate: f64, mut next: impl FnMut(&mut Vec<i16>) -> Result<Vec<Write>, String>) -> Result<Timing, String> {
    let max_frames = (MEASURE_FRAMES as f64 * frame_rate / 60.0) as usize;
    let mut hashes: Vec<u64> = Vec::new();
    let mut last_loud = 0usize;
    let mut samples = Vec::new();
    let mut windows: HashMap<u64, usize> = HashMap::new();
    for f in 0..max_frames {
        samples.clear();
        let writes = next(&mut samples)?;
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

/// Every write, port and value, for [`measure_frames_by_writes`].
pub fn every_write(w: &Write) -> Option<u64> {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (w.port, w.value).hash(&mut h);
    Some(h.finish())
}

/// [`measure_frames`] for a player whose play routine runs at its own timer's rate, so one
/// frame holds one call and the next two: the frames never repeat even when the song does.
/// The loop is found in the stream of writes instead, with the frames left out of it, each
/// write as `note` sees it (None: not part of the notes; a driver's vibrato or duty sweep can
/// run on a count that doesn't restart with the loop). [`every_write`] keeps them all.
pub fn measure_frames_by_writes(
    frame_rate: f64,
    mut note: impl FnMut(&Write) -> Option<u64>,
    mut next: impl FnMut(&mut Vec<i16>) -> Result<Vec<Write>, String>,
) -> Result<Timing, String> {
    const BASE: u64 = 0x100_0000_01B3;
    let max_frames = (MEASURE_FRAMES as f64 * frame_rate / 60.0) as usize;
    let mut last_loud = 0usize;
    let mut samples = Vec::new();
    // Each write as one token, and the frame it came in.
    let mut tokens: Vec<u64> = Vec::new();
    let mut frame_of: Vec<usize> = Vec::new();
    let mut seen: HashMap<u64, usize> = HashMap::new();
    // How many writes a window holds: those of the first LOOP_WINDOW frames.
    let mut window = 0usize;
    let mut drop = 1u64;
    let mut rolling = 0u64;
    for f in 0..max_frames {
        samples.clear();
        let writes = next(&mut samples)?;
        if samples.iter().any(|s| s.unsigned_abs() > AUDIBLE) {
            last_loud = f;
        }
        if f > last_loud + SILENT_END_FRAMES {
            return Ok(Timing { frames: last_loud + 1, looping: None });
        }
        for t in writes.iter().filter_map(&mut note) {
            tokens.push(t);
            frame_of.push(f);
            if window == 0 {
                continue;
            }
            let end = tokens.len();
            rolling = rolling.wrapping_mul(BASE).wrapping_add(tokens[end - 1]);
            if end > window {
                rolling = rolling.wrapping_sub(tokens[end - 1 - window].wrapping_mul(drop));
            }
            if end < window {
                continue;
            }
            let start = end - window;
            match seen.get(&rolling) {
                Some(&first) if frame_of[start] - frame_of[first] >= MIN_LOOP_FRAMES && tokens[first..first + window] == tokens[start..end] => {
                    let (a, b) = (frame_of[first], frame_of[start]);
                    return Ok(Timing { frames: b, looping: Some((a, b - a)) });
                }
                Some(_) => {}
                None => {
                    seen.insert(rolling, start);
                }
            }
        }
        if window == 0 && f + 1 >= LOOP_WINDOW && tokens.len() >= 16 {
            window = tokens.len();
            drop = (0..window).fold(1u64, |d, _| d.wrapping_mul(BASE));
            // The windows so far, now that their length is known.
            rolling = 0;
            for (i, &t) in tokens.iter().enumerate() {
                rolling = rolling.wrapping_mul(BASE).wrapping_add(t);
                if i >= window {
                    rolling = rolling.wrapping_sub(tokens[i - window].wrapping_mul(drop));
                }
            }
            seen.insert(rolling, 0);
        }
    }
    Ok(Timing { frames: max_frames, looping: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_loop_is_found_in_the_writes_when_the_frames_never_repeat() {
        // A 100-call tune, after a 30-call intro, played 1.37 calls a frame.
        let call = |n: usize| -> Vec<Write> {
            let note = if n < 30 { 900 + n as u16 } else { ((n - 30) % 100) as u16 * 7 % 61 };
            vec![Write { at: 0, port: 0x10, value: note & 0xFF }, Write { at: 0, port: 0x11, value: note >> 8 }]
        };
        let (mut due, mut n) = (0.0f64, 0usize);
        let t = measure_frames_by_writes(60.0, every_write, |out| {
            out.clear();
            out.resize(10, 1000);
            due += 1.37;
            let mut w = Vec::new();
            while due >= 1.0 {
                due -= 1.0;
                w.extend(call(n));
                n += 1;
            }
            Ok(w)
        })
        .unwrap();
        let (start, len) = t.looping.expect("a loop");
        // 100 calls at 1.37 a frame, give or take a frame.
        assert!((72..=74).contains(&len), "{len}");
        assert!(start <= 30, "{start}");
    }

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
        let r = Recipe {
            boot_frames: 60,
            press_start: 1,
            play: 0xC081,
            trigger: Trigger::Jump { addr: 0xA06B, reg: 0 },
            calls: 1,
        };
        assert_eq!(r.encode(), "60@1@c081@ja06b.0");
        assert_eq!(Recipe::decode(&r.encode()), Some(r));
    }
}
