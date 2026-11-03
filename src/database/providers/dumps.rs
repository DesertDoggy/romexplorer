//! Whole-database imports from official dump files, one download each, no per-item API
//! calls (the `full` scope of `provider_bulk_sync`, and Hasheous' per-platform dumps):
//!
//! - **TheGamesDB** publishes its whole games table as one JSON file
//!   (`cdn.thegamesdb.net/json/database-latest.json`, ~146 MB, ~121k games). It costs no
//!   API allowance. Every game goes into `thegamesdb_games` (`raw_format = 'dump'`, the path
//!   pointing at the dump file itself rather than 121k per-game files), then links to the
//!   catalog by exact name per platform (`bulk::link_exact_names`).
//! - **Hasheous** publishes its hash->metadata map: `Dumps/MetadataMap.zip` (~2.6 GB, whole
//!   map) or `Dumps/platforms/<name>.zip`, one JSON per game with its ROMs' MD5/SHA-1 and
//!   the ids it has mapped at IGDB/TheGamesDB/RetroAchievements/SteamGridDB/... Every ROM
//!   whose hash the catalog has gets those ids linked exactly (`Confidence::Hash`, the same
//!   as a live Hasheous lookup, and recorded in `hasheous_lookups` so no live lookup
//!   repeats it). The IGDB ids found are then queued as `provider_query` jobs, so titles
//!   get IGDB's metadata for the exact game rather than a name search's guess.
//!
//! Downloads stream to `<provider cache>/<provider>/...` through a `.part` file, reporting
//! bytes through the job's `Progress`.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use serde::Deserialize;
use sqlx::mysql::MySqlPool;
use sqlx::Row;

use crate::common::models::Platform;

use super::super::error::DatabaseError;
use super::super::jobs::{self, JobType, Progress};
use super::super::platform_map;
use super::bulk::{self, BulkSummary};
use super::cache::{self, Confidence};
use super::raw_store::RawStore;
use super::sync_state;
use super::{Provider, ProviderClients};

pub const THEGAMESDB_DUMP_URL: &str = "https://cdn.thegamesdb.net/json/database-latest.json";
pub const HASHEOUS_DUMPS_URL: &str = "https://hasheous.org/api/v1/Dumps";

/// What a remote dump file looked like when last fetched and processed
/// (`provider_dump_files`): its provider-side identity (md5 / ETag / Last-Modified / size),
/// how many games it held, and how far the catalog was when it was last matched.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DumpFile {
    pub md5: Option<String>,
    pub size: Option<u64>,
    pub last_modified: Option<String>,
    pub etag: Option<String>,
    pub entries: Option<u64>,
    pub processed_content_max: Option<u64>,
}

pub async fn dump_file(pool: &MySqlPool, provider: Provider, name: &str) -> Result<Option<DumpFile>, DatabaseError> {
    let Some(row) = sqlx::query("SELECT md5, size, last_modified, etag, entries, processed_content_max FROM provider_dump_files WHERE provider = ? AND name = ?").bind(provider.name()).bind(name).fetch_optional(pool).await? else {
        return Ok(None);
    };
    Ok(Some(DumpFile {
        md5: row.try_get("md5")?,
        size: row.try_get("size")?,
        last_modified: row.try_get("last_modified")?,
        etag: row.try_get("etag")?,
        entries: row.try_get::<Option<u32>, _>("entries")?.map(u64::from),
        processed_content_max: row.try_get("processed_content_max")?,
    }))
}

pub async fn record_dump_file(pool: &MySqlPool, provider: Provider, name: &str, f: &DumpFile) -> Result<(), DatabaseError> {
    sqlx::query(
        "INSERT INTO provider_dump_files (provider, name, md5, size, last_modified, etag, entries, processed_content_max, fetched_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, NOW()) \
         ON DUPLICATE KEY UPDATE md5 = VALUES(md5), size = VALUES(size), last_modified = VALUES(last_modified), etag = VALUES(etag), entries = VALUES(entries), processed_content_max = VALUES(processed_content_max), fetched_at = NOW()",
    )
    .bind(provider.name())
    .bind(name)
    .bind(&f.md5)
    .bind(f.size)
    .bind(&f.last_modified)
    .bind(&f.etag)
    .bind(f.entries.map(|n| n.min(u32::MAX as u64) as u32))
    .bind(f.processed_content_max)
    .execute(pool)
    .await?;
    Ok(())
}

/// `MAX(content.id)` -- "has the catalog gained content since this file was matched?".
pub async fn content_marker(pool: &MySqlPool) -> Result<u64, DatabaseError> {
    let n: Option<u64> = sqlx::query("SELECT MAX(id) AS n FROM content").fetch_one(pool).await?.try_get("n")?;
    Ok(n.unwrap_or(0))
}

/// Whether a 200's validators show the local copy is already this file.
fn same_as_local(dest: &Path, known: Option<&DumpFile>, etag: Option<&str>, last_modified: Option<&str>, length: Option<u64>) -> bool {
    if let Some(k) = known {
        if (k.last_modified.is_some() && k.last_modified.as_deref() == last_modified) || (k.etag.is_some() && k.etag.as_deref() == etag) {
            return true;
        }
        if k.last_modified.is_some() || k.etag.is_some() {
            return false;
        }
    }
    let Ok(meta) = std::fs::metadata(dest) else { return false };
    let Some(remote) = last_modified.and_then(|l| chrono::DateTime::parse_from_rfc2822(l).ok()) else { return false };
    let Ok(local) = meta.modified() else { return false };
    let local: chrono::DateTime<chrono::Utc> = local.into();
    local >= remote && length.is_none_or(|n| n == meta.len())
}

/// Outcome of [`download_if_changed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fetched {
    /// The server answered 304: the local copy is current, nothing was transferred.
    Unchanged,
    Downloaded { bytes: u64 },
}

/// Streams `url` to `dest` (through `dest.part`, renamed only when complete) unless the
/// file hasn't changed. With a local copy it asks conditionally -- the recorded
/// `Last-Modified` echoed back exactly as `If-Modified-Since` (some servers, TheGamesDB's CDN
/// included, only answer 304 to an exact match and ignore weak ETags; sending
/// `If-None-Match` as well would make them skip the date check), else the recorded ETag,
/// else the local file's time -- and a 304 is `Unchanged`. A 200 whose `Last-Modified` /
/// ETag equals the recorded one, or (nothing recorded) whose `Last-Modified` is older than
/// the local copy of the same size, is also `Unchanged`: the connection is dropped before
/// the body is read, so nothing is wasted on the provider's side. `force` always
/// downloads. Returns the outcome and the file's current ETag / Last-Modified.
pub async fn download_if_changed(http: &reqwest::Client, url: &str, dest: &Path, known: Option<&DumpFile>, force: bool, progress: &Progress, label: &str) -> Result<(Fetched, Option<String>, Option<String>), DatabaseError> {
    use reqwest::header::{ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED};
    use tokio::io::AsyncWriteExt;
    // The shared client's 120 s timeout covers the whole response, body included, so a big
    // dump (Hasheous' Commodore 64 zip is 650 MB) was cut off every time on an ordinary
    // connection. Here only a stall fails the download: headers within `DUMP_STALL`, then
    // no gap longer than that between chunks, however long the whole file takes.
    let mut request = http.get(url).timeout(DUMP_MAX_DURATION);
    if !force && dest.is_file() {
        match known {
            Some(k) if k.last_modified.is_some() || k.etag.is_some() => {
                if let Some(l) = &k.last_modified {
                    request = request.header(IF_MODIFIED_SINCE, l);
                } else if let Some(e) = &k.etag {
                    request = request.header(IF_NONE_MATCH, e);
                }
            }
            _ => {
                if let Ok(modified) = std::fs::metadata(dest).and_then(|m| m.modified()) {
                    let when: chrono::DateTime<chrono::Utc> = modified.into();
                    request = request.header(IF_MODIFIED_SINCE, when.format("%a, %d %b %Y %H:%M:%S GMT").to_string());
                }
            }
        }
    }
    let response = tokio::time::timeout(DUMP_STALL, request.send()).await.map_err(|_| stalled(label, 0))??;
    if response.status() == reqwest::StatusCode::NOT_MODIFIED {
        progress.force(1, Some(1), &format!("{label}: unchanged, not downloaded")).await;
        return Ok((Fetched::Unchanged, known.and_then(|k| k.etag.clone()), known.and_then(|k| k.last_modified.clone())));
    }
    let mut response = response.error_for_status()?;
    let header = |name| response.headers().get(name).and_then(|v: &reqwest::header::HeaderValue| v.to_str().ok()).map(str::to_string);
    let (etag, last_modified) = (header(ETAG), header(LAST_MODIFIED));
    if !force && dest.is_file() && same_as_local(dest, known, etag.as_deref(), last_modified.as_deref(), response.content_length()) {
        drop(response);
        progress.force(1, Some(1), &format!("{label}: unchanged, not downloaded")).await;
        return Ok((Fetched::Unchanged, etag, last_modified));
    }
    if let Some(dir) = dest.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let total = response.content_length();
    let part = dest.with_extension(format!("{}.part", dest.extension().and_then(|e| e.to_str()).unwrap_or("")));
    let mut file = tokio::fs::File::create(&part).await?;
    let mut done = 0u64;
    while let Some(chunk) = tokio::time::timeout(DUMP_STALL, response.chunk()).await.map_err(|_| stalled(label, done))?? {
        file.write_all(&chunk).await?;
        done += chunk.len() as u64;
        progress.report(done, total, &format!("{label}: {} of {}", mib(done), total.map(mib).unwrap_or_else(|| "?".to_string()))).await;
    }
    file.flush().await?;
    drop(file);
    tokio::fs::rename(&part, dest).await?;
    progress.force(done, Some(done), &format!("{label}: downloaded, {}", mib(done))).await;
    Ok((Fetched::Downloaded { bytes: done }, etag, last_modified))
}

/// How long a dump download may go without receiving anything before it is given up.
const DUMP_STALL: std::time::Duration = std::time::Duration::from_secs(120);
/// Backstop for a download that keeps trickling: 650 MB at 64 KB/s still fits.
const DUMP_MAX_DURATION: std::time::Duration = std::time::Duration::from_secs(3 * 60 * 60);

fn stalled(label: &str, done: u64) -> DatabaseError {
    DatabaseError::Fetch(format!("{label}: no data for {} s after {}", DUMP_STALL.as_secs(), mib(done)))
}

fn mib(bytes: u64) -> String {
    jobs::human_bytes(bytes)
}

// ---------------------------------------------------------------------------
// TheGamesDB
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TgdbDump {
    #[serde(default)]
    last_edit_id: Option<i64>,
    data: TgdbDumpData,
}

#[derive(Deserialize)]
struct TgdbDumpData {
    games: Vec<playmatch_deredis::providers::thegamesdb::model::TgdbApiGame>,
}

/// `full` sync for TheGamesDB: the official JSON dump, cached, then linked per platform by
/// exact name. The only API call is the (cached) platform list for mapping platform ids.
///
/// `SyncMode::Diff` (default) asks for the dump conditionally: when TheGamesDB hasn't
/// published a new one since the last import (a 304), nothing is downloaded or re-imported.
pub async fn sync_thegamesdb_full(clients: &ProviderClients, http: &reqwest::Client, pool: &MySqlPool, store: &RawStore, mode: bulk::SyncMode, progress: &Progress) -> Result<BulkSummary, DatabaseError> {
    const NAME: &str = "database-latest.json";
    let path = store.root().join("thegamesdb").join(NAME);
    let known = dump_file(pool, Provider::TheGamesDb, NAME).await?;
    let (fetched, etag, last_modified) = download_if_changed(http, THEGAMESDB_DUMP_URL, &path, known.as_ref(), mode == bulk::SyncMode::Redownload, progress, "TheGamesDB dump").await?;
    // Rows cached before the platform (or region) columns existed are refilled from the local
    // dump once, without downloading anything -- and relinked, now by region.
    let backfill = fetched == Fetched::Unchanged
        && path.is_file()
        && sqlx::query("SELECT 1 FROM thegamesdb_games WHERE raw_format = 'dump' AND (remote_platform_id IS NULL OR country_id IS NULL) LIMIT 1").fetch_optional(pool).await?.is_some();
    if fetched == Fetched::Unchanged && mode == bulk::SyncMode::Diff && !backfill {
        progress.force(1, Some(1), &format!("TheGamesDB dump unchanged since {} -- nothing to import", last_modified.as_deref().unwrap_or("the last sync"))).await;
        return Ok(BulkSummary { provider: Some(Provider::TheGamesDb), remote_id: Some("unchanged".to_string()), ..Default::default() });
    }
    if backfill {
        log::info!("TheGamesDB dump unchanged; re-reading the local copy to fill in game platforms and regions");
    }
    progress.force(0, None, "reading TheGamesDB dump").await;
    let parse_path = path.clone();
    let dump: TgdbDump = tokio::task::spawn_blocking(move || -> Result<TgdbDump, DatabaseError> {
        let file = std::fs::File::open(&parse_path)?;
        Ok(serde_json::from_reader(std::io::BufReader::with_capacity(1 << 20, file))?)
    })
    .await
    .map_err(|e| DatabaseError::Server(e.to_string()))??;
    let games = dump.data.games;
    let total = games.len() as u64;
    let mut summary = BulkSummary { provider: Some(Provider::TheGamesDb), remote_id: dump.last_edit_id.map(|id| format!("last_edit_id {id}")), pages: 1, ..Default::default() };

    // Cache rows, 1000 per statement, with each game's platform (TheGamesDB's id and name,
    // and the app platform it maps to).
    let info = bulk::thegamesdb_platform_info(clients, store).await?;
    let raw_path = path.display().to_string();
    for chunk in games.chunks(1000) {
        let placeholders = vec!["(?, ?, 'dump', ?, ?, ?, ?, ?, ?, NOW(), FALSE)"; chunk.len()].join(", ");
        let sql = format!(
            "INSERT INTO thegamesdb_games (provider_id, raw_response_path, raw_format, title, remote_platform_id, platform_name, app_platform_id, region_id, country_id, fetched_at, deleted) VALUES {placeholders} \
             ON DUPLICATE KEY UPDATE raw_response_path = VALUES(raw_response_path), raw_format = VALUES(raw_format), title = VALUES(title), \
             remote_platform_id = VALUES(remote_platform_id), platform_name = VALUES(platform_name), app_platform_id = VALUES(app_platform_id), \
             region_id = VALUES(region_id), country_id = VALUES(country_id), fetched_at = NOW(), deleted = FALSE"
        );
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
        for game in chunk {
            let title: Option<String> = game.game_title.as_deref().map(bulk::decode_html_entities).map(|t| t.chars().take(512).collect());
            let platform = game.platform.and_then(|p| info.get(&p));
            query = query
                .bind(game.id)
                .bind(&raw_path)
                .bind(title)
                .bind(game.platform.unwrap_or(0))
                .bind(platform.and_then(|p| p.0.clone()))
                .bind(platform.and_then(|p| p.1).map(|p| p as u16))
                // 0, not NULL, for "TheGamesDB has none": NULL is "not read yet".
                .bind(game.region_id.unwrap_or(0).max(0))
                .bind(game.country_id.unwrap_or(0).max(0));
        }
        query.execute(pool).await?;
        summary.games += chunk.len() as u64;
        progress.report(summary.games, Some(total), &format!("caching TheGamesDB games: {} of {total}", summary.games)).await;
    }

    // Link, one app platform at a time, each release to the entry of its own region.
    let map = bulk::thegamesdb_platform_map(clients, store).await?;
    let regions = super::thegamesdb_region::TgdbRegions::load(clients, store.root()).await?;
    let priority = super::super::settings::region_priority(pool).await?;
    let mut by_platform: HashMap<Platform, Vec<super::thegamesdb_region::Candidate>> = HashMap::new();
    for game in &games {
        if let Some(platform) = game.platform.and_then(|p| map.get(&p)) {
            by_platform.entry(*platform).or_default().push(bulk::thegamesdb_candidate(game));
        }
    }
    let platforms = by_platform.len() as u64;
    for (i, (platform, found)) in by_platform.into_iter().enumerate() {
        progress.report(i as u64, Some(platforms), &format!("linking by name: {platform:?} ({} games)", found.len())).await;
        summary.linked += super::thegamesdb_region::link_platform(pool, &regions, &priority, platform, &found).await?;
        // The dump is TheGamesDB's whole list, so each platform's count is its total.
        let n = found.len() as u64;
        sync_state::record(pool, Provider::TheGamesDb, Some(platform), None, n, Some(n), true).await?;
    }
    sync_state::record(pool, Provider::TheGamesDb, None, summary.remote_id.as_deref(), total, Some(total), true).await?;
    // Recorded only after a successful import, so a failed one is retried in full.
    let size = std::fs::metadata(&path).map(|m| m.len()).ok();
    record_dump_file(pool, Provider::TheGamesDb, NAME, &DumpFile { etag, last_modified, size, entries: Some(total), ..Default::default() }).await?;
    progress.force(platforms, Some(platforms), &format!("{} games cached, {} linked by name", summary.games, summary.linked)).await;
    Ok(summary)
}

// ---------------------------------------------------------------------------
// Hasheous
// ---------------------------------------------------------------------------

/// What one dump entry contributes: its name/platform, its ROM hashes, the ids it maps.
#[derive(Debug, Default, PartialEq)]
pub struct HasheousDumpGame {
    pub id: i64,
    pub name: String,
    pub platform: Option<String>,
    /// (md5, sha1), lowercase hex, either may be missing.
    pub roms: Vec<(Option<String>, Option<String>)>,
    /// (provider, provider id) for every `Status: "Mapped"` metadata entry we have a
    /// provider for.
    pub mapped: Vec<(Provider, String)>,
}

/// Reads one dump entry (`{Id, Name, SignatureDataObjects, Metadata, Attributes: [{
/// attributeName: "ROMs", Value: [{Md5, Sha1, ...}]}, ...]}`).
pub fn parse_hasheous_dump_game(value: &serde_json::Value) -> HasheousDumpGame {
    let hex = |v: &serde_json::Value| v.as_str().map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit()));
    let roms = value["Attributes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["attributeName"].as_str() == Some("ROMs"))
        .flat_map(|a| a["Value"].as_array().into_iter().flatten())
        .map(|rom| (hex(&rom["Md5"]).filter(|h| h.len() == 32), hex(&rom["Sha1"]).filter(|h| h.len() == 40)))
        .filter(|(md5, sha1)| md5.is_some() || sha1.is_some())
        .collect();
    let mapped = value["Metadata"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["Status"].as_str() == Some("Mapped"))
        .filter_map(|m| {
            let provider = Provider::from_hasheous_source(m["Source"].as_str()?)?;
            let id = match &m["Id"] {
                serde_json::Value::String(s) => s.trim().to_string(),
                serde_json::Value::Number(n) => n.to_string(),
                _ => return None,
            };
            (!id.is_empty() && id != "0").then_some((provider, id))
        })
        .collect();
    HasheousDumpGame {
        id: value["Id"].as_i64().unwrap_or(0),
        name: value["Name"].as_str().unwrap_or_default().to_string(),
        platform: value["SignatureDataObjects"].as_array().and_then(|s| s.first()).and_then(|s| s["Platform"].as_str()).map(str::to_string),
        roms,
        mapped,
    }
}

/// One entry of `Dumps/platforms?includeDetails=true`: a per-platform zip and its identity.
#[derive(Debug, Clone, Deserialize)]
pub struct HasheousZipInfo {
    pub name: String,
    #[serde(rename = "sizeBytes", default)]
    pub size_bytes: Option<u64>,
    #[serde(rename = "lastModifiedUtc", default)]
    pub last_modified_utc: Option<String>,
    #[serde(rename = "md5hash", default)]
    pub md5: Option<String>,
}

/// What to do with each zip: `(name, download, process)`. Download when forced, when there
/// is no local copy, or when Hasheous' md5 differs from the one last processed; process when
/// downloaded, or when the catalog gained content since the zip was last matched (new DATs
/// can match ROMs an unchanged zip already had). Neither = skipped as unchanged.
pub fn plan_hasheous_zips(zips: &[HasheousZipInfo], known: &HashMap<String, DumpFile>, local: &std::collections::HashSet<String>, content_max: u64, redownload: bool) -> Vec<(String, bool, bool)> {
    zips.iter()
        .map(|z| {
            let k = known.get(&z.name);
            let changed = k.map_or(true, |k| k.md5.is_none() || k.md5 != z.md5);
            let download = redownload || !local.contains(&z.name) || changed;
            let process = download || k.and_then(|k| k.processed_content_max).map_or(true, |m| m < content_max);
            (z.name.clone(), download, process)
        })
        .collect()
}

fn hasheous_zip_url(name: &str) -> Result<String, DatabaseError> {
    let mut url = reqwest::Url::parse(&format!("{HASHEOUS_DUMPS_URL}/platforms/")).map_err(|e| DatabaseError::Config(e.to_string()))?;
    url.path_segments_mut().map_err(|_| DatabaseError::Config("bad Hasheous dump url".to_string()))?.pop_if_empty().push(name);
    Ok(url.to_string())
}

/// Hasheous dump import from its per-platform zips: `platform` = that platform's zip(s),
/// `None` = all of them (Hasheous' whole map, ~2.6 GB over ~600 zips). One
/// `includeDetails` call gives every zip's md5; a diff (the default) downloads only zips
/// that changed, `parallel` at a time, and processes each as soon as it's on disk while the
/// others keep downloading. Unchanged zips are skipped unless the catalog gained content
/// since they were last matched (then re-processed from the local copy, no download).
/// `summary.games` = games read, `hashes` = local contents matched, `linked` = links,
/// `pages` = zips processed. Failed downloads don't stop the rest; the job fails at the end
/// naming them, and a retry redoes only those (everything else is recorded as current).
pub async fn sync_hasheous_dump(http: &reqwest::Client, pool: &MySqlPool, store: &RawStore, platform: Option<Platform>, mode: bulk::SyncMode, parallel: usize, progress: &Progress) -> Result<BulkSummary, DatabaseError> {
    let all: Vec<HasheousZipInfo> = http.get(format!("{HASHEOUS_DUMPS_URL}/platforms?includeDetails=true")).send().await?.error_for_status()?.json().await?;
    let zips: Vec<HasheousZipInfo> = match platform {
        Some(p) => all.into_iter().filter(|z| platform_map::platform_from_dat_name(z.name.trim_end_matches(".zip")) == p).collect(),
        None => all,
    };
    if zips.is_empty() {
        return Err(DatabaseError::Provider(format!("Hasheous has no platform dump for {platform:?}")));
    }
    let dir = store.root().join("hasheous").join("dumps");
    let mut known: HashMap<String, DumpFile> = HashMap::new();
    for z in &zips {
        if let Some(k) = dump_file(pool, Provider::Hasheous, &z.name).await? {
            known.insert(z.name.clone(), k);
        }
    }
    let local: std::collections::HashSet<String> = zips.iter().filter(|z| dir.join(&z.name).is_file()).map(|z| z.name.clone()).collect();
    let content_max = content_marker(pool).await?;
    let plan = plan_hasheous_zips(&zips, &known, &local, content_max, mode == bulk::SyncMode::Redownload);
    let info: HashMap<String, &HasheousZipInfo> = zips.iter().map(|z| (z.name.clone(), z)).collect();
    let to_download = plan.iter().filter(|p| p.1).count();
    let to_process = plan.iter().filter(|p| p.2).count() as u64;
    let unchanged = zips.len() - to_process as usize;
    progress.force(0, Some(to_process.max(1)), &format!("Hasheous: {} zip(s): {to_download} to download ({parallel} at a time), {to_process} to process, {unchanged} unchanged", zips.len())).await;

    // Downloads run as tasks (dropping the set on cancel aborts them); processing happens
    // here, one zip at a time, as they arrive.
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(parallel.max(1)));
    let mut downloads: tokio::task::JoinSet<(String, Result<(Option<String>, Option<String>, u64), DatabaseError>)> = tokio::task::JoinSet::new();
    let mut ready: std::collections::VecDeque<(String, Option<String>, Option<String>)> = std::collections::VecDeque::new();
    for (name, download, process) in &plan {
        if *download {
            let (http, dest, name, url, semaphore) = (http.clone(), dir.join(name), name.clone(), hasheous_zip_url(name)?, semaphore.clone());
            downloads.spawn(async move {
                let _permit = semaphore.acquire_owned().await;
                let result = download_if_changed(&http, &url, &dest, None, true, &Progress::none(), &name).await.map(|(fetched, etag, lm)| {
                    let bytes = match fetched {
                        Fetched::Downloaded { bytes } => bytes,
                        Fetched::Unchanged => 0,
                    };
                    (etag, lm, bytes)
                });
                (name, result)
            });
        } else if *process {
            let k = known.get(name);
            ready.push_back((name.clone(), k.and_then(|k| k.etag.clone()), k.and_then(|k| k.last_modified.clone())));
        }
    }

    let mut summary = BulkSummary { provider: Some(Provider::Hasheous), platform, remote_id: Some(format!("{} zip(s)", zips.len())), ..Default::default() };
    let mut igdb_ids: BTreeSet<String> = BTreeSet::new();
    let (mut downloaded, mut bytes) = (0usize, 0u64);
    let mut failed: Vec<String> = Vec::new();
    loop {
        while let Some((name, etag, last_modified)) = ready.pop_front() {
            let status = format!("Hasheous: {downloaded}/{to_download} downloaded ({}), {}/{to_process} processed; {} games, {} local matches, {} links -- {name}", mib(bytes), summary.pages, summary.games, summary.hashes, summary.linked);
            progress.report(summary.pages as u64, Some(to_process.max(1)), &status).await;
            match process_hasheous_zip(pool, store, &dir.join(&name), &mut igdb_ids, progress, (summary.pages as u64, to_process.max(1), &status)).await {
                Ok((games, hashes, links)) => {
                    summary.games += games;
                    summary.hashes += hashes;
                    summary.linked += links;
                    summary.pages += 1;
                    let z = info.get(&name);
                    record_dump_file(pool, Provider::Hasheous, &name, &DumpFile { md5: z.and_then(|z| z.md5.clone()), size: z.and_then(|z| z.size_bytes), last_modified: last_modified.or_else(|| z.and_then(|z| z.last_modified_utc.clone())), etag, entries: Some(games), processed_content_max: Some(content_max) }).await?;
                }
                Err(e) => {
                    log::warn!("hasheous: processing {name} failed: {e}");
                    failed.push(format!("{name}: {e}"));
                }
            }
        }
        let Some(joined) = downloads.join_next().await else { break };
        let (name, result) = joined.map_err(|e| DatabaseError::Server(e.to_string()))?;
        match result {
            Ok((etag, last_modified, n)) => {
                downloaded += 1;
                bytes += n;
                ready.push_back((name, etag, last_modified));
            }
            Err(e) => {
                log::warn!("hasheous: downloading {name} failed: {e}");
                failed.push(format!("{name}: {e}"));
            }
        }
    }

    // Coverage from what every zip held (processed now or earlier), per app platform.
    let rows = sqlx::query("SELECT name, entries FROM provider_dump_files WHERE provider = 'hasheous'").fetch_all(pool).await?;
    let mut by_platform: HashMap<Platform, u64> = HashMap::new();
    let mut total_entries = 0u64;
    for row in &rows {
        let name: String = row.try_get("name")?;
        let n = row.try_get::<Option<u32>, _>("entries")?.unwrap_or(0) as u64;
        total_entries += n;
        let p = platform_map::platform_from_dat_name(name.trim_end_matches(".zip"));
        if p != Platform::Unknown {
            *by_platform.entry(p).or_default() += n;
        }
    }
    for (p, n) in &by_platform {
        if platform.is_none() || platform == Some(*p) {
            sync_state::record(pool, Provider::Hasheous, Some(*p), None, *n, Some(*n), true).await?;
        }
    }
    if platform.is_none() {
        sync_state::record(pool, Provider::Hasheous, None, Some(&format!("{} zips", zips.len())), total_entries, Some(total_entries), failed.is_empty()).await?;
    }

    // Exact IGDB ids -> IGDB metadata by id (cached games are reused, not refetched).
    let ids: Vec<String> = igdb_ids.into_iter().collect();
    for chunk in ids.chunks(500) {
        let job = jobs::enqueue(pool, JobType::ProviderQuery, serde_json::json!({ "provider": "igdb", "provider_ids": chunk })).await?;
        log::info!("hasheous: queued IGDB metadata job {job} for {} id(s)", chunk.len());
    }
    progress.force(to_process.max(1), Some(to_process.max(1)), &format!("Hasheous: {downloaded} zip(s) downloaded ({}), {} processed, {unchanged} unchanged; {} games, {} local matches, {} links; {} IGDB ids queued{}", mib(bytes), summary.pages, summary.games, summary.hashes, summary.linked, ids.len(), if failed.is_empty() { String::new() } else { format!("; {} failed", failed.len()) })).await;
    if !failed.is_empty() {
        return Err(DatabaseError::Provider(format!("{} Hasheous zip(s) failed: {}", failed.len(), failed.join("; "))));
    }
    Ok(summary)
}

/// Makes sure every Hasheous zip already on disk is in the hash index
/// (`hasheous_dump_games` / `hasheous_dump_roms`). No network. Returns how many zips it read.
///
/// The index is written whenever a zip is processed, so this only has work to do for zips
/// downloaded by a build older than the index (or after the tables were emptied): each of
/// those is read once, here, and never again until Hasheous publishes a new one.
pub async fn ensure_hasheous_index(pool: &MySqlPool, store: &RawStore, progress: &Progress) -> Result<u64, DatabaseError> {
    let dir = store.root().join("hasheous").join("dumps");
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT f.name FROM provider_dump_files f WHERE f.provider = 'hasheous' AND COALESCE(f.entries, 0) > 0 \
         AND NOT EXISTS (SELECT 1 FROM hasheous_dump_games g WHERE g.dump_name = f.name) ORDER BY f.name",
    )
    .fetch_all(pool)
    .await?;
    let missing: Vec<String> = names.into_iter().filter(|n| dir.join(n).is_file()).collect();
    let total = missing.len() as u64;
    // Not queued from here: `hash_match` collects the ids of the files it is matching.
    let mut igdb_ids: BTreeSet<String> = BTreeSet::new();
    let mut read = 0u64;
    for (i, name) in missing.iter().enumerate() {
        let status = format!("Hasheous: indexing downloaded dump {} of {total} -- {name}", i + 1);
        progress.report(i as u64, Some(total), &status).await;
        match process_hasheous_zip(pool, store, &dir.join(name), &mut igdb_ids, progress, (i as u64, total, &status)).await {
            Ok(_) => read += 1,
            // Left unindexed; the live lookup still covers its files.
            Err(e) => log::warn!("hasheous: indexing {name} failed: {e}"),
        }
    }
    Ok(read)
}

/// Answers, from the hash index, every live file under the `scope` directories that has no
/// Hasheous answer recorded yet: one join, no network, no zip. Each match is recorded exactly
/// as a live lookup or a dump sync would record it (`hasheous_lookups` + the provider links),
/// so `hash_match` then finds it cached. Returns how many contents were answered.
///
/// `scope` is a list of `local_directories` ids built by the caller from the database.
pub async fn match_hasheous_index(pool: &MySqlPool, store: &RawStore, scope: &str, progress: &Progress) -> Result<u64, DatabaseError> {
    let mut content_game: HashMap<u64, u64> = HashMap::new();
    // The directory's files and their archive members (the dumps list Redump's bins, not our
    // CHDs), then five thousand at a time -- as one statement over a whole archive it showed
    // nothing for minutes.
    let mut contents: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT f.content_id FROM local_files f WHERE f.deleted = FALSE AND f.directory_id IN ({scope}) \
         UNION SELECT cc.child_content_id FROM local_files f JOIN content_contents cc ON cc.parent_content_id = f.content_id WHERE f.deleted = FALSE AND f.directory_id IN ({scope})"
    )))
    .fetch_all(pool)
    .await?;
    contents.sort_unstable();
    let n = contents.len() as u64;
    for (i, chunk) in contents.chunks(5000).enumerate() {
        progress.report((i * 5000) as u64, Some(n.max(1)), &format!("Hasheous: checking the downloaded dumps, {} of {n} file(s), {} known", i * 5000, content_game.len())).await;
        let list = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        // One statement per algorithm, each with the algorithm as a literal on both sides, so
        // both `(algorithm, value_hex)` indexes are used.
        for algorithm in ["md5", "sha1"] {
            let sql = format!(
                "SELECT h.content_id, MIN(r.game_id) AS game_id FROM hashes h \
                 JOIN hasheous_dump_roms r ON r.algorithm = '{algorithm}' AND r.value_hex = h.value_hex \
                 WHERE h.content_id IN ({list}) AND h.algorithm = '{algorithm}' \
                 AND NOT EXISTS (SELECT 1 FROM hasheous_lookups hl WHERE hl.content_id = h.content_id AND hl.deleted = FALSE AND hl.hit = TRUE) GROUP BY h.content_id"
            );
            for row in sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(pool).await? {
                content_game.entry(row.try_get("content_id")?).or_insert(row.try_get("game_id")?);
            }
        }
    }
    Ok(record_index_matches(pool, store, content_game, progress, "Hasheous: answering from the downloaded dumps").await?.0)
}

/// [`match_hasheous_index`] for the whole catalogue instead of the files on disk: every
/// content a release lists (a DAT line's hashes) that the index knows and that has no
/// Hasheous answer yet. This is what makes a link hash-grade without a file -- the DAT
/// supplies the hash, the dump says which game that hash is. A Hasheous sync does the same
/// while it reads a zip; this covers DATs imported after the zips were. No network.
/// Returns how many contents were answered and the IGDB ids that linked something.
pub async fn match_hasheous_index_catalog(pool: &MySqlPool, store: &RawStore, progress: &Progress) -> Result<(u64, BTreeSet<String>), DatabaseError> {
    if sqlx::query("SELECT 1 FROM hasheous_dump_roms LIMIT 1").fetch_optional(pool).await?.is_none() {
        return Ok((0, BTreeSet::new()));
    }
    let mut content_game: HashMap<u64, u64> = HashMap::new();
    for algorithm in ["md5", "sha1"] {
        let sql = format!(
            "SELECT h.content_id, MIN(r.game_id) AS game_id FROM hashes h \
             JOIN hasheous_dump_roms r ON r.algorithm = '{algorithm}' AND r.value_hex = h.value_hex \
             WHERE h.algorithm = '{algorithm}' AND h.content_id IS NOT NULL \
             AND EXISTS (SELECT 1 FROM release_contents rc WHERE rc.content_id = h.content_id) \
             AND NOT EXISTS (SELECT 1 FROM hasheous_lookups hl WHERE hl.content_id = h.content_id AND hl.deleted = FALSE AND hl.hit = TRUE) GROUP BY h.content_id"
        );
        for row in sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(pool).await? {
            content_game.entry(row.try_get("content_id")?).or_insert(row.try_get("game_id")?);
        }
    }
    record_index_matches(pool, store, content_game, progress, "Hasheous: matching the catalogue against the downloaded dumps").await
}

/// Records index matches (content -> `hasheous_dump_games` id). Returns how many contents
/// were recorded and the IGDB ids that linked something.
async fn record_index_matches(pool: &MySqlPool, store: &RawStore, content_game: HashMap<u64, u64>, progress: &Progress, status: &str) -> Result<(u64, BTreeSet<String>), DatabaseError> {
    if content_game.is_empty() {
        return Ok((0, BTreeSet::new()));
    }
    // The games those contents matched, as the same structure a zip entry parses to.
    let mut ids: Vec<u64> = content_game.values().copied().collect();
    ids.sort_unstable();
    ids.dedup();
    let mut games: Vec<HasheousDumpGame> = Vec::with_capacity(ids.len());
    let mut position: HashMap<u64, usize> = HashMap::new();
    for chunk in ids.chunks(5000) {
        let sql = format!("SELECT id, name, platform, mapped FROM hasheous_dump_games WHERE id IN ({})", chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(","));
        for row in sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(pool).await? {
            let id: u64 = row.try_get("id")?;
            let mapped: Vec<(String, String)> = serde_json::from_str(&row.try_get::<String, _>("mapped")?).unwrap_or_default();
            position.insert(id, games.len());
            games.push(HasheousDumpGame {
                id: id as i64,
                name: row.try_get("name")?,
                platform: row.try_get("platform")?,
                roms: Vec::new(),
                mapped: mapped.into_iter().filter_map(|(p, id)| Provider::from_name(&p).map(|p| (p, id))).collect(),
            });
        }
    }
    let matches: HashMap<u64, usize> = content_game.into_iter().filter_map(|(content, game)| position.get(&game).map(|&i| (content, i))).collect();
    let mut igdb_ids: BTreeSet<String> = BTreeSet::new();
    let (matched, _links) = record_hasheous_matches(pool, store, &games, &matches, &mut igdb_ids, progress, (0, 1, status)).await?;
    Ok((matched, igdb_ids))
}

/// Writes one batch of a zip's games into the hash index. Only games Hasheous has mapped to
/// a provider: nothing else can answer a lookup.
async fn index_hasheous_batch(pool: &MySqlPool, dump_name: &str, batch: &[HasheousDumpGame]) -> Result<(), DatabaseError> {
    let games: Vec<&HasheousDumpGame> = batch.iter().filter(|g| g.id > 0 && !g.mapped.is_empty() && !g.roms.is_empty()).collect();
    for chunk in games.chunks(500) {
        let sql = format!(
            "INSERT INTO hasheous_dump_games (id, name, platform, mapped, dump_name) VALUES {} \
             ON DUPLICATE KEY UPDATE name = VALUES(name), platform = VALUES(platform), mapped = VALUES(mapped), dump_name = VALUES(dump_name)",
            vec!["(?, ?, ?, ?, ?)"; chunk.len()].join(", ")
        );
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
        for game in chunk {
            let mapped: Vec<(&str, &str)> = game.mapped.iter().map(|(p, id)| (p.name(), id.as_str())).collect();
            let name: String = game.name.chars().take(512).collect();
            query = query.bind(game.id as u64).bind(name).bind(game.platform.as_ref().map(|p| p.chars().take(255).collect::<String>())).bind(serde_json::to_string(&mapped).unwrap_or_else(|_| "[]".to_string())).bind(dump_name);
        }
        query.execute(pool).await?;
    }
    // (algorithm, hash, game): a collection entry lists thousands of ROMs, so this is written
    // in its own, larger chunks. 3 placeholders a row keeps 10,000 rows well under MariaDB's
    // 65,535 limit.
    let mut roms: Vec<(&'static str, &str, u64)> = Vec::new();
    for game in &games {
        for (md5, sha1) in &game.roms {
            if let Some(h) = md5 {
                roms.push(("md5", h, game.id as u64));
            }
            if let Some(h) = sha1 {
                roms.push(("sha1", h, game.id as u64));
            }
        }
    }
    for chunk in roms.chunks(10_000) {
        let sql = format!("INSERT IGNORE INTO hasheous_dump_roms (algorithm, value_hex, game_id) VALUES {}", vec!["(?, ?, ?)"; chunk.len()].join(", "));
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
        for (algorithm, hash, game) in chunk {
            query = query.bind(*algorithm).bind(*hash).bind(*game);
        }
        query.execute(pool).await?;
    }
    Ok(())
}

/// One Hasheous zip: parse its entries on a blocking thread (batches of 500 over a
/// channel) and match/link each batch. Returns (games, contents matched, links).
async fn process_hasheous_zip(pool: &MySqlPool, store: &RawStore, path: &Path, igdb_ids: &mut BTreeSet<String>, progress: &Progress, at: (u64, u64, &str)) -> Result<(u64, u64, u64), DatabaseError> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<HasheousDumpGame>>(4);
    let read_path = path.to_path_buf();
    let reader = tokio::task::spawn_blocking(move || -> Result<(), DatabaseError> {
        let mut zip = zip::ZipArchive::new(std::io::BufReader::new(std::fs::File::open(&read_path)?))?;
        let mut batch = Vec::with_capacity(500);
        for i in 0..zip.len() {
            let entry = zip.by_index(i)?;
            if !entry.name().to_ascii_lowercase().ends_with(".json") {
                continue;
            }
            match serde_json::from_reader::<_, serde_json::Value>(std::io::BufReader::new(entry)) {
                Ok(value) => batch.push(parse_hasheous_dump_game(&value)),
                Err(e) => log::warn!("hasheous dump {}: entry {i}: {e}", read_path.display()),
            }
            if batch.len() >= 500 && tx.blocking_send(std::mem::take(&mut batch)).is_err() {
                return Ok(());
            }
        }
        if !batch.is_empty() {
            let _ = tx.blocking_send(batch);
        }
        Ok(())
    });
    // This zip's rows in the hash index are replaced by what it holds now.
    let dump_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    sqlx::query("DELETE r FROM hasheous_dump_roms r JOIN hasheous_dump_games g ON g.id = r.game_id WHERE g.dump_name = ?").bind(&dump_name).execute(pool).await?;
    sqlx::query("DELETE FROM hasheous_dump_games WHERE dump_name = ?").bind(&dump_name).execute(pool).await?;
    let (mut games, mut hashes, mut links) = (0u64, 0u64, 0u64);
    while let Some(batch) = rx.recv().await {
        games += batch.len() as u64;
        index_hasheous_batch(pool, &dump_name, &batch).await?;
        let (h, l) = apply_hasheous_batch(pool, store, &batch, igdb_ids, progress, at).await?;
        hashes += h;
        links += l;
    }
    reader.await.map_err(|e| DatabaseError::Server(e.to_string()))??;
    Ok((games, hashes, links))
}

/// One batch: which of its ROM hashes the catalog has, and the links for those contents.
/// Returns (contents matched, links written).
/// `at` = (games read so far, total, status line) -- a batch can hold tens of thousands of
/// local matches (collection entries list thousands of ROMs), so linking reports its own
/// progress instead of leaving the bar still until the batch ends.
async fn apply_hasheous_batch(pool: &MySqlPool, store: &RawStore, batch: &[HasheousDumpGame], igdb_ids: &mut BTreeSet<String>, progress: &Progress, at: (u64, u64, &str)) -> Result<(u64, u64), DatabaseError> {
    let mut by_md5: HashMap<&str, usize> = HashMap::new();
    let mut by_sha1: HashMap<&str, usize> = HashMap::new();
    for (i, game) in batch.iter().enumerate().filter(|(_, g)| !g.mapped.is_empty()) {
        for (md5, sha1) in &game.roms {
            if let Some(h) = md5 {
                by_md5.insert(h, i);
            }
            if let Some(h) = sha1 {
                by_sha1.insert(h, i);
            }
        }
    }
    if by_md5.is_empty() && by_sha1.is_empty() {
        return Ok((0, 0));
    }
    // One query per algorithm per HASH_CHUNK hashes: a batch of 500 games can carry far more
    // ROMs than that (a TOSEC collection entry lists thousands), and MariaDB refuses a
    // statement with more than 65,535 placeholders.
    const HASH_CHUNK: usize = 10_000;
    let mut content_game: HashMap<u64, usize> = HashMap::new();
    for (algorithm, index) in [("md5", &by_md5), ("sha1", &by_sha1)] {
        let hashes: Vec<&str> = index.keys().copied().collect();
        for chunk in hashes.chunks(HASH_CHUNK) {
            let sql = format!("SELECT DISTINCT content_id, value_hex FROM hashes WHERE content_id IS NOT NULL AND algorithm = '{algorithm}' AND value_hex IN ({})", vec!["?"; chunk.len()].join(","));
            let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
            for h in chunk {
                query = query.bind(*h);
            }
            for row in query.fetch_all(pool).await? {
                let content_id: u64 = row.try_get("content_id")?;
                let value: String = row.try_get("value_hex")?;
                if let Some(&g) = index.get(value.as_str()) {
                    content_game.insert(content_id, g);
                }
            }
        }
    }
    record_hasheous_matches(pool, store, batch, &content_game, igdb_ids, progress, at).await
}

/// Records that each content in `content_game` is the Hasheous game at that index of
/// `batch`: the cache file, the `hasheous_lookups` row and the provider links. Shared by a
/// zip being processed and by a lookup answered from the hash index. Returns (contents
/// recorded, links written).
async fn record_hasheous_matches(pool: &MySqlPool, store: &RawStore, batch: &[HasheousDumpGame], content_game: &HashMap<u64, usize>, igdb_ids: &mut BTreeSet<String>, progress: &Progress, at: (u64, u64, &str)) -> Result<(u64, u64), DatabaseError> {
    // Batched: one cache file per Hasheous game (not per matched ROM -- a collection entry
    // matches thousands), `hasheous_lookups` 1,000 rows per statement, and the links
    // deduplicated and written in bulk (`cache::link_contents_bulk`).
    let matched = content_game.len() as u64;
    let mut raw_paths: HashMap<usize, Option<String>> = HashMap::new();
    for &g in content_game.values() {
        if raw_paths.contains_key(&g) {
            continue;
        }
        let game = &batch[g];
        // The same shape a live lookup caches (cache::hasheous_lookup), so
        // `hasheous_record_from_cache` reads it and no live lookup repeats these contents.
        let raw = serde_json::json!({
            "id": game.id,
            "name": game.name,
            "platform": game.platform,
            "signature": { "game": { "name": game.name, "system": game.platform } },
            "metadata": game.mapped.iter().map(|(p, id)| serde_json::json!({ "id": id, "source": p.name() })).collect::<Vec<_>>(),
            "from_dump": true,
        });
        let path = store.save_json(Provider::Hasheous, &format!("dump_game_{}", game.id), &raw)?;
        raw_paths.insert(g, path.map(|p| p.display().to_string()));
    }
    let rows: Vec<(u64, Option<&String>)> = content_game.iter().map(|(&c, g)| (c, raw_paths.get(g).and_then(|p| p.as_ref()))).collect();
    for (i, chunk) in rows.chunks(1000).enumerate() {
        let sql = format!(
            "INSERT INTO hasheous_lookups (content_id, raw_response_path, raw_format, hit, fetched_at, deleted) VALUES {} \
             ON DUPLICATE KEY UPDATE raw_response_path = VALUES(raw_response_path), raw_format = VALUES(raw_format), hit = TRUE, fetched_at = NOW(), deleted = FALSE",
            vec!["(?, ?, 'json', TRUE, NOW(), FALSE)"; chunk.len()].join(", ")
        );
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
        for (content_id, path) in chunk {
            query = query.bind(*content_id).bind(*path);
        }
        query.execute(pool).await?;
        progress.report(at.0, Some(at.1), &format!("{} -- recording this batch: {} of {matched} matches", at.2, ((i + 1) * 1000).min(matched as usize))).await;
    }
    let items: Vec<(u64, Provider, String)> = content_game.iter().flat_map(|(&c, &g)| batch[g].mapped.iter().map(move |(p, id)| (c, *p, id.clone()))).collect();
    if matched > 5000 {
        progress.report(at.0, Some(at.1), &format!("{} -- linking this batch's {matched} matches", at.2)).await;
    }
    let (links, linked) = cache::link_contents_bulk(pool, &items, Confidence::Hash).await?;
    // Only ids that linked something are worth an IGDB fetch.
    igdb_ids.extend(linked.into_iter().filter(|(p, _)| *p == Provider::Igdb).map(|(_, id)| id));
    Ok((matched, links))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_200_with_the_same_validators_counts_as_unchanged() {
        let dir = std::env::temp_dir().join(format!("romexplorer_same_as_local_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("dump.json");
        std::fs::write(&dest, b"0123456789").unwrap();
        let lm = "Sat, 18 Jul 2026 23:00:41 GMT";
        let rec = |l: Option<&str>, e: Option<&str>| DumpFile { last_modified: l.map(str::to_string), etag: e.map(str::to_string), ..Default::default() };
        assert!(same_as_local(&dest, Some(&rec(Some(lm), None)), None, Some(lm), Some(999)), "recorded Last-Modified matches");
        assert!(same_as_local(&dest, Some(&rec(None, Some("W/\"x\""))), Some("W/\"x\""), None, None), "recorded ETag matches");
        assert!(!same_as_local(&dest, Some(&rec(Some(lm), None)), None, Some("Sun, 19 Jul 2026 00:00:00 GMT"), Some(10)), "newer dump");
        // Nothing recorded: the local copy (written just now) is newer than the remote file.
        assert!(same_as_local(&dest, None, None, Some(lm), Some(10)));
        assert!(!same_as_local(&dest, None, None, Some(lm), Some(11)), "size differs");
        assert!(!same_as_local(&dest, None, None, Some("not a date"), Some(10)), "unparseable date");
        assert!(!same_as_local(&dest, None, None, Some("Fri, 01 Jan 2100 00:00:00 GMT"), Some(10)), "remote newer than local");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hasheous_diff_downloads_only_changed_zips() {
        let zip = |name: &str, md5: &str| HasheousZipInfo { name: name.to_string(), size_bytes: Some(1), last_modified_utc: None, md5: Some(md5.to_string()) };
        let zips = vec![zip("same.zip", "aa"), zip("changed.zip", "bb"), zip("new.zip", "cc"), zip("gone_locally.zip", "dd"), zip("stale_match.zip", "ee")];
        let known: HashMap<String, DumpFile> = [
            ("same.zip", "aa", 100),
            ("changed.zip", "old", 100),
            ("gone_locally.zip", "dd", 100),
            ("stale_match.zip", "ee", 50),
        ]
        .into_iter()
        .map(|(n, m, c)| (n.to_string(), DumpFile { md5: Some(m.to_string()), processed_content_max: Some(c), ..Default::default() }))
        .collect();
        let local: std::collections::HashSet<String> = ["same.zip", "changed.zip", "stale_match.zip"].into_iter().map(str::to_string).collect();
        let plan = plan_hasheous_zips(&zips, &known, &local, 100, false);
        let get = |n: &str| plan.iter().find(|p| p.0 == n).map(|p| (p.1, p.2)).unwrap();
        assert_eq!(get("same.zip"), (false, false), "unchanged: skipped");
        assert_eq!(get("changed.zip"), (true, true), "md5 differs: downloaded");
        assert_eq!(get("new.zip"), (true, true), "never seen: downloaded");
        assert_eq!(get("gone_locally.zip"), (true, true), "local copy missing: downloaded");
        assert_eq!(get("stale_match.zip"), (false, true), "catalog grew: re-processed from the local copy");
        let all = plan_hasheous_zips(&zips, &known, &local, 100, true);
        assert!(all.iter().all(|p| p.1 && p.2), "redownload: everything");
    }

    #[test]
    fn reads_roms_and_only_mapped_ids() {
        let value = serde_json::json!({
            "Id": 61546, "Name": "Cross Chase",
            "SignatureDataObjects": [{ "Platform": "Amstrad GX4000" }],
            "Metadata": [
                { "Source": "IGDB", "Id": "", "Status": "NotMapped" },
                { "Source": "IGDB", "Id": "1234", "Status": "Mapped" },
                { "Source": "TheGamesDb", "Id": 99, "Status": "Mapped" },
                { "Source": "SomethingElse", "Id": "5", "Status": "Mapped" },
            ],
            "Attributes": [
                { "attributeName": "Publisher", "Value": {} },
                { "attributeName": "ROMs", "Value": [
                    { "Md5": "0EC500AD7BB5CFD67517E3556D614B3B", "Sha1": "e374455ea86801203d4a5858a637b1ea045c6aa1" },
                    { "Md5": "", "Sha1": "" },
                ] },
            ],
        });
        let g = parse_hasheous_dump_game(&value);
        assert_eq!(g.id, 61546);
        assert_eq!(g.platform.as_deref(), Some("Amstrad GX4000"));
        assert_eq!(g.roms, vec![(Some("0ec500ad7bb5cfd67517e3556d614b3b".to_string()), Some("e374455ea86801203d4a5858a637b1ea045c6aa1".to_string()))]);
        assert_eq!(g.mapped, vec![(Provider::Igdb, "1234".to_string()), (Provider::TheGamesDb, "99".to_string())]);
    }
}
