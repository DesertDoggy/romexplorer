//! Which container an input wants.
//!
//! `romexplorer::rom::archive` picks its chdman command from the extension alone, because the
//! catalogue has already told it the platform. A dropped file comes with no such context, so
//! this looks at the image itself: a GameCube/Wii header means RVZ, raw 2352-byte sectors mean
//! a CD, and a cooked 2048-byte image is a DVD. An Xbox 360 image becomes Xenia's `.zar`, an
//! Xbox image an XISO, a Wii U disc (`.wud`/`.wux`) Cemu's `.wua`: the three lossy targets,
//! there because those emulators read no lossless container.
//!
//! Inputs are lossless only: disc images, CHD, RVZ, CSO and PBP. A `.wua`, `.zar` or XISO
//! has already lost the disc, so it is not taken.

use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use romexplorer::common::models::Platform;

use crate::cart::Cart;
use romexplorer::rom::detect::descriptor::descriptor_payload_paths;
use romexplorer::rom::detect::disc_dispatch::nintendo_disc_magic_offset;
use romexplorer::rom::models::ChdTrackType;
use romexplorer::rom::sheet_build::probe_track;
use romexplorer::rom::xbox;

/// Extensions a directory walk picks up: disc images and containers, then cartridge dumps.
pub const SUPPORTED_EXTENSIONS: [&str; 31] = [
    "cue", "gdi", "toc", "iso", "bin", "img", "gcm", "chd", "rvz", "wud", "wux", "cso", "pbp", "nes", "unh", "z64", "v64", "n64", "sfc", "smc", "swc", "cia", "3ds", "cci", "cxi", "cfa",
    "3dsx", "zcci", "zcia", "zcxi", "z3dsx",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    CdChd,
    GdChd,
    DvdChd,
    Rvz,
    /// A Wii U disc, decrypted into Cemu's archive. Needs the disc's key.
    Wua,
    /// An Xbox 360 disc's game files, in Xenia's archive.
    Zar,
    /// An Xbox disc's game partition alone, as xemu reads it.
    Xiso,
}

impl Target {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Rvz => "rvz",
            Self::Wua => "wua",
            Self::Zar => "zar",
            // Beside its source, which is an .iso too.
            Self::Xiso => "xiso.iso",
            _ => "chd",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::CdChd => "CD CHD",
            Self::GdChd => "GD-ROM CHD",
            Self::DvdChd => "DVD CHD",
            Self::Rvz => "RVZ",
            Self::Wua => "WUA",
            Self::Zar => "ZAR",
            Self::Xiso => "XISO",
        }
    }

    /// Keeps the disc's files but not the disc: not for bit-perfect archiving.
    pub fn is_lossy(self) -> bool {
        matches!(self, Self::Wua | Self::Zar | Self::Xiso)
    }

    pub fn is_cd(self) -> bool {
        matches!(self, Self::CdChd | Self::GdChd)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// Already a CHD: extract, read or hash it.
    Chd,
    /// Already an RVZ.
    Rvz,
    /// A PS2 / PSP ISO in compressed blocks: extracted to the ISO, or made a DVD CHD.
    Cso,
    /// A NES, N64 or SNES cartridge dump: rewritten into the settings' layout, not compressed.
    Cartridge(Cart),
    /// A 3DS title (`crate::n3ds`): decrypted into the settings' format.
    N3ds,
    /// A PlayStation disc (or several) in Sony's EBOOT.PBP: extracted to cue + bin, or made
    /// a CD CHD.
    Pbp,
    Image {
        target: Target,
        /// What the creator is fed: the sheet for a CD, the image itself otherwise.
        source: PathBuf,
        /// A raw CD track with no sheet anywhere beside it; one has to be written first.
        needs_cue: bool,
    },
}

pub fn extension(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

pub fn classify(path: &Path) -> Result<Input, String> {
    if !path.is_file() {
        return Err(format!("not a file: {}", path.display()));
    }
    let image = |target, source: &Path, needs_cue| Ok(Input::Image { target, source: source.to_path_buf(), needs_cue });
    if let Some(cart) = Cart::of(path) {
        return Ok(Input::Cartridge(cart));
    }
    if crate::n3ds::is_3ds(path) {
        return Ok(Input::N3ds);
    }
    match extension(path).as_str() {
        "chd" => Ok(Input::Chd),
        "rvz" => Ok(Input::Rvz),
        "cso" => Ok(Input::Cso),
        "pbp" => Ok(Input::Pbp),
        "wud" | "wux" => image(Target::Wua, path, false),
        "cue" | "toc" => image(Target::CdChd, path, false),
        "gdi" => image(Target::GdChd, path, false),
        "iso" | "bin" | "img" | "gcm" => {
            let size = std::fs::metadata(path).map_err(|err| format!("{}: {err}", path.display()))?.len();
            let mut head = [0u8; 0x20];
            let read = File::open(path).and_then(|mut file| file.read(&mut head)).map_err(|err| format!("{}: {err}", path.display()))?;
            let head = &head[..read];
            if nintendo_disc_magic_offset(head, Platform::GameCube).is_some() || nintendo_disc_magic_offset(head, Platform::Wii).is_some() {
                return image(Target::Rvz, path, false);
            }
            if let Some(target) = xbox_target(path)? {
                return image(target, path, false);
            }
            let probe = probe_track(path, size).map_err(|err| format!("{}: {err}", path.display()))?;
            // A size that is whole 2352-byte sectors is also whole 2048-byte ones every 147
            // sectors, so a cooked PS2 or PSP ISO can look like an audio track: one with an
            // ISO 9660 or UDF volume where a cooked image keeps it is a DVD.
            if probe.track_type == ChdTrackType::Audio && size % 2048 == 0 && cooked_volume(path) {
                return image(Target::DvdChd, path, false);
            }
            if probe.sector_size == 2352 {
                // An audio verdict is only "no sync pattern and not 2048-aligned": believe it
                // for a file whose size really is whole raw sectors, and nothing else.
                if probe.track_type == ChdTrackType::Audio && (size == 0 || size % 2352 != 0) {
                    return Err(format!("not a disc image chdlite recognises: {}", path.display()));
                }
                // A track file is normally one of several a sheet describes: convert the disc,
                // not the track.
                return match sibling_sheet(path) {
                    Some(sheet) => classify(&sheet),
                    None => image(Target::CdChd, path, true),
                };
            }
            image(Target::DvdChd, path, false)
        }
        other => Err(format!("unsupported input type .{other}: {}", path.display())),
    }
}

/// Whether sector 16 of a 2048-byte image holds an ISO 9660 or UDF volume descriptor.
fn cooked_volume(path: &Path) -> bool {
    use std::io::{Seek, SeekFrom};
    let mut id = [0u8; 6];
    let read = File::open(path).and_then(|mut file| {
        file.seek(SeekFrom::Start(16 * 2048))?;
        file.read_exact(&mut id)
    });
    read.is_ok() && matches!(&id[1..], b"CD001" | b"BEA01" | b"NSR02" | b"NSR03")
}

/// An Xbox or Xbox 360 image, by its XDVDFS volume. Only a full (Redump) image is taken: one
/// whose game partition starts at 0 is already an XISO, the video partition gone.
fn xbox_target(path: &Path) -> Result<Option<Target>, String> {
    let Some(platform) = xbox::platform_of(path) else { return Ok(None) };
    let base = File::open(path).and_then(|mut file| xbox::open_volume(&mut file)).map_err(|err| format!("{}: {err}", path.display()))?.map(|v| v.base);
    if base == Some(0) {
        return Err(format!("{}: an XISO, which has already lost the disc's video partition -- chdlite takes the full disc image", path.display()));
    }
    Ok(Some(if platform == Platform::Xbox360 { Target::Zar } else { Target::Xiso }))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.file_name(), b.file_name()) {
        (Some(a), Some(b)) => a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()),
        _ => false,
    }
}

/// The `.gdi` or `.cue` beside `track` that lists it (gdi first, as everywhere in romexplorer).
pub fn sibling_sheet(track: &Path) -> Option<PathBuf> {
    let dir = track.parent()?;
    let mut sheets: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| matches!(extension(path).as_str(), "cue" | "gdi") && path.is_file())
        .collect();
    sheets.sort_by_key(|path| (extension(path) != "gdi", path.clone()));
    sheets.into_iter().find(|sheet| descriptor_payload_paths(sheet).iter().any(|payload| same_file(payload, track)))
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = entries.filter_map(|entry| entry.ok().map(|e| e.path())).collect();
    entries.sort();
    for path in entries {
        // Hidden entries are never a dropped game: chdlite's own scratch folders and the
        // marks it leaves while writing, and macOS's `._` resource files.
        if path.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
            continue;
        }
        if path.is_dir() {
            walk(&path, out);
        } else if SUPPORTED_EXTENSIONS.contains(&extension(&path).as_str()) {
            out.push(path);
        }
    }
}

/// Turns what was dropped (files and directories, any mix) into the list of inputs to run:
/// directories walked recursively, and every track file a sheet in the list already covers
/// dropped, so a `.cue` and its `.bin`s are one job, not several.
pub fn expand(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_dir() {
            walk(path, &mut files);
        } else {
            files.push(path.clone());
        }
    }
    // A track dropped on its own stands for its disc, so two tracks of one disc are one job.
    let files: Vec<PathBuf> =
        files.into_iter().map(|path| if matches!(extension(&path).as_str(), "bin" | "img") { sibling_sheet(&path).unwrap_or(path) } else { path }).collect();
    // Separators and case differ between a path as dropped and the same path as a sheet
    // names it; neither makes it a different file.
    let key = |path: &Path| path.to_string_lossy().replace('\\', "/").to_lowercase();
    let covered: HashSet<String> =
        files.iter().filter(|path| matches!(extension(path).as_str(), "cue" | "gdi")).flat_map(|sheet| descriptor_payload_paths(sheet)).map(|p| key(&p)).collect();
    let mut seen = HashSet::new();
    files.into_iter().filter(|path| !covered.contains(&key(path)) && seen.insert(key(path))).collect()
}

/// The inputs a create would make a lossy output of ([`Target::is_lossy`]), for the warning
/// asked or printed first.
pub fn lossy_targets(inputs: &[PathBuf]) -> Vec<(PathBuf, Target)> {
    inputs
        .iter()
        .filter_map(|path| match classify(path) {
            Ok(Input::Image { target, .. }) if target.is_lossy() => Some((path.clone(), target)),
            _ => None,
        })
        .collect()
}
