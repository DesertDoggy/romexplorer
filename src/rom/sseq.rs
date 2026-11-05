//! Nintendo DS sequenced music played from the game's own sound archive (SDAT) through kode54's
//! SSEQ Player (`external/SSEQPlayer`, WTFPL), built into the `rommusic` library as `rsseq_`
//! (`deps/rommusic/sseq/rsseq.cpp`). Loaded at run time like [`super::agbplay`]: when the
//! library has not been built for this machine, or is older than this, [`available`] is false.
//!
//! Most DS games keep their music this way: a sequence per song (SSEQ), played on the DS's
//! sound channels with the instruments of a bank (SBNK) and its wave archives (SWAR). The
//! streams beside them (STRM) are read by vgmstream instead, through [`super::inner_archives`].

use std::ffi::{c_char, c_void, CStr};
use std::sync::OnceLock;

use libloading::Library;

use super::native_lib;

type SongFn = unsafe extern "C" fn(u32, *const c_char, u32, u32, *mut c_void);
type ListFn = unsafe extern "C" fn(*const u8, usize, SongFn, *mut c_void, *mut c_char, usize) -> i32;
type OpenFn = unsafe extern "C" fn(*const u8, usize, u32, u32, *mut c_char, usize) -> *mut c_void;
type CloseFn = unsafe extern "C" fn(*mut c_void);
type RenderFn = unsafe extern "C" fn(*mut c_void, *mut i16, usize) -> usize;
type EndedFn = unsafe extern "C" fn(*mut c_void) -> i32;

struct Api {
    _lib: Library,
    list: ListFn,
    open: OpenFn,
    close: CloseFn,
    render: RenderFn,
    ended: EndedFn,
}

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| {
        let path = native_lib::rommusic_library()?;
        unsafe {
            let lib = Library::new(&path).ok()?;
            Some(Api {
                list: *lib.get(b"rsseq_list\0").ok()?,
                open: *lib.get(b"rsseq_open\0").ok()?,
                close: *lib.get(b"rsseq_close\0").ok()?,
                render: *lib.get(b"rsseq_render\0").ok()?,
                ended: *lib.get(b"rsseq_ended\0").ok()?,
                _lib: lib,
            })
        }
    })
    .as_ref()
}

/// Whether the DS sequence player is in this machine's `rommusic` library.
pub fn available() -> bool {
    api().is_some()
}

fn error_text(buf: &[c_char]) -> String {
    unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned()
}

/// One song in a sound archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Song {
    pub id: u32,
    /// From the archive's symbol table (`SEQ_BGM_FIELD`...); empty when it has none.
    pub name: String,
    pub bank: u32,
    pub volume: u32,
}

/// The songs in sound archive `sdat`.
pub fn songs(sdat: &[u8]) -> Result<Vec<Song>, String> {
    let api = api().ok_or("the DS music player (rommusic) is not installed")?;
    unsafe extern "C" fn each(id: u32, name: *const c_char, bank: u32, volume: u32, user: *mut c_void) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let out = unsafe { &mut *(user as *mut Vec<Song>) };
            let name = if name.is_null() { String::new() } else { unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned() };
            out.push(Song { id, name, bank, volume });
        }));
    }
    let mut out: Vec<Song> = Vec::new();
    let mut err = [0 as c_char; 256];
    let n = unsafe { (api.list)(sdat.as_ptr(), sdat.len(), each, &mut out as *mut _ as *mut c_void, err.as_mut_ptr(), err.len()) };
    if n < 0 {
        return Err(error_text(&err));
    }
    Ok(out)
}

/// One song, playing.
pub struct Player {
    api: &'static Api,
    handle: *mut c_void,
    rate: u32,
}

// The handle is used only through `&mut self`.
unsafe impl Send for Player {}

impl Player {
    pub fn open(sdat: &[u8], id: u32, rate: u32) -> Result<Self, String> {
        let api = api().ok_or("the DS music player (rommusic) is not installed")?;
        let mut err = [0 as c_char; 256];
        let handle = unsafe { (api.open)(sdat.as_ptr(), sdat.len(), id, rate, err.as_mut_ptr(), err.len()) };
        if handle.is_null() {
            return Err(error_text(&err));
        }
        Ok(Self { api, handle, rate })
    }

    pub fn sample_rate(&self) -> u32 {
        self.rate
    }

    /// Fills `out` (interleaved stereo, an even length).
    pub fn render(&mut self, out: &mut [i16]) -> Result<(), String> {
        let frames = out.len() / 2;
        if unsafe { (self.api.render)(self.handle, out.as_mut_ptr(), frames) } != frames {
            return Err("the DS music player failed".into());
        }
        Ok(())
    }

    /// Every track has finished and every note died away. A looping song never does.
    pub fn ended(&self) -> bool {
        unsafe { (self.api.ended)(self.handle) != 0 }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        unsafe { (self.api.close)(self.handle) };
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn std_header(magic: &[u8; 4], size: u32, blocks: u16) -> Vec<u8> {
        let mut h = magic.to_vec();
        h.extend_from_slice(&0x0100_FEFFu32.to_le_bytes());
        h.extend_from_slice(&size.to_le_bytes());
        h.extend_from_slice(&16u16.to_le_bytes());
        h.extend_from_slice(&blocks.to_le_bytes());
        h
    }

    fn le32(v: &mut Vec<u8>, x: u32) {
        v.extend_from_slice(&x.to_le_bytes());
    }

    /// A one-song sound archive: `sequence` played on one instrument, a square wave.
    pub(crate) fn sdat_with(sequence: &[u8], name: &str) -> Vec<u8> {
        // SSEQ: header, DATA block (size, offset of the data from the SSEQ's start), data.
        let mut sseq = std_header(b"SSEQ", 0, 1);
        sseq.extend_from_slice(b"DATA");
        le32(&mut sseq, 12 + sequence.len() as u32);
        le32(&mut sseq, 0x1C);
        sseq.extend_from_slice(sequence);
        // SWAV: 8-bit PCM, looping, 32 samples of a square wave.
        let mut swav = vec![0u8, 1];
        swav.extend_from_slice(&8372u16.to_le_bytes());
        swav.extend_from_slice(&((16_756_991u32 / 8372) as u16).to_le_bytes());
        swav.extend_from_slice(&0u16.to_le_bytes());
        le32(&mut swav, 8);
        swav.extend((0..32).map(|i| if i < 16 { 0x60u8 } else { 0xA0 }));
        // SWAR: header, DATA, 8 reserved words, count, offsets.
        let mut swar = std_header(b"SWAR", 0, 1);
        swar.extend_from_slice(b"DATA");
        le32(&mut swar, 0);
        swar.extend_from_slice(&[0; 32]);
        le32(&mut swar, 1);
        let at = swar.len() as u32 + 4;
        le32(&mut swar, at);
        swar.extend_from_slice(&swav);
        // SBNK: one instrument of record 1 (PCM) using wave 0 of archive 0.
        let mut sbnk = std_header(b"SBNK", 0, 1);
        sbnk.extend_from_slice(b"DATA");
        le32(&mut sbnk, 0);
        sbnk.extend_from_slice(&[0; 32]);
        le32(&mut sbnk, 1);
        let def_at = sbnk.len() as u16 + 4;
        sbnk.push(1);
        sbnk.extend_from_slice(&def_at.to_le_bytes());
        sbnk.push(0);
        sbnk.extend_from_slice(&0u16.to_le_bytes()); // swav
        sbnk.extend_from_slice(&0u16.to_le_bytes()); // swar
        sbnk.extend_from_slice(&[60, 127, 127, 127, 127, 64]);

        // SDAT: header + 4 block offsets/sizes, then SYMB, INFO, FAT, the files.
        let mut symb = b"SYMB".to_vec();
        le32(&mut symb, 0);
        let mut recs = [0u32; 8];
        let seq_rec_at = 8 + 32;
        recs[0] = seq_rec_at;
        for r in &recs {
            le32(&mut symb, *r);
        }
        le32(&mut symb, 1);
        let name_at = seq_rec_at + 8;
        le32(&mut symb, name_at);
        symb.extend_from_slice(name.as_bytes());
        symb.push(0);
        while symb.len() % 4 != 0 {
            symb.push(0);
        }

        let mut info = b"INFO".to_vec();
        le32(&mut info, 0);
        // Records: SEQ (0), BANK (2), WAVEARC (3), each a count and one entry offset.
        let base = 8 + 32;
        let (seq_at, bank_at, war_at) = (base, base + 8 + 12, base + 8 + 12 + 8 + 12);
        let mut offs = [0u32; 8];
        offs[0] = seq_at;
        offs[2] = bank_at;
        offs[3] = war_at;
        for o in &offs {
            le32(&mut info, *o);
        }
        // SEQ: file 0, bank 0, volume 127.
        le32(&mut info, 1);
        le32(&mut info, seq_at + 8);
        info.extend_from_slice(&0u16.to_le_bytes());
        info.extend_from_slice(&0u16.to_le_bytes());
        info.extend_from_slice(&0u16.to_le_bytes());
        info.extend_from_slice(&[127, 64, 64, 0, 0, 0]);
        // BANK: file 1, wave archive 0.
        le32(&mut info, 1);
        le32(&mut info, bank_at + 8);
        info.extend_from_slice(&1u16.to_le_bytes());
        info.extend_from_slice(&0u16.to_le_bytes());
        for w in [0u16, 0xFFFF, 0xFFFF, 0xFFFF] {
            info.extend_from_slice(&w.to_le_bytes());
        }
        // WAVEARC: file 2.
        le32(&mut info, 1);
        le32(&mut info, war_at + 8);
        info.extend_from_slice(&2u16.to_le_bytes());
        info.extend_from_slice(&[0; 10]);

        let head_len = 16 + 32 + 8;
        let symb_at = head_len as u32;
        let info_at = symb_at + symb.len() as u32;
        let fat_at = info_at + info.len() as u32;
        let fat_len = 12 + 3 * 16;
        let mut files_at = fat_at + fat_len as u32;
        let mut fat = b"FAT ".to_vec();
        le32(&mut fat, fat_len as u32);
        le32(&mut fat, 3);
        for f in [&sseq, &sbnk, &swar] {
            le32(&mut fat, files_at);
            le32(&mut fat, f.len() as u32);
            le32(&mut fat, 0);
            le32(&mut fat, 0);
            files_at += f.len() as u32;
        }
        let mut sdat = std_header(b"SDAT", 0, 4);
        for (at, len) in [(symb_at, symb.len()), (info_at, info.len()), (fat_at, fat_len)] {
            le32(&mut sdat, at);
            le32(&mut sdat, len as u32);
        }
        le32(&mut sdat, 0);
        le32(&mut sdat, 0);
        sdat.extend_from_slice(&[0; 8]);
        for part in [&symb, &info, &fat, &sseq, &sbnk, &swar] {
            sdat.extend_from_slice(part);
        }
        sdat
    }

    #[test]
    fn a_sound_archive_lists_and_plays_its_song() {
        if !available() {
            eprintln!("rommusic with rsseq is not built here; skipped");
            return;
        }
        // Patch 0, then C4 for 48 ticks, a rest, E4, then the end.
        let seq = [0x81, 0x00, 60, 100, 48, 0x80, 24, 64, 100, 48, 0xFF];
        let sdat = sdat_with(&seq, "SEQ_BGM_TEST");
        assert_eq!(songs(&sdat).unwrap(), vec![Song { id: 0, name: "SEQ_BGM_TEST".into(), bank: 0, volume: 127 }]);
        let mut p = Player::open(&sdat, 0, 32_000).unwrap();
        let mut buf = vec![0i16; 32_000 * 2];
        p.render(&mut buf).unwrap();
        let peak = buf.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!(peak > 500, "{peak}");
        // At 120 bpm, 48 ticks a beat: about 1.25 s of notes, then the release dies away.
        for _ in 0..5 {
            p.render(&mut buf).unwrap();
        }
        assert!(p.ended());
        assert!(Player::open(&sdat, 7, 32_000).is_err());
        assert!(songs(&sdat[..40]).is_err());
    }
}
