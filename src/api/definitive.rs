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
    /// The base game has a version note; its rank.
    pub note_rank: Option<String>,
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

/// The Definitive list. Filters: `search` (either title's name), `platforms` (a version on
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
           AND (f.root_title_id IS NOT NULL OR (t.family_root_id IS NULL AND t.category IN ({MEMBER_CATEGORIES}) \
                AND NOT EXISTS (SELECT 1 FROM title_bundle_games bx WHERE bx.bundle_title_id = t.id) \
                AND EXISTS (SELECT 1 FROM title_releases trx JOIN `release` rx ON rx.id = trx.release_id AND rx.deleted = FALSE AND rx.material IS NULL WHERE trx.title_id = t.id AND {retail})))"
    );
    let mut binds: Vec<Bind> = Vec::new();
    if let Some(s) = q.search.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        body.push_str(" AND (t.name LIKE ? OR d.name LIKE ? OR EXISTS (SELECT 1 FROM title_locales tls WHERE tls.title_id IN (t.id, d.id) AND tls.name LIKE ?))");
        for _ in 0..3 {
            binds.push(Bind::Str(like_pattern(s)));
        }
    }
    let chosen: Vec<String> = catalog::chosen_platforms(q).into_iter().flatten().map(|p| p.to_string()).collect();
    if !chosen.is_empty() {
        body.push_str(&format!(
            " AND EXISTS (SELECT 1 FROM title m JOIN title_releases trp ON trp.title_id = m.id JOIN `release` rp ON rp.id = trp.release_id AND rp.deleted = FALSE AND rp.material IS NULL \
               WHERE m.deleted = FALSE AND COALESCE(m.family_root_id, m.id) = t.id AND rp.app_platform_id IN ({}))",
            chosen.join(",")
        ));
    }
    if let Some(min) = note_floor(q)? {
        body.push_str(&format!(" AND EXISTS (SELECT 1 FROM title_provider_links ln WHERE ln.title_id = t.id AND ln.provider = 'igdb' AND ln.deleted = FALSE AND ln.provider_id IN ({}))", noted_ids(min)));
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
    let notes_by_head = head_notes(pool, &pairs.iter().map(|(h, _, _)| *h).collect::<Vec<_>>()).await?;
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
            DefinitiveRow { id: head, local_games: local.get(&lead.title_id).copied().unwrap_or(0), lead, other, definitive_title_id: def, base_title_id: head, members, note_rank: notes_by_head.get(&head).map(|n| n.rank.clone()) }
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
    fn notes_parse_and_are_in_id_order() {
        let text = include_str!("../../app_root_ex/assets/definitive_notes.json");
        let raw: serde_json::Value = serde_json::from_str(text).expect("parses");
        let keys: Vec<u64> = raw["notes"].as_object().expect("notes is an object").keys().map(|k| k.parse().expect("keys are IGDB ids")).collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted, "notes are kept in IGDB id order");
        for (id, n) in notes() {
            assert!(NOTE_RANKS.contains(&n.rank.as_str()), "{id}: rank {} is not S, A or B", n.rank);
            assert!(n.note.contains_key("en") && n.recommend.contains_key("en"), "{id}: every note has English text");
        }
    }
}
