//! Typed job requests: the one place each job type's payload shape is defined and
//! validated. `database::jobs` stores `serde_json::Value`; the shapes here serialize to
//! exactly what the worker's handlers and `jobs.rs`'s doc comments already expect.

use serde::{Deserialize, Serialize};

use crate::common::models::Platform;
use crate::database::dat_download::Source;
use crate::database::jobs::{self as db_jobs, FileScanMode, JobRow, JobType};
use crate::database::directories;
use crate::database::providers::Provider;

use super::{clamp_page, App, Error, Result};

// The request and page types are shared with the GUI through `romexplorer_types`; the
// validation and enqueueing that need the database stay here.
pub use romexplorer_types::jobs::{
    CatalogSyncRequest, DatSyncRequest, FileScanRequest, HashMatchRequest, JobEnqueued, JobRequest, JobsPage, MediaDownloadRequest, MediaSyncRequest, NamedFileRequest, PlatformSelect,
    ProviderBulkSyncRequest, ProviderQueryRequest, RaGenreFetchRequest, RaPatchDownloadRequest, SqliteExportRequest, StatusCount, SyncModeName,
};

async fn require_directory(app: &App, id: u64) -> Result<()> {
    match directories::get(&app.pool, id).await? {
        Some(d) if !d.deleted => Ok(()),
        Some(_) => Err(Error::invalid(format!("directory {id} is deleted"))),
        None => Err(Error::not_found(format!("no directory {id}"))),
    }
}

fn require_provider(name: &str) -> Result<Provider> {
    Provider::from_name(name).ok_or_else(|| Error::invalid(format!("unknown provider {name:?}")))
}

fn require_platform(id: u16) -> Result<Platform> {
    Platform::from_igdb_id(id).ok_or_else(|| Error::invalid(format!("unknown platform id {id}")))
}

/// Refuses catalogue-building work on a `client` installation.
///
/// Not a warning: a client that imports a DAT gets row ids its main server does not have, and
/// the next catalogue sync either overwrites them or leaves them stranded -- after which any
/// ownership mirrored from a peer can attach to the wrong releases. The mode exists to make
/// that unrepresentable rather than merely discouraged.
async fn check_mode(app: &App, request: &JobRequest) -> Result<()> {
    let job = request.job_type();
    if !job.writes_catalog() {
        return Ok(());
    }
    if crate::database::settings::instance_mode(&app.pool).await? == crate::database::settings::InstanceMode::Client {
        return Err(Error::invalid(format!(
            "this installation is in client mode, so it takes its catalogue from its main server and cannot run {} itself. \
             Run it on the main server and sync, or switch this machine to standalone mode (which replaces its catalogue).",
            job.name()
        )));
    }
    Ok(())
}

pub async fn validate(app: &App, request: &JobRequest) -> Result<()> {
    check_mode(app, request).await?;
    match request {
        JobRequest::IntegrityCheck(r) => crate::api::integrity::validate(app, r).await,
        JobRequest::SheetCreate(r) => crate::api::sheets::run::validate_create(app, r).await,
        JobRequest::SheetVerify(r) | JobRequest::M3uVerify(r) => crate::api::sheets::run::validate_verify(r),
        JobRequest::AudioScan(r) => crate::api::audio::validate(app, r).await,
        JobRequest::RomAudioIndex(r) => crate::api::rom_audio::validate(app, r).await,
        JobRequest::AudioSync => crate::api::audio_sync::validate(app).await,
        JobRequest::MediaBackfill(r) => crate::api::media_backfill::validate_with(app, r).await,
        JobRequest::CopyFiles(r) => {
            crate::api::copy::validate_mode(app, r).await?;
            if r.selection.is_empty() {
                return Err(Error::invalid("copy_files: nothing selected"));
            }
            // A cap, not a judgement: the plan holds every file in memory, and a selection
            // this large is a mis-click rather than a request.
            if r.selection.len() > 20_000 {
                return Err(Error::invalid(format!("copy_files: {} items selected, at most 20000 at a time", r.selection.len())));
            }
            let root = crate::api::copy::check_destination(&r.destination)?;
            crate::api::copy::check_writable(&root)?;
            if let Some(output) = &r.output {
                crate::api::copy::validate_output(output)?;
            }
            // Registering writes `local_files` rows, which need a configured directory to
            // belong to. Refused here rather than discovered after the bytes have moved.
            if r.mode.registers() {
                let effective = crate::database::directories::resolve(&app.pool, &root.display().to_string()).await?;
                if effective.directory.is_none() {
                    return Err(Error::invalid(format!(
                        "{} isn't inside a configured collection directory, so the copied files couldn't be registered -- add it under Config > Directories, or copy to external media instead",
                        root.display()
                    )));
                }
            }
            // Every remote source the selection names has to be reachable, or the copy would
            // silently drop its files.
            let mounted: std::collections::HashSet<u16> = crate::api::copy::mount::load(app, None).await?.into_iter().map(|m| m.source_id).collect();
            let named: std::collections::HashSet<u16> = r.selection.game_ids.iter().chain(&r.selection.rom_ids).chain(&r.selection.file_ids).filter_map(|o| o.source_id).collect();
            for id in named {
                if !mounted.contains(&id) {
                    return Err(Error::invalid(format!("source {id} has no mount on this machine -- set one up under Config > Share first")));
                }
            }
            Ok(())
        }
        JobRequest::FileScan(r) => {
            if let PlatformSelect::Id(id) = r.platform {
                require_platform(id)?;
            }
            require_directory(app, r.directory_id).await
        }
        JobRequest::HashMatch(r) => {
            require_directory(app, r.directory_id).await?;
            if !(r.to_dat || r.to_hasheous || r.to_playmatch) {
                return Err(Error::invalid("hash_match: at least one of to_dat/to_hasheous/to_playmatch must be set"));
            }
            Ok(())
        }
        JobRequest::DatSync(r) => {
            if let Some(name) = &r.source {
                Source::from_name(name).ok_or_else(|| Error::invalid(format!("unknown DAT source {name:?}")))?;
            }
            if let Some(bad) = r.extras.iter().flatten().find(|x| !crate::database::dat_download::DAT_EXTRAS.iter().any(|(k, _)| k == x)) {
                return Err(Error::invalid(format!("dat_sync: unknown extra {bad:?}")));
            }
            if r.platforms.as_ref().is_some_and(|p| p.is_empty()) && r.extras.as_ref().is_some_and(|e| e.is_empty()) {
                return Err(Error::invalid("dat_sync: nothing ticked; leave platforms and extras out for everything"));
            }
            Ok(())
        }
        JobRequest::ProviderQuery(r) => {
            let provider = require_provider(&r.provider)?;
            if !app.clients().is_available(provider) {
                return Err(Error::unavailable(format!("provider {} has no usable credentials", provider.name())));
            }
            if let Some(p) = r.platform {
                require_platform(p)?;
            }
            if r.name.is_none() && r.content_id.is_none() && r.provider_id.is_none() && r.provider_ids.as_ref().map_or(true, |v| v.is_empty()) && r.title_ids.as_ref().map_or(true, |v| v.is_empty()) {
                return Err(Error::invalid("provider_query: needs one of name, content_id, provider_id, provider_ids, title_ids"));
            }
            if r.name.is_some() {
                let caps = super::providers::capabilities(provider);
                if !caps.keyword {
                    return Err(Error::invalid(format!("{} has no name search", provider.name())));
                }
                if caps.keyword_needs_platform && r.platform.is_none() {
                    return Err(Error::invalid(format!("{} searches by name per system: choose a platform", provider.name())));
                }
            }
            Ok(())
        }
        JobRequest::ProviderBulkSync(r) => {
            if let PlatformSelect::Id(id) = r.platform {
                require_platform(id)?;
            }
            let provider = require_provider(&r.provider)?;
            if !app.clients().is_available(provider) {
                return Err(Error::unavailable(format!("provider {} has no usable credentials", provider.name())));
            }
            let caps = super::providers::capabilities(provider);
            if r.full && !caps.full {
                return Err(Error::invalid(format!("{} can't fetch its whole database: {}", provider.name(), caps.notes)));
            }
            if !r.full && !caps.platform {
                return Err(Error::invalid(format!("{} has no per-platform listing: {}", provider.name(), caps.notes)));
            }
            if let (PlatformSelect::Id(id), Some(supported), false) = (&r.platform, super::providers::bulk_platforms(provider), r.full) {
                if !supported.contains(id) {
                    return Err(Error::invalid(format!("{} has no system for that platform", provider.name())));
                }
            }
            Ok(())
        }
        JobRequest::MediaDownload(r) => {
            require_provider(&r.provider)?;
            if r.url.trim().is_empty() {
                return Err(Error::invalid("media_download: url is required"));
            }
            if [r.title_id, r.release_id, r.franchise_id, r.keyword_id].iter().filter(|t| t.is_some()).count() != 1 {
                return Err(Error::invalid("media_download: exactly one of title_id / release_id / franchise_id / keyword_id"));
            }
            Ok(())
        }
        JobRequest::DbOptimize | JobRequest::MediaRelayout | JobRequest::TitleDedupe | JobRequest::TitleMergeAudit => Ok(()),
        // The backup directory is created on demand, so there is nothing to check up front
        // that would not be a lie by the time the job runs.
        JobRequest::BackupDatabase | JobRequest::BackupCatalog | JobRequest::BackupDatSeed => Ok(()),
        // Checked here rather than in the worker so a typo comes back on the request that
        // made it, not as a failed job minutes later.
        JobRequest::CatalogSync(r) => {
            let exists: Option<u16> = sqlx::query_scalar("SELECT id FROM remote_sources WHERE id = ? AND deleted = FALSE").bind(r.source_id).fetch_optional(&app.pool).await?;
            exists.map(|_| ()).ok_or_else(|| Error::invalid(format!("no source {}", r.source_id)))
        }
        JobRequest::MediaSync(r) => match r.source_id {
            Some(id) => {
                let exists: Option<u16> = sqlx::query_scalar("SELECT id FROM remote_sources WHERE id = ? AND deleted = FALSE").bind(id).fetch_optional(&app.pool).await?;
                exists.map(|_| ()).ok_or_else(|| Error::invalid(format!("no source {id}")))
            }
            None => super::media_sync::main_source(app).await?.map(|_| ()).ok_or_else(|| Error::invalid("media_sync: there is no main server to fetch art from -- add one under Config > Share, or name a source_id")),
        },
        JobRequest::RestoreBackup(r) => super::backup::file(app, &r.name).await.map(|_| ()),
        JobRequest::SqliteExport(r) => {
            super::sqlite::tables_for(&r.kind)?;
            match &r.mobile {
                Some(o) if r.kind == super::sqlite::MOBILE_KIND => super::mobile::validate(o),
                Some(_) => Err(Error::invalid("mobile options go with kind mobile only")),
                None => Ok(()),
            }
        }
        JobRequest::SqliteImport(r) => {
            if !r.name.ends_with(".sqlite") {
                return Err(Error::invalid("that is not a .sqlite cache"));
            }
            Ok(())
        }
        JobRequest::IgdbReapply(r) => {
            if !app.clients().is_available(Provider::Igdb) {
                return Err(Error::unavailable("IGDB has no usable credentials"));
            }
            if r.title_ids.as_ref().is_some_and(|ids| ids.is_empty()) {
                return Err(Error::invalid("no titles selected"));
            }
            Ok(())
        }
        JobRequest::RaGenreFetch(r) if !matches!(r.scope.as_str(), "local" | "all") => Err(Error::invalid("scope must be local or all")),
        JobRequest::RaPatchDownload(r) => {
            if r.md5.len() != 32 || !r.md5.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(Error::invalid("md5 must be 32 hex digits"));
            }
            let url: Option<Option<String>> = sqlx::query_scalar("SELECT patch_url FROM ra_game_hashes WHERE md5 = ?").bind(r.md5.to_ascii_lowercase()).fetch_optional(&app.pool).await?;
            match url.flatten() {
                Some(u) if crate::database::ra_patches::allowed_url(&u) => Ok(()),
                Some(u) => Err(Error::invalid(format!("refusing patch URL {u}"))),
                None => Err(Error::not_found(format!("RetroAchievements lists no patch for hash {}", r.md5))),
            }
        }
        JobRequest::DictionaryBuild(_) | JobRequest::AudioAutotag => Ok(()),
        JobRequest::DisplayData(r) => super::display_data::validate(r),
        JobRequest::ArtworkExport(r) => super::artwork::validate(app, r).await,
        JobRequest::AiBatchRate(r) => super::definitive::batch_validate(app, r).await,
        JobRequest::RaBadges(r) => {
            crate::api::ra::validate_badges(r)?;
            if !app.clients().is_available(Provider::RetroAchievements) {
                return Err(Error::unavailable("RetroAchievements has no usable credentials (ra_username / ra_api_key in api.env)"));
            }
            Ok(())
        }
        JobRequest::RaAccountSync | JobRequest::RaGenreFetch(_) => {
            if !app.clients().is_available(Provider::RetroAchievements) {
                return Err(Error::unavailable("RetroAchievements has no usable credentials (ra_username / ra_api_key in api.env)"));
            }
            Ok(())
        }
    }
}

pub async fn enqueue(app: &App, request: &JobRequest) -> Result<u64> {
    enqueue_with_priority(app, request, 0).await
}

/// Queues `request` to wait its turn rather than start alongside the other lanes
/// (`db_jobs::enqueue_in_turn`): the "add to queue" beside a job's own button.
pub async fn enqueue_in_turn(app: &App, request: &JobRequest) -> Result<u64> {
    validate(app, request).await?;
    Ok(db_jobs::enqueue_in_turn(&app.pool, request.job_type(), request.payload()).await?)
}

/// `POST /api/v1/jobs/clear`: which finished jobs to remove from the log.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ClearRequest {
    /// Any of done / failed / cancelled; absent = all three.
    #[serde(default)]
    pub statuses: Option<Vec<String>>,
    /// Only these (the rows selected in the list); absent = every job with those statuses.
    #[serde(default)]
    pub ids: Option<Vec<u64>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClearResult {
    pub removed: u64,
    /// Selected jobs left in place because they are still pending or running.
    pub kept: u64,
}

/// Clears finished jobs from the log; a pending or running one is never removed.
pub async fn clear(app: &App, request: &ClearRequest) -> Result<ClearResult> {
    let statuses: Vec<&str> = match &request.statuses {
        None => db_jobs::FINISHED.to_vec(),
        Some(list) => {
            if let Some(other) = list.iter().find(|s| !db_jobs::FINISHED.contains(&s.as_str())) {
                return Err(Error::invalid(format!("only finished jobs can be cleared (done, failed, cancelled), not {other:?}")));
            }
            list.iter().map(String::as_str).collect()
        }
    };
    let mut removed = db_jobs::clear(&app.pool, &statuses, request.ids.as_deref()).await?;
    // Picked by hand: a job that never started goes as well (removing it is cancelling it),
    // and a running one is asked to stop -- it stays, as cancelled, until cleared.
    // An empty selection clears nothing (and would be `IN ()` below).
    if let Some(ids) = request.ids.as_deref().filter(|ids| !ids.is_empty()) {
        for part in ids.chunks(1000) {
            let list = part.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
            removed += sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM jobs WHERE status = 'pending' AND id IN ({list})"))).execute(&app.pool).await?.rows_affected();
        }
        let running: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT id FROM jobs WHERE status = 'in_progress' AND id IN ({})",
            ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",")
        )))
        .fetch_all(&app.pool)
        .await?;
        for id in running {
            db_jobs::cancel(&app.pool, id).await?;
        }
    }
    let kept = request.ids.as_ref().map_or(0, |ids| (ids.len() as u64).saturating_sub(removed));
    // With the census the worker logs at start, this tells a clear that did not last from
    // jobs that are simply new.
    log::info!("jobs cleared from the log: {removed} removed ({:?}{})", statuses, if request.ids.is_some() { ", selected rows" } else { "" });
    Ok(ClearResult { removed, kept })
}

/// Priority for jobs the user is waiting on (an opened page's metadata/art refresh).
pub const PRIORITY_INTERACTIVE: u16 = 10;

pub async fn enqueue_with_priority(app: &App, request: &JobRequest, priority: u16) -> Result<u64> {
    validate(app, request).await?;
    Ok(db_jobs::enqueue_with_priority(&app.pool, request.job_type(), request.payload(), priority).await?)
}

/// Enqueues `request` unless an identical job (same type and payload) is already pending or
/// running -- a schedule firing while its previous run is still going adds nothing.
/// `Ok(None)` when one was already queued.
pub async fn enqueue_unless_queued(app: &App, request: &JobRequest) -> Result<Option<u64>> {
    let payload = request.payload();
    let rows = sqlx::query("SELECT payload FROM jobs WHERE job_type = ? AND status IN ('pending', 'in_progress')").bind(request.job_type().name()).fetch_all(&app.pool).await?;
    for row in rows {
        let existing: serde_json::Value = sqlx::Row::try_get(&row, "payload")?;
        if existing == payload {
            return Ok(None);
        }
    }
    Ok(Some(enqueue(app, request).await?))
}

/// The same request with its sync mode replaced (bulk syncs only; other jobs unchanged).
pub fn with_mode(request: &JobRequest, mode: SyncModeName) -> JobRequest {
    let mut request = request.clone();
    if let JobRequest::ProviderBulkSync(r) = &mut request {
        r.mode = mode;
    }
    request
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ListQuery {
    /// One job type, or several separated by commas (`file_scan,hash_match`).
    pub job_type: Option<String>,
    pub status: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

pub async fn list(app: &App, q: &ListQuery) -> Result<JobsPage> {
    if let Some(status) = q.status.as_deref() {
        if !db_jobs::STATUSES.contains(&status) {
            return Err(Error::invalid(format!("unknown job status {status:?}")));
        }
    }
    let mut types = Vec::new();
    for name in q.job_type.as_deref().unwrap_or("").split(',').map(str::trim).filter(|n| !n.is_empty()) {
        types.push(JobType::from_name(name).ok_or_else(|| Error::invalid(format!("unknown job type {name:?}")))?);
    }
    let (limit, offset) = clamp_page(q.limit, q.offset);
    let (jobs, total) = db_jobs::list(&app.pool, &types, q.status.as_deref(), limit, offset).await?;
    Ok(JobsPage { jobs, total, limit, offset })
}

pub async fn get(app: &App, id: u64) -> Result<JobRow> {
    db_jobs::get(&app.pool, id).await?.ok_or_else(|| Error::not_found(format!("no job {id}")))
}

pub async fn retry(app: &App, id: u64) -> Result<JobRow> {
    if !db_jobs::retry(&app.pool, id).await? {
        let job = get(app, id).await?;
        return Err(Error::conflict(format!("job {id} is {}, only failed or cancelled jobs can be retried", job.status)));
    }
    get(app, id).await
}

/// `POST /api/v1/jobs/run-next`: the jobs to move to the front of their lanes.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RunNextRequest {
    pub ids: Vec<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunNextResult {
    /// Per job asked for: where it now stands in its lane (1 = starts next), `None` when it
    /// was not pending (running or finished jobs are never moved).
    pub positions: Vec<RunNextPosition>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunNextPosition {
    pub id: u64,
    pub position: Option<u64>,
}

/// "Run next": pending jobs to the front of their lanes, keeping the order among them. They
/// start after the job running in their lane; jobs in other lanes are not affected.
pub async fn run_next(app: &App, ids: &[u64]) -> Result<RunNextResult> {
    if ids.is_empty() {
        return Err(Error::invalid("no jobs given"));
    }
    let moved = db_jobs::run_next(&app.pool, ids).await?;
    Ok(RunNextResult { positions: moved.into_iter().map(|(id, position)| RunNextPosition { id, position }).collect() })
}

/// One job's "Run next"; a job that is not pending is a conflict.
pub async fn run_next_one(app: &App, id: u64) -> Result<RunNextPosition> {
    let job = get(app, id).await?;
    if job.status != "pending" {
        return Err(Error::conflict(format!("job {id} is {}, only a pending job can be moved", job.status)));
    }
    let result = run_next(app, &[id]).await?;
    Ok(result.positions.into_iter().next().unwrap_or(RunNextPosition { id, position: None }))
}

/// Cancels a job: a pending one never runs; a running one is flagged and the worker aborts
/// it within a tick. Returns the job row and which of the two happened.
pub async fn cancel(app: &App, id: u64) -> Result<(JobRow, db_jobs::CancelOutcome)> {
    match db_jobs::cancel(&app.pool, id).await? {
        Some(outcome) => Ok((get(app, id).await?, outcome)),
        None => {
            let job = get(app, id).await?;
            Err(Error::conflict(format!("job {id} is {}, only pending or running jobs can be cancelled", job.status)))
        }
    }
}

pub async fn counts(app: &App) -> Result<Vec<StatusCount>> {
    Ok(db_jobs::status_counts(&app.pool).await?.into_iter().map(|(status, count)| StatusCount { status, count }).collect())
}

/// The job types and scan modes a UI can offer, by name.
#[derive(Debug, Clone, Serialize)]
pub struct JobVocabulary {
    pub job_types: Vec<&'static str>,
    pub scan_modes: Vec<&'static str>,
    pub statuses: Vec<&'static str>,
    pub dat_sources: Vec<&'static str>,
    pub providers: Vec<&'static str>,
    pub raw_retention: Vec<&'static str>,
    /// Job types a `client` installation may not run (`JobType::writes_catalog`), so a
    /// screen can dim those buttons instead of letting the click be refused.
    pub catalog_writing: Vec<&'static str>,
    /// `metadata_source_priority`'s fields, each with the sources that can supply it, in
    /// the default order (`settings::METADATA_FIELD_SOURCES`). The settings editors build
    /// their lists from this instead of offering every source under every field.
    pub metadata_field_sources: std::collections::BTreeMap<&'static str, Vec<&'static str>>,
}

pub fn vocabulary() -> JobVocabulary {
    JobVocabulary {
        job_types: JobType::ALL.iter().map(|t| t.name()).collect(),
        scan_modes: FileScanMode::ALL.iter().map(|m| m.name()).collect(),
        statuses: db_jobs::STATUSES.to_vec(),
        dat_sources: Source::ALL.iter().map(|s| s.name()).collect(),
        providers: Provider::ALL.iter().map(|p| p.name()).collect(),
        raw_retention: vec!["keep_all", "keep_latest", "discard"],
        catalog_writing: JobType::ALL.iter().filter(|t| t.writes_catalog()).map(|t| t.name()).collect(),
        metadata_field_sources: crate::database::settings::METADATA_FIELD_SOURCES.iter().map(|(field, sources)| (*field, sources.to_vec())).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn file_scan_payload_matches_documented_shape() {
        let r: JobRequest = serde_json::from_value(json!({"job_type": "file_scan", "directory_id": 7, "mode": "new_only", "platform": "all"})).unwrap();
        assert_eq!(r.job_type(), JobType::FileScan);
        assert_eq!(r.payload(), json!({"directory_id": 7, "mode": "new_only", "platform": "all"}));

        let forced: JobRequest = serde_json::from_value(json!({"job_type": "file_scan", "directory_id": 7, "mode": "full", "platform": 18})).unwrap();
        assert_eq!(forced.payload()["platform"], json!(18));
        // An id the backend does not know is no longer refused at this layer: `PlatformSelect`
        // carries the bare number (it is shared with the GUI, which has no `Platform`), and
        // `validate` refuses it with a 400 that names it. What is still refused here is a
        // value that is not a platform id at all.
        let unknown: JobRequest = serde_json::from_value(json!({"job_type": "file_scan", "directory_id": 7, "mode": "full", "platform": 65000})).unwrap();
        assert_eq!(unknown.payload()["platform"], json!(65000));
        assert!(serde_json::from_value::<JobRequest>(json!({"job_type": "file_scan", "directory_id": 7, "mode": "full", "platform": 70000})).is_err(), "out of u16 range");
        assert!(serde_json::from_value::<JobRequest>(json!({"job_type": "file_scan", "directory_id": 7, "mode": "full", "platform": "sixty"})).is_err());
        assert!(serde_json::from_value::<JobRequest>(json!({"job_type": "file_scan", "directory_id": 7, "mode": "quick"})).is_err());

        // The Scan screen's DAT / Hasheous / PlayMatch boxes ride along for the follow-up match.
        let with: JobRequest = serde_json::from_value(json!({"job_type": "file_scan", "directory_id": 7, "mode": "rehash", "hash_match": {"to_dat": true, "to_hasheous": true, "to_playmatch": false}})).unwrap();
        assert_eq!(with.payload()["hash_match"], json!({"to_dat": true, "to_hasheous": true, "to_playmatch": false}));
    }

    #[test]
    fn hash_match_defaults_and_unit_variant() {
        let r: JobRequest = serde_json::from_value(json!({"job_type": "hash_match", "directory_id": 3})).unwrap();
        assert_eq!(r.payload(), json!({"directory_id": 3, "to_dat": true, "to_hasheous": true, "to_playmatch": false}));
        let o: JobRequest = serde_json::from_value(json!({"job_type": "db_optimize"})).unwrap();
        assert_eq!(o.job_type(), JobType::DbOptimize);
        assert_eq!(o.payload(), json!({}));
    }

    #[test]
    fn dat_sync_defaults_import_on() {
        let r: JobRequest = serde_json::from_value(json!({"job_type": "dat_sync", "source": "redump"})).unwrap();
        assert_eq!(r.payload(), json!({"source": "redump", "limit": null, "import": true}));
    }
}
