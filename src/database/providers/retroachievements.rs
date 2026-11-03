//! RetroAchievements web API client. This project's own (the reference implementation,
//! x3kim/RAChecker's `server/src/ra-api.js`, is Node.js -- see
//! `app_root_ex/lib/LICENSES/RAChecker.LICENSE`), keeping its shape: `z`/`y` query-param
//! auth, calls serialized behind a minimum interval, exponential backoff on 429/5xx/network
//! errors, and "be gentle and cache aggressively" (RA publishes no numeric rate limit).
//!
//! Only the endpoints this project uses are wrapped; every response is also kept as raw
//! JSON so the cache can store it verbatim.

use std::time::{Duration, Instant};

use reqwest::Client;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::super::error::DatabaseError;

pub const API_BASE: &str = "https://retroachievements.org/API";
pub const MEDIA_BASE: &str = "https://media.retroachievements.org";

pub struct RaClient {
    http: Client,
    username: String,
    api_key: String,
    gate: Mutex<Option<Instant>>,
    pub min_interval: Duration,
    pub max_retries: u32,
}

impl std::fmt::Debug for RaClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RaClient").field("username", &self.username).field("min_interval", &self.min_interval).finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaConsole {
    #[serde(rename = "ID")]
    pub id: u32,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Active", default)]
    pub active: Option<bool>,
    #[serde(rename = "IsGameSystem", default)]
    pub is_game_system: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaGameListEntry {
    #[serde(rename = "ID")]
    pub id: u32,
    #[serde(rename = "Title", default)]
    pub title: String,
    #[serde(rename = "ConsoleID", default)]
    pub console_id: u32,
    #[serde(rename = "ConsoleName", default)]
    pub console_name: Option<String>,
    #[serde(rename = "ImageIcon", default)]
    pub image_icon: Option<String>,
    #[serde(rename = "NumAchievements", default)]
    pub num_achievements: Option<u32>,
    #[serde(rename = "Points", default)]
    pub points: Option<u32>,
    #[serde(rename = "DateModified", default)]
    pub date_modified: Option<String>,
    #[serde(rename = "Hashes", default)]
    pub hashes: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaGame {
    /// `API_GetGame` omits this (only `GetGameExtended` carries it); the client fills it
    /// from the requested id.
    #[serde(rename = "ID", default)]
    pub id: u32,
    #[serde(rename = "Title", default)]
    pub title: String,
    #[serde(rename = "ConsoleID", default)]
    pub console_id: u32,
    #[serde(rename = "ConsoleName", default)]
    pub console_name: Option<String>,
    #[serde(rename = "ImageIcon", default)]
    pub image_icon: Option<String>,
    #[serde(rename = "ImageTitle", default)]
    pub image_title: Option<String>,
    #[serde(rename = "ImageIngame", default)]
    pub image_ingame: Option<String>,
    #[serde(rename = "ImageBoxArt", default)]
    pub image_box_art: Option<String>,
    #[serde(rename = "Publisher", default)]
    pub publisher: Option<String>,
    #[serde(rename = "Developer", default)]
    pub developer: Option<String>,
    #[serde(rename = "Genre", default)]
    pub genre: Option<String>,
    #[serde(rename = "Released", default)]
    pub released: Option<String>,
    #[serde(rename = "NumAchievements", default)]
    pub num_achievements: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaUserProfile {
    #[serde(rename = "User", default)]
    pub user: String,
    #[serde(rename = "UserPic", default)]
    pub user_pic: Option<String>,
    #[serde(rename = "MemberSince", default)]
    pub member_since: Option<String>,
    #[serde(rename = "RichPresenceMsg", default)]
    pub rich_presence: Option<String>,
    #[serde(rename = "LastGameID", default)]
    pub last_game_id: Option<u32>,
    #[serde(rename = "TotalPoints", default)]
    pub total_points: Option<u64>,
    #[serde(rename = "TotalSoftcorePoints", default)]
    pub total_softcore_points: Option<u64>,
    #[serde(rename = "TotalTruePoints", default)]
    pub total_true_points: Option<u64>,
    #[serde(rename = "Motto", default)]
    pub motto: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaCompletionPage {
    #[serde(rename = "Count", default)]
    pub count: u32,
    #[serde(rename = "Total", default)]
    pub total: u32,
    #[serde(rename = "Results", default)]
    pub results: Vec<RaCompletionEntry>,
}

/// One played game: `highest_award_kind` is `mastered` (hardcore complete), `completed`
/// (softcore complete), `beaten-hardcore`, `beaten-softcore` or none.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaCompletionEntry {
    #[serde(rename = "GameID")]
    pub game_id: u32,
    #[serde(rename = "Title", default)]
    pub title: String,
    #[serde(rename = "ImageIcon", default)]
    pub image_icon: Option<String>,
    #[serde(rename = "ConsoleID", default)]
    pub console_id: u32,
    #[serde(rename = "ConsoleName", default)]
    pub console_name: Option<String>,
    #[serde(rename = "MaxPossible", default)]
    pub max_possible: Option<u32>,
    #[serde(rename = "NumAwarded", default)]
    pub num_awarded: Option<u32>,
    #[serde(rename = "NumAwardedHardcore", default)]
    pub num_awarded_hardcore: Option<u32>,
    #[serde(rename = "MostRecentAwardedDate", default)]
    pub most_recent_awarded: Option<String>,
    #[serde(rename = "HighestAwardKind", default)]
    pub highest_award_kind: Option<String>,
    #[serde(rename = "HighestAwardDate", default)]
    pub highest_award_date: Option<String>,
}

impl RaClient {
    pub fn new(http: Client, username: String, api_key: String) -> Self {
        Self { http, username, api_key, gate: Mutex::new(None), min_interval: Duration::from_millis(1500), max_retries: 3 }
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    /// Absolute URL for a media path RA returns (`/Images/...`).
    pub fn media_url(path: &str) -> String {
        if path.starts_with("http") {
            path.to_string()
        } else {
            format!("{MEDIA_BASE}/{}", path.trim_start_matches('/'))
        }
    }

    /// One serialized, rate-limited, retried GET of `API_<endpoint>.php`, as raw JSON.
    pub async fn call_raw(&self, endpoint: &str, params: &[(&str, String)]) -> Result<serde_json::Value, DatabaseError> {
        let url = format!("{API_BASE}/API_{endpoint}.php");
        let mut attempt = 0u32;
        loop {
            // Serialize calls and space them out.
            {
                let mut gate = self.gate.lock().await;
                if let Some(last) = *gate {
                    let elapsed = last.elapsed();
                    if elapsed < self.min_interval {
                        tokio::time::sleep(self.min_interval - elapsed).await;
                    }
                }
                *gate = Some(Instant::now());
            }
            let mut query: Vec<(&str, String)> = vec![("z", self.username.clone()), ("y", self.api_key.clone())];
            query.extend(params.iter().cloned());
            let response = self.http.get(&url).query(&query).header(reqwest::header::ACCEPT, "application/json").send().await;
            let retryable = match &response {
                Ok(r) => r.status().as_u16() == 429 || r.status().is_server_error(),
                Err(e) => e.is_connect() || e.is_timeout() || e.is_request(),
            };
            match response {
                Ok(r) if !retryable => {
                    let status = r.status();
                    let body = r.text().await.map_err(|e| DatabaseError::Fetch(format!("retroachievements {endpoint}: {e}")))?;
                    if !status.is_success() {
                        // Never echo the URL (it carries the key); the body is RA's own message.
                        return Err(DatabaseError::Provider(format!("retroachievements {endpoint}: HTTP {status}: {}", body.chars().take(200).collect::<String>())));
                    }
                    return serde_json::from_str(&body).map_err(|e| DatabaseError::Provider(format!("retroachievements {endpoint}: bad JSON: {e}")));
                }
                _ if attempt < self.max_retries => {
                    attempt += 1;
                    let backoff = Duration::from_millis(1000 * (1u64 << attempt));
                    log::warn!("retroachievements {endpoint}: retry {attempt}/{} after {backoff:?}", self.max_retries);
                    tokio::time::sleep(backoff).await;
                }
                Ok(r) => return Err(DatabaseError::Provider(format!("retroachievements {endpoint}: HTTP {} after {attempt} retries", r.status()))),
                Err(e) => return Err(DatabaseError::Fetch(format!("retroachievements {endpoint}: {e}"))),
            }
        }
    }

    pub async fn call<T: DeserializeOwned>(&self, endpoint: &str, params: &[(&str, String)]) -> Result<(T, serde_json::Value), DatabaseError> {
        let raw = self.call_raw(endpoint, params).await?;
        let parsed = serde_json::from_value(raw.clone()).map_err(|e| DatabaseError::Provider(format!("retroachievements {endpoint}: unexpected shape: {e}")))?;
        Ok((parsed, raw))
    }

    pub async fn get_console_ids(&self, active_only: bool, game_systems_only: bool) -> Result<(Vec<RaConsole>, serde_json::Value), DatabaseError> {
        self.call("GetConsoleIDs", &[("a", (active_only as u8).to_string()), ("g", (game_systems_only as u8).to_string())]).await
    }

    /// Every game on `console_id` (with MD5 hashes when `with_hashes`) -- one call per
    /// console, the bulk `by_platform` sync.
    pub async fn get_game_list(&self, console_id: u32, with_hashes: bool, only_with_achievements: bool) -> Result<(Vec<RaGameListEntry>, serde_json::Value), DatabaseError> {
        self.call("GetGameList", &[("i", console_id.to_string()), ("h", (with_hashes as u8).to_string()), ("f", (only_with_achievements as u8).to_string())]).await
    }

    pub async fn get_game(&self, game_id: u32) -> Result<(RaGame, serde_json::Value), DatabaseError> {
        let (mut game, raw): (RaGame, _) = self.call("GetGame", &[("i", game_id.to_string())]).await?;
        if game.id == 0 {
            game.id = game_id;
        }
        Ok((game, raw))
    }

    pub async fn get_game_extended(&self, game_id: u32) -> Result<(RaGame, serde_json::Value), DatabaseError> {
        let (mut game, raw): (RaGame, _) = self.call("GetGameExtended", &[("i", game_id.to_string())]).await?;
        if game.id == 0 {
            game.id = game_id;
        }
        Ok((game, raw))
    }

    /// The account's profile: avatar, points, rich presence (`API_GetUserProfile`).
    pub async fn get_user_profile(&self, user: &str) -> Result<(RaUserProfile, serde_json::Value), DatabaseError> {
        self.call("GetUserProfile", &[("u", user.to_string())]).await
    }

    /// One page (at most 500) of every game the account has played, with its award level
    /// (`API_GetUserCompletionProgress`).
    pub async fn get_user_completion_progress(&self, user: &str, offset: u32, count: u32) -> Result<(RaCompletionPage, serde_json::Value), DatabaseError> {
        self.call("GetUserCompletionProgress", &[("u", user.to_string()), ("c", count.min(500).to_string()), ("o", offset.to_string())]).await
    }

    /// One game's details, images and every achievement with `user`'s unlock dates
    /// (`API_GetGameInfoAndUserProgress`), as raw JSON.
    pub async fn get_game_info_and_user_progress(&self, game_id: u32, user: &str) -> Result<serde_json::Value, DatabaseError> {
        self.call_raw("GetGameInfoAndUserProgress", &[("g", game_id.to_string()), ("u", user.to_string()), ("a", "1".to_string())]).await
    }

    /// ROM names/regions per hash for one game (one call per game -- use sparingly).
    pub async fn get_game_hashes(&self, game_id: u32) -> Result<serde_json::Value, DatabaseError> {
        self.call_raw("GetGameHashes", &[("i", game_id.to_string())]).await
    }
}
