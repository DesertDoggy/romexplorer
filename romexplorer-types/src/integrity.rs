//! Checking that the files this machine holds still are what the catalogue says they are.
//!
//! A cheap, repeatable counterpart to a rehash scan: one quick algorithm (the same one a
//! verified copy uses, configured separately so the two can differ), compared against the
//! hash already recorded for that content. It answers "has anything rotted, been truncated
//! or been replaced?" without re-deriving the full hash set, and stamps
//! `local_files.last_integrity_check` so the next run can start with what it looked at
//! longest ago.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrityRequest {
    /// One configured directory, or every one of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory_id: Option<u64>,
    /// At most this many files in one run (0 = no limit). The point of a limit is that this
    /// is meant to run often and briefly rather than seldom and for hours.
    #[serde(default)]
    pub limit: Option<u64>,
    /// Check files whose last check is older than this many days, oldest first. `None`
    /// checks everything in range, still oldest first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub older_than_days: Option<u64>,
    /// Override the configured *populate* algorithm for this run. What each file is compared
    /// against is never a choice: it is whichever hash the catalogue already holds for that
    /// content and is cheapest here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub algorithm: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IntegritySummary {
    /// The algorithm this run filled in, not the one it compared with.
    pub algorithm: String,
    /// Which algorithm each file was actually compared against, and how many used it.
    #[serde(default)]
    pub compared_with: std::collections::BTreeMap<String, u64>,
    pub checked: u64,
    pub ok: u64,
    /// Files whose bytes no longer match the recorded hash: the answer this job exists for.
    pub mismatched: Vec<String>,
    /// Files the catalogue has, and the disk does not. Soft-deleted, as a scan would.
    pub missing: Vec<String>,
    /// Files whose content had no hash of the populate algorithm recorded, and now does --
    /// filled in off the same read that confirmed them. The first pass over an old
    /// collection is largely this.
    pub recorded: u64,
    /// Files whose content records no hash at all, so there was nothing to compare against.
    /// Nothing was concluded and nothing was written.
    pub unverifiable: Vec<String>,
    pub bytes_read: u64,
    pub cancelled: bool,
    pub errors: Vec<String>,
}
