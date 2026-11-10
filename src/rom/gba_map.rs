//! Game Boy Advance music maps: what a GBA ROM needs laid over it to play each of its songs,
//! found from a matched GSF set and kept as numbers only.
//!
//! A GSF track is the game's own cartridge image, cut down and patched so that it boots
//! straight into one song: a library file holds the image (bytes the song never reads left
//! as zeros), and each track file adds the few bytes that pick its song. So the ROM is most
//! of the answer already. A map keeps where the set's image differs from the ROM inside the
//! areas the set covers: the boot patches and song numbers as bytes, the cut-down areas as
//! zero runs. Played, the ROM with those bytes laid over it is the set's own image, so it
//! plays what the set plays, on the same mGBA core.
//!
//! Bytes every track shares are kept once; each track keeps its own on top, with its length
//! and fade from the set's tags.

use super::psf::{self, GbaImage, Loaded};

/// A run of bytes laid over the ROM at a cartridge offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Run {
    Bytes(u32, Vec<u8>),
    /// That many zero bytes.
    Zeros(u32, u32),
}

impl Run {
    fn at(&self) -> u32 {
        match self {
            Run::Bytes(a, _) | Run::Zeros(a, _) => *a,
        }
    }

    fn end(&self) -> usize {
        match self {
            Run::Bytes(a, b) => *a as usize + b.len(),
            Run::Zeros(a, n) => (*a + *n) as usize,
        }
    }

    fn apply(&self, image: &mut Vec<u8>) {
        if image.len() < self.end() {
            image.resize(self.end(), 0);
        }
        match self {
            Run::Bytes(a, b) => image[*a as usize..self.end()].copy_from_slice(b),
            Run::Zeros(a, _) => image[*a as usize..self.end()].fill(0),
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

/// A set belongs to the ROM when at least this share of its non-zero bytes is the ROM's: a
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

/// The share of the image's non-zero bytes inside `spans` that the ROM has the same.
pub fn same_share(image: &[u8], spans: &[(usize, usize)], rom: &[u8]) -> f64 {
    let (mut same, mut all) = (0usize, 0usize);
    for &(a, b) in spans {
        for i in a..b.min(image.len()) {
            if image[i] != 0 {
                all += 1;
                same += usize::from(rom.get(i) == Some(&image[i]));
            }
        }
    }
    if all == 0 {
        0.0
    } else {
        same as f64 / all as f64
    }
}

/// Where `image` differs from `rom` inside `spans`, as runs: long stretches of zeros as zero
/// runs, everything else as bytes.
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

/// Splits a stretch into byte runs and zero runs of at least [`ZERO_RUN`].
fn push_split(runs: &mut Vec<Run>, at: usize, bytes: &[u8]) {
    let mut i = 0;
    let mut start = 0;
    while i < bytes.len() {
        if bytes[i] != 0 {
            i += 1;
            continue;
        }
        let z = i;
        while i < bytes.len() && bytes[i] == 0 {
            i += 1;
        }
        if i - z >= ZERO_RUN {
            if z > start {
                runs.push(Run::Bytes((at + start) as u32, bytes[start..z].to_vec()));
            }
            runs.push(Run::Zeros((at + z) as u32, (i - z) as u32));
            start = i;
        }
    }
    if bytes.len() > start {
        runs.push(Run::Bytes((at + start) as u32, bytes[start..].to_vec()));
    }
}

/// One track's runs from its loaded file, or `None` when the set is not this ROM's.
pub fn track_runs(loaded: &Loaded, rom: &[u8]) -> Option<Vec<Run>> {
    let sections = loaded.gba_sections();
    if sections.is_empty() {
        return None;
    }
    let image = image_of(&sections);
    let spans = covered(&sections);
    if same_share(&image, &spans, rom) < SAME_SHARE {
        return None;
    }
    Some(diff(&image, &spans, rom))
}

impl Map {
    /// The map from each track's runs (in the set's order) and its tags.
    pub fn from_tracks(rom_len: usize, tracks: Vec<(Vec<Run>, Option<u32>, Option<u32>)>) -> Option<Map> {
        let first = tracks.first()?.0.clone();
        let common: Vec<Run> = first.into_iter().filter(|r| tracks.iter().all(|(t, _, _)| t.contains(r))).collect();
        let tracks = tracks
            .into_iter()
            .map(|(runs, length, fade)| Track {
                runs: runs.into_iter().filter(|r| !common.contains(r)).collect(),
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
        for r in self.common.iter().chain(&track.runs) {
            r.apply(&mut image);
        }
        Ok(image)
    }

    pub fn encode(&self) -> String {
        fn runs(out: &mut String, runs: &[Run]) {
            for r in runs {
                match r {
                    Run::Bytes(a, b) => {
                        out.push_str(&format!(" {a:x}:"));
                        for x in b {
                            out.push_str(&format!("{x:02x}"));
                        }
                    }
                    Run::Zeros(a, n) => out.push_str(&format!(" {a:x}:z{n:x}")),
                }
            }
        }
        let mut out = format!("gbamap 1\nl {}\nc", self.rom_len);
        runs(&mut out, &self.common);
        out.push('\n');
        for t in &self.tracks {
            out.push_str(&format!("t {} {}", t.length_ms, t.fade_ms));
            runs(&mut out, &t.runs);
            out.push('\n');
        }
        out
    }

    pub fn decode(text: &str) -> Option<Map> {
        fn runs<'a>(words: impl Iterator<Item = &'a str>) -> Option<Vec<Run>> {
            words
                .map(|w| {
                    let (a, v) = w.split_once(':')?;
                    let a = u32::from_str_radix(a, 16).ok()?;
                    if let Some(n) = v.strip_prefix('z') {
                        return Some(Run::Zeros(a, u32::from_str_radix(n, 16).ok()?));
                    }
                    if v.len() % 2 != 0 {
                        return None;
                    }
                    let bytes = (0..v.len()).step_by(2).map(|i| u8::from_str_radix(&v[i..i + 2], 16).ok()).collect::<Option<Vec<u8>>>()?;
                    Some(Run::Bytes(a, bytes))
                })
                .collect()
        }
        let mut lines = text.lines();
        if lines.next()? != "gbamap 1" {
            return None;
        }
        let rom_len = lines.next()?.strip_prefix("l ")?.parse().ok()?;
        let mut common_words = lines.next()?.split_whitespace();
        if common_words.next()? != "c" {
            return None;
        }
        let common = runs(common_words)?;
        let mut tracks = Vec::new();
        for line in lines.filter(|l| !l.is_empty()) {
            let mut w = line.split_whitespace();
            if w.next()? != "t" {
                return None;
            }
            let length_ms = w.next()?.parse().ok()?;
            let fade_ms = w.next()?.parse().ok()?;
            tracks.push(Track { runs: runs(w)?, length_ms, fade_ms });
        }
        Some(Map { rom_len, common, tracks })
    }

    /// How many bytes the map lays over the ROM that are not zeros.
    pub fn file_bytes(&self) -> usize {
        let n = |runs: &[Run]| runs.iter().map(|r| if let Run::Bytes(_, b) = r { b.len() } else { 0 }).sum::<usize>();
        n(&self.common) + self.tracks.iter().map(|t| n(&t.runs)).sum::<usize>()
    }
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
    for loaded in tracks {
        let info = loaded.info();
        let runs = track_runs(loaded, rom)?;
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
    fn diff_keeps_patches_as_bytes_and_cut_areas_as_zeros() {
        let rom: Vec<u8> = (0..200u32).map(|i| (i % 251 + 1) as u8).collect();
        let mut image = rom.clone();
        image[10] = 0xAA;
        image[11] = 0xBB;
        image[50..90].fill(0);
        image[120] = 0;
        let runs = diff(&image, &[(0, 200)], &rom);
        assert_eq!(runs, vec![Run::Bytes(10, vec![0xAA, 0xBB]), Run::Zeros(50, 40), Run::Bytes(120, vec![0])]);
        let mut back = rom.clone();
        runs.iter().for_each(|r| r.apply(&mut back));
        assert_eq!(back, image);
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
        let shared = Run::Zeros(0x100, 0x40);
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
