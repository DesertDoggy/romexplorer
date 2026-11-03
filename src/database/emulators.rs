//! Emulators the launcher opens roms / games with, and how a launch command is built.
//!
//! Presets are the emulators RetroAchievements supports (retroachievements.org's emulator
//! support list): RetroArch with a recommended core per platform, and the standalone ones
//! with RetroAchievements built in. A preset only pre-fills a new emulator row -- the user
//! sets the path, and can edit the arguments and platforms afterwards.
//!
//! Launching never goes through a shell: the argument template is split into arguments as
//! written (quotes group words), and `{rom}` / `{core}` are substituted as whole arguments,
//! so a file name can't inject anything.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sqlx::mysql::MySqlPool;
use sqlx::Row;

use super::error::DatabaseError;
use crate::common::models::Platform;

/// A pre-filled emulator.
#[derive(Debug, Clone, Serialize)]
pub struct Preset {
    pub key: &'static str,
    pub name: &'static str,
    pub kind: &'static str,
    pub args: &'static str,
    pub supports_archives: bool,
    /// Opens a multi-disc game's `.m3u` itself.
    pub supports_playlists: bool,
    /// (platform, RetroArch core -- None for standalone emulators)
    pub platforms: Vec<(u16, Option<&'static str>)>,
    pub url: &'static str,
    /// Where its own art goes, as a new row starts (the folder is the user's to set).
    pub artwork: Option<EmulatorArtwork>,
}

/// Exporting art into an emulator's own folders: where, in which layout, in which formats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmulatorArtwork {
    /// RetroArch's thumbnails folder, or PCSX2's / DuckStation's covers folder.
    #[serde(default)]
    pub dir: String,
    /// `retroarch`: `<dir>/<system>/Named_Boxarts|Named_Titles|Named_Snaps|Named_Logos/<rom>.png`.
    /// `cover_folder`: `<dir>/<serial or rom>.<ext>`, one cover per game (PCSX2, DuckStation).
    /// `cover_folder_name`: `<dir>/<rom>.<ext>`, the same folder named by the game's file name.
    pub layout: String,
    /// The formats it reads, in order of preference: a picture already in one of them is
    /// copied as it is, anything else converted to the first one this build can write.
    #[serde(default)]
    pub formats: Vec<String>,
    /// Wider pictures are scaled down to this (never up); None keeps them as they are.
    #[serde(default)]
    pub max_width: Option<u32>,
    /// Ticked in the copy dialog for a copy to this emulator's platforms.
    #[serde(default)]
    pub after_copy: bool,
}

pub const ARTWORK_LAYOUTS: [&str; 3] = ["retroarch", "cover_folder", "cover_folder_name"];

impl EmulatorArtwork {
    /// RetroArch reads PNG unless "allow non-PNG" is on; 512 px wide is what its own
    /// thumbnail repository uses, which keeps menus quick.
    pub fn retroarch() -> Self {
        Self { dir: String::new(), layout: "retroarch".to_string(), formats: vec!["png".to_string()], max_width: Some(512), after_copy: false }
    }

    /// PCSX2 and DuckStation read JPG, PNG and WebP covers; JPG keeps the folder small.
    pub fn cover_folder() -> Self {
        Self { dir: String::new(), layout: "cover_folder".to_string(), formats: vec!["jpg".to_string(), "png".to_string(), "webp".to_string()], max_width: None, after_copy: false }
    }
}

/// RetroArch's recommended core per platform (RetroAchievements-compatible cores).
const RETROARCH_CORES: [(Platform, &str); 34] = [
    (Platform::Nes, "fceumm"),
    (Platform::Famicom, "fceumm"),
    (Platform::FamicomDiskSystem, "fceumm"),
    (Platform::Snes, "snes9x"),
    (Platform::SuperFamicom, "snes9x"),
    (Platform::Nintendo64, "mupen64plus_next"),
    (Platform::GameBoy, "gambatte"),
    (Platform::GameBoyColor, "gambatte"),
    (Platform::GameBoyAdvance, "mgba"),
    (Platform::NintendoDs, "melonds"),
    (Platform::VirtualBoy, "mednafen_vb"),
    (Platform::PokemonMini, "pokemini"),
    (Platform::SegaGenesis, "genesis_plus_gx"),
    (Platform::SegaMasterSystem, "genesis_plus_gx"),
    (Platform::SegaGameGear, "genesis_plus_gx"),
    (Platform::SegaCd, "genesis_plus_gx"),
    (Platform::Sg1000, "genesis_plus_gx"),
    (Platform::Sega32x, "picodrive"),
    (Platform::SegaSaturn, "mednafen_saturn"),
    (Platform::Dreamcast, "flycast"),
    (Platform::PlayStation, "swanstation"),
    (Platform::PlayStationPortable, "ppsspp"),
    (Platform::TurboGrafx16, "mednafen_pce_fast"),
    (Platform::TurboGrafxCd, "mednafen_pce_fast"),
    (Platform::SuperGrafx, "mednafen_supergrafx"),
    (Platform::PcFx, "mednafen_pcfx"),
    (Platform::NeoGeoPocket, "mednafen_ngp"),
    (Platform::NeoGeoPocketColor, "mednafen_ngp"),
    (Platform::WonderSwan, "mednafen_wswan"),
    (Platform::WonderSwanColor, "mednafen_wswan"),
    (Platform::AtariLynx, "handy"),
    (Platform::Atari2600, "stella"),
    (Platform::Atari7800, "prosystem"),
    (Platform::AtariJaguar, "virtualjaguar"),
];

pub fn presets() -> Vec<Preset> {
    let ids = |ps: &[Platform]| ps.iter().map(|p| (p.igdb_id(), None)).collect::<Vec<_>>();
    vec![
        Preset {
            key: "retroarch",
            name: "RetroArch",
            kind: "retroarch",
            args: "-L {core} {rom}",
            supports_archives: true, supports_playlists: true,
            platforms: RETROARCH_CORES.iter().map(|(p, core)| (p.igdb_id(), Some(*core))).collect(),
            url: "https://www.retroarch.com/",
            artwork: Some(EmulatorArtwork::retroarch()),
        },
        Preset { key: "duckstation", name: "DuckStation", kind: "standalone", args: "{rom}", supports_archives: false, supports_playlists: true, platforms: ids(&[Platform::PlayStation]), url: "https://www.duckstation.org/", artwork: Some(EmulatorArtwork::cover_folder()) },
        Preset { key: "pcsx2", name: "PCSX2", kind: "standalone", args: "{rom}", supports_archives: false, supports_playlists: false, platforms: ids(&[Platform::PlayStation2]), url: "https://pcsx2.net/", artwork: Some(EmulatorArtwork::cover_folder()) },
        Preset { key: "ppsspp", name: "PPSSPP", kind: "standalone", args: "{rom}", supports_archives: true, supports_playlists: false, platforms: ids(&[Platform::PlayStationPortable]), url: "https://www.ppsspp.org/", artwork: None },
        Preset { key: "dolphin", name: "Dolphin", kind: "standalone", args: "-e {rom}", supports_archives: false, supports_playlists: true, platforms: ids(&[Platform::GameCube, Platform::Wii]), url: "https://dolphin-emu.org/", artwork: None },
        Preset { key: "flycast", name: "Flycast", kind: "standalone", args: "{rom}", supports_archives: true, supports_playlists: false, platforms: ids(&[Platform::Dreamcast]), url: "https://github.com/flyinghead/flycast", artwork: None },
        Preset {
            key: "bizhawk",
            name: "BizHawk",
            kind: "standalone",
            args: "{rom}",
            supports_archives: true, supports_playlists: false,
            platforms: ids(&[
                Platform::Nes,
                Platform::Snes,
                Platform::Nintendo64,
                Platform::GameBoy,
                Platform::GameBoyColor,
                Platform::GameBoyAdvance,
                Platform::NintendoDs,
                Platform::SegaGenesis,
                Platform::SegaMasterSystem,
                Platform::SegaGameGear,
                Platform::SegaSaturn,
                Platform::PlayStation,
                Platform::TurboGrafx16,
                Platform::Atari2600,
                Platform::AtariLynx,
            ]),
            url: "https://tasvideos.org/BizHawk",
            artwork: None,
        },
        Preset {
            key: "ralibretro",
            name: "RALibretro",
            kind: "standalone",
            args: "{rom}",
            supports_archives: true, supports_playlists: true,
            platforms: RETROARCH_CORES.iter().map(|(p, _)| (p.igdb_id(), None)).collect(),
            url: "https://github.com/RetroAchievements/RALibretro",
            artwork: None,
        },
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmulatorPlatform {
    pub app_platform_id: u16,
    #[serde(default)]
    pub core: Option<String>,
    #[serde(default)]
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Emulator {
    #[serde(default)]
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub preset: Option<String>,
    /// standalone | retroarch | command
    pub kind: String,
    pub path: String,
    #[serde(default = "default_args")]
    pub args: String,
    #[serde(default)]
    pub cores_dir: Option<String>,
    #[serde(default)]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub supports_archives: bool,
    /// Opens a multi-disc game's `.m3u` itself; when false, a game starts from its first disc.
    #[serde(default = "default_true")]
    pub supports_playlists: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub platforms: Vec<EmulatorPlatform>,
    /// Where `artwork_export` writes its art; None for an emulator nobody exports to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artwork: Option<EmulatorArtwork>,
}

fn default_args() -> String {
    "{rom}".to_string()
}

fn default_true() -> bool {
    true
}

pub async fn list(pool: &MySqlPool) -> Result<Vec<Emulator>, DatabaseError> {
    let rows = sqlx::query("SELECT id, name, preset, CAST(kind AS CHAR) AS kind, path, args, cores_dir, working_dir, supports_archives, supports_playlists, enabled, artwork FROM emulators ORDER BY name, id").fetch_all(pool).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let id: u64 = r.try_get("id")?;
        let platforms = sqlx::query("SELECT app_platform_id, core, is_default FROM emulator_platforms WHERE emulator_id = ? ORDER BY app_platform_id")
            .bind(id)
            .fetch_all(pool)
            .await?
            .iter()
            .map(|p| Ok(EmulatorPlatform { app_platform_id: p.try_get("app_platform_id")?, core: p.try_get("core")?, is_default: p.try_get("is_default")? }))
            .collect::<Result<Vec<_>, sqlx::Error>>()?;
        out.push(Emulator {
            id,
            name: r.try_get("name")?,
            preset: r.try_get("preset")?,
            kind: r.try_get("kind")?,
            path: r.try_get("path")?,
            args: r.try_get("args")?,
            cores_dir: r.try_get("cores_dir")?,
            working_dir: r.try_get("working_dir")?,
            supports_archives: r.try_get("supports_archives")?,
            supports_playlists: r.try_get("supports_playlists")?,
            enabled: r.try_get("enabled")?,
            platforms,
            // A row whose JSON no longer reads (hand-edited) exports nothing rather than failing.
            artwork: r.try_get::<Option<serde_json::Value>, _>("artwork")?.and_then(|v| serde_json::from_value(v).ok()),
        });
    }
    Ok(out)
}

pub async fn get(pool: &MySqlPool, id: u64) -> Result<Option<Emulator>, DatabaseError> {
    Ok(list(pool).await?.into_iter().find(|e| e.id == id))
}

/// Inserts (`e.id == 0`) or replaces an emulator and its platforms. A platform marked
/// default here stops being another emulator's default. Returns the id.
pub async fn save(pool: &MySqlPool, e: &Emulator) -> Result<u64, DatabaseError> {
    let mut tx = pool.begin().await?;
    let id = if e.id == 0 {
        sqlx::query("INSERT INTO emulators (name, preset, kind, path, args, cores_dir, working_dir, supports_archives, supports_playlists, enabled, artwork) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(&e.name)
            .bind(&e.preset)
            .bind(&e.kind)
            .bind(&e.path)
            .bind(&e.args)
            .bind(&e.cores_dir)
            .bind(&e.working_dir)
            .bind(e.supports_archives)
            .bind(e.supports_playlists)
            .bind(e.enabled)
            .bind(e.artwork.as_ref().map(serde_json::to_value).transpose()?)
            .execute(&mut *tx)
            .await?
            .last_insert_id()
    } else {
        sqlx::query("UPDATE emulators SET name = ?, preset = ?, kind = ?, path = ?, args = ?, cores_dir = ?, working_dir = ?, supports_archives = ?, supports_playlists = ?, enabled = ?, artwork = ? WHERE id = ?")
            .bind(&e.name)
            .bind(&e.preset)
            .bind(&e.kind)
            .bind(&e.path)
            .bind(&e.args)
            .bind(&e.cores_dir)
            .bind(&e.working_dir)
            .bind(e.supports_archives)
            .bind(e.supports_playlists)
            .bind(e.enabled)
            .bind(e.artwork.as_ref().map(serde_json::to_value).transpose()?)
            .bind(e.id)
            .execute(&mut *tx)
            .await?;
        e.id
    };
    sqlx::query("DELETE FROM emulator_platforms WHERE emulator_id = ?").bind(id).execute(&mut *tx).await?;
    for p in &e.platforms {
        if p.is_default {
            sqlx::query("UPDATE emulator_platforms SET is_default = FALSE WHERE app_platform_id = ?").bind(p.app_platform_id).execute(&mut *tx).await?;
        }
        sqlx::query("INSERT INTO emulator_platforms (emulator_id, app_platform_id, core, is_default) VALUES (?, ?, ?, ?)")
            .bind(id)
            .bind(p.app_platform_id)
            .bind(p.core.as_deref().filter(|c| !c.trim().is_empty()))
            .bind(p.is_default)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(id)
}

pub async fn delete(pool: &MySqlPool, id: u64) -> Result<bool, DatabaseError> {
    Ok(sqlx::query("DELETE FROM emulators WHERE id = ?").bind(id).execute(pool).await?.rows_affected() > 0)
}

/// Splits an argument template into arguments: whitespace separates, double quotes group
/// ("a b" is one argument), no other shell syntax.
pub fn split_args(template: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut any) = (false, false);
    for c in template.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

/// The arguments for one launch: each template argument with `{rom}` / `{core}` replaced
/// (an argument that is only `{core}` is dropped when there is no core).
pub fn build_args(template: &str, rom: &str, core: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    let args = split_args(template);
    for (i, a) in args.iter().enumerate() {
        if core.is_none() && (a == "{core}" || (a == "-L" && args.get(i + 1).map(String::as_str) == Some("{core}"))) {
            continue;
        }
        out.push(a.replace("{rom}", rom).replace("{core}", core.unwrap_or("")));
    }
    out
}

/// The program to run: a macOS `.app` bundle's executable (`Contents/MacOS/<CFBundleExecutable>`),
/// otherwise the path itself. `command` kinds split their first word off as the program.
pub fn program_and_prefix(kind: &str, path: &str) -> (PathBuf, Vec<String>) {
    if kind == "command" {
        let mut parts = split_args(path);
        let program = if parts.is_empty() { String::new() } else { parts.remove(0) };
        return (PathBuf::from(program), parts);
    }
    let p = Path::new(path);
    if p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("app")) && p.is_dir() {
        let plist = std::fs::read_to_string(p.join("Contents/Info.plist")).unwrap_or_default();
        let exe = plist
            .split("<key>CFBundleExecutable</key>")
            .nth(1)
            .and_then(|rest| rest.split("<string>").nth(1))
            .and_then(|rest| rest.split("</string>").next())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string());
        return (p.join("Contents/MacOS").join(exe), Vec::new());
    }
    (p.to_path_buf(), Vec::new())
}

/// A RetroArch core's file: `<cores dir>/<core>_libretro.<dll|dylib|so>`. The cores dir is
/// the configured one, else `cores` next to the executable (Windows / portable), else the
/// OS default location.
pub fn core_path(emulator: &Emulator, core: &str) -> PathBuf {
    let ext = if cfg!(windows) {
        "dll"
    } else if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    };
    let file = format!("{core}_libretro.{ext}");
    if let Some(dir) = emulator.cores_dir.as_deref().filter(|d| !d.trim().is_empty()) {
        return Path::new(dir).join(file);
    }
    let beside = Path::new(&emulator.path).parent().map(|d| d.join("cores"));
    if let Some(dir) = beside.filter(|d| d.is_dir()) {
        return dir.join(file);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/RetroArch/cores").join(file)
    } else {
        home.join(".config/retroarch/cores").join(file)
    }
}

/// Starts the emulator on `file` (detached; the app doesn't wait). Returns the process id and
/// the command line as run (for the log / the user).
pub fn spawn(emulator: &Emulator, platform: u16, file: &Path) -> Result<(u32, String), DatabaseError> {
    let core = emulator.platforms.iter().find(|p| p.app_platform_id == platform).and_then(|p| p.core.clone());
    let core_file = core.as_deref().map(|c| core_path(emulator, c));
    if let Some(cf) = &core_file {
        if !cf.is_file() {
            return Err(DatabaseError::Config(format!("RetroArch core not found: {}", cf.display())));
        }
    }
    let (program, prefix) = program_and_prefix(&emulator.kind, &emulator.path);
    if emulator.kind != "command" && !program.is_file() {
        return Err(DatabaseError::Config(format!("emulator not found: {}", program.display())));
    }
    let rom = file.display().to_string();
    let core_s = core_file.as_ref().map(|p| p.display().to_string());
    let mut args = prefix;
    args.extend(build_args(&emulator.args, &rom, core_s.as_deref()));
    let mut cmd = std::process::Command::new(&program);
    cmd.args(&args);
    if let Some(dir) = emulator.working_dir.as_deref().filter(|d| !d.trim().is_empty()) {
        cmd.current_dir(dir);
    } else if let Some(dir) = program.parent().filter(|d| d.is_dir()) {
        cmd.current_dir(dir);
    }
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    let child = cmd.spawn().map_err(|e| DatabaseError::Config(format!("couldn't start {}: {e}", program.display())))?;
    let shown = std::iter::once(program.display().to_string()).chain(args.iter().map(|a| if a.contains(' ') { format!("\"{a}\"") } else { a.clone() })).collect::<Vec<_>>().join(" ");
    Ok((child.id(), shown))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_split_and_substitute() {
        assert_eq!(split_args("-L {core} {rom}"), vec!["-L", "{core}", "{rom}"]);
        assert_eq!(split_args(r#"-batch "--fullscreen now" {rom}"#), vec!["-batch", "--fullscreen now", "{rom}"]);
        // A file name with spaces or shell characters stays one argument.
        let rom = r#"C:\roms\Game (USA) & "x".cue"#;
        assert_eq!(build_args("-e {rom}", rom, None), vec!["-e".to_string(), rom.to_string()]);
        assert_eq!(build_args("-L {core} {rom}", "g.chd", Some("c.dll")), vec!["-L", "c.dll", "g.chd"]);
        assert_eq!(build_args("-L {core} {rom}", "g.chd", None), vec!["g.chd"], "no core: -L {{core}} dropped");
    }

    #[test]
    fn command_kind_splits_program() {
        let (program, prefix) = program_and_prefix("command", "flatpak run org.libretro.RetroArch");
        assert_eq!(program, PathBuf::from("flatpak"));
        assert_eq!(prefix, vec!["run", "org.libretro.RetroArch"]);
    }

    #[test]
    fn presets_cover_ra_platforms() {
        let p = presets();
        assert!(p.iter().any(|x| x.key == "retroarch" && x.platforms.iter().any(|(id, core)| *id == Platform::PlayStation.igdb_id() && core.is_some())));
        assert!(p.iter().any(|x| x.key == "duckstation"));
    }
}
