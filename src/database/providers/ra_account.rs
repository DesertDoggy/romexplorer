//! The RetroAchievements account's own progress (the api.env `ra_username`): profile and
//! every game it has played with its award level, cached in `ra_user_profile` /
//! `ra_user_progress` by the `ra_account_sync` job (one profile call plus one call per 500
//! played games). The RetroAchievements tab reads only these tables.

use sqlx::mysql::MySqlPool;

use super::super::error::DatabaseError;
use super::super::jobs::Progress;
use super::retroachievements::RaCompletionEntry;
use super::ProviderClients;

/// RA game ids linked to releases the user holds a local game for, with how many local
/// games and one such release. Only RA-supported releases count: an exact link (the
/// release's hash is in RA's hash list) or a manual one -- a hash service's guess
/// (Hasheous) doesn't make a disc RetroAchievements-compatible.
pub const LOCAL_RA: &str = "SELECT CAST(rpl.provider_id AS UNSIGNED) AS game_id, COUNT(DISTINCT lg.id) AS local_games, MIN(rpl.release_id) AS release_id \
    FROM release_provider_links rpl JOIN local_release_games lrg ON lrg.release_id = rpl.release_id \
    JOIN local_game lg ON lg.id = lrg.game_id AND lg.deleted = FALSE \
    WHERE rpl.provider = 'retroachievements' AND rpl.deleted = FALSE AND rpl.confidence IN ('exact', 'manual') AND rpl.provider_id REGEXP '^[0-9]+$' GROUP BY rpl.provider_id";

/// RA's genre text folded onto the 19 major genres, comma-separated (`None` when empty).
pub fn genre_majors(raw: &str) -> Option<String> {
    let majors = super::ra_genre::normalize_ra_genres(raw);
    (!majors.is_empty()).then(|| majors.join(","))
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct GenreFetchSummary {
    pub candidates: u64,
    pub fetched: u64,
    pub with_genre: u64,
    pub failed: u64,
}

/// RAChecker's genre job: RA publishes no genres in bulk, so each game is asked once
/// (`API_GetGame`, ~1.5 s apart -- the client's pacing), local games first. `local_only`
/// limits it to games linked to local games; `limit` caps one run (0 = all). The same call
/// also fills in the game's title screen / screenshot / box art paths. Resumable: games
/// already asked (`genre_checked_at`) are skipped, so a cancelled run continues next time.
pub async fn fetch_genres(clients: &ProviderClients, pool: &MySqlPool, local_only: bool, limit: u64, progress: &Progress) -> Result<GenreFetchSummary, DatabaseError> {
    let client = clients.retroachievements.as_ref().ok_or_else(|| DatabaseError::Provider("retroachievements client unavailable (ra_username / ra_api_key in api.env)".to_string()))?;
    // Rows whose genre text arrived before genre_major existed.
    for row in sqlx::query("SELECT provider_id, genre_raw FROM retroachievements_games WHERE genre_raw IS NOT NULL AND genre_major IS NULL").fetch_all(pool).await? {
        let id: u32 = sqlx::Row::try_get(&row, "provider_id")?;
        let raw: String = sqlx::Row::try_get(&row, "genre_raw")?;
        sqlx::query("UPDATE retroachievements_games SET genre_major = ?, genre_checked_at = COALESCE(genre_checked_at, NOW()) WHERE provider_id = ?").bind(genre_majors(&raw)).bind(id).execute(pool).await?;
    }
    let scope = if local_only { "loc.game_id IS NOT NULL" } else { "(loc.game_id IS NOT NULL OR rg.num_achievements > 0)" };
    let sql = format!(
        "SELECT CAST(u.game_id AS UNSIGNED) AS game_id FROM (SELECT game_id FROM ({LOCAL_RA}) l0 UNION SELECT provider_id FROM retroachievements_games WHERE deleted = FALSE AND num_achievements > 0) u \
         LEFT JOIN ({LOCAL_RA}) loc ON loc.game_id = u.game_id LEFT JOIN retroachievements_games rg ON rg.provider_id = u.game_id \
         WHERE {scope} AND rg.genre_checked_at IS NULL ORDER BY (loc.game_id IS NULL), u.game_id{}",
        if limit > 0 { format!(" LIMIT {limit}") } else { String::new() }
    );
    let ids: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql)).fetch_all(pool).await?;
    let mut s = GenreFetchSummary { candidates: ids.len() as u64, ..Default::default() };
    let total = Some(s.candidates);
    let started = std::time::Instant::now();
    for (i, id) in ids.iter().enumerate() {
        let eta = if i > 0 { format!(", ~{} min left", (started.elapsed().as_secs_f64() / i as f64 * (ids.len() - i) as f64 / 60.0).ceil() as u64) } else { String::new() };
        progress.report(i as u64, total, &format!("genres: game {} of {} ({} with a genre{eta})", i + 1, ids.len(), s.with_genre)).await;
        match client.get_game(*id as u32).await {
            Ok((game, _)) => {
                let genre = game.genre.as_deref().map(str::trim).filter(|g| !g.is_empty());
                sqlx::query(
                    "INSERT INTO retroachievements_games (provider_id, console_id, console_name, title, genre_raw, genre_major, genre_checked_at, image_icon, image_title, image_ingame, image_boxart, fetched_at, deleted) \
                     VALUES (?, ?, ?, ?, ?, ?, NOW(), ?, ?, ?, ?, NOW(), FALSE) ON DUPLICATE KEY UPDATE console_name = COALESCE(VALUES(console_name), console_name), \
                     genre_raw = COALESCE(VALUES(genre_raw), genre_raw), genre_major = COALESCE(VALUES(genre_major), genre_major), genre_checked_at = NOW(), \
                     image_icon = COALESCE(VALUES(image_icon), image_icon), image_title = COALESCE(VALUES(image_title), image_title), \
                     image_ingame = COALESCE(VALUES(image_ingame), image_ingame), image_boxart = COALESCE(VALUES(image_boxart), image_boxart)",
                )
                .bind(*id)
                .bind(game.console_id)
                .bind(game.console_name.as_deref())
                .bind(&game.title)
                .bind(genre)
                .bind(genre.and_then(genre_majors))
                .bind(game.image_icon.as_deref())
                .bind(game.image_title.as_deref())
                .bind(game.image_ingame.as_deref())
                .bind(game.image_box_art.as_deref())
                .execute(pool)
                .await?;
                s.fetched += 1;
                if genre.is_some() {
                    s.with_genre += 1;
                }
            }
            Err(e) => {
                s.failed += 1;
                log::warn!("ra_genre_fetch: game {id}: {e}");
            }
        }
    }
    progress.force(s.candidates, total, &format!("done: {} game(s) asked, {} with a genre, {} failed", s.fetched, s.with_genre, s.failed)).await;
    Ok(s)
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct AccountSyncSummary {
    pub user: String,
    pub games: u64,
    pub requests: u32,
}

/// RA dates come as `2023-05-01 12:34:56` or ISO 8601 with an offset; stored as UTC.
pub fn parse_ra_date(s: &str) -> Option<chrono::NaiveDateTime> {
    let s = s.trim();
    if let Ok(d) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(d.naive_utc());
    }
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").ok()
}

/// Fetches the profile and the whole completion list and replaces the cached rows.
pub async fn sync(clients: &ProviderClients, pool: &MySqlPool, progress: &Progress) -> Result<AccountSyncSummary, DatabaseError> {
    let client = clients.retroachievements.as_ref().ok_or_else(|| DatabaseError::Provider("retroachievements client unavailable (ra_username / ra_api_key in api.env)".to_string()))?;
    let user = client.username().to_string();
    let mut summary = AccountSyncSummary { user: user.clone(), ..Default::default() };
    progress.force(0, None, "RetroAchievements: profile").await;
    let (profile, _) = client.get_user_profile(&user).await?;
    summary.requests += 1;
    sqlx::query(
        "INSERT INTO ra_user_profile (ra_user, user_pic, member_since, rich_presence, last_game_id, total_points, total_softcore_points, total_true_points, motto, fetched_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, NOW()) ON DUPLICATE KEY UPDATE user_pic = VALUES(user_pic), member_since = VALUES(member_since), rich_presence = VALUES(rich_presence), \
         last_game_id = VALUES(last_game_id), total_points = VALUES(total_points), total_softcore_points = VALUES(total_softcore_points), total_true_points = VALUES(total_true_points), motto = VALUES(motto), fetched_at = NOW()",
    )
    .bind(&user)
    .bind(profile.user_pic.as_deref())
    .bind(profile.member_since.as_deref())
    .bind(profile.rich_presence.as_deref().map(|s| s.chars().take(512).collect::<String>()))
    .bind(profile.last_game_id)
    .bind(profile.total_points)
    .bind(profile.total_softcore_points)
    .bind(profile.total_true_points)
    .bind(profile.motto.as_deref().map(|s| s.chars().take(255).collect::<String>()))
    .execute(pool)
    .await?;

    // Console names for the cached catalogue (the game list rows cached before the name
    // column existed have none): one small call.
    progress.force(0, None, "RetroAchievements: consoles").await;
    let (consoles, _) = client.get_console_ids(false, false).await?;
    summary.requests += 1;
    for c in &consoles {
        sqlx::query("UPDATE retroachievements_games SET console_name = ? WHERE console_id = ? AND (console_name IS NULL OR console_name <> ?)").bind(&c.name).bind(c.id).bind(&c.name).execute(pool).await?;
    }

    let mut entries: Vec<RaCompletionEntry> = Vec::new();
    let mut total: Option<u32> = None;
    loop {
        let offset = entries.len() as u32;
        progress.force(offset as u64, total.map(u64::from), &format!("RetroAchievements: played games {offset} of {}", total.map_or("?".to_string(), |t| t.to_string()))).await;
        let (page, _) = client.get_user_completion_progress(&user, offset, 500).await?;
        summary.requests += 1;
        total = Some(page.total);
        let n = page.results.len();
        entries.extend(page.results);
        if n == 0 || entries.len() as u32 >= page.total {
            break;
        }
    }

    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM ra_user_progress WHERE ra_user = ?").bind(&user).execute(&mut *tx).await?;
    for chunk in entries.chunks(500) {
        let placeholders = vec!["(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NOW())"; chunk.len()].join(", ");
        let sql = format!(
            "INSERT INTO ra_user_progress (ra_user, game_id, title, console_id, console_name, image_icon, max_possible, num_awarded, num_awarded_hardcore, most_recent_awarded, highest_award_kind, highest_award_date, fetched_at) VALUES {placeholders}"
        );
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
        for e in chunk {
            q = q
                .bind(&user)
                .bind(e.game_id)
                .bind(e.title.chars().take(512).collect::<String>())
                .bind(e.console_id)
                .bind(e.console_name.as_deref())
                .bind(e.image_icon.as_deref())
                .bind(e.max_possible)
                .bind(e.num_awarded)
                .bind(e.num_awarded_hardcore)
                .bind(e.most_recent_awarded.as_deref().and_then(parse_ra_date))
                .bind(e.highest_award_kind.as_deref().filter(|k| !k.is_empty()))
                .bind(e.highest_award_date.as_deref().and_then(parse_ra_date));
        }
        q.execute(&mut *tx).await?;
    }
    tx.commit().await?;
    summary.games = entries.len() as u64;
    progress.force(summary.games, Some(summary.games), &format!("done: {} played game(s) for {user}", summary.games)).await;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genres_fold_onto_majors() {
        assert_eq!(genre_majors("2D Platforming, Shoot 'em Up").as_deref(), Some("Platforming,Shooter"));
        assert_eq!(genre_majors("  ").as_deref(), None);
    }

    #[test]
    fn ra_dates_parse() {
        assert_eq!(parse_ra_date("2023-05-01 12:34:56").unwrap().to_string(), "2023-05-01 12:34:56");
        assert_eq!(parse_ra_date("2024-01-02T03:04:05+00:00").unwrap().to_string(), "2024-01-02 03:04:05");
        assert_eq!(parse_ra_date("2024-01-02T12:00:00+09:00").unwrap().to_string(), "2024-01-02 03:00:00");
        assert!(parse_ra_date("yesterday").is_none());
    }
}
