//! Releases combined into one by hand (`release_combined`, migration 0087).
//!
//! A DAT can list the discs of one game as entries of their own: a bundle's two discs, a
//! bonus disc named apart from the game. The import keeps "(Disc 1)" / "(Disc 2)" together
//! (`import::release_name_of`), but any other difference in the name makes two releases, and
//! a copy of the game's title took one of them -- one disc.
//!
//! Combining them keeps every release as it is (its row, its files, its local copies, its
//! name, so the next DAT import still finds it) and records which one leads. The lists show
//! the lead only, the release page lists what was combined into it, and a copy of the lead
//! takes every one of them ([`with_combined`]). Splitting one off is deleting its row.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sqlx::Row;

use super::{App, Error, Result};

/// The most releases one combine takes: the discs of one game, not a batch edit.
const MAX: usize = 50;

#[derive(Debug, Clone, Deserialize)]
pub struct CombineRequest {
    pub release_ids: Vec<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CombineResult {
    /// The release the others were combined into.
    pub release_id: u64,
    /// How many releases are combined into it now.
    pub combined: usize,
}

/// A release in a combined group, as the release page lists it.
#[derive(Debug, Clone, Serialize)]
pub struct CombinedRef {
    pub id: u64,
    pub name: String,
    pub disc_count: u16,
    /// Its copies on this machine.
    pub local_games: u64,
}

fn id_list(ids: &[u64]) -> String {
    ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",")
}

/// Combines releases into one. A release already in a group brings its whole group along,
/// so combining a third disc into a pair makes one group of three. The lead is a release
/// that already leads one of the groups, else the lowest id (as titles combine into the
/// lowest). Every release must be live and on one platform, and an unlicensed or pirate
/// release is never combined with a licensed one. The others are moved under the lead's
/// title, so the title lists one release.
pub async fn combine(app: &App, req: &CombineRequest) -> Result<CombineResult> {
    let pool = &app.pool;
    let mut ids = req.release_ids.clone();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() < 2 {
        return Err(Error::invalid("pick at least two releases to combine"));
    }
    if ids.len() > MAX {
        return Err(Error::invalid(format!("at most {MAX} releases at once")));
    }
    // The groups they are in already, whole.
    let list = id_list(&ids);
    let leads: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT DISTINCT lead_id FROM release_combined WHERE release_id IN ({list}) OR lead_id IN ({list})"))).fetch_all(pool).await?;
    let mut all: Vec<u64> = ids.clone();
    if !leads.is_empty() {
        let leads_list = id_list(&leads);
        let members: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT release_id FROM release_combined WHERE lead_id IN ({leads_list})"))).fetch_all(pool).await?;
        all.extend(leads.iter().copied());
        all.extend(members);
        all.sort_unstable();
        all.dedup();
    }
    if all.len() > MAX {
        return Err(Error::invalid(format!("at most {MAX} releases in one")));
    }
    let list = id_list(&all);
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT id, app_platform_id, (is_unlicensed OR is_pirate) AS unl FROM `release` WHERE id IN ({list}) AND deleted = FALSE"))).fetch_all(pool).await?;
    if rows.len() != all.len() {
        return Err(Error::not_found("some of those releases do not exist"));
    }
    let mut platforms = HashSet::new();
    let mut unlicensed = HashSet::new();
    for r in &rows {
        platforms.insert(r.try_get::<u16, _>("app_platform_id")?);
        unlicensed.insert(r.try_get::<i64, _>("unl")? != 0);
    }
    if platforms.len() > 1 {
        return Err(Error::invalid("only releases of one platform can be combined"));
    }
    if unlicensed.len() > 1 {
        return Err(Error::invalid("an unlicensed or pirate release is never combined with a licensed one"));
    }
    let lead = leads.iter().copied().min().unwrap_or(all[0]);
    let members: Vec<u64> = all.iter().copied().filter(|id| *id != lead).collect();

    // Under the lead's title, so the title lists the one release.
    let title: Option<u64> = sqlx::query_scalar("SELECT tr.title_id FROM title_releases tr JOIN title t ON t.id = tr.title_id AND t.deleted = FALSE WHERE tr.release_id = ? ORDER BY tr.title_id LIMIT 1").bind(lead).fetch_optional(pool).await?;
    if let Some(title_id) = title {
        let elsewhere: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT r.id FROM `release` r WHERE r.id IN ({}) AND NOT EXISTS (SELECT 1 FROM title_releases tr WHERE tr.release_id = r.id AND tr.title_id = ?)",
            id_list(&members)
        )))
        .bind(title_id)
        .fetch_all(pool)
        .await?;
        if !elsewhere.is_empty() {
            super::titles::move_releases(app, &super::titles::ReleasesMove { release_ids: elsewhere, title_id }).await?;
        }
    }

    let mut tx = pool.begin().await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM release_combined WHERE release_id IN ({list}) OR lead_id IN ({list})"))).execute(&mut *tx).await?;
    let values = members.iter().map(|m| format!("({m}, {lead})")).collect::<Vec<_>>().join(", ");
    sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO release_combined (release_id, lead_id) VALUES {values}"))).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(CombineResult { release_id: lead, combined: members.len() })
}

/// Takes a release out of its group: a combined one stands alone again, and a lead lets go
/// of every release combined into it. It stays under the title it is in. Returns how many
/// releases now stand alone that did not.
pub async fn split(app: &App, release_id: u64) -> Result<u64> {
    let pool = &app.pool;
    let gone = sqlx::query("DELETE FROM release_combined WHERE release_id = ? OR lead_id = ?").bind(release_id).bind(release_id).execute(pool).await?.rows_affected();
    if gone == 0 && sqlx::query_scalar::<_, u64>("SELECT id FROM `release` WHERE id = ?").bind(release_id).fetch_optional(pool).await?.is_none() {
        return Err(Error::not_found(format!("no release {release_id}")));
    }
    Ok(gone)
}

/// The release `release_id` is combined into, if it is.
pub async fn lead_of(pool: &sqlx::MySqlPool, release_id: u64) -> Result<Option<CombinedRef>> {
    let lead: Option<u64> = sqlx::query_scalar("SELECT lead_id FROM release_combined WHERE release_id = ?").bind(release_id).fetch_optional(pool).await?;
    Ok(match lead {
        Some(id) => refs(pool, &[id]).await?.into_iter().next(),
        None => None,
    })
}

/// The releases combined into `release_id`, by name.
pub async fn combined_into(pool: &sqlx::MySqlPool, release_id: u64) -> Result<Vec<CombinedRef>> {
    let ids: Vec<u64> = sqlx::query_scalar("SELECT release_id FROM release_combined WHERE lead_id = ?").bind(release_id).fetch_all(pool).await?;
    refs(pool, &ids).await
}

async fn refs(pool: &sqlx::MySqlPool, ids: &[u64]) -> Result<Vec<CombinedRef>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT r.id, r.name, r.disc_count, (SELECT COUNT(DISTINCT lg.id) FROM local_release_games lrg JOIN local_game lg ON lg.id = lrg.game_id AND lg.deleted = FALSE WHERE lrg.release_id = r.id) AS local_games \
         FROM `release` r WHERE r.id IN ({}) ORDER BY r.name, r.id",
        id_list(ids)
    )))
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|r| Ok(CombinedRef { id: r.try_get("id")?, name: r.try_get("name")?, disc_count: r.try_get("disc_count")?, local_games: r.try_get::<i64, _>("local_games")? as u64 }))
        .collect()
}

/// `ids` with every release combined with each one, in order: each named release, then the
/// rest of its group (the lead first, then the others by name) -- so a copy of a game takes
/// all of its discs, whichever of them was named.
pub async fn with_combined(pool: &sqlx::MySqlPool, ids: &[u64]) -> Result<Vec<u64>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut groups: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut lead_of: HashMap<u64, u64> = HashMap::new();
    for chunk in ids.chunks(1000) {
        let list = id_list(chunk);
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT c.release_id, c.lead_id FROM release_combined c JOIN `release` r ON r.id = c.release_id AND r.deleted = FALSE \
             WHERE c.lead_id IN (SELECT lead_id FROM release_combined WHERE release_id IN ({list}) OR lead_id IN ({list})) ORDER BY c.lead_id, r.name, r.id"
        )))
        .fetch_all(pool)
        .await?;
        for r in &rows {
            let (member, lead): (u64, u64) = (r.try_get("release_id")?, r.try_get("lead_id")?);
            lead_of.insert(member, lead);
            lead_of.insert(lead, lead);
            let group = groups.entry(lead).or_insert_with(|| vec![lead]);
            if !group.contains(&member) {
                group.push(member);
            }
        }
    }
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        match lead_of.get(id).and_then(|l| groups.get(l)) {
            Some(group) => {
                if seen.insert(*id) {
                    out.push(*id);
                }
                for g in group {
                    if seen.insert(*g) {
                        out.push(*g);
                    }
                }
            }
            None => {
                if seen.insert(*id) {
                    out.push(*id);
                }
            }
        }
    }
    Ok(out)
}

/// SQL true for a release combined into another, which the lists leave out (`alias`: the
/// release's alias).
pub fn is_combined_sql(alias: &str) -> String {
    format!("EXISTS (SELECT 1 FROM release_combined rcmb WHERE rcmb.release_id = {alias}.id)")
}
