//! `romexplorer-dbtool`: dev/ops CLI for the embedded database.
//!
//! ```text
//! romexplorer_dbtool serve [--port N]                 start the bundled mariadbd (bootstraps on first
//!                                                     run), print "ready", stop on Ctrl-C
//! romexplorer_dbtool shutdown [--port N]              SQL SHUTDOWN of a running instance
//! romexplorer_dbtool init [--port N]                  create db + migrate + seed platforms
//! romexplorer_dbtool import <source> <path> [--member <substr>]...
//!                                                     import a .dat/.zip/.json (pack members matching
//!                                                     any --member substring, all if none given)
//! romexplorer_dbtool make-dats <folder>... --out <folder>
//! romexplorer_dbtool fetch <source> --out <folder> [--limit N]
//!                                                     download a DAT source's files into
//!                                                     <folder>/<source>/ (no database needed)
//! romexplorer_dbtool link-patched [--port N]             # fold patched works, link them to their originals
//!                                                     collectors' DAT folders -> hacks.dat, homebrew.dat,
//!                                                     etc.dat, trainers.dat, saves.dat, source.dat
//!                                                     (`database::dat_convert`), for the manual sources
//! romexplorer_dbtool query "<select ...>" [--port N]  print one statement's rows, tab-separated
//! romexplorer_dbtool virtual-collection [--games N] [--platforms N] [--seed N] [--replace]
//!                                                     synthesize an owned collection from catalog data
//! romexplorer_dbtool counts                           row counts per table
//! romexplorer_dbtool check                            where this install found its app root,
//!                                                     native libraries and bundled MariaDB
//! ```
//!
//! Env: `ROMEXPLORER_APP_ROOT`, `ROMEXPLORER_DB_*` (see `database::config`).

use std::time::Duration;

use romexplorer::database::dat_download::Source;
use romexplorer::database::import::{self, ImportOptions};
use romexplorer::database::lookup;
use romexplorer::database::server::{check_layout, MariaDbServer, ServerConfig};
use romexplorer::database::virtual_collection::{self, VirtualCollectionOptions};
use romexplorer::database::{self, AppPaths, DatabaseConfig, DatabaseError};

fn usage() -> ! {
    eprintln!("usage: romexplorer_dbtool <serve|shutdown|init|import|fetch|make-dats|link-patched|query|virtual-collection|counts|check> [options]\n(see the module doc comment in src/bin/romexplorer_dbtool.rs)");
    std::process::exit(2);
}

struct Args {
    command: String,
    positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

impl Args {
    fn parse() -> Self {
        let mut args = std::env::args().skip(1);
        let command = args.next().unwrap_or_else(|| usage());
        let mut positional = Vec::new();
        let mut flags = Vec::new();
        let rest: Vec<String> = args.collect();
        let mut i = 0;
        while i < rest.len() {
            if let Some(name) = rest[i].strip_prefix("--") {
                let takes_value = !matches!(name, "replace" | "foreground");
                let value = if takes_value { rest.get(i + 1).cloned() } else { None };
                flags.push((name.to_string(), value));
                i += if takes_value { 2 } else { 1 };
            } else {
                positional.push(rest[i].clone());
                i += 1;
            }
        }
        Self { command, positional, flags }
    }

    fn flag(&self, name: &str) -> Option<&str> {
        self.flags.iter().rev().find(|(n, _)| n == name).and_then(|(_, v)| v.as_deref())
    }

    fn flag_values(&self, name: &str) -> Vec<String> {
        self.flags.iter().filter(|(n, _)| n == name).filter_map(|(_, v)| v.clone()).collect()
    }

    fn has(&self, name: &str) -> bool {
        self.flags.iter().any(|(n, _)| n == name)
    }
}

#[tokio::main]
async fn main() {
    env_logger::init();
    romexplorer::common::fd_limit::raise_and_log();
    let args = Args::parse();
    if let Err(err) = run(args).await {
        eprintln!("romexplorer_dbtool: {err}");
        std::process::exit(1);
    }
}

fn config_from(args: &Args) -> DatabaseConfig {
    let mut config = DatabaseConfig::from_env();
    if let Some(port) = args.flag("port").and_then(|p| p.parse().ok()) {
        config.port = port;
    }
    config
}

async fn run(args: Args) -> Result<(), DatabaseError> {
    let paths = AppPaths::detect();
    let config = config_from(&args);
    match args.command.as_str() {
        "serve" => {
            check_layout(&paths.mariadb_dir())?;
            let mut server = MariaDbServer::new(ServerConfig::new(paths.mariadb_dir(), paths.mariadb_data_dir(), config.port));
            server.start().await?;
            println!("ready: mariadbd on 127.0.0.1:{} (datadir {})", config.port, paths.mariadb_data_dir().display());
            tokio::signal::ctrl_c().await.map_err(DatabaseError::Io)?;
            println!("shutting down...");
            server.shutdown(Duration::from_secs(60)).await?;
            println!("stopped");
        }
        "shutdown" => {
            use sqlx::Connection;
            let mut conn = sqlx::mysql::MySqlConnection::connect_with(&config.connect_options(false)).await?;
            let _ = sqlx::query("SHUTDOWN").execute(&mut conn).await;
            println!("SHUTDOWN sent to 127.0.0.1:{}", config.port);
        }
        "init" => {
            database::initialize(&config).await?;
            println!("database `{}` ready on port {}", config.database, config.port);
        }
        "import" => {
            let (Some(source), Some(path)) = (args.positional.first(), args.positional.get(1)) else { usage() };
            let source = Source::from_name(source).ok_or_else(|| DatabaseError::Config(format!("unknown source {source:?}; one of: {}", Source::ALL.iter().map(|s| s.name()).collect::<Vec<_>>().join(", "))))?;
            let members: Vec<String> = args.flag_values("member").iter().map(|m| m.to_ascii_lowercase()).collect();
            let filter = move |name: &str| members.is_empty() || members.iter().any(|m| name.to_ascii_lowercase().contains(m.as_str()));
            let pool = database::initialize(&config).await?;
            let started = std::time::Instant::now();
            let summaries = import::import_path(&pool, source, std::path::Path::new(path), &filter, &ImportOptions::default()).await?;
            for (name, s) in &summaries {
                println!("{name}: platform {:?}; {} games, {} files -> {} releases, {} new content (+{} reused), {} hashes, {} new titles, {} superseded, {} restored", s.platform, s.games, s.files, s.releases_upserted, s.content_new, s.content_reused, s.hashes_inserted, s.titles_new, s.content_superseded, s.content_restored);
                if let Some(why) = &s.supersede_held {
                    println!("  nothing taken off: {why}");
                }
            }
            println!("{} dat(s) in {:.1}s", summaries.len(), started.elapsed().as_secs_f64());
        }
        "query" => {
            // One read-only look at the database, tab-separated: the bundled `mariadb`
            // client needs the server's own DLL path to start, which a dev shell lacks.
            use sqlx::{Column, Row};
            let Some(sql) = args.positional.first() else { usage() };
            let pool = sqlx::mysql::MySqlPoolOptions::new().max_connections(1).connect_with(config.connect_options(true)).await?;
            let rows = sqlx::query(sqlx::AssertSqlSafe(sql.clone())).fetch_all(&pool).await?;
            if let Some(first) = rows.first() {
                println!("{}", first.columns().iter().map(|c| c.name().to_string()).collect::<Vec<_>>().join("	"));
            }
            for row in &rows {
                let cells: Vec<String> = (0..row.columns().len())
                    .map(|i| {
                        row.try_get::<Option<String>, _>(i)
                            .map(|v| v.unwrap_or_else(|| "NULL".into()))
                            .or_else(|_| row.try_get::<Option<i64>, _>(i).map(|v| v.map_or("NULL".into(), |n| n.to_string())))
                            .or_else(|_| row.try_get::<Option<u64>, _>(i).map(|v| v.map_or("NULL".into(), |n| n.to_string())))
                            .or_else(|_| row.try_get::<Option<bool>, _>(i).map(|v| v.map_or("NULL".into(), |n| n.to_string())))
                            .unwrap_or_else(|_| "?".into())
                    })
                    .collect();
                println!("{}", cells.join("	"));
            }
        }
        "fetch" => {
            // What a DAT sync downloads for one source, on its own: to see that a source's
            // upstream is still where and what the downloader expects.
            use romexplorer::database::dat_download;
            let (Some(source), Some(out)) = (args.positional.first(), args.flag("out")) else { usage() };
            let source = Source::from_name(source).ok_or_else(|| DatabaseError::Config(format!("unknown source {source:?}; one of: {}", Source::ALL.iter().map(|s| s.name()).collect::<Vec<_>>().join(", "))))?;
            let options = dat_download::DownloadOptions { limit: args.flag("limit").and_then(|n| n.parse().ok()), ..Default::default() };
            let client = dat_download::build_client(&options)?;
            let started = std::time::Instant::now();
            let files = dat_download::fetch(&client, source, std::path::Path::new(out), &options).await?;
            for f in &files {
                println!("{} {:>9} {}", if f.fresh { "new " } else { "kept" }, f.bytes, f.path.file_name().and_then(|n| n.to_str()).unwrap_or_default());
            }
            println!("{}: {} file(s), {} downloaded, {} byte(s) in {:.1}s", source.name(), files.len(), files.iter().filter(|f| f.fresh).count(), files.iter().map(|f| f.bytes).sum::<u64>(), started.elapsed().as_secs_f64());
        }
        "link-patched" => {
            // What the title merge job does for patched releases, on its own: one release
            // per work, then what each is made from.
            let pool = database::initialize(&config).await?;
            let none = romexplorer::database::jobs::Progress::none();
            let started = std::time::Instant::now();
            let folded = romexplorer::database::release_merge::fold_patched_works(&pool, &none).await?;
            println!("folded: {} by name, {} by content ({:.1}s)", folded.by_name, folded.by_content, started.elapsed().as_secs_f64());
            let started = std::time::Instant::now();
            let linked = romexplorer::database::release_bases::resolve(&pool, &none).await?;
            println!(
                "{} release(s) given the kind their category states; {} title(s) unlinked from a hack's IGDB entry; linked: {} by hash ({} original file(s) set aside), {} exact, {} by IGDB, {} suggested, {} moved to their original's title ({:.1}s)",
                linked.kinds_restored,
                linked.titles_unlinked_from_hacks,
                linked.by_hash,
                linked.originals_set_aside,
                linked.exact,
                linked.igdb,
                linked.suggested,
                linked.moved,
                started.elapsed().as_secs_f64()
            );
        }
        "make-dats" => {
            // Collectors' DAT folders -> hacks.dat, homebrew.dat, ... (`database::dat_convert`).
            let Some(out) = args.flag("out") else { usage() };
            if args.positional.is_empty() {
                usage();
            }
            let inputs: Vec<std::path::PathBuf> = args.positional.iter().map(std::path::PathBuf::from).collect();
            let started = std::time::Instant::now();
            let summary = romexplorer::database::dat_convert::convert_folders(&inputs, std::path::Path::new(out))?;
            println!("{} dat(s) read in {:.1}s", summary.dats_read, started.elapsed().as_secs_f64());
            for (file, (entries, roms)) in &summary.written {
                println!("  {file}: {entries} entries, {roms} files");
            }
            println!("by kind: {}", summary.kinds.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(", "));
            println!("left out: {} machine(s) that are only notes, {} trainer or save entries that are a hack's own ROM", summary.dropped, summary.games_filed_as_extras);
            for (folder, n) in &summary.unknown_folders {
                println!("  unknown folder {folder:?}: {n} machine(s) left out");
            }
            for (system, n) in &summary.unknown_systems {
                println!("  no platform for {system:?}: {n} entries will import as unknown");
            }
        }
        "virtual-collection" => {
            let pool = database::initialize(&config).await?;
            let mut options = VirtualCollectionOptions::default();
            if let Some(n) = args.flag("games").and_then(|v| v.parse().ok()) {
                options.games = n;
            }
            if let Some(n) = args.flag("platforms").and_then(|v| v.parse().ok()) {
                options.platforms = n;
            }
            if let Some(n) = args.flag("seed").and_then(|v| v.parse().ok()) {
                options.seed = n;
            }
            if let Some(root) = args.flag("root") {
                options.root = root.to_string();
            }
            let summary = virtual_collection::generate(&pool, &options, args.has("replace")).await?;
            println!("virtual collection: {} games ({} multi-disc), {} roms, {} files; {} zipped singles, {} m3u playlists, {} duplicate copies", summary.games, summary.multi_disc_games, summary.roms, summary.files, summary.zipped, summary.playlists, summary.duplicates.len());
            println!("\n| platform | games | multi-disc | formats |\n|---|---|---|---|");
            for p in &summary.per_platform {
                let formats: Vec<String> = p.formats.iter().map(|(k, v)| format!("{k}:{v}")).collect();
                println!("| {:?} | {} | {} | {} |", p.platform, p.games, p.multi_disc, formats.join(" "));
            }
            for d in &summary.duplicates {
                println!("- duplicate: {d}");
            }
        }
        "export-counterparts" => {
            // Release -> USA / English counterpart tables (database::counterparts) as CSV.
            let out = std::path::PathBuf::from(args.flag("out").unwrap_or("counterparts"));
            std::fs::create_dir_all(&out)?;
            let pool = database::connect(&config).await?;
            let started = std::time::Instant::now();
            let rows = database::counterparts::build(&pool).await?;
            let stripped = database::counterparts::stripped(&rows);
            std::fs::write(out.join("counterparts_full.csv"), database::counterparts::to_csv(&rows))?;
            std::fs::write(out.join("counterparts_stripped.csv"), database::counterparts::to_csv(&stripped))?;
            let mut per: std::collections::BTreeMap<&str, (usize, usize, usize)> = std::collections::BTreeMap::new();
            for r in &rows {
                let e = per.entry(r.platform.as_str()).or_default();
                match &r.counterpart {
                    Some(c) if *c == r.release => e.1 += 1,
                    Some(_) => e.0 += 1,
                    None => e.2 += 1,
                }
            }
            println!("| platform | matched | Japan-only (itself) | empty |\n|---|---|---|---|");
            for (p, (m, j, e)) in &per {
                println!("| {p} | {m} | {j} | {e} |");
            }
            println!("{} rows ({} stripped) -> {} in {:.1}s", rows.len(), stripped.len(), out.display(), started.elapsed().as_secs_f64());
        }
        "import-icons" => {
            // Platform icons from the RomM checkout (read-only) into app_root_ex/assets/icons.
            let repo = std::path::PathBuf::from(args.flag("from").unwrap_or("submodules/romm"));
            let pool = database::connect(&config).await?;
            let creds = database::ApiCredentials::load(&paths.api_env())?;
            let clients = database::providers::ProviderClients::from_credentials(&creds, database::providers::build_http_client()?);
            let licenses = paths.root.join("lib").join("LICENSES");
            let s = database::icon_import::import_romm_icons(&pool, &clients, &repo, &paths.icons_dir(), &licenses, args.has("replace")).await?;
            println!("RomM commit {}: {} icon(s) copied, {} kept (already present; --replace to overwrite), {} platform(s) without an SVG", s.commit, s.copied.len(), s.kept_existing.len(), s.unmapped.len());
            for (name, src) in &s.copied {
                println!("  + {name} <- {src}");
            }
            if !s.unmapped.is_empty() {
                println!("no SVG: {}", s.unmapped.join(", "));
            }
        }
        // No database, no network: just "does this install have what it needs". The answer
        // used to be unobtainable without reading the source, which made a package that
        // had quietly lost its fast paths indistinguishable from a healthy one -- the
        // wrappers fall back silently and report themselves unavailable much later.
        "check" => {
            use romexplorer::rom::native_lib::{self, LibVariant};
            let paths = AppPaths::detect();
            println!("app root      {}", paths.root.display());
            println!("libraries     {}", paths.os_lib_dir().display());
            println!("data          {}", paths.mariadb_data_dir().parent().map(|p| p.display().to_string()).unwrap_or_default());
            println!("assets        {}", paths.media_dir().parent().map(|p| p.display().to_string()).unwrap_or_default());
            println!();

            let caps = native_lib::detect_capabilities(LibVariant::Streaming);
            let mut missing = Vec::new();
            for (name, lib) in [("7-Zip", &caps.sevenzip), ("chdman", &caps.chdman), ("dolphinrvz", &caps.dolphin)] {
                match &lib.path {
                    Some(p) => println!("{name:<13} {} {}", if lib.streaming { "streaming" } else { "found, NOT streaming" }, p.display()),
                    None => {
                        println!("{name:<13} MISSING");
                        missing.push(name);
                    }
                }
            }
            // These two have no streaming symbol to probe, so a path is the whole answer.
            let zlib_base = if cfg!(windows) { "zlib" } else { "z" };
            let rahash_base = if cfg!(windows) { "libRAHasher" } else { "RAHasher" };
            for (name, base) in [("zlib-ng", zlib_base), ("RAHasher", rahash_base)] {
                match native_lib::find_app_root_library(base) {
                    Some(p) => println!("{name:<13} found     {}", p.display()),
                    None => {
                        println!("{name:<13} MISSING");
                        missing.push(name);
                    }
                }
            }
            println!();
            // No mariadb/ at all is an install with the bundled database removed: it runs against
            // a database set in data/database.env and never starts one, so that is not a fault.
            // A mariadb/ that exists but is incomplete still is.
            if !paths.mariadb_dir().exists() {
                println!("MariaDB       not bundled (the database is external, set in data/database.env)");
            } else {
                match check_layout(&paths.mariadb_dir()) {
                    Ok(()) => println!("MariaDB       ok        {}", paths.mariadb_dir().display()),
                    Err(e) => {
                        println!("MariaDB       BROKEN    {e}");
                        missing.push("MariaDB");
                    }
                }
            }
            if missing.is_empty() {
                println!("\neverything this install needs is present.");
            } else {
                println!("\nmissing: {}", missing.join(", "));
                std::process::exit(1);
            }
        }
        "counts" => {
            let pool = database::connect(&config).await?;
            println!("| table | rows |\n|---|---|");
            for (table, n) in lookup::table_counts(&pool).await? {
                println!("| {table} | {n} |");
            }
            for table in ["local_directories", "local_files", "local_rom", "local_rom_files", "local_game", "local_game_roms", "local_release_games", "content_contents"] {
                let row = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) AS n FROM `{table}`"))).fetch_one(&pool).await?;
                let n: i64 = sqlx::Row::try_get(&row, "n")?;
                println!("| {table} | {n} |");
            }
        }
        _ => usage(),
    }
    Ok(())
}
