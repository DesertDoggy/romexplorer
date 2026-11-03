//! xlenore's PS1 and PS2 covers (<https://github.com/xlenore/psx-covers>,
//! <https://github.com/xlenore/ps2-covers>): the covers DuckStation's and PCSX2's own cover
//! downloaders use, one file per disc serial, read straight from GitHub. A 2D front
//! (`covers/default/<serial>.jpg`, stored as a box front) and a 3D box
//! (`covers/3d/<serial>.png`, stored as `box_3d`).
//!
//! No provider link stands for it: a release's own serial (`release.serial`, from the DAT)
//! names its file. Serials are per region and per edition, so a budget reissue ("the Best",
//! Platinum) or a limited edition that has a serial of its own gets its own cover, and a
//! cover is linked to its release alone, never to the rest of its release group
//! (`media::store_url_own`).
//!
//! The repositories carry no license file; nothing of theirs is bundled, each cover is
//! downloaded on the machine that shows it, as the emulators do.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sqlx::mysql::MySqlPool;

use super::super::error::DatabaseError;
use super::super::settings::MediaRules;
use super::media::{self, Ask, MediaTarget};
use crate::common::models::Platform;

/// Its name in `media.source`, `media_download` and `media_priority`.
pub const NAME: &str = "xlenore";

/// What it has, as `(media type, sub-category)`.
pub const SLOTS: [(&str, Option<&str>); 2] = [("box", Some("front")), ("box_3d", None)];

/// The media types of [`SLOTS`].
pub const TYPES: [&str; 2] = ["box", "box_3d"];

/// A serial found missing is not asked for again for this long: most of these never get a
/// cover, and a run over a whole catalogue would otherwise ask GitHub thousands of times
/// for nothing.
const MISS_TTL_SECS: u64 = 30 * 24 * 60 * 60;

/// The repository for a platform's covers.
pub fn repo(platform_id: u16) -> Option<&'static str> {
    if platform_id == Platform::PlayStation.igdb_id() {
        Some("psx-covers")
    } else if platform_id == Platform::PlayStation2.igdb_id() {
        Some("ps2-covers")
    } else {
        None
    }
}

/// The platforms it has covers for.
pub fn platform_ids() -> [u16; 2] {
    [Platform::PlayStation.igdb_id(), Platform::PlayStation2.igdb_id()]
}

/// The file for one serial and media type.
pub fn url(repo: &str, media_type: &str, serial: &str) -> String {
    let (folder, ext) = if media_type == "box_3d" { ("3d", "png") } else { ("default", "jpg") };
    format!("https://raw.githubusercontent.com/xlenore/{repo}/main/covers/{folder}/{serial}.{ext}")
}

/// One serial as the repositories name their files: upper case, `XXXX-NNNNN`. A disc's own
/// spelling (`SLUS_005.94`) and a DAT's (`SLUS-00594`) both come out the same.
pub fn normalize(serial: &str) -> Option<String> {
    let s: String = serial.trim().chars().filter(|c| *c != '.').map(|c| if c == '_' || c == ' ' { '-' } else { c.to_ascii_uppercase() }).collect();
    let (prefix, rest) = s.split_once('-')?;
    if !(2..=4).contains(&prefix.len()) || !prefix.chars().all(|c| c.is_ascii_alphabetic()) || !rest.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    Some(s)
}

/// The serials to try for a release, in order: each serial its DAT lists (Redump gives every
/// serial a disc was sold under, `SLPS-00001, SLPS-00002`), and after one with a suffix
/// (`SLUS-20062GH`) its plain form, which is what the repositories name the file.
pub fn candidates(serials: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for serial in serials.split([',', ';', '/']).filter_map(normalize) {
        let (prefix, rest) = serial.split_once('-').unwrap_or((&serial, ""));
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        let plain = format!("{prefix}-{digits}");
        for s in [serial.clone(), plain] {
            if !out.contains(&s) {
                out.push(s);
            }
        }
    }
    out
}

/// Serials found to have no file, kept on disk between runs.
pub struct Misses {
    path: PathBuf,
    seen: BTreeMap<String, u64>,
    changed: bool,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Misses {
    pub fn load(cache_dir: &Path) -> Self {
        let path = cache_dir.join("xlenore_missing.json");
        let now = now_secs();
        let seen: BTreeMap<String, u64> = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let fresh: BTreeMap<String, u64> = seen.into_iter().filter(|(_, at)| now.saturating_sub(*at) < MISS_TTL_SECS).collect();
        Self { path, seen: fresh, changed: false }
    }

    fn key(url: &str) -> String {
        url.trim_start_matches("https://raw.githubusercontent.com/xlenore/").to_string()
    }

    pub fn is_missing(&self, url: &str) -> bool {
        self.seen.contains_key(&Self::key(url))
    }

    pub fn add(&mut self, url: &str) {
        self.seen.insert(Self::key(url), now_secs());
        self.changed = true;
    }

    pub fn save(&mut self) {
        if !self.changed {
            return;
        }
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match serde_json::to_vec(&self.seen) {
            Ok(bytes) => match std::fs::write(&self.path, bytes) {
                Ok(()) => self.changed = false,
                Err(e) => log::warn!("xlenore: could not save {}: {e}", self.path.display()),
            },
            Err(e) => log::warn!("xlenore: {e}"),
        }
    }
}

/// What one release came to.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    /// Covers linked (downloaded, or already stored for another release).
    pub linked: u32,
    /// Requests made to GitHub.
    pub requests: u32,
}

/// The covers `release_id` wants from xlenore, by its serials: for each slot the settings
/// download (or `ask` names) and the release still wants (nothing from xlenore yet, nothing
/// from a source ranked above it), the first of its serials that has a file.
#[allow(clippy::too_many_arguments)]
pub async fn attach_release(
    http: &reqwest::Client,
    pool: &MySqlPool,
    media_dir: &Path,
    rules: &MediaRules,
    ask: &Ask,
    misses: &mut Misses,
    release_id: u64,
    platform_id: u16,
    serials: &str,
) -> Result<Outcome, DatabaseError> {
    let mut outcome = Outcome::default();
    let Some(repo) = repo(platform_id) else { return Ok(outcome) };
    let target = MediaTarget::Release(release_id);
    let serials = candidates(serials);
    for (media_type, sub) in SLOTS {
        if !ask.allows(rules, NAME, media_type, sub) || !media::wants_as(pool, rules, ask, NAME, media_type, sub, target, 1).await? {
            continue;
        }
        for serial in &serials {
            let url = url(repo, media_type, serial);
            if misses.is_missing(&url) {
                continue;
            }
            // Stored already (the same disc under another release): linked, not fetched.
            let known = sqlx::query("SELECT 1 FROM media WHERE source_url = ? AND deleted = FALSE LIMIT 1").bind(&url).fetch_optional(pool).await?.is_some();
            let prefetched = if known {
                None
            } else {
                outcome.requests += 1;
                let response = http.get(&url).send().await?;
                if response.status() == reqwest::StatusCode::NOT_FOUND {
                    misses.add(&url);
                    continue;
                }
                let response = response.error_for_status()?;
                let content_type = response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_string);
                Some((response.bytes().await?.to_vec(), content_type))
            };
            outcome.linked += media::store_url_own(http, pool, media_dir, NAME, Some(serial), &url, media_type, sub, &[target], prefetched).await?.1;
            break;
        }
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serials_as_the_files_are_named() {
        assert_eq!(normalize("SLUS_005.94"), Some("SLUS-00594".to_string()));
        assert_eq!(normalize(" slus-00594 "), Some("SLUS-00594".to_string()));
        assert_eq!(normalize("SLPM 65001"), Some("SLPM-65001".to_string()));
        assert_eq!(normalize("not a serial"), None);
        assert_eq!(normalize(""), None);
    }

    #[test]
    fn every_listed_serial_then_its_plain_form() {
        assert_eq!(candidates("SLPS-00001, SLPS-00002"), vec!["SLPS-00001", "SLPS-00002"]);
        assert_eq!(candidates("SLUS-20062GH"), vec!["SLUS-20062GH", "SLUS-20062"]);
        assert_eq!(candidates("SCES-02105-1, SCES-02105"), vec!["SCES-02105-1", "SCES-02105"]);
        assert!(candidates("").is_empty());
    }

    #[test]
    fn urls_and_repositories() {
        assert_eq!(url("psx-covers", "box", "SLUS-00594"), "https://raw.githubusercontent.com/xlenore/psx-covers/main/covers/default/SLUS-00594.jpg");
        assert_eq!(url("ps2-covers", "box_3d", "SLUS-20062"), "https://raw.githubusercontent.com/xlenore/ps2-covers/main/covers/3d/SLUS-20062.png");
        assert_eq!(repo(Platform::PlayStation.igdb_id()), Some("psx-covers"));
        assert_eq!(repo(Platform::PlayStation2.igdb_id()), Some("ps2-covers"));
        assert_eq!(repo(Platform::PlayStationPortable.igdb_id()), None);
    }
}
