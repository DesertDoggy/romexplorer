//! `romexplorer::api` + `romexplorer::server` against a throwaway embedded MariaDB: the
//! directory/storage/settings/jobs operations, the generic DB viewer, the catalog read side
//! on an empty collection, and the REST layer's auth + routing over real HTTP on loopback.
//! Local-only, no network beyond 127.0.0.1. `cargo test --test test_api -- --nocapture`.

#[path = "support/fixtures.rs"]
mod fixtures;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use romexplorer::api::{self, App, ErrorKind};
use romexplorer::database::jobs::{self as db_jobs, FileScanMode, JobType};
use romexplorer::database::server::{check_layout, free_port, MariaDbServer, ServerConfig};
use romexplorer::database::{self, AppPaths, DatabaseConfig};
use romexplorer::server::{self, ServeOptions, ServerState};
use serde_json::json;

/// An absolute path for `p` (a `/`-rooted literal): Windows needs a drive to call it absolute.
fn abs(p: &str) -> String {
    if cfg!(windows) { format!("C:{p}") } else { p.to_string() }
}

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `tests/output/mariadb_api_test`, or `rest` under it, in the OS's own separators: a path
/// joined from a `/` literal keeps the `/` on Windows, and then matches no stored path.
fn output_dir(rest: &str) -> PathBuf {
    rest.split('/').filter(|part| !part.is_empty()).fold(crate_dir().join("tests").join("output").join("mariadb_api_test"), |dir, part| dir.join(part))
}

async fn bring_up(base: &PathBuf) -> (MariaDbServer, App) {
    let paths = AppPaths::from_root(crate_dir().join("app_root_ex"));
    check_layout(&paths.mariadb_dir()).expect("bundled mariadb layout");
    let _ = std::fs::remove_dir_all(base);
    let datadir = base.join("data");
    let port = free_port().expect("free port");
    // A small cache: a test database is tiny, and a quarter of this machine's memory fails to
    // start while the app's own server is running.
    let mut cfg = ServerConfig::new(paths.mariadb_dir(), &datadir, port);
    cfg.buffer_pool_bytes = 256 * 1024 * 1024;
    let mut server = MariaDbServer::new(cfg);
    server.start().await.expect("mariadbd start");
    let config = DatabaseConfig::default().with_port(port).with_database("romexplorer_api_test");
    let pool = database::initialize(&config).await.expect("initialize");
    let app = App::with_pool(pool, paths, config).expect("app");
    (server, app)
}

async fn directories(app: &App, base: &PathBuf) -> u64 {
    let root = base.join("collection");
    let sub = root.join("snes");
    std::fs::create_dir_all(&sub).unwrap();

    use api::directories::{create, delete, list, resolve, update, DirectorySpec};
    let spec = |path: &PathBuf, root: bool| DirectorySpec { path: path.display().to_string(), is_collection_root: root, ..Default::default() };
    let kind = |r: Result<api::directories::DirectoryView, api::Error>| r.err().map(|e| e.kind);

    let bad = create(app, &DirectorySpec { path: "relative/path".into(), ..Default::default() }).await;
    assert!(matches!(bad, Err(e) if e.kind == ErrorKind::Invalid), "relative path must be rejected");
    assert_eq!(kind(create(app, &spec(&sub, false)).await), Some(ErrorKind::Invalid), "nothing but a root before a root exists");

    let root_view = create(app, &DirectorySpec { dir_type: Some("rom".into()), ..spec(&root, true) }).await.unwrap();
    assert!(root_view.directory.is_collection_root && root_view.directory.parent_directory_id.is_none() && root_view.exists_on_disk && root_view.file_count == 0);

    let sub_view = create(app, &DirectorySpec { app_platform_id: Some(19), ..spec(&sub, false) }).await.unwrap();
    assert_eq!(sub_view.directory.parent_directory_id, Some(root_view.directory.id), "parent derived from the path");
    assert_eq!(sub_view.effective_dir_type.as_deref(), Some("rom"), "dir_type inherits from the root");
    assert_eq!(sub_view.effective_platform_id, Some(19));

    let leaf = resolve(app, &sub.join("games").join("x").display().to_string()).await.unwrap();
    assert_eq!(leaf.directory.as_ref().map(|d| d.id), Some(sub_view.directory.id), "nearest configured ancestor wins");
    assert_eq!(leaf.collection_root.as_ref().map(|d| d.id), Some(root_view.directory.id));
    assert_eq!(leaf.dir_type.as_deref(), Some("rom"));
    assert_eq!(leaf.app_platform_id, Some(19));

    let outside = resolve(app, &abs("/definitely/not/configured")).await.unwrap();
    assert!(outside.directory.is_none());

    // Hierarchy rules: subdirectories only inside a root, roots never nested.
    assert_eq!(kind(create(app, &spec(&base.join("elsewhere"), false)).await), Some(ErrorKind::Invalid), "outside every root");
    assert_eq!(kind(create(app, &spec(&sub.join("nested"), true)).await), Some(ErrorKind::Invalid), "root inside a root");
    assert_eq!(kind(create(app, &spec(base, true)).await), Some(ErrorKind::Invalid), "root around a root");
    assert_eq!(kind(update(app, root_view.directory.id, &spec(&root, false)).await), Some(ErrorKind::Invalid), "unticking root would strand its subdirectory");

    // Adding a directory between a row and its configured child re-parents the child.
    let deep = sub.join("hacks").join("translated");
    std::fs::create_dir_all(&deep).unwrap();
    let deep_view = create(app, &spec(&deep, false)).await.unwrap();
    assert_eq!(deep_view.directory.parent_directory_id, Some(sub_view.directory.id));
    let mid_view = create(app, &spec(&sub.join("hacks"), false)).await.unwrap();
    assert_eq!(mid_view.directory.parent_directory_id, Some(sub_view.directory.id));
    assert_eq!(api::directories::get(app, deep_view.directory.id).await.unwrap().directory.parent_directory_id, Some(mid_view.directory.id));

    // A second, separate root (e.g. no-intro next to redump) is fine.
    let other = base.join("nointro");
    std::fs::create_dir_all(other.join("snes")).unwrap();
    let other_view = create(app, &spec(&other, true)).await.unwrap();
    let other_child = create(app, &spec(&other.join("snes"), false)).await.unwrap();
    assert_eq!(other_child.directory.parent_directory_id, Some(other_view.directory.id));
    assert_eq!(list(app).await.unwrap().len(), 6);

    let updated = update(app, sub_view.directory.id, &DirectorySpec { is_archival: true, ..spec(&sub, false) }).await.unwrap();
    assert!(updated.directory.is_archival && updated.directory.app_platform_id.is_none());
    assert_eq!(updated.descendants.len(), 2);

    // Removal: refused while configured subdirectories exist unless recursive; then the
    // row and everything below go -- as configuration only, the folders stay on disk.
    assert!(matches!(delete(app, sub_view.directory.id, false).await, Err(e) if e.kind == ErrorKind::Conflict));
    let removed = delete(app, sub_view.directory.id, true).await.unwrap();
    assert_eq!(removed.len(), 3);
    let removed = delete(app, other_view.directory.id, true).await.unwrap();
    assert_eq!(removed.len(), 2);
    assert!(sub.is_dir() && deep.is_dir() && other.join("snes").is_dir(), "removing configuration never touches disk");
    let remaining = list(app).await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert!(remaining[0].descendants.is_empty());
    assert!(matches!(api::directories::get(app, 999_999).await, Err(e) if e.kind == ErrorKind::NotFound));
    root_view.directory.id
}
async fn storage(app: &App) {
    // Outside the test's own folder, so a run that broke off left its art here, and the next
    // run's uploads then found "cover.png already exists".
    for dir in ["/tmp/rx-tier1-a", "/tmp/rx-tier1-b", "/tmp/rx-img-a", "/tmp/rx-img-b"] {
        let _ = std::fs::remove_dir_all(abs(dir));
    }
    api::storage::add_media_dir(app, "tier1_cache", &abs("/tmp/rx-tier1-a")).await.unwrap();
    let v = api::storage::add_media_dir(app, "tier1_cache", &abs("/tmp/rx-tier1-b")).await.unwrap();
    let tier1: Vec<_> = v.media_dirs.iter().filter(|d| d.dir.role == "tier1_cache").collect();
    assert_eq!(tier1.len(), 1, "tier1_cache is single-row");
    assert_eq!(tier1[0].dir.path, abs("/tmp/rx-tier1-b"));
    api::storage::add_media_dir(app, "image", &abs("/tmp/rx-img-a")).await.unwrap();
    let v = api::storage::add_media_dir(app, "image", &abs("/tmp/rx-img-b")).await.unwrap();
    assert_eq!(v.media_dirs.iter().filter(|d| d.dir.role == "image").count(), 2, "image dirs are multi-row");
    assert!(api::storage::add_media_dir(app, "bogus", &abs("/tmp/x")).await.is_err());
    api::storage::set_temp_dir(app, 1, &abs("/tmp/rx-temp-a")).await.unwrap();
    let v = api::storage::set_temp_dir(app, 1, &abs("/tmp/rx-temp-b")).await.unwrap();
    assert_eq!(v.temp_dirs.len(), 1);
    assert_eq!(v.temp_dirs[0].dir.path, abs("/tmp/rx-temp-b"));
    assert!(api::storage::set_temp_dir(app, 3, &abs("/tmp/x")).await.is_err());
    let id = v.temp_dirs[0].dir.id;
    let v = api::storage::remove_temp_dir(app, id).await.unwrap();
    assert!(v.temp_dirs.is_empty());
}

async fn settings(app: &App) {
    let list = api::settings::list(app).await.unwrap();
    assert!(list.iter().any(|s| s.key == "title_display_mode" && s.value.is_none()));
    api::settings::set(app, "cache_ttl_days", json!(45)).await.unwrap();
    assert_eq!(api::settings::get(app, "cache_ttl_days").await.unwrap(), Some(json!(45)));
    assert!(matches!(api::settings::set(app, "cache_ttl_days", json!("soon")).await, Err(e) if e.kind == ErrorKind::Invalid));
    api::settings::set(app, "my_custom", json!({"anything": [1, 2]})).await.unwrap();
    let list = api::settings::list(app).await.unwrap();
    assert!(list.iter().any(|s| s.key == "my_custom" && s.description.is_none()));
    assert!(api::settings::delete(app, "my_custom").await.unwrap());
    assert!(!api::settings::delete(app, "my_custom").await.unwrap());
}

async fn jobs(app: &App, directory_id: u64) {
    let req = api::jobs::JobRequest::FileScan(api::jobs::FileScanRequest { directory_id, mode: FileScanMode::NewOnly, platform: api::jobs::PlatformSelect::All, hash_match: None, file_id: None });
    let id = api::jobs::enqueue(app, &req).await.unwrap();
    let claimed = db_jobs::claim(&app.pool, JobType::FileScan, 5).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, id);
    assert_eq!(claimed[0].payload, json!({"directory_id": directory_id, "mode": "new_only", "platform": "all"}));

    let missing = api::jobs::JobRequest::HashMatch(api::jobs::HashMatchRequest { directory_id: 424242, to_dat: true, to_hasheous: false, to_playmatch: false, all: false });
    assert!(matches!(api::jobs::enqueue(app, &missing).await, Err(e) if e.kind == ErrorKind::NotFound));
    let nothing = api::jobs::JobRequest::HashMatch(api::jobs::HashMatchRequest { directory_id, to_dat: false, to_hasheous: false, to_playmatch: false, all: false });
    assert!(matches!(api::jobs::enqueue(app, &nothing).await, Err(e) if e.kind == ErrorKind::Invalid));
    let bad_source = api::jobs::JobRequest::DatSync(api::jobs::DatSyncRequest { source: Some("nope".into()), limit: None, import: true, offline: false, platforms: None, extras: None });
    assert!(api::jobs::enqueue(app, &bad_source).await.is_err());

    db_jobs::fail(&app.pool, &claimed[0], "simulated").await.unwrap();
    for _ in 0..3 {
        let again = db_jobs::claim(&app.pool, JobType::FileScan, 5).await.unwrap();
        if again.is_empty() {
            break;
        }
        db_jobs::fail(&app.pool, &again[0], "simulated").await.unwrap();
    }
    let job = api::jobs::get(app, id).await.unwrap();
    assert_eq!(job.status, "failed");
    let retried = api::jobs::retry(app, id).await.unwrap();
    assert_eq!(retried.status, "pending");
    assert_eq!(retried.attempts, 0);
    assert!(matches!(api::jobs::retry(app, id).await, Err(e) if e.kind == ErrorKind::Conflict));

    let page = api::jobs::list(app, &api::jobs::ListQuery { job_type: Some(JobType::FileScan.name().into()), ..Default::default() }).await.unwrap();
    assert_eq!(page.total, 1);
    // Several types at once (the Scan tab's list): scans and hash matches, nothing else.
    let page = api::jobs::list(app, &api::jobs::ListQuery { job_type: Some("file_scan, hash_match".into()), ..Default::default() }).await.unwrap();
    assert!(page.jobs.iter().all(|j| matches!(j.job_type, JobType::FileScan | JobType::HashMatch)), "{:?}", page.jobs);
    assert_eq!(page.total, 1);
    assert!(api::jobs::list(app, &api::jobs::ListQuery { job_type: Some("file_scan,nope".into()), ..Default::default() }).await.is_err());
    assert!(api::jobs::list(app, &api::jobs::ListQuery { status: Some("sleeping".into()), ..Default::default() }).await.is_err());
    // The per-file detail the Scan tab draws reaches the job row, and a finished job drops it.
    let progress = romexplorer::database::jobs::Progress::new(&app.pool, id);
    let detail = json!({"files": [{"name": "a.iso", "size": 100, "done": 40, "stage": "hashing"}], "running": 1});
    progress.report_detail(40, Some(100), "1 file", &detail).await;
    assert_eq!(api::jobs::get(app, id).await.unwrap().progress_detail, Some(detail));
    let counts = api::jobs::counts(app).await.unwrap();
    assert_eq!(counts.iter().find(|c| c.status == "pending").map(|c| c.count), Some(1));

    // Clearing by status takes finished jobs only, and only the statuses asked for; clearing
    // a selection also takes a selected job that never started.
    let finished = api::jobs::enqueue(app, &api::jobs::JobRequest::DbOptimize).await.unwrap();
    sqlx::query("UPDATE jobs SET status = 'done' WHERE id = ?").bind(finished).execute(&app.pool).await.unwrap();
    let failed = api::jobs::enqueue(app, &api::jobs::JobRequest::DbOptimize).await.unwrap();
    sqlx::query("UPDATE jobs SET status = 'failed' WHERE id = ?").bind(failed).execute(&app.pool).await.unwrap();
    assert!(api::jobs::clear(app, &api::jobs::ClearRequest { statuses: Some(vec!["pending".into()]), ids: None }).await.is_err(), "pending is not a finished status");
    let selected = api::jobs::clear(app, &api::jobs::ClearRequest { statuses: None, ids: Some(vec![id, finished]) }).await.unwrap();
    assert_eq!((selected.removed, selected.kept), (2, 0), "the done one goes, and so does the selected pending one");
    assert!(api::jobs::get(app, id).await.is_err() && api::jobs::get(app, finished).await.is_err());
    let waiting = api::jobs::enqueue(app, &api::jobs::JobRequest::DbOptimize).await.unwrap();
    let by_status = api::jobs::clear(app, &api::jobs::ClearRequest { statuses: Some(vec!["done".into()]), ids: None }).await.unwrap();
    assert!(api::jobs::get(app, failed).await.is_ok(), "a failed job is not cleared with the done ones: {by_status:?}");
    api::jobs::clear(app, &api::jobs::ClearRequest::default()).await.unwrap();
    assert!(api::jobs::get(app, failed).await.is_err() && api::jobs::get(app, waiting).await.is_ok(), "all finished: the failed one goes, an unselected pending one stays");
    api::jobs::clear(app, &api::jobs::ClearRequest { statuses: None, ids: Some(vec![waiting]) }).await.unwrap();
}

async fn db_viewer(app: &App) {
    let tables = api::db::tables(app).await.unwrap();
    for want in ["local_files", "local_directories", "release", "title", "jobs"] {
        assert!(tables.iter().any(|t| t.name == want), "missing {want}");
    }
    let page = api::db::rows(app, "platforms", &api::db::RowsQuery { sort: Some("name".into()), desc: true, limit: Some(5), ..Default::default() }).await.unwrap();
    assert_eq!(page.rows.len(), 5);
    assert!(page.total > 5);
    let name_idx = page.columns.iter().position(|c| c == "name").unwrap();
    let names: Vec<String> = page.rows.iter().map(|r| r[name_idx].clone().unwrap()).collect();
    let mut sorted = names.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(names, sorted, "server-side DESC sort");
    let filtered = api::db::rows(app, "platforms", &api::db::RowsQuery { filter: Some("Game Boy".into()), column: Some("name".into()), ..Default::default() }).await.unwrap();
    assert!(filtered.total >= 1 && filtered.rows.iter().all(|r| r[name_idx].as_deref().unwrap().contains("Game Boy")));
    assert!(matches!(api::db::rows(app, "no_such_table", &api::db::RowsQuery::default()).await, Err(e) if e.kind == ErrorKind::NotFound));
    assert!(matches!(api::db::rows(app, "platforms", &api::db::RowsQuery { sort: Some("nope".into()), ..Default::default() }).await, Err(e) if e.kind == ErrorKind::Invalid));
}

async fn sets(app: &App) {
    // A release to put in a set: insert a minimal catalog row directly.
    sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash) VALUES (0, 'set test release', SHA1('set test release'), 'set test release', SHA1('set test release'))").execute(&app.pool).await.unwrap();
    sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash) VALUES (0, 'set test release 2', SHA1('set test release 2'), 'set test release 2', SHA1('set test release 2'))").execute(&app.pool).await.unwrap();
    let rel: Vec<u64> = sqlx::query("SELECT id FROM `release` ORDER BY id").fetch_all(&app.pool).await.unwrap().iter().map(|r| sqlx::Row::try_get::<u64, _>(r, "id").unwrap()).collect();
    assert!(rel.len() >= 2);

    assert!(matches!(api::sets::create(app, &api::sets::SetSpec { name: "  ".into(), description: None }).await, Err(e) if e.kind == ErrorKind::Invalid));
    let set = api::sets::create(app, &api::sets::SetSpec { name: "Trip".into(), description: Some("for the laptop".into()) }).await.unwrap();
    assert_eq!(set.release_count, 0);
    let set = api::sets::add_release(app, set.id, rel[0]).await.unwrap();
    let set = api::sets::add_release(app, set.id, rel[1]).await.unwrap();
    let again = api::sets::add_release(app, set.id, rel[1]).await.unwrap();
    assert_eq!(again.release_count, 2, "adding twice is a no-op");
    assert!(matches!(api::sets::add_release(app, set.id, 999_999).await, Err(e) if e.kind == ErrorKind::NotFound));
    // Bulk add: already-present and unknown releases add nothing.
    assert_eq!(api::sets::add_releases(app, set.id, &[rel[0], 999_999]).await.unwrap(), 0);
    let page = api::sets::releases(app, set.id, &Default::default()).await.unwrap();
    assert_eq!(page.items.iter().map(|r| (r.id, r.set_position)).collect::<Vec<_>>(), vec![(rel[0], Some(1)), (rel[1], Some(2))]);
    api::sets::reorder(app, set.id, &[rel[1]]).await.unwrap();
    let page = api::sets::releases(app, set.id, &Default::default()).await.unwrap();
    assert_eq!(page.items.iter().map(|r| r.id).collect::<Vec<_>>(), vec![rel[1], rel[0]], "reorder puts the listed release first, the rest keep their order after");
    let detail = api::catalog::release(app, rel[0]).await.unwrap();
    assert_eq!(detail.sets.iter().map(|s| (s.id, s.position)).collect::<Vec<_>>(), vec![(set.id, 2)]);
    let set = api::sets::remove_release(app, set.id, rel[0]).await.unwrap();
    assert_eq!(set.release_count, 1);
    assert!(matches!(api::sets::remove_release(app, set.id, rel[0]).await, Err(e) if e.kind == ErrorKind::NotFound));
    let renamed = api::sets::update(app, set.id, &api::sets::SetSpec { name: "Trip 2".into(), description: None }).await.unwrap();
    assert_eq!(renamed.name, "Trip 2");
    assert_eq!(api::sets::list(app).await.unwrap().len(), 1);
    api::sets::delete(app, set.id).await.unwrap();
    assert!(api::sets::list(app).await.unwrap().is_empty());
    assert!(matches!(api::sets::get(app, set.id).await, Err(e) if e.kind == ErrorKind::NotFound));
}

/// A title is listed once per bucket of its releases, ranked full versions first, and a row
/// added to a set or copied stays that row's: a retail row never takes the demo.
async fn release_buckets(app: &App) {
    use romexplorer_types::copy::{CopySelection, TitleRowRef};
    let pool = &app.pool;
    sqlx::query("INSERT IGNORE INTO sources (name) VALUES ('no-intro')").execute(pool).await.unwrap();
    let source: u8 = sqlx::query_scalar("SELECT id FROM sources WHERE name = 'no-intro'").fetch_one(pool).await.unwrap();
    sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES ('Bucket Quest', 'bucket quest', SHA1('bucket quest'))").execute(pool).await.unwrap();
    let title: u64 = sqlx::query_scalar("SELECT id FROM title WHERE name = 'Bucket Quest'").fetch_one(pool).await.unwrap();
    // (name, region, flags set on the row)
    let releases = [
        ("Bucket Quest (Japan)", "JP", ""),
        ("Bucket Quest (USA) (Demo)", "US", "is_demo = TRUE"),
        ("Bucket Quest (USA) (Beta) (Unl)", "US", "is_beta = TRUE, is_unlicensed = TRUE"),
        ("Bucket Quest (Taiwan) (Unl)", "TW", "is_unlicensed = TRUE"),
        ("Bucket Quest (USA) (Pirate)", "US", "is_pirate = TRUE, is_unlicensed = TRUE"),
        ("Bucket Quest - Harder Mode (USA) (Hack)", "US", "community_patched = TRUE, patch_type = 'gameplay', game_kind = 'hack'"),
    ];
    let mut ids = Vec::new();
    for (name, region, flags) in releases {
        sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region) VALUES (0, ?, SHA1(?), ?, SHA1(?), ?)").bind(name).bind(name).bind(name.to_lowercase()).bind(name.to_lowercase()).bind(region).execute(pool).await.unwrap();
        let id: u64 = sqlx::query_scalar("SELECT id FROM `release` WHERE name = ?").bind(name).fetch_one(pool).await.unwrap();
        if !flags.is_empty() {
            sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE `release` SET {flags} WHERE id = ?"))).bind(id).execute(pool).await.unwrap();
        }
        sqlx::query("INSERT INTO release_sources (release_id, source_id) VALUES (?, ?)").bind(id).bind(source).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(title).bind(id).execute(pool).await.unwrap();
        ids.push(id);
    }
    let (jp, demo, beta, unl, pirate, hack) = (ids[0], ids[1], ids[2], ids[3], ids[4], ids[5]);
    let q = |kinds: Option<&str>| api::catalog::ListQuery { search: Some("Bucket Quest".into()), kinds: kinds.map(str::to_string), ..Default::default() };
    let rows = |page: api::catalog::Page<api::catalog::TitleRow>| page.items.iter().filter(|t| t.id == title).map(|t| (t.row_key.clone(), t.top_release_id, t.release_count)).collect::<Vec<_>>();
    let all = api::catalog::titles(app, &q(None)).await.unwrap();
    assert_eq!(all.total, 6, "one row per bucket, each hack and pirate copy its own");
    assert_eq!(
        rows(all),
        vec![("retail".to_string(), Some(jp), 1), ("unlicensed".to_string(), Some(unl), 1), ("hack:".to_string() + &hack.to_string(), Some(hack), 1), ("pirate:".to_string() + &pirate.to_string(), Some(pirate), 1), ("demo".to_string(), Some(demo), 1), ("beta".to_string(), Some(beta), 1)],
        "retail first; a beta tagged (Unl) is a beta"
    );
    let some = api::catalog::titles(app, &q(Some("retail,demo"))).await.unwrap();
    assert_eq!(rows(some).iter().map(|r| r.0.as_str()).collect::<Vec<_>>(), vec!["retail", "demo"]);
    // The title page: the Japanese retail release first, though the USA comes first by region.
    assert_eq!(api::catalog::title(app, title).await.unwrap().releases.first().map(|r| r.id), Some(jp), "a full version before a USA demo");

    // Unlicensed listed with retail: one row, the Japanese release still on top.
    db_settings_set(app, "release_unlicensed", serde_json::json!("retail")).await;
    let joined = api::catalog::titles(app, &q(None)).await.unwrap();
    assert_eq!(rows(joined).first(), Some(&("retail".to_string(), Some(jp), 2)));
    assert_eq!(api::catalog::titles(app, &q(Some("unlicensed"))).await.unwrap().items.iter().filter(|t| t.id == title).count(), 1, "still found by its kind");
    db_settings_set(app, "release_unlicensed", serde_json::Value::Null).await;

    // A set row stays its row: the demo row never resolves to the retail release, and a
    // retail row with no retail release left resolves to nothing rather than to the demo.
    let set = api::sets::create(app, &api::sets::SetSpec { name: "Buckets".into(), description: None }).await.unwrap();
    let row = |key: &str| TitleRowRef { title_id: title, row_key: key.to_string() };
    assert_eq!(api::sets::add_title_rows(app, set.id, &[row("retail"), row("demo"), row("nonsense")]).await.unwrap(), 2);
    let listed = |page: api::catalog::Page<api::catalog::ReleaseRow>| page.items.iter().map(|r| r.id).collect::<Vec<_>>();
    assert_eq!(listed(api::sets::releases(app, set.id, &Default::default()).await.unwrap()), vec![jp, demo]);
    let mut warnings = Vec::new();
    let picked = api::selection::named_releases(app, &CopySelection { title_rows: vec![row("demo")], ..Default::default() }, false, &mut warnings).await.unwrap();
    assert_eq!((picked, warnings.is_empty()), (vec![demo], true));
    sqlx::query("UPDATE `release` SET deleted = TRUE WHERE id = ?").bind(jp).execute(pool).await.unwrap();
    assert_eq!(listed(api::sets::releases(app, set.id, &Default::default()).await.unwrap()), vec![demo], "no retail release: nothing, not the demo");
    let mut warnings = Vec::new();
    let picked = api::selection::named_releases(app, &CopySelection { title_rows: vec![row("retail")], ..Default::default() }, true, &mut warnings).await.unwrap();
    assert!(picked.is_empty() && warnings.len() == 1, "{picked:?} {warnings:?}");
    // An entry from before rows takes the row of what it stands for now: the unlicensed one.
    sqlx::query("INSERT INTO collection_set_entries (set_id, title_id, position) VALUES (?, ?, 9)").bind(set.id).bind(title).execute(pool).await.unwrap();
    api::sets::resolve(app, &[set.id]).await.unwrap();
    let keys: Vec<String> = sqlx::query_scalar("SELECT row_key FROM collection_set_entries WHERE set_id = ? ORDER BY position").bind(set.id).fetch_all(pool).await.unwrap();
    assert_eq!(keys, vec!["retail", "demo", "unlicensed"]);
    api::sets::delete(app, set.id).await.unwrap();
    sqlx::query("UPDATE `release` SET deleted = TRUE WHERE id IN (?, ?, ?, ?, ?)").bind(demo).bind(beta).bind(unl).bind(pirate).bind(hack).execute(pool).await.unwrap();
    sqlx::query("UPDATE title SET deleted = TRUE WHERE id = ?").bind(title).execute(pool).await.unwrap();
}

async fn db_settings_set(app: &App, key: &str, value: serde_json::Value) {
    if value.is_null() {
        sqlx::query("DELETE FROM settings WHERE `key` = ?").bind(key).execute(&app.pool).await.unwrap();
    } else {
        romexplorer::database::settings::set(&app.pool, key, &value).await.unwrap();
    }
}

async fn views(app: &App) {
    for view in ["local", "server", "collection", "collection_local_first", "local_ra_local_first", "local_ra_ra_first", "ra_only"] {
        api::catalog::releases(app, &api::catalog::ListQuery { view: Some(view.into()), ..Default::default() }).await.unwrap();
        api::catalog::titles(app, &api::catalog::ListQuery { view: Some(view.into()), ..Default::default() }).await.unwrap();
    }
    assert!(matches!(api::catalog::releases(app, &api::catalog::ListQuery { view: Some("mine".into()), ..Default::default() }).await, Err(e) if e.kind == ErrorKind::Invalid));
    // No local copies exist here: "local" is empty, "server" is empty (no remote mirror yet),
    // "collection" sees the catalog rows the sets test inserted.
    assert_eq!(api::catalog::releases(app, &api::catalog::ListQuery { view: Some("local".into()), ..Default::default() }).await.unwrap().total, 0);
    assert_eq!(api::catalog::releases(app, &api::catalog::ListQuery { view: Some("server".into()), ..Default::default() }).await.unwrap().total, 0);
    assert!(api::catalog::releases(app, &api::catalog::ListQuery { view: Some("collection".into()), ..Default::default() }).await.unwrap().total >= 2);
    assert!(api::catalog::titles(app, &api::catalog::ListQuery { provider: Some("igdb".into()), ..Default::default() }).await.unwrap().items.is_empty());
    assert!(matches!(api::catalog::media_file(app, 1).await, Err(e) if e.kind == ErrorKind::NotFound));
}

async fn catalog_empty(app: &App) {
    assert!(!api::catalog::platforms(app).await.unwrap().is_empty());
    assert_eq!(api::catalog::titles(app, &Default::default()).await.unwrap().total, 0);
    assert_eq!(api::catalog::releases(app, &api::catalog::ListQuery { search: Some("x".into()), view: Some("local".into()), ..Default::default() }).await.unwrap().total, 0);
    assert_eq!(api::catalog::games(app, &Default::default()).await.unwrap().total, 0);
    assert_eq!(api::catalog::files(app, &Default::default()).await.unwrap().total, 0);
    assert!(matches!(api::catalog::titles(app, &api::catalog::ListQuery { sort: Some("bogus".into()), ..Default::default() }).await, Err(e) if e.kind == ErrorKind::Invalid));
    assert!(matches!(api::catalog::title(app, 1).await, Err(e) if e.kind == ErrorKind::NotFound));
    assert!(matches!(api::catalog::lookup_hash(app, None, "zz").await, Err(e) if e.kind == ErrorKind::Invalid));
    assert!(api::catalog::lookup_hash(app, Some("sha1"), "da39a3ee5e6b4b0d3255bfef95601890afd80709").await.unwrap().is_empty());
    let status = api::status::status(app).await.unwrap();
    assert_eq!(status.directories, 1);
    assert!(status.tables.iter().any(|t| t.table == "platforms" && t.rows > 0));
}

/// Provider metadata without a network: a Hasheous hit for content no DAT covers becomes
/// a release + title under the `hasheous` source (idempotent), cached hits rebuild from
/// their raw file, and the source-tagged title tables accept one row per (value, source).
async fn provider_metadata(app: &App, base: &PathBuf) {
    use romexplorer::common::models::Platform;
    use romexplorer::database::providers::cache::{self, Confidence, HasheousGameInfo, HasheousRecord};
    use romexplorer::database::providers::{metadata, Provider};
    use sqlx::Row;

    let pool = &app.pool;
    let content_id = sqlx::query("INSERT INTO content (app_platform_id, file_name, size, partial) VALUES (0, 'fixture.bin', 4096, FALSE)").execute(pool).await.unwrap().last_insert_id();
    let record = HasheousRecord {
        hit: true,
        name: Some("Fixture Quest".into()),
        game: Some(HasheousGameInfo { name: Some("Fixture Quest".into()), year: Some("1991-02-03".into()), publisher: Some("Fixture Publishing".into()), system: Some("Nintendo Entertainment System".into()) }),
        ..Default::default()
    };
    let (release_id, title_id) = cache::ensure_hasheous_release(pool, content_id, &record, Platform::Unknown).await.unwrap().expect("release built from the hit");
    assert_eq!(cache::ensure_hasheous_release(pool, content_id, &record, Platform::Unknown).await.unwrap(), Some((release_id, title_id)), "second call finds the same rows");
    let row = sqlx::query("SELECT r.app_platform_id, r.year, s.name AS source FROM `release` r JOIN sources s ON s.id = r.source_id WHERE r.id = ?").bind(release_id).fetch_one(pool).await.unwrap();
    assert_eq!(row.try_get::<String, _>("source").unwrap(), "hasheous");
    assert_eq!(row.try_get::<u16, _>("app_platform_id").unwrap(), Platform::Nes.igdb_id(), "platform from the signature's system name");
    assert_eq!(row.try_get::<Option<u16>, _>("year").unwrap(), Some(1991));
    let title = api::catalog::title(app, title_id).await.unwrap();
    assert!(title.companies.iter().any(|c| c.name == "Fixture Publishing" && c.role == "publisher" && c.source == "hasheous"));
    assert_eq!(api::catalog::release(app, release_id).await.unwrap().contents.len(), 1);
    // A miss / nameless hit builds nothing.
    assert_eq!(cache::ensure_hasheous_release(pool, content_id + 1000, &HasheousRecord { hit: true, ..Default::default() }, Platform::Unknown).await.unwrap(), None);

    // Source-tagged metadata: one row per (value, source), never duplicated.
    metadata::set_title_genres(pool, title_id, "igdb", &["Platform", "Platform", ""]).await.unwrap();
    metadata::set_title_genres(pool, title_id, "retroachievements", &["Platform"]).await.unwrap();
    metadata::set_title_franchises(pool, title_id, "igdb", &["Fixture"]).await.unwrap();
    metadata::set_title_companies(pool, title_id, "igdb", &[("Fixture Publishing", metadata::ROLE_PUBLISHER), ("Fixture Dev", metadata::ROLE_DEVELOPER)]).await.unwrap();
    metadata::set_title_category(pool, title_id, "port", Some(title_id)).await.unwrap();
    let title = api::catalog::title(app, title_id).await.unwrap();
    assert_eq!(title.genres.len(), 2, "same genre from two sources = two tagged rows: {:?}", title.genres);
    assert_eq!(title.franchises.len(), 1);
    assert_eq!(title.companies.len(), 3, "hasheous publisher + igdb publisher + igdb developer");
    assert_eq!(title.category, "port");
    assert_eq!(title.basegame_title_id, None, "a title is never its own base game");
    // Provider links resolve back to titles for metadata application.
    cache::link_title(pool, title_id, Provider::Igdb, "424242", Confidence::Fuzzy).await.unwrap();
    assert_eq!(metadata::titles_linked(pool, Provider::Igdb, "424242").await.unwrap(), vec![title_id]);
    cache::link_release(pool, release_id, Provider::RetroAchievements, "777", Confidence::Exact).await.unwrap();
    assert_eq!(metadata::titles_linked(pool, Provider::RetroAchievements, "777").await.unwrap(), vec![title_id]);

    // A cached lookup rebuilds its record (name, signature game, cross refs) from the raw file.
    let raw_dir = base.join("hasheous_raw");
    std::fs::create_dir_all(&raw_dir).unwrap();
    let raw_path = raw_dir.join(format!("{content_id}.json"));
    std::fs::write(&raw_path, serde_json::to_string(&json!({"id": 1, "name": "Fixture Quest", "platform": "NES", "publisher": "Fixture Publishing", "signature": {"game": {"name": "Fixture Quest", "year": "1991", "publisher": "Fixture Publishing", "system": "NES"}}, "metadata": [{"source": "IGDB", "id": "424242"}, {"source": "RetroAchievements", "id": ""}, {"source": "Unknown", "id": "1"}]})).unwrap()).unwrap();
    sqlx::query("INSERT INTO hasheous_lookups (content_id, raw_response_path, raw_format, hit, fetched_at, deleted) VALUES (?, ?, 'json', TRUE, NOW(), FALSE)").bind(content_id).bind(raw_path.display().to_string()).execute(pool).await.unwrap();
    let cached = cache::hasheous_record_from_cache(pool, content_id).await.unwrap().expect("cached row");
    assert!(cached.hit);
    assert_eq!(cached.game.as_ref().and_then(|g| g.system.clone()).as_deref(), Some("NES"));
    assert_eq!(cached.cross_refs.len(), 1, "empty and unknown-source ids are dropped: {:?}", cached.cross_refs);
    assert_eq!(cached.cross_refs[0].0, Provider::Igdb);
    assert!(cache::hasheous_record_from_cache(pool, content_id + 1000).await.unwrap().is_none());

    // Manual match: link by hand (any tier), unlink, and the release page carries the
    // title-tier links. RA is release-tier: linking it to the title links its release.
    let detail = api::links::link_title(app, title_id, &api::links::LinkSpec { provider: "retroachievements".into(), provider_id: "9001".into() }).await.unwrap();
    assert!(detail.provider_links.iter().any(|l| l.provider == "retroachievements" && l.provider_id == "9001" && l.via == "release" && l.confidence == "manual"));
    let rel = api::catalog::release(app, release_id).await.unwrap();
    assert!(rel.provider_links.iter().any(|l| l.provider == "igdb" && l.via == "title"), "release page lists the title's providers: {:?}", rel.provider_links);
    assert!(rel.provider_links.iter().any(|l| l.provider == "retroachievements" && l.via == "release"));
    let rel = api::links::link_release(app, release_id, &api::links::LinkSpec { provider: "mobygames".into(), provider_id: "77".into() }).await.unwrap();
    assert!(rel.provider_links.iter().any(|l| l.provider == "mobygames" && l.via == "title"), "title-tier provider linked through the release attaches to its title");
    assert!(matches!(api::links::link_title(app, title_id, &api::links::LinkSpec { provider: "nope".into(), provider_id: "1".into() }).await, Err(e) if e.kind == ErrorKind::Invalid));
    assert!(matches!(api::links::link_title(app, 999_999, &api::links::LinkSpec { provider: "igdb".into(), provider_id: "1".into() }).await, Err(e) if e.kind == ErrorKind::NotFound));
    let detail = api::links::unlink_title(app, title_id, "mobygames").await.unwrap();
    assert!(!detail.provider_links.iter().any(|l| l.provider == "mobygames"));
    let rel = api::links::unlink_release(app, release_id, "retroachievements").await.unwrap();
    assert!(!rel.provider_links.iter().any(|l| l.provider == "retroachievements"));
    // Hash-derived and exact-name links carry their own confidence levels.
    cache::link_title(pool, title_id, Provider::ScreenScraper, "555", Confidence::Hash).await.unwrap();
    cache::link_title(pool, title_id, Provider::SteamGridDb, "556", Confidence::Name).await.unwrap();
    let detail = api::catalog::title(app, title_id).await.unwrap();
    assert!(detail.provider_links.iter().any(|l| l.provider == "screenscraper" && l.confidence == "hash"));
    assert!(detail.provider_links.iter().any(|l| l.provider == "steamgriddb" && l.confidence == "name"));
    // Page refresh = interactive-priority jobs, deduplicated while pending, in :fast lanes
    // that the scheduler orders ahead of background work. (No provider credentials in the
    // test env, so only providers that need none can produce jobs; the ordering is what's
    // checked here.)
    let refresh = api::links::refresh_title(app, title_id).await.unwrap();
    assert_eq!(refresh.title_id, title_id);
    let again = api::links::refresh_title(app, title_id).await.unwrap();
    assert!(again.jobs.is_empty(), "second refresh queues nothing while the first is pending: {:?}", again);
    assert!(matches!(api::links::refresh_title(app, 999_999).await, Err(e) if e.kind == ErrorKind::NotFound));
    let background = db_jobs::enqueue(pool, JobType::DbOptimize, json!({})).await.unwrap();
    let urgent = db_jobs::enqueue_with_priority(pool, JobType::DbOptimize, json!({"why": "test"}), api::jobs::PRIORITY_INTERACTIVE).await.unwrap();
    let pending = db_jobs::peek_pending(pool, 50).await.unwrap();
    let pos = |id: u64| pending.iter().position(|j| j.id == id).unwrap();
    assert!(pos(urgent) < pos(background), "priority jobs come first");
    assert_eq!(pending[pos(urgent)].lane, "cpu:fast");
    assert_eq!(pending[pos(background)].lane, "cpu");
    assert_eq!(pending[pos(urgent)].priority, api::jobs::PRIORITY_INTERACTIVE);
    // The scheduler sees the head of every lane group however deep another lane's queue is.
    let igdb: Vec<u64> = enqueue_igdb_jobs(pool, 3).await;
    let heads = db_jobs::peek_lane_heads(pool).await.unwrap();
    let head_ids: Vec<u64> = heads.iter().map(|j| j.id).collect();
    assert!(head_ids.contains(&igdb[0]) && !head_ids.contains(&igdb[1]) && !head_ids.contains(&igdb[2]), "one head per lane group: {head_ids:?}");
    assert!(heads.iter().any(|j| j.lane == "cpu:fast") && heads.iter().any(|j| j.lane == "cpu"), "idle lanes still visible: {:?}", heads.iter().map(|j| &j.lane).collect::<Vec<_>>());
    assert!(heads.iter().position(|j| j.lane == "cpu:fast") < heads.iter().position(|j| j.lane == "provider:igdb"), "priority first");
    for id in igdb {
        db_jobs::cancel(pool, id).await.unwrap();
    }

    // Schedules: validation, CRUD, run-now never double-enqueues.
    let spec = |v: serde_json::Value| serde_json::from_value::<romexplorer::database::schedules::ScheduleSpec>(v).unwrap();
    let bad = spec(json!({"name": "x", "target": {"job_type": "file_scan", "directory_id": 1}, "trigger": "fixed", "fixed_every_n": 1, "fixed_unit": "days"}));
    assert!(matches!(api::schedules::create(app, &bad).await, Err(e) if e.kind == ErrorKind::Invalid), "file_scan isn't schedulable");
    // "Download missing media" runs on a timer; its scope is checked like a job's.
    let media = spec(json!({"name": "nightly art", "target": {"job_type": "media_backfill", "scope": "owned", "platform": null}, "trigger": "fixed", "fixed_every_n": 1, "fixed_unit": "days"}));
    let media = api::schedules::create(app, &media).await.unwrap();
    api::schedules::delete(app, media.id).await.unwrap();
    // RA badges: the scope is checked first, then the account (none in this test).
    let ra_badges = |scope: &str| api::jobs::JobRequest::RaBadges(api::ra::RaBadgesRequest { scope: scope.into() });
    assert!(matches!(api::jobs::enqueue(app, &ra_badges("everything")).await, Err(e) if e.kind == ErrorKind::Invalid));
    assert!(matches!(api::jobs::enqueue(app, &ra_badges("played")).await, Err(e) if e.kind == ErrorKind::Unavailable));
    assert_eq!(db_jobs::lane_of(JobType::RaBadges, &json!({"scope": "played"})), "provider:retroachievements");
    let bad_scope = spec(json!({"name": "x", "target": {"job_type": "media_backfill", "scope": "mine"}, "trigger": "fixed", "fixed_every_n": 1, "fixed_unit": "days"}));
    assert!(matches!(api::schedules::create(app, &bad_scope).await, Err(e) if e.kind == ErrorKind::Invalid));
    let no_interval = spec(json!({"name": "x", "target": {"job_type": "db_optimize"}, "trigger": "both", "fixed_every_n": 1, "fixed_unit": "weeks"}));
    assert!(matches!(api::schedules::create(app, &no_interval).await, Err(e) if e.kind == ErrorKind::Invalid), "both needs a check interval too");
    let weekly = spec(json!({"name": "weekly optimize", "target": {"job_type": "db_optimize"}, "trigger": "fixed", "fixed_every_n": 1, "fixed_unit": "weeks"}));
    let created = api::schedules::create(app, &weekly).await.unwrap();
    assert_eq!(created.spec.name, "weekly optimize");
    assert!(created.next_fixed_at.is_some() && created.next_check_at.is_none());
    let mut edited = weekly.clone();
    edited.fixed_every_n = Some(2);
    edited.enabled = false;
    let updated = api::schedules::update(app, created.id, &edited).await.unwrap();
    assert_eq!((updated.spec.fixed_every_n, updated.spec.enabled), (Some(2), false));
    assert!(api::schedules::list(app).await.unwrap().iter().any(|x| x.id == created.id));
    assert_eq!(api::schedules::run_now(app, created.id).await.unwrap(), None, "an identical db_optimize is already pending");
    db_jobs::cancel(pool, background).await.unwrap();
    let first = api::schedules::run_now(app, created.id).await.unwrap();
    assert!(first.is_some());
    assert_eq!(api::schedules::run_now(app, created.id).await.unwrap(), None, "same job still pending: not enqueued twice");
    db_jobs::cancel(pool, first.unwrap()).await.unwrap();
    api::schedules::delete(app, created.id).await.unwrap();
    assert!(matches!(api::schedules::get(app, created.id).await, Err(e) if e.kind == ErrorKind::NotFound));
    // A name search needs a searchable provider and a name.
    assert!(matches!(api::links::search(app, "retroachievements", "x", None).await, Err(e) if e.kind == ErrorKind::Invalid));
    assert!(matches!(api::links::search(app, "igdb", "  ", None).await, Err(e) if e.kind == ErrorKind::Invalid));

    // Region-priority cover: a release image represents the title; the best-priority
    // region wins, and title-level media is only the fallback.
    let img = base.join("cover_fixture.png");
    std::fs::write(&img, b"not really a png").unwrap();
    let media_id = sqlx::query("INSERT INTO media (media_type, sub_category, path, source, source_url) VALUES ('box', 'front', ?, 'igdb', 'fixture://cover')").bind(img.display().to_string()).execute(pool).await.unwrap().last_insert_id();
    sqlx::query("INSERT INTO release_media (release_id, media_id) VALUES (?, ?)").bind(release_id).bind(media_id).execute(pool).await.unwrap();
    api::settings::set(app, "region_priority", json!(["JP", "US"])).await.unwrap();
    let detail = api::catalog::title(app, title_id).await.unwrap();
    assert_eq!(detail.cover_media_id, Some(media_id));
    assert_eq!(api::catalog::titles(app, &api::catalog::ListQuery { search: Some("Fixture".into()), ..Default::default() }).await.unwrap().items.iter().find(|t| t.id == title_id).unwrap().cover_media_id, Some(media_id));
    assert!(matches!(api::settings::set(app, "region_priority", json!(["XX"])).await, Err(e) if e.kind == ErrorKind::Invalid));
    assert!(matches!(api::settings::set(app, "language_priority", json!(["xx"])).await, Err(e) if e.kind == ErrorKind::Invalid));
    api::settings::set(app, "language_priority", json!(["ja", "en"])).await.unwrap();
    // Release rank within the title: a second release of the same title, JP region, ranks
    // first under region priority JP > US; RetroAchievements support only breaks ties; the
    // user's manual order beats everything.
    let source_id: u8 = sqlx::query("SELECT source_id FROM `release` WHERE id = ?").bind(release_id).fetch_one(pool).await.unwrap().try_get("source_id").unwrap();
    let jp_release = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, languages, source_id) VALUES (?, 'Fixture Quest (Japan)', 'jp-hash', 'fixture quest', 'x', 'JP', 'ja', ?)").bind(Platform::Nes.igdb_id()).bind(source_id).execute(pool).await.unwrap().last_insert_id();
    sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(title_id).bind(jp_release).execute(pool).await.unwrap();
    // A DAT lists it (as an import records): a commercial release, ranked with the others.
    sqlx::query("INSERT INTO release_sources (release_id, source_id) VALUES (?, ?)").bind(jp_release).bind(source_id).execute(pool).await.unwrap();
    sqlx::query("UPDATE `release` SET region = 'US', languages = 'en' WHERE id = ?").bind(release_id).execute(pool).await.unwrap();
    let ranked = |items: &[api::catalog::ReleaseRow]| items.iter().map(|r| r.id).collect::<Vec<_>>();
    let page = api::catalog::releases(app, &api::catalog::ListQuery { title_id: Some(title_id), ..Default::default() }).await.unwrap();
    assert_eq!(ranked(&page.items), vec![jp_release, release_id], "JP first under region priority JP > US");
    api::settings::set(app, "region_priority", json!(["US", "JP"])).await.unwrap();
    let page = api::catalog::releases(app, &api::catalog::ListQuery { title_id: Some(title_id), ..Default::default() }).await.unwrap();
    assert_eq!(ranked(&page.items), vec![release_id, jp_release], "US first under region priority US > JP");
    // RA support doesn't override the region priority: the JP release is in RA's hash list,
    // US still comes first under US > JP.
    let jp_content = sqlx::query("INSERT INTO content (app_platform_id, file_name, size, partial) VALUES (?, 'fixture-jp.bin', 4096, FALSE)").bind(Platform::Nes.igdb_id()).execute(pool).await.unwrap().last_insert_id();
    sqlx::query("INSERT INTO release_contents (release_id, content_id, file_name) VALUES (?, ?, 'fixture-jp.bin')").bind(jp_release).bind(jp_content).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO hashes (content_id, algorithm, value_hex) VALUES (?, 'md5', 'ffffffffffffffffffffffffffffffff')").bind(jp_content).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO retroachievements_hashes (md5, ra_game_id, console_id) VALUES ('ffffffffffffffffffffffffffffffff', 424242, 7)").execute(pool).await.unwrap();
    let page = api::catalog::releases(app, &api::catalog::ListQuery { title_id: Some(title_id), ..Default::default() }).await.unwrap();
    assert_eq!(ranked(&page.items), vec![release_id, jp_release], "region priority beats RA support");
    assert_eq!(api::catalog::title(app, title_id).await.unwrap().releases.first().map(|r| r.id), Some(release_id));
    // Manual order: the JP release dragged to the top wins; resetting restores the default.
    let order = api::titles::move_release(app, title_id, &api::titles::ReleaseMoveRequest { release_id: jp_release, after_release_id: None }).await.unwrap();
    assert_eq!(order, vec![jp_release, release_id]);
    let detail = api::catalog::title(app, title_id).await.unwrap();
    assert!(detail.manual_order);
    assert_eq!(detail.releases.first().map(|r| r.id), Some(jp_release), "manual order beats region priority");
    assert_eq!(api::titles::release_orders(app, &Default::default()).await.unwrap().items.iter().filter(|o| o.title_id == title_id).count(), 1);
    assert!(api::titles::move_release(app, title_id, &api::titles::ReleaseMoveRequest { release_id: 999_999_999, after_release_id: None }).await.is_err());
    api::titles::clear_release_orders(app, &api::titles::ReleaseOrderClear { title_ids: Some(vec![title_id]), all: None }).await.unwrap();
    assert_eq!(api::catalog::title(app, title_id).await.unwrap().releases.first().map(|r| r.id), Some(release_id), "reset: back to region priority");
    // Explicit sort still wins; `rank` is a sort key anywhere.
    let page = api::catalog::releases(app, &api::catalog::ListQuery { title_id: Some(title_id), sort: Some("name".into()), ..Default::default() }).await.unwrap();
    assert_eq!(ranked(&page.items), vec![release_id, jp_release]);
    assert!(api::catalog::releases(app, &api::catalog::ListQuery { sort: Some("rank".into()), limit: Some(5), ..Default::default() }).await.is_ok());
    // Franchise / keyword media targets exist for audio.
    let franchise_id: u64 = sqlx::query("SELECT id FROM franchise WHERE name = 'Fixture'").fetch_one(pool).await.unwrap().try_get("id").unwrap();
    sqlx::query("INSERT INTO keyword (name) VALUES ('fixture-series')").execute(pool).await.unwrap();
    sqlx::query("INSERT INTO franchise_media (franchise_id, media_id) VALUES (?, ?)").bind(franchise_id).bind(media_id).execute(pool).await.unwrap();
    assert!(romexplorer::database::providers::media::has_media(pool, romexplorer::database::providers::media::MediaTarget::Franchise(franchise_id), romexplorer::database::providers::Provider::Igdb, "box", Some("front")).await.unwrap());

    // DAT source counts: what landed in the DB per source (nothing imported here).
    let sources = api::dats::sources(app).await.unwrap();
    assert!(sources.iter().all(|s| s.releases_in_db == 0 && s.titles_in_db == 0 && s.content_in_db == 0), "no DAT imported in this test");
    // The catalog view sees the built title as a real title (not local, no local game).
    assert!(api::catalog::titles(app, &api::catalog::ListQuery { search: Some("Fixture".into()), ..Default::default() }).await.unwrap().items.iter().any(|t| t.id == title_id));
}

async fn rest(app: App, directory_id: u64) {
    let token = "t".repeat(64);
    let lifecycle = romexplorer::server::Lifecycle::new(false);
    // `fetch_loopback` lets the server fetch from a loopback "provider" for the media
    // scenario; requests come from loopback anyway, so nothing depends on `allow_public_ips`.
    let state = Arc::new(ServerState { app, setup_token: Some(token.clone()), in_process: false, allow_public_ips: true, fetch_loopback: true, lifecycle: lifecycle.clone(), tls_fingerprint: None });
    let shared = state.clone();
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel::<SocketAddr>();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        let options = ServeOptions { bind: "127.0.0.1".parse().unwrap(), port: 0, tls: None };
        server::serve(state, &options, |a| { let _ = addr_tx.send(a); }, async { let _ = stop_rx.await; }).await.unwrap();
    });
    let addr = addr_rx.await.unwrap();
    let base = format!("http://{addr}");
    let http = reqwest::Client::new();

    let ping = http.get(format!("{base}/api/v1/ping")).send().await.unwrap();
    assert_eq!(ping.status(), 200, "ping is token-exempt");
    let page = http.get(format!("{base}/")).send().await.unwrap();
    assert_eq!(page.status(), 200);
    assert!(page.text().await.unwrap().contains("romexplorer console"));
    assert_eq!(http.get(format!("{base}/api/v1/status")).send().await.unwrap().status(), 401, "no token");
    assert_eq!(http.get(format!("{base}/api/v1/status")).bearer_auth("wrong").send().await.unwrap().status(), 401, "wrong token");

    let status: serde_json::Value = http.get(format!("{base}/api/v1/status")).bearer_auth(&token).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    assert_eq!(status["database"]["database"], "romexplorer_api_test");

    let created = http.post(format!("{base}/api/v1/jobs")).bearer_auth(&token).json(&json!({"job_type": "file_scan", "directory_id": directory_id, "mode": "full", "platform": "all"})).send().await.unwrap();
    assert_eq!(created.status(), 201);
    let body: serde_json::Value = created.json().await.unwrap();
    assert_eq!(body["job_type"], "file_scan");

    let bad = http.post(format!("{base}/api/v1/jobs")).bearer_auth(&token).json(&json!({"job_type": "file_scan", "directory_id": 99999, "mode": "full"})).send().await.unwrap();
    assert_eq!(bad.status(), 404);
    let err: serde_json::Value = bad.json().await.unwrap();
    assert_eq!(err["error"]["kind"], "not_found");

    assert_eq!(http.get(format!("{base}/api/v1/db/tables/nope/rows")).bearer_auth(&token).send().await.unwrap().status(), 404);
    let rows: serde_json::Value = http.get(format!("{base}/api/v1/db/tables/platforms/rows?limit=3&sort=id")).bearer_auth(&token).send().await.unwrap().json().await.unwrap();
    assert_eq!(rows["rows"].as_array().unwrap().len(), 3);
    // Lifecycle: the endpoints only record a request (the owning binary acts on it); an
    // unauthenticated one is refused like any other route.
    let info: serde_json::Value = http.get(format!("{base}/api/v1/server")).bearer_auth(&token).send().await.unwrap().json().await.unwrap();
    assert_eq!(info["pid"].as_u64(), Some(std::process::id() as u64));
    assert_eq!(http.post(format!("{base}/api/v1/server/shutdown")).send().await.unwrap().status(), 401);
    assert_eq!(http.post(format!("{base}/api/v1/server/restart")).bearer_auth(&token).send().await.unwrap().status(), 202);
    let requested = tokio::time::timeout(std::time::Duration::from_secs(2), lifecycle.requested()).await.expect("restart recorded");
    assert_eq!(requested, romexplorer::server::LifecycleRequest::Restart);

    let dirs: serde_json::Value = http.get(format!("{base}/api/v1/directories")).bearer_auth(&token).send().await.unwrap().json().await.unwrap();
    assert_eq!(dirs.as_array().unwrap().len(), 1);
    // A root with a configured child: plain DELETE is a 409, ?recursive=true removes both.
    let scratch = output_dir("rest_root");
    std::fs::create_dir_all(scratch.join("child")).unwrap();
    let made: serde_json::Value = http.post(format!("{base}/api/v1/directories")).bearer_auth(&token).json(&json!({ "path": scratch.display().to_string(), "is_collection_root": true })).send().await.unwrap().json().await.unwrap();
    let child = http.post(format!("{base}/api/v1/directories")).bearer_auth(&token).json(&json!({ "path": scratch.join("child").display().to_string() })).send().await.unwrap();
    assert_eq!(child.status(), 200);
    let root_id = made["id"].as_u64().unwrap();
    assert_eq!(http.delete(format!("{base}/api/v1/directories/{root_id}")).bearer_auth(&token).send().await.unwrap().status(), 409);
    let gone: serde_json::Value = http.delete(format!("{base}/api/v1/directories/{root_id}?recursive=true")).bearer_auth(&token).send().await.unwrap().json().await.unwrap();
    assert_eq!(gone["removed"].as_array().unwrap().len(), 2);
    assert!(scratch.join("child").is_dir(), "config removal never touches disk");
    let dirs: serde_json::Value = http.get(format!("{base}/api/v1/directories")).bearer_auth(&token).send().await.unwrap().json().await.unwrap();
    assert_eq!(dirs.as_array().unwrap().len(), 1);
    let put = http.put(format!("{base}/api/v1/settings/cache_ttl_days")).bearer_auth(&token).json(&json!(7)).send().await.unwrap();
    assert_eq!(put.status(), 200);
    let vocab: serde_json::Value = http.get(format!("{base}/api/v1/vocabulary")).bearer_auth(&token).send().await.unwrap().json().await.unwrap();
    assert!(vocab["jobs"]["scan_modes"].as_array().unwrap().iter().any(|m| m == "new_only"));
    assert!(vocab["languages"].as_array().unwrap().iter().any(|l| l["code"] == "ja" && l["native"] == "日本語"));
    assert!(vocab["regions"].as_array().unwrap().iter().any(|r| r["code"] == "JP"));
    // Media settings: what the editors draw from, the shape checks, and the lists still
    // answering with a provider order in force (the order is part of every cover query).
    assert!(vocab["media"]["providers"].as_array().unwrap().iter().any(|p| p["name"] == "igdb" && p["types"].as_array().unwrap().iter().any(|t| t == "box")));
    assert!(vocab["media"]["single_types"].as_array().unwrap().iter().any(|t| t == "box"));
    assert_eq!(vocab["media"]["default_on"], json!(["box", "cover"]), "screenshots and the rest are opt-in");
    assert!(vocab["media"]["providers"].as_array().unwrap().iter().any(|p| p["name"] == "igdb" && p["types"].as_array().unwrap().iter().any(|t| t == "screenshot")));
    assert_eq!(http.put(format!("{base}/api/v1/settings/media_list_cap")).bearer_auth(&token).json(&json!(0)).send().await.unwrap().status(), 400);
    assert_eq!(http.put(format!("{base}/api/v1/settings/media_list_cap")).bearer_auth(&token).json(&json!(4)).send().await.unwrap().status(), 200);
    let put_setting = |key: &'static str, value: serde_json::Value| http.put(format!("{base}/api/v1/settings/{key}")).bearer_auth(&token).json(&value).send();
    assert_eq!(put_setting("media_download", json!({"nope": {"box": true}})).await.unwrap().status(), 400);
    assert_eq!(put_setting("media_priority", json!({"bgm": ["igdb"]})).await.unwrap().status(), 400, "an order only for images and video");
    assert_eq!(put_setting("media_show", json!({"box": "yes"})).await.unwrap().status(), 400);
    assert_eq!(put_setting("media_download", json!({"igdb": {"box": false}})).await.unwrap().status(), 200);
    // Your uploads ("manual") placed in the order, and a list type ordered for display; the
    // cover queries below run with both in force.
    assert_eq!(put_setting("media_priority", json!({"box": ["mobygames", "manual", "igdb"], "screenshot": ["igdb", "manual"]})).await.unwrap().status(), 200);
    assert_eq!(put_setting("media_client_keep", json!({"video": true})).await.unwrap().status(), 200);
    for list in ["titles", "releases", "games"] {
        assert_eq!(http.get(format!("{base}/api/v1/catalog/{list}?limit=5")).bearer_auth(&token).send().await.unwrap().status(), 200, "{list} with media_priority set");
    }
    assert_eq!(http.get(format!("{base}/api/v1/media/strip")).bearer_auth(&token).send().await.unwrap().status(), 400);
    let strip: serde_json::Value = http.get(format!("{base}/api/v1/media/strip?title_id=1")).bearer_auth(&token).send().await.unwrap().json().await.unwrap();
    assert!(strip.is_array());
    // Streaming is from the main server, and this instance has none.
    assert_eq!(http.get(format!("{base}/api/v1/media/remote/1")).bearer_auth(&token).send().await.unwrap().status(), 404);
    assert_eq!(http.get(format!("{base}/api/v1/audio/remote/1/file")).bearer_auth(&token).send().await.unwrap().status(), 404);
    // A token in the query string only where a header can't go (<img src>); media 404s cleanly.
    assert_eq!(http.get(format!("{base}/api/v1/status?token={token}")).send().await.unwrap().status(), 401, "not accepted on a JSON route");
    assert_eq!(http.get(format!("{base}/api/v1/media/999999?token={token}")).send().await.unwrap().status(), 404);
    assert_eq!(http.get(format!("{base}/api/v1/media/999999")).send().await.unwrap().status(), 401);
    // Manual match routes: bad provider -> 400, unknown title -> 404, search without a name -> 400.
    assert_eq!(http.post(format!("{base}/api/v1/catalog/titles/1/links")).bearer_auth(&token).json(&json!({"provider": "nope", "provider_id": "1"})).send().await.unwrap().status(), 400);
    assert_eq!(http.post(format!("{base}/api/v1/catalog/titles/999999/links")).bearer_auth(&token).json(&json!({"provider": "igdb", "provider_id": "1"})).send().await.unwrap().status(), 404);
    assert_eq!(http.get(format!("{base}/api/v1/providers/igdb/search?name=")).bearer_auth(&token).send().await.unwrap().status(), 400);
    assert_eq!(http.get(format!("{base}/api/v1/providers/retroachievements/search?name=x")).bearer_auth(&token).send().await.unwrap().status(), 400);
    let created = http.post(format!("{base}/api/v1/sets")).bearer_auth(&token).json(&json!({"name": "via rest"})).send().await.unwrap();
    assert_eq!(created.status(), 201);
    let sets: serde_json::Value = http.get(format!("{base}/api/v1/sets")).bearer_auth(&token).send().await.unwrap().json().await.unwrap();
    assert_eq!(sets.as_array().unwrap().len(), 1);

    // Before the accounts scenario: creating an admin retires the setup token the client
    // below signs in with.
    media_from_the_server(&shared.app, &http, &base, &token).await;

    accounts_over_rest(&shared.app, &http, &base, &token, directory_id).await;

    let _ = stop_tx.send(());
    handle.await.unwrap();
}

/// "Download missing media": the art the settings enable that linked releases lack, from a
/// provider entry's cached response, release by release -- owned releases only unless the
/// whole catalogue is asked for, a release that already has it left alone, and a provider
/// with no credentials counted rather than failing the run.
async fn media_backfill(app: &App, base: &PathBuf) {
    use romexplorer::common::models::Platform;
    use romexplorer::database::jobs::Progress;
    use romexplorer::database::media_store::MediaTarget;
    use romexplorer::database::providers::media as pmedia;

    let front: Vec<u8> = b"\x89PNG\r\n\x1a\n backfill".iter().chain(std::iter::repeat(&5u8).take(2000)).copied().collect();
    let older: Vec<u8> = b"\x89PNG\r\n\x1a\n older".iter().chain(std::iter::repeat(&6u8).take(2000)).copied().collect();
    let (host, host_handle) = start_art_host(front.clone(), older, b"\x89PNG\r\n\x1a\n unused".to_vec()).await;
    let platform = Platform::GameBoyAdvance.igdb_id();

    let title_id = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES ('Backfill Test', 'backfill test', SHA1('backfill test'))").execute(&app.pool).await.unwrap().last_insert_id();
    let mut releases = Vec::new();
    // Regions of their own: releases sharing one form one art group and share its files.
    for region in ["USA", "Europe", "Japan"] {
        let name = format!("Backfill Test ({region})");
        let id = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region) VALUES (?, ?, SHA1(?), 'backfill test', SHA1('backfill test'), ?)")
            .bind(platform)
            .bind(&name)
            .bind(&name)
            .bind(region)
            .execute(&app.pool)
            .await
            .unwrap()
            .last_insert_id();
        sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(title_id).bind(id).execute(&app.pool).await.unwrap();
        sqlx::query("INSERT INTO release_provider_links (release_id, provider, provider_id, confidence) VALUES (?, 'retroachievements', '90777', 'exact')").bind(id).execute(&app.pool).await.unwrap();
        releases.push(id);
    }
    let (usa, europe, japan) = (releases[0], releases[1], releases[2]);
    // USA and Europe are owned; Japan is only in the catalogue.
    for release in [usa, europe] {
        let game = sqlx::query("INSERT INTO local_game (name) VALUES ('backfill')").execute(&app.pool).await.unwrap().last_insert_id();
        sqlx::query("INSERT INTO local_release_games (release_id, game_id) VALUES (?, ?)").bind(release).bind(game).execute(&app.pool).await.unwrap();
    }
    // A SteamGridDB link for the title: covers are on by default, and the test has no key.
    sqlx::query("INSERT INTO title_provider_links (title_id, provider, provider_id, confidence) VALUES (?, 'steamgriddb', '555', 'name')").bind(title_id).execute(&app.pool).await.unwrap();
    // RetroAchievements' answer for game 90777, cached as a by-id fetch leaves it.
    let raw = base.join("ra_90777.json");
    std::fs::write(&raw, json!({"ID": 90777, "Title": "Backfill Test", "ConsoleID": 5, "ImageBoxArt": format!("http://{host}/art.png")}).to_string()).unwrap();
    sqlx::query("INSERT INTO retroachievements_games (provider_id, console_id, raw_response_path, title) VALUES (90777, 5, ?, 'Backfill Test')").bind(raw.to_str().unwrap()).execute(&app.pool).await.unwrap();
    // Europe already has RetroAchievements box art (an older file).
    pmedia::store_url(&app.http, &app.pool, &app.paths.media_dir(), "retroachievements", Some("90777"), &format!("http://{host}/back.png"), "box", Some("front"), &[MediaTarget::Release(europe)]).await.unwrap();

    let box_url = |release: u64| async move {
        sqlx::query_scalar::<_, String>("SELECT m.source_url FROM media m JOIN release_media rm ON rm.media_id = m.id WHERE rm.release_id = ? AND m.source = 'retroachievements' AND m.media_type = 'box' AND m.deleted = FALSE")
            .bind(release)
            .fetch_all(&app.pool)
            .await
            .unwrap()
    };
    let request = |scope: &str| api::media_backfill::MediaBackfillRequest { scope: scope.into(), platform: Some(platform), ..Default::default() };
    assert!(matches!(api::jobs::enqueue(app, &api::jobs::JobRequest::MediaBackfill(request("mine"))).await, Err(e) if e.kind == ErrorKind::Invalid));

    let owned = api::media_backfill::run(app, &request("owned"), &Progress::none()).await.unwrap();
    assert_eq!((owned.releases, owned.entries, owned.downloaded), (2, 2, 1), "{owned:?}");
    assert_eq!(owned.unavailable.get("steamgriddb"), Some(&1), "{owned:?}");
    assert!(owned.errors.is_empty(), "{owned:?}");
    assert_eq!(box_url(usa).await, vec![format!("http://{host}/art.png")], "the owned release without art got it");
    assert_eq!(box_url(europe).await, vec![format!("http://{host}/back.png")], "the one that had art kept it");
    assert!(box_url(japan).await.is_empty(), "not owned: left for a whole-catalogue run");

    let again = api::media_backfill::run(app, &request("owned"), &Progress::none()).await.unwrap();
    assert_eq!((again.downloaded, again.already), (0, 1), "nothing left to fetch: {again:?}");

    let all = api::media_backfill::run(app, &request("all"), &Progress::none()).await.unwrap();
    assert_eq!((all.releases, all.downloaded), (3, 1), "{all:?}");
    assert_eq!(box_url(japan).await, vec![format!("http://{host}/art.png")]);
    let id: u64 = sqlx::query_scalar("SELECT m.id FROM media m JOIN release_media rm ON rm.media_id = m.id WHERE rm.release_id = ? AND m.source = 'retroachievements'").bind(japan).fetch_one(&app.pool).await.unwrap();
    let (path, _) = api::media::file(app, id, false, None).await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), front);

    // What counts as "already there" is per slot: a box back is not a box front, and one
    // screenshot is not the whole list once the cap is higher.
    use romexplorer::database::providers::Provider;
    use romexplorer::database::settings as db_settings;
    let usa_t = MediaTarget::Release(usa);
    let rules = db_settings::MediaRules::load(&app.pool).await.unwrap();
    let ra = Provider::RetroAchievements;
    pmedia::store_url(&app.http, &app.pool, &app.paths.media_dir(), "igdb", Some("1"), &format!("http://{host}/shot.png"), "box", Some("back"), &[usa_t]).await.unwrap();
    assert!(pmedia::wants(&app.pool, &rules, Provider::Igdb, "box", Some("front"), usa_t, 1).await.unwrap(), "IGDB's box back does not stand for its front");
    assert!(!pmedia::wants(&app.pool, &rules, Provider::Igdb, "box", Some("back"), usa_t, 1).await.unwrap());
    pmedia::store_url(&app.http, &app.pool, &app.paths.media_dir(), "retroachievements", Some("90777"), &format!("http://{host}/shot.png"), "screenshot", None, &[usa_t]).await.unwrap();
    assert!(!pmedia::wants(&app.pool, &rules, ra, "screenshot", None, usa_t, 1).await.unwrap());
    assert!(pmedia::wants(&app.pool, &rules, ra, "screenshot", None, usa_t, 3).await.unwrap(), "a higher cap is topped up");

    // Your own upload outranks every provider unless media_priority places it lower.
    api::media::upload(app, &api::media::SlotTarget { title_id: None, release_id: Some(japan) }, "cover", None, "mine.png", b"\x89PNG\r\n\x1a\n mine", None).await.unwrap();
    let japan_t = MediaTarget::Release(japan);
    let sgdb = Provider::SteamGridDb;
    assert!(!pmedia::wants(&app.pool, &rules, sgdb, "cover", None, japan_t, 1).await.unwrap(), "uploads come first by default");
    api::settings::set(app, "media_priority", json!({"cover": ["steamgriddb", "manual"], "screenshot": ["manual", "retroachievements"]})).await.unwrap();
    let rules = db_settings::MediaRules::load(&app.pool).await.unwrap();
    assert!(pmedia::wants(&app.pool, &rules, sgdb, "cover", None, japan_t, 1).await.unwrap(), "a provider placed above uploads still downloads");
    assert!(!pmedia::wants(&app.pool, &rules, Provider::Igdb, "cover", None, japan_t, 1).await.unwrap(), "an unplaced provider ranks below the upload");
    assert!(rules.rank("steamgriddb", "cover") < rules.rank("manual", "cover"));
    assert!(rules.rank("manual", "box") < rules.rank("igdb", "box"), "unplaced uploads stay first");
    assert!(matches!(api::settings::set(app, "media_priority", json!({"cover": ["nobody"]})).await, Err(e) if e.kind == ErrorKind::Invalid));
    assert!(matches!(api::settings::set(app, "media_priority", json!({"bgm": ["manual"]})).await, Err(e) if e.kind == ErrorKind::Invalid));
    api::settings::delete(app, "media_priority").await.unwrap();
    host_handle.abort();
}

/// "Download art" on a selection: one provider, the types asked for and nothing else, for the
/// releases picked and no others -- fetched even where the settings would not have (a type
/// that is off, a higher-ranked provider that already has one), never over a user's own pick.
/// A manual-only provider is never run without chosen types.
async fn media_for_a_selection(app: &App, base: &PathBuf) {
    use romexplorer::common::models::Platform;
    use romexplorer::database::jobs::Progress;
    use romexplorer::database::media_store::MediaTarget;
    use romexplorer::database::providers::media as pmedia;
    use romexplorer::database::providers::Provider;
    use romexplorer_types::copy::CopySelection;

    let boxart: Vec<u8> = b"\x89PNG\r\n\x1a\n asked box".iter().chain(std::iter::repeat(&7u8).take(2000)).copied().collect();
    let title: Vec<u8> = b"\x89PNG\r\n\x1a\n asked title".iter().chain(std::iter::repeat(&8u8).take(2000)).copied().collect();
    let (host, host_handle) = start_art_host(boxart, title, b"\x89PNG\r\n\x1a\n igdb box".to_vec()).await;
    let platform = Platform::GameBoyAdvance.igdb_id();
    let title_id = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES ('Hoge Quest', 'hoge quest', SHA1('hoge quest'))").execute(&app.pool).await.unwrap().last_insert_id();
    let mut releases = Vec::new();
    for region in ["USA", "Japan"] {
        let name = format!("Hoge Quest ({region})");
        let id = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region) VALUES (?, ?, SHA1(?), 'hoge quest', SHA1('hoge quest'), ?)")
            .bind(platform)
            .bind(&name)
            .bind(&name)
            .bind(region)
            .execute(&app.pool)
            .await
            .unwrap()
            .last_insert_id();
        sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(title_id).bind(id).execute(&app.pool).await.unwrap();
        sqlx::query("INSERT INTO release_provider_links (release_id, provider, provider_id, confidence) VALUES (?, 'retroachievements', '90778', 'exact')").bind(id).execute(&app.pool).await.unwrap();
        releases.push(id);
    }
    let (usa, japan) = (releases[0], releases[1]);
    let raw = base.join("ra_90778.json");
    std::fs::write(&raw, json!({"ID": 90778, "Title": "Hoge Quest", "ConsoleID": 5, "ImageBoxArt": format!("http://{host}/art.png"), "ImageTitle": format!("http://{host}/back.png")}).to_string()).unwrap();
    sqlx::query("INSERT INTO retroachievements_games (provider_id, console_id, raw_response_path, title) VALUES (90778, 5, ?, 'Hoge Quest')").bind(raw.to_str().unwrap()).execute(&app.pool).await.unwrap();
    // IGDB already gave USA a box front, and is ranked above RetroAchievements: on its own,
    // RetroAchievements' box would never be fetched for it.
    pmedia::store_url(&app.http, &app.pool, &app.paths.media_dir(), "igdb", Some("2"), &format!("http://{host}/shot.png"), "box", Some("front"), &[MediaTarget::Release(usa)]).await.unwrap();
    api::settings::set(app, "media_priority", json!({"box": ["manual", "igdb", "retroachievements"]})).await.unwrap();

    let ra_media = |release: u64| async move {
        sqlx::query_scalar::<_, String>("SELECT CONCAT(m.media_type, ':', COALESCE(m.sub_category, '')) FROM media m JOIN release_media rm ON rm.media_id = m.id WHERE rm.release_id = ? AND m.source = 'retroachievements' AND m.deleted = FALSE ORDER BY 1")
            .bind(release)
            .fetch_all(&app.pool)
            .await
            .unwrap()
    };
    let asked = |types: &[&str]| api::media_backfill::MediaBackfillRequest {
        scope: "owned".into(),
        selection: Some(CopySelection { release_ids: vec![usa], ..Default::default() }),
        providers: vec!["retroachievements".into()],
        types: types.iter().map(|t| t.to_string()).collect(),
        ..Default::default()
    };

    // The settings alone fetch nothing for USA: IGDB's box outranks it, and title screens are off.
    let auto = api::media_backfill::MediaBackfillRequest { selection: asked(&[]).selection, providers: vec!["retroachievements".into()], scope: "owned".into(), ..Default::default() };
    api::media_backfill::run(app, &auto, &Progress::none()).await.unwrap();
    assert!(ra_media(usa).await.is_empty(), "on its own it leaves USA alone: {:?}", ra_media(usa).await);

    let run = api::media_backfill::run(app, &asked(&["title_screen"]), &Progress::none()).await.unwrap();
    assert_eq!((run.scope.as_str(), run.releases, run.downloaded), ("selection", 1, 1), "{run:?}");
    assert_eq!(ra_media(usa).await, vec!["title_screen:".to_string()], "only the type asked for, though title screens are off in the settings");
    assert!(ra_media(japan).await.is_empty(), "not selected");

    api::media_backfill::run(app, &asked(&["box:front", "title_screen"]), &Progress::none()).await.unwrap();
    assert_eq!(ra_media(usa).await, vec!["box:front".to_string(), "title_screen:".to_string()], "asked for, so fetched though IGDB's box ranks higher");
    let again = api::media_backfill::run(app, &asked(&["box:front", "title_screen"]), &Progress::none()).await.unwrap();
    assert_eq!(again.downloaded, 0, "what it already gave is not fetched twice: {again:?}");

    // A user's own pick is never downloaded over, asked or not.
    api::media::upload(app, &api::media::SlotTarget { title_id: None, release_id: Some(japan) }, "box", Some("front"), "mine.png", b"\x89PNG\r\n\x1a\n mine box", None).await.unwrap();
    let japan_run = api::media_backfill::MediaBackfillRequest { selection: Some(CopySelection { release_ids: vec![japan], ..Default::default() }), ..asked(&["box:front"]) };
    api::media_backfill::run(app, &japan_run, &Progress::none()).await.unwrap();
    assert!(ra_media(japan).await.is_empty(), "the upload stays the box: {:?}", ra_media(japan).await);

    // Requests that make no sense are refused before they run.
    let bad = |r: api::media_backfill::MediaBackfillRequest| async move { matches!(api::jobs::enqueue(app, &api::jobs::JobRequest::MediaBackfill(r)).await, Err(e) if e.kind == ErrorKind::Invalid) };
    assert!(bad(api::media_backfill::MediaBackfillRequest { providers: vec!["thegamesdb".into()], ..asked(&[]) }).await, "a manual-only provider needs chosen types");
    assert!(bad(asked(&["box:sideways"])).await, "unknown sub-category");
    assert!(bad(asked(&["poster"])).await, "unknown type");
    assert!(bad(api::media_backfill::MediaBackfillRequest { selection: Some(CopySelection::default()), ..asked(&["box:front"]) }).await, "empty selection");
    assert!(bad(api::media_backfill::MediaBackfillRequest { providers: vec!["thegamesdb".into()], ..asked(&["box:front"]) }).await, "a provider with no key here is refused when queued, not run to \"done, 0\"");
    // Nothing selected is linked to the provider asked for: stopped, with the reason.
    let unlinked = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region) VALUES (?, 'Fuga Quest (USA)', SHA1('Fuga Quest (USA)'), 'fuga quest', SHA1('fuga quest'), 'USA')").bind(platform).execute(&app.pool).await.unwrap().last_insert_id();
    let nothing = api::media_backfill::run(app, &api::media_backfill::MediaBackfillRequest { selection: Some(CopySelection { release_ids: vec![unlinked], ..Default::default() }), ..asked(&["box:front"]) }, &Progress::none()).await.unwrap();
    assert!(nothing.stopped.as_deref().is_some_and(|w| w.contains("none of the 1 selected release(s) is linked to retroachievements")), "{nothing:?}");
    // And a manual-only provider's art is never fetched by an ordinary call.
    let unasked = pmedia::attach_for(&app.http, &app.pool, &app.paths.media_dir(), &app.clients(), &romexplorer::database::providers::raw_store::RawStore::new(base.join("raw"), romexplorer::database::settings::RawRetention::Discard), Provider::TheGamesDb, "1", &[MediaTarget::Release(usa)], &pmedia::Ask::Auto).await.unwrap();
    assert_eq!(unasked, pmedia::Attached::Unsupported);

    api::settings::delete(app, "media_priority").await.unwrap();
    host_handle.abort();
}

/// "Export artwork": a release's box, written into a RetroArch thumbnails folder (converted to
/// a 512-px PNG) and a PCSX2-style covers folder (the JPG as it is, named by serial); a re-run
/// changes nothing; a file it did not write is left alone; an emulator with no folder is refused.
async fn artwork_export(app: &App, base: &PathBuf) {
    use romexplorer::database::emulators::{Emulator, EmulatorPlatform};
    use romexplorer::common::models::Platform;
    use romexplorer::database::emulators::EmulatorArtwork;
    use romexplorer::database::jobs::Progress;
    use romexplorer::database::media_store::MediaTarget;
    use romexplorer::database::providers::media as pmedia;
    use romexplorer_types::copy::CopySelection;
    use romexplorer_types::jobs::ArtworkExportRequest;

    // A 1000-px-wide JPEG box front, as a provider would serve it.
    let jpeg = {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(1000, 1400, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 90])));
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Jpeg).unwrap();
        out
    };
    let (host, host_handle) = start_art_host(jpeg.clone(), b"unused".to_vec(), b"unused".to_vec()).await;
    let snes = Platform::Snes.igdb_id();
    let mut releases = Vec::new();
    for (name, serial) in [("Hoge Quest (USA)", Some("SNS-HQ-USA, SNS-HQ-USA-1")), ("Fuga Quest (USA)", None)] {
        let id = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, serial) VALUES (?, ?, SHA1(?), ?, SHA1(?), 'US', ?)")
            .bind(snes).bind(name).bind(name).bind(name.to_lowercase()).bind(name.to_lowercase()).bind(serial)
            .execute(&app.pool).await.unwrap().last_insert_id();
        pmedia::store_url(&app.http, &app.pool, &app.paths.media_dir(), "thegamesdb", Some("77"), &format!("http://{host}/art.png"), "box", Some("front"), &[MediaTarget::Release(id)]).await.unwrap();
        releases.push(id);
    }
    let (hoge, fuga) = (releases[0], releases[1]);
    let ra_dir = base.join("ra-thumbnails");
    let covers_dir = base.join("covers");
    let _ = std::fs::remove_dir_all(&ra_dir);
    let _ = std::fs::remove_dir_all(&covers_dir);
    let emulator = |name: &str, artwork: Option<EmulatorArtwork>| Emulator {
        id: 0, name: name.into(), preset: None, kind: "standalone".into(), path: base.join("no-such-emu.exe").display().to_string(), args: "{rom}".into(),
        cores_dir: None, working_dir: None, supports_archives: false, supports_playlists: true, enabled: true, platforms: vec![EmulatorPlatform { app_platform_id: snes, core: None, is_default: false }], artwork,
    };
    let ra = api::launch::save(app, &emulator("Export RA", Some(EmulatorArtwork { dir: ra_dir.display().to_string(), ..EmulatorArtwork::retroarch() }))).await.unwrap().id;
    let covers = api::launch::save(app, &emulator("Export covers", Some(EmulatorArtwork { dir: covers_dir.display().to_string(), formats: vec!["jpg".into()], ..EmulatorArtwork::cover_folder() }))).await.unwrap().id;
    let none = api::launch::save(app, &emulator("Export none", None)).await.unwrap().id;

    let system = api::copy::sanitize_segment(&Platform::Snes.dat_folder_name());
    let boxart = |name: &str| ra_dir.join(&system).join("Named_Boxarts").join(format!("{name}.png"));
    // Someone else's cover is already where Fuga's would go.
    std::fs::create_dir_all(boxart("x").parent().unwrap()).unwrap();
    std::fs::write(boxart("Fuga Quest (USA)"), b"theirs").unwrap();

    let request = |emulator_id: u64| ArtworkExportRequest { emulator_id: Some(emulator_id), target: None, files: Vec::new(), selection: Some(CopySelection { release_ids: vec![hoge, fuga], ..Default::default() }) };
    let first = api::artwork::run(app, &request(ra), &Progress::none()).await.unwrap();
    assert_eq!((first.games, first.written, first.converted, first.kept), (2, 1, 1, 1), "{first:?}");
    assert_eq!(first.no_art, 6, "two games without title screens, snaps or logos: {first:?}");
    let written = image::open(boxart("Hoge Quest (USA)")).unwrap();
    assert_eq!(written.width(), 512, "scaled to RetroArch's width");
    assert_eq!(std::fs::read(boxart("Fuga Quest (USA)")).unwrap(), b"theirs", "a file it did not write is left alone");

    let again = api::artwork::run(app, &request(ra), &Progress::none()).await.unwrap();
    assert_eq!((again.written, again.unchanged, again.kept), (0, 1, 1), "a re-run changes nothing: {again:?}");

    let pcsx2 = api::artwork::run(app, &request(covers), &Progress::none()).await.unwrap();
    assert_eq!((pcsx2.written, pcsx2.converted), (2, 0), "JPG is read there: copied as it is: {pcsx2:?}");
    assert_eq!(std::fs::read(covers_dir.join("SNS-HQ-USA.jpg")).unwrap(), jpeg, "named by its first serial");
    assert!(covers_dir.join("Fuga Quest (USA).jpg").is_file(), "no serial: the game's name");

    let refused = api::jobs::enqueue(app, &api::jobs::JobRequest::ArtworkExport(request(none))).await;
    assert!(matches!(refused, Err(e) if e.kind == ErrorKind::Invalid), "an emulator with no artwork folder");
    assert!(matches!(api::jobs::enqueue(app, &api::jobs::JobRequest::ArtworkExport(ArtworkExportRequest { emulator_id: Some(ra), ..Default::default() })).await, Err(e) if e.kind == ErrorKind::Invalid), "nothing to export");
    // Any folder, no emulator: the selection's art as covers named by serial, PNG.
    let folder = base.join("any-folder");
    let _ = std::fs::remove_dir_all(&folder);
    let target = romexplorer_types::jobs::ArtworkTarget { dir: folder.display().to_string(), layout: "cover_folder".into(), formats: vec!["png".into()], max_width: Some(300) };
    let anywhere = api::artwork::run(app, &ArtworkExportRequest { emulator_id: None, target: Some(target.clone()), files: Vec::new(), selection: Some(CopySelection { release_ids: vec![hoge], ..Default::default() }) }, &Progress::none()).await.unwrap();
    assert_eq!((anywhere.written, anywhere.converted), (1, 1), "{anywhere:?}");
    assert_eq!(image::open(folder.join("SNS-HQ-USA.png")).unwrap().width(), 300);
    assert!(matches!(api::jobs::enqueue(app, &api::jobs::JobRequest::ArtworkExport(ArtworkExportRequest { target: Some(romexplorer_types::jobs::ArtworkTarget { dir: " ".into(), ..target }), selection: Some(CopySelection { release_ids: vec![hoge], ..Default::default() }), ..Default::default() })).await, Err(e) if e.kind == ErrorKind::Invalid), "no folder");
    for id in [ra, covers, none] {
        api::launch::delete(app, id).await.unwrap();
    }
    host_handle.abort();
}

/// IGDB's localized covers: the Japanese box goes in the Japanese release's region folder,
/// beside the main cover in `common/`, and is what an export of that release takes. The US
/// release keeps the main cover. Read from the cached list, so no IGDB call is made.
async fn igdb_localized_covers(app: &App, base: &PathBuf) {
    use romexplorer::common::models::Platform;
    use romexplorer::database::jobs::Progress;
    use romexplorer::database::media_store::MediaTarget;
    use romexplorer::database::providers::{igdb_localized, media as pmedia};
    use romexplorer_types::copy::CopySelection;
    use romexplorer_types::jobs::{ArtworkExportRequest, ArtworkTarget};

    let main: Vec<u8> = b"\x89PNG\r\n\x1a\n main cover".to_vec();
    let japanese: Vec<u8> = b"\x89PNG\r\n\x1a\n japanese box".to_vec();
    let (host, host_handle) = start_art_host(main.clone(), japanese.clone(), b"unused".to_vec()).await;
    let snes = Platform::Snes.igdb_id();
    let title_id = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES ('Piyo Quest', 'piyo quest', SHA1('piyo quest'))").execute(&app.pool).await.unwrap().last_insert_id();
    let mut releases = Vec::new();
    for region in ["US", "JP"] {
        let name = format!("Piyo Quest ({region})");
        let id = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region) VALUES (?, ?, SHA1(?), 'piyo quest', SHA1('piyo quest'), ?)")
            .bind(snes).bind(&name).bind(&name).bind(region)
            .execute(&app.pool).await.unwrap().last_insert_id();
        sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(title_id).bind(id).execute(&app.pool).await.unwrap();
        releases.push(id);
    }
    let (usa, japan) = (releases[0], releases[1]);
    let targets = [MediaTarget::Release(usa), MediaTarget::Release(japan)];
    // The main cover, as a sync gives it: every release, so `common/`.
    pmedia::store_url(&app.http, &app.pool, &app.paths.media_dir(), "igdb", Some("4242"), &format!("http://{host}/art.png"), "box", Some("front"), &targets).await.unwrap();

    let root = base.join("raw-igdb-localized");
    let cached = root.join("igdb").join("localized_covers");
    std::fs::create_dir_all(&cached).unwrap();
    std::fs::write(cached.join("4242.json"), json!([{"region": "JP", "url": format!("http://{host}/back.png")}, {"region": "EU", "url": format!("http://{host}/eu.png")}]).to_string()).unwrap();
    let game: playmatch_deredis::providers::igdb::model::Game = serde_json::from_value(json!({
        "id": 4242, "checksum": "00000000-0000-0000-0000-000000000000", "created_at": 0, "updated_at": 0,
        "name": "Piyo Quest", "slug": "piyo-quest", "url": "", "cover": 9, "game_localizations": [1, 2]
    })).unwrap();
    api::settings::set(app, "region_priority", json!(["JP", "US", "EU"])).await.unwrap();

    let made = igdb_localized::attach(&app.http, &app.pool, &app.paths.media_dir(), &app.clients(), &root, &pmedia::Ask::Auto, &[(targets.to_vec(), &game)]).await.unwrap();
    assert_eq!(made, 1, "only the Japanese release, and no European one to give");
    let boxes = |release: u64| async move {
        sqlx::query_scalar::<_, String>("SELECT m.path FROM media m JOIN release_media rm ON rm.media_id = m.id WHERE rm.release_id = ? AND m.media_type = 'box' AND m.deleted = FALSE ORDER BY m.id")
            .bind(release).fetch_all(&app.pool).await.unwrap()
    };
    let jp = boxes(japan).await;
    assert_eq!(jp.len(), 2, "the main cover stays beside it: {jp:?}");
    assert!(jp[0].contains("igdb/4242/common/") && jp[1].contains("igdb/4242/JP/box_front"), "{jp:?}");
    let us = boxes(usa).await;
    assert!(us.len() == 1 && us[0].contains("/common/"), "the US release keeps the main cover: {us:?}");
    let again = igdb_localized::attach(&app.http, &app.pool, &app.paths.media_dir(), &app.clients(), &root, &pmedia::Ask::Auto, &[(targets.to_vec(), &game)]).await.unwrap();
    assert_eq!(again, 0, "nothing fetched twice");

    // A Japanese demo shares the cover. A cover filed under the demo's group before
    // (`JP/_prerelease/`, when the demo came first) is moved to the retail folder, and the
    // demo is linked to that one file.
    let demo = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, is_demo) VALUES (?, 'Piyo Quest (JP) (Demo)', SHA1('Piyo Quest (JP) (Demo)'), 'piyo quest', SHA1('piyo quest'), 'JP', TRUE)")
        .bind(snes).execute(&app.pool).await.unwrap().last_insert_id();
    sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(title_id).bind(demo).execute(&app.pool).await.unwrap();
    let roots = romexplorer::database::media_store::Roots::load(&app.pool, &app.paths.media_dir()).await.unwrap();
    let (jp_id, jp_path, jp_role): (u64, String, Option<String>) = sqlx::query_as("SELECT id, path, root_role FROM media WHERE source_url = ?").bind(format!("http://{host}/back.png")).fetch_one(&app.pool).await.unwrap();
    let misfiled = jp_path.replace("/JP/", "/JP/_prerelease/");
    romexplorer::database::media_store::move_file(&roots.resolve(jp_role.as_deref(), &jp_path), &roots.resolve(jp_role.as_deref(), &misfiled)).await.unwrap();
    sqlx::query("UPDATE media SET path = ? WHERE id = ?").bind(&misfiled).bind(jp_id).execute(&app.pool).await.unwrap();
    let with_demo = [MediaTarget::Release(demo), MediaTarget::Release(usa), MediaTarget::Release(japan)];
    let linked = igdb_localized::attach(&app.http, &app.pool, &app.paths.media_dir(), &app.clients(), &root, &pmedia::Ask::Auto, &[(with_demo.to_vec(), &game)]).await.unwrap();
    assert_eq!(linked, 1, "the demo, to the same file");
    let refiled: String = sqlx::query_scalar("SELECT path FROM media WHERE id = ?").bind(jp_id).fetch_one(&app.pool).await.unwrap();
    assert!(refiled.contains("igdb/4242/JP/box_front") && !refiled.contains("_prerelease"), "{refiled}");
    assert_eq!(std::fs::read(roots.resolve(jp_role.as_deref(), &refiled)).unwrap(), japanese, "the file moved with it");
    assert!(boxes(demo).await.iter().any(|p| *p == refiled), "{:?}", boxes(demo).await);

    // A box front and a cover are one slot: the release's picture follows the provider order
    // across both, and its region's picture comes before either.
    let poster = pmedia::store_url(&app.http, &app.pool, &app.paths.media_dir(), "steamgriddb", Some("5"), &format!("http://{host}/shot.png"), "cover", None, &targets).await.unwrap().0.media_id;
    let shown = |release: u64| async move { api::catalog::release(app, release).await.unwrap().release.cover_media_id };
    let main_box: u64 = sqlx::query_scalar("SELECT id FROM media WHERE source_url = ?").bind(format!("http://{host}/art.png")).fetch_one(&app.pool).await.unwrap();
    let japanese_box: u64 = sqlx::query_scalar("SELECT id FROM media WHERE source_url = ?").bind(format!("http://{host}/back.png")).fetch_one(&app.pool).await.unwrap();
    api::settings::set(app, "media_priority", json!({"box": ["manual", "igdb"], "cover": ["manual", "steamgriddb"]})).await.unwrap();
    assert_eq!(shown(usa).await, Some(main_box), "IGDB's box: the box order comes first");
    // SteamGridDB has no regions: its cover in a region folder (all its releases were one
    // group) is still not region art, so it doesn't jump IGDB's common box.
    let poster_path: String = sqlx::query_scalar("SELECT path FROM media WHERE id = ?").bind(poster).fetch_one(&app.pool).await.unwrap();
    sqlx::query("UPDATE media SET path = ? WHERE id = ?").bind(poster_path.replace("/common/", "/US/")).bind(poster).execute(&app.pool).await.unwrap();
    assert_eq!(shown(usa).await, Some(main_box), "a regionless provider's file in a region folder is not region art");
    let strip = api::media::strip(app, &api::media::StripQuery { title_id: None, release_id: Some(usa) }).await.unwrap();
    let fronts: Vec<u64> = strip.iter().filter(|i| i.media_type == "cover" || (i.media_type == "box" && i.sub_category.as_deref() == Some("front"))).map(|i| i.id).collect();
    assert_eq!(fronts, vec![main_box, poster], "the page lists box fronts and covers together, by the order");
    sqlx::query("UPDATE media SET path = ? WHERE id = ?").bind(&poster_path).bind(poster).execute(&app.pool).await.unwrap();
    api::settings::set(app, "media_priority", json!({"box": ["manual", "steamgriddb", "igdb"]})).await.unwrap();
    assert_eq!(shown(usa).await, Some(poster), "SteamGridDB's cover, placed above IGDB");
    assert_eq!(shown(japan).await, Some(japanese_box), "the region's box before both");
    api::settings::set(app, "grid_media_type", json!("cover")).await.unwrap();
    assert_eq!(shown(usa).await, Some(poster), "the cover setting takes boxes as well");
    api::settings::delete(app, "grid_media_type").await.unwrap();
    api::settings::delete(app, "media_priority").await.unwrap();

    // An export takes the regional box over the main one, though both are IGDB's.
    let folder = base.join("igdb-localized-export");
    let _ = std::fs::remove_dir_all(&folder);
    let target = ArtworkTarget { dir: folder.display().to_string(), layout: "cover_folder_name".into(), formats: vec!["png".into()], max_width: None };
    let run = api::artwork::run(app, &ArtworkExportRequest { emulator_id: None, target: Some(target), files: Vec::new(), selection: Some(CopySelection { release_ids: vec![japan], ..Default::default() }) }, &Progress::none()).await.unwrap();
    assert_eq!(run.written, 1, "{run:?}");
    let written: Vec<_> = std::fs::read_dir(&folder).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(written.len(), 1, "{written:?}");
    assert_eq!(std::fs::read(&written[0]).unwrap(), japanese, "the Japanese box");

    api::settings::delete(app, "region_priority").await.unwrap();
    host_handle.abort();
}

/// A tiny stand-in for a provider's image host: two PNGs on loopback.
/// A title whose own IGDB link was lost while its DLC kept one: the DLC's link does not make
/// the title look linked (so a page refresh and the scan's follow-ups look it up by name), and
/// the one-off repair lists it until it is linked again.
async fn lost_title_links(app: &App) {
    use romexplorer::database::merge_audit;
    use romexplorer::database::providers::cache::{self, Confidence};
    use romexplorer::database::providers::Provider;
    let pool = &app.pool;
    let release = |name: &'static str, material: Option<&'static str>| async move {
        sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, material) VALUES (?, ?, SHA1(?), 'fuga kart', SHA1('fuga kart'), 'USA', ?)")
            .bind(romexplorer::common::models::Platform::GameBoyAdvance.igdb_id())
            .bind(name)
            .bind(name)
            .bind(material)
            .execute(pool)
            .await
            .unwrap()
            .last_insert_id()
    };
    let title = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES ('Fuga Kart', 'fuga kart', SHA1('fuga kart'))").execute(pool).await.unwrap().last_insert_id();
    let game = release("Fuga Kart (USA)", None).await;
    let dlc = release("Fuga Kart (USA) (DLC)", Some("dlc")).await;
    // A title holding nothing but an add-on has no release of its own game to look up by.
    let addon_only = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES ('Fuga Kart Pack', 'fuga kart pack', SHA1('fuga kart pack'))").execute(pool).await.unwrap().last_insert_id();
    let pack = release("Fuga Kart Pack (USA)", Some("dlc")).await;
    for (t, r) in [(title, game), (title, dlc), (addon_only, pack)] {
        sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(t).bind(r).execute(pool).await.unwrap();
    }
    for t in [title, addon_only] {
        sqlx::query("INSERT INTO title_provider_links (title_id, provider, provider_id, confidence, deleted) VALUES (?, 'igdb', '8801', 'hash', TRUE)").bind(t).execute(pool).await.unwrap();
    }
    cache::link_release(pool, dlc, Provider::Igdb, "8801", Confidence::Hash).await.unwrap();
    cache::link_release(pool, dlc, Provider::RetroAchievements, "8802", Confidence::Hash).await.unwrap();

    assert!(!api::links::title_has_provider(pool, title, Provider::Igdb).await.unwrap(), "a DLC's IGDB link is not the title's");
    assert!(api::links::title_has_provider(pool, title, Provider::RetroAchievements).await.unwrap(), "a release-tier link on any release counts");
    let lost = merge_audit::titles_that_lost_igdb(pool).await.unwrap();
    assert!(lost.contains(&title), "{lost:?}");
    assert!(!lost.contains(&addon_only), "nothing of its own game to look up: {lost:?}");
    let summary = api::links::refresh_title(app, title).await.unwrap();
    assert!(summary.already_queued.is_empty(), "each entry is fetched once: {summary:?}");

    cache::link_title(pool, title, Provider::Igdb, "8801", Confidence::Name).await.unwrap();
    assert!(api::links::title_has_provider(pool, title, Provider::Igdb).await.unwrap());
    assert!(!merge_audit::titles_that_lost_igdb(pool).await.unwrap().contains(&title), "linked again, so no longer listed");

    // "Hash match all"'s reset: the statement runs (no directory in scope, so it changes nothing).
    let req = api::jobs::HashMatchRequest { directory_id: 0, to_dat: false, to_hasheous: true, to_playmatch: true, all: true };
    api::scan::unlink_for_rematch(pool, "0", &req).await.unwrap();
    assert!(api::links::title_has_provider(pool, title, Provider::Igdb).await.unwrap());
}

/// The move-to-title picker's search: the exact name before names that only contain it, the
/// moved releases' platform first (or alone), and every match reachable a page at a time.
async fn title_picker(app: &App) {
    use romexplorer::common::models::Platform;
    let pool = &app.pool;
    let (gba, snes) = (Platform::GameBoyAdvance.igdb_id(), Platform::Snes.igdb_id());
    let add = |name: String, platform: u16| async move {
        let title = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, LOWER(?), SHA1(LOWER(?)))").bind(&name).bind(&name).bind(&name).execute(pool).await.unwrap().last_insert_id();
        let release = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region) VALUES (?, ?, SHA1(?), LOWER(?), SHA1(LOWER(?)), 'JP')")
            .bind(platform)
            .bind(&name)
            .bind(&name)
            .bind(&name)
            .bind(&name)
            .execute(pool)
            .await
            .unwrap()
            .last_insert_id();
        sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(title).bind(release).execute(pool).await.unwrap();
        (title, release)
    };
    // Thirty "Piyomon ..." titles that sort before the plain name, one plain one on SNES.
    for i in 0..30 {
        add(format!("Piyomon Adventure {i:02}"), gba).await;
    }
    let (plain, _) = add("Piyomon".to_string(), snes).await;
    let (_, moved) = add("Piyomon Ao (Japan)".to_string(), snes).await;
    let q = |extra: &[(&str, &str)]| {
        let mut q = api::catalog::ListQuery { search: Some("piyomon".into()), view: Some("collection".into()), limit: Some(25), ..Default::default() };
        for (k, v) in extra {
            match *k {
                "offset" => q.offset = v.parse().ok(),
                "near" => q.near_releases = Some(v.to_string()),
                "near_only" => q.near_only = true,
                _ => unreachable!(),
            }
        }
        q
    };
    let page = api::catalog::titles(app, &q(&[])).await.unwrap();
    assert_eq!(page.items[0].id, plain, "the exact name first: {:?}", page.items.iter().map(|t| &t.name).collect::<Vec<_>>());
    assert_eq!(page.total, 32);
    let rest = api::catalog::titles(app, &q(&[("offset", "25")])).await.unwrap();
    assert_eq!(rest.items.len(), 7, "the second page holds the rest");
    let moved = moved.to_string();
    let near = api::catalog::titles(app, &q(&[("near", &moved)])).await.unwrap();
    assert!(near.items[..2].iter().all(|t| t.platforms.as_deref().unwrap_or("").contains("Super")), "the moved release's platform first: {:?}", near.items.iter().map(|t| (&t.name, &t.platforms)).collect::<Vec<_>>());
    let only = api::catalog::titles(app, &q(&[("near", &moved), ("near_only", "1")])).await.unwrap();
    assert_eq!(only.total, 2, "only titles on its platform");
}

/// Merge review and aspect review: an International found, narrowed by a set, its choice
/// taken (the releases regrouped, the IGDB games kept) and not offered again; the aspect queue
/// with titles nobody answered first, its pick applying to the title's open releases.
async fn review_queues(app: &App) {
    use romexplorer::common::models::Platform;
    use romexplorer::database::jobs::Progress;
    use romexplorer::database::providers::cache::{self, Confidence};
    use romexplorer::database::providers::Provider;
    use romexplorer::database::title_merge::PassScope;
    let pool = &app.pool;
    let snes = Platform::Snes.igdb_id();
    let title = |name: &'static str| async move { sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, LOWER(?), SHA1(LOWER(?)))").bind(name).bind(name).bind(name).execute(pool).await.unwrap().last_insert_id() };
    let release = |title: u64, name: &'static str, region: &'static str| async move {
        let norm = romexplorer::database::matching::util::norm_key(name);
        let id = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region) VALUES (?, ?, SHA1(?), ?, SHA1(?), ?)")
            .bind(snes)
            .bind(name)
            .bind(name)
            .bind(&norm)
            .bind(&norm)
            .bind(region)
            .execute(pool)
            .await
            .unwrap()
            .last_insert_id();
        sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(title).bind(id).execute(pool).await.unwrap();
        id
    };
    // IGDB's shape: the Japanese original with the foreign versions, the International alone.
    let world = title("Hoge Quest Zeta").await;
    let jp = release(world, "Hoge Quest Zeta (Japan)", "JP").await;
    let us = release(world, "Hoge Quest Zeta (USA)", "US").await;
    let intl_title = title("Hoge Quest Zeta International").await;
    let intl = release(intl_title, "Hoge Quest Zeta International (Japan)", "JP").await;
    cache::link_title(pool, world, Provider::Igdb, "9901", Confidence::Hash).await.unwrap();
    cache::link_title(pool, intl_title, Provider::Igdb, "9902", Confidence::Hash).await.unwrap();
    romexplorer::database::title_review::suggest(pool, &PassScope::full(), &Progress::none()).await.unwrap();

    let set = api::sets::create(app, &api::sets::SetSpec { name: "Review month".into(), description: None }).await.unwrap();
    api::sets::add_releases(app, set.id, &[us]).await.unwrap();
    api::sets::resolve(app, &[set.id]).await.unwrap();
    let in_set = api::catalog::ListQuery { set_id: Some(set.id), ..Default::default() };
    let page = api::review::merge_queue(app, &in_set, &Default::default()).await.unwrap();
    let item = page.items.iter().find(|i| i.kind == "international").unwrap_or_else(|| panic!("the International is suggested: {:?}", page.items.iter().map(|i| &i.key).collect::<Vec<_>>()));
    assert!(item.options[0].recommended && item.can_look_up);
    let other_set = api::catalog::ListQuery { set_id: Some(set.id + 1000), ..Default::default() };
    assert_eq!(api::review::merge_queue(app, &other_set, &Default::default()).await.unwrap().total, 0, "a set narrows the queue");

    // No Gemini key here: the lookup still gives the question as a Google search.
    let look = api::review::lookup(app, &item.key, &Default::default()).await.unwrap();
    assert!(look.result.answer.is_none() && look.result.error.is_some());
    assert!(look.google_url.starts_with("https://www.google.com/search?q=") && look.ask_links.iter().any(|l| l.id == "google" && l.url.ends_with("udm=50")), "{look:?}");

    let key = item.key.clone();
    let done = api::review::apply(app, &key, &api::review::ApplyRequest { option: 0, ..Default::default() }).await.unwrap();
    assert_eq!(done.title_ids.len(), 2);
    let title_of = |r: u64| async move { sqlx::query_scalar::<_, u64>("SELECT title_id FROM title_releases WHERE release_id = ?").bind(r).fetch_one(pool).await.unwrap() };
    let (t_us, t_intl, t_jp) = (title_of(us).await, title_of(intl).await, title_of(jp).await);
    assert_eq!(t_us, t_intl, "the International with the foreign version");
    assert_ne!(t_jp, t_us, "the Japanese original apart");
    let igdb: Vec<String> = sqlx::query_scalar("SELECT provider_id FROM release_provider_links WHERE provider = 'igdb' AND deleted = FALSE AND release_id IN (?, ?) ORDER BY provider_id").bind(us).bind(intl).fetch_all(pool).await.unwrap();
    assert_eq!(igdb, vec!["9901".to_string(), "9902".to_string()], "each release keeps its own game");
    romexplorer::database::title_review::suggest(pool, &PassScope::full(), &Progress::none()).await.unwrap();
    let status: String = sqlx::query_scalar("SELECT status FROM title_merge_suggestions WHERE igdb_id = ?").bind(&key).fetch_one(pool).await.unwrap();
    assert_eq!(status, "accepted", "taken, and not offered again");
    api::sets::delete(app, set.id).await.unwrap();

    // A bundle holding a single game's Japanese release: one click moves it into that game's
    // title, the collection itself staying where it is.
    let bundle = title("Piyo Collection").await;
    sqlx::query("UPDATE title SET category = 'bundle' WHERE id = ?").bind(bundle).execute(pool).await.unwrap();
    let collection = release(bundle, "Piyo Collection (USA)", "US").await;
    let stray = release(bundle, "Piyo Kingdom (Japan)", "JP").await;
    let single = title("Piyo Kingdom").await;
    let single_us = release(single, "Piyo Kingdom (USA)", "US").await;
    // A second game of the bundle, its Japanese release named as sold there: found through
    // the single title's alternative name.
    let renamed = release(bundle, "Piyo Oukoku Ni (Japan)", "JP").await;
    let second = title("Piyo Kingdom II").await;
    let second_us = release(second, "Piyo Kingdom II (USA)", "US").await;
    sqlx::query("INSERT INTO title_alt_names (title_id, name, name_hash, source) VALUES (?, 'Piyo Oukoku Ni', SHA1('Piyo Oukoku Ni'), 'igdb')").bind(second).execute(pool).await.unwrap();
    romexplorer::database::title_review::suggest(pool, &PassScope::full(), &Progress::none()).await.unwrap();
    let only_bundles = api::review::ReviewParams { kind: Some("bundle_vs_game".into()), ..Default::default() };
    let page = api::review::merge_queue(app, &Default::default(), &only_bundles).await.unwrap();
    let item = page.items.iter().find(|i| i.key == format!("bundle:{bundle}")).expect("the bundle is suggested");
    assert!(item.options[0].recommended && item.options[0].label == "split_bundle");
    api::review::apply(app, &item.key, &api::review::ApplyRequest { option: 0, ..Default::default() }).await.unwrap();
    assert_eq!(title_of(stray).await, title_of(single_us).await, "the game's release joined its title");
    assert_eq!(title_of(renamed).await, title_of(second_us).await, "matched by the title's alternative name, in the same click");
    assert_eq!(title_of(collection).await, bundle, "the collection stays");

    // Aspect review: two SNES titles, one already answered for another release.
    let open = title("Fuga Aspect Open").await;
    let open_a = release(open, "Fuga Aspect Open (Japan)", "JP").await;
    let open_b = release(open, "Fuga Aspect Open (USA)", "US").await;
    let half = title("Fuga Aspect Half").await;
    let half_a = release(half, "Fuga Aspect Half (Japan)", "JP").await;
    let half_b = release(half, "Fuga Aspect Half (USA)", "US").await;
    api::display::set(app, &api::display::DisplaySet { release_ids: vec![half_a], field: "aspect".into(), value: Some("8:7".into()) }).await.unwrap();
    let only_fuga = api::catalog::ListQuery { search: Some("Fuga Aspect".into()), ..Default::default() };
    let queue = api::review::aspect_queue(app, &only_fuga, &Default::default()).await.unwrap();
    let order: Vec<u64> = queue.items.iter().map(|i| i.title_id).collect();
    assert_eq!(order, vec![open, half], "the title nobody answered first");
    let mut apply_to = queue.items[0].apply_to.clone();
    apply_to.sort_unstable();
    assert_eq!(apply_to, vec![open_a, open_b]);
    assert_eq!(queue.items[1].apply_to, vec![half_b], "an answered release is not asked again");
    assert!(queue.items.iter().all(|i| i.compare == "8:7" && !i.has_screenshot));
    let shots = api::review::aspect_queue(app, &only_fuga, &api::review::ReviewParams { with_screenshots: true, ..Default::default() }).await.unwrap();
    assert_eq!(shots.total, 0, "none has a screenshot");
}

/// A set out to a file and back into a catalogue that lost it: every kind of entry found again
/// by name, content or IGDB game, in its order; what is not there listed, nothing removed.
async fn sets_file(app: &App) {
    use romexplorer::common::models::Platform;
    use romexplorer::database::providers::cache::{self, Confidence};
    use romexplorer::database::providers::Provider;
    let pool = &app.pool;
    let snes = Platform::Snes.igdb_id();
    let title = |name: &'static str| async move { sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, LOWER(?), SHA1(LOWER(?)))").bind(name).bind(name).bind(name).execute(pool).await.unwrap().last_insert_id() };
    let release = |title: u64, name: &'static str| async move {
        let id = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region) VALUES (?, ?, SHA1(?), LOWER(?), SHA1(LOWER(?)), 'JP')")
            .bind(snes)
            .bind(name)
            .bind(name)
            .bind(name)
            .bind(name)
            .execute(pool)
            .await
            .unwrap()
            .last_insert_id();
        sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(title).bind(id).execute(pool).await.unwrap();
        id
    };
    let a = title("Setfile Alpha").await;
    let a_rel = release(a, "Setfile Alpha (Japan)").await;
    let b = title("Setfile Beta").await;
    release(b, "Setfile Beta (Japan)").await;
    let c = title("Setfile Gamma").await;
    release(c, "Setfile Gamma (Japan)").await;
    cache::link_title(pool, c, Provider::Igdb, "77001", Confidence::Hash).await.unwrap();
    let set = api::sets::create(app, &api::sets::SetSpec { name: "Setfile month".into(), description: Some("for the trip".into()) }).await.unwrap();
    api::sets::add_entries(app, set.id, api::sets::EntryKind::Title, &[b.to_string()]).await.unwrap();
    api::sets::add_entries(app, set.id, api::sets::EntryKind::Release, &[a_rel.to_string()]).await.unwrap();
    api::sets::add_entries(app, set.id, api::sets::EntryKind::Game, &["77001".to_string()]).await.unwrap();

    let mut file = api::set_file::export(app, &[set.id]).await.unwrap();
    assert_eq!(file.sets.len(), 1);
    assert_eq!(file.sets[0].entries.len(), 3);
    // An entry this catalogue never had.
    file.sets[0].entries.push(api::set_file::EntryDoc::Release { release: api::set_file::ReleaseRef { name: "Setfile Nowhere (Japan)".into(), platform: snes, ..Default::default() }, title: None });
    let text = serde_json::to_string(&file).unwrap();
    api::sets::delete(app, set.id).await.unwrap();

    let file: api::set_file::SetFile = serde_json::from_str(&text).unwrap();
    let done = api::set_file::import(app, &api::set_file::ImportRequest { file: file.clone(), names: Vec::new() }).await.unwrap();
    assert_eq!(done.len(), 1);
    assert!(done[0].created && done[0].added == 3, "{done:?}");
    assert_eq!(done[0].missing.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), vec!["Setfile Nowhere (Japan)"]);
    let entries: Vec<(Option<u64>, Option<u64>, Option<String>)> = sqlx::query_as("SELECT release_id, title_id, igdb_id FROM collection_set_entries WHERE set_id = ? ORDER BY position").bind(done[0].set_id).fetch_all(pool).await.unwrap();
    assert_eq!(entries, vec![(None, Some(b), None), (Some(a_rel), None, None), (None, None, Some("77001".to_string()))], "the same things, in the same order");
    let description: Option<String> = sqlx::query_scalar("SELECT description FROM collection_sets WHERE id = ?").bind(done[0].set_id).fetch_one(pool).await.unwrap();
    assert_eq!(description.as_deref(), Some("for the trip"));
    // Again: into the same set, nothing added twice.
    let again = api::set_file::import(app, &api::set_file::ImportRequest { file, names: Vec::new() }).await.unwrap();
    assert!(!again[0].created && again[0].added == 0 && again[0].already == 3 && again[0].set_id == done[0].set_id, "{again:?}");
    api::sets::delete(app, done[0].set_id).await.unwrap();
}

async fn start_art_host(front: Vec<u8>, back: Vec<u8>, shot: Vec<u8>) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    use axum::response::IntoResponse;
    use axum::routing::get;
    let png = |bytes: Vec<u8>| move || async move { ([(axum::http::header::CONTENT_TYPE, "image/png")], bytes).into_response() };
    let router = axum::Router::new().route("/art.png", get(png(front))).route("/back.png", get(png(back))).route("/shot.png", get(png(shot)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (addr, handle)
}

/// A client takes its art from its main server before it asks a provider.
///
/// `app` is the main server, already serving at `base`. A second database on the same
/// embedded MariaDB is the client: it syncs the catalogue and the art mirror, fetches the
/// files from the server, and only goes to the "provider" (a loopback image host) for a
/// file the server no longer has -- and not even then once the fallback setting is off.
async fn media_from_the_server(app: &App, http: &reqwest::Client, base: &str, token: &str) {
    use romexplorer::common::models::Platform;
    use romexplorer::database::jobs::Progress;
    use romexplorer::database::media_store::MediaTarget;
    use romexplorer::database::providers::media as pmedia;
    use romexplorer::database::settings as db_settings;

    // Two distinct images: a "provider" cover and a user's own pick for another slot.
    let front: Vec<u8> = b"\x89PNG\r\n\x1a\n front".iter().chain(std::iter::repeat(&7u8).take(2000)).copied().collect();
    let back: Vec<u8> = b"\x89PNG\r\n\x1a\n back".iter().chain(std::iter::repeat(&9u8).take(2000)).copied().collect();
    // Distinct bytes: the store dedups identical content, and this one must be a new row.
    let shot: Vec<u8> = b"\x89PNG\r\n\x1a\n shot".iter().chain(std::iter::repeat(&3u8).take(2000)).copied().collect();
    let (host, host_handle) = start_art_host(front.clone(), back.clone(), shot.clone()).await;
    let cover_url = format!("http://{host}/art.png");

    // --- the main server: one title, one release, a provider cover and a manual pick ---
    let title_id = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES ('Mirror Test', 'mirror test', SHA1('mirror test'))").execute(&app.pool).await.unwrap().last_insert_id();
    let release_id = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash) VALUES (?, 'Mirror Test (USA)', SHA1('Mirror Test (USA)'), 'mirror test', SHA1('mirror test'))")
        .bind(Platform::Snes.igdb_id())
        .execute(&app.pool)
        .await
        .unwrap()
        .last_insert_id();
    sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(title_id).bind(release_id).execute(&app.pool).await.unwrap();
    let (cover, linked) = pmedia::store_url(&app.http, &app.pool, &app.paths.media_dir(), "igdb", Some("4242"), &cover_url, "cover", None, &[MediaTarget::Release(release_id)]).await.unwrap();
    assert_eq!(linked, 1);
    let pick = api::media::upload(app, &api::media::SlotTarget { title_id: None, release_id: Some(release_id) }, "box", Some("back"), "back.png", &back, None).await.unwrap();
    assert_eq!(pick.linked_releases, vec![release_id]);

    // --- the client: its own database and media root, the server as its main source ---
    let config = app.config.clone().with_database("romexplorer_api_test_client");
    let pool = romexplorer::database::initialize(&config).await.expect("client database");
    let root = output_dir("client_root");
    let _ = std::fs::remove_dir_all(&root);
    let client = App::with_pool(pool, AppPaths::from_root(&root), config).unwrap();
    let source_id = api::sources::create(
        &client,
        &api::sources::SourceSpec { name: "the server".into(), base_url: base.to_string(), token: Some(token.to_string()), role: "main".into(), enabled: true, replace_catalog: false, media: Default::default(), username: None, password: None },
    )
    .await
    .unwrap()
    .id;

    let pulled = api::sync::pull(&client, source_id, false, &Progress::none()).await.unwrap();
    // Earlier scenarios left art of their own on the server, so count this release's rows.
    assert!(pulled.tables.iter().any(|(t, n)| t == "remote_media" && *n >= 2), "the art mirror came with the catalogue: {:?}", pulled.tables);
    let mirrored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM remote_release_media WHERE source_id = ? AND release_id = ?").bind(source_id).bind(release_id).fetch_one(&client.pool).await.unwrap();
    assert_eq!(mirrored, 2, "both of the release's items are listed");
    assert!(pulled.media_sync_job.is_some(), "new art on the server queues its fetch: {pulled:?}");
    let have_release: Option<i64> = sqlx::query_scalar("SELECT 1 FROM `release` WHERE id = ?").bind(release_id).fetch_optional(&client.pool).await.unwrap();
    assert_eq!(have_release, Some(1), "the catalogue row the art hangs off arrived with the same id");
    let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE job_type = 'media_sync' AND status = 'pending'").fetch_one(&client.pool).await.unwrap();
    assert_eq!(queued, 1);
    // A second pull with nothing changed queues nothing more.
    let again = api::sync::pull(&client, source_id, false, &Progress::none()).await.unwrap();
    assert!(again.media_sync_job.is_none(), "{again:?}");

    // --- the fetch: everything from the server, nothing from the provider ---
    let done = api::media_sync::run(&client, Some(source_id), &Progress::none()).await.unwrap();
    // At least this release's two; whatever earlier scenarios attached to releases the
    // client now has comes along the same way.
    assert!(done.from_server >= 2 && (done.from_provider, done.failed) == (0, 0), "{done:?}");
    let rows: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as("SELECT m.source, m.provider_id, m.source_url FROM media m JOIN release_media rm ON rm.media_id = m.id WHERE rm.release_id = ? AND m.deleted = FALSE ORDER BY m.source")
        .bind(release_id)
        .fetch_all(&client.pool)
        .await
        .unwrap();
    assert_eq!(rows, vec![("igdb".to_string(), Some("4242".to_string()), Some(cover_url.clone())), ("manual".to_string(), None, None)], "recorded as if this machine had fetched them itself");
    let client_cover: u64 = sqlx::query_scalar("SELECT m.id FROM media m JOIN release_media rm ON rm.media_id = m.id WHERE rm.release_id = ? AND m.source = 'igdb'").bind(release_id).fetch_one(&client.pool).await.unwrap();
    let (path, _) = api::media::file(&client, client_cover, false, None).await.unwrap();
    assert!(path.starts_with(&root), "stored under the client's own media root: {}", path.display());
    assert_eq!(std::fs::read(&path).unwrap(), front, "the bytes are the server's");

    // Nothing to do the second time.
    let repeat = api::media_sync::run(&client, Some(source_id), &Progress::none()).await.unwrap();
    assert!(repeat.already_held >= 2 && (repeat.from_server, repeat.from_provider, repeat.failed) == (0, 0, 0), "{repeat:?}");

    // A plain media_download for the provider URL would take it from the server too.
    let cover_target = MediaTarget::Release(release_id);
    let igdb = romexplorer::database::providers::Provider::Igdb;
    match api::media_sync::via_main(&client, &cover_url, igdb, Some("4242"), "cover", None, cover_target).await.unwrap() {
        api::media_sync::ServerCopy::Bytes(f) => assert_eq!(f.bytes, front),
        _ => panic!("the server has this URL"),
    }

    // --- the server lost the file: it fetches it again itself, the client never asks the
    // provider. The route serves through the cache tiers, so first the case where a tier
    // still has a copy (the earlier GET made one), then the case where nothing does. ---
    // The stored original, not what the route serves: that may be a cache-tier copy.
    std::fs::remove_file(&cover.path).unwrap();
    assert_eq!(http.get(format!("{base}/api/v1/media/{}", cover.media_id)).bearer_auth(token).send().await.unwrap().status(), 200, "a cache tier still has it: the original is only the deepest tier");
    // Served from the tier, and put back while it was at hand: a tier is evicted, the media
    // dir is not, so art held only in a tier is art about to be lost.
    assert_eq!(std::fs::read(&cover.path).unwrap(), front, "a view served from the cache copy puts the original back");
    // Asked for outright, with no view first, the tier fills it just the same.
    std::fs::remove_file(&cover.path).unwrap();
    let restored: api::media::RefetchResult = http.post(format!("{base}/api/v1/media/{}/refetch", cover.media_id)).bearer_auth(token).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    assert_eq!(restored.from, "cache", "put back from the tier, no provider call: {restored:?}");
    assert_eq!(std::fs::read(&cover.path).unwrap(), front);
    std::fs::remove_file(&cover.path).unwrap();
    let roots = romexplorer::database::media_store::Roots::load(&app.pool, &app.paths.media_dir()).await.unwrap();
    romexplorer::database::media_cache::forget(&app.pool, &roots, cover.media_id).await.unwrap();
    assert_eq!(http.get(format!("{base}/api/v1/media/{}", cover.media_id)).bearer_auth(token).send().await.unwrap().status(), 404, "gone from every tier");
    sqlx::query("UPDATE media SET deleted = TRUE WHERE id = ?").bind(client_cover).execute(&client.pool).await.unwrap();
    let refilled = api::media_sync::run(&client, Some(source_id), &Progress::none()).await.unwrap();
    assert_eq!((refilled.from_server, refilled.server_refetched, refilled.from_provider, refilled.failed), (1, 1, 0, 0), "{refilled:?}");
    assert_eq!(std::fs::read(&cover.path).unwrap(), front, "the server's own copy is back: it is the cache");
    let refetched: u64 = sqlx::query_scalar("SELECT m.id FROM media m JOIN release_media rm ON rm.media_id = m.id WHERE rm.release_id = ? AND m.source = 'igdb' AND m.deleted = FALSE").bind(release_id).fetch_one(&client.pool).await.unwrap();
    assert_ne!(refetched, client_cover);
    let (path, _) = api::media::file(&client, refetched, false, None).await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), front);

    // A URL the server never had: it fetches it into its cache for the client.
    let shot_url = format!("http://{host}/shot.png");
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media WHERE source_url = ?").bind(&shot_url).fetch_one(&app.pool).await.unwrap();
    assert_eq!(before, 0);
    match api::media_sync::via_main(&client, &shot_url, igdb, Some("4242"), "screenshot", None, cover_target).await.unwrap() {
        api::media_sync::ServerCopy::Bytes(f) => assert_eq!(f.bytes, shot),
        _ => panic!("the server fetches what it lacks"),
    }
    let cached: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media WHERE source_url = ? AND source = 'igdb' AND deleted = FALSE").bind(&shot_url).fetch_one(&app.pool).await.unwrap();
    assert_eq!(cached, 1, "and now holds it for everyone");
    // The server fetches from named hosts only: an address is not a way into its network.
    let inside = http.post(format!("{base}/api/v1/media/fetch")).bearer_auth(token).json(&json!({"provider": "igdb", "url": "http://192.168.1.1/x.png", "media_type": "cover", "release_id": release_id})).send().await.unwrap();
    assert_eq!(inside.status(), 400, "{}", inside.text().await.unwrap());
    let manual_refetch = http.post(format!("{base}/api/v1/media/{}/refetch", pick.media_id)).bearer_auth(token).send().await.unwrap();
    assert_eq!(manual_refetch.status(), 200, "present, nothing to do");

    // --- the server offline: the provider, and only if allowed ---
    sqlx::query("UPDATE remote_sources SET base_url = 'http://127.0.0.1:1' WHERE id = ?").bind(source_id).execute(&client.pool).await.unwrap();
    assert!(matches!(api::media_sync::via_main(&client, &cover_url, igdb, Some("4242"), "cover", None, cover_target).await.unwrap(), api::media_sync::ServerCopy::Offline(_)));
    sqlx::query("UPDATE media SET deleted = TRUE WHERE id = ?").bind(refetched).execute(&client.pool).await.unwrap();
    let offline = api::media_sync::run(&client, Some(source_id), &Progress::none()).await.unwrap();
    assert_eq!((offline.from_server, offline.from_provider, offline.failed), (0, 1, 0), "{offline:?}");
    let direct: u64 = sqlx::query_scalar("SELECT m.id FROM media m JOIN release_media rm ON rm.media_id = m.id WHERE rm.release_id = ? AND m.source = 'igdb' AND m.deleted = FALSE").bind(release_id).fetch_one(&client.pool).await.unwrap();
    let (path, _) = api::media::file(&client, direct, false, None).await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), front, "the provider's copy");

    db_settings::set(&client.pool, db_settings::KEY_MEDIA_PROVIDER_FALLBACK, &false).await.unwrap();
    sqlx::query("UPDATE media SET deleted = TRUE WHERE id = ?").bind(direct).execute(&client.pool).await.unwrap();
    let refused = api::media_sync::run(&client, Some(source_id), &Progress::none()).await;
    assert!(refused.is_err(), "offline and no fallback: nowhere to fetch from, said up front: {refused:?}");
    sqlx::query("UPDATE remote_sources SET base_url = ? WHERE id = ?").bind(base).bind(source_id).execute(&client.pool).await.unwrap();
    // Back online, the server is used regardless of the fallback setting.
    let back_online = api::media_sync::run(&client, Some(source_id), &Progress::none()).await.unwrap();
    assert_eq!((back_online.from_server, back_online.from_provider, back_online.failed), (1, 0, 0), "{back_online:?}");

    // --- client mode: keep a copy, or stream from the server (media_client_keep) ---
    db_settings::set(&client.pool, db_settings::KEY_INSTANCE_MODE, &"client").await.unwrap();
    db_settings::set(&client.pool, db_settings::KEY_MEDIA_CLIENT_KEEP, &json!({"cover": false})).await.unwrap();
    let kept: u64 = sqlx::query_scalar("SELECT m.id FROM media m JOIN release_media rm ON rm.media_id = m.id WHERE rm.release_id = ? AND m.source = 'igdb' AND m.media_type = 'cover' AND m.deleted = FALSE").bind(release_id).fetch_one(&client.pool).await.unwrap();
    sqlx::query("UPDATE media SET deleted = TRUE WHERE id = ?").bind(kept).execute(&client.pool).await.unwrap();
    let streamed = api::media_sync::run(&client, Some(source_id), &Progress::none()).await.unwrap();
    assert!(streamed.streamed_only >= 1 && (streamed.from_server, streamed.from_provider, streamed.failed) == (0, 0, 0), "a type that is not kept is not fetched: {streamed:?}");
    // The page still shows it: listed as remote, served through this machine from the server.
    let strip = api::media::strip(&client, &api::media::StripQuery { title_id: None, release_id: Some(release_id) }).await.unwrap();
    let remote_cover = strip.iter().find(|i| i.media_type == "cover").expect("the server's cover is in the strip");
    assert!(remote_cover.remote && remote_cover.id == cover.media_id && remote_cover.file == format!("/api/v1/media/remote/{}", cover.media_id), "{remote_cover:?}");
    assert!(strip.iter().any(|i| i.media_type == "box" && !i.remote), "what is held here is local: {strip:?}");
    let whole = api::media_sync::from_main(&client, &format!("/api/v1/media/{}", cover.media_id), None).await.unwrap();
    assert_eq!(whole.status(), 200);
    assert_eq!(whole.bytes().await.unwrap().to_vec(), front, "streamed bytes are the server's");
    let part = api::media_sync::from_main(&client, &format!("/api/v1/media/{}", cover.media_id), Some("bytes=1-3")).await.unwrap();
    assert_eq!(part.status(), 206, "a range is passed on, so a player can seek");
    assert_eq!(part.headers().get("content-range").and_then(|v| v.to_str().ok()), Some(format!("bytes 1-3/{}", front.len()).as_str()));
    assert_eq!(part.bytes().await.unwrap().to_vec(), front[1..4].to_vec());
    assert!(api::media_sync::from_main(&client, "/api/v1/media/99999999", None).await.is_err(), "what the server lacks is an error, not an empty stream");
    // "Show in UI" off hides the type from the strip, kept or not.
    db_settings::set(&client.pool, db_settings::KEY_MEDIA_SHOW, &json!({"cover": false})).await.unwrap();
    let hidden = api::media::strip(&client, &api::media::StripQuery { title_id: None, release_id: Some(release_id) }).await.unwrap();
    assert!(!hidden.iter().any(|i| i.media_type == "cover") && hidden.iter().any(|i| i.media_type == "box"), "{hidden:?}");
    db_settings::delete(&client.pool, db_settings::KEY_MEDIA_SHOW).await.unwrap();
    // Kept again: fetched, and then it is the local copy the strip lists. These bytes are not
    // a real image, so there is nothing to convert and they are stored as they came.
    db_settings::delete(&client.pool, db_settings::KEY_MEDIA_CLIENT_KEEP).await.unwrap();
    let keeping = api::media_sync::run(&client, Some(source_id), &Progress::none()).await.unwrap();
    assert_eq!((keeping.from_server, keeping.converted, keeping.failed), (1, 0, 0), "{keeping:?}");
    let strip = api::media::strip(&client, &api::media::StripQuery { title_id: None, release_id: Some(release_id) }).await.unwrap();
    assert!(strip.iter().any(|i| i.media_type == "cover" && !i.remote) && !strip.iter().any(|i| i.media_type == "cover" && i.remote), "held here now, listed once: {strip:?}");

    // --- a regional box next to the common one: both come over, each in the server's folder ---
    // The server holds IGDB's main box for every release (`common/`) and its Japanese box for
    // the Japanese one (`JP/`). The client already holding "an IGDB box front" for the
    // Japanese release must not stop the second, and it must land under `JP/` too, or it
    // ranks level with the common one and the older wins.
    let main_box: Vec<u8> = b"\x89PNG\r\n\x1a\n main".iter().chain(std::iter::repeat(&5u8).take(2000)).copied().collect();
    let japan_box: Vec<u8> = b"\x89PNG\r\n\x1a\n japan".iter().chain(std::iter::repeat(&6u8).take(2000)).copied().collect();
    let regional_host = {
        use axum::response::IntoResponse;
        use axum::routing::get;
        let png = |bytes: Vec<u8>| move || async move { ([(axum::http::header::CONTENT_TYPE, "image/png")], bytes).into_response() };
        let router = axum::Router::new().route("/main.png", get(png(main_box.clone()))).route("/jp.png", get(png(japan_box.clone())));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        (addr, tokio::spawn(async move { axum::serve(listener, router).await.unwrap() }))
    };
    let japan_release = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region) VALUES (?, 'Mirror Test (Japan)', SHA1('Mirror Test (Japan)'), 'mirror test', SHA1('mirror test'), 'JP')")
        .bind(Platform::Snes.igdb_id())
        .execute(&app.pool)
        .await
        .unwrap()
        .last_insert_id();
    sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(title_id).bind(japan_release).execute(&app.pool).await.unwrap();
    let main_url = format!("http://{}/main.png", regional_host.0);
    let japan_url = format!("http://{}/jp.png", regional_host.0);
    let both = [MediaTarget::Release(release_id), MediaTarget::Release(japan_release)];
    let (main_stored, _) = pmedia::store_url(&app.http, &app.pool, &app.paths.media_dir(), "igdb", Some("4242"), &main_url, "box", Some("front"), &both).await.unwrap();
    let (japan_stored, _) = pmedia::store_url(&app.http, &app.pool, &app.paths.media_dir(), "igdb", Some("4242"), &japan_url, "box", Some("front"), &[MediaTarget::Release(japan_release)]).await.unwrap();
    let server_paths: Vec<String> = sqlx::query_scalar("SELECT path FROM media WHERE id IN (?, ?) ORDER BY id").bind(main_stored.media_id).bind(japan_stored.media_id).fetch_all(&app.pool).await.unwrap();
    assert!(server_paths[0].contains("/common/") && server_paths[1].contains("/JP/"), "{server_paths:?}");
    api::sync::pull(&client, source_id, false, &Progress::none()).await.unwrap();
    let regional = api::media_sync::run(&client, Some(source_id), &Progress::none()).await.unwrap();
    // Both boxes, plus the screenshot the server fetched for `via_main` above.
    assert!(regional.from_server >= 2 && regional.failed == 0, "both boxes, from the server: {regional:?}");
    let held: Vec<(u64, String, Option<String>)> = sqlx::query_as("SELECT m.id, m.path, m.source_url FROM media m JOIN release_media rm ON rm.media_id = m.id WHERE rm.release_id = ? AND m.media_type = 'box' AND m.sub_category = 'front' AND m.deleted = FALSE ORDER BY m.id")
        .bind(japan_release)
        .fetch_all(&client.pool)
        .await
        .unwrap();
    assert_eq!(held.iter().map(|h| h.2.clone().unwrap_or_default()).collect::<Vec<_>>(), vec![main_url.clone(), japan_url.clone()], "the Japanese release holds both, as on the server: {held:?}");
    assert!(held[0].1.contains("/common/") && held[1].1.contains("/JP/"), "filed in the server's folders: {held:?}");
    assert_eq!(api::catalog::release(&client, japan_release).await.unwrap().release.cover_media_id, Some(held[1].0), "the Japanese release shows its Japanese box");
    let fronts = |strip: Vec<api::media::StripItem>| strip.into_iter().filter(|i| i.media_type == "box" && i.sub_category.as_deref() == Some("front")).map(|i| i.id).collect::<Vec<_>>();
    let japan_query = api::media::StripQuery { title_id: None, release_id: Some(japan_release) };
    let japan_strip = || api::media::strip(&client, &japan_query);
    assert_eq!(fronts(japan_strip().await.unwrap()), vec![held[1].0, held[0].0], "the page lists the Japanese box first");
    // A user's pick placed in the order counts as region art: after IGDB's Japanese box, before
    // IGDB's common one.
    let own_front: Vec<u8> = b"\x89PNG\r\n\x1a\n own".iter().chain(std::iter::repeat(&4u8).take(2000)).copied().collect();
    let own = api::media::upload(&client, &api::media::SlotTarget { title_id: None, release_id: Some(japan_release) }, "box", Some("front"), "own.png", &own_front, None).await.unwrap();
    assert_eq!(fronts(japan_strip().await.unwrap())[0], own.media_id, "an unplaced pick comes first");
    db_settings::set(&client.pool, "media_priority", &json!({"box": ["igdb", "manual"]})).await.unwrap();
    // The pick takes the slot here, so the IGDB boxes may now be the server's (streamed): told
    // apart by what they are, not by id.
    let what = |i: &api::media::StripItem| match (i.remote, i.id) {
        (false, id) if id == own.media_id => "pick",
        (false, id) if id == held[1].0 => "japan",
        (false, id) if id == held[0].0 => "common",
        (true, id) if id == japan_stored.media_id => "japan",
        (true, id) if id == main_stored.media_id => "common",
        _ => "other",
    };
    let order: Vec<&str> = japan_strip().await.unwrap().iter().filter(|i| i.media_type == "box" && i.sub_category.as_deref() == Some("front")).map(what).collect();
    assert_eq!(order, vec!["japan", "pick", "common"], "Japanese IGDB, the pick, common IGDB");
    db_settings::delete(&client.pool, "media_priority").await.unwrap();
    let again = api::media_sync::run(&client, Some(source_id), &Progress::none()).await.unwrap();
    assert_eq!((again.from_server, again.failed), (0, 0), "{again:?}");
    regional_host.1.abort();

    // --- export on a client: the main server's original, not the copy kept for display ---
    // A box front picked by hand on the title (no provider link to match it by).
    let manual_front = {
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(40, 56, |x, y| image::Rgb([x as u8 * 6, y as u8 * 4, 200]))).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        out
    };
    api::media::upload(app, &api::media::SlotTarget { title_id: Some(title_id), release_id: None }, "box", Some("front"), "front.png", &manual_front, None).await.unwrap();
    api::sync::pull(&client, source_id, false, &Progress::none()).await.unwrap();
    api::media_sync::run(&client, Some(source_id), &Progress::none()).await.unwrap();
    let (path, role): (String, Option<String>) = sqlx::query_as("SELECT m.path, m.root_role FROM media m JOIN title_media tm ON tm.media_id = m.id WHERE tm.title_id = ? AND m.media_type = 'box' AND m.sub_category = 'front' AND m.deleted = FALSE")
        .bind(title_id)
        .fetch_one(&client.pool)
        .await
        .unwrap();
    let client_copy = romexplorer::database::media_store::Roots::load(&client.pool, &client.paths.media_dir()).await.unwrap().resolve(role.as_deref(), &path);
    // What a client keeps for display: an AVIF this build cannot read.
    let display_copy: Vec<u8> = b"\0\0\0\x1cftypavif\0\0\0\0avifmif1miaf".iter().chain(std::iter::repeat(&0u8).take(64)).copied().collect();
    std::fs::write(&client_copy, &display_copy).unwrap();
    let export_into = |dir: &std::path::Path| romexplorer_types::jobs::ArtworkExportRequest {
        emulator_id: None,
        target: Some(romexplorer_types::jobs::ArtworkTarget { dir: dir.display().to_string(), layout: "cover_folder".into(), formats: vec!["png".into()], max_width: None }),
        files: Vec::new(),
        selection: Some(romexplorer_types::copy::CopySelection { release_ids: vec![release_id], ..Default::default() }),
    };
    let exported_dir = root.join("exported");
    let exported = api::artwork::run(&client, &export_into(&exported_dir), &Progress::none()).await.unwrap();
    assert_eq!((exported.written, exported.errors.len()), (1, 0), "{exported:?}");
    assert_eq!(std::fs::read(exported_dir.join("Mirror Test (USA).png")).unwrap(), manual_front, "the server's original");
    // The server's original is not kept here: the export cache is for conversions. A re-run
    // that would write the same file knows it from the server's hash, without fetching.
    let cache_root = romexplorer::api::artwork::cache::dir(&client.paths.media_dir());
    let stale = cache_root.join("ab").join("ab12-original");
    std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
    std::fs::write(&stale, b"left by an older build").unwrap();
    sqlx::query("UPDATE remote_sources SET base_url = 'http://127.0.0.1:1' WHERE id = ?").bind(source_id).execute(&client.pool).await.unwrap();
    let again = api::artwork::run(&client, &export_into(&exported_dir), &Progress::none()).await.unwrap();
    assert_eq!((again.written, again.unchanged, again.errors.len()), (0, 1, 0), "unchanged, with the server offline: {again:?}");
    let originals = std::fs::read_dir(&cache_root)
        .unwrap()
        .flatten()
        .flat_map(|fan| std::fs::read_dir(fan.path()).into_iter().flatten().flatten())
        .filter(|e| e.file_name().to_string_lossy().ends_with("-original"))
        .count();
    assert_eq!(originals, 0, "no originals in the export cache");
    // A new folder needs the original, and the client's own copy is only for display.
    let offline = api::artwork::run(&client, &export_into(&root.join("exported-again")), &Progress::none()).await.unwrap();
    assert_eq!((offline.written, offline.errors.len()), (0, 1), "{offline:?}");
    sqlx::query("UPDATE remote_sources SET base_url = ? WHERE id = ?").bind(base).bind(source_id).execute(&client.pool).await.unwrap();
    // Art this client streams instead of keeping (no row here at all): the server's row is
    // exported just the same.
    sqlx::query("UPDATE media m JOIN title_media tm ON tm.media_id = m.id SET m.deleted = TRUE WHERE tm.title_id = ? AND m.media_type = 'box' AND m.sub_category = 'front'").bind(title_id).execute(&client.pool).await.unwrap();
    let streamed_dir = root.join("exported-streamed");
    let streamed = api::artwork::run(&client, &export_into(&streamed_dir), &Progress::none()).await.unwrap();
    assert_eq!((streamed.written, streamed.errors.len()), (1, 0), "{streamed:?}");
    assert_eq!(std::fs::read(streamed_dir.join("Mirror Test (USA).png")).unwrap(), manual_front, "the server's original of a streamed type");
    db_settings::set(&client.pool, db_settings::KEY_INSTANCE_MODE, &"standalone").await.unwrap();

    // --- audio: a client keeps converted copies of the server's tagged tracks ---
    audio_copies_for_a_client(app, &client, source_id, title_id, &root).await;

    // --- list types (screenshots): opt-in, per file, capped ---
    let title_target = [MediaTarget::Title(title_id)];
    let shots = [shot_url.clone(), cover_url.clone()];
    let server_media = app.paths.media_dir();
    let attach = || pmedia::attach_media_list(&app.http, &app.pool, &server_media, igdb, Some("4242"), &shots, "screenshot", &title_target);
    db_settings::delete(&app.pool, db_settings::KEY_MEDIA_DOWNLOAD).await.unwrap();
    assert_eq!(attach().await.unwrap(), 0, "screenshots are off until ticked");
    db_settings::set(&app.pool, db_settings::KEY_MEDIA_DOWNLOAD, &json!({"igdb": {"screenshot": true}})).await.unwrap();
    db_settings::set(&app.pool, db_settings::KEY_MEDIA_LIST_CAP, &1u64).await.unwrap();
    assert_eq!(attach().await.unwrap(), 1, "only as many as the cap");
    assert_eq!(attach().await.unwrap(), 0, "and a file already there is not attached twice");
    let on_title: Vec<String> = sqlx::query_scalar("SELECT m.source_url FROM media m JOIN title_media tm ON tm.media_id = m.id WHERE tm.title_id = ? AND m.media_type = 'screenshot' AND m.deleted = FALSE").bind(title_id).fetch_all(&app.pool).await.unwrap();
    assert_eq!(on_title, vec![shot_url.clone()]);
    db_settings::delete(&app.pool, db_settings::KEY_MEDIA_DOWNLOAD).await.unwrap();
    db_settings::delete(&app.pool, db_settings::KEY_MEDIA_LIST_CAP).await.unwrap();

    // A machine with no main server has nothing to fetch from: the job is refused up front.
    let no_main = http.post(format!("{base}/api/v1/jobs")).bearer_auth(token).json(&json!({"job_type": "media_sync"})).send().await.unwrap();
    assert_eq!(no_main.status(), 400, "{}", no_main.text().await.unwrap());

    changes_only_sync(app, &client, source_id, base).await;

    host_handle.abort();
    sqlx::query("DROP DATABASE romexplorer_api_test_client").execute(&app.pool).await.unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

/// An update sync takes only what changed (`database::sync_rev`): inserts, real updates and
/// hard deletes, and nothing for a write that changed no value. A full sync takes everything.
async fn changes_only_sync(app: &App, client: &App, source_id: u16, base: &str) {
    use romexplorer::database::jobs::Progress;
    sqlx::query("UPDATE remote_sources SET base_url = ? WHERE id = ?").bind(base).bind(source_id).execute(&client.pool).await.unwrap();
    api::sync::pull(client, source_id, false, &Progress::none()).await.unwrap();

    let rev = |table: &'static str, id: u64| async move { sqlx::query_scalar::<_, u64>(sqlx::AssertSqlSafe(format!("SELECT sync_rev FROM `{table}` WHERE id = ?"))).bind(id).fetch_one(&app.pool).await.unwrap() };
    let title = |name: &'static str| async move { sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, LOWER(?), SHA1(LOWER(?)))").bind(name).bind(name).bind(name).execute(&app.pool).await.unwrap().last_insert_id() };
    let kept = title("Diff Kept").await;
    let renamed = title("Diff Before").await;
    let release = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash) VALUES (19, 'Diff (USA)', SHA1('Diff (USA)'), 'diff', SHA1('diff'))").execute(&app.pool).await.unwrap().last_insert_id();
    sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(kept).bind(release).execute(&app.pool).await.unwrap();
    api::sync::pull(client, source_id, false, &Progress::none()).await.unwrap();
    let on_client = |sql: &'static str, id: u64| async move { sqlx::query_scalar::<_, i64>(sql).bind(id).fetch_one(&client.pool).await.unwrap() };
    assert_eq!(on_client("SELECT COUNT(*) FROM title_releases WHERE release_id = ?", release).await, 1);

    // A write that changes nothing keeps its number; a real one takes a new one.
    let before = rev("title", kept).await;
    sqlx::query("UPDATE title SET name = name WHERE id = ?").bind(kept).execute(&app.pool).await.unwrap();
    assert_eq!(rev("title", kept).await, before, "an update that changed no value is not a change");
    sqlx::query("UPDATE title SET name = 'Diff After' WHERE id = ?").bind(renamed).execute(&app.pool).await.unwrap();
    assert!(rev("title", renamed).await > before);
    sqlx::query("DELETE FROM title_releases WHERE release_id = ?").bind(release).execute(&app.pool).await.unwrap();
    let added = title("Diff Added").await;

    let pulled = api::sync::pull(client, source_id, false, &Progress::none()).await.unwrap();
    for table in ["title", "title_releases"] {
        assert!(pulled.changes_only.iter().any(|t| t == table), "{table} came as changes: {pulled:?}");
    }
    assert!(pulled.incomplete.is_empty(), "{pulled:?}");
    let name: String = sqlx::query_scalar("SELECT name FROM title WHERE id = ?").bind(renamed).fetch_one(&client.pool).await.unwrap();
    assert_eq!(name, "Diff After");
    assert_eq!(on_client("SELECT COUNT(*) FROM title WHERE id = ?", added).await, 1, "the new row");
    assert_eq!(on_client("SELECT COUNT(*) FROM title_releases WHERE release_id = ?", release).await, 0, "the hard delete");

    // A rename that only changes case is a change, and so is a link moved to another title in
    // place: the client ends up with the new link only, not both.
    sqlx::query("UPDATE title SET name = 'DIFF AFTER' WHERE id = ?").bind(renamed).execute(&app.pool).await.unwrap();
    sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(kept).bind(release).execute(&app.pool).await.unwrap();
    api::sync::pull(client, source_id, false, &Progress::none()).await.unwrap();
    sqlx::query("UPDATE title_releases SET title_id = ? WHERE release_id = ?").bind(added).bind(release).execute(&app.pool).await.unwrap();
    api::sync::pull(client, source_id, false, &Progress::none()).await.unwrap();
    let name: String = sqlx::query_scalar("SELECT name FROM title WHERE id = ?").bind(renamed).fetch_one(&client.pool).await.unwrap();
    assert_eq!(name, "DIFF AFTER");
    let titles: Vec<u64> = sqlx::query_scalar("SELECT title_id FROM title_releases WHERE release_id = ?").bind(release).fetch_all(&client.pool).await.unwrap();
    assert_eq!(titles, vec![added], "moved, not copied");

    // The check stamps are not counted: an integrity run does not resend the whole table.
    let body: String = sqlx::query_scalar("SELECT ACTION_STATEMENT FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = DATABASE() AND TRIGGER_NAME = 'sync_rev_u_local_files'").fetch_one(&app.pool).await.unwrap();
    assert!(!body.contains("last_integrity_check") && body.contains("last_integrity_failure"), "{body}");
    // Never in an export: the number is this database's own.
    let page = api::sync::export(app, "title", None, 1, true).await.unwrap();
    assert!(page.rows[0].get("sync_rev").is_none(), "{:?}", page.rows[0]);

    // On demand, everything again -- and a row the source does not have is gone after it.
    let stray = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES ('Stray', 'stray', SHA1('stray'))").execute(&client.pool).await.unwrap().last_insert_id();
    let full = api::sync::pull(client, source_id, true, &Progress::none()).await.unwrap();
    assert!(full.changes_only.is_empty() && full.unchanged.is_empty(), "{full:?}");
    assert!(full.tables.iter().any(|(t, n)| t == "title" && *n >= 3), "{full:?}");
    assert_eq!(on_client("SELECT COUNT(*) FROM title WHERE id = ?", stray).await, 0, "reconciled with the source");
    assert_eq!(on_client("SELECT COUNT(*) FROM title WHERE id = ?", added).await, 1);
}

/// A 16-bit PCM WAV of a 440 Hz tone.
fn tone_wav(sample_rate: u32, channels: u16, seconds: u32) -> Vec<u8> {
    let frames = sample_rate * seconds;
    let data_len = frames * u32::from(channels) * 2;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend(b"RIFF");
    wav.extend((36 + data_len).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(channels.to_le_bytes());
    wav.extend(sample_rate.to_le_bytes());
    wav.extend((sample_rate * u32::from(channels) * 2).to_le_bytes());
    wav.extend((channels * 2).to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend(data_len.to_le_bytes());
    for n in 0..frames {
        let v = ((std::f32::consts::TAU * 440.0 * n as f32 / sample_rate as f32).sin() * 12000.0) as i16;
        for _ in 0..channels {
            wav.extend(v.to_le_bytes());
        }
    }
    wav
}

/// The server holds four tracks: a CD-quality WAV and an 8 kHz mono one, both tagged with a
/// keyword `title_id` carries; an untagged one; and a tagged WMA nothing can decode. A client
/// asking for AAC at 128 kbps gets the first converted (when the encoder library is built),
/// the second as it is (it is 128 kbps: not above the limit), and neither of the others.
async fn audio_copies_for_a_client(app: &App, client: &App, source_id: u16, title_id: u64, client_root: &std::path::Path) {
    use romexplorer::database::jobs::Progress;
    use romexplorer::database::settings as db_settings;
    use romexplorer::rom::xaac;

    let library = output_dir("server_audio");
    let _ = std::fs::remove_dir_all(&library);
    std::fs::create_dir_all(&library).unwrap();
    let big = tone_wav(44_100, 2, 3);
    let small = tone_wav(8_000, 1, 3);
    let mut ids = Vec::new();
    for (name, bytes, format, support, tag) in [
        ("big.wav", big.clone(), "wav", "direct", Some("mirror-ost")),
        ("small.wav", small.clone(), "wav", "direct", Some("mirror-ost")),
        ("untagged.wav", small.clone(), "wav", "direct", None),
        ("old.wma", b"not decodable".to_vec(), "wma", "unsupported", Some("mirror-ost")),
    ] {
        let path = library.join(name);
        std::fs::write(&path, &bytes).unwrap();
        let text = path.display().to_string();
        let id = sqlx::query("INSERT INTO audio_files (path, path_hash, file_name, size, format, playable, support, duration_ms, title, album) VALUES (?, SHA1(?), ?, ?, ?, ?, ?, 3000, ?, 'Mirror OST')")
            .bind(&text)
            .bind(&text)
            .bind(name)
            .bind(bytes.len() as u64)
            .bind(format)
            .bind(support != "unsupported")
            .bind(support)
            .bind(name)
            .execute(&app.pool)
            .await
            .unwrap()
            .last_insert_id();
        if let Some(tag) = tag {
            sqlx::query("INSERT INTO audio_file_tags (audio_file_id, name) VALUES (?, ?)").bind(id).bind(tag).execute(&app.pool).await.unwrap();
        }
        ids.push(id);
    }
    // The keyword on the title, on both machines (a pull would carry it; this is shorter).
    for pool in [&app.pool, &client.pool] {
        sqlx::query("INSERT IGNORE INTO keyword (name) VALUES ('mirror-ost')").execute(pool).await.unwrap();
        sqlx::query("INSERT IGNORE INTO title_keywords (title_id, keyword_id, source) SELECT ?, id, 'user' FROM keyword WHERE name = 'mirror-ost'").bind(title_id).execute(pool).await.unwrap();
    }

    // The bitrate rule on the server's own route, whatever the encoder's state.
    let big_served = api::audio::file(app, ids[0], &api::audio_convert::FileQuery { format: Some("aac".into()), bitrate: Some(128) }).await.unwrap();
    assert_eq!(big_served.converted, xaac::available(), "1411 kbps is above 128: converted wherever there is an encoder ({:?})", big_served.note);
    let small_served = api::audio::file(app, ids[1], &api::audio_convert::FileQuery { format: Some("aac".into()), bitrate: Some(128) }).await.unwrap();
    assert!(!small_served.converted, "8 kHz mono is 128 kbps: not above the limit");
    assert!(api::audio::file(app, ids[3], &Default::default()).await.is_err(), "nothing decodes a WMA: it is not served at all");

    // A title with no audio of its own is silent until `audio_fallback` says what stands in.
    let bare = api::audio::PlaylistQuery { title_id: Some(title_id + 1_000_000), release_id: None, ra_game_id: None, fallback: None, ..Default::default() };
    let own = api::audio::PlaylistQuery { title_id: Some(title_id), release_id: None, ra_game_id: None, fallback: None, ..Default::default() };
    assert!(api::audio::playlist(app, &bare).await.unwrap().tracks.is_empty(), "unset is none");
    db_settings::set(&app.pool, db_settings::KEY_AUDIO_FALLBACK, &"random").await.unwrap();
    let random = api::audio::playlist(app, &bare).await.unwrap();
    assert_eq!((random.tracks.len(), random.fallback.as_deref()), (2, Some("random")), "the two playable tracks that are a title's; not the untagged one, not the WMA");
    let theirs = api::audio::playlist(app, &own).await.unwrap();
    assert_eq!((theirs.tracks.len(), theirs.fallback), (2, None), "a title with audio plays its own");
    db_settings::set(&app.pool, db_settings::KEY_AUDIO_FALLBACK, &"generic_bgm").await.unwrap();
    assert!(api::audio::playlist(app, &bare).await.unwrap().tracks.is_empty(), "nothing carries the tag yet");
    // The tag goes on a file under its one spelling, with no keyword behind it for a title
    // to take, and comes off again.
    let generic = api::audio::AudioTagRequest { ids: vec![ids[2]], name: " Generic_BGM ".into() };
    assert_eq!(api::audio::add_tag(app, &generic).await.unwrap(), 1);
    let played = api::audio::playlist(app, &bare).await.unwrap();
    assert_eq!((played.tracks.iter().map(|t| t.id).collect::<Vec<_>>(), played.fallback.as_deref()), (vec![ids[2]], Some("generic_bgm")));
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM keyword WHERE name = 'generic_bgm'").fetch_one(&app.pool).await.unwrap(), 0);
    assert!(api::titles::add_keyword(app, &api::titles::KeywordRequest { title_ids: vec![title_id], name: "generic_bgm".into() }).await.is_err(), "not a title's to carry");
    assert_eq!(api::audio::suggest_tags(app, &api::audio::TagQuery { q: Some("generic".into()), scope: None, title_ids: None }).await.unwrap()[0].name, "generic_bgm");
    assert_eq!(api::audio::playlist(app, &api::audio::PlaylistQuery { fallback: Some("none".into()), ..bare.clone() }).await.unwrap().tracks.len(), 0, "the caller's own answer beats the setting");
    assert_eq!(api::audio::remove_tag(app, &generic).await.unwrap(), 1);
    db_settings::set(&app.pool, db_settings::KEY_AUDIO_FALLBACK, &"none").await.unwrap();

    // A client streams until told to keep: the job is refused with the reason.
    db_settings::set(&client.pool, db_settings::KEY_INSTANCE_MODE, &"client").await.unwrap();
    assert!(api::audio_sync::validate(client).await.is_err());
    let query = api::audio::PlaylistQuery { title_id: Some(title_id), release_id: None, ra_game_id: None, fallback: None, ..Default::default() };
    let streamed = api::audio::playlist(client, &query).await.unwrap();
    assert_eq!(streamed.tracks.iter().filter(|t| t.remote).count(), 2, "both playable tagged tracks, streamed: {:?}", streamed.tracks.iter().map(|t| &t.file_name).collect::<Vec<_>>());

    db_settings::set(&client.pool, db_settings::KEY_MEDIA_CLIENT_KEEP, &json!({"bgm": true})).await.unwrap();
    db_settings::set(&client.pool, db_settings::KEY_AUDIO_CLIENT_FORMAT, &"aac").await.unwrap();
    db_settings::set(&client.pool, db_settings::KEY_AUDIO_CLIENT_BITRATE, &128u32).await.unwrap();
    let first = api::audio_sync::run(client, &Progress::none()).await.unwrap();
    assert_eq!((first.wanted, first.fetched, first.already_held, first.failed), (2, 2, 0, 0), "{first:?}");
    assert_eq!(first.converted, u64::from(xaac::available()), "{first:?}");

    let rows: Vec<(String, String, u64, Option<u64>, Option<String>)> = sqlx::query_as(
        "SELECT a.path, a.format, a.size, a.remote_id, (SELECT GROUP_CONCAT(t.name) FROM audio_file_tags t WHERE t.audio_file_id = a.id) FROM audio_files a WHERE a.remote_source_id = ? AND a.deleted = FALSE ORDER BY a.remote_id",
    )
    .bind(source_id)
    .fetch_all(&client.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2, "the untagged track and the WMA were not copied: {rows:?}");
    assert_eq!(rows.iter().map(|r| r.3).collect::<Vec<_>>(), vec![Some(ids[0]), Some(ids[1])]);
    assert!(rows.iter().all(|r| r.4.as_deref() == Some("mirror-ost")), "the server's tags came along: {rows:?}");
    assert!(rows.iter().all(|r| std::path::Path::new(&r.0).starts_with(client_root)), "under the client's own root: {rows:?}");
    assert_eq!(std::fs::read(&rows[1].0).unwrap(), small, "at or below the bitrate: byte for byte the original");
    if xaac::available() {
        assert_eq!(rows[0].1, "m4a");
        assert!(rows[0].2 < big.len() as u64 / 5, "a 1411 kbps WAV as 128 kbps AAC: {} of {} bytes", rows[0].2, big.len());
        let decoded = romexplorer::rom::audio_decode::decode_file(std::path::Path::new(&rows[0].0)).expect("the copy is a playable AAC file");
        assert_eq!((decoded.sample_rate, decoded.channels), (44_100, 2));
    } else {
        eprintln!("audio conversion not exercised: rxaac is not built for this machine");
        assert_eq!(std::fs::read(&rows[0].0).unwrap(), big, "no encoder on the server: the original");
    }

    // A second run has nothing to do, and the playlist lists each track once, as local.
    let again = api::audio_sync::run(client, &Progress::none()).await.unwrap();
    assert_eq!((again.fetched, again.already_held, again.failed), (0, 2, 0), "{again:?}");
    let kept = api::audio::playlist(client, &query).await.unwrap();
    assert_eq!(kept.tracks.len(), 2, "{:?}", kept.tracks.iter().map(|t| (&t.file_name, t.remote)).collect::<Vec<_>>());
    assert!(kept.tracks.iter().all(|t| !t.remote));
    // It is a sync: the server retags one track, renames its album, and untags the other.
    sqlx::query("UPDATE audio_file_tags SET name = 'mirror-ost-2' WHERE audio_file_id = ?").bind(ids[0]).execute(&app.pool).await.unwrap();
    sqlx::query("UPDATE audio_files SET album = 'Mirror OST (Remaster)' WHERE id = ?").bind(ids[0]).execute(&app.pool).await.unwrap();
    sqlx::query("DELETE FROM audio_file_tags WHERE audio_file_id = ?").bind(ids[1]).execute(&app.pool).await.unwrap();
    let removed_path = rows[1].0.clone();
    let synced = api::audio_sync::run(client, &Progress::none()).await.unwrap();
    assert_eq!((synced.wanted, synced.fetched, synced.already_held, synced.updated, synced.removed, synced.failed), (1, 0, 1, 1, 1, 0), "{synced:?}");
    let after: Vec<(Option<u64>, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT a.remote_id, a.album, (SELECT GROUP_CONCAT(t.name) FROM audio_file_tags t WHERE t.audio_file_id = a.id) FROM audio_files a WHERE a.remote_source_id = ?").bind(source_id).fetch_all(&client.pool).await.unwrap();
    assert_eq!(after, vec![(Some(ids[0]), Some("Mirror OST (Remaster)".to_string()), Some("mirror-ost-2".to_string()))], "the kept copy follows the server; the untagged one is gone");
    assert!(!std::path::Path::new(&removed_path).exists(), "and so is its file");
    assert!(std::path::Path::new(&rows[0].0).is_file(), "the other copy was not fetched again or touched");
    let unchanged = api::audio_sync::run(client, &Progress::none()).await.unwrap();
    assert_eq!((unchanged.updated, unchanged.removed, unchanged.fetched), (0, 0, 0), "nothing changed, nothing done: {unchanged:?}");
    // Tagged again on the server: fetched again.
    sqlx::query("INSERT INTO audio_file_tags (audio_file_id, name) VALUES (?, 'mirror-ost')").bind(ids[1]).execute(&app.pool).await.unwrap();
    sqlx::query("UPDATE audio_file_tags SET name = 'mirror-ost' WHERE audio_file_id = ?").bind(ids[0]).execute(&app.pool).await.unwrap();
    let back = api::audio_sync::run(client, &Progress::none()).await.unwrap();
    assert_eq!((back.fetched, back.updated, back.removed), (1, 1, 0), "{back:?}");

    // A full scan of the client's own folders leaves the fetched copies alone.
    sqlx::query("UPDATE audio_files SET deleted = TRUE WHERE deleted = FALSE AND remote_id IS NULL").execute(&client.pool).await.unwrap();
    assert_eq!(api::audio::playlist(client, &query).await.unwrap().tracks.len(), 2);

    for key in [db_settings::KEY_MEDIA_CLIENT_KEEP, db_settings::KEY_AUDIO_CLIENT_FORMAT, db_settings::KEY_AUDIO_CLIENT_BITRATE] {
        db_settings::delete(&client.pool, key).await.unwrap();
    }
    db_settings::set(&client.pool, db_settings::KEY_INSTANCE_MODE, &"standalone").await.unwrap();
    let _ = std::fs::remove_dir_all(&library);
}

/// Accounts end to end: the setup token creates the admin and is then retired, each role gets
/// exactly what it should, and signing out or changing a password ends sessions.
async fn accounts_over_rest(app: &App, http: &reqwest::Client, base: &str, setup: &str, directory_id: u64) {
    use serde_json::Value;
    let login = |name: &'static str, password: &'static str| async move {
        let r = http.post(format!("{base}/api/v1/auth/login")).json(&json!({ "username": name, "password": password, "device": "test" })).send().await.unwrap();
        (r.status().as_u16(), r.json::<Value>().await.unwrap_or(Value::Null))
    };
    let status = |method: reqwest::Method, path: &str, token: &str, body: Option<Value>| {
        let mut req = http.request(method, format!("{base}{path}")).bearer_auth(token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        async move { req.send().await.unwrap().status().as_u16() }
    };
    use reqwest::Method as M;

    // Before any admin: the setup token is admin, and /me says accounts aren't ready.
    let me: Value = http.get(format!("{base}/api/v1/auth/me")).bearer_auth(setup).send().await.unwrap().json().await.unwrap();
    assert_eq!(me["kind"], "setup");
    assert_eq!(me["accounts_ready"], false);
    assert_eq!(status(M::POST, "/api/v1/users", setup, Some(json!({ "username": "boss", "password": "short", "role": "admin" }))).await, 400, "too short");
    assert_eq!(status(M::POST, "/api/v1/users", setup, Some(json!({ "username": "boss", "password": "adminpass1", "role": "admin" }))).await, 201);
    // ... and from that moment the setup token is refused, and its file is gone.
    assert_eq!(status(M::GET, "/api/v1/status", setup, None).await, 401);
    assert!(!app.paths.server_token_path().exists());

    let (code, body) = login("boss", "adminpass1").await;
    assert_eq!(code, 200);
    let admin = body["token"].as_str().unwrap().to_string();
    assert_eq!(body["me"]["admin"], true);
    // A wrong password and an unknown name get the same answer.
    let (bad_pw, bad_body) = login("boss", "wrongpass1").await;
    let (no_user, no_body) = login("nobody", "wrongpass1").await;
    assert_eq!((bad_pw, no_user), (401, 401));
    assert_eq!(bad_body["error"]["message"], no_body["error"]["message"]);

    assert_eq!(status(M::POST, "/api/v1/users", &admin, Some(json!({ "username": "bob", "password": "bobpass12" }))).await, 201);
    assert_eq!(status(M::POST, "/api/v1/users", &admin, Some(json!({ "username": "carol", "password": "carolpass1", "can_upload": true }))).await, 201);
    assert_eq!(status(M::POST, "/api/v1/users", &admin, Some(json!({ "username": "BOB", "password": "bobpass12" }))).await, 409, "names are case-insensitive");

    // A regular account reads, and changes nothing.
    let bob = login("bob", "bobpass12").await.1["token"].as_str().unwrap().to_string();
    assert_eq!(status(M::GET, "/api/v1/catalog/titles", &bob, None).await, 200);
    assert_eq!(status(M::GET, "/api/v1/sync/manifest", &bob, None).await, 200);
    assert_eq!(status(M::GET, "/api/v1/media/999999", &bob, None).await, 404, "allowed, just not there");
    assert_eq!(status(M::PUT, "/api/v1/settings/cache_ttl_days", &bob, Some(json!(3))).await, 403);
    assert_eq!(status(M::GET, "/api/v1/backup", &bob, None).await, 403);
    assert_eq!(status(M::GET, "/api/v1/users", &bob, None).await, 403);
    assert_eq!(status(M::GET, "/api/v1/ra/games/1?refresh=true", &bob, None).await, 403, "reaching RA is an admin's call");
    let new_only = json!({ "job_type": "file_scan", "directory_id": directory_id, "mode": "new_only", "platform": "all" });
    let full = json!({ "job_type": "file_scan", "directory_id": directory_id, "mode": "full", "platform": "all" });
    assert_eq!(status(M::POST, "/api/v1/jobs", &bob, Some(new_only.clone())).await, 403, "no upload permission");

    // Upload: a new-files scan and nothing else.
    let carol = login("carol", "carolpass1").await.1["token"].as_str().unwrap().to_string();
    assert_eq!(status(M::POST, "/api/v1/jobs", &carol, Some(new_only)).await, 201);
    assert_eq!(status(M::POST, "/api/v1/jobs", &carol, Some(full)).await, 403);
    assert_eq!(status(M::PUT, "/api/v1/settings/cache_ttl_days", &carol, Some(json!(3))).await, 403);

    // A second device, then a password change from the first: the second is signed out.
    let bob2 = login("bob", "bobpass12").await.1["token"].as_str().unwrap().to_string();
    let devices: Value = http.get(format!("{base}/api/v1/auth/devices")).bearer_auth(&bob).send().await.unwrap().json().await.unwrap();
    assert_eq!(devices.as_array().unwrap().len(), 2);
    assert_eq!(status(M::PUT, "/api/v1/auth/password", &bob, Some(json!({ "current": "wrong-one", "new": "newbobpass" }))).await, 401);
    assert_eq!(status(M::PUT, "/api/v1/auth/password", &bob, Some(json!({ "current": "bobpass12", "new": "newbobpass" }))).await, 200);
    assert_eq!(status(M::GET, "/api/v1/status", &bob2, None).await, 401, "the other device is signed out");
    assert_eq!(status(M::GET, "/api/v1/status", &bob, None).await, 200, "this one stays in");
    assert_eq!(status(M::POST, "/api/v1/auth/logout", &bob, None).await, 200);
    assert_eq!(status(M::GET, "/api/v1/status", &bob, None).await, 401);

    // A browser: the token lives in an HttpOnly cookie, never in the page.
    let r = http.post(format!("{base}/api/v1/auth/login")).json(&json!({ "username": "carol", "password": "carolpass1", "cookie": true })).send().await.unwrap();
    let cookie = r.headers().get("set-cookie").unwrap().to_str().unwrap().to_string();
    assert!(cookie.starts_with("rx_session=") && cookie.contains("HttpOnly"), "{cookie}");
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["token"], "");
    let session = cookie.split(';').next().unwrap().to_string();
    assert_eq!(http.get(format!("{base}/api/v1/status")).header("cookie", session).send().await.unwrap().status(), 200);

    // The server's own console is for admins. A regular account is refused there -- with no
    // session left behind -- while the same account still signs in as a client, and an admin
    // signs in to the console as usual.
    let console = |name: &'static str, password: &'static str| async move {
        let r = http.post(format!("{base}/api/v1/auth/login")).json(&json!({ "username": name, "password": password, "device": "console", "cookie": true, "console": true })).send().await.unwrap();
        (r.status().as_u16(), r.headers().get("set-cookie").is_some())
    };
    assert_eq!(console("carol", "carolpass1").await, (403, false), "a regular account, and no cookie handed out");
    assert_eq!(console("carol", "wrong-pass1").await.0, 401, "the password is still checked first");
    assert_eq!(login("carol", "carolpass1").await.0, 200, "as a client it signs in as before");
    assert_eq!(console("boss", "adminpass1").await, (200, true));

    // The last admin can't be removed or demoted: nobody could sign in to fix it.
    let users: Value = http.get(format!("{base}/api/v1/users")).bearer_auth(&admin).send().await.unwrap().json().await.unwrap();
    let boss_id = users.as_array().unwrap().iter().find(|u| u["username"] == "boss").unwrap()["id"].as_u64().unwrap();
    assert_eq!(status(M::DELETE, &format!("/api/v1/users/{boss_id}"), &admin, None).await, 409);
    assert_eq!(status(M::PUT, &format!("/api/v1/users/{boss_id}"), &admin, Some(json!({ "username": "boss", "role": "user" }))).await, 409);

    // Backups keep the accounts but never their hashes.
    use std::io::Read;
    let backup = api::backup::create(app, api::backup::Kind::Database, &romexplorer::database::jobs::Progress::none()).await.unwrap();
    let path = api::backup::file(app, &backup.file.name).await.unwrap();
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let mut text = String::new();
    zip.by_name("users.ndjson").unwrap().read_to_string(&mut text).unwrap();
    assert!(text.contains("boss") && !text.contains("$argon2"), "accounts yes, hashes no");
    assert!(zip.by_name("user_tokens.ndjson").is_err(), "devices are never backed up");
    let _ = std::fs::remove_file(&path);

    // With no admin able to sign in (as after a restore onto a fresh install), the setup token
    // works again, so nobody is locked out.
    sqlx::query("UPDATE users SET password_hash = NULL WHERE role = 'admin'").execute(&app.pool).await.unwrap();
    assert_eq!(status(M::GET, "/api/v1/status", setup, None).await, 200);
    assert_eq!(status(M::GET, "/api/v1/status", &admin, None).await, 401, "an account with no password can't be used");

    // auth.env: applied while no admin can sign in, then deleted...
    let seed = app.paths.auth_env_path();
    std::fs::write(&seed, "admin_username = boss\nadmin_password = seededpass1\nuser.dave = davepass12\nupload.dave = true\n").unwrap();
    assert_eq!(api::auth::seed_from_file(app).await.unwrap(), api::auth::SeedOutcome::Applied { users: 1 });
    assert!(!seed.exists(), "a file of passwords doesn't stay on disk");
    assert_eq!(login("boss", "seededpass1").await.0, 200, "the existing admin got the seeded password");
    assert_eq!(login("dave", "davepass12").await.0, 200);
    assert_eq!(status(M::GET, "/api/v1/status", setup, None).await, 401, "and the setup token is retired again");
    // ...and once an admin can, a new file is never applied: dropping a file is not a reset.
    std::fs::write(&seed, "admin_username = boss\nadmin_password = takeover99\n").unwrap();
    assert_eq!(api::auth::seed_from_file(app).await.unwrap(), api::auth::SeedOutcome::Ignored);
    assert!(!seed.exists());
    assert_eq!(login("boss", "takeover99").await.0, 401);
}

#[test]
fn api_and_server_end_to_end() {
    let _ = env_logger::builder().is_test(true).try_init();
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    let base = output_dir("");
    runtime.block_on(async {
        let (mut server, app) = bring_up(&base).await;
        let directory_id = directories(&app, &base).await;
        storage(&app).await;
        settings(&app).await;
        jobs(&app, directory_id).await;
        db_viewer(&app).await;
        catalog_empty(&app).await;
        sets(&app).await;
        release_buckets(&app).await;
        views(&app).await;
        playlists(&app, &base).await;
        dsi_cdn_folder(&app, &base).await;
        scan_containers(&app, &base).await;
        scan_platforms(&app, &base).await;
        scan_replaced_files(&app, &base).await;
        scan_goes_on_from_where_it_was(&app, &base).await;
        rescan_one_file(&app, &base).await;
        naming(&app).await;
        mounts(&app, &base).await;
        secrets_stay_in(&app, &base).await;
        copy_plan_and_run(&app, &base).await;
        copy_from_a_mounted_server(&app, &base).await;
        copy_modes(&app, &base).await;
        copy_converts(&app, &base).await;
        integrity(&app, &base).await;
        media_backfill(&app, &base).await;
        media_for_a_selection(&app, &base).await;
        artwork_export(&app, &base).await;
        igdb_localized_covers(&app, &base).await;
        provider_metadata(&app, &base).await;
        lost_title_links(&app).await;
        title_picker(&app).await;
        review_queues(&app).await;
        sets_file(&app).await;
        rest(app, directory_id).await;
        server.shutdown(std::time::Duration::from_secs(20)).await.expect("shutdown");
    });
    let _ = std::fs::remove_dir_all(&base);
}

/// "Rescan this file" reads one file again and links its game to the release a DAT lists it
/// under, on the spot. And the two ways a file a DAT lists stayed unlinked: a scan of a
/// subfolder whose files a scan of the folder above recorded never matched them, and a DAT
/// imported after the scan queued no match.
async fn rescan_one_file(app: &App, base: &PathBuf) {
    use romexplorer::database::jobs::Progress;
    let root = base.join("collection");
    let dir = root.join("rescan");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("a")).unwrap();
    let path = |n: &str| dir.join("a").join(format!("Rescan {n}.bin"));
    std::fs::write(path("One"), vec![0x31u8; 3000]).unwrap();
    std::fs::write(path("Two"), vec![0x32u8; 3000]).unwrap();
    let root_id = api::directories::resolve(app, &root.display().to_string()).await.unwrap().directory.unwrap().id;
    let scan = |directory_id: u64, mode: FileScanMode, file_id: Option<u64>| api::jobs::FileScanRequest { directory_id, mode, platform: api::jobs::PlatformSelect::All, hash_match: None, file_id };
    api::scan::run_file_scan(app, &scan(root_id, FileScanMode::NewOnly, None), &Progress::none()).await.unwrap();
    // The subfolder is configured after the folder above it was scanned.
    let child = api::directories::create(app, &api::directories::DirectorySpec { path: dir.display().to_string(), ..Default::default() }).await.unwrap().directory.id;
    let file = |n: &'static str| async move {
        sqlx::query_as::<_, (u64, u64, u64)>("SELECT id, content_id, directory_id FROM local_files WHERE path = ? AND deleted = FALSE").bind(path(n).display().to_string()).fetch_one(&app.pool).await.unwrap()
    };
    // A DAT that lists the files' bytes arrives after the scan.
    let release = |n: &'static str, content: u64| async move {
        let name = format!("Rescan {n} (Japan)");
        let title = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, ?, SHA1(?))").bind(format!("Rescan {n}")).bind(format!("rescan {n}")).bind(format!("rescan {n}")).execute(&app.pool).await.unwrap().last_insert_id();
        let id = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region) VALUES (0, ?, SHA1(?), ?, SHA1(?), 'JP')").bind(&name).bind(&name).bind(name.to_lowercase()).bind(name.to_lowercase()).execute(&app.pool).await.unwrap().last_insert_id();
        sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(title).bind(id).execute(&app.pool).await.unwrap();
        sqlx::query("INSERT INTO release_contents (release_id, content_id, file_name) VALUES (?, ?, ?)").bind(id).bind(content).bind(format!("{name}.bin")).execute(&app.pool).await.unwrap();
        (id, title)
    };
    let (one, one_content, one_dir) = file("One").await;
    assert_eq!(one_dir, root_id, "recorded by the scan of the folder above");
    let (one_release, one_title) = release("One", one_content).await;

    // "Rescan this file": read again, its game linked to the DAT's release and its title.
    let rescanned = api::scan::run_file_scan(app, &scan(child, FileScanMode::Rehash, Some(one)), &Progress::none()).await.unwrap();
    let outcome = rescanned.file_rescan.expect("a file rescan's outcome");
    assert_eq!(outcome.read, vec![path("One").display().to_string()]);
    assert_eq!((outcome.games, outcome.unmatched), (1, 0));
    assert!(outcome.linked.len() == 1 && outcome.linked[0].contains("Rescan One (Japan)") && outcome.linked[0].contains("title Rescan One"), "{outcome:?}");
    assert!(outcome.listed_by.iter().any(|r| r.starts_with("Rescan One (Japan)")), "{outcome:?}");
    let detail = api::catalog::file(app, one).await.unwrap();
    let releases = &detail.owners[0].releases;
    assert!(releases.len() == 1 && releases[0].release_id == one_release && releases[0].title_id == Some(one_title), "the file page shows the release and title: {releases:?}");
    assert_eq!(detail.file.directory_id, Some(child), "recorded under the nearest configured directory");
    // Again: already linked, nothing new.
    let again = api::scan::run_file_scan(app, &scan(child, FileScanMode::Rehash, Some(one)), &Progress::none()).await.unwrap().file_rescan.unwrap();
    assert!(again.linked.is_empty() && again.already_linked.len() == 1, "{again:?}");

    // A quick scan of the subfolder takes over what the scan above recorded, so the match
    // after it reaches the file.
    let (_, two_content, _) = file("Two").await;
    release("Two", two_content).await;
    std::fs::write(path("Three"), vec![0x33u8; 3000]).unwrap();
    api::scan::run_file_scan(app, &scan(child, FileScanMode::NewOnly, None), &Progress::none()).await.unwrap();
    assert_eq!(file("Two").await.2, child, "taken over by the subfolder");
    let req = api::jobs::HashMatchRequest { directory_id: child, to_dat: true, to_hasheous: false, to_playmatch: false, all: false };
    api::scan::run_hash_match(app, &req, &Progress::none()).await.unwrap();
    let linked = |n: &'static str| async move {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM local_files f JOIN local_rom_files lrf ON lrf.file_id = f.id JOIN local_game_roms lgr ON lgr.rom_id = lrf.rom_id JOIN local_release_games lrg ON lrg.game_id = lgr.game_id WHERE f.path = ?")
            .bind(path(n).display().to_string())
            .fetch_one(&app.pool)
            .await
            .unwrap()
    };
    assert_eq!(linked("Two").await, 1, "the subfolder's match linked the file the folder above recorded");
    assert_eq!(linked("Three").await, 0, "no DAT lists it yet");

    // A DAT that lists it now: the re-link after a DAT sync queues a match for its directory.
    let (_, three_content, _) = file("Three").await;
    release("Three", three_content).await;
    let queued = api::sync::relink_matchable(app).await.unwrap();
    assert!(!queued.is_empty(), "a match queued for the directory holding it");
    let payloads: Vec<serde_json::Value> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT payload FROM jobs WHERE id IN ({})", queued.iter().map(u64::to_string).collect::<Vec<_>>().join(",")))).fetch_all(&app.pool).await.unwrap();
    assert!(payloads.iter().any(|p| p["directory_id"] == json!(child)), "{payloads:?}");
    sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM jobs WHERE id IN ({})", queued.iter().map(u64::to_string).collect::<Vec<_>>().join(",")))).execute(&app.pool).await.unwrap();
    api::directories::delete(app, child, false).await.ok();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A peer's token never leaves the database through a generic reader: the viewer masks it and
/// refuses to filter or sort on it, a database backup and a SQLite cache carry NULL instead,
/// and restoring such a backup keeps the token a live database already has.
async fn secrets_stay_in(app: &App, base: &PathBuf) {
    use std::io::Read;
    use romexplorer::database::jobs::Progress;

    const TOKEN: &str = "SEKRIT-7f3a9c";
    let source = api::sources::create(
        app,
        &api::sources::SourceSpec { name: "secret peer".into(), base_url: "http://127.0.0.1:59998".into(), token: Some(TOKEN.into()), role: "peer".into(), enabled: true, replace_catalog: false, media: Default::default(), username: None, password: None },
    )
    .await
    .unwrap();
    // Stored sealed (`database::vault`), and opening it gives the token back.
    let raw = || async { sqlx::query_scalar::<_, Option<String>>("SELECT token FROM remote_sources WHERE id = ?").bind(source.id).fetch_one(&app.pool).await.unwrap() };
    let stored = || async { raw().await.map(|v| app.vault.open("remote_sources.token", &v).unwrap()) };
    assert!(raw().await.is_some_and(|v| v.starts_with("enc:v1:") && !v.contains(TOKEN)), "sealed at rest");
    assert_eq!(stored().await.as_deref(), Some(TOKEN));

    // The viewer: set, but not shown.
    let page = api::db::rows(app, "remote_sources", &api::db::RowsQuery::default()).await.unwrap();
    let token_idx = page.columns.iter().position(|c| c == "token").unwrap();
    let id_idx = page.columns.iter().position(|c| c == "id").unwrap();
    let row = page.rows.iter().find(|r| r[id_idx].as_deref() == Some(&source.id.to_string())).unwrap();
    assert_eq!(row[token_idx].as_deref(), Some("********"));
    assert!(!format!("{:?}", page.rows).contains(TOKEN));
    // A prefix filter would recover it a character at a time; a sort would order it.
    let refused = |q: api::db::RowsQuery| async move { matches!(api::db::rows(app, "remote_sources", &q).await, Err(e) if e.kind == ErrorKind::Invalid) };
    assert!(refused(api::db::RowsQuery { filter: Some("SEK".into()), column: Some("token".into()), ..Default::default() }).await);
    assert!(refused(api::db::RowsQuery { sort: Some("token".into()), ..Default::default() }).await);
    // Filtering every column skips the token instead of matching on it.
    let all = api::db::rows(app, "remote_sources", &api::db::RowsQuery { filter: Some("SEKRIT".into()), ..Default::default() }).await.unwrap();
    assert_eq!(all.total, 0, "a whole-row filter must not match on the token");

    // The database backup: the row is there, the token is not.
    let backup = api::backup::create(app, api::backup::Kind::Database, &Progress::none()).await.unwrap();
    let path = api::backup::file(app, &backup.file.name).await.unwrap();
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let mut text = String::new();
    zip.by_name("remote_sources.ndjson").unwrap().read_to_string(&mut text).unwrap();
    assert!(text.contains("secret peer"), "the peer itself is backed up");
    assert!(!text.contains(TOKEN), "its token is not");
    let exported: serde_json::Value = text.lines().map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()).find(|v| v["id"].as_u64() == Some(source.id as u64)).unwrap();
    assert!(exported.get("token").is_some_and(|t| t.is_null()), "the key stays, as NULL: {exported}");

    // Restored over the live database, the token already there is kept.
    let writer = api::rows::Writer::new(app, "remote_sources", Vec::new()).await.unwrap();
    let mut conn = app.pool.acquire().await.unwrap();
    writer.write(&mut conn, std::slice::from_ref(&exported)).await.unwrap();
    assert_eq!(stored().await.as_deref(), Some(TOKEN), "a restore must not blank a live token");
    // Restored onto a database without the row, the peer comes back with no token.
    sqlx::query("DELETE FROM remote_sources WHERE id = ?").bind(source.id).execute(&mut *conn).await.unwrap();
    writer.write(&mut conn, std::slice::from_ref(&exported)).await.unwrap();
    assert_eq!(stored().await, None, "re-entered on the Sources screen, never recovered from a backup");
    drop(conn);
    let _ = std::fs::remove_file(&path);

    // The SQLite cache a phone downloads.
    sqlx::query("UPDATE remote_sources SET token = ? WHERE id = ?").bind(TOKEN).bind(source.id).execute(&app.pool).await.unwrap();
    let cache = base.join("secrets_cache.sqlite");
    api::sqlite::export(app, &["remote_sources"], &cache, &Progress::none()).await.unwrap();
    let bytes = std::fs::read(&cache).unwrap();
    assert!(bytes.windows(b"secret peer".len()).any(|w| w == b"secret peer"), "the peer is in the cache");
    assert!(!bytes.windows(TOKEN.len()).any(|w| w == TOKEN.as_bytes()), "its token is not");
    let _ = std::fs::remove_file(&cache);

    api::sources::delete(app, source.id).await.unwrap();
}

/// A DSi CDN dump is a folder per game -- its content `00000001`, `tmd.1` (whose title id says
/// DSiWare), the title key and password -- filed under DSi. Scanned before its TMD was there,
/// each file was a rom of its own; scanned again, the folder is one rom and those are retired.
async fn dsi_cdn_folder(app: &App, base: &PathBuf) {
    use romexplorer::common::models::Platform;
    let dir = base.join("collection").join("dsi_cdn");
    let game = dir.join("Hoge Quest (Europe)");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&game).unwrap();
    std::fs::write(game.join("00000001"), vec![0x5Au8; 4096]).unwrap();
    std::fs::write(game.join("titleKey.bin"), [7u8; 16]).unwrap();
    std::fs::write(game.join("titlePass.txt"), b"hogepass").unwrap();
    let view = api::directories::create(app, &api::directories::DirectorySpec { path: dir.display().to_string(), app_platform_id: Some(Platform::NintendoDsi.igdb_id()), ..Default::default() }).await.unwrap();
    let req = api::jobs::FileScanRequest { directory_id: view.directory.id, mode: FileScanMode::Full, platform: api::jobs::PlatformSelect::All, hash_match: None, file_id: None };
    let live = || async {
        sqlx::query_as::<_, (String, i64)>("SELECT lr.main_path, (SELECT COUNT(*) FROM local_rom_files lrf WHERE lrf.rom_id = lr.id) FROM local_rom lr WHERE lr.main_path LIKE '%Hoge Quest (Europe)%' AND lr.deleted = FALSE ORDER BY lr.main_path")
            .fetch_all(&app.pool)
            .await
            .unwrap()
    };
    api::scan::run_file_scan(app, &req, &romexplorer::database::jobs::Progress::none()).await.unwrap();
    let before = live().await;
    let mut tmd = vec![0u8; 0x208];
    tmd[0..4].copy_from_slice(&0x0001_0001u32.to_be_bytes());
    tmd[0x140 + 0x4C..0x140 + 0x54].copy_from_slice(&0x0003_0004_4B48_4F45u64.to_be_bytes());
    tmd[0x140 + 0x9E..0x140 + 0xA0].copy_from_slice(&1u16.to_be_bytes());
    std::fs::write(game.join("tmd.1"), &tmd).unwrap();
    api::scan::run_file_scan(app, &req, &romexplorer::database::jobs::Progress::none()).await.unwrap();
    let after = live().await;
    assert!(before.len() > 1, "without its TMD the folder is loose files: {before:?}");
    assert_eq!(after.len(), 1, "one rom for the folder, the loose ones retired: {after:?}");
    assert!(after[0].0.ends_with("Hoge Quest (Europe)") && after[0].1 == 4, "the folder with all four files: {after:?}");
    let games: i64 = sqlx::query_scalar("SELECT COUNT(DISTINCT lg.id) FROM local_game lg JOIN local_game_roms lgr ON lgr.game_id = lg.id JOIN local_rom lr ON lr.id = lgr.rom_id WHERE lr.main_path LIKE '%Hoge Quest (Europe)%' AND lg.deleted = FALSE AND lr.deleted = FALSE").fetch_one(&app.pool).await.unwrap();
    assert_eq!(games, 1, "one game");
    let platform: u16 = sqlx::query_scalar("SELECT app_platform_id FROM local_rom WHERE main_path LIKE '%Hoge Quest (Europe)' AND deleted = FALSE").fetch_one(&app.pool).await.unwrap();
    assert_eq!(platform, Platform::NintendoDsi.igdb_id(), "filed as DSi, as its folder is");
    api::directories::delete(app, view.directory.id, false).await.ok();
}

/// An .m3u makes its discs one game: two cue/bin discs and the playlist listing them are
/// scanned into one game that points at the playlist, with disc numbers from its order.
async fn playlists(app: &App, base: &PathBuf) {
    let dir = base.join("collection").join("ps1m3u");
    std::fs::create_dir_all(&dir).unwrap();
    for n in 1..=2 {
        let bin = format!("Playlist Game (Disc {n}).bin");
        // A real PlayStation filesystem: the platform is only recorded on a positive signature.
        std::fs::write(dir.join(&bin), fixtures::ps1_disc(&format!("SLUS_000.0{n}"), 32, n as u8)).unwrap();
        std::fs::write(dir.join(format!("Playlist Game (Disc {n}).cue")), format!("FILE \"{bin}\" BINARY
  TRACK 01 MODE2/2352
    INDEX 01 00:00:00
")).unwrap();
    }
    std::fs::write(dir.join("Playlist Game.m3u"), "Playlist Game (Disc 1).cue
Playlist Game (Disc 2).cue
").unwrap();
    let view = api::directories::create(app, &api::directories::DirectorySpec { path: dir.display().to_string(), app_platform_id: Some(romexplorer::common::models::Platform::PlayStation.igdb_id()), ..Default::default() }).await.unwrap();
    let req = api::jobs::FileScanRequest { directory_id: view.directory.id, mode: FileScanMode::Full, platform: api::jobs::PlatformSelect::All, hash_match: None, file_id: None };
    let summary = api::scan::run_file_scan(app, &req, &romexplorer::database::jobs::Progress::new(&app.pool, 0)).await.unwrap();
    assert_eq!(summary.playlists_grouped, 1, "{summary:?}");
    let rows: Vec<(u64, Option<u16>, Option<u64>)> = sqlx::query_as(
        "SELECT lgr.game_id, lr.disc_number, lg.playlist_file_id FROM local_rom lr JOIN local_game_roms lgr ON lgr.rom_id = lr.id JOIN local_game lg ON lg.id = lgr.game_id WHERE lr.main_path LIKE '%Playlist Game (Disc %' AND lr.deleted = FALSE ORDER BY lr.disc_number",
    )
    .fetch_all(&app.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].0, rows[1].0, "both discs in one game");
    assert_eq!((rows[0].1, rows[1].1), (Some(1), Some(2)), "disc numbers from the playlist order");
    assert!(rows[0].2.is_some(), "the game points at its playlist");
    // Launcher: an emulator for the platform is offered for the game (its .m3u); a missing
    // emulator path is refused cleanly (nothing is started).
    use romexplorer::database::emulators::{Emulator, EmulatorPlatform};
    let ps1 = romexplorer::common::models::Platform::PlayStation.igdb_id();
    let saved = api::launch::save(app, &Emulator { id: 0, name: "Test Emu".into(), preset: None, kind: "standalone".into(), path: dir.join("no-such-emu.exe").display().to_string(), args: "{rom}".into(), cores_dir: None, working_dir: None, supports_archives: false, supports_playlists: true, enabled: true, platforms: vec![EmulatorPlatform { app_platform_id: ps1, core: None, is_default: true }], artwork: None }).await.unwrap();
    let target = api::launch::Target { rom_id: None, game_id: Some(rows[0].0), emulator_id: None };
    let options = api::launch::options(app, &target).await.unwrap();
    assert!(options.target.file.ends_with(".m3u"), "a game opens its playlist: {}", options.target.file);
    assert_eq!(options.emulators.first().map(|e| e.id), Some(saved.id));
    assert!(api::launch::launch(app, &target).await.is_err(), "missing emulator binary is an error, not a crash");
    assert!(api::launch::save(app, &Emulator { args: "--no-rom".into(), ..saved.clone() }).await.is_err(), "args must contain {{rom}}");
    api::launch::delete(app, saved.id).await.unwrap();

    // Copying the game -- picked by its title, or by its release -- takes the playlist along,
    // beside the discs; converted to CHD, its lines name the CHDs.
    use romexplorer_types::copy::{CopyFilesRequest, CopyMode, CopySelection, FolderScheme, OutputFormat, OutputSettings};
    let src = romexplorer::database::import::ensure_source(&app.pool, "redump").await.unwrap();
    let title = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES ('Playlist Game', 'playlist game', SHA1('playlist game'))").execute(&app.pool).await.unwrap().last_insert_id();
    let release = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (?, 'Playlist Game (USA)', SHA1('Playlist Game (USA)'), 'playlist game', SHA1('playlist game'), 'US', ?)")
        .bind(ps1)
        .bind(src)
        .execute(&app.pool)
        .await
        .unwrap()
        .last_insert_id();
    sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(title).bind(release).execute(&app.pool).await.unwrap();
    sqlx::query("INSERT INTO local_release_games (release_id, game_id) VALUES (?, ?)").bind(release).bind(rows[0].0).execute(&app.pool).await.unwrap();
    let copy_to = |name: &str, selection: CopySelection, output: Option<OutputSettings>| {
        let dest = base.join(name);
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).unwrap();
        let request = CopyFilesRequest {
            selection,
            mode: CopyMode::ToExternal,
            destination: dest.display().to_string(),
            folder_scheme: Some(FolderScheme::Es),
            output,
            source_preference: Default::default(),
            on_conflict: Default::default(),
            titles_all_releases: false,
            conflict_choices: Vec::new(),
            upload_source_id: None,
            export_artwork: Vec::new(),
            verify: Some(false),
        };
        async move {
            let summary = api::copy::run::run(app, &request, &romexplorer::database::jobs::Progress::none(), None).await.unwrap();
            assert!(summary.errors.is_empty(), "{:?}", summary.errors);
            dest.join("psx")
        }
    };
    let by_title = copy_to("m3u-by-title", CopySelection { title_ids: vec![title], ..Default::default() }, None).await;
    let playlist = std::fs::read_to_string(by_title.join("Playlist Game.m3u")).expect("the playlist is copied with a title");
    assert_eq!(playlist.lines().collect::<Vec<_>>(), ["Playlist Game (Disc 1).cue", "Playlist Game (Disc 2).cue"], "{playlist:?}");
    for n in 1..=2 {
        assert!(by_title.join(format!("Playlist Game (Disc {n}).cue")).is_file() && by_title.join(format!("Playlist Game (Disc {n}).bin")).is_file(), "disc {n} beside it");
    }
    let by_release = copy_to("m3u-by-release", CopySelection { release_ids: vec![release], ..Default::default() }, None).await;
    assert!(by_release.join("Playlist Game.m3u").is_file(), "the playlist is copied with a release");
    let as_chd = copy_to("m3u-as-chd", CopySelection { release_ids: vec![release], ..Default::default() }, Some(OutputSettings { per_platform: vec![(ps1, OutputFormat::Chd)], ..Default::default() })).await;
    let rewritten = std::fs::read_to_string(as_chd.join("Playlist Game.m3u")).expect("the playlist is copied with converted discs");
    assert_eq!(rewritten.lines().collect::<Vec<_>>(), ["Playlist Game (Disc 1).chd", "Playlist Game (Disc 2).chd"], "its lines name the converted discs: {rewritten:?}");
    assert!(as_chd.join("Playlist Game (Disc 1).chd").is_file());
    sqlx::query("DELETE FROM local_release_games WHERE release_id = ?").bind(release).execute(&app.pool).await.unwrap();

    // Later checks count the configured directories.
    api::directories::delete(app, view.directory.id, false).await.unwrap();
}

/// A scan records a platform only when the file's own signature confirms it. The folder's
/// platform -- configured on a subfolder, scanned from the folder above -- is the hint that
/// gets checked first; junk under the right name stays Unknown (a DAT hash match identifies
/// it later), and a disc in the wrong platform's folder is what its own signature says. A zip keeps its folder's platform:
/// its members are hashed and matched on their own.
async fn scan_platforms(app: &App, base: &PathBuf) {
    use std::io::Write;
    use romexplorer::common::models::Platform;
    let top = base.join("collection").join("scanplatforms");
    let (ps1, snes, gc) = (top.join("ps1"), top.join("snes"), top.join("gc"));
    for d in [&ps1, &snes, &gc] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(ps1.join("Real Disc (USA).bin"), fixtures::ps1_disc("SLUS_000.03", 40, 3)).unwrap();
    std::fs::write(ps1.join("Real Disc (USA).cue"), fixtures::ps1_cue("Real Disc (USA).bin")).unwrap();
    std::fs::write(ps1.join("Junk Disc (USA).bin"), vec![5u8; 2352 * 40]).unwrap();
    std::fs::write(ps1.join("Junk Disc (USA).cue"), fixtures::ps1_cue("Junk Disc (USA).bin")).unwrap();
    {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(ps1.join("Zipped Disc (USA).zip")).unwrap());
        zip.start_file("Zipped Disc (USA).bin", zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();
        zip.write_all(&[6u8; 4096]).unwrap();
        zip.finish().unwrap();
    }
    std::fs::write(snes.join("Real Cart (USA).sfc"), fixtures::snes_rom("REAL CART", 0x8000, 1)).unwrap();
    std::fs::write(snes.join("Junk Cart (USA).sfc"), vec![2u8; 0x8000]).unwrap();
    std::fs::write(gc.join("Misplaced (USA).bin"), fixtures::ps1_disc("SLUS_000.04", 40, 4)).unwrap();
    std::fs::write(gc.join("Misplaced (USA).cue"), fixtures::ps1_cue("Misplaced (USA).bin")).unwrap();

    let mut created = Vec::new();
    let view = api::directories::create(app, &api::directories::DirectorySpec { path: top.display().to_string(), ..Default::default() }).await.unwrap();
    created.push(view.directory.id);
    for (d, p) in [(&ps1, Platform::PlayStation), (&snes, Platform::Snes), (&gc, Platform::GameCube)] {
        created.push(api::directories::create(app, &api::directories::DirectorySpec { path: d.display().to_string(), app_platform_id: Some(p.igdb_id()), ..Default::default() }).await.unwrap().directory.id);
    }
    // One scan, of the folder above: the subfolders' platforms come along as hints.
    let req = api::jobs::FileScanRequest { directory_id: view.directory.id, mode: FileScanMode::Full, platform: api::jobs::PlatformSelect::All, hash_match: None, file_id: None };
    let summary = api::scan::run_file_scan(app, &req, &romexplorer::database::jobs::Progress::new(&app.pool, 0)).await.unwrap();
    assert!(summary.errors.is_empty(), "{:?}", summary.errors);
    let platform_of = |path: PathBuf| async move {
        sqlx::query_scalar::<_, u16>("SELECT app_platform_id FROM local_rom WHERE main_path = ? AND deleted = FALSE").bind(path.display().to_string()).fetch_one(&app.pool).await.unwrap()
    };
    assert_eq!(platform_of(ps1.join("Real Disc (USA).cue")).await, Platform::PlayStation.igdb_id(), "SYSTEM.CNF confirms the hint");
    assert_eq!(platform_of(snes.join("Real Cart (USA).sfc")).await, Platform::Snes.igdb_id(), "a valid header confirms it, kept as the folder's SNES");
    assert_eq!(platform_of(ps1.join("Junk Disc (USA).cue")).await, 0, "the right folder and name are not a signature");
    assert_eq!(platform_of(snes.join("Junk Cart (USA).sfc")).await, 0);
    // The wrong folder's hint is a negative; the disc's own signature then says what it is.
    assert_eq!(platform_of(gc.join("Misplaced (USA).cue")).await, Platform::PlayStation.igdb_id(), "a misplaced disc is what its signature says, not what its folder says");
    assert_eq!(platform_of(ps1.join("Zipped Disc (USA).zip")).await, Platform::PlayStation.igdb_id(), "an archive keeps its folder's platform");
    assert_eq!(summary.platform_unconfirmed, 2, "{summary:?}");
    for id in created.into_iter().rev() {
        api::directories::delete(app, id, false).await.unwrap();
    }
}

/// A file whose bytes changed under the same path, found by a full scan -- while still live,
/// and after it went missing and came back: the same file row, pointed at the new content,
/// its integrity stamp cleared, its rom on the new bytes' platform, and its game's release
/// link dropped because no file backs it any more. An unchanged file keeps everything.
/// A full scan started again (the server restarted under it) reads only what its earlier
/// run had not: files already recorded since the job was queued are left alone, a file
/// changed in between is read again, and nothing the earlier run recorded is taken for
/// missing.
async fn scan_goes_on_from_where_it_was(app: &App, base: &PathBuf) {
    use romexplorer::common::models::Platform;
    use romexplorer::database::jobs::Progress;
    let dir = base.join("collection").join("scanresume");
    std::fs::create_dir_all(&dir).unwrap();
    for (name, seed) in [("Resume A (USA).sfc", 21u8), ("Resume B (USA).sfc", 22), ("Resume C (USA).sfc", 23)] {
        std::fs::write(dir.join(name), fixtures::snes_rom(&name[..8].to_uppercase(), 0x8000, seed)).unwrap();
    }
    let directory_id = api::directories::create(app, &api::directories::DirectorySpec { path: dir.display().to_string(), app_platform_id: Some(Platform::Snes.igdb_id()), ..Default::default() }).await.unwrap().directory.id;
    let req = api::jobs::FileScanRequest { directory_id, mode: FileScanMode::Full, platform: api::jobs::PlatformSelect::All, hash_match: None, file_id: None };
    // When the job was queued: a second before now, as the database counts it.
    let queued: chrono::NaiveDateTime = sqlx::query_scalar("SELECT CAST(NOW() - INTERVAL 1 SECOND AS DATETIME)").fetch_one(&app.pool).await.unwrap();
    let first = api::scan::run_file_scan_since(app, &req, Some(queued), &Progress::none()).await.unwrap();
    assert_eq!((first.files_hashed, first.skipped_existing), (3, 0), "the first run reads everything: {first:?}");
    // One file changes and one arrives before the scan is started again.
    std::fs::write(dir.join("Resume B (USA).sfc"), fixtures::snes_rom("RESUME B2", 0x10000, 24)).unwrap();
    std::fs::write(dir.join("Resume D (USA).sfc"), fixtures::snes_rom("RESUME D", 0x8000, 25)).unwrap();
    let again = api::scan::run_file_scan_since(app, &req, Some(queued), &Progress::none()).await.unwrap();
    assert_eq!((again.files_hashed, again.skipped_existing, again.marked_missing), (2, 2, 0), "started again it reads the changed file and the new one only: {again:?}");
    // The same request as a job of its own -- no earlier run -- reads all four.
    let fresh = api::scan::run_file_scan(app, &req, &romexplorer::database::jobs::Progress::none()).await.unwrap();
    assert_eq!((fresh.files_hashed, fresh.skipped_existing), (4, 0), "{fresh:?}");
    api::directories::delete(app, directory_id, false).await.unwrap();
}

async fn scan_replaced_files(app: &App, base: &PathBuf) {
    use sqlx::Row as _;
    use romexplorer::common::models::Platform;
    use romexplorer::database::jobs::Progress;
    let dir = base.join("collection").join("scanreplaced");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("Swap (USA).sfc");
    let original = fixtures::snes_rom("SWAP ORIGINAL", 0x8000, 7);
    std::fs::write(&file, &original).unwrap();
    let directory_id = api::directories::create(app, &api::directories::DirectorySpec { path: dir.display().to_string(), app_platform_id: Some(Platform::Snes.igdb_id()), ..Default::default() }).await.unwrap().directory.id;
    let full = || async {
        let req = api::jobs::FileScanRequest { directory_id, mode: FileScanMode::Full, platform: api::jobs::PlatformSelect::All, hash_match: None, file_id: None };
        let s = api::scan::run_file_scan(app, &req, &romexplorer::database::jobs::Progress::none()).await.unwrap();
        assert!(s.errors.is_empty(), "{:?}", s.errors);
        s
    };
    #[derive(Debug, PartialEq)]
    struct Row {
        file: u64,
        content: u64,
        deleted: bool,
        checked: bool,
        rom: u64,
        platform: u16,
        game: u64,
    }
    let row = || async {
        let r = sqlx::query(
            "SELECT f.id, f.content_id, f.deleted, f.last_integrity_check IS NOT NULL AS checked, lr.id AS rom, lr.app_platform_id, lgr.game_id \
             FROM local_files f JOIN local_rom_files lrf ON lrf.file_id = f.id JOIN local_rom lr ON lr.id = lrf.rom_id JOIN local_game_roms lgr ON lgr.rom_id = lr.id WHERE f.path = ?",
        )
        .bind(file.display().to_string())
        .fetch_one(&app.pool)
        .await
        .unwrap();
        Row { file: r.get("id"), content: r.get("content_id"), deleted: r.get("deleted"), checked: r.get::<i64, _>("checked") == 1, rom: r.get("rom"), platform: r.get("app_platform_id"), game: r.get("game_id") }
    };
    let linked = |game: u64, release: u64| async move { sqlx::query("SELECT 1 FROM local_release_games WHERE game_id = ? AND release_id = ?").bind(game).bind(release).fetch_optional(&app.pool).await.unwrap().is_some() };

    full().await;
    let first = row().await;
    assert_eq!(first.platform, Platform::Snes.igdb_id());
    // A DAT release for those bytes, the game linked to it, and an integrity check done.
    let release = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash) VALUES (?, 'Swap (USA)', SHA1('Swap (USA)'), 'swap', SHA1('swap'))").bind(Platform::Snes.igdb_id()).execute(&app.pool).await.unwrap().last_insert_id();
    sqlx::query("INSERT INTO release_contents (release_id, content_id, file_name) VALUES (?, ?, 'Swap (USA).sfc')").bind(release).bind(first.content).execute(&app.pool).await.unwrap();
    sqlx::query("INSERT INTO local_release_games (release_id, game_id) VALUES (?, ?)").bind(release).bind(first.game).execute(&app.pool).await.unwrap();
    sqlx::query("UPDATE local_files SET last_integrity_check = NOW() WHERE id = ?").bind(first.file).execute(&app.pool).await.unwrap();

    // Unchanged: nothing moves.
    let same = full().await;
    assert_eq!((same.contents_replaced, same.release_links_dropped), (0, 0));
    assert_eq!(row().await, Row { checked: true, ..first }, "an unchanged file keeps its rows, link and stamp");
    assert!(linked(first.game, release).await);

    // Other bytes under the same name, no signature in them.
    std::fs::write(&file, vec![9u8; 0x8000]).unwrap();
    let swapped = full().await;
    assert_eq!((swapped.contents_replaced, swapped.release_links_dropped), (1, 1), "{swapped:?}");
    let now = row().await;
    assert_eq!((now.file, now.rom, now.game), (first.file, first.rom, first.game), "same file, rom and game rows");
    assert_ne!(now.content, first.content, "the file points at the new bytes");
    assert!(!now.checked, "the integrity stamp was the old bytes'");
    assert_eq!(now.platform, 0, "the old bytes' platform is not the new ones'");
    assert!(!linked(first.game, release).await, "nothing backs the old release any more");

    // Gone, then back with the original bytes: the soft-deleted row is revived and repointed.
    std::fs::remove_file(&file).unwrap();
    full().await;
    assert!(row().await.deleted);
    std::fs::write(&file, &original).unwrap();
    let back = full().await;
    assert_eq!(back.contents_replaced, 1);
    let revived = row().await;
    assert_eq!((revived.file, revived.content, revived.deleted, revived.platform), (first.file, first.content, false, Platform::Snes.igdb_id()));
    api::directories::delete(app, directory_id, false).await.unwrap();
}

/// A scan reads each candidate once through `Single`: a zip's own bytes are its identity and
/// its member hangs off it; a cue set's tracks hang off the sheet; a cue naming a track that
/// is not there is still recorded, alone, with the missing track reported -- as a warning when
/// none of its tracks are there (they were kept elsewhere, as a CHD), as an error when only
/// some are; and scanning again leaves exactly the same member links behind.
async fn scan_containers(app: &App, base: &PathBuf) {
    use std::io::Write;
    let dir = base.join("collection").join("scancontainers");
    std::fs::create_dir_all(&dir).unwrap();
    let rom: Vec<u8> = (0..5000u32).map(|i| (i * 7 % 256) as u8).collect();
    let zip_path = dir.join("Zipped (USA).zip");
    {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        zip.start_file("Zipped (USA).bin", zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();
        zip.write_all(&rom).unwrap();
        zip.finish().unwrap();
    }
    std::fs::write(dir.join("Disc (USA).bin"), vec![9u8; 2352 * 4]).unwrap();
    std::fs::write(dir.join("Disc (USA).cue"), "FILE \"Disc (USA).bin\" BINARY\r\n  TRACK 01 MODE2/2352\r\n    INDEX 01 00:00:00\r\n").unwrap();
    std::fs::write(dir.join("Broken (USA).cue"), "FILE \"Broken (USA).bin\" BINARY\r\n  TRACK 01 MODE2/2352\r\n    INDEX 01 00:00:00\r\n").unwrap();
    std::fs::write(dir.join("Half (USA) (Track 1).bin"), vec![7u8; 2352 * 4]).unwrap();
    std::fs::write(
        dir.join("Half (USA).cue"),
        "FILE \"Half (USA) (Track 1).bin\" BINARY\r\n  TRACK 01 MODE2/2352\r\n    INDEX 01 00:00:00\r\nFILE \"Half (USA) (Track 2).bin\" BINARY\r\n  TRACK 02 AUDIO\r\n    INDEX 01 00:00:00\r\n",
    )
    .unwrap();
    let view = api::directories::create(app, &api::directories::DirectorySpec { path: dir.display().to_string(), app_platform_id: Some(romexplorer::common::models::Platform::PlayStation.igdb_id()), ..Default::default() }).await.unwrap();
    let req = api::jobs::FileScanRequest { directory_id: view.directory.id, mode: FileScanMode::Full, platform: api::jobs::PlatformSelect::All, hash_match: None, file_id: None };
    let summary = api::scan::run_file_scan(app, &req, &romexplorer::database::jobs::Progress::new(&app.pool, 0)).await.unwrap();
    assert!(summary.errors.iter().any(|e| e.contains("Half (USA).cue") && e.contains("missing track")), "a sheet missing some of its tracks is an error: {:?}", summary.errors);
    assert_eq!(summary.errors.len(), 1, "{:?}", summary.errors);
    assert!(summary.warnings.iter().any(|w| w.contains("Broken (USA).cue")), "a sheet with none of its tracks is a warning: {:?}", summary.warnings);
    assert_eq!(summary.warnings.len(), 1, "{:?}", summary.warnings);

    let sha1 = |bytes: &[u8]| romexplorer::rom::hash::hash_bytes(romexplorer::rom::hash::HashFlags::SHA1, bytes).sha1.unwrap();
    let content_of = |name: &str| {
        let path = dir.join(name).display().to_string();
        async move { sqlx::query_scalar::<_, u64>("SELECT content_id FROM local_files WHERE path = ? AND deleted = FALSE").bind(path).fetch_one(&app.pool).await.unwrap() }
    };
    let sha1_of = |content: u64| async move { sqlx::query_scalar::<_, String>("SELECT value_hex FROM hashes WHERE algorithm = 'sha1' AND content_id = ?").bind(content).fetch_one(&app.pool).await.unwrap() };
    let members_of = |content: u64| async move {
        sqlx::query_as::<_, (u64, Option<String>)>("SELECT child_content_id, member_path FROM content_contents WHERE parent_content_id = ? ORDER BY child_content_id").bind(content).fetch_all(&app.pool).await.unwrap()
    };

    // The zip: its own bytes, not its member's, and the member linked under it.
    let zip_content = content_of("Zipped (USA).zip").await;
    assert_eq!(sha1_of(zip_content).await, sha1(&std::fs::read(&zip_path).unwrap()), "the archive is identified by its own bytes");
    let zip_members = members_of(zip_content).await;
    assert_eq!(zip_members.len(), 1, "{zip_members:?}");
    assert_eq!(zip_members[0].1.as_deref(), Some("Zipped (USA).bin"));
    assert_eq!(sha1_of(zip_members[0].0).await, sha1(&rom), "the member is the rom inside");

    // The cue set: each file its own row, the track linked under the sheet.
    let cue_content = content_of("Disc (USA).cue").await;
    let bin_content = content_of("Disc (USA).bin").await;
    assert_eq!(sha1_of(cue_content).await, sha1(&std::fs::read(dir.join("Disc (USA).cue")).unwrap()), "a loose sheet is its bytes as they are");
    assert_eq!(members_of(cue_content).await.iter().map(|m| m.0).collect::<Vec<_>>(), vec![bin_content]);

    // The broken cue: recorded on its own.
    let broken = content_of("Broken (USA).cue").await;
    assert!(members_of(broken).await.is_empty());

    // Again: identical links, nothing doubled.
    let before = (members_of(zip_content).await, members_of(cue_content).await);
    api::scan::run_file_scan(app, &req, &romexplorer::database::jobs::Progress::new(&app.pool, 0)).await.unwrap();
    assert_eq!((members_of(zip_content).await, members_of(cue_content).await), before, "a rescan leaves the same member links");
    api::directories::delete(app, view.directory.id, false).await.unwrap();
}

/// A copy that re-containers: a cartridge into a zip and a cue set into a CHD, verified. The
/// conversion is one `Single` that also reads its output back -- the output's own bytes,
/// which become its identity, and its members decoded at the same time, which have to match
/// what the catalogue says went in.
async fn copy_converts(app: &App, base: &PathBuf) {
    use romexplorer::database::jobs::Progress;
    use romexplorer_types::copy::{CopyFilesRequest, CopyMode, CopySelection, FolderScheme, OutputFormat, OutputSettings, OwnedRef};
    if !romexplorer::rom::native_lib::detect_capabilities(romexplorer::rom::native_lib::LibVariant::Streaming).all_streaming() {
        eprintln!("streaming builds not present -- skipping copy_converts");
        return;
    }
    let dir = base.join("collection").join("convertsrc");
    std::fs::create_dir_all(&dir).unwrap();
    let cart = fixtures::snes_rom("CONV GAME", 0x10000, 5);
    std::fs::write(dir.join("Conv (USA).sfc"), &cart).unwrap();
    let ps1 = base.join("collection").join("convertps1");
    std::fs::create_dir_all(&ps1).unwrap();
    std::fs::write(ps1.join("Disc Conv (USA).bin"), fixtures::ps1_disc("SLUS_000.02", 300, 7)).unwrap();
    std::fs::write(ps1.join("Disc Conv (USA).cue"), "FILE \"Disc Conv (USA).bin\" BINARY\r\n  TRACK 01 MODE2/2352\r\n    INDEX 01 00:00:00\r\n").unwrap();
    let snes = romexplorer::common::models::Platform::Snes.igdb_id();
    let psx = romexplorer::common::models::Platform::PlayStation.igdb_id();
    let mut file_ids = Vec::new();
    let mut rom_ids = Vec::new();
    let mut created = Vec::new();
    for (path, platform) in [(&dir, snes), (&ps1, psx)] {
        let view = api::directories::create(app, &api::directories::DirectorySpec { path: path.display().to_string(), app_platform_id: Some(platform), ..Default::default() }).await.unwrap();
        created.push(view.directory.id);
        let req = api::jobs::FileScanRequest { directory_id: view.directory.id, mode: FileScanMode::Full, platform: api::jobs::PlatformSelect::All, hash_match: None, file_id: None };
        let summary = api::scan::run_file_scan(app, &req, &Progress::new(&app.pool, 0)).await.unwrap();
        assert!(summary.errors.is_empty(), "{:?}", summary.errors);
        if path == &dir {
            let ids: Vec<u64> = sqlx::query_scalar("SELECT id FROM local_files WHERE directory_id = ? AND deleted = FALSE ORDER BY id").bind(view.directory.id).fetch_all(&app.pool).await.unwrap();
            file_ids.extend(ids);
        } else {
            // A cue set is one rom: selected as such, its track folds into the one CHD.
            rom_ids.push(sqlx::query_scalar::<_, u64>("SELECT id FROM local_rom WHERE main_path = ? AND deleted = FALSE").bind(ps1.join("Disc Conv (USA).cue").display().to_string()).fetch_one(&app.pool).await.unwrap());
        }
    }
    let dest = base.join("convertdest");
    std::fs::create_dir_all(&dest).unwrap();
    let request = CopyFilesRequest {
        selection: CopySelection { file_ids: file_ids.iter().map(|id| OwnedRef::local(*id)).collect(), rom_ids: rom_ids.iter().map(|id| OwnedRef::local(*id)).collect(), ..Default::default() },
        mode: CopyMode::ToExternal,
        destination: dest.display().to_string(),
        folder_scheme: Some(FolderScheme::Es),
        output: Some(OutputSettings { per_platform: vec![(snes, OutputFormat::Zip), (psx, OutputFormat::Chd)], ..Default::default() }),
        source_preference: Default::default(),
        on_conflict: Default::default(),
        titles_all_releases: false,
        conflict_choices: Vec::new(),
        upload_source_id: None,
        export_artwork: Vec::new(),
        verify: Some(true),
    };
    let summary = api::copy::run::run(app, &request, &Progress::none(), None).await.unwrap();
    assert!(summary.errors.is_empty(), "{:?}", summary.errors);
    assert!(summary.verify_failed.is_empty(), "{:?}", summary.verify_failed);
    assert_eq!(summary.verified, 2, "the zip and the CHD both checked out: {summary:?}");

    let sha1 = |bytes: &[u8]| romexplorer::rom::hash::hash_bytes(romexplorer::rom::hash::HashFlags::SHA1, bytes).sha1.unwrap();
    for out in [dest.join("snes").join("Conv (USA).zip"), dest.join("psx").join("Disc Conv (USA).chd")] {
        assert!(out.is_file(), "{} written; dest has {:?}", out.display(), walk(&dest));
        // Recorded under its own bytes, linked to what went in.
        let own: Option<u64> = sqlx::query_scalar("SELECT content_id FROM hashes WHERE algorithm = 'sha1' AND value_hex = ?").bind(sha1(&std::fs::read(&out).unwrap())).fetch_optional(&app.pool).await.unwrap();
        let own = own.unwrap_or_else(|| panic!("{} has a content row of its own", out.display()));
        let members: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM content_contents WHERE parent_content_id = ?").bind(own).fetch_one(&app.pool).await.unwrap();
        assert!(members >= 1, "{} is linked to its members", out.display());
    }
    // Later checks count the configured directories.
    for id in created {
        api::directories::delete(app, id, false).await.unwrap();
    }
}

fn walk(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if e.path().is_dir() {
            out.extend(walk(&e.path()));
        } else {
            out.push(e.path().display().to_string());
        }
    }
    out
}

/// Config > Copy: the folder schemes, a per-platform override, the combine groups, and the
/// validation that keeps an unusable name or an impossible format out of the table.
async fn naming(app: &App) {
    use romexplorer::common::models::Platform;
    use romexplorer_types::copy::{FolderScheme, NamingUpdate, OutputFormat};

    let snes = Platform::Snes.igdb_id();
    let view = api::naming::view(app).await.unwrap();
    assert_eq!(view.scheme, FolderScheme::Es, "the default scheme");
    assert_eq!(view.platforms.len(), romexplorer::database::connection::all_platforms().len());
    let row = |v: &romexplorer_types::copy::NamingView, id: u16| v.platforms.iter().find(|p| p.app_platform_id == id).cloned().unwrap();
    let s = row(&view, snes);
    assert_eq!((s.dat.as_str(), s.display.as_str(), s.es.as_str()), ("Nintendo - Super Nintendo Entertainment System", "Super Nintendo Entertainment System", "snes"));
    assert_eq!(s.effective, "snes");
    assert!(s.formats.contains(&OutputFormat::Zip) && !s.formats.contains(&OutputFormat::Chd), "a cartridge has no CHD");

    let view = api::naming::update(app, &NamingUpdate { scheme: Some(FolderScheme::Dat), ..Default::default() }).await.unwrap();
    assert_eq!(row(&view, snes).effective, "Nintendo - Super Nintendo Entertainment System");

    let view = api::naming::update(app, &NamingUpdate { folders: vec![(snes, "Super Famicom".into())], ..Default::default() }).await.unwrap();
    assert_eq!(row(&view, snes).effective, "Super Famicom", "an override beats the scheme");

    // A name no filesystem would take is refused rather than silently rewritten.
    assert_eq!(api::naming::update(app, &NamingUpdate { folders: vec![(snes, "SNES/SFC".into())], ..Default::default() }).await.err().map(|e| e.kind), Some(ErrorKind::Invalid));
    assert_eq!(api::naming::update(app, &NamingUpdate { formats: vec![(snes, OutputFormat::Chd)], ..Default::default() }).await.err().map(|e| e.kind), Some(ErrorKind::Invalid), "a cartridge can't be a CHD");
    assert_eq!(api::naming::update(app, &NamingUpdate { combine: vec![("nope".into(), true)], ..Default::default() }).await.err().map(|e| e.kind), Some(ErrorKind::Invalid));

    // Combining sends the Super Famicom's games into the SNES folder, override included.
    let sfc = Platform::SuperFamicom.igdb_id();
    let view = api::naming::update(app, &NamingUpdate { combine: vec![("super_famicom".into(), true)], ..Default::default() }).await.unwrap();
    assert_eq!(row(&view, sfc).effective, "Super Famicom", "combined into the SNES row's own custom name");
    let view = api::naming::update(app, &NamingUpdate { combine: vec![("super_famicom".into(), false)], ..Default::default() }).await.unwrap();
    assert_eq!(row(&view, sfc).effective, "Nintendo - Super Famicom", "uncombined, it takes its own pack name");

    api::naming::reset_folders(app).await.unwrap();
    let view = api::naming::update(app, &NamingUpdate { scheme: Some(FolderScheme::Es), ..Default::default() }).await.unwrap();
    assert_eq!(row(&view, snes).effective, "snes", "reset drops the override");
    assert!(row(&view, snes).custom.is_none());
}

/// A mount, its probe, and the path arithmetic that turns another machine's absolute path
/// into one on this one.
async fn mounts(app: &App, base: &PathBuf) {
    use romexplorer_types::copy::MountSpec;

    let local = base.join("mnt");
    std::fs::create_dir_all(local.join("snes")).unwrap();
    std::fs::write(local.join("snes").join("game.sfc"), vec![7u8; 1024]).unwrap();

    let source_id = api::sources::create(
        app,
        &api::sources::SourceSpec { name: "test server".into(), base_url: "http://127.0.0.1:59999".into(), token: Some("t".into()), role: "peer".into(), enabled: true, replace_catalog: false, media: Default::default(), username: None, password: None },
    )
    .await
    .unwrap()
    .id;

    assert!(api::copy::mount::create(app, source_id, &MountSpec { remote_prefix: "/srv/roms".into(), local_path: "relative".into(), remote_sep: None, remote_directory_id: None, enabled: true }).await.is_err(), "the local path must be absolute");

    let mount = api::copy::mount::create(app, source_id, &MountSpec { remote_prefix: "/srv/roms/".into(), local_path: local.display().to_string(), remote_sep: None, remote_directory_id: None, enabled: true }).await.unwrap();
    assert_eq!(mount.remote_prefix, "/srv/roms", "the trailing separator is dropped");
    assert_eq!(mount.remote_sep, "/");
    assert_eq!(api::copy::mount::list(app, source_id).await.unwrap().len(), 1);

    let resolved = api::copy::mount::load(app, Some(source_id)).await.unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(api::copy::mount::rewrite(&resolved, "/srv/roms/snes/game.sfc"), Some(local.join("snes").join("game.sfc")));
    assert_eq!(api::copy::mount::rewrite(&resolved, "/elsewhere/game.sfc"), None);

    // Nothing is mirrored from this source, so there is nothing to probe -- and that is a
    // pass, not a failure.
    let check = api::copy::mount::verify(app, &resolved[0]).await.unwrap();
    assert!(check.ok, "{check:?}");

    // A mount pointing at a directory that isn't there is the failure worth catching.
    let gone = api::copy::mount::Resolved { local_path: base.join("not-mounted").display().to_string(), ..resolved[0].clone() };
    assert!(!api::copy::mount::verify(app, &gone).await.unwrap().ok);

    api::copy::mount::remove(app, mount.id).await.unwrap();
    assert!(api::copy::mount::list(app, source_id).await.unwrap().is_empty(), "soft-deleted mounts are gone from the list");
    api::sources::delete(app, source_id).await.unwrap();
}

/// The copy itself, end to end on local files: plan, dedup, layout, the byte copy, the
/// no-op re-run, and the validation that refuses a destination it cannot use.
async fn copy_plan_and_run(app: &App, base: &PathBuf) {
    use romexplorer::database::jobs::Progress;
    use romexplorer_types::copy::{CopyFilesRequest, CopyMode, CopySelection, FolderScheme, OwnedRef};

    let dir = base.join("collection").join("copysrc");
    std::fs::create_dir_all(&dir).unwrap();
    // Valid SNES headers: a scan records a platform only when the file's signature confirms it.
    std::fs::write(dir.join("Copy Me (USA).sfc"), fixtures::snes_rom("COPY ME", 0x8000, 3)).unwrap();
    std::fs::write(dir.join("Copy Me Too (USA).sfc"), fixtures::snes_rom("COPY ME TOO", 0x8000, 4)).unwrap();
    let view = api::directories::create(app, &api::directories::DirectorySpec { path: dir.display().to_string(), app_platform_id: Some(romexplorer::common::models::Platform::Snes.igdb_id()), ..Default::default() }).await.unwrap();
    let req = api::jobs::FileScanRequest { directory_id: view.directory.id, mode: FileScanMode::Full, platform: api::jobs::PlatformSelect::All, hash_match: None, file_id: None };
    api::scan::run_file_scan(app, &req, &Progress::new(&app.pool, 0)).await.unwrap();

    let file_ids: Vec<u64> = sqlx::query_scalar("SELECT id FROM local_files WHERE directory_id = ? AND deleted = FALSE ORDER BY id").bind(view.directory.id).fetch_all(&app.pool).await.unwrap();
    assert_eq!(file_ids.len(), 2, "the scan found both files");

    let dest = base.join("copydest");
    std::fs::create_dir_all(&dest).unwrap();
    let selection = CopySelection { file_ids: file_ids.iter().map(|id| OwnedRef::local(*id)).collect(), ..Default::default() };
    let request = CopyFilesRequest {
        selection: selection.clone(),
        // to_external: nothing is skipped for being held here, which it all is.
        mode: CopyMode::ToExternal,
        destination: dest.display().to_string(),
        folder_scheme: Some(FolderScheme::Es),
        output: None,
        source_preference: Default::default(),
        on_conflict: Default::default(),
        titles_all_releases: false,
        conflict_choices: Vec::new(),
        upload_source_id: None,
        export_artwork: Vec::new(),
        verify: Some(false),
    };

    let plan = api::copy::plan::plan(app, &request).await.unwrap().view;
    assert_eq!((plan.files, plan.files_to_copy, plan.bytes_to_copy), (2, 2, 2 * 0x8000), "{plan:?}");
    assert!(plan.sample.iter().all(|f| f.destination.contains("snes")), "the ES scheme puts them under snes/: {:?}", plan.sample);

    let summary = api::copy::run::run(app, &request, &Progress::none(), None).await.unwrap();
    assert_eq!((summary.files_copied, summary.bytes_copied), (2, 2 * 0x8000), "{summary:?}");
    assert!(summary.errors.is_empty(), "{:?}", summary.errors);
    assert_eq!(std::fs::read(dest.join("snes").join("Copy Me (USA).sfc")).unwrap().len(), 0x8000);
    assert!(std::fs::read_dir(dest.join("snes")).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().ends_with(".recopy-part")), "no part files survive");

    // Re-running the identical job is the resume path: everything matches by hash already.
    let again = api::copy::run::run(app, &request, &Progress::none(), None).await.unwrap();
    assert_eq!((again.files_copied, again.files_skipped_same), (0, 2), "{again:?}");

    // Naming the same file twice copies it once.
    let doubled = CopyFilesRequest { selection: CopySelection { file_ids: vec![OwnedRef::local(file_ids[0]), OwnedRef::local(file_ids[0])], ..Default::default() }, ..request.clone() };
    let plan = api::copy::plan::plan(app, &doubled).await.unwrap().view;
    assert_eq!(plan.files_to_copy + plan.already_at_destination, 1, "a repeated selection is one file: {plan:?}");

    // to_collection would skip every one of these, because this machine holds them all.
    let into_collection = CopyFilesRequest { mode: CopyMode::ToCollection, ..request.clone() };
    let plan = api::copy::plan::plan(app, &into_collection).await.unwrap().view;
    assert_eq!((plan.files_to_copy, plan.already_held), (0, 2), "{plan:?}");

    // What `jobs::validate` refuses before a single byte moves.
    let bad = |r: CopyFilesRequest| async move { api::jobs::validate(app, &api::jobs::JobRequest::CopyFiles(r)).await.err().map(|e| e.kind) };
    assert_eq!(bad(CopyFilesRequest { selection: CopySelection::default(), ..request.clone() }).await, Some(ErrorKind::Invalid), "an empty selection");
    assert_eq!(bad(CopyFilesRequest { destination: "not/absolute".into(), ..request.clone() }).await, Some(ErrorKind::Invalid), "a relative destination");
    assert_eq!(bad(CopyFilesRequest { destination: base.join("no-such-folder").display().to_string(), ..request.clone() }).await, Some(ErrorKind::Invalid), "a destination that isn't there");
    // No mounted server yet at this point in the run, so there is nowhere to upload to.
    assert_eq!(bad(CopyFilesRequest { mode: CopyMode::Upload, ..request.clone() }).await, Some(ErrorKind::Invalid), "an upload with no server");
    // to_collection into a folder outside every configured directory has nothing to scan.
    assert_eq!(bad(CopyFilesRequest { mode: CopyMode::ToCollection, ..request.clone() }).await, Some(ErrorKind::Invalid));
    // Into the collection it is, though.
    let inside = base.join("collection").join("copyinto");
    std::fs::create_dir_all(&inside).unwrap();
    let inside_view = api::directories::create(app, &api::directories::DirectorySpec { path: inside.display().to_string(), ..Default::default() }).await.unwrap();
    assert_eq!(bad(CopyFilesRequest { mode: CopyMode::ToCollection, destination: inside.display().to_string(), ..request.clone() }).await, None);

    // Verified: each file is read back and compared against a hash the catalogue ALREADY had
    // (the scan recorded crc32/md5/sha1), and xxh3_128 -- which it did not have -- is filled
    // in off that same read, but only because the comparison passed first.
    std::fs::remove_dir_all(dest.join("snes")).unwrap();
    let content_ids: Vec<u64> = sqlx::query_scalar("SELECT DISTINCT content_id FROM local_files WHERE directory_id = ?").bind(view.directory.id).fetch_all(&app.pool).await.unwrap();
    let xxh_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM hashes WHERE algorithm = 'xxh3_128' AND content_id IN (?, ?)").bind(content_ids[0]).bind(content_ids[1]).fetch_one(&app.pool).await.unwrap();
    assert_eq!(xxh_before, 0, "nothing has recorded xxh3_128 for these yet");
    let checked = api::copy::run::run(app, &CopyFilesRequest { verify: Some(true), ..request.clone() }, &Progress::none(), None).await.unwrap();
    assert_eq!((checked.files_copied, checked.verified), (2, 2), "{checked:?}");
    assert!(checked.verify_failed.is_empty(), "{:?}", checked.verify_failed);
    assert_eq!(checked.hashes_recorded, 2, "the populate hash filled in off the confirming read: {checked:?}");
    let xxh_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM hashes WHERE algorithm = 'xxh3_128' AND content_id IN (?, ?)").bind(content_ids[0]).bind(content_ids[1]).fetch_one(&app.pool).await.unwrap();
    assert_eq!(xxh_after, 2);

    // Bytes that do not match what the catalogue records are reported, and -- the point --
    // record NOTHING. `hashes` hangs off `content`, which the original file shares, so a hash
    // taken from a bad copy would become the catalogue's idea of what those bytes are and go
    // on to declare the original corrupt.
    sqlx::query("DELETE FROM hashes WHERE algorithm = 'xxh3_128' AND content_id IN (?, ?)").bind(content_ids[0]).bind(content_ids[1]).execute(&app.pool).await.unwrap();
    let wrong = dest.join("snes").join("Copy Me (USA).sfc");
    std::fs::write(&wrong, vec![99u8; 4096]).unwrap();
    let (outcome, _) = api::copy::check_file(app, &wrong, content_ids[0], romexplorer::common::models::HashAlgorithms::Xxhash3_128).await.unwrap();
    assert!(matches!(outcome, api::copy::CheckOutcome::Mismatched { .. }), "{outcome:?}");
    let poisoned: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM hashes WHERE algorithm = 'xxh3_128' AND content_id = ?").bind(content_ids[0]).fetch_one(&app.pool).await.unwrap();
    assert_eq!(poisoned, 0, "nothing is written for bytes that did not match");

    // The chosen hash is what a check compares against once the catalogue has it; until then
    // the cheapest recorded one, recording the chosen one off the same read.
    {
        use romexplorer::common::models::HashAlgorithms;
        let right = dest.join("snes").join("Copy Me Too (USA).sfc");
        // By file, not by position: `SELECT DISTINCT` promises no order.
        let too: u64 = sqlx::query_scalar("SELECT content_id FROM local_files WHERE path = ?").bind(dir.join("Copy Me Too (USA).sfc").display().to_string()).fetch_one(&app.pool).await.unwrap();
        sqlx::query("DELETE FROM hashes WHERE algorithm = 'xxh3_128' AND content_id = ?").bind(too).execute(&app.pool).await.unwrap();
        let cheapest = romexplorer::rom::hash::fallback_order()[0];
        let (first, _) = api::copy::check_file(app, &right, too, HashAlgorithms::Xxhash3_128).await.unwrap();
        assert_eq!(first, api::copy::CheckOutcome::Matched { algorithm: cheapest, recorded: true });
        let (second, _) = api::copy::check_file(app, &right, too, HashAlgorithms::Xxhash3_128).await.unwrap();
        assert_eq!(second, api::copy::CheckOutcome::Matched { algorithm: HashAlgorithms::Xxhash3_128, recorded: false });
    }

    // Content with nothing recorded at all cannot be checked, and still nothing is invented.
    sqlx::query("DELETE FROM hashes WHERE content_id = ?").bind(content_ids[1]).execute(&app.pool).await.unwrap();
    let right = dest.join("snes").join("Copy Me Too (USA).sfc");
    let (outcome, read) = api::copy::check_file(app, &right, content_ids[1], romexplorer::common::models::HashAlgorithms::Xxhash3_128).await.unwrap();
    assert_eq!(outcome, api::copy::CheckOutcome::Unverifiable);
    assert_eq!(read, 0, "it does not even read the file: there is nothing to compare it with");
    let invented: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM hashes WHERE content_id = ?").bind(content_ids[1]).fetch_one(&app.pool).await.unwrap();
    assert_eq!(invented, 0);

    api::directories::delete(app, inside_view.directory.id, false).await.unwrap();
    api::directories::delete(app, view.directory.id, false).await.unwrap();
}

/// The headline path: a client copies from a mounted server share, and the copy records what
/// it wrote **without re-hashing a byte**.
///
/// The fixture is the state a client is actually in after a catalogue sync -- it knows the
/// `content` rows (they are shared, identical on both machines) and holds none of the files.
/// Built by scanning the files once to create that content, then dropping this machine's own
/// ownership of them and handing them to a `remote_*` mirror instead.
async fn copy_from_a_mounted_server(app: &App, base: &PathBuf) {
    use romexplorer::common::models::Platform;
    use romexplorer::database::jobs::Progress;
    use romexplorer_types::copy::{CopyFilesRequest, CopyMode, CopySelection, FolderScheme, MountSpec, OwnedRef};

    // --- the share, as it exists on this machine ---
    let share = base.join("server_share");
    std::fs::create_dir_all(&share).unwrap();
    let names = ["Server Game (USA).sfc", "Server Game 2 (USA).sfc"];
    for (i, n) in names.iter().enumerate() {
        std::fs::write(share.join(n), vec![40 + i as u8; 6000 + i * 100]).unwrap();
    }

    // --- content rows, then give up ownership: exactly what a synced client has ---
    // Staged as a root of its own: the share sits beside the collection, not inside it, and
    // a configured directory has to be one or the other.
    let stage = api::directories::create(app, &api::directories::DirectorySpec { path: share.display().to_string(), is_collection_root: true, app_platform_id: Some(Platform::Snes.igdb_id()), ..Default::default() }).await.unwrap();
    let req = api::jobs::FileScanRequest { directory_id: stage.directory.id, mode: FileScanMode::Full, platform: api::jobs::PlatformSelect::All, hash_match: None, file_id: None };
    api::scan::run_file_scan(app, &req, &romexplorer::database::jobs::Progress::none()).await.unwrap();
    let staged: Vec<(u64, String, u64, u64)> = sqlx::query_as("SELECT id, path, content_id, size FROM local_files WHERE directory_id = ? ORDER BY path").bind(stage.directory.id).fetch_all(&app.pool).await.unwrap();
    assert_eq!(staged.len(), 2);
    // The scan also made this machine's rom and game rows for the share; a synced client has
    // none of those either (removing the directory only retires the directory row), so take
    // them away with the files, or the copy's own game would sit next to a stale twin.
    let stage_roms: Vec<u64> = sqlx::query_scalar("SELECT DISTINCT lrf.rom_id FROM local_rom_files lrf JOIN local_files f ON f.id = lrf.file_id WHERE f.directory_id = ?").bind(stage.directory.id).fetch_all(&app.pool).await.unwrap();
    assert!(!stage_roms.is_empty(), "the scan registered the share's roms");
    let mut stage_games: Vec<u64> = Vec::new();
    for rom in &stage_roms {
        stage_games.extend(sqlx::query_scalar::<_, u64>("SELECT game_id FROM local_game_roms WHERE rom_id = ?").bind(rom).fetch_all(&app.pool).await.unwrap());
    }
    stage_games.sort_unstable();
    stage_games.dedup();
    sqlx::query("DELETE lrf FROM local_rom_files lrf JOIN local_files f ON f.id = lrf.file_id WHERE f.directory_id = ?").bind(stage.directory.id).execute(&app.pool).await.unwrap();
    sqlx::query("DELETE FROM local_files WHERE directory_id = ?").bind(stage.directory.id).execute(&app.pool).await.unwrap();
    for rom in &stage_roms {
        for sql in ["DELETE FROM local_game_roms WHERE rom_id = ?", "DELETE FROM local_rom_rahash WHERE rom_id = ?", "DELETE FROM local_rom WHERE id = ?"] {
            sqlx::query(sql).bind(rom).execute(&app.pool).await.unwrap();
        }
    }
    for game in &stage_games {
        for sql in ["DELETE FROM local_release_games WHERE game_id = ?", "DELETE FROM local_game WHERE id = ?"] {
            sqlx::query(sql).bind(game).execute(&app.pool).await.unwrap();
        }
    }
    api::directories::delete(app, stage.directory.id, false).await.unwrap();
    let held: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM local_files WHERE content_id IN (?, ?) AND deleted = FALSE").bind(staged[0].2).bind(staged[1].2).fetch_one(&app.pool).await.unwrap();
    assert_eq!(held, 0, "the client holds none of it, only knows what it is");

    // --- the server's ownership rows, with ITS paths ---
    let source_id = api::sources::create(app, &api::sources::SourceSpec { name: "main".into(), base_url: "http://127.0.0.1:59998".into(), token: Some("t".into()), role: "peer".into(), enabled: true, replace_catalog: false, media: Default::default(), username: None, password: None }).await.unwrap().id;
    let remote_root = "/srv/roms";
    sqlx::query("INSERT INTO remote_directories (source_id, id, path, path_hash, is_collection_root, app_platform_id) VALUES (?, 1, ?, SHA1(?), TRUE, ?)")
        .bind(source_id).bind(remote_root).bind(remote_root).bind(Platform::Snes.igdb_id()).execute(&app.pool).await.unwrap();
    sqlx::query("INSERT INTO remote_game (source_id, id, name) VALUES (?, 1, 'Server Game')").bind(source_id).execute(&app.pool).await.unwrap();
    sqlx::query("INSERT INTO remote_rom (source_id, id, app_platform_id, main_path) VALUES (?, 1, ?, ?)")
        .bind(source_id).bind(Platform::Snes.igdb_id()).bind(format!("{remote_root}/{}", names[0])).execute(&app.pool).await.unwrap();
    sqlx::query("INSERT INTO remote_game_roms (source_id, game_id, rom_id) VALUES (?, 1, 1)").bind(source_id).execute(&app.pool).await.unwrap();
    for (i, (_, _, content_id, size)) in staged.iter().enumerate() {
        let path = format!("{remote_root}/{}", names[i]);
        sqlx::query("INSERT INTO remote_files (source_id, id, path, path_hash, content_id, directory_id, size) VALUES (?, ?, ?, SHA1(?), ?, 1, ?)")
            .bind(source_id).bind(i as u64 + 1).bind(&path).bind(&path).bind(content_id).bind(size).execute(&app.pool).await.unwrap();
        sqlx::query("INSERT INTO remote_rom_files (source_id, rom_id, file_id) VALUES (?, 1, ?)").bind(source_id).bind(i as u64 + 1).execute(&app.pool).await.unwrap();
    }

    // An RA hash on the server's rom: per playable unit, so it hangs off the rom rather than
    // the content and is the one hash a copy has to carry by hand.
    let ra_hash_id: u64 = sqlx::query("INSERT INTO hashes (content_id, algorithm, value_hex) VALUES (NULL, 'ra_hash', 'deadbeefdeadbeefdeadbeefdeadbeef')").execute(&app.pool).await.unwrap().last_insert_id();
    sqlx::query("INSERT INTO remote_rom_rahash (source_id, rom_id, hash_id) VALUES (?, 1, ?)").bind(source_id).bind(ra_hash_id).execute(&app.pool).await.unwrap();

    // --- the mount that makes those paths reachable here ---
    api::copy::mount::create(app, source_id, &MountSpec { remote_prefix: remote_root.into(), local_path: share.display().to_string(), remote_sep: None, remote_directory_id: Some(1), enabled: true }).await.unwrap();

    // --- copy it down ---
    let dest = base.join("collection").join("fromserver");
    std::fs::create_dir_all(&dest).unwrap();
    let dest_view = api::directories::create(app, &api::directories::DirectorySpec { path: dest.display().to_string(), ..Default::default() }).await.unwrap();
    let request = CopyFilesRequest {
        selection: CopySelection { game_ids: vec![OwnedRef { source_id: Some(source_id), id: 1 }], ..Default::default() },
        mode: CopyMode::ToCollection,
        destination: dest.display().to_string(),
        folder_scheme: Some(FolderScheme::Es),
        output: None,
        source_preference: Default::default(),
        on_conflict: Default::default(),
        titles_all_releases: false,
        conflict_choices: Vec::new(),
        upload_source_id: None,
        export_artwork: Vec::new(),
        verify: Some(false),
    };

    let plan = api::copy::plan::plan(app, &request).await.unwrap().view;
    assert_eq!((plan.items, plan.files_to_copy, plan.already_held), (1, 2, 0), "{plan:?}");
    assert!(plan.no_mount.is_empty(), "{:?}", plan.no_mount);

    // Nothing new is hashed, so no content row can appear: the identity came with the rows.
    let content_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM content").fetch_one(&app.pool).await.unwrap();
    let summary = api::copy::run::run(app, &request, &Progress::none(), None).await.unwrap();
    assert_eq!((summary.files_copied, summary.registered), (2, 2), "{summary:?}");
    assert!(summary.errors.is_empty(), "{:?}", summary.errors);
    assert_eq!(summary.verified, 0, "verify was off, so nothing was read back");
    let content_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM content").fetch_one(&app.pool).await.unwrap();
    assert_eq!(content_before, content_after, "a plain copy invents no content");

    // Every hash the server had for these bytes applies to the copy, with verify off and
    // nothing re-read -- because the row points at the same shared content.
    let inherited: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM local_files f JOIN hashes h ON h.content_id = f.content_id WHERE f.directory_id = ? AND h.algorithm IN ('crc32','md5','sha1')",
    )
    .bind(dest_view.directory.id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(inherited, 6, "three hashes each, inherited rather than recomputed");

    // The rom-level RA hash is the one that does not come for free, and is carried.
    assert_eq!(summary.ra_hashes_carried, 1, "{summary:?}");
    let carried: Vec<u64> = sqlx::query_scalar(
        "SELECT rr.hash_id FROM local_rom_rahash rr JOIN local_rom r ON r.id = rr.rom_id WHERE r.main_path LIKE ?",
    )
    .bind(format!("%{}%", names[0]))
    .fetch_all(&app.pool)
    .await
    .unwrap();
    assert_eq!(carried, vec![ra_hash_id], "the copy points at the very same shared ra_hash row");

    // The rows are there, pointing at the SAME content -- no new content row, which is the
    // whole claim: the copy reused an identity it already had instead of re-deriving one.
    let rows: Vec<(String, u64)> = sqlx::query_as("SELECT path, content_id FROM local_files WHERE directory_id = ? ORDER BY path").bind(dest_view.directory.id).fetch_all(&app.pool).await.unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows.iter().all(|(p, _)| p.contains("snes")), "laid out under the platform folder: {rows:?}");
    let mut got: Vec<u64> = rows.iter().map(|(_, c)| *c).collect();
    let mut want: Vec<u64> = staged.iter().map(|(_, _, c, _)| *c).collect();
    got.sort();
    want.sort();
    assert_eq!(got, want, "the copied files carry the server's own content ids");

    let games: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM local_game lg JOIN local_game_roms lgr ON lgr.game_id = lg.id JOIN local_rom lr ON lr.id = lgr.rom_id WHERE lr.main_path LIKE ?")
        .bind(format!("%{}%", names[0])).fetch_one(&app.pool).await.unwrap();
    assert_eq!(games, 1, "one game, holding both files' rom");

    // Re-running is a no-op: everything is now held here as well as there.
    let again = api::copy::plan::plan(app, &request).await.unwrap().view;
    assert_eq!(again.already_held, 2, "{again:?}");

    // A rom that only half arrives must not claim the playable unit's RA hash. Take the
    // second file away from the share and copy into a fresh destination.
    sqlx::query("DELETE FROM local_rom_rahash").execute(&app.pool).await.unwrap();
    sqlx::query("DELETE lrf FROM local_rom_files lrf JOIN local_files f ON f.id = lrf.file_id WHERE f.directory_id = ?").bind(dest_view.directory.id).execute(&app.pool).await.unwrap();
    sqlx::query("DELETE FROM local_files WHERE directory_id = ?").bind(dest_view.directory.id).execute(&app.pool).await.unwrap();
    let half = base.join("collection").join("halfway");
    std::fs::create_dir_all(&half).unwrap();
    let half_view = api::directories::create(app, &api::directories::DirectorySpec { path: half.display().to_string(), ..Default::default() }).await.unwrap();
    std::fs::remove_file(share.join(names[1])).unwrap();
    let partial = api::copy::run::run(app, &CopyFilesRequest { destination: half.display().to_string(), ..request.clone() }, &Progress::none(), None).await.unwrap();
    assert_eq!(partial.files_copied, 1, "only the file still on the share: {partial:?}");
    assert_eq!(partial.ra_hashes_carried, 0, "half a rom is not the playable unit the hash describes");
    assert_eq!(partial.incomplete_items.len(), 1, "{partial:?}");
    api::directories::delete(app, half_view.directory.id, false).await.unwrap();

    api::directories::delete(app, dest_view.directory.id, false).await.unwrap();
    api::sources::delete(app, source_id).await.unwrap();
}

/// The integrity job: one quick hash per file, compared to the catalogue, oldest first.
/// What each of the five modes refuses, and what it does about a destination that is already
/// occupied. The policy predicates are unit-tested; this is the half that needs a database and
/// a real filesystem -- whether `plan`/`validate` actually apply them.
async fn copy_modes(app: &App, base: &PathBuf) {
    use romexplorer::database::jobs::Progress;
    use romexplorer_types::copy::{ConflictChoice, CopyFilesRequest, CopyMode, CopySelection, FolderScheme, MountSpec, OwnedRef};

    let src = base.join("collection").join("modesrc");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("Mode Game (USA).sfc"), fixtures::snes_rom("MODE GAME", 0x8000, 9)).unwrap();
    let view = api::directories::create(app, &api::directories::DirectorySpec { path: src.display().to_string(), app_platform_id: Some(romexplorer::common::models::Platform::Snes.igdb_id()), ..Default::default() }).await.unwrap();
    let scan = api::jobs::FileScanRequest { directory_id: view.directory.id, mode: FileScanMode::Full, platform: api::jobs::PlatformSelect::All, hash_match: None, file_id: None };
    api::scan::run_file_scan(app, &scan, &Progress::none()).await.unwrap();
    let file_id: u64 = sqlx::query_scalar("SELECT id FROM local_files WHERE directory_id = ? AND deleted = FALSE").bind(view.directory.id).fetch_one(&app.pool).await.unwrap();

    let dest = base.join("collection").join("modedest");
    std::fs::create_dir_all(&dest).unwrap();
    let dest_view = api::directories::create(app, &api::directories::DirectorySpec { path: dest.display().to_string(), ..Default::default() }).await.unwrap();
    let base_request = CopyFilesRequest {
        selection: CopySelection { file_ids: vec![OwnedRef::local(file_id)], ..Default::default() },
        mode: CopyMode::InLocal,
        destination: dest.display().to_string(),
        folder_scheme: Some(FolderScheme::Es),
        output: None,
        source_preference: Default::default(),
        on_conflict: Default::default(),
        titles_all_releases: false,
        conflict_choices: Vec::new(),
        upload_source_id: None,
        export_artwork: Vec::new(),
        verify: Some(false),
    };

    // A local-to-local copy skips nothing for being held: a second copy is the whole point,
    // which is what makes it the convert path.
    let plan = api::copy::plan::plan(app, &base_request).await.unwrap().view;
    assert_eq!((plan.files_to_copy, plan.already_held), (1, 0), "in_local skips nothing for being held: {plan:?}");
    let done = api::copy::run::run(app, &base_request, &Progress::none(), None).await.unwrap();
    assert_eq!(done.files_copied, 1, "{done:?}");
    let landed = dest.join("snes").join("Mode Game (USA).sfc");
    assert!(landed.exists());

    // Now put something DIFFERENT in the way, so it is a collision rather than a resume.
    std::fs::write(&landed, vec![1u8; 11]).unwrap();

    // in_local asks. Unanswered is not yes.
    let plan = api::copy::plan::plan(app, &base_request).await.unwrap().view;
    assert_eq!(plan.conflicts, vec![landed.display().to_string()], "the preview names what is in the way: {plan:?}");
    assert_eq!(plan.files_to_copy, 0, "unanswered means skip");
    let skipped = api::copy::run::run(app, &base_request, &Progress::none(), None).await.unwrap();
    assert_eq!(skipped.files_copied, 0, "{skipped:?}");
    assert_eq!(std::fs::metadata(&landed).unwrap().len(), 11, "the file nobody said to destroy is still there");

    // Answered yes, it is replaced.
    let overwrite = CopyFilesRequest { conflict_choices: vec![(landed.display().to_string(), ConflictChoice::Overwrite)], ..base_request.clone() };
    let plan = api::copy::plan::plan(app, &overwrite).await.unwrap().view;
    assert_eq!(plan.files_to_copy, 1, "{plan:?}");
    let replaced = api::copy::run::run(app, &overwrite, &Progress::none(), None).await.unwrap();
    assert_eq!(replaced.files_copied, 1, "{replaced:?}");
    assert_eq!(std::fs::metadata(&landed).unwrap().len(), 0x8000, "the copy won because it was asked to");

    // Answered rename, the file there stays and the copy lands beside it.
    std::fs::write(&landed, vec![1u8; 11]).unwrap();
    let rename = CopyFilesRequest { conflict_choices: vec![(landed.display().to_string(), ConflictChoice::Rename)], ..base_request.clone() };
    let plan = api::copy::plan::plan(app, &rename).await.unwrap().view;
    assert_eq!((plan.files_to_copy, plan.renamed.len()), (1, 1), "{plan:?}");
    assert_eq!(plan.conflicts, vec![landed.display().to_string()], "still listed, so the answer can be changed: {plan:?}");
    let beside = api::copy::run::run(app, &rename, &Progress::none(), None).await.unwrap();
    assert_eq!(beside.files_copied, 1, "{beside:?}");
    assert_eq!(std::fs::metadata(&landed).unwrap().len(), 11, "the file there kept its name");
    assert_eq!(std::fs::metadata(dest.join("snes").join("Mode Game (USA) (2).sfc")).unwrap().len(), 0x8000);
    std::fs::remove_file(dest.join("snes").join("Mode Game (USA) (2).sfc")).unwrap();

    // Answered skip, it is kept -- and that is not a failure.
    let skip = CopyFilesRequest { conflict_choices: vec![(landed.display().to_string(), ConflictChoice::Skip)], ..base_request.clone() };
    let kept = api::copy::run::run(app, &skip, &Progress::none(), None).await.unwrap();
    assert_eq!((kept.files_copied, kept.kept_existing.clone(), kept.blocked_existing.len()), (0, vec![landed.display().to_string()], 0), "{kept:?}");

    // in_server never asks and never overwrites, whatever the request says.
    std::fs::write(&landed, vec![1u8; 11]).unwrap();
    let in_server = CopyFilesRequest { mode: CopyMode::InServer, conflict_choices: vec![(landed.display().to_string(), ConflictChoice::Overwrite)], ..base_request.clone() };
    let plan = api::copy::plan::plan(app, &in_server).await.unwrap().view;
    assert_eq!(plan.files_to_copy, 0, "in_server refuses even an explicit overwrite: {plan:?}");
    let refused = api::copy::run::run(app, &in_server, &Progress::none(), None).await.unwrap();
    assert_eq!(refused.blocked_existing, vec![landed.display().to_string()], "and reports it as the failure it is: {refused:?}");
    assert_eq!(std::fs::metadata(&landed).unwrap().len(), 11);

    // A client's catalogue is its server's, so rearranging the server's collection is not
    // this machine's call to make.
    let mode_key = romexplorer::database::settings::KEY_INSTANCE_MODE;
    romexplorer::database::settings::set(&app.pool, mode_key, &"client").await.unwrap();
    let err = api::jobs::validate(app, &api::jobs::JobRequest::CopyFiles(in_server.clone())).await.err().map(|e| e.kind);
    assert_eq!(err, Some(ErrorKind::Invalid), "in_server is refused on a client");
    romexplorer::database::settings::set(&app.pool, mode_key, &"standalone").await.unwrap();

    // An upload has to be written THROUGH the share, not next to it. A server of our own,
    // mounted at the share `copy_from_a_mounted_server` left on disk (its source, and with
    // it its mount, went when that scenario cleaned up).
    let upstream = api::sources::create(app, &api::sources::SourceSpec { name: "upload target".into(), base_url: "http://127.0.0.1:59997".into(), token: Some("t".into()), role: "peer".into(), enabled: true, replace_catalog: false, media: Default::default(), username: None, password: None }).await.unwrap().id;
    api::copy::mount::create(app, upstream, &MountSpec { remote_prefix: "/srv/roms".into(), local_path: base.join("server_share").display().to_string(), remote_sep: None, remote_directory_id: None, enabled: true }).await.unwrap();
    let upload = CopyFilesRequest { mode: CopyMode::Upload, ..base_request.clone() };
    assert_eq!(
        api::jobs::validate(app, &api::jobs::JobRequest::CopyFiles(upload.clone())).await.err().map(|e| e.kind),
        Some(ErrorKind::Invalid),
        "a destination outside every mount is not an upload"
    );
    let into_share = CopyFilesRequest { destination: base.join("server_share").display().to_string(), ..upload };
    let verdict = api::jobs::validate(app, &api::jobs::JobRequest::CopyFiles(into_share.clone())).await.err();
    assert!(verdict.is_none(), "into the share, it is: {verdict:?}");
    // And it skips what that server already holds -- which is everything under the share.
    let plan = api::copy::plan::plan(app, &into_share).await.unwrap().view;
    assert_eq!(plan.already_held, 0, "this file is not one of the server's: {plan:?}");
    api::sources::delete(app, upstream).await.unwrap();
    api::directories::delete(app, dest_view.directory.id, false).await.unwrap();
    api::directories::delete(app, view.directory.id, false).await.unwrap();
}

async fn integrity(app: &App, base: &PathBuf) {
    use romexplorer::database::jobs::Progress;
    use romexplorer_types::integrity::IntegrityRequest;

    let dir = base.join("collection").join("integrity");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Intact (USA).sfc"), vec![11u8; 3000]).unwrap();
    std::fs::write(dir.join("Rotted (USA).sfc"), vec![12u8; 3000]).unwrap();
    let view = api::directories::create(app, &api::directories::DirectorySpec { path: dir.display().to_string(), app_platform_id: Some(romexplorer::common::models::Platform::Snes.igdb_id()), ..Default::default() }).await.unwrap();
    let req = api::jobs::FileScanRequest { directory_id: view.directory.id, mode: FileScanMode::Full, platform: api::jobs::PlatformSelect::All, hash_match: None, file_id: None };
    api::scan::run_file_scan(app, &req, &romexplorer::database::jobs::Progress::none()).await.unwrap();

    // A copy-registered file is the one worth checking: it has hashes it never computed and
    // no full check. Assert the scan's own stamps first, so the contrast is real.
    let stamps: Vec<(Option<chrono::NaiveDateTime>, Option<chrono::NaiveDateTime>)> =
        sqlx::query_as("SELECT last_full_check, last_integrity_check FROM local_files WHERE directory_id = ?").bind(view.directory.id).fetch_all(&app.pool).await.unwrap();
    assert!(stamps.iter().all(|(full, _)| full.is_some()), "a scan earns the full-check stamp");
    assert!(stamps.iter().all(|(_, integrity)| integrity.is_none()), "and not the integrity one");

    let request = IntegrityRequest { directory_id: Some(view.directory.id), ..Default::default() };
    // First pass: compared against a hash the catalogue already had (crc32 -- cheapest of the
    // three the scan recorded, on any CPU), with xxh3_128 filled in off the same read.
    let first = api::integrity::run(app, &request, &Progress::none(), None).await.unwrap();
    assert_eq!(first.algorithm, "xxh3_128", "the algorithm it populates");
    assert_eq!(first.compared_with.get("crc32").copied(), Some(2), "compared with the cheapest recorded one: {:?}", first.compared_with);
    assert_eq!((first.checked, first.ok, first.recorded), (2, 2, 2), "{first:?}");
    assert!(first.mismatched.is_empty() && first.unverifiable.is_empty());

    // A pass earns the integrity stamp and leaves the full-check one alone.
    let after: Vec<(Option<chrono::NaiveDateTime>, Option<chrono::NaiveDateTime>)> =
        sqlx::query_as("SELECT last_full_check, last_integrity_check FROM local_files WHERE directory_id = ? AND deleted = FALSE").bind(view.directory.id).fetch_all(&app.pool).await.unwrap();
    assert!(after.iter().all(|(full, integrity)| full.is_some() && integrity.is_some()), "{after:?}");

    // Second pass: nothing left to fill in, still compared against crc32.
    let second = api::integrity::run(app, &request, &Progress::none(), None).await.unwrap();
    assert_eq!((second.checked, second.ok, second.recorded), (2, 2, 0), "{second:?}");

    // Rot one file and it is named.
    std::fs::write(dir.join("Rotted (USA).sfc"), vec![13u8; 3000]).unwrap();
    let third = api::integrity::run(app, &request, &Progress::none(), None).await.unwrap();
    assert_eq!(third.mismatched.len(), 1, "{third:?}");
    assert!(third.mismatched[0].ends_with("Rotted (USA).sfc"));

    // A file that is gone is missing, not corrupt, and is soft-deleted as a scan would.
    std::fs::remove_file(dir.join("Intact (USA).sfc")).unwrap();
    let fourth = api::integrity::run(app, &request, &Progress::none(), None).await.unwrap();
    assert_eq!(fourth.missing.len(), 1, "{fourth:?}");
    let deleted: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM local_files WHERE directory_id = ? AND deleted = TRUE").bind(view.directory.id).fetch_one(&app.pool).await.unwrap();
    assert_eq!(deleted, 1);

    // A limit keeps a run short, which is the point of running it often.
    let capped = api::integrity::run(app, &IntegrityRequest { limit: Some(1), ..request.clone() }, &Progress::none(), None).await.unwrap();
    assert!(capped.checked <= 1, "{capped:?}");

    // Content with no recorded hash at all cannot be checked, and nothing is invented for it.
    sqlx::query("DELETE h FROM hashes h JOIN local_files f ON f.content_id = h.content_id WHERE f.directory_id = ? AND f.deleted = FALSE").bind(view.directory.id).execute(&app.pool).await.unwrap();
    let blind = api::integrity::run(app, &request, &Progress::none(), None).await.unwrap();
    assert_eq!(blind.checked, 0, "{blind:?}");
    assert_eq!(blind.unverifiable.len(), 1, "{blind:?}");
    let invented: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM hashes h JOIN local_files f ON f.content_id = h.content_id WHERE f.directory_id = ? AND f.deleted = FALSE").bind(view.directory.id).fetch_one(&app.pool).await.unwrap();
    assert_eq!(invented, 0, "a file with nothing to compare against gets no hash written for it");

    assert!(api::jobs::validate(app, &api::jobs::JobRequest::IntegrityCheck(IntegrityRequest { algorithm: Some("ra_hash".into()), ..Default::default() })).await.is_err(), "ra_hash is per playable unit, not per file");

    api::directories::delete(app, view.directory.id, false).await.unwrap();
}

/// Enqueues `n` background IGDB by-id jobs (one lane) and returns their ids in order.
async fn enqueue_igdb_jobs(pool: &sqlx::MySqlPool, n: u64) -> Vec<u64> {
    let mut ids = Vec::new();
    for i in 0..n {
        ids.push(db_jobs::enqueue(pool, JobType::ProviderQuery, json!({"provider": "igdb", "provider_ids": [format!("{}", 900_000 + i)]})).await.unwrap());
    }
    ids
}
