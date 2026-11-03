//! Choosing directories on the server's filesystem, for every path field a user fills in
//! (collection directories, media/temp dirs). Two ways, because a browser can't hand an
//! absolute path back from its own OS picker (`showDirectoryPicker`/`webkitdirectory` only
//! ever expose names, by design):
//!
//! - [`pick_dir_native`]: the OS's own folder dialog (Explorer / Finder / the xdg desktop
//!   portal), opened by *this* process -- so it appears on the server machine's screen and is
//!   only useful when the user sits there too. Desktop targets only; rfd has no Android/iOS
//!   backend.
//! - [`list_dir`]: one directory level at a time (roots first), for an in-app folder browser
//!   that works from anywhere -- a phone's browser, a remote client, a headless server, and
//!   the Android/iOS builds.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use super::{Error, Result};

#[derive(Debug, Clone, Serialize)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DirListing {
    /// The listed directory, or `None` for the roots view.
    pub path: Option<String>,
    /// One level up; `None` at a filesystem root (go back to the roots view from there).
    pub parent: Option<String>,
    /// Subdirectories of `path` (or the roots), sorted case-insensitively.
    pub entries: Vec<DirEntry>,
    /// Drive letters (Windows) or `/`, plus the user's home -- always filled, for a sidebar.
    pub roots: Vec<DirEntry>,
    pub separator: String,
}

/// Lists `path`'s subdirectories, or the roots when `path` is `None`/empty. Files are left
/// out: every caller is choosing a directory.
pub async fn list_dir(path: Option<&str>) -> Result<DirListing> {
    let path = path.map(str::trim).filter(|p| !p.is_empty()).map(PathBuf::from);
    tokio::task::spawn_blocking(move || list_dir_blocking(path.as_deref())).await.map_err(|e| Error::internal(e.to_string()))?
}

fn list_dir_blocking(path: Option<&Path>) -> Result<DirListing> {
    let roots = roots();
    let separator = std::path::MAIN_SEPARATOR_STR.to_string();
    let Some(path) = path else {
        return Ok(DirListing { path: None, parent: None, entries: roots.clone(), roots, separator });
    };
    if !path.is_absolute() {
        return Err(Error::invalid(format!("path must be absolute: {}", path.display())));
    }
    let read = std::fs::read_dir(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => Error::not_found(format!("no such directory: {}", path.display())),
        _ => Error::invalid(format!("can't read {}: {e}", path.display())),
    })?;
    let mut entries: Vec<DirEntry> = read
        .flatten()
        // `Path::is_dir` follows symlinks/junctions, so a linked directory still shows up.
        .filter(|e| e.path().is_dir())
        .map(|e| DirEntry { name: e.file_name().to_string_lossy().into_owned(), path: e.path().display().to_string() })
        .collect();
    entries.sort_by_key(|e| e.name.to_lowercase());
    Ok(DirListing {
        path: Some(path.display().to_string()),
        parent: path.parent().map(|p| p.display().to_string()),
        entries,
        roots,
        separator,
    })
}

fn roots() -> Vec<DirEntry> {
    let mut out = Vec::new();
    if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).filter(|p| p.is_dir()) {
        out.push(DirEntry { name: "Home".to_string(), path: home.display().to_string() });
    }
    if cfg!(windows) {
        // Probing the letters (rather than asking sysinfo for disks) also finds mapped
        // network drives. A:/B: are skipped: probing an empty floppy drive can stall.
        for letter in b'C'..=b'Z' {
            let root = format!("{}:\\", letter as char);
            if Path::new(&root).is_dir() {
                out.push(DirEntry { name: root[..2].to_string(), path: root });
            }
        }
    } else {
        out.push(DirEntry { name: "/".to_string(), path: "/".to_string() });
    }
    out
}

/// Whether this build can show an OS folder dialog at all ([`pick_dir_native`]).
pub fn native_picker_available() -> bool {
    cfg!(any(windows, target_os = "macos", target_os = "linux"))
}

/// Shows the OS folder dialog on the server machine's screen, starting in `start` when
/// that's an existing directory. `Ok(None)` when the user cancels. One dialog at a time
/// (a second request while one is open is a `Conflict`, also after the request gave up on
/// it), and the request gives up after 10 minutes.
///
/// On Linux, a missing xdg desktop portal (no desktop session) reads as a cancel -- rfd
/// can't tell the two apart -- which is why the console always keeps [`list_dir`] as a way
/// out.
pub async fn pick_dir_native(start: Option<&str>) -> Result<Option<String>> {
    pick_native(start, false).await
}

/// Like [`pick_dir_native`] for a file (an emulator executable, AppImage, `.app`).
pub async fn pick_file_native(start: Option<&str>) -> Result<Option<String>> {
    pick_native(start, true).await
}

async fn pick_native(start: Option<&str>, file: bool) -> Result<Option<String>> {
    #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
    {
        static OPEN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let Ok(guard) = OPEN.try_lock() else {
            return Err(Error::conflict("a folder dialog is already open on the server"));
        };
        let start = start.map(PathBuf::from).filter(|p| p.is_dir());
        let handle = tokio::runtime::Handle::current();
        // The dialog future isn't `Send` on every platform, so it's driven on its own
        // blocking thread rather than inside the (Send-requiring) request handler.
        let task = tokio::task::spawn_blocking(move || {
            // Held by the dialog's own thread, so a dialog left open past the timeout below
            // still counts as open and no second one stacks on top of it.
            let _guard = guard;
            let mut dialog = rfd::AsyncFileDialog::new().set_title(if file { "Choose a file" } else { "Choose a folder" });
            if let Some(start) = &start {
                dialog = dialog.set_directory(start);
            }
            if file {
                handle.block_on(dialog.pick_file()).map(|f| f.path().display().to_string())
            } else {
                handle.block_on(dialog.pick_folder()).map(|folder| folder.path().display().to_string())
            }
        });
        match tokio::time::timeout(Duration::from_secs(600), task).await {
            Ok(joined) => joined.map_err(|e| Error::internal(e.to_string())),
            Err(_) => Err(Error::unavailable("folder dialog timed out")),
        }
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        let _ = (start, file, Duration::ZERO);
        Err(Error::unavailable("no native folder dialog on this platform"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn roots_view_lists_at_least_one_root() {
        let listing = list_dir(None).await.unwrap();
        assert!(listing.path.is_none());
        assert!(!listing.roots.is_empty());
        assert_eq!(listing.entries.len(), listing.roots.len());
    }

    #[tokio::test]
    async fn lists_only_subdirectories_with_a_parent() {
        let dir = std::env::temp_dir().join(format!("romexplorer_fs_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Beta")).unwrap();
        std::fs::create_dir_all(dir.join("alpha")).unwrap();
        std::fs::write(dir.join("file.txt"), b"x").unwrap();
        let listing = list_dir(Some(&dir.display().to_string())).await.unwrap();
        let names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["alpha", "Beta"]);
        assert_eq!(listing.parent.as_deref(), dir.parent().map(|p| p.display().to_string()).as_deref());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn relative_and_missing_paths_are_errors() {
        assert_eq!(list_dir(Some("relative/dir")).await.unwrap_err().kind, super::super::ErrorKind::Invalid);
        let missing = std::env::temp_dir().join("romexplorer_fs_test_definitely_missing");
        assert_eq!(list_dir(Some(&missing.display().to_string())).await.unwrap_err().kind, super::super::ErrorKind::NotFound);
    }
}
