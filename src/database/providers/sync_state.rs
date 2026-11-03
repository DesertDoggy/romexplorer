//! `provider_sync_state`: what each bulk sync covered -- per provider and app platform, the
//! entries it fetched, the provider's own total when known, and whether it got the whole
//! list. Written at the end of every per-platform/full sync (`bulk`, `dumps`), read by
//! `api::providers` for the console's coverage figures. Platform `None` (stored as 0) is
//! the provider as a whole.

use chrono::NaiveDateTime;
use serde::Serialize;
use sqlx::mysql::MySqlPool;
use sqlx::Row;

use crate::common::models::Platform;

use super::super::error::DatabaseError;
use super::Provider;

#[derive(Debug, Clone, Serialize)]
pub struct SyncState {
    pub provider: String,
    /// `None` = the provider as a whole.
    pub app_platform_id: Option<u16>,
    pub remote_id: Option<String>,
    pub entries: u64,
    pub remote_total: Option<u64>,
    pub complete: bool,
    pub synced_at: NaiveDateTime,
}

/// Records one sync's coverage. A `None` `remote_total` keeps a total an earlier sync
/// learned (a TheGamesDB platform page sync after the full dump), so the percentage stays
/// computable; and a partial sync never downgrades a platform an earlier sync got
/// completely (a capped page sync after the full dump: everything is still cached). The
/// `complete` assignment comes last because MySQL applies them left to right.
pub async fn record(pool: &MySqlPool, provider: Provider, platform: Option<Platform>, remote_id: Option<&str>, entries: u64, remote_total: Option<u64>, complete: bool) -> Result<(), DatabaseError> {
    sqlx::query(
        "INSERT INTO provider_sync_state (provider, app_platform_id, remote_id, entries, remote_total, complete, synced_at) VALUES (?, ?, ?, ?, ?, ?, NOW()) \
         ON DUPLICATE KEY UPDATE remote_id = COALESCE(VALUES(remote_id), remote_id), \
         entries = IF(complete AND NOT VALUES(complete), entries, VALUES(entries)), \
         remote_total = COALESCE(VALUES(remote_total), remote_total), \
         synced_at = IF(complete AND NOT VALUES(complete), synced_at, NOW()), \
         complete = complete OR VALUES(complete)",
    )
    .bind(provider.name())
    .bind(platform.map_or(0, |p| p.igdb_id()))
    .bind(remote_id)
    .bind(entries)
    .bind(remote_total)
    .bind(complete)
    .execute(pool)
    .await?;
    Ok(())
}

/// Only the provider's overall total (IGDB's `games/count`), leaving a full-sync record's
/// entries/complete alone.
pub async fn record_total(pool: &MySqlPool, provider: Provider, remote_total: u64) -> Result<(), DatabaseError> {
    sqlx::query("INSERT INTO provider_sync_state (provider, app_platform_id, remote_total, synced_at) VALUES (?, 0, ?, NOW()) ON DUPLICATE KEY UPDATE remote_total = VALUES(remote_total)")
        .bind(provider.name())
        .bind(remote_total)
        .execute(pool)
        .await?;
    Ok(())
}

/// The record for one provider + platform (`None` = the provider as a whole).
pub async fn get(pool: &MySqlPool, provider: Provider, platform: Option<Platform>) -> Result<Option<SyncState>, DatabaseError> {
    let id = platform.map_or(0, |p| p.igdb_id());
    Ok(all(pool).await?.into_iter().find(|s| s.provider == provider.name() && s.app_platform_id.unwrap_or(0) == id))
}

pub async fn all(pool: &MySqlPool) -> Result<Vec<SyncState>, DatabaseError> {
    let rows = sqlx::query("SELECT provider, app_platform_id, remote_id, entries, remote_total, complete, synced_at FROM provider_sync_state ORDER BY provider, app_platform_id").fetch_all(pool).await?;
    rows.iter()
        .map(|r| {
            let platform: u16 = r.try_get("app_platform_id")?;
            Ok(SyncState {
                provider: r.try_get("provider")?,
                app_platform_id: (platform != 0).then_some(platform),
                remote_id: r.try_get("remote_id")?,
                entries: r.try_get::<u32, _>("entries")? as u64,
                remote_total: r.try_get::<Option<u32>, _>("remote_total")?.map(u64::from),
                complete: r.try_get("complete")?,
                synced_at: r.try_get("synced_at")?,
            })
        })
        .collect()
}
