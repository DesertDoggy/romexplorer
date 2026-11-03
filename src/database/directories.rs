//! The `local_directories` table: collection roots and the configured subdirectories inside
//! them. Only directories that need a setting different from their nearest configured
//! ancestor have a row; everything else inherits (`resolve`).
//!
//! `parent_directory_id` is never user input: it's always the nearest configured directory
//! whose path contains this one (`derive_parent`, kept current by `relink_parents`), and
//! NULL exactly for collection roots. The rules about what may be configured where live in
//! `api::directories`, the only writer.
//!
//! Nothing in here touches the filesystem: every function reads or writes config rows only.

use serde::{Deserialize, Serialize};
use sqlx::mysql::MySqlPool;
use sqlx::Row;

use super::error::DatabaseError;
use super::import::sha1_hex;

/// `patch`: compatibility / translation patches (RetroAchievements patch downloads land in
/// the first one, see [`patch_dir`]); scanned and hashed, not made into ROMs.
pub const DIR_TYPES: [&str; 4] = ["bios", "rom", "mixed", "patch"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Directory {
    pub id: u64,
    pub parent_directory_id: Option<u64>,
    pub path: String,
    pub is_collection_root: bool,
    pub dir_type: Option<String>,
    pub app_platform_id: Option<u16>,
    pub is_archival: bool,
    pub deleted: bool,
    /// Its place among its siblings (same parent; the roots among the roots), lower first
    /// (migration 0057): which copy of a release comes first.
    #[serde(default)]
    pub priority: i64,
}

/// Insert/update payload. `None` for `dir_type`/`app_platform_id` means "inherit". There's
/// no parent field: the parent follows from the path (`derive_parent`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DirectorySpec {
    pub path: String,
    #[serde(default)]
    pub is_collection_root: bool,
    pub dir_type: Option<String>,
    pub app_platform_id: Option<u16>,
    #[serde(default)]
    pub is_archival: bool,
}

/// What actually applies to a path once inheritance is resolved.
#[derive(Debug, Clone, Serialize)]
pub struct Effective {
    /// The nearest configured row at or above the path, if any.
    pub directory: Option<Directory>,
    pub collection_root: Option<Directory>,
    pub dir_type: Option<String>,
    pub app_platform_id: Option<u16>,
    pub is_archival: bool,
}

const SELECT: &str = "SELECT id, parent_directory_id, path, is_collection_root, dir_type, app_platform_id, is_archival, deleted, priority FROM local_directories";

fn row_to_directory(row: &sqlx::mysql::MySqlRow) -> Result<Directory, DatabaseError> {
    Ok(Directory {
        id: row.try_get("id")?,
        parent_directory_id: row.try_get("parent_directory_id")?,
        path: row.try_get("path")?,
        is_collection_root: row.try_get("is_collection_root")?,
        dir_type: row.try_get("dir_type")?,
        app_platform_id: row.try_get("app_platform_id")?,
        is_archival: row.try_get("is_archival")?,
        deleted: row.try_get("deleted")?,
        priority: row.try_get::<i32, _>("priority").map(i64::from).unwrap_or(0),
    })
}

/// Each directory's place in the whole tree: its ancestors' priorities from the root down,
/// then its own. Comparing two keys compares the directories the way the list shows them.
pub async fn priority_keys(pool: &MySqlPool) -> Result<std::collections::HashMap<u64, Vec<i64>>, DatabaseError> {
    let all = list(pool, true).await?;
    Ok(keys_of(all.iter().map(|d| (d.id, d.parent_directory_id, d.priority)).collect()))
}

/// [`priority_keys`] for a server's or a peer's directories, as mirrored from it
/// (`remote_directories.priority`): the order is the one set on that machine.
pub async fn remote_priority_keys(pool: &MySqlPool, source_id: u16) -> Result<std::collections::HashMap<u64, Vec<i64>>, DatabaseError> {
    let rows: Vec<(u64, Option<u64>, i32)> = sqlx::query_as("SELECT id, parent_directory_id, priority FROM remote_directories WHERE source_id = ?").bind(source_id).fetch_all(pool).await?;
    Ok(keys_of(rows.into_iter().map(|(id, parent, priority)| (id, parent, priority as i64)).collect()))
}

/// Each directory's priority key: its ancestors' priorities, root first, then its own.
fn keys_of(dirs: Vec<(u64, Option<u64>, i64)>) -> std::collections::HashMap<u64, Vec<i64>> {
    let by_id: std::collections::HashMap<u64, (Option<u64>, i64)> = dirs.iter().map(|(id, parent, priority)| (*id, (*parent, *priority))).collect();
    let mut out = std::collections::HashMap::new();
    for (id, parent, priority) in &dirs {
        let mut key = vec![*priority];
        let mut at = *parent;
        // A cycle cannot be made through the API, but a key is never worth a hang.
        while let Some((next, p)) = at.and_then(|id| by_id.get(&id)).copied().filter(|_| key.len() < 64) {
            key.push(p);
            at = next;
        }
        key.reverse();
        out.insert(*id, key);
    }
    out
}

/// Moves a directory one place up or down among its siblings, renumbering them 1..n.
pub async fn move_directory(pool: &MySqlPool, id: u64, up: bool) -> Result<(), DatabaseError> {
    let all = list(pool, false).await?;
    let Some(me) = all.iter().find(|d| d.id == id) else { return Err(DatabaseError::Config(format!("no directory {id}"))) };
    let mut siblings: Vec<&Directory> = all.iter().filter(|d| d.parent_directory_id == me.parent_directory_id).collect();
    siblings.sort_by(|a, b| (a.priority, &a.path).cmp(&(b.priority, &b.path)));
    let Some(at) = siblings.iter().position(|d| d.id == id) else { return Ok(()) };
    let to = if up { at.checked_sub(1) } else { Some(at + 1).filter(|n| *n < siblings.len()) };
    if let Some(to) = to {
        siblings.swap(at, to);
    }
    for (n, d) in siblings.iter().enumerate() {
        sqlx::query("UPDATE local_directories SET priority = ? WHERE id = ?").bind(n as i64 + 1).bind(d.id).execute(pool).await?;
    }
    Ok(())
}

/// A new directory goes last among its siblings.
async fn place_last(pool: &MySqlPool, id: u64, parent: Option<u64>) -> Result<(), DatabaseError> {
    let max: Option<i32> = sqlx::query_scalar("SELECT MAX(priority) FROM local_directories WHERE deleted = FALSE AND id <> ? AND COALESCE(parent_directory_id, 0) = COALESCE(?, 0)").bind(id).bind(parent).fetch_one(pool).await?;
    sqlx::query("UPDATE local_directories SET priority = ? WHERE id = ?").bind(max.unwrap_or(0) + 1).bind(id).execute(pool).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// paths
// ---------------------------------------------------------------------------

/// Whether paths on this OS compare case-insensitively (NTFS and APFS/HFS+ by default).
const CASE_INSENSITIVE: bool = cfg!(any(windows, target_os = "macos"));

fn is_sep(b: u8) -> bool {
    b == b'/' || b == b'\\'
}

/// The form a path is stored in: trimmed, no trailing separator (a bare `/` or `C:\` is
/// kept), and on Windows `\` separators and an uppercase drive letter.
pub fn canonical_path(path: &str) -> String {
    let mut p = path.trim().to_string();
    if cfg!(windows) {
        p = p.replace('/', "\\");
        if p.as_bytes().get(1) == Some(&b':') {
            p[..1].make_ascii_uppercase();
        }
    }
    let trimmed = p.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return p.chars().next().map(String::from).unwrap_or_default();
    }
    if trimmed.len() == 2 && trimmed.as_bytes()[1] == b':' {
        return format!("{trimmed}{}", std::path::MAIN_SEPARATOR);
    }
    trimmed.to_string()
}

/// Comparison key: trailing separators dropped, `\` read as `/`, case folded where the OS
/// folds it.
fn path_key(path: &str) -> String {
    let p = path.trim().trim_end_matches(['/', '\\']).replace('\\', "/");
    if CASE_INSENSITIVE {
        p.to_lowercase()
    } else {
        p
    }
}

/// Same directory, modulo trailing separators, separator style and (on Windows/macOS) case.
pub fn same_path(a: &str, b: &str) -> bool {
    path_key(a) == path_key(b)
}

/// `path` is `ancestor` itself or lies below it -- on a component boundary, so `/a/b` does
/// not contain `/a/bc`.
pub fn path_contains(ancestor: &str, path: &str) -> bool {
    let (a, p) = (path_key(ancestor), path_key(path));
    if a.is_empty() {
        // `/` (its key is empty once the trailing separator is gone) contains every
        // absolute unix path.
        return p.starts_with('/') || p.is_empty();
    }
    p == a || (p.starts_with(&a) && p.as_bytes().get(a.len()).copied().is_some_and(is_sep))
}

/// `path` lies strictly below `ancestor`.
pub fn path_strictly_inside(ancestor: &str, path: &str) -> bool {
    path_contains(ancestor, path) && !same_path(ancestor, path)
}

/// The nearest (longest-path) non-deleted row strictly containing `path`, skipping
/// `self_id` -- what `parent_directory_id` must be for a directory at `path`.
pub fn derive_parent(rows: &[Directory], path: &str, self_id: Option<u64>) -> Option<u64> {
    rows.iter()
        .filter(|d| !d.deleted && Some(d.id) != self_id && path_strictly_inside(&d.path, path))
        .max_by_key(|d| path_key(&d.path).len())
        .map(|d| d.id)
}

/// Every non-deleted row strictly below `path`.
pub fn descendants<'a>(rows: &'a [Directory], path: &str) -> impl Iterator<Item = &'a Directory> + 'a {
    let path = path.to_string();
    rows.iter().filter(move |d| !d.deleted && path_strictly_inside(&path, &d.path))
}

// ---------------------------------------------------------------------------
// rows
// ---------------------------------------------------------------------------

/// Inserts, or -- when a (possibly soft-deleted) row for the same path already exists --
/// updates it in place and undeletes it. Returns the row id. `same_path` rather than the
/// exact `path_hash`, so `d:\roms` finds an existing `D:\Roms` row on Windows.
pub async fn upsert(pool: &MySqlPool, spec: &DirectorySpec, parent_directory_id: Option<u64>) -> Result<u64, DatabaseError> {
    if let Some(existing) = list(pool, true).await?.into_iter().find(|d| same_path(&d.path, &spec.path)) {
        update(pool, existing.id, spec, parent_directory_id).await?;
        return Ok(existing.id);
    }
    let result = sqlx::query("INSERT INTO local_directories (parent_directory_id, path, path_hash, is_collection_root, dir_type, app_platform_id, is_archival) VALUES (?, ?, ?, ?, ?, ?, ?)")
        .bind(parent_directory_id)
        .bind(&spec.path)
        .bind(sha1_hex(&spec.path))
        .bind(spec.is_collection_root)
        .bind(&spec.dir_type)
        .bind(spec.app_platform_id)
        .bind(spec.is_archival)
        .execute(pool)
        .await?;
    let id = result.last_insert_id();
    place_last(pool, id, parent_directory_id).await?;
    Ok(id)
}

pub async fn update(pool: &MySqlPool, id: u64, spec: &DirectorySpec, parent_directory_id: Option<u64>) -> Result<bool, DatabaseError> {
    let result = sqlx::query("UPDATE local_directories SET parent_directory_id = ?, path = ?, path_hash = ?, is_collection_root = ?, dir_type = ?, app_platform_id = ?, is_archival = ?, deleted = FALSE WHERE id = ?")
        .bind(parent_directory_id)
        .bind(&spec.path)
        .bind(sha1_hex(&spec.path))
        .bind(spec.is_collection_root)
        .bind(&spec.dir_type)
        .bind(spec.app_platform_id)
        .bind(spec.is_archival)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn get(pool: &MySqlPool, id: u64) -> Result<Option<Directory>, DatabaseError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!("{SELECT} WHERE id = ?"))).bind(id).fetch_optional(pool).await?;
    row.as_ref().map(row_to_directory).transpose()
}

pub async fn list(pool: &MySqlPool, include_deleted: bool) -> Result<Vec<Directory>, DatabaseError> {
    let sql = if include_deleted { format!("{SELECT} ORDER BY path") } else { format!("{SELECT} WHERE deleted = FALSE ORDER BY path") };
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(pool).await?;
    rows.iter().map(row_to_directory).collect()
}

/// Soft-deletes these configuration rows -- the rows only: the directories and files on
/// disk are never touched. Their `local_files` keep pointing at them; a scan of an
/// enclosing directory re-governs them. Returns how many rows changed.
pub async fn soft_delete_many(pool: &MySqlPool, ids: &[u64]) -> Result<u64, DatabaseError> {
    if ids.is_empty() {
        return Ok(0);
    }
    let placeholders = vec!["?"; ids.len()].join(", ");
    let sql = format!("UPDATE local_directories SET deleted = TRUE WHERE deleted = FALSE AND id IN ({placeholders})");
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    for id in ids {
        query = query.bind(id);
    }
    Ok(query.execute(pool).await?.rows_affected())
}

/// Recomputes `parent_directory_id` of every non-deleted row from the paths (NULL for
/// roots), writing only rows that changed -- after an insert between a directory and its
/// configured child, or a removal, the child's parent moves with it.
pub async fn relink_parents(pool: &MySqlPool) -> Result<(), DatabaseError> {
    let rows = list(pool, false).await?;
    for d in &rows {
        let want = if d.is_collection_root { None } else { derive_parent(&rows, &d.path, Some(d.id)) };
        if want != d.parent_directory_id {
            sqlx::query("UPDATE local_directories SET parent_directory_id = ? WHERE id = ?").bind(want).bind(d.id).execute(pool).await?;
        }
    }
    Ok(())
}

/// Resolves what applies to `path`: the nearest non-deleted configured directory at or
/// above it (longest matching prefix on a path-component boundary), then its ancestors for
/// any setting the nearest row leaves as "inherit".
pub async fn resolve(pool: &MySqlPool, path: &str) -> Result<Effective, DatabaseError> {
    let all = list(pool, false).await?;
    let nearest = all.iter().filter(|d| path_contains(&d.path, path)).max_by_key(|d| path_key(&d.path).len());
    let Some(nearest) = nearest else {
        return Ok(Effective { directory: None, collection_root: None, dir_type: None, app_platform_id: None, is_archival: false });
    };
    let mut dir_type = nearest.dir_type.clone();
    let mut platform = nearest.app_platform_id;
    let mut archival = nearest.is_archival;
    let mut root = if nearest.is_collection_root { Some(nearest.clone()) } else { None };
    let mut cursor = nearest.parent_directory_id;
    let mut hops = 0;
    while let Some(pid) = cursor {
        hops += 1;
        if hops > 64 {
            break;
        }
        let Some(parent) = all.iter().find(|d| d.id == pid) else { break };
        if dir_type.is_none() {
            dir_type = parent.dir_type.clone();
        }
        if platform.is_none() {
            platform = parent.app_platform_id;
        }
        archival = archival || parent.is_archival;
        if root.is_none() && parent.is_collection_root {
            root = Some(parent.clone());
        }
        cursor = parent.parent_directory_id;
    }
    Ok(Effective { directory: Some(nearest.clone()), collection_root: root, dir_type, app_platform_id: platform, is_archival: archival })
}

/// The directory patches are downloaded to: the live directory of type `patch`, a collection
/// root first, then the oldest (None: no such directory configured).
pub async fn patch_dir(pool: &MySqlPool) -> Result<Option<std::path::PathBuf>, DatabaseError> {
    let path: Option<String> = sqlx::query_scalar("SELECT path FROM local_directories WHERE dir_type = 'patch' AND deleted = FALSE ORDER BY is_collection_root DESC, id LIMIT 1").fetch_optional(pool).await?;
    Ok(path.map(std::path::PathBuf::from))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(id: u64, path: &str, root: bool) -> Directory {
        Directory { id, parent_directory_id: None, path: path.to_string(), is_collection_root: root, dir_type: None, app_platform_id: None, is_archival: false, deleted: false, priority: 0 }
    }

    #[test]
    fn contains_only_on_component_boundaries() {
        assert!(path_contains("/a/b", "/a/b"));
        assert!(path_contains("/a/b", "/a/b/c"));
        assert!(path_contains("/a/b/", "/a/b/c"));
        assert!(!path_contains("/a/b", "/a/bc"));
        assert!(!path_contains("/a/b/c", "/a/b"));
        assert!(path_contains("/", "/anything"));
        assert!(path_contains("D:\\roms", "D:/roms/snes"), "separator style doesn't matter");
        assert!(!path_strictly_inside("/a/b", "/a/b/"));
        assert_eq!(path_contains("D:\\Roms", "d:\\roms\\snes"), CASE_INSENSITIVE);
    }

    #[test]
    fn canonical_form() {
        if !cfg!(windows) {
            assert_eq!(canonical_path("  /a/b/ "), "/a/b");
            assert_eq!(canonical_path("/"), "/");
        } else {
            assert_eq!(canonical_path("d:/roms/snes\\"), "D:\\roms\\snes");
            assert_eq!(canonical_path("c:\\"), "C:\\");
            assert_eq!(canonical_path("c:"), "C:\\");
        }
    }

    #[test]
    fn parent_is_the_nearest_enclosing_row() {
        let rows = vec![dir(1, "/r", true), dir(2, "/r/a", false), dir(3, "/r/a/b", false), dir(4, "/other", true)];
        assert_eq!(derive_parent(&rows, "/r/a/b/c", None), Some(3));
        assert_eq!(derive_parent(&rows, "/r/a/b", Some(3)), Some(2), "a row is never its own parent");
        assert_eq!(derive_parent(&rows, "/r/x", None), Some(1));
        assert_eq!(derive_parent(&rows, "/elsewhere", None), None);
        let below: Vec<u64> = descendants(&rows, "/r").map(|d| d.id).collect();
        assert_eq!(below, [2, 3]);
    }
}
