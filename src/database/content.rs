//! `content` rows and their `hashes`, shared by the file scan and the patch downloads: a
//! file's bytes are one content row whatever its name or place, found again by its
//! strongest hash (sha256 > sha1 > md5) -- the same dedup rule `import.rs` applies to DAT
//! lines, so a scanned file lands on the DAT's own content row when it matches.

use sqlx::mysql::MySqlPool;
use sqlx::Row;

use super::error::DatabaseError;
use crate::common::models::Platform;
use crate::rom::hash::HashReport;

/// The hash values of a report as (algorithm, lowercase hex) pairs.
fn values(report: &HashReport) -> [(&'static str, &Option<String>); 4] {
    [("crc32", &report.crc32), ("md5", &report.md5), ("sha1", &report.sha1), ("sha256", &report.sha256)]
}

/// Records a report's hashes for `content_id` (existing ones are kept).
pub async fn insert_hashes(pool: &MySqlPool, content_id: u64, report: &HashReport) -> Result<(), DatabaseError> {
    for (algorithm, value) in values(report) {
        if let Some(v) = value {
            sqlx::query("INSERT IGNORE INTO hashes (content_id, algorithm, value_hex) VALUES (?, ?, ?)").bind(content_id).bind(algorithm).bind(v.to_ascii_lowercase()).execute(pool).await?;
        }
    }
    Ok(())
}

/// The existing `content` row with the same strongest hash, else a new one. Returns the id
/// and whether it already existed.
pub async fn upsert_content(pool: &MySqlPool, platform: Platform, file_name: &str, size: u64, report: &HashReport) -> Result<(u64, bool), DatabaseError> {
    for (algorithm, value) in [("sha256", &report.sha256), ("sha1", &report.sha1), ("md5", &report.md5)] {
        let Some(v) = value else { continue };
        let row = sqlx::query("SELECT content_id FROM hashes WHERE algorithm = ? AND value_hex = ? AND content_id IS NOT NULL LIMIT 1").bind(algorithm).bind(v.to_ascii_lowercase()).fetch_optional(pool).await?;
        if let Some(row) = row {
            let id: u64 = row.try_get("content_id")?;
            insert_hashes(pool, id, report).await?;
            if platform != Platform::Unknown {
                sqlx::query("UPDATE content SET app_platform_id = ? WHERE id = ? AND app_platform_id = 0").bind(platform.igdb_id()).bind(id).execute(pool).await?;
            }
            sqlx::query("UPDATE content SET size = ? WHERE id = ? AND size IS NULL").bind(size).bind(id).execute(pool).await?;
            return Ok((id, true));
        }
    }
    // Hashes set to crc32 only (md5, sha1 and sha256 all turned off): the crc32 and the size
    // together, or every rescan made every file new content and dropped its DAT links.
    if report.sha256.is_none() && report.sha1.is_none() && report.md5.is_none() {
        if let Some(crc) = &report.crc32 {
            let row = sqlx::query("SELECT h.content_id FROM hashes h JOIN content c ON c.id = h.content_id WHERE h.algorithm = 'crc32' AND h.value_hex = ? AND c.size = ? LIMIT 1")
                .bind(crc.to_ascii_lowercase())
                .bind(size)
                .fetch_optional(pool)
                .await?;
            if let Some(row) = row {
                let id: u64 = row.try_get("content_id")?;
                insert_hashes(pool, id, report).await?;
                if platform != Platform::Unknown {
                    sqlx::query("UPDATE content SET app_platform_id = ? WHERE id = ? AND app_platform_id = 0").bind(platform.igdb_id()).bind(id).execute(pool).await?;
                }
                return Ok((id, true));
            }
        }
    }
    let result = sqlx::query("INSERT INTO content (app_platform_id, file_name, size, partial) VALUES (?, ?, ?, FALSE)").bind(platform.igdb_id()).bind(file_name).bind(size).execute(pool).await?;
    let id = result.last_insert_id();
    insert_hashes(pool, id, report).await?;
    Ok((id, false))
}

/// Drops `parent`'s member links to anything not in `keep` -- after its members were
/// recorded afresh, so the junction says what the container holds now, not also what an
/// earlier convention (another sheet style, another member name) said it held.
pub async fn unlink_members_except(pool: &MySqlPool, parent: u64, keep: &[u64]) -> Result<(), DatabaseError> {
    if keep.is_empty() {
        sqlx::query("DELETE FROM content_contents WHERE parent_content_id = ?").bind(parent).execute(pool).await?;
        return Ok(());
    }
    let list = keep.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM content_contents WHERE parent_content_id = ? AND child_content_id NOT IN ({list})"))).bind(parent).execute(pool).await?;
    Ok(())
}

/// Records that archive `parent` contains member `child`, at `member_path` inside it.
pub async fn link_member(pool: &MySqlPool, parent: u64, child: u64, member_path: Option<&str>) -> Result<(), DatabaseError> {
    if parent != child {
        sqlx::query(
            "INSERT INTO content_contents (parent_content_id, child_content_id, member_path) VALUES (?, ?, ?) \
             ON DUPLICATE KEY UPDATE member_path = COALESCE(VALUES(member_path), member_path)",
        )
        .bind(parent)
        .bind(child)
        .bind(member_path.map(|p| p.chars().take(512).collect::<String>()))
        .execute(pool)
        .await?;
    }
    Ok(())
}
