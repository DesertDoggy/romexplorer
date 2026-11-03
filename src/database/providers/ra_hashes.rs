//! RetroAchievements' per-hash list for one game (`API_GetGameHashes`): the name RA knows
//! each supported ROM by, its labels, and -- for a patched ROM -- the compatibility
//! patch's URL. Cached in `ra_game_hashes`, refreshed when a game's page is opened and the
//! cache is older than `cache_ttl_days` (one API call per game).

use sqlx::mysql::MySqlPool;

use super::retroachievements::RaClient;
use crate::database::error::DatabaseError;

/// One entry of the hash list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameHash {
    pub md5: String,
    pub name: Option<String>,
    pub labels: Vec<String>,
    pub patch_url: Option<String>,
}

/// Reads `{"Results": [{"Name", "MD5", "Labels", "PatchUrl"}]}`.
pub fn parse_game_hashes(value: &serde_json::Value) -> Vec<GameHash> {
    let text = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_str()).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    value
        .get("Results")
        .and_then(|r| r.as_array())
        .into_iter()
        .flatten()
        .filter_map(|h| {
            let md5 = text(h, "MD5")?.to_ascii_lowercase();
            (md5.len() == 32 && md5.chars().all(|c| c.is_ascii_hexdigit())).then(|| GameHash {
                md5,
                name: text(h, "Name"),
                labels: h.get("Labels").and_then(|l| l.as_array()).into_iter().flatten().filter_map(|l| l.as_str().map(str::to_string)).collect(),
                patch_url: text(h, "PatchUrl"),
            })
        })
        .collect()
}

/// Whether the game's cached hash list is missing or older than `ttl_days`.
pub async fn is_stale(pool: &MySqlPool, ra_game_id: u32, ttl_days: u32) -> Result<bool, DatabaseError> {
    let newest: Option<chrono::NaiveDateTime> = sqlx::query_scalar("SELECT MAX(fetched_at) FROM ra_game_hashes WHERE ra_game_id = ?").bind(ra_game_id).fetch_one(pool).await?;
    Ok(newest.is_none_or(|t| t < chrono::Utc::now().naive_utc() - chrono::Duration::days(i64::from(ttl_days))))
}

/// Fetches the game's hash list and replaces its cached rows. Returns how many hashes.
pub async fn refresh_game_hashes(pool: &MySqlPool, client: &RaClient, ra_game_id: u32) -> Result<usize, DatabaseError> {
    let hashes = parse_game_hashes(&client.get_game_hashes(ra_game_id).await?);
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM ra_game_hashes WHERE ra_game_id = ?").bind(ra_game_id).execute(&mut *tx).await?;
    for h in &hashes {
        sqlx::query(
            "INSERT INTO ra_game_hashes (md5, ra_game_id, name, labels, patch_url, fetched_at) VALUES (?, ?, ?, ?, ?, UTC_TIMESTAMP()) \
             ON DUPLICATE KEY UPDATE ra_game_id = VALUES(ra_game_id), name = VALUES(name), labels = VALUES(labels), patch_url = VALUES(patch_url), fetched_at = VALUES(fetched_at)",
        )
        .bind(&h.md5)
        .bind(ra_game_id)
        .bind(h.name.as_ref().map(|n| n.chars().take(512).collect::<String>()))
        .bind((!h.labels.is_empty()).then(|| h.labels.join(",").chars().take(255).collect::<String>()))
        .bind(h.patch_url.as_ref().map(|u| u.chars().take(1024).collect::<String>()))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(hashes.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_hash_list() {
        let v = serde_json::json!({ "Results": [
            { "Name": "Game (Japan) [T-En by X]", "MD5": "ABCDEF0123456789ABCDEF0123456789", "Labels": ["nointro", "rapatches"], "PatchUrl": "https://github.com/RetroAchievements/RAPatches/raw/main/x.zip" },
            { "Name": "Game (USA)", "MD5": "0123456789abcdef0123456789abcdef", "Labels": [], "PatchUrl": null },
            { "Name": "broken", "MD5": "nope" }
        ]});
        let h = parse_game_hashes(&v);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].md5, "abcdef0123456789abcdef0123456789");
        assert_eq!(h[0].labels, vec!["nointro", "rapatches"]);
        assert!(h[0].patch_url.is_some() && h[1].patch_url.is_none());
    }
}
