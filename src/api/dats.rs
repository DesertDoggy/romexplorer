//! Reference DAT sources and what has been imported from each. Triggering a
//! download/import is a `DatSync` job (`api::jobs`); this is the read side -- plus the DAT
//! files themselves, as downloaded under `<dats_dir>/<source>/`: listed, handed out one at a
//! time or all at once, and taken back in, so a clean install can be given the files and
//! import them offline (`DatSyncRequest::offline`) instead of downloading everything again.

use std::path::{Path, PathBuf};

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::common::models::Platform;
use crate::database::dat_download::{self, Source};

use super::{App, Error, Result};

#[derive(Debug, Clone, Serialize)]
pub struct SourceInfo {
    pub name: &'static str,
    pub imports: u64,
    /// As the DATs declared them (summed over imports).
    pub games: u64,
    pub files: u64,
    /// What actually landed in the database tagged with this source.
    pub releases_in_db: u64,
    pub content_in_db: u64,
    pub titles_in_db: u64,
    pub last_imported_at: Option<NaiveDateTime>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportRow {
    pub id: u64,
    pub source: String,
    pub platform_id: u16,
    pub platform: String,
    pub file_name: String,
    pub dat_name: Option<String>,
    pub dat_version: Option<String>,
    pub game_count: u32,
    pub file_count: u32,
    pub imported_at: NaiveDateTime,
}

pub async fn sources(app: &App) -> Result<Vec<SourceInfo>> {
    let rows = sqlx::query(
        "SELECT s.name, COUNT(di.id) AS imports, CAST(COALESCE(SUM(di.game_count), 0) AS UNSIGNED) AS games, CAST(COALESCE(SUM(di.file_count), 0) AS UNSIGNED) AS files, MAX(di.imported_at) AS last \
         FROM sources s LEFT JOIN dat_imports di ON di.source_id = s.id GROUP BY s.id, s.name",
    )
    .fetch_all(&app.pool)
    .await?;
    let in_db = sqlx::query(
        "SELECT s.name, \
         (SELECT COUNT(*) FROM release_sources rs JOIN `release` r ON r.id = rs.release_id WHERE rs.source_id = s.id AND r.deleted = FALSE) AS releases_in_db, \
         (SELECT COUNT(*) FROM content c WHERE c.source_id = s.id AND c.deleted = FALSE) AS content_in_db, \
         (SELECT COUNT(DISTINCT tr.title_id) FROM release_sources rs2 JOIN title_releases tr ON tr.release_id = rs2.release_id JOIN `release` r2 ON r2.id = rs2.release_id WHERE rs2.source_id = s.id AND r2.deleted = FALSE) AS titles_in_db \
         FROM sources s",
    )
    .fetch_all(&app.pool)
    .await?;
    let mut out = Vec::new();
    for source in Source::ALL {
        let row = rows.iter().find(|r| r.try_get::<String, _>("name").map(|n| n == source.name()).unwrap_or(false));
        let db = in_db.iter().find(|r| r.try_get::<String, _>("name").map(|n| n == source.name()).unwrap_or(false));
        let count = |key: &str| db.and_then(|r| r.try_get::<i64, _>(key).ok()).unwrap_or(0) as u64;
        out.push(SourceInfo {
            name: source.name(),
            imports: row.and_then(|r| r.try_get::<i64, _>("imports").ok()).unwrap_or(0) as u64,
            games: row.and_then(|r| r.try_get::<u64, _>("games").ok()).unwrap_or(0),
            files: row.and_then(|r| r.try_get::<u64, _>("files").ok()).unwrap_or(0),
            releases_in_db: count("releases_in_db"),
            content_in_db: count("content_in_db"),
            titles_in_db: count("titles_in_db"),
            last_imported_at: row.and_then(|r| r.try_get("last").ok()),
        });
    }
    Ok(out)
}

pub async fn imports(app: &App, limit: u32) -> Result<Vec<ImportRow>> {
    let rows = sqlx::query(
        "SELECT di.id, s.name AS source, di.app_platform_id, di.file_name, di.dat_name, di.dat_version, di.game_count, di.file_count, di.imported_at \
         FROM dat_imports di JOIN sources s ON s.id = di.source_id ORDER BY di.imported_at DESC, di.id DESC LIMIT ?",
    )
    .bind(limit.clamp(1, 1000))
    .fetch_all(&app.pool)
    .await?;
    rows.iter()
        .map(|r| {
            let platform_id: u16 = r.try_get("app_platform_id")?;
            Ok(ImportRow {
                id: r.try_get("id")?,
                source: r.try_get("source")?,
                platform_id,
                platform: Platform::from_igdb_id(platform_id).map(|p| p.display_name().to_string()).unwrap_or_else(|| "unknown".to_string()),
                file_name: r.try_get("file_name")?,
                dat_name: r.try_get("dat_name")?,
                dat_version: r.try_get("dat_version")?,
                game_count: r.try_get("game_count")?,
                file_count: r.try_get("file_count")?,
                imported_at: r.try_get("imported_at")?,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// the downloaded files
// ---------------------------------------------------------------------------

/// One file under `<dats_dir>/<source>/`.
#[derive(Debug, Clone, Serialize)]
pub struct DatFile {
    pub source: &'static str,
    /// Relative to the source's folder, `/`-separated.
    pub name: String,
    pub bytes: u64,
    pub modified: Option<NaiveDateTime>,
    /// False when a newer copy of the same DAT is next to it (`dat_download::latest_versions`):
    /// an offline import skips it, and it is only worth keeping as history.
    pub latest: bool,
}

/// Every source's files, source by source in `Source::ALL` order.
pub async fn files(app: &App) -> Result<Vec<DatFile>> {
    let dir = app.paths.dats_dir();
    tokio::task::spawn_blocking(move || Source::ALL.into_iter().flat_map(|source| source_files(&dir, source)).collect()).await.map_err(|e| Error::internal(e.to_string()))
}

fn source_files(dats_dir: &Path, source: Source) -> Vec<DatFile> {
    let root = dats_dir.join(source.name());
    let all = dat_download::local_files(dats_dir, source);
    let latest = dat_download::latest_versions(&all);
    all.iter()
        .filter_map(|path| {
            let meta = std::fs::metadata(path).ok()?;
            let name = relative_name(&root, path)?;
            let modified = meta.modified().ok().map(|t| chrono::DateTime::<chrono::Local>::from(t).naive_local());
            Some(DatFile { source: source.name(), name, bytes: meta.len(), modified, latest: latest.contains(path) })
        })
        .collect()
}

fn relative_name(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn require_source(name: &str) -> Result<Source> {
    Source::from_name(name).ok_or_else(|| Error::invalid(format!("unknown DAT source {name:?}")))
}

/// The file `name` of `source` -- only ever one [`files`] lists, so no path a caller makes up
/// can reach outside the DAT folder.
pub async fn file(app: &App, source: &str, name: &str) -> Result<PathBuf> {
    let source = require_source(source)?;
    let dats_dir = app.paths.dats_dir();
    let root = dats_dir.join(source.name());
    dat_download::local_files(&dats_dir, source).into_iter().find(|p| relative_name(&root, p).as_deref() == Some(name)).ok_or_else(|| Error::not_found(format!("{}: no DAT file {name:?}", source.name())))
}

/// What a bundle of `source` (or of every source) is called when handed out.
pub fn bundle_name(source: Option<&str>) -> String {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    match source {
        Some(s) => format!("romexplorer-dats-{s}-{stamp}.zip"),
        None => format!("romexplorer-dats-{stamp}.zip"),
    }
}

/// Every downloaded file of `source` (or of all of them) in one zip at `out`, as
/// `<source>/<name>` -- the layout [`accept_upload`] recognises and unpacks. Returns how many
/// files went in.
///
/// Stored, not compressed: nearly all of it is already zip (No-Intro, TOSEC and every redump
/// DAT arrive zipped, and those are the bulk by far), and recompressing compressed data
/// costs time for nothing (see CLAUDE.md). The loose `.dat`s from GitHub would shrink, but
/// they are a few MB beside packs of a hundred.
pub async fn write_bundle(app: &App, source: Option<&str>, out: &Path) -> Result<usize> {
    use crate::rom::sevenzip_wrapper::{create_archive, ArchiveFormat, CompressionMethod, CreateArchiveArgs, CreateEntry};
    let sources = match source {
        Some(name) => vec![require_source(name)?],
        None => Source::ALL.to_vec(),
    };
    let dats_dir = app.paths.dats_dir();
    let mut entries = Vec::new();
    for source in sources {
        let root = dats_dir.join(source.name());
        for path in dat_download::local_files(&dats_dir, source) {
            if let Some(name) = relative_name(&root, &path) {
                entries.push(CreateEntry { archive_name: format!("{}/{name}", source.name()), input_path: path });
            }
        }
    }
    if entries.is_empty() {
        return Err(Error::not_found(match source {
            Some(s) => format!("{s}: no downloaded DAT files"),
            None => "no downloaded DAT files".to_string(),
        }));
    }
    let count = entries.len();
    let out = out.to_path_buf();
    let _ = std::fs::remove_file(&out);
    let args = CreateArchiveArgs { format: ArchiveFormat::Zip, method: CompressionMethod::Copy, level: 0, entries };
    let written = tokio::task::spawn_blocking(move || create_archive(&out, &args, |_, _| true).map_err(|e| (out, e))).await.map_err(|e| Error::internal(e.to_string()))?;
    if let Err((out, e)) = written {
        let _ = std::fs::remove_file(&out);
        return Err(Error::internal(format!("writing {}: {e}", out.display())));
    }
    Ok(count)
}

/// Scratch space for bundles being handed out and uploads being received: beside the source
/// folders, so moving a received file into place is a rename, and outside them, so nothing
/// half-written is ever listed or imported. A day-old leftover is a run that died; it is cleared.
fn scratch_dir(app: &App) -> Result<PathBuf> {
    let dir = app.paths.dats_dir().join(".incoming");
    std::fs::create_dir_all(&dir).map_err(|e| Error::internal(format!("creating {}: {e}", dir.display())))?;
    Ok(dir)
}

fn scratch_path(app: &App, what: &str) -> Result<PathBuf> {
    let dir = scratch_dir(app)?;
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // Leftovers from a run that died mid-way (a day old: nothing in flight is).
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() > 86_400) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    Ok(dir.join(format!("{what}-{}-{n}.part", std::process::id())))
}

/// A bundle written to scratch space, for a download to stream and then delete.
pub async fn bundle_temp(app: &App, source: Option<&str>) -> Result<PathBuf> {
    let path = scratch_path(app, "bundle")?;
    write_bundle(app, source, &path).await?;
    Ok(path)
}

/// Where a REST upload is received before [`accept_upload`] looks at it.
pub fn upload_part_path(app: &App) -> Result<PathBuf> {
    scratch_path(app, "upload")
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExportRequest {
    /// A folder on this machine.
    pub dir: String,
    /// One source's files only; absent for all.
    #[serde(default)]
    pub source: Option<String>,
    /// One file of `source` copied as is, instead of a bundle.
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Exported {
    pub path: String,
    pub files: usize,
}

/// Copies one file, or writes a bundle, into a folder on this machine -- the desktop app's
/// "download", where there is no browser to save a response. The folder must exist; a file of
/// the same name there is not overwritten.
pub async fn export(app: &App, req: &ExportRequest) -> Result<Exported> {
    let dir = PathBuf::from(req.dir.trim());
    if !dir.is_dir() {
        return Err(Error::invalid(format!("{}: not a folder", dir.display())));
    }
    let free = |name: &str| {
        let wanted = dir.join(name);
        if wanted.exists() {
            Err(Error::conflict(format!("{} already exists", wanted.display())))
        } else {
            Ok(wanted)
        }
    };
    match (&req.source, &req.name) {
        (Some(source), Some(name)) => {
            let from = file(app, source, name).await?;
            let to = free(&from.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())?;
            let (f, t) = (from.clone(), to.clone());
            tokio::task::spawn_blocking(move || std::fs::copy(&f, &t)).await.map_err(|e| Error::internal(e.to_string()))?.map_err(|e| Error::internal(format!("copying to {}: {e}", to.display())))?;
            Ok(Exported { path: to.display().to_string(), files: 1 })
        }
        (None, Some(_)) => Err(Error::invalid("a file name needs its source")),
        (source, None) => {
            let to = free(&bundle_name(source.as_deref()))?;
            let files = write_bundle(app, source.as_deref(), &to).await?;
            Ok(Exported { path: to.display().to_string(), files })
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Uploaded {
    /// Whether the upload was a bundle ([`write_bundle`]'s layout) and was unpacked.
    pub bundle: bool,
    /// What was stored, as [`files`] now lists it.
    pub files: Vec<DatFile>,
}

const DAT_EXTENSIONS: [&str; 4] = ["dat", "xml", "zip", "json"];

fn has_dat_extension(name: &str) -> bool {
    Path::new(name).extension().and_then(|e| e.to_str()).is_some_and(|e| DAT_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// `name` split into safe path components, or `None` for anything that could leave the folder
/// it is joined to.
fn safe_components(name: &str) -> Option<Vec<&str>> {
    let parts: Vec<&str> = name.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    (!parts.is_empty() && parts.iter().all(|p| *p != "." && *p != ".." && !p.contains(':') && !p.contains('\0'))).then_some(parts)
}

/// The member names of `path` when it is a bundle: a zip whose every file sits under a
/// known source's folder. `None` for anything else, a DAT pack zip included.
fn bundle_members(path: &Path) -> Option<Vec<(Source, String, String)>> {
    let archive = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
    let mut out = Vec::new();
    for member in archive.file_names() {
        if member.ends_with('/') {
            continue;
        }
        let (first, rest) = member.split_once('/')?;
        let source = Source::from_name(first)?;
        out.push((source, rest.to_string(), member.to_string()));
    }
    (!out.is_empty()).then_some(out)
}

/// Takes a received upload (`part`) into the DAT folder. A bundle is unpacked into each
/// source's folder; anything else is one DAT file (`.dat`, `.xml`, `.json`, or a `.zip` pack)
/// for `source`, stored under its own name. A file already there under the same name is
/// replaced -- it is the same DAT, and the upload is what was asked for. Nothing is imported:
/// that is a `dat_sync` with `offline`, so it shows on the jobs list like any other import.
pub async fn accept_upload(app: &App, part: &Path, original: &str, source: Option<&str>) -> Result<Uploaded> {
    let dats_dir = app.paths.dats_dir();
    let part_owned = part.to_path_buf();
    let original = original.to_string();
    let source = source.map(str::trim).filter(|s| !s.is_empty()).map(require_source).transpose();
    let into = dats_dir.clone();
    let outcome = tokio::task::spawn_blocking(move || -> Result<(bool, Vec<(Source, PathBuf)>)> {
        let dats_dir = into;
        if let Some(members) = bundle_members(&part_owned) {
            let mut archive = zip::ZipArchive::new(std::fs::File::open(&part_owned).map_err(|e| Error::internal(e.to_string()))?).map_err(|e| Error::invalid(format!("{original}: {e}")))?;
            let mut stored = Vec::new();
            for (source, rest, member) in members {
                let Some(parts) = safe_components(&rest).filter(|_| has_dat_extension(&rest)) else {
                    log::warn!("DAT bundle {original}: skipped {member}");
                    continue;
                };
                let mut target = dats_dir.join(source.name());
                for p in parts {
                    target.push(p);
                }
                let mut entry = archive.by_name(&member).map_err(|e| Error::invalid(format!("{original}: {member}: {e}")))?;
                store(&mut entry, &target).map_err(|e| Error::internal(format!("{}: {e}", target.display())))?;
                stored.push((source, target));
            }
            if stored.is_empty() {
                return Err(Error::invalid(format!("{original}: the bundle held no DAT files")));
            }
            let _ = std::fs::remove_file(&part_owned);
            return Ok((true, stored));
        }
        let source = source?.ok_or_else(|| Error::invalid(format!("{original} is a single DAT file: choose the source it belongs to")))?;
        let name = Path::new(&original).file_name().and_then(|n| n.to_str()).map(str::to_string).unwrap_or_default();
        if !has_dat_extension(&name) || safe_components(&name).is_none() {
            return Err(Error::invalid(format!("{original}: not a DAT file (.dat, .xml, .json or a .zip pack)")));
        }
        if name.to_ascii_lowercase().ends_with(".zip") {
            zip::ZipArchive::new(std::fs::File::open(&part_owned).map_err(|e| Error::internal(e.to_string()))?).map_err(|e| Error::invalid(format!("{original}: not a readable zip: {e}")))?;
        }
        let folder = dats_dir.join(source.name());
        std::fs::create_dir_all(&folder).map_err(|e| Error::internal(format!("creating {}: {e}", folder.display())))?;
        let target = folder.join(&name);
        std::fs::rename(&part_owned, &target).map_err(|e| Error::internal(format!("storing {}: {e}", target.display())))?;
        Ok((false, vec![(source, target)]))
    })
    .await
    .map_err(|e| Error::internal(e.to_string()))?;
    let (bundle, stored) = match outcome {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_file(part);
            return Err(e);
        }
    };
    let mut files = Vec::new();
    for source in Source::ALL {
        if stored.iter().any(|(s, _)| *s == source) {
            let root = dats_dir.join(source.name());
            files.extend(source_files(&dats_dir, source).into_iter().filter(|f| stored.iter().any(|(s, p)| *s == source && relative_name(&root, p).as_deref() == Some(f.name.as_str()))));
        }
    }
    Ok(Uploaded { bundle, files })
}

/// Writes `from` to `target` through a `.part` beside it, so an interrupted write never
/// leaves a truncated DAT where an import would read it.
fn store(from: &mut impl std::io::Read, target: &Path) -> std::io::Result<()> {
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = target.with_extension(format!("{}.part", target.extension().and_then(|e| e.to_str()).unwrap_or("")));
    let written = (|| {
        let mut out = std::fs::File::create(&tmp)?;
        std::io::copy(from, &mut out)?;
        out.sync_all()?;
        std::fs::rename(&tmp, target)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_names_cannot_leave_the_folder() {
        assert_eq!(safe_components("a/b.dat"), Some(vec!["a", "b.dat"]));
        assert_eq!(safe_components("../x.dat"), None);
        assert_eq!(safe_components("a/../../x.dat"), None);
        assert_eq!(safe_components("C:/x.dat"), None);
        assert_eq!(safe_components("a\\..\\x.dat"), None);
        assert_eq!(safe_components("/abs/x.dat"), Some(vec!["abs", "x.dat"]), "rooted names land inside, not at /");
        assert!(has_dat_extension("No-Intro Love Pack (2026-10-01).ZIP"));
        assert!(!has_dat_extension("setup.exe"));
    }

    #[test]
    fn a_bundle_is_recognised_by_its_source_folders() {
        let dir = std::env::temp_dir().join(format!("rx_dat_bundle_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, members: &[&str]| {
            let path = dir.join(name);
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            for m in members {
                zip.start_file(*m, zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();
                std::io::Write::write_all(&mut zip, b"x").unwrap();
            }
            zip.finish().unwrap();
            path
        };
        let bundle = write("b.zip", &["no-intro/Pack (2026-10-01).zip", "redump/Sony - PlayStation.zip"]);
        let members = bundle_members(&bundle).unwrap();
        assert_eq!(members.iter().map(|(s, r, _)| (s.name(), r.as_str())).collect::<Vec<_>>(), vec![("no-intro", "Pack (2026-10-01).zip"), ("redump", "Sony - PlayStation.zip")]);
        assert!(bundle_members(&write("p.zip", &["Nintendo - Game Boy (2026).dat"])).is_none(), "a DAT pack is not a bundle");
        assert!(bundle_members(&write("m.zip", &["no-intro/a.dat", "other/b.dat"])).is_none(), "every member must be under a source");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
