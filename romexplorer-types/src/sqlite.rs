//! The SQLite cache a phone reads: export, import, and the files on disk.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SqliteSummary {
    pub file: String,
    pub bytes: u64,
    pub tables: Vec<(String, u64)>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ImportSummary {
    pub tables: Vec<(String, u64)>,
    /// Tables in the file that this build will not import, and why it is not an error: a
    /// cache may legitimately carry more than the importer is willing to write.
    pub skipped: Vec<String>,
    /// The installation that produced the file, if it says.
    pub from_instance: Option<String>,
    /// Whether that was this machine.
    pub from_self: bool,
    /// Ownership tables that were present and deliberately not imported (see
    /// `import`'s note on adopting another machine's ownership).
    pub declined_ownership: Vec<String>,
}

/// One cache file on disk, the shape `backup::BackupFile` has for a backup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheFile {
    pub name: String,
    /// `database` | `catalog` | `all`, read back out of the name.
    pub kind: String,
    pub bytes: u64,
    pub created: chrono::NaiveDateTime,
}
