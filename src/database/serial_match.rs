//! Lenient matching: a game whose files no DAT hashes (a `.cia`, an `.xiso`, a `.wua`, a
//! `.zar`, a `.vpk`, a Vita app folder) is linked to the release whose DAT serial is the one in
//! the file's header (`rom::serial`), when the `serial_match` setting allows that format
//! ([`format_of`]). Formats a DAT does hash (PSN packages, PlayStation discs) can be turned on
//! too, for dumps that no longer match their hashes. The link is marked
//! (`local_release_games.serial_match`), so it is never taken for a verified dump.
//!
//! DAT serials are compared in a normalized form ([`key`]), and a DAT serial that only adds
//! a suffix to the file's also counts: Redump writes Wii U serials as `WUP-P-XXXX-USA-0`
//! where `meta.xml` says `WUP-P-XXXX`. One serial can belong to several releases (regions,
//! revisions); the region in the file name decides, then the source order, and a game still
//! tied is left unmatched for a person to settle.

use std::path::Path;

use sqlx::mysql::MySqlPool;
use sqlx::Row;

use super::error::DatabaseError;
use super::matching::name_parse::{parse_name, RegionTag};
use super::settings::SerialMatch;
use crate::common::models::Platform;
use crate::rom::platforms::sony::{self, ContentKind};
use crate::rom::platforms::switch;
use crate::rom::serial::{self, FoundSerial, SerialOrigin};

/// A serial as compared: upper case, `_` and spaces as `-`, dots dropped (`SLUS_123.45` →
/// `SLUS-12345`).
pub fn key(serial: &str) -> String {
    serial
        .trim()
        .chars()
        .filter(|c| *c != '.')
        .map(|c| if c == '_' || c == ' ' { '-' } else { c.to_ascii_uppercase() })
        .collect()
}

/// The serials one DAT field holds: Redump lists every serial a disc was sold under
/// (`SLPS-00001, SLPS-00002`). Placeholders (`n/a`) are dropped.
pub fn dat_serials(field: &str) -> Vec<String> {
    field.split([',', ';']).flat_map(|s| s.split(" / ")).map(key).filter(|s| s.len() >= 3 && s != "N/A" && s != "NA").collect()
}

/// Whether a file's serial is this DAT serial: the same, or the DAT's with a suffix after a
/// dash (`WUP-P-XXXX` and `WUP-P-XXXX-USA-0`), in any of its [`spellings`].
pub fn same_serial(file: &str, dat: &str) -> bool {
    let dat = key(dat);
    spellings(file).into_iter().any(|file| !file.is_empty() && (file == dat || (dat.len() > file.len() && dat.starts_with(&file) && dat.as_bytes()[file.len()] == b'-')))
}

/// A serial's spellings, as [`key`]s: as read, and a Sony title id with and without the dash
/// after its four letters, since a PARAM.SFO writes `BLES00000` and Redump `BLES-00000`.
pub fn spellings(serial: &str) -> Vec<String> {
    let k = key(serial);
    let b = k.as_bytes();
    let letters = |s: &[u8]| s.len() >= 4 && s[..4].iter().all(u8::is_ascii_uppercase);
    let digits = |s: &[u8]| s.len() == 5 && s.iter().all(u8::is_ascii_digit);
    let other = if b.len() == 9 && letters(b) && digits(&b[4..]) {
        Some(format!("{}-{}", &k[..4], &k[4..]))
    } else if b.len() == 10 && letters(b) && b[4] == b'-' && digits(&b[5..]) {
        Some(format!("{}{}", &k[..4], &k[5..]))
    } else {
        None
    };
    std::iter::once(k).chain(other).collect()
}

/// Which `serial_match` format a file is (`settings::SERIAL_MATCH_FORMATS`), if any.
pub fn format_of(path: &Path, platform: Platform) -> Option<&'static str> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name.eq_ignore_ascii_case("param.sfo") {
        let parent = path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()).unwrap_or("");
        return if parent.eq_ignore_ascii_case("sce_sys") {
            Some("vita_folder")
        } else if parent.eq_ignore_ascii_case("PS3_GAME") {
            Some("ps_disc")
        } else {
            None
        };
    }
    if serial::is_ps_disc(&ext, platform) {
        return Some("ps_disc");
    }
    match ext.as_str() {
        "cia" => Some("cia"),
        "xiso" => Some("xiso"),
        "wua" => Some("wua"),
        "zar" => Some("zar"),
        "vpk" => Some("vpk"),
        "pkg" => Some("pkg"),
        "psv" => Some("psv"),
        "img" if platform == Platform::PlayStationVita => Some("psv"),
        "nsp" | "nsz" => Some("nsp"),
        _ => None,
    }
}

/// The platforms a format's releases are under, when the rom's own is not known yet.
fn platforms_for(format: &str) -> &'static [Platform] {
    match format {
        "cia" => &[Platform::Nintendo3ds, Platform::NewNintendo3ds],
        "wua" => &[Platform::WiiU],
        "zar" => &[Platform::Xbox360],
        "xiso" => &[Platform::Xbox, Platform::Xbox360],
        "vpk" | "vita_folder" | "psv" => &[Platform::PlayStationVita],
        "pkg" => &[Platform::PlayStationVita, Platform::PlayStation3, Platform::PlayStationPortable, Platform::PspPs1Classic],
        "ps_disc" => &[Platform::PlayStation3],
        "nsp" => &[Platform::NintendoSwitch],
        _ => &[],
    }
}

/// The platforms to look a rom's serial up under: its own, with its sibling (3DS / New 3DS,
/// whose DATs list each other's games), else what the format can be.
fn search_platforms(format: &str, rom_platform: Platform) -> Vec<Platform> {
    match rom_platform {
        Platform::Unknown => platforms_for(format).to_vec(),
        Platform::Nintendo3ds | Platform::NewNintendo3ds => vec![Platform::Nintendo3ds, Platform::NewNintendo3ds],
        p => vec![p],
    }
}

/// The serials of one file the setting allows, surest first.
pub fn usable_serials(path: &Path, platform: Platform, cfg: &SerialMatch) -> Vec<FoundSerial> {
    let Some(format) = format_of(path, platform).filter(|f| cfg.formats.contains(f)) else { return Vec::new() };
    let guess = if platform == Platform::Unknown { platforms_for(format).first().copied().unwrap_or(Platform::Unknown) } else { platform };
    let mut found: Vec<FoundSerial> = serial::read(path, guess)
        .into_iter()
        .filter(|f| match f.origin {
            SerialOrigin::Header => true,
            SerialOrigin::Manifest => cfg.manifest,
            // An NSP holds no title id outside its encrypted NCAs unless it carries a ticket,
            // so the `[0100…]` its dumper put in the name is what there is.
            SerialOrigin::FileName => cfg.file_name || (format == "nsp" && f.serial.len() == 16),
        })
        .collect();
    found.sort_by_key(|f| f.origin);
    found
}

/// One release a serial lookup found.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub release_id: u64,
    pub platform_id: u16,
    pub name: String,
    /// The sources listing it, by name.
    pub sources: Vec<String>,
}

/// Which of `candidates` a file named `file_name` is: alone, else the ones whose region the
/// name shares, then the best ranked source (`source_order`, a name not in it last). `None`
/// when that still leaves more than one.
pub fn pick<'a>(candidates: &'a [Candidate], file_name: &str, source_order: &[String]) -> Option<&'a Candidate> {
    if candidates.len() <= 1 {
        return candidates.first();
    }
    let regions = |name: &str| -> Vec<RegionTag> { parse_name(name).regions };
    let mine = regions(file_name);
    let mut left: Vec<&Candidate> = candidates.iter().collect();
    if !mine.is_empty() {
        let same: Vec<&Candidate> = left.iter().copied().filter(|c| regions(&c.name).iter().any(|r| mine.contains(r))).collect();
        if !same.is_empty() {
            left = same;
        }
    }
    let rank = |c: &Candidate| c.sources.iter().filter_map(|s| source_order.iter().position(|o| o == s)).min().unwrap_or(usize::MAX);
    let best = left.iter().map(|c| rank(c)).min()?;
    let mut top = left.into_iter().filter(|c| rank(c) == best);
    let first = top.next()?;
    top.next().is_none().then_some(first)
}

/// The releases on `platforms` one of whose DAT serials is `serial`.
async fn releases_with_serial(pool: &MySqlPool, platforms: &[Platform], serial: &str) -> Result<Vec<Candidate>, DatabaseError> {
    if platforms.is_empty() {
        return Ok(Vec::new());
    }
    let ids = platforms.iter().map(|p| p.igdb_id().to_string()).collect::<Vec<_>>().join(",");
    // The LIKE narrows to rows that mention one of its spellings; `same_serial` then decides.
    let forms = spellings(serial);
    let like = vec!["r.serial LIKE CONCAT('%', ?, '%')"; forms.len()].join(" OR ");
    let mut query = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT r.id, r.app_platform_id, r.name, r.serial, \
                (SELECT GROUP_CONCAT(s.name) FROM release_sources rs JOIN sources s ON s.id = rs.source_id WHERE rs.release_id = r.id) AS sources \
         FROM `release` r WHERE r.deleted = FALSE AND r.app_platform_id IN ({ids}) AND ({like}) LIMIT 200"
    )));
    for form in &forms {
        query = query.bind(form);
    }
    let rows = query.fetch_all(pool).await?;
    let mut out = Vec::new();
    for row in rows {
        let field: Option<String> = row.try_get("serial")?;
        if !field.as_deref().map(dat_serials).unwrap_or_default().iter().any(|d| same_serial(serial, d)) {
            continue;
        }
        let sources: Option<String> = row.try_get("sources")?;
        out.push(Candidate {
            release_id: row.try_get("id")?,
            platform_id: row.try_get("app_platform_id")?,
            name: row.try_get("name")?,
            sources: sources.map(|s| s.split(',').map(str::to_string).collect()).unwrap_or_default(),
        });
    }
    Ok(out)
}

/// The candidates that are what the file is -- an update, an add-on or the game -- by the
/// tags in their names, when that leaves any: a Vita game's update and add-ons share its
/// title id.
pub fn of_kind(candidates: Vec<Candidate>, kind: Option<ContentKind>) -> Vec<Candidate> {
    let Some(kind) = kind else { return candidates };
    let tagged = |c: &Candidate, tags: &[&str]| {
        let name = c.name.to_ascii_lowercase();
        tags.iter().any(|t| name.contains(t))
    };
    const UPDATE: &[&str] = &["(update", "update)", "(patch"];
    const DLC: &[&str] = &["(dlc", "dlc)", "(add-on", "add-on)"];
    let keep = |c: &Candidate| match kind {
        ContentKind::Update => tagged(c, UPDATE),
        ContentKind::Dlc => tagged(c, DLC),
        ContentKind::UpdateOrDlc => tagged(c, UPDATE) || tagged(c, DLC),
        ContentKind::Game | ContentKind::Other => !tagged(c, UPDATE) && !tagged(c, DLC),
    };
    if candidates.iter().any(keep) {
        candidates.into_iter().filter(keep).collect()
    } else {
        candidates
    }
}

/// What [`match_games`] did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub matched: u64,
    /// Games whose serial belongs to several releases that nothing told apart.
    pub ambiguous: u64,
}

/// Tries each of `games` (none linked to a release yet) by serial; returns the games it
/// linked, and counts.
pub async fn match_games(pool: &MySqlPool, games: &[u64], cfg: &SerialMatch, source_order: &[String]) -> Result<(Vec<u64>, Outcome), DatabaseError> {
    let mut linked = Vec::new();
    let mut outcome = Outcome::default();
    if cfg.formats.is_empty() {
        return Ok((linked, outcome));
    }
    for &game_id in games {
        let files = sqlx::query(
            "SELECT f.path, lr.app_platform_id FROM local_game_roms lgr JOIN local_rom lr ON lr.id = lgr.rom_id AND lr.deleted = FALSE \
             JOIN local_rom_files lrf ON lrf.rom_id = lr.id JOIN local_files f ON f.id = lrf.file_id AND f.deleted = FALSE WHERE lgr.game_id = ? ORDER BY f.path",
        )
        .bind(game_id)
        .fetch_all(pool)
        .await?;
        let mut tied = false;
        'files: for row in files {
            let path: String = row.try_get("path")?;
            let platform = Platform::from_igdb_id(row.try_get::<u16, _>("app_platform_id")?).unwrap_or(Platform::Unknown);
            let Some(format) = format_of(Path::new(&path), platform).filter(|f| cfg.formats.contains(f)) else { continue };
            let (owned, cfg2) = (path.clone(), cfg.clone());
            let (found, kind) = tokio::task::spawn_blocking(move || {
                let path = Path::new(&owned);
                let kind = match format {
                    "vpk" | "vita_folder" | "pkg" | "psv" | "ps_disc" => sony::serial_and_kind(path).map(|(_, k)| k),
                    "nsp" => switch::title_id_of(path).map(|(id, _)| switch::kind_of_title_id(&id)),
                    _ => None,
                };
                (usable_serials(path, platform, &cfg2), kind)
            })
            .await
            .unwrap_or_default();
            let file_name = Path::new(&path).file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            for f in found {
                let candidates = releases_with_serial(pool, &search_platforms(format, platform), &f.serial).await?;
                if candidates.is_empty() {
                    continue;
                }
                let candidates = of_kind(candidates, kind);
                let Some(best) = pick(&candidates, &file_name, source_order) else {
                    tied = true;
                    continue;
                };
                let origin = match f.origin {
                    SerialOrigin::Header => "header",
                    SerialOrigin::Manifest => "manifest",
                    SerialOrigin::FileName => "file_name",
                };
                sqlx::query("INSERT IGNORE INTO local_release_games (release_id, game_id, serial_match, serial_matched) VALUES (?, ?, ?, ?)")
                    .bind(best.release_id)
                    .bind(game_id)
                    .bind(origin)
                    .bind(&f.serial)
                    .execute(pool)
                    .await?;
                sqlx::query("UPDATE local_rom SET app_platform_id = ? WHERE app_platform_id = 0 AND id IN (SELECT rom_id FROM local_game_roms WHERE game_id = ?)")
                    .bind(best.platform_id)
                    .bind(game_id)
                    .execute(pool)
                    .await?;
                linked.push(game_id);
                outcome.matched += 1;
                tied = false;
                break 'files;
            }
        }
        if tied {
            outcome.ambiguous += 1;
        }
    }
    Ok((linked, outcome))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dat_serials_and_their_forms() {
        assert_eq!(dat_serials("SLPS-00001, SLPS-00002"), ["SLPS-00001", "SLPS-00002"]);
        assert!(dat_serials("n/a").is_empty());
        assert!(same_serial("WUP-P-ABCD", "WUP-P-ABCD-USA-0"));
        assert!(same_serial("ms-004", "MS-004"));
        assert!(same_serial("SLUS_123.45", "SLUS-12345"));
        assert!(!same_serial("WUP-P-ABC", "WUP-P-ABCD-USA-0"));
        assert!(!same_serial("MS-00", "MS-004"));
    }

    fn c(id: u64, name: &str, sources: &[&str]) -> Candidate {
        Candidate { release_id: id, platform_id: 0, name: name.into(), sources: sources.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn sony_title_ids_match_with_or_without_the_dash() {
        assert_eq!(spellings("ABCD00001"), ["ABCD00001", "ABCD-00001"]);
        assert_eq!(spellings("abcd-00001"), ["ABCD-00001", "ABCD00001"]);
        assert_eq!(spellings("WUP-P-ABCD"), ["WUP-P-ABCD"]);
        assert!(same_serial("ABCD00001", "ABCD-00001"));
        assert!(same_serial("ABCD-00001", "ABCD00001"));
        assert!(!same_serial("ABCD00001", "ABCD-00002"));
    }

    #[test]
    fn formats_by_extension_folder_and_platform() {
        let f = |p: &str, platform| format_of(Path::new(p), platform);
        assert_eq!(f("g/TEST.vpk", Platform::Unknown), Some("vpk"));
        assert_eq!(f("g/ABCD00001/sce_sys/param.sfo", Platform::PlayStationVita), Some("vita_folder"));
        assert_eq!(f("g/TEST/PS3_GAME/PARAM.SFO", Platform::PlayStation3), Some("ps_disc"));
        assert_eq!(f("g/TEST/USRDIR/PARAM.SFO", Platform::PlayStation3), None);
        assert_eq!(f("g/TEST.iso", Platform::PlayStation2), Some("ps_disc"));
        assert_eq!(f("g/TEST.iso", Platform::Xbox), None, "an Xbox .iso is not an .xiso");
        assert_eq!(f("g/TEST.img", Platform::PlayStationVita), Some("psv"));
        assert_eq!(f("g/TEST.img", Platform::Unknown), None);
        assert_eq!(f("g/TEST.pkg", Platform::PlayStation3), Some("pkg"));
        assert_eq!(f("g/TEST.nsz", Platform::Unknown), Some("nsp"));
        assert_eq!(f("g/TEST.xci", Platform::NintendoSwitch), None, "No-Intro hashes cartridge images");
    }

    #[test]
    fn an_update_goes_to_the_update_release() {
        let all = vec![c(1, "TEST TITLE (Japan)", &[]), c(2, "TEST TITLE (Japan) (Update)", &[]), c(3, "TEST TITLE (Japan) (DLC)", &[])];
        let ids = |kind| of_kind(all.clone(), kind).iter().map(|c| c.release_id).collect::<Vec<_>>();
        assert_eq!(ids(Some(ContentKind::Game)), [1]);
        assert_eq!(ids(Some(ContentKind::Update)), [2]);
        assert_eq!(ids(Some(ContentKind::Dlc)), [3]);
        assert_eq!(ids(None), [1, 2, 3]);
        // Nothing tagged as asked: all of them, for the region and sources to decide.
        assert_eq!(of_kind(vec![c(1, "TEST TITLE (Japan)", &[])], Some(ContentKind::Update)).len(), 1);
    }

    #[test]
    fn several_releases_are_told_apart_by_region_then_source() {
        let order = vec!["redump".to_string(), "no-intro".to_string()];
        let one = [c(1, "Game (USA)", &["no-intro"])];
        assert_eq!(pick(&one, "anything.cia", &order).map(|c| c.release_id), Some(1));

        let regions = [c(1, "Game (USA)", &["no-intro"]), c(2, "Game (Europe)", &["no-intro"])];
        assert_eq!(pick(&regions, "Game (Europe).cia", &order).map(|c| c.release_id), Some(2));
        assert_eq!(pick(&regions, "Game.cia", &order).map(|c| c.release_id), None, "no region in the name: left for review");

        let sources = [c(1, "Game (USA)", &["no-intro"]), c(2, "Game (USA)", &["redump"])];
        assert_eq!(pick(&sources, "Game (USA).xiso", &order).map(|c| c.release_id), Some(2));

        let revisions = [c(1, "Game (USA)", &["redump"]), c(2, "Game (USA) (Rev 1)", &["redump"])];
        assert_eq!(pick(&revisions, "Game (USA).xiso", &order).map(|c| c.release_id), None);
    }
}
