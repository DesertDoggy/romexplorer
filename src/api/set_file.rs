//! Collection sets as a file: what each set holds, in its order, written so another
//! catalogue can find the same things again -- a server started clean, or another machine.
//! Ids mean nothing there, so every entry carries what names it everywhere:
//!
//! - a release: its DAT name and platform (the catalogue's own key), and the SHA-1 of its
//!   first content, for when the DAT renamed it;
//! - a title: its IGDB game and platforms, its top release (as a release above), and its name;
//! - a game (a cross-platform row): its IGDB game, with a name to read.
//!
//! Importing puts each set's entries back in order, into the set of the same name (created when
//! there is none), and lists what this catalogue could not find.

use serde::{Deserialize, Serialize};
use sqlx::Row;

use super::sets::{self, EntryKind, SetSpec};
use super::{App, Error, Result};

pub const FORMAT: &str = "romexplorer-sets";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetFile {
    pub format: String,
    pub version: u32,
    #[serde(default)]
    pub exported_at: String,
    #[serde(default)]
    pub app_version: String,
    pub sets: Vec<SetDoc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetDoc {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub entries: Vec<EntryDoc>,
}

/// A release as another catalogue finds it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReleaseRef {
    /// The DAT's name for it.
    pub name: String,
    pub platform: u16,
    #[serde(default)]
    pub platform_name: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    /// SHA-1 of its first content.
    #[serde(default)]
    pub sha1: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EntryDoc {
    Release {
        #[serde(flatten)]
        release: ReleaseRef,
        /// Its title's name, to read.
        #[serde(default)]
        title: Option<String>,
    },
    Title {
        /// The title's (canonical) name.
        name: String,
        #[serde(default)]
        igdb_id: Option<String>,
        #[serde(default)]
        platforms: Vec<u16>,
        /// Its top release, the way back when the title has no IGDB game.
        #[serde(default)]
        top_release: Option<ReleaseRef>,
        /// Which of the title's rows (`release_buckets::BUCKETS`: retail, demo, hack, ...).
        /// A hack's or other own-release row is found again through `top_release`. Absent in
        /// files from before rows: the title's best row.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        row: String,
    },
    Game {
        igdb_id: String,
        /// A name to read.
        #[serde(default)]
        name: Option<String>,
    },
    /// A game's family (Collection > Definitive): found again by its base game's IGDB game,
    /// else by any of its versions'.
    Family {
        /// The base game's name.
        name: String,
        /// The IGDB games of the family's titles, the base game's first.
        #[serde(default)]
        igdb_ids: Vec<String>,
        /// The base game's top release: the way back for a family with no IGDB game.
        #[serde(default)]
        top_release: Option<ReleaseRef>,
    },
}

impl EntryDoc {
    fn label(&self) -> String {
        match self {
            EntryDoc::Release { release, .. } => release.name.clone(),
            EntryDoc::Title { name, .. } => name.clone(),
            EntryDoc::Game { igdb_id, name } => name.clone().unwrap_or_else(|| format!("IGDB {igdb_id}")),
            EntryDoc::Family { name, .. } => name.clone(),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            EntryDoc::Release { .. } => "release",
            EntryDoc::Title { .. } => "title",
            EntryDoc::Game { .. } => "game",
            EntryDoc::Family { .. } => "family",
        }
    }
}

const RELEASE_REF: &str = "SELECT r.name, r.app_platform_id, p.name AS platform_name, r.region, \
    (SELECT h.value_hex FROM release_contents rc JOIN hashes h ON h.content_id = rc.content_id AND h.algorithm = 'sha1' WHERE rc.release_id = r.id ORDER BY rc.content_id LIMIT 1) AS sha1 \
    FROM `release` r LEFT JOIN platforms p ON p.id = r.app_platform_id WHERE r.id = ?";

async fn release_ref(app: &App, id: u64) -> Result<Option<ReleaseRef>> {
    let Some(r) = sqlx::query(RELEASE_REF).bind(id).fetch_optional(&app.pool).await? else { return Ok(None) };
    Ok(Some(ReleaseRef { name: r.try_get("name")?, platform: r.try_get("app_platform_id")?, platform_name: r.try_get("platform_name")?, region: r.try_get("region")?, sha1: r.try_get("sha1")? }))
}

/// The sets `ids` (every set when empty) as a file.
pub async fn export(app: &App, ids: &[u64]) -> Result<SetFile> {
    let ids: Vec<u64> = if ids.is_empty() {
        sqlx::query_scalar("SELECT id FROM collection_sets WHERE deleted = FALSE ORDER BY name, id").fetch_all(&app.pool).await?
    } else {
        ids.to_vec()
    };
    let rank = super::catalog::ReleaseRank::load(app).await?;
    let top_sql = format!(
        "SELECT r.id FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE WHERE tr.title_id = ? ORDER BY {} LIMIT 1",
        rank.sql("r")
    );
    let mut out = Vec::new();
    for id in ids {
        let Some(set) = sqlx::query("SELECT name, description FROM collection_sets WHERE id = ? AND deleted = FALSE").bind(id).fetch_optional(&app.pool).await? else {
            return Err(Error::not_found(format!("no collection set {id}")));
        };
        let mut entries = Vec::new();
        for e in sqlx::query("SELECT release_id, title_id, row_key, igdb_id, family_title_id FROM collection_set_entries WHERE set_id = ? ORDER BY position, id").bind(id).fetch_all(&app.pool).await? {
            if let Some(rid) = e.try_get::<Option<u64>, _>("release_id")? {
                let Some(release) = release_ref(app, rid).await? else { continue };
                let title: Option<String> = sqlx::query_scalar("SELECT t.name FROM title_releases tr JOIN title t ON t.id = tr.title_id WHERE tr.release_id = ? LIMIT 1").bind(rid).fetch_optional(&app.pool).await?;
                entries.push(EntryDoc::Release { release, title });
            } else if let Some(tid) = e.try_get::<Option<u64>, _>("title_id")? {
                let Some(name) = sqlx::query_scalar::<_, String>("SELECT name FROM title WHERE id = ?").bind(tid).fetch_optional(&app.pool).await? else { continue };
                let igdb_id: Option<String> = sqlx::query_scalar("SELECT provider_id FROM title_provider_links WHERE title_id = ? AND provider = 'igdb' AND deleted = FALSE").bind(tid).fetch_optional(&app.pool).await?;
                let platforms: Vec<u16> = sqlx::query_scalar("SELECT DISTINCT r.app_platform_id FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE WHERE tr.title_id = ? ORDER BY r.app_platform_id").bind(tid).fetch_all(&app.pool).await?;
                let row_key: String = e.try_get("row_key")?;
                let row = row_key.split(':').next().unwrap_or("").to_string();
                // An own-release row's release is its top release, so it is found again.
                let top: Option<u64> = match crate::database::release_buckets::parse_row_key(&row_key) {
                    Some((_, Some(release))) => Some(release),
                    _ => sqlx::query_scalar(sqlx::AssertSqlSafe(top_sql.clone())).bind(tid).fetch_optional(&app.pool).await?,
                };
                let top_release = match top {
                    Some(r) => release_ref(app, r).await?,
                    None => None,
                };
                entries.push(EntryDoc::Title { name, igdb_id, platforms, top_release, row });
            } else if let Some(igdb_id) = e.try_get::<Option<String>, _>("igdb_id")? {
                let name: Option<String> = sqlx::query_scalar("SELECT t.name FROM title_provider_links l JOIN title t ON t.id = l.title_id AND t.deleted = FALSE WHERE l.provider = 'igdb' AND l.provider_id = ? AND l.deleted = FALSE ORDER BY t.id LIMIT 1").bind(&igdb_id).fetch_optional(&app.pool).await?;
                entries.push(EntryDoc::Game { igdb_id, name });
            } else if let Some(fid) = e.try_get::<Option<u64>, _>("family_title_id")? {
                let head = crate::database::title_family::head_of(&app.pool, fid).await?;
                let Some(name) = sqlx::query_scalar::<_, String>("SELECT name FROM title WHERE id = ?").bind(head).fetch_optional(&app.pool).await? else { continue };
                let igdb_ids: Vec<String> = sqlx::query_scalar(
                    "SELECT l.provider_id FROM title t JOIN title_provider_links l ON l.title_id = t.id AND l.provider = 'igdb' AND l.deleted = FALSE \
                     WHERE t.deleted = FALSE AND (t.id = ? OR t.family_root_id = ?) GROUP BY l.provider_id ORDER BY MAX(t.id = ?) DESC, MIN(t.id)",
                )
                .bind(head)
                .bind(head)
                .bind(head)
                .fetch_all(&app.pool)
                .await?;
                let top_release = match sqlx::query_scalar::<_, u64>(sqlx::AssertSqlSafe(top_sql.clone())).bind(head).fetch_optional(&app.pool).await? {
                    Some(r) => release_ref(app, r).await?,
                    None => None,
                };
                entries.push(EntryDoc::Family { name, igdb_ids, top_release });
            }
        }
        out.push(SetDoc { name: set.try_get("name")?, description: set.try_get("description")?, entries });
    }
    Ok(SetFile { format: FORMAT.to_string(), version: 1, exported_at: chrono::Utc::now().to_rfc3339(), app_version: env!("CARGO_PKG_VERSION").to_string(), sets: out })
}

/// The release this catalogue has for `r`: by DAT name and platform, else by its content.
async fn find_release(app: &App, r: &ReleaseRef) -> Result<Option<u64>> {
    let by_name: Option<u64> = sqlx::query_scalar("SELECT id FROM `release` WHERE name_hash = SHA1(?) AND app_platform_id = ? AND deleted = FALSE ORDER BY id LIMIT 1").bind(&r.name).bind(r.platform).fetch_optional(&app.pool).await?;
    if by_name.is_some() {
        return Ok(by_name);
    }
    let Some(sha1) = r.sha1.as_deref().filter(|h| !h.is_empty()) else { return Ok(None) };
    Ok(sqlx::query_scalar(
        "SELECT rc.release_id FROM hashes h JOIN release_contents rc ON rc.content_id = h.content_id JOIN `release` x ON x.id = rc.release_id AND x.deleted = FALSE \
         WHERE h.algorithm = 'sha1' AND h.value_hex = ? AND x.app_platform_id = ? ORDER BY rc.release_id LIMIT 1",
    )
    .bind(sha1.to_ascii_lowercase())
    .bind(r.platform)
    .fetch_optional(&app.pool)
    .await?)
}

/// What this catalogue has for an entry, as the id `sets::add_entries` takes, and for a
/// title row its row key; or why not.
async fn find(app: &App, e: &EntryDoc) -> Result<std::result::Result<(EntryKind, String, Option<String>), String>> {
    Ok(match e {
        EntryDoc::Release { release, .. } => match find_release(app, release).await? {
            Some(id) => Ok((EntryKind::Release, id.to_string(), None)),
            None => Err("no release of that name or content on that platform".into()),
        },
        EntryDoc::Title { name, igdb_id, platforms, top_release, row } => {
            use crate::database::release_buckets::{parse_row_key, PER_RELEASE};
            let row_key = if row.is_empty() {
                None
            } else if PER_RELEASE.contains(&row.as_str()) {
                match top_release {
                    Some(r) => find_release(app, r).await?.map(|id| format!("{row}:{id}")),
                    None => None,
                }
            } else {
                Some(row.clone())
            }
            .filter(|k| parse_row_key(k).is_some());
            if !row.is_empty() && row_key.is_none() {
                return Ok(Err(format!("no {row} release of that title here")));
            }
            let on = if platforms.is_empty() { "0".to_string() } else { platforms.iter().map(u16::to_string).collect::<Vec<_>>().join(",") };
            let mut found: Option<u64> = None;
            // Its IGDB game, on one of its platforms.
            if let Some(g) = igdb_id {
                found = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                    "SELECT t.id FROM title_provider_links l JOIN title t ON t.id = l.title_id AND t.deleted = FALSE WHERE l.provider = 'igdb' AND l.provider_id = ? AND l.deleted = FALSE \
                     AND EXISTS (SELECT 1 FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE WHERE tr.title_id = t.id AND r.app_platform_id IN ({on})) ORDER BY t.id LIMIT 1"
                )))
                .bind(g)
                .fetch_optional(&app.pool)
                .await?;
            }
            // Else the title its top release is under.
            if found.is_none() {
                if let Some(r) = top_release {
                    if let Some(rid) = find_release(app, r).await? {
                        found = sqlx::query_scalar("SELECT tr.title_id FROM title_releases tr JOIN title t ON t.id = tr.title_id AND t.deleted = FALSE WHERE tr.release_id = ? LIMIT 1").bind(rid).fetch_optional(&app.pool).await?;
                    }
                }
            }
            // Else its name on its platforms.
            if found.is_none() {
                found = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                    "SELECT t.id FROM title t WHERE t.deleted = FALSE AND t.norm_hash = SHA1(?) \
                     AND EXISTS (SELECT 1 FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE WHERE tr.title_id = t.id AND r.app_platform_id IN ({on})) ORDER BY t.id LIMIT 1"
                )))
                .bind(crate::database::matching::util::norm_key(name))
                .fetch_optional(&app.pool)
                .await?;
            }
            match found {
                Some(id) => Ok((EntryKind::Title, id.to_string(), row_key)),
                None => Err("no title with that IGDB game, top release or name".into()),
            }
        }
        EntryDoc::Game { igdb_id, .. } => {
            let linked: Option<u64> = sqlx::query_scalar("SELECT l.title_id FROM title_provider_links l JOIN title t ON t.id = l.title_id AND t.deleted = FALSE WHERE l.provider = 'igdb' AND l.provider_id = ? AND l.deleted = FALSE LIMIT 1").bind(igdb_id).fetch_optional(&app.pool).await?;
            match linked {
                Some(_) => Ok((EntryKind::Game, igdb_id.clone(), None)),
                None => Err("no title is linked to that IGDB game".into()),
            }
        }
        EntryDoc::Family { igdb_ids, top_release, .. } => {
            let mut found: Option<u64> = None;
            for g in igdb_ids {
                found = sqlx::query_scalar("SELECT l.title_id FROM title_provider_links l JOIN title t ON t.id = l.title_id AND t.deleted = FALSE WHERE l.provider = 'igdb' AND l.provider_id = ? AND l.deleted = FALSE ORDER BY t.id LIMIT 1").bind(g).fetch_optional(&app.pool).await?;
                if found.is_some() {
                    break;
                }
            }
            if found.is_none() {
                if let Some(r) = top_release {
                    if let Some(rid) = find_release(app, r).await? {
                        found = sqlx::query_scalar("SELECT tr.title_id FROM title_releases tr JOIN title t ON t.id = tr.title_id AND t.deleted = FALSE WHERE tr.release_id = ? LIMIT 1").bind(rid).fetch_optional(&app.pool).await?;
                    }
                }
            }
            match found {
                Some(t) => Ok((EntryKind::Family, t.to_string(), None)),
                None => Err("no title with that family's IGDB games or top release".into()),
            }
        }
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImportRequest {
    pub file: SetFile,
    /// Only these sets of the file (by name); all when empty.
    #[serde(default)]
    pub names: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Missing {
    pub kind: String,
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetImported {
    pub name: String,
    pub set_id: u64,
    /// A set of this name did not exist and was made.
    pub created: bool,
    pub added: u64,
    /// In the set already.
    pub already: u64,
    pub missing: Vec<Missing>,
}

/// Puts the file's sets into this catalogue: each into the live set of its name (made when
/// there is none, with the file's description), its entries in the file's order after what
/// the set holds. Nothing is removed.
pub async fn import(app: &App, req: &ImportRequest) -> Result<Vec<SetImported>> {
    if req.file.format != FORMAT {
        return Err(Error::invalid("that is not a romexplorer sets file"));
    }
    if req.file.version > 1 {
        return Err(Error::invalid(format!("this sets file is version {}; this romexplorer reads version 1", req.file.version)));
    }
    let mut out = Vec::new();
    for doc in req.file.sets.iter().filter(|s| req.names.is_empty() || req.names.contains(&s.name)) {
        let existing: Option<u64> = sqlx::query_scalar("SELECT id FROM collection_sets WHERE name = ? AND deleted = FALSE ORDER BY id LIMIT 1").bind(doc.name.trim()).fetch_optional(&app.pool).await?;
        let (set_id, created) = match existing {
            Some(id) => (id, false),
            None => (sets::create(app, &SetSpec { name: doc.name.clone(), description: doc.description.clone() }).await?.id, true),
        };
        let mut done = SetImported { name: doc.name.clone(), set_id, created, added: 0, already: 0, missing: Vec::new() };
        for e in &doc.entries {
            match find(app, e).await? {
                Ok((kind, id, row_key)) => match match row_key {
                    Some(row_key) => sets::add_title_rows(app, set_id, &[romexplorer_types::copy::TitleRowRef { title_id: id.parse().unwrap_or(0), row_key }]).await?,
                    None => sets::add_entries(app, set_id, kind, &[id]).await?,
                } {
                    0 => done.already += 1,
                    n => done.added += n,
                },
                Err(reason) => done.missing.push(Missing { kind: e.kind().to_string(), name: e.label(), reason }),
            }
        }
        sets::resolve(app, &[set_id]).await?;
        out.push(done);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file reads back as written, each entry kind with its own fields.
    #[test]
    fn a_sets_file_round_trips() {
        let file = SetFile {
            format: FORMAT.into(),
            version: 1,
            exported_at: "2026-10-28T00:00:00Z".into(),
            app_version: "0.1.6".into(),
            sets: vec![SetDoc {
                name: "Trip".into(),
                description: None,
                entries: vec![
                    EntryDoc::Release { release: ReleaseRef { name: "Hoge Quest (Japan)".into(), platform: 19, sha1: Some("ab".into()), ..Default::default() }, title: Some("Hoge Quest".into()) },
                    EntryDoc::Title { name: "Fuga Saga".into(), igdb_id: Some("42".into()), platforms: vec![19], top_release: None, row: String::new() },
                    EntryDoc::Game { igdb_id: "7".into(), name: None },
                ],
            }],
        };
        let text = serde_json::to_string(&file).unwrap();
        assert!(text.contains(r#""kind":"release","name":"Hoge Quest (Japan)","platform":19"#), "{text}");
        let back: SetFile = serde_json::from_str(&text).unwrap();
        assert_eq!(back.sets[0].entries.iter().map(EntryDoc::kind).collect::<Vec<_>>(), vec!["release", "title", "game"]);
    }
}
