//! What restoring a backup produced.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RestoreSummary {
    pub file: String,
    /// `database` | `catalog`, as the backup's own manifest states it.
    pub kind: String,
    pub schema_version: i64,
    pub tables: Vec<(String, u64)>,
    /// Named in the backup but not written: a table this build no longer has, or one the
    /// backup carried that is not on the allowed list. Reported rather than failed -- a
    /// backup from a different version is exactly when a restore matters most.
    pub skipped: Vec<String>,
}
