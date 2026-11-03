//! Where another machine's paths appear on this one.
//!
//! `remote_files.path` is absolute **as the source machine writes it** -- a Windows server
//! stores `D:\Roms\PS1\game.chd`, and a Linux client has that share at `/mnt/roms`. A mount
//! is the prefix pair that turns the first into the second. Copying is then plain OS file
//! copy, which is the whole point of the design in `docs/plan/api.md`: bytes never travel
//! through REST.
//!
//! [`rewrite`] is pure and is where the fiddly parts live -- separator style, case rules,
//! prefix boundaries, UNC. It has no database in it precisely so it can be tested exactly.

use std::path::{Path, PathBuf};

use romexplorer_types::copy::{Mount, MountCheck, MountSpec};
use sqlx::Row;

use crate::database::directories::canonical_path;
use crate::database::import::sha1_hex;

use super::super::{App, Error, Result};

/// A mount as the resolver uses it -- the wire [`Mount`] minus the display fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub id: u16,
    pub source_id: u16,
    pub remote_prefix: String,
    /// `/` or `\`: what the SOURCE writes, which also decides whether the prefix comparison
    /// folds case.
    pub remote_sep: char,
    pub local_path: String,
}

/// Which separator a path written like this uses: `\` for a drive letter or a UNC root,
/// `/` otherwise. Windows tolerates both, so this is about the *machine*, not the string.
pub fn detect_sep(prefix: &str) -> char {
    let b = prefix.as_bytes();
    let drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/');
    if drive || prefix.starts_with("\\\\") {
        '\\'
    } else {
        '/'
    }
}

/// Trailing separators dropped, but never the `\\` of a UNC root and never a bare `/` or
/// `C:\`. Deliberately *not* `directories::canonical_path`, which normalises for the machine
/// this code runs on; a remote prefix describes a different machine.
pub fn normalize_prefix(prefix: &str) -> String {
    let p = prefix.trim();
    if p.starts_with("\\\\") {
        let rest = p[2..].trim_end_matches(['/', '\\']);
        return format!("\\\\{rest}");
    }
    let trimmed = p.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return p.chars().next().map(String::from).unwrap_or_default();
    }
    // `D:` alone is the drive's *current* directory on Windows, not its root; keep the root.
    if trimmed.len() == 2 && trimmed.as_bytes()[1] == b':' {
        return format!("{trimmed}\\");
    }
    trimmed.to_string()
}

fn prefix_key(prefix: &str, sep: char) -> String {
    let p = normalize_prefix(prefix).replace('\\', "/");
    // Windows paths are case-insensitive, POSIX ones are not.
    if sep == '\\' {
        p.to_lowercase()
    } else {
        p
    }
}

/// The remainder of `remote_path` below `prefix`, or `None` when it is not below it.
///
/// The boundary check is the point: `D:\Roms` must not swallow `D:\RomsOld\x.zip`.
fn strip_prefix<'a>(remote_path: &'a str, prefix: &str, sep: char) -> Option<&'a str> {
    let key = prefix_key(prefix, sep);
    let path_key = {
        let p = remote_path.replace('\\', "/");
        if sep == '\\' {
            p.to_lowercase()
        } else {
            p
        }
    };
    if !path_key.starts_with(&key) {
        return None;
    }
    let rest = &remote_path[key.len()..];
    // The prefix itself, or a real child. `key` ends in a separator only for a root
    // (`/`, `C:/`), where the remainder starts immediately.
    if key.ends_with('/') || rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\') {
        Some(rest)
    } else {
        None
    }
}

/// `remote_path` (absolute, as the source machine writes it) as a path on this machine, or
/// `None` when no mount covers it.
///
/// The longest matching prefix wins -- the same nearest-configured-ancestor rule
/// `database::directories::resolve` applies to local directories, so a mount for one
/// platform's folder can sit inside a mount for the whole collection.
///
/// The remainder is split on **both** separators (a Windows path can still contain `/`) and
/// re-joined with this machine's own, and is never case-folded: the prefix comparison
/// describes the source machine, but the remainder becomes a real lookup here, and an
/// SMB-mounted Linux share is case-sensitive.
pub fn rewrite(mounts: &[Resolved], remote_path: &str) -> Option<PathBuf> {
    let (mount, rest) = mounts
        .iter()
        .filter_map(|m| strip_prefix(remote_path, &m.remote_prefix, m.remote_sep).map(|rest| (m, rest)))
        .max_by_key(|(m, _)| prefix_key(&m.remote_prefix, m.remote_sep).len())?;
    let mut out = PathBuf::from(&mount.local_path);
    for part in rest.split(['/', '\\']).filter(|p| !p.is_empty()) {
        out.push(part);
    }
    Some(out)
}

/// A path on this machine as the source machine would write it, or `None` when it is not
/// under any of `mounts`.
///
/// The inverse of `rewrite`, and it exists for uploads: the client writes through the share,
/// so the bytes land at a path only this machine knows, and the server has to be told about
/// them in its own terms. Doing it by arithmetic rather than by asking keeps the round trip
/// to one call and works for a path the server has never heard of -- which, for something
/// just uploaded, is every one of them.
///
/// The local side is compared with this machine's own case rules (exact), since it names a
/// real directory here; the remainder is re-joined with the *source's* separator, because it
/// is the source that has to read it back.
pub fn to_remote(mounts: &[Resolved], local_path: &Path) -> Option<String> {
    let (mount, rest) = mounts
        .iter()
        .filter_map(|m| {
            let root = Path::new(&m.local_path);
            local_path.strip_prefix(root).ok().map(|rest| (m, rest.to_path_buf()))
        })
        .max_by_key(|(m, _)| m.local_path.len())?;
    let sep = mount.remote_sep;
    let mut out = mount.remote_prefix.trim_end_matches(['/', '\\']).to_string();
    // A root prefix (`/`, `C:\`) trims to nothing or to a bare drive; both need the separator
    // back before anything is appended.
    if out.is_empty() || out.ends_with(':') {
        out.push(sep);
    }
    for part in rest.components() {
        let part = part.as_os_str().to_string_lossy();
        if part.is_empty() {
            continue;
        }
        if !out.ends_with(sep) {
            out.push(sep);
        }
        out.push_str(&part);
    }
    Some(out)
}

fn row_to_resolved(row: &sqlx::mysql::MySqlRow) -> Result<Resolved> {
    let sep: String = row.try_get("remote_sep")?;
    Ok(Resolved {
        id: row.try_get("id")?,
        source_id: row.try_get("source_id")?,
        remote_prefix: row.try_get("remote_prefix")?,
        remote_sep: sep.chars().next().unwrap_or('/'),
        local_path: row.try_get("local_path")?,
    })
}

const SELECT: &str = "SELECT m.id, m.source_id, m.remote_prefix, m.remote_sep, m.local_path, m.enabled, m.last_verified_at, s.name AS source_name \
    FROM remote_mounts m JOIN remote_sources s ON s.id = m.source_id";

/// Every enabled mount of `source_id` (or of every source when `None`), for the resolver.
pub async fn load(app: &App, source_id: Option<u16>) -> Result<Vec<Resolved>> {
    let sql = match source_id {
        Some(_) => format!("{SELECT} WHERE m.deleted = FALSE AND m.enabled = TRUE AND s.deleted = FALSE AND s.enabled = TRUE AND m.source_id = ?"),
        None => format!("{SELECT} WHERE m.deleted = FALSE AND m.enabled = TRUE AND s.deleted = FALSE AND s.enabled = TRUE"),
    };
    let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
    if let Some(id) = source_id {
        q = q.bind(id);
    }
    q.fetch_all(&app.pool).await?.iter().map(row_to_resolved).collect()
}

fn row_to_mount(row: &sqlx::mysql::MySqlRow) -> Result<Mount> {
    Ok(Mount {
        id: row.try_get("id")?,
        source_id: row.try_get("source_id")?,
        source_name: row.try_get("source_name")?,
        remote_prefix: row.try_get("remote_prefix")?,
        remote_sep: row.try_get("remote_sep")?,
        local_path: row.try_get("local_path")?,
        enabled: row.try_get("enabled")?,
        last_verified_at: row.try_get("last_verified_at")?,
    })
}

/// Every mount of one source, disabled ones included -- the config table shows those.
pub async fn list(app: &App, source_id: u16) -> Result<Vec<Mount>> {
    let sql = format!("{SELECT} WHERE m.deleted = FALSE AND m.source_id = ? ORDER BY CHAR_LENGTH(m.remote_prefix) DESC, m.id");
    sqlx::query(sqlx::AssertSqlSafe(sql)).bind(source_id).fetch_all(&app.pool).await?.iter().map(row_to_mount).collect()
}

pub async fn get(app: &App, id: u16) -> Result<Mount> {
    let sql = format!("{SELECT} WHERE m.id = ? AND m.deleted = FALSE");
    let row = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(id).fetch_optional(&app.pool).await?;
    row.as_ref().map(row_to_mount).transpose()?.ok_or_else(|| Error::not_found(format!("no mount {id}")))
}

pub async fn create(app: &App, source_id: u16, spec: &MountSpec) -> Result<Mount> {
    let prefix = normalize_prefix(&spec.remote_prefix);
    if prefix.is_empty() {
        return Err(Error::invalid("the remote prefix can't be empty -- it's the path as the other machine writes it, e.g. D:\\Roms or /srv/roms"));
    }
    let local = canonical_path(&spec.local_path);
    if local.is_empty() || !Path::new(&local).is_absolute() {
        return Err(Error::invalid(format!("the local path must be absolute: {local:?}")));
    }
    let sep = match spec.remote_sep.as_deref() {
        Some("\\") => '\\',
        Some("/") => '/',
        Some(other) => return Err(Error::invalid(format!("remote_sep must be \"/\" or \"\\\\\", got {other:?}"))),
        None => detect_sep(&prefix),
    };
    sqlx::query(
        "INSERT INTO remote_mounts (source_id, remote_prefix, remote_prefix_hash, remote_directory_id, remote_sep, local_path, local_path_hash, enabled, deleted) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, FALSE) \
         ON DUPLICATE KEY UPDATE remote_prefix = VALUES(remote_prefix), remote_directory_id = VALUES(remote_directory_id), remote_sep = VALUES(remote_sep), \
             local_path = VALUES(local_path), local_path_hash = VALUES(local_path_hash), enabled = VALUES(enabled), deleted = FALSE",
    )
    .bind(source_id)
    .bind(&prefix)
    .bind(sha1_hex(&prefix_key(&prefix, sep)))
    .bind(spec.remote_directory_id)
    .bind(sep.to_string())
    .bind(&local)
    .bind(sha1_hex(&local))
    .bind(spec.enabled)
    .execute(&app.pool)
    .await?;
    let id: u16 = sqlx::query_scalar("SELECT id FROM remote_mounts WHERE source_id = ? AND remote_prefix_hash = ?").bind(source_id).bind(sha1_hex(&prefix_key(&prefix, sep))).fetch_one(&app.pool).await?;
    get(app, id).await
}

/// Soft-delete, like every other entity here (`CODING_RULES.md`): `db_optimize` is the only
/// thing that ever hard-deletes.
pub async fn remove(app: &App, id: u16) -> Result<()> {
    let n = sqlx::query("UPDATE remote_mounts SET deleted = TRUE WHERE id = ?").bind(id).execute(&app.pool).await?;
    if n.rows_affected() == 0 {
        return Err(Error::not_found(format!("no mount {id}")));
    }
    Ok(())
}

/// The collection roots a source offers, so the add-a-mount dialog can offer them rather
/// than asking the user to retype a path from another machine.
pub async fn roots(app: &App, source_id: u16) -> Result<Vec<(u64, String)>> {
    let rows = sqlx::query("SELECT id, path FROM remote_directories WHERE source_id = ? AND is_collection_root = TRUE AND deleted = FALSE ORDER BY path").bind(source_id).fetch_all(&app.pool).await?;
    rows.iter().map(|r| Ok((r.try_get("id")?, r.try_get("path")?))).collect()
}

/// Is this mount actually there, and is it the *right* share?
///
/// Three escalating checks, each cheap. The probe is the one that matters: a mount aimed at
/// the wrong share passes `is_dir` and would otherwise only be discovered after copying a
/// thousand wrong files.
pub async fn verify(app: &App, mount: &Resolved) -> Result<MountCheck> {
    let local = PathBuf::from(&mount.local_path);
    if !local.is_dir() {
        return Ok(MountCheck { ok: false, detail: format!("{} is not a directory -- is the share mounted?", mount.local_path), probed: None });
    }
    let row = sqlx::query(
        "SELECT path, size FROM remote_files WHERE source_id = ? AND deleted = FALSE AND path LIKE ? ORDER BY id LIMIT 1",
    )
    .bind(mount.source_id)
    .bind(format!("{}%", super::super::like_escape(&normalize_prefix(&mount.remote_prefix))))
    .fetch_optional(&app.pool)
    .await?;
    let Some(row) = row else {
        return Ok(MountCheck { ok: true, detail: "mounted; nothing synced under this prefix yet, so there was nothing to probe".into(), probed: None });
    };
    let (remote_path, size): (String, u64) = (row.try_get("path")?, row.try_get("size")?);
    let Some(local_file) = rewrite(std::slice::from_ref(mount), &remote_path) else {
        return Ok(MountCheck { ok: false, detail: format!("the prefix doesn't cover {remote_path}"), probed: Some(remote_path) });
    };
    let shown = local_file.display().to_string();
    match std::fs::metadata(&local_file) {
        Ok(m) if m.len() == size => Ok(MountCheck { ok: true, detail: format!("mounted; {shown} is there and the right size"), probed: Some(shown) }),
        Ok(m) => Ok(MountCheck {
            ok: false,
            detail: format!("{shown} is {} bytes, the catalogue says {size} -- this looks like a different share", m.len()),
            probed: Some(shown),
        }),
        Err(e) => Ok(MountCheck { ok: false, detail: format!("can't read {shown}: {e}"), probed: Some(shown) }),
    }
}

/// Records a successful check, so the config table can say when it last worked.
pub async fn mark_verified(app: &App, id: u16) -> Result<()> {
    sqlx::query("UPDATE remote_mounts SET last_verified_at = NOW() WHERE id = ?").bind(id).execute(&app.pool).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(prefix: &str, local: &str) -> Resolved {
        Resolved { id: 1, source_id: 1, remote_sep: detect_sep(prefix), remote_prefix: normalize_prefix(prefix), local_path: local.into() }
    }

    fn rw(mounts: &[Resolved], path: &str) -> Option<String> {
        rewrite(mounts, path).map(|p| p.display().to_string())
    }

    /// The upload direction. Round-tripping is the property that matters: what the client
    /// wrote is what the server is told to scan.
    #[test]
    fn maps_a_local_path_back_to_the_source_machines_spelling() {
        let mounts = [m("D:\\Roms", "/mnt/server")];
        assert_eq!(to_remote(&mounts, Path::new("/mnt/server/snes/Game.sfc")).as_deref(), Some("D:\\Roms\\snes\\Game.sfc"));
        assert_eq!(to_remote(&mounts, Path::new("/mnt/server")).as_deref(), Some("D:\\Roms"));
        assert_eq!(to_remote(&mounts, Path::new("/mnt/elsewhere/x.sfc")), None, "outside every mount");
    }

    #[test]
    fn the_reverse_mapping_undoes_the_forward_one() {
        for (prefix, local, remote) in [
            ("D:\\Roms", "/mnt/server", "D:\\Roms\\snes\\Game.sfc"),
            ("/srv/roms", "/mnt/server", "/srv/roms/snes/Game.sfc"),
            ("\\\\nas\\share\\Roms", "/mnt/nas", "\\\\nas\\share\\Roms\\ps1\\disc.chd"),
        ] {
            let mounts = [m(prefix, local)];
            let here = rewrite(&mounts, remote).expect("maps in");
            assert_eq!(to_remote(&mounts, &here).as_deref(), Some(remote), "round trip through {prefix}");
        }
    }

    /// The same nearest-ancestor rule as the forward direction, so a mount inside a mount
    /// does not send the server a path built from the outer one.
    #[test]
    fn the_longest_local_prefix_wins_going_back() {
        let mounts = [m("/srv/roms", "/mnt/server"), m("/srv/roms/snes", "/mnt/server/snes")];
        assert_eq!(to_remote(&mounts, Path::new("/mnt/server/snes/Game.sfc")).as_deref(), Some("/srv/roms/snes/Game.sfc"));
    }

    #[test]
    fn detects_the_source_machines_separator() {
        assert_eq!(detect_sep("D:\\Roms"), '\\');
        assert_eq!(detect_sep("D:/Roms"), '\\');
        assert_eq!(detect_sep("\\\\nas\\share\\Roms"), '\\');
        assert_eq!(detect_sep("/srv/roms"), '/');
        assert_eq!(detect_sep("/mnt/d/Roms"), '/');
    }

    #[test]
    fn normalizes_without_eating_roots() {
        assert_eq!(normalize_prefix("  /srv/roms/  "), "/srv/roms");
        assert_eq!(normalize_prefix("D:\\Roms\\"), "D:\\Roms");
        assert_eq!(normalize_prefix("D:"), "D:\\");
        assert_eq!(normalize_prefix("D:\\"), "D:\\");
        assert_eq!(normalize_prefix("/"), "/");
        assert_eq!(normalize_prefix("\\\\nas\\share\\"), "\\\\nas\\share");
    }

    #[test]
    fn rewrites_a_windows_source_onto_a_posix_mount() {
        let sep = std::path::MAIN_SEPARATOR;
        let mounts = [m("D:\\Roms", "/mnt/roms")];
        assert_eq!(rw(&mounts, "D:\\Roms\\PS1\\game.chd"), Some(format!("/mnt/roms{sep}PS1{sep}game.chd")));
        // A Windows path may still carry forward slashes; the remainder splits on both.
        assert_eq!(rw(&mounts, "D:/Roms/PS1/game.chd"), Some(format!("/mnt/roms{sep}PS1{sep}game.chd")));
    }

    #[test]
    fn windows_prefixes_fold_case_and_posix_ones_do_not() {
        assert!(rw(&[m("D:\\Roms", "/mnt/roms")], "d:\\roms\\a.zip").is_some());
        assert!(rw(&[m("/srv/Roms", "/mnt/roms")], "/srv/roms/a.zip").is_none());
    }

    /// The remainder is a real lookup on this machine, so folding its case would break an
    /// SMB-mounted Linux share.
    #[test]
    fn the_remainder_keeps_its_case() {
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(rw(&[m("D:\\Roms", "/mnt/roms")], "D:\\Roms\\PS1\\Game.CHD"), Some(format!("/mnt/roms{sep}PS1{sep}Game.CHD")));
    }

    #[test]
    fn a_prefix_only_matches_on_a_component_boundary() {
        let mounts = [m("D:\\Roms", "/mnt/roms")];
        assert_eq!(rw(&mounts, "D:\\RomsOld\\x.zip"), None);
        assert_eq!(rw(&mounts, "D:\\Roms"), Some("/mnt/roms".to_string()));
    }

    #[test]
    fn the_longest_prefix_wins() {
        let sep = std::path::MAIN_SEPARATOR;
        let mounts = [m("/srv", "/mnt/all"), m("/srv/roms/ps1", "/mnt/ps1")];
        assert_eq!(rw(&mounts, "/srv/roms/ps1/game.chd"), Some(format!("/mnt/ps1{sep}game.chd")));
        assert_eq!(rw(&mounts, "/srv/bios/x.bin"), Some(format!("/mnt/all{sep}bios{sep}x.bin")));
    }

    #[test]
    fn unc_and_drive_roots_work() {
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(rw(&[m("\\\\nas\\share", "/mnt/nas")], "\\\\nas\\share\\Roms\\a.zip"), Some(format!("/mnt/nas{sep}Roms{sep}a.zip")));
        assert_eq!(rw(&[m("D:\\", "/mnt/d")], "D:\\Roms\\a.zip"), Some(format!("/mnt/d{sep}Roms{sep}a.zip")));
    }

    #[test]
    fn an_uncovered_path_is_none_rather_than_a_guess() {
        assert_eq!(rw(&[m("/srv/roms", "/mnt/roms")], "/other/place/a.zip"), None);
        assert_eq!(rw(&[], "/srv/roms/a.zip"), None);
    }
}
