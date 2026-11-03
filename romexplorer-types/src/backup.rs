//! Backups: the two kinds and what a backup file and a backup run look like.

use serde::{Deserialize, Serialize};

/// Which backup. Each writes its own file and prunes its own history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// What this machine holds and how it is configured. Small; run it often.
    Database,
    /// What a DAT import and the providers built. Large; run it rarely.
    Catalog,
    /// The DAT-derived half of the catalogue and nothing else: no provider caches, no
    /// provider id links, no provider vocabularies. A strict subset of [`Kind::Catalog`].
    ///
    /// It exists because the catalogue mixes two things with very different terms attached.
    /// What a DAT import built is hashes and names; what the providers added is other
    /// people's databases, held under API terms that restrict passing them on. Splitting the
    /// export along that line is what makes a catalogue movable -- between a user's own
    /// machines today, and, if the DAT projects ever agree to it, as a published seed.
    DatSeed,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Database => "database",
            Kind::Catalog => "catalog",
            Kind::DatSeed => "dat-seed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "database" => Some(Kind::Database),
            "catalog" => Some(Kind::Catalog),
            "dat-seed" => Some(Kind::DatSeed),
            _ => None,
        }
    }

    /// `romexplorer-database-` / `romexplorer-catalog-` / `romexplorer-dat-seed-`. Also what
    /// tells them apart on disk, so each kind's keep-count only ever prunes its own files.
    pub fn prefix(self) -> &'static str {
        match self {
            Kind::Database => "romexplorer-database-",
            Kind::Catalog => "romexplorer-catalog-",
            Kind::DatSeed => "romexplorer-dat-seed-",
        }
    }

    /// Whether this kind replaces the catalogue on restore. [`Kind::DatSeed`] writes into the
    /// same tables [`Kind::Catalog`] does, so a restore of either has to be treated the same
    /// way -- the seed is a subset, not a different shape.
    pub fn is_catalog(self) -> bool {
        matches!(self, Kind::Catalog | Kind::DatSeed)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackupFile {
    pub name: String,
    pub kind: String,
    pub bytes: u64,
    pub created: chrono::NaiveDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackupSummary {
    pub file: BackupFile,
    /// Rows written per table, in the order they were written.
    pub tables: Vec<(String, u64)>,
}
