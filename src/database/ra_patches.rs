//! RetroAchievements compatibility patches: the archive a hash's `PatchUrl` points at
//! (`ra_game_hashes`) is downloaded as is into the directory of type `patch`, under
//! `<patch dir>/<Platform>/`, hashed (the archive and every member, into `content` /
//! `hashes`, members linked in `content_contents`) and recorded in `ra_patches` /
//! `ra_patch_files` with the names a later install needs: the patched release's canonical
//! name and the stem the archive's patch / sheet files share.

use std::path::{Path, PathBuf};

use sqlx::mysql::MySqlPool;
use sqlx::Row;

use super::content;
use super::error::DatabaseError;
use super::jobs::Progress;
use crate::common::models::Platform;
use crate::rom::hash::{self, HashDigest, HashFlags, HashOptions, HashReport};

/// Largest patch download accepted.
pub const MAX_BYTES: u64 = 1 << 30;

/// What a file inside a patch archive is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchKind {
    /// A patch format: .ips / .bps / .ups / .xdelta / .ppf / ...
    Patch,
    /// A disc sheet the patched image needs: .cue / .gdi / .ccd / ...
    Sheet,
    /// Text / documents / pictures for the reader.
    Readme,
    /// Anything else (kept, and counted with the patch files).
    Other,
}

impl PatchKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Patch => "patch",
            Self::Sheet => "sheet",
            Self::Readme => "readme",
            Self::Other => "other",
        }
    }
}

fn extension(file_name: &str) -> Option<String> {
    Path::new(file_name).extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase())
}

/// Classifies a member by its extension.
pub fn patch_kind(file_name: &str) -> PatchKind {
    match extension(file_name).as_deref() {
        Some(
            "ips" | "ips32" | "bps" | "ups" | "xdelta" | "xdelta3" | "vcdiff" | "delta" | "ppf" | "aps" | "rup" | "ebp" | "bdf" | "bsdiff" | "mod" | "dps" | "gdiff" | "hdiff"
            | "pmsr" | "ffp" | "pat",
        ) => PatchKind::Patch,
        Some("cue" | "gdi" | "ccd" | "sub" | "m3u" | "toc") => PatchKind::Sheet,
        Some(
            "txt" | "nfo" | "md" | "pdf" | "htm" | "html" | "rtf" | "doc" | "docx" | "diz" | "1st" | "url" | "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp",
        ) => PatchKind::Readme,
        _ => PatchKind::Other,
    }
}

fn base_name(member_path: &str) -> &str {
    member_path.rsplit(['/', '\\']).next().unwrap_or(member_path)
}

/// The stem the non-readme members share ("Game (Japan) [T-En]" for its .xdelta + .cue):
/// (the stem -- the most common one when they differ --, whether they all agree). No such
/// members: (None, true).
pub fn common_stem(members: &[String]) -> (Option<String>, bool) {
    let stems: Vec<String> = members
        .iter()
        .filter(|m| patch_kind(m) != PatchKind::Readme)
        .map(|m| {
            let name = base_name(m);
            Path::new(name).file_stem().and_then(|s| s.to_str()).unwrap_or(name).to_string()
        })
        .collect();
    let Some(first) = stems.first() else { return (None, true) };
    if stems.iter().all(|s| s == first) {
        return (Some(first.clone()), true);
    }
    let mut best = first.clone();
    let mut best_count = 0;
    for s in &stems {
        let count = stems.iter().filter(|x| *x == s).count();
        if count > best_count {
            best = s.clone();
            best_count = count;
        }
    }
    (Some(best), false)
}

/// Patch URLs come from a third-party API: only https downloads from RetroAchievements or
/// GitHub (where RAPatches lives) are followed.
pub fn allowed_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else { return false };
    if parsed.scheme() != "https" {
        return false;
    }
    let Some(host) = parsed.host_str().map(str::to_ascii_lowercase) else { return false };
    ["retroachievements.org", "github.com", "githubusercontent.com"].iter().any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

/// The platform's folder under the patch directory: its code name (`PlayStation2`).
pub fn platform_folder(platform: Platform) -> String {
    super::media_store::sanitize_component(&format!("{platform:?}"))
}

/// A safe file name for the download: the URL's last path segment, percent-decoded, with
/// characters no filesystem accepts replaced; the extension is kept.
pub fn file_name_from_url(url: &str, fallback_stem: &str) -> String {
    let segment = reqwest::Url::parse(url).ok().and_then(|u| u.path_segments().and_then(|mut s| s.next_back().map(str::to_string))).unwrap_or_default();
    let decoded = super::dat_download::percent_decode(&segment);
    let clean: String = decoded.chars().map(|c| if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') { '_' } else { c }).collect();
    let clean = clean.trim().trim_end_matches(['.', ' ']).to_string();
    let path = Path::new(&clean);
    let (stem, ext) = (path.file_stem().and_then(|s| s.to_str()).unwrap_or(""), path.extension().and_then(|e| e.to_str()).unwrap_or(""));
    let stem: String = if stem.is_empty() { fallback_stem.to_string() } else { stem.chars().take(180).collect() };
    if ext.is_empty() {
        format!("{stem}.zip")
    } else {
        format!("{stem}.{ext}")
    }
}

/// One file inside a patch archive (or the downloaded file itself, when it isn't one).
#[derive(Debug, Clone)]
pub struct Member {
    pub path: String,
    pub size: Option<u64>,
    pub report: HashReport,
}

fn hash_options() -> HashOptions {
    HashOptions { flags: HashFlags::CRC32 | HashFlags::MD5 | HashFlags::SHA1 | HashFlags::SHA256, buf_size: None, console_id: None }
}

/// zip members with the pure-Rust reader (when the native archive library is missing).
fn zip_members(path: &Path) -> std::io::Result<Vec<Member>> {
    use std::io::Read;
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path)?).map_err(std::io::Error::other)?;
    let mut out = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(std::io::Error::other)?;
        if entry.is_dir() {
            continue;
        }
        let digest = HashDigest::new(hash_options().flags);
        let mut buf = vec![0u8; 1 << 16];
        let mut size = 0u64;
        loop {
            let n = entry.read(&mut buf)?;
            if n == 0 {
                break;
            }
            size += n as u64;
            digest.update(&buf[..n]).map_err(|e| std::io::Error::other(format!("{e:?}")))?;
        }
        out.push(Member { path: entry.name().to_string(), size: Some(size), report: digest.finish() });
    }
    Ok(out)
}

/// Hashes a downloaded patch: the file itself, and each member of a zip / 7z (streamed,
/// nothing extracted) -- one `Single` pass, the file's own bytes read beside the decode of its
/// members. Returns (the file's hashes, "zip" / "7z" / None, members). A non-archive file (a
/// bare .bps) is its own single member. Blocking.
pub fn hash_patch_file(path: &Path) -> std::io::Result<(HashReport, Option<&'static str>, Vec<Member>)> {
    use crate::rom::operation::{Detect, Single, SingleArgs, StageSwitches};
    let flags = hash_options().flags;
    let args = SingleArgs {
        stages: StageSwitches { hash: true, ..StageSwitches::NONE },
        hash_flags: flags,
        detect: Detect::Trust(Platform::Unknown),
        whole_file: true,
        container_hash: Some(flags),
        ..SingleArgs::default()
    };
    let outcome = Single::new(path, args).run(&mut |_| true).map_err(|e| std::io::Error::other(e.to_string()))?;
    let whole = match outcome.input_container {
        Some(Ok(report)) => report,
        Some(Err(e)) => return Err(std::io::Error::other(e)),
        None => hash::hash(path, &hash_options(), |_| {})?,
    };
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
    let format = match extension(&file_name).as_deref() {
        Some("zip") => Some("zip"),
        Some("7z") => Some("7z"),
        _ => None,
    };
    let Some(format) = format else {
        let size = std::fs::metadata(path)?.len();
        return Ok((whole.clone(), None, vec![Member { path: file_name, size: Some(size), report: whole }]));
    };
    // Decoded members, unless the archive could only be read as a plain file (no streaming
    // decoder here): then its one "content file" is itself.
    let decoded = outcome.content_files.iter().any(|f| f.source_path.is_none());
    let mut members: Vec<Member> = if decoded {
        outcome.input_hashes.map(|r| r.content_hashes).unwrap_or_default().into_iter().map(|m| Member { path: m.path.clone(), size: m.size_bytes, report: HashReport { crc32: m.crc32, md5: m.md5, sha1: m.sha1, sha256: m.sha256, ..Default::default() } }).collect()
    } else {
        Vec::new()
    };
    if members.is_empty() && format == "zip" {
        members = zip_members(path)?;
    }
    Ok((whole, Some(format), members))
}

/// What a patch record is for.
#[derive(Debug, Clone)]
pub struct PatchTarget {
    pub ra_game_id: u32,
    pub md5: String,
    pub patch_url: String,
    pub patched_name: String,
    pub release_id: Option<u64>,
    pub platform: Platform,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RecordSummary {
    pub patch_id: u64,
    pub rel_path: String,
    pub members: usize,
    pub patch_rom_name: Option<String>,
    pub names_consistent: bool,
    /// The same archive was already held under this path (the fresh download was dropped).
    pub duplicate_of: Option<String>,
}

/// `path` relative to `root` with '/' separators.
fn relative(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/")
}

/// Hashes and records a patch file lying under `root` (the patch directory).
/// `drop_duplicate`: the file is this app's own fresh download -- when the same archive is
/// already recorded for another hash, the new copy is removed and the record points at the
/// existing one.
pub async fn record(pool: &MySqlPool, root: &Path, file: &Path, target: &PatchTarget, drop_duplicate: bool) -> Result<RecordSummary, DatabaseError> {
    let owned = file.to_path_buf();
    let (whole, format, members) = tokio::task::spawn_blocking(move || hash_patch_file(&owned)).await.map_err(|e| DatabaseError::Server(format!("hash task: {e}")))??;
    let size = std::fs::metadata(file)?.len();
    let file_name = file.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
    let (archive_id, _) = content::upsert_content(pool, target.platform, &file_name, size, &whole).await?;
    let mut rel_path = relative(root, file);
    let mut duplicate_of = None;
    let existing: Option<String> = sqlx::query_scalar("SELECT rel_path FROM ra_patches WHERE content_id = ? AND md5 <> ? LIMIT 1").bind(archive_id).bind(&target.md5).fetch_optional(pool).await?;
    if let Some(existing) = existing {
        if drop_duplicate && existing != rel_path && root.join(&existing).is_file() {
            tokio::fs::remove_file(file).await?;
            rel_path = existing.clone();
            duplicate_of = Some(existing);
        }
    }
    let mut rows = Vec::with_capacity(members.len());
    for m in &members {
        let name = base_name(&m.path).to_string();
        let child = if format.is_some() {
            let (id, _) = content::upsert_content(pool, target.platform, &name, m.size.unwrap_or(0), &m.report).await?;
            content::link_member(pool, archive_id, id, Some(&m.path)).await?;
            id
        } else {
            archive_id
        };
        rows.push((m.path.clone(), patch_kind(&m.path), extension(&name), m.size, child));
    }
    let (stem, consistent) = common_stem(&members.iter().map(|m| m.path.clone()).collect::<Vec<_>>());
    let patch_id = sqlx::query(
        "INSERT INTO ra_patches (ra_game_id, md5, release_id, patch_url, rel_path, archive_format, size, content_id, patched_name, patch_rom_name, names_consistent, downloaded_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NOW()) ON DUPLICATE KEY UPDATE ra_game_id = VALUES(ra_game_id), release_id = VALUES(release_id), patch_url = VALUES(patch_url), \
         rel_path = VALUES(rel_path), archive_format = VALUES(archive_format), size = VALUES(size), content_id = VALUES(content_id), patched_name = VALUES(patched_name), \
         patch_rom_name = VALUES(patch_rom_name), names_consistent = VALUES(names_consistent), downloaded_at = NOW(), id = LAST_INSERT_ID(id)",
    )
    .bind(target.ra_game_id)
    .bind(&target.md5)
    .bind(target.release_id)
    .bind(&target.patch_url)
    .bind(&rel_path)
    .bind(format)
    .bind(size)
    .bind(archive_id)
    .bind(target.patched_name.chars().take(512).collect::<String>())
    .bind(stem.as_ref().map(|s| s.chars().take(512).collect::<String>()))
    .bind(consistent)
    .execute(pool)
    .await?
    .last_insert_id();
    sqlx::query("DELETE FROM ra_patch_files WHERE patch_id = ?").bind(patch_id).execute(pool).await?;
    for (path, kind, ext, size, content_id) in &rows {
        sqlx::query("INSERT IGNORE INTO ra_patch_files (patch_id, member_path, kind, ext, size, content_id) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(patch_id)
            .bind(path.chars().take(512).collect::<String>())
            .bind(kind.name())
            .bind(ext)
            .bind(size)
            .bind(content_id)
            .execute(pool)
            .await?;
    }
    Ok(RecordSummary { patch_id, rel_path, members: rows.len(), patch_rom_name: stem, names_consistent: consistent, duplicate_of })
}

/// What the RA hash list says about `md5`, and the release / platform it belongs to.
pub async fn target_for(pool: &MySqlPool, md5: &str) -> Result<PatchTarget, DatabaseError> {
    let md5 = md5.trim().to_ascii_lowercase();
    let row = sqlx::query("SELECT ra_game_id, name, patch_url FROM ra_game_hashes WHERE md5 = ?")
        .bind(&md5)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| DatabaseError::Config(format!("RetroAchievements lists no hash {md5} (open the game's page first)")))?;
    let ra_game_id: u32 = row.try_get("ra_game_id")?;
    let ra_name: Option<String> = row.try_get("name")?;
    let patch_url: String = row.try_get::<Option<String>, _>("patch_url")?.ok_or_else(|| DatabaseError::Config(format!("RetroAchievements has no patch for hash {md5}")))?;
    let release = sqlx::query(
        "SELECT r.id, r.name, r.app_platform_id FROM hashes h JOIN release_contents rc ON rc.content_id = h.content_id JOIN `release` r ON r.id = rc.release_id AND r.deleted = FALSE \
         WHERE h.algorithm IN ('ra_hash', 'md5') AND h.value_hex = ? ORDER BY r.id LIMIT 1",
    )
    .bind(&md5)
    .fetch_optional(pool)
    .await?;
    let release_id: Option<u64> = release.as_ref().map(|r| r.try_get("id")).transpose()?;
    let release_name: Option<String> = release.as_ref().map(|r| r.try_get("name")).transpose()?;
    let mut platform = release.as_ref().and_then(|r| r.try_get::<u16, _>("app_platform_id").ok()).and_then(Platform::from_igdb_id).unwrap_or(Platform::Unknown);
    if platform == Platform::Unknown {
        let console: Option<String> = sqlx::query_scalar("SELECT console_name FROM retroachievements_games WHERE provider_id = ?").bind(ra_game_id).fetch_optional(pool).await?.flatten();
        if let Some(c) = console {
            platform = super::platform_map::platform_from_ra_console_name(&c);
        }
    }
    let patched_name = release_name.or(ra_name).unwrap_or_else(|| md5.clone());
    Ok(PatchTarget { ra_game_id, md5, patch_url, patched_name, release_id, platform })
}

/// A path under `dir` for `name` that doesn't clobber a file this app didn't write:
/// `name`, or `name (2)` ... when a different file sits there. `ours`: the path already
/// recorded for this hash (re-downloading may replace it).
fn free_path(dir: &Path, name: &str, ours: Option<&Path>) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() || ours == Some(first.as_path()) {
        return first;
    }
    let p = Path::new(name);
    let (stem, ext) = (p.file_stem().and_then(|s| s.to_str()).unwrap_or(name), p.extension().and_then(|e| e.to_str()));
    for n in 2..1000 {
        let candidate = dir.join(match ext {
            Some(e) => format!("{stem} ({n}).{e}"),
            None => format!("{stem} ({n})"),
        });
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{stem}-{}", chrono::Utc::now().timestamp()))
}

/// Downloads the patch for RA hash `md5` into the patch directory and records it.
pub async fn download(pool: &MySqlPool, http: &reqwest::Client, md5: &str, progress: Option<&Progress>) -> Result<RecordSummary, DatabaseError> {
    use tokio::io::AsyncWriteExt;
    let target = target_for(pool, md5).await?;
    if !allowed_url(&target.patch_url) {
        return Err(DatabaseError::Config(format!("refusing patch URL {} (only https from RetroAchievements or GitHub)", target.patch_url)));
    }
    let root = super::directories::patch_dir(pool).await?.ok_or_else(|| DatabaseError::Config("no patch directory: add a directory of type \"patch\" in Config > Directories".to_string()))?;
    let dir = root.join(platform_folder(target.platform));
    tokio::fs::create_dir_all(&dir).await?;
    let ours: Option<String> = sqlx::query_scalar("SELECT rel_path FROM ra_patches WHERE md5 = ?").bind(&target.md5).fetch_optional(pool).await?;
    let ours = ours.map(|r| root.join(r));
    let dest = free_path(&dir, &file_name_from_url(&target.patch_url, &target.md5), ours.as_deref());

    let mut response = http.get(&target.patch_url).send().await.map_err(|e| DatabaseError::Fetch(format!("{}: {e}", target.patch_url)))?.error_for_status().map_err(|e| DatabaseError::Fetch(format!("{}: {e}", target.patch_url)))?;
    let expected = response.content_length();
    if expected.is_some_and(|n| n > MAX_BYTES) {
        return Err(DatabaseError::Fetch(format!("patch is {} bytes, over the {} limit", expected.unwrap_or(0), MAX_BYTES)));
    }
    let part = dest.with_file_name(format!("{}.part", dest.file_name().and_then(|n| n.to_str()).unwrap_or("patch")));
    let mut file = tokio::fs::File::create(&part).await?;
    let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut total = 0u64;
    let streamed: Result<(), DatabaseError> = async {
        while let Some(chunk) = response.chunk().await.map_err(|e| DatabaseError::Fetch(format!("{}: {e}", target.patch_url)))? {
            total += chunk.len() as u64;
            if total > MAX_BYTES {
                return Err(DatabaseError::Fetch(format!("patch is over the {MAX_BYTES} byte limit")));
            }
            file.write_all(&chunk).await?;
            if let Some(p) = progress {
                p.report(total, expected, &format!("downloading {name}: {}", super::jobs::human_bytes(total))).await;
            }
        }
        file.flush().await?;
        Ok(())
    }
    .await;
    drop(file);
    if let Err(e) = streamed {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(e);
    }
    tokio::fs::rename(&part, &dest).await?;
    if let Some(p) = progress {
        p.force(total, Some(total), &format!("hashing {name}")).await;
    }
    let summary = record(pool, &root, &dest, &target, true).await?;
    log::info!("ra patch {}: {} ({} member(s){})", target.md5, summary.rel_path, summary.members, summary.duplicate_of.as_ref().map(|d| format!(", same archive as {d}")).unwrap_or_default());
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zipped patch: the zip's own bytes are its hashes, and its member is hashed from the
    /// one decode -- the same pass, not a raw read followed by a second decode.
    #[test]
    fn a_zipped_patch_is_hashed_as_itself_and_its_member() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("ra_patch_hash_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let patch: Vec<u8> = (0..3000u32).map(|i| (i * 13 % 256) as u8).collect();
        let path = dir.join("Game (Japan) [T-En].zip");
        {
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            zip.start_file("Game (Japan) [T-En].bps", zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(&patch).unwrap();
            zip.finish().unwrap();
        }
        let (whole, format, members) = hash_patch_file(&path).unwrap();
        assert_eq!(format, Some("zip"));
        assert_eq!(whole.sha1, hash::hash_bytes(HashFlags::SHA1, &std::fs::read(&path).unwrap()).sha1, "the zip's own bytes");
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].path, "Game (Japan) [T-En].bps");
        assert_eq!(members[0].report.sha1, hash::hash_bytes(HashFlags::SHA1, &patch).sha1, "the member inside");

        let bare = dir.join("Other.bps");
        std::fs::write(&bare, &patch).unwrap();
        let (whole, format, members) = hash_patch_file(&bare).unwrap();
        assert_eq!((format, members.len()), (None, 1));
        assert_eq!(whole.sha1, members[0].report.sha1, "a bare patch is its own member");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn classifies_members() {
        assert_eq!(patch_kind("Game (Japan) [T-En].xdelta"), PatchKind::Patch);
        assert_eq!(patch_kind("dir/Game.BPS"), PatchKind::Patch);
        assert_eq!(patch_kind("Game (Japan) [T-En].cue"), PatchKind::Sheet);
        assert_eq!(patch_kind("readme.txt"), PatchKind::Readme);
        assert_eq!(patch_kind("Readme.PDF"), PatchKind::Readme);
        assert_eq!(patch_kind("data.bin"), PatchKind::Other);
    }

    #[test]
    fn stems_agree_or_are_flagged() {
        let ok = vec!["Game (Japan) [T-En].xdelta".to_string(), "Game (Japan) [T-En].cue".to_string(), "readme.txt".to_string()];
        assert_eq!(common_stem(&ok), (Some("Game (Japan) [T-En]".to_string()), true));
        let bad = vec!["Game A.ips".to_string(), "Game B.cue".to_string(), "Game A.bps".to_string()];
        assert_eq!(common_stem(&bad), (Some("Game A".to_string()), false));
        assert_eq!(common_stem(&["readme.txt".to_string()]), (None, true));
    }

    #[test]
    fn only_trusted_https_urls() {
        assert!(allowed_url("https://github.com/RetroAchievements/RAPatches/raw/main/NES/Translation/English/1234-Game.zip"));
        assert!(allowed_url("https://raw.githubusercontent.com/x/y/main/z.7z"));
        assert!(allowed_url("https://retroachievements.org/some/patch.zip"));
        assert!(!allowed_url("http://github.com/x.zip"), "plain http");
        assert!(!allowed_url("https://evilgithub.com/x.zip"));
        assert!(!allowed_url("https://github.com.evil.example/x.zip"));
        assert!(!allowed_url("file:///etc/passwd"));
    }

    #[test]
    fn names_and_folders() {
        assert_eq!(platform_folder(Platform::PlayStation2), "PlayStation2");
        assert_eq!(file_name_from_url("https://github.com/a/b/raw/main/SNES/12%20Game%20(Japan).zip", "x"), "12 Game (Japan).zip");
        assert_eq!(file_name_from_url("https://github.com/a/b/raw/main/bad%3Aname.7z", "x"), "bad_name.7z");
        assert_eq!(file_name_from_url("https://github.com/", "abc"), "abc.zip");
    }
}
