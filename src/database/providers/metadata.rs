//! Provider metadata -> the source-tagged title tables (`title_genres`/`title_franchises`/
//! `title_companies`, `title.category`/`basegame_title_id`): one row per (title, value,
//! source), never overwriting another source's rows (docs/plan/database.md). IGDB stores
//! ids for genres/companies/franchises, so [`IgdbNames`] resolves them through the client's
//! list getters -- batched per page, cached for the process.

use std::collections::{HashMap, HashSet};

use playmatch_deredis::providers::igdb::model::{Game as IgdbGame, InvolvedCompany};
use playmatch_deredis::providers::mobygames::model::MgGame;
use playmatch_deredis::providers::screenscraper::model::SsGame;
use sqlx::mysql::MySqlPool;
use sqlx::Row;

use super::{ra_genre, Provider, ProviderClients, Tier};
use crate::database::error::DatabaseError;

pub const ROLE_DEVELOPER: &str = "developer";
pub const ROLE_PUBLISHER: &str = "publisher";
pub const ROLE_PORTING: &str = "porting_studio";

/// Every genre row sits under one of `ra_genre::MAJOR_GENRES` (its `parent_genre_id`),
/// classified by the same keyword rules for every source, so "2D Platforming" (RA),
/// "Platform" (IGDB) and "Platformer" all filter as the major genre Platformer while the
/// raw name stays available as a subgenre. A major genre's own row has no parent.
pub async fn ensure_genre(pool: &MySqlPool, raw: &str) -> Result<u16, DatabaseError> {
    let raw = raw.trim();
    let major = ra_genre::normalize_ra_genre(raw);
    // Select-first throughout (see `database::lookup_or_insert_name`): `genre.id` is a
    // SMALLINT that an insert-every-call upsert would exhaust.
    let major_id = super::super::lookup_or_insert_name(pool, "genre", major).await? as u16;
    if ra_genre::genre_key(raw) == ra_genre::genre_key(major) {
        return Ok(major_id);
    }
    if let Some(row) = sqlx::query("SELECT id, parent_genre_id FROM genre WHERE name = ?").bind(raw).fetch_optional(pool).await? {
        let id: u16 = row.try_get("id")?;
        if row.try_get::<Option<u16>, _>("parent_genre_id")?.is_none() && id != major_id {
            sqlx::query("UPDATE genre SET parent_genre_id = ? WHERE id = ?").bind(major_id).bind(id).execute(pool).await?;
        }
        return Ok(id);
    }
    sqlx::query("INSERT IGNORE INTO genre (name, parent_genre_id) VALUES (?, ?)").bind(raw).bind(major_id).execute(pool).await?;
    Ok(sqlx::query("SELECT id FROM genre WHERE name = ?").bind(raw).fetch_one(pool).await?.try_get("id")?)
}

pub async fn set_title_genres(pool: &MySqlPool, title_id: u64, source: &str, genres: &[&str]) -> Result<(), DatabaseError> {
    for genre in genres.iter().map(|g| g.trim()).filter(|g| !g.is_empty() && !ra_genre::is_not_a_genre(g)) {
        let genre_id = ensure_genre(pool, genre).await?;
        sqlx::query("INSERT IGNORE INTO title_genres (title_id, genre_id, source) VALUES (?, ?, ?)").bind(title_id).bind(genre_id).bind(source).execute(pool).await?;
    }
    Ok(())
}

/// Free tags across titles: a provider's scope tags that aren't genres ("Indie"), its
/// keyword list, or the user's own. Source-tagged like the other title tables.
pub async fn set_title_keywords(pool: &MySqlPool, title_id: u64, source: &str, names: &[&str]) -> Result<(), DatabaseError> {
    for name in names.iter().map(|n| n.trim()).filter(|n| !n.is_empty()) {
        sqlx::query("INSERT IGNORE INTO keyword (name) VALUES (?)").bind(name).execute(pool).await?;
        let id: u64 = sqlx::query("SELECT id FROM keyword WHERE name = ?").bind(name).fetch_one(pool).await?.try_get("id")?;
        sqlx::query("INSERT IGNORE INTO title_keywords (title_id, keyword_id, source) VALUES (?, ?, ?)").bind(title_id).bind(id).bind(source).execute(pool).await?;
    }
    Ok(())
}

/// Player perspective ("Side view", "First person", ...): its own metadata, never a genre.
pub async fn set_title_perspectives(pool: &MySqlPool, title_id: u64, source: &str, names: &[&str]) -> Result<(), DatabaseError> {
    for name in names.iter().map(|n| n.trim()).filter(|n| !n.is_empty()) {
        let id = super::super::lookup_or_insert_name(pool, "perspective", name).await? as u16;
        sqlx::query("INSERT IGNORE INTO title_perspectives (title_id, perspective_id, source) VALUES (?, ?, ?)").bind(title_id).bind(id).bind(source).execute(pool).await?;
    }
    Ok(())
}

pub async fn set_title_franchises(pool: &MySqlPool, title_id: u64, source: &str, names: &[&str]) -> Result<(), DatabaseError> {
    for name in names.iter().map(|n| n.trim()).filter(|n| !n.is_empty()) {
        sqlx::query("INSERT IGNORE INTO franchise (name) VALUES (?)").bind(name).execute(pool).await?;
        let row = sqlx::query("SELECT id FROM franchise WHERE name = ?").bind(name).fetch_one(pool).await?;
        let id: u64 = row.try_get("id")?;
        sqlx::query("INSERT IGNORE INTO title_franchises (title_id, franchise_id, source) VALUES (?, ?, ?)").bind(title_id).bind(id).bind(source).execute(pool).await?;
    }
    Ok(())
}

/// `entries`: (company name, role) with role one of `ROLE_*`.
pub async fn set_title_companies(pool: &MySqlPool, title_id: u64, source: &str, entries: &[(&str, &str)]) -> Result<(), DatabaseError> {
    for (name, role) in entries.iter().map(|(n, r)| (n.trim(), *r)).filter(|(n, _)| !n.is_empty()) {
        sqlx::query("INSERT IGNORE INTO company (name) VALUES (?)").bind(name).execute(pool).await?;
        let row = sqlx::query("SELECT id FROM company WHERE name = ?").bind(name).fetch_one(pool).await?;
        let id: u64 = row.try_get("id")?;
        sqlx::query("INSERT IGNORE INTO title_companies (title_id, company_id, role, source) VALUES (?, ?, ?, ?)").bind(title_id).bind(id).bind(role).bind(source).execute(pool).await?;
    }
    Ok(())
}

pub async fn set_title_category(pool: &MySqlPool, title_id: u64, category: &str, basegame_title_id: Option<u64>) -> Result<(), DatabaseError> {
    // The category always lands; the base-game pointer only when it names another title.
    let basegame = basegame_title_id.filter(|b| *b != title_id);
    sqlx::query("UPDATE title SET category = ?, basegame_title_id = COALESCE(?, basegame_title_id) WHERE id = ? AND basegame_locked = FALSE").bind(category).bind(basegame).bind(title_id).execute(pool).await?;
    Ok(())
}

/// Every title a provider id is linked to: directly (title-tier providers) or through a
/// linked release (release-tier providers).
pub async fn titles_linked(pool: &MySqlPool, provider: Provider, provider_id: &str) -> Result<Vec<u64>, DatabaseError> {
    let rows = match provider.tier() {
        Tier::Title | Tier::Content => sqlx::query("SELECT title_id AS id FROM title_provider_links WHERE provider = ? AND provider_id = ? AND deleted = FALSE").bind(provider.name()).bind(provider_id).fetch_all(pool).await?,
        Tier::Release => sqlx::query("SELECT DISTINCT tr.title_id AS id FROM release_provider_links rpl JOIN title_releases tr ON tr.release_id = rpl.release_id WHERE rpl.provider = ? AND rpl.provider_id = ? AND rpl.deleted = FALSE").bind(provider.name()).bind(provider_id).fetch_all(pool).await?,
    };
    rows.iter().map(|r| Ok(r.try_get::<u64, _>("id")?)).collect()
}

// ---------------------------------------------------------------------------
// IGDB
// ---------------------------------------------------------------------------

/// IGDB `game_type` (new) / `category` (old) codes -> this schema's `title.category`.
pub fn igdb_category(game: &IgdbGame) -> &'static str {
    match game.game_type.or(game.category).unwrap_or(0) {
        1 => "dlc",
        2 => "expansion",
        4 => "standalone",
        3 | 13 => "bundle",
        5 => "mod",
        8 | 10 => "remake",
        9 => "remaster",
        11 => "port",
        // A Complete / GOTY edition: a main game that is a version of another one.
        0 if game.version_parent.is_some() => "edition",
        _ => "base_game",
    }
}

/// Resolved names for the ids an IGDB game carries. One instance per bulk page / job:
/// `resolve` fetches whatever isn't cached yet in as few list requests as possible.
#[derive(Default)]
pub struct IgdbNames {
    genres: HashMap<i32, String>,
    perspectives: HashMap<i32, String>,
    franchises: HashMap<i32, String>,
    companies: HashMap<i32, String>,
    involved: HashMap<i32, InvolvedCompany>,
    /// game_localizations id -> (name, region id); region id -> IGDB region identifier.
    localizations: HashMap<i32, (String, Option<i32>)>,
    regions: HashMap<i32, String>,
    /// alternative_names id -> (name, comment such as "Japanese title").
    alternative_names: HashMap<i32, (String, Option<String>)>,
}

const IGDB_LIST_MAX: usize = 100;

impl IgdbNames {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn resolve(&mut self, clients: &ProviderClients, games: &[IgdbGame]) -> Result<(), DatabaseError> {
        let client = clients.igdb.as_ref().ok_or_else(|| DatabaseError::Provider("igdb client unavailable".to_string()))?;
        let missing = |have: &HashMap<i32, String>, ids: Vec<i32>| -> Vec<i32> {
            let mut seen = HashSet::new();
            ids.into_iter().filter(|id| !have.contains_key(id) && seen.insert(*id)).collect()
        };
        let genre_ids = missing(&self.genres, games.iter().flat_map(|g| g.genres.clone().unwrap_or_default()).collect());
        for chunk in genre_ids.chunks(IGDB_LIST_MAX) {
            for g in client.get_genres_by_id(chunk.to_vec()).await? {
                self.genres.insert(g.id, g.name);
            }
        }
        let perspective_ids = missing(&self.perspectives, games.iter().flat_map(|g| g.player_perspectives.clone().unwrap_or_default()).collect());
        for chunk in perspective_ids.chunks(IGDB_LIST_MAX) {
            for p in client.get_player_perspectives_by_id(chunk.to_vec()).await? {
                self.perspectives.insert(p.id, p.name);
            }
        }
        let franchise_ids = missing(&self.franchises, games.iter().flat_map(|g| g.franchises.clone().unwrap_or_default().into_iter().chain(g.franchise)).collect());
        for chunk in franchise_ids.chunks(IGDB_LIST_MAX) {
            for f in client.get_franchises_by_id(chunk.to_vec()).await? {
                self.franchises.insert(f.id, f.name);
            }
        }
        let mut involved_ids: Vec<i32> = games.iter().flat_map(|g| g.involved_companies.clone().unwrap_or_default()).filter(|id| !self.involved.contains_key(id)).collect();
        involved_ids.sort_unstable();
        involved_ids.dedup();
        for chunk in involved_ids.chunks(IGDB_LIST_MAX) {
            for ic in client.get_involved_companies_by_id(chunk.to_vec()).await? {
                self.involved.insert(ic.id, ic);
            }
        }
        let company_ids = missing(&self.companies, self.involved.values().map(|ic| ic.company).collect());
        for chunk in company_ids.chunks(IGDB_LIST_MAX) {
            for c in client.get_companies_by_id(chunk.to_vec()).await? {
                self.companies.insert(c.id, c.name);
            }
        }
        self.resolve_names(clients, games).await
    }

    /// Only the localized and alternative names -- what [`Self::other_names`] reads, for a
    /// name match that should not pay for genres and companies too.
    pub async fn resolve_names(&mut self, clients: &ProviderClients, games: &[IgdbGame]) -> Result<(), DatabaseError> {
        let client = clients.igdb.as_ref().ok_or_else(|| DatabaseError::Provider("igdb client unavailable".to_string()))?;
        let missing = |have: &HashMap<i32, String>, ids: Vec<i32>| -> Vec<i32> {
            let mut seen = HashSet::new();
            ids.into_iter().filter(|id| !have.contains_key(id) && seen.insert(*id)).collect()
        };
        // Localized names: per-region localizations and alternative names. Fetched as raw
        // JSON with just the fields used -- IGDB leaves `name` out of some of these records,
        // which the typed models reject.
        let text = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
        let num = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_i64()).map(|n| n as i32);
        let mut loc_ids: Vec<i32> = games.iter().flat_map(|g| g.game_localizations.clone().unwrap_or_default()).filter(|id| !self.localizations.contains_key(id)).collect();
        loc_ids.sort_unstable();
        loc_ids.dedup();
        for chunk in loc_ids.chunks(IGDB_LIST_MAX) {
            for l in client.get_raw_by_ids("game_localizations", chunk.to_vec(), "name,region").await? {
                if let (Some(id), Some(name)) = (num(&l, "id"), text(&l, "name")) {
                    self.localizations.insert(id, (name, num(&l, "region")));
                }
            }
        }
        let region_ids = missing(&self.regions, self.localizations.values().filter_map(|(_, r)| *r).collect());
        for chunk in region_ids.chunks(IGDB_LIST_MAX) {
            for r in client.get_raw_by_ids("regions", chunk.to_vec(), "identifier").await? {
                if let (Some(id), Some(ident)) = (num(&r, "id"), text(&r, "identifier")) {
                    self.regions.insert(id, ident);
                }
            }
        }
        let mut alt_ids: Vec<i32> = games.iter().flat_map(|g| g.alternative_names.clone().unwrap_or_default()).filter(|id| !self.alternative_names.contains_key(id)).collect();
        alt_ids.sort_unstable();
        alt_ids.dedup();
        for chunk in alt_ids.chunks(IGDB_LIST_MAX) {
            for a in client.get_raw_by_ids("alternative_names", chunk.to_vec(), "name,comment").await? {
                if let (Some(id), Some(name)) = (num(&a, "id"), text(&a, "name")) {
                    self.alternative_names.insert(id, (name, text(&a, "comment")));
                }
            }
        }
        Ok(())
    }

    /// `game`'s names besides its main one -- per-region localizations ("Gensou Suikoden",
    /// "幻想水滸伝") and alternative names -- as far as `resolve`/`resolve_names` fetched them.
    pub fn other_names<'a>(&'a self, game: &IgdbGame) -> Vec<&'a str> {
        let localized = game.game_localizations.iter().flatten().filter_map(|id| self.localizations.get(id)).map(|(n, _)| n.as_str());
        let alternative = game.alternative_names.iter().flatten().filter_map(|id| self.alternative_names.get(id)).map(|(n, _)| n.as_str());
        localized.chain(alternative).collect()
    }
}

/// (language, region) of an IGDB localization region. IGDB identifies regions by locale
/// code ("ja-JP", "ko-KR") or continent ("EU"); older names ("japan", "europe") are accepted
/// too. `None`: not a region this app has a code for.
pub fn igdb_region_locale(identifier: &str) -> Option<(&'static str, &'static str)> {
    let id = identifier.trim();
    Some(match id.to_ascii_lowercase().as_str() {
        "ja-jp" | "japan" => ("ja", "JP"),
        "ko-kr" | "korea" => ("kr", "KR"),
        "zh-cn" | "china" => ("zh-s", "CN"),
        "zh-tw" | "taiwan" => ("zh-t", "TW"),
        "zh-hk" | "hong_kong" => ("zh-t", "HK"),
        "eu" | "europe" => ("en", "EU"),
        "en-us" | "na" | "north_america" => ("en", "US"),
        "en-gb" => ("en", "UK"),
        "en-au" | "au" | "australia" | "new_zealand" => ("en", "AU"),
        "pt-br" | "brazil" => ("pt", "BR"),
        "fr-fr" => ("fr", "FR"),
        "de-de" => ("de", "DE"),
        "es-es" => ("es", "ES"),
        "it-it" => ("it", "IT"),
        "ru-ru" => ("ru", "RU"),
        "ww" | "worldwide" => ("en", ""),
        _ => return None,
    })
}

/// (language, region) an IGDB alternative name's comment means ("Japanese title",
/// "Chinese title - traditional", ...); romanizations / translations / working titles are
/// skipped (`None`).
pub fn igdb_alt_name_locale(comment: &str) -> Option<(&'static str, &'static str)> {
    let c = comment.to_ascii_lowercase();
    if c.contains("romaniz") || c.contains("romanis") || c.contains("translat") || c.contains("working") || c.contains("acronym") || c.contains("stylized") || c.contains("alternative") || c.contains("abbreviation") {
        return None;
    }
    let lang = |prefix: &str| c.starts_with(prefix);
    Some(if lang("japanese") {
        ("ja", "JP")
    } else if lang("korean") {
        ("kr", "KR")
    } else if lang("chinese") && c.contains("traditional") {
        ("zh-t", "TW")
    } else if lang("chinese") && c.contains("simplified") {
        ("zh-s", "CN")
    } else if lang("chinese") {
        ("zh", "")
    } else if lang("european") {
        ("en", "EU")
    } else if lang("russian") {
        ("ru", "RU")
    } else if lang("german") {
        ("de", "DE")
    } else if lang("french") {
        ("fr", "FR")
    } else if lang("spanish") {
        ("es", "ES")
    } else if lang("italian") {
        ("it", "IT")
    } else if lang("brazilian") || lang("portuguese") {
        ("pt", "BR")
    } else if lang("polish") {
        ("pl", "PL")
    } else {
        return None;
    })
}

/// The IGDB game's localized names onto `title_locales` (source `igdb`): its main name as
/// English, then per-region localizations, then alternative names that name a language.
/// The first name for a (language, region) wins.
/// `locales`: false writes the alternative names only (the user's names are locked).
pub async fn set_igdb_locales(pool: &MySqlPool, names: &IgdbNames, title_id: u64, game: &IgdbGame, locales: bool) -> Result<(), DatabaseError> {
    let mut rows: Vec<(&str, &str, String)> = vec![("en", "", game.name.clone())];
    for (name, region) in game.game_localizations.iter().flatten().filter_map(|id| names.localizations.get(id)) {
        if let Some((lang, reg)) = region.and_then(|r| names.regions.get(&r)).and_then(|ident| igdb_region_locale(ident)) {
            rows.push((lang, reg, name.clone()));
        }
    }
    for (name, comment) in game.alternative_names.iter().flatten().filter_map(|id| names.alternative_names.get(id)) {
        if let Some((lang, reg)) = comment.as_deref().and_then(igdb_alt_name_locale) {
            rows.push((lang, reg, name.clone()));
        }
        // Every alternative name, whatever it stands for (acronym, alternative spelling,
        // romanization, ...): shown on the title page and matched by the title search.
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            sqlx::query("INSERT IGNORE INTO title_alt_names (title_id, name, name_hash, source, comment) VALUES (?, ?, ?, 'igdb', ?)")
                .bind(title_id)
                .bind(trimmed.chars().take(512).collect::<String>())
                .bind(super::super::import::sha1_hex(trimmed))
                .bind(comment.as_deref().map(|c| c.chars().take(128).collect::<String>()))
                .execute(pool)
                .await?;
        }
    }
    for (lang, region, name) in rows.into_iter().filter(|_| locales) {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        sqlx::query("INSERT IGNORE INTO title_locales (title_id, language, region, source, name) VALUES (?, ?, ?, 'igdb', ?)")
            .bind(title_id)
            .bind(lang)
            .bind(region)
            .bind(name.chars().take(512).collect::<String>())
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// The title tables an IGDB game writes into (rows with source `igdb`).
pub const IGDB_TITLE_TABLES: [&str; 7] = ["title_locales", "title_alt_names", "title_genres", "title_perspectives", "title_franchises", "title_companies", "title_keywords"];

/// Writes an IGDB game's genres/franchises/companies/category onto `title_id` (source
/// `igdb`), and points `basegame_title_id` at the local title linked to IGDB's parent game
/// when there is one.
///
/// What an IGDB game wrote before goes first: a title is one IGDB game, and the writes below
/// only add. When a title's link moved to another game (a wrong link put right), the first
/// game's localized names, genres and companies stayed beside the new one's -- the title went
/// on showing the wrong game's Japanese name.
///
/// A title whose localized names the user set (`title.locales_locked`) keeps them: IGDB's
/// are neither cleared nor written.
pub async fn apply_igdb_game(pool: &MySqlPool, names: &IgdbNames, title_id: u64, game: &IgdbGame) -> Result<(), DatabaseError> {
    let locked: bool = sqlx::query_scalar("SELECT locales_locked FROM title WHERE id = ?").bind(title_id).fetch_optional(pool).await?.unwrap_or(false);
    for table in IGDB_TITLE_TABLES {
        if locked && table == "title_locales" {
            continue;
        }
        sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {table} WHERE title_id = ? AND source = 'igdb'"))).bind(title_id).execute(pool).await?;
    }
    // IGDB's list mixes real genres with scope tags ("Indie"): the latter become keywords.
    let (keywords, genres): (Vec<&str>, Vec<&str>) = game.genres.iter().flatten().filter_map(|id| names.genres.get(id).map(String::as_str)).partition(|g| ra_genre::is_not_a_genre(g));
    set_title_genres(pool, title_id, "igdb", &genres).await?;
    set_title_keywords(pool, title_id, "igdb", &keywords).await?;
    let perspectives: Vec<&str> = game.player_perspectives.iter().flatten().filter_map(|id| names.perspectives.get(id).map(String::as_str)).collect();
    set_title_perspectives(pool, title_id, "igdb", &perspectives).await?;
    let franchises: Vec<&str> = game.franchises.iter().flatten().chain(game.franchise.iter()).filter_map(|id| names.franchises.get(id).map(String::as_str)).collect();
    set_title_franchises(pool, title_id, "igdb", &franchises).await?;
    let mut companies: Vec<(&str, &str)> = Vec::new();
    for ic in game.involved_companies.iter().flatten().filter_map(|id| names.involved.get(id)) {
        let Some(name) = names.companies.get(&ic.company) else { continue };
        if ic.developer {
            companies.push((name, ROLE_DEVELOPER));
        }
        if ic.publisher {
            companies.push((name, ROLE_PUBLISHER));
        }
        if ic.porting {
            companies.push((name, ROLE_PORTING));
        }
    }
    set_title_companies(pool, title_id, "igdb", &companies).await?;
    set_igdb_locales(pool, names, title_id, game, !locked).await?;
    let parent = game.parent_game.or(game.version_parent);
    let basegame = match parent {
        Some(pid) => titles_linked(pool, Provider::Igdb, &pid.to_string()).await?.into_iter().find(|t| *t != title_id),
        None => None,
    };
    set_title_category(pool, title_id, igdb_category(game), basegame).await?;
    // The parent's id is kept, so the link can still be made when the parent arrives later
    // (`link_basegames`); and this game may be the parent others have been waiting for.
    sqlx::query("UPDATE title SET basegame_igdb_id = ? WHERE id = ? AND basegame_locked = FALSE").bind(parent.unwrap_or(0)).bind(title_id).execute(pool).await?;
    sqlx::query("UPDATE title SET basegame_title_id = ? WHERE basegame_igdb_id = ? AND basegame_title_id IS NULL AND id <> ? AND deleted = FALSE AND basegame_locked = FALSE").bind(title_id).bind(game.id).bind(title_id).execute(pool).await?;
    Ok(())
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct BasegameSummary {
    /// Titles whose parent IGDB id was read from the cached IGDB game (once per title).
    pub parents_read: u64,
    /// Titles pointed at their base game's title.
    pub linked: u64,
}

/// Points every port, remake, DLC and mod at its base game's title, whichever of the two
/// came into the catalogue first.
///
/// 1. A title IGDB data was applied to before `title.basegame_igdb_id` existed has no
///    parent id yet: it is read from the cached IGDB game, once (`0` when IGDB names none).
/// 2. A title with a parent id is pointed at a title linked to that IGDB game: one that
///    shares a platform with it when there is one (the SNES port's base game is the SNES
///    title, not the Game Boy Advance one); else one on a platform IGDB lists for the base
///    game itself (a port IGDB keeps as a game of its own, linked here to the original's
///    entry anyway, is not on it); else the one on the oldest hardware, since a game is
///    ported to newer machines, not older ones (a remake's base game is the Super Famicom
///    original, not the WonderSwan Color port of it); then the earliest known release, then
///    the lowest id. A pointer that is still right is left alone; one whose title is gone,
///    or that a better title now beats, is replaced.
pub async fn link_basegames(pool: &MySqlPool, progress: &super::super::jobs::Progress) -> Result<BasegameSummary, DatabaseError> {
    let mut summary = BasegameSummary::default();
    let slice = |base: u64, span: u64| progress.set_inner_frame(Some(super::super::jobs::Frame { base, span, whole: 100 }));

    // 1. the parent id, from the cache
    slice(0, 50);
    progress.force(0, None, "base games: finding titles whose parent game is not read yet").await;
    let rows = sqlx::query(
        "SELECT t.id, CAST(tpl.provider_id AS UNSIGNED) AS game_id, ig.raw_response_path FROM title t \
         JOIN title_provider_links tpl ON tpl.title_id = t.id AND tpl.provider = 'igdb' AND tpl.deleted = FALSE AND tpl.provider_id REGEXP '^[0-9]+$' \
         JOIN igdb_games ig ON ig.provider_id = CAST(tpl.provider_id AS UNSIGNED) AND ig.raw_response_path IS NOT NULL \
         WHERE t.deleted = FALSE AND t.basegame_igdb_id IS NULL ORDER BY ig.raw_response_path",
    )
    .fetch_all(pool)
    .await?;
    let total = rows.len() as u64;
    let mut files: HashMap<String, Vec<IgdbGame>> = HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        progress.report(i as u64, Some(total), &format!("base games: reading IGDB's parent game for title {} of {total}", i + 1)).await;
        let (title_id, game_id): (u64, u64) = (r.try_get("id")?, r.try_get("game_id")?);
        let path: String = r.try_get("raw_response_path")?;
        if !files.contains_key(&path) {
            // Keep only the current file's games in memory (rows are ordered by file).
            files.clear();
            let parsed: Vec<IgdbGame> = std::fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .map(|v| match v {
                    serde_json::Value::Array(a) => a.into_iter().filter_map(|g| serde_json::from_value(g).ok()).collect(),
                    other => serde_json::from_value(other).ok().into_iter().collect(),
                })
                .unwrap_or_default();
            files.insert(path.clone(), parsed);
        }
        // A cached file that cannot be read says nothing: the title is asked again next time.
        let Some(game) = files[&path].iter().find(|g| g.id as u64 == game_id) else { continue };
        sqlx::query("UPDATE title SET basegame_igdb_id = ? WHERE id = ? AND basegame_locked = FALSE").bind(game.parent_game.or(game.version_parent).unwrap_or(0)).bind(title_id).execute(pool).await?;
        summary.parents_read += 1;
    }

    // 2. the title linked to that id
    slice(50, 10);
    progress.force(0, None, &format!("base games: finding the titles of each base game ({} parent game(s) read)", summary.parents_read)).await;
    let rows = sqlx::query(
        "SELECT t.id, t.basegame_title_id, b.id AS candidate, CAST(t.basegame_igdb_id AS UNSIGNED) AS base_igdb, \
           EXISTS (SELECT 1 FROM title_releases ta JOIN `release` ra ON ra.id = ta.release_id AND ra.deleted = FALSE \
                   JOIN `release` rb ON rb.app_platform_id = ra.app_platform_id AND rb.deleted = FALSE JOIN title_releases tb ON tb.release_id = rb.id AND tb.title_id = b.id \
                   WHERE ta.title_id = t.id) AS same_platform, \
           (SELECT CAST(GROUP_CONCAT(DISTINCT rc.app_platform_id) AS CHAR) FROM title_releases tc JOIN `release` rc ON rc.id = tc.release_id AND rc.deleted = FALSE AND rc.material IS NULL WHERE tc.title_id = b.id) AS platforms, \
           (SELECT CAST(MIN(COALESCE(YEAR(rc.released_on), rc.year)) AS SIGNED) FROM title_releases tc JOIN `release` rc ON rc.id = tc.release_id AND rc.deleted = FALSE AND rc.material IS NULL WHERE tc.title_id = b.id) AS first_year \
         FROM title t JOIN title_provider_links l ON l.provider = 'igdb' AND l.deleted = FALSE AND l.provider_id = CAST(t.basegame_igdb_id AS CHAR) AND l.title_id <> t.id \
         JOIN title b ON b.id = l.title_id AND b.deleted = FALSE \
         WHERE t.deleted = FALSE AND t.basegame_locked = FALSE AND t.basegame_igdb_id > 0 ORDER BY t.id, b.id",
    )
    .fetch_all(pool)
    .await?;
    // The platforms IGDB lists for each base game (from its cached game; none when it is not
    // cached, and then this says nothing).
    slice(60, 30);
    let mut base_platforms: HashMap<u64, Vec<u16>> = HashMap::new();
    let total = rows.len() as u64;
    for (i, r) in rows.iter().enumerate() {
        progress.report(i as u64, Some(total), &format!("base games: reading the base game's platforms, {} of {total}", i + 1)).await;
        let base: u64 = r.try_get::<Option<u64>, _>("base_igdb")?.unwrap_or(0);
        if base == 0 || base_platforms.contains_key(&base) {
            continue;
        }
        let platforms = super::cache::load_cached_igdb_game(pool, base as i32).await?.and_then(|g| g.platforms).unwrap_or_default();
        base_platforms.insert(base, platforms.into_iter().map(|p| p as u16).collect());
    }
    // Per title, its candidates ranked: same platform, on the base game's IGDB platforms,
    // oldest hardware, earliest release, id.
    let mut ranked: Vec<(u64, Option<u64>, (bool, bool, u16, u32, u64))> = Vec::with_capacity(rows.len());
    for r in &rows {
        let candidate: u64 = r.try_get("candidate")?;
        let platforms = r.try_get::<Option<String>, _>("platforms")?;
        let hardware = basegame_hardware_year(platforms.as_deref());
        let listed = base_platforms.get(&r.try_get::<Option<u64>, _>("base_igdb")?.unwrap_or(0)).filter(|l| !l.is_empty());
        let on_base = listed.is_some_and(|l| on_igdb_platforms(platforms.as_deref(), l));
        let year: Option<i64> = r.try_get("first_year")?;
        let key = (!r.try_get::<bool, _>("same_platform")?, !on_base, hardware.unwrap_or(u16::MAX), year.map(|y| y as u32).unwrap_or(u32::MAX), candidate);
        ranked.push((r.try_get("id")?, r.try_get("basegame_title_id")?, key));
    }
    ranked.sort_by_key(|(title, _, key)| (*title, *key));
    slice(90, 10);
    let total = ranked.len() as u64;
    let mut seen: HashSet<u64> = HashSet::new();
    for (i, (title_id, current, (_, _, _, _, best))) in ranked.into_iter().enumerate() {
        progress.report(i as u64, Some(total), &format!("base games: linking ports, remakes and add-ons to their base game ({} linked)", summary.linked)).await;
        if !seen.insert(title_id) {
            continue;
        }
        if current != Some(best) {
            sqlx::query("UPDATE title SET basegame_title_id = ? WHERE id = ?").bind(best).bind(title_id).execute(pool).await?;
            summary.linked += 1;
        }
    }
    progress.set_inner_frame(None);
    Ok(summary)
}

/// Whether any of a title's platforms (`app_platform_id`s, comma-separated) is one of IGDB's
/// `listed` platform ids, the same machine under another name included (Super Famicom for
/// an SNES title, IGDB's own id for an app-only platform).
fn on_igdb_platforms(platforms: Option<&str>, listed: &[u16]) -> bool {
    platforms.unwrap_or("").split(',').filter_map(|p| p.trim().parse::<u16>().ok()).any(|p| super::cache::sibling_igdb_platforms(p).iter().any(|s| listed.contains(s)))
}

/// The oldest launch year among a title's platforms (`app_platform_id`s, comma-separated).
fn basegame_hardware_year(platforms: Option<&str>) -> Option<u16> {
    platforms?.split(',').filter_map(|p| p.trim().parse::<u16>().ok()).filter_map(crate::common::models::Platform::from_igdb_id).filter_map(|p| p.launch_year()).min()
}

// ---------------------------------------------------------------------------
// other providers
// ---------------------------------------------------------------------------

/// RA's raw components ("2D Platforming", "Sports - Golf") are stored as subgenres; each
/// lands under its major genre through `ensure_genre`.
pub async fn apply_ra_genres(pool: &MySqlPool, title_id: u64, genre_raw: &str) -> Result<(), DatabaseError> {
    let parts: Vec<&str> = genre_raw.split([',', '/', '|']).map(str::trim).filter(|p| !p.is_empty()).collect();
    set_title_genres(pool, title_id, "retroachievements", &parts).await
}

/// MobyGames' genre list spans categories; only "Basic Genres" and "Gameplay" describe
/// what the game is (each lands under its major genre), the rest (Perspective, Setting,
/// Interface, Narrative, ...) are dropped.
pub async fn apply_mobygames_game(pool: &MySqlPool, title_id: u64, game: &MgGame) -> Result<(), DatabaseError> {
    let genres: Vec<&str> = game.genres.iter().flatten().filter(|g| matches!(g.genre_category.trim(), "Basic Genres" | "Gameplay")).map(|g| g.genre_name.as_str()).collect();
    set_title_genres(pool, title_id, "mobygames", &genres).await?;
    let perspectives: Vec<&str> = game.genres.iter().flatten().filter(|g| g.genre_category.trim() == "Perspective").map(|g| g.genre_name.as_str()).collect();
    set_title_perspectives(pool, title_id, "mobygames", &perspectives).await
}

pub async fn apply_screenscraper_game(pool: &MySqlPool, title_id: u64, game: &SsGame) -> Result<(), DatabaseError> {
    let mut companies: Vec<(&str, &str)> = Vec::new();
    if let Some(name) = game.developpeur.as_ref().and_then(|e| e.text.as_deref()) {
        companies.push((name, ROLE_DEVELOPER));
    }
    if let Some(name) = game.editeur.as_ref().and_then(|e| e.text.as_deref()) {
        companies.push((name, ROLE_PUBLISHER));
    }
    set_title_companies(pool, title_id, "screenscraper", &companies).await
}

pub async fn apply_hasheous_publisher(pool: &MySqlPool, title_id: u64, publisher: &str) -> Result<(), DatabaseError> {
    set_title_companies(pool, title_id, "hasheous", &[(publisher, ROLE_PUBLISHER)]).await
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ReapplySummary {
    pub titles: u64,
    pub applied: u64,
    pub missing_raw: u64,
}

async fn apply_batch(clients: &ProviderClients, pool: &MySqlPool, names: &mut IgdbNames, work: Vec<(u64, IgdbGame)>) -> Result<u64, DatabaseError> {
    let games: Vec<IgdbGame> = work.iter().map(|(_, g)| g.clone()).collect();
    names.resolve(clients, &games).await?;
    for (title_id, game) in &work {
        apply_igdb_game(pool, names, *title_id, game).await?;
    }
    Ok(work.len() as u64)
}

/// Re-applies every IGDB-linked title's cached IGDB game (genres, companies, ... and the
/// localized names) without re-fetching the games themselves; only the id -> name lookups
/// (genres, localizations, alternative names, 100 per request) go to IGDB.
///
/// Which titles [`reapply_igdb`] takes.
#[derive(Debug, Clone, Copy)]
pub enum ReapplyScope<'a> {
    /// Those with no IGDB names yet (every applied title gets at least the English one).
    /// Resumable: an interrupted run continues where it stopped.
    Missing,
    /// Every IGDB-linked title, names or not -- after the way data is applied changed.
    All,
    /// These titles, names or not -- to put right names applied from a link that was wrong
    /// at the time.
    Titles(&'a [u64]),
}

pub async fn reapply_igdb(clients: &ProviderClients, pool: &MySqlPool, scope: ReapplyScope<'_>, progress: &super::super::jobs::Progress) -> Result<ReapplySummary, DatabaseError> {
    let only = match scope {
        ReapplyScope::Titles(ids) => Some(ids),
        _ => None,
    };
    let which = match scope {
        ReapplyScope::Titles([]) => return Ok(ReapplySummary::default()),
        ReapplyScope::Titles(ids) => format!("tpl.title_id IN ({})", ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",")),
        ReapplyScope::All => "TRUE".to_string(),
        ReapplyScope::Missing => "NOT t.locales_locked AND NOT EXISTS (SELECT 1 FROM title_locales tlx WHERE tlx.title_id = tpl.title_id AND tlx.source = 'igdb')".to_string(),
    };
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT tpl.title_id, CAST(tpl.provider_id AS UNSIGNED) AS game_id, ig.raw_response_path FROM title_provider_links tpl \
         JOIN title t ON t.id = tpl.title_id AND t.deleted = FALSE \
         LEFT JOIN igdb_games ig ON ig.provider_id = CAST(tpl.provider_id AS UNSIGNED) \
         WHERE tpl.provider = 'igdb' AND tpl.deleted = FALSE AND tpl.provider_id REGEXP '^[0-9]+$' AND {which} ORDER BY ig.raw_response_path, tpl.title_id"
    )))
    .fetch_all(pool)
    .await?;
    let mut s = ReapplySummary { titles: rows.len() as u64, ..Default::default() };
    let total = Some(s.titles);
    let mut names = IgdbNames::new();
    let mut files: HashMap<String, Vec<IgdbGame>> = HashMap::new();
    let mut batch: Vec<(u64, IgdbGame)> = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        progress.report(i as u64, total, &format!("IGDB metadata: title {} of {} ({} applied)", i + 1, rows.len(), s.applied)).await;
        let title_id: u64 = r.try_get("title_id")?;
        let game_id: u64 = r.try_get("game_id")?;
        let Some(path) = r.try_get::<Option<String>, _>("raw_response_path")? else {
            s.missing_raw += 1;
            continue;
        };
        if !files.contains_key(&path) {
            // Keep only the current file's games in memory (rows are ordered by file).
            files.clear();
            let parsed: Vec<IgdbGame> = std::fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .map(|v| match v {
                    serde_json::Value::Array(a) => a.into_iter().filter_map(|g| serde_json::from_value(g).ok()).collect(),
                    other => serde_json::from_value(other).ok().into_iter().collect(),
                })
                .unwrap_or_default();
            files.insert(path.clone(), parsed);
        }
        match files[&path].iter().find(|g| g.id as u64 == game_id) {
            Some(game) => batch.push((title_id, game.clone())),
            None => s.missing_raw += 1,
        }
        if batch.len() >= 200 {
            s.applied += apply_batch(clients, pool, &mut names, std::mem::take(&mut batch)).await?;
        }
    }
    if !batch.is_empty() {
        s.applied += apply_batch(clients, pool, &mut names, std::mem::take(&mut batch)).await?;
    }
    let note = if only.is_some() && (s.titles as usize) < only.map_or(0, <[u64]>::len) { format!(", {} not linked to IGDB", only.map_or(0, <[u64]>::len) - s.titles as usize) } else { String::new() };
    progress.force(s.titles, total, &format!("done: {} title(s) re-applied, {} without a cached IGDB game{note}", s.applied, s.missing_raw)).await;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn igdb_locales_map_to_app_codes() {
        assert_eq!(igdb_region_locale("ja-JP"), Some(("ja", "JP")), "IGDB's actual identifiers");
        assert_eq!(igdb_region_locale("ko-KR"), Some(("kr", "KR")));
        assert_eq!(igdb_region_locale("EU"), Some(("en", "EU")));
        assert_eq!(igdb_region_locale("japan"), Some(("ja", "JP")));
        assert_eq!(igdb_region_locale("asia"), None);
        assert_eq!(igdb_alt_name_locale("Japanese title"), Some(("ja", "JP")));
        assert_eq!(igdb_alt_name_locale("Japanese title - romanization"), None);
        assert_eq!(igdb_alt_name_locale("Japanese title - translated"), None);
        assert_eq!(igdb_alt_name_locale("Chinese title - traditional"), Some(("zh-t", "TW")));
        assert_eq!(igdb_alt_name_locale("European title"), Some(("en", "EU")));
        assert_eq!(igdb_alt_name_locale("Acronym"), None);
    }

    #[test]
    fn other_names_are_the_resolved_localizations_and_alternative_names() {
        let mut g: IgdbGame = serde_json::from_value(serde_json::json!({"id": 1, "name": "Suikoden", "slug": "x", "url": "u", "checksum": "00000000-0000-0000-0000-000000000000", "created_at": 0, "updated_at": 0})).expect("minimal game");
        g.game_localizations = Some(vec![10, 11]);
        g.alternative_names = Some(vec![20]);
        let mut names = IgdbNames::new();
        names.localizations.insert(10, ("幻想水滸伝".to_string(), Some(3)));
        names.alternative_names.insert(20, ("Gensou Suikoden".to_string(), Some("Japanese title - romanization".to_string())));
        assert_eq!(names.other_names(&g), vec!["幻想水滸伝", "Gensou Suikoden"], "an id not resolved (11) is left out");
        let key = crate::database::matching::util::norm_key("Gensou Suikoden");
        assert!(names.other_names(&g).into_iter().any(|n| crate::database::matching::util::norm_key(n) == key));
    }

    #[test]
    fn igdb_codes_map_onto_the_category_enum() {
        let mut g: IgdbGame = serde_json::from_value(serde_json::json!({"id": 1, "name": "x", "slug": "x", "url": "u", "checksum": "00000000-0000-0000-0000-000000000000", "created_at": 0, "updated_at": 0})).expect("minimal game");
        assert_eq!(igdb_category(&g), "base_game");
        g.category = Some(11);
        assert_eq!(igdb_category(&g), "port");
        g.game_type = Some(8);
        assert_eq!(igdb_category(&g), "remake", "game_type wins over the legacy category");
        g.game_type = Some(9);
        assert_eq!(igdb_category(&g), "remaster");
        g.game_type = Some(3);
        assert_eq!(igdb_category(&g), "bundle");
        g.game_type = Some(4);
        assert_eq!(igdb_category(&g), "standalone");
        g.game_type = Some(0);
        g.version_parent = Some(7);
        assert_eq!(igdb_category(&g), "edition", "a main game that is a version of another is an edition");
    }
}
