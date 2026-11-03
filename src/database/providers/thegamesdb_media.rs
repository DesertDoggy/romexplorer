//! TheGamesDB art: box front and back, title screen, screenshots, logo, fan art -- per regional
//! release, since each regional release is its own TheGamesDB game.
//!
//! The image lists cost the monthly allowance (a few thousand calls), so they are fetched for
//! many games per call (`/v1/Games/Images` takes a list of ids), kept per game for 30 days in
//! `<provider cache>/thegamesdb/images/<id>.json`, and never fetched on their own -- only for a
//! "download art" run someone asked for (`Provider::media_manual_only`). The file records
//! which image types it holds, so asking for logos later fetches only those.
//!
//! The allowance is saved in `provider_quota` after every call: the client keeps it in memory
//! only, and every process (server, worker, desktop) has a client of its own, rebuilt when the
//! credentials change. A fresh client starts from the saved figure instead of spending a call
//! to probe for it.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use playmatch_deredis::providers::thegamesdb::model::TgdbImage;
use serde::{Deserialize, Serialize};
use sqlx::mysql::MySqlPool;

use super::super::error::DatabaseError;
use super::ProviderClients;

/// Games per `/v1/Games/Images` call.
pub const BATCH: usize = 20;

/// Calls never spent: the last ones are left for whatever the user does by hand next.
const RESERVE: i32 = 2;

/// TheGamesDB image types and the slots they fill.
const TYPES: [(&str, &str, Option<&str>); 6] = [
    ("boxart", "box", Some("front")),
    ("boxart", "box", Some("back")),
    ("titlescreen", "title_screen", None),
    ("screenshot", "screenshot", None),
    ("clearlogo", "logo", None),
    ("fanart", "fanart", None),
];

/// The slot an image fills, or None (banners, an unknown type).
pub fn slot_of(image: &TgdbImage) -> Option<(&'static str, Option<&'static str>)> {
    TYPES.iter().find(|(kind, _, sub)| *kind == image.kind && (image.kind != "boxart" || image.side.as_deref() == *sub)).map(|(_, t, s)| (*t, *s))
}

/// The TheGamesDB types to ask for, for these of our media types.
pub fn tgdb_types(media_types: &[&str]) -> Vec<&'static str> {
    let set: BTreeSet<&'static str> = TYPES.iter().filter(|(_, t, _)| media_types.contains(t)).map(|(k, _, _)| *k).collect();
    set.into_iter().collect()
}

/// One game's images as cached.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CachedImages {
    /// The image types asked for when these were fetched: an absent type is "not asked",
    /// not "has none".
    pub fetched: Vec<String>,
    pub base_url: String,
    pub images: Vec<TgdbImage>,
}

impl CachedImages {
    pub fn covers(&self, types: &[&str]) -> bool {
        types.iter().all(|t| self.fetched.iter().any(|f| f == t))
    }

    pub fn url(&self, image: &TgdbImage) -> String {
        format!("{}{}", self.base_url, image.filename)
    }
}

fn cache_path(root: &Path, id: i64) -> PathBuf {
    root.join("thegamesdb").join("images").join(format!("{id}.json"))
}

/// A game's cached images, if fetched within 30 days.
pub fn cached(root: &Path, id: i64) -> Option<CachedImages> {
    let path = cache_path(root, id);
    let fresh = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() < 30 * 86_400);
    if !fresh {
        return None;
    }
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn store(root: &Path, id: i64, images: &CachedImages) -> Result<(), DatabaseError> {
    let path = cache_path(root, id);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, serde_json::to_vec(images)?)?;
    Ok(())
}

/// What a fetch came to.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Fetched {
    pub calls: u32,
    /// The allowance ran out (or down to the reserve) before every game was fetched.
    pub used_up: bool,
}

/// Fetches the image lists of those of `ids` whose cache does not cover `types`, `BATCH`
/// games per call, and caches them. Stops, with `used_up`, rather than spend the last calls.
pub async fn prefetch(clients: &ProviderClients, pool: &MySqlPool, root: &Path, ids: &[i64], types: &[&str]) -> Result<Fetched, DatabaseError> {
    let mut out = Fetched::default();
    let missing: Vec<i64> = ids.iter().copied().filter(|id| !cached(root, *id).is_some_and(|c| c.covers(types))).collect();
    if missing.is_empty() || types.is_empty() {
        return Ok(out);
    }
    let client = clients.thegamesdb.as_ref().ok_or_else(|| DatabaseError::Provider("thegamesdb client unavailable".to_string()))?;
    seed_allowance(clients, pool).await?;
    for chunk in missing.chunks(BATCH) {
        let mut found: HashMap<i64, Vec<TgdbImage>> = HashMap::new();
        let mut base_url = String::new();
        let mut page = 1u32;
        loop {
            // An unknown allowance is probed by the call itself, which costs one more.
            let left = client.known_remaining_allowance();
            if left.is_some_and(|n| n <= RESERVE) {
                out.used_up = true;
                save_allowance(clients, pool).await?;
                return Ok(out);
            }
            let (data, more) = client.games_images(chunk, types, page).await.map_err(|e| DatabaseError::Provider(e.to_string()))?;
            out.calls += 1;
            if !data.base_url.original.is_empty() {
                base_url = data.base_url.original.clone();
            }
            for (id, images) in data.images {
                if let Ok(id) = id.parse::<i64>() {
                    found.entry(id).or_default().extend(images);
                }
            }
            if !more {
                break;
            }
            page += 1;
        }
        for id in chunk {
            let mut entry = cached(root, *id).unwrap_or_default();
            let mut fetched: BTreeSet<String> = entry.fetched.into_iter().collect();
            fetched.extend(types.iter().map(|t| t.to_string()));
            entry.fetched = fetched.into_iter().collect();
            // The new answer replaces what was held of the types asked for.
            entry.images.retain(|i| !types.contains(&i.kind.as_str()));
            entry.images.extend(found.remove(id).unwrap_or_default());
            entry.images.sort_by_key(|i| i.id);
            if !base_url.is_empty() {
                entry.base_url = base_url.clone();
            }
            store(root, *id, &entry)?;
        }
        save_allowance(clients, pool).await?;
    }
    Ok(out)
}

/// A game's images covering `types`: from the cache, else fetched (one call).
pub async fn images_for(clients: &ProviderClients, pool: &MySqlPool, root: &Path, id: i64, types: &[&str]) -> Result<Option<CachedImages>, DatabaseError> {
    if let Some(hit) = cached(root, id).filter(|c| c.covers(types)) {
        return Ok(Some(hit));
    }
    if clients.thegamesdb.is_none() {
        return Ok(None);
    }
    let fetched = prefetch(clients, pool, root, &[id], types).await?;
    if fetched.used_up {
        return Err(DatabaseError::Provider(USED_UP.to_string()));
    }
    Ok(cached(root, id))
}

/// The error a fetch past the allowance ends with; `media_backfill` stops on it.
pub const USED_UP: &str = "thegamesdb: monthly API allowance used up";

/// The allowance as last saved, and when.
pub async fn saved_allowance(pool: &MySqlPool) -> Result<Option<(i32, chrono::NaiveDateTime)>, DatabaseError> {
    Ok(sqlx::query_as::<_, (Option<i32>, chrono::NaiveDateTime)>("SELECT remaining, updated_at FROM provider_quota WHERE provider = 'thegamesdb'").fetch_optional(pool).await?.and_then(|(r, at)| r.map(|r| (r, at))))
}

/// A fresh client starts from the figure saved this month, not from a probe call.
async fn seed_allowance(clients: &ProviderClients, pool: &MySqlPool) -> Result<(), DatabaseError> {
    let Some(client) = clients.thegamesdb.as_ref() else { return Ok(()) };
    if client.known_remaining_allowance().is_some() {
        return Ok(());
    }
    if let Some((remaining, at)) = saved_allowance(pool).await? {
        use chrono::Datelike;
        let now = chrono::Local::now().naive_local();
        // The allowance is monthly: last month's figure says nothing about this one.
        if at.year() == now.year() && at.month() == now.month() {
            client.seed_remaining_allowance(remaining);
        }
    }
    Ok(())
}

async fn save_allowance(clients: &ProviderClients, pool: &MySqlPool) -> Result<(), DatabaseError> {
    let Some(remaining) = clients.thegamesdb.as_ref().and_then(|c| c.known_remaining_allowance()) else { return Ok(()) };
    sqlx::query("INSERT INTO provider_quota (provider, remaining) VALUES ('thegamesdb', ?) ON DUPLICATE KEY UPDATE remaining = VALUES(remaining), updated_at = NOW()").bind(remaining).execute(pool).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(id: i64, kind: &str, side: Option<&str>) -> TgdbImage {
        TgdbImage { id, kind: kind.to_string(), side: side.map(str::to_string), filename: format!("{kind}/{id}.jpg"), resolution: None }
    }

    #[test]
    fn images_fill_their_slots() {
        assert_eq!(slot_of(&image(1, "boxart", Some("front"))), Some(("box", Some("front"))));
        assert_eq!(slot_of(&image(2, "boxart", Some("back"))), Some(("box", Some("back"))));
        assert_eq!(slot_of(&image(3, "clearlogo", None)), Some(("logo", None)));
        assert_eq!(slot_of(&image(4, "titlescreen", None)), Some(("title_screen", None)));
        assert_eq!(slot_of(&image(5, "banner", None)), None);
        assert_eq!(slot_of(&image(6, "boxart", None)), None, "a box with no side fills no slot");
    }

    #[test]
    fn only_the_types_needed_are_asked_for() {
        assert_eq!(tgdb_types(&["box"]), vec!["boxart"]);
        assert_eq!(tgdb_types(&["box", "logo", "title_screen"]), vec!["boxart", "clearlogo", "titlescreen"]);
        assert!(tgdb_types(&["cover"]).is_empty());
    }

    #[test]
    fn a_cache_covers_only_what_was_asked() {
        let c = CachedImages { fetched: vec!["boxart".into()], base_url: "https://cdn/original/".into(), images: vec![image(1, "boxart", Some("front"))] };
        assert!(c.covers(&["boxart"]));
        assert!(!c.covers(&["boxart", "clearlogo"]), "logos were never asked for");
        assert_eq!(c.url(&c.images[0]), "https://cdn/original/boxart/1.jpg");
        let dir = std::env::temp_dir().join(format!("tgdb-cache-{}", std::process::id()));
        store(&dir, 7, &c).unwrap();
        assert_eq!(cached(&dir, 7), Some(c));
        assert_eq!(cached(&dir, 8), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
