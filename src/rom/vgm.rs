//! VGM logs for the sound chips gme lacks: the Neo Geo Pocket's T6W28 (two SN76489 halves
//! sharing tone registers, flagged in the header's SN76489 clock) and the Virtual Boy's VSU.
//! Played through libvgm, built into the `rommusic` library as `rvgm_`
//! (`deps/rommusic/vgm/rvgm.cpp`) with only those chips' cores. Every other VGM stays with
//! gme.
//!
//! The header gives the length and loop; the GD3 tag gives the titles.

use std::ffi::c_void;
use std::sync::OnceLock;

use libloading::Library;

use super::gme::TrackInfo;
use super::native_lib;

/// The rate the player renders at.
pub const SAMPLE_RATE: u32 = 44_100;

type NewFn = unsafe extern "C" fn(*const u8, usize) -> *mut c_void;
type LengthFn = unsafe extern "C" fn(*mut c_void, *mut u32, *mut u32);
type StartFn = unsafe extern "C" fn(*mut c_void) -> i32;
type RenderFn = unsafe extern "C" fn(*mut c_void, *mut i16, usize) -> i32;
type FreeFn = unsafe extern "C" fn(*mut c_void);

struct Api {
    _lib: Library,
    new: NewFn,
    length: LengthFn,
    start: StartFn,
    render: RenderFn,
    free: FreeFn,
}

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| {
        let path = native_lib::rommusic_library()?;
        unsafe {
            let lib = Library::new(&path).ok()?;
            Some(Api {
                new: *lib.get(b"rvgm_new\0").ok()?,
                length: *lib.get(b"rvgm_length\0").ok()?,
                start: *lib.get(b"rvgm_start\0").ok()?,
                render: *lib.get(b"rvgm_render\0").ok()?,
                free: *lib.get(b"rvgm_free\0").ok()?,
                _lib: lib,
            })
        }
    })
    .as_ref()
}

pub fn available() -> bool {
    api().is_some()
}

fn le32(data: &[u8], at: usize) -> Option<u32> {
    data.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Where the header ends: the commands start there, and a clock field past it is absent.
fn header_end(vgm: &[u8]) -> usize {
    let version = le32(vgm, 0x08).unwrap_or(0);
    match le32(vgm, 0x34) {
        Some(rel) if version >= 0x150 && rel != 0 => 0x34 + rel as usize,
        _ => 0x40,
    }
}

/// Offset of the SN76489 clock; bit 30 marks a second chip, bit 31 with it the T6W28.
const SN76489_CLOCK: usize = 0x0C;
/// Offset of the Virtual Boy VSU clock (VGM 1.71).
const VSU_CLOCK: usize = 0xC4;

/// Whether `vgm` (unpacked) is one this player is for: a T6W28 or a VSU.
pub fn wants(vgm: &[u8]) -> bool {
    if !vgm.starts_with(b"Vgm ") {
        return false;
    }
    let end = header_end(vgm);
    let clock = |at: usize| if at + 4 <= end { le32(vgm, at).unwrap_or(0) } else { 0 };
    let sn = clock(SN76489_CLOCK);
    (sn & 0x4000_0000 != 0 && sn & 0x3FFF_FFFF != 0) || clock(VSU_CLOCK) & 0x3FFF_FFFF != 0
}

/// The GD3 tag's strings: English and Japanese track title, game, system and author, in that
/// order (a field the tag lacks is empty).
fn gd3(vgm: &[u8]) -> Vec<String> {
    let Some(rel) = le32(vgm, 0x14).filter(|&r| r != 0) else { return Vec::new() };
    let at = 0x14 + rel as usize;
    if vgm.get(at..at + 4) != Some(b"Gd3 ") {
        return Vec::new();
    }
    let len = le32(vgm, at + 8).unwrap_or(0) as usize;
    let Some(body) = vgm.get(at + 12..(at + 12 + len).min(vgm.len())) else { return Vec::new() };
    let units: Vec<u16> = body.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    units.split(|&u| u == 0).take(11).map(|s| String::from_utf16_lossy(s).trim().to_string()).collect()
}

/// A VGM log, with its song being played.
pub struct Player {
    state: *mut c_void,
    info: TrackInfo,
    ended: bool,
}

// The handle is only touched through `&mut self`.
unsafe impl Send for Player {}

impl Player {
    pub fn open(vgm: &[u8]) -> Result<Self, String> {
        let api = api().ok_or("the VGM player (rommusic) is not installed")?;
        let state = unsafe { (api.new)(vgm.as_ptr(), vgm.len()) };
        if state.is_null() {
            return Err("not a VGM file libvgm can play".into());
        }
        let (mut total, mut lp) = (0u32, 0u32);
        unsafe { (api.length)(state, &mut total, &mut lp) };
        let tags = gd3(vgm);
        let tag = |i: usize| tags.get(i).cloned().unwrap_or_default();
        let (intro_ms, loop_ms, length_ms) = if lp > 0 { (Some(total.saturating_sub(lp)), Some(lp), None) } else { (None, None, Some(total).filter(|&t| t > 0)) };
        let song = if tag(0).is_empty() { tag(1) } else { tag(0) };
        let game = if tag(2).is_empty() { tag(3) } else { tag(2) };
        let info = TrackInfo {
            length_ms,
            intro_ms,
            loop_ms,
            play_length_ms: length_ms.unwrap_or(total.saturating_sub(lp) + 2 * lp).max(1),
            fade_ms: None,
            system: tag(4),
            game,
            song,
            author: if tag(6).is_empty() { tag(7) } else { tag(6) },
            ..Default::default()
        };
        Ok(Player { state, info, ended: false })
    }

    pub fn info(&self) -> TrackInfo {
        self.info.clone()
    }

    pub fn restart(&mut self) -> Result<(), String> {
        let api = api().ok_or("the VGM player (rommusic) is not installed")?;
        self.ended = false;
        if unsafe { (api.start)(self.state) } != 0 {
            return Err("the VGM player could not start".into());
        }
        Ok(())
    }

    pub fn play(&mut self, out: &mut [i16]) -> Result<(), String> {
        let api = api().ok_or("the VGM player (rommusic) is not installed")?;
        match unsafe { (api.render)(self.state, out.as_mut_ptr(), out.len() / 2) } {
            0 => Ok(()),
            1 => {
                self.ended = true;
                Ok(())
            }
            _ => Err("the VGM player was not started".into()),
        }
    }

    pub fn ended(&self) -> bool {
        self.ended
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        if let Some(api) = api() {
            unsafe { (api.free)(self.state) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(version: u32, clocks: &[(usize, u32)]) -> Vec<u8> {
        let mut v = vec![0u8; 0x100];
        v[..4].copy_from_slice(b"Vgm ");
        v[0x08..0x0C].copy_from_slice(&version.to_le_bytes());
        v[0x34..0x38].copy_from_slice(&(0x100u32 - 0x34).to_le_bytes());
        for &(at, clock) in clocks {
            v[at..at + 4].copy_from_slice(&clock.to_le_bytes());
        }
        v
    }

    #[test]
    fn only_the_t6w28_and_the_vsu_come_here() {
        assert!(wants(&header(0x151, &[(SN76489_CLOCK, 3_072_000 | 0xC000_0000)])), "T6W28");
        assert!(wants(&header(0x171, &[(VSU_CLOCK, 5_000_000)])), "VSU");
        assert!(!wants(&header(0x150, &[(SN76489_CLOCK, 3_579_545)])), "one SN76489 stays with gme");
        let mut short = header(0x150, &[(VSU_CLOCK, 5_000_000)]);
        short[0x34..0x38].copy_from_slice(&0x0Cu32.to_le_bytes());
        assert!(!wants(&short), "a field past the header is not read");
    }

    #[test]
    fn the_gd3_tag_gives_titles() {
        let mut v = header(0x151, &[]);
        let at = v.len();
        v[0x14..0x18].copy_from_slice(&((at - 0x14) as u32).to_le_bytes());
        let text: Vec<u8> = ["Stage 1", "", "Game", "", "System"].iter().flat_map(|s| s.encode_utf16().chain([0]).flat_map(|u| u.to_le_bytes())).collect();
        v.extend_from_slice(b"Gd3 \x00\x01\x00\x00");
        v.extend_from_slice(&(text.len() as u32).to_le_bytes());
        v.extend_from_slice(&text);
        assert_eq!(gd3(&v)[..5], ["Stage 1", "", "Game", "", "System"].map(String::from));
    }
}
