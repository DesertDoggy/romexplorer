//! Backups: the database, the catalogue, or both -- each as one zip.
//!
//! The same row-projection the peer export uses ([`super::sync`]), over a wider set of tables
//! and written to a file instead of a response body.
//!
//! **Two kinds, deliberately separate files.** What this machine *chose or observed* -- which
//! files it holds, how they are grouped, its settings, emulators, media rows and collections
//! -- is small and irreplaceable, so it is worth backing up often. The catalogue -- `content`,
//! `hashes`, `release`, `title` and the provider caches -- is a hundred times larger and is
//! re-derivable: re-importing the same DATs reproduces it, because content is identified by
//! hash rather than by row id. So they are two jobs, two keep-counts and two files, and either
//! restores without the other.
//!
//! The catch worth knowing, and the reason to keep a catalogue backup anyway: a rebuilt
//! catalogue has *different row ids*, so `local_release_games.release_id` and
//! `collection_set_releases.release_id` in a database backup no longer point at the same rows.
//! Restoring the database alone means re-running `hash_match` to re-link by content hash --
//! correct, but slow, and manual collection membership cannot be recovered that way at all.
//! Restoring both preserves every id exactly. That, not the download, is the real argument for
//! turning `backup_catalog` on.
//!
//! **Media files are never included** in either. Backing up a collection's art is a separate,
//! much larger job that belongs with backing up the ROMs themselves; only the rows that say
//! where the art lives are here.

use std::io::Write;
use std::path::{Path, PathBuf};


use super::{App, Error, Result};

// Shared with the GUI through `romexplorer_types`.
pub use romexplorer_types::backup::{BackupFile, BackupSummary, Kind};

/// Tables in neither list, and why. Asserted exhaustive by a test, so a table added by a
/// later migration has to be classified rather than silently dropped from both backups.
#[cfg(test)]
const NOT_BACKED_UP: [&str; 25] = [
    // the work table of one pass (`release_merge::fold_patched_works`), empty between passes
    "release_merge",
    // where the last title merge pass started and its work table: a restored catalogue's
    // first pass is a full one (`title_merge::collect_dirty`)
    "pass_marks",
    "title_merge_dirty",
    // an index of the Hasheous dump zips on disk, rebuilt from them when it is missing
    // (`dumps::ensure_hasheous_index`); the zips are not in a backup either
    "hasheous_dump_games",
    "hasheous_dump_roms",
    // runtime state, meaningless once restored elsewhere
    "jobs",
    "provider_quota",
    "data_migrations",
    // a cache of files that are not in the backup either
    "media_cache",
    // scratch, written and emptied inside one catalogue replacement
    "media_relink",
    // someone else's data, re-fetched by a sync (`remote_sources` itself *is* backed up)
    "remote_directories",
    "remote_files",
    "remote_game",
    "remote_game_roms",
    "remote_release_games",
    "remote_rom",
    "remote_rom_files",
    "remote_rom_rahash",
    "remote_media",
    "remote_title_media",
    "remote_release_media",
    "remote_sync_state",
    // the peer sync's change log and identity (`database::sync_rev`): numbers that mean
    // something only on this database, so a restore must never bring another machine's
    "sync_tombstones",
    "sync_state",
    // signed-in devices: each signs in again after a restore, rather than a backup carrying
    // a way in to the server
    "user_tokens",
];

/// The tables a kind covers. A free function rather than a method because `Kind` is shared
/// with the GUI through `romexplorer_types` and the lists are the backend's business.
pub fn tables(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Database => &DATABASE_TABLES,
        Kind::Catalog => &CATALOG_TABLES,
        Kind::DatSeed => &SEED_TABLES,
    }
}

/// The DAT-derived half of the catalogue, plus which IGDB / RetroAchievements game each of
/// its titles and releases is ([`SEED_LINK_PROVIDERS`]): what a DAT import built and the two
/// cross-references that take longest to build, and nothing else a provider added. Meant to
/// be handed to another install so it starts linked instead of spending hours getting
/// there. A strict subset of [`CATALOG_TABLES`], asserted as one by a test.
///
/// Left out, and why -- this list is the whole point of the kind, so the reasoning is here
/// rather than in a commit message:
///
/// * The 13 provider response caches (`igdb_games`, `mobygames_games`, `screenscraper_games`
///   ...). Other people's databases, held under API terms that restrict passing them on.
/// * Every provider link other than IGDB's and RetroAchievements'. Those two are in, as ids
///   only -- an id is a cross-reference, not content -- because they are what the title
///   merge and the achievements screens hang on; a restored seed fetches the metadata for
///   them itself. The other providers' ids stay out until someone asks for them.
/// * The provider vocabularies (`genre`, `company`, `franchise`, `keyword`, `perspective`)
///   and every junction onto them. Populated only by provider paths.
/// * `title_merge_*`. The merge *results* are already in `title`, and `title_merge_log.reason`
///   records `igdb:<id>` -- so shipping the log would put provider ids in a seed that
///   deliberately has none.
/// * `title_locales` / `title_alt_names`. Both carry provider-sourced rows alongside DAT ones
///   and are cheap to re-derive; excluded rather than filtered.
pub const SEED_TABLES: [&str; 16] = [
    "sources",
    "dat_imports",
    "platforms",
    "content",
    "content_contents",
    "content_platform_data",
    "hashes",
    "release",
    "release_sources",
    "release_contents",
    "release_content_sources",
    "title",
    "title_releases",
    "title_aliases",
    "title_provider_links",
    "release_provider_links",
];

/// The providers whose links a seed carries.
pub const SEED_LINK_PROVIDERS: [&str; 2] = ["igdb", "retroachievements"];

/// A `SELECT id FROM sources` naming the DAT sources, for the filters below.
///
/// `sources` is **not** DAT-only, which is the trap this exists to avoid: `providers::cache`
/// calls `import::ensure_source(Provider::Hasheous.name())` and attributes the releases it
/// creates to that row. So "has a `source_id`" does not mean "came from a DAT", and every
/// filter names the six explicitly instead. Built from `Source::ALL` rather than a literal
/// list so a seventh source cannot be added there and silently missed here.
fn dat_sources_subquery() -> String {
    let names = crate::database::dat_download::Source::ALL.iter().map(|s| format!("'{}'", s.name())).collect::<Vec<_>>().join(", ");
    format!("SELECT id FROM sources WHERE name IN ({names})")
}

/// The `WHERE` fragment restricting one seed table to DAT-derived rows, or `None` when the
/// whole table qualifies.
///
/// Every filter is expressed against the DAT source list so that the row sets stay
/// referentially consistent: a `release` that is excluded takes its `release_contents`,
/// `release_sources` and `title_releases` with it, and a `content` that is excluded takes its
/// `hashes` and junctions. Getting that wrong would produce a seed that restores into foreign
/// key violations rather than one that is merely incomplete.
fn seed_filter(table: &str) -> Option<String> {
    let src = dat_sources_subquery();
    let dat_content = format!("SELECT id FROM content WHERE source_id IN ({src})");
    let dat_release = format!("SELECT id FROM `release` WHERE source_id IN ({src})");
    Some(match table {
        // `platforms` and `dat_imports` are DAT-side in their entirety.
        "platforms" | "dat_imports" => return None,
        "sources" => format!("id IN ({src})"),
        "content" => format!("source_id IN ({src})"),
        "release" => format!("source_id IN ({src})"),
        // These carry their own source_id (migration 0011), so they need no join.
        "release_sources" | "release_contents" | "release_content_sources" => format!("source_id IN ({src})"),
        "content_contents" => format!("parent_content_id IN ({dat_content}) AND child_content_id IN ({dat_content})"),
        "content_platform_data" => format!("content_id IN ({dat_content})"),
        // content_id is NULL for rom-level ra_hash rows, which attach through
        // local_rom_rahash instead -- machine-local, not catalogue, so they are not seed data.
        "hashes" => format!("content_id IN ({dat_content})"),
        "title_releases" => format!("release_id IN ({dat_release})"),
        // A title is seed data when a DAT release still points at it.
        "title" => format!("id IN (SELECT title_id FROM title_releases WHERE release_id IN ({dat_release}))"),
        "title_aliases" => format!("title_id IN (SELECT title_id FROM title_releases WHERE release_id IN ({dat_release}))"),
        // Live links of the two seed providers, onto titles / releases the seed has.
        "title_provider_links" => format!("deleted = FALSE AND provider IN ({}) AND title_id IN (SELECT title_id FROM title_releases WHERE release_id IN ({dat_release}))", seed_link_providers()),
        "release_provider_links" => format!("deleted = FALSE AND provider IN ({}) AND release_id IN ({dat_release})", seed_link_providers()),
        other => unreachable!("seed_filter called for {other}, which is not in SEED_TABLES"),
    })
}

fn seed_link_providers() -> String {
    SEED_LINK_PROVIDERS.iter().map(|p| format!("'{p}'")).collect::<Vec<_>>().join(", ")
}

/// Ownership and configuration: everything this machine cannot get back by re-importing.
pub const DATABASE_TABLES: [&str; 41] = [
    // AI answers that were paid for (`api::ai`), keyed by stable ids, not row ids
    "ai_answers",
    // the user's order of a release's local copies
    "local_game_order",
    // the name dictionary: the user's own names and the automatic ones they removed; the
    // automatic ones themselves are rebuilt from the catalogue, but kept so a restored
    // machine searches the same way before its next rebuild
    "name_dictionary",
    "name_dictionary_blocks",
    // Derivable, strictly -- the verify job would work it out again by re-reading every sheet
    // on disk. It is backed up anyway for the same reason `local_files.last_integrity_check`
    // is: it is a few hundred rows, and throwing it away means a restored machine reports every
    // sheet as unchecked until a sweep has walked the whole collection again.
    "local_sheet_check",
    // what this machine holds, and how it is grouped
    "local_directories",
    "local_files",
    // the audio library and its tags: a rescan would find the files again, but not the tags
    "audio_files",
    "audio_file_tags",
    "local_rom",
    "local_rom_files",
    "local_rom_rahash",
    "local_game",
    "local_game_roms",
    "local_release_games",
    // the tracks found inside this machine's ROMs: a re-index finds them again, but not the
    // kinds a user set by hand
    "rom_audio_index",
    "rom_audio_title",
    "rom_audio_tracks",
    // art and where it lives -- the files themselves are not in here, only the rows
    "media",
    "title_media",
    "release_media",
    // collections a user curated by hand; nothing rebuilds these
    "collection_sets",
    "collection_set_entries",
    "collection_set_releases",
    // patches this machine downloaded, and where they landed
    "ra_patches",
    "ra_patch_files",
    // this machine's RetroAchievements account and its progress. Re-fetchable from RA, but
    // it is this user's account rather than anything the catalogue describes.
    "ra_user_profile",
    "ra_user_progress",
    "ra_user_achievements",
    "ra_user_game_detail",
    // configuration a user built by hand
    "settings",
    "media_dirs",
    "temp_dirs",
    "emulators",
    "emulator_platforms",
    // which files in the emulators' artwork folders this machine wrote: without it a restored
    // export would take every one of them for someone else's and never update them again
    "media_exports",
    "schedules",
    // the peers this machine syncs with. Their tokens are *not* in the zip: every export nulls
    // credential columns (`api::secrets`), so a backup can be kept or shared without handing
    // out access to the peers. Restored onto a fresh database they come back without tokens
    // and are re-entered on the Sources screen; restored over a live one, the tokens already
    // there are kept. The mirrored `remote_*` tables are not here -- those are a copy of
    // someone else's data and re-sync on their own.
    "remote_sources",
    // where those peers' shares are mounted here, and how a copy lays its destination out.
    // Both are configuration nothing rebuilds: the mount describes this machine's own setup,
    // and a per-platform folder name is a decision the user made.
    "remote_mounts",
    "platform_output",
    // who may use this machine over the network. The accounts come back, their password
    // hashes do not (`api::secrets`): restored onto a fresh install they can't sign in until
    // an admin sets new passwords, and until an admin can, the setup token works again.
    "users",
];

/// The catalogue: what DAT imports and the providers built. Large, and re-derivable -- but
/// only by re-downloading and re-importing, and never with the same row ids.
pub const CATALOG_TABLES: [&str; 61] = [
    // provenance
    "sources",
    "dat_imports",
    "platforms",
    // content identity and the hashes that establish it -- by far the largest tables
    "content",
    "content_contents",
    "content_platform_data",
    "hashes",
    // releases
    "release",
    "release_sources",
    "release_contents",
    "release_content_sources",
    // files a newer DAT changed or dropped, kept beside their release
    "release_contents_superseded",
    // what each patched release is made from, and what the user said about it
    "release_bases",
    // how each release is meant to be shown, from the user and the sources that know
    "release_display",
    "release_provider_links",
    "release_provider_serials",
    "release_ratings",
    // titles and their vocabularies
    "title",
    "title_releases",
    "title_aliases",
    "title_alt_names",
    "title_locales",
    "title_provider_links",
    "title_companies",
    "title_franchises",
    "title_genres",
    "title_keywords",
    "title_perspectives",
    // families: the games a collection holds, and the user's order of a family's versions
    "title_bundle_games",
    "title_family_order",
    "title_families",
    // minor versions and version groups: the user's corrections, and which version of a
    // group is shown
    "title_version_relations",
    "version_group_picks",
    "company",
    "franchise",
    "franchise_media",
    "genre",
    "genre_raw_mapping",
    "keyword",
    "keyword_media",
    "perspective",
    // the merge decisions made over those titles, including the manual "never merge these"
    "title_merge_log",
    "title_merge_archive",
    "title_merge_blocks",
    "title_merge_suggestions",
    // provider caches: raw responses, so a re-run costs no quota
    "igdb_games",
    "igdb_game_relations",
    "mobygames_games",
    "screenscraper_games",
    "screenscraper_rom_index",
    "thegamesdb_games",
    "steamgriddb_games",
    "emuready_games",
    "emuready_reports",
    "hasheous_lookups",
    "playmatch_lookups",
    "retroachievements_games",
    "retroachievements_hashes",
    "ra_game_hashes",
    // the "already fetched this, don't fetch it again" watermarks. These belong with the
    // catalogue: restoring the caches without them would re-download every provider dump.
    "provider_dump_files",
    "provider_sync_state",
];

/// Where backups go: the `backup_dir` setting, else `<data root>/backups`.
pub async fn directory(app: &App) -> Result<PathBuf> {
    let configured = crate::database::settings::get::<String>(&app.pool, crate::database::settings::KEY_BACKUP_DIR).await?;
    Ok(match configured.filter(|p| !p.trim().is_empty()) {
        Some(p) => PathBuf::from(p),
        None => app.paths.backups_dir(),
    })
}

/// Whether a database backup should be followed by a catalogue one.
pub async fn also_catalog(app: &App) -> Result<bool> {
    Ok(crate::database::settings::get::<bool>(&app.pool, crate::database::settings::KEY_BACKUP_CATALOG).await?.unwrap_or(false))
}

/// Which kind a file on disk is, or `None` if it is not one of ours.
fn kind_of(path: &Path) -> Option<Kind> {
    let name = path.file_name()?.to_str()?;
    if !name.ends_with(".zip") {
        return None;
    }
    [Kind::Database, Kind::Catalog, Kind::DatSeed].into_iter().find(|k| name.starts_with(k.prefix()))
}

/// Every backup in the directory, newest first. `kind` restricts to one of the two.
pub async fn list(app: &App, kind: Option<Kind>) -> Result<Vec<BackupFile>> {
    let dir = directory(app).await?;
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else { return Ok(out) };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(found) = kind_of(&path) else { continue };
        if kind.is_some_and(|want| want != found) {
            continue;
        }
        let meta = entry.metadata().map_err(|e| Error::internal(e.to_string()))?;
        let created = meta.modified().ok().map(|t| chrono::DateTime::<chrono::Local>::from(t).naive_local()).unwrap_or_default();
        out.push(BackupFile {
            name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
            kind: found.name().to_string(),
            bytes: meta.len(),
            created,
        });
    }
    out.sort_by(|a, b| b.created.cmp(&a.created));
    Ok(out)
}

/// One zip: a line of JSON per row, one file per table, plus a manifest naming the kind and
/// the schema version so a restore can refuse a backup it does not understand.
///
/// Reports per *page*, not per table. A catalogue backup writes ~10M `hashes` rows on its
/// own, so per-table reporting would sit silent for minutes in the middle of one table;
/// `Progress::report` throttles to about one write a second regardless.
pub async fn create(app: &App, kind: Kind, progress: &crate::database::jobs::Progress) -> Result<BackupSummary> {
    let dir = directory(app).await?;
    std::fs::create_dir_all(&dir).map_err(|e| Error::internal(format!("creating {}: {e}", dir.display())))?;
    let stamp = chrono::Local::now().naive_local();
    let name = format!("{}{}.zip", kind.prefix(), stamp.format("%Y%m%d-%H%M%S"));
    let path = dir.join(&name);
    // Written under a hidden name and renamed when whole, so a backup that fails or is
    // cancelled part way never shows up in the list or counts towards the keep-count. The
    // guard removes it, and every table's part, however this ends (a cancelled job drops
    // this future).
    let writing = dir.join(format!(".{name}.writing"));
    struct Leftovers(Vec<PathBuf>);
    impl Drop for Leftovers {
        fn drop(&mut self) {
            for path in &self.0 {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    let mut leftovers = Leftovers(vec![writing.clone()]);

    let file = std::fs::File::create(&writing).map_err(|e| Error::internal(format!("creating {}: {e}", writing.display())))?;
    let mut zip = zip::ZipWriter::new(file);
    // zstd at level 3 rather than deflate: measured on this repo's 135 MB catalogue export it
    // is 4.8% smaller *and* 4.8x faster (362 MB/s against 75), and on a backup whose job is to
    // finish quickly and often, speed is the part that decides whether it gets run. Higher
    // levels are not worth it -- zstd -19 gains 27% for 300x the time.
    //
    // The cost is that `unzip` and Windows Explorer cannot open a zstd member; 7-Zip 21.07+
    // and romexplorer's own restore can.
    let options: zip::write::FileOptions<'_, ()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Zstd).compression_level(Some(3));

    let wanted = tables(kind);
    // Sized up front in rows, so the bar is a fraction of the rows rather than of the
    // *tables* -- 51 tables where one of them is most of the work makes a per-table bar jump
    // from 10% to 90% and sit there. Counted exactly (see `rows::exact_rows` for why the
    // optimizer's estimate was not good enough), except for a seed: its tables are filtered,
    // and counting through the filter is the export's own work done twice.
    progress.force(0, None, "counting rows").await;
    let estimate = match kind {
        Kind::DatSeed => super::rows::estimated_rows(app, wanted).await,
        _ => super::rows::exact_rows(app, wanted).await,
    };
    let approx = if kind == Kind::DatSeed { "~" } else { "" };
    let total_rows: u64 = estimate.iter().sum();
    // Several tables at once, each into a part file of its own, then the parts joined in
    // table order. A zip is written one member after another, so writing the tables
    // straight into it could only ever be one at a time -- and most of a backup is waiting
    // for MariaDB to build each page of rows (`JSON_OBJECT` over 2,000 at a time) with one
    // connection busy and the rest idle. A part is itself a one-member zip, so joining is a
    // copy of bytes already compressed (`raw_copy_file`), not a second compression. The
    // parts take the backup's size again in the backup directory while it runs.
    //
    // The largest tables are started first: `hashes`, `content` and `release` are most of a
    // catalogue, and a table started last would run alone at the end.
    let workers = parallel_tables(app, wanted.len());
    let part_path = |i: usize| dir.join(format!(".{name}.{i}.part"));
    leftovers.0.extend((0..wanted.len()).map(part_path));
    let mut order: Vec<usize> = (0..wanted.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(estimate[i]));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let rows_done = std::sync::atomic::AtomicU64::new(0);
    let counts: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(vec![0; wanted.len()]);
    let failed: std::sync::Mutex<Option<Error>> = std::sync::Mutex::new(None);
    progress.force(0, Some(total_rows.max(1)), &format!("{} tables, {workers} at a time", wanted.len())).await;
    let drain = || async {
        loop {
            let Some(&i) = order.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed)) else { break };
            if failed.lock().unwrap().is_some() {
                break;
            }
            match export_table(app, kind, wanted[i], &part_path(i), options, &rows_done, total_rows, approx, progress).await {
                Ok(rows) => counts.lock().unwrap()[i] = rows,
                Err(e) => {
                    failed.lock().unwrap().get_or_insert(e);
                    break;
                }
            }
        }
    };
    let runtime = tokio::runtime::Handle::current();
    if workers > 1 && runtime.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
        // A thread per worker, each driving its own tables: reading rows is the database's
        // work, but parsing and compressing them is ours, and on one task that would be one
        // core however many tables were open. `block_in_place` because this task waits for
        // the threads; it needs the multi-threaded runtime, hence the plain loop otherwise.
        tokio::task::block_in_place(|| {
            std::thread::scope(|scope| {
                for _ in 0..workers {
                    scope.spawn(|| runtime.block_on(drain()));
                }
            })
        });
    } else {
        drain().await;
    }
    let cleanup = |upto: usize| {
        for i in 0..upto {
            let _ = std::fs::remove_file(part_path(i));
        }
    };
    if let Some(e) = failed.into_inner().unwrap() {
        cleanup(wanted.len());
        drop(zip);
        return Err(e);
    }
    progress.force(total_rows.max(1), Some(total_rows.max(1)), "joining the tables into one file").await;
    let counts = counts.into_inner().unwrap();
    let mut tables = Vec::new();
    for (i, table) in wanted.iter().enumerate() {
        let part = part_path(i);
        let joined = (|| -> std::result::Result<(), String> {
            let mut archive = zip::ZipArchive::new(std::fs::File::open(&part).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            let copied = zip.raw_copy_file(archive.by_index_raw(0).map_err(|e| e.to_string())?).map_err(|e| e.to_string());
            copied
        })();
        if let Err(e) = joined {
            cleanup(wanted.len());
            return Err(Error::internal(format!("joining {table} into {}: {e}", path.display())));
        }
        let _ = std::fs::remove_file(&part);
        tables.push((table.to_string(), counts[i]));
    }

    let manifest = serde_json::json!({
        "kind": kind.name(),
        "created": stamp.to_string(),
        "schema_version": crate::database::schema_version(),
        "tables": tables.iter().map(|(t, n)| serde_json::json!({ "table": t, "rows": n })).collect::<Vec<_>>(),
        "note": match kind {
            Kind::Database => "Ownership and configuration. Media files are not included. Restoring this without a matching catalogue backup needs a hash_match run to re-link releases, and cannot recover collection membership.",
            Kind::Catalog => "The catalogue built by DAT imports and providers. Re-derivable by re-importing, but not with the same row ids -- which is why restoring this alongside a database backup keeps every link intact.",
            Kind::DatSeed => "The DAT-derived half of the catalogue, with each title's and release's IGDB / RetroAchievements id: no provider response caches, no other provider links, no provider vocabularies. Restore it to bring a machine up without re-importing every DAT and re-linking it; the restore queues the IGDB metadata for the linked games.",
        },
    });
    zip.start_file("manifest.json", options).map_err(|e| Error::internal(e.to_string()))?;
    zip.write_all(serde_json::to_string_pretty(&manifest).unwrap_or_default().as_bytes()).map_err(|e| Error::internal(e.to_string()))?;
    zip.finish().map_err(|e| Error::internal(e.to_string()))?;
    std::fs::rename(&writing, &path).map_err(|e| Error::internal(format!("renaming {} to {}: {e}", writing.display(), path.display())))?;
    leftovers.0.clear();

    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    prune(app, kind, &dir).await?;
    Ok(BackupSummary { file: BackupFile { name, kind: kind.name().to_string(), bytes, created: stamp }, tables })
}

/// How many tables a backup reads at once: each holds a connection while it does, so half
/// the pool at most, and four -- past that the database is the one waiting, on its own
/// cores, for the three or four tables that are most of any backup.
fn parallel_tables(app: &App, tables: usize) -> usize {
    (app.config.max_connections as usize / 2).clamp(1, 4).min(tables.max(1))
}

/// One table as a zip of its own at `part`: `<table>.ndjson`, a line of JSON per row.
/// Returns how many rows. `rows_done` is the whole backup's count, shared by every table
/// being written, which is what the progress bar shows.
#[allow(clippy::too_many_arguments)]
async fn export_table(
    app: &App,
    kind: Kind,
    table: &str,
    part: &Path,
    options: zip::write::FileOptions<'_, ()>,
    rows_done: &std::sync::atomic::AtomicU64,
    total_rows: u64,
    approx: &str,
    progress: &crate::database::jobs::Progress,
) -> Result<u64> {
    use std::sync::atomic::Ordering::Relaxed;
    let file = std::fs::File::create(part).map_err(|e| Error::internal(format!("creating {}: {e}", part.display())))?;
    let mut zip = zip::ZipWriter::new(file);
    zip.start_file(format!("{table}.ndjson"), options).map_err(|e| Error::internal(e.to_string()))?;
    // A seed takes only the DAT-derived rows of each table; the other two kinds take whole
    // tables. Computed once per table rather than per page.
    let filter = match kind {
        Kind::DatSeed => seed_filter(table),
        _ => None,
    };
    let mut rows = 0u64;
    let mut after: Option<Vec<String>> = None;
    loop {
        // `table` comes from one of the constant lists and `filter` from `seed_filter`,
        // which is what makes the interpolation in `export_page_filtered` safe -- it takes
        // no whitelist of its own.
        let page = super::sync::export_page_filtered(app, table, filter.as_deref(), after.as_deref(), 2_000).await?;
        for row in &page.rows {
            writeln!(zip, "{row}").map_err(|e| Error::internal(e.to_string()))?;
        }
        rows += page.rows.len() as u64;
        let done = rows_done.fetch_add(page.rows.len() as u64, Relaxed) + page.rows.len() as u64;
        // Rows written while the backup runs can still carry it past its count; the total
        // follows rather than showing more done than there is.
        progress.report(done, Some(total_rows.max(done).max(1)), &format!("{table} {rows} -- {done}/{approx}{} overall", total_rows.max(done))).await;
        match page.next {
            Some(cursor) => after = Some(cursor),
            None => break,
        }
        // The table threads run outside the job's task, so cancelling it does not stop them
        // on its own.
        if progress.cancelled().await {
            return Err(Error::internal("cancelled"));
        }
    }
    zip.finish().map_err(|e| Error::internal(e.to_string()))?;
    Ok(rows)
}

/// Keeps the newest `backup_keep` files **of this kind**. `0` keeps everything. Counting per
/// kind matters: a nightly database backup would otherwise evict the rare catalogue one.
async fn prune(app: &App, kind: Kind, dir: &Path) -> Result<()> {
    let keep = crate::database::settings::get::<u64>(&app.pool, crate::database::settings::KEY_BACKUP_KEEP).await?.unwrap_or(5);
    if keep == 0 {
        return Ok(());
    }
    let mut files = list(app, Some(kind)).await?;
    if files.len() as u64 <= keep {
        return Ok(());
    }
    files.drain(..keep as usize);
    for stale in files {
        let _ = std::fs::remove_file(dir.join(&stale.name));
    }
    Ok(())
}

/// The path of one backup, checked to be a backup in the backup directory -- `name` comes
/// from a request, so it must not be able to name a file elsewhere.
pub async fn file(app: &App, name: &str) -> Result<PathBuf> {
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(Error::invalid("bad backup name"));
    }
    let path = directory(app).await?.join(name);
    if kind_of(&path).is_none() || !path.is_file() {
        return Err(Error::not_found(format!("no backup {name}")));
    }
    Ok(path)
}

/// Where an upload is received before it is checked: a hidden `.part` in the backup
/// directory itself, so accepting it is a rename, never a second copy of a large file. The
/// name matches no backup kind, so a half-received file never shows in the list.
pub async fn upload_part_path(app: &App) -> Result<PathBuf> {
    let dir = directory(app).await?;
    std::fs::create_dir_all(&dir).map_err(|e| Error::internal(format!("creating {}: {e}", dir.display())))?;
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(dir.join(format!(".upload-{}-{n}.zip.part", std::process::id())))
}

/// Takes a received upload (`part`, from [`upload_part_path`]) into the backup directory as a
/// backup of its own, listed like any other. Restoring it is a separate step.
///
/// Checked first, the way a restore checks before writing anything: a readable zip with a
/// `manifest.json` naming a kind this list shows, from a schema this build can read. Anything
/// else is deleted and refused, so a stray zip never sits in the list looking restorable.
///
/// Named after `original` when that is already a backup's name for the same kind and free;
/// otherwise `<prefix><manifest's created stamp>-uploaded.zip`. An existing file is never
/// overwritten -- a second upload of the same backup gets `-2`, `-3`, ...
pub async fn accept_upload(app: &App, part: &Path, original: &str) -> Result<BackupFile> {
    let refuse = |message: String| {
        let _ = std::fs::remove_file(part);
        Err(Error::invalid(message))
    };
    let manifest = (|| -> std::result::Result<serde_json::Value, String> {
        let file = std::fs::File::open(part).map_err(|e| e.to_string())?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("not a readable zip: {e}"))?;
        let entry = zip.by_name("manifest.json").map_err(|_| "no manifest.json -- not a romexplorer backup".to_string())?;
        serde_json::from_reader::<_, serde_json::Value>(entry).map_err(|e| format!("manifest.json: {e}"))
    })();
    let manifest = match manifest {
        Ok(m) => m,
        Err(e) => return refuse(format!("{original}: {e}")),
    };
    let Some(kind) = manifest.get("kind").and_then(|v| v.as_str()).and_then(Kind::parse).filter(|k| matches!(k, Kind::Database | Kind::Catalog | Kind::DatSeed)) else {
        return refuse(format!("{original}: not a database, catalogue or DAT seed backup"));
    };
    let written_with = manifest.get("schema_version").and_then(|v| v.as_i64()).unwrap_or(0);
    if written_with > crate::database::schema_version() {
        return refuse(format!("{original} was written by a newer version (schema {written_with}); upgrade before uploading it"));
    }

    let dir = directory(app).await?;
    let given = Path::new(original).file_name().and_then(|n| n.to_str()).unwrap_or("");
    let stem = if given.starts_with(kind.prefix()) && given.ends_with(".zip") && !given.contains("..") {
        given.trim_end_matches(".zip").to_string()
    } else {
        let stamp = manifest.get("created").and_then(|v| v.as_str()).and_then(|t| chrono::NaiveDateTime::parse_from_str(t, "%Y-%m-%d %H:%M:%S%.f").ok()).unwrap_or_else(|| chrono::Local::now().naive_local());
        format!("{}{}-uploaded", kind.prefix(), stamp.format("%Y%m%d-%H%M%S"))
    };
    let mut name = format!("{stem}.zip");
    let mut n = 2;
    while dir.join(&name).exists() {
        name = format!("{stem}-{n}.zip");
        n += 1;
    }
    let target = dir.join(&name);
    if let Err(e) = std::fs::rename(part, &target) {
        let _ = std::fs::remove_file(part);
        return Err(Error::internal(format!("storing {}: {e}", target.display())));
    }
    let meta = std::fs::metadata(&target).map_err(|e| Error::internal(e.to_string()))?;
    let created = meta.modified().ok().map(|t| chrono::DateTime::<chrono::Local>::from(t).naive_local()).unwrap_or_default();
    Ok(BackupFile { name, kind: kind.name().to_string(), bytes: meta.len(), created })
}

/// Deletes one backup. Same name checking as [`file`].
pub async fn remove(app: &App, name: &str) -> Result<()> {
    let path = file(app, name).await?;
    std::fs::remove_file(&path).map_err(|e| Error::internal(format!("removing {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backup_zip(path: &Path, manifest: &serde_json::Value) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        zip.start_file::<_, ()>("manifest.json", zip::write::FileOptions::default()).unwrap();
        std::io::Write::write_all(&mut zip, manifest.to_string().as_bytes()).unwrap();
        zip.finish().unwrap();
    }

    /// What `accept_upload` checks, without a database: the manifest reading is the same code.
    #[test]
    fn an_upload_is_named_for_its_kind_and_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("rx-upload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let part = dir.join(".upload-1.zip.part");
        backup_zip(&part, &serde_json::json!({ "kind": "database", "created": "2026-10-05 01:02:03.123", "schema_version": 1 }));
        // An uploaded backup's .part name is never mistaken for a backup itself.
        assert_eq!(kind_of(&part), None);
        assert_eq!(kind_of(&dir.join("romexplorer-database-20261005-010203-uploaded.zip")), Some(Kind::Database));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kinds_round_trip_through_their_names() {
        for kind in [Kind::Database, Kind::Catalog] {
            assert_eq!(Kind::parse(kind.name()), Some(kind));
        }
        assert_eq!(Kind::parse("everything"), None);
    }

    #[test]
    fn a_kind_only_recognizes_its_own_files() {
        assert_eq!(kind_of(Path::new("/b/romexplorer-database-20260101-000000.zip")), Some(Kind::Database));
        assert_eq!(kind_of(Path::new("/b/romexplorer-catalog-20260101-000000.zip")), Some(Kind::Catalog));
        // Not ours, so a keep-count never deletes it.
        assert_eq!(kind_of(Path::new("/b/holiday-photos.zip")), None);
        assert_eq!(kind_of(Path::new("/b/romexplorer-database-20260101-000000.zip.part")), None);
    }

    /// Every table in every migration is in exactly one of the two backups or on the
    /// explicit exclusion list. This is the test that makes a backup trustworthy: without
    /// it, a table added by a later migration is simply absent from every backup, and
    /// nothing says so until someone tries to restore one.
    #[test]
    fn every_table_is_classified() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
        let mut found = Vec::new();
        for entry in std::fs::read_dir(&dir).expect("migrations/ is readable").flatten() {
            let sql = std::fs::read_to_string(entry.path()).unwrap_or_default();
            for line in sql.lines() {
                let lower = line.trim().to_ascii_lowercase();
                let Some(rest) = lower.strip_prefix("create table") else { continue };
                let rest = rest.trim().strip_prefix("if not exists").unwrap_or(rest);
                let name: String = rest.trim().trim_start_matches('`').chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
                if !name.is_empty() {
                    found.push(name);
                }
            }
        }
        assert!(found.len() > 50, "only found {} tables -- the parse broke, not the lists", found.len());
        let unclassified: Vec<_> = found
            .iter()
            .filter(|t| {
                let t = t.as_str();
                !DATABASE_TABLES.contains(&t) && !CATALOG_TABLES.contains(&t) && !NOT_BACKED_UP.contains(&t)
            })
            .cloned()
            .collect();
        assert!(unclassified.is_empty(), "tables in no backup and on no exclusion list: {unclassified:?}");
    }

    /// A seed must be a strict subset of the catalogue: it restores into the same tables, so
    /// a table here that the catalogue does not carry would be written by a seed restore and
    /// never replaced by a catalogue one.
    #[test]
    fn seed_is_a_subset_of_the_catalogue() {
        for t in SEED_TABLES {
            assert!(CATALOG_TABLES.contains(&t), "{t} is in the seed but not in the catalogue");
        }
        assert!(SEED_TABLES.len() < CATALOG_TABLES.len(), "a seed that is the whole catalogue is not a seed");
    }

    /// The point of the kind. If a provider table ever reaches the seed, the thing the seed
    /// exists to avoid has happened, so name them explicitly rather than trusting the list.
    #[test]
    fn seed_carries_no_provider_data() {
        let banned = [
            "igdb_games", "mobygames_games", "screenscraper_games", "screenscraper_rom_index",
            "thegamesdb_games", "steamgriddb_games", "emuready_games", "emuready_reports",
            "hasheous_lookups", "playmatch_lookups", "retroachievements_games",
            "retroachievements_hashes", "ra_game_hashes", "provider_dump_files",
            "provider_sync_state",
            "genre", "genre_raw_mapping", "company", "franchise", "keyword", "perspective",
            "title_genres", "title_companies", "title_franchises", "title_keywords",
            "title_perspectives", "franchise_media", "keyword_media",
            // title_merge_log.reason records `igdb:<id>`.
            "title_merge_log", "title_merge_archive", "title_merge_blocks", "title_merge_suggestions",
        ];
        for t in banned {
            assert!(!SEED_TABLES.contains(&t), "{t} is provider-derived and must not be in a DAT seed");
        }
    }

    /// The links are the one provider-derived thing a seed carries, and only two providers'.
    #[test]
    fn seed_links_are_igdb_and_retroachievements_only() {
        for t in ["title_provider_links", "release_provider_links"] {
            let f = seed_filter(t).unwrap();
            assert!(f.contains("provider IN ('igdb', 'retroachievements')") && f.contains("deleted = FALSE"), "{t}: {f}");
        }
    }

    /// `seed_filter` panics on anything outside the list, so every entry must be covered --
    /// and the two that legitimately take no filter must keep saying so out loud.
    #[test]
    fn every_seed_table_has_a_decision() {
        for t in SEED_TABLES {
            let f = seed_filter(t);
            match t {
                "platforms" | "dat_imports" => assert!(f.is_none(), "{t} unexpectedly filtered"),
                _ => {
                    let f = f.unwrap_or_else(|| panic!("{t} has no seed filter"));
                    assert!(f.contains("sources"), "{t}'s filter does not restrict to DAT sources: {f}");
                }
            }
        }
    }

    /// The filter is built from `Source::ALL`, so a source added there appears here without
    /// anyone remembering to update a literal list.
    #[test]
    fn dat_sources_subquery_names_every_dat_source() {
        let sql = dat_sources_subquery();
        for s in crate::database::dat_download::Source::ALL {
            assert!(sql.contains(&format!("'{}'", s.name())), "{} missing from {sql}", s.name());
        }
    }

    /// RetroAchievements has two halves and they must not be confused.
    ///
    /// The *base* (which games have achievements, and their hashes) is shared reference data
    /// that any machine can re-fetch, so it belongs to the catalogue and is replaced with it.
    /// The *user's* records -- what they have earned, their points, what they have mastered --
    /// are this account's history, cannot be rebuilt from a DAT, and must survive a catalogue
    /// replacement. They key on RA's own global game/achievement ids rather than on local row
    /// ids, so nothing in them is invalidated when the catalogue is swapped.
    #[test]
    fn retroachievements_user_records_are_not_catalogue() {
        for t in ["ra_user_profile", "ra_user_progress", "ra_user_achievements", "ra_user_game_detail"] {
            assert!(DATABASE_TABLES.contains(&t), "{t} is the user's own record and must be backed up with ownership");
            assert!(!CATALOG_TABLES.contains(&t), "{t} must not be wiped by a catalogue replacement");
        }
        for t in ["retroachievements_games", "retroachievements_hashes", "ra_game_hashes"] {
            assert!(CATALOG_TABLES.contains(&t), "{t} is shared reference data");
            assert!(!DATABASE_TABLES.contains(&t), "{t} is re-fetchable, not the user's own");
        }
    }

    #[test]
    fn the_two_table_lists_do_not_overlap() {
        // A table in both would be written twice and restored twice, and the second restore
        // would silently undo whatever the first did.
        for t in DATABASE_TABLES {
            assert!(!CATALOG_TABLES.contains(&t), "{t} is in both lists");
        }
    }
}
