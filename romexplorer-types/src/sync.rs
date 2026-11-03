//! The peer mirror: the manifest, an export page, and what a pull produced.

use serde::{Deserialize, Serialize};

/// Which half of the sync a table belongs to.
///
/// The distinction is the whole main-vs-share design. `ownership` is what a machine *holds*
/// and is mirrored into `remote_*` under a `source_id`; `catalog` is the shared identity of
/// the things it holds -- `content`, `hashes`, `release`, `title` -- which is the same on
/// every machine and so is written into the ordinary tables, not into a mirror.
///
/// A client cannot mirror ownership without the catalogue: `remote_files.content_id` is NOT
/// NULL, so a file whose `content` row is unknown here cannot be stored at all. Hence a main
/// server offers both halves and a share offers only ownership -- and a client that syncs a
/// share relies on already having the catalogue, which it does, because content is identified
/// by hash and the same DATs produce the same rows everywhere.
pub const GROUP_OWNERSHIP: &str = "ownership";

pub const GROUP_CATALOG: &str = "catalog";

/// The main server's art (`media` and its title/release junctions), mirrored into
/// `remote_media` so a client can fetch the bytes from the server before it asks a provider
/// (`api::media_sync`). Offered only where the catalogue is, since the junctions' ids are
/// catalogue ids: a share offers neither.
pub const GROUP_MEDIA: &str = "media";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableState {
    pub table: String,
    /// `GROUP_OWNERSHIP` or `GROUP_CATALOG` -- how the client must store it.
    #[serde(default = "default_group")]
    pub group: String,
    /// Changes whenever any row does. Compared against what the client last stored, so an
    /// unchanged table costs one number rather than a download.
    pub hash: String,
    pub rows: u64,
    /// Whether the table carries change numbers, so a client that knows its position can ask
    /// for only the rows that changed since (`ExportPage` with `since`). False from a server
    /// too old to have them.
    #[serde(default)]
    pub tracked: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub generated_at: chrono::NaiveDateTime,
    /// Who is answering. Lets a client recognise its own main server, and itself.
    #[serde(default)]
    pub instance_id: String,
    /// Which installation this machine's catalogue came from, if it was synced rather than
    /// imported locally. Together with `instance_id` this is the *lineage* of the catalogue,
    /// and it is what makes an ownership mirror meaningful -- see `lineage_ok`.
    #[serde(default)]
    pub catalog_from: Option<String>,
    pub tables: Vec<TableState>,
    /// The server's sync epoch. A client's remembered position is only meaningful under the
    /// epoch it was recorded in: a new database or an emptied catalogue starts a new one.
    #[serde(default)]
    pub sync_epoch: Option<String>,
    /// Every change numbered at or below this is committed, so a client that has pulled the
    /// changes may record it as its new position. `None` when the server could not tell;
    /// the client then keeps its old position, and gets a few rows twice next time.
    #[serde(default)]
    pub stable_rev: Option<u64>,
    /// Deletes at or below this number are no longer kept. A client whose position is lower
    /// may have missed some, so it takes the table in full.
    #[serde(default)]
    pub pruned_through: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportPage {
    pub table: String,
    pub rows: Vec<serde_json::Value>,
    /// Pass back as `after` for the next page; `None` when the table is exhausted.
    pub next: Option<Vec<String>>,
}

/// One page of a table's deletes since a change number: each entry is a deleted row's
/// primary key, as text, in the order of `key`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeletedPage {
    pub table: String,
    pub key: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// Pass back as `after` for the next page; `None` when there are no more.
    pub next: Option<u64>,
}

fn default_group() -> String {
    GROUP_OWNERSHIP.to_string()
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PullSummary {
    pub source: String,
    pub role: String,
    /// Tables actually downloaded, and how many rows each brought.
    pub tables: Vec<(String, u64)>,
    /// Tables whose hash matched what we already had -- the point of the manifest.
    pub unchanged: Vec<String>,
    /// Of `tables`, those that brought only the rows changed since the last pull rather than
    /// the whole table. Their count in `tables` is changed rows plus deleted ones.
    #[serde(default)]
    pub changes_only: Vec<String>,
    /// Tables that took their changes but still hold fewer rows than the source has. Nothing
    /// is done about it automatically; a full sync is what fixes it.
    #[serde(default)]
    pub incomplete: Vec<String>,
    /// Catalogue tables the source offered that this client declined, because it is not
    /// marked as the main server.
    pub declined_catalog: Vec<String>,
    /// Mirror rows dropped because they referenced catalogue rows this machine does not
    /// have. Non-zero means the mirror is incomplete until the catalogue is synced -- which
    /// is exactly what a share cannot supply and a main server can.
    pub unresolved: u64,
    /// The installation that answered, so a screen can show which machine a mirror came from.
    pub source_instance: String,
    /// Directories whose local games were left unlinked and have had a hash match queued to
    /// re-link them (see `relink_unlinked`).
    pub relink_jobs: Vec<u64>,
    /// Art re-attached to its title or release after a catalogue replacement.
    pub media_relinked: u64,
    /// Art that could not be re-attached because it carries no `provider_id`. The files are
    /// still there; `media_download` can fetch them again.
    pub media_unresolved: u64,
    /// The `media_sync` job queued after this pull, when the main server's art changed and
    /// the `media_sync_after_pull` setting is on.
    #[serde(default)]
    pub media_sync_job: Option<u64>,
}

/// What a `media_sync` job did (`api::media_sync::run`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MediaSyncSummary {
    pub source: String,
    /// Server rows looked at.
    pub considered: u64,
    /// Rows whose every target already holds this art here (or a user's own pick for the
    /// slot, which always wins).
    pub already_held: u64,
    /// Files fetched from the server.
    pub from_server: u64,
    /// Of those, files the server had lost and fetched again (from a cache tier or the
    /// provider) at this machine's request before handing them over.
    #[serde(default)]
    pub server_refetched: u64,
    /// Files fetched from the provider's own URL, which happens only while the server is
    /// offline and `media_provider_fallback` is on.
    pub from_provider: u64,
    /// Rows attached to a file this machine already had (same bytes or the same URL, from
    /// an earlier download) without fetching anything.
    pub linked_existing: u64,
    /// Rows that could be fetched from neither place.
    pub failed: u64,
    /// Bytes downloaded, from both places.
    pub bytes: u64,
    /// Rows of a media type this machine does not keep a copy of (`media_client_keep`):
    /// they are streamed from the server when a page shows them.
    #[serde(default)]
    pub streamed_only: u64,
    /// Images stored as AVIF because that was smaller than what the server sent.
    #[serde(default)]
    pub converted: u64,
}
