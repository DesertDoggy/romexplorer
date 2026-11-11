//! The phone's synced copy: a SQLite file the mobile app reads instead of asking for every
//! screen.
//!
//! **The file holds responses, not tables.** The catalogue's lists are thousands of lines of
//! MariaDB SQL (`catalog`, `definitive`, `ra`); copying the tables and porting that SQL to
//! SQLite would be a second implementation to keep in step forever. So this calls the very
//! functions the REST routes call and stores each row as the JSON the route would have sent,
//! beside the few columns the phone filters and sorts on (name, platforms, where it is held,
//! its title, its IGDB game). The phone answers the same route strings from it
//! (`romexplorer-gui`'s offline backend), and every screen renders unchanged.
//!
//! **It is bounded by what the phone asks for** ([`SyncOptions`]): which machines count as
//! holding something, whether the catalogue's unheld titles come too (then only for the
//! platforms picked), which tabs, whether full pages and covers come. Pages are stored for
//! held items only; the rest load live when the phone is connected. Covers are made into
//! thumbnails here, never on the phone.
//!
//! Format version [`FORMAT`]; the phone resyncs on a mismatch rather than migrating.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use serde::Serialize;
use serde_json::Value;
use sqlx::Row;

use super::catalog::{self, ListQuery};
use super::{App, Error, Result};
use crate::database::jobs::Progress;

pub use romexplorer_types::mobile::{
    Machine, SyncEstimate, SyncOptions, SyncSummary, SyncTabs, FORMAT, THIS_MACHINE,
};

/// Rows per call into the list functions: their own page cap.
const PAGE: u32 = 500;

/// The machines a phone can count holdings on: this one, then each enabled remote source.
pub async fn machines(app: &App) -> Result<Vec<Machine>> {
    let here: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM local_game WHERE deleted = FALSE")
        .fetch_one(&app.pool)
        .await?;
    let mut out = vec![Machine {
        id: THIS_MACHINE.to_string(),
        name: String::new(),
        role: "this".to_string(),
        games: here as u64,
    }];
    let rows = sqlx::query(
        "SELECT rs.id, rs.name, CAST(rs.role AS CHAR) AS role, (SELECT COUNT(*) FROM remote_game g WHERE g.source_id = rs.id AND g.deleted = FALSE) AS games \
         FROM remote_sources rs WHERE rs.enabled = TRUE AND rs.deleted = FALSE ORDER BY rs.role = 'main' DESC, rs.name",
    )
    .fetch_all(&app.pool)
    .await?;
    for r in rows {
        let id: u16 = r.try_get("id")?;
        let games: i64 = r.try_get("games")?;
        out.push(Machine {
            id: id.to_string(),
            name: r.try_get("name")?,
            role: r.try_get("role")?,
            games: games as u64,
        });
    }
    Ok(out)
}

/// Checks a phone's options before anything runs, so a typo is a 400 and not a failed job.
pub fn validate(o: &SyncOptions) -> Result<()> {
    if o.machines.is_empty() {
        return Err(Error::invalid("choose at least one machine"));
    }
    for m in &o.machines {
        if m != THIS_MACHINE && m.parse::<u16>().is_err() {
            return Err(Error::invalid(format!(
                "machine {m:?} must be {THIS_MACHINE:?} or a remote source id"
            )));
        }
    }
    if !matches!(o.scope.as_str(), "held" | "catalog") {
        return Err(Error::invalid("scope must be held or catalog"));
    }
    if !matches!(o.cover_format.as_str(), "avif" | "jpeg") {
        return Err(Error::invalid("cover_format must be avif or jpeg"));
    }
    if !(64..=2048).contains(&o.cover_px) {
        return Err(Error::invalid("cover_px must be 64 to 2048"));
    }
    Ok(())
}

/// What is in reach: the releases and titles the file covers, and who holds which.
#[derive(Default)]
struct Scope {
    /// Release -> the chosen machines holding it.
    held: HashMap<u64, BTreeSet<String>>,
    /// Title -> the chosen machines holding one of its releases.
    held_titles: HashMap<u64, BTreeSet<String>>,
    releases: HashSet<u64>,
    titles: HashSet<u64>,
    /// Title -> its releases' platforms.
    title_platforms: HashMap<u64, BTreeSet<u16>>,
    release_platform: HashMap<u64, u16>,
    release_title: HashMap<u64, u64>,
}

impl Scope {
    fn platforms(&self) -> BTreeSet<u16> {
        self.release_platform.values().copied().collect()
    }

    fn held_text(set: Option<&BTreeSet<String>>) -> String {
        match set {
            Some(s) if !s.is_empty() => {
                format!(",{},", s.iter().cloned().collect::<Vec<_>>().join(","))
            }
            _ => String::new(),
        }
    }
}

fn id_list(ids: &[u64]) -> String {
    ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",")
}

async fn scope(app: &App, o: &SyncOptions) -> Result<Scope> {
    let mut s = Scope::default();
    let add_held = |s: &mut Scope, release: u64, machine: &str| {
        s.held
            .entry(release)
            .or_default()
            .insert(machine.to_string());
        s.releases.insert(release);
    };
    if o.machines.iter().any(|m| m == THIS_MACHINE) {
        let ids: Vec<u64> = sqlx::query_scalar(
            "SELECT DISTINCT lrg.release_id FROM local_release_games lrg JOIN local_game lg ON lg.id = lrg.game_id AND lg.deleted = FALSE",
        )
        .fetch_all(&app.pool)
        .await?;
        for id in ids {
            add_held(&mut s, id, THIS_MACHINE);
        }
    }
    let sources: Vec<u16> = o.machines.iter().filter_map(|m| m.parse().ok()).collect();
    if !sources.is_empty() {
        let list = sources
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(",");
        // Ids parsed as integers above: safe to inline.
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT DISTINCT rrg.release_id, rrg.source_id FROM remote_release_games rrg \
             JOIN remote_game rg ON rg.source_id = rrg.source_id AND rg.id = rrg.game_id AND rg.deleted = FALSE \
             WHERE rrg.source_id IN ({list})"
        )))
        .fetch_all(&app.pool)
        .await?;
        for r in rows {
            let release: u64 = r.try_get(0)?;
            let source: u16 = r.try_get(1)?;
            add_held(&mut s, release, &source.to_string());
        }
    }

    // The held releases' titles and platforms.
    let held: Vec<u64> = s.releases.iter().copied().collect();
    for chunk in held.chunks(5000) {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT r.id, r.app_platform_id, tr.title_id FROM `release` r LEFT JOIN title_releases tr ON tr.release_id = r.id \
             WHERE r.deleted = FALSE AND r.id IN ({})",
            id_list(chunk)
        )))
        .fetch_all(&app.pool)
        .await?;
        for r in rows {
            let id: u64 = r.try_get(0)?;
            let platform: u16 = r.try_get(1)?;
            s.release_platform.insert(id, platform);
            if let Some(title) = r.try_get::<Option<u64>, _>(2)? {
                s.release_title.insert(id, title);
                s.titles.insert(title);
                let machines = s.held.get(&id).cloned().unwrap_or_default();
                s.held_titles.entry(title).or_default().extend(machines);
            }
        }
    }
    // A release deleted since it was matched is not in reach.
    s.releases.retain(|r| s.release_platform.contains_key(r));

    // The catalogue's unheld titles and releases, for the platforms picked.
    if o.scope == "catalog" && !o.catalog_platforms.is_empty() {
        let list = o
            .catalog_platforms
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT r.id, r.app_platform_id, tr.title_id FROM `release` r JOIN title_releases tr ON tr.release_id = r.id \
             JOIN title t ON t.id = tr.title_id AND t.deleted = FALSE WHERE r.deleted = FALSE AND r.app_platform_id IN ({list})"
        )))
        .fetch_all(&app.pool)
        .await?;
        for r in rows {
            let id: u64 = r.try_get(0)?;
            let platform: u16 = r.try_get(1)?;
            let title: u64 = r.try_get(2)?;
            s.releases.insert(id);
            s.release_platform.insert(id, platform);
            s.release_title.insert(id, title);
            s.titles.insert(title);
        }
    }

    // Every scope title's releases and platforms: a held title's page lists its other
    // releases too, and its platform filter goes by all of them.
    let titles: Vec<u64> = s.titles.iter().copied().collect();
    for chunk in titles.chunks(5000) {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT tr.title_id, r.app_platform_id, r.id FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE \
             WHERE tr.title_id IN ({})",
            id_list(chunk)
        )))
        .fetch_all(&app.pool)
        .await?;
        for r in rows {
            let title: u64 = r.try_get(0)?;
            let platform: u16 = r.try_get(1)?;
            let release: u64 = r.try_get(2)?;
            s.title_platforms.entry(title).or_default().insert(platform);
            s.releases.insert(release);
            s.release_platform.insert(release, platform);
            s.release_title.entry(release).or_insert(title);
        }
    }
    Ok(s)
}

/// Rough bytes per stored item, measured on the JSON the routes send (a title row is about
/// 1.2 KB, a title page with its releases 6-10 KB, a 512 px AVIF cover 15-30 KB).
const ROW_BYTES: u64 = 1_400;
const PAGE_BYTES: u64 = 9_000;
const COVER_BYTES: u64 = 22_000;

/// What a sync with `o` would carry.
pub async fn estimate(app: &App, o: &SyncOptions) -> Result<SyncEstimate> {
    validate(o)?;
    let s = scope(app, o).await?;
    let t = &o.tabs;
    let titles = if t.titles || t.crossplatform || t.definitive {
        s.titles.len() as u64
    } else {
        0
    };
    let releases = if t.releases {
        s.releases.len() as u64
    } else {
        0
    };
    let pages = if o.pages {
        (s.held_titles.len() + s.held.len()) as u64
    } else {
        0
    };
    let ra_games = if t.ra {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM ra_game")
            .fetch_one(&app.pool)
            .await
            .unwrap_or(0)
            .max(0) as u64
    } else {
        0
    };
    let local = o.machines.iter().any(|m| m == THIS_MACHINE);
    let games = if t.games && local {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM local_game WHERE deleted = FALSE")
            .fetch_one(&app.pool)
            .await?
            .max(0) as u64
    } else {
        0
    };
    let files = if t.files && local {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM local_files")
            .fetch_one(&app.pool)
            .await
            .unwrap_or(0)
            .max(0) as u64
    } else {
        0
    };
    // About one cover per title and per release; a release usually shares its title's art.
    let covers = if o.covers { titles + releases / 2 } else { 0 };
    let rows = titles * u64::from(t.titles)
        + titles * u64::from(t.crossplatform) / 2
        + releases
        + ra_games
        + games
        + files;
    let bytes = rows * ROW_BYTES + pages * PAGE_BYTES + covers * COVER_BYTES;
    Ok(SyncEstimate {
        titles,
        releases,
        pages,
        covers,
        ra_games,
        games,
        files,
        bytes,
    })
}

/// The tables the phone reads. `rows.held` and `rows.platforms` are `,a,b,` lists so a
/// `LIKE '%,x,%'` finds one member; `ord` is the order the route itself returned them in.
const SCHEMA: &[&str] = &[
    "CREATE TABLE _mobile_meta (key TEXT PRIMARY KEY, value TEXT)",
    "CREATE TABLE _mobile_fixed (path TEXT PRIMARY KEY, body TEXT NOT NULL)",
    "CREATE TABLE _mobile_rows (tab TEXT NOT NULL, key TEXT NOT NULL, ord INTEGER NOT NULL, id INTEGER, title_id INTEGER, igdb TEXT, platforms TEXT NOT NULL DEFAULT '', name TEXT NOT NULL DEFAULT '', held TEXT NOT NULL DEFAULT '', body TEXT NOT NULL, PRIMARY KEY (tab, key))",
    "CREATE INDEX _mobile_rows_ord ON _mobile_rows (tab, ord)",
    "CREATE INDEX _mobile_rows_title ON _mobile_rows (tab, title_id)",
    "CREATE INDEX _mobile_rows_igdb ON _mobile_rows (tab, igdb)",
    "CREATE TABLE _mobile_pages (kind TEXT NOT NULL, id INTEGER NOT NULL, body TEXT NOT NULL, PRIMARY KEY (kind, id))",
    "CREATE TABLE _mobile_images (key TEXT PRIMARY KEY, mime TEXT NOT NULL, bytes BLOB NOT NULL)",
    "CREATE TABLE _mobile_ra (game_id INTEGER PRIMARY KEY, ord INTEGER NOT NULL, title TEXT NOT NULL DEFAULT '', console_id INTEGER, local INTEGER NOT NULL DEFAULT 0, num_awarded INTEGER NOT NULL DEFAULT 0, highest_award_kind TEXT NOT NULL DEFAULT '', most_recent_awarded TEXT, body TEXT NOT NULL)",
];

struct Out {
    db: sqlx::SqlitePool,
    rows: Vec<(String, u64)>,
}

impl Out {
    async fn page(&self, kind: &str, id: u64, body: &impl Serialize) -> Result<()> {
        let text = serde_json::to_string(body).map_err(|e| Error::internal(e.to_string()))?;
        sqlx::query("INSERT OR REPLACE INTO _mobile_pages (kind, id, body) VALUES (?, ?, ?)")
            .bind(kind)
            .bind(id as i64)
            .bind(text)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    fn count(&mut self, tab: &str, n: u64) {
        match self.rows.iter_mut().find(|(t, _)| t == tab) {
            Some((_, c)) => *c += n,
            None => self.rows.push((tab.to_string(), n)),
        }
    }
}

/// One stored list row.
struct RowIn {
    key: String,
    id: Option<u64>,
    title_id: Option<u64>,
    igdb: Option<String>,
    platforms: String,
    name: String,
    held: String,
    body: Value,
}

async fn insert_rows(out: &mut Out, tab: &str, ord: &mut i64, rows: Vec<RowIn>) -> Result<()> {
    let n = rows.len() as u64;
    let mut tx = out.db.begin().await?;
    for r in rows {
        *ord += 1;
        sqlx::query("INSERT OR REPLACE INTO _mobile_rows (tab, key, ord, id, title_id, igdb, platforms, name, held, body) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(tab)
            .bind(&r.key)
            .bind(*ord)
            .bind(r.id.map(|v| v as i64))
            .bind(r.title_id.map(|v| v as i64))
            .bind(&r.igdb)
            .bind(&r.platforms)
            .bind(r.name.to_lowercase())
            .bind(&r.held)
            .bind(r.body.to_string())
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    out.count(tab, n);
    Ok(())
}

fn set_text(set: Option<&BTreeSet<u16>>) -> String {
    match set {
        Some(s) if !s.is_empty() => format!(
            ",{},",
            s.iter().map(u16::to_string).collect::<Vec<_>>().join(",")
        ),
        _ => String::new(),
    }
}

fn to_value(v: &impl Serialize) -> Result<Value> {
    serde_json::to_value(v).map_err(|e| Error::internal(e.to_string()))
}

fn su(v: &Value, k: &str) -> Option<u64> {
    v.get(k).and_then(Value::as_u64)
}

fn ss(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Every `cover_media_id` in `v`, however deep: the covers the stored rows and pages show.
fn covers_in(v: &Value, out: &mut BTreeSet<u64>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                if k == "cover_media_id" {
                    if let Some(id) = x.as_u64() {
                        out.insert(id);
                    }
                } else {
                    covers_in(x, out);
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| covers_in(x, out)),
        _ => {}
    }
}

/// The list query a sync pages through: everything, the default order, on `platforms`.
fn list_query(platforms: &BTreeSet<u16>, offset: u32) -> ListQuery {
    ListQuery {
        view: Some("collection".to_string()),
        platforms: Some(
            platforms
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(","),
        ),
        limit: Some(PAGE),
        offset: Some(offset),
        ..Default::default()
    }
}

/// Builds the phone's file at `path` for `o`.
pub async fn export(
    app: &App,
    o: &SyncOptions,
    path: &Path,
    progress: &Progress,
) -> Result<SyncSummary> {
    validate(o)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::internal(format!("creating {}: {e}", parent.display())))?;
    }
    let part = path.with_file_name(format!(
        "{}.part",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    let _ = std::fs::remove_file(&part);
    struct PartGuard(Option<std::path::PathBuf>);
    impl Drop for PartGuard {
        fn drop(&mut self) {
            if let Some(path) = self.0.take() {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    let mut guard = PartGuard(Some(part.clone()));
    let db = sqlx::SqlitePool::connect(&format!("sqlite://{}?mode=rwc", part.display()))
        .await
        .map_err(|e| Error::internal(format!("opening {}: {e}", part.display())))?;
    sqlx::query("PRAGMA journal_mode = OFF")
        .execute(&db)
        .await
        .ok();
    sqlx::query("PRAGMA synchronous = OFF")
        .execute(&db)
        .await
        .ok();
    for stmt in SCHEMA {
        sqlx::query(*stmt).execute(&db).await?;
    }
    let mut out = Out {
        db,
        rows: Vec::new(),
    };

    // Steps, for one bar that only goes up: scope, lists, pages, RA, covers, tables.
    const STEPS: u64 = 7;
    progress
        .force(
            0,
            Some(STEPS),
            "working out what the phone gets (step 1 of 7)",
        )
        .await;
    let s = scope(app, o).await?;
    let instance = crate::database::settings::instance_id(&app.pool).await?;
    let meta = [
        ("format", FORMAT.to_string()),
        ("instance_id", instance),
        (
            "exported_at",
            chrono::Local::now().naive_local().to_string(),
        ),
        (
            "schema_version",
            crate::database::schema_version().to_string(),
        ),
        ("options", serde_json::to_string(o).unwrap_or_default()),
        (
            "machines",
            serde_json::to_string(&machines(app).await?).unwrap_or_default(),
        ),
    ];
    for (k, v) in &meta {
        sqlx::query("INSERT INTO _mobile_meta (key, value) VALUES (?, ?)")
            .bind(*k)
            .bind(v)
            .execute(&out.db)
            .await?;
    }
    // The provenance every cache of ours carries (`sqlite::export`), so `sqlite::import`
    // treats this file like any other: a full database can take its tables back in.
    let meta_table = super::sqlite::META_TABLE;
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE TABLE \"{meta_table}\" (instance_id TEXT, exported_at TEXT, schema_version INTEGER)"))).execute(&out.db).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO \"{meta_table}\" (instance_id, exported_at, schema_version) VALUES (?, ?, ?)"
    )))
    .bind(&meta[1].1)
    .bind(&meta[2].1)
    .bind(crate::database::schema_version())
    .execute(&out.db)
    .await?;

    // What every screen needs before it draws (vocabulary, settings, translations) the
    // phone fetches over REST itself as part of the sync and keeps in `fixed`: they are the
    // routes' own answers, so it asks the routes.

    let platforms = s.platforms();
    let tabs = &o.tabs;
    let held_title = |id: u64| Scope::held_text(s.held_titles.get(&id));
    let mut covers = BTreeSet::new();

    // --- lists ---
    if !platforms.is_empty() {
        if tabs.titles {
            let mut ord = 0;
            let mut offset = 0;
            loop {
                if progress.cancelled().await {
                    return Err(Error::invalid("cancelled"));
                }
                let page = catalog::titles(app, &list_query(&platforms, offset)).await?;
                progress
                    .force(
                        1,
                        Some(STEPS),
                        &format!(
                            "titles: {} of {} read (step 2 of 7)",
                            (offset as u64 + page.items.len() as u64).min(page.total),
                            page.total
                        ),
                    )
                    .await;
                let n = page.items.len() as u32;
                let mut rows = Vec::new();
                for t in page.items.iter().filter(|t| s.titles.contains(&t.id)) {
                    let body = to_value(t)?;
                    covers_in(&body, &mut covers);
                    rows.push(RowIn {
                        key: t.row_key.clone(),
                        id: Some(t.id),
                        title_id: Some(t.id),
                        igdb: None,
                        platforms: set_text(s.title_platforms.get(&t.id)),
                        name: format!("{} {}", t.name, t.display_name),
                        held: held_title(t.id),
                        body,
                    });
                }
                insert_rows(&mut out, "titles", &mut ord, rows).await?;
                offset += n;
                if n < PAGE || offset as u64 >= page.total {
                    break;
                }
            }
            // `titles?igdb=` (a cross-platform row's titles) is answered from these rows.
            let ids: Vec<u64> = s.titles.iter().copied().collect();
            for chunk in ids.chunks(5000) {
                let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
                    "SELECT title_id, provider_id FROM title_provider_links WHERE provider = 'igdb' AND deleted = FALSE AND title_id IN ({})",
                    id_list(chunk)
                )))
                .fetch_all(&app.pool)
                .await?;
                for r in rows {
                    let title: u64 = r.try_get(0)?;
                    let igdb: String = r.try_get(1)?;
                    sqlx::query(
                        "UPDATE _mobile_rows SET igdb = ? WHERE tab = 'titles' AND title_id = ?",
                    )
                    .bind(igdb)
                    .bind(title as i64)
                    .execute(&out.db)
                    .await?;
                }
            }
        }
        if tabs.releases {
            let mut ord = 0;
            let mut offset = 0;
            loop {
                if progress.cancelled().await {
                    return Err(Error::invalid("cancelled"));
                }
                let page = catalog::releases(app, &list_query(&platforms, offset)).await?;
                progress
                    .force(
                        1,
                        Some(STEPS),
                        &format!(
                            "releases: {} of {} read (step 2 of 7)",
                            (offset as u64 + page.items.len() as u64).min(page.total),
                            page.total
                        ),
                    )
                    .await;
                let n = page.items.len() as u32;
                let mut rows = Vec::new();
                for r in page.items.iter().filter(|r| s.releases.contains(&r.id)) {
                    let body = to_value(r)?;
                    covers_in(&body, &mut covers);
                    let plat: BTreeSet<u16> = [r.platform_id].into_iter().collect();
                    rows.push(RowIn {
                        key: r.id.to_string(),
                        id: Some(r.id),
                        title_id: r.title_id,
                        igdb: None,
                        platforms: set_text(Some(&plat)),
                        name: format!("{} {}", r.name, r.display_name),
                        held: Scope::held_text(s.held.get(&r.id)),
                        body,
                    });
                }
                insert_rows(&mut out, "releases", &mut ord, rows).await?;
                offset += n;
                if n < PAGE || offset as u64 >= page.total {
                    break;
                }
            }
        }
        if tabs.crossplatform {
            let mut ord = 0;
            let mut offset = 0;
            loop {
                if progress.cancelled().await {
                    return Err(Error::invalid("cancelled"));
                }
                let page = catalog::crossplatform(app, &list_query(&platforms, offset)).await?;
                progress
                    .force(
                        1,
                        Some(STEPS),
                        &format!(
                            "cross-platform: {} of {} read (step 2 of 7)",
                            (offset as u64 + page.items.len() as u64).min(page.total),
                            page.total
                        ),
                    )
                    .await;
                let n = page.items.len() as u32;
                let mut rows = Vec::new();
                for c in &page.items {
                    let ids: Vec<u64> = c
                        .titles
                        .iter()
                        .map(|t| t.id)
                        .filter(|id| s.titles.contains(id))
                        .collect();
                    if ids.is_empty() {
                        continue;
                    }
                    let mut plats = BTreeSet::new();
                    let mut held = BTreeSet::new();
                    for id in &ids {
                        plats.extend(s.title_platforms.get(id).into_iter().flatten().copied());
                        held.extend(s.held_titles.get(id).into_iter().flatten().cloned());
                    }
                    let body = to_value(c)?;
                    covers_in(&body, &mut covers);
                    rows.push(RowIn {
                        key: c.igdb_id.clone(),
                        id: None,
                        title_id: Some(c.title_id),
                        igdb: Some(c.igdb_id.clone()),
                        platforms: set_text(Some(&plats)),
                        name: c.name.clone(),
                        held: Scope::held_text(Some(&held)),
                        body,
                    });
                }
                insert_rows(&mut out, "crossplatform", &mut ord, rows).await?;
                offset += n;
                if n < PAGE || offset as u64 >= page.total {
                    break;
                }
            }
        }
        if tabs.definitive {
            let mut ord = 0;
            let mut offset = 0;
            loop {
                if progress.cancelled().await {
                    return Err(Error::invalid("cancelled"));
                }
                let page = super::definitive::list(app, &list_query(&platforms, offset)).await?;
                progress
                    .force(
                        1,
                        Some(STEPS),
                        &format!(
                            "definitive: {} of {} read (step 2 of 7)",
                            (offset as u64 + page.items.len() as u64).min(page.total),
                            page.total
                        ),
                    )
                    .await;
                let n = page.items.len() as u32;
                let mut rows = Vec::new();
                for d in &page.items {
                    let ids: Vec<u64> = [
                        d.definitive_title_id,
                        d.base_title_id,
                        d.lead.title_id,
                        d.other.title_id,
                    ]
                    .into_iter()
                    .filter(|id| s.titles.contains(id))
                    .collect();
                    if ids.is_empty() {
                        continue;
                    }
                    let mut plats = BTreeSet::new();
                    let mut held = BTreeSet::new();
                    for id in &ids {
                        plats.extend(s.title_platforms.get(id).into_iter().flatten().copied());
                        held.extend(s.held_titles.get(id).into_iter().flatten().cloned());
                    }
                    let body = to_value(d)?;
                    covers_in(&body, &mut covers);
                    rows.push(RowIn {
                        key: d.id.to_string(),
                        id: Some(d.id),
                        title_id: Some(d.lead.title_id),
                        igdb: None,
                        platforms: set_text(Some(&plats)),
                        name: format!("{} {}", d.lead.name, d.other.name),
                        held: Scope::held_text(Some(&held)),
                        body,
                    });
                }
                insert_rows(&mut out, "definitive", &mut ord, rows).await?;
                offset += n;
                if n < PAGE || offset as u64 >= page.total {
                    break;
                }
            }
        }
    }

    // Games, ROM files, contents: this machine's own, so only when it is one of the chosen.
    let local = o.machines.iter().any(|m| m == THIS_MACHINE);
    if local && (tabs.games || tabs.files || tabs.contents) {
        let all = ListQuery {
            limit: Some(PAGE),
            ..Default::default()
        };
        for (tab, on) in [
            ("games", tabs.games),
            ("files", tabs.files),
            ("contents", tabs.contents),
        ] {
            if !on {
                continue;
            }
            let mut ord = 0;
            let mut offset = 0;
            loop {
                if progress.cancelled().await {
                    return Err(Error::invalid("cancelled"));
                }
                let q = ListQuery {
                    offset: Some(offset),
                    ..all.clone()
                };
                let (items, total): (Vec<Value>, u64) = match tab {
                    "games" => {
                        let p = catalog::games(app, &q).await?;
                        (
                            p.items.iter().map(to_value).collect::<Result<_>>()?,
                            p.total,
                        )
                    }
                    "files" => {
                        let p = catalog::files(app, &q).await?;
                        (
                            p.items.iter().map(to_value).collect::<Result<_>>()?,
                            p.total,
                        )
                    }
                    _ => {
                        let p = catalog::contents(app, &q).await?;
                        (
                            p.items.iter().map(to_value).collect::<Result<_>>()?,
                            p.total,
                        )
                    }
                };
                progress
                    .force(
                        1,
                        Some(STEPS),
                        &format!(
                            "{tab}: {} of {total} read (step 2 of 7)",
                            (offset as u64 + items.len() as u64).min(total)
                        ),
                    )
                    .await;
                let n = items.len() as u32;
                let rows = items
                    .into_iter()
                    .enumerate()
                    .map(|(i, body)| {
                        let id = su(&body, "id");
                        let name = [
                            ss(&body, "name"),
                            ss(&body, "path"),
                            ss(&body, "release_name"),
                            ss(&body, "title_name"),
                        ]
                        .join(" ");
                        let plat: BTreeSet<u16> = su(&body, "platform_id")
                            .map(|p| p as u16)
                            .into_iter()
                            .collect();
                        RowIn {
                            key: id.map_or_else(
                                || format!("{}", offset as usize + i),
                                |v| v.to_string(),
                            ),
                            id,
                            title_id: su(&body, "title_id"),
                            igdb: None,
                            platforms: set_text(Some(&plat)),
                            name,
                            held: format!(",{THIS_MACHINE},"),
                            body,
                        }
                    })
                    .collect();
                insert_rows(&mut out, tab, &mut ord, rows).await?;
                offset += n;
                if n < PAGE || offset as u64 >= total {
                    break;
                }
            }
        }
    }

    // --- pages: held items only ---
    let mut pages = 0u64;
    if o.pages {
        let titles: Vec<u64> = s.held_titles.keys().copied().collect();
        let releases: Vec<u64> = s
            .held
            .keys()
            .copied()
            .filter(|r| s.release_platform.contains_key(r))
            .collect();
        let total = (titles.len() + releases.len()) as u64;
        for (i, id) in titles.iter().enumerate() {
            if i % 50 == 0 {
                if progress.cancelled().await {
                    return Err(Error::invalid("cancelled"));
                }
                progress
                    .force(
                        2,
                        Some(STEPS),
                        &format!("pages: {i} of {total} (step 3 of 7)"),
                    )
                    .await;
            }
            // A title merged away since the scope was read is skipped, not fatal.
            if let Ok(page) = catalog::title(app, *id).await {
                let body = to_value(&page)?;
                covers_in(&body, &mut covers);
                out.page("title", *id, &body).await?;
                pages += 1;
            }
        }
        for (i, id) in releases.iter().enumerate() {
            if i % 50 == 0 {
                if progress.cancelled().await {
                    return Err(Error::invalid("cancelled"));
                }
                progress
                    .force(
                        2,
                        Some(STEPS),
                        &format!("pages: {} of {total} (step 3 of 7)", titles.len() + i),
                    )
                    .await;
            }
            if let Ok(page) = catalog::release(app, *id).await {
                let body = to_value(&page)?;
                covers_in(&body, &mut covers);
                out.page("release", *id, &body).await?;
                pages += 1;
            }
        }
    }

    // --- RetroAchievements: every RA game with the account's progress ---
    let mut ra_boxart = Vec::new();
    if tabs.ra {
        progress
            .force(
                3,
                Some(STEPS),
                "RetroAchievements games and progress (step 4 of 7)",
            )
            .await;
        let n = write_ra(app, &out.db, &mut covers, &mut ra_boxart).await?;
        out.count("ra", n);
    }

    // --- covers ---
    let mut cover_count = 0u64;
    if o.covers {
        cover_count = write_covers(app, &out.db, o, &covers, &ra_boxart, progress, STEPS).await?;
    }

    // --- the catalogue's own tables, cut down to the scope ---
    progress
        .force(5, Some(STEPS), "copying the catalogue tables (step 6 of 7)")
        .await;
    let tables = write_tables(app, &out.db, o, &s, &covers, progress, STEPS).await?;
    for (t, n) in tables {
        out.count(&t, n);
    }

    progress
        .force(6, Some(STEPS), "finishing the file (step 7 of 7)")
        .await;
    sqlx::query("ANALYZE").execute(&out.db).await.ok();
    out.db.close().await;
    std::fs::rename(&part, path).map_err(|e| {
        Error::internal(format!(
            "renaming {} to {}: {e}",
            part.display(),
            path.display()
        ))
    })?;
    guard.0 = None;
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    Ok(SyncSummary {
        file: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        bytes,
        rows: out.rows,
        pages,
        covers: cover_count,
    })
}

/// Every RetroAchievements game this machine knows, with the account's progress, into `ra`
/// (and, for RA progress alone, as the same rows over REST: [`ra_rows`]).
async fn write_ra(
    app: &App,
    db: &sqlx::SqlitePool,
    covers: &mut BTreeSet<u64>,
    boxart: &mut Vec<u64>,
) -> Result<u64> {
    let rows = ra_rows(app).await?;
    let mut tx = db.begin().await?;
    sqlx::query("DELETE FROM _mobile_ra")
        .execute(&mut *tx)
        .await?;
    for (i, r) in rows.iter().enumerate() {
        let body = to_value(r)?;
        match r.cover_media_id {
            Some(id) => {
                covers.insert(id);
            }
            None => boxart.push(r.game_id),
        }
        insert_ra(&mut tx, i as i64, r, &body).await?;
    }
    tx.commit().await?;
    Ok(rows.len() as u64)
}

async fn insert_ra(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    ord: i64,
    r: &super::ra::GameRow,
    body: &Value,
) -> Result<()> {
    sqlx::query("INSERT OR REPLACE INTO _mobile_ra (game_id, ord, title, console_id, local, num_awarded, highest_award_kind, most_recent_awarded, body) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)")
        .bind(r.game_id as i64)
        .bind(ord)
        .bind(r.title.clone().unwrap_or_default().to_lowercase())
        .bind(r.console_id)
        .bind(r.local as i64)
        .bind(r.num_awarded)
        .bind(r.highest_award_kind.clone().unwrap_or_default())
        .bind(r.most_recent_awarded.map(|d| d.to_string()))
        .bind(body.to_string())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Every RA game, held ones first, by title: what the phone's RA tabs are made from. Also
/// RA progress on its own (`GET /api/v1/mobile/ra`), which a phone fetches without a full
/// sync.
pub async fn ra_rows(app: &App) -> Result<Vec<super::ra::GameRow>> {
    let mut out = Vec::new();
    let mut offset = 0;
    loop {
        let q = super::ra::GamesQuery {
            list: Some("titles".into()),
            source: Some("all".into()),
            include_beaten: Some(true),
            limit: Some(PAGE),
            offset: Some(offset),
            ..Default::default()
        };
        let page = super::ra::games(app, &q).await?;
        let n = page.items.len() as u32;
        out.extend(page.items);
        offset += n;
        if n < PAGE || i64::from(offset) >= page.total {
            break;
        }
    }
    Ok(out)
}

/// RA progress on its own, for a phone between full syncs: the rows its RA tabs show, and
/// the account's `ra_user_progress` rows for the MariaDB-shaped table beside them.
#[derive(Debug, Serialize)]
pub struct RaSync {
    pub rows: Vec<super::ra::GameRow>,
    pub progress: Vec<Value>,
}

pub async fn ra_sync(app: &App) -> Result<RaSync> {
    let rows = ra_rows(app).await?;
    let mut progress = Vec::new();
    let mut after: Option<Vec<String>> = None;
    loop {
        let page =
            super::sync::export_page(app, "ra_user_progress", after.as_deref(), 5_000).await?;
        progress.extend(page.rows);
        match page.next {
            Some(cursor) => after = Some(cursor),
            None => break,
        }
    }
    Ok(RaSync { rows, progress })
}

/// The covers, as thumbnails in the phone's format, `images` keyed the way the phone asks
/// for them (`media/<id>`, `ra/<game>/boxart`). Made in batches on the blocking pool.
async fn write_covers(
    app: &App,
    db: &sqlx::SqlitePool,
    o: &SyncOptions,
    covers: &BTreeSet<u64>,
    boxart: &[u64],
    progress: &Progress,
    steps: u64,
) -> Result<u64> {
    use crate::database::media_cache::{make_thumbs, ThumbFormat};
    let format = ThumbFormat::from_name(&o.cover_format).unwrap_or(ThumbFormat::Avif);
    let (_, quality, _) = crate::database::settings::media_thumb(&app.pool).await?;
    let px = o.cover_px;
    let mut wanted: Vec<(String, Option<std::path::PathBuf>)> = Vec::new();
    for id in covers {
        wanted.push((
            format!("media/{id}"),
            super::media::file(app, *id, false, None)
                .await
                .ok()
                .map(|(p, _)| p),
        ));
    }
    for gid in boxart {
        wanted.push((
            format!("ra/{gid}/boxart"),
            super::ra::image(app, *gid as u32, "boxart").await.ok(),
        ));
    }
    let total = wanted.len() as u64;
    let mut done = 0u64;
    let mut written = 0u64;
    for chunk in wanted.chunks(32) {
        if progress.cancelled().await {
            return Err(Error::invalid("cancelled"));
        }
        progress
            .force(
                4,
                Some(steps),
                &format!("covers: {done} of {total} made (step 5 of 7)"),
            )
            .await;
        let mut keys = Vec::new();
        let mut sources = Vec::new();
        for (key, path) in chunk {
            if let Some(bytes) = path.as_ref().and_then(|p| std::fs::read(p).ok()) {
                keys.push(key.clone());
                sources.push(bytes);
            }
        }
        let made = tokio::task::spawn_blocking(move || make_thumbs(&sources, px, format, quality))
            .await
            .map_err(|e| Error::internal(e.to_string()))?;
        let mut tx = db.begin().await?;
        for (key, result) in keys.iter().zip(made) {
            if let Ok((bytes, used)) = result {
                let mime = if used == ThumbFormat::Avif {
                    "image/avif"
                } else {
                    "image/jpeg"
                };
                sqlx::query(
                    "INSERT OR REPLACE INTO _mobile_images (key, mime, bytes) VALUES (?, ?, ?)",
                )
                .bind(key)
                .bind(mime)
                .bind(bytes)
                .execute(&mut *tx)
                .await?;
                written += 1;
            }
        }
        tx.commit().await?;
        done += chunk.len() as u64;
    }
    Ok(written)
}

/// Which rows of a MariaDB table come along: the scope's titles, releases, cover media,
/// this machine's games / ROMs / files behind them, the chosen remote sources' games.
#[derive(Clone, Copy)]
enum Keep {
    All,
    /// `column` is a title id in scope.
    Title(&'static str),
    /// `column` is a release id in scope.
    Release(&'static str),
    /// Both, for the title / release link table.
    TitleRelease,
    /// `column` is a cover media id; the junctions are also narrowed by their title
    /// (`true`) or release (`false`) column.
    Media(&'static str, Option<(&'static str, bool)>),
    LocalGame(&'static str),
    LocalRom(&'static str),
    LocalFile(&'static str),
    Source(&'static str),
    /// A chosen source's row for a release in scope.
    SourceRelease,
    /// A chosen source's game behind a release in scope.
    SourceGame,
}

/// The tables the file carries with the MariaDB schema's names and columns, cut down by
/// [`Keep`]: the slim version of the full database, so a full romexplorer on the phone can
/// grow from the same file. Each comes only when the option it serves is on (see
/// [`write_tables`]); one this server doesn't have, or whose key column moved, is left out.
const TABLES: &[(&str, Keep)] = &[
    ("platforms", Keep::All),
    ("sources", Keep::All),
    ("title", Keep::Title("id")),
    ("title_releases", Keep::TitleRelease),
    ("title_locales", Keep::Title("title_id")),
    ("title_alt_names", Keep::Title("title_id")),
    ("title_alt_names_hidden", Keep::Title("title_id")),
    ("title_provider_links", Keep::Title("title_id")),
    ("title_genres", Keep::Title("title_id")),
    ("title_companies", Keep::Title("title_id")),
    ("title_franchises", Keep::Title("title_id")),
    ("release", Keep::Release("id")),
    ("release_sources", Keep::Release("release_id")),
    ("release_provider_links", Keep::Release("release_id")),
    ("release_display", Keep::Release("release_id")),
    ("release_combined", Keep::Release("release_id")),
    ("media", Keep::Media("id", None)),
    (
        "title_media",
        Keep::Media("media_id", Some(("title_id", true))),
    ),
    (
        "release_media",
        Keep::Media("media_id", Some(("release_id", false))),
    ),
    ("local_release_games", Keep::Release("release_id")),
    ("local_game", Keep::LocalGame("id")),
    ("local_game_roms", Keep::LocalGame("game_id")),
    ("local_rom", Keep::LocalRom("id")),
    ("local_rom_files", Keep::LocalRom("rom_id")),
    ("local_files", Keep::LocalFile("id")),
    ("remote_sources", Keep::Source("id")),
    ("remote_release_games", Keep::SourceRelease),
    ("remote_game", Keep::SourceGame),
    ("ra_game", Keep::All),
    ("ra_user_progress", Keep::All),
];

async fn exists(app: &App, table: &str, column: Option<&str>) -> Result<bool> {
    let n: i64 = match column {
        Some(c) => sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? AND COLUMN_NAME = ?").bind(table).bind(c).fetch_one(&app.pool).await?,
        None => sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ?").bind(table).fetch_one(&app.pool).await?,
    };
    Ok(n > 0)
}

async fn ids_of(app: &App, sql: String) -> Result<Vec<u64>> {
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .fetch_all(&app.pool)
        .await?)
}

/// `col IN (...)`, or never true for an empty set. Ids are integers: safe to inline.
fn in_list(col: &str, ids: &[u64]) -> String {
    if ids.is_empty() {
        "FALSE".to_string()
    } else {
        format!("`{col}` IN ({})", id_list(ids))
    }
}

/// Whether `table` serves an option that is on.
fn wanted(
    table: &str,
    keep: Keep,
    o: &SyncOptions,
    local: bool,
    local_files: bool,
    remote: bool,
) -> bool {
    match keep {
        Keep::LocalGame(_) => local,
        Keep::LocalRom(_) | Keep::LocalFile(_) => local_files,
        Keep::Source(_) | Keep::SourceRelease | Keep::SourceGame => remote,
        Keep::Media(..) => o.covers,
        _ if table == "local_release_games" => local,
        _ if table.starts_with("ra_") => o.tabs.ra,
        _ => true,
    }
}

async fn write_tables(
    app: &App,
    db: &sqlx::SqlitePool,
    o: &SyncOptions,
    s: &Scope,
    covers: &BTreeSet<u64>,
    progress: &Progress,
    steps: u64,
) -> Result<Vec<(String, u64)>> {
    let local = o.machines.iter().any(|m| m == THIS_MACHINE);
    let sources: Vec<u64> = o
        .machines
        .iter()
        .filter_map(|m| m.parse::<u16>().ok())
        .map(u64::from)
        .collect();
    let local_files = local && (o.tabs.games || o.tabs.files || o.tabs.contents);
    let mut titles: Vec<u64> = s.titles.iter().copied().collect();
    titles.sort_unstable();
    let mut releases: Vec<u64> = s.releases.iter().copied().collect();
    releases.sort_unstable();
    let media: Vec<u64> = covers.iter().copied().collect();
    let rel = in_list("release_id", &releases);
    let src = in_list("source_id", &sources);

    let games = if local {
        ids_of(
            app,
            format!("SELECT DISTINCT game_id FROM local_release_games WHERE {rel}"),
        )
        .await?
    } else {
        Vec::new()
    };
    let roms = if local_files && !games.is_empty() {
        ids_of(
            app,
            format!(
                "SELECT DISTINCT rom_id FROM local_game_roms WHERE {}",
                in_list("game_id", &games)
            ),
        )
        .await?
    } else {
        Vec::new()
    };
    let files = if local_files && !roms.is_empty() {
        ids_of(
            app,
            format!(
                "SELECT DISTINCT file_id FROM local_rom_files WHERE {}",
                in_list("rom_id", &roms)
            ),
        )
        .await?
    } else {
        Vec::new()
    };

    let mut out = Vec::new();
    let mut skipped = Vec::new();
    let n = TABLES.len();
    for (i, (table, keep)) in TABLES.iter().enumerate() {
        if !wanted(table, *keep, o, local, local_files, !sources.is_empty()) {
            continue;
        }
        let (filter, column) = match *keep {
            Keep::All => (None, None),
            Keep::Title(c) => (Some(in_list(c, &titles)), Some(c)),
            Keep::Release(c) => (Some(in_list(c, &releases)), Some(c)),
            Keep::TitleRelease => (Some(format!("{} AND {rel}", in_list("title_id", &titles))), Some("title_id")),
            Keep::Media(c, None) => (Some(in_list(c, &media)), Some(c)),
            Keep::Media(c, Some((by, title))) => (Some(format!("{} AND {}", in_list(c, &media), in_list(by, if title { &titles } else { &releases }))), Some(by)),
            Keep::LocalGame(c) => (Some(in_list(c, &games)), Some(c)),
            Keep::LocalRom(c) => (Some(in_list(c, &roms)), Some(c)),
            Keep::LocalFile(c) => (Some(in_list(c, &files)), Some(c)),
            Keep::Source(c) => (Some(in_list(c, &sources)), Some(c)),
            Keep::SourceRelease => (Some(format!("{src} AND {rel}")), Some("source_id")),
            Keep::SourceGame => (Some(format!("{src} AND `id` IN (SELECT game_id FROM remote_release_games WHERE {src} AND {rel})")), Some("source_id")),
        };
        if !exists(app, table, None).await?
            || (column.is_some() && !exists(app, table, column).await?)
        {
            skipped.push(*table);
            continue;
        }
        progress
            .force(
                5,
                Some(steps),
                &format!("table {} of {n} ({table}) (step 6 of 7)", i + 1),
            )
            .await;
        let rows =
            super::sqlite::copy_table(app, db, table, filter.as_deref(), progress, &|rows| {
                (
                    5,
                    Some(steps),
                    format!(
                        "table {} of {n} ({table}): {rows} rows (step 6 of 7)",
                        i + 1
                    ),
                )
            })
            .await?;
        out.push((table.to_string(), rows));
    }
    // A peer's token never leaves the machine that holds it.
    if out.iter().any(|(t, _)| t == "remote_sources") {
        sqlx::query("UPDATE remote_sources SET token = NULL")
            .execute(db)
            .await?;
    }
    if !skipped.is_empty() {
        log::warn!(
            "mobile sync: tables left out (missing here or reshaped): {}",
            skipped.join(", ")
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_are_found_at_any_depth() {
        let v = serde_json::json!({ "cover_media_id": 1, "releases": [{ "cover_media_id": 2 }, { "x": { "cover_media_id": 3 } }], "cover": 9 });
        let mut out = BTreeSet::new();
        covers_in(&v, &mut out);
        assert_eq!(out.into_iter().collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    #[test]
    fn options_are_checked() {
        assert!(validate(&SyncOptions::default()).is_ok());
        assert!(validate(&SyncOptions {
            machines: vec![],
            ..Default::default()
        })
        .is_err());
        assert!(validate(&SyncOptions {
            machines: vec!["main".into()],
            ..Default::default()
        })
        .is_err());
        assert!(validate(&SyncOptions {
            scope: "everything".into(),
            ..Default::default()
        })
        .is_err());
        assert!(validate(&SyncOptions {
            cover_format: "webp".into(),
            ..Default::default()
        })
        .is_err());
        assert!(validate(&SyncOptions {
            machines: vec!["local".into(), "3".into()],
            scope: "catalog".into(),
            catalog_platforms: vec![5],
            ..Default::default()
        })
        .is_ok());
    }

    #[test]
    fn every_table_is_one_a_backup_or_peer_sync_carries() {
        // The file's tables are the full database's, so a full database can take them back.
        for (table, _) in TABLES {
            assert!(
                super::super::backup::CATALOG_TABLES.contains(table)
                    || super::super::backup::DATABASE_TABLES.contains(table)
                    || table.starts_with("remote_")
                    || table.starts_with("ra_"),
                "{table} is not a table the backups carry"
            );
        }
    }

    #[test]
    fn held_lists_find_one_member() {
        let set: BTreeSet<String> = ["local".to_string(), "3".to_string()].into_iter().collect();
        assert_eq!(Scope::held_text(Some(&set)), ",3,local,");
        assert_eq!(Scope::held_text(None), "");
        assert_eq!(set_text(Some(&[5u16, 12].into_iter().collect())), ",5,12,");
    }
}
