//! The RetroAchievements tab: the account's status block, its games by award level
//! (RAChecker's profile/mastery view), one game's achievements, and RA's images -- game
//! icon / title screen / in-game screenshot / box art, the account avatar and a composed
//! mosaic of a game's achievement badges -- downloaded once into the image dir (else the
//! provider cache); the mosaics themselves are composed into the provider cache.
//!
//! The lists cover RetroAchievements' whole game list -- every game with achievements in the
//! locally cached RA catalogue (`retroachievements_games`, filled by RA bulk syncs) -- plus
//! the account's played games (`ra_user_progress`, refreshed by the `ra_account_sync` job)
//! and the local ones (RA game ids linked to a release the user holds a local game for).
//! `source`: **all** = every RA game, the held ones first; **local** = only the ones held
//! here. Titles is everything; To be played is Titles
//! minus mastered / completed / beaten / in progress, never-started games included. Award levels are RetroAchievements' own:
//! `mastered` (hardcore complete), `completed` (softcore complete), `beaten-hardcore` /
//! `beaten-softcore`; each list shows a game at its highest award.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::database::providers::retroachievements::RaClient;
use crate::database::settings as db_settings;

use super::jobs::{JobRequest, PRIORITY_INTERACTIVE};
use super::{App, Error, Result};

/// The account progress counts as stale after this long (the tab then queues a refresh).
const STALE_MINUTES: i64 = 60;

/// The RetroAchievements client, holding the provider set it belongs to so it outlives a
/// credential change made while a request is running.
struct Ra(std::sync::Arc<crate::database::providers::ProviderClients>);

impl std::ops::Deref for Ra {
    type Target = RaClient;
    fn deref(&self) -> &RaClient {
        self.0.retroachievements.as_ref().expect("checked when built")
    }
}

fn client(app: &App) -> Result<Ra> {
    let clients = app.clients();
    if clients.retroachievements.is_none() {
        return Err(Error::unavailable("RetroAchievements has no usable credentials (ra_username / ra_api_key in api.env)"));
    }
    Ok(Ra(clients))
}

use crate::database::providers::ra_account::LOCAL_RA;

// ---------------------------------------------------------------------------
// status block
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Profile {
    pub user: String,
    pub has_avatar: bool,
    pub member_since: Option<String>,
    pub rich_presence: Option<String>,
    pub motto: Option<String>,
    pub last_game_id: Option<u32>,
    pub total_points: Option<u64>,
    pub total_softcore_points: Option<u64>,
    pub total_true_points: Option<u64>,
    pub fetched_at: chrono::NaiveDateTime,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Counts {
    /// Every local game in the collection.
    pub local_games: i64,
    /// Distinct RA games among them (local games linked to an RA set).
    pub local_ra_games: i64,
    /// Games the account has played.
    pub account_games: i64,
    /// RA games in the local RA catalogue cache that have achievements.
    pub ra_catalogue: i64,
    pub mastered: i64,
    pub completed: i64,
    pub beaten: i64,
    pub in_progress: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncState {
    pub job_id: u64,
    pub status: String,
    pub progress_message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub available: bool,
    pub user: Option<String>,
    pub profile: Option<Profile>,
    pub counts: Counts,
    /// No profile yet, or older than an hour.
    pub stale: bool,
    /// The account sync job, when one is queued or running.
    pub sync: Option<SyncState>,
    pub include_beaten_default: bool,
    pub source_default: String,
    /// RA consoles seen in the cached catalogue / the account, for the console filter.
    pub consoles: Vec<(u32, String)>,
    /// The 19 major genres the genre filter offers.
    pub genres: Vec<&'static str>,
    /// console | genre -- which base filter the lists start with.
    pub base_filter_default: String,
    /// Genre coverage: RA games with achievements asked for their genre / total, and the
    /// same for local games.
    pub genre_checked: i64,
    pub genre_checked_local: i64,
    /// The genre fetch job, when one is queued or running.
    pub genre_job: Option<SyncState>,
}

pub async fn status(app: &App) -> Result<Status> {
    let pool = &app.pool;
    let include_beaten_default = db_settings::get::<bool>(pool, db_settings::KEY_RA_INCLUDE_BEATEN).await?.unwrap_or(true);
    let source_default = db_settings::get::<String>(pool, db_settings::KEY_RA_SOURCE).await?.map(|s| normalize_source(&s).to_string()).unwrap_or_else(|| "all".to_string());
    let user = app.clients().retroachievements.as_ref().map(|c| c.username().to_string());
    let profile = match &user {
        Some(u) => sqlx::query("SELECT * FROM ra_user_profile WHERE ra_user = ?").bind(u).fetch_optional(pool).await?.map(|r| -> Result<Profile> {
            Ok(Profile {
                user: u.clone(),
                has_avatar: r.try_get::<Option<String>, _>("user_pic")?.is_some(),
                member_since: r.try_get("member_since")?,
                rich_presence: r.try_get("rich_presence")?,
                motto: r.try_get("motto")?,
                last_game_id: r.try_get("last_game_id")?,
                total_points: r.try_get("total_points")?,
                total_softcore_points: r.try_get("total_softcore_points")?,
                total_true_points: r.try_get("total_true_points")?,
                fetched_at: r.try_get("fetched_at")?,
            })
        }).transpose()?,
        None => None,
    };
    let stale = profile.as_ref().is_none_or(|p| (chrono::Utc::now().naive_utc() - p.fetched_at).num_minutes() >= STALE_MINUTES);
    let one = |sql: String| async move { sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql)).fetch_one(pool).await };
    let u = user.clone().unwrap_or_default().replace('\'', "''");
    let counts = Counts {
        local_games: one("SELECT COUNT(*) FROM local_game WHERE deleted = FALSE".to_string()).await?,
        local_ra_games: one(format!("SELECT COUNT(*) FROM ({LOCAL_RA}) l")).await?,
        account_games: one(format!("SELECT COUNT(*) FROM ra_user_progress WHERE ra_user = '{u}'")).await?,
        ra_catalogue: one("SELECT COUNT(*) FROM retroachievements_games WHERE deleted = FALSE AND num_achievements > 0".to_string()).await?,
        mastered: one(format!("SELECT COUNT(*) FROM ra_user_progress WHERE ra_user = '{u}' AND highest_award_kind = 'mastered'")).await?,
        completed: one(format!("SELECT COUNT(*) FROM ra_user_progress WHERE ra_user = '{u}' AND highest_award_kind = 'completed'")).await?,
        beaten: one(format!("SELECT COUNT(*) FROM ra_user_progress WHERE ra_user = '{u}' AND highest_award_kind LIKE 'beaten%'")).await?,
        in_progress: one(format!("SELECT COUNT(*) FROM ra_user_progress WHERE ra_user = '{u}' AND num_awarded > 0 AND COALESCE(highest_award_kind, '') NOT IN ('mastered', 'completed')")).await?,
    };
    let sync = sqlx::query("SELECT id, status, progress_message FROM jobs WHERE job_type = 'ra_account_sync' AND status IN ('pending', 'in_progress') ORDER BY id LIMIT 1")
        .fetch_optional(pool)
        .await?
        .map(|r| -> Result<SyncState> { Ok(SyncState { job_id: r.try_get("id")?, status: r.try_get("status")?, progress_message: r.try_get("progress_message")? }) })
        .transpose()?;
    let consoles = sqlx::query(
        "SELECT console_id, MAX(console_name) AS name FROM (SELECT console_id, console_name FROM retroachievements_games WHERE console_id IS NOT NULL AND console_name IS NOT NULL \
         UNION ALL SELECT console_id, console_name FROM ra_user_progress WHERE ra_user = ? AND console_id IS NOT NULL AND console_name IS NOT NULL) c GROUP BY console_id ORDER BY name",
    )
    .bind(user.clone().unwrap_or_default())
    .fetch_all(pool)
    .await?
    .iter()
    .map(|r| -> Result<(u32, String)> { Ok((r.try_get("console_id")?, r.try_get("name")?)) })
    .collect::<Result<Vec<_>>>()?;
    let base_filter_default = db_settings::get::<String>(pool, db_settings::KEY_RA_BASE_FILTER).await?.unwrap_or_else(|| "console".to_string());
    let genre_checked = one("SELECT COUNT(*) FROM retroachievements_games WHERE deleted = FALSE AND num_achievements > 0 AND genre_checked_at IS NOT NULL".to_string()).await?;
    let genre_checked_local = one(format!("SELECT COUNT(*) FROM ({LOCAL_RA}) l JOIN retroachievements_games rg ON rg.provider_id = l.game_id WHERE rg.genre_checked_at IS NOT NULL")).await?;
    let genre_job = sqlx::query("SELECT id, status, progress_message FROM jobs WHERE job_type = 'ra_genre_fetch' AND status IN ('pending', 'in_progress') ORDER BY id LIMIT 1")
        .fetch_optional(pool)
        .await?
        .map(|r| -> Result<SyncState> { Ok(SyncState { job_id: r.try_get("id")?, status: r.try_get("status")?, progress_message: r.try_get("progress_message")? }) })
        .transpose()?;
    Ok(Status {
        available: user.is_some(),
        user,
        profile,
        counts,
        stale,
        sync,
        include_beaten_default,
        source_default,
        consoles,
        genres: crate::database::providers::ra_genre::MAJOR_GENRES.to_vec(),
        base_filter_default,
        genre_checked,
        genre_checked_local,
        genre_job,
    })
}

/// Queues a genre fetch: `all` = RetroAchievements' whole list, else local games only.
pub async fn fetch_genres(app: &App, all: bool) -> Result<Option<u64>> {
    client(app)?;
    let queued = sqlx::query("SELECT 1 FROM jobs WHERE job_type = 'ra_genre_fetch' AND status IN ('pending', 'in_progress') LIMIT 1").fetch_optional(&app.pool).await?.is_some();
    if queued {
        return Ok(None);
    }
    let request = JobRequest::RaGenreFetch(super::jobs::RaGenreFetchRequest { scope: if all { "all" } else { "local" }.to_string(), limit: None });
    Ok(Some(super::jobs::enqueue_with_priority(app, &request, 0).await?))
}

/// Queues an account refresh at interactive priority (none when one is already queued).
pub async fn refresh(app: &App) -> Result<Option<u64>> {
    client(app)?;
    let queued = sqlx::query("SELECT 1 FROM jobs WHERE job_type = 'ra_account_sync' AND status IN ('pending', 'in_progress') LIMIT 1").fetch_optional(&app.pool).await?.is_some();
    if queued {
        return Ok(None);
    }
    Ok(Some(super::jobs::enqueue_with_priority(app, &JobRequest::RaAccountSync, PRIORITY_INTERACTIVE).await?))
}

// ---------------------------------------------------------------------------
// game lists
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
pub struct GamesQuery {
    /// titles (all) | mastery | complete | beat | in_progress | to_play
    pub list: Option<String>,
    /// all | local (default: the setting)
    pub source: Option<String>,
    /// in_progress only: include beaten games (default: the setting)
    pub include_beaten: Option<bool>,
    pub q: Option<String>,
    pub console_id: Option<u32>,
    /// One of the 19 major genres: RetroAchievements' own genre for the game -- the one the
    /// list shows -- whatever `metadata_source_priority` puts first for the catalogue.
    pub genre: Option<String>,
    /// all (default) | base | subset -- RetroAchievements lists a subset (bonus / challenge
    /// sets of a game) as its own game titled "... [Subset - ...]".
    pub set: Option<String>,
    /// title | console | progress | recent | award (default recent for account lists, title otherwise)
    pub sort: Option<String>,
    pub desc: Option<bool>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    /// Just this RA game (a detail page's row).
    #[serde(skip)]
    pub game_id: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GameRow {
    pub game_id: u64,
    pub title: Option<String>,
    pub console_id: Option<i64>,
    pub console_name: Option<String>,
    pub local: bool,
    pub in_account: bool,
    pub local_games: i64,
    /// A local release of this game (for its page and cover art).
    pub release_id: Option<u64>,
    pub cover_media_id: Option<u64>,
    pub max_possible: i64,
    pub num_awarded: i64,
    pub num_awarded_hardcore: i64,
    pub highest_award_kind: Option<String>,
    pub highest_award_date: Option<chrono::NaiveDateTime>,
    pub most_recent_awarded: Option<chrono::NaiveDateTime>,
    /// RetroAchievements' major genre(s), comma-separated, when fetched.
    pub genre: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GamesPage {
    pub items: Vec<GameRow>,
    pub total: i64,
    /// Per list, how many games it holds under the same source / filters.
    pub tab_counts: std::collections::BTreeMap<String, i64>,
    pub source: String,
    pub include_beaten: bool,
}

const LISTS: [&str; 6] = ["titles", "mastery", "complete", "beat", "in_progress", "to_play"];

fn list_condition(list: &str, include_beaten: bool) -> Result<&'static str> {
    Ok(match list {
        "titles" => "TRUE",
        "mastery" => "p.highest_award_kind = 'mastered'",
        "complete" => "p.highest_award_kind = 'completed'",
        "beat" => "p.highest_award_kind IN ('beaten-hardcore', 'beaten-softcore')",
        "in_progress" if include_beaten => "COALESCE(p.num_awarded, 0) > 0 AND COALESCE(p.highest_award_kind, '') NOT IN ('mastered', 'completed')",
        "in_progress" => "COALESCE(p.num_awarded, 0) > 0 AND COALESCE(p.highest_award_kind, '') = ''",
        "to_play" => "COALESCE(p.num_awarded, 0) = 0 AND COALESCE(p.highest_award_kind, '') = ''",
        other => return Err(Error::invalid(format!("unknown list {other:?} (titles, mastery, complete, beat, in_progress, to_play)"))),
    })
}

/// `source` as it is understood now. The values from before the collection took over the
/// combined views (`local_first`, `server_first`, `server`) all meant "every RA game" in one
/// order or another, so a stored setting or an old link still works.
pub fn normalize_source(source: &str) -> &str {
    match source {
        "local_first" | "server_first" | "server" => "all",
        other => other,
    }
}

pub async fn games(app: &App, q: &GamesQuery) -> Result<GamesPage> {
    let pool = &app.pool;
    let st = status(app).await?;
    let user = st.user.clone().unwrap_or_default();
    let list = q.list.as_deref().unwrap_or("titles");
    let source = q.source.clone().unwrap_or_else(|| st.source_default.clone());
    let include_beaten = q.include_beaten.unwrap_or(st.include_beaten_default);
    // Every row of this tab is a RetroAchievements game, so there are two questions to ask
    // of it: all of them (held ones first), or only the ones held here. The "local + RA" and
    // "RA only" choices this used to offer are views of the *collection* now
    // (`catalog::VIEWS`), where they separate RA-supported games from everything else.
    let (source_where, source_order) = match normalize_source(&source) {
        "local" => ("loc.game_id IS NOT NULL", ""),
        "all" => ("TRUE", "(loc.game_id IS NULL), "),
        other => return Err(Error::invalid(format!("unknown source {other:?} (all, local)"))),
    };
    let condition = list_condition(list, include_beaten)?;
    let mut filters = String::new();
    if q.q.as_deref().is_some_and(|s| !s.trim().is_empty()) {
        filters.push_str(" AND COALESCE(p.title, rg.title) LIKE ?");
    }
    if q.console_id.is_some() {
        filters.push_str(" AND COALESCE(p.console_id, rg.console_id) = ?");
    }
    if q.game_id.is_some() {
        filters.push_str(" AND u.game_id = ?");
    }
    match q.set.as_deref().unwrap_or("all") {
        "all" | "" => {}
        "base" => filters.push_str(" AND COALESCE(p.title, rg.title, '') NOT LIKE '%[Subset%'"),
        "subset" => filters.push_str(" AND COALESCE(p.title, rg.title, '') LIKE '%[Subset%'"),
        other => return Err(Error::invalid(format!("unknown set {other:?} (all, base, subset)"))),
    }
    // RetroAchievements' own genre only. It also took any genre of a linked catalogue title,
    // so a game RetroAchievements files under Puzzle turned up under Role-Playing because its
    // IGDB entry said so -- while the row showed Puzzle.
    if q.genre.as_deref().is_some_and(|g| !g.is_empty()) {
        filters.push_str(" AND FIND_IN_SET(?, rg.genre_major) > 0");
    }
    let desc = q.desc.unwrap_or(matches!(q.sort.as_deref(), None | Some("recent") | Some("progress") | Some("award")) && list != "titles" && list != "to_play");
    let dir = if desc { "DESC" } else { "ASC" };
    let sort = match q.sort.as_deref().unwrap_or(if list == "titles" || list == "to_play" { "title" } else { "recent" }) {
        "title" => format!("COALESCE(p.title, rg.title) {dir}"),
        "console" => format!("COALESCE(p.console_name, rg.console_name) {dir}, COALESCE(p.title, rg.title)"),
        "progress" => format!("(COALESCE(p.num_awarded, 0) / GREATEST(COALESCE(p.max_possible, rg.num_achievements, 1), 1)) {dir}, COALESCE(p.title, rg.title)"),
        "recent" => format!("p.most_recent_awarded {dir}, COALESCE(p.title, rg.title)"),
        "award" => format!("p.highest_award_date {dir}, COALESCE(p.title, rg.title)"),
        other => return Err(Error::invalid(format!("unknown sort {other:?}"))),
    };
    let media_type = super::catalog::grid_types(&db_settings::get::<String>(pool, db_settings::KEY_GRID_MEDIA_TYPE).await?.unwrap_or_else(|| "box".to_string()));
    let from = format!(
        "FROM (SELECT game_id FROM ({LOCAL_RA}) l0 UNION SELECT game_id FROM ra_user_progress WHERE ra_user = ? \
               UNION SELECT provider_id FROM retroachievements_games WHERE deleted = FALSE AND num_achievements > 0) u \
         LEFT JOIN ({LOCAL_RA}) loc ON loc.game_id = u.game_id \
         LEFT JOIN ra_user_progress p ON p.ra_user = ? AND p.game_id = u.game_id \
         LEFT JOIN retroachievements_games rg ON rg.provider_id = u.game_id"
    );
    // Only games with achievements (a local game whose set size isn't cached yet stays).
    let with_achievements = "COALESCE(p.max_possible, rg.num_achievements, 1) > 0";
    let bind_common = |mut query: sqlx::query::Query<'static, sqlx::MySql, sqlx::mysql::MySqlArguments>| {
        query = query.bind(user.clone()).bind(user.clone());
        if let Some(s) = q.q.as_deref().filter(|s| !s.trim().is_empty()) {
            query = query.bind(super::like_pattern(s.trim()));
        }
        if let Some(c) = q.console_id {
            query = query.bind(c);
        }
        if let Some(g) = q.game_id {
            query = query.bind(g);
        }
        if let Some(g) = q.genre.as_deref().filter(|g| !g.is_empty()) {
            query = query.bind(g.to_string());
        }
        query
    };
    let (limit, offset) = super::clamp_page(q.limit, q.offset);
    // The held release's art first, then the catalogue's order: front, a user's pick, the
    // release's own region before `common`, the provider order.
    let media_order = super::catalog::MEDIA_ORDER;
    let sql = format!(
        "SELECT CAST(u.game_id AS UNSIGNED) AS game_id, COALESCE(p.title, rg.title) AS title, CAST(COALESCE(p.console_id, rg.console_id) AS SIGNED) AS console_id, \
         COALESCE(p.console_name, rg.console_name) AS console_name, CAST(loc.game_id IS NOT NULL AS SIGNED) AS is_local, CAST(p.game_id IS NOT NULL AS SIGNED) AS in_account, \
         CAST(COALESCE(loc.local_games, 0) AS SIGNED) AS local_games, loc.release_id, \
         (SELECT m.id FROM release_provider_links rpl2 JOIN release_media rm ON rm.release_id = rpl2.release_id JOIN media m ON m.id = rm.media_id \
            WHERE rpl2.provider = 'retroachievements' AND rpl2.provider_id = CAST(u.game_id AS CHAR) AND rpl2.deleted = FALSE AND rpl2.confidence IN ('exact', 'manual') AND FIND_IN_SET(m.media_type, ?) AND m.deleted = FALSE \
            ORDER BY (rpl2.release_id = loc.release_id) DESC, {media_order} LIMIT 1) AS cover_media_id, \
         CAST(COALESCE(p.max_possible, rg.num_achievements, 0) AS SIGNED) AS max_possible, CAST(COALESCE(p.num_awarded, 0) AS SIGNED) AS num_awarded, \
         CAST(COALESCE(p.num_awarded_hardcore, 0) AS SIGNED) AS num_awarded_hardcore, p.highest_award_kind, p.highest_award_date, p.most_recent_awarded, rg.genre_major \
         {from} WHERE {with_achievements} AND {source_where} AND {condition}{filters} ORDER BY {source_order}{sort} LIMIT {limit} OFFSET {offset}"
    );
    // The cover subquery's bind comes first in the SELECT list.
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(media_type);
    query = bind_common(query);
    let rows = query.fetch_all(pool).await?;
    let items = rows
        .iter()
        .map(|r| -> Result<GameRow> {
            Ok(GameRow {
                game_id: r.try_get("game_id")?,
                title: r.try_get("title")?,
                console_id: r.try_get("console_id")?,
                console_name: r.try_get("console_name")?,
                local: r.try_get::<i64, _>("is_local")? != 0,
                in_account: r.try_get::<i64, _>("in_account")? != 0,
                local_games: r.try_get("local_games")?,
                release_id: r.try_get("release_id")?,
                cover_media_id: r.try_get("cover_media_id")?,
                max_possible: r.try_get("max_possible")?,
                num_awarded: r.try_get("num_awarded")?,
                num_awarded_hardcore: r.try_get("num_awarded_hardcore")?,
                highest_award_kind: r.try_get("highest_award_kind")?,
                highest_award_date: r.try_get("highest_award_date")?,
                most_recent_awarded: r.try_get("most_recent_awarded")?,
                genre: r.try_get("genre_major")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    // Tab counts (and this list's total) in one pass.
    let sums: Vec<String> = LISTS.iter().map(|l| format!("CAST(SUM(CASE WHEN {} THEN 1 ELSE 0 END) AS SIGNED) AS `{l}`", list_condition(l, include_beaten).unwrap_or("FALSE"))).collect();
    let sql = format!("SELECT {} {from} WHERE {with_achievements} AND {source_where}{filters}", sums.join(", "));
    let row = bind_common(sqlx::query(sqlx::AssertSqlSafe(sql))).fetch_one(pool).await?;
    let mut tab_counts = std::collections::BTreeMap::new();
    for l in LISTS {
        tab_counts.insert(l.to_string(), row.try_get::<Option<i64>, _>(l)?.unwrap_or(0));
    }
    let total = tab_counts.get(list).copied().unwrap_or(0);
    Ok(GamesPage { items, total, tab_counts, source, include_beaten })
}

// ---------------------------------------------------------------------------
// one game: achievements (lazily fetched) and its local releases
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Achievement {
    pub id: u32,
    pub title: Option<String>,
    pub description: Option<String>,
    pub points: Option<u32>,
    pub true_ratio: Option<u32>,
    pub badge_name: Option<String>,
    pub kind: Option<String>,
    pub date_earned: Option<chrono::NaiveDateTime>,
    pub date_earned_hardcore: Option<chrono::NaiveDateTime>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalRelease {
    pub release_id: u64,
    pub name: String,
    pub platform: Option<String>,
    pub title_id: Option<u64>,
    pub local_games: i64,
    /// How the release was linked to this RA game (manual / exact / hash / name / fuzzy).
    pub confidence: String,
    /// RetroAchievements' hash of this release, and its compatibility patch (a patched ROM).
    #[serde(flatten)]
    pub patch: PatchState,
}

/// A supported hash's compatibility patch: its URL, the downloaded archive, a running download.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PatchState {
    pub ra_md5: Option<String>,
    /// RetroAchievements' name for the hash.
    pub ra_name: Option<String>,
    pub patch_url: Option<String>,
    pub patch: Option<PatchHeld>,
    /// A queued / running download: its progress message.
    pub patch_job: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PatchHeld {
    pub id: u64,
    /// Relative to the patch directory.
    pub rel_path: String,
    pub file_present: bool,
    pub patch_rom_name: Option<String>,
    pub names_consistent: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct GameDetail {
    /// The account/local row as the lists show it (None: neither local nor played).
    pub row: Option<GameRow>,
    pub game_id: u32,
    pub title: Option<String>,
    pub console_name: Option<String>,
    pub achievements: Vec<Achievement>,
    pub releases: Vec<LocalRelease>,
    /// Patched hashes RetroAchievements lists that no release above carries.
    pub other_patches: Vec<PatchState>,
    pub fetched_at: Option<chrono::NaiveDateTime>,
}

/// Fetches the game's details and the account's unlocks when missing or stale: the
/// account's progress shows an unlock newer than the last fetch, RetroAchievements changed
/// the achievement set since (`set_modified_at`), or it is 30 days old.
async fn ensure_game_detail(app: &App, game_id: u32, force: bool) -> Result<()> {
    ensure_game_detail_with(app, game_id, force, None).await.map(|_| ())
}

/// Whether a game's fetched achievements (`fetched`, made against set date `detail_set`) need
/// fetching again: never fetched, an unlock newer than the fetch (`recent`), a set changed
/// since (`set` later than `detail_set`), a set date known now that the fetch did not record,
/// or a month old.
fn detail_stale(
    fetched: Option<chrono::NaiveDateTime>,
    recent: Option<chrono::NaiveDateTime>,
    detail_set: Option<chrono::NaiveDateTime>,
    set: Option<chrono::NaiveDateTime>,
    now: chrono::NaiveDateTime,
) -> bool {
    let Some(f) = fetched else { return true };
    let set_changed = set.is_some_and(|s| detail_set.is_none_or(|d| s > d));
    set_changed || recent.is_some_and(|r| r > f) || (now - f).num_days() >= 30
}

/// [`ensure_game_detail`] given the set's change date from a fresh game list (`known_set`),
/// which is recorded on the game. Details fetched before a set date was known are fetched once
/// more when one is, so they carry it. Returns whether it fetched.
async fn ensure_game_detail_with(app: &App, game_id: u32, force: bool, known_set: Option<chrono::NaiveDateTime>) -> Result<bool> {
    let client = client(app)?;
    let user = client.username().to_string();
    let pool = &app.pool;
    let row = sqlx::query(
        "SELECT d.fetched_at, d.set_modified_at AS detail_set, rg.set_modified_at AS game_set, p.most_recent_awarded FROM (SELECT ? AS game_id) g \
         LEFT JOIN ra_user_game_detail d ON d.ra_user = ? AND d.game_id = g.game_id LEFT JOIN ra_user_progress p ON p.ra_user = ? AND p.game_id = g.game_id \
         LEFT JOIN retroachievements_games rg ON rg.provider_id = g.game_id",
    )
    .bind(game_id)
    .bind(&user)
    .bind(&user)
    .fetch_one(pool)
    .await?;
    let fetched: Option<chrono::NaiveDateTime> = row.try_get("fetched_at")?;
    let recent: Option<chrono::NaiveDateTime> = row.try_get("most_recent_awarded")?;
    let detail_set: Option<chrono::NaiveDateTime> = row.try_get("detail_set")?;
    let set = known_set.or(row.try_get::<Option<chrono::NaiveDateTime>, _>("game_set")?);
    if !force && !detail_stale(fetched, recent, detail_set, set, chrono::Utc::now().naive_utc()) {
        return Ok(false);
    }
    let raw = client.get_game_info_and_user_progress(game_id, &user).await?;
    let s = |k: &str| raw.get(k).and_then(|v| v.as_str()).filter(|v| !v.is_empty()).map(str::to_string);
    let n = |k: &str| raw.get(k).and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|x| x.parse().ok())));
    sqlx::query(
        "INSERT INTO retroachievements_games (provider_id, console_id, console_name, title, num_achievements, image_icon, image_title, image_ingame, image_boxart, fetched_at, deleted) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, NOW(), FALSE) ON DUPLICATE KEY UPDATE console_id = COALESCE(VALUES(console_id), console_id), console_name = COALESCE(VALUES(console_name), console_name), \
         title = COALESCE(VALUES(title), title), num_achievements = COALESCE(VALUES(num_achievements), num_achievements), image_icon = COALESCE(VALUES(image_icon), image_icon), \
         image_title = COALESCE(VALUES(image_title), image_title), image_ingame = COALESCE(VALUES(image_ingame), image_ingame), image_boxart = COALESCE(VALUES(image_boxart), image_boxart)",
    )
    .bind(game_id)
    .bind(n("ConsoleID"))
    .bind(s("ConsoleName"))
    .bind(s("Title"))
    .bind(n("NumAchievements"))
    .bind(s("ImageIcon"))
    .bind(s("ImageTitle"))
    .bind(s("ImageIngame"))
    .bind(s("ImageBoxArt"))
    .execute(pool)
    .await?;
    if let Some(genre) = s("Genre") {
        sqlx::query("UPDATE retroachievements_games SET genre_raw = ?, genre_major = ?, genre_checked_at = NOW() WHERE provider_id = ?")
            .bind(&genre)
            .bind(crate::database::providers::ra_account::genre_majors(&genre))
            .bind(game_id)
            .execute(pool)
            .await?;
    }
    let date = |v: Option<&serde_json::Value>| v.and_then(|v| v.as_str()).and_then(crate::database::providers::ra_account::parse_ra_date);
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM ra_user_achievements WHERE ra_user = ? AND game_id = ?").bind(&user).bind(game_id).execute(&mut *tx).await?;
    if let Some(map) = raw.get("Achievements").and_then(|a| a.as_object()) {
        for a in map.values() {
            let an = |k: &str| a.get(k).and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|x| x.parse().ok())));
            let as_ = |k: &str| a.get(k).and_then(|v| v.as_str()).filter(|v| !v.is_empty()).map(str::to_string);
            let Some(id) = an("ID") else { continue };
            sqlx::query(
                "INSERT INTO ra_user_achievements (ra_user, game_id, achievement_id, title, description, points, true_ratio, badge_name, display_order, kind, date_earned, date_earned_hardcore) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&user)
            .bind(game_id)
            .bind(id)
            .bind(as_("Title").map(|t| t.chars().take(255).collect::<String>()))
            .bind(as_("Description").map(|t| t.chars().take(1024).collect::<String>()))
            .bind(an("Points"))
            .bind(an("TrueRatio"))
            .bind(as_("BadgeName").map(|t| t.chars().take(32).collect::<String>()))
            .bind(a.get("DisplayOrder").and_then(|v| v.as_i64()))
            // RA spells it "Type" (older docs: "type").
            .bind(as_("Type").or_else(|| as_("type")))
            .bind(date(a.get("DateEarned")))
            .bind(date(a.get("DateEarnedHardcore")))
            .execute(&mut *tx)
            .await?;
        }
    }
    if known_set.is_some() {
        sqlx::query("UPDATE retroachievements_games SET set_modified_at = ? WHERE provider_id = ?").bind(known_set).bind(game_id).execute(&mut *tx).await?;
    }
    sqlx::query("INSERT INTO ra_user_game_detail (ra_user, game_id, fetched_at, set_modified_at) VALUES (?, ?, NOW(), ?) ON DUPLICATE KEY UPDATE fetched_at = NOW(), set_modified_at = VALUES(set_modified_at)")
        .bind(&user)
        .bind(game_id)
        .bind(set)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}

async fn achievements(app: &App, game_id: u32) -> Result<Vec<Achievement>> {
    let user = client(app)?.username().to_string();
    let rows = sqlx::query("SELECT * FROM ra_user_achievements WHERE ra_user = ? AND game_id = ? ORDER BY COALESCE(display_order, 0), achievement_id").bind(&user).bind(game_id).fetch_all(&app.pool).await?;
    rows.iter()
        .map(|r| {
            Ok(Achievement {
                id: r.try_get("achievement_id")?,
                title: r.try_get("title")?,
                description: r.try_get("description")?,
                points: r.try_get("points")?,
                true_ratio: r.try_get("true_ratio")?,
                badge_name: r.try_get("badge_name")?,
                kind: r.try_get("kind")?,
                date_earned: r.try_get("date_earned")?,
                date_earned_hardcore: r.try_get("date_earned_hardcore")?,
            })
        })
        .collect()
}

pub async fn game(app: &App, game_id: u32, refresh: bool) -> Result<GameDetail> {
    ensure_game_detail(app, game_id, refresh).await?;
    let pool = &app.pool;
    let achievements = achievements(app, game_id).await?;
    let info = sqlx::query("SELECT title, console_name FROM retroachievements_games WHERE provider_id = ?").bind(game_id).fetch_optional(pool).await?;
    // RetroAchievements' per-hash list (names, patch links): cached, refreshed when stale.
    if let Ok(c) = client(app) {
        let ttl = db_settings::cache_ttl_days(pool).await.unwrap_or(90);
        if crate::database::providers::ra_hashes::is_stale(pool, game_id, ttl).await.unwrap_or(false) || refresh {
            if let Err(e) = crate::database::providers::ra_hashes::refresh_game_hashes(pool, &c, game_id).await {
                log::warn!("RA hash list for game {game_id}: {e}");
            }
        }
    }
    let patches = patch_states(app, game_id).await?;
    let releases = sqlx::query(
        "SELECT r.id, r.name, p.name AS platform, rpl.confidence, \
         (SELECT gh.md5 FROM release_contents rc JOIN hashes h ON h.content_id = rc.content_id AND h.algorithm IN ('ra_hash', 'md5') JOIN ra_game_hashes gh ON gh.md5 = h.value_hex AND gh.ra_game_id = rpl.provider_id \
           WHERE rc.release_id = r.id ORDER BY gh.patch_url IS NULL LIMIT 1) AS ra_md5, (SELECT tr.title_id FROM title_releases tr WHERE tr.release_id = r.id LIMIT 1) AS title_id, \
         CAST((SELECT COUNT(DISTINCT lrg.game_id) FROM local_release_games lrg JOIN local_game lg ON lg.id = lrg.game_id AND lg.deleted = FALSE WHERE lrg.release_id = r.id) AS SIGNED) AS local_games \
         FROM release_provider_links rpl JOIN `release` r ON r.id = rpl.release_id AND r.deleted = FALSE LEFT JOIN platforms p ON p.id = r.app_platform_id \
         WHERE rpl.provider = 'retroachievements' AND rpl.provider_id = ? AND rpl.deleted = FALSE AND rpl.confidence IN ('exact', 'manual') ORDER BY FIELD(rpl.confidence, 'manual', 'exact', 'hash', 'name', 'fuzzy'), local_games DESC, r.name",
    )
    .bind(game_id.to_string())
    .fetch_all(pool)
    .await?
    .iter()
    .map(|r| -> Result<LocalRelease> {
        let md5: Option<String> = r.try_get("ra_md5")?;
        let patch = md5.as_ref().and_then(|m| patches.get(m).cloned()).unwrap_or_default();
        Ok(LocalRelease { release_id: r.try_get("id")?, name: r.try_get("name")?, platform: r.try_get("platform")?, title_id: r.try_get("title_id")?, local_games: r.try_get("local_games")?, confidence: r.try_get("confidence")?, patch })
    })
    .collect::<Result<Vec<_>>>()?;
    let shown: std::collections::HashSet<&str> = releases.iter().filter_map(|r| r.patch.ra_md5.as_deref()).collect();
    let mut other_patches: Vec<PatchState> = patches.values().filter(|p| p.patch_url.is_some() && !shown.contains(p.ra_md5.as_deref().unwrap_or(""))).cloned().collect();
    other_patches.sort_by(|a, b| a.ra_name.cmp(&b.ra_name));
    let fetched_at = sqlx::query_scalar("SELECT fetched_at FROM ra_user_game_detail WHERE ra_user = ? AND game_id = ?").bind(client(app)?.username()).bind(game_id).fetch_optional(pool).await?;
    let row = games(app, &GamesQuery { list: Some("titles".into()), source: Some("all".into()), game_id: Some(game_id as u64), limit: Some(1), ..Default::default() }).await?.items.into_iter().next();
    Ok(GameDetail {
        row,
        game_id,
        title: info.as_ref().and_then(|r| r.try_get("title").ok()),
        console_name: info.as_ref().and_then(|r| r.try_get("console_name").ok()),
        achievements,
        releases,
        other_patches,
        fetched_at,
    })
}

/// Every hash RetroAchievements lists for the game, with its patch link, the downloaded
/// patch and any running download, by md5.
async fn patch_states(app: &App, game_id: u32) -> Result<std::collections::HashMap<String, PatchState>> {
    let pool = &app.pool;
    let root = crate::database::directories::patch_dir(pool).await?;
    let mut out = std::collections::HashMap::new();
    for r in sqlx::query("SELECT md5, name, patch_url FROM ra_game_hashes WHERE ra_game_id = ?").bind(game_id).fetch_all(pool).await? {
        let md5: String = r.try_get("md5")?;
        out.insert(md5.clone(), PatchState { ra_md5: Some(md5), ra_name: r.try_get("name")?, patch_url: r.try_get("patch_url")?, ..Default::default() });
    }
    for r in sqlx::query("SELECT id, md5, rel_path, patch_rom_name, names_consistent FROM ra_patches WHERE ra_game_id = ?").bind(game_id).fetch_all(pool).await? {
        let md5: String = r.try_get("md5")?;
        let rel_path: String = r.try_get("rel_path")?;
        let file_present = root.as_ref().is_some_and(|root| root.join(&rel_path).is_file());
        let held = PatchHeld { id: r.try_get("id")?, rel_path, file_present, patch_rom_name: r.try_get("patch_rom_name")?, names_consistent: r.try_get("names_consistent")? };
        out.entry(md5.clone()).or_insert_with(|| PatchState { ra_md5: Some(md5), ..Default::default() }).patch = Some(held);
    }
    for r in sqlx::query("SELECT payload, status, progress_message FROM jobs WHERE job_type = 'ra_patch_download' AND status IN ('pending', 'in_progress')").fetch_all(pool).await? {
        let payload: serde_json::Value = r.try_get("payload")?;
        let Some(md5) = payload.get("md5").and_then(|m| m.as_str()).map(str::to_ascii_lowercase) else { continue };
        if let Some(state) = out.get_mut(&md5) {
            let status: String = r.try_get("status")?;
            state.patch_job = Some(r.try_get::<Option<String>, _>("progress_message")?.unwrap_or(status));
        }
    }
    Ok(out)
}

/// Queues the download of a hash's compatibility patch into the patch directory.
pub async fn download_patch(app: &App, md5: &str) -> Result<Option<u64>> {
    if crate::database::directories::patch_dir(&app.pool).await?.is_none() {
        return Err(Error::invalid("no patch directory: add a directory of type \"patch\" in Config > Directories"));
    }
    let request = JobRequest::RaPatchDownload(super::jobs::RaPatchDownloadRequest { md5: md5.trim().to_ascii_lowercase() });
    super::jobs::validate(app, &request).await?;
    super::jobs::enqueue_unless_queued(app, &request).await
}

// ---------------------------------------------------------------------------
// images
// ---------------------------------------------------------------------------

/// A media path RA hands out (`/Images/012345.png`, `/Badge/123.png`, `/UserPic/x.png`),
/// downloaded once into [`media_root`].
async fn cached_media(app: &App, path: &str) -> Result<PathBuf> {
    let clean = path.trim().trim_start_matches('/');
    let valid = !clean.is_empty() && clean.split('/').all(|seg| !seg.is_empty() && seg != "." && seg != ".." && seg.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')));
    if !valid || path.starts_with("http") {
        return Err(Error::invalid(format!("unexpected RetroAchievements media path {path:?}")));
    }
    let local = media_file(app, &media_root(app).await?, clean).await;
    if local.is_file() {
        return Ok(local);
    }
    let response = app.http.get(RaClient::media_url(clean)).send().await.and_then(|r| r.error_for_status()).map_err(|e| Error::unavailable(format!("RetroAchievements media: {e}")))?;
    let bytes = response.bytes().await.map_err(|e| Error::unavailable(format!("RetroAchievements media: {e}")))?;
    crate::database::media_store::write_file(&local, &bytes).await?;
    Ok(local)
}

/// icon | title | ingame | boxart for one game.
pub async fn image(app: &App, game_id: u32, kind: &str) -> Result<PathBuf> {
    let column = match kind {
        "icon" => "image_icon",
        "title" => "image_title",
        "ingame" | "screenshot" => "image_ingame",
        "boxart" => "image_boxart",
        other => return Err(Error::invalid(format!("unknown image kind {other:?} (icon, title, ingame, boxart)"))),
    };
    let user = client(app)?.username().to_string();
    let lookup = || async {
        let sql = format!(
            "SELECT COALESCE(rg.{column}, {}) AS path FROM (SELECT ? AS game_id) g LEFT JOIN retroachievements_games rg ON rg.provider_id = g.game_id \
             LEFT JOIN ra_user_progress p ON p.ra_user = ? AND p.game_id = g.game_id",
            if column == "image_icon" { "p.image_icon" } else { "NULL" }
        );
        sqlx::query_scalar::<_, Option<String>>(sqlx::AssertSqlSafe(sql)).bind(game_id).bind(user.clone()).fetch_one(&app.pool).await
    };
    let mut path = lookup().await?;
    if path.is_none() {
        ensure_game_detail(app, game_id, true).await?;
        path = lookup().await?;
    }
    let path = path.ok_or_else(|| Error::not_found(format!("RetroAchievements has no {kind} image for game {game_id}")))?;
    cached_media(app, &path).await
}

/// One achievement badge (`locked`: RA's greyed `_lock` variant).
pub async fn badge(app: &App, name: &str, locked: bool) -> Result<PathBuf> {
    if name.is_empty() || name.len() > 32 || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(Error::invalid("bad badge name"));
    }
    cached_media(app, &format!("Badge/{name}{}.png", if locked { "_lock" } else { "" })).await
}

pub async fn avatar(app: &App) -> Result<PathBuf> {
    let user = client(app)?.username().to_string();
    let pic: Option<String> = sqlx::query_scalar("SELECT user_pic FROM ra_user_profile WHERE ra_user = ?").bind(&user).fetch_optional(&app.pool).await?.flatten();
    cached_media(app, &pic.ok_or_else(|| Error::not_found("no account profile yet (refresh the RetroAchievements tab)"))?).await
}

/// How an achievement's badge is drawn in the mosaic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unlock {
    Locked,
    Softcore,
    Hardcore,
}

/// Columns of a mosaic of `n` badges: roughly square, at most 8 wide (a big set grows
/// downward; grid cards show the top square of it).
pub fn mosaic_columns(n: usize) -> u32 {
    ((n as f64).sqrt().ceil() as u32).clamp(1, 8)
}

/// Composes 64 px badges into one JPEG: unlocked in colour (hardcore with a gold frame,
/// softcore with a silver one), locked greyed and darkened.
pub fn compose_mosaic(badges: &[(Option<Vec<u8>>, Unlock)]) -> std::result::Result<Vec<u8>, String> {
    use image::{GenericImageView, ImageEncoder, Rgb, RgbImage};
    const S: u32 = 64;
    let cols = mosaic_columns(badges.len());
    let rows = (badges.len() as u32).div_ceil(cols).max(1);
    let mut canvas = RgbImage::from_pixel(cols * S, rows * S, Rgb([22, 22, 28]));
    for (i, (bytes, unlock)) in badges.iter().enumerate() {
        let (x, y) = ((i as u32 % cols) * S, (i as u32 / cols) * S);
        let Some(img) = bytes.as_deref().and_then(|b| image::load_from_memory(b).ok()) else { continue };
        let img = if img.dimensions() == (S, S) { img } else { img.resize_exact(S, S, image::imageops::FilterType::Triangle) };
        let mut tile = img.to_rgb8();
        if *unlock == Unlock::Locked {
            for p in tile.pixels_mut() {
                let l = (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32) * 0.45;
                *p = Rgb([l as u8, l as u8, l as u8]);
            }
        }
        let frame = match unlock {
            Unlock::Hardcore => Some((Rgb([240, 190, 40]), 3)),
            Unlock::Softcore => Some((Rgb([190, 196, 210]), 2)),
            Unlock::Locked => None,
        };
        if let Some((color, w)) = frame {
            for (px, py, p) in tile.enumerate_pixels_mut() {
                if px < w || py < w || px >= S - w || py >= S - w {
                    *p = color;
                }
            }
        }
        image::imageops::replace(&mut canvas, &tile, x as i64, y as i64);
    }
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85).write_image(canvas.as_raw(), canvas.width(), canvas.height(), image::ExtendedColorType::Rgb8).map_err(|e| e.to_string())?;
    Ok(out)
}

/// The game's achievement badges as one image (cached per unlock state). `None` when the
/// game has no achievements.
pub async fn badge_mosaic(app: &App, game_id: u32) -> Result<Option<PathBuf>> {
    ensure_game_detail(app, game_id, false).await?;
    let list = achievements(app, game_id).await?;
    if list.is_empty() {
        return Ok(None);
    }
    let states: Vec<(Option<String>, Unlock)> = list
        .iter()
        .map(|a| (a.badge_name.clone(), if a.date_earned_hardcore.is_some() { Unlock::Hardcore } else if a.date_earned.is_some() { Unlock::Softcore } else { Unlock::Locked }))
        .collect();
    let key: String = states.iter().map(|(b, u)| format!("{}:{:?};", b.as_deref().unwrap_or(""), u)).collect();
    let user = crate::database::media_store::sanitize_component(client(app)?.username());
    let file = app.paths.provider_cache_dir().join("retroachievements").join("mosaic").join(format!("{user}_{game_id}_{}.jpg", &crate::database::media_store::sha1_bytes(key.as_bytes())[..12]));
    if file.is_file() {
        return Ok(Some(file));
    }
    let paths: Vec<Option<String>> = states.iter().map(|(b, _)| b.as_ref().map(|b| format!("Badge/{b}.png"))).collect();
    download_media(app, paths.iter().flatten().cloned().collect()).await;
    let root = media_root(app).await?;
    let mut badges: Vec<(Option<Vec<u8>>, Unlock)> = Vec::with_capacity(states.len());
    for (path, (_, unlock)) in paths.iter().zip(&states) {
        let bytes = match path {
            Some(p) => tokio::fs::read(media_file(app, &root, p).await).await.ok(),
            None => None,
        };
        badges.push((bytes, *unlock));
    }
    let jpeg = tokio::task::spawn_blocking(move || compose_mosaic(&badges)).await.map_err(|e| Error::internal(e.to_string()))?.map_err(Error::internal)?;
    crate::database::media_store::write_file(&file, &jpeg).await?;
    Ok(Some(file))
}

/// Fetches RA media paths not cached yet, 6 at a time from RA's media host (no API allowance
/// involved), each once into the media cache. Returns how many were fetched; one that fails
/// is left for the next caller.
async fn download_media(app: &App, paths: Vec<String>) -> u64 {
    let root = match media_root(app).await {
        Ok(root) => root,
        Err(e) => {
            log::warn!("RetroAchievements media: {e}");
            return 0;
        }
    };
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(6));
    let mut set = tokio::task::JoinSet::new();
    for path in paths {
        let local = media_file(app, &root, &path).await;
        if local.is_file() {
            continue;
        }
        let (http, url, sem) = (app.http.clone(), RaClient::media_url(&path), semaphore.clone());
        set.spawn(async move {
            let _permit = sem.acquire_owned().await.ok();
            let Ok(r) = http.get(url).send().await.and_then(|r| r.error_for_status()) else { return false };
            let Ok(b) = r.bytes().await else { return false };
            crate::database::media_store::write_file(&local, &b).await.is_ok()
        });
    }
    let mut fetched = 0;
    while let Some(done) = set.join_next().await {
        fetched += u64::from(done.unwrap_or(false));
    }
    fetched
}

pub use romexplorer_types::jobs::RaBadgesRequest;

/// What an `ra_badges` job did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct BadgesSummary {
    pub scope: String,
    pub games: u64,
    /// Consoles whose game list was read for the sets' change dates (one request each).
    pub consoles: u64,
    /// Games whose achievements were fetched again: a newer unlock, a set RetroAchievements
    /// changed since, a fetch from before set dates were recorded, or a month old.
    pub refreshed: u64,
    pub badges: u64,
    /// Badge images fetched this run (both the colour and the locked one of each).
    pub downloaded: u64,
    pub mosaics: u64,
    pub errors: Vec<String>,
}

pub fn validate_badges(request: &RaBadgesRequest) -> Result<()> {
    if !matches!(request.scope.as_str(), "played" | "owned") {
        return Err(Error::invalid("scope must be played or owned"));
    }
    Ok(())
}

pub fn badges_one_liner(s: &BadgesSummary) -> String {
    let mut line = format!("{} game(s) on {} console(s), {} refreshed, {} badge(s), {} downloaded", s.games, s.consoles, s.refreshed, s.badges, s.downloaded);
    if !s.errors.is_empty() {
        line.push_str(&format!(", {} error(s)", s.errors.len()));
    }
    line
}

/// "RA badges download/update": the account's progress first, so new unlocks are known, and
/// the game list of each console the games are on (no hashes; one request per console), so
/// sets RetroAchievements edited are known by their `DateModified`. Then per game: its
/// achievements fetched again where an unlock or the set is newer than the last fetch (or the
/// last fetch predates set dates, which records one), every badge in both its colour and
/// locked image, and the mosaic redrawn for the current unlocks (older mosaics of the game
/// removed). `played`: the account's games; `owned`: games linked to a release held here.
pub async fn update_badges(app: &App, request: &RaBadgesRequest, progress: &crate::database::jobs::Progress) -> Result<BadgesSummary> {
    validate_badges(request)?;
    let user = client(app)?.username().to_string();
    let mut summary = BadgesSummary { scope: request.scope.clone(), ..Default::default() };
    progress.force(0, None, "refreshing the account's progress").await;
    crate::database::providers::ra_account::sync(&app.clients(), &app.pool, &crate::database::jobs::Progress::none()).await?;
    let games: Vec<u64> = if request.scope == "owned" {
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT game_id FROM ({LOCAL_RA}) l ORDER BY game_id"))).fetch_all(&app.pool).await?
    } else {
        sqlx::query_scalar("SELECT CAST(game_id AS UNSIGNED) FROM ra_user_progress WHERE ra_user = ? ORDER BY game_id").bind(&user).fetch_all(&app.pool).await?
    };
    summary.games = games.len() as u64;
    let consoles: Vec<u32> = if request.scope == "owned" {
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT DISTINCT rg.console_id FROM retroachievements_games rg JOIN ({LOCAL_RA}) l ON l.game_id = rg.provider_id WHERE rg.console_id IS NOT NULL"))).fetch_all(&app.pool).await?
    } else {
        sqlx::query_scalar("SELECT DISTINCT console_id FROM ra_user_progress WHERE ra_user = ? AND console_id IS NOT NULL").bind(&user).fetch_all(&app.pool).await?
    };
    let mut set_dates: std::collections::HashMap<u32, chrono::NaiveDateTime> = std::collections::HashMap::new();
    let ra = client(app)?;
    for (i, console_id) in consoles.iter().enumerate() {
        progress.report(i as u64, Some(consoles.len() as u64), &format!("reading set dates: console {} of {}", i + 1, consoles.len())).await;
        match ra.get_game_list(*console_id, false, false).await {
            Ok((list, _)) => {
                summary.consoles += 1;
                set_dates.extend(list.iter().filter_map(|e| e.date_modified.as_deref().and_then(crate::database::providers::ra_account::parse_ra_date).map(|d| (e.id, d))));
            }
            Err(e) if summary.errors.len() < 50 => summary.errors.push(format!("console {console_id}: {e}")),
            Err(_) => {}
        }
    }
    let total = games.len() as u64;
    let safe_user = crate::database::media_store::sanitize_component(&user);
    for (i, game_id) in games.into_iter().enumerate() {
        progress.report(i as u64, Some(total), &format!("game {} of {total}: {} refreshed, {} badge(s) downloaded", i + 1, summary.refreshed, summary.downloaded)).await;
        let game_id = game_id as u32;
        match ensure_game_detail_with(app, game_id, false, set_dates.get(&game_id).copied()).await {
            Ok(fetched) => summary.refreshed += u64::from(fetched),
            Err(e) => {
                if summary.errors.len() < 50 {
                    summary.errors.push(format!("game {game_id}: {e}"));
                }
                continue;
            }
        }
        let list = achievements(app, game_id).await?;
        let names: Vec<&str> = list.iter().filter_map(|a| a.badge_name.as_deref()).collect();
        summary.badges += names.len() as u64;
        let paths = names.iter().flat_map(|b| [format!("Badge/{b}.png"), format!("Badge/{b}_lock.png")]).collect();
        summary.downloaded += download_media(app, paths).await;
        if let Some(file) = badge_mosaic(app, game_id).await? {
            summary.mosaics += 1;
            prune_mosaics(&file, &format!("{safe_user}_{game_id}_"));
        }
    }
    progress.force(total, Some(total.max(1)), &badges_one_liner(&summary)).await;
    Ok(summary)
}

/// Removes a game's mosaics for earlier unlock states, keeping `keep`.
fn prune_mosaics(keep: &std::path::Path, prefix: &str) {
    let Some(dir) = keep.parent() else { return };
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let stale = path != keep && path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(prefix) && n.ends_with(".jpg"));
        if stale {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Where RA's media files go: like every other provider download, under the first configured
/// image dir (`<image dir>/_retroachievements/`), else `<provider cache>/retroachievements/media/`.
async fn media_root(app: &App) -> Result<PathBuf> {
    let image = crate::database::storage::list_media_dirs(&app.pool).await?.into_iter().find(|d| d.role == "image");
    Ok(match image {
        Some(dir) => PathBuf::from(dir.path).join("_retroachievements"),
        None => provider_media_root(app),
    })
}

fn provider_media_root(app: &App) -> PathBuf {
    app.paths.provider_cache_dir().join("retroachievements").join("media")
}

/// RA media `path` under `root`. A file still in the provider cache from before an image dir
/// was configured is moved over rather than downloaded again.
async fn media_file(app: &App, root: &std::path::Path, path: &str) -> PathBuf {
    let rel = path.trim_start_matches('/');
    let local = crate::database::media_store::join_rel(root, rel);
    if !local.is_file() {
        let old = crate::database::media_store::join_rel(&provider_media_root(app), rel);
        if old != local && old.is_file() {
            if let Err(e) = crate::database::media_store::move_file(&old, &local).await {
                log::warn!("RetroAchievements media: moving {} to {}: {e}", old.display(), local.display());
            }
        }
    }
    local
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_is_fetched_again_for_unlocks_set_edits_and_missing_set_dates() {
        let t = |d: u32| chrono::NaiveDate::from_ymd_opt(2026, 1, d).unwrap().and_hms_opt(12, 0, 0).unwrap();
        let now = t(20);
        assert!(detail_stale(None, None, None, None, now), "never fetched");
        assert!(!detail_stale(Some(t(10)), Some(t(9)), Some(t(5)), Some(t(5)), now), "nothing new");
        assert!(detail_stale(Some(t(10)), Some(t(11)), Some(t(5)), Some(t(5)), now), "an unlock since");
        assert!(detail_stale(Some(t(10)), None, Some(t(5)), Some(t(12)), now), "the set was edited since");
        assert!(detail_stale(Some(t(10)), None, None, Some(t(5)), now), "fetched before set dates were recorded: once more, to record it");
        assert!(!detail_stale(Some(t(10)), None, None, None, now), "no set date known anywhere: nothing to add");
        assert!(detail_stale(Some(t(10)), None, Some(t(5)), Some(t(5)), t(10) + chrono::Duration::days(30)), "a month old");
    }

    #[test]
    fn stale_mosaics_of_the_game_are_removed() {
        let dir = std::env::temp_dir().join(format!("ra_mosaic_prune_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["me_12_aaa.jpg", "me_12_bbb.jpg", "me_123_ccc.jpg", "you_12_ddd.jpg"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        prune_mosaics(&dir.join("me_12_bbb.jpg"), "me_12_");
        let mut left: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["me_123_ccc.jpg", "me_12_bbb.jpg", "you_12_ddd.jpg"], "game 123 and another account untouched");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mosaic_is_roughly_square_and_capped_at_8_wide() {
        assert_eq!(mosaic_columns(1), 1);
        assert_eq!(mosaic_columns(5), 3);
        assert_eq!(mosaic_columns(40), 7);
        assert_eq!(mosaic_columns(64), 8);
        assert_eq!(mosaic_columns(150), 8);
    }

    #[test]
    fn mosaic_composes_states() {
        let badge = {
            let img = image::RgbImage::from_pixel(64, 64, image::Rgb([200, 40, 40]));
            let mut out = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgb8(img).write_to(&mut out, image::ImageFormat::Png).unwrap();
            out.into_inner()
        };
        let set = vec![(Some(badge.clone()), Unlock::Hardcore), (Some(badge.clone()), Unlock::Softcore), (Some(badge), Unlock::Locked), (None, Unlock::Locked)];
        let jpeg = compose_mosaic(&set).unwrap();
        let img = image::load_from_memory(&jpeg).unwrap().to_rgb8();
        assert_eq!((img.width(), img.height()), (128, 128), "4 badges -> 2 x 2");
        let gold = img.get_pixel(1, 1);
        assert!(gold[0] > 200 && gold[1] > 150 && gold[2] < 100, "hardcore frame is gold: {gold:?}");
        let colour = img.get_pixel(32, 32);
        assert!(colour[0] > 150 && colour[1] < 90, "unlocked keeps its colour: {colour:?}");
        let locked = img.get_pixel(32, 96);
        assert!(locked[0] < 90 && (locked[0] as i32 - locked[1] as i32).abs() < 12, "locked is grey and dark: {locked:?}");
    }

    #[test]
    fn list_conditions() {
        assert!(list_condition("in_progress", false).unwrap().contains("= ''"));
        assert!(list_condition("in_progress", true).unwrap().contains("NOT IN"));
        assert!(list_condition("nope", true).is_err());
    }
}
