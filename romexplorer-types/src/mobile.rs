//! The phone's synced copy (`romexplorer::api::mobile`): what the phone asks for, and what the
//! machine it syncs from tells it before and after.
//!
//! A phone cannot run MariaDB, so the mobile app reads a SQLite file the server or a client
//! builds for it. The whole catalogue can be hundreds of thousands of titles, so the phone
//! says which machines count as "held", how far past them to reach, and which tabs it shows;
//! the file holds only that.

use serde::{Deserialize, Serialize};

/// The file format version. A phone refuses a file with another one and syncs again.
/// 2: RA rows say which synced machines hold them (`_mobile_ra.held`), for the Machine filter.
pub const FORMAT: u32 = 2;

/// The machine the phone syncs from, by this name in `machines`.
pub const THIS_MACHINE: &str = "local";

/// What goes into a phone's synced copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncOptions {
    /// Whose holdings count: [`THIS_MACHINE`] and/or remote source ids (the main server, the
    /// shared clients), as `GET /api/v1/mobile/machines` lists them.
    pub machines: Vec<String>,
    /// `held`: only what the chosen machines hold. `catalog`: also the catalogue's titles
    /// that nobody holds, on `catalog_platforms` only.
    pub scope: String,
    /// Platforms (app platform ids) whose whole catalogue comes along with `scope = catalog`.
    /// Empty means none: the whole catalogue is never taken by default.
    pub catalog_platforms: Vec<u16>,
    /// Machines (of `machines`) whose platforms bring the catalogue's titles along: every
    /// title, owned or not, on each platform that machine holds games for. The phone sets it
    /// per machine ("local and catalogue titles"), so the catalogue is bounded by what each
    /// machine is for rather than a platform list kept by hand.
    pub catalog_machines: Vec<String>,
    pub tabs: SyncTabs,
    /// Full title / release pages for the held items. A catalogue-only item's page is loaded
    /// live when the phone is connected.
    pub pages: bool,
    /// Cover thumbnails, made on the machine syncing (never on the phone).
    pub covers: bool,
    /// `avif` (smallest) or `jpeg` (oldest phones).
    pub cover_format: String,
    /// Longest edge, in pixels.
    pub cover_px: u32,
    /// Original audio and ROM music: not synced to a phone unless asked for. Reserved.
    pub audio: bool,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            machines: vec![THIS_MACHINE.to_string()],
            scope: "held".to_string(),
            catalog_platforms: Vec::new(),
            catalog_machines: Vec::new(),
            tabs: SyncTabs::default(),
            pages: true,
            covers: true,
            cover_format: "avif".to_string(),
            cover_px: 512,
            audio: false,
        }
    }
}

/// Which lists the phone shows, and so which go into the file. Games / ROMs / Files /
/// Contents are off by default on a phone (they clutter it); a handheld with emulators turns
/// them on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncTabs {
    pub crossplatform: bool,
    pub definitive: bool,
    pub titles: bool,
    pub releases: bool,
    pub ra: bool,
    pub games: bool,
    pub files: bool,
    pub contents: bool,
}

impl Default for SyncTabs {
    fn default() -> Self {
        Self {
            crossplatform: true,
            definitive: true,
            titles: true,
            releases: true,
            ra: true,
            games: false,
            files: false,
            contents: false,
        }
    }
}

/// One machine a phone can count holdings on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Machine {
    /// [`THIS_MACHINE`] or a remote source id.
    pub id: String,
    pub name: String,
    /// `this`, `main` or `peer`.
    pub role: String,
    /// The main server: the `main` source, or `this` machine when it has none (it is the
    /// main server itself).
    #[serde(default)]
    pub is_main: bool,
    /// Games it holds, as this machine last saw them.
    pub games: u64,
}

/// What a sync with these options would carry, before it runs.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SyncEstimate {
    pub titles: u64,
    pub releases: u64,
    pub pages: u64,
    pub covers: u64,
    pub ra_games: u64,
    pub games: u64,
    pub files: u64,
    /// Rough size of the file, from per-row averages.
    pub bytes: u64,
}

/// What a finished sync built: the job's result.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SyncSummary {
    /// The file to download (`GET /api/v1/cache/{file}`).
    pub file: String,
    pub bytes: u64,
    /// Rows per list.
    pub rows: Vec<(String, u64)>,
    pub pages: u64,
    pub covers: u64,
}
