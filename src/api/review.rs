//! Edit › Merge review and Edit › Aspect review: decisions taken one at a time, like a
//! discovery queue. Each queue is narrowed by the Titles list's own filters (a set included),
//! so a month spent on one set can review just that set.
//!
//! Merge review walks `title_merge_suggestions` (`database::title_review` writes the regional
//! kinds), ordered by `merge_review_order`. A suggestion's choice is applied here: whole titles
//! merged, or releases moved into the titles the choice names. For an International or a
//! Director's Cut it can also ask the web what differs (`lookup`): Gemini with Google Search
//! when a key is set, and always a ready Google / AI Mode link. Nothing of that is stored.
//!
//! Aspect review walks the titles whose 4:3 / 8:7 (or square-pixel) question is still open:
//! no release of a title on that platform has the user's answer.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::common::models::Platform;
use crate::database::settings as db_settings;
use crate::database::title_merge;
use crate::database::title_review::{self, ReviewDetail, ReviewOption};

use super::catalog::{self, ListQuery};
use super::titles::{self, ReleasesMove, ReleasesSplit};
use super::{App, Error, Result};

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReviewParams {
    /// Merge review: one suggestion kind only (`international`, `igdb`, ...).
    pub kind: Option<String>,
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
    let mut sql = String::from("SELECT igdb_id, title_ids, kind FROM title_merge_suggestions WHERE status = 'pending'");
    if p.kind.is_some() {
        sql.push_str(" AND kind = ?");
    }
    sql.push_str(&format!(" ORDER BY {order}, updated_at DESC, igdb_id"));
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

#[derive(Debug, Clone, Deserialize)]
pub struct ApplyRequest {
    /// Which of the suggestion's options (0 for the IGDB-game kind's merge).
    #[serde(default)]
    pub option: usize,
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

#[derive(Debug, Clone, Default, Serialize)]
pub struct LookupSource {
    pub title: String,
    pub url: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LookupAnswer {
    pub question: String,
    /// The question as a Google search, and in Google's AI Mode.
    pub google_url: String,
    pub ai_mode_url: String,
    /// Gemini's answer, when a key is set.
    pub answer: Option<String>,
    pub sources: Vec<LookupSource>,
    pub model: Option<String>,
    /// Why there is no answer (no key, the request failed).
    pub error: Option<String>,
}

lazy_static::lazy_static! {
    /// Answers already given, for an hour: the queue goes back and forth over the same cards.
    static ref ANSWERS: Mutex<HashMap<String, (Instant, LookupAnswer)>> = Mutex::new(HashMap::new());
    /// The Gemini model picked from the key's own list (`pick_model`), once per process.
    static ref MODEL: Mutex<Option<String>> = Mutex::new(None);
}

const GEMINI: &str = "https://generativelanguage.googleapis.com/v1beta";

/// The question a variant suggestion asks: what differs between the Japanese original, the
/// foreign release and the variant, and which of the two the variant is closer to.
fn question(variant: &str, platform: &str, original: &str, foreign: &[(String, String)], variant_name: &str) -> String {
    let foreign_list = foreign.iter().map(|(n, r)| format!("\"{n}\" ({r})")).collect::<Vec<_>>().join(", ");
    format!(
        "On {platform}: what are the differences between the Japanese original \"{original}\", the foreign release {foreign_list}, and \"{variant_name}\" ({variant})? \
         Is \"{variant_name}\" closer in content to the Japanese original or to the foreign release? Answer briefly: the main differences, then which one it is closer to."
    )
}

fn search_urls(q: &str) -> (String, String) {
    let google = reqwest::Url::parse_with_params("https://www.google.com/search", &[("q", q)]).map(|u| u.to_string()).unwrap_or_default();
    let ai = reqwest::Url::parse_with_params("https://www.google.com/search", &[("q", q), ("udm", "50")]).map(|u| u.to_string()).unwrap_or_default();
    (google, ai)
}

async fn gemini_key(app: &App) -> Option<String> {
    let mut creds = crate::database::ApiCredentials::load(&app.paths.api_env()).ok()?;
    creds.decrypt(&app.vault);
    creds.get("gemini_api_key").map(str::to_string)
}

/// The newest stable Flash model the key can use for `generateContent` (the setting
/// `gemini_model` overrides). Google renames these often; asking the API beats a fixed id.
async fn pick_model(app: &App, key: &str) -> std::result::Result<String, String> {
    if let Some(m) = db_settings::get::<String>(&app.pool, db_settings::KEY_GEMINI_MODEL).await.ok().flatten().filter(|m| !m.trim().is_empty()) {
        return Ok(m.trim().trim_start_matches("models/").to_string());
    }
    if let Some(m) = MODEL.lock().ok().and_then(|m| m.clone()) {
        return Ok(m);
    }
    let list: serde_json::Value = app.http.get(format!("{GEMINI}/models?pageSize=1000")).header("x-goog-api-key", key).timeout(Duration::from_secs(20)).send().await.map_err(|e| e.to_string())?.error_for_status().map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;
    let version = |name: &str| -> Vec<u32> { name.split(|c: char| !c.is_ascii_digit()).filter_map(|d| d.parse().ok()).take(2).collect() };
    let best = list["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["supportedGenerationMethods"].as_array().is_some_and(|a| a.iter().any(|x| x == "generateContent")))
        .filter_map(|m| m["name"].as_str().map(|n| n.trim_start_matches("models/").to_string()))
        .filter(|n| n.starts_with("gemini-") && n.contains("flash") && !["lite", "image", "tts", "live", "audio", "exp", "thinking", "embedding"].iter().any(|w| n.contains(w)))
        .max_by_key(|n| (!n.contains("preview"), version(n), std::cmp::Reverse(n.len())))
        .ok_or_else(|| "this key lists no Gemini Flash model".to_string())?;
    if let Ok(mut m) = MODEL.lock() {
        *m = Some(best.clone());
    }
    Ok(best)
}

/// Asks Gemini, with Google Search to ground the answer.
async fn ask_gemini(app: &App, key: &str, q: &str) -> std::result::Result<(String, String, Vec<LookupSource>), String> {
    let model = pick_model(app, key).await?;
    let body = serde_json::json!({ "contents": [{ "parts": [{ "text": q }] }], "tools": [{ "google_search": {} }] });
    let resp = app.http.post(format!("{GEMINI}/models/{model}:generateContent")).header("x-goog-api-key", key).json(&body).timeout(Duration::from_secs(60)).send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(v["error"]["message"].as_str().map(str::to_string).unwrap_or_else(|| format!("Gemini answered {status}")));
    }
    let candidate = &v["candidates"][0];
    let text: String = candidate["content"]["parts"].as_array().into_iter().flatten().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("");
    let mut seen = HashSet::new();
    let sources = candidate["groundingMetadata"]["groundingChunks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| Some(LookupSource { title: c["web"]["title"].as_str().unwrap_or("").to_string(), url: c["web"]["uri"].as_str()?.to_string() }))
        .filter(|s| seen.insert(s.url.clone()))
        .collect();
    if text.trim().is_empty() {
        return Err("Gemini gave no answer".into());
    }
    Ok((model, text, sources))
}

/// What the web says about an International / Director's Cut suggestion. Always the question
/// and its Google links; Gemini's answer too when a key is set.
pub async fn lookup(app: &App, key: &str) -> Result<LookupAnswer> {
    if let Some((at, a)) = ANSWERS.lock().ok().and_then(|m| m.get(key).cloned()) {
        if at.elapsed() < Duration::from_secs(3600) {
            return Ok(a);
        }
    }
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
    let (google_url, ai_mode_url) = search_urls(&q);
    let mut answer = LookupAnswer { question: q.clone(), google_url, ai_mode_url, ..Default::default() };
    match gemini_key(app).await {
        None => answer.error = Some("no Gemini API key set (provider credentials): use Ask Google".into()),
        Some(k) => match ask_gemini(app, &k, &q).await {
            Ok((model, text, sources)) => {
                answer.model = Some(model);
                answer.answer = Some(text);
                answer.sources = sources;
            }
            Err(e) => answer.error = Some(e),
        },
    }
    if answer.answer.is_some() {
        if let Ok(mut m) = ANSWERS.lock() {
            m.insert(key.to_string(), (Instant::now(), answer.clone()));
        }
    }
    Ok(answer)
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
        let (google, ai) = search_urls("a b");
        assert_eq!(google, "https://www.google.com/search?q=a+b");
        assert!(ai.ends_with("&udm=50"));
    }
}
