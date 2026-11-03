//! The `jobs` table: the only channel between the app and the background worker
//! (`romexplorer-worker`). Claiming uses `SELECT ... FOR UPDATE SKIP LOCKED`, so several
//! workers (or worker threads) never hand out the same row twice.

use sqlx::mysql::MySqlPool;
use sqlx::Row;

use super::error::DatabaseError;

// The row types are shared with the GUI through `romexplorer_types` (see that crate's docs);
// everything that touches the database stays here.
pub use romexplorer_types::jobs::{CancelOutcome, FileScanMode, JobRow, JobType};

#[derive(Debug, Clone)]
pub struct Job {
    pub id: u64,
    pub job_type: JobType,
    pub payload: serde_json::Value,
    pub attempts: u16,
}

/// Max attempts before a job is marked `failed` rather than returned to `pending`.
pub const MAX_ATTEMPTS: u16 = 3;

pub async fn enqueue(pool: &MySqlPool, job_type: JobType, payload: serde_json::Value) -> Result<u64, DatabaseError> {
    enqueue_with_priority(pool, job_type, payload, 0).await
}

/// `priority` 0 = background; higher runs before lower and in the lane's fast slot (see
/// [`lane_of`] / the worker), so a page the user just opened isn't queued behind a bulk sync.
pub async fn enqueue_with_priority(pool: &MySqlPool, job_type: JobType, payload: serde_json::Value, priority: u16) -> Result<u64, DatabaseError> {
    let result = sqlx::query("INSERT INTO jobs (job_type, payload, priority) VALUES (?, ?, ?)").bind(job_type.name()).bind(payload).bind(priority).execute(pool).await?;
    let id = result.last_insert_id();
    set_queue_order(pool, id).await?;
    Ok(id)
}

/// Queues a job that waits its turn (migration 0051): it starts only when nothing is running
/// and no ordinary job is waiting, oldest first among such jobs. A list of them runs one
/// after another instead of side by side in their lanes.
pub async fn enqueue_in_turn(pool: &MySqlPool, job_type: JobType, payload: serde_json::Value) -> Result<u64, DatabaseError> {
    let result = sqlx::query("INSERT INTO jobs (job_type, payload, priority, in_turn) VALUES (?, ?, 0, TRUE)").bind(job_type.name()).bind(payload).execute(pool).await?;
    let id = result.last_insert_id();
    set_queue_order(pool, id).await?;
    Ok(id)
}

/// A new job's place in its lane: its id (migration 0077). "Run next" ([`run_next`]) is
/// what moves it.
async fn set_queue_order(pool: &MySqlPool, id: u64) -> Result<(), DatabaseError> {
    sqlx::query("UPDATE jobs SET queue_order = id WHERE id = ? AND queue_order IS NULL").bind(id).execute(pool).await?;
    Ok(())
}

/// Claims up to `limit` pending jobs of `job_type` (oldest first), marking them
/// `in_progress` and bumping `attempts`.
pub async fn claim(pool: &MySqlPool, job_type: JobType, limit: u32) -> Result<Vec<Job>, DatabaseError> {
    let mut tx = pool.begin().await?;
    let rows = sqlx::query("SELECT id, payload, attempts FROM jobs WHERE job_type = ? AND status = 'pending' ORDER BY COALESCE(queue_order, id), id LIMIT ? FOR UPDATE SKIP LOCKED")
        .bind(job_type.name())
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
    let mut jobs = Vec::with_capacity(rows.len());
    for row in rows {
        let id: u64 = row.try_get("id")?;
        let payload: serde_json::Value = row.try_get("payload")?;
        let attempts: u16 = row.try_get("attempts")?;
        sqlx::query("UPDATE jobs SET status = 'in_progress', claimed_at = NOW(), worker_id = ?, attempts = attempts + 1, progress_done = NULL, progress_total = NULL, progress_message = NULL, progress_detail = NULL WHERE id = ?").bind(worker_id()).bind(id).execute(&mut *tx).await?;
        jobs.push(Job { id, job_type, payload, attempts: attempts + 1 });
    }
    tx.commit().await?;
    Ok(jobs)
}

/// `1.4 KiB`, `12.0 MiB`, `2.6 GiB` -- for progress messages.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let (mut value, mut unit) = (bytes as f64, 0);
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}

/// Writes a running job's progress, throttled: long jobs (provider bulk syncs, DAT
/// downloads) call [`Progress::report`] as often as they like, and at most about one
/// `UPDATE` a second reaches the database. [`Progress::none`] writes nothing, for callers
/// outside a job. A failed progress write is logged, never an error for the job itself.
pub struct Progress {
    target: Option<(MySqlPool, u64)>,
    last: std::sync::Mutex<Option<std::time::Instant>>,
    /// See [`Progress::set_frame`]. `None` for every job that has one scale of its own.
    frame: std::sync::Mutex<Option<Frame>>,
    /// See [`Progress::set_inner_frame`]: a step's own slices, inside `frame`.
    inner: std::sync::Mutex<Option<Frame>>,
    /// See [`Progress::set_prefix`].
    prefix: std::sync::Mutex<Option<String>>,
    /// When [`Progress::cancelled`] last asked the database, and what it heard.
    cancel_seen: std::sync::Mutex<(Option<std::time::Instant>, bool)>,
}

/// A slice of a larger whole: reports made while it is set are fitted into
/// `base..base + span` out of `whole`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    pub base: u64,
    pub span: u64,
    pub whole: u64,
}

impl Frame {
    /// Where `done` out of `total` lands in the whole. A step with no known total sits at
    /// the start of its slice; a `done` past its `total` is held to the end of it, so one
    /// step can never push the bar past the next step's start.
    fn place(self, done: u64, total: Option<u64>) -> (u64, Option<u64>) {
        let within = match total {
            Some(total) if total > 0 => ((done.min(total) as u128 * self.span as u128) / total as u128) as u64,
            _ => 0,
        };
        ((self.base + within).min(self.whole), Some(self.whole))
    }
}

impl std::fmt::Debug for Progress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Progress").field("job_id", &self.target.as_ref().map(|(_, id)| *id)).finish()
    }
}

impl Progress {
    const INTERVAL: std::time::Duration = std::time::Duration::from_millis(1000);

    pub fn new(pool: &MySqlPool, job_id: u64) -> Self {
        Self { target: Some((pool.clone(), job_id)), last: std::sync::Mutex::new(None), frame: std::sync::Mutex::new(None), inner: std::sync::Mutex::new(None), prefix: std::sync::Mutex::new(None), cancel_seen: std::sync::Mutex::new((None, false)) }
    }

    pub fn none() -> Self {
        Self { target: None, last: std::sync::Mutex::new(None), frame: std::sync::Mutex::new(None), inner: std::sync::Mutex::new(None), prefix: std::sync::Mutex::new(None), cancel_seen: std::sync::Mutex::new((None, false)) }
    }

    /// Whether a cancel was asked for this job (or it was marked cancelled while running).
    /// Asks the database at most every two seconds; once true, stays true.
    pub async fn cancelled(&self) -> bool {
        let Some((pool, id)) = self.target.as_ref() else { return false };
        {
            let seen = self.cancel_seen.lock().unwrap_or_else(|p| p.into_inner());
            if seen.1 || seen.0.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(2)) {
                return seen.1;
            }
        }
        let now = cancel_requested(pool, &[*id]).await.map(|ids| !ids.is_empty()).unwrap_or(false);
        *self.cancel_seen.lock().unwrap_or_else(|p| p.into_inner()) = (Some(std::time::Instant::now()), now);
        now
    }

    /// Where an earlier run of this job got to (`save_checkpoint`), if it saved anything: a
    /// job interrupted by a restart, or one that failed and was retried.
    pub async fn checkpoint(&self) -> Option<String> {
        let (pool, id) = self.target.as_ref()?;
        sqlx::query_scalar::<_, Option<String>>("SELECT checkpoint FROM jobs WHERE id = ?").bind(*id).fetch_optional(pool).await.ok().flatten().flatten()
    }

    /// Records how far the job got, for a later run of it to go on from. A failed write is
    /// logged, never fatal: the cost is only redoing some work.
    pub async fn save_checkpoint(&self, checkpoint: &str) {
        let Some((pool, id)) = self.target.as_ref() else { return };
        if let Err(e) = sqlx::query("UPDATE jobs SET checkpoint = ? WHERE id = ?").bind(checkpoint.chars().take(512).collect::<String>()).bind(*id).execute(pool).await {
            log::warn!("job {id}: could not save its checkpoint: {e}");
        }
    }

    /// Makes later reports parts of a whole, or (`None`) goes back to taking them as given.
    ///
    /// For a job made of steps that each count in their own unit. `dat_sync` over every
    /// source downloads (bytes) and then imports (files) per source, and reporting each as
    /// it came made one bar read "2 of 6", then "13 MiB of 40", then "3 of 51", then "3 of
    /// 6". With a frame per step the steps keep reporting in their own units, unchanged,
    /// and the row shows one number that only goes up.
    /// Setting it also clears the inner frame (a new step starts with none).
    pub fn set_frame(&self, frame: Option<Frame>) {
        *self.frame.lock().unwrap_or_else(|p| p.into_inner()) = frame;
        *self.inner.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    /// Slices of whatever the frame set by the caller is, or (`None`) back to that frame
    /// alone. For a function made of steps of its own that may run as one step of a bigger
    /// job: the title merge pass splits its run into sub-steps, and as step 4 of 7 of the
    /// title dedupe job those sub-steps land inside step 4's seventh instead of taking over
    /// the whole bar.
    pub fn set_inner_frame(&self, frame: Option<Frame>) {
        *self.inner.lock().unwrap_or_else(|p| p.into_inner()) = frame;
    }

    /// Replaces the live detail beside the bar (`JobRow::progress_detail`) at once, leaving
    /// the bar and message as they are: a job made of steps says which are done, and how
    /// each came out, as each one ends.
    pub async fn set_detail(&self, detail: &serde_json::Value) {
        let Some((pool, id)) = &self.target else { return };
        if let Err(e) = sqlx::query("UPDATE jobs SET progress_detail = ? WHERE id = ?").bind(detail).bind(id).execute(pool).await {
            log::warn!("job {id}: progress detail write failed: {e}");
        }
    }

    /// Puts `prefix` in front of every later message, or (`None`) stops. For a job that runs
    /// the same step over several items, so a step's own reports still say which item.
    pub fn set_prefix(&self, prefix: Option<String>) {
        *self.prefix.lock().unwrap_or_else(|p| p.into_inner()) = prefix;
    }

    fn prefixed(&self, message: &str) -> String {
        let message = match self.prefix.lock().unwrap_or_else(|p| p.into_inner()).as_deref() {
            Some(prefix) => format!("{prefix}{message}"),
            None => message.to_string(),
        };
        message.chars().take(255).collect()
    }

    fn framed(&self, done: u64, total: Option<u64>) -> (u64, Option<u64>) {
        let (done, total) = match *self.inner.lock().unwrap_or_else(|p| p.into_inner()) {
            Some(inner) => inner.place(done, total),
            None => (done, total),
        };
        match *self.frame.lock().unwrap_or_else(|p| p.into_inner()) {
            Some(frame) => frame.place(done, total),
            None => (done, total),
        }
    }

    /// A second writer for the same job, with its own throttle clock.
    ///
    /// For a job whose work happens on a blocking thread that cannot await: the blocking
    /// side sends byte counts to an async relay, and the relay needs a `Progress` of its own
    /// because this one is borrowed by the outer loop. Two writers on one row race only over
    /// which message lands, which is exactly as interesting as it sounds.
    /// It keeps the frame and prefix set so far, so its messages land where this one's would.
    pub fn duplicate(&self) -> Self {
        let frame = *self.frame.lock().unwrap_or_else(|p| p.into_inner());
        let inner = *self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let prefix = self.prefix.lock().unwrap_or_else(|p| p.into_inner()).clone();
        Self { target: self.target.clone(), last: std::sync::Mutex::new(None), frame: std::sync::Mutex::new(frame), inner: std::sync::Mutex::new(inner), prefix: std::sync::Mutex::new(prefix), cancel_seen: std::sync::Mutex::new((None, false)) }
    }

    /// `done` out of `total` (when known), and a short note on the current step.
    pub async fn report(&self, done: u64, total: Option<u64>, message: &str) {
        let due = {
            let mut last = self.last.lock().unwrap_or_else(|p| p.into_inner());
            let due = last.map_or(true, |t| t.elapsed() >= Self::INTERVAL);
            if due {
                *last = Some(std::time::Instant::now());
            }
            due
        };
        if due {
            let (done, total) = self.framed(done, total);
            self.write(done, total, message).await;
        }
    }

    /// Unthrottled: a phase change or the final numbers, which shouldn't be skipped.
    pub async fn force(&self, done: u64, total: Option<u64>, message: &str) {
        *self.last.lock().unwrap_or_else(|p| p.into_inner()) = Some(std::time::Instant::now());
        let (done, total) = self.framed(done, total);
        self.write(done, total, message).await;
    }

    /// The final numbers *and* the structured outcome (`JobRow::result`), in one write. The
    /// message stays the one-liner the jobs list shows; `result` is what a screen renders in
    /// full -- rows per table, what was skipped, art left unresolved.
    pub async fn finish<T: serde::Serialize>(&self, done: u64, total: Option<u64>, message: &str, result: &T) {
        let Some((pool, id)) = &self.target else { return };
        let message: String = message.chars().take(255).collect();
        let result = serde_json::to_value(result).unwrap_or(serde_json::Value::Null);
        if let Err(e) = sqlx::query("UPDATE jobs SET progress_done = ?, progress_total = ?, progress_message = ?, progress_detail = NULL, result = ? WHERE id = ?")
            .bind(done)
            .bind(total)
            .bind(message)
            .bind(result)
            .bind(id)
            .execute(pool)
            .await
        {
            log::warn!("job {id}: result write failed: {e}");
        }
    }

    /// [`Self::report`] with live detail beside the bar (`JobRow::progress_detail`): a scan's
    /// files being read right now. Same throttle.
    pub async fn report_detail(&self, done: u64, total: Option<u64>, message: &str, detail: &serde_json::Value) {
        let due = {
            let mut last = self.last.lock().unwrap_or_else(|p| p.into_inner());
            let due = last.map_or(true, |t| t.elapsed() >= Self::INTERVAL);
            if due {
                *last = Some(std::time::Instant::now());
            }
            due
        };
        if !due {
            return;
        }
        let Some((pool, id)) = &self.target else { return };
        let (done, total) = self.framed(done, total);
        let message = self.prefixed(message);
        if let Err(e) = sqlx::query("UPDATE jobs SET progress_done = ?, progress_total = ?, progress_message = ?, progress_detail = ? WHERE id = ?")
            .bind(done)
            .bind(total)
            .bind(message)
            .bind(detail)
            .bind(id)
            .execute(pool)
            .await
        {
            log::warn!("job {id}: progress write failed: {e}");
        }
    }

    async fn write(&self, done: u64, total: Option<u64>, message: &str) {
        let Some((pool, id)) = &self.target else { return };
        let message = self.prefixed(message);
        if let Err(e) = sqlx::query("UPDATE jobs SET progress_done = ?, progress_total = ?, progress_message = ? WHERE id = ?").bind(done).bind(total).bind(message).bind(id).execute(pool).await {
            log::warn!("job {id}: progress write failed: {e}");
        }
    }
}

/// A job made of named steps that each count in their own unit (the title dedupe pass):
/// gives each step an equal slice of the bar and says "step n of N: " in front of its
/// messages, and keeps a list of every step in the live detail (`progress_detail.steps`) --
/// which are done, how long each took and how it came out, which one is running and which
/// are still to come -- so a screen can show all of it, not just the current line.
pub struct Steps<'a> {
    progress: &'a Progress,
    rows: Vec<StepRow>,
    scope: String,
    /// 1-based; 0 before the first step starts.
    current: usize,
    started: Option<std::time::Instant>,
}

struct StepRow {
    key: &'static str,
    label: &'static str,
    state: &'static str,
    secs: Option<u64>,
    note: Option<String>,
}

impl<'a> Steps<'a> {
    /// Each step's slice of the bar, in the units `progress_done` counts.
    pub const SLICE: u64 = 1000;

    /// `steps` are (key, English label): the key is what a screen translates by, the label
    /// what it shows when it does not know the key. `scope` says what the run covers.
    pub fn new(progress: &'a Progress, steps: &[(&'static str, &'static str)], scope: &str) -> Self {
        let rows = steps.iter().map(|&(key, label)| StepRow { key, label, state: "pending", secs: None, note: None }).collect();
        Self { progress, rows, scope: scope.to_string(), current: 0, started: None }
    }

    /// Starts step `n` (1-based), ending the one before it if it was not ended.
    pub async fn start(&mut self, n: usize) {
        if self.current > 0 && self.rows[self.current - 1].state == "running" {
            self.close(None);
        }
        let whole = self.rows.len() as u64;
        self.current = n;
        self.started = Some(std::time::Instant::now());
        self.rows[n - 1].state = "running";
        self.progress.set_frame(Some(Frame { base: (n as u64 - 1) * Self::SLICE, span: Self::SLICE, whole: whole * Self::SLICE }));
        self.progress.set_prefix(Some(format!("step {n} of {whole}: ")));
        self.progress.set_detail(&self.detail()).await;
    }

    /// Ends the running step with a one-line note on how it came out.
    pub async fn done(&mut self, note: String) {
        self.close(Some(note));
        self.progress.set_detail(&self.detail()).await;
    }

    /// After the last step: messages and the bar are the job's own again. The step list
    /// stays until the job ends.
    pub fn finish(&self) {
        self.progress.set_frame(None);
        self.progress.set_prefix(None);
    }

    fn close(&mut self, note: Option<String>) {
        let Some(row) = self.current.checked_sub(1).and_then(|i| self.rows.get_mut(i)) else { return };
        row.state = "done";
        row.secs = self.started.map(|t| t.elapsed().as_secs());
        row.note = note;
    }

    fn detail(&self) -> serde_json::Value {
        let steps: Vec<serde_json::Value> = self
            .rows
            .iter()
            .map(|r| serde_json::json!({ "key": r.key, "label": r.label, "state": r.state, "secs": r.secs, "note": r.note }))
            .collect();
        let base = (self.current.max(1) as u64 - 1) * Self::SLICE;
        serde_json::json!({ "scope": self.scope, "step": self.current, "base": base, "span": Self::SLICE, "steps": steps })
    }
}

/// `'copy_files', ...`: the job types a restart does not start again by itself
/// ([`JobType::resumes_after_restart`]). Static names, so safe to put in SQL as they are.
fn not_resuming_sql() -> String {
    JobType::ALL.iter().filter(|t| !t.resumes_after_restart()).map(|t| format!("'{}'", t.name())).collect::<Vec<_>>().join(",")
}

/// Jobs a stopping worker aborted mid-run: straight back to `pending` (the interrupted try
/// doesn't count against `MAX_ATTEMPTS`), instead of waiting for [`release_orphans`] -- except
/// one a cancel was asked for and the worker stopped before acting on, and one of a type that
/// does not resume by itself (a copy): those are cancelled, not started again after the
/// restart. A cancelled one can still be retried.
pub async fn release_interrupted(pool: &MySqlPool, ids: &[u64]) -> Result<(), DatabaseError> {
    let not_resuming = not_resuming_sql();
    for id in ids {
        sqlx::query("UPDATE jobs SET status = 'cancelled', cancel_requested = FALSE, progress_detail = NULL, progress_message = LEFT(CONCAT('cancelled at: ', COALESCE(progress_message, 'start')), 255) WHERE id = ? AND status = 'in_progress' AND cancel_requested = TRUE")
            .bind(id)
            .execute(pool)
            .await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE jobs SET status = 'cancelled', progress_detail = NULL, progress_message = LEFT(CONCAT('interrupted by shutdown; retry to start it again (was at: ', COALESCE(progress_message, 'start'), ')'), 255) WHERE id = ? AND status = 'in_progress' AND job_type IN ({not_resuming})")))
            .bind(id)
            .execute(pool)
            .await?;
        sqlx::query("UPDATE jobs SET status = 'pending', attempts = GREATEST(attempts, 1) - 1, progress_message = 'interrupted by shutdown; will resume from the start' WHERE id = ? AND status = 'in_progress'")
            .bind(id)
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// This process's worker: written on the jobs it claims (migration 0054).
pub fn worker_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        format!("{}-{:x}", std::process::id(), nanos as u64)
    })
}

/// The named lock a worker holds while it lives; MariaDB frees it when the process (or
/// the database) goes away. `IS_USED_LOCK(...) IS NULL` on a job's worker means nothing
/// runs that job any more.
const WORKER_ALIVE_LOCK: &str = "romexplorer_job_worker:";

/// A running job with no task behind it: claimed before workers wrote their id, or by a
/// worker that no longer holds its lock.
const NO_WORKER_SQL: &str = "(worker_id IS NULL OR IS_USED_LOCK(CONCAT('romexplorer_job_worker:', worker_id)) IS NULL)";

/// Takes this worker's lock, held by the returned connection (dropping it frees the lock).
pub async fn hold_worker_lock(pool: &MySqlPool) -> Option<sqlx::pool::PoolConnection<sqlx::MySql>> {
    let mut conn = pool.acquire().await.ok()?;
    let got: Option<i64> = sqlx::query_scalar("SELECT GET_LOCK(?, 0)").bind(format!("{WORKER_ALIVE_LOCK}{}", worker_id())).fetch_one(&mut *conn).await.ok()?;
    (got == Some(1)).then_some(conn)
}

/// Whether this worker's lock is still held -- it is not after the database restarted.
pub async fn worker_lock_held(pool: &MySqlPool) -> Result<bool, DatabaseError> {
    let holder: Option<u64> = sqlx::query_scalar("SELECT IS_USED_LOCK(?)").bind(format!("{WORKER_ALIVE_LOCK}{}", worker_id())).fetch_one(pool).await?;
    Ok(holder.is_some())
}

/// Cancels a job: a pending one at once, a running one by setting `cancel_requested` (the
/// worker that owns the task aborts it) -- or at once too when no worker is behind it any
/// more. `Ok(None)` when the job is finished or missing.
pub async fn cancel(pool: &MySqlPool, id: u64) -> Result<Option<CancelOutcome>, DatabaseError> {
    let pending = sqlx::query("UPDATE jobs SET status = 'cancelled', progress_message = 'cancelled before it started' WHERE id = ? AND status = 'pending'").bind(id).execute(pool).await?;
    if pending.rows_affected() > 0 {
        return Ok(Some(CancelOutcome::Cancelled));
    }
    let orphan = sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE jobs SET status = 'cancelled', cancel_requested = FALSE, progress_detail = NULL, progress_message = LEFT(CONCAT('cancelled at: ', COALESCE(progress_message, 'start')), 255) WHERE id = ? AND status = 'in_progress' AND {NO_WORKER_SQL}"))).bind(id).execute(pool).await?;
    if orphan.rows_affected() > 0 {
        return Ok(Some(CancelOutcome::Cancelled));
    }
    let running = sqlx::query("UPDATE jobs SET cancel_requested = TRUE WHERE id = ? AND status = 'in_progress'").bind(id).execute(pool).await?;
    Ok((running.rows_affected() > 0).then_some(CancelOutcome::Requested))
}

/// What [`release_orphans`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Orphans {
    pub cancelled: Vec<u64>,
    pub requeued: Vec<u64>,
    pub failed: Vec<u64>,
}

/// Running jobs nothing runs any more (`running`: this worker's own tasks): a cancel asked
/// for is done, and a job of a type that does not resume by itself (a copy) is cancelled too;
/// otherwise the job goes back to `pending`, or to `failed` once it has used up
/// [`MAX_ATTEMPTS`] -- the interrupted try counts, or a job that takes the database down with
/// it would do so forever.
pub async fn release_orphans(pool: &MySqlPool, running: &[u64]) -> Result<Orphans, DatabaseError> {
    // A cancel asked for while the job was running, put back in the queue by a shutdown
    // from before `release_interrupted` knew better: cancelled, never started again.
    let stale = sqlx::query("UPDATE jobs SET status = 'cancelled', cancel_requested = FALSE, progress_message = 'cancelled before it started' WHERE status = 'pending' AND cancel_requested = TRUE").execute(pool).await?.rows_affected();
    if stale > 0 {
        log::info!("{stale} queued job(s) a cancel was asked for are cancelled");
    }
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT id, job_type, cancel_requested, attempts FROM jobs WHERE status = 'in_progress' AND (worker_id = ? OR {NO_WORKER_SQL})"))).bind(worker_id()).fetch_all(pool).await?;
    let mut out = Orphans::default();
    for row in rows {
        let id: u64 = row.try_get("id")?;
        if running.contains(&id) {
            continue;
        }
        let cancel: bool = row.try_get("cancel_requested")?;
        let attempts: u16 = row.try_get("attempts")?;
        let job_type: String = row.try_get("job_type")?;
        let resumes = JobType::from_name(&job_type).map_or(true, JobType::resumes_after_restart);
        if cancel {
            mark_cancelled(pool, id).await?;
            out.cancelled.push(id);
        } else if !resumes {
            sqlx::query("UPDATE jobs SET status = 'cancelled', progress_detail = NULL, progress_message = LEFT(CONCAT('interrupted: the worker running it stopped; retry to start it again (was at: ', COALESCE(progress_message, 'start'), ')'), 255) WHERE id = ? AND status = 'in_progress'")
                .bind(id)
                .execute(pool)
                .await?;
            out.cancelled.push(id);
        } else if attempts >= MAX_ATTEMPTS {
            sqlx::query("UPDATE jobs SET status = 'failed', last_error = 'interrupted: the worker running it stopped', progress_detail = NULL WHERE id = ? AND status = 'in_progress'").bind(id).execute(pool).await?;
            out.failed.push(id);
        } else {
            sqlx::query("UPDATE jobs SET status = 'pending', progress_detail = NULL, progress_message = 'interrupted: the worker running it stopped; will run again' WHERE id = ? AND status = 'in_progress'").bind(id).execute(pool).await?;
            out.requeued.push(id);
        }
    }
    Ok(out)
}

/// Of `ids` (running jobs), those a cancel was requested for.
pub async fn cancel_requested(pool: &MySqlPool, ids: &[u64]) -> Result<Vec<u64>, DatabaseError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    // Also one no longer running at all: cancelled while this worker had lost its lock
    // (the database restarted), so its task is still going.
    let sql = format!("SELECT id FROM jobs WHERE ((cancel_requested = TRUE AND status = 'in_progress') OR status = 'cancelled') AND id IN ({})", vec!["?"; ids.len()].join(","));
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    for id in ids {
        query = query.bind(id);
    }
    query.fetch_all(pool).await?.iter().map(|r| Ok(r.try_get("id")?)).collect()
}

/// A running job the worker aborted on request: `cancelled`, keeping where it got to.
pub async fn mark_cancelled(pool: &MySqlPool, id: u64) -> Result<(), DatabaseError> {
    sqlx::query("UPDATE jobs SET status = 'cancelled', cancel_requested = FALSE, progress_detail = NULL, progress_message = LEFT(CONCAT('cancelled at: ', COALESCE(progress_message, 'start')), 255) WHERE id = ? AND status = 'in_progress'")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn complete(pool: &MySqlPool, id: u64) -> Result<(), DatabaseError> {
    // Only a running job: one cancelled meanwhile stays cancelled.
    sqlx::query("UPDATE jobs SET status = 'done', last_error = NULL, progress_detail = NULL WHERE id = ? AND status = 'in_progress'").bind(id).execute(pool).await?;
    Ok(())
}

/// Records the error; returns the job to `pending` for another try unless it has used up
/// [`MAX_ATTEMPTS`].
pub async fn fail(pool: &MySqlPool, job: &Job, error: &str) -> Result<(), DatabaseError> {
    let status = if job.attempts >= MAX_ATTEMPTS { "failed" } else { "pending" };
    let error: String = error.chars().take(4000).collect();
    sqlx::query("UPDATE jobs SET status = ?, last_error = ?, progress_detail = NULL WHERE id = ? AND status = 'in_progress'").bind(status).bind(error).bind(job.id).execute(pool).await?;
    Ok(())
}

/// Records an error another try would only repeat (`DatabaseError::Partial`): straight to
/// `failed`, whatever attempts are left. The retry in [`fail`] is for a job that broke off
/// -- a dropped connection, a provider that timed out -- not for one that finished.
pub async fn fail_final(pool: &MySqlPool, id: u64, error: &str) -> Result<(), DatabaseError> {
    let error: String = error.chars().take(4000).collect();
    sqlx::query("UPDATE jobs SET status = 'failed', last_error = ?, progress_detail = NULL WHERE id = ? AND status = 'in_progress'").bind(error).bind(id).execute(pool).await?;
    Ok(())
}

/// The statuses the job log can be cleared of. A pending or running job is never removed.
pub const FINISHED: [&str; 3] = ["done", "failed", "cancelled"];

/// Removes finished jobs from the log: those with one of `statuses` (finished ones only;
/// anything else is ignored), and with `ids` only those of them. Returns how many went.
/// Nothing points at a job row by foreign key: a schedule's `last_job_id` that names a
/// removed job simply finds nothing.
pub async fn clear(pool: &MySqlPool, statuses: &[&str], ids: Option<&[u64]>) -> Result<u64, DatabaseError> {
    let wanted: Vec<&str> = FINISHED.iter().copied().filter(|s| statuses.contains(s)).collect();
    if wanted.is_empty() {
        return Ok(0);
    }
    let status_list = wanted.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(",");
    let Some(ids) = ids else {
        return Ok(sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM jobs WHERE status IN ({status_list})"))).execute(pool).await?.rows_affected());
    };
    let mut removed = 0;
    for part in ids.chunks(1000) {
        let list = part.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        removed += sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM jobs WHERE status IN ({status_list}) AND id IN ({list})"))).execute(pool).await?.rows_affected();
    }
    Ok(removed)
}

/// How many jobs the log holds by state, and the highest id: logged when the worker starts.
#[derive(Debug, Clone, Copy, Default)]
pub struct Census {
    pub finished: u64,
    pub pending: u64,
    pub running: u64,
    pub max_id: Option<u64>,
}

pub async fn census(pool: &MySqlPool) -> Result<Census, DatabaseError> {
    let row = sqlx::query(
        "SELECT CAST(COALESCE(SUM(status IN ('done', 'failed', 'cancelled')), 0) AS UNSIGNED) AS finished, CAST(COALESCE(SUM(status = 'pending'), 0) AS UNSIGNED) AS pending, CAST(COALESCE(SUM(status = 'in_progress'), 0) AS UNSIGNED) AS running, MAX(id) AS max_id FROM jobs",
    )
    .fetch_one(pool)
    .await?;
    Ok(Census { finished: row.try_get("finished")?, pending: row.try_get("pending")?, running: row.try_get("running")?, max_id: row.try_get("max_id")? })
}

/// Whether any job is waiting or running: what a "run only when idle" schedule waits out.
pub async fn queue_busy(pool: &MySqlPool) -> Result<bool, DatabaseError> {
    Ok(sqlx::query("SELECT 1 FROM jobs WHERE status IN ('pending', 'in_progress') LIMIT 1").fetch_optional(pool).await?.is_some())
}

pub async fn pending_count(pool: &MySqlPool, job_type: Option<JobType>) -> Result<u64, DatabaseError> {
    let row = match job_type {
        Some(t) => sqlx::query("SELECT COUNT(*) AS n FROM jobs WHERE status = 'pending' AND job_type = ?").bind(t.name()).fetch_one(pool).await?,
        None => sqlx::query("SELECT COUNT(*) AS n FROM jobs WHERE status = 'pending'").fetch_one(pool).await?,
    };
    let n: i64 = row.try_get("n")?;
    Ok(n as u64)
}


/// The worker runs jobs in *lanes*, one job at a time per lane, lanes concurrently: every
/// outbound provider call goes through that provider's own lane (so its client's rate
/// limiter sees all of the app's traffic for it in one serial stream), CPU-bound jobs
/// share one lane sized by the scan code's own parallelism, DAT downloads have their own.
pub fn lane_of(job_type: JobType, payload: &serde_json::Value) -> String {
    match job_type {
        JobType::ProviderQuery | JobType::ProviderBulkSync | JobType::MediaDownload => {
            let provider = payload.get("provider").and_then(|v| v.as_str()).map(|s| s.to_ascii_lowercase()).unwrap_or_else(|| "unknown".to_string());
            format!("provider:{provider}")
        }
        // A hash match that goes to a hash-lookup service (Hasheous, PlayMatch) is that
        // traffic: single-file APIs, serial through their limiters.
        JobType::HashMatch if payload.get("to_hasheous").and_then(|v| v.as_bool()).unwrap_or(false) || payload.get("to_playmatch").and_then(|v| v.as_bool()).unwrap_or(false) => "provider:hasheous".to_string(),
        JobType::FileScan | JobType::HashMatch | JobType::DbOptimize | JobType::MediaRelayout => "cpu".to_string(),
        // Its own lane, not `cpu`: the merge pass is all SQL over the catalogue's title
        // tables, and a scan is disk and hashing over the `local_*` ones -- nothing either
        // writes is read by the other, so queued behind a scan it only waited (hours, on a
        // large library) for a core it does not use. One lane still means one pass at a time.
        JobType::TitleDedupe => "titles".to_string(),
        JobType::IgdbReapply | JobType::TitleMergeAudit => "provider:igdb".to_string(),
        JobType::DatSync | JobType::RaPatchDownload => "dat".to_string(),
        JobType::PlaymatchFill => "provider:playmatch".to_string(),
        JobType::RaAccountSync | JobType::RaGenreFetch | JobType::RaBadges => "provider:retroachievements".to_string(),
        // Their own lane, not `cpu`: a backup is disk- and database-bound rather than
        // CPU-bound, so it has no business blocking a file scan -- and one lane for every
        // kind means a database, a catalogue and a seed backup never dump concurrently and
        // fight for the same disk. The seed reads the same tables the catalogue does, so it
        // would be the worst one to run alongside it.
        JobType::BackupDatabase | JobType::BackupCatalog | JobType::BackupDatSeed | JobType::SqliteExport => "backup".to_string(),
        // A restore or an import writes the very tables everything else reads, so it shares
        // the lane that never runs two of those at once rather than racing a backup.
        JobType::RestoreBackup | JobType::SqliteImport => "backup".to_string(),
        // Pulling from a peer is network work, but its own: a slow peer must not hold up
        // provider traffic, and two sources can sync at once only if they are not in one lane.
        JobType::CatalogSync => format!("sync:{}", payload.get("source_id").and_then(|v| v.as_u64()).unwrap_or(0)),
        // The same lane as that source's pull: the pull is what queues it, and the two must
        // not overlap -- a mirror being rewritten under a media sync is a mirror half read.
        JobType::MediaSync => format!("sync:{}", payload.get("source_id").and_then(|v| v.as_u64()).map(|id| id.to_string()).unwrap_or_else(|| "main".to_string())),
        // Per destination: two copies to different drives may run at once and two to the same
        // one may not -- one disk, one queue, and the second would only halve both their
        // throughputs. Hashed because a lane name is a short column, not a path.
        JobType::CopyFiles => format!("copy:{}", &crate::database::import::sha1_hex(payload.get("destination").and_then(|v| v.as_str()).unwrap_or(""))[..8]),
        // Disk-bound and long, like a scan, and for the same reason it must not run two at
        // once against one collection.
        JobType::IntegrityCheck => "cpu".to_string(),
        // Sheet work reads track bytes and writes into the collection, so it belongs in the
        // same single-file queue as a scan -- and `sheet_create` must never run beside a
        // verify that is reading the very files it is about to rename.
        JobType::SheetCreate | JobType::SheetVerify | JobType::M3uVerify => "cpu".to_string(),
        // Disk-bound like the ROM scan, and reads a different tree: its own lane, so a long
        // ROM scan does not hold the music back or the other way round.
        // One lane with the scan: both write the audio tables.
        JobType::AudioScan | JobType::AudioSync | JobType::AudioAutotag => "audio".to_string(),
        // Reads every title name and rewrites a table of its own: beside a scan, not behind it.
        JobType::DictionaryBuild => "dictionary".to_string(),
        // Downloads and reads game files for one table of its own: beside the rest.
        JobType::DisplayData => "display".to_string(),
        // Disk work into an emulator's folders, beside the rest: one emulator at a time.
        // Per destination folder, like a copy: one disk, one queue.
        JobType::ArtworkExport => match payload.get("target").and_then(|t| t.get("dir")).and_then(|d| d.as_str()) {
            Some(dir) => format!("export:{}", &crate::database::import::sha1_hex(dir)[..8]),
            None => format!("export:emulator:{}", payload.get("emulator_id").and_then(|v| v.as_u64()).unwrap_or(0)),
        },
        // Its own lane: it spans every provider, so it would hold any one provider's lane for
        // its whole run, and a scan or a page's own refresh must never wait behind it.
        JobType::MediaBackfill => "media".to_string(),
    }
}

/// Pending jobs, highest priority then oldest first, without claiming them -- the
/// scheduler picks which to claim by lane.
pub async fn peek_pending(pool: &MySqlPool, limit: u32) -> Result<Vec<JobRow>, DatabaseError> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("{JOB_SELECT} WHERE status = 'pending' ORDER BY priority DESC, id LIMIT ?"))).bind(limit).fetch_all(pool).await?;
    rows.iter().map(row_to_job).collect()
}

/// The first pending job of every lane group (job type x priority x provider x the hash
/// match service flags -- everything `lane_of` and the `:fast` slot depend on), highest
/// priority then first in line. "First" is the lowest `queue_order` (migration 0077): the
/// oldest, unless "Run next" ([`run_next`]) moved a job ahead. What the scheduler starts from: unlike a fixed-size
/// `peek_pending` window, a job whose lane is idle is always seen, however many jobs are
/// queued in busy lanes ahead of it.
///
/// Jobs waiting their turn are not among them (`peek_next_in_turn`): one that headed its
/// lane group would hide the ordinary jobs behind it, which are the very jobs it waits for.
pub async fn peek_lane_heads(pool: &MySqlPool) -> Result<Vec<JobRow>, DatabaseError> {
    let sql = format!(
        "{JOB_SELECT} WHERE id IN (SELECT id FROM (SELECT id, ROW_NUMBER() OVER (PARTITION BY job_type, priority, LOWER(JSON_VALUE(payload, '$.provider')), JSON_VALUE(payload, '$.to_hasheous'), JSON_VALUE(payload, '$.to_playmatch') ORDER BY {QUEUE_ORDER}) AS n FROM jobs WHERE status = 'pending' AND in_turn = FALSE) heads WHERE n = 1) ORDER BY priority DESC, {QUEUE_ORDER}"
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(pool).await?;
    rows.iter().map(row_to_job).collect()
}

/// The first pending job that waits its turn (lowest `queue_order`), if any. The worker starts it only when
/// nothing is running (`enqueue_in_turn`).
pub async fn peek_next_in_turn(pool: &MySqlPool) -> Result<Option<JobRow>, DatabaseError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!("{JOB_SELECT} WHERE status = 'pending' AND in_turn = TRUE ORDER BY {QUEUE_ORDER} LIMIT 1"))).fetch_optional(pool).await?;
    row.as_ref().map(row_to_job).transpose()
}

/// Claims one specific pending job (no-op, `None`, if something else took it first).
pub async fn claim_by_id(pool: &MySqlPool, id: u64) -> Result<Option<Job>, DatabaseError> {
    let result = sqlx::query("UPDATE jobs SET status = 'in_progress', claimed_at = NOW(), worker_id = ?, attempts = attempts + 1, progress_done = NULL, progress_total = NULL, progress_message = NULL, progress_detail = NULL WHERE id = ? AND status = 'pending' AND cancel_requested = FALSE").bind(worker_id()).bind(id).execute(pool).await?;
    if result.rows_affected() == 0 {
        return Ok(None);
    }
    Ok(get(pool, id).await?.map(|r| Job { id: r.id, job_type: r.job_type, payload: r.payload, attempts: r.attempts }))
}

/// A job's place in line: `queue_order`, falling back to the id for a row written before
/// migration 0077's backfill could reach it, then the id to break a tie.
const QUEUE_ORDER: &str = "COALESCE(queue_order, id), id";

/// "Run next": pending job `id` moves to the front of its lane -- ahead of every other job
/// waiting in the same lane ([`lane_of`] and the `:fast` slot) with the same `in_turn`
/// flag -- so it starts as soon as the one running there finishes. A running job is never
/// touched, and other lanes are not affected. `None` when `id` is not pending.
///
/// `ids` together (a selection) keep their relative order: they are moved last first, so
/// the first of them ends up in front.
pub async fn run_next(pool: &MySqlPool, ids: &[u64]) -> Result<Vec<(u64, Option<u64>)>, DatabaseError> {
    let mut wanted: Vec<JobRow> = Vec::new();
    for id in ids {
        if let Some(row) = get(pool, *id).await?.filter(|r| r.status == "pending") {
            wanted.push(row);
        }
    }
    // Their order in line as it is now, so moving them one by one, last first, keeps it.
    let mut order: Vec<(i64, u64)> = Vec::new();
    for row in &wanted {
        let q: Option<i64> = sqlx::query_scalar("SELECT queue_order FROM jobs WHERE id = ?").bind(row.id).fetch_one(pool).await?;
        order.push((q.unwrap_or(row.id as i64), row.id));
    }
    order.sort();
    for (_, id) in order.iter().rev() {
        let Some(row) = wanted.iter().find(|r| r.id == *id) else { continue };
        let lowest = lane_peers(pool, row).await?.into_iter().map(|(_, q)| q).min();
        if let Some(lowest) = lowest {
            sqlx::query("UPDATE jobs SET queue_order = ? WHERE id = ? AND status = 'pending'").bind(lowest - 1).bind(row.id).execute(pool).await?;
        }
    }
    let mut out = Vec::new();
    for id in ids {
        out.push((*id, position_in_lane(pool, *id).await?));
    }
    Ok(out)
}

/// Where pending job `id` stands among the jobs waiting in its lane: 1 = starts next. `None`
/// when it is not pending.
pub async fn position_in_lane(pool: &MySqlPool, id: u64) -> Result<Option<u64>, DatabaseError> {
    let Some(row) = get(pool, id).await?.filter(|r| r.status == "pending") else { return Ok(None) };
    let mut peers = lane_peers(pool, &row).await?;
    peers.sort_by_key(|(peer, q)| (*q, *peer));
    Ok(peers.iter().position(|(peer, _)| *peer == id).map(|i| i as u64 + 1))
}

/// Every pending job in `row`'s lane (same lane name and `in_turn` flag, `row` included),
/// with its place in line.
async fn lane_peers(pool: &MySqlPool, row: &JobRow) -> Result<Vec<(u64, i64)>, DatabaseError> {
    let rows = sqlx::query("SELECT id, job_type, payload, priority, COALESCE(queue_order, id) AS q FROM jobs WHERE status = 'pending' AND in_turn = ?").bind(row.in_turn).fetch_all(pool).await?;
    let mut out = Vec::new();
    for r in rows {
        let type_name: String = r.try_get("job_type")?;
        let Some(job_type) = JobType::from_name(&type_name) else { continue };
        let payload: serde_json::Value = r.try_get("payload")?;
        let priority: u16 = r.try_get("priority")?;
        let mut lane = lane_of(job_type, &payload);
        if priority > 0 {
            lane.push_str(":fast");
        }
        if lane == row.lane {
            out.push((r.try_get("id")?, r.try_get("q")?));
        }
    }
    Ok(out)
}

pub const STATUSES: [&str; 5] = ["pending", "in_progress", "done", "failed", "cancelled"];

fn row_to_job(row: &sqlx::mysql::MySqlRow) -> Result<JobRow, DatabaseError> {
    let type_name: String = row.try_get("job_type")?;
    let job_type = JobType::from_name(&type_name).ok_or_else(|| DatabaseError::Parse(format!("unknown job_type {type_name:?} in jobs table")))?;
    let payload: serde_json::Value = row.try_get("payload")?;
    let priority: u16 = row.try_get("priority")?;
    let mut lane = lane_of(job_type, &payload);
    if priority > 0 {
        lane.push_str(":fast");
    }
    Ok(JobRow {
        id: row.try_get("id")?,
        job_type,
        status: row.try_get("status")?,
        lane,
        payload,
        priority,
        in_turn: row.try_get("in_turn")?,
        attempts: row.try_get("attempts")?,
        last_error: row.try_get("last_error")?,
        claimed_at: row.try_get("claimed_at")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        progress_done: row.try_get("progress_done")?,
        progress_total: row.try_get("progress_total")?,
        progress_message: row.try_get("progress_message")?,
        progress_detail: row.try_get("progress_detail")?,
        cancel_requested: row.try_get("cancel_requested")?,
        result: row.try_get("result")?,
    })
}

const JOB_SELECT: &str = "SELECT id, job_type, status, payload, attempts, priority, in_turn, last_error, progress_done, progress_total, progress_message, progress_detail, result, cancel_requested, claimed_at, created_at, updated_at FROM jobs";

/// Newest first, optionally narrowed by type and/or status. Returns the page and the total
/// matching count.
pub async fn list(pool: &MySqlPool, job_types: &[JobType], status: Option<&str>, limit: u32, offset: u32) -> Result<(Vec<JobRow>, u64), DatabaseError> {
    let mut clauses: Vec<String> = Vec::new();
    if !job_types.is_empty() {
        clauses.push(format!("job_type IN ({})", vec!["?"; job_types.len()].join(", ")));
    }
    if status.is_some() {
        clauses.push("status = ?".to_string());
    }
    let where_sql = if clauses.is_empty() { String::new() } else { format!(" WHERE {}", clauses.join(" AND ")) };

    let count_sql = format!("SELECT COUNT(*) AS n FROM jobs{where_sql}");
    let mut count_query = sqlx::query(sqlx::AssertSqlSafe(count_sql));
    for t in job_types {
        count_query = count_query.bind(t.name());
    }
    if let Some(s) = status {
        count_query = count_query.bind(s);
    }
    let total: i64 = count_query.fetch_one(pool).await?.try_get("n")?;

    let sql = format!("{JOB_SELECT}{where_sql} ORDER BY id DESC LIMIT ? OFFSET ?");
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    for t in job_types {
        query = query.bind(t.name());
    }
    if let Some(s) = status {
        query = query.bind(s);
    }
    let rows = query.bind(limit).bind(offset).fetch_all(pool).await?;
    let jobs = rows.iter().map(row_to_job).collect::<Result<Vec<_>, _>>()?;
    Ok((jobs, total as u64))
}

pub async fn get(pool: &MySqlPool, id: u64) -> Result<Option<JobRow>, DatabaseError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!("{JOB_SELECT} WHERE id = ?"))).bind(id).fetch_optional(pool).await?;
    row.as_ref().map(row_to_job).transpose()
}

/// A `failed` job back to `pending` with its attempt counter reset. Returns false if the
/// job wasn't in `failed`.
pub async fn retry(pool: &MySqlPool, id: u64) -> Result<bool, DatabaseError> {
    let result = sqlx::query("UPDATE jobs SET status = 'pending', attempts = 0, last_error = NULL, cancel_requested = FALSE WHERE id = ? AND status IN ('failed', 'cancelled')").bind(id).execute(pool).await?;
    Ok(result.rows_affected() > 0)
}

/// Row count per status, every status present (zero when empty).
pub async fn status_counts(pool: &MySqlPool) -> Result<Vec<(String, u64)>, DatabaseError> {
    let rows = sqlx::query("SELECT status, COUNT(*) AS n FROM jobs GROUP BY status").fetch_all(pool).await?;
    let mut counts: Vec<(String, u64)> = STATUSES.iter().map(|s| (s.to_string(), 0)).collect();
    for row in rows {
        let status: String = row.try_get("status")?;
        let n: i64 = row.try_get("n")?;
        if let Some(entry) = counts.iter_mut().find(|(s, _)| *s == status) {
            entry.1 = n as u64;
        }
    }
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A step reporting in its own units lands inside its slice of the whole, and no step
    /// can carry the bar past the end of its slice.
    #[test]
    fn a_framed_report_stays_inside_its_slice() {
        // The second of six sources, importing: the second half of 1000..2000 out of 6000.
        let frame = Frame { base: 1500, span: 500, whole: 6000 };
        assert_eq!(frame.place(0, Some(40)), (1500, Some(6000)));
        assert_eq!(frame.place(20, Some(40)), (1750, Some(6000)));
        assert_eq!(frame.place(40, Some(40)), (2000, Some(6000)));
        assert_eq!(frame.place(99, Some(40)), (2000, Some(6000)), "past its own total: held at the slice's end");
        assert_eq!(frame.place(7, None), (1500, Some(6000)), "no total known: the start of the slice");
        assert_eq!(frame.place(7, Some(0)), (1500, Some(6000)));
        // Byte counts are large; the arithmetic must not overflow on the way.
        assert_eq!(Frame { base: 0, span: 500, whole: 6000 }.place(u64::MAX / 2, Some(u64::MAX)), (249, Some(6000)));
        assert_eq!(Frame { base: 5900, span: 500, whole: 6000 }.place(1, Some(1)), (6000, Some(6000)), "never past the whole");
    }

    /// A per-platform step's own messages keep saying which platform of how many it is.
    #[test]
    fn a_prefix_goes_in_front_of_every_message_until_cleared() {
        let progress = Progress::none();
        assert_eq!(progress.prefixed("Nes: page 2"), "Nes: page 2");
        progress.set_prefix(Some("platform 3 of 40: ".to_string()));
        assert_eq!(progress.prefixed("Nes: page 2"), "platform 3 of 40: Nes: page 2");
        assert_eq!(progress.prefixed(&"x".repeat(300)).chars().count(), 255, "still fits the column");
        progress.set_prefix(None);
        assert_eq!(progress.prefixed("done"), "done");
    }

    #[test]
    fn progress_without_a_frame_reports_what_it_is_given() {
        let progress = Progress::none();
        assert_eq!(progress.framed(3, Some(51)), (3, Some(51)));
        assert_eq!(progress.framed(3, None), (3, None));
        progress.set_frame(Some(Frame { base: 0, span: 500, whole: 6000 }));
        assert_eq!(progress.framed(1, Some(2)), (250, Some(6000)));
        progress.set_frame(None);
        assert_eq!(progress.framed(3, Some(51)), (3, Some(51)), "and again once the frame is cleared");
    }

    /// The step list says which step is running, which are done and how they came out.
    #[tokio::test]
    async fn steps_keep_a_list_of_every_step() {
        let progress = Progress::none();
        let mut steps = Steps::new(&progress, &[("a", "first"), ("b", "second"), ("c", "third")], "full pass");
        let d = steps.detail();
        assert_eq!(d["step"], 0);
        assert!(d["steps"].as_array().unwrap().iter().all(|r| r["state"] == "pending"));
        steps.start(1).await;
        steps.done("3 merged".to_string()).await;
        steps.start(2).await;
        assert_eq!(progress.prefixed("x"), "step 2 of 3: x");
        assert_eq!(progress.framed(1, Some(2)), (1500, Some(3000)));
        let d = steps.detail();
        assert_eq!((d["step"].as_u64(), d["base"].as_u64(), d["span"].as_u64()), (Some(2), Some(1000), Some(1000)));
        assert_eq!(d["scope"], "full pass");
        let rows = d["steps"].as_array().unwrap();
        assert_eq!((rows[0]["state"].as_str(), rows[0]["note"].as_str()), (Some("done"), Some("3 merged")));
        assert!(rows[0]["secs"].is_u64());
        assert_eq!((rows[1]["state"].as_str(), rows[1]["key"].as_str(), rows[1]["label"].as_str()), (Some("running"), Some("b"), Some("second")));
        assert_eq!(rows[2]["state"], "pending");
        // A step started without the last one being ended still ends it, with no note.
        steps.start(3).await;
        let d = steps.detail();
        assert_eq!(d["steps"][1]["state"], "done");
        assert!(d["steps"][1]["note"].is_null());
        steps.finish();
        assert_eq!(progress.prefixed("done"), "done");
        assert_eq!(progress.framed(1, Some(2)), (1, Some(2)));
    }

    /// Step 4 of 7 with sub-steps of its own: the sub-steps stay inside step 4's seventh.
    #[test]
    fn an_inner_frame_lands_inside_the_outer_one() {
        let progress = Progress::none();
        progress.set_inner_frame(Some(Frame { base: 45, span: 20, whole: 100 }));
        assert_eq!(progress.framed(1, Some(2)), (55, Some(100)), "on its own it is the whole scale");
        progress.set_frame(Some(Frame { base: 3000, span: 1000, whole: 7000 }));
        assert_eq!(progress.framed(1, Some(2)), (3500, Some(7000)), "a new step starts with no inner frame");
        progress.set_inner_frame(Some(Frame { base: 45, span: 20, whole: 100 }));
        assert_eq!(progress.framed(0, None), (3450, Some(7000)));
        assert_eq!(progress.framed(1, Some(2)), (3550, Some(7000)));
        progress.set_inner_frame(Some(Frame { base: 95, span: 5, whole: 100 }));
        assert_eq!(progress.framed(9, Some(9)), (4000, Some(7000)), "never past the step's end");
        progress.set_inner_frame(None);
        assert_eq!(progress.framed(1, Some(4)), (3250, Some(7000)));
    }

    #[test]
    fn job_type_name_round_trips() {
        for t in JobType::ALL {
            assert_eq!(JobType::from_name(t.name()), Some(t));
        }
        assert_eq!(JobType::from_name("hash_match"), Some(JobType::HashMatch));
        assert_eq!(JobType::from_name("not_a_real_job_type"), None);
    }

    /// Only a copy waits for the user after a restart; everything else picks up by itself.
    #[test]
    fn only_a_copy_is_not_resumed_after_a_restart() {
        let not_resuming: Vec<JobType> = JobType::ALL.into_iter().filter(|t| !t.resumes_after_restart()).collect();
        assert_eq!(not_resuming, vec![JobType::CopyFiles]);
        assert_eq!(not_resuming_sql(), "'copy_files'");
    }

    #[test]
    fn lanes_group_by_provider_and_kind() {
        use serde_json::json;
        assert_eq!(lane_of(JobType::ProviderQuery, &json!({"provider": "IGDB"})), "provider:igdb");
        assert_eq!(lane_of(JobType::ProviderBulkSync, &json!({"provider": "retroachievements"})), "provider:retroachievements");
        assert_eq!(lane_of(JobType::MediaDownload, &json!({})), "provider:unknown");
        assert_eq!(lane_of(JobType::FileScan, &json!({})), "cpu");
        assert_eq!(lane_of(JobType::HashMatch, &json!({})), "cpu");
        assert_eq!(lane_of(JobType::HashMatch, &json!({"to_hasheous": true})), "provider:hasheous");
        assert_eq!(lane_of(JobType::DbOptimize, &json!({})), "cpu");
        assert_eq!(lane_of(JobType::TitleDedupe, &json!({})), "titles", "beside a scan, not behind it");
        assert_eq!(lane_of(JobType::DatSync, &json!({})), "dat");
    }

    #[test]
    fn file_scan_mode_name_round_trips() {
        for m in FileScanMode::ALL {
            assert_eq!(FileScanMode::from_name(m.name()), Some(m));
        }
        assert_eq!(FileScanMode::from_name("new_only"), Some(FileScanMode::NewOnly));
        assert_eq!(FileScanMode::from_name("quickscan"), None);
    }
}
