//! `schedules`: recurring syncs. Each row is a job payload (`target`, e.g. a
//! `provider_bulk_sync` or `dat_sync`) plus when to enqueue it:
//!
//! - `fixed`: every `fixed_every_n` `fixed_unit` (minutes / hours / days / weeks / months);
//! - `check`: every `check_every_n` `check_unit`, run the provider's cheap change check
//!   (`providers::change_check`) and enqueue a diff job only when it reports a change;
//! - `both`: the two independently.
//!
//! The worker's scheduler step (`worker::run_due_schedules`) calls [`due`] about once a
//! minute. Nothing here talks to a provider.

use chrono::{Duration, Months, NaiveDateTime};
use serde::{Deserialize, Serialize};
use sqlx::mysql::MySqlPool;
use sqlx::Row;

use super::error::DatabaseError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Minutes,
    Hours,
    Days,
    Weeks,
    Months,
}

impl Unit {
    pub fn name(self) -> &'static str {
        match self {
            Self::Minutes => "minutes",
            Self::Hours => "hours",
            Self::Days => "days",
            Self::Weeks => "weeks",
            Self::Months => "months",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Minutes, Self::Hours, Self::Days, Self::Weeks, Self::Months].into_iter().find(|u| u.name() == name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    Fixed,
    Check,
    Both,
}

impl Trigger {
    pub fn name(self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::Check => "check",
            Self::Both => "both",
        }
    }

    pub fn fixed(self) -> bool {
        matches!(self, Self::Fixed | Self::Both)
    }

    pub fn check(self) -> bool {
        matches!(self, Self::Check | Self::Both)
    }
}

/// `n` `unit`s after `from`. Months are calendar months (Jan 31 + 1 month = Feb 28/29).
/// An interval past what MySQL's DATETIME can hold (a typo of a huge `n`) lands on its
/// latest date rather than panicking or failing the write.
pub fn advance(from: NaiveDateTime, n: u32, unit: Unit) -> NaiveDateTime {
    let latest = chrono::NaiveDate::from_ymd_opt(9999, 12, 31).and_then(|d| d.and_hms_opt(23, 59, 59)).unwrap_or(NaiveDateTime::MAX);
    let n = n.max(1) as i64;
    let step = match unit {
        Unit::Minutes => Duration::try_minutes(n),
        Unit::Hours => Duration::try_hours(n),
        Unit::Days => Duration::try_days(n),
        Unit::Weeks => Duration::try_weeks(n),
        Unit::Months => return from.checked_add_months(Months::new(n as u32)).unwrap_or(latest).min(latest),
    };
    step.and_then(|d| from.checked_add_signed(d)).unwrap_or(latest).min(latest)
}

/// The input of a create/update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleSpec {
    pub name: String,
    /// A job payload including `job_type` (validated by `api::schedules`).
    pub target: serde_json::Value,
    #[serde(default = "default_mode")]
    pub mode: String,
    pub trigger: Trigger,
    pub fixed_every_n: Option<u32>,
    pub fixed_unit: Option<Unit>,
    pub check_every_n: Option<u32>,
    pub check_unit: Option<Unit>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Hold a due run until the job queue is empty (`jobs::queue_busy`), rather than start
    /// it alongside whatever is running. Its timers do not move while it waits.
    #[serde(default = "default_true")]
    pub when_idle: bool,
}

fn default_mode() -> String {
    "diff".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize)]
pub struct Schedule {
    pub id: u64,
    #[serde(flatten)]
    pub spec: ScheduleSpec,
    pub next_fixed_at: Option<NaiveDateTime>,
    pub next_check_at: Option<NaiveDateTime>,
    pub last_run_at: Option<NaiveDateTime>,
    pub last_job_id: Option<u64>,
    pub last_check_at: Option<NaiveDateTime>,
    pub last_check_result: Option<String>,
}

const SELECT: &str = "SELECT id, name, target, mode, trigger_kind, fixed_every_n, fixed_unit, check_every_n, check_unit, enabled, when_idle, next_fixed_at, next_check_at, last_run_at, last_job_id, last_check_at, last_check_result FROM schedules WHERE deleted = FALSE";

fn row_to_schedule(r: &sqlx::mysql::MySqlRow) -> Result<Schedule, DatabaseError> {
    let unit = |col: &str| -> Result<Option<Unit>, DatabaseError> { Ok(r.try_get::<Option<String>, _>(col)?.as_deref().and_then(Unit::from_name)) };
    let trigger: String = r.try_get("trigger_kind")?;
    Ok(Schedule {
        id: r.try_get("id")?,
        spec: ScheduleSpec {
            name: r.try_get("name")?,
            target: r.try_get("target")?,
            mode: r.try_get("mode")?,
            trigger: match trigger.as_str() {
                "check" => Trigger::Check,
                "both" => Trigger::Both,
                _ => Trigger::Fixed,
            },
            fixed_every_n: r.try_get("fixed_every_n")?,
            fixed_unit: unit("fixed_unit")?,
            check_every_n: r.try_get("check_every_n")?,
            check_unit: unit("check_unit")?,
            enabled: r.try_get("enabled")?,
            when_idle: r.try_get("when_idle")?,
        },
        next_fixed_at: r.try_get("next_fixed_at")?,
        next_check_at: r.try_get("next_check_at")?,
        last_run_at: r.try_get("last_run_at")?,
        last_job_id: r.try_get("last_job_id")?,
        last_check_at: r.try_get("last_check_at")?,
        last_check_result: r.try_get("last_check_result")?,
    })
}

pub async fn list(pool: &MySqlPool) -> Result<Vec<Schedule>, DatabaseError> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("{SELECT} ORDER BY name, id"))).fetch_all(pool).await?;
    rows.iter().map(row_to_schedule).collect()
}

pub async fn get(pool: &MySqlPool, id: u64) -> Result<Option<Schedule>, DatabaseError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!("{SELECT} AND id = ?"))).bind(id).fetch_optional(pool).await?;
    row.as_ref().map(row_to_schedule).transpose()
}

/// First times: a fixed trigger first fires one interval from now, a check trigger at the
/// next scheduler step.
fn first_times(spec: &ScheduleSpec, now: NaiveDateTime) -> (Option<NaiveDateTime>, Option<NaiveDateTime>) {
    let fixed = (spec.trigger.fixed()).then(|| advance(now, spec.fixed_every_n.unwrap_or(1), spec.fixed_unit.unwrap_or(Unit::Days)));
    let check = spec.trigger.check().then_some(now);
    (fixed, check)
}

pub async fn create(pool: &MySqlPool, spec: &ScheduleSpec) -> Result<u64, DatabaseError> {
    let now = chrono::Local::now().naive_local();
    let (fixed, check) = first_times(spec, now);
    let result = sqlx::query("INSERT INTO schedules (name, target, mode, trigger_kind, fixed_every_n, fixed_unit, check_every_n, check_unit, enabled, when_idle, next_fixed_at, next_check_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
        .bind(&spec.name)
        .bind(&spec.target)
        .bind(&spec.mode)
        .bind(spec.trigger.name())
        .bind(spec.fixed_every_n)
        .bind(spec.fixed_unit.map(Unit::name))
        .bind(spec.check_every_n)
        .bind(spec.check_unit.map(Unit::name))
        .bind(spec.enabled)
        .bind(spec.when_idle)
        .bind(fixed)
        .bind(check)
        .execute(pool)
        .await?;
    Ok(result.last_insert_id())
}

/// Replaces the spec and restarts its timers from now.
pub async fn update(pool: &MySqlPool, id: u64, spec: &ScheduleSpec) -> Result<bool, DatabaseError> {
    let now = chrono::Local::now().naive_local();
    let (fixed, check) = first_times(spec, now);
    let result = sqlx::query("UPDATE schedules SET name = ?, target = ?, mode = ?, trigger_kind = ?, fixed_every_n = ?, fixed_unit = ?, check_every_n = ?, check_unit = ?, enabled = ?, when_idle = ?, next_fixed_at = ?, next_check_at = ? WHERE id = ? AND deleted = FALSE")
        .bind(&spec.name)
        .bind(&spec.target)
        .bind(&spec.mode)
        .bind(spec.trigger.name())
        .bind(spec.fixed_every_n)
        .bind(spec.fixed_unit.map(Unit::name))
        .bind(spec.check_every_n)
        .bind(spec.check_unit.map(Unit::name))
        .bind(spec.enabled)
        .bind(spec.when_idle)
        .bind(fixed)
        .bind(check)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn delete(pool: &MySqlPool, id: u64) -> Result<bool, DatabaseError> {
    Ok(sqlx::query("UPDATE schedules SET deleted = TRUE WHERE id = ? AND deleted = FALSE").bind(id).execute(pool).await?.rows_affected() > 0)
}

/// What is due at `now`: (schedule, fixed due, check due) for enabled schedules.
pub async fn due(pool: &MySqlPool, now: NaiveDateTime) -> Result<Vec<(Schedule, bool, bool)>, DatabaseError> {
    Ok(list(pool)
        .await?
        .into_iter()
        .filter(|s| s.spec.enabled)
        .map(|s| {
            let fixed = s.spec.trigger.fixed() && s.next_fixed_at.is_some_and(|t| t <= now);
            let check = s.spec.trigger.check() && s.next_check_at.is_some_and(|t| t <= now);
            (s, fixed, check)
        })
        .filter(|(_, f, c)| *f || *c)
        .collect())
}

/// After a fixed run: when it ran, which job, and the next time.
pub async fn fixed_ran(pool: &MySqlPool, s: &Schedule, job_id: Option<u64>, now: NaiveDateTime) -> Result<(), DatabaseError> {
    let next = advance(now, s.spec.fixed_every_n.unwrap_or(1), s.spec.fixed_unit.unwrap_or(Unit::Days));
    sqlx::query("UPDATE schedules SET last_run_at = IF(? IS NULL, last_run_at, ?), last_job_id = COALESCE(?, last_job_id), next_fixed_at = ? WHERE id = ?")
        .bind(job_id)
        .bind(now)
        .bind(job_id)
        .bind(next)
        .bind(s.id)
        .execute(pool)
        .await?;
    Ok(())
}

/// After a check: its result, the job it enqueued if any, and the next check time.
pub async fn checked(pool: &MySqlPool, s: &Schedule, result: &str, job_id: Option<u64>, now: NaiveDateTime) -> Result<(), DatabaseError> {
    let next = advance(now, s.spec.check_every_n.unwrap_or(1), s.spec.check_unit.unwrap_or(Unit::Hours));
    let result: String = result.chars().take(255).collect();
    sqlx::query("UPDATE schedules SET last_check_at = ?, last_check_result = ?, next_check_at = ?, last_run_at = IF(? IS NULL, last_run_at, ?), last_job_id = COALESCE(?, last_job_id) WHERE id = ?")
        .bind(now)
        .bind(result)
        .bind(next)
        .bind(job_id)
        .bind(now)
        .bind(job_id)
        .bind(s.id)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_huge_interval_lands_on_the_latest_date_instead_of_panicking() {
        let now = chrono::NaiveDate::from_ymd_opt(2026, 10, 29).unwrap().and_hms_opt(12, 0, 0).unwrap();
        for unit in [Unit::Minutes, Unit::Hours, Unit::Days, Unit::Weeks, Unit::Months] {
            assert_eq!(advance(now, u32::MAX, unit).date(), chrono::NaiveDate::from_ymd_opt(9999, 12, 31).unwrap());
        }
        assert_eq!(advance(now, 2, Unit::Hours), now + Duration::hours(2));
    }

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    #[test]
    fn advance_by_each_unit() {
        let t = at("2026-01-31 10:00");
        assert_eq!(advance(t, 30, Unit::Minutes), at("2026-01-31 10:30"));
        assert_eq!(advance(t, 2, Unit::Hours), at("2026-01-31 12:00"));
        assert_eq!(advance(t, 1, Unit::Days), at("2026-02-01 10:00"));
        assert_eq!(advance(t, 2, Unit::Weeks), at("2026-02-14 10:00"));
        assert_eq!(advance(t, 1, Unit::Months), at("2026-02-28 10:00"), "calendar month, clamped to the month's end");
        assert_eq!(advance(t, 0, Unit::Days), at("2026-02-01 10:00"), "0 counts as 1");
    }

    #[test]
    fn trigger_kinds() {
        assert!(Trigger::Fixed.fixed() && !Trigger::Fixed.check());
        assert!(Trigger::Both.fixed() && Trigger::Both.check());
        assert_eq!(Unit::from_name("weeks"), Some(Unit::Weeks));
    }
}
