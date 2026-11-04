//! Jobs: the type list, the row a screen polls, and every enqueueable request.

use chrono::NaiveDateTime;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobType {
    DatSync,
    MediaDownload,
    ProviderQuery,
    ProviderBulkSync,
    /// Payload: `{"directory_id": u64, "mode": "full"|"new_only"|"rehash", "platform":
    /// "all"|<app_platform_id>}` (see `FileScanMode`). One job type, not three --
    /// full/new-only/rehash are payload modes of the same `FileScan`, never separate
    /// `JobType` variants, since they're one capability family distinguished only by how
    /// much of the directory gets walked/rehashed.
    FileScan,
    /// Payload: `{"directory_id": u64, "to_dat": bool, "to_hasheous": bool,
    /// "to_playmatch": bool}`.
    HashMatch,
    DbOptimize,
    /// Background PlayMatch gap filler: bulk-identifies contents that have an exact match
    /// but are missing ids for usable providers (`api::playmatch_fill`). Payload
    /// `{"limit": n?}`; enqueued by the scheduler only while the network is idle.
    PlaymatchFill,
    /// Moves media the app downloaded before the storage layout existed (hashed names,
    /// absolute paths) into `<root>/<Platform>/<Title>/<Region>/` (`media_store`).
    /// Payload `{}`. Only app-written files are moved; user files are never touched.
    MediaRelayout,
    /// Refreshes the RetroAchievements account's profile and played-games progress
    /// (`providers::ra_account`). Payload `{}`.
    RaAccountSync,
    /// Asks RetroAchievements for each game's genre (one `API_GetGame` per game;
    /// `providers::ra_account::fetch_genres`). Payload `{"scope": "local"|"all", "limit": n?}`.
    RaGenreFetch,
    /// Merges titles sharing an IGDB id (`title_merge::dedupe_by_igdb`) and refreshes the
    /// merge backlog. Payload `{}`.
    TitleDedupe,
    /// Re-applies cached IGDB games to their titles (`metadata::reapply_igdb`). Payload `{}`.
    IgdbReapply,
    /// Downloads a RetroAchievements compatibility patch into the patch directory and
    /// records it (`ra_patches::download`). Payload `{"md5": "<RA hash of the patched ROM>"}`.
    RaPatchDownload,
    /// Decouples title merges and removes IGDB links whose names don't match the IGDB game
    /// (`merge_audit::audit`). Payload `{}`.
    TitleMergeAudit,
    /// Writes a backup of what this machine holds and how it is configured
    /// (`api::backup::Kind::Database`). Payload `{}`. Small, so this is the one worth
    /// scheduling nightly. With the `backup_catalog` setting on it also enqueues a
    /// `BackupCatalog` job rather than doing both itself, so each is its own history entry.
    BackupDatabase,
    /// Writes a backup of the catalogue the DAT imports and providers built
    /// (`api::backup::Kind::Catalog`). Payload `{}`. Large and re-derivable, so it is a
    /// separate job with its own schedule and its own keep-count.
    BackupCatalog,
    /// Writes a backup of the DAT-derived half of the catalogue and nothing else
    /// (`api::backup::Kind::DatSeed`). Payload `{}`. A strict subset of `BackupCatalog`, and
    /// its own job because it has its own keep-count and is the one a user moves between
    /// their own machines.
    BackupDatSeed,
    /// Pulls one remote source's ownership tables into this machine's `remote_*` mirror
    /// (`api::sync::pull`). Payload `{"source_id": u16}` -- one schedule per source, so a
    /// main server can be polled often and a peer rarely.
    CatalogSync,
    /// Fetches the art a source (the main server, unless told otherwise) holds and this
    /// machine lacks -- from the server first, from the provider's own URL when the server no
    /// longer has the file (`api::media_sync`). Payload `{"source_id": u16?}`. Queued after a
    /// catalogue sync that brought new art, and on demand from the Sources screen.
    MediaSync,
    /// Restores a backup zip into this database (`api::restore`). Payload
    /// `{"name": "<file in the backup directory>"}`.
    RestoreBackup,
    /// Builds the SQLite cache a phone reads (`api::sqlite::export`). Payload
    /// `{"kind": "database"|"catalog"|"all"}`.
    SqliteExport,
    /// Reads a SQLite cache back into this database (`api::sqlite::import`). Payload
    /// `{"name": "<file in the backup directory>"}`.
    SqliteImport,
    /// Copies selected ROMs to a folder, converting or re-archiving on the way
    /// (`api::copy::run`). Payload: `super::copy::CopyFilesRequest`. One job type for all
    /// four directions (`CopyMode`), since the direction only picks which holder to prefer,
    /// whether to skip what this machine already has, and whether to register the result.
    CopyFiles,
    /// Re-reads files this machine holds and checks one quick hash against what the
    /// catalogue records (`api::integrity`). Payload: `super::integrity::IntegrityRequest`.
    IntegrityCheck,
    /// Rebuilds the index file a dump is short of -- a `.cue`/`.gdi` whose tracks are all
    /// there, or an `.m3u` over discs that are.
    SheetCreate,
    /// Re-reads the `.cue`/`.gdi` sheets this machine holds and says whether each still works.
    SheetVerify,
    /// The same for `.m3u` playlists, which fail differently: a line that no longer resolves.
    M3uVerify,
    /// Walks the audio folders (every subfolder) and records each file with its tags.
    /// Payload: [`AudioScanRequest`].
    AudioScan,
    /// A client fetches its own copies of the main server's tagged audio, in the format its
    /// settings ask for. No payload.
    AudioSync,
    /// Downloads the media types `media_download` enables that linked releases don't have
    /// yet. Payload: [`MediaBackfillRequest`].
    MediaBackfill,
    /// Refreshes the account's unlocks and downloads every achievement badge of its games,
    /// redrawing each game's badge mosaic. Payload: [`RaBadgesRequest`].
    RaBadges,
    /// Rebuilds the name dictionary's automatic names (catalogue, series, Wikidata).
    /// Payload: [`DictionaryBuildRequest`].
    DictionaryBuild,
    /// Tags audio files with the titles their albums are named after. No payload.
    AudioAutotag,
    /// Collects how releases are meant to be shown (aspect, widescreen, video modes, refresh)
    /// from game files, PCSX2's patches and MAME. Payload: [`DisplayDataRequest`].
    DisplayData,
    /// Writes art into an emulator's own folders (RetroArch thumbnails, PCSX2 / DuckStation
    /// covers), converted to the formats it reads. Payload: [`ArtworkExportRequest`].
    ArtworkExport,
    /// Looks inside this machine's ROMs (disc images, dumped folders) for music and sound
    /// banks and records each track, so a title page can play them like a CD player.
    /// Payload: [`RomAudioIndexRequest`].
    RomAudioIndex,
}

impl JobType {
    pub const ALL: [JobType; 37] = [JobType::DatSync, JobType::MediaDownload, JobType::ProviderQuery, JobType::ProviderBulkSync, JobType::FileScan, JobType::HashMatch, JobType::DbOptimize, JobType::PlaymatchFill, JobType::MediaRelayout, JobType::RaAccountSync, JobType::RaGenreFetch, JobType::TitleDedupe, JobType::IgdbReapply, JobType::RaPatchDownload, JobType::TitleMergeAudit, JobType::BackupDatabase, JobType::BackupCatalog, JobType::BackupDatSeed, JobType::CatalogSync, JobType::MediaSync, JobType::RestoreBackup, JobType::SqliteExport, JobType::SqliteImport, JobType::CopyFiles, JobType::IntegrityCheck, JobType::SheetCreate, JobType::SheetVerify, JobType::M3uVerify, JobType::AudioScan, JobType::AudioSync, JobType::MediaBackfill, JobType::RaBadges, JobType::DictionaryBuild, JobType::AudioAutotag, JobType::DisplayData, JobType::ArtworkExport, JobType::RomAudioIndex];

    pub fn name(self) -> &'static str {
        match self {
            Self::DatSync => "dat_sync",
            Self::MediaDownload => "media_download",
            Self::ProviderQuery => "provider_query",
            Self::ProviderBulkSync => "provider_bulk_sync",
            Self::FileScan => "file_scan",
            Self::HashMatch => "hash_match",
            Self::DbOptimize => "db_optimize",
            Self::PlaymatchFill => "playmatch_fill",
            Self::MediaRelayout => "media_relayout",
            Self::RaAccountSync => "ra_account_sync",
            Self::RaGenreFetch => "ra_genre_fetch",
            Self::TitleDedupe => "title_dedupe",
            Self::IgdbReapply => "igdb_reapply",
            Self::RaPatchDownload => "ra_patch_download",
            Self::TitleMergeAudit => "title_merge_audit",
            Self::BackupDatabase => "backup_database",
            Self::BackupCatalog => "backup_catalog",
            Self::BackupDatSeed => "backup_dat_seed",
            Self::CatalogSync => "catalog_sync",
            Self::MediaSync => "media_sync",
            Self::RestoreBackup => "restore_backup",
            Self::SqliteExport => "sqlite_export",
            Self::SqliteImport => "sqlite_import",
            Self::CopyFiles => "copy_files",
            Self::IntegrityCheck => "integrity_check",
            Self::SheetCreate => "sheet_create",
            Self::SheetVerify => "sheet_verify",
            Self::M3uVerify => "m3u_verify",
            Self::AudioScan => "audio_scan",
            Self::AudioSync => "audio_sync",
            Self::MediaBackfill => "media_backfill",
            Self::RaBadges => "ra_badges",
            Self::DictionaryBuild => "dictionary_build",
            Self::AudioAutotag => "audio_autotag",
            Self::DisplayData => "display_data",
            Self::ArtworkExport => "artwork_export",
            Self::RomAudioIndex => "rom_audio_index",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.name() == name)
    }

    /// Whether this job writes to the **shared catalogue** -- the tables whose row ids must
    /// be identical across every machine that mirrors ownership.
    ///
    /// A `client` installation may not run these (`super::settings::InstanceMode`): its
    /// catalogue is its main server's, verbatim, and anything it adds locally gets an id the
    /// server has never heard of. `file_scan` and `hash_match` are absent on purpose -- they
    /// write `local_*` and read the catalogue, which is exactly what a client should do.
    pub fn writes_catalog(self) -> bool {
        matches!(
            self,
            Self::DatSync
                | Self::ProviderQuery
                | Self::ProviderBulkSync
                | Self::PlaymatchFill
                | Self::IgdbReapply
                | Self::TitleDedupe
                | Self::TitleMergeAudit
                | Self::RaGenreFetch
                // Gives titles keywords as well as tagging this machine's files.
                | Self::AudioAutotag
                // Writes `release_display`, a catalogue table a client takes from its server.
                | Self::DisplayData
        )
    }

    /// Whether a run cut short by a shutdown or a lost worker is queued again by itself.
    ///
    /// A copy is not: it is a long write to a drive the user picked, started from a dialog,
    /// and redoing it unasked on the next launch looks like a zombie. It is cancelled, and
    /// the user starts it again (retry).
    pub fn resumes_after_restart(self) -> bool {
        !matches!(self, Self::CopyFiles)
    }
}

/// `FileScan`'s `"mode"` payload field -- which of the plan's job 1/2/3 this run is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileScanMode {
    /// Soft-deletes every existing `files` row under the directory first, then
    /// re-inserts/undeletes as the walk finds them -- the initial scan of a directory, or
    /// a full redo.
    Full,
    /// Same walk as `Full`, but skips any path whose `files` row already exists and isn't
    /// soft-deleted -- only genuinely new (or revived) files get hashed.
    NewOnly,
    /// No directory walk at all: re-reads and rehashes every existing, non-deleted
    /// `files` row already under the directory.
    Rehash,
}

impl FileScanMode {
    pub const ALL: [FileScanMode; 3] = [FileScanMode::Full, FileScanMode::NewOnly, FileScanMode::Rehash];

    pub fn name(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::NewOnly => "new_only",
            Self::Rehash => "rehash",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == name)
    }
}

/// Outcome of `cancel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelOutcome {
    /// Was pending: now `cancelled`, it will never run.
    Cancelled,
    /// Is running: flagged; the worker aborts it on its next tick and marks it `cancelled`.
    Requested,
}

/// One `jobs` row as the read side sees it (list/detail views).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobRow {
    pub id: u64,
    pub job_type: JobType,
    pub status: String,
    pub payload: serde_json::Value,
    pub attempts: u16,
    pub last_error: Option<String>,
    pub claimed_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
    /// 0 = background; higher first, in the lane's fast slot.
    pub priority: u16,
    /// Waits its turn: starts only when nothing else is running or waiting, rather than
    /// alongside the other lanes ("add to queue").
    #[serde(default)]
    pub in_turn: bool,
    /// Which worker lane runs it (see `lane_of`); priority jobs get a `:fast` lane of
    /// their own so they run alongside, not behind, the lane's background job.
    pub lane: String,
    /// Live progress while it runs (see `Progress`): work done, the total when known, and
    /// what it's doing right now.
    pub progress_done: Option<u64>,
    pub progress_total: Option<u64>,
    pub progress_message: Option<String>,
    /// Live detail beyond one bar, for jobs that have it: a scan's files being read right
    /// now (`{"files": [{"name", "size", "done", "stage"}], "running"}`). Cleared when the
    /// job ends.
    #[serde(default)]
    pub progress_detail: Option<serde_json::Value>,
    /// A cancel was requested while it runs; the worker aborts it on its next tick.
    pub cancel_requested: bool,
    /// What the job produced, once it is done -- the summary struct the worker got back,
    /// as JSON (`RestoreSummary`, `PullSummary`, ...). `progress_message` is the one-line
    /// version of the same thing; this is what a screen renders in full.
    pub result: Option<serde_json::Value>,
}

/// `"all"` (conform to the directory's own configured platform) or an app platform id
/// (force that platform for every candidate).
///
/// Carries the raw id rather than the backend's `Platform` enum, so this crate stays free of
/// it. The wire format is unchanged (`"all"` | `u16`); whether the id names a platform the
/// backend knows is checked where job payloads are validated (`api::jobs::validate`), which
/// turns an unknown id into an ordinary `400 invalid` instead of the extractor's bare `422`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlatformSelect {
    #[default]
    All,
    Id(u16),
}

impl Serialize for PlatformSelect {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::All => s.serialize_str("all"),
            Self::Id(id) => s.serialize_u16(*id),
        }
    }
}

impl<'de> Deserialize<'de> for PlatformSelect {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        match &value {
            serde_json::Value::Null => Ok(Self::All),
            serde_json::Value::String(s) if s.eq_ignore_ascii_case("all") || s.is_empty() => Ok(Self::All),
            serde_json::Value::String(s) => s.parse::<u16>().map(Self::Id).map_err(|_| D::Error::custom(format!("platform must be \"all\" or a platform id, got {s:?}"))),
            serde_json::Value::Number(n) => n.as_u64().and_then(|n| u16::try_from(n).ok()).map(Self::Id).ok_or_else(|| D::Error::custom(format!("platform id out of range: {n}"))),
            other => Err(D::Error::custom(format!("platform must be \"all\" or a platform id, got {other}"))),
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileScanRequest {
    pub directory_id: u64,
    pub mode: FileScanMode,
    #[serde(default)]
    pub platform: PlatformSelect,
    /// What the hash match queued after the scan matches against: the Scan screen's DAT /
    /// Hasheous / PlayMatch boxes. Absent (a schedule, an upload, an older client): DAT only.
    /// All three off: no hash match after the scan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash_match: Option<ScanHashMatch>,
}

/// The sources a scan's follow-up hash match uses (see `FileScanRequest::hash_match`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanHashMatch {
    #[serde(default)]
    pub to_dat: bool,
    #[serde(default)]
    pub to_hasheous: bool,
    #[serde(default)]
    pub to_playmatch: bool,
}

impl Default for ScanHashMatch {
    /// DAT only: a scan is not a reason to start querying providers unless asked.
    fn default() -> Self {
        Self { to_dat: true, to_hasheous: false, to_playmatch: false }
    }
}

/// `mode`: `full` re-reads every file and marks the ones that have gone; `new_only` (the
/// quick scan) reads only files with no live row yet. `rehash` is taken as `full`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioScanRequest {
    pub mode: FileScanMode,
}

/// `mode`: `full` reads every ROM again; `new_only` only ROMs with no index yet.
/// `title_id`: only that title's ROMs (the title page's button).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RomAudioIndexRequest {
    pub mode: FileScanMode,
    #[serde(default)]
    pub title_id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HashMatchRequest {
    pub directory_id: u64,
    #[serde(default = "default_true")]
    pub to_dat: bool,
    #[serde(default = "default_true")]
    pub to_hasheous: bool,
    #[serde(default)]
    pub to_playmatch: bool,
    /// Hash match **all** instead of new: every game in the directory is matched again --
    /// its DAT link and the hash/name/fuzzy provider links this run can re-create are
    /// removed first, and cached "not found" answers are asked again. Manual and exact
    /// links stay. Off: only games with no link yet (and files not yet asked).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub all: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DatSyncRequest {
    /// A `dat_download::Source` name, or absent for every source.
    pub source: Option<String>,
    pub limit: Option<u64>,
    #[serde(default = "default_true")]
    pub import: bool,
    /// Import what is already under the DAT directory instead of downloading: a clean
    /// install restoring DAT files it was handed, without the network. Only the newest copy
    /// of each DAT is read, and only if it is not imported yet.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub offline: bool,
    /// The platforms to download and import, ticked on the form. Absent: the
    /// `dat_sync_platforms` setting decides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Vec<u16>>,
    /// The DATs without a platform to download and import (audio, video, image, document,
    /// source, theme, other), ticked on the form. Absent: the setting decides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extras: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderQueryRequest {
    pub provider: String,
    pub name: Option<String>,
    pub content_id: Option<u64>,
    pub provider_id: Option<String>,
    /// Several provider ids at once -- fetched as list requests through `api::batch`
    /// (chunked to the provider's maximum), e.g. the cross-refs a hash match collected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_ids: Option<Vec<String>>,
    /// Catalog titles to look up by name (their own name + platform) and link when the
    /// provider has a same-named entry -- the fallback when no hash source gave an id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_ids: Option<Vec<u64>>,
    pub platform: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderBulkSyncRequest {
    pub provider: String,
    /// One platform, or `"all"`: every platform the catalog/collection has rows for.
    /// Ignored with `full`.
    #[serde(default)]
    pub platform: PlatformSelect,
    pub max_pages: Option<u32>,
    /// Everything the provider has, not per platform (`api::providers::capabilities().full`:
    /// the TheGamesDB/Hasheous dumps, every RetroAchievements console).
    #[serde(default)]
    pub full: bool,
    /// `diff` (default): fetch only what's new/changed since the last complete sync;
    /// `redownload`: fetch everything again.
    #[serde(default)]
    pub mode: SyncModeName,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncModeName {
    #[default]
    Diff,
    Redownload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaDownloadRequest {
    pub provider: String,
    pub url: String,
    pub media_type: Option<String>,
    pub sub_category: Option<String>,
    pub title_id: Option<u64>,
    pub release_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub franchise_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyword_id: Option<u64>,
}

/// Every enqueueable job, tagged by `job_type`; the variant's fields are the payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "job_type", rename_all = "snake_case")]
pub enum JobRequest {
    FileScan(FileScanRequest),
    HashMatch(HashMatchRequest),
    DatSync(DatSyncRequest),
    ProviderQuery(ProviderQueryRequest),
    ProviderBulkSync(ProviderBulkSyncRequest),
    MediaDownload(MediaDownloadRequest),
    DbOptimize,
    /// Moves pre-layout app-downloaded media into `<root>/<Platform>/<Title>/<Region>/`.
    MediaRelayout,
    /// Refreshes the RetroAchievements account's profile and progress.
    RaAccountSync,
    /// Fetches RetroAchievements genres game by game.
    RaGenreFetch(RaGenreFetchRequest),
    /// Merges titles sharing an IGDB id; refreshes the merge backlog.
    TitleDedupe,
    /// Re-applies cached IGDB metadata (incl. localized names) to titles.
    IgdbReapply(IgdbReapplyRequest),
    /// Downloads a RetroAchievements compatibility patch into the patch directory.
    RaPatchDownload(RaPatchDownloadRequest),
    /// Decouples merges / removes IGDB links whose names don't match the IGDB game.
    TitleMergeAudit,
    /// Backs up what this machine holds and how it is configured.
    BackupDatabase,
    /// Backs up the catalogue the DAT imports and providers built.
    BackupCatalog,
    /// Backs up only the DAT-derived half of the catalogue.
    BackupDatSeed,
    /// Pulls one remote source into this machine's mirror.
    CatalogSync(CatalogSyncRequest),
    /// Fetches the art a source holds and this machine lacks, server first.
    MediaSync(MediaSyncRequest),
    /// Restores a backup zip from the backup directory.
    RestoreBackup(NamedFileRequest),
    /// Builds the SQLite cache a phone reads.
    SqliteExport(SqliteExportRequest),
    /// Reads a SQLite cache back into this database.
    SqliteImport(NamedFileRequest),
    /// Copies selected ROMs to a folder.
    CopyFiles(super::copy::CopyFilesRequest),
    /// Re-reads held files and checks one quick hash against the catalogue.
    IntegrityCheck(super::integrity::IntegrityRequest),
    SheetCreate(super::sheets::SheetCreateRequest),
    SheetVerify(super::sheets::SheetVerifyRequest),
    M3uVerify(super::sheets::SheetVerifyRequest),
    /// Scans the audio folders into the audio library.
    AudioScan(AudioScanRequest),
    /// Fetches this client's copies of the main server's tagged audio.
    AudioSync,
    /// Downloads enabled media types that linked releases don't have yet.
    MediaBackfill(MediaBackfillRequest),
    /// Downloads / updates RetroAchievements badges and badge mosaics.
    RaBadges(RaBadgesRequest),
    /// Rebuilds the name dictionary.
    DictionaryBuild(DictionaryBuildRequest),
    /// Tags audio files with the titles their albums are named after.
    AudioAutotag,
    /// Collects display data (aspect, widescreen, video modes, refresh).
    DisplayData(DisplayDataRequest),
    /// Writes art into an emulator's own folders.
    ArtworkExport(ArtworkExportRequest),
    /// Indexes the music and sound banks inside this machine's ROMs.
    RomAudioIndex(RomAudioIndexRequest),
}

/// Art into a folder: for the files a copy just wrote (`files`), or for what a selection
/// names (`selection`, named after this machine's files). Where it goes is `target`, or an
/// emulator's artwork setting (`emulator_id`) when there is no target.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ArtworkExportRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emulator_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<ArtworkTarget>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<ExportFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<super::copy::CopySelection>,
}

/// Where exported art goes and in what shape: any folder, in RetroArch's thumbnails layout
/// (`retroarch`) or as one cover per game named by serial (`cover_folder`) or by file name (`cover_folder_name`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ArtworkTarget {
    pub dir: String,
    pub layout: String,
    /// The formats the reader takes, in order of preference.
    #[serde(default)]
    pub formats: Vec<String>,
    #[serde(default)]
    pub max_width: Option<u32>,
}

/// A game file the emulator will open, and the release it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportFile {
    pub release_id: u64,
    pub platform: u16,
    /// The file name without its extension, as the emulator lists the game (a multi-disc
    /// game's playlist name).
    pub stem: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DisplayDataRequest {
    /// Any of `sfo` (this machine's PS3 games), `pcsx2` (PCSX2's widescreen patches), `mame`.
    #[serde(default)]
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DictionaryBuildRequest {
    /// Download Wikidata's names now (else the kept download is used, if there is one).
    #[serde(default)]
    pub wikidata: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RaBadgesRequest {
    /// played (the account's games; the default) | owned (games linked to a release held here)
    #[serde(default = "default_badges_scope")]
    pub scope: String,
}

fn default_badges_scope() -> String {
    "played".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MediaBackfillRequest {
    /// owned (releases with a local game; the default) | all (the whole catalogue)
    #[serde(default = "default_backfill_scope")]
    pub scope: String,
    /// Only this platform's releases.
    #[serde(default)]
    pub platform: Option<u16>,
    /// "Download art" on a selection: these releases (through sets, titles, games...) in
    /// place of `scope`. Every release of a selected title, not only its top-ranked one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<super::copy::CopySelection>,
    /// Only these providers (`Provider::name`). Empty: every provider whose art downloads on
    /// its own -- never one whose art is fetched only by hand (TheGamesDB).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<String>,
    /// Only these slots, `type` or `type:sub` (`box:front`, `title_screen`, `screenshot`,
    /// `logo`), fetched whatever `media_download` says -- they were asked for. Empty: what
    /// `media_download` turns on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<String>,
}

impl MediaBackfillRequest {
    /// Asked for by hand, for chosen types: the run that may use a manual-only provider.
    pub fn is_asked(&self) -> bool {
        !self.types.is_empty()
    }
}

fn default_backfill_scope() -> String {
    "owned".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogSyncRequest {
    /// A `remote_sources` row. One job (and so one schedule) per source.
    pub source_id: u16,
    /// Download every table again instead of only what changed: the on-demand repair for a
    /// mirror that has gone wrong. Never chosen automatically.
    #[serde(default)]
    pub full: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MediaSyncRequest {
    /// A `remote_sources` row; `None` means the main server.
    #[serde(default)]
    pub source_id: Option<u16>,
}

/// A file in the backup directory, by name -- shared by restore and SQLite import.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedFileRequest {
    pub name: String,
    /// SQLite import only: also take the `local_*` ownership tables when the file came from
    /// another installation. Off by default -- see `api::sqlite::import`. A phone building
    /// itself a view of the collection sets it; a desktop should mirror a peer through
    /// `catalog_sync` instead, which keeps the rows in `remote_*` where they belong.
    #[serde(default)]
    pub adopt_ownership: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqliteExportRequest {
    /// database | catalog | all (the default -- a phone needs both halves to render).
    #[serde(default = "default_cache_kind")]
    pub kind: String,
}

fn default_cache_kind() -> String {
    "all".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RaPatchDownloadRequest {
    /// RetroAchievements' hash of the patched ROM (its `ra_game_hashes` row has the URL).
    pub md5: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IgdbReapplyRequest {
    /// These titles, whether or not they have IGDB names already. None: every IGDB-linked
    /// title that has none yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_ids: Option<Vec<u64>>,
    /// Every IGDB-linked title, names or not (without `title_ids`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub all: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RaGenreFetchRequest {
    /// local (games linked to local games; the default) | all (RetroAchievements' whole list)
    #[serde(default = "default_genre_scope")]
    pub scope: String,
    /// At most this many games in one run (none = all).
    #[serde(default)]
    pub limit: Option<u64>,
}

fn default_genre_scope() -> String {
    "local".to_string()
}

impl JobRequest {
    pub fn job_type(&self) -> JobType {
        match self {
            Self::FileScan(_) => JobType::FileScan,
            Self::HashMatch(_) => JobType::HashMatch,
            Self::DatSync(_) => JobType::DatSync,
            Self::ProviderQuery(_) => JobType::ProviderQuery,
            Self::ProviderBulkSync(_) => JobType::ProviderBulkSync,
            Self::MediaDownload(_) => JobType::MediaDownload,
            Self::DbOptimize => JobType::DbOptimize,
            Self::MediaRelayout => JobType::MediaRelayout,
            Self::RaAccountSync => JobType::RaAccountSync,
            Self::RaGenreFetch(_) => JobType::RaGenreFetch,
            Self::TitleDedupe => JobType::TitleDedupe,
            Self::IgdbReapply(_) => JobType::IgdbReapply,
            Self::RaPatchDownload(_) => JobType::RaPatchDownload,
            Self::TitleMergeAudit => JobType::TitleMergeAudit,
            Self::BackupDatabase => JobType::BackupDatabase,
            Self::BackupCatalog => JobType::BackupCatalog,
            Self::BackupDatSeed => JobType::BackupDatSeed,
            Self::CatalogSync(_) => JobType::CatalogSync,
            Self::MediaSync(_) => JobType::MediaSync,
            Self::RestoreBackup(_) => JobType::RestoreBackup,
            Self::SqliteExport(_) => JobType::SqliteExport,
            Self::SqliteImport(_) => JobType::SqliteImport,
            Self::CopyFiles(_) => JobType::CopyFiles,
            Self::IntegrityCheck(_) => JobType::IntegrityCheck,
            Self::SheetCreate(_) => JobType::SheetCreate,
            Self::SheetVerify(_) => JobType::SheetVerify,
            Self::M3uVerify(_) => JobType::M3uVerify,
            Self::AudioScan(_) => JobType::AudioScan,
            Self::AudioSync => JobType::AudioSync,
            Self::MediaBackfill(_) => JobType::MediaBackfill,
            Self::RaBadges(_) => JobType::RaBadges,
            Self::DictionaryBuild(_) => JobType::DictionaryBuild,
            Self::AudioAutotag => JobType::AudioAutotag,
            Self::DisplayData(_) => JobType::DisplayData,
            Self::ArtworkExport(_) => JobType::ArtworkExport,
            Self::RomAudioIndex(_) => JobType::RomAudioIndex,
        }
    }

    /// The stored payload: the variant's fields without the `job_type` tag.
    pub fn payload(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        if let Some(map) = value.as_object_mut() {
            map.remove("job_type");
        }
        value
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobsPage {
    pub jobs: Vec<JobRow>,
    pub total: u64,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusCount {
    pub status: String,
    pub count: u64,
}

/// What the routes that enqueue a job answer with (`POST /api/v1/backup`, `.../restore`,
/// `/api/v1/cache`, `/api/v1/sources/{id}/sync`): the id to poll at `GET /api/v1/jobs/{id}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobEnqueued {
    pub job_id: u64,
}
