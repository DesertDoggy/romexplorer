//! `romexplorer_worker`: the background worker (`romexplorer::worker`) as its own process,
//! for running jobs without the REST server. `romexplorer_server` normally runs the same
//! worker in-process -- start this only with `romexplorer_server --no-worker`, since two
//! workers on one set of credentials get two rate-limit budgets.
//!
//! Env: `ROMEXPLORER_APP_ROOT` (default: detected), `ROMEXPLORER_DB_*` (see
//! `database::config`), `ROMEXPLORER_WORKER_POLL_SECS` (default 5),
//! `ROMEXPLORER_WORKER_ONCE=1` to drain what's pending and exit (used by tests). Ctrl+C
//! stops it: running jobs are re-queued.

use romexplorer::api::App;
use romexplorer::database::{self, AppPaths, DatabaseConfig, DatabaseError};
use romexplorer::worker::{self, Worker, WorkerOptions};

#[tokio::main]
async fn main() {
    // Info by default: a long-running server/worker should say what its jobs are doing;
    // RUST_LOG still overrides.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,sqlx=warn")).init();
    romexplorer::common::fd_limit::raise_and_log();
    if let Err(err) = run().await {
        eprintln!("romexplorer-worker: {err}");
        std::process::exit(1);
    }
    // Not a plain return: the runtime would wait for an aborted job's blocking thread (a
    // scan still hashing its file). The jobs are already back to `pending`.
    std::process::exit(0);
}

async fn run() -> Result<(), DatabaseError> {
    let paths = AppPaths::detect();
    let config = DatabaseConfig::from_env();
    let pool = database::initialize(&config).await?;
    let app = App::with_pool(pool, paths, config)?;
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        romexplorer::lifecycle::stop_requested().await;
        log::info!("stopping");
        let _ = stop_tx.send(true);
    });
    worker::run_scheduler(Worker::new(app).await?, WorkerOptions::from_env(), stop_rx).await;
    Ok(())
}
