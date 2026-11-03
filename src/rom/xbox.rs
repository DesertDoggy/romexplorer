//! Xbox and Xbox 360 discs: their XDVDFS filesystem, and the two forms emulators play them
//! from that are not the disc image.
//!
//! - **Xbox 360 → `.zar`** (Xenia Canary): the game partition's files in a ZArchive, written
//!   through `cemuwua`'s ZArchive API ([`super::cemu_wrapper::zar_create`]). The image is read
//!   once, front to back, and hashed whole in the same pass, so the manifest
//!   (`romexplorer.json`, the same shape as a `.wua`'s) records the disc it came from at no
//!   extra read. [`verify_zar`] checks an archive against it.
//! - **Xbox → `.xiso`** (xemu): extract-xiso's rewrite, through `exiso`
//!   (`deps/extract-xiso/exiso.h`). An XISO is a disc image of its own, so it carries no
//!   manifest; [`compare_files`] checks it file by file against the image it came from.
//!
//! Both drop what the Redump image holds besides the game's files (the video partition, the
//! security and unused sectors), so neither can be turned back into the image.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use libloading::Library;
use sha1::{Digest, Sha1};

use super::cemu_wrapper::{self, Manifest, ManifestFile, ManifestTitle};
use super::hash::{HashDigest, HashFlags, HashReport};
use super::native_lib;
use crate::common::models::Platform;

pub const SECTOR: u64 = 2048;
const MAGIC: &[u8; 20] = b"MICROSOFT*XBOX*MEDIA";

/// Where the game partition starts: an XISO (0), and the three Redump layouts -- XGD1 (Xbox),
/// XGD2 and XGD3 (Xbox 360).
pub const PARTITION_OFFSETS: [u64; 4] = [0, 0x1830_0000, 0x0FD9_0000, 0x0208_0000];

/// The tool name a `.zar` manifest carries.
pub const ZAR_TOOL: &str = "romexplorer-zar";

/// An XDVDFS volume found in an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Volume {
    /// Byte offset of the game partition in the image.
    pub base: u64,
    root_sector: u32,
    root_size: u32,
}

/// One entry of the filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// `/`-separated, from the partition root.
    pub path: String,
    pub is_dir: bool,
    /// Absolute byte offset of the file's data in the image.
    pub offset: u64,
    pub size: u64,
}

fn read_at(file: &mut File, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(buf)
}

/// The game partition, when `file` holds one (at any of [`PARTITION_OFFSETS`]).
pub fn open_volume(file: &mut File) -> std::io::Result<Option<Volume>> {
    let len = file.metadata()?.len();
    let mut vd = [0u8; 2048];
    for base in PARTITION_OFFSETS {
        let at = base + 32 * SECTOR;
        if at + SECTOR > len || read_at(file, at, &mut vd).is_err() {
            continue;
        }
        if &vd[..20] == MAGIC && &vd[0x7EC..0x800] == MAGIC {
            let root_sector = u32::from_le_bytes(vd[0x14..0x18].try_into().unwrap());
            let root_size = u32::from_le_bytes(vd[0x18..0x1C].try_into().unwrap());
            return Ok(Some(Volume { base, root_sector, root_size }));
        }
    }
    Ok(None)
}

/// Every entry, directories before what they hold, each directory's entries in the order its
/// tree sorts them.
pub fn entries(file: &mut File, volume: &Volume) -> std::io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    if volume.root_size > 0 {
        let file_len = file.metadata()?.len();
        let mut visited = std::collections::HashSet::new();
        walk_dir(file, file_len, volume.base, volume.root_sector, volume.root_size, "", &mut out, &mut visited, 0)?;
    }
    Ok(out)
}

fn bad(what: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, what.into())
}

/// `visited` holds every directory table read so far: a damaged or made-up image whose
/// directories name each other (or the root) would otherwise be walked without end.
#[allow(clippy::too_many_arguments)]
fn walk_dir(file: &mut File, file_len: u64, base: u64, sector: u32, size: u32, prefix: &str, out: &mut Vec<Entry>, visited: &mut std::collections::HashSet<(u32, u32)>, depth: usize) -> std::io::Result<()> {
    if depth > 64 {
        return Err(bad(format!("{prefix}: directories nested too deep")));
    }
    if !visited.insert((sector, size)) {
        return Err(bad(format!("{prefix}: a directory names one already listed")));
    }
    // Inside the file, before anything is allocated for it.
    if base + sector as u64 * SECTOR + size as u64 > file_len {
        return Err(bad(format!("{prefix}: a directory table lies past the end of the image")));
    }
    let mut table = vec![0u8; size as usize];
    read_at(file, base + sector as u64 * SECTOR, &mut table)?;
    // A binary tree: each node names its left and right children by offset in 4-byte units.
    let mut stack = vec![0usize];
    let mut seen = std::collections::HashSet::new();
    let mut here = Vec::new();
    while let Some(at) = stack.pop() {
        if !seen.insert(at) || at + 14 > table.len() {
            continue;
        }
        let left = u16::from_le_bytes([table[at], table[at + 1]]);
        let right = u16::from_le_bytes([table[at + 2], table[at + 3]]);
        let start = u32::from_le_bytes(table[at + 4..at + 8].try_into().unwrap());
        let len = u32::from_le_bytes(table[at + 8..at + 12].try_into().unwrap());
        let attributes = table[at + 12];
        let name_len = table[at + 13] as usize;
        if left == 0xFFFF && right == 0xFFFF && start == u32::MAX {
            continue; // an empty directory's table, or padding
        }
        if at + 14 + name_len > table.len() {
            return Err(bad(format!("{prefix}: a directory entry runs past its table")));
        }
        let name = String::from_utf8_lossy(&table[at + 14..at + 14 + name_len]).into_owned();
        if left != 0 && left != 0xFFFF {
            stack.push(left as usize * 4);
        }
        if right != 0 && right != 0xFFFF {
            stack.push(right as usize * 4);
        }
        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return Err(bad(format!("{prefix}: a directory entry has a bad name")));
        }
        here.push((name, attributes & 0x10 != 0, start, len));
    }
    here.sort_by(|a, b| a.0.to_ascii_uppercase().cmp(&b.0.to_ascii_uppercase()));
    for (name, is_dir, start, len) in here {
        let path = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        out.push(Entry { path: path.clone(), is_dir, offset: base + start as u64 * SECTOR, size: if is_dir { 0 } else { len as u64 } });
        if is_dir && len > 0 {
            walk_dir(file, file_len, base, start, len, &path, out, visited, depth + 1)?;
        }
    }
    Ok(())
}

/// Which console a disc is for: an Xbox 360 game boots `default.xex`, an Xbox one
/// `default.xbe`. `None` when the image holds no XDVDFS volume or neither file.
pub fn platform_of(path: &Path) -> Option<Platform> {
    let mut file = File::open(path).ok()?;
    let volume = open_volume(&mut file).ok()??;
    let list = entries(&mut file, &volume).ok()?;
    let has = |n: &str| list.iter().any(|e| !e.is_dir && e.path.eq_ignore_ascii_case(n));
    if has("default.xex") {
        Some(Platform::Xbox360)
    } else if has("default.xbe") {
        Some(Platform::Xbox)
    } else {
        // The XGD2/XGD3 layouts are Xbox 360 only.
        match volume.base {
            0x0FD9_0000 | 0x0208_0000 => Some(Platform::Xbox360),
            0x1830_0000 => Some(Platform::Xbox),
            _ => None,
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What [`create_zar`] read and wrote.
#[derive(Debug, Clone)]
pub struct ZarWritten {
    /// The whole image's CRC-32, MD5 and SHA-1, and its size.
    pub image: HashReport,
    pub image_size: u64,
    pub manifest: Manifest,
}

/// Writes the game partition's files of the Xbox 360 image `iso` into a `.zar` at `out`.
///
/// The image is read once, front to back, and hashed whole on the way; `source` turns those
/// hashes into the manifest's `"source"` (normally a [`cemu_wrapper::SourceRecord`] with its
/// DAT verdict). `progress(done, total)` gets image bytes read; returning false cancels and
/// removes the output.
pub fn create_zar(
    iso: &Path,
    out: &Path,
    source: impl FnOnce(&HashReport, u64) -> serde_json::Value,
    mut progress: impl FnMut(u64, u64) -> bool,
) -> Result<ZarWritten, String> {
    let shown = iso.display().to_string();
    let mut file = File::open(iso).map_err(|e| format!("{shown}: {e}"))?;
    let total = file.metadata().map_err(|e| format!("{shown}: {e}"))?.len();
    let volume = open_volume(&mut file).map_err(|e| format!("{shown}: {e}"))?.ok_or_else(|| format!("{shown}: not an Xbox or Xbox 360 disc image"))?;
    let list = entries(&mut file, &volume).map_err(|e| format!("{shown}: {e}"))?;
    let mut writer = cemu_wrapper::zar_create(out).map_err(|e| e.to_string())?;
    for e in list.iter().filter(|e| e.is_dir) {
        writer.dir(&e.path).map_err(|e| e.to_string())?;
    }
    let mut files: Vec<&Entry> = list.iter().filter(|e| !e.is_dir).collect();
    if let Some(e) = files.iter().find(|e| e.offset + e.size > total) {
        return Err(format!("{shown}: {} runs past the end of the image -- it may be cut short", e.path));
    }
    files.sort_by_key(|e| (e.offset, e.size));

    // Files one after another in the image are streamed in the same pass that hashes it; one
    // that shares bytes with the file before it is read again on its own afterwards.
    let mut in_order: Vec<&Entry> = Vec::new();
    let mut again: Vec<&Entry> = Vec::new();
    let mut end = 0u64;
    for e in files {
        if e.size > 0 && e.offset < end {
            again.push(e);
        } else {
            end = e.offset + e.size;
            in_order.push(e);
        }
    }

    let digest = HashDigest::new(HashFlags::CRC32 | HashFlags::MD5 | HashFlags::SHA1);
    let mut hashed: Vec<(String, u64, String)> = Vec::with_capacity(in_order.len() + again.len());
    let mut current: Option<(usize, Sha1, u64)> = None; // index into in_order, hasher, bytes written
    let mut next = 0usize;
    let mut buf = vec![0u8; 4 << 20];
    let mut pos = 0u64;
    file.seek(SeekFrom::Start(0)).map_err(|e| format!("{shown}: {e}"))?;
    let finish_file = |current: &mut Option<(usize, Sha1, u64)>, hashed: &mut Vec<(String, u64, String)>, in_order: &[&Entry]| {
        if let Some((i, h, _)) = current.take() {
            hashed.push((in_order[i].path.clone(), in_order[i].size, hex(&h.finalize())));
        }
    };
    loop {
        let n = file.read(&mut buf).map_err(|e| format!("{shown}: {e}"))?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        digest.update(chunk).map_err(|e| e.to_string())?;
        let chunk_end = pos + n as u64;
        let mut at = pos;
        while at < chunk_end {
            if current.is_none() {
                // Files with nothing in them, and the next file once its start is reached.
                while next < in_order.len() && in_order[next].size == 0 {
                    writer.file(&in_order[next].path).map_err(|e| e.to_string())?;
                    hashed.push((in_order[next].path.clone(), 0, hex(&Sha1::new().finalize())));
                    next += 1;
                }
                if next >= in_order.len() || in_order[next].offset >= chunk_end {
                    break;
                }
                let e = in_order[next];
                writer.file(&e.path).map_err(|e| e.to_string())?;
                current = Some((next, Sha1::new(), 0));
                next += 1;
            }
            let (i, h, written) = current.as_mut().unwrap();
            let e = in_order[*i];
            let from = e.offset + *written;
            if from >= chunk_end {
                break;
            }
            let take = (e.size - *written).min(chunk_end - from);
            let slice = &chunk[(from - pos) as usize..(from - pos + take) as usize];
            writer.write(slice).map_err(|e| e.to_string())?;
            h.update(slice);
            *written += take;
            at = from + take;
            if *written == e.size {
                finish_file(&mut current, &mut hashed, &in_order);
            }
        }
        pos = chunk_end;
        if !progress(pos, total) {
            return Err("cancelled".into());
        }
    }
    finish_file(&mut current, &mut hashed, &in_order);
    while next < in_order.len() && in_order[next].size == 0 {
        writer.file(&in_order[next].path).map_err(|e| e.to_string())?;
        hashed.push((in_order[next].path.clone(), 0, hex(&Sha1::new().finalize())));
        next += 1;
    }
    if next < in_order.len() || pos != total {
        return Err(format!("{shown}: the image ended early"));
    }
    for e in again {
        writer.file(&e.path).map_err(|e| e.to_string())?;
        let mut h = Sha1::new();
        file.seek(SeekFrom::Start(e.offset)).map_err(|err| format!("{shown}: {err}"))?;
        let mut left = e.size;
        while left > 0 {
            let n = left.min(buf.len() as u64) as usize;
            file.read_exact(&mut buf[..n]).map_err(|err| format!("{shown}: {err}"))?;
            writer.write(&buf[..n]).map_err(|e| e.to_string())?;
            h.update(&buf[..n]);
            left -= n as u64;
            // The bar is already full here; this only lets a cancel through.
            if !progress(total, total) {
                return Err("cancelled".into());
            }
        }
        hashed.push((e.path.clone(), e.size, hex(&h.finalize())));
    }

    let image = digest.finish();
    // The boot file's title id, so a scan can match the archive by serial (`rom::serial`).
    let title_id = super::serial::image_title_id(iso).map(|id| format!("{id:08X}")).unwrap_or_default();
    let manifest = Manifest {
        format: 1,
        tool: ZAR_TOOL.into(),
        source: source(&image, total),
        titles: vec![ManifestTitle { folder: String::new(), title_id, version: 0, files: hashed.into_iter().map(|(path, size, sha1)| ManifestFile { path, size, sha1 }).collect() }],
    };
    let text = serde_json::to_string(&manifest).map_err(|e| e.to_string())?;
    writer.finish(Some(&text)).map_err(|e| e.to_string())?;
    Ok(ZarWritten { image, image_size: total, manifest })
}

/// Checks a `.zar` against its manifest by streaming it, as [`cemu_wrapper::verify`] does a
/// `.wua`.
pub fn verify_zar(zar: &Path, progress: impl FnMut(u64, u64) -> bool) -> cemu_wrapper::Result<cemu_wrapper::Verified> {
    use cemu_wrapper::Verified;
    let Some(manifest) = cemu_wrapper::manifest(zar)? else { return Ok(Verified::NoManifest) };
    let files = manifest.titles.iter().flat_map(|t| t.files.iter().map(move |f| (if t.folder.is_empty() { f.path.clone() } else { format!("{}/{}", t.folder, f.path) }, f)));
    match cemu_wrapper::check_files(files, |begin, data| cemu_wrapper::zar_stream(zar, begin, data), progress)? {
        Some((path, reason)) => Ok(Verified::Mismatch { manifest, path, reason }),
        None => Ok(Verified::Intact(manifest)),
    }
}

/// Each file's size and SHA-1, by path (upper-cased: XDVDFS names are case-insensitive).
/// `keep_going` is asked between chunks; false gives `Err("cancelled")`.
pub fn file_hashes(image: &Path, keep_going: &mut dyn FnMut() -> bool) -> Result<std::collections::BTreeMap<String, (u64, String)>, String> {
    let shown = image.display().to_string();
    let mut file = File::open(image).map_err(|e| format!("{shown}: {e}"))?;
    let volume = open_volume(&mut file).map_err(|e| format!("{shown}: {e}"))?.ok_or_else(|| format!("{shown}: not an Xbox disc image"))?;
    let mut out = std::collections::BTreeMap::new();
    let mut buf = vec![0u8; 1 << 20];
    for e in entries(&mut file, &volume).map_err(|e| format!("{shown}: {e}"))?.into_iter().filter(|e| !e.is_dir) {
        let mut h = Sha1::new();
        file.seek(SeekFrom::Start(e.offset)).map_err(|err| format!("{shown}: {err}"))?;
        let mut left = e.size;
        while left > 0 {
            let n = left.min(buf.len() as u64) as usize;
            file.read_exact(&mut buf[..n]).map_err(|err| format!("{shown}: {}: {err}", e.path))?;
            h.update(&buf[..n]);
            left -= n as u64;
            if !keep_going() {
                return Err("cancelled".into());
            }
        }
        out.insert(e.path.to_ascii_uppercase(), (e.size, hex(&h.finalize())));
    }
    Ok(out)
}

/// Whether `copy` holds the same files as `original`, byte for byte: `None` when it does, else
/// the first path that differs and how. `keep_going` is asked between chunks; false gives
/// `Err("cancelled")`.
pub fn compare_files(original: &Path, copy: &Path, mut keep_going: impl FnMut() -> bool) -> Result<Option<(String, String)>, String> {
    let a = file_hashes(original, &mut keep_going)?;
    let mut b = file_hashes(copy, &mut keep_going)?;
    for (path, (size, sha1)) in a {
        match b.remove(&path) {
            None => return Ok(Some((path, "missing".into()))),
            Some((s, _)) if s != size => return Ok(Some((path, format!("{s} bytes, the original has {size}")))),
            Some((_, h)) if h != sha1 => return Ok(Some((path, "different content".into()))),
            Some(_) => {}
        }
    }
    Ok(b.into_keys().next().map(|p| (p, "not in the original".into())))
}

// --- exiso: extract-xiso's rewrite ----------------------------------------------------------

const EXISO_ABI: c_int = 2;
const EXISO_CANCELLED: c_int = -5;
type ExisoProgressFn = unsafe extern "C" fn(u64, u64, *mut c_void) -> c_int;
type ExisoRewriteFn = unsafe extern "C" fn(*const c_char, *const c_char, Option<ExisoProgressFn>, *mut c_void, *mut c_char, usize) -> c_int;

struct Exiso {
    _lib: Library,
    rewrite: ExisoRewriteFn,
}

fn find_exiso() -> Option<PathBuf> {
    native_lib::find_app_root_library("exiso").or_else(|| native_lib::find_native_library(&native_lib::source_root().join("deps/extract-xiso/release"), "exiso"))
}

fn exiso() -> Option<&'static Exiso> {
    static API: OnceLock<Option<Exiso>> = OnceLock::new();
    API.get_or_init(|| {
        let path = find_exiso()?;
        unsafe {
            let lib = Library::new(&path).map_err(|e| log::warn!("exiso: {} would not load: {e}", path.display())).ok()?;
            let version = *lib.get::<unsafe extern "C" fn() -> c_int>(b"exiso_abi_version").ok()?;
            if version() != EXISO_ABI {
                log::warn!("exiso: {} is ABI version {}, this build needs {EXISO_ABI}; rebuild it (deps/extract-xiso/scripts/build.sh)", path.display(), version());
                return None;
            }
            let rewrite = *lib.get::<ExisoRewriteFn>(b"exiso_rewrite").ok()?;
            Some(Exiso { _lib: lib, rewrite })
        }
    })
    .as_ref()
}

/// Whether `.zar` can be written here (the `cemuwua` library is present).
pub fn zar_available() -> bool {
    cemu_wrapper::available()
}

/// Whether `.xiso` can be written here (the `exiso` library is present).
pub fn xiso_available() -> bool {
    exiso().is_some()
}

/// Rewrites `iso` as an XISO at `out` through extract-xiso. `progress(done, total)` gets bytes
/// written, on every write; returning false cancels, removes the output and gives
/// `Err("cancelled")`.
pub fn rewrite_xiso(iso: &Path, out: &Path, mut progress: impl FnMut(u64, u64) -> bool) -> Result<(), String> {
    let api = exiso().ok_or("the XISO library (exiso) is not built for this machine")?;
    let c = |p: &Path| CString::new(p.to_string_lossy().as_bytes()).map_err(|_| format!("{} has a NUL in its name", p.display()));
    let (input, output) = (c(iso)?, c(out)?);
    unsafe extern "C" fn on_progress(done: u64, total: u64, user: *mut c_void) -> c_int {
        let f = &mut *(user as *mut &mut dyn FnMut(u64, u64) -> bool);
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(done, total))).unwrap_or(false) as c_int
    }
    let mut f: &mut dyn FnMut(u64, u64) -> bool = &mut progress;
    let mut error = [0u8; 512];
    let code = unsafe { (api.rewrite)(input.as_ptr(), output.as_ptr(), Some(on_progress), &mut f as *mut _ as *mut c_void, error.as_mut_ptr() as *mut c_char, error.len()) };
    if code == 0 {
        return Ok(());
    }
    if code == EXISO_CANCELLED {
        return Err("cancelled".into());
    }
    let message = CStr::from_bytes_until_nul(&error).ok().map(|s| s.to_string_lossy().into_owned()).filter(|s| !s.is_empty()).unwrap_or_else(|| format!("exiso error {code}"));
    Err(format!("{}: {message}", iso.display()))
}
