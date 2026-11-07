//! Collection sets: user-made, ordered lists of releases (a playlist) -- for copying a
//! whole set to another machine or just for grouping. `collection_sets` /
//! `collection_set_releases` in the schema.

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::Row;

use super::catalog::{self, ListQuery, Page, ReleaseRow};
use std::collections::{HashMap, HashSet};

use romexplorer_types::copy::HeaderPreference;

use super::{App, Error, Result};

#[derive(Debug, Clone, Serialize)]
pub struct CollectionSet {
    pub id: u64,
    pub name: String,
    pub description: Option<String>,
    pub release_count: u64,
    /// How many of the set's releases are held locally.
    pub local_releases: u64,
    /// Live titles the set's releases belong to.
    pub title_count: u64,
    /// Platforms the set's releases are for.
    pub platform_count: u64,
    /// What the set takes on disk, in bytes: the files of one local copy of each release --
    /// the copy a copy job takes (`set_sizes`) -- at their real size (a CHD as a CHD).
    pub total_size: u64,
    /// Releases with no local copy here: `total_size` leaves them out.
    pub size_unknown: u64,
    /// The same on the main server: one copy per release, the one a copy takes, at its real
    /// size. `None` with no main server. On a client it is the figure that says how much a
    /// copy of the set will take, since `total_size` there is nothing.
    pub server_size: Option<u64>,
    /// Releases the main server has no copy of: `server_size` leaves them out.
    pub server_size_unknown: u64,
    /// The set by the DATs: each release's raw dump (`catalog::catalog_size_sql`), held or not.
    pub catalog_size: u64,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SetSpec {
    pub name: String,
    pub description: Option<String>,
}

const SELECT: &str = "SELECT s.id, s.name, s.description, s.created_at, s.updated_at, \
    (SELECT COUNT(*) FROM collection_set_releases csr WHERE csr.set_id = s.id) AS release_count, \
    (SELECT COUNT(DISTINCT csr.release_id) FROM collection_set_releases csr JOIN local_release_games lrg ON lrg.release_id = csr.release_id JOIN local_game lg ON lg.id = lrg.game_id AND lg.deleted = FALSE WHERE csr.set_id = s.id) AS local_releases, \
    (SELECT COUNT(DISTINCT tr.title_id) FROM collection_set_releases csr JOIN title_releases tr ON tr.release_id = csr.release_id JOIN title t ON t.id = tr.title_id AND t.deleted = FALSE WHERE csr.set_id = s.id) AS title_count, \
    (SELECT COUNT(DISTINCT r.app_platform_id) FROM collection_set_releases csr JOIN `release` r ON r.id = csr.release_id WHERE csr.set_id = s.id) AS platform_count \
    FROM collection_sets s";

fn row_to_set(row: &sqlx::mysql::MySqlRow) -> Result<CollectionSet> {
    let release_count: i64 = row.try_get("release_count")?;
    let local_releases: i64 = row.try_get("local_releases")?;
    Ok(CollectionSet {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        description: row.try_get("description")?,
        release_count: release_count as u64,
        local_releases: local_releases as u64,
        title_count: row.try_get::<i64, _>("title_count")?.max(0) as u64,
        platform_count: row.try_get::<i64, _>("platform_count")?.max(0) as u64,
        total_size: 0,
        size_unknown: 0,
        server_size: None,
        server_size_unknown: 0,
        catalog_size: 0,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

pub async fn list(app: &App) -> Result<Vec<CollectionSet>> {
    let ids: Vec<u64> = sqlx::query_scalar("SELECT id FROM collection_sets WHERE deleted = FALSE").fetch_all(&app.pool).await?;
    resolve(app, &ids).await?;
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("{SELECT} WHERE s.deleted = FALSE ORDER BY s.name"))).fetch_all(&app.pool).await?;
    let mut sets: Vec<CollectionSet> = rows.iter().map(row_to_set).collect::<Result<_>>()?;
    with_sizes(app, &mut sets).await?;
    Ok(sets)
}

pub async fn get(app: &App, id: u64) -> Result<CollectionSet> {
    resolve(app, &[id]).await?;
    let row = sqlx::query(sqlx::AssertSqlSafe(format!("{SELECT} WHERE s.id = ? AND s.deleted = FALSE"))).bind(id).fetch_optional(&app.pool).await?;
    let mut set = row.as_ref().map(row_to_set).transpose()?.ok_or_else(|| Error::not_found(format!("no collection set {id}")))?;
    with_sizes(app, std::slice::from_mut(&mut set)).await?;
    Ok(set)
}

async fn with_sizes(app: &App, sets: &mut [CollectionSet]) -> Result<()> {
    let sizes = set_sizes(app, &sets.iter().map(|s| s.id).collect::<Vec<_>>()).await?;
    let list = sets.iter().map(|s| s.id.to_string()).collect::<Vec<_>>().join(",");
    let catalog: std::collections::HashMap<u64, u64> = if list.is_empty() {
        Default::default()
    } else {
        sqlx::query_as::<_, (u64, Option<u64>)>(sqlx::AssertSqlSafe(format!(
            "SELECT csr.set_id, CAST(SUM({}) AS UNSIGNED) FROM collection_set_releases csr WHERE csr.set_id IN ({list}) GROUP BY csr.set_id",
            catalog::catalog_size_sql("csr.release_id", "k")
        )))
        .fetch_all(&app.pool)
        .await?
        .into_iter()
        .map(|(id, size)| (id, size.unwrap_or(0)))
        .collect()
    };
    for s in sets {
        if let Some(z) = sizes.get(&s.id) {
            s.total_size = z.on_disk;
            s.size_unknown = z.not_held;
            s.server_size = z.on_server;
            s.server_size_unknown = z.not_on_server;
        }
        s.catalog_size = catalog.get(&s.id).copied().unwrap_or(0);
    }
    Ok(())
}


/// A set's sizes (`set_sizes`).
#[derive(Debug, Clone, Copy, Default)]
pub struct SetSizes {
    pub on_disk: u64,
    pub not_held: u64,
    /// On the main server, when there is one: `None` on a machine with no main server.
    pub on_server: Option<u64>,
    pub not_on_server: u64,
}

/// One file of one copy of a release, as the size count reads it.
struct CopyFile {
    release: u64,
    copy: u64,
    size: u64,
    directory: Option<u64>,
    path: String,
    content: u64,
    position: Option<i64>,
}

/// The size of the copy a copy job takes of each release among `files`: the user's order of
/// the release's copies, then directory priority (`catalog::order_local_games`); in that
/// copy one file per name -- two dumps of one file (a headered and a headerless cartridge)
/// count the one the copy plan takes, by the header preference and then directory priority.
fn copy_sizes(files: &[CopyFile], keys: &HashMap<u64, Vec<i64>>, prefs: &CopyPrefs, headerless: &HashSet<u64>) -> HashMap<u64, u64> {
    let key_of = |dir: Option<u64>| dir.and_then(|d| keys.get(&d).cloned()).unwrap_or_else(|| vec![i64::MAX]);
    // (release, copy) -> its files (name, headerless, directory key, size), its first
    // directory, its position, whether it is an encrypted dump.
    type Copy = (Vec<(String, bool, Vec<i64>, u64)>, Option<u64>, Option<i64>, bool);
    let mut per_copy: HashMap<(u64, u64), Copy> = HashMap::new();
    let mut seen: HashSet<(u64, u64, String)> = HashSet::new();
    for f in files {
        // A file reached through two of the copy's roms counts once.
        if !seen.insert((f.release, f.copy, f.path.clone())) {
            continue;
        }
        let name = f.path.rsplit(['/', '\\']).next().unwrap_or(&f.path).to_string();
        let entry = per_copy.entry((f.release, f.copy)).or_insert_with(|| (Vec::new(), f.directory, f.position, false));
        entry.1 = match (entry.1, f.directory) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        entry.3 |= super::copy::is_encrypted_path(&f.path);
        entry.0.push((name, headerless.contains(&f.content), key_of(f.directory), f.size));
    }
    // (copy, size, first directory key, position, encrypted) per release.
    let mut by_release: HashMap<u64, Vec<(u64, u64, Vec<i64>, Option<i64>, bool)>> = HashMap::new();
    for ((release, copy), (copy_files, dir, position, encrypted)) in per_copy {
        let candidates: Vec<super::copy::Candidate<'_>> = copy_files.iter().map(|(n, h, d, _)| super::copy::Candidate { name: n.as_str(), place: n.clone(), headerless: *h, directory: d.clone() }).collect();
        let keep = super::copy::chosen_files(&candidates, prefs.header, prefs.wiiu, &prefs.order);
        let size = copy_files.iter().zip(keep).filter(|(_, k)| *k).map(|((_, _, _, s), _)| s).sum();
        by_release.entry(release).or_default().push((copy, size, key_of(dir), position, encrypted));
    }
    // The copy a copy job takes: the user's own order, then decrypted before encrypted, then
    // directory priority (`copy::plan`'s holder choice for one machine).
    let mut size_of = HashMap::new();
    for (release, mut held) in by_release {
        held.sort_by(|a, b| {
            let rank = |h: &(u64, u64, Vec<i64>, Option<i64>, bool)| (h.3.is_none(), h.3.unwrap_or(0), prefs.prefer_decrypted && h.4, h.2.clone(), h.0);
            rank(a).cmp(&rank(b))
        });
        if let Some(first) = held.first() {
            size_of.insert(release, first.1);
        }
    }
    size_of
}

/// The copy settings the size count follows (Config > Copy).
struct CopyPrefs {
    header: HeaderPreference,
    wiiu: romexplorer_types::copy::WiiuPreference,
    order: Vec<Vec<String>>,
    prefer_decrypted: bool,
}

fn copy_file(r: &sqlx::mysql::MySqlRow) -> Result<CopyFile> {
    let position: Option<i32> = r.try_get("position")?;
    Ok(CopyFile {
        release: r.try_get("release_id")?,
        copy: r.try_get("game_id")?,
        size: r.try_get("size")?,
        directory: r.try_get("directory_id")?,
        path: r.try_get("path")?,
        content: r.try_get("content_id")?,
        position: position.map(i64::from),
    })
}

/// Each set's size on disk and how many of its releases have no local copy, and the same on
/// the main server (a client holds nothing itself, and the server's figure is what a copy
/// will take). One copy per release, the one a copy job takes, at the size its files really
/// have: the same disc held as an ISO and as a CHD is a different number of bytes, and the
/// DATs know only the first.
pub async fn set_sizes(app: &App, set_ids: &[u64]) -> Result<HashMap<u64, SetSizes>> {
    let mut out: HashMap<u64, SetSizes> = HashMap::new();
    if set_ids.is_empty() {
        return Ok(out);
    }
    let list = set_ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    let members: Vec<(u64, u64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT set_id, release_id FROM collection_set_releases WHERE set_id IN ({list})"))).fetch_all(&app.pool).await?;
    let local: Vec<CopyFile> = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT DISTINCT lrg.release_id, lrg.game_id, f.size, f.directory_id, f.path, f.content_id, \
           (SELECT o.position FROM local_game_order o WHERE o.release_id = lrg.release_id AND o.game_id = lrg.game_id) AS position \
         FROM collection_set_releases csr \
         JOIN local_release_games lrg ON lrg.release_id = csr.release_id JOIN local_game lg ON lg.id = lrg.game_id AND lg.deleted = FALSE \
         JOIN local_game_roms lgr ON lgr.game_id = lg.id JOIN local_rom_files lrf ON lrf.rom_id = lgr.rom_id JOIN local_files f ON f.id = lrf.file_id AND f.deleted = FALSE \
         WHERE csr.set_id IN ({list})"
    )))
    .fetch_all(&app.pool)
    .await?
    .iter()
    .map(copy_file)
    .collect::<Result<_>>()?;
    // The main server's copies, in its own directory order (mirrored with its directories).
    let main: Option<u16> = sqlx::query_scalar("SELECT id FROM remote_sources WHERE role = 'main' AND enabled = TRUE AND deleted = FALSE LIMIT 1").fetch_optional(&app.pool).await?;
    let remote: Vec<CopyFile> = match main {
        Some(source) => sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT DISTINCT rrg.release_id, rrg.game_id, f.size, f.directory_id, f.path, f.content_id, NULL AS position \
             FROM collection_set_releases csr \
             JOIN remote_release_games rrg ON rrg.release_id = csr.release_id AND rrg.source_id = {source} \
             JOIN remote_game g ON g.source_id = rrg.source_id AND g.id = rrg.game_id AND g.deleted = FALSE \
             JOIN remote_game_roms gr ON gr.source_id = g.source_id AND gr.game_id = g.id \
             JOIN remote_rom r ON r.source_id = gr.source_id AND r.id = gr.rom_id AND r.deleted = FALSE \
             JOIN remote_rom_files rf ON rf.source_id = r.source_id AND rf.rom_id = r.id \
             JOIN remote_files f ON f.source_id = rf.source_id AND f.id = rf.file_id AND f.deleted = FALSE \
             WHERE csr.set_id IN ({list})"
        )))
        .fetch_all(&app.pool)
        .await?
        .iter()
        .map(copy_file)
        .collect::<Result<_>>()?,
        None => Vec::new(),
    };
    let preference = super::copy::header_preference(app).await?;
    let prefs = CopyPrefs { header: preference, wiiu: super::copy::wiiu_preference(app).await?, order: super::copy::format_order(app).await?, prefer_decrypted: super::copy::prefer_decrypted(app).await? };
    let headerless = if preference == HeaderPreference::Priority {
        HashSet::new()
    } else {
        super::copy::headerless_contents(app, &local.iter().chain(remote.iter()).map(|f| f.content).collect::<HashSet<_>>().into_iter().collect::<Vec<_>>()).await?
    };
    let local_size = copy_sizes(&local, &crate::database::directories::priority_keys(&app.pool).await?, &prefs, &headerless);
    let server_size = match main {
        Some(source) => Some(copy_sizes(&remote, &crate::database::directories::remote_priority_keys(&app.pool, source).await?, &prefs, &headerless)),
        None => None,
    };
    for (set, release) in members {
        let entry = out.entry(set).or_default();
        match local_size.get(&release) {
            Some(size) => entry.on_disk += size,
            None => entry.not_held += 1,
        }
        if let Some(server) = &server_size {
            let total = entry.on_server.get_or_insert(0);
            match server.get(&release) {
                Some(size) => *total += size,
                None => entry.not_on_server += 1,
            }
        }
    }
    Ok(out)
}

fn check(spec: &SetSpec) -> Result<String> {
    let name = spec.name.trim();
    if name.is_empty() || name.len() > 256 {
        return Err(Error::invalid("set name must be 1-256 characters"));
    }
    Ok(name.to_string())
}

pub async fn create(app: &App, spec: &SetSpec) -> Result<CollectionSet> {
    let name = check(spec)?;
    let result = sqlx::query("INSERT INTO collection_sets (name, description) VALUES (?, ?)").bind(name).bind(spec.description.as_deref().map(str::trim).filter(|d| !d.is_empty())).execute(&app.pool).await?;
    get(app, result.last_insert_id()).await
}

pub async fn update(app: &App, id: u64, spec: &SetSpec) -> Result<CollectionSet> {
    let name = check(spec)?;
    let result = sqlx::query("UPDATE collection_sets SET name = ?, description = ? WHERE id = ? AND deleted = FALSE").bind(name).bind(spec.description.as_deref().map(str::trim).filter(|d| !d.is_empty())).bind(id).execute(&app.pool).await?;
    if result.rows_affected() == 0 {
        return Err(Error::not_found(format!("no collection set {id}")));
    }
    get(app, id).await
}

pub async fn delete(app: &App, id: u64) -> Result<()> {
    let result = sqlx::query("UPDATE collection_sets SET deleted = TRUE WHERE id = ? AND deleted = FALSE").bind(id).execute(&app.pool).await?;
    if result.rows_affected() == 0 {
        return Err(Error::not_found(format!("no collection set {id}")));
    }
    Ok(())
}

/// Appends the release (no-op if already in the set).
pub async fn add_release(app: &App, set_id: u64, release_id: u64) -> Result<CollectionSet> {
    let exists = sqlx::query("SELECT 1 FROM `release` WHERE id = ? AND deleted = FALSE").bind(release_id).fetch_optional(&app.pool).await?;
    if exists.is_none() {
        return Err(Error::not_found(format!("no release {release_id}")));
    }
    add_releases(app, set_id, &[release_id]).await?;
    get(app, set_id).await
}

/// What a set entry is: a release, a title (its best release, held first), or a game -- an
/// IGDB id -- on whichever platform (`resolve`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Release,
    Title,
    Game,
    /// A game's family (Collection > Definitive), by any of its titles: one release of it,
    /// as `copy_definitive` says.
    Family,
}

impl EntryKind {
    fn column(self) -> &'static str {
        match self {
            EntryKind::Release => "release_id",
            EntryKind::Title => "title_id",
            EntryKind::Game => "igdb_id",
            EntryKind::Family => "family_title_id",
        }
    }
}

/// Appends entries of one kind after the set's last (ids the catalogue doesn't have, or that
/// are in the set already, are skipped). Returns how many were added.
pub async fn add_entries(app: &App, set_id: u64, kind: EntryKind, ids: &[String]) -> Result<u64> {
    let live: Option<u64> = sqlx::query_scalar("SELECT id FROM collection_sets WHERE id = ? AND deleted = FALSE").bind(set_id).fetch_optional(&app.pool).await?;
    if live.is_none() {
        return Err(Error::not_found(format!("no collection set {set_id}")));
    }
    if ids.len() > 5000 {
        return Err(Error::invalid("at most 5000 at once"));
    }
    // Only what the catalogue has: a live release, a live title, a game some live title is
    // linked to.
    // A family by its head, so two titles of one family are one entry.
    let heads: Vec<String>;
    let ids: &[String] = if kind == EntryKind::Family {
        let mut seen = HashSet::new();
        let mut list = Vec::new();
        for t in ids.iter().filter_map(|i| i.parse::<u64>().ok()) {
            let head = crate::database::title_family::head_of(&app.pool, t).await?;
            if seen.insert(head) {
                list.push(head.to_string());
            }
        }
        heads = list;
        &heads
    } else {
        ids
    };
    let known = match kind {
        EntryKind::Release => "EXISTS (SELECT 1 FROM `release` x WHERE x.id = ? AND x.deleted = FALSE)",
        // Any row of the title already in the set is the title already there.
        EntryKind::Title => "EXISTS (SELECT 1 FROM title x WHERE x.id = ? AND x.deleted = FALSE) AND NOT EXISTS (SELECT 1 FROM collection_set_entries ex WHERE ex.set_id = ? AND ex.title_id = ?)",
        EntryKind::Game => "EXISTS (SELECT 1 FROM title_provider_links x JOIN title tx ON tx.id = x.title_id AND tx.deleted = FALSE WHERE x.provider = 'igdb' AND x.provider_id = ? AND x.deleted = FALSE)",
        EntryKind::Family => "EXISTS (SELECT 1 FROM title x WHERE x.id = ? AND x.deleted = FALSE)",
    };
    let column = kind.column();
    let mut added = 0;
    for id in ids {
        let q = sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT IGNORE INTO collection_set_entries (set_id, {column}, position) \
             SELECT ?, ?, (SELECT COALESCE(MAX(e.position), 0) + 1 FROM collection_set_entries e WHERE e.set_id = ?) FROM DUAL WHERE {known}"
        )))
        .bind(set_id)
        .bind(id)
        .bind(set_id)
        .bind(id);
        let q = if kind == EntryKind::Title { q.bind(set_id).bind(id) } else { q };
        added += q.execute(&app.pool).await?.rows_affected();
    }
    // Picking a set to add to is what makes it recent: the "Add to set" menus list the three
    // latest first and fold the rest. Bumped even when everything was in the set already.
    sqlx::query("UPDATE collection_sets SET updated_at = CURRENT_TIMESTAMP WHERE id = ?").bind(set_id).execute(&app.pool).await?;
    resolve(app, &[set_id]).await?;
    Ok(added)
}

/// Appends rows of the Titles list (`TitleRow::row_key`): each resolves to the best release of
/// that row only. Unknown titles and malformed row keys are skipped; a row the title has no
/// release in is kept, and resolves to nothing until it has one. Returns how many were added.
pub async fn add_title_rows(app: &App, set_id: u64, rows: &[romexplorer_types::copy::TitleRowRef]) -> Result<u64> {
    let live: Option<u64> = sqlx::query_scalar("SELECT id FROM collection_sets WHERE id = ? AND deleted = FALSE").bind(set_id).fetch_optional(&app.pool).await?;
    if live.is_none() {
        return Err(Error::not_found(format!("no collection set {set_id}")));
    }
    if rows.len() > 5000 {
        return Err(Error::invalid("at most 5000 at once"));
    }
    let mut added = 0;
    for row in rows {
        if crate::database::release_buckets::parse_row_key(&row.row_key).is_none() {
            continue;
        }
        added += sqlx::query(
            "INSERT IGNORE INTO collection_set_entries (set_id, title_id, row_key, position) \
             SELECT ?, ?, ?, (SELECT COALESCE(MAX(e.position), 0) + 1 FROM collection_set_entries e WHERE e.set_id = ?) FROM DUAL WHERE EXISTS (SELECT 1 FROM title x WHERE x.id = ? AND x.deleted = FALSE)",
        )
        .bind(set_id)
        .bind(row.title_id)
        .bind(&row.row_key)
        .bind(set_id)
        .bind(row.title_id)
        .execute(&app.pool)
        .await?
        .rows_affected();
    }
    sqlx::query("UPDATE collection_sets SET updated_at = CURRENT_TIMESTAMP WHERE id = ?").bind(set_id).execute(&app.pool).await?;
    resolve(app, &[set_id]).await?;
    Ok(added)
}

/// Appends several releases (unknown ids are skipped). Returns how many were added.
pub async fn add_releases(app: &App, set_id: u64, release_ids: &[u64]) -> Result<u64> {
    add_entries(app, set_id, EntryKind::Release, &release_ids.iter().map(u64::to_string).collect::<Vec<_>>()).await
}

/// Works out each set's releases from its entries into `collection_set_releases`, which is
/// what lists, sizes and copies read: a release entry is the release; a title its best
/// release, among the ones a copy can be taken of (here or on a server / peer) first, then
/// the rest; a game the same across every title linked to it, `platform_priority` deciding
/// between platforms. Run whenever a set is read or changed, so a better release that came
/// in since is used without adding anything again.
///
/// A set with members and no entries (restored from a backup older than entries, or pulled
/// from a server not yet upgraded) has its members made release entries first.
pub async fn resolve(app: &App, set_ids: &[u64]) -> Result<()> {
    if set_ids.is_empty() {
        return Ok(());
    }
    let list = set_ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT IGNORE INTO collection_set_entries (set_id, release_id, position, added_at) SELECT csr.set_id, csr.release_id, csr.position, csr.added_at FROM collection_set_releases csr \
         WHERE csr.set_id IN ({list}) AND NOT EXISTS (SELECT 1 FROM collection_set_entries e WHERE e.set_id = csr.set_id)"
    )))
    .execute(&app.pool)
    .await?;
    let ranking = catalog::ReleaseRank::load(app).await?;
    let rank = ranking.sql("r");
    let row_key = ranking.row_key("r");
    let held = "(EXISTS (SELECT 1 FROM local_release_games lrh JOIN local_game lgh ON lgh.id = lrh.game_id AND lgh.deleted = FALSE WHERE lrh.release_id = r.id) \
        OR EXISTS (SELECT 1 FROM remote_release_games rrh JOIN remote_game rgh ON rgh.source_id = rrh.source_id AND rgh.id = rrh.game_id AND rgh.deleted = FALSE \
          JOIN remote_sources rsh ON rsh.id = rrh.source_id AND rsh.enabled = TRUE AND rsh.deleted = FALSE WHERE rrh.release_id = r.id))";
    // A title entry from before rows (or from a set file, which names only the title): the
    // row of the release it stands for today, once -- from then on it stays that row.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        // IGNORE: an entry for that row already there keeps the old one as it was.
        "UPDATE IGNORE collection_set_entries e SET e.row_key = COALESCE((SELECT {row_key} FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE \
           WHERE tr.title_id = e.title_id ORDER BY r.material IS NOT NULL, {held} DESC, {rank} LIMIT 1), 'retail') \
         WHERE e.set_id IN ({list}) AND e.title_id IS NOT NULL AND e.row_key = ''"
    )))
    .execute(&app.pool)
    .await?;
    let platforms: Vec<String> = crate::database::settings::platform_priority(&app.pool).await?.iter().map(u16::to_string).collect();
    let platform = if platforms.is_empty() { "0".to_string() } else { format!("COALESCE(NULLIF(FIELD(r.app_platform_id, {}), 0), 65535)", platforms.join(", ")) };
    let mut rows: Vec<(u64, u64, u64, u32)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT x.set_id, x.entry_id, x.release_id, CAST(x.position AS UNSIGNED) FROM (            SELECT e.set_id, e.id AS entry_id, e.position, r.id AS release_id, 1 AS n FROM collection_set_entries e JOIN `release` r ON r.id = e.release_id AND r.deleted = FALSE WHERE e.set_id IN ({list})            UNION ALL            SELECT e.set_id, e.id, e.position, r.id, ROW_NUMBER() OVER (PARTITION BY e.id ORDER BY r.material IS NOT NULL, {held} DESC, {rank})              FROM collection_set_entries e JOIN title_releases tr ON tr.title_id = e.title_id JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE WHERE e.set_id IN ({list}) AND (e.row_key = '' OR {row_key} = e.row_key)            UNION ALL            SELECT e.set_id, e.id, e.position, r.id, ROW_NUMBER() OVER (PARTITION BY e.id ORDER BY r.material IS NOT NULL, {held} DESC, {platform}, {rank})              FROM collection_set_entries e JOIN title_provider_links l ON l.provider = 'igdb' AND l.provider_id = e.igdb_id AND l.deleted = FALSE              JOIN title t ON t.id = l.title_id AND t.deleted = FALSE JOIN title_releases tr ON tr.title_id = t.id JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE WHERE e.set_id IN ({list})          ) x WHERE x.n = 1 ORDER BY x.set_id, x.position, x.entry_id"
    )))
    .fetch_all(&app.pool)
    .await?;
    rows.extend(family_rows(app, &list, &held, &rank, &row_key).await?);
    rows.sort_by_key(|(set, entry, _, position)| (*set, *position, *entry));
    let mut tx = app.pool.begin().await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM collection_set_releases WHERE set_id IN ({list})"))).execute(&mut *tx).await?;
    let mut position: std::collections::HashMap<u64, u32> = std::collections::HashMap::new();
    let mut seen: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
    let mut values: Vec<(u64, u64, u32, u64)> = Vec::new();
    for (set_id, entry_id, release_id, _) in rows {
        // A release two entries come to is listed once, where the first one is. Different
        // releases of one game both count: a title's best release and one added by hand (for
        // RetroAchievements, say) are two things the user wants.
        if seen.insert((set_id, release_id)) {
            let p = position.entry(set_id).or_default();
            *p += 1;
            values.push((set_id, release_id, *p, entry_id));
        }
    }
    for chunk in values.chunks(1000) {
        let mut q = sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT IGNORE INTO collection_set_releases (set_id, release_id, position, entry_id) VALUES {}",
            vec!["(?, ?, ?, ?)"; chunk.len()].join(", ")
        )));
        for v in chunk {
            q = q.bind(v.0).bind(v.1).bind(v.2).bind(v.3);
        }
        q.execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// `copy_definitive`: what a family entry (Collection > Definitive) puts in a set.
/// `best_held` (default): the first version in the family's order (its definitive order, or
/// the user's) that is held here or on a server, its best release; with none held, the
/// definitive version's best release. `definitive_only`: the definitive version's best
/// release when it is held, else nothing of that game.
pub const KEY_COPY_DEFINITIVE: &str = "copy_definitive";
pub const COPY_DEFINITIVE: [&str; 2] = ["best_held", "definitive_only"];

/// The release each family entry of the sets in `list` stands for, as [`resolve`] lists them.
async fn family_rows(app: &App, list: &str, held: &str, rank: &str, row_key: &str) -> Result<Vec<(u64, u64, u64, u32)>> {
    use crate::database::title_family as family;
    let entries: Vec<(u64, u64, u32, u64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT set_id, id, CAST(position AS UNSIGNED), family_title_id FROM collection_set_entries WHERE set_id IN ({list}) AND family_title_id IS NOT NULL"
    )))
    .fetch_all(&app.pool)
    .await?;
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let only_definitive = crate::database::settings::get::<String>(&app.pool, KEY_COPY_DEFINITIVE).await?.as_deref() == Some("definitive_only");
    let mut heads: Vec<u64> = Vec::new();
    for (.., t) in &entries {
        heads.push(family::head_of(&app.pool, *t).await?);
    }
    let members = family::members_of(&app.pool, &heads).await?;
    let priority = crate::database::settings::platform_priority(&app.pool).await?;
    let pc_max_age = family::pc_max_age(&app.pool).await?;
    // A title's best retail release: one held first, then by the release priority.
    let best = format!(
        "SELECT r.id, {held} AS held FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE AND r.material IS NULL \
         WHERE tr.title_id = ? AND {row_key} = 'retail' AND NOT ({}) ORDER BY {held} DESC, {rank} LIMIT 1",
        family::provider_only_sql("r")
    );
    let mut out = Vec::new();
    for ((set_id, entry_id, position, _), head) in entries.into_iter().zip(heads) {
        // Read, not taken: the same family can be in several of the sets.
        let mut order: Vec<u64> = match members.get(&head) {
            Some(m) if !m.is_empty() => family::rank(m.clone(), &priority, pc_max_age).into_iter().map(|m| m.title_id).collect(),
            _ => vec![head],
        };
        // The definitive version alone: a better version you don't hold means nothing.
        if only_definitive {
            order.truncate(1);
        }
        let mut fallback: Option<u64> = None;
        let mut chosen: Option<u64> = None;
        for title in &order {
            let found: Option<(u64, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(best.clone())).bind(title).fetch_optional(&app.pool).await?;
            let Some((release, is_held)) = found else { continue };
            if is_held != 0 {
                chosen = Some(release);
                break;
            }
            fallback.get_or_insert(release);
        }
        let pick = if only_definitive { chosen } else { chosen.or(fallback) };
        if let Some(release) = pick {
            out.push((set_id, entry_id, release, position));
        }
    }
    Ok(out)
}

/// Each title's top-priority release (the one it is shown by: manual order, kind, region /
/// language priority, edition). Titles without releases are left out.
pub async fn top_releases(app: &App, title_ids: &[u64]) -> Result<Vec<u64>> {
    let rank = catalog::ReleaseRank::load(app).await?;
    // A release combined into another stands for nothing on its own: its lead is picked,
    // and a copy takes it with the lead (`release_combine::with_combined`).
    let sql = format!("SELECT r.id FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE WHERE tr.title_id = ? AND NOT {} ORDER BY {} LIMIT 1", super::release_combine::is_combined_sql("r"), rank.sql("r"));
    let mut out = Vec::new();
    for t in title_ids {
        if let Some(id) = sqlx::query_scalar::<_, u64>(sqlx::AssertSqlSafe(sql.clone())).bind(t).fetch_optional(&app.pool).await? {
            out.push(id);
        }
    }
    Ok(out)
}

/// The release each RetroAchievements game stands for in a set: one per game, as a title
/// entry resolves -- of the releases linked to it (exact or manual links, as the
/// RetroAchievements tab counts "local"), the best-ranked one you hold, else the best-ranked
/// one, else its title's top-priority release. A game the catalogue has nothing of adds
/// nothing.
///
/// One, not every one you hold: a RetroAchievements game is linked to each hash it accepts --
/// the USA, European and Japanese releases, each revision -- and adding all of those made a
/// copy of the set take every region of the game.
pub async fn ra_game_releases(app: &App, ra_game_ids: &[u64]) -> Result<Vec<u64>> {
    let rank = catalog::ReleaseRank::load(app).await?;
    let linked = "rpl.provider = 'retroachievements' AND rpl.provider_id = ? AND rpl.deleted = FALSE AND rpl.confidence IN ('exact', 'manual')";
    let held = "EXISTS (SELECT 1 FROM local_release_games lrg JOIN local_game lg ON lg.id = lrg.game_id AND lg.deleted = FALSE WHERE lrg.release_id = r.id)";
    let best_sql = format!("SELECT r.id FROM release_provider_links rpl JOIN `release` r ON r.id = rpl.release_id AND r.deleted = FALSE WHERE {linked} ORDER BY {held} DESC, {}, r.id LIMIT 1", rank.sql("r"));
    let mut out = Vec::new();
    for id in ra_game_ids {
        let id = id.to_string();
        if let Some(best) = sqlx::query_scalar::<_, u64>(sqlx::AssertSqlSafe(best_sql.clone())).bind(&id).fetch_optional(&app.pool).await? {
            out.push(best);
            continue;
        }
        let titles: Vec<u64> = sqlx::query_scalar("SELECT title_id FROM title_provider_links WHERE provider = 'retroachievements' AND provider_id = ? AND deleted = FALSE ORDER BY title_id").bind(&id).fetch_all(&app.pool).await?;
        out.extend(top_releases(app, &titles).await?);
    }
    out.dedup();
    Ok(out)
}

pub async fn remove_release(app: &App, set_id: u64, release_id: u64) -> Result<CollectionSet> {
    if remove_releases(app, set_id, &[release_id]).await? == 0 {
        return Err(Error::not_found(format!("release {release_id} is not in set {set_id}")));
    }
    get(app, set_id).await
}

/// Takes `release_ids` out of the set (the selection of a filtered set list): the entries
/// they came from -- a title or a game entry goes with the release it gave. Ids not in the
/// set are ignored. Returns how many entries went.
pub async fn remove_releases(app: &App, set_id: u64, release_ids: &[u64]) -> Result<u64> {
    get(app, set_id).await?;
    let mut removed = 0;
    for part in release_ids.chunks(1000) {
        let list = part.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        let entries: Vec<u64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT DISTINCT entry_id FROM collection_set_releases WHERE set_id = ? AND release_id IN ({list}) AND entry_id IS NOT NULL \
             UNION SELECT id FROM collection_set_entries WHERE set_id = ? AND release_id IN ({list})"
        )))
        .bind(set_id)
        .bind(set_id)
        .fetch_all(&app.pool)
        .await?;
        if entries.is_empty() {
            continue;
        }
        let ids = entries.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        removed += sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM collection_set_entries WHERE set_id = ? AND id IN ({ids})"))).bind(set_id).execute(&app.pool).await?.rows_affected();
    }
    resolve(app, &[set_id]).await?;
    Ok(removed)
}

/// Rewrites the order of the set's entries to the order of `release_ids` (the releases the
/// set page shows; each stands for the entry it came from); entries not mentioned keep their
/// relative order after the listed ones.
pub async fn reorder(app: &App, set_id: u64, release_ids: &[u64]) -> Result<CollectionSet> {
    get(app, set_id).await?;
    let came_from: std::collections::HashMap<u64, u64> = sqlx::query_as::<_, (u64, Option<u64>)>("SELECT release_id, entry_id FROM collection_set_releases WHERE set_id = ?")
        .bind(set_id)
        .fetch_all(&app.pool)
        .await?
        .into_iter()
        .filter_map(|(r, e)| e.map(|e| (r, e)))
        .collect();
    let current: Vec<u64> = sqlx::query_scalar("SELECT id FROM collection_set_entries WHERE set_id = ? ORDER BY position, id").bind(set_id).fetch_all(&app.pool).await?;
    let mut order: Vec<u64> = Vec::new();
    for e in release_ids.iter().filter_map(|r| came_from.get(r)).chain(current.iter()) {
        if !order.contains(e) {
            order.push(*e);
        }
    }
    let mut tx = app.pool.begin().await?;
    for (i, id) in order.iter().enumerate() {
        sqlx::query("UPDATE collection_set_entries SET position = ? WHERE set_id = ? AND id = ?").bind((i + 1) as u32).bind(set_id).bind(id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    get(app, set_id).await
}

/// Swaps the places of the entries releases `a` and `b` came from; nothing else moves.
pub async fn swap(app: &App, set_id: u64, a: u64, b: u64) -> Result<CollectionSet> {
    get(app, set_id).await?;
    let entry_of = |release: u64| {
        sqlx::query_scalar::<_, Option<u64>>("SELECT entry_id FROM collection_set_releases WHERE set_id = ? AND release_id = ?").bind(set_id).bind(release).fetch_optional(&app.pool)
    };
    let (Some(Some(ea)), Some(Some(eb))) = (entry_of(a).await?, entry_of(b).await?) else {
        return Err(Error::invalid("both releases must be in the set"));
    };
    if ea == eb {
        return get(app, set_id).await;
    }
    // Numbered first, so two entries that share a position (never reordered) still swap.
    let current: Vec<u64> = sqlx::query_scalar("SELECT id FROM collection_set_entries WHERE set_id = ? ORDER BY position, id").bind(set_id).fetch_all(&app.pool).await?;
    let mut order = current;
    let (Some(i), Some(j)) = (order.iter().position(|e| *e == ea), order.iter().position(|e| *e == eb)) else {
        return Err(Error::invalid("both releases must be in the set"));
    };
    order.swap(i, j);
    let mut tx = app.pool.begin().await?;
    for (n, id) in order.iter().enumerate() {
        sqlx::query("UPDATE collection_set_entries SET position = ? WHERE set_id = ? AND id = ?").bind((n + 1) as u32).bind(set_id).bind(id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    get(app, set_id).await
}

/// The set's releases, in set order unless another sort is asked for.
pub async fn releases(app: &App, set_id: u64, q: &ListQuery) -> Result<Page<ReleaseRow>> {
    get(app, set_id).await?;
    let mut q = q.clone();
    q.set_id = Some(set_id);
    if q.sort.is_none() {
        q.sort = Some("position".to_string());
    }
    catalog::releases(app, &q).await
}
