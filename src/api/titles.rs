//! Title merging for clients (`database::title_merge`): merge titles the user picked, the
//! backlog of suggested merges (titles sharing an IGDB game that weren't merged
//! automatically), and queueing the automatic pass.

use crate::database::title_family as family;
use serde::{Deserialize, Serialize};

use crate::database::title_merge::{self, Suggestion};

use super::jobs::{self, JobRequest};
use super::{App, Error, Result};

#[derive(Debug, Clone, Deserialize)]
pub struct MergeRequest {
    pub title_ids: Vec<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MergeResult {
    /// The title the others were merged into (the lowest id), now under its canonical name.
    pub title_id: u64,
    pub merged: usize,
}

pub async fn merge(app: &App, req: &MergeRequest) -> Result<MergeResult> {
    let mut ids = req.title_ids.clone();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() < 2 {
        return Err(Error::invalid("pick at least two titles to merge"));
    }
    if ids.len() > 200 {
        return Err(Error::invalid("at most 200 titles at once"));
    }
    keep_igdb_games(app, &ids).await?;
    let heads = family::heads_of(&app.pool, &ids).await?;
    let title_id = title_merge::merge_titles(&app.pool, &ids, "manual", "manual").await?;
    sync_lead_igdb(app, Some(&[title_id])).await?;
    family::after_edit(&app.pool, &[title_id], &heads).await?;
    Ok(MergeResult { title_id, merged: ids.len() - 1 })
}

/// Titles combined by hand that IGDB keeps as different games (a game and its collection):
/// a title holds one IGDB link, so each title's game goes onto its own releases first, where
/// it keeps their art, their page and their names. The combined title then takes the game of
/// its first release by region priority (`sync_lead_igdb`). With one game between them, or
/// none, nothing is written.
pub(crate) async fn keep_igdb_games(app: &App, ids: &[u64]) -> Result<()> {
    use sqlx::Row;
    let list = ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let links: Vec<(u64, String, String)> = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT title_id, provider_id, CAST(confidence AS CHAR) AS confidence FROM title_provider_links WHERE provider = 'igdb' AND deleted = FALSE AND title_id IN ({list})"
    )))
    .fetch_all(&app.pool)
    .await?
    .iter()
    .map(|r| Ok((r.try_get("title_id")?, r.try_get("provider_id")?, r.try_get("confidence")?)))
    .collect::<std::result::Result<_, sqlx::Error>>()?;
    if links.iter().map(|(_, id, _)| id).collect::<std::collections::HashSet<_>>().len() < 2 {
        return Ok(());
    }
    let apart = crate::database::providers::cache::APART_SQL;
    for (title_id, igdb, confidence) in &links {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT IGNORE INTO release_provider_links (release_id, provider, provider_id, confidence) \
             SELECT r.id, 'igdb', ?, ? FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE WHERE tr.title_id = ? AND NOT {apart}"
        )))
        .bind(igdb)
        .bind(confidence)
        .bind(title_id)
        .execute(&app.pool)
        .await?;
    }
    Ok(())
}

/// The IGDB game of a title whose releases carry games of their own (combined by hand,
/// `keep_igdb_games`): the game of its first release by region priority -- language, edition
/// and the user's manual order included, as the title's art and name follow -- so its
/// metadata, name and art are that release's. Run again whenever that order can change.
///
/// Once a title's releases are back to one game (a combine undone), their own links are
/// dropped: the title's says the same. `only`: these titles (None: every such title).
/// Returns the titles whose game changed; their metadata is re-applied and art no longer
/// theirs taken off, by jobs queued here.
pub async fn sync_lead_igdb(app: &App, only: Option<&[u64]>) -> Result<Vec<u64>> {
    use sqlx::Row;
    let apart = crate::database::providers::cache::APART_SQL;
    let scope = match only {
        Some([]) => return Ok(Vec::new()),
        Some(ids) => format!(" AND tr.title_id IN ({})", ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",")),
        None => String::new(),
    };
    let titles: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT DISTINCT tr.title_id FROM release_provider_links rl JOIN `release` r ON r.id = rl.release_id AND r.deleted = FALSE \
         JOIN title_releases tr ON tr.release_id = r.id JOIN title t ON t.id = tr.title_id AND t.deleted = FALSE \
         WHERE rl.provider = 'igdb' AND rl.deleted = FALSE AND NOT {apart}{scope}"
    )))
    .fetch_all(&app.pool)
    .await?;
    let rank = super::catalog::ReleaseRank::load(app).await?;
    let lead_sql = format!(
        "SELECT rl.provider_id FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE \
         JOIN release_provider_links rl ON rl.release_id = r.id AND rl.provider = 'igdb' AND rl.deleted = FALSE \
         WHERE tr.title_id = ? AND NOT {apart} ORDER BY {} LIMIT 1",
        rank.sql("r")
    );
    let games_sql = format!(
        "SELECT COUNT(DISTINCT rl.provider_id) FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE \
         JOIN release_provider_links rl ON rl.release_id = r.id AND rl.provider = 'igdb' AND rl.deleted = FALSE WHERE tr.title_id = ? AND NOT {apart}"
    );
    let mut changed = Vec::new();
    for title_id in titles {
        let Some(lead): Option<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(lead_sql.clone())).bind(title_id).fetch_optional(&app.pool).await? else { continue };
        let current: Option<(String, String)> = sqlx::query("SELECT provider_id, CAST(confidence AS CHAR) AS confidence FROM title_provider_links WHERE title_id = ? AND provider = 'igdb' AND deleted = FALSE")
            .bind(title_id)
            .fetch_optional(&app.pool)
            .await?
            .map(|r| Ok::<_, sqlx::Error>((r.try_get("provider_id")?, r.try_get("confidence")?)))
            .transpose()?;
        let games: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(games_sql.clone())).bind(title_id).fetch_one(&app.pool).await?;
        let same_game = current.as_ref().is_some_and(|(id, _)| *id == lead);
        // Manual while it is combined from several games: the user put them together, so the
        // merge audit leaves the link alone. Back to one game, a link of its own keeps its own.
        if !same_game || (games > 1 && current.as_ref().is_some_and(|(_, c)| c != "manual")) {
            sqlx::query(
                "INSERT INTO title_provider_links (title_id, provider, provider_id, confidence, deleted) VALUES (?, 'igdb', ?, 'manual', FALSE) \
                 ON DUPLICATE KEY UPDATE provider_id = VALUES(provider_id), confidence = 'manual', deleted = FALSE, linked_at = NOW()",
            )
            .bind(title_id)
            .bind(&lead)
            .execute(&app.pool)
            .await?;
            if !same_game {
                changed.push(title_id);
            }
        }
        if games <= 1 {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE rl FROM release_provider_links rl JOIN `release` r ON r.id = rl.release_id JOIN title_releases tr ON tr.release_id = r.id \
                 WHERE tr.title_id = ? AND rl.provider = 'igdb' AND NOT {apart}"
            )))
            .bind(title_id)
            .execute(&app.pool)
            .await?;
        }
    }
    if !changed.is_empty() {
        let reapply = JobRequest::IgdbReapply(romexplorer_types::jobs::IgdbReapplyRequest { title_ids: Some(changed.clone()), ..Default::default() });
        if let Err(e) = jobs::enqueue_unless_queued(app, &reapply).await {
            log::warn!("titles {changed:?}: IGDB game changed; could not queue re-applying its metadata: {}", e.message);
        }
        if crate::database::providers::media::detach_stale(&app.pool, Some(&changed)).await? > 0 {
            let backfill = JobRequest::MediaBackfill(romexplorer_types::jobs::MediaBackfillRequest { scope: "owned".to_string(), platform: None, ..Default::default() });
            if let Err(e) = jobs::enqueue_unless_queued(app, &backfill).await {
                log::warn!("could not queue the media backfill: {}", e.message);
            }
        }
    }
    Ok(changed)
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SuggestionsQuery {
    /// pending (default) | accepted | dismissed
    pub status: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SuggestionsPage {
    pub items: Vec<Suggestion>,
    pub total: i64,
}

pub async fn suggestions(app: &App, q: &SuggestionsQuery) -> Result<SuggestionsPage> {
    let status = q.status.as_deref().unwrap_or("pending");
    if !matches!(status, "pending" | "accepted" | "dismissed") {
        return Err(Error::invalid("status must be pending, accepted or dismissed"));
    }
    let (limit, offset) = super::clamp_page(q.limit, q.offset);
    let (items, total) = title_merge::suggestions(&app.pool, status, limit, offset).await?;
    Ok(SuggestionsPage { items, total })
}

/// Takes a suggestion (its first choice, the merge for the IGDB-game kind) or turns it down,
/// through Merge review's own path (`review::apply`), which keeps each title's IGDB game.
pub async fn resolve_suggestion(app: &App, igdb_id: &str, accept: bool) -> Result<Option<u64>> {
    if accept {
        Ok(super::review::apply(app, igdb_id, &super::review::ApplyRequest::default()).await?.title_ids.first().copied())
    } else {
        super::review::dismiss(app, igdb_id).await?;
        Ok(None)
    }
}

/// What `POST /catalog/titles/merge-suggestions/bulk` is asked: merge or dismiss the listed
/// suggestions, or -- with `all` -- every one of `status` (pending unless said).
#[derive(Debug, Clone, Deserialize)]
pub struct BulkSuggestions {
    /// `accept` | `dismiss`.
    pub action: String,
    #[serde(default)]
    pub ids: Vec<String>,
    #[serde(default)]
    pub all: bool,
    pub status: Option<String>,
}

pub async fn resolve_suggestions(app: &App, req: &BulkSuggestions) -> Result<title_merge::BulkResolved> {
    let accept = match req.action.as_str() {
        "accept" => true,
        "dismiss" => false,
        _ => return Err(Error::invalid("action must be accept or dismiss")),
    };
    let status = req.status.as_deref().unwrap_or("pending");
    // A merged suggestion is done; a dismissed one can still be merged, one or all.
    if !matches!((status, accept), ("pending", _) | ("dismissed", true)) {
        return Err(Error::invalid("pending suggestions can be merged or dismissed, dismissed ones merged"));
    }
    if !req.all && req.ids.is_empty() {
        return Err(Error::invalid("no suggestions selected"));
    }
    if !accept {
        return Ok(title_merge::resolve_suggestions(&app.pool, (!req.all).then_some(req.ids.as_slice()), status, false).await?);
    }
    // One at a time through the same path as a single one: one that cannot be merged does
    // not stop the rest.
    let ids: Vec<String> = if req.all {
        sqlx::query_scalar("SELECT igdb_id FROM title_merge_suggestions WHERE status = ? ORDER BY updated_at DESC, igdb_id").bind(status).fetch_all(&app.pool).await?
    } else {
        req.ids.clone()
    };
    let mut out = title_merge::BulkResolved::default();
    for id in &ids {
        match super::review::apply(app, id, &super::review::ApplyRequest::default()).await {
            Ok(_) => out.resolved += 1,
            Err(e) => out.failed.push(format!("{id}: {}", e.message)),
        }
    }
    Ok(out)
}

/// Queues the automatic pass: over what changed since the last one, or with `full` over the
/// whole catalog (the mark is forgotten, so the next pass starts from nothing).
pub async fn dedupe(app: &App, full: bool) -> Result<Option<u64>> {
    if full {
        crate::database::title_merge::forget_pass(&app.pool).await?;
    }
    jobs::enqueue_unless_queued(app, &JobRequest::TitleDedupe).await
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseTitle {
    pub title_id: u64,
}

/// Puts a release under a title, taking it from whichever title had it: how a soundtrack
/// disc, a strategy guide or a making-of DVD is tied to the game it belongs to (they come
/// with no title of their own -- `platform_map::STANDALONE`), and how a release filed under
/// the wrong game is moved. The title page then lists it in its kind's section.
pub async fn link_release(app: &App, release_id: u64, req: &ReleaseTitle) -> Result<()> {
    move_releases(app, &ReleasesMove { release_ids: vec![release_id], title_id: req.title_id }).await.map(|_| ())
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReleasesMove {
    pub release_ids: Vec<u64>,
    pub title_id: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReleasesSplit {
    pub release_ids: Vec<u64>,
}

/// The most releases one move or split takes.
const MOVE_MAX: usize = 2000;

/// The titles the releases `ids` are under; an error unless every one of them is live.
async fn releases_and_titles(app: &App, ids: &[u64]) -> Result<Vec<u64>> {
    if ids.is_empty() {
        return Err(Error::invalid("pick at least one release"));
    }
    if ids.len() > MOVE_MAX {
        return Err(Error::invalid(format!("at most {MOVE_MAX} releases at once")));
    }
    let list = ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let found: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(DISTINCT id) FROM `release` WHERE id IN ({list}) AND deleted = FALSE"))).fetch_one(&app.pool).await?;
    let wanted = ids.iter().collect::<std::collections::HashSet<_>>().len();
    if found as usize != wanted {
        return Err(Error::not_found("some of those releases do not exist"));
    }
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT DISTINCT title_id FROM title_releases WHERE release_id IN ({list})"))).fetch_all(&app.pool).await?)
}

/// Puts releases under a title, taking them from whichever titles had them. Returns how
/// many moved.
pub async fn move_releases(app: &App, req: &ReleasesMove) -> Result<u64> {
    let pool = &app.pool;
    let before = releases_and_titles(app, &req.release_ids).await?;
    if sqlx::query_scalar::<_, u64>("SELECT id FROM title WHERE id = ? AND deleted = FALSE").bind(req.title_id).fetch_optional(pool).await?.is_none() {
        return Err(Error::not_found(format!("no title {}", req.title_id)));
    }
    let list = req.release_ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let mut tx = pool.begin().await?;
    let moved = sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM title_releases WHERE release_id IN ({list}) AND title_id <> ?"))).bind(req.title_id).execute(&mut *tx).await?.rows_affected();
    sqlx::query(sqlx::AssertSqlSafe(format!("INSERT IGNORE INTO title_releases (title_id, release_id) SELECT ?, id FROM `release` WHERE id IN ({list})"))).bind(req.title_id).execute(&mut *tx).await?;
    tx.commit().await?;
    after_releases_moved(app, &before, req.title_id).await?;
    Ok(moved)
}

/// After releases changed titles. A title left with none is gone (its place in sets goes
/// to where its releases went); the rest take their names and lead IGDB game from the
/// releases they now hold.
async fn after_releases_moved(app: &App, from: &[u64], to: u64) -> Result<()> {
    let pool = &app.pool;
    // The families the titles headed or were in, before any of them is gone.
    let mut touched = from.to_vec();
    touched.push(to);
    let heads = family::heads_of(pool, &touched).await?;
    let mut titles = vec![to];
    for &f in from.iter().filter(|f| **f != to) {
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title_releases WHERE title_id = ?").bind(f).fetch_one(pool).await?;
        if left > 0 {
            titles.push(f);
            continue;
        }
        sqlx::query("UPDATE IGNORE collection_set_entries SET title_id = ? WHERE title_id = ?").bind(to).bind(f).execute(pool).await?;
        sqlx::query("DELETE FROM collection_set_entries WHERE title_id = ?").bind(f).execute(pool).await?;
        sqlx::query("UPDATE IGNORE collection_set_entries SET family_title_id = ? WHERE family_title_id = ?").bind(to).bind(f).execute(pool).await?;
        sqlx::query("DELETE FROM collection_set_entries WHERE family_title_id = ?").bind(f).execute(pool).await?;
        sqlx::query("UPDATE title SET deleted = TRUE, basegame_title_id = NULL WHERE id = ?").bind(f).execute(pool).await?;
    }
    for t in &titles {
        title_merge::rename_canonical(pool, *t).await?;
    }
    sync_lead_igdb(app, Some(&titles)).await?;
    // A head that lost its last release, or a version that changed, re-forms its family.
    family::after_edit(pool, &titles, &heads).await?;
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
pub struct LocaleEdit {
    pub language: String,
    #[serde(default)]
    pub region: String,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LocalesEdit {
    pub locales: Vec<LocaleEdit>,
}

/// The user's own localized names for a title, in place of what IGDB gave: written as
/// source `user`, every other source's dropped, and the title locked so IGDB re-apply --
/// this title's or a batch over all -- does not write them back (`apply_igdb_game`).
pub async fn set_locales(app: &App, title_id: u64, req: &LocalesEdit) -> Result<()> {
    let pool = &app.pool;
    if sqlx::query_scalar::<_, u64>("SELECT id FROM title WHERE id = ? AND deleted = FALSE").bind(title_id).fetch_optional(pool).await?.is_none() {
        return Err(Error::not_found(format!("no title {title_id}")));
    }
    let mut rows: Vec<(String, String, String)> = Vec::new();
    for l in &req.locales {
        let (language, region, name) = (l.language.trim().to_ascii_lowercase(), l.region.trim().to_ascii_uppercase(), l.name.trim().to_string());
        if language.is_empty() || language.len() > 8 || !language.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(Error::invalid(format!("language code {:?}: 1-8 letters, like ja or en", l.language)));
        }
        if region.len() > 16 || !region.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(Error::invalid(format!("region code {:?}: letters only, like JP, or empty", l.region)));
        }
        if name.is_empty() {
            return Err(Error::invalid(format!("the {language} name is empty")));
        }
        if rows.iter().any(|(a, b, _)| *a == language && *b == region) {
            return Err(Error::invalid(format!("{language}{} is listed twice", if region.is_empty() { String::new() } else { format!("/{region}") })));
        }
        rows.push((language, region, name.chars().take(512).collect()));
    }
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM title_locales WHERE title_id = ?").bind(title_id).execute(&mut *tx).await?;
    for (language, region, name) in &rows {
        sqlx::query("INSERT INTO title_locales (title_id, language, region, source, name) VALUES (?, ?, ?, 'user', ?)").bind(title_id).bind(language).bind(region).bind(name).execute(&mut *tx).await?;
    }
    sqlx::query("UPDATE title SET locales_locked = TRUE WHERE id = ?").bind(title_id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Unlocks a title's localized names: the user's go, and IGDB's are applied again (a job,
/// queued when the title has an IGDB link). Returns that job.
pub async fn unlock_locales(app: &App, title_id: u64) -> Result<Option<u64>> {
    let pool = &app.pool;
    let done = sqlx::query("UPDATE title SET locales_locked = FALSE WHERE id = ? AND deleted = FALSE").bind(title_id).execute(pool).await?;
    if done.rows_affected() == 0 && sqlx::query_scalar::<_, u64>("SELECT id FROM title WHERE id = ? AND deleted = FALSE").bind(title_id).fetch_optional(pool).await?.is_none() {
        return Err(Error::not_found(format!("no title {title_id}")));
    }
    sqlx::query("DELETE FROM title_locales WHERE title_id = ? AND source = 'user'").bind(title_id).execute(pool).await?;
    let linked: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM title_provider_links WHERE title_id = ? AND provider = 'igdb' AND deleted = FALSE)").bind(title_id).fetch_one(pool).await?;
    if !linked {
        return Ok(None);
    }
    let reapply = JobRequest::IgdbReapply(romexplorer_types::jobs::IgdbReapplyRequest { title_ids: Some(vec![title_id]), ..Default::default() });
    Ok(jobs::enqueue_unless_queued(app, &reapply).await?)
}

/// Gives a release a title of its own (`split_releases`); returns that title.
pub async fn split_release(app: &App, release_id: u64) -> Result<u64> {
    split_releases(app, &ReleasesSplit { release_ids: vec![release_id] }).await
}

/// Gives releases one new title of their own, together (`title_merge::split_releases`);
/// returns that title.
pub async fn split_releases(app: &App, req: &ReleasesSplit) -> Result<u64> {
    let before = releases_and_titles(app, &req.release_ids).await?;
    let title_id = title_merge::split_releases(&app.pool, &req.release_ids).await?;
    after_releases_moved(app, &before, title_id).await?;
    Ok(title_id)
}

/// Takes a release out from under its title. Only for what stands without one
/// (`platform_map::STANDALONE`): a game release with no title would be in no list at all.
pub async fn unlink_release(app: &App, release_id: u64) -> Result<()> {
    let material: Option<Option<String>> = sqlx::query_scalar("SELECT material FROM `release` WHERE id = ? AND deleted = FALSE").bind(release_id).fetch_optional(&app.pool).await?;
    match material {
        None => Err(Error::not_found(format!("no release {release_id}"))),
        Some(m) if m.as_deref().is_some_and(|m| crate::database::platform_map::STANDALONE.contains(&m)) => {
            sqlx::query("DELETE FROM title_releases WHERE release_id = ?").bind(release_id).execute(&app.pool).await?;
            Ok(())
        }
        Some(_) => Err(Error::invalid("only disc media, documents and source code can be without a title; move a game release to another title instead")),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct KeywordRequest {
    pub title_ids: Vec<u64>,
    pub name: String,
}

/// Adds the user's keyword tag to titles (`keyword` + `title_keywords`, source `user`);
/// they filter like any keyword. Returns how many titles got it.
pub async fn add_keyword(app: &App, req: &KeywordRequest) -> Result<u64> {
    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(Error::invalid("a keyword tag is 1-64 characters"));
    }
    if super::audio::is_reserved_tag(name) {
        return Err(Error::invalid(format!("{} is the tag for generic background music: it goes on audio files, not on titles", super::audio::GENERIC_BGM_TAG)));
    }
    if req.title_ids.is_empty() || req.title_ids.len() > 1000 {
        return Err(Error::invalid("give 1-1000 titles"));
    }
    let mut added = 0;
    for id in &req.title_ids {
        let exists: Option<u64> = sqlx::query_scalar("SELECT id FROM title WHERE id = ? AND deleted = FALSE").bind(id).fetch_optional(&app.pool).await?;
        if exists.is_none() {
            continue;
        }
        crate::database::providers::metadata::set_title_keywords(&app.pool, *id, "user", &[name]).await?;
        added += 1;
    }
    if added == 0 {
        return Err(Error::not_found("none of those titles exist"));
    }
    Ok(added)
}

/// Removes one of the user's keyword tags from a title (provider keywords stay).
pub async fn remove_keyword(app: &App, title_id: u64, name: &str) -> Result<()> {
    let removed = sqlx::query("DELETE tk FROM title_keywords tk JOIN keyword k ON k.id = tk.keyword_id WHERE tk.title_id = ? AND k.name = ? AND tk.source IN ('user', 'auto')")
        .bind(title_id)
        .bind(name)
        .execute(&app.pool)
        .await?
        .rows_affected();
    if removed == 0 {
        return Err(Error::not_found(format!("title {title_id} has no user keyword {name:?}")));
    }
    Ok(())
}

// TODO(release): the edit endpoints below (keyword management, decoupling merges) are for
// admin users only once accounts exist; open to every client while testing.

#[derive(Debug, Clone, Deserialize)]
pub struct KeywordRemoveRequest {
    pub title_ids: Vec<u64>,
    pub name: String,
}

/// Removes the user's keyword tag from titles (provider keywords stay). Returns how many
/// titles had it.
pub async fn remove_keyword_bulk(app: &App, req: &KeywordRemoveRequest) -> Result<u64> {
    if req.title_ids.is_empty() || req.title_ids.len() > 1000 {
        return Err(Error::invalid("give 1-1000 titles"));
    }
    let list = req.title_ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let removed = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DELETE tk FROM title_keywords tk JOIN keyword k ON k.id = tk.keyword_id WHERE tk.title_id IN ({list}) AND k.name = ? AND tk.source IN ('user', 'auto')"
    )))
    .bind(req.name.trim())
    .execute(&app.pool)
    .await?
    .rows_affected();
    Ok(removed)
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct KeywordListQuery {
    /// user (default: keywords with user tags, and unused ones) | all
    pub scope: Option<String>,
    /// Only keywords no live title has.
    pub unused: Option<bool>,
    /// user | auto: only keywords put on titles or audio files by the user / by the
    /// auto-tagger.
    pub source: Option<String>,
    pub search: Option<String>,
    /// name (default) | titles
    pub sort: Option<String>,
    pub desc: Option<bool>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KeywordRow {
    pub id: u64,
    pub name: String,
    /// Live titles with this keyword (any source).
    pub titles: i64,
    /// Of those, tagged by the user.
    pub user_titles: i64,
    /// Of those, tagged by the auto-tagger.
    pub auto_titles: i64,
    /// Audio files carrying it as a tag, and of those the auto-tagger's.
    pub files: i64,
    pub auto_files: i64,
    /// Where the keyword comes from (user, igdb, ...).
    pub sources: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KeywordPage {
    pub items: Vec<KeywordRow>,
    pub total: i64,
}

pub async fn keywords(app: &App, q: &KeywordListQuery) -> Result<KeywordPage> {
    use sqlx::Row;
    let scope = q.scope.as_deref().unwrap_or("user");
    let mut cond = String::from(" WHERE 1=1");
    match scope {
        "user" => cond.push_str(
            " AND (EXISTS (SELECT 1 FROM title_keywords u WHERE u.keyword_id = k.id AND u.source IN ('user', 'auto')) OR EXISTS (SELECT 1 FROM audio_file_tags af WHERE af.name = k.name) \
             OR NOT EXISTS (SELECT 1 FROM title_keywords u WHERE u.keyword_id = k.id))",
        ),
        "all" => {}
        other => return Err(Error::invalid(format!("scope {other:?} must be user or all"))),
    }
    let like = q.search.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(|s| format!("%{}%", s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")));
    if like.is_some() {
        cond.push_str(" AND k.name LIKE ?");
    }
    match q.source.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some("user") => cond.push_str(" AND (EXISTS (SELECT 1 FROM title_keywords u WHERE u.keyword_id = k.id AND u.source = 'user') OR EXISTS (SELECT 1 FROM audio_file_tags af WHERE af.name = k.name AND af.source = 'manual'))"),
        Some("auto") => cond.push_str(" AND (EXISTS (SELECT 1 FROM title_keywords u WHERE u.keyword_id = k.id AND u.source = 'auto') OR EXISTS (SELECT 1 FROM audio_file_tags af WHERE af.name = k.name AND af.source = 'auto'))"),
        Some(other) => return Err(Error::invalid(format!("source {other:?} must be user or auto"))),
        None => {}
    }
    let having = if q.unused.unwrap_or(false) { " HAVING titles = 0" } else { "" };
    let order = match q.sort.as_deref().filter(|s| !s.is_empty()).unwrap_or("name") {
        "name" => "k.name",
        "titles" => "titles",
        other => return Err(Error::invalid(format!("sort {other:?} must be name or titles"))),
    };
    let dir = if q.desc.unwrap_or(false) { "DESC" } else { "ASC" };
    let (limit, offset) = super::clamp_page(q.limit, q.offset);
    let inner = format!(
        "SELECT k.id, k.name, COUNT(DISTINCT t.id) AS titles, COUNT(DISTINCT IF(tk.source = 'user', t.id, NULL)) AS user_titles, COUNT(DISTINCT IF(tk.source = 'auto', t.id, NULL)) AS auto_titles, \
         (SELECT COUNT(*) FROM audio_file_tags af WHERE af.name = k.name) AS files, (SELECT COUNT(*) FROM audio_file_tags af WHERE af.name = k.name AND af.source = 'auto') AS auto_files, \
         GROUP_CONCAT(DISTINCT tk.source ORDER BY tk.source) AS sources \
         FROM keyword k LEFT JOIN title_keywords tk ON tk.keyword_id = k.id LEFT JOIN title t ON t.id = tk.title_id AND t.deleted = FALSE{cond} GROUP BY k.id, k.name{having}"
    );
    let mut count = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM ({inner}) x")));
    if let Some(l) = &like {
        count = count.bind(l.clone());
    }
    let total = count.fetch_one(&app.pool).await?;
    let mut query = sqlx::query(sqlx::AssertSqlSafe(format!("{inner} ORDER BY {order} {dir}, k.id LIMIT {limit} OFFSET {offset}")));
    if let Some(l) = &like {
        query = query.bind(l.clone());
    }
    let items = query
        .fetch_all(&app.pool)
        .await?
        .iter()
        .map(|r| {
            Ok(KeywordRow {
                id: r.try_get("id")?,
                name: r.try_get("name")?,
                titles: r.try_get("titles")?,
                user_titles: r.try_get("user_titles")?,
                auto_titles: r.try_get("auto_titles")?,
                files: r.try_get("files")?,
                auto_files: r.try_get("auto_files")?,
                sources: r.try_get("sources")?,
            })
        })
        .collect::<std::result::Result<_, sqlx::Error>>()?;
    Ok(KeywordPage { items, total })
}

#[derive(Debug, Clone, Deserialize)]
pub struct KeywordRename {
    pub name: String,
}

/// Renames a keyword; into an existing name, the two become one. Provider keywords can be
/// renamed too, but the provider brings its own name back on its next fetch. Returns the
/// keyword's id afterwards.
pub async fn rename_keyword(app: &App, id: u64, req: &KeywordRename) -> Result<u64> {
    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(Error::invalid("a keyword is 1-64 characters"));
    }
    if super::audio::is_reserved_tag(name) {
        return Err(Error::invalid(format!("{} is the tag for generic background music: a keyword cannot take its name", super::audio::GENERIC_BGM_TAG)));
    }
    let current: Option<String> = sqlx::query_scalar("SELECT name FROM keyword WHERE id = ?").bind(id).fetch_optional(&app.pool).await?;
    if current.is_none() {
        return Err(Error::not_found(format!("no keyword {id}")));
    }
    let existing: Option<u64> = sqlx::query_scalar("SELECT id FROM keyword WHERE name = ? AND id <> ?").bind(name).bind(id).fetch_optional(&app.pool).await?;
    let mut tx = app.pool.begin().await?;
    let target = match existing {
        Some(other) => {
            sqlx::query("INSERT IGNORE INTO title_keywords (title_id, keyword_id, source) SELECT title_id, ?, source FROM title_keywords WHERE keyword_id = ?").bind(other).bind(id).execute(&mut *tx).await?;
            sqlx::query("INSERT IGNORE INTO keyword_media (keyword_id, media_id) SELECT ?, media_id FROM keyword_media WHERE keyword_id = ?").bind(other).bind(id).execute(&mut *tx).await?;
            sqlx::query("DELETE FROM title_keywords WHERE keyword_id = ?").bind(id).execute(&mut *tx).await?;
            sqlx::query("DELETE FROM keyword_media WHERE keyword_id = ?").bind(id).execute(&mut *tx).await?;
            sqlx::query("DELETE FROM keyword WHERE id = ?").bind(id).execute(&mut *tx).await?;
            other
        }
        None => {
            sqlx::query("UPDATE keyword SET name = ? WHERE id = ?").bind(name).bind(id).execute(&mut *tx).await?;
            id
        }
    };
    tx.commit().await?;
    Ok(target)
}

/// Deletes a keyword from every title (all sources) and the keyword itself. Returns how many
/// title tags went with it.
pub async fn delete_keyword(app: &App, id: u64) -> Result<u64> {
    let exists: Option<u64> = sqlx::query_scalar("SELECT id FROM keyword WHERE id = ?").bind(id).fetch_optional(&app.pool).await?;
    if exists.is_none() {
        return Err(Error::not_found(format!("no keyword {id}")));
    }
    let media: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM keyword_media WHERE keyword_id = ?").bind(id).fetch_one(&app.pool).await?;
    if media > 0 {
        return Err(Error::conflict("that keyword has images; remove them first"));
    }
    let mut tx = app.pool.begin().await?;
    let tags = sqlx::query("DELETE FROM title_keywords WHERE keyword_id = ?").bind(id).execute(&mut *tx).await?.rows_affected();
    sqlx::query("DELETE FROM keyword WHERE id = ?").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(tags)
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AutoTagRemove {
    /// These keywords; with neither this nor `search`, every automatic tag.
    pub ids: Option<Vec<u64>>,
    /// Keywords whose name contains this.
    pub search: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AutoTagRemoved {
    pub title_tags: u64,
    pub file_tags: u64,
}

/// Removes the auto-tagger's tags (titles and audio files) of the chosen keywords; the user's
/// own stay.
pub async fn remove_auto_tags(app: &App, req: &AutoTagRemove) -> Result<AutoTagRemoved> {
    let mut cond = String::new();
    let mut binds: Vec<String> = Vec::new();
    if let Some(ids) = req.ids.as_ref().filter(|ids| !ids.is_empty()) {
        cond.push_str(&format!(" AND k.id IN ({})", ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",")));
    }
    if let Some(s) = req.search.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        cond.push_str(" AND k.name LIKE ?");
        binds.push(super::like_pattern(s));
    }
    let mut q = sqlx::query(sqlx::AssertSqlSafe(format!("DELETE tk FROM title_keywords tk JOIN keyword k ON k.id = tk.keyword_id WHERE tk.source = 'auto'{cond}")));
    for b in &binds {
        q = q.bind(b);
    }
    let title_tags = q.execute(&app.pool).await?.rows_affected();
    let file_cond = if cond.is_empty() { String::new() } else { format!(" AND af.name IN (SELECT k.name FROM keyword k WHERE 1 = 1{cond})") };
    let mut q = sqlx::query(sqlx::AssertSqlSafe(format!("DELETE af FROM audio_file_tags af WHERE af.source = 'auto'{file_cond}")));
    for b in &binds {
        q = q.bind(b);
    }
    let file_tags = q.execute(&app.pool).await?.rows_affected();
    Ok(AutoTagRemoved { title_tags, file_tags })
}

/// Deletes every keyword no title has (and no image hangs on). Returns how many.
pub async fn delete_unused_keywords(app: &App) -> Result<u64> {
    Ok(sqlx::query(
        "DELETE k FROM keyword k WHERE NOT EXISTS (SELECT 1 FROM title_keywords tk WHERE tk.keyword_id = k.id) AND NOT EXISTS (SELECT 1 FROM keyword_media km WHERE km.keyword_id = k.id)",
    )
    .execute(&app.pool)
    .await?
    .rows_affected())
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MergeLogQuery {
    pub search: Option<String>,
    /// Only merges into this title.
    pub title_id: Option<u64>,
    /// manual | hash | name
    pub grade: Option<String>,
    /// Include decoupled merges (default false).
    pub include_undone: Option<bool>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MergeLogPage {
    pub items: Vec<title_merge::MergeLogRow>,
    pub total: i64,
}

pub async fn merge_log(app: &App, q: &MergeLogQuery) -> Result<MergeLogPage> {
    let (limit, offset) = super::clamp_page(q.limit, q.offset);
    let grade = q.grade.as_deref().filter(|g| !g.is_empty());
    if grade.is_some_and(|g| !title_merge::GRADES.contains(&g)) {
        return Err(Error::invalid("grade must be manual, hash or name"));
    }
    let (items, total) = title_merge::merge_log(&app.pool, q.search.as_deref(), q.title_id, grade, q.include_undone.unwrap_or(false), limit, offset).await?;
    Ok(MergeLogPage { items, total })
}

#[derive(Debug, Clone, Deserialize)]
pub struct DecoupleGradeRequest {
    /// manual | hash | name
    pub grade: String,
}

/// Decouples the merges of one grade, 500 a call (`title_merge::decouple_grade`); the
/// summary says how many are left, and the caller asks again.
pub async fn decouple_grade(app: &App, req: &DecoupleGradeRequest) -> Result<title_merge::DecoupleSummary> {
    if !title_merge::GRADES.contains(&req.grade.as_str()) {
        return Err(Error::invalid("grade must be manual, hash or name"));
    }
    let summary = title_merge::decouple_grade(&app.pool, &req.grade, 500).await?;
    sync_lead_igdb(app, None).await?;
    Ok(summary)
}

/// Splits one merge back out (`title_merge::decouple`); returns the title it became. Both
/// sides then take their IGDB game again (`sync_lead_igdb`), and the suggestion the merge
/// came from is pending in the review again (`title_merge::reopen_suggestion`).
pub async fn decouple(app: &App, log_id: u64) -> Result<u64> {
    let survivor: Option<u64> = sqlx::query_scalar("SELECT survivor_id FROM title_merge_log WHERE id = ?").bind(log_id).fetch_optional(&app.pool).await?;
    let heads = family::heads_of(&app.pool, &survivor.into_iter().collect::<Vec<_>>()).await?;
    let revived = title_merge::decouple(&app.pool, log_id).await?;
    let both: Vec<u64> = survivor.into_iter().chain([revived]).collect();
    sync_lead_igdb(app, Some(&both)).await?;
    family::after_edit(&app.pool, &both, &heads).await?;
    // Undone by the user: a merge the review suggested is up for review again.
    title_merge::reopen_suggestion(&app.pool, log_id).await?;
    Ok(revived)
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseMoveRequest {
    pub release_id: u64,
    /// The release it goes right after; null = to the top.
    pub after_release_id: Option<u64>,
}

/// Moves one of a title's releases in the user's manual order (`database::release_order`).
/// The first move fixes the order as displayed now. Returns the release ids in the new order.
pub async fn move_release(app: &App, title_id: u64, req: &ReleaseMoveRequest) -> Result<Vec<u64>> {
    use sqlx::Row;
    let rank = super::catalog::ReleaseRank::load(app).await?;
    let sql = format!(
        "SELECT r.id, tr.sort_weight FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE WHERE tr.title_id = ? ORDER BY {}, r.id",
        rank.sql("r")
    );
    let current: Vec<(u64, Option<f64>)> = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(title_id)
        .fetch_all(&app.pool)
        .await?
        .iter()
        .map(|r| Ok((r.try_get("id")?, r.try_get("sort_weight")?)))
        .collect::<std::result::Result<_, sqlx::Error>>()?;
    if current.is_empty() {
        return Err(Error::not_found(format!("title {title_id} has no releases")));
    }
    let weights = crate::database::release_order::plan_move(&current, req.release_id, req.after_release_id).map_err(Error::invalid)?;
    crate::database::release_order::write_weights(&app.pool, title_id, &weights).await?;
    sync_lead_igdb(app, Some(&[title_id])).await?;
    Ok(weights.into_iter().map(|(id, _)| id).collect())
}

/// Puts a title's releases back in the default order.
pub async fn reset_release_order(app: &App, title_id: u64) -> Result<u64> {
    let cleared = crate::database::release_order::clear(&app.pool, Some(&[title_id])).await?;
    sync_lead_igdb(app, Some(&[title_id])).await?;
    Ok(cleared)
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReleaseOrderQuery {
    pub search: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReleaseOrderRow {
    pub title_id: u64,
    pub title_name: String,
    pub top_release_id: Option<u64>,
    pub top_release_name: Option<String>,
    pub region: Option<String>,
    /// Releases with a manual place.
    pub placed: i64,
    pub set_at: Option<chrono::NaiveDateTime>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReleaseOrderPage {
    pub items: Vec<ReleaseOrderRow>,
    pub total: i64,
}

/// Titles whose releases the user ordered by hand (Edit > Manual order), latest first.
pub async fn release_orders(app: &App, q: &ReleaseOrderQuery) -> Result<ReleaseOrderPage> {
    use sqlx::Row;
    let like = q.search.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(|s| format!("%{}%", s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")));
    let cond = if like.is_some() { " AND t.name LIKE ?" } else { "" };
    let (limit, offset) = super::clamp_page(q.limit, q.offset);
    let base = format!("FROM title_releases tr JOIN title t ON t.id = tr.title_id AND t.deleted = FALSE WHERE tr.sort_weight IS NOT NULL{cond}");
    let mut count = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!("SELECT COUNT(DISTINCT tr.title_id) {base}")));
    if let Some(l) = &like {
        count = count.bind(l.clone());
    }
    let total = count.fetch_one(&app.pool).await?;
    let mut query = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT tr.title_id, t.name AS title_name, COUNT(*) AS placed, MAX(tr.sort_set_at) AS set_at {base} GROUP BY tr.title_id, t.name ORDER BY set_at DESC, tr.title_id LIMIT {limit} OFFSET {offset}"
    )));
    if let Some(l) = &like {
        query = query.bind(l.clone());
    }
    let mut items = Vec::new();
    for r in query.fetch_all(&app.pool).await? {
        let title_id: u64 = r.try_get("title_id")?;
        let top = sqlx::query("SELECT r.id, r.name, r.region FROM title_releases tr JOIN `release` r ON r.id = tr.release_id WHERE tr.title_id = ? AND tr.sort_weight IS NOT NULL ORDER BY tr.sort_weight LIMIT 1")
            .bind(title_id)
            .fetch_optional(&app.pool)
            .await?;
        items.push(ReleaseOrderRow {
            title_id,
            title_name: r.try_get("title_name")?,
            top_release_id: top.as_ref().map(|t| t.try_get("id")).transpose()?,
            top_release_name: top.as_ref().map(|t| t.try_get("name")).transpose()?,
            region: top.as_ref().map(|t| t.try_get::<Option<String>, _>("region")).transpose()?.flatten(),
            placed: r.try_get("placed")?,
            set_at: r.try_get("set_at")?,
        });
    }
    Ok(ReleaseOrderPage { items, total })
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseOrderClear {
    pub title_ids: Option<Vec<u64>>,
    /// Every title.
    pub all: Option<bool>,
}

/// Back to the default order: these titles, or all. Returns how many titles were reset.
pub async fn clear_release_orders(app: &App, req: &ReleaseOrderClear) -> Result<u64> {
    if req.all.unwrap_or(false) {
        return Ok(crate::database::release_order::clear(&app.pool, None).await?);
    }
    let ids = req.title_ids.clone().unwrap_or_default();
    if ids.is_empty() || ids.len() > 5000 {
        return Err(Error::invalid("give 1-5000 title_ids, or all: true"));
    }
    Ok(crate::database::release_order::clear(&app.pool, Some(&ids)).await?)
}
