//! The RetroAchievements hash of a ROM inside a container, read in memory.
//!
//! RAHasher reads files, and has no CHD, RVZ, CSO or PBP reader of its own here. A disc hash
//! reads only some sectors (a PS1 one: the volume descriptor, `SYSTEM.CNF` and the boot
//! executable), so instead of extracting anything, RAHasher is given the container's content
//! files as [`RaFiles`]: the cue or gdi a CD CHD decodes to, served from the bytes
//! `ContentPlan` builds anyway, and each track or ISO read by random access through the same
//! readers the scan's decode uses, sector by sector, only where RAHasher asks. A cartridge in
//! a zip or 7z is read into memory whole (it is small). Nothing touches the disk.

use std::path::Path;
use std::sync::Arc;

use super::chd_cd::CdImageReader;
use super::chdman_wrapper::ChdReader;
use super::content::{ContainerPlan, ContentPlan, InputSet};
use super::detect::sheet::{CueSheetOptions, SheetKind};
use super::hash::{self, RaFiles, RahashError};
use super::sevenzip_wrapper::SevenZipArchive;
use super::source::ReadAt;

/// A cartridge in an archive larger than this is not one.
const CARTRIDGE_MAX: u64 = 512 << 20;

enum Body {
    Bytes(Vec<u8>),
    /// `[start, start + size)` of the image `reader` reads.
    Span(u64),
}

struct File {
    name: String,
    size: u64,
    body: Body,
}

struct Files<'r> {
    files: Vec<File>,
    reader: Option<Box<dyn ReadAt + 'r>>,
}

impl RaFiles for Files<'_> {
    fn open(&mut self, name: &str) -> Option<(i64, u64)> {
        let index = self.files.iter().position(|f| f.name.eq_ignore_ascii_case(name))?;
        Some((index as i64 + 1, self.files[index].size))
    }

    fn read(&mut self, handle: i64, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        let file = self.files.get((handle - 1) as usize).ok_or_else(|| std::io::Error::other("no such file"))?;
        match &file.body {
            Body::Bytes(bytes) => {
                let at = offset as usize;
                buf.copy_from_slice(bytes.get(at..at + buf.len()).ok_or_else(|| std::io::Error::other("read past the end"))?);
                Ok(())
            }
            Body::Span(start) => self.reader.as_mut().ok_or_else(|| std::io::Error::other("no reader"))?.read_at(start + offset, buf),
        }
    }
}

/// The base name of a content file (`Disc 1/track.bin` -> `track.bin`): what a sheet names.
fn base(name: &str) -> String {
    name.rsplit(['/', '\\']).next().unwrap_or(name).to_string()
}

/// Whether [`hash_of`] reads `path` (by extension) rather than RAHasher reading it from disk.
pub fn handles(path: &Path) -> bool {
    matches!(path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(), Some("chd" | "rvz" | "wia" | "gcz" | "cso" | "pbp" | "zip" | "7z"))
}

/// The RetroAchievements hash of `path` for `console`: a CHD, RVZ, CSO, PBP or an archived
/// cartridge read in memory ([`hash_of`]), anything else read by RAHasher where it lies (a cue
/// finds its tracks). Nothing is extracted to disk.
pub fn ra_hash(path: &Path, console: u32) -> Result<hash::RahashOutcome, RahashError> {
    if handles(path) {
        // N64, SNES, NES, PC Engine, Lynx, DS, 7800, DSi, FDS: the cartridge (or disk) systems
        // whose RA hash is not the file's MD5.
        let cartridge = matches!(console, 2 | 3 | 7 | 8 | 13 | 18 | 51 | 78 | 81);
        return hash_of(path, console, cartridge).map(hash::RahashOutcome::Done);
    }
    hash::rahash_file(path, console, 0)
}

/// The RA hash of the ROM in container `path` for `console`. `cartridge`: a zip or 7z holds
/// one cartridge (its largest member is hashed); a disc in an archive is not hashed.
pub fn hash_of(path: &Path, console: u32, cartridge: bool) -> Result<String, RahashError> {
    let failed = |e: String| RahashError::Failed(format!("{}: {e}", path.display()));
    let input = InputSet::resolve_file(path).map_err(failed)?;
    // RAHasher reads either sheet; a GD-ROM as a gdi, as MAME and chdman lay it out.
    let plan = ContentPlan::build(&input, &CueSheetOptions::chdman(), SheetKind::Gdi).map_err(|e| failed(e.to_string()))?;
    let span = |i: usize, start: u64| File { name: base(&plan.files[i].name), size: plan.files[i].size, body: Body::Span(start) };
    match &plan.container {
        ContainerPlan::ChdCd { layout, sheet_bytes } => {
            let tracks: Arc<[_]> = layout.tracks.clone().into();
            let reader = ChdReader::open(path, None, 0).map_err(|e| failed(e.to_string()))?;
            let mut files = vec![File { name: base(&plan.files[0].name), size: sheet_bytes.len() as u64, body: Body::Bytes(sheet_bytes.clone()) }];
            files.extend(layout.files().into_iter().enumerate().map(|(i, (_, start, _))| span(i + 1, start)));
            let name = files[0].name.clone();
            hash::rahash_vfs(&name, console, &mut Files { files, reader: Some(Box::new(CdImageReader::new(reader, tracks))) })
        }
        ContainerPlan::ChdImage { .. } => {
            let reader = ChdReader::open(path, None, 0).map_err(|e| failed(e.to_string()))?;
            single(&plan, console, Box::new(reader))
        }
        ContainerPlan::Disc { .. } => {
            let reader = super::dolphin_wrapper::DiscReader::open(path, None).map_err(|e| failed(e.to_string()))?;
            single(&plan, console, Box::new(reader))
        }
        ContainerPlan::Cso { .. } => {
            let reader = super::cso::CsoReader::open(path).map_err(|e| failed(e.to_string()))?;
            single(&plan, console, Box::new(reader))
        }
        // The first disc: the one RetroAchievements hashes a multi-disc PBP by.
        ContainerPlan::Pbp { discs } => {
            let disc = discs.first().ok_or_else(|| failed("no disc".to_string()))?;
            let pbp = super::pbp::Pbp::open(path).map_err(failed)?;
            let mut files = vec![File { name: base(&disc.cue_name), size: disc.cue.len() as u64, body: Body::Bytes(disc.cue.clone().into_bytes()) }];
            for (i, track) in disc.tracks.iter().enumerate() {
                files.push(span(i + 1, track.first_sector * super::pbp::SECTOR));
            }
            let name = files[0].name.clone();
            hash::rahash_vfs(&name, console, &mut Files { files, reader: Some(Box::new(super::pbp::DiscReader::new(&pbp, disc))) })
        }
        ContainerPlan::Zip { format, entries } => {
            if !cartridge {
                return Err(failed("a disc in an archive".to_string()));
            }
            let (index, name, size) = entries.iter().max_by_key(|(_, _, size)| *size).ok_or_else(|| failed("empty archive".to_string()))?;
            if *size > CARTRIDGE_MAX {
                return Err(failed("too large to be a cartridge".to_string()));
            }
            let archive = SevenZipArchive::open(path, *format, |_, _| true).map_err(|e| failed(e.to_string()))?;
            let bytes = archive.read_entry(*index).map_err(|e| failed(e.to_string()))?;
            let name = base(name);
            hash::rahash_vfs(&name, console, &mut Files { files: vec![File { name: name.clone(), size: bytes.len() as u64, body: Body::Bytes(bytes) }], reader: None })
        }
        ContainerPlan::None => Err(failed("not a container".to_string())),
    }
}

/// A container whose content is one image (an ISO).
fn single(plan: &ContentPlan, console: u32, reader: Box<dyn ReadAt + '_>) -> Result<String, RahashError> {
    let file = File { name: base(&plan.files[0].name), size: plan.files[0].size, body: Body::Span(0) };
    let name = file.name.clone();
    hash::rahash_vfs(&name, console, &mut Files { files: vec![file], reader: Some(reader) })
}
