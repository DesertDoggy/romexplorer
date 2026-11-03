//! `romexplorer_server`: the REST server + built-in browser console (docs/plan/api.md).
//!
//! ```text
//! romexplorer_server [--port N] [--bind ADDR] [--share-port N] [--no-db-server] [--no-worker]
//!                    [--init-from-config PATH] [--allow-public-ips]
//!                    [--tls-cert PEM --tls-key PEM | --no-tls]
//! ```
//!
//! Starts the bundled `mariadbd` if nothing answers on the configured database port (skip
//! with `--no-db-server`), creates/migrates the database, makes the setup token
//! (`data/server_token`, never printed) if no admin account can sign in yet, then serves `/`
//! (console) and `/api/v1/...`.
//! `--init-from-config` runs the one-time `initial_setup` from a `KEY=VALUE` file first
//! (see `api::setup::SetupFile`) and continues with the account it returns.
//!
//! Runs the background worker (`romexplorer::worker`) in-process unless `--no-worker`: its
//! own tasks, independent of HTTP traffic, so queued jobs run with no browser open. The
//! process stops only deliberately -- Ctrl+C, or the console's shutdown/restart (`POST
//! /api/v1/server/{shutdown,restart}`) -- and then in order: listener, worker (running jobs
//! re-queued), embedded database; a restart then starts a fresh copy of this process with
//! the same arguments.
//!
//! Subcommands, each run instead of the server:
//!
//! ```text
//! romexplorer_server encrypt-credentials         seal every real value in api.env with the master key
//! romexplorer_server reset-admin [--username N]  set an admin's password (asks for the MariaDB
//!                                                root password first)
//! ```
//!
//! On start, `data/auth.env` (first accounts, written by an installer) is applied if no admin
//! can sign in yet, and deleted either way. The `--init-from-config` file is deleted once setup
//! has succeeded: both hold passwords, and neither is needed again.
//!
//! Env: `ROMEXPLORER_APP_ROOT`, `ROMEXPLORER_DB_*`, `ROMEXPLORER_SERVER_PORT`,
//! `ROMEXPLORER_SERVER_BIND`, `ROMEXPLORER_MASTER_KEY_FILE`, `ROMEXPLORER_TLS_CERT`,
//! `ROMEXPLORER_TLS_KEY`.
//!
//! Both listeners speak TLS with a self-signed certificate made on first start
//! (`data/tls/`), or the given one. Plain HTTP is still served to this machine itself; from
//! anywhere else it is redirected to https. `--no-tls` serves plain HTTP to everyone -- only
//! for a server behind a proxy that does TLS itself.

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use romexplorer::api::{self, App};
use romexplorer::database::server::{check_layout, MariaDbServer, ServerConfig};
use romexplorer::database::{AppPaths, DatabaseConfig};
use romexplorer::server::{self, Lifecycle, LifecycleRequest, ServeOptions, ServerState};
use romexplorer::worker::{self, Worker, WorkerOptions};

fn usage() -> ! {
    eprintln!("usage: romexplorer_server [--port N] [--bind ADDR] [--share-port N] [--no-db-server] [--no-worker] [--init-from-config PATH] [--allow-public-ips] [--tls-cert PEM --tls-key PEM | --no-tls]");
    eprintln!("       romexplorer_server encrypt-credentials");
    eprintln!("       romexplorer_server reset-admin [--username NAME]");
    std::process::exit(2);
}

struct Args {
    port: Option<u16>,
    bind: Option<String>,
    /// Also bind the minimal, read-only share router on this port (`server::share_router`).
    share_port: Option<u16>,
    no_db_server: bool,
    no_worker: bool,
    init_from_config: Option<PathBuf>,
    allow_public_ips: bool,
    tls_cert: Option<PathBuf>,
    tls_key: Option<PathBuf>,
    no_tls: bool,
}

impl Args {
    fn parse() -> Self {
        let mut out = Self { port: None, bind: None, share_port: None, no_db_server: false, no_worker: false, init_from_config: None, allow_public_ips: false, tls_cert: None, tls_key: None, no_tls: false };
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--port" => out.port = Some(args.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| usage())),
                "--bind" => out.bind = Some(args.next().unwrap_or_else(|| usage())),
                "--share-port" => out.share_port = Some(args.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| usage())),
                "--no-db-server" => out.no_db_server = true,
                "--no-worker" => out.no_worker = true,
                "--init-from-config" => out.init_from_config = Some(PathBuf::from(args.next().unwrap_or_else(|| usage()))),
                "--allow-public-ips" => out.allow_public_ips = true,
                "--tls-cert" => out.tls_cert = Some(PathBuf::from(args.next().unwrap_or_else(|| usage()))),
                "--tls-key" => out.tls_key = Some(PathBuf::from(args.next().unwrap_or_else(|| usage()))),
                "--no-tls" => out.no_tls = true,
                "-h" | "--help" => usage(),
                _ => usage(),
            }
        }
        out
    }
}

#[tokio::main]
async fn main() {
    // Info by default: a long-running server/worker should say what its jobs are doing;
    // RUST_LOG still overrides.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,sqlx=warn")).init();
    romexplorer::common::fd_limit::raise_and_log();
    let result = match std::env::args().nth(1).as_deref() {
        Some("encrypt-credentials") => encrypt_credentials(),
        Some("reset-admin") => reset_admin().await,
        _ => run().await,
    };
    if let Err(err) = result {
        eprintln!("romexplorer_server: {err}");
        std::process::exit(1);
    }
    // Not a plain return: dropping the runtime waits for every blocking thread, and a job
    // that was aborted mid-scan still has one hashing its file -- minutes for a disc image,
    // with the listener, the worker and the database already gone. Everything that needed
    // an orderly stop has had it by here.
    std::process::exit(0);
}

/// Seals `api.env` in place. Needs no database: only the file and the master key, which is
/// created here if this is the first thing ever run on the machine.
fn encrypt_credentials() -> Result<(), Box<dyn std::error::Error>> {
    let paths = AppPaths::detect();
    let file = paths.api_env();
    if !file.is_file() {
        return Err(format!("no credentials file at {}", file.display()).into());
    }
    let vault = romexplorer::database::Vault::load_or_create(&paths.master_key_path())?;
    let sealed = romexplorer::database::env_creds::seal_file(&file, &vault)?;
    if sealed == 0 {
        eprintln!("{}: nothing to seal (every real value already is)", file.display());
    } else {
        eprintln!("{}: sealed {sealed} value(s) with {}", file.display(), paths.master_key_path().display());
        eprintln!("keep that key file: without it these values can't be read, and must be entered again");
    }
    Ok(())
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let paths = AppPaths::detect();
    let stored = romexplorer::database::config::StoredConfig::load(&paths);
    let mut config = stored.database.clone();
    let mut port = args.port.or_else(|| std::env::var("ROMEXPLORER_SERVER_PORT").ok().and_then(|v| v.parse().ok())).or(stored.server_port).unwrap_or(server::DEFAULT_PORT);
    let bind: IpAddr = args.bind.or_else(|| std::env::var("ROMEXPLORER_SERVER_BIND").ok()).unwrap_or_else(|| "127.0.0.1".to_string()).parse()?;

    let mut embedded: Option<MariaDbServer> = None;
    let local_db = matches!(config.host.as_str(), "127.0.0.1" | "localhost" | "::1");

    if let Some(path) = &args.init_from_config {
        let file = api::setup::SetupFile::load(path)?;
        if let Some(p) = file.db_port {
            config.port = p;
        }
        if let Some(p) = file.server_port {
            port = p;
        }
        check_layout(&paths.mariadb_dir())?;
        let mut db = MariaDbServer::new(ServerConfig::new(paths.mariadb_dir(), stored.datadir(&paths), config.port).with_buffer_pool(stored.buffer_pool).with_fast_writes(stored.fast_writes()).with_disk(stored.disk));
        config = api::setup::run(&mut db, &file).await?;
        // Persist it, or this was setup for one process only: the next start would read the
        // defaults again -- passwordless root on 3307 -- and no longer be able to connect.
        let mut done = stored.clone();
        done.database = config.clone();
        done.root_password = Some(file.root_password.clone());
        done.server_port = Some(port);
        done.save(&paths)?;
        embedded = Some(db);
        eprintln!("initial setup done: database {} on port {}, day-to-day user {}", config.database, config.port, config.user);
        eprintln!("connection settings written to {} (owner-only)", paths.database_env_path().display());
        // It holds the MariaDB root and admin passwords in plaintext, and everything it
        // decided now lives in database.env: leaving it would be leaving the keys out.
        match std::fs::remove_file(path) {
            Ok(()) => eprintln!("removed {} (its settings are saved)", path.display()),
            Err(e) => eprintln!("could not remove {}: {e} -- delete it by hand, it holds passwords", path.display()),
        }
    } else if let Some(romexplorer::database::LocalDatabase::Start { port }) =
        if !args.no_db_server && local_db { Some(romexplorer::database::plan_local_database(&config, &stored.datadir(&paths)).await) } else { None }
    {
        // Another installation's database on our port is not ours to use: start beside it,
        // and remember where, so this installation's desktop app finds the same one.
        if port != config.port {
            eprintln!("port {} belongs to another installation's database; using {port} for this one", config.port);
            config.port = port;
            let mut moved = stored.clone();
            moved.database.port = port;
            if let Err(e) = moved.save(&paths) {
                eprintln!("could not save the database port to {}: {e}", paths.database_env_path().display());
            }
        }
        check_layout(&paths.mariadb_dir())?;
        let mut server_config = ServerConfig::new(paths.mariadb_dir(), stored.datadir(&paths), config.port).with_buffer_pool(stored.buffer_pool).with_fast_writes(stored.fast_writes()).with_disk(stored.disk);
        if let Some(root) = &stored.root_password {
            server_config = server_config.with_root_password(root);
        }
        let mut db = MariaDbServer::new(server_config);
        db.start().await?;
        eprintln!("database cache {} ({} MB)", stored.buffer_pool.to_text(), stored.buffer_pool.bytes() >> 20);
        eprintln!("started embedded mariadbd on 127.0.0.1:{} (datadir {})", config.port, paths.mariadb_data_dir().display());
        embedded = Some(db);
    }

    // The embedded database is watched from here on: if mariadbd dies under the running
    // server (it has, in the middle of a title merge pass), every request after it failed --
    // signing in, the tabs, even the UI language, which fell back to English -- until the
    // whole service was restarted by hand. Now it is started again within a few seconds,
    // and its error log's last lines say why it stopped.
    let embedded = embedded.map(|db| Arc::new(tokio::sync::Mutex::new(db)));
    let db_watch = embedded.as_ref().map(|db| {
        let db = Arc::clone(db);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let mut db = db.lock().await;
                if db.is_running() {
                    continue;
                }
                eprintln!("the embedded database stopped unexpectedly; its error log ({}) ends with:\n{}", db.error_log_path().display(), db.error_log_tail(30));
                match db.restart_after_exit().await {
                    Ok(()) => eprintln!("embedded database started again"),
                    Err(e) => eprintln!("could not start the embedded database again (trying again shortly): {e}"),
                }
            }
        })
    });

    let app = App::open(paths.clone(), config.clone()).await?;
    romexplorer::database::warm::start(&app.pool, &stored, local_db);
    // A server is standalone: it is what client machines take their catalogue from. One
    // that was never asked is recorded as such (so nothing prompts for it later); one that a
    // desktop app put in client mode is refused, because serving a catalogue this machine is
    // forbidden to build -- DAT imports and provider syncs are refused in client mode --
    // would be a server that cannot do its job.
    {
        use romexplorer::database::settings::{self as db_settings, InstanceMode};
        let stored = db_settings::get::<String>(&app.pool, db_settings::KEY_INSTANCE_MODE).await?;
        match stored.as_deref().and_then(InstanceMode::parse) {
            Some(InstanceMode::Client) => {
                return Err("this installation is in client mode (it takes its catalogue from a main server), and a server only runs standalone. Switch it to standalone in the desktop app under Config > Mode, then start the server again.".into());
            }
            Some(InstanceMode::Standalone) => {}
            None => db_settings::set(&app.pool, db_settings::KEY_INSTANCE_MODE, &InstanceMode::Standalone.name()).await?,
        }
    }
    match api::auth::seed_from_file(&app).await? {
        api::auth::SeedOutcome::Applied { users } => eprintln!("created the admin and {users} other account(s) from {}, and removed the file", paths.auth_env_path().display()),
        api::auth::SeedOutcome::Ignored => eprintln!("ignored and removed {}: an admin account already exists", paths.auth_env_path().display()),
        api::auth::SeedOutcome::NoFile => {}
    }
    let setup_token = api::setup::setup_token(&app).await?;
    match &setup_token {
        Some(_) => eprintln!("no admin account yet: sign in with the setup token in {} to create one", paths.server_token_path().display()),
        None => eprintln!("sign in with an account (the setup token is retired once an admin can sign in)"),
    }

    // The worker: its own App on the same pool, its own tasks.
    let (stop_worker, stop_rx) = tokio::sync::watch::channel(false);
    // Underscore-prefixed, not `_`: the binding must stay in scope for the whole run,
    // because dropping the connection is what releases the lock. Never read, only held.
    let mut _worker_lock = None;
    let worker_task = if args.no_worker {
        None
    } else {
        // One worker per database, whoever gets there first. The desktop GUI now hosts one
        // too, so this is no longer hypothetical: two would each take a full provider
        // rate-limit budget and between them break the provider's limit. Held by the
        // connection, so it frees itself if this process dies.
        match worker::take_worker_lock(&app.pool).await {
            Some(conn) => {
                _worker_lock = Some(conn);
                let worker_app = App::with_pool(app.pool.clone(), paths.clone(), config.clone())?;
                let w = Worker::new(worker_app).await?;
                eprintln!("background worker running (jobs run here; --no-worker to use a separate romexplorer_worker)");
                Some(tokio::spawn(worker::run_scheduler(w, WorkerOptions::from_env(), stop_rx)))
            }
            None => {
                eprintln!("another romexplorer already runs the worker for this database; not starting a second one");
                None
            }
        }
    };

    let tls = if args.no_tls {
        eprintln!("--no-tls: serving plain HTTP to every client; sign-ins and tokens cross the network unencrypted");
        None
    } else {
        let cert = args.tls_cert.clone().or_else(|| std::env::var_os("ROMEXPLORER_TLS_CERT").map(PathBuf::from));
        let key = args.tls_key.clone().or_else(|| std::env::var_os("ROMEXPLORER_TLS_KEY").map(PathBuf::from));
        let custom = match (cert, key) {
            (Some(c), Some(k)) => Some((c, k)),
            (None, None) => None,
            _ => return Err("--tls-cert and --tls-key go together".into()),
        };
        let tls = server::tls::load_or_create(&paths.tls_dir(), custom)?;
        eprintln!("TLS certificate fingerprint (SHA-256): {}", romexplorer_types::pinning::display(&tls.fingerprint));
        Some(tls)
    };
    let scheme = if tls.is_some() { "https" } else { "http" };

    let lifecycle = Lifecycle::new(worker_task.is_some());
    let state = Arc::new(ServerState {
        app,
        setup_token,
        in_process: false,
        allow_public_ips: args.allow_public_ips,
        fetch_loopback: false,
        lifecycle: lifecycle.clone(),
        tls_fingerprint: tls.as_ref().map(|t| t.fingerprint.clone()),
    });
    let options = ServeOptions { bind, port, tls: tls.clone() };
    // The share listener, when asked for: four read-only routes on a port of its own, so a
    // peer can mirror this machine without being handed the control surface.
    let share_task = args.share_port.map(|share_port| {
        let state = Arc::clone(&state);
        let lifecycle = lifecycle.clone();
        let share = ServeOptions { bind, port: share_port, tls: tls.clone() };
        tokio::spawn(async move {
            let stop = async move {
                let _ = lifecycle.requested().await;
            };
            if let Err(e) = server::serve_share(state, &share, |addr| eprintln!("sharing (read-only) on {scheme}://{addr}/"), stop).await {
                eprintln!("share listener on port {share_port} stopped: {e}");
            }
        })
    });
    let requested = Arc::new(std::sync::Mutex::new(LifecycleRequest::Shutdown));
    let shutdown = {
        let (lifecycle, requested) = (lifecycle.clone(), Arc::clone(&requested));
        async move {
            let what = tokio::select! {
                _ = romexplorer::lifecycle::stop_requested() => LifecycleRequest::Shutdown,
                what = lifecycle.requested() => what,
            };
            *requested.lock().unwrap_or_else(|p| p.into_inner()) = what;
            eprintln!("{} requested: stopping", if what == LifecycleRequest::Restart { "restart" } else { "shutdown" });
        }
    };
    server::serve(state, &options, |addr| eprintln!("ready: {scheme}://{addr}/  (database {}@{}:{}/{})", config.user, config.host, config.port, config.database), shutdown).await?;
    let what = *requested.lock().unwrap_or_else(|p| p.into_inner());

    if let Some(task) = share_task {
        task.abort();
    }
    // Each step says so: a stop that hangs otherwise gives no clue where.
    if let Some(mut task) = worker_task {
        eprintln!("stopping: worker");
        let _ = stop_worker.send(true);
        if tokio::time::timeout(Duration::from_secs(10), &mut task).await.is_err() {
            task.abort();
            eprintln!("worker didn't stop within 10s; its jobs are re-queued on the next start");
        }
    }
    if let Some(task) = db_watch {
        task.abort();
    }
    if let Some(db) = embedded {
        eprintln!("stopping: database");
        db.lock().await.shutdown(Duration::from_secs(20)).await?;
    }
    eprintln!("stopped");
    if what == LifecycleRequest::Restart {
        // Under a service manager, starting a child of a process that is about to exit is
        // the wrong shape: systemd kills it with the cgroup, launchd never adopts it as the
        // job, and if it does survive there are briefly two servers wanting the same port.
        // Exit and ask to be started again instead.
        if romexplorer::lifecycle::service_managed() {
            eprintln!("restart requested; exiting {} for the service manager to start us again", romexplorer::lifecycle::RESTART_EXIT_CODE);
            std::process::exit(romexplorer::lifecycle::RESTART_EXIT_CODE);
        }
        let exe = std::env::current_exe()?;
        let child = std::process::Command::new(&exe).args(restart_args(std::env::args_os().skip(1))).spawn()?;
        eprintln!("restarted as pid {}", child.id());
    }
    Ok(())
}

/// The arguments to restart with: the same ones, minus `--init-from-config <path>`. Setup ran
/// once and deleted its file; asking the restarted copy to run it again would fail on the
/// missing file, or worse, re-run setup against a database already in use.
fn restart_args(args: impl Iterator<Item = std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    let mut out = Vec::new();
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        if arg == "--init-from-config" {
            args.next();
            continue;
        }
        out.push(arg);
    }
    out
}

/// `reset-admin [--username NAME]`: the way back in when no admin can sign in.
///
/// Gated on the MariaDB root password, checked by actually connecting as root. That adds no
/// new secret: whoever has it can edit the accounts table directly anyway, and it is kept in
/// `data/database.env` on this machine, so the real requirement is "can read this machine's
/// data directory" -- which is what a reset should require. No sudo, and the same on Windows.
async fn reset_admin() -> Result<(), Box<dyn std::error::Error>> {
    let mut username: Option<String> = None;
    let mut args = std::env::args().skip(2);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--username" => username = Some(args.next().ok_or("--username needs a name")?),
            other => return Err(format!("reset-admin: unknown argument {other:?} (usage: reset-admin [--username NAME])").into()),
        }
    }
    let paths = AppPaths::detect();
    let stored = romexplorer::database::config::StoredConfig::load(&paths);
    let config = stored.database.clone();

    // Start the embedded database if this machine's isn't running (the server may be stopped).
    let local_db = matches!(config.host.as_str(), "127.0.0.1" | "localhost" | "::1");
    let mut embedded: Option<MariaDbServer> = None;
    if local_db && !romexplorer::database::database_answers(&config).await {
        check_layout(&paths.mariadb_dir())?;
        let mut server_config = ServerConfig::new(paths.mariadb_dir(), stored.datadir(&paths), config.port);
        if let Some(root) = &stored.root_password {
            server_config = server_config.with_root_password(root);
        }
        let mut db = MariaDbServer::new(server_config);
        db.start().await?;
        embedded = Some(db);
    }
    let result = reset_admin_with(&paths, &config, username).await;
    if let Some(mut db) = embedded {
        db.shutdown(Duration::from_secs(20)).await?;
    }
    result
}

async fn reset_admin_with(paths: &AppPaths, config: &DatabaseConfig, username: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let root_password = rpassword::prompt_password("MariaDB root password: ")?;
    let root = DatabaseConfig::default().with_host(config.host.clone()).with_port(config.port).with_password(root_password);
    use sqlx::Connection;
    let options = root.connect_options(false);
    let connect = sqlx::mysql::MySqlConnection::connect_with(&options);
    match tokio::time::timeout(Duration::from_secs(5), connect).await {
        Ok(Ok(conn)) => {
            conn.close().await.ok();
        }
        Ok(Err(_)) => return Err("that is not the MariaDB root password".into()),
        Err(_) => return Err(format!("the database at {}:{} did not answer", config.host, config.port).into()),
    }

    let app = App::open(paths.clone(), config.clone()).await?;
    let name = match username {
        Some(n) => n,
        None => {
            let existing: Option<String> = sqlx::query_scalar("SELECT username FROM users WHERE role = 'admin' AND deleted = FALSE ORDER BY id LIMIT 1").fetch_optional(&app.pool).await?;
            let default = existing.unwrap_or_else(|| "admin".to_string());
            eprint!("admin username [{default}]: ");
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            let line = line.trim();
            if line.is_empty() { default } else { line.to_string() }
        }
    };
    let password = rpassword::prompt_password(format!("new password for {name}: "))?;
    if rpassword::prompt_password("again: ")? != password {
        return Err("the two passwords differ; nothing changed".into());
    }
    let user = api::auth::reset_admin(&app, &name, &password).await?;
    eprintln!("{} is an admin and can sign in; every device it was signed in on has been signed out", user.username);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_restart_does_not_run_setup_again() {
        let args = ["--port", "7000", "--init-from-config", "/tmp/setup.env", "--bind", "0.0.0.0"].map(std::ffi::OsString::from);
        let kept: Vec<String> = restart_args(args.into_iter()).into_iter().map(|a| a.into_string().unwrap()).collect();
        assert_eq!(kept, ["--port", "7000", "--bind", "0.0.0.0"]);
    }
}
