//! Matching music rip sets to ROMs by content. A rip is the game's own sound driver and song
//! data cut out of the ROM, so short runs of its bytes turn up in every ROM of that game: each
//! region and revision, under any file name. A set's fingerprint is a few dozen 32-byte runs
//! taken from its rips; a ROM is scanned once against the fingerprints of every set for its
//! system, and a set whose runs are mostly found there belongs to it.
//!
//! Only rips that carry ROM bytes can be matched: NSF, NSFE, GBS, SPC, HES, KSS, SGC and WSR. VGM,
//! GYM, AY and SAP are logs or other machines' memory and are never matched.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The length of one run.
pub const RUN: usize = 32;
/// At most this many runs per set.
const MAX_RUNS: usize = 48;
/// A run needs this many different byte values: padding, blank tables and silence would be
/// found in any ROM.
const MIN_DISTINCT: usize = 12;
/// A set belongs to a ROM when at least this share of its runs is found there.
pub const MATCH_SHARE: f32 = 0.5;
/// ...or this share, when another ROM of the same title holds the set strongly: a PAL version
/// keeps much of its NTSC music but changes tempo tables and moves code.
pub const SIBLING_SHARE: f32 = 0.3;
/// ...and at least this many runs: a set with a few runs proves too little.
const MIN_RUNS: usize = 6;

/// The machine a rip set's music runs on, as far as matching goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum RipSystem {
    Nes,
    Snes,
    GameBoy,
    PcEngine,
    Msx,
    MasterSystem,
    GameGear,
    ColecoVision,
    WonderSwan,
}

impl RipSystem {
    /// The `platforms.id`s whose ROMs this system's sets are matched against.
    pub fn platform_ids(self) -> &'static [u16] {
        use crate::common::models::Platform as P;
        match self {
            Self::Nes => &[P::Nes as u16, P::Famicom as u16, P::FamicomDiskSystem as u16],
            Self::Snes => &[P::Snes as u16, P::SuperFamicom as u16, P::SufamiTurbo as u16],
            Self::GameBoy => &[P::GameBoy as u16, P::GameBoyColor as u16],
            Self::PcEngine => &[P::TurboGrafx16 as u16],
            Self::Msx => &[P::Msx as u16, P::Msx2 as u16, P::Msx2Plus as u16, P::MsxTurbo as u16],
            // SGC sets for the Master System and Game Gear share a driver often enough that
            // each is tried against both.
            Self::MasterSystem | Self::GameGear => &[P::SegaMasterSystem as u16, P::SegaGameGear as u16, P::Sg1000 as u16],
            Self::ColecoVision => &[P::ColecoVision as u16],
            Self::WonderSwan => &[P::WonderSwan as u16, P::WonderSwanColor as u16],
        }
    }
}

/// The ROM bytes a rip file carries, and its system. `None` for formats with none.
pub fn rom_image(data: &[u8]) -> Option<(RipSystem, &[u8])> {
    // A WSR is a cartridge image with its `WSRF` header in the last 32 bytes.
    if data.len() >= 32 && &data[data.len() - 32..data.len() - 28] == b"WSRF" {
        return Some((RipSystem::WonderSwan, &data[..data.len() - 32]));
    }
    let tag = data.get(..4)?;
    match tag {
        b"NESM" => Some((RipSystem::Nes, data.get(0x80..)?)),
        b"NSFE" => Some((RipSystem::Nes, nsfe_data(data)?)),
        [b'G', b'B', b'S', _] => Some((RipSystem::GameBoy, data.get(0x70..)?)),
        b"SNES" => Some((RipSystem::Snes, data.get(0x100..0x10100.min(data.len()))?)),
        b"HESM" => Some((RipSystem::PcEngine, data.get(0x20..)?)),
        // A KSS with the SN76489 flag (device flags bit 1) is Master System / Game Gear music,
        // not MSX. A KSSX header is 0x10 bytes plus its own extra length.
        b"KSCC" | b"KSSX" => {
            let flags = *data.get(0x0F)?;
            let start = if tag == b"KSSX" { 0x10 + usize::from(*data.get(0x0E)?) } else { 0x10 };
            Some((if flags & 0x02 != 0 { RipSystem::MasterSystem } else { RipSystem::Msx }, data.get(start..)?))
        }
        [b'S', b'G', b'C', 0x1A] => {
            let system = match data.get(0x28) {
                Some(1) => RipSystem::GameGear,
                Some(2) => RipSystem::ColecoVision,
                _ => RipSystem::MasterSystem,
            };
            Some((system, data.get(0xA0..)?))
        }
        _ => None,
    }
}

/// An NSFE's DATA chunk.
fn nsfe_data(data: &[u8]) -> Option<&[u8]> {
    let mut pos = 4;
    while pos + 8 <= data.len() {
        let len = u32::from_le_bytes(data[pos..pos + 4].try_into().ok()?) as usize;
        let id = &data[pos + 4..pos + 8];
        let body = data.get(pos + 8..pos + 8 + len)?;
        if id == b"DATA" {
            return Some(body);
        }
        if id == b"NEND" {
            return None;
        }
        pos += 8 + len;
    }
    None
}

fn distinct(run: &[u8]) -> usize {
    let mut seen = [false; 256];
    run.iter().filter(|&&b| !std::mem::replace(&mut seen[b as usize], true)).count()
}

/// A set's fingerprint: its system and its runs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Fingerprint {
    pub system: RipSystem,
    pub runs: Vec<[u8; RUN]>,
}

/// The fingerprint of a set made of `files` (each a whole rip file). `None` when none of
/// them carries ROM bytes, or they disagree on the system.
pub fn fingerprint<'a>(files: impl IntoIterator<Item = &'a [u8]>) -> Option<Fingerprint> {
    let mut system = None;
    let mut candidates: Vec<[u8; RUN]> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for data in files {
        let Some((sys, image)) = rom_image(data) else { continue };
        match system {
            None => system = Some(sys),
            Some(s) if s != sys => continue,
            _ => {}
        }
        for chunk in image.chunks_exact(RUN) {
            let run: [u8; RUN] = chunk.try_into().unwrap();
            if distinct(&run) >= MIN_DISTINCT && seen.insert(run) {
                candidates.push(run);
            }
        }
    }
    let system = system?;
    // Evenly spread over the whole set, so one file's driver does not fill it alone.
    let runs = if candidates.len() <= MAX_RUNS {
        candidates
    } else {
        (0..MAX_RUNS).map(|i| candidates[i * candidates.len() / MAX_RUNS]).collect()
    };
    (!runs.is_empty()).then_some(Fingerprint { system, runs })
}

const BASE: u64 = 0x100_0000_01B3;

fn hash_run(run: &[u8]) -> u64 {
    run.iter().fold(0u64, |h, &b| h.wrapping_mul(BASE).wrapping_add(u64::from(b) + 1))
}

/// Fingerprints of many sets, to scan ROMs against.
pub struct Matcher {
    /// Run hash -> (set, run) for every run of every set.
    runs: HashMap<u64, Vec<(u32, u16)>>,
    sets: Vec<Fingerprint>,
    /// BASE^(RUN-1), to take the oldest byte out of the rolling hash.
    top: u64,
}

impl Matcher {
    pub fn new(sets: Vec<Fingerprint>) -> Self {
        let mut runs: HashMap<u64, Vec<(u32, u16)>> = HashMap::new();
        for (s, fp) in sets.iter().enumerate() {
            for (r, run) in fp.runs.iter().enumerate() {
                runs.entry(hash_run(run)).or_default().push((s as u32, r as u16));
            }
        }
        let top = (1..RUN).fold(1u64, |p, _| p.wrapping_mul(BASE));
        Self { runs, sets, top }
    }

    pub fn is_empty(&self) -> bool {
        self.sets.is_empty()
    }

    /// A scanner for one ROM, fed its bytes in any chunks.
    pub fn scanner(&self) -> Scanner<'_> {
        Scanner { m: self, window: [0; RUN], filled: 0, pos: 0, hash: 0, found: HashMap::new() }
    }
}

/// One ROM being scanned.
pub struct Scanner<'a> {
    m: &'a Matcher,
    /// The last RUN bytes, as a ring at `pos % RUN`.
    window: [u8; RUN],
    filled: usize,
    pos: usize,
    hash: u64,
    /// Set -> which of its runs were found.
    found: HashMap<u32, Vec<bool>>,
}

impl Scanner<'_> {
    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            let slot = self.pos % RUN;
            if self.filled == RUN {
                let old = self.window[slot];
                self.hash = self.hash.wrapping_sub((u64::from(old) + 1).wrapping_mul(self.m.top));
            } else {
                self.filled += 1;
            }
            self.window[slot] = b;
            self.hash = self.hash.wrapping_mul(BASE).wrapping_add(u64::from(b) + 1);
            self.pos += 1;
            if self.filled < RUN {
                continue;
            }
            let Some(hits) = self.m.runs.get(&self.hash) else { continue };
            let start = self.pos % RUN;
            let mut run = [0u8; RUN];
            run[..RUN - start].copy_from_slice(&self.window[start..]);
            run[RUN - start..].copy_from_slice(&self.window[..start]);
            for &(set, r) in hits {
                if self.m.sets[set as usize].runs[r as usize] == run {
                    let n = self.m.sets[set as usize].runs.len();
                    self.found.entry(set).or_insert_with(|| vec![false; n])[r as usize] = true;
                }
            }
        }
    }

    /// The sets that belong to this ROM, by index into the matcher's sets, with the share of
    /// their runs found, best first.
    pub fn finish(self) -> Vec<(usize, f32)> {
        let mut out: Vec<(usize, f32)> = self
            .found
            .into_iter()
            .filter_map(|(set, hits)| {
                let total = hits.len();
                let share = hits.iter().filter(|&&h| h).count() as f32 / total as f32;
                (total >= MIN_RUNS && share >= SIBLING_SHARE).then_some((set as usize, share))
            })
            .collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        out
    }
}

// --- choosing among a ROM's matches -----------------------------------------------------

/// A match this strong is the game's own music. Below it, a set found in a ROM can be another
/// game sharing its sound driver or samples.
pub const SURE_SHARE: f32 = 0.7;
/// A ROM keeps its best set, and the others within this much of it: a compilation holds
/// several games whole, while a shared driver matches well below the game's own set.
const NEAR_BEST: f32 = 0.05;
/// At most this many sets per ROM.
const MAX_SETS_PER_ROM: usize = 8;

/// ROMs that hold other games' sound drivers without being those games: BIOS images, and the
/// Super Game Boy (it carries the SNES driver every SGB-enhanced game's rip shares).
pub fn skip_rom(path: &Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    name.contains("[bios]") || name.contains("(bios)") || name.contains("super game boy") || name.contains("system card")
}

/// Whether a ROM's file name says it is a multi-cart ("36 in 1", "110-in-1").
pub fn multi_cart(path: &Path) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?i)\b\d+\s*-?\s*in\s*-?\s*1\b").expect("pattern"));
    path.file_name().is_some_and(|n| re.is_match(&n.to_string_lossy()))
}

/// Which of a ROM's matches (best first) it keeps. A weak match (under [`SURE_SHARE`]) is
/// dropped when `claimed(set)` says a ROM of another title holds that set strongly: that is the
/// set's own game, and this ROM only shares its driver. A match under [`MATCH_SHARE`] stays only
/// when `sibling(set)` says a ROM of this same title holds it strongly (another region or
/// revision). Then the best set stays, with the others close to it.
pub fn choose(matches: &[(PathBuf, f32)], claimed: impl Fn(&Path) -> bool, sibling: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let kept: Vec<&(PathBuf, f32)> = matches
        .iter()
        .filter(|(set, share)| *share >= MATCH_SHARE || sibling(set))
        .filter(|(set, share)| *share >= SURE_SHARE || !claimed(set))
        .collect();
    let Some(best) = kept.first().map(|m| m.1) else { return Vec::new() };
    kept.into_iter().filter(|(_, share)| *share >= best - NEAR_BEST).take(MAX_SETS_PER_ROM).map(|(set, _)| set.clone()).collect()
}

// --- rip libraries ----------------------------------------------------------------------

/// The largest ROM file scanned whole; bigger is not a cartridge these sets are from.
const MAX_ROM_BYTES: u64 = 64 << 20;

/// One set of a rip library.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CachedSet {
    path: PathBuf,
    size: u64,
    modified: u64,
    fingerprint: Option<Fingerprint>,
}

fn stamp(path: &Path) -> (u64, u64) {
    let Ok(meta) = std::fs::metadata(path) else { return (0, 0) };
    let modified = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
    // A folder's own time changes when a file is added or removed.
    (if meta.is_dir() { 0 } else { meta.len() }, modified)
}

/// The sets of the rip library folders, fingerprinted, with a matcher per system.
pub struct Library {
    sets: Vec<(PathBuf, RipSystem)>,
    /// System -> its matcher, and the index into `sets` of each of the matcher's sets.
    by_system: HashMap<RipSystem, (Matcher, Vec<usize>)>,
}

impl Library {
    /// Reads the sets under `folders`. Fingerprints are kept in `cache` (a JSON file) by path,
    /// size and time, so only new or changed sets are read again.
    pub fn build(folders: &[PathBuf], cache: Option<&Path>, mut progress: impl FnMut(usize, usize)) -> Self {
        let old: HashMap<PathBuf, CachedSet> = cache
            .and_then(|c| std::fs::read(c).ok())
            .and_then(|b| serde_json::from_slice::<Vec<CachedSet>>(&b).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|c| (c.path.clone(), c))
            .collect();
        let paths: Vec<PathBuf> = folders.iter().flat_map(|f| super::rip_sets::sets_in(f)).collect();
        let mut fresh = Vec::with_capacity(paths.len());
        for (i, path) in paths.iter().enumerate() {
            progress(i, paths.len());
            let (size, modified) = stamp(path);
            let entry = match old.get(path) {
                Some(c) if c.size == size && c.modified == modified => c.clone(),
                _ => CachedSet { path: path.clone(), size, modified, fingerprint: super::rip_sets::set_fingerprint(path) },
            };
            fresh.push(entry);
        }
        if let Some(c) = cache {
            if let Ok(json) = serde_json::to_vec(&fresh) {
                let _ = std::fs::write(c, json);
            }
        }
        let mut sets = Vec::new();
        let mut grouped: HashMap<RipSystem, (Vec<Fingerprint>, Vec<usize>)> = HashMap::new();
        for c in fresh {
            let Some(fp) = c.fingerprint else { continue };
            let g = grouped.entry(fp.system).or_default();
            g.1.push(sets.len());
            sets.push((c.path, fp.system));
            g.0.push(fp);
        }
        let by_system = grouped.into_iter().map(|(sys, (fps, idx))| (sys, (Matcher::new(fps), idx))).collect();
        Self { sets, by_system }
    }

    /// How many sets can be matched.
    pub fn len(&self) -> usize {
        self.sets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sets.is_empty()
    }

    fn systems_for(&self, platform: u16) -> Vec<RipSystem> {
        self.by_system.keys().copied().filter(|s| s.platform_ids().contains(&platform)).collect()
    }

    /// The library set a rip track's stored path (`inner_path`) lies in, if any.
    pub fn set_of(&self, track: &str) -> Option<&Path> {
        let file = Path::new(track.split('|').next().unwrap_or(track));
        file.ancestors().find_map(|a| self.sets.iter().find(|(p, _)| p == a).map(|(p, _)| p.as_path()))
    }

    /// Every set in the library, by path.
    pub fn set_paths(&self) -> impl Iterator<Item = &Path> {
        self.sets.iter().map(|(p, _)| p.as_path())
    }

    /// Whether any set could belong to a ROM of `platform` (`platforms.id`).
    pub fn covers(&self, platform: u16) -> bool {
        !self.systems_for(platform).is_empty()
    }

    /// The sets that belong to the ROM at `rom` (a file, or a zip / 7z of one), with the share
    /// of their runs found, best first.
    pub fn match_rom(&self, rom: &Path, platform: u16) -> Result<Vec<(PathBuf, f32)>, String> {
        let systems = self.systems_for(platform);
        let mut scanners: Vec<(RipSystem, Scanner<'_>)> = systems.iter().map(|s| (*s, self.by_system[s].0.scanner())).collect();
        if scanners.is_empty() {
            return Ok(Vec::new());
        }
        let mut feed = |bytes: &[u8]| scanners.iter_mut().for_each(|(_, s)| s.feed(bytes));
        let name = rom.to_string_lossy().to_ascii_lowercase();
        let format = if name.ends_with(".zip") {
            Some(super::sevenzip_wrapper::ArchiveFormat::Zip)
        } else if name.ends_with(".7z") {
            Some(super::sevenzip_wrapper::ArchiveFormat::SevenZ)
        } else {
            None
        };
        match format {
            Some(format) => {
                let archive = super::sevenzip_wrapper::SevenZipArchive::open_variant(rom, format, super::native_lib::LibVariant::Streaming, |_, _| true).map_err(|e| e.to_string())?;
                for i in 0..archive.entry_count() {
                    let Ok(entry) = archive.entry(i) else { continue };
                    if entry.is_dir || entry.size > MAX_ROM_BYTES {
                        continue;
                    }
                    archive.extract_stream(i, None, |_, _, chunk| {
                        feed(chunk);
                        true
                    }, |_, _| true).map_err(|e| e.to_string())?;
                }
            }
            None => {
                use std::io::Read;
                let mut file = std::fs::File::open(rom).map_err(|e| format!("{}: {e}", rom.display()))?;
                if file.metadata().map(|m| m.len()).unwrap_or(0) > MAX_ROM_BYTES {
                    return Ok(Vec::new());
                }
                let mut buf = vec![0u8; 1 << 20];
                loop {
                    let n = file.read(&mut buf).map_err(|e| format!("{}: {e}", rom.display()))?;
                    if n == 0 {
                        break;
                    }
                    feed(&buf[..n]);
                }
            }
        }
        let mut out: Vec<(PathBuf, f32)> = scanners
            .into_iter()
            .flat_map(|(sys, s)| {
                let idx = &self.by_system[&sys].1;
                s.finish().into_iter().map(|(i, share)| (self.sets[idx[i]].0.clone(), share)).collect::<Vec<_>>()
            })
            .collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes that look like code: many different values, never the same run twice.
    fn noise(seed: u32, len: usize) -> Vec<u8> {
        let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect()
    }

    fn nsf(image: &[u8]) -> Vec<u8> {
        let mut h = vec![0u8; 0x80];
        h[..4].copy_from_slice(b"NESM");
        h.extend_from_slice(image);
        h
    }

    #[test]
    fn a_rip_is_found_in_every_rom_that_carries_its_bytes() {
        let driver = noise(1, 4096);
        let rip = nsf(&driver);
        let other = nsf(&noise(2, 4096));
        let m = Matcher::new(vec![fingerprint([rip.as_slice()]).unwrap(), fingerprint([other.as_slice()]).unwrap()]);

        // Two "regions": the driver at different places, around different code, one chunk
        // boundary cutting through it.
        for (pad, chunk) in [(1000usize, 7usize), (77_777, 4096)] {
            let mut rom = noise(9, pad);
            rom.extend_from_slice(&driver);
            rom.extend(noise(10, 5000));
            let mut s = m.scanner();
            for part in rom.chunks(chunk) {
                s.feed(part);
            }
            let got = s.finish();
            assert_eq!(got.len(), 1, "{got:?}");
            assert_eq!(got[0].0, 0);
            assert!(got[0].1 > 0.99);
        }
    }

    #[test]
    fn a_rom_without_the_bytes_matches_nothing() {
        let rip = nsf(&noise(1, 4096));
        let m = Matcher::new(vec![fingerprint([rip.as_slice()]).unwrap()]);
        let mut s = m.scanner();
        s.feed(&noise(3, 1 << 16));
        assert!(s.finish().is_empty());
    }

    #[test]
    fn a_rom_keeps_its_best_set_and_close_ones() {
        let m = |name: &str, share: f32| (PathBuf::from(name), share);
        let none = |_: &Path| false;
        // A shared driver well below the game's own set is left out.
        assert_eq!(choose(&[m("own", 0.98), m("sibling", 0.6)], none, none), [PathBuf::from("own")]);
        // A compilation keeps every game it holds whole.
        assert_eq!(choose(&[m("a", 0.99), m("b", 0.97), m("c", 0.96)], none, none).len(), 3);
        // A weak best match whose set belongs strongly to another ROM is that game's, not this one's.
        let claimed = |set: &Path| set == Path::new("other game");
        assert!(choose(&[m("other game", 0.55)], claimed, none).is_empty());
        // ...but with no stronger owner it stays (a port on another system, say).
        assert_eq!(choose(&[m("port", 0.55)], none, none), [PathBuf::from("port")]);
        // A low match is kept only when the same title's other version holds the set.
        let ntsc = |set: &Path| set == Path::new("ntsc rip");
        assert_eq!(choose(&[m("ntsc rip", 0.42)], none, ntsc), [PathBuf::from("ntsc rip")]);
        assert!(choose(&[m("ntsc rip", 0.42)], none, none).is_empty());
        assert!(skip_rom(Path::new("x/Super Game Boy (World) (Rev 2).sfc")));
        assert!(skip_rom(Path::new("[BIOS] Something (Japan).pce")));
        assert!(!skip_rom(Path::new("Some Game (USA).sfc")));
        assert!(multi_cart(Path::new("110-in-1 (Asia) (Unl).nes")));
        assert!(multi_cart(Path::new("36 in 1 (Unl).nes")));
        assert!(!multi_cart(Path::new("Game 2 (Japan).nes")));
    }

    #[test]
    fn padding_never_makes_a_run() {
        let rip = nsf(&[0xFF; 4096]);
        assert!(fingerprint([rip.as_slice()]).is_none());
    }

    #[test]
    fn logs_carry_no_rom_bytes() {
        assert!(rom_image(b"Vgm \0\0\0\0").is_none());
        let mut sgc = vec![0u8; 0xA0 + 64];
        sgc[..4].copy_from_slice(b"SGC\x1A");
        sgc[0x28] = 1;
        assert_eq!(rom_image(&sgc).map(|(s, i)| (s, i.len())), Some((RipSystem::GameGear, 64)));
        let mut kss = vec![0u8; 0x20 + 64];
        kss[..4].copy_from_slice(b"KSSX");
        kss[0x0E] = 0x10;
        kss[0x0F] = 0x02;
        assert_eq!(rom_image(&kss).map(|(s, i)| (s, i.len())), Some((RipSystem::MasterSystem, 64)), "an SN76489 KSS is Sega music");
        kss[0x0F] = 0;
        assert_eq!(rom_image(&kss).map(|(s, _)| s), Some(RipSystem::Msx));
    }

    #[test]
    fn a_set_of_many_files_spreads_its_runs() {
        let files: Vec<Vec<u8>> = (0..20).map(|i| nsf(&noise(100 + i, 2048))).collect();
        let fp = fingerprint(files.iter().map(Vec::as_slice)).unwrap();
        assert_eq!(fp.runs.len(), MAX_RUNS);
        // Runs from the last file are in it, not only the first file's.
        let last: std::collections::HashSet<[u8; RUN]> = files[19][0x80..].chunks_exact(RUN).map(|c| c.try_into().unwrap()).collect();
        assert!(fp.runs.iter().any(|r| last.contains(r)));
    }
}
