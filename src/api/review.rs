//! Edit › Merge review and Edit › Aspect review: decisions taken one at a time, like a
//! discovery queue. Each queue is narrowed by the Titles list's own filters (a set included),
//! so a month spent on one set can review just that set.
//!
//! Merge review walks `title_merge_suggestions` (`database::title_review` writes the regional
//! kinds), ordered by `merge_review_order`. A suggestion's choice is applied here: whole titles
//! merged, or releases moved into the titles the choice names. For an International or a
//! Director's Cut it can also ask the web what differs (`lookup`): the in-app AI (`api::ai`,
//! saved per suggestion and language), and always an Ask menu of browser links.
//!
//! Aspect review walks the titles whose 4:3 / 8:7 (or square-pixel) question is still open:
//! no release of a title on that platform has the user's answer.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::common::models::Platform;
use crate::database::settings as db_settings;
use crate::database::title_merge;
use crate::database::title_review::{self, ReviewDetail, ReviewOption};

use super::ai;
use super::catalog::{self, ListQuery};
use super::titles::{self, ReleasesMove, ReleasesSplit};
use super::{App, Error, Result};

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReviewParams {
    /// Merge review: one suggestion kind only (`international`, `igdb`, ...).
    pub kind: Option<String>,
    /// Merge review: `todo` (default: pending), `done` (taken or said no to) or `all`.
    pub state: Option<String>,
    /// Aspect review: only titles with a screenshot or title screen to judge by.
    #[serde(default)]
    pub with_screenshots: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewPage<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub offset: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewRelease {
    pub id: u64,
    pub name: String,
    pub region: Option<String>,
    pub platform: Option<String>,
    pub title_id: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewTitle {
    pub id: u64,
    pub name: String,
    pub category: Option<String>,
    pub platforms: Option<String>,
    pub igdb: Option<String>,
    pub cover_media_id: Option<u64>,
    pub deleted: bool,
    pub releases: Vec<ReviewRelease>,
    pub release_total: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MergeItem {
    pub key: String,
    pub kind: String,
    pub reason: String,
    pub status: String,
    pub variant: Option<String>,
    pub matched: Vec<String>,
    pub options: Vec<ReviewOption>,
    pub titles: Vec<ReviewTitle>,
    /// Every release an option names, so a choice can be shown as the titles it makes.
    pub releases: HashMap<u64, ReviewRelease>,
    /// Whether `lookup` has something to ask.
    pub can_look_up: bool,
}

/// Whether the query narrows titles at all (else the queue is the whole backlog, and the
/// Titles list is not run for nothing).
fn narrows(q: &ListQuery) -> bool {
    let some = |o: &Option<String>| o.as_deref().is_some_and(|v| !v.trim().is_empty());
    some(&q.search)
        || q.platform.is_some()
        || some(&q.platforms)
        || q.set_id.is_some()
        || some(&q.genre_major)
        || some(&q.genres)
        || some(&q.franchises)
        || some(&q.companies)
        || some(&q.keywords)
        || some(&q.tags)
        || some(&q.tag_source)
        || some(&q.region)
        || some(&q.language)
        || some(&q.provider)
        || some(&q.kinds)
        || some(&q.material)
        || some(&q.display_aspect)
        || some(&q.display_widescreen)
        || some(&q.display_refresh)
        || some(&q.display_res)
        || some(&q.display_scan)
        || some(&q.display_state)
        || q.view.as_deref().is_some_and(|v| !matches!(v, "" | "collection" | "collection_local_first"))
        || q.igdb.is_some()
        || q.basegame.is_some()
}

async fn matching_titles(app: &App, q: &ListQuery) -> Result<Option<HashSet<u64>>> {
    if !narrows(q) {
        return Ok(None);
    }
    let q = ListQuery { limit: None, offset: None, sort: None, ..q.clone() };
    Ok(Some(catalog::title_ids_matching(app, &q).await?.into_iter().collect()))
}

/// The queue's order: `regional` (default) puts the regional kinds first, `english` the
/// suggestions of titles sharing an IGDB game.
pub async fn merge_review_order(app: &App) -> Result<String> {
    Ok(db_settings::get::<String>(&app.pool, db_settings::KEY_MERGE_REVIEW_ORDER).await?.filter(|v| v == "english").unwrap_or_else(|| "regional".into()))
}

fn list(ids: &[u64]) -> String {
    ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",")
}

/// The pending suggestions, one page.
pub async fn merge_queue(app: &App, q: &ListQuery, p: &ReviewParams) -> Result<ReviewPage<MergeItem>> {
    let order = if merge_review_order(app).await? == "english" { "(kind = 'igdb') DESC, priority DESC" } else { "priority DESC" };
    let status = match p.state.as_deref() {
        Some("done") => "status <> 'pending'",
        Some("all") => "1=1",
        _ => "status = 'pending'",
    };
    // Still to do first, then the newest decisions.
    let mut sql = format!("SELECT igdb_id, title_ids, kind FROM title_merge_suggestions WHERE {status}");
    if p.kind.is_some() {
        sql.push_str(" AND kind = ?");
    }
    sql.push_str(&format!(" ORDER BY status <> 'pending', IF(status = 'pending', 0, UNIX_TIMESTAMP(updated_at)) DESC, {order}, updated_at DESC, igdb_id"));
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    if let Some(kind) = &p.kind {
        query = query.bind(kind);
    }
    let rows = query.fetch_all(&app.pool).await?;
    let only = matching_titles(app, q).await?;
    let mut keys: Vec<String> = Vec::new();
    for r in &rows {
        let titles: Vec<u64> = r.try_get::<String, _>("title_ids")?.split(',').filter_map(|x| x.parse().ok()).collect();
        if only.as_ref().is_none_or(|o| titles.iter().any(|t| o.contains(t))) {
            keys.push(r.try_get("igdb_id")?);
        }
    }
    let (limit, offset) = super::clamp_page(q.limit.or(Some(20)), q.offset);
    let mut items = Vec::new();
    for key in keys.iter().skip(offset as usize).take(limit as usize) {
        if let Some(item) = merge_item(app, key).await? {
            items.push(item);
        }
    }
    Ok(ReviewPage { items, total: keys.len() as u64, offset })
}

async fn releases_by_id(app: &App, ids: &[u64]) -> Result<HashMap<u64, ReviewRelease>> {
    let mut out = HashMap::new();
    for chunk in ids.chunks(1000) {
        let sql = format!(
            "SELECT r.id, r.name, r.region, p.name AS platform, (SELECT tr.title_id FROM title_releases tr WHERE tr.release_id = r.id LIMIT 1) AS title_id \
             FROM `release` r LEFT JOIN platforms p ON p.id = r.app_platform_id WHERE r.id IN ({})",
            list(chunk)
        );
        for r in sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(&app.pool).await? {
            let id: u64 = r.try_get("id")?;
            out.insert(id, ReviewRelease { id, name: r.try_get("name")?, region: r.try_get("region")?, platform: r.try_get("platform")?, title_id: r.try_get("title_id")? });
        }
    }
    Ok(out)
}

/// A title as a review card shows it: its covers, platforms, IGDB game and releases.
async fn review_titles(app: &App, ids: &[u64]) -> Result<Vec<ReviewTitle>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT t.id, t.name, CAST(t.category AS CHAR) AS category, t.deleted, \
           (SELECT GROUP_CONCAT(DISTINCT p.name ORDER BY p.name SEPARATOR ', ') FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE JOIN platforms p ON p.id = r.app_platform_id WHERE tr.title_id = t.id) AS platforms, \
           (SELECT tpl.provider_id FROM title_provider_links tpl WHERE tpl.title_id = t.id AND tpl.provider = 'igdb' AND tpl.deleted = FALSE) AS igdb, \
           (SELECT m.id FROM title_releases trc JOIN release_media rm ON rm.release_id = trc.release_id JOIN media m ON m.id = rm.media_id AND m.deleted = FALSE AND m.media_type IN ('box', 'cover') \
             WHERE trc.title_id = t.id ORDER BY {}, m.id LIMIT 1) AS cover_media_id, \
           (SELECT COUNT(*) FROM title_releases trn JOIN `release` rn ON rn.id = trn.release_id AND rn.deleted = FALSE WHERE trn.title_id = t.id) AS release_total \
         FROM title t WHERE t.id IN ({}) ORDER BY t.id",
        catalog::MEDIA_ORDER,
        list(ids)
    );
    let mut out = Vec::new();
    for r in sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(&app.pool).await? {
        let id: u64 = r.try_get("id")?;
        let releases = sqlx::query(
            "SELECT r.id, r.name, r.region, p.name AS platform FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE LEFT JOIN platforms p ON p.id = r.app_platform_id \
             WHERE tr.title_id = ? ORDER BY r.material IS NOT NULL, r.name LIMIT 40",
        )
        .bind(id)
        .fetch_all(&app.pool)
        .await?
        .iter()
        .map(|x| Ok(ReviewRelease { id: x.try_get("id")?, name: x.try_get("name")?, region: x.try_get("region")?, platform: x.try_get("platform")?, title_id: Some(id) }))
        .collect::<std::result::Result<Vec<_>, sqlx::Error>>()?;
        out.push(ReviewTitle {
            id,
            name: r.try_get("name")?,
            category: r.try_get("category")?,
            platforms: r.try_get("platforms")?,
            igdb: r.try_get("igdb")?,
            cover_media_id: r.try_get("cover_media_id")?,
            deleted: r.try_get("deleted")?,
            releases,
            release_total: r.try_get("release_total")?,
        });
    }
    Ok(out)
}

/// One suggestion as the queue shows it. The IGDB-game ones have one choice: merge.
pub async fn merge_item(app: &App, key: &str) -> Result<Option<MergeItem>> {
    let Some(r) = sqlx::query("SELECT igdb_id, title_ids, reason, kind, status, CAST(detail AS CHAR) AS detail FROM title_merge_suggestions WHERE igdb_id = ?").bind(key).fetch_optional(&app.pool).await? else {
        return Ok(None);
    };
    let title_ids: Vec<u64> = r.try_get::<String, _>("title_ids")?.split(',').filter_map(|x| x.parse().ok()).collect();
    let kind: String = r.try_get("kind")?;
    let detail: ReviewDetail = match r.try_get::<Option<String>, _>("detail")? {
        Some(d) => serde_json::from_str(&d).map_err(|e| Error::internal(format!("suggestion {key}: {e}")))?,
        None => ReviewDetail { options: vec![ReviewOption { label: "merge".into(), recommended: true, merge: title_ids.clone(), ..Default::default() }], ..Default::default() },
    };
    let named: Vec<u64> = detail.options.iter().flat_map(|o| o.groups.iter().flatten().copied()).collect::<BTreeSet<_>>().into_iter().collect();
    // The titles the choices' releases sit in now, beside the ones recorded.
    let releases = releases_by_id(app, &named).await?;
    let all_titles: Vec<u64> = title_ids.iter().copied().chain(releases.values().filter_map(|x| x.title_id)).collect::<BTreeSet<_>>().into_iter().collect();
    Ok(Some(MergeItem {
        key: r.try_get("igdb_id")?,
        can_look_up: matches!(kind.as_str(), title_review::KIND_INTERNATIONAL | title_review::KIND_DIRECTORS_CUT),
        kind,
        reason: r.try_get("reason")?,
        status: r.try_get("status")?,
        variant: detail.variant,
        matched: detail.matched,
        options: detail.options,
        titles: review_titles(app, &all_titles).await?,
        releases,
    }))
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ApplyRequest {
    /// Which of the suggestion's options (0 for the IGDB-game kind's merge).
    #[serde(default)]
    pub option: usize,
    /// The user's own grouping instead of an option (releases dragged between the titles on
    /// the card): each group into its title, or a new title when it names none.
    #[serde(default)]
    pub layout: Option<Vec<LayoutGroup>>,
    /// `lookup`: the UI language the answer should come back in (en, ja, fr, ...).
    #[serde(default)]
    pub lang: Option<String>,
    /// `lookup`: ask again and replace the saved answer (not a pinned one).
    #[serde(default)]
    pub again: bool,
    /// `lookup`: ask in `lang` although only another language's answer is saved.
    #[serde(default)]
    pub ask: bool,
}

/// One title of a grouping the user arranged on a merge review card.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LayoutGroup {
    /// The title the releases go into; none: a new title, named after the first of them.
    #[serde(default)]
    pub title_id: Option<u64>,
    pub releases: Vec<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyResult {
    /// The titles the choice left (the merged one, or one per group).
    pub title_ids: Vec<u64>,
}

/// Takes one of a suggestion's choices: merges the titles, or moves the releases into the
/// titles the choice names -- each group into the title already holding most of it, or a new
/// one when that title went to an earlier group. A pending or dismissed one can be taken.
pub async fn apply(app: &App, key: &str, req: &ApplyRequest) -> Result<ApplyResult> {
    let row = sqlx::query("SELECT title_ids, kind, status, CAST(detail AS CHAR) AS detail FROM title_merge_suggestions WHERE igdb_id = ?").bind(key).fetch_optional(&app.pool).await?.ok_or_else(|| Error::not_found(format!("no suggestion {key}")))?;
    if row.try_get::<String, _>("status")? == "accepted" {
        return Err(Error::invalid("this suggestion was taken already"));
    }
    let title_ids: Vec<u64> = row.try_get::<String, _>("title_ids")?.split(',').filter_map(|x| x.parse().ok()).collect();
    let detail: Option<ReviewDetail> = row.try_get::<Option<String>, _>("detail")?.map(|d| serde_json::from_str(&d)).transpose().map_err(|e| Error::internal(format!("suggestion {key}: {e}")))?;
    if let Some(layout) = &req.layout {
        let result = ApplyResult { title_ids: apply_layout(app, layout).await? };
        sqlx::query("UPDATE title_merge_suggestions SET status = 'accepted', updated_at = NOW() WHERE igdb_id = ?").bind(key).execute(&app.pool).await?;
        return Ok(result);
    }
    let result = match detail {
        // A suggestion of titles sharing an IGDB game: merged platform by platform, each
        // title's own game kept on its releases.
        None => {
            titles::keep_igdb_games(app, &title_ids).await?;
            let survivor = title_merge::resolve_suggestion(&app.pool, key, true).await?;
            let kept: Vec<u64> = survivor.into_iter().collect();
            titles::sync_lead_igdb(app, Some(&kept)).await?;
            return Ok(ApplyResult { title_ids: kept });
        }
        Some(detail) => {
            let option = detail.options.get(req.option).ok_or_else(|| Error::invalid(format!("suggestion {key} has no choice {}", req.option)))?;
            if !option.merge.is_empty() {
                let live: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT id FROM title WHERE id IN ({}) AND deleted = FALSE", list(&option.merge)))).fetch_all(&app.pool).await?;
                if live.len() < 2 {
                    return Err(Error::invalid("those titles are not there any more: nothing to merge"));
                }
                let merged = titles::merge(app, &titles::MergeRequest { title_ids: live }).await?;
                ApplyResult { title_ids: vec![merged.title_id] }
            } else {
                ApplyResult { title_ids: apply_groups(app, &option.groups).await? }
            }
        }
    };
    sqlx::query("UPDATE title_merge_suggestions SET status = 'accepted', updated_at = NOW() WHERE igdb_id = ?").bind(key).execute(&app.pool).await?;
    Ok(result)
}

/// The user's own grouping ([`LayoutGroup`]): the releases of each group go into its title
/// (those already there stay), a group naming no title becomes a new one (`split_releases`,
/// so it is logged and can be undone under Edit › Combined titles). A title left with nothing
/// is gone. Returns the titles the groups ended up in.
async fn apply_layout(app: &App, layout: &[LayoutGroup]) -> Result<Vec<u64>> {
    let every: Vec<u64> = layout.iter().flat_map(|g| g.releases.iter().copied()).collect();
    if every.is_empty() {
        return Err(Error::invalid("this grouping names no releases"));
    }
    if every.len() != every.iter().collect::<HashSet<_>>().len() {
        return Err(Error::invalid("a release is in two groups"));
    }
    let involved: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT DISTINCT title_id FROM title_releases WHERE release_id IN ({})", list(&every)))).fetch_all(&app.pool).await?;
    titles::keep_igdb_games(app, &involved).await?;
    let mut out = Vec::new();
    // Into existing titles first; a group's own title is never emptied by this.
    for g in layout.iter().filter(|g| g.title_id.is_some() && !g.releases.is_empty()) {
        let target = g.title_id.unwrap_or_default();
        let away: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT r.id FROM `release` r WHERE r.id IN ({}) AND r.deleted = FALSE AND NOT EXISTS (SELECT 1 FROM title_releases tr WHERE tr.release_id = r.id AND tr.title_id = ?)",
            list(&g.releases)
        )))
        .bind(target)
        .fetch_all(&app.pool)
        .await?;
        if !away.is_empty() {
            titles::move_releases(app, &ReleasesMove { release_ids: away, title_id: target }).await?;
        }
        out.push(target);
    }
    for g in layout.iter().filter(|g| g.title_id.is_none() && !g.releases.is_empty()) {
        let live: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT id FROM `release` WHERE id IN ({}) AND deleted = FALSE", list(&g.releases)))).fetch_all(&app.pool).await?;
        if live.is_empty() {
            continue;
        }
        // In the order dragged: the first names the new title.
        let ordered: Vec<u64> = g.releases.iter().copied().filter(|id| live.contains(id)).collect();
        out.push(titles::split_releases(app, &ReleasesSplit { release_ids: ordered }).await?);
    }
    Ok(out)
}

/// Puts each group of releases under a title of its own (see [`apply`]). Returns the titles.
async fn apply_groups(app: &App, groups: &[Vec<u64>]) -> Result<Vec<u64>> {
    let every: Vec<u64> = groups.iter().flatten().copied().collect();
    if every.is_empty() {
        return Err(Error::invalid("this choice names no releases"));
    }
    let involved: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT DISTINCT title_id FROM title_releases WHERE release_id IN ({})", list(&every)))).fetch_all(&app.pool).await?;
    // A title's own IGDB game stays on its releases when they part (`keep_igdb_games`).
    titles::keep_igdb_games(app, &involved).await?;
    let mut used: HashSet<u64> = HashSet::new();
    let mut out = Vec::new();
    for group in groups {
        let live: Vec<u64> = if group.is_empty() {
            Vec::new()
        } else {
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT id FROM `release` WHERE id IN ({}) AND deleted = FALSE", list(group)))).fetch_all(&app.pool).await?
        };
        if live.is_empty() {
            continue;
        }
        let holders: Vec<(u64, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT tr.title_id, COUNT(*) AS n FROM title_releases tr JOIN title t ON t.id = tr.title_id AND t.deleted = FALSE WHERE tr.release_id IN ({}) GROUP BY tr.title_id ORDER BY n DESC, tr.title_id",
            list(&live)
        )))
        .fetch_all(&app.pool)
        .await?;
        let title = match holders.iter().map(|(t, _)| *t).find(|t| !used.contains(t)) {
            Some(target) => {
                let away: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                    "SELECT r.id FROM `release` r WHERE r.id IN ({}) AND NOT EXISTS (SELECT 1 FROM title_releases tr WHERE tr.release_id = r.id AND tr.title_id = ?)",
                    list(&live)
                )))
                .bind(target)
                .fetch_all(&app.pool)
                .await?;
                if !away.is_empty() {
                    titles::move_releases(app, &ReleasesMove { release_ids: away, title_id: target }).await?;
                }
                target
            }
            None => titles::split_releases(app, &ReleasesSplit { release_ids: live }).await?,
        };
        used.insert(title);
        out.push(title);
    }
    Ok(out)
}

/// Says no. The two titles of a merge suggestion are kept apart from then on (the automatic
/// pass too); a regrouping that was turned down leaves the titles as they are.
pub async fn dismiss(app: &App, key: &str) -> Result<()> {
    let row = sqlx::query("SELECT kind, CAST(detail AS CHAR) AS detail FROM title_merge_suggestions WHERE igdb_id = ?").bind(key).fetch_optional(&app.pool).await?.ok_or_else(|| Error::not_found(format!("no suggestion {key}")))?;
    let kind: String = row.try_get("kind")?;
    if matches!(kind.as_str(), title_review::KIND_TRANSLATED_NAME | title_review::KIND_BUNDLE) {
        if let Some(detail) = row.try_get::<Option<String>, _>("detail")?.and_then(|d| serde_json::from_str::<ReviewDetail>(&d).ok()) {
            for option in detail.options {
                if let [a, b] = option.merge[..] {
                    sqlx::query("INSERT IGNORE INTO title_merge_blocks (title_a, title_b) VALUES (?, ?)").bind(a.min(b)).bind(a.max(b)).execute(&app.pool).await?;
                }
            }
        }
    }
    title_merge::resolve_suggestion(&app.pool, key, false).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// lookup
// ---------------------------------------------------------------------------

/// What [`lookup`] answers: the question, its links, and the AI's answer (saved or new).
#[derive(Debug, Clone, Default, Serialize)]
pub struct LookupAnswer {
    /// As sent: the English question and its "Answer in <Language>." line.
    pub question: String,
    /// The bare question as a Google search.
    pub google_url: String,
    /// The Ask menu: the question in Google AI Mode, Claude, ChatGPT and Perplexity, the
    /// `ai_ask_first` one first. The user's own sign-in, no cost here.
    pub ask_links: Vec<ai::AskLink>,
    /// The in-app AI (`ai_provider`) and the language asked in.
    pub provider: String,
    pub lang: String,
    #[serde(flatten)]
    pub result: ai::Lookup,
}

/// The `ai_answers` purpose of Merge review's lookups.
pub const LOOKUP_PURPOSE: &str = "merge_review.differences";

/// The question a variant suggestion asks: what differs between the Japanese original, the
/// foreign release and the variant, and which of the two the variant is closer to.
fn question(variant: &str, platform: &str, original: &str, foreign: &[(String, String)], variant_name: &str) -> String {
    let foreign_list = foreign.iter().map(|(n, r)| format!("\"{n}\" ({r})")).collect::<Vec<_>>().join(", ");
    format!(
        "On {platform}: what are the differences between the Japanese original \"{original}\", the foreign release {foreign_list}, and \"{variant_name}\" ({variant})? \
         Is \"{variant_name}\" closer in content to the Japanese original or to the foreign release? Answer briefly: the main differences, then which one it is closer to."
    )
}

fn google_url(q: &str) -> String {
    reqwest::Url::parse_with_params("https://www.google.com/search", &[("q", q)]).map(|u| u.to_string()).unwrap_or_default()
}

/// What the web says about an International / Director's Cut suggestion: always the question
/// and its links; the in-app AI's answer from the saved answers (`api::ai`), else asked --
/// automatically only when nothing about this suggestion is saved in any language. `again`
/// asks again and replaces the saved answer, `ask` asks in this language when only another
/// one is saved.
pub async fn lookup(app: &App, key: &str, req: &ApplyRequest) -> Result<LookupAnswer> {
    let item = merge_item(app, key).await?.ok_or_else(|| Error::not_found(format!("no suggestion {key}")))?;
    if !item.can_look_up {
        return Err(Error::invalid("only International and Director's Cut suggestions are looked up"));
    }
    let variant = item.variant.clone().unwrap_or_default();
    // The first choice is (foreign + variant, Japanese original).
    let with = item.options.iter().find(|o| o.label == "with_foreign").ok_or_else(|| Error::internal("variant suggestion without its choices"))?;
    let name = |id: &u64| item.releases.get(id).map(|r| title_merge::base_title(&r.name)).unwrap_or_default();
    let jp: Vec<u64> = with.groups.get(1).cloned().unwrap_or_default();
    let (variants, foreign): (Vec<u64>, Vec<u64>) = with.groups.first().cloned().unwrap_or_default().into_iter().partition(|id| item.releases.get(id).is_some_and(|r| title_review::variant_of(&r.name).is_some()));
    let mut foreign_names: Vec<(String, String)> = Vec::new();
    for id in &foreign {
        let entry = (name(id), item.releases.get(id).and_then(|r| r.region.clone()).unwrap_or_default());
        if !foreign_names.contains(&entry) {
            foreign_names.push(entry);
        }
    }
    foreign_names.truncate(2);
    let platform = variants.first().and_then(|id| item.releases.get(id)).and_then(|r| r.platform.clone()).unwrap_or_default();
    let q = question(&variant, &platform, &jp.first().map(name).unwrap_or_default(), &foreign_names, &variants.first().map(name).unwrap_or_default());
    let lang = ai::language_code(req.lang.as_deref()).unwrap_or("en");
    let asked = ai::with_language(&q, Some(lang));
    // The suggestion's key is made of IGDB ids, not row ids: with the platform and the
    // variant it names the same question on any install.
    let subject = format!("{key}|{platform}|{variant}");
    let spend = if req.again {
        ai::Spend::Again
    } else if req.ask {
        ai::Spend::IfMissing
    } else {
        ai::Spend::IfNothingSaved
    };
    let result = ai::lookup(app, LOOKUP_PURPOSE, &subject, Some(lang), &q, spend).await?;
    Ok(LookupAnswer { google_url: google_url(&q), ask_links: ai::ask_links(&asked, &ai::ask_first(app).await?), question: asked, provider: ai::provider(app).await?, lang: lang.to_string(), result })
}

// ---------------------------------------------------------------------------
// aspect review
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct AspectItem {
    pub title_id: u64,
    pub title_name: String,
    /// The release to look at: the title's best-ranked one still open on this platform.
    pub release_id: u64,
    pub release_name: String,
    pub platform_id: u16,
    pub platform: Option<String>,
    /// What the picker shows beside 4:3 (`8:7` | `square`), and the value shown now.
    pub compare: String,
    pub value: Option<String>,
    /// The releases a pick applies to: the title's on this platform with no answer yet.
    pub apply_to: Vec<u64>,
    /// No release of the title has an answer on any platform (these come first).
    pub untouched: bool,
    /// A screenshot or title screen to judge by (else the picker shows the box).
    pub has_screenshot: bool,
}

/// The platforms whose aspect is a per-game question.
fn aspect_platforms() -> Vec<u16> {
    crate::database::connection::all_platforms().into_iter().filter(|p| crate::common::display::aspect_comparison(*p).is_some()).map(Platform::igdb_id).collect()
}

/// The open aspect questions, one per title and platform: titles none of whose releases has
/// an answer first, then the rest, by name.
pub async fn aspect_queue(app: &App, q: &ListQuery, p: &ReviewParams) -> Result<ReviewPage<AspectItem>> {
    let platforms = aspect_platforms();
    let only = matching_titles(app, q).await?;
    let rank = catalog::ReleaseRank::load(app).await?;
    let shots = "EXISTS (SELECT 1 FROM title_releases ts JOIN release_media rm ON rm.release_id = ts.release_id JOIN media m ON m.id = rm.media_id AND m.deleted = FALSE AND m.media_type IN ('screenshot', 'title_screen') WHERE ts.title_id = tr.title_id) \
                 OR EXISTS (SELECT 1 FROM title_media tm JOIN media m ON m.id = tm.media_id AND m.deleted = FALSE AND m.media_type IN ('screenshot', 'title_screen') WHERE tm.title_id = tr.title_id)";
    let mut sql = format!(
        "SELECT tr.title_id, MAX(t.name) AS title_name, r.app_platform_id, GROUP_CONCAT(r.id ORDER BY {} SEPARATOR ',') AS ids, \
           NOT EXISTS (SELECT 1 FROM title_releases tx JOIN release_display d ON d.release_id = tx.release_id AND d.field = 'aspect' AND d.source = 'user' WHERE tx.title_id = tr.title_id) AS untouched, \
           ({shots}) AS has_screenshot \
         FROM `release` r JOIN title_releases tr ON tr.release_id = r.id JOIN title t ON t.id = tr.title_id AND t.deleted = FALSE \
         WHERE r.deleted = FALSE AND r.material IS NULL AND r.app_platform_id IN ({}) \
           AND NOT EXISTS (SELECT 1 FROM release_display d WHERE d.release_id = r.id AND d.field = 'aspect' AND d.source = 'user')",
        rank.sql("r"),
        platforms.iter().map(u16::to_string).collect::<Vec<_>>().join(",")
    );
    if let Some(only) = &only {
        if only.is_empty() {
            return Ok(ReviewPage { items: Vec::new(), total: 0, offset: 0 });
        }
        sql.push_str(&format!(" AND tr.title_id IN ({})", only.iter().map(u64::to_string).collect::<Vec<_>>().join(",")));
    }
    if p.with_screenshots {
        sql.push_str(&format!(" AND ({shots})"));
    }
    sql.push_str(" GROUP BY tr.title_id, r.app_platform_id ORDER BY untouched DESC, title_name, tr.title_id, r.app_platform_id");
    let groups = sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(&app.pool).await?;
    let (limit, offset) = super::clamp_page(q.limit.or(Some(20)), q.offset);
    let mut items = Vec::new();
    for g in groups.iter().skip(offset as usize).take(limit as usize) {
        let ids: Vec<u64> = g.try_get::<String, _>("ids")?.split(',').filter_map(|x| x.parse().ok()).collect();
        let Some(&first) = ids.first() else { continue };
        let platform_id: u16 = g.try_get("app_platform_id")?;
        let row = sqlx::query("SELECT r.name, r.region, p.name AS platform FROM `release` r LEFT JOIN platforms p ON p.id = r.app_platform_id WHERE r.id = ?").bind(first).fetch_one(&app.pool).await?;
        let region: Option<String> = row.try_get("region")?;
        let aspect = super::display::values(app, first, platform_id, region.as_deref()).await?.into_iter().find(|v| v.field == "aspect");
        let Some(compare) = aspect.as_ref().and_then(|a| a.compare.clone()) else { continue };
        items.push(AspectItem {
            title_id: g.try_get("title_id")?,
            title_name: g.try_get("title_name")?,
            release_id: first,
            release_name: row.try_get("name")?,
            platform_id,
            platform: row.try_get("platform")?,
            compare,
            value: aspect.and_then(|a| a.value),
            apply_to: ids,
            untouched: g.try_get::<i64, _>("untouched")? != 0,
            has_screenshot: g.try_get::<i64, _>("has_screenshot")? != 0,
        });
    }
    Ok(ReviewPage { items, total: groups.len() as u64, offset })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_question_names_every_version() {
        let q = question("Director's Cut", "PlayStation 2", "Fuga Saga", &[("Fuga Saga".into(), "US".into())], "Fuga Saga - Director's Cut");
        assert!(q.contains("Japanese original \"Fuga Saga\"") && q.contains("\"Fuga Saga\" (US)") && q.contains("(Director's Cut)"), "{q}");
        assert_eq!(google_url("a b"), "https://www.google.com/search?q=a+b");
        // The question stays English; the language line goes under it.
        assert_eq!(ai::with_language(&q, Some("ja")), format!("{q}
Answer in Japanese."));
    }
}
