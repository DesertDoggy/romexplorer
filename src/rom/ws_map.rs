//! A WonderSwan game's music map, from a matched WSR file and the ROM: the WonderSwan
//! counterpart of `gb_map` and `hes_map` (see there).
//!
//! A WSR is a cartridge image booted with the song number in AX. Almost all of it is the
//! game's own bytes; the rest is a short stub its author wrote (the boot, the interrupt
//! handlers) that calls into the game's sound code. The map keeps where the file's bytes are
//! in the ROM, and in place of the stub what it did each time the CPU entered it: the RAM and
//! port writes it made and the registers it left. Only numbers are kept, and the music plays
//! from the user's ROM with the file's layout rebuilt from it.

use std::collections::HashMap;

use super::music_map::Run;
use super::music_search::{self, Write};
use super::v30mz::AX;
use super::wswan_music::{WonderSwan, FRAME};

/// Whether `d` is a WSR: `WSRF` 32 bytes from its end.
pub fn is_wsr(d: &[u8]) -> bool {
    d.len() >= 32 && &d[d.len() - 32..d.len() - 28] == b"WSRF"
}

/// The song number a WSR starts from (its header's first song).
pub fn first_song(d: &[u8]) -> u8 {
    d[d.len() - 32 + 5]
}

/// The cartridge a WSR of `len` bytes fills, as the player lays it: a power of two, at least
/// 64 KB, with the file at its top.
fn image_size(len: usize) -> usize {
    (len.div_ceil(0x10000) * 0x10000).max(0x10000).next_power_of_two()
}

fn image_of(data: &[u8]) -> Vec<u8> {
    let size = image_size(data.len());
    let mut v = vec![0xFF; size];
    v[size - data.len()..].copy_from_slice(data);
    v
}

/// A write the file's own code made.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Op {
    Poke(u16, u8),
    Out(u8, u8),
}

/// The CPU's registers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Regs {
    pub r: [u16; 8],
    pub seg: [u16; 4],
    pub ip: u16,
    pub flags: u16,
}

/// One run of the file's own code: from the CPU entering it until it leaves for the game's
/// code or sleeps. Its writes, the registers it left, whether it slept, the cycles it took.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Script {
    pub ops: Vec<Op>,
    pub end: Regs,
    pub halt: bool,
    /// Cut short by an interrupt (the code was waiting for one): played back, the interrupt
    /// is waited for before anything else runs.
    pub cut: bool,
    pub cycles: u32,
}

/// Each place the file's code was entered (a cartridge offset), and the scripts it ran there
/// in order: `head` once, then `cycle` over and over.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entries {
    pub at: Vec<(u32, Vec<u32>, Vec<u32>)>,
}

/// What fills the file's bytes no run places.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill<'a> {
    Zeros,
    /// A gap between two runs at the same distance in the ROM: the ROM's bytes there.
    Bridged,
    /// The file's own bytes (an oracle, for checks).
    File(&'a [u8]),
}

/// A WonderSwan game's music map: numbers only, nothing of the game or the file.
#[derive(Debug, Clone, Default)]
pub struct Map {
    pub data_len: usize,
    pub runs: Vec<Run>,
    pub scripts: Vec<Script>,
    /// Per song number kept: its entries.
    pub songs: Vec<(u8, Entries)>,
    /// Bytes of the file's own the game's code reads (a bank number or pointer its author
    /// changed): data offset and value.
    pub patches: Vec<(u32, u8)>,
}

/// Frames each song is recorded for. Past them its scripts repeat their cycle.
pub const RECORD_FRAMES: usize = 900;
/// A script longer than this (a busy wait with interrupts off) is cut.
const MAX_SCRIPT_CYCLES: u32 = FRAME * 2;

/// The sound unit's four channels, by their pitch ports.
const CHANNELS: [(u32, u32); 4] = [(0x80, 0x81), (0x82, 0x83), (0x84, 0x85), (0x86, 0x87)];

/// A melody's fingerprint (see [`music_search::melody`]) on the four channels.
pub fn melody(frames: &[Vec<Write>]) -> std::collections::HashSet<u64> {
    music_search::melody(frames, &CHANNELS)
}

/// Which of the cartridge's bytes are the file's own (no run nor bridged gap places them).
fn own_bytes(data_len: usize, runs: &[Run], fill: Fill) -> Vec<bool> {
    let size = image_size(data_len);
    let base = size - data_len;
    let mut own = vec![false; size];
    own[base..].iter_mut().for_each(|b| *b = true);
    let mut set = |from: usize, to: usize| own[base + from..base + to].iter_mut().for_each(|b| *b = false);
    for r in runs {
        set(r.data, r.data + r.len);
    }
    if fill == Fill::Bridged {
        for w in runs.windows(2) {
            let (a, b) = (w[0], w[1]);
            if b.data > a.data + a.len && b.rom as isize - b.data as isize == a.rom as isize - a.data as isize {
                set(a.data + a.len, b.data);
            }
        }
    }
    own
}

/// The file's data as the map rebuilds it from the ROM.
fn rebuilt(data_len: usize, runs: &[Run], patches: &[(u32, u8)], rom: &[u8], fill: Fill) -> Vec<u8> {
    let mut data = match fill {
        Fill::File(d) => d.to_vec(),
        _ => vec![0u8; data_len],
    };
    data.resize(data_len, 0);
    for r in runs {
        if let Some(src) = rom.get(r.rom..r.rom + r.len) {
            data[r.data..r.data + r.len].copy_from_slice(src);
        }
    }
    if fill == Fill::Bridged {
        for w in runs.windows(2) {
            let (a, b) = (w[0], w[1]);
            if b.data > a.data + a.len && b.rom as isize - b.data as isize == a.rom as isize - a.data as isize {
                let from = a.rom + a.len;
                if let Some(src) = rom.get(from..from + (b.data - a.data - a.len)) {
                    data[a.data + a.len..b.data].copy_from_slice(src);
                }
            }
        }
    }
    for &(d, v) in patches {
        if let Some(b) = data.get_mut(d as usize) {
            *b = v;
        }
    }
    data
}

fn console(image: Vec<u8>, song: u8) -> WonderSwan {
    let mut ws = WonderSwan::with_image(image, true, 0);
    ws.cpu_mut().r[AX] = song as u16;
    ws
}

fn regs(ws: &WonderSwan) -> Regs {
    let c = ws.cpu();
    Regs { r: c.r, seg: c.seg, ip: c.ip, flags: c.flags }
}

fn is_own(ws: &WonderSwan, own: &[bool]) -> bool {
    ws.rom_offset(ws.cpu().pc()).is_some_and(|o| own.get(o).copied().unwrap_or(false))
}

/// Interrupts first: one due is taken, a sleeping CPU woken by one it masks.
fn interrupts(ws: &mut WonderSwan) {
    if ws.interrupt_due() || ws.cpu().halted {
        ws.dispatch();
    }
}

/// Plays song `song` of the file (its data `data`) for `frames` frames, recording what its
/// own code (`own`) does each time it is entered. The frames' sound writes, and per entry
/// the scripts' numbers in `table`.
fn record(
    data: &[u8],
    own: &[bool],
    song: u8,
    frames: usize,
    table: &mut Vec<Script>,
    index: &mut HashMap<Script, u32>,
    mut events: Option<&mut Vec<String>>,
    mut read: Option<(&[bool], &mut std::collections::BTreeSet<usize>)>,
) -> (Vec<Vec<Write>>, Vec<(u32, Vec<u32>)>) {
    let mut ws = console(image_of(data), song);
    ws.log_cart(read.is_some());
    let mut seqs: Vec<(u32, Vec<u32>)> = Vec::new();
    let mut out = Vec::with_capacity(frames);
    // The script being recorded: where it was entered, its writes, its cycles.
    let mut cur: Option<(u32, Vec<Op>, u32)> = None;
    let mut finish = |ws: &mut WonderSwan, cur: &mut Option<(u32, Vec<Op>, u32)>, events: &mut Option<&mut Vec<String>>, frame: usize, cut: bool| {
        let Some((key, ops, cycles)) = cur.take() else { return };
        ws.log(false);
        let s = Script { ops, end: regs(ws), halt: ws.cpu().halted, cut, cycles };
        if let Some(e) = events.as_deref_mut() {
            e.push(format!("f{frame} c{} script at {key:X}: {}", ws.cycle(), describe(&s)));
        }
        let n = *index.entry(s.clone()).or_insert_with(|| {
            table.push(s);
            table.len() as u32 - 1
        });
        match seqs.iter_mut().find(|e| e.0 == key) {
            Some(e) => e.1.push(n),
            None => seqs.push((key, vec![n])),
        }
    };
    for frame in 0..frames {
        ws.begin_frame();
        while !ws.frame_over() {
            if ws.interrupt_due() {
                // Interrupted in the file's code: the script so far ends here.
                finish(&mut ws, &mut cur, &mut events, frame, true);
                if let Some(e) = events.as_deref_mut() {
                    e.push(format!("f{frame} c{} interrupt at {:X}", ws.cycle(), ws.cpu().pc()));
                }
            }
            interrupts(&mut ws);
            if ws.cpu().halted {
                ws.exec();
                continue;
            }
            if cur.is_none() && is_own(&ws, own) {
                let key = ws.rom_offset(ws.cpu().pc()).unwrap_or(0) as u32;
                cur = Some((key, Vec::new(), 0));
                ws.log(true);
            }
            let in_script = cur.is_some();
            let t = ws.exec();
            if let Some((unplaced, read)) = read.as_mut() {
                // The bytes no run places that the game's code runs or reads (and the sound DMA
                // plays): its own where a gap is bridged, else the file's.
                let reads = ws.log_cart(true);
                if !in_script {
                    read.extend(reads.into_iter().map(|o| o as usize).filter(|&o| unplaced.get(o).copied().unwrap_or(false)));
                }
            }
            if let Some((_, ops, cycles)) = cur.as_mut() {
                let (ram, outs) = ws.take_log();
                // A general DMA's own RAM writes come again from the port write that starts it.
                if !outs.iter().any(|&(p, v)| p == 0x48 && v & 0x80 != 0) {
                    ops.extend(ram.into_iter().map(|(a, v)| Op::Poke(a, v)));
                }
                ops.extend(outs.into_iter().map(|(p, v)| Op::Out(p, v)));
                *cycles += t;
                if ws.cpu().halted || !is_own(&ws, own) || *cycles > MAX_SCRIPT_CYCLES {
                    finish(&mut ws, &mut cur, &mut events, frame, false);
                }
            }
        }
        out.push(ws.sound_writes());
    }
    // A script the recording's end cut short is left out: it ends nowhere the code was ever
    // entered again.
    ws.log(false);
    (out, seqs)
}

/// `seq` as a head and a cycle repeated after it: the shortest both together.
fn head_and_cycle(seq: &[u32]) -> (Vec<u32>, Vec<u32>) {
    let mut best = (seq.len(), 0usize);
    for p in 1..=64.min(seq.len()) {
        // From where on each entry is the one `p` before.
        let mut start = seq.len();
        while start > p && seq[start - 1] == seq[start - 1 - p] {
            start -= 1;
        }
        let start = start - p;
        if best.1 == 0 || start + p < best.0 + best.1 {
            best = (start, p);
        }
    }
    let (start, p) = best;
    (seq[..start].to_vec(), seq[start..start + p].to_vec())
}

/// The music map of a WonderSwan ROM for the WSR it matches, over `songs`. None when the
/// WSR places nothing in the ROM.
pub fn map(wsr: &[u8], rom: &[u8], songs: impl Iterator<Item = u8>) -> Option<Map> {
    let layout = super::music_map::layout(wsr, &[], rom);
    if layout.runs.is_empty() {
        return None;
    }
    let own = own_bytes(wsr.len(), &layout.runs, Fill::Bridged);
    let mut m = Map { data_len: wsr.len(), runs: layout.runs, ..Default::default() };
    let mut index = HashMap::new();
    let mut read = std::collections::BTreeSet::new();
    let unplaced = own_bytes(wsr.len(), &m.runs, Fill::Zeros);
    for song in songs {
        let (frames, seqs) = record(wsr, &own, song, RECORD_FRAMES, &mut m.scripts, &mut index, None, Some((&unplaced, &mut read)));
        if melody(&frames).len() < music_search::MIN_MELODY {
            continue;
        }
        let at = seqs.into_iter().map(|(k, s)| {
            let (h, c) = head_and_cycle(&s);
            (k, h, c)
        });
        m.songs.push((song, Entries { at: at.collect() }));
    }
    // Kept where the file differs from the layout bridged from the ROM.
    let base = image_size(wsr.len()) - wsr.len();
    let bridged = rebuilt(m.data_len, &m.runs, &[], rom, Fill::Bridged);
    m.patches = read
        .into_iter()
        .filter(|&o| o >= base && wsr[o - base] != bridged[o - base])
        .map(|o| ((o - base) as u32, wsr[o - base]))
        .collect();
    Some(m)
}

/// The file's own sound writes for song `song`: the known answer.
pub fn track_writes(wsr: &[u8], song: u8, frames: usize) -> Vec<Vec<Write>> {
    let own = vec![false; image_size(wsr.len())];
    record(wsr, &own, song, frames, &mut Vec::new(), &mut HashMap::new(), None, None).0
}

/// Song `song` played by the map from the ROM: each frame's sound writes. Stops early where
/// the CPU enters the file's code somewhere the map never saw it.
pub fn map_track_writes(m: &Map, rom: &[u8], song: u8, frames: usize, fill: Fill) -> Option<Vec<Vec<Write>>> {
    replay(m, rom, song, frames, fill, None)
}

fn replay(m: &Map, rom: &[u8], song: u8, frames: usize, fill: Fill, mut events: Option<&mut Vec<String>>) -> Option<Vec<Vec<Write>>> {
    let mut p = LayoutSong::open(m.clone(), rom, song, fill)?;
    let mut out = Vec::with_capacity(frames);
    for _ in 0..frames {
        match p.frame(events.as_deref_mut()) {
            Some(w) => out.push(w),
            None => break,
        }
    }
    Some(out)
}

/// A song played by its map on the file's layout rebuilt from the ROM, a frame at a time.
pub struct LayoutSong {
    m: Map,
    song: usize,
    own: Vec<bool>,
    ws: WonderSwan,
    count: HashMap<u32, usize>,
    /// Cycles a script took that are still to pass, a scanline at a time: the time it ran,
    /// over which nothing interrupted it.
    idle: u32,
    /// Waiting for the interrupt that cut the last script short.
    wait: bool,
    frame: usize,
    stopped: bool,
}

impl LayoutSong {
    pub fn open(m: Map, rom: &[u8], song: u8, fill: Fill) -> Option<Self> {
        let index = m.songs.iter().position(|s| s.0 == song)?;
        let data = rebuilt(m.data_len, &m.runs, &m.patches, rom, fill);
        let own = own_bytes(m.data_len, &m.runs, Fill::Bridged);
        let ws = console(image_of(&data), song);
        Some(Self { m, song: index, own, ws, count: HashMap::new(), idle: 0, wait: false, frame: 0, stopped: false })
    }

    /// The next frame's sound writes; None once the CPU enters the file's code somewhere the
    /// map never saw it.
    pub fn frame(&mut self, mut events: Option<&mut Vec<String>>) -> Option<Vec<Write>> {
        if self.stopped {
            return None;
        }
        let frame = self.frame;
        self.frame += 1;
        let ws = &mut self.ws;
        ws.begin_frame();
        while !ws.frame_over() {
            if self.idle > 0 {
                let t = self.idle.min(ws.to_next_line());
                ws.advance(t);
                self.idle -= t;
                continue;
            }
            if self.wait {
                if !ws.interrupt_due() {
                    let t = ws.to_next_line();
                    ws.advance(t);
                    continue;
                }
                self.wait = false;
            }
            if let Some(e) = events.as_deref_mut() {
                if ws.interrupt_due() {
                    e.push(format!("f{frame} c{} interrupt at {:X}", ws.cycle(), ws.cpu().pc()));
                }
            }
            interrupts(ws);
            if ws.cpu().halted {
                ws.exec();
                continue;
            }
            if !is_own(ws, &self.own) {
                ws.exec();
                continue;
            }
            let key = ws.rom_offset(ws.cpu().pc()).unwrap_or(0) as u32;
            let entries = &self.m.songs[self.song].1;
            let Some((_, head, cycle)) = entries.at.iter().find(|e| e.0 == key) else {
                if let Some(e) = events.as_deref_mut() {
                    e.push(format!("f{frame} c{} entered at {key:X}, never seen", ws.cycle()));
                }
                self.stopped = true;
                return None;
            };
            let n = self.count.entry(key).or_default();
            let i = match head.get(*n) {
                Some(&i) => i,
                None if !cycle.is_empty() => cycle[(*n - head.len()) % cycle.len()],
                None => {
                    self.stopped = true;
                    return None;
                }
            };
            *n += 1;
            let s = &self.m.scripts[i as usize];
            for op in &s.ops {
                match *op {
                    Op::Poke(a, v) => ws.ram_write(a, v),
                    Op::Out(p, v) => ws.out(p, v),
                }
            }
            let c = ws.cpu_mut();
            c.r = s.end.r;
            c.seg = s.end.seg;
            c.ip = s.end.ip;
            c.flags = s.end.flags;
            c.halted = s.halt;
            self.idle = s.cycles;
            self.wait = s.cut;
            if let Some(e) = events.as_deref_mut() {
                e.push(format!("f{frame} c{} script at {key:X}: {}", ws.cycle() + s.cycles, describe(s)));
            }
        }
        Some(ws.sound_writes())
    }
}

/// A song from the user's ROM by its map, as sound (stereo).
pub struct MapSong {
    song: LayoutSong,
    chip: super::wswan_music::Chip,
}

impl MapSong {
    pub fn open(m: &Map, rom: &[u8], song: u8, rate: u32) -> Result<Self, String> {
        use super::music_search::Machine;
        let song = LayoutSong::open(m.clone(), rom, song, Fill::Bridged).ok_or("the map has no such song")?;
        let chip = song.ws.new_chip(rate)?;
        Ok(Self { song, chip })
    }

    pub fn frame_rate(&self) -> f64 {
        super::wswan_music::RATE
    }

    pub fn frame(&mut self, out: &mut Vec<i16>) -> Option<Vec<Write>> {
        use super::music_search::Machine;
        let w = self.song.frame(None)?;
        self.song.ws.feed(&mut self.chip, &w, out);
        Some(w)
    }
}

/// How alike a song plays from the ROM to the file's own for it to be kept.
pub const MAP_RIGHT: f64 = 0.9;

/// The map, keeping only the songs it plays right from the ROM (gaps bridged), checked over
/// the frames recorded. None when it plays none.
pub fn build_map(wsr: &[u8], rom: &[u8], songs: impl Iterator<Item = u8>) -> Option<Map> {
    let mut m = map(wsr, rom, songs)?;
    let right: Vec<u8> = m
        .songs
        .iter()
        .filter(|s| {
            let want = melody(&track_writes(wsr, s.0, RECORD_FRAMES));
            let got = map_track_writes(&m, rom, s.0, RECORD_FRAMES, Fill::Bridged).map(|f| melody(&f)).unwrap_or_default();
            music_search::alike(&want, &got) >= MAP_RIGHT
        })
        .map(|s| s.0)
        .collect();
    m.songs.retain(|s| right.contains(&s.0));
    if m.songs.is_empty() {
        return None;
    }
    // Scripts no kept song uses go, the rest renumbered.
    let mut used: Vec<u32> = m.songs.iter().flat_map(|s| s.1.at.iter().flat_map(|e| e.1.iter().chain(&e.2).copied())).collect();
    used.sort_unstable();
    used.dedup();
    let new: HashMap<u32, u32> = used.iter().enumerate().map(|(i, &o)| (o, i as u32)).collect();
    m.scripts = used.iter().map(|&o| m.scripts[o as usize].clone()).collect();
    for s in &mut m.songs {
        for e in &mut s.1.at {
            e.1.iter_mut().chain(e.2.iter_mut()).for_each(|i| *i = new[i]);
        }
    }
    Some(m)
}

impl Map {
    /// The map as text: numbers only.
    pub fn encode(&self) -> String {
        let list = |v: &[u32]| v.iter().map(|i| format!("{i:x}")).collect::<Vec<_>>().join(".");
        let mut out = vec![
            "wsmap 1".to_string(),
            format!("d {:x}", self.data_len),
            format!("r {}", self.runs.iter().map(|r| format!("{:x}:{:x}:{:x}", r.data, r.len, r.rom)).collect::<Vec<_>>().join(" ")),
            format!("p {}", self.patches.iter().map(|(d, v)| format!("{d:x}:{v:x}")).collect::<Vec<_>>().join(" ")),
        ];
        for s in &self.scripts {
            let e = &s.end;
            let regs: Vec<String> = e.r.iter().chain(&e.seg).chain([&e.ip, &e.flags]).map(|v| format!("{v:x}")).collect();
            let ops: Vec<String> = s
                .ops
                .iter()
                .map(|o| match *o {
                    Op::Poke(a, v) => format!("P{a:x}:{v:x}"),
                    Op::Out(p, v) => format!("O{p:x}:{v:x}"),
                })
                .collect();
            out.push(format!("x {} {:x} {} {}", if s.cut { 2 } else { s.halt as u8 }, s.cycles, regs.join(","), ops.join(" ")).trim_end().to_string());
        }
        for (song, e) in &self.songs {
            let at: Vec<String> = e.at.iter().map(|(k, h, c)| format!("{k:x}={}/{}", list(h), list(c))).collect();
            out.push(format!("s {song} {}", at.join(" ")));
        }
        out.join("\n")
    }

    pub fn decode(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != "wsmap 1" {
            return None;
        }
        let num = |t: &str| u32::from_str_radix(t, 16).ok();
        let list = |t: &str| -> Option<Vec<u32>> { if t.is_empty() { Some(Vec::new()) } else { t.split('.').map(num).collect() } };
        let mut m = Map::default();
        for line in lines {
            let (tag, rest) = line.split_once(' ').unwrap_or((line, ""));
            match tag {
                "d" => m.data_len = num(rest)? as usize,
                "r" => {
                    for r in rest.split_whitespace() {
                        let f: Vec<u32> = r.split(':').map(num).collect::<Option<_>>()?;
                        let [data, len, rom] = f[..] else { return None };
                        m.runs.push(Run { data: data as usize, len: len as usize, rom: rom as usize });
                    }
                }
                "p" => {
                    for p in rest.split_whitespace() {
                        let (d, v) = p.split_once(':')?;
                        m.patches.push((num(d)?, num(v)? as u8));
                    }
                }
                "x" => {
                    let mut f = rest.split_whitespace();
                    let state = f.next()?;
                    let (halt, cut) = (state == "1", state == "2");
                    let cycles = num(f.next()?)?;
                    let regs: Vec<u16> = f.next()?.split(',').map(|v| num(v).map(|v| v as u16)).collect::<Option<_>>()?;
                    if regs.len() != 14 {
                        return None;
                    }
                    let mut end = Regs { r: [0; 8], seg: [0; 4], ip: regs[12], flags: regs[13] };
                    end.r.copy_from_slice(&regs[..8]);
                    end.seg.copy_from_slice(&regs[8..12]);
                    let mut ops = Vec::new();
                    for o in f {
                        let (a, v) = o[1..].split_once(':')?;
                        ops.push(match &o[..1] {
                            "P" => Op::Poke(num(a)? as u16, num(v)? as u8),
                            "O" => Op::Out(num(a)? as u8, num(v)? as u8),
                            _ => return None,
                        });
                    }
                    m.scripts.push(Script { ops, end, halt, cut, cycles });
                }
                "s" => {
                    let mut f = rest.split_whitespace();
                    let song: u8 = f.next()?.parse().ok()?;
                    let mut e = Entries::default();
                    for a in f {
                        let (k, hc) = a.split_once('=')?;
                        let (h, c) = hc.split_once('/')?;
                        e.at.push((num(k)?, list(h)?, list(c)?));
                    }
                    m.songs.push((song, e));
                }
                _ => {}
            }
        }
        let n = m.scripts.len() as u32;
        let fine = m.songs.iter().all(|s| s.1.at.iter().all(|e| e.1.iter().chain(&e.2).all(|&i| i < n)));
        (m.data_len > 0 && fine).then_some(m)
    }
}

/// A script in a line, for traces.
fn describe(s: &Script) -> String {
    let outs: Vec<String> = s.ops.iter().filter_map(|o| if let Op::Out(p, v) = o { Some(format!("{p:02X}={v:02X}")) } else { None }).take(12).collect();
    let pokes = s.ops.iter().filter(|o| matches!(o, Op::Poke(..))).count();
    format!(
        "{} cycles, {pokes} pokes, outs [{}], to {:04X}:{:04X}{}",
        s.cycles,
        outs.join(" "),
        s.end.seg[1],
        s.end.ip,
        if s.halt { " halted" } else { "" }
    )
}

/// For diagnosis: song `song` as the file plays it and as the map plays it (gaps bridged from
/// the ROM, the patches over them), the first frame their sound writes differ, and both runs' events up to it.
pub fn trace(wsr: &[u8], m: &Map, rom: &[u8], song: u8, frames: usize) -> Vec<String> {
    let own = own_bytes(m.data_len, &m.runs, Fill::Bridged);
    let mut rec = Vec::new();
    let (want, _) = record(wsr, &own, song, frames, &mut Vec::new(), &mut HashMap::new(), Some(&mut rec), None);
    let mut rep = Vec::new();
    let got = replay(m, rom, song, frames, Fill::Bridged, Some(&mut rep)).unwrap_or_default();
    let key = |f: &Vec<Write>| f.iter().map(|w| (w.port, w.value)).collect::<Vec<_>>();
    let first = (0..want.len()).find(|&i| got.get(i).map(key) != Some(key(&want[i])));
    let mut out = vec![format!("song {song}: {} frames from the file, {} from the map; first differing frame {first:?}", want.len(), got.len())];
    if let Some(f) = first {
        let show = |ws: Option<&Vec<Write>>| ws.map(|ws| ws.iter().take(16).map(|w| format!("{:X}={:02X}@{}", w.port, w.value, w.at)).collect::<Vec<_>>().join(" ")).unwrap_or_default();
        out.push(format!("  file: {}", show(want.get(f))));
        out.push(format!("  map:  {}", show(got.get(f))));
    }
    let upto = first.unwrap_or(2) + 1;
    let before = |e: &String| e[1..].split(' ').next().and_then(|n| n.parse::<usize>().ok()).is_some_and(|n| n <= upto);
    out.push("  file's events:".into());
    out.extend(rec.iter().filter(|e| before(e)).rev().take(40).collect::<Vec<_>>().into_iter().rev().map(|e| format!("    {e}")));
    out.push("  map's events:".into());
    out.extend(rep.iter().filter(|e| before(e)).rev().take(40).collect::<Vec<_>>().into_iter().rev().map(|e| format!("    {e}")));
    out
}

/// How alike each kept song plays from the ROM to the file's own, over `frames` frames.
pub fn map_check(wsr: &[u8], m: &Map, rom: &[u8], frames: usize, fill: Fill) -> Vec<(u8, f64)> {
    m.songs
        .iter()
        .map(|&(song, _)| {
            let want = melody(&track_writes(wsr, song, frames));
            let got = map_track_writes(m, rom, song, frames, fill).map(|f| melody(&f)).unwrap_or_default();
            (song, music_search::alike(&want, &got))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sequence_is_a_head_and_a_repeating_cycle() {
        assert_eq!(head_and_cycle(&[5, 1, 2, 1, 2, 1, 2]), (vec![5], vec![1, 2]));
        assert_eq!(head_and_cycle(&[3, 3, 3]), (vec![], vec![3]));
        assert_eq!(head_and_cycle(&[7]), (vec![], vec![7]));
    }

    /// The tiny game's WSR: its code as it is, but booted by a stub of the file's own that
    /// takes the song from AX instead of starting song 1.
    fn tiny_wsr() -> Vec<u8> {
        let mut wsr = super::super::wswan_music::tests::tiny_game();
        // F000:E000: CLI; SS = DS = ES = 0; SP = 2000; vector 26h to the game's handler at
        // F000:0100; [0200] = AL; interrupt base 20h, vertical blank on; STI; HLT; JMP $-3
        let stub = [
            0xFA, 0x31, 0xC9, 0x8E, 0xD1, 0xBC, 0x00, 0x20, 0x8E, 0xD9, 0x8E, 0xC1, 0xC7, 0x06, 0x98, 0x00, 0x00, 0x01, 0xC7, 0x06,
            0x9A, 0x00, 0x00, 0xF0, 0xA2, 0x00, 0x02, 0xB0, 0x20, 0xE6, 0xB0, 0xB0, 0x40, 0xE6, 0xB2, 0xFB, 0xF4, 0xEB, 0xFD,
        ];
        wsr[0xE000..0xE000 + stub.len()].copy_from_slice(&stub);
        let top = wsr.len() - 32;
        wsr[top..].fill(0);
        wsr[top..top + 4].copy_from_slice(b"WSRF");
        wsr[top + 5] = 1;
        wsr[0xFFF0..0xFFF5].copy_from_slice(&[0xEA, 0x00, 0xE0, 0x00, 0xF0]);
        wsr
    }

    #[test]
    fn the_map_plays_the_songs_from_the_rom_without_the_stub() {
        let rom = super::super::wswan_music::tests::tiny_game();
        let wsr = tiny_wsr();
        let m = map(&wsr, &rom, 0..4).unwrap();
        // Song 0 starts nothing.
        assert_eq!(m.songs.iter().map(|s| s.0).collect::<Vec<_>>(), vec![1, 2, 3]);
        for (song, alike) in map_check(&wsr, &m, &rom, 300, Fill::Zeros) {
            assert!(alike >= 0.99, "song {song}: {alike}");
        }
        // It plays on past what was recorded.
        assert_eq!(map_track_writes(&m, &rom, 1, RECORD_FRAMES * 3, Fill::Zeros).unwrap().len(), RECORD_FRAMES * 3);
        // Written down and read back, it plays the same.
        let back = Map::decode(&m.encode()).unwrap();
        assert_eq!(back.encode(), m.encode());
        assert_eq!(map_track_writes(&back, &rom, 2, 300, Fill::Zeros).unwrap(), map_track_writes(&m, &rom, 2, 300, Fill::Zeros).unwrap());
        assert_eq!(build_map(&wsr, &rom, 0..4).unwrap().songs.len(), 3);
        // Nothing of the stub is kept but numbers: its writes and registers.
        assert!(m.scripts.iter().all(|s| s.ops.iter().all(|o| matches!(o, Op::Poke(..) | Op::Out(..)))));
    }
}
