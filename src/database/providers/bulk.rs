//! `full`/`by_platform` provider syncs (the `provider_bulk_sync` job): one big fetch per
//! platform, cached and linked, instead of per-item queries.

use std::collections::{HashMap, HashSet};

use sqlx::mysql::MySqlPool;
use sqlx::Row;

use crate::common::models::Platform;

use super::super::error::DatabaseError;
use super::super::jobs::Progress;
use super::super::matching::util::norm_key;
use super::super::platform_map;
use super::cache::{self, Confidence};
use super::raw_store::RawStore;
use super::{Provider, ProviderClients, Tier};

/// What a bulk sync fetches: only what's new/changed since the last complete sync (the
/// default -- no re-downloading what's already cached), or everything again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncMode {
    #[default]
    Diff,
    Redownload,
}

impl SyncMode {
    /// From a job payload's `"mode"` (`"diff"` default, `"redownload"`).
    pub fn from_payload(payload: &serde_json::Value) -> Self {
        match payload.get("mode").and_then(|v| v.as_str()) {
            Some("redownload") => Self::Redownload,
            _ => Self::Diff,
        }
    }
}

/// A platform's catalog releases by normalized name, loaded once, for exact-name linking
/// of bulk results (`link_exact_names`, the IGDB page loop).
pub struct ExactNameIndex {
    by_name: HashMap<String, Vec<(u64, Option<u64>)>>,
}

impl ExactNameIndex {
    pub async fn load(pool: &MySqlPool, platform: Platform) -> Result<Self, DatabaseError> {
        let rows = sqlx::query("SELECT r.id, r.norm_name, tr.title_id FROM `release` r LEFT JOIN title_releases tr ON tr.release_id = r.id WHERE r.deleted = FALSE AND r.app_platform_id = ?")
            .bind(platform.igdb_id())
            .fetch_all(pool)
            .await?;
        let mut by_name: HashMap<String, Vec<(u64, Option<u64>)>> = HashMap::new();
        for row in &rows {
            by_name.entry(row.try_get("norm_name")?).or_default().push((row.try_get("id")?, row.try_get("title_id")?));
        }
        Ok(Self { by_name })
    }

    /// The (release, title) pairs whose normalized name equals one of `names` (first name
    /// that matches wins).
    pub fn find(&self, names: &[String]) -> Option<&Vec<(u64, Option<u64>)>> {
        names.iter().map(|n| norm_key(n)).filter(|k| !k.is_empty()).find_map(|k| self.by_name.get(&k))
    }
}

/// Links provider games to the catalog on one platform by **exact** normalized name (the
/// game's title or any alternate equal to a release's `norm_name`), `Confidence::Name`,
/// never a fuzzy guess -- for bulk results, where one wrong first-candidate guess per game
/// would add up. One query for the platform's releases, then in-memory lookups, instead of
/// `link_by_name`'s per-game search. Release-tier providers link each matching release,
/// title-tier ones the releases' titles. Returns how many games linked.
pub async fn link_exact_names(pool: &MySqlPool, provider: Provider, platform: Platform, games: &[(String, Vec<String>)]) -> Result<u64, DatabaseError> {
    if games.is_empty() || platform == Platform::Unknown {
        return Ok(0);
    }
    let index = ExactNameIndex::load(pool, platform).await?;
    let release_tier = !matches!(provider.tier(), Tier::Title);
    let mut linked = 0u64;
    let mut titles_done: HashSet<(u64, String)> = HashSet::new();
    for (id, names) in games {
        let Some(targets) = index.find(names) else { continue };
        for (release_id, title_id) in targets {
            if release_tier {
                cache::link_release(pool, *release_id, provider, id, Confidence::Name).await?;
            } else if let Some(t) = title_id {
                if titles_done.insert((*t, id.clone())) {
                    cache::link_title(pool, *t, provider, id, Confidence::Name).await?;
                }
            }
        }
        linked += 1;
    }
    Ok(linked)
}

/// TheGamesDB titles carry HTML entities (`&#039;`, `&amp;`); undo the common ones before
/// comparing names.
pub fn decode_html_entities(s: &str) -> String {
    s.replace("&#039;", "'").replace("&#39;", "'").replace("&quot;", "\"").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// A TheGamesDB game as something to link: its names and the region it is for.
pub fn thegamesdb_candidate(game: &playmatch_deredis::providers::thegamesdb::model::TgdbApiGame) -> super::thegamesdb_region::Candidate {
    super::thegamesdb_region::Candidate { id: game.id.to_string(), names: thegamesdb_names(game), region_id: game.region_id, country_id: game.country_id }
}

/// Records which region each cached TheGamesDB game is for (0 when it says none).
pub async fn set_thegamesdb_regions(pool: &MySqlPool, games: &[&playmatch_deredis::providers::thegamesdb::model::TgdbApiGame]) -> Result<(), DatabaseError> {
    for game in games {
        sqlx::query("UPDATE thegamesdb_games SET region_id = ?, country_id = ? WHERE provider_id = ?").bind(game.region_id.unwrap_or(0).max(0)).bind(game.country_id.unwrap_or(0).max(0)).bind(game.id).execute(pool).await?;
    }
    Ok(())
}

/// A TheGamesDB game's names for `link_exact_names`: its title and alternates.
pub fn thegamesdb_names(game: &playmatch_deredis::providers::thegamesdb::model::TgdbApiGame) -> Vec<String> {
    game.game_title.iter().chain(game.alternates.iter().flatten()).map(|n| decode_html_entities(n)).collect()
}

/// TheGamesDB platform id -> app platform, from `/v1/Platforms` (one call of the monthly
/// allowance), cached as `<provider cache>/thegamesdb/platforms_list.json` for 30 days
/// regardless of the raw-retention setting. Names/aliases map through
/// `platform_map::platform_from_dat_name`; unmapped platforms are left out.
pub async fn thegamesdb_platform_map(clients: &ProviderClients, store: &RawStore) -> Result<HashMap<i64, Platform>, DatabaseError> {
    Ok(thegamesdb_platforms(clients, store)
        .await?
        .into_iter()
        .filter_map(|p| {
            let mapped = [p.name.as_deref(), p.alias.as_deref()].into_iter().flatten().map(platform_map::platform_from_dat_name).find(|x| *x != Platform::Unknown)?;
            Some((p.id, mapped))
        })
        .collect())
}

/// TheGamesDB platform id -> (its own name, the app platform it maps to).
pub async fn thegamesdb_platform_info(clients: &ProviderClients, store: &RawStore) -> Result<HashMap<i64, (Option<String>, Option<Platform>)>, DatabaseError> {
    Ok(thegamesdb_platforms(clients, store)
        .await?
        .into_iter()
        .map(|p| {
            let mapped = [p.name.as_deref(), p.alias.as_deref()].into_iter().flatten().map(platform_map::platform_from_dat_name).find(|x| *x != Platform::Unknown);
            (p.id, (p.name.clone(), mapped))
        })
        .collect())
}

/// Records which platform cached TheGamesDB games are on (`thegamesdb_games` platform
/// columns; see `migrations/0007`).
pub async fn set_thegamesdb_platforms(pool: &MySqlPool, info: &HashMap<i64, (Option<String>, Option<Platform>)>, games: &[(i64, Option<i64>)]) -> Result<(), DatabaseError> {
    for chunk in games.chunks(1000) {
        let mut sql = String::from("UPDATE thegamesdb_games SET remote_platform_id = CASE provider_id");
        for _ in chunk {
            sql.push_str(" WHEN ? THEN ?");
        }
        sql.push_str(" END, platform_name = CASE provider_id");
        for _ in chunk {
            sql.push_str(" WHEN ? THEN ?");
        }
        sql.push_str(" END, app_platform_id = CASE provider_id");
        for _ in chunk {
            sql.push_str(" WHEN ? THEN ?");
        }
        sql.push_str(&format!(" END WHERE provider_id IN ({})", vec!["?"; chunk.len()].join(",")));
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
        // A game without a platform is recorded as platform 0, so a backfill can tell it
        // from a row that was never filled.
        for (id, p) in chunk {
            q = q.bind(*id).bind(p.unwrap_or(0));
        }
        for (id, p) in chunk {
            q = q.bind(*id).bind(p.and_then(|p| info.get(&p)).and_then(|i| i.0.clone()));
        }
        for (id, p) in chunk {
            q = q.bind(*id).bind(p.and_then(|p| info.get(&p)).and_then(|i| i.1).map(|x| x as u16));
        }
        for (id, _) in chunk {
            q = q.bind(*id);
        }
        q.execute(pool).await?;
    }
    Ok(())
}

async fn thegamesdb_platforms(clients: &ProviderClients, store: &RawStore) -> Result<Vec<playmatch_deredis::providers::thegamesdb::model::TgdbPlatform>, DatabaseError> {
    use playmatch_deredis::providers::thegamesdb::model::TgdbPlatform;
    let path = store.root().join("thegamesdb").join("platforms_list.json");
    let fresh = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() < 30 * 86_400);
    let platforms: Vec<TgdbPlatform> = if fresh {
        serde_json::from_slice(&std::fs::read(&path)?)?
    } else {
        let client = clients.thegamesdb.as_ref().ok_or_else(|| DatabaseError::Provider("thegamesdb client unavailable".to_string()))?;
        let list = client.platforms().await?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, serde_json::to_vec_pretty(&list)?)?;
        list
    };
    Ok(platforms)
}

/// `by_platform` sync for TheGamesDB: `/v1/Games/ByPlatformID`, 20 games a page, every
/// TheGamesDB platform that maps to `platform`; each page costs one call of the key's
/// monthly allowance (stops with an error once it's used up). Linked by exact name.
///
/// `SyncMode::Diff` skips the paging (which costs monthly allowance) when a full-dump
/// import already covered this platform completely: the dump diff keeps it current.
pub async fn sync_thegamesdb_platform(clients: &ProviderClients, pool: &MySqlPool, store: &RawStore, platform: Platform, max_pages: Option<u32>, mode: SyncMode, progress: &Progress) -> Result<BulkSummary, DatabaseError> {
    let client = clients.thegamesdb.as_ref().ok_or_else(|| DatabaseError::Provider("thegamesdb client unavailable".to_string()))?;
    if mode == SyncMode::Diff {
        let full = super::sync_state::get(pool, Provider::TheGamesDb, None).await?.is_some_and(|s| s.complete);
        let this = super::sync_state::get(pool, Provider::TheGamesDb, Some(platform)).await?;
        if full && this.as_ref().is_some_and(|s| s.complete) {
            progress.force(1, Some(1), &format!("{platform:?}: covered by the full TheGamesDB dump -- run a full diff to update, or redownload to page it again")).await;
            return Ok(BulkSummary { provider: Some(Provider::TheGamesDb), platform: Some(platform), remote_id: Some("covered by dump".to_string()), games: this.map_or(0, |s| s.entries), ..Default::default() });
        }
    }
    let map = thegamesdb_platform_map(clients, store).await?;
    let info = thegamesdb_platform_info(clients, store).await?;
    let ids: Vec<i64> = map.iter().filter(|(_, p)| **p == platform).map(|(id, _)| *id).collect();
    if ids.is_empty() {
        return Err(DatabaseError::Provider(format!("no TheGamesDB platform maps to {platform:?}")));
    }
    let mut summary = BulkSummary { provider: Some(Provider::TheGamesDb), platform: Some(platform), remote_id: Some(ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")), ..Default::default() };
    let mut found: Vec<super::thegamesdb_region::Candidate> = Vec::new();
    let mut complete = true;
    // TheGamesDB has no count to ask for (a page only says whether another follows), so
    // the total is what the last complete run found: an estimate, and said to be one.
    let expected = super::sync_state::get(pool, Provider::TheGamesDb, Some(platform)).await?.filter(|s| s.complete && s.entries > 0).map(|s| s.entries);
    for tgdb_platform in ids {
        let mut page = 1u32;
        loop {
            if max_pages.is_some_and(|m| summary.pages >= m) {
                complete = false;
                break;
            }
            let (games, more) = client.games_by_platform_page(tgdb_platform, page).await?;
            summary.pages += 1;
            for game in &games {
                let raw = serde_json::to_value(game)?;
                let title = game.game_title.as_deref().map(decode_html_entities);
                cache::upsert_game(pool, store, Provider::TheGamesDb, &game.id.to_string(), title.as_deref(), &raw).await?;
                summary.games += 1;
                found.push(thegamesdb_candidate(game));
            }
            let placed: Vec<(i64, Option<i64>)> = games.iter().map(|g| (g.id, g.platform.or(Some(tgdb_platform)))).collect();
            set_thegamesdb_platforms(pool, &info, &placed).await?;
            set_thegamesdb_regions(pool, &games.iter().collect::<Vec<_>>()).await?;
            let allowance = client.known_remaining_allowance().map(|a| format!(", {a} API calls left this month")).unwrap_or_default();
            progress.report(summary.games, expected.map(|n| n.max(summary.games)), &format!("{platform:?}: page {page}, {}{allowance}", games_of(summary.games, expected, true))).await;
            if !more || games.is_empty() {
                break;
            }
            page += 1;
        }
    }
    progress.force(summary.games, Some(summary.games), &format!("{platform:?}: linking {} games by name", summary.games)).await;
    let regions = super::thegamesdb_region::TgdbRegions::load(clients, store.root()).await?;
    let priority = super::super::settings::region_priority(pool).await?;
    summary.linked = super::thegamesdb_region::link_platform(pool, &regions, &priority, platform, &found).await?;
    // The total is only known from a complete run (or kept from an earlier full dump).
    super::sync_state::record(pool, Provider::TheGamesDb, Some(platform), summary.remote_id.as_deref(), summary.games, complete.then_some(summary.games), complete).await?;
    Ok(summary)
}

/// `by_platform` sync for EmuReady: `games.get` for the platform's system, 50 a page until a
/// short page. Linked by exact name.
///
/// `SyncMode::Diff` asks for the system's game count first (a one-item page) and skips the
/// paging when it equals what the last complete sync fetched.
pub async fn sync_emuready_platform(clients: &ProviderClients, pool: &MySqlPool, store: &RawStore, platform: Platform, max_pages: Option<u32>, mode: SyncMode, progress: &Progress) -> Result<BulkSummary, DatabaseError> {
    const PAGE: i64 = 50;
    let client = clients.emuready.as_ref().ok_or_else(|| DatabaseError::Provider("emuready client unavailable".to_string()))?;
    let system = cache::emuready_system_id(clients, platform).await?.ok_or_else(|| DatabaseError::Provider(format!("no EmuReady system maps to {platform:?}")))?;
    // The system's own count (a one-item page): what a diff compares, and the total the
    // progress is out of. Best effort.
    let expected = client.count_games(&system).await.ok().flatten().map(|n| n.max(0) as u64);
    if mode == SyncMode::Diff {
        let previous = super::sync_state::get(pool, Provider::EmuReady, Some(platform)).await?;
        if let (Some(prev), Some(total)) = (previous.filter(|s| s.complete), expected) {
            if total == prev.entries {
                progress.force(1, Some(1), &format!("{platform:?}: unchanged ({total} games)")).await;
                return Ok(BulkSummary { provider: Some(Provider::EmuReady), platform: Some(platform), remote_id: Some(format!("{system} (unchanged)")), games: prev.entries, ..Default::default() });
            }
        }
    }
    let mut summary = BulkSummary { provider: Some(Provider::EmuReady), platform: Some(platform), remote_id: Some(system.clone()), ..Default::default() };
    let mut found: Vec<(String, Vec<String>)> = Vec::new();
    let mut page = 1i64;
    let mut complete = false;
    loop {
        if max_pages.is_some_and(|m| summary.pages >= m) {
            break;
        }
        let games = client.list_games_page(&system, page, PAGE).await?;
        summary.pages += 1;
        for game in &games {
            let raw = serde_json::to_value(game)?;
            cache::upsert_game(pool, store, Provider::EmuReady, &game.id, Some(&game.title), &raw).await?;
            summary.games += 1;
            found.push((game.id.clone(), vec![game.title.clone()]));
        }
        progress.report(summary.games, expected.map(|n| n.max(summary.games)), &format!("{platform:?}: page {page}, {}", games_of(summary.games, expected, false))).await;
        if (games.len() as i64) < PAGE {
            complete = true;
            break;
        }
        page += 1;
    }
    progress.force(summary.games, Some(summary.games), &format!("{platform:?}: linking {} games by name", summary.games)).await;
    summary.linked = link_exact_names(pool, Provider::EmuReady, platform, &found).await?;
    super::sync_state::record(pool, Provider::EmuReady, Some(platform), Some(&system), summary.games, complete.then_some(summary.games), complete).await?;
    Ok(summary)
}

/// `120 of 4310 games`, or `120 games` when the provider gives no total. `estimate` is for
/// a total that is the last run's rather than the provider's own: `120 of about 4310 games`.
fn games_of(done: u64, total: Option<u64>, estimate: bool) -> String {
    match total {
        Some(total) if total >= done => format!("{done} of {}{total} games", if estimate { "about " } else { "" }),
        _ => format!("{done} games"),
    }
}

#[derive(Debug, Clone, Default)]
pub struct BulkSummary {
    pub provider: Option<Provider>,
    pub platform: Option<Platform>,
    pub remote_id: Option<String>,
    pub games: u64,
    pub hashes: u64,
    pub linked: u64,
    pub pages: u32,
    /// Art files newly linked (covers/box fronts).
    pub media: u64,
}

/// RA's console id for `platform`, via `API_GetConsoleIDs` (cached per call site).
pub async fn ra_console_id_for(clients: &ProviderClients, platform: Platform) -> Result<Option<(u32, String)>, DatabaseError> {
    let client = clients.retroachievements.as_ref().ok_or_else(|| DatabaseError::Provider("retroachievements client unavailable".to_string()))?;
    let (consoles, _) = client.get_console_ids(true, true).await?;
    Ok(consoles.into_iter().find(|c| platform_map::platform_from_ra_console_name(&c.name) == platform).map(|c| (c.id, c.name)))
}

/// `by_platform` sync for RetroAchievements: `API_GetGameList(console, withHashes)` once,
/// every game into `retroachievements_games`, every MD5 into `retroachievements_hashes`,
/// and an exact release link for every MD5 the catalog already knows.
///
/// `SyncMode::Diff` first fetches the console's list without hashes (`h=0`, small) and
/// compares it with the cached games (ids and achievement counts); only when something is
/// new or changed does it fetch the full list with hashes.
pub async fn sync_retroachievements_console(clients: &ProviderClients, pool: &MySqlPool, store: &RawStore, console_id: u32, platform: Option<Platform>, mode: SyncMode) -> Result<BulkSummary, DatabaseError> {
    let client = clients.retroachievements.as_ref().ok_or_else(|| DatabaseError::Provider("retroachievements client unavailable".to_string()))?;
    if mode == SyncMode::Diff {
        let cached = sqlx::query("SELECT provider_id, num_achievements, set_modified_at FROM retroachievements_games WHERE console_id = ? AND deleted = FALSE").bind(console_id).fetch_all(pool).await?;
        if !cached.is_empty() {
            type Known = (Option<u32>, Option<chrono::NaiveDateTime>);
            let known: HashMap<u32, Known> = cached
                .iter()
                .map(|r| Ok((r.try_get::<u32, _>("provider_id")?, (r.try_get::<Option<u32>, _>("num_achievements")?, r.try_get::<Option<chrono::NaiveDateTime>, _>("set_modified_at")?))))
                .collect::<Result<_, DatabaseError>>()?;
            let (light, _) = client.get_game_list(console_id, false, false).await?;
            // Unchanged only when every game is there with the same count and the same set
            // date. A game cached before set dates were recorded (none here, one in the list)
            // counts as changed, so the full list is read once and records them.
            let unchanged = light.len() == known.len()
                && light.iter().all(|e| known.get(&e.id).is_some_and(|(n, set)| *n == e.num_achievements && *set == e.date_modified.as_deref().and_then(super::ra_account::parse_ra_date)));
            if unchanged {
                let games = light.len() as u64;
                if platform.is_some() {
                    super::sync_state::record(pool, Provider::RetroAchievements, platform, Some(&console_id.to_string()), games, Some(games), true).await?;
                }
                return Ok(BulkSummary { provider: Some(Provider::RetroAchievements), platform, remote_id: Some(format!("{console_id} (unchanged)")), games, pages: 1, ..Default::default() });
            }
        }
    }
    let (entries, raw) = client.get_game_list(console_id, true, false).await?;
    store.save_json(Provider::RetroAchievements, &format!("console_{console_id}_gamelist"), &raw)?;
    let mut summary = BulkSummary { provider: Some(Provider::RetroAchievements), platform, remote_id: Some(console_id.to_string()), pages: 1, ..Default::default() };

    let mut tx = pool.begin().await?;
    for entry in &entries {
        let raw_entry = serde_json::to_value(entry)?;
        // Per-game raw file only under keep_all/keep_latest -- the list response above
        // already holds everything; store a compact per-game copy for the cache row.
        let raw_path = store.save_json(Provider::RetroAchievements, &entry.id.to_string(), &raw_entry)?;
        sqlx::query(
            "INSERT INTO retroachievements_games (provider_id, console_id, console_name, image_icon, raw_response_path, raw_format, title, num_achievements, set_modified_at, fetched_at, deleted) VALUES (?, ?, ?, ?, ?, 'json', ?, ?, ?, NOW(), FALSE) \
             ON DUPLICATE KEY UPDATE console_id = VALUES(console_id), console_name = COALESCE(VALUES(console_name), console_name), image_icon = COALESCE(VALUES(image_icon), image_icon), raw_response_path = VALUES(raw_response_path), title = VALUES(title), num_achievements = VALUES(num_achievements), set_modified_at = COALESCE(VALUES(set_modified_at), set_modified_at), fetched_at = NOW(), deleted = FALSE",
        )
        .bind(entry.id)
        .bind(entry.console_id)
        .bind(entry.console_name.as_deref())
        .bind(entry.image_icon.as_deref())
        .bind(raw_path.map(|p| p.display().to_string()))
        .bind(&entry.title)
        .bind(entry.num_achievements)
        .bind(entry.date_modified.as_deref().and_then(super::ra_account::parse_ra_date))
        .execute(&mut *tx)
        .await?;
        summary.games += 1;
        for md5 in entry.hashes.iter().flatten() {
            let md5 = md5.trim().to_ascii_lowercase();
            if md5.len() != 32 {
                continue;
            }
            sqlx::query("INSERT INTO retroachievements_hashes (md5, ra_game_id, console_id, fetched_at) VALUES (?, ?, ?, NOW()) ON DUPLICATE KEY UPDATE ra_game_id = VALUES(ra_game_id), console_id = VALUES(console_id), fetched_at = NOW()")
                .bind(&md5)
                .bind(entry.id)
                .bind(entry.console_id)
                .execute(&mut *tx)
                .await?;
            summary.hashes += 1;
        }
    }
    tx.commit().await?;

    // Link: RA md5 == the plain-file MD5 the DATs carry for most cartridge systems.
    let rows = sqlx::query("SELECT h.content_id, rh.ra_game_id FROM retroachievements_hashes rh JOIN hashes h ON h.algorithm IN ('md5', 'ra_hash') AND h.value_hex = rh.md5 WHERE rh.console_id = ? AND h.content_id IS NOT NULL")
        .bind(console_id)
        .fetch_all(pool)
        .await?;
    for row in rows {
        let content_id: u64 = row.try_get("content_id")?;
        let ra_game_id: u32 = row.try_get("ra_game_id")?;
        summary.linked += cache::link_for_content(pool, content_id, Provider::RetroAchievements, &ra_game_id.to_string(), Confidence::Exact).await? as u64;
    }
    // And roms whose RA hash a scan worked out (NES, SNES, disc systems, ...).
    summary.linked += cache::link_ra_rom_hashes(pool, None, Some(console_id)).await?;
    // A console's game list is always the whole list.
    if platform.is_some() {
        super::sync_state::record(pool, Provider::RetroAchievements, platform, Some(&console_id.to_string()), summary.games, Some(summary.games), true).await?;
    }
    Ok(summary)
}

/// `full` sync for RetroAchievements: every game system RA lists (`API_GetConsoleIDs`),
/// one `sync_retroachievements_console` each -- ~70 list calls, the RA client's own spacing
/// between them. A console that fails is logged and skipped; the summary adds up the rest,
/// with `pages` = consoles synced.
pub async fn sync_retroachievements_all(clients: &ProviderClients, pool: &MySqlPool, store: &RawStore, mode: SyncMode, progress: &Progress) -> Result<(BulkSummary, Vec<String>), DatabaseError> {
    let client = clients.retroachievements.as_ref().ok_or_else(|| DatabaseError::Provider("retroachievements client unavailable".to_string()))?;
    let (consoles, _) = client.get_console_ids(true, true).await?;
    let mut total = BulkSummary { provider: Some(Provider::RetroAchievements), ..Default::default() };
    let mut failed = Vec::new();
    let count = consoles.len() as u64;
    for (i, console) in consoles.into_iter().enumerate() {
        progress.report(i as u64, Some(count), &format!("console {} of {count}: {} ({} games so far)", i + 1, console.name, total.games)).await;
        let platform = Some(platform_map::platform_from_ra_console_name(&console.name)).filter(|p| *p != Platform::Unknown);
        match sync_retroachievements_console(clients, pool, store, console.id, platform, mode).await {
            Ok(s) => {
                log::info!("RA full sync: {} ({}): {} games, {} hashes, {} linked", console.name, console.id, s.games, s.hashes, s.linked);
                total.games += s.games;
                total.hashes += s.hashes;
                total.linked += s.linked;
                total.pages += 1;
            }
            Err(e) => {
                log::warn!("RA full sync: {} ({}) failed: {e}", console.name, console.id);
                failed.push(console.name);
            }
        }
    }
    super::sync_state::record(pool, Provider::RetroAchievements, None, None, total.games, Some(total.games), failed.is_empty()).await?;
    Ok((total, failed))
}

/// `by_platform` sync for IGDB: pages of 500 games for one platform, each cached and
/// linked by exact name (`ExactNameIndex`, `Confidence::Name`) to a title on that platform,
/// metadata and box art applied to what linked. `max_pages` caps the fetch.
///
/// `SyncMode::Diff` (the default) asks IGDB only for games whose `updated_at` is after the
/// last complete sync of this platform (minus a day of slack), so a re-sync fetches just
/// what changed; the first sync, or `Redownload`, pages through everything.
pub async fn sync_igdb_platform(clients: &ProviderClients, pool: &MySqlPool, store: &RawStore, platform: Platform, max_pages: Option<u32>, media: Option<(&reqwest::Client, &std::path::Path)>, mode: SyncMode, progress: &Progress) -> Result<BulkSummary, DatabaseError> {
    let client = clients.igdb.as_ref().ok_or_else(|| DatabaseError::Provider("igdb client unavailable".to_string()))?;
    // App-only ids sync under their IGDB sibling (Famicom Disk System -> IGDB 51, ...).
    let igdb_platform = cache::sibling_igdb_platforms(platform.igdb_id())[0];
    if igdb_platform == 0 || igdb_platform >= 2048 {
        return Err(DatabaseError::Provider(format!("{platform:?} has no IGDB platform id")));
    }
    let mut summary = BulkSummary { provider: Some(Provider::Igdb), platform: Some(platform), remote_id: Some(igdb_platform.to_string()), ..Default::default() };
    let previous = super::sync_state::get(pool, Provider::Igdb, Some(platform)).await?;
    let since: Option<i64> = match (mode, &previous) {
        (SyncMode::Diff, Some(p)) if p.complete => Some(p.synced_at.and_utc().timestamp() - 86_400),
        _ => None,
    };
    // IGDB's own counts (one request each) -- the denominators for coverage and progress.
    // Best effort.
    let platform_total = client.count_games(Some(igdb_platform as i32), None).await.ok().map(|n| n.max(0) as u64);
    let to_fetch = match since {
        Some(t) => client.count_games(Some(igdb_platform as i32), Some(t)).await.ok().map(|n| n.max(0) as u64),
        None => platform_total,
    };
    if let Ok(all) = client.count_games(None, None).await {
        super::sync_state::record_total(pool, Provider::Igdb, all.max(0) as u64).await?;
    }
    if since.is_some() {
        progress.force(0, to_fetch, &format!("{platform:?}: diff since the last full sync -- {} changed game(s) to fetch", to_fetch.map_or("?".to_string(), |n| n.to_string()))).await;
    }
    let index = ExactNameIndex::load(pool, platform).await?;
    const PAGE: usize = 500;
    let mut offset = 0usize;
    let mut complete = false;
    loop {
        if to_fetch == Some(0) {
            complete = true;
            break;
        }
        if max_pages.map(|m| summary.pages >= m).unwrap_or(false) {
            break;
        }
        let games = client.get_games_by_platform(igdb_platform as i32, since, PAGE, offset).await?;
        summary.pages += 1;
        let mut linked: Vec<(u64, &playmatch_deredis::providers::igdb::model::Game)> = Vec::new();
        for game in &games {
            let raw = serde_json::to_value(game)?;
            cache::upsert_game(pool, store, Provider::Igdb, &game.id.to_string(), Some(&game.name), &raw).await?;
            sqlx::query("UPDATE igdb_games SET updated_at = FROM_UNIXTIME(?) WHERE provider_id = ?").bind(game.updated_at.timestamp()).bind(game.id).execute(pool).await?;
            summary.games += 1;
            let names = vec![game.name.clone()];
            let mut titles: Vec<u64> = index.find(&names).map(|t| t.iter().filter_map(|(_, title)| *title).collect()).unwrap_or_default();
            titles.sort_unstable();
            titles.dedup();
            for title_id in titles {
                if cache::link_title(pool, title_id, Provider::Igdb, &game.id.to_string(), Confidence::Name).await? {
                    linked.push((title_id, game));
                }
            }
            if linked.last().is_some_and(|(_, g)| g.id == game.id) {
                summary.linked += 1;
            }
        }
        // Metadata for every title this page linked: ids -> names in a few list requests;
        // covers the same way (`get_covers_by_id`, 100 per request), each linked to the
        // title's releases.
        if !linked.is_empty() {
            let mut names = super::metadata::IgdbNames::new();
            let page_games: Vec<playmatch_deredis::providers::igdb::model::Game> = linked.iter().map(|(_, g)| (*g).clone()).collect();
            names.resolve(clients, &page_games).await?;
            for (title_id, game) in &linked {
                super::metadata::apply_igdb_game(pool, &names, *title_id, game).await?;
            }
            if let Some((http, media_dir)) = media.as_ref() {
                let covers = super::media::igdb_cover_urls(clients, &page_games).await?;
                for (title_id, game) in &linked {
                    let Some(url) = game.cover.and_then(|c| covers.get(&c)) else { continue };
                    let targets = super::media::release_targets(pool, *title_id, Provider::Igdb, &game.id.to_string()).await?;
                    summary.media += super::media::attach_media(http, pool, media_dir, Provider::Igdb, Some(&game.id.to_string()), url, "box", Some("front"), &targets).await? as u64;
                }
                summary.media += super::igdb_localized::attach_for_titles(http, pool, media_dir, clients, store.root(), &linked).await? as u64;
                summary.media += super::media::attach_igdb_extras(http, pool, media_dir, clients, &linked).await? as u64;
            }
        }
        progress.report(summary.games, to_fetch, &format!("{platform:?}: page {}, {}{}, {} linked", summary.pages, games_of(summary.games, to_fetch, false), if since.is_some() { " changed" } else { "" }, summary.linked)).await;
        if games.len() < PAGE {
            complete = true;
            break;
        }
        offset += PAGE;
    }
    // A complete diff run leaves the platform complete: the cache holds IGDB's whole list.
    let entries = match (since, complete) {
        (Some(_), true) => platform_total.unwrap_or(summary.games),
        _ => summary.games,
    };
    super::sync_state::record(pool, Provider::Igdb, Some(platform), Some(&igdb_platform.to_string()), entries, platform_total.or(complete.then_some(entries)), complete).await?;
    Ok(summary)
}
