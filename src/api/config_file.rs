//! Configuration as one file: what the Config screens hold, written out and read back.
//!
//! A backup already carries all of this, but as part of a whole database (`api::backup`):
//! restoring it brings the files this machine held along with it. This is only the choices
//! -- settings, scheduled jobs, emulators, copy folder names and formats, collection
//! directories, media and temp folders -- so they can be moved to a new install, kept
//! beside a reinstall, or handed to another machine.
//!
//! **Plain JSON, on purpose.** It is a few kilobytes and meant to be read and edited by
//! hand; compressing it would save nothing a person would notice and cost the one thing the
//! file is for. (Over REST the response is compressed like any other, `server::compression`.)
//!
//! **An import merges, and never removes.** Each item goes through the same function its
//! own screen saves with, so a value that screen would refuse is refused here too -- that
//! item is reported as skipped and the rest go on. Items are matched by what a person would
//! call them (a setting by key, a schedule or emulator by name, a directory by path), never
//! by row id, which means nothing in another install.
//!
//! **Not in the file:** provider credentials (`assets/api.env`, `api::secrets`), accounts and
//! peers' tokens, and the three identity keys `api::settings` guards -- `instance_id` is this
//! installation, and `instance_mode` / `catalog_from` can mean replacing the catalogue, which
//! has its own endpoint and its own consent.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::database::directories::{same_path, DirectorySpec};
use crate::database::emulators::Emulator;
use crate::database::schedules::ScheduleSpec;
use crate::database::settings as db_settings;
use crate::database::storage as db_storage;

use super::naming::{NamingUpdate, OutputFormat};
use super::{App, Error, Result};

pub const FORMAT: &str = "romexplorer-config";
/// Raised only when a file an older version wrote could no longer be read as it is.
pub const VERSION: u32 = 1;

/// The parts of the file, in the order they are applied: directories before anything that
/// may name a path under one.
pub const SECTIONS: [&str; 6] = ["settings", "directories", "storage", "emulators", "output", "schedules"];

/// Never written and never read: see the module comment.
const IDENTITY_KEYS: [&str; 3] = [db_settings::KEY_INSTANCE_ID, db_settings::KEY_INSTANCE_MODE, db_settings::KEY_CATALOG_FROM];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MediaDirEntry {
    pub role: String,
    pub path: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TempDirEntry {
    pub tier: u8,
    pub path: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Storage {
    #[serde(default)]
    pub media_dirs: Vec<MediaDirEntry>,
    #[serde(default)]
    pub temp_dirs: Vec<TempDirEntry>,
}

/// A platform's own folder name and output format on a copy.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Output {
    /// `app_platform_id` -> folder name.
    #[serde(default)]
    pub folders: BTreeMap<u16, String>,
    /// `app_platform_id` -> output format.
    #[serde(default)]
    pub formats: BTreeMap<u16, OutputFormat>,
}

/// Every section is optional on the way in: a file holding only `settings` is a valid one.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigFile {
    pub format: String,
    pub version: u32,
    #[serde(default)]
    pub app_version: String,
    #[serde(default)]
    pub exported_at: String,
    /// Stored values only: a key left at its default is not in the file, so importing it
    /// does not pin another install to today's defaults.
    #[serde(default)]
    pub settings: BTreeMap<String, Value>,
    #[serde(default)]
    pub directories: Vec<DirectorySpec>,
    #[serde(default)]
    pub storage: Storage,
    #[serde(default)]
    pub emulators: Vec<Emulator>,
    #[serde(default)]
    pub output: Output,
    #[serde(default)]
    pub schedules: Vec<ScheduleSpec>,
}

impl ConfigFile {
    /// How many items a section holds, for the dialog that asks which to bring in.
    pub fn count(&self, section: &str) -> usize {
        match section {
            "settings" => self.settings.len(),
            "directories" => self.directories.len(),
            "storage" => self.storage.media_dirs.len() + self.storage.temp_dirs.len(),
            "emulators" => self.emulators.len(),
            "output" => self.output.folders.len() + self.output.formats.len(),
            "schedules" => self.schedules.len(),
            _ => 0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImportRequest {
    pub config: ConfigFile,
    /// The sections to bring in; absent = every one the file has.
    #[serde(default)]
    pub sections: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Skipped {
    pub item: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SectionSummary {
    pub section: String,
    /// Added or changed.
    pub applied: u64,
    /// Already as the file has it.
    pub unchanged: u64,
    pub skipped: Vec<Skipped>,
}

impl SectionSummary {
    fn new(section: &str) -> Self {
        Self { section: section.to_string(), ..Default::default() }
    }

    /// One item's outcome: `Ok(true)` changed something, `Ok(false)` found it already so,
    /// and an error is that item's alone -- the import goes on.
    fn note(&mut self, item: impl Into<String>, outcome: Result<bool>) {
        match outcome {
            Ok(true) => self.applied += 1,
            Ok(false) => self.unchanged += 1,
            Err(e) => self.skipped.push(Skipped { item: item.into(), reason: e.message }),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ImportSummary {
    pub sections: Vec<SectionSummary>,
}

pub async fn export(app: &App) -> Result<ConfigFile> {
    let settings = db_settings::list_all(&app.pool).await?.into_iter().filter(|(key, _, _)| !IDENTITY_KEYS.contains(&key.as_str())).map(|(key, value, _)| (key, value)).collect();
    let directories = crate::database::directories::list(&app.pool, false)
        .await?
        .into_iter()
        .map(|d| DirectorySpec { path: d.path, is_collection_root: d.is_collection_root, dir_type: d.dir_type, app_platform_id: d.app_platform_id, is_archival: d.is_archival })
        .collect();
    let storage = Storage {
        media_dirs: db_storage::list_media_dirs(&app.pool).await?.into_iter().map(|d| MediaDirEntry { role: d.role, path: d.path }).collect(),
        temp_dirs: db_storage::list_temp_dirs(&app.pool).await?.into_iter().map(|d| TempDirEntry { tier: d.tier, path: d.path }).collect(),
    };
    // Row ids mean nothing elsewhere: an import matches by name.
    let emulators = super::launch::list(app).await?.into_iter().map(|e| Emulator { id: 0, ..e }).collect();
    let output = Output { folders: super::naming::folder_overrides(app).await?.into_iter().collect(), formats: super::naming::format_overrides(app).await?.into_iter().collect() };
    let schedules = super::schedules::list(app).await?.into_iter().map(|s| s.spec).collect();
    Ok(ConfigFile {
        format: FORMAT.to_string(),
        version: VERSION,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        exported_at: chrono::Local::now().naive_local().format("%Y-%m-%d %H:%M:%S").to_string(),
        settings,
        directories,
        storage,
        emulators,
        output,
        schedules,
    })
}

/// Refuses a file that is not one of these, or is from a version this one cannot read.
fn check(file: &ConfigFile) -> Result<()> {
    if file.format != FORMAT {
        return Err(Error::invalid(format!("not a romexplorer configuration file (format is {:?}, expected {FORMAT:?})", file.format)));
    }
    if file.version == 0 || file.version > VERSION {
        return Err(Error::invalid(format!("this configuration file is version {}, and this romexplorer reads up to version {VERSION} -- update romexplorer to import it", file.version)));
    }
    Ok(())
}

pub async fn import(app: &App, request: &ImportRequest) -> Result<ImportSummary> {
    let file = &request.config;
    check(file)?;
    if let Some(unknown) = request.sections.iter().flatten().find(|s| !SECTIONS.contains(&s.as_str())) {
        return Err(Error::invalid(format!("unknown section {unknown:?}; the sections are {}", SECTIONS.join(", "))));
    }
    let wanted = |section: &str| request.sections.as_ref().is_none_or(|s| s.iter().any(|x| x == section)) && file.count(section) > 0;
    let mut summary = ImportSummary::default();
    for section in SECTIONS {
        if !wanted(section) {
            continue;
        }
        let mut s = SectionSummary::new(section);
        match section {
            "settings" => import_settings(app, file, &mut s).await?,
            "directories" => import_directories(app, file, &mut s).await?,
            "storage" => import_storage(app, file, &mut s).await?,
            "emulators" => import_emulators(app, file, &mut s).await?,
            "output" => import_output(app, file, &mut s).await?,
            _ => import_schedules(app, file, &mut s).await?,
        }
        summary.sections.push(s);
    }
    Ok(summary)
}

async fn import_settings(app: &App, file: &ConfigFile, s: &mut SectionSummary) -> Result<()> {
    let stored: BTreeMap<String, Value> = db_settings::list_all(&app.pool).await?.into_iter().map(|(key, value, _)| (key, value)).collect();
    for (key, value) in &file.settings {
        let outcome = if stored.get(key) == Some(value) {
            Ok(false)
        } else {
            // `set` validates, and refuses the identity keys with the reason why.
            super::settings::set(app, key, value.clone()).await.map(|()| true)
        };
        s.note(key.clone(), outcome);
    }
    Ok(())
}

async fn import_directories(app: &App, file: &ConfigFile, s: &mut SectionSummary) -> Result<()> {
    // Shortest path first, so a collection root is there before what lies under it.
    let mut specs: Vec<&DirectorySpec> = file.directories.iter().collect();
    specs.sort_by_key(|d| d.path.len());
    for spec in specs {
        let existing = crate::database::directories::list(&app.pool, false).await?.into_iter().find(|d| same_path(&d.path, &spec.path));
        let same = existing.is_some_and(|d| d.is_collection_root == spec.is_collection_root && d.dir_type == spec.dir_type.clone().filter(|t| !t.is_empty()) && d.app_platform_id == spec.app_platform_id && d.is_archival == spec.is_archival);
        let outcome = if same { Ok(false) } else { super::directories::create(app, spec).await.map(|_| true) };
        s.note(spec.path.clone(), outcome);
    }
    Ok(())
}

async fn import_storage(app: &App, file: &ConfigFile, s: &mut SectionSummary) -> Result<()> {
    for dir in &file.storage.media_dirs {
        let held = db_storage::list_media_dirs(&app.pool).await?.iter().any(|d| d.role == dir.role && same_path(&d.path, &dir.path));
        let outcome = if held { Ok(false) } else { super::storage::add_media_dir(app, &dir.role, &dir.path).await.map(|_| true) };
        s.note(format!("{}: {}", dir.role, dir.path), outcome);
    }
    for dir in &file.storage.temp_dirs {
        let held = db_storage::list_temp_dirs(&app.pool).await?.iter().any(|d| d.tier == dir.tier && same_path(&d.path, &dir.path));
        let outcome = if held { Ok(false) } else { super::storage::set_temp_dir(app, dir.tier, &dir.path).await.map(|_| true) };
        s.note(format!("temp tier {}: {}", dir.tier, dir.path), outcome);
    }
    Ok(())
}

/// The same emulator apart from its row id and the order its platforms are listed in.
fn same_emulator(a: &Emulator, b: &Emulator) -> bool {
    let platforms = |e: &Emulator| {
        let mut p: Vec<(u16, Option<String>, bool)> = e.platforms.iter().map(|p| (p.app_platform_id, p.core.clone().filter(|c| !c.trim().is_empty()), p.is_default)).collect();
        p.sort();
        p
    };
    a.name == b.name && a.preset == b.preset && a.kind == b.kind && a.path == b.path && a.args == b.args && a.cores_dir == b.cores_dir && a.working_dir == b.working_dir && a.supports_archives == b.supports_archives && a.supports_playlists == b.supports_playlists && a.enabled == b.enabled && platforms(a) == platforms(b) && a.artwork == b.artwork
}

async fn import_emulators(app: &App, file: &ConfigFile, s: &mut SectionSummary) -> Result<()> {
    for emulator in &file.emulators {
        let existing = super::launch::list(app).await?.into_iter().find(|e| e.name == emulator.name);
        let outcome = match existing {
            Some(e) if same_emulator(&e, emulator) => Ok(false),
            existing => super::launch::save(app, &Emulator { id: existing.map_or(0, |e| e.id), ..emulator.clone() }).await.map(|_| true),
        };
        s.note(emulator.name.clone(), outcome);
    }
    Ok(())
}

async fn import_output(app: &App, file: &ConfigFile, s: &mut SectionSummary) -> Result<()> {
    let platform_name = |id: u16| crate::common::models::Platform::from_igdb_id(id).map_or_else(|| format!("platform {id}"), |p| p.display_name().to_string());
    let folders = super::naming::folder_overrides(app).await?;
    for (id, name) in &file.output.folders {
        // One at a time: `naming::update` stops at the first name it refuses.
        let outcome = if folders.get(id) == Some(name) { Ok(false) } else { super::naming::update(app, &NamingUpdate { folders: vec![(*id, name.clone())], ..Default::default() }).await.map(|_| true) };
        s.note(format!("{} folder: {name}", platform_name(*id)), outcome);
    }
    let formats = super::naming::format_overrides(app).await?;
    for (id, format) in &file.output.formats {
        let outcome = if formats.get(id) == Some(format) { Ok(false) } else { super::naming::update(app, &NamingUpdate { formats: vec![(*id, *format)], ..Default::default() }).await.map(|_| true) };
        s.note(format!("{} format: {}", platform_name(*id), format.name()), outcome);
    }
    Ok(())
}

async fn import_schedules(app: &App, file: &ConfigFile, s: &mut SectionSummary) -> Result<()> {
    for spec in &file.schedules {
        let existing = super::schedules::list(app).await?.into_iter().find(|e| e.spec.name == spec.name);
        let same = existing.as_ref().is_some_and(|e| serde_json::to_value(&e.spec).ok() == serde_json::to_value(spec).ok());
        let outcome = match existing {
            _ if same => Ok(false),
            Some(e) => super::schedules::update(app, e.id, spec).await.map(|_| true),
            None => super::schedules::create(app, spec).await.map(|_| true),
        };
        s.note(spec.name.clone(), outcome);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(json: Value) -> ConfigFile {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn a_file_with_one_section_is_a_whole_file() {
        let f = file(serde_json::json!({ "format": FORMAT, "version": 1, "settings": { "cache_ttl_days": 30 } }));
        assert!(check(&f).is_ok());
        assert_eq!(SECTIONS.iter().map(|s| f.count(s)).collect::<Vec<_>>(), vec![1, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn another_kind_of_file_and_a_newer_version_are_refused() {
        assert!(check(&file(serde_json::json!({ "format": "something-else", "version": 1 }))).is_err());
        let newer = check(&file(serde_json::json!({ "format": FORMAT, "version": VERSION + 1 }))).unwrap_err();
        assert!(newer.message.contains("update romexplorer"), "{}", newer.message);
    }

    #[test]
    fn what_is_written_reads_back_the_same() {
        let written = ConfigFile {
            format: FORMAT.to_string(),
            version: VERSION,
            output: Output { folders: BTreeMap::from([(19, "SFC".to_string())]), formats: BTreeMap::new() },
            storage: Storage { media_dirs: vec![MediaDirEntry { role: "image".to_string(), path: "/srv/art".to_string() }], temp_dirs: vec![TempDirEntry { tier: 1, path: "/tmp/rx".to_string() }] },
            ..Default::default()
        };
        let read: ConfigFile = serde_json::from_str(&serde_json::to_string_pretty(&written).unwrap()).unwrap();
        assert_eq!((read.output, read.storage, read.version), (written.output, written.storage, written.version));
    }

    /// The identity keys are the ones `api::settings` refuses: a file carrying one (edited
    /// in by hand) has that key skipped with the reason, rather than the import failing.
    #[test]
    fn the_identity_keys_are_the_ones_settings_refuses() {
        for key in IDENTITY_KEYS {
            assert!(super::super::settings::validate(key, &Value::String("x".to_string())).is_err(), "{key} is settable");
        }
    }

    #[test]
    fn an_emulator_is_the_same_whatever_its_id_and_platform_order() {
        let emulator = |id: u64, platforms: &[u16]| -> Emulator {
            serde_json::from_value(serde_json::json!({ "id": id, "name": "snes9x", "kind": "standalone", "path": "/usr/bin/snes9x", "platforms": platforms.iter().map(|p| serde_json::json!({ "app_platform_id": p })).collect::<Vec<_>>() })).unwrap()
        };
        assert!(same_emulator(&emulator(4, &[19, 58]), &emulator(0, &[58, 19])));
        assert!(!same_emulator(&emulator(4, &[19]), &emulator(0, &[19, 58])));
    }
}
