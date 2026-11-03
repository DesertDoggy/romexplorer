//! The `display_data` job: what the sources that know say about how releases are meant to be
//! shown, into `release_display`. Each source replaces only its own rows; the user's stay.
//!
//! - `sfo`: this machine's PS3 games -- their PARAM.SFO lists the resolutions they support.
//! - `ipbin`: this machine's Dreamcast discs -- their IP.BIN says whether the game allows the
//!   VGA box (640x480p).
//! - `pcsx2`: PCSX2's widescreen patches (github.com/PCSX2/pcsx2_patches) -- a PS2 release
//!   with one gets `widescreen = patch`.
//! - `mame`: MAME's machine list -- an arcade release's monitor orientation, resolution and
//!   refresh.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Serialize;

use crate::common::display::{mame_aspect, mame_refresh, mame_resolution, ps3_resolution, serial_key, sfo_u32};
use crate::common::models::Platform;
use crate::database::jobs::Progress;

use super::{App, Error, Result};

pub use romexplorer_types::jobs::DisplayDataRequest;

pub const SOURCES: [&str; 4] = ["sfo", "ipbin", "pcsx2", "mame"];

const PCSX2_PATCHES_ZIP: &str = "https://github.com/PCSX2/pcsx2_patches/archive/refs/heads/main.zip";
const MAME_LATEST: &str = "https://api.github.com/repos/mamedev/mame/releases/latest";

pub fn validate(req: &DisplayDataRequest) -> Result<()> {
    if req.sources.is_empty() {
        return Err(Error::invalid("pick at least one source: sfo, ipbin, pcsx2, mame"));
    }
    if let Some(bad) = req.sources.iter().find(|s| !SOURCES.contains(&s.as_str())) {
        return Err(Error::invalid(format!("unknown display data source {bad:?}: sfo, ipbin, pcsx2 or mame")));
    }
    Ok(())
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DisplayDataSummary {
    pub sfo_releases: u64,
    #[serde(default)]
    pub ipbin_releases: u64,
    pub pcsx2_releases: u64,
    pub mame_releases: u64,
}

pub fn one_liner(s: &DisplayDataSummary) -> String {
    format!(
        "display data: {} PS3 release(s) from their PARAM.SFO, {} Dreamcast release(s) from their IP.BIN, {} PS2 release(s) with a widescreen patch, {} arcade release(s) from MAME",
        s.sfo_releases, s.ipbin_releases, s.pcsx2_releases, s.mame_releases
    )
}

pub async fn run(app: &App, req: &DisplayDataRequest, progress: &Progress) -> Result<DisplayDataSummary> {
    validate(req)?;
    let mut summary = DisplayDataSummary::default();
    if req.sources.iter().any(|s| s == "sfo") {
        summary.sfo_releases = from_sfo(app, progress).await?;
    }
    if req.sources.iter().any(|s| s == "ipbin") {
        summary.ipbin_releases = from_ipbin(app, progress).await?;
    }
    if req.sources.iter().any(|s| s == "pcsx2") {
        summary.pcsx2_releases = from_pcsx2(app, progress).await?;
    }
    if req.sources.iter().any(|s| s == "mame") {
        summary.mame_releases = from_mame(app, progress).await?;
    }
    progress.finish(1, Some(1), &one_liner(&summary), &summary).await;
    Ok(summary)
}

fn downloads(app: &App) -> PathBuf {
    let dats = app.paths.dats_dir();
    dats.parent().map(|p| p.join("display")).unwrap_or_else(|| dats.join("display"))
}

fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent("romexplorer (+https://github.com/DesertDoggy/romexplorer)")
        .timeout(std::time::Duration::from_secs(1800))
        .build()
        .map_err(|e| Error::internal(e.to_string()))
}

async fn download(url: &str, to: &Path) -> Result<()> {
    let bytes = client()?.get(url).send().await.and_then(|r| r.error_for_status()).map_err(|e| Error::invalid(format!("{url}: {e}")))?.bytes().await.map_err(|e| Error::invalid(format!("{url}: {e}")))?;
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir).map_err(|e| Error::internal(format!("{}: {e}", dir.display())))?;
    }
    std::fs::write(to, &bytes).map_err(|e| Error::internal(format!("{}: {e}", to.display())))?;
    Ok(())
}

// --- sfo ---

/// A PS3 game's PARAM.SFO: in its folder (`PS3_GAME/PARAM.SFO`, the main path or a folder
/// above a file of it) or inside its ISO.
fn ps3_param_sfo(main_path: &Path) -> Option<Vec<u8>> {
    if main_path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("iso")) {
        return crate::rom::detect::disc_fs::read_iso_file_bytes_case_insensitive(main_path, &["PS3_GAME", "PARAM.SFO"]);
    }
    for dir in main_path.ancestors().take(4) {
        for candidate in [dir.join("PS3_GAME").join("PARAM.SFO"), dir.join("PARAM.SFO")] {
            if let Ok(bytes) = std::fs::read(&candidate) {
                return Some(bytes);
            }
        }
    }
    None
}

/// This machine's files of `platform`, each with the release it is a dump of.
async fn local_files(app: &App, platform: Platform) -> Result<Vec<(u64, String)>> {
    Ok(sqlx::query_as(
        "SELECT DISTINCT lrg.release_id, lr.main_path FROM local_rom lr JOIN local_game_roms lgr ON lgr.rom_id = lr.id \
         JOIN local_game lg ON lg.id = lgr.game_id AND lg.deleted = FALSE JOIN local_release_games lrg ON lrg.game_id = lg.id \
         WHERE lr.deleted = FALSE AND lr.app_platform_id = ?",
    )
    .bind(platform.igdb_id())
    .fetch_all(&app.pool)
    .await?)
}

async fn from_sfo(app: &App, progress: &Progress) -> Result<u64> {
    let rows = local_files(app, Platform::PlayStation3).await?;
    let total = rows.len() as u64;
    let mut done: HashSet<u64> = HashSet::new();
    for (i, (release, main_path)) in rows.into_iter().enumerate() {
        progress.report(i as u64, Some(total.max(1)), &format!("display data: PS3 PARAM.SFO, {} of {total}", i + 1)).await;
        if done.contains(&release) {
            continue;
        }
        let path = PathBuf::from(&main_path);
        let Some(bytes) = tokio::task::spawn_blocking(move || ps3_param_sfo(&path)).await.map_err(|e| Error::internal(e.to_string()))? else { continue };
        let Some(flags) = sfo_u32(&bytes, "RESOLUTION") else { continue };
        let (modes, widescreen) = ps3_resolution(flags);
        if modes.is_empty() {
            continue;
        }
        // 16:9 beside a plain 480 / 576: an option; 16:9 only: native.
        let wide = if !widescreen {
            "none"
        } else if flags & (1 | 2) != 0 {
            "option"
        } else {
            "native"
        };
        super::display::record(app, release, "modes", "sfo", &modes.join(",")).await?;
        super::display::record(app, release, "widescreen", "sfo", wide).await?;
        done.insert(release);
    }
    Ok(done.len() as u64)
}

// --- ipbin ---

/// The IP.BIN header (0x100 bytes from `SEGA SEGAKATANA`) of a Dreamcast disc: a CHD, a
/// `.gdi` (its high-density data track, the one at LBA 45000), or a track file itself.
/// Archives and DiscJuggler `.cdi` images aren't read: their header isn't at a fixed place.
pub fn dreamcast_header(main_path: &Path) -> Option<Vec<u8>> {
    let ext = main_path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    let from_bytes = |bytes: &[u8]| {
        let at = crate::rom::detect::disc_dispatch::sega_disc_hardware_id_offset(bytes, Platform::Dreamcast)?;
        bytes.get(at..at + 0x100).map(<[u8]>::to_vec)
    };
    let head = |path: &Path| -> Option<Vec<u8>> {
        let mut buf = vec![0u8; 0x1000];
        let mut f = std::fs::File::open(path).ok()?;
        let n = f.read(&mut buf).ok()?;
        buf.truncate(n);
        Some(buf)
    };
    match ext.as_str() {
        "chd" => crate::rom::detect::chd_disc::dreamcast_ip_bin(main_path),
        "gdi" => {
            let text = std::fs::read_to_string(main_path).ok()?;
            let dir = main_path.parent()?;
            // `track lba type sector_size "file" offset`; type 4 is data.
            let mut tracks: Vec<(u64, PathBuf)> = Vec::new();
            for line in text.lines().skip(1) {
                let (head_part, file) = match line.split_once('"') {
                    Some((h, rest)) => (h, rest.split('"').next().unwrap_or("").to_string()),
                    None => (line, line.split_whitespace().nth(4).unwrap_or("").to_string()),
                };
                let fields: Vec<&str> = head_part.split_whitespace().collect();
                if fields.len() >= 3 && fields[2] == "4" && !file.is_empty() {
                    tracks.push((fields[1].parse().unwrap_or(0), dir.join(file)));
                }
            }
            tracks.sort_by_key(|(lba, _)| std::cmp::Reverse(*lba >= 45000));
            tracks.iter().find_map(|(_, path)| head(path).and_then(|b| from_bytes(&b)))
        }
        "bin" | "iso" | "raw" | "img" => head(main_path).and_then(|b| from_bytes(&b)),
        _ => None,
    }
}

async fn from_ipbin(app: &App, progress: &Progress) -> Result<u64> {
    let rows = local_files(app, Platform::Dreamcast).await?;
    let total = rows.len() as u64;
    let mut done: HashSet<u64> = HashSet::new();
    for (i, (release, main_path)) in rows.into_iter().enumerate() {
        progress.report(i as u64, Some(total.max(1)), &format!("display data: Dreamcast IP.BIN, {} of {total}", i + 1)).await;
        if done.contains(&release) {
            continue;
        }
        let path = PathBuf::from(&main_path);
        let Some(header) = tokio::task::spawn_blocking(move || dreamcast_header(&path)).await.map_err(|e| Error::internal(e.to_string()))? else { continue };
        let Some(vga) = crate::common::display::dreamcast_vga(&header) else { continue };
        // On a TV every game is 640x480i; with the VGA box, 640x480p where the game allows it.
        let modes = if vga { "640x480i,640x480p" } else { "640x480i" };
        super::display::record(app, release, "modes", "ipbin", modes).await?;
        done.insert(release);
    }
    Ok(done.len() as u64)
}

// --- pcsx2 ---

/// The serials PCSX2's patches have a widescreen patch for: patch files are named
/// `SERIAL_CRC.pnach`, and a widescreen one says so in its sections or comments.
pub fn widescreen_serials<'a>(patches: impl Iterator<Item = (&'a str, &'a str)>) -> HashSet<String> {
    let mut out = HashSet::new();
    for (name, text) in patches {
        let file = name.rsplit(['/', '\\']).next().unwrap_or(name);
        let Some(stem) = file.strip_suffix(".pnach") else { continue };
        let serial = stem.split('_').next().unwrap_or("");
        if serial.len() < 6 || !text.to_ascii_lowercase().contains("widescreen") {
            continue;
        }
        out.insert(serial_key(serial));
    }
    out
}

async fn from_pcsx2(app: &App, progress: &Progress) -> Result<u64> {
    progress.force(0, None, "display data: downloading PCSX2's widescreen patches").await;
    let zip_path = downloads(app).join("pcsx2_patches.zip");
    download(PCSX2_PATCHES_ZIP, &zip_path).await?;
    let path = zip_path.clone();
    // A GitHub archive of small text files, read whole: the `zip` crate, as `dat_parse` does.
    let serials = tokio::task::spawn_blocking(move || -> std::result::Result<HashSet<String>, String> {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let mut patches: Vec<(String, String)> = Vec::new();
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
            if !entry.name().ends_with(".pnach") {
                continue;
            }
            let mut text = String::new();
            let _ = entry.read_to_string(&mut text);
            patches.push((entry.name().to_string(), text));
        }
        Ok(widescreen_serials(patches.iter().map(|(n, t)| (n.as_str(), t.as_str()))))
    })
    .await
    .map_err(|e| Error::internal(e.to_string()))?
    .map_err(Error::invalid)?;
    let releases: Vec<(u64, String)> = sqlx::query_as("SELECT id, serial FROM `release` WHERE app_platform_id = ? AND serial IS NOT NULL AND deleted = FALSE").bind(Platform::PlayStation2.igdb_id()).fetch_all(&app.pool).await?;
    sqlx::query("DELETE FROM release_display WHERE source = 'pcsx2'").execute(&app.pool).await?;
    let mut n = 0u64;
    for (release, serial) in releases {
        if serial.split([',', ';']).any(|s| serials.contains(&serial_key(s))) {
            super::display::record(app, release, "widescreen", "pcsx2", "patch").await?;
            n += 1;
        }
    }
    Ok(n)
}

// --- mame ---

/// One MAME machine's display.
#[derive(Debug, Clone, PartialEq)]
pub struct MameDisplay {
    pub description: String,
    pub rotate: u32,
    pub refresh: Option<f64>,
    pub width: u32,
    pub height: u32,
    pub pixclock: Option<f64>,
    pub htotal: Option<u32>,
}

/// Every machine of a MAME `-listxml` with a display: short name -> its display (the first
/// `<display>` of a machine with several screens).
pub fn mame_displays<R: BufRead>(reader: R) -> std::result::Result<HashMap<String, MameDisplay>, String> {
    let mut xml = Reader::from_reader(reader);
    let mut buf = Vec::new();
    let mut out = HashMap::new();
    let (mut name, mut description, mut display): (Option<String>, String, Option<MameDisplay>) = (None, String::new(), None);
    let mut in_description = false;
    let attr = |e: &quick_xml::events::BytesStart, key: &[u8]| e.attributes().flatten().find(|a| a.key.as_ref() == key).and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()));
    loop {
        match xml.read_event_into(&mut buf).map_err(|e| e.to_string())? {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) if e.name().as_ref() == b"machine" => {
                name = attr(&e, b"name");
                description.clear();
                display = None;
            }
            Event::Start(e) if e.name().as_ref() == b"description" => in_description = true,
            Event::Text(t) if in_description => description.push_str(&t.unescape().map(|v| v.into_owned()).unwrap_or_default()),
            Event::End(e) if e.name().as_ref() == b"description" => in_description = false,
            Event::Start(e) | Event::Empty(e) if e.name().as_ref() == b"display" && display.is_none() => {
                let num = |key: &[u8]| attr(&e, key).and_then(|v| v.parse::<u32>().ok());
                display = Some(MameDisplay {
                    description: String::new(),
                    rotate: num(b"rotate").unwrap_or(0),
                    refresh: attr(&e, b"refresh").and_then(|v| v.parse().ok()),
                    width: num(b"width").unwrap_or(0),
                    height: num(b"height").unwrap_or(0),
                    pixclock: attr(&e, b"pixclock").and_then(|v| v.parse().ok()),
                    htotal: num(b"htotal"),
                });
            }
            Event::End(e) if e.name().as_ref() == b"machine" => {
                if let (Some(n), Some(d)) = (name.take(), display.take()) {
                    out.insert(n, MameDisplay { description: description.clone(), ..d });
                }
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

async fn from_mame(app: &App, progress: &Progress) -> Result<u64> {
    progress.force(0, None, "display data: finding MAME's latest machine list").await;
    let latest: serde_json::Value = client()?.get(MAME_LATEST).send().await.and_then(|r| r.error_for_status()).map_err(|e| Error::invalid(format!("MAME release: {e}")))?.json().await.map_err(|e| Error::invalid(format!("MAME release: {e}")))?;
    let asset = latest["assets"].as_array().into_iter().flatten().find(|a| a["name"].as_str().is_some_and(|n| n.ends_with("lx.zip"))).ok_or_else(|| Error::invalid("MAME's latest release has no listxml zip"))?;
    let (url, file) = (asset["browser_download_url"].as_str().unwrap_or_default().to_string(), asset["name"].as_str().unwrap_or("mamelx.zip").to_string());
    let zip_path = downloads(app).join(&file);
    if !zip_path.exists() {
        progress.force(0, None, &format!("display data: downloading {file}")).await;
        download(&url, &zip_path).await?;
    }
    progress.force(0, None, "display data: reading MAME's machine list").await;
    let path = zip_path.clone();
    let machines = tokio::task::spawn_blocking(move || -> std::result::Result<HashMap<String, MameDisplay>, String> {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let index = (0..archive.len()).find(|&i| archive.by_index(i).map(|e| e.name().ends_with(".xml")).unwrap_or(false)).ok_or("no XML in MAME's listxml zip")?;
        let entry = archive.by_index(index).map_err(|e| e.to_string())?;
        mame_displays(std::io::BufReader::new(entry))
    })
    .await
    .map_err(|e| Error::internal(e.to_string()))?
    .map_err(Error::invalid)?;
    let by_description: HashMap<String, &MameDisplay> = machines.values().map(|m| (norm(&m.description), m)).collect();
    let releases: Vec<(u64, String)> = sqlx::query_as("SELECT id, name FROM `release` WHERE app_platform_id IN (?, ?) AND deleted = FALSE").bind(Platform::Arcade.igdb_id()).bind(Platform::NeoGeoMvs.igdb_id()).fetch_all(&app.pool).await?;
    sqlx::query("DELETE FROM release_display WHERE source = 'mame'").execute(&app.pool).await?;
    let total = releases.len() as u64;
    let mut n = 0u64;
    for (i, (release, name)) in releases.into_iter().enumerate() {
        progress.report(i as u64, Some(total.max(1)), &format!("display data: MAME, {} of {total} arcade release(s), {n} found", i + 1)).await;
        // RetroAchievements names arcade games by MAME's short name; the DATs by its description.
        let Some(m) = machines.get(&name).or_else(|| by_description.get(&norm(&name)).copied()) else { continue };
        super::display::record(app, release, "aspect", "mame", mame_aspect(m.rotate)).await?;
        if let Some(r) = m.refresh.and_then(mame_refresh) {
            super::display::record(app, release, "refresh", "mame", &r).await?;
        }
        if let Some(r) = mame_resolution(m.width, m.height, m.pixclock, m.htotal) {
            super::display::record(app, release, "modes", "mame", &r).await?;
        }
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widescreen_patches_name_their_serials() {
        let patches = [
            ("pcsx2_patches-main/patches/SLUS-20946_6B5A2C14.pnach", "gametitle=Hoge Quest\n[Widescreen 16:9]\npatch=1,EE,..."),
            ("pcsx2_patches-main/patches/SLPM-65881_00000000.pnach", "[No-Interlacing]\npatch=1,EE,..."),
            ("pcsx2_patches-main/README.md", "widescreen"),
        ];
        let got = widescreen_serials(patches.iter().map(|(n, t)| (*n, *t)));
        assert!(got.contains("SLUS20946"));
        assert!(!got.contains("SLPM65881"));
        assert_eq!(got.len(), 1);
    }

    /// `DC_PROBE="a.chd;b.gdi" cargo test --lib probe_dreamcast_headers -- --ignored --nocapture`:
    /// the peripherals field and VGA flag of real discs.
    #[test]
    #[ignore]
    fn probe_dreamcast_headers() {
        for p in std::env::var("DC_PROBE").unwrap_or_default().split(';').filter(|p| !p.is_empty()) {
            let h = dreamcast_header(Path::new(p));
            let field = h.as_ref().and_then(|h| h.get(0x38..0x40)).map(|b| String::from_utf8_lossy(b).to_string());
            let product = h.as_ref().and_then(|h| h.get(0x40..0x4A)).map(|b| String::from_utf8_lossy(b).to_string());
            println!("{p}: peripherals {field:?} product {product:?} vga {:?}", h.as_deref().and_then(crate::common::display::dreamcast_vga));
        }
    }

    #[test]
    fn mame_machines_give_their_first_display() {
        let xml = r#"<mame><machine name="hogeq"><description>Hoge Quest (World)</description><display tag="screen" type="raster" rotate="270" width="224" height="288" refresh="60.606061"/></machine>
            <machine name="fuga"><description>Fuga &amp; Co</description><display rotate="0" refresh="59.18"/><display rotate="90"/></machine>
            <machine name="nodisplay"><description>A device</description></machine></mame>"#;
        let m = mame_displays(std::io::Cursor::new(xml)).unwrap();
        assert_eq!(m["hogeq"].rotate, 270);
        assert_eq!((m["hogeq"].width, m["hogeq"].height), (224, 288));
        assert_eq!(m["fuga"].description, "Fuga & Co");
        assert_eq!(m["fuga"].rotate, 0);
        assert!(!m.contains_key("nodisplay"));
    }
}
