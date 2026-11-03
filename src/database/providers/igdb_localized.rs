//! IGDB's localized covers: a game's own box for a region (the Japanese box, the European
//! one), beside the main cover every release gets.
//!
//! IGDB gives them per `game_localizations` record (a region and a cover). Each is filed in
//! the region folder of the releases it is for -- `igdb/<id>/JP/box_front.jpg` next to the main
//! cover in `igdb/<id>/common/` -- and region art ranks above `common` wherever art is picked,
//! so a Japanese release shows its Japanese box and the rest keep the main one. Nothing is
//! replaced or unlinked.
//!
//! A release takes the localized cover of its main region: the first of its regions in
//! `region_priority`. So `JP` takes the Japanese box, `EU` or `DE` the European one, and a
//! `US,EU` or `WORLD` release keeps the main cover.
//!
//! The lists are fetched for many games per call (localizations, their regions, the covers --
//! 100 ids each) and kept per game for 30 days in `<provider cache>/igdb/localized_covers/<id>.json`,
//! an empty list included, so a game with none is not asked again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use playmatch_deredis::providers::igdb::model::Game;
use serde::{Deserialize, Serialize};
use sqlx::mysql::MySqlPool;
use sqlx::Row;

use super::super::error::DatabaseError;
use super::super::media_store::{self, MediaTarget};
use super::media::Ask;
use super::{Provider, ProviderClients};

/// Ids per IGDB list request.
const LIST_MAX: usize = 100;

/// One localized cover: our region code (`JP`, `EU`) and the picture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalCover {
    pub region: String,
    pub url: String,
}

fn cache_path(root: &Path, game_id: i32) -> PathBuf {
    root.join("igdb").join("localized_covers").join(format!("{game_id}.json"))
}

/// A game's localized covers, if fetched within 30 days.
pub fn cached(root: &Path, game_id: i32) -> Option<Vec<LocalCover>> {
    let path = cache_path(root, game_id);
    let fresh = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() < 30 * 86_400);
    if !fresh {
        return None;
    }
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn store(root: &Path, game_id: i32, covers: &[LocalCover]) -> Result<(), DatabaseError> {
    let path = cache_path(root, game_id);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, serde_json::to_vec(covers)?)?;
    Ok(())
}

/// Whether `game` could have any: it has localizations, and none are cached yet.
fn needs_fetch(root: &Path, game: &Game) -> bool {
    game.game_localizations.as_ref().is_some_and(|l| !l.is_empty()) && cached(root, game.id).is_none()
}

/// An IGDB image URL at cover size, as the main cover is stored.
pub fn cover_url(url: &str) -> String {
    let url = url.replace("t_thumb", "t_cover_big");
    if url.starts_with("//") { format!("https:{url}") } else { url }
}

/// Fetches and caches the localized covers of those of `games` not cached yet. Returns the
/// calls made.
pub async fn prefetch(clients: &ProviderClients, root: &Path, games: &[&Game]) -> Result<u32, DatabaseError> {
    let games: Vec<&Game> = games.iter().copied().filter(|g| needs_fetch(root, g)).collect();
    if games.is_empty() {
        return Ok(0);
    }
    let client = clients.igdb.as_ref().ok_or_else(|| DatabaseError::Provider("igdb client unavailable".to_string()))?;
    let mut calls = 0u32;
    let num = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_i64()).map(|n| n as i32);
    // localization id -> (cover id, region id)
    let mut locs: HashMap<i32, (i32, i32)> = HashMap::new();
    let mut loc_ids: Vec<i32> = games.iter().flat_map(|g| g.game_localizations.clone().unwrap_or_default()).collect();
    loc_ids.sort_unstable();
    loc_ids.dedup();
    for chunk in loc_ids.chunks(LIST_MAX) {
        calls += 1;
        for l in client.get_raw_by_ids("game_localizations", chunk.to_vec(), "cover,region").await.map_err(|e| DatabaseError::Provider(e.to_string()))? {
            if let (Some(id), Some(cover), Some(region)) = (num(&l, "id"), num(&l, "cover"), num(&l, "region")) {
                locs.insert(id, (cover, region));
            }
        }
    }
    let mut region_ids: Vec<i32> = locs.values().map(|(_, r)| *r).collect();
    region_ids.sort_unstable();
    region_ids.dedup();
    let mut regions: HashMap<i32, &'static str> = HashMap::new();
    for chunk in region_ids.chunks(LIST_MAX) {
        calls += 1;
        for r in client.get_raw_by_ids("regions", chunk.to_vec(), "identifier").await.map_err(|e| DatabaseError::Provider(e.to_string()))? {
            let ident = r.get("identifier").and_then(|x| x.as_str());
            if let (Some(id), Some((_, code))) = (num(&r, "id"), ident.and_then(super::metadata::igdb_region_locale)) {
                // Worldwide is what the main cover is for.
                if !code.is_empty() {
                    regions.insert(id, code);
                }
            }
        }
    }
    let mut cover_ids: Vec<i32> = locs.values().filter(|(_, r)| regions.contains_key(r)).map(|(c, _)| *c).collect();
    cover_ids.sort_unstable();
    cover_ids.dedup();
    let mut urls: HashMap<i32, String> = HashMap::new();
    for chunk in cover_ids.chunks(LIST_MAX) {
        calls += 1;
        // Raw, with just the URL: a localization's cover need not carry every field the typed
        // model insists on (its game, say), and one such record would fail the whole list.
        for c in client.get_raw_by_ids("covers", chunk.to_vec(), "url").await.map_err(|e| DatabaseError::Provider(e.to_string()))? {
            if let (Some(id), Some(url)) = (num(&c, "id"), c.get("url").and_then(|u| u.as_str())) {
                urls.insert(id, cover_url(url));
            }
        }
    }
    log::info!(
        "igdb localized covers: {} game(s), {} localization(s), {} with a region we map, {} cover URL(s), {calls} call(s)",
        games.len(),
        locs.len(),
        locs.values().filter(|(_, r)| regions.contains_key(r)).count(),
        urls.len()
    );
    for game in games {
        let mut list: Vec<LocalCover> = Vec::new();
        for id in game.game_localizations.iter().flatten() {
            let Some((cover, region)) = locs.get(id) else { continue };
            // The main cover itself, given again for a region: nothing to add.
            if game.cover == Some(*cover) {
                continue;
            }
            if let (Some(code), Some(url)) = (regions.get(region), urls.get(cover)) {
                if !list.iter().any(|c| c.region == *code) {
                    list.push(LocalCover { region: code.to_string(), url: url.clone() });
                }
            }
        }
        store(root, game.id, &list)?;
    }
    Ok(calls)
}

/// [`prefetch`] for IGDB ids, from their cached games (ids with no cached game are skipped:
/// [`attach`] fetches theirs when it gets to them).
pub async fn prefetch_ids(clients: &ProviderClients, pool: &MySqlPool, root: &Path, ids: &[i32]) -> Result<u32, DatabaseError> {
    let mut games = Vec::new();
    for id in ids {
        if let Some(game) = super::cache::load_cached_igdb_game(pool, *id).await? {
            games.push(game);
        }
    }
    prefetch(clients, root, &games.iter().collect::<Vec<_>>()).await
}

/// A release's main region: the first of its regions in `priority` (else its first).
fn main_region<'a>(codes: &[&'a str], priority: &[String]) -> Option<&'a str> {
    let rank = |c: &str| priority.iter().position(|p| p == c).unwrap_or(usize::MAX);
    codes.iter().enumerate().min_by_key(|(i, c)| (rank(c), *i)).map(|(_, c)| *c)
}

/// The localized cover for a release sold in `codes`, if one is for its main region: the
/// same code, or a country inside it (`DE` takes the European box). `WORLD` takes none.
pub fn cover_for<'a>(covers: &'a [LocalCover], codes: &[&str], priority: &[String]) -> Option<&'a LocalCover> {
    let main = main_region(codes, priority).filter(|c| *c != "WORLD")?;
    covers.iter().map(|c| (super::thegamesdb_region::score(&[main], Some(c.region.as_str()), priority).0, c)).filter(|(s, _)| *s >= 3).max_by_key(|(s, _)| *s).map(|(_, c)| c)
}

/// Downloads the localized covers of `linked` (each game with the releases its art goes to)
/// into their releases' region folders. Fetches the lists first for those not cached, when
/// the client is there. Returns the links made.
pub async fn attach(
    http: &reqwest::Client,
    pool: &MySqlPool,
    media_dir: &Path,
    clients: &ProviderClients,
    root: &Path,
    ask: &Ask,
    linked: &[(Vec<MediaTarget>, &Game)],
) -> Result<u32, DatabaseError> {
    let rules = super::super::settings::MediaRules::load(pool).await?;
    if !ask.allows(&rules, Provider::Igdb.name(), "box", Some("front")) {
        return Ok(0);
    }
    let games: Vec<&Game> = linked.iter().map(|(_, g)| *g).filter(|g| g.game_localizations.as_ref().is_some_and(|l| !l.is_empty())).collect();
    if games.is_empty() {
        return Ok(0);
    }
    if clients.is_available(Provider::Igdb) {
        if let Err(e) = prefetch(clients, root, &games).await {
            log::warn!("media: igdb localized covers: {e}");
        }
    }
    let priority = super::super::settings::region_priority(pool).await?;
    let mut made = 0u32;
    for (targets, game) in linked {
        let Some(covers) = cached(root, game.id).filter(|c| !c.is_empty()) else { continue };
        let mut by_url: Vec<(&str, Vec<MediaTarget>)> = Vec::new();
        for target in targets {
            let MediaTarget::Release(id) = target else { continue };
            let region: Option<String> = sqlx::query("SELECT region FROM `release` WHERE id = ?").bind(id).fetch_optional(pool).await?.and_then(|r| r.try_get("region").ok()).flatten();
            let codes = super::thegamesdb_region::release_codes(region.as_deref());
            let Some(cover) = cover_for(&covers, &codes, &priority) else { continue };
            // The user's own pick always shows first: nothing to fetch for it.
            if media_store::has_manual(pool, *target, "box", Some("front")).await? {
                continue;
            }
            match by_url.iter_mut().find(|(u, _)| *u == cover.url) {
                Some((_, list)) => list.push(*target),
                None => by_url.push((cover.url.as_str(), vec![*target])),
            }
        }
        let provider_id = game.id.to_string();
        for (url, releases) in by_url {
            // One file for every group it is for, filed in the first group's folder: the
            // retail one, not a demo's (`JP/_prerelease/`).
            let mut groups = Vec::new();
            for group in super::media::split_by_group(pool, &releases).await? {
                let pre = match group.first() {
                    Some(MediaTarget::Release(id)) => media_store::release_group(pool, *id).await?.is_some_and(|g| g.folder.ends_with("_prerelease")),
                    _ => false,
                };
                groups.push((pre, group));
            }
            groups.sort_by_key(|(pre, _)| *pre);
            if let Some((false, retail)) = groups.first() {
                if let Err(e) = refile(pool, media_dir, url, &provider_id, retail).await {
                    log::warn!("media: igdb localized box for game {}: moving it out of _prerelease failed: {e}", game.id);
                }
            }
            for (_, group) in groups {
                let mut pending = Vec::new();
                for target in group {
                    if !super::media::has_url(pool, target, url).await? {
                        pending.push(target);
                    }
                }
                if pending.is_empty() {
                    continue;
                }
                match super::media::store_url(http, pool, media_dir, Provider::Igdb.name(), Some(&provider_id), url, "box", Some("front"), &pending).await {
                    Ok((_, n)) => made += n,
                    Err(e) => log::warn!("media: igdb localized box for game {} failed: {e}", game.id),
                }
            }
        }
    }
    Ok(made)
}

/// A localized cover filed under a demo's group (`JP/_prerelease/`) -- it was stored for that
/// group first -- moved to the retail group's folder (`JP/`). Nothing when it is elsewhere.
async fn refile(pool: &MySqlPool, media_dir: &Path, url: &str, provider_id: &str, retail: &[MediaTarget]) -> Result<(), DatabaseError> {
    let Some(row) = sqlx::query("SELECT id, path, root_role FROM media WHERE source_url = ? AND media_type = 'box' AND deleted = FALSE ORDER BY id LIMIT 1").bind(url).fetch_optional(pool).await? else {
        return Ok(());
    };
    let path: String = row.try_get("path")?;
    if !path.contains("/_prerelease/") {
        return Ok(());
    }
    let media_id: u64 = row.try_get("id")?;
    let roots = media_store::Roots::load(pool, media_dir).await?;
    let old = roots.resolve(row.try_get::<Option<String>, _>("root_role")?.as_deref(), &path);
    if !old.is_file() {
        return Ok(());
    }
    let (placement, _) = media_store::placement_for(pool, retail, "box", Some("front"), Some(url), Provider::Igdb.name(), Some(provider_id)).await?;
    if placement.folder.contains("_prerelease") {
        return Ok(());
    }
    let ext = old.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_else(|| "jpg".to_string());
    let (root_role, root) = roots.write_root(placement.role);
    let rel = media_store::free_path(pool, root, root_role, &placement, &ext).await?;
    media_store::move_file(&old, &media_store::join_rel(root, &rel)).await?;
    sqlx::query("UPDATE media SET path = ?, root_role = ? WHERE id = ?").bind(&rel).bind(root_role.name()).bind(media_id).execute(pool).await?;
    Ok(())
}

/// [`attach`] for the titles a sync just linked, each to every release of its title.
pub async fn attach_for_titles(http: &reqwest::Client, pool: &MySqlPool, media_dir: &Path, clients: &ProviderClients, root: &Path, linked: &[(u64, &Game)]) -> Result<u32, DatabaseError> {
    let mut targeted = Vec::new();
    for (title_id, game) in linked {
        if game.game_localizations.as_ref().is_some_and(|l| !l.is_empty()) {
            targeted.push((super::media::release_targets(pool, *title_id, Provider::Igdb, &game.id.to_string()).await?, *game));
        }
    }
    attach(http, pool, media_dir, clients, root, &Ask::Auto, &targeted).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn covers() -> Vec<LocalCover> {
        vec![LocalCover { region: "JP".into(), url: "jp".into() }, LocalCover { region: "EU".into(), url: "eu".into() }]
    }

    fn prio(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_release_takes_its_main_regions_cover() {
        let p = prio(&["JP", "US", "EU"]);
        let c = covers();
        assert_eq!(cover_for(&c, &["JP"], &p).map(|c| c.url.as_str()), Some("jp"));
        assert_eq!(cover_for(&c, &["EU"], &p).map(|c| c.url.as_str()), Some("eu"));
        assert_eq!(cover_for(&c, &["DE"], &p).map(|c| c.url.as_str()), Some("eu"), "a country inside Europe");
        assert_eq!(cover_for(&c, &["US", "EU"], &p), None, "US comes first: the main cover");
        assert_eq!(cover_for(&c, &["EU", "US"], &prio(&["EU", "US"])).map(|c| c.url.as_str()), Some("eu"), "EU first in the priority");
        assert_eq!(cover_for(&c, &["WORLD"], &p), None);
        assert_eq!(cover_for(&c, &["US"], &p), None);
        assert_eq!(cover_for(&c, &[], &p), None);
    }
}
