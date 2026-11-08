//! Telling the server about files a client just wrote into its collection.
//!
//! An upload is the one copy direction whose destination is not this machine's to describe.
//! The bytes go through the mount, so writing them is an ordinary file copy -- but the rows
//! that say the server holds them belong in the server's own `local_*` tables, and a client
//! cannot write those: `remote_*` is a mirror, and inventing rows in it would be claiming
//! something on the server's behalf that the next sync would contradict.
//!
//! So the client asks. It maps each destination directory back to the path the server writes
//! (`mount::to_remote`), asks the server which of its configured directories that is, and
//! enqueues a **new-only** `file_scan` there. The server hashes what it now has, in its own
//! terms, and the next sync brings the answer back.
//!
//! That server-side hash is the point, and it is why the client does not hash on upload. The
//! user asked for a mandatory rehash of an upload, and a hash the *sender* computed is not
//! one: it says the bytes left correctly, not that they arrived. Only the receiving machine
//! reading its own disk can say that, and `file_scan` is exactly that read.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use romexplorer_types::jobs::{FileScanMode, FileScanRequest, JobRequest, PlatformSelect};
use sqlx::Row;

use super::mount;
use super::super::{App, Error, Result};

/// Enough of a remote source to talk to it.
struct Target {
    name: String,
    base_url: String,
    token: Option<String>,
    peer: crate::api::sources::Peer,
}

async fn load(app: &App, source_id: u16) -> Result<Target> {
    let row = sqlx::query("SELECT name, base_url, token, tls_fingerprint FROM remote_sources WHERE id = ? AND enabled = TRUE AND deleted = FALSE")
        .bind(source_id)
        .fetch_optional(&app.pool)
        .await?
        .ok_or_else(|| Error::not_found(format!("no enabled source {source_id}")))?;
    let peer = crate::api::sources::Peer::new(app, Some(source_id), &row.try_get::<String, _>("base_url")?, row.try_get("tls_fingerprint")?)?;
    Ok(Target {
        name: row.try_get("name")?,
        base_url: peer.base.clone(),
        token: crate::api::sources::open_token(app, row.try_get("token")?)?,
        peer,
    })
}

fn authed(builder: reqwest::RequestBuilder, target: &Target) -> reqwest::RequestBuilder {
    match &target.token {
        Some(token) if !token.is_empty() => builder.bearer_auth(token),
        _ => builder,
    }
}

/// What the server made of the request. Failures are per-directory and reported, never
/// fatal: the bytes are already there, and a scan that did not start is something the user
/// can trigger again from the server -- losing the upload over it would be worse.
#[derive(Debug, Default)]
pub struct ScanRequests {
    pub job_ids: Vec<u64>,
    pub errors: Vec<String>,
}

/// Asks `source_id` to scan each of `dirs` for new files.
///
/// `dirs` are paths on **this** machine, inside a mount of that source. Directories that map
/// to the same one on the server collapse into a single scan, which is the usual case: a
/// whole upload normally lands under one platform folder.
pub async fn request_scan(app: &App, source_id: u16, dirs: &BTreeSet<PathBuf>) -> Result<ScanRequests> {
    let mut out = ScanRequests::default();
    if dirs.is_empty() {
        return Ok(out);
    }
    let target = load(app, source_id).await?;
    let mounts = mount::load(app, Some(source_id)).await?;

    let mut seen: BTreeSet<u64> = BTreeSet::new();
    for dir in dirs {
        match resolve_and_scan(&target, &mounts, dir, &mut seen).await {
            Ok(Some(job_id)) => out.job_ids.push(job_id),
            Ok(None) => {}
            Err(e) => out.errors.push(format!("{}: {}", dir.display(), e.message)),
        }
    }
    Ok(out)
}

async fn resolve_and_scan(target: &Target, mounts: &[mount::Resolved], dir: &Path, seen: &mut BTreeSet<u64>) -> Result<Option<u64>> {
    let remote = mount::to_remote(mounts, dir)
        .ok_or_else(|| Error::invalid(format!("{} isn't inside a mount of {}", dir.display(), target.name)))?;

    // The server's own answer to "which of your directories is this?" -- asked rather than
    // guessed, because a client's mirror of `remote_directories` can be stale and a scan
    // pointed at the wrong directory is worse than no scan.
    let url = format!("{}/api/v1/directories/resolve", target.base_url);
    // `query` rather than a formatted string: a Windows path is full of backslashes and
    // spaces, and one of them unescaped is a 400 nobody would think to look for.
    let response = authed(target.peer.http.get(&url).query(&[("path", &remote)]), target).send().await.map_err(|e| Error::unavailable(format!("{}: {e}", target.name)))?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let hint = match status {
            401 | 403 => " -- check the token",
            404 => " -- that server doesn't serve this",
            _ => "",
        };
        return Err(Error::unavailable(format!("{} answered {status} for {remote}{hint}", target.name)));
    }
    let effective: serde_json::Value = response.json().await.map_err(|e| Error::internal(format!("{}: {e}", target.name)))?;
    let directory_id = effective
        .get("directory")
        .and_then(|d| d.get("id"))
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| Error::invalid(format!("{remote} isn't inside a collection directory on {} -- add it there first", target.name)))?;
    if !seen.insert(directory_id) {
        return Ok(None);
    }

    // New-only: the server's other files are its own business, and a full rescan of a
    // collection because a client added one game is a cost the user did not ask for.
    let request = JobRequest::FileScan(FileScanRequest { directory_id, mode: FileScanMode::NewOnly, platform: PlatformSelect::All, hash_match: None, file_id: None });
    let url = format!("{}/api/v1/jobs", target.base_url);
    let response = authed(target.peer.http.post(&url), target).json(&request).send().await.map_err(|e| Error::unavailable(format!("{}: {e}", target.name)))?;
    if !response.status().is_success() {
        return Err(Error::unavailable(format!("{} refused the scan ({})", target.name, response.status().as_u16())));
    }
    let enqueued: serde_json::Value = response.json().await.map_err(|e| Error::internal(format!("{}: {e}", target.name)))?;
    // `POST /jobs` answers `{"id": .., "job_type": ..}`.
    Ok(enqueued.get("id").and_then(serde_json::Value::as_u64))
}
