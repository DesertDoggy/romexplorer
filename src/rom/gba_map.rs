//! Game Boy Advance music maps: what a GBA ROM needs laid over it to play each of its songs,
//! found from a matched GSF set and kept as numbers only.
//!
//! A GSF track is the game's own cartridge image, cut down and patched so that it boots
//! straight into one song: a library file holds the image (bytes the song never reads left
//! as zeros), and each track file adds the few bytes that pick its song. So the ROM is most
//! of the answer already. A map keeps where the set's image differs from the ROM inside the
//! areas the set covers: the boot patches and song numbers as bytes, the cut-down areas as
//! fill runs. Played, the ROM with those bytes laid over it is the set's own image, so it
//! plays what the set plays, on the same mGBA core.
//!
//! Bytes every track shares are kept once; each track keeps its own on top, with its length
//! and fade from the set's tags.

use super::psf::{self, GbaImage, Loaded};

/// A run of bytes laid over the ROM at a cartridge offset.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Run {
    Bytes(u32, Vec<u8>),
    /// That many of one byte (a cut-down area's filler).
    Fill(u32, u32, u8),
    /// That many of the ROM's own bytes from another offset: data a set made from another
    /// region or revision has elsewhere. Laid before the other runs.
    Copy(u32, u32, u32),
    /// A copy from another offset with every cartridge pointer in it (an aligned word from
    /// 0x08000000 to 0x09FFFFFF) moved by the given amount: code and tables another revision
    /// has at other addresses. Laid after the plain copies.
    Reloc(u32, u32, u32, u32),
}

impl Run {
    fn at(&self) -> u32 {
        match self {
            Run::Bytes(a, _) | Run::Fill(a, _, _) | Run::Copy(a, _, _) | Run::Reloc(a, _, _, _) => *a,
        }
    }

    fn end(&self) -> usize {
        match self {
            Run::Bytes(a, b) => *a as usize + b.len(),
            Run::Fill(a, n, _) | Run::Copy(a, n, _) | Run::Reloc(a, n, _, _) => *a as usize + *n as usize,
        }
    }

    /// Lays the run over `image`; a copy takes its bytes from `rom` as it is.
    pub(crate) fn apply(&self, image: &mut Vec<u8>, rom: &[u8]) {
        if image.len() < self.end() {
            image.resize(self.end(), 0);
        }
        let end = self.end();
        match self {
            Run::Bytes(a, b) => image[*a as usize..end].copy_from_slice(b),
            Run::Fill(a, _, b) => image[*a as usize..end].fill(*b),
            Run::Copy(a, n, from) => {
                let (from, n) = (*from as usize, *n as usize);
                if let Some(src) = rom.get(from..from + n) {
                    image[*a as usize..end].copy_from_slice(src);
                }
            }
            Run::Reloc(a, n, from, k) => {
                let (a, from, n) = (*a as usize, *from as usize, *n as usize);
                if let Some(src) = rom.get(from..from + n) {
                    image[a..end].copy_from_slice(src);
                    relocate_words(&mut image[a..end], a, *k);
                }
            }
        }
    }

    /// When the run is laid: plain copies, then relocated copies, then the rest.
    pub(crate) fn phase(&self) -> u8 {
        match self {
            Run::Copy(..) => 0,
            Run::Reloc(..) => 1,
            _ => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub runs: Vec<Run>,
    pub length_ms: u32,
    pub fade_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Map {
    pub rom_len: u32,
    pub common: Vec<Run>,
    pub tracks: Vec<Track>,
}

/// A set belongs to the ROM when at least this share of its bytes (filler aside) is the ROM's: a
/// set from another revision differs in far more.
pub const SAME_SHARE: f64 = 0.6;
/// Zero bytes shorter than this between differing bytes stay in a byte run.
const ZERO_RUN: usize = 16;
/// A track with no length tag plays this long, and fades this long.
const DEFAULT_LENGTH_MS: u32 = 150_000;
const DEFAULT_FADE_MS: u32 = 8_000;

/// The set's cartridge image of one track, laid out as the player would load it.
fn image_of(sections: &[(usize, &[u8])]) -> Vec<u8> {
    let mut image = Vec::new();
    for &(at, data) in sections {
        if image.len() < at + data.len() {
            image.resize(at + data.len(), 0);
        }
        image[at..at + data.len()].copy_from_slice(data);
    }
    image
}

/// The set's own cartridge image of a loaded track, as its player loads it.
pub fn set_image(loaded: &Loaded) -> Vec<u8> {
    image_of(&loaded.gba_sections())
}

/// The cartridge areas the sections cover, merged, in order.
fn covered(sections: &[(usize, &[u8])]) -> Vec<(usize, usize)> {
    let mut spans: Vec<(usize, usize)> = sections.iter().map(|&(a, d)| (a, a + d.len())).collect();
    spans.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (a, b) in spans {
        match out.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// The share of the image's bytes inside `spans` that the ROM has the same, leaving out
/// filler (one byte repeated at least [`ZERO_RUN`] times: what a cut-down area holds).
pub fn same_share(image: &[u8], spans: &[(usize, usize)], rom: &[u8]) -> f64 {
    let (mut same, mut all) = (0usize, 0usize);
    for &(a, b) in spans {
        let b = b.min(image.len());
        let mut i = a;
        while i < b {
            let mut j = i + 1;
            while j < b && image[j] == image[i] {
                j += 1;
            }
            if j - i < ZERO_RUN {
                all += j - i;
                same += (i..j).filter(|&k| rom.get(k) == Some(&image[k])).count();
            }
            i = j;
        }
    }
    if all == 0 {
        0.0
    } else {
        same as f64 / all as f64
    }
}

/// Where `image` differs from `rom` inside `spans`, as runs: long stretches of one byte as
/// fill runs, everything else as bytes.
pub fn diff(image: &[u8], spans: &[(usize, usize)], rom: &[u8]) -> Vec<Run> {
    let mut runs = Vec::new();
    for &(a, b) in spans {
        let b = b.min(image.len());
        let mut i = a;
        while i < b {
            if rom.get(i) == Some(&image[i]) {
                i += 1;
                continue;
            }
            // A stretch of differing bytes, allowing short equal gaps inside it.
            let start = i;
            let mut end = i + 1;
            let mut j = end;
            while j < b && j - end < 4 {
                if rom.get(j) != Some(&image[j]) {
                    end = j + 1;
                }
                j += 1;
            }
            push_split(&mut runs, start, &image[start..end]);
            i = end;
        }
    }
    runs
}

/// Splits a stretch into byte runs and fill runs (one byte repeated at least [`ZERO_RUN`]
/// times).
fn push_split(runs: &mut Vec<Run>, at: usize, bytes: &[u8]) {
    let mut i = 0;
    let mut start = 0;
    while i < bytes.len() {
        let z = i;
        let b = bytes[i];
        while i < bytes.len() && bytes[i] == b {
            i += 1;
        }
        if i - z >= ZERO_RUN {
            if z > start {
                runs.push(Run::Bytes((at + start) as u32, bytes[start..z].to_vec()));
            }
            runs.push(Run::Fill((at + z) as u32, (i - z) as u32, b));
            start = i;
        }
    }
    if bytes.len() > start {
        runs.push(Run::Bytes((at + start) as u32, bytes[start..].to_vec()));
    }
}

/// The length of the pieces [`moves`] looks for, and how far apart it takes them.
const PIECE: usize = 32;
const PIECE_STEP: usize = 32;
/// Above this share of the image already in place, no moved pieces are looked for.
const IN_PLACE: f64 = 0.98;

/// Whether a piece is mostly one byte: it would be found anywhere.
fn is_filler(piece: &[u8]) -> bool {
    let mut counts = [0u16; 256];
    piece.iter().for_each(|&b| counts[b as usize] += 1);
    counts.iter().any(|&n| n as usize * 4 >= piece.len() * 3)
}

/// Stretches of `image` (inside `spans`) the ROM holds at another offset: (image offset,
/// length, ROM offset). Pieces of the image are hashed every [`PIECE_STEP`] bytes, the ROM is
/// scanned once with a rolling hash, and each hit is grown both ways as far as the bytes agree.
pub fn moves(image: &[u8], spans: &[(usize, usize)], rom: &[u8]) -> Vec<(usize, usize, usize)> {
    use std::collections::HashMap;
    const B: u64 = 0x100_0000_01B3;
    let hash = |p: &[u8]| p.iter().fold(0u64, |h, &b| h.wrapping_mul(B).wrapping_add(u64::from(b) + 1));
    let mut pieces: HashMap<u64, Vec<usize>> = HashMap::new();
    for &(a, b) in spans {
        let b = b.min(image.len());
        let mut p = a;
        while p + PIECE <= b {
            let piece = &image[p..p + PIECE];
            if !is_filler(piece) && rom.get(p..p + PIECE) != Some(piece) {
                let v = pieces.entry(hash(piece)).or_default();
                if v.len() < 4 {
                    v.push(p);
                }
            }
            p += PIECE_STEP;
        }
    }
    if pieces.is_empty() || rom.len() < PIECE {
        return Vec::new();
    }
    let top = (1..PIECE).fold(1u64, |t, _| t.wrapping_mul(B));
    let mut anchors: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut h = hash(&rom[..PIECE]);
    for j in 0..=rom.len() - PIECE {
        if j > 0 {
            h = h.wrapping_sub((u64::from(rom[j - 1]) + 1).wrapping_mul(top)).wrapping_mul(B).wrapping_add(u64::from(rom[j + PIECE - 1]) + 1);
        }
        if let Some(ps) = pieces.get(&h) {
            for &p in ps {
                if image[p..p + PIECE] == rom[j..j + PIECE] {
                    let v = anchors.entry(p).or_default();
                    if v.len() < 8 {
                        v.push(j);
                    }
                }
            }
        }
    }
    let mut keys: Vec<usize> = anchors.keys().copied().collect();
    keys.sort_unstable();
    let mut out = Vec::new();
    let mut done = 0usize;
    for p in keys {
        if p < done {
            continue;
        }
        let Some(&(lo, hi)) = spans.iter().find(|(a, b)| *a <= p && p < *b) else { continue };
        let hi = hi.min(image.len());
        let lo = lo.max(done);
        let best = anchors[&p]
            .iter()
            .map(|&j| {
                let mut s = p;
                while s > lo && j >= p - s + 1 && image[s - 1] == rom[j - (p - s) - 1] {
                    s -= 1;
                }
                let mut e = p + PIECE;
                while e < hi && j + (e - p) < rom.len() && image[e] == rom[j + (e - p)] {
                    e += 1;
                }
                (e - s, s, j - (p - s))
            })
            .max();
        if let Some((len, s, r)) = best {
            if r != s {
                out.push((s, len, r));
            }
            done = s + len;
        }
    }
    out
}

/// Works out one ROM's runs for each track of a set, finding the set's moved pieces once (on
/// the first track: they live in the library every track shares).
#[derive(Default)]
pub struct Placer {
    /// The moved pieces, and the ROM with them laid in.
    placed: Option<(Vec<(usize, usize, usize)>, Vec<u8>)>,
}

impl Placer {
    /// One track's runs from its loaded file, or `None` when the set is not this ROM's.
    pub fn track_runs(&mut self, loaded: &Loaded, rom: &[u8]) -> Option<Vec<Run>> {
        let sections = loaded.gba_sections();
        if sections.is_empty() {
            return None;
        }
        let image = image_of(&sections);
        let spans = covered(&sections);
        let (moved, source) = self.placed.get_or_insert_with(|| {
            // A set's image can run past the ROM's end (a demo cut from a bigger build).
            let mut source = rom.to_vec();
            source.resize(rom.len().max(image.len()), 0);
            if same_share(&image, &spans, rom) >= IN_PLACE {
                return (Vec::new(), source);
            }
            let moved = moves(&image, &spans, rom);
            for &(s, len, r) in &moved {
                source[s..s + len].copy_from_slice(&rom[r..r + len]);
            }
            (moved, source)
        });
        if same_share(&image, &spans, source) < SAME_SHARE {
            return None;
        }
        let mut runs: Vec<Run> = moved.iter().map(|&(s, len, r)| Run::Copy(s as u32, len as u32, r as u32)).collect();
        for run in diff(&image, &spans, source) {
            runs.extend(relocated(run, &image, rom, moved));
        }
        Some(runs)
    }
}

/// Whether a word is an address in the cartridge.
fn is_pointer(w: u32) -> bool {
    (0x0800_0000..0x0A00_0000).contains(&w)
}

/// Moves every cartridge pointer among the aligned words of `bytes` (laid at cartridge offset
/// `at`) by `k`.
fn relocate_words(bytes: &mut [u8], at: usize, k: u32) {
    let first = (4 - at % 4) % 4;
    let mut i = first;
    while i + 4 <= bytes.len() {
        let w = u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
        if is_pointer(w) {
            bytes[i..i + 4].copy_from_slice(&w.wrapping_add(k).to_le_bytes());
        }
        i += 4;
    }
}

/// A byte run as a relocated copy of the ROM where that covers most of it, with what it
/// leaves as bytes; the run as it is otherwise. Tried at this offset and at the offsets of
/// the moved pieces nearby, with the pointer shift most of its differing pointers agree on.
fn relocated(run: Run, image: &[u8], rom: &[u8], moved: &[(usize, usize, usize)]) -> Vec<Run> {
    const NEAR: usize = 1 << 16;
    let Run::Bytes(at, bytes) = &run else { return vec![run] };
    let (at, len) = (*at as usize, bytes.len());
    if len < 8 {
        return vec![run];
    }
    let mut deltas: Vec<isize> = vec![0];
    for &(s, n, r) in moved {
        if s + n + NEAR >= at && s <= at + len + NEAR {
            let d = r as isize - s as isize;
            if !deltas.contains(&d) {
                deltas.push(d);
            }
        }
    }
    let mut best: Option<(usize, usize, u32, Vec<u8>)> = None;
    for d in deltas.into_iter().take(16) {
        let Some(from) = at.checked_add_signed(d).filter(|f| f + len <= rom.len()) else { continue };
        let src = &rom[from..from + len];
        let mut ks: std::collections::HashMap<u32, usize> = Default::default();
        let mut i = (4 - at % 4) % 4;
        while i + 4 <= len {
            let rw = u32::from_le_bytes(src[i..i + 4].try_into().unwrap());
            let iw = u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
            if rw != iw && is_pointer(rw) && is_pointer(iw) {
                *ks.entry(iw.wrapping_sub(rw)).or_default() += 1;
            }
            i += 4;
        }
        let k = ks.into_iter().max_by_key(|&(k, n)| (n, std::cmp::Reverse(k))).map_or(0, |(k, _)| k);
        let mut laid = src.to_vec();
        relocate_words(&mut laid, at, k);
        let same = laid.iter().zip(bytes).filter(|(a, b)| a == b).count();
        if best.as_ref().is_none_or(|b| same > b.0) {
            best = Some((same, from, k, laid));
        }
    }
    let Some((same, from, k, laid)) = best else { return vec![run] };
    if same * 4 < len * 3 {
        return vec![run];
    }
    let mut out = vec![Run::Reloc(at as u32, len as u32, from as u32, k)];
    // What the copy leaves different stays as bytes.
    let spans = [(at, at + len)];
    let mut padded = vec![0u8; at];
    padded.extend_from_slice(&laid);
    out.extend(diff(image, &spans, &padded));
    out
}

/// One track's runs on its own (see [`Placer`] for a whole set).
pub fn track_runs(loaded: &Loaded, rom: &[u8]) -> Option<Vec<Run>> {
    Placer::default().track_runs(loaded, rom)
}

impl Map {
    /// The map from each track's runs (in the set's order) and its tags.
    pub fn from_tracks(rom_len: usize, tracks: Vec<(Vec<Run>, Option<u32>, Option<u32>)>) -> Option<Map> {
        use std::collections::HashSet;
        let first = tracks.first()?.0.clone();
        // Sets, not lists: a game's tracks can hold tens of thousands of runs each.
        let others: Vec<HashSet<&Run>> = tracks[1..].iter().map(|(t, _, _)| t.iter().collect()).collect();
        let common: Vec<Run> = first.into_iter().filter(|r| others.iter().all(|t| t.contains(r))).collect();
        drop(others);
        let shared: HashSet<Run> = common.iter().cloned().collect();
        let tracks = tracks
            .into_iter()
            .map(|(runs, length, fade)| Track {
                runs: runs.into_iter().filter(|r| !shared.contains(r)).collect(),
                length_ms: length.unwrap_or(DEFAULT_LENGTH_MS),
                fade_ms: fade.unwrap_or(if length.is_some() { 0 } else { DEFAULT_FADE_MS }),
            })
            .collect();
        Some(Map { rom_len: rom_len as u32, common, tracks })
    }

    /// Track `t`'s cartridge image: the ROM with the map's bytes laid over it.
    pub fn image(&self, rom: &[u8], t: usize) -> Result<Vec<u8>, String> {
        let track = self.tracks.get(t).ok_or("no such song in this game's music map")?;
        if rom.len() != self.rom_len as usize {
            return Err("this ROM is not the one the music map was made for".into());
        }
        let mut image = rom.to_vec();
        // Copies first: the other runs patch what they bring.
        for phase in 0..3 {
            for r in self.common.iter().chain(&track.runs).filter(|r| r.phase() == phase) {
                r.apply(&mut image, rom);
            }
        }
        Ok(image)
    }

    /// `gbamap 1`, `l <ROM length>`, `c <runs>` (shared), then `t <length ms> <fade ms> <runs>`
    /// per track. A run is `<offset>:<hex bytes>`, `<offset>:z<n>` (zeros), `<offset>:r<byte>.<n>`
    /// (a byte repeated) or `<offset>:m<from>.<n>` (the ROM's bytes from another offset), all
    /// numbers in hex.
    pub fn encode(&self) -> String {
        let mut out = format!("gbamap 1\nl {}\nc", self.rom_len);
        encode_runs(&mut out, &self.common);
        out.push('\n');
        for t in &self.tracks {
            out.push_str(&format!("t {} {}", t.length_ms, t.fade_ms));
            encode_runs(&mut out, &t.runs);
            out.push('\n');
        }
        out
    }

    pub fn decode(text: &str) -> Option<Map> {
        let mut lines = text.lines();
        if lines.next()? != "gbamap 1" {
            return None;
        }
        let rom_len = lines.next()?.strip_prefix("l ")?.parse().ok()?;
        let mut common_words = lines.next()?.split_whitespace();
        if common_words.next()? != "c" {
            return None;
        }
        let common = decode_runs(common_words)?;
        let mut tracks = Vec::new();
        for line in lines.filter(|l| !l.is_empty()) {
            let mut w = line.split_whitespace();
            if w.next()? != "t" {
                return None;
            }
            let length_ms = w.next()?.parse().ok()?;
            let fade_ms = w.next()?.parse().ok()?;
            tracks.push(Track { runs: decode_runs(w)?, length_ms, fade_ms });
        }
        Some(Map { rom_len, common, tracks })
    }

    /// How many bytes the map lays over the ROM that are not filler.
    pub fn file_bytes(&self) -> usize {
        let n = |runs: &[Run]| runs.iter().map(|r| if let Run::Bytes(_, b) = r { b.len() } else { 0 }).sum::<usize>();
        n(&self.common) + self.tracks.iter().map(|t| n(&t.runs)).sum::<usize>()
    }
}

/// Runs as map text: each ` <offset>:...` (see [`Map::encode`]).
pub(crate) fn encode_runs(out: &mut String, runs: &[Run]) {
    for r in runs {
        match r {
            Run::Bytes(a, b) => {
                out.push_str(&format!(" {a:x}:"));
                for x in b {
                    out.push_str(&format!("{x:02x}"));
                }
            }
            Run::Fill(a, n, 0) => out.push_str(&format!(" {a:x}:z{n:x}")),
            Run::Fill(a, n, b) => out.push_str(&format!(" {a:x}:r{b:02x}.{n:x}")),
            Run::Copy(a, n, from) => out.push_str(&format!(" {a:x}:m{from:x}.{n:x}")),
            Run::Reloc(a, n, from, k) => out.push_str(&format!(" {a:x}:p{from:x}.{n:x}.{k:x}")),
        }
    }
}

/// Runs read back from the words [`encode_runs`] writes.
/// Where a run may end at most: past every image a map lays (a GBA program, an SNES sound
/// memory, an N64 save state with its ROM parts). A damaged map file reaching further is
/// refused rather than growing an image without bound.
const RUN_END_MAX: u64 = 0x0800_0000;

pub(crate) fn decode_runs<'a>(words: impl Iterator<Item = &'a str>) -> Option<Vec<Run>> {
    words
        .map(|w| {
            let run = decode_run(w)?;
            let (a, n) = match &run {
                Run::Bytes(a, b) => (*a as u64, b.len() as u64),
                Run::Fill(a, n, _) | Run::Copy(a, n, _) | Run::Reloc(a, n, _, _) => (*a as u64, *n as u64),
            };
            (a + n <= RUN_END_MAX).then_some(run)
        })
        .collect()
}

fn decode_run(w: &str) -> Option<Run> {
    if !w.is_ascii() {
        return None;
    }
    let (a, v) = w.split_once(':')?;
    let a = u32::from_str_radix(a, 16).ok()?;
    if let Some(n) = v.strip_prefix('z') {
        return Some(Run::Fill(a, u32::from_str_radix(n, 16).ok()?, 0));
    }
    if let Some(c) = v.strip_prefix('p') {
        let mut f = c.split('.');
        let (from, n, k) = (f.next()?, f.next()?, f.next()?);
        return Some(Run::Reloc(a, u32::from_str_radix(n, 16).ok()?, u32::from_str_radix(from, 16).ok()?, u32::from_str_radix(k, 16).ok()?));
    }
    if let Some(c) = v.strip_prefix('m') {
        let (from, n) = c.split_once('.')?;
        return Some(Run::Copy(a, u32::from_str_radix(n, 16).ok()?, u32::from_str_radix(from, 16).ok()?));
    }
    if let Some(f) = v.strip_prefix('r') {
        let (b, n) = f.split_once('.')?;
        return Some(Run::Fill(a, u32::from_str_radix(n, 16).ok()?, u8::from_str_radix(b, 16).ok()?));
    }
    if v.len() % 2 != 0 {
        return None;
    }
    let bytes = (0..v.len()).step_by(2).map(|i| u8::from_str_radix(&v[i..i + 2], 16).ok()).collect::<Option<Vec<u8>>>()?;
    Some(Run::Bytes(a, bytes))
}

/// How long each track is played while [`slim`] tries leaving runs out.
const SLIM_SECONDS: u32 = 8;
/// How long each track is played to check what [`slim`] left out: songs that only reach the
/// left-out bytes after the first seconds lose them otherwise.
const SLIM_CHECK_SECONDS: u32 = 15;
/// [`slim`] stops halving a group below this share of the runs it started with, and keeps
/// the group: it bounds the plays a game costs.
const SLIM_FINEST: usize = 128;

/// Leaves out the shared byte and fill runs the music does not need: a set's image often
/// differs from the ROM in code and data its songs never read, and fills areas the ROM keeps
/// its own data in. First all of them are left out at once, which is often enough; otherwise
/// they are left out in groups in ROM order, a group whose loss changes any track's sound
/// halved down to 1/[`SLIM_FINEST`] of the runs. Each track is then played longer: for those
/// that changed, the left-out runs are tried again the same way, on those tracks alone, and
/// what they need goes back in. Copy runs always stay. Returns how many of the file's bytes
/// it left out.
pub fn slim(map: &mut Map, rom: &[u8]) -> Result<usize, String> {
    if map.rom_len as usize != rom.len() {
        return Err("this ROM is not the one the music map was made for".into());
    }
    let play = |common: &[Run], t: usize, seconds: u32| -> Option<Vec<i16>> {
        let mut m = Map { rom_len: map.rom_len, common: common.to_vec(), tracks: Vec::new() };
        m.tracks.push(map.tracks[t].clone());
        render(&m.image(rom, 0).ok()?, seconds).ok()
    };
    let dropped = slim_common(&map.common, map.tracks.len(), &play);
    let before = map.file_bytes();
    map.common = map.common.iter().enumerate().filter(|(i, _)| !dropped.contains(i)).map(|(_, r)| r.clone()).collect();
    Ok(before - map.file_bytes())
}

/// Which of the shared byte and fill runs `common` the music does not need, `play(runs, track,
/// seconds)` rendering a track with only `runs` shared (None for a track without sound): see
/// [`slim`]. Any music map built of [`Run`]s slims this way.
pub(crate) fn slim_common(common: &[Run], tracks: usize, play: &dyn Fn(&[Run], usize, u32) -> Option<Vec<i16>>) -> std::collections::HashSet<usize> {
    use std::collections::HashSet;
    let candidates: Vec<usize> = common.iter().enumerate().filter(|(_, r)| matches!(r, Run::Bytes(..) | Run::Fill(..))).map(|(i, _)| i).collect();
    if candidates.is_empty() {
        return HashSet::new();
    }
    let render_all = |seconds| -> Vec<Option<Vec<i16>>> { (0..tracks).map(|t| play(common, t, seconds)).collect() };
    let wanted = render_all(SLIM_SECONDS);
    let order: Vec<usize> = (0..tracks).filter(|&t| wanted[t].is_some()).collect();
    let mut dropped = leave_out(common, &candidates, order, &wanted, SLIM_SECONDS, play);
    if !dropped.is_empty() {
        let longer = render_all(SLIM_CHECK_SECONDS);
        let trial = without(common, &dropped);
        let changed: Vec<usize> = (0..tracks)
            .filter(|&t| longer[t].as_ref().is_some_and(|want| !play(&trial, t, SLIM_CHECK_SECONDS).is_some_and(|got| alike(want, &got) >= 0.999)))
            .collect();
        if !changed.is_empty() {
            // Only what these tracks can do without stays out; the rest goes back in.
            let mut again: Vec<usize> = dropped.iter().copied().collect();
            again.sort_unstable();
            dropped = leave_out(common, &again, changed, &longer, SLIM_CHECK_SECONDS, play);
        }
    }
    dropped
}

/// `common` with the runs `dropped` left out.
fn without(common: &[Run], dropped: &std::collections::HashSet<usize>) -> Vec<Run> {
    common.iter().enumerate().filter(|(i, _)| !dropped.contains(i)).map(|(_, r)| r.clone()).collect()
}

/// Which of the common runs `candidates` can be left out with `order`'s tracks played as
/// `wanted` over `seconds`: all at once, else in halving groups.
fn leave_out(
    common: &[Run],
    candidates: &[usize],
    mut order: Vec<usize>,
    wanted: &[Option<Vec<i16>>],
    seconds: u32,
    play: &dyn Fn(&[Run], usize, u32) -> Option<Vec<i16>>,
) -> std::collections::HashSet<usize> {
    let mut dropped = std::collections::HashSet::new();
    let finest = (candidates.len() / SLIM_FINEST).max(1);
    // The track that last told a trial apart is played first: a failing trial ends sooner.
    let same_without = |dropped: &std::collections::HashSet<usize>, more: &[usize], order: &mut Vec<usize>| -> bool {
        let mut out = dropped.clone();
        out.extend(more.iter().copied());
        let trial = without(common, &out);
        for k in 0..order.len() {
            let t = order[k];
            let want = wanted[t].as_ref().expect("only tracks with sound are played");
            if !play(&trial, t, seconds).is_some_and(|got| alike(want, &got) >= 0.999) {
                order.remove(k);
                order.insert(0, t);
                return false;
            }
        }
        true
    };
    let mut groups = vec![candidates.to_vec()];
    while let Some(group) = groups.pop() {
        if same_without(&dropped, &group, &mut order) {
            dropped.extend(group);
        } else if group.len() > finest {
            let (a, b) = group.split_at(group.len() / 2);
            groups.push(b.to_vec());
            groups.push(a.to_vec());
        }
    }
    dropped
}

/// Renders `seconds` of a cartridge image (stereo at [`GbaImage::RATE`]).
pub fn render(image: &[u8], seconds: u32) -> Result<Vec<i16>, String> {
    let mut player = GbaImage::open(image)?;
    let mut out = vec![0i16; GbaImage::RATE as usize * 2 * seconds as usize];
    player.render(&mut out)?;
    Ok(out)
}

/// The share of samples two renders have the same.
pub fn alike(a: &[i16], b: &[i16]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    a.iter().zip(b).filter(|(x, y)| x == y).count() as f64 / n as f64
}

/// The map of `rom` from a set's loaded tracks (in the set's order), or `None` when the set is
/// not this ROM's.
pub fn build_map<'a>(rom: &[u8], tracks: impl IntoIterator<Item = &'a Loaded>) -> Option<Map> {
    let mut found = Vec::new();
    let mut placer = Placer::default();
    for loaded in tracks {
        let info = loaded.info();
        let runs = placer.track_runs(loaded, rom)?;
        found.push((runs, info.length_ms, info.fade_ms));
    }
    Map::from_tracks(rom.len(), found)
}

/// Whether the map can be played here.
pub fn available() -> bool {
    psf::gba_available()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_damaged_run_is_refused_not_laid() {
        assert!(decode_runs(["10:z20", "40:abcd"].into_iter()).is_some());
        // Past every image a map lays, an overflowing end, half a byte, a non-ASCII word.
        for bad in ["7fffff0:z20", "ffffffff:zffffffff", "40:abc", "40:aé"] {
            assert!(decode_runs([bad].into_iter()).is_none(), "{bad}");
        }
    }

    #[test]
    fn diff_keeps_patches_as_bytes_and_cut_areas_as_zeros() {
        let rom: Vec<u8> = (0..200u32).map(|i| (i % 251 + 1) as u8).collect();
        let mut image = rom.clone();
        image[10] = 0xAA;
        image[11] = 0xBB;
        image[50..90].fill(0);
        image[120] = 0;
        let runs = diff(&image, &[(0, 200)], &rom);
        assert_eq!(runs, vec![Run::Bytes(10, vec![0xAA, 0xBB]), Run::Fill(50, 40, 0), Run::Bytes(120, vec![0])]);
        let mut back = rom.clone();
        runs.iter().for_each(|r| r.apply(&mut back, &rom));
        assert_eq!(back, image);
    }

    #[test]
    fn any_repeated_filler_is_a_fill_run() {
        let rom: Vec<u8> = (0..100u32).map(|i| (i % 7 + 1) as u8).collect();
        let mut image = rom.clone();
        image[20..60].fill(0xFF);
        let runs = diff(&image, &[(0, 100)], &rom);
        assert_eq!(runs, vec![Run::Fill(20, 40, 0xFF)]);
        let m = Map { rom_len: 100, common: runs, tracks: vec![Track { runs: vec![], length_ms: 1, fade_ms: 0 }] };
        assert_eq!(Map::decode(&m.encode()), Some(m.clone()));
        assert_eq!(m.image(&rom, 0).unwrap(), image);
    }

    #[test]
    fn data_another_revision_moved_is_copied_from_where_it_is() {
        let mut x = 99u32;
        let mut noise = |n: usize| -> Vec<u8> {
            (0..n)
                .map(|_| {
                    x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    (x >> 16) as u8
                })
                .collect()
        };
        let a = noise(3000);
        let b = noise(2000);
        // The set's game: a then b. This ROM: 500 new bytes, then a and b moved up.
        let mut image = a.clone();
        image.extend_from_slice(&b);
        image[100] ^= 0xFF;
        let mut rom = noise(500);
        rom.extend_from_slice(&a);
        rom.extend_from_slice(&b);
        let spans = [(0, image.len())];
        let moved = moves(&image, &spans, &rom);
        assert!(moved.iter().map(|m| m.1).sum::<usize>() >= 4900, "{moved:?}");
        let mut source = rom.clone();
        for &(s, len, r) in &moved {
            source[s..s + len].copy_from_slice(&rom[r..r + len]);
        }
        let mut runs: Vec<Run> = moved.iter().map(|&(s, len, r)| Run::Copy(s as u32, len as u32, r as u32)).collect();
        runs.extend(diff(&image, &spans, &source));
        let m = Map { rom_len: rom.len() as u32, common: runs, tracks: vec![Track { runs: vec![], length_ms: 1, fade_ms: 0 }] };
        assert!(m.file_bytes() < 8, "{}", m.file_bytes());
        let back = Map::decode(&m.encode()).unwrap();
        assert_eq!(&back.image(&rom, 0).unwrap()[..image.len()], &image[..]);
    }

    #[test]
    fn code_another_revision_moved_is_a_relocated_copy() {
        // The ROM's code at 0x100, with pointers into the cartridge; the set's image has it at
        // 0x40 with every pointer 0x20 lower.
        let mut rom = vec![0x11u8; 0x400];
        for (n, i) in (0x100..0x200).step_by(8).enumerate() {
            rom[i..i + 4].copy_from_slice(&(0x0800_1000u32 + n as u32 * 4).to_le_bytes());
            rom[i + 4..i + 8].copy_from_slice(&[n as u8, 2, 3, 4]);
        }
        let mut image = rom.clone();
        image[0x40..0x140].copy_from_slice(&rom[0x100..0x200]);
        relocate_words(&mut image[0x40..0x140], 0x40, 0u32.wrapping_sub(0x20));
        // A moved piece nearby gives the offset to try.
        let runs = relocated(Run::Bytes(0x40, image[0x40..0x140].to_vec()), &image, &rom, &[(0x20, 0x10, 0xE0)]);
        assert_eq!(runs, vec![Run::Reloc(0x40, 0x100, 0x100, 0u32.wrapping_sub(0x20))]);
        let m = Map { rom_len: 0x400, common: runs, tracks: vec![Track { runs: vec![], length_ms: 1, fade_ms: 0 }] };
        assert_eq!(Map::decode(&m.encode()).unwrap().image(&rom, 0).unwrap(), image);
    }

    #[test]
    fn diff_ignores_what_the_set_does_not_cover() {
        let rom = vec![7u8; 100];
        let image = vec![0u8; 100];
        assert_eq!(diff(&image, &[(20, 30)], &rom), vec![Run::Bytes(20, vec![0; 10])]);
        assert_eq!(same_share(&image, &[(0, 100)], &rom), 0.0);
    }

    #[test]
    fn shared_runs_are_kept_once_and_the_text_reads_back() {
        let shared = Run::Fill(0x100, 0x40, 0);
        let m = Map::from_tracks(
            0x1000,
            vec![
                (vec![shared.clone(), Run::Bytes(0x20, vec![1])], Some(90_000), Some(10_000)),
                (vec![shared.clone(), Run::Bytes(0x20, vec![2])], None, None),
            ],
        )
        .unwrap();
        assert_eq!(m.common, vec![shared]);
        assert_eq!(m.tracks[0].runs, vec![Run::Bytes(0x20, vec![1])]);
        assert_eq!((m.tracks[1].length_ms, m.tracks[1].fade_ms), (DEFAULT_LENGTH_MS, DEFAULT_FADE_MS));
        assert_eq!(Map::decode(&m.encode()), Some(m.clone()));
        let rom = vec![9u8; 0x1000];
        let image = m.image(&rom, 1).unwrap();
        assert_eq!((image[0x20], image[0x100], image[0x13F], image[0x140]), (2, 0, 0, 9));
        assert!(m.image(&rom[..10], 0).is_err());
    }
}
