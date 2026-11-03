//! Disc metadata straight out of a CHD by random access (`chdman_wrapper::ChdReader`, the
//! streaming build): only the sectors the platform's own method asks for are decoded -- no
//! extraction. This is the default way CHD metadata is read (see
//! `source::default_access`); many CHDs are handled in parallel by
//! [`chd_disc_metadata_batch`], one reader per thread, defaulting to one thread per logical
//! CPU with no cap.
//!
//! Per platform, the same methods as for an extracted image:
//! - ISO9660-based (PS1, PS2, PSP, PS3): `disc_dispatch::disc_metadata_with_reader` over the
//!   disc's first data track (CD) or its logical bytes (DVD).
//! - byte-based (PC Engine CD, Sega CD/Saturn/Dreamcast, ...): `disc_metadata_from_bytes` over a
//!   prefix of the first data track, grown 64 KiB -> 1 MiB until the platform's signature
//!   shows up (1 MiB is the widest window any of them scans).

use std::path::{Path, PathBuf};

use crate::common::models::Platform;
use crate::rom::chdman_wrapper::{self, ChdReader, ChdTrack, CD_MAX_SECTOR_BYTES, CD_TRACK_AUDIO, CD_TRACK_RAW_DONTCARE};
use crate::rom::native_lib::{self, LibVariant};

use super::disc_dispatch::{self, DiscTitleSerial};
use super::disc_fs;

const PREFIX_START: usize = 64 * 1024;
const PREFIX_MAX: usize = 1 << 20;

/// Whether CHD random access is available (the streaming chdman build is present).
pub fn available() -> bool {
    chdman_wrapper::find_library(LibVariant::Streaming).is_some()
}

fn is_iso_platform(target: Platform) -> bool {
    matches!(target, Platform::PlayStation | Platform::PlayStation2 | Platform::PlayStationPortable | Platform::PlayStation3 | Platform::NeoGeoCd)
}

/// A CHD opened as the disc bytes the platform methods expect: the first data track as its
/// extracted `.bin` would read (CD/GD-ROM), or the logical image (DVD/raw).
pub struct ChdDisc {
    reader: ChdReader,
    track: Option<ChdTrack>,
    size: u64,
    sector: Vec<u8>,
    bytes_read: u64,
}

impl ChdDisc {
    pub fn open(path: &Path) -> Option<Self> {
        Self::open_data_track(path, 0)
    }

    /// The `nth` data track (0 = the first) of a CD / GD-ROM CHD; a DVD's image for any `nth`.
    pub fn open_data_track(path: &Path, nth: usize) -> Option<Self> {
        let reader = ChdReader::open(path, None, 8).ok()?;
        let info = reader.info().ok()?;
        let (track, size) = if info.is_cd {
            let track = (0..info.num_tracks).filter_map(|i| reader.track(i).ok()).filter(|t| t.track_type != CD_TRACK_AUDIO).nth(nth)?;
            (Some(track), track.frames as u64 * track.data_size as u64)
        } else {
            (None, info.logical_bytes)
        };
        Some(Self { reader, track, size, sector: vec![0u8; CD_MAX_SECTOR_BYTES], bytes_read: 0 })
    }

    /// Size of the disc bytes this view exposes.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Disc bytes handed out by [`Self::read`] so far.
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    /// Exactly `len` bytes at `offset`.
    pub fn read(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, String> {
        if offset + len as u64 > self.size {
            return Err("read past end of disc".into());
        }
        self.bytes_read += len as u64;
        let Some(track) = self.track else {
            let mut buf = vec![0u8; len];
            self.reader.read_bytes(offset, &mut buf).map_err(|e| e.to_string())?;
            return Ok(buf);
        };
        let data_size = track.data_size as u64;
        let mut out = Vec::with_capacity(len);
        let (mut pos, end) = (offset, offset + len as u64);
        while pos < end {
            let lba = (pos / data_size) as u32;
            let within = (pos % data_size) as usize;
            self.reader
                .read_sector(track.physical_start + lba, &mut self.sector, CD_TRACK_RAW_DONTCARE, true)
                .map_err(|e| e.to_string())?;
            let take = (data_size as usize - within).min((end - pos) as usize);
            out.extend_from_slice(&self.sector[within..within + take]);
            pos += take as u64;
        }
        Ok(out)
    }

    /// Smallest prefix (64 KiB doubling up to 1 MiB) in which `found` holds, or the 1 MiB one.
    /// Each step reads only the new part.
    fn grow_prefix(&mut self, found: impl Fn(&[u8]) -> bool) -> Option<(Vec<u8>, bool)> {
        let limit = (PREFIX_MAX as u64).min(self.size) as usize;
        let mut bytes = Vec::new();
        let mut len = PREFIX_START.min(limit);
        loop {
            let more = self.read(bytes.len() as u64, len - bytes.len()).ok()?;
            bytes.extend_from_slice(&more);
            if found(&bytes) {
                return Some((bytes, true));
            }
            if len >= limit {
                return Some((bytes, false));
            }
            len = (len * 2).min(limit);
        }
    }
}

/// A Dreamcast disc's IP.BIN header (from its `SEGA SEGAKATANA` hardware id, 0x100 bytes):
/// in a GD-ROM CHD the high-density area's first data track (track 3) carries the game's,
/// so that is read first, then the first data track.
pub fn dreamcast_ip_bin(path: &Path) -> Option<Vec<u8>> {
    for nth in [1, 0] {
        let Some(mut disc) = ChdDisc::open_data_track(path, nth) else { continue };
        if let Some((bytes, true)) = disc.grow_prefix(|b| disc_dispatch::sega_disc_hardware_id_offset(b, Platform::Dreamcast).is_some()) {
            let at = disc_dispatch::sega_disc_hardware_id_offset(&bytes, Platform::Dreamcast)?;
            return bytes.get(at..at + 0x100).map(<[u8]>::to_vec);
        }
    }
    None
}

/// The platform's disc signature check, applied to a CHD: ISO9660 system id ("PLAYSTATION" for
/// PS1/PS2, "PSP GAME", "PS3VOLUME") or the byte signature (PC Engine CD, Sega, GameCube/Wii).
/// `None` if the CHD can't be read (or random access isn't available).
pub fn chd_disc_signature(path: &Path, target: Platform) -> Option<bool> {
    disc_signature(&mut ChdDisc::open(path)?, target)
}

/// [`chd_disc_signature`] on an already open disc.
pub fn disc_signature(disc: &mut ChdDisc, target: Platform) -> Option<bool> {
    if is_iso_platform(target) {
        let mut reader = |o: u64, l: usize| disc.read(o, l);
        if target == Platform::NeoGeoCd {
            // The loader script every Neo Geo CD boots from.
            return Some(disc_fs::read_iso_file_bytes_with(&mut reader, &["IPL.TXT"]).is_some());
        }
        let id = disc_fs::read_iso_system_id_with(&mut reader).unwrap_or_default();
        let expected = match target {
            Platform::PlayStationPortable => "PSP GAME",
            Platform::PlayStation3 => "PS3VOLUME",
            _ => "PLAYSTATION",
        };
        return Some(id.starts_with(expected));
    }
    let (_, found) = disc.grow_prefix(|bytes| byte_signature(bytes, target))?;
    Some(found)
}

fn byte_signature(bytes: &[u8], target: Platform) -> bool {
    match target {
        Platform::TurboGrafxCd => disc_dispatch::pce_cd_signature_offset(bytes).is_some(),
        Platform::Dreamcast | Platform::SegaSaturn | Platform::SegaCd => disc_dispatch::sega_disc_hardware_id_offset(bytes, target).is_some(),
        Platform::GameCube | Platform::Wii => disc_dispatch::nintendo_disc_magic_offset(bytes, target).is_some(),
        Platform::ThreeDo => disc_dispatch::three_do_signature_offset(bytes).is_some(),
        Platform::PcFx => disc_dispatch::pcfx_signature_offset(bytes).is_some(),
        _ => false,
    }
}

/// (title, serial, manufacturer id) from a CHD by random access, with the platform's own
/// method; `None` when the disc doesn't carry it (or the CHD can't be read).
pub fn chd_disc_metadata(path: &Path, target: Platform) -> Option<DiscTitleSerial> {
    disc_metadata(&mut ChdDisc::open(path)?, target)
}

/// [`chd_disc_metadata`] on an already open disc.
pub fn disc_metadata(disc: &mut ChdDisc, target: Platform) -> Option<DiscTitleSerial> {
    if is_iso_platform(target) {
        let mut reader = |o: u64, l: usize| disc.read(o, l);
        let found = disc_dispatch::disc_metadata_with_reader(&mut reader, target)?;
        if target == Platform::NeoGeoCd && found.1.is_none() {
            // The game id sits behind the `NEO-GEO` marker in the boot area; scan the
            // first data track's prefix for it (the same scan the extracted-file path does).
            let (bytes, _) = disc.grow_prefix(|bytes| crate::rom::platforms::other::infer_neogeo_cd_game_id_from_bytes(bytes).is_some())?;
            let id = crate::rom::platforms::other::infer_neogeo_cd_game_id_from_bytes(&bytes);
            return Some((found.0, id.clone(), id));
        }
        return Some(found);
    }
    let (bytes, _) = disc.grow_prefix(|bytes| byte_signature(bytes, target))?;
    let found = disc_dispatch::disc_metadata_from_bytes(&bytes, target, false);
    (found.0.is_some() || found.1.is_some()).then_some(found)
}

/// [`chd_disc_metadata`] for many CHDs at once, one reader per thread. `threads`: `None` = one
/// per logical CPU (no cap). Results are in input order.
pub fn chd_disc_metadata_batch(items: &[(PathBuf, Platform)], threads: Option<usize>) -> Vec<Option<DiscTitleSerial>> {
    let threads = threads.unwrap_or_else(native_lib::recommended_reader_threads).clamp(1, items.len().max(1));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results: Vec<std::sync::Mutex<Option<DiscTitleSerial>>> = items.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some((path, target)) = items.get(i) else { break };
                *results[i].lock().unwrap() = chd_disc_metadata(path, *target);
            });
        }
    });
    results.into_iter().map(|m| m.into_inner().unwrap()).collect()
}
