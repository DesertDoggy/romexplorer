//! Runtime fetch of the six reference DAT sources into `<dats_dir>/<source>/` -- the in-app
//! port of `scripts/fetch_*.sh` (which remain the dev-time way to populate `samples/dats/`).
//! Each source's fetch shape is exactly what the corresponding script worked out:
//!
//! | source | shape |
//! |---|---|
//! | retrobios | GitHub commits API for `database.json`'s latest commit -> raw file, saved as `database_<commit time>.json` |
//! | retroachievements | GitHub contents listing of one directory -> every `.dat` (size-checked idempotency) |
//! | mame-redump | same, for three directories |
//! | redump | scrape redump.info/downloads for per-system `/datfile/<system>` and `/bios/<system>` links (HEAD for the dated filename, then GET) |
//! | no-intro | datomatic's daily-pack flow (accept the terms if asked -> form -> POST -> job page -> POST the non-honeypot button) |
//! | tosec | scrape tosecdev.org/downloads for the newest dated release, then its "Complete" pack link |
//!
//! Idempotent: a file already present under its current name (and, where the listing gives
//! one, at the listed size) is left alone unless `force`. `limit` caps how many files a
//! multi-file source fetches -- what the tests use to keep bandwidth small.

use std::path::{Path, PathBuf};
use std::time::Duration;

use lazy_static::lazy_static;
use regex::Regex;
use reqwest::Client;
use serde::Deserialize;

use super::error::DatabaseError;
use crate::common::models::Platform;


#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    Retrobios,
    Redump,
    NoIntro,
    RetroAchievements,
    MameRedump,
    Tosec,
    LostLevel,
    Hacks,
    Homebrew,
    Etc,
}

impl Source {
    pub const ALL: [Source; 10] =
        [Source::Retrobios, Source::RetroAchievements, Source::MameRedump, Source::Redump, Source::NoIntro, Source::Tosec, Source::LostLevel, Source::Hacks, Source::Homebrew, Source::Etc];

    pub fn name(self) -> &'static str {
        match self {
            Self::Retrobios => "retrobios",
            Self::Redump => "redump",
            Self::NoIntro => "no-intro",
            Self::RetroAchievements => "retroachievements",
            Self::MameRedump => "mame-redump",
            Self::Tosec => "tosec",
            Self::LostLevel => "lost-level-archive",
            Self::Hacks => "hacks",
            Self::Homebrew => "homebrew",
            Self::Etc => "etc",
        }
    }

    /// Lists games as they were sold: an entry here that is not marked as a hack is the
    /// original, whatever another DAT filed under the same name.
    pub fn lists_originals(self) -> bool {
        matches!(self, Self::Redump | Self::NoIntro | Self::MameRedump | Self::Tosec)
    }

    /// Nothing to download: the source is whatever DATs are in its folder.
    pub fn is_manual(self) -> bool {
        matches!(self, Self::Hacks | Self::Homebrew | Self::Etc)
    }

    /// What a DAT sync with no source named covers: every source but the manual ones. Those
    /// hold what the user put there -- tens of thousands of entries that take minutes to
    /// import and change only when the user changes them -- so they run when asked for by
    /// name, not along with a routine sync of the reference DATs.
    pub fn all_synced() -> Vec<Self> {
        Self::ALL.into_iter().filter(|s| !s.is_manual()).collect()
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }

    /// Rough download size class, for scheduling: the whole-catalog packs are ~100-150MB.
    pub fn is_large(self) -> bool {
        matches!(self, Self::NoIntro | Self::Tosec)
    }
}

#[derive(Debug, Clone)]
pub struct DownloadOptions {
    pub force: bool,
    /// Max files to fetch for multi-file sources (`None` = all).
    pub limit: Option<usize>,
    pub user_agent: String,
    /// Courtesy delay between consecutive requests to the same site.
    pub courtesy_delay: Duration,
    pub timeout: Duration,
    /// Where byte progress goes while a file downloads (the `dat_sync` job's row).
    pub progress: Option<std::sync::Arc<super::jobs::Progress>>,
    /// The platforms to fetch (`dat_sync_platforms`); `None` = every platform.
    pub platforms: Option<std::sync::Arc<std::collections::HashSet<u16>>>,
    /// The DATs without a platform of their own to fetch, by [`DAT_EXTRAS`] key; `None` =
    /// every one.
    pub extras: Option<std::sync::Arc<std::collections::HashSet<String>>>,
}

impl DownloadOptions {
    /// Whether a DAT named `name` (a file, a pack member or a header name) is for one of
    /// [`Self::platforms`]. A name no platform is known for is always wanted: nothing says
    /// it isn't for a platform you have.
    pub fn wants(&self, name: &str) -> bool {
        wants_dat(self.platforms.as_deref(), self.extras.as_deref(), name)
    }
}

/// The DATs a DAT sync can leave out that are not a platform's games, each with its own
/// checkbox next to the platforms: key and label. A DAT is one of these by
/// [`dat_extra`]; a DAT that is not goes by its platform.
pub const DAT_EXTRAS: [(&str, &str); 7] = [
    ("audio", "audio CDs and music"),
    ("video", "video discs"),
    ("image", "photo CDs and images"),
    ("document", "manuals, magazines and scans (TOSEC-PIX)"),
    ("source", "source code"),
    ("theme", "avatars, themes and wallpapers"),
    ("other", "other DATs, platform unknown (Palm OS, ...)"),
];

/// Which of [`DAT_EXTRAS`] a DAT (a file name or a pack member's path) is, or `None` for a
/// platform's own DAT. Source code and scans count here even when they name a platform.
pub fn dat_extra(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    // A whole-catalogue pack (No-Intro, TOSEC) is every kind at once: its members decide.
    if lower.contains("love pack") || lower.contains("dat pack") {
        return None;
    }
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let base = base.strip_suffix(".zip").or_else(|| base.strip_suffix(".dat")).or_else(|| base.strip_suffix(".xml")).unwrap_or(base);
    match super::platform_map::material_from_dat_name(base, lower.contains("tosec")) {
        // A console's own decorations (PS3 avatars and themes, Wii wallpapers); DLC and
        // updates stay with their platform's games.
        Some("image") if lower.contains("wallpaper") => return Some("theme"),
        Some(m @ ("audio" | "video" | "image" | "document" | "source" | "theme")) => return Some(m),
        _ => {}
    }
    if lower.contains("tosec-pix") {
        return Some("document");
    }
    (super::platform_map::platform_from_dat_name(base) == Platform::Unknown).then_some("other")
}

/// [`DownloadOptions::wants`], for a caller that has only the sets.
pub fn wants_dat(platforms: Option<&std::collections::HashSet<u16>>, extras: Option<&std::collections::HashSet<String>>, name: &str) -> bool {
    if platforms.is_none() && extras.is_none() {
        return true;
    }
    if let Some(extra) = dat_extra(name) {
        return extras.is_none_or(|e| e.contains(extra));
    }
    let Some(set) = platforms else { return true };
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let base = base.strip_suffix(".zip").or_else(|| base.strip_suffix(".dat")).or_else(|| base.strip_suffix(".xml")).unwrap_or(base);
    match super::platform_map::platform_from_dat_name(base) {
        // A pack.
        Platform::Unknown => true,
        p => set.contains(&p.igdb_id()),
    }
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self { force: false, limit: None, user_agent: "romexplorer/0.1 (+https://github.com/DesertDoggy/romexplorer)".to_string(), courtesy_delay: Duration::from_millis(300), timeout: Duration::from_secs(600), progress: None, platforms: None, extras: None }
    }
}

#[derive(Debug, Clone)]
pub struct Downloaded {
    pub path: PathBuf,
    /// False when the file was already present and left alone.
    pub fresh: bool,
    pub bytes: u64,
}

pub fn build_client(options: &DownloadOptions) -> Result<Client, DatabaseError> {
    Client::builder().user_agent(&options.user_agent).cookie_store(true).timeout(options.timeout).build().map_err(DatabaseError::from)
}

/// Fetches `source`'s current data into `<dats_dir>/<source name>/`.
pub async fn fetch(client: &Client, source: Source, dats_dir: &Path, options: &DownloadOptions) -> Result<Vec<Downloaded>, DatabaseError> {
    let out_dir = dats_dir.join(source.name());
    std::fs::create_dir_all(&out_dir)?;
    match source {
        Source::Retrobios => fetch_retrobios(client, &out_dir, options).await,
        Source::RetroAchievements => fetch_github_dir(client, "UltraGodAzgorath/UGA_RA_DATs", &["DATs/RetroAchievements (No Subfolders)"], &[".dat"], &out_dir, options).await,
        Source::MameRedump => fetch_github_dir(client, "MetalSlug/MAMERedump", &["MAME Redump", "MAME Redump/MAME", "MAME Redump/Tosec"], &[".dat"], &out_dir, options).await,
        Source::Redump => fetch_redump(client, &out_dir, options).await,
        Source::NoIntro => fetch_no_intro(client, &out_dir, options).await,
        Source::Tosec => fetch_tosec(client, &out_dir, options).await,
        // Its DATs are Logiqx XML under an `.xml` name (a filter for `.dat` alone found
        // none of the 72 and reported an empty, successful sync). The extras -- manuals,
        // soundtracks -- are a folder of their own; "Legacy DATs" is what it has retired.
        Source::LostLevel => fetch_github_dir(client, "televandalist/lost-level-archive", &["DATs", "Extra/Lost Level Archive Extras"], &[".xml", ".dat"], &out_dir, options).await,
        // What is there is what there is; the import takes the ones it has not seen.
        Source::Hacks | Source::Homebrew | Source::Etc => Ok(latest_versions(&local_files(dats_dir, source))
            .into_iter()
            .map(|path| Downloaded { bytes: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0), path, fresh: false })
            .collect()),
    }
}

/// The files currently on disk for `source` (any run's), newest name last.
pub fn local_files(dats_dir: &Path, source: Source) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let dir = dats_dir.join(source.name());
    let mut stack = vec![dir];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()).map(|e| matches!(e.to_ascii_lowercase().as_str(), "dat" | "xml" | "zip" | "json")).unwrap_or(false) {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// `files` without the ones a newer copy of the same DAT supersedes -- what an offline import
/// should read, since each fetch saves under the publisher's dated name and leaves the older
/// one where it was. Two names are the same DAT when they match once their version stamps are
/// taken out: dates (`2026-10-01`, `2026-10-01 12-34-56`, `20261001-123456`, retrobios'
/// `database_<14 digits>`) and parenthesised counts (redump's and TOSEC's `(12345)`). The
/// newest stamp wins; a name with no stamp at all is its own DAT and always kept.
pub fn latest_versions(files: &[PathBuf]) -> Vec<PathBuf> {
    let mut best: std::collections::HashMap<(Option<PathBuf>, String), (String, &PathBuf)> = std::collections::HashMap::new();
    for path in files {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let (key, stamp) = version_key(&name);
        let slot = (path.parent().map(Path::to_path_buf), key);
        match best.get(&slot) {
            Some((kept, _)) if *kept >= stamp => {}
            _ => {
                best.insert(slot, (stamp, path));
            }
        }
    }
    let mut out: Vec<PathBuf> = best.into_values().map(|(_, p)| p.clone()).collect();
    out.sort();
    out
}

/// `name` with its version stamps removed, and the stamps' digits (comparable as text, since
/// every stamp shape is big-endian).
fn version_key(name: &str) -> (String, String) {
    lazy_static! {
        static ref RE_STAMP: Regex = Regex::new(r"\d{4}-\d{2}-\d{2}(?:[ T_]\d{2}[-:]\d{2}[-:]\d{2})?|\d{8}(?:[-_]?\d{6})?").unwrap();
        static ref RE_COUNT: Regex = Regex::new(r"\(\d+\)").unwrap();
        static ref RE_SPACE: Regex = Regex::new(r"\s+").unwrap();
    }
    let stamp: String = RE_STAMP.find_iter(name).flat_map(|m| m.as_str().chars().filter(char::is_ascii_digit)).collect();
    let key = RE_STAMP.replace_all(name, "");
    let key = RE_COUNT.replace_all(&key, "");
    (RE_SPACE.replace_all(key.trim(), " ").into_owned(), stamp)
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn fetch_err(context: &str, err: impl std::fmt::Display) -> DatabaseError {
    DatabaseError::Fetch(format!("{context}: {err}"))
}

/// Streams `url` to `dest` via a `.part` temp file, returning the byte count.
async fn download_to(client: &Client, url: &str, dest: &Path, referer: Option<&str>, options: &DownloadOptions) -> Result<u64, DatabaseError> {
    let mut request = client.get(url);
    if let Some(referer) = referer {
        request = request.header(reqwest::header::REFERER, referer);
    }
    let response = request.send().await.map_err(|e| fetch_err(url, e))?.error_for_status().map_err(|e| fetch_err(url, e))?;
    write_response(response, dest, options).await
}

async fn write_response(mut response: reqwest::Response, dest: &Path, options: &DownloadOptions) -> Result<u64, DatabaseError> {
    use tokio::io::AsyncWriteExt;
    let part = dest.with_extension(format!("{}.part", dest.extension().and_then(|e| e.to_str()).unwrap_or("bin")));
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    // Removed again however the download ends: an error, a dropped connection, or the job
    // being cancelled (which drops this future part way).
    struct PartGuard(Option<std::path::PathBuf>);
    impl Drop for PartGuard {
        fn drop(&mut self) {
            if let Some(path) = self.0.take() {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    let mut guard = PartGuard(Some(part.clone()));
    let mut file = tokio::fs::File::create(&part).await?;
    let expected = response.content_length();
    let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut total = 0u64;
    while let Some(chunk) = response.chunk().await.map_err(|e| fetch_err("stream", e))? {
        file.write_all(&chunk).await?;
        total += chunk.len() as u64;
        if let Some(progress) = &options.progress {
            let of = expected.map(|e| format!(" of {}", super::jobs::human_bytes(e))).unwrap_or_default();
            progress.report(total, expected, &format!("downloading {name}: {}{of}", super::jobs::human_bytes(total))).await;
        }
    }
    file.flush().await?;
    drop(file);
    if let Some(expected) = expected.filter(|e| *e != total) {
        return Err(fetch_err(&name, format!("the download ended at {total} of {expected} bytes")));
    }
    tokio::fs::rename(&part, dest).await?;
    guard.0 = None;
    Ok(total)
}

fn content_disposition_filename(headers: &reqwest::header::HeaderMap) -> Option<String> {
    lazy_static! {
        static ref RE_FILENAME: Regex = Regex::new(r#"(?i)filename\*?=(?:UTF-8'')?"?([^";]+)"?"#).unwrap();
    }
    let value = headers.get(reqwest::header::CONTENT_DISPOSITION)?.to_str().ok()?;
    let name = RE_FILENAME.captures(value)?.get(1)?.as_str().trim().to_string();
    Some(percent_decode(&name))
}

pub(crate) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() + 0 && i + 2 <= bytes.len() - 1 {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("zz"), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Filesystem-safe version of a server-provided filename: no path separators, none of the
/// characters Windows refuses in a name (`<>:"|?*`, control characters), no trailing dots or
/// spaces (Windows drops them), and never `.` / `..` or empty -- a name that would point
/// anywhere but a file in the download folder.
fn safe_file_name(name: &str) -> String {
    let cleaned: String = name.chars().map(|c| if matches!(c, '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*') || c.is_control() { '_' } else { c }).collect();
    let cleaned = cleaned.trim().trim_end_matches(['.', ' ']).to_string();
    if cleaned.is_empty() || cleaned.chars().all(|c| c == '.') {
        return "_".repeat(name.len().clamp(1, 2));
    }
    cleaned
}

fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&").replace("&quot;", "\"").replace("&#39;", "'").replace("&lt;", "<").replace("&gt;", ">")
}

// ---------------------------------------------------------------------------
// GitHub-hosted sources
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GithubContent {
    name: String,
    size: u64,
    #[serde(rename = "type")]
    kind: String,
    download_url: Option<String>,
}

async fn github_list(client: &Client, repo: &str, dir: &str) -> Result<Vec<GithubContent>, DatabaseError> {
    let api_path: String = dir.split('/').map(|seg| seg.replace(' ', "%20")).collect::<Vec<_>>().join("/");
    let url = format!("https://api.github.com/repos/{repo}/contents/{api_path}");
    let response = client.get(&url).header(reqwest::header::ACCEPT, "application/vnd.github+json").send().await.map_err(|e| fetch_err(&url, e))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(DatabaseError::Fetch(format!("{url}: HTTP {status} {}", body.chars().take(200).collect::<String>())));
    }
    response.json::<Vec<GithubContent>>().await.map_err(|e| fetch_err(&url, e))
}

async fn fetch_github_dir(client: &Client, repo: &str, dirs: &[&str], extensions: &[&str], out_dir: &Path, options: &DownloadOptions) -> Result<Vec<Downloaded>, DatabaseError> {
    let mut out = Vec::new();
    let mut fetched = 0usize;
    for (index, dir) in dirs.iter().enumerate() {
        // Subdirectories mirror upstream's layout (mame-redump's MAME/ and Tosec/).
        let sub_dir = if index == 0 { out_dir.to_path_buf() } else { out_dir.join(dir.rsplit('/').next().unwrap_or(dir)) };
        std::fs::create_dir_all(&sub_dir)?;
        let listing = github_list(client, repo, dir).await?;
        for entry in listing.into_iter().filter(|e| e.kind == "file" && extensions.iter().any(|x| e.name.to_ascii_lowercase().ends_with(x))) {
            if let Some(limit) = options.limit {
                if fetched >= limit {
                    return Ok(out);
                }
            }
            if !options.wants(&entry.name) {
                continue;
            }
            let dest = sub_dir.join(safe_file_name(&entry.name));
            let existing = std::fs::metadata(&dest).map(|m| m.len()).ok();
            if !options.force && existing == Some(entry.size) {
                out.push(Downloaded { path: dest, fresh: false, bytes: entry.size });
                fetched += 1;
                continue;
            }
            let Some(url) = entry.download_url else { continue };
            let bytes = download_to(client, &url, &dest, None, options).await?;
            out.push(Downloaded { path: dest, fresh: true, bytes });
            fetched += 1;
            tokio::time::sleep(options.courtesy_delay).await;
        }
    }
    Ok(out)
}

#[derive(Debug, Deserialize)]
struct GithubCommit {
    sha: String,
    commit: GithubCommitInner,
}

#[derive(Debug, Deserialize)]
struct GithubCommitInner {
    committer: GithubCommitter,
}

#[derive(Debug, Deserialize)]
struct GithubCommitter {
    date: String,
}

async fn fetch_retrobios(client: &Client, out_dir: &Path, options: &DownloadOptions) -> Result<Vec<Downloaded>, DatabaseError> {
    const REPO: &str = "Abdess/retrobios";
    const FILE_PATH: &str = "database.json";
    let url = format!("https://api.github.com/repos/{REPO}/commits?path={FILE_PATH}&per_page=1");
    let commits: Vec<GithubCommit> = client.get(&url).header(reqwest::header::ACCEPT, "application/vnd.github+json").send().await.map_err(|e| fetch_err(&url, e))?.error_for_status().map_err(|e| fetch_err(&url, e))?.json().await.map_err(|e| fetch_err(&url, e))?;
    let Some(commit) = commits.first() else { return Err(DatabaseError::Fetch("retrobios: no commits returned".to_string())) };
    let stamp: String = commit.commit.committer.date.chars().filter(|c| c.is_ascii_digit()).collect();
    let dest = out_dir.join(format!("database_{stamp}.json"));
    if !options.force && dest.is_file() {
        let bytes = std::fs::metadata(&dest)?.len();
        return Ok(vec![Downloaded { path: dest, fresh: false, bytes }]);
    }
    let raw_url = format!("https://raw.githubusercontent.com/{REPO}/{}/{FILE_PATH}", commit.sha);
    let bytes = download_to(client, &raw_url, &dest, None, options).await?;
    Ok(vec![Downloaded { path: dest, fresh: true, bytes }])
}

// ---------------------------------------------------------------------------
// redump
// ---------------------------------------------------------------------------

lazy_static! {
    // One link per system and kind: `/datfile/PSX` for the game DAT, `/bios/PSX` for the
    // BIOS DAT, absolute or relative. The older `/datfile/PSX/serial,version` form still
    // matches, minus its suffix.
    static ref RE_REDUMP_LINK: Regex = Regex::new(r#"href="(?:https?://(?:www\.)?redump\.info)?/(datfile|bios)/([A-Za-z0-9_.-]+)(?:/[^"]*)?""#).unwrap();
}

async fn fetch_redump(client: &Client, out_dir: &Path, options: &DownloadOptions) -> Result<Vec<Downloaded>, DatabaseError> {
    const DOWNLOADS_URL: &str = "https://redump.info/downloads";
    let page = client.get(DOWNLOADS_URL).send().await.map_err(|e| fetch_err(DOWNLOADS_URL, e))?.error_for_status().map_err(|e| fetch_err(DOWNLOADS_URL, e))?.text().await.map_err(|e| fetch_err(DOWNLOADS_URL, e))?;

    let links = redump_links(&page);
    if links.is_empty() {
        return Err(DatabaseError::Fetch("redump: downloads page had no datfile links (page layout changed?)".to_string()));
    }

    let mut out = Vec::new();
    let mut fetched = 0usize;
    for (kind, code) in links {
        if options.limit.map(|l| fetched >= l).unwrap_or(false) {
            break;
        }
        let url = format!("https://redump.info/{kind}/{code}");
        // The dated filename is only in the headers, so ask for them first and skip the
        // download when that file is already here.
        let head = client.head(&url).header(reqwest::header::REFERER, DOWNLOADS_URL).send().await.map_err(|e| fetch_err(&url, e))?;
        let filename = match content_disposition_filename(head.headers()) {
            Some(name) => name,
            None => {
                // Some servers answer HEAD without the header; a GET's headers have it.
                let response = client.get(&url).header(reqwest::header::REFERER, DOWNLOADS_URL).send().await.map_err(|e| fetch_err(&url, e))?.error_for_status().map_err(|e| fetch_err(&url, e))?;
                let Some(name) = content_disposition_filename(response.headers()) else {
                    log::warn!("redump: {kind}/{code}: no filename in response headers, skipped");
                    continue;
                };
                if !options.wants(&name) {
                    continue;
                }
                let dest = out_dir.join(safe_file_name(&name));
                if !options.force && dest.is_file() {
                    out.push(Downloaded { path: dest.clone(), fresh: false, bytes: std::fs::metadata(&dest)?.len() });
                } else {
                    let bytes = write_response(response, &dest, options).await?;
                    out.push(Downloaded { path: dest, fresh: true, bytes });
                }
                fetched += 1;
                tokio::time::sleep(options.courtesy_delay).await;
                continue;
            }
        };
        if !options.wants(&filename) {
            continue;
        }
        let dest = out_dir.join(safe_file_name(&filename));
        if !options.force && dest.is_file() {
            out.push(Downloaded { path: dest.clone(), fresh: false, bytes: std::fs::metadata(&dest)?.len() });
        } else {
            let bytes = download_to(client, &url, &dest, Some(DOWNLOADS_URL), options).await?;
            out.push(Downloaded { path: dest, fresh: true, bytes });
        }
        fetched += 1;
        tokio::time::sleep(options.courtesy_delay).await;
    }
    Ok(out)
}

/// The `(kind, system)` pairs linked from redump's downloads page, game DATs first, each once.
fn redump_links(page: &str) -> Vec<(String, String)> {
    let mut links: Vec<(String, String)> = RE_REDUMP_LINK.captures_iter(page).map(|c| (c[1].to_string(), c[2].to_string())).collect();
    links.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    links.dedup();
    links
}

// ---------------------------------------------------------------------------
// no-intro (datomatic daily pack)
// ---------------------------------------------------------------------------

async fn fetch_no_intro(client: &Client, out_dir: &Path, options: &DownloadOptions) -> Result<Vec<Downloaded>, DatabaseError> {
    lazy_static! {
        static ref RE_SUBMIT: Regex = Regex::new(r#"name="(daily_dl_[^"]+)""#).unwrap();
        static ref RE_BUTTON_LINE: Regex = Regex::new(r#"(?m)^.*type="submit".*$"#).unwrap();
        static ref RE_BUTTON_NAME: Regex = Regex::new(r#"name="([a-f0-9]+)""#).unwrap();
    }
    const BASE: &str = "https://datomatic.no-intro.org";
    let form_url = format!("{BASE}/index.php?page=download&op=daily&s=64");
    // The POST steps 302 to the job page; that redirect must be read, not followed.
    let flow = Client::builder().user_agent(&options.user_agent).cookie_store(true).redirect(reqwest::redirect::Policy::none()).timeout(options.timeout).build()?;

    // The form, with a pause and another go when it isn't there: datomatic is slow at
    // times, and the page a browser waits for can come back half-made or as a notice.
    let mut submit_name = None;
    let mut form_html = String::new();
    for attempt in 0..3u32 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(10 * attempt as u64)).await;
        }
        form_html = no_intro_page(&flow, &form_url).await?;
        // A new session first gets the terms page: one form posting its csrf token and an
        // "I accept" button back to the same URL. Accepting sets the session cookie a
        // browser keeps; then the page asked for is served.
        if let Some(token) = no_intro_terms_token(&form_html) {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let accepted = flow.post(&form_url).header(reqwest::header::REFERER, &form_url).form(&[("csrf_token", token.as_str()), ("terms_accept", "I accept")]).send().await.map_err(|e| fetch_err(&form_url, e))?;
            // Read to the end, so the session the accept sets is in place before the next request.
            let _ = accepted.text().await;
            tokio::time::sleep(Duration::from_secs(2)).await;
            form_html = no_intro_page(&flow, &form_url).await?;
        }
        // Was `daily_dl_<hex>`; since 2026-11 it is `download_request`.
        submit_name = RE_SUBMIT.captures(&form_html).map(|c| c[1].to_string()).or_else(|| no_intro_request_button(&no_intro_form_block(&form_html, "daily")));
        // A notice that says why (maintenance, throttled, banned) won't change in seconds.
        if submit_name.is_some() || !no_intro_form_missing(&form_html).contains("not found") {
            break;
        }
        log::info!("no-intro: no daily form on the page yet ({}); trying again", no_intro_page_gist(&form_html));
    }
    let Some(submit_name) = submit_name else {
        // Kept for a look: what datomatic served instead.
        let _ = std::fs::write(out_dir.join("no-intro-last-page.html"), &form_html);
        let why = no_intro_form_missing(&form_html);
        return Err(DatabaseError::Fetch(if why.contains("not found") { format!("{why}; the page said: {} (saved as no-intro-last-page.html in the DAT folder)", no_intro_page_gist(&form_html)) } else { why }));
    };

    let mut form = reqwest::multipart::Form::new().text("dat_type", "dat");
    // The form's hidden fields (its csrf token, since 2026-11) go back with it.
    for (name, value) in no_intro_hidden_inputs(&no_intro_form_block(&form_html, "daily")) {
        form = form.text(name, value);
    }
    for field in ["set[1]", "set[2]", "set[3]", "set[4]", "set[5]", "set_redump_custom", "set_redump_bios", "include_standard", "include_numbered", "include_aftermarket"] {
        form = form.text(field, "Ok");
    }
    form = form.text(submit_name, "Request");
    let prepare = flow.post(&form_url).header(reqwest::header::REFERER, &form_url).multipart(form).send().await.map_err(|e| fetch_err(&form_url, e))?;
    // The job page: behind a redirect, or (since 2026-11) the answer itself.
    let (manager_url, manager_html) = match prepare.headers().get(reqwest::header::LOCATION).and_then(|v| v.to_str().ok()).map(|s| s.to_string()) {
        Some(path) => {
            let url = if path.starts_with("http") { path } else { format!("{BASE}/{}", path.trim_start_matches('/')) };
            let html = flow.get(&url).header(reqwest::header::REFERER, &form_url).send().await.map_err(|e| fetch_err(&url, e))?.text().await.map_err(|e| fetch_err(&url, e))?;
            (url, html)
        }
        None => {
            let html = prepare.text().await.map_err(|e| fetch_err(&form_url, e))?;
            if no_intro_visible_submit(&html, "download").is_none() {
                let _ = std::fs::write(out_dir.join("no-intro-last-page.html"), &html);
                return Err(DatabaseError::Fetch(format!("no-intro: the request didn't lead to a download page; the page said: {} (saved as no-intro-last-page.html in the DAT folder)", no_intro_page_gist(&html))));
            }
            (form_url.clone(), html)
        }
    };
    // Two submit buttons share the "Download!!" value: a display:none honeypot and the real one.
    // Since 2026-11 there is one, `download_file`, beside a csrf token.
    let download_name = no_intro_visible_submit(&manager_html, "download")
        .or_else(|| RE_BUTTON_LINE.find_iter(&manager_html).map(|m| m.as_str()).filter(|line| !line.contains("display:none")).find_map(|line| RE_BUTTON_NAME.captures(line).map(|c| c[1].to_string())))
        .ok_or_else(|| DatabaseError::Fetch("no-intro: job page's download button not found".to_string()))?;

    let mut form = reqwest::multipart::Form::new().text(download_name, "Download!!");
    for (name, value) in no_intro_hidden_inputs(&manager_html) {
        form = form.text(name, value);
    }
    let response = flow.post(&manager_url).header(reqwest::header::REFERER, &manager_url).multipart(form).send().await.map_err(|e| fetch_err(&manager_url, e))?;
    let filename = content_disposition_filename(response.headers()).ok_or_else(|| DatabaseError::Fetch("no-intro: the download response had no filename".to_string()))?;
    let dest = out_dir.join(safe_file_name(&filename));
    if !options.force && dest.is_file() {
        return Ok(vec![Downloaded { path: dest.clone(), fresh: false, bytes: std::fs::metadata(&dest)?.len() }]);
    }
    let bytes = write_response(response, &dest, options).await?;
    let _ = client;
    Ok(vec![Downloaded { path: dest, fresh: true, bytes }])
}

/// A datomatic page by GET, its redirects followed by hand: the flow's client reads the
/// POST steps' redirects rather than following them, and a GET that redirects (to the
/// terms page, say) would otherwise come back as an empty body.
async fn no_intro_page(flow: &Client, url: &str) -> Result<String, DatabaseError> {
    let mut url = url.to_string();
    for _ in 0..5 {
        let response = flow.get(&url).send().await.map_err(|e| fetch_err(&url, e))?;
        if response.status().is_redirection() {
            if let Some(next) = response.headers().get(reqwest::header::LOCATION).and_then(|v| v.to_str().ok()) {
                url = response.url().join(next).map(|u| u.to_string()).unwrap_or_else(|_| next.to_string());
                continue;
            }
        }
        return response.text().await.map_err(|e| fetch_err(&url, e));
    }
    Err(DatabaseError::Fetch(format!("no-intro: too many redirects from {url}")))
}

/// The inside of the `<form name="...">` on a page, or the whole page when there is none.
fn no_intro_form_block(html: &str, name: &str) -> String {
    let re = Regex::new(&format!(r#"(?is)<form[^>]*name="{}"[^>]*>(.*?)</form>"#, regex::escape(name))).unwrap();
    re.captures(html).map(|c| c[1].to_string()).unwrap_or_else(|| html.to_string())
}

/// Every `<input>` tag in `html`, as (type, name, value, hidden by style).
fn no_intro_inputs(html: &str) -> Vec<(String, String, String, bool)> {
    lazy_static! {
        static ref RE_INPUT: Regex = Regex::new(r"(?is)<input\b[^>]*>").unwrap();
        static ref RE_ATTR: Regex = Regex::new(r#"(?i)\b(type|name|value|style)\s*=\s*"([^"]*)""#).unwrap();
    }
    RE_INPUT
        .find_iter(html)
        .map(|tag| {
            let (mut kind, mut name, mut value, mut style) = (String::new(), String::new(), String::new(), String::new());
            for c in RE_ATTR.captures_iter(tag.as_str()) {
                let v = c[2].to_string();
                match c[1].to_ascii_lowercase().as_str() {
                    "type" => kind = v.to_ascii_lowercase(),
                    "name" => name = v,
                    "value" => value = v,
                    _ => style = v.to_ascii_lowercase().replace(' ', ""),
                }
            }
            (kind, name, value, style.contains("display:none"))
        })
        .collect()
}

fn no_intro_hidden_inputs(html: &str) -> Vec<(String, String)> {
    no_intro_inputs(html).into_iter().filter(|(k, n, _, _)| k == "hidden" && !n.is_empty()).map(|(_, n, v, _)| (n, v)).collect()
}

/// The daily form's "Request" button, whatever it is called.
fn no_intro_request_button(form: &str) -> Option<String> {
    no_intro_visible_submit(form, "request")
}

/// The name of the first submit button shown (not a display:none honeypot) whose value has `word`.
fn no_intro_visible_submit(html: &str, word: &str) -> Option<String> {
    no_intro_inputs(html).into_iter().find(|(k, n, v, hidden)| k == "submit" && !n.is_empty() && !hidden && v.to_ascii_lowercase().contains(word)).map(|(_, n, _, _)| n)
}

/// What a page says, in a line: its title and the start of its text, for an error.
fn no_intro_page_gist(html: &str) -> String {
    lazy_static! {
        static ref RE_TITLE: Regex = Regex::new(r"(?is)<title[^>]*>(.*?)</title>").unwrap();
        static ref RE_DROP: Regex = Regex::new(r"(?is)<(script|style|head)[^>]*>.*?</(script|style|head)>").unwrap();
        static ref RE_TAG: Regex = Regex::new(r"(?s)<[^>]*>").unwrap();
    }
    if html.trim().is_empty() {
        return "an empty page".to_string();
    }
    let title = RE_TITLE.captures(html).map(|c| c[1].split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default();
    let text = RE_TAG.replace_all(&RE_DROP.replace_all(html, " "), " ").split_whitespace().collect::<Vec<_>>().join(" ");
    let text: String = text.chars().take(160).collect();
    if title.is_empty() { format!("\"{text}\"") } else { format!("[{title}] \"{text}\"") }
}

/// The csrf token of datomatic's terms page, when `html` is that page.
fn no_intro_terms_token(html: &str) -> Option<String> {
    lazy_static! {
        static ref RE_TOKEN: Regex = Regex::new(r#"name="csrf_token"\s+value="([^"]+)""#).unwrap();
        // The same input with its attributes the other way round.
        static ref RE_TOKEN_REV: Regex = Regex::new(r#"value="([^"]+)"\s+name="csrf_token""#).unwrap();
    }
    if !html.contains(r#"name="terms_accept""#) {
        return None;
    }
    RE_TOKEN.captures(html).or_else(|| RE_TOKEN_REV.captures(html)).map(|c| c[1].to_string())
}

/// Why datomatic's daily form wasn't there: the site's own notice when it gave one.
fn no_intro_form_missing(html: &str) -> String {
    let lower = html.to_lowercase();
    if lower.contains("under maintenance") {
        "no-intro: datomatic is down for maintenance; try again later".to_string()
    } else if lower.contains("temporarily throttled") {
        "no-intro: datomatic is throttling requests right now; try again later".to_string()
    } else if lower.contains(r#"name="terms_accept""#) {
        "no-intro: datomatic kept asking to accept its terms and conditions".to_string()
    } else if lower.contains("banned") {
        "no-intro: datomatic has banned this IP address (it lifts by itself after 3 days; syncing again meanwhile can extend it)".to_string()
    } else {
        "no-intro: daily request form's submit button not found (page layout changed?)".to_string()
    }
}

// ---------------------------------------------------------------------------
// tosec
// ---------------------------------------------------------------------------

async fn fetch_tosec(client: &Client, out_dir: &Path, options: &DownloadOptions) -> Result<Vec<Downloaded>, DatabaseError> {
    lazy_static! {
        static ref RE_CATEGORY: Regex = Regex::new(r#"href="(/downloads/category/(\d+)-(\d{4}-\d{2}-\d{2}))""#).unwrap();
        static ref RE_COMPLETE: Regex = Regex::new(r#"(?i)href="([^"]*\?download=\d+:[^"]*complete[^"]*)""#).unwrap();
    }
    const BASE: &str = "https://www.tosecdev.org";
    let downloads_url = format!("{BASE}/downloads");
    let html = client.get(&downloads_url).send().await.map_err(|e| fetch_err(&downloads_url, e))?.error_for_status().map_err(|e| fetch_err(&downloads_url, e))?.text().await.map_err(|e| fetch_err(&downloads_url, e))?;
    // Highest date wins (ids aren't issued in date order).
    let latest = RE_CATEGORY.captures_iter(&html).map(|c| (c[3].to_string(), c[1].to_string())).max().map(|(_, path)| path).ok_or_else(|| DatabaseError::Fetch("tosec: no dated release category found".to_string()))?;
    let category_url = format!("{BASE}{latest}");
    let category_html = client.get(&category_url).send().await.map_err(|e| fetch_err(&category_url, e))?.text().await.map_err(|e| fetch_err(&category_url, e))?;
    let href = RE_COMPLETE.captures(&category_html).map(|c| html_unescape(&c[1])).ok_or_else(|| DatabaseError::Fetch("tosec: \"Complete\" pack link not found".to_string()))?;
    let download_url = if href.starts_with("http") {
        href
    } else if href.starts_with('/') {
        format!("{BASE}{href}")
    } else {
        format!("{category_url}{href}")
    };
    let response = client.get(&download_url).send().await.map_err(|e| fetch_err(&download_url, e))?.error_for_status().map_err(|e| fetch_err(&download_url, e))?;
    let filename = content_disposition_filename(response.headers()).ok_or_else(|| DatabaseError::Fetch("tosec: the download response had no filename".to_string()))?;
    let dest = out_dir.join(safe_file_name(&filename));
    if !options.force && dest.is_file() {
        return Ok(vec![Downloaded { path: dest.clone(), fresh: false, bytes: std::fs::metadata(&dest)?.len() }]);
    }
    let bytes = write_response(response, &dest, options).await?;
    Ok(vec![Downloaded { path: dest, fresh: true, bytes }])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_file_name_stays_one_file_in_the_folder() {
        assert_eq!(safe_file_name("Sega - Mega Drive (20240101).zip"), "Sega - Mega Drive (20240101).zip");
        assert_eq!(safe_file_name("a/b\\c"), "a_b_c");
        assert_eq!(safe_file_name("Name: Sub <x>|\"y\"?*.dat"), "Name_ Sub _x___y___.dat");
        assert_eq!(safe_file_name(".."), "__");
        assert_eq!(safe_file_name("."), "_");
        assert_eq!(safe_file_name(""), "_");
        assert_eq!(safe_file_name("pack.zip. "), "pack.zip");
    }

    #[test]
    fn latest_versions_keeps_the_newest_of_each_dat() {
        let files: Vec<PathBuf> = [
            "d/no-intro/No-Intro Love Pack (PC XML) (2026-09-01).zip",
            "d/no-intro/No-Intro Love Pack (PC XML) (2026-10-01).zip",
            "d/redump/Sony - PlayStation 2 - Datfile (11000) (2026-09-01 01-02-03).zip",
            "d/redump/Sony - PlayStation 2 - Datfile (11050) (2026-10-01 01-02-03).zip",
            "d/redump/Sony - PlayStation 3 - Datfile (5000) (2026-08-01 01-02-03).zip",
            "d/retrobios/database_20260901120000.json",
            "d/retrobios/database_20261001120000.json",
            "d/tosec/TOSEC - DAT Pack - Complete (4321) (TOSEC-v2025-03-13).zip",
            "d/retroachievements/Nintendo - Game Boy.dat",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        let kept: Vec<String> = latest_versions(&files).iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(
            kept,
            vec![
                "No-Intro Love Pack (PC XML) (2026-10-01).zip",
                "Sony - PlayStation 2 - Datfile (11050) (2026-10-01 01-02-03).zip",
                "Sony - PlayStation 3 - Datfile (5000) (2026-08-01 01-02-03).zip",
                "Nintendo - Game Boy.dat",
                "database_20261001120000.json",
                "TOSEC - DAT Pack - Complete (4321) (TOSEC-v2025-03-13).zip",
            ],
            "PlayStation 2 and 3 are different DATs; each keeps its own newest"
        );
    }

    #[test]
    fn content_disposition_parsing() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::CONTENT_DISPOSITION, "attachment; filename=\"Some System - Datfile (12) (2026-01-01 00-00-00).zip\"".parse().unwrap());
        assert_eq!(content_disposition_filename(&headers).as_deref(), Some("Some System - Datfile (12) (2026-01-01 00-00-00).zip"));
        headers.insert(reqwest::header::CONTENT_DISPOSITION, "attachment; filename*=UTF-8''pack%20name.zip".parse().unwrap());
        assert_eq!(content_disposition_filename(&headers).as_deref(), Some("pack name.zip"));
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%20b%2Dc.dat"), "a b-c.dat");
        assert_eq!(percent_decode("plain"), "plain");
    }

    #[test]
    fn a_sync_of_every_source_leaves_the_manual_ones_out() {
        let synced = Source::all_synced();
        assert!(synced.contains(&Source::NoIntro) && synced.contains(&Source::LostLevel));
        assert!(!synced.iter().any(|s| s.is_manual()) && synced.len() + 3 == Source::ALL.len());
    }

    #[test]
    fn redump_links_in_the_current_and_old_layouts() {
        let page = r#"<a href="https://redump.info/datfile/PSX">Datfile</a> <a href="https://redump.info/bios/PSX">BIOS</a>
            <a href="/datfile/3DO">x</a> <a href="/datfile/3DO/serial,version">old</a> <a href="https://redump.info/cues/PSX">cues</a>"#;
        let links = redump_links(page);
        let pairs: Vec<(&str, &str)> = links.iter().map(|(k, c)| (k.as_str(), c.as_str())).collect();
        assert_eq!(pairs, vec![("datfile", "3DO"), ("datfile", "PSX"), ("bios", "PSX")]);
    }

    #[test]
    fn no_intro_says_why_the_form_is_missing() {
        assert!(no_intro_form_missing("<b>Warning</b> The server is under maintenance.").contains("maintenance"));
        assert!(no_intro_form_missing("Banned by cron. Error ID: 156").contains("banned"));
        assert!(no_intro_form_missing("<b>Requests to the server have been temporarily throttled.</b>").contains("throttling"));
        assert!(no_intro_form_missing("<html></html>").contains("not found"));
    }

    #[test]
    fn no_intro_terms_page_token() {
        let terms = r#"<form method="post" action="" accept-charset="UTF-8">
                <input type="hidden" name="csrf_token" value="c25d67abc" />
                <input type="submit" name="terms_accept" value="I accept" />
            </form>"#;
        assert_eq!(no_intro_terms_token(terms).as_deref(), Some("c25d67abc"));
        assert_eq!(no_intro_terms_token(r#"<input name="daily_dl_x">"#), None);
        assert_eq!(no_intro_terms_token(r#"<input type="hidden" value="ab12" name="csrf_token"><input name="terms_accept">"#).as_deref(), Some("ab12"));
        assert_eq!(no_intro_page_gist("<html><head><title>Just a moment...</title></head><body><p>Checking your browser</p></body></html>"), "[Just a moment...] \"Checking your browser\"");
        assert_eq!(no_intro_page_gist("  "), "an empty page");
        // The daily form as of 2026-11: a csrf token, and the button named download_request.
        let page = r#"<form class="search-bar" action="x" method="post"><input type="hidden" name="csrf_token" value="aaa"><input type="submit" class="submit" name="search_all_submit" value="Search" /></form>
            <form action="index.php?page=download&op=daily&s=64" name="daily" method="post">
            <input type="hidden" name="csrf_token" value="c75a" />
            <input type="radio" name="dat_type" value="dat" checked="checked"/> DAT
            <input type="checkbox" name="set[1]" value="Ok" checked="checked"> No-Intro
            <input type="submit" name="download_request" value="Request" />
            </form>"#;
        let form = no_intro_form_block(page, "daily");
        assert_eq!(no_intro_request_button(&form).as_deref(), Some("download_request"));
        assert_eq!(no_intro_hidden_inputs(&form), vec![("csrf_token".to_string(), "c75a".to_string())]);
        let job = r#"<input type="submit" name="trap" value="Download!!" style="display: none" /><input type="submit" name="get_it" value="Download!!" />"#;
        assert_eq!(no_intro_visible_submit(job, "download").as_deref(), Some("get_it"));
        let job = r#"<form action="" method="post">
        <input type="hidden" name="csrf_token" value="c75a" />
        <br /><input type="submit" name="download_file" value="Download!!">"#;
        assert_eq!(no_intro_visible_submit(job, "download").as_deref(), Some("download_file"));
        assert_eq!(no_intro_hidden_inputs(job), vec![("csrf_token".to_string(), "c75a".to_string())]);
        assert!(no_intro_form_missing(terms).contains("terms"));
    }

    #[test]
    fn platform_scope_picks_dats_by_name() {
        let psx = Platform::PlayStation.igdb_id();
        let set: std::collections::HashSet<u16> = [psx].into_iter().collect();
        let wants = |n: &str| wants_dat(Some(&set), None, n);
        assert!(wants("Sony - PlayStation - Datfile (10870) (2026-10-31 10-00-00).zip"));
        assert!(wants("Sony - PlayStation - BIOS Datfile (13) (2026-10-31).zip"));
        assert!(!wants("Sega - Saturn - Datfile (1500) (2026-10-31 10-00-00).zip"));
        assert!(!wants("No-Intro/Nintendo - Game Boy (20261031-000000).dat"));
        // Nothing says what it is for: kept.
        assert!(wants("No-Intro Love Pack (DAT) (2026-10-31).zip"));
        assert!(wants_dat(None, None, "Sega - Saturn - Datfile (1500).zip"));
    }

    #[test]
    fn extras_pick_dats_without_a_platform() {
        assert_eq!(dat_extra("Audio CD - Datfile (7545) (2026-09-24 20-22-35).zip"), Some("audio"));
        assert_eq!(dat_extra("Source Code - Nintendo - Game Boy Advance (20261031).dat"), Some("source"));
        assert_eq!(dat_extra("TOSEC-PIX/Commodore Amiga - Magazines - Amiga Format (TOSEC-v2024).dat"), Some("document"));
        assert_eq!(dat_extra("Sony - PlayStation - Datfile (10870).zip"), None);
        assert_eq!(dat_extra("Sony - PlayStation 3 (PSN) (Avatars) (20261031).dat"), Some("theme"));
        assert_eq!(dat_extra("Nintendo - Wii (Digital) (Wallpapers) (20261031).dat"), Some("theme"));
        assert_eq!(dat_extra("Sony - PlayStation 3 (PSN) (DLC) (20261031).dat"), None);
        assert_eq!(dat_extra("No-Intro Love Pack (DAT) (2026-10-31).zip"), None);
        assert_eq!(dat_extra("TOSEC - DAT Pack - Complete (4321) (TOSEC-v2025-03-13).zip"), None);
        let psx: std::collections::HashSet<u16> = [Platform::PlayStation.igdb_id()].into_iter().collect();
        let audio: std::collections::HashSet<String> = ["audio".to_string()].into_iter().collect();
        let none = std::collections::HashSet::new();
        assert!(wants_dat(Some(&psx), Some(&audio), "Audio CD - Datfile (7545).zip"));
        assert!(!wants_dat(Some(&psx), Some(&audio), "Source Code - Sony - PlayStation (20261031).dat"));
        assert!(wants_dat(Some(&psx), Some(&none), "Sony - PlayStation - Datfile (10870).zip"));
        assert!(!wants_dat(None, Some(&none), "Audio CD - Datfile (7545).zip"));
        assert!(wants_dat(None, Some(&none), "No-Intro Love Pack (DAT) (2026-10-31).zip"));
    }

    #[test]
    fn platform_groups() {
        use crate::database::connection::platform_group;
        assert_eq!(platform_group(Platform::PlayStation), "console");
        assert_eq!(platform_group(Platform::GameBoy), "handheld");
        assert_eq!(platform_group(Platform::NeoGeoPocketColor), "handheld");
        assert_eq!(platform_group(Platform::NeoGeoCd), "console");
        assert_eq!(platform_group(Platform::NeoGeoAes), "arcade");
        assert_eq!(platform_group(Platform::Msx2), "retro_pc");
        assert_eq!(platform_group(Platform::Pc98), "retro_pc");
        assert_eq!(platform_group(Platform::FmTowns), "retro_pc");
        assert_eq!(platform_group(Platform::PcDos), "retro_pc");
        assert_eq!(platform_group(Platform::PcWindows), "modern_pc");
        assert_eq!(platform_group(Platform::Mac), "modern_pc");
        assert_eq!(platform_group(Platform::Atari2600), "other");
        assert_eq!(platform_group(Platform::PcFx), "other");
        assert_eq!(platform_group(Platform::TurboGrafxCd), "console");
        assert_eq!(platform_group(Platform::SegaSaturn), "other");
        assert_eq!(platform_group(Platform::SegaGameGear), "other");
        assert_eq!(crate::database::connection::platform_family(Platform::SegaGameGear), Some("sega"));
        assert_eq!(crate::database::connection::platform_family(Platform::SuperFamicom), Some("nintendo"));
        assert_eq!(crate::database::connection::platform_family(Platform::PokemonMini), Some("nintendo"));
        assert_eq!(crate::database::connection::platform_family(Platform::PocketStation), Some("sony"));
        assert_eq!(platform_group(Platform::SuperFamicom), "console");
        assert_eq!(platform_group(Platform::AtariLynx), "other");
        assert_eq!(platform_group(Platform::WonderSwanColor), "handheld");
    }

    #[test]
    fn source_names_round_trip() {
        for s in Source::ALL {
            assert_eq!(Source::from_name(s.name()), Some(s));
        }
    }
}
