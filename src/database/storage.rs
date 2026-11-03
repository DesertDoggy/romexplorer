//! `media_dirs` / `temp_dirs`: app-infrastructure storage locations, independent of any
//! collection root (docs/plan/database.md). The "at most one active tier1/tier2 row" rule
//! lives here, in Rust, not in the schema.

use serde::{Deserialize, Serialize};
use sqlx::mysql::MySqlPool;
use sqlx::Row;

use super::error::DatabaseError;

pub const MEDIA_ROLES: [&str; 5] = ["tier1_cache", "tier2_cache", "image", "video", "audio"];
/// Roles limited to one active row.
pub const SINGLE_ROLES: [&str; 2] = ["tier1_cache", "tier2_cache"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaDir {
    pub id: u64,
    pub path: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TempDir {
    pub id: u64,
    pub path: String,
    pub tier: u8,
}

pub async fn list_media_dirs(pool: &MySqlPool) -> Result<Vec<MediaDir>, DatabaseError> {
    let rows = sqlx::query("SELECT id, path, role FROM media_dirs WHERE deleted = FALSE ORDER BY role, id").fetch_all(pool).await?;
    rows.iter().map(|r| Ok(MediaDir { id: r.try_get("id")?, path: r.try_get("path")?, role: r.try_get("role")? })).collect()
}

/// Adds a media dir. For the single-row cache roles the existing active row (if any) is
/// soft-deleted first, so "set tier 1 cache" replaces rather than duplicates.
pub async fn add_media_dir(pool: &MySqlPool, role: &str, path: &str) -> Result<u64, DatabaseError> {
    if !MEDIA_ROLES.contains(&role) {
        return Err(DatabaseError::Config(format!("unknown media role {role:?}")));
    }
    let mut tx = pool.begin().await?;
    if SINGLE_ROLES.contains(&role) {
        sqlx::query("UPDATE media_dirs SET deleted = TRUE WHERE role = ? AND deleted = FALSE").bind(role).execute(&mut *tx).await?;
    }
    let result = sqlx::query("INSERT INTO media_dirs (path, role) VALUES (?, ?)").bind(path).bind(role).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(result.last_insert_id())
}

pub async fn remove_media_dir(pool: &MySqlPool, id: u64) -> Result<bool, DatabaseError> {
    let result = sqlx::query("UPDATE media_dirs SET deleted = TRUE WHERE id = ? AND deleted = FALSE").bind(id).execute(pool).await?;
    Ok(result.rows_affected() > 0)
}

pub async fn list_temp_dirs(pool: &MySqlPool) -> Result<Vec<TempDir>, DatabaseError> {
    let rows = sqlx::query("SELECT id, path, tier FROM temp_dirs WHERE deleted = FALSE ORDER BY tier, id").fetch_all(pool).await?;
    rows.iter().map(|r| Ok(TempDir { id: r.try_get("id")?, path: r.try_get("path")?, tier: r.try_get("tier")? })).collect()
}

/// Sets the tier's directory, replacing any active row for that tier.
pub async fn set_temp_dir(pool: &MySqlPool, tier: u8, path: &str) -> Result<u64, DatabaseError> {
    if !(1..=2).contains(&tier) {
        return Err(DatabaseError::Config(format!("temp dir tier must be 1 or 2, got {tier}")));
    }
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE temp_dirs SET deleted = TRUE WHERE tier = ? AND deleted = FALSE").bind(tier).execute(&mut *tx).await?;
    let result = sqlx::query("INSERT INTO temp_dirs (path, tier) VALUES (?, ?)").bind(path).bind(tier).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(result.last_insert_id())
}

pub async fn remove_temp_dir(pool: &MySqlPool, id: u64) -> Result<bool, DatabaseError> {
    let result = sqlx::query("UPDATE temp_dirs SET deleted = TRUE WHERE id = ? AND deleted = FALSE").bind(id).execute(pool).await?;
    Ok(result.rows_affected() > 0)
}

/// Tier 1 first, then tier 2 -- the first configured temp dir that exists on disk.
pub async fn preferred_temp_dir(pool: &MySqlPool) -> Result<Option<TempDir>, DatabaseError> {
    Ok(list_temp_dirs(pool).await?.into_iter().find(|t| std::path::Path::new(&t.path).is_dir()))
}
