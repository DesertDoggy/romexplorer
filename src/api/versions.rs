//! A title's versions (`database::versions`): what it is a minor version of, what it hides,
//! its version set and which version of it is shown, and the user's corrections. The
//! "Versions" card of a title page.

use serde::{Deserialize, Serialize};
use sqlx::Row;

use super::{App, Error, Result};
use crate::database::versions;

/// Another title, as the card names it.
#[derive(Debug, Clone, Serialize)]
pub struct Other {
    pub title_id: u64,
    pub name: String,
    pub platforms: Option<String>,
    /// `igdb` (an IGDB edition), `name` (the names say so), `data` (the built-in list) or
    /// `user`.
    pub source: String,
}

/// One version of a set.
#[derive(Debug, Clone, Serialize)]
pub struct SetVersion {
    pub key: String,
    pub titles: Vec<Other>,
    pub owned: bool,
    pub shown: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetInfo {
    pub key: String,
    pub versions: Vec<SetVersion>,
    /// The title the user picked to show, if any.
    pub pinned: Option<u64>,
}

/// A correction of the user's.
#[derive(Debug, Clone, Serialize)]
pub struct Relation {
    pub id: u64,
    pub kind: String,
    pub title_id: u64,
    pub other: Option<Other>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VersionInfo {
    /// Titles this one is a minor version of.
    pub minor_of: Vec<Other>,
    /// "Hide minor versions" leaves it out (every platform and region of it covered).
    pub hidden: bool,
    /// Titles that are minor versions of this one.
    pub fuller_than: Vec<Other>,
    pub set: Option<SetInfo>,
    /// The user's corrections that name this title.
    pub relations: Vec<Relation>,
    /// Its releases a fuller release of it covers (hidden by "hide minor versions").
    pub minor_releases: Vec<u64>,
}

async fn others(app: &App, ids: &[(u64, &str)]) -> Result<Vec<Other>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let list = ids.iter().map(|(i, _)| i.to_string()).collect::<Vec<_>>().join(",");
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT t.id, t.name, (SELECT GROUP_CONCAT(DISTINCT p.name ORDER BY p.name SEPARATOR ', ') FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE \
           JOIN platforms p ON p.id = r.app_platform_id WHERE tr.title_id = t.id) AS platforms FROM title t WHERE t.id IN ({list})"
    )))
    .fetch_all(&app.pool)
    .await?;
    let mut out = Vec::new();
    for (id, source) in ids {
        if let Some(r) = rows.iter().find(|r| r.try_get::<u64, _>("id").ok() == Some(*id)) {
            out.push(Other { title_id: *id, name: r.try_get("name")?, platforms: r.try_get("platforms")?, source: source.to_string() });
        }
    }
    Ok(out)
}

/// `GET /catalog/titles/{id}/versions`.
pub async fn info(app: &App, title_id: u64) -> Result<VersionInfo> {
    let v = versions::view(&app.pool).await?;
    let minor_of: Vec<(u64, &str)> = v.pairs.iter().filter(|(m, _, _)| *m == title_id).map(|(_, j, s)| (*j, *s)).collect();
    let fuller_than: Vec<(u64, &str)> = v.pairs.iter().filter(|(_, j, _)| *j == title_id).map(|(m, _, s)| (*m, *s)).collect();
    let set = match v.group_of.get(&title_id).and_then(|k| v.groups.get(k)) {
        Some(g) => {
            let mut out = Vec::new();
            for (key, ts) in &g.variants {
                let pairs: Vec<(u64, &str)> = ts.iter().map(|t| (*t, "data")).collect();
                out.push(SetVersion { key: key.clone(), titles: others(app, &pairs).await?, owned: g.owned.contains(key), shown: *key == g.shown });
            }
            Some(SetInfo { key: g.key.clone(), versions: out, pinned: g.pinned })
        }
        None => None,
    };
    let mut relations = Vec::new();
    for r in sqlx::query("SELECT id, title_id, other_title_id, CAST(kind AS CHAR) AS kind FROM title_version_relations WHERE title_id = ? OR other_title_id = ? ORDER BY id").bind(title_id).bind(title_id).fetch_all(&app.pool).await? {
        let (a, b): (u64, Option<u64>) = (r.try_get("title_id")?, r.try_get("other_title_id")?);
        let other_id = if a == title_id { b } else { Some(a) };
        let other = match other_id {
            Some(o) => others(app, &[(o, "user")]).await?.into_iter().next(),
            None => None,
        };
        relations.push(Relation { id: r.try_get("id")?, kind: r.try_get("kind")?, title_id: a, other });
    }
    let own: Vec<u64> = sqlx::query_scalar("SELECT release_id FROM title_releases WHERE title_id = ?").bind(title_id).fetch_all(&app.pool).await?;
    let minor_releases = own.into_iter().filter(|r| v.minor_releases.contains(r)).collect();
    Ok(VersionInfo { minor_of: others(app, &minor_of).await?, hidden: v.minor_titles.contains(&title_id), fuller_than: others(app, &fuller_than).await?, set, relations, minor_releases })
}

/// `POST /catalog/titles/{id}/versions`: a correction. `minor`: this title is a minor
/// version of `other_title_id`; `not_minor`: the two are not minor / fuller versions of each
/// other; `group`: the two are equal versions of one game; `ungroup`: this title is in no set.
#[derive(Debug, Clone, Deserialize)]
pub struct RelationRequest {
    pub kind: String,
    #[serde(default)]
    pub other_title_id: Option<u64>,
}

pub async fn add_relation(app: &App, title_id: u64, req: &RelationRequest) -> Result<VersionInfo> {
    if !["minor", "not_minor", "group", "ungroup"].contains(&req.kind.as_str()) {
        return Err(Error::invalid("kind must be minor, not_minor, group or ungroup"));
    }
    let other = if req.kind == "ungroup" { None } else { Some(req.other_title_id.ok_or_else(|| Error::invalid("other_title_id is required"))?) };
    if other == Some(title_id) {
        return Err(Error::invalid("pick another title"));
    }
    for id in std::iter::once(title_id).chain(other) {
        let live: Option<u64> = sqlx::query_scalar("SELECT id FROM title WHERE id = ? AND deleted = FALSE").bind(id).fetch_optional(&app.pool).await?;
        if live.is_none() {
            return Err(Error::not_found(format!("no title {id}")));
        }
    }
    // A pair holds one statement about minor / fuller: the new one replaces the old.
    if let Some(o) = other.filter(|_| matches!(req.kind.as_str(), "minor" | "not_minor")) {
        sqlx::query("DELETE FROM title_version_relations WHERE kind IN ('minor','not_minor') AND ((title_id = ? AND other_title_id = ?) OR (title_id = ? AND other_title_id = ?))").bind(title_id).bind(o).bind(o).bind(title_id).execute(&app.pool).await?;
    }
    if req.kind == "group" {
        sqlx::query("DELETE FROM title_version_relations WHERE kind = 'ungroup' AND title_id IN (?, ?)").bind(title_id).bind(other).execute(&app.pool).await?;
    }
    sqlx::query("INSERT IGNORE INTO title_version_relations (title_id, other_title_id, kind) VALUES (?, ?, ?)").bind(title_id).bind(other).bind(&req.kind).execute(&app.pool).await?;
    after_change(app, req.kind == "group" || req.kind == "ungroup");
    info(app, title_id).await
}

/// `DELETE /catalog/titles/versions/relations/{id}`.
pub async fn delete_relation(app: &App, id: u64) -> Result<()> {
    let kind: Option<String> = sqlx::query_scalar("SELECT CAST(kind AS CHAR) FROM title_version_relations WHERE id = ?").bind(id).fetch_optional(&app.pool).await?;
    let Some(kind) = kind else { return Err(Error::not_found(format!("no correction {id}"))) };
    sqlx::query("DELETE FROM title_version_relations WHERE id = ?").bind(id).execute(&app.pool).await?;
    after_change(app, kind == "group" || kind == "ungroup");
    Ok(())
}

/// The lists see the change at once; a change of sets also redraws the Definitive families
/// (which never join two versions of a set).
fn after_change(app: &App, sets_changed: bool) {
    versions::invalidate();
    if sets_changed {
        let pool = app.pool.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::database::title_family::recompute_roots(&pool).await {
                log::warn!("version sets: families: {e}");
            } else if let Err(e) = crate::database::title_family::refresh(&pool, None).await {
                log::warn!("version sets: families: {e}");
            }
        });
    }
}

/// `PUT /catalog/version-groups/{key}/pick`: `title_id` to show that version, null for the
/// held one / a random one again.
#[derive(Debug, Clone, Deserialize)]
pub struct PickRequest {
    #[serde(default)]
    pub title_id: Option<u64>,
}

fn check_key(key: &str) -> Result<()> {
    let ok = key.len() <= 64 && ["igdb:", "sha1:", "title:"].iter().any(|p| key.starts_with(p)) && key.split_once(':').is_some_and(|(_, v)| !v.is_empty() && v.chars().all(|c| c.is_ascii_alphanumeric()));
    if ok {
        Ok(())
    } else {
        Err(Error::invalid("not a version set key"))
    }
}

pub async fn pick(app: &App, key: &str, req: &PickRequest) -> Result<()> {
    check_key(key)?;
    Ok(versions::set_pick(&app.pool, key, req.title_id).await?)
}

/// `POST /catalog/version-groups/{key}/reroll`: the 🎲 button.
pub async fn reroll(app: &App, key: &str) -> Result<()> {
    check_key(key)?;
    Ok(versions::reroll(&app.pool, key).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_keys_are_checked() {
        assert!(check_key("igdb:1561").is_ok());
        assert!(check_key("sha1:0da501e3e5c51ab8fef55b092dcdd7e6b050e424").is_ok());
        assert!(check_key("title:12").is_ok());
        assert!(check_key("igdb:").is_err());
        assert!(check_key("x:1").is_err());
        assert!(check_key("igdb:1;DROP").is_err());
    }
}
