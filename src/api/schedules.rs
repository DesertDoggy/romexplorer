//! Recurring syncs (`database::schedules`): list / create / update / delete, and "run now".
//! A schedule's `target` is an ordinary job request (`api::jobs::JobRequest` JSON), checked
//! with the same validation as a job enqueued by hand.

use crate::database::schedules::{self as db, Schedule, ScheduleSpec};

use super::jobs::{self, JobRequest, SyncModeName};
use super::{App, Error, Result};

/// Job types a schedule may run.
// `copy_files` is deliberately absent: a copy needs a live destination and a live mount, and
// one firing on a schedule into a path that has since gone stale is a data-loss shape.
// `integrity_check` is the opposite -- occasional, read-only, and self-limiting.
//
// The two sheet *verifies* join it for the same reasons, and `sheet_create` deliberately does
// not: it writes files into the collection and can rename the user's own ROMs, which is not
// something to do on a timer with nobody watching.
//
// `file_scan` is here for its quick mode (`new_only`): it reads only files that have no row
// yet, so a run with nothing new costs a directory listing. A full scan is not refused --
// it is the same request -- but it is the quick one a timer is for.
pub const SCHEDULABLE: [&str; 14] = [
    "provider_bulk_sync", "dat_sync", "db_optimize", "ra_account_sync", "ra_genre_fetch", "title_dedupe", "backup_database", "backup_catalog", "integrity_check", "sheet_verify", "m3u_verify",
    "file_scan", "media_backfill", "ra_badges",
];

async fn validate(app: &App, spec: &ScheduleSpec) -> Result<JobRequest> {
    if spec.name.trim().is_empty() {
        return Err(Error::invalid("name is required"));
    }
    let job_type = spec.target.get("job_type").and_then(|v| v.as_str()).unwrap_or("");
    if !SCHEDULABLE.contains(&job_type) {
        return Err(Error::invalid(format!("target job_type must be one of {}", SCHEDULABLE.join(", "))));
    }
    let request: JobRequest = serde_json::from_value(spec.target.clone()).map_err(|e| Error::invalid(format!("target: {e}")))?;
    jobs::validate(app, &request).await?;
    if !matches!(spec.mode.as_str(), "diff" | "redownload") {
        return Err(Error::invalid("mode must be diff or redownload"));
    }
    if spec.trigger.fixed() && (spec.fixed_every_n.unwrap_or(0) == 0 || spec.fixed_unit.is_none()) {
        return Err(Error::invalid("a fixed trigger needs fixed_every_n (> 0) and fixed_unit"));
    }
    if spec.trigger.check() && (spec.check_every_n.unwrap_or(0) == 0 || spec.check_unit.is_none()) {
        return Err(Error::invalid("a check trigger needs check_every_n (> 0) and check_unit"));
    }
    Ok(request)
}

pub async fn list(app: &App) -> Result<Vec<Schedule>> {
    Ok(db::list(&app.pool).await?)
}

pub async fn get(app: &App, id: u64) -> Result<Schedule> {
    db::get(&app.pool, id).await?.ok_or_else(|| Error::not_found(format!("no schedule {id}")))
}

pub async fn create(app: &App, spec: &ScheduleSpec) -> Result<Schedule> {
    validate(app, spec).await?;
    let id = db::create(&app.pool, spec).await?;
    get(app, id).await
}

pub async fn update(app: &App, id: u64, spec: &ScheduleSpec) -> Result<Schedule> {
    validate(app, spec).await?;
    if !db::update(&app.pool, id, spec).await? {
        return Err(Error::not_found(format!("no schedule {id}")));
    }
    get(app, id).await
}

pub async fn delete(app: &App, id: u64) -> Result<()> {
    if !db::delete(&app.pool, id).await? {
        return Err(Error::not_found(format!("no schedule {id}")));
    }
    Ok(())
}

/// Enqueues the schedule's job now (in its own mode), without moving its timers. `None`
/// when the same job is already pending or running.
pub async fn run_now(app: &App, id: u64) -> Result<Option<u64>> {
    let s = get(app, id).await?;
    let request = validate(app, &s.spec).await?;
    let mode = if s.spec.mode == "redownload" { SyncModeName::Redownload } else { SyncModeName::Diff };
    jobs::enqueue_unless_queued(app, &jobs::with_mode(&request, mode)).await
}
