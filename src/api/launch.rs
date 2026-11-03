//! The emulator launcher: configured emulators (`database::emulators`) and "Open with ..."
//! for a rom (its main file) or a game (its `.m3u` playlist when it has one, else its first
//! disc). The emulator starts on the machine running the app; a REST client on another
//! machine may only launch when `allow_remote_launch` is on (the server checks the
//! caller's address).

use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::database::emulators::{self, Emulator, Preset};
use crate::database::settings as db_settings;

use super::{App, Error, Result};

pub async fn list(app: &App) -> Result<Vec<Emulator>> {
    Ok(emulators::list(&app.pool).await?)
}

pub fn presets() -> Vec<Preset> {
    emulators::presets()
}

pub async fn save(app: &App, e: &Emulator) -> Result<Emulator> {
    if e.name.trim().is_empty() || e.path.trim().is_empty() {
        return Err(Error::invalid("an emulator needs a name and a path"));
    }
    if !matches!(e.kind.as_str(), "standalone" | "retroarch" | "command") {
        return Err(Error::invalid("kind must be standalone, retroarch or command"));
    }
    if !e.args.contains("{rom}") {
        return Err(Error::invalid("the arguments must contain {rom} (where the file goes)"));
    }
    let id = emulators::save(&app.pool, e).await?;
    emulators::get(&app.pool, id).await?.ok_or_else(|| Error::internal("saved emulator vanished"))
}

pub async fn delete(app: &App, id: u64) -> Result<()> {
    if !emulators::delete(&app.pool, id).await? {
        return Err(Error::not_found(format!("no emulator {id}")));
    }
    Ok(())
}

pub async fn remote_launch_allowed(app: &App) -> Result<bool> {
    Ok(db_settings::get::<bool>(&app.pool, db_settings::KEY_ALLOW_REMOTE_LAUNCH).await?.unwrap_or(false))
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Target {
    pub rom_id: Option<u64>,
    pub game_id: Option<u64>,
    /// Which emulator (none: the platform's default, else the first that fits).
    pub emulator_id: Option<u64>,
}

/// What a launch opens: the file and its platform.
#[derive(Debug, Clone, Serialize)]
pub struct Resolved {
    pub file: String,
    pub platform_id: u16,
    pub platform: Option<String>,
    /// When `file` is a game's `.m3u`: its first disc, what an emulator that doesn't open
    /// playlists is given instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_disc: Option<String>,
}

impl Resolved {
    /// The file `e` is started with: the playlist when it opens one, else the first disc.
    fn file_for(&self, e: &Emulator) -> &str {
        match &self.first_disc {
            Some(disc) if !e.supports_playlists => disc,
            _ => &self.file,
        }
    }
}

async fn resolve(app: &App, t: &Target) -> Result<Resolved> {
    let pool = &app.pool;
    let rom_row = |sql: &'static str, id: u64| async move { sqlx::query(sql).bind(id).fetch_optional(pool).await };
    let (file, platform_id, first_disc): (String, u16, Option<String>) = match (t.rom_id, t.game_id) {
        (Some(rom), _) => {
            let r = rom_row("SELECT main_path, app_platform_id FROM local_rom WHERE id = ? AND deleted = FALSE", rom).await?.ok_or_else(|| Error::not_found(format!("no rom {rom}")))?;
            (r.try_get("main_path")?, r.try_get("app_platform_id")?, None)
        }
        (None, Some(game)) => {
            let first = rom_row("SELECT lr.main_path, lr.app_platform_id FROM local_game_roms lgr JOIN local_rom lr ON lr.id = lgr.rom_id AND lr.deleted = FALSE WHERE lgr.game_id = ? ORDER BY lr.disc_number IS NULL, lr.disc_number, lr.id LIMIT 1", game)
                .await?
                .ok_or_else(|| Error::not_found(format!("game {game} has no roms")))?;
            let platform: u16 = first.try_get("app_platform_id")?;
            let playlist: Option<String> = sqlx::query_scalar("SELECT f.path FROM local_game lg JOIN local_files f ON f.id = lg.playlist_file_id AND f.deleted = FALSE WHERE lg.id = ?").bind(game).fetch_optional(pool).await?;
            let disc: String = first.try_get("main_path")?;
            match playlist {
                Some(m3u) => (m3u, platform, Some(disc)),
                None => (disc, platform, None),
            }
        }
        (None, None) => return Err(Error::invalid("give rom_id or game_id")),
    };
    if !std::path::Path::new(&file).is_file() {
        return Err(Error::not_found(format!("{file} is not on disk")));
    }
    let platform: Option<String> = sqlx::query_scalar("SELECT name FROM platforms WHERE id = ?").bind(platform_id).fetch_optional(pool).await?;
    Ok(Resolved { file, platform_id, platform, first_disc })
}

#[derive(Debug, Clone, Serialize)]
pub struct LaunchOption {
    pub id: u64,
    pub name: String,
    pub is_default: bool,
    /// Why it can't open this file (e.g. an archive it doesn't read); None = it can.
    pub unusable: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Options {
    #[serde(flatten)]
    pub target: Resolved,
    pub emulators: Vec<LaunchOption>,
}

fn is_archive(file: &str) -> bool {
    std::path::Path::new(file).extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "zip" | "7z" | "rar"))
}

/// The enabled emulators for the target's platform, default first.
pub async fn options(app: &App, t: &Target) -> Result<Options> {
    let target = resolve(app, t).await?;
    let mut out: Vec<LaunchOption> = emulators::list(&app.pool)
        .await?
        .into_iter()
        .filter(|e| e.enabled)
        .filter_map(|e| {
            let p = e.platforms.iter().find(|p| p.app_platform_id == target.platform_id)?;
            let unusable = (is_archive(target.file_for(&e)) && !e.supports_archives).then(|| "doesn't open archives".to_string());
            Some(LaunchOption { id: e.id, name: e.name.clone(), is_default: p.is_default, unusable })
        })
        .collect();
    out.sort_by(|a, b| b.is_default.cmp(&a.is_default).then(a.name.cmp(&b.name)));
    Ok(Options { target, emulators: out })
}

#[derive(Debug, Clone, Serialize)]
pub struct Launched {
    pub emulator: String,
    pub pid: u32,
    pub command: String,
}

pub async fn launch(app: &App, t: &Target) -> Result<Launched> {
    let opts = options(app, t).await?;
    let pick = match t.emulator_id {
        Some(id) => opts.emulators.iter().find(|o| o.id == id).ok_or_else(|| Error::invalid(format!("emulator {id} isn't set up for {}", opts.target.platform.as_deref().unwrap_or("this platform"))))?,
        None => opts.emulators.iter().find(|o| o.unusable.is_none()).ok_or_else(|| Error::invalid(format!("no emulator set up for {} (Config > Emulators)", opts.target.platform.as_deref().unwrap_or("this platform"))))?,
    };
    if let Some(why) = &pick.unusable {
        return Err(Error::invalid(format!("{} {why}", pick.name)));
    }
    let emulator = emulators::get(&app.pool, pick.id).await?.ok_or_else(|| Error::not_found("emulator gone"))?;
    let (pid, command) = emulators::spawn(&emulator, opts.target.platform_id, std::path::Path::new(opts.target.file_for(&emulator)))?;
    log::info!("launched {} (pid {pid}): {command}", emulator.name);
    Ok(Launched { emulator: emulator.name, pid, command })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emulator(supports_playlists: bool) -> Emulator {
        Emulator { id: 1, name: "e".into(), preset: None, kind: "standalone".into(), path: "e".into(), args: "{rom}".into(), cores_dir: None, working_dir: None, supports_archives: false, supports_playlists, enabled: true, platforms: Vec::new(), artwork: None }
    }

    #[test]
    fn a_playlist_goes_only_to_an_emulator_that_opens_one() {
        let game = Resolved { file: "/g/Game.m3u".into(), platform_id: 1, platform: None, first_disc: Some("/g/Game (Disc 1).chd".into()) };
        assert_eq!(game.file_for(&emulator(true)), "/g/Game.m3u");
        assert_eq!(game.file_for(&emulator(false)), "/g/Game (Disc 1).chd");
        let rom = Resolved { file: "/g/Game (Disc 2).chd".into(), platform_id: 1, platform: None, first_disc: None };
        assert_eq!(rom.file_for(&emulator(false)), "/g/Game (Disc 2).chd");
    }
}
