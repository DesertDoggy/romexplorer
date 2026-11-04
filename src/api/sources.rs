//! The servers and peers this machine syncs from (`remote_sources`).
//!
//! This *is* the sync configuration: address, token and role live here as rows rather than as
//! settings keys, because there are several of them ([`docs/plan/api.md`], §2.6). One may be
//! the `main` server -- the one "not backed up yet" is measured against, and the only one
//! this machine will accept catalogue rows from -- and the rest are `peer` shares.
//!
//! **A token never comes back out.** It goes in on a create or an update and is reported
//! afterwards only as `has_token`. A peer's bearer token is a credential for another machine,
//! and listing sources is something every screen does; there is no reason for it to travel to
//! a browser, and a console left open would otherwise show it in plain text.

use sqlx::Row;

use super::{App, Error, Result};

// Shared with the GUI through `romexplorer_types`.
pub use romexplorer_types::sources::{MediaOnReplace, ModeBody, ModeView, Reachability, RemoteSource, SourceSpec, SourceTable};

/// Whether designating this source as `main` would destroy a locally-built catalogue, and
/// the sentence to put in front of the user if so.
///
/// Catalogue row ids are per-machine AUTO_INCREMENT values and the mirror copies them
/// verbatim, so two machines can only exchange ownership if one took its catalogue from the
/// other. There is no merge: the same game is a different id on each side, and nothing in a
/// row says which. So designating a main server means adopting *its* numbering, and anything
/// this machine imported itself has to go.
///
/// Only asked when there is something to lose: an empty catalogue, or one already taken from
/// this same main, needs no confirmation.
pub async fn catalog_conflict(app: &App, base_url: &str) -> Result<Option<String>> {
    let releases: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM `release`").fetch_one(&app.pool).await?;
    if releases == 0 {
        return Ok(None);
    }
    let from = crate::database::settings::get::<String>(&app.pool, crate::database::settings::KEY_CATALOG_FROM).await?.filter(|s| !s.is_empty());
    // Already syncing a catalogue from somewhere: re-pointing at the same main is a no-op,
    // and at a different one is a change the same warning covers.
    let existing_main: Option<String> = sqlx::query_scalar("SELECT base_url FROM remote_sources WHERE role = 'main' AND deleted = FALSE")
        .fetch_optional(&app.pool)
        .await?;
    if from.is_some() && existing_main.as_deref() == Some(base_url) {
        return Ok(None);
    }
    let titles: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM title").fetch_one(&app.pool).await.unwrap_or(0);
    Ok(Some(format!(
        "This machine already has a catalogue of its own ({releases} releases, {titles} titles). \
         Setting a main server replaces it with that server's catalogue -- the two cannot be merged, because \
         each machine numbers its own rows and the same game has a different id on each.\n\n{}",
        losses(app).await?
    )))
}

/// What a replacement costs, counted rather than described in the abstract.
///
/// Every one of these is a human decision that only exists here, and after the swap the main
/// server's version takes its place. Saying "some curation may be lost" invites a shrug;
/// saying "40 manually ordered releases" does not, and the numbers are cheap -- one indexed
/// `COUNT(*)` each.
pub async fn losses(app: &App) -> Result<String> {
    async fn count(app: &App, sql: &str) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string())).fetch_one(&app.pool).await.unwrap_or(0)
    }
    let tags = count(app, "SELECT COUNT(*) FROM title_keywords WHERE source = 'user'").await;
    let blocks = count(app, "SELECT COUNT(*) FROM title_merge_blocks").await;
    let dismissed = count(app, "SELECT COUNT(*) FROM title_merge_suggestions WHERE status <> 'pending'").await;
    let manual_links = count(
        app,
        "SELECT (SELECT COUNT(*) FROM title_provider_links WHERE confidence = 'manual' OR deleted = TRUE) + (SELECT COUNT(*) FROM release_provider_links WHERE confidence = 'manual' OR deleted = TRUE)",
    )
    .await;
    let ordered = count(app, "SELECT COUNT(*) FROM title_releases WHERE sort_set_at IS NOT NULL").await;
    let sets = count(app, "SELECT COUNT(*) FROM collection_sets WHERE deleted = FALSE").await;
    let art = count(
        app,
        "SELECT COUNT(DISTINCT m.id) FROM media m WHERE m.deleted = FALSE AND (EXISTS (SELECT 1 FROM title_media t WHERE t.media_id = m.id) OR EXISTS (SELECT 1 FROM release_media r WHERE r.media_id = m.id))",
    )
    .await;
    let orphan_art = count(
        app,
        "SELECT COUNT(DISTINCT m.id) FROM media m WHERE m.deleted = FALSE AND m.provider_id IS NULL AND (EXISTS (SELECT 1 FROM title_media t WHERE t.media_id = m.id) OR EXISTS (SELECT 1 FROM release_media r WHERE r.media_id = m.id))",
    )
    .await;

    let mut lost = Vec::new();
    for (n, what) in [
        (tags, "of your own keyword tags"),
        (blocks, "never-merge rules"),
        (dismissed, "dismissed merge suggestions"),
        (manual_links, "manual provider links"),
        (ordered, "manually ordered releases"),
        (sets, "collection sets"),
    ] {
        if n > 0 {
            lost.push(format!("{n} {what}"));
        }
    }
    let curation = if lost.is_empty() {
        "There is no hand-made curation here to lose.".to_string()
    } else {
        format!("This also deletes curation that only exists here, and takes the main server's instead: {}. Your ROMs are not touched.", lost.join(", "))
    };
    if art == 0 {
        return Ok(curation);
    }
    Ok(format!(
        "{curation}\n\n{art} art file(s) are attached to titles that are about to be renumbered. Choose one:\n\
         - keep and re-link (media = relink): free and instant{}.\n\
         - delete and re-download (media = redownload): the rows are retired now and the art comes back on the next provider sync -- complete, but it spends provider quota, and anything they no longer serve is gone.",
        if orphan_art > 0 {
            format!(", but {orphan_art} of them predate provider tracking and cannot be re-linked -- they stay on disk, unlinked, and can be re-downloaded later")
        } else {
            String::new()
        }
    ))
}

/// Throws away this machine's catalogue so the main server's can take its place.
///
/// `local_release_games` goes too -- it points into the catalogue by id, and those ids are
/// about to mean something else. Everything that says what this machine *holds*
/// (`local_files`, `local_rom`, `local_game`) is untouched, so a `hash_match` re-links it by
/// content hash, which is identity rather than id and therefore survives this.
/// Catalogue tables that a replacement must **not** touch.
///
/// `platforms` is what `0001_schema.sql` calls "fixed, app-defined reference data (a compiled-in
/// enum)": its ids come from `Platform::igdb_id()`, so they are identical on every installation
/// by construction rather than by luck, and `seed_platforms` recreates them on any database.
/// It is also referenced by `local_directories.app_platform_id` and `local_rom.app_platform_id`
/// -- the very rows a replacement is supposed to preserve -- so deleting it would orphan them
/// to gain nothing at all.
///
/// Everything else in the catalogue really is DAT- or provider-derived, including `sources`,
/// whose ids are AUTO_INCREMENT and therefore genuinely machine-specific.
const CATALOG_KEEP: [&str; 1] = ["platforms"];

/// Why choosing `client` later is worse than choosing it now.
///
/// Shown at the first-run mode choice *and* used verbatim in `set_mode`'s refusal, so the
/// warning someone reads before there is anything to lose is the same sentence they meet if
/// they change their mind afterwards. A test asserts they stay identical -- a warning that
/// drifts from what actually happens is worse than no warning.
pub const SWITCH_WARNING: &str = "You can change this later, but it is not the easy path. \
Going from standalone to client replaces this machine's entire catalogue with the server's -- the two number \
the same games differently and cannot be merged. Everything that points at the catalogue then has to be \
re-linked: your games by content hash (reliable), your art by provider id (anything downloaded without \
provider tracking cannot be re-linked), and your curation -- keyword tags, merge decisions, manual ordering, \
collection sets -- is deleted and re-taken from the server. If you plan to sync, set the server up first and \
start this machine in client mode.";

/// The first-run choice, in one place both front ends read rather than three paraphrases.
pub fn mode_options() -> serde_json::Value {
    serde_json::json!([
        {
            "value": "standalone",
            "label": "Standalone",
            "detail": "This machine builds its own catalogue: it downloads DATs and queries providers, and the row ids it assigns are its own. Choose this if it is the only machine, or if it will be the server others sync from."
        },
        {
            "value": "client",
            "label": "Client",
            "detail": "This machine takes its catalogue from a main server and never builds one. It still scans and holds its own files; only the catalogue comes from the server."
        }
    ])
}

/// Empties one table by `TRUNCATE`, returning how many rows it held.
///
/// `DELETE FROM t` walks every row: each one goes through the undo log and every index, so
/// the 383,000-row `hashes` table took two seconds on a modest catalogue and a full No-Intro
/// set would take minutes. `TRUNCATE` drops the tablespace and recreates it empty -- constant
/// time, no undo -- which is what "throw the catalogue away" means anyway. Two things make it
/// safe here and would not elsewhere: nothing in [`clear_catalog`] runs inside a transaction
/// (`TRUNCATE` is DDL and commits implicitly), and `FOREIGN_KEY_CHECKS` is already off (InnoDB
/// refuses to truncate a table another one references while they are on). The count is read
/// first because `TRUNCATE` reports no rows affected, and both screens show the number.
/// Needs the `DROP` privilege, which the bundled server's `root` has.
async fn truncate(conn: &mut sqlx::MySqlConnection, table: &str) -> Result<u64> {
    let rows: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM `{table}`"))).fetch_one(&mut *conn).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("TRUNCATE TABLE `{table}`"))).execute(&mut *conn).await?;
    Ok(rows.max(0) as u64)
}

pub async fn clear_catalog(app: &App, media: MediaOnReplace) -> Result<u64> {
    // Before the first TRUNCATE as well as after: a clear cut short must not leave mirrors
    // trusting their positions over rows that vanished without tombstones.
    crate::database::sync_rev::new_epoch(&app.pool).await?;
    let mut conn = app.pool.acquire().await?;
    // Closed rather than returned to the pool if this is cut short (a cancel, a dropped
    // request): FOREIGN_KEY_CHECKS is per connection and must never be left off on one.
    conn.close_on_drop();
    sqlx::query("SET FOREIGN_KEY_CHECKS = 0").execute(&mut *conn).await?;
    let mut removed = 0u64;
    let result = async {
        removed += truncate(&mut conn, "local_release_games").await?;

        // Art: the files stay, but "which game is this of" is about to become false either
        // way, so the junctions always go. What was recorded first is *which* junction each
        // row was in -- provider art hangs off releases, manual picks off titles, and
        // rebuilding from the provider id alone could not tell them apart.
        if media == MediaOnReplace::Relink {
            sqlx::query("INSERT IGNORE INTO media_relink (media_id, kind) SELECT media_id, 'title' FROM title_media").execute(&mut *conn).await?;
            sqlx::query("INSERT IGNORE INTO media_relink (media_id, kind) SELECT media_id, 'release' FROM release_media").execute(&mut *conn).await?;
        } else {
            // Re-download: retire the rows now so nothing serves art that may be wrong, and
            // leave the files for the ordinary media storage cleanup rather than unlinking
            // them here.
            sqlx::query("UPDATE media m SET m.deleted = TRUE WHERE EXISTS (SELECT 1 FROM title_media tm WHERE tm.media_id = m.id) OR EXISTS (SELECT 1 FROM release_media rm WHERE rm.media_id = m.id)")
                .execute(&mut *conn)
                .await?;
        }
        removed += truncate(&mut conn, "title_media").await?;
        removed += truncate(&mut conn, "release_media").await?;

        // Collection sets come from the main server now (`sync::catalog_tables`), so the
        // local ones go with the catalogue they point into. Children first.
        removed += truncate(&mut conn, "collection_set_releases").await?;
        removed += truncate(&mut conn, "collection_set_entries").await?;
        removed += truncate(&mut conn, "collection_sets").await?;
        for table in super::backup::CATALOG_TABLES {
            if CATALOG_KEEP.contains(&table) {
                continue;
            }
            removed += truncate(&mut conn, table).await?;
        }
        // Every mirrored row references the catalogue by id, and those ids are about to mean
        // something else, so the mirror has to be rebuilt too.
        for table in [
            "remote_release_games",
            "remote_game_roms",
            "remote_rom_rahash",
            "remote_rom_files",
            "remote_game",
            "remote_rom",
            "remote_files",
            "remote_directories",
        ] {
            removed += truncate(&mut conn, table).await?;
        }
        // And the per-table hashes: without this the next sync compares the source's manifest
        // against what we *used* to hold, concludes nothing has changed, and downloads nothing
        // -- leaving the catalogue empty and every mirror row swept away as unresolved.
        truncate(&mut conn, "remote_sync_state").await?;
        // The lineage is about to be set by the first sync; until then this machine has no
        // catalogue at all, which is the honest state.
        sqlx::query("DELETE FROM settings WHERE `key` = ?").bind(crate::database::settings::KEY_CATALOG_FROM).execute(&mut *conn).await?;
        Ok::<(), Error>(())
    }
    .await;
    sqlx::query("SET FOREIGN_KEY_CHECKS = 1").execute(&mut *conn).await?;
    // TRUNCATE fires no triggers, so nothing recorded these deletes: whoever mirrors this
    // machine has to start from scratch rather than trust its position (`sync_rev`).
    crate::database::sync_rev::new_epoch(&app.pool).await?;
    result?;
    Ok(removed)
}

fn validate(spec: &SourceSpec) -> Result<String> {
    if spec.name.trim().is_empty() {
        return Err(Error::invalid("name is required"));
    }
    if !matches!(spec.role.as_str(), "main" | "peer") {
        return Err(Error::invalid("role must be main or peer"));
    }
    let url = spec.base_url.trim().trim_end_matches('/');
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(Error::invalid("base_url must start with http:// or https://"));
    }
    if url.contains("/api/v1") {
        return Err(Error::invalid("base_url is just the host and port -- leave off /api/v1"));
    }
    Ok(url.to_string())
}

/// The schema enforces at most one main (`uq_rsrc_main`) and one row per address
/// (`uq_rsrc_base`). Both come back as the same MariaDB error, so say which one it was.
fn duplicate(err: sqlx::Error, role: &str) -> Error {
    let text = err.to_string();
    if text.contains("uq_rsrc_main") {
        Error::invalid("there is already a main server; change that one to a peer first")
    } else if text.contains("uq_rsrc_base") {
        Error::invalid("a source with that address already exists")
    } else {
        let _ = role;
        Error::from(err)
    }
}

fn row_to_source(row: &sqlx::mysql::MySqlRow) -> Result<RemoteSource> {
    Ok(RemoteSource {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        base_url: row.try_get("base_url")?,
        role: row.try_get("role")?,
        enabled: row.try_get("enabled")?,
        has_token: row.try_get::<Option<String>, _>("token")?.is_some_and(|t| !t.is_empty()),
        tls_fingerprint: row.try_get("tls_fingerprint")?,
        last_sync_at: row.try_get("last_sync_at")?,
        created_at: row.try_get("created_at")?,
        tables: Vec::new(),
    })
}

pub async fn list(app: &App) -> Result<Vec<RemoteSource>> {
    let rows = sqlx::query("SELECT id, name, base_url, token, tls_fingerprint, role, enabled, last_sync_at, created_at FROM remote_sources WHERE deleted = FALSE ORDER BY role, name")
        .fetch_all(&app.pool)
        .await?;
    let mut out: Vec<RemoteSource> = rows.iter().map(row_to_source).collect::<Result<_>>()?;
    let state = sqlx::query("SELECT source_id, table_name, row_count, synced_at FROM remote_sync_state ORDER BY table_name").fetch_all(&app.pool).await?;
    for row in &state {
        let id: u16 = row.try_get("source_id")?;
        if let Some(source) = out.iter_mut().find(|s| s.id == id) {
            source.tables.push(SourceTable {
                table_name: row.try_get("table_name")?,
                row_count: row.try_get::<Option<u64>, _>("row_count")?.unwrap_or(0),
                synced_at: row.try_get("synced_at")?,
            });
        }
    }
    Ok(out)
}

pub async fn get(app: &App, id: u16) -> Result<RemoteSource> {
    list(app).await?.into_iter().find(|s| s.id == id).ok_or_else(|| Error::not_found(format!("no source {id}")))
}

/// A client for one peer: over https it trusts only the certificate this machine saw the first
/// time (`romexplorer_types::pinning`), since a romexplorer server's certificate is self-signed
/// and there is no CA to ask.
pub(crate) struct Peer {
    pub http: reqwest::Client,
    /// The address to use, with a LAN `http://` upgraded (see [`effective_base`]).
    pub base: String,
    id: Option<u16>,
    /// Set on a first connection: what the peer presented, to store once it has worked.
    captured: Option<std::sync::Arc<std::sync::Mutex<Option<String>>>>,
}

impl Peer {
    /// A client for `base`, pinned to `fingerprint` (or learning it, when there is none yet).
    pub(crate) fn new(app: &App, id: Option<u16>, base: &str, fingerprint: Option<String>) -> Result<Self> {
        use romexplorer_types::pinning::{client_config, Pin};
        let base = effective_base(base);
        if !base.starts_with("https://") {
            return Ok(Self { http: app.http.clone(), base, id, captured: None });
        }
        let (pin, captured) = match fingerprint.filter(|f| !f.is_empty()) {
            Some(f) => (Pin::Expect(f), None),
            None => {
                let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
                (Pin::Capture(slot.clone()), Some(slot))
            }
        };
        let http = reqwest::Client::builder()
            .use_preconfigured_tls(client_config(pin))
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| Error::internal(format!("building a client for {base}: {e}")))?;
        Ok(Self { http, base, id, captured })
    }

    /// What the peer presented on this first connection, if it was one.
    pub(crate) fn captured(&self) -> Option<String> {
        self.captured.as_ref().and_then(|slot| slot.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }

    /// Stores the certificate a first connection saw -- called once that connection has
    /// actually worked, and never over one already stored.
    pub(crate) async fn remember(&self, app: &App) -> Result<()> {
        if let (Some(id), Some(fingerprint)) = (self.id, self.captured()) {
            sqlx::query("UPDATE remote_sources SET tls_fingerprint = ? WHERE id = ? AND tls_fingerprint IS NULL").bind(fingerprint).bind(id).execute(&app.pool).await?;
        }
        Ok(())
    }
}

/// `http://` to another machine becomes `https://`: a romexplorer server only answers plain
/// HTTP from itself, and redirects everyone else. Loopback stays as it is.
pub(crate) fn effective_base(base: &str) -> String {
    let base = base.trim_end_matches('/');
    match base.strip_prefix("http://") {
        Some(rest) => {
            let host = rest.split('/').next().unwrap_or("");
            let host = host.rsplit_once(':').filter(|(h, p)| !h.is_empty() && p.chars().all(|c| c.is_ascii_digit())).map(|(h, _)| h).unwrap_or(host);
            if matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1") {
                base.to_string()
            } else {
                format!("https://{rest}")
            }
        }
        None => base.to_string(),
    }
}

/// Signs in to a source and returns the token it issued for this machine, plus the
/// certificate it presented if this was the first connection. The password goes no further.
async fn sign_in(app: &App, base: &str, fingerprint: Option<String>, username: &str, password: &str) -> Result<(String, Option<String>)> {
    let peer = Peer::new(app, None, base, fingerprint)?;
    let device = format!("romexplorer on {}", sysinfo::System::host_name().unwrap_or_else(|| "another machine".into()));
    let body = romexplorer_types::auth::LoginRequest { username: username.to_string(), password: password.to_string(), device, cookie: false, console: false };
    let response = peer.http.post(format!("{}/api/v1/auth/login", peer.base)).json(&body).send().await.map_err(|e| Error::unavailable(format!("could not reach {}: {e}", peer.base)))?;
    let status = response.status();
    if status.as_u16() == 401 {
        return Err(Error::invalid(format!("{} refused the sign-in: wrong username or password", peer.base)));
    }
    if !status.is_success() {
        return Err(Error::unavailable(format!("{} answered {status} to the sign-in", peer.base)));
    }
    let reply: romexplorer_types::auth::LoginResponse = response.json().await.map_err(|e| Error::invalid(format!("{}: {e}", peer.base)))?;
    Ok((reply.token, peer.captured()))
}

/// A signed-in session on the main server's own console, for the desktop app's Server
/// console tab.
///
/// The tab shows the server's console page inside this app, and every request that page
/// makes is forwarded from here rather than sent by the webview. That is what makes it work
/// at all: a romexplorer server's certificate is self-signed, which an embedded browser
/// refuses outright, while this client pins the certificate the source was added with
/// ([`Peer`]). It also keeps the account's token out of the page: it lives in this struct, in
/// memory, and is attached per request.
pub struct ConsoleSession {
    http: reqwest::Client,
    /// The server's address as it is actually reached.
    pub base: String,
    /// The source's name, for the tab's heading.
    pub name: String,
    pub username: String,
    token: String,
}

/// Signs in to the enabled main server with an account's credentials. The password goes no
/// further than the sign-in request; the token that comes back is a device of that account
/// on the server ("... console on <host>"), revocable there like any other.
pub async fn console_login(app: &App, username: &str, password: &str) -> Result<ConsoleSession> {
    let row = sqlx::query("SELECT id, name, base_url, tls_fingerprint FROM remote_sources WHERE role = 'main' AND enabled = TRUE AND deleted = FALSE LIMIT 1")
        .fetch_optional(&app.pool)
        .await?
        .ok_or_else(|| Error::invalid("no main server is set -- add one under Config > Share first"))?;
    let peer = Peer::new(app, Some(row.try_get("id")?), &row.try_get::<String, _>("base_url")?, row.try_get("tls_fingerprint")?)?;
    let device = format!("romexplorer console on {}", sysinfo::System::host_name().unwrap_or_else(|| "another machine".into()));
    let body = romexplorer_types::auth::LoginRequest { username: username.to_string(), password: password.to_string(), device, cookie: false, console: true };
    let response = peer.http.post(format!("{}/api/v1/auth/login", peer.base)).json(&body).send().await.map_err(|e| Error::unavailable(format!("could not reach {}: {e}", peer.base)))?;
    let status = response.status();
    if status.as_u16() == 401 {
        return Err(Error::invalid(format!("{} refused the sign-in: wrong username or password", peer.base)));
    }
    const ADMINS_ONLY: &str = "the server console is for admin accounts";
    if status.as_u16() == 403 {
        return Err(Error::forbidden(ADMINS_ONLY));
    }
    if !status.is_success() {
        return Err(Error::unavailable(format!("{} answered {status} to the sign-in", peer.base)));
    }
    let reply: romexplorer_types::auth::LoginResponse = response.json().await.map_err(|e| Error::invalid(format!("{}: {e}", peer.base)))?;
    // A server older than the `console` flag signs anyone in. Hold it to the same rule from
    // this side, and do not leave the token it issued behind.
    if !reply.me.admin {
        let _ = peer.http.post(format!("{}/api/v1/auth/logout", peer.base)).bearer_auth(&reply.token).send().await;
        return Err(Error::forbidden(ADMINS_ONLY));
    }
    peer.remember(app).await?;
    Ok(ConsoleSession { http: peer.http.clone(), base: peer.base.clone(), name: row.try_get("name")?, username: username.to_string(), token: reply.token })
}

impl ConsoleSession {
    /// Forwards one request of the console page to the server, as this session's account:
    /// (status, content type, body). `path` is the path and query as the page asked for it.
    pub async fn forward(&self, method: &str, path: &str, content_type: Option<&str>, body: Vec<u8>) -> Result<(u16, String, Vec<u8>)> {
        let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| Error::invalid(format!("unsupported method {method}")))?;
        let mut request = self.http.request(method, format!("{}{path}", self.base)).bearer_auth(&self.token);
        if let Some(ct) = content_type {
            request = request.header(reqwest::header::CONTENT_TYPE, ct);
        }
        if !body.is_empty() {
            request = request.body(body);
        }
        let response = request.send().await.map_err(|e| Error::unavailable(format!("could not reach {}: {e}", self.base)))?;
        let status = response.status().as_u16();
        let content_type = response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("application/octet-stream").to_string();
        let bytes = response.bytes().await.map_err(|e| Error::unavailable(format!("{}: {e}", self.base)))?;
        Ok((status, content_type, bytes.to_vec()))
    }

    /// Ends the session on the server (revokes this device's token). Best effort.
    pub async fn logout(&self) {
        let _ = self.http.post(format!("{}/api/v1/auth/logout", self.base)).bearer_auth(&self.token).send().await;
    }
}

/// Forgets the certificate trusted for a source, so the next connection takes whatever it
/// presents. For after a source's certificate was deliberately replaced.
pub async fn trust_new_certificate(app: &App, id: u16) -> Result<RemoteSource> {
    let n = sqlx::query("UPDATE remote_sources SET tls_fingerprint = NULL WHERE id = ? AND deleted = FALSE").bind(id).execute(&app.pool).await?.rows_affected();
    if n == 0 {
        return Err(Error::not_found(format!("no source {id}")));
    }
    get(app, id).await
}

/// The associated data a peer token is sealed with (`database::vault`).
const TOKEN_CONTEXT: &str = "remote_sources.token";

/// A token as it is stored: sealed, and `None` for an empty one.
fn seal_token(app: &App, token: Option<&str>) -> Option<String> {
    token.filter(|t| !t.is_empty()).map(|t| app.vault.seal(TOKEN_CONTEXT, t))
}

/// A stored token opened for use. A token that won't open is an error naming the fix rather
/// than being sent -- the peer would only answer 401, which reads as "wrong token".
pub(crate) fn open_token(app: &App, stored: Option<String>) -> Result<Option<String>> {
    match stored.filter(|t| !t.is_empty()) {
        None => Ok(None),
        Some(value) => app
            .vault
            .open(TOKEN_CONTEXT, &value)
            .map(Some)
            .map_err(|_| Error::invalid("this source's stored token won't open with this machine's master key -- sign in to it again on the Sources screen")),
    }
}

/// Seals any peer token still stored in plaintext (written before tokens were sealed).
/// Idempotent, so it runs on every start.
pub async fn seal_plaintext_tokens(app: &App) -> Result<u64> {
    let rows = sqlx::query("SELECT id, token FROM remote_sources WHERE token IS NOT NULL AND token <> '' AND token NOT LIKE 'enc:v1:%'").fetch_all(&app.pool).await?;
    for row in &rows {
        let id: u16 = row.try_get("id")?;
        let token: String = row.try_get("token")?;
        sqlx::query("UPDATE remote_sources SET token = ? WHERE id = ?").bind(app.vault.seal(TOKEN_CONTEXT, &token)).bind(id).execute(&app.pool).await?;
    }
    Ok(rows.len() as u64)
}

pub async fn create(app: &App, spec: &SourceSpec) -> Result<RemoteSource> {
    let url = validate(spec)?;
    // Signing in comes first: a source that can't be signed in to is not worth saving, and
    // the token it hands back is what gets stored.
    let (signed_in, fingerprint) = match (&spec.username, &spec.password) {
        (Some(u), Some(p)) if !u.trim().is_empty() => {
            let (token, fp) = sign_in(app, &url, None, u.trim(), p).await?;
            (Some(token), fp)
        }
        _ => (None, None),
    };
    let token = signed_in.as_deref().or(spec.token.as_deref());
    // Ask before doing anything, but do not act on the answer until the row actually exists:
    // clearing the catalogue first meant a create that then failed -- on a duplicate address,
    // say -- destroyed the catalogue and left nothing to sync it back from.
    let needs_replace = if spec.role == "main" {
        match catalog_conflict(app, &url).await? {
            Some(warning) if !spec.replace_catalog => return Err(Error::conflict(warning)),
            other => other.is_some(),
        }
    } else {
        false
    };

    // `uq_rsrc_base` does not know about soft deletes, so an address that was removed still
    // occupies it. Bring that row back rather than refusing an address the user can no longer
    // see anywhere.
    let revived: Option<u16> = sqlx::query_scalar("SELECT id FROM remote_sources WHERE base_url = ? AND deleted = TRUE").bind(&url).fetch_optional(&app.pool).await?;
    let id = match revived {
        Some(id) => {
            sqlx::query("UPDATE remote_sources SET name = ?, token = ?, tls_fingerprint = ?, role = ?, enabled = ?, deleted = FALSE, last_sync_at = NULL WHERE id = ?")
                .bind(spec.name.trim())
                .bind(seal_token(app, token))
                .bind(&fingerprint)
                .bind(&spec.role)
                .bind(spec.enabled)
                .bind(id)
                .execute(&app.pool)
                .await
                .map_err(|e| duplicate(e, &spec.role))?;
            id
        }
        None => {
            let result = sqlx::query("INSERT INTO remote_sources (name, base_url, token, tls_fingerprint, role, enabled) VALUES (?, ?, ?, ?, ?, ?)")
                .bind(spec.name.trim())
                .bind(&url)
                .bind(seal_token(app, token))
                .bind(&fingerprint)
                .bind(&spec.role)
                .bind(spec.enabled)
                .execute(&app.pool)
                .await
                .map_err(|e| duplicate(e, &spec.role))?;
            result.last_insert_id() as u16
        }
    };
    if needs_replace {
        clear_catalog(app, spec.media).await?;
    }
    get(app, id).await
}

pub async fn update(app: &App, id: u16, spec: &SourceSpec) -> Result<RemoteSource> {
    let url = validate(spec)?;
    let was_main: Option<String> = sqlx::query_scalar("SELECT role FROM remote_sources WHERE id = ? AND deleted = FALSE").bind(id).fetch_optional(&app.pool).await?;
    // Only when it *becomes* main: re-saving a main server's name or token must not ask.
    let needs_replace = if spec.role == "main" && was_main.as_deref() != Some("main") {
        match catalog_conflict(app, &url).await? {
            Some(warning) if !spec.replace_catalog => return Err(Error::conflict(warning)),
            other => other.is_some(),
        }
    } else {
        false
    };
    // A new address is a different machine as far as trust goes: forget its certificate.
    let stored: Option<(String, Option<String>)> = sqlx::query("SELECT base_url, tls_fingerprint FROM remote_sources WHERE id = ? AND deleted = FALSE")
        .bind(id)
        .fetch_optional(&app.pool)
        .await?
        .map(|r| -> Result<_> { Ok((r.try_get("base_url")?, r.try_get("tls_fingerprint")?)) })
        .transpose()?;
    let same_address = stored.as_ref().is_some_and(|(b, _)| b.trim_end_matches('/') == url);
    let mut fingerprint = if same_address { stored.and_then(|(_, f)| f) } else { None };
    let signed_in = match (&spec.username, &spec.password) {
        (Some(u), Some(p)) if !u.trim().is_empty() => {
            let (token, captured) = sign_in(app, &url, fingerprint.clone(), u.trim(), p).await?;
            fingerprint = fingerprint.or(captured);
            Some(token)
        }
        _ => None,
    };
    sqlx::query("UPDATE remote_sources SET tls_fingerprint = ? WHERE id = ? AND deleted = FALSE").bind(&fingerprint).bind(id).execute(&app.pool).await?;
    let token_update = signed_in.or_else(|| spec.token.clone());
    // `token IS NULL` in the spec means "keep what is stored": a screen that lists sources
    // never receives the token, so it cannot send it back, and a blind update would wipe it.
    match &token_update {
        Some(token) => {
            sqlx::query("UPDATE remote_sources SET name = ?, base_url = ?, token = ?, role = ?, enabled = ? WHERE id = ? AND deleted = FALSE")
                .bind(spec.name.trim())
                .bind(&url)
                .bind(seal_token(app, Some(token.as_str())))
                .bind(&spec.role)
                .bind(spec.enabled)
                .bind(id)
        }
        None => sqlx::query("UPDATE remote_sources SET name = ?, base_url = ?, role = ?, enabled = ? WHERE id = ? AND deleted = FALSE")
            .bind(spec.name.trim())
            .bind(&url)
            .bind(&spec.role)
            .bind(spec.enabled)
            .bind(id),
    }
    .execute(&app.pool)
    .await
    .map_err(|e| duplicate(e, &spec.role))?;
    // Only once the row is actually saved -- see `create`.
    if needs_replace {
        clear_catalog(app, spec.media).await?;
    }
    get(app, id).await
}

/// Soft-deletes. The mirror rows stay until [`forget`] removes them, so disabling a source by
/// accident does not throw away a sync.
pub async fn delete(app: &App, id: u16) -> Result<bool> {
    let result = sqlx::query("UPDATE remote_sources SET deleted = TRUE, enabled = FALSE WHERE id = ? AND deleted = FALSE").bind(id).execute(&app.pool).await?;
    Ok(result.rows_affected() > 0)
}

/// Drops everything mirrored from a source. Children first, so the composite foreign keys
/// hold at every step without turning checks off.
pub async fn forget(app: &App, id: u16) -> Result<u64> {
    let mut removed = 0u64;
    let mut conn = app.pool.acquire().await?;
    for table in [
        "remote_release_games",
        "remote_game_roms",
        "remote_rom_rahash",
        "remote_rom_files",
        "remote_game",
        "remote_rom",
        "remote_files",
        "remote_directories",
        "remote_sync_state",
    ] {
        removed += sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM `{table}` WHERE source_id = ?"))).bind(id).execute(&mut *conn).await?.rows_affected();
    }
    Ok(removed)
}

/// Asks a source whether it is there and whether the token works, as three separate answers.
/// "Cannot connect", "connected but the token is wrong" and "fine" need different fixes, and
/// a single boolean hides which one you have.
pub async fn check(app: &App, id: u16) -> Result<Reachability> {
    let row = sqlx::query("SELECT base_url, token, tls_fingerprint FROM remote_sources WHERE id = ? AND deleted = FALSE")
        .bind(id)
        .fetch_optional(&app.pool)
        .await?
        .ok_or_else(|| Error::not_found(format!("no source {id}")))?;
    let peer = Peer::new(app, Some(id), &row.try_get::<String, _>("base_url")?, row.try_get("tls_fingerprint")?)?;
    let base = peer.base.clone();
    let token = open_token(app, row.try_get("token")?)?;

    // `ping` is token-exempt on both routers, so it separates "unreachable" from "unauthorized".
    let ping = peer.http.get(format!("{base}/api/v1/ping")).send().await;
    let ping = match ping {
        Ok(p) => p,
        Err(e) => {
            let detail = if format!("{e:?}").contains("certificate changed") {
                format!("{base}'s certificate is not the one this machine trusted before -- if it was replaced on purpose, choose \"trust new certificate\"")
            } else {
                format!("could not reach {base}")
            };
            return Ok(Reachability { reachable: false, authorized: false, offers_catalog: false, detail });
        }
    };
    if !ping.status().is_success() {
        return Ok(Reachability { reachable: false, authorized: false, offers_catalog: false, detail: format!("{base} answered {} to a ping", ping.status()) });
    }

    peer.remember(app).await?;
    let mut request = peer.http.get(format!("{base}/api/v1/sync/manifest"));
    if let Some(token) = token.filter(|t| !t.is_empty()) {
        request = request.bearer_auth(token);
    }
    match request.send().await {
        Ok(response) if response.status().is_success() => {
            let manifest: super::sync::Manifest = response.json().await.map_err(|e| Error::invalid(format!("manifest: {e}")))?;
            let offers_catalog = manifest.tables.iter().any(|t| t.group == super::sync::GROUP_CATALOG);
            let detail = format!(
                "{} table(s) offered{}",
                manifest.tables.len(),
                if offers_catalog { ", including the catalogue" } else { " (ownership only -- this is a share, not a main server)" }
            );
            Ok(Reachability { reachable: true, authorized: true, offers_catalog, detail })
        }
        Ok(response) => Ok(Reachability {
            reachable: true,
            authorized: false,
            offers_catalog: false,
            detail: format!("reachable, but the manifest answered {}", response.status()),
        }),
        Err(e) => Ok(Reachability { reachable: true, authorized: false, offers_catalog: false, detail: e.to_string() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Another machine only answers plain HTTP to itself, so a LAN address is used over https.
    #[test]
    fn a_lan_address_is_reached_over_https() {
        assert_eq!(effective_base("http://192.168.1.5:7337"), "https://192.168.1.5:7337");
        assert_eq!(effective_base("http://nas.local:7337/"), "https://nas.local:7337");
        assert_eq!(effective_base("http://127.0.0.1:7337"), "http://127.0.0.1:7337");
        assert_eq!(effective_base("http://localhost"), "http://localhost");
        assert_eq!(effective_base("https://10.0.0.2:7337"), "https://10.0.0.2:7337");
    }

    fn spec(url: &str, role: &str) -> SourceSpec {
        SourceSpec { name: "s".into(), base_url: url.into(), token: None, username: None, password: None, role: role.into(), enabled: true, replace_catalog: false, media: MediaOnReplace::Relink }
    }

    #[test]
    fn addresses_are_a_host_and_port_not_an_endpoint() {
        assert_eq!(validate(&spec("http://192.168.1.10:7337/", "main")).unwrap(), "http://192.168.1.10:7337");
        assert!(validate(&spec("192.168.1.10:7337", "peer")).is_err());
        // The mistake worth catching by name: pasting the endpoint you were looking at.
        assert!(validate(&spec("http://nas.local:7337/api/v1/sync/manifest", "peer")).is_err());
    }

    #[test]
    fn role_is_one_of_two_things() {
        assert!(validate(&spec("http://h:1", "main")).is_ok());
        assert!(validate(&spec("http://h:1", "peer")).is_ok());
        assert!(validate(&spec("http://h:1", "primary")).is_err());
    }
}

pub async fn mode_view(app: &App) -> Result<ModeView> {
    use crate::database::settings::{self as db_settings, InstanceMode};
    let stored = db_settings::get::<String>(&app.pool, db_settings::KEY_INSTANCE_MODE).await?;
    let mode = stored.as_deref().and_then(InstanceMode::parse).unwrap_or(InstanceMode::Standalone);
    let catalog_from = db_settings::get::<String>(&app.pool, db_settings::KEY_CATALOG_FROM).await?.filter(|s| !s.is_empty());
    let releases: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM `release`").fetch_one(&app.pool).await?;
    // The same condition `set_mode` refuses on.
    let destructive = mode != InstanceMode::Client && releases > 0 && catalog_from.is_none();
    Ok(ModeView {
        mode: mode.name().to_string(),
        chosen: stored.is_some(),
        catalog_from,
        releases: releases.max(0) as u64,
        losses: if destructive { Some(losses(app).await?) } else { None },
        options: mode_options(),
        switch_warning: SWITCH_WARNING.to_string(),
    })
}

pub async fn set_mode(app: &App, mode: &str, replace_catalog: bool, media: MediaOnReplace) -> Result<serde_json::Value> {
    use crate::database::settings::{self as db_settings, InstanceMode};
    let wanted = InstanceMode::parse(mode).ok_or_else(|| Error::invalid("mode must be standalone or client"))?;
    let current = db_settings::instance_mode(&app.pool).await?;
    let mut cleared = 0u64;
    if wanted == InstanceMode::Client && current != InstanceMode::Client {
        let releases: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM `release`").fetch_one(&app.pool).await?;
        let from = db_settings::get::<String>(&app.pool, db_settings::KEY_CATALOG_FROM).await?.filter(|s| !s.is_empty());
        // A catalogue that was already synced from a main server is the server's, not this
        // machine's, so there is nothing of its own to lose.
        if releases > 0 && from.is_none() {
            if !replace_catalog {
                return Err(Error::conflict(format!(
                    "Client mode means this machine takes its catalogue from a main server and never builds its own. \
                     It currently has a catalogue of its own ({releases} releases).\n\n{SWITCH_WARNING}\n\n{}",
                    losses(app).await?
                )));
            }
            cleared = clear_catalog(app, media).await?;
        }
    }
    db_settings::set(&app.pool, db_settings::KEY_INSTANCE_MODE, &wanted.name()).await?;
    Ok(serde_json::json!({ "mode": wanted.name(), "catalog_rows_cleared": cleared }))
}

#[cfg(test)]
mod mode_tests {
    use crate::database::jobs::JobType;

    /// Which jobs a client installation may not run. Stated as a test so a new job type has
    /// to be classified rather than defaulting into "allowed", which is the direction that
    /// fails silently -- a catalogue-writing job permitted on a client corrupts ids, while a
    /// harmless job wrongly refused is merely annoying and obvious.
    #[test]
    fn catalogue_writing_jobs_are_classified() {
        for t in [
            JobType::DatSync,
            JobType::ProviderQuery,
            JobType::ProviderBulkSync,
            JobType::PlaymatchFill,
            JobType::IgdbReapply,
            JobType::TitleDedupe,
            JobType::TitleMergeAudit,
            JobType::RaGenreFetch,
            JobType::AudioAutotag,
            JobType::DisplayData,
        ] {
            assert!(t.writes_catalog(), "{} writes the catalogue", t.name());
        }
        // A client must still be able to scan its own files, match them against the catalogue
        // it was given, sync, back up and launch -- that is the whole point of the mode.
        for t in [
            JobType::FileScan,
            JobType::HashMatch,
            JobType::CatalogSync,
            JobType::MediaSync,
            JobType::MediaDownload,
            JobType::MediaRelayout,
            JobType::DbOptimize,
            JobType::RaAccountSync,
            JobType::RaPatchDownload,
            JobType::BackupDatabase,
            JobType::BackupCatalog,
            // Exports the DAT-derived rows; reads the catalogue, never writes it.
            JobType::BackupDatSeed,
            JobType::RestoreBackup,
            JobType::SqliteExport,
            JobType::SqliteImport,
            // A copy writes `local_*` and reads the catalogue -- exactly what a client does.
            JobType::CopyFiles,
            // An integrity check reads files and writes `hashes` for content it already has;
            // it never invents a catalogue row.
            JobType::IntegrityCheck,
            // Rebuilding a cue and checking one both act on this machine's own files and read
            // the catalogue to know what they should look like -- a client's whole posture.
            JobType::SheetCreate,
            JobType::SheetVerify,
            JobType::M3uVerify,
            // The audio library is this machine's own files and its own tables.
            JobType::AudioScan,
            // A client's own copies of the server's audio: what a client is for.
            JobType::AudioSync,
            // Art for this machine's releases; a client's goes through its main server.
            JobType::MediaBackfill,
            // The account's own unlocks and badge images, like ra_account_sync.
            JobType::RaBadges,
            // The name dictionary is this machine's own table, built from what it has.
            JobType::DictionaryBuild,
            // Writes this machine's art into its own emulators' folders.
            JobType::ArtworkExport,
            // Reads this machine's ROMs into its own tables.
            JobType::RomAudioIndex,
        ] {
            assert!(!t.writes_catalog(), "{} should be allowed on a client", t.name());
        }
        // Exhaustive: every job type is on one of the two lists above.
        assert_eq!(JobType::ALL.len(), 10 + 27, "a new job type needs classifying in this test");
    }
}

#[cfg(test)]
mod clear_tests {
    use super::*;
    use crate::api::backup::CATALOG_TABLES;

    #[test]
    fn reference_data_survives_a_catalogue_replacement() {
        // `platforms` is seeded from a compiled-in enum, so its ids are the same everywhere
        // and nothing is gained by deleting it -- while `local_rom.app_platform_id` and
        // `local_directories.app_platform_id` point straight at it, so deleting it orphans
        // exactly the rows a replacement promises to keep.
        assert!(CATALOG_KEEP.contains(&"platforms"));
        // It still belongs to the catalogue for backup and sync: identical rows are harmless
        // to copy, and a fresh machine needs them.
        assert!(CATALOG_TABLES.contains(&"platforms"));
        // But everything DAT- or provider-derived must go, including `sources`, whose ids are
        // AUTO_INCREMENT and so differ between independently-built machines.
        for t in ["sources", "content", "hashes", "release", "title", "release_contents", "title_releases", "igdb_games", "hasheous_lookups"] {
            assert!(!CATALOG_KEEP.contains(&t), "{t} must be cleared");
            assert!(CATALOG_TABLES.contains(&t), "{t} should be a catalogue table");
        }
    }

    /// Collection sets are backed up as this machine's own but synced from the main server.
    /// That double life is the whole point -- a standalone has no other copy of a hand-made
    /// list, while a client should see the household's -- and it is easy to undo by accident
    /// by "tidying" one list into the other.
    #[test]
    fn collection_sets_are_backed_up_locally_but_synced_from_the_server() {
        use crate::api::backup::DATABASE_TABLES;
        for t in ["collection_sets", "collection_set_releases", "collection_set_entries"] {
            assert!(DATABASE_TABLES.contains(&t), "{t} must be backed up as this machine's own");
            assert!(!CATALOG_TABLES.contains(&t), "{t} must not be in the catalogue backup as well");
            assert!(crate::api::sync::catalog_tables().contains(&t), "{t} must be offered by a main server");
        }
        // And they must arrive after the releases they reference.
        let synced = crate::api::sync::catalog_tables();
        let pos = |name: &str| synced.iter().position(|t| *t == name).expect("present");
        assert!(pos("release") < pos("collection_set_releases"));
        assert!(pos("collection_sets") < pos("collection_set_releases"));
        assert!(pos("collection_sets") < pos("collection_set_entries"));
    }

    /// The first-run warning and the later refusal must be the same words. If they drift, the
    /// warning stops describing what actually happens, which is worse than not warning.
    #[test]
    fn the_first_run_warning_is_what_the_refusal_says() {
        assert!(SWITCH_WARNING.contains("not the easy path"));
        assert!(SWITCH_WARNING.contains("set the server up first"));
        // `set_mode`'s refusal interpolates the constant rather than restating it; this is the
        // guard against someone "improving" one copy.
        let source = include_str!("sources.rs");
        assert!(source.contains("{SWITCH_WARNING}"), "set_mode must interpolate SWITCH_WARNING, not paraphrase it");
    }

    #[test]
    fn the_mode_options_name_both_modes() {
        let options = mode_options();
        let values: Vec<&str> = options.as_array().expect("array").iter().filter_map(|o| o["value"].as_str()).collect();
        assert_eq!(values, ["standalone", "client"]);
        for option in options.as_array().expect("array") {
            assert!(option["detail"].as_str().is_some_and(|d| d.len() > 40), "each mode needs an explanation, not just a label");
        }
    }

    #[test]
    fn relink_is_the_default_media_choice() {
        // Free, instant, and keeps art no provider would serve again -- the destructive option
        // must be asked for.
        assert_eq!(MediaOnReplace::default(), MediaOnReplace::Relink);
    }
}
