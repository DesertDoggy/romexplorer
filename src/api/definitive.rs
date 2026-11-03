//! Collection > Definitive: one row per family (`database::title_family`), led by its
//! definitive version or, toggled, by its base game; a family's page with its versions in
//! order (dragged by the user); and the version notes for games whose latest version may have
//! less content than an earlier one.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use sqlx::Row;

use super::catalog::{self, apply, Bind, ListQuery, Page};
use super::{clamp_page, like_pattern, App, Error, Result};
use crate::database::title_family::{self as family, MEMBER_CATEGORIES};
use crate::database::jobs::Progress;
use romexplorer_types::jobs::AiBatchRateRequest;

/// One version note (`assets/definitive_notes.json`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VersionNote {
    /// S: the older version is strongly recommended; A: depends on preference; B: the latest
    /// lacks some content but is still the better version.
    pub rank: String,
    /// Which version to play, per UI language (`en` is the fallback).
    #[serde(default)]
    pub recommend: BTreeMap<String, String>,
    /// Why, per UI language.
    #[serde(default)]
    pub note: BTreeMap<String, String>,
    /// When the note was last edited (YYYY-MM-DD): a version of the family that came out
    /// later is flagged.
    #[serde(default)]
    pub date: String,
}

#[derive(Deserialize)]
struct NotesFile {
    notes: BTreeMap<String, VersionNote>,
}

pub const NOTE_RANKS: [&str; 3] = ["S", "A", "B"];

/// The notes, by the base game's IGDB id.
pub fn notes() -> &'static HashMap<u64, VersionNote> {
    static NOTES: std::sync::OnceLock<HashMap<u64, VersionNote>> = std::sync::OnceLock::new();
    NOTES.get_or_init(|| {
        let file: NotesFile = serde_json::from_str(include_str!("../../app_root_ex/assets/definitive_notes.json")).expect("definitive_notes.json parses");
        file.notes.into_iter().filter_map(|(k, v)| Some((k.parse().ok()?, v))).collect()
    })
}

/// One side of a Definitive row: the definitive version, or the base game.
#[derive(Debug, Clone, Serialize, Default)]
pub struct Side {
    pub title_id: u64,
    pub name: String,
    /// Its platforms by name.
    pub platforms: Option<String>,
    pub cover_media_id: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DefinitiveRow {
    /// The family's head (its page's id).
    pub id: u64,
    /// lead: the definitive version, or the base game with `lead=base`; other: the other
    /// one (the same title when the base game is the definitive version).
    pub lead: Side,
    pub other: Side,
    pub definitive_title_id: u64,
    pub base_title_id: u64,
    pub members: u32,
    /// Copies of the leading title held here.
    pub local_games: u64,
    /// The base game's rank: its version note's, or (with `rank_source`) an AI rating's.
    pub note_rank: Option<String>,
    /// Where `note_rank` came from: `curated`, `bundled`, `api:<provider>`, `browser:<site>`.
    pub note_source: Option<String>,
    /// When it was rated, and a version of the family that came out after that.
    pub rated_on: Option<String>,
    pub newer_version: Option<NewerVersion>,
}

fn lead_is_base(q: &ListQuery) -> Result<bool> {
    match q.lead.as_deref().filter(|l| !l.is_empty()).unwrap_or("definitive") {
        "definitive" => Ok(false),
        "base" => Ok(true),
        other => Err(Error::invalid(format!("lead must be definitive or base, not {other:?}"))),
    }
}

/// `noted`: the lowest rank shown (`S`: S only; `A`: S and A; `B`: any note); none: no filter.
fn note_floor(q: &ListQuery) -> Result<Option<usize>> {
    match q.noted.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        None => Ok(None),
        Some(n) => NOTE_RANKS.iter().position(|r| r.eq_ignore_ascii_case(n)).map(Some).ok_or_else(|| Error::invalid(format!("noted must be S, A or B, not {n:?}"))),
    }
}

/// The IGDB ids with a note of rank `NOTE_RANKS[floor]` or above, quoted for SQL (`'0'` when
/// there are none).
fn noted_ids(floor: usize) -> String {
    let mut ids: Vec<u64> = notes().iter().filter(|(_, n)| NOTE_RANKS.iter().position(|r| *r == n.rank).is_some_and(|i| i <= floor)).map(|(id, _)| *id).collect();
    ids.sort_unstable();
    if ids.is_empty() {
        return "'0'".to_string();
    }
    ids.iter().map(|i| format!("'{i}'")).collect::<Vec<_>>().join(",")
}

/// At most this many titles match a Definitive search (a one-letter search would otherwise
/// hand the list query every title there is).
const SEARCH_CAP: u32 = 20_000;

/// The Definitive list: family heads, and titles of a family of one, that have an IGDB id --
/// a title without one (a Wii U title still named by its hex title id, say) has no family to
/// show and is left to the Titles list, as in Crossplat. Filters: `search` (any version's
/// name, localized or alternative, like the Titles list), `platforms` (a version on
/// one of them), `noted` (the base game has a version note of that rank or above), `lead` (definitive | base).
/// Sort: name (the leading title's, default) | base (the base game's) | members.
pub async fn list(app: &App, q: &ListQuery) -> Result<Page<DefinitiveRow>> {
    let pool = &app.pool;
    let base_leads = lead_is_base(q)?;
    let unl = crate::database::release_buckets::unlicensed_joins_retail(pool).await?;
    let retail = format!("{} = 'retail'", crate::database::release_buckets::bucket_sql("rx", unl));
    let mut body = format!(
        "FROM title t LEFT JOIN title_families f ON f.root_title_id = t.id JOIN title d ON d.id = COALESCE(f.definitive_title_id, t.id) \
         WHERE t.deleted = FALSE AND (t.family_root_id IS NULL OR t.family_root_id = t.id) \
           AND EXISTS (SELECT 1 FROM title_provider_links li WHERE li.title_id = t.id AND li.provider = 'igdb' AND li.deleted = FALSE AND li.provider_id REGEXP '^[0-9]+$') \
           AND (f.root_title_id IS NOT NULL OR (t.family_root_id IS NULL AND t.category IN ({MEMBER_CATEGORIES}) \
                AND NOT EXISTS (SELECT 1 FROM title_bundle_games bx WHERE bx.bundle_title_id = t.id) \
                AND EXISTS (SELECT 1 FROM title_releases trx JOIN `release` rx ON rx.id = trx.release_id AND rx.deleted = FALSE AND rx.material IS NULL WHERE trx.title_id = t.id AND {retail})))"
    );
    let mut binds: Vec<Bind> = Vec::new();
    if let Some(s) = q.search.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        // Matched like the Titles list (its own name, localized and alternative names, the
        // name dictionary's other-language versions), over every version of a family -- the
        // row shows display names and finds its family by any member. Looked up once here and
        // handed to both queries as ids, rather than as LIKEs correlated into the count and the
        // page over every title.
        let (cond, pats) = crate::database::dictionary::search_condition(
            pool,
            s,
            &["m.name LIKE ?", "m.norm_name LIKE ?", "EXISTS (SELECT 1 FROM title_locales tls WHERE tls.title_id = m.id AND tls.name LIKE ?)", "EXISTS (SELECT 1 FROM title_alt_names tan WHERE tan.title_id = m.id AND tan.name LIKE ?)"],
            like_pattern(s),
        )
        .await?;
        let mut found = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT m.id, COALESCE(m.family_root_id, m.id) AS root FROM title m WHERE m.deleted = FALSE AND {cond} LIMIT {SEARCH_CAP}")));
        for p in &pats {
            found = found.bind(p);
        }
        let mut ids: Vec<u64> = Vec::new();
        for r in found.fetch_all(pool).await? {
            ids.push(r.try_get("id")?);
            ids.push(r.try_get("root")?);
        }
        ids.sort_unstable();
        ids.dedup();
        if ids.is_empty() {
            let (limit, offset) = clamp_page(q.limit, q.offset);
            return Ok(Page { items: Vec::new(), total: 0, limit, offset });
        }
        let list = ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        body.push_str(&format!(" AND (t.id IN ({list}) OR d.id IN ({list}))"));
    }
    let chosen: Vec<String> = catalog::chosen_platforms(q).into_iter().flatten().map(|p| p.to_string()).collect();
    if !chosen.is_empty() {
        body.push_str(&format!(
            " AND EXISTS (SELECT 1 FROM title m JOIN title_releases trp ON trp.title_id = m.id JOIN `release` rp ON rp.id = trp.release_id AND rp.deleted = FALSE AND rp.material IS NULL \
               WHERE m.deleted = FALSE AND COALESCE(m.family_root_id, m.id) = t.id AND rp.app_platform_id IN ({}))",
            chosen.join(",")
        ));
    }
    let mode = rank_source(q)?;
    let mut rank_by_game = ranks(app, mode).await?;
    if q.skip_newer {
        let dates: HashMap<u64, Option<String>> = rank_by_game.iter().map(|(i, r)| (*i, r.date.clone())).collect();
        let newer = newer_versions(app, &dates).await?;
        rank_by_game.retain(|i, _| !newer.contains_key(i));
    }
    if let Some(min) = note_floor(q)? {
        let ids = if mode == RankSource::Curated && !q.skip_newer { noted_ids(min) } else { ranked_ids(&rank_by_game, min) };
        body.push_str(&format!(" AND EXISTS (SELECT 1 FROM title_provider_links ln WHERE ln.title_id = t.id AND ln.provider = 'igdb' AND ln.deleted = FALSE AND ln.provider_id IN ({ids}))"));
    }
    let (display, display_binds) = catalog::title_display_filters(q, "d.id")?;
    body.push_str(&display);
    binds.extend(display_binds.into_iter().map(Bind::Str));
    let lead_name = if base_leads { "t.name" } else { "d.name" };
    let order = catalog::order_clause(q.sort.as_deref(), q.desc, &[("name", lead_name), ("base", "t.name"), ("definitive", "d.name"), ("members", "COALESCE(f.members, 1)")], lead_name)?;
    let (limit, offset) = clamp_page(q.limit, q.offset);
    let total: i64 = apply(sqlx::query(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) AS n {body}"))), &binds).fetch_one(pool).await?.try_get("n")?;
    let rows = apply(sqlx::query(sqlx::AssertSqlSafe(format!("SELECT t.id AS head, d.id AS def, CAST(COALESCE(f.members, 1) AS SIGNED) AS members {body} ORDER BY {order}, t.id LIMIT ? OFFSET ?"))), &binds)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;
    let pairs: Vec<(u64, u64, u32)> = rows.iter().map(|r| Ok((r.try_get("head")?, r.try_get("def")?, r.try_get::<i64, _>("members")?.max(0) as u32))).collect::<Result<_>>()?;
    let mut ids: Vec<u64> = pairs.iter().flat_map(|(h, d, _)| [*h, *d]).collect();
    ids.sort_unstable();
    ids.dedup();
    let sides = sides(app, &ids).await?;
    let ranks_by_head = head_ranks(pool, &pairs.iter().map(|(h, _, _)| *h).collect::<Vec<_>>(), &rank_by_game).await?;
    let newer_by_head: HashMap<u64, NewerVersion> = {
        let dates: HashMap<u64, Option<String>> = ranks_by_head.values().map(|(game, r)| (*game, r.date.clone())).collect();
        let newer = newer_versions(app, &dates).await?;
        ranks_by_head.iter().filter_map(|(head, (game, _))| newer.get(game).map(|n| (*head, n.clone()))).collect()
    };
    let local = local_games(pool, &ids).await?;
    let held = holds(pool, &pairs.iter().map(|(h, d, _)| (*h, *d)).collect::<Vec<_>>()).await?;
    let items = pairs
        .into_iter()
        .map(|(head, def, members)| {
            let mut d = sides.get(&def).cloned().unwrap_or_default();
            if let Some(game) = held.get(&(head, def)) {
                d.name = format!("{} ({game})", d.name);
            }
            let b = sides.get(&head).cloned().unwrap_or_default();
            let (lead, other) = if base_leads { (b, d) } else { (d, b) };
            DefinitiveRow { id: head, local_games: local.get(&lead.title_id).copied().unwrap_or(0), lead, other, definitive_title_id: def, base_title_id: head, members, note_rank: ranks_by_head.get(&head).map(|r| r.1.rank.clone()), note_source: ranks_by_head.get(&head).map(|r| r.1.source.clone()), rated_on: ranks_by_head.get(&head).and_then(|r| r.1.date.clone()), newer_version: newer_by_head.get(&head).cloned() }
        })
        .collect();
    Ok(Page { items, total: total.max(0) as u64, limit, offset })
}

/// Each title's display name, platforms and cover.
async fn sides(app: &App, ids: &[u64]) -> Result<HashMap<u64, Side>> {
    let mut out = HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let rank = catalog::ReleaseRank::load(app).await?;
    let sql = format!(
        "SELECT t.id, t.name, ({}) AS platforms, {} AS cover_media_id, {} AS top_release_id FROM title t WHERE t.id IN ({})",
        catalog::TITLE_PLATFORMS_SQL.replace("= ?", "= t.id"),
        catalog::title_cover_sql(&rank, None),
        catalog::top_release_sql(&rank, None),
        catalog::placeholders(ids.len())
    );
    let binds = catalog::title_cover_binds(app).await?;
    let mut query = apply(sqlx::query(sqlx::AssertSqlSafe(sql)), &binds);
    for id in ids {
        query = query.bind(*id);
    }
    let rows = query.fetch_all(&app.pool).await?;
    let mut pairs = Vec::new();
    for r in &rows {
        let id: u64 = r.try_get("id")?;
        pairs.push((id, r.try_get::<Option<u64>, _>("top_release_id")?));
        out.insert(id, Side { title_id: id, name: r.try_get("name")?, platforms: r.try_get("platforms")?, cover_media_id: r.try_get("cover_media_id")? });
    }
    for (id, name) in catalog::display_names(app, &pairs).await? {
        if let Some(s) = out.get_mut(&id) {
            s.name = name;
        }
    }
    Ok(out)
}

/// The copies held here of each title's releases.
async fn local_games(pool: &sqlx::MySqlPool, ids: &[u64]) -> Result<HashMap<u64, u64>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let sql = format!(
        "SELECT tr.title_id, COUNT(DISTINCT lg.id) AS n FROM title_releases tr JOIN local_release_games lrg ON lrg.release_id = tr.release_id JOIN local_game lg ON lg.id = lrg.game_id AND lg.deleted = FALSE WHERE tr.title_id IN ({}) GROUP BY tr.title_id",
        ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",")
    );
    Ok(sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(pool).await?.iter().filter_map(|r| Some((r.try_get("title_id").ok()?, r.try_get::<i64, _>("n").ok()?.max(0) as u64))).collect())
}

/// For a collection leading a family: the name of the family's game it holds.
async fn holds(pool: &sqlx::MySqlPool, pairs: &[(u64, u64)]) -> Result<HashMap<(u64, u64), String>> {
    let mut out = HashMap::new();
    for (head, def) in pairs.iter().filter(|(h, d)| h != d) {
        let name: Option<String> = sqlx::query_scalar("SELECT g.name FROM title_bundle_games bg JOIN title g ON g.id = bg.game_title_id WHERE bg.bundle_title_id = ? AND COALESCE(g.family_root_id, g.id) = ? ORDER BY g.id = ? DESC, g.id LIMIT 1")
            .bind(def)
            .bind(head)
            .bind(head)
            .fetch_optional(pool)
            .await?;
        if let Some(n) = name {
            out.insert((*head, *def), n);
        }
    }
    Ok(out)
}

/// The rank of each family head's IGDB game, from `ranks`: (game, rating).
async fn head_ranks(pool: &sqlx::MySqlPool, heads: &[u64], ranks: &HashMap<u64, Rated>) -> Result<HashMap<u64, (u64, Rated)>> {
    let mut out = HashMap::new();
    if heads.is_empty() || ranks.is_empty() {
        return Ok(out);
    }
    let sql = format!("SELECT title_id, provider_id FROM title_provider_links WHERE provider = 'igdb' AND deleted = FALSE AND title_id IN ({})", heads.iter().map(u64::to_string).collect::<Vec<_>>().join(","));
    for r in sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(pool).await? {
        let game: Option<u64> = r.try_get::<String, _>("provider_id")?.parse().ok();
        if let Some((g, found)) = game.and_then(|g| ranks.get(&g).map(|f| (g, f))) {
            out.insert(r.try_get("title_id")?, (g, found.clone()));
        }
    }
    Ok(out)
}

/// The version note of each family head's IGDB game.
async fn head_notes(pool: &sqlx::MySqlPool, heads: &[u64]) -> Result<HashMap<u64, VersionNote>> {
    let mut out = HashMap::new();
    if heads.is_empty() || notes().is_empty() {
        return Ok(out);
    }
    let sql = format!("SELECT title_id, provider_id FROM title_provider_links WHERE provider = 'igdb' AND deleted = FALSE AND title_id IN ({})", heads.iter().map(u64::to_string).collect::<Vec<_>>().join(","));
    for r in sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(pool).await? {
        let game: Option<u64> = r.try_get::<String, _>("provider_id")?.parse().ok();
        if let Some(n) = game.and_then(|g| notes().get(&g)) {
            out.insert(r.try_get("title_id")?, n.clone());
        }
    }
    Ok(out)
}

/// A version on a family's page.
#[derive(Debug, Clone, Serialize)]
pub struct FamilyMember {
    pub title_id: u64,
    pub name: String,
    pub category: String,
    pub platforms: Option<String>,
    pub year: Option<u16>,
    pub cover_media_id: Option<u64>,
    pub local_games: u64,
    /// No DAT lists it: a version IGDB knows of.
    pub provider_only: bool,
    /// The family's base game.
    pub is_base: bool,
    /// Placed by the user's drag.
    pub manual: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FamilyDetail {
    pub id: u64,
    pub base: Side,
    pub definitive_title_id: u64,
    /// In definitive order.
    pub members: Vec<FamilyMember>,
    pub manual_order: bool,
    pub note: Option<VersionNote>,
}

/// The family `title_id` is in.
pub async fn detail(app: &App, title_id: u64) -> Result<FamilyDetail> {
    let pool = &app.pool;
    let exists: Option<u64> = sqlx::query_scalar("SELECT id FROM title WHERE id = ? AND deleted = FALSE").bind(title_id).fetch_optional(pool).await?;
    if exists.is_none() {
        return Err(Error::not_found(format!("no title {title_id}")));
    }
    let head = family::head_of(pool, title_id).await?;
    let members = family::members_of(pool, &[head]).await?.remove(&head).unwrap_or_default();
    let priority = crate::database::settings::platform_priority(pool).await?;
    let ranked = family::rank(members, &priority, family::pc_max_age(pool).await?);
    let mut ids: Vec<u64> = ranked.iter().map(|m| m.title_id).collect();
    ids.push(head);
    ids.sort_unstable();
    ids.dedup();
    let sides = sides(app, &ids).await?;
    let local = local_games(pool, &ids).await?;
    let provider_only: std::collections::HashSet<u64> = if ids.is_empty() {
        Default::default()
    } else {
        sqlx::query_scalar::<_, u64>(sqlx::AssertSqlSafe(format!(
            "SELECT tr.title_id FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE WHERE tr.title_id IN ({}) GROUP BY tr.title_id HAVING SUM(NOT ({})) = 0",
            ids.iter().map(u64::to_string).collect::<Vec<_>>().join(","),
            family::provider_only_sql("r")
        )))
        .fetch_all(pool)
        .await?
        .into_iter()
        .collect()
    };
    let note = head_notes(pool, &[head]).await?.remove(&head);
    let out: Vec<FamilyMember> = ranked
        .iter()
        .map(|m| {
            let side = sides.get(&m.title_id).cloned().unwrap_or_default();
            FamilyMember {
                title_id: m.title_id,
                name: match &m.holds {
                    Some(game) => format!("{} ({game})", side.name),
                    None => side.name,
                },
                category: m.category.clone(),
                platforms: side.platforms,
                year: m.year,
                cover_media_id: side.cover_media_id,
                local_games: local.get(&m.title_id).copied().unwrap_or(0),
                provider_only: provider_only.contains(&m.title_id),
                is_base: m.title_id == head,
                manual: m.weight.is_some(),
            }
        })
        .collect();
    Ok(FamilyDetail {
        id: head,
        base: sides.get(&head).cloned().unwrap_or_default(),
        definitive_title_id: out.first().map(|m| m.title_id).unwrap_or(head),
        manual_order: out.iter().any(|m| m.manual),
        members: out,
        note,
    })
}

/// `POST /catalog/definitive/{id}/ask`: what the request may spend.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AskRequest {
    /// The UI language the answer should come back in.
    #[serde(default)]
    pub lang: Option<String>,
    /// Ask (the "Ask the AI" button); without it only a saved answer is shown.
    #[serde(default)]
    pub ask: bool,
    /// Ask again and replace the saved answer (not a pinned one).
    #[serde(default)]
    pub again: bool,
}

/// What the AI says about a family: which version to play, as a rank like the curated
/// notes' and the explanation under it.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AskAnswer {
    /// As sent: the question with its "Answer in <Language>." line.
    pub question: String,
    pub google_url: String,
    pub ask_links: Vec<super::ai::AskLink>,
    pub provider: String,
    pub lang: String,
    /// The answer's first line, `RANK: S|A|B|none`, read back: info only, never the
    /// curated rank (which the Definitive filter goes by).
    pub ai_rank: Option<String>,
    /// Where that answer came from: `api:<provider>` or `browser:<site>`.
    pub ai_source: Option<String>,
    /// The rank the family page shows: the curated note's or the AI's, whichever comes
    /// first in `ai_source_priority` (`curated` first unless moved).
    pub rank: Option<String>,
    /// `curated`, or the AI answer's source.
    pub rank_source: Option<String>,
    /// When the shown rank was rated, and a version of the family that came out after that.
    pub rated_on: Option<String>,
    pub newer_version: Option<NewerVersion>,
    #[serde(flatten)]
    pub result: super::ai::Lookup,
}

/// The `ai_answers` purpose of the Definitive family question.
pub const ASK_PURPOSE: &str = "definitive.compare";

/// The question about a family: its versions, one per line, and what to rank them by. The
/// rank definitions are the curated notes' own, so the AI's rank reads the same way.
fn compare_question(members: &[FamilyMember]) -> String {
    let mut q = String::from("These are the versions of one video game (name, platforms, year):\n");
    for m in members {
        let year = m.year.map(|y| y.to_string()).unwrap_or_else(|| "?".into());
        q.push_str(&format!("- {} ({}, {year})\n", m.name, m.platforms.as_deref().unwrap_or("?")));
    }
    q.push_str(
        "Which version has the most gameplay content, and which has the best features (graphics, sound, quality of life)? \
         Then rank whether an older version should be played instead of the newest one: \
         S = strongly recommend an older version; A = which one is better depends on preference; \
         B = the newest is better, but an older version has things the newest lacks; none = the newest is clearly best and nothing is lost. \
         Pixel art against 3D alone gives at most B, except that when the newer version is an early-3D remake (PS1 / PS2 era, low-poly or low-resolution 3D), \
         its graphics can look lower in quality today than refined 2D pixel art, and that can make it A. \
         Start the answer with exactly one line \"RANK: S\", \"RANK: A\", \"RANK: B\" or \"RANK: none\", then explain briefly, naming the versions.",
    );
    q
}

/// The rank on an answer's first line (`RANK: S`), and the answer without that line.
pub fn split_rank(answer: &str) -> (Option<String>, String) {
    let mut lines = answer.trim_start().splitn(2, '\n');
    let first = lines.next().unwrap_or("").trim().trim_matches('*').trim();
    let rank = first.strip_prefix("RANK:").or_else(|| first.strip_prefix("Rank:")).map(|r| r.trim().trim_matches('*').trim().to_string());
    match rank.filter(|r| ["S", "A", "B", "none"].iter().any(|k| k.eq_ignore_ascii_case(r))) {
        Some(r) => (Some(if r.eq_ignore_ascii_case("none") { "none".into() } else { r.to_ascii_uppercase() }), lines.next().unwrap_or("").trim().to_string()),
        None => (None, answer.to_string()),
    }
}

/// Which version of the family `title_id` is in to play, from the in-app AI: the saved
/// answer, else asked when `req.ask` / `req.again` says so. The subject is the base game's
/// IGDB id, so the answer survives a rebuilt catalogue.
pub async fn ask(app: &App, title_id: u64, req: &AskRequest) -> Result<AskAnswer> {
    let (fam, subject, q) = ask_subject(app, title_id).await?;
    let lang = super::ai::language_code(req.lang.as_deref()).unwrap_or("en");
    let asked = super::ai::with_language(&q, Some(lang));
    let spend = if req.again {
        super::ai::Spend::Again
    } else if req.ask {
        super::ai::Spend::IfMissing
    } else {
        super::ai::Spend::Never
    };
    let mut result = super::ai::lookup(app, ASK_PURPOSE, &subject, Some(lang), &q, spend).await?;
    // The bundled rating, where the source order puts it first (or nothing else is saved).
    if let Some(b) = subject.strip_prefix("igdb:").and_then(|i| i.parse::<u64>().ok()).and_then(|i| bundled().get(&i)) {
        if spend == super::ai::Spend::Never && super::ai::bundled_on(app).await {
            let priority = super::ai::source_priority(app).await?;
            let better = match &result.answer {
                None => true,
                Some(a) => super::ai::rank_of(&priority, "bundled") < super::ai::rank_of(&priority, &a.source_key()),
            };
            if better {
                result.answer = Some(bundled_answer(&subject, lang, &asked, b));
                result.other_language = None;
                result.cached = true;
            }
        }
    }
    let mut ai_rank = None;
    for a in [result.answer.as_mut(), result.other_language.as_mut()].into_iter().flatten() {
        let (rank, text) = split_rank(&a.answer);
        if ai_rank.is_none() {
            ai_rank = rank;
        }
        a.answer = text;
    }
    let ai_source = result.answer.as_ref().or(result.other_language.as_ref()).map(super::ai::Answer::source_key);
    // The shown rank: curated or AI, by the source order.
    let priority = super::ai::source_priority(app).await?;
    let curated = fam.note.as_ref().map(|n| n.rank.clone());
    let (rank, rank_source) = match (&curated, &ai_rank, &ai_source) {
        (Some(c), Some(a), Some(src)) => {
            if super::ai::rank_of(&priority, "curated") <= super::ai::rank_of(&priority, src) {
                (Some(c.clone()), Some("curated".to_string()))
            } else {
                (Some(a.clone()), Some(src.clone()))
            }
        }
        (Some(c), _, _) => (Some(c.clone()), Some("curated".to_string())),
        (None, Some(a), Some(src)) => (Some(a.clone()), Some(src.clone())),
        _ => (None, None),
    };
    let rated_on = match rank_source.as_deref() {
        Some("curated") => fam.note.as_ref().map(|n| n.date.chars().take(10).collect::<String>()).filter(|d| !d.is_empty()),
        Some(_) => result.answer.as_ref().or(result.other_language.as_ref()).and_then(|a| a.created_at.as_ref()).map(|d| d.chars().take(10).collect()),
        None => None,
    };
    let newer_version = match subject.strip_prefix("igdb:").and_then(|i| i.parse::<u64>().ok()) {
        Some(game) if rated_on.is_some() => newer_versions(app, &HashMap::from([(game, rated_on.clone())])).await?.remove(&game),
        _ => None,
    };
    let google_url = reqwest::Url::parse_with_params("https://www.google.com/search", &[("q", q.as_str())]).map(|u| u.to_string()).unwrap_or_default();
    Ok(AskAnswer { google_url, ask_links: super::ai::ask_links(&asked, &super::ai::ask_first(app).await?), question: asked, provider: super::ai::provider(app).await?, lang: lang.to_string(), ai_rank, ai_source, rank, rank_source, rated_on, newer_version, result })
}

/// The family `title_id` is in, its `ai_answers` subject (the base game's IGDB id, so the
/// answer survives a rebuilt catalogue) and the question asked about it.
async fn ask_subject(app: &App, title_id: u64) -> Result<(FamilyDetail, String, String)> {
    let fam = detail(app, title_id).await?;
    let igdb: Option<String> = sqlx::query_scalar("SELECT provider_id FROM title_provider_links WHERE title_id = ? AND provider = 'igdb' AND deleted = FALSE ORDER BY provider_id LIMIT 1").bind(fam.id).fetch_optional(&app.pool).await?;
    let subject = match igdb {
        Some(id) => format!("igdb:{id}"),
        None => format!("title:{}", fam.id),
    };
    let q = compare_question(&fam.members);
    Ok((fam, subject, q))
}

/// `POST /catalog/definitive/{id}/paste`: an answer copied from an Ask menu link.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PasteRequest {
    #[serde(default)]
    pub lang: Option<String>,
    /// The Ask menu link it came from: google / claude / chatgpt / perplexity.
    pub site: String,
    pub answer: String,
}

/// Saves a pasted answer for the family (`ai::save_pasted`) and returns the family's AI
/// panel as it now is.
pub async fn paste(app: &App, title_id: u64, req: &PasteRequest) -> Result<AskAnswer> {
    let (_, subject, q) = ask_subject(app, title_id).await?;
    super::ai::save_pasted(app, ASK_PURPOSE, &subject, req.lang.as_deref(), &q, &req.site, &req.answer).await?;
    ask(app, title_id, &AskRequest { lang: req.lang.clone(), ask: false, again: false }).await
}

// ---------------------------------------------------------------------------
// Batch rate with AI
// ---------------------------------------------------------------------------

/// What a batch AI rating would do, before it is started (the confirm dialog).
#[derive(Debug, Clone, Default, Serialize)]
pub struct BatchEstimate {
    /// Families the selection is in.
    pub families: u64,
    /// Skipped: only one version, nothing to compare.
    pub one_version: u64,
    /// With a saved answer in this language: skipped unless re-asked.
    pub cached: u64,
    /// Questions the job would send.
    pub to_ask: u64,
    pub provider: String,
    pub model: String,
    /// The chosen provider has an API key set.
    pub has_key: bool,
    /// Roughly, per question and in all, in US dollars; `None` when the model has no price.
    pub each_usd: Option<f64>,
    pub total_usd: Option<f64>,
}

/// The families a batch asks about, in title id order of their heads.
struct BatchPlan {
    families: u64,
    one_version: u64,
    cached: u64,
    ask: Vec<u64>,
}

async fn batch_plan(app: &App, req: &AiBatchRateRequest) -> Result<BatchPlan> {
    let pool = &app.pool;
    let mut titles = req.title_ids.clone();
    for chunk in req.release_ids.chunks(500) {
        let list = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        titles.extend(sqlx::query_scalar::<_, u64>(sqlx::AssertSqlSafe(format!("SELECT DISTINCT title_id FROM title_releases WHERE release_id IN ({list})"))).fetch_all(pool).await?);
    }
    if let Some(set) = req.set_id {
        titles.extend(sqlx::query_scalar::<_, u64>("SELECT DISTINCT tr.title_id FROM collection_set_releases csr JOIN title_releases tr ON tr.release_id = csr.release_id WHERE csr.set_id = ?").bind(set).fetch_all(pool).await?);
    }
    titles.sort_unstable();
    titles.dedup();
    let heads = family::heads_of(pool, &titles).await?;
    let members = family::members_of(pool, &heads).await?;
    let multi: Vec<u64> = heads.iter().copied().filter(|h| members.get(h).is_some_and(|m| m.len() >= 2)).collect();
    let subjects = batch_subjects(app, &multi).await?;
    let lang = super::ai::language_code(req.lang.as_deref()).unwrap_or("en");
    let keys: Vec<String> = subjects.values().cloned().collect();
    let saved = super::ai::saved_subjects(app, ASK_PURPOSE, lang, &keys).await?;
    let (cached, mut ask): (Vec<u64>, Vec<u64>) = multi.iter().copied().partition(|h| subjects.get(h).is_some_and(|s| saved.contains(s)));
    if req.reask {
        ask = multi.clone();
    }
    if let Some(max) = req.max {
        ask.truncate(max as usize);
    }
    Ok(BatchPlan { families: heads.len() as u64, one_version: (heads.len() - multi.len()) as u64, cached: cached.len() as u64, ask })
}

/// Each head's `ai_answers` subject, as [`ask_subject`] makes it.
async fn batch_subjects(app: &App, heads: &[u64]) -> Result<HashMap<u64, String>> {
    let mut out: HashMap<u64, String> = heads.iter().map(|h| (*h, format!("title:{h}"))).collect();
    for chunk in heads.chunks(500) {
        let list = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT title_id, MIN(provider_id) AS igdb FROM title_provider_links WHERE title_id IN ({list}) AND provider = 'igdb' AND deleted = FALSE GROUP BY title_id")))
            .fetch_all(&app.pool)
            .await?;
        for r in rows {
            out.insert(r.try_get("title_id")?, format!("igdb:{}", r.try_get::<String, _>("igdb")?));
        }
    }
    Ok(out)
}

/// `POST /catalog/definitive/batch-estimate`: what "Batch rate with AI" would ask and cost.
pub async fn batch_estimate(app: &App, req: &AiBatchRateRequest) -> Result<BatchEstimate> {
    let plan = batch_plan(app, req).await?;
    let provider = super::ai::provider(app).await?;
    let (model, each) = super::ai::estimate_each(app, ASK_PURPOSE).await?;
    let to_ask = plan.ask.len() as u64;
    Ok(BatchEstimate {
        families: plan.families,
        one_version: plan.one_version,
        cached: plan.cached,
        to_ask,
        has_key: super::ai::has_key(app, &provider).await,
        provider,
        model,
        each_usd: each,
        total_usd: each.map(|e| e * to_ask as f64),
    })
}

/// The job's request is checked before it is queued: something to ask about, and a key.
pub async fn batch_validate(app: &App, req: &AiBatchRateRequest) -> Result<()> {
    if req.title_ids.is_empty() && req.release_ids.is_empty() && req.set_id.is_none() {
        return Err(Error::invalid("nothing selected"));
    }
    if req.max == Some(0) {
        return Err(Error::invalid("max must be at least 1"));
    }
    let provider = super::ai::provider(app).await?;
    if !super::ai::has_key(app, &provider).await {
        return Err(Error::unavailable(format!("no {provider} API key set (Config › AI)")));
    }
    Ok(())
}

/// What a batch AI rating did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct BatchSummary {
    pub families: u64,
    pub one_version: u64,
    /// Skipped: an answer was already saved (or is pinned, when re-asking).
    pub cached: u64,
    pub asked: u64,
    pub failed: u64,
    /// By the AI's rank: S, A, B, none, and answers without a RANK line.
    pub ranks: BTreeMap<String, u64>,
    /// What the answers cost, as the providers reported or the price table says.
    pub cost_usd: f64,
    pub cancelled: bool,
    /// Why the run stopped early (the same error three times in a row).
    pub stopped: Option<String>,
    pub last_error: Option<String>,
}

pub fn batch_one_liner(s: &BatchSummary) -> String {
    let mut out = format!("{} asked, {} skipped (saved), {} one-version, {} failed, ~US${:.2}", s.asked, s.cached, s.one_version, s.failed, s.cost_usd);
    if s.cancelled {
        out.push_str(", cancelled");
    }
    if let Some(e) = &s.stopped {
        out.push_str(&format!(", stopped: {e}"));
    }
    out
}

/// Waits `secs`, or less when the job is cancelled meanwhile. `true` if it was.
async fn wait_or_cancel(progress: &Progress, secs: u64) -> bool {
    for _ in 0..secs {
        if progress.cancelled().await {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    progress.cancelled().await
}

/// The `ai_batch_rate` job: the family question for each family of the selection, one at a
/// time. A rate-limit refusal waits and asks again (30 s, doubling, four times); any other
/// failure counts and moves on, and three in a row stop the run, since the next would fail
/// too (no credit, a revoked key).
pub async fn batch_rate(app: &App, req: &AiBatchRateRequest, progress: &Progress) -> Result<BatchSummary> {
    /// Between two questions: a provider's per-minute limit is rarely below 30.
    const PACE_SECS: u64 = 2;
    let plan = batch_plan(app, req).await?;
    let total = plan.ask.len() as u64;
    let mut s = BatchSummary { families: plan.families, one_version: plan.one_version, cached: if req.reask { 0 } else { plan.cached }, ..Default::default() };
    let mut in_a_row = 0u32;
    for (i, head) in plan.ask.iter().enumerate() {
        if progress.cancelled().await {
            s.cancelled = true;
            break;
        }
        progress.report(i as u64, Some(total.max(1)), &format!("{i} of {total} asked")).await;
        let mut wait = 30;
        let outcome = loop {
            let asked = ask(app, *head, &AskRequest { lang: req.lang.clone(), ask: !req.reask, again: req.reask }).await;
            match asked {
                Ok(a) => match a.result.error.clone() {
                    Some(e) if req.reask && a.result.answer.as_ref().is_some_and(|x| x.pinned) => break Ok((a, Some(e))),
                    Some(e) if super::ai::is_rate_limited(&e) && wait <= 240 => {
                        log::info!("ai_batch_rate: rate limited, waiting {wait}s: {e}");
                        progress.report(i as u64, Some(total.max(1)), &format!("{i} of {total} asked; rate limited, waiting {wait}s")).await;
                        if wait_or_cancel(progress, wait).await {
                            s.cancelled = true;
                            break Err(String::new());
                        }
                        wait *= 2;
                    }
                    Some(e) => break Err(e),
                    None => break Ok((a, None)),
                },
                Err(e) => break Err(e.message),
            }
        };
        if s.cancelled {
            break;
        }
        match outcome {
            // Re-asking a pinned answer: kept, as the family page keeps it.
            Ok((_, Some(_))) => s.cached += 1,
            Ok((a, None)) if a.result.cached => s.cached += 1,
            Ok((a, None)) => {
                in_a_row = 0;
                s.asked += 1;
                *s.ranks.entry(a.ai_rank.clone().unwrap_or_else(|| "unranked".into())).or_default() += 1;
                s.cost_usd += a.result.answer.as_ref().and_then(|x| x.cost_usd).unwrap_or(0.0);
            }
            Err(e) => {
                log::warn!("ai_batch_rate: family {head}: {e}");
                s.failed += 1;
                in_a_row += 1;
                s.last_error = Some(e.clone());
                if in_a_row >= 3 {
                    s.stopped = Some(e);
                    break;
                }
            }
        }
        if i + 1 < plan.ask.len() && wait_or_cancel(progress, PACE_SECS).await {
            s.cancelled = true;
            break;
        }
    }
    progress.finish(s.asked + s.failed, Some(total.max(1)), &batch_one_liner(&s), &s).await;
    Ok(s)
}

// ---------------------------------------------------------------------------
// AI ratings: bundled with the app, and the rank each family gets
// ---------------------------------------------------------------------------

/// An AI rating that ships with the app (`assets/definitive_ai.json`), by the base game's IGDB
/// id: the family question's answer, rank and text per language.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BundledRating {
    /// S, A, B or none.
    pub rank: String,
    /// The answer without its RANK line, per UI language (`en` is the fallback).
    #[serde(default)]
    pub text: BTreeMap<String, String>,
    #[serde(default)]
    pub model: String,
    /// When it was asked (YYYY-MM-DD).
    #[serde(default)]
    pub date: String,
}

#[derive(Deserialize, Serialize, Default)]
struct BundledFile {
    #[serde(default, rename = "_", skip_serializing_if = "String::is_empty")]
    about: String,
    #[serde(default)]
    ratings: BTreeMap<String, BundledRating>,
}

const BUNDLED_ABOUT: &str = "AI ratings for Collection > Definitive that ship with the app (source \"bundled AI\"), by the base game's IGDB id: the family question's rank (S, A, B, none), its text per UI language without the RANK line, the model and the date asked. Information only; the curated notes come first. No game names: the id says which game.";

/// The bundled AI ratings, by IGDB id.
pub fn bundled() -> &'static HashMap<u64, BundledRating> {
    static RATINGS: std::sync::OnceLock<HashMap<u64, BundledRating>> = std::sync::OnceLock::new();
    RATINGS.get_or_init(|| {
        let file: BundledFile = serde_json::from_str(include_str!("../../app_root_ex/assets/definitive_ai.json")).expect("definitive_ai.json parses");
        file.ratings.into_iter().filter_map(|(k, v)| Some((k.parse().ok()?, v))).collect()
    })
}

/// A bundled rating as a saved answer, in `lang` (else English, else any).
fn bundled_answer(subject: &str, lang: &str, question: &str, r: &BundledRating) -> super::ai::Answer {
    let (text_lang, text) = r.text.get_key_value(lang).or_else(|| r.text.get_key_value("en")).or_else(|| r.text.iter().next()).map(|(l, t)| (l.clone(), t.clone())).unwrap_or_else(|| (lang.to_string(), String::new()));
    super::ai::Answer {
        id: 0,
        purpose: ASK_PURPOSE.to_string(),
        subject_key: subject.to_string(),
        lang: text_lang,
        provider: String::new(),
        source: "bundled".into(),
        pasted_at: None,
        model: r.model.clone(),
        prompt: question.to_string(),
        answer: format!("RANK: {}\n{text}", r.rank),
        sources: Vec::new(),
        tokens_in: 0,
        tokens_out: 0,
        cost_usd: None,
        created_at: (!r.date.is_empty()).then(|| r.date.clone()),
        pinned: false,
    }
}

/// `rank_source`: which ranks the Definitive filter and a set's filter go by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RankSource {
    /// The curated notes only (the default).
    Curated,
    /// And the bundled AI ratings.
    Bundled,
    /// And the user's own saved answers (API and pasted).
    All,
}

pub fn rank_source(q: &ListQuery) -> Result<RankSource> {
    match q.rank_source.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("curated") {
        "curated" => Ok(RankSource::Curated),
        "bundled" => Ok(RankSource::Bundled),
        "all" => Ok(RankSource::All),
        other => Err(Error::invalid(format!("rank_source must be curated, bundled or all, not {other:?}"))),
    }
}

/// The rank each IGDB game has, and where it came from: the curated note first, then the
/// sources `mode` allows in the user's source order (`ai_source_priority`), newest first
/// among equals. `none` ranks are kept (a source can say so).
pub async fn ranks(app: &App, mode: RankSource) -> Result<HashMap<u64, Rated>> {
    let day = |d: &str| (!d.is_empty()).then(|| d.chars().take(10).collect::<String>());
    let mut out: HashMap<u64, Rated> = notes().iter().map(|(id, n)| (*id, Rated { rank: n.rank.clone(), source: "curated".to_string(), date: day(&n.date) })).collect();
    if mode == RankSource::Curated {
        return Ok(out);
    }
    let priority = super::ai::source_priority(app).await?;
    let mut best: HashMap<u64, (usize, Rated)> = HashMap::new();
    let mut offer = |id: u64, rated: Rated| {
        let at = super::ai::rank_of(&priority, &rated.source);
        match best.get(&id) {
            Some((b, _)) if *b <= at => {}
            _ => {
                best.insert(id, (at, rated));
            }
        }
    };
    if mode == RankSource::All {
        let rows = sqlx::query("SELECT subject_key, CAST(source AS CHAR) AS source, provider, answer, DATE_FORMAT(created_at, '%Y-%m-%d') AS day FROM ai_answers WHERE purpose = ? AND subject_key LIKE 'igdb:%' ORDER BY created_at DESC, id DESC")
            .bind(ASK_PURPOSE)
            .fetch_all(&app.pool)
            .await?;
        for r in rows {
            let subject: String = r.try_get("subject_key")?;
            let Some(id) = subject.strip_prefix("igdb:").and_then(|i| i.parse::<u64>().ok()) else { continue };
            let answer: String = r.try_get("answer")?;
            if let (Some(rank), _) = split_rank(&answer) {
                let source = format!("{}:{}", r.try_get::<String, _>("source")?, r.try_get::<String, _>("provider")?);
                offer(id, Rated { rank, source, date: r.try_get("day")? });
            }
        }
    }
    if super::ai::bundled_on(app).await {
        for (id, b) in bundled() {
            offer(*id, Rated { rank: b.rank.clone(), source: "bundled".to_string(), date: day(&b.date) });
        }
    }
    for (id, (_, rated)) in best {
        out.entry(id).or_insert(rated);
    }
    Ok(out)
}

/// A family's rank and where it came from.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Rated {
    pub rank: String,
    /// `curated`, `bundled`, `api:<provider>`, `browser:<site>`.
    pub source: String,
    /// When it was rated (YYYY-MM-DD), if known.
    pub date: Option<String>,
}

/// A version of the family that came out after its rating.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct NewerVersion {
    pub name: String,
    pub date: String,
}

/// For each IGDB game rated on a known day, the newest version of its family that IGDB says
/// came out after that day (`igdb_games.first_release`, any platform).
pub async fn newer_versions(app: &App, rated: &HashMap<u64, Option<String>>) -> Result<HashMap<u64, NewerVersion>> {
    let ids: Vec<u64> = rated.iter().filter(|(_, d)| d.is_some()).map(|(i, _)| *i).collect();
    let mut out: HashMap<u64, NewerVersion> = HashMap::new();
    for chunk in ids.chunks(500) {
        let list = chunk.iter().map(|i| format!("'{i}'")).collect::<Vec<_>>().join(",");
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT DISTINCT l.provider_id AS game, g.title AS name, DATE_FORMAT(g.first_release, '%Y-%m-%d') AS day FROM title_provider_links l \
             JOIN title t ON t.id = l.title_id AND t.deleted = FALSE \
             JOIN title m ON COALESCE(m.family_root_id, m.id) = COALESCE(t.family_root_id, t.id) AND m.deleted = FALSE \
             JOIN title_provider_links lm ON lm.title_id = m.id AND lm.provider = 'igdb' AND lm.deleted = FALSE \
             JOIN igdb_games g ON g.provider_id = CAST(lm.provider_id AS UNSIGNED) AND g.first_release IS NOT NULL \
             WHERE l.provider = 'igdb' AND l.deleted = FALSE AND l.provider_id IN ({list})"
        )))
        .fetch_all(&app.pool)
        .await?;
        for r in rows {
            let game: Option<u64> = r.try_get::<String, _>("game")?.parse().ok();
            let (Some(game), Some(day)) = (game, r.try_get::<Option<String>, _>("day")?) else { continue };
            let Some(Some(rated_on)) = rated.get(&game) else { continue };
            if day.as_str() > rated_on.as_str() && out.get(&game).is_none_or(|n| day > n.date) {
                out.insert(game, NewerVersion { name: r.try_get::<Option<String>, _>("name")?.unwrap_or_default(), date: day });
            }
        }
    }
    Ok(out)
}

/// The IGDB ids ranked `NOTE_RANKS[floor]` or above by `ranks`, quoted for SQL (`'0'` when none).
pub fn ranked_ids(ranks: &HashMap<u64, Rated>, floor: usize) -> String {
    let mut ids: Vec<u64> = ranks.iter().filter(|(_, r)| NOTE_RANKS.iter().position(|x| *x == r.rank).is_some_and(|i| i <= floor)).map(|(id, _)| *id).collect();
    ids.sort_unstable();
    if ids.is_empty() {
        return "'0'".to_string();
    }
    ids.iter().map(|i| format!("'{i}'")).collect::<Vec<_>>().join(",")
}

/// `noted` as a floor (public for a set's release list).
pub fn rank_floor(q: &ListQuery) -> Result<Option<usize>> {
    note_floor(q)
}

/// `GET /ai/bundled-export`: the user's saved API answers to the family question, in the
/// bundled file's shape -- the newest per language, for games whose family has two versions
/// or more. What a maintainer puts in `assets/definitive_ai.json`.
pub async fn bundled_export(app: &App) -> Result<serde_json::Value> {
    let rows = sqlx::query(
        "SELECT a.subject_key, a.lang, a.model, a.answer, DATE_FORMAT(a.created_at, '%Y-%m-%d') AS day FROM ai_answers a \
         WHERE a.purpose = ? AND a.source = 'api' AND a.subject_key LIKE 'igdb:%' ORDER BY a.created_at DESC, a.id DESC",
    )
    .bind(ASK_PURPOSE)
    .fetch_all(&app.pool)
    .await?;
    // Games whose family has two versions or more.
    let multi: std::collections::HashSet<String> = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT l.provider_id FROM title_provider_links l JOIN title t ON t.id = l.title_id AND t.deleted = FALSE \
         JOIN title_families f ON f.root_title_id = COALESCE(t.family_root_id, t.id) WHERE l.provider = 'igdb' AND l.deleted = FALSE AND f.members >= 2",
    )
    .fetch_all(&app.pool)
    .await?
    .into_iter()
    .collect();
    let mut ratings: BTreeMap<u64, BundledRating> = BTreeMap::new();
    for r in rows {
        let subject: String = r.try_get("subject_key")?;
        let Some(id) = subject.strip_prefix("igdb:").filter(|i| multi.contains(*i)).and_then(|i| i.parse::<u64>().ok()) else { continue };
        let lang: String = r.try_get("lang")?;
        let (rank, text) = split_rank(&r.try_get::<String, _>("answer")?);
        let Some(rank) = rank else { continue };
        let e = ratings.entry(id).or_insert_with(|| BundledRating { rank: rank.clone(), text: BTreeMap::new(), model: String::new(), date: String::new() });
        if e.model.is_empty() {
            e.model = r.try_get("model")?;
            e.date = r.try_get::<Option<String>, _>("day")?.unwrap_or_default();
        }
        // English decides the rank when there is one; the newest of each language its text.
        if lang == "en" && !e.text.contains_key("en") {
            e.rank = rank;
        }
        e.text.entry(lang).or_insert(text);
    }
    let file = BundledFile { about: BUNDLED_ABOUT.to_string(), ratings: ratings.into_iter().map(|(k, v)| (k.to_string(), v)).collect() };
    serde_json::to_value(file).map_err(|e| Error::internal(e.to_string()))
}

#[derive(Debug, Clone, Deserialize)]
pub struct MoveRequest {
    pub title_id: u64,
    /// The version it goes after; none: to the top.
    pub after_title_id: Option<u64>,
}

pub async fn move_member(app: &App, head: u64, req: &MoveRequest) -> Result<FamilyDetail> {
    let head = family::head_of(&app.pool, head).await?;
    family::move_member(&app.pool, head, req.title_id, req.after_title_id).await.map_err(|e| Error::invalid(e.to_string()))?;
    detail(app, head).await
}

pub async fn reset_order(app: &App, head: u64) -> Result<FamilyDetail> {
    let head = family::head_of(&app.pool, head).await?;
    family::reset_order(&app.pool, head).await?;
    detail(app, head).await
}

#[derive(Debug, Clone, Deserialize)]
pub struct BasegameRequest {
    /// One of `title_family::CATEGORIES`. `base_game`: no base game.
    pub category: String,
    /// The base game (one), or for a collection (`bundle`) the games it holds.
    #[serde(default)]
    pub base_title_ids: Vec<u64>,
}

/// The user sets a title's base game and what it is of it.
pub async fn set_basegame(app: &App, title_id: u64, req: &BasegameRequest) -> Result<()> {
    let ids: Vec<u64> = if req.category == "base_game" { Vec::new() } else { req.base_title_ids.clone() };
    if req.category != "base_game" && req.category != "bundle" && ids.is_empty() {
        return Err(Error::invalid("pick the base game"));
    }
    for id in ids.iter().chain(std::iter::once(&title_id)) {
        let found: Option<u64> = sqlx::query_scalar("SELECT id FROM title WHERE id = ? AND deleted = FALSE").bind(id).fetch_optional(&app.pool).await?;
        if found.is_none() {
            return Err(Error::not_found(format!("no title {id}")));
        }
    }
    family::set_basegame(&app.pool, title_id, &req.category, &ids).await.map_err(|e| Error::invalid(e.to_string()))
}

/// Back to IGDB's base game and category.
pub async fn reset_basegame(app: &App, title_id: u64) -> Result<()> {
    Ok(family::reset_basegame(&app.pool, title_id).await?)
}

/// After a setting the ranking depends on changed: every family again, in the background.
pub fn refresh_all_later(app: &App) {
    let pool = app.pool.clone();
    tokio::spawn(async move {
        if let Err(e) = family::refresh(&pool, None).await {
            log::warn!("definitive versions: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_ratings_parse_and_rank_like_notes() {
        for (id, r) in bundled() {
            assert!(["S", "A", "B", "none"].contains(&r.rank.as_str()), "{id}: rank {}", r.rank);
            assert!(r.text.contains_key("en"), "{id}: every rating has English text");
        }
    }

    #[test]
    fn the_ai_rank_is_read_from_the_first_line() {
        assert_eq!(split_rank("RANK: S\nPlay the older one."), (Some("S".into()), "Play the older one.".into()));
        assert_eq!(split_rank("**RANK: none**\n\nThe newest is best."), (Some("none".into()), "The newest is best.".into()));
        assert_eq!(split_rank("Rank: b\nx"), (Some("B".into()), "x".into()));
        assert_eq!(split_rank("No rank here.\nMore."), (None, "No rank here.\nMore.".into()));
        let members = vec![FamilyMember { title_id: 1, name: "Hoge Quest".into(), category: "base_game".into(), platforms: Some("NES".into()), year: Some(1987), cover_media_id: None, local_games: 0, provider_only: false, is_base: true, manual: false }];
        let q = compare_question(&members);
        assert!(q.contains("- Hoge Quest (NES, 1987)") && q.contains("RANK: none") && q.contains("S = strongly recommend"), "{q}");
    }

    #[test]
    fn notes_parse_and_are_in_id_order() {
        let text = include_str!("../../app_root_ex/assets/definitive_notes.json");
        let _: serde_json::Value = serde_json::from_str(text).expect("parses");
        // The keys as the file has them: a parsed object sorts its keys as strings.
        let keys: Vec<u64> = text.lines().filter_map(|l| l.trim().strip_prefix('"')?.split_once("\": {").and_then(|(k, _)| k.parse().ok())).collect();
        assert!(!keys.is_empty() || notes().is_empty(), "the keys were found");
        assert_eq!(keys.len(), notes().len(), "every note key is an IGDB id");
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted, "notes are kept in IGDB id order");
        for (id, n) in notes() {
            assert!(NOTE_RANKS.contains(&n.rank.as_str()), "{id}: rank {} is not S, A or B", n.rank);
            assert!(n.note.contains_key("en") && n.recommend.contains_key("en"), "{id}: every note has English text");
        }
    }
}
