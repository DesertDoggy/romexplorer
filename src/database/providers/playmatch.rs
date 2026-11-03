//! PlayMatch (the hosted service at playmatch.retrorealm.dev, not the vendored provider
//! clients): identifies one file by its hashes/name/size and returns the game's ids at
//! other providers (IGDB, MobyGames, ScreenScraper, SteamGridDB, RetroAchievements,
//! TheGamesDB, LaunchBox, ...). Public API, no credentials; ~4 requests/second, so calls
//! are serialized and spaced like the RetroAchievements client's. Own implementation
//! against the documented endpoint -- nothing vendored.

use std::time::{Duration, Instant};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::super::error::DatabaseError;
use super::Provider;

pub const API_BASE: &str = "https://playmatch.retrorealm.dev/api/v2";

#[derive(Debug, Clone, Default, Serialize)]
pub struct IdentifyRequest {
    pub file_name: Option<String>,
    pub file_size: Option<u64>,
    pub md5: Option<String>,
    pub sha1: Option<String>,
    pub sha256: Option<String>,
    pub crc: Option<String>,
}

impl IdentifyRequest {
    pub fn has_hash(&self) -> bool {
        self.md5.is_some() || self.sha1.is_some() || self.sha256.is_some() || self.crc.is_some()
    }
}

/// One provider id PlayMatch reports for the identified game.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalMetadata {
    #[serde(default)]
    pub provider_name: Option<String>,
    #[serde(default)]
    pub provider_id: Option<serde_json::Value>,
    #[serde(default)]
    pub match_type: Option<String>,
    #[serde(default)]
    pub automatic_match_reason: Option<String>,
    #[serde(default)]
    pub manual_match_type: Option<String>,
    #[serde(default)]
    pub comment: Option<String>,
}

impl ExternalMetadata {
    /// This project's provider for PlayMatch's spelling, if it's one we have.
    pub fn provider(&self) -> Option<Provider> {
        let name = self.provider_name.as_deref()?.to_ascii_uppercase().replace(['-', '_', ' '], "");
        match name.as_str() {
            "IGDB" => Some(Provider::Igdb),
            "MOBYGAMES" => Some(Provider::MobyGames),
            "SCREENSCRAPER" => Some(Provider::ScreenScraper),
            "RETROACHIEVEMENTS" => Some(Provider::RetroAchievements),
            "STEAMGRIDDB" => Some(Provider::SteamGridDb),
            "TGDB" | "THEGAMESDB" => Some(Provider::TheGamesDb),
            "HASHEOUS" => Some(Provider::Hasheous),
            _ => None,
        }
    }

    /// The id as a string ("0"/empty are misses).
    pub fn id(&self) -> Option<String> {
        let id = match self.provider_id.as_ref()? {
            serde_json::Value::String(s) => s.trim().to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            _ => return None,
        };
        (!id.is_empty() && id != "0").then_some(id)
    }
}

/// `GET /identify/ids` response; unknown fields are ignored, missing ones default so a
/// shape change on their side degrades to "no ids" rather than an error.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentifyResponse {
    #[serde(default)]
    pub game_match_type: Option<String>,
    #[serde(default)]
    pub external_metadata: Vec<ExternalMetadata>,
}

impl IdentifyResponse {
    /// Anything but an explicit `NoMatch` (or nothing) counts as a hit.
    pub fn is_hit(&self) -> bool {
        match self.game_match_type.as_deref() {
            None | Some("") => !self.external_metadata.is_empty(),
            Some(t) => !t.eq_ignore_ascii_case("NoMatch"),
        }
    }

    /// Whether PlayMatch found the game by one of the file's hashes. Its other answer,
    /// `FileNameAndSize`, is a guess from a name and size many unrelated files share
    /// (`EBOOT.BIN`, `PARAM.SFO`, a disc's `track01.bin`): it put a PS3 game, a PSP DLC and
    /// an unknown-platform title under one IGDB id. A response without a match type (the
    /// shape before PlayMatch reported one) is taken as a hash match.
    pub fn matched_by_hash(&self) -> bool {
        match self.game_match_type.as_deref() {
            None | Some("") => true,
            Some(t) => ["MD5", "SHA1", "SHA256", "CRC"].iter().any(|h| t.eq_ignore_ascii_case(h)),
        }
    }

    /// (provider, id) pairs for providers this project knows; none when the match was not
    /// by hash (`matched_by_hash`), so a name-and-size guess never becomes a `hash` link.
    pub fn cross_refs(&self) -> Vec<(Provider, String)> {
        let mut out: Vec<(Provider, String)> = Vec::new();
        if !self.matched_by_hash() {
            return out;
        }
        for m in &self.external_metadata {
            let (Some(p), Some(id)) = (m.provider(), m.id()) else { continue };
            if !out.iter().any(|(q, i)| *q == p && *i == id) {
                out.push((p, id));
            }
        }
        out
    }
}

pub struct PlayMatchClient {
    http: Client,
    base: String,
    gate: Mutex<Option<Instant>>,
    min_interval: Duration,
    max_retries: u32,
}

impl PlayMatchClient {
    pub fn new(http: Client) -> Self {
        // 4/s documented; 275 ms spacing keeps a 10% margin against clock drift.
        Self { http, base: API_BASE.to_string(), gate: Mutex::new(None), min_interval: Duration::from_millis(275), max_retries: 2 }
    }

    #[cfg(test)]
    pub fn with_base(mut self, base: &str) -> Self {
        self.base = base.trim_end_matches('/').to_string();
        self
    }

    /// One serialized, rate-limited GET of `/identify/ids`; returns the parsed response
    /// and the raw JSON (for the raw store).
    pub async fn identify(&self, request: &IdentifyRequest) -> Result<(IdentifyResponse, serde_json::Value), DatabaseError> {
        let url = format!("{}/identify/ids", self.base);
        let mut query: Vec<(&str, String)> = Vec::new();
        if let Some(n) = &request.file_name {
            query.push(("fileName", n.clone()));
        }
        if let Some(s) = request.file_size {
            query.push(("fileSize", s.to_string()));
        }
        for (key, value) in [("md5", &request.md5), ("sha1", &request.sha1), ("sha256", &request.sha256), ("crc", &request.crc)] {
            if let Some(v) = value {
                query.push((key, v.to_ascii_lowercase()));
            }
        }
        let mut attempt = 0u32;
        loop {
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
            let response = self.http.get(&url).query(&query).header(reqwest::header::ACCEPT, "application/json").send().await;
            let retryable = match &response {
                Ok(r) => r.status().as_u16() == 429 || r.status().is_server_error(),
                Err(e) => e.is_connect() || e.is_timeout() || e.is_request(),
            };
            match response {
                Ok(r) if !retryable => {
                    let status = r.status();
                    if status.as_u16() == 404 {
                        return Ok((IdentifyResponse { game_match_type: Some("NoMatch".into()), external_metadata: Vec::new() }, serde_json::json!({"gameMatchType": "NoMatch"})));
                    }
                    let body = r.text().await.map_err(|e| DatabaseError::Fetch(format!("playmatch identify: {e}")))?;
                    if !status.is_success() {
                        return Err(DatabaseError::Provider(format!("playmatch identify: HTTP {status}: {}", body.chars().take(200).collect::<String>())));
                    }
                    let raw: serde_json::Value = serde_json::from_str(&body).map_err(|e| DatabaseError::Provider(format!("playmatch identify: bad JSON: {e}")))?;
                    let parsed: IdentifyResponse = serde_json::from_value(raw.clone()).map_err(|e| DatabaseError::Provider(format!("playmatch identify: unexpected shape: {e}")))?;
                    return Ok((parsed, raw));
                }
                _ if attempt < self.max_retries => {
                    attempt += 1;
                    let backoff = Duration::from_millis(1000 * (1u64 << attempt));
                    log::warn!("playmatch identify: retry {attempt}/{} after {backoff:?}", self.max_retries);
                    tokio::time::sleep(backoff).await;
                }
                Ok(r) => return Err(DatabaseError::Provider(format!("playmatch identify: HTTP {} after {attempt} retries", r.status()))),
                Err(e) => return Err(DatabaseError::Fetch(format!("playmatch identify: {e}"))),
            }
        }
    }

    /// `POST /identify/bulk/ids`: up to [`BULK_MAX`] items in one request (one rate-limit
    /// slot). Items are `(key, request)`; results come back keyed the same way, `None` for
    /// an item PlayMatch rejected or that errored. Items without both file name and size
    /// (required by the endpoint) should be filtered out by the caller.
    pub async fn identify_bulk(&self, items: &[(String, IdentifyRequest)]) -> Result<Vec<(String, Option<(IdentifyResponse, serde_json::Value)>)>, DatabaseError> {
        let url = format!("{}/identify/bulk/ids", self.base);
        let body = bulk_body(items);
        let mut attempt = 0u32;
        loop {
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
            let response = self.http.post(&url).json(&body).header(reqwest::header::ACCEPT, "application/json").send().await;
            let retryable = match &response {
                Ok(r) => r.status().as_u16() == 429 || r.status().is_server_error(),
                Err(e) => e.is_connect() || e.is_timeout() || e.is_request(),
            };
            match response {
                Ok(r) if !retryable => {
                    let status = r.status();
                    let text = r.text().await.map_err(|e| DatabaseError::Fetch(format!("playmatch bulk identify: {e}")))?;
                    if !status.is_success() {
                        return Err(DatabaseError::Provider(format!("playmatch bulk identify: HTTP {status}: {}", text.chars().take(200).collect::<String>())));
                    }
                    let raw: serde_json::Value = serde_json::from_str(&text).map_err(|e| DatabaseError::Provider(format!("playmatch bulk identify: bad JSON: {e}")))?;
                    return Ok(parse_bulk(&raw));
                }
                _ if attempt < self.max_retries => {
                    attempt += 1;
                    let backoff = Duration::from_millis(1000 * (1u64 << attempt));
                    log::warn!("playmatch bulk identify: retry {attempt}/{} after {backoff:?}", self.max_retries);
                    tokio::time::sleep(backoff).await;
                }
                Ok(r) => return Err(DatabaseError::Provider(format!("playmatch bulk identify: HTTP {} after {attempt} retries", r.status()))),
                Err(e) => return Err(DatabaseError::Fetch(format!("playmatch bulk identify: {e}"))),
            }
        }
    }
}

/// The public instance's per-request cap for bulk identify.
pub const BULK_MAX: usize = 100;

/// The bulk identify body: `{"items": [{key, fileName, fileSize, md5, sha1, sha256, crc}]}`
/// (hashes lowercase, absent ones left out).
pub fn bulk_body(items: &[(String, IdentifyRequest)]) -> serde_json::Value {
    let items: Vec<serde_json::Value> = items
        .iter()
        .map(|(key, r)| {
            let mut item = serde_json::json!({ "key": key, "fileName": r.file_name.clone().unwrap_or_default(), "fileSize": r.file_size.unwrap_or(0) });
            for (name, value) in [("md5", &r.md5), ("sha1", &r.sha1), ("sha256", &r.sha256), ("crc", &r.crc)] {
                if let Some(v) = value {
                    item[name] = serde_json::Value::String(v.to_ascii_lowercase());
                }
            }
            item
        })
        .collect();
    serde_json::json!({ "items": items })
}

/// Bulk results by caller key: the parsed match and its raw JSON (the same shape a single
/// identify returns); an item that wasn't `ok` (or has no `match`) is `None`.
pub fn parse_bulk(raw: &serde_json::Value) -> Vec<(String, Option<(IdentifyResponse, serde_json::Value)>)> {
    raw["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let key = r["key"].as_str()?.to_string();
            let matched = (r["status"].as_str() == Some("ok")).then(|| serde_json::from_value::<IdentifyResponse>(r["match"].clone()).ok().map(|m| (m, r["match"].clone()))).flatten();
            Some((key, matched))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulk_body_and_results_round_trip_by_key() {
        let req = IdentifyRequest { file_name: Some("Game.gba".into()), file_size: Some(4096), md5: Some("ABCDEF".into()), sha1: None, sha256: None, crc: Some("1234ABCD".into()) };
        let body = bulk_body(&[("42".to_string(), req)]);
        assert_eq!(body["items"][0]["key"], "42");
        assert_eq!(body["items"][0]["md5"], "abcdef", "hashes lowercased");
        assert_eq!(body["items"][0]["crc"], "1234abcd");
        assert!(body["items"][0].get("sha1").is_none(), "absent hashes left out");
        let raw = serde_json::json!({"summary": {"total": 2}, "results": [
            {"index": 0, "key": "42", "status": "ok", "match": {"gameMatchType": "MD5", "externalMetadata": [{"providerName": "IGDB", "providerId": 7}]}},
            {"index": 1, "key": "43", "status": "invalid", "error": {"code": "x"}}
        ]});
        let parsed = parse_bulk(&raw);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0, "42");
        assert!(parsed[0].1.as_ref().is_some_and(|(r, raw)| r.is_hit() && raw["gameMatchType"] == "MD5"));
        assert!(parsed[1].1.is_none(), "a non-ok item is no match");
    }

    #[test]
    fn response_parses_and_maps_providers() {
        let raw = serde_json::json!({
            "gameMatchType": "SHA1",
            "externalMetadata": [
                {"providerName": "IGDB", "providerId": 1234, "matchType": "Manual"},
                {"providerName": "RETRO_ACHIEVEMENTS", "providerId": "77"},
                {"providerName": "LAUNCHBOX", "providerId": "5"},
                {"providerName": "MOBYGAMES", "providerId": "0"},
                {"providerName": "TGDB", "providerId": ""}
            ],
            "somethingNew": true
        });
        let parsed: IdentifyResponse = serde_json::from_value(raw).unwrap();
        assert!(parsed.is_hit());
        assert_eq!(parsed.cross_refs(), vec![(Provider::Igdb, "1234".to_string()), (Provider::RetroAchievements, "77".to_string())], "unknown providers and 0/empty ids are dropped");
        let miss: IdentifyResponse = serde_json::from_value(serde_json::json!({"gameMatchType": "NoMatch", "externalMetadata": []})).unwrap();
        assert!(!miss.is_hit());
        let bare: IdentifyResponse = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(!bare.is_hit());
    }

    #[test]
    fn a_name_and_size_match_links_nothing() {
        let guess: IdentifyResponse = serde_json::from_value(serde_json::json!({
            "gameMatchType": "FileNameAndSize",
            "externalMetadata": [{"providerName": "IGDB", "providerId": 21076}]
        }))
        .unwrap();
        assert!(guess.is_hit(), "still a hit, so it is cached and not asked again");
        assert!(!guess.matched_by_hash());
        assert!(guess.cross_refs().is_empty());
        let crc: IdentifyResponse = serde_json::from_value(serde_json::json!({"gameMatchType": "crc", "externalMetadata": [{"providerName": "IGDB", "providerId": 7}]})).unwrap();
        assert_eq!(crc.cross_refs(), vec![(Provider::Igdb, "7".to_string())]);
    }
}
