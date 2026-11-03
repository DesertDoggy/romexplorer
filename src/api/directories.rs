//! Collection roots and configured subdirectories, with inheritance resolved and a few
//! on-disk facts a UI wants next to each row.
//!
//! The only writer of `local_directories`, so the hierarchy rules live here:
//! - any number of collection roots, but never one inside another;
//! - every other directory lies inside a root (so none can be added before a root exists),
//!   and its parent is the nearest configured directory containing it -- derived from the
//!   path, never chosen;
//! - removing a directory with configured subdirectories removes those too, and only
//!   when asked for explicitly (`recursive`).
//!
//! Removal only ever soft-deletes configuration rows. Nothing here creates, moves or
//! deletes anything on disk.

use std::collections::HashMap;

use serde::Serialize;
use sqlx::Row;

use crate::common::models::Platform;
use crate::database::directories::{self as db_dirs, canonical_path, path_strictly_inside, same_path};
pub use crate::database::directories::{Directory, DirectorySpec, Effective, DIR_TYPES};

use super::{App, Error, Result};

/// A configured directory below another one.
#[derive(Debug, Clone, Serialize)]
pub struct DescendantRef {
    pub id: u64,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DirectoryView {
    #[serde(flatten)]
    pub directory: Directory,
    pub platform_name: Option<String>,
    pub effective_dir_type: Option<String>,
    pub effective_platform_id: Option<u16>,
    pub effective_platform_name: Option<String>,
    pub exists_on_disk: bool,
    pub file_count: u64,
    /// Every configured directory below this one -- what a recursive removal also removes.
    pub descendants: Vec<DescendantRef>,
}

fn platform_name(id: Option<u16>) -> Option<String> {
    id.and_then(Platform::from_igdb_id).map(|p| p.display_name().to_string())
}

async fn file_counts(app: &App) -> Result<HashMap<u64, u64>> {
    let rows = sqlx::query("SELECT directory_id, COUNT(*) AS n FROM local_files WHERE deleted = FALSE AND directory_id IS NOT NULL GROUP BY directory_id").fetch_all(&app.pool).await?;
    let mut out = HashMap::new();
    for row in rows {
        let id: u64 = row.try_get("directory_id")?;
        let n: i64 = row.try_get("n")?;
        out.insert(id, n as u64);
    }
    Ok(out)
}

async fn view(app: &App, directory: Directory, all: &[Directory], counts: &HashMap<u64, u64>) -> Result<DirectoryView> {
    let effective = db_dirs::resolve(&app.pool, &directory.path).await?;
    Ok(DirectoryView {
        exists_on_disk: std::path::Path::new(&directory.path).is_dir(),
        platform_name: platform_name(directory.app_platform_id),
        effective_dir_type: effective.dir_type,
        effective_platform_id: effective.app_platform_id,
        effective_platform_name: platform_name(effective.app_platform_id),
        file_count: counts.get(&directory.id).copied().unwrap_or(0),
        descendants: db_dirs::descendants(all, &directory.path).map(|d| DescendantRef { id: d.id, path: d.path.clone() }).collect(),
        directory,
    })
}

/// In tree order: each directory under its parent, siblings by priority (then path).
pub async fn list(app: &App) -> Result<Vec<DirectoryView>> {
    let mut all = db_dirs::list(&app.pool, false).await?;
    let keys = db_dirs::priority_keys(&app.pool).await?;
    all.sort_by(|a, b| (keys.get(&a.id), &a.path).cmp(&(keys.get(&b.id), &b.path)));
    let counts = file_counts(app).await?;
    let mut out = Vec::with_capacity(all.len());
    for d in &all {
        out.push(view(app, d.clone(), &all, &counts).await?);
    }
    Ok(out)
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct MoveRequest {
    /// up | down
    pub direction: String,
}

/// Moves a directory one place among its siblings. Returns the list as it is now.
pub async fn move_directory(app: &App, id: u64, req: &MoveRequest) -> Result<Vec<DirectoryView>> {
    let up = match req.direction.as_str() {
        "up" => true,
        "down" => false,
        other => return Err(Error::invalid(format!("direction {other:?} must be up or down"))),
    };
    db_dirs::move_directory(&app.pool, id, up).await?;
    list(app).await
}

pub async fn get(app: &App, id: u64) -> Result<DirectoryView> {
    let d = db_dirs::get(&app.pool, id).await?.ok_or_else(|| Error::not_found(format!("no directory {id}")))?;
    let all = db_dirs::list(&app.pool, false).await?;
    view(app, d, &all, &file_counts(app).await?).await
}

/// Checks `spec` (already canonical) against the other non-deleted rows. `self_id` is the
/// row being updated, if any.
fn validate(spec: &DirectorySpec, self_id: Option<u64>, all: &[Directory]) -> Result<()> {
    let path = spec.path.as_str();
    if path.is_empty() {
        return Err(Error::invalid("path is required"));
    }
    if !std::path::Path::new(path).is_absolute() {
        return Err(Error::invalid("path must be absolute"));
    }
    if let Some(t) = spec.dir_type.as_deref() {
        if !DIR_TYPES.contains(&t) {
            return Err(Error::invalid(format!("dir_type must be one of {}", DIR_TYPES.join("|"))));
        }
    }
    if let Some(p) = spec.app_platform_id {
        if Platform::from_igdb_id(p).is_none() {
            return Err(Error::invalid(format!("unknown platform id {p}")));
        }
    }

    let others: Vec<&Directory> = all.iter().filter(|d| !d.deleted && Some(d.id) != self_id).collect();
    if let Some(dup) = others.iter().find(|d| same_path(&d.path, path)) {
        return Err(Error::conflict(format!("{} is already configured (directory {})", dup.path, dup.id)));
    }
    let roots: Vec<&Directory> = others.iter().copied().filter(|d| d.is_collection_root).collect();

    if spec.is_collection_root {
        if let Some(outer) = roots.iter().find(|r| path_strictly_inside(&r.path, path)) {
            return Err(Error::invalid(format!("{path} is inside collection root {} -- roots can't be nested; add it as a subdirectory instead", outer.path)));
        }
        if let Some(inner) = roots.iter().find(|r| path_strictly_inside(path, &r.path)) {
            return Err(Error::invalid(format!("collection root {} is inside {path} -- roots can't be nested", inner.path)));
        }
    } else {
        if roots.is_empty() {
            return Err(Error::invalid("add a collection root first -- other directories can only be configured inside one"));
        }
        if !roots.iter().any(|r| path_strictly_inside(&r.path, path)) {
            let list: Vec<&str> = roots.iter().map(|r| r.path.as_str()).collect();
            return Err(Error::invalid(format!("{path} must be inside a collection root ({})", list.join(", "))));
        }
    }

    // An update must not strand the row's configured subdirectories outside every root
    // (unticking "collection root", or moving a root elsewhere).
    if let Some(id) = self_id {
        if let Some(old) = all.iter().find(|d| d.id == id) {
            let mut new_roots: Vec<&str> = roots.iter().map(|r| r.path.as_str()).collect();
            if spec.is_collection_root {
                new_roots.push(path);
            }
            let stranded: Vec<&str> = db_dirs::descendants(all, &old.path)
                .filter(|d| d.id != id && !new_roots.iter().any(|r| path_strictly_inside(r, &d.path)))
                .map(|d| d.path.as_str())
                .collect();
            if !stranded.is_empty() {
                return Err(Error::invalid(format!("this would leave configured subdirectories outside every collection root: {} -- remove them first", stranded.join(", "))));
            }
        }
    }
    Ok(())
}

fn normalized(spec: &DirectorySpec) -> DirectorySpec {
    let mut s = spec.clone();
    s.path = canonical_path(&s.path);
    s.dir_type = s.dir_type.filter(|t| !t.is_empty());
    s
}

/// Adds a directory, or updates the existing row when the path is already configured.
pub async fn create(app: &App, spec: &DirectorySpec) -> Result<DirectoryView> {
    let spec = normalized(spec);
    let all = db_dirs::list(&app.pool, false).await?;
    // An already-configured path is an update of that row, and checked as one.
    let existing = all.iter().find(|d| same_path(&d.path, &spec.path)).map(|d| d.id);
    validate(&spec, existing, &all)?;
    let parent = if spec.is_collection_root { None } else { db_dirs::derive_parent(&all, &spec.path, existing) };
    let id = db_dirs::upsert(&app.pool, &spec, parent).await?;
    db_dirs::relink_parents(&app.pool).await?;
    get(app, id).await
}

pub async fn update(app: &App, id: u64, spec: &DirectorySpec) -> Result<DirectoryView> {
    let spec = normalized(spec);
    if db_dirs::get(&app.pool, id).await?.is_none() {
        return Err(Error::not_found(format!("no directory {id}")));
    }
    let all = db_dirs::list(&app.pool, false).await?;
    validate(&spec, Some(id), &all)?;
    let parent = if spec.is_collection_root { None } else { db_dirs::derive_parent(&all, &spec.path, Some(id)) };
    db_dirs::update(&app.pool, id, &spec, parent).await?;
    db_dirs::relink_parents(&app.pool).await?;
    get(app, id).await
}

/// Removes a directory's configuration. With configured subdirectories below it, that's
/// a `Conflict` unless `recursive`, which removes them too. Returns every removed id.
///
/// Configuration only: the rows are soft-deleted, nothing on disk is touched.
pub async fn delete(app: &App, id: u64, recursive: bool) -> Result<Vec<u64>> {
    let all = db_dirs::list(&app.pool, false).await?;
    let Some(target) = all.iter().find(|d| d.id == id) else {
        return Err(Error::not_found(format!("no directory {id}")));
    };
    let below: Vec<u64> = db_dirs::descendants(&all, &target.path).map(|d| d.id).collect();
    if !below.is_empty() && !recursive {
        return Err(Error::conflict(format!("{} has {} configured subdirector{} -- remove recursively to remove them too", target.path, below.len(), if below.len() == 1 { "y" } else { "ies" })));
    }
    let mut ids = vec![id];
    ids.extend(below);
    db_dirs::soft_delete_many(&app.pool, &ids).await?;
    db_dirs::relink_parents(&app.pool).await?;
    Ok(ids)
}

pub async fn resolve(app: &App, path: &str) -> Result<Effective> {
    Ok(db_dirs::resolve(&app.pool, path).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(id: u64, path: &str, root: bool) -> Directory {
        Directory { id, parent_directory_id: None, path: path.to_string(), is_collection_root: root, dir_type: None, app_platform_id: None, is_archival: false, deleted: false, priority: 0 }
    }

    fn spec(path: &str, root: bool) -> DirectorySpec {
        normalized(&DirectorySpec { path: path.to_string(), is_collection_root: root, ..Default::default() })
    }

    // Absolute on every OS the tests run on.
    fn p(s: &str) -> String {
        if cfg!(windows) { format!("C:{s}") } else { s.to_string() }
    }

    fn kind(r: Result<()>) -> Option<super::super::ErrorKind> {
        r.err().map(|e| e.kind)
    }

    #[test]
    fn nothing_but_a_root_before_a_root_exists() {
        assert!(validate(&spec(&p("/roms"), true), None, &[]).is_ok());
        assert_eq!(kind(validate(&spec(&p("/roms/snes"), false), None, &[])), Some(super::super::ErrorKind::Invalid));
    }

    #[test]
    fn subdirectories_only_inside_a_root_and_roots_never_nested() {
        let all = vec![dir(1, &p("/redump"), true), dir(2, &p("/nointro"), true)];
        assert!(validate(&spec(&p("/redump/ps1"), false), None, &all).is_ok());
        assert!(validate(&spec(&p("/nointro/snes/hacks"), false), None, &all).is_ok());
        assert!(validate(&spec(&p("/tosec"), true), None, &all).is_ok(), "any number of separate roots");
        let invalid = Some(super::super::ErrorKind::Invalid);
        assert_eq!(kind(validate(&spec(&p("/elsewhere/x"), false), None, &all)), invalid);
        assert_eq!(kind(validate(&spec(&p("/redump"), false), Some(1), &all)), invalid, "a root's own path isn't inside it");
        assert_eq!(kind(validate(&spec(&p("/redump/ps1"), true), None, &all)), invalid);
        assert_eq!(kind(validate(&spec(&p("/"), true), None, &all)), invalid);
    }

    #[test]
    fn an_update_cant_strand_subdirectories() {
        let all = vec![dir(1, &p("/r"), true), dir(2, &p("/r/a"), false)];
        let invalid = Some(super::super::ErrorKind::Invalid);
        assert_eq!(kind(validate(&spec(&p("/r"), false), Some(1), &all)), invalid, "unticking root");
        assert_eq!(kind(validate(&spec(&p("/moved"), true), Some(1), &all)), invalid, "moving the root away");
        assert!(validate(&spec(&p("/r"), true), Some(1), &all).is_ok());
        assert_eq!(kind(validate(&spec(&p("/r/a"), false), Some(1), &all)), Some(super::super::ErrorKind::Conflict), "collides with row 2");
    }
}
