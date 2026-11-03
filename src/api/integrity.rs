//! Checking that the files this machine holds still are what the catalogue says they are.
//!
//! A cheap counterpart to a rehash scan. `file_scan { mode: rehash }` re-derives every file's
//! full identity -- crc32, md5, sha1, and whatever else is configured -- which is the right
//! thing when you do not know what a file is. Here we do: the `content` row says so. The only
//! open question is whether the bytes on disk still match, and one algorithm answers it.
//!
//! So this reads each file **once** and takes two answers off that read:
//!
//! 1. **Does it still match?** -- compared against `integrity_populate_hash` (xxh3_128 by
//!    default) when the catalogue already records it for that content; otherwise against
//!    whichever recorded hash is cheapest to compute on this CPU (`rom::hash::fallback_order`:
//!    crc32, then xxh3_128, then sha1/sha256 where there is hardware SHA and md5 where there
//!    is not, since MD5 has no hardware path and beats software SHA). It is always a hash the
//!    catalogue already held: comparing against one it did not could not settle anything.
//! 2. **Does the catalogue now also hold the chosen hash?** -- after a fallback,
//!    `integrity_populate_hash` is computed off the same pass and recorded *only after* the
//!    comparison passed, so the next check compares against it. A hash taken from a file nobody has confirmed must
//!    never become the catalogue's idea of what that content is; `hashes` hangs off `content`,
//!    which other copies of those bytes share.
//!
//! A file that passes is stamped with `local_files.last_integrity_check`, which is also the
//! ordering: a run takes the least recently checked first, so repeated short runs sweep the
//! collection rather than re-reading its head. A file that fails is not stamped, so it stays
//! at the front of the queue until someone deals with it; the mismatch itself is recorded in
//! `last_integrity_failure`, which the next pass clears, so the file page can say which of
//! "passed", "failed" and "never looked at" it is.
//!
//! The two stamps mean what `common::models::FileData`'s two methods mean: `last_full_check`
//! is `FileData::hash` (every algorithm, archive members and all) and only a scan earns it;
//! `last_integrity_check` is `FileData::integrity` (one hash, compared) and this job and a
//! verified copy earn it. A file registered by a copy has no full check and hashes it never
//! computed, which is exactly right -- and makes it the most worthwhile thing here to look at.

use std::path::Path;

pub use romexplorer_types::integrity::{IntegrityRequest, IntegritySummary};
use sqlx::Row;

use crate::database::jobs::Progress;
use crate::database::settings as db_settings;

use super::copy::{algorithm_from_name, is_quick_hash, CheckOutcome, QUICK_HASHES};
use super::{App, Error, Result};

/// The algorithm an integrity run checks with, from the request or the setting: compared
/// against wherever the catalogue records it, and recorded (after a match against the
/// cheapest recorded hash) wherever it does not yet.
pub async fn populate_algorithm(app: &App, request: &IntegrityRequest) -> Result<String> {
    if let Some(name) = request.algorithm.as_deref() {
        if !is_quick_hash(name) {
            return Err(Error::invalid(format!("integrity_check: the algorithm must be one of {}", QUICK_HASHES.join(" | "))));
        }
        return Ok(name.to_string());
    }
    Ok(db_settings::get::<String>(&app.pool, db_settings::KEY_INTEGRITY_POPULATE_HASH).await?.filter(|n| is_quick_hash(n)).unwrap_or_else(super::copy::default_quick_hash))
}

struct Candidate {
    id: u64,
    path: String,
    content_id: u64,
    size: u64,
}

/// Least recently checked first, so short runs repeated over time sweep the whole collection
/// instead of re-reading its beginning.
async fn candidates(app: &App, request: &IntegrityRequest) -> Result<Vec<Candidate>> {
    let mut sql = String::from(
        "SELECT f.id, f.path, f.content_id, f.size FROM local_files f WHERE f.deleted = FALSE",
    );
    if request.directory_id.is_some() {
        sql.push_str(" AND f.directory_id = ?");
    }
    if request.older_than_days.is_some() {
        sql.push_str(" AND (f.last_integrity_check IS NULL OR f.last_integrity_check < NOW() - INTERVAL ? DAY)");
    }
    // NULL first is the point: a file never checked is the one most worth looking at.
    sql.push_str(" ORDER BY f.last_integrity_check IS NOT NULL, f.last_integrity_check, f.id");
    let limit = request.limit.unwrap_or(0);
    if limit > 0 {
        sql.push_str(" LIMIT ?");
    }
    let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
    if let Some(id) = request.directory_id {
        q = q.bind(id);
    }
    if let Some(days) = request.older_than_days {
        q = q.bind(days);
    }
    if limit > 0 {
        q = q.bind(limit);
    }
    let rows = q.fetch_all(&app.pool).await?;
    rows.iter().map(|r| Ok(Candidate { id: r.try_get("id")?, path: r.try_get("path")?, content_id: r.try_get("content_id")?, size: r.try_get("size")? })).collect()
}

pub async fn validate(app: &App, request: &IntegrityRequest) -> Result<()> {
    populate_algorithm(app, request).await?;
    if let Some(id) = request.directory_id {
        let dir = crate::database::directories::get(&app.pool, id).await?;
        match dir {
            Some(d) if !d.deleted => {}
            Some(_) => return Err(Error::invalid(format!("directory {id} is deleted"))),
            None => return Err(Error::not_found(format!("no directory {id}"))),
        }
    }
    Ok(())
}

pub async fn run(app: &App, request: &IntegrityRequest, progress: &Progress, job_id: Option<u64>) -> Result<IntegritySummary> {
    let populate_name = populate_algorithm(app, request).await?;
    let populate = algorithm_from_name(&populate_name).ok_or_else(|| Error::invalid(format!("{populate_name:?} can't be used on its own")))?;
    let mut summary = IntegritySummary { algorithm: populate_name.clone(), ..Default::default() };

    progress.force(0, None, "listing files").await;
    let files = candidates(app, request).await?;
    let total: u64 = files.iter().map(|f| f.size).sum();
    let count = files.len();
    let mut done = 0u64;

    let cancel = super::copy::run::Cancel::new();
    let stop = super::copy::run::Cancel::new();
    let watcher = job_id.map(|id| cancel.watch(app, id, stop.clone()));

    for (i, file) in files.iter().enumerate() {
        if cancel.is_set() {
            summary.cancelled = true;
            break;
        }
        let name = Path::new(&file.path).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| file.path.clone());
        progress.report(done, Some(total), &format!("file {} of {count}: {name}", i + 1)).await;
        done += file.size;

        // Gone from disk is not a mismatch, it is the thing a scan soft-deletes for. Say so
        // and mark it, rather than reporting a file that is simply not there as corrupt.
        if !Path::new(&file.path).exists() {
            summary.missing.push(file.path.clone());
            let _ = sqlx::query("UPDATE local_files SET deleted = TRUE WHERE id = ?").bind(file.id).execute(&app.pool).await;
            continue;
        }
        // One read answers both questions: does it still match a hash the catalogue already
        // had, and does the catalogue now also hold the one we want everything to have.
        let (outcome, read) = match super::copy::check_file(app, Path::new(&file.path), file.content_id, populate).await {
            Ok(v) => v,
            Err(e) => {
                summary.errors.push(format!("{}: {}", file.path, e.message));
                continue;
            }
        };
        summary.bytes_read += read;
        match outcome {
            CheckOutcome::Matched { algorithm, recorded } => {
                summary.checked += 1;
                summary.ok += 1;
                if recorded {
                    summary.recorded += 1;
                }
                *summary.compared_with.entry(super::copy::algorithm_name(algorithm).to_string()).or_default() += 1;
                stamp(app, file.id).await;
            }
            CheckOutcome::Mismatched { algorithm, .. } => {
                summary.checked += 1;
                summary.mismatched.push(file.path.clone());
                fail(app, file.id).await;
                *summary.compared_with.entry(super::copy::algorithm_name(algorithm).to_string()).or_default() += 1;
            }
            // No recorded hash at all: nothing to compare against, so nothing is concluded
            // and -- deliberately -- nothing is written. A hash taken from an unconfirmed
            // file would become the catalogue's idea of what that content is.
            CheckOutcome::Unverifiable => summary.unverifiable.push(file.path.clone()),
        }
    }

    stop.set();
    if let Some(w) = watcher {
        w.abort();
    }
    progress.force(done, Some(total), &one_liner(&summary)).await;
    Ok(summary)
}

/// Only a file that actually matched is stamped: a mismatch must stay at the front of the
/// queue until someone deals with it. A pass also clears an earlier failure, so the file page
/// never shows both.
async fn stamp(app: &App, file_id: u64) {
    if let Err(e) = sqlx::query("UPDATE local_files SET last_integrity_check = NOW(), last_integrity_failure = NULL WHERE id = ?").bind(file_id).execute(&app.pool).await {
        log::warn!("integrity_check: couldn't stamp file {file_id}: {e}");
    }
}

/// Records a mismatch on the file itself (the job summary only lists paths, and is gone with
/// the job). `last_integrity_check` is left alone: it keeps saying when the file last passed,
/// and the file keeps its place at the front of the queue.
async fn fail(app: &App, file_id: u64) {
    if let Err(e) = sqlx::query("UPDATE local_files SET last_integrity_failure = NOW() WHERE id = ?").bind(file_id).execute(&app.pool).await {
        log::warn!("integrity_check: couldn't record the mismatch on file {file_id}: {e}");
    }
}

pub fn one_liner(s: &IntegritySummary) -> String {
    let with = if s.compared_with.is_empty() {
        String::new()
    } else {
        let mut named: Vec<String> = s.compared_with.iter().map(|(a, n)| format!("{n} by {a}")).collect();
        named.sort();
        format!(" ({})", named.join(", "))
    };
    let mut parts = vec![format!("{} checked{with}", s.checked)];
    if !s.mismatched.is_empty() {
        parts.push(format!("{} MISMATCHED", s.mismatched.len()));
    }
    if !s.missing.is_empty() {
        parts.push(format!("{} missing", s.missing.len()));
    }
    if s.recorded > 0 {
        parts.push(format!("{} {} recorded", s.recorded, s.algorithm));
    }
    if !s.unverifiable.is_empty() {
        parts.push(format!("{} with nothing to compare against", s.unverifiable.len()));
    }
    if !s.errors.is_empty() {
        parts.push(format!("{} error(s)", s.errors.len()));
    }
    if s.cancelled {
        parts.push("cancelled".into());
    }
    parts.join(", ")
}
