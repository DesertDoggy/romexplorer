//! End-to-end exercise of `romexplorer::database` against the bundled MariaDB, in the order
//! the plan calls for: server bring-up -> schema -> DAT download (bandwidth-limited) -> DAT
//! parse/import -> the four lookup scenarios (existing hash / made-up hash + real title /
//! real hash + random title / neither) -> one single + one list request per available
//! provider -> one media file per provider (spaced out) -> bulk `by_platform` sync last.
//!
//! One sequential `#[test]` rather than many: the phases share a server and build on each
//! other's data, and provider calls are rate-limited, so ordering matters. Writes
//! `tests/results/database_end_to_end.md` (git-ignored: it carries real game names and
//! hashes pulled from the DATs at run time -- nothing here is hardcoded).
//!
//! Inputs: reference DATs from the `dats` root in `tests/results/test_inputs.tsv` (default
//! `samples/dats/`; git-ignored, populated by `scripts/fetch_*.sh` or by this test's own
//! download phase into `app_root_ex/assets/download/dats/`);
//! credentials from `app_root_ex/assets/api.env` (never echoed). Env knobs:
//! `ROMEXPLORER_DB_TEST_OFFLINE=1` skips every network phase;
//! `ROMEXPLORER_DB_TEST_BIG_DOWNLOADS=1` also downloads the ~100-150MB no-intro/tosec packs;
//! `ROMEXPLORER_DB_TEST_FULL_IMPORT=1` imports every DAT instead of a small per-source subset;
//! `ROMEXPLORER_DB_TEST_SKIP_BULK=1` skips the final bulk sync phase.

#[path = "support/inputs.rs"]
#[allow(dead_code)]
mod inputs;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use romexplorer::common::models::{HashAlgorithms, Platform};
use romexplorer::database::dat_download::{self, DownloadOptions, Source};
use romexplorer::database::dat_parse;
use romexplorer::database::import::{self, ImportOptions, ImportSummary};
use romexplorer::database::jobs::{self, JobType};
use romexplorer::database::lookup;
use romexplorer::database::matching::{resolve, MatchOutcome, ResolveRequest};
use romexplorer::database::providers::raw_store::RawStore;
use romexplorer::database::providers::{bulk, cache, media, Provider, ProviderClients};
use romexplorer::database::server::{check_layout, free_port, MariaDbServer, ServerConfig};
use romexplorer::database::settings::RawRetention;
use romexplorer::database::{self, ApiCredentials, AppPaths, DatabaseConfig};
use sqlx::mysql::MySqlPool;
use sqlx::Row;

const NET_TIMEOUT: Duration = Duration::from_secs(300);

fn env_flag(name: &str) -> bool {
    std::env::var(name).map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(false)
}

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

struct Report {
    out: String,
    passed: u32,
    failed: u32,
    skipped: u32,
}

impl Report {
    fn new(title: &str) -> Self {
        let mut out = format!("# {title}\n\n");
        out.push_str(&format!("Run at (unix seconds): {}\n\n", SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)));
        Self { out, passed: 0, failed: 0, skipped: 0 }
    }

    fn section(&mut self, name: &str) {
        self.out.push_str(&format!("\n## {name}\n\n"));
        eprintln!("== {name}");
    }

    fn line(&mut self, text: &str) {
        self.out.push_str(text);
        self.out.push('\n');
        eprintln!("{text}");
    }

    fn pass(&mut self, what: &str, detail: &str, elapsed: Duration) {
        self.passed += 1;
        self.line(&format!("- PASS `{what}` ({} ms): {}", elapsed.as_millis(), escape(detail)));
    }

    fn fail(&mut self, what: &str, detail: &str, elapsed: Duration) {
        self.failed += 1;
        self.line(&format!("- **FAIL** `{what}` ({} ms): {}", elapsed.as_millis(), escape(detail)));
    }

    fn skip(&mut self, what: &str, why: &str) {
        self.skipped += 1;
        self.line(&format!("- SKIP `{what}`: {}", escape(why)));
    }

    fn write(&self, test_name: &str) {
        let dir = crate_dir().join("tests/results");
        let _ = std::fs::create_dir_all(&dir);
        let mut text = self.out.clone();
        text.insert_str(text.find("\n\n").map(|i| i + 2).unwrap_or(0), &format!("**{} passed, {} failed, {} skipped**\n\n", self.passed, self.failed, self.skipped));
        let _ = std::fs::write(dir.join(format!("{test_name}.md")), text);
    }
}

fn escape(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}

/// Runs `fut` with a timeout, recording PASS/FAIL with the closure's summary string.
async fn step<F, T>(report: &mut Report, what: &str, fut: F) -> Option<T>
where
    F: std::future::Future<Output = Result<(T, String), String>>,
{
    let started = Instant::now();
    match tokio::time::timeout(NET_TIMEOUT, fut).await {
        Ok(Ok((value, detail))) => {
            report.pass(what, &detail, started.elapsed());
            Some(value)
        }
        Ok(Err(err)) => {
            report.fail(what, &err, started.elapsed());
            None
        }
        Err(_) => {
            report.fail(what, &format!("timed out after {NET_TIMEOUT:?}"), started.elapsed());
            None
        }
    }
}

struct Env {
    server: MariaDbServer,
    pool: MySqlPool,
    paths: AppPaths,
    clients: ProviderClients,
    http: reqwest::Client,
    store: RawStore,
    offline: bool,
}

/// A test database is tiny: a small cache, not the quarter of this machine's memory the app
/// takes, which fails to start while the app's own server is running.
fn small_pool(mut cfg: ServerConfig) -> ServerConfig {
    cfg.buffer_pool_bytes = 256 * 1024 * 1024;
    cfg
}

async fn bring_up(report: &mut Report) -> Result<Env, String> {
    let paths = AppPaths::from_root(crate_dir().join("app_root_ex"));
    check_layout(&paths.mariadb_dir()).map_err(|e| e.to_string())?;

    // A throwaway data dir per run under tests/output/ (git-ignored), never the app's own.
    let base = crate_dir().join("tests/output/mariadb_test");
    let _ = std::fs::remove_dir_all(&base);
    let datadir = base.join("data");
    let port = free_port().map_err(|e| e.to_string())?;
    let mut server = MariaDbServer::new(small_pool(ServerConfig::new(paths.mariadb_dir(), &datadir, port)));
    let started = Instant::now();
    server.start().await.map_err(|e| e.to_string())?;
    report.pass("server start (bootstrap + ready)", &format!("port {port}, datadir {}", datadir.display()), started.elapsed());

    let config = DatabaseConfig::default().with_port(port).with_database("romexplorer_test");
    let started = Instant::now();
    let pool = database::initialize(&config).await.map_err(|e| e.to_string())?;
    let platform_rows: i64 = sqlx::query("SELECT COUNT(*) AS n FROM platforms").fetch_one(&pool).await.map_err(|e| e.to_string())?.try_get("n").map_err(|e| e.to_string())?;
    report.pass("create database + migrate + seed platforms", &format!("{platform_rows} platform rows"), started.elapsed());

    let creds = ApiCredentials::load(&paths.api_env()).map_err(|e| e.to_string())?;
    let http = database::providers::build_http_client().map_err(|e| e.to_string())?;
    let clients = ProviderClients::from_credentials(&creds, http.clone());
    report.line(&format!("- credential keys present in api.env: {}", creds.keys().join(", ")));
    report.line(&format!("- providers available: {}", clients.available().iter().map(|p| p.name()).collect::<Vec<_>>().join(", ")));
    for (provider, why) in &clients.unavailable {
        report.line(&format!("- provider unavailable: {} -- {}", provider.name(), escape(why)));
    }
    let store = RawStore::new(paths.provider_cache_dir(), RawRetention::KeepLatest);
    let offline = env_flag("ROMEXPLORER_DB_TEST_OFFLINE");
    Ok(Env { server, pool, paths, clients, http, store, offline })
}

// ---------------------------------------------------------------------------
// phases
// ---------------------------------------------------------------------------

async fn phase_jobs(env: &Env, report: &mut Report) {
    report.section("Jobs queue");
    let pool = &env.pool;
    step(report, "enqueue/claim/complete/fail round trip", async {
        let id = jobs::enqueue(pool, JobType::ProviderQuery, serde_json::json!({"provider": "hasheous", "content_id": 1})).await.map_err(|e| e.to_string())?;
        let claimed = jobs::claim(pool, JobType::ProviderQuery, 5).await.map_err(|e| e.to_string())?;
        if claimed.len() != 1 || claimed[0].id != id {
            return Err(format!("expected to claim job {id}, got {:?}", claimed.iter().map(|j| j.id).collect::<Vec<_>>()));
        }
        let again = jobs::claim(pool, JobType::ProviderQuery, 5).await.map_err(|e| e.to_string())?;
        if !again.is_empty() {
            return Err("claimed an in-progress job twice".to_string());
        }
        jobs::fail(pool, &claimed[0], "simulated").await.map_err(|e| e.to_string())?;
        let retried = jobs::claim(pool, JobType::ProviderQuery, 5).await.map_err(|e| e.to_string())?;
        if retried.len() != 1 || retried[0].attempts != 2 {
            return Err("failed job was not returned to pending for a retry".to_string());
        }
        jobs::complete(pool, id).await.map_err(|e| e.to_string())?;
        let pending = jobs::pending_count(pool, None).await.map_err(|e| e.to_string())?;
        Ok(((), format!("job {id}: claimed once, retried after failure, completed; pending now {pending}")))
    })
    .await;
    step(report, "a running job with no worker behind it can be cancelled", async {
        let e = |e: database::DatabaseError| e.to_string();
        let status = |id: u64| async move { jobs::get(pool, id).await.map(|j| j.map(|j| (j.status, j.cancel_requested))) };
        let claimed = |payload: serde_json::Value| async move {
            let id = jobs::enqueue(pool, JobType::DbOptimize, payload).await?;
            jobs::claim_by_id(pool, id).await?;
            Ok::<u64, database::DatabaseError>(id)
        };
        // No worker lock held: nothing runs the job, so a cancel is done at once.
        let orphan = claimed(serde_json::json!({"t": 1})).await.map_err(e)?;
        let at_once = jobs::cancel(pool, orphan).await.map_err(e)?;
        // A live worker (this process, lock held): the cancel is asked for, and the worker's
        // sweep does it once no task of its own runs the job.
        let lock = jobs::hold_worker_lock(pool).await.ok_or("could not take the worker lock")?;
        let owned = claimed(serde_json::json!({"t": 2})).await.map_err(e)?;
        let asked = jobs::cancel(pool, owned).await.map_err(e)?;
        let while_running = jobs::release_orphans(pool, &[owned]).await.map_err(e)?;
        let swept = jobs::release_orphans(pool, &[]).await.map_err(e)?;
        // One with no cancel goes back to pending, and finishing a cancelled job keeps it cancelled.
        let lost = claimed(serde_json::json!({"t": 3})).await.map_err(e)?;
        let requeued = jobs::release_orphans(pool, &[]).await.map_err(e)?;
        jobs::complete(pool, orphan).await.map_err(e)?;
        let (s_orphan, s_owned, s_lost) = (status(orphan).await.map_err(e)?, status(owned).await.map_err(e)?, status(lost).await.map_err(e)?);
        jobs::cancel(pool, lost).await.map_err(e)?;
        // A restart: a running job a cancel was asked for is cancelled when the worker stops,
        // not queued again; the one without a cancel is queued again; and a cancelled job
        // stays cancelled whatever a restart does.
        let cancelling = claimed(serde_json::json!({"t": 4})).await.map_err(e)?;
        let resuming = claimed(serde_json::json!({"t": 5})).await.map_err(e)?;
        // A copy is not started again by a restart: it waits for the user to retry it.
        let copy_claimed = |destination: &'static str| async move {
            let id = jobs::enqueue(pool, JobType::CopyFiles, serde_json::json!({"destination": destination})).await?;
            jobs::claim_by_id(pool, id).await?;
            Ok::<u64, database::DatabaseError>(id)
        };
        let copy_stopped = copy_claimed("restart-test-a").await.map_err(e)?;
        jobs::cancel(pool, cancelling).await.map_err(e)?;
        jobs::release_interrupted(pool, &[cancelling, resuming, orphan, copy_stopped]).await.map_err(e)?;
        let copy_reclaimed = jobs::claim_by_id(pool, copy_stopped).await.map_err(e)?.is_some();
        let r_copy_stopped = status(copy_stopped).await.map_err(e)?;
        let copy_retried = jobs::retry(pool, copy_stopped).await.map_err(e)?;
        let r_copy_retried = status(copy_stopped).await.map_err(e)?;
        jobs::cancel(pool, copy_stopped).await.map_err(e)?;
        drop(lock);
        let after_restart = jobs::release_orphans(pool, &[]).await.map_err(e)?;
        // And a copy whose worker went away (a crash): cancelled by the sweep, not queued again.
        let copy_lost = copy_claimed("restart-test-b").await.map_err(e)?;
        let copy_swept = jobs::release_orphans(pool, &[]).await.map_err(e)?;
        let r_copy_lost = status(copy_lost).await.map_err(e)?;
        let reclaimed = jobs::claim_by_id(pool, cancelling).await.map_err(e)?.is_some();
        let (r_cancelling, r_resuming, r_orphan) = (status(cancelling).await.map_err(e)?, status(resuming).await.map_err(e)?, status(orphan).await.map_err(e)?);
        let checks = [
            (r_copy_stopped.as_ref().map(|(s, _)| s.as_str()) == Some("cancelled") && !copy_reclaimed, format!("a copy stopped by a shutdown is cancelled, not resumed: {r_copy_stopped:?}, claimed again {copy_reclaimed}")),
            (copy_retried && r_copy_retried.as_ref().map(|(s, _)| s.as_str()) == Some("pending"), format!("and retry starts it again: {copy_retried} {r_copy_retried:?}")),
            (copy_swept.cancelled == vec![copy_lost] && copy_swept.requeued.is_empty() && r_copy_lost.as_ref().map(|(s, _)| s.as_str()) == Some("cancelled"), format!("a copy with no worker behind it is cancelled by the sweep: {copy_swept:?} {r_copy_lost:?}")),
            (r_cancelling.as_ref().map(|(s, c)| (s.as_str(), *c)) == Some(("cancelled", false)) && !reclaimed, format!("a cancel asked for before a shutdown sticks: {r_cancelling:?}, claimed again {reclaimed}")),
            (r_resuming.as_ref().map(|(s, _)| s.as_str()) == Some("pending"), format!("a job without a cancel resumes: {r_resuming:?}")),
            (r_orphan.as_ref().map(|(s, _)| s.as_str()) == Some("cancelled") && after_restart == jobs::Orphans::default(), format!("a cancelled job stays cancelled: {r_orphan:?} {after_restart:?}")),
            (at_once == Some(jobs::CancelOutcome::Cancelled), format!("no worker: cancelled at once ({at_once:?})")),
            (asked == Some(jobs::CancelOutcome::Requested), format!("live worker: requested ({asked:?})")),
            (while_running == jobs::Orphans::default(), format!("its own running task is left alone: {while_running:?}")),
            (swept.cancelled == vec![owned], format!("then cancelled by the sweep: {swept:?}")),
            (requeued.requeued == vec![lost], format!("a lost job runs again: {requeued:?}")),
            (s_orphan.as_ref().map(|(s, _)| s.as_str()) == Some("cancelled") && s_owned.as_ref().map(|(s, c)| (s.as_str(), *c)) == Some(("cancelled", false)) && s_lost.as_ref().map(|(s, _)| s.as_str()) == Some("pending"), format!("rows: {s_orphan:?} {s_owned:?} {s_lost:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("jobs {orphan}, {owned} cancelled; {lost} queued again")))
    })
    .await;
    step(report, "a job added to the queue waits its turn and hides nothing behind it", async {
        let e = |e: database::DatabaseError| e.to_string();
        // Two that wait their turn, then an ordinary one in the same lane group as the first.
        let first = jobs::enqueue_in_turn(pool, JobType::DatSync, serde_json::json!({"source": "no-intro"})).await.map_err(e)?;
        let second = jobs::enqueue_in_turn(pool, JobType::DbOptimize, serde_json::json!({})).await.map_err(e)?;
        let ordinary = jobs::enqueue(pool, JobType::DatSync, serde_json::json!({"source": "redump"})).await.map_err(e)?;
        let heads: Vec<u64> = jobs::peek_lane_heads(pool).await.map_err(e)?.iter().map(|j| j.id).collect();
        let next = jobs::peek_next_in_turn(pool).await.map_err(e)?.map(|j| (j.id, j.in_turn));
        // The first has its turn; the second is then the one in line.
        jobs::claim_by_id(pool, first).await.map_err(e)?;
        jobs::complete(pool, first).await.map_err(e)?;
        let then = jobs::peek_next_in_turn(pool).await.map_err(e)?.map(|j| j.id);
        for id in [second, ordinary] {
            jobs::cancel(pool, id).await.map_err(e)?;
        }
        let checks = [
            (heads.contains(&ordinary) && !heads.contains(&first) && !heads.contains(&second), format!("the ordinary job heads its lane, the waiting ones head none: {heads:?} (waiting {first}, {second}; ordinary {ordinary})")),
            (next == Some((first, true)), format!("the oldest waiting job is the next in turn: {next:?}")),
            (then == Some(second), format!("then the one queued after it: {then:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("jobs {first} and {second} wait their turn in order; job {ordinary} starts in its lane regardless")))
    })
    .await;
}

/// `release_merge`: puts the old per-source unique key back, adds one disc as two releases
/// (redump cue/bin + mame-redump CHD, like the real catalog had) with links, local games,
/// files and a title on both, then checks the merge folds them into one.
async fn phase_release_merge(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "release merge: same platform + name from two DATs becomes one release", async {
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        sqlx::query("ALTER TABLE `release` DROP INDEX uq_release_identity, ADD UNIQUE KEY uq_release_identity (source_id, app_platform_id, name_hash)").execute(pool).await.map_err(e)?;
        let redump = import::ensure_source(pool, "redump").await.map_err(de)?;
        let chd = import::ensure_source(pool, "mame-redump").await.map_err(de)?;
        let platform = Platform::Dreamcast.igdb_id();
        let name = "Merge Test (USA)";
        let hash = import::sha1_hex(name);
        let mut ids = Vec::new();
        for (source, region, serial, demo) in [(redump, Some("USA"), None::<&str>, false), (chd, None, Some("T-1234"), true)] {
            let id = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, serial, source_id, is_demo) VALUES (?, ?, ?, 'merge test', ?, ?, ?, ?, ?)")
                .bind(platform).bind(name).bind(&hash).bind(import::sha1_hex("merge test")).bind(region).bind(serial).bind(source).bind(demo)
                .execute(pool).await.map_err(e)?.last_insert_id();
            sqlx::query("INSERT INTO release_sources (release_id, source_id) VALUES (?, ?)").bind(id).bind(source).execute(pool).await.map_err(e)?;
            let content = sqlx::query("INSERT INTO content (app_platform_id, file_name, size) VALUES (?, ?, 1)").bind(platform).bind(format!("merge{id}.bin")).execute(pool).await.map_err(e)?.last_insert_id();
            sqlx::query("INSERT INTO release_contents (release_id, content_id, file_name, source_id) VALUES (?, ?, ?, ?)").bind(id).bind(content).bind(format!("merge{id}.bin")).bind(source).execute(pool).await.map_err(e)?;
            ids.push(id);
        }
        let title = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, 'merge test', ?)").bind(name).bind(import::sha1_hex("merge test")).execute(pool).await.map_err(e)?.last_insert_id();
        for id in &ids {
            sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(title).bind(id).execute(pool).await.map_err(e)?;
        }
        // Survivor: a fuzzy RA guess; the other: a hash-confirmed id -> the hash one wins.
        sqlx::query("INSERT INTO release_provider_links (release_id, provider, provider_id, confidence) VALUES (?, 'retroachievements', '111', 'fuzzy'), (?, 'retroachievements', '222', 'hash')").bind(ids[0]).bind(ids[1]).execute(pool).await.map_err(e)?;
        let game = sqlx::query("INSERT INTO local_game (name) VALUES ('merge test chd')").execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO local_release_games (release_id, game_id) VALUES (?, ?)").bind(ids[1]).bind(game).execute(pool).await.map_err(e)?;

        let summary = database::release_merge::merge_duplicate_releases(pool).await.map_err(de)?.ok_or("merge did nothing")?;
        let one = |sql: &'static str| async move { sqlx::query_scalar::<_, i64>(sql).fetch_one(pool).await };
        let releases = one("SELECT COUNT(*) FROM `release` WHERE name = 'Merge Test (USA)'").await.map_err(e)?;
        let row = sqlx::query("SELECT id, region, serial, is_demo FROM `release` WHERE name = 'Merge Test (USA)'").fetch_one(pool).await.map_err(e)?;
        let (id, region, serial, demo): (u64, Option<String>, Option<String>, bool) = (row.try_get("id").map_err(e)?, row.try_get("region").map_err(e)?, row.try_get("serial").map_err(e)?, row.try_get("is_demo").map_err(e)?);
        let sources = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM release_sources WHERE release_id = ?").bind(id).fetch_one(pool).await.map_err(e)?;
        let files = sqlx::query_scalar::<_, i64>("SELECT COUNT(DISTINCT source_id) FROM release_contents WHERE release_id = ?").bind(id).fetch_one(pool).await.map_err(e)?;
        let link = sqlx::query("SELECT provider_id, confidence FROM release_provider_links WHERE release_id = ? AND provider = 'retroachievements'").bind(id).fetch_one(pool).await.map_err(e)?;
        let (link_id, confidence): (String, String) = (link.try_get("provider_id").map_err(e)?, link.try_get("confidence").map_err(e)?);
        let local = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM local_release_games WHERE release_id = ?").bind(id).fetch_one(pool).await.map_err(e)?;
        let titled = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM title_releases WHERE release_id = ?").bind(id).fetch_one(pool).await.map_err(e)?;
        let again = database::release_merge::merge_duplicate_releases(pool).await.map_err(de)?;
        let dup = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, source_id) VALUES (?, ?, ?, 'merge test', ?, ?)")
            .bind(platform).bind(name).bind(&hash).bind(import::sha1_hex("merge test")).bind(redump).execute(pool).await;
        let checks = [
            (releases == 1, format!("{releases} releases left")),
            (id == ids[0], "the lowest id survives".to_string()),
            (region.as_deref() == Some("USA") && serial.as_deref() == Some("T-1234"), format!("attributes combined: {region:?} {serial:?}")),
            (demo, "a flag any source sets is kept".to_string()),
            (sources == 2 && files == 2, format!("{sources} sources, {files} dumps")),
            (link_id == "222" && confidence == "hash", format!("stronger link kept: {link_id} {confidence}")),
            (local == 1 && titled == 1, format!("{local} local game, {titled} title link")),
            (again.is_none(), "second run is a no-op".to_string()),
            (dup.is_err(), "platform + name is now unique".to_string()),
            (summary.merged_rows >= 1, format!("{} row(s) merged", summary.merged_rows)),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} row(s) merged into {} release(s); sources, dumps, links, local games and title carried over", summary.merged_rows, summary.groups)))
    })
    .await;
}

/// `title_merge`: regional titles sharing an IGDB id on one platform merge, by hash or by
/// name -- no file on disk needed (canonical US name, the Japanese name kept as an alias); a
/// fuzzy search guess and a port on another platform go to the backlog instead.
async fn phase_title_merge(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "title merge: regional names sharing an IGDB id become one title", async {
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let src = import::ensure_source(pool, "redump").await.map_err(de)?;
        // (title name, release name, region, platform, igdb id, confidence)
        let rows = [
            ("Shin Legend of Hogehoge Test", "Shin Legend of Hogehoge Test (Japan)", "JP", Platform::PlayStation2, "990001", "hash"),
            ("Legend of Hogehoge Test", "Legend of Hogehoge Test (USA)", "US", Platform::PlayStation2, "990001", "exact"),
            ("Legend of Hogehoge Test", "Legend of Hogehoge Test (USA)", "US", Platform::PlayStation, "990001", "hash"),
            ("Hoge Chronicle Test", "Hoge Chronicle Test (Europe)", "EU", Platform::PlayStation2, "990002", "hash"),
            ("Dark Hoge Test 2", "Dark Hoge Test 2 (USA)", "US", Platform::PlayStation2, "990002", "fuzzy"),
            ("Hoge Senki Test", "Hoge Senki Test (Japan)", "JP", Platform::PlayStation2, "990003", "name"),
            ("Hoge Wars Test", "Hoge Wars Test (USA)", "US", Platform::PlayStation2, "990003", "name"),
        ];
        sqlx::query("INSERT IGNORE INTO genre (name) VALUES ('Merge Test Genre')").execute(pool).await.map_err(e)?;
        let mut titles = Vec::new();
        for (tname, rname, region, platform, igdb, conf) in rows {
            let norm = tname.to_lowercase();
            let t = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, ?, ?)").bind(tname).bind(&norm).bind(import::sha1_hex(&norm)).execute(pool).await.map_err(e)?.last_insert_id();
            let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (?, ?, ?, ?, ?, ?, ?)")
                .bind(platform.igdb_id()).bind(rname).bind(import::sha1_hex(rname)).bind(&norm).bind(import::sha1_hex(&norm)).bind(region).bind(src)
                .execute(pool).await.map_err(e)?.last_insert_id();
            sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(t).bind(r).execute(pool).await.map_err(e)?;
            sqlx::query("INSERT INTO title_provider_links (title_id, provider, provider_id, confidence) VALUES (?, 'igdb', ?, ?)").bind(t).bind(igdb).bind(conf).execute(pool).await.map_err(e)?;
            sqlx::query("INSERT INTO title_genres (title_id, genre_id, source) SELECT ?, id, 'test' FROM genre ORDER BY id LIMIT 1").bind(t).execute(pool).await.map_err(e)?;
            titles.push(t);
        }
        let summary = database::title_merge::dedupe_by_igdb(pool, None, &database::jobs::Progress::none()).await.map_err(de)?;
        let name: String = sqlx::query_scalar("SELECT name FROM title WHERE id = ?").bind(titles[0]).fetch_one(pool).await.map_err(e)?;
        let gone: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title WHERE id = ? AND deleted = FALSE").bind(titles[1]).fetch_one(pool).await.map_err(e)?;
        let releases: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title_releases WHERE title_id = ?").bind(titles[0]).fetch_one(pool).await.map_err(e)?;
        let alias: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title_aliases WHERE title_id = ? AND norm_hash = ?").bind(titles[0]).bind(import::sha1_hex("shin legend of hogehoge test")).fetch_one(pool).await.map_err(e)?;
        let logged: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title_merge_log WHERE survivor_id = ? AND reason = 'igdb:990001'").bind(titles[0]).fetch_one(pool).await.map_err(e)?;
        let port_alive: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title WHERE id = ?").bind(titles[2]).fetch_one(pool).await.map_err(e)?;
        let hash_grade: String = sqlx::query_scalar("SELECT grade FROM title_merge_log WHERE survivor_id = ? AND reason = 'igdb:990001'").bind(titles[0]).fetch_one(pool).await.map_err(e)?;
        let by_name: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title_merge_log WHERE survivor_id = ? AND merged_title_id = ? AND reason = 'igdb:990003' AND grade = 'name'").bind(titles[5]).bind(titles[6]).fetch_one(pool).await.map_err(e)?;
        let dark_alive: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM title WHERE id IN ({}, {})", titles[3], titles[4]))).fetch_one(pool).await.map_err(e)?;
        let sug: Vec<(String, String)> = sqlx::query_as("SELECT igdb_id, reason FROM title_merge_suggestions WHERE igdb_id IN ('990001', '990002') ORDER BY igdb_id").fetch_all(pool).await.map_err(e)?;
        // Several at once: dismissing the lot is one step, and a dismissed one can still be
        // merged -- by itself below, or with the rest.
        let dismissed = database::title_merge::resolve_suggestions(pool, None, "pending", false).await.map_err(de)?;
        let still_pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title_merge_suggestions WHERE status = 'pending'").fetch_one(pool).await.map_err(e)?;
        let unknown = database::title_merge::resolve_suggestions(pool, Some(&["no-such-game".to_string()]), "dismissed", true).await.map_err(de)?;
        if dismissed.resolved < 1 || still_pending != 0 || unknown.resolved != 0 || unknown.failed.len() != 1 {
            return Err(format!("bulk: dismissed {dismissed:?}, {still_pending} still pending, a missing one {unknown:?}"));
        }
        let accepted = database::title_merge::resolve_suggestion(pool, "990002", true).await.map_err(de)?;
        let dark_name: String = sqlx::query_scalar("SELECT name FROM title WHERE id = ?").bind(accepted.unwrap_or(0)).fetch_one(pool).await.map_err(e)?;
        // Decoupling brings the JP title back as it was, and the automatic pass leaves it be.
        let log_id: u64 = sqlx::query_scalar("SELECT id FROM title_merge_log WHERE merged_title_id = ? AND undone_at IS NULL").bind(titles[1]).fetch_one(pool).await.map_err(e)?;
        let restored = database::title_merge::decouple(pool, log_id).await.map_err(de)?;
        let back: (String, i64) = sqlx::query_as("SELECT name, CAST(deleted AS SIGNED) FROM title WHERE id = ?").bind(restored).fetch_one(pool).await.map_err(e)?;
        let own = |t: u64, table: &'static str| sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table} WHERE title_id = {t}"))).fetch_one(pool);
        let (back_releases, back_genres, back_links, kept_releases) = (own(restored, "title_releases").await.map_err(e)?, own(restored, "title_genres").await.map_err(e)?, own(restored, "title_provider_links").await.map_err(e)?, own(titles[0], "title_releases").await.map_err(e)?);
        let survivor_name: String = sqlx::query_scalar("SELECT name FROM title WHERE id = ?").bind(titles[0]).fetch_one(pool).await.map_err(e)?;
        database::title_merge::dedupe_by_igdb(pool, None, &database::jobs::Progress::none()).await.map_err(de)?;
        let still_apart: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title WHERE id = ? AND deleted = FALSE").bind(restored).fetch_one(pool).await.map_err(e)?;
        let checks = [
            (summary.merges >= 1 && gone == 0, format!("{} merge(s); JP/US PS2 titles merged: {}", summary.merges, gone == 0)),
            (name == "Legend of Hogehoge Test", format!("canonical name from the US release: {name:?}")),
            (releases == 2, format!("{releases} releases on the merged title")),
            (alias == 1 && logged == 1, format!("alias kept {alias}, logged {logged}")),
            (port_alive == 1, "the PS1 port stays its own title".to_string()),
            (dark_alive == 2, "a fuzzy search guess doesn't merge".to_string()),
            (by_name == 1, "titles linked to one IGDB game by name merge, with no file on disk, at grade name".to_string()),
            (hash_grade == "hash", format!("a hash + exact merge is grade hash: {hash_grade:?}")),
            // 990001 is left on PS2 and on PlayStation: the same game on two platforms is two
            // titles on purpose, and not something to suggest merging.
            (sug == vec![("990002".to_string(), "name_link".to_string())], format!("backlog: {sug:?}")),
            (dark_name == "Dark Hoge Test 2", format!("accepting a suggestion merges and names by region: {dark_name:?}")),
            (restored == titles[1] && back == ("Legend of Hogehoge Test".to_string(), 0) && survivor_name == "Shin Legend of Hogehoge Test", format!("decoupled into #{restored} {back:?}; survivor now {survivor_name:?}")),
            ((back_releases, back_genres, back_links, kept_releases) == (1, 1, 1, 1), format!("decoupled rows: releases {back_releases}, genres {back_genres}, links {back_links}; survivor keeps {kept_releases}")),
            (still_apart == 1, "the automatic pass doesn't merge a decoupled pair again".to_string()),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} merge(s); backlog {} suggestion(s); accepting one merged into {:?}", summary.merges, summary.suggestions, dark_name)))
    })
    .await;
}

/// `title_merge` incremental: a pass looks only at titles touched since the last one. A pair
/// already there when the mark was taken is left for a full pass; a pair added after it merges.
async fn phase_title_merge_incremental(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "title merge: an incremental pass looks only at what changed", async {
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        use database::title_merge::{self as merge, PassScope};
        let src = import::ensure_source(pool, "redump").await.map_err(de)?;
        let add = |tname: &'static str, rname: &'static str, region: &'static str, igdb: &'static str, linked: Option<&'static str>| async move {
            let norm = tname.to_lowercase();
            let t = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, ?, ?)").bind(tname).bind(&norm).bind(import::sha1_hex(&norm)).execute(pool).await?.last_insert_id();
            let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (?, ?, ?, ?, ?, ?, ?)")
                .bind(Platform::PlayStation2.igdb_id()).bind(rname).bind(import::sha1_hex(rname)).bind(&norm).bind(import::sha1_hex(&norm)).bind(region).bind(src)
                .execute(pool).await?.last_insert_id();
            sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(t).bind(r).execute(pool).await?;
            sqlx::query("INSERT INTO title_provider_links (title_id, provider, provider_id, confidence, linked_at) VALUES (?, 'igdb', ?, 'hash', COALESCE(?, NOW()))").bind(t).bind(igdb).bind(linked).execute(pool).await?;
            Ok::<u64, sqlx::Error>(t)
        };
        // Looked at by an earlier pass (linked long ago), but never merged.
        let old = [add("Hoge Incremental Old Test", "Hoge Incremental Old Test (USA)", "US", "990101", Some("2000-01-01 00:00:00")).await.map_err(e)?, add("Hoge Incremental Old Test", "Hoge Incremental Old Test (Japan)", "JP", "990101", Some("2000-01-01 00:00:00")).await.map_err(e)?];
        let mark = merge::pass_start(pool).await.map_err(de)?;
        let new = [add("Fuga Incremental New Test", "Fuga Incremental New Test (USA)", "US", "990102", None).await.map_err(e)?, add("Fuga Incremental New Test", "Fuga Incremental New Test (Japan)", "JP", "990102", None).await.map_err(e)?];
        let scope = merge::collect_dirty(pool, &mark).await.map_err(de)?;
        let dirty = scope.dirty.clone().unwrap_or_default();
        let alive = |t: u64| sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM title WHERE id = ? AND deleted = FALSE").bind(t).fetch_one(pool);
        merge::dedupe_by_igdb_in(pool, None, &database::jobs::Progress::none(), &scope).await.map_err(de)?;
        merge::clear_dirty(pool).await.map_err(de)?;
        let (old_after_inc, new_after_inc) = (alive(old[0]).await.map_err(e)? + alive(old[1]).await.map_err(e)?, alive(new[0]).await.map_err(e)? + alive(new[1]).await.map_err(e)?);
        merge::dedupe_by_igdb_in(pool, None, &database::jobs::Progress::none(), &PassScope::full()).await.map_err(de)?;
        let old_after_full = alive(old[0]).await.map_err(e)? + alive(old[1]).await.map_err(e)?;
        // A mark from a catalogue with more titles than this one is of no use.
        let replaced = merge::PassMark { max_title_id: mark.max_title_id + 1_000_000, ..mark.clone() };
        merge::save_pass(pool, &mark).await.map_err(de)?;
        let stored = merge::stored_pass(pool).await.map_err(de)?;
        merge::forget_pass(pool).await.map_err(de)?;
        let forgotten = merge::stored_pass(pool).await.map_err(de)?;
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title_merge_dirty").fetch_one(pool).await.map_err(e)?;
        let checks = [
            (new.iter().all(|t| dirty.contains(t)) && !old.iter().any(|t| dirty.contains(t)), format!("dirty: new {new:?} in, old {old:?} out of {} title(s)", dirty.len())),
            (new_after_inc == 1, format!("the new pair merges in an incremental pass ({new_after_inc} left)")),
            (old_after_inc == 2, format!("the untouched pair is left alone ({old_after_inc} left)")),
            (old_after_full == 1, format!("a full pass merges it ({old_after_full} left)")),
            (merge::usable_pass(Some(replaced), &mark).is_none() && merge::usable_pass(Some(mark.clone()), &mark).is_some(), "a mark beyond the catalogue's ids is ignored".to_string()),
            (stored.as_ref() == Some(&mark) && forgotten.is_none(), format!("saved {stored:?}, forgotten {forgotten:?}")),
            (left == 0, format!("{left} row(s) left in the work table")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} title(s) touched since the mark; the old pair waited for a full pass", dirty.len())))
    })
    .await;
}

/// `reapply_igdb`: on its own it takes only titles with no IGDB names yet; given titles, it
/// takes those, names or not (no IGDB request here: the title has no cached game).
async fn phase_igdb_reapply_selected(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "IGDB re-apply: selected titles are re-applied even with names", async {
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let norm = "hoge reapply test";
        let t = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES ('Hoge Reapply Test', ?, ?)").bind(norm).bind(import::sha1_hex(norm)).execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO title_provider_links (title_id, provider, provider_id, confidence) VALUES (?, 'igdb', '99990777', 'exact')").bind(t).execute(pool).await.map_err(e)?;
        sqlx::query("INSERT INTO title_locales (title_id, language, region, source, name) VALUES (?, 'ja', 'JP', 'igdb', 'ホゲ')").bind(t).execute(pool).await.map_err(e)?;
        let progress = database::jobs::Progress::none();
        use database::providers::metadata::{reapply_igdb, ReapplyScope};
        let every = reapply_igdb(&env.clients, pool, ReapplyScope::Missing, &progress).await.map_err(de)?;
        let chosen = reapply_igdb(&env.clients, pool, ReapplyScope::Titles(&[t]), &progress).await.map_err(de)?;
        let none = reapply_igdb(&env.clients, pool, ReapplyScope::Titles(&[]), &progress).await.map_err(de)?;
        // All: every linked title, this one with its names included.
        let all = reapply_igdb(&env.clients, pool, ReapplyScope::All, &progress).await.map_err(de)?;
        if all.titles < every.titles + 1 {
            return Err(format!("all takes every linked title, named ones too: {all:?} against {every:?}"));
        }
        let skipped: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title_locales WHERE title_id = ?").bind(t).fetch_one(pool).await.map_err(e)?;
        if (chosen.titles, chosen.missing_raw, none.titles) != (1, 1, 0) || skipped != 1 {
            return Err(format!("chosen {chosen:?}, empty {none:?}, every {every:?}, locales {skipped}"));
        }
        Ok(((), format!("unselected run took {} title(s) without it; the selected title was taken", every.titles)))
    })
    .await;
}

/// The name dictionary (built from the catalogue, edited by hand), searches that try the
/// other script, audio auto-tagging with its tags kept apart from the user's, the keyword tag
/// filter, and the merge pass taking the Super Famicom and the SNES as one platform.
async fn phase_name_dictionary(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "name dictionary: built from the catalogue, used by search and the auto-tagger", async {
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let ae = |e: romexplorer::api::Error| e.message;
        let app = romexplorer::api::App::with_pool(pool.clone(), env.paths.clone(), DatabaseConfig::default()).map_err(ae)?;
        let src = import::ensure_source(pool, "no-intro").await.map_err(de)?;
        let add = |tname: &'static str, rname: &'static str, platform: Platform| async move {
            let norm = tname.to_lowercase();
            let t = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, ?, ?)").bind(tname).bind(&norm).bind(import::sha1_hex(&norm)).execute(pool).await?.last_insert_id();
            let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (?, ?, ?, ?, ?, 'US', ?)")
                .bind(platform.igdb_id()).bind(rname).bind(import::sha1_hex(rname)).bind(&norm).bind(import::sha1_hex(&norm)).bind(src)
                .execute(pool).await?.last_insert_id();
            sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(t).bind(r).execute(pool).await?;
            Ok::<u64, sqlx::Error>(t)
        };
        // A numbered title with its Japanese name, and another of the series with none.
        let four = add("Piyo Saga 4", "Piyo Saga 4 (USA)", Platform::Snes).await.map_err(e)?;
        sqlx::query("INSERT INTO title_locales (title_id, language, region, source, name) VALUES (?, 'ja', 'JP', 'igdb', 'ピヨサーガ4')").bind(four).execute(pool).await.map_err(e)?;
        let two = add("Piyo Saga 2", "Piyo Saga 2 (USA)", Platform::Snes).await.map_err(e)?;
        let progress = database::jobs::Progress::none();
        let built = database::dictionary::rebuild(pool, &env.http, &env.paths.provider_cache_dir(), false, &progress).await.map_err(de)?;
        let variants = romexplorer::api::dictionary::variants(&app, "ピヨサーガ2").await.map_err(ae)?;
        let found = |search: &'static str| {
            let app = &app;
            async move { romexplorer::api::catalog::titles(app, &romexplorer::api::catalog::ListQuery { search: Some(search.into()), ..Default::default() }).await.map(|p| p.items.into_iter().map(|t| t.id).collect::<Vec<u64>>()) }
        };
        let by_japanese = found("ピヨサーガ").await.map_err(ae)?;
        // Switched off, a search is only what was typed.
        sqlx::query("INSERT INTO settings (`key`, value) VALUES ('search_translations', 'false') ON DUPLICATE KEY UPDATE value = 'false'").execute(pool).await.map_err(e)?;
        let off = found("ピヨサーガ").await.map_err(ae)?;
        sqlx::query("DELETE FROM settings WHERE `key` = 'search_translations'").execute(pool).await.map_err(e)?;
        // The user's own names, and an automatic group removed for good.
        romexplorer::api::dictionary::add(&app, &romexplorer::api::dictionary::DictAdd { names: vec!["Mogera Quest".into(), "モゲラクエスト".into()] }).await.map_err(ae)?;
        let mine = romexplorer::api::dictionary::variants(&app, "モゲラクエスト").await.map_err(ae)?;
        romexplorer::api::dictionary::remove(&app, &romexplorer::api::dictionary::DictRemove { group_keys: vec![format!("title:{four}")], ids: Vec::new() }).await.map_err(ae)?;
        database::dictionary::rebuild(pool, &env.http, &env.paths.provider_cache_dir(), false, &progress).await.map_err(de)?;
        let blocked: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM name_dictionary WHERE group_key = ?").bind(format!("title:{four}")).fetch_one(pool).await.map_err(e)?;
        let kept: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM name_dictionary WHERE source = 'user' AND term = 'モゲラクエスト'").fetch_one(pool).await.map_err(e)?;

        // Audio: an album named in Japanese, one the user tagged, and one nothing matches.
        let audio = |path: &'static str, album: &'static str| async move {
            sqlx::query("INSERT INTO audio_files (path, path_hash, file_name, size, format, album) VALUES (?, ?, '01.flac', 1, 'flac', ?)").bind(path).bind(import::sha1_hex(path)).bind(album).execute(pool).await.map(|r| r.last_insert_id())
        };
        let ost = audio("/music/a/01.flac", "ピヨサーガ2 オリジナル・サウンドトラック").await.map_err(e)?;
        let manual = audio("/music/b/01.flac", "Piyo Saga 2 Original Soundtrack").await.map_err(e)?;
        romexplorer::api::audio::add_tag(&app, &romexplorer::api::audio::AudioTagRequest { ids: vec![manual], name: "My Own Tag".into() }).await.map_err(ae)?;
        let stray = audio("/music/c/01.flac", "Completely Unknown Hogehoge Album").await.map_err(e)?;
        let tagged = romexplorer::api::audio_autotag::run(&app, &progress).await.map_err(ae)?;
        let tags_of = |id: u64| async move { sqlx::query_as::<_, (String, String)>("SELECT name, source FROM audio_file_tags WHERE audio_file_id = ? ORDER BY name").bind(id).fetch_all(pool).await };
        let (ost_tags, manual_tags, stray_tags) = (tags_of(ost).await.map_err(e)?, tags_of(manual).await.map_err(e)?, tags_of(stray).await.map_err(e)?);
        let filtered = |source: &'static str| {
            let app = &app;
            async move {
                romexplorer::api::catalog::titles(app, &romexplorer::api::catalog::ListQuery { tags: Some("Piyo Saga 2".into()), tag_source: Some(source.into()), ..Default::default() })
                    .await
                    .map(|p| p.items.into_iter().map(|t| t.id).collect::<Vec<u64>>())
            }
        };
        let (auto_titles, user_titles) = (filtered("auto").await.map_err(ae)?, filtered("user").await.map_err(ae)?);
        let removed = romexplorer::api::titles::remove_auto_tags(&app, &romexplorer::api::titles::AutoTagRemove { ids: None, search: Some("Piyo Saga".into()) }).await.map_err(ae)?;
        let manual_after = tags_of(manual).await.map_err(e)?;

        // One game filed under the Super Famicom in one DAT and the SNES in another.
        let sfc = add("Fuga Region Test", "Fuga Region Test (Japan)", Platform::SuperFamicom).await.map_err(e)?;
        let snes = add("Fuga Region Test", "Fuga Region Test (USA)", Platform::Snes).await.map_err(e)?;
        for t in [sfc, snes] {
            sqlx::query("INSERT INTO title_provider_links (title_id, provider, provider_id, confidence) VALUES (?, 'igdb', '990777', 'hash')").bind(t).execute(pool).await.map_err(e)?;
        }
        database::title_merge::dedupe_by_igdb(pool, Some("990777"), &progress).await.map_err(de)?;
        let alive: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM title WHERE id IN ({sfc}, {snes}) AND deleted = FALSE"))).fetch_one(pool).await.map_err(e)?;

        let checks = [
            (built.catalog_groups >= 1 && built.series_groups >= 1, format!("built {built:?}")),
            (variants.iter().any(|v| v == "Piyo Saga 2"), format!("the series name carries the number across: {variants:?}")),
            (by_japanese.contains(&four) && by_japanese.contains(&two), format!("a Japanese search finds both, the one with no Japanese name too: {by_japanese:?} (want {four}, {two})")),
            (off.contains(&four) && !off.contains(&two), format!("switched off, only the localized name matches: {off:?}")),
            (mine == vec!["Mogera Quest".to_string()] && kept == 1, format!("your own names: {mine:?}, kept through a rebuild {kept}")),
            (blocked == 0, format!("a removed automatic group stays out of a rebuild ({blocked} rows)")),
            (ost_tags == vec![("Piyo Saga 2".to_string(), "auto".to_string())], format!("the Japanese album is tagged automatically: {ost_tags:?} ({tagged:?})")),
            (manual_tags == vec![("My Own Tag".to_string(), "manual".to_string())], format!("a file you tagged is left alone: {manual_tags:?}")),
            (stray_tags.is_empty(), format!("an album nothing matches gets nothing: {stray_tags:?}")),
            (auto_titles == vec![two] && user_titles.is_empty(), format!("the tag filter tells automatic from yours: auto {auto_titles:?}, yours {user_titles:?}")),
            (removed.title_tags == 1 && removed.file_tags == 1 && manual_after == manual_tags, format!("removing automatic tags leaves yours: {removed:?}, then {manual_after:?}")),
            (alive == 1, format!("Super Famicom and SNES titles of one game merge ({alive} left)")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} name(s); {} album(s) matched; translated search and tag filters work", built.names, tagged.matched)))
    })
    .await;
}

/// A release for two regions shows the localized name of the one first in region priority,
/// whichever locale was stored first.
async fn phase_localized_by_region(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "localized names: a release for Japan and Korea follows region priority", async {
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let ae = |e: romexplorer::api::Error| e.message;
        let app = romexplorer::api::App::with_pool(pool.clone(), env.paths.clone(), DatabaseConfig::default()).map_err(ae)?;
        let src = import::ensure_source(pool, "no-intro").await.map_err(de)?;
        let name = "Hoge Region Pick (Japan, Korea)";
        let t = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES ('Hoge Region Pick', 'hoge region pick', ?)").bind(import::sha1_hex("hoge region pick")).execute(pool).await.map_err(e)?.last_insert_id();
        let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (19, ?, ?, 'hoge region pick', ?, 'JP,KR', ?)")
            .bind(name).bind(import::sha1_hex(name)).bind(import::sha1_hex("hoge region pick")).bind(src)
            .execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(t).bind(r).execute(pool).await.map_err(e)?;
        // The Korean name stored first.
        sqlx::query("INSERT INTO title_locales (title_id, language, region, source, name) VALUES (?, 'kr', 'KR', 'igdb', '호게'), (?, 'ja', 'JP', 'igdb', 'ホゲ')").bind(t).bind(t).execute(pool).await.map_err(e)?;
        sqlx::query("INSERT INTO settings (`key`, value) VALUES ('title_display_mode', '\"local\"') ON DUPLICATE KEY UPDATE value = VALUES(value)").execute(pool).await.map_err(e)?;
        let names = || {
            let app = &app;
            async move {
                let q = romexplorer::api::catalog::ListQuery { search: Some("Hoge Region Pick".into()), ..Default::default() };
                let title = romexplorer::api::catalog::titles(app, &q).await?.items.into_iter().find(|x| x.id == t).map(|x| x.display_name);
                let release = romexplorer::api::catalog::releases(app, &q).await?.items.into_iter().find(|x| x.id == r).map(|x| x.display_name);
                Ok::<_, romexplorer::api::Error>((title, release))
            }
        };
        let default = names().await.map_err(ae)?;
        sqlx::query("INSERT INTO settings (`key`, value) VALUES ('region_priority', '[\"KR\", \"JP\"]') ON DUPLICATE KEY UPDATE value = VALUES(value)").execute(pool).await.map_err(e)?;
        let korea_first = names().await.map_err(ae)?;
        sqlx::query("DELETE FROM settings WHERE `key` IN ('title_display_mode', 'region_priority')").execute(pool).await.map_err(e)?;
        let ja = (Some("ホゲ".to_string()), Some("ホゲ".to_string()));
        let kr = (Some("호게".to_string()), Some("호게".to_string()));
        if default != ja || korea_first != kr {
            return Err(format!("default {default:?} (want Japanese), Korea first {korea_first:?} (want Korean)"));
        }
        Ok(((), "Japanese by default, Korean with Korea first".to_string()))
    })
    .await;
}

/// Editions (budget / special / aftermarket) and versions from release names, filled into
/// existing rows by the one-time backfill; the user's manual release order survives a title
/// merge (appended after the survivor's order) and a decouple.
async fn phase_release_editions(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "release editions + manual order: backfill flags, order through merge / decouple", async {
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let src = import::ensure_source(pool, "redump").await.map_err(de)?;
        let platform = Platform::PlayStation2.igdb_id();
        let names = [
            ("Edition Test (Japan) (PlayStation 2 the Best)", "JP", None),
            ("Edition Test (Japan) (Shokai Genteiban)", "JP", None),
            ("Edition Test (Japan) (v1.02)", "JP", Some("v1.02")),
            ("Edition Test (Japan) (Taikenban)", "JP", None),
            ("Edition Test (USA)", "US", None),
        ];
        let norm = "edition test";
        let mut t_jp = 0u64;
        let mut ids = Vec::new();
        for (i, (name, region, revision)) in names.iter().enumerate() {
            // Two titles: the JP releases, and the USA one (to merge them later).
            if i == 0 || i == 4 {
                let tn = if i == 0 { "Edition Test JP" } else { "Edition Test US" };
                let t = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, ?, ?)").bind(tn).bind(tn.to_lowercase()).bind(import::sha1_hex(&tn.to_lowercase())).execute(pool).await.map_err(e)?.last_insert_id();
                if i == 0 {
                    t_jp = t;
                }
                ids.push((t, Vec::new()));
            }
            let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, revision, source_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?)")
                .bind(platform).bind(name).bind(import::sha1_hex(name)).bind(norm).bind(import::sha1_hex(norm)).bind(region).bind(revision).bind(src)
                .execute(pool).await.map_err(e)?.last_insert_id();
            let (t, list) = ids.last_mut().unwrap();
            sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(*t).bind(r).execute(pool).await.map_err(e)?;
            list.push(r);
        }
        sqlx::query("DELETE FROM data_migrations WHERE name = 'release_editions_0015'").execute(pool).await.map_err(e)?;
        database::release_editions::backfill(pool).await.map_err(de)?;
        let flags = |id: u64| async move {
            sqlx::query_as::<_, (bool, bool, bool, u32, bool)>("SELECT is_budget, is_special_edition, is_aftermarket, revision_sort, is_demo FROM `release` WHERE id = ?").bind(id).fetch_one(pool).await
        };
        let jp = ids[0].1.clone();
        let (budget, special, rev, demo) = (flags(jp[0]).await.map_err(e)?, flags(jp[1]).await.map_err(e)?, flags(jp[2]).await.map_err(e)?, flags(jp[3]).await.map_err(e)?);

        // Manual order on both titles, then merge: US title's order goes after the JP one's.
        use database::release_order::{plan_move, write_weights};
        let current = |t: u64| async move { sqlx::query_as::<_, (u64, Option<f64>)>("SELECT release_id, sort_weight FROM title_releases WHERE title_id = ? ORDER BY COALESCE(sort_weight, 1e300), release_id").bind(t).fetch_all(pool).await };
        let w = plan_move(&current(t_jp).await.map_err(e)?, jp[2], None).map_err(|x| x)?;
        write_weights(pool, t_jp, &w).await.map_err(de)?;
        let (t_us, us) = (ids[1].0, ids[1].1.clone());
        let w = plan_move(&current(t_us).await.map_err(e)?, us[0], None).map_err(|x| x)?;
        write_weights(pool, t_us, &w).await.map_err(de)?;
        database::title_merge::merge_titles(pool, &[t_jp, t_us], "manual", "manual").await.map_err(de)?;
        let merged: Vec<u64> = current(t_jp).await.map_err(e)?.into_iter().map(|(id, _)| id).collect();
        let log_id: u64 = sqlx::query_scalar("SELECT id FROM title_merge_log WHERE merged_title_id = ? AND undone_at IS NULL").bind(t_us).fetch_one(pool).await.map_err(e)?;
        database::title_merge::decouple(pool, log_id).await.map_err(de)?;
        let back: Vec<(u64, Option<f64>)> = current(t_us).await.map_err(e)?;
        let checks = [
            (budget.0 && !budget.1, format!("budget flag: {budget:?}")),
            (special.1 && !special.0, format!("special flag: {special:?}")),
            (rev.3 > 0 && !rev.0 && !rev.1, format!("v1.02 is a plain version: {rev:?}")),
            (demo.4, format!("Taikenban is a demo: {demo:?}")),
            (merged.first() == Some(&jp[2]) && merged.last() == Some(&us[0]) && merged.len() == 5, format!("merged order {merged:?} (JP v1.02 first, US last)")),
            (back.len() == 1 && back[0].0 == us[0] && back[0].1.is_some(), format!("decoupled US title keeps its release and weight: {back:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("flags ok; merged order {merged:?}")))
    })
    .await;
}

/// Migration 0044 on rows as an older import left them: a release that is nothing but PDFs,
/// or whose DAT category says it is reading matter, becomes a document; a game that ships
/// its manual, and a BIOS pack's icons, stay what they are.
async fn phase_document_backfill(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "document backfill: PDFs and guide categories leave the games", async {
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let src = import::ensure_source(pool, "tosec").await.map_err(de)?;
        let mut ids = Vec::new();
        for (name, category, material, files) in [
            ("Docfill Guide (1992)(Legend)", Some("Game Guides"), None, vec!["Docfill Guide.bin"]),
            ("Docfill Manual (1994)(Interplay)(US)", Some("Games"), None, vec!["Docfill Manual.PDF"]),
            ("Docfill Patch Notes (USA) (Update)", None, Some("update"), vec!["notes.pdf", "more.cbz"]),
            ("Docfill Game (USA)", Some("Games"), None, vec!["game.iso", "manual.pdf"]),
            ("Docfill Icons", Some("BIOS"), None, vec!["icon.png"]),
            ("Docfill Empty (USA)", Some("Games"), None, vec![]),
        ] {
            let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, source_id, category, material) VALUES (0, ?, ?, ?, ?, ?, ?, ?)")
                .bind(name).bind(import::sha1_hex(name)).bind(name.to_lowercase()).bind(import::sha1_hex(&name.to_lowercase())).bind(src).bind(category).bind(material)
                .execute(pool).await.map_err(e)?.last_insert_id();
            for file in files {
                let content = sqlx::query("INSERT INTO content (app_platform_id, file_name, size) VALUES (0, ?, 1)").bind(file).execute(pool).await.map_err(e)?.last_insert_id();
                sqlx::query("INSERT INTO release_contents (release_id, content_id, file_name) VALUES (?, ?, ?)").bind(r).bind(content).bind(file).execute(pool).await.map_err(e)?;
            }
            ids.push(r);
        }
        sqlx::raw_sql(include_str!("../../migrations/0044_release_documents.sql")).execute(pool).await.map_err(e)?;
        let mut got = Vec::new();
        for id in &ids {
            got.push(sqlx::query_scalar::<_, Option<String>>("SELECT material FROM `release` WHERE id = ?").bind(id).fetch_one(pool).await.map_err(e)?);
        }
        let doc = Some("document".to_string());
        let expected = vec![doc.clone(), doc.clone(), doc, None, None, None];
        if got != expected {
            return Err(format!("guide by category, manual by its PDF, PDF-only update, game with a manual, icons, no files: {got:?}"));
        }
        Ok(((), format!("{got:?}")))
    })
    .await;
}

/// Disc media, documents and source code have no title: migration 0046 takes the ones an
/// older import made away, a reclassifying sync does the same later, and from then on such a
/// release is under a title only when linked by hand.
async fn phase_standalone(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "standalone material: no title until linked; listed under Multimedia", async {
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let ae = |e: romexplorer::api::Error| e.message;
        use romexplorer::api::catalog::{self, ListQuery};
        use romexplorer::api::titles;
        let app = romexplorer::api::App::with_pool(pool.clone(), env.paths.clone(), DatabaseConfig::default()).map_err(ae)?;
        let src = import::ensure_source(pool, "redump").await.map_err(de)?;
        let title = |name: &'static str| async move {
            let norm = name.to_lowercase();
            sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, ?, ?)").bind(name).bind(&norm).bind(import::sha1_hex(&norm)).execute(pool).await.map(|r| r.last_insert_id())
        };
        let release = |name: &'static str, material: Option<&'static str>, under: u64| async move {
            let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, source_id, material) VALUES (0, ?, ?, ?, ?, ?, ?)")
                .bind(name).bind(import::sha1_hex(name)).bind(name.to_lowercase()).bind(import::sha1_hex(&name.to_lowercase())).bind(src).bind(material)
                .execute(pool).await?.last_insert_id();
            sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(under).bind(r).execute(pool).await?;
            Ok::<u64, sqlx::Error>(r)
        };
        let home = |release: u64| async move { sqlx::query_scalar::<_, u64>("SELECT title_id FROM title_releases WHERE release_id = ?").bind(release).fetch_optional(pool).await };
        let live = |title: u64| async move { sqlx::query_scalar::<_, bool>("SELECT deleted = FALSE FROM title WHERE id = ?").bind(title).fetch_one(pool).await };

        // As an older import left it: a CD with a title of its own, and a film that landed
        // under a game's title because the names matched.
        let (cd_title, game_title) = (title("Standalone Album").await.map_err(e)?, title("Standalone Game").await.map_err(e)?);
        let cd = release("Standalone Album (Japan)", Some("audio"), cd_title).await.map_err(e)?;
        let game = release("Standalone Game (USA)", None, game_title).await.map_err(e)?;
        let film = release("Standalone Game (USA) (Making Of)", Some("video"), game_title).await.map_err(e)?;
        sqlx::raw_sql(include_str!("../../migrations/0046_standalone_titles.sql")).execute(pool).await.map_err(e)?;
        let after_migration = (live(cd_title).await.map_err(e)?, home(cd).await.map_err(e)?, live(game_title).await.map_err(e)?, home(game).await.map_err(e)?, home(film).await.map_err(e)?);

        // Listed under Multimedia by their own names, and not among the releases.
        let listed = |material: Option<&'static str>| {
            let app = &app;
            async move {
                let q = ListQuery { search: Some("Standalone".into()), material: material.map(str::to_string), limit: Some(100), view: Some("collection".into()), ..Default::default() };
                catalog::releases(app, &q).await.map(|p| p.items.into_iter().map(|r| (r.id, r.title_id)).collect::<Vec<_>>())
            }
        };
        let multimedia = listed(Some("multimedia")).await.map_err(ae)?;
        let as_releases = listed(None).await.map_err(ae)?;

        // Linked by hand, the film is the game's; unlinked, it stands alone again. A game
        // release cannot be left without a title.
        titles::link_release(&app, film, &titles::ReleaseTitle { title_id: game_title }).await.map_err(ae)?;
        let linked = home(film).await.map_err(e)?;
        let on_page: Vec<Option<String>> = catalog::title(&app, game_title).await.map_err(ae)?.releases.into_iter().map(|r| r.material).collect();

        // A later sync marks a release that still has its first import's title: that title
        // goes, the hand-made link stays.
        let doc_title = title("Standalone Guide").await.map_err(e)?;
        let doc = release("Standalone Guide (1992)", Some("document"), doc_title).await.map_err(e)?;
        let gone = database::title_merge::drop_standalone_titles(pool).await.map_err(de)?;
        let after_sync = (live(doc_title).await.map_err(e)?, home(doc).await.map_err(e)?, home(film).await.map_err(e)?);

        titles::unlink_release(&app, film).await.map_err(ae)?;
        let unlinked = home(film).await.map_err(e)?;
        let game_refused = titles::unlink_release(&app, game).await.is_err();
        let checks = [
            (after_migration == (false, None, true, Some(game_title), None), format!("the CD's title is gone and neither disc has one; the game keeps its own: {after_migration:?}")),
            (multimedia.len() == 2 && multimedia.iter().all(|(id, t)| (*id == cd || *id == film) && t.is_none()), format!("Multimedia lists both discs, untitled: {multimedia:?}")),
            (as_releases == vec![(game, Some(game_title))], format!("Releases lists only the game: {as_releases:?}")),
            (linked == Some(game_title) && on_page == vec![None, Some("video".to_string())], format!("linked by hand the film is on the game's page, after the game: {linked:?} {on_page:?}")),
            (gone == 1 && after_sync == (false, None, Some(game_title)), format!("a reclassifying sync removes the guide's title and leaves the hand-made link: {gone} {after_sync:?}")),
            (unlinked.is_none() && game_refused, format!("unlinking frees the film; a game release is refused: {unlinked:?} {game_refused}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} disc(s) under Multimedia", multimedia.len())))
    })
    .await;
}

/// A catalogue backup, written several tables at a time: every table is in the zip, in the
/// list's own order (a restore reads them in that order), with every row, and nothing of the
/// part files it was assembled from is left behind.
async fn phase_backup(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "backup: tables written in parallel come out whole and in order", async {
        use romexplorer::api::backup::{self, Kind};
        use std::io::BufRead;
        let ae = |e: romexplorer::api::Error| e.message;
        let de = |e: database::DatabaseError| e.to_string();
        let app = romexplorer::api::App::with_pool(pool.clone(), env.paths.clone(), DatabaseConfig::default()).map_err(ae)?;
        let dir = crate_dir().join("tests").join("output").join("mariadb_database_test_backups");
        let _ = std::fs::remove_dir_all(&dir);
        database::settings::set(pool, database::settings::KEY_BACKUP_DIR, &dir.display().to_string()).await.map_err(de)?;
        let started = Instant::now();
        let summary = backup::create(&app, Kind::Catalog, &jobs::Progress::none()).await.map_err(ae)?;
        let took = started.elapsed();

        let path = dir.join(&summary.file.name);
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let names: Vec<String> = (0..zip.len()).map(|i| zip.by_index_raw(i).map(|f| f.name().to_string()).unwrap_or_default()).collect();
        let expected: Vec<String> = backup::tables(Kind::Catalog).iter().map(|t| format!("{t}.ndjson")).chain(["manifest.json".to_string()]).collect();
        if names != expected {
            return Err(format!("members out of order or missing: {names:?}"));
        }
        // Every row is there: what the summary says, what the file holds, what the table has.
        let mut total = 0u64;
        for (table, rows) in &summary.tables {
            let lines = std::io::BufReader::new(zip.by_name(&format!("{table}.ndjson")).map_err(|e| e.to_string())?).lines().count() as u64;
            let live: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM `{table}`"))).fetch_one(pool).await.map_err(|e| e.to_string())?;
            if lines != *rows || lines != live as u64 {
                return Err(format!("{table}: {rows} in the summary, {lines} in the file, {live} in the table"));
            }
            total += rows;
        }
        let stray: Vec<String> = std::fs::read_dir(&dir).map_err(|e| e.to_string())?.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| *n != summary.file.name).collect();
        database::settings::set(pool, database::settings::KEY_BACKUP_DIR, &String::new()).await.map_err(de)?;
        let _ = std::fs::remove_dir_all(&dir);
        if total < 1000 || !stray.is_empty() {
            return Err(format!("{total} row(s) backed up; left in the folder: {stray:?}"));
        }
        Ok(((), format!("{} tables, {total} rows in {:.1}s", summary.tables.len(), took.as_secs_f64())))
    })
    .await;
}

/// A DAT made by `dat_convert` and imported as the `hacks` source: a translation is the
/// original's region and the translation's language, every version of it is one release
/// under the original's title, another DAT's name for one of those versions joins it, and
/// what is not a game is not a title.
/// BIOS, firmware and system updates are `material = 'bios'`: no title, not among the games,
/// listed under Collection › BIOS / firmware with their platform -- from the entry's name, its
/// category or a BIOS-only DAT; and a catalogue imported the old way is put right by the
/// title merge pass.
async fn phase_bios(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "bios: marked at import, kept out of titles, listed with platform", async {
        use romexplorer::database::dat_parse;
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let ae = |e: romexplorer::api::Error| e.message;
        let app = romexplorer::api::App::with_pool(pool.clone(), env.paths.clone(), DatabaseConfig::default()).map_err(ae)?;
        let no_intro = "<datafile><header><name>Nintendo - Game Boy Advance</name></header>\
            <game name=\"[BIOS] Biostest Boot ROM (World)\"><rom name=\"gba_bios.bin\" size=\"8\" sha1=\"00000000000000000000000000000000000000d1\"/></game>\
            <game name=\"Biostest Game (USA)\"><rom name=\"Biostest Game (USA).gba\" size=\"8\" sha1=\"00000000000000000000000000000000000000d2\"/></game></datafile>";
        import::import_dat(pool, Source::NoIntro, "bios-no-intro.dat", &dat_parse::parse_dat_xml(no_intro).map_err(de)?, &ImportOptions::default()).await.map_err(de)?;
        // A DAT of nothing but BIOS images: its entries say nothing themselves.
        let redump = "<datafile><header><name>Sony - PlayStation - BIOS Images</name></header>\
            <game name=\"Biostest PS (v1.0) (USA)\"><rom name=\"scph1001.bin\" size=\"8\" sha1=\"00000000000000000000000000000000000000d3\"/></game></datafile>";
        import::import_dat(pool, Source::Redump, "bios-redump.dat", &dat_parse::parse_dat_xml(redump).map_err(de)?, &ImportOptions::default()).await.map_err(de)?;
        let kind_of = |name: &'static str| async move {
            sqlx::query_as::<_, (Option<String>, i64)>("SELECT r.material, (SELECT COUNT(*) FROM title_releases tr WHERE tr.release_id = r.id) FROM `release` r WHERE r.name = ? AND r.deleted = FALSE").bind(name).fetch_one(pool).await
        };
        let boot = kind_of("[BIOS] Biostest Boot ROM (World)").await.map_err(e)?;
        let game = kind_of("Biostest Game (USA)").await.map_err(e)?;
        let ps = kind_of("Biostest PS (v1.0) (USA)").await.map_err(e)?;
        let titles: Vec<String> = romexplorer::api::catalog::titles(&app, &romexplorer::api::catalog::ListQuery { search: Some("Biostest".into()), limit: Some(50), view: Some("collection".into()), ..Default::default() }).await.map_err(ae)?.items.into_iter().map(|t| t.name).collect();
        let listed: Vec<(String, Option<String>, String)> = romexplorer::api::bios::list(&app, &romexplorer::api::bios::BiosQuery { search: Some("Biostest".into()), ..Default::default() }).await.map_err(ae)?.items.into_iter().map(|r| (r.name, r.platform, r.kind)).collect();

        // "My files" is everything held: the DAT entry a local file matches as well as a
        // system file no DAT lists -- and not a DAT entry nobody holds.
        let content_of = |name: &'static str| async move {
            sqlx::query_scalar::<_, u64>("SELECT rc.content_id FROM release_contents rc JOIN `release` r ON r.id = rc.release_id WHERE r.name = ? LIMIT 1").bind(name).fetch_one(pool).await
        };
        let boot_content = content_of("[BIOS] Biostest Boot ROM (World)").await.map_err(e)?;
        let other_content = content_of("Biostest Game (USA)").await.map_err(e)?;
        sqlx::query("INSERT INTO local_files (path, path_hash, content_id, size) VALUES ('/bios/gba/gba_bios.bin', ?, ?, 8)").bind(import::sha1_hex("/bios/gba/gba_bios.bin")).bind(boot_content).execute(pool).await.map_err(e)?;
        sqlx::query("INSERT INTO local_files (path, path_hash, content_id, size, system_type, system_platform) VALUES ('/bios/3ds/biostest_nand.bin', ?, ?, 8, 'nand', 37)").bind(import::sha1_hex("/bios/3ds/biostest_nand.bin")).bind(other_content).execute(pool).await.map_err(e)?;
        let mine: Vec<(String, String, bool)> = romexplorer::api::bios::list(&app, &romexplorer::api::bios::BiosQuery { from: Some("own".into()), search: Some("biostest".into()), ..Default::default() }).await.map_err(ae)?.items.into_iter().map(|r| (r.name, r.from, r.held)).collect();
        sqlx::query("DELETE FROM local_files WHERE path_hash IN (?, ?)").bind(import::sha1_hex("/bios/gba/gba_bios.bin")).bind(import::sha1_hex("/bios/3ds/biostest_nand.bin")).execute(pool).await.map_err(e)?;
        // retrobios lists an emulator's support files beside the BIOS dumps: those are not a BIOS.
        let retro = "<datafile><header><name>retrobios</name></header>\
            <game name=\"biostest_patches.zip\"><rom name=\"biostest_patches.zip\" size=\"8\" sha1=\"00000000000000000000000000000000000000d4\"/></game>\
            <game name=\"biostest_scph39001.bin\"><rom name=\"biostest_scph39001.bin\" size=\"8\" sha1=\"00000000000000000000000000000000000000d5\"/></game></datafile>";
        import::import_dat(pool, Source::Retrobios, "bios-retro.dat", &dat_parse::parse_dat_xml(retro).map_err(de)?, &ImportOptions::default()).await.map_err(de)?;
        let retro_kinds: Vec<(String, String)> = romexplorer::api::bios::list(&app, &romexplorer::api::bios::BiosQuery { search: Some("biostest_".into()), ..Default::default() }).await.map_err(ae)?.items.into_iter().map(|r| (r.name, r.kind)).collect();
        // The same BIOS filed under a console and again under MAME: listed once, the console's.
        let shared = "<datafile><header><name>retrobios</name></header>\
            <game name=\"biostestfds.nes\"><description>bios/Nintendo/Famicom Disk System/biostestfds.nes</description><rom name=\"biostestfds.nes\" size=\"8\" sha1=\"00000000000000000000000000000000000000d6\"/></game>\
            <game name=\"biostestfds.nes\"><description>bios/Arcade/MAME/biostestfds.nes</description><rom name=\"biostestfds.nes\" size=\"8\" sha1=\"00000000000000000000000000000000000000d6\"/></game></datafile>";
        import::import_dat(pool, Source::Retrobios, "bios-shared.dat", &dat_parse::parse_dat_xml(shared).map_err(de)?, &ImportOptions::default()).await.map_err(de)?;
        let shared_platforms: Vec<i64> = sqlx::query_scalar("SELECT CAST(app_platform_id AS SIGNED) FROM `release` WHERE name = 'biostestfds.nes' AND deleted = FALSE ORDER BY app_platform_id").fetch_all(pool).await.map_err(e)?;
        let shared_listed: Vec<Option<String>> = romexplorer::api::bios::list(&app, &romexplorer::api::bios::BiosQuery { search: Some("biostestfds".into()), ..Default::default() }).await.map_err(ae)?.items.into_iter().map(|r| r.platform).collect();

        // As an older import left one: a firmware entry with a title of its own.
        let src = import::ensure_source(pool, "tosec").await.map_err(de)?;
        let old = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, source_id, category) VALUES (0, 'Biostest Old Firmware (Japan)', ?, 'biostest old firmware', ?, ?, 'Firmware')")
            .bind(import::sha1_hex("Biostest Old Firmware (Japan)")).bind(import::sha1_hex("biostest old firmware")).bind(src).execute(pool).await.map_err(e)?.last_insert_id();
        let old_title = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES ('Biostest Old Firmware', 'biostest old firmware', ?)").bind(import::sha1_hex("biostest old firmware")).execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(old_title).bind(old).execute(pool).await.map_err(e)?;
        let marked = romexplorer::database::release_bases::mark_bios(pool).await.map_err(de)?;
        let after = kind_of("Biostest Old Firmware (Japan)").await.map_err(e)?;
        let old_title_gone: bool = sqlx::query_scalar("SELECT deleted FROM title WHERE id = ?").bind(old_title).fetch_one(pool).await.map_err(e)?;

        let bios = Some("bios".to_string());
        let checks = [
            ((boot.0.clone(), boot.1) == (bios.clone(), 0), format!("a [BIOS] entry is bios with no title: {boot:?}")),
            ((game.0.clone(), game.1) == (None, 1), format!("the game beside it is a game with its title: {game:?}")),
            ((ps.0.clone(), ps.1) == (bios.clone(), 0), format!("an entry of a BIOS-only DAT is bios: {ps:?}")),
            (titles == vec!["Biostest Game".to_string()], format!("only the game is a title: {titles:?}")),
            (listed.len() == 2 && listed.iter().any(|(n, p, k)| n.starts_with("[BIOS]") && p.as_deref() == Some("Game Boy Advance") && k == "bios") && listed.iter().any(|(n, p, _)| n.starts_with("Biostest PS") && p.as_deref() == Some("PlayStation")), format!("both are listed with their platform: {listed:?}")),
            (marked >= 1 && (after.0.clone(), after.1) == (bios, 0) && old_title_gone, format!("the title merge pass marks what an older import left and drops its title: {marked} {after:?} {old_title_gone}")),
            (mine.len() == 2 && mine.iter().any(|(n, f, h)| n.starts_with("[BIOS]") && f == "dat" && *h) && mine.iter().any(|(n, f, h)| n == "biostest_nand.bin" && f == "own" && *h), format!("my files: the held DAT entry and the file no DAT lists, not the unheld entry: {mine:?}")),
            (shared_platforms.len() == 2 && shared_platforms.contains(&52) && shared_listed.len() == 1 && shared_listed[0].as_deref().is_some_and(|p| p != "Arcade (MAME family)" && p != "Arcade"), format!("a BIOS filed under a console and MAME is listed once, as the console's: {shared_platforms:?} {shared_listed:?}")),
            (retro_kinds.contains(&("biostest_patches.zip".to_string(), "other".to_string())) && retro_kinds.contains(&("biostest_scph39001.bin".to_string(), "bios".to_string())), format!("a support archive is another system file, the dump a BIOS: {retro_kinds:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} listed, {marked} marked by the repair pass", listed.len())))
    })
    .await;
}

/// A newer DAT that changes or drops a file sets the old one aside (`import::supersede_unlisted`)
/// instead of deleting it; listing it again puts it back; a DAT cut short or leaving out too
/// many at once takes nothing off; another cut of the same system leaves this one's be.
async fn phase_dat_supersede(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "dat import: changed and dropped files superseded, not deleted", async {
        let de = |e: database::DatabaseError| e.to_string();
        let e = |e: sqlx::Error| e.to_string();
        let xml = |name: &str, games: &[(&str, Vec<(String, String)>)], cut: bool| {
            let mut s = format!("<datafile><header><name>{name}</name></header>");
            for (g, files) in games {
                s.push_str(&format!("<game name=\"{g}\">"));
                for (f, sha) in files {
                    s.push_str(&format!("<rom name=\"{f}\" size=\"8\" sha1=\"{sha:0>40}\"/>"));
                }
                s.push_str("</game>");
            }
            if !cut {
                s.push_str("</datafile>");
            }
            s
        };
        let tracks = |game: &str, n: usize, tag: &str| -> Vec<(String, String)> { (0..n).map(|i| (format!("{game} (Track {i}).bin"), format!("{tag}{i:02}"))).collect() };
        let import = |name: &'static str, games: Vec<(&'static str, Vec<(String, String)>)>, cut: bool| {
            let body = xml(name, &games, cut);
            async move { import::import_dat(pool, Source::NoIntro, &format!("supersede-{name}.dat"), &dat_parse::parse_dat_xml(&body).map_err(de)?, &ImportOptions::default()).await.map_err(de) }
        };
        let live = |release: &'static str| async move {
            sqlx::query_scalar::<_, String>("SELECT h.value_hex FROM release_contents rc JOIN `release` r ON r.id = rc.release_id JOIN hashes h ON h.content_id = rc.content_id AND h.algorithm = 'sha1' WHERE r.name = ? ORDER BY 1").bind(release).fetch_all(pool).await
        };
        let set_aside = |release: &'static str| async move {
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM release_contents_superseded x JOIN `release` r ON r.id = x.release_id WHERE r.name = ?").bind(release).fetch_one(pool).await
        };
        let one = |sha: &str| vec![("Supersede Alpha (World).gb".to_string(), sha.to_string())];
        let (alpha, beta) = ("Supersede Alpha (World)", "Supersede Beta (World)");

        import("Nintendo - Game Boy (20240101-000000)", vec![(alpha, one("5a1")), (beta, tracks(beta, 14, "5b"))], false).await?;
        let fixed = import("Nintendo - Game Boy (20240201-000000)", vec![(alpha, one("5a2")), (beta, tracks(beta, 13, "5b"))], false).await?;
        let alpha_fixed = live(alpha).await.map_err(e)?;
        let back = import("Nintendo - Game Boy (20240301-000000)", vec![(alpha, one("5a1")), (beta, tracks(beta, 13, "5b"))], false).await?;
        let alpha_back = live(alpha).await.map_err(e)?;
        let cut = import("Nintendo - Game Boy (20240401-000000)", vec![(alpha, one("5a1")), (beta, tracks(beta, 1, "5b"))], true).await?;
        let other_cut = import("Nintendo - Game Boy (Private) (20240401-000000)", vec![(beta, tracks(beta, 1, "5c"))], false).await?;
        let too_many = import("Nintendo - Game Boy (20240501-000000)", vec![(alpha, one("5a1")), (beta, tracks(beta, 1, "5b"))], false).await?;
        let beta_after = live(beta).await.map_err(e)?.len();
        let beta_aside = set_aside(beta).await.map_err(e)?;
        let checks = [
            (fixed.content_superseded == 2 && alpha_fixed.len() == 1 && alpha_fixed[0].ends_with("5a2"), format!("a fixed hash and a dropped track set aside: {fixed:?} {alpha_fixed:?}")),
            (back.content_restored == 1 && back.content_superseded == 1 && alpha_back.len() == 1 && alpha_back[0].ends_with("5a1"), format!("the old dump listed again is back: {back:?} {alpha_back:?}")),
            (cut.supersede_held.is_some() && cut.content_superseded == 0, format!("a DAT cut short takes nothing off: {cut:?}")),
            (other_cut.content_superseded == 0, format!("another cut of the system leaves this one's files: {other_cut:?}")),
            (too_many.supersede_held.is_some() && too_many.content_superseded == 0 && beta_after == 14 && beta_aside == 1, format!("12 of 14 at once is too many: {too_many:?}, {beta_after} live, {beta_aside} aside")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), "fixed, dropped, restored, and the two guards held".to_string()))
    })
    .await;
}

/// A title's genres are its leading genre source's (`metadata_source_priority`): one IGDB
/// calls a puzzle game is not listed under role-playing because MobyGames says so, until
/// MobyGames is put first.
async fn phase_genre_source(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "genre filter: the title's leading genre source only", async {
        use romexplorer::api::catalog::{self, ListQuery};
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let ae = |e: romexplorer::api::Error| e.message;
        let app = romexplorer::api::App::with_pool(pool.clone(), env.paths.clone(), DatabaseConfig::default()).map_err(ae)?;
        let src = import::ensure_source(pool, "no-intro").await.map_err(de)?;
        let title = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES ('Genresource Game', 'genresource game', ?)").bind(import::sha1_hex("genresource game")).execute(pool).await.map_err(e)?.last_insert_id();
        let release = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (24, 'Genresource Game (USA)', ?, 'genresource game', ?, 'US', ?)")
            .bind(import::sha1_hex("Genresource Game (USA)")).bind(import::sha1_hex("genresource game")).bind(src).execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(title).bind(release).execute(pool).await.map_err(e)?;
        let puzzle = sqlx::query("INSERT INTO genre (name) VALUES ('Genresource Puzzle')").execute(pool).await.map_err(e)?.last_insert_id();
        let rpg = sqlx::query("INSERT INTO genre (name) VALUES ('Genresource RPG')").execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO title_genres (title_id, genre_id, source) VALUES (?, ?, 'igdb'), (?, ?, 'mobygames')").bind(title).bind(puzzle).bind(title).bind(rpg).execute(pool).await.map_err(e)?;
        let found = |major: &'static str| {
            let app = &app;
            async move {
                let by_major = catalog::titles(app, &ListQuery { search: Some("Genresource".into()), genre_major: Some(major.into()), view: Some("collection".into()), ..Default::default() }).await?.total;
                let by_genre = catalog::titles(app, &ListQuery { search: Some("Genresource".into()), genres: Some(major.into()), view: Some("collection".into()), ..Default::default() }).await?.total;
                Ok::<(u64, u64), romexplorer::api::Error>((by_major, by_genre))
            }
        };
        let igdb_first = (found("Genresource Puzzle").await.map_err(ae)?, found("Genresource RPG").await.map_err(ae)?);
        database::settings::set(pool, database::settings::KEY_METADATA_SOURCE_PRIORITY, &serde_json::json!({ "genre": ["mobygames", "igdb"] })).await.map_err(de)?;
        let moby_first = (found("Genresource Puzzle").await.map_err(ae)?, found("Genresource RPG").await.map_err(ae)?);
        database::settings::delete(pool, database::settings::KEY_METADATA_SOURCE_PRIORITY).await.map_err(de)?;
        let checks = [
            (igdb_first == ((1, 1), (0, 0)), format!("by default IGDB's genres: puzzle, not role-playing: {igdb_first:?}")),
            (moby_first == ((0, 0), (1, 1)), format!("MobyGames put first: role-playing, not puzzle: {moby_first:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{igdb_first:?} then {moby_first:?}")))
    })
    .await;
}

/// RetroAchievements games into a set: the releases you hold of a game, else its top
/// linked release, else its title's top release; a game with nothing adds nothing.
async fn phase_ra_into_sets(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "ra games into a set: held releases, else the top linked one, else the title's", async {
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let ae = |e: romexplorer::api::Error| e.message;
        let app = romexplorer::api::App::with_pool(pool.clone(), env.paths.clone(), DatabaseConfig::default()).map_err(ae)?;
        let src = import::ensure_source(pool, "no-intro").await.map_err(de)?;
        let release = |name: &'static str| async move {
            sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (24, ?, ?, ?, ?, 'US', ?)")
                .bind(name).bind(import::sha1_hex(name)).bind(name.to_lowercase()).bind(import::sha1_hex(&name.to_lowercase())).bind(src)
                .execute(pool).await.map(|r| r.last_insert_id())
        };
        let (usa, europe, other) = (release("Ra Set Game (USA)").await.map_err(e)?, release("Ra Set Game (Europe)").await.map_err(e)?, release("Ra Set Other (USA)").await.map_err(e)?);
        sqlx::query("INSERT INTO release_provider_links (release_id, provider, provider_id, confidence) VALUES (?, 'retroachievements', '990001', 'exact'), (?, 'retroachievements', '990001', 'exact')").bind(usa).bind(europe).execute(pool).await.map_err(e)?;
        let title = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES ('Ra Set Other', 'ra set other', ?)").bind(import::sha1_hex("ra set other")).execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(title).bind(other).execute(pool).await.map_err(e)?;
        sqlx::query("INSERT INTO title_provider_links (title_id, provider, provider_id, confidence) VALUES (?, 'retroachievements', '990002', 'manual')").bind(title).execute(pool).await.map_err(e)?;
        let none_held = romexplorer::api::sets::ra_game_releases(&app, &[990001]).await.map_err(ae)?;
        let game = sqlx::query("INSERT INTO local_game (name) VALUES ('ra set game europe')").execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO local_release_games (release_id, game_id) VALUES (?, ?)").bind(europe).bind(game).execute(pool).await.map_err(e)?;
        let held = romexplorer::api::sets::ra_game_releases(&app, &[990001]).await.map_err(ae)?;
        // Both regions held: still one release, the better-ranked (USA, by region priority) --
        // not every region of the game. Then USA is let go again for the steps below.
        let usa_game = sqlx::query("INSERT INTO local_game (name) VALUES ('ra set game usa')").execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO local_release_games (release_id, game_id) VALUES (?, ?)").bind(usa).bind(usa_game).execute(pool).await.map_err(e)?;
        let both_held = romexplorer::api::sets::ra_game_releases(&app, &[990001]).await.map_err(ae)?;
        sqlx::query("UPDATE local_game SET deleted = TRUE WHERE id = ?").bind(usa_game).execute(pool).await.map_err(e)?;
        let by_title = romexplorer::api::sets::ra_game_releases(&app, &[990002, 990999]).await.map_err(ae)?;
        // A set's list filters like Releases, and what a filter picked out leaves at once.
        let set = romexplorer::api::sets::create(&app, &romexplorer::api::sets::SetSpec { name: "Ra Set Filter".into(), description: None }).await.map_err(ae)?;
        let snes = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (19, 'Ra Set Snes (Japan)', ?, 'ra set snes', ?, 'JP', ?)")
            .bind(import::sha1_hex("Ra Set Snes (Japan)")).bind(import::sha1_hex("ra set snes")).bind(src).execute(pool).await.map_err(e)?.last_insert_id();
        romexplorer::api::sets::add_releases(&app, set.id, &[usa, europe, snes]).await.map_err(ae)?;
        // What the set takes on disk: the files of the copy a copy job would take, at their own
        // size. Europe is held twice -- an ISO and, put first by hand, a CHD -- and counts as
        // the CHD; the DATs' 1000 bytes for the USA release, which is not held, count nothing.
        let c = sqlx::query("INSERT INTO content (file_name, size) VALUES ('set-total.bin', 1000)").execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO release_contents (release_id, content_id, file_name) VALUES (?, ?, 'set-total.bin')").bind(usa).bind(c).execute(pool).await.map_err(e)?;
        let chd_game = sqlx::query("INSERT INTO local_game (name) VALUES ('ra set game europe chd')").execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO local_release_games (release_id, game_id) VALUES (?, ?)").bind(europe).bind(chd_game).execute(pool).await.map_err(e)?;
        for (g, path, size) in [(game, "/settotal/Ra Set Game (Europe).iso", 700u64), (chd_game, "/settotal/chd/Ra Set Game (Europe).chd", 300)] {
            let file = sqlx::query("INSERT INTO local_files (path, path_hash, content_id, size) VALUES (?, ?, ?, ?)").bind(path).bind(import::sha1_hex(path)).bind(c).bind(size).execute(pool).await.map_err(e)?.last_insert_id();
            let rom = sqlx::query("INSERT INTO local_rom (main_path) VALUES (?)").bind(path).execute(pool).await.map_err(e)?.last_insert_id();
            sqlx::query("INSERT INTO local_rom_files (rom_id, file_id) VALUES (?, ?)").bind(rom).bind(file).execute(pool).await.map_err(e)?;
            sqlx::query("INSERT INTO local_game_roms (game_id, rom_id) VALUES (?, ?)").bind(g).bind(rom).execute(pool).await.map_err(e)?;
        }
        sqlx::query("INSERT INTO local_game_order (release_id, game_id, position) VALUES (?, ?, 0), (?, ?, 1)").bind(europe).bind(chd_game).bind(europe).bind(game).execute(pool).await.map_err(e)?;
        let totals = romexplorer::api::sets::get(&app, set.id).await.map_err(ae)?;
        // Each release's two sizes: on disk every copy's files (the ISO and the CHD), by the
        // DATs the raw dump -- a CHD entry is no raw size.
        let chd = sqlx::query("INSERT INTO content (file_name, size) VALUES ('set-total.chd', 400)").execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO release_contents (release_id, content_id, file_name) VALUES (?, ?, 'set-total.chd')").bind(snes).bind(chd).execute(pool).await.map_err(e)?;
        let sizes: std::collections::HashMap<u64, (Option<u64>, Option<u64>)> = romexplorer::api::sets::releases(&app, set.id, &romexplorer::api::catalog::ListQuery { view: Some("collection".into()), ..Default::default() })
            .await.map_err(ae)?.items.into_iter().map(|r| (r.id, (r.local_size, r.catalog_size))).collect();
        let by_size: Vec<u64> = romexplorer::api::sets::releases(&app, set.id, &romexplorer::api::catalog::ListQuery { view: Some("collection".into()), sort: Some("local_size".into()), desc: true, ..Default::default() })
            .await.map_err(ae)?.items.into_iter().map(|r| r.id).collect();
        let want_titles: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(DISTINCT tr.title_id) FROM title_releases tr JOIN title t ON t.id = tr.title_id AND t.deleted = FALSE WHERE tr.release_id IN ({usa}, {europe}, {snes})"))).fetch_one(pool).await.map_err(e)?;
        let on_gba: Vec<u64> = romexplorer::api::sets::releases(&app, set.id, &romexplorer::api::catalog::ListQuery { platform: Some(24), view: Some("collection".into()), ..Default::default() }).await.map_err(ae)?.items.into_iter().map(|r| r.id).collect();
        let in_japan: Vec<u64> = romexplorer::api::sets::releases(&app, set.id, &romexplorer::api::catalog::ListQuery { region: Some("JP".into()), view: Some("collection".into()), ..Default::default() }).await.map_err(ae)?.items.into_iter().map(|r| r.id).collect();
        let removed = romexplorer::api::sets::remove_releases(&app, set.id, &on_gba).await.map_err(ae)?;
        let left: Vec<u64> = romexplorer::api::sets::releases(&app, set.id, &romexplorer::api::catalog::ListQuery { view: Some("collection".into()), ..Default::default() }).await.map_err(ae)?.items.into_iter().map(|r| r.id).collect();
        let checks = [
            (none_held.len() == 1 && (none_held[0] == usa || none_held[0] == europe), format!("nothing held: one release, the top linked one: {none_held:?}")),
            (held == vec![europe], format!("the release you hold, not the top one: {held:?} (europe {europe})")),
            (both_held == vec![usa], format!("both held: one release, the better-ranked, not every region: {both_held:?} (usa {usa})")),
            (by_title == vec![other], format!("linked only by title: the title's release; an unknown game adds nothing: {by_title:?}")),
            (on_gba.len() == 2 && !on_gba.contains(&snes) && in_japan == vec![snes], format!("a set filters by platform and region: {on_gba:?} / {in_japan:?}")),
            ((totals.release_count, totals.platform_count, totals.total_size, totals.size_unknown, totals.title_count as i64) == (3, 2, 300, 2, want_titles), format!("set totals: {totals:?} (titles want {want_titles})")),
            (removed == 2 && left == vec![snes], format!("the filtered selection leaves the set at once: {removed} removed, {left:?} left")),
            (sizes.get(&europe) == Some(&(Some(1000), None)) && sizes.get(&usa) == Some(&(None, Some(1000))) && sizes.get(&snes) == Some(&(None, None)), format!("release sizes: on disk both copies, by the DATs the raw dump only: {sizes:?}")),
            (by_size.first() == Some(&europe), format!("sorted by size on disk, largest first: {by_size:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{held:?} / {by_title:?}")))
    })
    .await;
}

async fn phase_patched_import(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "patched import: one release per work, language and kind recorded", async {
        use romexplorer::database::dat_convert::{self, Kind};
        use romexplorer::database::dat_parse::{self, ParsedFile, ParsedGame};
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let rom = |name: &str, sha1: &str| ParsedFile { name: name.to_string(), size: Some(8), sha1: Some(format!("{sha1:0>40}")), ..Default::default() };
        let machine = |name: &str, files: Vec<ParsedFile>| ParsedGame { name: name.to_string(), files, ..Default::default() };
        let system = "Nintendo - Game Boy Advance";
        let gba = Platform::GameBoyAdvance.igdb_id();

        // The original, as a commercial DAT lists it.
        let original = "<datafile><header><name>Nintendo - Game Boy Advance</name></header>\
            <game name=\"Patchwork Test (Japan)\"><rom name=\"Patchwork Test (Japan).gba\" size=\"8\" sha1=\"00000000000000000000000000000000000000a0\"/></game></datafile>";
        import::import_dat(pool, Source::NoIntro, "patched-original.dat", &dat_parse::parse_dat_xml(original).map_err(de)?, &ImportOptions::default()).await.map_err(de)?;

        // A collector's DATs, converted.
        let mut works = Vec::new();
        for (kind, m) in [
            (Kind::Translation, machine("Patchwork Test (Japan) [T-En by Someone v1.0]", vec![rom("Patchwork Test (Japan) [T-En by Someone v1.0].gba", "b1")])),
            (Kind::Translation, machine("Patchwork Test (Japan) [T-En by Someone v1.02] [i]", vec![rom("Patchwork Test (Japan) [T-En by Someone v1.02] [i].gba", "b2")])),
            (Kind::Hack, machine("Hacks/Patchwork Test", vec![rom("Patchwork Test - Hard Mode (V2) (Hack).gba", "c1"), rom("Patchwork Test - Hard Mode (V3) (Hack).gba", "c2"), rom("Patchwork Test - Colours (Hack).gba", "c3")])),
            (Kind::Hack, machine("Translations/Brazil/Patchwork Test", vec![rom("Patchwork Test (J) (Portuguese patch).gba", "d1")])),
            (Kind::Hack, machine("Trainers/Patchwork Test", vec![rom("Patchwork Test (J) [t1].gba", "e1")])),
            (Kind::Hack, machine("Dashboards/Skins/Patchwork Skin", vec![rom("skin.xml", "f1"), rom("back.png", "f2")])),
        ] {
            works.extend(dat_convert::works_of(system, kind, &m).map_err(|folder| format!("unknown folder {folder}"))?);
        }
        // As `convert_folders` does: one entry per work.
        let mut by_file: std::collections::BTreeMap<&str, Vec<dat_convert::Work>> = Default::default();
        for work in works {
            let entries = by_file.entry(work.kind.file()).or_default();
            match entries.iter_mut().find(|w| w.name.eq_ignore_ascii_case(&work.name)) {
                Some(existing) => existing.absorb(work),
                None => entries.push(work),
            }
        }
        for (file, entries) in by_file.iter_mut() {
            entries.iter_mut().for_each(dat_convert::settle_variants);
            let source = Source::from_name(match *file { "hacks" => "hacks", "homebrew" => "homebrew", _ => "etc" }).unwrap();
            let dat = dat_parse::parse_dat_xml(&dat_convert::render(file, entries)).map_err(de)?;
            import::import_dat(pool, source, &format!("patched-{file}.dat"), &dat, &ImportOptions::default()).await.map_err(de)?;
        }
        // RetroAchievements' own name for one more version of the translation.
        let ra = "<datafile><header><name>RA - Game Boy Advance</name></header>\
            <game name=\"Patchwork Test (Japan) [T-En by Someone v1.1]\"><category>Hack</category><rom name=\"x.gba\" size=\"8\" sha1=\"00000000000000000000000000000000000000b3\"/></game></datafile>";
        import::import_dat(pool, Source::RetroAchievements, "patched-ra.dat", &dat_parse::parse_dat_xml(ra).map_err(de)?, &ImportOptions::default()).await.map_err(de)?;

        type Rel = (u64, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, bool, Option<String>, Option<String>);
        let rel = |name: &'static str| async move {
            sqlx::query_as::<_, Rel>("SELECT id, region, languages, languages_state, game_kind, patch_type, community_patched, material, base_hint FROM `release` WHERE name = ? AND app_platform_id = ?").bind(name).bind(gba).fetch_optional(pool).await
        };
        let title_of = |release: u64| async move { sqlx::query_scalar::<_, u64>("SELECT title_id FROM title_releases WHERE release_id = ?").bind(release).fetch_optional(pool).await };
        let variants = |release: u64| async move { sqlx::query_scalar::<_, Option<String>>("SELECT variant FROM release_contents WHERE release_id = ? ORDER BY variant").bind(release).fetch_all(pool).await };

        let original_id: u64 = sqlx::query_scalar("SELECT id FROM `release` WHERE name = 'Patchwork Test (Japan)' AND app_platform_id = ?").bind(gba).fetch_one(pool).await.map_err(e)?;
        let home = title_of(original_id).await.map_err(e)?;
        let translation = rel("Patchwork Test (Japan) [T-En by Someone]").await.map_err(e)?.ok_or("the translation is not one release named for the work")?;
        let versions = variants(translation.0).await.map_err(e)?;
        let hard = rel("Patchwork Test - Hard Mode (Hack)").await.map_err(e)?.ok_or("the hack's versions are not one release")?;
        let hard_versions = variants(hard.0).await.map_err(e)?;
        let colours = rel("Patchwork Test - Colours (Hack)").await.map_err(e)?.ok_or("the other hack is missing")?;
        let guessed = rel("Patchwork Test (J) (Portuguese patch)").await.map_err(e)?.ok_or("the folder-only translation is missing")?;
        let trainer = rel("Patchwork Test (J) [t1]").await.map_err(e)?.ok_or("the trainer is missing")?;
        let tool = rel("Skins - Patchwork Skin").await.map_err(e)?.ok_or("the tool is missing")?;
        let stray: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM `release` WHERE name LIKE 'Patchwork Test (Japan) [T-En by Someone v%'").fetch_one(pool).await.map_err(e)?;
        let s = |v: &str| Some(v.to_string());
        let checks = [
            (
                (translation.1.as_deref(), translation.2.as_deref(), translation.3.as_deref(), translation.4.as_deref(), translation.5.as_deref(), translation.6) == (Some("JP"), Some("en"), Some("confirmed"), Some("translation"), Some("translation"), true),
                format!("a translation is the original's region and the translation's language, and a translation: {translation:?}"),
            ),
            (versions == vec![s("v1.0"), s("v1.02 [i]"), s("v1.1")] && stray == 0, format!("three versions, RetroAchievements' included, are one release: {versions:?}, {stray} stray")),
            (title_of(translation.0).await.map_err(e)? == home && home.is_some(), "the translation is under the original's title".to_string()),
            (translation.8.as_deref() == Some("Patchwork Test (Japan)"), format!("the original is named: {:?}", translation.8)),
            (hard_versions == vec![s("V2"), s("V3")] && hard.4.as_deref() == Some("hack") && hard.8.as_deref() == Some("Patchwork Test"), format!("two versions of a hack are one release: {hard_versions:?} {hard:?}")),
            (title_of(hard.0).await.map_err(e)? == home && title_of(colours.0).await.map_err(e)? == home, "hacks are under the game their folder names".to_string()),
            ((guessed.2.as_deref(), guessed.3.as_deref(), guessed.4.as_deref(), guessed.1.as_deref()) == (Some("pt"), Some("guessed"), Some("translation"), Some("JP")), format!("a language only the folder gives is a guess: {guessed:?}")),
            (trainer.7.as_deref() == Some("trainer") && trainer.4.is_none(), format!("a trainer is not a game: {trainer:?}")),
            (tool.7.as_deref() == Some("tool") && title_of(tool.0).await.map_err(e)?.is_none(), format!("a tool has no title: {tool:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }

        // Through the lists: kinds are ticked, any of which may match; what is not a game is
        // under Etc; a patch file is under Patches.
        use romexplorer::api::catalog::{self, ListQuery};
        use romexplorer::api::patches::{self, PatchQuery};
        let ae = |e: romexplorer::api::Error| e.message;
        let app = romexplorer::api::App::with_pool(pool.clone(), env.paths.clone(), DatabaseConfig::default()).map_err(ae)?;
        let patch_dat = "<datafile><header><name>hacks</name></header>\
            <game name=\"Patchwork Test - Palette Fix (Hack)\" cloneof=\"Patchwork Test\"><system>Nintendo - Game Boy Advance</system><category>Hack</category><rom name=\"colours.ips\" size=\"8\" sha1=\"00000000000000000000000000000000000000c9\"/></game></datafile>";
        import::import_dat(pool, Source::from_name("hacks").unwrap(), "patched-patch.dat", &dat_parse::parse_dat_xml(patch_dat).map_err(de)?, &ImportOptions::default()).await.map_err(de)?;
        let named = |kinds: Option<&'static str>, material: Option<&'static str>| {
            let app = &app;
            async move {
                let q = ListQuery { search: Some("Patchwork".into()), kinds: kinds.map(str::to_string), material: material.map(str::to_string), limit: Some(100), view: Some("collection".into()), ..Default::default() };
                catalog::releases(app, &q).await.map(|p| {
                    let mut names: Vec<(String, Option<String>, Option<String>)> = p.items.into_iter().map(|r| (r.name, r.game_kind, r.languages_state)).collect();
                    names.sort();
                    names
                })
            }
        };
        let only = |rows: &[(String, Option<String>, Option<String>)]| rows.iter().map(|(n, _, _)| n.as_str().to_string()).collect::<Vec<_>>();
        let commercial = named(Some("commercial"), None).await.map_err(ae)?;
        let translations = named(Some("translation"), None).await.map_err(ae)?;
        let either = named(Some("commercial,translation"), None).await.map_err(ae)?;
        let hacks = named(Some("hack"), None).await.map_err(ae)?;
        let guessed_listed = named(Some("guessed"), None).await.map_err(ae)?;
        let etc = named(None, Some("etc")).await.map_err(ae)?;
        let trainers = named(None, Some("trainer")).await.map_err(ae)?;
        let listed = patches::list(&app, &PatchQuery { from: Some("dat".into()), search: Some("Patchwork".into()), ..Default::default() }).await.map_err(ae)?;
        let all_patches = patches::list(&app, &PatchQuery { search: Some("Patchwork".into()), ..Default::default() }).await.map_err(ae)?;
        let s = |v: &str| v.to_string();
        let checks = [
            (only(&commercial) == vec![s("Patchwork Test (Japan)")], format!("commercial alone is the original: {commercial:?}")),
            (only(&translations) == vec![s("Patchwork Test (J) (Portuguese patch)"), s("Patchwork Test (Japan) [T-En by Someone]")] && translations.iter().all(|(_, k, _)| k.as_deref() == Some("translation")), format!("translations are badged translations: {translations:?}")),
            (either.len() == commercial.len() + translations.len(), format!("two ticks are either of them: {either:?}")),
            (only(&hacks) == vec![s("Patchwork Test - Colours (Hack)"), s("Patchwork Test - Hard Mode (Hack)")], format!("hacks, and not the patch file or the trainer: {hacks:?}")),
            (only(&guessed_listed) == vec![s("Patchwork Test (J) (Portuguese patch)")], format!("the guessed language can be asked for: {guessed_listed:?}")),
            (only(&etc) == vec![s("Patchwork Test (J) [t1]"), s("Skins - Patchwork Skin")] && only(&trainers) == vec![s("Patchwork Test (J) [t1]")], format!("Etc is the trainer and the tool: {etc:?} {trainers:?}")),
            (
                listed.total == 1 && listed.items[0].name == "Patchwork Test - Palette Fix (Hack)" && listed.items[0].from == "dat" && listed.items[0].target.as_deref() == Some("Patchwork Test") && !listed.items[0].held && all_patches.total == 1,
                format!("a DAT's patch file is under Patches: {:?}", listed.items),
            ),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }

        // What each is made from: certain for the translation, whose name is the original's
        // plus a tag; a suggestion for what only a folder placed. Then what the user says.
        use romexplorer::api::bases;
        use romexplorer::database::release_bases;
        let resolved = release_bases::resolve(pool, &jobs::Progress::none()).await.map_err(de)?;
        let again = release_bases::resolve(pool, &jobs::Progress::none()).await.map_err(de)?;
        let link = |id: u64| async move { bases::of_release(pool, id).await.map(|l| l.map(|l| (l.status, l.title_id, l.release_id))) };
        let translation_link = link(translation.0).await.map_err(ae)?;
        let hard_link = link(hard.0).await.map_err(ae)?;
        let trainer_link = link(trainer.0).await.map_err(ae)?;
        let original_link = link(original_id).await.map_err(ae)?;
        let on_original: Vec<u64> = catalog::release(&app, original_id).await.map_err(ae)?.patched.iter().map(|p| p.id).collect();
        let suggestions = bases::list(&app, &bases::LinkQuery { search: Some("Patchwork".into()), ..Default::default() }).await.map_err(ae)?;
        let sg = |v: &str| Some((v.to_string(), home, Some(original_id)));
        let checks = [
            (translation_link == sg("exact"), format!("the translation is of the release its name is made from: {translation_link:?}")),
            (hard_link == sg("suggested") && trainer_link == sg("suggested"), format!("what a folder placed is a suggestion, with the game's release: {hard_link:?} {trainer_link:?}")),
            (original_link.is_none(), format!("an original is made from nothing: {original_link:?}")),
            (on_original.contains(&translation.0) && on_original.contains(&hard.0) && on_original.contains(&guessed.0), format!("the original lists what is made from it: {on_original:?}")),
            (suggestions.total >= 4 && suggestions.items.iter().all(|s| s.base.status == "suggested") && !suggestions.items.iter().any(|s| s.release.id == translation.0), format!("the suggestions are the uncertain ones: {}", suggestions.total)),
            (resolved.exact == 1 && resolved.suggested >= 4 && (again.exact, again.igdb, again.suggested, again.moved) == (0, 0, 0, 0), format!("a second pass finds nothing new: {resolved:?} then {again:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        // Confirm one, unlink another, set a third by hand; a later pass leaves all three.
        bases::set(&app, hard.0, &bases::BaseRequest { action: "confirm".into(), title_id: None, release_id: None }).await.map_err(ae)?;
        bases::bulk(&app, &bases::BulkLinks { action: "reject".into(), ids: vec![colours.0], all: false }).await.map_err(ae)?;
        bases::set(&app, trainer.0, &bases::BaseRequest { action: "set".into(), title_id: None, release_id: Some(original_id) }).await.map_err(ae)?;
        release_bases::resolve(pool, &jobs::Progress::none()).await.map_err(de)?;
        let colours_title = title_of(colours.0).await.map_err(e)?;
        let colours_detail = catalog::release(&app, colours.0).await.map_err(ae)?;
        let after = (link(hard.0).await.map_err(ae)?, link(colours.0).await.map_err(ae)?.map(|l| l.0), link(trainer.0).await.map_err(ae)?);
        // The guessed language: confirmed, changed, cleared -- and an import leaves each alone.
        let state_of = |id: u64| async move { sqlx::query_as::<_, (Option<String>, Option<String>)>("SELECT languages, languages_state FROM `release` WHERE id = ?").bind(id).fetch_one(pool).await };
        let say = |action: &'static str, languages: Option<&'static str>| {
            let app = &app;
            async move { bases::language(app, guessed.0, &bases::LanguageRequest { action: action.into(), languages: languages.map(str::to_string) }).await }
        };
        say("confirm", None).await.map_err(ae)?;
        let confirmed = state_of(guessed.0).await.map_err(e)?;
        say("set", Some("ES")).await.map_err(ae)?;
        let changed = state_of(guessed.0).await.map_err(e)?;
        say("clear", None).await.map_err(ae)?;
        let etc_dat = dat_parse::parse_dat_xml(&dat_convert::render("hacks", by_file.get("hacks").map(Vec::as_slice).unwrap_or_default())).map_err(de)?;
        import::import_dat(pool, Source::from_name("hacks").unwrap(), "patched-hacks.dat", &etc_dat, &ImportOptions::default()).await.map_err(de)?;
        let cleared = state_of(guessed.0).await.map_err(e)?;
        let bad = say("set", Some("not a code!")).await.is_err();
        let s = |v: &str| Some(v.to_string());
        let checks = [
            (after.0 == Some(("manual".to_string(), home, Some(original_id))) && after.2 == Some(("manual".to_string(), home, Some(original_id))), format!("confirmed and set by hand are the user's: {:?} {:?}", after.0, after.2)),
            (after.1.as_deref() == Some("rejected") && colours_title.is_some() && colours_title != home && colours_detail.base.is_none(), format!("unlinked is out of the game's title and stays out: {:?} title {colours_title:?}", after.1)),
            (confirmed == (s("pt"), s("manual")) && changed == (s("es"), s("manual")) && cleared == (None, s("manual")) && bad, format!("the language is the user's once said: {confirmed:?} {changed:?} {cleared:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }

        // What an import no longer makes, but an older catalogue holds: a version of a work
        // as a release of its own, and the same ROM under another DAT's name.
        let stray = "<datafile><header><name>hacks</name></header>\
            <game name=\"Foldwork Zeta (Hack)\"><system>Nintendo - Game Boy Advance</system><category>Hack</category><rom name=\"zeta.gba\" size=\"8\" sha1=\"00000000000000000000000000000000000000d7\"/></game>\
            <game name=\"Colours Hack As Another DAT Calls It (Hack)\"><system>Nintendo - Game Boy Advance</system><category>Hack</category><rom name=\"other name.gba\" size=\"8\" sha1=\"00000000000000000000000000000000000000c3\"/></game></datafile>";
        import::import_dat(pool, Source::from_name("hacks").unwrap(), "patched-stray.dat", &dat_parse::parse_dat_xml(stray).map_err(de)?, &ImportOptions::default()).await.map_err(de)?;
        sqlx::query("UPDATE `release` SET name = 'Patchwork Test - Hard Mode (V9) (Hack)', name_hash = SHA1('Patchwork Test - Hard Mode (V9) (Hack)') WHERE name = 'Foldwork Zeta (Hack)' AND app_platform_id = ?").bind(gba).execute(pool).await.map_err(e)?;
        let folded = romexplorer::database::release_merge::fold_patched_works(pool, &jobs::Progress::none()).await.map_err(de)?;
        let refolded = romexplorer::database::release_merge::fold_patched_works(pool, &jobs::Progress::none()).await.map_err(de)?;
        let hard_now = variants(hard.0).await.map_err(e)?;
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM `release` WHERE name IN ('Patchwork Test - Hard Mode (V9) (Hack)', 'Colours Hack As Another DAT Calls It (Hack)')").fetch_one(pool).await.map_err(e)?;
        let kept = (link(hard.0).await.map_err(ae)?.map(|l| l.0), link(colours.0).await.map_err(ae)?.map(|l| l.0));
        let s = |v: &str| Some(v.to_string());
        let checks = [
            ((folded.by_name, folded.by_content) == (1, 1) && left == 0, format!("a version and a second name fold into their work: {folded:?}, {left} left")),
            (hard_now == vec![s("V2"), s("V3"), s("V9")], format!("the folded version is a build of the work: {hard_now:?}")),
            ((refolded.by_name, refolded.by_content) == (0, 0), format!("nothing folds twice: {refolded:?}")),
            (kept == (s("manual"), s("rejected")), format!("what the user said of a work stays said: {kept:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }

        // A hack collection holds ROMs named exactly as the game they are of. The game as
        // it was sold stays that, whichever DAT is imported first.
        let named_as_game = "<datafile><header><name>hacks</name></header>\
            <game name=\"Patchwork Test (Japan)\" cloneof=\"Patchwork Test\"><system>Nintendo - Game Boy Advance</system><category>Hack</category><rom name=\"Patchwork Test (Japan).gba\" size=\"8\" sha1=\"00000000000000000000000000000000000000e8\"/></game>\
            <game name=\"Patchwork Solo (USA)\" cloneof=\"Patchwork Solo\"><system>Nintendo - Game Boy Advance</system><category>Hack</category><rom name=\"Patchwork Solo (USA).gba\" size=\"8\" sha1=\"00000000000000000000000000000000000000e9\"/></game></datafile>";
        import::import_dat(pool, Source::from_name("hacks").unwrap(), "patched-named-as-game.dat", &dat_parse::parse_dat_xml(named_as_game).map_err(de)?, &ImportOptions::default()).await.map_err(de)?;
        let kind_of = |name: &'static str| async move { sqlx::query_as::<_, (Option<String>, bool, Option<String>)>("SELECT game_kind, community_patched, patch_type FROM `release` WHERE name = ? AND app_platform_id = ?").bind(name).bind(gba).fetch_one(pool).await };
        let listed_first = kind_of("Patchwork Test (Japan)").await.map_err(e)?;
        let hack_first = kind_of("Patchwork Solo (USA)").await.map_err(e)?;
        let later = "<datafile><header><name>Nintendo - Game Boy Advance</name></header>\
            <game name=\"Patchwork Solo (USA)\"><rom name=\"Patchwork Solo (USA).gba\" size=\"8\" sha1=\"00000000000000000000000000000000000000ea\"/></game></datafile>";
        import::import_dat(pool, Source::NoIntro, "patched-later.dat", &dat_parse::parse_dat_xml(later).map_err(de)?, &ImportOptions::default()).await.map_err(de)?;
        let then_listed = kind_of("Patchwork Solo (USA)").await.map_err(e)?;
        let checks = [
            (listed_first == (None, false, None), format!("a hack named as a listed game does not make the game a hack: {listed_first:?}")),
            (hack_first.0.as_deref() == Some("hack") && then_listed == (None, false, None), format!("and a game listed after a hack of its name is the game: {hack_first:?} then {then_listed:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }

        // The original kept beside the patch: its hash says which ROM the patch is of, and
        // it is not a version of the patch. An entry that is nothing but the original's
        // bytes is linked the same way and keeps its file.
        let with_original = "<datafile><header><name>etc</name></header>\
            <game name=\"Patchwork Test (NTSC-J) (+2 Trainer)\" cloneof=\"Patchwork Test\"><system>Nintendo - Game Boy Advance</system><category>Trainer</category>\
              <rom name=\"Patchwork Test (Japan).gba\" size=\"8\" sha1=\"00000000000000000000000000000000000000a0\"/><rom name=\"plus2.ppf\" size=\"8\" sha1=\"00000000000000000000000000000000000000f7\"/></game>\
            <game name=\"Patchwork Test Retitled (Hack)\" cloneof=\"Patchwork Test\"><system>Nintendo - Game Boy Advance</system><category>Hack</category>\
              <rom name=\"Patchwork Test Retitled.gba\" size=\"8\" sha1=\"00000000000000000000000000000000000000a0\"/></game></datafile>";
        let with_original = dat_parse::parse_dat_xml(with_original).map_err(de)?;
        import::import_dat(pool, Source::from_name("etc").unwrap(), "patched-with-original.dat", &with_original, &ImportOptions::default()).await.map_err(de)?;
        let id_of = |name: &'static str| async move { sqlx::query_scalar::<_, u64>("SELECT id FROM `release` WHERE name = ? AND app_platform_id = ?").bind(name).bind(gba).fetch_one(pool).await };
        let files_of = |release: u64| async move { sqlx::query_scalar::<_, String>("SELECT rc.file_name FROM release_contents rc WHERE rc.release_id = ? ORDER BY rc.file_name").bind(release).fetch_all(pool).await };
        let (plus2, retitled) = (id_of("Patchwork Test (NTSC-J) (+2 Trainer)").await.map_err(e)?, id_of("Patchwork Test Retitled (Hack)").await.map_err(e)?);
        // Already right when the import returns: no pass has run since.
        let at_import = (link(plus2).await.map_err(ae)?, files_of(plus2).await.map_err(e)?, link(retitled).await.map_err(ae)?, files_of(retitled).await.map_err(e)?);
        let originals_files = files_of(original_id).await.map_err(e)?;
        let pass = release_bases::resolve(pool, &jobs::Progress::none()).await.map_err(de)?;
        // Importing the DAT again lists the original again; it is set aside again.
        import::import_dat(pool, Source::from_name("etc").unwrap(), "patched-with-original.dat", &with_original, &ImportOptions::default()).await.map_err(de)?;
        let after_reimport = (link(plus2).await.map_err(ae)?, files_of(plus2).await.map_err(e)?);
        let by_hash = Some(("hash".to_string(), home, Some(original_id)));
        let checks = [
            (at_import.0 == by_hash && at_import.1 == vec!["plus2.ppf".to_string()], format!("the original beside a trainer links it and is not one of its files: {:?} {:?}", at_import.0, at_import.1)),
            (at_import.2 == by_hash && at_import.3 == vec!["Patchwork Test Retitled.gba".to_string()], format!("an entry that is only the original's bytes keeps them: {:?} {:?}", at_import.2, at_import.3)),
            (originals_files == vec!["Patchwork Test (Japan).gba".to_string()], format!("the original keeps its own file: {originals_files:?}")),
            ((pass.by_hash, pass.originals_set_aside, pass.exact) == (0, 0, 0), format!("a pass finds it done, and a name does not overrule the hash: {pass:?}")),
            (after_reimport == (by_hash.clone(), vec!["plus2.ppf".to_string()]), format!("a second import ends the same: {after_reimport:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }

        // What a hash says about a hack's file is about the hack: it has its own IGDB
        // entry, a "mod" by someone else. The link goes on the hack's release, never on the
        // title it sits under, which is the original's.
        let content_of = |sha1: &'static str| async move { sqlx::query_scalar::<_, u64>("SELECT content_id FROM hashes WHERE algorithm = 'sha1' AND value_hex = ?").bind(format!("{sha1:0>40}")).fetch_one(pool).await };
        let home_id = home.ok_or("the original has no title")?;
        cache::link_for_content(pool, content_of("c1").await.map_err(e)?, Provider::Igdb, "990001", cache::Confidence::Hash).await.map_err(de)?;
        cache::link_contents_bulk(pool, &[(content_of("c2").await.map_err(e)?, Provider::Igdb, "990003".to_string())], cache::Confidence::Hash).await.map_err(de)?;
        cache::link_for_content(pool, content_of("a0").await.map_err(e)?, Provider::Igdb, "990002", cache::Confidence::Hash).await.map_err(de)?;
        let title_igdb: Vec<String> = sqlx::query_scalar("SELECT provider_id FROM title_provider_links WHERE title_id = ? AND provider = 'igdb' AND deleted = FALSE").bind(home_id).fetch_all(pool).await.map_err(e)?;
        let hack_igdb: Vec<String> = sqlx::query_scalar("SELECT provider_id FROM release_provider_links WHERE release_id = ? AND provider = 'igdb' AND deleted = FALSE").bind(hard.0).fetch_all(pool).await.map_err(e)?;
        // On the pages: the title lists its game's pages, the hack its own and not the game's.
        let title_page = catalog::title(&app, home_id).await.map_err(ae)?;
        let hack_page = catalog::release(&app, hard.0).await.map_err(ae)?;
        let on_title: Vec<&str> = title_page.provider_links.iter().filter(|l| l.provider == "igdb").map(|l| l.provider_id.as_str()).collect();
        let on_hack: Vec<&str> = hack_page.provider_links.iter().filter(|l| l.provider == "igdb").map(|l| l.provider_id.as_str()).collect();
        // A title a hack's hash had already relabelled is put right by the pass.
        sqlx::query("UPDATE title SET category = 'mod' WHERE id = ?").bind(home_id).execute(pool).await.map_err(e)?;
        sqlx::query("INSERT INTO title_locales (title_id, language, region, source, name) VALUES (?, 'ja', 'JP', 'igdb', 'ハックの名前')").bind(home_id).execute(pool).await.map_err(e)?;
        let hack_shown_before = catalog::release(&app, hard.0).await.map_err(ae)?.release.display_name;
        let unlinked = release_bases::unlink_titles_from_hacks(pool).await.map_err(de)?;
        // A hack whose row lost its kind (a restored backup from before kinds) gets it back
        // from its own category; the original, listed by a DAT of originals, stays as it is.
        sqlx::query("UPDATE `release` SET game_kind = NULL, community_patched = FALSE, patch_type = NULL WHERE id IN (?, ?)").bind(hard.0).bind(translation.0).execute(pool).await.map_err(e)?;
        let restored = release_bases::restore_kinds(pool).await.map_err(de)?;
        let kinds_now: Vec<Option<String>> = sqlx::query_scalar("SELECT game_kind FROM `release` WHERE id IN (?, ?, ?) ORDER BY FIELD(id, ?, ?, ?)").bind(hard.0).bind(translation.0).bind(original_id).bind(hard.0).bind(translation.0).bind(original_id).fetch_all(pool).await.map_err(e)?;
        let after: (String, i64, i64) = sqlx::query_as("SELECT t.category, (SELECT COUNT(*) FROM title_provider_links l WHERE l.title_id = t.id AND l.provider = 'igdb' AND l.deleted = FALSE), (SELECT COUNT(*) FROM title_locales tl WHERE tl.title_id = t.id AND tl.source = 'igdb') FROM title t WHERE t.id = ?").bind(home_id).fetch_one(pool).await.map_err(e)?;
        let checks = [
            (title_igdb == vec!["990002".to_string()] && hack_igdb.len() == 1, format!("a hack's hash links the hack, the original's its title: title {title_igdb:?}, hack {hack_igdb:?}")),
            (on_title == vec!["990002"] && on_hack == hack_igdb.iter().map(String::as_str).collect::<Vec<_>>(), format!("each page lists its own: title {on_title:?}, hack {on_hack:?}")),
            (!hack_shown_before.contains("ハック"), format!("a hack is shown by its own name, not its title's localized one: {hack_shown_before:?}")),
            (unlinked == 1 && after == ("base_game".to_string(), 0, 0), format!("a title relabelled by a hack is a base game again: {unlinked} title(s), {after:?}")),
            (restored == 2 && kinds_now == vec![Some("hack".to_string()), Some("translation".to_string()), None], format!("a lost kind comes back from the row's own category: {restored} restored, {kinds_now:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }

        // A remake and its base game, whichever is known first: the remake keeps the IGDB id
        // of its parent, and is pointed at the title linked to it when there is one -- the
        // one on its own platform when the game is on two. Each page names the other.
        use romexplorer::database::providers::metadata;
        let snes_dat = "<datafile><header><name>Nintendo - Super Nintendo Entertainment System</name></header>\
            <game name=\"Basegame Order (USA)\"><rom name=\"Basegame Order (USA).sfc\" size=\"8\" sha1=\"00000000000000000000000000000000000000b0\"/></game></datafile>";
        let gba_dat = "<datafile><header><name>Nintendo - Game Boy Advance</name></header>\
            <game name=\"Basegame Order (USA)\"><rom name=\"Basegame Order (USA).gba\" size=\"8\" sha1=\"00000000000000000000000000000000000000b4\"/></game>\
            <game name=\"Basegame Order Remake (USA)\"><rom name=\"Basegame Order Remake (USA).gba\" size=\"8\" sha1=\"00000000000000000000000000000000000000b5\"/></game></datafile>";
        import::import_dat(pool, Source::NoIntro, "basegame-snes.dat", &dat_parse::parse_dat_xml(snes_dat).map_err(de)?, &ImportOptions::default()).await.map_err(de)?;
        import::import_dat(pool, Source::NoIntro, "basegame-gba.dat", &dat_parse::parse_dat_xml(gba_dat).map_err(de)?, &ImportOptions::default()).await.map_err(de)?;
        let title_named = |release: &'static str, platform: u16| async move {
            sqlx::query_scalar::<_, u64>("SELECT tr.title_id FROM title_releases tr JOIN `release` r ON r.id = tr.release_id WHERE r.name = ? AND r.app_platform_id = ?").bind(release).bind(platform).fetch_one(pool).await
        };
        let on_snes = title_named("Basegame Order (USA)", Platform::Snes.igdb_id()).await.map_err(e)?;
        let on_gba = title_named("Basegame Order (USA)", gba).await.map_err(e)?;
        let remake = title_named("Basegame Order Remake (USA)", gba).await.map_err(e)?;
        let base_of = |title: u64| async move { sqlx::query_scalar::<_, Option<u64>>("SELECT basegame_title_id FROM title WHERE id = ?").bind(title).fetch_one(pool).await };
        // The remake's IGDB data came first: it knows its parent's id, and nothing has it yet.
        sqlx::query("UPDATE title SET basegame_igdb_id = 880001, category = 'remake' WHERE id = ?").bind(remake).execute(pool).await.map_err(e)?;
        metadata::link_basegames(pool, &jobs::Progress::none()).await.map_err(de)?;
        let before = base_of(remake).await.map_err(e)?;
        // Then the base game is linked, on another platform; and later on the remake's own.
        cache::link_title(pool, on_snes, Provider::Igdb, "880001", cache::Confidence::Exact).await.map_err(de)?;
        let first = metadata::link_basegames(pool, &jobs::Progress::none()).await.map_err(de)?;
        let other_platform = base_of(remake).await.map_err(e)?;
        cache::link_title(pool, on_gba, Provider::Igdb, "880001", cache::Confidence::Exact).await.map_err(de)?;
        metadata::link_basegames(pool, &jobs::Progress::none()).await.map_err(de)?;
        let again = metadata::link_basegames(pool, &jobs::Progress::none()).await.map_err(de)?;
        let same_platform = base_of(remake).await.map_err(e)?;
        let base_page = catalog::title(&app, on_gba).await.map_err(ae)?;
        let remake_page = catalog::title(&app, remake).await.map_err(ae)?;
        let derived: Vec<(u64, &str)> = base_page.derived.iter().map(|d| (d.id, d.category.as_str())).collect();
        let checks = [
            (before.is_none() && first.linked == 1 && other_platform == Some(on_snes), format!("a base game that arrives later is linked: {before:?} then {other_platform:?} ({first:?})")),
            (same_platform == Some(on_gba) && again.linked == 0, format!("the title on the remake's own platform is preferred, and a second pass changes nothing: {same_platform:?}, {again:?}")),
            (derived == vec![(remake, "remake")], format!("the base game's page lists the remake: {derived:?}")),
            (remake_page.basegame_title_id == Some(on_gba) && remake_page.basegame_platforms.as_deref().is_some_and(|p| p.contains("Game Boy Advance")), format!("the remake's page names the base game and its platform: {:?} {:?}", remake_page.basegame_title_id, remake_page.basegame_platforms)),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }

        // A remake on hardware neither of them is on: the base game on the oldest machine.
        // IGDB files an original and its port as one game, and the port's title may well be
        // the older row -- the lowest id used to win.
        let nointro = import::ensure_source(pool, "no-intro").await.map_err(de)?;
        let titled = |name: &'static str, platform: Platform| async move {
            let norm = name.to_lowercase();
            let t = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, ?, ?)").bind(name).bind(&norm).bind(import::sha1_hex(&norm)).execute(pool).await?.last_insert_id();
            let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (?, ?, ?, ?, ?, 'JP', ?)")
                .bind(platform.igdb_id()).bind(name).bind(import::sha1_hex(name)).bind(&norm).bind(import::sha1_hex(&norm)).bind(nointro)
                .execute(pool).await?.last_insert_id();
            sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(t).bind(r).execute(pool).await?;
            Ok::<u64, sqlx::Error>(t)
        };
        let port_first = titled("Basegame Hardware", Platform::WonderSwanColor).await.map_err(e)?;
        let original = titled("Basegame Hardware", Platform::SuperFamicom).await.map_err(e)?;
        let newer_remake = titled("Basegame Hardware Remake", Platform::PlayStation2).await.map_err(e)?;
        for t in [port_first, original] {
            cache::link_title(pool, t, Provider::Igdb, "880050", cache::Confidence::Exact).await.map_err(de)?;
        }
        sqlx::query("UPDATE title SET basegame_igdb_id = 880050, category = 'remake' WHERE id = ?").bind(newer_remake).execute(pool).await.map_err(e)?;
        // Releases with a year and a date: those are read too (a year that is there must
        // decode, not only an empty one).
        sqlx::query("UPDATE `release` r JOIN title_releases tr ON tr.release_id = r.id SET r.year = 2001, r.released_on = '2001-03-30' WHERE tr.title_id = ?").bind(port_first).execute(pool).await.map_err(e)?;
        sqlx::query("UPDATE `release` r JOIN title_releases tr ON tr.release_id = r.id SET r.year = 1992 WHERE tr.title_id = ?").bind(original).execute(pool).await.map_err(e)?;
        metadata::link_basegames(pool, &jobs::Progress::none()).await.map_err(de)?;
        let oldest = base_of(newer_remake).await.map_err(e)?;
        if oldest != Some(original) {
            return Err(format!("the base game on the oldest hardware: {oldest:?} (Super Famicom {original}, WonderSwan Color port {port_first})"));
        }
        // What IGDB lists for the base game comes before that: here it names only the
        // WonderSwan Color, so the title there is the base game, however old the other machine.
        let listed_wsc = titled("Basegame Listed", Platform::WonderSwanColor).await.map_err(e)?;
        let listed_sfc = titled("Basegame Listed", Platform::SuperFamicom).await.map_err(e)?;
        let listed_remake = titled("Basegame Listed Remake", Platform::PlayStation2).await.map_err(e)?;
        let base_file = std::env::temp_dir().join("romexplorer-test-igdb-880060.json");
        let base_game = serde_json::json!([{ "id": 880060, "checksum": "00000000-0000-0000-0000-000000000000", "created_at": 0, "updated_at": 0, "name": "Test 880060", "slug": "test-880060", "url": "https://www.igdb.com/games/test-880060", "platforms": [123] }]);
        std::fs::write(&base_file, serde_json::to_vec(&base_game).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        sqlx::query("INSERT INTO igdb_games (provider_id, raw_response_path, title) VALUES (880060, ?, 'Test 880060') ON DUPLICATE KEY UPDATE raw_response_path = VALUES(raw_response_path)").bind(base_file.to_string_lossy().to_string()).execute(pool).await.map_err(e)?;
        for t in [listed_sfc, listed_wsc] {
            cache::link_title(pool, t, Provider::Igdb, "880060", cache::Confidence::Exact).await.map_err(de)?;
        }
        sqlx::query("UPDATE title SET basegame_igdb_id = 880060, category = 'remake' WHERE id = ?").bind(listed_remake).execute(pool).await.map_err(e)?;
        metadata::link_basegames(pool, &jobs::Progress::none()).await.map_err(de)?;
        let by_igdb = base_of(listed_remake).await.map_err(e)?;
        let _ = std::fs::remove_file(&base_file);
        if by_igdb != Some(listed_wsc) {
            return Err(format!("the base game on a platform IGDB lists for it: {by_igdb:?} (WonderSwan Color {listed_wsc}, Super Famicom {listed_sfc})"));
        }

        // The same through IGDB's own data, as a provider query applies it: a port and a
        // remaster say what they are and who their parent is; a parent applied after its
        // child is linked to it at once; and a title whose IGDB data was applied before the
        // parent id was kept gets it from the cached IGDB game.
        let family_dat = "<datafile><header><name>Nintendo - Game Boy Advance</name></header>\
            <game name=\"Basegame Order Port (USA)\"><rom name=\"p.gba\" size=\"8\" sha1=\"00000000000000000000000000000000000000b6\"/></game>\
            <game name=\"Basegame Order Remaster (USA)\"><rom name=\"m.gba\" size=\"8\" sha1=\"00000000000000000000000000000000000000b7\"/></game>\
            <game name=\"Basegame Order Side Story (USA)\"><rom name=\"s.gba\" size=\"8\" sha1=\"00000000000000000000000000000000000000b8\"/></game>\
            <game name=\"Basegame Order Late Parent (USA)\"><rom name=\"l.gba\" size=\"8\" sha1=\"00000000000000000000000000000000000000b9\"/></game>\
            <game name=\"Basegame Order Cached (USA)\"><rom name=\"c.gba\" size=\"8\" sha1=\"00000000000000000000000000000000000000ba\"/></game></datafile>";
        import::import_dat(pool, Source::NoIntro, "basegame-family.dat", &dat_parse::parse_dat_xml(family_dat).map_err(de)?, &ImportOptions::default()).await.map_err(de)?;
        let port = title_named("Basegame Order Port (USA)", gba).await.map_err(e)?;
        let remaster = title_named("Basegame Order Remaster (USA)", gba).await.map_err(e)?;
        let side_story = title_named("Basegame Order Side Story (USA)", gba).await.map_err(e)?;
        let late_parent = title_named("Basegame Order Late Parent (USA)", gba).await.map_err(e)?;
        let cached = title_named("Basegame Order Cached (USA)", gba).await.map_err(e)?;
        let igdb_game = |id: i64, kind: i64, parent: Option<i64>| -> Result<playmatch_deredis::providers::igdb::model::Game, String> {
            let mut game = serde_json::json!({ "id": id, "checksum": "00000000-0000-0000-0000-000000000000", "created_at": 0, "updated_at": 0, "name": format!("Test {id}"), "slug": format!("test-{id}"), "url": format!("https://www.igdb.com/games/test-{id}"), "game_type": kind });
            if let Some(parent) = parent {
                game["parent_game"] = serde_json::json!(parent);
            }
            serde_json::from_value(game).map_err(|e| e.to_string())
        };
        let names = metadata::IgdbNames::new();
        // 11 is IGDB's "port", 9 "remaster"; both name game 880001, which two titles hold.
        metadata::apply_igdb_game(pool, &names, port, &igdb_game(880011, 11, Some(880001))?).await.map_err(de)?;
        metadata::apply_igdb_game(pool, &names, remaster, &igdb_game(880012, 9, Some(880001))?).await.map_err(de)?;
        // The side story's parent, 880020, is nobody's yet; then its parent's own data arrives.
        metadata::apply_igdb_game(pool, &names, side_story, &igdb_game(880021, 0, Some(880020))?).await.map_err(de)?;
        let waiting = base_of(side_story).await.map_err(e)?;
        // A title whose link moves to another IGDB game takes that game's names, not both:
        // the first game's are gone once the second is applied.
        let moved = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES ('Basegame Moved Link', 'basegame moved link', ?)").bind(import::sha1_hex("basegame moved link")).execute(pool).await.map_err(e)?.last_insert_id();
        metadata::apply_igdb_game(pool, &names, moved, &igdb_game(880041, 0, None)?).await.map_err(de)?;
        metadata::apply_igdb_game(pool, &names, moved, &igdb_game(880042, 0, None)?).await.map_err(de)?;
        let igdb_names: Vec<String> = sqlx::query_scalar("SELECT name FROM title_locales WHERE title_id = ? AND source = 'igdb' ORDER BY name").bind(moved).fetch_all(pool).await.map_err(e)?;
        metadata::apply_igdb_game(pool, &names, late_parent, &igdb_game(880020, 0, None)?).await.map_err(de)?;
        let found_at_once = base_of(side_story).await.map_err(e)?;
        // Applied before the parent id was kept: only the link and the cached game exist.
        let cache_file = std::env::temp_dir().join("romexplorer-test-igdb-880030.json");
        std::fs::write(&cache_file, serde_json::to_vec(&vec![igdb_game(880030, 8, Some(880001))?]).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        sqlx::query("INSERT INTO igdb_games (provider_id, raw_response_path, title) VALUES (880030, ?, 'Test 880030') ON DUPLICATE KEY UPDATE raw_response_path = VALUES(raw_response_path)").bind(cache_file.to_string_lossy().to_string()).execute(pool).await.map_err(e)?;
        cache::link_title(pool, cached, Provider::Igdb, "880030", cache::Confidence::Exact).await.map_err(de)?;
        let pass = metadata::link_basegames(pool, &jobs::Progress::none()).await.map_err(de)?;
        let _ = std::fs::remove_file(&cache_file);
        let kinds: Vec<(String, Option<u64>)> = sqlx::query_as("SELECT category, basegame_title_id FROM title WHERE id IN (?, ?, ?) ORDER BY FIELD(id, ?, ?, ?)").bind(port).bind(remaster).bind(cached).bind(port).bind(remaster).bind(cached).fetch_all(pool).await.map_err(e)?;
        let family: Vec<(u64, String)> = catalog::title(&app, on_gba).await.map_err(ae)?.derived.into_iter().map(|d| (d.id, d.category)).collect();
        let s = |v: &str| v.to_string();
        let checks = [
            (kinds == vec![(s("port"), Some(on_gba)), (s("remaster"), Some(on_gba)), (s("base_game"), Some(on_gba))], format!("a port, a remaster and a title read from the cache all point at the base game on their platform: {kinds:?}")),
            (waiting.is_none() && found_at_once == Some(late_parent), format!("a parent applied after its child is its base game at once: {waiting:?} then {found_at_once:?}")),
            (igdb_names == vec!["Test 880042".to_string()], format!("the second game's name replaces the first's: {igdb_names:?}")),
            (pass.parents_read == 1, format!("the parent id is read from the cached IGDB game once: {pass:?}")),
            (family.len() == 4 && family.contains(&(port, s("port"))) && family.contains(&(remaster, s("remaster"))) && family.contains(&(remake, s("remake"))) && family.contains(&(cached, s("base_game"))), format!("the base game's page lists its port, remake and remaster: {family:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} versions of one translation; {} of one hack; kinds, Etc and Patches list them; {} exact and {} suggested links; {} folded", versions.len(), hard_versions.len(), resolved.exact, resolved.suggested, folded.by_name + folded.by_content)))
    })
    .await;
}

/// DLC whose DAT name starts with a game's is merged into that game's title, on the game's
/// platform only; the title keeps the game's name even when the DLC's row is the older one.
async fn phase_addon_bundle(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "DLC bundling: a DLC-only title joins its game's, by name and platform", async {
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let src = import::ensure_source(pool, "no-intro").await.map_err(de)?;
        let (ps3, vita) = (Platform::PlayStation3.igdb_id(), Platform::PlayStationVita.igdb_id());
        // The DLC first, so its title has the lower id and is the row that survives.
        let mut titles = Vec::new();
        for (title, release, platform) in [
            ("Bundle Game - Costume Pack", "Bundle Game - Costume Pack (USA) (DLC)", ps3),
            ("Bundle Game - Vita Pack", "Bundle Game - Vita Pack (USA) (DLC)", vita),
            ("Bundle Game", "Bundle Game (USA)", ps3),
            ("Bundle Game", "Bundle Game (USA) (v1.01) (Update)", ps3),
        ] {
            let norm = romexplorer::database::matching::normalize_title(title).to_lowercase();
            let hash = import::sha1_hex(&norm);
            let existing: Option<u64> = sqlx::query_scalar("SELECT id FROM title WHERE norm_hash = ? AND deleted = FALSE").bind(&hash).fetch_optional(pool).await.map_err(e)?;
            let t = match existing {
                Some(t) => t,
                None => sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, ?, ?)").bind(title).bind(&norm).bind(&hash).execute(pool).await.map_err(e)?.last_insert_id(),
            };
            let material = romexplorer::database::platform_map::addon_from_release(release, None);
            let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id, material) VALUES (?, ?, ?, ?, ?, 'US', ?, ?)")
                .bind(platform).bind(release).bind(import::sha1_hex(release)).bind(&norm).bind(&hash).bind(src).bind(material)
                .execute(pool).await.map_err(e)?.last_insert_id();
            sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(t).bind(r).execute(pool).await.map_err(e)?;
            titles.push((t, r));
        }
        let (dlc_title, dlc_release) = titles[0];
        let (vita_title, _) = titles[1];
        let (game_title, _) = titles[2];
        let summary = database::title_merge::bundle_addons(pool, &jobs::Progress::none()).await.map_err(de)?;
        let home: u64 = sqlx::query_scalar("SELECT title_id FROM title_releases WHERE release_id = ?").bind(dlc_release).fetch_one(pool).await.map_err(e)?;
        let together: Vec<Option<String>> = sqlx::query_scalar("SELECT r.material FROM title_releases tr JOIN `release` r ON r.id = tr.release_id WHERE tr.title_id = ? ORDER BY r.id").bind(home).fetch_all(pool).await.map_err(e)?;
        let (name, deleted): (String, bool) = sqlx::query_as("SELECT name, deleted FROM title WHERE id = ?").bind(home).fetch_one(pool).await.map_err(e)?;
        let vita_alone: bool = sqlx::query_scalar("SELECT deleted = FALSE FROM title WHERE id = ?").bind(vita_title).fetch_one(pool).await.map_err(e)?;
        let again = database::title_merge::bundle_addons(pool, &jobs::Progress::none()).await.map_err(de)?;

        // The same through the lists: the game is a title, its DLC is not; the title page has
        // all three releases, the game first; the release page has the DLC and the update.
        use romexplorer::api::catalog::{self, ListQuery};
        let ae = |e: romexplorer::api::Error| e.message;
        let app = romexplorer::api::App::with_pool(pool.clone(), env.paths.clone(), DatabaseConfig::default()).map_err(ae)?;
        let titles_of = |material: Option<&'static str>, igdb: Option<u64>| {
            let app = &app;
            async move {
                let q = ListQuery { search: Some("Bundle Game".into()), material: material.map(str::to_string), igdb, limit: Some(100), view: Some("collection".into()), ..Default::default() };
                catalog::titles(app, &q).await.map(|p| p.items.into_iter().map(|t| t.id).collect::<Vec<u64>>())
            }
        };
        let as_games = titles_of(None, None).await.map_err(ae)?;
        let unbundled = titles_of(Some("unbundled"), None).await.map_err(ae)?;
        let page: Vec<Option<String>> = catalog::title(&app, home).await.map_err(ae)?.releases.into_iter().map(|r| r.material).collect();
        let game_release = titles[2].1;
        let with_release: Vec<Option<String>> = catalog::release(&app, game_release).await.map_err(ae)?.addons.into_iter().map(|r| r.material).collect();

        // The same game on another platform, as a title of its own under one IGDB id: one
        // row of the cross-platform list, two titles behind it.
        let xo = Platform::XboxOne.igdb_id();
        let other = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES ('Bundle Game XO', 'bundle game xo', ?)").bind(import::sha1_hex("bundle game xo")).execute(pool).await.map_err(e)?.last_insert_id();
        let other_release = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (?, 'Bundle Game XO (USA)', ?, 'bundle game xo', ?, 'US', ?)")
            .bind(xo).bind(import::sha1_hex("Bundle Game XO (USA)")).bind(import::sha1_hex("bundle game xo")).bind(src)
            .execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(other).bind(other_release).execute(pool).await.map_err(e)?;
        for t in [home, other, vita_title] {
            sqlx::query("INSERT INTO title_provider_links (title_id, provider, provider_id, confidence) VALUES (?, 'igdb', '424242', 'manual')").bind(t).execute(pool).await.map_err(e)?;
        }
        // IGDB's box front, as a sync stores it, on one of the game's releases.
        let front = sqlx::query("INSERT INTO media (media_type, sub_category, path, root_role, source, provider_id) VALUES ('box', 'front', 'igdb/424242/common/box_front.jpg', 'image', 'igdb', '424242')").execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO release_media (release_id, media_id) VALUES (?, ?)").bind(other_release).bind(front).execute(pool).await.map_err(e)?;
        let cross = catalog::crossplatform(&app, &ListQuery { search: Some("Bundle Game".into()), multi: true, ..Default::default() }).await.map_err(ae)?;
        let cross_on_xo = catalog::crossplatform(&app, &ListQuery { search: Some("Bundle Game".into()), platform: Some(xo), ..Default::default() }).await.map_err(ae)?;
        let same_game = titles_of(None, Some(424242)).await.map_err(ae)?;
        let row = cross.items.first().map(|r| (r.title_count, r.titles.iter().map(|t| t.id).collect::<Vec<_>>(), r.platforms.clone(), r.releases));
        // Which title leads the row is `platform_priority`'s say: the newer generation's unless
        // told (Xbox One is a generation after PS3).
        let lead = |items: &[catalog::CrossRow]| items.first().map(|r| (r.title_id, r.platforms.clone()));
        let by_default = lead(&cross.items);
        database::settings::set(pool, database::settings::KEY_PLATFORM_PRIORITY, &vec![ps3]).await.map_err(de)?;
        let ps3_first = lead(&catalog::crossplatform(&app, &ListQuery { search: Some("Bundle Game".into()), ..Default::default() }).await.map_err(ae)?.items);
        let offered = catalog::crossplatform_platforms(&app).await.map_err(ae)?;
        let checks = [
            (summary.bundled >= 1 && summary.games >= 1, format!("{summary:?}")),
            (home == dlc_title.min(game_title) && !deleted, format!("the DLC's release is under the merged title: {home} (dlc {dlc_title}, game {game_title})")),
            (together == vec![Some("dlc".to_string()), None, Some("update".to_string())], format!("game, its DLC and its update in one title: {together:?}")),
            (name == "Bundle Game", format!("named after the game, not the DLC: {name:?}")),
            (vita_alone, "DLC for a platform the game is not on stays on its own".to_string()),
            (again.bundled == 0, format!("nothing left to bundle the second time: {again:?}")),
            (as_games.contains(&home) && !as_games.contains(&vita_title), format!("Titles lists the game and not the DLC-only title: {as_games:?}")),
            (unbundled == vec![vita_title], format!("the DLC nothing claimed is what is left to merge by hand: {unbundled:?}")),
            (page.len() == 3 && page[0].is_none() && page[1..].iter().all(Option::is_some), format!("the title page has the game first, then what came with it: {page:?}")),
            (with_release.len() == 2 && with_release.iter().all(Option::is_some), format!("the release page has its DLC and its update: {with_release:?}")),
            (cross.total == 1 && row.as_ref().is_some_and(|(n, ids, platforms, releases)| *n == 2 && ids.len() == 2 && ids.contains(&home) && ids.contains(&other) && platforms.contains("Xbox One") && platforms.contains("PlayStation 3") && *releases == 2), format!("one cross-platform row for the two titles; the DLC-only one is not a third: {row:?} of {}", cross.total)),
            (cross_on_xo.total == 1 && cross_on_xo.items[0].title_count == 2, format!("filtered by one platform the row is still the whole game: {}", cross_on_xo.total)),
            (cross.items.first().and_then(|r| r.cover_media_id) == Some(front), format!("the row shows the box front its titles have, as the Titles grid does: {:?}", cross.items.first().map(|r| r.cover_media_id))),
            (same_game.len() == 2, format!("Titles filtered by the IGDB id shows both: {same_game:?}")),
            (by_default == Some((other, "Xbox One, PlayStation 3".to_string())), format!("by default the newer generation's title leads: {by_default:?}")),
            (ps3_first == Some((home, "PlayStation 3, Xbox One".to_string())), format!("with PlayStation 3 put first, its title and badge lead: {ps3_first:?}")),
            (offered.contains(&ps3) && offered.contains(&xo) && !offered.contains(&vita), format!("the editor offers the platforms of multi-platform games only: {offered:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{summary:?}")))
    })
    .await;
}

/// Localized names set by hand are the user's: IGDB re-apply leaves them, a merge keeps the
/// lock, and unlocking drops them.
async fn phase_locale_edit(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "titles: localized names set by hand survive IGDB re-apply", async {
        use romexplorer::api::titles::{LocaleEdit, LocalesEdit};
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let ae = |e: romexplorer::api::Error| e.message;
        let app = romexplorer::api::App::with_pool(pool.clone(), env.paths.clone(), DatabaseConfig::default()).map_err(ae)?;
        let title = |name: &'static str| async move {
            sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, ?, ?)").bind(name).bind(name.to_lowercase()).bind(import::sha1_hex(&format!("loctest/{name}"))).execute(pool).await.map(|r| r.last_insert_id())
        };
        let t = title("Loctest Hoge Quest").await.map_err(e)?;
        sqlx::query("INSERT INTO title_locales (title_id, language, region, source, name) VALUES (?, 'ja', 'JP', 'igdb', 'Wrong Fuga'), (?, 'en', '', 'igdb', 'Loctest Hoge Quest')").bind(t).bind(t).execute(pool).await.map_err(e)?;
        let edit = LocalesEdit { locales: vec![LocaleEdit { language: "ja".into(), region: "jp".into(), name: " Hoge no Bouken ".into() }] };
        romexplorer::api::titles::set_locales(&app, t, &edit).await.map_err(ae)?;
        let rows = |id: u64| async move { sqlx::query_as::<_, (String, String, String, String)>("SELECT language, region, source, name FROM title_locales WHERE title_id = ? ORDER BY language").bind(id).fetch_all(pool).await };
        let edited = rows(t).await.map_err(e)?;
        // An IGDB re-apply of the title: its names stay the user's.
        let game: playmatch_deredis::providers::igdb::model::Game = serde_json::from_value(serde_json::json!({"id": 886101, "name": "Wrong Fuga", "slug": "x", "url": "u", "checksum": "00000000-0000-0000-0000-000000000000", "created_at": 0, "updated_at": 0})).map_err(|e| e.to_string())?;
        database::providers::metadata::apply_igdb_game(pool, &Default::default(), t, &game).await.map_err(de)?;
        let after_apply = rows(t).await.map_err(e)?;
        let dup = romexplorer::api::titles::set_locales(&app, t, &LocalesEdit { locales: vec![LocaleEdit { language: "ja".into(), region: "JP".into(), name: "a".into() }, LocaleEdit { language: "JA".into(), region: "jp".into(), name: "b".into() }] }).await.is_err();
        // Merged into a lower id, the lock goes along.
        let other = title("Loctest Hoge Quest J").await.map_err(e)?;
        let (keep, gone) = (t.min(other), t.max(other));
        let survivor = database::title_merge::merge_titles(pool, &[keep, gone], "manual", "manual").await.map_err(de)?;
        let locked: bool = sqlx::query_scalar("SELECT locales_locked FROM title WHERE id = ?").bind(survivor).fetch_one(pool).await.map_err(e)?;
        romexplorer::api::titles::unlock_locales(&app, survivor).await.map_err(ae)?;
        let unlocked: (bool, i64) = sqlx::query_as("SELECT locales_locked, (SELECT COUNT(*) FROM title_locales WHERE title_id = ? AND source = 'user') FROM title WHERE id = ?").bind(survivor).bind(survivor).fetch_one(pool).await.map_err(e)?;
        let want = vec![("ja".to_string(), "JP".to_string(), "user".to_string(), "Hoge no Bouken".to_string())];
        let checks = [
            (edited == want, format!("the edit replaces the names with the user's: {edited:?}")),
            (after_apply == want, format!("IGDB re-apply leaves them: {after_apply:?}")),
            (dup, "the same language and region twice is refused".to_string()),
            (survivor == keep && locked, format!("a merge keeps the lock: survivor {survivor}, locked {locked}")),
            (unlocked == (false, 0), format!("unlocking drops the user's names: {unlocked:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} checks", checks.len())))
    })
    .await;
}

/// Merge review: releases arranged by hand go where they were put -- one into another title,
/// two into a new one named after the first -- and the card leaves the to-do list for "done".
async fn phase_review_layout(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "merge review: a grouping arranged by hand, and done cards", async {
        use romexplorer::api::review::{self, ApplyRequest, LayoutGroup, ReviewParams};
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let ae = |e: romexplorer::api::Error| e.message;
        let app = romexplorer::api::App::with_pool(pool.clone(), env.paths.clone(), DatabaseConfig::default()).map_err(ae)?;
        let src = import::ensure_source(pool, "no-intro").await.map_err(de)?;
        let nes = Platform::Nes.igdb_id();
        let title = |name: &'static str| async move {
            sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, ?, ?)").bind(name).bind(name.to_lowercase()).bind(import::sha1_hex(&format!("layout/{name}"))).execute(pool).await.map(|r| r.last_insert_id())
        };
        let release = |t: u64, name: &'static str| async move {
            let norm = name.to_lowercase();
            let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (?, ?, ?, ?, ?, 'CN', ?)")
                .bind(nes).bind(name).bind(import::sha1_hex(name)).bind(&norm).bind(import::sha1_hex(&norm)).bind(src)
                .execute(pool).await?.last_insert_id();
            sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(t).bind(r).execute(pool).await?;
            Ok::<u64, sqlx::Error>(r)
        };
        let a = title("Layout Hoge").await.map_err(e)?;
        let b = title("Layout Fuga").await.map_err(e)?;
        let own = release(a, "Layout Hoge (China)").await.map_err(e)?;
        let fuga = release(a, "Layout Fuga (China) (Pirate)").await.map_err(e)?;
        let piyo = release(a, "Layout Piyo (China) (Pirate)").await.map_err(e)?;
        let piyo2 = release(a, "Layout Piyo (China) (Pirate) [b]").await.map_err(e)?;
        let key = format!("split:{a}:test");
        sqlx::query("INSERT INTO title_merge_suggestions (igdb_id, title_ids, reason, kind, priority, detail) VALUES (?, ?, 'split_names', 'split_names', 25, ?)")
            .bind(&key).bind(a.to_string()).bind(serde_json::json!({ "options": [{ "label": "split", "recommended": true, "groups": [[own], [fuga, piyo, piyo2]] }] }).to_string())
            .execute(pool).await.map_err(e)?;
        let p = |state: &str| ReviewParams { kind: Some("split_names".into()), state: Some(state.into()), ..Default::default() };
        let q = romexplorer::api::catalog::ListQuery::default();
        let todo_before = review::merge_queue(&app, &q, &p("todo")).await.map_err(ae)?.items.iter().any(|i| i.key == key);
        let layout = vec![LayoutGroup { title_id: Some(a), releases: vec![own] }, LayoutGroup { title_id: Some(b), releases: vec![fuga] }, LayoutGroup { title_id: None, releases: vec![piyo, piyo2] }];
        let out = review::apply(&app, &key, &ApplyRequest { layout: Some(layout), ..Default::default() }).await.map_err(ae)?;
        let home = |r: u64| async move { sqlx::query_scalar::<_, u64>("SELECT title_id FROM title_releases WHERE release_id = ?").bind(r).fetch_one(pool).await };
        let (h_own, h_fuga, h_piyo, h_piyo2) = (home(own).await.map_err(e)?, home(fuga).await.map_err(e)?, home(piyo).await.map_err(e)?, home(piyo2).await.map_err(e)?);
        let new_name: String = sqlx::query_scalar("SELECT name FROM title WHERE id = ?").bind(h_piyo).fetch_one(pool).await.map_err(e)?;
        let logged: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title_merge_log WHERE reason = 'split' AND survivor_id = ? AND merged_title_id = ?").bind(a).bind(h_piyo).fetch_one(pool).await.map_err(e)?;
        // A split made before splits were logged: the accepted card rebuilds its row.
        sqlx::query("DELETE a, l FROM title_merge_log l LEFT JOIN title_merge_archive a ON a.log_id = l.id WHERE l.reason = 'split' AND l.survivor_id = ?").bind(a).execute(pool).await.map_err(e)?;
        sqlx::query("DELETE FROM data_migrations WHERE name = 'split_log_backfill'").execute(pool).await.map_err(e)?;
        romexplorer::database::title_merge::log_past_splits_once(pool).await.map_err(de)?;
        let rebuilt: Vec<(u64, i64)> = sqlx::query_as("SELECT l.merged_title_id, CAST(JSON_LENGTH(a.rows_json, '$.title_releases') AS SIGNED) FROM title_merge_log l JOIN title_merge_archive a ON a.log_id = l.id WHERE l.reason = 'split' AND l.survivor_id = ? ORDER BY l.merged_title_id").bind(a).fetch_all(pool).await.map_err(e)?;
        // A card made before running numbers counted as the same game is dropped once.
        let n = title("Layout Numbered Hoge").await.map_err(e)?;
        release(n, "Layout Numbered Hoge (China)").await.map_err(e)?;
        let numbered = release(n, "0743 - Layout Numbered Hoge (China)").await.map_err(e)?;
        let stale = format!("split:{n}:{numbered}");
        sqlx::query("INSERT INTO title_merge_suggestions (igdb_id, title_ids, reason, kind, priority, detail) VALUES (?, ?, 'split_names', 'split_names', 25, '{\"options\":[]}')").bind(&stale).bind(n.to_string()).execute(pool).await.map_err(e)?;
        let dropped = romexplorer::database::title_review::recheck_split_names_once(pool, "test_recheck").await.map_err(de)?;
        let stale_left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title_merge_suggestions WHERE igdb_id = ?").bind(&stale).fetch_one(pool).await.map_err(e)?;
        let todo_after = review::merge_queue(&app, &q, &p("todo")).await.map_err(ae)?.items.iter().any(|i| i.key == key);
        let done = review::merge_queue(&app, &q, &p("done")).await.map_err(ae)?.items.iter().find(|i| i.key == key).map(|i| i.status.clone());
        let checks = [
            (todo_before, "the card is to do at first".to_string()),
            (h_own == a && h_fuga == b && h_piyo == h_piyo2 && h_piyo != a && h_piyo != b && out.title_ids.len() == 3, format!("each where it was put: {h_own} {h_fuga} {h_piyo} {h_piyo2}, titles {:?}", out.title_ids)),
            (new_name.contains("Piyo") && logged == 1, format!("the new title is named after the first and logged: {new_name:?}, {logged}")),
            (rebuilt.iter().any(|(t, n)| *t == h_piyo && *n == 2) && rebuilt.iter().any(|(t, n)| *t == b && *n == 1), format!("the log is rebuilt from the card: {rebuilt:?}")),
            (dropped >= 1 && stale_left == 0, format!("a card for a running number is dropped: {dropped} dropped, left {stale_left}")),
            (!todo_after && done.as_deref() == Some("accepted"), format!("then it is done: to do {todo_after}, done {done:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} checks", checks.len())))
    })
    .await;
}

/// A title of nothing but pirate releases shares an IGDB game with a retail title on the same
/// platform: the automatic pass leaves them apart and suggests nothing; two retail titles of
/// that game still merge.
async fn phase_unlicensed_apart(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "titles: pirate games never merge into a retail title", async {
        use romexplorer::database::title_merge;
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let src = import::ensure_source(pool, "no-intro").await.map_err(de)?;
        let nes = Platform::Nes.igdb_id();
        let igdb = "990077";
        let title = |name: &'static str, release: &'static str| async move {
            let t = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, ?, ?)").bind(name).bind(name.to_lowercase()).bind(import::sha1_hex(&format!("unlapart/{name}"))).execute(pool).await?.last_insert_id();
            let norm = release.to_lowercase();
            let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (?, ?, ?, ?, ?, 'JP', ?)")
                .bind(nes).bind(release).bind(import::sha1_hex(release)).bind(&norm).bind(import::sha1_hex(&norm)).bind(src)
                .execute(pool).await?.last_insert_id();
            sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(t).bind(r).execute(pool).await?;
            sqlx::query("INSERT INTO title_provider_links (title_id, provider, provider_id, confidence, linked_at, deleted) VALUES (?, 'igdb', ?, 'hash', NOW(), FALSE)").bind(t).bind(igdb).execute(pool).await?;
            Ok::<u64, sqlx::Error>(t)
        };
        let retail = title("Unlapart Hoge II", "Unlapart Hoge II (Japan)").await.map_err(e)?;
        let other = title("Unlapart Hoge Ni", "Unlapart Hoge Ni (Japan)").await.map_err(e)?;
        let pirate = title("Unlapart Fuga Zhan", "Unlapart Fuga Zhan (China) (Pirate)").await.map_err(e)?;
        title_merge::dedupe_by_igdb(pool, Some(igdb), &database::jobs::Progress::none()).await.map_err(de)?;
        let live = |t: u64| async move { sqlx::query_scalar::<_, bool>("SELECT NOT deleted FROM title WHERE id = ?").bind(t).fetch_one(pool).await };
        let (retail_live, other_live, pirate_live) = (live(retail).await.map_err(e)?, live(other).await.map_err(e)?, live(pirate).await.map_err(e)?);
        let pirate_home: Option<u64> = sqlx::query_scalar("SELECT tr.title_id FROM title_releases tr JOIN `release` r ON r.id = tr.release_id WHERE r.name = 'Unlapart Fuga Zhan (China) (Pirate)'").fetch_optional(pool).await.map_err(e)?;
        let suggested: Option<String> = sqlx::query_scalar("SELECT title_ids FROM title_merge_suggestions WHERE igdb_id = ? AND status = 'pending'").bind(igdb).fetch_optional(pool).await.map_err(e)?;
        // One merged before this rule (the audit's job to find): flagged, the retail one not.
        let retail_log: Option<u64> = sqlx::query_scalar("SELECT id FROM title_merge_log WHERE merged_title_id = ? AND undone_at IS NULL").bind(other).fetch_optional(pool).await.map_err(e)?;
        let retail_flag = match retail_log { Some(id) => title_merge::merge_joined_unlicensed(pool, id).await.map_err(de)?, None => true };
        title_merge::merge_titles(pool, &[retail, pirate], &format!("igdb:{igdb}"), "hash").await.map_err(de)?;
        let old_log: Option<u64> = sqlx::query_scalar("SELECT id FROM title_merge_log WHERE merged_title_id = ? AND undone_at IS NULL").bind(pirate).fetch_optional(pool).await.map_err(e)?;
        let flagged = match old_log { Some(id) => title_merge::merge_joined_unlicensed(pool, id).await.map_err(de)?, None => false };
        let checks = [
            (flagged && !retail_flag, format!("an earlier pirate merge is found: {flagged}, the retail merge is not: {retail_flag}")),
            (retail_live && !other_live, format!("the two retail titles merge: {retail} live {retail_live}, {other} live {other_live}")),
            (pirate_live && pirate_home == Some(pirate), format!("the pirate title stays its own: live {pirate_live}, its release under {pirate_home:?}")),
            (suggested.as_deref().is_none_or(|s| !s.split(',').any(|x| x == pirate.to_string())), format!("and is not suggested: {suggested:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} checks", checks.len())))
    })
    .await;
}

/// A RetroAchievements subset is a patched ROM: an import gives it a title of its own, a
/// subset shared by two versions of a game does not merge them, and the audit's step takes
/// subsets out of their game's title and splits a merge one made.
async fn phase_ra_subsets_apart(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "titles: RetroAchievements subsets stay titles of their own", async {
        use romexplorer::database::title_merge;
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let dat = "<datafile><header><name>Nintendo - Game Boy</name></header>\
            <game name=\"Subhoge Blue (Japan)\"><rom name=\"Subhoge Blue (Japan).gb\" size=\"8\" sha1=\"0000000000000000000000000000000000005b01\"/></game>\
            <game name=\"Subhoge Blue [Subset - Speedrun] (Japan)\"><rom name=\"Subhoge Blue [Subset - Speedrun] (Japan).gb\" size=\"8\" sha1=\"0000000000000000000000000000000000005b02\"/></game>\
            <game name=\"Subhoge Red (Japan)\"><rom name=\"Subhoge Red (Japan).gb\" size=\"8\" sha1=\"0000000000000000000000000000000000005b03\"/></game></datafile>";
        import::import_dat(pool, Source::RetroAchievements, "subset-test.dat", &dat_parse::parse_dat_xml(dat).map_err(de)?, &ImportOptions::default()).await.map_err(de)?;
        let home = |name: &'static str| async move {
            sqlx::query_as::<_, (u64, String)>("SELECT t.id, t.name FROM `release` r JOIN title_releases tr ON tr.release_id = r.id JOIN title t ON t.id = tr.title_id WHERE r.name = ?").bind(name).fetch_one(pool).await
        };
        let (blue, _) = home("Subhoge Blue (Japan)").await.map_err(e)?;
        let (sub, sub_name) = home("Subhoge Blue [Subset - Speedrun] (Japan)").await.map_err(e)?;
        let (red, _) = home("Subhoge Red (Japan)").await.map_err(e)?;

        // One subset for both versions, linked to both: the RA pass leaves them apart.
        let sub_release: u64 = sqlx::query_scalar("SELECT id FROM `release` WHERE name = 'Subhoge Blue [Subset - Speedrun] (Japan)'").fetch_one(pool).await.map_err(e)?;
        let src = import::ensure_source(pool, "retroachievements").await.map_err(de)?;
        let gb = Platform::GameBoy.igdb_id();
        let red_sub = "Subhoge Red [Subset - Speedrun] (Japan)";
        let red_sub_release = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (?, ?, ?, ?, ?, 'JP', ?)")
            .bind(gb).bind(red_sub).bind(import::sha1_hex(red_sub)).bind(red_sub.to_lowercase()).bind(import::sha1_hex(&red_sub.to_lowercase())).bind(src)
            .execute(pool).await.map_err(e)?.last_insert_id();
        // As an import before this change had it: inside the game's title.
        sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(red).bind(red_sub_release).execute(pool).await.map_err(e)?;
        for r in [sub_release, red_sub_release] {
            sqlx::query("INSERT INTO release_provider_links (release_id, provider, provider_id, confidence, linked_at, deleted) VALUES (?, 'retroachievements', '990088', 'exact', NOW(), FALSE)").bind(r).execute(pool).await.map_err(e)?;
        }
        title_merge::dedupe_by_igdb(pool, None, &database::jobs::Progress::none()).await.map_err(de)?;
        let live = |t: u64| async move { sqlx::query_scalar::<_, bool>("SELECT NOT deleted FROM title WHERE id = ?").bind(t).fetch_one(pool).await };
        let (blue_live, red_live, sub_live) = (live(blue).await.map_err(e)?, live(red).await.map_err(e)?, live(sub).await.map_err(e)?);

        // A merge made through the subset before this rule: found by the audit.
        title_merge::merge_titles(pool, &[red, blue], "ra:990088", "hash").await.map_err(de)?;
        let found = title_merge::ra_subset_merges(pool).await.map_err(de)?;
        let log: Option<u64> = sqlx::query_scalar("SELECT id FROM title_merge_log WHERE reason = 'ra:990088' AND undone_at IS NULL").fetch_optional(pool).await.map_err(e)?;
        let flagged = log.is_some_and(|l| found.contains(&l));
        if let Some(l) = log {
            title_merge::decouple(pool, l).await.map_err(de)?;
        }
        let bundled = title_merge::bundled_subsets(pool).await.map_err(de)?;
        let in_red = bundled.get(&red).cloned().unwrap_or_default();
        let split = title_merge::split_subsets(pool, &in_red).await.map_err(de)?;
        let (red_sub_home, red_sub_title) = home("Subhoge Red [Subset - Speedrun] (Japan)").await.map_err(e)?;
        let checks = [
            (sub != blue && sub_name.contains("[Subset - Speedrun]"), format!("the import gives the subset its own title: #{sub} {sub_name:?}, the game #{blue}")),
            (blue_live && red_live && sub_live, format!("a subset shared by two versions does not merge them: {blue_live} {red_live} {sub_live}")),
            (flagged, format!("an earlier merge through the subset is found: {found:?}")),
            (in_red.len() == 1 && split.len() == 1 && red_sub_home != red && red_sub_title.contains("[Subset - Speedrun]"), format!("the subset inside the game's title is split out: {in_red:?} -> #{red_sub_home} {red_sub_title:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} checks", checks.len())))
    })
    .await;
}

/// Undoing a merge taken from a suggestion puts the suggestion back in the review, pending.
async fn phase_undo_reopens_suggestion(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "merge review: undoing a taken suggestion puts it back", async {
        use romexplorer::database::title_merge;
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let ae = |e: romexplorer::api::Error| e.message;
        let app = romexplorer::api::App::with_pool(pool.clone(), env.paths.clone(), DatabaseConfig::default()).map_err(ae)?;
        let src = import::ensure_source(pool, "no-intro").await.map_err(de)?;
        let nes = Platform::Nes.igdb_id();
        let title = |name: &'static str, release: &'static str| async move {
            let t = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, ?, ?)").bind(name).bind(name.to_lowercase()).bind(import::sha1_hex(&format!("reopen/{name}"))).execute(pool).await?.last_insert_id();
            let norm = release.to_lowercase();
            let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (?, ?, ?, ?, ?, 'JP', ?)")
                .bind(nes).bind(release).bind(import::sha1_hex(release)).bind(&norm).bind(import::sha1_hex(&norm)).bind(src)
                .execute(pool).await?.last_insert_id();
            sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(t).bind(r).execute(pool).await?;
            Ok::<u64, sqlx::Error>(t)
        };
        let a = title("Reopen Hoge", "Reopen Hoge (Japan)").await.map_err(e)?;
        let b = title("Reopen Hoge Gaiden", "Reopen Hoge Gaiden (Japan)").await.map_err(e)?;
        let key = "name:reopen-test";
        sqlx::query("INSERT INTO title_merge_suggestions (igdb_id, title_ids, reason) VALUES (?, ?, 'name_suffix')").bind(key).bind(format!("{a},{b}")).execute(pool).await.map_err(e)?;
        title_merge::resolve_suggestion(pool, key, true).await.map_err(de)?;
        let taken: String = sqlx::query_scalar("SELECT CAST(status AS CHAR) FROM title_merge_suggestions WHERE igdb_id = ?").bind(key).fetch_one(pool).await.map_err(e)?;
        let log: u64 = sqlx::query_scalar("SELECT id FROM title_merge_log WHERE suggestion_key = ? AND undone_at IS NULL").bind(key).fetch_one(pool).await.map_err(e)?;
        romexplorer::api::titles::decouple(&app, log).await.map_err(ae)?;
        let back: String = sqlx::query_scalar("SELECT CAST(status AS CHAR) FROM title_merge_suggestions WHERE igdb_id = ?").bind(key).fetch_one(pool).await.map_err(e)?;
        let both_live: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title WHERE id IN (?, ?) AND deleted = FALSE").bind(a).bind(b).fetch_one(pool).await.map_err(e)?;
        // Merged by hand instead of from the card: the card is done, and undoing brings it back.
        let c = title("Reopen Fuga", "Reopen Fuga (Japan)").await.map_err(e)?;
        let d = title("Reopen Fuga Gaiden", "Reopen Fuga Gaiden (Japan)").await.map_err(e)?;
        let by_hand = "name:reopen-by-hand";
        sqlx::query("INSERT INTO title_merge_suggestions (igdb_id, title_ids, reason) VALUES (?, ?, 'name_suffix')").bind(by_hand).bind(format!("{c},{d}")).execute(pool).await.map_err(e)?;
        romexplorer::api::titles::merge(&app, &romexplorer::api::titles::MergeRequest { title_ids: vec![c, d] }).await.map_err(ae)?;
        let hand_status: String = sqlx::query_scalar("SELECT CAST(status AS CHAR) FROM title_merge_suggestions WHERE igdb_id = ?").bind(by_hand).fetch_one(pool).await.map_err(e)?;
        let hand_log: u64 = sqlx::query_scalar("SELECT id FROM title_merge_log WHERE merged_title_id = ? AND undone_at IS NULL").bind(c.max(d)).fetch_one(pool).await.map_err(e)?;
        romexplorer::api::titles::decouple(&app, hand_log).await.map_err(ae)?;
        let hand_back: String = sqlx::query_scalar("SELECT CAST(status AS CHAR) FROM title_merge_suggestions WHERE igdb_id = ?").bind(by_hand).fetch_one(pool).await.map_err(e)?;
        let checks = [
            (taken == "accepted", format!("taking it marks it accepted: {taken}")),
            (back == "pending" && both_live == 2, format!("undone, it is pending again: {back}, {both_live} titles live")),
            (hand_status == "accepted", format!("merging its titles by hand takes it off the review: {hand_status}")),
            (hand_back == "pending", format!("and undoing that merge puts it back: {hand_back}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} checks", checks.len())))
    })
    .await;
}

/// A split is logged under Edit › Combined titles, the title page of either side lists it,
/// "decouple all" leaves it, and undoing it puts the releases back and drops the empty title.
async fn phase_split_log(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "titles: a split is logged and can be undone", async {
        use romexplorer::api::titles::ReleasesSplit;
        use romexplorer::database::title_merge;
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let ae = |e: romexplorer::api::Error| e.message;
        let app = romexplorer::api::App::with_pool(pool.clone(), env.paths.clone(), DatabaseConfig::default()).map_err(ae)?;
        let src = import::ensure_source(pool, "no-intro").await.map_err(de)?;
        let nes = Platform::Nes.igdb_id();
        let t: u64 = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES ('Splitlog Hoge', 'splitlog hoge', ?)").bind(import::sha1_hex("splitlog/hoge")).execute(pool).await.map_err(e)?.last_insert_id();
        let release = |name: &'static str| async move {
            let norm = name.to_lowercase();
            let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (?, ?, ?, ?, ?, 'CN', ?)")
                .bind(nes).bind(name).bind(import::sha1_hex(name)).bind(&norm).bind(import::sha1_hex(&norm)).bind(src)
                .execute(pool).await?.last_insert_id();
            sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(t).bind(r).execute(pool).await?;
            Ok::<u64, sqlx::Error>(r)
        };
        let own = release("Splitlog Hoge (China)").await.map_err(e)?;
        let odd = release("Splitlog Fuga Zhan (China) (Pirate)").await.map_err(e)?;
        let new = romexplorer::api::titles::split_releases(&app, &ReleasesSplit { release_ids: vec![odd] }).await.map_err(ae)?;
        let (rows, _) = title_merge::merge_log(pool, None, Some(t), None, false, 50, 0).await.map_err(de)?;
        let (theirs, _) = title_merge::merge_log(pool, None, Some(new), None, false, 50, 0).await.map_err(de)?;
        let row = rows.iter().find(|r| r.reason == title_merge::SPLIT).cloned();
        let logged = row.as_ref().is_some_and(|r| r.survivor_id == t && r.merged_title_id == new && r.release_count == Some(1) && r.merged_name.contains("Fuga"));
        let listed_on_new = theirs.iter().any(|r| r.reason == title_merge::SPLIT && r.merged_title_id == new);
        let all = title_merge::decouple_grade(pool, "manual", 500).await.map_err(de)?;
        let still: Vec<u64> = sqlx::query_scalar("SELECT release_id FROM title_releases WHERE title_id = ?").bind(new).fetch_all(pool).await.map_err(e)?;
        let back = match &row { Some(r) => title_merge::decouple(pool, r.id).await.map_err(de)?, None => 0 };
        let mut holds: Vec<u64> = sqlx::query_scalar("SELECT release_id FROM title_releases WHERE title_id = ?").bind(t).fetch_all(pool).await.map_err(e)?;
        holds.sort_unstable();
        let gone: bool = sqlx::query_scalar("SELECT deleted FROM title WHERE id = ?").bind(new).fetch_one(pool).await.map_err(e)?;
        let blocked: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title_merge_blocks WHERE title_a = ? AND title_b = ?").bind(t.min(new)).bind(t.max(new)).fetch_one(pool).await.map_err(e)?;
        let again = match &row { Some(r) => title_merge::decouple(pool, r.id).await.is_err(), None => false };
        let mut want = vec![own, odd];
        want.sort_unstable();
        let checks = [
            (logged, format!("the split is in the log: {row:?}")),
            (listed_on_new, "the new title's page lists it too".to_string()),
            (still == vec![odd], format!("decouple all of a grade leaves splits: {all:?}, new title holds {still:?}")),
            (back == t && holds == want, format!("undo puts the release back: into {back}, title holds {holds:?}")),
            (gone && blocked == 0, format!("the emptied title is gone and the pair may merge again: deleted {gone}, blocks {blocked}")),
            (again, "a split undone already is refused".to_string()),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} checks", checks.len())))
    })
    .await;
}

/// Several releases at once: split together into one new title, or moved to another title;
/// a title left with no releases is gone and its place in sets goes along.
async fn phase_bulk_release_moves(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "titles: several releases split together or moved at once", async {
        use romexplorer::api::titles::{ReleasesMove, ReleasesSplit};
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let ae = |e: romexplorer::api::Error| e.message;
        let app = romexplorer::api::App::with_pool(pool.clone(), env.paths.clone(), DatabaseConfig::default()).map_err(ae)?;
        let src = import::ensure_source(pool, "no-intro").await.map_err(de)?;
        let n3ds = Platform::Nintendo3ds.igdb_id();
        let title = |name: &'static str| async move {
            sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, ?, ?)").bind(name).bind(name.to_lowercase()).bind(import::sha1_hex(&format!("bulkmove/{name}"))).execute(pool).await.map(|r| r.last_insert_id())
        };
        let release = |t: u64, name: &'static str| async move {
            let norm = name.to_lowercase();
            let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (?, ?, ?, ?, ?, 'US', ?)")
                .bind(n3ds).bind(name).bind(import::sha1_hex(name)).bind(&norm).bind(import::sha1_hex(&norm)).bind(src)
                .execute(pool).await?.last_insert_id();
            sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(t).bind(r).execute(pool).await?;
            Ok::<u64, sqlx::Error>(r)
        };
        let t = title("Bulkmove Hoge Quest").await.map_err(e)?;
        let plain = release(t, "Bulkmove Hoge Quest (USA)").await.map_err(e)?;
        let special = release(t, "Bulkmove Hoge Quest - Special Edition (USA)").await.map_err(e)?;
        let limited = release(t, "Bulkmove Hoge Quest - Limited Edition (USA)").await.map_err(e)?;
        let holds = |title: u64| async move { sqlx::query_scalar::<_, u64>("SELECT release_id FROM title_releases WHERE title_id = ? ORDER BY release_id").bind(title).fetch_all(pool).await };
        let split = romexplorer::api::titles::split_releases(&app, &ReleasesSplit { release_ids: vec![special, limited] }).await.map_err(ae)?;
        let (old_holds, new_holds) = (holds(t).await.map_err(e)?, holds(split).await.map_err(e)?);
        let name: String = sqlx::query_scalar("SELECT name FROM title WHERE id = ?").bind(split).fetch_one(pool).await.map_err(e)?;
        // A set holding the old title as an entry; then every release moves away from it.
        let set = sqlx::query("INSERT INTO collection_sets (name) VALUES ('Bulkmove set')").execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO collection_set_entries (set_id, title_id, position) VALUES (?, ?, 1)").bind(set).bind(t).execute(pool).await.map_err(e)?;
        let moved = romexplorer::api::titles::move_releases(&app, &ReleasesMove { release_ids: vec![plain], title_id: split }).await.map_err(ae)?;
        let all = holds(split).await.map_err(e)?;
        let gone: bool = sqlx::query_scalar("SELECT deleted FROM title WHERE id = ?").bind(t).fetch_one(pool).await.map_err(e)?;
        let entry: Option<u64> = sqlx::query_scalar("SELECT title_id FROM collection_set_entries WHERE set_id = ?").bind(set).fetch_optional(pool).await.map_err(e)?.flatten();
        let refused = romexplorer::api::titles::move_releases(&app, &ReleasesMove { release_ids: vec![plain, 999_999_999], title_id: split }).await.is_err();
        let mut want_all = vec![plain, special, limited];
        want_all.sort_unstable();
        let checks = [
            (old_holds == vec![plain] && new_holds == vec![special, limited], format!("both go to one new title: old {old_holds:?}, new {new_holds:?}")),
            (name.contains("Edition"), format!("named after its releases: {name:?}")),
            (moved == 1 && all == want_all, format!("the last one moves over: {moved} moved, new title holds {all:?}")),
            (gone && entry == Some(split), format!("the emptied title is gone and its set entry follows: deleted {gone}, entry {entry:?}")),
            (refused, "a release that does not exist is refused".to_string()),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} checks", checks.len())))
    })
    .await;
}

/// A copy holding two dumps of one file (a headered and a headerless cartridge a scan
/// joined, in two folders): a copy takes one file, the one in the priority directory, and
/// the set's size on disk counts that one.
async fn phase_copy_priority_file(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "copy: one file per release, from the priority directory", async {
        use romexplorer::api::sets::{self, EntryKind};
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let ae = |e: romexplorer::api::Error| e.message;
        let app = romexplorer::api::App::with_pool(pool.clone(), env.paths.clone(), DatabaseConfig::default()).map_err(ae)?;
        let src = import::ensure_source(pool, "no-intro").await.map_err(de)?;
        let nes = Platform::Nes.igdb_id();
        let mut dirs = Vec::new();
        for (path, priority) in [("/prioritytest/Headered", 2), ("/prioritytest/Headerless", 1)] {
            dirs.push(sqlx::query("INSERT INTO local_directories (path, path_hash, is_collection_root, priority) VALUES (?, ?, TRUE, ?)").bind(path).bind(import::sha1_hex(path)).bind(priority).execute(pool).await.map_err(e)?.last_insert_id());
        }
        let name = "Prioritytest Hoge Quest (USA)";
        let t = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES ('Prioritytest Hoge Quest', 'prioritytest hoge quest', ?)").bind(import::sha1_hex("prioritytest hoge quest")).execute(pool).await.map_err(e)?.last_insert_id();
        let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (?, ?, ?, 'prioritytest hoge quest', ?, 'US', ?)")
            .bind(nes).bind(name).bind(import::sha1_hex(name)).bind(import::sha1_hex("prioritytest hoge quest")).bind(src)
            .execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(t).bind(r).execute(pool).await.map_err(e)?;
        // One rom, two files of one name: headered (in the folder second in priority) first.
        let mut files = Vec::new();
        for (dir, size) in [(dirs[0], 524_304u64), (dirs[1], 524_288)] {
            let c = sqlx::query("INSERT INTO content (file_name, size) VALUES (?, ?)").bind(format!("{name}.nes")).bind(size).execute(pool).await.map_err(e)?.last_insert_id();
            let dat_name = if dir == dirs[0] { format!("{name}.nes") } else { format!("{name}.unh") };
            sqlx::query("INSERT INTO release_contents (release_id, content_id, file_name, source_id) VALUES (?, ?, ?, ?)").bind(r).bind(c).bind(dat_name).bind(src).execute(pool).await.map_err(e)?;
            let folder = if dir == dirs[0] { "/prioritytest/Headered" } else { "/prioritytest/Headerless" };
            let path = format!("{folder}/{name}.nes");
            files.push(sqlx::query("INSERT INTO local_files (path, path_hash, content_id, directory_id, size) VALUES (?, ?, ?, ?, ?)").bind(&path).bind(import::sha1_hex(&path)).bind(c).bind(dir).bind(size).execute(pool).await.map_err(e)?.last_insert_id());
        }
        let main = format!("/prioritytest/Headered/{name}.nes");
        let rom = sqlx::query("INSERT INTO local_rom (main_path, app_platform_id) VALUES (?, ?)").bind(&main).bind(nes).execute(pool).await.map_err(e)?.last_insert_id();
        for f in &files {
            sqlx::query("INSERT INTO local_rom_files (rom_id, file_id) VALUES (?, ?)").bind(rom).bind(f).execute(pool).await.map_err(e)?;
        }
        let game = sqlx::query("INSERT INTO local_game (name) VALUES (?)").bind(name).execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO local_game_roms (game_id, rom_id) VALUES (?, ?)").bind(game).bind(rom).execute(pool).await.map_err(e)?;
        sqlx::query("INSERT INTO local_release_games (release_id, game_id) VALUES (?, ?)").bind(r).bind(game).execute(pool).await.map_err(e)?;
        let set = sets::create(&app, &sets::SetSpec { name: "Prioritytest set".into(), description: None }).await.map_err(ae)?;
        sets::add_entries(&app, set.id, EntryKind::Title, &[t.to_string()]).await.map_err(ae)?;

        let dest = crate_dir().join("tests").join("output").join("prioritytest-copy");
        let _ = std::fs::create_dir_all(&dest);
        let request = romexplorer_types::copy::CopyFilesRequest {
            selection: romexplorer_types::copy::CopySelection { set_ids: vec![set.id], ..Default::default() },
            mode: romexplorer_types::copy::CopyMode::ToExternal,
            destination: dest.display().to_string(),
            folder_scheme: None,
            output: None,
            source_preference: Default::default(),
            on_conflict: Default::default(),
            titles_all_releases: false,
            conflict_choices: Vec::new(),
            upload_source_id: None,
            export_artwork: Vec::new(),
            verify: Some(false),
        };
        let copied = |p: &romexplorer::api::copy::plan::Planned| p.files.iter().filter(|f| f.skip.is_none()).map(|f| f.source.file_id).collect::<Vec<u64>>();
        let first = romexplorer::api::copy::plan::plan(&app, &request).await.map_err(ae)?;
        let size_first = sets::get(&app, set.id).await.map_err(ae)?.total_size;
        // The other folder first now.
        sqlx::query("UPDATE local_directories SET priority = IF(id = ?, 1, 2) WHERE id IN (?, ?)").bind(dirs[0]).bind(dirs[0]).bind(dirs[1]).execute(pool).await.map_err(e)?;
        let second = romexplorer::api::copy::plan::plan(&app, &request).await.map_err(ae)?;
        let size_second = sets::get(&app, set.id).await.map_err(ae)?.total_size;
        // Config > Copy says headerless: that one, whichever folder comes first.
        let pref = |p: romexplorer_types::copy::HeaderPreference| romexplorer_types::copy::DefaultsUpdate { header_preference: Some(p), ..Default::default() };
        romexplorer::api::copy::set_defaults(&app, &pref(romexplorer_types::copy::HeaderPreference::Headerless)).await.map_err(ae)?;
        let third = romexplorer::api::copy::plan::plan(&app, &request).await.map_err(ae)?;
        let size_third = sets::get(&app, set.id).await.map_err(ae)?.total_size;
        romexplorer::api::copy::set_defaults(&app, &pref(romexplorer_types::copy::HeaderPreference::Priority)).await.map_err(ae)?;
        let _ = std::fs::remove_dir_all(&dest);
        let (a, b, c) = (copied(&first), copied(&second), copied(&third));

        // The same release on a main server, in the server's own folder order: the set's size
        // on the server counts the server's priority file, and nothing is on disk for it here.
        let had_main: Option<u16> = sqlx::query_scalar("SELECT id FROM remote_sources WHERE role = 'main' AND deleted = FALSE").fetch_optional(pool).await.map_err(e)?;
        let main = match had_main {
            Some(id) => id,
            None => sqlx::query("INSERT INTO remote_sources (name, base_url, role) VALUES ('Prioritytest server', 'http://prioritytest.invalid:1', 'main')").execute(pool).await.map_err(e)?.last_insert_id() as u16,
        };
        let r2 = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (?, 'Prioritytest Fuga Saga (USA)', ?, 'prioritytest fuga saga', ?, 'US', ?)")
            .bind(nes).bind(import::sha1_hex("Prioritytest Fuga Saga (USA)")).bind(import::sha1_hex("prioritytest fuga saga")).bind(src)
            .execute(pool).await.map_err(e)?.last_insert_id();
        sets::add_entries(&app, set.id, EntryKind::Release, &[r2.to_string()]).await.map_err(ae)?;
        for (id, path, priority) in [(9_101u64, "/srv/Headered", 1), (9_102, "/srv/Headerless", 2)] {
            sqlx::query("INSERT INTO remote_directories (source_id, id, path, path_hash, is_collection_root, priority) VALUES (?, ?, ?, ?, TRUE, ?)").bind(main).bind(id).bind(path).bind(import::sha1_hex(path)).bind(priority).execute(pool).await.map_err(e)?;
        }
        sqlx::query("INSERT INTO remote_rom (source_id, id, app_platform_id, main_path) VALUES (?, 9301, ?, '/srv/Headered/Fuga.nes')").bind(main).bind(nes).execute(pool).await.map_err(e)?;
        for (fid, dir, size, path) in [(9_201u64, 9_101u64, 700u64, "/srv/Headered/Fuga.nes"), (9_202, 9_102, 600, "/srv/Headerless/Fuga.nes")] {
            let c = sqlx::query("INSERT INTO content (file_name, size) VALUES ('Fuga.nes', ?)").bind(size).execute(pool).await.map_err(e)?.last_insert_id();
            sqlx::query("INSERT INTO remote_files (source_id, id, path, path_hash, content_id, directory_id, size) VALUES (?, ?, ?, ?, ?, ?, ?)").bind(main).bind(fid).bind(path).bind(import::sha1_hex(path)).bind(c).bind(dir).bind(size).execute(pool).await.map_err(e)?;
            sqlx::query("INSERT INTO remote_rom_files (source_id, rom_id, file_id) VALUES (?, 9301, ?)").bind(main).bind(fid).execute(pool).await.map_err(e)?;
        }
        sqlx::query("INSERT INTO remote_game (source_id, id, name) VALUES (?, 9401, 'Fuga')").bind(main).execute(pool).await.map_err(e)?;
        sqlx::query("INSERT INTO remote_game_roms (source_id, game_id, rom_id) VALUES (?, 9401, 9301)").bind(main).execute(pool).await.map_err(e)?;
        sqlx::query("INSERT INTO remote_release_games (source_id, release_id, game_id) VALUES (?, ?, 9401)").bind(main).bind(r2).execute(pool).await.map_err(e)?;
        let with_server = sets::get(&app, set.id).await.map_err(ae)?;
        let server = (with_server.server_size, with_server.server_size_unknown, with_server.size_unknown);
        // Leave no main server behind for the phases after this one.
        for t in ["remote_release_games", "remote_game_roms", "remote_rom_files", "remote_game", "remote_rom", "remote_files", "remote_directories"] {
            sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {t} WHERE source_id = ?"))).bind(main).execute(pool).await.map_err(e)?;
        }
        if had_main.is_none() {
            sqlx::query("DELETE FROM remote_sources WHERE id = ?").bind(main).execute(pool).await.map_err(e)?;
        }
        let checks = [
            (a == vec![files[1]] && first.view.renamed.is_empty(), format!("one file, the priority folder's (headerless): copied {a:?}, renamed {:?}", first.view.renamed)),
            (b == vec![files[0]], format!("the order changed, so the other one: {b:?}")),
            (size_first == 524_288 && size_second == 524_304, format!("the set's size on disk counts that one file: {size_first} then {size_second}")),
            (c == vec![files[1]] && size_third == 524_288, format!("headerless asked for: that one, whatever the folder order: {c:?}, size {size_third}")),
            // The server holds only the second release, its Headered folder first (700); the
            // first release is missing there.
            (server == (Some(700), 1, 1), format!("size on the server: its priority file, the release it lacks left out; nothing on disk here for the new one: {server:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} checks", checks.len())))
    })
    .await;
}

/// RetroAchievements lists the WonderSwan Color under the WonderSwan (and the Pocket Color
/// under the Neo Geo Pocket): those releases move to the colour model, folding into the one it
/// already lists under the same name.
async fn phase_color_handhelds(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "platforms: colour handheld games filed under the original model move", async {
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let ra = import::ensure_source(pool, "retroachievements").await.map_err(de)?;
        let ni = import::ensure_source(pool, "no-intro").await.map_err(de)?;
        let (ws, wsc, ngp, ngpc) = (Platform::WonderSwan.igdb_id(), Platform::WonderSwanColor.igdb_id(), Platform::NeoGeoPocket.igdb_id(), Platform::NeoGeoPocketColor.igdb_id());
        let release = |platform: u16, name: &'static str, file: &'static str, src: u8| async move {
            let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (?, ?, ?, ?, ?, 'JP', ?)")
                .bind(platform).bind(name).bind(import::sha1_hex(name)).bind(name.to_lowercase()).bind(import::sha1_hex(&name.to_lowercase())).bind(src)
                .execute(pool).await?.last_insert_id();
            let c = sqlx::query("INSERT INTO content (file_name, size) VALUES (?, 4096)").bind(file).execute(pool).await?.last_insert_id();
            sqlx::query("INSERT INTO release_contents (release_id, content_id, file_name, source_id) VALUES (?, ?, ?, ?)").bind(r).bind(c).bind(file).bind(src).execute(pool).await?;
            let t = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, ?, ?)").bind(name).bind(name.to_lowercase()).bind(import::sha1_hex(&format!("colortest/{r}"))).execute(pool).await?.last_insert_id();
            sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(t).bind(r).execute(pool).await?;
            Ok::<(u64, u64), sqlx::Error>((r, t))
        };
        // RA's copy of a colour game the colour DAT also lists; one it alone lists; a mono game.
        let (ra_twin, ra_twin_title) = release(ws, "Colortest Hoge Quest (Japan)", "Colortest Hoge Quest (Japan).wsc", ra).await.map_err(e)?;
        let (ni_twin, _) = release(wsc, "Colortest Hoge Quest (Japan)", "Colortest Hoge Quest (Japan).wsc", ni).await.map_err(e)?;
        let (ra_alone, _) = release(ws, "Colortest Fuga Saga (Japan)", "Colortest Fuga Saga (Japan).wsc", ra).await.map_err(e)?;
        let (mono, _) = release(ws, "Colortest Piyo (Japan)", "Colortest Piyo (Japan).ws", ni).await.map_err(e)?;
        let (pocket, _) = release(ngp, "Colortest Pocket Hoge (Japan)", "Colortest Pocket Hoge (Japan).ngc", ra).await.map_err(e)?;
        sqlx::query("DELETE FROM data_migrations WHERE name = 'color_handhelds_0062'").execute(pool).await.map_err(e)?;
        let (moved, folded) = database::release_merge::move_color_releases_once(pool).await.map_err(de)?;
        let platform = |id: u64| async move { sqlx::query_scalar::<_, u16>("SELECT app_platform_id FROM `release` WHERE id = ?").bind(id).fetch_optional(pool).await };
        let twin_gone = platform(ra_twin).await.map_err(e)?.is_none();
        let titles_of_twin: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title_releases WHERE release_id = ?").bind(ni_twin).fetch_one(pool).await.map_err(e)?;
        let old_title_gone: bool = sqlx::query_scalar("SELECT deleted FROM title WHERE id = ?").bind(ra_twin_title).fetch_one(pool).await.map_err(e)?;
        let places = (platform(ra_alone).await.map_err(e)?, platform(mono).await.map_err(e)?, platform(pocket).await.map_err(e)?);
        let checks = [
            (moved >= 2 && folded >= 1, format!("moved {moved}, folded {folded}")),
            (twin_gone && titles_of_twin == 1 && old_title_gone, format!("RA's copy folds into the colour DAT's release, which keeps its one title: gone {twin_gone}, titles {titles_of_twin}, old title deleted {old_title_gone}")),
            (places == (Some(wsc), Some(ws), Some(ngpc)), format!("alone it moves, a mono game stays, the Pocket Color too: {places:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} checks", checks.len())))
    })
    .await;
}

/// A DAT imported before its system had a platform here left its releases on none: imported
/// again they go onto the platform and the old rows fold in. A DAT whose games split between
/// two models is recorded as the system its name says. A rom linked to a release with a
/// platform, however it was linked, takes it.
async fn phase_platformless_dats(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "platforms: DATs with no platform before, and roms that kept none", async {
        use romexplorer::database::dat_parse;
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let game = "<game name=\"Nonetest Hoge Quest (1985)(Hoge Soft)\"><rom name=\"Nonetest Hoge Quest (1985)(Hoge Soft).d64\" size=\"8\" sha1=\"00000000000000000000000000000000000000e1\"/></game>";
        // As an older build read it: a name it did not know.
        let before = format!("<datafile><header><name>Nonetest Unknown Machine - Games - [D64]</name></header>{game}</datafile>");
        import::import_dat(pool, Source::Tosec, "nonetest-c64.dat", &dat_parse::parse_dat_xml(&before).map_err(de)?, &ImportOptions::default()).await.map_err(de)?;
        let on = |platform: u16| async move { sqlx::query_scalar::<_, u64>("SELECT id FROM `release` WHERE name = 'Nonetest Hoge Quest (1985)(Hoge Soft)' AND app_platform_id = ? AND deleted = FALSE").bind(platform).fetch_optional(pool).await };
        let old = on(0).await.map_err(e)?.ok_or("the first import put it on no platform")?;
        // A scanned copy of it, linked while it was on no platform.
        let path = "/nonetest/Nonetest Hoge Quest (1985)(Hoge Soft).d64";
        let content: u64 = sqlx::query_scalar("SELECT content_id FROM release_contents WHERE release_id = ?").bind(old).fetch_one(pool).await.map_err(e)?;
        let file = sqlx::query("INSERT INTO local_files (path, path_hash, content_id, size) VALUES (?, ?, ?, 8)").bind(path).bind(import::sha1_hex(path)).bind(content).execute(pool).await.map_err(e)?.last_insert_id();
        let rom = sqlx::query("INSERT INTO local_rom (main_path, app_platform_id) VALUES (?, 0)").bind(path).execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO local_rom_files (rom_id, file_id) VALUES (?, ?)").bind(rom).bind(file).execute(pool).await.map_err(e)?;
        let g = sqlx::query("INSERT INTO local_game (name) VALUES ('Nonetest Hoge Quest')").execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO local_game_roms (game_id, rom_id) VALUES (?, ?)").bind(g).bind(rom).execute(pool).await.map_err(e)?;
        sqlx::query("INSERT INTO local_release_games (release_id, game_id) VALUES (?, ?)").bind(old).bind(g).execute(pool).await.map_err(e)?;
        // The same DAT as TOSEC names it, now that its system is known.
        let after = format!("<datafile><header><name>Commodore C64 - Games - [D64]</name></header>{game}</datafile>");
        let summary = import::import_dat(pool, Source::Tosec, "nonetest-c64.dat", &dat_parse::parse_dat_xml(&after).map_err(de)?, &ImportOptions::default()).await.map_err(de)?;
        let c64 = Platform::Commodore64.igdb_id();
        let new = on(c64).await.map_err(e)?;
        let old_left = on(0).await.map_err(e)?;
        let held: i64 = match new {
            Some(id) => sqlx::query_scalar("SELECT COUNT(*) FROM local_release_games WHERE release_id = ? AND game_id = ?").bind(id).bind(g).fetch_one(pool).await.map_err(e)?,
            None => 0,
        };
        let placed = import::confirm_rom_platforms(pool).await.map_err(de)?;
        let rom_platform: u16 = sqlx::query_scalar("SELECT app_platform_id FROM local_rom WHERE id = ?").bind(rom).fetch_one(pool).await.map_err(e)?;
        // RetroAchievements' WonderSwan DAT holds colour games too: still a WonderSwan DAT.
        let ra = "<datafile><header><name>RA - WonderSwan</name></header>\
            <game name=\"Nonetest Mono (Japan)\"><rom name=\"Nonetest Mono (Japan).ws\" size=\"8\" sha1=\"00000000000000000000000000000000000000e2\"/></game>\
            <game name=\"Nonetest Color (Japan)\"><rom name=\"Nonetest Color (Japan).wsc\" size=\"8\" sha1=\"00000000000000000000000000000000000000e3\"/></game></datafile>";
        import::import_dat(pool, Source::RetroAchievements, "nonetest-ra-ws.dat", &dat_parse::parse_dat_xml(ra).map_err(de)?, &ImportOptions::default()).await.map_err(de)?;
        let recorded: u16 = sqlx::query_scalar("SELECT app_platform_id FROM dat_imports WHERE file_name = 'nonetest-ra-ws.dat'").fetch_one(pool).await.map_err(e)?;
        let color: Option<u16> = sqlx::query_scalar("SELECT app_platform_id FROM `release` WHERE name = 'Nonetest Color (Japan)'").fetch_optional(pool).await.map_err(e)?;
        let checks = [
            (new.is_some() && old_left.is_none() && summary.platformless_folded == 1, format!("the old row folds into the Commodore 64 release: new {new:?}, left {old_left:?}, folded {}", summary.platformless_folded)),
            (held == 1, format!("the copy linked to the old row follows it: {held}")),
            (placed >= 1 && rom_platform == c64, format!("the rom takes the release's platform: {placed} placed, now {rom_platform}")),
            (recorded == Platform::WonderSwan.igdb_id() && color == Some(Platform::WonderSwanColor.igdb_id()), format!("a split DAT is recorded as its named system, its colour game on the colour model: {recorded}, {color:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} checks", checks.len())))
    })
    .await;
}

/// One game held in two formats in one copy (`.fds` + `.qd`): a copy takes the format first in
/// its family (Config > Copy), and the set's size counts that one.
async fn phase_copy_one_format(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "copy: one format of a game held in several", async {
        use romexplorer::api::sets::{self, EntryKind};
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let ae = |e: romexplorer::api::Error| e.message;
        let app = romexplorer::api::App::with_pool(pool.clone(), env.paths.clone(), DatabaseConfig::default()).map_err(ae)?;
        let src = import::ensure_source(pool, "no-intro").await.map_err(de)?;
        let fds = Platform::FamicomDiskSystem.igdb_id();
        let name = "Formattest Hoge Quest (Japan)";
        let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (?, ?, ?, 'formattest hoge quest', ?, 'JP', ?)")
            .bind(fds).bind(name).bind(import::sha1_hex(name)).bind(import::sha1_hex("formattest hoge quest")).bind(src)
            .execute(pool).await.map_err(e)?.last_insert_id();
        let mut files = Vec::new();
        for (ext, size) in [("qd", 131_000u64), ("fds", 65_500)] {
            let file = format!("{name}.{ext}");
            let c = sqlx::query("INSERT INTO content (file_name, size) VALUES (?, ?)").bind(&file).bind(size).execute(pool).await.map_err(e)?.last_insert_id();
            sqlx::query("INSERT INTO release_contents (release_id, content_id, file_name, source_id) VALUES (?, ?, ?, ?)").bind(r).bind(c).bind(&file).bind(src).execute(pool).await.map_err(e)?;
            let path = format!("/formattest/{file}");
            files.push(sqlx::query("INSERT INTO local_files (path, path_hash, content_id, size) VALUES (?, ?, ?, ?)").bind(&path).bind(import::sha1_hex(&path)).bind(c).bind(size).execute(pool).await.map_err(e)?.last_insert_id());
        }
        let rom = sqlx::query("INSERT INTO local_rom (main_path, app_platform_id) VALUES (?, ?)").bind(format!("/formattest/{name}.qd")).bind(fds).execute(pool).await.map_err(e)?.last_insert_id();
        for f in &files {
            sqlx::query("INSERT INTO local_rom_files (rom_id, file_id) VALUES (?, ?)").bind(rom).bind(f).execute(pool).await.map_err(e)?;
        }
        let game = sqlx::query("INSERT INTO local_game (name) VALUES (?)").bind(name).execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO local_game_roms (game_id, rom_id) VALUES (?, ?)").bind(game).bind(rom).execute(pool).await.map_err(e)?;
        sqlx::query("INSERT INTO local_release_games (release_id, game_id) VALUES (?, ?)").bind(r).bind(game).execute(pool).await.map_err(e)?;
        let set = sets::create(&app, &sets::SetSpec { name: "Formattest set".into(), description: None }).await.map_err(ae)?;
        sets::add_entries(&app, set.id, EntryKind::Release, &[r.to_string()]).await.map_err(ae)?;
        let dest = crate_dir().join("tests").join("output").join("formattest-copy");
        let _ = std::fs::create_dir_all(&dest);
        let request = romexplorer_types::copy::CopyFilesRequest {
            selection: romexplorer_types::copy::CopySelection { set_ids: vec![set.id], ..Default::default() },
            mode: romexplorer_types::copy::CopyMode::ToExternal,
            destination: dest.display().to_string(),
            folder_scheme: None,
            output: None,
            source_preference: Default::default(),
            on_conflict: Default::default(),
            titles_all_releases: false,
            conflict_choices: Vec::new(),
            upload_source_id: None,
            export_artwork: Vec::new(),
            verify: Some(false),
        };
        let plan = romexplorer::api::copy::plan::plan(&app, &request).await.map_err(ae)?;
        let copied: Vec<u64> = plan.files.iter().filter(|f| f.skip.is_none()).map(|f| f.source.file_id).collect();
        let size = sets::get(&app, set.id).await.map_err(ae)?.total_size;
        // The user turns it round: qd first.
        romexplorer::api::copy::set_defaults(&app, &romexplorer_types::copy::DefaultsUpdate { format_order: Some(vec![vec!["qd".into(), "fds".into()]]), ..Default::default() }).await.map_err(ae)?;
        let turned: Vec<u64> = romexplorer::api::copy::plan::plan(&app, &request).await.map_err(ae)?.files.iter().filter(|f| f.skip.is_none()).map(|f| f.source.file_id).collect();
        romexplorer::api::copy::set_defaults(&app, &romexplorer_types::copy::DefaultsUpdate { format_order: Some(romexplorer_types::copy::default_format_order()), ..Default::default() }).await.map_err(ae)?;
        let _ = std::fs::remove_dir_all(&dest);
        let checks = [
            (copied == vec![files[1]], format!("the .fds only: {copied:?}")),
            (size == 65_500, format!("the set's size counts the .fds: {size}")),
            (turned == vec![files[0]], format!("qd first when the user says so: {turned:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} checks", checks.len())))
    })
    .await;
}

/// A release's display: the system's default where nothing is recorded (PAL 640x512i / 50 Hz),
/// a source's value over it, the user's over that, the user's taken back to the source's; the
/// rows follow a release merge. And a set's rows say which title or game entry they came from.
async fn phase_display_data(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "display: defaults, sources, the user's value, merges; set rows' entries", async {
        use romexplorer::api::display::{self, DisplaySet};
        use romexplorer::api::sets::{self, EntryKind};
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let ae = |e: romexplorer::api::Error| e.message;
        let app = romexplorer::api::App::with_pool(pool.clone(), env.paths.clone(), DatabaseConfig::default()).map_err(ae)?;
        let src = import::ensure_source(pool, "redump").await.map_err(de)?;
        let ps2 = Platform::PlayStation2.igdb_id();
        let release = |name: &'static str, region: &'static str| async move {
            sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (?, ?, ?, ?, ?, ?, ?)")
                .bind(ps2).bind(name).bind(import::sha1_hex(name)).bind(name.to_lowercase()).bind(import::sha1_hex(&name.to_lowercase())).bind(region).bind(src)
                .execute(pool).await.map(|r| r.last_insert_id())
        };
        let usa = release("Disptest Hoge Quest (USA)", "US").await.map_err(e)?;
        let europe = release("Disptest Hoge Quest (Europe)", "EU").await.map_err(e)?;
        let get = |id: u64, region: &'static str| { let app = &app; async move { display::values(app, id, ps2, Some(region)).await } };
        let field = |vals: &[display::DisplayValue], f: &str| vals.iter().find(|v| v.field == f).map(|v| (v.value.clone(), v.state.clone(), v.source.clone()));
        let us0 = get(usa, "US").await.map_err(ae)?;
        let eu0 = get(europe, "EU").await.map_err(ae)?;
        // A source says 480p/1080i; then the user says 640x448i only; then takes it back.
        display::record(&app, usa, "modes", "sfo", "720x480p,1920x1080i").await.map_err(ae)?;
        let with_source = get(usa, "US").await.map_err(ae)?;
        display::set(&app, &DisplaySet { release_ids: vec![usa], field: "modes".into(), value: Some("640X448I".into()) }).await.map_err(ae)?;
        let with_user = get(usa, "US").await.map_err(ae)?;
        display::set(&app, &DisplaySet { release_ids: vec![usa], field: "modes".into(), value: None }).await.map_err(ae)?;
        let taken_back = get(usa, "US").await.map_err(ae)?;
        let refused = display::set(&app, &DisplaySet { release_ids: vec![usa], field: "modes".into(), value: Some("480p".into()) }).await.is_err();
        // Merged into the European release, the USA one's rows go along.
        display::set(&app, &DisplaySet { release_ids: vec![usa], field: "aspect".into(), value: Some("16:9".into()) }).await.map_err(ae)?;
        database::release_merge::fold_pairs(pool, &[(usa, europe)]).await.map_err(de)?;
        let merged = get(europe, "EU").await.map_err(ae)?;

        // A set's rows: which title or game entry put them there.
        let t = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES ('Disptest Fuga', 'disptest fuga', ?)").bind(import::sha1_hex("disptest/fuga")).execute(pool).await.map_err(e)?.last_insert_id();
        let fuga = release("Disptest Fuga (Japan)", "JP").await.map_err(e)?;
        sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(t).bind(fuga).execute(pool).await.map_err(e)?;
        let set = sets::create(&app, &sets::SetSpec { name: "Disptest set".into(), description: None }).await.map_err(ae)?;
        sets::add_entries(&app, set.id, EntryKind::Title, &[t.to_string()]).await.map_err(ae)?;
        let rows = sets::releases(&app, set.id, &romexplorer::api::catalog::ListQuery { view: Some("collection".into()), ..Default::default() }).await.map_err(ae)?.items;
        let entry = rows.iter().find(|r| r.id == fuga).map(|r| (r.set_via.clone(), r.set_title_id));

        // The lists: filtered by the same values the page shows, recorded or default.
        use romexplorer::api::catalog::{self, ListQuery};
        let ids = |q: ListQuery| {
            let app = &app;
            async move { catalog::releases(app, &ListQuery { search: Some("Disptest".into()), view: Some("collection".into()), ..q }).await.map(|p| p.items) }
        };
        let wide = ids(ListQuery { display_aspect: Some("16:9".into()), ..Default::default() }).await.map_err(ae)?;
        let ntsc_i = ids(ListQuery { display_res: Some("640x448i".into()), ..Default::default() }).await.map_err(ae)?;
        let undecided = ids(ListQuery { display_state: Some("aspect:default".into()), ..Default::default() }).await.map_err(ae)?;
        let all = ids(ListQuery { sort: Some("resolutions".into()), ..Default::default() }).await.map_err(ae)?;
        let fuga_row = all.iter().find(|r| r.id == fuga).map(|r| (r.display_aspect.clone(), r.display_modes.clone(), r.display_refresh.clone()));
        let fuga_page = get(fuga, "JP").await.map_err(ae)?;
        let titles = catalog::titles(&app, &ListQuery { search: Some("Disptest Fuga".into()), view: Some("collection".into()), display_aspect: Some("4:3".into()), ..Default::default() }).await.map_err(ae)?.items;
        let bad_filter = ids(ListQuery { display_scan: Some("x".into()), ..Default::default() }).await.is_err();
        // Every list takes them: the cross-platform and games lists too, and titles sorted.
        let every = ListQuery { display_aspect: Some("4:3".into()), display_res: Some("640x448i".into()), display_scan: Some("i".into()), display_refresh: Some("60".into()), display_widescreen: Some("none".into()), display_state: Some("modes:default".into()), ..Default::default() };
        let others = [
            catalog::crossplatform(&app, &every).await.map(|_| ()),
            catalog::games(&app, &every).await.map(|_| ()),
            catalog::titles(&app, &ListQuery { sort: Some("resolutions".into()), ..every.clone() }).await.map(|_| ()),
            catalog::releases(&app, &ListQuery { sort: Some("refresh".into()), desc: true, ..every.clone() }).await.map(|_| ()),
        ];
        let others_ok = others.iter().all(|r| r.is_ok());
        let others_err = others.iter().filter_map(|r| r.as_ref().err().map(|e| e.message.clone())).collect::<Vec<_>>();

        let checks = [
            (field(&us0, "modes") == Some((Some("640x448i".into()), "default".into(), None)) && field(&us0, "refresh").map(|x| x.0) == Some(Some("60".into())), format!("NTSC default: {us0:?}")),
            (us0.iter().find(|v| v.field == "modes").is_some_and(|v| v.system.iter().any(|r| r == "720x480p")), format!("the system's modes beside the default: {us0:?}")),
            (field(&eu0, "modes").map(|x| x.0) == Some(Some("640x512i".into())) && field(&eu0, "refresh").map(|x| x.0) == Some(Some("50".into())), format!("PAL default: {eu0:?}")),
            (field(&with_source, "modes") == Some((Some("720x480p,1920x1080i".into()), "confirmed".into(), Some("sfo".into()))), format!("a source's value over the default: {with_source:?}")),
            (field(&with_user, "modes") == Some((Some("640x448i".into()), "confirmed".into(), Some("user".into()))), format!("the user's over the source's: {with_user:?}")),
            (field(&taken_back, "modes").map(|x| x.2) == Some(Some("sfo".into())), format!("taken back, the source's again: {taken_back:?}")),
            (refused, "a value outside the vocabulary is refused".to_string()),
            (field(&merged, "aspect") == Some((Some("16:9".into()), "confirmed".into(), Some("user".into()))), format!("the rows follow a merge: {merged:?}")),
            (entry == Some((Some("title".into()), Some(t))), format!("a set row names its title entry: {entry:?}")),
            (wide.iter().any(|r| r.id == europe) && !wide.iter().any(|r| r.id == fuga), "filtered by the user's 16:9".to_string()),
            (ntsc_i.iter().any(|r| r.id == fuga) && !ntsc_i.iter().any(|r| r.id == europe), "filtered by a default resolution".to_string()),
            (undecided.iter().any(|r| r.id == fuga) && !undecided.iter().any(|r| r.id == europe), "aspect still the default".to_string()),
            (
                fuga_row == Some((field(&fuga_page, "aspect").and_then(|x| x.0), field(&fuga_page, "modes").and_then(|x| x.0), field(&fuga_page, "refresh").and_then(|x| x.0))),
                format!("the list's columns are the page's values: {fuga_row:?} / {fuga_page:?}"),
            ),
            (titles.iter().any(|x| x.id == t), "a title filtered by its release's aspect".to_string()),
            (bad_filter, "a bad display filter is refused".to_string()),
            (others_ok, format!("every list runs with every display filter: {others_err:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} checks", checks.len())))
    })
    .await;
}

/// A release split off into a title of its own: it takes its own IGDB game along, the old
/// title keeps the rest, and the two are not merged back by a later IGDB pass.
async fn phase_split_release(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "titles: a release split into its own title stays apart", async {
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let ae = |e: romexplorer::api::Error| e.message;
        let app = romexplorer::api::App::with_pool(pool.clone(), env.paths.clone(), DatabaseConfig::default()).map_err(ae)?;
        let src = import::ensure_source(pool, "no-intro").await.map_err(de)?;
        let n3ds = Platform::Nintendo3ds.igdb_id();
        let t = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES ('Spltest Hoge Quest', 'spltest hoge quest', ?)").bind(import::sha1_hex("spltest hoge quest")).execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO title_provider_links (title_id, provider, provider_id, confidence) VALUES (?, 'igdb', '886001', 'manual')").bind(t).execute(pool).await.map_err(e)?;
        let mut releases = Vec::new();
        // Combined by hand: each release carries its own game.
        for (name, igdb) in [("Spltest Hoge Quest (USA)", "886001"), ("Spltest Hoge Quest - Special Edition (USA)", "886002")] {
            let norm = name.to_lowercase();
            let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (?, ?, ?, ?, ?, 'US', ?)")
                .bind(n3ds).bind(name).bind(import::sha1_hex(name)).bind(&norm).bind(import::sha1_hex(&norm)).bind(src)
                .execute(pool).await.map_err(e)?.last_insert_id();
            sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(t).bind(r).execute(pool).await.map_err(e)?;
            sqlx::query("INSERT INTO release_provider_links (release_id, provider, provider_id, confidence) VALUES (?, 'igdb', ?, 'manual')").bind(r).bind(igdb).execute(pool).await.map_err(e)?;
            releases.push(r);
        }
        let special = releases[1];
        let split = romexplorer::api::titles::split_release(&app, special).await.map_err(ae)?;
        let holds = |title: u64| async move { sqlx::query_scalar::<_, u64>("SELECT release_id FROM title_releases WHERE title_id = ? ORDER BY release_id").bind(title).fetch_all(pool).await };
        let (old_holds, new_holds) = (holds(t).await.map_err(e)?, holds(split).await.map_err(e)?);
        let igdb = |title: u64| async move { sqlx::query_scalar::<_, String>("SELECT provider_id FROM title_provider_links WHERE title_id = ? AND provider = 'igdb' AND deleted = FALSE").bind(title).fetch_optional(pool).await };
        let games = (igdb(t).await.map_err(e)?, igdb(split).await.map_err(e)?);
        let again = romexplorer::api::titles::split_release(&app, special).await.map_err(ae)?;
        // Even linked to the same game later, the IGDB pass leaves them apart.
        sqlx::query("UPDATE title_provider_links SET provider_id = '886001' WHERE title_id = ? AND provider = 'igdb'").bind(split).execute(pool).await.map_err(e)?;
        database::title_merge::dedupe_by_igdb(pool, Some("886001"), &jobs::Progress::none()).await.map_err(de)?;
        let alive: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title WHERE id IN (?, ?) AND deleted = FALSE").bind(t).bind(split).fetch_one(pool).await.map_err(e)?;
        let checks = [
            (split != t && old_holds == vec![releases[0]] && new_holds == vec![special], format!("the release moves to a new title: old holds {old_holds:?}, new {split} holds {new_holds:?}")),
            (games == (Some("886001".into()), Some("886002".into())), format!("each title keeps its own game: {games:?}")),
            (again == split, format!("splitting a release already alone changes nothing: {again} vs {split}")),
            (alive == 2, format!("the IGDB pass does not merge them back: {alive} alive")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} checks", checks.len())))
    })
    .await;
}

/// A title's four sizes: held as a CHD, its size on disk is the CHD's and its catalogue size
/// the raw dump's -- the two "first release" numbers are not the same thing.
/// TheGamesDB lists each regional release as its own game: same-named entries go to the
/// releases of their own region, not all to whichever came last; a link that fits the
/// release's region better is not swapped for a worse one; a stronger (manual) link stays.
async fn phase_thegamesdb_regions(env: &Env, report: &mut Report) {
    use romexplorer::database::providers::thegamesdb_region::{link_platform, Candidate, TgdbRegions};
    let pool = &env.pool;
    step(report, "TheGamesDB: each release links the entry of its own region", async {
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let gb = Platform::GameBoy.igdb_id();
        let norm = romexplorer::database::matching::util::norm_key("Tgdbtest Hoge Quest");
        let mut releases = Vec::new();
        for (name, region) in [("Tgdbtest Hoge Quest (USA)", "US"), ("Tgdbtest Hoge Quest (Japan)", "JP"), ("Tgdbtest Hoge Quest (Europe)", "EU"), ("Tgdbtest Hoge Quest (World)", "WORLD")] {
            let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region) VALUES (?, ?, ?, ?, ?, ?)")
                .bind(gb).bind(name).bind(import::sha1_hex(name)).bind(&norm).bind(import::sha1_hex(&norm)).bind(region)
                .execute(pool).await.map_err(e)?.last_insert_id();
            releases.push(r);
        }
        let (usa, japan, europe, world) = (releases[0], releases[1], releases[2], releases[3]);
        // Japan's entry says PAL as its region, Japan as its country: the country decides.
        let regions = TgdbRegions::from_lists(&[(1, "NTSC-U".into()), (6, "PAL".into())], &[(28, "Japan".into()), (50, "USA".into()), (13, "United Kingdom".into())]);
        for (id, region, country) in [(990001i64, 1u32, 50u32), (990002, 6, 28), (990003, 6, 13)] {
            sqlx::query("INSERT INTO thegamesdb_games (provider_id, title, app_platform_id, region_id, country_id) VALUES (?, 'Tgdbtest Hoge Quest', ?, ?, ?)").bind(id).bind(gb).bind(region).bind(country).execute(pool).await.map_err(e)?;
        }
        let candidate = |id: &str, region: i64, country: i64| Candidate { id: id.to_string(), names: vec!["Tgdbtest Hoge Quest".to_string()], region_id: Some(region), country_id: Some(country) };
        let priority: Vec<String> = ["US", "WORLD", "EU", "JP"].iter().map(|s| s.to_string()).collect();
        let link = |release: u64| async move { sqlx::query_scalar::<_, String>("SELECT provider_id FROM release_provider_links WHERE release_id = ? AND provider = 'thegamesdb' AND deleted = FALSE").bind(release).fetch_optional(pool).await };
        // The Japanese entry last, as a dump in id order would have it.
        let all = [candidate("990001", 1, 50), candidate("990003", 6, 13), candidate("990002", 6, 28)];
        link_platform(pool, &regions, &priority, Platform::GameBoy, &all).await.map_err(de)?;
        let first = (link(usa).await.map_err(e)?, link(japan).await.map_err(e)?, link(europe).await.map_err(e)?, link(world).await.map_err(e)?);
        // A later pass that only saw the US entry (a partial sync) leaves Japan alone.
        link_platform(pool, &regions, &priority, Platform::GameBoy, &[candidate("990001", 1, 50)]).await.map_err(de)?;
        let japan_after_partial = link(japan).await.map_err(e)?;
        // A manual link is never moved.
        sqlx::query("UPDATE release_provider_links SET provider_id = '990001', confidence = 'manual' WHERE release_id = ? AND provider = 'thegamesdb'").bind(europe).execute(pool).await.map_err(e)?;
        link_platform(pool, &regions, &priority, Platform::GameBoy, &all).await.map_err(de)?;
        let europe_manual = link(europe).await.map_err(e)?;
        let want = (Some("990001".to_string()), Some("990002".to_string()), Some("990003".to_string()), Some("990001".to_string()));
        let checks = [
            (first == want, format!("USA, Japan, Europe (a UK entry), World (the first in region priority): {first:?}")),
            (japan_after_partial.as_deref() == Some("990002"), format!("a partial pass keeps Japan's own: {japan_after_partial:?}")),
            (europe_manual.as_deref() == Some("990001"), format!("the manual link stays: {europe_manual:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), "4 releases, 3 entries: each its own region".to_string()))
    })
    .await;
    step(report, "TheGamesDB: a selection is linked from the cached dump, nothing else", async {
        use romexplorer::database::providers::thegamesdb_region::link_releases;
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let gb = Platform::GameBoy.igdb_id();
        let norm = romexplorer::database::matching::util::norm_key("Tgdbtest Piyo Quest");
        let mut releases = Vec::new();
        for (name, region) in [("Tgdbtest Piyo Quest (Japan)", "JP"), ("Tgdbtest Piyo Quest (USA)", "US"), ("Tgdbtest Piyo Quest (Europe)", "EU")] {
            let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region) VALUES (?, ?, ?, ?, ?, ?)")
                .bind(gb).bind(name).bind(import::sha1_hex(name)).bind(&norm).bind(import::sha1_hex(&norm)).bind(region)
                .execute(pool).await.map_err(e)?.last_insert_id();
            releases.push(r);
        }
        let (japan, usa, europe) = (releases[0], releases[1], releases[2]);
        // As the dump left them: the title, the platform, the region fields -- one per region.
        for (id, country) in [(990011i64, 28u32), (990012, 50)] {
            sqlx::query("INSERT INTO thegamesdb_games (provider_id, title, raw_format, app_platform_id, region_id, country_id) VALUES (?, 'Tgdbtest Piyo Quest', 'dump', ?, 6, ?)").bind(id).bind(gb).bind(country).execute(pool).await.map_err(e)?;
        }
        let regions = TgdbRegions::from_lists(&[(6, "PAL".into())], &[(28, "Japan".into()), (50, "USA".into())]);
        let priority: Vec<String> = ["US", "EU", "JP"].iter().map(|s| s.to_string()).collect();
        // Only the Japanese release is picked (a set holding just it).
        let (linked, cached) = link_releases(pool, &regions, &priority, &[japan]).await.map_err(de)?;
        let link = |release: u64| async move { sqlx::query_scalar::<_, String>("SELECT provider_id FROM release_provider_links WHERE release_id = ? AND provider = 'thegamesdb' AND deleted = FALSE").bind(release).fetch_optional(pool).await };
        let (l_japan, l_usa, l_europe) = (link(japan).await.map_err(e)?, link(usa).await.map_err(e)?, link(europe).await.map_err(e)?);
        let checks = [
            ((linked, cached) == (1, true), format!("one linked from the cache: {linked} {cached}")),
            (l_japan.as_deref() == Some("990011"), format!("the Japanese entry: {l_japan:?}")),
            (l_usa.is_none() && l_europe.is_none(), format!("what wasn't picked stays unlinked: {l_usa:?} {l_europe:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), "1 of 3 releases linked, to its region's entry, with no API call".to_string()))
    })
    .await;
}

async fn phase_title_sizes(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "title sizes: on disk is the CHD, the catalogue the raw dump", async {
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let ae = |e: romexplorer::api::Error| e.message;
        let app = romexplorer::api::App::with_pool(pool.clone(), env.paths.clone(), DatabaseConfig::default()).map_err(ae)?;
        let src = import::ensure_source(pool, "redump").await.map_err(de)?;
        let ps1 = Platform::PlayStation.igdb_id();
        let t = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES ('Sztest Hoge Quest', 'sztest hoge quest', ?)").bind(import::sha1_hex("sztest hoge quest")).execute(pool).await.map_err(e)?.last_insert_id();
        let mut releases = Vec::new();
        for (name, region) in [("Sztest Hoge Quest (USA)", "US"), ("Sztest Hoge Quest (Japan)", "JP")] {
            let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (?, ?, ?, 'sztest hoge quest', ?, ?, ?)")
                .bind(ps1).bind(name).bind(import::sha1_hex(name)).bind(import::sha1_hex("sztest hoge quest")).bind(region).bind(src)
                .execute(pool).await.map_err(e)?.last_insert_id();
            sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(t).bind(r).execute(pool).await.map_err(e)?;
            // The DAT's raw dump: a cue and a 700-byte bin.
            for (file, size) in [(format!("{name}.cue"), 100u64), (format!("{name}.bin"), 700)] {
                let c = sqlx::query("INSERT INTO content (file_name, size) VALUES (?, ?)").bind(&file).bind(size).execute(pool).await.map_err(e)?.last_insert_id();
                sqlx::query("INSERT INTO release_contents (release_id, content_id, file_name, source_id) VALUES (?, ?, ?, ?)").bind(r).bind(c).bind(&file).bind(src).execute(pool).await.map_err(e)?;
            }
            releases.push(r);
        }
        // Held: the USA release (the first by region priority), as a 300-byte CHD.
        let usa = releases[0];
        let chd = sqlx::query("INSERT INTO content (file_name, size) VALUES ('Sztest Hoge Quest (USA).chd', 300)").execute(pool).await.map_err(e)?.last_insert_id();
        let path = "/sztest/Sztest Hoge Quest (USA).chd";
        let file = sqlx::query("INSERT INTO local_files (path, path_hash, content_id, size) VALUES (?, ?, ?, 300)").bind(path).bind(import::sha1_hex(path)).bind(chd).execute(pool).await.map_err(e)?.last_insert_id();
        let rom = sqlx::query("INSERT INTO local_rom (main_path, app_platform_id) VALUES (?, ?)").bind(path).bind(ps1).execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO local_rom_files (rom_id, file_id) VALUES (?, ?)").bind(rom).bind(file).execute(pool).await.map_err(e)?;
        let game = sqlx::query("INSERT INTO local_game (name) VALUES ('Sztest Hoge Quest (USA)')").execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO local_game_roms (game_id, rom_id) VALUES (?, ?)").bind(game).bind(rom).execute(pool).await.map_err(e)?;
        sqlx::query("INSERT INTO local_release_games (release_id, game_id) VALUES (?, ?)").bind(usa).bind(game).execute(pool).await.map_err(e)?;

        let row = romexplorer::api::catalog::titles(&app, &romexplorer::api::catalog::ListQuery { search: Some("Sztest Hoge".into()), view: Some("collection".into()), ..Default::default() })
            .await.map_err(ae)?.items.into_iter().find(|r| r.id == t).ok_or("title not listed")?;
        let sizes = (row.local_size_all, row.local_size_top, row.catalog_size_all, row.catalog_size_top);
        let checks = [(sizes == (Some(300), Some(300), Some(1600), Some(800)), format!("(on disk all, first, catalogue all, first) = {sizes:?}, want (300, 300, 1600, 800)"))];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{sizes:?}")))
    })
    .await;
}

/// A media download started again goes on after the last entry an earlier run finished (its
/// checkpoint) instead of checking every entry again.
async fn phase_media_resume(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "media download: a restarted run goes on from its checkpoint", async {
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let ae = |e: romexplorer::api::Error| e.message;
        let app = romexplorer::api::App::with_pool(pool.clone(), env.paths.clone(), DatabaseConfig::default()).map_err(ae)?;
        let src = import::ensure_source(pool, "no-intro").await.map_err(de)?;
        let platform = Platform::Atari5200.igdb_id();
        // Two held releases on a platform nothing else here uses, each linked to a provider
        // with no credentials (so each entry counts as "unavailable" when looked at).
        for (name, id) in [("Mrtest Hoge Quest (USA)", "1"), ("Mrtest Fuga Saga (USA)", "2")] {
            let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (?, ?, ?, ?, ?, 'US', ?)")
                .bind(platform).bind(name).bind(import::sha1_hex(name)).bind(name.to_lowercase()).bind(import::sha1_hex(&name.to_lowercase())).bind(src)
                .execute(pool).await.map_err(e)?.last_insert_id();
            let g = sqlx::query("INSERT INTO local_game (name) VALUES (?)").bind(name).execute(pool).await.map_err(e)?.last_insert_id();
            sqlx::query("INSERT INTO local_release_games (release_id, game_id) VALUES (?, ?)").bind(r).bind(g).execute(pool).await.map_err(e)?;
            sqlx::query("INSERT INTO release_provider_links (release_id, provider, provider_id, confidence) VALUES (?, 'mobygames', ?, 'hash')").bind(r).bind(id).execute(pool).await.map_err(e)?;
        }
        let request = romexplorer::api::media_backfill::MediaBackfillRequest { scope: "owned".to_string(), platform: Some(platform), ..Default::default() };
        let looked_at = |s: &romexplorer::api::media_backfill::MediaBackfillSummary| s.already + s.downloaded + s.unavailable.values().sum::<u64>() + s.errors.len() as u64;
        // A first run looks at both entries and leaves a checkpoint behind.
        let first_job = jobs::enqueue(pool, JobType::MediaBackfill, serde_json::json!({})).await.map_err(de)?;
        let first = romexplorer::api::media_backfill::run(&app, &request, &jobs::Progress::new(pool, first_job)).await.map_err(ae)?;
        // As a restart leaves it: the first entry done.
        let resumed_job = jobs::enqueue(pool, JobType::MediaBackfill, serde_json::json!({})).await.map_err(de)?;
        sqlx::query("UPDATE jobs SET checkpoint = 'entries:mobygames\t1' WHERE id = ?").bind(resumed_job).execute(pool).await.map_err(e)?;
        let resumed = romexplorer::api::media_backfill::run(&app, &request, &jobs::Progress::new(pool, resumed_job)).await.map_err(ae)?;
        let checks = [
            (first.entries == 2 && looked_at(&first) == 2, format!("a first run looks at every entry: {} of {}", looked_at(&first), first.entries)),
            (resumed.entries == 2 && looked_at(&resumed) == 1, format!("a restarted run only at what is left: {} of {}", looked_at(&resumed), resumed.entries)),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} checks", checks.len())))
    })
    .await;
}

/// A title linked to another game through a file it shares with many discs (a stock audio
/// track) pulled that game's titles in: the audit sees its own files name its own game,
/// undoes the merges and relinks it.
async fn phase_wrong_link_merges(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "merges: a link through a shared file is undone by the title's own files", async {
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let src = import::ensure_source(pool, "redump").await.map_err(de)?;
        let ps1 = Platform::PlayStation.igdb_id();
        // The stock track every disc here carries; Hasheous names it the Fuga game.
        let track = sqlx::query("INSERT INTO content (file_name, size) VALUES ('wltest (Track 2).bin', 37396800)").execute(pool).await.map_err(e)?.last_insert_id();
        let mut titles = Vec::new();
        for (name, own_dump) in [("Wltest Hoge Quest", 990101u64), ("Wltest Fuga Saga", 990102), ("Wltest Piyo One", 990103), ("Wltest Piyo Two", 990104)] {
            let norm = name.to_lowercase();
            let t = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, ?, ?)").bind(name).bind(&norm).bind(import::sha1_hex(&format!("wltest/{norm}"))).execute(pool).await.map_err(e)?.last_insert_id();
            let release = format!("{name} (USA)");
            let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (?, ?, ?, ?, ?, 'US', ?)")
                .bind(ps1).bind(&release).bind(import::sha1_hex(&release)).bind(&norm).bind(import::sha1_hex(&norm)).bind(src)
                .execute(pool).await.map_err(e)?.last_insert_id();
            sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(t).bind(r).execute(pool).await.map_err(e)?;
            let own = sqlx::query("INSERT INTO content (file_name, size) VALUES (?, 1000)").bind(format!("{release} (Track 1).bin")).execute(pool).await.map_err(e)?.last_insert_id();
            sqlx::query("INSERT INTO release_contents (release_id, content_id, file_name) VALUES (?, ?, 'track1.bin'), (?, ?, 'track2.bin')").bind(r).bind(own).bind(r).bind(track).execute(pool).await.map_err(e)?;
            sqlx::query("INSERT INTO hasheous_lookups (content_id, raw_response_path, raw_format, hit) VALUES (?, ?, 'json', TRUE)").bind(own).bind(format!("/cache/hasheous/dump_game_{own_dump}.json")).execute(pool).await.map_err(e)?;
            let game = 888100 + (own_dump - 990100);
            sqlx::query("INSERT INTO hasheous_dump_games (id, name, platform, mapped, dump_name) VALUES (?, ?, 'PS1', ?, 'wltest.zip')").bind(own_dump).bind(name).bind(format!("[[\"igdb\",\"{game}\"]]")).execute(pool).await.map_err(e)?;
            titles.push(t);
        }
        sqlx::query("INSERT INTO hasheous_lookups (content_id, raw_response_path, raw_format, hit) VALUES (?, '/cache/hasheous/dump_game_990199.json', 'json', TRUE)").bind(track).execute(pool).await.map_err(e)?;
        sqlx::query("INSERT INTO hasheous_dump_games (id, name, platform, mapped, dump_name) VALUES (990199, 'Wltest Fuga Saga', 'PS1', '[[\"igdb\",\"888102\"]]', 'wltest.zip')").execute(pool).await.map_err(e)?;
        let (hoge, fuga) = (titles[0], titles[1]);
        // As the track left it: the Hoge title linked to the Fuga game, and merged with Fuga.
        for t in [hoge, fuga] {
            sqlx::query("INSERT INTO title_provider_links (title_id, provider, provider_id, confidence) VALUES (?, 'igdb', '888102', 'hash')").bind(t).execute(pool).await.map_err(e)?;
        }
        let survivor = database::title_merge::merge_titles(pool, &[hoge, fuga], "igdb:888102", "hash").await.map_err(de)?;
        let summary = database::merge_audit::audit(pool, &env.clients, &jobs::Progress::none()).await.map_err(de)?;
        let alive: Vec<bool> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT deleted = FALSE FROM title WHERE id IN ({hoge}, {fuga}) ORDER BY id"))).fetch_all(pool).await.map_err(e)?;
        let links: Vec<(u64, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT title_id, provider_id FROM title_provider_links WHERE title_id IN ({hoge}, {fuga}) AND provider = 'igdb' AND deleted = FALSE ORDER BY title_id"))).fetch_all(pool).await.map_err(e)?;
        let marked: String = sqlx::query_scalar("SELECT reason FROM title_merge_log WHERE survivor_id = ? AND reason LIKE 'igdb:888102%' ORDER BY id DESC LIMIT 1").bind(survivor).fetch_one(pool).await.map_err(e)?;
        let checks = [
            (summary.relinked == vec![survivor], format!("the wrongly linked title is found: {:?}", summary.relinked)),
            (alive == vec![true, true], format!("the merge is undone: alive {alive:?}")),
            (links == vec![(hoge, "888101".to_string()), (fuga, "888102".to_string())], format!("each title linked to its own game: {links:?}")),
            (marked.ends_with("(wrong link)"), format!("marked so it is not joined back: {marked:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} checks", checks.len())))
    })
    .await;
}

/// The same game on two platforms is two titles: an automatic merge across platforms (a wrong
/// link, or a pass that did not ask) is split by the audit, and a split pair is never joined
/// back by name.
async fn phase_platform_merges(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "merges: titles on different platforms are split and kept apart", async {
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let src = import::ensure_source(pool, "no-intro").await.map_err(de)?;
        let (ps2, psp) = (Platform::PlayStation2.igdb_id(), Platform::PlayStationPortable.igdb_id());
        let mut titles = Vec::new();
        for (name, release, platform) in [("Platest Hoge Quest", "Platest Hoge Quest (USA)", ps2), ("Platest Hoge Quest", "Platest Hoge Quest (Europe)", psp), ("Platest Hoge Quest J", "Platest Hoge Quest (Japan)", ps2)] {
            let norm = name.to_lowercase();
            let t = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, ?, ?)").bind(name).bind(&norm).bind(import::sha1_hex(&format!("platest/{release}"))).execute(pool).await.map_err(e)?.last_insert_id();
            let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (?, ?, ?, ?, ?, 'US', ?)")
                .bind(platform).bind(release).bind(import::sha1_hex(release)).bind(&norm).bind(import::sha1_hex(&norm)).bind(src)
                .execute(pool).await.map_err(e)?.last_insert_id();
            sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(t).bind(r).execute(pool).await.map_err(e)?;
            titles.push(t);
        }
        let (on_ps2, on_psp, other_ps2) = (titles[0], titles[1], titles[2]);
        let share = (database::title_merge::share_platform(pool, on_ps2, on_psp).await.map_err(de)?, database::title_merge::share_platform(pool, on_ps2, other_ps2).await.map_err(de)?);
        // As an older pass (or a wrong link) left it: the PSP title merged into the PS2 one.
        let survivor = database::title_merge::merge_titles(pool, &[on_ps2, on_psp], "igdb:887001", "hash").await.map_err(de)?;
        let summary = database::merge_audit::audit(pool, &env.clients, &jobs::Progress::none()).await.map_err(de)?;
        let alive: Vec<bool> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT deleted = FALSE FROM title WHERE id IN ({on_ps2}, {on_psp}) ORDER BY id"))).fetch_all(pool).await.map_err(e)?;
        let marked: String = sqlx::query_scalar("SELECT reason FROM title_merge_log WHERE survivor_id = ? AND reason LIKE 'igdb:887001%' ORDER BY id DESC LIMIT 1").bind(survivor).fetch_one(pool).await.map_err(e)?;
        let again = database::merge_audit::audit(pool, &env.clients, &jobs::Progress::none()).await.map_err(de)?;
        let checks = [
            (share == (false, true), format!("PS2 and PSP share no platform, two PS2 titles do: {share:?}")),
            (summary.platform_splits == 1 && alive == vec![true, true], format!("the merge across platforms is split: {} split(s), alive {alive:?}", summary.platform_splits)),
            (marked.ends_with("(platforms)"), format!("and marked so it is not joined back: {marked:?}")),
            (again.platform_splits == 0 && again.remerged == 0, format!("a second audit leaves it apart: {again:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} checks", checks.len())))
    })
    .await;
}

/// A set holds titles and games as entries, resolved whenever the set is read: a title its
/// best release among those held, else its top-priority one; a game the same across its
/// platforms. Removing, reordering, merging titles and copying go by the entries.
async fn phase_set_entries(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "sets: titles and games as entries, resolved held-first", async {
        use romexplorer::api::sets::{self, EntryKind};
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let ae = |e: romexplorer::api::Error| e.message;
        let app = romexplorer::api::App::with_pool(pool.clone(), env.paths.clone(), DatabaseConfig::default()).map_err(ae)?;
        let src = import::ensure_source(pool, "no-intro").await.map_err(de)?;
        let (gba, gb) = (Platform::GameBoyAdvance.igdb_id(), Platform::GameBoy.igdb_id());
        let title = |name: &'static str| async move {
            let norm = name.to_lowercase();
            sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, ?, ?)").bind(name).bind(&norm).bind(import::sha1_hex(&format!("settest/{norm}"))).execute(pool).await.map(|r| r.last_insert_id())
        };
        let release = |t: u64, name: &'static str, platform: u16, region: &'static str| async move {
            let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (?, ?, ?, ?, ?, ?, ?)")
                .bind(platform).bind(name).bind(import::sha1_hex(name)).bind(name.to_lowercase()).bind(import::sha1_hex(&name.to_lowercase())).bind(region).bind(src)
                .execute(pool).await?.last_insert_id();
            sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(t).bind(r).execute(pool).await?;
            Ok::<u64, sqlx::Error>(r)
        };
        let hold = |r: u64| async move {
            let g = sqlx::query("INSERT INTO local_game (name) VALUES ('settest copy')").execute(pool).await?.last_insert_id();
            sqlx::query("INSERT INTO local_release_games (release_id, game_id) VALUES (?, ?)").bind(r).bind(g).execute(pool).await?;
            Ok::<u64, sqlx::Error>(g)
        };
        let hoge = title("Settest Hoge Quest").await.map_err(e)?;
        let usa = release(hoge, "Settest Hoge Quest (USA)", gba, "US").await.map_err(e)?;
        let japan = release(hoge, "Settest Hoge Quest (Japan)", gba, "JP").await.map_err(e)?;
        let japan_copy = hold(japan).await.map_err(e)?;
        // One game, two platforms: only the Game Boy one is held.
        let (fuga, fuga_gb) = (title("Settest Fuga Saga").await.map_err(e)?, title("Settest Fuga Saga GB").await.map_err(e)?);
        let on_gba = release(fuga, "Settest Fuga Saga (USA)", gba, "US").await.map_err(e)?;
        let on_gb = release(fuga_gb, "Settest Fuga Saga GB (USA)", gb, "US").await.map_err(e)?;
        hold(on_gb).await.map_err(e)?;
        for t in [fuga, fuga_gb] {
            sqlx::query("INSERT INTO title_provider_links (title_id, provider, provider_id, confidence) VALUES (?, 'igdb', '885001', 'manual')").bind(t).execute(pool).await.map_err(e)?;
        }

        let set = sets::create(&app, &sets::SetSpec { name: "Settest".into(), description: None }).await.map_err(ae)?;
        sets::add_entries(&app, set.id, EntryKind::Title, &[hoge.to_string()]).await.map_err(ae)?;
        sets::add_entries(&app, set.id, EntryKind::Game, &["885001".to_string()]).await.map_err(ae)?;
        let listed = || async {
            sets::releases(&app, set.id, &romexplorer::api::catalog::ListQuery { view: Some("collection".into()), ..Default::default() })
                .await
                .map(|p| p.items.into_iter().map(|r| (r.id, r.set_via.unwrap_or_default())).collect::<Vec<_>>())
        };
        let held_first = listed().await.map_err(ae)?;
        // Not held any more: the top-priority release; a newly held better one takes over.
        sqlx::query("DELETE FROM local_release_games WHERE game_id = ?").bind(japan_copy).execute(pool).await.map_err(e)?;
        let none_held = listed().await.map_err(ae)?;
        let rev = release(hoge, "Settest Hoge Quest (USA) (Rev 1)", gba, "US").await.map_err(e)?;
        sqlx::query("UPDATE `release` SET revision_sort = 1 WHERE id = ?").bind(rev).execute(pool).await.map_err(e)?;
        hold(rev).await.map_err(e)?;
        let newer = listed().await.map_err(ae)?;
        // Reordered by the rows shown; removing the title's row removes the title.
        sets::reorder(&app, set.id, &[on_gb]).await.map_err(ae)?;
        let reordered = listed().await.map_err(ae)?;
        sets::remove_releases(&app, set.id, &[rev]).await.map_err(ae)?;
        let after_remove = listed().await.map_err(ae)?;
        let entries_left: Vec<(Option<u64>, Option<String>)> = sqlx::query_as("SELECT title_id, igdb_id FROM collection_set_entries WHERE set_id = ?").bind(set.id).fetch_all(pool).await.map_err(e)?;

        // A title entry follows a title merge to the survivor.
        let (piyo_a, piyo_b) = (title("Settest Piyo A").await.map_err(e)?, title("Settest Piyo B").await.map_err(e)?);
        release(piyo_b, "Settest Piyo B (USA)", gba, "US").await.map_err(e)?;
        sets::add_entries(&app, set.id, EntryKind::Title, &[piyo_b.to_string()]).await.map_err(ae)?;
        database::title_merge::merge_titles(pool, &[piyo_a, piyo_b], "manual", "manual").await.map_err(de)?;
        let merged: Vec<u64> = sqlx::query_scalar("SELECT title_id FROM collection_set_entries WHERE set_id = ? AND title_id IS NOT NULL").bind(set.id).fetch_all(pool).await.map_err(e)?;
        let totals = sets::get(&app, set.id).await.map_err(ae)?;

        // Only the same release twice is a duplicate: a release added by hand and its title's
        // best release both count; a game resolving to a release a title already gave does not.
        let once = sets::create(&app, &sets::SetSpec { name: "Settest once".into(), description: None }).await.map_err(ae)?;
        sets::add_entries(&app, once.id, EntryKind::Release, &[usa.to_string()]).await.map_err(ae)?;
        sets::add_entries(&app, once.id, EntryKind::Title, &[hoge.to_string(), fuga_gb.to_string()]).await.map_err(ae)?;
        sets::add_entries(&app, once.id, EntryKind::Game, &["885001".to_string()]).await.map_err(ae)?;
        let counted_once: Vec<u64> = sets::releases(&app, once.id, &romexplorer::api::catalog::ListQuery { view: Some("collection".into()), ..Default::default() }).await.map_err(ae)?.items.into_iter().map(|r| r.id).collect();
        let once_totals = sets::get(&app, once.id).await.map_err(ae)?;

        // A deleted set copies nothing.
        sets::delete(&app, set.id).await.map_err(ae)?;
        let dest = crate_dir().join("tests").join("output").join("settest-copy");
        let _ = std::fs::create_dir_all(&dest);
        let request = romexplorer_types::copy::CopyFilesRequest {
            selection: romexplorer_types::copy::CopySelection { set_ids: vec![set.id], ..Default::default() },
            mode: romexplorer_types::copy::CopyMode::ToExternal,
            destination: dest.display().to_string(),
            folder_scheme: None,
            output: None,
            source_preference: Default::default(),
            on_conflict: Default::default(),
            titles_all_releases: false,
            conflict_choices: Vec::new(),
            upload_source_id: None,
            export_artwork: Vec::new(),
            verify: Some(false),
        };
        let deleted_copies = romexplorer::api::copy::plan::plan(&app, &request).await.map(|p| p.items.len()).unwrap_or(0);
        let _ = std::fs::remove_dir_all(&dest);

        let checks = [
            (held_first == vec![(japan, "title".to_string()), (on_gb, "game".to_string())], format!("a title gives the release you hold, a game the platform you hold it on: {held_first:?}")),
            (none_held.first() == Some(&(usa, "title".to_string())), format!("nothing held: the top-priority release: {none_held:?}")),
            (newer.first() == Some(&(rev, "title".to_string())), format!("a better release held later is used without adding again: {newer:?}")),
            (reordered.iter().map(|r| r.0).collect::<Vec<_>>() == vec![on_gb, rev], format!("reordered by the rows shown: {reordered:?}")),
            (after_remove == vec![(on_gb, "game".to_string())] && entries_left == vec![(None, Some("885001".to_string()))], format!("removing the title's row removes the title: {after_remove:?} / {entries_left:?}")),
            (merged == vec![piyo_a.min(piyo_b)], format!("a title entry follows the merge: {merged:?}")),
            (totals.release_count == 2, format!("the set counts what its entries resolve to: {}", totals.release_count)),
            (counted_once == vec![usa, rev, on_gb] && once_totals.release_count == 3, format!("the hand-added release and its title's best both count; the game's release is the GB title's, counted once: {counted_once:?}")),
            (deleted_copies == 0, format!("a deleted set copies nothing: {deleted_copies} item(s)")),
            (on_gba != 0, String::new()),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} checks", checks.len() - 1)))
    })
    .await;
}

/// A Virtual Console re-release is an edition of its own: flagged at import (and by the
/// one-time backfill for older imports), ranked after the original by default, and the edition
/// order is a list the user can rearrange.
async fn phase_rerelease_editions(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "re-releases: Virtual Console flagged and ranked by the edition order", async {
        use romexplorer::api::catalog;
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let ae = |e: romexplorer::api::Error| e.message;
        let app = romexplorer::api::App::with_pool(pool.clone(), env.paths.clone(), DatabaseConfig::default()).map_err(ae)?;
        let dat = "<datafile><header><name>Nintendo - Game Boy Advance</name></header>\
            <game name=\"Edtest Hoge Quest (USA) (Virtual Console)\"><rom name=\"edtest-vc.gba\" size=\"777001\" sha1=\"ed7e570000000000000000000000000000000ac1\"/></game>\
            <game name=\"Edtest Hoge Quest (USA)\"><rom name=\"edtest-orig.gba\" size=\"777002\" sha1=\"ed7e5700000000000000000000000000000000a2\"/></game></datafile>";
        import::import_dat(pool, Source::NoIntro, "edtest.dat", &dat_parse::parse_dat_xml(dat).map_err(de)?, &ImportOptions::default()).await.map_err(de)?;
        let flags: Vec<(String, bool)> = sqlx::query_as("SELECT name, is_rerelease FROM `release` WHERE name LIKE 'Edtest Hoge Quest%' ORDER BY name").fetch_all(pool).await.map_err(e)?;
        let title: u64 = sqlx::query_scalar("SELECT tr.title_id FROM title_releases tr JOIN `release` r ON r.id = tr.release_id WHERE r.name = 'Edtest Hoge Quest (USA)'").fetch_one(pool).await.map_err(e)?;
        let first = || async { catalog::title(&app, title).await.map(|t| t.releases.first().map(|r| r.name.clone()).unwrap_or_default()) };
        let by_default = first().await.map_err(ae)?;
        romexplorer::api::settings::set(&app, database::settings::KEY_RELEASE_EDITION_ORDER, serde_json::json!(["rerelease", "original"])).await.map_err(ae)?;
        let rerelease_first = first().await.map_err(ae)?;
        let legacy = romexplorer::api::settings::set(&app, database::settings::KEY_RELEASE_EDITION_ORDER, serde_json::json!("versions_first")).await.is_ok();
        let unknown = romexplorer::api::settings::set(&app, database::settings::KEY_RELEASE_EDITION_ORDER, serde_json::json!(["original", "remaster"])).await.is_err();
        romexplorer::api::settings::delete(&app, database::settings::KEY_RELEASE_EDITION_ORDER).await.map_err(ae)?;

        // Imported before the parser knew the tag: the one-time backfill flags it.
        let old = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region) VALUES (24, 'Edtest Fuga Saga (Japan) (Wii U Virtual Console)', ?, 'edtest fuga saga', ?, 'JP')")
            .bind(import::sha1_hex("Edtest Fuga Saga (Japan) (Wii U Virtual Console)")).bind(import::sha1_hex("edtest fuga saga")).execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("DELETE FROM data_migrations WHERE name = 'release_rereleases_0058'").execute(pool).await.map_err(e)?;
        database::release_editions::backfill_rereleases(pool).await.map_err(de)?;
        let backfilled: bool = sqlx::query_scalar("SELECT is_rerelease FROM `release` WHERE id = ?").bind(old).fetch_one(pool).await.map_err(e)?;

        let checks = [
            (flags == vec![("Edtest Hoge Quest (USA)".to_string(), false), ("Edtest Hoge Quest (USA) (Virtual Console)".to_string(), true)], format!("the Virtual Console release is flagged at import: {flags:?}")),
            (by_default == "Edtest Hoge Quest (USA)", format!("by default the original comes first: {by_default:?}")),
            (rerelease_first == "Edtest Hoge Quest (USA) (Virtual Console)", format!("re-releases put first, the Virtual Console one leads: {rerelease_first:?}")),
            (legacy && unknown, format!("an older value is still accepted ({legacy}), an unknown edition refused ({unknown})")),
            (backfilled, "an older import is flagged by the one-time backfill".to_string()),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} checks", checks.len())))
    })
    .await;
}

/// Titles combined by hand from two IGDB games (a game and its collection): each release keeps
/// its own game, the title takes the game of its first release by region priority and follows
/// it, art and pages go by the release's own game, and undoing the combine puts it all back.
async fn phase_combine_igdb_games(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "combining titles of different IGDB games keeps each game; region priority picks the title's", async {
        use romexplorer::api::catalog;
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let ae = |e: romexplorer::api::Error| e.message;
        let app = romexplorer::api::App::with_pool(pool.clone(), env.paths.clone(), DatabaseConfig::default()).map_err(ae)?;
        let src = import::ensure_source(pool, "no-intro").await.map_err(de)?;
        let ps1 = Platform::PlayStation.igdb_id();
        let mut title_of = std::collections::HashMap::new();
        let mut release_of = std::collections::HashMap::new();
        for (title, release, region, igdb) in [
            ("Combtest Hoge Quest", "Combtest Hoge Quest (Japan)", "JP", "884001"),
            ("Combtest Hoge Quest Collection", "Combtest Hoge Quest Collection (USA)", "US", "884002"),
            ("Combtest Fuga Saga", "Combtest Fuga Saga (Japan)", "JP", "884003"),
            ("Combtest Fuga Saga Special", "Combtest Fuga Saga Special (USA)", "US", "884003"),
        ] {
            let norm = romexplorer::database::matching::normalize_title(title).to_lowercase();
            let hash = import::sha1_hex(&norm);
            let t = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, ?, ?)").bind(title).bind(&norm).bind(&hash).execute(pool).await.map_err(e)?.last_insert_id();
            sqlx::query("INSERT INTO title_provider_links (title_id, provider, provider_id, confidence) VALUES (?, 'igdb', ?, 'hash')").bind(t).bind(igdb).execute(pool).await.map_err(e)?;
            let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (?, ?, ?, ?, ?, ?, ?)")
                .bind(ps1).bind(release).bind(import::sha1_hex(release)).bind(&norm).bind(&hash).bind(region).bind(src)
                .execute(pool).await.map_err(e)?.last_insert_id();
            sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(t).bind(r).execute(pool).await.map_err(e)?;
            title_of.insert(title, t);
            release_of.insert(release, r);
        }
        let (jp, us) = (release_of["Combtest Hoge Quest (Japan)"], release_of["Combtest Hoge Quest Collection (USA)"]);
        let release_igdb = |r: u64| async move { sqlx::query_scalar::<_, String>("SELECT provider_id FROM release_provider_links WHERE release_id = ? AND provider = 'igdb' AND deleted = FALSE").bind(r).fetch_optional(pool).await };
        let title_igdb = |t: u64| async move { sqlx::query_as::<_, (String, String)>("SELECT provider_id, CAST(confidence AS CHAR) FROM title_provider_links WHERE title_id = ? AND provider = 'igdb' AND deleted = FALSE").bind(t).fetch_optional(pool).await };
        let regions = |first: &str| serde_json::json!([first, if first == "JP" { "US" } else { "JP" }, "EU"]);

        romexplorer::api::settings::set(&app, database::settings::KEY_REGION_PRIORITY, regions("JP")).await.map_err(ae)?;
        let combined = romexplorer::api::titles::merge(&app, &romexplorer::api::titles::MergeRequest { title_ids: vec![title_of["Combtest Hoge Quest"], title_of["Combtest Hoge Quest Collection"]] }).await.map_err(ae)?.title_id;
        let kept = (release_igdb(jp).await.map_err(e)?, release_igdb(us).await.map_err(e)?);
        let jp_first = title_igdb(combined).await.map_err(e)?;
        romexplorer::api::settings::set(&app, database::settings::KEY_REGION_PRIORITY, regions("US")).await.map_err(ae)?;
        let us_first = title_igdb(combined).await.map_err(e)?;

        // Pages and art: each release by its own game.
        let us_links: Vec<String> = catalog::release(&app, us).await.map_err(ae)?.provider_links.into_iter().filter(|l| l.provider == "igdb").map(|l| l.provider_id).collect();
        let jp_links: Vec<String> = catalog::release(&app, jp).await.map_err(ae)?.provider_links.into_iter().filter(|l| l.provider == "igdb").map(|l| l.provider_id).collect();
        let targets = database::providers::media::release_targets(pool, combined, Provider::Igdb, "884002").await.map_err(de)?;
        let wrong = sqlx::query("INSERT INTO media (media_type, sub_category, path, root_role, source, provider_id) VALUES ('box', 'front', 'igdb/884002/common/box_front.jpg', 'image', 'igdb', '884002')").execute(pool).await.map_err(e)?.last_insert_id();
        let right = sqlx::query("INSERT INTO media (media_type, sub_category, path, root_role, source, provider_id) VALUES ('box', 'front', 'igdb/884001/common/box_front.jpg', 'image', 'igdb', '884001')").execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO release_media (release_id, media_id) VALUES (?, ?), (?, ?), (?, ?)").bind(jp).bind(wrong).bind(jp).bind(right).bind(us).bind(wrong).execute(pool).await.map_err(e)?;
        database::providers::media::detach_stale(pool, Some(&[combined])).await.map_err(de)?;
        let jp_art: Vec<u64> = sqlx::query_scalar("SELECT media_id FROM release_media WHERE release_id = ? ORDER BY media_id").bind(jp).fetch_all(pool).await.map_err(e)?;
        let us_art: Vec<u64> = sqlx::query_scalar("SELECT media_id FROM release_media WHERE release_id = ? ORDER BY media_id").bind(us).fetch_all(pool).await.map_err(e)?;

        // Undone: each title its own game again, the releases' copies gone.
        let log_id: u64 = sqlx::query_scalar("SELECT id FROM title_merge_log WHERE survivor_id = ? AND undone_at IS NULL ORDER BY id DESC LIMIT 1").bind(combined).fetch_one(pool).await.map_err(e)?;
        let revived = romexplorer::api::titles::decouple(&app, log_id).await.map_err(ae)?;
        let after = (title_igdb(combined).await.map_err(e)?.map(|l| l.0), title_igdb(revived).await.map_err(e)?.map(|l| l.0), release_igdb(jp).await.map_err(e)?, release_igdb(us).await.map_err(e)?);

        // One game between them: as before, nothing on the releases.
        let same = romexplorer::api::titles::merge(&app, &romexplorer::api::titles::MergeRequest { title_ids: vec![title_of["Combtest Fuga Saga"], title_of["Combtest Fuga Saga Special"]] }).await.map_err(ae)?.title_id;
        let same_links = (release_igdb(release_of["Combtest Fuga Saga (Japan)"]).await.map_err(e)?, release_igdb(release_of["Combtest Fuga Saga Special (USA)"]).await.map_err(e)?, title_igdb(same).await.map_err(e)?.map(|l| l.0));
        romexplorer::api::settings::delete(&app, database::settings::KEY_REGION_PRIORITY).await.map_err(ae)?;

        let checks = [
            (kept == (Some("884001".to_string()), Some("884002".to_string())), format!("each release keeps its title's game: {kept:?}")),
            (jp_first == Some(("884001".to_string(), "manual".to_string())), format!("Japan first: the Japanese release's game, as a manual link: {jp_first:?}")),
            (us_first.as_ref().map(|l| l.0.as_str()) == Some("884002"), format!("USA first: the title follows to the collection's game: {us_first:?}")),
            (us_links == vec!["884002".to_string()] && jp_links == vec!["884001".to_string()], format!("each release page links its own game only: USA {us_links:?}, Japan {jp_links:?}")),
            (targets == vec![database::providers::media::MediaTarget::Release(us)], format!("the title's game's art goes to the release without a game of its own: {targets:?}")),
            (jp_art == vec![right] && us_art == vec![wrong], format!("the other game's art comes off the Japanese release, not the collection's: {jp_art:?} / {us_art:?}")),
            (after == (Some("884001".to_string()), Some("884002".to_string()), None, None), format!("undone: each title its own game, no release copies: {after:?}")),
            (same_links == (None, None, Some("884003".to_string())), format!("one game: nothing written on the releases: {same_links:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} checks", checks.len())))
    })
    .await;
}

/// The LOCAL / RA answers and the order of local copies: RA only for a release
/// RetroAchievements verifies (its hash list or a manual link, and a set with achievements);
/// local copies by directory priority, the user's own order over it; the platform filter as
/// any-of, and all-of on the cross-platform list.
async fn phase_status_and_order(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "status badges, local-copy order, directory priority, multi-platform filters", async {
        use romexplorer::api::catalog::{self, ListQuery};
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let ae = |e: romexplorer::api::Error| e.message;
        let app = romexplorer::api::App::with_pool(pool.clone(), env.paths.clone(), DatabaseConfig::default()).map_err(ae)?;
        let src = import::ensure_source(pool, "no-intro").await.map_err(de)?;
        let (gba, gb) = (Platform::GameBoyAdvance.igdb_id(), Platform::GameBoy.igdb_id());
        let mut release_of = std::collections::HashMap::new();
        let mut title_of = std::collections::HashMap::new();
        // Two titles of one game (GBA and GB, one IGDB id), and another game on GBA only.
        for (title, release, platform, igdb) in [
            ("Statustest Hoge Quest", "Statustest Hoge Quest (USA)", gba, "434343"),
            ("Statustest Hoge Quest", "Statustest Hoge Quest (Europe)", gba, "434343"),
            ("Statustest Hoge Quest GB", "Statustest Hoge Quest GB (USA)", gb, "434343"),
            ("Statustest Fuga Saga", "Statustest Fuga Saga (USA)", gba, "434344"),
        ] {
            let norm = romexplorer::database::matching::normalize_title(title).to_lowercase();
            let hash = import::sha1_hex(&format!("{norm}/{platform}"));
            let t = match title_of.get(title) {
                Some(t) => *t,
                None => {
                    let t = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, ?, ?)").bind(title).bind(&norm).bind(&hash).execute(pool).await.map_err(e)?.last_insert_id();
                    sqlx::query("INSERT INTO title_provider_links (title_id, provider, provider_id, confidence) VALUES (?, 'igdb', ?, 'manual')").bind(t).bind(igdb).execute(pool).await.map_err(e)?;
                    title_of.insert(title, t);
                    t
                }
            };
            let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (?, ?, ?, ?, ?, 'US', ?)")
                .bind(platform).bind(release).bind(import::sha1_hex(release)).bind(&norm).bind(&hash).bind(src)
                .execute(pool).await.map_err(e)?.last_insert_id();
            sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(t).bind(r).execute(pool).await.map_err(e)?;
            release_of.insert(release, r);
        }
        let usa = release_of["Statustest Hoge Quest (USA)"];
        let europe = release_of["Statustest Hoge Quest (Europe)"];
        let on_gb = release_of["Statustest Hoge Quest GB (USA)"];
        let fuga = release_of["Statustest Fuga Saga (USA)"];
        // USA: on RetroAchievements' hash list, a set with achievements. Europe: a name guess
        // at the same set. GB: verified, but the set has no achievements. Fuga: linked by hand
        // to a set not fetched yet (counted as having achievements).
        sqlx::query("INSERT INTO retroachievements_games (provider_id, title, num_achievements) VALUES (970001, 'Statustest Hoge Quest', 50), (970003, 'Statustest Hoge Quest GB', 0)").execute(pool).await.map_err(e)?;
        sqlx::query("INSERT INTO release_provider_links (release_id, provider, provider_id, confidence) VALUES (?, 'retroachievements', '970001', 'exact'), (?, 'retroachievements', '970001', 'fuzzy'), (?, 'retroachievements', '970003', 'exact'), (?, 'retroachievements', '970004', 'manual')")
            .bind(usa).bind(europe).bind(on_gb).bind(fuga).execute(pool).await.map_err(e)?;

        let collection = |search: &str| ListQuery { search: Some(search.into()), limit: Some(50), view: Some("collection".into()), ..Default::default() };
        let ra_of: std::collections::HashMap<u64, bool> = catalog::releases(&app, &collection("Statustest")).await.map_err(ae)?.items.into_iter().map(|r| (r.id, r.ra)).collect();
        let title_ra: std::collections::HashMap<u64, bool> = catalog::titles(&app, &collection("Statustest")).await.map_err(ae)?.items.into_iter().map(|t| (t.id, t.ra)).collect();
        let detail = catalog::release(&app, usa).await.map_err(ae)?;
        let guess = catalog::release(&app, europe).await.map_err(ae)?;
        let cross = catalog::crossplatform(&app, &ListQuery { search: Some("Statustest Hoge".into()), ..Default::default() }).await.map_err(ae)?;
        let cross_titles: Vec<(u64, bool)> = cross.items.first().map(|r| r.titles.iter().map(|t| (t.id, t.ra)).collect()).unwrap_or_default();

        // The platform filter: any of the ticked platforms; all of them on cross-platform only.
        let both = format!("{gba},{gb}");
        let on = |platforms: &str, logic: Option<&str>, search: &str| ListQuery { search: Some(search.into()), limit: Some(50), view: Some("collection".into()), platforms: Some(platforms.to_string()), platform_logic: logic.map(str::to_string), ..Default::default() };
        let any_releases = catalog::releases(&app, &on(&both, None, "Statustest")).await.map_err(ae)?.total;
        let gb_releases = catalog::releases(&app, &on(&gb.to_string(), None, "Statustest")).await.map_err(ae)?.total;
        let all_hoge = catalog::crossplatform(&app, &on(&both, Some("and"), "Statustest Hoge")).await.map_err(ae)?.total;
        let all_fuga = catalog::crossplatform(&app, &on(&both, Some("and"), "Statustest Fuga")).await.map_err(ae)?.total;
        let any_fuga = catalog::crossplatform(&app, &on(&both, None, "Statustest Fuga")).await.map_err(ae)?.total;

        // Two copies of the USA release, in two sibling directories under one root; a second
        // root beside it, which a move among the first root's children must leave alone.
        let mut dir = std::collections::HashMap::new();
        for (path, parent, priority) in [("/statustest/root", None, 1), ("/statustest/other", None, 2), ("/statustest/root/a", Some("/statustest/root"), 1), ("/statustest/root/b", Some("/statustest/root"), 2)] {
            let parent_id = parent.map(|p| dir[p]);
            let id = sqlx::query("INSERT INTO local_directories (path, path_hash, parent_directory_id, is_collection_root, priority) VALUES (?, ?, ?, ?, ?)")
                .bind(path).bind(import::sha1_hex(path)).bind(parent_id).bind(parent.is_none()).bind(priority)
                .execute(pool).await.map_err(e)?.last_insert_id();
            dir.insert(path, id);
        }
        let mut copy_in = std::collections::HashMap::new();
        for d in ["/statustest/root/a", "/statustest/root/b"] {
            let path = format!("{d}/Statustest Hoge Quest (USA).gba");
            let content = sqlx::query("INSERT INTO content (file_name, size) VALUES ('Statustest Hoge Quest (USA).gba', 8)").execute(pool).await.map_err(e)?.last_insert_id();
            let file = sqlx::query("INSERT INTO local_files (path, path_hash, content_id, size, directory_id) VALUES (?, ?, ?, 8, ?)").bind(&path).bind(import::sha1_hex(&path)).bind(content).bind(dir[d]).execute(pool).await.map_err(e)?.last_insert_id();
            let rom = sqlx::query("INSERT INTO local_rom (main_path, app_platform_id) VALUES (?, ?)").bind(&path).bind(gba).execute(pool).await.map_err(e)?.last_insert_id();
            sqlx::query("INSERT INTO local_rom_files (rom_id, file_id) VALUES (?, ?)").bind(rom).bind(file).execute(pool).await.map_err(e)?;
            let game = sqlx::query("INSERT INTO local_game (name) VALUES ('Statustest Hoge Quest (USA)')").execute(pool).await.map_err(e)?.last_insert_id();
            sqlx::query("INSERT INTO local_game_roms (game_id, rom_id) VALUES (?, ?)").bind(game).bind(rom).execute(pool).await.map_err(e)?;
            sqlx::query("INSERT INTO local_release_games (release_id, game_id) VALUES (?, ?)").bind(usa).bind(game).execute(pool).await.map_err(e)?;
            copy_in.insert(d, game);
        }
        let (in_a, in_b) = (copy_in["/statustest/root/a"], copy_in["/statustest/root/b"]);
        let order = || async { catalog::release(&app, usa).await.map(|r| r.games.iter().map(|g| g.id).collect::<Vec<u64>>()) };
        let by_path = order().await.map_err(ae)?;
        database::directories::move_directory(pool, dir["/statustest/root/b"], true).await.map_err(de)?;
        let after_move = order().await.map_err(ae)?;
        let prio = |p: &'static str| async move { sqlx::query_scalar::<_, i32>("SELECT priority FROM local_directories WHERE path = ?").bind(p).fetch_one(pool).await };
        let (prio_a, prio_b, prio_root, prio_other) = (prio("/statustest/root/a").await.map_err(e)?, prio("/statustest/root/b").await.map_err(e)?, prio("/statustest/root").await.map_err(e)?, prio("/statustest/other").await.map_err(e)?);
        catalog::move_local_game(&app, usa, &catalog::LocalGameMove { game_id: in_a, after_game_id: None }).await.map_err(ae)?;
        let manual = catalog::release(&app, usa).await.map_err(ae)?;
        let manual_order: Vec<u64> = manual.games.iter().map(|g| g.id).collect();
        let marked = manual.games.iter().all(|g| g.manual_position.is_some());
        let paths: Vec<Option<String>> = manual.games.iter().map(|g| g.directory_path.clone()).collect();
        catalog::reset_local_game_order(&app, usa).await.map_err(ae)?;
        let reset = order().await.map_err(ae)?;

        let checks = [
            (ra_of.get(&usa) == Some(&true), format!("on RetroAchievements' hash list with achievements: RA ({:?})", ra_of.get(&usa))),
            (ra_of.get(&europe) == Some(&false), format!("a name guess is not RA ({:?})", ra_of.get(&europe))),
            (ra_of.get(&on_gb) == Some(&false), format!("a set without achievements is not RA ({:?})", ra_of.get(&on_gb))),
            (ra_of.get(&fuga) == Some(&true), format!("a manual link to a set not fetched yet is RA ({:?})", ra_of.get(&fuga))),
            (title_ra.get(&title_of["Statustest Hoge Quest"]) == Some(&true) && title_ra.get(&title_of["Statustest Hoge Quest GB"]) == Some(&false), format!("a title is RA when one of its releases is: {title_ra:?}")),
            (detail.ra_games.len() == 1 && detail.ra_games[0].id == "970001" && detail.ra_games[0].achievements == Some(50) && guess.ra_games.is_empty(), format!("the release page lists the verified set only: {:?} / {:?}", detail.ra_games.iter().map(|g| (g.id.clone(), g.achievements)).collect::<Vec<_>>(), guess.ra_games.len())),
            (cross.items.first().is_some_and(|r| r.ra) && cross_titles.contains(&(title_of["Statustest Hoge Quest"], true)) && cross_titles.contains(&(title_of["Statustest Hoge Quest GB"], false)), format!("cross-platform: the row and each title: {cross_titles:?}")),
            (any_releases == 4 && gb_releases == 1, format!("platforms are any-of: {any_releases} on GBA or GB, {gb_releases} on GB")),
            (all_hoge == 1 && all_fuga == 0 && any_fuga == 1, format!("cross-platform all-of: Hoge on both {all_hoge}, Fuga on both {all_fuga}, Fuga on either {any_fuga}")),
            (by_path == vec![in_a, in_b], format!("copies by directory priority: {by_path:?} (a {in_a}, b {in_b})")),
            (after_move == vec![in_b, in_a] && prio_b < prio_a, format!("b moved up goes first: {after_move:?} (a {prio_a}, b {prio_b})")),
            (prio_root == 1 && prio_other == 2, format!("the move leaves the roots alone: {prio_root}, {prio_other}")),
            (manual_order == vec![in_a, in_b] && marked, format!("the user's order wins: {manual_order:?} (marked {marked})")),
            (paths.iter().all(|p| p.as_deref().is_some_and(|p| p.starts_with("/statustest/root/"))), format!("each copy says where it is: {paths:?}")),
            (reset == vec![in_b, in_a], format!("reset: back to directory priority: {reset:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} checks", checks.len())))
    })
    .await;
}

/// RetroAchievements patches: a patch archive is hashed (itself and each member), its
/// members listed with their kinds and linked in `content_contents`; a renamed copy of the
/// same archive downloaded for another hash is recognized and dropped.
async fn phase_ra_patches(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "ra patches: archive + members hashed and recorded, renamed duplicate recognized", async {
        use std::io::Write;
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let root = std::env::temp_dir().join(format!("romexplorer_patch_test_{}", std::process::id()));
        let dir = root.join("PlayStation2");
        std::fs::create_dir_all(&dir).map_err(|x| x.to_string())?;
        let first = dir.join("Patch Test (Japan) [T-En].zip");
        {
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&first).map_err(|x| x.to_string())?);
            let opts = zip::write::SimpleFileOptions::default();
            for (name, body) in [("Patch Test (Japan) [T-En].xdelta", &b"xdelta-bytes"[..]), ("Patch Test (Japan) [T-En].cue", &b"FILE \"x.bin\" BINARY"[..]), ("readme.txt", &b"thanks"[..])] {
                zip.start_file(name, opts).map_err(|x| x.to_string())?;
                zip.write_all(body).map_err(|x| x.to_string())?;
            }
            zip.finish().map_err(|x| x.to_string())?;
        }
        let (md5_a, md5_b) = ("aaaaaaaaaaaaaaaaaaaaaaaaaaaa0001", "aaaaaaaaaaaaaaaaaaaaaaaaaaaa0002");
        for md5 in [md5_a, md5_b] {
            sqlx::query("INSERT INTO ra_game_hashes (md5, ra_game_id, name, patch_url) VALUES (?, 990077, 'Patch Test (Japan) [T-En]', 'https://github.com/RetroAchievements/RAPatches/raw/main/x.zip')")
                .bind(md5).execute(pool).await.map_err(e)?;
        }
        let target = database::ra_patches::target_for(pool, md5_a).await.map_err(de)?;
        let a = database::ra_patches::record(pool, &root, &first, &target, true).await.map_err(de)?;
        let archive: u64 = sqlx::query_scalar("SELECT content_id FROM ra_patches WHERE id = ?").bind(a.patch_id).fetch_one(pool).await.map_err(e)?;
        let members: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM content_contents WHERE parent_content_id = ?").bind(archive).fetch_one(pool).await.map_err(e)?;
        let hashed: i64 = sqlx::query_scalar("SELECT COUNT(DISTINCT cc.child_content_id) FROM content_contents cc JOIN hashes h ON h.content_id = cc.child_content_id AND h.algorithm = 'sha1' WHERE cc.parent_content_id = ?").bind(archive).fetch_one(pool).await.map_err(e)?;
        let kinds: Vec<String> = sqlx::query_scalar("SELECT CAST(kind AS CHAR) FROM ra_patch_files WHERE patch_id = ? ORDER BY kind").bind(a.patch_id).fetch_all(pool).await.map_err(e)?;
        // The same archive under another name, fetched for the other hash: recognized, dropped.
        let second = dir.join("Renamed Copy.zip");
        std::fs::copy(&first, &second).map_err(|x| x.to_string())?;
        let target_b = database::ra_patches::target_for(pool, md5_b).await.map_err(de)?;
        let b = database::ra_patches::record(pool, &root, &second, &target_b, true).await.map_err(de)?;
        let archive_b: u64 = sqlx::query_scalar("SELECT content_id FROM ra_patches WHERE id = ?").bind(b.patch_id).fetch_one(pool).await.map_err(e)?;
        let _ = std::fs::remove_dir_all(&root);
        let checks = [
            (a.rel_path == "PlayStation2/Patch Test (Japan) [T-En].zip", format!("rel_path {:?}", a.rel_path)),
            (members == 3 && hashed == 3, format!("{members} member(s) linked, {hashed} hashed")),
            (kinds == vec!["patch", "sheet", "readme"], format!("kinds {kinds:?}")),
            (a.patch_rom_name.as_deref() == Some("Patch Test (Japan) [T-En]") && a.names_consistent, format!("stem {:?} consistent {}", a.patch_rom_name, a.names_consistent)),
            (archive_b == archive && b.duplicate_of.as_deref() == Some(a.rel_path.as_str()) && b.rel_path == a.rel_path && !second.exists(), format!("duplicate: {:?}, same content {}", b.duplicate_of, archive_b == archive)),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} member(s); duplicate recognized", a.members)))
    })
    .await;
}

/// `merge_audit`: a title merged because of an IGDB id whose names it doesn't share is
/// decoupled; a related one stays merged; a stray hash link loses its IGDB metadata.
/// A bonus disc with a demo of the sequel, filed under the game it came with: its hash link
/// goes to the disc, not the title; and what such a disc already did -- the title linked to the
/// sequel and merged with it -- the audit puts right.
async fn phase_demo_of_another(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "a demo of another game: its link stays on the disc, and old merges are undone", async {
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let src = import::ensure_source(pool, "redump").await.map_err(de)?;
        let titled = |tname: &'static str, releases: &'static [(&'static str, bool)]| async move {
            let norm = tname.to_lowercase();
            let t = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, ?, ?)").bind(tname).bind(&norm).bind(import::sha1_hex(&norm)).execute(pool).await?.last_insert_id();
            let mut ids = Vec::new();
            for (rname, demo) in releases {
                let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id, is_demo) VALUES (7, ?, ?, ?, ?, 'JP', ?, ?)")
                    .bind(rname).bind(import::sha1_hex(rname)).bind(&norm).bind(import::sha1_hex(&norm)).bind(src).bind(demo)
                    .execute(pool).await?.last_insert_id();
                sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(t).bind(r).execute(pool).await?;
                ids.push(r);
            }
            Ok::<(u64, Vec<u64>), sqlx::Error>((t, ids))
        };
        sqlx::query("INSERT INTO igdb_games (provider_id, title) VALUES (990900, 'Fuga Demo Saga 2'), (990901, 'Piyo Demo Saga 2')").execute(pool).await.map_err(e)?;

        // From now on: the demo disc's hash links the disc, not the game's title.
        let (piyo, piyo_releases) = titled("Piyo Demo Saga", &[("Piyo Demo Saga (Japan) (Disc 1)", false), ("Piyo Demo Saga (Japan) (Disc 2) (Piyo Demo Saga 2 Taikenban)", true)]).await.map_err(e)?;
        let c = sqlx::query("INSERT INTO content (file_name, size) VALUES ('demo.bin', 1)").execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO release_contents (release_id, content_id, file_name) VALUES (?, ?, 'demo.bin')").bind(piyo_releases[1]).bind(c).execute(pool).await.map_err(e)?;
        cache::link_for_content(pool, c, Provider::Igdb, "990901", cache::Confidence::Hash).await.map_err(de)?;
        let title_linked: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title_provider_links WHERE title_id = ? AND provider = 'igdb' AND deleted = FALSE").bind(piyo).fetch_one(pool).await.map_err(e)?;
        let disc_linked: Option<String> = sqlx::query_scalar("SELECT provider_id FROM release_provider_links WHERE release_id = ? AND provider = 'igdb' AND deleted = FALSE").bind(piyo_releases[1]).fetch_optional(pool).await.map_err(e)?;

        // Before: the first game's title got the sequel's id that way, and was merged with it.
        let (first, _) = titled("Fuga Demo Saga", &[("Fuga Demo Saga (Japan) (Disc 1)", false), ("Fuga Demo Saga (Japan) (Disc 2) (Fuga Demo Saga 2 Taikenban)", true)]).await.map_err(e)?;
        let (sequel, _) = titled("Fuga Demo Saga 2", &[("Fuga Demo Saga 2 (Japan)", false)]).await.map_err(e)?;
        let (sequel_us, _) = titled("Fuga Demo Saga 2", &[("Fuga Demo Saga 2 (USA)", false)]).await.map_err(e)?;
        for t in [first, sequel, sequel_us] {
            cache::link_title(pool, t, Provider::Igdb, "990900", cache::Confidence::Hash).await.map_err(de)?;
        }
        database::title_merge::merge_titles(pool, &[first, sequel, sequel_us], "igdb:990900", "hash").await.map_err(de)?;
        let summary = database::merge_audit::audit(pool, &env.clients, &jobs::Progress::none()).await.map_err(de)?;
        let first_link: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title_provider_links WHERE title_id = ? AND provider = 'igdb' AND deleted = FALSE").bind(first).fetch_one(pool).await.map_err(e)?;
        // The sequel's two titles are free to merge again; the first game is on its own.
        database::title_merge::dedupe_by_igdb(pool, Some("990900"), &jobs::Progress::none()).await.map_err(de)?;
        let sequel_titles: Vec<u64> = sqlx::query_scalar("SELECT DISTINCT tr.title_id FROM title_releases tr JOIN `release` r ON r.id = tr.release_id JOIN title t ON t.id = tr.title_id AND t.deleted = FALSE WHERE r.name LIKE 'Fuga Demo Saga 2 (%'").fetch_all(pool).await.map_err(e)?;
        let first_holds: Vec<String> = sqlx::query_scalar("SELECT r.name FROM title_releases tr JOIN `release` r ON r.id = tr.release_id WHERE tr.title_id = ? ORDER BY r.name").bind(first).fetch_all(pool).await.map_err(e)?;
        let checks = [
            (title_linked == 0 && disc_linked.as_deref() == Some("990901"), format!("a demo's hash links the disc ({disc_linked:?}), not its title ({title_linked} link(s))")),
            (summary.demo_titles_fixed == 1 && first_link == 0, format!("the title linked through the demo is unlinked: {summary:?}, {first_link} link(s) left")),
            (sequel_titles.len() == 1 && !sequel_titles.contains(&first), format!("the sequel's titles merge again, without the first game: {sequel_titles:?} (first {first})")),
            (first_holds.len() == 2 && first_holds.iter().all(|n| n.starts_with("Fuga Demo Saga (Japan)")), format!("the first game keeps its own discs and the demo: {first_holds:?}")),
            (database::merge_audit::demo_of("Hoge (Japan) (Disc 2) (Fuga Saga 2 Taikenban)").as_deref() == Some("Fuga Saga 2") && database::merge_audit::demo_of("Hoge (Japan) (Demo)").is_none(), "the demo's game read from its name".to_string()),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{} title(s) put right", summary.demo_titles_fixed)))
    })
    .await;
}

/// Art from a game a title or release is no longer linked to comes off; art from the current
/// link, art found by name (no link for its provider) and art set by hand stay.
async fn phase_stale_media(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "media: art from a game no longer linked is detached", async {
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let src = import::ensure_source(pool, "redump").await.map_err(de)?;
        let t = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES ('Stale Art Test', 'stale art test', SHA1('stale art test'))").execute(pool).await.map_err(e)?.last_insert_id();
        let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (7, 'Stale Art Test (Japan)', SHA1('Stale Art Test (Japan)'), 'stale art test', SHA1('stale art test'), 'JP', ?)")
            .bind(src).execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(t).bind(r).execute(pool).await.map_err(e)?;
        // Now linked to 990950; it was linked to 990949, wrongly, when its art was fetched.
        cache::link_title(pool, t, Provider::Igdb, "990950", cache::Confidence::Hash).await.map_err(de)?;
        let media = |kind: &'static str, source: &'static str, provider_id: Option<&'static str>| async move {
            sqlx::query("INSERT INTO media (media_type, path, root_role, source, provider_id) VALUES (?, ?, 'image', ?, ?)")
                .bind(kind).bind(format!("stale/{source}/{}/{kind}.jpg", provider_id.unwrap_or("none"))).bind(source).bind(provider_id)
                .execute(pool).await.map(|x| x.last_insert_id())
        };
        let current = media("cover", "igdb", Some("990950")).await.map_err(e)?;
        let old = media("screenshot", "igdb", Some("990949")).await.map_err(e)?;
        let by_name = media("title_screen", "libretro", Some("Stale Art Test")).await.map_err(e)?;
        let uploaded = media("fanart", "manual", None).await.map_err(e)?;
        let old_on_title = media("fanart", "igdb", Some("990949")).await.map_err(e)?;
        for m in [current, old, by_name, uploaded] {
            sqlx::query("INSERT INTO release_media (release_id, media_id) VALUES (?, ?)").bind(r).bind(m).execute(pool).await.map_err(e)?;
        }
        sqlx::query("INSERT INTO title_media (title_id, media_id) VALUES (?, ?)").bind(t).bind(old_on_title).execute(pool).await.map_err(e)?;
        let detached = database::providers::media::detach_stale(pool, Some(&[t])).await.map_err(de)?;
        let left: Vec<u64> = sqlx::query_scalar("SELECT media_id FROM release_media WHERE release_id = ? ORDER BY media_id").bind(r).fetch_all(pool).await.map_err(e)?;
        let on_title: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title_media WHERE title_id = ?").bind(t).fetch_one(pool).await.map_err(e)?;
        // Unlinked by hand: the current game's art goes too.
        sqlx::query("UPDATE title_provider_links SET deleted = TRUE WHERE title_id = ? AND provider = 'igdb'").bind(t).execute(pool).await.map_err(e)?;
        database::providers::media::detach_stale(pool, Some(&[t])).await.map_err(de)?;
        let after_unlink: Vec<u64> = sqlx::query_scalar("SELECT media_id FROM release_media WHERE release_id = ? ORDER BY media_id").bind(r).fetch_all(pool).await.map_err(e)?;
        let checks = [
            (detached == 2 && left == vec![current, by_name, uploaded] && on_title == 0, format!("the old game's art comes off ({detached} detached), the rest stays: {left:?}, {on_title} on the title")),
            (after_unlink == vec![by_name, uploaded], format!("unlinked, the provider's art goes, the rest stays: {after_unlink:?}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{detached} stale attachment(s) removed")))
    })
    .await;
}

async fn phase_merge_audit(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "merge audit: unrelated merges decoupled, stray IGDB links removed", async {
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let src = import::ensure_source(pool, "redump").await.map_err(de)?;
        sqlx::query("INSERT INTO igdb_games (provider_id, title) VALUES (990098, 'Audit Quest'), (990099, 'Music Test Disc')").execute(pool).await.map_err(e)?;
        let mut ids = Vec::new();
        for (tname, igdb) in [("Audit Quest", "990098"), ("Audit Quest International", "990098"), ("Tales of Audit", "990098"), ("Unrelated Thing", "990099")] {
            let norm = tname.to_lowercase();
            let t = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, ?, ?)").bind(tname).bind(&norm).bind(import::sha1_hex(&norm)).execute(pool).await.map_err(e)?.last_insert_id();
            let rname = format!("{tname} (Japan)");
            let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, source_id) VALUES (?, ?, ?, ?, ?, 'JP', ?)")
                .bind(Platform::PlayStation.igdb_id()).bind(&rname).bind(import::sha1_hex(&rname)).bind(&norm).bind(import::sha1_hex(&norm)).bind(src)
                .execute(pool).await.map_err(e)?.last_insert_id();
            sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(t).bind(r).execute(pool).await.map_err(e)?;
            sqlx::query("INSERT INTO title_provider_links (title_id, provider, provider_id, confidence) VALUES (?, 'igdb', ?, 'hash')").bind(t).bind(igdb).execute(pool).await.map_err(e)?;
            sqlx::query("INSERT INTO title_locales (title_id, language, region, source, name) VALUES (?, 'ja', 'JP', 'igdb', 'テスト')").bind(t).execute(pool).await.map_err(e)?;
            ids.push(t);
        }
        database::title_merge::merge_titles(pool, &ids[..3], "igdb:990098", "hash").await.map_err(de)?;
        // A link from a search result that only shares a word (Fuga Hogeland -> Hogeland) is
        // dropped; the same pair as an exact-name link would only have to be related.
        sqlx::query("INSERT INTO igdb_games (provider_id, title) VALUES (990097, 'Hogeland')").execute(pool).await.map_err(e)?;
        let fuzzy_title = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES ('Fuga Hogeland', 'fuga hogeland', ?)").bind(import::sha1_hex("fuga hogeland")).execute(pool).await.map_err(e)?.last_insert_id();
        sqlx::query("INSERT INTO title_provider_links (title_id, provider, provider_id, confidence) VALUES (?, 'igdb', '990097', 'fuzzy')").bind(fuzzy_title).execute(pool).await.map_err(e)?;
        let progress = database::jobs::Progress::new(pool, 0);
        let summary = database::merge_audit::audit(pool, &env.clients, &progress).await.map_err(de)?;
        let live = |t: u64| async move { sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM title WHERE id = ? AND deleted = FALSE").bind(t).fetch_one(pool).await };
        let (intl_live, tales_live) = (live(ids[1]).await.map_err(e)?, live(ids[2]).await.map_err(e)?);
        let stray_link: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title_provider_links WHERE title_id = ? AND provider = 'igdb' AND deleted = FALSE").bind(ids[3]).fetch_one(pool).await.map_err(e)?;
        let stray_locales: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title_locales WHERE title_id = ? AND source = 'igdb'").bind(ids[3]).fetch_one(pool).await.map_err(e)?;
        let fuzzy_left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title_provider_links WHERE title_id = ? AND provider = 'igdb' AND deleted = FALSE").bind(fuzzy_title).fetch_one(pool).await.map_err(e)?;
        let checks = [
            (tales_live == 1, "the unrelated merged title (Tales of Audit) is its own title again".to_string()),
            (intl_live == 0, "the related one (Audit Quest International) stays merged".to_string()),
            (stray_link == 0 && stray_locales == 0, format!("stray link removed ({stray_link}) with its IGDB names ({stray_locales})")),
            (fuzzy_left == 0, format!("a fuzzy link to a game that only shares a word is removed: {fuzzy_left}")),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        Ok(((), format!("{summary:?}")))
    })
    .await;
}

/// `counterparts`: the export pairs releases of one title by region / language.
async fn phase_counterparts(env: &Env, report: &mut Report) {
    let pool = &env.pool;
    step(report, "counterparts: release -> USA/English table", async {
        let e = |e: sqlx::Error| e.to_string();
        let de = |e: database::DatabaseError| e.to_string();
        let src = import::ensure_source(pool, "redump").await.map_err(de)?;
        let platform = Platform::GameCube.igdb_id();
        for (title, releases) in [
            ("cp test", vec![("CP Test (Japan)", "JP", "ja"), ("CP Test (Europe) (En,Fr)", "EU", "en,fr"), ("CP Test (USA)", "US", "en")]),
            ("cp korea", vec![("CP Korea (Korea)", "KR", "")]),
            ("cp japan", vec![("CP Japan (Japan)", "JP", "")]),
        ] {
            let t = sqlx::query("INSERT INTO title (name, norm_name, norm_hash) VALUES (?, ?, ?)").bind(title).bind(title).bind(import::sha1_hex(title)).execute(pool).await.map_err(e)?.last_insert_id();
            for (name, region, langs) in releases {
                let r = sqlx::query("INSERT INTO `release` (app_platform_id, name, name_hash, norm_name, norm_hash, region, languages, source_id) VALUES (?, ?, ?, ?, ?, ?, NULLIF(?, ''), ?)")
                    .bind(platform).bind(name).bind(import::sha1_hex(name)).bind(title).bind(import::sha1_hex(title)).bind(region).bind(langs).bind(src)
                    .execute(pool).await.map_err(e)?.last_insert_id();
                sqlx::query("INSERT INTO title_releases (title_id, release_id) VALUES (?, ?)").bind(t).bind(r).execute(pool).await.map_err(e)?;
            }
        }
        let rows = database::counterparts::build(pool).await.map_err(de)?;
        let get = |n: &str| rows.iter().find(|r| r.release == n).map(|r| r.counterpart.clone());
        let stripped = database::counterparts::stripped(&rows);
        let sget = |n: &str| stripped.iter().find(|r| r.release == n).map(|r| r.counterpart.clone());
        let checks = [
            (get("CP Test (Japan)") == Some(Some("CP Test (USA)".into())), format!("JP -> US: {:?}", get("CP Test (Japan)"))),
            (get("CP Test (Europe) (En,Fr)") == Some(Some("CP Test (USA)".into())), "EU -> US".to_string()),
            (get("CP Test (USA)").is_none(), "USA is a target, not a row".to_string()),
            (get("CP Korea (Korea)") == Some(None), "Korea-only -> empty".to_string()),
            (get("CP Japan (Japan)") == Some(Some("CP Japan (Japan)".into())), "Japan-only -> itself".to_string()),
            (sget("CP Test") == Some(Some("CP Test".into())), "stripped JP/EU collapse to one row".to_string()),
        ];
        if let Some((_, why)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(why.clone());
        }
        let csv = database::counterparts::to_csv(&rows);
        Ok(((), format!("{} rows, {} stripped, {} CSV bytes", rows.len(), stripped.len(), csv.len())))
    })
    .await;
}

async fn phase_download(env: &Env, report: &mut Report) {
    report.section("DAT download (bandwidth-limited)");
    if env.offline {
        report.skip("all sources", "ROMEXPLORER_DB_TEST_OFFLINE=1");
        return;
    }
    let dats_dir = env.paths.dats_dir();
    let big = env_flag("ROMEXPLORER_DB_TEST_BIG_DOWNLOADS");
    let plan: [(Source, Option<usize>); 6] = [(Source::Retrobios, None), (Source::RetroAchievements, Some(3)), (Source::MameRedump, Some(3)), (Source::Redump, Some(2)), (Source::NoIntro, None), (Source::Tosec, None)];
    for (source, limit) in plan {
        if source.is_large() && !big {
            report.skip(&format!("download {}", source.name()), "~100-150MB whole-catalog pack; set ROMEXPLORER_DB_TEST_BIG_DOWNLOADS=1 (samples/dats/ copy used for import instead)");
            continue;
        }
        let options = DownloadOptions { limit, ..Default::default() };
        let client = match dat_download::build_client(&options) {
            Ok(c) => c,
            Err(e) => {
                report.fail(&format!("download {}", source.name()), &e.to_string(), Duration::ZERO);
                continue;
            }
        };
        step(report, &format!("download {}", source.name()), async {
            let files = dat_download::fetch(&client, source, &dats_dir, &options).await.map_err(|e| e.to_string())?;
            let fresh = files.iter().filter(|f| f.fresh).count();
            let bytes: u64 = files.iter().map(|f| f.bytes).sum();
            let names: Vec<String> = files.iter().take(4).map(|f| f.path.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_string()).collect();
            Ok(((), format!("{} file(s), {fresh} fresh, {bytes} bytes; e.g. {}", files.len(), names.join(" | "))))
        })
        .await;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Where each source's DATs come from for the import phase: `<dats root>/<source>/`
/// first, the app download dir second.
fn source_files(env: &Env, source: Source) -> Vec<PathBuf> {
    let samples = inputs::dats_root();
    let mut files = dat_download::local_files(&samples, source);
    if files.is_empty() {
        files = dat_download::local_files(&env.paths.dats_dir(), source);
    }
    files
}

fn smallest(files: &[PathBuf], n: usize, filter: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut v: Vec<(u64, PathBuf)> = files.iter().filter(|p| filter(p)).map(|p| (std::fs::metadata(p).map(|m| m.len()).unwrap_or(u64::MAX), p.clone())).collect();
    v.sort();
    v.into_iter().take(n).map(|(_, p)| p).collect()
}

async fn import_files(env: &Env, report: &mut Report, source: Source, files: &[PathBuf], member_filter: &dyn Fn(&str) -> bool) -> Vec<ImportSummary> {
    let mut summaries = Vec::new();
    for path in files {
        let label = path.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_string();
        let started = Instant::now();
        let parsed = match dat_parse::parse_dat_path(path, member_filter) {
            Ok(p) => p,
            Err(e) => {
                report.fail(&format!("parse {} / {label}", source.name()), &e.to_string(), started.elapsed());
                continue;
            }
        };
        if parsed.is_empty() {
            report.skip(&format!("parse {} / {label}", source.name()), "no members matched the filter");
            continue;
        }
        let parse_ms = started.elapsed().as_millis();
        for entry in parsed {
            let started = Instant::now();
            match import::import_dat(&env.pool, source, &entry.name, &entry.dat, &ImportOptions::default()).await {
                Ok(s) => {
                    report.pass(
                        &format!("import {} / {}", source.name(), entry.name),
                        &format!("platform {:?}; {} games, {} files -> {} releases, {} new content (+{} reused), {} hashes, {} new titles, {} ra_hash (parse {parse_ms} ms)", s.platform, s.games, s.files, s.releases_upserted, s.content_new, s.content_reused, s.hashes_inserted, s.titles_new, s.ra_hashes),
                        started.elapsed(),
                    );
                    summaries.push(s);
                }
                Err(e) => report.fail(&format!("import {} / {}", source.name(), entry.name), &e.to_string(), started.elapsed()),
            }
        }
    }
    summaries
}

async fn phase_import(env: &Env, report: &mut Report) {
    report.section("DAT parse + import");
    let full = env_flag("ROMEXPLORER_DB_TEST_FULL_IMPORT");
    let all = |_: &str| true;

    // retroachievements: small per-system dats (structured region + ra_hash) -- all of them.
    let ra = source_files(env, Source::RetroAchievements);
    if ra.is_empty() {
        report.skip("retroachievements", "no DATs under samples/dats/retroachievements or the download dir");
    } else {
        let files = if full { ra } else { smallest(&ra, 12, |_| true) };
        import_files(env, report, Source::RetroAchievements, &files, &all).await;
    }

    let retrobios = source_files(env, Source::Retrobios);
    match retrobios.last() {
        Some(json) => {
            import_files(env, report, Source::Retrobios, std::slice::from_ref(json), &all).await;
        }
        None => report.skip("retrobios", "no database json found"),
    }

    let redump = source_files(env, Source::Redump);
    if redump.is_empty() {
        report.skip("redump", "no DATs found");
    } else {
        let files = if full { redump } else { smallest(&redump, 3, |p| p.extension().map(|e| e == "zip").unwrap_or(false)) };
        import_files(env, report, Source::Redump, &files, &all).await;
    }

    let mame = source_files(env, Source::MameRedump);
    if mame.is_empty() {
        report.skip("mame-redump", "no DATs found");
    } else {
        let files = if full { mame } else { smallest(&mame, 3, |_| true) };
        import_files(env, report, Source::MameRedump, &files, &all).await;
    }

    // Whole-catalog packs: a handful of platform members unless a full import is asked for.
    // "(#1234 + ...)" members are No-Intro's *numbered* variant of the same set (names carry a
    // catalog-number prefix): same content, different names -- skip them by default.
    let wanted_members = |name: &str| {
        let n = name.to_ascii_lowercase();
        n.starts_with("no-intro/") && !n.contains("(#") && (n.contains("game boy advance (") || n.contains("nintendo entertainment system (") || n.contains("mega drive - genesis ("))
    };
    let no_intro = source_files(env, Source::NoIntro);
    match no_intro.last() {
        Some(pack) if full => {
            import_files(env, report, Source::NoIntro, std::slice::from_ref(pack), &all).await;
        }
        Some(pack) => {
            import_files(env, report, Source::NoIntro, std::slice::from_ref(pack), &wanted_members).await;
        }
        None => report.skip("no-intro", "no pack found"),
    }
    let tosec_members = |name: &str| {
        let n = name.to_ascii_lowercase();
        n.contains("tosec/") && (n.contains("game boy advance - games") || n.contains("mega drive & genesis - games") || n.contains("mega drive - games"))
    };
    let tosec = source_files(env, Source::Tosec);
    match tosec.last() {
        Some(pack) if full => {
            import_files(env, report, Source::Tosec, std::slice::from_ref(pack), &all).await;
        }
        Some(pack) => {
            import_files(env, report, Source::Tosec, std::slice::from_ref(pack), &tosec_members).await;
        }
        None => report.skip("tosec", "no pack found"),
    }

    // Idempotency: re-importing one small dat must not add rows.
    if let Some(first) = source_files(env, Source::RetroAchievements).first() {
        let before = lookup::table_counts(&env.pool).await.unwrap_or_default();
        let started = Instant::now();
        match import::import_path(&env.pool, Source::RetroAchievements, first, &all, &ImportOptions::default()).await {
            Ok(_) => {
                let after = lookup::table_counts(&env.pool).await.unwrap_or_default();
                let changed: Vec<String> = before.iter().zip(after.iter()).filter(|(a, b)| a.1 != b.1 && matches!(a.0, "content" | "hashes" | "release" | "release_contents" | "title" | "title_releases")).map(|(a, b)| format!("{}: {} -> {}", a.0, a.1, b.1)).collect();
                if changed.is_empty() {
                    report.pass("re-import is idempotent", "catalog row counts unchanged", started.elapsed());
                } else {
                    report.fail("re-import is idempotent", &changed.join(", "), started.elapsed());
                }
            }
            Err(e) => report.fail("re-import is idempotent", &e.to_string(), started.elapsed()),
        }
    }

    match lookup::table_counts(&env.pool).await {
        Ok(counts) => {
            report.line("\n| table | rows |\n|---|---|");
            for (table, n) in counts {
                report.line(&format!("| {table} | {n} |"));
            }
        }
        Err(e) => report.fail("table counts", &e.to_string(), Duration::ZERO),
    }
}

/// A real catalog entry to build the lookup scenarios from: picked from the imported data
/// at run time, never hardcoded.
#[derive(Debug, Clone)]
struct Sample {
    content_id: u64,
    release_id: u64,
    release_name: String,
    title_id: Option<u64>,
    title_name: Option<String>,
    platform: Platform,
    file_name: String,
    size: Option<u64>,
    sha1: Option<String>,
    md5: Option<String>,
    crc32: Option<String>,
}

async fn pick_sample(pool: &MySqlPool, platform: Option<Platform>, offset: u32) -> Option<Sample> {
    let mut sql = String::from(
        "SELECT c.id AS content_id, r.id AS release_id, r.name AS release_name, t.id AS title_id, t.name AS title_name, r.app_platform_id, rc.file_name, c.size \
         FROM `release` r JOIN release_contents rc ON rc.release_id = r.id JOIN content c ON c.id = rc.content_id \
         LEFT JOIN title_releases tr ON tr.release_id = r.id LEFT JOIN title t ON t.id = tr.title_id \
         WHERE EXISTS (SELECT 1 FROM hashes h WHERE h.content_id = c.id AND h.algorithm = 'sha1') AND r.is_beta = FALSE AND r.is_prototype = FALSE AND r.is_demo = FALSE \
         AND r.name NOT LIKE '%Games in 1%' AND r.name NOT LIKE '% + %' AND r.name NOT LIKE '%(Unl)%' AND r.name NOT LIKE '%(Pirate)%'",
    );
    if platform.is_some() {
        sql.push_str(" AND r.app_platform_id = ?");
    }
    sql.push_str(" ORDER BY r.id LIMIT 1 OFFSET ?");
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
    if let Some(p) = platform {
        query = query.bind(p.igdb_id());
    }
    let row = query.bind(offset).fetch_optional(pool).await.ok()??;
    let content_id: u64 = row.try_get("content_id").ok()?;
    let platform_id: u16 = row.try_get("app_platform_id").ok()?;
    let hashes = sqlx::query("SELECT algorithm, value_hex FROM hashes WHERE content_id = ?").bind(content_id).fetch_all(pool).await.ok()?;
    let mut sample = Sample {
        content_id,
        release_id: row.try_get("release_id").ok()?,
        release_name: row.try_get("release_name").ok()?,
        title_id: row.try_get("title_id").ok()?,
        title_name: row.try_get("title_name").ok()?,
        platform: Platform::from_igdb_id(platform_id).unwrap_or(Platform::Unknown),
        file_name: row.try_get("file_name").ok()?,
        size: row.try_get("size").ok()?,
        sha1: None,
        md5: None,
        crc32: None,
    };
    for h in hashes {
        let algorithm: String = h.try_get("algorithm").ok()?;
        let value: String = h.try_get("value_hex").ok()?;
        match algorithm.as_str() {
            "sha1" => sample.sha1 = Some(value),
            "md5" => sample.md5 = Some(value),
            "crc32" => sample.crc32 = Some(value),
            _ => {}
        }
    }
    Some(sample)
}

fn describe(outcome: &MatchOutcome) -> String {
    match outcome {
        MatchOutcome::Hash { algorithm, matches } => format!("hash ({algorithm:?}): {} match(es); first: release {:?} / title {:?} / platform {:?}", matches.len(), matches.first().and_then(|m| m.release_name.clone()), matches.first().and_then(|m| m.title_name.clone()), matches.first().map(|m| m.platform)),
        MatchOutcome::Name { best, score, candidates } => format!("name: best release {} ({:?}) out of {candidates} candidate(s), score {:?}", best.name, best.platform, score),
        MatchOutcome::Ambiguous { candidates } => format!("ambiguous: {} tied candidate(s): {}", candidates.len(), candidates.iter().take(3).map(|c| c.name.clone()).collect::<Vec<_>>().join(" | ")),
        MatchOutcome::None => "none".to_string(),
    }
}

async fn phase_lookup(env: &Env, report: &mut Report) -> Option<Sample> {
    report.section("Lookup scenarios (hash-first, name-second)");
    let pool = &env.pool;
    let Some(sample) = pick_sample(pool, Some(Platform::GameBoyAdvance), 25).await.or(pick_sample(pool, None, 25).await) else {
        report.skip("lookup scenarios", "nothing imported to sample from");
        return None;
    };
    report.line(&format!("- sample: release `{}` (platform {:?}, file `{}`, sha1 {}, md5 {}, crc32 {})", escape(&sample.release_name), sample.platform, escape(&sample.file_name), sample.sha1.as_deref().unwrap_or("-"), sample.md5.as_deref().unwrap_or("-"), sample.crc32.as_deref().unwrap_or("-")));
    let fake_sha1 = "0123456789abcdef0123456789abcdef01234567";
    let real_sha1 = sample.sha1.clone().unwrap_or_default();
    let title = sample.title_name.clone().unwrap_or_else(|| sample.release_name.clone());
    let release_id = sample.release_id;

    step(report, "A: existing hash", async {
        let outcome = resolve(pool, &ResolveRequest::new().hash(HashAlgorithms::Sha1, real_sha1.clone())).await.map_err(|e| e.to_string())?;
        match &outcome {
            MatchOutcome::Hash { matches, .. } if matches.iter().any(|m| m.release_id == Some(release_id)) => Ok(((), describe(&outcome))),
            other => Err(format!("expected a hash match on release {release_id}, got {}", describe(other))),
        }
    })
    .await;

    step(report, "B: made-up hash + existing title", async {
        let outcome = resolve(pool, &ResolveRequest::new().hash(HashAlgorithms::Sha1, fake_sha1).name(&title).platform(sample.platform)).await.map_err(|e| e.to_string())?;
        match &outcome {
            MatchOutcome::Name { best, .. } if best.title_id == sample.title_id || best.release_id == release_id => Ok(((), describe(&outcome))),
            MatchOutcome::Ambiguous { candidates } if candidates.iter().any(|c| c.release_id == release_id) => Ok(((), format!("{} (the sample release is among the tied candidates -- regional variants share a title)", describe(&outcome)))),
            other => Err(format!("expected a name match on title {:?}, got {}", sample.title_name, describe(other))),
        }
    })
    .await;

    step(report, "C: existing hash + random title (hash must win)", async {
        let outcome = resolve(pool, &ResolveRequest::new().hash(HashAlgorithms::Sha1, real_sha1.clone()).name("zzq nonsense title 9999")).await.map_err(|e| e.to_string())?;
        match &outcome {
            MatchOutcome::Hash { matches, .. } if matches.iter().any(|m| m.release_id == Some(release_id)) => Ok(((), describe(&outcome))),
            other => Err(format!("expected the hash to win, got {}", describe(other))),
        }
    })
    .await;

    step(report, "D: made-up hash + made-up title (no match)", async {
        let outcome = resolve(pool, &ResolveRequest::new().hash(HashAlgorithms::Sha1, fake_sha1).name("zzq nonsense title 9999")).await.map_err(|e| e.to_string())?;
        match &outcome {
            MatchOutcome::None => Ok(((), "none, as expected".to_string())),
            other => Err(format!("expected no match, got {}", describe(other))),
        }
    })
    .await;

    if let Some(crc) = sample.crc32.clone() {
        step(report, "E: crc32-only lookup still finds the content (lookup, not dedup)", async {
            let matches = lookup::lookup_by_hash(pool, HashAlgorithms::Crc32, &crc).await.map_err(|e| e.to_string())?;
            if matches.iter().any(|m| m.content_id == sample.content_id) {
                Ok(((), format!("{} match(es) for crc32 {crc}", matches.len())))
            } else {
                Err("sample content not found by crc32".to_string())
            }
        })
        .await;
    }
    Some(sample)
}

struct ProviderHits {
    igdb_first: Option<(String, Option<i32>)>,
    sgdb_first: Option<i64>,
    moby_first: Option<i64>,
    ra_game: Option<u32>,
}

async fn phase_providers(env: &Env, report: &mut Report, sample: Option<&Sample>) -> ProviderHits {
    report.section("Provider requests (single + list per available provider)");
    let mut hits = ProviderHits { igdb_first: None, sgdb_first: None, moby_first: None, ra_game: None };
    if env.offline {
        report.skip("all providers", "ROMEXPLORER_DB_TEST_OFFLINE=1");
        return hits;
    }
    let Some(sample) = sample else {
        report.skip("all providers", "no catalog sample to query with");
        return hits;
    };
    let pool = &env.pool;
    let clients = &env.clients;
    let store = &env.store;
    let name = sample.title_name.clone().unwrap_or_else(|| sample.release_name.clone());
    let platform = sample.platform;
    let title_id = sample.title_id;
    let pause = || tokio::time::sleep(Duration::from_secs(2));

    // Hasheous: hash-keyed single lookup.
    if clients.is_available(Provider::Hasheous) {
        step(report, "hasheous single (lookup by the sample's hashes)", async {
            let request = cache::hasheous_request_for_content(pool, sample.content_id).await.map_err(|e| e.to_string())?;
            let record = cache::hasheous_lookup(clients, pool, store, sample.content_id, &request).await.map_err(|e| e.to_string())?;
            let refs: Vec<String> = record.cross_refs.iter().map(|(p, id, n)| format!("{}={id} ({n} link)", p.name())).collect();
            Ok(((), format!("hit={} name={:?} raw={:?} cross-refs: [{}]", record.hit, record.name, record.raw_path.as_ref().map(|p| p.file_name().unwrap_or_default().to_string_lossy().to_string()), refs.join(", "))))
        })
        .await;
        pause().await;
    } else {
        report.skip("hasheous", "client unavailable");
    }

    // IGDB: search by name (single) + get_games_by_id (list).
    if clients.is_available(Provider::Igdb) {
        let entries = step(report, "igdb single (search by name + platform)", async {
            let entries = cache::igdb_search(clients, pool, store, &name, Some(platform)).await.map_err(|e| e.to_string())?;
            let shown: Vec<String> = entries.iter().take(3).map(|e| format!("{}:{}", e.provider_id, e.title.clone().unwrap_or_default())).collect();
            Ok((entries, format!("{} result(s): {}", shown.len(), shown.join(" | "))))
        })
        .await;
        if let Some(entries) = entries {
            if let Some(first) = entries.first() {
                let cover = first.raw_path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()).and_then(|v| v.get("cover").and_then(|c| c.as_i64())).map(|c| c as i32);
                hits.igdb_first = Some((first.provider_id.clone(), cover));
                if let Some(title_id) = title_id {
                    let _ = cache::link_title(pool, title_id, Provider::Igdb, &first.provider_id, cache::Confidence::Fuzzy).await;
                }
            }
            pause().await;
            let ids: Vec<i32> = entries.iter().take(3).filter_map(|e| e.provider_id.parse().ok()).collect();
            if !ids.is_empty() {
                step(report, "igdb list (get_games_by_id, batched)", async {
                    let got = cache::igdb_games_by_ids(clients, pool, store, &ids).await.map_err(|e| e.to_string())?;
                    Ok(((), format!("asked {} id(s), got {}", ids.len(), got.len())))
                })
                .await;
                pause().await;
            }
        }
    } else {
        report.skip("igdb", "client unavailable");
    }

    if clients.is_available(Provider::TheGamesDb) {
        step(report, "thegamesdb single (search by name)", async {
            let entries = cache::thegamesdb_search(clients, pool, store, &name).await.map_err(|e| e.to_string())?;
            let shown: Vec<String> = entries.iter().take(3).map(|e| format!("{}:{}", e.provider_id, e.title.clone().unwrap_or_default())).collect();
            Ok(((), format!("{} result(s): {}", entries.len(), shown.join(" | "))))
        })
        .await;
        pause().await;
    } else {
        report.skip("thegamesdb", "client unavailable");
    }

    if clients.is_available(Provider::SteamGridDb) {
        let entries = step(report, "steamgriddb single (search by name)", async {
            let entries = cache::steamgriddb_search(clients, pool, store, &name).await.map_err(|e| e.to_string())?;
            let shown: Vec<String> = entries.iter().take(3).map(|e| format!("{}:{}", e.provider_id, e.title.clone().unwrap_or_default())).collect();
            Ok((entries, format!("{} result(s): {}", shown.len(), shown.join(" | "))))
        })
        .await;
        hits.sgdb_first = entries.and_then(|e| e.first().and_then(|f| f.provider_id.parse().ok()));
        pause().await;
    } else {
        report.skip("steamgriddb", "client unavailable");
    }

    if clients.is_available(Provider::MobyGames) {
        let entries = step(report, "mobygames single (search by name)", async {
            let entries = cache::mobygames_search(clients, pool, store, &name, None).await.map_err(|e| e.to_string())?;
            let shown: Vec<String> = entries.iter().take(3).map(|e| format!("{}:{}", e.provider_id, e.title.clone().unwrap_or_default())).collect();
            Ok((entries, format!("{} result(s): {}", shown.len(), shown.join(" | "))))
        })
        .await;
        hits.moby_first = entries.and_then(|e| e.first().and_then(|f| f.provider_id.parse().ok()));
        pause().await;
    } else {
        report.skip("mobygames", "client unavailable");
    }

    if clients.is_available(Provider::EmuReady) {
        step(report, "emuready single (system lookup + search by name)", async {
            let system = cache::emuready_system_id(clients, platform).await.map_err(|e| e.to_string())?;
            let Some(system_id) = system else {
                let names: Vec<String> = clients.emuready.as_ref().unwrap().list_systems().await.map(|s| s.iter().map(|x| x.name.clone()).collect()).unwrap_or_default();
                return Ok(((), format!("no EmuReady system matches {platform:?}; systems listed: {}", names.join(", "))));
            };
            let entries = cache::emuready_search(clients, pool, store, &system_id, &name).await.map_err(|e| e.to_string())?;
            let shown: Vec<String> = entries.iter().take(3).map(|e| format!("{}:{}", e.provider_id, e.title.clone().unwrap_or_default())).collect();
            Ok(((), format!("system {system_id}: {} result(s): {}", entries.len(), shown.join(" | "))))
        })
        .await;
        pause().await;
    } else {
        report.skip("emuready", "client unavailable");
    }

    if clients.is_available(Provider::ScreenScraper) {
        step(report, "screenscraper single (system lookup + game by hashes)", async {
            let system = cache::screenscraper_system_id(clients, platform).await.map_err(|e| e.to_string())?;
            let Some(system_id) = system else { return Ok(((), format!("no ScreenScraper system matches {platform:?}"))) };
            let entry = cache::screenscraper_by_hashes(clients, pool, store, Some(sample.content_id), system_id, &sample.file_name, sample.size.map(|s| s as i64), sample.md5.as_deref(), sample.sha1.as_deref(), sample.crc32.as_deref()).await.map_err(|e| e.to_string())?;
            Ok(((), format!("system {system_id}: {}", entry.map(|e| format!("jeu {} {:?}", e.provider_id, e.title)).unwrap_or_else(|| "no match".to_string()))))
        })
        .await;
        pause().await;
    } else {
        report.skip("screenscraper", "client unavailable (needs screenscraper_dev_id/screenscraper_dev_password in api.env)");
    }

    if clients.is_available(Provider::RetroAchievements) {
        let console = step(report, "retroachievements single (console list)", async {
            let found = bulk::ra_console_id_for(clients, platform).await.map_err(|e| e.to_string())?;
            Ok((found.clone(), format!("{platform:?} -> RA console {:?}", found)))
        })
        .await
        .flatten();
        pause().await;
        if let Some((console_id, _)) = console {
            // A single game id is only known after the bulk list; peek at one page via the
            // list call restricted to games with achievements (still one request).
            step(report, "retroachievements single (get_game for the first game on that console)", async {
                let client = clients.retroachievements.as_ref().unwrap();
                let (entries, _) = client.get_game_list(console_id, false, true).await.map_err(|e| e.to_string())?;
                let Some(first) = entries.first() else { return Ok(((), "console has no games".to_string())) };
                let (entry, game) = cache::retroachievements_game(clients, pool, store, first.id).await.map_err(|e| e.to_string())?;
                let genres = romexplorer::database::providers::ra_genre::normalize_ra_genres(game.genre.as_deref().unwrap_or(""));
                if let Some(title_id) = cache::link_by_name(pool, Provider::RetroAchievements, &entry.provider_id, &game.title, Some(platform)).await.map_err(|e| e.to_string())? {
                    cache::set_title_genres(pool, title_id, "retroachievements", &genres).await.map_err(|e| e.to_string())?;
                }
                hits.ra_game = Some(game.id);
                Ok(((), format!("game {} {:?} genre {:?} -> {:?}, box art {:?}", game.id, game.title, game.genre, genres, game.image_box_art)))
            })
            .await;
            pause().await;
        }
    } else {
        report.skip("retroachievements", "client unavailable (needs ra_username/ra_api_key in api.env)");
    }
    hits
}

async fn phase_media(env: &Env, report: &mut Report, sample: Option<&Sample>, hits: &ProviderHits) {
    report.section("Media (one image per provider, spaced out)");
    if env.offline {
        report.skip("media", "ROMEXPLORER_DB_TEST_OFFLINE=1");
        return;
    }
    let Some(sample) = sample else {
        report.skip("media", "no sample");
        return;
    };
    let target = match sample.title_id {
        Some(id) => media::MediaTarget::Title(id),
        None => media::MediaTarget::Release(sample.release_id),
    };
    let pool = &env.pool;
    let clients = &env.clients;
    let media_dir = env.paths.media_dir();
    // "Do something else in between": a catalog lookup between downloads, plus a pause.
    let sample_sha1 = sample.sha1.clone().unwrap_or_default();
    async fn between(report: &mut Report, pool: &MySqlPool, sha1: &str) {
        tokio::time::sleep(Duration::from_secs(5)).await;
        if let Ok(m) = lookup::lookup_by_hash(pool, HashAlgorithms::Sha1, sha1).await {
            report.line(&format!("  - (between downloads) hash lookup still answers: {} match(es)", m.len()));
        }
    }

    if let Some((game_id, Some(cover_id))) = &hits.igdb_first {
        step(report, "igdb cover download", async {
            let url = media::igdb_cover_url(clients, *cover_id).await.map_err(|e| e.to_string())?.ok_or("no cover url")?;
            let rec = media::download_media(&env.http, pool, &media_dir, Provider::Igdb, None, &url, "cover", Some("front"), target).await.map_err(|e| e.to_string())?;
            Ok(((), format!("igdb game {game_id}: media #{} {} bytes -> {}", rec.media_id, rec.bytes, rec.path.display())))
        })
        .await;
        between(report, pool, &sample_sha1).await;
    } else {
        report.skip("igdb cover", "no IGDB result with a cover");
    }

    if let Some(game_id) = hits.sgdb_first {
        step(report, "steamgriddb grid download", async {
            let url = media::steamgriddb_grid_url(clients, game_id).await.map_err(|e| e.to_string())?.ok_or("no grid for this game")?;
            let rec = media::download_media(&env.http, pool, &media_dir, Provider::SteamGridDb, None, &url, "cover", None, target).await.map_err(|e| e.to_string())?;
            Ok(((), format!("sgdb game {game_id}: media #{} {} bytes -> {}", rec.media_id, rec.bytes, rec.path.display())))
        })
        .await;
        between(report, pool, &sample_sha1).await;
    } else {
        report.skip("steamgriddb grid", "no SteamGridDB result");
    }

    if let Some(game_id) = hits.moby_first {
        step(report, "mobygames cover download", async {
            let url = media::mobygames_cover_url(clients, game_id).await.map_err(|e| e.to_string())?.ok_or("no sample cover")?;
            let rec = media::download_media(&env.http, pool, &media_dir, Provider::MobyGames, None, &url, "cover", Some("front"), target).await.map_err(|e| e.to_string())?;
            Ok(((), format!("moby game {game_id}: media #{} {} bytes -> {}", rec.media_id, rec.bytes, rec.path.display())))
        })
        .await;
        between(report, pool, &sample_sha1).await;
    } else {
        report.skip("mobygames cover", "no MobyGames result");
    }

    if let Some(game_id) = hits.ra_game {
        step(report, "retroachievements box art download", async {
            let client = clients.retroachievements.as_ref().ok_or("ra unavailable")?;
            let (game, _) = client.get_game(game_id).await.map_err(|e| e.to_string())?;
            let url = media::retroachievements_boxart_url(&game).ok_or("no box art")?;
            let rec = media::download_media(&env.http, pool, &media_dir, Provider::RetroAchievements, None, &url, "box", Some("front"), target).await.map_err(|e| e.to_string())?;
            Ok(((), format!("ra game {game_id}: media #{} {} bytes -> {}", rec.media_id, rec.bytes, rec.path.display())))
        })
        .await;
    } else {
        report.skip("retroachievements box art", "no RA game (needs credentials)");
    }
    report.line("- thegamesdb/screenscraper/emuready media: not exposed by the vendored clients (TheGamesDB images need its separate Images endpoint; ScreenScraper media isn't in the vendored model) -- deferred.");
}

async fn phase_bulk(env: &Env, report: &mut Report) {
    report.section("Bulk by_platform sync (last: bandwidth/rate-limit heavy)");
    if env.offline {
        report.skip("bulk", "ROMEXPLORER_DB_TEST_OFFLINE=1");
        return;
    }
    if env_flag("ROMEXPLORER_DB_TEST_SKIP_BULK") {
        report.skip("bulk", "ROMEXPLORER_DB_TEST_SKIP_BULK=1");
        return;
    }
    let pool = &env.pool;
    let clients = &env.clients;
    let store = &env.store;
    // One page (500 games) of a platform the import phase actually loaded, so the by-name
    // linking has catalog titles to hit; RA's per-console list is one request either way.
    let platform = Platform::GameBoyAdvance;

    if clients.is_available(Provider::Igdb) {
        step(report, "igdb by_platform (one page)", async {
            let s = bulk::sync_igdb_platform(clients, pool, store, platform, Some(1), None, bulk::SyncMode::Redownload, &romexplorer::database::jobs::Progress::none()).await.map_err(|e| e.to_string())?;
            Ok(((), format!("{platform:?}: {} games over {} page(s), {} linked to catalog titles by name", s.games, s.pages, s.linked)))
        })
        .await;
        tokio::time::sleep(Duration::from_secs(3)).await;
    } else {
        report.skip("igdb by_platform", "client unavailable");
    }

    if clients.is_available(Provider::RetroAchievements) {
        step(report, "retroachievements by_platform (one console, with hashes)", async {
            let (console_id, console_name) = bulk::ra_console_id_for(clients, platform).await.map_err(|e| e.to_string())?.ok_or("RA has no such console")?;
            let s = bulk::sync_retroachievements_console(clients, pool, store, console_id, Some(platform), bulk::SyncMode::Redownload).await.map_err(|e| e.to_string())?;
            Ok(((), format!("{platform:?} = RA console {console_id} ({console_name}): {} games, {} md5 hashes, {} linked to catalog releases by md5", s.games, s.hashes, s.linked)))
        })
        .await;
    } else {
        report.skip("retroachievements by_platform", "client unavailable");
    }

    match lookup::table_counts(pool).await {
        Ok(counts) => {
            report.line("\n| table | rows (after providers + bulk) |\n|---|---|");
            for (table, n) in counts {
                report.line(&format!("| {table} | {n} |"));
            }
        }
        Err(e) => report.fail("table counts", &e.to_string(), Duration::ZERO),
    }
}

/// The read-only check first-run setup makes before it offers a database: is this one
/// usable, empty, or from a schema with no upgrade path? Verified against a real server,
/// because the failure mode being guarded against (a migration edited after a database
/// applied it) only exists in `_sqlx_migrations`.
#[test]
fn probe_tells_a_usable_database_from_one_that_is_too_old() {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    runtime.block_on(async {
        let paths = AppPaths::from_root(crate_dir().join("app_root_ex"));
        check_layout(&paths.mariadb_dir()).expect("layout");

        let base = crate_dir().join("tests/output/mariadb_probe");
        let _ = std::fs::remove_dir_all(&base);
        let port = free_port().expect("free port");
        let mut server = MariaDbServer::new(small_pool(ServerConfig::new(paths.mariadb_dir(), base.join("data"), port)));
        server.start().await.expect("start");

        let config = DatabaseConfig::default().with_port(port).with_database("romexplorer_probe_test");

        // Nothing there yet.
        assert_eq!(database::probe(&config).await.expect("probe empty"), database::DatabaseState::Empty);

        // After a normal open, everything this build carries is applied.
        let pool = database::initialize(&config).await.expect("initialize");
        match database::probe(&config).await.expect("probe ready") {
            database::DatabaseState::Ready { applied, pending } => {
                assert!(applied > 0, "a migrated database should report applied migrations");
                assert_eq!(pending, 0, "nothing should be left pending straight after initialize");
            }
            other => panic!("expected Ready, got {other:?}"),
        }

        // Now the case that started this: a migration edited after the database applied it.
        // That is exactly what the pre-`local_*` schema looks like -- its migration 1 no
        // longer matches the file, so there is no upgrade path and setup must say so
        // rather than failing later inside sqlx.
        sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = 1").bind(vec![0u8; 48]).execute(&pool).await.expect("tamper");
        match database::probe(&config).await.expect("probe too old") {
            database::DatabaseState::TooOld { reason } => assert!(reason.contains("migration 1"), "reason should name the migration: {reason}"),
            other => panic!("expected TooOld, got {other:?}"),
        }

        // A database written by a newer build is also refused, and distinguishably so.
        sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 1").execute(&pool).await.expect("untamper");
        sqlx::query("INSERT INTO _sqlx_migrations (version, description, installed_on, success, checksum, execution_time) VALUES (999999, 'from the future', NOW(), TRUE, ?, 0)")
            .bind(vec![1u8; 48])
            .execute(&pool)
            .await
            .expect("future migration");
        match database::probe(&config).await.expect("probe future") {
            database::DatabaseState::TooOld { reason } => assert!(reason.contains("newer build"), "reason should say it is newer: {reason}"),
            other => panic!("expected TooOld, got {other:?}"),
        }

        drop(pool);
        let _ = server.shutdown(Duration::from_secs(30)).await;
    });
}

/// Local-only, no network: `initial_setup` against its own throwaway server/datadir.
/// Verifies root gets a password, the admin account works, a separate app account gets
/// only the least-privilege grants, and the old passwordless root no longer connects.
#[test]
fn initial_setup_end_to_end() {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    runtime.block_on(async {
        let paths = AppPaths::from_root(crate_dir().join("app_root_ex"));
        check_layout(&paths.mariadb_dir()).expect("layout");

        let base = crate_dir().join("tests/output/mariadb_initial_setup");
        let _ = std::fs::remove_dir_all(&base);
        let datadir = base.join("data");
        let port = free_port().expect("free port");
        let mut server = MariaDbServer::new(small_pool(ServerConfig::new(paths.mariadb_dir(), &datadir, port)));

        let setup = database::InitialSetup {
            database: "romexplorer_initial_setup_test",
            root_password: "r00t-s3cret-throwaway",
            admin_user: "ex_admin",
            admin_password: "adm1n-s3cret-throwaway",
            app_user: Some(("ex_app", "app-s3cret-throwaway")),
        };
        let app_config = database::initial_setup(&mut server, setup).await.expect("initial_setup");
        assert_eq!(app_config.user, "ex_app");

        // The returned config (the app account) can connect and read the seeded platforms.
        let pool = database::connect(&app_config).await.expect("connect as app user");
        let n: i64 = sqlx::query("SELECT COUNT(*) AS n FROM platforms").fetch_one(&pool).await.expect("query as app user").try_get("n").expect("n");
        assert!(n > 0, "app user could not read seeded platforms");

        // The app account has no DDL rights: creating a table must fail.
        let ddl = sqlx::query("CREATE TABLE should_not_be_allowed (id INT)").execute(&pool).await;
        assert!(ddl.is_err(), "app user (least privilege) was able to run DDL");

        // The admin account can connect too, and does have DDL rights.
        let admin_config = DatabaseConfig::default().with_port(port).with_database("romexplorer_initial_setup_test").with_user("ex_admin").with_password("adm1n-s3cret-throwaway");
        let admin_pool = database::connect(&admin_config).await.expect("connect as admin");
        sqlx::query("CREATE TABLE admin_can_ddl (id INT)").execute(&admin_pool).await.expect("admin user could not run DDL");

        // The original passwordless root account no longer works.
        let old_root = DatabaseConfig::default().with_port(port).with_database("romexplorer_initial_setup_test");
        assert!(database::connect(&old_root).await.is_err(), "passwordless root still worked after set_root_password");

        drop(pool);
        drop(admin_pool);

        // --- the part setup alone never exercised: stopping and starting again ---------
        //
        // Readiness polling and graceful shutdown both connect as root, so a datadir that
        // now HAS a root password is a different problem from the one bootstrap left
        // behind. Before this was fixed, `shutdown` couldn't send SQL `SHUTDOWN` and fell
        // through to killing the process, and the next `start` spun for the full 60 s
        // before reporting a server that was actually up and serving.
        server.shutdown(Duration::from_secs(30)).await.expect("graceful shutdown with a root password set");
        assert!(!server.is_running(), "server still running after shutdown");

        // A fresh handle for the same datadir, as a later run of the app would build it --
        // the password has to come back from somewhere (Part 6: database.env).
        let mut restarted = MariaDbServer::new(small_pool(ServerConfig::new(paths.mariadb_dir(), &datadir, port).with_root_password("r00t-s3cret-throwaway")));
        let began = std::time::Instant::now();
        restarted.start().await.expect("restart a datadir that has a root password");
        assert!(began.elapsed() < Duration::from_secs(30), "start took {:?} -- readiness polling is not presenting the root password", began.elapsed());

        // And it is really serving, not merely spawned.
        let again = database::connect(&app_config).await.expect("connect as app user after restart");
        let n: i64 = sqlx::query("SELECT COUNT(*) AS n FROM platforms").fetch_one(&again).await.expect("query after restart").try_get("n").expect("n");
        assert!(n > 0);
        drop(again);

        // A handle that does NOT know the password must still come up: readiness is about
        // whether the server answers, and "access denied" is an answer.
        restarted.shutdown(Duration::from_secs(30)).await.expect("second graceful shutdown");
        let mut blind = MariaDbServer::new(small_pool(ServerConfig::new(paths.mariadb_dir(), &datadir, port)));
        let began = std::time::Instant::now();
        blind.start().await.expect("start without knowing the root password");
        assert!(began.elapsed() < Duration::from_secs(30), "start without the password took {:?}", began.elapsed());
        let _ = blind.shutdown(Duration::from_secs(30)).await; // no password: falls back to kill
    });
}

#[test]
fn database_end_to_end() {
    let _ = env_logger::builder().is_test(true).try_init();
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    let mut report = Report::new("database_end_to_end");
    // On the heap, and each phase boxed too: every phase adds to this future, and polled in
    // place -- or with the phases inline in it -- it outgrew the test thread's stack.
    let failed = runtime.block_on(Box::pin(async {
        let mut env = match bring_up(&mut report).await {
            Ok(env) => env,
            Err(err) => {
                report.fail("bring-up", &err, Duration::ZERO);
                report.write("database_end_to_end");
                panic!("database bring-up failed: {err}");
            }
        };
        Box::pin(phase_jobs(&env, &mut report)).await;
        Box::pin(phase_release_merge(&env, &mut report)).await;
        Box::pin(phase_title_merge(&env, &mut report)).await;
        Box::pin(phase_title_merge_incremental(&env, &mut report)).await;
        Box::pin(phase_igdb_reapply_selected(&env, &mut report)).await;
        Box::pin(phase_name_dictionary(&env, &mut report)).await;
        Box::pin(phase_localized_by_region(&env, &mut report)).await;
        Box::pin(phase_release_editions(&env, &mut report)).await;
        Box::pin(phase_addon_bundle(&env, &mut report)).await;
        Box::pin(phase_status_and_order(&env, &mut report)).await;
        Box::pin(phase_combine_igdb_games(&env, &mut report)).await;
        Box::pin(phase_rerelease_editions(&env, &mut report)).await;
        Box::pin(phase_set_entries(&env, &mut report)).await;
        Box::pin(phase_platform_merges(&env, &mut report)).await;
        Box::pin(phase_wrong_link_merges(&env, &mut report)).await;
        Box::pin(phase_media_resume(&env, &mut report)).await;
        Box::pin(phase_title_sizes(&env, &mut report)).await;
        Box::pin(phase_thegamesdb_regions(&env, &mut report)).await;
        Box::pin(phase_split_release(&env, &mut report)).await;
        Box::pin(phase_display_data(&env, &mut report)).await;
        Box::pin(phase_color_handhelds(&env, &mut report)).await;
        Box::pin(phase_platformless_dats(&env, &mut report)).await;
        Box::pin(phase_copy_priority_file(&env, &mut report)).await;
        Box::pin(phase_copy_one_format(&env, &mut report)).await;
        Box::pin(phase_bulk_release_moves(&env, &mut report)).await;
        Box::pin(phase_split_log(&env, &mut report)).await;
        Box::pin(phase_unlicensed_apart(&env, &mut report)).await;
        Box::pin(phase_ra_subsets_apart(&env, &mut report)).await;
        Box::pin(phase_undo_reopens_suggestion(&env, &mut report)).await;
        Box::pin(phase_review_layout(&env, &mut report)).await;
        Box::pin(phase_locale_edit(&env, &mut report)).await;
        Box::pin(phase_document_backfill(&env, &mut report)).await;
        Box::pin(phase_standalone(&env, &mut report)).await;
        Box::pin(phase_patched_import(&env, &mut report)).await;
        Box::pin(phase_bios(&env, &mut report)).await;
        Box::pin(phase_dat_supersede(&env, &mut report)).await;
        Box::pin(phase_ra_into_sets(&env, &mut report)).await;
        Box::pin(phase_genre_source(&env, &mut report)).await;
        Box::pin(phase_ra_patches(&env, &mut report)).await;
        Box::pin(phase_merge_audit(&env, &mut report)).await;
        Box::pin(phase_demo_of_another(&env, &mut report)).await;
        Box::pin(phase_stale_media(&env, &mut report)).await;
        Box::pin(phase_counterparts(&env, &mut report)).await;
        Box::pin(phase_download(&env, &mut report)).await;
        Box::pin(phase_import(&env, &mut report)).await;
        Box::pin(phase_backup(&env, &mut report)).await;
        let sample = Box::pin(phase_lookup(&env, &mut report)).await;
        let hits = Box::pin(phase_providers(&env, &mut report, sample.as_ref())).await;
        Box::pin(phase_media(&env, &mut report, sample.as_ref(), &hits)).await;
        Box::pin(phase_bulk(&env, &mut report)).await;

        let started = Instant::now();
        match env.server.shutdown(Duration::from_secs(30)).await {
            Ok(()) => report.pass("server shutdown (SQL SHUTDOWN)", "clean", started.elapsed()),
            Err(e) => report.fail("server shutdown", &e.to_string(), started.elapsed()),
        }
        report.write("database_end_to_end");
        report.failed
    }));
    assert_eq!(failed, 0, "{failed} step(s) failed -- see tests/results/database_end_to_end.md");
}
