//! What a patched release was made from, as the pages and the "patch links" dialog see it
//! (`database::release_bases`): the link on a release page with its confirm / change /
//! unlink, the patched ROMs of an original, the suggestions to look through, and the same
//! three buttons for a guessed language.

use serde::{Deserialize, Serialize};
use sqlx::{MySqlPool, Row};

use super::catalog::Page;
use super::{App, Error, Result};
use crate::database::release_bases;

/// The link of one release: what it is made from, and how that is known.
#[derive(Debug, Clone, Serialize)]
pub struct BaseLink {
    /// One of `release_bases::STATUSES`. `suggested` is shown as one, with the buttons.
    pub status: String,
    pub title_id: Option<u64>,
    pub title_name: Option<String>,
    pub release_id: Option<u64>,
    pub release_name: Option<String>,
}

/// A release made from another, in a list on the original's page.
#[derive(Debug, Clone, Serialize)]
pub struct PatchedRef {
    pub id: u64,
    pub name: String,
    pub platform: Option<String>,
    pub game_kind: Option<String>,
    pub material: Option<String>,
    pub languages: Option<String>,
    pub languages_state: Option<String>,
    pub status: String,
    pub local_games: u64,
}

const REF_SELECT: &str = "SELECT r.id, r.name, pl.name AS platform, r.game_kind, r.material, r.languages, r.languages_state, rb.status, \
     (SELECT COUNT(*) FROM local_release_games lrg JOIN local_game lg ON lg.id = lrg.game_id AND lg.deleted = FALSE WHERE lrg.release_id = r.id) AS local_games \
     FROM release_bases rb JOIN `release` r ON r.id = rb.release_id AND r.deleted = FALSE LEFT JOIN platforms pl ON pl.id = r.app_platform_id";

fn row_to_ref(r: &sqlx::mysql::MySqlRow) -> Result<PatchedRef> {
    Ok(PatchedRef {
        id: r.try_get("id")?,
        name: r.try_get("name")?,
        platform: r.try_get("platform")?,
        game_kind: r.try_get("game_kind")?,
        material: r.try_get("material")?,
        languages: r.try_get("languages")?,
        languages_state: r.try_get("languages_state")?,
        status: r.try_get("status")?,
        local_games: r.try_get::<i64, _>("local_games")? as u64,
    })
}

/// What release `id` is made from; `None` when nothing says, or the user unlinked it.
pub async fn of_release(pool: &MySqlPool, id: u64) -> Result<Option<BaseLink>> {
    let row = sqlx::query(
        "SELECT rb.status, rb.base_title_id, t.name AS title_name, rb.base_release_id, b.name AS release_name FROM release_bases rb \
         LEFT JOIN title t ON t.id = rb.base_title_id AND t.deleted = FALSE LEFT JOIN `release` b ON b.id = rb.base_release_id AND b.deleted = FALSE WHERE rb.release_id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(match row {
        Some(r) => Some(BaseLink { status: r.try_get("status")?, title_id: r.try_get("base_title_id")?, title_name: r.try_get("title_name")?, release_id: r.try_get("base_release_id")?, release_name: r.try_get("release_name")? }),
        None => None,
    })
}

/// The releases made from release `id`: an original's "patched ROMs of this release".
pub async fn made_from_release(pool: &MySqlPool, id: u64) -> Result<Vec<PatchedRef>> {
    sqlx::query(sqlx::AssertSqlSafe(format!("{REF_SELECT} WHERE rb.base_release_id = ? AND rb.status <> 'rejected' ORDER BY r.name LIMIT 500"))).bind(id).fetch_all(pool).await?.iter().map(row_to_ref).collect()
}

/// The releases made from title `id` that are under a title of their own (IGDB lists the
/// hack as a game): the title page lists them with its own patched ROMs.
pub async fn made_from_title_elsewhere(pool: &MySqlPool, id: u64) -> Result<Vec<PatchedRef>> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "{REF_SELECT} WHERE rb.base_title_id = ? AND rb.status <> 'rejected' AND NOT EXISTS (SELECT 1 FROM title_releases tr WHERE tr.release_id = r.id AND tr.title_id = rb.base_title_id) ORDER BY r.name LIMIT 500"
    )))
    .bind(id)
    .fetch_all(pool)
    .await?
    .iter()
    .map(row_to_ref)
    .collect()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LinkQuery {
    /// One of `release_bases::STATUSES`; `suggested` unless said.
    pub status: Option<String>,
    pub search: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// A row of the "patch links" dialog.
#[derive(Debug, Clone, Serialize)]
pub struct LinkRow {
    #[serde(flatten)]
    pub release: PatchedRef,
    pub base: BaseLink,
}

pub async fn list(app: &App, q: &LinkQuery) -> Result<Page<LinkRow>> {
    let pool = &app.pool;
    let status = q.status.as_deref().filter(|s| !s.is_empty()).unwrap_or("suggested");
    if !release_bases::STATUSES.contains(&status) {
        return Err(Error::invalid(format!("status {status:?} must be one of {}", release_bases::STATUSES.join(", "))));
    }
    let search = q.search.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(|s| format!("%{}%", s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")));
    let filter = if search.is_some() { " AND (r.name LIKE ? OR t.name LIKE ?)" } else { "" };
    let (limit, offset) = (q.limit.unwrap_or(50).clamp(1, 500), q.offset.unwrap_or(0));

    let count_sql = format!("SELECT COUNT(*) FROM release_bases rb JOIN `release` r ON r.id = rb.release_id AND r.deleted = FALSE LEFT JOIN title t ON t.id = rb.base_title_id WHERE rb.status = ?{filter}");
    let mut count = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(count_sql)).bind(status);
    if let Some(s) = &search {
        count = count.bind(s.clone()).bind(s.clone());
    }
    let total = count.fetch_one(pool).await? as u64;

    let page_sql = format!(
        "SELECT r.id, r.name, pl.name AS platform, r.game_kind, r.material, r.languages, r.languages_state, rb.status, \
         (SELECT COUNT(*) FROM local_release_games lrg JOIN local_game lg ON lg.id = lrg.game_id AND lg.deleted = FALSE WHERE lrg.release_id = r.id) AS local_games, \
         rb.base_title_id, t.name AS title_name, rb.base_release_id, b.name AS release_name \
         FROM release_bases rb JOIN `release` r ON r.id = rb.release_id AND r.deleted = FALSE LEFT JOIN platforms pl ON pl.id = r.app_platform_id \
         LEFT JOIN title t ON t.id = rb.base_title_id LEFT JOIN `release` b ON b.id = rb.base_release_id \
         WHERE rb.status = ?{filter} ORDER BY t.name, r.name, r.id LIMIT ? OFFSET ?"
    );
    let mut page = sqlx::query(sqlx::AssertSqlSafe(page_sql)).bind(status);
    if let Some(s) = &search {
        page = page.bind(s.clone()).bind(s.clone());
    }
    let items = page
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?
        .iter()
        .map(|r| -> Result<LinkRow> {
            Ok(LinkRow {
                release: row_to_ref(r)?,
                base: BaseLink { status: r.try_get("status")?, title_id: r.try_get("base_title_id")?, title_name: r.try_get("title_name")?, release_id: r.try_get("base_release_id")?, release_name: r.try_get("release_name")? },
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Page { items, total, limit, offset })
}

/// What `POST /catalog/base-links/bulk` is asked: confirm or unlink the listed releases'
/// links, or -- with `all` -- every suggestion.
#[derive(Debug, Clone, Deserialize)]
pub struct BulkLinks {
    /// `accept` | `reject`.
    pub action: String,
    #[serde(default)]
    pub ids: Vec<u64>,
    #[serde(default)]
    pub all: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct BulkDone {
    pub changed: u64,
}

pub async fn bulk(app: &App, req: &BulkLinks) -> Result<BulkDone> {
    if !req.all && req.ids.is_empty() {
        return Err(Error::invalid("no links selected"));
    }
    if req.ids.len() > 5000 {
        return Err(Error::invalid("give at most 5000 links at once, or all"));
    }
    let ids = if req.all { None } else { Some(req.ids.as_slice()) };
    let changed = match req.action.as_str() {
        "accept" => release_bases::confirm(&app.pool, ids).await?,
        "reject" => release_bases::reject(&app.pool, ids).await?,
        _ => return Err(Error::invalid("action must be accept or reject")),
    };
    Ok(BulkDone { changed })
}

/// What `POST /catalog/releases/{id}/base` is asked.
#[derive(Debug, Clone, Deserialize)]
pub struct BaseRequest {
    /// `confirm` the link as it stands, `set` it to `title_id` and / or `release_id`, or
    /// `unlink` it.
    pub action: String,
    pub title_id: Option<u64>,
    pub release_id: Option<u64>,
}

pub async fn set(app: &App, release_id: u64, req: &BaseRequest) -> Result<Option<BaseLink>> {
    let pool = &app.pool;
    if sqlx::query_scalar::<_, u64>("SELECT id FROM `release` WHERE id = ? AND deleted = FALSE").bind(release_id).fetch_optional(pool).await?.is_none() {
        return Err(Error::not_found(format!("no release {release_id}")));
    }
    match req.action.as_str() {
        "confirm" => {
            if release_bases::confirm(pool, Some(&[release_id])).await? == 0 && of_release(pool, release_id).await?.is_none_or(|b| b.status != "manual") {
                return Err(Error::invalid("this release has no link to confirm"));
            }
        }
        "set" => release_bases::set_base(pool, release_id, req.title_id, req.release_id).await?,
        "unlink" => {
            release_bases::reject(pool, Some(&[release_id])).await?;
        }
        _ => return Err(Error::invalid("action must be confirm, set or unlink")),
    }
    of_release(pool, release_id).await
}

/// What `POST /catalog/releases/{id}/language` is asked: see `release_bases::set_language`.
#[derive(Debug, Clone, Deserialize)]
pub struct LanguageRequest {
    pub action: String,
    pub languages: Option<String>,
}

pub async fn language(app: &App, release_id: u64, req: &LanguageRequest) -> Result<()> {
    Ok(release_bases::set_language(&app.pool, release_id, &req.action, req.languages.as_deref()).await?)
}
