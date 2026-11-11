//! The track lists of CloneCD (`.ccd` + `.img`) and Alcohol 120% (`.mds` + `.mdf`) images,
//! for reading their data and CD audio like a cue sheet's.

/// One track of the image file, as stored in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawTrack {
    pub number: u32,
    pub audio: bool,
    /// Byte offset of the track's first sector (its INDEX 01) in the image file.
    pub start: u64,
    /// Bytes per sector in the image file.
    pub sector: u32,
}

/// A `.ccd`'s tracks: its image is raw 2352-byte sectors, `[TRACK n]` sections give each
/// track's `MODE` (0 = audio) and `INDEX 1` (its first sector).
pub fn ccd(text: &str) -> Vec<RawTrack> {
    let mut tracks: Vec<RawTrack> = Vec::new();
    let mut current: Option<RawTrack> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            if let Some(t) = current.take() {
                tracks.push(t);
            }
            let name = line.trim_matches(|c| c == '[' || c == ']').trim();
            if let Some(n) = name.strip_prefix("TRACK").or_else(|| name.strip_prefix("Track")).and_then(|n| n.trim().parse::<u32>().ok()) {
                current = Some(RawTrack { number: n, audio: false, start: 0, sector: 2352 });
            }
            continue;
        }
        let Some(t) = current.as_mut() else { continue };
        let Some((key, value)) = line.split_once('=') else { continue };
        let (key, value) = (key.trim().to_ascii_uppercase(), value.trim());
        match key.as_str() {
            "MODE" => t.audio = value == "0",
            "INDEX 1" => {
                if let Ok(lba) = value.parse::<u64>() {
                    t.start = lba * 2352;
                }
            }
            _ => {}
        }
    }
    if let Some(t) = current {
        tracks.push(t);
    }
    tracks.sort_by_key(|t| t.number);
    tracks
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// An `.mds`'s tracks (version 1, the format Alcohol 120% writes): each session block points
/// at 80-byte track blocks; points 1-99 are tracks, the rest lead-in entries.
pub fn mds(b: &[u8]) -> Result<Vec<RawTrack>, String> {
    if !b.starts_with(b"MEDIA DESCRIPTOR") {
        return Err("not an Alcohol .mds".into());
    }
    let sessions = u16_at(b, 0x14).ok_or("short .mds")? as usize;
    let session_at = u32_at(b, 0x50).ok_or("short .mds")? as usize;
    let mut tracks = Vec::new();
    for s in 0..sessions.min(99) {
        let at = session_at + s * 24;
        let blocks = *b.get(at + 0x0A).ok_or("short .mds session")? as usize;
        let blocks_at = u32_at(b, at + 0x14).ok_or("short .mds session")? as usize;
        for k in 0..blocks {
            let t = blocks_at + k * 80;
            let block = b.get(t..t + 80).ok_or("short .mds track block")?;
            let point = block[4];
            if point == 0 || point > 99 {
                continue;
            }
            // The track mode (0xA9/0xE9 audio), else the control nibble's data bit.
            let mode = block[0];
            let audio = if mode != 0 { mode & 0x0F == 0x09 } else { block[2] & 0x40 == 0 };
            let sector = u16_at(block, 0x10).unwrap_or(2352) as u32;
            let start = u64_at(block, 0x28).unwrap_or(0);
            tracks.push(RawTrack { number: point as u32, audio, start, sector });
        }
    }
    tracks.sort_by_key(|t| t.number);
    Ok(tracks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ccd_tracks() {
        let text = "[CloneCD]\nVersion=3\n[Disc]\nTocEntries=5\n[Entry 0]\nSession=1\n[TRACK 1]\nMODE=2\nINDEX 1=0\n[TRACK 2]\nMODE=0\nINDEX 0=1000\nINDEX 1=1150\n";
        assert_eq!(
            ccd(text),
            vec![
                RawTrack { number: 1, audio: false, start: 0, sector: 2352 },
                RawTrack { number: 2, audio: true, start: 1150 * 2352, sector: 2352 },
            ]
        );
    }

    #[test]
    fn mds_tracks() {
        let mut b = vec![0u8; 0x200];
        b[..16].copy_from_slice(b"MEDIA DESCRIPTOR");
        b[0x14] = 1;
        b[0x50..0x54].copy_from_slice(&0x60u32.to_le_bytes());
        // Session at 0x60: 3 blocks (one lead-in entry, two tracks) at 0x80.
        b[0x60 + 0x0A] = 3;
        b[0x60 + 0x14..0x60 + 0x18].copy_from_slice(&0x80u32.to_le_bytes());
        let block = |b: &mut Vec<u8>, i: usize, point: u8, mode: u8, start: u64| {
            let t = 0x80 + i * 80;
            b[t] = mode;
            b[t + 4] = point;
            b[t + 0x10..t + 0x12].copy_from_slice(&2352u16.to_le_bytes());
            b[t + 0x28..t + 0x30].copy_from_slice(&start.to_le_bytes());
        };
        block(&mut b, 0, 0xA0, 0, 0);
        block(&mut b, 1, 1, 0xAA, 0);
        block(&mut b, 2, 2, 0xA9, 5000 * 2352);
        assert_eq!(
            mds(&b).unwrap(),
            vec![
                RawTrack { number: 1, audio: false, start: 0, sector: 2352 },
                RawTrack { number: 2, audio: true, start: 5000 * 2352, sector: 2352 },
            ]
        );
    }
}
