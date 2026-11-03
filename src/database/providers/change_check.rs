//! Cheap "has the provider's data changed since our last sync?" checks, for the `check`
//! trigger of `schedules`. Each is one small request (a count, a conditional request, a
//! checksum list) -- never a download of the data itself:
//!
//! | provider | check |
//! |---|---|
//! | IGDB | `games/count where updated_at > <last complete sync>` (per platform, or overall) |
//! | TheGamesDB | conditional request on the JSON dump (304 = unchanged) |
//! | Hasheous | `Dumps/platforms?includeDetails` md5s vs the zips last processed |
//! | EmuReady | the system's game count vs the last complete sync |
//! | RetroAchievements | none exists (no count or last-updated endpoint): `Unknown` |
//! | DAT sources | the `dat_sync` job itself only fetches what's new, so it *is* the check |

use sqlx::mysql::MySqlPool;

use crate::common::models::Platform;

use super::super::error::DatabaseError;
use super::{cache, dumps, sync_state, Provider, ProviderClients};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckResult {
    Changed(String),
    Unchanged(String),
    /// No cheap check exists (or it failed): the schedule's fixed trigger is what runs it.
    Unknown(String),
}

impl CheckResult {
    pub fn summary(&self) -> String {
        match self {
            Self::Changed(s) => format!("changed: {s}"),
            Self::Unchanged(s) => format!("unchanged: {s}"),
            Self::Unknown(s) => format!("unknown: {s}"),
        }
    }
}

/// Checks a schedule target (a job payload). DAT syncs report `Changed` because the sync
/// itself is the cheap check.
pub async fn check(clients: &ProviderClients, http: &reqwest::Client, pool: &MySqlPool, target: &serde_json::Value) -> Result<CheckResult, DatabaseError> {
    match target.get("job_type").and_then(|v| v.as_str()) {
        Some("dat_sync") => Ok(CheckResult::Changed("DAT sources are checked by the sync itself (existing files are skipped)".to_string())),
        Some("provider_bulk_sync") => {
            let provider = target.get("provider").and_then(|v| v.as_str()).and_then(Provider::from_name).ok_or_else(|| DatabaseError::Config("schedule target: unknown provider".to_string()))?;
            let full = target.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
            let platform = target.get("platform").and_then(|v| v.as_u64()).and_then(|id| Platform::from_igdb_id(id as u16));
            check_provider(clients, http, pool, provider, if full { None } else { platform }).await
        }
        other => Ok(CheckResult::Unknown(format!("no change check for {}", other.unwrap_or("?")))),
    }
}

async fn check_provider(clients: &ProviderClients, http: &reqwest::Client, pool: &MySqlPool, provider: Provider, platform: Option<Platform>) -> Result<CheckResult, DatabaseError> {
    match provider {
        Provider::Igdb => {
            let Some(client) = clients.igdb.as_ref() else { return Ok(CheckResult::Unknown("igdb client unavailable".to_string())) };
            let state = sync_state::get(pool, Provider::Igdb, platform).await?;
            // Overall: the oldest complete platform sync is the safe "since".
            let since = match (platform, state) {
                (Some(_), Some(s)) if s.complete => s.synced_at,
                (Some(_), _) => return Ok(CheckResult::Changed("never synced completely".to_string())),
                (None, _) => match sync_state::all(pool).await?.into_iter().filter(|s| s.provider == "igdb" && s.app_platform_id.is_some() && s.complete).map(|s| s.synced_at).min() {
                    Some(t) => t,
                    None => return Ok(CheckResult::Changed("never synced completely".to_string())),
                },
            };
            let igdb_platform = platform.map(|p| cache::sibling_igdb_platforms(p.igdb_id())[0] as i32);
            match client.count_games(igdb_platform, Some(since.and_utc().timestamp())).await {
                Ok(0) => Ok(CheckResult::Unchanged(format!("no game changed since {since}"))),
                Ok(n) => Ok(CheckResult::Changed(format!("{n} game(s) changed since {since}"))),
                Err(e) => Ok(CheckResult::Unknown(format!("count failed: {e}"))),
            }
        }
        Provider::TheGamesDb => {
            let known = dumps::dump_file(pool, Provider::TheGamesDb, "database-latest.json").await?;
            let Some(known) = known else { return Ok(CheckResult::Changed("dump never imported".to_string())) };
            // HEAD and compare validators: TheGamesDB's CDN answers conditional GETs with 304
            // only on an exact date match, so asking for its current Last-Modified / ETag is
            // the reliable (and cheapest) check.
            match http.head(dumps::THEGAMESDB_DUMP_URL).send().await {
                Ok(r) if r.status().is_success() => {
                    let header = |name| r.headers().get(name).and_then(|v: &reqwest::header::HeaderValue| v.to_str().ok()).map(str::to_string);
                    let (etag, last_modified) = (header(reqwest::header::ETAG), header(reqwest::header::LAST_MODIFIED));
                    let same = (known.last_modified.is_some() && known.last_modified == last_modified) || (known.last_modified.is_none() && known.etag.is_some() && known.etag == etag);
                    if same {
                        Ok(CheckResult::Unchanged(format!("dump unchanged since {}", known.last_modified.unwrap_or_default())))
                    } else {
                        Ok(CheckResult::Changed(format!("new dump ({})", last_modified.as_deref().unwrap_or("?"))))
                    }
                }
                Ok(r) => Ok(CheckResult::Unknown(format!("HTTP {}", r.status()))),
                Err(e) => Ok(CheckResult::Unknown(e.to_string())),
            }
        }
        Provider::Hasheous => {
            let zips: Vec<dumps::HasheousZipInfo> = match http.get(format!("{}/platforms?includeDetails=true", dumps::HASHEOUS_DUMPS_URL)).send().await {
                Ok(r) => match r.error_for_status() {
                    Ok(r) => r.json().await?,
                    Err(e) => return Ok(CheckResult::Unknown(e.to_string())),
                },
                Err(e) => return Ok(CheckResult::Unknown(e.to_string())),
            };
            let mut changed = 0usize;
            let mut considered = 0usize;
            for z in &zips {
                if let Some(p) = platform {
                    if super::super::platform_map::platform_from_dat_name(z.name.trim_end_matches(".zip")) != p {
                        continue;
                    }
                }
                considered += 1;
                let known = dumps::dump_file(pool, Provider::Hasheous, &z.name).await?;
                if known.map_or(true, |k| k.md5.is_none() || k.md5 != z.md5) {
                    changed += 1;
                }
            }
            Ok(if changed > 0 { CheckResult::Changed(format!("{changed} of {considered} zip(s) new or changed")) } else { CheckResult::Unchanged(format!("all {considered} zip(s) unchanged")) })
        }
        Provider::EmuReady => {
            let Some(p) = platform else { return Ok(CheckResult::Unknown("EmuReady is checked per platform".to_string())) };
            let Some(client) = clients.emuready.as_ref() else { return Ok(CheckResult::Unknown("emuready client unavailable".to_string())) };
            let Some(system) = cache::emuready_system_id(clients, p).await? else { return Ok(CheckResult::Unknown(format!("no EmuReady system for {p:?}"))) };
            let previous = sync_state::get(pool, Provider::EmuReady, Some(p)).await?;
            match (client.count_games(&system).await, previous.filter(|s| s.complete)) {
                (Ok(Some(n)), Some(prev)) if n.max(0) as u64 == prev.entries => Ok(CheckResult::Unchanged(format!("{n} games, same as last sync"))),
                (Ok(Some(n)), _) => Ok(CheckResult::Changed(format!("{n} games now"))),
                (Ok(None), _) => Ok(CheckResult::Unknown("no count in response".to_string())),
                (Err(e), _) => Ok(CheckResult::Unknown(e.to_string())),
            }
        }
        Provider::RetroAchievements => Ok(CheckResult::Unknown("RetroAchievements has no cheap change check; use a fixed interval (a diff sync only fetches hashes for consoles that changed)".to_string())),
        other => Ok(CheckResult::Unknown(format!("{} has no bulk sync", other.name()))),
    }
}
