//! What one machine hands another so it can mirror this collection.
//!
//! Only the `local_*` ownership tables travel (`docs/plan/api.md`): the shared catalogue --
//! `content`, `hashes`, `release`, `title` -- is identified the same way on every machine,
//! so a mirrored row references the very same `release_id` a local one would. That is the
//! whole reason the mirror is small and the import is an upsert rather than a merge.
//!
//! A main server also offers its art: `media` and its title/release junctions, mirrored into
//! `remote_media` the same way ownership is, so a client knows what the server holds and
//! fetches the bytes from it rather than from a provider (`super::media_sync`).
//!
//! Two calls. [`manifest`] is the cheap question "has anything moved?", answered per table
//! so a client re-downloads only what changed. [`export`] is the paged answer. Both are
//! read-only: a client mirrors by pulling, and nothing here ever writes.
//!
//! Rows are projected to JSON **by the database**, from `information_schema`, rather than
//! through eight hand-written serialisers. Those would have to be kept in step with every
//! future migration by hand, and a column silently missing from an export is exactly the
//! kind of drift nothing would catch.

use sqlx::Row;

use super::{App, Error, Result};

// Shared with the GUI through `romexplorer_types`.
pub use romexplorer_types::sync::{DeletedPage, ExportPage, Manifest, PullSummary, TableState, GROUP_CATALOG, GROUP_MEDIA, GROUP_OWNERSHIP};

/// The ownership tables a peer mirrors, and the only table names this module will accept.
/// Everything here is interpolated into SQL, so membership of this list *is* the validation.
pub const MIRRORED: [&str; 8] = [
    "local_directories",
    "local_files",
    "local_rom",
    "local_rom_files",
    "local_rom_rahash",
    "local_game",
    "local_game_roms",
    "local_release_games",
];

/// The art a main server offers alongside its catalogue, mirrored under a `source_id` like
/// ownership is: `media` rows are this machine's own (own ids, own paths), so the server's
/// cannot be written into them. Interpolated into SQL like [`MIRRORED`].
pub const MEDIA_MIRRORED: [&str; 3] = ["media", "title_media", "release_media"];

/// The mirror name for a source table: `local_files` on the source is `remote_files` here,
/// and `media` is `remote_media`.
pub fn remote_name(local: &str) -> String {
    format!("remote_{}", local.trim_start_matches("local_"))
}

/// The catalogue tables a *main* server offers.
///
/// The backup's `CATALOG_TABLES` plus the curated collection sets. Those are deliberately in
/// two places at once, and the reason is worth stating: they are **backed up** as this
/// machine's own, because a standalone install has no other copy of a hand-made list, but they
/// are **synced** because where there is a main server the household's sets are its to hand
/// down. Splitting the two lists is what lets both be true.
///
/// Appended last so `collection_sets` and `collection_set_releases` arrive after `release`,
/// which they reference.
pub fn catalog_tables() -> &'static [&'static str] {
    static ALL: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    ALL.get_or_init(|| super::backup::CATALOG_TABLES.iter().copied().chain(["collection_sets", "collection_set_entries", "collection_set_releases"]).collect())
}

/// Every table a peer can be offered, and so every table that carries change numbers
/// (`database::sync_rev`).
pub fn tracked_tables() -> Vec<&'static str> {
    MIRRORED.iter().copied().chain(catalog_tables().iter().copied()).chain(MEDIA_MIRRORED.iter().copied()).collect()
}

/// Whether this machine may mirror `source`'s ownership rows.
///
/// The mirror copies the source's `release_id` and `content_id` **verbatim**, because the
/// catalogue is meant to be identical on both machines. That is true when both got it from
/// the same place, and false when each imported the same DATs independently -- ids are
/// per-machine AUTO_INCREMENT values, so the same game is id 2 on one machine and id 61260 on
/// another. Importing across that boundary does not fail; it silently attaches the peer's
/// games to whichever unrelated releases happen to hold those ids.
///
/// So ownership travels only between machines whose catalogues share a lineage:
/// - the source is my main server (I take my catalogue from it),
/// - the source took its catalogue from me,
/// - or we both took ours from the same third machine.
pub fn lineage_ok(ours: &str, our_catalog_from: Option<&str>, source_instance: &str, source_catalog_from: Option<&str>, source_is_main: bool) -> bool {
    if source_is_main {
        // The catalogue is about to come from it, so its ids become ours by definition.
        return true;
    }
    if source_instance.is_empty() {
        // A source too old to say who it is cannot be checked, and an unverifiable mirror is
        // worse than none.
        return false;
    }
    if source_catalog_from == Some(ours) {
        return true;
    }
    match our_catalog_from {
        Some(mine) => source_catalog_from == Some(mine) || source_instance == mine,
        None => false,
    }
}

/// A table's sum for the manifest, and when it was taken.
struct Summed {
    /// The table's `UPDATE_TIME` when it was summed, in seconds since the epoch.
    updated: Option<i64>,
    /// When it was summed, by the database's clock.
    at: i64,
    hash: String,
    rows: u64,
}

fn summed() -> &'static std::sync::Mutex<std::collections::HashMap<String, Summed>> {
    static SUMMED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, Summed>>> = std::sync::OnceLock::new();
    SUMMED.get_or_init(Default::default)
}

/// What a table is, or `None` if this is not a table anyone may ask for. `with_catalog` is
/// false on a share listener: a client that shares its collection offers what it *holds*, and
/// nothing else. Keeping that decision on the serving side means a client asking for more
/// than a share offers is refused rather than trusted.
pub fn group_of(table: &str, with_catalog: bool) -> Option<&'static str> {
    if MIRRORED.contains(&table) {
        Some(GROUP_OWNERSHIP)
    } else if with_catalog && catalog_tables().contains(&table) {
        Some(GROUP_CATALOG)
    } else if with_catalog && MEDIA_MIRRORED.contains(&table) {
        // Junction ids are catalogue ids, so art goes wherever the catalogue goes and nowhere
        // it does not.
        Some(GROUP_MEDIA)
    } else {
        None
    }
}

fn check_table(table: &str, with_catalog: bool) -> Result<()> {
    if group_of(table, with_catalog).is_none() {
        return Err(Error::invalid(format!("{table:?} is not a table this server exports")));
    }
    Ok(())
}

/// The primary key columns, in order -- the only stable sort for a paged export, and the
/// cursor a client pages with. Junction tables have composite keys, so this is a list.
async fn primary_key(app: &App, table: &str) -> Result<Vec<String>> {
    let rows = sqlx::query(
        "SELECT COLUMN_NAME FROM information_schema.STATISTICS \
         WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? AND INDEX_NAME = 'PRIMARY' \
         ORDER BY SEQ_IN_INDEX",
    )
    .bind(table)
    .fetch_all(&app.pool)
    .await?;
    let cols: Vec<String> = rows.iter().filter_map(|r| r.try_get::<String, _>("COLUMN_NAME").ok()).collect();
    if cols.is_empty() {
        return Err(Error::internal(format!("{table} has no primary key to page by")));
    }
    Ok(cols)
}

/// `'col', `col`, ...` for `JSON_OBJECT`, straight from the live schema, so a column added
/// by a later migration is exported without anyone remembering to add it here.
///
/// Every export -- a peer's, the backup's, the mobile cache's -- comes through here, which is
/// why credentials are nulled here and not by each caller ([`super::secrets`]).
async fn json_projection(app: &App, table: &str) -> Result<String> {
    // Never the change number: it means something only in this database (`sync_rev`).
    let columns: Vec<String> = sqlx::query_scalar("SELECT COLUMN_NAME FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? AND COLUMN_NAME <> 'sync_rev' ORDER BY ORDINAL_POSITION")
        .bind(table)
        .fetch_all(&app.pool)
        .await?;
    if columns.is_empty() {
        return Err(Error::not_found(format!("no such table {table}")));
    }
    Ok(super::secrets::projection_sql(table, &columns))
}

/// Per-table state: what a client compares against to decide whether to download at all.
///
/// `with_catalog` is what makes a server a *main* server rather than a share (see
/// [`GROUP_OWNERSHIP`]).
pub async fn manifest(app: &App, with_catalog: bool) -> Result<Manifest> {
    let names: Vec<&str> = MIRRORED
        .iter()
        .copied()
        .chain(if with_catalog { catalog_tables() } else { &[] }.iter().copied())
        .chain(if with_catalog { &MEDIA_MIRRORED[..] } else { &[] }.iter().copied())
        .collect();
    // Taken before any table is summed: a client records it as its new position, and a change
    // made while the tables are being summed must stay above it.
    let (epoch, pruned_through) = crate::database::sync_rev::state(&app.pool).await?;
    let stable_rev = crate::database::sync_rev::stable(&app.pool, &epoch).await;
    prune_now_and_then(app).await;
    let tracked: Vec<String> = sqlx::query_scalar("SELECT TABLE_NAME FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = DATABASE() AND COLUMN_NAME = ?")
        .bind(crate::database::sync_rev::COLUMN)
        .fetch_all(&app.pool)
        .await?;
    let is_tracked = |t: &str| tracked.iter().any(|x| x == t);
    let mut tables = Vec::new();
    for table in names {
        // Both `CHECKSUM TABLE` and `COUNT(*)` read the whole table, and every client asks for
        // this on every sync. InnoDB keeps when a table was last written (`UPDATE_TIME`), so
        // a table nobody wrote since it was last summed keeps its sum. Only a write at least
        // two seconds before the sum is trusted that way, the time being in whole seconds,
        // and a NULL time (none since the server started, or an engine that keeps none) is
        // always summed again.
        // Both read in the database's clock, as whole seconds since the epoch.
        let (updated, now): (Option<i64>, i64) = match sqlx::query_as::<_, (Option<i64>, i64)>(
            "SELECT CAST(UNIX_TIMESTAMP(UPDATE_TIME) AS SIGNED), CAST(UNIX_TIMESTAMP() AS SIGNED) FROM information_schema.tables WHERE table_schema = DATABASE() AND table_name = ?",
        )
        .bind(table)
        .fetch_optional(&app.pool)
        .await
        {
            Ok(Some(found)) => found,
            _ => (None, i64::MIN),
        };
        let reuse = now != i64::MIN;
        if let Some(cached) = summed().lock().unwrap_or_else(|p| p.into_inner()).get(table).filter(|c| reuse && c.updated == updated && updated.is_some_and(|u| u + 2 <= c.at)) {
            tables.push(TableState { table: table.to_string(), group: group_of(table, with_catalog).unwrap_or(GROUP_OWNERSHIP).to_string(), hash: cached.hash.clone(), rows: cached.rows, tracked: is_tracked(table) });
            continue;
        }
        let at = now;
        // `CHECKSUM TABLE` is row-content based and changes whenever any row does.
        //
        // The width it comes back as is not something to assume: it is the second column of a
        // result set MariaDB builds itself, and a decode that only tries one Rust type fails
        // silently into "no hash", which means the client re-downloads every table on every
        // sync and the manifest quietly stops doing its job. So try both signednesses, and
        // say so if neither works rather than degrading in silence.
        let row = sqlx::query(sqlx::AssertSqlSafe(format!("CHECKSUM TABLE `{table}`"))).fetch_one(&app.pool).await?;
        let checksum: Option<u64> = row
            .try_get::<Option<u64>, _>(1)
            .ok()
            .flatten()
            .or_else(|| row.try_get::<Option<i64>, _>(1).ok().flatten().map(|v| v as u64));
        if checksum.is_none() {
            log::warn!("sync: no checksum for {table}; clients will re-download it every time");
        }
        let rows: i64 = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) AS n FROM `{table}`")))
            .fetch_one(&app.pool)
            .await?
            .try_get("n")?;
        let hash = checksum.map(|c| format!("{c:x}")).unwrap_or_default();
        if checksum.is_some() && reuse {
            summed().lock().unwrap_or_else(|p| p.into_inner()).insert(table.to_string(), Summed { updated, at, hash: hash.clone(), rows: rows as u64 });
        }
        tables.push(TableState { table: table.to_string(), group: group_of(table, with_catalog).unwrap_or(GROUP_OWNERSHIP).to_string(), hash, rows: rows as u64, tracked: is_tracked(table) });
    }
    Ok(Manifest {
        generated_at: chrono::Local::now().naive_local(),
        instance_id: crate::database::settings::instance_id(&app.pool).await?,
        catalog_from: crate::database::settings::get::<String>(&app.pool, crate::database::settings::KEY_CATALOG_FROM).await?.filter(|s| !s.is_empty()),
        tables,
        sync_epoch: Some(epoch),
        stable_rev,
        pruned_through,
    })
}

/// Prunes old tombstones at most once a day. Housekeeping rather than sync: it runs here only
/// because a server that is never restarted would otherwise keep them for ever.
async fn prune_now_and_then(app: &App) {
    static LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    {
        let mut last = LAST.lock().unwrap_or_else(|p| p.into_inner());
        if last.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(24 * 3600)) {
            return;
        }
        *last = Some(std::time::Instant::now());
    }
    if let Err(e) = crate::database::sync_rev::prune(&app.pool).await {
        log::warn!("sync: pruning old tombstones failed: {e}");
    }
}

/// The rows of `table` changed after `since`, in change order, paged by change number.
///
/// `after` is the previous page's `next`. A row changed again while a client pages moves
/// past the cursor and is sent once more, later -- never skipped.
pub async fn export_since(app: &App, table: &str, since: u64, after: Option<u64>, limit: u32, with_catalog: bool) -> Result<ExportPage> {
    check_table(table, with_catalog)?;
    let limit = limit.clamp(1, 5_000);
    let projection = json_projection(app, table).await?;
    let from = since.max(after.unwrap_or(0));
    let col = crate::database::sync_rev::COLUMN;
    let sql = format!("SELECT JSON_OBJECT({projection}) AS row_json, `{col}` AS rev FROM `{table}` WHERE `{col}` > ? ORDER BY `{col}` LIMIT {limit}");
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(from).fetch_all(&app.pool).await?;
    let mut out = Vec::with_capacity(rows.len());
    let mut last = None;
    for row in &rows {
        let json: String = row.try_get("row_json")?;
        out.push(serde_json::from_str(&json).map_err(|e| Error::internal(format!("{table}: {e}")))?);
        last = Some(row.try_get::<u64, _>("rev")?);
    }
    let next = if out.len() as u32 == limit { last.map(|r| vec![r.to_string()]) } else { None };
    Ok(ExportPage { table: table.to_string(), rows: out, next })
}

/// The rows of `table` deleted after `since`, as primary keys, paged by change number.
pub async fn deleted_since(app: &App, table: &str, since: u64, after: Option<u64>, limit: u32, with_catalog: bool) -> Result<DeletedPage> {
    check_table(table, with_catalog)?;
    let limit = limit.clamp(1, 20_000);
    let key = primary_key(app, table).await?;
    let from = since.max(after.unwrap_or(0));
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT rev, pk FROM sync_tombstones WHERE table_name = ? AND rev > ? ORDER BY rev LIMIT {limit}")))
        .bind(table)
        .bind(from)
        .fetch_all(&app.pool)
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    let mut last = None;
    for row in &rows {
        let pk: String = row.try_get("pk")?;
        let values: Vec<Option<String>> = serde_json::from_str(&pk).map_err(|e| Error::internal(format!("{table}: tombstone {pk:?}: {e}")))?;
        // A key that no longer has the table's shape (the key changed since) cannot be
        // applied; leaving it out is safe because a key change rebuilds the table anyway.
        if values.len() == key.len() {
            out.push(values.into_iter().map(Option::unwrap_or_default).collect());
        }
        last = Some(row.try_get::<u64, _>("rev")?);
    }
    let next = if rows.len() as u32 == limit { last } else { None };
    Ok(DeletedPage { table: table.to_string(), key, rows: out, next })
}

/// One page of a table, ordered by its primary key.
///
/// Pages by a key cursor rather than `OFFSET`: a deep offset re-scans everything before it,
/// and on a table being written during the export it silently skips or repeats rows.
pub async fn export(app: &App, table: &str, after: Option<&[String]>, limit: u32, with_catalog: bool) -> Result<ExportPage> {
    check_table(table, with_catalog)?;
    export_page(app, table, after, limit).await
}

/// [`export`] without the mirror whitelist, for callers with their own -- [`super::backup`]
/// covers a wider set of tables than a peer is ever shown. **The caller must have validated
/// `table` against a fixed list**: it is interpolated into SQL.
pub(crate) async fn export_page(app: &App, table: &str, after: Option<&[String]>, limit: u32) -> Result<ExportPage> {
    export_page_filtered(app, table, None, after, limit).await
}

/// [`export_page`] with an extra `WHERE` fragment ANDed onto the cursor condition.
///
/// `filter` is interpolated rather than bound, which is the same posture `table` has always
/// had here: both come from constant lists inside this crate, never from a request. The one
/// caller is `backup::create` for [`Kind::DatSeed`], whose fragments are built in
/// `backup::seed_filter` from `dat_download::Source::ALL`.
///
/// Paging still works because the filter is ANDed with the keyset condition rather than
/// replacing it, so the cursor keeps advancing over the filtered set. What it does skew is
/// `rows::estimated_rows`, which asks the optimizer about the whole table -- the progress bar
/// therefore over-counts on a filtered export and finishes early. That is a cosmetic cost on
/// a job that runs rarely, and the alternative is a COUNT over `hashes`.
pub(crate) async fn export_page_filtered(
    app: &App,
    table: &str,
    filter: Option<&str>,
    after: Option<&[String]>,
    limit: u32,
) -> Result<ExportPage> {
    let limit = limit.clamp(1, 5_000);
    let key = primary_key(app, table).await?;
    let projection = json_projection(app, table).await?;
    let order = key.iter().map(|c| format!("`{c}`")).collect::<Vec<_>>().join(", ");

    let cursor_clause = match after {
        Some(values) if values.len() == key.len() => {
            let placeholders = vec!["?"; key.len()].join(", ");
            Some(format!("({order}) > ({placeholders})"))
        }
        Some(_) => return Err(Error::invalid("after must have one value per primary key column")),
        None => None,
    };
    let where_clause = match (cursor_clause, filter) {
        (Some(cursor), Some(f)) => format!("WHERE {cursor} AND ({f})"),
        (Some(cursor), None) => format!("WHERE {cursor}"),
        (None, Some(f)) => format!("WHERE ({f})"),
        (None, None) => String::new(),
    };

    // The cursor columns are CAST to CHAR *in SQL* so they genuinely arrive as text. Reading
    // a BIGINT key back as a `String` fails in sqlx, and the failure used to be swallowed
    // into an empty cursor -- which MySQL then coerced to `id > 0`, so every page returned
    // the same rows and any table larger than one page exported forever.
    let cursor_select = key.iter().enumerate().map(|(i, c)| format!("CAST(`{c}` AS CHAR) AS `__cursor{i}`")).collect::<Vec<_>>().join(", ");
    let sql = format!("SELECT JSON_OBJECT({projection}) AS row_json, {cursor_select} FROM `{table}` {where_clause} ORDER BY {order} LIMIT {limit}");
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    if let Some(values) = after {
        for value in values {
            query = query.bind(value);
        }
    }
    let rows = query.fetch_all(&app.pool).await?;

    let mut out = Vec::with_capacity(rows.len());
    let mut last: Option<Vec<String>> = None;
    for row in &rows {
        let json: String = row.try_get("row_json")?;
        out.push(serde_json::from_str(&json).map_err(|e| Error::internal(format!("{table}: {e}")))?);
        // Text for both integer and string keys, via the CAST above. A genuine read failure
        // is an error now, never an empty cursor.
        let mut cursor = Vec::with_capacity(key.len());
        for i in 0..key.len() {
            let value: Option<String> = row.try_get(format!("__cursor{i}").as_str())?;
            cursor.push(value.unwrap_or_default());
        }
        last = Some(cursor);
    }
    // A short page means the table is exhausted; a full one might not be.
    let next = if out.len() as u32 == limit { last } else { None };
    // A full page whose cursor did not move would be asked for again, forever. That cannot
    // happen with a primary-key cursor and a matching ORDER BY, so if it ever does, it is a
    // bug in this function -- and an export that silently repeats itself is far worse than
    // one that stops and says so.
    if let (Some(next), Some(after)) = (next.as_ref(), after) {
        if next.as_slice() == after {
            return Err(Error::internal(format!("{table}: export cursor did not advance past {after:?}")));
        }
    }
    Ok(ExportPage { table: table.to_string(), rows: out, next })
}

// ---------------------------------------------------------------------------------------
// The client half: pulling a source into this machine.
// ---------------------------------------------------------------------------------------

/// The ownership tables with no `deleted` column. Everything else is soft-deleted, so an
/// upsert already carries a deletion across; these are hard-deleted on the source, so a row
/// removed there would linger here forever. They are replaced wholesale per source instead,
/// which is cheap -- they are narrow -- and none of them is a parent of anything, so deleting
/// them first cannot cascade.
const OWNERSHIP_JUNCTIONS: [&str; 4] = ["local_rom_files", "local_game_roms", "local_release_games", "local_rom_rahash"];

/// The art junctions, hard-deleted on the source for the same reason.
const MEDIA_JUNCTIONS: [&str; 2] = ["title_media", "release_media"];

/// The catalogue's link tables, hard-deleted on the source when a title is split or merged,
/// a release folded, a genre dropped. Replaced whole when pulled, like the ownership ones,
/// or a release moved to another title there would stay under both here. Only those
/// without a `deleted` column are (checked when pulled); the large tables -- `hashes`,
/// `content*`, provider caches -- are not in this list and are upserted as before.
const CATALOG_JUNCTIONS: [&str; 23] = [
    "title_releases",
    "release_sources",
    "release_contents",
    "release_content_sources",
    "release_contents_superseded",
    "release_bases",
    "release_provider_serials",
    "release_ratings",
    "title_aliases",
    "title_alt_names",
    "title_locales",
    "title_companies",
    "title_franchises",
    "title_genres",
    "title_keywords",
    "title_perspectives",
    "franchise_media",
    "keyword_media",
    "genre_raw_mapping",
    "igdb_game_relations",
    "title_bundle_games",
    "title_family_order",
    "title_families",
];

/// What the main server's sets hold, hard-deleted there too: a client's sets are the server's
/// (its own go with a catalogue replacement), so these are overwritten whole, and a release
/// taken out of a set on the server is out of it here as well.
const SET_MEMBERS: [&str; 2] = ["collection_set_entries", "collection_set_releases"];

pub(crate) struct Source {
    pub(crate) id: u16,
    pub(crate) name: String,
    pub(crate) base_url: String,
    pub(crate) token: Option<String>,
    pub(crate) role: String,
    pub(crate) peer: super::sources::Peer,
}

pub(crate) async fn load_source(app: &App, id: u16) -> Result<Source> {
    let row = sqlx::query("SELECT id, name, base_url, token, tls_fingerprint, role, enabled FROM remote_sources WHERE id = ? AND deleted = FALSE")
        .bind(id)
        .fetch_optional(&app.pool)
        .await?
        .ok_or_else(|| Error::not_found(format!("no source {id}")))?;
    if !row.try_get::<bool, _>("enabled").unwrap_or(false) {
        return Err(Error::invalid("that source is disabled"));
    }
    let id: u16 = row.try_get("id")?;
    let peer = super::sources::Peer::new(app, Some(id), &row.try_get::<String, _>("base_url")?, row.try_get("tls_fingerprint")?)?;
    Ok(Source {
        id,
        name: row.try_get("name")?,
        base_url: peer.base.clone(),
        token: super::sources::open_token(app, row.try_get("token")?)?,
        role: row.try_get("role")?,
        peer,
    })
}

pub(crate) fn request(_app: &App, source: &Source, url: &str) -> reqwest::RequestBuilder {
    let builder = source.peer.http.get(url);
    match &source.token {
        Some(token) if !token.is_empty() => builder.bearer_auth(token),
        _ => builder,
    }
}

/// Turns a non-200 into a message that says which of the three things went wrong -- wrong
/// token, wrong address, or a server that does not serve this at all -- because from a
/// client those look identical until you read the status.
async fn json<T: serde::de::DeserializeOwned>(response: reqwest::Response, what: &str) -> Result<T> {
    let status = response.status();
    if !status.is_success() {
        let hint = match status.as_u16() {
            401 | 403 => " -- check the token",
            404 => " -- that address does not serve this (is it sharing enabled, and the right port?)",
            _ => "",
        };
        return Err(Error::invalid(format!("{what}: the source answered {status}{hint}")));
    }
    response.json::<T>().await.map_err(|e| Error::invalid(format!("{what}: {e}")))
}

/// [`json`], also returning the body's size.
///
/// This is the **decoded** size: reqwest negotiates `Content-Encoding` and decompresses before
/// handing the body over, so what is counted here is JSON, not what crossed the wire. The wire
/// figure is roughly a quarter of it (measured: a 177,932-byte export page ships as 42,424
/// zstd), but reqwest does not expose the compressed length once it has decoded, and
/// reporting the decoded number as "received" would overstate what a metered connection was
/// charged for. So the message says what this actually is.
async fn json_sized<T: serde::de::DeserializeOwned>(response: reqwest::Response, what: &str) -> Result<(T, u64)> {
    let status = response.status();
    if !status.is_success() {
        let hint = match status.as_u16() {
            401 | 403 => " -- check the token",
            404 => " -- that address does not serve this (is it sharing enabled, and the right port?)",
            _ => "",
        };
        return Err(Error::invalid(format!("{what}: the source answered {status}{hint}")));
    }
    let body = response.bytes().await.map_err(|e| Error::invalid(format!("{what}: {e}")))?;
    let value = serde_json::from_slice(&body).map_err(|e| Error::invalid(format!("{what}: {e}")))?;
    Ok((value, body.len() as u64))
}

/// Pulls one source: its manifest, then whichever tables have changed since last time --
/// only their changed rows where the source numbers its changes.
///
/// `full` downloads every table whole, whatever the manifest says: the on-demand repair for
/// a mirror that has gone wrong, and never chosen here on the pull's own initiative.
pub async fn pull(app: &App, source_id: u16, full: bool, progress: &crate::database::jobs::Progress) -> Result<PullSummary> {
    let source = load_source(app, source_id).await?;
    let mut summary = PullSummary { source: source.name.clone(), role: source.role.clone(), ..Default::default() };
    // Only a source the user marked as the main server is trusted with catalogue rows, which
    // go into the ordinary shared tables rather than into a mirror. A share's catalogue is
    // declined even if it offers one -- and so is its art, whose junctions are catalogue ids.
    let take_catalog = source.role == "main";

    progress.force(0, None, &format!("{}: asking for the manifest", source.name)).await;
    let manifest: Manifest = json(
        request(app, &source, &format!("{}/api/v1/sync/manifest", source.base_url)).send().await.map_err(|e| Error::invalid(format!("{}: {e}", source.name)))?,
        "manifest",
    )
    .await?;
    // The first connection worked: from now on only this certificate is trusted.
    source.peer.remember(app).await?;

    // What we already hold, per table, so an unchanged table costs nothing.
    let known: std::collections::HashMap<String, Known> = sqlx::query("SELECT table_name, source_hash, sync_epoch, sync_rev FROM remote_sync_state WHERE source_id = ?")
        .bind(source.id)
        .fetch_all(&app.pool)
        .await?
        .iter()
        .filter_map(|r| {
            Some((
                r.try_get::<String, _>("table_name").ok()?,
                Known { hash: r.try_get("source_hash").ok()?, epoch: r.try_get("sync_epoch").ok().flatten(), rev: r.try_get("sync_rev").ok().flatten() },
            ))
        })
        .collect();

    // Can this machine's catalogue ids be compared with the source's at all? Ownership rows
    // carry the source's `release_id`/`content_id` unchanged, so mirroring across two
    // independently-built catalogues does not error -- it attaches the source's games to
    // whatever unrelated rows hold those ids here. Refuse rather than corrupt.
    let ours = crate::database::settings::instance_id(&app.pool).await?;
    let our_catalog_from = crate::database::settings::get::<String>(&app.pool, crate::database::settings::KEY_CATALOG_FROM).await?.filter(|s| !s.is_empty());
    summary.source_instance = manifest.instance_id.clone();
    if !lineage_ok(&ours, our_catalog_from.as_deref(), &manifest.instance_id, manifest.catalog_from.as_deref(), take_catalog) {
        return Err(Error::invalid(format!(
            "{}'s catalogue does not share a lineage with this one, so its row ids mean something different here. \
             Ownership can only be mirrored between machines that took their catalogue from the same main server \
             (this machine's came from {}; that one's came from {}). Sync that machine from the same main server first.",
            source.name,
            our_catalog_from.as_deref().unwrap_or("nowhere -- it was imported locally"),
            manifest.catalog_from.as_deref().unwrap_or("nowhere -- it was imported locally"),
        )));
    }

    // A hash match means "nothing changed at the source", not "we have it". If a table is
    // empty here while the source says it has rows, something removed it locally -- and
    // trusting the recorded hash then leaves it empty forever, because every later sync also
    // sees no change. One cheap estimate query for every table guards against that.
    //
    // Asked as `SELECT 1 ... LIMIT 1` per table rather than from `information_schema`: that
    // view's row counts are optimizer estimates and lag badly right after a mass delete --
    // which is precisely when this check matters, since a catalogue replacement is what
    // empties the tables. One index seek per table is exact and costs nothing.
    let mut is_empty = Vec::with_capacity(manifest.tables.len());
    for state in &manifest.tables {
        // A mirror table is shared by every source, so "is it empty" has to mean "empty *for
        // this source*" -- otherwise another peer's rows make it look populated and the main
        // server's missing half is never noticed.
        let any: Option<i64> = if state.group == GROUP_CATALOG {
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT 1 FROM `{}` LIMIT 1", state.table))).fetch_optional(&app.pool).await.unwrap_or(None)
        } else {
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT 1 FROM `{}` WHERE source_id = ? LIMIT 1", remote_name(&state.table))))
                .bind(source.id)
                .fetch_optional(&app.pool)
                .await
                .unwrap_or(None)
        };
        is_empty.push(any.is_none());
    }

    // Where a table's next position comes from: the source's settled change number, under its
    // epoch. A source too old to number its changes gives neither, and every pull of a changed
    // table is then a full one, as it always was.
    let position = |state: &TableState| -> Option<(String, u64)> {
        let epoch = manifest.sync_epoch.clone().filter(|_| state.tracked)?;
        Some((epoch, manifest.stable_rev?))
    };

    let mut todo = Vec::new();
    for (i, state) in manifest.tables.iter().enumerate() {
        if (state.group == GROUP_CATALOG || state.group == GROUP_MEDIA) && !take_catalog {
            summary.declined_catalog.push(state.table.clone());
            continue;
        }
        let held = known.get(&state.table);
        // An empty hash means the source could not compute one; re-download rather than
        // assume nothing changed.
        let looks_empty = state.rows > 0 && is_empty.get(i).copied().unwrap_or(false);
        if !full && !state.hash.is_empty() && held.is_some_and(|k| k.hash == state.hash) && !looks_empty {
            summary.unchanged.push(state.table.clone());
            // Nothing moved since we last took it, so we hold everything up to the settled
            // number too. Recording that is what lets a table pulled in full by an older
            // build take only its changes next time, without a full pull first.
            if let Some((epoch, rev)) = position(state) {
                sqlx::query("UPDATE remote_sync_state SET sync_epoch = ?, sync_rev = ? WHERE source_id = ? AND table_name = ?")
                    .bind(&epoch)
                    .bind(rev)
                    .bind(source.id)
                    .bind(&state.table)
                    .execute(&app.pool)
                    .await?;
            }
            continue;
        }
        // Only the changes, when we know where we got to and the source can still say what
        // changed since: same epoch, and no deletes pruned past our position.
        let since = held.filter(|_| !full).and_then(|k| {
            let same_epoch = state.tracked && k.epoch.is_some() && k.epoch == manifest.sync_epoch;
            k.rev.filter(|rev| same_epoch && !looks_empty && *rev >= manifest.pruned_through)
        });
        todo.push((state.clone(), since));
    }

    let mut conn = app.pool.acquire().await?;
    // A mirror row may reference catalogue rows that arrive later in this same pull (or not
    // at all, from a share); the sweep at the end is what makes the result consistent.
    // Closed rather than returned to the pool if this is cut short (a cancel, a dropped
    // request): FOREIGN_KEY_CHECKS is per connection and must never be left off on one.
    conn.close_on_drop();
    sqlx::query("SET FOREIGN_KEY_CHECKS = 0").execute(&mut *conn).await?;

    // The manifest already carries a row count per table, so the size of the whole job is
    // known before a single row is downloaded -- there is no need for a separate "how big is
    // this" call. Reporting against that total makes the bar a real fraction of the work
    // instead of "table 7 of 48", which sits still for minutes on a table like `hashes`.
    // A full table's size is known up front from the manifest; a table pulled as changes adds
    // what it brings as it brings it, so the bar is exact for full pulls and grows for diffs.
    let total_rows: u64 = todo.iter().filter(|(_, since)| since.is_none()).map(|(t, _)| t.rows).sum();
    let mut rows_done = 0u64;
    let mut bytes_done = 0u64;
    let total = todo.len() as u64;
    // Wrapped so that no error path can return this connection to the pool with foreign-key
    // checks still off, which would silently disable them for whatever query picked it up
    // next. The same reason `restore` and the SQLite import do it.
    let result = async {
    for (i, (state, since)) in todo.iter().enumerate() {
        let target = if state.group == GROUP_CATALOG { state.table.clone() } else { remote_name(&state.table) };
        let mirror = state.group != GROUP_CATALOG;
        let extra = if mirror { vec![("source_id".to_string(), source.id.to_string())] } else { Vec::new() };
        let writer = super::rows::Writer::new(app, &target, extra).await?;
        let report = |rows: u64, rows_done: u64, bytes_done: u64, total_rows: u64, how: &str| {
            let message = format!(
                "{}: table {} of {total} ({}{how}) {rows}/{} -- {rows_done}/{total_rows} rows overall, {} of JSON (compressed in transit)",
                source.name,
                i + 1,
                state.table,
                state.rows,
                crate::database::jobs::human_bytes(bytes_done)
            );
            async move { progress.report(rows_done, Some(total_rows.max(rows_done).max(1)), &message).await }
        };

        let mut rows = 0u64;
        let mut changes_only = false;
        if let Some(since) = *since {
            // Deletes first, then the changed rows: a row deleted and inserted again under the
            // same key is exported as it is now, so applying the delete after it would lose it.
            let mut after: Option<u64> = None;
            loop {
                let mut url = format!("{}/api/v1/sync/deleted/{}?since={since}&limit=10000", source.base_url, state.table);
                if let Some(cursor) = after {
                    url.push_str(&format!("&after={cursor}"));
                }
                let (page, bytes): (DeletedPage, u64) = json_sized(request(app, &source, &url).send().await.map_err(|e| Error::invalid(format!("{}: {e}", state.table)))?, &state.table).await?;
                let n = delete_keys(&mut conn, &target, mirror.then_some(source.id), &page.key, &page.rows).await?;
                rows += n;
                rows_done += n;
                bytes_done += bytes;
                report(rows, rows_done, bytes_done, total_rows, ", changes").await;
                match page.next {
                    Some(cursor) => after = Some(cursor),
                    None => break,
                }
            }
            let mut after: Option<u64> = None;
            loop {
                let mut url = format!("{}/api/v1/sync/export/{}?since={since}&limit=2000", source.base_url, state.table);
                if let Some(cursor) = after {
                    url.push_str(&format!("&after={}", urlencode(&serde_json::to_string(&[cursor.to_string()]).unwrap_or_default())));
                }
                let (page, bytes): (ExportPage, u64) = json_sized(request(app, &source, &url).send().await.map_err(|e| Error::invalid(format!("{}: {e}", state.table)))?, &state.table).await?;
                writer.write(&mut conn, &page.rows).await?;
                rows += page.rows.len() as u64;
                rows_done += page.rows.len() as u64;
                bytes_done += bytes;
                report(rows, rows_done, bytes_done, total_rows, ", changes").await;
                match page.next.as_deref() {
                    Some([cursor]) => after = Some(cursor.parse().map_err(|_| Error::invalid(format!("{}: bad change cursor {cursor:?}", state.table)))?),
                    Some(_) => return Err(Error::invalid(format!("{}: bad change cursor", state.table))),
                    None => break,
                }
            }
            // The diff can only miss rows if something went wrong on the way -- a change the
            // source made without its triggers, say. Fewer rows here than there is the sign.
            // It is reported, not repaired: a full sync is the user's call.
            changes_only = true;
            let held = count_held(&mut conn, &target, mirror.then_some(source.id)).await?;
            if held < state.rows {
                log::warn!("sync: {} holds {held} row(s) after taking the changes, the source has {}; a full sync will repair it", state.table, state.rows);
                summary.incomplete.push(state.table.clone());
            }
        }

        if !changes_only {
            let junction = (state.group == GROUP_OWNERSHIP && OWNERSHIP_JUNCTIONS.contains(&state.table.as_str()))
                || (state.group == GROUP_MEDIA && MEDIA_JUNCTIONS.contains(&state.table.as_str()));
            // A table that is not emptied first keeps whatever the source no longer has unless
            // it is reconciled: the keys this pull receives are remembered (as hashes, so a
            // ten-million-row `hashes` costs ~80 MB, not gigabytes), and every row here whose
            // key was not among them goes once the pull ends. Nothing to do for an empty one.
            let key = mirror_key(&mut conn, &target).await?;
            let mut seen: Option<Vec<u64>> = if count_held(&mut conn, &target, mirror.then_some(source.id)).await? > 0 { Some(Vec::new()) } else { None };
            if junction {
                seen = None;
                sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM `{target}` WHERE source_id = ?"))).bind(source.id).execute(&mut *conn).await?;
            }
            if state.group == GROUP_CATALOG && SET_MEMBERS.contains(&state.table.as_str()) {
                seen = None;
                sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM `{target}`"))).execute(&mut *conn).await?;
            }
            if state.group == GROUP_CATALOG && CATALOG_JUNCTIONS.contains(&state.table.as_str()) {
                let soft: Option<i64> = sqlx::query_scalar("SELECT 1 FROM information_schema.columns WHERE table_schema = DATABASE() AND table_name = ? AND column_name = 'deleted' LIMIT 1")
                    .bind(&target)
                    .fetch_optional(&mut *conn)
                    .await?;
                if soft.is_none() {
                    seen = None;
                    sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM `{target}`"))).execute(&mut *conn).await?;
                }
            }

            let mut after: Option<Vec<String>> = None;
            loop {
                let mut url = format!("{}/api/v1/sync/export/{}?limit=2000", source.base_url, state.table);
                if let Some(cursor) = &after {
                    // One opaque JSON parameter -- see `ExportQuery` in the server for why this
                    // is not a repeated `after=`.
                    url.push_str(&format!("&after={}", urlencode(&serde_json::to_string(cursor).unwrap_or_default())));
                }
                let (page, bytes): (ExportPage, u64) = json_sized(
                    request(app, &source, &url).send().await.map_err(|e| Error::invalid(format!("{}: {e}", state.table)))?,
                    &state.table,
                )
                .await?;
                writer.write(&mut conn, &page.rows).await?;
                if let Some(seen) = seen.as_mut() {
                    seen.extend(page.rows.iter().map(|row| key_hash(key.iter().map(|k| key_text(row.get(k))))));
                }
                rows += page.rows.len() as u64;
                rows_done += page.rows.len() as u64;
                bytes_done += bytes;
                report(rows, rows_done, bytes_done, total_rows, "").await;
                match page.next {
                    Some(cursor) => after = Some(cursor),
                    None => break,
                }
            }
            if let Some(mut seen) = seen {
                seen.sort_unstable();
                let gone = drop_unseen(&mut conn, &target, mirror.then_some(source.id), &key, &seen).await?;
                if gone > 0 {
                    log::info!("sync: {target}: removed {gone} row(s) the source no longer has");
                }
            }
        }

        // The new position. A diff with no settled number from the source keeps the old one,
        // so the rows above it are asked for again next time rather than possibly missed. A
        // full pull with none starts from 0, which still beats another full pull.
        let (epoch, rev) = match position(state) {
            Some((epoch, rev)) => (Some(epoch), Some(rev)),
            None if state.tracked && manifest.sync_epoch.is_some() => (manifest.sync_epoch.clone(), Some(if changes_only { since.unwrap_or(0) } else { 0 })),
            None => (None, None),
        };
        sqlx::query(
            "INSERT INTO remote_sync_state (source_id, table_name, source_hash, row_count, generated_at, synced_at, sync_epoch, sync_rev) VALUES (?, ?, ?, ?, ?, NOW(), ?, ?) \
             ON DUPLICATE KEY UPDATE source_hash = VALUES(source_hash), row_count = VALUES(row_count), generated_at = VALUES(generated_at), synced_at = NOW(), \
             sync_epoch = VALUES(sync_epoch), sync_rev = VALUES(sync_rev)",
        )
        .bind(source.id)
        .bind(&state.table)
        .bind(&state.hash)
        .bind(if changes_only { state.rows } else { rows })
        .bind(manifest.generated_at)
        .bind(epoch)
        .bind(rev)
        .execute(&mut *conn)
        .await?;
        if changes_only {
            summary.changes_only.push(target.clone());
        }
        summary.tables.push((target, rows));
    }
    // Releases the source deleted (a merge folds one into another) are gone here now too.
    // This machine's own games linked to them are unlinked, so `relink_unlinked` below finds
    // them and a hash match puts them on the release that took over.
    if summary.tables.iter().any(|(t, _)| t == "release") {
        let unlinked = sqlx::query("DELETE lrg FROM local_release_games lrg LEFT JOIN `release` r ON r.id = lrg.release_id WHERE r.id IS NULL").execute(&mut *conn).await?.rows_affected();
        if unlinked > 0 {
            log::info!("sync: unlinked {unlinked} local game(s) from releases the source removed");
        }
    }
    summary.unresolved = sweep_unresolved(&mut conn, source.id).await?;
    Ok::<(), Error>(())
    }
    .await;

    sqlx::query("SET FOREIGN_KEY_CHECKS = 1").execute(&mut *conn).await?;
    result?;
    let took_catalog = take_catalog && summary.tables.iter().any(|(t, _)| catalog_tables().contains(&t.as_str()));
    // Having taken the catalogue from this source, this machine's ids are now its ids -- which
    // is exactly what a later peer sync needs to know.
    if take_catalog && !manifest.instance_id.is_empty() {
        crate::database::settings::set(&app.pool, crate::database::settings::KEY_CATALOG_FROM, &manifest.instance_id).await?;
    }
    sqlx::query("UPDATE remote_sources SET last_sync_at = NOW() WHERE id = ?").bind(source.id).execute(&mut *conn).await?;
    if took_catalog {
        summary.relink_jobs = relink_unlinked(app).await?;
        let (relinked, unresolved) = relink_media(app).await?;
        summary.media_relinked = relinked;
        summary.media_unresolved = unresolved;
    }
    // New art on the server: fetch it, unless the user would rather press the button. Queued
    // rather than done here so the pull stays the quick step and the downloads get their own
    // progress bar -- and `enqueue_unless_queued` so a schedule firing every few minutes
    // never stacks them.
    let media_changed = summary.tables.iter().any(|(t, _)| t == "remote_media" || t == "remote_title_media" || t == "remote_release_media");
    if take_catalog && media_changed && crate::database::settings::media_sync_after_pull(&app.pool).await? {
        let request = super::jobs::JobRequest::MediaSync(super::jobs::MediaSyncRequest { source_id: Some(source.id) });
        summary.media_sync_job = super::jobs::enqueue_unless_queued(app, &request).await?;
    }
    // Kept audio follows the catalogue the same way: tags decide what is fetched, and they
    // arrive with a pull. Only where the user asked for copies (bgm under media_client_keep).
    if take_catalog && source.role == "main" && crate::database::settings::MediaRules::load(&app.pool).await?.keeps("bgm") {
        super::jobs::enqueue_unless_queued(app, &super::jobs::JobRequest::AudioSync).await?;
    }
    progress
        .force(total, Some(total.max(1)), &format!("{}: {} table(s), {} unchanged, {} unresolved row(s)", source.name, summary.tables.len(), summary.unchanged.len(), summary.unresolved))
        .await;
    Ok(summary)
}

/// What this machine recorded about one source table at its last pull.
struct Known {
    hash: String,
    epoch: Option<String>,
    rev: Option<u64>,
}

/// Deletes the rows a source reported deleted. `key` names the source table's primary key;
/// the mirror tables use the same column names, plus `source_id`. The names come from the
/// source, so they are checked against this table's own columns before going into SQL.
async fn delete_keys(conn: &mut sqlx::MySqlConnection, target: &str, source_id: Option<u16>, key: &[String], rows: &[Vec<String>]) -> Result<u64> {
    if rows.is_empty() {
        return Ok(0);
    }
    let columns: Vec<String> = sqlx::query_scalar("SELECT COLUMN_NAME FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ?").bind(target).fetch_all(&mut *conn).await?;
    if key.is_empty() || key.iter().any(|k| !columns.contains(k)) {
        return Err(Error::invalid(format!("{target}: the source's key {key:?} does not match this table")));
    }
    let tuple = format!("({})", vec!["?"; key.len()].join(", "));
    let names = key.iter().map(|c| format!("`{c}`")).collect::<Vec<_>>().join(", ");
    let mut deleted = 0u64;
    for chunk in rows.chunks((10_000 / key.len()).max(1)) {
        let list = vec![tuple.as_str(); chunk.len()].join(", ");
        let scope = if source_id.is_some() { "source_id = ? AND " } else { "" };
        let mut query = sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM `{target}` WHERE {scope}({names}) IN ({list})")));
        if let Some(id) = source_id {
            query = query.bind(id);
        }
        for row in chunk {
            for value in row {
                query = query.bind(value);
            }
        }
        query.execute(&mut *conn).await?;
        // Counted as reported rather than as affected: a delete for a row this machine
        // never had is still a change the source sent.
        deleted += chunk.len() as u64;
    }
    Ok(deleted)
}

/// A target table's primary key without `source_id`: the source's own key, in key order.
async fn mirror_key(conn: &mut sqlx::MySqlConnection, target: &str) -> Result<Vec<String>> {
    let key: Vec<String> = sqlx::query_scalar("SELECT COLUMN_NAME FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? AND INDEX_NAME = 'PRIMARY' ORDER BY SEQ_IN_INDEX")
        .bind(target)
        .fetch_all(&mut *conn)
        .await?;
    Ok(key.into_iter().filter(|c| c != "source_id").collect())
}

/// One key's values as text, the way both an export row and `CAST(... AS CHAR)` give them.
fn key_text(value: Option<&serde_json::Value>) -> String {
    match value {
        None | Some(serde_json::Value::Null) => String::new(),
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

fn key_hash<S: AsRef<str>>(values: impl IntoIterator<Item = S>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for value in values {
        value.as_ref().hash(&mut hasher);
    }
    hasher.finish()
}

/// Deletes the rows of `target` (this source's, for a mirror) whose key hash is not in
/// `seen`, which must be sorted. Pages through this machine's keys rather than loading them.
async fn drop_unseen(conn: &mut sqlx::MySqlConnection, target: &str, source_id: Option<u16>, key: &[String], seen: &[u64]) -> Result<u64> {
    if key.is_empty() {
        return Ok(0);
    }
    let order = key.iter().map(|c| format!("`{c}`")).collect::<Vec<_>>().join(", ");
    let select = key.iter().enumerate().map(|(i, c)| format!("CAST(`{c}` AS CHAR) AS `k{i}`")).collect::<Vec<_>>().join(", ");
    let scope = if source_id.is_some() { "source_id = ?" } else { "TRUE" };
    let tuple = format!("({})", vec!["?"; key.len()].join(", "));
    let mut after: Option<Vec<String>> = None;
    let mut gone: Vec<Vec<String>> = Vec::new();
    loop {
        let cursor = if after.is_some() { format!(" AND ({order}) > {tuple}") } else { String::new() };
        let mut query = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT {select} FROM `{target}` WHERE {scope}{cursor} ORDER BY {order} LIMIT 5000")));
        if let Some(id) = source_id {
            query = query.bind(id);
        }
        if let Some(values) = &after {
            for value in values {
                query = query.bind(value);
            }
        }
        let rows = query.fetch_all(&mut *conn).await?;
        for row in &rows {
            let values: Vec<String> = (0..key.len()).map(|i| row.try_get::<Option<String>, _>(i).ok().flatten().unwrap_or_default()).collect();
            if seen.binary_search(&key_hash(&values)).is_err() {
                gone.push(values.clone());
            }
            after = Some(values);
        }
        if rows.len() < 5000 {
            break;
        }
    }
    delete_keys(conn, target, source_id, key, &gone).await
}

/// How many rows this machine holds of a table, for this source when it is a mirror.
async fn count_held(conn: &mut sqlx::MySqlConnection, target: &str, source_id: Option<u16>) -> Result<u64> {
    let n: i64 = match source_id {
        Some(id) => sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM `{target}` WHERE source_id = ?"))).bind(id).fetch_one(&mut *conn).await?,
        None => sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM `{target}`"))).fetch_one(&mut *conn).await?,
    };
    Ok(n.max(0) as u64)
}

/// Percent-encodes a cursor value for a query string. Cursors are primary-key values -- ids,
/// paths, hashes -- so `&`, `=`, `+`, `#` and spaces all really do occur.
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(*byte as char),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Drops mirror rows whose catalogue rows this machine does not have.
///
/// `remote_files.content_id` is NOT NULL, so such a row cannot be represented at all -- it is
/// not a matter of leaving a column blank. Children are removed before their parents, and the
/// one nullable reference (`remote_game.playlist_file_id`) is blanked rather than taking the
/// game with it.
async fn sweep_unresolved(conn: &mut sqlx::MySqlConnection, source_id: u16) -> Result<u64> {
    let mut dropped = 0u64;
    for sql in [
        "DELETE rrg FROM remote_release_games rrg LEFT JOIN `release` r ON r.id = rrg.release_id WHERE rrg.source_id = ? AND r.id IS NULL",
        "DELETE rrr FROM remote_rom_rahash rrr LEFT JOIN hashes h ON h.id = rrr.hash_id WHERE rrr.source_id = ? AND h.id IS NULL",
        "DELETE rrf FROM remote_rom_files rrf JOIN remote_files f ON f.source_id = rrf.source_id AND f.id = rrf.file_id \
         LEFT JOIN content c ON c.id = f.content_id WHERE rrf.source_id = ? AND c.id IS NULL",
        // Art attached to a title or release this machine does not have cannot be placed.
        "DELETE rtm FROM remote_title_media rtm LEFT JOIN title t ON t.id = rtm.title_id WHERE rtm.source_id = ? AND t.id IS NULL",
        "DELETE rrm FROM remote_release_media rrm LEFT JOIN `release` r ON r.id = rrm.release_id WHERE rrm.source_id = ? AND r.id IS NULL",
    ] {
        dropped += sqlx::query(sqlx::AssertSqlSafe(sql.to_string())).bind(source_id).execute(&mut *conn).await?.rows_affected();
    }
    sqlx::query(
        "UPDATE remote_game g LEFT JOIN remote_files f ON f.source_id = g.source_id AND f.id = g.playlist_file_id \
         SET g.playlist_file_id = NULL WHERE g.source_id = ? AND g.playlist_file_id IS NOT NULL AND f.id IS NULL",
    )
    .bind(source_id)
    .execute(&mut *conn)
    .await?;
    dropped += sqlx::query("DELETE f FROM remote_files f LEFT JOIN content c ON c.id = f.content_id WHERE f.source_id = ? AND c.id IS NULL")
        .bind(source_id)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    Ok(dropped)
}

#[cfg(test)]
mod lineage_tests {
    use super::lineage_ok;

    const ME: &str = "me";
    const MAIN: &str = "main";
    const OTHER: &str = "other";

    /// Art follows the catalogue: a main server offers it, a share does not, and it lands in
    /// a `remote_` mirror like ownership rather than in this machine's own `media`.
    #[test]
    fn art_is_offered_with_the_catalogue_and_mirrored_like_ownership() {
        use super::{group_of, remote_name, GROUP_MEDIA, MEDIA_MIRRORED};
        for table in MEDIA_MIRRORED {
            assert_eq!(group_of(table, true), Some(GROUP_MEDIA), "{table}");
            assert_eq!(group_of(table, false), None, "{table} on a share");
        }
        assert_eq!(remote_name("media"), "remote_media");
        assert_eq!(remote_name("title_media"), "remote_title_media");
        assert!(!super::super::backup::CATALOG_TABLES.contains(&"media"), "media is per machine, never a catalogue table");
    }

    #[test]
    fn a_main_server_is_always_acceptable() {
        // Its catalogue is about to become this machine's, so its ids become ours.
        assert!(lineage_ok(ME, None, MAIN, None, true));
    }

    #[test]
    fn two_clients_of_the_same_main_may_mirror_each_other() {
        assert!(lineage_ok(ME, Some(MAIN), "peer", Some(MAIN), false));
    }

    #[test]
    fn my_own_main_server_may_be_mirrored_as_a_peer_too() {
        assert!(lineage_ok(ME, Some(MAIN), MAIN, None, false));
    }

    #[test]
    fn a_machine_that_synced_from_me_may_be_mirrored() {
        assert!(lineage_ok(ME, None, "client", Some(ME), false));
    }

    #[test]
    fn independently_built_catalogues_are_refused() {
        // The case the fixture caught: both imported the same DAT themselves, so the same
        // game is a different id on each, and mirroring silently mis-links every row.
        assert!(!lineage_ok(ME, None, OTHER, None, false));
        assert!(!lineage_ok(ME, Some(MAIN), OTHER, Some(OTHER), false));
        assert!(!lineage_ok(ME, Some(MAIN), OTHER, None, false));
    }

    #[test]
    fn a_source_that_will_not_say_who_it_is_is_refused() {
        assert!(!lineage_ok(ME, Some(MAIN), "", Some(MAIN), false));
    }
}

/// Queues a DAT hash match for every directory holding games that aren't linked to a release.
///
/// Called after a catalogue sync, and it is what makes replacing a catalogue a single step
/// rather than two. `sources::clear_catalog` drops `local_release_games` -- it has to, because
/// those rows point into the catalogue by id and the ids are about to mean something else --
/// so every local game comes out of it unlinked. Re-linking is a pure content-hash lookup, the
/// files have not changed, and nothing about it needs a decision from the user; asking them to
/// remember to run it by hand would just be a step to forget.
///
/// It also covers the ordinary case: the main server imported a DAT that finally describes a
/// file you already had, and the next sync links it without being asked.
///
/// Cheap when there is nothing to do -- one query returning no rows -- and `to_dat` only, so
/// it never reaches the network.
pub async fn relink_unlinked(app: &App) -> Result<Vec<u64>> {
    let rows = sqlx::query(
        "SELECT DISTINCT f.directory_id AS id FROM local_game g \
           JOIN local_game_roms lgr ON lgr.game_id = g.id \
           JOIN local_rom_files lrf ON lrf.rom_id = lgr.rom_id \
           JOIN local_files f ON f.id = lrf.file_id AND f.deleted = FALSE \
         WHERE g.deleted = FALSE AND f.directory_id IS NOT NULL \
           AND NOT EXISTS (SELECT 1 FROM local_release_games lrg WHERE lrg.game_id = g.id)",
    )
    .fetch_all(&app.pool)
    .await?;

    let mut queued = Vec::new();
    for row in &rows {
        let Ok(directory_id) = row.try_get::<u64, _>("id") else { continue };
        let request = super::jobs::JobRequest::HashMatch(super::jobs::HashMatchRequest {
            directory_id,
            to_dat: true,
            // Deliberately offline: this is a re-link against the catalogue that just arrived,
            // not an excuse to start querying providers.
            to_hasheous: false,
            to_playmatch: false,
            all: false,
        });
        match super::jobs::enqueue_unless_queued(app, &request).await {
            Ok(Some(id)) => queued.push(id),
            Ok(None) => {}
            Err(e) => log::warn!("sync: could not queue the re-link for directory {directory_id}: {e}"),
        }
    }
    Ok(queued)
}

/// Rebuilds `title_media` / `release_media` after a catalogue replacement renumbered every
/// title and release.
///
/// A catalogue swap makes `title_media.title_id` mean a different game, so
/// [`super::sources::clear_catalog`] drops those junctions and records what each media row was
/// attached to in `media_relink`. This puts them back, resolving through the *freshly synced*
/// provider links: "which title does IGDB call 1234?" is a question whose answer came from the
/// server, so the id it yields is the server's.
///
/// Returns `(relinked, unresolved)`. Unresolved means the media row has no `provider_id` --
/// a manual upload, or anything downloaded before migration 0023 -- and is reported rather
/// than guessed at. Those files stay on disk and `media_download` can re-fetch them.
pub async fn relink_media(app: &App) -> Result<(u64, u64)> {
    let mut relinked = 0u64;
    // Set-based rather than row-by-row: one statement per junction, joining the recorded
    // scope to the provider links the sync just brought in.
    relinked += sqlx::query(
        "INSERT IGNORE INTO title_media (title_id, media_id) \
         SELECT tpl.title_id, m.id FROM media_relink mr \
           JOIN media m ON m.id = mr.media_id AND m.deleted = FALSE AND m.provider_id IS NOT NULL \
           JOIN title_provider_links tpl ON tpl.provider = m.source AND tpl.provider_id = m.provider_id AND tpl.deleted = FALSE \
         WHERE mr.kind = 'title'",
    )
    .execute(&app.pool)
    .await?
    .rows_affected();
    relinked += sqlx::query(
        "INSERT IGNORE INTO release_media (release_id, media_id) \
         SELECT rpl.release_id, m.id FROM media_relink mr \
           JOIN media m ON m.id = mr.media_id AND m.deleted = FALSE AND m.provider_id IS NOT NULL \
           JOIN release_provider_links rpl ON rpl.provider = m.source AND rpl.provider_id = m.provider_id AND rpl.deleted = FALSE \
         WHERE mr.kind = 'release'",
    )
    .execute(&app.pool)
    .await?
    .rows_affected();

    let unresolved: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT mr.media_id) FROM media_relink mr \
           JOIN media m ON m.id = mr.media_id AND m.deleted = FALSE \
         WHERE m.provider_id IS NULL",
    )
    .fetch_one(&app.pool)
    .await?;

    // Scratch only: whatever could be rebuilt has been, and leaving rows behind would make a
    // later replacement resurrect links from the one before it.
    sqlx::query("DELETE FROM media_relink").execute(&app.pool).await?;
    Ok((relinked, unresolved as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_replaced_catalogue_junction_is_a_catalogue_table() {
        for table in CATALOG_JUNCTIONS {
            assert!(catalog_tables().contains(&table), "{table}");
        }
    }
}
