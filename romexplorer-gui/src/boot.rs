//! Desktop start-up: open this machine's own database and serve the GUI from it, in this
//! process. No `romexplorer_server` child, no socket, no `data/server_token`.
//!
//! This is `docs/plan/api.md`'s client model: a client machine owns its catalog, and REST
//! is only for reaching a database that isn't in your process. What used to happen here --
//! probe port 7337, spawn a server, wait up to 90 s for it to answer -- is exactly what
//! that model says should not be needed.
//!
//! What this still owes the old arrangement is the background worker. The server hosted
//! one, and it outlived the window; here it runs alongside the UI and stops with
//! [`Boot::shutdown`]. Two workers on one database would split every provider's rate-limit
//! budget in half, so this one only starts if it wins a database-wide advisory lock -- if a
//! `romexplorer_server` is already running against the same database, the GUI quietly
//! leaves the jobs to it.

use std::time::Duration;

use romexplorer::api::App;
use romexplorer::database::config::{AppPaths, StoredConfig};
use romexplorer::database::server::{check_layout, MariaDbServer, ServerConfig};
use romexplorer::worker::{self, Worker, WorkerOptions};
use romexplorer::database::MySqlPoolConnection;

use crate::api::local::LocalBackend;

/// What start-up leaves running, so quitting can stop it in the right order.
pub struct Boot {
    embedded: Option<MariaDbServer>,
    stop_worker: tokio::sync::watch::Sender<bool>,
    worker: Option<tokio::task::JoinHandle<()>>,
    /// The connection holding the worker lock; dropping it releases the lock.
    lock: Option<MySqlPoolConnection>,
}

impl Boot {
    /// Stops the worker (running jobs go back to `pending`, their try not counted), releases
    /// the worker lock, then stops the embedded database -- in that order, because the first
    /// two need it still up.
    pub async fn shutdown(mut self) {
        let _ = self.stop_worker.send(true);
        if let Some(handle) = self.worker.take() {
            let _ = tokio::time::timeout(Duration::from_secs(30), handle).await;
        }
        drop(self.lock.take());
        if let Some(mut db) = self.embedded.take() {
            if let Err(e) = db.shutdown(Duration::from_secs(30)).await {
                eprintln!("stopping the bundled database failed: {e}");
            }
        }
    }
}

/// Opens the database (starting the bundled server first if nothing answers), installs the
/// in-process backend the screens will use, and starts the worker if no one else is running
/// one. Returns what has to be stopped again on quit.
pub async fn prepare() -> Result<Boot, Box<dyn std::error::Error>> {
    let paths = AppPaths::detect();
    // Whatever first-run setup chose, unless an environment variable says otherwise
    // (`ROMEXPLORER_DB_PORT=3309` for a second instance, say).
    let mut stored = StoredConfig::load(&paths);
    let mut config = stored.database.clone();

    // Only ever start a database that is meant to be ours: a configured remote host is
    // someone else's to run.
    let local_db = matches!(config.host.as_str(), "127.0.0.1" | "localhost" | "::1");
    let mut embedded = None;
    // Adopted only if it is *this installation's* database; another installation's on the
    // same port is left alone and ours starts beside it (see `plan_local_database`).
    let datadir = stored.datadir(&paths);
    let plan = if local_db { romexplorer::database::plan_local_database(&config, &datadir).await } else { romexplorer::database::LocalDatabase::Adopt };
    if let romexplorer::database::LocalDatabase::Start { port } = plan {
        if port != config.port {
            eprintln!("port {} belongs to another installation's database; using {port} for this one", config.port);
            config.port = port;
            stored.database.port = port;
            if let Err(e) = stored.save(&paths) {
                eprintln!("could not save the database port to {}: {e}", paths.database_env_path().display());
            }
        }
        check_layout(&paths.mariadb_dir())?;
        let mut server_config = ServerConfig::new(paths.mariadb_dir(), &datadir, config.port).with_buffer_pool(stored.buffer_pool).with_fast_writes(stored.fast_writes()).with_disk(stored.disk);
        // Once setup has set one, readiness polling and graceful shutdown both need it.
        if let Some(root) = &stored.root_password {
            server_config = server_config.with_root_password(root);
        }
        let mut db = MariaDbServer::new(server_config);
        db.start().await?;
        eprintln!("started the bundled database on 127.0.0.1:{} (datadir {})", config.port, datadir.display());
        embedded = Some(db);
    }

    let app = App::open(paths.clone(), config.clone()).await?;
    let pool = app.pool.clone();
    romexplorer::database::warm::start(&pool, &stored, local_db);

    let (stop_worker, stop_rx) = tokio::sync::watch::channel(false);
    let lock = worker::take_worker_lock(&pool).await;
    let worker = match &lock {
        Some(_) => {
            let worker_app = App::with_pool(pool.clone(), paths.clone(), config.clone())?;
            let w = Worker::new(worker_app).await?;
            Some(tokio::spawn(worker::run_scheduler(w, WorkerOptions::from_env(), stop_rx)))
        }
        None => {
            eprintln!("another romexplorer already runs the worker for this database; not starting a second one");
            None
        }
    };

    crate::api::set_backend(Box::new(LocalBackend::new(app, worker.is_some())));
    Ok(Boot { embedded, stop_worker, worker, lock })
}
