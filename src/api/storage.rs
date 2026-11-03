//! `media_dirs` / `temp_dirs` configuration, and choosing a temp dir for an operation by
//! free space: tier 1 first, then tier 2, each required to keep a configurable margin
//! (`temp_dir_margin`) beyond what the operation needs, with in-process reservations so
//! several concurrent `Single`s don't all pick the same dir on the same
//! free-space reading.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::database::settings as db_settings;
use crate::database::storage as db_storage;
pub use crate::database::storage::{MediaDir, TempDir, MEDIA_ROLES, SINGLE_ROLES};

use super::{App, Error, Result};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TempMargin {
    #[serde(default = "TempMargin::default_percent")]
    pub percent: f64,
    #[serde(default = "TempMargin::default_min_bytes")]
    pub min_bytes: u64,
}

impl TempMargin {
    fn default_percent() -> f64 {
        10.0
    }
    fn default_min_bytes() -> u64 {
        1 << 30
    }
    /// The margin to keep for an operation needing `required` bytes: the larger of
    /// `percent` of it and `min_bytes`.
    pub fn bytes_for(&self, required: u64) -> u64 {
        ((required as f64) * self.percent / 100.0).ceil().max(self.min_bytes as f64) as u64
    }
}

impl Default for TempMargin {
    fn default() -> Self {
        Self { percent: Self::default_percent(), min_bytes: Self::default_min_bytes() }
    }
}

pub async fn margin(app: &App) -> Result<TempMargin> {
    Ok(db_settings::get::<TempMargin>(&app.pool, db_settings::KEY_TEMP_DIR_MARGIN).await?.unwrap_or_default())
}

/// Free bytes on the disk `path` lives on (longest matching mount point), `None` when no
/// known disk contains it.
pub fn available_bytes(path: &Path) -> Option<u64> {
    let target = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks.list().iter().filter(|d| target.starts_with(d.mount_point())).max_by_key(|d| d.mount_point().as_os_str().len()).map(|d| d.available_space())
}

fn reservations() -> &'static Mutex<HashMap<u64, u64>> {
    static RESERVATIONS: OnceLock<Mutex<HashMap<u64, u64>>> = OnceLock::new();
    RESERVATIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn reserved_bytes(temp_dir_id: u64) -> u64 {
    reservations().lock().map(|m| m.get(&temp_dir_id).copied().unwrap_or(0)).unwrap_or(0)
}

/// Bytes an in-flight operation has claimed on a temp dir; released on drop.
#[derive(Debug)]
pub struct TempReservation {
    temp_dir_id: u64,
    bytes: u64,
}

impl Drop for TempReservation {
    fn drop(&mut self) {
        if let Ok(mut m) = reservations().lock() {
            if let Some(v) = m.get_mut(&self.temp_dir_id) {
                *v = v.saturating_sub(self.bytes);
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TempChoice {
    pub dir: TempDir,
    pub available_bytes: u64,
    pub reserved_bytes: u64,
    pub required_bytes: u64,
    pub margin_bytes: u64,
}

/// Pure decision over already-measured candidates: the first (tier order) whose free
/// space, minus what's already reserved on it, covers `required` plus the margin.
pub fn choose(candidates: &[(TempDir, Option<u64>)], required: u64, margin: &TempMargin) -> Option<TempChoice> {
    let margin_bytes = margin.bytes_for(required);
    candidates.iter().find_map(|(dir, available)| {
        let available = (*available)?;
        let reserved = reserved_bytes(dir.id);
        let usable = available.saturating_sub(reserved);
        (usable >= required.saturating_add(margin_bytes)).then(|| TempChoice { dir: dir.clone(), available_bytes: available, reserved_bytes: reserved, required_bytes: required, margin_bytes })
    })
}

/// Picks the temp dir for an operation needing `required_bytes` (use
/// [`estimated_extracted_bytes`] for an archive). `None` when no configured temp dir
/// exists on disk with enough room.
pub async fn pick_temp_dir(app: &App, required_bytes: u64) -> Result<Option<TempChoice>> {
    let margin = margin(app).await?;
    let candidates: Vec<(TempDir, Option<u64>)> = db_storage::list_temp_dirs(&app.pool).await?.into_iter().filter(|t| Path::new(&t.path).is_dir()).map(|t| { let a = available_bytes(Path::new(&t.path)); (t, a) }).collect();
    Ok(choose(&candidates, required_bytes, &margin))
}

/// [`pick_temp_dir`] plus a reservation for the chosen dir, so the next concurrent pick
/// sees this operation's bytes as taken. Hold the reservation for the operation's life.
pub async fn reserve_temp_dir(app: &App, required_bytes: u64) -> Result<Option<(TempChoice, TempReservation)>> {
    let Some(choice) = pick_temp_dir(app, required_bytes).await? else { return Ok(None) };
    let claim = required_bytes.saturating_add(choice.margin_bytes);
    if let Ok(mut m) = reservations().lock() {
        *m.entry(choice.dir.id).or_insert(0) += claim;
    }
    let reservation = TempReservation { temp_dir_id: choice.dir.id, bytes: claim };
    Ok(Some((choice, reservation)))
}

/// How many bytes extracting/decoding `path` would need on disk: the summed member sizes
/// for a zip (read from its central directory), otherwise the file's own size times a
/// conservative factor for the compressed container formats this project handles, or
/// just its size for anything else.
pub fn estimated_extracted_bytes(path: &Path) -> u64 {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "zip" => zip_uncompressed_size(path).unwrap_or(size.saturating_mul(3)),
        "7z" | "chd" | "rvz" | "gcz" | "wia" | "cso" | "zar" => size.saturating_mul(3),
        _ => size,
    }
}

fn zip_uncompressed_size(path: &Path) -> Option<u64> {
    let file = std::fs::File::open(path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    let mut total = 0u64;
    for i in 0..archive.len() {
        let entry = archive.by_index_raw(i).ok()?;
        total = total.saturating_add(entry.size());
    }
    Some(total)
}

#[derive(Debug, Clone, Serialize)]
pub struct MediaDirView {
    #[serde(flatten)]
    pub dir: MediaDir,
    pub exists_on_disk: bool,
    /// Whether the server itself can write there ([`write_probe`]). A server run as a
    /// service has a user of its own, and a folder that user cannot write to takes every
    /// download as a warning in a log nobody reads.
    pub writable: bool,
}

/// Creates and removes an empty file in `dir`: the only honest test of "can this process
/// write here" -- mode bits do not know about ACLs, read-only mounts or a service sandbox.
fn write_probe(dir: &Path) -> std::io::Result<()> {
    let probe = dir.join(format!(".romexplorer-write-test-{}", std::process::id()));
    std::fs::write(&probe, b"")?;
    std::fs::remove_file(&probe)
}

#[derive(Debug, Clone, Serialize)]
pub struct TempDirView {
    #[serde(flatten)]
    pub dir: TempDir,
    pub exists_on_disk: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct StorageView {
    pub media_dirs: Vec<MediaDirView>,
    pub temp_dirs: Vec<TempDirView>,
    pub media_roles: Vec<&'static str>,
    pub single_roles: Vec<&'static str>,
    /// Where provider raw responses and downloaded media live when nothing else is set.
    pub default_cache_dir: String,
    pub default_media_dir: String,
    /// Where a new download of each kind is written right now: the first dir of that role,
    /// else the default media dir (`media_store::Roots::write_root`). The cache tiers are
    /// not in it -- they hold copies made when art is shown, never the art itself.
    pub download_dirs: Vec<DownloadDir>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DownloadDir {
    pub role: &'static str,
    pub path: String,
    /// The id of the `media_dirs` row it is; `None` when it is the default media dir.
    pub media_dir_id: Option<u64>,
}

fn check_path(path: &str) -> Result<String> {
    let p = path.trim().trim_end_matches(['/', '\\']);
    if p.is_empty() {
        return Err(Error::invalid("path is required"));
    }
    if !std::path::Path::new(p).is_absolute() {
        return Err(Error::invalid("path must be absolute"));
    }
    Ok(p.to_string())
}

pub async fn view(app: &App) -> Result<StorageView> {
    let media_dirs: Vec<MediaDirView> = db_storage::list_media_dirs(&app.pool)
        .await?
        .into_iter()
        .map(|d| {
            let exists_on_disk = std::path::Path::new(&d.path).is_dir();
            MediaDirView { exists_on_disk, writable: exists_on_disk && write_probe(Path::new(&d.path)).is_ok(), dir: d }
        })
        .collect();
    let default_media_dir = app.paths.media_dir().display().to_string();
    let download_dirs = ["image", "video", "audio"]
        .into_iter()
        .map(|role| match media_dirs.iter().find(|d| d.dir.role == role) {
            Some(d) => DownloadDir { role, path: d.dir.path.clone(), media_dir_id: Some(d.dir.id) },
            None => DownloadDir { role, path: default_media_dir.clone(), media_dir_id: None },
        })
        .collect();
    let temp_dirs = db_storage::list_temp_dirs(&app.pool).await?.into_iter().map(|d| TempDirView { exists_on_disk: std::path::Path::new(&d.path).is_dir(), dir: d }).collect();
    Ok(StorageView {
        media_dirs,
        temp_dirs,
        media_roles: MEDIA_ROLES.to_vec(),
        single_roles: SINGLE_ROLES.to_vec(),
        default_cache_dir: app.paths.provider_cache_dir().display().to_string(),
        default_media_dir,
        download_dirs,
    })
}

pub async fn add_media_dir(app: &App, role: &str, path: &str) -> Result<StorageView> {
    let path = check_path(path)?;
    if !MEDIA_ROLES.contains(&role) {
        return Err(Error::invalid(format!("role must be one of {}", MEDIA_ROLES.join("|"))));
    }
    // Refused now rather than found out later: every download into a folder the server
    // cannot write to fails, one warning each, and the job still ends "done".
    std::fs::create_dir_all(&path).and_then(|()| write_probe(Path::new(&path))).map_err(|e| {
        Error::invalid(format!("the server cannot write to {path}: {e}. A server run as a service has its own user -- give that user the folder (chown), or choose one it owns"))
    })?;
    db_storage::add_media_dir(&app.pool, role, &path).await?;
    view(app).await
}

pub async fn remove_media_dir(app: &App, id: u64) -> Result<StorageView> {
    if !db_storage::remove_media_dir(&app.pool, id).await? {
        return Err(Error::not_found(format!("no media dir {id}")));
    }
    view(app).await
}

pub async fn set_temp_dir(app: &App, tier: u8, path: &str) -> Result<StorageView> {
    let path = check_path(path)?;
    if !(1..=2).contains(&tier) {
        return Err(Error::invalid("tier must be 1 or 2"));
    }
    db_storage::set_temp_dir(&app.pool, tier, &path).await?;
    view(app).await
}

pub async fn remove_temp_dir(app: &App, id: u64) -> Result<StorageView> {
    if !db_storage::remove_temp_dir(&app.pool, id).await? {
        return Err(Error::not_found(format!("no temp dir {id}")));
    }
    view(app).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(id: u64, tier: u8) -> TempDir {
        TempDir { id, path: format!("/tmp/t{id}"), tier }
    }

    #[test]
    fn margin_is_the_larger_of_percent_and_floor() {
        let m = TempMargin { percent: 10.0, min_bytes: 1000 };
        assert_eq!(m.bytes_for(5_000), 1000);
        assert_eq!(m.bytes_for(50_000), 5000);
    }

    #[test]
    fn chooses_first_tier_with_room_and_honors_reservations() {
        let m = TempMargin { percent: 10.0, min_bytes: 100 };
        let candidates = vec![(dir(9001, 1), Some(1_000)), (dir(9002, 2), Some(100_000))];
        // 1_000 free on tier 1 can't hold 2_000 + margin -> tier 2.
        assert_eq!(choose(&candidates, 2_000, &m).map(|c| c.dir.id), Some(9002));
        // 500 + max(50, 100) = 600 <= 1_000 -> tier 1.
        assert_eq!(choose(&candidates, 500, &m).map(|c| c.dir.id), Some(9001));
        // A reservation on tier 1 pushes the same request to tier 2; releasing it restores.
        let _r = {
            reservations().lock().unwrap().insert(9001, 600);
            TempReservation { temp_dir_id: 9001, bytes: 600 }
        };
        assert_eq!(choose(&candidates, 500, &m).map(|c| c.dir.id), Some(9002));
        drop(_r);
        assert_eq!(choose(&candidates, 500, &m).map(|c| c.dir.id), Some(9001));
        // Nothing fits -> None; an unmeasurable dir is skipped.
        assert!(choose(&candidates, 1_000_000, &m).is_none());
        assert!(choose(&[(dir(9003, 1), None)], 1, &m).is_none());
    }
}
