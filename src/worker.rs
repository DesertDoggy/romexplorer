//! The background worker from docs/plan/database.md: talks to the rest of the app only
//! through the `jobs` table, dispatches by `job_type`, records the outcome. Runs inside
//! `romexplorer_server` by default (its own tasks, independent of HTTP traffic -- it keeps
//! working with no browser open and stops only when the server is deliberately shut down),
//! or as the standalone `romexplorer_worker` process. Restarts just re-claim whatever is
//! still pending.
//!
//! It is also the app's only outgoing path to metadata providers, and it runs jobs in
//! **lanes** (`jobs::lane_of`): one job at a time per lane, lanes concurrently. Every
//! provider has its own lane, so that provider's client-side rate limiter (IGDB: 4/s) sees
//! all of the app's traffic to it as one serial stream; CPU-bound scan/match/optimize jobs
//! share the `cpu` lane; DAT downloads the `dat` lane; the title merge pass has `titles` to
//! itself, since it and a scan touch different tables. The scheduler peeks pending jobs
//! oldest-first and claims only those whose lane is idle (`jobs::claim_by_id`), so a long
//! IGDB bulk sync never holds up a RetroAchievements sync or a file scan. Run one worker per
//! set of credentials -- a second process would get a second rate-limit budget.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use sqlx::MySqlPool;

use crate::api::{self, App};
use crate::common::models::Platform;
use crate::database::dat_download::{self, Source};
use crate::database::jobs::{self, Job, JobType, Progress};
use crate::database::providers::raw_store::RawStore;
use crate::database::providers::{bulk, cache, dumps, media, metadata, Provider};
use crate::database::{import, settings, DatabaseError};
use sqlx::Row;

/// The shared `api::App` (pool, paths, provider clients) plus the raw-response store.
pub struct Worker {
    app: App,
    store: RawStore,
}

#[derive(Debug, Clone)]
pub struct WorkerOptions {
    /// How long to wait between looks at the queue when nothing started.
    pub poll: Duration,
    /// Drain what's pending, then return (tests).
    pub once: bool,
}

impl WorkerOptions {
    /// `ROMEXPLORER_WORKER_POLL_SECS` (default 5), `ROMEXPLORER_WORKER_ONCE=1`.
    pub fn from_env() -> Self {
        Self {
            poll: Duration::from_secs(std::env::var("ROMEXPLORER_WORKER_POLL_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(5)),
            once: std::env::var("ROMEXPLORER_WORKER_ONCE").map(|v| v == "1").unwrap_or(false),
        }
    }
}

impl Worker {
    pub async fn new(app: App) -> Result<Arc<Self>, DatabaseError> {
        for (provider, why) in &app.clients().unavailable {
            log::info!("provider {} unavailable: {why}", provider.name());
        }
        let store = RawStore::new(app.paths.provider_cache_dir(), settings::raw_retention(&app.pool).await?);
        Ok(Arc::new(Self { app, store }))
    }
}

/// The advisory lock every worker in a collection competes for. One worker per database:
/// two would each get a full provider rate-limit budget and between them exceed it, which
/// is the whole reason `WorkerOptions`' lanes exist.
pub const WORKER_LOCK: &str = "romexplorer_worker";

/// Takes [`WORKER_LOCK`], or `None` if another process already holds it.
///
/// The connection is handed back rather than dropped on purpose: MariaDB releases a
/// `GET_LOCK` when the session that took it ends, so *holding* the lock means holding this
/// connection. That also means a crashed or killed process frees it by itself -- there is
/// no stale lock file to clean up.
pub async fn take_worker_lock(pool: &MySqlPool) -> Option<sqlx::pool::PoolConnection<sqlx::MySql>> {
    use sqlx::Row;
    let mut conn = pool.acquire().await.ok()?;
    let row = sqlx::query("SELECT GET_LOCK(?, 0)").bind(WORKER_LOCK).fetch_one(&mut *conn).await.ok()?;
    let got: Option<i64> = row.try_get(0).ok()?;
    (got == Some(1)).then_some(conn)
}

/// The lane scheduler. Runs until `stop` turns true (or, with `options.once`, until nothing
/// is pending or running). A database error while polling is logged and retried on the
/// next tick rather than ending the worker. On stop it claims nothing new, aborts the jobs
/// still running and puts them straight back to `pending` (`jobs::release_interrupted`), so
/// the next start picks them up at once -- except a copy, which is cancelled for the user to
/// start again.
pub async fn run_scheduler(worker: Arc<Worker>, options: WorkerOptions, mut stop: tokio::sync::watch::Receiver<bool>) {
    let pool = worker.app.pool.clone();
    let mut running: HashMap<String, (u64, tokio::task::JoinHandle<()>)> = HashMap::new();
    let mut ticks: u64 = 0;
    // Held while this worker lives, so a job it claimed is known to have a task behind it
    // (`jobs::release_orphans`, `jobs::cancel`).
    let mut alive = jobs::hold_worker_lock(&pool).await;
    // What the job log holds as the worker starts: if jobs the user cleared show up again
    // after a restart, this line says whether they are really back in this database.
    match jobs::census(&pool).await {
        Ok(c) => log::info!("worker start: job log has {} finished, {} pending, {} running; highest id {}", c.finished, c.pending, c.running, c.max_id.map_or("none".to_string(), |id| id.to_string())),
        Err(e) => log::warn!("worker start: could not count the job log: {e}"),
    }
    loop {
        if *stop.borrow() {
            break;
        }
        // A restarted database dropped the lock with its connections: take it again.
        if !jobs::worker_lock_held(&pool).await.unwrap_or(true) {
            alive = jobs::hold_worker_lock(&pool).await;
            log::info!("worker: lock taken again ({})", if alive.is_some() { "held" } else { "not yet" });
        }
        running.retain(|_, (_, handle)| !handle.is_finished());
        let started = match tick(&worker, &mut running, ticks).await {
            Ok(n) => n,
            Err(e) => {
                log::warn!("worker: polling the job queue failed, retrying: {e}");
                0
            }
        };
        ticks += 1;
        if started == 0 {
            if options.once && running.is_empty() {
                return;
            }
            let wait = if options.once { Duration::from_millis(300) } else { options.poll };
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = stop.changed() => {}
            }
        }
    }
    let interrupted: Vec<u64> = running
        .into_values()
        .filter(|(_, handle)| !handle.is_finished())
        .map(|(id, handle)| {
            handle.abort();
            id
        })
        .collect();
    if !interrupted.is_empty() {
        log::info!("worker stopping: {} running job(s) interrupted, re-queued or (copies) cancelled: {interrupted:?}", interrupted.len());
        if let Err(e) = jobs::release_interrupted(&pool, &interrupted).await {
            log::warn!("worker: could not re-queue interrupted jobs (the next worker will): {e}");
        }
    }
    drop(alive);
}

static SCHEDULES_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Enqueues what the schedules say is due: a fixed-interval run, or -- for a check trigger
/// -- a diff run when the provider's cheap change check reports a change. An identical
/// job still pending or running is never queued twice.
async fn run_due_schedules(worker: &Arc<Worker>) -> Result<(), DatabaseError> {
    let app = &worker.app;
    let now = chrono::Local::now().naive_local();
    enqueue_gap_fill_if_idle(app).await?;
    for (s, fixed, check) in crate::database::schedules::due(&app.pool, now).await? {
        // Asked each time round, not once for the pass: the schedule before this one may
        // just have queued a job, and two idle-only schedules due together run in turn.
        if s.spec.when_idle && jobs::queue_busy(&app.pool).await? {
            log::debug!("schedule {} ({}): due, waiting for the queue to be idle", s.id, s.spec.name);
            continue;
        }
        let request: api::jobs::JobRequest = match serde_json::from_value(s.spec.target.clone()) {
            Ok(r) => r,
            Err(e) => {
                crate::database::schedules::checked(&app.pool, &s, &format!("invalid target: {e}"), None, now).await?;
                continue;
            }
        };
        if fixed {
            let mode = if s.spec.mode == "redownload" { api::jobs::SyncModeName::Redownload } else { api::jobs::SyncModeName::Diff };
            let job = match api::jobs::enqueue_unless_queued(app, &api::jobs::with_mode(&request, mode)).await {
                Ok(job) => job,
                Err(e) => {
                    log::warn!("schedule {} ({}): enqueue failed: {e}", s.id, s.spec.name);
                    None
                }
            };
            log::info!("schedule {} ({}): fixed run -> {}", s.id, s.spec.name, job.map_or("already queued".to_string(), |j| format!("job {j}")));
            crate::database::schedules::fixed_ran(&app.pool, &s, job, now).await?;
        }
        if check {
            let result = crate::database::providers::change_check::check(&app.clients(), &app.http, &app.pool, &s.spec.target).await?;
            let job = match &result {
                crate::database::providers::change_check::CheckResult::Changed(_) => api::jobs::enqueue_unless_queued(app, &api::jobs::with_mode(&request, api::jobs::SyncModeName::Diff)).await.unwrap_or(None),
                _ => None,
            };
            log::info!("schedule {} ({}): check {} -> {}", s.id, s.spec.name, result.summary(), job.map_or("no job".to_string(), |j| format!("job {j}")));
            crate::database::schedules::checked(&app.pool, &s, &result.summary(), job, now).await?;
        }
    }
    Ok(())
}

/// The background PlayMatch gap filler runs only while the network is otherwise idle: no
/// provider/DAT job pending or running (and not a gap fill already). The
/// `playmatch_batch_size` setting sizes the batch; -1 turns it off.
async fn enqueue_gap_fill_if_idle(app: &api::App) -> Result<(), DatabaseError> {
    let Some(limit) = crate::database::settings::playmatch_batch_size(&app.pool).await? else { return Ok(()) };
    let busy = sqlx::query("SELECT 1 FROM jobs WHERE status IN ('pending', 'in_progress') AND job_type IN ('provider_bulk_sync', 'dat_sync', 'provider_query', 'playmatch_fill') LIMIT 1").fetch_optional(&app.pool).await?.is_some();
    if busy {
        return Ok(());
    }
    // Only enqueue when there is something to do (one candidate is enough to know).
    if api::scan::playmatch_fill_candidates(app, 1).await.map(|c| c.is_empty()).unwrap_or(true) {
        return Ok(());
    }
    let id = jobs::enqueue(&app.pool, JobType::PlaymatchFill, serde_json::json!({ "limit": limit })).await?;
    log::info!("network idle: queued PlayMatch gap fill job {id} (batch {})", if limit == 0 { "unlimited".to_string() } else { limit.to_string() });
    Ok(())
}

/// One look at the queue: re-queue stale claims now and then, start every pending job whose
/// lane is idle. Returns how many started.
/// How long a job that stops itself ([`stops_itself`]) gets after its cancel before its task
/// is aborted anyway.
const WIND_DOWN: Duration = Duration::from_secs(120);
/// Running jobs that stop themselves on a cancel, by id.
static COOPERATIVE: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<u64>>> = std::sync::LazyLock::new(Default::default);
/// Those cancelled and not yet ended, with when the cancel was seen.
static WINDING_DOWN: std::sync::LazyLock<std::sync::Mutex<HashMap<u64, std::time::Instant>>> = std::sync::LazyLock::new(Default::default);

/// Job types whose run watches for its own cancel and returns: a copy (`copy::run::Cancel`)
/// and a scan (`rom::multiple::Multiple::cancel`).
fn stops_itself(job_type: JobType) -> bool {
    matches!(job_type, JobType::CopyFiles | JobType::FileScan)
}

async fn tick(worker: &Arc<Worker>, running: &mut HashMap<String, (u64, tokio::task::JoinHandle<()>)>, ticks: u64) -> Result<usize, DatabaseError> {
    let pool = &worker.app.pool;
    // Cancels requested for running jobs: abort the task (an open transaction rolls back,
    // a half-written download stays a .part file) and free the lane.
    //
    // Except a job that stops itself (a copy, a scan): aborting its task leaves its blocking
    // work running with nobody to tell it, and skips what it does on the way out (a copy
    // records the files it finished). Such a job is marked cancelled, which its own watcher
    // sees, and keeps its lane until it has wound down -- or is aborted after `WIND_DOWN`.
    let running_ids: Vec<u64> = running.values().map(|(id, _)| *id).collect();
    {
        WINDING_DOWN.lock().unwrap().retain(|id, _| running_ids.contains(id));
        COOPERATIVE.lock().unwrap().retain(|id| running_ids.contains(id));
    }
    for id in jobs::cancel_requested(pool, &running_ids).await? {
        if let Some(lane) = running.iter().find(|(_, (j, _))| *j == id).map(|(lane, _)| lane.clone()) {
            if COOPERATIVE.lock().unwrap().contains(&id) {
                let first = WINDING_DOWN.lock().unwrap().get(&id).copied();
                match first {
                    None => {
                        WINDING_DOWN.lock().unwrap().insert(id, std::time::Instant::now());
                        jobs::mark_cancelled(pool, id).await?;
                        log::info!("job {id}: cancelled on request, winding down (lane {lane})");
                        continue;
                    }
                    Some(since) if since.elapsed() < WIND_DOWN => continue,
                    Some(_) => log::warn!("job {id}: still running {}s after its cancel, aborting it", WIND_DOWN.as_secs()),
                }
            }
            if let Some((_, handle)) = running.remove(&lane) {
                handle.abort();
            }
            jobs::mark_cancelled(pool, id).await?;
            log::info!("job {id}: cancelled on request (lane {lane} freed)");
        }
    }
    // Running jobs with no task behind them: a cancel is done, the rest run again. Left
    // alone, such a job could not be cancelled and came back on its own two hours later.
    let orphans = jobs::release_orphans(pool, &running_ids).await?;
    if orphans != jobs::Orphans::default() {
        log::warn!("jobs left running with no worker behind them: cancelled {:?}, queued again {:?}, failed {:?}", orphans.cancelled, orphans.requeued, orphans.failed);
    }
    if ticks % 12 == 0 {
        // Schedules and the background gap filler: about once a minute, as their own
        // task (a change check makes a network request) and never two at once.
        if !SCHEDULES_RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
            let w = Arc::clone(worker);
            tokio::spawn(async move {
                if let Err(e) = run_due_schedules(&w).await {
                    log::warn!("schedules: {e}");
                }
                SCHEDULES_RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
            });
        }
    }
    let mut started = 0;
    for pending in jobs::peek_lane_heads(pool).await? {
        if running.contains_key(&pending.lane) {
            continue;
        }
        let Some(job) = jobs::claim_by_id(pool, pending.id).await? else { continue };
        log::info!("job {} ({}) -> lane {}", job.id, job.job_type.name(), pending.lane);
        if stops_itself(job.job_type) {
            COOPERATIVE.lock().unwrap().insert(job.id);
        }
        let w = Arc::clone(worker);
        let id = job.id;
        running.insert(pending.lane.clone(), (id, tokio::spawn(async move { w.run(job).await })));
        started += 1;
    }
    // Then, only with every lane idle -- nothing running, and nothing ordinary that could
    // be started just now -- the oldest job that waits its turn. Its follow-up jobs are
    // ordinary ones, queued before its task ends, so they run before the next in line.
    if running.is_empty() {
        if let Some(pending) = jobs::peek_next_in_turn(pool).await? {
            if let Some(job) = jobs::claim_by_id(pool, pending.id).await? {
                log::info!("job {} ({}) -> lane {} (its turn)", job.id, job.job_type.name(), pending.lane);
                if stops_itself(job.job_type) {
                    COOPERATIVE.lock().unwrap().insert(job.id);
                }
                let w = Arc::clone(worker);
                let id = job.id;
                running.insert(pending.lane.clone(), (id, tokio::spawn(async move { w.run(job).await })));
                started += 1;
            }
        }
    }
    Ok(started)
}

fn payload_str<'a>(job: &'a Job, key: &str) -> Option<&'a str> {
    job.payload.get(key).and_then(|v| v.as_str())
}

fn payload_u64(job: &Job, key: &str) -> Option<u64> {
    job.payload.get(key).and_then(|v| v.as_u64())
}

/// Whether a bulk sync runs as a whole-database fetch, given what the request said and what
/// the `provider_sync_scope` setting says for this provider.
///
/// The sources a finished scan's hash match uses: what its payload asked for, or DAT only.
fn scan_hash_match(job: &Job) -> romexplorer_types::jobs::ScanHashMatch {
    job.payload.get("hash_match").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default()
}

/// The request wins when it speaks: a schedule or a button that asked for `full: true` or
/// `full: false` meant it. Only when the payload is silent does the setting apply, and the
/// only value that changes anything is `full` -- `by_platform` is what a silent request
/// already does (walk every platform the catalogue has), and `single`/`list`/`by_keyword`
/// are per-lookup scopes, not bulk ones, so they fall through to that same walk rather than
/// refusing a run. A provider that cannot do a full fetch falls through too: a setting is not
/// a reason to fail a scheduled job. An explicit `full: true` on such a provider is still an
/// error, as before -- that one was asked for.
fn wants_full_sync(explicit: Option<bool>, setting: settings::SyncScope, provider_can_full: bool) -> bool {
    match explicit {
        Some(asked) => asked,
        None => setting == settings::SyncScope::Full && provider_can_full,
    }
}

fn payload_platform(job: &Job) -> Option<Platform> {
    payload_u64(job, "platform").and_then(|id| Platform::from_igdb_id(id as u16))
}

impl Worker {
    /// One job, start to recorded outcome. A DB error while recording the outcome is
    /// logged; `jobs::release_orphans` puts the row right on the next tick if it was left
    /// claimed.
    async fn run(&self, job: Job) {
        // Something to show from the first moment: a job's own first report can be seconds
        // away (a big query, a download starting), and an empty row reads as stuck.
        Progress::new(&self.app.pool, job.id).force(0, None, &format!("{}: starting", job.job_type.name())).await;
        let outcome = self.dispatch(&job).await;
        // Ran to its end, with or without items that failed: what follows a finished job
        // follows this one too.
        let ran_through = outcome.is_ok() || matches!(&outcome, Err(DatabaseError::Partial(_)));
        let recorded = match &outcome {
            Ok(()) => jobs::complete(&self.app.pool, job.id).await,
            // Finished, some items failed: shown as failed with what went wrong, and not
            // run again. A full scan that could not read three files used to be redone from
            // the first byte twice more, to fail on the same three.
            Err(DatabaseError::Partial(message)) => {
                log::warn!("job {} ({}) finished with failures: {message}", job.id, job.job_type.name());
                jobs::fail_final(&self.app.pool, job.id, message).await
            }
            Err(err) => {
                let mut message = err.to_string();
                // Out of file descriptors: say what holds them, so the job's error shows
                // whether it was sockets, files or one folder's worth of something.
                if crate::common::fd_limit::is_out_of_files(&message) {
                    if let Some(summary) = crate::common::fd_limit::open_files_summary() {
                        message.push_str(&format!(" [{summary}]"));
                    }
                }
                log::error!("job {} ({}) failed: {message}", job.id, job.job_type.name());
                jobs::fail(&self.app.pool, &job, &message).await
            }
        };
        if let Err(err) = recorded {
            log::error!("job {}: could not record outcome: {err}", job.id);
        }
        // A scan is followed by a hash match of the same directory, so new files get linked
        // without a second click. Against what the scan was started with (the Scan screen's
        // DAT / Hasheous / PlayMatch boxes); DAT only when it says nothing, like the re-link
        // after a catalogue sync: a schedule is not a reason to start querying providers.
        // All boxes off: no match. Only after a scan that reached its end -- one that broke
        // off left the directory half-recorded, and matching that would just be redone. A
        // few unreadable files do not hold back the rest.
        if ran_through && job.job_type == JobType::FileScan {
            self.queue_hash_match_after_scan(&job).await;
        }
        // Jobs that link titles to IGDB games can reveal titles that are one game under
        // regional names: queue one merge pass (never a second while one is queued).
        let links_igdb = match job.job_type {
            // A hash match only links releases; the merge pass groups by IGDB id, so without
            // any IGDB links it has nothing to work with. A failed match is not followed up.
            // A DAT sync links its new entries from the downloaded Hasheous dumps.
            JobType::HashMatch => outcome.is_ok() && self.igdb_data_exists().await,
            // A DAT sync is also what brings DLC and updates in, and bundling those with
            // their games needs no IGDB.
            JobType::DatSync => ran_through,
            JobType::PlaymatchFill | JobType::IgdbReapply => true,
            JobType::ProviderQuery | JobType::ProviderBulkSync => matches!(payload_str(&job, "provider"), Some("igdb" | "hasheous" | "playmatch")),
            _ => false,
        };
        if links_igdb {
            if let Err(e) = api::jobs::enqueue_unless_queued(&self.app, &api::jobs::JobRequest::TitleDedupe).await {
                log::warn!("could not queue the title merge pass: {e}");
            }
        }
    }

    async fn queue_hash_match_after_scan(&self, job: &Job) {
        let Some(directory_id) = payload_u64(job, "directory_id") else { return };
        let with = scan_hash_match(job);
        if !(with.to_dat || with.to_hasheous || with.to_playmatch) {
            return;
        }
        let request = api::jobs::JobRequest::HashMatch(api::jobs::HashMatchRequest { directory_id, to_dat: with.to_dat, to_hasheous: with.to_hasheous, to_playmatch: with.to_playmatch, all: false });
        match api::jobs::enqueue_unless_queued(&self.app, &request).await {
            Ok(Some(id)) => log::info!("file_scan job {}: queued hash_match job {id} for directory {directory_id}", job.id),
            Ok(None) => {}
            Err(e) => log::warn!("file_scan job {}: could not queue the hash match for directory {directory_id}: {e}", job.id),
        }
    }

    /// After art was detached: "download missing media" over the releases held here, which
    /// attaches the right game's art (reusing files already downloaded).
    async fn queue_media_backfill(&self) {
        let request = api::jobs::JobRequest::MediaBackfill(romexplorer_types::jobs::MediaBackfillRequest { scope: "owned".to_string(), platform: None, ..Default::default() });
        if let Err(e) = api::jobs::enqueue_unless_queued(&self.app, &request).await {
            log::warn!("could not queue the media backfill: {e}");
        }
    }

    async fn igdb_data_exists(&self) -> bool {
        match crate::database::title_merge::has_igdb_links(&self.app.pool).await {
            Ok(exists) => exists,
            Err(e) => {
                log::warn!("could not check for IGDB links before the merge pass: {e}");
                false
            }
        }
    }

    async fn dispatch(&self, job: &Job) -> Result<(), DatabaseError> {
        match job.job_type {
            JobType::DatSync => self.dat_sync(job).await,
            JobType::ProviderQuery => self.provider_query(job).await,
            JobType::ProviderBulkSync => self.provider_bulk_sync(job).await,
            JobType::MediaDownload => self.media_download(job).await,
            JobType::DbOptimize => self.db_optimize(&Progress::new(&self.app.pool, job.id)).await,
            JobType::PlaymatchFill => self.playmatch_fill(job).await,
            JobType::RaAccountSync => {
                let summary = crate::database::providers::ra_account::sync(&self.app.clients(), &self.app.pool, &Progress::new(&self.app.pool, job.id)).await?;
                log::info!("ra_account_sync job {}: {} game(s) for {} in {} request(s)", job.id, summary.games, summary.user, summary.requests);
                // Genres of local games are fetched automatically (the whole list only on request).
                let request = api::jobs::JobRequest::RaGenreFetch(api::jobs::RaGenreFetchRequest { scope: "local".to_string(), limit: None });
                if let Err(e) = api::jobs::enqueue_unless_queued(&self.app, &request).await {
                    log::warn!("ra_account_sync: could not queue the local genre fetch: {e}");
                }
                Ok(())
            }
            JobType::TitleDedupe => {
                use crate::database::title_merge::{self as merge, PassScope};
                let progress = Progress::new(&self.app.pool, job.id);
                // Only what changed since the last completed pass (and what it could merge
                // with), unless there was none, the catalogue was replaced since, or a full
                // pass was asked for (which forgets the mark).
                let start = merge::pass_start(&self.app.pool).await?;
                let stored = merge::stored_pass(&self.app.pool).await?;
                let scope = match merge::usable_pass(stored.clone(), &start) {
                    Some(last) => merge::collect_dirty(&self.app.pool, &last).await?,
                    None => PassScope::full(),
                };
                let what = match &scope.dirty {
                    Some(d) => format!("{} title(s) changed since the last pass", d.len()),
                    None => "full pass".to_string(),
                };
                log::info!("title_dedupe job {}: {what}", job.id);
                // Seven steps, each counting in its own unit (and some of them set-based
                // statements with nothing to count): the row says which step it is on, the
                // bar gives each a seventh, and the live detail lists all seven with how each
                // came out (the Scan and Jobs tabs show it).
                let mut steps = jobs::Steps::new(
                    &progress,
                    &[
                        ("bundle", "Bundle DLC and updates with their games"),
                        ("patched", "One release per patched work"),
                        ("bases", "Link patched releases to their originals"),
                        ("merge", "Merge titles that share a game"),
                        ("versions", "Find each game's official versions"),
                        ("basegames", "Link ports, remakes and add-ons to their base game"),
                        ("families", "Collections, families and definitive versions"),
                    ],
                    &what,
                );
                // First, so the pass below sees each game with its DLC already under it.
                steps.start(1).await;
                let bundle = merge::bundle_addons_in(&self.app.pool, &progress, &scope).await?;
                log::info!("title_dedupe job {}: {}", job.id, serde_json::to_string(&bundle).unwrap_or_default());
                let ambiguous = if bundle.ambiguous > 0 { format!(", {} left alone (more than one game fits)", bundle.ambiguous) } else { String::new() };
                steps.done(format!("{} DLC / update title(s) bundled into {} game(s){ambiguous}", bundle.bundled, bundle.games)).await;
                // One release per patched work, before anything is said about each.
                steps.start(2).await;
                let folded = crate::database::release_merge::fold_patched_works(&self.app.pool, &progress).await?;
                log::info!("title_dedupe job {}: {}", job.id, serde_json::to_string(&folded).unwrap_or_default());
                steps.done(format!("{} release(s) folded by name, {} by their ROMs", folded.by_name, folded.by_content)).await;
                // Then what each patched release is made from, now that it sits where the
                // import and the bundling put it.
                steps.start(3).await;
                let bases = crate::database::release_bases::resolve(&self.app.pool, &progress).await?;
                log::info!("title_dedupe job {}: {}", job.id, serde_json::to_string(&bases).unwrap_or_default());
                steps.done(format!("{} linked by hash, {} exact, {} by IGDB, {} suggested", bases.by_hash, bases.exact, bases.igdb, bases.suggested)).await;
                steps.start(4).await;
                let summary = merge::dedupe_by_igdb_in(&self.app.pool, None, &progress, &scope).await?;
                steps.done(format!("{} merge(s), {} title(s) folded, {} renamed, {} suggestion(s) in the backlog", summary.merges, summary.titles_merged, summary.renamed, summary.suggestions)).await;
                // The official versions IGDB knows that no DAT lists (a PS5 or PC version), so
                // the base-game links below take them in too.
                steps.start(5).await;
                let clients = self.app.clients();
                let media_dir = self.app.paths.media_dir();
                let mut families = crate::database::title_family::run_versions(&self.app.pool, Some(clients.as_ref()), &self.store, Some((&self.app.http, media_dir.as_path())), &progress).await?;
                // `definitive_fetch_versions` = whole_platform: the platforms those versions are on.
                for id in &families.versions.platforms_to_sync {
                    let request = api::jobs::JobRequest::ProviderBulkSync(api::jobs::ProviderBulkSyncRequest { provider: "igdb".to_string(), platform: api::jobs::PlatformSelect::Id(*id), max_pages: None, full: false, mode: api::jobs::SyncModeName::Diff });
                    if let Err(e) = api::jobs::enqueue_unless_queued(&self.app, &request).await {
                        log::warn!("title_dedupe: could not queue the IGDB sync of platform {id}: {e}");
                    }
                }
                let syncs = match families.versions.platforms_to_sync.len() {
                    0 => String::new(),
                    n => format!(", IGDB sync queued for {n} platform(s)"),
                };
                steps.done(format!("{} IGDB game(s) read, {} version(s) added, {} linked{syncs}", families.games_read, families.versions.added, families.versions.linked)).await;
                // Then, with the titles as the merges left them: each port, remake and
                // add-on to its base game, whichever of the two arrived first.
                steps.start(6).await;
                let basegames = crate::database::providers::metadata::link_basegames(&self.app.pool, &progress).await?;
                log::info!("title_dedupe job {}: {}", job.id, serde_json::to_string(&basegames).unwrap_or_default());
                steps.done(format!("{} parent game(s) read, {} title(s) linked to their base game", basegames.parents_read, basegames.linked)).await;
                // Last: collections, the families, and each family's definitive version.
                steps.start(7).await;
                crate::database::title_family::run_families(&self.app.pool, &mut families, &progress).await?;
                log::info!("title_dedupe job {}: {}", job.id, serde_json::to_string(&families).unwrap_or_default());
                steps.done(format!("{} collection link(s), {} root game(s) changed, {} family(ies)", families.bundles, families.roots_changed, families.families)).await;
                steps.finish();
                // Art from games the titles it touched are no longer linked to.
                // The first pass after this came in looks at everything once: art left by links
                // that were put right before anything took it off.
                const STALE_ONCE: &str = "stale_media_0056";
                let first_time = sqlx::query_scalar::<_, String>("SELECT name FROM data_migrations WHERE name = ?").bind(STALE_ONCE).fetch_optional(&self.app.pool).await?.is_none();
                let touched: Option<Vec<u64>> = if first_time { None } else { scope.dirty.as_ref().map(|d| d.iter().copied().collect()) };
                let detached = crate::database::providers::media::detach_stale(&self.app.pool, touched.as_deref()).await?;
                if detached > 0 {
                    log::info!("title_dedupe job {}: detached {detached} piece(s) of art from games no longer linked", job.id);
                    self.queue_media_backfill().await;
                }
                if first_time {
                    sqlx::query("INSERT IGNORE INTO data_migrations (name) VALUES (?)").bind(STALE_ONCE).execute(&self.app.pool).await?;
                }
                merge::clear_dirty(&self.app.pool).await?;
                // Not when a full pass was asked for while this one ran: that one starts over.
                if merge::stored_pass(&self.app.pool).await? == stored {
                    merge::save_pass(&self.app.pool, &start).await?;
                }
                progress.force(1, Some(1), &format!("done ({what}): {} merge(s), {} title(s) folded, {} suggestion(s) in the backlog, {} DLC / update title(s) bundled into {} game(s)", summary.merges, summary.titles_merged, summary.suggestions, bundle.bundled, bundle.games)).await;
                Ok(())
            }
            JobType::IgdbReapply => {
                let only: Option<Vec<u64>> = job.payload.get("title_ids").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|v| v.as_u64()).collect());
                use crate::database::providers::metadata::ReapplyScope;
                let scope = match (&only, job.payload.get("all").and_then(|v| v.as_bool()).unwrap_or(false)) {
                    (Some(ids), _) => ReapplyScope::Titles(ids),
                    (None, true) => ReapplyScope::All,
                    (None, false) => ReapplyScope::Missing,
                };
                let summary = crate::database::providers::metadata::reapply_igdb(&self.app.clients(), &self.app.pool, scope, &Progress::new(&self.app.pool, job.id)).await?;
                log::info!("igdb_reapply job {}: {}", job.id, serde_json::to_string(&summary).unwrap_or_default());
                Ok(())
            }
            JobType::TitleMergeAudit => {
                let summary = crate::database::merge_audit::audit(&self.app.pool, &self.app.clients(), &Progress::new(&self.app.pool, job.id)).await?;
                // Links it removed leave their game's art behind: take it off.
                if summary.links_removed > 0 && crate::database::providers::media::detach_stale(&self.app.pool, None).await? > 0 {
                    self.queue_media_backfill().await;
                }
                log::info!("title_merge_audit job {}: {}", job.id, serde_json::to_string(&summary).unwrap_or_default());
                if summary.merges_decoupled + summary.links_removed > 0 {
                    // Renames / legitimate merges settle again without the wrong links.
                    let _ = api::jobs::enqueue_unless_queued(&self.app, &api::jobs::JobRequest::TitleDedupe).await;
                }
                // Titles linked to their own game again take its metadata.
                if !summary.relinked.is_empty() {
                    let request = api::jobs::JobRequest::IgdbReapply(romexplorer_types::jobs::IgdbReapplyRequest { title_ids: Some(summary.relinked.clone()), ..Default::default() });
                    if let Err(e) = api::jobs::enqueue_unless_queued(&self.app, &request).await {
                        log::warn!("title_merge_audit job {}: could not queue re-applying IGDB metadata: {}", job.id, e.message);
                    }
                }
                Ok(())
            }
            JobType::BackupDatabase | JobType::BackupCatalog | JobType::BackupDatSeed => {
                let kind = match job.job_type {
                    JobType::BackupCatalog => api::backup::Kind::Catalog,
                    JobType::BackupDatSeed => api::backup::Kind::DatSeed,
                    _ => api::backup::Kind::Database,
                };
                let progress = Progress::new(&self.app.pool, job.id);
                let summary = api::backup::create(&self.app, kind, &progress).await?;
                progress.finish(1, Some(1), &format!("{} ({} table(s), {} bytes)", summary.file.name, summary.tables.len(), summary.file.bytes), &summary).await;
                log::info!("backup_{} job {}: {}", kind.name(), job.id, summary.file.name);
                let chain = match api::backup::also_catalog(&self.app).await {
                    Ok(chain) => chain,
                    // A malformed `backup_catalog` value would otherwise silently mean "no",
                    // and the catalogue backup someone thought they had scheduled would just
                    // never appear.
                    Err(e) => {
                        log::warn!("backup: could not read the backup_catalog setting ({e}); not chaining a catalogue backup");
                        false
                    }
                };
                if kind == api::backup::Kind::Database && chain {
                    // A separate job, not a second file written here: it gets its own history
                    // row, its own progress and its own keep-count.
                    let _ = api::jobs::enqueue_unless_queued(&self.app, &api::jobs::JobRequest::BackupCatalog).await;
                }
                Ok(())
            }
            JobType::CatalogSync => {
                let source_id = payload_u64(job, "source_id").unwrap_or(0) as u16;
                if source_id == 0 {
                    return Err(api::Error::invalid("catalog_sync needs a source_id").into());
                }
                let progress = Progress::new(&self.app.pool, job.id);
                let full = job.payload.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
                let summary = api::sync::pull(&self.app, source_id, full, &progress).await?;
                let n = summary.tables.len() as u64;
                let incomplete = if summary.incomplete.is_empty() {
                    String::new()
                } else {
                    format!("; {} table(s) short of rows -- run a full sync to repair: {}", summary.incomplete.len(), summary.incomplete.join(", "))
                };
                progress
                    .finish(
                        n,
                        Some(n),
                        &format!(
                            "{}: {n} table(s){}, {} unchanged, {} unresolved row(s){incomplete}",
                            summary.source,
                            if full { " in full" } else { "" },
                            summary.unchanged.len(),
                            summary.unresolved
                        ),
                        &summary,
                    )
                    .await;
                log::info!("catalog_sync job {}: {}", job.id, serde_json::to_string(&summary).unwrap_or_default());
                Ok(())
            }
            JobType::MediaSync => {
                let source_id = payload_u64(job, "source_id").map(|id| id as u16);
                let progress = Progress::new(&self.app.pool, job.id);
                let summary = api::media_sync::run(&self.app, source_id, &progress).await?;
                progress
                    .finish(
                        summary.considered,
                        Some(summary.considered.max(1)),
                        &format!("{}: {} from the server, {} from providers, {} already here, {} not available", summary.source, summary.from_server, summary.from_provider, summary.already_held + summary.linked_existing, summary.failed),
                        &summary,
                    )
                    .await;
                log::info!("media_sync job {}: {}", job.id, serde_json::to_string(&summary).unwrap_or_default());
                Ok(())
            }
            JobType::RestoreBackup => {
                let name = payload_str(job, "name").unwrap_or_default().to_string();
                let progress = Progress::new(&self.app.pool, job.id);
                let summary = api::restore::restore(&self.app, &name, &progress).await?;
                progress.finish(1, Some(1), &format!("{}: {} table(s) restored", summary.file, summary.tables.len()), &summary).await;
                // A seed brings IGDB ids without the games behind them: fetch those by id,
                // 500 a job like a Hasheous sync does, so the titles get their metadata.
                if summary.kind == api::backup::Kind::DatSeed.name() && self.app.clients().is_available(Provider::Igdb) {
                    let ids: Vec<String> = sqlx::query_scalar(
                        "SELECT DISTINCT tpl.provider_id FROM title_provider_links tpl LEFT JOIN igdb_games g ON g.provider_id = CAST(tpl.provider_id AS UNSIGNED) \
                         WHERE tpl.provider = 'igdb' AND tpl.deleted = FALSE AND tpl.provider_id REGEXP '^[0-9]+$' AND g.provider_id IS NULL",
                    )
                    .fetch_all(&self.app.pool)
                    .await?;
                    for chunk in ids.chunks(500) {
                        jobs::enqueue(&self.app.pool, JobType::ProviderQuery, serde_json::json!({ "provider": "igdb", "provider_ids": chunk })).await?;
                    }
                    log::info!("restore_backup job {}: queued IGDB metadata for {} linked game(s)", job.id, ids.len());
                }
                log::info!("restore_backup job {}: {}", job.id, serde_json::to_string(&summary).unwrap_or_default());
                Ok(())
            }
            JobType::SqliteExport => {
                let kind = payload_str(job, "kind").unwrap_or("all").to_string();
                let progress = Progress::new(&self.app.pool, job.id);
                let summary = api::sqlite::export_kind(&self.app, &kind, &progress).await?;
                progress.finish(1, Some(1), &format!("{} ({} table(s), {} bytes)", summary.file, summary.tables.len(), summary.bytes), &summary).await;
                log::info!("sqlite_export job {}: {}", job.id, summary.file);
                Ok(())
            }
            JobType::SqliteImport => {
                let name = payload_str(job, "name").unwrap_or_default().to_string();
                let adopt = job.payload.get("adopt_ownership").and_then(|v| v.as_bool()).unwrap_or(false);
                let progress = Progress::new(&self.app.pool, job.id);
                let summary = api::sqlite::import_file(&self.app, &name, adopt, &progress).await?;
                let declined = if summary.declined_ownership.is_empty() {
                    String::new()
                } else {
                    format!(", {} ownership table(s) declined (from another installation)", summary.declined_ownership.len())
                };
                progress.finish(1, Some(1), &format!("{} table(s) imported, {} skipped{declined}", summary.tables.len(), summary.skipped.len()), &summary).await;
                log::info!("sqlite_import job {}: {}", job.id, serde_json::to_string(&summary).unwrap_or_default());
                Ok(())
            }
            JobType::IntegrityCheck => {
                let request: romexplorer_types::integrity::IntegrityRequest =
                    serde_json::from_value(job.payload.clone()).map_err(|e| DatabaseError::Config(format!("integrity_check: bad payload: {e}")))?;
                let progress = Progress::new(&self.app.pool, job.id);
                let summary = api::integrity::run(&self.app, &request, &progress, Some(job.id)).await?;
                progress.finish(1, Some(1), &api::integrity::one_liner(&summary), &summary).await;
                log::info!("integrity_check job {}: {}", job.id, serde_json::to_string(&summary).unwrap_or_default());
                Ok(())
            }
            JobType::SheetCreate => {
                let request: romexplorer_types::sheets::SheetCreateRequest =
                    serde_json::from_value(job.payload.clone()).map_err(|e| DatabaseError::Config(format!("sheet_create: bad payload: {e}")))?;
                let progress = Progress::new(&self.app.pool, job.id);
                let summary = api::sheets::run::run_create(&self.app, &request, &progress).await?;
                progress.finish(1, Some(1), &api::sheets::run::create_one_liner(&summary), &summary).await;
                // What was written is recorded at once: a playlist is tied to its game (and
                // used to launch it), and a run after this one knows the dump has its sidecar.
                for directory_id in &summary.directories {
                    let scan = api::jobs::JobRequest::FileScan(api::jobs::FileScanRequest { directory_id: *directory_id, mode: romexplorer_types::jobs::FileScanMode::NewOnly, platform: api::jobs::PlatformSelect::All, hash_match: None });
                    if let Err(e) = api::jobs::enqueue_unless_queued(&self.app, &scan).await {
                        log::warn!("sheet_create job {}: could not queue a scan of directory {directory_id}: {e}", job.id);
                    }
                }
                log::info!("sheet_create job {}: {}", job.id, serde_json::to_string(&summary).unwrap_or_default());
                Ok(())
            }
            JobType::DictionaryBuild => {
                let request: romexplorer_types::jobs::DictionaryBuildRequest = serde_json::from_value(job.payload.clone()).unwrap_or_default();
                let progress = Progress::new(&self.app.pool, job.id);
                let summary = crate::database::dictionary::rebuild(&self.app.pool, &self.app.http, &self.app.paths.provider_cache_dir(), request.wikidata, &progress).await?;
                log::info!("dictionary_build job {}: {}", job.id, serde_json::to_string(&summary).unwrap_or_default());
                Ok(())
            }
            JobType::DisplayData => {
                let request: romexplorer_types::jobs::DisplayDataRequest = serde_json::from_value(job.payload.clone()).unwrap_or_default();
                let progress = Progress::new(&self.app.pool, job.id);
                let summary = crate::api::display_data::run(&self.app, &request, &progress).await.map_err(|e| DatabaseError::Config(e.message))?;
                log::info!("display_data job {}: {}", job.id, crate::api::display_data::one_liner(&summary));
                Ok(())
            }
            JobType::ArtworkExport => {
                let request: romexplorer_types::jobs::ArtworkExportRequest =
                    serde_json::from_value(job.payload.clone()).map_err(|e| DatabaseError::Config(format!("artwork_export: bad payload: {e}")))?;
                let progress = Progress::new(&self.app.pool, job.id);
                let summary = api::artwork::run(&self.app, &request, &progress).await?;
                progress.finish(summary.games, Some(summary.games.max(1)), &api::artwork::one_liner(&summary), &summary).await;
                log::info!("artwork_export job {}: {}", job.id, serde_json::to_string(&summary).unwrap_or_default());
                Ok(())
            }
            JobType::AiBatchRate => {
                let request: romexplorer_types::jobs::AiBatchRateRequest =
                    serde_json::from_value(job.payload.clone()).map_err(|e| DatabaseError::Config(format!("ai_batch_rate: bad payload: {e}")))?;
                let progress = Progress::new(&self.app.pool, job.id);
                let summary = api::definitive::batch_rate(&self.app, &request, &progress).await.map_err(|e| DatabaseError::Config(e.message))?;
                log::info!("ai_batch_rate job {}: {}", job.id, api::definitive::batch_one_liner(&summary));
                Ok(())
            }
            JobType::AudioAutotag => {
                let summary = api::audio_autotag::run(&self.app, &Progress::new(&self.app.pool, job.id)).await?;
                log::info!("audio_autotag job {}: {}", job.id, serde_json::to_string(&summary).unwrap_or_default());
                Ok(())
            }
            JobType::AudioScan => {
                let request: romexplorer_types::jobs::AudioScanRequest =
                    serde_json::from_value(job.payload.clone()).map_err(|e| DatabaseError::Config(format!("audio_scan: bad payload: {e}")))?;
                let progress = Progress::new(&self.app.pool, job.id);
                let summary = api::audio::run_scan(&self.app, &request, &progress).await?;
                progress.finish(1, Some(1), &api::audio::one_liner(&summary), &summary).await;
                log::info!("audio_scan job {}: {}", job.id, serde_json::to_string(&summary).unwrap_or_default());
                Ok(())
            }
            JobType::RomAudioIndex => {
                let request: romexplorer_types::jobs::RomAudioIndexRequest =
                    serde_json::from_value(job.payload.clone()).map_err(|e| DatabaseError::Config(format!("rom_audio_index: bad payload: {e}")))?;
                let progress = Progress::new(&self.app.pool, job.id);
                let summary = api::rom_audio::run_index(&self.app, &request, &progress).await?;
                progress.finish(summary.roms_seen, Some(summary.roms_seen.max(1)), &api::rom_audio::one_liner(&summary), &summary).await;
                log::info!("rom_audio_index job {}: {}", job.id, serde_json::to_string(&summary).unwrap_or_default());
                Ok(())
            }
            JobType::RaBadges => {
                let request: romexplorer_types::jobs::RaBadgesRequest =
                    serde_json::from_value(job.payload.clone()).map_err(|e| DatabaseError::Config(format!("ra_badges: bad payload: {e}")))?;
                let progress = Progress::new(&self.app.pool, job.id);
                let summary = api::ra::update_badges(&self.app, &request, &progress).await?;
                progress.finish(summary.games, Some(summary.games.max(1)), &api::ra::badges_one_liner(&summary), &summary).await;
                log::info!("ra_badges job {}: {}", job.id, serde_json::to_string(&summary).unwrap_or_default());
                Ok(())
            }
            JobType::MediaBackfill => {
                let request: romexplorer_types::jobs::MediaBackfillRequest =
                    serde_json::from_value(job.payload.clone()).map_err(|e| DatabaseError::Config(format!("media_backfill: bad payload: {e}")))?;
                let progress = Progress::new(&self.app.pool, job.id);
                let summary = api::media_backfill::run(&self.app, &request, &progress).await?;
                progress.finish(summary.entries, Some(summary.entries.max(1)), &api::media_backfill::one_liner(&summary), &summary).await;
                log::info!("media_backfill job {}: {}", job.id, serde_json::to_string(&summary).unwrap_or_default());
                // Stopped short (an allowance used up, nothing linked to ask for): not done, and
                // not worth retrying as it is -- failed, with the reason.
                match summary.stopped {
                    Some(why) => Err(DatabaseError::Partial(why)),
                    None => Ok(()),
                }
            }
            JobType::AudioSync => {
                let progress = Progress::new(&self.app.pool, job.id);
                let summary = api::audio_sync::run(&self.app, &progress).await?;
                progress.finish(summary.wanted, Some(summary.wanted.max(1)), &api::audio_sync::one_liner(&summary), &summary).await;
                log::info!("audio_sync job {}: {}", job.id, serde_json::to_string(&summary).unwrap_or_default());
                Ok(())
            }
            JobType::SheetVerify | JobType::M3uVerify => {
                let request: romexplorer_types::sheets::SheetVerifyRequest =
                    serde_json::from_value(job.payload.clone()).map_err(|e| DatabaseError::Config(format!("{}: bad payload: {e}", job.job_type.name())))?;
                let progress = Progress::new(&self.app.pool, job.id);
                let summary = api::sheets::run::run_verify(&self.app, &request, &progress, job.job_type == JobType::M3uVerify).await?;
                progress.finish(1, Some(1), &api::sheets::run::verify_one_liner(&summary), &summary).await;
                log::info!("{} job {}: {}", job.job_type.name(), job.id, serde_json::to_string(&summary).unwrap_or_default());
                Ok(())
            }
            JobType::CopyFiles => {
                let request: romexplorer_types::copy::CopyFilesRequest =
                    serde_json::from_value(job.payload.clone()).map_err(|e| DatabaseError::Config(format!("copy_files: bad payload: {e}")))?;
                let progress = Progress::new(&self.app.pool, job.id);
                let summary = api::copy::run::run(&self.app, &request, &progress, Some(job.id)).await?;
                progress.finish(1, Some(1), &api::copy::run::one_liner(&summary), &summary).await;
                log::info!("copy_files job {}: {}", job.id, serde_json::to_string(&summary).unwrap_or_default());
                Ok(())
            }
            JobType::RaPatchDownload => {
                let md5 = payload_str(job, "md5").unwrap_or_default().to_string();
                let summary = crate::database::ra_patches::download(&self.app.pool, &self.app.http, &md5, Some(&Progress::new(&self.app.pool, job.id))).await?;
                log::info!("ra_patch_download job {}: {}", job.id, serde_json::to_string(&summary).unwrap_or_default());
                Ok(())
            }
            JobType::RaGenreFetch => {
                let local_only = payload_str(job, "scope") != Some("all");
                let limit = payload_u64(job, "limit").unwrap_or(0);
                let summary = crate::database::providers::ra_account::fetch_genres(&self.app.clients(), &self.app.pool, local_only, limit, &Progress::new(&self.app.pool, job.id)).await?;
                log::info!("ra_genre_fetch job {}: {}", job.id, serde_json::to_string(&summary).unwrap_or_default());
                Ok(())
            }
            JobType::MediaRelayout => {
                let roots = crate::database::media_store::Roots::load(&self.app.pool, &self.app.paths.media_dir()).await?;
                let summary = crate::database::media_store::relayout(&self.app.pool, &roots, &Progress::new(&self.app.pool, job.id)).await?;
                log::info!("media_relayout job {}: {}", job.id, serde_json::to_string(&summary).unwrap_or_default());
                if summary.failed > 0 {
                    return Err(DatabaseError::Partial(format!("{} of {} media files could not be moved (see log)", summary.failed, summary.candidates)));
                }
                Ok(())
            }
            JobType::FileScan => {
                let req: api::jobs::FileScanRequest = serde_json::from_value(job.payload.clone()).map_err(|e| DatabaseError::Config(format!("file_scan payload: {e}")))?;
                // From when the job was queued, so a scan started again (a restart, a try
                // that broke off) goes on from where it was.
                let queued_at = jobs::get(&self.app.pool, job.id).await?.map(|row| row.created_at);
                let summary = api::scan::run_file_scan_since(&self.app, &req, queued_at, &Progress::new(&self.app.pool, job.id)).await?;
                log::info!("file_scan job {}: {}", job.id, serde_json::to_string(&summary).unwrap_or_default());
                if !summary.errors.is_empty() {
                    return Err(DatabaseError::Partial(format!("{} of {} candidates failed; first: {}", summary.errors.len(), summary.candidates, summary.errors[0])));
                }
                Ok(())
            }
            JobType::HashMatch => {
                let req: api::jobs::HashMatchRequest = serde_json::from_value(job.payload.clone()).map_err(|e| DatabaseError::Config(format!("hash_match payload: {e}")))?;
                let summary = api::scan::run_hash_match(&self.app, &req, &Progress::new(&self.app.pool, job.id)).await?;
                log::info!("hash_match job {}: {}", job.id, serde_json::to_string(&summary).unwrap_or_default());
                Ok(())
            }
        }
    }

    /// payload: {"source": "<name>" | absent for all, "limit": n?, "import": true?, "offline": false?}
    ///
    /// "All" is every source a sync downloads (`Source::all_synced`): the manual ones
    /// (hacks, homebrew, etc) are imported only when named.
    ///
    /// Offline, nothing is fetched: each source's files already under the DAT directory --
    /// the newest copy of each DAT (`dat_download::latest_versions`) -- are imported if they
    /// are not yet. That is a clean install restoring the DAT files it was handed.
    async fn dat_sync(&self, job: &Job) -> Result<(), DatabaseError> {
        let sources: Vec<Source> = match payload_str(job, "source") {
            Some(name) => vec![Source::from_name(name).ok_or_else(|| DatabaseError::Config(format!("unknown source {name:?}")))?],
            None => Source::all_synced(),
        };
        let progress = Arc::new(Progress::new(&self.app.pool, job.id));
        // `dat_sync_platforms`: other platforms' files are not downloaded where a source has
        // a file per platform, and not imported from the packs that have them all.
        // Ticked on the job's form, else the setting.
        let ticked: Option<Vec<u16>> = job.payload.get("platforms").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_u64()).filter_map(|x| u16::try_from(x).ok()).collect());
        let (platform_mode, platform_ids) = match ticked {
            Some(ids) => ("ticked on the job".to_string(), Some(ids)),
            None => crate::database::settings::dat_sync_platforms(&self.app.pool).await?,
        };
        let platforms = platform_ids.map(|ids| Arc::new(ids.into_iter().collect::<std::collections::HashSet<u16>>()));
        if let Some(set) = &platforms {
            log::info!("dat_sync: platforms {platform_mode}, {} platform(s)", set.len());
        }
        // The DATs without a platform (audio CDs, scans, source code, ...): the job's, else the setting's.
        let extras: Option<Vec<String>> = match job.payload.get("extras").and_then(|v| v.as_array()) {
            Some(a) => Some(a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()),
            None => crate::database::settings::dat_sync_extras(&self.app.pool).await?,
        };
        let extras = extras.map(|e| Arc::new(e.into_iter().collect::<std::collections::HashSet<String>>()));
        if let Some(set) = &extras {
            log::info!("dat_sync: {} kind(s) of DAT without a platform", set.len());
        }
        let options = dat_download::DownloadOptions { limit: payload_u64(job, "limit").map(|n| n as usize), progress: Some(Arc::clone(&progress)), platforms: platforms.clone(), extras: extras.clone(), ..Default::default() };
        let wanted = |member: &str| dat_download::wants_dat(platforms.as_deref(), extras.as_deref(), member);
        let client = dat_download::build_client(&options)?;
        let do_import = job.payload.get("import").and_then(|v| v.as_bool()).unwrap_or(true);
        let offline = job.payload.get("offline").and_then(|v| v.as_bool()).unwrap_or(false);
        let count = sources.len() as u64;
        let mut failed: Vec<String> = Vec::new();
        // Files DATs no longer list, set aside (`import::supersede_unlisted`), and the DATs
        // that left too many out to be trusted with that.
        let mut superseded = 0u64;
        let mut held: Vec<String> = Vec::new();
        let mut imported = false;
        // DATs imported again only to mark what they list (music, video, scans).
        let mut reclassified = false;
        // One scale for the whole job: each source is a slice of it, its download the first
        // half of the slice and its import the second. The downloader and the import loop
        // go on reporting in their own units (bytes, files); the frame places them.
        const SLICE: u64 = 1000;
        let whole = count * SLICE;
        for (i, source) in sources.into_iter().enumerate() {
            let base = i as u64 * SLICE;
            progress.set_frame(Some(jobs::Frame { base, span: SLICE / 2, whole }));
            let files = if offline {
                progress.force(0, None, &format!("source {} of {count}: {} -- reading downloaded files", i + 1, source.name())).await;
                let on_disk = dat_download::local_files(&self.app.paths.dats_dir(), source);
                let latest = dat_download::latest_versions(&on_disk);
                log::info!("dat_sync {} (offline): {} file(s) on disk, {} newest", source.name(), on_disk.len(), latest.len());
                latest
                    .into_iter()
                    .filter(|path| path.file_name().and_then(|n| n.to_str()).is_none_or(&wanted))
                    .map(|path| dat_download::Downloaded { bytes: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0), path, fresh: false })
                    .collect()
            } else {
                progress.force(0, None, &format!("source {} of {count}: {} -- fetching", i + 1, source.name())).await;
                let files = dat_download::fetch(&client, source, &self.app.paths.dats_dir(), &options).await?;
                log::info!("dat_sync {}: {} file(s) ({} fresh)", source.name(), files.len(), files.iter().filter(|f| f.fresh).count());
                files
            };
            if do_import || offline {
                // Fresh downloads, plus files an interrupted run downloaded but never imported.
                let mut fresh = Vec::new();
                for file in &files {
                    // A file that can't even be listed is imported anyway (and fails there,
                    // with its name).
                    if file.fresh || !import::is_imported(&self.app.pool, source, &file.path, &wanted).await.unwrap_or(false) {
                        fresh.push(file);
                    }
                }
                // DATs of music, video and scans imported before releases recorded that:
                // those are imported once more, which is all it takes to stamp them.
                let mut restamp: Vec<(&dat_download::Downloaded, Vec<String>)> = Vec::new();
                for file in &files {
                    if fresh.iter().any(|f| f.path == file.path) {
                        continue;
                    }
                    // And DATs imported before their system had a platform here: imported again,
                    // their releases go onto it and the platform-less rows fold in.
                    let mut members = match import::unclassified_members(&self.app.pool, source, &file.path).await {
                        Ok(m) => m,
                        Err(e) => {
                            log::warn!("dat_sync {}: could not check {} for unclassified DATs: {e}", source.name(), file.path.display());
                            Vec::new()
                        }
                    };
                    match import::misplaced_members(&self.app.pool, source, &file.path).await {
                        Ok(m) => {
                            for member in m {
                                if !members.contains(&member) {
                                    members.push(member);
                                }
                            }
                        }
                        Err(e) => log::warn!("dat_sync {}: could not check {} for DATs with no platform: {e}", source.name(), file.path.display()),
                    }
                    if !members.is_empty() {
                        restamp.push((file, members));
                    }
                }
                // The import half of the source's slice, a part of it per file; within a
                // file's part the entries of its DATs move the bar. A source that is one
                // pack (no-intro, tosec) or one big DAT (hacks) has no file count to move
                // along, and its import is most of the job's time.
                let import_frame = jobs::Frame { base: base + SLICE / 2, span: SLICE / 2, whole };
                let per_file = (import_frame.span / fresh.len().max(1) as u64).max(1);
                for (n, file) in fresh.iter().enumerate() {
                    let name = file.path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    let of_files = if fresh.len() > 1 { format!(" (file {} of {})", n + 1, fresh.len()) } else { String::new() };
                    let label = format!("source {} of {count}: {} -- importing {name}{of_files}", i + 1, source.name());
                    progress.set_frame(Some(jobs::Frame { base: (import_frame.base + n as u64 * per_file).min(import_frame.base + import_frame.span), span: per_file, whole }));
                    progress.force(0, None, &label).await;
                    // One bad DAT doesn't stop the rest; the job fails at the end, naming it.
                    match import::import_path_reporting(&self.app.pool, source, &file.path, &wanted, &import::ImportOptions::default(), Some((&*progress, label.as_str()))).await {
                        Ok(summaries) => {
                            imported |= !summaries.is_empty();
                            for (name, s) in summaries {
                                log::info!("imported {name}: {} games, {} new content, {} superseded, {} restored", s.games, s.content_new, s.content_superseded, s.content_restored);
                                superseded += s.content_superseded;
                                if let Some(why) = s.supersede_held {
                                    log::warn!("dat_sync {}: {name}: nothing taken off, {why}", source.name());
                                    held.push(name);
                                }
                            }
                        }
                        Err(e) => {
                            // A pack with some DATs that failed still imported the others.
                            imported |= matches!(e, DatabaseError::Partial(_));
                            log::warn!("dat_sync {}: importing {name} failed: {e}", source.name());
                            failed.push(format!("{name}: {e}"));
                        }
                    }
                }
                progress.set_frame(Some(import_frame));
                reclassified |= !restamp.is_empty();
                for (file, members) in &restamp {
                    let name = file.path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    let label = format!("source {} of {count}: {} -- importing {} DAT(s) in {name} again (kind of material, platform)", i + 1, source.name(), members.len());
                    progress.force(0, None, &label).await;
                    match import::import_path_reporting(&self.app.pool, source, &file.path, &|member| wanted(member) && members.iter().any(|m| m == member), &import::ImportOptions::default(), Some((&*progress, label.as_str()))).await {
                        Ok(summaries) => {
                            let folded: u64 = summaries.iter().map(|(_, s)| s.platformless_folded).sum();
                            log::info!("dat_sync {}: {} DAT(s) in {name} imported again, {folded} platform-less release(s) folded", source.name(), summaries.len());
                        }
                        // Not the sync's failure: the DATs are imported, only not yet told apart.
                        Err(e) => log::warn!("dat_sync {}: importing DATs in {name} again failed: {e}", source.name()),
                    }
                }
            }
        }
        // New DAT lines are new hashes: whatever the downloaded Hasheous dumps know about them
        // links now, at hash confidence, with no file on disk and no request. Without this
        // they waited for the next Hasheous sync. A failure here is not the import's.
        // Releases that have just been marked as disc media, documents or source code and
        // still hold the title their first import gave them: such things have none.
        // Roms linked to releases that now have a platform take it.
        match import::confirm_rom_platforms(&self.app.pool).await {
            Ok(0) => {}
            Ok(n) => log::info!("dat_sync: {n} rom(s) took their release's platform"),
            Err(e) => log::warn!("dat_sync: giving roms their release's platform failed: {e}"),
        }
        if reclassified {
            match crate::database::title_merge::drop_standalone_titles(&self.app.pool).await {
                Ok(gone) => log::info!("dat_sync: {gone} title(s) of disc media, documents and source code removed"),
                Err(e) => log::warn!("dat_sync: removing the titles of disc media, documents and source code failed: {e}"),
            }
        }
        if imported {
            progress.set_frame(Some(jobs::Frame { base: whole.saturating_sub(1), span: 1, whole }));
            progress.force(0, None, "matching the new catalogue entries against the downloaded Hasheous dumps").await;
            match dumps::match_hasheous_index_catalog(&self.app.pool, &self.store, &progress).await {
                Ok((matched, igdb_ids)) => {
                    let ids: Vec<String> = igdb_ids.into_iter().collect();
                    for chunk in ids.chunks(500) {
                        jobs::enqueue(&self.app.pool, JobType::ProviderQuery, serde_json::json!({ "provider": "igdb", "provider_ids": chunk })).await?;
                    }
                    log::info!("dat_sync: {matched} catalogue content(s) answered from the downloaded Hasheous dumps, {} IGDB id(s) queued", ids.len());
                }
                Err(e) => log::warn!("dat_sync: matching the catalogue against the downloaded Hasheous dumps failed: {e}"),
            }
        }
        progress.set_frame(None);
        let mut done = format!("done: {count} source(s)");
        if superseded > 0 {
            done.push_str(&format!(", {superseded} file(s) a DAT changed or dropped set aside as superseded"));
        }
        if !held.is_empty() {
            done.push_str(&format!(", {} DAT(s) left out too many files to take any off ({}{}; see log)", held.len(), held.iter().take(3).cloned().collect::<Vec<_>>().join(", "), if held.len() > 3 { ", ..." } else { "" }));
        }
        if !failed.is_empty() {
            done.push_str(&format!(", {} file(s) failed to import", failed.len()));
        }
        progress.force(count, Some(count), &done).await;
        if !failed.is_empty() {
            return Err(DatabaseError::Partial(format!("{} DAT file(s) failed to import: {}", failed.len(), failed.join("; "))));
        }
        Ok(())
    }

    /// Background PlayMatch gap filler -- see `api::scan::playmatch_fill`. payload
    /// `{"limit": n}` (0 = every candidate).
    async fn playmatch_fill(&self, job: &Job) -> Result<(), DatabaseError> {
        let limit = payload_u64(job, "limit").unwrap_or(100);
        let progress = Progress::new(&self.app.pool, job.id);
        let summary = api::scan::playmatch_fill(&self.app, limit, &progress).await?;
        log::info!("playmatch_fill job {}: {}", job.id, serde_json::to_string(&summary).unwrap_or_default());
        Ok(())
    }

    /// payload: {"provider": name, "name": str?, "content_id": n?, "provider_id": str?, "provider_ids": [str]?, "title_ids": [n]?, "platform": app platform id?}
    async fn provider_query(&self, job: &Job) -> Result<(), DatabaseError> {
        let provider = payload_str(job, "provider").and_then(Provider::from_name).ok_or_else(|| DatabaseError::Config("provider_query: missing/unknown provider".to_string()))?;
        // Progress by requests: the count is known up front for id lists and titles.
        let progress = Progress::new(&self.app.pool, job.id);
        if let Some(ids) = job.payload.get("provider_ids").and_then(|v| v.as_array()) {
            let ids: Vec<String> = ids.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
            return self.provider_query_ids(provider, &ids, &progress).await;
        }
        if let Some(ids) = job.payload.get("title_ids").and_then(|v| v.as_array()) {
            let ids: Vec<u64> = ids.iter().filter_map(|v| v.as_u64()).collect();
            return self.provider_query_titles(provider, &ids, &progress).await;
        }
        if let Some(id) = payload_str(job, "provider_id") {
            return self.provider_query_ids(provider, &[id.to_string()], &progress).await;
        }
        let platform = payload_platform(job);
        match provider {
            Provider::Hasheous => {
                let content_id = payload_u64(job, "content_id").ok_or_else(|| DatabaseError::Config("hasheous query needs content_id".to_string()))?;
                let request = cache::hasheous_request_for_content(&self.app.pool, content_id).await?;
                cache::hasheous_lookup(&self.app.clients(), &self.app.pool, &self.store, content_id, &request).await?;
            }
            Provider::PlayMatch => {
                let content_id = payload_u64(job, "content_id").ok_or_else(|| DatabaseError::Config("playmatch query needs content_id".to_string()))?;
                let request = cache::playmatch_request_for_content(&self.app.pool, content_id).await?;
                let record = cache::playmatch_lookup(&self.app.clients(), &self.app.pool, &self.store, content_id, &request).await?;
                log::info!("playmatch content {content_id}: hit={} match={:?} refs={}", record.hit, record.match_type, record.cross_refs.len());
            }
            _ => {
                let name = payload_str(job, "name").ok_or_else(|| DatabaseError::Config("provider query needs a name".to_string()))?;
                progress.force(0, Some(1), &format!("{}: searching \"{name}\"", provider.name())).await;
                // A free name query caches the results and links the best one fuzzily.
                let entries = cache::search_by_name(&self.app.clients(), &self.app.pool, &self.store, provider, name, platform).await?;
                if let Some(best) = cache::best_hit(&entries, name) {
                    if let Some(title) = &best.title {
                        if provider == Provider::TheGamesDb {
                            crate::database::providers::thegamesdb_region::TgdbRegions::load(&self.app.clients(), self.store.root()).await?;
                        }
                        if let Some(title_id) = cache::link_by_name(&self.app.pool, provider, &best.provider_id, title, platform).await? {
                            self.after_link(provider, &best.provider_id, title_id).await?;
                        }
                    }
                }
                log::info!("provider_query {} by name: {} result(s)", provider.name(), entries.len());
                progress.force(1, Some(1), &format!("{}: done, {} result(s)", provider.name(), entries.len())).await;
            }
        }
        Ok(())
    }

    /// Titles to look up by their own name + platform: the by-name fallback for a provider
    /// no hash source gave an id for. Title-tier providers link the title; a release-tier
    /// provider (TheGamesDB) links the title's releases on that platform.
    async fn provider_query_titles(&self, provider: Provider, title_ids: &[u64], progress: &Progress) -> Result<(), DatabaseError> {
        let pool = &self.app.pool;
        let mut linked = 0usize;
        let total = title_ids.len() as u64;
        for (i, title_id) in title_ids.iter().enumerate() {
            progress.report(i as u64, Some(total), &format!("{}: title {} of {total} ({linked} linked)", provider.name(), i + 1)).await;
            // The user removed this provider's link from the title: no search links it again.
            if cache::unlinked_by_user(pool, *title_id, provider).await? {
                continue;
            }
            let Some(row) = sqlx::query("SELECT t.name, (SELECT MIN(r.app_platform_id) FROM title_releases tr JOIN `release` r ON r.id = tr.release_id WHERE tr.title_id = t.id AND r.deleted = FALSE) AS platform_id FROM title t WHERE t.id = ? AND t.deleted = FALSE").bind(title_id).fetch_optional(pool).await? else { continue };
            let name: String = row.try_get("name")?;
            let platform = row.try_get::<Option<u16>, _>("platform_id")?.and_then(Platform::from_igdb_id);
            // The catalogue first: an IGDB game already cached here (a per-platform bulk sync
            // or an earlier lookup stored it) under exactly this name, for this platform,
            // is the answer a search would return -- without the request. Only an exact
            // name on a known platform qualifies; anything looser still goes to the search,
            // which ranks candidates.
            let mut best: Option<(cache::CachedEntry, cache::Confidence)> = None;
            if provider == Provider::Igdb && platform.is_some() {
                // The name as written and with in-word hyphens joined ("Fu-Ga" -> "FuGa").
                let joined = crate::database::matching::util::join_word_hyphens(&name);
                let rows = sqlx::query("SELECT CAST(provider_id AS CHAR) AS provider_id, title, raw_response_path FROM igdb_games WHERE deleted = FALSE AND title IN (?, ?) LIMIT 20").bind(&name).bind(&joined).fetch_all(pool).await?;
                let local: Vec<cache::CachedEntry> = rows
                    .iter()
                    .map(|r| Ok(cache::CachedEntry { provider, provider_id: r.try_get("provider_id")?, title: r.try_get("title")?, raw_path: r.try_get::<Option<String>, _>("raw_response_path")?.map(std::path::PathBuf::from) }))
                    .collect::<Result<_, DatabaseError>>()?;
                let local = cache::platform_compatible(pool, provider, local, platform).await?;
                best = cache::exact_hit(&local, &name).cloned().map(|e| (e, cache::Confidence::Name));
            }
            if best.is_none() {
                let entries = match cache::search_by_name(&self.app.clients(), pool, &self.store, provider, &name, platform).await {
                    Ok(e) => e,
                    Err(e) => {
                        log::warn!("provider_query {} for title {title_id}: search failed: {e}", provider.name());
                        continue;
                    }
                };
                // Platform-scoped search first; empty -> retry unscoped (providers file some
                // games under a sibling platform, IGDB's Famicom vs NES) but then only an exact
                // name whose entry is for our platform (or a sibling) may link: a same-named
                // port for another system is a wrong link, not a fuzzy one.
                let (candidates, scoped) = if entries.is_empty() && platform.is_some() {
                    match cache::search_by_name(&self.app.clients(), pool, &self.store, provider, &name, None).await {
                        Ok(unscoped) => (unscoped, false),
                        Err(_) => (Vec::new(), false),
                    }
                } else {
                    (entries, true)
                };
                let candidates = cache::platform_compatible(pool, provider, candidates, platform).await?;
                let exact = match cache::exact_hit(&candidates, &name).cloned() {
                    Some(e) => Some(e),
                    None if provider == Provider::Igdb => self.igdb_other_name_hit(&candidates, &name).await?,
                    None => None,
                };
                // No exact name: a result only when it names this game too ("Fuga Hoge" is not
                // "Hoge"), not simply the first a search returned.
                best = match exact {
                    Some(e) => Some((e, cache::Confidence::Name)),
                    None if scoped => candidates.iter().find(|c| c.title.as_deref().is_some_and(|t| crate::database::matching::names_close(&name, t))).cloned().map(|c| (c, cache::Confidence::Fuzzy)),
                    None => None,
                };
            }
            let Some((best, confidence)) = best else {
                log::info!("provider_query {} for title {title_id}: no platform-compatible match", provider.name());
                continue;
            };
            match provider.tier() {
                crate::database::providers::Tier::Title | crate::database::providers::Tier::Content => {
                    if !cache::link_title(pool, *title_id, provider, &best.provider_id, confidence).await? {
                        continue;
                    }
                }
                crate::database::providers::Tier::Release => {
                    let rows = sqlx::query("SELECT tr.release_id FROM title_releases tr JOIN `release` r ON r.id = tr.release_id WHERE tr.title_id = ? AND r.deleted = FALSE AND (? IS NULL OR r.app_platform_id = ?)").bind(title_id).bind(platform.map(|p| p.igdb_id())).bind(platform.map(|p| p.igdb_id())).fetch_all(pool).await?;
                    // TheGamesDB's entry is one region's release: a release already linked to
                    // an entry that fits its own region better keeps that.
                    let regions = if provider == Provider::TheGamesDb { Some(crate::database::providers::thegamesdb_region::TgdbRegions::load(&self.app.clients(), self.store.root()).await?) } else { None };
                    let priority = settings::region_priority(pool).await?;
                    for r in rows {
                        let release_id: u64 = r.try_get("release_id")?;
                        if let Some(regions) = &regions {
                            if !crate::database::providers::thegamesdb_region::should_link(pool, regions, &priority, release_id, &best.provider_id).await? {
                                continue;
                            }
                        }
                        cache::link_release(pool, release_id, provider, &best.provider_id, confidence).await?;
                    }
                }
            }
            linked += 1;
            self.after_link(provider, &best.provider_id, *title_id).await?;
        }
        log::info!("provider_query {} by title name: {linked} of {} title(s) linked", provider.name(), title_ids.len());
        Ok(())
    }

    /// The IGDB candidate one of whose other names -- a regional title ("Gensou Suikoden" for
    /// "Suikoden"), an alternative name -- is exactly `name`: as good a name match as the
    /// main one, where only the main name was compared before and a DAT's Japanese title
    /// fell through to the first search result. Costs at most a couple of list requests.
    async fn igdb_other_name_hit(&self, candidates: &[cache::CachedEntry], name: &str) -> Result<Option<cache::CachedEntry>, DatabaseError> {
        let key = crate::database::matching::util::loose_key(name);
        if key.is_empty() || candidates.is_empty() {
            return Ok(None);
        }
        let mut games = Vec::with_capacity(candidates.len());
        for c in candidates {
            let Some(id) = c.provider_id.parse::<i32>().ok() else { continue };
            if let Some(game) = cache::load_cached_igdb_game(&self.app.pool, id).await? {
                games.push((c, game));
            }
        }
        let mut names = metadata::IgdbNames::new();
        let only_games: Vec<_> = games.iter().map(|(_, g)| g.clone()).collect();
        if let Err(e) = names.resolve_names(&self.app.clients(), &only_games).await {
            log::warn!("igdb: resolving other names for \"{name}\" failed: {e}");
            return Ok(None);
        }
        Ok(games.into_iter().find(|(_, g)| names.other_names(g).into_iter().any(|n| crate::database::matching::util::loose_key(n) == key)).map(|(c, _)| c.clone()))
    }

    /// Metadata + art for a freshly linked entry, from what the search already cached (a
    /// full by-id fetch would be another request for the same data).
    async fn after_link(&self, provider: Provider, provider_id: &str, title_id: u64) -> Result<(), DatabaseError> {
        let pool = &self.app.pool;
        match provider {
            Provider::Igdb => {
                let Some(id) = provider_id.parse::<i32>().ok() else { return Ok(()) };
                let Some(game) = cache::load_cached_igdb_game(pool, id).await? else { return Ok(()) };
                let mut names = metadata::IgdbNames::new();
                names.resolve(&self.app.clients(), std::slice::from_ref(&game)).await?;
                metadata::apply_igdb_game(pool, &names, title_id, &game).await?;
                self.attach_art(provider, provider_id, title_id).await?;
            }
            Provider::SteamGridDb | Provider::MobyGames => self.attach_art(provider, provider_id, title_id).await?,
            _ => {}
        }
        Ok(())
    }

    /// Art for every release of `title_id` from one provider entry -- each release judged on
    /// its own (`media::attach_for`), so one that lacks art gets it even when another of the
    /// title's releases already has some.
    async fn attach_art(&self, provider: Provider, provider_id: &str, title_id: u64) -> Result<(), DatabaseError> {
        let targets = media::release_targets(&self.app.pool, title_id, provider, provider_id).await?;
        if targets.is_empty() {
            return Ok(());
        }
        let outcome = media::attach_for(&self.app.http, &self.app.pool, &self.app.paths.media_dir(), &self.app.clients(), &self.store, provider, provider_id, &targets, &media::Ask::Auto).await?;
        log::debug!("{} {provider_id} art for title {title_id}: {outcome:?}", provider.name());
        Ok(())
    }

    /// A list of provider ids (a hash match's cross-refs): fetched as list requests where
    /// the provider has one (IGDB: `get_games_by_id`, 100 per request -- the client's
    /// limiter paces the requests), one by one where it doesn't (RetroAchievements).
    async fn provider_query_ids(&self, provider: Provider, ids: &[String], progress: &Progress) -> Result<(), DatabaseError> {
        let total = ids.len() as u64;
        let ttl_days = settings::cache_ttl_days(&self.app.pool).await?;
        match provider {
            Provider::Igdb => {
                // Ids cached within the TTL are re-read from their raw files; the rest are
                // fetched in list requests of 100. Metadata is applied to every linked title
                // either way, with one name resolver for the whole job.
                //
                // One scale for the whole job, each step a slice of it and reporting entry by
                // entry. It used to count only the requests: a job of 500 ids all in the
                // cache showed "500 from cache, 0 requests" and then nothing at all while it
                // read 500 files and applied them to their titles.
                const WHOLE: u64 = 100;
                let step = |base: u64, span: u64| progress.set_frame(Some(jobs::Frame { base, span, whole: WHOLE }));
                let mut games: Vec<playmatch_deredis::providers::igdb::model::Game> = Vec::new();
                let mut fresh: Vec<i32> = Vec::new();
                step(0, 15);
                for (i, id) in ids.iter().filter_map(|s| s.parse::<i32>().ok()).enumerate() {
                    progress.report(i as u64, Some(total), &format!("IGDB: checking the cache, id {} of {total} ({} found)", i + 1, games.len())).await;
                    if cache::is_cached(&self.app.pool, Provider::Igdb, &id.to_string(), ttl_days).await? {
                        match cache::load_cached_igdb_game(&self.app.pool, id).await? {
                            Some(game) => games.push(game),
                            None => fresh.push(id),
                        }
                    } else {
                        fresh.push(id);
                    }
                }
                let cached = games.len();
                let chunks = api::batch::chunks(&fresh, 100);
                let requests = chunks.len();
                step(15, 25);
                progress.force(0, Some(requests.max(1) as u64), &format!("IGDB: {} ids, {cached} from cache, {requests} request(s) to make", ids.len())).await;
                let mut fetched = 0usize;
                for (i, chunk) in chunks.into_iter().enumerate() {
                    progress.force(i as u64, Some(requests as u64), &format!("IGDB: request {} of {requests} to the provider ({} ids; {fetched} of {} fetched)", i + 1, chunk.len(), fresh.len())).await;
                    let got = cache::igdb_games_by_ids_full(&self.app.clients(), &self.app.pool, &self.store, &chunk).await?;
                    fetched += got.len();
                    games.extend(got.into_iter().map(|(_, g)| g));
                }
                step(40, 5);
                progress.force(0, None, &format!("IGDB: looking up the names of genres, companies and franchises for {} game(s)", games.len())).await;
                let mut names = metadata::IgdbNames::new();
                names.resolve(&self.app.clients(), &games).await?;
                let mut applied = 0usize;
                let mut art = 0u64;
                // Cover ids -> URLs in list requests, only for games whose titles have none yet.
                let mut need_cover: Vec<(u64, playmatch_deredis::providers::igdb::model::Game)> = Vec::new();
                let mut linked: Vec<(u64, &playmatch_deredis::providers::igdb::model::Game)> = Vec::new();
                let rules = settings::MediaRules::load(&self.app.pool).await?;
                step(45, 25);
                let game_count = games.len() as u64;
                for (i, game) in games.iter().enumerate() {
                    progress.report(i as u64, Some(game_count), &format!("IGDB: applying game {} of {game_count} to its titles ({applied} done)", i + 1)).await;
                    for title_id in metadata::titles_linked(&self.app.pool, Provider::Igdb, &game.id.to_string()).await? {
                        metadata::apply_igdb_game(&self.app.pool, &names, title_id, game).await?;
                        applied += 1;
                        linked.push((title_id, game));
                        if game.cover.is_some() {
                            let targets = media::release_targets(&self.app.pool, title_id, Provider::Igdb, &game.id.to_string()).await?;
                            if media::any_wanting(&self.app.pool, &rules, Provider::Igdb, "box", Some("front"), &targets, 1).await? {
                                need_cover.push((title_id, game.clone()));
                            }
                        }
                    }
                }
                step(70, 15);
                // Covers that could not be saved, and the first reason: said in the job's
                // own line, since "0 cover(s)" alone cannot be told from nothing wanted.
                let mut cover_failures = 0u64;
                let mut cover_error: Option<String> = None;
                let wanted_covers = need_cover.len();
                if !need_cover.is_empty() {
                    let cover_count = need_cover.len() as u64;
                    progress.force(0, Some(cover_count), &format!("IGDB: asking the provider for {cover_count} cover(s)")).await;
                    let covers = media::igdb_cover_urls(&self.app.clients(), &need_cover.iter().map(|(_, g)| g.clone()).collect::<Vec<_>>()).await?;
                    for (i, (title_id, game)) in need_cover.iter().enumerate() {
                        progress.report(i as u64, Some(cover_count), &format!("IGDB: downloading cover {} of {cover_count} ({art} attached{})", i + 1, if cover_failures > 0 { format!(", {cover_failures} failed") } else { String::new() })).await;
                        let Some(url) = game.cover.and_then(|c| covers.get(&c)) else { continue };
                        let targets = media::release_targets(&self.app.pool, *title_id, Provider::Igdb, &game.id.to_string()).await?;
                        match media::try_attach_media(&self.app.http, &self.app.pool, &self.app.paths.media_dir(), Provider::Igdb, Some(&game.id.to_string()), url, "box", Some("front"), &targets).await {
                            Ok(linked) => art += linked as u64,
                            Err(e) => {
                                log::warn!("media: igdb box for game {} failed: {e}", game.id);
                                cover_failures += 1;
                                cover_error.get_or_insert_with(|| e.to_string());
                            }
                        }
                    }
                }
                // Where they went, and why there may be none: every title already had a
                // cover on record, or the downloads failed.
                let image_dir = crate::database::media_store::Roots::load(&self.app.pool, &self.app.paths.media_dir()).await?.write_root(crate::database::media_store::RootRole::Image).1.display().to_string();
                let cover_note = match (wanted_covers, cover_failures) {
                    (0, _) => "no cover wanted (every title already has one on record, or box is off in media_download)".to_string(),
                    (_, 0) => format!("covers saved under {image_dir}"),
                    (_, failed) => format!("{failed} cover(s) FAILED to save under {image_dir}: {}", cover_error.as_deref().unwrap_or("unknown error")),
                };
                step(85, 15);
                // Says what this step will really do: both types are off unless `media_download`
                // turns them on, and "screenshots and fan art for 1382 title(s)" read as a
                // download in progress when nothing was being fetched at all.
                let extra_types: Vec<&str> = [("screenshot", "screenshots"), ("fanart", "fan art")].into_iter().filter(|(media_type, _)| rules.downloads(Provider::Igdb.name(), media_type)).map(|(_, label)| label).collect();
                if extra_types.is_empty() {
                    progress.force(0, None, "IGDB: screenshots and fan art are switched off in media_download -- nothing more to download").await;
                } else {
                    progress.force(0, None, &format!("IGDB: downloading {} for {} title(s)", extra_types.join(" and "), linked.len())).await;
                }
                art += crate::database::providers::igdb_localized::attach_for_titles(&self.app.http, &self.app.pool, &self.app.paths.media_dir(), &self.app.clients(), self.store.root(), &linked).await? as u64;
                let extras = media::attach_igdb_extras(&self.app.http, &self.app.pool, &self.app.paths.media_dir(), &self.app.clients(), &linked).await?;
                progress.set_frame(None);
                progress.force(total, Some(total), &format!("IGDB: {applied} title(s) updated, {art} cover(s) and {extras} screenshot / fan art link(s) attached; {cover_note}; {} ids, {cached} from cache, {fetched} fetched in {requests} request(s)", ids.len())).await;
                log::info!("provider_query igdb: {} ids, {cached} from cache, {} fetched in {requests} request(s), metadata applied to {applied} title(s), {art} box front(s) and {extras} screenshot / fan art link(s) attached", ids.len(), fresh.len());
            }
            Provider::RetroAchievements => {
                let mut done = 0usize;
                let mut applied = 0usize;
                let mut cached = 0usize;
                for (i, id) in ids.iter().filter_map(|s| s.parse::<u32>().ok()).enumerate() {
                    progress.report(i as u64, Some(total), &format!("retroachievements: id {} of {total} ({cached} from cache)", i + 1)).await;
                    let game = match cache::load_cached::<crate::database::providers::retroachievements::RaGame>(&self.app.pool, provider, &id.to_string(), ttl_days).await? {
                        Some(game) => {
                            cached += 1;
                            game
                        }
                        None => cache::retroachievements_game(&self.app.clients(), &self.app.pool, &self.store, id).await?.1,
                    };
                    done += 1;
                    if let Some(genre) = game.genre.as_deref() {
                        for title_id in metadata::titles_linked(&self.app.pool, Provider::RetroAchievements, &id.to_string()).await? {
                            metadata::apply_ra_genres(&self.app.pool, title_id, genre).await?;
                            applied += 1;
                        }
                    }
                    // RA's box art is per game entry = per release: attach to the releases linked.
                    if let Some(url) = media::retroachievements_boxart_url(&game) {
                        let rows = sqlx::query("SELECT release_id FROM release_provider_links WHERE provider = 'retroachievements' AND provider_id = ? AND deleted = FALSE").bind(id.to_string()).fetch_all(&self.app.pool).await?;
                        let targets: Vec<media::MediaTarget> = rows.iter().filter_map(|r| r.try_get::<u64, _>("release_id").ok()).map(media::MediaTarget::Release).collect();
                        media::attach_media(&self.app.http, &self.app.pool, &self.app.paths.media_dir(), Provider::RetroAchievements, Some(&id.to_string()), &url, "box", Some("front"), &targets).await?;
                    }
                    // RA's title screen and in-game shot, as media of the same releases. Both
                    // are off unless ticked under media_download, and then nothing is asked of
                    // RA beyond the two image files: the paths are in the game already.
                    let title_screen = media::retroachievements_image_url(game.image_title.as_deref());
                    let ingame = media::retroachievements_image_url(game.image_ingame.as_deref());
                    if title_screen.is_some() || ingame.is_some() {
                        let rows = sqlx::query("SELECT release_id FROM release_provider_links WHERE provider = 'retroachievements' AND provider_id = ? AND deleted = FALSE").bind(id.to_string()).fetch_all(&self.app.pool).await?;
                        let targets: Vec<media::MediaTarget> = rows.iter().filter_map(|r| r.try_get::<u64, _>("release_id").ok()).map(media::MediaTarget::Release).collect();
                        if let Some(url) = title_screen {
                            media::attach_media(&self.app.http, &self.app.pool, &self.app.paths.media_dir(), Provider::RetroAchievements, Some(&id.to_string()), &url, "title_screen", None, &targets).await?;
                        }
                        if let Some(url) = ingame {
                            media::attach_media_list(&self.app.http, &self.app.pool, &self.app.paths.media_dir(), Provider::RetroAchievements, Some(&id.to_string()), &[url], "screenshot", &targets).await?;
                        }
                    }
                }
                log::info!("provider_query retroachievements: {done} of {} ids fetched, genres applied to {applied} title(s)", ids.len());
            }
            // Single-id endpoints: one request per id, still serial in the provider's lane.
            Provider::SteamGridDb => {
                let clients = self.app.clients();
                let client = clients.steamgriddb.as_ref().ok_or_else(|| DatabaseError::Provider("steamgriddb client unavailable".to_string()))?;
                let mut done = 0usize;
                let mut cached = 0usize;
                for (i, id) in ids.iter().filter_map(|s| s.parse::<i64>().ok()).enumerate() {
                    progress.report(i as u64, Some(total), &format!("steamgriddb: id {} of {total} ({cached} from cache)", i + 1)).await;
                    let hit = cache::load_cached(&self.app.pool, provider, &id.to_string(), ttl_days).await?;
                    let from_cache = hit.is_some();
                    let game = match hit {
                        Some(game) => Some(game),
                        None => client.get_game_by_id(id).await?,
                    };
                    if let Some(game) = game {
                        if from_cache {
                            cached += 1;
                        } else {
                            cache::upsert_game(&self.app.pool, &self.store, provider, &game.id.to_string(), Some(&game.name), &serde_json::to_value(&game)?).await?;
                        }
                        for title_id in metadata::titles_linked(&self.app.pool, provider, &game.id.to_string()).await? {
                            self.after_link(provider, &game.id.to_string(), title_id).await?;
                        }
                        done += 1;
                    }
                }
                log::info!("provider_query steamgriddb: {done} of {} ids fetched", ids.len());
            }
            Provider::MobyGames => {
                let clients = self.app.clients();
                let client = clients.mobygames.as_ref().ok_or_else(|| DatabaseError::Provider("mobygames client unavailable".to_string()))?;
                let mut done = 0usize;
                let mut cached = 0usize;
                for (i, id) in ids.iter().filter_map(|s| s.parse::<i64>().ok()).enumerate() {
                    progress.report(i as u64, Some(total), &format!("mobygames: id {} of {total} ({cached} from cache)", i + 1)).await;
                    let hit = cache::load_cached(&self.app.pool, provider, &id.to_string(), ttl_days).await?;
                    let from_cache = hit.is_some();
                    let game = match hit {
                        Some(game) => Some(game),
                        None => client.get_game_by_id(id).await?,
                    };
                    if let Some(game) = game {
                        if from_cache {
                            cached += 1;
                        } else {
                            cache::upsert_game(&self.app.pool, &self.store, provider, &game.game_id.to_string(), Some(&game.title), &serde_json::to_value(&game)?).await?;
                        }
                        for title_id in metadata::titles_linked(&self.app.pool, provider, &game.game_id.to_string()).await? {
                            metadata::apply_mobygames_game(&self.app.pool, title_id, &game).await?;
                            if let Some(url) = game.sample_cover.as_ref().map(|c| c.image.clone()) {
                                let targets = media::release_targets(&self.app.pool, title_id, provider, &game.game_id.to_string()).await?;
                                media::attach_media(&self.app.http, &self.app.pool, &self.app.paths.media_dir(), provider, Some(&game.game_id.to_string()), &url, "box", Some("front"), &targets).await?;
                            }
                        }
                        done += 1;
                    }
                }
                log::info!("provider_query mobygames: {done} of {} ids fetched", ids.len());
            }
            Provider::ScreenScraper => {
                let clients = self.app.clients();
                let client = clients.screenscraper.as_ref().ok_or_else(|| DatabaseError::Provider("screenscraper client unavailable".to_string()))?;
                let mut done = 0usize;
                let mut cached = 0usize;
                for (i, id) in ids.iter().filter_map(|s| s.parse::<i64>().ok()).enumerate() {
                    progress.report(i as u64, Some(total), &format!("screenscraper: id {} of {total} ({cached} from cache)", i + 1)).await;
                    let hit = cache::load_cached(&self.app.pool, provider, &id.to_string(), ttl_days).await?;
                    let from_cache = hit.is_some();
                    let game = match hit {
                        Some(game) => Some(game),
                        None => client.get_game_by_id(id).await?,
                    };
                    if let Some(game) = game {
                        if from_cache {
                            cached += 1;
                        } else {
                            let title = game.iter_candidate_names().next().map(str::to_string);
                            cache::upsert_game(&self.app.pool, &self.store, provider, &id.to_string(), title.as_deref(), &serde_json::to_value(&game)?).await?;
                        }
                        for title_id in metadata::titles_linked(&self.app.pool, provider, &id.to_string()).await? {
                            metadata::apply_screenscraper_game(&self.app.pool, title_id, &game).await?;
                        }
                        done += 1;
                    }
                }
                log::info!("provider_query screenscraper: {done} of {} ids fetched", ids.len());
            }
            other => {
                // No by-id call in the vendored client (TheGamesDB, EmuReady, Hasheous): the
                // cross-reference link itself was already written by the hash match, so
                // there's nothing to fail -- just nothing more to cache.
                log::info!("provider_query {}: no by-id fetch available, {} cross-ref id(s) left as links only", other.name(), ids.len());
            }
        }
        progress.force(total, Some(total), &format!("{}: done, {total} id(s)", provider.name())).await;
        Ok(())
    }

    /// payload: `ProviderBulkSyncRequest` -- `full: true` for the whole database where the
    /// provider allows it (`api::providers::capabilities`), else one platform or `"all"`
    /// (every platform the catalog has releases for: a platform the provider doesn't know
    /// is logged and skipped, an error on one doesn't stop the rest). Progress goes to the
    /// job row.
    async fn provider_bulk_sync(&self, job: &Job) -> Result<(), DatabaseError> {
        /// A diff run that found nothing new: the provider-side check said unchanged (RA's
        /// game list, EmuReady's count, TheGamesDB's dump / "covered by dump"), or Hasheous
        /// had no zip to process.
        fn summary_unchanged(provider: Provider, s: &bulk::BulkSummary) -> bool {
            s.remote_id.as_deref().is_some_and(|r| r.contains("unchanged") || r == "covered by dump") || (provider == Provider::Hasheous && s.pages == 0 && s.games == 0)
        }
        let provider = payload_str(job, "provider").and_then(Provider::from_name).ok_or_else(|| DatabaseError::Config("provider_bulk_sync: missing/unknown provider".to_string()))?;
        let caps = api::providers::capabilities(provider);
        let progress = Progress::new(&self.app.pool, job.id);
        // `full` absent from the payload is not the same as `full: false`: only then does the
        // `provider_sync_scope` setting get a say (see `wants_full_sync`).
        let explicit_full = job.payload.get("full").and_then(|v| v.as_bool());
        let full = wants_full_sync(explicit_full, settings::sync_scope(&self.app.pool, provider).await?, caps.full);
        let mode = bulk::SyncMode::from_payload(&job.payload);
        let parallel = settings::hasheous_parallel_downloads(&self.app.pool).await?;
        if full {
            if !caps.full {
                return Err(DatabaseError::Provider(format!("{} can't fetch its whole database: {}", provider.name(), caps.notes)));
            }
            progress.force(0, None, "starting full sync").await;
            let summary = match provider {
                Provider::TheGamesDb => dumps::sync_thegamesdb_full(&self.app.clients(), &self.app.http, &self.app.pool, &self.store, mode, &progress).await?,
                Provider::Hasheous => dumps::sync_hasheous_dump(&self.app.http, &self.app.pool, &self.store, None, mode, parallel, &progress).await?,
                Provider::RetroAchievements => {
                    let (summary, failed) = bulk::sync_retroachievements_all(&self.app.clients(), &self.app.pool, &self.store, mode, &progress).await?;
                    if !failed.is_empty() {
                        log::warn!("RA full sync: {} console(s) failed: {}", failed.len(), failed.join(", "));
                    }
                    summary
                }
                _ => return Err(DatabaseError::Provider(format!("{} full sync not implemented", provider.name()))),
            };
            let line = if summary_unchanged(provider, &summary) {
                "full sync done: provider data unchanged since the last sync, nothing downloaded or imported".to_string()
            } else {
                format!("full sync done: {} games, {} hashes, {} linked", summary.games, summary.hashes, summary.linked)
            };
            log::info!("{} {line}", provider.name());
            progress.force(summary.games, Some(summary.games), &line).await;
            return Ok(());
        }
        if !caps.platform {
            return Err(DatabaseError::Provider(format!("{} has no per-platform listing: {}", provider.name(), caps.notes)));
        }
        let platforms: Vec<Platform> = match payload_platform(job) {
            Some(p) => vec![p],
            None if job.payload.get("platform").and_then(|v| v.as_str()) == Some("all") || job.payload.get("platform").is_none() => {
                // The catalog's platforms *and* the collection's: with no DAT imported for a
                // platform, `release` has nothing on it, and a collection of scanned files
                // alone made this an empty list, finishing at once at "done: 0 games".
                let rows = sqlx::query("SELECT app_platform_id FROM `release` WHERE deleted = FALSE AND app_platform_id > 0 UNION SELECT app_platform_id FROM local_rom WHERE deleted = FALSE AND app_platform_id > 0 ORDER BY app_platform_id").fetch_all(&self.app.pool).await?;
                rows.iter().filter_map(|r| r.try_get::<u16, _>("app_platform_id").ok()).filter_map(Platform::from_igdb_id).collect()
            }
            None => return Err(DatabaseError::Config("provider_bulk_sync: unknown platform".to_string())),
        };
        let max_pages = payload_u64(job, "max_pages").map(|n| n as u32);
        let count = platforms.len() as u64;
        if platforms.is_empty() {
            progress.force(0, Some(0), "done: no platforms to sync -- neither the catalog nor the collection has any platform yet").await;
            return Ok(());
        }
        let (mut failures, mut games, mut linked, mut unchanged, mut skipped) = (0usize, 0u64, 0u64, 0u64, 0u64);
        // With several platforms each one gets its slice of the bar, and its own page
        // reports land in it: a quiet sink here left the row on "platform 1 of n", 0 done,
        // for the hours one big IGDB platform takes.
        const SLICE: u64 = 1000;
        for (i, platform) in platforms.iter().enumerate() {
            let platform = *platform;
            if count > 1 {
                progress.set_frame(Some(jobs::Frame { base: i as u64 * SLICE, span: SLICE, whole: count * SLICE }));
                progress.set_prefix(Some(format!("platform {} of {count}: ", i + 1)));
                progress.force(0, None, &format!("{platform:?}")).await;
            }
            let inner = &progress;
            let result: Result<Option<bulk::BulkSummary>, DatabaseError> = async {
                Ok(match provider {
                    Provider::RetroAchievements => {
                        let Some((console_id, _)) = bulk::ra_console_id_for(&self.app.clients(), platform).await? else {
                            log::info!("RA sync: no console for {platform:?}, skipped");
                            return Ok(None);
                        };
                        Some(bulk::sync_retroachievements_console(&self.app.clients(), &self.app.pool, &self.store, console_id, Some(platform), mode).await?)
                    }
                    Provider::Igdb => {
                        let id = cache::sibling_igdb_platforms(platform.igdb_id())[0];
                        if id == 0 || id >= 2048 {
                            log::info!("IGDB sync: {platform:?} has no IGDB platform id, skipped");
                            return Ok(None);
                        }
                        let media_dir = self.app.paths.media_dir();
                        Some(bulk::sync_igdb_platform(&self.app.clients(), &self.app.pool, &self.store, platform, max_pages, Some((&self.app.http, media_dir.as_path())), mode, inner).await?)
                    }
                    Provider::TheGamesDb => Some(bulk::sync_thegamesdb_platform(&self.app.clients(), &self.app.pool, &self.store, platform, max_pages, mode, inner).await?),
                    Provider::EmuReady => {
                        // It has 19 systems: a platform it does not have is passed over,
                        // not a failure (it was one, for 51 platforms of 70, every time).
                        if cache::emuready_system_id(&self.app.clients(), platform).await?.is_none() {
                            log::info!("EmuReady sync: no system for {platform:?}, skipped");
                            return Ok(None);
                        }
                        Some(bulk::sync_emuready_platform(&self.app.clients(), &self.app.pool, &self.store, platform, max_pages, mode, inner).await?)
                    }
                    Provider::Hasheous => Some(dumps::sync_hasheous_dump(&self.app.http, &self.app.pool, &self.store, Some(platform), mode, parallel, inner).await?),
                    _ => return Err(DatabaseError::Provider(format!("{} per-platform sync not implemented", provider.name()))),
                })
            }
            .await;
            match result {
                Ok(Some(s)) => {
                    log::info!("{} sync {platform:?}: {} games over {} page(s), {} hashes, {} linked, {} media{}", provider.name(), s.games, s.pages, s.hashes, s.linked, s.media, if summary_unchanged(provider, &s) { " (unchanged)" } else { "" });
                    if summary_unchanged(provider, &s) {
                        unchanged += 1;
                    } else {
                        games += s.games;
                        linked += s.linked;
                    }
                }
                Ok(None) => skipped += 1,
                Err(e) => {
                    failures += 1;
                    if count == 1 {
                        return Err(e);
                    }
                    log::warn!("{} sync {platform:?} failed: {e}", provider.name());
                }
            }
        }
        progress.set_frame(None);
        progress.set_prefix(None);
        let unchanged_note = match (unchanged, count) {
            (0, _) => String::new(),
            (1, 1) => " -- unchanged since the last sync, nothing fetched".to_string(),
            (n, _) => format!(", {n} platform(s) unchanged"),
        };
        let over = if count > 1 { format!(" over {count} platforms") } else { String::new() };
        let skipped_note = if skipped > 0 { format!(", {skipped} skipped ({} has no entry for them)", provider.name()) } else { String::new() };
        progress.force(count, Some(count), &format!("done: {games} games{over}, {linked} linked{unchanged_note}{skipped_note}{}", if failures > 0 { format!(", {failures} platform(s) failed") } else { String::new() })).await;
        if failures > 0 {
            return Err(DatabaseError::Provider(format!("{} sync: {failures} of {count} platform(s) failed (see worker log)", provider.name())));
        }
        Ok(())
    }

    /// payload: {"provider": name, "url": str, "media_type": str, "sub_category": str?, "title_id": n? | "release_id": n?}
    async fn media_download(&self, job: &Job) -> Result<(), DatabaseError> {
        let provider = payload_str(job, "provider").and_then(Provider::from_name).ok_or_else(|| DatabaseError::Config("media_download: missing/unknown provider".to_string()))?;
        let url = payload_str(job, "url").ok_or_else(|| DatabaseError::Config("media_download: missing url".to_string()))?;
        let media_type = payload_str(job, "media_type").unwrap_or("cover");
        if !crate::database::settings::MediaRules::load(&self.app.pool).await?.downloads(provider.name(), media_type) {
            return Err(DatabaseError::Config(format!("media_download: {media_type} from {} is turned off under Settings > Media", provider.name())));
        }
        let target = if let Some(id) = payload_u64(job, "title_id") {
            media::MediaTarget::Title(id)
        } else if let Some(id) = payload_u64(job, "release_id") {
            media::MediaTarget::Release(id)
        } else if let Some(id) = payload_u64(job, "franchise_id") {
            media::MediaTarget::Franchise(id)
        } else if let Some(id) = payload_u64(job, "keyword_id") {
            media::MediaTarget::Keyword(id)
        } else {
            return Err(DatabaseError::Config("media_download: needs title_id, release_id, franchise_id or keyword_id".to_string()));
        };
        // Through the main server where there is one: it fetches what it lacks and hands it
        // over, so the provider hears from the household once. The provider is called from
        // here only with no main server at all, or with the server offline and the fallback
        // allowed; a server that is up but cannot supply it fails the job rather than being
        // worked around.
        use api::media_sync::ServerCopy;
        Progress::new(&self.app.pool, job.id).force(0, Some(1), &format!("downloading the {media_type} from {}: {url}", provider.name())).await;
        let prefetched = match api::media_sync::via_main(&self.app, url, provider, payload_str(job, "provider_id"), media_type, payload_str(job, "sub_category"), target).await? {
            ServerCopy::Bytes(f) => Some((f.bytes, f.content_type)),
            ServerCopy::NoMainServer | ServerCopy::NotServed => None,
            ServerCopy::Offline(name) => {
                if !crate::database::settings::media_provider_fallback(&self.app.pool).await? {
                    return Err(DatabaseError::Config(format!("media_download: {name} cannot be reached, and media_provider_fallback is off")));
                }
                log::info!("media_download: {name} cannot be reached; fetching {url} from the provider");
                None
            }
        };
        media::store_url_with(&self.app.http, &self.app.pool, &self.app.paths.media_dir(), provider.name(), payload_str(job, "provider_id"), url, media_type, payload_str(job, "sub_category"), &[target], prefetched).await?;
        Ok(())
    }

    /// The deletion-policy hard-delete pass, bottom-up: only rows flagged `deleted` with
    /// nothing live still pointing at them.
    async fn db_optimize(&self, progress: &Progress) -> Result<(), DatabaseError> {
        const STATEMENTS: [(&str, &str); 9] = [
            ("finished jobs older than 30 days", "DELETE FROM jobs WHERE status IN ('done','failed') AND updated_at < NOW() - INTERVAL 30 DAY"),
            ("links to deleted files", "DELETE rf FROM local_rom_files rf JOIN local_files f ON f.id = rf.file_id WHERE f.deleted = TRUE"),
            ("deleted files", "DELETE FROM local_files WHERE deleted = TRUE"),
            ("game links of deleted ROMs", "DELETE gr FROM local_game_roms gr JOIN local_rom r ON r.id = gr.rom_id WHERE r.deleted = TRUE AND NOT EXISTS (SELECT 1 FROM local_rom_files rf WHERE rf.rom_id = r.id)"),
            ("deleted ROMs", "DELETE FROM local_rom WHERE deleted = TRUE AND NOT EXISTS (SELECT 1 FROM local_rom_files rf WHERE rf.rom_id = local_rom.id)"),
            ("release links of deleted games", "DELETE rg FROM local_release_games rg JOIN local_game g ON g.id = rg.game_id WHERE g.deleted = TRUE AND NOT EXISTS (SELECT 1 FROM local_game_roms gr WHERE gr.game_id = g.id)"),
            ("deleted games", "DELETE FROM local_game WHERE deleted = TRUE AND NOT EXISTS (SELECT 1 FROM local_game_roms gr WHERE gr.game_id = local_game.id)"),
            ("hashes of deleted content", "DELETE h FROM hashes h JOIN content c ON c.id = h.content_id WHERE c.deleted = TRUE AND NOT EXISTS (SELECT 1 FROM local_files f WHERE f.content_id = c.id) AND NOT EXISTS (SELECT 1 FROM release_contents rc WHERE rc.content_id = c.id)"),
            ("deleted content", "DELETE FROM content WHERE deleted = TRUE AND NOT EXISTS (SELECT 1 FROM local_files f WHERE f.content_id = content.id) AND NOT EXISTS (SELECT 1 FROM release_contents rc WHERE rc.content_id = content.id) AND NOT EXISTS (SELECT 1 FROM hashes h WHERE h.content_id = content.id)"),
        ];
        let n = STATEMENTS.len() as u64;
        let mut tx = self.app.pool.begin().await?;
        for (i, (what, sql)) in STATEMENTS.iter().enumerate() {
            progress.force(i as u64, Some(n), &format!("removing {what} ({} of {n})", i + 1)).await;
            sqlx::query(*sql).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::settings::SyncScope;

    /// The setting was validated, stored and editable for a long time while nothing read it;
    /// this pins down exactly when it now matters.
    #[test]
    fn sync_scope_setting_applies_only_when_the_request_is_silent() {
        // The request wins, whatever the setting says.
        assert!(wants_full_sync(Some(true), SyncScope::Single, true));
        assert!(wants_full_sync(Some(true), SyncScope::Single, false), "an explicit ask on a provider that can't: still asked, still an error downstream");
        assert!(!wants_full_sync(Some(false), SyncScope::Full, true));

        // Silent request: only `full` on a capable provider changes the outcome.
        assert!(wants_full_sync(None, SyncScope::Full, true));
        assert!(!wants_full_sync(None, SyncScope::Full, false), "a setting never fails a scheduled job");
        for not_bulk in [SyncScope::Single, SyncScope::List, SyncScope::ByKeyword, SyncScope::ByPlatform] {
            assert!(!wants_full_sync(None, not_bulk, true), "{not_bulk:?} falls through to the platform walk");
        }
    }

    #[test]
    fn scan_hash_match_follows_the_scan_payload() {
        let job = |payload| Job { id: 1, job_type: JobType::FileScan, payload, attempts: 0 };
        // Silent (a schedule, an upload): DAT only.
        let silent = scan_hash_match(&job(serde_json::json!({"directory_id": 1, "mode": "full"})));
        assert_eq!((silent.to_dat, silent.to_hasheous, silent.to_playmatch), (true, false, false));
        let asked = scan_hash_match(&job(serde_json::json!({"directory_id": 1, "mode": "rehash", "hash_match": {"to_dat": false, "to_hasheous": true, "to_playmatch": true}})));
        assert_eq!((asked.to_dat, asked.to_hasheous, asked.to_playmatch), (false, true, true));
    }
}
