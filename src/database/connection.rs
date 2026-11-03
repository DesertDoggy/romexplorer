//! Connecting to MariaDB, creating the database, applying migrations, seeding reference rows.

use sqlx::mysql::{MySqlPool, MySqlPoolOptions};
use sqlx::Connection;

use crate::common::models::Platform;
use crate::rom::platforms::helpers;

use super::config::DatabaseConfig;
use super::error::DatabaseError;
use super::server::{MariaDbServer, UserPrivileges};

/// Opens a connection pool per `config`. Doesn't apply migrations -- call [`migrate`] once
/// after connecting (typically once at app startup), not on every connect, since a shared
/// fileserver instance may already be migrated by another client.
pub async fn connect(config: &DatabaseConfig) -> Result<MySqlPool, DatabaseError> {
    MySqlPoolOptions::new().max_connections(config.max_connections).connect_with(config.connect_options(true)).await.map_err(DatabaseError::from)
}

/// `CREATE DATABASE IF NOT EXISTS <config.database>` (utf8mb4), connecting without a
/// database selected.
pub async fn ensure_database(config: &DatabaseConfig) -> Result<(), DatabaseError> {
    let mut conn = sqlx::mysql::MySqlConnection::connect_with(&config.connect_options(false)).await?;
    let name = config.database.replace('`', "``");
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE IF NOT EXISTS `{name}` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci"))).execute(&mut conn).await?;
    conn.close().await?;
    Ok(())
}

/// Every migration under `migrations/`, embedded at compile time. Named rather than
/// inlined so [`probe`] can compare what a database has recorded against what this build
/// carries, without applying anything.
static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// The highest migration this build carries -- recorded in a backup so a future restore can
/// refuse one written by a version it cannot read.
pub fn schema_version() -> i64 {
    MIGRATOR.iter().map(|m| m.version).max().unwrap_or(0)
}

/// What this build can do with the database it found, decided **without writing to it**.
///
/// The reason this exists: a database from an older schema fails at `migrate` with a
/// checksum mismatch, deep inside sqlx, after the app has already committed to opening it.
/// First-run setup needs to say "that one is too old" *before* the user picks it.
#[derive(Debug, Clone, PartialEq)]
pub enum DatabaseState {
    /// Nothing there yet -- no database, or one with no migrations recorded. A fresh
    /// schema will be created.
    Empty,
    /// Opens as is. `pending` migrations will be applied on open.
    Ready { applied: usize, pending: usize },
    /// This build cannot open it, and no migration path leads from it to the current
    /// schema. `reason` is meant to be shown to the user.
    TooOld { reason: String },
}

/// Classifies an existing database. Read-only: it reads `_sqlx_migrations` and compares
/// against [`MIGRATOR`], rather than trying a migration and interpreting the wreckage.
///
/// Three things make a database unusable, and they are worth distinguishing because only
/// the first is recoverable by the user picking a different directory:
/// * a migration this build has, recorded with a **different checksum** -- the file was
///   edited after that database applied it (this is what happened to the pre-`local_*`
///   schema, whose migration 1 no longer matches);
/// * a migration recorded that this build **doesn't have** -- the database was written by
///   a newer version of the app;
/// * a failed migration left recorded as unsuccessful.
pub async fn probe(config: &DatabaseConfig) -> Result<DatabaseState, DatabaseError> {
    use sqlx::Row;

    let pool = match connect(config).await {
        Ok(pool) => pool,
        // No such database yet: there is nothing here to be too old.
        Err(_) => return Ok(DatabaseState::Empty),
    };

    let recorded = sqlx::query("SELECT version, checksum, success FROM _sqlx_migrations ORDER BY version")
        .fetch_all(&pool)
        .await;
    let rows = match recorded {
        Ok(rows) if rows.is_empty() => return Ok(DatabaseState::Empty),
        Ok(rows) => rows,
        // The table itself is absent: an empty database, or one this app never touched.
        Err(_) => return Ok(DatabaseState::Empty),
    };

    let ours: std::collections::HashMap<i64, &[u8]> = MIGRATOR.iter().map(|m| (m.version, m.checksum.as_ref())).collect();
    for row in &rows {
        let version: i64 = row.try_get("version")?;
        let checksum: Vec<u8> = row.try_get("checksum")?;
        let success: bool = row.try_get("success")?;
        if !success {
            return Ok(DatabaseState::TooOld { reason: format!("migration {version} is recorded as failed; this database was left half-migrated") });
        }
        match ours.get(&version) {
            None => {
                return Ok(DatabaseState::TooOld {
                    reason: format!("it has migration {version}, which this version of romexplorer doesn't know about -- it was made by a newer build"),
                })
            }
            Some(expected) if *expected != checksum.as_slice() => {
                return Ok(DatabaseState::TooOld {
                    reason: format!("migration {version} doesn't match the one this build carries, so there is no upgrade path from it"),
                })
            }
            Some(_) => {}
        }
    }

    let applied = rows.len();
    Ok(DatabaseState::Ready { applied, pending: MIGRATOR.iter().count().saturating_sub(applied) })
}

/// Runs every migration under `migrations/` (embedded at compile time via
/// [`sqlx::migrate!`]) that this pool's database hasn't already recorded as applied, then
/// the data migrations done in Rust (`release_merge`, `release_editions`).
pub async fn migrate(pool: &MySqlPool) -> Result<(), DatabaseError> {
    MIGRATOR.run(pool).await.map_err(DatabaseError::from)?;
    data_migrations(pool).await?;
    // Here and not in `data_migrations`: it is DDL (columns, triggers), so it needs the same
    // rights the migrations do, and it only has anything to do when they changed the schema.
    super::sync_rev::ensure(pool, &crate::api::sync::tracked_tables()).await
}

/// The data migrations done in Rust. Idempotent, and plain DML -- unlike [`MIGRATOR`], which
/// opens with `CREATE TABLE IF NOT EXISTS _sqlx_migrations` even when nothing is pending.
async fn data_migrations(pool: &MySqlPool) -> Result<(), DatabaseError> {
    super::release_merge::merge_duplicate_releases(pool).await?;
    super::release_editions::backfill(pool).await?;
    super::release_editions::backfill_rereleases(pool).await?;
    super::release_editions::backfill_variants(pool).await?;
    super::merge_audit::queue_demo_repair_once(pool).await?;
    super::merge_audit::queue_platform_repair_once(pool).await?;
    super::merge_audit::queue_lost_igdb_repair_once(pool).await?;
    super::release_merge::move_color_releases_once(pool).await?;
    // Cheap (roms still on no platform only), and it catches what any path linked.
    let placed = super::import::confirm_rom_platforms(pool).await?;
    if placed > 0 {
        log::info!("{placed} rom(s) took their release's platform");
    }
    Ok(())
}

/// Upserts one `platforms` row per `common::models::Platform` variant. The enum is the
/// source of truth (`platforms` is fixed reference data); rerunning after an app update
/// that added a variant just adds its row.
pub async fn seed_platforms(pool: &MySqlPool) -> Result<usize, DatabaseError> {
    let mut count = 0;
    let mut tx = pool.begin().await?;
    for platform in all_platforms() {
        let id = platform.igdb_id();
        // Real IGDB ids are the small ones; 2048 (PC-FX placeholder) and 30000+ are internal
        // sentinels with no IGDB equivalent.
        let igdb_id: Option<u16> = if id != 0 && id < 2048 { Some(id) } else { None };
        sqlx::query(
            "INSERT INTO platforms (id, igdb_id, name, family, is_disc_based) VALUES (?, ?, ?, ?, ?) \
             ON DUPLICATE KEY UPDATE igdb_id = VALUES(igdb_id), name = VALUES(name), family = VALUES(family), is_disc_based = VALUES(is_disc_based)",
        )
        .bind(id)
        .bind(igdb_id)
        .bind(platform.display_name())
        .bind(platform_family(platform))
        .bind(helpers::is_disc_console(platform))
        .execute(&mut *tx)
        .await?;
        count += 1;
    }
    tx.commit().await?;
    Ok(count)
}

/// Every `Platform` variant, via the enum's own id round-trip (the same trick
/// `tests/src/support/inputs.rs` uses).
pub fn all_platforms() -> Vec<Platform> {
    let mut seen = std::collections::HashSet::new();
    (0..=u16::MAX).filter_map(Platform::from_igdb_id).filter(|p| seen.insert(*p)).collect()
}

pub fn platform_family(platform: Platform) -> Option<&'static str> {
    if helpers::is_nintendo_platform(platform) {
        Some("nintendo")
    } else if helpers::is_sega_platform(platform) {
        Some("sega")
    } else if helpers::is_playstation_platform(platform) {
        Some("sony")
    } else if helpers::is_xbox_platform(platform) {
        Some("microsoft")
    } else if platform.is_arcade_family() {
        Some("arcade")
    } else {
        match platform {
            Platform::SegaGameGear => Some("sega"),
            Platform::SuperFamicom | Platform::PokemonMini => Some("nintendo"),
            Platform::PocketStation | Platform::PspPs1Classic | Platform::PlayStationVr => Some("sony"),
            Platform::Atari2600 | Platform::Atari5200 | Platform::Atari7800 | Platform::Atari8Bit | Platform::AtariJaguar | Platform::AtariLynx | Platform::AtariStSte => Some("atari"),
            Platform::Pc8801 | Platform::Pc98 | Platform::Pc6000Series | Platform::PcFx | Platform::TurboGrafx16 | Platform::TurboGrafxCd | Platform::SuperGrafx => Some("nec"),
            Platform::NeoGeoPocket | Platform::NeoGeoPocketColor | Platform::NeoGeoCd | Platform::NeoGeoAes | Platform::NeoGeoMvs => Some("snk"),
            Platform::Msx | Platform::Msx2 | Platform::Msx2Plus | Platform::MsxTurbo => Some("msx"),
            Platform::WonderSwan | Platform::WonderSwanColor => Some("bandai"),
            Platform::Fm7 | Platform::FmTowns => Some("fujitsu"),
            Platform::SharpX68000 => Some("sharp"),
            Platform::PcWindows | Platform::PcDos | Platform::Linux | Platform::Mac => Some("computer"),
            Platform::Commodore64 | Platform::Amiga | Platform::AmstradCpc | Platform::ZxSpectrum | Platform::BbcMicro | Platform::Vic20 | Platform::AppleII | Platform::CommodorePet | Platform::Commodore16 | Platform::AppleIIgs | Platform::AcornArchimedes | Platform::Trs80 | Platform::Ti99 | Platform::AcornElectron | Platform::Trs80CoCo | Platform::Dragon32 | Platform::Zx81 | Platform::Oric | Platform::ThomsonMoTo | Platform::SamCoupe | Platform::Enterprise => Some("computer"),
            _ => None,
        }
    }
}

/// Which kind of machine a platform is, for picking platforms by group in a checkbox list:
/// `console` (mainstream home consoles: Nintendo, Sony, Microsoft, Neo Geo CD and NEC's
/// PC Engine / SuperGrafx / CD), `handheld` (Nintendo, Sony, Neo Geo Pocket / Color and
/// WonderSwan / Color), `arcade` (arcade boards, Neo Geo AES/MVS included), `retro_pc`
/// (PC-98, FM Towns, DOS/V, MSX, C64, ...), `modern_pc` (Windows, Mac, Linux) or `other`
/// (everything else: Sega, Atari, Game Gear, ...).
pub fn platform_group(platform: Platform) -> &'static str {
    match platform {
        Platform::Arcade | Platform::NeoGeoMvs | Platform::NeoGeoAes => "arcade",
        Platform::GameBoy
        | Platform::GameBoyColor
        | Platform::GameBoyAdvance
        | Platform::VirtualBoy
        | Platform::NintendoDs
        | Platform::NintendoDsi
        | Platform::DsiWare
        | Platform::Nintendo3ds
        | Platform::NewNintendo3ds
        | Platform::PokemonMini
        | Platform::PlayStationPortable
        | Platform::PspPs1Classic
        | Platform::PlayStationVita
        | Platform::PocketStation
        | Platform::NeoGeoPocket
        | Platform::NeoGeoPocketColor
        | Platform::WonderSwan
        | Platform::WonderSwanColor => "handheld",
        Platform::PcWindows | Platform::Mac | Platform::Linux => "modern_pc",
        Platform::Pc8801 | Platform::Pc98 | Platform::Pc6000Series | Platform::CommodoreCdtv => "retro_pc",
        Platform::NeoGeoCd | Platform::TurboGrafx16 | Platform::TurboGrafxCd | Platform::SuperGrafx => "console",
        _ => match platform_family(platform) {
            Some("computer" | "msx" | "fujitsu" | "sharp") => "retro_pc",
            Some("nintendo" | "sony" | "microsoft") => "console",
            // Sega, Atari, Coleco, 3DO, CD-i, PC-FX, Game Gear, Lynx, ...: no group button of
            // their own (the maker buttons still reach them).
            _ => "other",
        },
    }
}

/// Bring-up for a local install: create the database, migrate, seed. Idempotent.
///
/// An up-to-date database skips the two DDL steps. That is not only an optimisation: the
/// least-privilege app account the installers can create (`app_user` in setup.env) holds
/// SELECT/INSERT/UPDATE/DELETE on the one database and nothing else, and MariaDB checks
/// privileges *before* existence -- so `CREATE DATABASE IF NOT EXISTS` on a database that
/// exists still fails for it with 1044, as does sqlx's `CREATE TABLE IF NOT EXISTS
/// _sqlx_migrations`. Every start after first-run setup connects as that account; found by
/// running a packaged Windows server through setup. A schema *change* still needs an
/// account with DDL rights, and says so by failing here.
pub async fn initialize(config: &DatabaseConfig) -> Result<MySqlPool, DatabaseError> {
    if let DatabaseState::Ready { pending: 0, .. } = probe(config).await? {
        let pool = connect(config).await?;
        data_migrations(&pool).await?;
        seed_platforms(&pool).await?;
        return Ok(pool);
    }
    ensure_database(config).await?;
    let pool = connect(config).await?;
    migrate(&pool).await?;
    seed_platforms(&pool).await?;
    Ok(pool)
}

/// [`initialize`] for an install whose day-to-day account cannot change the schema.
///
/// The installers create a least-privilege app account by default, and every start connects
/// as it. That is right until an upgrade carries a migration: the app account is refused
/// (1044), the server exits, and the service manager restarts it into the same refusal --
/// which is what the first upgrade of a real install did, twice. The install already holds
/// the answer: setup stored the database root password beside the connection settings
/// (`db_root_password` in `data/database.env`). So when migrations are pending and that
/// password is there, they are applied over a short-lived root connection, and the server
/// then carries on as the account it was given. Root is used for the migration and nothing
/// else.
///
/// Without a stored root password this is [`initialize`] unchanged, including its failure
/// for an account that cannot migrate.
pub async fn initialize_with_root(config: &DatabaseConfig, root_password: Option<&str>) -> Result<MySqlPool, DatabaseError> {
    if let (Some(password), true) = (root_password, config.user != "root") {
        if let DatabaseState::Ready { pending, .. } = probe(config).await? {
            if pending > 0 {
                let root = DatabaseConfig { user: "root".to_string(), password: password.to_string(), ..config.clone() };
                log::info!("{pending} schema migration(s) pending: applying them as the database root account");
                let pool = connect(&root).await?;
                let migrated = migrate(&pool).await;
                // The sync's settled-change check reads `INNODB_TRX`, which needs PROCESS.
                // Granted here because an install made before it was needed is upgraded here.
                if migrated.is_ok() {
                    if let Err(e) = super::server::grant_process(&pool, &config.user).await {
                        log::warn!("could not grant PROCESS to {}: {e}; catalog sync clients will re-fetch some rows each time", config.user);
                    }
                }
                pool.close().await;
                migrated?;
            }
        }
    }
    initialize(config).await
}

/// Credentials for [`initial_setup`]. A struct rather than positional args: two passwords
/// side by side in a call is an easy transposition mistake to make silently.
pub struct InitialSetup<'a> {
    pub database: &'a str,
    pub root_password: &'a str,
    pub admin_user: &'a str,
    pub admin_password: &'a str,
    /// A separate, least-privilege day-to-day account. `None`: ordinary use connects as
    /// the admin account instead.
    pub app_user: Option<(&'a str, &'a str)>,
}

/// One-time bring-up for a fresh install, run once right after a server is bootstrapped:
/// starts `server` (bootstrapping first if needed), creates the database/schema/seed data
/// as the initial passwordless root ([`initialize`]), then locks root down with a password
/// and creates an admin account, plus an optional separate, least-privilege app account.
///
/// Returns the [`DatabaseConfig`] ordinary use should connect with -- the app account if
/// one was created, otherwise the admin account; root itself is never returned for
/// day-to-day use once an admin exists. Nothing here logs, prints, or otherwise persists a
/// password beyond that returned config, matching `env_creds`'s own rule.
pub async fn initial_setup(server: &mut MariaDbServer, setup: InitialSetup<'_>) -> Result<DatabaseConfig, DatabaseError> {
    server.start().await?;
    let port = server.config().port;

    let root_config = DatabaseConfig::default().with_port(port).with_database(setup.database);
    initialize(&root_config).await?;

    server.set_root_password(setup.root_password).await?;
    let root_config = root_config.with_password(setup.root_password);

    server.create_user(&root_config, setup.admin_user, setup.admin_password, setup.database, UserPrivileges::Admin).await?;
    let admin_config = DatabaseConfig::default().with_port(port).with_database(setup.database).with_user(setup.admin_user).with_password(setup.admin_password);

    if let Some((app_user, app_password)) = setup.app_user {
        server.create_user(&admin_config, app_user, app_password, setup.database, UserPrivileges::App).await?;
        return Ok(DatabaseConfig::default().with_port(port).with_database(setup.database).with_user(app_user).with_password(app_password));
    }

    Ok(admin_config)
}
