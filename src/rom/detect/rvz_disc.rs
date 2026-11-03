//! Disc metadata straight out of an RVZ/WIA/GCZ by random access
//! (`dolphin_wrapper::DiscReader`, the streaming build): the mirror of [`super::chd_disc`]
//! for Dolphin's containers, so a GameCube/Wii image's magic/title/game id are read at the
//! very start of an operation, before (and without) any extraction. The reader exposes the
//! plain ISO bytes, so the same platform methods apply as for a raw `.iso`.

use std::path::Path;

use crate::common::models::Platform;
use crate::rom::dolphin_wrapper::{self, DiscReader};
use crate::rom::native_lib::LibVariant;

use super::disc_dispatch::{self, DiscTitleSerial};
use super::disc_fs;

const PREFIX_START: usize = 64 * 1024;
const PREFIX_MAX: usize = 1 << 20;

/// Whether RVZ/WIA/GCZ random access is available (the streaming dolphin build is present).
pub fn available() -> bool {
    dolphin_wrapper::find_library(LibVariant::Streaming).is_some()
}

fn is_iso_platform(target: Platform) -> bool {
    matches!(target, Platform::PlayStation | Platform::PlayStation2 | Platform::PlayStationPortable | Platform::PlayStation3 | Platform::NeoGeoCd)
}

/// An RVZ/WIA/GCZ opened as its plain disc bytes.
pub struct RvzDisc {
    reader: DiscReader,
    size: u64,
    bytes_read: u64,
}

impl RvzDisc {
    pub fn open(path: &Path) -> Option<Self> {
        let reader = DiscReader::open(path, None).ok()?;
        let size = reader.info().ok()?.data_size;
        Some(Self { reader, size, bytes_read: 0 })
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    /// Exactly `len` bytes at `offset`.
    pub fn read(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, String> {
        if offset + len as u64 > self.size {
            return Err("read past end of disc".into());
        }
        let mut buf = vec![0u8; len];
        self.reader.read(offset, &mut buf).map_err(|e| e.to_string())?;
        self.bytes_read += len as u64;
        Ok(buf)
    }

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

/// The platform's disc signature check applied to the container; `None` if it can't be read.
pub fn rvz_disc_signature(path: &Path, target: Platform) -> Option<bool> {
    disc_signature(&mut RvzDisc::open(path)?, target)
}

pub fn disc_signature(disc: &mut RvzDisc, target: Platform) -> Option<bool> {
    if is_iso_platform(target) {
        let mut reader = |o: u64, l: usize| disc.read(o, l);
        if target == Platform::NeoGeoCd {
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

/// (title, serial, manufacturer id) from the container by random access, with the
/// platform's own method; `None` when the disc doesn't carry it (or it can't be read).
pub fn rvz_disc_metadata(path: &Path, target: Platform) -> Option<DiscTitleSerial> {
    disc_metadata(&mut RvzDisc::open(path)?, target)
}

pub fn disc_metadata(disc: &mut RvzDisc, target: Platform) -> Option<DiscTitleSerial> {
    if is_iso_platform(target) {
        let mut reader = |o: u64, l: usize| disc.read(o, l);
        let found = disc_dispatch::disc_metadata_with_reader(&mut reader, target)?;
        if target == Platform::NeoGeoCd && found.1.is_none() {
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
