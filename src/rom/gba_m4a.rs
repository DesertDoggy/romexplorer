//! GBA music read from the ROM: the song table of Nintendo's standard sound engine ("m4a",
//! also called Sappy or MusicPlayer2000), which most GBA games use unchanged.
//!
//! The engine's song-select routine is the same compiled code in every game that links it, and
//! its literal pool holds the address of the song table. Each table entry points at a song
//! header (track count, voice group, track pointers); entries the game leaves unused point at a
//! shared empty song. So the songs are found without any rip, in any game with the engine.

/// The start of the engine's song-select routine, as compiled in every game that uses it.
const SELECT_SONG: [u8; 30] = [
    0x00, 0xB5, 0x00, 0x04, 0x07, 0x4A, 0x08, 0x49, 0x40, 0x0B, 0x40, 0x18, 0x83, 0x88, 0x59, 0x00, 0xC9, 0x18, 0x89, 0x00, 0x89, 0x18, 0x0A, 0x68,
    0x01, 0x68, 0x10, 0x1C, 0x00, 0xF0,
];
/// Where, after the routine's start, its literal pool holds the song table's address.
const TABLE_POINTER_AT: usize = 40;
/// GBA cartridge ROM is mapped at this address.
const ROM_BASE: u32 = 0x0800_0000;
const MAX_ROM: usize = 0x0200_0000;
/// A table longer than this has run past its end into other data.
const MAX_SONGS: usize = 2048;
/// A song with more tracks than this is not a song header.
const MAX_TRACKS: u8 = 16;

fn u32_at(rom: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(rom.get(at..at + 4)?.try_into().ok()?))
}

/// A ROM address as an offset in the ROM image, if it points inside it.
fn offset(rom: &[u8], address: u32) -> Option<usize> {
    let off = address.checked_sub(ROM_BASE)? as usize;
    (off < rom.len().min(MAX_ROM)).then_some(off)
}

/// One song as the table lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Song {
    /// The song number the game plays it by.
    pub number: u32,
    /// Where its header is in the ROM image.
    pub header: usize,
    pub tracks: u8,
    /// Which music player (channel group) it plays on: BGM and sound effects usually differ.
    pub player: u16,
}

/// The engine's song table in a ROM image, if the engine is there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SongTable {
    pub offset: usize,
    pub songs: Vec<Song>,
}

/// Where the engine's song-select routine is, if the ROM links it.
pub fn find_select_song(rom: &[u8]) -> Option<usize> {
    // The routine is Thumb code, so it starts on an even address.
    rom.windows(SELECT_SONG.len()).enumerate().step_by(2).find(|(_, w)| *w == SELECT_SONG).map(|(at, _)| at)
}

/// A song header at `at`: track count, then (after the voice group) that many track pointers
/// into the ROM.
fn song_header(rom: &[u8], at: usize) -> Option<u8> {
    let tracks = *rom.get(at)?;
    if tracks > MAX_TRACKS {
        return None;
    }
    offset(rom, u32_at(rom, at + 4)?).filter(|_| tracks > 0)?;
    (0..tracks as usize).all(|t| u32_at(rom, at + 8 + t * 4).and_then(|p| offset(rom, p)).is_some()).then_some(tracks)
}

/// The song table at `table`, read entry by entry until an entry stops looking like one.
/// Empty slots (headers with no tracks) are left out of `songs`; the numbers of the rest are
/// kept.
fn read_table(rom: &[u8], table: usize) -> SongTable {
    let mut songs = Vec::new();
    for n in 0..MAX_SONGS {
        let at = table + n * 8;
        let Some(header) = u32_at(rom, at).and_then(|p| offset(rom, p)) else { break };
        let player = u16::from_le_bytes([rom[at + 4], rom[at + 5]]);
        match rom.get(header) {
            Some(0) => continue,
            Some(_) => match song_header(rom, header) {
                Some(tracks) => songs.push(Song { number: n as u32, header, tracks, player }),
                None => break,
            },
            None => break,
        }
    }
    SongTable { offset: table, songs }
}

/// The song table: from the engine's song-select routine where the ROM has the usual build of
/// it, else by the table's own shape.
pub fn song_table(rom: &[u8]) -> Option<SongTable> {
    let from_code = find_select_song(rom).and_then(|r| offset(rom, u32_at(rom, r + TABLE_POINTER_AT)?)).map(|t| read_table(rom, t));
    match from_code {
        Some(t) if t.songs.len() >= MIN_SONGS => Some(t),
        other => find_by_shape(rom).or(other),
    }
}

/// A table found by shape has at least this many songs; fewer is likely chance.
const MIN_SONGS: usize = 4;
/// The players a table found by shape plays on: the engine's usual set-up, as
/// `ragb_open_at` assumes it.
const SHAPE_PLAYERS: u16 = 4;

/// Whether an entry at `at` looks like a song table entry: a pointer to a song header (or an
/// empty one), then the music player number twice, small.
fn entry_shape(rom: &[u8], at: usize) -> bool {
    let Some(e) = rom.get(at..at + 8) else { return false };
    let player = u16::from_le_bytes([e[4], e[5]]);
    if player >= 32 || e[4..6] != e[6..8] {
        return false;
    }
    u32_at(rom, at).and_then(|p| offset(rom, p)).is_some_and(|h| rom.get(h) == Some(&0) || song_header(rom, h).is_some())
}

/// The longest run of song-table entries in the ROM, for engines whose song-select routine was
/// built differently (another SDK version or compiler).
pub fn find_by_shape(rom: &[u8]) -> Option<SongTable> {
    let mut best: Option<SongTable> = None;
    let mut at = 0;
    while at + 8 <= rom.len() {
        if !entry_shape(rom, at) {
            at += 4;
            continue;
        }
        let mut len = 0;
        while entry_shape(rom, at + len * 8) && len < MAX_SONGS {
            len += 1;
        }
        let mut table = read_table(rom, at);
        table.songs.retain(|s| (s.number as usize) < len);
        // Real tables put nearly every song on one of the engine's few players; a run that
        // only looks like a table has them scattered.
        let on_players = table.songs.iter().filter(|s| s.player < SHAPE_PLAYERS).count();
        let plausible = on_players * 10 >= table.songs.len() * 9;
        table.songs.retain(|s| s.player < SHAPE_PLAYERS);
        if plausible && table.songs.len() >= MIN_SONGS && best.as_ref().is_none_or(|b| table.songs.len() > b.songs.len()) {
            best = Some(table);
        }
        at += len.max(1) * 8;
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put32(rom: &mut [u8], at: usize, v: u32) {
        rom[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }

    /// A ROM with the routine at 0x100, a table at 0x1000 of: song 0 (2 tracks), an empty slot,
    /// song 2 (1 track), then data that is not a table entry.
    fn rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x4000];
        rom[0x100..0x100 + SELECT_SONG.len()].copy_from_slice(&SELECT_SONG);
        put32(&mut rom, 0x100 + TABLE_POINTER_AT, ROM_BASE + 0x1000);
        let empty = 0x2000;
        let song = |rom: &mut Vec<u8>, at: usize, tracks: u8| {
            rom[at] = tracks;
            put32(rom, at + 4, ROM_BASE + 0x3000);
            for t in 0..tracks as usize {
                put32(rom, at + 8 + t * 4, ROM_BASE + 0x3100 + t as u32 * 0x10);
            }
        };
        song(&mut rom, 0x2100, 2);
        song(&mut rom, 0x2200, 1);
        put32(&mut rom, 0x1000, ROM_BASE + 0x2100);
        rom[0x1004] = 0;
        put32(&mut rom, 0x1008, ROM_BASE + empty as u32);
        rom[0x1014] = 1;
        rom[0x1016] = 1;
        put32(&mut rom, 0x1010, ROM_BASE + 0x2200);
        put32(&mut rom, 0x1018, 0xFFFF_FFFF);
        rom
    }

    #[test]
    fn the_song_table_is_found_from_the_engine_code() {
        let rom = rom();
        assert_eq!(find_select_song(&rom), Some(0x100));
        let table = song_table(&rom).unwrap();
        assert_eq!(table.offset, 0x1000);
        let numbers: Vec<(u32, u8, u16)> = table.songs.iter().map(|s| (s.number, s.tracks, s.player)).collect();
        assert_eq!(numbers, [(0, 2, 0), (2, 1, 1)]);
    }

    #[test]
    fn a_table_is_found_by_shape_without_the_usual_routine() {
        let mut rom = rom();
        rom[0x100..0x100 + SELECT_SONG.len()].fill(0);
        // Two songs is too few to trust by shape alone; add more.
        for n in 3..8u32 {
            let at = 0x1000 + n as usize * 8;
            put32(&mut rom, at, ROM_BASE + 0x2200);
            rom[at + 4] = 1;
            rom[at + 6] = 1;
        }
        put32(&mut rom, 0x1040, 0xFFFF_FFFF);
        let table = song_table(&rom).unwrap();
        assert_eq!(table.offset, 0x1000);
        assert_eq!(table.songs.len(), 7);
    }

    #[test]
    fn a_rom_without_the_engine_has_no_table() {
        assert!(song_table(&[0u8; 0x4000]).is_none());
    }
}
